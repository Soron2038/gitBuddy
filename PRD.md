# gitBuddy — Product Requirements Document (PRD)

## 1. Context

Developers who use multiple Git forges (GitHub, GitLab, Codeberg/Gitea/Forgejo,
self-hosted instances) and maintain many repositories — some cloned locally,
some not — lack a unified, low-friction way to answer everyday questions:

- Which repos do I have, and where (which host, which org)?
- Is this repo already cloned locally? If yes, where on disk?
- What is waiting for me: issues, pull requests, merge requests?
- What version is the project at? Is there a recent release?
- Does my local checkout have uncommitted or unpushed work?
- Is CI green on the latest commit / PR?

gitBuddy is a lightweight macOS menu-bar companion that aggregates these
signals across all connected forges into a single overview, with a small
tray popover for at-a-glance status and a dedicated window for detail, search,
and configuration. It is designed from day one to be portable to Windows and
Linux without rewriting the UI.

---

## 2. Goals & Non-Goals

### Goals
- Single pane of glass across multiple Git forges and multiple accounts.
- Always-on, ambient awareness via the macOS menu bar.
- Fast and resource-light (small RAM/CPU footprint suitable for an always-running app).
- Treat local clones as first-class: detect them, surface their state.
- Architecture portable to Windows and Linux later without major rewrites.

### Non-Goals (for v1)
- Full Git client (no commit/push/merge UI). Hand off to terminal/IDE.
- Code review UI (no inline diff comments). Hand off to the forge's web UI.
- Project/Kanban management.
- Real-time updates via webhooks (deferred — polling is sufficient for v1).
- Mac App Store distribution (sandboxing limits filesystem scans).
- Mobile platforms.

---

## 3. Target User & Persona

Primary persona: A polyglot developer/engineer who:
- Maintains 10–200 repositories across at least two forges.
- Often works with separate personal and work accounts.
- Uses macOS as primary OS, may later run Linux/Windows.
- Cares about quick context switches: "which PR needs me right now?"

---

## 4. Functional Requirements (MVP)

### 4.1 Account Management
- Support GitHub.com, GitLab.com, Codeberg.org, and arbitrary self-hosted
  GitLab/Gitea/Forgejo instances (custom base URL).
- **Multiple accounts per provider type** (e.g. personal + work GitHub).
- Authentication:
  - **OAuth flow** (browser-based) as primary method for public hosts where
    a registered OAuth app is provided.
  - **Personal Access Token (PAT)** as fallback, especially for self-hosted
    instances. Required scopes documented per provider.
- Credentials stored in the **macOS Keychain** (and equivalent secret stores
  on other OSes later).
- Per-account display: avatar, username, host, account label (user-editable).

### 4.2 Repository Aggregation & Local Detection
- Fetch all repos the user has access to per connected account (own, member, starred — configurable).
- **Local clone detection** via configurable scan roots (default suggestion:
  `~/Developer`, `~/Code`, `~/src` — user-editable list).
  - Recursive scan for `.git` directories under each root.
  - Resolve each local repo to its remote URL(s) and match to fetched remote
    repos (by clone URL or `host/owner/name` triplet).
  - Multiple local clones of the same remote repo are all surfaced.
  - Repos not matching any connected account are shown as "orphans"
    (still listed, no remote enrichment).
  - Excludable paths (e.g. `node_modules`, `.Trash`, vendored deps) via
    sensible defaults + user-editable ignore list.
- Per-repo display: name, owner/org, host icon, default branch, description,
  primary language, stars (optional), clone status with local path(s).

### 4.3 Issues / Pull Requests / Merge Requests
- Surface items where the user is one of:
  - **assigned**
  - **review requested**
  - **authored**
  - **@mentioned**
- All four filters enabled by default; user can toggle in settings.
- Grouped by status (open / draft / waiting on me) and by repo or by reason.
- Click-through opens the item in the system browser.
- Counter badges on the tray icon and per-account/per-repo rows.

### 4.4 Versions & Releases
- "Version" = latest annotated semver-style **Git tag** for a repo, fetched
  via the provider API.
- "Release" = latest **published release** via the provider API
  (GitHub Releases / GitLab Releases / Gitea Releases).
- Show:
  - Latest release name, tag, published date.
  - Whether the latest tag has a corresponding release.
  - Optional badge if a new release was published since the last app launch.
- Manifest-file parsing (`package.json`, `Cargo.toml`, etc.) is **out of
  scope for v1**.

### 4.5 CI/CD Status
- Latest CI run status for the default branch and for open PRs/MRs of interest.
- Providers:
  - GitHub Actions (Checks API / Workflow runs).
  - GitLab Pipelines.
  - Gitea/Forgejo: best-effort via their actions API (graceful no-op if
    pipelines aren't enabled).
- Status display: success / failure / running / cancelled / pending, with
  link to the run.

### 4.6 Local Repository Diagnostics
For every locally cloned repo:
- Current branch.
- **Uncommitted changes** (staged / unstaged counts).
- **Untracked files** count.
- **Ahead / behind** vs. tracked upstream branch.
- **Unpushed commits** count.
- Last fetch time (warn if stale).
- Implementation via the `git2` Rust crate (libgit2 bindings) — no shelling
  out to `git` required.

### 4.7 Quick Actions
Per repo (context menu / row buttons):
- Open in browser (forge URL).
- Clone (with destination picker; defaults to first configured scan root).
- Open in Finder (local repos only).
- Open in Terminal (configurable: Terminal.app / iTerm2 / Warp / ...).
- Open in editor/IDE via **configurable command** (user enters a CLI like
  `code`, `cursor`, `zed`, `idea`, or a custom URL scheme). Single default
  command; per-repo override deferred.
- Copy clone URL (HTTPS / SSH).

### 4.8 Notifications
- Native macOS notifications via the OS notification center for:
  - New issue assigned / mentioned.
  - New review requested.
  - New release published on a watched repo.
  - CI failure on a repo where the user authored the latest commit/PR.
- Per-event-type toggles in settings.
- "Do Not Disturb" toggle (snooze all notifications).
- Notifications deduplicated by (provider, repo, item id) so a polling cycle
  doesn't replay them.

### 4.9 Settings & Preferences
- Account management (add / remove / re-auth).
- Scan roots (add / remove paths, ignore patterns).
- Polling interval (default **5 min**, range 1–60 min).
- Issue/PR filter toggles.
- Notification preferences.
- Default editor / terminal commands.
- Theme: follow system (light/dark).
- Start at login toggle.
- Export / import config (JSON) for portability across machines.

---

## 5. Non-Functional Requirements

### 5.1 Performance
- Idle RAM target: **< 150 MB**.
- Tray popover render < 200 ms after click.
- Initial sync of 100 repos across 3 accounts: < 30 s.
- Local scan of 10k subdirectories: < 5 s (incremental, with cache).

### 5.2 Security & Privacy
- All tokens in OS keystore (Keychain on macOS).
- No telemetry, no analytics, no third-party crash reporters in v1.
- All network traffic goes only to user-configured forge endpoints.
- Optional opt-in anonymous error reporting in a later version.

### 5.3 Portability
- All forge integration, git logic, and storage in a **platform-agnostic Rust
  core**. Only tray icons, notifications, and "open in X" handlers have
  per-OS adapters.
- Tauri provides cross-platform tray, notifications, and webview out of the box.

### 5.4 Accessibility
- Keyboard navigation throughout the main window.
- Honors system font-size and reduced-motion preferences.

### 5.5 Internationalization
- v1: English UI only.
- All user-facing strings centralized to enable later i18n (German first).

---

## 6. Architecture

### 6.1 Tech Stack
- **Backend / core**: Rust.
- **UI shell**: Tauri 2.
- **Frontend**: SvelteKit (SPA mode) + TypeScript.
- **Local store**: SQLite via `sqlx` (cache of remote data + repo index).
- **Secrets**: `keyring` crate → macOS Keychain.
- **HTTP**: `reqwest` with rustls.
- **Git**: `git2` (libgit2 bindings).
- **Async runtime**: `tokio`.

### 6.2 High-Level Modules (Rust core)
- `providers/` — one module per forge type (`github`, `gitlab`, `gitea`).
  Each implements a common `Provider` trait: `list_repos`, `list_items`,
  `list_releases`, `list_ci_runs`, `authenticate`, etc.
- `accounts/` — account registry + credential storage.
- `local_index/` — filesystem scanner, libgit2 wrapper, local repo diagnostics.
- `aggregator/` — fan-out across providers, merging of remote + local data.
- `scheduler/` — polling loop with backoff and rate-limit awareness.
- `notifier/` — native notification dispatch via Tauri.
- `store/` — SQLite cache layer.
- `ipc/` — Tauri command handlers exposed to the SvelteKit frontend.

### 6.3 Data Model (conceptual)
- `Account(id, provider_type, host, username, label, token_ref)`
- `Repo(id, account_id, host, owner, name, default_branch, description,
  language, stars, html_url, last_synced)`
- `LocalRepo(id, path, remote_match_id, branch, dirty, ahead, behind, last_fetched)`
- `Item(id, repo_id, kind[Issue|PR|MR], state, title, url, author, assignees,
  reviewers, labels, updated_at, reason[assigned|review|author|mention])`
- `Release(repo_id, tag, name, published_at, html_url)`
- `CiRun(repo_id, ref, status, conclusion, html_url, started_at)`

### 6.4 Sync Strategy
- Polling, configurable interval (default 5 min, min 1 min).
- Provider-specific rate-limit honoring (HTTP 429 / `X-RateLimit-*` headers).
- ETag / `If-Modified-Since` where supported to minimize quota usage.
- Incremental: only refresh items updated since last sync where the API allows.

### 6.5 Distribution & Updates
- **Signed & notarized `.dmg`** published on GitHub Releases.
- **Tauri Updater** for in-app auto-updates, signature-verified.
- Requires Apple Developer ID (~99 USD/year).
- Homebrew Cask and other channels: post-v1.

---

## 7. User Experience

### 7.1 Menu Bar Popover (always-on)
- Tray icon shows a small count badge with the total number of items waiting.
- Popover (~360×500 px) shows:
  - Tabs / segments: **Waiting on me**, **Repos**, **Releases**.
  - Each row: provider icon, repo, title, age, status.
  - "Open main window" button.
  - Refresh button + last-sync timestamp.
  - Settings icon.

### 7.2 Main Window
- Sidebar: filters by account, provider, repo, item type, state.
- Main pane: virtualized list (handles 1000+ items).
- Top bar: global search across repos / items.
- Per-repo detail view: counters for items, latest release, CI status,
  local clone path(s), quick actions.
- Settings as a separate screen, not a modal.

---

## 8. Provider Integration Details

| Capability                  | GitHub                         | GitLab (incl. self-hosted) | Gitea / Codeberg / Forgejo |
|----------------------------|--------------------------------|----------------------------|----------------------------|
| Auth                        | OAuth + PAT                    | OAuth + PAT                | PAT (OAuth opt-in)         |
| List user repos             | REST + GraphQL (preferred)     | REST v4                    | REST v1                    |
| Issues / PRs filter by user | GraphQL `viewer` / search      | REST with `scope=assigned…`| REST issues + PRs          |
| Releases                    | `/repos/.../releases`          | `/projects/.../releases`   | `/repos/.../releases`      |
| CI runs                     | Actions / Checks API           | Pipelines API              | Gitea Actions (best-effort)|
| Rate-limit headers          | `X-RateLimit-*`                | `RateLimit-*`              | varies                     |

GraphQL preferred for GitHub to minimize request count for the
"waiting on me" view.

---

## 9. Out of Scope (v1) / Future Ideas

- Webhook listener for true real-time updates (requires tunneling or local
  listener).
- Bitbucket / Azure DevOps / SourceHut providers.
- Inline code review / diff browsing.
- Manifest-file version parsing.
- Per-repo notification subscriptions ("watch list").
- Stats dashboard (PR throughput, review latency).
- Multi-language UI (German, etc.).
- Windows and Linux release builds (architecture is ready; needs CI + signing).
- Mac App Store distribution.

---

## 10. Milestones

1. **M1 — Skeleton** (week 1–2)
   - Tauri + SvelteKit scaffold, tray icon, empty popover & main window,
     CI build pipeline.
2. **M2 — Provider abstraction + GitHub PAT** (week 2–4)
   - `Provider` trait, GitHub implementation (PAT only), Keychain storage,
     basic repo list in UI.
3. **M3 — Local index** (week 4–5)
   - Scan-root config, libgit2-based local detection, repo↔clone matching.
4. **M4 — Items + Releases + CI for GitHub** (week 5–7)
   - "Waiting on me" view, releases, CI status, polling scheduler.
5. **M5 — GitLab + Gitea providers** (week 7–9)
   - Generalize to GitLab.com + self-hosted, Codeberg/Gitea/Forgejo.
6. **M6 — Notifications + Quick actions + OAuth** (week 9–11)
   - Native notifications, editor/terminal launchers, OAuth where applicable.
7. **M7 — Polishing, signing, updater, v1.0 release** (week 11–13)
   - Notarization, Tauri Updater, documentation, MIT release on GitHub.

---

## 11. Risks & Open Questions

- **OAuth app registration** for public hosts: requires registering an
  OAuth app per provider (GitHub, GitLab.com). Public client secret rotation
  strategy needed. PAT fallback mitigates this for early releases.
- **Rate limits**: GitHub REST is 5000/h authenticated; GraphQL has point
  budgets. Adaptive polling and ETag usage are essential.
- **Local scan performance** on machines with very deep directory trees —
  mitigated by ignore-list defaults and incremental scans.
- **Tauri 2 maturity**: stable but moving fast; pin versions and follow
  migration notes.
- **Notarization workflow**: Apple Developer Program subscription required;
  CI signing keys must be managed securely.

---

## 12. Verification Plan

How to confirm the MVP works end-to-end before declaring v1.0:

- Manual smoke test against two real GitHub accounts (PAT auth), one real
  GitLab.com account, and one Codeberg account, with at least 30 repos
  across them.
- Local scan against the developer's actual `~/Developer` tree;
  verify clone detection, dirty status, ahead/behind on at least 10 repos
  in various states.
- Trigger known issue/PR assignments and confirm they appear within one
  polling cycle and emit a notification.
- Publish a draft release on a test repo; verify it appears and notifies.
- Force CI failures (e.g. push a broken commit to a test repo) and verify
  the failure surfaces.
- Idle RAM measured via Activity Monitor < 150 MB after 1 h.
- Tauri Updater test: ship a v1.0.1 with one cosmetic change; confirm
  in-app update succeeds on a real machine.

Unit tests:
- Provider trait conformance tests with recorded HTTP fixtures.
- libgit2 wrapper tests against fixture repositories.
- Aggregator merge logic tests.

---

## 13. Next Steps After Approval

1. Initialize Tauri + SvelteKit scaffold (`pnpm create tauri-app`).
2. Set up CI (GitHub Actions: lint, test, build for macOS).
3. Begin Milestone 1.
