//! Resumable HuggingFace model downloads. Each download gets its own cancel
//! flag (the old single-global-cancel bug is gone); progress flows as
//! [`crate::progress::Event::Download`] over the caller's channel.
//!
//! Resume contract (all on the worker, never the UI thread):
//! - Interrupted transfers leave `<file>.part` behind; the next `start()`
//!   re-hashes the prefix and continues with `Range: bytes=<prefix>-`, so a
//!   retry never re-downloads bytes it already has.
//! - A size-complete finalized file is hash-verified (when pinned) before it
//!   is accepted without network — a corrupt-but-right-sized file is
//!   quarantined and re-downloaded, never silently trusted.
//! - The final SHA-256 always covers every byte (prefix re-hash + stream) and
//!   is checked against the catalog pin before the single `.part`→final move.

use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::progress::Event;

const CHUNK_REPORT: u64 = 1 << 20;

/// Owns a tokio runtime plus one cancel flag per in-flight download id.
/// Flags are removed when their download terminates (done/cancel/fail), so
/// the map never grows across sessions.
pub struct Downloader {
    rt: tokio::runtime::Runtime,
    cancel: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>,
}

/// Drop a finished id's cancel flag. Best-effort by design (a poisoned lock
/// means the process is already going down).
fn drop_flag(map: &Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>, id: &str) {
    if let Ok(mut m) = map.lock() {
        m.remove(id);
    }
}

impl Downloader {
    pub fn new() -> Result<Self, String> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Downloader {
            rt,
            cancel: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    /// Start (or resume) the download for a catalog id. Emits progress events;
    /// a final `done: true` event means the file verified and activated.
    /// Unknown ids and missing backends are `Err` (caller falls back).
    pub fn start(&self, id: &str, tx: std::sync::mpsc::Sender<Event>) -> Result<(), String> {
        let cat = crate::catalog::load()?;
        let role =
            crate::catalog::role_of(&cat, id).ok_or_else(|| format!("unknown model id: {id}"))?;
        let (url, file, total) =
            crate::catalog::entry(&cat, id).ok_or_else(|| format!("unknown model id: {id}"))?;
        let dir = crate::dirs::models_dir();
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        // No fast path here: completeness + hash verification of a finalized
        // file happens in the worker (run_download), where multi-GB hashing
        // cannot jank the caller. Retries always resume any `.part` prefix.
        let flag = Arc::new(AtomicBool::new(false));
        if let Ok(mut m) = self.cancel.lock() {
            m.insert(id.to_string(), flag.clone());
        }
        let id_owned = id.to_string();
        let map = Arc::clone(&self.cancel);
        self.rt.spawn(run_download(
            id_owned,
            role.to_string(),
            url,
            dir,
            file,
            total,
            flag,
            map,
            tx,
        ));
        Ok(())
    }

    /// Start (or resume) the download for a vision id (text GGUF + mmproj
    /// projector). Progress reports against the combined total under the
    /// vision id; one cancel flag covers both files; the manifest records a
    /// single entry only when BOTH verify. Unknown ids are `Err`.
    pub fn start_vision(&self, id: &str, tx: std::sync::mpsc::Sender<Event>) -> Result<(), String> {
        let cat = crate::catalog::load()?;
        let e = crate::catalog::vision_entry(&cat, id)
            .ok_or_else(|| format!("unknown vision id: {id}"))?;
        let dir = crate::dirs::models_dir();
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        // Same as start(): the worker (run_vision_download) verifies each
        // finalized half and skips only what is complete + hash-clean.
        let flag = Arc::new(AtomicBool::new(false));
        if let Ok(mut m) = self.cancel.lock() {
            m.insert(id.to_string(), flag.clone());
        }
        let map = Arc::clone(&self.cancel);
        self.rt.spawn(run_vision_download(
            id.to_string(),
            e.text_url,
            e.text_file,
            e.text_bytes,
            e.mmproj_url,
            e.mmproj_file,
            e.mmproj_bytes,
            dir,
            flag,
            map,
            tx,
        ));
        Ok(())
    }

    /// Cancel one download; unknown ids are a no-op.
    pub fn cancel(&self, id: &str) {
        if let Ok(m) = self.cancel.lock() {
            if let Some(f) = m.get(id) {
                f.store(true, Ordering::SeqCst);
            }
        }
    }

    /// Cancel everything in flight.
    pub fn cancel_all(&self) {
        if let Ok(m) = self.cancel.lock() {
            for f in m.values() {
                f.store(true, Ordering::SeqCst);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_download(
    id: String,
    role: String,
    url: String,
    dir: std::path::PathBuf,
    file: String,
    total: u64,
    cancel: Arc<AtomicBool>,
    cancels: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>,
    tx: std::sync::mpsc::Sender<Event>,
) {
    let emit = |downloaded: u64, done: bool, error: String| {
        let _ = tx.send(Event::Download {
            id: id.clone(),
            downloaded,
            total,
            done,
            error,
        });
    };
    let fail = |msg: String| emit(0, false, msg);
    // Fast path: a finalized file that is complete AND hash-clean (when
    // pinned) needs no network. Backfill the manifest so boot verification
    // takes its fast path too; a corrupt-but-right-sized file was already
    // quarantined inside verified_complete, so fall through to a fresh fetch.
    let expected = crate::catalog::load()
        .ok()
        .and_then(|c| crate::catalog::entry_full(&c, &id))
        .map(|(_, _, _, sha)| sha)
        .unwrap_or_default();
    if let Some(hex) = verified_complete(&dir, &id, &file, total, &expected) {
        let mut man = crate::manifest::read(&dir);
        if !man.files.contains_key(&id) {
            crate::manifest::record_complete(&mut man, &id, &role, total, hex);
            let _ = crate::manifest::write(&dir, &man);
        }
        drop_flag(&cancels, &id);
        emit(total, true, String::new());
        return;
    }
    match fetch_one(&id, &url, &dir, &file, total, &cancel, 0, total, &tx).await {
        Ok((hex, downloaded)) => {
            // Hard-fail on hash mismatch: delete everything, no manifest entry.
            if let Err(e) = check_pinned(&id, &hex) {
                let _ = std::fs::remove_file(dir.join(&file));
                let _ = std::fs::remove_file(dir.join(file.clone() + ".part"));
                drop_flag(&cancels, &id);
                fail(e);
                return;
            }
            finish(&id, &role, &dir, &dir.join(file.clone() + ".part"), &dir.join(&file), &hex, downloaded, total, &tx);
            drop_flag(&cancels, &id);
        }
        Err(e) => {
            drop_flag(&cancels, &id);
            fail(e);
        }
    }
}

/// One vision id = two sequential files (text weights, then projector).
/// Progress accumulates against the combined total; a single manifest entry
/// records only after both verify. Cancel stops between or mid-file.
async fn run_vision_download(
    id: String,
    text_url: String,
    text_file: String,
    text_total: u64,
    mmproj_url: String,
    mmproj_file: String,
    mmproj_total: u64,
    dir: std::path::PathBuf,
    cancel: Arc<AtomicBool>,
    cancels: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>,
    tx: std::sync::mpsc::Sender<Event>,
) {
    let grand = text_total + mmproj_total;
    let emit = |downloaded: u64, done: bool, error: String| {
        let _ = tx.send(Event::Download {
            id: id.clone(),
            downloaded,
            total: grand,
            done,
            error,
        });
    };
    let fail = |msg: String| emit(0, false, msg);
    // Each finalized half is verified (hash when pinned) and skipped when
    // clean — a half-finished pair resumes exactly where it stopped, and a
    // corrupt-but-right-sized half is quarantined and re-fetched, never kept.
    let (text_expected, mmproj_expected) = crate::catalog::load()
        .ok()
        .and_then(|c| crate::catalog::vision_entry(&c, &id))
        .map(|e| (e.text_sha256, e.mmproj_sha256))
        .unwrap_or_default();
    let (thex, tdone, text_fetched) =
        match verified_complete(&dir, &id, &text_file, text_total, &text_expected) {
            Some(hex) => {
                emit(text_total, false, String::new());
                (hex, text_total, false)
            }
            None => {
                match fetch_one(&id, &text_url, &dir, &text_file, text_total, &cancel, 0, grand, &tx)
                    .await
                {
                    Ok((hex, done)) => (hex, done, true),
                    Err(e) => {
                        drop_flag(&cancels, &id);
                        return fail(e);
                    }
                }
            }
        };
    // Per-file pin check before the second fetch: a bad text blob never
    // leaves a half-pinned pair behind. (Skipped halves already verified
    // clean above, so the check below is a no-op for them.)
    if text_fetched {
        if let Ok(cat) = crate::catalog::load() {
            if let Some(e) = crate::catalog::vision_entry(&cat, &id) {
                if !e.text_sha256.is_empty() && !thex.eq_ignore_ascii_case(&e.text_sha256) {
                    let _ = std::fs::remove_file(dir.join(&text_file));
                    let _ = std::fs::remove_file(dir.join(format!("{text_file}.part")));
                    drop_flag(&cancels, &id);
                    return fail(format!(
                        "hash mismatch for {id} (text): re-download required"
                    ));
                }
            }
        }
        // fetch_one never renames (single owner rule) — finalize the text
        // blob here. Skipped halves are already finalized; moving nothing
        // would only manufacture an error.
        if let Err(e) = finalize_file(&dir.join(format!("{text_file}.part")), &dir.join(&text_file))
        {
            drop_flag(&cancels, &id);
            return fail(e);
        }
    }
    let (mhex, mdone, mmproj_fetched) =
        match verified_complete(&dir, &id, &mmproj_file, mmproj_total, &mmproj_expected) {
            Some(hex) => {
                emit(tdone + mmproj_total, false, String::new());
                (hex, mmproj_total, false)
            }
            None => {
                match fetch_one(
                    &id,
                    &mmproj_url,
                    &dir,
                    &mmproj_file,
                    mmproj_total,
                    &cancel,
                    tdone,
                    grand,
                    &tx,
                )
                .await
                {
                    Ok((hex, done)) => (hex, done, true),
                    Err(e) => {
                        drop_flag(&cancels, &id);
                        return fail(e);
                    }
                }
            }
        };
    if mmproj_fetched {
        if let Ok(cat) = crate::catalog::load() {
            if let Some(e) = crate::catalog::vision_entry(&cat, &id) {
                if !e.mmproj_sha256.is_empty() && !mhex.eq_ignore_ascii_case(&e.mmproj_sha256) {
                    let _ = std::fs::remove_file(dir.join(&mmproj_file));
                    let _ = std::fs::remove_file(dir.join(format!("{mmproj_file}.part")));
                    drop_flag(&cancels, &id);
                    return fail(format!(
                        "hash mismatch for {id} (mmproj): re-download required"
                    ));
                }
            }
        }
        if let Err(e) = finalize_file(
            &dir.join(format!("{mmproj_file}.part")),
            &dir.join(&mmproj_file),
        ) {
            drop_flag(&cancels, &id);
            return fail(e);
        }
    }
    let dir_buf = dir.clone();
    let mut man = crate::manifest::read(&dir_buf);
    crate::manifest::record_complete(&mut man, &id, "vlm", tdone + mdone, format!("{thex}+{mhex}"));
    let _ = crate::manifest::write(&dir_buf, &man);
    drop_flag(&cancels, &id);
    emit(grand, true, String::new());
}

/// Check a finalized file before (re)downloading: `Some(hex)` means complete
/// and clean — skip the fetch. `None` means fetch (resuming any `.part`
/// prefix, which `fetch_one` owns). A complete-sized but corrupt file is
/// quarantined to `<file>.corrupted` (pinned entries) or deleted, with any
/// stale `.part` dropped alongside so the retry starts fresh — never resumes
/// a prefix that already proved untrustworthy.
fn verified_complete(
    dir: &std::path::Path,
    id: &str,
    file: &str,
    want: u64,
    expected: &str,
) -> Option<String> {
    if want == 0 {
        return None;
    }
    let dest = dir.join(file);
    if std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0) != want {
        return None;
    }
    match crate::verify::sha256_file(&dest) {
        Ok(hex) if expected.is_empty() || hex.eq_ignore_ascii_case(expected) => Some(hex),
        _ => {
            // Right size, wrong bytes: never trust it, never resume onto it.
            if !expected.is_empty() {
                crate::verify::quarantine(dir, file, id);
            } else {
                let _ = std::fs::remove_file(&dest);
            }
            let _ = std::fs::remove_file(dir.join(format!("{file}.part")));
            None
        }
    }
}

/// True when a `206 Partial Content` response's `Content-Range` start
/// disagrees with our `.part` prefix length (`downloaded`). A missing or
/// unparseable header returns false (trust the append — the final SHA-256
/// still guards every byte); an explicit mismatch means the server is not
/// continuing where we asked, and appending would corrupt the file.
fn range_start_mismatch(headers: &reqwest::header::HeaderMap, downloaded: u64) -> bool {
    let Some(v) = headers.get(reqwest::header::CONTENT_RANGE) else {
        return false;
    };
    let Ok(s) = v.to_str() else {
        return false;
    };
    // "bytes <start>-<end>/<total>" (or ".../*").
    let s = s.strip_prefix("bytes ").unwrap_or(s);
    let Some(start) = s.split('-').next() else {
        return false;
    };
    match start.trim().parse::<u64>() {
        Ok(n) => n != downloaded,
        Err(_) => false,
    }
}

/// Transfer one file (resume-aware) with progress against a grand total.
/// Returns `(hex_sha256, bytes_written)`. Emits progress/fail events inline;
/// the caller owns manifest recording + the terminal done event.
async fn fetch_one(
    id: &str,
    url: &str,
    dir: &std::path::Path,
    file: &str,
    total: u64,
    cancel: &AtomicBool,
    base: u64,
    grand: u64,
    tx: &std::sync::mpsc::Sender<Event>,
) -> Result<(String, u64), String> {
    let emit = |downloaded: u64, done: bool, error: String| {
        let _ = tx.send(Event::Download {
            id: id.to_string(),
            downloaded,
            total: grand,
            done,
            error,
        });
    };
    let part = dir.join(format!("{file}.part"));
    let fail = |msg: String| msg;

    // Seed the hasher with any resumed prefix so the final hash covers all bytes.
    let mut hasher = Sha256::new();
    let mut downloaded: u64 = 0;
    if part.exists() {
        match std::fs::File::open(&part) {
            Ok(mut f) => {
                let mut buf = [0u8; 1 << 20];
                loop {
                    match f.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            hasher.update(&buf[..n]);
                            downloaded += n as u64;
                        }
                        Err(e) => return Err(fail(format!("cannot read partial file: {e}"))),
                    }
                }
            }
            Err(e) => return Err(fail(format!("cannot open partial file: {e}"))),
        }
        if total > 0 && downloaded == total {
            // Complete prefix on disk: hash it and return — finish() owns
            // the single .part→dest move.
            let hex = crate::verify::hex_digest(hasher);
            return Ok((hex, downloaded));
        }
        if total > 0 && downloaded > total {
            // Overshot prefix (stale part from a changed catalog entry, or a
            // torn write): it can never verify — drop it and start clean
            // instead of forcing the user through a fail + manual retry.
            let _ = std::fs::remove_file(&part);
            downloaded = 0;
            hasher = Sha256::new();
        }
    }
    let client = match reqwest::Client::builder()
        .user_agent("voice-mail/0.1")
        .build()
    {
        Ok(c) => c,
        Err(e) => return Err(fail(format!("http client: {e}"))),
    };
    use reqwest::StatusCode;
    // At most one restart: a server that answers our Range with a mismatched
    // window (or rejects it) gets one clean full-fetch attempt. Anything
    // beyond that is a real failure, not a resume hiccup.
    let mut restarts = 0;
    let (resp, append) = loop {
        let mut req = client.get(url);
        if downloaded > 0 {
            req = req.header("Range", format!("bytes={downloaded}-"));
        }
        let resp = match req.send().await {
            Ok(r) => r,
            Err(e) => return Err(fail(format!("request failed: {e}"))),
        };
        match resp.status() {
            StatusCode::PARTIAL_CONTENT => {
                if downloaded > 0 && range_start_mismatch(resp.headers(), downloaded) {
                    // The server is not continuing where we asked (stale
                    // cache, rotated file): appending would corrupt the hash
                    // beyond recovery — restart from byte zero, once.
                    restarts += 1;
                    if restarts > 1 {
                        return Err(fail("server range mismatch twice, aborting".to_string()));
                    }
                    let _ = std::fs::remove_file(&part);
                    downloaded = 0;
                    hasher = Sha256::new();
                    continue;
                }
                break (resp, true);
            }
            StatusCode::OK => {
                if downloaded > 0 {
                    // No range support: the prefix is unusable — truncate and
                    // take the full body instead of failing the retry.
                    downloaded = 0;
                    hasher = Sha256::new();
                    if std::fs::write(&part, b"").is_err() {
                        return Err(fail("cannot truncate partial file".to_string()));
                    }
                }
                break (resp, false);
            }
            StatusCode::RANGE_NOT_SATISFIABLE => {
                if total > 0 && downloaded == total {
                    let hex = crate::verify::hex_digest(hasher);
                    return Ok((hex, downloaded));
                }
                // Our prefix is beyond the server's EOF (upstream file
                // shrank): one clean restart, then a real error.
                restarts += 1;
                if restarts > 1 {
                    let _ = std::fs::remove_file(&part);
                    return Err(fail("range rejected twice, retry the download".to_string()));
                }
                let _ = std::fs::remove_file(&part);
                downloaded = 0;
                hasher = Sha256::new();
                continue;
            }
            s => return Err(fail(format!("server status: {s}"))),
        }
    };
    let mut out = match std::fs::OpenOptions::new()
        .create(true)
        .append(append)
        .write(!append)
        .open(&part)
    {
        Ok(f) => f,
        Err(e) => return Err(fail(format!("cannot write partial file: {e}"))),
    };
    let mut stream = resp.bytes_stream();
    let mut last_emit = downloaded;
    use futures_util::StreamExt;
    use std::io::Write;
    while let Some(chunk) = stream.next().await {
        if cancel.load(Ordering::SeqCst) {
            emit(base + downloaded, false, "cancelled".to_string());
            return Err("cancelled".to_string());
        }
        let bytes = match chunk {
            Ok(b) => b,
            Err(e) => return Err(fail(format!("transfer failed at {downloaded}/{total}: {e}"))),
        };
        if out.write_all(&bytes).is_err() {
            return Err(fail("disk write failed (out of space?)".to_string()));
        }
        hasher.update(&bytes);
        downloaded += bytes.len() as u64;
        if downloaded - last_emit >= CHUNK_REPORT {
            last_emit = downloaded;
            emit(base + downloaded, false, String::new());
        }
    }
    if total > 0 && downloaded != total {
        return Err(fail(format!("incomplete: got {downloaded}, want {total}")));
    }
    // NOTE: no rename here — the caller owns finalization (hash check, then
    // exactly one .part→dest move). Renaming here AND in finish() deleted
    // nothing but broke everything: the second rename always failed and
    // single-file downloads never recorded nor emitted done.
    Ok((crate::verify::hex_digest(hasher), downloaded))
}

/// Single `.part`→final move. `finish` (single-file flow) and the vision
/// flow (one call per blob) are the only owners — `fetch_one` never renames.
fn finalize_file(part: &std::path::Path, dest: &std::path::Path) -> Result<(), String> {
    if std::fs::rename(part, dest).is_err()
        && (std::fs::copy(part, dest).is_err() || std::fs::remove_file(part).is_err())
    {
        return Err("cannot finalize file".to_string());
    }
    Ok(())
}

/// Compare a computed hash against the pinned catalog value.
/// Empty expected = dev-sidecar unverified model → allowed, flagged upstream.
fn check_pinned(id: &str, hex: &str) -> Result<(), String> {
    let cat = crate::catalog::load().map_err(|e| e)?;
    // Single-file roles.
    if let Some((_, _, _, expected)) = crate::catalog::entry_full(&cat, id) {
        if expected.is_empty() || hex.eq_ignore_ascii_case(&expected) {
            return Ok(());
        }
        return Err(format!(
            "hash mismatch for {id}: got {hex}, want {expected} — deleted, re-download required"
        ));
    }
    // Vision ids never reach here with a single hex (run_vision_download
    // checks text + mmproj separately); unknown ids pass through — the
    // caller's catalog lookup already rejected them.
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn finish(
    id: &str,
    role: &str,
    dir: &std::path::Path,
    part: &std::path::Path,
    dest: &std::path::Path,
    hex: &str,
    downloaded: u64,
    total: u64,
    tx: &std::sync::mpsc::Sender<Event>,
) {
    if std::fs::rename(part, dest).is_err()
        && (std::fs::copy(part, dest).is_err() || std::fs::remove_file(part).is_err())
    {
        let _ = tx.send(Event::Download {
            id: id.to_string(),
            downloaded,
            total,
            done: false,
            error: "cannot finalize file".to_string(),
        });
        return;
    }
    let dir_buf = dir.to_path_buf();
    let mut man = crate::manifest::read(&dir_buf);
    crate::manifest::record_complete(&mut man, id, role, downloaded, hex.to_string());
    let _ = crate::manifest::write(&dir_buf, &man);
    let _ = tx.send(Event::Download {
        id: id.to_string(),
        downloaded,
        total,
        done: true,
        error: String::new(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finish_moves_part_records_manifest_and_completes() {
        // P0 regression: fetch_one must NOT rename (finish owns the single
        // .part→dest move). Simulate post-fetch state and prove finish lands
        // manifest + done:true.
        let dir = std::env::temp_dir().join(format!("pv-fin-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let part = dir.join("m.bin.part");
        let dest = dir.join("m.bin");
        std::fs::write(&part, b"weights").unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        finish("mid", "llm", &dir, &part, &dest, "hex", 7, 7, &tx);
        drop(tx); // close so the event drain below terminates
        assert!(!part.exists() && dest.exists());
        assert_eq!(crate::manifest::read(&dir).files["mid"].bytes, 7);
        let mut saw_done = false;
        for ev in rx {
            if let Event::Download { done, error, .. } = ev {
                if done {
                    assert!(error.is_empty());
                    saw_done = true;
                }
            }
        }
        assert!(saw_done);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn finalize_file_moves_once() {
        let dir = std::env::temp_dir().join(format!("pv-fin2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let part = dir.join("v.part");
        let dest = dir.join("v");
        std::fs::write(&part, b"x").unwrap();
        assert!(finalize_file(&part, &dest).is_ok());
        assert!(!part.exists() && dest.exists());
        // Second move of the same part fails cleanly (nothing to move).
        assert!(finalize_file(&part, &dest).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn verified_complete_skips_only_clean_files() {
        use sha2::Digest;
        let dir = std::env::temp_dir().join(format!("pv-vc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // Missing file → fetch.
        assert!(verified_complete(&dir, "id", "m.bin", 3, "").is_none());
        // Wrong size → fetch (no hashing, no deletion).
        std::fs::write(dir.join("m.bin"), b"ab").unwrap();
        assert!(verified_complete(&dir, "id", "m.bin", 3, "").is_none());
        assert!(dir.join("m.bin").exists());
        // Right size, no pin → hashed and accepted (hex recorded).
        std::fs::write(dir.join("m.bin"), b"abc").unwrap();
        let want = format!("{:x}", sha2::Sha256::digest(b"abc"));
        assert_eq!(
            verified_complete(&dir, "id", "m.bin", 3, ""),
            Some(want.clone())
        );
        // Right size + matching pin → accepted.
        assert_eq!(
            verified_complete(&dir, "id", "m.bin", 3, &want),
            Some(want.clone())
        );
        assert!(dir.join("m.bin").exists());
        // Right size + wrong pin → quarantined (pinned), stale part dropped.
        std::fs::write(dir.join("m.bin.part"), b"stale-prefix").unwrap();
        assert!(verified_complete(&dir, "id", "m.bin", 3, "00").is_none());
        assert!(!dir.join("m.bin").exists());
        assert!(!dir.join("m.bin.part").exists());
        assert!(dir.join("m.bin.corrupted").exists());
        // Right size + wrong pin is retried fresh, never resumed: covered.
        // Unpinned corrupt-by-size? Size IS the check — exact size passes.
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn range_start_mismatch_guards_resume_window() {
        use reqwest::header::{HeaderMap, HeaderValue, CONTENT_RANGE};
        let with = |v: &str| {
            let mut h = HeaderMap::new();
            h.insert(CONTENT_RANGE, HeaderValue::from_str(v).unwrap());
            h
        };
        // Exact continuation → append.
        assert!(!range_start_mismatch(&with("bytes 100-199/1000"), 100));
        // Server starts elsewhere → restart, never append.
        assert!(range_start_mismatch(&with("bytes 0-99/1000"), 100));
        assert!(range_start_mismatch(&with("bytes 50-149/1000"), 100));
        // Missing / junk header → trust append (final hash still guards).
        assert!(!range_start_mismatch(&HeaderMap::new(), 100));
        assert!(!range_start_mismatch(&with("nonsense"), 100));
    }

    #[test]
    fn cancel_unknown_id_is_noop() {
        let d = Downloader::new().unwrap();
        d.cancel("nope");
        d.cancel_all();
    }

    #[test]
    fn start_unknown_id_errors() {
        let d = Downloader::new().unwrap();
        let (tx, _rx) = std::sync::mpsc::channel();
        assert!(d.start("no-such-model", tx).is_err());
    }
}
