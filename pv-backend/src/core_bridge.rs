//! Safe wrapper around `present_core.dll`.
//!
//! The C worker calls back on its own thread; the trampoline below forwards
//! into an `mpsc` channel with the file **path** resolved from the run's
//! order vector (never the raw FIFO index). A run epoch tags every event so
//! stale pre-abort events are dropped instead of confusing the UI.
//!
//! Panic-safety: the `extern "C"` trampoline catches unwinds and never
//! touches a poisoned lock via `unwrap` — worst case an event is dropped.

use libloading::{Library, Symbol};
use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::progress::{Event, Stage};

#[repr(C)]
struct JobOpts {
    audio_path: *const c_char,
    stt_model: *const c_char,
    llm_model: *const c_char,
    out_dir: *const c_char,
    known_classes: *const c_char,
    db_path: *const c_char,
    chunk_tokens: i32,
    audio_window_sec: i32,
    vram_budget_pct: i32,
    skip_summary: i32,
    cpu_only: i32,
    delete_converted: i32,
    is_text: i32,
    display_name: *const c_char,
    summary_tier: *const c_char,
    audio_retention: *const c_char,
    denoise_mode: *const c_char,
    is_video: i32,
    web_research: i32,
    attention: i32,
    vlm_text_model: *const c_char,
    vlm_mmproj: *const c_char,
    transcript_path: *const c_char,
}

type PvQueueAdd = unsafe extern "C" fn(*const JobOpts) -> i32;
type PvProgressCb = Option<unsafe extern "C" fn(i32, i32, f32, *const c_char)>;
type PvRunAsync = unsafe extern "C" fn(PvProgressCb) -> i32;
type PvPending = unsafe extern "C" fn() -> i32;
type PvClear = unsafe extern "C" fn();
type PvPause = unsafe extern "C" fn(i32);
type PvAbort = unsafe extern "C" fn();
type PvAbortJoin = unsafe extern "C" fn(i32) -> i32;
type PvRemove = unsafe extern "C" fn(*const c_char) -> i32;
type PvResolve =
    unsafe extern "C" fn(*const c_char, *const c_char, *mut c_char, i32, *mut c_char, i32) -> i32;
type PvValidate = unsafe extern "C" fn(*const c_char, *mut c_char, i32, i32) -> i32;
type PvDetect = unsafe extern "C" fn(*mut f32, *mut f32, *mut i64, *const c_char) -> i32;
type PvAbi = unsafe extern "C" fn() -> i32;

/// Expected `PV_ABI_VERSION` from `present_core.h`. Bump together with it.
const EXPECT_ABI: i32 = 8;

/// Owned job description; the DLL copies everything synchronously on add.
#[derive(Clone, Debug, Default)]
pub struct JobSpec {
    pub audio: String,
    pub stt: String,
    pub llm: String,
    pub out_dir: String,
    pub classes: String,
    pub db: String,
    pub skip_summary: bool,
    pub cpu_only: bool,
    pub delete_converted: bool,
    /// Nonzero: `audio` is a text payload (extracted document / merged input),
    /// not media — the core skips decode+STT and summarizes directly.
    pub is_text: bool,
    pub chunk_tokens: i32,
    pub audio_window_sec: i32,
    pub vram_budget_pct: i32,
    /// Original filename for titles/Day/Class (doc caches + merges).
    pub display_name: String,
    /// Summary length tier: "recap" (25%) | "standard" (50%) | "detailed" (75%).
    pub summary_tier: String,
    /// Source retention after success: "keep" | "delete" | "archive".
    pub audio_retention: String,
    /// Pre-STT denoise: "recommended" | "off" | "aggressive".
    pub denoise_mode: String,
    /// Video job: timestamped STT + frame sampling downstream. Source file is
    /// read in place — never copied into output, never deleted.
    pub is_video: bool,
    /// Opt-in web research for this job (up to 10 cited sources, Section 5).
    pub web_research: bool,
    /// Attention slider 0–100: frame density + caption detail + length.
    pub attention: i32,
    /// Active vision pair for video captioning: text GGUF + mmproj paths.
    /// Empty when no VLM is active (transcript+research only).
    pub vlm_text: String,
    pub vlm_mmproj: String,
    /// Phase-2 video pass: pass-1 transcript file. Empty = transcribe.
    pub transcript_path: String,
}

#[derive(Clone, Copy, Debug)]
pub struct DetectOut {
    pub tier: i32, // 0 lite, 1 standard, 2 full
    pub vram_gb: f32,
    pub ram_gb: f32,
    pub disk_free: i64,
}

/// Options remembered per queued path: the retry/skip re-queue source of
/// truth. Lives here (not in `queue.rs`) so [`SharedState`] owns one type.
#[derive(Clone, Debug, Default)]
pub struct StoredOpts {
    pub stt: String,
    pub llm: String,
    pub classes: String,
    pub db: String,
    pub cpu_only: bool,
    pub delete_converted: bool,
    pub is_text: bool,
    pub chunk_tokens: i32,
    pub audio_window_sec: i32,
    pub vram_budget_pct: i32,
    pub display_name: String,
    pub summary_tier: String,
    pub audio_retention: String,
    pub denoise_mode: String,
    pub is_video: bool,
    pub web_research: bool,
    pub attention: i32,
    pub vlm_text: String,
    pub vlm_mmproj: String,
    pub transcript_path: String,
}

pub fn spec_to_stored(spec: &JobSpec) -> StoredOpts {
    StoredOpts {
        stt: spec.stt.clone(),
        llm: spec.llm.clone(),
        classes: spec.classes.clone(),
        db: spec.db.clone(),
        cpu_only: spec.cpu_only,
        delete_converted: spec.delete_converted,
        is_text: spec.is_text,
        chunk_tokens: spec.chunk_tokens,
        audio_window_sec: spec.audio_window_sec,
        vram_budget_pct: spec.vram_budget_pct,
        display_name: spec.display_name.clone(),
        summary_tier: spec.summary_tier.clone(),
        audio_retention: spec.audio_retention.clone(),
        denoise_mode: spec.denoise_mode.clone(),
        is_video: spec.is_video,
        web_research: spec.web_research,
        attention: spec.attention,
        vlm_text: spec.vlm_text.clone(),
        vlm_mmproj: spec.vlm_mmproj.clone(),
        transcript_path: spec.transcript_path.clone(),
    }
}

pub fn stored_to_spec(path: &str, stored: &StoredOpts, out_dir: &Path, skip: bool) -> JobSpec {
    JobSpec {
        audio: path.to_string(),
        stt: stored.stt.clone(),
        llm: stored.llm.clone(),
        out_dir: out_dir.to_string_lossy().into_owned(),
        classes: stored.classes.clone(),
        db: stored.db.clone(),
        skip_summary: skip,
        cpu_only: stored.cpu_only,
        delete_converted: stored.delete_converted,
        is_text: stored.is_text,
        chunk_tokens: stored.chunk_tokens,
        audio_window_sec: stored.audio_window_sec,
        vram_budget_pct: stored.vram_budget_pct,
        display_name: stored.display_name.clone(),
        summary_tier: stored.summary_tier.clone(),
        audio_retention: stored.audio_retention.clone(),
        denoise_mode: stored.denoise_mode.clone(),
        is_video: stored.is_video,
        web_research: stored.web_research,
        attention: stored.attention,
        vlm_text: stored.vlm_text.clone(),
        vlm_mmproj: stored.vlm_mmproj.clone(),
        transcript_path: stored.transcript_path.clone(),
    }
}

/// Mutable run state shared between [`Queue`](crate::queue::Queue) and the
/// C-callback trampoline. Guarded by ONE mutex — no lock ordering exists.
#[derive(Default)]
pub struct SharedState {
    /// Append-only pop history: index `i` in core events means `order[i]`
    /// because the core file index counts pops globally (never reset except
    /// by `pv_queue_clear`, which is paired with a full clear + epoch bump
    /// in `abort_all`). Re-queued paths append duplicates; aborted ghosts
    /// stay as placeholders. Only pre-pop (waiting) drops and full clears
    /// ever remove entries — anything else misattributes future events.
    pub order: Vec<String>,
    /// Bumped on every abort; events from older epochs are dropped.
    pub epoch: u64,
    /// Last-known options per path (retry/skip re-queue source of truth).
    pub opts: std::collections::HashMap<String, StoredOpts>,
}

/// Record a queued path in pop order. Always appends — even for duplicates:
/// a re-queued file pops again under a NEW core index, so it needs a NEW
/// slot at the end. Callers must have already validated the path.
pub fn remember_path(st: &mut SharedState, path: &str, spec: StoredOpts) {
    st.order.push(path.to_string());
    st.opts.insert(path.to_string(), spec);
}

struct Sink {
    tx: std::sync::mpsc::Sender<Event>,
    epoch: u64,
    state: Arc<Mutex<SharedState>>,
}

static SINK: Mutex<Option<Sink>> = Mutex::new(None);

unsafe extern "C" fn trampoline(file_index: i32, stage: i32, fraction: f32, msg: *const c_char) {
    let _ = std::panic::catch_unwind(|| {
        let message = if msg.is_null() {
            String::new()
        } else {
            CStr::from_ptr(msg).to_string_lossy().into_owned()
        };
        let guard = match SINK.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        let sink = match guard.as_ref() {
            Some(s) => s,
            None => return,
        };
        let st = match sink.state.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        if sink.epoch != st.epoch {
            return; // stale pre-abort run: drop, the UI already owns that state
        }
        let path = match st.order.get(file_index as usize) {
            Some(p) => p.clone(),
            None => return, // unknown index (removed file): drop, never misattribute
        };
        let is_done = stage == 5;
        let aborted = stage == 6 && message == "aborted";
        drop(st);
        // Completion retires the stored options so removed files can't resurrect.
        // `order` is deliberately append-only (duplicates/ghosts stay): core
        // file indices count pops globally, so order[i] must forever equal
        // the i-th popped path. Only pre-pop removals (waiting drops) and a
        // full clear (abort_all, paired with the core counter reset) mutate it.
        if is_done {
            if let Ok(mut st) = sink.state.lock() {
                st.opts.remove(&path);
            }
        }
        let _ = sink.tx.send(Event::File {
            file: path,
            stage: Stage::from_i32(stage),
            fraction,
            message,
            aborted,
        });
    });
}

/// Loaded `present_core.dll`. Absence is normal in dev — callers treat
/// `load_bundled` failure as "simulation mode".
///
/// The library handle is `ManuallyDrop`: the DLL is intentionally NEVER
/// unloaded for the life of the process. Rationale: framework teardown drops
/// `CoreLib` while a backend worker (or GPU dispatch) may still execute
/// inside the DLL — `FreeLibrary` under a live worker aborts the process
/// (the observed close-crash). Leaking one handle per process is free;
/// the OS reclaims everything at exit.
pub struct CoreLib {
    lib: std::mem::ManuallyDrop<Library>,
}

impl CoreLib {
    pub fn load(path: &Path) -> Result<Self, String> {
        // SAFETY: loading a DLL is inherently unsafe; symbols are looked up
        // per-call with exact C signatures matching present_core.h.
        unsafe { Library::new(path) }
            .map(|lib| CoreLib {
                lib: std::mem::ManuallyDrop::new(lib),
            })
            .map_err(|e| e.to_string())
    }

    /// Exe dir first (dev/portable), then the bundled `$RESOURCE` subdir.
    /// Verifies the C ABI version: a stale DLL from a previous install is
    /// refused with an actionable message instead of running into struct
    /// layout mismatches (previously a no-diagnosis native crash class).
    pub fn load_bundled() -> Result<Self, String> {
        let exe = crate::dirs::exe_dir();
        let mut mismatch: Option<String> = None;
        for cand in [
            exe.join("present_core.dll"),
            exe.join("resources").join("present_core.dll"),
        ] {
            if let Ok(lib) = Self::load(&cand) {
                match lib.abi_version() {
                    Ok(v) if v == EXPECT_ABI => return Ok(lib),
                    Ok(v) => {
                        mismatch = Some(format!(
                            "backend DLL mismatch (ABI {v}, want {EXPECT_ABI}) — reinstall or relaunch the app"
                        ));
                    }
                    Err(_) => {
                        mismatch = Some(
                            "backend DLL too old (no ABI marker) — reinstall or relaunch the app"
                                .to_string(),
                        );
                    }
                }
            }
        }
        Err(mismatch.unwrap_or_else(|| "present_core.dll not loaded".to_string()))
    }

    fn abi_version(&self) -> Result<i32, String> {
        unsafe {
            let f: Symbol<PvAbi> = self
                .lib
                .get(b"pv_abi_version")
                .map_err(|e| e.to_string())?;
            Ok(f())
        }
    }

    pub fn queue_add(&self, o: &JobSpec) -> Result<i32, String> {
        unsafe {
            let add: Symbol<PvQueueAdd> =
                self.lib.get(b"pv_queue_add").map_err(|e| e.to_string())?;
            let audio = CString::new(o.audio.clone()).map_err(|e| e.to_string())?;
            let stt = CString::new(o.stt.clone()).map_err(|e| e.to_string())?;
            let llm = CString::new(o.llm.clone()).map_err(|e| e.to_string())?;
            let out = CString::new(o.out_dir.clone()).map_err(|e| e.to_string())?;
            let cls = CString::new(o.classes.clone()).map_err(|e| e.to_string())?;
            let db = CString::new(o.db.clone()).map_err(|e| e.to_string())?;
            let disp = CString::new(o.display_name.clone()).map_err(|e| e.to_string())?;
            let tier = CString::new(o.summary_tier.clone()).map_err(|e| e.to_string())?;
            let ret = CString::new(o.audio_retention.clone()).map_err(|e| e.to_string())?;
            let den = CString::new(o.denoise_mode.clone()).map_err(|e| e.to_string())?;
            let vtx = CString::new(o.vlm_text.clone()).map_err(|e| e.to_string())?;
            let vmp = CString::new(o.vlm_mmproj.clone()).map_err(|e| e.to_string())?;
            let trp = CString::new(o.transcript_path.clone()).map_err(|e| e.to_string())?;
            let opts = JobOpts {
                audio_path: audio.as_ptr(),
                stt_model: stt.as_ptr(),
                llm_model: llm.as_ptr(),
                out_dir: out.as_ptr(),
                known_classes: cls.as_ptr(),
                db_path: db.as_ptr(),
                chunk_tokens: if o.chunk_tokens > 0 { o.chunk_tokens } else { 1000 },
                audio_window_sec: if o.audio_window_sec > 0 {
                    o.audio_window_sec
                } else {
                    30
                },
                vram_budget_pct: if o.vram_budget_pct > 0 {
                    o.vram_budget_pct
                } else {
                    80
                },
                skip_summary: if o.skip_summary { 1 } else { 0 },
                cpu_only: if o.cpu_only { 1 } else { 0 },
                delete_converted: if o.delete_converted { 1 } else { 0 },
                is_text: if o.is_text { 1 } else { 0 },
                display_name: disp.as_ptr(),
                summary_tier: tier.as_ptr(),
                audio_retention: ret.as_ptr(),
                denoise_mode: den.as_ptr(),
                is_video: if o.is_video { 1 } else { 0 },
                web_research: if o.web_research { 1 } else { 0 },
                attention: o.attention.clamp(0, 100),
                vlm_text_model: vtx.as_ptr(),
                vlm_mmproj: vmp.as_ptr(),
                transcript_path: trp.as_ptr(),
            };
            Ok(add(&opts))
        }
    }

    pub fn pending(&self) -> Result<i32, String> {
        unsafe {
            let f: Symbol<PvPending> = self
                .lib
                .get(b"pv_queue_pending")
                .map_err(|e| e.to_string())?;
            Ok(f())
        }
    }

    pub fn clear(&self) -> Result<(), String> {
        unsafe {
            let f: Symbol<PvClear> = self.lib.get(b"pv_queue_clear").map_err(|e| e.to_string())?;
            f();
        }
        Ok(())
    }

    pub fn pause(&self, on: bool) -> Result<(), String> {
        unsafe {
            let f: Symbol<PvPause> = self.lib.get(b"pv_queue_pause").map_err(|e| e.to_string())?;
            f(if on { 1 } else { 0 });
        }
        Ok(())
    }

    pub fn abort_current(&self) -> Result<(), String> {
        unsafe {
            let f: Symbol<PvAbort> = self
                .lib
                .get(b"pv_abort_current")
                .map_err(|e| e.to_string())?;
            f();
        }
        Ok(())
    }

    /// Quit-path shutdown: abort + bounded drain + JOIN the worker thread.
    /// Returns 0 joined, 1 timeout (detached inside; exit anyway).
    /// Missing export (old DLL) degrades to plain abort.
    pub fn abort_and_join(&self, timeout_ms: i32) -> i32 {
        unsafe {
            let f: Symbol<PvAbortJoin> = match self.lib.get(b"pv_abort_and_join") {
                Ok(f) => f,
                Err(_) => {
                    let _ = self.abort_current();
                    return 0;
                }
            };
            f(timeout_ms)
        }
    }

    /// Drop a waiting job by path. Returns `true` when one was removed.
    pub fn remove(&self, path: &str) -> Result<bool, String> {
        unsafe {
            let f: Symbol<PvRemove> = self
                .lib
                .get(b"pv_queue_remove")
                .map_err(|e| e.to_string())?;
            let c = CString::new(path).map_err(|e| e.to_string())?;
            Ok(f(c.as_ptr()) == 1)
        }
    }

    /// Install the event sink for this run epoch, then start the worker.
    /// Re-running while draining is a no-op inside the core.
    pub fn run(
        &self,
        tx: std::sync::mpsc::Sender<Event>,
        epoch: u64,
        state: Arc<Mutex<SharedState>>,
    ) -> Result<(), String> {
        // Single assignment under one lock: the old two-lock sequence
        // (clear, drop, re-lock, set) let another thread interleave a stale
        // sink between the locks.
        if let Ok(mut guard) = SINK.lock() {
            *guard = Some(Sink { tx, epoch, state });
        }
        unsafe {
            let run: Symbol<PvRunAsync> =
                self.lib.get(b"pv_run_async").map_err(|e| e.to_string())?;
            run(Some(trampoline));
        }
        Ok(())
    }

    pub fn wait_idle(&self) -> Result<(), String> {
        unsafe {
            let f: Symbol<unsafe extern "C" fn()> =
                self.lib.get(b"pv_wait_idle").map_err(|e| e.to_string())?;
            f();
        }
        Ok(())
    }

    /// Day/Class probe for the skip-summary filename check.
    pub fn resolve_day_class(&self, audio: &str, db: &str) -> Result<(String, String), String> {
        unsafe {
            let f: Symbol<PvResolve> = self
                .lib
                .get(b"pv_resolve_day_class")
                .map_err(|e| e.to_string())?;
            let a = CString::new(audio).map_err(|e| e.to_string())?;
            let d = CString::new(db).map_err(|e| e.to_string())?;
            let mut day = vec![0 as c_char; 256];
            let mut cls = vec![0 as c_char; 256];
            f(
                a.as_ptr(),
                d.as_ptr(),
                day.as_mut_ptr(),
                256,
                cls.as_mut_ptr(),
                256,
            );
            Ok((
                CStr::from_ptr(day.as_ptr()).to_string_lossy().into_owned(),
                CStr::from_ptr(cls.as_ptr()).to_string_lossy().into_owned(),
            ))
        }
    }

    /// Hardware tier probe. Callers treat DLL absence as "standard guess".
    pub fn detect_tier(&self, anchor: &str) -> Result<DetectOut, String> {
        unsafe {
            let f: Symbol<PvDetect> = self.lib.get(b"pv_detect_tier").map_err(|e| e.to_string())?;
            let a = CString::new(anchor).map_err(|e| e.to_string())?;
            let mut vram: f32 = -1.0;
            let mut ram: f32 = -1.0;
            let mut disk: i64 = -1;
            let tier = f(&mut vram, &mut ram, &mut disk, a.as_ptr());
            Ok(DetectOut {
                tier,
                vram_gb: vram,
                ram_gb: ram,
                disk_free: disk,
            })
        }
    }

    fn validate_one(&self, symbol: &[u8], model: &str, cpu_only: bool) -> Result<(), String> {
        unsafe {
            let f: Symbol<PvValidate> = self.lib.get(symbol).map_err(|e| e.to_string())?;
            let m = CString::new(model).map_err(|e| e.to_string())?;
            let mut err = vec![0 as c_char; 1024];
            let rc = f(
                m.as_ptr(),
                err.as_mut_ptr(),
                1024,
                if cpu_only { 1 } else { 0 },
            );
            if rc == 0 {
                Ok(())
            } else {
                let msg = CStr::from_ptr(err.as_ptr()).to_string_lossy().into_owned();
                Err(if msg.is_empty() {
                    "validation failed".to_string()
                } else {
                    msg
                })
            }
        }
    }

    /// Pre-run STT validation (load + test inference). See present_core.h.
    pub fn validate_stt(&self, stt: &str, cpu_only: bool) -> Result<(), String> {
        self.validate_one(b"pv_validate_stt", stt, cpu_only)
    }

    /// Pre-run LLM validation (load + tiny generate). See present_core.h.
    pub fn validate_llm(&self, llm: &str, cpu_only: bool) -> Result<(), String> {
        self.validate_one(b"pv_validate_llm", llm, cpu_only)
    }

    /// Pre-run vision validation (projector init + text model open).
    /// See present_core.h. Failure degrades runs, never blocks them.
    pub fn validate_vlm(&self, text: &str, mmproj: &str, cpu_only: bool) -> Result<(), String> {
        unsafe {
            type PvValidateVlm =
                unsafe extern "C" fn(*const c_char, *const c_char, i32) -> i32;
            let f: Symbol<PvValidateVlm> =
                self.lib.get(b"pv_validate_vlm").map_err(|e| e.to_string())?;
            let t = CString::new(text).map_err(|e| e.to_string())?;
            let m = CString::new(mmproj).map_err(|e| e.to_string())?;
            let rc = f(t.as_ptr(), m.as_ptr(), if cpu_only { 1 } else { 0 });
            if rc == 0 {
                Ok(())
            } else {
                Err("vision validation failed".to_string())
            }
        }
    }
}

/// Locate the DLL without loading (lets callers skip straight to sim mode).
pub fn bundled_dll_path() -> Option<PathBuf> {
    let exe = crate::dirs::exe_dir();
    [
        exe.join("present_core.dll"),
        exe.join("resources").join("present_core.dll"),
    ]
    .into_iter()
    .find(|p| p.exists())
}
