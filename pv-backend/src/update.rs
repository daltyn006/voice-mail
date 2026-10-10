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

    #[test]
    fn verify_against_sums_fixtures() {
        let data = b"installer-bytes";
        let good = "9d6064e3f5a5c7b3b1f0a4e2e2f8b1c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8090  voice-mail_1.0.2_x64_en-US.msi";
        // Wrong hash for these bytes.
        assert!(verify_against_sums(data, good, "voice-mail_1.0.2_x64_en-US.msi").is_err());
        // Right hash passes (computed here, not hardcoded, so the test
        // exercises parsing + compare, not a magic string).
        use sha2::Digest;
        let mut h = sha2::Sha256::new();
        h.update(data);
        let hex = crate::verify::hex_digest(h);
        let sums = format!("{hex}  voice-mail_1.0.2_x64_en-US.msi\n");
        assert!(verify_against_sums(data, &sums, "voice-mail_1.0.2_x64_en-US.msi").is_ok());
        // Full-path sidecars match by leaf name; comments/blanks skipped.
        let sums_paths = format!("# comment\n\n{hex}  C:\\dist\\voice-mail_1.0.2_x64_en-US.msi\n");
        assert!(verify_against_sums(data, &sums_paths, "voice-mail_1.0.2_x64_en-US.msi").is_ok());
        // Missing entry fails closed.
        assert!(verify_against_sums(data, &sums, "other.msi").is_err());
        // Empty document fails closed.
        assert!(verify_against_sums(data, "", "voice-mail_1.0.2_x64_en-US.msi").is_err());
    }

    #[test]
    fn install_command_shapes() {
        let (prog, args) = install_command(std::path::Path::new("C:\\t\\app.msi"));
        assert_eq!(prog, "msiexec");
        assert_eq!(args, vec!["/i", "C:\\t\\app.msi", "/quiet", "/norestart"]);
        let (prog, args) = install_command(std::path::Path::new("C:\\t\\setup.EXE"));
        assert_eq!(prog, "C:\\t\\setup.EXE");
        assert_eq!(args, vec!["/S"]);
    }

    #[test]
    fn fetch_update_rejects_missing_feed() {
        // No PV_UPDATE_FEED in test builds (option_env! is unset here).
        assert!(feed_url().is_none());
        assert!(fetch_update().is_err());
    }
}

/// A newer release worth installing: tag + installer + checksums sidecar.
/// Resolved from the releases feed; the installer URL is an opaque download
/// link (GitHub asset URL in production, any URL in tests/tools).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReleaseAsset {
    pub tag: String,
    pub name: String,
    pub url: String,
    pub sums_url: Option<String>,
}

/// Structured half of the update check: fetch the feed, compare versions.
/// `Ok(None)` = up to date; `Ok(Some(_))` = newer release with an
/// installer; `Err` = no feed / network / unparsable feed. `check_blocking`
/// keeps its human message behavior on top of this.
pub fn fetch_update() -> Result<Option<ReleaseAsset>, String> {
    let body = fetch_release_body()?;
    let tag = body
        .get("tag_name")
        .or_else(|| body.get("tag"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| "update feed has no release tag".to_string())?;
    if !newer_than(current_version(), tag) {
        return Ok(None);
    }
    match pick_installer(&body) {
        Some((name, url)) => Ok(Some(ReleaseAsset {
            tag: tag.to_string(),
            name,
            url,
            sums_url: pick_checksums(&body),
        })),
        None => Err("update feed has no installer asset".to_string()),
    }
}

fn fetch_release_body() -> Result<serde_json::Value, String> {
    let url = feed_url()
        .ok_or_else(|| "automatic update checks are not configured for this build (see RELEASE.md)".to_string())?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("update worker: {e}"))?;
    rt.block_on(async {
        let text = get_text(url, 15).await?;
        serde_json::from_str(&text).map_err(|e| format!("update feed unreadable: {e}"))
    })
}

fn http_client(timeout_secs: u64) -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent("voice-mail/0.1 (update-check; manual)")
        .timeout(std::time::Duration::from_secs(timeout_secs))
        .build()
        .map_err(|e| format!("update check failed: {e}"))
}

async fn get_text(url: &str, timeout_secs: u64) -> Result<String, String> {
    let text = http_client(timeout_secs)?
        .get(url)
        .send()
        .await
        .map_err(|e| format!("update check failed: {e}"))?
        .text()
        .await
        .map_err(|e| format!("update feed unreadable: {e}"))?;
    Ok(text)
}

/// Download raw bytes with a hard cap (fail closed before allocating).
/// 1 GiB ceiling comfortably covers installers (~120 MB) while bounding
/// a malicious/compromised feed. Runs on the caller's (worker) thread.
pub fn download_bytes(url: &str, max_bytes: u64) -> Result<Vec<u8>, String> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("update worker: {e}"))?;
    rt.block_on(async {
        let resp = http_client(1800)?
            .get(url)
            .send()
            .await
            .map_err(|e| format!("update download failed: {e}"))?;
        if let Some(total) = resp.content_length() {
            if total > max_bytes {
                return Err(format!(
                    "update refused: installer reports {total} bytes (cap {max_bytes})"
                ));
            }
        }
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| format!("update download failed: {e}"))?;
        if bytes.len() as u64 > max_bytes {
            return Err(format!(
                "update refused: installer is {} bytes (cap {max_bytes})",
                bytes.len()
            ));
        }
        Ok(bytes.to_vec())
    })
}

/// Verify bytes against a SHA256SUMS document. Matches by file NAME
/// (basename), so both bare-name and full-path sidecars work. Pure —
/// unit-tested with fixtures below.
pub fn verify_against_sums(
    data: &[u8],
    sums_text: &str,
    file_name: &str,
) -> Result<(), String> {
    use sha2::Digest;
    let want = sums_text
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let mut parts = line.split_whitespace();
            let hash = parts.next()?;
            let path = parts.next()?;
            // Basename compare: sidecars list bare names or full paths.
            let leaf = path.rsplit(['/', '\\']).next().unwrap_or(path);
            (leaf == file_name).then(|| hash.to_lowercase())
        })
        .next()
        .ok_or_else(|| format!("SHA256SUMS has no entry for {file_name}"))?;
    let mut hasher = sha2::Sha256::new();
    hasher.update(data);
    let got = crate::verify::hex_digest(hasher);
    if got.eq_ignore_ascii_case(&want) {
        Ok(())
    } else {
        Err(format!(
            "SHA256 mismatch for {file_name}: refusing to install unverified bytes"
        ))
    }
}

/// Largest installer we will ever fetch (per-machine MSI + NSIS exe are
/// ~80–130 MB; the cap leaves headroom without inviting abuse).
pub const MAX_INSTALLER_BYTES: u64 = 1024 * 1024 * 1024;

/// Fetch + verify one release asset end to end (worker thread only).
/// Returns the verified bytes; the caller writes them to a file and hands
/// the path to the UI layer for elevation + install + exit.
pub fn fetch_and_verify_asset(asset: &ReleaseAsset) -> Result<Vec<u8>, String> {
    let sums_url = asset
        .sums_url
        .clone()
        .ok_or_else(|| "release has no SHA256SUMS asset — refusing to install unverified bytes".to_string())?;
    let data = download_bytes(&asset.url, MAX_INSTALLER_BYTES)?;
    let sums = get_text_blocking(&sums_url)?;
    verify_against_sums(&data, &sums, &asset.name)?;
    Ok(data)
}

/// Blocking fetch of a small text document (SHA256SUMS sidecar).
/// Worker threads only — never the UI thread.
pub fn fetch_text(url: &str) -> Result<String, String> {
    get_text_blocking(url)
}

fn get_text_blocking(url: &str) -> Result<String, String> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("update worker: {e}"))?;
    rt.block_on(get_text(url, 120))
}

/// Install command for a verified installer file. Pure (no side effects):
/// per-machine MSI via quiet msiexec (major upgrade, never side-by-side),
/// NSIS exe via its silent switch. The caller must already run elevated
/// (per-machine targets) and must exit the app right after spawning —
/// an MSI cannot replace a running exe.
pub fn install_command(path: &std::path::Path) -> (String, Vec<String>) {
    let is_msi = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("msi"));
    if is_msi {
        (
            "msiexec".to_string(),
            vec![
                "/i".to_string(),
                path.to_string_lossy().into_owned(),
                "/quiet".to_string(),
                "/norestart".to_string(),
            ],
        )
    } else {
        (path.to_string_lossy().into_owned(), vec!["/S".to_string()])
    }
}

/// Spawn the installer detached (never wait — the caller exits next).
/// Per-machine targets prompt for elevation via the RunAs verb when the
/// caller isn't elevated. Windows-only: other platforms install from the
/// downloaded file by hand.
#[cfg(windows)]
pub fn launch_elevated(program: &str, args: &[String]) -> Result<(), String> {
    // PowerShell Start-Process -Verb RunAs is the scriptable elevation
    // path guaranteed present on Windows (no extra win32 bindings needed).
    // Quoting: each argument is single-quoted with embedded quotes doubled.
    fn q(s: &str) -> String {
        format!("'{}'", s.replace('\'', "''"))
    }
    let mut cmd = format!("Start-Process {} -Verb RunAs", q(program));
    if !args.is_empty() {
        let joined = args.iter().map(|a| q(a)).collect::<Vec<_>>().join(",");
        cmd.push_str(&format!(" -ArgumentList {joined}"));
    }
    std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            &cmd,
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("could not launch installer ({e}); install it by hand: {program}"))
}

/// Temporary download location for a verified installer (cleaned by the OS
/// temp policy; the file is single-use — install then forget).
pub fn staging_path(file_name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("voice-mail-update-{file_name}"))
}

/// Non-Windows: no elevated silent install path — the user installs the
/// downloaded file by hand (same bytes the MSI/EXE flow verifies).
#[cfg(not(windows))]
pub fn launch_elevated(program: &str, _args: &[String]) -> Result<(), String> {
    Err(format!(
        "automatic install is Windows-only; install by hand: {program}"
    ))
}
