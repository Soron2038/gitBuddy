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
#   scripts/build-app.sh                 # build for the host architecture
#   scripts/build-app.sh --clean         # wipe the old bundle dir first
#   scripts/build-app.sh --target aarch64-apple-darwin   # arch-specific build
#   scripts/build-app.sh --target universal-apple-darwin # fat binary
#
# Any flags other than --clean are passed straight through to `tauri build`.
#
# SIGNING: this produces an *ad-hoc / unsigned* bundle. Production signing +
# notarization with an Apple Developer ID is a later milestone (see
# docs/DECISIONS.md and PRD M7). Until then, opening the .app on another Mac
# trips Gatekeeper — the recipient right-clicks the app and chooses "Open", or
# runs `xattr -dr com.apple.quarantine /path/to/gitBuddy.app`. If the standard
# Apple signing env vars (APPLE_SIGNING_IDENTITY, APPLE_ID, APPLE_PASSWORD,
# APPLE_TEAM_ID) are exported, `tauri build` picks them up automatically and
# this script needs no change.

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

# ── Parse args: peel off --clean, pass the rest to `tauri build` ──────────
CLEAN=0
PASSTHROUGH=()
for arg in "$@"; do
  case "$arg" in
    --clean) CLEAN=1 ;;
    *) PASSTHROUGH+=("$arg") ;;
  esac
done

BUNDLE_ROOT="src-tauri/target"
OUT_DIR="release"

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

# ── Copy the .dmg into ./release/ under a clean name ──────────────────────
mkdir -p "$OUT_DIR"
DMG_NAME="$(basename "$DMG")"
cp -f "$DMG" "$OUT_DIR/$DMG_NAME"

# ── Summary ───────────────────────────────────────────────────────────────
echo
echo "✓ Build complete."
echo
printf '  %-9s %s\n' "DMG:" "$OUT_DIR/$DMG_NAME  ($(du -h "$OUT_DIR/$DMG_NAME" | cut -f1))"
[[ -n "$APP" ]] && printf '  %-9s %s\n' "App:" "$APP"
echo
echo "  This bundle is unsigned (ad-hoc). On another Mac, Gatekeeper will block"
echo "  it until the recipient right-clicks → Open, or runs:"
echo "    xattr -dr com.apple.quarantine \"/Applications/gitBuddy.app\""
