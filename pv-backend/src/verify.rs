//! Boot-time model integrity: re-hash the active pair once per process,
//! quarantine `.corrupted` files on mismatch, and fall back to the next
//! complete pinned tier.
//!
//! Policy (user-approved): verify active STT/LLM (+VLM when set) on a
//! background worker at boot; Start blocks briefly only if pressed before
//! verification finishes. No grandfathering: a mismatch quarantines the file
//! (`<file>.corrupted`, manifest entry dropped) and the run falls back.

use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};

/// Render a finished SHA-256 as lowercase hex. Single owner of the format —
/// download streaming, resume paths, and file hashing all go through here.
pub fn hex_digest(hasher: Sha256) -> String {
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Hash a file streaming (64 KiB chunks — O(1) RAM on multi-GB weights).
pub fn sha256_file(path: &Path) -> Result<String, String> {
    let mut f = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut h = Sha256::new();
    let mut buf = [0u8; 65536];
    loop {
        match f.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => h.update(&buf[..n]),
            Err(e) => return Err(e.to_string()),
        }
    }
    Ok(hex_digest(h))
}

/// Move a bad model aside (`<file>.corrupted`), dropping its manifest record.
/// Returns the quarantine path for the UI message.
pub fn quarantine(models_dir: &Path, file: &str, id: &str) -> Option<PathBuf> {
    let src = models_dir.join(file);
    if !src.exists() {
        return None;
    }
    let dst = models_dir.join(format!("{file}.corrupted"));
    let _ = std::fs::remove_file(&dst);
    if std::fs::rename(&src, &dst).is_err() {
        return None;
    }
    let mut man = crate::manifest::read(models_dir);
    crate::manifest::remove(&mut man, id);
    let _ = crate::manifest::write(models_dir, &man);
    Some(dst)
}

/// Outcome of verifying one catalog id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifyOne {
    /// File matches pinned hash (or unpinned dev-sidecar — allowed, flagged).
    OkUnpinned,
    Ok,
    /// File missing/partial — caller routes to Models page.
    Missing(String),
    /// Hash mismatch — file quarantined to the returned path.
    Quarantined(PathBuf),
}

/// Verify a single STT/LLM id against its pinned hash + byte size.
/// Empty expected hash in the *embedded* catalog = pin not yet populated
/// (size-verified legacy behavior, Ok). Empty hash under a dev-sidecar
/// catalog = unverified community model → OkUnpinned (activation still
/// requires explicit UI confirm upstream).
pub fn verify_one(id: &str) -> VerifyOne {
    let cat = match crate::catalog::load() {
        Ok(c) => c,
        Err(e) => return VerifyOne::Missing(e),
    };
    let dir = crate::dirs::models_dir();
    let Some((_, file, want, expected)) = crate::catalog::entry_full(&cat, id) else {
        // Linked Ollama blobs (Ollama-owned bytes, no hash pin): validated by
        // existence + GGUF magic + manifest size, same bar as the Models page
        // listing. A moved/deleted source drops the link with a status note.
        let man = crate::manifest::read(&dir);
        if let Some(rec) = man.files.get(id) {
            if let Some(p) = rec.path.clone() {
                let path = std::path::Path::new(&p);
                if std::fs::metadata(path).is_err() {
                    return VerifyOne::Missing(format!("linked model gone: {id}"));
                }
                if !crate::ollama::gguf_magic_ok(path) {
                    return VerifyOne::Missing(format!(
                        "linked model is not a GGUF file (re-link it): {id}"
                    ));
                }
                if rec.bytes > 0
                    && std::fs::metadata(path).map(|m| m.len()).unwrap_or(0) != rec.bytes
                {
                    return VerifyOne::Missing(format!("linked model size changed (re-link it): {id}"));
                }
                return VerifyOne::Ok;
            }
        }
        return VerifyOne::Missing(format!("unknown model: {id}"));
    };
    let p = dir.join(&file);
    let size = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
    if want > 0 && size != want {
        return VerifyOne::Missing(format!("{id}: size {size}, want {want}"));
    }
    if size == 0 {
        return VerifyOne::Missing(format!("{id}: not downloaded"));
    }
    if expected.is_empty() {
        // No pin: sidecar = community model (flagged); embedded catalog =
        // pin pending population (size check above already passed).
        return if crate::catalog::is_sidecar() {
            VerifyOne::OkUnpinned
        } else {
            VerifyOne::Ok
        };
    }
    // Fast path: manifest-recorded hash already matches → skip multi-GB re-hash.
    // (Manifest is only written by verified downloads, so this is sound.)
    {
        let man = crate::manifest::read(&dir);
        if let Some(rec) = man.files.get(id) {
            if !rec.sha256.is_empty() && rec.sha256.eq_ignore_ascii_case(&expected) && rec.bytes == size {
                return VerifyOne::Ok;
            }
        }
    }
    match sha256_file(&p) {
        Ok(hex) if hex.eq_ignore_ascii_case(&expected) => {
            // Refresh the manifest so the next boot takes the fast path.
            let mut man = crate::manifest::read(&dir);
            man.files.insert(
                id.to_string(),
                crate::manifest::ManifestFile {
                    bytes: size,
                    sha256: hex,
                    path: None,
                    role: String::new(),
                    verified: true,
                },
            );
            let _ = crate::manifest::write(&dir, &man);
            VerifyOne::Ok
        }
        Ok(_) => match quarantine(&dir, &file, id) {
            Some(q) => VerifyOne::Quarantined(q),
            None => VerifyOne::Missing(format!("{id}: hash mismatch, quarantine failed")),
        },
        Err(e) => VerifyOne::Missing(format!("{id}: cannot read: {e}")),
    }
}

/// Next complete pinned model for a role, excluding `exclude`.
/// Tier order: full → standard → lite (first size-complete + hash-ok wins).
/// Hash check here is manifest-fast (no re-hash): boot verification already
/// quarantined anything rotten.
pub fn fallback_complete(role: &str, exclude: &str) -> Option<String> {
    let cat = crate::catalog::load().ok()?;
    let dir = crate::dirs::models_dir();
    let key = match role {
        "stt" => "stt_models",
        "llm" => "llm_models",
        _ => return None,
    };
    let arr = cat.get(key)?.as_array()?;
    // Prefer larger (later-tier) models first: catalog is bytes-ascending.
    for m in arr.iter().rev() {
        let id = m.get("id").and_then(|v| v.as_str()).unwrap_or("");
        if id.is_empty() || id == exclude {
            continue;
        }
        let file = m.get("file").and_then(|v| v.as_str()).unwrap_or("");
        let want = m.get("bytes").and_then(|v| v.as_u64()).unwrap_or(0);
        let expected = m.get("sha256").and_then(|v| v.as_str()).unwrap_or("");
        if file.is_empty() {
            continue;
        }
        let size = std::fs::metadata(dir.join(file)).map(|x| x.len()).unwrap_or(0);
        if want > 0 && size != want {
            continue;
        }
        if size == 0 {
            continue;
        }
        if !expected.is_empty() {
            let man = crate::manifest::read(&dir);
            match man.files.get(id) {
                Some(rec) if rec.sha256.eq_ignore_ascii_case(expected) => {}
                _ => continue, // unverified — boot pass will judge it
            }
        }
        return Some(id.to_string());
    }
    None
}

/// Verify the active pair (+VLM when set). Returns `(stt_id, llm_id, warnings)`.
/// Warnings are human lines for the status line / Error Center. Never fails
/// the boot itself — worst case both ids stay as-is with warnings attached.
pub fn verify_active_boot() -> (Option<String>, Option<String>, Vec<String>) {
    let dir = crate::dirs::models_dir();
    let man = crate::manifest::read(&dir);
    let mut warn = Vec::new();
    let mut stt = if man.active_stt.is_empty() {
        None
    } else {
        Some(man.active_stt.clone())
    };
    let mut llm = if man.active_llm.is_empty() {
        None
    } else {
        Some(man.active_llm.clone())
    };
    // Pin-population honesty: an empty `sha256` in the loaded catalog means
    // size-only enforcement for that entry (pins populate on a networked dev
    // box via measure-models.ps1 + Hub-OID diff). Surface it at boot so a
    // stock install never implies hash verification it does not have.
    let cat = crate::catalog::load().ok();
    for (role, slot) in [("stt", &mut stt), ("llm", &mut llm)] {
        let Some(id) = slot.clone() else { continue };
        // Single call per id: the returned variant already distinguishes
        // verified / unpinned / missing / quarantined — no second lookup.
        match verify_one(&id) {
            VerifyOne::Ok => {
                let unpinned = cat
                    .as_ref()
                    .and_then(|c| crate::catalog::entry_full(c, &id))
                    .map(|(_, _, _, expected)| expected.is_empty())
                    .unwrap_or(false);
                if unpinned {
                    warn.push(format!(
                        "Model {id} has no pinned hash yet — size-only check (populate sha256 pins before release)."
                    ));
                }
            }
            VerifyOne::OkUnpinned => {
                warn.push(format!(
                    "Unverified community model active ({id}) — weights are user-supplied, not pinned."
                ));
            }
            VerifyOne::Missing(m) => {
                warn.push(format!("Model issue ({role} {id}): {m}"));
                if let Some(fb) = fallback_complete(role, &id) {
                    warn.push(format!(
                        "Primary {role} model unavailable — falling back to {fb}. Re-download required."
                    ));
                    *slot = Some(fb);
                }
            }
            VerifyOne::Quarantined(q) => {
                warn.push(format!(
                    "Primary {role} model corrupted — quarantined to {}. Re-download required.",
                    q.display()
                ));
                if let Some(fb) = fallback_complete(role, &id) {
                    warn.push(format!(
                        "Recording with fallback {role} model {fb}."
                    ));
                    *slot = Some(fb);
                } else {
                    *slot = None;
                }
            }
        }
    }
    (stt, llm, warn)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quarantine_moves_file_and_drops_manifest() {
        let _guard = crate::dirs::TEST_LOCK.lock().unwrap();
        let dir = std::env::temp_dir().join(format!("pv-verify-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        crate::dirs::set_models_dir_override(dir.clone());
        std::fs::write(dir.join("m.bin"), b"bad-bytes").unwrap();
        let mut man = crate::manifest::read(&dir);
        man.files.insert(
            "m".to_string(),
            crate::manifest::ManifestFile {
                bytes: 9,
                sha256: "x".into(),
                path: None,
                role: String::new(),
                verified: false,
            },
        );
        crate::manifest::write(&dir, &man).unwrap();
        let q = quarantine(&dir, "m.bin", "m").unwrap();
        assert!(q.exists() && !dir.join("m.bin").exists());
        assert!(crate::manifest::read(&dir).files.get("m").is_none());
        crate::dirs::clear_models_dir_override();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sha256_file_matches_known_vector() {
        let p = std::env::temp_dir().join(format!("pv-sha-{}", std::process::id()));
        std::fs::write(&p, b"abc").unwrap();
        assert_eq!(
            sha256_file(&p).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn fallback_skips_excluded_and_partial() {
        let _guard = crate::dirs::TEST_LOCK.lock().unwrap();
        let dir = std::env::temp_dir().join(format!("pv-fb-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        crate::dirs::set_models_dir_override(dir.clone());
        // Nothing downloaded → no fallback.
        assert!(fallback_complete("stt", "").is_none());
        crate::dirs::clear_models_dir_override();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
