//! Download manifest: which model files are verified present, and which pair
//! is active. Lives at `<models_dir>/manifest.json`.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Deserialize, Serialize, Default, Clone, Debug)]
pub struct ManifestFile {
    #[serde(default)]
    pub bytes: u64,
    #[serde(default)]
    pub sha256: String,
    /// Absolute path of a LINKED external file (e.g. an Ollama blob).
    /// `None` means a managed file living under the models dir. Added later,
    /// so old manifests without it keep working.
    #[serde(default)]
    pub path: Option<String>,
    /// Role this link serves: "stt" or "llm". Chosen at link time on the
    /// Models page; empty in manifests written before roles existed.
    #[serde(default)]
    pub role: String,
}

#[derive(Deserialize, Serialize, Default, Clone, Debug)]
pub struct Manifest {
    #[serde(default)]
    pub files: HashMap<String, ManifestFile>,
    #[serde(default)]
    pub active_stt: String,
    #[serde(default)]
    pub active_llm: String,
    /// Active vision id (text GGUF + mmproj pair). Empty = none yet.
    /// Added later, so old manifests without it keep working.
    #[serde(default)]
    pub active_vlm: String,
}

/// Manifest path for a given models dir (pure function — testable).
pub fn manifest_path(models_dir: &Path) -> PathBuf {
    models_dir.join("manifest.json")
}

/// Read the manifest; missing/corrupt files yield an empty default.
pub fn read(models_dir: &Path) -> Manifest {
    std::fs::read_to_string(manifest_path(models_dir))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// Write the manifest, creating the models dir on demand.
pub fn write(models_dir: &Path, m: &Manifest) -> Result<(), String> {
    std::fs::create_dir_all(models_dir).map_err(|e| e.to_string())?;
    let t = serde_json::to_string_pretty(m).map_err(|e| e.to_string())?;
    std::fs::write(manifest_path(models_dir), t).map_err(|e| e.to_string())
}

/// Record a completed download; first completed model per role becomes active.
pub fn record_complete(man: &mut Manifest, id: &str, role: &str, bytes: u64, sha256: String) {
    man.files.insert(
        id.to_string(),
        ManifestFile {
            bytes,
            sha256,
            path: None,
            role: String::new(),
        },
    );
    if role == "stt" && man.active_stt.is_empty() {
        man.active_stt = id.to_string();
    } else if role == "llm" && man.active_llm.is_empty() {
        man.active_llm = id.to_string();
    } else if role == "vlm" && man.active_vlm.is_empty() {
        man.active_vlm = id.to_string();
    }
}

/// Record a LINKED external file (used in place, never copied).
/// First linked model per empty slot becomes active for that slot — and ONLY
/// that slot: a linked STT model must never occupy the LLM slot (or vice
/// versa) just because it happens to be empty.
pub fn record_link(
    man: &mut Manifest,
    id: &str,
    bytes: u64,
    sha256: String,
    path: String,
    role: &str,
) {
    man.files.insert(
        id.to_string(),
        ManifestFile {
            bytes,
            sha256,
            path: Some(path),
            role: role.to_string(),
        },
    );
    match role {
        "stt" if man.active_stt.is_empty() => man.active_stt = id.to_string(),
        "llm" if man.active_llm.is_empty() => man.active_llm = id.to_string(),
        "vlm" if man.active_vlm.is_empty() => man.active_vlm = id.to_string(),
        _ => {}
    }
}

/// Drop one manifest record (unlink). Returns true when something was removed.
pub fn remove(man: &mut Manifest, id: &str) -> bool {
    let gone = man.files.remove(id).is_some();
    if man.active_stt == id {
        man.active_stt.clear();
    }
    if man.active_llm == id {
        man.active_llm.clear();
    }
    if man.active_vlm == id {
        man.active_vlm.clear();
    }
    gone
}

/// Drop every record whose linked file no longer exists.
/// Returns the removed ids. Managed (copied) entries are never touched.
pub fn prune_missing_links(man: &mut Manifest) -> Vec<String> {
    let dead: Vec<String> = man
        .files
        .iter()
        .filter(|(_, f)| match &f.path {
            Some(p) => std::fs::metadata(p).is_err(),
            None => false,
        })
        .map(|(id, _)| id.clone())
        .collect();
    for id in &dead {
        remove(man, id);
    }
    dead
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("pv-manifest-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        p
    }

    #[test]
    fn record_link_keeps_role() {
        let dir = tmp("linkrole");
        let mut m = read(&dir);
        record_link(&mut m, "ollama:mx", 7, "h".into(), "C:/x.bin".into(), "stt");
        write(&dir, &m).unwrap();
        let back = read(&dir);
        assert_eq!(back.files["ollama:mx"].role, "stt");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn record_link_never_crosses_slots() {
        // Linking STT while its slot is full must NOT occupy the LLM slot.
        let dir = tmp("linkslots");
        let mut m = read(&dir);
        record_link(&mut m, "ollama:llm", 7, "h".into(), "C:/l.bin".into(), "llm");
        record_link(&mut m, "ollama:stt1", 7, "h".into(), "C:/s1.bin".into(), "stt");
        record_link(&mut m, "ollama:stt2", 7, "h".into(), "C:/s2.bin".into(), "stt");
        assert_eq!(m.active_stt, "ollama:stt1");
        assert_eq!(m.active_llm, "ollama:llm");
        assert!(m.active_vlm.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn roundtrip_and_first_active_wins() {
        let dir = tmp("roundtrip");
        let mut m = read(&dir);
        assert!(m.active_stt.is_empty());
        record_complete(&mut m, "a", "stt", 10, "h".into());
        record_complete(&mut m, "b", "stt", 11, "h".into());
        assert_eq!(m.active_stt, "a");
        write(&dir, &m).unwrap();
        let back = read(&dir);
        assert_eq!(back.active_stt, "a");
        assert_eq!(back.files["b"].bytes, 11);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_manifest_reads_empty() {
        let dir = tmp("corrupt");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(manifest_path(&dir), "{not json").unwrap();
        assert!(read(&dir).files.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
