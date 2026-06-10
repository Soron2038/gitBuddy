//! Small helpers shared between modules that don't naturally live in any one
//! domain module.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

/// Write `bytes` to `path` via a temp file + rename, so a crash mid-write
/// can't truncate the existing file. Used for `settings.json`, `accounts.json`
/// and any future on-disk config — callers can rely on either seeing the old
/// contents or the new contents, never a partial write.
///
/// The temp file name is unique per call (pid + process-wide counter): two
/// concurrent saves of the same file (e.g. settings UI and aggregator) must
/// not share a temp path, or one write's rename can fail or carry the other
/// write's bytes. The temp file sits next to the target so the rename stays
/// on one volume (rename is only atomic within a filesystem).
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let file_name = path
        .file_name()
        .ok_or_else(|| format!("{path:?} has no file name"))?
        .to_string_lossy()
        .into_owned();
    let unique = format!(
        ".{file_name}.{}.{}.tmp",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    );
    let tmp = path.with_file_name(unique);
    std::fs::write(&tmp, bytes).map_err(|e| format!("writing {tmp:?}: {e}"))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("renaming {tmp:?} → {path:?}: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two concurrent saves (e.g. settings UI + aggregator) must never race
    /// on a shared temp file: every call succeeds and the target always holds
    /// one complete payload, never a partial or vanished one.
    #[test]
    fn concurrent_writes_never_collide_or_corrupt() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("settings.json");
        let threads: Vec<_> = (0..8)
            .map(|i| {
                let target = target.clone();
                std::thread::spawn(move || {
                    let payload = vec![b'a' + i as u8; 4096];
                    for _ in 0..50 {
                        atomic_write(&target, &payload).expect("atomic_write must not race");
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }

        let final_bytes = std::fs::read(&target).unwrap();
        assert_eq!(final_bytes.len(), 4096);
        assert!(final_bytes.iter().all(|b| *b == final_bytes[0]));

        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "stale temp files: {leftovers:?}");
    }
}
