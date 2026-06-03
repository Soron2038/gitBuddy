# gitBuddy

A calm macOS menu-bar companion for GitHub, GitLab, and Codeberg/Gitea/Forgejo.
gitBuddy aggregates repositories, issues, pull/merge requests, releases, and CI
status across multiple forge accounts — and surfaces the state of your local
clones (branch, dirty/untracked, ahead/behind) via libgit2.

## Features

- **One overview across forges and accounts** — multiple GitHub, GitLab, and
  Codeberg/Gitea/Forgejo accounts at once.
- **Waiting on me** — issues/PRs/MRs where you're assigned, review-requested,
  authored, or mentioned.
- **Releases & CI** — latest release per repo and the CI status of the default
  branch.
- **Local clone diagnostics** — current branch, staged/unstaged/untracked
  counts, ahead/behind upstream, all via libgit2 (no shelling out to `git`).
- **Native notifications** — for waiting items, new releases, and CI failures
  you triggered, with per-event toggles and Do-Not-Disturb.
- **Quick actions** — open in browser, clone, reveal in Finder, open in your
  editor or terminal, copy clone URLs.
- **Quality-of-life** — start at login, configurable poll interval, and
  export/import of your settings as JSON.

## Install

Download the latest `.dmg` from the [Releases](https://github.com/Soron2038/gitBuddy/releases)
page and drag gitBuddy to Applications. Signed + notarized builds open normally;
gitBuddy keeps itself up to date in place via the built-in updater (Settings →
Updates, plus a silent check on launch).

## Authentication

GitHub can be connected two ways:

- **Sign in with browser** (recommended) — OAuth Device Flow. The app shows a
  code you enter at `github.com/login/device`. No token setup needed.
- **Personal access token** — create a token with scopes `repo, read:org` and
  paste it in.

GitLab and Codeberg/Gitea/Forgejo currently use PATs only.

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
scripts/build-app.sh       # produce a release .dmg
```

The first `cargo build` is slow — `git2` compiles libgit2 and OpenSSL in-tree.

## Releasing

Cutting a signed, notarized, auto-updatable release is documented step by step
in [docs/RELEASING.md](docs/RELEASING.md).

## License

[MIT](LICENSE).
