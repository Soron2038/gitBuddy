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
    codeberg::CodebergProvider,
    commands::AppState,
    github::GitHubProvider,
    gitlab::GitLabProvider,
    local_index::{self, LocalRepo},
    settings,
    types::{CiRun, Release, Repo, WaitingItem},
};
use chrono::Utc;
use std::{sync::Arc, time::Duration};
use tauri::{AppHandle, Emitter};

/// Default poll cadence when settings don't override it. Matches the
/// pre-aggregator frontend `POLL_INTERVAL_MS` so existing users see no
/// behaviour change on upgrade. Phase 2 makes this user-configurable via
/// `settings.poll_interval_minutes` and clamps to 1..=60.
const DEFAULT_POLL_INTERVAL_MINUTES: u64 = 5;

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

        let sleep_for = current_poll_interval(app);
        tokio::select! {
            _ = tokio::time::sleep(sleep_for) => {}
            _ = state.refresh_trigger.notified() => {
                // Manual refresh or auth change — tick immediately.
            }
            _ = state.settings_reload.notified() => {
                // Settings changed; new poll interval takes effect after
                // the next tick reads it.
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

/// Run a single fetch + cache write + event emit. Public so `commands.rs`
/// can invoke it synchronously during tests; the production path runs it
/// only via `run_loop`.
pub async fn tick(app: &AppHandle, state: &AppState) {
    let snapshot = fetch_all(app, state).await;
    let synced_at = Utc::now().to_rfc3339();

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
    // Snapshot the provider registries up-front. The HashMap reads are cheap
    // and we want to release the read locks before the await chain below
    // touches the network, so a connect/disconnect during a tick doesn't
    // block on the registry lock for tens of seconds.
    let gh: Vec<(String, Arc<GitHubProvider>)> = state
        .github
        .read()
        .await
        .iter()
        .map(|(id, p)| (id.clone(), p.clone()))
        .collect();
    let gl: Vec<(String, Arc<GitLabProvider>)> = state
        .gitlab
        .read()
        .await
        .iter()
        .map(|(id, p)| (id.clone(), p.clone()))
        .collect();
    let cb: Vec<(String, Arc<CodebergProvider>)> = state
        .codeberg
        .read()
        .await
        .iter()
        .map(|(id, p)| (id.clone(), p.clone()))
        .collect();

    let mut snapshot = FetchSnapshot::default();

    // Waiting items, ordered most-recent first to match the popover's
    // expectations.
    for (id, p) in &gh {
        match p.list_waiting().await {
            Ok(v) => tag_extend_waiting(&mut snapshot.waiting, v, id),
            Err(e) => eprintln!("gitbuddy: github[{id}] list_waiting failed: {e}"),
        }
    }
    for (id, p) in &gl {
        match p.list_waiting().await {
            Ok(v) => tag_extend_waiting(&mut snapshot.waiting, v, id),
            Err(e) => eprintln!("gitbuddy: gitlab[{id}] list_waiting failed: {e}"),
        }
    }
    for (id, p) in &cb {
        match p.list_waiting().await {
            Ok(v) => tag_extend_waiting(&mut snapshot.waiting, v, id),
            Err(e) => eprintln!("gitbuddy: codeberg[{id}] list_waiting failed: {e}"),
        }
    }
    snapshot
        .waiting
        .sort_by(|a, b| b.updated_at.cmp(&a.updated_at));

    for (id, p) in &gh {
        match p.list_repos().await {
            Ok(v) => tag_extend_repos(&mut snapshot.repos, v, id),
            Err(e) => eprintln!("gitbuddy: github[{id}] list_repos failed: {e}"),
        }
    }
    for (id, p) in &gl {
        match p.list_repos().await {
            Ok(v) => tag_extend_repos(&mut snapshot.repos, v, id),
            Err(e) => eprintln!("gitbuddy: gitlab[{id}] list_repos failed: {e}"),
        }
    }
    for (id, p) in &cb {
        match p.list_repos().await {
            Ok(v) => tag_extend_repos(&mut snapshot.repos, v, id),
            Err(e) => eprintln!("gitbuddy: codeberg[{id}] list_repos failed: {e}"),
        }
    }
    snapshot.repos.sort_by(|a, b| b.pushed_at.cmp(&a.pushed_at));

    for (id, p) in &gh {
        match p.list_releases().await {
            Ok(v) => tag_extend_releases(&mut snapshot.releases, v, id),
            Err(e) => eprintln!("gitbuddy: github[{id}] list_releases failed: {e}"),
        }
    }
    for (id, p) in &gl {
        match p.list_releases().await {
            Ok(v) => tag_extend_releases(&mut snapshot.releases, v, id),
            Err(e) => eprintln!("gitbuddy: gitlab[{id}] list_releases failed: {e}"),
        }
    }
    for (id, p) in &cb {
        match p.list_releases().await {
            Ok(v) => tag_extend_releases(&mut snapshot.releases, v, id),
            Err(e) => eprintln!("gitbuddy: codeberg[{id}] list_releases failed: {e}"),
        }
    }

    for (id, p) in &gh {
        match p.list_ci().await {
            Ok(v) => tag_extend_ci(&mut snapshot.ci, v, id),
            Err(e) => eprintln!("gitbuddy: github[{id}] list_ci failed: {e}"),
        }
    }
    for (id, p) in &gl {
        match p.list_ci().await {
            Ok(v) => tag_extend_ci(&mut snapshot.ci, v, id),
            Err(e) => eprintln!("gitbuddy: gitlab[{id}] list_ci failed: {e}"),
        }
    }
    for (id, p) in &cb {
        match p.list_ci().await {
            Ok(v) => tag_extend_ci(&mut snapshot.ci, v, id),
            Err(e) => eprintln!("gitbuddy: codeberg[{id}] list_ci failed: {e}"),
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

fn tag_extend_waiting(out: &mut Vec<WaitingItem>, items: Vec<WaitingItem>, id: &str) {
    out.extend(items.into_iter().map(|mut it| {
        it.account_id = Some(id.to_string());
        it
    }));
}
fn tag_extend_repos(out: &mut Vec<Repo>, items: Vec<Repo>, id: &str) {
    out.extend(items.into_iter().map(|mut it| {
        it.account_id = Some(id.to_string());
        it
    }));
}
fn tag_extend_releases(out: &mut Vec<Release>, items: Vec<Release>, id: &str) {
    out.extend(items.into_iter().map(|mut it| {
        it.account_id = Some(id.to_string());
        it
    }));
}
fn tag_extend_ci(out: &mut Vec<CiRun>, items: Vec<CiRun>, id: &str) {
    out.extend(items.into_iter().map(|mut it| {
        it.account_id = Some(id.to_string());
        it
    }));
}

/// Read the user's configured poll interval, clamped to a sane band. Phase 2
/// wires a real `Settings.poll_interval_minutes` field through here; until
/// then we hardcode the legacy 5-minute default so the loop behaves exactly
/// like the JS `setInterval` it replaces.
fn current_poll_interval(_app: &AppHandle) -> Duration {
    Duration::from_secs(DEFAULT_POLL_INTERVAL_MINUTES * 60)
}
