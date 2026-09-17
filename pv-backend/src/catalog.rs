//! Model catalog: `config/models.json` embedded at compile time, with an
//! optional sidecar override next to the exe (offline layouts).

use serde_json::Value;

/// Compile-time fallback so the app works even if the sidecar is missing.
pub const CATALOG_EMBEDDED: &str = include_str!("../../config/models.json");

/// Load the catalog: sidecar `models.json` beside the exe wins, else embedded.
pub fn load() -> Result<Value, String> {
    let sidecar = super::dirs::exe_dir().join("models.json");
    if sidecar.exists() {
        let t = std::fs::read_to_string(&sidecar).map_err(|e| e.to_string())?;
        return serde_json::from_str(&t).map_err(|e| e.to_string());
    }
    serde_json::from_str(CATALOG_EMBEDDED).map_err(|e| e.to_string())
}

/// A single catalog entry: `(download url, file name, exact byte size, expected SHA-256 hex)`.
/// `sha256` is empty for dev-sidecar overrides that decline to pin (treated as
/// unverified community models — see `is_sidecar()`).
pub fn entry(cat: &Value, id: &str) -> Option<(String, String, u64)> {
    entry_full(cat, id).map(|(url, file, bytes, _)| (url, file, bytes))
}

/// Full entry with the pinned hash. New code should prefer this.
pub fn entry_full(cat: &Value, id: &str) -> Option<(String, String, u64, String)> {
    for role in ["stt_models", "llm_models"] {
        if let Some(arr) = cat.get(role).and_then(|v| v.as_array()) {
            for m in arr {
                if m.get("id").and_then(|v| v.as_str()) == Some(id) {
                    let url = m
                        .get("url")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let file = m
                        .get("file")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let bytes = m.get("bytes").and_then(|v| v.as_u64()).unwrap_or(0);
                    let sha = m
                        .get("sha256")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    if !url.is_empty() && !file.is_empty() {
                        return Some((url, file, bytes, sha));
                    }
                }
            }
        }
    }
    None
}

/// True when the loaded catalog came from the dev-sidecar `models.json`
/// beside the exe (explicit override → unverified community models).
pub fn is_sidecar() -> bool {
    super::dirs::exe_dir().join("models.json").exists()
}

/// Role of an entry id: `"stt"`, `"llm"`, `"vlm"`, or `None` if unknown.
pub fn role_of(cat: &Value, id: &str) -> Option<&'static str> {
    for (role_key, role) in [
        ("stt_models", "stt"),
        ("llm_models", "llm"),
        ("vision_models", "vlm"),
    ] {
        if let Some(arr) = cat.get(role_key).and_then(|v| v.as_array()) {
            if arr
                .iter()
                .any(|m| m.get("id").and_then(|v| v.as_str()) == Some(id))
            {
                return Some(role);
            }
        }
    }
    None
}

/// The `(stt_id, llm_id)` pair for a tier name (`lite`/`standard`/`full`).
pub fn tier_pair(cat: &Value, tier: &str) -> Option<(String, String)> {
    let t = cat.get("tiers")?.get(tier)?;
    let stt = t.get("stt")?.as_str()?.to_string();
    let llm = t.get("llm")?.as_str()?.to_string();
    Some((stt, llm))
}

/// The vision id for a tier name. `None` on old catalogs without one.
pub fn tier_vlm(cat: &Value, tier: &str) -> Option<String> {
    cat.get("tiers")?
        .get(tier)?
        .get("vlm")?
        .as_str()
        .map(|s| s.to_string())
}

/// A two-file vision entry: text weights + mmproj projector.
#[derive(Clone, Debug)]
pub struct VisionEntry {
    pub id: String,
    pub text_url: String,
    pub text_file: String,
    pub text_bytes: u64,
    pub text_sha256: String,
    pub mmproj_url: String,
    pub mmproj_file: String,
    pub mmproj_bytes: u64,
    pub mmproj_sha256: String,
    pub tier: String,
    pub attention_min: i64,
    pub note: String,
}

fn get_str(m: &Value, key: &str) -> String {
    m.get(key).and_then(|v| v.as_str()).unwrap_or("").to_string()
}

/// Look up a vision entry by id. `None` when absent or malformed.
pub fn vision_entry(cat: &Value, id: &str) -> Option<VisionEntry> {
    let arr = cat.get("vision_models")?.as_array()?;
    for m in arr {
        if m.get("id").and_then(|v| v.as_str()) != Some(id) {
            continue;
        }
        let e = VisionEntry {
            id: id.to_string(),
            text_url: get_str(m, "text_url"),
            text_file: get_str(m, "text_file"),
            text_bytes: m.get("text_bytes").and_then(|v| v.as_u64()).unwrap_or(0),
            text_sha256: get_str(m, "text_sha256"),
            mmproj_url: get_str(m, "mmproj_url"),
            mmproj_file: get_str(m, "mmproj_file"),
            mmproj_bytes: m.get("mmproj_bytes").and_then(|v| v.as_u64()).unwrap_or(0),
            mmproj_sha256: {
                // `sha256` (singular) is the legacy combined slot; prefer the
                // per-file keys when present.
                let per = get_str(m, "mmproj_sha256");
                if per.is_empty() {
                    get_str(m, "sha256")
                } else {
                    per
                }
            },
            tier: get_str(m, "tier"),
            attention_min: m.get("attention_min").and_then(|v| v.as_i64()).unwrap_or(0),
            note: get_str(m, "note"),
        };
        if e.text_url.is_empty() || e.text_file.is_empty() || e.mmproj_url.is_empty() {
            return None;
        }
        return Some(e);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cat() -> Value {
        serde_json::from_str(CATALOG_EMBEDDED).unwrap()
    }

    #[test]
    fn embedded_catalog_resolves_default_pair() {
        let c = cat();
        let d = c.get("defaults").unwrap();
        let stt = d.get("stt_model").unwrap().as_str().unwrap();
        let llm = d.get("llm_model").unwrap().as_str().unwrap();
        let (url, file, bytes) = entry(&c, stt).unwrap();
        assert!(url.starts_with("https://huggingface.co/") && !file.is_empty() && bytes > 0);
        assert!(entry(&c, llm).is_some());
    }

    #[test]
    fn tiers_reference_known_ids() {
        let c = cat();
        for tier in ["lite", "standard", "full"] {
            let (s, l) = tier_pair(&c, tier).unwrap();
            assert_eq!(role_of(&c, &s), Some("stt"));
            assert_eq!(role_of(&c, &l), Some("llm"));
            let v = tier_vlm(&c, tier).unwrap();
            assert_eq!(role_of(&c, &v), Some("vlm"));
        }
    }

    #[test]
    fn vision_entries_carry_both_files() {
        let c = cat();
        for tier in ["lite", "standard", "full"] {
            let v = tier_vlm(&c, tier).unwrap();
            let e = vision_entry(&c, &v).unwrap();
            assert!(e.text_bytes > 0 && e.mmproj_bytes > 0);
            assert!(e.text_url.starts_with("https://huggingface.co/"));
            assert!(e.mmproj_url.starts_with("https://huggingface.co/"));
            assert!((0..=100).contains(&e.attention_min));
        }
        assert!(vision_entry(&c, "nope").is_none());
    }

    #[test]
    fn unknown_id_resolves_to_none() {
        assert!(entry(&cat(), "nope").is_none());
        assert!(role_of(&cat(), "nope").is_none());
        assert!(tier_pair(&cat(), "nope").is_none());
    }
}
