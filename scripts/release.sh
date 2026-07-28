#!/usr/bin/env bash
#
# Cut a gitBuddy release: build → verify → latest.json → publish.
#
# Wraps the four manual steps from docs/RELEASING.md so a release is one
# command instead of a checklist that can be half-executed. The version comes
# from tauri.conf.json — bump that (plus Cargo.toml and package.json) and
# commit *before* running this.
#
# Usage:
#   scripts/release.sh                          # build + publish
#   scripts/release.sh --dry-run                # everything except `gh release create`
#   scripts/release.sh --skip-build             # reuse artifacts already in release/
#   scripts/release.sh --notes-file PATH        # release notes (default: CHANGELOG section)
#   scripts/release.sh --target aarch64-apple-darwin   # default is universal
#
# SECRETS — this script asks for them interactively and never writes them
# anywhere. Nothing here belongs in a file: a passphrase in a dotfile gets
# picked up by backups and Spotlight. If a variable is already exported (CI,
# or a shell you set up by hand) that value is used and you are not prompted:
#
#   APPLE_ID                            your Apple ID (email)
#   APPLE_PASSWORD                      app-specific password from appleid.apple.com
#                                       — NOT your Apple ID password; format is
#                                         xxxx-xxxx-xxxx-xxxx
#   TAURI_SIGNING_PRIVATE_KEY_PASSWORD  passphrase for ~/.tauri/gitbuddy.key
#                                       (may be empty if you set none)
#
# The signing identity and team ID are read out of the login keychain, and the
# updater private key out of ~/.tauri/gitbuddy.key, so neither is prompted for.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

UPDATER_KEY="${UPDATER_KEY:-$HOME/.tauri/gitbuddy.key}"

# ── Args ──────────────────────────────────────────────────────────────────
DRY_RUN=0
SKIP_BUILD=0
NOTES_FILE=""
# Universal by default: every release since 1.0.0 has shipped a fat binary, and
# its latest.json maps *both* darwin-aarch64 and darwin-x86_64 at it. Publishing
# an arch-specific build silently strands the other architecture's installed
# base — they get no update offered at all.
TARGET="universal-apple-darwin"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --dry-run)    DRY_RUN=1; shift ;;
    --skip-build) SKIP_BUILD=1; shift ;;
    --notes-file)
      [[ $# -ge 2 ]] || { echo "error: --notes-file needs a path." >&2; exit 1; }
      NOTES_FILE="$2"; shift 2 ;;
    --target)
      [[ $# -ge 2 ]] || { echo "error: --target needs a value." >&2; exit 1; }
      TARGET="$2"; shift 2 ;;
    -h|--help) sed -n '2,30p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "error: unknown argument '$1' (see --help)." >&2; exit 1 ;;
  esac
done

VERSION="$(python3 -c 'import json; print(json.load(open("src-tauri/tauri.conf.json"))["version"])')"
[[ -n "$VERSION" ]] || { echo "error: could not read version from tauri.conf.json." >&2; exit 1; }

case "$TARGET" in
  universal-apple-darwin) ARCH=universal ;;
  aarch64-apple-darwin)   ARCH=aarch64 ;;
  x86_64-apple-darwin)    ARCH=x64 ;;
  *) echo "error: unsupported --target '$TARGET'." >&2; exit 1 ;;
esac

echo "▸ Releasing gitBuddy $VERSION ($ARCH)"
echo

# ── Preflight ─────────────────────────────────────────────────────────────
# Everything that can be checked without secrets, checked before we ask for
# any — no point prompting for a passphrase and then failing on a dirty tree.

command -v gh >/dev/null || { echo "error: gh CLI not found." >&2; exit 1; }
gh auth status >/dev/null 2>&1 || { echo "error: gh is not authenticated (run: gh auth login)." >&2; exit 1; }
[[ -f "$UPDATER_KEY" ]] || { echo "error: updater signing key not found at $UPDATER_KEY (see docs/RELEASING.md)." >&2; exit 1; }

if [[ -n "$(git status --porcelain)" ]]; then
  echo "error: working tree is dirty — commit before releasing, so the tag" >&2
  echo "       points at exactly what gets published." >&2
  exit 1
fi

BRANCH="$(git rev-parse --abbrev-ref HEAD)"
[[ "$BRANCH" == "main" ]] || {
  echo "error: on '$BRANCH', not main. Releases are cut from main." >&2; exit 1
}

if git rev-parse -q --verify "refs/tags/v$VERSION" >/dev/null; then
  TAGGED_AT="$(git rev-list -n1 "v$VERSION")"
  [[ "$TAGGED_AT" == "$(git rev-parse HEAD)" ]] || {
    echo "error: tag v$VERSION exists but points at $TAGGED_AT, not HEAD." >&2
    echo "       Either you forgot to bump the version, or the tag is stale." >&2
    exit 1
  }
fi

if gh release view "v$VERSION" >/dev/null 2>&1; then
  echo "error: release v$VERSION already exists on GitHub. Bump the version," >&2
  echo "       or delete the release first (gh release delete v$VERSION)." >&2
  exit 1
fi

# Signing identity + team ID out of the keychain — no reason to make anyone
# retype what's already provisioned on the machine.
IDENTITY="$(security find-identity -v -p codesigning \
  | sed -n 's/.*"\(Developer ID Application: .*\)".*/\1/p' | head -n 1)"
[[ -n "$IDENTITY" ]] || {
  echo "error: no 'Developer ID Application' certificate in the login keychain." >&2
  echo "       Without it the build can be signed ad-hoc at best, and macOS" >&2
  echo "       will refuse it on any other machine. See docs/RELEASING.md." >&2
  exit 1
}
TEAM_ID="$(sed -n 's/.*(\([A-Z0-9]\{10\}\))$/\1/p' <<<"$IDENTITY")"
[[ -n "$TEAM_ID" ]] || { echo "error: could not parse the team ID out of '$IDENTITY'." >&2; exit 1; }

echo "  identity : $IDENTITY"
echo "  team     : $TEAM_ID"
echo "  tag      : v$VERSION @ $(git rev-parse --short HEAD)"
echo

# ── Release notes ─────────────────────────────────────────────────────────
# Default to this version's CHANGELOG section, so the notes have exactly one
# source of truth and can't drift from the file users read in the repo.
NOTES_TMP=""
if [[ -z "$NOTES_FILE" ]]; then
  NOTES_TMP="$(mktemp)"
  trap 'rm -f "$NOTES_TMP"' EXIT
  python3 - "$VERSION" >"$NOTES_TMP" <<'PY'
import re, sys
version = sys.argv[1]
text = open("CHANGELOG.md", encoding="utf-8").read()
# Everything between "## [x.y.z]" and the next "## [" heading.
m = re.search(rf"^## \[{re.escape(version)}\][^\n]*\n(.*?)(?=^## \[)", text, re.S | re.M)
if not m:
    sys.exit(f"no CHANGELOG.md section for {version}")
sys.stdout.write(m.group(1).strip() + "\n")
PY
  NOTES_FILE="$NOTES_TMP"
  echo "  notes    : CHANGELOG.md section for $VERSION ($(wc -l <"$NOTES_FILE" | tr -d ' ') lines)"
else
  [[ -f "$NOTES_FILE" ]] || { echo "error: --notes-file '$NOTES_FILE' not found." >&2; exit 1; }
  echo "  notes    : $NOTES_FILE"
fi
echo

# ── Secrets: prompt only for what isn't already in the environment ────────
# read -s keeps the passphrase off the screen; none of these are ever written
# to disk or exported beyond this process.
ask_secret() {
  local var="$1" prompt="$2" allow_empty="${3:-0}" value
  if [[ -n "${!var:-}" ]]; then
    echo "  $var: taken from the environment"
    return
  fi
  while true; do
    read -rsp "  $prompt: " value < /dev/tty
    echo
    [[ -n "$value" || "$allow_empty" -eq 1 ]] && break
    echo "  (empty — try again)"
  done
  printf -v "$var" '%s' "$value"
  export "${var?}"
}

ask_plain() {
  local var="$1" prompt="$2" value
  if [[ -n "${!var:-}" ]]; then
    echo "  $var: taken from the environment (${!var})"
    return
  fi
  read -rp "  $prompt: " value < /dev/tty
  printf -v "$var" '%s' "$value"
  export "${var?}"
}

echo "▸ Credentials (nothing is written to disk)"
ask_plain  APPLE_ID "Apple ID (email)"
ask_secret APPLE_PASSWORD "App-specific password (xxxx-xxxx-xxxx-xxxx)"
# The updater key is scrypt-encrypted; an empty passphrase is legitimate if
# that's what was used at generation time, so don't insist on a value.
ask_secret TAURI_SIGNING_PRIVATE_KEY_PASSWORD "Updater key passphrase (empty if none)" 1
echo

if ! grep -Eq '^[a-z]{4}-[a-z]{4}-[a-z]{4}-[a-z]{4}$' <<<"$APPLE_PASSWORD"; then
  echo "  warning: that doesn't look like an app-specific password (expected" >&2
  echo "           xxxx-xxxx-xxxx-xxxx). Notarization rejects normal Apple ID" >&2
  echo "           passwords with a confusing 401." >&2
  read -rp "  Continue anyway? [y/N] " yn < /dev/tty
  [[ "$yn" == [yY] ]] || exit 1
fi

export APPLE_SIGNING_IDENTITY="$IDENTITY"
export APPLE_TEAM_ID="$TEAM_ID"
export TAURI_SIGNING_PRIVATE_KEY="$(cat "$UPDATER_KEY")"

# Fail on bad credentials now, not after a multi-minute build.
echo "▸ Checking the notarization credentials with Apple…"
if ! xcrun notarytool history --apple-id "$APPLE_ID" --password "$APPLE_PASSWORD" \
       --team-id "$TEAM_ID" >/dev/null 2>&1; then
  echo "error: Apple rejected these credentials. Check that the app-specific" >&2
  echo "       password was generated under $APPLE_ID and that team $TEAM_ID" >&2
  echo "       is the right one." >&2
  exit 1
fi
echo "  ✓ accepted"
echo

# ── Build ─────────────────────────────────────────────────────────────────
# Stale artifacts from previous versions are moved aside rather than deleted —
# generate-latest-json.sh globs release/*.app.tar.gz and refuses to run when it
# finds one whose filename doesn't carry the current version, and these are
# often the only local copy of what was published.
if compgen -G "release/*" >/dev/null; then
  STALE=()
  for f in release/*; do
    case "$(basename "$f")" in
      *"$VERSION"*|archive) ;;
      *) STALE+=("$f") ;;
    esac
  done
  if [[ ${#STALE[@]} -gt 0 ]]; then
    mkdir -p release/archive
    echo "▸ Moving ${#STALE[@]} artifact(s) from earlier versions to release/archive/"
    mv "${STALE[@]}" release/archive/
    echo
  fi
fi

if [[ "$SKIP_BUILD" -eq 0 ]]; then
  scripts/build-app.sh --clean --target "$TARGET"
else
  echo "▸ --skip-build: reusing what's in release/"
fi
echo

# ── Verify before publishing anything ─────────────────────────────────────
echo "▸ Verifying the signature"
APP="$(find src-tauri/target -type d -name 'gitBuddy.app' -path '*/release/bundle/macos/*' | head -n 1)"
[[ -n "$APP" ]] || { echo "error: could not find the built gitBuddy.app." >&2; exit 1; }
codesign --verify --deep --strict --verbose=2 "$APP"
spctl -a -vv "$APP"
echo

DMG="release/gitBuddy_${VERSION}_${ARCH}.dmg"
TARBALL="release/gitBuddy_${VERSION}_${ARCH}.app.tar.gz"
for f in "$DMG" "$TARBALL" "$TARBALL.sig"; do
  [[ -f "$f" ]] || { echo "error: expected artifact missing: $f" >&2; exit 1; }
done

# ── latest.json ───────────────────────────────────────────────────────────
scripts/generate-latest-json.sh --notes "gitBuddy $VERSION"
echo

python3 - "$VERSION" <<'PY'
import json, sys
d = json.load(open("latest.json"))
assert d["version"] == sys.argv[1], f"latest.json says {d['version']}, expected {sys.argv[1]}"
plats = sorted(d["platforms"])
print(f"  latest.json → version {d['version']}, platforms: {', '.join(plats)}")
# A universal build must serve both; anything less strands an architecture.
missing = {"darwin-aarch64", "darwin-x86_64"} - set(plats)
if missing:
    print(f"  note: no entry for {', '.join(sorted(missing))} — that architecture")
    print(f"        will not be offered this update.")
PY
echo

# ── Publish ───────────────────────────────────────────────────────────────
if [[ "$DRY_RUN" -eq 1 ]]; then
  echo "▸ --dry-run: stopping before publishing. Artifacts are in release/."
  exit 0
fi

echo "▸ About to publish v$VERSION to GitHub:"
printf '    %s\n' "$DMG" "$TARBALL" "$TARBALL.sig" latest.json
read -rp "  Publish? [y/N] " yn < /dev/tty
[[ "$yn" == [yY] ]] || { echo "  aborted."; exit 1; }

if ! git rev-parse -q --verify "refs/tags/v$VERSION" >/dev/null; then
  git tag -a "v$VERSION" -m "gitBuddy $VERSION"
fi
git push origin "v$VERSION"

gh release create "v$VERSION" \
  "$DMG" "$TARBALL" "$TARBALL.sig" latest.json \
  --title "gitBuddy $VERSION" \
  --notes-file "$NOTES_FILE"

echo
echo "✓ https://github.com/Soron2038/gitBuddy/releases/tag/v$VERSION"
