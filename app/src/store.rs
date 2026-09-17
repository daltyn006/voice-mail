//! UI store: every page reads this entity, every control mutates it.
//! Files are keyed by stable numeric id + path (never FIFO index).
//! Input entries live until they reach Output or are manually deleted.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};

use gpui_kit::{App, Entity};
use pv_backend::download::Downloader;
use pv_backend::models::ModelStatus;
use pv_backend::progress::{Event, Stage};
use pv_backend::queue::Queue;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Page {
    Input,
    /// In-app voice capture (never auto-transcribes; takes are staged to
    /// Input explicitly or on navigating away with an unsent take).
    Record,
    Output,
    #[default]
    Models,
    Settings,
}

/// Explorer-style density, like the old View menu.
/// Full density layouts arrive with the M3 processing columns.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ViewMode {
    #[default]
    Large,
    List,
    Details,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum RecStatus {
    #[default]
    Empty,
    Recording,
    Paused,
    Stopped,
    Playing,
    PlayPaused,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum FileState {
    #[default]
    Queued,
    Active,
}

/// Staged input kind: audio goes through STT, docs go through extraction,
/// video goes through STT (audio track) + vision + research. Videos are read
/// in place from any path — never copied, never deleted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum InputKind {
    #[default]
    Audio,
    Doc,
    Video,
}

/// Video containers (subset of AUDIO_EXTS): staged as Video so the pipeline
/// runs timestamped STT + frame sampling + (opt-in) research on them.
pub const VIDEO_EXTS: &[&str] = &[
    "mp4", "m4v", "mov", "qt", "mkv", "mk3d", "webm", "avi", "divx", "wmv", "asf", "flv",
    "f4v", "mpg", "mpeg", "mpe", "mpv", "m2v", "ts", "m2ts", "m2t", "mts", "vob", "3gp",
    "3g2", "ogv", "rm", "rmvb", "nsv", "mxf",
];

/// True for video container extensions (case-insensitive, no dot). Pure.
pub fn is_video_ext(ext: &str) -> bool {
    VIDEO_EXTS.contains(&ext.to_ascii_lowercase().as_str())
}

#[derive(Clone, Debug)]
pub struct InputFile {
    pub id: u64,
    pub path: PathBuf,
    pub state: FileState,
    /// Bytes on disk at stage time (0 when unreadable). Shown in the
    /// Details view; captured once so rendering never does fs I/O.
    pub size: u64,
    pub kind: InputKind,
}

/// Structured GUI error (Error Center). `status` remains the last-line
/// summary; this list is the persistent history.
#[derive(Clone, Debug)]
pub struct AppError {
    pub id: u64,
    #[allow(dead_code)]
    pub time: String,
    pub source: String,
    pub msg: String,
    pub detail: String,
}

/// Review session (centralized; views hold only widget entities mirroring it).
#[derive(Clone, Debug, Default)]
pub struct ReviewSession {
    pub id: u64,
    #[allow(dead_code)]
    pub md_name: String,
    pub stem: String,
    pub left: String,
    pub right: String,
    pub center: String,
    pub merged: bool,
    pub dirty: bool,
    pub undo: Option<RevSnapData>,
}

#[derive(Clone, Debug, Default)]
pub struct RevSnapData {
    pub left: String,
    pub right: String,
    pub center: String,
    pub merged: bool,
}

#[derive(Clone, Debug)]
pub struct RenameSession {
    pub id: u64,
    pub text: String,
}

/// One similarity group for the merge UI.
#[derive(Clone, Debug)]
pub struct MergeSuggestion {
    pub key: String,
    pub members: Vec<(u64, String, String)>,
    pub score: f32,
    pub band: String,
}

pub fn view_to_string(v: ViewMode) -> String {
    match v {
        ViewMode::Large => "large".to_string(),
        ViewMode::List => "list".to_string(),
        ViewMode::Details => "details".to_string(),
    }
}

pub fn view_from_string(s: &str) -> ViewMode {
    match s {
        "list" => ViewMode::List,
        "details" => ViewMode::Details,
        _ => ViewMode::Large,
    }
}

/// M3 renders every field (dual bars, selection, skip badge).
#[derive(Clone, Debug)]
pub struct ProcFile {
    pub id: u64,
    pub path: PathBuf,
    pub name: String,
    pub stt: f32,
    pub sum: f32,
    pub stage: String,
    pub msg: String,
    #[allow(dead_code)]
    pub selected: bool,
    pub skipped: bool,
}

/// Every extension the bundled ffmpeg build can demux+decode into the
/// transcription pipe (verified against its `-formats`/`-decoders` tables:
/// native codecs, libopus/libgsm/libopenmpt/libgme families, and all
/// audio-bearing containers). The file picker AND the validation gate share
/// this list so they can never drift apart again.
///
/// Deliberately excluded: headerless raw PCM (`.raw/.pcm/.s16/…` — the pipe
/// cannot infer sample params), MIDI (`.mid/.midi/.kar` — no synthesizer),
/// playlists/subs/images, video-only raws, and legacy Audacity 2 projects
/// (`.aup`, BAD_EXTS). Modern `.aup3`/`.aup4` ride along via PROJECT_EXTS.
pub const AUDIO_EXTS: &[&str] = &[
    // lossless / uncompressed
    "wav", "flac", "aiff", "aif", "aifc", "au", "snd", "avr", "caf", "w64", "rf64",
    "voc", "wma", "alac", "wv", "ape", "mpc", "mpp", "mp+", "tak", "tta", "shn",
    "dsf", "dss", "sln", "gsm",
    // lossy voice/music
    "mp3", "mp2", "mp1", "mpa", "m1a", "m2a", "aac", "m4a", "m4b", "m4r", "ogg",
    "oga", "ogv", "ogx", "opus", "spx", "amr", "awb", "ac3", "eac3", "ec3", "dts",
    "dtshd", "mlp", "thd", "aa", "aax",
    // tracker + chip (libopenmpt / libmodplug / libgme demuxers)
    "mod", "xm", "it", "s3m", "mtm", "med", "okt", "669", "far", "stm", "ult",
    "ay", "gbs", "gym", "hes", "kss", "nsf", "nsfe", "sap", "spc", "vgm", "vgz",
    // audio-bearing containers (ffmpeg probes content; audio track is extracted)
    "mp4", "m4v", "mov", "qt", "3gp", "3g2", "3gp2", "3gpp", "isma", "ismv",
    "mkv", "mka", "mk3d", "webm", "weba", "webp", "avi", "divx", "wmv", "asf",
    "flv", "f4v", "f4a", "f4b", "mpg", "mpeg", "mpe", "mpv", "m2v", "ts", "m2ts",
    "m2t", "mts", "vob", "evo", "vro", "mxf", "gxf", "rm", "rmvb", "ra", "nsv",
    "ivf", "nut", "wtv", "dvr-ms", "smk", "roq", "bink", "binka", "brstm",
];

/// Project files that look stageable but contain no decodable audio.
/// Legacy `.aup` (XML + sidecar _data folder) is still rejected; modern
/// `.aup3`/`.aup4` (Audacity 3/4, Tenacity, Saucedacity — all SQLite) import
/// natively via the core renderer, as do crash-recovery `.aup3unsaved`.
pub const BAD_EXTS: &[&str] = &["aup"];

/// Audacity-family project containers (SQLite). Staged as Audio: the core
/// detects them by content ('AUDY' application_id) and renders a mono
/// mixdown before the normal STT path. Never copied, never modified.
pub const PROJECT_EXTS: &[&str] = &["aup3", "aup4", "aup3unsaved"];

/// Modern document extensions (single shared gate with the picker).
/// Backed by `pv-backend::docs::DOC_EXTS` so the two can never drift.
pub use pv_backend::docs::DOC_EXTS;

/// M4 renders every field (tree, editor, merge modal).
#[derive(Clone, Debug)]
pub struct OutFile {
    pub id: u64,
    pub name: String,
    pub md_name: String,
    /// Output dir holding this file's bytes (captured at creation/refresh).
    /// All read/write/remove/rename ops resolve under THIS dir — never the
    /// queue's current (possibly retargeted) dir — so moves of the Out
    /// folder can't orphan or duplicate files.
    pub dir: PathBuf,
    pub raw: String,
    pub summary: String,
    pub skipped: bool,
}

#[derive(Clone, Debug, Default)]
pub struct DlState {
    pub downloaded: u64,
    pub total: u64,
    pub done: bool,
    pub error: String,
}

pub struct Store {
    pub page: Page,
    pub input: Vec<InputFile>,
    pub processing: Vec<ProcFile>,
    pub output: Vec<OutFile>,
    pub unlocked: bool,
    pub started: bool,
    /// M3 pause/resume.
    pub paused: bool,
    pub backend_live: bool,
    /// A validation run is in flight (models loading, batch not queued yet).
    /// Second Start presses are refused until it resolves.
    validating: bool,
    /// Background boot integrity pass (hashing the active pair) is running.
    /// Start blocks with "Verifying models" only while this is true; the
    /// normal path resolves via the manifest fast-path without waiting.
    pub verifying_models: bool,
    /// The boot worker landed (Event::BootVerified): manifest fast-path
    /// rules from here, no synchronous hashing in begin_run.
    pub boot_verified: bool,
    /// Human-readable loading phase while `validating` ("Loading STT model…").
    /// None otherwise. The Start button renders from this.
    pub loading: Option<String>,
    pub view: ViewMode,
    /// Last-line status summary (the Error Center holds history).
    pub status: String,
    pub classes: String,
    pub queue: Queue,
    next_id: u64,
    /// Kept from `begin_run` so retry/skip can re-drive the worker on the
    /// same event stream (the pump thread outlives individual files).
    event_tx: Option<Sender<Event>>,
    downloader: Option<Downloader>,
    /// Per-model download state, fed by `Event::Download`.
    pub downloads: HashMap<String, DlState>,
    /// Last inventory snapshot (refreshed on entering Models + after ops).
    pub models: Vec<ModelStatus>,
    pub models_dir: String,
    pub active_stt: String,
    pub active_llm: String,
    /// Active vision pair id (text GGUF + mmproj). Empty = none downloaded.
    pub active_vlm: String,
    /// Run-scoped VLM paths (resolved once per Start for video batches).
    /// `vlm_expect` gates the validation phase; `None` paths degrade the
    /// run to transcript+research (never a hard error — vision is additive).
    vlm_expect: bool,
    vlm_validated: bool,
    run_vlm: Option<(PathBuf, PathBuf)>,
    /// First-run wizard visibility + chosen tier.
    pub wizard_open: bool,
    pub wizard_tier: String,
    pub wizard_detail: String,
    /// Theme mode (true = dark, the shipped look). Persisted in ui.json.
    pub dark_mode: bool,
    /// Pinned custom models dir; None = default home. Persisted in ui.json.
    pub custom_models_dir: Option<String>,
    /// Compute mode: "auto" (GPU first, CPU fallback) or "cpu".
    /// Persisted in ui.json; bridged to the backend via PV_CPU_ONLY.
    pub compute_mode: String,
    /// Delete converted WAV cache copies after a successful output.
    /// Persisted in ui.json; originals are never touched.
    pub delete_converted: bool,
    /// Last Ollama library scan (names + blob paths, nothing copied).
    pub ollama_scan: Vec<pv_backend::ollama::OllamaModel>,
    /// Warnings for files that couldn't be added (bad format, etc.).
    pub file_warnings: HashMap<PathBuf, String>,
    /// Output `md_name`s hidden via × (row-only dismiss). Kept so Refresh
    /// never resurrects them; 🗑 (file delete) clears its entry here.
    dismissed: HashSet<String>,
    // ---- Settings-centralized state (single source of truth) ----
    /// Default output folder override (None = per-user default home).
    pub default_outdir: Option<String>,
    /// Default known-classes list (`;`-separated). Start reads this.
    pub default_classes: String,
    /// Models-page tier filter (was local view state).
    pub models_tier: String,
    /// Review-drafts base override (None = `<data>/reviews`).
    pub review_drafts_dir: Option<String>,
    /// Summarizer chunk size in AI tokens (Settings → Summary).
    /// Presets: Small 1000 / Medium 4000 / Large 8000; custom 200–12000.
    pub chunk_tokens: i32,
    /// Summary length tier: "recap" (25%) | "standard" (50%) | "detailed" (75%).
    /// Drives chunk preset + tier guide + length ratio consistently.
    pub summary_tier: String,
    pub audio_window_sec: i32,
    pub vram_budget_pct: i32,
    /// Merge thresholds + mode (Settings → Merging).
    pub merge_suggest: f32,
    pub merge_prompt: f32,
    pub merge_mode: String,
    pub merge_auto_prompt: bool,
    /// Document caps (Settings → Documents).
    pub doc_max_chars: i32,
    pub pdf_mode: String,
    /// Source retention after success: "keep" | "delete" | "archive".
    /// Persisted; only successful outputs are eligible, videos never.
    pub audio_retention: String,
    /// Pre-STT denoise: "recommended" | "off" | "aggressive". Persisted.
    pub denoise_mode: String,
    /// Web research for video jobs (Settings → Research, default OFF).
    pub web_research: bool,
    /// Attention slider 0–100 (Settings → Documentary). 50 = Balanced.
    pub attention: i32,
    // ---- Error Center ----
    pub errors: Vec<AppError>,
    next_err: u64,
    pub show_errors: bool,
    /// Diagnostics log viewer visibility (Settings → Diagnostics).
    pub show_logs: bool,
    /// Loaded tails (filled on toggle, not per-frame — render never does fs I/O).
    pub log_view: Vec<pv_backend::diag::LogTail>,
    // ---- Centralized review/rename (views hold widget entities only) ----
    pub review: Option<ReviewSession>,
    pub renaming: Option<RenameSession>,
    /// `md_name`s with a persisted draft folder (from `index.json`).
    /// Refreshed on boot/refresh/open/submit/delete/sweep/rename.
    pub draft_badges: HashSet<String>,
    // ---- Merge UI ----
    pub merge_selection: HashSet<u64>,
    pub suggested: Vec<MergeSuggestion>,
    pub prompted: Vec<MergeSuggestion>,
    dismissed_groups: HashSet<String>,
    /// Cache-path → original display name (doc/merge payloads).
    display_names: HashMap<String, String>,
    /// Cache-path → merge members (for Sources-on-top rewrite at Done).
    pending_merged: HashMap<String, Vec<(String, String, String)>>,
    // ---- Record page (in-app voice capture; never auto-transcribes) ----
    /// Transport state for the Record page.
    pub rec_status: RecStatus,
    /// Finished take awaiting send/discard (None while recording/empty).
    pub rec_path: Option<PathBuf>,
    /// Chosen input (None = system default) + requested rate (None = native).
    pub rec_device: Option<String>,
    pub rec_rate_opt: Option<u32>,
    /// Live capture format + counters (polled from the recorder).
    pub rec_rate: u32,
    pub rec_channels: u16,
    pub rec_samples: u64,
    pub rec_peaks: Vec<f32>,
    pub rec_peak_db: String,
    pub rec_clips: u64,
    /// Finished-take length + playback cursor (frames) + sent flag.
    pub rec_total_secs: u64,
    pub rec_play_pos: u64,
    pub rec_sent: bool,
    /// All finished part files of the current/last take (gapless splits).
    /// `rec_path` stays the newest part for playback; every entry stages
    /// as its own standalone Input file on send.
    pub rec_parts: Vec<PathBuf>,
    /// Current 1-based part number while recording (status line "Part N").
    pub rec_part: u32,
    recorder: Option<pv_backend::record::Recorder>,
    player: Option<pv_backend::record::Player>,
    /// At most one Record refresh tick runs (see `spawn_rec_tick`).
    pub rec_tick_live: bool,
    /// Phase-2 video research: input id → pass-1 state. Web videos run a
    /// skip-summary pass 1 (transcript only) first; on Done the transcript
    /// seeds research, then pass 2 runs the full job with `transcript_path`.
    /// Abort clears the map so a late thread can never resurrect a run.
    phase2: HashMap<u64, Phase2Job>,
}

/// One video's phase-2 state across pass 1 (skip-summary transcript),
/// research, and pass 2 (full job with `transcript_path`).
#[derive(Clone, Debug, Default)]
struct Phase2Job {
    /// Display filename (research stem source).
    display: String,
    /// Research stem (matches core reader + notes dir exactly).
    stem: String,
    /// Transcript .txt for pass 2 (written at pass-1 Done; empty before).
    transcript_txt: PathBuf,
}

impl Store {
    pub fn new() -> Self {
        let mut store = Store {
            page: Page::Models,
            input: Vec::new(),
            processing: Vec::new(),
            output: Vec::new(),
            unlocked: false,
            started: false,
            paused: false,
            backend_live: false,
            validating: false,
            verifying_models: false,
            boot_verified: false,
            loading: None,
            view: ViewMode::Large,
            status: String::from("Ready. Add audio files to begin."),
            classes: String::new(),
            queue: Queue::new(None),
            next_id: 1,
            event_tx: None,
            downloader: None,
            downloads: HashMap::new(),
            models: Vec::new(),
            models_dir: String::new(),
            active_stt: String::new(),
            active_llm: String::new(),
            active_vlm: String::new(),
            vlm_expect: false,
            vlm_validated: false,
            run_vlm: None,
            wizard_open: false,
            wizard_tier: "standard".to_string(),
            wizard_detail: String::new(),
            dark_mode: true,
            custom_models_dir: None,
            compute_mode: "auto".to_string(),
            delete_converted: false,
            ollama_scan: Vec::new(),
            file_warnings: HashMap::new(),
            dismissed: HashSet::new(),
            default_outdir: None,
            default_classes: String::new(),
            models_tier: "standard".to_string(),
            review_drafts_dir: None,
            chunk_tokens: 4000,
            summary_tier: "standard".to_string(),
            audio_window_sec: 30,
            vram_budget_pct: 80,
            merge_suggest: 0.35,
            merge_prompt: 0.80,
            merge_mode: "ask".to_string(),
            merge_auto_prompt: true,
            doc_max_chars: 500_000,
            pdf_mode: "warn".to_string(),
            audio_retention: "keep".to_string(),
            denoise_mode: "recommended".to_string(),
            web_research: false,
            attention: 50,
            errors: Vec::new(),
            next_err: 1,
            show_errors: false,
            show_logs: false,
            log_view: Vec::new(),
            review: None,
            renaming: None,
            draft_badges: HashSet::new(),
            merge_selection: HashSet::new(),
            suggested: Vec::new(),
            prompted: Vec::new(),
            dismissed_groups: HashSet::new(),
            display_names: HashMap::new(),
            pending_merged: HashMap::new(),
            rec_status: RecStatus::Empty,
            rec_path: None,
            rec_device: None,
            rec_rate_opt: None,
            rec_rate: 0,
            rec_channels: 0,
            rec_samples: 0,
            rec_peaks: Vec::new(),
            rec_peak_db: "-inf dB".to_string(),
            rec_clips: 0,
            rec_total_secs: 0,
            rec_play_pos: 0,
            rec_sent: false,
            rec_parts: Vec::new(),
            rec_part: 1,
            recorder: None,
            player: None,
            rec_tick_live: false,
            phase2: HashMap::new(),
        };
        store.apply_prefs(pv_backend::prefs::load());
        // Effective overrides from Settings (if pinned); retarget the queue
        // so Start uses them from the first run (Queue was built pre-prefs).
        if let Some(ref d) = store.default_outdir {
            if !d.trim().is_empty() {
                pv_backend::dirs::set_outdir_override(PathBuf::from(d));
            }
        }
        store.queue.set_out_dir(pv_backend::dirs::output_dir());
        if let Some(ref d) = store.review_drafts_dir {
            if !d.trim().is_empty() {
                pv_backend::dirs::set_reviews_dir_override(PathBuf::from(d));
            }
        }
        store.refresh_merge_suggestions();
        store.refresh_draft_badges();
        store.restore_staged();
        // Best-effort boot hygiene (never fatal, never noisy).
        let _ = pv_backend::diag::sweep_empty_dumps();
        store
    }

    /// Re-resolve the persisted staged queue (FIFO order kept). Missing or
    /// unreadable files are dropped with a one-line count note — never an
    /// error dialog. Runs once at boot, after prefs load.
    fn restore_staged(&mut self) {
        let saved = pv_backend::prefs::load().staged;
        if saved.is_empty() {
            return;
        }
        let mut live = Vec::new();
        let mut gone = 0;
        for p in saved.iter().take(256) {
            let pb = PathBuf::from(p);
            if pb.is_file() {
                live.push(pb);
            } else {
                gone += 1;
            }
        }
        if !live.is_empty() {
            self.add_files(live);
        }
        if gone > 0 {
            self.status = format!(
                "{} file(s) staged. ({gone} previously staged file(s) no longer on disk.)",
                self.input.len()
            );
        }
    }

    fn alloc_err(&mut self) -> u64 {
        let id = self.next_err;
        self.next_err += 1;
        id
    }

    /// Push a structured GUI error (Error Center) + mirror to `status`.
    pub fn push_error(&mut self, source: &str, msg: String, detail: String) {
        let id = self.alloc_err();
        let time = format!("{:?}", std::time::SystemTime::now());
        self.status = format!("{msg} — see Errors.");
        self.errors.push(AppError {
            id,
            time,
            source: source.to_string(),
            msg,
            detail,
        });
        if self.errors.len() > 200 {
            let n = self.errors.len() - 200;
            self.errors.drain(0..n);
        }
    }

    pub fn clear_errors(&mut self) {
        self.errors.clear();
        self.status = "Errors cleared.".to_string();
    }

    pub fn dismiss_error(&mut self, id: u64) {
        self.errors.retain(|e| e.id != id);
    }

    fn alloc(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    pub fn file_name(p: &std::path::Path) -> String {
        p.file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("?")
            .to_string()
    }

    /// Add staged files (deduped by path). Single + batch arrive together.
    /// Audio → STT path; modern documents → extraction path. Anything else
    /// is skipped with a `file_warnings` entry + Error Center record.
    /// Single source of truth for the gate (pickers use it too).
    pub fn add_files(&mut self, paths: Vec<PathBuf>) {
        for path in paths {
            if self.input.iter().any(|f| f.path == path) {
                continue;
            }
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.to_lowercase())
                .unwrap_or_default();
            if BAD_EXTS.contains(&ext.as_str()) {
                let reason = format!(
                    "Export as .wav/.mp3/.flac/.opus first — .{} is a legacy Audacity 2 project (XML + _data folder), not raw audio.",
                    ext
                );
                self.file_warnings.insert(path.clone(), reason.clone());
                self.push_error("Input", format!("{}: {reason}", Self::file_name(&path)), String::new());
                continue;
            }
            let kind = if is_video_ext(&ext) {
                InputKind::Video
            } else if AUDIO_EXTS.contains(&ext.as_str()) || PROJECT_EXTS.contains(&ext.as_str()) {
                InputKind::Audio
            } else if DOC_EXTS.contains(&ext.as_str()) {
                InputKind::Doc
            } else {
                let reason = if ext.is_empty() {
                    "No extension — rename with .wav/.mp3/.txt/.pdf/etc.".to_string()
                } else {
                    format!("Unsupported format .{ext} — audio (.wav/.mp3/.flac/.opus) or documents (.txt/.md/.pdf/.docx/.pptx/.xlsx/.odt/.rtf/.html/.epub/.csv) accepted.", ext = ext)
                };
                self.file_warnings.insert(path.clone(), reason.clone());
                self.push_error("Input", format!("{}: {reason}", Self::file_name(&path)), String::new());
                continue;
            };
            let id = self.alloc();
            let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            self.input.push(InputFile {
                id,
                path,
                state: FileState::Queued,
                size,
                kind,
            });
        }
        self.status = format!("{} file(s) staged. Press Start.", self.input.len());
        // Staged documents carry text: re-cluster so similar items surface
        // without waiting for the next completion.
        self.refresh_merge_suggestions();
        self.save_prefs(); // persist the staged queue across restarts
    }

    /// Stage dropped paths (OS drag-and-drop + folders). Directories expand
    /// recursively (symlinked dirs skipped — no cycle risk), capped at 256
    /// files per drop so a stray drive-root drop can't flood the queue.
    /// Everything then flows through the normal `add_files` gates.
    pub fn add_dropped(&mut self, paths: &[PathBuf]) {
        let mut files: Vec<PathBuf> = Vec::new();
        let mut stack: Vec<PathBuf> = paths.to_vec();
        while let Some(p) = stack.pop() {
            if files.len() >= 256 {
                break;
            }
            let Ok(meta) = std::fs::symlink_metadata(&p) else {
                continue;
            };
            if meta.file_type().is_symlink() {
                continue;
            }
            if meta.is_dir() {
                if let Ok(rd) = std::fs::read_dir(&p) {
                    let mut kids: Vec<PathBuf> =
                        rd.flatten().map(|e| e.path()).collect();
                    kids.sort();
                    stack.extend(kids);
                }
                continue;
            }
            if meta.is_file() {
                files.push(p);
            }
        }
        files.sort();
        let n = files.len();
        self.add_files(files);
        if n >= 256 {
            self.status = format!(
                "{} file(s) staged (drop capped at 256). Press Start.",
                self.input.len()
            );
        }
    }

    /// Get warnings for a specific file path, if any.
    pub fn get_warning(&self, path: &PathBuf) -> Option<&str> {
        self.file_warnings.get(path).map(|s| s.as_str())
    }

    /// Manual delete on the Input page — the only place Input entries die
    /// outside of reaching Output.
    pub fn remove_input(&mut self, id: u64) {
        if let Some(f) = self.input.iter().find(|f| f.id == id) {
            self.file_warnings.remove(&f.path);
        }
        self.input.retain(|f| f.id != id);
        self.status = format!("{} file(s) staged.", self.input.len());
        self.save_prefs();
    }

    /// Start: stage the batch, validate (load) both models with visible
    /// progress, then hand the batch to the backend worker.
    /// Returns the event receiver the UI pumps into this store. Validation
    /// runs on a worker thread (model loads take minutes); the batch is
    /// only queued after both backends prove they load+discharge.
    /// Backend absence is an `Err` with an actionable message (no silent sim here).
    /// Output dir + classes come from Settings defaults (no per-run overrides).
    pub fn begin_run(&mut self) -> Result<Receiver<Event>, String> {
        self.begin_run_with(None)
    }

    /// Test/back-compat entry: explicit outdir wins when given, else Settings.
    pub fn begin_run_with(&mut self, outdir: Option<PathBuf>) -> Result<Receiver<Event>, String> {
        if self.validating {
            return Err("Still loading models — one moment.".to_string());
        }
        let queued: Vec<InputFile> = self
            .input
            .iter()
            .filter(|f| f.state == FileState::Queued)
            .cloned()
            .collect();
        if queued.is_empty() {
            return Err("Stage files first with \"+ Add files\".".to_string());
        }
        if !self.queue.live() {
            return Err("backend offline — reinstall or relaunch the app.".to_string());
        }
        if let Some(dir) = outdir.or_else(|| self.run_outdir()) {
            if dir.is_absolute() {
                self.queue.set_out_dir(dir);
            }
        }
        // Settings default classes feed Start when the session box is empty.
        if self.classes.trim().is_empty() && !self.default_classes.trim().is_empty() {
            self.classes = self.default_classes.clone();
        }
        // Boot integrity gate: the background worker owns verification, so
        // Start itself never hashes gigabytes. Only two exits here: block
        // while the worker is in flight, or run the synchronous fallback
        // when no worker ever ran (tests, edge boots).
        if self.verifying_models {
            return Err("Verifying models — one moment.".to_string());
        }
        if !self.boot_verified {
            let (vstt, vllm, vw) = pv_backend::verify::verify_active_boot();
            for w in vw {
                self.push_error("Models", w.clone(), String::new());
                if self.status.starts_with("Ready")
                    || self.status.starts_with("Loading models")
                    || self.status.is_empty()
                {
                    self.status = w;
                }
            }
            // Persist any fallback the verifier selected (it already chose the
            // next complete pinned tier on quarantine).
            if let (Some(s), Some(l)) = (vstt.clone(), vllm.clone()) {
                let _ = pv_backend::models::set_active(&s, &l);
            }
            self.boot_verified = true;
        }
        let (stt, llm) = pv_backend::models::active_model_paths()?;
        for q in &queued {
            if !q.path.is_file() {
                return Err(format!("input not found: {}", q.path.display()));
            }
        }
        let (tx, rx): (Sender<Event>, Receiver<Event>) = std::sync::mpsc::channel();
        let stt = stt.to_string_lossy().into_owned();
        let llm = llm.to_string_lossy().into_owned();
        // A fresh Start owns its phase-2 lifecycle (a late research thread
        // from a previous run can never requeue into this one: entries are
        // keyed per Start and cleared here).
        self.phase2.clear();
        // Run-scoped vision paths for video batches. Missing VLM degrades to
        // transcript+research (never a hard error — vision is additive).
        let batch_has_video = queued.iter().any(|q| q.kind == InputKind::Video);
        self.vlm_expect = false;
        self.vlm_validated = false;
        self.run_vlm = None;
        let vlm_paths: Option<(String, String)> = if batch_has_video {
            match pv_backend::models::active_vlm_paths() {
                Ok((t, m)) => {
                    self.vlm_expect = true;
                    Some((
                        t.to_string_lossy().into_owned(),
                        m.to_string_lossy().into_owned(),
                    ))
                }
                Err(e) => {
                    self.status = format!("{e} — videos run transcript-only.");
                    None
                }
            }
        } else {
            None
        };
        self.run_vlm = vlm_paths
            .clone()
            .map(|(t, m)| (PathBuf::from(t), PathBuf::from(m)));
        // Explicit per-run flag: the backend must never depend on ambient
        // env (Rust set_var is invisible to the CRT getenv cache, which
        // silently ran GPU code on "CPU" runs).
        let cpu_only = self.compute_mode == "cpu";
        // Generous stack: model init (especially Vulkan shader work) is deep,
        // and a guard-page death here would look identical to a backend crash.
        // 64MB reserves address space; only touched pages commit memory.
        // Spawned BEFORE any state mutates: a spawn failure returns Err with
        // nothing to roll back, so the UI can never hang on "Loading…".
        let txc = tx.clone();
        if std::thread::Builder::new()
            .name("pv-validate".to_string())
            .stack_size(64 << 20)
            .spawn(move || {
                // The validator always terminates the phase (ok or error event),
                // and a thread panic must never hang the UI waiting for one.
                let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    pv_backend::queue::Queue::validate_models(
                        &stt,
                        &llm,
                        vlm_paths.clone(),
                        cpu_only,
                        &txc,
                    )
                }));
                if res.is_err() {
                    let _ = txc.send(Event::Validate {
                        stage: "stt".to_string(),
                        done: true,
                        error: "validator crashed".to_string(),
                    });
                }
            })
            .is_err()
        {
            return Err("Could not start validation thread.".to_string());
        }
        for q in &queued {
            if let Some(inp) = self.input.iter_mut().find(|f| f.id == q.id) {
                inp.state = FileState::Active;
            }
            if !self.processing.iter().any(|p| p.id == q.id) {
                self.processing.push(ProcFile {
                    id: q.id,
                    path: q.path.clone(),
                    name: Self::file_name(&q.path),
                    stt: 0.0,
                    sum: 0.0,
                    stage: "loading".to_string(),
                    msg: "waiting for models".to_string(),
                    selected: false,
                    skipped: false,
                });
            }
        }
        self.started = true;
        self.unlocked = true;
        self.backend_live = true;
        self.validating = true;
        // The mode tag travels into the UI so nobody has to guess (or go
        // log-diving to verify) which backend path is actually executing.
        let tag = self.compute_tag();
        self.loading = Some(format!("Loading STT model ({tag})…"));
        self.status = "Loading models before the run…".to_string();
        self.event_tx = Some(tx);
        Ok(rx)
    }

    /// Short mode tag for loading messages ("CPU"/"GPU").
    fn compute_tag(&self) -> &'static str {
        if self.compute_mode == "cpu" {
            "CPU"
        } else {
            "GPU"
        }
    }

    /// True when a video's research notes are absent (needs a fetch pass).
    /// Stem rules mirror both the Rust writer and the core reader.
    fn research_notes_missing(display: &str) -> bool {
        let stem = std::path::Path::new(display)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(display);
        !pv_backend::research::notes_dir_for(stem)
            .join("notes.md")
            .exists()
    }

    /// Other merge-selected inputs beside `id` (user-declared similarity).
    /// Pure view of selection + staged files.
    fn declared_peers(&self, id: u64) -> Vec<String> {
        self.merge_selection
            .iter()
            .filter(|sid| **sid != id)
            .filter_map(|sid| self.input.iter().find(|f| f.id == *sid))
            .map(|f| Self::file_name(&f.path))
            .collect()
    }

    /// Write the declared-similar sidecar for a video (`declared.md` next
    /// to `notes.md`): user-declared pairs are confirmed similar — the core
    /// compares them directly (still citing timestamps), unlike the loose
    /// AI-proposed `## Related` matches. No-op without peers. Best-effort.
    fn write_declared_sidecar(&self, display: &str, peers: &[String]) {
        if peers.is_empty() {
            return;
        }
        let stem = std::path::Path::new(display)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(display);
        let dir = pv_backend::research::notes_dir_for(stem);
        if std::fs::create_dir_all(&dir).is_err() {
            return;
        }
        let mut doc = String::from(
            "# Declared similar (user selection — confirmed, compare directly)\n\n",
        );
        for p in peers {
            doc.push_str(&format!("- {p} (user-declared similar; confirm shared claims against timestamps)\n"));
        }
        let _ = std::fs::write(dir.join("declared.md"), doc);
    }

    /// Temp dir for phase-1 (skip-summary) video transcripts. Swept by Clean
    /// caches; entries are keyed per Start so strays never confuse a run.
    fn phase1_dir() -> PathBuf {
        pv_backend::dirs::data_dir().join("phase1")
    }

    /// Temp dir for pass-2 transcript payloads (the core reads them at run
    /// time, so they live until pass 2 completes).
    fn transcript_dir() -> PathBuf {
        pv_backend::dirs::data_dir().join("transcript")
    }

    /// Research stem for a display filename. MUST match the core
    /// `research_stem_for` and `notes_dir_for` exactly (sanitize → trim →
    /// 60 chars), or the core will never find the notes.
    fn research_stem(display: &str) -> String {
        let stem = std::path::Path::new(display)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(display);
        let dir = pv_backend::research::notes_dir_for(stem);
        dir.file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(stem)
            .to_string()
    }

    /// Fetch cited sources for ONE video on a worker thread (never the UI
    /// thread), grounded in its pass-1 transcript, then queue pass 2 via the
    /// Research event. Best-effort end to end: zero notes is a normal
    /// offline degrade, not an error.
    fn launch_phase2_research(&mut self, pid: u64, transcript: String) {
        let tx = match self.pump_tx() {
            Ok(tx) => tx,
            Err(_) => {
                // No event channel (broken run): unwind to a clean retryable
                // state instead of wedging a processing entry with no owner.
                self.phase2.remove(&pid);
                self.processing.retain(|p| p.id != pid);
                if let Some(inp) = self.input.iter_mut().find(|f| f.id == pid) {
                    inp.state = FileState::Queued;
                }
                self.check_drained();
                return;
            }
        };
        let attention = self.attention;
        let classes = self.run_classes();
        let entry = match self.phase2.get(&pid) {
            Some(e) => e.clone(),
            None => return,
        };
        // Terms from the transcript head (first ~1500 chars): real film
        // vocabulary beats filename guesses. Falls back to display words.
        let head: String = transcript.chars().take(1500).collect();
        let terms =
            pv_backend::research::query_terms(&format!("{head} {classes}"), 6).join(" ");
        let topic = if terms.trim().is_empty() {
            entry
                .display
                .replace(['_', '-', '.'], " ")
        } else {
            terms
        };
        let stem = entry.stem.clone();
        self.status = format!("Researching cited sources for {}…", entry.display);
        let _ = std::thread::Builder::new()
            .name("pv-research".to_string())
            .spawn(move || {
                let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    pv_backend::research::research_blocking(&topic, &stem, attention)
                }));
                let _ = tx.send(Event::Research {
                    done: true,
                    notes: res.unwrap_or(0),
                    input: pid,
                });
            });
    }
    /// queue the staged files and launch the FIFO worker on the live channel.
    /// Documents are extracted to a UTF-8 cache first and queued with
    /// `is_text` so the core skips decode+STT and summarizes directly.
    /// Video+web jobs whose notes are missing are held back first: a worker
    /// thread researches (filenames+classes as queries, v1) while the rest
    /// of the batch runs, then they queue on the Research event.
    fn continue_validated_run(&mut self) -> Result<usize, String> {
        let mut queued: Vec<InputFile> = self
            .input
            .iter()
            .filter(|f| f.state == FileState::Active)
            .cloned()
            .collect();
        if queued.is_empty() {
            return Err("Nothing left to run.".to_string());
        }
        // Phase-2 video research: web videos lacking notes run a
        // skip-summary pass 1 (transcript only) into a temp dir first. On
        // Done the transcript seeds research with real film terms, then
        // pass 2 runs the full job with `transcript_path`.
        let phase1: Vec<InputFile> = if self.web_research {
            queued
                .iter()
                .filter(|f| {
                    f.kind == InputKind::Video
                        && Self::research_notes_missing(&Self::file_name(&f.path))
                })
                .cloned()
                .collect()
        } else {
            Vec::new()
        };
        if !phase1.is_empty() {
            queued.retain(|f| phase1.iter().all(|h| h.id != f.id));
        }
        let (stt, llm) = pv_backend::models::active_model_paths()?;
        let classes = self.run_classes();
        let db = pv_backend::dirs::db_path().to_string_lossy().into_owned();
        let cpu_only = self.compute_mode == "cpu";
        let delete_converted = self.delete_converted;
        let (chunk_tokens, audio_window_sec, vram_budget_pct) =
            (self.chunk_tokens, self.audio_window_sec, self.vram_budget_pct);
        let summary_tier = self.summary_tier.clone();
        let audio_retention = self.audio_retention.clone();
        let denoise_mode = self.denoise_mode.clone();
        let mut paths: Vec<String> = Vec::with_capacity(queued.len());
        let mut flags: Vec<bool> = Vec::with_capacity(queued.len());
        let mut displays: Vec<String> = Vec::with_capacity(queued.len());
        let mut id_paths: Vec<(u64, String)> = Vec::with_capacity(queued.len());
        let mut video_flags: Vec<bool> = Vec::with_capacity(queued.len());
        for q in &queued {
            if q.kind == InputKind::Doc {
                match self.doc_cache_path(q) {
                    Ok(cache) => {
                        let disp = Self::file_name(&q.path);
                        self.display_names.insert(cache.clone(), disp.clone());
                        paths.push(cache.clone());
                        flags.push(true);
                        video_flags.push(false);
                        displays.push(disp);
                        id_paths.push((q.id, cache));
                    }
                    Err(e) => {
                        // Extraction failure retires this file, others proceed.
                        self.processing.retain(|p| p.id != q.id);
                        if let Some(inp) = self.input.iter_mut().find(|f| f.id == q.id) {
                            inp.state = FileState::Queued;
                        }
                        let msg = format!("extract failed ({e})");
                        self.file_warnings.insert(q.path.clone(), format!("{msg} — fix or remove, then Start again"));
                        self.push_error("Documents", format!("{}: {msg}", Self::file_name(&q.path)), String::new());
                    }
                }
            } else {
                let p = q.path.to_string_lossy().into_owned();
                paths.push(p.clone());
                flags.push(false);
                // Video jobs ride the audio path (ffmpeg extracts the track)
                // with timestamped STT + frame sampling downstream.
                video_flags.push(q.kind == InputKind::Video);
                displays.push(Self::file_name(&q.path));
                id_paths.push((q.id, p));
            }
        }
        // Phase-1 batch vecs: skip-summary transcript jobs into the temp dir
        // (videos only — no doc branch). Entries register here so pass-1
        // Done events route to research instead of Output.
        let mut p1paths: Vec<String> = Vec::new();
        let mut p1flags: Vec<bool> = Vec::new();
        let mut p1video: Vec<bool> = Vec::new();
        let mut p1displays: Vec<String> = Vec::new();
        if !phase1.is_empty() {
            for q in &phase1 {
                let p = q.path.to_string_lossy().into_owned();
                let display = Self::file_name(&q.path);
                p1paths.push(p.clone());
                p1flags.push(false);
                p1video.push(true);
                p1displays.push(display.clone());
                id_paths.push((q.id, p.clone()));
                self.phase2.insert(
                    q.id,
                    Phase2Job {
                        display: display.clone(),
                        stem: Self::research_stem(&display),
                        transcript_txt: PathBuf::new(),
                    },
                );
                // Declared-similar sidecar now (selection may change later;
                // pass 2 refreshes it again before queueing).
                let peers = self.declared_peers(q.id);
                self.write_declared_sidecar(&display, &peers);
            }
        }
        // Declared-similar sidecars for direct-full videos (phase-1 batch
        // handled above at registration).
        for q in &queued {
            if q.kind == InputKind::Video {
                let peers = self.declared_peers(q.id);
                self.write_declared_sidecar(&Self::file_name(&q.path), &peers);
            }
        }
        // Sync processing paths to the queued (cache) paths so events match.
        for (id, qp) in &id_paths {
            if let Some(p) = self.processing.iter_mut().find(|p| p.id == *id) {
                p.path = PathBuf::from(qp);
            }
        }
        if paths.is_empty() && p1paths.is_empty() {
            return Err("Nothing extractable — fix document errors, then Start again.".to_string());
        }
        let web_research = self.web_research;
        let attention = self.attention;
        let (vlm_text, vlm_mmproj) = match &self.run_vlm {
            Some((t, m)) => (
                t.to_string_lossy().into_owned(),
                m.to_string_lossy().into_owned(),
            ),
            None => (String::new(), String::new()),
        };
        let n = if paths.is_empty() {
            0
        } else {
            self.queue.queue_files(
                &paths,
                &flags,
                &video_flags,
                &displays,
                &stt.to_string_lossy(),
                &llm.to_string_lossy(),
                &classes,
                &db,
                cpu_only,
                delete_converted,
                chunk_tokens,
                audio_window_sec,
                vram_budget_pct,
                &summary_tier,
                &audio_retention,
                &denoise_mode,
                web_research,
                attention,
                &vlm_text,
                &vlm_mmproj,
                "",
                false,
                None,
            )?
        };
        // Phase-1 videos queue skip-summary into the temp dir (same worker,
        // same channel — one run call below drives both batches).
        let mut n2 = 0;
        if !p1paths.is_empty() {
            let dir = Self::phase1_dir();
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            n2 = self.queue.queue_files(
                &p1paths,
                &p1flags,
                &p1video,
                &p1displays,
                &stt.to_string_lossy(),
                &llm.to_string_lossy(),
                &classes,
                &db,
                cpu_only,
                delete_converted,
                chunk_tokens,
                audio_window_sec,
                vram_budget_pct,
                &summary_tier,
                &audio_retention,
                &denoise_mode,
                web_research,
                attention,
                &vlm_text,
                &vlm_mmproj,
                "",
                true,
                Some(dir.as_path()),
            )?;
        }
        let tx = self.pump_tx()?;
        self.queue.run(tx)?;
        Ok(n + n2)
    }

    /// Extract a staged document to the UTF-8 cache; returns the cache path.
    fn doc_cache_path(&self, q: &InputFile) -> Result<String, String> {
        let ext = q
            .path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_string();
        let ex = pv_backend::docs::extract_text(&q.path, self.doc_max_chars.max(0) as usize)
            .map_err(|e| {
                if ext.eq_ignore_ascii_case("pdf") && self.pdf_mode == "shell" {
                    format!("{e} (PDF shell kept — attach manually)")
                } else {
                    e
                }
            })?;
        let base = pv_backend::dirs::data_dir().join("doc_cache");
        std::fs::create_dir_all(&base).map_err(|e| e.to_string())?;
        let stem = q
            .path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("doc");
        let safe: String = stem
            .chars()
            .map(|c| {
                if matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*')
                    || c.is_control()
                {
                    '-'
                } else {
                    c
                }
            })
            .take(60)
            .collect();
        let cache = base.join(format!("{safe}__{}.txt", q.id));
        std::fs::write(&cache, ex.text).map_err(|e| e.to_string())?;
        Ok(cache.to_string_lossy().into_owned())
    }

    /// Display name for a queued path (original filename for doc caches).
    fn display_for(&self, queued: &str) -> String {
        if let Some(n) = self.display_names.get(queued) {
            return n.clone();
        }
        PathBuf::from(queued)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(queued)
            .to_string()
    }

    /// Tear down a pending run (validation failed, worker refused to start):
    /// files back to Input, flags re-armed, caller-supplied message kept.
    fn cancel_pending_run(&mut self, msg: String) {
        self.abort_all();
        self.started = false;
        self.backend_live = false;
        self.validating = false;
        self.loading = None;
        self.status = msg;
    }

    /// Remove from processing → back to Input (X marker / Abort semantics).
    /// Returns the backend removal result for the UI to report.
    /// M3 wires this to the per-card X and bulk Abort.
    /// Always clears a held pause: removing the runner means "move on".
    pub fn remove_processing(&mut self, id: u64, active: bool) -> Result<(), String> {
        if self.processing.iter().find(|x| x.id == id).is_none() {
            return Err("file not in processing".to_string());
        }
        if let Some(p) = self.processing.iter().find(|x| x.id == id).cloned() {
            let path = p.path.to_string_lossy().into_owned();
            let _ = self.queue.remove(&path, active);
            if let Some(inp) = self.input.iter_mut().find(|x| x.id == id) {
                inp.state = FileState::Queued;
            }
        }
        // A removed phase-2 video restarts clean (transcript re-derived).
        self.phase2.remove(&id);
        self.processing.retain(|x| x.id != id);
        self.paused = false;
        self.check_drained();
        Ok(())
    }

    /// Fold one backend event into the store. Path-keyed: indices never shift.
    pub fn apply_event(&mut self, ev: Event) {
        match ev {
            Event::File {
                file,
                stage,
                fraction,
                message,
                aborted,
            } => {
                let path = PathBuf::from(&file);
                // Display label prefers the original filename for doc caches.
                let label = self.display_for(&file);
                // Retire by processing id (doc caches differ from input paths).
                let finished_id = self.processing.iter().find(|p| p.path == path).map(|p| p.id);
                if aborted {
                    self.processing.retain(|p| p.path != path);
                    if let Some(id) = finished_id {
                        self.phase2.remove(&id);
                        if let Some(inp) = self.input.iter_mut().find(|x| x.id == id) {
                            inp.state = FileState::Queued;
                        }
                    } else if let Some(inp) = self.input.iter_mut().find(|x| x.path == path) {
                        inp.state = FileState::Queued;
                    }
                    self.status = format!("{label} returned to Input (aborted).");
                    self.check_drained();
                    return;
                }
                match stage {
                    Stage::Done => self.complete_file(&path, &message),
                    Stage::Error => {
                        // A failed file must not wedge the queue: retire it
                        // to Input as retryable and let the drain proceed.
                        // Specific backend message is preserved (was generic).
                        let detail = if message.trim().is_empty() || message == "file failed, continuing FIFO" {
                            "backend reported failure; see backend.log (Settings → Diagnostics)"
                        } else {
                            message.as_str()
                        };
                        self.processing.retain(|x| x.path != path);
                        if let Some(id) = finished_id {
                            // Failed phase-2 passes restart clean (transcript
                            // re-derived on the next pass 1).
                            self.phase2.remove(&id);
                            if let Some(inp) = self.input.iter_mut().find(|x| x.id == id) {
                                inp.state = FileState::Queued;
                            }
                        } else if let Some(inp) = self.input.iter_mut().find(|x| x.path == path) {
                            inp.state = FileState::Queued;
                        }
                        self.file_warnings.insert(
                            path.clone(),
                            format!("failed ({detail}) — press Start to retry"),
                        );
                        self.push_error(
                            "Pipeline",
                            format!("{label} failed ({detail}) — returned to Input."),
                            detail.to_string(),
                        );
                        self.check_drained();
                    }
                    _ => {
                        let pct = (fraction.clamp(0.0, 1.0) * 100.0).round();
                        if let Some(p) = self.processing.iter_mut().find(|x| x.path == path) {
                            if stage.summary_column() {
                                p.sum = pct;
                            } else {
                                p.stt = pct;
                            }
                            p.stage = stage_label(stage).to_string();
                            p.msg = message.clone();
                        }
                        self.status = format!(
                            "{}: {} {pct}% — {message}",
                            Self::file_name(&path),
                            stage_label(stage)
                        );
                    }
                }
            }
            Event::Download {
                id,
                downloaded,
                total,
                done,
                error,
            } => {
                let entry = self.downloads.entry(id.clone()).or_default();
                entry.downloaded = downloaded;
                entry.total = total;
                entry.error = error.clone();
                if done {
                    entry.done = true;
                    self.refresh_models();
                    self.maybe_activate_completed_tier(&id);
                    // Wizard pair completing closes the wizard (M4 flow).
                    if self.wizard_open && self.models_ready() {
                        self.wizard_open = false;
                        self.status = "Models ready. Add audio files to begin.".to_string();
                    }
                } else if !error.is_empty() {
                    self.push_error("Download", format!("Download failed ({id}): {error}"), error.clone());
                }
            }
            Event::Validate { stage, done, error } => {
                // Late validation events after an abort are stale: the queue
                // is already idle and relocked, so they must not resurrect it.
                // Fragile coupling — correct only because abort_all always
                // clears processing AND relocks together (check_drained). Any
                // future path that clears one without the other must revisit
                // this guard.
                if self.processing.is_empty() && !self.unlocked {
                    return;
                }
                if !done {
                    let tag = self.compute_tag();
                    let label = if stage == "stt" {
                        format!("Loading STT model ({tag})…")
                    } else if stage == "vlm" {
                        format!("Loading vision model ({tag})…")
                    } else {
                        format!("Loading LLM ({tag})…")
                    };
                    self.loading = Some(label.clone());
                    self.status = label;
                    return;
                }
                self.validating = false;
                if !error.is_empty() {
                    // Report the mode that actually ran (compute_mode is the
                    // source of truth — never hardcode it).
                    let ran = self.compute_tag();
                    if stage == "stt" && self.compute_mode != "cpu" {
                        // GPU load failed cleanly: move to CPU now and invite
                        // a retry, instead of marching into a known-bad run.
                        self.set_compute_mode("cpu");
                        let msg = format!(
                            "STT failed on {ran} ({error}) — switched to CPU mode, press Start to retry."
                        );
                        self.push_error("Models", msg.clone(), error.clone());
                        self.cancel_pending_run(msg);
                    } else if stage == "stt" {
                        let msg = format!(
                            "STT failed on {ran} ({error}) — press Start to retry."
                        );
                        self.push_error("Models", msg.clone(), error.clone());
                        self.cancel_pending_run(msg);
                    } else if stage == "vlm" {
                        // Vision is additive: degrade to transcript+research
                        // and run anyway (the core would do the same per file).
                        self.run_vlm = None;
                        self.vlm_validated = true;
                        self.push_error(
                            "Models",
                            format!("Vision model failed ({error}) — videos run transcript-only."),
                            error.clone(),
                        );
                        self.loading = None;
                        match self.continue_validated_run() {
                            Ok(n) => {
                                self.status = format!("Queued {n} file(s), processing FIFO…");
                            }
                            Err(e) => self.cancel_pending_run(e),
                        }
                    } else {
                        let msg = format!("LLM validation failed ({error}).");
                        self.push_error("Models", msg.clone(), error.clone());
                        self.cancel_pending_run(msg);
                    }
                    return;
                }
                if stage == "llm" {
                    // Both backends proven: hand the staged batch to the worker —
                    // unless a vision phase is still ahead for this run.
                    if self.vlm_expect && !self.vlm_validated {
                        self.loading = Some(format!("Loading vision model ({})…", self.compute_tag()));
                        self.status = "Loading vision model before the run…".to_string();
                        return;
                    }
                    self.loading = None;
                    match self.continue_validated_run() {
                        Ok(n) => {
                            self.status = format!("Queued {n} file(s), processing FIFO…");
                            if let Some(w) = self.chunk_model_note() {
                                self.status.push_str(&format!(" NOTE: {w}"));
                            }
                        }
                        Err(e) => self.cancel_pending_run(e),
                    }
                } else if stage == "vlm" {
                    // Vision proven (or skipped): same handoff as llm-done.
                    self.vlm_validated = true;
                    self.loading = None;
                    match self.continue_validated_run() {
                        Ok(n) => {
                            self.status = format!("Queued {n} file(s), processing FIFO…");
                            if let Some(w) = self.chunk_model_note() {
                                self.status.push_str(&format!(" NOTE: {w}"));
                            }
                        }
                        Err(e) => self.cancel_pending_run(e),
                    }
                }
            }
            Event::Research { done, notes, input } => {
                // Late thread from an aborted/removed run: no phase-2 entry
                // means no-op by construction (never resurrects).
                if !done {
                    return;
                }
                if !self.phase2.contains_key(&input) {
                    return;
                }
                match self.queue_pass2(input) {
                    Ok(()) => {
                        self.status = format!(
                            "Research done ({notes} source(s)) — full pass queued, processing FIFO…"
                        );
                    }
                    Err(e) => {
                        // Degrade: video back to Input for a plain retry.
                        self.phase2.remove(&input);
                        self.processing.retain(|p| p.id != input);
                        if let Some(inp) = self.input.iter_mut().find(|f| f.id == input) {
                            inp.state = FileState::Queued;
                        }
                        self.push_error("Research", format!("Pass 2 failed ({e}) — returned to Input."), e);
                        self.check_drained();
                    }
                }
            }
            Event::BootVerified { warnings } => {
                // Once-per-process integrity pass landed. Start presses
                // during the pass got "Verifying models"; from here the
                // manifest fast-path rules and no synchronous hashing happens.
                self.verifying_models = false;
                self.boot_verified = true;
                self.refresh_models();
                if warnings.is_empty() {
                    if self.status.starts_with("Verifying models") {
                        self.status = "Models verified.".to_string();
                    }
                    return;
                }
                for w in warnings {
                    self.push_error("Models", w, String::new());
                }
            }
            Event::UpdateCheck { message } => {
                self.status = message;
            }
            Event::LinkVerified { id, ok, message } => {
                // The worker already updated the manifest (verified flag or
                // dropped record with cleared slots) — mirror it locally.
                self.refresh_models();
                if ok {
                    // A re-link race may have replaced this id meanwhile;
                    // only announce when the verified record is still ours.
                    let dir = pv_backend::dirs::models_dir();
                    let man = pv_backend::manifest::read(&dir);
                    if man.files.contains_key(&id) {
                        self.status = message;
                    }
                } else {
                    self.push_error("Models", format!("Ollama link failed: {message}"), id);
                }
            }
        }
    }

    /// Queue pass 2 for a phase-2 video: full job with `transcript_path`
    /// (core skips STT; research notes are already on disk). The processing
    /// entry survives from pass 1 (same video path), so progress just
    /// continues and pass-2 Done lands in the normal Output flow.
    fn queue_pass2(&mut self, pid: u64) -> Result<(), String> {
        let entry = self
            .phase2
            .get(&pid)
            .cloned()
            .ok_or_else(|| "phase-2 entry gone".to_string())?;
        let inp = self
            .input
            .iter()
            .find(|f| f.id == pid)
            .cloned()
            .ok_or_else(|| "staged file gone".to_string())?;
        let (stt, llm) = pv_backend::models::active_model_paths()?;
        let classes = self.run_classes();
        let db = pv_backend::dirs::db_path().to_string_lossy().into_owned();
        let tpath = entry.transcript_txt.to_string_lossy().into_owned();
        let (vlm_text, vlm_mmproj) = match &self.run_vlm {
            Some((t, m)) => (
                t.to_string_lossy().into_owned(),
                m.to_string_lossy().into_owned(),
            ),
            None => (String::new(), String::new()),
        };
        let video = inp.path.to_string_lossy().into_owned();
        // Refresh the declared-similar sidecar (selection may have changed
        // since pass 1) before the full job reads it.
        let peers = self.declared_peers(pid);
        self.write_declared_sidecar(&entry.display, &peers);
        self.queue.queue_files(
            &[video],
            &[false],
            &[true],
            &[entry.display.clone()],
            &stt.to_string_lossy(),
            &llm.to_string_lossy(),
            &classes,
            &db,
            self.compute_mode == "cpu",
            self.delete_converted,
            self.chunk_tokens,
            self.audio_window_sec,
            self.vram_budget_pct,
            &self.summary_tier.clone(),
            &self.audio_retention.clone(),
            &self.denoise_mode.clone(),
            self.web_research,
            self.attention,
            &vlm_text,
            &vlm_mmproj,
            &tpath,
            false,
            None,
        )?;
        if let Some(p) = self.processing.iter_mut().find(|p| p.id == pid) {
            p.stage = "loading".to_string();
            p.msg = "full pass queued".to_string();
        }
        let tx = self.pump_tx()?;
        self.queue.run(tx)?;
        Ok(())
    }

    /// Pass-1 Done intercept: transcript-only md in the temp dir. Extracts
    /// the transcript, writes the pass-2 payload, launches transcript-
    /// grounded research, and keeps the processing entry alive (same video
    /// path re-queues for pass 2 — progress continues, no Output entry).
    fn complete_phase1(&mut self, pid: u64, md_path: &str) {
        let entry = match self.phase2.get(&pid).cloned() {
            Some(e) => e,
            None => return,
        };
        let md_text = std::fs::read_to_string(md_path).unwrap_or_default();
        let (raw, _, _) = parse_md(&md_text);
        let raw = raw.trim().to_string();
        if raw.is_empty() {
            // Nothing to research from: run the full job now with an empty
            // transcript payload (core transcribes; research degrades to
            // film-only). The phase-2 entry survives for pass-2 cleanup.
            self.push_error(
                "Research",
                format!(
                    "{} transcribed empty — running the full pass without research.",
                    entry.display
                ),
                String::new(),
            );
            if self.queue_pass2(pid).is_err() {
                self.phase2.remove(&pid);
            }
            let _ = std::fs::remove_file(md_path);
            return;
        }
        // Transcript payload for pass 2 (core reads at run time).
        let tdir = Self::transcript_dir();
        let txt = tdir.join(format!("transcript__{}.txt", pid));
        if std::fs::create_dir_all(&tdir).is_err() || std::fs::write(&txt, &raw).is_err() {
            self.push_error(
                "Research",
                format!("{}: cannot stage transcript — full pass without research.", entry.display),
                String::new(),
            );
            self.phase2.remove(&pid);
            let _ = self.queue_pass2(pid);
            let _ = std::fs::remove_file(md_path);
            return;
        }
        if let Some(e) = self.phase2.get_mut(&pid) {
            e.transcript_txt = txt;
        }
        if let Some(p) = self.processing.iter_mut().find(|p| p.id == pid) {
            p.stage = "research".to_string();
            p.msg = "researching from transcript…".to_string();
        }
        let _ = std::fs::remove_file(md_path);
        self.status = format!("Researching cited sources for {}…", entry.display);
        self.launch_phase2_research(pid, raw);
    }

    /// A freshly completed download promotes its tier to the default pair —
    /// but never mid-run (that would yank the active pair underneath a live
    /// job). While processing, completion only posts a switch hint.
    /// Linked models resolve to no catalog pair and stay manual.
    fn maybe_activate_completed_tier(&mut self, id: &str) {
        let tier = match self.models.iter().find(|m| m.id == id) {
            Some(m) => m.tier.clone(),
            None => return,
        };
        let Some((s, l, _)) = Self::tier_info(&tier) else {
            return;
        };
        let ready = self.models.iter().any(|m| m.id == s && m.complete)
            && self.models.iter().any(|m| m.id == l && m.complete);
        if !ready {
            return;
        }
        if self.active_stt == s && self.active_llm == l {
            return;
        }
        if self.processing.is_empty() {
            self.set_active_pair(&s, &l);
            let label = match tier.as_str() {
                "lite" => "Lite",
                "standard" => "Medium",
                "full" => "Large",
                _ => tier.as_str(),
            };
            self.status = format!("{label} pair downloaded — now the default pair.");
        } else {
            self.status = format!("Tier pair downloaded — switch with its Use-pair button.");
        }
    }

    /// DONE: read the finished `.md` (core's AI filename wins), snapshot it to
    /// Output, and only then clear the Input entry. An unreadable finish is
    /// NOT snapshotted as an empty ghost — the file retires to Input as
    /// retryable, exactly like a backend error. Merged jobs get their
    /// Sources-on-top rewrite before snapshotting.
    fn complete_file(&mut self, path: &PathBuf, md_path: &str) {
        let finished_id = self.processing.iter().find(|p| p.path == *path).map(|p| p.id);
        // Phase-1 Done (temp dir): route to transcript research, never to
        // Output. Component-wise prefix match — no string-prefix traps.
        if let Some(pid) = finished_id {
            if self.phase2.contains_key(&pid)
                && PathBuf::from(md_path).starts_with(Self::phase1_dir())
            {
                return self.complete_phase1(pid, md_path);
            }
        }
        let label = self.display_for(&path.to_string_lossy().into_owned());
        let mut name = PathBuf::from(md_path)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("output.md")
            .to_string();
        let mut md_text = match self.queue.output_read(&name) {
            Ok(md) => md,
            Err(e) => {
                self.push_error(
                    "Output",
                    format!("Done but unreadable ({label}): {e} — returned to Input."),
                    e.to_string(),
                );
                self.processing.retain(|p| p.path != *path);
                if let Some(id) = finished_id {
                    if let Some(inp) = self.input.iter_mut().find(|f| f.id == id) {
                        inp.state = FileState::Queued;
                    }
                }
                self.file_warnings.insert(
                    path.clone(),
                    format!("finished file unreadable ({e}) — press Start to retry"),
                );
                self.check_drained();
                return;
            }
        };
        // Merged job? Rewrite with Sources-on-top layout before snapshotting.
        // Parsed ONCE below (after the rewrite) — no double parse.
        let path_s = path.to_string_lossy().into_owned();
        if let Some(members) = self.pending_merged.remove(&path_s) {
            let (_, summary0, _) = parse_md(&md_text);
            let items: Vec<pv_backend::merge::MergeItem> = members
                .into_iter()
                .map(|(mname, mkind, mtext)| pv_backend::merge::MergeItem {
                    id: mname.clone(),
                    name: mname,
                    kind: mkind,
                    text: mtext,
                })
                .collect();
            let core_stem = Self::file_name(&PathBuf::from(&name))
                .trim_end_matches(".md")
                .to_string();
            // The core title already starts with the display label
            // ("Merged (N sources) - …") — strip any existing merged prefix
            // so the final name never reads "Merged - Merged …".
            let mut rest = core_stem.as_str();
            if let Some(s) = rest.strip_prefix("Merged") {
                rest = s.trim_start_matches([' ', '-']);
                // Drop the "(N sources)" segment when present.
                if rest.starts_with('(') {
                    if let Some(end) = rest.find(')') {
                        rest = rest[end + 1..].trim_start_matches([' ', '-']);
                    }
                }
                // Also collapse a literal "Merged - " prefix.
                if rest.is_empty() {
                    rest = "untitled";
                }
            }
            let title = format!("Merged - {rest}");
            md_text = pv_backend::merge::render_merged_md(&title, &items, &summary0);
            // Write back under a merged filename so singles never collide.
            // IO failures are reported, not swallowed: a failed write means
            // the on-disk file is missing/stale even though the snapshot
            // below keeps the session working.
            let merged_name = format!("{}.md", title.replace(['<', '>', ':', '"', '/', '\\', '|', '?', '*'], "-"));
            if let Err(e) = self.queue.output_write(&merged_name, &md_text) {
                self.push_error("Output", format!("Merged file write failed ({e}) — snapshot kept in memory."), e.to_string());
            }
            if let Err(e) = self.queue.output_remove(&name) {
                self.push_error("Output", format!("Stale single file kept on disk ({e}) — Refresh may show both."), e.to_string());
            }
            name = merged_name;
        }
        let stem = name.trim_end_matches(".md").to_string();
        let (raw, summary, skipped) = parse_md(&md_text);
        let id = self.alloc();
        // Coverage sidecar lookup needs the md name before the move below.
        let md_name = name.clone();
        self.output.push(OutFile {
            id,
            name: stem,
            md_name: name,
            dir: self.queue.out_dir().to_path_buf(),
            raw,
            summary,
            skipped,
        });
        self.display_names.remove(&path_s);
        self.processing.retain(|p| p.path != *path);
        if let Some(fid) = finished_id {
            // Pass-2 Done: temp transcript payload served its purpose.
            if let Some(entry) = self.phase2.remove(&fid) {
                if !entry.transcript_txt.as_os_str().is_empty() {
                    let _ = std::fs::remove_file(&entry.transcript_txt);
                }
            }
            self.input.retain(|f| f.id != fid);
        } else {
            self.input.retain(|f| f.path != *path);
        }
        // Coverage accounting: the core records portion stats in the sidecar
        // (`{"coverage":{...}}`). Surfaced per file in the GUI status (the
        // recommended option) — never silent degradation.
        let label = Self::file_name(&PathBuf::from(&md_name));
        let mut cover_note = String::new();
        if let Ok(side) = self.queue.output_read(&format!("{md_name}.json")) {
            let chunks = sidecar_int(&side, "chunks");
            let fallbacks = sidecar_int(&side, "fallbacks").unwrap_or(0);
            let ratio = sidecar_int(&side, "achieved_pct");
            let relaxed = side.contains("\"relaxed\":true");
            if let (Some(c), Some(r)) = (chunks, ratio) {
                let good = (c - fallbacks).max(0);
                cover_note = format!(
                    " — {good}/{c} portions fully summarized, {r}% of source{}{}",
                    if relaxed { " (repetition relaxed)" } else { "" },
                    match self.chunk_model_note() {
                        Some(w) => format!(" NOTE: {w}"),
                        None => String::new(),
                    },
                );
            }
        }
        self.refresh_merge_suggestions();
        self.check_drained();
        if !self.processing.is_empty() {
            self.status = format!("{label} finished → Output ({} total){cover_note}.", self.output.len());
        } else if !cover_note.is_empty() {
            self.status = format!("{label} finished{cover_note}.");
        }
    }

    /// Processing stays open until completely empty; emptied while viewing
    /// kicks back to Input and re-locks. The run flags re-arm here too, so
    /// the pipeline is immediately ready for the next Start.
    fn check_drained(&mut self) {
        // Phase-2 videos keep their processing entries across pass 1, the
        // research thread, and pass 2 — so an empty queue here genuinely
        // means drained. No guard needed.
        if self.processing.is_empty() && self.unlocked {
            self.unlocked = false;
            self.started = false;
            self.backend_live = false;
            self.paused = false;
            self.validating = false;
            self.loading = None;
            self.page = Page::Input;
            self.status = "Processing complete — queue empty.".to_string();
        }
    }

    /// position 0 is the runner while the backend is live (FIFO order is
    /// mirrored 1:1 because removals go through the backend too).
    fn is_active(&self, id: u64) -> bool {
        self.backend_live && self.processing.first().map(|p| p.id) == Some(id)
    }

    fn pump_tx(&self) -> Result<Sender<Event>, String> {
        self.event_tx
            .clone()
            .ok_or_else(|| "run channel gone — press Start again".to_string())
    }

    /// Retry: clear bars, reload model, force restart (backend re-queues).
    pub fn retry_file(&mut self, id: u64) -> Result<(), String> {
        if !self.backend_live {
            return Err("backend offline — cannot retry".to_string());
        }
        let pos = self
            .processing
            .iter()
            .position(|p| p.id == id)
            .ok_or("file not in processing")?;
        let active = self.is_active(id);
        let path = self.processing[pos].path.clone();
        {
            let p = &mut self.processing[pos];
            p.stt = 0.0;
            p.sum = 0.0;
            p.skipped = false;
            p.stage = "decode".to_string();
            p.msg = "model reloaded — restarting".to_string();
        }
        let tx = self.pump_tx()?;
        self.queue.retry(&path.to_string_lossy(), active, &tx)?;
        self.status = format!("{} restarting…", Self::file_name(&path));
        Ok(())
    }

    /// Skip summary: probe the AI filename, then bypass map-reduce. The
    /// backend DONE completes the file (raw-only `.md`).
    pub fn skip_file(&mut self, id: u64) -> Result<(), String> {
        if !self.backend_live {
            return Err("backend offline — cannot skip".to_string());
        }
        let pos = self
            .processing
            .iter()
            .position(|p| p.id == id)
            .ok_or("file not in processing")?;
        let active = self.is_active(id);
        let path = self.processing[pos].path.clone();
        let path_s = path.to_string_lossy().into_owned();
        let name = match self.queue.probe_name(&path_s) {
            Ok(Some(n)) if !n.trim().is_empty() => n,
            _ => {
                // Probe failures stay visible (the heuristic still runs).
                self.file_warnings.insert(
                    path.clone(),
                    "Day/Class probe failed — filename is a heuristic guess.".to_string(),
                );
                Self::heuristic_name(&path)
            }
        };
        {
            let p = &mut self.processing[pos];
            p.name = name.clone();
            p.skipped = true;
            p.stage = "title".to_string();
            p.msg = "summary skipped — backend finishing (raw only)".to_string();
        }
        let tx = self.pump_tx()?;
        self.queue.skip_summary(&path_s, active, &tx)?;
        self.status = format!("{name}: summary skipped.");
        Ok(())
    }

    /// Local day/class filename guess (backend probe is primary).
    pub fn heuristic_name(path: &std::path::Path) -> String {
        let lower = path.to_string_lossy().to_lowercase();
        let toks: Vec<&str> = lower
            .split(|c: char| !c.is_ascii_alphanumeric())
            .filter(|t| !t.is_empty())
            .collect();
        let mut day = "Day 1".to_string();
        let mut i = 0;
        while i < toks.len() {
            let t = toks[i];
            if day == "Day 1" {
                if (t == "day" || t == "lec" || t == "lecture")
                    && i + 1 < toks.len()
                    && toks[i + 1].len() <= 3
                    && toks[i + 1].chars().all(|c| c.is_ascii_digit())
                {
                    day = format!("Day {}", toks[i + 1]);
                    break;
                }
                for key in ["day", "lec", "lecture"] {
                    if let Some(rest) = t.strip_prefix(key) {
                        if !rest.is_empty()
                            && rest.len() <= 3
                            && rest.chars().all(|c| c.is_ascii_digit())
                        {
                            day = format!("Day {rest}");
                            break;
                        }
                    }
                }
                if day != "Day 1" {
                    break;
                }
            }
            i += 1;
        }
        let cls = path
            .parent()
            .and_then(|d| d.file_name())
            .and_then(|s| s.to_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("General")
            .to_string();
        let mut topic: String = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("Untitled")
            .chars()
            .take(40)
            .collect();
        if topic.to_lowercase().starts_with(&day.to_lowercase()) {
            topic = topic[day.len()..].trim_start_matches(|c: char| c == '-' || c == ' ').to_string();
        }
        format!("{cls} - {day} - {topic}")
    }

    #[allow(dead_code)]
    pub fn set_selected(&mut self, id: u64, selected: bool) {
        if let Some(p) = self.processing.iter_mut().find(|p| p.id == id) {
            p.selected = selected;
        }
    }

    pub fn set_paused(&mut self, paused: bool) {
        self.paused = paused;
        let _ = self.queue.pause(paused);
        self.status = if paused {
            "Processing paused.".to_string()
        } else {
            "Processing continued.".to_string()
        };
    }

    /// Abort: selection if any, else everything. Aborted files return to
    /// Input; an emptied queue re-locks via `check_drained`.
    #[allow(dead_code)]
    pub fn abort_selected(&mut self) -> usize {
        let any_selected = self.processing.iter().any(|p| p.selected);
        if !any_selected {
            return self.abort_all();
        }
        let head = self.processing.first().map(|p| p.id);
        let ids: Vec<u64> = self
            .processing
            .iter()
            .filter(|p| p.selected)
            .map(|p| p.id)
            .collect();
        let mut n = 0;
        for id in ids {
            let active = self.backend_live && head == Some(id);
            if self.remove_processing(id, active).is_ok() {
                n += 1;
            }
        }
        self.status = "Aborted — files returned to Input.".to_string();
        n
    }

    /// Abort everything (backend abort + clear + epoch) and return all files
    /// to Input.
    pub fn abort_all(&mut self) -> usize {
        let n = self.processing.len();
        let _ = self.queue.abort_all();
        // A late research thread must never resurrect this run: phase-2
        // entries die here (temp files fall to Clean caches).
        self.phase2.clear();
        for p in self.processing.drain(..) {
            if let Some(inp) = self.input.iter_mut().find(|x| x.id == p.id) {
                inp.state = FileState::Queued;
            }
        }
        self.paused = false;
        self.status = "Aborted — files returned to Input.".to_string();
        self.check_drained();
        n
    }

    /// Cancel all in-flight model downloads (best-effort, never blocks).
    /// Mid-`rename(part→dest)` exits otherwise leave truncated models behind.
    pub fn cancel_downloads(&self) {
        if let Some(dl) = &self.downloader {
            dl.cancel_all();
        }
    }

    /// Single quit-path entry: flush drafts + prefs, cancel downloads, then
    /// abort the backend AND JOIN its worker (bounded 5 s). Every step is
    /// best-effort so shutdown can never hang — callers always `exit(0)`
    /// afterwards. The join is load-bearing: a joinable worker destroyed at
    /// process exit terminates via SIGABRT (the observed close-crash).
    /// (Recorder stop and VLM temp sweep hook in here ahead of the join.)
    pub fn shutdown_prepare(&mut self) {
        // Stop any live capture first: the take file is kept on disk (never
        // auto-deleted) so nothing recorded is lost to the exit.
        self.rec_shutdown();
        // Review widgets flush into `self.review` on every keystroke, so the
        // session holds the latest text even though disk persistence is
        // debounced — persist it now so quit never loses the last edits.
        let _ = self.persist_review();
        self.save_prefs();
        self.cancel_downloads();
        let _ = self.abort_all();
        let rc = self.queue.abort_and_join(5000);
        self.status = if rc == 0 {
            "Backend stopped cleanly.".to_string()
        } else {
            "Backend stop timed out — exiting anyway.".to_string()
        };
    }

    /// Boot integrity pass (once per process): hash the active pair on a
    /// low-priority worker thread and report via `Event::BootVerified`.
    /// Start blocks with "Verifying models" only while this is in flight —
    /// Start itself never hashes gigabytes synchronously. Panic-safe: a
    /// worker panic resolves the flag with a warning instead of hanging Start.
    pub fn spawn_boot_verify(&mut self, tx: Sender<Event>) {
        if self.verifying_models || self.boot_verified {
            return;
        }
        self.verifying_models = true;
        self.status = "Verifying models…".to_string();
        if std::thread::Builder::new()
            .name("pv-boot-verify".to_string())
            .stack_size(8 << 20)
            .spawn(move || {
                let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    pv_backend::verify::verify_active_boot()
                }));
                let warnings = match res {
                    Ok((stt, llm, w)) => {
                        // Persist any fallback the verifier selected so the
                        // run below + Models page agree on the actives.
                        if let (Some(s), Some(l)) = (stt, llm) {
                            let _ = pv_backend::models::set_active(&s, &l);
                        }
                        w
                    }
                    Err(_) => vec!["Model verification crashed — run unvalidated.".to_string()],
                };
                let _ = tx.send(Event::BootVerified { warnings });
            })
            .is_err()
        {
            self.verifying_models = false;
        }
    }

    /// Manual update check (Settings → Diagnostics). Spawns a worker that
    /// reports via `Event::UpdateCheck`; the caller pumps `rx` with
    /// `spawn_pump`. User-initiated only — no background polling, ever.
    pub fn check_updates(&mut self) -> Result<Receiver<Event>, String> {
        let (tx, rx): (Sender<Event>, Receiver<Event>) = std::sync::mpsc::channel();
        self.status = "Checking for updates…".to_string();
        if std::thread::Builder::new()
            .name("pv-update-check".to_string())
            .stack_size(8 << 20)
            .spawn(move || {
                let msg = match pv_backend::update::check_blocking() {
                    Ok(m) => m,
                    Err(e) => e,
                };
                let _ = tx.send(Event::UpdateCheck { message: msg });
            })
            .is_err()
        {
            return Err("Could not start update check.".to_string());
        }
        Ok(rx)
    }

    /// Navigate between pages. Leaving Record with an unsent finished take
    /// auto-stages it to Input (per spec) — recording in progress is never
    /// touched, and already-sent takes are not duplicated.
    pub fn goto_page(&mut self, page: Page) {
        if self.page == Page::Record && page != Page::Record {
            self.rec_auto_stage();
        }
        self.page = page;
    }

    // ---------------- Record page: transport + takes ----------------

    /// Capture devices for the Record picker (empty when audio is down).
    pub fn rec_devices(&self) -> Vec<String> {
        pv_backend::record::list_devices()
    }

    /// Rates the chosen device reports (empty = query failed → native only).
    pub fn rec_device_rates(&self) -> Vec<u32> {
        match &self.rec_device {
            Some(n) => pv_backend::record::device_rates(n),
            None => Vec::new(),
        }
    }

    pub fn rec_status_line(&self) -> String {
        let len = pv_backend::record::fmt_len(self.rec_samples, self.rec_rate);
        let fmt = if self.rec_rate > 0 {
            format!("{} Hz", self.rec_rate)
        } else {
            "—".to_string()
        };
        let ch = match self.rec_channels {
            1 => "mono".to_string(),
            n if n > 1 => format!("{n}ch"),
            _ => "—".to_string(),
        };
        match self.rec_status {
            RecStatus::Empty => "Empty — press Record.".to_string(),
            RecStatus::Recording => {
                format!("● REC {len} · 24-bit/{fmt} {ch} · peak {} · clips {} · learning room tone…",
                    self.rec_peak_db, self.rec_clips)
            }
            RecStatus::Paused => format!("❚❚ PAUSED {len} · press Record to resume."),
            RecStatus::Stopped => {
                let total = pv_backend::record::fmt_secs(self.rec_total_secs);
                let tail = if self.rec_sent { " · sent to Input ✓" } else { " · unsent take" };
                format!("■ STOPPED {total}{tail}")
            }
            RecStatus::Playing => {
                let total = pv_backend::record::fmt_secs(self.rec_total_secs);
                let pos = pv_backend::record::fmt_len(self.rec_play_pos, self.rec_rate);
                format!("▶ PLAYING {pos}/{total}")
            }
            RecStatus::PlayPaused => {
                let total = pv_backend::record::fmt_secs(self.rec_total_secs);
                let pos = pv_backend::record::fmt_len(self.rec_play_pos, self.rec_rate);
                format!("❚❚ PLAY PAUSED {pos}/{total}")
            }
        }
    }

    /// Start (or resume) capture. Resume after pause continues the SAME open
    /// stream (pause only drops samples), so the take stays one file.
    pub fn rec_start(&mut self) -> Result<(), String> {
        match self.rec_status {
            RecStatus::Paused => {
                if let Some(r) = &self.recorder {
                    r.set_paused(false);
                    self.rec_status = RecStatus::Recording;
                    self.status = "Recording resumed.".to_string();
                    return Ok(());
                }
                // Stream died while paused: fall through to a fresh take.
            }
            RecStatus::Recording => return Ok(()),
            RecStatus::Playing | RecStatus::PlayPaused => {
                self.rec_play_stop_silent();
            }
            _ => {}
        }
        // Fresh take (previous finished take must be sent/cleared first).
        if self.rec_status == RecStatus::Stopped && !self.rec_sent {
            return Err("Send or Clear the finished take first.".to_string());
        }
        let rec = pv_backend::record::Recorder::start(
            self.rec_device.as_deref(),
            self.rec_rate_opt,
        )?;
        self.rec_rate = rec.sample_rate;
        self.rec_channels = rec.channels;
        self.rec_samples = 0;
        self.rec_peaks.clear();
        self.rec_peak_db = "-inf dB".to_string();
        self.rec_clips = 0;
        self.rec_play_pos = 0;
        self.rec_total_secs = 0;
        self.rec_sent = false;
        self.rec_path = None;
        self.rec_parts.clear();
        self.rec_part = 1;
        self.recorder = Some(rec);
        self.rec_status = RecStatus::Recording;
        self.status = "Recording…".to_string();
        Ok(())
    }

    /// Pause capture (take file stays open; timer frozen — sample-counted).
    /// Pause during playback pauses playback instead (position kept).
    pub fn rec_pause(&mut self) {
        match self.rec_status {
            RecStatus::Recording => {
                if let Some(r) = &self.recorder {
                    r.set_paused(true);
                }
                self.rec_status = RecStatus::Paused;
                self.status = "Recording paused.".to_string();
            }
            RecStatus::Playing => {
                if let Some(p) = self.player.take() {
                    self.rec_play_pos = p.pos_frames();
                    p.stop();
                }
                self.rec_status = RecStatus::PlayPaused;
                self.status = "Playback paused.".to_string();
            }
            RecStatus::PlayPaused => {
                // Resume playback from the held position.
                self.rec_play_from(self.rec_play_pos);
            }
            RecStatus::Paused => {
                // Resume capture (same as Record while paused).
                let _ = self.rec_start();
            }
            _ => {}
        }
    }

    /// Stop capture, finalize all RF64 part headers, keep every file.
    /// Multi-part takes (gapless splits) stage as separate entries. Never deletes.
    /// A sticky disk-full surfaces here as Err AND keeps the finalized parts.
    pub fn rec_stop(&mut self) -> Result<(), String> {
        match self.rec_status {
            RecStatus::Recording | RecStatus::Paused => {
                let rec = self.recorder.take().ok_or("nothing recording")?;
                let rate = rec.sample_rate;
                match rec.stop() {
                    Ok(parts) => {
                        let total: u64 = parts.iter().map(pv_backend::record::take_secs).sum();
                        self.rec_total_secs = total;
                        self.rec_rate = rate;
                        self.rec_samples = total * rate as u64;
                        self.rec_path = parts.last().cloned();
                        self.rec_parts = parts;
                        self.rec_status = RecStatus::Stopped;
                        self.rec_sent = false;
                        self.status = if self.rec_parts.len() > 1 {
                            format!(
                                "Take kept ({} parts) — Send to Input or Clear.",
                                self.rec_parts.len()
                            )
                        } else {
                            "Take kept — Send to Input or Clear.".to_string()
                        };
                        Ok(())
                    }
                    Err(e) => {
                        // Disk-full path: finalize what exists, keep parts if
                        // the backend salvaged any, report — never retry.
                        self.rec_status = RecStatus::Stopped;
                        self.rec_sent = false;
                        self.status = format!("Take halted (disk full?) — {e}");
                        self.push_error("Record", e.clone(), String::new());
                        Err(e)
                    }
                }
            }
            RecStatus::Playing | RecStatus::PlayPaused => {
                self.rec_play_stop_silent();
                self.rec_status = RecStatus::Stopped;
                self.status = "Playback stopped.".to_string();
                Ok(())
            }
            _ => Err("Nothing to stop.".to_string()),
        }
    }

    /// Play the finished take (or resume from pause position).
    pub fn rec_play(&mut self) -> Result<(), String> {
        match self.rec_status {
            RecStatus::Stopped => {
                self.rec_play_from(0);
                Ok(())
            }
            RecStatus::PlayPaused => {
                let pos = self.rec_play_pos;
                self.rec_play_from(pos);
                Ok(())
            }
            RecStatus::Playing => Ok(()),
            _ => Err("Stop recording first, then play the take.".to_string()),
        }
    }

    fn rec_play_from(&mut self, offset: u64) {
        let path = match &self.rec_path {
            Some(p) => p.clone(),
            None => {
                self.status = "No take to play.".to_string();
                return;
            }
        };
        match pv_backend::record::Player::play(&path, offset) {
            Ok(p) => {
                self.player = Some(p);
                self.rec_status = RecStatus::Playing;
                self.status = "Playing take…".to_string();
            }
            Err(e) => {
                self.status = format!("Playback failed: {e}");
            }
        }
    }

    fn rec_play_stop_silent(&mut self) {
        if let Some(p) = self.player.take() {
            self.rec_play_pos = p.pos_frames();
            p.stop();
        }
    }

    /// Poll live counters (called from the Record view's refresh tick).
    /// Detects natural playback end and returns to Stopped. While recording:
    /// gapless part rotation, 10-minute split warning, disk-full halt, and
    /// the 24 h / 32 GiB sanity cap.
    pub fn rec_poll(&mut self) {
        // Hard sanity cap: 24 h of sample-counted frames at the take's own
        // rate (plus the 32 GiB backend byte cap) — clean stop, never
        // unbounded weekend fills.
        let max_samples_24h = (self.rec_rate.max(8000) as u64) * 86_400;
        if let Some(r) = &self.recorder {
            if matches!(self.rec_status, RecStatus::Recording | RecStatus::Paused) {
                // Disk-full is sticky in the backend: halt now, keep parts.
                if let Some(e) = r.write_failed() {
                    self.recorder.take();
                    self.rec_status = RecStatus::Stopped;
                    self.rec_sent = false;
                    self.status = format!("Take halted (disk full?) — {e}");
                    self.push_error("Record", e, String::new());
                    return;
                }
                self.rec_samples = r.samples();
                self.rec_peaks = r.peaks();
                self.rec_peak_db = r.peak_db();
                self.rec_clips = r.clips();
                self.rec_part = r.part();
                if self.rec_samples >= max_samples_24h {
                    let _ = self.rec_stop();
                    self.status = "Stopped: Maximum duration reached (24 h).".to_string();
                    return;
                }
                if let Some(finished) = r.poll_rotate() {
                    self.rec_parts.push(finished);
                    self.rec_part = r.part();
                    self.status = format!(
                        "Continuing in Part {} — Part {} finalized.",
                        self.rec_part,
                        self.rec_part.saturating_sub(1)
                    );
                } else if pv_backend::record::split_warning_due(self.rec_samples, self.rec_rate)
                    && matches!(self.rec_status, RecStatus::Recording)
                {
                    let left =
                        pv_backend::record::frames_until_split(self.rec_samples, self.rec_rate)
                            / self.rec_rate.max(1) as u64;
                    self.status = format!(
                        "● REC — Approaching file split in {:02}:{:02} (Part {}).",
                        left / 60,
                        left % 60,
                        self.rec_part
                    );
                }
            }
        }
        if let Some(p) = &self.player {
            if self.rec_status == RecStatus::Playing {
                self.rec_play_pos = p.pos_frames();
                if p.is_done() {
                    self.player.take().map(|p| p.stop());
                    self.rec_status = RecStatus::Stopped;
                    self.status = "Playback finished.".to_string();
                }
            }
        }
    }

    /// Boot crash-repair: rebuild placeholder ds64 sizes from EOF for every
    /// `.wav` under the recordings dir (killed takes). Silent scan; each
    /// repaired file lands in the Error Center as "Recovered an unsent
    /// recording" so the save is announced, never silently queued.
    pub fn repair_stale_takes(&mut self) -> Vec<String> {
        let repaired = pv_backend::record::repair_stale_takes();
        for name in &repaired {
            self.push_error(
                "Record",
                format!("Recovered an unsent recording from a previous session: {name}"),
                String::new(),
            );
        }
        repaired
    }

    /// Discard the finished take files (explicit Clear only — never automatic).
    pub fn rec_clear(&mut self) -> Result<(), String> {
        match self.rec_status {
            RecStatus::Recording | RecStatus::Paused | RecStatus::Playing | RecStatus::PlayPaused => {
                return Err("Stop first, then Clear.".to_string());
            }
            _ => {}
        }
        if let Some(p) = self.rec_path.take() {
            let _ = std::fs::remove_file(&p);
        }
        for p in self.rec_parts.drain(..) {
            let _ = std::fs::remove_file(&p);
        }
        self.rec_status = RecStatus::Empty;
        self.rec_samples = 0;
        self.rec_peaks.clear();
        self.rec_total_secs = 0;
        self.rec_play_pos = 0;
        self.rec_sent = false;
        self.rec_part = 1;
        self.rec_peak_db = "-inf dB".to_string();
        self.rec_clips = 0;
        self.status = "Take cleared.".to_string();
        Ok(())
    }

    /// Stage the finished take(s) into Input for transcription. Multi-part
    /// takes stage each part as its own standalone entry (FIFO-simple: if
    /// Part 2 fails later, Part 1's output still stands). Videos are
    /// never touched by this path — only Record takes and picked files.
    pub fn rec_send_to_input(&mut self) -> Result<(), String> {
        // State first: the error names the state, never the take shape.
        match self.rec_status {
            RecStatus::Stopped | RecStatus::PlayPaused => {}
            _ => return Err("Stop the take first.".to_string()),
        }
        let paths: Vec<PathBuf> = if self.rec_parts.len() > 1 {
            self.rec_parts.clone()
        } else {
            vec![self.rec_path.clone().ok_or("No take to send.")?]
        };
        self.rec_play_stop_silent();
        self.add_files(paths);
        self.rec_sent = true;
        self.status = if self.rec_parts.len() > 1 {
            format!("{} take parts sent to Input ✓", self.rec_parts.len())
        } else {
            "Take sent to Input ✓".to_string()
        };
        // Land on Input so Start is one click away (auto-stage no-ops:
        // rec_sent is already true).
        self.goto_page(Page::Input);
        Ok(())
    }

    /// Auto-stage on navigating away with an unsent finished take.
    /// Best-effort: failures only set the status line, never block nav.
    fn rec_auto_stage(&mut self) {
        if self.rec_status == RecStatus::Stopped && !self.rec_sent && self.rec_path.is_some() {
            let _ = self.rec_send_to_input();
        }
    }

    /// Quit-path: finalize any live capture, keeping the file. Never deletes.
    fn rec_shutdown(&mut self) {
        if matches!(self.rec_status, RecStatus::Recording | RecStatus::Paused) {
            let _ = self.rec_stop();
        }
        if self.player.is_some() {
            self.rec_play_stop_silent();
            if self.rec_status == RecStatus::Playing || self.rec_status == RecStatus::PlayPaused {
                self.rec_status = RecStatus::Stopped;
            }
        }
    }

    // ---------------- M4: models / downloads / wizard ----------------

    /// Re-read the inventory snapshot from disk + manifest.
    /// Links whose files vanished are removed first (user moved/deleted
    /// them) so stale paths can never linger.
    pub fn refresh_models(&mut self) {
        let dir = pv_backend::dirs::models_dir();
        let mut man = pv_backend::manifest::read(&dir);
        let dead = pv_backend::manifest::prune_missing_links(&mut man);
        if !dead.is_empty() {
            let _ = pv_backend::manifest::write(&dir, &man);
            self.status = format!("Removed {} missing link(s).", dead.len());
        }
        match pv_backend::models::status() {
            Ok(snap) => {
                self.models = snap.models;
                self.models_dir = snap.models_dir;
                self.active_stt = snap.active_stt;
                self.active_llm = snap.active_llm;
                self.active_vlm = snap.active_vlm;
            }
            Err(e) => self.status = format!("Models unreadable: {e}"),
        }
    }

    /// True when at least one complete STT + one complete LLM exist, or
    /// when a single complete model fills both slots (same-model mode).
    pub fn models_ready(&self) -> bool {
        if self.models.iter().any(|m| m.role == "stt" && m.complete)
            && self.models.iter().any(|m| m.role == "llm" && m.complete)
        {
            return true;
        }
        !self.active_stt.is_empty()
            && self.active_stt == self.active_llm
            && self
                .models
                .iter()
                .any(|m| m.id == self.active_stt && m.complete)
    }

    /// `(stt_id, llm_id, total_bytes)` for a tier (pure catalog read).
    pub fn tier_info(tier: &str) -> Option<(String, String, u64)> {
        let cat = pv_backend::catalog::load().ok()?;
        let (s, l) = pv_backend::catalog::tier_pair(&cat, tier)?;
        let sb = pv_backend::catalog::entry(&cat, &s)
            .map(|(_, _, b)| b)
            .unwrap_or(0);
        let lb = pv_backend::catalog::entry(&cat, &l)
            .map(|(_, _, b)| b)
            .unwrap_or(0);
        Some((s, l, sb + lb))
    }

    /// Apply loaded prefs (theme + models-dir override + compute mode).
    /// NOTE: this deliberately does NOT touch the process environment —
    /// constructor-time env writes race with parallel tests (and any other
    /// Store::new caller). The PV_CPU_ONLY bridge is applied explicitly at
    /// boot and on user switch (see [`Store::apply_compute_env`]).
    fn apply_prefs(&mut self, prefs: pv_backend::prefs::UiPrefs) {
        self.dark_mode = prefs.dark_mode;
        self.delete_converted = prefs.delete_converted;
        self.dismissed = prefs.dismissed.into_iter().collect();
        self.dismissed_groups = prefs.dismissed_groups.into_iter().collect();
        self.custom_models_dir = prefs.models_dir.clone();
        match prefs.models_dir {
            Some(dir) => pv_backend::dirs::set_models_dir_override(PathBuf::from(dir)),
            None => pv_backend::dirs::clear_models_dir_override(),
        }
        self.compute_mode = normalize_compute_mode(&prefs.compute_mode);
        self.view = view_from_string(&prefs.view);
        self.default_outdir = prefs.default_outdir.clone();
        self.default_classes = prefs.default_classes.clone();
        self.models_tier = normalize_tier(&prefs.models_tier);
        self.review_drafts_dir = prefs.review_drafts_dir.clone();
        self.chunk_tokens = prefs.chunk_tokens.clamp(200, 12000);
        self.summary_tier = normalize_summary_tier(&prefs.summary_tier);
        self.audio_window_sec = prefs.audio_window_sec.clamp(10, 60);
        self.vram_budget_pct = prefs.vram_budget_pct.clamp(50, 95);
        self.merge_suggest = prefs.merge_suggest.clamp(0.05, 0.95);
        self.merge_prompt = prefs.merge_prompt.clamp(0.05, 0.99);
        if self.merge_prompt < self.merge_suggest {
            self.merge_prompt = self.merge_suggest;
        }
        self.merge_mode = normalize_merge_mode(&prefs.merge_mode);
        self.merge_auto_prompt = prefs.merge_auto_prompt;
        self.doc_max_chars = prefs.doc_max_chars.clamp(10_000, 2_000_000);
        self.pdf_mode = normalize_pdf_mode(&prefs.pdf_mode);
        self.audio_retention = normalize_retention(&prefs.audio_retention);
        self.denoise_mode = normalize_denoise(&prefs.denoise_mode);
        self.web_research = prefs.web_research;
        self.attention = prefs.attention.clamp(0, 100);
    }

    /// Bridge the current compute mode into the backend (PV_CPU_ONLY is
    /// read by the loaders on every model load). Called at boot and on
    /// every user switch — never from constructors or tests.
    pub fn apply_compute_env(&self) {
        if self.compute_mode == "cpu" {
            std::env::set_var("PV_CPU_ONLY", "1");
        } else {
            std::env::remove_var("PV_CPU_ONLY");
        }
    }

    fn apply_compute_mode(&mut self, mode: &str) {
        self.compute_mode = normalize_compute_mode(mode);
        self.apply_compute_env();
    }

    /// Switch compute mode (persisted). Takes effect for models loaded
    /// after the switch; an already-loaded model keeps its backend until
    /// the next run.
    pub fn set_compute_mode(&mut self, mode: &str) {
        self.apply_compute_mode(mode);
        self.save_prefs();
        self.status = if self.compute_mode == "cpu" {
            "Compute: CPU only (applies to newly loaded models).".to_string()
        } else {
            "Compute: automatic (GPU first, CPU fallback).".to_string()
        };
    }

    /// Toggle post-processing cache cleanup (persisted). When on, converted
    /// WAV copies are deleted after a successful output; originals and
    /// failed/retried files are never touched.
    pub fn set_delete_converted(&mut self, on: bool) {
        self.delete_converted = on;
        self.save_prefs();
        self.status = if on {
            "Converted audio will be deleted after each output.".to_string()
        } else {
            "Converted audio will be kept for retries.".to_string()
        };
    }

    fn save_prefs(&self) {
        let prefs = pv_backend::prefs::UiPrefs {
            dark_mode: self.dark_mode,
            models_dir: self.custom_models_dir.clone(),
            compute_mode: self.compute_mode.clone(),
            delete_converted: self.delete_converted,
            dismissed: self.dismissed.iter().cloned().collect(),
            dismissed_groups: self.dismissed_groups.iter().cloned().collect(),
            view: view_to_string(self.view),
            default_outdir: self.default_outdir.clone(),
            default_classes: self.default_classes.clone(),
            models_tier: self.models_tier.clone(),
            review_drafts_dir: self.review_drafts_dir.clone(),
            chunk_tokens: self.chunk_tokens,
            summary_tier: self.summary_tier.clone(),
            audio_window_sec: self.audio_window_sec,
            vram_budget_pct: self.vram_budget_pct,
            merge_suggest: self.merge_suggest,
            merge_prompt: self.merge_prompt,
            merge_mode: self.merge_mode.clone(),
            merge_auto_prompt: self.merge_auto_prompt,
            doc_max_chars: self.doc_max_chars,
            pdf_mode: self.pdf_mode.clone(),
            audio_retention: self.audio_retention.clone(),
            denoise_mode: self.denoise_mode.clone(),
            web_research: self.web_research,
            attention: self.attention,
            staged: self
                .input
                .iter()
                .map(|f| f.path.to_string_lossy().into_owned())
                .take(256)
                .collect(),
        };
        if let Err(e) = pv_backend::prefs::save(&prefs) {
            // Non-fatal: the session keeps working, the pref just won't stick.
            let _ = e;
        }
    }

    pub fn set_dark_mode(&mut self, dark: bool) {
        self.dark_mode = dark;
        self.save_prefs();
    }

    // ---- Settings setters (single source of truth; toolbars call these) ----
    pub fn set_view_mode(&mut self, mode: ViewMode) {
        self.view = mode;
        self.save_prefs();
        self.status = format!("View: {:?}.", mode);
    }

    pub fn set_default_classes(&mut self, classes: String) {
        self.default_classes = classes.trim().to_string();
        self.classes = self.default_classes.clone();
        self.save_prefs();
        self.status = "Default classes updated — Start uses these.".to_string();
    }

    pub fn set_models_tier(&mut self, tier: &str) {
        self.models_tier = normalize_tier(tier);
        self.save_prefs();
    }

    pub fn set_default_outdir(&mut self, dir: &str) -> Result<(), String> {
        let acc = pv_backend::paths::accept_override(dir, pv_backend::paths::Purpose::Output)?;
        pv_backend::dirs::set_outdir_override(acc.path.clone());
        self.queue.set_out_dir(acc.path.clone());
        self.default_outdir = Some(acc.path.to_string_lossy().into_owned());
        self.save_prefs();
        self.status = match acc.warning {
            Some(w) => format!("Output folder: {} — {w}", acc.path.display()),
            None => format!("Output folder: {}", acc.path.display()),
        };
        Ok(())
    }

    pub fn reset_default_outdir(&mut self) {
        pv_backend::dirs::clear_outdir_override();
        self.queue.set_out_dir(pv_backend::dirs::output_dir());
        self.default_outdir = None;
        self.save_prefs();
        self.status = "Output folder reset to default.".to_string();
    }

    pub fn set_reviews_dir(&mut self, dir: &str) -> Result<(), String> {
        let acc = pv_backend::paths::accept_override(dir, pv_backend::paths::Purpose::Reviews)?;
        pv_backend::dirs::set_reviews_dir_override(acc.path.clone());
        self.review_drafts_dir = Some(acc.path.to_string_lossy().into_owned());
        self.save_prefs();
        let _ = pv_backend::drafts::ensure_base();
        self.status = match acc.warning {
            Some(w) => format!("Review drafts folder: {} — {w}", acc.path.display()),
            None => format!("Review drafts folder: {}", acc.path.display()),
        };
        Ok(())
    }

    pub fn reset_reviews_dir(&mut self) {
        pv_backend::dirs::clear_reviews_dir_override();
        self.review_drafts_dir = None;
        self.save_prefs();
        let _ = pv_backend::drafts::ensure_base();
        self.status = "Review drafts folder reset to default.".to_string();
    }

    pub fn move_review_drafts(&mut self, to: &str) -> Result<(), String> {
        let to = to.trim();
        if to.is_empty() {
            return Err("empty path".to_string());
        }
        let dest = PathBuf::from(to);
        let n = pv_backend::drafts::move_all_drafts(&dest)?;
        pv_backend::dirs::set_reviews_dir_override(dest);
        self.review_drafts_dir = Some(to.to_string());
        self.save_prefs();
        self.status = format!("Moved {n} draft(s).");
        Ok(())
    }

    pub fn set_audio_window(&mut self, v: i32) {
        self.audio_window_sec = v.clamp(10, 60);
        self.save_prefs();
    }

    pub fn set_vram_budget(&mut self, v: i32) {
        self.vram_budget_pct = v.clamp(50, 95);
        self.save_prefs();
    }

    pub fn set_merge_thresholds(&mut self, lo: f32, hi: f32) {
        let lo = lo.clamp(0.05, 0.95);
        let mut hi = hi.clamp(0.05, 0.99);
        if hi < lo {
            hi = lo;
        }
        self.merge_suggest = lo;
        self.merge_prompt = hi;
        self.save_prefs();
        self.refresh_merge_suggestions();
        self.status = format!("Merge bands: suggest ≥{lo:.2}, prompt ≥{hi:.2}.");
    }

    pub fn set_merge_mode(&mut self, mode: &str) {
        self.merge_mode = normalize_merge_mode(mode);
        self.save_prefs();
    }

    pub fn set_merge_auto_prompt(&mut self, on: bool) {
        self.merge_auto_prompt = on;
        self.save_prefs();
    }

    pub fn set_doc_max(&mut self, v: i32) {
        self.doc_max_chars = v.clamp(10_000, 2_000_000);
        self.save_prefs();
    }

    /// Chunk preset tokens for a summary tier (single selector drives both).
    /// Recap→Small 1000, Standard→Medium 4000, Detailed→Large 8000.
    pub fn tier_chunk_tokens(tier: &str) -> i32 {
        match tier {
            "recap" => 1000,
            "detailed" => 8000,
            _ => 4000,
        }
    }

    /// Length ratio target (percent of source words) per tier.
    pub fn tier_ratio_pct(tier: &str) -> i32 {
        match tier {
            "recap" => 25,
            "detailed" => 75,
            _ => 50,
        }
    }

    /// Set the summary tier (persisted). Applies the tier's recommended
    /// chunk preset too — custom sizes stay editable via set_chunk_tokens.
    pub fn set_summary_tier(&mut self, tier: &str) {
        self.summary_tier = normalize_summary_tier(tier);
        self.chunk_tokens = Self::tier_chunk_tokens(&self.summary_tier);
        self.save_prefs();
        let ratio = Self::tier_ratio_pct(&self.summary_tier);
        self.status = format!(
            "Summary: {} (chunks {} tokens, target ~{ratio}%, {} guide).",
            self.summary_tier,
            self.chunk_tokens,
            self.summary_tier,
        );
    }

    pub fn set_chunk_tokens(&mut self, v: i32) {
        self.chunk_tokens = v.clamp(200, 12000);
        self.save_prefs();
        self.status = format!("Chunk size: {} tokens.", self.chunk_tokens);
    }

    /// Warn (don't block) when Large chunks meet a Gemma summarizer: Gemma 2
    /// is 8k-native, so 8000-token chunks + context past training length
    /// degrade. Llama/Qwen handle the Large profile.
    pub fn chunk_model_note(&self) -> Option<String> {
        if self.chunk_tokens > 4000
            && self.active_llm.to_lowercase().contains("gemma")
        {
            Some(
                "Large chunks with a Gemma summarizer may degrade past its 8k \
                training length — Llama/Qwen recommended for Large."
                    .to_string(),
            )
        } else {
            None
        }
    }

    pub fn set_pdf_mode(&mut self, mode: &str) {
        self.pdf_mode = normalize_pdf_mode(mode);
        self.save_prefs();
    }

    /// Source retention after success (persisted). Only successful outputs
    /// are ever eligible; failures, aborts, and (Section 5) videos exempt.
    pub fn set_audio_retention(&mut self, mode: &str) {
        self.audio_retention = normalize_retention(mode);
        self.save_prefs();
        self.status = match self.audio_retention.as_str() {
            "delete" => "Sources will be deleted after each successful output.".to_string(),
            "archive" => "Sources will be archived beside each output.".to_string(),
            _ => "Sources will be kept after outputs.".to_string(),
        };
    }

    /// Pre-STT denoise mode (persisted). Applies to the STT copy only —
    /// archival takes and raw transcripts are never processed.
    pub fn set_denoise_mode(&mut self, mode: &str) {
        self.denoise_mode = normalize_denoise(mode);
        self.save_prefs();
        self.status = format!("Denoise: {}.", self.denoise_mode);
    }

    /// Web research toggle (persisted, default OFF). Plainly visible in
    /// Settings → Research — never hidden behind an obscure name.
    pub fn set_web_research(&mut self, on: bool) {
        self.web_research = on;
        self.save_prefs();
        self.status = if on {
            "Web research ON: video jobs may fetch up to 10 cited sources.".to_string()
        } else {
            "Web research OFF: fully offline.".to_string()
        };
    }

    /// Attention slider 0–100 (persisted). Drives frame density, caption
    /// detail, and summary length together for video jobs.
    pub fn set_attention(&mut self, v: i32) {
        self.attention = v.clamp(0, 100);
        self.save_prefs();
        self.status = format!("Attention: {} ({}).", self.attention, Self::attention_label(self.attention));
    }

    /// Detent label for an attention value. Pure.
    pub fn attention_label(v: i32) -> &'static str {
        if v < 34 {
            "Overview"
        } else if v < 67 {
            "Balanced"
        } else {
            "Academic"
        }
    }

    /// Start reads Settings defaults (no per-run overrides).
    pub fn run_outdir(&self) -> Option<PathBuf> {
        self.default_outdir
            .as_ref()
            .map(|d| d.trim())
            .filter(|d| !d.is_empty())
            .map(PathBuf::from)
    }

    pub fn run_classes(&self) -> String {
        if self.classes.trim().is_empty() {
            self.default_classes.clone()
        } else {
            self.classes.clone()
        }
    }

    /// Effective models dir for display (backend override or default home).
    pub fn effective_models_dir() -> String {
        pv_backend::dirs::models_dir()
            .to_string_lossy()
            .into_owned()
    }

    /// Pin a custom models dir (validated + created on demand).
    pub fn set_custom_models_dir(&mut self, dir: &str) -> Result<(), String> {
        let acc = pv_backend::paths::accept_override(dir, pv_backend::paths::Purpose::Models)?;
        pv_backend::dirs::set_models_dir_override(acc.path.clone());
        self.custom_models_dir = Some(acc.path.to_string_lossy().into_owned());
        self.save_prefs();
        self.refresh_models();
        self.status = match acc.warning {
            Some(w) => format!("Models folder: {} — {w}", acc.path.display()),
            None => format!("Models folder: {}", acc.path.display()),
        };
        Ok(())
    }

    /// Forget the custom dir; back to the default home.
    pub fn reset_models_dir(&mut self) {
        pv_backend::dirs::clear_models_dir_override();
        self.custom_models_dir = None;
        self.save_prefs();
        self.refresh_models();
        self.status = "Models folder reset to default.".to_string();
    }

    /// Decide wizard visibility (call once at boot, after refresh_models).
    pub fn maybe_wizard(&mut self) {
        self.wizard_open = !self.models_ready();
        if self.wizard_open {
            let det = self.queue.detect_tier();
            self.wizard_tier = match det.tier {
                0 => "lite",
                2 => "full",
                _ => "standard",
            }
            .to_string();
            // Distinguish "hardware truly unreadable" from "no backend loaded"
            // (dev runs without the staged DLL): only the latter is actionable.
            let backend_note = if self.queue.live() {
                ""
            } else {
                " Detection unavailable — backend DLL not loaded."
            };
            self.wizard_detail = format!(
                "Detected: VRAM {} · RAM {}. Recommended: {}.{}",
                if det.vram_gb < 0.0 {
                    "unknown".to_string()
                } else {
                    format!("{:.1} GB", det.vram_gb)
                },
                if det.ram_gb < 0.0 {
                    "unknown".to_string()
                } else {
                    format!("{:.1} GB", det.ram_gb)
                },
                self.wizard_tier,
                backend_note
            );
        }
    }

    fn ensure_downloader(&mut self) -> Result<(), String> {
        if self.downloader.is_none() {
            self.downloader = Some(Downloader::new()?);
        }
        Ok(())
    }

    /// Start (or resume) one model download. Returns the event receiver —
    /// the caller pumps it via [`spawn_pump`] (downloads run outside runs).
    pub fn start_download(&mut self, id: &str) -> Result<Receiver<Event>, String> {
        self.ensure_downloader()?;
        let (tx, rx) = std::sync::mpsc::channel();
        let dl = self.downloader.as_ref().ok_or("downloader unavailable")?;
        self.downloads.entry(id.to_string()).or_default();
        dl.start(id, tx)?;
        self.status = format!("Downloading {id}…");
        Ok(rx)
    }

    /// Download both files of a tier pair (wizard + tier buttons).
    /// Returns one receiver per started download for [`spawn_pump`].
    pub fn download_tier(&mut self, tier: &str) -> Result<Vec<Receiver<Event>>, String> {
        let cat = pv_backend::catalog::load()?;
        let (s, l) = pv_backend::catalog::tier_pair(&cat, tier).ok_or("unknown tier")?;
        let need_s = !self.models.iter().any(|m| m.id == s && m.complete);
        let need_l = !self.models.iter().any(|m| m.id == l && m.complete);
        let mut out = Vec::new();
        if need_s {
            out.push(self.start_download(&s)?);
        }
        if need_l {
            out.push(self.start_download(&l)?);
        }
        Ok(out)
    }

    pub fn cancel_download(&mut self, id: &str) {
        if let Some(dl) = &self.downloader {
            dl.cancel(id);
        }
        self.status = format!("Download cancelled: {id}");
    }

    pub fn set_active_pair(&mut self, stt: &str, llm: &str) {
        match pv_backend::models::set_active(stt, llm) {
            Ok(()) => {
                self.refresh_models();
                self.status = "Active pair updated.".to_string();
            }
            Err(e) => self.status = format!("Could not set active pair: {e}"),
        }
    }

    /// Start a vision download (text GGUF + mmproj under one id/progress).
    pub fn start_vision_download(&mut self, id: &str) -> Result<Receiver<Event>, String> {
        self.ensure_downloader()?;
        let (tx, rx) = std::sync::mpsc::channel();
        let dl = self.downloader.as_ref().ok_or("downloader unavailable")?;
        self.downloads.entry(id.to_string()).or_default();
        dl.start_vision(id, tx)?;
        self.status = format!("Downloading vision model {id} (weights + projector)…");
        Ok(rx)
    }

    pub fn set_active_vlm(&mut self, id: &str) {
        match pv_backend::models::set_active_vlm(id) {
            Ok(()) => {
                self.refresh_models();
                self.status = "Active vision model updated.".to_string();
            }
            Err(e) => self.status = format!("Could not set active vision model: {e}"),
        }
    }

    /// Recommended vision id for an attention value: the highest complete
    /// VLM whose `attention_min` fits, else the highest complete VLM, else
    /// the tier default. Pure view of the inventory snapshot.
    pub fn vlm_for_attention(&self, attention: i32) -> Option<String> {
        let mut best: Option<(&str, i64)> = None;
        for m in self.models.iter().filter(|m| m.role == "vlm" && m.complete) {
            let min = pv_backend::catalog::load()
                .ok()
                .and_then(|c| pv_backend::catalog::vision_entry(&c, &m.id))
                .map(|e| e.attention_min)
                .unwrap_or(0);
            if min as i32 <= attention && best.map(|(_, b)| min > b).unwrap_or(true) {
                best = Some((m.id.as_str(), min));
            }
        }
        if let Some((id, _)) = best {
            return Some(id.to_string());
        }
        // Fallback: any complete VLM, else the models-tier default.
        if let Some(m) = self.models.iter().find(|m| m.role == "vlm" && m.complete) {
            return Some(m.id.clone());
        }
        pv_backend::catalog::load()
            .ok()
            .and_then(|c| pv_backend::catalog::tier_vlm(&c, &self.models_tier))
    }

    /// Size-verify one model file against the catalog; human report.
    pub fn verify_model(&self, id: &str) -> String {
        match self.models.iter().find(|m| m.id == id) {
            None => format!("{id}: unknown model"),
            Some(m) if m.bytes > 0 && m.size == m.bytes => format!("{id}: OK ({} bytes)", m.size),
            Some(m) if m.size == 0 => format!("{id}: missing"),
            Some(m) => format!(
                "{id}: PARTIAL ({}/{} bytes) — re-download to repair",
                m.size, m.bytes
            ),
        }
    }

    /// Scan the local Ollama library (names + blob paths, nothing copied).
    pub fn scan_ollama(&mut self) -> usize {
        self.ollama_scan = pv_backend::ollama::scan();
        let n = self.ollama_scan.len();
        self.status = if n == 0 {
            "No Ollama models found. Is Ollama installed with pulled models?".to_string()
        } else {
            format!("Found {n} Ollama model(s) — Link one to use it here, no copy.")
        };
        n
    }

    /// Link one scanned Ollama model by display name under the given role
    /// ("stt", "llm", or "vlm"): validate the blob, record its absolute path
    /// plus role, refresh, and hash the blob on a worker (multi-GB — never
    /// the UI thread). Returns the event receiver — the caller pumps it via
    /// [`spawn_pump`]; `Event::LinkVerified` flips the badge or drops a
    /// tampered link. The file stays where Ollama put it. A linked VLM
    /// serves as the text half; its projector still comes from a downloaded
    /// catalog mmproj (same family).
    pub fn link_ollama(
        &mut self,
        name: &str,
        role: &str,
    ) -> Result<Receiver<Event>, String> {
        if role != "stt" && role != "llm" && role != "vlm" {
            return Err(format!("bad link role: {role}"));
        }
        let found = pv_backend::ollama::scan()
            .into_iter()
            .find(|m| m.name == name)
            .ok_or_else(|| format!("{name} not found — rescan the Ollama library"))?;
        if !pv_backend::ollama::gguf_magic_ok(&found.blob) {
            return Err(format!("{} is not a GGUF file", found.blob.display()));
        }
        let size = std::fs::metadata(&found.blob)
            .map_err(|e| e.to_string())?
            .len();
        if found.bytes > 0 && size != found.bytes {
            return Err(format!(
                "size mismatch for {name}: blob is {size}, manifest says {}",
                found.bytes
            ));
        }
        let dir = pv_backend::dirs::models_dir();
        let mut man = pv_backend::manifest::read(&dir);
        let id = format!("ollama:{}", found.name);
        pv_backend::manifest::record_link(
            &mut man,
            &id,
            size,
            found.digest,
            found.blob.to_string_lossy().into_owned(),
            role,
        );
        pv_backend::manifest::write(&dir, &man)?;
        self.refresh_models();
        let slot = if role == "stt" {
            "transcriber"
        } else if role == "vlm" {
            "vision text half (projector still comes from a downloaded Vision model)"
        } else {
            "summarizer"
        };
        // The link is usable now (magic+size gates passed); the hash proof
        // lands via LinkVerified. Spawn failures are terminal for the link:
        // without a worker there is no verification, so refuse, loudly.
        let (tx, rx) = std::sync::mpsc::channel();
        let id_clone = id.clone();
        if std::thread::Builder::new()
            .name("pv-link-verify".to_string())
            .stack_size(8 << 20)
            .spawn(move || {
                pv_backend::ollama::verify_link_blocking(&id_clone, tx);
            })
            .is_err()
        {
            let mut man = pv_backend::manifest::read(&dir);
            pv_backend::manifest::remove(&mut man, &id);
            let _ = pv_backend::manifest::write(&dir, &man);
            self.refresh_models();
            return Err("could not start link verification — link dropped".to_string());
        }
        self.status =
            format!("Linked {name} as {slot} — verifying blob hash in background…");
        Ok(rx)
    }

    /// Drop a link record. The Ollama file itself is never touched.
    pub fn unlink_model(&mut self, id: &str) {
        let dir = pv_backend::dirs::models_dir();
        let mut man = pv_backend::manifest::read(&dir);
        if pv_backend::manifest::remove(&mut man, id) {
            let _ = pv_backend::manifest::write(&dir, &man);
            self.status = format!("Unlinked {id}.");
        } else {
            self.status = format!("{id} is not linked.");
        }
        self.refresh_models();
    }

    // ---------------- M4: output store ops (view calls these) ----------------

    /// Rename an output (plus sidecars, best-effort). Returns the new name.
    pub fn rename_output(&mut self, id: u64, next: &str) -> Result<String, String> {
        let next = next.trim();
        if next.is_empty() {
            return Err("empty name".to_string());
        }
        let o = self
            .output
            .iter()
            .find(|o| o.id == id)
            .cloned()
            .ok_or("output not found")?;
        let base = next.trim_end_matches(".md");
        let target = format!("{base}.md");
        self.queue.output_rename_in(&o.dir, &o.md_name, &target)?;
        for ext in [".json", ".diff.json"] {
            let _ = self.queue.output_rename_in(
                &o.dir,
                &(o.md_name.clone() + ext),
                &(target.clone() + ext),
            );
        }
        if let Some(oo) = self.output.iter_mut().find(|o| o.id == id) {
            oo.name = base.to_string();
            oo.md_name = target.clone();
        }
        Ok(target)
    }

    /// × : hide the row only. Files on disk are untouched, and Refresh will
    /// not resurrect the row (its name joins `dismissed`).
    pub fn dismiss_output(&mut self, id: u64) -> Result<(), String> {
        let o = self
            .output
            .iter()
            .find(|o| o.id == id)
            .cloned()
            .ok_or("output not found")?;
        self.heal_review();
        self.dismissed.insert(o.md_name);
        self.output.retain(|x| x.id != id);
        self.status = "Row hidden (file kept on disk).".to_string();
        self.save_prefs();
        Ok(())
    }

    /// 🗑 : delete the row AND its files (.md + sidecars). Missing files
    /// never block the row removal — they are reported, not fatal.
    /// (SRT companions, when present, are swept too.)
    pub fn delete_output(&mut self, id: u64) -> Result<(), String> {
        let o = self
            .output
            .iter()
            .find(|o| o.id == id)
            .cloned()
            .ok_or("output not found")?;
        let mut missing = 0;
        let mut srt = o.md_name.clone();
        if srt.to_ascii_lowercase().ends_with(".md") {
            srt.truncate(srt.len() - 3);
            srt.push_str(".srt");
        } else {
            srt.push_str(".srt");
        }
        for name in [
            o.md_name.clone(),
            o.md_name.clone() + ".json",
            o.md_name.clone() + ".diff.json",
            srt,
        ] {
            if self.queue.output_remove_in(&o.dir, &name).is_err() {
                missing += 1;
            }
        }
        self.dismissed.remove(&o.md_name);
        self.output.retain(|x| x.id != id);
        self.save_prefs();
        self.heal_review();
        self.refresh_draft_badges();
        self.refresh_merge_suggestions();
        self.status = if missing == 0 {
            "Output + files deleted.".to_string()
        } else {
            "Row removed (some files were already gone).".to_string()
        };
        Ok(())
    }

    /// Copy an output's summary (`raw=false`) or full transcript to the
    /// system clipboard. Reads the in-memory snapshot — never touches disk.
    pub fn copy_output_text(&mut self, id: u64, raw: bool) -> Result<(), String> {
        let o = self
            .output
            .iter()
            .find(|o| o.id == id)
            .cloned()
            .ok_or("output not found")?;
        let text = if raw { o.raw } else { o.summary };
        if text.trim().is_empty() {
            return Err("nothing to copy (empty text)".to_string());
        }
        pv_backend::share::copy_text(&text).map_err(|e| e.to_string())?;
        self.status = if raw {
            "Transcript copied to clipboard.".to_string()
        } else {
            "Summary copied to clipboard.".to_string()
        };
        Ok(())
    }

    /// Reveal an output's `.md` in the OS file manager (selects the file).
    pub fn reveal_output(&mut self, id: u64) -> Result<(), String> {
        let o = self
            .output
            .iter()
            .find(|o| o.id == id)
            .cloned()
            .ok_or("output not found")?;
        let full = o.dir.join(&o.md_name);
        pv_backend::share::reveal_in_folder(&full).map_err(|e| e.to_string())?;
        self.status = "Opened in file manager.".to_string();
        Ok(())
    }

    /// Merge brand-new backend files into the static snapshot (Refresh).
    /// Never removes or rewrites existing entries.
    pub fn refresh_output(&mut self) {
        let list = match self.queue.output_list() {
            Ok(l) => l,
            Err(_) => {
                self.status =
                    "Refresh unavailable (backend offline) — snapshot unchanged.".to_string();
                return;
            }
        };
        let mut added = 0;
        for full in list {
            let name = PathBuf::from(&full)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();
            if name.is_empty()
                || self.output.iter().any(|o| o.md_name == name)
                || self.dismissed.contains(&name)
            {
                continue;
            }
            if let Ok(md) = self.queue.output_read(&name) {
                let (raw, summary, skipped) = parse_md(&md);
                let id = self.alloc();
                self.output.push(OutFile {
                    id,
                    name: name.trim_end_matches(".md").to_string(),
                    md_name: name,
                    dir: self.queue.out_dir().to_path_buf(),
                    raw,
                    summary,
                    skipped,
                });
                added += 1;
            }
        }
        self.status = if added > 0 {
            format!("Refresh: {added} new file(s).")
        } else {
            "Refresh: snapshot already current.".to_string()
        };
        self.heal_review();
        self.refresh_merge_suggestions();
        self.refresh_draft_badges();
    }

    /// Overwrite an output's `.md` (+ regen diff sidecar, best-effort).
    pub fn save_output_text(&mut self, id: u64, content: &str) -> Result<(), String> {
        let o = self
            .output
            .iter()
            .find(|o| o.id == id)
            .cloned()
            .ok_or("output not found")?;
        self.queue
            .output_write_in(&o.dir, &o.md_name, content)?;
        let (raw, summary, skipped) = parse_md(content);
        let diff = naive_diff(&raw, &summary);
        let _ = self
            .queue
            .output_write_in(&o.dir, &(o.md_name.clone() + ".diff.json"), &serde_json_lite(&diff));
        if let Some(oo) = self.output.iter_mut().find(|o| o.id == id) {
            oo.raw = raw;
            oo.summary = summary;
            oo.skipped = skipped;
        }
        Ok(())
    }

    // ---------------- Merge selection + runs ----------------

    pub fn toggle_merge_select(&mut self, id: u64) {
        if !self.merge_selection.remove(&id) {
            // Only staged inputs and outputs can be merge members.
            if self.input.iter().any(|f| f.id == id) || self.output.iter().any(|o| o.id == id) {
                self.merge_selection.insert(id);
            }
        }
    }

    pub fn dismiss_group(&mut self, key: &str) {
        self.dismissed_groups.insert(key.to_string());
        self.suggested.retain(|g| g.key != key);
        self.prompted.retain(|g| g.key != key);
        self.save_prefs();
    }

    fn merge_text_for_input(&self, f: &InputFile) -> Option<(String, String, String)> {
        // Returns (name, kind, text): staged docs via quick extract head,
        // staged audio has no text yet → None (clusters after Done).
        if f.kind != InputKind::Doc {
            return None;
        }
        let cap = 4000;
        match pv_backend::docs::extract_text(&f.path, cap) {
            Ok(ex) => Some((Self::file_name(&f.path), "document".to_string(), head_chars(&ex.text, 4000))),
            Err(_) => None,
        }
    }

    fn merge_items(&self) -> Vec<pv_backend::merge::MergeItem> {
        let mut items = Vec::new();
        for o in &self.output {
            let basis = if o.summary.trim().is_empty() { &o.raw } else { &o.summary };
            let t = head_chars(basis, 4000);
            if t.trim().is_empty() {
                continue;
            }
            items.push(pv_backend::merge::MergeItem {
                id: format!("out:{}", o.id),
                name: o.md_name.clone(),
                kind: "note".to_string(),
                text: t,
            });
        }
        for f in &self.input {
            if let Some((name, kind, text)) = self.merge_text_for_input(f) {
                items.push(pv_backend::merge::MergeItem {
                    id: format!("in:{}", f.id),
                    name,
                    kind,
                    text,
                });
            }
        }
        items
    }

    /// Recompute suggest/prompt groups (after Done, staging, threshold change).
    pub fn refresh_merge_suggestions(&mut self) {
        let items = self.merge_items();
        let (sugg, prom) = pv_backend::merge::suggest_groups(&items, self.merge_suggest, self.merge_prompt);
        let to_ui = |g: pv_backend::merge::MergeGroup| MergeSuggestion {
            key: pv_backend::merge::group_key(
                &g.members.iter().map(|m| pv_backend::merge::MergeItem {
                    id: m.id.clone(),
                    name: m.name.clone(),
                    kind: m.kind.clone(),
                    text: String::new(),
                }).collect::<Vec<_>>(),
            ),
            members: g.members.iter().map(|m| {
                let id = m.id.strip_prefix("out:").and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
                (id, m.name.clone(), m.kind.clone())
            }).collect(),
            score: g.score,
            band: String::new(),
        };
        let mut sugg: Vec<MergeSuggestion> = sugg.into_iter().map(|g| {
            let mut u = to_ui(g);
            u.band = "suggest".to_string();
            u
        }).collect();
        let mut prom: Vec<MergeSuggestion> = prom.into_iter().map(|g| {
            let mut u = to_ui(g);
            u.band = "prompt".to_string();
            u
        }).collect();
        sugg.retain(|g| !self.dismissed_groups.contains(&g.key));
        prom.retain(|g| !self.dismissed_groups.contains(&g.key));
        if !self.merge_auto_prompt {
            // Auto-prompt off → everything degrades to suggest chips.
            for mut g in prom {
                g.band = "suggest".to_string();
                sugg.push(g);
            }
            prom = Vec::new();
        }
        self.suggested = sugg;
        self.prompted = prom;
    }

    fn resolve_merge_mode(&self, mode: &str) -> String {
        match mode {
            "executive" | "long" => mode.to_string(),
            _ => match self.merge_mode.as_str() {
                "executive" | "long" => self.merge_mode.clone(),
                _ => "executive".to_string(),
            },
        }
    }

    /// Manual merge of the checkbox selection. Returns the event stream when
    /// a job was actually queued (None = validation-only path not needed;
    /// merge path validates inline and queues directly).
    pub fn begin_merge_run_selected(
        &mut self,
        mode: &str,
    ) -> Result<Option<Receiver<Event>>, String> {
        let ids: Vec<u64> = self.merge_selection.iter().cloned().collect();
        if ids.len() < 2 {
            return Err("Select at least two files to merge.".to_string());
        }
        self.begin_merge_run_ids(&ids, mode)
    }

    pub fn begin_merge_run_group(
        &mut self,
        key: &str,
        mode: &str,
    ) -> Result<Option<Receiver<Event>>, String> {
        let g = self
            .suggested
            .iter()
            .chain(self.prompted.iter())
            .find(|g| g.key == key)
            .cloned()
            .ok_or_else(|| "suggestion expired — suggestions refreshed".to_string())?;
        let ids: Vec<u64> = g.members.iter().map(|(id, _, _)| *id).filter(|id| *id != 0).collect();
        if ids.len() < 2 {
            return Err("Group members are no longer available.".to_string());
        }
        let out = self.begin_merge_run_ids(&ids, mode)?;
        self.dismiss_group(key);
        Ok(out)
    }

    fn begin_merge_run_ids(
        &mut self,
        ids: &[u64],
        mode: &str,
    ) -> Result<Option<Receiver<Event>>, String> {
        if !self.queue.live() {
            return Err("backend offline — reinstall or relaunch the app.".to_string());
        }
        let mode = self.resolve_merge_mode(mode);
        // Gather texts: outputs (summary/raw) + staged docs (extract).
        let mut members: Vec<pv_backend::merge::MergeItem> = Vec::new();
        for id in ids {
            if let Some(o) = self.output.iter().find(|o| o.id == *id) {
                let basis = if o.summary.trim().is_empty() {
                    o.raw.clone()
                } else {
                    format!("{}\n\n{}", o.summary, head_chars(&o.raw, 4000))
                };
                members.push(pv_backend::merge::MergeItem {
                    id: format!("out:{id}"),
                    name: o.md_name.clone(),
                    kind: "note".to_string(),
                    text: basis,
                });
            } else if let Some(f) = self.input.iter().find(|f| f.id == *id) {
                if f.kind != InputKind::Doc {
                    return Err(format!(
                        "{} has no text yet — transcribe it first, then merge the result.",
                        Self::file_name(&f.path)
                    ));
                }
                match pv_backend::docs::extract_text(&f.path, self.doc_max_chars.max(0) as usize) {
                    Ok(ex) => members.push(pv_backend::merge::MergeItem {
                        id: format!("in:{id}"),
                        name: Self::file_name(&f.path),
                        kind: "document".to_string(),
                        text: ex.text,
                    }),
                    Err(e) => {
                        return Err(format!("{}: extract failed ({e})", Self::file_name(&f.path)));
                    }
                }
            }
        }
        if members.len() < 2 {
            return Err("Need at least two texts to merge.".to_string());
        }
        // Write merged input cache; queue as is_text.
        let input_text = pv_backend::merge::build_merged_input(&members, &mode);
        let base = pv_backend::dirs::data_dir().join("merge_cache");
        std::fs::create_dir_all(&base).map_err(|e| e.to_string())?;
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let cache = base.join(format!("merged_{stamp}.txt"));
        std::fs::write(&cache, input_text).map_err(|e| e.to_string())?;
        let cache_s = cache.to_string_lossy().into_owned();
        let (stt, llm) = pv_backend::models::active_model_paths()?;
        let classes = self.run_classes();
        let db = pv_backend::dirs::db_path().to_string_lossy().into_owned();
        let merged_label = format!("Merged ({} sources)", members.len());
        let n = self.queue.queue_files(
            &[cache_s.clone()],
            &[true],
            &[false],
            &[merged_label.clone()],
            &stt.to_string_lossy(),
            &llm.to_string_lossy(),
            &classes,
            &db,
            self.compute_mode == "cpu",
            self.delete_converted,
            self.chunk_tokens,
            self.audio_window_sec,
            self.vram_budget_pct,
            &self.summary_tier.clone(),
            &self.audio_retention.clone(),
            &self.denoise_mode.clone(),
            false,
            self.attention,
            "",
            "",
            "",
            false,
            None,
        )?;
        // Remember members for the Sources-on-top rewrite at Done.
        self.pending_merged.insert(
            cache_s.clone(),
            members.iter().map(|m| (m.name.clone(), m.kind.clone(), head_chars(&m.text, 200_000))).collect(),
        );
        self.display_names.insert(cache_s.clone(), format!("Merged ({} sources)", members.len()));
        // Track as processing under a fresh id so progress renders.
        let pid = self.alloc();
        self.processing.push(ProcFile {
            id: pid,
            path: PathBuf::from(&cache_s),
            name: format!("Merged ({} sources)", members.len()),
            stt: 100.0,
            sum: 0.0,
            stage: "summarize".to_string(),
            msg: format!("merging {n} source(s) → {mode}"),
            selected: false,
            skipped: false,
        });
        self.started = true;
        self.unlocked = true;
        self.backend_live = true;
        let (tx, rx): (Sender<Event>, Receiver<Event>) = std::sync::mpsc::channel();
        self.event_tx = Some(tx.clone());
        self.queue.run(tx)?;
        self.merge_selection.clear();
        self.status = format!("Merging {} source(s) → {mode}…", members.len());
        Ok(Some(rx))
    }

    // ---------------- Review drafts (folder-per-output + index.json) ----------------

    fn draft_stem(md_name: &str) -> String {
        pv_backend::drafts::sanitize_stem(md_name.trim_end_matches(".md"))
    }

    /// Open (or seed) the persistent draft for an output.
    pub fn open_review(&mut self, id: u64) -> Result<(), String> {
        // Heal first so a dead session can never linger underneath the new one.
        self.heal_review();
        let o = self.output.iter().find(|o| o.id == id).cloned().ok_or("output not found")?;
        let d = pv_backend::drafts::open_draft(id, &o.md_name, &o.raw, &o.summary)?;
        self.review = Some(ReviewSession {
            id,
            md_name: o.md_name,
            stem: Self::draft_stem(&d.meta.md_name),
            left: d.left,
            right: d.right,
            center: d.center,
            merged: d.meta.merged,
            dirty: d.meta.dirty,
            undo: d.meta.undo.map(|u| RevSnapData {
                left: u.left,
                right: u.right,
                center: u.center,
                merged: u.merged,
            }),
        });
        self.refresh_draft_badges();
        Ok(())
    }

    pub fn close_review(&mut self) {
        if self.review.is_some() {
            let _ = self.persist_review();
        }
        self.review = None;
    }

    /// Persist current session texts to the draft folder + index.
    pub fn persist_review(&mut self) -> Result<(), String> {
        let r = self.review.clone().ok_or("no review open")?;
        let undo = r.undo.clone().map(|u| pv_backend::drafts::DraftUndo {
            left: u.left,
            right: u.right,
            center: u.center,
            merged: u.merged,
        });
        pv_backend::drafts::save_draft(&r.stem, &r.left, &r.right, &r.center, r.merged, r.dirty, undo)?;
        Ok(())
    }

    pub fn update_review_texts(&mut self, left: String, right: String, center: String) {
        if let Some(r) = self.review.as_mut() {
            if r.left != left || r.right != right || r.center != center {
                r.left = left;
                r.right = right;
                r.center = center;
                r.dirty = true;
            }
        }
    }

    pub fn mark_review_dirty(&mut self) {
        if let Some(r) = self.review.as_mut() {
            r.dirty = true;
        }
    }

    fn snap_review(&mut self) {
        if let Some(r) = self.review.as_mut() {
            r.undo = Some(RevSnapData {
                left: r.left.clone(),
                right: r.right.clone(),
                center: r.center.clone(),
                merged: r.merged,
            });
        }
    }

    pub fn review_merge(&mut self) {
        self.snap_review();
        if let Some(r) = self.review.as_mut() {
            r.center = format!("{}\n\n{}", r.left.trim(), r.right.trim());
            r.merged = true;
            r.dirty = true;
        }
        let _ = self.persist_review();
    }

    pub fn review_keep(&mut self, which: &str) {
        self.snap_review();
        if let Some(r) = self.review.as_mut() {
            r.center = if which == "left" { r.left.clone() } else { r.right.clone() };
            r.merged = true;
            r.dirty = true;
        }
        let _ = self.persist_review();
    }

    pub fn review_revert(&mut self) {
        if let Some(r) = self.review.as_mut() {
            if let Some(u) = r.undo.clone() {
                r.left = u.left;
                r.right = u.right;
                r.center = u.center;
                r.merged = u.merged;
                r.undo = None;
                r.dirty = true;
            }
        }
        let _ = self.persist_review();
    }

    /// Submit the center (merged) or side panes back to the `.md`; clears draft.
    pub fn submit_review(&mut self, content: String) -> Result<(), String> {
        let r = self.review.clone().ok_or("no review open")?;
        self.save_output_text(r.id, &content)?;
        let _ = pv_backend::drafts::delete_draft(&r.stem);
        self.review = None;
        self.refresh_draft_badges();
        self.refresh_merge_suggestions();
        self.status = "Review submitted.".to_string();
        Ok(())
    }

    pub fn heal_review(&mut self) {
        let ids: HashSet<u64> = self.output.iter().map(|o| o.id).collect();
        if let Some(r) = &self.review {
            if !ids.contains(&r.id) {
                self.review = None;
            }
        }
        if let Some(r) = &self.renaming {
            if !ids.contains(&r.id) {
                self.renaming = None;
            }
        }
    }

    // ---------------- Rename (centralized text) ----------------

    pub fn rename_begin(&mut self, id: u64) {
        if let Some(o) = self.output.iter().find(|o| o.id == id) {
            self.renaming = Some(RenameSession { id, text: o.name.clone() });
        }
    }

    pub fn rename_cancel(&mut self) {
        self.renaming = None;
    }

    pub fn rename_commit(&mut self) -> Result<String, String> {
        let r = self.renaming.clone().ok_or("nothing to rename")?;
        let prev = self
            .output
            .iter()
            .find(|o| o.id == r.id)
            .map(|o| o.md_name.clone())
            .unwrap_or_default();
        let next = self.rename_output(r.id, &r.text)?;
        // Best-effort draft folder rename (never loses text on failure).
        if !prev.is_empty() {
            let from = Self::draft_stem(&prev);
            let _ = pv_backend::drafts::rename_draft(&from, &next);
        }
        self.renaming = None;
        self.refresh_draft_badges();
        Ok(next)
    }

    pub fn rename_set_text(&mut self, t: String) {
        if let Some(r) = self.renaming.as_mut() {
            r.text = t;
        }
    }

    // ---------------- Diagnostics helpers ----------------

    pub fn unhide_dismissed(&mut self) {
        let n = self.dismissed.len();
        self.dismissed.clear();
        self.save_prefs();
        self.refresh_output();
        self.status = format!("Unhid {n} dismissed row(s).");
    }

    pub fn sweep_drafts(&mut self) {
        let live: Vec<String> = self.output.iter().map(|o| o.md_name.clone()).collect();
        let removed = pv_backend::drafts::sweep_orphans(&live);
        self.refresh_draft_badges();
        self.status = format!("Cleaned {} orphan draft(s).", removed.len());
    }

    /// Re-read persisted-draft membership from `index.json` (cheap metadata
    /// read; never touches pane contents). Drives the row `●` badges for
    /// closed drafts.
    pub fn refresh_draft_badges(&mut self) {
        let base = pv_backend::drafts::drafts_base();
        let idx = pv_backend::drafts::read_index(&base);
        self.draft_badges = idx.folders.values().map(|e| e.md_name.clone()).collect();
    }

    /// Drop extracted-document + merged-input caches (`doc_cache/`,
    /// `merge_cache/` under the data dir). In-flight runs reference cache
    /// paths, so this refuses while processing is active. Best-effort:
    /// counts removed top-level files, reports instead of failing.
    pub fn clean_caches(&mut self) {
        if !self.processing.is_empty() {
            self.push_error(
                "Settings",
                "Caches kept — a run is active; clean after it drains.".to_string(),
                String::new(),
            );
            return;
        }
        let base = pv_backend::dirs::data_dir();
        let mut removed = 0usize;
        for dir in [
            base.join("doc_cache"),
            base.join("merge_cache"),
            base.join("spill"),
            // Video temp: sampled frames + frames.md (kept for the VLM pass).
            base.join("frames"),
            // Research fetches (re-fetched on demand when missing).
            base.join("research"),
            // Phase-2 video temps: pass-1 transcripts + pass-2 payloads.
            // Orphans only (live entries hold their files until pass 2).
            base.join("phase1"),
            base.join("transcript"),
        ] {
            if let Ok(entries) = std::fs::read_dir(&dir) {
                for ent in entries.flatten() {
                    let p = ent.path();
                    let ok = if p.is_dir() {
                        std::fs::remove_dir_all(&p).is_ok()
                    } else {
                        std::fs::remove_file(&p).is_ok()
                    };
                    if ok {
                        removed += 1;
                    }
                }
            }
        }
        self.status = format!("Cleaned {removed} cached file(s).");
    }

    /// Log tails for the Settings Diagnostics viewer (bounded, seek-based —
    /// the UI never loads multi-GB logs).
    pub fn log_tails(&self) -> Vec<pv_backend::diag::LogTail> {
        pv_backend::diag::tail_logs()
    }
} // end impl Store — everything below is file scope

/// Client-side unified diff: `-` lines only in raw, `+` only in summary.
/// Order-insensitive bag comparison (matches core behavior on huge inputs).
pub fn naive_diff(raw: &str, summary: &str) -> Vec<(char, String)> {
    const CAP: usize = 500;
    let a: Vec<&str> = raw.split('\n').take(CAP).collect();
    let b: Vec<&str> = summary.split('\n').take(CAP).collect();
    let (sa, sb): (
        std::collections::HashSet<&str>,
        std::collections::HashSet<&str>,
    ) = (a.iter().copied().collect(), b.iter().copied().collect());
    let mut out = Vec::new();
    for l in &a {
        if !sb.contains(l) {
            out.push(('-', l.to_string()));
        }
    }
    for l in &b {
        if !sa.contains(l) {
            out.push(('+', l.to_string()));
        }
    }
    if out.is_empty() {
        out.push((' ', "(no differences)".to_string()));
    }
    out.truncate(CAP * 2);
    out
}

/// Pump a backend event stream into the store: worker thread for the
/// blocking receive, async channel hop, UI task for application.
/// Shared by pipeline runs and model downloads.
pub fn spawn_pump(store: Entity<Store>, rx: Receiver<Event>, cx: &mut App) {
    let (atx, arx) = async_channel::bounded(256);
    std::thread::spawn(move || {
        for ev in rx {
            if atx.send_blocking(ev).is_err() {
                break;
            }
        }
    });
    let pump = store;
    cx.spawn(async move |cx| {
        while let Ok(ev) = arx.recv().await {
            let pump = pump.clone();
            cx.update(|cx| {
                pump.update(cx, |s, cx| {
                    s.apply_event(ev);
                    cx.notify();
                })
            });
        }
    })
    .detach();
}

/// Minimal JSON encoder for diff items (avoids pulling serde into `app`).
fn serde_json_lite(items: &[(char, String)]) -> String {
    fn esc(s: &str) -> String {
        let mut o = String::with_capacity(s.len());
        for c in s.chars() {
            match c {
                '"' => o.push_str("\\\""),
                '\\' => o.push_str("\\\\"),
                '\n' => o.push_str("\\n"),
                '\r' => o.push_str("\\r"),
                '\t' => o.push_str("\\t"),
                c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
                c => o.push(c),
            }
        }
        o
    }
    let mut j = String::from("[");
    for (t, text) in items {
        j.push_str(&format!(
            "{{\"t\":\"{t}\",\"text\":\"{}\",\"chunk\":0}},",
            esc(text)
        ));
    }
    if j.ends_with(',') {
        j.pop();
    }
    j.push(']');
    j
}

fn stage_label(stage: Stage) -> &'static str {
    match stage {
        Stage::Decode => "decode",
        Stage::Transcribe => "transcribe",
        Stage::Summarize => "summarize",
        Stage::Title => "title",
        Stage::Diff => "diff",
        Stage::Done => "done",
        Stage::Error => "error",
    }
}

/// Split a finished `.md` into `(raw, summary, skipped)`. Mirrors the core's
/// writer: `## Summary` … `## Raw Transcript` (singles) or `## Sources` …
/// `## Summary` … `## Source Texts` (merged), with the skip marker note.
fn parse_md(md: &str) -> (String, String, bool) {
    let raw_marker = if md.contains("## Source Texts") {
        "## Source Texts"
    } else {
        "## Raw Transcript"
    };
    let parts: Vec<&str> = md.split(raw_marker).collect();
    let summary = parts
        .first()
        .unwrap_or(&"")
        .split("## Summary")
        .nth(1)
        .unwrap_or("")
        .trim()
        .to_string();
    let raw = parts.get(1).unwrap_or(&"").trim().to_string();
    // Retention archives append a trailing "## Audio\n<file>" section: it is
    // metadata, not transcript — strip it so Review/diff never show it.
    let raw = raw
        .split("\n## Audio")
        .next()
        .unwrap_or(&raw)
        .trim()
        .to_string();
    let skipped = summary.contains("*(summary skipped — raw transcript only)*");
    if skipped {
        (raw, String::new(), true)
    } else {
        (raw, summary, false)
    }
}

/// Head-truncate to `n` chars (merge clustering + cache payloads).
fn head_chars(t: &str, n: usize) -> String {
    if t.chars().count() <= n {
        return t.to_string();
    }
    t.chars().take(n).collect()
}

/// Normalize a persisted/selected compute mode to "cpu" or "auto".
/// Pure (unit-testable without touching process-global env or disk).
fn normalize_compute_mode(mode: &str) -> String {
    if mode == "cpu" {
        "cpu".to_string()
    } else {
        "auto".to_string()
    }
}

fn normalize_tier(t: &str) -> String {
    match t {
        "lite" | "full" => t.to_string(),
        _ => "standard".to_string(),
    }
}

fn normalize_merge_mode(m: &str) -> String {
    match m {
        "executive" | "long" => m.to_string(),
        _ => "ask".to_string(),
    }
}

fn normalize_pdf_mode(m: &str) -> String {
    match m {
        "shell" => "shell".to_string(),
        _ => "warn".to_string(),
    }
}

/// Source retention after success: "keep" | "delete" | "archive".
/// Anything else (including legacy missing keys) keeps originals.
fn normalize_retention(m: &str) -> String {
    match m {
        "delete" | "archive" => m.to_string(),
        _ => "keep".to_string(),
    }
}

/// Pre-STT denoise: "recommended" | "off" | "aggressive".
fn normalize_denoise(m: &str) -> String {
    match m {
        "off" | "aggressive" => m.to_string(),
        _ => "recommended".to_string(),
    }
}

fn normalize_summary_tier(t: &str) -> String {
    match t {
        "recap" | "detailed" => t.to_string(),
        _ => "standard".to_string(),
    }
}

/// Extract an integer from a tiny sidecar JSON blob (`"key":12345`) without
/// pulling serde into `app`. Returns None when absent/unparseable.
fn sidecar_int(text: &str, key: &str) -> Option<i64> {
    let pat = format!("\"{key}\":");
    let i = text.find(&pat)? + pat.len();
    let rest = text[i..].trim_start().as_bytes();
    let mut j = 0;
    if j < rest.len() && rest[j] == b'-' {
        j += 1;
    }
    let start = j;
    while j < rest.len() && rest[j].is_ascii_digit() {
        j += 1;
    }
    if j == start {
        return None;
    }
    text[i..i + j].parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_dedupes_and_removes() {
        let mut s = Store::new();
        s.add_files(vec![
            PathBuf::from("a.wav"),
            PathBuf::from("a.wav"),
            PathBuf::from("b.mp3"),
        ]);
        assert_eq!(s.input.len(), 2);
        let id = s.input[0].id;
        s.remove_input(id);
        assert_eq!(s.input.len(), 1);
    }

    #[test]
    fn add_dropped_expands_dirs_and_caps() {
        let mut s = Store::new();
        let dir = std::env::temp_dir().join(format!("pv-drop-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("a.wav"), b"RIFF").unwrap();
        std::fs::write(dir.join("sub").join("b.mp3"), b"ID3").unwrap();
        std::fs::write(dir.join("c.xyz"), b"nope").unwrap();
        s.add_dropped(&[dir.clone()]);
        let staged: Vec<String> = s
            .input
            .iter()
            .map(|f| f.path.to_string_lossy().into_owned())
            .collect();
        assert_eq!(s.input.len(), 2);
        assert!(staged.iter().any(|p| p.ends_with("a.wav")));
        assert!(staged.iter().any(|p| p.ends_with("b.mp3")));
        assert!(s.get_warning(&dir.join("c.xyz")).is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn copy_and_reveal_unknown_ids_error() {
        let mut s = Store::new();
        assert!(s.copy_output_text(424242, false).is_err());
        assert!(s.copy_output_text(424242, true).is_err());
        assert!(s.reveal_output(424242).is_err());
    }

    #[test]
    fn staged_queue_survives_restart_and_drops_missing() {
        let _plock = PREFS_LOCK.lock().unwrap();
        let _prefs = hermetic_prefs(r#"{"dark_mode":true}"#);
        let dir = std::env::temp_dir().join(format!("pv-staged-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let wav = dir.join("lec.wav");
        std::fs::write(&wav, b"RIFF").unwrap();
        {
            let mut s = Store::new();
            assert!(s.input.is_empty());
            s.add_files(vec![wav.clone()]);
            assert_eq!(s.input.len(), 1);
        }
        {
            let s2 = Store::new();
            assert_eq!(s2.input.len(), 1);
            assert_eq!(s2.input[0].path, wav);
        }
        std::fs::remove_file(&wav).unwrap();
        {
            let s3 = Store::new();
            assert!(s3.input.is_empty());
            assert!(s3.status.contains("no longer on disk"));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn validating_store() -> Store {
        let mut s = Store::new();
        // Hermetic: live user prefs must never leak into expectations.
        s.compute_mode = "auto".to_string();
        s.add_files(vec![PathBuf::from("a.wav")]);
        let id = s.input[0].id;
        s.input[0].state = FileState::Active;
        s.processing.push(ProcFile {
            id,
            path: PathBuf::from("a.wav"),
            name: "a.wav".into(),
            stt: 0.0,
            sum: 0.0,
            stage: "loading".into(),
            msg: "waiting for models".into(),
            selected: false,
            skipped: false,
        });
        s.started = true;
        s.unlocked = true;
        s.backend_live = true;
        s.validating = true;
        s.loading = Some("Loading STT model (GPU)…".into());
        s
    }

    #[test]
    fn validate_progress_sets_loading() {
        let mut s = validating_store();
        s.apply_event(Event::Validate {
            stage: "llm".into(),
            done: false,
            error: String::new(),
        });
        assert_eq!(s.loading.as_deref(), Some("Loading LLM (GPU)…"));
        assert!(s.validating);
    }

    #[test]
    fn validate_stt_error_switches_cpu_and_rolls_back() {
        let _env = ENV_LOCK.lock().unwrap();
        let _plock = PREFS_LOCK.lock().unwrap();
        let _prefs = PrefsGuard::take();
        let mut s = validating_store();
        // Hermetic mode regardless of the user's live setting.
        s.compute_mode = "auto".to_string();
        s.apply_event(Event::Validate {
            stage: "stt".into(),
            done: true,
            error: "boom".into(),
        });
        assert!(s.processing.is_empty());
        assert_eq!(s.input[0].state, FileState::Queued);
        assert!(!s.started && !s.backend_live && !s.validating);
        assert!(s.loading.is_none());
        assert_eq!(s.compute_mode, "cpu");
        assert_eq!(std::env::var("PV_CPU_ONLY").as_deref(), Ok("1"));
        assert!(s.status.contains("CPU") && s.status.contains("Start"));
        s.set_compute_mode("auto");
    }

    #[test]
    fn validate_llm_done_without_models_rolls_back() {
        // Test binaries have no models configured: seed an empty active
        // pair so continuation fails deterministically at resolution
        // (never hangs, never wedges). Serialized + restored like above.
        let _lock = MANIFEST_LOCK.lock().unwrap();
        let _manifest = ManifestGuard::take();
        let path = pv_backend::manifest::manifest_path(&pv_backend::dirs::models_dir());
        std::fs::write(&path, r#"{"files":{},"active_stt":"","active_llm":""}"#).unwrap();
        let mut s = validating_store();
        s.apply_event(Event::Validate {
            stage: "llm".into(),
            done: true,
            error: String::new(),
        });
        assert!(s.processing.is_empty());
        assert_eq!(s.input[0].state, FileState::Queued);
        assert!(!s.started && !s.backend_live && !s.validating);
        assert!(s.loading.is_none());
        assert!(s.status.contains("no active models"));
    }

    #[test]
    fn stale_validate_dropped_after_abort() {
        let mut s = validating_store();
        s.abort_all();
        let status = s.status.clone();
        s.apply_event(Event::Validate {
            stage: "llm".into(),
            done: true,
            error: String::new(),
        });
        assert_eq!(s.status, status);
        assert!(!s.validating);
    }

    #[test]
    fn double_start_while_validating_is_refused() {
        let mut s = Store::new();
        s.validating = true;
        let err = s.begin_run().unwrap_err();
        assert!(err.contains("Still loading"));
    }

    #[test]
    fn begin_run_needs_queued_files() {
        let mut s = Store::new();
        assert!(s.begin_run().is_err());
    }

    /// Process-global env (`PV_CPU_ONLY`) is shared by all threads in the
    /// test binary: tests that bridge or assert it serialize here.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Backup/restore guard for the LIVE prefs file: Store methods persist
    /// through fixed paths, so any test that triggers a save must leave
    /// the user's file exactly as found — even on panic (Drop always runs).
    struct PrefsGuard {        path: PathBuf,
        content: Option<String>,
    }
    impl PrefsGuard {
        fn take() -> Self {
            let path = pv_backend::prefs::prefs_path();
            let content = std::fs::read_to_string(&path).ok();
            PrefsGuard { path, content }
        }
    }
    impl Drop for PrefsGuard {
        fn drop(&mut self) {
            match &self.content {
                Some(c) => {
                    let _ = std::fs::write(&self.path, c);
                }
                None => {
                    let _ = std::fs::remove_file(&self.path);
                }
            }
        }
    }

    /// Hermetic prefs for tests: backs up the live file (restored on drop
    /// via the guard) AND writes known JSON, so ambient user state can never
    /// leak into expectations. Returns the guard (bind it: `_prefs`).
    fn hermetic_prefs(json: &str) -> PrefsGuard {
        let g = PrefsGuard::take();
        std::fs::write(pv_backend::prefs::prefs_path(), json).unwrap();
        g
    }

    #[test]
    fn delete_converted_toggle_persists() {
        let _plock = PREFS_LOCK.lock().unwrap();
        let _prefs = PrefsGuard::take();
        std::fs::write(
            pv_backend::prefs::prefs_path(),
            r#"{"dark_mode":true,"models_dir":null,"compute_mode":"auto","delete_converted":false,"dismissed":[]}"#,
        )
        .unwrap();
        let mut s = Store::new();
        assert!(!s.delete_converted);
        s.set_delete_converted(true);
        assert!(s.delete_converted);
        s.set_delete_converted(false);
        assert!(!s.delete_converted);
    }

    #[test]
    fn compute_mode_persists_and_bridges_env() {
        let _env = ENV_LOCK.lock().unwrap();
        let _plock = PREFS_LOCK.lock().unwrap();
        let _prefs = PrefsGuard::take();
        // Hermetic start regardless of ambient file state.
        std::fs::write(
            pv_backend::prefs::prefs_path(),
            r#"{"dark_mode":true,"models_dir":null,"compute_mode":"auto"}"#,
        )
        .unwrap();
        let mut s = Store::new();
        assert_eq!(s.compute_mode, "auto");
        s.set_compute_mode("cpu");
        assert_eq!(s.compute_mode, "cpu");
        assert_eq!(std::env::var("PV_CPU_ONLY").as_deref(), Ok("1"));
        s.set_compute_mode("auto");
        assert_eq!(s.compute_mode, "auto");
        assert!(std::env::var("PV_CPU_ONLY").is_err());
        s.set_compute_mode("bogus");
        assert_eq!(s.compute_mode, "auto");
    }

    #[test]
    fn models_ready_allows_same_model_in_both_slots() {        use pv_backend::models::ModelStatus;
        let mut s = Store::new();
        assert!(!s.models_ready());
        s.models.push(ModelStatus {
            id: "ollama:mx".to_string(),
            role: "stt".to_string(),
            file: "mx.bin".to_string(),
            bytes: 7,
            tier: "linked".to_string(),
            note: String::new(),
            present: true,
            size: 7,
            complete: true,
            active: false,
            expected_sha: String::new(),
            unverified: false,
        });
        s.active_stt = "ollama:mx".to_string();
        s.active_llm = "ollama:mx".to_string();
        assert!(s.models_ready());
    }

    /// Serializes tests that rewrite the live manifest (activation paths).
    /// Without this, parallel tests observe each other's mid-test actives.
    static MANIFEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Serializes tests that save prefs (compute toggle, dismiss, etc.).
    /// save_prefs targets the live ui.json; without this, parallel tests
    /// overwrite each other's file mid-assertion (same race as manifests).
    static PREFS_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Backup/restore guard for the LIVE manifest: tier activation writes
    /// through to disk, so tests that trigger it must leave the user's
    /// file exactly as found — even on panic (Drop always runs).
    struct ManifestGuard {
        path: PathBuf,
        content: Option<Vec<u8>>,
    }
    impl ManifestGuard {
        fn take() -> Self {
            let path = pv_backend::manifest::manifest_path(&pv_backend::dirs::models_dir());
            let content = std::fs::read(&path).ok();
            ManifestGuard { path, content }
        }
    }
    impl Drop for ManifestGuard {
        fn drop(&mut self) {
            match &self.content {
                Some(c) => {
                    let _ = std::fs::write(&self.path, c);
                }
                None => {
                    let _ = std::fs::remove_file(&self.path);
                }
            }
        }
    }

    fn seed_lite_pair(s: &mut Store) {
        use pv_backend::models::ModelStatus;
        for (id, role) in [
            ("small.en-Q5_1", "stt"),
            ("qwen2.5-7b-it-Q4_K_M", "llm"),
        ] {
            // refresh_models() rebuilds the inventory from disk (dropping
            // anything not really downloaded), so re-seed after any op
            // that refreshes — e.g. set_active_pair below.
            s.models.retain(|m| m.id != id);
            s.models.push(ModelStatus {
                id: id.to_string(),
                role: role.to_string(),
                file: String::new(),
                bytes: 1,
                tier: "lite".to_string(),
                note: String::new(),
                present: true,
                size: 1,
                complete: true,
                active: false,
                expected_sha: String::new(),
                unverified: false,
            });
        }
    }

    #[test]
    fn completed_tier_becomes_default_when_idle() {
        // Serialized: activation rewrites the live manifest (guarded above).
        let _lock = MANIFEST_LOCK.lock().unwrap();
        let _manifest = ManifestGuard::take();
        let mut s = Store::new();
        seed_lite_pair(&mut s);
        s.active_stt = "large-v3-turbo-Q5_0".to_string();
        s.active_llm = "gemma-2-9b-it-Q4_K_M".to_string();
        // 1. Idle completion promotes the pair.
        s.maybe_activate_completed_tier("small.en-Q5_1");
        assert_eq!(s.active_stt, "small.en-Q5_1");
        assert_eq!(s.active_llm, "qwen2.5-7b-it-Q4_K_M");
        assert!(s.status.contains("Lite"));
        // 2. Already active: no-op.
        s.maybe_activate_completed_tier("small.en-Q5_1");
        assert_eq!(s.active_stt, "small.en-Q5_1");
        // 3. Busy run: no switch, hint posted instead.
        s.active_stt = "large-v3-turbo-Q5_0".to_string();
        s.active_llm = "gemma-2-9b-it-Q4_K_M".to_string();
        seed_lite_pair(&mut s);
        s.processing.push(ProcFile {
            id: 999,
            path: PathBuf::from("busy.wav"),
            name: "busy.wav".into(),
            stt: 10.0,
            sum: 0.0,
            stage: "transcribe".into(),
            msg: String::new(),
            selected: false,
            skipped: false,
        });
        s.maybe_activate_completed_tier("small.en-Q5_1");
        assert_eq!(s.active_stt, "large-v3-turbo-Q5_0");
        assert!(s.status.contains("Use-pair"));
        // 4. Unknown/linked ids never switch.
        s.processing.clear();
        s.maybe_activate_completed_tier("ollama:ghost");
        assert_eq!(s.active_stt, "large-v3-turbo-Q5_0");
    }

    /// Codec gate matrix: every ffmpeg family stages clean, junk warns,
    /// Audacity projects get the export-first message, tracker .okt stages.
    #[test]
    fn codec_gate_covers_ffmpeg_families() {
        let mut s = Store::new();
        s.add_files(vec![
            PathBuf::from("a.weba"),
            PathBuf::from("b.okt"),
            PathBuf::from("c.mkv"),
            PathBuf::from("d.ts"),
            PathBuf::from("e.opus"),
            PathBuf::from("f.spc"),
            PathBuf::from("h.aup3"),
            PathBuf::from("j.aup4"),
            PathBuf::from("g.mid"),
            PathBuf::from("k.aup"),
            PathBuf::from("i.raw"),
        ]);
        let staged: Vec<String> = s
            .input
            .iter()
            .map(|f| f.path.to_string_lossy().into_owned())
            .collect();
        for good in ["a.weba", "b.okt", "c.mkv", "d.ts", "e.opus", "f.spc", "h.aup3", "j.aup4"] {
            assert!(staged.contains(&good.to_string()), "{good} should stage");
        }
        let warn = |p: &str| {
            s.get_warning(&PathBuf::from(p))
                .unwrap_or_else(|| panic!("{p} should warn"))
                .to_string()
        };
        assert!(warn("g.mid").contains("Unsupported"));
        assert!(warn("k.aup").contains("Audacity"));
        assert!(warn("i.raw").contains("Unsupported"));
    }

    #[test]
    fn drain_relocks_and_kicks_home() {
        let mut s = Store::new();
        s.unlocked = true;
        s.started = true;
        s.backend_live = true;
        s.paused = true;
        s.check_drained();
        assert!(!s.unlocked && s.page == Page::Input);
        assert!(!s.started && !s.backend_live && !s.paused);
    }

    #[test]
    fn parse_md_split_and_skip_marker() {
        let (raw, sum, sk) = parse_md("# T\n\n## Summary\nhello\n\n## Raw Transcript\nworld\n");
        assert_eq!(raw, "world");
        assert_eq!(sum, "hello");
        assert!(!sk);
        let (_, _, sk2) = parse_md("# T\n\n## Summary\n*(summary skipped — raw transcript only)*\n\n## Raw Transcript\nr\n");
        assert!(sk2);
    }

    #[test]
    fn aborted_event_returns_file_to_input() {
        let mut s = Store::new();
        s.add_files(vec![PathBuf::from("a.wav")]);
        let id = s.input[0].id;
        s.input[0].state = FileState::Active;
        s.processing.push(ProcFile {
            id,
            path: PathBuf::from("a.wav"),
            name: "a.wav".into(),
            stt: 10.0,
            sum: 0.0,
            stage: "transcribe".into(),
            msg: String::new(),
            selected: false,
            skipped: false,
        });
        s.unlocked = true;
        s.apply_event(Event::File {
            file: "a.wav".into(),
            stage: Stage::Error,
            fraction: 0.0,
            message: "aborted".into(),
            aborted: true,
        });
        assert!(s.processing.is_empty());
        assert_eq!(s.input[0].state, FileState::Queued);
    }

    #[test]
    fn error_event_retires_file_to_input() {
        let mut s = Store::new();
        s.add_files(vec![PathBuf::from("a.wav")]);
        let id = s.input[0].id;
        s.input[0].state = FileState::Active;
        s.processing.push(ProcFile {
            id,
            path: PathBuf::from("a.wav"),
            name: "a.wav".into(),
            stt: 10.0,
            sum: 0.0,
            stage: "transcribe".into(),
            msg: String::new(),
            selected: false,
            skipped: false,
        });
        s.unlocked = true;
        s.apply_event(Event::File {
            file: "a.wav".into(),
            stage: Stage::Error,
            fraction: 0.0,
            message: "boom".into(),
            aborted: false,
        });
        assert!(s.processing.is_empty());
        assert_eq!(s.input[0].state, FileState::Queued);
        assert!(s
            .file_warnings
            .get(&PathBuf::from("a.wav"))
            .is_some_and(|w| w.contains("boom")));
    }

    fn staged_proc() -> Store {
        let mut s = Store::new();
        s.add_files(vec![PathBuf::from("a.wav"), PathBuf::from("b.mp3")]);
        let ids: Vec<u64> = s.input.iter().map(|f| f.id).collect();
        for (i, id) in ids.iter().enumerate() {
            s.input[i].state = FileState::Active;
            s.processing.push(ProcFile {
                id: *id,
                path: s.input[i].path.clone(),
                name: format!("f{i}"),
                stt: 10.0,
                sum: 0.0,
                stage: "transcribe".into(),
                msg: String::new(),
                selected: false,
                skipped: false,
            });
        }
        s.unlocked = true;
        s
    }

    #[test]
    fn set_selected_targets_only_target() {
        let mut s = staged_proc();
        let id = s.processing[0].id;
        s.set_selected(id, true);
        assert!(s.processing[0].selected && !s.processing[1].selected);
        s.set_selected(id, false);
        assert!(!s.processing[0].selected);
    }

    #[test]
    fn abort_selected_returns_selection_to_input() {
        let mut s = staged_proc();
        let id = s.processing[0].id;
        s.set_selected(id, true);
        assert_eq!(s.abort_selected(), 1);
        assert_eq!(s.processing.len(), 1);
        assert_eq!(
            s.input.iter().find(|f| f.id == id).unwrap().state,
            FileState::Queued
        );
        assert!(s.unlocked); // queue not empty: stays unlocked
    }

    #[test]
    fn abort_all_drains_and_relocks() {
        let mut s = staged_proc();
        s.paused = true;
        assert_eq!(s.abort_all(), 2);
        assert!(s.processing.is_empty());
        assert!(!s.unlocked && s.page == Page::Input);
        assert!(!s.paused, "abort releases a held pause");
        assert!(s.input.iter().all(|f| f.state == FileState::Queued));
    }
    #[test]
    fn shutdown_prepare_flushes_and_aborts_without_hanging() {
        // Best-effort quit path: no review open, no downloader, sim backend.
        // Must drain processing and never panic — exit(0) follows in prod.
        let mut s = staged_proc();
        s.paused = true;
        s.shutdown_prepare();
        assert!(s.processing.is_empty());
        assert!(!s.paused);
        assert!(s.input.iter().all(|f| f.state == FileState::Queued));
        // Idempotent: second call (alt-q + app-quit double-fire) is safe.
        s.shutdown_prepare();
        assert!(s.processing.is_empty());
    }

    #[test]
    fn rec_transport_guards_empty_state() {
        let mut s = Store::new();
        assert_eq!(s.rec_status, RecStatus::Empty);
        assert!(s.rec_stop().is_err());
        assert!(s.rec_clear().is_ok()); // clearing nothing is a no-op success
        assert!(s.rec_send_to_input().is_err());
        assert!(s.rec_play().is_err());
        assert!(s.rec_status_line().contains("Empty"));
    }

    #[test]
    fn rec_auto_stage_moves_unsent_take_on_nav() {
        let mut s = Store::new();
        // Fake a finished take (no hardware needed — path only).
        let dir = std::env::temp_dir().join(format!("pv-rec-nav-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let wav = dir.join("take.wav");
        std::fs::write(&wav, b"RIFFfake").unwrap();
        s.rec_status = RecStatus::Stopped;
        s.rec_path = Some(wav.clone());
        s.rec_sent = false;
        s.page = Page::Record;
        // WAV content is not validated here (STT validates at Start).
        s.goto_page(Page::Input);
        assert_eq!(s.page, Page::Input);
        assert!(s.rec_sent);
        assert!(s.input.iter().any(|f| f.path == wav));
        // Staying put never duplicates.
        let n = s.input.len();
        s.goto_page(Page::Input);
        assert_eq!(s.input.len(), n);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn retention_and_denoise_normalize_and_persist() {
        let _plock = PREFS_LOCK.lock().unwrap();
        let _prefs = PrefsGuard::take();
        assert_eq!(normalize_retention("delete"), "delete");
        assert_eq!(normalize_retention("archive"), "archive");
        assert_eq!(normalize_retention("nuke"), "keep");
        assert_eq!(normalize_denoise("off"), "off");
        assert_eq!(normalize_denoise("aggressive"), "aggressive");
        assert_eq!(normalize_denoise("max"), "recommended");
        let mut s = Store::new();
        s.set_audio_retention("archive");
        assert_eq!(s.audio_retention, "archive");
        s.set_audio_retention("bogus");
        assert_eq!(s.audio_retention, "keep");
        s.set_denoise_mode("off");
        assert_eq!(s.denoise_mode, "off");
        s.set_denoise_mode("bogus");
        assert_eq!(s.denoise_mode, "recommended");
    }

    #[test]
    fn attention_and_research_settings() {
        // Live prefs file: serialize + restore like every other prefs test
        // (parallel tests + ambient user state must never leak in).
        let _plock = PREFS_LOCK.lock().unwrap();
        // Hermetic start regardless of ambient file state (a leaked
        // web_research=true in the live ui.json failed this assert before).
        let _prefs = hermetic_prefs(
            r#"{"dark_mode":true,"models_dir":null,"compute_mode":"auto","web_research":false,"attention":50}"#,
        );
        assert_eq!(Store::attention_label(0), "Overview");
        assert_eq!(Store::attention_label(33), "Overview");
        assert_eq!(Store::attention_label(34), "Balanced");
        assert_eq!(Store::attention_label(66), "Balanced");
        assert_eq!(Store::attention_label(67), "Academic");
        assert_eq!(Store::attention_label(100), "Academic");
        assert!(is_video_ext("mp4"));
        assert!(is_video_ext("MKV"));
        assert!(!is_video_ext("mp3"));
        assert!(!is_video_ext("wav"));
        let mut s = Store::new();
        assert!(!s.web_research);
        s.set_web_research(true);
        assert!(s.web_research);
        s.set_attention(85);
        assert_eq!(s.attention, 85);
        s.set_attention(999);
        assert_eq!(s.attention, 100);
        s.set_attention(-5);
        assert_eq!(s.attention, 0);
    }

    #[test]
    fn video_staged_as_video_kind() {
        let mut s = Store::new();
        let dir = std::env::temp_dir().join(format!("pv-vid-kind-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let v = dir.join("film.mp4");
        let a = dir.join("lec.mp3");
        std::fs::write(&v, b"fake").unwrap();
        std::fs::write(&a, b"fake").unwrap();
        s.add_files(vec![v.clone(), a.clone()]);
        assert_eq!(
            s.input.iter().find(|f| f.path == v).unwrap().kind,
            InputKind::Video
        );
        assert_eq!(
            s.input.iter().find(|f| f.path == a).unwrap().kind,
            InputKind::Audio
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn research_stem_matches_core_rules() {
        assert_eq!(Store::research_stem("film.mp4"), "film");
        // Path semantics (both sides): separators split before sanitizing;
        // every illegal char becomes exactly one dash, both sides.
        assert_eq!(Store::research_stem("A<b>:c.mp4"), "A-b--c");
        assert!(!Store::research_stem("x.mp4").contains(['<', '>', ':', '"', '/', '\\', '|', '?', '*']));
        let long = "a".repeat(100) + ".mp4";
        assert!(Store::research_stem(&long).len() <= 60);
    }

    #[test]
    fn phase1_done_feeds_research_not_output() {
        // No event channel: the research thread cannot run, so the run
        // unwinds to retryable state — while still proving the intercept
        // contract (transcript staged, temp md gone, no Output entry).
        let mut s = Store::new();
        let vpath = PathBuf::from("C:/vids/film.mp4");
        let id = 4242;
        s.input.push(InputFile {
            id,
            path: vpath.clone(),
            state: FileState::Active,
            size: 1,
            kind: InputKind::Video,
        });
        s.processing.push(ProcFile {
            id,
            path: vpath.clone(),
            name: "film.mp4".to_string(),
            stt: 100.0,
            sum: 0.0,
            stage: "transcribe".to_string(),
            msg: String::new(),
            selected: false,
            skipped: false,
        });
        s.phase2.insert(
            id,
            Phase2Job {
                display: "film.mp4".to_string(),
                stem: "film".to_string(),
                transcript_txt: PathBuf::new(),
            },
        );
        let pdir = Store::phase1_dir();
        let _ = std::fs::create_dir_all(&pdir);
        let md = pdir.join("phase1-probe-t.md");
        std::fs::write(
            &md,
            "# T\n\n## Summary\n*(summary skipped — raw transcript only)*\n\n## Raw Transcript\nhello world transcript here\n",
        )
        .unwrap();
        let vpath2 = PathBuf::from("C:/vids/film.mp4");
        s.complete_file(&vpath2, &md.to_string_lossy());
        // Intercept, not Output: transcript staged, temp md gone.
        assert!(s.output.is_empty());
        assert!(!md.exists());
        let entry = s.phase2.get(&id).cloned();
        // No event channel → clean unwind (entry dropped, file retryable).
        assert!(entry.is_none());
        assert!(s.input.iter().find(|f| f.id == id).unwrap().state == FileState::Queued);
        let _ = std::fs::remove_file(&md);
        let _ = std::fs::remove_file(Store::transcript_dir().join("transcript__4242.txt"));
    }

    #[test]
    fn phase2_done_cleans_transcript_payload() {
        // Normal Done with a phase-2 entry: Output created, payload swept.
        let mut s = Store::new();
        let outdir = std::env::temp_dir().join(format!("pv-p2out-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&outdir);
        s.queue.set_out_dir(outdir.clone());
        let vpath = PathBuf::from("C:/vids/film.mp4");
        let id = 4243;
        s.input.push(InputFile {
            id,
            path: vpath.clone(),
            state: FileState::Active,
            size: 1,
            kind: InputKind::Video,
        });
        s.processing.push(ProcFile {
            id,
            path: vpath.clone(),
            name: "film.mp4".to_string(),
            stt: 100.0,
            sum: 100.0,
            stage: "done".to_string(),
            msg: String::new(),
            selected: false,
            skipped: false,
        });
        let tdir = Store::transcript_dir();
        let _ = std::fs::create_dir_all(&tdir);
        let txt = tdir.join("phase2-probe-t.txt");
        std::fs::write(&txt, "transcript words").unwrap();
        s.phase2.insert(
            id,
            Phase2Job {
                display: "film.mp4".to_string(),
                stem: "film".to_string(),
                transcript_txt: txt.clone(),
            },
        );
        s.queue
            .output_write("T.md", "# T\n\n## Summary\nsum\n\n## Raw Transcript\nraw\n")
            .unwrap();
        s.complete_file(&vpath, &outdir.join("T.md").to_string_lossy());
        assert!(s.output.iter().any(|o| o.md_name == "T.md"));
        assert!(!txt.exists());
        assert!(!s.phase2.contains_key(&id));
        let _ = std::fs::remove_dir_all(&outdir);
        let _ = std::fs::remove_file(&txt);
    }

    #[test]
    fn declared_peers_come_from_merge_selection() {
        let mut s = Store::new();
        let a = PathBuf::from("C:/vids/a.mp4");
        let b = PathBuf::from("C:/vids/b.mp4");
        for (id, p) in [(11, &a), (22, &b)] {
            s.input.push(InputFile {
                id,
                path: p.clone(),
                state: FileState::Queued,
                size: 1,
                kind: InputKind::Video,
            });
        }
        assert!(s.declared_peers(11).is_empty());
        s.merge_selection.insert(11);
        s.merge_selection.insert(22);
        assert_eq!(s.declared_peers(11), vec!["b.mp4".to_string()]);
        // Sidecar round-trips through the real research dir (unique stem).
        s.write_declared_sidecar("probe-declared-4242.mp4", &s.declared_peers(11));
        let side = pv_backend::research::notes_dir_for("probe-declared-4242")
            .join("declared.md");
        let text = std::fs::read_to_string(&side).unwrap();
        assert!(text.contains("USER-DECLARED") || text.contains("Declared similar"));
        assert!(text.contains("b.mp4"));
        let _ = std::fs::remove_file(&side);
    }

    #[test]
    fn vlm_recommendation_prefers_fitting_complete_model() {
        use pv_backend::models::ModelStatus;
        let mut s = Store::new();
        // Nothing downloaded: falls back to the tier default from catalog.
        assert_eq!(
            s.vlm_for_attention(50).as_deref(),
            Some("qwen2-vl-7b-Q4_K_M")
        );
        let mk = |id: &str| ModelStatus {
            id: id.to_string(),
            role: "vlm".to_string(),
            file: String::new(),
            bytes: 1,
            tier: String::new(),
            note: String::new(),
            present: true,
            size: 1,
            complete: true,
            active: false,
            expected_sha: String::new(),
            unverified: false,
        };
        s.models.push(mk("qwen2-vl-2b-Q4_K_M"));
        // Only Small complete: recommended at every attention.
        assert_eq!(
            s.vlm_for_attention(90).as_deref(),
            Some("qwen2-vl-2b-Q4_K_M")
        );
        s.models.push(mk("qwen2-vl-7b-Q4_K_M"));
        // Academic now fits Medium; Overview still prefers Small.
        assert_eq!(
            s.vlm_for_attention(85).as_deref(),
            Some("qwen2-vl-7b-Q4_K_M")
        );
        assert_eq!(
            s.vlm_for_attention(15).as_deref(),
            Some("qwen2-vl-2b-Q4_K_M")
        );
    }

    #[test]
    fn retry_skip_need_backend() {
        let mut s = staged_proc();
        let id = s.processing[0].id;
        assert!(s.retry_file(id).is_err());
        assert!(s.skip_file(id).is_err());
        assert!(s.retry_file(9999).is_err());
    }

    #[test]
    fn heuristic_name_parses_day_and_class() {
        let p = PathBuf::from("Biology/lec12_intro.mp3");
        assert_eq!(Store::heuristic_name(&p), "Biology - Day 12 - lec12_intro");
        let p2 = PathBuf::from("Day 4 - mitosis.wav");
        assert_eq!(
            Store::heuristic_name(&p2),
            "General - Day 4 - mitosis"
        );
        let p3 = PathBuf::from("x/no-tokens-here.xyz");
        assert!(Store::heuristic_name(&p3).contains("Day 1"));
    }

    #[test]
    fn naive_diff_marks_add_remove() {
        let d = naive_diff("a\nb\nc", "a\nC\nd");
        assert!(d.contains(&('-', "b".to_string())));
        assert!(d.contains(&('-', "c".to_string())));
        assert!(d.contains(&('+', "C".to_string())));
        assert!(d.contains(&('+', "d".to_string())));
        assert_eq!(
            naive_diff("same\n", "same\n"),
            vec![(' ', "(no differences)".to_string())]
        );
    }

    /// Hermetic Store with its queue pinned to a temp dir. Writes a clean
    /// prefs file first so `Store::new()` never inherits live user state
    /// (e.g. a dismissed `"T.md"` silently swallowing the seed fixture).
    /// Callers MUST hold `PREFS_LOCK` + `PrefsGuard` (live-file restore).
    fn output_store(tag: &str) -> (Store, PathBuf) {
        let dir = std::env::temp_dir().join(format!("pv-out-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::write(pv_backend::prefs::prefs_path(), r#"{"dismissed":[]}"#).unwrap();
        let mut s = Store::new();
        s.queue = pv_backend::queue::Queue::new(Some(dir.clone()));
        (s, dir)
    }

    fn seed_output(s: &mut Store) -> u64 {
        s.queue
            .output_write("T.md", "# T\n\n## Summary\nsum\n\n## Raw Transcript\nraw\n")
            .unwrap();
        s.refresh_output();
        s.output.iter().find(|o| o.md_name == "T.md").unwrap().id
    }

    #[test]
    fn save_rename_delete_roundtrip() {
        let _plock = PREFS_LOCK.lock().unwrap();
        let _prefs = PrefsGuard::take();
        let (mut s, dir) = output_store("srd");
        let id = seed_output(&mut s);
        s.save_output_text(id, "# T\n\n## Summary\nnew\n\n## Raw Transcript\nraw2\n")
            .unwrap();
        let o = s.output.iter().find(|x| x.id == id).unwrap();
        assert_eq!(o.summary, "new");
        assert!(dir.join("T.md.diff.json").exists());
        let renamed = s.rename_output(id, "Renamed").unwrap();
        assert_eq!(renamed, "Renamed.md");
        assert!(dir.join("Renamed.md").exists() && !dir.join("T.md").exists());
        assert!(s.rename_output(id, "").is_err());
        s.delete_output(id).unwrap();
        assert!(s.output.is_empty() && !dir.join("Renamed.md").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Full failure drill: progress moves bars → error retires to Input as
    /// retryable (never wedges) → re-run reaches Done → unreadable Done
    /// retires too (never a ghost entry).
    #[test]
    fn pipeline_drill_progress_error_retry_done() {
        let _plock = PREFS_LOCK.lock().unwrap();
        let _prefs = PrefsGuard::take();
        let (mut s, dir) = output_store("drill");
        s.add_files(vec![PathBuf::from("a.wav")]);
        let id = s.input[0].id;
        let activate = |s: &mut Store, id: u64, path: PathBuf| {
            s.input.iter_mut().find(|f| f.id == id).unwrap().state = FileState::Active;
            s.processing.push(ProcFile {
                id,
                path,
                name: "a.wav".into(),
                stt: 0.0,
                sum: 0.0,
                stage: "decode".into(),
                msg: String::new(),
                selected: false,
                skipped: false,
            });
        };
        activate(&mut s, id, PathBuf::from("a.wav"));
        s.unlocked = true;
        // Progress lands on the STT bar.
        s.apply_event(Event::File {
            file: "a.wav".into(),
            stage: Stage::Transcribe,
            fraction: 0.5,
            message: "half".into(),
            aborted: false,
        });
        assert_eq!(s.processing[0].stt, 50.0);
        // Error retires (queue keeps draining afterwards).
        s.apply_event(Event::File {
            file: "a.wav".into(),
            stage: Stage::Error,
            fraction: 0.0,
            message: "boom".into(),
            aborted: false,
        });
        assert!(s.processing.is_empty());
        assert_eq!(s.input[0].state, FileState::Queued);
        assert!(s.file_warnings.contains_key(&PathBuf::from("a.wav")));
        // Retry reaches Done with a real snapshot.
        activate(&mut s, id, PathBuf::from("a.wav"));
        s.unlocked = true;
        s.queue
            .output_write("A.md", "# A\n\n## Summary\nsum\n\n## Raw Transcript\nraw\n")
            .unwrap();
        s.complete_file(&PathBuf::from("a.wav"), "A.md");
        assert_eq!(s.output.len(), 1);
        assert_eq!(s.output[0].summary, "sum");
        assert!(s.input.is_empty() && s.processing.is_empty());
        // Unreadable Done retires instead of ghosting.
        s.add_files(vec![PathBuf::from("b.mp3")]);
        let bid = s.input.iter().find(|f| f.path == PathBuf::from("b.mp3")).unwrap().id;
        activate(&mut s, bid, PathBuf::from("b.mp3"));
        s.unlocked = true;
        s.complete_file(&PathBuf::from("b.mp3"), "Missing.md");
        assert!(s.output.len() == 1);
        assert!(s.file_warnings.contains_key(&PathBuf::from("b.mp3")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dismiss_hides_row_but_keeps_file() {
        let _plock = PREFS_LOCK.lock().unwrap();
        let _prefs = PrefsGuard::take();
        let (mut s, dir) = output_store("dismiss");
        let id = seed_output(&mut s);
        s.dismiss_output(id).unwrap();
        assert!(s.output.is_empty());
        assert!(dir.join("T.md").exists());
        // Refresh must not resurrect the hidden row.
        s.refresh_output();
        assert!(s.output.is_empty());
        // …and the dismissal survives a fresh boot (persisted prefs).
        let s2 = Store::new();
        assert!(s2.dismissed.contains("T.md"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn output_ops_follow_file_dir_not_current_dir() {
        let _plock = PREFS_LOCK.lock().unwrap();
        let _prefs = PrefsGuard::take();
        let (mut s, dir_a) = output_store("dira");
        let id = seed_output(&mut s);
        // Retarget the queue AFTER the file landed: save/delete must still
        // hit dir_a (the file's own dir), never the new current dir.
        let dir_b = std::env::temp_dir().join(format!("pv-out-dirb-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir_b);
        s.queue.set_out_dir(dir_b.clone());
        s.save_output_text(id, "# T\n\n## Summary\nnew\n\n## Raw Transcript\nraw2")
            .unwrap();
        assert!(dir_a.join("T.md").exists());
        assert!(!dir_b.join("T.md").exists());
        s.delete_output(id).unwrap();
        assert!(s.output.is_empty());
        assert!(!dir_a.join("T.md").exists());
        let _ = std::fs::remove_dir_all(&dir_a);
        let _ = std::fs::remove_dir_all(&dir_b);
    }

    #[test]
    fn delete_missing_file_still_drops_row() {
        let _plock = PREFS_LOCK.lock().unwrap();
        let _prefs = PrefsGuard::take();
        let (mut s, dir) = output_store("ghost");
        let id = seed_output(&mut s);
        std::fs::remove_file(dir.join("T.md")).unwrap();
        s.delete_output(id).unwrap();
        assert!(s.output.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn download_tier_queues_missing_pair_offline() {
        let _plock = PREFS_LOCK.lock().unwrap();
        let _prefs = PrefsGuard::take();
        let (mut s, dir) = output_store("tier");
        // Embedded catalog resolves; spawn succeeds offline (failure, if any,
        // arrives later as an event, never as a return).
        let rxs = s.download_tier("lite").unwrap();
        // One receiver per actually-missing file (0-2 depending on disk).
        assert_eq!(rxs.len(), s.downloads.len());
        assert!(rxs.len() <= 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Serializes tests that retarget the process-global reviews override.
    static REVIEWS_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Points the drafts base at a temp dir; restores the default on drop.
    struct ReviewsGuard;
    impl ReviewsGuard {
        fn take(tag: &str) -> PathBuf {
            let dir =
                std::env::temp_dir().join(format!("pv-rev-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            pv_backend::dirs::set_reviews_dir_override(dir.clone());
            dir
        }
    }
    impl Drop for ReviewsGuard {
        fn drop(&mut self) {
            pv_backend::dirs::clear_reviews_dir_override();
        }
    }

    #[test]
    fn merge_select_toggle_and_group_dismiss_persists() {
        let _plock = PREFS_LOCK.lock().unwrap();
        let _prefs = PrefsGuard::take();
        let (mut s, dir) = output_store("mg");
        let id = seed_output(&mut s);
        s.toggle_merge_select(id);
        assert!(s.merge_selection.contains(&id));
        s.toggle_merge_select(id);
        assert!(!s.merge_selection.contains(&id));
        s.toggle_merge_select(999_999); // unknown id: ignored, never selected
        assert!(!s.merge_selection.contains(&999_999));
        // Dismiss a suggestion: hidden now + persisted across boots.
        s.suggested.push(MergeSuggestion {
            key: "A + B".to_string(),
            members: vec![(id, "A.md".into(), "note".into())],
            score: 0.5,
            band: "suggest".to_string(),
        });
        s.dismiss_group("A + B");
        assert!(s.suggested.is_empty());
        assert!(s.dismissed_groups.contains("A + B"));
        let s2 = Store::new();
        assert!(s2.dismissed_groups.contains("A + B"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn merge_thresholds_clamp_and_keep_order() {
        let _plock = PREFS_LOCK.lock().unwrap();
        let _prefs = PrefsGuard::take();
        let (mut s, dir) = output_store("th");
        s.set_merge_thresholds(0.9, 0.1); // inverted: hi rises to lo
        assert!((s.merge_suggest - 0.9).abs() < 1e-6);
        assert!(s.merge_prompt >= s.merge_suggest);
        s.set_merge_thresholds(-5.0, 50.0); // clamped to legal bands
        assert!(s.merge_suggest >= 0.05 && s.merge_prompt <= 0.99);
        assert!(s.merge_prompt >= s.merge_suggest);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn review_draft_roundtrip_badges_and_submit_clears() {
        let _plock = PREFS_LOCK.lock().unwrap();
        let _prefs = PrefsGuard::take();
        let _rlock = REVIEWS_LOCK.lock().unwrap();
        let _rev = ReviewsGuard::take("rt");
        let (mut s, dir) = output_store("rev");
        let id = seed_output(&mut s);
        s.open_review(id).unwrap();
        assert!(s.review.as_ref().is_some_and(|r| r.id == id));
        assert!(s.draft_badges.contains("T.md"));
        s.update_review_texts("raw!".into(), "sum!".into(), "center!".into());
        s.persist_review().unwrap();
        let stem = pv_backend::drafts::sanitize_stem("T");
        let back =
            pv_backend::drafts::load_draft(&pv_backend::drafts::drafts_base(), &stem).unwrap();
        assert_eq!(back.left, "raw!");
        assert_eq!(back.center, "center!");
        // Reopen loads the persisted panes (not the seed).
        s.close_review();
        s.open_review(id).unwrap();
        assert_eq!(s.review.as_ref().unwrap().left, "raw!");
        // Submit writes the .md, clears the session AND the draft.
        s.submit_review("# T\n\n## Summary\nsum!\n\n## Raw Transcript\nraw!\n".to_string())
            .unwrap();
        assert!(s.review.is_none());
        assert!(!s.draft_badges.contains("T.md"));
        let o = s.output.iter().find(|x| x.id == id).unwrap();
        assert_eq!(o.summary, "sum!");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(pv_backend::drafts::drafts_base());
    }

    #[test]
    fn review_revert_restores_snapshot() {
        let _plock = PREFS_LOCK.lock().unwrap();
        let _prefs = PrefsGuard::take();
        let _rlock = REVIEWS_LOCK.lock().unwrap();
        let _rev = ReviewsGuard::take("rv");
        let (mut s, dir) = output_store("rev2");
        let id = seed_output(&mut s);
        s.open_review(id).unwrap();
        s.review_merge();
        assert!(s.review.as_ref().unwrap().merged);
        s.review_revert();
        let r = s.review.as_ref().unwrap();
        assert!(!r.merged && r.center.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(pv_backend::drafts::drafts_base());
    }

    #[test]
    fn default_outdir_classes_and_unhide() {
        let _plock = PREFS_LOCK.lock().unwrap();
        let _prefs = PrefsGuard::take();
        let (mut s, dir) = output_store("set");
        // Seed while the queue points at the temp dir (never the live home).
        let id = seed_output(&mut s);
        let custom =
            std::env::temp_dir().join(format!("pv-out-custom-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&custom);
        s.set_default_outdir(custom.to_str().unwrap()).unwrap();
        assert_eq!(s.run_outdir().unwrap(), custom);
        assert_eq!(s.queue.out_dir(), custom.as_path());
        s.reset_default_outdir();
        assert!(s.run_outdir().is_none());
        assert!(s.set_default_outdir("").is_err());
        s.set_default_classes("Bio; Chem".to_string());
        assert_eq!(s.run_classes(), "Bio; Chem");
        // Re-pin the queue to the temp dir, then dismiss/unhide roundtrips.
        s.queue.set_out_dir(dir.clone());
        s.dismiss_output(id).unwrap();
        assert!(s.output.is_empty());
        s.unhide_dismissed();
        assert_eq!(s.output.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&custom);
        pv_backend::dirs::clear_outdir_override();
    }

    #[test]
    fn summary_tier_sets_chunk_ratio_and_persists() {
        let _plock = PREFS_LOCK.lock().unwrap();
        let _prefs = PrefsGuard::take();
        let (mut s, dir) = output_store("tier2");
        assert_eq!(Store::tier_chunk_tokens("recap"), 1000);
        assert_eq!(Store::tier_chunk_tokens("standard"), 4000);
        assert_eq!(Store::tier_chunk_tokens("detailed"), 8000);
        assert_eq!(Store::tier_ratio_pct("recap"), 25);
        assert_eq!(Store::tier_ratio_pct("standard"), 50);
        assert_eq!(Store::tier_ratio_pct("detailed"), 75);
        s.set_summary_tier("detailed");
        assert_eq!(s.summary_tier, "detailed");
        assert_eq!(s.chunk_tokens, 8000);
        s.set_summary_tier("bogus");
        assert_eq!(s.summary_tier, "standard");
        assert_eq!(s.chunk_tokens, 4000);
        // Custom sizes survive independently of the tier.
        s.set_chunk_tokens(2500);
        assert_eq!(s.chunk_tokens, 2500);
        assert_eq!(s.summary_tier, "standard");
        s.set_chunk_tokens(99_999);
        assert_eq!(s.chunk_tokens, 12000);
        // Gemma + Large warns; anything else stays quiet.
        s.active_llm = "gemma-2-9b-it-Q4_K_M".to_string();
        s.set_chunk_tokens(8000);
        assert!(s.chunk_model_note().is_some());
        s.active_llm = "llama-3.1-8b-it-Q4_K_M".to_string();
        assert!(s.chunk_model_note().is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sidecar_int_parses_coverage_fields() {
        let side = r#"{"title":"T","coverage":{"chunks":90,"fallbacks":3,"achieved_pct":47,"relaxed":true}}"#;
        assert_eq!(sidecar_int(side, "chunks"), Some(90));
        assert_eq!(sidecar_int(side, "fallbacks"), Some(3));
        assert_eq!(sidecar_int(side, "achieved_pct"), Some(47));
        assert_eq!(sidecar_int(side, "missing"), None);
        assert_eq!(sidecar_int("not json", "chunks"), None);
    }

    #[test]
    fn clean_caches_refuses_while_processing() {
        let _plock = PREFS_LOCK.lock().unwrap();
        let _prefs = PrefsGuard::take();
        let (mut s, dir) = output_store("cc");
        s.processing.push(ProcFile {
            id: 1,
            path: PathBuf::from("a.wav"),
            name: "a.wav".into(),
            stt: 0.0,
            sum: 0.0,
            stage: "decode".into(),
            msg: String::new(),
            selected: false,
            skipped: false,
        });
        s.clean_caches();
        assert!(!s.errors.is_empty());
        s.processing.clear();
        s.clean_caches();
        assert!(s.status.contains("Cleaned"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
