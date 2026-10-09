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

/// Pick the installer asset from a `/releases/latest` body: the per-machine
/// `.msi` first, then the NSIS `.exe`. Returns (file name, download URL).
/// Pure — unit-tested.
pub fn pick_installer(body: &serde_json::Value) -> Option<(String, String)> {
    let assets = body.get("assets")?.as_array()?;
    let mut exe: Option<(String, String)> = None;
    for a in assets {
        let name = a.get("name")?.as_str()?;
        let url = a.get("browser_download_url")?.as_str()?;
        let lower = name.to_ascii_lowercase();
        if lower.ends_with(".msi") {
            return Some((name.to_string(), url.to_string()));
        }
        if exe.is_none() && lower.ends_with(".exe") {
            exe = Some((name.to_string(), url.to_string()));
        }
    }
    exe
}

/// SHA256SUMS asset URL from the same body, when published. Pure.
pub fn pick_checksums(body: &serde_json::Value) -> Option<String> {
    body.get("assets")?.as_array()?.iter().find_map(|a| {
        let name = a.get("name")?.as_str()?;
        if name.eq_ignore_ascii_case("SHA256SUMS") {
            a.get("browser_download_url")?.as_str().map(|s| s.to_string())
        } else {
            None
        }
    })
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
            let mut msg = format!("Update available: {tag} (you have {cur}).");
            match pick_installer(&body) {
                Some((name, url)) => {
                    msg.push_str(&format!(" Installer: {name} — {url}"));
                }
                None => msg.push_str(" Grab it from the releases page."),
            }
            if let Some(sum) = pick_checksums(&body) {
                msg.push_str(&format!(" Verify with SHA256SUMS: {sum}"));
            }
            Ok(msg)
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

    #[test]
    fn installer_pick_prefers_msi() {
        let body: serde_json::Value = serde_json::from_str(
            r#"{"tag_name":"v0.2.0","assets":[
                {"name":"SHA256SUMS","browser_download_url":"https://x/SHA256SUMS"},
                {"name":"voice-mail_0.2.0_x64-setup.exe","browser_download_url":"https://x/setup.exe"},
                {"name":"voice-mail_0.2.0_x64_en-US.msi","browser_download_url":"https://x/app.msi"}]}"#,
        )
        .unwrap();
        assert_eq!(
            pick_installer(&body),
            Some(("voice-mail_0.2.0_x64_en-US.msi".to_string(), "https://x/app.msi".to_string()))
        );
        assert_eq!(pick_checksums(&body), Some("https://x/SHA256SUMS".to_string()));
        let exe_only: serde_json::Value = serde_json::from_str(
            r#"{"assets":[{"name":"setup.exe","browser_download_url":"https://x/setup.exe"}]}"#,
        )
        .unwrap();
        assert_eq!(
            pick_installer(&exe_only),
            Some(("setup.exe".to_string(), "https://x/setup.exe".to_string()))
        );
        let none: serde_json::Value = serde_json::from_str(r#"{"assets":[]}"#).unwrap();
        assert_eq!(pick_installer(&none), None);
    }
}
