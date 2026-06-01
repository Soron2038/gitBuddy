//! Backend polling loop and in-memory cache.
//!
//! Pre-M6.5, polling lived in the Popover webview's `setInterval`. That worked
//! while gitBuddy had one window, but the moment the main window started
//! pulling the same data we had two timers fighting for the same API budget,
//! and there was no single place to diff "what's new since last tick" — a
//! prerequisite for the notifications we're about to ship in Phase 2.
//!
//! This module owns the periodic fetch. A single tokio task fans out across
//! all connected providers (`fetch_all`), writes the result into
//! `AppState.cache`, and emits a `data-updated` Tauri event. Both windows
//! subscribe to that event and re-read the cache via the existing
//! `list_waiting` / `list_repos` / `list_releases` / `list_ci` / `list_local_repos`
//! commands, which are now cheap synchronous cache reads.
//!
//! Two `Notify` primitives gate the loop's sleep:
//!  * `refresh_trigger` — fired by `aggregator_refresh_now` or any auth
//!    command (set_token, oauth_poll, disconnect) so a freshly-connected
//!    account populates immediately instead of waiting up to 5 minutes.
//!  * `settings_reload` — fired by `save_settings`, so a poll-interval change
//!    takes effect on the *current* sleep cycle, not the next.
//!
//! Provider fan-out failures are logged per-provider but don't abort the
//! tick. Same contract as the pre-aggregator `list_waiting` etc., preserved
//! so the popover never sees half a list when one provider rate-limits.

use crate::{
    accounts,
    commands::AppState,
    local_index::{self, LocalRepo},
    notifications::{self, Kind, SeenStore},
    provider_util::ProviderBackend,
    settings::{self, NotificationSettings, Settings, POLL_INTERVAL_DEFAULT},
    types::{CiRun, CiStatus, ItemReason, Release, Repo, WaitingItem},
};
use chrono::Utc;
use std::{collections::HashMap, sync::Arc, time::Duration};
use tauri::{AppHandle, Emitter};

/// Snapshot of every aggregated list as of the most recent successful tick.
/// `last_synced_at` is `None` until the first tick completes, so the UI can
/// tell "we haven't polled yet" from "we polled and got an empty list".
#[derive(Default, Clone)]
pub struct AggregatorCache {
    pub waiting: Vec<WaitingItem>,
    pub repos: Vec<Repo>,
    pub releases: Vec<Release>,
    pub ci: Vec<CiRun>,
    pub locals: Vec<LocalRepo>,
    pub last_synced_at: Option<String>,
    pub last_error: Option<String>,
}

/// Spawn the polling task. Called exactly once from `lib.rs::setup`. The
/// returned task is detached — its lifetime is tied to the Tokio runtime,
/// which lives as long as the Tauri app process.
pub fn spawn_loop(app: AppHandle, state: Arc<AppState>) {
    tauri::async_runtime::spawn(async move {
        // Ensure keychain restore + account migrations have run before the
        // first tick. Otherwise `fetch_all` finds an empty provider registry
        // and emits a useless empty snapshot, and the frontend sees nothing
        // until the user opens a window and triggers a lazy init.
        state.ensure_initialized(&app).await;
        run_loop(&app, &state).await;
    });
}

/// The polling task body. Loops forever, alternating ticks and sleeps,
/// breaking out only if the runtime shuts down.
async fn run_loop(app: &AppHandle, state: &AppState) {
    loop {
        tick(app, state).await;

        // Re-read the interval *after* each tick so a save_settings during
        // a tick is picked up next sleep — combined with the `Notify`
        // wakeup below, this gives near-instant feedback on a slider drag.
        let sleep_for = current_poll_interval(app);
        tokio::select! {
            _ = tokio::time::sleep(sleep_for) => {}
            _ = state.refresh_trigger.notified() => {
                // Manual refresh or auth change — tick immediately.
            }
            _ = state.settings_reload.notified() => {
                // Settings changed; the new poll interval takes effect on
                // the very next iteration because `current_poll_interval`
                // is called above.
            }
        }
    }
}

/// External entry point so commands can request an immediate tick without
/// importing `Notify` directly. Fire-and-forget — the actual tick runs in
/// the polling task and surfaces its result via `data-updated`.
pub fn refresh_now(state: &AppState) {
    state.refresh_trigger.notify_one();
}

/// Run a single fetch + cache write + diff + notify + event emit.
/// Public so `commands.rs` can invoke it during tests; production drives
/// it from `run_loop`.
pub async fn tick(app: &AppHandle, state: &AppState) {
    let snapshot = fetch_all(app, state).await;
    let synced_at = Utc::now().to_rfc3339();
    let now_ts = synced_at.clone();

    // Load settings + seen-store outside the cache write lock so the
    // notification step (which doesn't touch the cache) can't be blocked
    // by a reader. Failures load defaults instead of aborting — the worst
    // case is a one-tick over-notify, which is preferable to skipping
    // notifications altogether on a transient disk hiccup.
    let settings = settings::load(app).unwrap_or_default();
    let mut store = notifications::load(app);
    notifications::prune(&mut store);

    // Map account-id → viewer-login (lowercased). The CI-failure diff
    // needs this to decide whether the user *triggered* a failing run
    // worth notifying about; pulling once per tick keeps the per-CiRun
    // lookup constant-time. A load failure → empty map, which silently
    // disables CI-failure notifications for this tick rather than
    // panicking.
    let viewer_logins = accounts::load(app)
        .map(|f| {
            f.accounts
                .into_iter()
                .map(|a| (a.id, a.viewer.login.to_lowercase()))
                .collect::<HashMap<String, String>>()
        })
        .unwrap_or_default();

    diff_and_notify(
        app,
        &settings.notifications,
        &snapshot,
        &viewer_logins,
        &mut store,
        &now_ts,
    );

    if let Err(e) = notifications::save(app, &store) {
        eprintln!("gitbuddy: persisting notifications.json failed: {e}");
    }

    {
        let mut cache = state.cache.write().await;
        cache.waiting = snapshot.waiting;
        cache.repos = snapshot.repos;
        cache.releases = snapshot.releases;
        cache.ci = snapshot.ci;
        cache.locals = snapshot.locals;
        cache.last_synced_at = Some(synced_at.clone());
        cache.last_error = snapshot.error;
    }

    if let Err(e) = app.emit("data-updated", DataUpdatedPayload { synced_at }) {
        eprintln!("gitbuddy: emitting data-updated failed: {e}");
    }
}

/// Compare the current snapshot against the persisted seen-store. On a
/// cold start (`initialised == false`) we *only* seed — every visible
/// item is recorded as already-seen and the flag flips. From the next
/// tick on, anything not in the store is genuinely new and produces a
/// `notifications::fire` call.
///
/// Mutates the store in place; the caller is responsible for persisting
/// afterwards. Kept in this module (not `notifications`) because the diff
/// shape is aggregator-internal — `notifications` deliberately doesn't
/// know what a `WaitingItem` looks like.
fn diff_and_notify(
    app: &AppHandle,
    settings: &NotificationSettings,
    snapshot: &FetchSnapshot,
    viewer_logins: &HashMap<String, String>,
    store: &mut SeenStore,
    now_ts: &str,
) {
    let cold_start = !store.initialised;

    // Each item is recorded as seen exactly once (the first-sight timestamp is
    // preserved across ticks so the TTL prune can expire it), and a
    // notification fires only when it's a genuinely new sighting: past the
    // cold-start seed and not already in the store.
    for item in &snapshot.waiting {
        let key = waiting_key(item);
        let already_seen = store.waiting.contains_key(&key);
        store
            .waiting
            .entry(key)
            .or_insert_with(|| now_ts.to_string());
        if !cold_start && !already_seen {
            notifications::fire(
                app,
                settings,
                Kind::Waiting {
                    reason_label: waiting_reason_label(item.reason).to_string(),
                    repo: item.repo.clone(),
                    title: item.title.clone(),
                },
            );
        }
    }

    for release in &snapshot.releases {
        let key = release_key(release);
        let already_seen = store.releases.contains_key(&key);
        store
            .releases
            .entry(key)
            .or_insert_with(|| now_ts.to_string());
        // The provider marks "published within last 7 days" via `is_new`.
        // Older releases are backfill (the user just connected an account
        // that's been around a while) — seed them silently so we don't spam
        // on first sight of an old changelog.
        if !cold_start && !already_seen && release.is_new {
            notifications::fire(
                app,
                settings,
                Kind::Release {
                    repo: release.repo_full_name.clone(),
                    tag_name: release.tag.clone(),
                },
            );
        }
    }

    // CI-failure diff. Three gates compose:
    //   1. Status must be `Fail` (Cancelled / Run / None / Ok all skip).
    //   2. The run's `author_login` must match the connected account's
    //      viewer login — we only notify the *triggerer* of a failed run,
    //      not the whole org. Providers that don't surface an actor
    //      (some self-hosted Forgejo) produce `None` here, which never
    //      matches → silent skip (DECISIONS.md 2026-05-26).
    //   3. The seen-key must not already be in `store.ci_failures`. The
    //      key is composed from the run's `html_url` when available, so
    //      a re-run (which gets a fresh URL) counts as a new event;
    //      a tick that sees the *same* still-failing run reuses the
    //      already-stored key and no second notification fires.
    for run in &snapshot.ci {
        if run.status != CiStatus::Fail {
            continue;
        }
        let Some(account_id) = run.account_id.as_deref() else {
            continue;
        };
        let Some(author) = run.author_login.as_deref() else {
            continue;
        };
        let Some(viewer) = viewer_logins.get(account_id) else {
            continue;
        };
        if author.to_lowercase() != *viewer {
            continue;
        }

        let key = ci_failure_key(run);
        let already_seen = store.ci_failures.contains_key(&key);
        store
            .ci_failures
            .entry(key)
            .or_insert_with(|| now_ts.to_string());
        if !cold_start && !already_seen {
            notifications::fire(
                app,
                settings,
                Kind::CiFailure {
                    repo: run.repo_full_name.clone(),
                    branch: run.branch.clone().unwrap_or_else(|| "main".to_string()),
                },
            );
        }
    }

    if cold_start {
        store.initialised = true;
    }
}

fn waiting_key(item: &WaitingItem) -> String {
    // Composite of account + provider-stable id so the same issue id
    // observed via two different accounts produces two store rows
    // (otherwise one account's view could mask another's notification).
    let account = item.account_id.as_deref().unwrap_or("unknown");
    format!("{account}:{}", item.id)
}

fn release_key(r: &Release) -> String {
    let account = r.account_id.as_deref().unwrap_or("unknown");
    format!("{account}:{}:{}", r.repo_full_name, r.tag)
}

/// Per-failed-run key. The `html_url` is the strongest provider-stable
/// identifier we get — every re-run produces a new URL on GitHub /
/// GitLab / Gitea, so it naturally distinguishes "still the same fail"
/// from "another fail happened". When the URL is missing we fall back
/// to `repo_full_name + branch`, which collapses any still-failing run
/// on that branch into a single key — acceptable since the alternative
/// is no notification at all.
fn ci_failure_key(run: &CiRun) -> String {
    let account = run.account_id.as_deref().unwrap_or("unknown");
    let suffix = run.html_url.clone().unwrap_or_else(|| {
        format!(
            "{}:{}",
            run.repo_full_name,
            run.branch.as_deref().unwrap_or("?")
        )
    });
    format!("{account}:{suffix}")
}

fn waiting_reason_label(reason: ItemReason) -> &'static str {
    match reason {
        ItemReason::Assigned => "Assigned to you",
        ItemReason::Review => "Review requested",
        ItemReason::Authored => "Update on your PR",
        ItemReason::Mentioned => "Mentioned",
    }
}

#[derive(serde::Serialize, Clone)]
pub struct DataUpdatedPayload {
    /// RFC 3339 timestamp of the tick that produced the new cache contents.
    pub synced_at: String,
}

#[derive(Default)]
struct FetchSnapshot {
    waiting: Vec<WaitingItem>,
    repos: Vec<Repo>,
    releases: Vec<Release>,
    ci: Vec<CiRun>,
    locals: Vec<LocalRepo>,
    /// Last non-fatal aggregate-level error to surface in the UI (e.g. "local
    /// scan failed"). Per-provider failures are logged but not propagated
    /// here so one bad provider doesn't paint the whole status as broken.
    error: Option<String>,
}

/// Run all four aggregated fetches plus the local scan in parallel. Mirrors
/// what the pre-aggregator `list_*` commands did individually, but in a
/// single coordinated pass per tick so the snapshot is internally consistent.
async fn fetch_all(app: &AppHandle, state: &AppState) -> FetchSnapshot {
    // Snapshot the provider registry up-front. The HashMap read is cheap and
    // we want to release the read lock before the await chain below touches
    // the network, so a connect/disconnect during a tick doesn't block on the
    // registry lock for tens of seconds. One unified map means a single
    // snapshot and a single fan-out loop per list, regardless of forge.
    let providers: Vec<(String, Arc<dyn ProviderBackend>)> = state
        .providers
        .read()
        .await
        .iter()
        .map(|(id, p)| (id.clone(), p.clone()))
        .collect();

    let mut snapshot = FetchSnapshot::default();

    // Waiting items, ordered most-recent first to match the popover's
    // expectations.
    for (id, p) in &providers {
        match p.list_waiting().await {
            Ok(v) => tag_extend(&mut snapshot.waiting, v, id),
            Err(e) => eprintln!("gitbuddy: list_waiting[{id}] failed: {e}"),
        }
    }
    snapshot
        .waiting
        .sort_by(|a, b| b.updated_at.cmp(&a.updated_at));

    for (id, p) in &providers {
        match p.list_repos().await {
            Ok(v) => tag_extend(&mut snapshot.repos, v, id),
            Err(e) => eprintln!("gitbuddy: list_repos[{id}] failed: {e}"),
        }
    }
    snapshot.repos.sort_by(|a, b| b.pushed_at.cmp(&a.pushed_at));

    for (id, p) in &providers {
        match p.list_releases().await {
            Ok(v) => tag_extend(&mut snapshot.releases, v, id),
            Err(e) => eprintln!("gitbuddy: list_releases[{id}] failed: {e}"),
        }
    }

    for (id, p) in &providers {
        match p.list_ci().await {
            Ok(v) => tag_extend(&mut snapshot.ci, v, id),
            Err(e) => eprintln!("gitbuddy: list_ci[{id}] failed: {e}"),
        }
    }

    // Local index scan — runs on a blocking thread because libgit2 is
    // synchronous. We try, and on failure record the error for the UI but
    // leave the cache's prior local list intact (the caller decides via
    // `cache.last_error`) so a momentary scan glitch doesn't blank the
    // "Local clones" view.
    match settings::load(app) {
        Ok(s) => match tokio::task::spawn_blocking(move || local_index::scan(&s)).await {
            Ok(v) => snapshot.locals = v,
            Err(e) => snapshot.error = Some(format!("Local scan task panicked: {e}")),
        },
        Err(e) => snapshot.error = Some(format!("Loading settings failed: {e}")),
    }

    snapshot
}

/// Items the aggregator stamps with the account id that surfaced them, so the
/// UI can show per-account badges and the diff/notify pass can key by account.
trait Tagged {
    fn set_account_id(&mut self, id: &str);
}
impl Tagged for WaitingItem {
    fn set_account_id(&mut self, id: &str) {
        self.account_id = Some(id.to_string());
    }
}
impl Tagged for Repo {
    fn set_account_id(&mut self, id: &str) {
        self.account_id = Some(id.to_string());
    }
}
impl Tagged for Release {
    fn set_account_id(&mut self, id: &str) {
        self.account_id = Some(id.to_string());
    }
}
impl Tagged for CiRun {
    fn set_account_id(&mut self, id: &str) {
        self.account_id = Some(id.to_string());
    }
}

/// Append `items` to `out`, stamping each with the account `id` it came from.
fn tag_extend<T: Tagged>(out: &mut Vec<T>, items: Vec<T>, id: &str) {
    out.extend(items.into_iter().map(|mut it| {
        it.set_account_id(id);
        it
    }));
}

/// Read the user's configured poll interval from Settings. `Settings::load`
/// already clamps `poll_interval_minutes` to `[1, 60]`, so this never
/// produces a sleep less than a minute or more than an hour. A load
/// failure (corrupt file, missing dir) falls back to the default rather
/// than letting the loop panic — better to keep polling at 5 min than to
/// silently stop.
fn current_poll_interval(app: &AppHandle) -> Duration {
    let minutes: u64 = settings::load(app)
        .as_ref()
        .map(|s: &Settings| s.poll_interval_minutes)
        .unwrap_or(POLL_INTERVAL_DEFAULT) as u64;
    Duration::from_secs(minutes * 60)
}
