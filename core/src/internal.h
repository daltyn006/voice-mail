#pragma once
// Internal helpers shared by core/*.cpp. All Windows-only.
#include "../include/present_core.h"  // PvProgressCb + PV_* stages for hooks

#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <ctime>
#include <fstream>
#include <mutex>
#include <string>
#include <vector>

// MSVC + MinGW mkdir/access without pulling in <filesystem>
// (keeps clangd/MSVC/MinGW agreeable: no C++17 filesystem link quirks).
#include <direct.h>
#include <io.h>

namespace pv {

// 16kHz mono float PCM.
struct Audio {
    std::vector<float> pcm;
    int sample_rate = 16000;
};

// Decode ANY ffmpeg-supported file -> 16kHz mono (audio_ffmpeg.cpp).
// Loads avformat/avcodec/avutil/swresample DLLs at runtime from exe dir.
bool decode_to_16k_mono(const std::string& path, Audio& out, std::string& err);

// ---- STT (stt_whisper.cpp). whisper.cpp linked statically, Vulkan preferred.
struct SttConfig {
    std::string model_path;
    int window_sec = 30;   // per-window inference keeps VRAM flat
    std::string language = "auto";
    // NOTE: no temperature field by design — greedy decoding is hard-coded
    // (temperature-0 equivalent); a dormant field would imply tunability.
    bool cpu_only = false;  // skip GPU paths (explicit flag, not env)
    bool timestamps = false;  // video jobs: sentence-grouped [MM:SS] lines
    // Cross-part continuity: tail of Part N-1's transcript, fed as whisper's
    // first-window initial_prompt (carry_initial_prompt=false, so later
    // windows condition on live context as before). Empty = no seeding.
    std::string initial_prompt;
};
bool stt_transcribe(const Audio& audio, const SttConfig& cfg, std::string& out_text,
                    std::string& err);
// Spilled variant: same windowing, PCM streamed window-by-window from a raw
// float32LE sidecar (see SPILL_FLOATS in pipeline.cpp). Bounded peak RAM
// for multi-hour inputs; model lifecycle identical to stt_transcribe.
bool stt_transcribe_spilled(const std::string& pcm_path, size_t total_floats,
                            const SttConfig& cfg, std::string& out_text,
                            std::string& err);

// ---- LLM (llm_llama.cpp). llama.cpp linked statically. Only ONE model resident.
struct LlmConfig {
    std::string model_path;
    int n_ctx = 8192;
    // Must exceed guide (~1600 tok) + largest chunk (~1600 tok) + headroom:
    // llama_decode ABORTS the process past n_batch, so undersizing here is
    // a crash, not a slowdown. Per-call headroom clamping (see llm_llama.cpp)
    // degrades oversized inputs instead; batch stays fixed for the session.
    int n_batch = 4096;
    int n_predict = 1024;  // validation passes a tiny budget (e.g. 8)
    bool cpu_only = false;  // skip GPU layers (explicit flag, not env)
    // The AGENTS.md behavior supplement rides along by default, but content
    // calls (extract/summarize) MUST opt out: a 6KB guide drowns short chunk
    // notes under greedy decoding and the model ends up summarizing the
    // guide instead of the lecture (observed in the wild).
    bool include_guide = true;
    // Tier guide file stare ("recap" | "standard" | "detailed"); "" means
    // the legacy AGENTS.md. Content calls set the length tier's guide and
    // keep include_guide true; CLASS/validation leave it guideless/default.
    std::string guide_name;
    // Length tier driving ratio + prompts ("recap" | "standard" | "detailed";
    // normalized, default standard).
    std::string summary_tier;
};
bool llm_generate(const std::string& system, const std::string& prompt,
                  const LlmConfig& cfg, std::string& out, std::string& err);

// ---- Persistent LLM session: one model+ctx load per file, shared across
// all MAP chunks + REDUCE passes. Previously every llm_generate() call
// reloaded multi-GB weights from disk + Vulkan upload + teardown — hundreds
// of cycles on long docs (churn, fragmentation, hours of redundant IO).
// Single-threaded FIFO worker only; not thread-safe. Always close (even on
// abort/error); close tolerates partially-opened sessions. Generate keeps
// the session usable after soft failures; hard aborts still close+reopen
// at the caller.
struct LlmSession;
LlmSession* llm_session_open(const LlmConfig& cfg, std::string& err);
// n_predict_want > 0 overrides cfg.n_predict for this call (long-form
// REDUCE parts scale output length); -1 keeps the configured budget.
// Still clamped to batch/ctx headroom — never an assert.
bool llm_session_generate(LlmSession* s, const std::string& system,
                          const std::string& prompt, std::string& out,
                          std::string& err, int n_predict_want = -1);
void llm_session_close(LlmSession* s);

// ---- Agent guides (in agent_guide.cpp): bounded runtime behavior
// supplements prepended to LLM system prompts. Never fatal.
// "" = legacy AGENTS.md; otherwise config/guides/<name>.md (tier guides:
// "recap" | "standard" | "detailed"). Missing file -> built-in fallback.
std::string agent_guide();
std::string agent_guide(const std::string& name);

// ---- Summarize (in llm_llama.cpp): portion-wise MAP over token-sized
// chunks + hierarchical part-wise REDUCE scaled to the tier length ratio.
// Coverage accounting rides along so degraded portions are never silent.
struct SummaryStats {
    int chunks = 0;          // MAP portions attempted
    int fallbacks = 0;       // portions that fell back to extracts
    int grammar_fallbacks = 0;  // polish chunks kept verbatim (guard tripped)
    bool hierarchical = false;  // REDUCE split into parts
    bool relaxed = false;    // repetition detector relaxed the target
    bool verbatim = false;   // final is verbatim notes (context overflow)
    size_t src_words = 0;
};
struct SummaryResult {
    std::string text;
    std::string polished;  // readable transcript (grammar stage output)
    SummaryStats stats;
};
SummaryResult summarize_map_reduce(const std::string& transcript, const LlmConfig& cfg,
                                   int chunk_tokens, bool is_video = false,
                                   int attention = 50,
                                   const std::string& research_notes = "",
                                   const std::string& folder_context = "",
                                   const std::string& frame_captions = "",
                                   bool is_doc = false);

// ---- Vision captions (vlm_qwen.cpp, mtmd/Qwen2-VL family). One VLM session
// per film: text GGUF + mmproj projector. NEVER co-resident with STT or the
// summarizer — pipeline opens it after STT unloads, closes before the LLM
// loads (same VRAM-after-unload logging). Mock builds (no mtmd) degrade.
struct VlmConfig {
    std::string text_path;
    std::string mmproj_path;
    bool cpu_only = false;  // skip GPU layers (explicit flag, not env)
    int n_ctx = 8192;
    int n_batch = 4096;
};
struct VlmSession;
VlmSession* vlm_session_open(const VlmConfig& cfg, std::string& err);
// detail: 0 Overview (one line) / 1 Balanced (two lines) / 2 Academic.
// Caption covers visible content only; timestamps are the caller's job.
bool vlm_caption(VlmSession* s, const std::string& image_path, int detail,
                 std::string& out, std::string& err);
void vlm_session_close(VlmSession* s);
// Projector init + text model open, then unload. Failure degrades runs.
bool vlm_validate(const std::string& text_path, const std::string& mmproj_path,
                  bool cpu_only, std::string& err);

// ---- Diff (diff.cpp): line LCS -> JSON [{t:"+/-/ ", text, chunk}].
std::string diff_to_json(const std::string& raw, const std::string& summary);

// ---- Day/Class (meta_day_class.cpp + db.cpp).
// `display_path`: human-meaningful name for token parsing + DB memory.
// `real_path`: on-disk payload for the creation-date fallback (may equal
// display_path for plain audio jobs).
bool resolve_day_class(const std::string& display_path, const std::string& real_path,
                       const std::string& db_path, std::string& day_label,
                       std::string& class_hint);
void db_remember(const std::string& db_path, const std::string& audio_path,
                 const std::string& day_label, const std::string& class_label);
// Returns true if a remembered row existed (fills non-empty columns only).
bool db_lookup(const std::string& db_path, const std::string& audio_path, std::string& day_label,
               std::string& class_label);
std::string sanitize_filename(std::string s);

// ---- VRAM guard (vram_guard.cpp): DXGI usage 0..1, -1 if unknown.
float vram_usage_fraction();
bool vram_over_budget(int budget_pct);

// ---- Worker control points (implemented in pipeline.cpp). ----
// The FIFO worker is single-threaded; STT windows / LLM chunks call these so
// Pause/Abort take effect at safe points without leaking models.
// progress_hook forwards per-window/per-chunk progress to the GUI callback
// installed by run_one (no-op when no run is active).
void hook_begin(int file_index);
void progress_hook(int stage, float fraction, const std::string& message);
// Returns true to keep going, false when an abort was requested (consume-once).
// Sleeps while paused, so callers simply stop calling it to stay paused.
bool checkpoint();

// ---- Minimal path helpers (header-inline, no <filesystem>).
// basename("C:/a/b.wav") -> "b.wav"; stem -> "b";
// parent_name("C:/a/b.wav") -> "a". Never throws.
inline std::string pv_basename(const std::string& p) {
    size_t i = p.find_last_of("/\\");
    return (i == std::string::npos) ? p : p.substr(i + 1);
}
inline std::string pv_stem(const std::string& p) {
    std::string b = pv_basename(p);
    size_t d = b.rfind('.');
    return (d == std::string::npos || d == 0) ? b : b.substr(0, d);
}
inline std::string pv_parent_name(const std::string& p) {
    size_t i = p.find_last_of("/\\");
    if (i == std::string::npos || i == 0) return "";
    size_t j = p.find_last_of("/\\", i - 1);
    return (j == std::string::npos) ? p.substr(0, i) : p.substr(j + 1, i - j - 1);
}
inline bool pv_file_exists(const std::string& p) { return _access(p.c_str(), 0) == 0; }
// mkdir -p equivalent: creates each prefix, ignores "already exists".
inline void pv_make_dirs(const std::string& dir) {
    for (size_t i = 1; i < dir.size(); ++i) {
        if (dir[i] == '/' || dir[i] == '\\') {
            std::string pre = dir.substr(0, i);
            if (!pre.empty() && pre.back() != ':') _mkdir(pre.c_str());
        }
    }
    if (!dir.empty()) _mkdir(dir.c_str());
}

// ---- Backend file log (backend.log): pipeline errors/milestones land on
// disk, not just in the GUI status line. Best-effort, mutex-guarded, never
// throws — safe to call from worker threads and crash handlers.
inline std::string pv_data_dir() {
    const char* base = std::getenv("LOCALAPPDATA");
    return (base && *base) ? std::string(base) + "\\voice mail" : std::string(".");
}
inline bool pv_rotate_if_huge(const std::string& path) {
    // Keep runaway logs bounded (stderr.log hit multi-MB of vendor spam
    // live): past 5MB, shift to ".1" (dropping the previous ".1").
    FILE* q = nullptr;
#ifdef _WIN32
    fopen_s(&q, path.c_str(), "rb");
#else
    q = fopen(path.c_str(), "rb");
#endif
    if (!q) return false;
    fseek(q, 0, SEEK_END);
    long n = ftell(q);
    fclose(q);
    if (n < 5 * 1024 * 1024) return false;
    std::string prev = path + ".1";
    std::remove(prev.c_str());
    std::rename(path.c_str(), prev.c_str());
    return true;
}
inline void pv_log(const std::string& line) {
    // First call per process: capture everything the backends print
    // (ggml/whisper GGML_ABORT diagnostics, ffmpeg errors) into stderr.log.
    // A GUI-subsystem exe has no console, so without this those messages —
    // including the reason for most abort() deaths — vanish silently.
    static std::once_flag stdio_flag;
    std::call_once(stdio_flag, [] {
        std::string dir = pv_data_dir();
        std::string path =
            (dir == ".") ? std::string("stderr.log") : (dir + "\\stderr.log");
        if (dir != ".") _mkdir(dir.c_str());
        pv_rotate_if_huge(path);
        FILE* f = freopen(path.c_str(), "a", stderr);
        (void)f;
        std::string blog =
            (dir == ".") ? std::string("backend.log") : (dir + "\\backend.log");
        pv_rotate_if_huge(blog);
    });
    static std::mutex mu;
    std::lock_guard<std::mutex> l(mu);
    std::string dir = pv_data_dir();
    std::string path;
    if (dir == ".") {
        path = "backend.log";
    } else {
        _mkdir(dir.c_str());
        path = dir + "\\backend.log";
    }
    std::ofstream f(path, std::ios::app);
    if (f) f << "[" << std::time(nullptr) << "] " << line << "\n";
}

// ---- Audio pre-convert cache (audio_ffmpeg.cpp): big inputs (multi-GB
// video) are transcoded ONCE to 16k mono WAV under the data dir, then read
// directly. Retries/re-runs never re-decode. Keyed by path+size+mtime.
// Already-16k-mono WAVs skip conversion (header-sniffed, not by extension).
// `delete_converted` removes the cached file after a successful output.
std::string convert_cache_path(const std::string& src);
bool wav_is_16k_mono(const std::string& path);
bool read_wav_16k_mono(const std::string& path, Audio& out, std::string& err);
bool convert_to_wav_16k(const std::string& src, const std::string& dst, std::string& err);
bool load_audio_16k(const std::string& src, Audio& out, std::string& err, int idx,
                    std::string& used_path);

// ---- Audacity project import (audio_audacity.cpp): .aup3/.aup4/
// .aup3unsaved (Tenacity etc. share the AUP3 layout). Detection is cheap
// (extension + SQLite magic + 'AUDY' application_id); rendering produces
// one float32 mono mixdown WAV that flows through the normal ffmpeg path.
// Builds only with PV_HAVE_SQLITE; otherwise both report unavailable.
bool is_audacity_project(const std::string& path);
bool render_audacity_to_wav(const std::string& src, const std::string& dst, std::string& err);

// ---- SRT sidecar builder (srt.cpp, video jobs): timestamped transcript
// lines become numbered cues. Standalone TU so the test harness executes
// the real code.
long long parse_srt_stamp(const std::string& line, size_t& end_off);
std::string fmt_srt_ts(long long ms);
std::string srt_from_transcript(const std::string& raw, double duration_secs);

// ---- JSON string escaping (diff sidecars + audit sidecars): single owner.
// diff.cpp and pipeline.cpp previously carried identical copies — the next
// control-char fix lands here once, not twice.
inline std::string json_escape(const std::string& s) {
    std::string o;
    char b[8];
    for (unsigned char c : s) {
        switch (c) {
            case '"': o += "\\\""; break;
            case '\\': o += "\\\\"; break;
            case '\n': o += "\\n"; break;
            case '\r': o += "\\r"; break;
            case '\t': o += "\\t"; break;
            default:
                if (c < 0x20) {
                    snprintf(b, sizeof(b), "\\u%04x", c);
                    o += b;
                } else {
                    o += (char)c;
                }
        }
    }
    return o;
}
// ---- Command-line escaping (ffmpeg spawn sites + harness): Windows
// CreateProcessA quoting rules. No shell is involved, but the callee parses
// ONE command line back into argv: a `"` or trailing `\` inside a path would
// break out of quoting. Control chars are rejected outright.
inline bool qarg(const std::string& in, std::string& out) {
    for (unsigned char c : in) {
        if (c < 0x20 || c == 0x7f) return false;
    }
    out += '"';
    size_t back = 0;
    for (char c : in) {
        if (c == '\\') {
            ++back;
            continue;
        }
        if (c == '"') {
            out.append(back * 2 + 1, '\\');
            out += '"';
            back = 0;
            continue;
        }
        if (back) {
            out.append(back, '\\');
            back = 0;
        }
        out += c;
    }
    out.append(back * 2, '\\');  // trailing runs must not eat the close quote
    out += '"';
    return true;
}

// ---- Pre-STT denoise (audio_ffmpeg.cpp): adaptive room-tone gate + light
// 2:1 compressor over the in-RAM 16k mono copy. "off" is a no-op;
// "recommended" gates at floor+8dB, "aggressive" at floor+12dB. The archival
// file and convert cache are never touched — only this STT copy. False only
// on abort (checkpoint); deterministic, no allocations past scratch.
bool denoise_for_stt(std::vector<float>& pcm, const std::string& mode, int idx);

// ---- Video frame sampling (audio_ffmpeg.cpp): attention-driven base rate +
// showinfo scene-cut seeks, every frame timestamped exactly in frames.md.
// Temp dir swept on success. The source video is only ever read.
struct SampledFrame {
    std::string file;
    double t;
    bool cut;
};
bool sample_video_frames(const std::string& src, const std::string& shown, int idx,
                         int attention, std::string& frames_dir,
                         std::vector<SampledFrame>& frames, std::string& err);
void sweep_frames_dir(const std::string& dir);
// t=MM:SS / H:MM:SS formatter shared by the sampler and caption writer.
std::string fmt_t(double secs);

// ---- Hardware detector (hw_detect.cpp): first-run tier auto-pick.
struct HwInfo {
    float vram_gb = -1;  // largest local-budget adapter, -1 unknown
    float ram_gb = -1;   // total system RAM, -1 unknown
    unsigned long long disk_free_bytes = 0;
    bool disk_known = false;
};
bool query_hw(HwInfo& hw, const std::string& path_for_disk);
int pick_tier(const HwInfo& hw);  // 0 lite, 1 standard, 2 full

}  // namespace pv
