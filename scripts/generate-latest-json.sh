#!/usr/bin/env bash
#
# Generate the updater manifest `latest.json` from the artifacts in ./release/.
#
# The in-app updater fetches
#   https://github.com/Soron2038/gitBuddy/releases/latest/download/latest.json
# and compares `version` + verifies `signature` against the embedded pubkey.
# This file used to be written by hand (docs/RELEASING.md step 5) — a typo in
# the pasted signature or URL silently bricks auto-update for every installed
# copy, so the mechanical assembly lives in a script instead.
#
# Usage:
#   scripts/generate-latest-json.sh                       # notes default to "gitBuddy <version>"
#   scripts/generate-latest-json.sh --notes "What changed"
#
# Reads:  src-tauri/tauri.conf.json (version), release/*.app.tar.gz(.sig)
# Writes: latest.json (repo root) — upload it as a release asset named exactly
#         `latest.json`.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

NOTES=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --notes)
      [[ $# -ge 2 ]] || { echo "error: --notes needs a value." >&2; exit 1; }
      NOTES="$2"
      shift 2
      ;;
    *)
      echo "error: unknown argument '$1' (only --notes <text> is supported)." >&2
      exit 1
      ;;
  esac
done

VERSION="$(python3 -c 'import json; print(json.load(open("src-tauri/tauri.conf.json"))["version"])')"
[[ -n "$VERSION" ]] || { echo "error: could not read version from tauri.conf.json." >&2; exit 1; }
[[ -n "$NOTES" ]] || NOTES="gitBuddy $VERSION"

shopt -s nullglob
TARBALLS=(release/*.app.tar.gz)
shopt -u nullglob
if [[ ${#TARBALLS[@]} -eq 0 ]]; then
  echo "error: no .app.tar.gz under release/ — run scripts/build-app.sh first" >&2
  echo "       (with TAURI_SIGNING_PRIVATE_KEY exported, so updater artifacts are emitted)." >&2
  exit 1
fi

# Assemble the platforms map. Filename arch tag → updater platform key;
# a universal build serves both architectures from the same artifact.
PLATFORM_ARGS=()
for tarball in "${TARBALLS[@]}"; do
  base="$(basename "$tarball")"
  sig="$tarball.sig"
  if [[ ! -f "$sig" ]]; then
    echo "error: missing signature next to $tarball — expected $sig." >&2
    exit 1
  fi
  if [[ "$base" != *"$VERSION"* ]]; then
    echo "error: $base does not contain version $VERSION — stale artifact in release/?" >&2
    echo "       Clean release/ and rebuild, or bump tauri.conf.json." >&2
    exit 1
  fi
  url="https://github.com/Soron2038/gitBuddy/releases/download/v${VERSION}/${base}"
  case "$base" in
    *_aarch64.*)   keys="darwin-aarch64" ;;
    *_x64.*)       keys="darwin-x86_64" ;;
    *_universal.*) keys="darwin-aarch64 darwin-x86_64" ;;
    *)
      echo "error: cannot derive the architecture from '$base'." >&2
      exit 1
      ;;
  esac
  for key in $keys; do
    PLATFORM_ARGS+=("$key" "$url" "$sig")
  done
done

python3 - "$VERSION" "$NOTES" "${PLATFORM_ARGS[@]}" <<'PY'
import json, sys
from datetime import datetime, timezone

version, notes, *rest = sys.argv[1:]
platforms = {}
for key, url, sig_path in zip(rest[0::3], rest[1::3], rest[2::3]):
    with open(sig_path, encoding="ascii") as f:
        signature = f.read().strip()
    if not signature:
        sys.exit(f"error: {sig_path} is empty.")
    if key in platforms:
        sys.exit(f"error: duplicate platform {key} — both an arch-specific and a "
                 f"universal artifact in release/? Keep one.")
    platforms[key] = {"signature": signature, "url": url}

manifest = {
    "version": version,
    "notes": notes,
    "pub_date": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
    "platforms": platforms,
}
with open("latest.json", "w", encoding="utf-8") as f:
    json.dump(manifest, f, indent=2)
    f.write("\n")
print(f"✓ wrote latest.json (version {version}, platforms: {', '.join(sorted(platforms))})")
PY
