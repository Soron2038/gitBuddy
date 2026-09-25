//! Fetching release assets straight into the Downloads folder.
//!
//! The browser would do this too, but only where it happens to hold a signed-in
//! session: a private repo's installer opened from a machine whose browser was
//! never logged in to the forge lands on a sign-in page. The app already holds
//! the account token, so it downloads the file itself and falls back to the
//! browser only when the forge insists on a session.
//!
//! The request (URL + credentials) comes from
//! `ProviderBackend::asset_request`, which is where the rule "the token only
//! goes to the account's own forge" lives. This module only moves bytes.

use crate::provider_util::{is_rate_limited, USER_AGENT};
use reqwest::{Client, RequestBuilder, Response, StatusCode};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::AsyncWriteExt;

/// Outcome of a download attempt that didn't fail outright.
#[derive(Debug, PartialEq, Eq)]
pub enum Fetched {
    Saved(PathBuf),
    /// The forge answered with a sign-in page or refused the token for this
    /// file — something a browser session may still get past. Not an error:
    /// the caller hands the browser URL over instead.
    NeedsBrowser,
}

/// A client for downloads. Separate from `provider_util::http_client`
/// because that one carries a 30-second *total* deadline, which a large
/// installer on a slow line would blow through. A stalled transfer is caught
/// by the per-read timeout instead.
pub fn download_client() -> reqwest::Result<Client> {
    Client::builder()
        .user_agent(USER_AGENT)
        .connect_timeout(Duration::from_secs(10))
        .read_timeout(Duration::from_secs(60))
        .build()
}

/// Send `req` and stream the body into `dir` under (a sanitised,
/// de-duplicated form of) `file_name`.
///
/// The body is written to a hidden part file next to the target and renamed
/// into place only once complete, so an interrupted download never leaves a
/// truncated file that looks finished.
pub async fn fetch_to(req: RequestBuilder, dir: &Path, file_name: &str) -> Result<Fetched, String> {
    let mut resp = req
        .send()
        .await
        .map_err(|e| format!("Download failed: {e}"))?;
    let status = resp.status();

    if is_rate_limited(status, resp.headers()) {
        return Err("The forge is rate-limiting requests — try again in a minute.".into());
    }
    // 401/403: the token isn't accepted for this route. 404: GitLab's
    // uploads API on an instance older than 17.4, or a file only a session
    // can see. All three are worth a try in the browser.
    if matches!(
        status,
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN | StatusCode::NOT_FOUND
    ) {
        return Ok(Fetched::NeedsBrowser);
    }
    if !status.is_success() {
        return Err(format!("Download failed: HTTP {status}"));
    }
    if is_sign_in_page(&resp, file_name) {
        return Ok(Fetched::NeedsBrowser);
    }

    let name = safe_file_name(file_name);
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let part = dir.join(format!(".{name}.{nonce}.gitbuddy-part"));

    let written = write_body(&mut resp, &part).await;
    if let Err(e) = written {
        let _ = tokio::fs::remove_file(&part).await;
        return Err(e);
    }

    let target = unique_target(dir, &name);
    if let Err(e) = tokio::fs::rename(&part, &target).await {
        let _ = tokio::fs::remove_file(&part).await;
        return Err(format!("Couldn't save {}: {e}", target.display()));
    }
    mark_quarantined(&target);
    Ok(Fetched::Saved(target))
}

async fn write_body(resp: &mut Response, part: &Path) -> Result<(), String> {
    let mut file = tokio::fs::File::create(part)
        .await
        .map_err(|e| format!("Couldn't create {}: {e}", part.display()))?;
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| format!("Download interrupted: {e}"))?
    {
        file.write_all(&chunk)
            .await
            .map_err(|e| format!("Couldn't write the download: {e}"))?;
    }
    file.flush()
        .await
        .map_err(|e| format!("Couldn't write the download: {e}"))
}

/// A 200 that is really a web sign-in page. GitLab redirects an
/// unauthenticated upload request to `/users/sign_in`, and Gitea to
/// `/user/login`; both end in an HTML page with status 200. Saving that as
/// `app.dmg` would be worse than useless. An asset that is itself an HTML
/// file is taken at its word.
fn is_sign_in_page(resp: &Response, file_name: &str) -> bool {
    let html = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.trim_start().to_ascii_lowercase().starts_with("text/html"));
    let lower = file_name.to_ascii_lowercase();
    html && !(lower.ends_with(".html") || lower.ends_with(".htm"))
}

/// Reduce a publisher-supplied asset name to a plain file name. Asset names
/// come from the forge and are not trusted to be one: a `../` or an absolute
/// path must not steer the write out of the Downloads folder, and a leading
/// dot would hide the file.
pub(crate) fn safe_file_name(raw: &str) -> String {
    let last = raw.rsplit(['/', '\\']).next().unwrap_or("");
    let cleaned: String = last
        .chars()
        .map(|c| if c.is_control() || c == ':' { '_' } else { c })
        .collect();
    let cleaned = cleaned.trim().trim_start_matches('.').to_string();
    if cleaned.is_empty() {
        "download".into()
    } else {
        cleaned
    }
}

/// `dir/name`, or `dir/name (1)`, `(2)`, … when that is taken — the way a
/// browser names a repeated download, keeping the extension (including the
/// compound `.tar.*` ones) at the end so the file still opens.
pub(crate) fn unique_target(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let lower = name.to_ascii_lowercase();
    let split_at = ["tar.gz", "tar.bz2", "tar.xz", "tar.zst"]
        .iter()
        .find(|ext| lower.ends_with(&format!(".{ext}")))
        .map(|ext| name.len() - ext.len() - 1)
        .or_else(|| name.rfind('.').filter(|&i| i > 0));
    let (stem, ext) = match split_at {
        Some(i) => name.split_at(i),
        None => (name, ""),
    };
    (1..)
        .map(|n| dir.join(format!("{stem} ({n}){ext}")))
        .find(|p| !p.exists())
        .expect("an unbounded counter always finds a free name")
}

/// Give the file the quarantine attribute a browser would have set, so
/// Gatekeeper checks a downloaded app or installer exactly as it would after
/// a browser download. Without it, the app would be a quiet way around that
/// check for anything published as a release asset in any repo the account
/// can see. Best-effort: a failure is logged, not surfaced.
#[cfg(target_os = "macos")]
fn mark_quarantined(path: &Path) {
    let stamp = format!(
        "0081;{:08x};gitBuddy;",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or_default()
    );
    let status = std::process::Command::new("/usr/bin/xattr")
        .args(["-w", "com.apple.quarantine", &stamp])
        .arg(path)
        .status();
    if !matches!(status, Ok(s) if s.success()) {
        eprintln!(
            "gitbuddy: couldn't set the quarantine attribute on {}",
            path.display()
        );
    }
}

#[cfg(not(target_os = "macos"))]
fn mark_quarantined(_path: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn file_names_cannot_escape_the_folder() {
        assert_eq!(safe_file_name("app.dmg"), "app.dmg");
        assert_eq!(safe_file_name("../../.zshrc"), "zshrc");
        assert_eq!(safe_file_name("/etc/passwd"), "passwd");
        assert_eq!(safe_file_name("dir\\evil.exe"), "evil.exe");
        assert_eq!(safe_file_name("a:b\u{7}.txt"), "a_b_.txt");
        assert_eq!(safe_file_name(".."), "download");
        assert_eq!(safe_file_name(""), "download");
    }

    #[test]
    fn repeated_downloads_get_numbered_before_the_extension() {
        let dir = TempDir::new().unwrap();
        let d = dir.path();
        assert_eq!(unique_target(d, "app.dmg"), d.join("app.dmg"));
        std::fs::write(d.join("app.dmg"), b"").unwrap();
        assert_eq!(unique_target(d, "app.dmg"), d.join("app (1).dmg"));
        std::fs::write(d.join("app (1).dmg"), b"").unwrap();
        assert_eq!(unique_target(d, "app.dmg"), d.join("app (2).dmg"));

        std::fs::write(d.join("src.tar.gz"), b"").unwrap();
        assert_eq!(unique_target(d, "src.tar.gz"), d.join("src (1).tar.gz"));
        std::fs::write(d.join("README"), b"").unwrap();
        assert_eq!(unique_target(d, "README"), d.join("README (1)"));
        std::fs::write(d.join(".hidden"), b"").unwrap();
        assert_eq!(unique_target(d, ".hidden"), d.join(".hidden (1)"));
    }

    fn get(url: String) -> RequestBuilder {
        download_client().unwrap().get(url)
    }

    #[tokio::test]
    async fn saves_the_body_and_leaves_no_part_file() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/f"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw(b"payload".to_vec(), "application/octet-stream"),
            )
            .mount(&server)
            .await;
        let dir = TempDir::new().unwrap();

        let out = fetch_to(get(format!("{}/f", server.uri())), dir.path(), "app.dmg")
            .await
            .unwrap();

        let saved = dir.path().join("app.dmg");
        assert_eq!(out, Fetched::Saved(saved.clone()));
        assert_eq!(std::fs::read(&saved).unwrap(), b"payload");
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from("app.dmg")]);
    }

    #[tokio::test]
    async fn a_sign_in_page_is_handed_to_the_browser_not_saved() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/f"))
            .respond_with(
                // `set_body_raw`, not `insert_header` + `set_body_string`: the
                // latter resets the content type to text/plain.
                ResponseTemplate::new(200)
                    .set_body_raw("<html>Sign in</html>", "text/html; charset=utf-8"),
            )
            .mount(&server)
            .await;
        let dir = TempDir::new().unwrap();

        let out = fetch_to(get(format!("{}/f", server.uri())), dir.path(), "app.dmg")
            .await
            .unwrap();

        assert_eq!(out, Fetched::NeedsBrowser);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn refused_or_missing_files_fall_back_to_the_browser() {
        for status in [401, 403, 404] {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .respond_with(ResponseTemplate::new(status))
                .mount(&server)
                .await;
            let dir = TempDir::new().unwrap();
            let out = fetch_to(get(server.uri()), dir.path(), "app.dmg").await;
            assert_eq!(out, Ok(Fetched::NeedsBrowser), "HTTP {status}");
        }
    }

    #[tokio::test]
    async fn rate_limiting_and_server_errors_are_errors() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/limited"))
            .respond_with(ResponseTemplate::new(403).insert_header("x-ratelimit-remaining", "0"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/broken"))
            .respond_with(ResponseTemplate::new(502))
            .mount(&server)
            .await;
        let dir = TempDir::new().unwrap();

        let limited = fetch_to(get(format!("{}/limited", server.uri())), dir.path(), "a").await;
        assert!(limited.unwrap_err().contains("rate-limiting"));
        let broken = fetch_to(get(format!("{}/broken", server.uri())), dir.path(), "a").await;
        assert!(broken.unwrap_err().contains("502"));
    }

    /// The property GitHub downloads rely on: the asset API answers with a
    /// redirect to a storage host, and the token must not follow it there.
    #[tokio::test]
    async fn the_token_does_not_follow_a_cross_host_redirect() {
        let storage = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/signed"))
            .respond_with(move |req: &wiremock::Request| {
                if req.headers.contains_key("authorization") {
                    ResponseTemplate::new(400).set_body_string("token leaked")
                } else {
                    ResponseTemplate::new(200).set_body_bytes(b"bytes".to_vec())
                }
            })
            .mount(&storage)
            .await;
        let api = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/asset"))
            .and(header("authorization", "Bearer tok"))
            .respond_with(
                ResponseTemplate::new(302)
                    .insert_header("location", format!("{}/signed", storage.uri())),
            )
            .mount(&api)
            .await;
        let dir = TempDir::new().unwrap();

        let req = get(format!("{}/asset", api.uri())).bearer_auth("tok");
        let out = fetch_to(req, dir.path(), "app.dmg").await.unwrap();

        assert_eq!(out, Fetched::Saved(dir.path().join("app.dmg")));
        assert_eq!(std::fs::read(dir.path().join("app.dmg")).unwrap(), b"bytes");
    }
}
