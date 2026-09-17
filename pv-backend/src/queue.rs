//! FIFO pipeline orchestration. Owns an optional [`CoreLib`] (absent in dev),
//! the shared run state, and the session output dir. All paths are resolved
//! absolute at construction; every `output_*` op is confined under the
//! output dir with an extension whitelist.

use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::core_bridge::{
    remember_path, spec_to_stored, stored_to_spec, CoreLib, DetectOut, JobSpec, SharedState,
    StoredOpts,
};
use crate::progress::Event;

const ALLOWED_EXTS: &[&str] = &["md", "json"];

/// Join `name` under `base`, rejecting absolute paths, `..` escapes, and
/// non-whitelisted extensions. Symlinks are not followed: components are
/// checked lexically before any filesystem access.
fn confine(base: &Path, name: &str) -> Result<PathBuf, String> {
    let rel = Path::new(name);
    if rel.is_absolute() {
        return Err(format!("absolute paths not allowed: {name}"));
    }
    if rel.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err(format!("path escapes output dir: {name}"));
    }
    match rel.extension().and_then(|e| e.to_str()) {
        Some(ext) if ALLOWED_EXTS.contains(&ext.to_ascii_lowercase().as_str()) => {}
        _ => return Err(format!("extension not allowed (want .md/.json): {name}")),
    }
    Ok(base.join(rel))
}

pub struct Queue {
    core: Option<CoreLib>,
    state: Arc<Mutex<SharedState>>,
    out_dir: PathBuf,
}

impl Queue {
    /// New session. `out_dir = None` selects the per-user default. The dir is
    /// created on demand; a missing DLL means simulation mode (`live() == false`).
    pub fn new(out_dir: Option<PathBuf>) -> Self {
        let out_dir = out_dir.unwrap_or_else(crate::dirs::output_dir);
        Queue {
            core: CoreLib::load_bundled().ok(),
            state: Arc::new(Mutex::new(SharedState::default())),
            out_dir,
        }
    }

    pub fn live(&self) -> bool {
        self.core.is_some()
    }

    pub fn out_dir(&self) -> &Path {
        &self.out_dir
    }

    /// Retarget the session output dir (Input page override). Absolute paths
    /// only; the dir is created on first use.
    pub fn set_out_dir(&mut self, dir: PathBuf) {
        if dir.is_absolute() {
            self.out_dir = dir;
        }
    }

    fn state_op<T>(&self, f: impl FnOnce(&mut SharedState) -> T) -> Option<T> {
        self.state.lock().ok().map(|mut st| f(&mut st))
    }

    /// Queue files. **Validates everything before pushing anything**: unknown
    /// or missing inputs abort the whole call with no partial queue (#3).
    /// `is_text` parallels `paths` (true = extracted-document payload, the
    /// core skips decode+STT). Returns the number of files queued (#7).
    /// `skip_all` queues transcript-only jobs (phase-1 video passes);
    /// `out_dir` overrides the session dir (phase-1 temp staging).
    pub fn queue_files(
        &self,
        paths: &[String],
        is_text: &[bool],
        is_video: &[bool],
        displays: &[String],
        stt: &str,
        llm: &str,
        classes: &str,
        db: &str,
        cpu_only: bool,
        delete_converted: bool,
        chunk_tokens: i32,
        audio_window_sec: i32,
        vram_budget_pct: i32,
        summary_tier: &str,
        audio_retention: &str,
        denoise_mode: &str,
        web_research: bool,
        attention: i32,
        vlm_text: &str,
        vlm_mmproj: &str,
        transcript_path: &str,
        skip_all: bool,
        out_dir: Option<&Path>,
    ) -> Result<usize, String> {
        let core = self
            .core
            .as_ref()
            .ok_or_else(|| "backend offline".to_string())?;
        for p in paths {
            let meta = std::fs::metadata(p).map_err(|_| format!("input not found: {p}"))?;
            if !meta.is_file() {
                return Err(format!("not a file: {p}"));
            }
        }
        let mut n = 0;
        for (i, p) in paths.iter().enumerate() {
            let spec = JobSpec {
                audio: p.clone(),
                stt: stt.to_string(),
                llm: llm.to_string(),
                out_dir: out_dir
                    .map(|d| d.to_string_lossy().into_owned())
                    .unwrap_or_else(|| self.out_dir.to_string_lossy().into_owned()),
                classes: classes.to_string(),
                db: db.to_string(),
                skip_summary: skip_all,
                cpu_only,
                delete_converted,
                is_text: is_text.get(i).copied().unwrap_or(false),
                chunk_tokens,
                audio_window_sec,
                vram_budget_pct,
                display_name: displays.get(i).cloned().unwrap_or_default(),
                summary_tier: summary_tier.to_string(),
                audio_retention: audio_retention.to_string(),
                denoise_mode: denoise_mode.to_string(),
                is_video: is_video.get(i).copied().unwrap_or(false),
                web_research,
                attention: attention.clamp(0, 100),
                vlm_text: vlm_text.to_string(),
                vlm_mmproj: vlm_mmproj.to_string(),
                transcript_path: transcript_path.to_string(),
            };
            core.queue_add(&spec)?;
            let stored = spec_to_stored(&spec);
            self.state_op(|st| remember_path(st, p, stored));
            n += 1;
        }
        Ok(n)
    }

    /// Start (or resume) the worker, forwarding events tagged with this epoch.
    pub fn run(&self, tx: std::sync::mpsc::Sender<Event>) -> Result<(), String> {
        let core = self
            .core
            .as_ref()
            .ok_or_else(|| "backend offline".to_string())?;
        let epoch = self.state_op(|st| st.epoch).unwrap_or(0);
        core.run(tx, epoch, Arc::clone(&self.state))
    }

    /// Pre-run validation on a worker thread: load + discharge each backend
    /// in run order, emitting `Event::Validate` per phase. Loads its OWN
    /// CoreLib handle (refcounted, thread-safe) so the caller never lends
    /// shared state across threads. Total function: every path ends with a
    /// terminal `done: true` event, so the UI can never hang waiting.
    /// `cpu_only` travels explicitly — never env-dependent. `vlm` (text +
    /// mmproj paths) validates only when the batch holds video; its failure
    /// still terminates the phase (the UI degrades to transcript-only).
    pub fn validate_models(
        stt: &str,
        llm: &str,
        vlm: Option<(String, String)>,
        cpu_only: bool,
        tx: &std::sync::mpsc::Sender<Event>,
    ) {
        let send = |stage: &str, done: bool, error: String| {
            let _ = tx.send(Event::Validate {
                stage: stage.to_string(),
                done,
                error,
            });
        };
        let core = match crate::core_bridge::CoreLib::load_bundled() {
            Ok(c) => c,
            Err(e) => {
                send("stt", true, format!("backend offline: {e}"));
                return;
            }
        };
        send("stt", false, String::new());
        match core.validate_stt(stt, cpu_only) {
            Ok(()) => send("stt", true, String::new()),
            Err(e) => {
                send("stt", true, format!("stt: {e}"));
                return;
            }
        }
        send("llm", false, String::new());
        match core.validate_llm(llm, cpu_only) {
            Ok(()) => send("llm", true, String::new()),
            Err(e) => {
                send("llm", true, format!("llm: {e}"));
                return;
            }
        }
        if let Some((text, mmproj)) = vlm {
            send("vlm", false, String::new());
            match core.validate_vlm(&text, &mmproj, cpu_only) {
                Ok(()) => send("vlm", true, String::new()),
                Err(e) => send("vlm", true, format!("vlm: {e}")),
            }
        }
    }

    pub fn pending(&self) -> i32 {
        self.core
            .as_ref()
            .and_then(|c| c.pending().ok())
            .unwrap_or(0)
    }

    pub fn pause(&self, on: bool) -> Result<(), String> {
        match &self.core {
            Some(c) => c.pause(on),
            None => Ok(()), // sim mode: the UI honors its own paused flag
        }
    }

    /// Abort everything: stop the active file, drop the waiting queue, bump
    /// the epoch so in-flight events die, forget all options. The UI returns
    /// files to Input and re-queues on the next Start.
    /// Also releases a held pause — otherwise the worker would sleep forever
    /// on an empty queue and the next run would stall on arrival.
    pub fn abort_all(&self) -> Result<(), String> {
        if let Some(c) = &self.core {
            let _ = c.abort_current();
            let _ = c.clear();
            let _ = c.pause(false);
        }
        self.state_op(|st| {
            st.epoch += 1;
            st.order.clear();
            st.opts.clear();
        });
        Ok(())
    }

    /// Quit-path tail after [`abort_all`]: bounded drain + JOIN the worker
    /// thread. A joinable global thread destroyed at process exit terminates
    /// via SIGABRT (the close-crash) — this is the call that prevents it.
    /// No-op without a backend. Returns the core code (0 joined, 1 timeout).
    pub fn abort_and_join(&self, timeout_ms: i32) -> i32 {
        match &self.core {
            Some(c) => c.abort_and_join(timeout_ms),
            None => 0,
        }
    }

    /// Remove one file from processing. `active == true` aborts the running
    /// file (queue untouched) and releases any held pause so the worker moves
    /// on; otherwise drops its waiting copy.
    /// Index-stability rule (see `SharedState::order`): an aborted ACTIVE
    /// file already consumed a core pop index, so its entry stays as a
    /// placeholder ghost — removing it would shift every later mapping.
    /// Options stay too (retry re-queues from them and appends a new slot).
    /// Only pre-pop (waiting) drops prune `order`/`opts`.
    pub fn remove(&self, path: &str, active: bool) -> Result<(), String> {
        if active {
            if let Some(c) = &self.core {
                let _ = c.abort_current();
                let _ = c.pause(false);
            }
            self.state_op(|st| {
                st.epoch += 1;
            });
            return Ok(());
        }
        if let Some(c) = &self.core {
            let _ = c.remove(path);
        }
        self.state_op(|st| {
            st.order.retain(|p| p != path);
            st.opts.remove(path);
        });
        Ok(())
    }

    /// Retry: stop/drop the old copy, re-queue identical options, run.
    /// `active` selects abort-the-runner vs drop-the-waiter — never both,
    /// so neighbors are never killed by someone else's retry.
    pub fn retry(
        &self,
        path: &str,
        active: bool,
        tx: &std::sync::mpsc::Sender<Event>,
    ) -> Result<(), String> {
        let core = self
            .core
            .as_ref()
            .ok_or_else(|| "backend offline".to_string())?;
        let stored: StoredOpts = self
            .state_op(|st| st.opts.get(path).cloned())
            .flatten()
            .ok_or_else(|| format!("no stored options for: {path}"))?;
        stop_old_copy(core, path, active, &self.state)?;
        let spec = stored_to_spec(path, &stored, &self.out_dir, false);
        core.queue_add(&spec)?;
        // Append unconditionally (duplicates allowed): the re-queued file
        // pops again under a NEW core index and needs a NEW trailing slot.
        self.state_op(|st| remember_path(st, path, stored));
        let epoch = self.state_op(|st| st.epoch).unwrap_or(0);
        core.run(tx.clone(), epoch, Arc::clone(&self.state))
    }

    /// Skip-summary: same as retry but with the map-reduce bypass. The core
    /// emits the normal DONE with a raw-only `.md`.
    pub fn skip_summary(
        &self,
        path: &str,
        active: bool,
        tx: &std::sync::mpsc::Sender<Event>,
    ) -> Result<(), String> {
        let core = self
            .core
            .as_ref()
            .ok_or_else(|| "backend offline".to_string())?;
        let stored: StoredOpts = self
            .state_op(|st| st.opts.get(path).cloned())
            .flatten()
            .ok_or_else(|| format!("no stored options for: {path}"))?;
        stop_old_copy(core, path, active, &self.state)?;
        let spec = stored_to_spec(path, &stored, &self.out_dir, true);
        core.queue_add(&spec)?;
        self.state_op(|st| remember_path(st, path, stored));
        let epoch = self.state_op(|st| st.epoch).unwrap_or(0);
        core.run(tx.clone(), epoch, Arc::clone(&self.state))
    }

    /// Tiny filename probe for skip-summary: Day/Class via the core helper.
    /// `None` means the UI should use its local heuristic.
    pub fn probe_name(&self, path: &str) -> Result<Option<String>, String> {
        let core = self
            .core
            .as_ref()
            .ok_or_else(|| "backend offline".to_string())?;
        let db = self
            .state_op(|st| st.opts.get(path).map(|o| o.db.clone()))
            .flatten()
            .unwrap_or_else(|| crate::dirs::db_path().to_string_lossy().into_owned());
        let (day, cls) = core.resolve_day_class(path, &db)?;
        if day.is_empty() && cls.is_empty() {
            return Ok(None);
        }
        let stem = Path::new(path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("Untitled");
        Ok(Some(format!("{cls} - {day} - {stem}")))
    }

    /// Hardware tier probe; DLL absence is a labeled standard guess.
    pub fn detect_tier(&self) -> DetectOut {
        let anchor = crate::dirs::models_dir().to_string_lossy().into_owned();
        self.core
            .as_ref()
            .and_then(|c| c.detect_tier(&anchor).ok())
            .unwrap_or(DetectOut {
                tier: 1,
                vram_gb: -1.0,
                ram_gb: -1.0,
                disk_free: -1,
            })
    }

    // ---------- output store (all confined under out_dir) ----------

    fn ensure_out_dir(&self) -> Result<(), String> {
        std::fs::create_dir_all(&self.out_dir).map_err(|e| e.to_string())
    }

    pub fn output_list(&self) -> Result<Vec<String>, String> {
        self.ensure_out_dir()?;
        let mut out = Vec::new();
        for ent in std::fs::read_dir(&self.out_dir)
            .map_err(|e| e.to_string())?
            .flatten()
        {
            let p = ent.path();
            if p.extension()
                .and_then(|e| e.to_str())
                .map(|e| e.eq_ignore_ascii_case("md"))
                .unwrap_or(false)
            {
                out.push(p.to_string_lossy().into_owned());
            }
        }
        out.sort();
        Ok(out)
    }

    pub fn output_read(&self, name: &str) -> Result<String, String> {
        let base = self.out_dir.clone();
        self.output_read_in(&base, name)
    }

    pub fn output_read_in(&self, dir: &Path, name: &str) -> Result<String, String> {
        let p = confine(dir, name)?;
        std::fs::read_to_string(&p).map_err(|e| e.to_string())
    }

    pub fn output_write(&self, name: &str, content: &str) -> Result<(), String> {
        let base = self.out_dir.clone();
        self.output_write_in(&base, name, content)
    }

    pub fn output_write_in(&self, dir: &Path, name: &str, content: &str) -> Result<(), String> {
        let p = confine(dir, name)?;
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        std::fs::write(&p, content).map_err(|e| e.to_string())
    }

    pub fn output_remove(&self, name: &str) -> Result<(), String> {
        let base = self.out_dir.clone();
        self.output_remove_in(&base, name)
    }

    pub fn output_remove_in(&self, dir: &Path, name: &str) -> Result<(), String> {
        let p = confine(dir, name)?;
        std::fs::remove_file(&p).map_err(|e| e.to_string())
    }

    pub fn output_rename(&self, from: &str, to: &str) -> Result<(), String> {
        let base = self.out_dir.clone();
        self.output_rename_in(&base, from, to)
    }

    pub fn output_rename_in(&self, dir: &Path, from: &str, to: &str) -> Result<(), String> {
        let a = confine(dir, from)?;
        let b = confine(dir, to)?;
        if b.exists() {
            return Err(format!("refusing to overwrite: {to}"));
        }
        std::fs::rename(&a, &b).map_err(|e| e.to_string())
    }
}

/// Abort-the-runner (epoch bump, options kept for re-queue) or drop-the-waiter
/// (options forgotten). Single call site for both retry and skip paths.
fn stop_old_copy(
    core: &CoreLib,
    path: &str,
    active: bool,
    state: &Arc<Mutex<SharedState>>,
) -> Result<(), String> {
    if active {
        core.abort_current()?;
        if let Ok(mut st) = state.lock() {
            st.epoch += 1;
        }
    } else {
        let _ = core.remove(path);
        if let Ok(mut st) = state.lock() {
            st.order.retain(|p| p != path);
            st.opts.remove(path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_out(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("pv-q-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        p
    }

    #[test]
    fn spec_stored_roundtrip_preserves_cpu_flag() {
        let spec = crate::core_bridge::JobSpec {
            audio: "a.wav".into(),
            stt: "s".into(),
            llm: "l".into(),
            out_dir: "o".into(),
            classes: String::new(),
            db: "d".into(),
            skip_summary: false,
            cpu_only: true,
            delete_converted: true,
            is_text: true,
            chunk_tokens: 4000,
            audio_window_sec: 30,
            vram_budget_pct: 80,
            display_name: "a.wav".to_string(),
            summary_tier: "standard".to_string(),
            audio_retention: "archive".to_string(),
            denoise_mode: "recommended".to_string(),
            is_video: true,
            web_research: false,
            attention: 80,
            vlm_text: "t.gguf".to_string(),
            vlm_mmproj: "m.gguf".to_string(),
            transcript_path: "t.txt".to_string(),
        };
        let stored = crate::core_bridge::spec_to_stored(&spec);
        assert!(stored.cpu_only);
        assert!(stored.delete_converted);
        assert_eq!(stored.audio_retention, "archive");
        assert_eq!(stored.denoise_mode, "recommended");
        assert!(stored.is_video);
        assert_eq!(stored.attention, 80);
        let back =
            crate::core_bridge::stored_to_spec("a.wav", &stored, Path::new("o"), false);
        assert!(back.cpu_only);
        assert!(back.delete_converted);
    }

    #[test]
    fn validate_offline_ends_phase_with_error() {        // Test binaries have no DLL beside them: validation must report,
        // never hang waiting for events that will never come.
        let (tx, rx) = std::sync::mpsc::channel();
        Queue::validate_models("s", "l", None, false, &tx);
        drop(tx);
        let mut saw_terminal = false;
        for ev in rx {
            if let crate::progress::Event::Validate { done, error, .. } = ev {
                if done {
                    assert!(!error.is_empty());
                    saw_terminal = true;
                }
            }
        }
        assert!(saw_terminal);
    }

    #[test]
    fn confine_rejects_escapes_absolutes_and_bad_exts() {        let base = Path::new("C:/data/out");
        assert!(confine(base, "../x.md").is_err());
        assert!(confine(base, "C:/other/y.md").is_err());
        assert!(confine(base, "y.exe").is_err());
        assert!(confine(base, "y").is_err());
        assert_eq!(confine(base, "sub/y.md").unwrap(), base.join("sub/y.md"));
        assert_eq!(confine(base, "Y.MD").unwrap(), base.join("Y.MD"));
    }

    #[test]
    fn output_roundtrip_rename_remove() {
        let dir = tmp_out("rr");
        let q = Queue {
            core: None,
            state: Arc::new(Mutex::new(SharedState::default())),
            out_dir: dir.clone(),
        };
        q.output_write("a.md", "hello").unwrap();
        assert_eq!(q.output_read("a.md").unwrap(), "hello");
        assert!(q.output_list().unwrap().iter().any(|p| p.ends_with("a.md")));
        q.output_rename("a.md", "b.md").unwrap();
        assert!(q.output_read("b.md").is_ok());
        assert!(q.output_rename("b.md", "b.md").is_err()); // refuse overwrite
        q.output_remove("b.md").unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn queue_paths_reject_traversal_but_list_needs_dir() {
        let dir = tmp_out("trav");
        let q = Queue {
            core: None,
            state: Arc::new(Mutex::new(SharedState::default())),
            out_dir: dir.clone(),
        };
        assert!(q.output_read("../evil.md").is_err());
        assert!(q.output_list().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn remember_path_appends_duplicates_for_requeues() {
        // Core indices count pops globally: a re-queued file pops again
        // under a NEW index, so it needs a NEW trailing slot (ghosts stay).
        let mut st = SharedState::default();
        let mk = |s: &str| StoredOpts {
            stt: s.to_string(),
            ..StoredOpts::default()
        };
        remember_path(&mut st, "a", mk("1"));
        remember_path(&mut st, "b", mk("1"));
        remember_path(&mut st, "a", mk("2")); // re-queue after abort
        assert_eq!(st.order, vec!["a", "b", "a"]);
        assert_eq!(st.opts["a"].stt, "2");
    }

    #[test]
    fn remove_active_keeps_ghost_waiting_drops() {
        // Aborted ACTIVE files consumed a pop index: the ghost slot must
        // stay or every later mapping shifts. Waiting drops are pre-pop
        // and safe to prune.
        let dir = tmp_out("rmidx");
        let q = Queue {
            core: None,
            state: Arc::new(Mutex::new(SharedState::default())),
            out_dir: dir,
        };
        q.state_op(|st| {
            remember_path(st, "a", StoredOpts::default());
            remember_path(st, "b", StoredOpts::default());
        });
        q.remove("a", true).unwrap();
        let order = q.state_op(|st| st.order.clone()).unwrap();
        assert_eq!(order, vec!["a", "b"]);
        q.remove("b", false).unwrap();
        let order = q.state_op(|st| st.order.clone()).unwrap();
        assert_eq!(order, vec!["a"]);
    }

    #[test]
    fn offline_queue_ops_fail_cleanly() {
        let dir = tmp_out("off");
        let q = Queue {
            core: None,
            state: Arc::new(Mutex::new(SharedState::default())),
            out_dir: dir,
        };
        let (tx, _rx) = std::sync::mpsc::channel();
        assert!(q
            .queue_files(&[], &[], &[], &[], "", "", "", "", false, false, 4000, 30, 80, "standard", "keep", "recommended", false, 50, "", "", "", false, None)
            .is_err());
        assert!(q.run(tx).is_err());
        assert_eq!(q.pending(), 0);
        assert!(q.pause(true).is_ok());
        assert!(q.abort_all().is_ok());
        assert!(q.remove("x", false).is_ok());
    }

    #[test]
    fn detect_tier_without_dll_is_standard_guess() {
        let dir = tmp_out("det");
        let q = Queue {
            core: None,
            state: Arc::new(Mutex::new(SharedState::default())),
            out_dir: dir,
        };
        assert_eq!(q.detect_tier().tier, 1);
    }
}
