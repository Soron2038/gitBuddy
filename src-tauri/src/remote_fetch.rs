//! Background `git fetch` for local clones.
//!
//! `local_index` computes ahead/behind against the clone's remote-tracking
//! ref, and that ref only moves when something fetches. Without this module a
//! clone that never sees a manual `git fetch` reports "0 behind" forever, no
//! matter how far its forge has moved on. A loop of its own — on a slower
//! cadence than the API poll, since every round talks to every clone's
//! remote — fetches each clone whose current branch tracks an upstream, then
//! rescans so the counts (and the tray badge) reflect the forge.
//!
//! What a fetch writes is what `git fetch <remote>` writes: objects, the
//! remote-tracking refs the remote's own fetch refspecs name, auto-followed
//! tags, and `FETCH_HEAD`. Never a local branch, the index or the worktree:
//! refspecs whose destination isn't under `refs/remotes/<name>/` or
//! `refs/tags/` are dropped, since libgit2 — unlike git — would happily
//! force-update the checked-out branch.
//!
//! Transport: libgit2 is built without SSH support, so a clone whose remote is
//! `git@host:owner/name` is fetched from its HTTPS twin into the same refs.
//! A token is only ever offered the way `clone_repo` and `asset_request` do
//! it: an account's own token, only to that account's origin, only when the
//! server challenges, and at most once per fetch. Remote URLs come from
//! `.git/config` files nobody vetted (an unpacked archive can ship one), and
//! the origin check parses with the `url` crate while libgit2 connects with
//! its own parser — `https://github.com\@evil.example/…` is github.com to one
//! and evil.example to the other. So a token is only attached to a URL in a
//! shape both read identically (`credential_safe_url`), libgit2 must ask for
//! exactly that URL, and redirects are off whenever a token is attached
//! (libgit2 hands the callback the original URL, not the redirect target).

use std::{
    cell::Cell,
    collections::{HashMap, HashSet},
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use chrono::Utc;
use git2::{
    Cred, CredentialType, FetchOptions, ProxyOptions, RemoteCallbacks, RemoteRedirect, Repository,
};
use tauri::{AppHandle, Emitter};
use tokio::{sync::Semaphore, task::JoinSet};

use crate::{
    aggregator::DataUpdatedPayload,
    commands::AppState,
    git_credential::HelperScope,
    local_index::{self, LocalRepo},
    provider_util::same_origin,
    settings::{self, Settings},
};

/// Fetches in flight at once. Each one is a blocking libgit2 call on its own
/// thread; a handful overlaps the network waits without opening dozens of
/// connections to one forge at the same moment.
const MAX_CONCURRENT_FETCHES: usize = 4;

/// A manual refresh forces a round only when the previous one is at least
/// this old — every auth command and every Refresh click calls
/// `aggregator::refresh_now`, and none of them needs a second full round
/// right after the last one.
const MIN_TRIGGERED_GAP: Duration = Duration::from_secs(60);

/// libgit2 waits forever on a server that accepts the connection and then
/// goes quiet, which would pin a fetch thread (and the round) indefinitely.
const CONNECT_TIMEOUT_MS: i32 = 10_000;
const SERVER_TIMEOUT_MS: i32 = 30_000;

/// Longest a single fetch may keep downloading before it's aborted. The
/// timeouts above only catch a server that goes silent, not one that drips.
const MAX_FETCH_DURATION: Duration = Duration::from_secs(600);

/// How long a round waits for one clone. A fetch libgit2 can't interrupt —
/// a stalled ref advertisement, a hung network mount behind a path remote —
/// keeps its thread, but no longer holds up the round, and the clone is left
/// out of later rounds until that thread returns (see `InFlight`).
const ROUND_TASK_TIMEOUT: Duration = Duration::from_secs(300);

const REFLOG_MESSAGE: &str = "fetch (gitBuddy)";

/// Set libgit2's process-wide network timeouts. Called once from `setup`,
/// before anything — the fetch loop or `clone_repo` — opens a connection.
/// The read timeout is per read, not per transfer, so a long clone that
/// keeps streaming is unaffected.
pub fn init_libgit2_timeouts() {
    // SAFETY: both setters only write a global libgit2 option. They are
    // called once on the main thread during setup, before any libgit2
    // network operation can be running concurrently.
    unsafe {
        if let Err(e) = git2::opts::set_server_connect_timeout_in_milliseconds(CONNECT_TIMEOUT_MS) {
            eprintln!("gitbuddy: setting libgit2 connect timeout failed: {e}");
        }
        if let Err(e) = git2::opts::set_server_timeout_in_milliseconds(SERVER_TIMEOUT_MS) {
            eprintln!("gitbuddy: setting libgit2 server timeout failed: {e}");
        }
    }
}

/// One connected account's token and the origin it may be sent to.
pub(crate) struct Credential {
    pub account_id: String,
    /// `HelperScope::url` — `https://github.com` or the account's base URL.
    pub origin: String,
    pub token: String,
}

/// Everything a fetch round needs from the async side, snapshotted once so
/// the blocking fetches hold no locks.
#[derive(Default)]
pub(crate) struct FetchContext {
    pub creds: Vec<Credential>,
    /// Local-index join key (`host:owner/name`, lowercase — the same key the
    /// frontend uses) → the account whose repo list contains that repo.
    pub owners: HashMap<String, String>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    Fetched,
    /// Nothing to fetch: detached HEAD, no upstream, or a remote URL we
    /// can't turn into something libgit2 can reach.
    Skipped,
}

/// The fetch loop. Spawned by `aggregator::spawn_loop` after the first tick,
/// so the provider registry and the repo cache it picks tokens from are
/// already populated.
pub async fn run_loop(app: AppHandle, state: Arc<AppState>) {
    let mut last_round: Option<Instant> = None;
    let in_flight = InFlight::default();
    loop {
        // A settings file that won't load means no fetching — the same
        // "don't act on defaults the user may have changed" call the local
        // scan makes.
        let loaded = settings::load(&app).ok();
        let enabled = loaded.as_ref().is_some_and(|s| s.remote_fetch_enabled);
        let interval = fetch_interval(loaded.as_ref());

        if enabled {
            let due = last_round.is_none_or(|t| t.elapsed() >= interval);
            if due {
                if let Some(s) = &loaded {
                    round(&app, &state, s, &in_flight).await;
                }
                last_round = Some(Instant::now());
            }
        } else if !state.fetch_errors.read().await.is_empty() {
            // Switched off: errors from the last round would otherwise stay
            // on screen for good. They drop off at the next scan.
            state.fetch_errors.write().await.clear();
        }

        let wait = match last_round {
            Some(t) if enabled => interval.saturating_sub(t.elapsed()),
            _ => interval,
        };
        tokio::select! {
            _ = tokio::time::sleep(wait) => {}
            _ = state.fetch_trigger.notified() => {
                if last_round.is_some_and(|t| t.elapsed() >= MIN_TRIGGERED_GAP) {
                    last_round = None;
                }
            }
            // Re-read the toggle and interval; whether a round is due is
            // decided at the top of the loop, so dragging the interval slider
            // doesn't fire a round per step.
            _ = state.fetch_reload.notified() => {}
        }
    }
}

fn fetch_interval(settings: Option<&Settings>) -> Duration {
    let minutes = settings
        .map(|s| s.remote_fetch_interval_minutes)
        .unwrap_or(settings::FETCH_INTERVAL_DEFAULT);
    Duration::from_secs(u64::from(minutes) * 60)
}

/// Paths whose fetch thread is still running, possibly from an earlier round
/// that gave up waiting on it. Never fetch the same clone twice at once.
#[derive(Clone, Default)]
struct InFlight(Arc<Mutex<HashSet<String>>>);

impl InFlight {
    /// Claim `path`; `None` if a fetch of it is still running.
    fn claim(&self, path: &str) -> Option<Claim> {
        let mut set = self.0.lock().unwrap_or_else(|e| e.into_inner());
        set.insert(path.to_string()).then(|| Claim {
            set: self.clone(),
            path: path.to_string(),
        })
    }
}

/// Releases its path when the fetch thread finishes, however it finishes.
struct Claim {
    set: InFlight,
    path: String,
}

impl Drop for Claim {
    fn drop(&mut self) {
        let mut set = self.set.0.lock().unwrap_or_else(|e| e.into_inner());
        set.remove(&self.path);
    }
}

/// Fetch every cached clone, then rescan and publish the fresh counts.
async fn round(app: &AppHandle, state: &Arc<AppState>, settings: &Settings, in_flight: &InFlight) {
    let ctx = Arc::new(context(state).await);
    let paths: Vec<String> = state
        .cache
        .read()
        .await
        .locals
        .iter()
        .filter(|l| l.branch.is_some() && !l.detached)
        .map(|l| l.path.clone())
        .collect();

    let gate = Arc::new(Semaphore::new(MAX_CONCURRENT_FETCHES));
    let mut tasks = JoinSet::new();
    for path in paths {
        let Some(claim) = in_flight.claim(&path) else {
            eprintln!("gitbuddy: previous fetch of {path} still running, skipping it");
            continue;
        };
        let ctx = ctx.clone();
        let gate = gate.clone();
        tasks.spawn(async move {
            // The semaphore is never closed, so acquiring can't fail.
            let _permit = gate.acquire_owned().await.ok();
            let job = tokio::task::spawn_blocking({
                let path = path.clone();
                move || {
                    let _claim = claim;
                    fetch_clone(Path::new(&path), &ctx)
                }
            });
            let result = match tokio::time::timeout(ROUND_TASK_TIMEOUT, job).await {
                Ok(Ok(r)) => r,
                Ok(Err(e)) => Err(format!("fetch task panicked: {e}")),
                Err(_) => Err("The remote didn't finish answering in time.".to_string()),
            };
            (path, result)
        });
    }

    let mut errors = HashMap::new();
    while let Some(joined) = tasks.join_next().await {
        match joined {
            Ok((path, Err(e))) => {
                eprintln!("gitbuddy: background fetch of {path} failed: {e}");
                errors.insert(path, e);
            }
            Ok(_) => {}
            Err(e) => eprintln!("gitbuddy: fetch task failed: {e}"),
        }
    }
    *state.fetch_errors.write().await = errors;

    let s = settings.clone();
    match tokio::task::spawn_blocking(move || local_index::scan(&s)).await {
        Ok(locals) => state.store_locals(app, locals).await,
        Err(e) => eprintln!("gitbuddy: rescan after fetch panicked: {e}"),
    }

    // Both windows re-read every list on `data-updated`; the provider sync
    // time is unchanged, so report that rather than claiming a fresh sync.
    let synced_at = state
        .cache
        .read()
        .await
        .last_synced_at
        .clone()
        .unwrap_or_else(|| Utc::now().to_rfc3339());
    if let Err(e) = app.emit("data-updated", DataUpdatedPayload { synced_at }) {
        eprintln!("gitbuddy: emitting data-updated failed: {e}");
    }
}

async fn context(state: &AppState) -> FetchContext {
    let creds = state
        .providers
        .read()
        .await
        .iter()
        .filter_map(|(id, p)| {
            Some(Credential {
                account_id: id.clone(),
                origin: HelperScope::for_forge(p.base_url())?.url,
                token: p.token().to_string(),
            })
        })
        .collect();
    let mut owners = HashMap::new();
    for r in &state.cache.read().await.repos {
        let (Some(account), Ok(url)) = (&r.account_id, reqwest::Url::parse(&r.html_url)) else {
            continue;
        };
        let Some(host) = url.host_str() else { continue };
        owners
            .entry(join_key(host, &r.owner, &r.name))
            .or_insert_with(|| account.clone());
    }
    FetchContext { creds, owners }
}

fn join_key(host: &str, owner: &str, name: &str) -> String {
    format!("{host}:{owner}/{name}").to_lowercase()
}

/// Fill in each clone's `fetch_error` from the last round's results.
pub(crate) fn annotate(locals: &mut [LocalRepo], errors: &HashMap<String, String>) {
    for l in locals {
        l.fetch_error = errors.get(&l.path).cloned();
    }
}

/// How many clones are behind their upstream — the tray badge.
pub(crate) fn behind_count(locals: &[LocalRepo]) -> usize {
    locals.iter().filter(|l| l.behind > 0).count()
}

/// The URL to fetch `raw` (a remote's configured URL) from, or `None` if
/// there is no form of it libgit2 can reach.
///
/// HTTP(S) and local paths pass through. SSH and `git://` remotes are
/// rewritten to HTTPS on the same host — onto the matching account's origin
/// when one is connected, which carries an instance's port or relative URL
/// root, and onto `https://<host>` otherwise (enough for public repos).
pub(crate) fn fetch_url(raw: &str, origins: &[&str]) -> Option<String> {
    let raw = raw.trim();
    if ["https://", "http://", "file://", "/"]
        .iter()
        .any(|p| raw.starts_with(p))
    {
        return Some(raw.to_string());
    }
    let r = local_index::parse_remote_url(raw)?;
    let base = origins
        .iter()
        .find(|o| {
            reqwest::Url::parse(o)
                .ok()
                .and_then(|u| u.host_str().map(|h| h.eq_ignore_ascii_case(&r.host)))
                .unwrap_or(false)
        })
        .map(|o| o.trim_end_matches('/').to_string())
        .unwrap_or_else(|| format!("https://{}", r.host));
    Some(format!("{base}/{}/{}.git", r.owner, r.name))
}

/// `url` if it may carry a token: `https://host[:port]/path` with nothing in
/// it that libgit2's URL parser and the `url` crate's (which `same_origin`
/// uses) could disagree on — no userinfo, no `\`, `?` or `#`, no non-ASCII
/// or IPv6 host. libgit2 ends the authority at the first `/` and takes the
/// host after the last `@`; WHATWG also ends it at `\`, `?` and `#`.
pub(crate) fn credential_safe_url(url: &str) -> Option<&str> {
    let rest = url.strip_prefix("https://")?;
    let (authority, path) = rest.split_once('/')?;
    let authority_ok = !authority.is_empty()
        && authority
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b':'));
    let path_ok = path
        .bytes()
        .all(|b| b.is_ascii_graphic() && !matches!(b, b'\\' | b'?' | b'#' | b'@'));
    if !authority_ok || !path_ok {
        return None;
    }
    // Belt and braces: the `url` crate must read the same `host[:port]`.
    let parsed = reqwest::Url::parse(url).ok()?;
    let host = parsed.host_str()?;
    let reparsed = match parsed.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_string(),
    };
    reparsed.eq_ignore_ascii_case(authority).then_some(url)
}

/// The credential to offer for `url`: one whose origin `url` is on,
/// preferring the account that lists this repo (`key`) — with two GitHub
/// accounts, only one of them may see a private repo.
pub(crate) fn pick_credential<'a>(
    url: &str,
    key: Option<&str>,
    ctx: &'a FetchContext,
) -> Option<&'a Credential> {
    let owner = key.and_then(|k| ctx.owners.get(k));
    let mut candidates = ctx.creds.iter().filter(|c| same_origin(url, &c.origin));
    match owner {
        Some(id) => ctx
            .creds
            .iter()
            .find(|c| &c.account_id == id && same_origin(url, &c.origin))
            .or_else(|| candidates.next()),
        None => candidates.next(),
    }
}

/// Fetch the remote `path`'s current branch tracks. Blocking.
pub(crate) fn fetch_clone(path: &Path, ctx: &FetchContext) -> Result<Outcome, String> {
    let repo = Repository::open(path).map_err(|e| format!("couldn't open the clone: {e}"))?;
    let Some(remote_name) = upstream_remote(&repo) else {
        return Ok(Outcome::Skipped);
    };
    let raw = repo
        .find_remote(&remote_name)
        .ok()
        .and_then(|r| r.url().map(str::to_string));
    let Some(raw) = raw else {
        return Ok(Outcome::Skipped);
    };
    let origins: Vec<&str> = ctx.creds.iter().map(|c| c.origin.as_str()).collect();
    let Some(url) = fetch_url(&raw, &origins) else {
        return Ok(Outcome::Skipped);
    };
    let key = local_index::parse_remote_url(&raw).map(|r| join_key(&r.host, &r.owner, &r.name));
    // Any other URL is still fetched — anonymously, which is all a public
    // repo needs.
    let cred = credential_safe_url(&url).and_then(|u| pick_credential(u, key.as_deref(), ctx));
    fetch_remote(&repo, &remote_name, &url, cred)?;
    Ok(Outcome::Fetched)
}

/// The remote the checked-out branch's upstream lives on. `None` for a
/// detached or unborn HEAD and for a branch that tracks nothing — there is
/// no "behind" to refresh for those.
fn upstream_remote(repo: &Repository) -> Option<String> {
    let head = repo.head().ok()?;
    if !head.is_branch() {
        return None;
    }
    let buf = repo.branch_upstream_remote(head.name()?).ok()?;
    buf.as_str().map(str::to_string)
}

/// The remote's fetch refspecs, minus any that could write outside
/// `refs/remotes/<name>/` and `refs/tags/` (a mirror-style
/// `+refs/heads/*:refs/heads/*` would otherwise move the checked-out branch).
/// Falls back to git's default refspec when nothing survives.
pub(crate) fn safe_refspecs(specs: &[String], remote_name: &str) -> Vec<String> {
    let own = format!("refs/remotes/{remote_name}/");
    let kept: Vec<String> = specs
        .iter()
        .filter(|spec| {
            let spec = spec.strip_prefix('+').unwrap_or(spec);
            match spec.split_once(':') {
                Some((src, dst)) => {
                    !src.starts_with('^')
                        && (dst.starts_with(&own) || dst.starts_with("refs/tags/"))
                }
                None => false,
            }
        })
        .cloned()
        .collect();
    if kept.is_empty() {
        vec![format!("+refs/heads/*:{own}*")]
    } else {
        kept
    }
}

/// Fetch `remote_name`'s `safe_refspecs` from `url`, through an anonymous
/// remote even when `url` is the remote's own: fetching the *named* remote
/// makes libgit2 also apply its configured refspecs "opportunistically" to
/// whatever it fetched, which would bring the filtered-out ones straight back.
/// The anonymous remote lands in the same `refs/remotes/<name>/*`.
fn fetch_remote(
    repo: &Repository,
    remote_name: &str,
    url: &str,
    cred: Option<&Credential>,
) -> Result<(), String> {
    let remote = repo
        .find_remote(remote_name)
        .map_err(|e| format!("remote {remote_name}: {e}"))?;

    let challenged = Cell::new(false);
    let offered = Cell::new(false);
    let started = Instant::now();
    let mut callbacks = RemoteCallbacks::new();
    callbacks.credentials(|asked_url, _username, allowed| {
        challenged.set(true);
        match cred {
            // Exactly the URL that passed `credential_safe_url` — not just
            // one on the same origin.
            Some(c)
                if !offered.get()
                    && allowed.contains(CredentialType::USER_PASS_PLAINTEXT)
                    && asked_url == url =>
            {
                offered.set(true);
                // Same username `clone_repo` uses; every forge accepts it
                // for a token.
                Cred::userpass_plaintext("oauth2", &c.token)
            }
            _ => Err(git2::Error::from_str("no usable credentials")),
        }
    });
    callbacks.transfer_progress(|_| started.elapsed() < MAX_FETCH_DURATION);
    let mut proxy = ProxyOptions::new();
    proxy.auto();
    let mut fo = FetchOptions::new();
    fo.remote_callbacks(callbacks).proxy_options(proxy);
    if cred.is_some() {
        // A redirect target would get the token: libgit2 asks the callback
        // with the original URL, so the check above can't see where it goes.
        fo.follow_redirects(RemoteRedirect::None);
    }

    let configured: Vec<String> = remote
        .fetch_refspecs()
        .map(|specs| specs.iter().flatten().map(str::to_string).collect())
        .unwrap_or_default();
    let refspecs = safe_refspecs(&configured, remote_name);
    let result = repo
        .remote_anonymous(url)
        .and_then(|mut anon| anon.fetch(&refspecs, Some(&mut fo), Some(REFLOG_MESSAGE)));

    result.map_err(|e| {
        if challenged.get() && offered.get() {
            "The forge rejected the account's token for this repo (revoked, or missing repo access?)."
                .to_string()
        } else if challenged.get() {
            "This repo needs authentication, and no connected account can sign in to its host."
                .to_string()
        } else {
            e.message().to_string()
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn cred(id: &str, origin: &str) -> Credential {
        Credential {
            account_id: id.into(),
            origin: origin.into(),
            token: format!("tok-{id}"),
        }
    }

    #[test]
    fn fetch_url_passes_https_and_paths_through() {
        let o: &[&str] = &[];
        for raw in [
            "https://github.com/o/r.git",
            "http://git.local/o/r",
            "file:///srv/git/r.git",
            "/srv/git/r.git",
        ] {
            assert_eq!(fetch_url(raw, o).as_deref(), Some(raw));
        }
    }

    #[test]
    fn fetch_url_rewrites_ssh_to_https() {
        let o: &[&str] = &[];
        assert_eq!(
            fetch_url("git@github.com:owner/repo.git", o).as_deref(),
            Some("https://github.com/owner/repo.git")
        );
        // The SSH port is not the HTTPS port.
        assert_eq!(
            fetch_url("ssh://git@gitlab.example.com:2222/group/sub/proj.git", o).as_deref(),
            Some("https://gitlab.example.com/group/sub/proj.git")
        );
        assert_eq!(
            fetch_url("git://codeberg.org/o/r", o).as_deref(),
            Some("https://codeberg.org/o/r.git")
        );
    }

    #[test]
    fn fetch_url_uses_the_accounts_origin_for_its_host() {
        // An instance under a relative URL root or on a custom port is only
        // reachable at its account's base URL, not at https://<host>.
        let o = ["https://git.example.org:8443/gitlab/"];
        assert_eq!(
            fetch_url("git@git.example.org:team/proj.git", &o).as_deref(),
            Some("https://git.example.org:8443/gitlab/team/proj.git")
        );
    }

    #[test]
    fn fetch_url_gives_up_on_unparseable_remotes() {
        assert_eq!(fetch_url("gh:owner/repo", &[]), None);
        assert_eq!(fetch_url("", &[]), None);
    }

    #[test]
    fn pick_credential_only_matches_the_url_origin() {
        let ctx = FetchContext {
            creds: vec![
                cred("gh", "https://github.com"),
                cred("gl", "https://gitlab.gwdg.de"),
            ],
            owners: HashMap::new(),
        };
        let pick = |url| pick_credential(url, None, &ctx).map(|c| c.account_id.as_str());
        assert_eq!(pick("https://gitlab.gwdg.de/g/p.git"), Some("gl"));
        assert_eq!(pick("https://github.com/o/r.git"), Some("gh"));
        // Never in clear, never to a lookalike host.
        assert_eq!(pick("http://github.com/o/r.git"), None);
        assert_eq!(pick("https://github.com.evil.example/o/r.git"), None);
        assert_eq!(pick("https://github.com@evil.example/o/r.git"), None);
    }

    #[test]
    fn credential_safe_url_rejects_parser_differentials() {
        assert_eq!(
            credential_safe_url("https://github.com/o/r.git"),
            Some("https://github.com/o/r.git")
        );
        assert!(credential_safe_url("https://git.example.org:8443/gitlab/team/p.git").is_some());
        for bad in [
            // `url` reads github.com, libgit2 connects to evil.example.
            "https://github.com\\@evil.example/o/r.git",
            "https://github.com?@evil.example/o/r.git",
            "https://github.com#@evil.example/o/r.git",
            "https://user@github.com/o/r.git",
            "https://github.com@evil.example/o/r.git",
            // Fullwidth letters that WHATWG folds to github.com.
            "https://ｇｉｔｈｕｂ.com/o/r.git",
            "https://[::1]/o/r.git",
            "https://github.com/o/r.git?x=1",
            "https://github.com/o/r\\x.git",
            "http://github.com/o/r.git",
            "https://github.com",
        ] {
            assert_eq!(credential_safe_url(bad), None, "{bad}");
        }
        // The scp form of the same trick rewrites into a rejected URL.
        let rewritten = fetch_url("git@github.com\\@evil.example:o/r.git", &[]).unwrap();
        assert_eq!(credential_safe_url(&rewritten), None, "{rewritten}");
    }

    #[test]
    fn safe_refspecs_keep_remote_tracking_and_tags_only() {
        let specs = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            safe_refspecs(
                &specs(&[
                    "+refs/heads/*:refs/remotes/origin/*",
                    "+refs/heads/*:refs/heads/*",
                    "refs/tags/*:refs/tags/*",
                    "+refs/heads/main:refs/remotes/other/main",
                    "^refs/heads/wip:refs/remotes/origin/wip",
                    "refs/heads/main",
                ]),
                "origin"
            ),
            specs(&[
                "+refs/heads/*:refs/remotes/origin/*",
                "refs/tags/*:refs/tags/*"
            ])
        );
        // A mirror config keeps nothing — git's default takes over.
        assert_eq!(
            safe_refspecs(&specs(&["+refs/*:refs/*"]), "origin"),
            specs(&["+refs/heads/*:refs/remotes/origin/*"])
        );
    }

    #[test]
    fn mirror_refspec_does_not_move_the_checked_out_branch() {
        let (_tmp, upstream, clone) = upstream_and_clone();
        clone
            .config()
            .unwrap()
            .set_str("remote.origin.fetch", "+refs/heads/*:refs/heads/*")
            .unwrap();
        let before = clone.head().unwrap().target();
        commit_file(&upstream, "b.txt", "2");
        fetch_clone(clone.workdir().unwrap(), &FetchContext::default()).unwrap();
        assert_eq!(clone.head().unwrap().target(), before);
        // The commits landed in the remote-tracking ref instead.
        let branch = clone.head().unwrap().shorthand().unwrap().to_string();
        assert_eq!(
            clone
                .refname_to_id(&format!("refs/remotes/origin/{branch}"))
                .ok(),
            upstream.head().unwrap().target()
        );
    }

    #[test]
    fn in_flight_claims_are_exclusive_until_dropped() {
        let set = InFlight::default();
        let claim = set.claim("/a").expect("first claim");
        assert!(set.claim("/a").is_none());
        assert!(set.claim("/b").is_some());
        drop(claim);
        assert!(set.claim("/a").is_some());
    }

    #[test]
    fn pick_credential_prefers_the_account_that_lists_the_repo() {
        let mut owners = HashMap::new();
        owners.insert("github.com:work/secret".to_string(), "gh-work".to_string());
        let ctx = FetchContext {
            creds: vec![
                cred("gh-home", "https://github.com"),
                cred("gh-work", "https://github.com"),
            ],
            owners,
        };
        let url = "https://github.com/work/secret.git";
        assert_eq!(
            pick_credential(url, Some("github.com:work/secret"), &ctx)
                .map(|c| c.account_id.as_str()),
            Some("gh-work")
        );
        // Unknown repo: any account on that origin.
        assert_eq!(
            pick_credential(url, Some("github.com:x/y"), &ctx).map(|c| c.account_id.as_str()),
            Some("gh-home")
        );
    }

    // ── Real fetches between fixture repos on disk ─────────────────────

    fn init_repo(dir: &Path) -> Repository {
        let repo = Repository::init(dir).unwrap();
        let mut cfg = repo.config().unwrap();
        cfg.set_str("user.name", "Test").unwrap();
        cfg.set_str("user.email", "test@example.com").unwrap();
        repo
    }

    fn commit_file(repo: &Repository, name: &str, content: &str) {
        let workdir = repo.workdir().unwrap();
        fs::write(workdir.join(name), content).unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new(name)).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = repo.signature().unwrap();
        let parent = repo.head().ok().and_then(|h| h.peel_to_commit().ok());
        let parents: Vec<&git2::Commit> = parent.iter().collect();
        repo.commit(Some("HEAD"), &sig, &sig, name, &tree, &parents)
            .unwrap();
    }

    /// An upstream repo with one commit and a clone of it, tracking its
    /// branch. Returns the temp dir (keep it alive) and both repos.
    fn upstream_and_clone() -> (TempDir, Repository, Repository) {
        let tmp = tempfile::tempdir().unwrap();
        let upstream = init_repo(&tmp.path().join("upstream"));
        commit_file(&upstream, "a.txt", "1");
        let clone = Repository::clone(
            tmp.path().join("upstream").to_str().unwrap(),
            tmp.path().join("clone"),
        )
        .unwrap();
        (tmp, upstream, clone)
    }

    fn behind(clone: &Repository) -> u32 {
        let settings = Settings {
            scan_roots: vec![clone.workdir().unwrap().to_path_buf()],
            ..Settings::default()
        };
        local_index::scan(&settings)[0].behind
    }

    #[test]
    fn fetch_brings_the_behind_count_up_to_date() {
        let (_tmp, upstream, clone) = upstream_and_clone();
        commit_file(&upstream, "b.txt", "2");
        commit_file(&upstream, "c.txt", "3");
        assert_eq!(behind(&clone), 0, "nothing fetched yet");

        let outcome = fetch_clone(clone.workdir().unwrap(), &FetchContext::default()).unwrap();
        assert_eq!(outcome, Outcome::Fetched);
        assert_eq!(behind(&clone), 2);
        // Only the tracking ref moved — the local branch and worktree did not.
        assert!(!clone.workdir().unwrap().join("b.txt").exists());
    }

    #[test]
    fn rewritten_url_fetches_into_the_named_remotes_refs() {
        // The SSH case: the configured URL is unreachable for libgit2, so the
        // fetch goes to another URL (here the upstream's path) through an
        // anonymous remote — and must still land in refs/remotes/origin/*.
        let (tmp, upstream, clone) = upstream_and_clone();
        clone
            .remote_set_url("origin", "git@example.invalid:owner/upstream.git")
            .unwrap();
        commit_file(&upstream, "b.txt", "2");

        let url = tmp.path().join("upstream").to_string_lossy().into_owned();
        fetch_remote(&clone, "origin", &url, None).unwrap();
        assert_eq!(behind(&clone), 1);
    }

    #[test]
    fn branch_without_upstream_is_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = init_repo(tmp.path());
        commit_file(&repo, "a.txt", "1");
        assert_eq!(
            fetch_clone(tmp.path(), &FetchContext::default()),
            Ok(Outcome::Skipped)
        );
    }

    #[test]
    fn unreachable_remote_reports_an_error() {
        let (tmp, _upstream, clone) = upstream_and_clone();
        fs::remove_dir_all(tmp.path().join("upstream")).unwrap();
        assert!(fetch_clone(clone.workdir().unwrap(), &FetchContext::default()).is_err());
    }

    #[test]
    fn annotate_and_count() {
        let (_tmp, upstream, clone) = upstream_and_clone();
        commit_file(&upstream, "b.txt", "2");
        fetch_clone(clone.workdir().unwrap(), &FetchContext::default()).unwrap();
        let settings = Settings {
            scan_roots: vec![clone.workdir().unwrap().to_path_buf()],
            ..Settings::default()
        };
        let mut locals = local_index::scan(&settings);
        assert_eq!(behind_count(&locals), 1);
        assert!(locals[0].last_fetched_at.is_some());

        let mut errors = HashMap::new();
        errors.insert(locals[0].path.clone(), "boom".to_string());
        annotate(&mut locals, &errors);
        assert_eq!(locals[0].fetch_error.as_deref(), Some("boom"));
        annotate(&mut locals, &HashMap::new());
        assert_eq!(locals[0].fetch_error, None);
    }
}
