//! Manual update check (Settings → Diagnostics → "Check for updates").
//!
//! User-initiated only: no background polling, no telemetry — consistent
//! with the offline doctrine. The releases feed URL is baked in at release
//! time via the `PV_UPDATE_FEED` env var (see RELEASE.md); unset builds
//! report "not configured" instead of contacting anything.

/// Releases feed URL baked at release time (`PV_UPDATE_FEED`), e.g.
/// `https://api.github.com/repos/OWNER/REPO/releases/latest`.
pub fn feed_url() -> Option<&'static str> {
    option_env!("PV_UPDATE_FEED")
}

/// This build's version (workspace Cargo.toml files stay in lockstep).
pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// True when `latest` (optional leading `v`) is newer than `current`.
/// Dotted-numeric compare; non-numeric tails are ignored, never fatal.
pub fn newer_than(current: &str, latest: &str) -> bool {
    fn parts(s: &str) -> Vec<u64> {
        s.trim()
            .trim_start_matches('v')
            .split('.')
            .map(|p| {
                p.chars()
                    .take_while(|c| c.is_ascii_digit())
                    .collect::<String>()
                    .parse()
                    .unwrap_or(0)
            })
            .collect()
    }
    let (mut a, mut b) = (parts(current), parts(latest));
    while a.len() < b.len() {
        a.push(0);
    }
    while b.len() < a.len() {
        b.push(0);
    }
    a < b
}

/// Blocking fetch + compare. Runs on a worker thread — never the UI thread.
pub fn check_blocking() -> Result<String, String> {
    let url = feed_url()
        .ok_or_else(|| "automatic update checks are not configured for this build (see RELEASE.md)".to_string())?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("update worker: {e}"))?;
    rt.block_on(async {
        let client = reqwest::Client::builder()
            .user_agent("voice-mail/0.1 (update-check; manual)")
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .map_err(|e| format!("update check failed: {e}"))?;
        let text = client
            .get(url)
            .send()
            .await
            .map_err(|e| format!("update check failed: {e}"))?
            .text()
            .await
            .map_err(|e| format!("update feed unreadable: {e}"))?;
        // (reqwest `json` feature is off by pin policy — parse by hand.)
        let body: serde_json::Value = serde_json::from_str(&text)
            .map_err(|e| format!("update feed unreadable: {e}"))?;
        let tag = body
            .get("tag_name")
            .or_else(|| body.get("tag"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| "update feed has no release tag".to_string())?;
        let cur = current_version();
        if newer_than(cur, tag) {
            Ok(format!("Update available: {tag} (you have {cur}). Grab it from the releases page."))
        } else {
            Ok(format!("Up to date ({cur})."))
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_compare() {
        assert!(newer_than("0.1.0", "0.2.0"));
        assert!(newer_than("0.1.0", "v0.1.1"));
        assert!(!newer_than("0.2.0", "0.2.0"));
        assert!(!newer_than("0.2.0", "0.1.9"));
        assert!(!newer_than("0.1.0", "0.1.0-rc1"));
        assert!(newer_than("0.9", "0.10"));
    }
}
