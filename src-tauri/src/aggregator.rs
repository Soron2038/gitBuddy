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
    provider_util::{ProviderBackend, ProviderError},
    settings::{self, NotificationSettings, Settings},
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
        // The tick hands back the settings it ran with, so the sleep below
        // needs no second disk read. A save_settings *during* the tick still
        // takes effect immediately: it fires `settings_reload`, whose stored
        // permit makes the select! return at once, and the next iteration
        // re-reads the file.
        let settings = tick(app, state).await;

        let sleep_for = poll_interval(&settings);
        tokio::select! {
            _ = tokio::time::sleep(sleep_for) => {}
            _ = state.refresh_trigger.notified() => {
                // Manual refresh or auth change — tick immediately.
            }
            _ = state.settings_reload.notified() => {
                // Settings changed; the next iteration's tick reloads them.
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

/// Run a single fetch + cache write + diff + notify + event emit. Returns
/// the settings the tick ran with so `run_loop` can derive its sleep without
/// a second disk read. Public so `commands.rs` can invoke it during tests;
/// production drives it from `run_loop`.
pub async fn tick(app: &AppHandle, state: &AppState) -> Settings {
    // One settings read per tick: the fetch (scan roots), the notification
    // gates and the caller's sleep interval all see the same values. A load
    // failure falls back to defaults for the gates — the worst case is a
    // one-tick over-notify, which is preferable to skipping notifications
    // altogether on a transient disk hiccup.
    let loaded = settings::load(app);
    let snapshot = fetch_all(state, &loaded).await;
    let settings = loaded.unwrap_or_default();

    let synced_at = Utc::now().to_rfc3339();
    let now_ts = synced_at.clone();

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

    settings
}

/// Settings-gated wrapper around [`compute_new_events`]: compute the genuinely
/// new events for this tick (mutating the seen-store in place), then fire each
/// through `notifications::fire`, which applies the user's master / DnD /
/// per-event gates. Split this way so the diff core is unit-testable without a
/// Tauri `AppHandle`.
fn diff_and_notify(
    app: &AppHandle,
    settings: &NotificationSettings,
    snapshot: &FetchSnapshot,
    viewer_logins: &HashMap<String, String>,
    store: &mut SeenStore,
    now_ts: &str,
) {
    for kind in compute_new_events(snapshot, viewer_logins, store, now_ts) {
        notifications::fire(app, settings, kind);
    }
}

/// Pure diff core. Walks the snapshot, records every sighting in `store`
/// (preserving the first-seen timestamp so the TTL prune can expire it), and
/// returns the events that are *genuinely new* — past the cold-start seed and
/// not already recorded. No `AppHandle`, no settings gates: the gating stays in
/// `notifications::fire`, which the wrapper applies to each returned event.
/// Kept in this module (not `notifications`) because the diff shape is
/// aggregator-internal — `notifications` deliberately doesn't know what a
/// `WaitingItem` looks like.
///
/// On a cold start (`!store.initialised`) every visible item is seeded as
/// already-seen and the flag flips, so the returned vec is empty — a fresh
/// install / upgrade never replays a backlog.
fn compute_new_events(
    snapshot: &FetchSnapshot,
    viewer_logins: &HashMap<String, String>,
    store: &mut SeenStore,
    now_ts: &str,
) -> Vec<Kind> {
    let cold_start = !store.initialised;
    let mut events = Vec::new();

    // Each item is recorded as seen exactly once (the first-sight timestamp is
    // preserved across ticks so the TTL prune can expire it), and an event is
    // emitted only on a genuinely new sighting: past the cold-start seed and
    // not already in the store.
    for item in &snapshot.waiting {
        let key = waiting_key(item);
        let already_seen = store.waiting.contains_key(&key);
        store
            .waiting
            .entry(key)
            .or_insert_with(|| now_ts.to_string());
        if !cold_start && !already_seen {
            events.push(Kind::Waiting {
                reason_label: waiting_reason_label(item.reason).to_string(),
                repo: item.repo.clone(),
                title: item.title.clone(),
            });
        }
    }

    for release in &snapshot.releases {
        let key = release_key(release);
        let already_seen = store.releases.contains_key(&key);
        store
            .releases
            .entry(key)
            .or_insert_with(|| now_ts.to_string());
        // `is_new` = published within the last 7 days. Older releases are
        // backfill (the user just connected a long-lived account) — seed them
        // silently so we don't spam on first sight of an old changelog.
        if !cold_start && !already_seen && release.is_new {
            events.push(Kind::Release {
                repo: release.repo_full_name.clone(),
                tag_name: release.tag.clone(),
            });
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
    //      already-stored key and no second event fires.
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
            events.push(Kind::CiFailure {
                repo: run.repo_full_name.clone(),
                branch: run.branch.clone().unwrap_or_else(|| "main".to_string()),
            });
        }
    }

    if cold_start {
        store.initialised = true;
    }

    events
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

/// Run every provider's fetches plus the local scan for one tick. Providers
/// run concurrently (one task each, so a slow forge can't serialise the
/// others); within a provider the waiting/repo fetches overlap too, and the
/// repo list is fetched once and feeds both the releases and CI lookups.
/// Mirrors what the pre-aggregator `list_*` commands did individually, but
/// in a single coordinated pass per tick so the snapshot is internally
/// consistent.
async fn fetch_all(state: &AppState, settings: &Result<Settings, String>) -> FetchSnapshot {
    // Snapshot the provider registry up-front. The HashMap read is cheap and
    // we want to release the read lock before the await chain below touches
    // the network, so a connect/disconnect during a tick doesn't block on the
    // registry lock for tens of seconds. One unified map means a single
    // snapshot and a single fan-out, regardless of forge.
    let providers: Vec<(String, Arc<dyn ProviderBackend>)> = state
        .providers
        .read()
        .await
        .iter()
        .map(|(id, p)| (id.clone(), p.clone()))
        .collect();

    let mut tasks = Vec::with_capacity(providers.len());
    for (id, p) in providers {
        tasks.push(tokio::spawn(async move {
            let (waiting, repos) = tokio::join!(p.list_waiting(), p.list_repos());
            // Releases and CI reuse the repo list fetched above; on a repo
            // fetch error they see an empty slice, preserving the per-list
            // failure isolation the sequential version had.
            let known = repos.as_deref().unwrap_or(&[]);
            let (releases, ci) = tokio::join!(p.list_releases(known), p.list_ci(known));
            (id, waiting, repos, releases, ci)
        }));
    }

    let mut snapshot = FetchSnapshot::default();
    for task in tasks {
        let (id, waiting, repos, releases, ci) = match task.await {
            Ok(f) => f,
            Err(e) => {
                eprintln!("gitbuddy: provider fetch task panicked: {e}");
                continue;
            }
        };
        merge_result(
            &mut snapshot.waiting,
            waiting,
            &id,
            "list_waiting",
            &mut snapshot.error,
        );
        merge_result(
            &mut snapshot.repos,
            repos,
            &id,
            "list_repos",
            &mut snapshot.error,
        );
        merge_result(
            &mut snapshot.releases,
            releases,
            &id,
            "list_releases",
            &mut snapshot.error,
        );
        merge_result(&mut snapshot.ci, ci, &id, "list_ci", &mut snapshot.error);
    }

    // Waiting items most-recent first (the popover's expectation); repos by
    // last push. Sorting after the merge keeps multi-account output stable
    // regardless of which provider's task finished first.
    snapshot
        .waiting
        .sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    snapshot.repos.sort_by(|a, b| b.pushed_at.cmp(&a.pushed_at));

    // Local index scan — runs on a blocking thread because libgit2 is
    // synchronous. We try, and on failure record the error for the UI but
    // leave the cache's prior local list intact (the caller decides via
    // `cache.last_error`) so a momentary scan glitch doesn't blank the
    // "Local clones" view. When settings failed to load we skip the scan
    // rather than scanning default roots the user may have removed.
    match settings {
        Ok(s) => {
            let s = s.clone();
            match tokio::task::spawn_blocking(move || local_index::scan(&s)).await {
                Ok(v) => snapshot.locals = v,
                Err(e) => snapshot.error = Some(format!("Local scan task panicked: {e}")),
            }
        }
        Err(e) => snapshot.error = Some(format!("Loading settings failed: {e}")),
    }

    snapshot
}

/// Fold one provider list result into the snapshot: `Ok` extends the list
/// (stamping the account id), `Err` is logged without aborting the tick.
/// Rate limiting additionally lands in the snapshot error so the UI shows
/// it — the user can act on that (lower the poll cadence) in a way they
/// can't for a transient 5xx.
fn merge_result<T: Tagged>(
    out: &mut Vec<T>,
    res: Result<Vec<T>, ProviderError>,
    id: &str,
    what: &str,
    error_slot: &mut Option<String>,
) {
    match res {
        Ok(v) => tag_extend(out, v, id),
        Err(e) => {
            eprintln!("gitbuddy: {what}[{id}] failed: {e}");
            if matches!(e, ProviderError::RateLimited { .. }) {
                *error_slot = Some(e.to_string());
            }
        }
    }
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

/// Sleep duration for the user's configured poll cadence. `Settings::load`
/// already clamps `poll_interval_minutes` to `[1, 60]`, so this never
/// produces a sleep under a minute or over an hour (and a load failure
/// upstream falls back to `Settings::default()`, i.e. 5 minutes).
fn poll_interval(settings: &Settings) -> Duration {
    Duration::from_secs(settings.poll_interval_minutes as u64 * 60)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ItemKind, Provider};

    fn waiting(id: &str, account: &str) -> WaitingItem {
        WaitingItem {
            id: id.into(),
            kind: ItemKind::Pr,
            title: format!("Item {id}"),
            repo: "o/r".into(),
            provider: Provider::Github,
            reason: ItemReason::Review,
            url: "https://example.com".into(),
            age_human: "1d".into(),
            updated_at: "2026-06-01T00:00:00Z".into(),
            account_id: Some(account.into()),
        }
    }

    fn release(tag: &str, account: &str, is_new: bool) -> Release {
        Release {
            repo_id: "1".into(),
            repo_full_name: "o/r".into(),
            provider: Provider::Github,
            tag: tag.into(),
            name: tag.into(),
            published_at: "2026-06-01T00:00:00Z".into(),
            html_url: "https://example.com".into(),
            is_prerelease: false,
            is_new,
            age_human: "1d".into(),
            account_id: Some(account.into()),
        }
    }

    fn ci(status: CiStatus, author: Option<&str>, account: Option<&str>, url: &str) -> CiRun {
        CiRun {
            repo_id: "1".into(),
            repo_full_name: "o/r".into(),
            status,
            html_url: Some(url.into()),
            branch: Some("main".into()),
            workflow_name: Some("CI".into()),
            author_login: author.map(Into::into),
            account_id: account.map(Into::into),
        }
    }

    /// `SeenStore` whose cold-start seed has already happened, so the diff
    /// emits on new sightings instead of silently seeding.
    fn seeded_store() -> SeenStore {
        SeenStore {
            initialised: true,
            ..Default::default()
        }
    }

    #[test]
    fn cold_start_seeds_without_emitting() {
        let mut store = SeenStore::default(); // initialised == false
        let snapshot = FetchSnapshot {
            waiting: vec![waiting("1", "acc")],
            releases: vec![release("v1", "acc", true)],
            ..Default::default()
        };
        let events = compute_new_events(
            &snapshot,
            &HashMap::new(),
            &mut store,
            "2026-06-02T00:00:00Z",
        );
        assert!(events.is_empty(), "cold start must emit nothing");
        assert!(store.initialised, "cold start flips the flag");
        // Everything visible is recorded as seen so the *next* tick is the
        // first one that can emit.
        assert!(store
            .waiting
            .contains_key(&waiting_key(&snapshot.waiting[0])));
        assert!(store
            .releases
            .contains_key(&release_key(&snapshot.releases[0])));
    }

    #[test]
    fn second_tick_emits_only_genuinely_new() {
        let mut store = seeded_store();
        let snap1 = FetchSnapshot {
            waiting: vec![waiting("1", "acc")],
            ..Default::default()
        };
        let ev1 = compute_new_events(&snap1, &HashMap::new(), &mut store, "t1");
        assert_eq!(ev1.len(), 1, "first sighting of item 1 emits");
        assert!(matches!(ev1[0], Kind::Waiting { .. }));

        // Same item again → already seen → nothing.
        let ev2 = compute_new_events(&snap1, &HashMap::new(), &mut store, "t2");
        assert!(ev2.is_empty(), "re-seeing the same item must not emit");

        // A brand-new item alongside the old one → only the new one emits.
        let snap3 = FetchSnapshot {
            waiting: vec![waiting("1", "acc"), waiting("2", "acc")],
            ..Default::default()
        };
        let ev3 = compute_new_events(&snap3, &HashMap::new(), &mut store, "t3");
        assert_eq!(ev3.len(), 1, "only the unseen item emits");
    }

    #[test]
    fn same_id_across_accounts_emits_independently() {
        let mut store = seeded_store();
        let snap = FetchSnapshot {
            waiting: vec![waiting("1", "acc-a"), waiting("1", "acc-b")],
            ..Default::default()
        };
        let ev = compute_new_events(&snap, &HashMap::new(), &mut store, "t");
        assert_eq!(ev.len(), 2, "the same id via two accounts is two events");
    }

    #[test]
    fn release_emits_only_when_is_new() {
        let mut store = seeded_store();
        let snap = FetchSnapshot {
            releases: vec![release("v1", "acc", false)], // backfill, not new
            ..Default::default()
        };
        let ev = compute_new_events(&snap, &HashMap::new(), &mut store, "t");
        assert!(ev.is_empty(), "stale release must not emit");
        // …but it is still recorded so it never emits later either.
        assert!(store.releases.contains_key(&release_key(&snap.releases[0])));
    }

    #[test]
    fn ci_failure_requires_fail_status_and_matching_author() {
        let mut store = seeded_store();
        let mut viewers = HashMap::new();
        viewers.insert("acc".to_string(), "bjoernw".to_string());

        // Passing run → no event.
        let ok = FetchSnapshot {
            ci: vec![ci(CiStatus::Ok, Some("bjoernw"), Some("acc"), "u1")],
            ..Default::default()
        };
        assert!(compute_new_events(&ok, &viewers, &mut store, "t").is_empty());

        // Failure triggered by someone else → no event.
        let other = FetchSnapshot {
            ci: vec![ci(CiStatus::Fail, Some("someoneelse"), Some("acc"), "u2")],
            ..Default::default()
        };
        assert!(compute_new_events(&other, &viewers, &mut store, "t").is_empty());

        // Failure I triggered (case-insensitive match) → one event.
        let mine = FetchSnapshot {
            ci: vec![ci(CiStatus::Fail, Some("BjoernW"), Some("acc"), "u3")],
            ..Default::default()
        };
        let ev = compute_new_events(&mine, &viewers, &mut store, "t");
        assert_eq!(ev.len(), 1);
        assert!(matches!(ev[0], Kind::CiFailure { .. }));

        // Same still-failing run on the next tick → no second event.
        assert!(compute_new_events(&mine, &viewers, &mut store, "t").is_empty());
    }

    #[test]
    fn ci_failure_skips_when_author_or_viewer_missing() {
        let mut store = seeded_store();
        // No author surfaced by the provider → skip.
        let no_author = FetchSnapshot {
            ci: vec![ci(CiStatus::Fail, None, Some("acc"), "u")],
            ..Default::default()
        };
        assert!(compute_new_events(&no_author, &HashMap::new(), &mut store, "t").is_empty());

        // Author present but the account has no known viewer login → skip.
        let no_viewer = FetchSnapshot {
            ci: vec![ci(CiStatus::Fail, Some("me"), Some("acc"), "u")],
            ..Default::default()
        };
        assert!(compute_new_events(&no_viewer, &HashMap::new(), &mut store, "t").is_empty());
    }

    #[test]
    fn key_functions_namespace_by_account() {
        let w = waiting("42", "github:github.com:me");
        assert_eq!(waiting_key(&w), "github:github.com:me:42");

        let r = release("v2", "acc", true);
        assert_eq!(release_key(&r), "acc:o/r:v2");

        let c = ci(CiStatus::Fail, Some("me"), Some("acc"), "https://run/1");
        assert_eq!(ci_failure_key(&c), "acc:https://run/1");

        // Without a URL the key falls back to repo:branch so a still-failing
        // run on a branch collapses to one key.
        let mut c2 = c.clone();
        c2.html_url = None;
        assert_eq!(ci_failure_key(&c2), "acc:o/r:main");
    }

    #[test]
    fn tag_extend_stamps_account_id() {
        let mut out: Vec<WaitingItem> = Vec::new();
        let mut item = waiting("1", "placeholder");
        item.account_id = None; // provider leaves it unset
        tag_extend(&mut out, vec![item], "acc-x");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].account_id.as_deref(), Some("acc-x"));
    }
}
