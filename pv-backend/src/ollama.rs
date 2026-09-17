//! Ollama library scanner: link GGUF weight blobs in place.
//!
//! Ollama keeps models as content-addressed blobs
//! (`<store>/blobs/sha256-<hex>`) described by JSON manifests
//! (`<store>/manifests/<registry>/<namespace>/<model>/<tag>`).
//! We only ever READ: resolve a model's weight blob and record its absolute
//! path. No copies, no downloads, no writes into the Ollama store.

use std::path::{Path, PathBuf};

/// Weight layer media type in Ollama manifests.
const MODEL_LAYER: &str = "application/vnd.ollama.image.model";

#[derive(Clone, Debug)]
pub struct OllamaModel {
    /// Display name: `model:tag`, or `namespace/model:tag` outside library.
    pub name: String,
    /// Absolute blob path (the file llama.cpp will load).
    pub blob: PathBuf,
    /// Expected byte size from the manifest (0 if unstated).
    pub bytes: u64,
    /// Blob digest hex (no `sha256:` prefix).
    pub digest: String,
}

/// Ollama models root: `$OLLAMA_MODELS`, else `%USERPROFILE%/.ollama/models`.
pub fn store_root() -> PathBuf {
    if let Some(v) = std::env::var_os("OLLAMA_MODELS") {
        if !v.is_empty() {
            return PathBuf::from(v);
        }
    }
    let home = std::env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .unwrap_or_default();
    home.join(".ollama").join("models")
}

/// Display name from manifest path components.
pub fn display_name(namespace: &str, model: &str, tag: &str) -> String {
    if namespace == "library" || namespace.is_empty() {
        format!("{model}:{tag}")
    } else {
        format!("{namespace}/{model}:{tag}")
    }
}

/// Parse one manifest document: `(weight digest hex, byte size)` of the first
/// `image.model` layer. Pure — unit-tested with fixture JSON.
pub fn parse_manifest(text: &str) -> Option<(String, u64)> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    let layers = v.get("layers")?.as_array()?;
    for layer in layers {
        if layer.get("mediaType")?.as_str()? != MODEL_LAYER {
            continue;
        }
        let digest = layer.get("digest")?.as_str()?;
        let hex = digest.strip_prefix("sha256:")?.to_string();
        let size = layer.get("size")?.as_u64().unwrap_or(0);
        if hex.chars().all(|c| c.is_ascii_hexdigit()) && !hex.is_empty() {
            return Some((hex, size));
        }
    }
    None
}

/// True when the file starts with the GGUF magic (`GGUF`). Reads 4 bytes.
pub fn gguf_magic_ok(path: &Path) -> bool {
    use std::io::Read;
    match std::fs::File::open(path) {
        Ok(mut f) => {
            let mut magic = [0u8; 4];
            matches!(f.read_exact(&mut magic), Ok(())) && &magic == b"GGUF"
        }
        Err(_) => false,
    }
}

/// Bounded manifest walk: symlinks never followed (a looped link in the
/// store would otherwise spin forever), depth capped at 8 (registries nest
/// ~4 deep; anything deeper is not a manifest layout).
fn walk_files(dir: &Path, out: &mut Vec<PathBuf>) {
    walk_files_depth(dir, out, 0);
}

fn walk_files_depth(dir: &Path, out: &mut Vec<PathBuf>, depth: usize) {
    if depth > 8 || out.len() > 4096 {
        return;
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let p = entry.path();
        // symlink_metadata: never follow links (cycle-proof).
        let Ok(meta) = std::fs::symlink_metadata(&p) else {
            continue;
        };
        if meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_dir() {
            walk_files_depth(&p, out, depth + 1);
        } else if meta.is_file() {
            out.push(p);
        }
    }
}

/// Scan one store root. Layout: `<root>/manifests/<registry>/<ns>/<model>/<tag>`
/// plus `<root>/blobs/sha256-<hex>`. Entries whose blob is absent are skipped
/// (listed nowhere — nothing to link).
pub fn scan_root(root: &Path) -> Vec<OllamaModel> {
    let mut found = Vec::new();
    let manifests = root.join("manifests");
    let blobs = root.join("blobs");
    if !manifests.is_dir() || !blobs.is_dir() {
        return found;
    }
    let mut files = Vec::new();
    walk_files(&manifests, &mut files);
    for mf in files {
        let rel = match mf.strip_prefix(&manifests) {
            Ok(r) => r,
            Err(_) => continue,
        };
        let comps: Vec<String> = rel
            .components()
            .filter_map(|c| c.as_os_str().to_str().map(|s| s.to_string()))
            .collect();
        // [..., registry-host, namespace, model, tag]
        if comps.len() < 4 {
            continue;
        }
        let n = comps.len();
        let (namespace, model, tag) = (
            comps[n - 3].clone(),
            comps[n - 2].clone(),
            comps[n - 1].clone(),
        );
        let text = match std::fs::read_to_string(&mf) {
            Ok(t) => t,
            Err(_) => continue,
        };
        let (digest, bytes) = match parse_manifest(&text) {
            Some(d) => d,
            None => continue,
        };
        let blob = blobs.join(format!("sha256-{digest}"));
        if !blob.is_file() {
            continue;
        }
        found.push(OllamaModel {
            name: display_name(&namespace, &model, &tag),
            blob,
            bytes,
            digest,
        });
    }
    found.sort_by(|a, b| a.name.cmp(&b.name));
    found
}

/// Scan the local Ollama library.
pub fn scan() -> Vec<OllamaModel> {
    scan_root(&store_root())
}

/// Hash one linked blob against its recorded manifest digest (blocking: call
/// from a worker thread, never the UI thread). On a match the manifest
/// record is flagged verified; on any failure the link is dropped (manifest
/// record removed, active slots cleared) so a tampered or rotated blob can
/// never stay linked. Always emits exactly one `Event::LinkVerified`.
pub fn verify_link_blocking(id: &str, tx: std::sync::mpsc::Sender<crate::progress::Event>) {
    use crate::progress::Event;
    let done = |ok: bool, message: String| {
        let _ = tx.send(Event::LinkVerified {
            id: id.to_string(),
            ok,
            message,
        });
    };
    let dir = crate::dirs::models_dir();
    let mut man = crate::manifest::read(&dir);
    let (path, expected) = match man.files.get(id) {
        Some(rec) => match (&rec.path, &rec.sha256) {
            (Some(p), e) => (p.clone(), e.clone()),
            _ => {
                return done(false, format!("{id} is not a linked model"));
            }
        },
        None => return done(false, format!("{id} is not linked anymore")),
    };
    if expected.is_empty() {
        // Pre-hash-era manifest record: nothing to compare against.
        // Keep the link (magic+size gates still hold) but say so honestly.
        return done(true, format!("{id}: no digest recorded — magic+size only"));
    }
    match crate::verify::sha256_file(std::path::Path::new(&path)) {
        Ok(hex) if hex.eq_ignore_ascii_case(&expected) => {
            crate::manifest::set_link_verified(&mut man, id, true);
            let _ = crate::manifest::write(&dir, &man);
            done(true, format!("{id}: blob hash verified"))
        }
        Ok(_) => {
            crate::manifest::remove(&mut man, id);
            let _ = crate::manifest::write(&dir, &man);
            done(
                false,
                format!("{id}: blob hash mismatch — link dropped, re-link to retry"),
            )
        }
        Err(e) => {
            crate::manifest::remove(&mut man, id);
            let _ = crate::manifest::write(&dir, &man);
            done(false, format!("{id}: cannot read blob ({e}) — link dropped"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"{"schemaVersion":2,"mediaType":"application/vnd.docker.distribution.manifest.v2+json","config":{"mediaType":"application/vnd.docker.container.image.v1+json","digest":"sha256:aaaa","size":156},"layers":[{"mediaType":"application/vnd.ollama.image.model","digest":"sha256:5fd0c53919a1fd064b60f3ea40a2c1a676baef2b7d1de9ac3178b17c9b351109","size":11815759456}]}"#;

    #[test]
    fn parse_manifest_extracts_weight_layer() {
        let (digest, bytes) = parse_manifest(FIXTURE).unwrap();
        assert_eq!(
            digest,
            "5fd0c53919a1fd064b60f3ea40a2c1a676baef2b7d1de9ac3178b17c9b351109"
        );
        assert_eq!(bytes, 11815759456);
    }

    #[test]
    fn parse_manifest_rejects_junk() {
        assert!(parse_manifest("{}").is_none());
        assert!(parse_manifest("not json").is_none());
        assert!(
            parse_manifest(r#"{"layers":[{"mediaType":"x","digest":"sha256:zz","size":1}]}"#)
                .is_none()
        );
    }

    #[test]
    fn display_names() {
        assert_eq!(display_name("library", "m", "t"), "m:t");
        assert_eq!(display_name("ns", "m", "t"), "ns/m:t");
    }

    #[test]
    fn gguf_magic_checks_four_bytes() {
        let dir = std::env::temp_dir().join(format!("pv-ollama-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let good = dir.join("good.bin");
        let bad = dir.join("bad.bin");
        std::fs::write(&good, b"GGUF\x03\0\0\0rest").unwrap();
        std::fs::write(&bad, b"NOPErest").unwrap();
        assert!(gguf_magic_ok(&good));
        assert!(!gguf_magic_ok(&bad));
        assert!(!gguf_magic_ok(&dir.join("missing.bin")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn verify_link_hash_match_flags_mismatch_drops() {
        use crate::progress::Event;
        use sha2::Digest;
        let _guard = crate::dirs::TEST_LOCK.lock().unwrap();
        let dir = std::env::temp_dir().join(format!("pv-linkvrf-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        crate::dirs::set_models_dir_override(dir.clone());

        let blob = dir.join("blob.gguf");
        let content = b"GGUF-fake-weights!";
        std::fs::write(&blob, content).unwrap();
        let hex = format!("{:x}", sha2::Sha256::digest(content));
        let mut man = crate::manifest::read(&dir);
        crate::manifest::record_link(
            &mut man,
            "ollama:m",
            content.len() as u64,
            hex,
            blob.to_string_lossy().into_owned(),
            "llm",
        );
        crate::manifest::write(&dir, &man).unwrap();
        assert!(!crate::manifest::read(&dir).files["ollama:m"].verified);

        // Match → verified flag set, ok event.
        let (tx, rx) = std::sync::mpsc::channel();
        verify_link_blocking("ollama:m", tx);
        match rx.recv().unwrap() {
            Event::LinkVerified { id, ok, .. } => {
                assert_eq!(id, "ollama:m");
                assert!(ok);
            }
            _ => panic!("expected LinkVerified"),
        }
        assert!(crate::manifest::read(&dir).files["ollama:m"].verified);

        // Tampered blob → link dropped, fail event.
        std::fs::write(&blob, b"GGUF-TAMPERED-!!!!").unwrap();
        let (tx2, rx2) = std::sync::mpsc::channel();
        verify_link_blocking("ollama:m", tx2);
        match rx2.recv().unwrap() {
            Event::LinkVerified { id, ok, message } => {
                assert_eq!(id, "ollama:m");
                assert!(!ok);
                assert!(message.contains("mismatch"));
            }
            _ => panic!("expected LinkVerified"),
        }
        assert!(crate::manifest::read(&dir).files.get("ollama:m").is_none());

        // Unknown id → fail event, no panic.
        let (tx3, rx3) = std::sync::mpsc::channel();
        verify_link_blocking("ollama:ghost", tx3);
        match rx3.recv().unwrap() {
            Event::LinkVerified { ok, .. } => assert!(!ok),
            _ => panic!("expected LinkVerified"),
        }

        crate::dirs::clear_models_dir_override();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scan_root_maps_manifests_to_blobs() {
        let root = std::env::temp_dir().join(format!("pv-olscan-{}", std::process::id()));
        let mfdir = root
            .join("manifests")
            .join("registry.ollama.ai")
            .join("library")
            .join("m");
        let blobs = root.join("blobs");
        std::fs::create_dir_all(&mfdir).unwrap();
        std::fs::create_dir_all(&blobs).unwrap();
        std::fs::write(mfdir.join("t"), FIXTURE).unwrap();
        // Blob missing -> skipped.
        assert!(scan_root(&root).is_empty());
        // Blob present but not GGUF -> listed (magic is checked at link time).
        let digest = "5fd0c53919a1fd064b60f3ea40a2c1a676baef2b7d1de9ac3178b17c9b351109";
        std::fs::write(blobs.join(format!("sha256-{digest}")), b"GGUFdata").unwrap();
        let found = scan_root(&root);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "m:t");
        assert_eq!(found[0].bytes, 11815759456);
        let _ = std::fs::remove_dir_all(&root);
    }
}
