# gitBuddy

A calm menu-bar companion for GitHub, GitLab, and Codeberg/Gitea/Forgejo, for
macOS and (as a preview) Windows.
gitBuddy aggregates repositories, issues, pull/merge requests, releases, and CI
status across multiple forge accounts — and surfaces the state of your local
clones (branch, dirty/untracked, ahead/behind) via libgit2.

## Features

- **One overview across forges and accounts** — multiple GitHub, GitLab, and
  Codeberg/Gitea/Forgejo accounts at once.
- **Waiting on me** — issues/PRs/MRs where you're assigned, review-requested,
  authored, or mentioned.
- **Releases & CI** — latest release per repo and the CI status of the default
  branch. Release files (installers, binaries) download straight into
  `~/Downloads` with your account's token, private repos included.
- **Push from anywhere** — clones made with gitBuddy push over HTTPS with the
  token of the account you're signed in with, from any terminal, without SSH
  keys (see below).
- **Local clone diagnostics** — current branch, staged/unstaged/untracked
  counts, ahead/behind upstream, all via libgit2 (no shelling out to `git`).
- **Native notifications** — for waiting items, new releases, and CI failures
  you triggered, with per-event toggles and Do-Not-Disturb.
- **Quick actions** — open in browser, clone, reveal in Finder or Explorer,
  open in your editor or terminal, copy clone URLs.
- **Quality-of-life** — start at login, configurable poll interval, light /
  dark / follow-the-system appearance, and export/import of your settings as
  JSON.

## Install

**macOS:** download the latest `.dmg` from the
[Releases](https://github.com/Soron2038/gitBuddy/releases) page and drag gitBuddy
to Applications. Signed + notarized builds open normally.

**Windows 10/11 (x64, preview):**
1. Download `gitBuddy_<version>_x64-setup.exe` from the same page and run it.
   It installs for your user only (`%LOCALAPPDATA%\gitBuddy`), without an
   administrator prompt, and fetches WebView2 if Windows lacks it.
2. Until the installer is code-signed (see
   [Code signing](#code-signing-policy)), SmartScreen warns before the first
   launch: choose *More info* → *Run anyway*.
3. gitBuddy opens its main window on that first launch, because Windows tucks
   new tray icons into the overflow (the `^` next to the clock). Drag the
   robot from there onto the taskbar to keep it in sight. Its tooltip says
   how many clones are behind.

On both platforms gitBuddy keeps itself up to date in place via the built-in
updater (Settings → Updates, plus a silent check on launch).

### Uninstall

Remove the app the usual way: drag it to the Trash on macOS, or use
*Settings → Apps → Installed apps → gitBuddy → Uninstall* on Windows. The
Windows uninstaller offers to delete the app data too. Three things stay
behind on purpose:
- **Tokens:** in the Keychain on macOS. On Windows they are in Credential
  Manager → Windows Credentials, generic entries ending in
  `.dev.soron2038.gitbuddy`.
- **Settings:** `~/Library/Application Support/dev.soron2038.gitbuddy` on
  macOS, `%APPDATA%\dev.soron2038.gitbuddy` on Windows.
- **Push helpers:** the `credential."https://<forge>".helper` entries in the
  `.git/config` of clones gitBuddy set up for pushing (see below). They point
  at the removed app, so delete them if you keep pushing from those clones.

## Authentication

GitHub can be connected two ways:

- **Sign in with browser** (recommended) — OAuth Device Flow. The app shows a
  code you enter at `github.com/login/device`. No token setup needed.
- **Personal access token** — create a token with scopes `repo, read:org` and
  paste it in.

GitLab and Codeberg/Gitea/Forgejo currently use PATs only. A Gitea/Forgejo
token needs `read:user` and `read:repository`, plus `write:repository` if you
want to push through gitBuddy.

### Pushing from gitBuddy clones

Every clone gitBuddy makes registers gitBuddy as its git credential helper, so
`git push` over HTTPS gets the account's token from gitBuddy — in any terminal,
whether or not the app is running. For a clone made some other way, use
**Enable push via gitBuddy** in the repo's detail pane. Only the clone's own
`.git/config` is changed (a `credential."https://<forge>".helper` entry);
remove that section to undo it. Clones that use SSH remotes are left alone.
On Windows that entry also keeps Git Credential Manager out of the way for
the clone, so pushing never brings up a separate sign-in.

Background on OAuth app registration and the Keychain layout: see
[docs/DECISIONS.md](docs/DECISIONS.md).

## Building from source

gitBuddy is a Tauri 2 shell with a SvelteKit 2 / Svelte 5 frontend and a Rust
core. Common commands (full list in [CLAUDE.md](CLAUDE.md)):

```bash
npm install
npm run tauri dev          # run locally
npm run check              # frontend type-check
cd src-tauri && cargo test --lib   # Rust tests
scripts/build-app.sh --unsigned    # produce a local .dmg (no updater key needed)
```

The first `cargo build` is slow — `git2` compiles libgit2 and OpenSSL in-tree.

### Windows (local, unsigned)

On a Windows x64 machine, this builds an unsigned NSIS installer without the
updater key (`src-tauri/tauri.windows.conf.json` holds the Windows bundle
settings). Run it from Git Bash: Windows PowerShell 5.1 strips the inner double
quotes from the JSON. `--config` also takes the path to a JSON file holding the
same object.

```bash
npm install
npm run tauri build -- --bundles nsis --config '{"bundle":{"createUpdaterArtifacts":false}}'
```

Prerequisites: the MSVC Build Tools with the "Desktop development with C++"
workload, WebView2 (preinstalled on Windows 11), Rust stable with the MSVC
toolchain, and Node ≥ 20. Perl and OpenSSL are not needed: on Windows libgit2
uses WinHTTP for HTTPS, so nothing compiles OpenSSL there.

## Releasing

Cutting a signed, notarized, auto-updatable release is documented step by step
in [docs/RELEASING.md](docs/RELEASING.md). The Mac build happens locally. The
Windows installer is built by GitHub Actions on the tag push. What changed per
version lives in the [CHANGELOG](CHANGELOG.md) and on the
[Releases](https://github.com/Soron2038/gitBuddy/releases) page.

## Code signing policy

- **macOS:** builds are signed with an Apple Developer ID and notarized.
- **Windows:** installers are to be signed through SignPath Foundation; the
  application is pending. Until then they are published unsigned, and the
  release notes say so.
- **How Windows builds are made:** only by the
  `.github/workflows/release-windows.yml` workflow in this repository, from a
  tagged commit, without build caches. Every signing request needs a manual
  approval.
- **Updates:** they are additionally signed with the updater's minisign key,
  which never leaves the maintainer's machine. The app refuses any update
  whose signature doesn't verify.
- **Team:** committer, reviewer and approver is
  [Soron2038](https://github.com/Soron2038).

## License

[MIT](LICENSE).
