#!/usr/bin/env bash
#
# Re-signs the freshly-built debug binary with a *stable* ad-hoc identifier
# before exec'ing it. Wired into Cargo via .cargo/config.toml's
# `[target.'cfg(target_os = "macos")'].runner` setting, so `cargo run` (and
# therefore `tauri dev`) goes through this on every Rust rebuild.
#
# Without this, macOS gives each fresh `cargo build` output a unique
# transient identifier. The Keychain's "Always Allow" grant is bound to
# that identifier, so every rebuild invalidates all previously-granted
# permissions and the user gets six fresh prompts on the next launch.
# Forcing the identifier to dev.soron2038.gitbuddy makes the grants stick.
#
# The `-` signer is ad-hoc (no Apple Developer ID needed). The
# --options=runtime flag enables the hardened runtime, which matches what
# a properly-signed release build looks like and avoids edge cases where
# unhardened binaries are treated differently by macOS subsystems.

set -euo pipefail

BIN="$1"
shift

# Silently sign; if it ever fails (e.g. /usr/bin/codesign missing on a CI
# box) we still want the binary to run rather than blocking development.
codesign \
  --sign - \
  --force \
  --identifier dev.soron2038.gitbuddy \
  --options runtime \
  "$BIN" \
  2>/dev/null || true

exec "$BIN" "$@"
