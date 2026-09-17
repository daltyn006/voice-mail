//! UI preferences: theme mode + custom models dir. Stored as tiny JSON at
//! `<data_dir>/ui.json`; every field has a safe default so a missing or
//! corrupt file simply yields defaults.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

fn default_true() -> bool {
    true
}

fn default_compute() -> String {
    "auto".to_string()
}

fn default_false() -> bool {
    false
}

fn default_empty() -> Vec<String> {
    Vec::new()
}

fn default_view() -> String {
    "large".to_string()
}

fn default_tier() -> String {
    "standard".to_string()
}

fn default_chunk_tokens() -> i32 {
    // Standard tier (Medium 4000-token chunks). Legacy `chunk_words` values
    // migrate ×4/3 on load (see load_with_migration).
    4000
}

fn default_summary_tier() -> String {
    "standard".to_string()
}

fn default_window() -> i32 {
    30
}

fn default_budget() -> i32 {
    80
}

fn default_suggest() -> f32 {
    0.35
}

fn default_prompt() -> f32 {
    0.80
}

fn default_merge_mode() -> String {
    "ask".to_string()
}

fn default_pdf_mode() -> String {
    "warn".to_string()
}

fn default_retention() -> String {
    "keep".to_string()
}

fn default_denoise() -> String {
    "recommended".to_string()
}

fn default_attention() -> i32 {
    50
}

fn default_doc_max() -> i32 {
    500_000
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct UiPrefs {
    /// Dark theme on (the shipped look).
    #[serde(default = "default_true")]
    pub dark_mode: bool,
    /// Pinned models directory; `None` means the default home.
    #[serde(default)]
    pub models_dir: Option<String>,
    /// Compute mode: "auto" (GPU first, CPU fallback) or "cpu".
    #[serde(default = "default_compute")]
    pub compute_mode: String,
    /// Delete converted WAV cache copies after a successful output.
    #[serde(default = "default_false")]
    pub delete_converted: bool,
    /// Output `md_name`s the user hid via ×. Persisted so Refresh (and
    /// restarts, which used to resurrect everything) never brings them back.
    #[serde(default = "default_empty")]
    pub dismissed: Vec<String>,
    /// Merge suggestion keys the user dismissed ("Keep separate"/"Dismiss").
    /// Persisted so re-clustering after restart never re-nags.
    #[serde(default = "default_empty")]
    pub dismissed_groups: Vec<String>,
    /// Explorer density: "large" | "list" | "details".
    #[serde(default = "default_view")]
    pub view: String,
    /// Staged Input paths (absolute) surviving restarts: re-resolved at boot
    /// (FIFO order kept; missing files dropped with a count note, never an
    /// error). Capped at 256 on write. Processing state is NOT persisted —
    /// restart means staged-and-queued, never mid-inference.
    #[serde(default = "default_empty")]
    pub staged: Vec<String>,
    /// Default output folder override; None = per-user default home.
    #[serde(default)]
    pub default_outdir: Option<String>,
    /// Default known-classes list (`;`-separated) used by Start.
    #[serde(default)]
    pub default_classes: String,
    /// Models-page tier filter selection.
    #[serde(default = "default_tier")]
    pub models_tier: String,
    /// Review drafts folder override; None = `<data>/reviews`.
    #[serde(default)]
    pub review_drafts_dir: Option<String>,
    /// Summarizer chunk size in AI tokens (Settings → Summary).
    /// Presets: Small 1000 / Medium 4000 / Large 8000; custom 200–12000.
    /// Renamed from `chunk_words` (legacy word counts migrate ×4/3).
    #[serde(default = "default_chunk_tokens")]
    pub chunk_tokens: i32,
    /// Summary length tier: "recap" (25%) | "standard" (50%) | "detailed" (75%).
    /// Drives chunk preset + tier guide + length ratio consistently.
    #[serde(default = "default_summary_tier")]
    pub summary_tier: String,
    #[serde(default = "default_window")]
    pub audio_window_sec: i32,
    #[serde(default = "default_budget")]
    pub vram_budget_pct: i32,
    /// Merge similarity thresholds (Settings → Merging).
    #[serde(default = "default_suggest")]
    pub merge_suggest: f32,
    #[serde(default = "default_prompt")]
    pub merge_prompt: f32,
    /// Merge default mode: "executive" | "long" | "ask".
    #[serde(default = "default_merge_mode")]
    pub merge_mode: String,
    /// Auto-prompt when a group scores >= merge_prompt.
    #[serde(default = "default_true")]
    pub merge_auto_prompt: bool,
    /// Document caps (Settings → Documents).
    #[serde(default = "default_doc_max")]
    pub doc_max_chars: i32,
    /// Scanned-PDF behavior: "warn" (skip + warning) or "shell".
    #[serde(default = "default_pdf_mode")]
    pub pdf_mode: String,
    /// Source retention after a successful output: "keep" (default, nothing
    /// deleted) | "delete" (queued source removed on success) | "archive"
    /// (`<stem>.src<ext>` copied beside the `.md`). Originals of failed or
    /// aborted jobs are never touched; videos are never touched (Section 5).
    #[serde(default = "default_retention")]
    pub audio_retention: String,
    /// Pre-STT denoise: "recommended" (adaptive gate, default) | "off" |
    /// "aggressive" (higher gate, same gentle compressor).
    #[serde(default = "default_denoise")]
    pub denoise_mode: String,
    /// Web research for video jobs (Settings → Research, default OFF).
    /// Opt-in per install; web text is cited and never mixed with film facts.
    #[serde(default = "default_false")]
    pub web_research: bool,
    /// Attention slider 0–100 (Settings → Documentary): drives frame density,
    /// caption detail, and REDUCE length together. 50 = Balanced.
    #[serde(default = "default_attention")]
    pub attention: i32,
}

impl Default for UiPrefs {
    fn default() -> Self {
        UiPrefs {
            dark_mode: true,
            models_dir: None,
            compute_mode: default_compute(),
            delete_converted: false,
            dismissed: Vec::new(),
            dismissed_groups: Vec::new(),
            view: default_view(),
            staged: Vec::new(),
            default_outdir: None,
            default_classes: String::new(),
            models_tier: default_tier(),
            review_drafts_dir: None,
            chunk_tokens: default_chunk_tokens(),
            summary_tier: default_summary_tier(),
            audio_window_sec: default_window(),
            vram_budget_pct: default_budget(),
            merge_suggest: default_suggest(),
            merge_prompt: default_prompt(),
            merge_mode: "ask".to_string(),
            merge_auto_prompt: true,
            doc_max_chars: 500_000,
            pdf_mode: "warn".to_string(),
            audio_retention: default_retention(),
            denoise_mode: default_denoise(),
            web_research: false,
            attention: default_attention(),
        }
    }
}

pub fn prefs_path() -> PathBuf {
    crate::dirs::data_dir().join("ui.json")
}

/// Load from an explicit path (pure — unit-testable).
pub fn load_from(path: &Path) -> UiPrefs {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// Save to an explicit path, creating parent dirs.
pub fn save_to(path: &Path, prefs: &UiPrefs) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
    }
    let t = serde_json::to_string_pretty(prefs).map_err(|e| e.to_string())?;
    std::fs::write(path, t).map_err(|e| e.to_string())
}

/// Load the live prefs file, migrating legacy keys.
/// Legacy `chunk_words` (word counts, range 400–2400) becomes `chunk_tokens`
/// (×4/3) exactly once: after the first save the file carries the new key
/// and migration never re-runs.
pub fn load() -> UiPrefs {
    load_with_migration(&prefs_path())
}

fn load_with_migration(path: &Path) -> UiPrefs {
    let text = std::fs::read_to_string(path).ok();
    let mut prefs: UiPrefs = text
        .as_deref()
        .and_then(|t| serde_json::from_str(t).ok())
        .unwrap_or_default();
    if let Some(t) = &text {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(t) {
            if v.get("chunk_tokens").is_none() {
                if let Some(w) = v.get("chunk_words").and_then(|x| x.as_i64()) {
                    prefs.chunk_tokens =
                        ((w as f32 * 4.0 / 3.0).round() as i32).clamp(200, 12000);
                }
            }
        }
    }
    prefs
}

/// Save the live prefs file.
pub fn save(prefs: &UiPrefs) -> Result<(), String> {
    save_to(&prefs_path(), prefs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("pv-prefs-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_file(&p);
        p
    }

    #[test]
    fn missing_file_yields_dark_default() {
        let p = tmp("missing");
        let loaded = load_from(&p);
        assert!(loaded.dark_mode);
        assert!(loaded.models_dir.is_none());
        assert_eq!(loaded.compute_mode, "auto");
    }

    #[test]
    fn legacy_chunk_words_migrate_once() {
        let p = tmp("migrate");
        std::fs::write(&p, r#"{"chunk_words":1200}"#).unwrap();
        let back = load_with_migration(&p);
        assert_eq!(back.chunk_tokens, 1600);
        // New key wins when both present; migration never re-runs.
        std::fs::write(&p, r#"{"chunk_words":1200,"chunk_tokens":4000}"#).unwrap();
        assert_eq!(load_with_migration(&p).chunk_tokens, 4000);
        // Absent keys yield the token default.
        std::fs::write(&p, r#"{}"#).unwrap();
        assert_eq!(load_with_migration(&p).chunk_tokens, 4000);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn roundtrip_and_corrupt_fallback() {
        let p = tmp("roundtrip");
        let prefs = UiPrefs {
            dark_mode: false,
            models_dir: Some("D:/m".to_string()),
            compute_mode: "cpu".to_string(),
            delete_converted: true,
            dismissed: vec!["gone.md".to_string()],
            ..UiPrefs::default()
        };
        save_to(&p, &prefs).unwrap();
        let back = load_from(&p);
        assert!(!back.dark_mode);
        assert_eq!(back.models_dir.as_deref(), Some("D:/m"));
        assert_eq!(back.compute_mode, "cpu");
        assert!(back.delete_converted);
        assert_eq!(back.dismissed, vec!["gone.md".to_string()]);
        std::fs::write(&p, "{oops").unwrap();
        assert!(load_from(&p).dark_mode);
        let _ = std::fs::remove_file(&p);
    }
}
