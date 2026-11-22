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
        // `*.tmp` out of the app's config directory.
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("writing {tmp:?}: {e}"));
    }

    if let Err(e) = rename_replacing(&tmp, path) {
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

/// `std::fs::rename` over an existing file, retried briefly on Windows.
///
/// There a replace fails with `PermissionDenied` while some other handle has
/// the target open without delete sharing: an antivirus scanner or the
/// search indexer inspecting the file that was just written, or a concurrent
/// replace of the same target. Those holds last milliseconds, so a few short
/// retries (≈310 ms in all) turn a failed save into a slightly slower one.
/// Elsewhere a rename over an existing file never fails that way.
fn rename_replacing(from: &Path, to: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        let mut delay = std::time::Duration::from_millis(10);
        for _ in 0..5 {
            match std::fs::rename(from, to) {
                Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                    std::thread::sleep(delay);
                    delay *= 2;
                }
                other => return other,
            }
        }
    }
    std::fs::rename(from, to)
}

/// Characters Windows doesn't allow in a file name. `/` and `\` are among
/// them, so a name without them can't be a path either.
const FORBIDDEN_IN_NAMES: &[char] = &['<', '>', ':', '"', '/', '\\', '|', '?', '*'];

/// Whether `name` is one of the device names Windows reserves in every
/// folder: `CON`, `PRN`, `AUX`, `NUL`, `COM0`–`COM9`, `LPT0`–`LPT9` (and the
/// superscript `COM¹` …), in any case and with any extension — `nul.txt` is
/// the device too, as is `NUL .txt`.
fn is_reserved_device_name(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or(name).trim_end_matches(' ');
    let upper = stem.to_uppercase();
    match upper.as_str() {
        "CON" | "PRN" | "AUX" | "NUL" => true,
        _ => ["COM", "LPT"].iter().any(|prefix| {
            upper.strip_prefix(prefix).is_some_and(|n| {
                let mut chars = n.chars();
                matches!(
                    (chars.next(), chars.next()),
                    (Some('0'..='9' | '¹' | '²' | '³'), None)
                )
            })
        }),
    }
}

/// Why `name` can't be a file or folder name everywhere gitBuddy runs, or
/// `None` when it can. The rules are Windows' (the strictest of the three
/// platforms) and apply on every platform, so a name that works on one works
/// on all — and the tests run the same everywhere. Phrased to follow
/// "The name …".
pub(crate) fn file_name_problem(name: &str) -> Option<&'static str> {
    if name.is_empty() {
        return Some("is empty");
    }
    if name
        .chars()
        .any(|c| c.is_control() || FORBIDDEN_IN_NAMES.contains(&c))
    {
        return Some(
            r#"contains a character that isn't allowed in file names (< > : " / \ | ? * or a control character)"#,
        );
    }
    if name.ends_with(['.', ' ']) {
        return Some("ends with a dot or a space");
    }
    if is_reserved_device_name(name) {
        return Some("is reserved by Windows for a device (CON, NUL, COM1, …)");
    }
    None
}

/// `raw` rewritten into a name [`file_name_problem`] accepts, or into the
/// empty string when nothing usable is left (the caller picks a fallback).
/// Forbidden and control characters become `_`, trailing dots and spaces go,
/// and a reserved device name gets a leading `_`.
pub(crate) fn sanitize_file_name(raw: &str) -> String {
    let replaced: String = raw
        .chars()
        .map(|c| {
            if c.is_control() || FORBIDDEN_IN_NAMES.contains(&c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    let trimmed = replaced.trim_end_matches(['.', ' ']);
    if trimmed.is_empty() {
        String::new()
    } else if is_reserved_device_name(trimmed) {
        format!("_{trimmed}")
    } else {
        trimmed.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_device_names_are_reserved_with_any_extension() {
        for name in [
            "CON", "con", "Prn", "aux", "NUL", "nul.txt", "NUL .txt", "COM1", "com9.log", "LPT0",
            "lpt5", "COM¹", "lpt³.x",
        ] {
            assert!(is_reserved_device_name(name), "{name:?}");
        }
        for name in [
            "console", "CONS", "nul_", "COM", "COM10", "LPT", "xcon", "_CON", "COMa",
        ] {
            assert!(!is_reserved_device_name(name), "{name:?}");
        }
    }

    #[test]
    fn file_name_problems_follow_the_windows_rules() {
        assert_eq!(file_name_problem("gitBuddy"), None);
        assert_eq!(file_name_problem("my repo.v2"), None);
        assert_eq!(file_name_problem(".dotfiles"), None);
        assert!(file_name_problem("").is_some());
        for bad in [
            "a:b", "D:evil", "a/b", r"a\b", "a|b", "what?", "*", "<x>", "q\"", "tab\t",
        ] {
            assert!(
                file_name_problem(bad).unwrap().contains("character"),
                "{bad:?}"
            );
        }
        for bad in ["..", ".", "name.", "name "] {
            assert!(
                file_name_problem(bad).unwrap().contains("ends with"),
                "{bad:?}"
            );
        }
        assert!(file_name_problem("CON").unwrap().contains("reserved"));
        assert!(file_name_problem("aux.git").unwrap().contains("reserved"));
    }

    #[test]
    fn sanitized_names_pass_the_check() {
        assert_eq!(
            sanitize_file_name("app_1.2_x64-setup.exe"),
            "app_1.2_x64-setup.exe"
        );
        assert_eq!(sanitize_file_name("a:b|c?.txt"), "a_b_c_.txt");
        assert_eq!(sanitize_file_name("report. . ."), "report");
        assert_eq!(sanitize_file_name("CON"), "_CON");
        assert_eq!(sanitize_file_name("nul.tar.gz"), "_nul.tar.gz");
        assert_eq!(sanitize_file_name(" . "), "");
        for raw in ["a:b|c?.txt", "report. . .", "CON", "lpt1.zip", "x\u{7}y"] {
            let clean = sanitize_file_name(raw);
            assert_eq!(file_name_problem(&clean), None, "{raw:?} -> {clean:?}");
        }
    }

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
