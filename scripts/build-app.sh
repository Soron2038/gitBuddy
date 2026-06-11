#!/usr/bin/env bash
#
# Build a release bundle of gitBuddy and surface the installable .dmg.
#
# Wraps `tauri build` (which runs `npm run build` for the SvelteKit frontend,
# compiles the Rust core in --release, and bundles a .app + .dmg) and then
# copies the freshly-produced .dmg into ./release/ under a clean, predictable
# name so it's easy to find and hand off.
#
# Usage:
#   scripts/build-app.sh                 # release build (needs TAURI_SIGNING_PRIVATE_KEY)
#   scripts/build-app.sh --unsigned      # local smoke-test build, no updater artifacts
#   scripts/build-app.sh --clean         # wipe the old bundle dir first
#   scripts/build-app.sh --target aarch64-apple-darwin   # arch-specific build
#   scripts/build-app.sh --target universal-apple-darwin # fat binary
#
# Any flags other than --clean / --unsigned are passed straight through to
# `tauri build`.
#
# SIGNING (Apple): without the standard Apple env vars this produces an
# *ad-hoc / unsigned* bundle — opening the .app on another Mac trips Gatekeeper
# (recipient right-clicks → "Open", or `xattr -dr com.apple.quarantine
# /path/to/gitBuddy.app`). Export APPLE_SIGNING_IDENTITY, APPLE_ID,
# APPLE_PASSWORD and APPLE_TEAM_ID and `tauri build` signs + notarizes + staples
# automatically; this script needs no change. See docs/RELEASING.md.
#
# SIGNING (updater): tauri.conf.json sets `bundle.createUpdaterArtifacts: true`,
# so `tauri build` also emits a `.app.tar.gz` updater bundle and its `.sig`
# minisign signature — but ONLY if TAURI_SIGNING_PRIVATE_KEY (and, if the key
# is password-protected, TAURI_SIGNING_PRIVATE_KEY_PASSWORD) is exported. Generate
# the keypair once with `npm run tauri signer generate` and paste the public key
# into tauri.conf.json's `plugins.updater.pubkey`. Without the env var the build
# fails at the bundle step. See docs/RELEASING.md for the full release flow.

set -euo pipefail

# ── Resolve repo root from this script's location ─────────────────────────
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

# ── macOS only: the .dmg target is a macOS bundle format ──────────────────
if [[ "$(uname)" != "Darwin" ]]; then
  echo "error: this script builds a macOS .dmg and must run on macOS (found $(uname))." >&2
  exit 1
fi

# ── Parse args: peel off --clean / --unsigned, pass the rest through ──────
CLEAN=0
UNSIGNED=0
PASSTHROUGH=()
for arg in "$@"; do
  case "$arg" in
    --clean) CLEAN=1 ;;
    --unsigned) UNSIGNED=1 ;;
    *) PASSTHROUGH+=("$arg") ;;
  esac
done

# ── Fail fast on the missing updater key ───────────────────────────────────
# tauri.conf.json sets `bundle.createUpdaterArtifacts: true`, so a default
# build *will* fail at the bundle step (after minutes of compiling) without
# TAURI_SIGNING_PRIVATE_KEY. Catch that up front; --unsigned opts into a
# local build that disables updater artifacts instead.
if [[ "$UNSIGNED" -eq 0 && -z "${TAURI_SIGNING_PRIVATE_KEY:-}" ]]; then
  echo "error: TAURI_SIGNING_PRIVATE_KEY is not set." >&2
  echo "       A release build signs the updater artifact and needs the minisign key" >&2
  echo "       (see docs/RELEASING.md). For a local smoke-test without the key, run:" >&2
  echo "         scripts/build-app.sh --unsigned" >&2
  exit 1
fi
if [[ "$UNSIGNED" -eq 1 ]]; then
  # Overlay config: skip updater artifacts so the bundle step succeeds with
  # no signing key. The resulting .dmg installs fine but cannot be shipped
  # through the in-app updater.
  PASSTHROUGH+=(--config '{"bundle":{"createUpdaterArtifacts":false}}')
fi

BUNDLE_ROOT="src-tauri/target"
OUT_DIR="release"
ICNS="src-tauri/icons/icon.icns"

# Give the .dmg FILE the gitBuddy icon in Finder. Tauri already bakes a
# `.VolumeIcon.icns` into the image (so the *mounted* volume shows the icon),
# but the .dmg file's own Finder icon stays generic otherwise. We attach the
# icon to the file's resource fork — using only Xcode Command Line Tools,
# which are a prerequisite for building Tauri anyway, so no extra dependency.
#
# Caveat: the resource-fork icon survives local copies and AirDrop but is
# stripped by a browser download (a macOS limitation) — the volume icon shown
# on mount is what survives web distribution. Best-effort: any failure here
# just warns, it never fails an otherwise-good build (call site uses `|| …`).
apply_dmg_icon() {
  local target="$1"
  if [[ ! -f "$ICNS" ]]; then
    echo "  note: $ICNS not found — leaving the generic .dmg icon."
    return 0
  fi
  for tool in sips DeRez Rez SetFile; do
    if ! command -v "$tool" >/dev/null 2>&1; then
      echo "  note: '$tool' missing (install Xcode Command Line Tools) — kept generic .dmg icon."
      return 0
    fi
  done
  local td
  td="$(mktemp -d)"
  cp "$ICNS" "$td/icon.icns"
  # `sips -i` writes the image as the file's own custom-icon resource, which
  # DeRez then extracts as an 'icns' resource we can graft onto the target.
  sips -i "$td/icon.icns" >/dev/null 2>&1
  DeRez -only icns "$td/icon.icns" > "$td/icon.rsrc" 2>/dev/null
  xattr -d com.apple.ResourceFork "$target" 2>/dev/null || true  # drop any prior icon
  Rez -append "$td/icon.rsrc" -o "$target" 2>/dev/null
  SetFile -a C "$target"                                          # flag: has custom icon
  rm -rf "$td"
  echo "  ✓ applied app icon to the .dmg file."
}

if [[ "$CLEAN" -eq 1 ]]; then
  echo "▸ Cleaning previous bundles…"
  # Only the bundle outputs, not the whole target dir — keeps the (slow to
  # rebuild) compiled dependencies cached.
  find "$BUNDLE_ROOT" -type d -path '*/release/bundle' -prune -exec rm -rf {} + 2>/dev/null || true
fi

# Marker so we can reliably find *this* run's .dmg afterwards, even when a
# --target build drops it under a triple-specific path.
MARKER="$(mktemp)"
trap 'rm -f "$MARKER"' EXIT

echo "▸ Building gitBuddy (release)…"
echo "  This recompiles the Rust core in --release; the first run is slow."
echo
# `${arr[@]+"${arr[@]}"}` expands to nothing when the array is empty — the
# portable guard for `set -u` on macOS's bash 3.2, where a bare
# `"${arr[@]}"` on an empty array counts as an unbound variable.
npm run tauri -- build ${PASSTHROUGH[@]+"${PASSTHROUGH[@]}"}

# ── Locate the freshest .dmg produced by this run ─────────────────────────
DMG="$(find "$BUNDLE_ROOT" -type f -name '*.dmg' -path '*/bundle/dmg/*' -newer "$MARKER" \
  -print0 2>/dev/null | xargs -0 ls -t 2>/dev/null | head -n 1 || true)"

if [[ -z "$DMG" ]]; then
  echo "error: build finished but no .dmg was found under $BUNDLE_ROOT." >&2
  echo "       Check that tauri.conf.json's bundle.targets includes \"dmg\" (or \"all\")." >&2
  exit 1
fi

APP="$(find "$BUNDLE_ROOT" -type d -name 'gitBuddy.app' -path '*/bundle/macos/*' -newer "$MARKER" \
  -print 2>/dev/null | head -n 1 || true)"

# ── Copy the .dmg into ./release/ under a clean name + brand its icon ─────
mkdir -p "$OUT_DIR"
DMG_NAME="$(basename "$DMG")"
cp -f "$DMG" "$OUT_DIR/$DMG_NAME"
apply_dmg_icon "$OUT_DIR/$DMG_NAME" || echo "  note: icon step skipped (non-fatal)."

# ── Copy updater artifacts (.app.tar.gz + .sig) when this build made them ──
# Produced only when TAURI_SIGNING_PRIVATE_KEY was exported (the build emits
# them next to the .app under bundle/macos/). Both go on the GitHub release and
# are referenced from latest.json — see docs/RELEASING.md.
#
# Tauri names the tarball unversioned (gitBuddy.app.tar.gz), but
# generate-latest-json.sh requires the version + arch in the filename (and
# derives the platform key from a `_universal` / `_aarch64` / `_x64` tag). Rename
# it to match the DMG's `_<version>_<arch>` scheme so the manifest step is
# hands-off. The minisign .sig signs the tarball *bytes*, not its name, so
# renaming is safe.
UPDATER_TARBALL="$(find "$BUNDLE_ROOT" -type f -name '*.app.tar.gz' -path '*/bundle/macos/*' \
  -newer "$MARKER" -print 2>/dev/null | head -n 1 || true)"
UPDATER_FOUND=0
if [[ -n "$UPDATER_TARBALL" ]]; then
  UPDATER_NAME="${DMG_NAME%.dmg}.app.tar.gz"   # e.g. gitBuddy_1.0.2_universal.app.tar.gz
  cp -f "$UPDATER_TARBALL" "$OUT_DIR/$UPDATER_NAME"
  [[ -f "$UPDATER_TARBALL.sig" ]] && cp -f "$UPDATER_TARBALL.sig" "$OUT_DIR/$UPDATER_NAME.sig"
  UPDATER_FOUND=1
fi

# ── Summary ───────────────────────────────────────────────────────────────
echo
echo "✓ Build complete."
echo
printf '  %-9s %s\n' "DMG:" "$OUT_DIR/$DMG_NAME  ($(du -h "$OUT_DIR/$DMG_NAME" | cut -f1))"
[[ -n "$APP" ]] && printf '  %-9s %s\n' "App:" "$APP"
if [[ "$UPDATER_FOUND" -eq 1 ]]; then
  printf '  %-9s %s\n' "Updater:" "$OUT_DIR/$UPDATER_NAME (+ .sig)"
fi
echo
if [[ "$UPDATER_FOUND" -eq 1 ]]; then
  echo "  Updater artifacts present — upload the .dmg, the .app.tar.gz and its .sig"
  echo "  to the GitHub release, then publish latest.json. See docs/RELEASING.md."
else
  echo "  No updater artifacts: TAURI_SIGNING_PRIVATE_KEY was not set, so the"
  echo "  in-app updater can't ship this build. See docs/RELEASING.md to enable it."
fi
echo
echo "  If unsigned (no Apple env vars), Gatekeeper blocks the .app on other Macs"
echo "  until the recipient right-clicks → Open, or runs:"
echo "    xattr -dr com.apple.quarantine \"/Applications/gitBuddy.app\""
