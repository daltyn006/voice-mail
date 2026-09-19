//! Persistent review drafts: folder-per-output + `index.json` layout manifest.
//!
//! Locked plan: `<reviews>/index.json` declares the directory layout;
//! each output gets `<reviews>/<stem>/{raw.md,summary.md,merged.md,meta.json}`.
//! The AI (Store) creates the structure on `open_review`; the user only
//! chooses the base path in Settings → Storage. All writes are atomic
//! (tmp + rename) and confined under the drafts base.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct DraftMeta {
    pub id: u64,
    pub md_name: String,
    pub stem: String,
    pub merged: bool,
    pub dirty: bool,
    pub updated_at: u64,
    #[serde(default)]
    pub undo: Option<DraftUndo>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct DraftUndo {
    pub left: String,
    pub right: String,
    pub center: String,
    pub merged: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct DraftIndex {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub folders: HashMap<String, DraftFolderEntry>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct DraftFolderEntry {
    pub id: u64,
    pub md_name: String,
    pub updated_at: u64,
    #[serde(default)]
    pub dirty: bool,
    #[serde(default)]
    pub merged: bool,
}

#[derive(Clone, Debug, Default)]
pub struct DraftContent {
    pub left: String,
    pub right: String,
    pub center: String,
    pub meta: DraftMeta,
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Sanitize a folder stem (mirrors core `sanitize_filename` intent).
pub fn sanitize_stem(s: &str) -> String {
    let mut o = String::new();
    for c in s.chars() {
        if matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') {
            o.push('-');
        } else if (c as u32) < 0x20 {
            o.push('-');
        } else {
            o.push(c);
        }
    }
    let t = o.trim().trim_matches('.').trim();
    let mut t = t.to_string();
    while t.contains("--") {
        t = t.replace("--", "-");
    }
    if t.is_empty() {
        t = "Untitled".to_string();
    }
    if t.chars().count() > 80 {
        t = t.chars().take(80).collect();
    }
    t
}

fn confine_dir(base: &Path, stem: &str) -> Result<PathBuf, String> {
    if stem.is_empty() || stem == "." || stem == ".." {
        return Err("bad draft name".to_string());
    }
    let rel = Path::new(stem);
    if rel.is_absolute() || rel.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err(format!("path escapes drafts dir: {stem}"));
    }
    if stem.contains('/') || stem.contains('\\') {
        return Err(format!("nested draft paths not allowed: {stem}"));
    }
    Ok(base.join(stem))
}

pub fn drafts_base() -> PathBuf {
    crate::dirs::reviews_dir()
}

pub fn index_path(base: &Path) -> PathBuf {
    base.join("index.json")
}

pub fn read_index(base: &Path) -> DraftIndex {
    std::fs::read_to_string(index_path(base))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(DraftIndex { version: 1, folders: HashMap::new() })
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())?;
    Ok(())
}

fn write_index(base: &Path, idx: &DraftIndex) -> Result<(), String> {
    let t = serde_json::to_string_pretty(idx).map_err(|e| e.to_string())?;
    write_atomic(&index_path(base), t.as_bytes())
}

/// Ensure the base dir + `index.json` exist (AI-created structure).
pub fn ensure_base() -> Result<PathBuf, String> {
    let base = drafts_base();
    std::fs::create_dir_all(&base).map_err(|e| e.to_string())?;
    if !index_path(&base).exists() {
        write_index(&base, &DraftIndex { version: 1, folders: HashMap::new() })?;
    }
    Ok(base)
}

/// Open (or seed) a draft folder for `md_name`. Returns full content.
pub fn open_draft(
    id: u64,
    md_name: &str,
    seed_left: &str,
    seed_right: &str,
) -> Result<DraftContent, String> {
    let base = ensure_base()?;
    let stem = sanitize_stem(md_name.trim_end_matches(".md"));
    let dir = confine_dir(&base, &stem)?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let meta_path = dir.join("meta.json");
    if meta_path.exists() {
        if let Ok(content) = load_draft(&base, &stem) {
            return Ok(content);
        }
    }
    // seed fresh structure
    write_atomic(&dir.join("raw.md"), seed_left.as_bytes())?;
    write_atomic(&dir.join("summary.md"), seed_right.as_bytes())?;
    write_atomic(&dir.join("merged.md"), b"")?;
    let meta = DraftMeta {
        id,
        md_name: md_name.to_string(),
        stem: stem.clone(),
        merged: false,
        dirty: false,
        updated_at: now_secs(),
        undo: None,
    };
    write_atomic(
        &meta_path,
        serde_json::to_string_pretty(&meta).map_err(|e| e.to_string())?.as_bytes(),
    )?;
    let mut idx = read_index(&base);
    idx.version = 1;
    idx.folders.insert(
        stem.clone(),
        DraftFolderEntry {
            id,
            md_name: md_name.to_string(),
            updated_at: meta.updated_at,
            dirty: false,
            merged: false,
        },
    );
    write_index(&base, &idx)?;
    Ok(DraftContent {
        left: seed_left.to_string(),
        right: seed_right.to_string(),
        center: String::new(),
        meta,
    })
}

pub fn load_draft(base: &Path, stem: &str) -> Result<DraftContent, String> {
    let dir = confine_dir(base, stem)?;
    let left = std::fs::read_to_string(dir.join("raw.md")).map_err(|e| e.to_string())?;
    let right = std::fs::read_to_string(dir.join("summary.md")).map_err(|e| e.to_string())?;
    let center = std::fs::read_to_string(dir.join("merged.md")).unwrap_or_default();
    let meta: DraftMeta = std::fs::read_to_string(dir.join("meta.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(DraftMeta {
            id: 0,
            md_name: format!("{stem}.md"),
            stem: stem.to_string(),
            merged: false,
            dirty: false,
            updated_at: 0,
            undo: None,
        });
    Ok(DraftContent { left, right, center, meta })
}

/// Persist pane texts + flags (called debounced + on close/switch/quit).
pub fn save_draft(
    stem: &str,
    left: &str,
    right: &str,
    center: &str,
    merged: bool,
    dirty: bool,
    undo: Option<DraftUndo>,
) -> Result<(), String> {
    let base = ensure_base()?;
    let dir = confine_dir(&base, stem)?;
    if !dir.exists() {
        return Err("draft folder gone — reopen Review".to_string());
    }
    write_atomic(&dir.join("raw.md"), left.as_bytes())?;
    write_atomic(&dir.join("summary.md"), right.as_bytes())?;
    write_atomic(&dir.join("merged.md"), center.as_bytes())?;
    let mut meta: DraftMeta = std::fs::read_to_string(dir.join("meta.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    meta.merged = merged;
    meta.dirty = dirty;
    meta.updated_at = now_secs();
    meta.undo = undo;
    if meta.stem.is_empty() {
        meta.stem = stem.to_string();
    }
    write_atomic(
        &dir.join("meta.json"),
        serde_json::to_string_pretty(&meta).map_err(|e| e.to_string())?.as_bytes(),
    )?;
    let mut idx = read_index(&base);
    if let Some(e) = idx.folders.get_mut(stem) {
        e.updated_at = meta.updated_at;
        e.dirty = dirty;
        e.merged = merged;
    } else {
        idx.folders.insert(
            stem.to_string(),
            DraftFolderEntry {
                id: meta.id,
                md_name: meta.md_name.clone(),
                updated_at: meta.updated_at,
                dirty,
                merged,
            },
        );
    }
    write_index(&base, &idx)?;
    Ok(())
}

/// Delete a draft folder + index entry (Submit / file delete).
pub fn delete_draft(stem: &str) -> Result<(), String> {
    let base = drafts_base();
    let dir = confine_dir(&base, stem)?;
    if dir.exists() {
        std::fs::remove_dir_all(&dir).map_err(|e| e.to_string())?;
    }
    let mut idx = read_index(&base);
    idx.folders.remove(stem);
    let _ = write_index(&base, &idx);
    Ok(())
}

/// Rename a draft folder on output rename (best-effort; never loses text).
pub fn rename_draft(from_stem: &str, to_md_name: &str) -> Result<String, String> {
    let base = ensure_base()?;
    let to_stem = sanitize_stem(to_md_name.trim_end_matches(".md"));
    let from = confine_dir(&base, from_stem)?;
    let to = confine_dir(&base, &to_stem)?;
    if !from.exists() {
        return Ok(to_stem);
    }
    if to.exists() {
        return Err(format!("draft target exists: {to_md_name}"));
    }
    std::fs::rename(&from, &to).map_err(|e| e.to_string())?;
    // patch meta + index
    if let Ok(meta_text) = std::fs::read_to_string(to.join("meta.json")) {
        if let Ok(mut meta) = serde_json::from_str::<DraftMeta>(&meta_text) {
            meta.md_name = to_md_name.to_string();
            meta.stem = to_stem.clone();
            let _ = write_atomic(
                &to.join("meta.json"),
                serde_json::to_string_pretty(&meta).unwrap_or_default().as_bytes(),
            );
        }
    }
    let mut idx = read_index(&base);
    if let Some(e) = idx.folders.remove(from_stem) {
        let mut e = e;
        e.md_name = to_md_name.to_string();
        idx.folders.insert(to_stem.clone(), e);
        let _ = write_index(&base, &idx);
    }
    Ok(to_stem)
}

/// Sweep orphans: index entries matching neither live outputs nor disk.
/// Returns removed stems.
pub fn sweep_orphans(live_md_names: &[String]) -> Vec<String> {
    let base = drafts_base();
    let mut idx = read_index(&base);
    let live: std::collections::HashSet<&str> =
        live_md_names.iter().map(|s| s.as_str()).collect();
    let stems: Vec<String> = idx.folders.keys().cloned().collect();
    let mut removed = Vec::new();
    for stem in stems {
        if let Some(e) = idx.folders.get(&stem) {
            if !live.contains(e.md_name.as_str()) {
                let dir = confine_dir(&base, &stem);
                if let Ok(d) = dir {
                    let _ = std::fs::remove_dir_all(&d);
                }
                idx.folders.remove(&stem);
                removed.push(stem);
            }
        }
    }
    let _ = write_index(&base, &idx);
    removed
}

/// Move all drafts to a new base (Settings → Move existing drafts).
/// Copy-then-verify: sources removed only after each folder verifies.
pub fn move_all_drafts(to: &Path) -> Result<usize, String> {
    let from = drafts_base();
    if from == to {
        return Ok(0);
    }
    std::fs::create_dir_all(to).map_err(|e| e.to_string())?;
    let idx = read_index(&from);
    let mut moved = 0;
    for stem in idx.folders.keys() {
        let src = confine_dir(&from, stem)?;
        let dst = confine_dir(to, stem)?;
        if !src.exists() || dst.exists() {
            continue;
        }
        std::fs::create_dir_all(&dst).map_err(|e| e.to_string())?;
        // Copy-then-verify per file: the source dies only when every file
        // that existed there landed here (a missing meta.json alone must
        // never bless a partial move).
        let mut ok = true;
        for f in ["raw.md", "summary.md", "merged.md", "meta.json"] {
            let s = src.join(f);
            if s.exists() {
                if std::fs::copy(&s, dst.join(f)).is_err() || !dst.join(f).exists() {
                    ok = false;
                    break;
                }
            }
        }
        if ok {
            let _ = std::fs::remove_dir_all(&src);
            moved += 1;
        }
    }
    write_index(to, &idx).map_err(|e| e.to_string())?;
    Ok(moved)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_blocks_traversal() {
        assert_eq!(sanitize_stem("a/b"), "a-b");
        assert!(confine_dir(Path::new("C:/d"), "../x").is_err());
        assert!(confine_dir(Path::new("C:/d"), "a/b").is_err());
    }
}
