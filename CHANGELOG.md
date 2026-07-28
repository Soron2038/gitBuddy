# Changelog

All notable changes to gitBuddy are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow
[SemVer](https://semver.org/). Release artifacts live on
[GitHub Releases](https://github.com/Soron2038/gitBuddy/releases).

## [Unreleased]

## [1.1.0] — 2026-07-28

A correctness and accessibility pass over the whole app, from a full code and
usability review (`docs/REVIEW-2026-07-27.md`). No new features — but several
of these were silently costing you data you thought you were seeing.

### Fixed

- **Sync failures are no longer invisible.** A revoked token, an expired PAT, a
  rate limit or a network outage used to blank every list while the footer kept
  reporting "Synced just now" — indistinguishable from "you have nothing to do".
  Every provider error now reaches the UI, the last known rows for a failing
  account stay on screen instead of disappearing, and the sync timestamp only
  advances when a poll actually reached a forge.
- **An account that fails to connect at launch now recovers by itself.** If the
  machine was offline, on a VPN that wasn't up yet, or behind a captive portal
  when gitBuddy started, that account stayed dead until the app was restarted —
  which for a menu-bar app is approximately never. It is now retried each poll.
- **Connecting an account can no longer wipe your settings.** If `settings.json`
  was unreadable, the connect flow silently overwrote it with defaults, losing
  scan roots, editor/terminal commands and every notification preference.
- **Two accounts that can see the same repo no longer break the view.** The
  duplicate row crashed the render of the Repos and Releases lists outright.
  Two self-hosted GitLab or Gitea instances could collide the same way, and
  additionally showed each other's CI status.
- **No more notification storm on a fresh install.** The "don't replay the
  backlog" baseline was being recorded before any account was connected, so the
  first poll that did reach the network announced every open item at once.
- **Rate limiting is detected properly.** GitHub answers HTTP 403 both for
  "Actions disabled here" and for exhausting your rate limit; the second was
  being read as the first, so a throttled account quietly lost its CI status and
  releases. Per-account request fan-out is now capped (it could put 120
  simultaneous requests on the wire per account), and a forge's `Retry-After` is
  respected.
- **HTTP 403 from an SSO-enforced org now explains itself** instead of showing a
  bare status code.
- **Long-open items stop being re-announced.** An item open longer than the
  60-day retention window was being treated as brand new, and then again every
  60 days after that.
- Items sort by their real timestamp across forges. GitHub, GitLab and
  Gitea/Forgejo emit three different time formats, and comparing them as plain
  strings put a Codeberg item up to an instance's UTC offset out of place.
  Releases from different accounts are now interleaved by date instead of
  grouped by account.
- Changing a setting no longer triggers a full network refetch and disk walk.
  Dragging the sync-frequency slider fired one per pixel.
- Editor, terminal and "Show in Finder" actions report failures instead of
  doing nothing visible when the configured command isn't installed.
- GitLab issues and merge requests can no longer hide each other — they have
  independent ID sequences, and one of a colliding pair was being dropped.
- Codeberg/Gitea: a draft release no longer hides the published one; repo
  pagination survives an instance that caps page size; CI and releases are
  fetched for the most recently updated repos rather than the oldest.
- GitLab: a release scheduled for the future no longer displaces the shipped
  one (badged "NEW", aged "now"); release links with `/` in the tag work.
- GitHub: "waiting on me" is sorted by recency rather than relevance, so recent
  items can't fall off the end of the list.
- Self-hosted GitLab instances are labelled consistently instead of flipping to
  the gitlab.com badge as soon as a second account was connected.
- The local scan is bounded in depth, stays on one filesystem, and recognises
  bare/mirror clones (which it previously walked into but never reported).
- The popover no longer reports "Not connected" to GitLab-only users, and its
  Refresh button works for them.
- Settings written by a newer version of gitBuddy are refused rather than
  silently stripped, and config writes are flushed to disk before being swapped
  into place.
- The main window opens on "On you" — the view the app exists for.

### Accessibility

- Text, accents and status colours now meet WCAG AA contrast. The "no CI" label
  in particular was effectively invisible.
- CI and local-clone indicators carry text labels instead of conveying their
  meaning through colour alone.
- The context menu takes keyboard focus, navigates with arrow keys, and returns
  focus where it came from. Orphan clone rows are reachable without a mouse.
- Sync and refresh state is announced to screen readers; error banners are
  announced as alerts.
- Focus is visible again on the search field and the sync-frequency slider.
- View switchers report which view is selected.

### Changed

- The per-provider `provider_status` and `provider_disconnect` commands were
  removed. Neither was reachable from the UI, and the latter would have
  disconnected every account sharing a provider — for GitLab, every self-hosted
  instance — in a single unconfirmed call. Per-account disconnect is unchanged.
- If macOS has blocked notifications, the popover now says so instead of
  leaving you to wonder why nothing arrives.

## [1.0.3] — 2026-06-12

### Fixed

- "Open in editor" now works with macOS app names containing spaces
  (e.g. `antigravity ide.app` or `Visual Studio Code`): a value ending
  in `.app` is launched whole via `open -a`, and a command whose
  program can't be found is retried as an app name instead of failing.
- Failures to launch the editor via `open` now surface an error message
  instead of failing silently.

## [1.0.2] — 2026-06-11

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

[Unreleased]: https://github.com/Soron2038/gitBuddy/compare/v1.0.2...HEAD
[1.0.2]: https://github.com/Soron2038/gitBuddy/compare/v1.0.1...v1.0.2
[1.0.1]: https://github.com/Soron2038/gitBuddy/compare/v1.0.0...v1.0.1
[1.0.0]: https://github.com/Soron2038/gitBuddy/releases/tag/v1.0.0
