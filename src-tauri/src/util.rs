//! Small helpers shared between modules that don't naturally live in any one
//! domain module.

use std::path::Path;

/// Write `bytes` to `path` via a temp file + rename, so a crash mid-write
/// can't truncate the existing file. Used for `settings.json`, `accounts.json`
/// and any future on-disk config — callers can rely on either seeing the old
/// contents or the new contents, never a partial write.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes).map_err(|e| format!("writing {tmp:?}: {e}"))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("renaming {tmp:?} → {path:?}: {e}"))?;
    Ok(())
}
