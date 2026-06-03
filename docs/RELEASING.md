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

### 5. Write `latest.json`

Paste the **contents** of the `.app.tar.gz.sig` file into `signature`, and point
`url` at the asset's download URL on the release you're about to publish:

```json
{
  "version": "1.0.0",
  "notes": "What changed in this release.",
  "pub_date": "2026-06-03T12:00:00Z",
  "platforms": {
    "darwin-aarch64": {
      "signature": "<contents of gitBuddy_1.0.0_aarch64.app.tar.gz.sig>",
      "url": "https://github.com/Soron2038/gitBuddy/releases/download/v1.0.0/gitBuddy_1.0.0_aarch64.app.tar.gz"
    },
    "darwin-x86_64": {
      "signature": "<contents of the x64 .sig>",
      "url": "https://github.com/Soron2038/gitBuddy/releases/download/v1.0.0/gitBuddy_1.0.0_x64.app.tar.gz"
    }
  }
}
```

For a **universal** build, point both `darwin-aarch64` and `darwin-x86_64` at the
same `…_universal.app.tar.gz` URL with its single signature.

### 6. Publish the GitHub release

Tag `v<version>` (e.g. `v1.0.0`), then upload the `.dmg`, the `.app.tar.gz`, its
`.sig`, and `latest.json`:

```bash
gh release create v1.0.0 \
  release/gitBuddy_1.0.0_aarch64.dmg \
  release/gitBuddy_1.0.0_aarch64.app.tar.gz \
  release/gitBuddy_1.0.0_aarch64.app.tar.gz.sig \
  latest.json \
  --title "gitBuddy 1.0.0" --notes "…"
```

Because the endpoint uses `/releases/latest/download/latest.json`, the asset
must be named exactly `latest.json`.

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
