# Changelog

All notable changes to gitBuddy are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow
[SemVer](https://semver.org/). Release artifacts live on
[GitHub Releases](https://github.com/Soron2038/gitBuddy/releases).

## [Unreleased]

### Fixed

- All forge HTTP requests now carry connect/request timeouts — a stalled
  host can no longer hang the background refresh indefinitely.
- Codeberg/Gitea repos sort by the last actual push instead of the last
  metadata edit.
- Concurrent settings saves can no longer race each other on a shared
  temp file.
- The notification bell in the main window now opens the "On you" view
  (it previously did nothing).
- Editor/terminal command fields in Settings no longer reset while typing
  when a settings change lands from another window.
- The popover follows the multi-account registry for its auth state and
  cleans up its event listeners and timers reliably.
- A GitHub device-flow in progress stops polling when the main window
  closes.

### Changed

- Background refresh fetches all accounts in parallel and reuses one repo
  list per tick — substantially less API quota per refresh, faster ticks,
  and HTTP 429 rate limiting is now reported in the footer status.
- "Open in editor" launches the configured command directly instead of
  through a shell; flags still work, shell metacharacters are no longer
  interpreted.

### Security

- The webview now runs under a production Content-Security-Policy.
- Imported configuration files can no longer inject `editor_command` /
  `terminal_command` (the local values stay authoritative).
- Authenticated clones refuse to send the account token to a host other
  than the account's own forge.

## [1.0.1] — 2026-06-05

### Added

- Settings → Updates shows the running app version ("You're running
  gitBuddy 1.0.1"), doubling as the visible proof of the 1.0.0 → 1.0.1
  updater roundtrip.

## [1.0.0] — 2026-06-05

First signed and notarized release.

### Added

- Menu-bar popover and main window aggregating repos, issues/PRs,
  releases and CI status across GitHub, GitLab and Codeberg/Gitea/Forgejo
  accounts (multi-account).
- Local clone index via libgit2: branch, dirty state, ahead/behind,
  orphan detection.
- GitHub OAuth Device Flow + PAT auth; GitLab/Codeberg PAT auth; tokens
  stored in the macOS Keychain.
- Native notifications for new waiting items, releases, and own CI
  failures, with per-event toggles and Do Not Disturb.
- "Open in editor" / "Open in terminal" quick actions, start-at-login,
  config export/import.
- In-app auto-update via signed `latest.json` on GitHub Releases.

[Unreleased]: https://github.com/Soron2038/gitBuddy/compare/v1.0.1...HEAD
[1.0.1]: https://github.com/Soron2038/gitBuddy/compare/v1.0.0...v1.0.1
[1.0.0]: https://github.com/Soron2038/gitBuddy/releases/tag/v1.0.0
