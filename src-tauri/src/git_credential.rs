//! gitBuddy as a git credential helper, so `git push` works in any terminal
//! for clones of a connected account without SSH keys or a separately stored
//! token. See the 2026-09-25 entry in docs/DECISIONS.md.
//!
//! Two halves:
//!
//! - **Configuration** — [`configure`] writes a helper entry into one clone's
//!   `.git/config`, scoped to the account's forge URL. `clone_repo` does it for
//!   every authenticated clone; `enable_git_push` does it for a clone that
//!   already exists.
//! - **The helper itself** — git runs the gitBuddy binary as
//!   `gitbuddy credential --account <id> --host <host> get` and reads a
//!   username/password pair from its stdout (the git credential protocol,
//!   `gitcredentials(7)`). [`run_from_args`] handles that before the GUI
//!   starts, so the app itself does not need to be running.
//!
//! The helper answers only for HTTPS requests to the host it was configured
//! for; for anything else it stays silent and git moves on to its next helper
//! or a prompt.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::Path;

/// First CLI argument that selects helper mode in `main`.
pub const SUBCOMMAND: &str = "credential";

/// The part of the helper command that identifies it as ours — what
/// [`is_configured`] looks for in a clone's config.
const MARKER: &str = "' credential --account '";

/// Handle a helper invocation, if this is one. Returns the process exit code
/// when `args` (as from `std::env::args`) select helper mode, `None` to start
/// the app normally.
pub fn run_from_args(args: &[String]) -> Option<i32> {
    if args.get(1).map(String::as_str) != Some(SUBCOMMAND) {
        return None;
    }
    let helper = match HelperArgs::parse(&args[2..]) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("gitbuddy credential: {e}");
            return Some(1);
        }
    };
    // `store` and `erase` are git telling helpers about a credential that
    // worked or failed. There is nothing to store (the Keychain entry is the
    // app's), and erasing it because one push was rejected would disconnect
    // the account — so both are acknowledged and ignored, as are operations a
    // future git might add.
    if helper.operation != "get" {
        return Some(0);
    }
    let mut input = String::new();
    if std::io::stdin().read_to_string(&mut input).is_err() {
        return Some(0);
    }
    let request = parse_request(&input);
    let response = answer(&request, &helper, || {
        match crate::keychain::load_blocking(&helper.account_id) {
            Ok(secret) => secret.map(token_from_secret),
            Err(e) => {
                eprintln!(
                    "gitbuddy credential: couldn't read the token for {} from the Keychain: {e}",
                    helper.account_id
                );
                None
            }
        }
    });
    if let Some(out) = response {
        let mut stdout = std::io::stdout();
        let _ = stdout.write_all(out.as_bytes());
        let _ = stdout.flush();
    }
    Some(0)
}

#[derive(Debug, PartialEq, Eq)]
struct HelperArgs {
    /// `Account.id`: `<slug>:<host>:<login>`.
    account_id: String,
    /// `host[:port]` the helper may answer for — the account's forge.
    host: String,
    /// `get`, `store` or `erase`, appended by git.
    operation: String,
}

impl HelperArgs {
    fn parse(args: &[String]) -> Result<Self, String> {
        let mut account_id = None;
        let mut host = None;
        let mut operation = None;
        let mut it = args.iter();
        while let Some(a) = it.next() {
            match a.as_str() {
                "--account" => account_id = it.next().cloned(),
                "--host" => host = it.next().cloned(),
                other if operation.is_none() && !other.starts_with("--") => {
                    operation = Some(other.to_string())
                }
                other => return Err(format!("unexpected argument {other:?}")),
            }
        }
        let account_id = account_id.ok_or("missing --account")?;
        if account_id.split(':').count() != 3 {
            return Err(format!("malformed account id {account_id:?}"));
        }
        Ok(Self {
            account_id,
            host: host.ok_or("missing --host")?.to_ascii_lowercase(),
            operation: operation.ok_or("missing operation")?,
        })
    }

    /// The login the token belongs to — the last segment of the account id.
    fn login(&self) -> &str {
        self.account_id.rsplit(':').next().unwrap_or_default()
    }
}

/// Parse git's `key=value` lines (terminated by a blank line or EOF).
fn parse_request(input: &str) -> HashMap<String, String> {
    input
        .lines()
        .take_while(|l| !l.is_empty())
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// The `get` response, or `None` when this helper must not answer.
///
/// `token` is only called once the request has been checked, so a request
/// for a foreign host never touches the Keychain.
fn answer(
    request: &HashMap<String, String>,
    helper: &HelperArgs,
    token: impl FnOnce() -> Option<String>,
) -> Option<String> {
    // Only HTTPS: over plain HTTP the token would cross the wire in clear.
    if request.get("protocol").map(String::as_str) != Some("https") {
        return None;
    }
    // The config entry is already URL-scoped to this host; checking again
    // here means a hand-copied or stale entry still can't leak the token to
    // another server.
    if request.get("host").map(|h| h.to_ascii_lowercase()) != Some(helper.host.clone()) {
        return None;
    }
    // A remote URL that names a different user wants that user's
    // credentials, not this account's.
    if let Some(user) = request.get("username") {
        if !user.eq_ignore_ascii_case(helper.login()) {
            return None;
        }
    }
    let token = token()?;
    if token.is_empty() || token.contains('\n') {
        return None;
    }
    Some(format!("username={}\npassword={token}\n", helper.login()))
}

/// The bare token inside a Keychain entry. PAT accounts store the token
/// itself; OAuth accounts store an `OAuthTokens` JSON blob. The helper runs
/// without `accounts.json`, so it tells them apart by shape — a token is
/// never a JSON object.
fn token_from_secret(raw: String) -> String {
    if raw.trim_start().starts_with('{') {
        if let Ok(t) = serde_json::from_str::<crate::oauth::OAuthTokens>(&raw) {
            return t.access_token;
        }
    }
    raw
}

// ── Configuration ─────────────────────────────────────────────────────────

/// Where an account's forge lives, as the helper config needs it.
#[derive(Debug, PartialEq, Eq)]
pub struct HelperScope {
    /// URL prefix the config entry is scoped to (`credential.<url>.helper`):
    /// `https://github.com`, or the account's base URL — including a path
    /// prefix for an instance under a relative URL root.
    pub url: String,
    /// `host[:port]` as git will report it in the request.
    pub host: String,
}

impl HelperScope {
    /// `base_url` is `ProviderBackend::base_url()` — `None` for GitHub.
    pub fn for_forge(base_url: Option<&str>) -> Option<Self> {
        let url = base_url
            .unwrap_or("https://github.com")
            .trim_end_matches('/')
            .to_string();
        let parsed = reqwest::Url::parse(&url).ok()?;
        if parsed.scheme() != "https" {
            return None;
        }
        let host = match parsed.port() {
            Some(port) => format!("{}:{port}", parsed.host_str()?),
            None => parsed.host_str()?.to_string(),
        };
        Some(Self { url, host })
    }
}

/// The helper command git runs, quoted for the shell git hands it to (the
/// leading `!`). `exe` is the gitBuddy binary.
pub fn helper_command(exe: &Path, account_id: &str, host: &str) -> String {
    format!(
        "!{} {SUBCOMMAND} --account {} --host {}",
        shell_quote(&exe.to_string_lossy()),
        shell_quote(account_id),
        shell_quote(host)
    )
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Why the running binary can't serve as the helper, if it can't. macOS runs
/// an app that was opened straight from Downloads from a randomised,
/// read-only copy ("App Translocation"); a helper path pointing there stops
/// working at the next launch.
pub fn unusable_exe(exe: &Path) -> Option<&'static str> {
    exe.to_string_lossy()
        .contains("/AppTranslocation/")
        .then_some("Move gitBuddy into your Applications folder and reopen it first — macOS is running it from a temporary copy.")
}

/// Make `repo` ask gitBuddy for credentials for `scope`.
///
/// Writes, in the clone's own `.git/config` only:
///
/// ```text
/// [credential "https://github.com"]
///     helper =
///     helper = !'/Applications/gitBuddy.app/Contents/MacOS/gitbuddy' credential --account '…' --host 'github.com'
/// ```
///
/// The empty entry resets the helper list inherited from the global and
/// system config for this URL — otherwise the macOS Keychain helper Apple's
/// git ships with would answer first with whatever it has stored for the
/// host, and would also copy gitBuddy's token into its own Keychain item on
/// every successful push. Re-running replaces the previous entries, so it is
/// safe to call for a clone that is already set up.
pub fn configure(
    repo: &git2::Repository,
    scope: &HelperScope,
    command: &str,
) -> Result<(), String> {
    let mut cfg = repo
        .config()
        .and_then(|c| c.open_level(git2::ConfigLevel::Local))
        .map_err(|e| format!("Couldn't open the clone's git config: {e}"))?;
    let key = format!("credential.{}.helper", scope.url);
    // Nothing to remove on a fresh clone; that "not found" is expected.
    let _ = cfg.remove_multivar(&key, ".*");
    // `set_multivar` replaces entries whose value matches the regex and
    // appends when none does. Nothing exists after the removal, so the first
    // call appends the empty reset; the second regex can't match that empty
    // value, so it appends the command after it.
    cfg.set_multivar(&key, "^$", "")
        .and_then(|_| cfg.set_multivar(&key, "^!", command))
        .map_err(|e| format!("Couldn't write the clone's git config: {e}"))
}

/// Whether `repo`'s own config routes credentials through gitBuddy.
pub fn is_configured(repo: &git2::Repository) -> bool {
    let Ok(cfg) = repo
        .config()
        .and_then(|c| c.open_level(git2::ConfigLevel::Local))
    else {
        return false;
    };
    let Ok(mut entries) = cfg.entries(Some(r"^credential\..*\.helper$")) else {
        return false;
    };
    let mut found = false;
    while let Some(Ok(entry)) = entries.next() {
        if entry.value().is_some_and(|v| v.contains(MARKER)) {
            found = true;
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn helper() -> HelperArgs {
        HelperArgs {
            account_id: "github:github.com:soron2038".into(),
            host: "github.com".into(),
            operation: "get".into(),
        }
    }

    fn request(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn only_the_credential_subcommand_selects_helper_mode() {
        assert_eq!(run_from_args(&args(&["gitbuddy"])), None);
        assert_eq!(run_from_args(&args(&["gitbuddy", "-psn_0_1234"])), None);
        // Malformed helper invocations fail rather than launching the GUI.
        assert_eq!(run_from_args(&args(&["gitbuddy", "credential"])), Some(1));
        // store/erase never touch stdin or the Keychain.
        let store = args(&[
            "gitbuddy",
            "credential",
            "--account",
            "github:github.com:x",
            "--host",
            "github.com",
            "store",
        ]);
        assert_eq!(run_from_args(&store), Some(0));
    }

    #[test]
    fn parses_what_git_appends_to_the_configured_command() {
        let parsed = HelperArgs::parse(&args(&[
            "--account",
            "gitlab:gitlab.mpsd.mpg.de:witt",
            "--host",
            "GitLab.MPSD.mpg.de",
            "get",
        ]))
        .unwrap();
        assert_eq!(
            parsed,
            HelperArgs {
                account_id: "gitlab:gitlab.mpsd.mpg.de:witt".into(),
                host: "gitlab.mpsd.mpg.de".into(),
                operation: "get".into(),
            }
        );
        assert_eq!(parsed.login(), "witt");
        assert!(
            HelperArgs::parse(&args(&["--account", "nocolons", "--host", "h", "get"])).is_err()
        );
        assert!(HelperArgs::parse(&args(&["--account", "a:b:c", "get"])).is_err());
        assert!(HelperArgs::parse(&args(&["--account", "a:b:c", "--host", "h"])).is_err());
    }

    #[test]
    fn reads_the_request_up_to_the_blank_line() {
        let r = parse_request("protocol=https\nhost=github.com\n\nignored=1\n");
        assert_eq!(r.len(), 2);
        assert_eq!(r["host"], "github.com");
        // Values may contain '='.
        assert_eq!(parse_request("path=a=b\n")["path"], "a=b");
    }

    #[test]
    fn answers_https_requests_for_its_own_host() {
        let out = answer(
            &request(&[("protocol", "https"), ("host", "github.com")]),
            &helper(),
            || Some("tok".into()),
        );
        assert_eq!(out.as_deref(), Some("username=soron2038\npassword=tok\n"));
        // A username in the remote URL that matches the account is fine.
        let out = answer(
            &request(&[
                ("protocol", "https"),
                ("host", "GITHUB.com"),
                ("username", "Soron2038"),
            ]),
            &helper(),
            || Some("tok".into()),
        );
        assert!(out.is_some());
    }

    #[test]
    fn stays_silent_and_never_reads_the_keychain_for_anything_else() {
        let refuse = [
            request(&[("protocol", "http"), ("host", "github.com")]),
            request(&[("protocol", "https"), ("host", "evil.example")]),
            request(&[("protocol", "https"), ("host", "github.com:8443")]),
            request(&[
                ("protocol", "https"),
                ("host", "github.com"),
                ("username", "someone-else"),
            ]),
            request(&[("host", "github.com")]),
        ];
        for r in refuse {
            let out = answer(&r, &helper(), || panic!("Keychain read for {r:?}"));
            assert_eq!(out, None);
        }
    }

    #[test]
    fn no_token_or_a_malformed_one_means_no_answer() {
        let r = request(&[("protocol", "https"), ("host", "github.com")]);
        assert_eq!(answer(&r, &helper(), || None), None);
        assert_eq!(answer(&r, &helper(), || Some(String::new())), None);
        // A newline would let the value inject protocol lines.
        assert_eq!(answer(&r, &helper(), || Some("a\nhost=x".into())), None);
    }

    #[test]
    fn unpacks_oauth_blobs_and_passes_pats_through() {
        assert_eq!(token_from_secret("glpat-abc".into()), "glpat-abc");
        let blob = r#"{"access_token":"gho_x","token_type":"bearer","scope":"repo","obtained_at":"2026-01-01T00:00:00Z"}"#;
        assert_eq!(token_from_secret(blob.into()), "gho_x");
    }

    #[test]
    fn scope_follows_the_forge_base_url() {
        assert_eq!(
            HelperScope::for_forge(None),
            Some(HelperScope {
                url: "https://github.com".into(),
                host: "github.com".into()
            })
        );
        assert_eq!(
            HelperScope::for_forge(Some("https://git.example.com:8443/gitlab/")),
            Some(HelperScope {
                url: "https://git.example.com:8443/gitlab".into(),
                host: "git.example.com:8443".into()
            })
        );
        assert_eq!(
            HelperScope::for_forge(Some("http://gitlab.example.com")),
            None
        );
    }

    #[test]
    fn the_command_survives_the_shell() {
        let cmd = helper_command(
            Path::new("/Applications/git Buddy's.app/Contents/MacOS/gitbuddy"),
            "github:github.com:soron2038",
            "github.com",
        );
        assert_eq!(
            cmd,
            r#"!'/Applications/git Buddy'\''s.app/Contents/MacOS/gitbuddy' credential --account 'github:github.com:soron2038' --host 'github.com'"#
        );
        assert!(cmd.contains(MARKER));
    }

    #[test]
    fn translocated_apps_are_refused() {
        assert!(unusable_exe(Path::new(
            "/private/var/folders/x/T/AppTranslocation/ABC/d/gitBuddy.app/Contents/MacOS/gitbuddy"
        ))
        .is_some());
        assert!(unusable_exe(Path::new(
            "/Applications/gitBuddy.app/Contents/MacOS/gitbuddy"
        ))
        .is_none());
    }

    fn helper_values(repo: &git2::Repository, key: &str) -> Vec<String> {
        let cfg = repo
            .config()
            .unwrap()
            .open_level(git2::ConfigLevel::Local)
            .unwrap();
        let mut out = Vec::new();
        let mut entries = cfg.multivar(key, None).unwrap();
        while let Some(Ok(e)) = entries.next() {
            out.push(e.value().unwrap_or_default().to_string());
        }
        out
    }

    #[test]
    fn configure_writes_a_reset_then_the_helper_and_is_idempotent() {
        let dir = TempDir::new().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        assert!(!is_configured(&repo));
        let scope = HelperScope::for_forge(None).unwrap();
        let cmd = helper_command(
            Path::new("/Applications/gitBuddy.app/Contents/MacOS/gitbuddy"),
            "github:github.com:x",
            "github.com",
        );

        configure(&repo, &scope, &cmd).unwrap();
        configure(&repo, &scope, &cmd).unwrap();

        assert_eq!(
            helper_values(&repo, "credential.https://github.com.helper"),
            vec![String::new(), cmd.clone()]
        );
        assert!(is_configured(&repo));
    }

    /// The config must mean to git what we intend: a helper configured for
    /// github.com is consulted for github.com with exactly the arguments
    /// `HelperArgs::parse` expects, it wins over a helper inherited from the
    /// user's global config (on a Mac, the osxkeychain helper Apple's git
    /// ships with), and other hosts still get the inherited one. Runs only
    /// where a `git` binary exists; the app itself never shells out to git.
    #[test]
    fn git_itself_resolves_the_helper_for_the_scoped_url_only() {
        let Ok(out) = std::process::Command::new("git").arg("--version").output() else {
            return;
        };
        if !out.status.success() {
            return;
        }
        let home = TempDir::new().unwrap();
        let dir = TempDir::new().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();

        // Stand-ins: our "gitbuddy" echoes the argv git gave it back as the
        // password; the global helper answers for any host.
        let script = |name: &str, body: &str| {
            let path = home.path().join(name);
            std::fs::write(&path, format!("#!/bin/sh\ncat >/dev/null\n{body}\n")).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
            path
        };
        let fake_gitbuddy = script(
            "fake gitbuddy",
            r#"printf 'username=u\npassword=%s\n' "$*""#,
        );
        let global = script(
            "global-helper",
            r#"printf 'username=g\npassword=from-global\n'"#,
        );
        std::fs::write(
            home.path().join(".gitconfig"),
            format!("[credential]\n\thelper = \"!{}\"\n", global.display()),
        )
        .unwrap();

        let scope = HelperScope::for_forge(None).unwrap();
        let command = helper_command(&fake_gitbuddy, "github:github.com:x", &scope.host);
        configure(&repo, &scope, &command).unwrap();

        let fill = |url: &str| {
            let mut child = std::process::Command::new("git")
                .args(["credential", "fill"])
                .current_dir(dir.path())
                .env("HOME", home.path())
                .env("XDG_CONFIG_HOME", home.path())
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_TERMINAL_PROMPT", "0")
                .env_remove("GIT_ASKPASS")
                .env_remove("SSH_ASKPASS")
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .spawn()
                .unwrap();
            child
                .stdin
                .take()
                .unwrap()
                .write_all(format!("url={url}\n\n").as_bytes())
                .unwrap();
            let out = String::from_utf8(child.wait_with_output().unwrap().stdout).unwrap();
            out.lines()
                .find_map(|l| l.strip_prefix("password="))
                .unwrap_or_default()
                .to_string()
        };

        // Scoped host: our helper, not the global one — and its argv, after
        // the shell has unquoted it and git has appended the operation, is
        // exactly what `run_from_args` parses.
        let argv = fill("https://github.com/o/r.git");
        assert_eq!(
            argv,
            "credential --account github:github.com:x --host github.com get"
        );
        let parsed: Vec<String> = argv.split(' ').skip(1).map(String::from).collect();
        assert_eq!(
            HelperArgs::parse(&parsed).unwrap().account_id,
            "github:github.com:x"
        );
        // Any other host is untouched. (A host nobody has a Keychain item
        // for, in case this runs where git also has the osxkeychain helper.)
        assert_eq!(fill("https://example.invalid/o/r.git"), "from-global");
    }
}
