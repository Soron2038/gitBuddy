# Releasing gitBuddy

How to cut a signed, notarized, auto-updatable release for macOS and Windows.
The app code (updater plugin, signing-aware `build-app.sh`) is already wired;
this is the operational checklist for the secrets-and-publish steps that can't
live in the repo.

The Mac build happens locally. The Windows installer is built and
Authenticode-signed by `.github/workflows/release-windows.yml` when the tag is
pushed. `scripts/release.sh` coordinates both and publishes them as one
release (see the 2026-11-22 entry in `docs/DECISIONS.md`).

The updater endpoint is configured in `src-tauri/tauri.conf.json` as:

```text
https://github.com/Soron2038/gitBuddy/releases/latest/download/latest.json
```

i.e. every release must attach a `latest.json` asset; the running app fetches
the *latest* release's copy to decide whether to update.

---

## One-time setup

### 1. Tauri updater signing key

The updater verifies each download against a minisign public key baked into the
app. Generate the keypair **once** and keep the private key secret (never
commit it):

```bash
npm run tauri signer generate -- -w ~/.tauri/gitbuddy.key
```

This prints (and writes) a private key and a **public key**. Paste the public
key into `src-tauri/tauri.conf.json` → `plugins.updater.pubkey`, replacing the
`REPLACE_WITH_TAURI_SIGNER_GENERATE_PUBLIC_KEY` placeholder. Commit that change
(the public key is not a secret).

At build time the private key must be available to `tauri build`:

```bash
export TAURI_SIGNING_PRIVATE_KEY="$(cat ~/.tauri/gitbuddy.key)"
# only if you set a password when generating the key:
export TAURI_SIGNING_PRIVATE_KEY_PASSWORD="…"
```

Without `TAURI_SIGNING_PRIVATE_KEY`, the build fails at the bundle step because
`bundle.createUpdaterArtifacts` is `true`.

### 2. Apple Developer ID (signing + notarization)

Install the **Developer ID Application** certificate into your login keychain
(Xcode → Settings → Accounts → Manage Certificates, or download from the Apple
Developer portal). Then export, for `tauri build` to pick up automatically:

```bash
export APPLE_SIGNING_IDENTITY="Developer ID Application: Your Name (TEAMID)"
export APPLE_ID="your-apple-id@example.com"
export APPLE_PASSWORD="app-specific-password"   # appleid.apple.com → App-Specific Passwords
export APPLE_TEAM_ID="TEAMID"
```

(Alternatively use an App Store Connect API key via `APPLE_API_ISSUER` +
`APPLE_API_KEY` + `APPLE_API_KEY_PATH` instead of `APPLE_ID`/`APPLE_PASSWORD`.)

With these set, `tauri build` signs, notarizes, and staples the bundle.

### 3. Windows code signing (SignPath Foundation)

Until this is done, the workflow builds the Windows installer **unsigned**, and
`release.sh` publishes it with a "Windows preview" note at the top of the
release notes. The repo variable `WINDOWS_SIGNING_POLICY` is unset, which
means `none`.

1. Apply at signpath.org/apply, with 2FA on GitHub and on SignPath. SignPath
   expects the project to be released already in the form that gets signed,
   which is what the unsigned previews are for.
2. Once approved, set up in SignPath:
   - a project `gitBuddy` with origin verification for this repository;
   - signing policies `test-signing` and `release-signing`;
   - artifact configurations `tauri-app-exe` and `tauri-installer`, each a
     `zip-file` (the GitHub artifact) wrapping one `pe-file`.
3. On GitHub (Settings → Environments and → Secrets and variables → Actions):
   - Create an environment `release` whose deployment rule allows only tags
     matching `v*`, with the secret `SIGNPATH_API_TOKEN`.
   - Add the repository variable `SIGNPATH_ORGANIZATION_ID`.
4. Try the pipeline on the newest tag:

   ```bash
   gh workflow run release-windows.yml --ref v<version> -f signing-policy=test-signing
   ```

   `scripts/check-authenticode.ps1` accepts any signature with a timestamp
   for `test-signing`. A test-signed installer is never published:
   `release.sh` refuses it.
5. Set the repository variable `WINDOWS_SIGNING_POLICY` to `release-signing`.
   From then on every tag push signs, and each signing request waits for an
   approval in SignPath.

The NSIS uninstaller stays unsigned: SignPath can only sign the artifacts the
workflow uploads, not files created in the middle of bundling.

---

## Per-release steps

**The short version:** bump + commit, then `scripts/release.sh`. That script
runs everything from step 3 down and asks for the two secrets interactively, so
they never land in a file:
- tag, then build and signature verification on the Mac;
- the Windows installer from GitHub Actions;
- `latest.json`, then publish.

The manual steps below document what it does, and are still what you follow if
something breaks midway.

```bash
# 1. bump the version in the three files + lockfiles, commit
# 2. verify the gate
# 3. everything else:
scripts/release.sh              # --dry-run to stop before publishing
```

It refuses to run on a dirty tree, off `main`, when the tag already points
somewhere else, or when the release already exists. It checks the Apple
credentials against Apple *before* starting a multi-minute build.

**The order of a run:**
1. Preflight and the credential checks.
2. It asks **"Push the tag now?"**. The tag push starts the Windows build in
   GitHub Actions, which runs while the Mac builds and notarises.
3. The Mac build and its verification.
4. It waits for the Windows run (`gh run watch`). If the run is signing, each
   signing request waits for your approval at app.signpath.io.
5. It downloads the installer, checks `SHA256SUMS`, and signs the installer
   for the updater with the local key.
6. `latest.json`, with all three platforms.
7. **"Publish?"**: a draft release with every asset, then a check that the
   draft's asset list is complete, then publishing it as the latest release.

Options for when something went wrong:
- `--skip-build` reuses what's in `release/`.
- `--windows-run <id>` takes the installer from a specific finished run.
- `--no-windows` publishes without Windows. See "Troubleshooting" before you
  use it.

### 1. Bump the version

Keep these three in sync — the updater compares the version string:

- `src-tauri/tauri.conf.json` → `version`
- `src-tauri/Cargo.toml` → `version`
- `package.json` → `version` (cosmetic, but keep it aligned)

Then refresh the lockfiles so they record the new version (v1.0.1's
`package-lock.json` shipped still claiming `0.1.0`):

```bash
npm install --package-lock-only
cd src-tauri && cargo check && cd ..   # updates Cargo.lock
```

### 2. Verify the gate

```bash
cd src-tauri && cargo test --lib && cargo clippy --all-targets -- -D warnings
cd .. && npm run check
```

### 3. Build

With all the env vars from one-time setup exported:

```bash
scripts/build-app.sh --clean
# or a universal binary:
scripts/build-app.sh --clean --target universal-apple-darwin
```

This produces, under `release/`:

- `gitBuddy_<version>_<arch>.dmg` — the installer
- `gitBuddy_<version>_<arch>.app.tar.gz` — the updater bundle
- `gitBuddy_<version>_<arch>.app.tar.gz.sig` — its minisign signature

### 4. Verify signing

```bash
codesign --verify --deep --strict --verbose=2 \
  "src-tauri/target/release/bundle/macos/gitBuddy.app"
spctl -a -vv "src-tauri/target/release/bundle/macos/gitBuddy.app"   # expect: accepted, source=Notarized Developer ID
```

### 5. The Windows installer

The tag push (`git tag -a v<version> -m "gitBuddy <version>" && git push origin
v<version>`) starts `.github/workflows/release-windows.yml`. When the run is
done:

```bash
gh run download <run-id> --name windows-installer --dir /tmp/win
(cd /tmp/win && shasum -a 256 -c SHA256SUMS)
cp /tmp/win/gitBuddy_<version>_x64-setup.exe release/
npm run tauri -- signer sign --app-version <version> release/gitBuddy_<version>_x64-setup.exe
```

`signer sign` reads the key and passphrase from the variables of step 1. Pass
`--password ""` if the passphrase is empty, or it stops to ask. The artifact's
`SIGNING_POLICY` file says whether the installer is Authenticode-signed
(`release-signing`) or an unsigned preview (`none`).

### 6. Generate `latest.json`

```bash
scripts/generate-latest-json.sh --notes "What changed in this release."
```

The script reads the version from `tauri.conf.json` and writes `latest.json`
to the repo root:
- every `release/*.app.tar.gz` + `.sig` pair from the Mac build, with the
  platform keys derived from the filename (`_aarch64` / `_x64`; a
  `_universal` artifact serves both);
- `release/*_x64-setup.exe` + `.sig` as `windows-x86_64`.

It refuses to run when one of darwin-aarch64, darwin-x86_64 and
windows-x86_64 is missing, or when an artifact's filename doesn't contain the
current version. `--no-windows` leaves Windows out deliberately.

(This step used to be manual copy-paste of the `.sig` contents — a typo there
bricks auto-update for the entire installed base, hence the script.)

### 7. Publish the GitHub release

Upload everything to a **draft** first, check it, then publish:
- the `.dmg`, the `.app.tar.gz` and its `.sig`;
- the setup `.exe` and its `.sig`;
- `latest.json`.

Drafts don't count for `/releases/latest/`, so no installed copy sees a
release whose assets are still uploading:

```bash
gh release create v1.7.0 --draft --title "gitBuddy 1.7.0" --notes-file notes.md \
  release/gitBuddy_1.7.0_universal.dmg \
  release/gitBuddy_1.7.0_universal.app.tar.gz \
  release/gitBuddy_1.7.0_universal.app.tar.gz.sig \
  release/gitBuddy_1.7.0_x64-setup.exe \
  release/gitBuddy_1.7.0_x64-setup.exe.sig \
  latest.json
gh release view v1.7.0 --json assets --jq '.assets[].name'   # all six there?
gh release edit v1.7.0 --draft=false --latest
```

Because the endpoint uses `/releases/latest/download/latest.json`, the asset
must be named exactly `latest.json`. If the installer is unsigned, start the
notes with the "Windows preview" note `release.sh` uses. Mirror the release
notes into `CHANGELOG.md`.

---

## Troubleshooting

### `codesign` fails with `unknown error -26276`

Seen on macOS 27.0 with Xcode 27: `tauri build` dies at "Signing … gitbuddy"
with `error: unknown error -26276=ffffffffffff995c` (errSecInternalError)
although `security find-identity` lists the Developer ID as valid and the
certificate chain verifies. The identity is looked up by *name*, and the name
contains a non-ASCII character ("Björn"). Addressing the same identity by its
SHA-1 hash signs fine:

```bash
security find-identity -v -p codesigning        # copy the 40-hex hash
export APPLE_SIGNING_IDENTITY=<hash>
```

`scripts/release.sh` does this automatically since 2026-09-16 and probes a
throwaway binary before building, so the failure shows up in seconds rather
than after the compile.

### The Windows build failed, or never showed up

`release.sh` stops with the run's URL. Fix the cause, re-run the job in GitHub
Actions (or start one with `gh workflow run release-windows.yml --ref
v<version>`), then continue without rebuilding the Mac side:

```bash
scripts/release.sh --skip-build --windows-run <run-id>
```

### Publishing without Windows

`scripts/release.sh --skip-build --no-windows` publishes the Mac artifacts
alone, and `latest.json` offers Windows no update. That is the only safe way
to leave Windows out. Pointing `windows-x86_64` at an older installer instead
would make every Windows client reinstall it on each launch, because
`version` applies to all platforms.

### Backing out after the tag push

The tag goes to GitHub before the Mac build, so answering "no" at
"Publish?", or a failed build, leaves it there. `release.sh` prints the
command to take it back:

```bash
git push --delete origin v<version> && git tag -d v<version>
```

A run that stopped after the draft was created leaves an unpublished draft.
Fix it on the Releases page, or delete it (`gh release delete v<version>`) and
run the script again.

## Verifying the updater end-to-end (PRD §12)

1. Install the current release (e.g. 1.0.0) from its `.dmg` and run it.
2. Bump to 1.0.1 with one visible change; rebuild; publish the 1.0.1 release
   with its own `latest.json`.
3. Launch the installed 1.0.0. The silent launch check (or Settings → Updates →
   *Check for updates*) should surface the banner; *Install & restart* should
   download, install, and relaunch into 1.0.1.

If the check silently does nothing in a dev build, that's expected: the
placeholder/your-real pubkey only verifies against artifacts signed by the
matching private key, and `tauri dev` has no published endpoint.

On Windows, do the same with the installed NSIS build. The passive installer
runs, the app restarts into the new version, and settings and accounts
survive. Rehearse it first against a staging `latest.json` by overriding the
endpoint with `--config` before trying a real release.
