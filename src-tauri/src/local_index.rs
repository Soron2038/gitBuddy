//! Local repository index — walks configured scan roots, finds every Git
//! checkout, and reports per-clone diagnostics (current branch, dirty file
//! counts, ahead/behind upstream) using libgit2.
//!
//! Pure data layer: this module knows nothing about Tauri or our remote
//! providers. The Svelte frontend matches each `LocalRepo` to the remote
//! list by `(host, owner, name)`.

use git2::{ErrorCode, Repository, StatusOptions};
use serde::Serialize;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

use crate::settings::Settings;

/// Names that are never descended into. Catches the obvious heavy folders
/// (`node_modules`, build outputs, vendored deps) plus macOS junk that can't
/// contain a repo we'd want to surface. Augmented at runtime by
/// `settings.scan_ignore`.
const SKIP_DIRS: &[&str] = &[
    "node_modules",
    ".Trash",
    ".Trashes",
    "Library",
    ".cache",
    "target",
    "build",
    "dist",
    ".next",
    ".svelte-kit",
    "vendor",
    "Pods",
    ".pyenv",
    ".venv",
    "venv",
    "__pycache__",
];

#[derive(Debug, Clone, Serialize)]
pub struct LocalRepo {
    pub path: String,
    pub branch: Option<String>,
    /// Parsed `(host, owner, name)` of the `origin` remote, if any. This is
    /// the join key the frontend uses to attach a local repo to a remote one.
    pub remote: Option<RemoteRef>,
    pub dirty_staged: u32,
    pub dirty_unstaged: u32,
    pub untracked: u32,
    pub ahead: u32,
    pub behind: u32,
    /// Detached HEAD, no current branch — kept as a flag because branchless
    /// state is interesting enough to surface in the UI.
    pub detached: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct RemoteRef {
    pub host: String,
    pub owner: String,
    pub name: String,
    /// The original URL, kept so the UI can show "(via SSH)" / "(via HTTPS)"
    /// or fall back to it if the parse was wonky.
    pub raw_url: String,
}

pub fn scan(settings: &Settings) -> Vec<LocalRepo> {
    let mut extra_skips: Vec<&str> = SKIP_DIRS.to_vec();
    for s in &settings.scan_ignore {
        extra_skips.push(s.as_str());
    }

    let mut found = Vec::new();
    for root in &settings.scan_roots {
        if !root.is_dir() {
            continue;
        }
        find_repos_in(root, &extra_skips, &mut found);
    }

    found
        .into_iter()
        .filter_map(|repo_dir| diagnose(&repo_dir).ok())
        .collect()
}

fn find_repos_in(root: &Path, skip: &[&str], out: &mut Vec<PathBuf>) {
    let walker = WalkDir::new(root).follow_links(false).into_iter();
    let mut iter = walker.filter_entry(|e| should_descend(e, skip));

    while let Some(entry) = iter.next() {
        let Ok(entry) = entry else { continue };
        if !entry.file_type().is_dir() {
            continue;
        }
        if is_repo_root(entry.path()) {
            out.push(entry.path().to_path_buf());
            // Don't descend inside this repo. Sibling clones (e.g.
            // a monorepo-of-checkouts layout) are still walked, but we
            // don't recurse into the repo's own .git subtree.
            iter.skip_current_dir();
        }
    }
}

/// Decides whether `walkdir` should yield (and recurse into) `e`. Files are
/// yielded but never descended into; for directories we filter out the
/// SKIP_DIRS + dot-prefixed paths so we never enter `node_modules`, `.git`,
/// `.cache`, etc.
fn should_descend(e: &walkdir::DirEntry, skip: &[&str]) -> bool {
    if !e.file_type().is_dir() {
        return true;
    }
    // The root of the walk always gets descended.
    if e.depth() == 0 {
        return true;
    }
    let Some(name) = e.file_name().to_str() else {
        return false; // non-UTF8 dir name — skip rather than choke on it
    };
    if skip.contains(&name) {
        return false;
    }
    // Dot-prefixed dirs can't contain a user-level repo and tend to be
    // heavyweight (.cache, .git internals, .DS_Store-adjacent metadata).
    if name.starts_with('.') {
        return false;
    }
    true
}

fn is_repo_root(p: &Path) -> bool {
    let git = p.join(".git");
    // A regular repo has a `.git` directory; a worktree has a `.git` file
    // that points to its real gitdir. Either is a checkout we care about.
    git.is_dir() || git.is_file()
}

fn diagnose(path: &Path) -> Result<LocalRepo, git2::Error> {
    let repo = Repository::open(path)?;

    let (branch, detached) = current_branch(&repo);
    let (staged, unstaged, untracked) = status_counts(&repo)?;
    let (ahead, behind) = ahead_behind(&repo);
    let remote = origin_remote(&repo);

    Ok(LocalRepo {
        path: path.to_string_lossy().into_owned(),
        branch,
        remote,
        dirty_staged: staged,
        dirty_unstaged: unstaged,
        untracked,
        ahead,
        behind,
        detached,
    })
}

fn current_branch(repo: &Repository) -> (Option<String>, bool) {
    match repo.head() {
        Ok(head) => {
            if head.is_branch() {
                (head.shorthand().map(str::to_string), false)
            } else {
                (head.shorthand().map(str::to_string), true)
            }
        }
        Err(e) if e.code() == ErrorCode::UnbornBranch || e.code() == ErrorCode::NotFound => {
            (None, false)
        }
        Err(_) => (None, false),
    }
}

fn status_counts(repo: &Repository) -> Result<(u32, u32, u32), git2::Error> {
    let mut opts = StatusOptions::new();
    opts.include_untracked(true)
        .include_ignored(false)
        .recurse_untracked_dirs(false);
    let statuses = repo.statuses(Some(&mut opts))?;

    let mut staged = 0u32;
    let mut unstaged = 0u32;
    let mut untracked = 0u32;

    for entry in statuses.iter() {
        use git2::Status as S;
        let s = entry.status();
        if s.intersects(
            S::INDEX_NEW
                | S::INDEX_MODIFIED
                | S::INDEX_DELETED
                | S::INDEX_RENAMED
                | S::INDEX_TYPECHANGE,
        ) {
            staged += 1;
        }
        if s.intersects(S::WT_MODIFIED | S::WT_DELETED | S::WT_RENAMED | S::WT_TYPECHANGE) {
            unstaged += 1;
        }
        if s.contains(S::WT_NEW) {
            untracked += 1;
        }
    }

    Ok((staged, unstaged, untracked))
}

fn ahead_behind(repo: &Repository) -> (u32, u32) {
    let Ok(head) = repo.head() else { return (0, 0) };
    let Some(local_oid) = head.target() else {
        return (0, 0);
    };
    let Ok(head_branch) = head.peel_to_commit() else {
        return (0, 0);
    };
    let _ = head_branch; // silence unused warning if peel fails later

    let Some(shorthand) = head.shorthand() else {
        return (0, 0);
    };
    let Ok(branch) = repo.find_branch(shorthand, git2::BranchType::Local) else {
        return (0, 0);
    };
    let Ok(upstream) = branch.upstream() else {
        return (0, 0);
    };
    let Some(upstream_oid) = upstream.get().target() else {
        return (0, 0);
    };

    repo.graph_ahead_behind(local_oid, upstream_oid)
        .map(|(a, b)| (a as u32, b as u32))
        .unwrap_or((0, 0))
}

fn origin_remote(repo: &Repository) -> Option<RemoteRef> {
    let remote = repo.find_remote("origin").ok()?;
    let url = remote.url()?.to_string();
    parse_remote_url(&url).or(Some(RemoteRef {
        host: String::new(),
        owner: String::new(),
        name: String::new(),
        raw_url: url,
    }))
}

/// Best-effort parse of a Git clone URL into `(host, owner, name)`. Covers:
///
///   * `https://github.com/owner/name(.git)?`
///   * `git@github.com:owner/name(.git)?`
///   * `ssh://git@gitlab.example.com:2222/group/sub/name.git`
///
/// Returns `None` if we can't extract all three pieces — callers fall back to
/// surfacing the raw URL.
fn parse_remote_url(url: &str) -> Option<RemoteRef> {
    let trimmed = url.trim().trim_end_matches('/');
    let stripped = trimmed.strip_suffix(".git").unwrap_or(trimmed);

    // SCP-style: git@host:owner/name
    if let Some(rest) = stripped.strip_prefix("git@") {
        if let Some((host, path)) = rest.split_once(':') {
            let (owner, name) = split_owner_name(path)?;
            return Some(RemoteRef {
                host: host.to_string(),
                owner,
                name,
                raw_url: url.to_string(),
            });
        }
    }

    // URL-style: https?:// or ssh://
    if let Some(no_scheme) = stripped
        .strip_prefix("https://")
        .or_else(|| stripped.strip_prefix("http://"))
        .or_else(|| stripped.strip_prefix("ssh://"))
        .or_else(|| stripped.strip_prefix("git://"))
    {
        // Strip optional user@ prefix for ssh:// URLs.
        let no_user = no_scheme
            .split_once('@')
            .map(|(_, r)| r)
            .unwrap_or(no_scheme);
        let (host_port, path) = no_user.split_once('/')?;
        let host = host_port
            .split_once(':')
            .map(|(h, _)| h)
            .unwrap_or(host_port);
        let (owner, name) = split_owner_name(path)?;
        return Some(RemoteRef {
            host: host.to_string(),
            owner,
            name,
            raw_url: url.to_string(),
        });
    }

    None
}

/// Owner/name from a path component, allowing nested groups (last two segments).
fn split_owner_name(path: &str) -> Option<(String, String)> {
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    if parts.len() < 2 {
        return None;
    }
    let name = parts.last()?.to_string();
    let owner = parts[..parts.len() - 1].join("/");
    Some((owner, name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_https_url() {
        let r = parse_remote_url("https://github.com/anthropics/claude-code.git").unwrap();
        assert_eq!(r.host, "github.com");
        assert_eq!(r.owner, "anthropics");
        assert_eq!(r.name, "claude-code");
    }

    #[test]
    fn parses_scp_url() {
        let r = parse_remote_url("git@github.com:anthropics/claude-code.git").unwrap();
        assert_eq!(r.host, "github.com");
        assert_eq!(r.owner, "anthropics");
        assert_eq!(r.name, "claude-code");
    }

    #[test]
    fn parses_ssh_url_with_port() {
        let r = parse_remote_url("ssh://git@gitlab.example.com:2222/group/sub/runner.git").unwrap();
        assert_eq!(r.host, "gitlab.example.com");
        assert_eq!(r.owner, "group/sub");
        assert_eq!(r.name, "runner");
    }

    #[test]
    fn parses_without_dot_git() {
        let r = parse_remote_url("https://codeberg.org/forgejo/runner").unwrap();
        assert_eq!(r.host, "codeberg.org");
        assert_eq!(r.owner, "forgejo");
        assert_eq!(r.name, "runner");
    }

    #[test]
    fn returns_none_for_garbage() {
        assert!(parse_remote_url("not a url").is_none());
    }
}
