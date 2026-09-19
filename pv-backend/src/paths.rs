//! Override-directory policy: local absolute paths allowed,
//! canonicalize-and-stay-put. Blocks UNC/network/removable for active
//! recordings + outputs (latency/disconnect corruption); model storage gets
//! a warn-only pass for removable. Cloud-synced parents (OneDrive/Dropbox)
//! are allowed with a stutter warning. Symlinks/junctions are resolved to
//! their physical target BEFORE the blocklist runs, so a symlink into
//! `\\server\share` is blocked even when the literal path looks local.
//!
//! Windows-only drive typing via raw `GetDriveTypeW` (kernel32 is always
//! present): no new crates for one query.

use std::path::PathBuf;

/// What the path will hold. Recordings + outputs are latency-sensitive;
/// model blobs are bulk loads that tolerate slow media (with a warning).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Purpose {
    Models,
    Output,
    Recordings,
    Reviews,
}

impl Purpose {
    fn latency_sensitive(self) -> bool {
        match self {
            Purpose::Models => false,
            Purpose::Output | Purpose::Recordings | Purpose::Reviews => true,
        }
    }
}

/// Outcome: the canonical path plus an optional non-fatal warning
/// (cloud-sync stutter, removable model media).
#[derive(Clone, Debug)]
pub struct Accepted {
    pub path: PathBuf,
    pub warning: Option<String>,
}

#[link(name = "kernel32")]
extern "system" {
    fn GetDriveTypeW(root: *const u16) -> u32;
}

const DRIVE_UNKNOWN: u32 = 0;
const DRIVE_REMOVABLE: u32 = 2;
const DRIVE_REMOTE: u32 = 4;

/// True for `\\server\share` and `\\?\…` prefixes (verbatim included).
pub fn is_unc(path: &str) -> bool {
    path.starts_with(r"\\") || path.starts_with("//")
}

/// True when any component is an NTFS ADS stream (`name:stream`).
/// Drive-letter colons (`C:\…`) are exempt — only post-separator colons count.
pub fn has_ads(path: &str) -> bool {
    let mut chars = path.chars().peekable();
    // Skip a leading `X:` drive prefix.
    if path.len() >= 2 {
        let b: Vec<char> = path.chars().take(3).collect();
        if b.len() >= 2 && b[1] == ':' {
            for _ in 0..2 {
                chars.next();
            }
        }
    }
    let mut after_sep = false;
    for c in chars {
        if c == '\\' || c == '/' {
            after_sep = true;
            continue;
        }
        if c == ':' && after_sep {
            return true;
        }
    }
    false
}

fn drive_kind(root: &str) -> u32 {
    let wide: Vec<u16> = root.encode_utf16().chain(Some(0)).collect();
    unsafe { GetDriveTypeW(wide.as_ptr()) }
}

/// `C:\`, `D:` → `C:\` root for the drive-type query. UNC → itself.
fn drive_root(canonical: &str) -> String {
    if is_unc(canonical) {
        return canonical.to_string();
    }
    if canonical.len() >= 2 && canonical.as_bytes()[1] == b':' {
        return format!("{}:\\", &canonical[0..1]);
    }
    canonical.to_string()
}

fn is_cloud_synced(canonical: &str) -> bool {
    let low = canonical.to_ascii_lowercase();
    low.contains("onedrive") || low.contains("dropbox")
}

/// Validate + canonicalize a user override dir. All literal checks (empty,
/// ADS, absolute, UNC-for-capture) run BEFORE any filesystem mutation, so a
/// rejected path never triggers a network round-trip or credential prompt.
/// Creation happens only for survivors; symlinks/junctions then resolve to
/// the physical target for the second (target-side) check round.
pub fn accept_override(raw: &str, purpose: Purpose) -> Result<Accepted, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("empty path".to_string());
    }
    if has_ads(trimmed) {
        return Err("ADS stream paths are not allowed".to_string());
    }
    let wanted = PathBuf::from(trimmed);
    if !wanted.is_absolute() {
        return Err("override must be an absolute path".to_string());
    }
    if is_unc(trimmed) {
        if purpose.latency_sensitive() {
            return Err("network (UNC) paths are blocked for recordings/outputs — pick a local drive".to_string());
        }
        // Models on UNC: allowed but flagged — no silent network dependency.
        let _ = std::fs::create_dir_all(&wanted)
            .map_err(|e| format!("cannot use {trimmed}: {e}"))?;
        let canon =
            std::fs::canonicalize(&wanted).map_err(|e| format!("cannot resolve {trimmed}: {e}"))?;
        let flat = canon.to_string_lossy().to_string();
        let check = flat.strip_prefix(r"\\?\").unwrap_or(&flat).to_string();
        return Ok(Accepted {
            path: PathBuf::from(check),
            warning: Some(
                "Network folder: model loads depend on the connection.".to_string(),
            ),
        });
    }
    std::fs::create_dir_all(&wanted).map_err(|e| format!("cannot use {trimmed}: {e}"))?;
    // Canonicalize AFTER creation so junctions/symlinks resolve to target.
    let canon = std::fs::canonicalize(&wanted).map_err(|e| format!("cannot resolve {trimmed}: {e}"))?;
    let flat = canon.to_string_lossy().to_string();
    // Verbatim `\\?\C:\…` prefix: strip for storage + checks (target already resolved).
    let check = flat.strip_prefix(r"\\?\").unwrap_or(&flat).to_string();
    if is_unc(&check) && purpose.latency_sensitive() {
        return Err("that folder resolves to a network location — pick a local drive".to_string());
    }
    if has_ads(&check) {
        return Err("ADS stream paths are not allowed".to_string());
    }
    let kind = drive_kind(&drive_root(&check));
    // UNKNOWN is treated like REMOTE here (fail closed for capture paths):
    // GetDriveType fails on volumes it cannot classify, and a capture stream
    // must never depend on unclassifiable media.
    if (kind == DRIVE_REMOTE || kind == DRIVE_UNKNOWN) && purpose.latency_sensitive() {
        return Err("network drives are blocked for recordings/outputs — pick a local drive".to_string());
    }
    let mut warning = None;
    if kind == DRIVE_REMOVABLE && !purpose.latency_sensitive() {
        warning = Some("Removable drive: model loads will be slower than internal storage.".to_string());
    } else if kind == DRIVE_REMOVABLE {
        return Err("removable drives are blocked for active recordings/outputs — pick an internal drive".to_string());
    }
    if is_cloud_synced(&check) {
        let w = "Saving directly to a cloud-synced folder may cause recording stutters.".to_string();
        warning = Some(match warning {
            Some(p) => format!("{p} {w}"),
            None => w,
        });
    }
    Ok(Accepted {
        path: PathBuf::from(check),
        warning,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unc_and_ads_detectors() {
        assert!(is_unc(r"\\server\share"));
        assert!(is_unc("//server/share"));
        assert!(!is_unc("C:\\audio"));
        assert!(!is_unc("D:/takes"));
        assert!(has_ads("C:\\dir\\file.txt:stream"));
        assert!(!has_ads("C:\\dir\\file.txt"));
        assert!(!has_ads("C:"));
    }

    #[test]
    fn relative_paths_rejected() {
        assert!(accept_override("relative/path", Purpose::Output).is_err());
        assert!(accept_override("", Purpose::Models).is_err());
    }

    #[test]
    fn unc_blocked_for_output_but_local_ok() {
        assert!(accept_override(r"\\srv\share", Purpose::Output).is_err());
        let dir = std::env::temp_dir().join(format!("pv-paths-{}", std::process::id()));
        let ok = accept_override(dir.to_string_lossy().as_ref(), Purpose::Output).unwrap();
        assert!(ok.path.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cloud_sync_warns_but_allows() {
        let base = std::env::temp_dir().join(format!(
            "pv-cloud-{}-OneDrive-test",
            std::process::id()
        ));
        let ok = accept_override(base.to_string_lossy().as_ref(), Purpose::Recordings).unwrap();
        assert!(ok.warning.is_some_and(|w| w.contains("cloud-synced")));
        let _ = std::fs::remove_dir_all(&base);
    }
}
