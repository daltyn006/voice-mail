#pragma once
// present_core.dll — C ABI consumed by the Tauri/Rust shell.
// Design: one file at a time (FIFO), internally chunked so VRAM stays under budget.
// STT and LLM are never resident simultaneously (1 speech + 1 LLM slot).

#include <cstdint>

#if defined(_WIN32) && defined(PRESENT_CORE_BUILD)
#define PV_API __declspec(dllexport)
#elif defined(_WIN32)
#define PV_API __declspec(dllimport)
#else
#define PV_API
#endif

#ifdef __cplusplus
extern "C" {
#endif

// C ABI version. Bump on ANY layout change to PvJobOptions or behavioral
// contract change the Rust side depends on. The shell refuses mismatched
// DLLs with a GUI message instead of running into undefined behavior
// (mixed exe/DLL installs previously crashed with no diagnosis).
#define PV_ABI_VERSION 8
PV_API int pv_abi_version(void);

// Progress stages delivered to GUI.
typedef enum PvStage {
    PV_DECODE = 0,
    PV_TRANSCRIBE = 1,
    PV_SUMMARIZE = 2,
    PV_TITLE = 3,
    PV_DIFF = 4,
    PV_DONE = 5,
    PV_ERROR = 6
} PvStage;

// file_index: position in FIFO batch. fraction: 0..1 within stage.
typedef void (*PvProgressCb)(int file_index, int stage, float fraction,
                             const char* message);

typedef struct PvJobOptions {
    const char* audio_path;   // input media, any ffmpeg-decodable format
    const char* stt_model;    // path to whisper GGML (large-v3-turbo default)
    const char* llm_model;    // path to instruct GGUF (gemma-2-9b default)
    const char* out_dir;      // where Title.md + sidecar .json go
    const char* known_classes;// optional ';'-separated class list for Class inference
    const char* db_path;      // optional sqlite path for Day/Class memory
    int chunk_tokens;         // AI tokens per summarize chunk (Small 1000 /
                              // Medium 4000 / Large 8000; custom 200-12000)
    int audio_window_sec;     // STT window per chunk (default 30, keeps VRAM flat)
    int vram_budget_pct;      // pause/shrink if DXGI usage exceeds this (default 80)
    int skip_summary;         // nonzero: transcribe + tiny LLM filename check only,
                              // no map-reduce summary (raw transcript to Output)
    int cpu_only;             // nonzero: skip all GPU paths (Vulkan/CUDA).
    int delete_converted;     // nonzero: delete the cached converted WAV
                              // after a successful output (originals untouched).
    int is_text;              // nonzero: audio_path is a UTF-8 text payload
                              // (extracted document / merged input) — skip
                              // decode+STT, summarize directly. Append-only.
    const char* display_name; // optional original filename for titles/Day/Class
                              // (doc caches + merges). NULL/empty = audio_path.
                              // Append-only ABI, same build both sides.
    const char* summary_tier; // "recap" (25%) | "standard" (50%) | "detailed" (75%):
                              // picks the tier guide + length ratio consistently.
                              // NULL/empty = standard. Append-only (ABI 4).
    const char* audio_retention; // "keep" (default, nothing deleted) | "delete"
                              // (queued source removed after a successful
                              // output) | "archive" ("<stem>.src<ext>" copied
                              // beside the .md). Failures/aborts/videos exempt.
                              // NULL/empty = keep. Append-only (ABI 5).
    const char* denoise_mode; // "recommended" (adaptive gate, default) | "off"
                              // | "aggressive". Applies to the STT copy only;
                              // archival takes stay untouched. Append-only (ABI 5).
    int is_video;             // nonzero: video job — timestamped STT sentences,
                              // frame sampling, opt-in research. The source is
                              // read in place: never copied, never deleted.
                              // Append-only (ABI 6).
    int web_research;         // nonzero: allow up-to-10 cited web sources for
                              // this video (Settings toggle is the master).
                              // Append-only (ABI 6).
    int attention;            // 0-100 attention slider: frame density +
                              // caption detail + length. 50 = Balanced.
                              // Append-only (ABI 6).
    const char* vlm_text_model;  // active vision text GGUF (Qwen2-VL family).
    const char* vlm_mmproj;      // active vision projector. Empty = no VLM:
                              // video runs degrade to transcript+research.
                              // Append-only (ABI 7).
    const char* transcript_path;  // phase-2 video pass: UTF-8 transcript from
                              // pass 1 (skip-summary). Non-empty skips
                              // decode+STT+denoise; frames+vision+REDUCE run
                              // normally. NULL/empty = transcribe. (ABI 8).
} PvJobOptions;

// Queue one file (FIFO). Thread-safe.
PV_API int pv_queue_add(const PvJobOptions* opts);
// Number of files still waiting (queued + active).
PV_API int pv_queue_pending(void);
// Cancel everything waiting (current chunk runs to a safe stop).
PV_API void pv_queue_clear(void);
// Pause (nonzero) / resume (0) the worker between safe points. Thread-safe.
// While paused the active file holds its model but makes no progress.
PV_API void pv_queue_pause(int on);
PV_API int pv_queue_paused(void);
// Stop the active file at the next safe point (no output written for it).
// Waiting jobs stay queued unless pv_queue_clear is also called. Thread-safe.
PV_API void pv_abort_current(void);
PV_API int pv_abort_and_join(int timeout_ms);
// Drop the first waiting (not yet active) job matching audio_path.
// Returns 1 if a job was removed, 0 if not found. Thread-safe.
PV_API int pv_queue_remove(const char* audio_path);

// Run the FIFO queue on a worker thread. progress may be NULL.
// Returns 0 on launch. Completion/errors arrive via progress callback.
PV_API int pv_run_async(PvProgressCb progress);
// Block until queue drains.
PV_API void pv_wait_idle(void);
// Quit-path shutdown: signal abort, wait bounded for the drain, then JOIN
// the worker (a joinable global thread destroyed at exit terminates the
// process — the close-crash). 0 = joined, 1 = timeout (detached; exit
// anyway), -1 = called on the worker thread itself. Additive: no ABI bump.

// ---- Synchronous single-file helpers (used by tests / CLI) ----
typedef struct PvResult {
    char* out_md_path;   // "Class - Day N - Title.md" full path (malloc'd, free with pv_free)
    char* raw_text;      // full transcript (malloc'd)
    char* summary_text;  // final summary (malloc'd)
    char* diff_json;     // [{t:"+/-/ ", text, chunk}] (malloc'd)
} PvResult;

PV_API int pv_process_file(const PvJobOptions* opts, PvProgressCb progress,
                           int file_index, PvResult* out);
PV_API void pv_free_result(PvResult* r);
PV_API void pv_free(void* p);

// Pre-run validation: init (+tiny discharge for) each backend in the
// run_one order, freed immediately after. 0 = ready; 1 = that backend
// failed OR bad args (null model path, tiny err buffer — err filled
// best-effort). Mock builds pass.
// Called before queueing so failures land on a labeled Loading step.
// `flags`: bit 0 (PV_VALIDATE_CPU_ONLY) skips GPU paths, mirroring cpu_only.
#define PV_VALIDATE_CPU_ONLY 1
PV_API int pv_validate_stt(const char* stt_model, char* err, int err_cap, int flags);
PV_API int pv_validate_llm(const char* llm_model, char* err, int err_cap, int flags);
// Vision pair validation: projector init + text model open, then unload.
// 0 = ready; 1 = failed or bad args (err filled best-effort).
PV_API int pv_validate_vlm(const char* text_model, const char* mmproj, int flags);

// Day/Class resolution (also used by GUI for pre-preview).
// day_label e.g. "Day 4"; class_label e.g. "Biology". Buffers must hold >=256 chars.
PV_API int pv_resolve_day_class(const char* audio_path, const char* db_path,
                                char* day_label, int day_cap,
                                char* class_hint, int class_cap);

// Hardware tier auto-pick for the first-run wizard (0 lite, 1 standard, 2 full).
// disk_free_bytes is for the drive holding path_for_disk (usually the exe dir);
// -1 values mean unknown. Never fails: worst case returns 1 (standard).
PV_API int pv_detect_tier(float* vram_gb, float* ram_gb,
                          long long* disk_free_bytes, const char* path_for_disk);

#ifdef __cplusplus
}
#endif
