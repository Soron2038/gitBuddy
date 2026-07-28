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
    provider_util::{cmp_newest_first, ProviderBackend, ProviderError},
    settings::{self, NotificationSettings, Settings},
    types::{CiRun, CiStatus, ItemReason, Release, Repo, WaitingItem},
};
use chrono::Utc;
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};
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
    let (mut settings, mut backoff) = tick(app, state).await;
    loop {
        // When a forge told us how long to wait, respect it — polling the same
        // endpoints again on the user's normal cadence while being throttled
        // just deepens the hole. Never *shorter* than the configured interval.
        let sleep_for = poll_interval(&settings)
            .max(backoff.map(Duration::from_secs).unwrap_or(Duration::ZERO));
        tokio::select! {
            _ = tokio::time::sleep(sleep_for) => {}
            _ = state.refresh_trigger.notified() => {
                // Manual refresh or auth change — tick immediately.
            }
            _ = state.settings_reload.notified() => {
                // Settings changed. Re-read them so a new poll interval takes
                // effect on the *current* sleep, then go straight back to
                // sleeping — deliberately without ticking.
                //
                // This arm used to fall through into the tick, which meant
                // every settings mutation triggered a full provider fan-out
                // across every account plus a complete scan-root disk walk.
                // Since the UI persists on each change (and the poll-interval
                // slider fires on `input`), dragging that slider from 5 to 45
                // burned dozens of full syncs — the exact rate-limit spend the
                // setting exists to control. Changes that alter the *data*
                // still refresh explicitly: `import_config` calls
                // `refresh_now`, as do the auth commands.
                settings = settings::load(app).unwrap_or_default();
                continue;
            }
        }
        (settings, backoff) = tick(app, state).await;
    }
}

/// External entry point so commands can request an immediate tick without
/// importing `Notify` directly. Fire-and-forget — the actual tick runs in
/// the polling task and surfaces its result via `data-updated`.
pub fn refresh_now(state: &AppState) {
    state.refresh_trigger.notify_one();
}

/// Run a single fetch + cache write + diff + notify + event emit.
/// Returns the settings the tick ran with (so `run_loop` needs no second disk
/// read) plus any backoff a provider asked for.
async fn tick(app: &AppHandle, state: &AppState) -> (Settings, Option<u64>) {
    // One settings read per tick: the fetch (scan roots), the notification
    // gates and the caller's sleep interval all see the same values. A load
    // failure falls back to defaults for the gates — the worst case is a
    // one-tick over-notify, which is preferable to skipping notifications
    // altogether on a transient disk hiccup.
    // Reconnect anything that isn't in the registry yet. Normally a no-op
    // (one file read), but it's what recovers an account whose restore failed
    // at launch because the machine was offline — without it that account
    // stays dead until the app is restarted.
    state.restore_missing(app).await;

    let loaded = settings::load(app);
    let mut snapshot = fetch_all(state, &loaded).await;
    let settings = loaded.unwrap_or_default();

    let synced_at = Utc::now().to_rfc3339();
    let now_ts = synced_at.clone();

    // Carry the previous tick's rows over for every account whose fetch
    // failed, then sort — the merge order is task-completion order, and the
    // carried-over rows are appended after the fresh ones, so both need the
    // sort below to produce a stable, chronological list.
    {
        let cache = state.cache.read().await;
        carry_over(
            &mut snapshot.waiting,
            &cache.waiting,
            &snapshot.failed,
            "list_waiting",
        );
        carry_over(
            &mut snapshot.repos,
            &cache.repos,
            &snapshot.failed,
            "list_repos",
        );
        carry_over(
            &mut snapshot.releases,
            &cache.releases,
            &snapshot.failed,
            "list_releases",
        );
        carry_over(&mut snapshot.ci, &cache.ci, &snapshot.failed, "list_ci");
    }

    // Waiting items and releases most-recent first (the popover's
    // expectation); repos by last push. Timestamps are compared as parsed
    // instants, not strings — see `cmp_newest_first`.
    snapshot
        .waiting
        .sort_by(|a, b| cmp_newest_first(&a.updated_at, &b.updated_at));
    // `pushed_at` is optional (a repo with no commits has none); the empty
    // string is unparseable and therefore sorts last, which is what we want.
    snapshot.repos.sort_by(|a, b| {
        cmp_newest_first(
            a.pushed_at.as_deref().unwrap_or(""),
            b.pushed_at.as_deref().unwrap_or(""),
        )
    });
    snapshot
        .releases
        .sort_by(|a, b| cmp_newest_first(&a.published_at, &b.published_at));

    let mut store = notifications::load(app);

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

    // A tick that reached no provider successfully has learned nothing: it
    // must not seed the cold-start baseline (or a fresh install would seed an
    // empty world and then notify about the user's entire backlog on the first
    // tick that does reach the network), and it must not refresh
    // `last_synced_at` (or the footer would claim a sync that never happened).
    // "No providers connected at all" is not a failure — that tick is
    // authoritative and correctly says "nothing to sync".
    let authoritative = snapshot.ok_count > 0 || snapshot.failed.is_empty();
    let backoff = snapshot.backoff_secs;

    diff_and_notify(
        app,
        &settings.notifications,
        &snapshot,
        &viewer_logins,
        &mut store,
        &now_ts,
        authoritative,
    );

    // Prune *after* the diff, never before. Pruning first drops any entry past
    // the TTL and the very same tick re-inserts it as unseen, so a review
    // request that has been open longer than the TTL gets re-notified as
    // "new" — and again every TTL thereafter. Running the diff first means an
    // item that is still visible was just refreshed and can never be pruned;
    // only genuinely gone items age out.
    notifications::prune(&mut store);

    if let Err(e) = notifications::save(app, &store) {
        eprintln!("gitbuddy: persisting notifications.json failed: {e}");
    }

    {
        let mut cache = state.cache.write().await;
        cache.waiting = snapshot.waiting;
        cache.repos = snapshot.repos;
        cache.releases = snapshot.releases;
        cache.ci = snapshot.ci;
        // `None` = the scan didn't run; keep whatever the last good tick found.
        if let Some(locals) = snapshot.locals {
            cache.locals = locals;
        }
        if authoritative {
            cache.last_synced_at = Some(synced_at.clone());
        }
        cache.last_error = if snapshot.errors.is_empty() {
            None
        } else {
            Some(snapshot.errors.join(" · "))
        };
    }

    if let Err(e) = app.emit("data-updated", DataUpdatedPayload { synced_at }) {
        eprintln!("gitbuddy: emitting data-updated failed: {e}");
    }

    (settings, backoff)
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
    seed_ready: bool,
) {
    for kind in compute_new_events(snapshot, viewer_logins, store, now_ts, seed_ready) {
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
/// install / upgrade never replays a backlog. `seed_ready` gates that flip: a
/// tick that reached no provider has nothing to seed *from*, and flipping the
/// flag anyway would make the first tick that does reach the network replay
/// the user's whole backlog as "new".
fn compute_new_events(
    snapshot: &FetchSnapshot,
    viewer_logins: &HashMap<String, String>,
    store: &mut SeenStore,
    now_ts: &str,
    seed_ready: bool,
) -> Vec<Kind> {
    let cold_start = !store.initialised;
    let mut events = Vec::new();

    // Every sighting refreshes the stored timestamp, so it means "last seen"
    // rather than "first seen". The TTL prune that runs after this diff can
    // then only expire items that have genuinely disappeared — with a
    // first-seen stamp, an item open longer than the TTL would be pruned and
    // immediately re-notified as new, over and over.
    for item in &snapshot.waiting {
        let key = waiting_key(item);
        let already_seen = store.waiting.contains_key(&key);
        store.waiting.insert(key, now_ts.to_string());
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
        store.releases.insert(key, now_ts.to_string());
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
        store.ci_failures.insert(key, now_ts.to_string());
        if !cold_start && !already_seen {
            events.push(Kind::CiFailure {
                repo: run.repo_full_name.clone(),
                branch: run.branch.clone().unwrap_or_else(|| "main".to_string()),
            });
        }
    }

    if cold_start && seed_ready {
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
    /// `None` when the local scan didn't run (settings unreadable) or panicked,
    /// so `tick` can leave the previous local list in the cache instead of
    /// blanking the "Local clones" view on a momentary glitch.
    locals: Option<Vec<LocalRepo>>,
    /// Every `(account id, list name)` pair whose fetch failed this tick.
    /// `tick` carries the previous tick's rows over for each of these, so a
    /// broken account keeps showing its last-known data instead of silently
    /// emptying — and so a still-working account is unaffected.
    failed: HashSet<(String, &'static str)>,
    /// Human-readable failures to surface in `cache.last_error`, deduplicated
    /// by `(account, message)` — all four list calls for one dead token
    /// produce the same message and would otherwise be repeated four times.
    errors: Vec<String>,
    /// Count of provider list calls that returned `Ok` this tick. Zero with a
    /// non-empty `failed` means the tick learned nothing and must not be
    /// treated as authoritative (no fresh `last_synced_at`, no cold-start
    /// seed).
    ok_count: usize,
    /// Longest `Retry-After` any provider asked for this tick, in seconds.
    /// The loop waits at least this long before the next tick instead of
    /// hammering the same endpoints on the user's normal cadence.
    backoff_secs: Option<u64>,
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
        // Four near-identical calls rather than a loop: the element types
        // differ, so there is no array that holds all four.
        let w = merge_result(&mut snapshot.waiting, waiting, &id);
        snapshot.record(w, &id, "list_waiting");
        let r = merge_result(&mut snapshot.repos, repos, &id);
        snapshot.record(r, &id, "list_repos");
        let rel = merge_result(&mut snapshot.releases, releases, &id);
        snapshot.record(rel, &id, "list_releases");
        let c = merge_result(&mut snapshot.ci, ci, &id);
        snapshot.record(c, &id, "list_ci");
    }

    // Local index scan — runs on a blocking thread because libgit2 is
    // synchronous. On failure `locals` stays `None` so `tick` leaves the
    // cache's prior local list intact and a momentary scan glitch doesn't
    // blank the "Local clones" view. When settings failed to load we skip the
    // scan rather than scanning default roots the user may have removed.
    match settings {
        Ok(s) => {
            let s = s.clone();
            match tokio::task::spawn_blocking(move || local_index::scan(&s)).await {
                Ok(v) => snapshot.locals = Some(v),
                Err(e) => snapshot
                    .errors
                    .push(format!("Local scan task panicked: {e}")),
            }
        }
        Err(e) => snapshot
            .errors
            .push(format!("Loading settings failed: {e}")),
    }

    snapshot
}

/// Fold one provider list result into the snapshot: `Ok` extends the list
/// (stamping the account id), `Err` is recorded without aborting the tick.
///
/// Every error is surfaced, not just rate limiting. The pre-2026-07 version
/// only propagated `RateLimited` and logged the rest to stderr — which a
/// bundled `.app` discards — so a revoked token or an SSO-enforced 401 blanked
/// every list while the UI kept reporting "Synced just now". An error the user
/// cannot act on is still better than a silent lie about having no work.
/// Splitting the fold from the bookkeeping keeps the generic part free of the
/// snapshot's four accumulators — passing all of them alongside `out` made for
/// an eight-argument function.
fn merge_result<T: Tagged>(
    out: &mut Vec<T>,
    res: Result<Vec<T>, ProviderError>,
    id: &str,
) -> Result<(), ProviderError> {
    match res {
        Ok(v) => {
            tag_extend(out, v, id);
            Ok(())
        }
        Err(e) => Err(e),
    }
}

impl FetchSnapshot {
    /// Record the outcome of one provider list call.
    fn record(&mut self, outcome: Result<(), ProviderError>, id: &str, what: &'static str) {
        let Err(e) = outcome else {
            self.ok_count += 1;
            return;
        };
        eprintln!("gitbuddy: {what}[{id}] failed: {e}");
        if let ProviderError::RateLimited {
            retry_after_secs: Some(secs),
            ..
        } = e
        {
            self.backoff_secs = Some(self.backoff_secs.unwrap_or(0).max(secs));
        }
        self.failed.insert((id.to_string(), what));
        // One dead token fails all four list calls with the same message;
        // repeating it four times in the UI banner helps nobody.
        let msg = format!("{id}: {e}");
        if !self.errors.contains(&msg) {
            self.errors.push(msg);
        }
    }
}

/// Re-attach the previous tick's rows for every `(account, list)` pair whose
/// fetch failed this tick. Without this a single failing account empties its
/// own section of every list; with it, the last-known rows stay visible and
/// `cache.last_error` explains why they aren't advancing.
fn carry_over<T: Tagged + Clone>(
    fresh: &mut Vec<T>,
    previous: &[T],
    failed: &HashSet<(String, &'static str)>,
    what: &'static str,
) {
    if failed.is_empty() {
        return;
    }
    fresh.extend(
        previous
            .iter()
            .filter(|it| {
                it.account_id()
                    .is_some_and(|id| failed.contains(&(id.to_string(), what)))
            })
            .cloned(),
    );
}

/// Items the aggregator stamps with the account id that surfaced them, so the
/// UI can show per-account badges and the diff/notify pass can key by account.
trait Tagged {
    fn set_account_id(&mut self, id: &str);
    /// Reading it back is what lets [`carry_over`] pick out exactly the rows
    /// belonging to an account whose fetch failed.
    fn account_id(&self) -> Option<&str>;
}
impl Tagged for WaitingItem {
    fn set_account_id(&mut self, id: &str) {
        self.account_id = Some(id.to_string());
    }
    fn account_id(&self) -> Option<&str> {
        self.account_id.as_deref()
    }
}
impl Tagged for Repo {
    fn set_account_id(&mut self, id: &str) {
        self.account_id = Some(id.to_string());
    }
    fn account_id(&self) -> Option<&str> {
        self.account_id.as_deref()
    }
}
impl Tagged for Release {
    fn set_account_id(&mut self, id: &str) {
        self.account_id = Some(id.to_string());
    }
    fn account_id(&self) -> Option<&str> {
        self.account_id.as_deref()
    }
}
impl Tagged for CiRun {
    fn set_account_id(&mut self, id: &str) {
        self.account_id = Some(id.to_string());
    }
    fn account_id(&self) -> Option<&str> {
        self.account_id.as_deref()
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
            true,
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
        let ev1 = compute_new_events(&snap1, &HashMap::new(), &mut store, "t1", true);
        assert_eq!(ev1.len(), 1, "first sighting of item 1 emits");
        assert!(matches!(ev1[0], Kind::Waiting { .. }));

        // Same item again → already seen → nothing.
        let ev2 = compute_new_events(&snap1, &HashMap::new(), &mut store, "t2", true);
        assert!(ev2.is_empty(), "re-seeing the same item must not emit");

        // A brand-new item alongside the old one → only the new one emits.
        let snap3 = FetchSnapshot {
            waiting: vec![waiting("1", "acc"), waiting("2", "acc")],
            ..Default::default()
        };
        let ev3 = compute_new_events(&snap3, &HashMap::new(), &mut store, "t3", true);
        assert_eq!(ev3.len(), 1, "only the unseen item emits");
    }

    #[test]
    fn same_id_across_accounts_emits_independently() {
        let mut store = seeded_store();
        let snap = FetchSnapshot {
            waiting: vec![waiting("1", "acc-a"), waiting("1", "acc-b")],
            ..Default::default()
        };
        let ev = compute_new_events(&snap, &HashMap::new(), &mut store, "t", true);
        assert_eq!(ev.len(), 2, "the same id via two accounts is two events");
    }

    #[test]
    fn release_emits_only_when_is_new() {
        let mut store = seeded_store();
        let snap = FetchSnapshot {
            releases: vec![release("v1", "acc", false)], // backfill, not new
            ..Default::default()
        };
        let ev = compute_new_events(&snap, &HashMap::new(), &mut store, "t", true);
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
        assert!(compute_new_events(&ok, &viewers, &mut store, "t", true).is_empty());

        // Failure triggered by someone else → no event.
        let other = FetchSnapshot {
            ci: vec![ci(CiStatus::Fail, Some("someoneelse"), Some("acc"), "u2")],
            ..Default::default()
        };
        assert!(compute_new_events(&other, &viewers, &mut store, "t", true).is_empty());

        // Failure I triggered (case-insensitive match) → one event.
        let mine = FetchSnapshot {
            ci: vec![ci(CiStatus::Fail, Some("BjoernW"), Some("acc"), "u3")],
            ..Default::default()
        };
        let ev = compute_new_events(&mine, &viewers, &mut store, "t", true);
        assert_eq!(ev.len(), 1);
        assert!(matches!(ev[0], Kind::CiFailure { .. }));

        // Same still-failing run on the next tick → no second event.
        assert!(compute_new_events(&mine, &viewers, &mut store, "t", true).is_empty());
    }

    #[test]
    fn ci_failure_skips_when_author_or_viewer_missing() {
        let mut store = seeded_store();
        // No author surfaced by the provider → skip.
        let no_author = FetchSnapshot {
            ci: vec![ci(CiStatus::Fail, None, Some("acc"), "u")],
            ..Default::default()
        };
        assert!(compute_new_events(&no_author, &HashMap::new(), &mut store, "t", true).is_empty());

        // Author present but the account has no known viewer login → skip.
        let no_viewer = FetchSnapshot {
            ci: vec![ci(CiStatus::Fail, Some("me"), Some("acc"), "u")],
            ..Default::default()
        };
        assert!(compute_new_events(&no_viewer, &HashMap::new(), &mut store, "t", true).is_empty());
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
    fn cold_start_without_a_reachable_provider_does_not_seed() {
        // The fresh-install shape: the very first tick runs before any account
        // is connected, so it fetches nothing. Flipping `initialised` here
        // would make the first tick that *does* reach the network treat the
        // user's entire backlog as new.
        let mut store = SeenStore::default();
        let empty = FetchSnapshot::default();
        let events = compute_new_events(&empty, &HashMap::new(), &mut store, "t0", false);
        assert!(events.is_empty());
        assert!(
            !store.initialised,
            "a tick that reached no provider must not seed the baseline"
        );

        // Account connected; the next tick reaches the network and finds the
        // backlog. That tick is the seed — still silent, but now it counts.
        let backlog = FetchSnapshot {
            waiting: vec![waiting("1", "acc"), waiting("2", "acc")],
            releases: vec![release("v1", "acc", true)],
            ..Default::default()
        };
        let events = compute_new_events(&backlog, &HashMap::new(), &mut store, "t1", true);
        assert!(events.is_empty(), "the real cold start still emits nothing");
        assert!(store.initialised);

        // And only genuinely new items emit from here on.
        let plus_one = FetchSnapshot {
            waiting: vec![
                waiting("1", "acc"),
                waiting("2", "acc"),
                waiting("3", "acc"),
            ],
            ..Default::default()
        };
        let events = compute_new_events(&plus_one, &HashMap::new(), &mut store, "t2", true);
        assert_eq!(events.len(), 1, "only the item added after the seed emits");
    }

    #[test]
    fn sighting_refreshes_the_seen_timestamp() {
        // The TTL prune keys on this timestamp. If it stayed at first-sight,
        // an item open longer than the TTL would be pruned and re-notified as
        // new on the very next tick, forever.
        let mut store = seeded_store();
        let snap = FetchSnapshot {
            waiting: vec![waiting("1", "acc")],
            ..Default::default()
        };
        compute_new_events(&snap, &HashMap::new(), &mut store, "day-1", true);
        assert_eq!(store.waiting[&waiting_key(&snap.waiting[0])], "day-1");

        compute_new_events(&snap, &HashMap::new(), &mut store, "day-61", true);
        assert_eq!(
            store.waiting[&waiting_key(&snap.waiting[0])],
            "day-61",
            "a still-visible item must carry a fresh last-seen stamp"
        );
    }

    #[test]
    fn carry_over_restores_only_the_failed_account() {
        let previous = vec![waiting("1", "acc-broken"), waiting("2", "acc-fine")];
        // acc-fine fetched cleanly this tick and returned one (newer) item;
        // acc-broken's fetch failed, so it contributed nothing.
        let mut fresh = vec![waiting("9", "acc-fine")];
        let failed = HashSet::from([("acc-broken".to_string(), "list_waiting")]);

        carry_over(&mut fresh, &previous, &failed, "list_waiting");

        let ids: Vec<&str> = fresh.iter().map(|w| w.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["9", "1"],
            "the broken account keeps its last-known row; the healthy one is not duplicated"
        );
    }

    #[test]
    fn carry_over_is_scoped_to_the_failed_list() {
        // list_repos failed for this account, list_waiting did not — the
        // waiting list must not resurrect anything.
        let previous = vec![waiting("1", "acc")];
        let mut fresh: Vec<WaitingItem> = Vec::new();
        let failed = HashSet::from([("acc".to_string(), "list_repos")]);
        carry_over(&mut fresh, &previous, &failed, "list_waiting");
        assert!(fresh.is_empty());
    }

    #[test]
    fn errors_are_all_recorded_not_just_rate_limits() {
        let mut snapshot = FetchSnapshot::default();
        let id = "github:github.com:me";

        let outcome = merge_result(
            &mut snapshot.waiting,
            Err(ProviderError::Unauthorized("check the token's repo scope")),
            id,
        );
        snapshot.record(outcome, id, "list_waiting");

        assert_eq!(snapshot.ok_count, 0);
        assert!(snapshot.failed.contains(&(id.to_string(), "list_waiting")));
        assert_eq!(snapshot.errors.len(), 1);
        assert!(
            snapshot.errors[0].contains("authentication failed"),
            "a dead token must reach the UI, not just stderr: {}",
            snapshot.errors[0]
        );

        // The same failure via another list is deduplicated — one dead token
        // shouldn't render as four identical messages.
        let outcome = merge_result(
            &mut snapshot.repos,
            Err(ProviderError::Unauthorized("check the token's repo scope")),
            id,
        );
        snapshot.record(outcome, id, "list_repos");
        assert_eq!(snapshot.errors.len(), 1, "identical messages collapse");
        assert_eq!(snapshot.failed.len(), 2, "but both lists are marked failed");
    }

    #[test]
    fn rate_limit_retry_after_becomes_the_tick_backoff() {
        let mut snapshot = FetchSnapshot::default();
        for (id, secs) in [("acc-a", 30u64), ("acc-b", 90), ("acc-c", 10)] {
            let outcome: Result<(), ProviderError> = Err(ProviderError::RateLimited {
                provider: "GitHub",
                retry_after_secs: Some(secs),
            });
            snapshot.record(outcome, id, "list_repos");
        }
        assert_eq!(
            snapshot.backoff_secs,
            Some(90),
            "the loop waits for the most patient forge"
        );
    }

    #[test]
    fn timestamps_sort_across_provider_formats() {
        // Gitea renders in the instance's timezone, GitLab appends millis,
        // GitHub uses plain Z. Sorted newest-first these must interleave by
        // real instant, not by byte order.
        let mut v = vec![
            "2026-06-01T14:00:00+02:00", // 12:00 UTC — Gitea
            "2026-06-01T13:00:00Z",      // 13:00 UTC — GitHub
            "2026-06-01T12:30:00.000Z",  // 12:30 UTC — GitLab
        ];
        v.sort_by(|a, b| cmp_newest_first(a, b));
        assert_eq!(
            v,
            vec![
                "2026-06-01T13:00:00Z",
                "2026-06-01T12:30:00.000Z",
                "2026-06-01T14:00:00+02:00",
            ]
        );
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
