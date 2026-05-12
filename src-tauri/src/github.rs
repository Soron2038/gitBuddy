//! GitHub provider — PAT-based authentication, viewer info, and the
//! "waiting on me" search across assigned/review-requested/authored/mentioned.
//!
//! M2 deliberately stays close to GitHub's REST search API. GraphQL would
//! consolidate the four queries into one, but the search API works fine for
//! single-digit account counts and avoids hand-rolling a GraphQL client for
//! milestone one of the providers.

use crate::types::{ItemKind, ItemReason, Provider, Viewer, WaitingItem};
use chrono::{DateTime, Utc};
use reqwest::{Client, StatusCode};
use serde::Deserialize;
use thiserror::Error;

const API_BASE: &str = "https://api.github.com";
const USER_AGENT: &str = concat!("gitBuddy/", env!("CARGO_PKG_VERSION"));
const ACCEPT: &str = "application/vnd.github+json";

#[derive(Debug, Error)]
pub enum GitHubError {
    #[error("network error: {0}")]
    Network(#[from] reqwest::Error),
    #[error("authentication failed — check that the token is valid and has `repo` scope")]
    Unauthorized,
    #[error("GitHub API returned HTTP {0}")]
    HttpStatus(StatusCode),
}

pub type Result<T> = std::result::Result<T, GitHubError>;

pub struct GitHubProvider {
    client: Client,
    token: String,
    pub viewer: Viewer,
}

impl GitHubProvider {
    /// Construct a provider from a personal access token, verifying that the
    /// token works by hitting `/user`. Returns the authenticated viewer so
    /// callers can show a "connected as @login" confirmation.
    pub async fn connect(token: String) -> Result<Self> {
        let client = Client::builder().user_agent(USER_AGENT).build()?;
        let viewer = fetch_viewer(&client, &token).await?;
        Ok(Self {
            client,
            token,
            viewer,
        })
    }

    /// Items where the user is assigned, review-requested, authored, or
    /// mentioned. Queries run concurrently; results are deduplicated, since
    /// the same PR can match multiple reasons.
    pub async fn list_waiting(&self) -> Result<Vec<WaitingItem>> {
        let login = self.viewer.login.as_str();
        let queries = [
            (
                ItemReason::Assigned,
                format!("is:open assignee:{login} archived:false"),
            ),
            (
                ItemReason::Review,
                format!("is:open review-requested:{login} archived:false"),
            ),
            (
                ItemReason::Authored,
                format!("is:open author:{login} archived:false"),
            ),
            (
                ItemReason::Mentioned,
                format!("is:open mentions:{login} archived:false"),
            ),
        ];

        let mut handles = Vec::with_capacity(queries.len());
        for (reason, q) in queries {
            let client = self.client.clone();
            let token = self.token.clone();
            handles.push(tokio::spawn(async move {
                search_issues(&client, &token, &q, reason).await
            }));
        }

        let mut items = Vec::new();
        for h in handles {
            match h.await {
                Ok(Ok(mut v)) => items.append(&mut v),
                Ok(Err(e)) => return Err(e),
                // A panic inside a spawned task — surface as a generic error
                // so the UI doesn't silently lose results without explanation.
                Err(_) => return Err(GitHubError::HttpStatus(StatusCode::INTERNAL_SERVER_ERROR)),
            }
        }

        // Dedup by (repo, id) keeping the highest-priority reason.
        items.sort_by(|a, b| {
            a.repo
                .cmp(&b.repo)
                .then(a.id.cmp(&b.id))
                .then(reason_priority(a.reason).cmp(&reason_priority(b.reason)))
        });
        items.dedup_by(|a, b| a.repo == b.repo && a.id == b.id);

        // After dedup, sort by recency (most recently updated first).
        items.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));

        Ok(items)
    }
}

async fn fetch_viewer(client: &Client, token: &str) -> Result<Viewer> {
    let resp = client
        .get(format!("{API_BASE}/user"))
        .bearer_auth(token)
        .header("Accept", ACCEPT)
        .send()
        .await?;

    match resp.status() {
        s if s.is_success() => {
            #[derive(Deserialize)]
            struct Raw {
                login: String,
                avatar_url: Option<String>,
                name: Option<String>,
            }
            let r: Raw = resp.json().await?;
            Ok(Viewer {
                login: r.login,
                avatar_url: r.avatar_url,
                name: r.name,
            })
        }
        StatusCode::UNAUTHORIZED => Err(GitHubError::Unauthorized),
        s => Err(GitHubError::HttpStatus(s)),
    }
}

async fn search_issues(
    client: &Client,
    token: &str,
    q: &str,
    reason: ItemReason,
) -> Result<Vec<WaitingItem>> {
    let resp = client
        .get(format!("{API_BASE}/search/issues"))
        .bearer_auth(token)
        .header("Accept", ACCEPT)
        .query(&[("q", q), ("per_page", "50")])
        .send()
        .await?;

    match resp.status() {
        s if s.is_success() => {}
        StatusCode::UNAUTHORIZED => return Err(GitHubError::Unauthorized),
        s => return Err(GitHubError::HttpStatus(s)),
    }

    #[derive(Deserialize)]
    struct SearchResp {
        items: Vec<RawItem>,
    }
    #[derive(Deserialize)]
    struct RawItem {
        id: u64,
        title: String,
        html_url: String,
        updated_at: String,
        repository_url: String,
        pull_request: Option<serde_json::Value>,
    }

    let body: SearchResp = resp.json().await?;
    let now = Utc::now();

    Ok(body
        .items
        .into_iter()
        .map(|it| WaitingItem {
            id: it.id.to_string(),
            kind: if it.pull_request.is_some() {
                ItemKind::Pr
            } else {
                ItemKind::Is
            },
            title: it.title,
            repo: parse_repo(&it.repository_url),
            provider: Provider::Github,
            reason,
            url: it.html_url,
            age_human: humanise_age(&it.updated_at, now),
            updated_at: it.updated_at,
        })
        .collect())
}

fn parse_repo(api_url: &str) -> String {
    // Inputs look like "https://api.github.com/repos/owner/name"; we want the
    // trailing "owner/name". A naive rsplit on "/repos/" is enough — the
    // value is always provider-controlled.
    api_url
        .rsplit_once("/repos/")
        .map(|(_, tail)| tail.to_string())
        .unwrap_or_else(|| api_url.to_string())
}

fn humanise_age(ts: &str, now: DateTime<Utc>) -> String {
    let Ok(t) = DateTime::parse_from_rfc3339(ts) else {
        return "?".into();
    };
    let delta = now - t.with_timezone(&Utc);
    let mins = delta.num_minutes();
    if mins < 1 {
        "now".into()
    } else if mins < 60 {
        format!("{mins}m")
    } else if mins < 60 * 24 {
        format!("{}h", mins / 60)
    } else if mins < 60 * 24 * 30 {
        format!("{}d", mins / (60 * 24))
    } else if mins < 60 * 24 * 365 {
        format!("{}mo", mins / (60 * 24 * 30))
    } else {
        format!("{}y", mins / (60 * 24 * 365))
    }
}

fn reason_priority(r: ItemReason) -> u8 {
    match r {
        ItemReason::Assigned => 0,
        ItemReason::Review => 1,
        ItemReason::Authored => 2,
        ItemReason::Mentioned => 3,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_repo_from_api_url() {
        assert_eq!(
            parse_repo("https://api.github.com/repos/anthropics/claude-code"),
            "anthropics/claude-code"
        );
    }

    #[test]
    fn humanises_age_buckets() {
        let now = DateTime::parse_from_rfc3339("2026-05-12T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(humanise_age("2026-05-12T11:30:00Z", now), "30m");
        assert_eq!(humanise_age("2026-05-12T08:00:00Z", now), "4h");
        assert_eq!(humanise_age("2026-05-09T12:00:00Z", now), "3d");
    }
}
