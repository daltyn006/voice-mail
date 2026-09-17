//! Model inventory: which catalog entries are fully downloaded, partially
//! present, or missing, and which pair is active.

use serde::Serialize;

#[derive(Serialize, Clone, Debug)]
pub struct ModelStatus {
    pub id: String,
    pub role: String,
    pub file: String,
    pub bytes: u64,
    pub tier: String,
    pub note: String,
    pub present: bool,
    pub size: u64,
    pub complete: bool,
    pub active: bool,
    /// Pinned SHA-256 hex from the catalog (empty = dev-sidecar, unpinned).
    #[serde(default)]
    pub expected_sha: String,
    /// True when the catalog is a dev-sidecar override or the entry carries
    /// no pin: activation requires explicit user confirm upstream.
    #[serde(default)]
    pub unverified: bool,
}

#[derive(Serialize, Clone, Debug, Default)]
pub struct ModelsSnapshot {
    pub models: Vec<ModelStatus>,
    pub models_dir: String,
    pub active_stt: String,
    pub active_llm: String,
    pub active_vlm: String,
}

/// Scan the models dir against the catalog. Creates the dir on demand.
pub fn status() -> Result<ModelsSnapshot, String> {
    let cat = crate::catalog::load()?;
    let dir = crate::dirs::models_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let man = crate::manifest::read(&dir);
    let mut models = Vec::new();
    for (role_key, role) in [("stt_models", "stt"), ("llm_models", "llm")] {
        if let Some(arr) = cat.get(role_key).and_then(|v| v.as_array()) {
            for m in arr {
                let id = m
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let file = m
                    .get("file")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let bytes = m.get("bytes").and_then(|v| v.as_u64()).unwrap_or(0);
                let tier = m
                    .get("tier")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let note = m
                    .get("note")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let size = std::fs::metadata(dir.join(&file))
                    .map(|x| x.len())
                    .unwrap_or(0);
                let complete = bytes > 0 && size == bytes;
                let active = (role == "stt" && man.active_stt == id)
                    || (role == "llm" && man.active_llm == id);
                let expected = m
                    .get("sha256")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                // Honest badging: an entry without a pin is size-only checked
                // no matter which catalog supplied it (stock catalog before
                // pins populate, or a dev-sidecar override). Either condition
                // needs explicit user confirmation before activation.
                let sidecar = crate::catalog::is_sidecar();
                let unpinned = expected.is_empty();
                let unverified = sidecar || unpinned;
                let mut tags = Vec::new();
                if unpinned {
                    tags.push("[Unpinned — size-only check; populate sha256 pins before release]");
                }
                if sidecar {
                    tags.push("[Unverified community model — confirm before activation]");
                }
                let note = if tags.is_empty() {
                    note
                } else if note.is_empty() {
                    tags.join(" ")
                } else {
                    format!("{note} {}", tags.join(" "))
                };
                models.push(ModelStatus {
                    id,
                    role: role.to_string(),
                    file,
                    bytes,
                    tier,
                    note,
                    present: size > 0,
                    size,
                    complete,
                    active,
                    expected_sha: expected,
                    unverified,
                });
            }
        }
    }
    // Linked external files (e.g. Ollama blobs): not in the catalog, used in
    // place. A link is complete while its source exists and reads as GGUF.
    // Role comes from link time ("stt"/"llm"/"vlm"); pre-role manifests read
    // as llm.
    for (id, rec) in man.files.iter().filter(|(_, f)| f.path.is_some()) {
        let path = rec.path.clone().unwrap_or_default();
        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        let complete = size > 0 && crate::ollama::gguf_magic_ok(std::path::Path::new(&path));
        let shown = std::path::Path::new(&path)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(&path)
            .to_string();
        let role = if rec.role == "stt" {
            "stt"
        } else if rec.role == "vlm" {
            "vlm"
        } else {
            "llm"
        };
        // Hash state of the linked blob: verified at link time by the
        // background worker, unverified while it hashes (or for pre-worker
        // manifest records, which carry no proof either way).
        let link_note = if rec.verified {
            format!("Linked in place — no copy. Loaded straight from {path} [Hash verified]")
        } else {
            format!("Linked in place — no copy. Loaded straight from {path} [Hash unverified — magic+size only]")
        };
        models.push(ModelStatus {
            id: id.clone(),
            role: role.to_string(),
            file: shown,
            bytes: rec.bytes,
            tier: "linked".to_string(),
            note: link_note,
            present: size > 0,
            size,
            complete,
            active: (role == "stt" && man.active_stt == *id)
                || (role == "llm" && man.active_llm == *id)
                || (role == "vlm" && man.active_vlm == *id),
            expected_sha: String::new(),
            unverified: false,
        });
    }
    // Vision pairs (text GGUF + mmproj): complete only when BOTH files verify.
    if let Some(arr) = cat.get("vision_models").and_then(|v| v.as_array()) {
        for m in arr {
            let id = m
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let entry = match crate::catalog::vision_entry(&cat, &id) {
                Some(e) => e,
                None => continue,
            };
            let tsize = std::fs::metadata(dir.join(&entry.text_file))
                .map(|x| x.len())
                .unwrap_or(0);
            let msize = std::fs::metadata(dir.join(&entry.mmproj_file))
                .map(|x| x.len())
                .unwrap_or(0);
            let complete = entry.text_bytes > 0
                && tsize == entry.text_bytes
                && entry.mmproj_bytes > 0
                && msize == entry.mmproj_bytes;
            let v_unpinned = entry.text_sha256.is_empty() || entry.mmproj_sha256.is_empty();
            let v_sidecar = crate::catalog::is_sidecar();
            let mut v_tags = Vec::new();
            if v_unpinned {
                v_tags.push("[Unpinned — size-only check; populate sha256 pins before release]");
            }
            if v_sidecar {
                v_tags.push("[Unverified community model — confirm before activation]");
            }
            let v_note = if v_tags.is_empty() {
                entry.note.clone()
            } else if entry.note.is_empty() {
                v_tags.join(" ")
            } else {
                format!("{} {}", entry.note, v_tags.join(" "))
            };
            models.push(ModelStatus {
                id: id.clone(),
                role: "vlm".to_string(),
                file: format!("{} + {}", entry.text_file, entry.mmproj_file),
                bytes: entry.text_bytes + entry.mmproj_bytes,
                tier: entry.tier.clone(),
                note: v_note,
                present: tsize > 0 || msize > 0,
                size: tsize + msize,
                complete,
                active: man.active_vlm == id,
                expected_sha: format!("{}+{}", entry.text_sha256, entry.mmproj_sha256),
                unverified: v_unpinned || v_sidecar,
            });
        }
    }
    Ok(ModelsSnapshot {
        models,
        models_dir: dir.to_string_lossy().into_owned(),
        active_stt: man.active_stt,
        active_llm: man.active_llm,
        active_vlm: man.active_vlm,
    })
}

/// Persist the active STT/LLM pair.
pub fn set_active(stt_id: &str, llm_id: &str) -> Result<(), String> {
    let dir = crate::dirs::models_dir();
    let mut man = crate::manifest::read(&dir);
    man.active_stt = stt_id.to_string();
    man.active_llm = llm_id.to_string();
    crate::manifest::write(&dir, &man)
}

/// Persist the active vision pair (text GGUF + mmproj).
pub fn set_active_vlm(vlm_id: &str) -> Result<(), String> {
    let dir = crate::dirs::models_dir();
    let mut man = crate::manifest::read(&dir);
    man.active_vlm = vlm_id.to_string();
    crate::manifest::write(&dir, &man)
}

/// Absolute `(stt_path, llm_path)` for the active pair. Errors name exactly
/// what is missing so the UI can route to Models instead of failing blind.
pub fn active_model_paths() -> Result<(std::path::PathBuf, std::path::PathBuf), String> {
    let cat = crate::catalog::load()?;
    let dir = crate::dirs::models_dir();
    let man = crate::manifest::read(&dir);
    if man.active_stt.is_empty() || man.active_llm.is_empty() {
        return Err("no active models — download a pair first (Models page)".to_string());
    }
    let sp = resolve_one(&cat, &dir, &man.active_stt)?;
    let lp = resolve_one(&cat, &dir, &man.active_llm)?;
    Ok((sp, lp))
}

/// Absolute `(text_path, mmproj_path)` for the active vision pair. Linked
/// VLM blobs serve as the text half; the projector still comes from a
/// downloaded catalog mmproj (same tier family). Errors name exactly what
/// is missing so the UI can route to Models instead of failing blind.
pub fn active_vlm_paths() -> Result<(std::path::PathBuf, std::path::PathBuf), String> {
    let cat = crate::catalog::load()?;
    let dir = crate::dirs::models_dir();
    let man = crate::manifest::read(&dir);
    if man.active_vlm.is_empty() {
        return Err("no active vision model — download one first (Models page → Vision)".to_string());
    }
    // Linked blob = text half used in place.
    if let Some(rec) = man.files.get(&man.active_vlm).and_then(|f| f.path.clone()) {
        match std::fs::metadata(&rec) {
            Ok(_) => {
                let mm = vision_mmproj_for(&cat, &dir, &man.active_vlm)?;
                return Ok((std::path::PathBuf::from(rec), mm));
            }
            Err(_) => {
                return Err(format!(
                    "linked vision model gone — its file was moved or deleted. Re-link it (Models page): {}",
                    man.active_vlm
                ))
            }
        }
    }
    let entry = crate::catalog::vision_entry(&cat, &man.active_vlm)
        .ok_or_else(|| format!("unknown active vision model: {}", man.active_vlm))?;
    let tp = dir.join(&entry.text_file);
    let mp = dir.join(&entry.mmproj_file);
    match (std::fs::metadata(&tp), std::fs::metadata(&mp)) {
        (Ok(t), Ok(m)) if t.len() == entry.text_bytes && m.len() == entry.mmproj_bytes => {
            Ok((tp, mp))
        }
        _ => Err(format!(
            "vision files missing or partial: {} + {}",
            tp.display(),
            mp.display()
        )),
    }
}

/// Projector for a linked VLM text blob: the tier family mmproj must be
/// downloaded (projectors are tiny vs weights and shared per family).
fn vision_mmproj_for(
    cat: &serde_json::Value,
    dir: &std::path::Path,
    vlm_id: &str,
) -> Result<std::path::PathBuf, String> {
    // Prefer the mmproj of the same catalog entry when its files exist...
    if let Some(e) = crate::catalog::vision_entry(cat, vlm_id) {
        let mp = dir.join(&e.mmproj_file);
        if let Ok(m) = std::fs::metadata(&mp) {
            if m.len() == e.mmproj_bytes {
                return Ok(mp);
            }
        }
    }
    // ...else a complete mmproj with the SAME filename (e.g. the 7B Q4 and
    // Q8 tiers share one projector file). Cross-family projectors silently
    // mis-caption, so a different filename never qualifies.
    let want_file = crate::catalog::vision_entry(cat, vlm_id).map(|e| e.mmproj_file);
    if let Some(arr) = cat.get("vision_models").and_then(|v| v.as_array()) {
        for m in arr {
            let id = m.get("id").and_then(|v| v.as_str()).unwrap_or("");
            if let Some(e) = crate::catalog::vision_entry(cat, id) {
                if Some(e.mmproj_file.as_str()) != want_file.as_deref() {
                    continue;
                }
                let mp = dir.join(&e.mmproj_file);
                if let Ok(md) = std::fs::metadata(&mp) {
                    if md.len() == e.mmproj_bytes {
                        return Ok(mp);
                    }
                }
            }
        }
    }
    Err("no vision projector downloaded — download any Vision model first (Models page → Vision)".to_string())
}

/// Resolve one active id to a loadable path: linked entries use their
/// recorded absolute path (and must still exist); catalog entries resolve
/// under the models dir with size verification.
fn resolve_one(
    cat: &serde_json::Value,
    dir: &std::path::Path,
    id: &str,
) -> Result<std::path::PathBuf, String> {
    let man = crate::manifest::read(dir);
    if let Some(rec) = man.files.get(id).and_then(|f| f.path.clone()) {
        match std::fs::metadata(&rec) {
            Ok(_) => return Ok(std::path::PathBuf::from(rec)),
            Err(_) => {
                return Err(format!(
                "linked model gone — its file was moved or deleted. Re-link it (Models page): {id}"
            ))
            }
        }
    }
    let (_, file, want) =
        crate::catalog::entry(cat, id).ok_or_else(|| format!("unknown active model: {id}"))?;
    let p = dir.join(&file);
    match std::fs::metadata(&p) {
        Ok(m) if want == 0 || m.len() == want => Ok(p),
        _ => Err(format!("model file missing or partial: {}", p.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_lists_catalog_without_models_present() {
        let snap = status().unwrap();
        assert!(!snap.models.is_empty());
        assert!(snap.models.iter().all(|m| !m.complete || m.present));
    }

    #[test]
    fn set_active_roundtrips_through_manifest() {
        // NEVER touch the real models dir: the override is process-global,
        // so serialize with the shared lock and point it at a temp dir.
        let _guard = crate::dirs::TEST_LOCK.lock().unwrap();
        let dir =
            std::env::temp_dir().join(format!("pv-active-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        crate::dirs::set_models_dir_override(dir.clone());
        set_active("s", "l").unwrap();
        let snap = status().unwrap();
        assert_eq!(snap.active_stt, "s");
        assert_eq!(snap.active_llm, "l");
        set_active("", "").unwrap();
        crate::dirs::clear_models_dir_override();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
