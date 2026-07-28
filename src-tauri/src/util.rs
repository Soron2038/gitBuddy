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

    // Write + fsync the temp file *before* renaming. The rename is atomic with
    // respect to other processes, but without the fsync the directory entry
    // can reach the platter ahead of the data — so a power loss between the
    // two leaves a zero-length or garbage settings.json. That file then fails
    // to parse on the next launch, which is exactly the state the callers
    // treat as fatal.
    let write_result = (|| -> std::io::Result<()> {
        use std::io::Write;
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()
    })();
    if let Err(e) = write_result {
        // Don't leave the temp file behind: nothing else ever cleans
        // `~/Library/Application Support/dev.soron2038.gitbuddy/*.tmp`.
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("writing {tmp:?}: {e}"));
    }

    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("renaming {tmp:?} → {path:?}: {e}"));
    }

    // Also fsync the containing directory so the rename itself is durable.
    // Best-effort: a failure here means the data is written but the directory
    // entry may not survive a crash, which is strictly better than the
    // pre-fsync behaviour and not worth failing the save over.
    if let Some(dir) = path.parent() {
        if let Ok(d) = std::fs::File::open(dir) {
            let _ = d.sync_all();
        }
    }
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

    /// A failed write must not leave its temp file behind — nothing else ever
    /// cleans them up, and they accumulate in the app-support directory.
    #[test]
    fn a_failed_write_cleans_up_its_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        // Target inside a directory that doesn't exist: File::create fails, so
        // we take the write-error path.
        let target = dir.path().join("missing-subdir").join("settings.json");
        assert!(atomic_write(&target, b"payload").is_err());

        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "stale temp files: {leftovers:?}");
    }
}
