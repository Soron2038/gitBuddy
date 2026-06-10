# Releasing gitBuddy

How to cut a signed, notarized, auto-updatable release. The app code (updater
plugin, signing-aware `build-app.sh`) is already wired; this is the operational
checklist for the secrets-and-publish steps that can't live in the repo.

The updater endpoint is configured in `src-tauri/tauri.conf.json` as:

```
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

---

## Per-release steps

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

### 5. Generate `latest.json`

```bash
scripts/generate-latest-json.sh --notes "What changed in this release."
```

The script reads the version from `tauri.conf.json`, picks up every
`release/*.app.tar.gz` + `.sig` pair from the build, derives the platform keys
from the filename (`_aarch64` / `_x64`; a `_universal` artifact serves both),
and writes `latest.json` to the repo root. It refuses to run on stale
artifacts whose filename doesn't contain the current version.

(This step used to be manual copy-paste of the `.sig` contents — a typo there
bricks auto-update for the entire installed base, hence the script.)

### 6. Publish the GitHub release

Create an **annotated** tag `v<version>` and upload the `.dmg`, the
`.app.tar.gz`, its `.sig`, and `latest.json`:

```bash
git tag -a v1.0.0 -m "gitBuddy 1.0.0"
git push origin v1.0.0
gh release create v1.0.0 \
  release/gitBuddy_1.0.0_aarch64.dmg \
  release/gitBuddy_1.0.0_aarch64.app.tar.gz \
  release/gitBuddy_1.0.0_aarch64.app.tar.gz.sig \
  latest.json \
  --title "gitBuddy 1.0.0" --notes "…"
```

Because the endpoint uses `/releases/latest/download/latest.json`, the asset
must be named exactly `latest.json`. Mirror the release notes into
`CHANGELOG.md`.

---

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
