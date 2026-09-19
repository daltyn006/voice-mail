//! Diagnostics: log tails for the GUI Error Center / Diagnostics view.
//!
//! Reads (never writes) `crash.log`, `backend.log`, `stderr.log` under the
//! per-user data dir. Bounded tails so the UI never loads multi-GB logs.

use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct LogTail {
    pub name: String,
    pub path: String,
    pub lines: Vec<String>,
    pub truncated: bool,
}

const TAIL_LINES: usize = 200;
const MAX_LINE: usize = 2000;

fn tail_file(path: PathBuf, name: &str) -> LogTail {
    // Seek-based tail: read the last 256 KiB only, then split lines. Never
    // loads the whole file — backend.log has hit multi-MB of vendor spam
    // live, and read_to_string on a runaway log is an OOM vector.
    use std::io::{Read, Seek, SeekFrom};
    const TAIL_BYTES: u64 = 256 * 1024;
    let mut buf = Vec::new();
    let mut cut_head = false;
    if let Ok(mut f) = std::fs::File::open(&path) {
        if let Ok(len) = f.metadata().map(|m| m.len()) {
            if len > TAIL_BYTES && f.seek(SeekFrom::Start(len - TAIL_BYTES)).is_ok() {
                cut_head = true;
                let _ = f.read_to_end(&mut buf);
            }
        }
    }
    let text = String::from_utf8_lossy(&buf);
    let mut all: Vec<String> = text
        .lines()
        .map(|l| {
            if l.len() > MAX_LINE {
                format!("{}…", &l[..MAX_LINE])
            } else {
                l.to_string()
            }
        })
        .collect();
    // A mid-line seek start yields a partial first line: drop it (and mark
    // truncated) so the tail never shows a misleading fragment.
    if cut_head && !all.is_empty() {
        all.remove(0);
    }
    let truncated = cut_head || all.len() > TAIL_LINES;
    let lines = if all.len() > TAIL_LINES {
        all[all.len() - TAIL_LINES..].to_vec()
    } else {
        all
    };
    LogTail {
        name: name.to_string(),
        path: path.to_string_lossy().into_owned(),
        lines,
        truncated,
    }
}

/// Tails of all three logs (missing files → empty tails, never Err).
pub fn tail_logs() -> Vec<LogTail> {
    let dir = crate::dirs::data_dir();
    vec![
        tail_file(dir.join("crash.log"), "crash.log"),
        tail_file(dir.join("backend.log"), "backend.log"),
        tail_file(dir.join("stderr.log"), "stderr.log"),
    ]
}

/// Best-effort clear of the three logs (Settings → Diagnostics).
pub fn clear_logs() -> Result<(), String> {
    let dir = crate::dirs::data_dir();
    for f in ["crash.log", "backend.log", "stderr.log"] {
        let p = dir.join(f);
        if p.exists() {
            std::fs::write(&p, "").map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// Delete 0-byte `crash-*.dmp` files left by failed minidump writes (seen
/// live: the SEH filter created the file but `MiniDumpWriteDump` failed).
/// Returns the number removed. Called at boot so they never accumulate.
pub fn sweep_empty_dumps() -> usize {
    let dir = crate::dirs::data_dir();
    let mut n = 0;
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for ent in entries.flatten() {
            let p = ent.path();
            let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if !name.starts_with("crash-") || !name.ends_with(".dmp") {
                continue;
            }
            if p.metadata().map(|m| m.len()).unwrap_or(1) == 0 {
                if std::fs::remove_file(&p).is_ok() {
                    n += 1;
                }
            }
        }
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tails_never_err() {
        let tails = tail_logs();
        assert_eq!(tails.len(), 3);
    }

    #[test]
    fn sweep_removes_only_empty_dumps() {
        // Operates on the live data dir by design (that is where the SEH
        // filter writes); the fixtures are uniquely named and the sweep
        // only ever removes 0-byte crash-*.dmp files.
        let dir = crate::dirs::data_dir();
        let _ = std::fs::create_dir_all(&dir);
        let tag = std::process::id();
        let empty = dir.join(format!("crash-pv-test-{tag}.dmp"));
        let keep = dir.join(format!("crash-pv-test-{tag}-keep.dmp"));
        std::fs::write(&empty, b"").unwrap();
        std::fs::write(&keep, b"not-empty").unwrap();
        let _ = sweep_empty_dumps();
        assert!(!empty.exists());
        assert!(keep.exists());
        let _ = std::fs::remove_file(&keep);
    }
}
