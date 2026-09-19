// FIFO orchestrator: one file at a time, chunked inside so VRAM stays flat.
// STT and LLM never co-resident: transcribe (whisper) -> free -> summarize (llama) -> free.
#include "../include/present_core.h"
#include "internal.h"

#include <atomic>
#include <algorithm>
#include <cctype>
#include <chrono>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <exception>
#include <fstream>
#include <sstream>
#include <vector>
#include <mutex>
#include <queue>
#include <string>
#include <thread>

#ifdef _WIN32
#include <windows.h>
#endif

namespace {

struct Job {
    std::string audio, stt, llm, out, classes, db, display, summary_tier;
    std::string retention = "keep";  // keep | delete | archive (Section 4)
    std::string denoise = "recommended";  // recommended | off | aggressive
    std::string vlm_text, vlm_mmproj;  // active vision pair (empty = none)
    std::string transcript_path;  // phase-2: pass-1 transcript, skips STT
    int chunk_tokens = 1000, window_sec = 30, budget = 80;
    bool skip = false;  // skip map-reduce summary (raw + filename check only)
    bool cpu_only = false;  // explicit per-run flag (never env-dependent)
    bool delete_converted = false;  // drop the cached WAV after success
    bool is_text = false;  // text payload: skip decode+STT, summarize directly
    bool is_video = false;  // video: timestamped STT + frames + research.
                            // Source read in place — retention never applies.
    bool web_research = false;  // opt-in cited web sources (this job)
    int attention = 50;  // 0-100 attention slider (50 = Balanced)
};

// Head+tail sample for tiny inference prompts (CLASS/topic): the first and
// last `head`/`tail` chars joined by an ellipsis marker. Front-loads the
// topic, tail-loads conclusions — far more representative than head-only
// for story content with cold opens.
std::string sample_head_tail(const std::string& t, size_t head, size_t tail) {
    if (t.size() <= head + tail) return t;
    return t.substr(0, head) + "\n[…]\n" + t.substr(t.size() - tail);
}

// Past this many 16k-mono floats (~33 min), PCM spills to disk and STT
// streams windows from the sidecar instead of holding the whole film.
constexpr size_t SPILL_FLOATS = 32u * 1024u * 1024u;

// Sidecar path for spilled PCM: <data>/spill/<sanitized-stem>__<idx>.f32.
// Same confine-by-construction pattern as the convert cache (no user input
// in paths beyond a sanitized stem).
std::string spill_path_for(const std::string& shown, int idx) {
    std::string stem = pv::pv_stem(pv::pv_basename(shown));
    if (stem.empty()) stem = "audio";
    for (char& c : stem) {
        if (c == '<' || c == '>' || c == ':' || c == '"' || c == '/' || c == '\\' ||
            c == '|' || c == '?' || c == '*')
            c = '-';
    }
    if (stem.size() > 60) stem.resize(60);
    std::string dir = pv::pv_data_dir() + "\\spill";
    pv::pv_make_dirs(dir);
    return dir + "\\" + stem + "__" + std::to_string(idx) + ".f32";
}

bool write_spill(const std::string& path, const std::vector<float>& pcm, std::string& err) {
    std::ofstream f(path, std::ios::binary | std::ios::trunc);
    if (!f) {
        err = "cannot write spill file";
        return false;
    }
    f.write(reinterpret_cast<const char*>(pcm.data()),
            (std::streamsize)(pcm.size() * sizeof(float)));
    f.flush();
    if (!f) {
        err = "spill write failed";
        return false;
    }
    return true;
}

// Research stem: MUST mirror pv-backend research::notes_dir_for exactly
// (sanitize → trim dots → 60 chars), or the core will never find the notes.
std::string research_stem_for(const std::string& shown) {
    std::string s = pv::pv_stem(pv::pv_basename(shown));
    for (char& c : s) {
        if (c == '<' || c == '>' || c == ':' || c == '"' || c == '/' || c == '\\' ||
            c == '|' || c == '?' || c == '*' || (unsigned char)c < 0x20)
            c = '-';
    }
    auto trim = [](std::string& t) {
        size_t a = t.find_first_not_of(" \t\r\n.");
        size_t b = t.find_last_not_of(" \t\r\n.");
        t = (a == std::string::npos) ? "" : t.substr(a, b - a + 1);
    };
    trim(s);
    if (s.empty()) s = "research";
    if (s.size() > 60) s.resize(60);
    return s;
}

// Part-split continuity seed: when `shown` ends with " (Part N).<ext>" (N>1),
// look for Part N-1's already-written output (FIFO order means Part 1 usually
// finished first) and return the tail of its Raw Transcript as whisper's
// first-window initial_prompt. Best-effort: any miss returns "" (standalone
// transcription, files stay distinct either way).
#ifdef _WIN32
std::string part_seed_prompt(const std::string& shown, const std::string& out_dir) {
    std::string base = pv::pv_basename(shown);
    size_t dot = base.rfind('.');
    std::string stem = (dot == std::string::npos) ? base : base.substr(0, dot);
    const std::string tag = " (Part ";
    size_t tp = stem.rfind(tag);
    if (tp == std::string::npos) return "";
    int n = atoi(stem.c_str() + tp + tag.size());
    if (n <= 1) return "";
    std::string prev = stem.substr(0, tp) + " (Part " + std::to_string(n - 1) + ")";
    // Sidecars are "<title>.md.json" with an "audio" field holding the take
    // path: the Part-1 stem survives JSON escaping intact, so a substring
    // hit identifies the file without parsing JSON.
    std::string dir = out_dir.empty() ? "output" : out_dir;
    std::string pat = dir + "\\*.md.json";
    WIN32_FIND_DATAA fd{};
    HANDLE h = FindFirstFileA(pat.c_str(), &fd);
    if (h == INVALID_HANDLE_VALUE) return "";
    std::string md_path;
    int scanned = 0;
    do {
        if (scanned++ >= 200) break;
        std::string jp = dir + "\\" + fd.cFileName;
        std::ifstream f(jp, std::ios::binary);
        if (!f) continue;
        std::string t((std::istreambuf_iterator<char>(f)), std::istreambuf_iterator<char>());
        if (t.size() > 64 * 1024) continue;
        if (t.find(prev) != std::string::npos) {
            md_path = jp.substr(0, jp.size() - 5);  // strip ".json" -> the .md
            break;
        }
    } while (FindNextFileA(h, &fd));
    FindClose(h);
    if (md_path.empty()) return "";
    std::ifstream f(md_path, std::ios::binary);
    if (!f) return "";
    std::string t((std::istreambuf_iterator<char>(f)), std::istreambuf_iterator<char>());
    if (t.size() > 256 * 1024) t = t.substr(t.size() - 256 * 1024);
    size_t rt = t.find("## Raw Transcript");
    std::string raw = (rt == std::string::npos) ? t : t.substr(rt);
    if (raw.size() <= 400) return raw.size() > 40 ? raw : "";
    return raw.substr(raw.size() - 400);
}
#else
std::string part_seed_prompt(const std::string& shown, const std::string& out_dir) {
    (void)shown;
    (void)out_dir;
    return "";
}
#endif

// Pre-fetched web notes for this job (capped 12 KB ≈ 3k tokens). Empty when
// research is off, unrun, or failed — a normal degrade, never fatal.
std::string read_research_notes(const std::string& shown) {
    std::string p = pv::pv_data_dir() + "\\research\\" + research_stem_for(shown) +
                    "\\notes.md";
    std::ifstream f(p, std::ios::binary);
    if (!f) return "";
    std::string t((std::istreambuf_iterator<char>(f)), std::istreambuf_iterator<char>());
    if (t.size() > 12 * 1024) t.resize(12 * 1024);
    return t;
}

#ifdef _WIN32
struct MdEntry {
    std::string path;
    unsigned long long wtime = 0;  // FILETIME as u64 for newest-first sort
};

// Newest-first .md siblings in the output dir (cap 20) with their ## Summary
// section head (800 chars). Video-only ## Related fuel; proposals, not merges.
std::string gather_folder_context(const std::string& dir) {
    std::vector<MdEntry> found;
    std::string pat = dir + "\\*.md";
    WIN32_FIND_DATAA fd{};
    HANDLE h = FindFirstFileA(pat.c_str(), &fd);
    if (h == INVALID_HANDLE_VALUE) return "";
    do {
        if (strcmp(fd.cFileName, ".") != 0 && strcmp(fd.cFileName, "..") != 0) {
            ULARGE_INTEGER u;
            u.LowPart = fd.ftLastWriteTime.dwLowDateTime;
            u.HighPart = fd.ftLastWriteTime.dwHighDateTime;
            found.push_back({dir + "\\" + fd.cFileName, u.QuadPart});
        }
    } while (FindNextFileA(h, &fd));
    FindClose(h);
    std::sort(found.begin(), found.end(),
              [](const MdEntry& a, const MdEntry& b) { return a.wtime > b.wtime; });
    std::string ctx;
    size_t kept = 0;
    for (const auto& e : found) {
        if (kept >= 20) break;
        std::ifstream f(e.path, std::ios::binary);
        if (!f) continue;
        std::string t((std::istreambuf_iterator<char>(f)), std::istreambuf_iterator<char>());
        if (t.size() > 4000) t.resize(4000);
        // Head of the ## Summary section only — never whole files.
        size_t s = t.find("## Summary");
        std::string sum = (s == std::string::npos) ? t.substr(0, 800) : t.substr(s, 800);
        auto nm = e.path.rfind('\\');
        ctx += "\n[" + (nm == std::string::npos ? e.path : e.path.substr(nm + 1)) +
               "]\n" + sum + "\n";
        ++kept;
    }
    if (ctx.size() > 9000) ctx.resize(9000);
    return ctx;
}
#else
std::string gather_folder_context(const std::string& dir) {
    (void)dir;
    return "";
}
#endif

// Caption subset policy (Section 5b): Academic captions everything;
// Balanced takes cuts + every 4th base frame; Overview takes cuts + every
// 10th base frame (min 8, evenly spaced). frames.md always lists ALL
// sampled frames — only the subset costs VLM time.
std::vector<pv::SampledFrame> caption_subset(const std::vector<pv::SampledFrame>& all,
                                             int attention) {
    std::vector<pv::SampledFrame> sub;
    if (all.empty()) return sub;
    if (attention >= 67) return all;
    const size_t stride = attention >= 34 ? 4 : 10;
    size_t base_i = 0;
    for (const auto& f : all) {
        if (f.cut) {
            sub.push_back(f);
        } else {
            if (base_i % stride == 0) sub.push_back(f);
            ++base_i;
        }
    }
    if (sub.size() < 8 && all.size() > sub.size()) {
        // Sparse film, thin cuts: top up evenly spaced to 8.
        sub = all;
        if (sub.size() > 8) {
            std::vector<pv::SampledFrame> even;
            for (size_t k = 0; k < 8; ++k) {
                size_t p = k * sub.size() / 8;
                even.push_back(sub[p]);
            }
            sub = even;
        }
    }
    return sub;
}

// Rewrite frames.md with the sampled list plus filled ## Captions.
void rewrite_frames_captions(const std::string& frames_dir,
                             const std::vector<pv::SampledFrame>& all,
                             const std::vector<std::pair<double, std::string>>& caps) {
    std::string doc = "# Frames\n\nSampled " + std::to_string(all.size()) +
                      " frame(s); captioned " + std::to_string(caps.size()) + ".\n\n";
    for (const auto& f : all)
        doc += (f.cut ? "- [CUT " : "- [") + pv::fmt_t(f.t) + "] " + f.file + "\n";
    doc += "\n## Captions\n\n";
    for (const auto& c : caps)
        doc += "- [t=" + pv::fmt_t(c.first) + "] " + c.second + "\n";
    FILE* fo = nullptr;
    if (fopen_s(&fo, (frames_dir + "\\frames.md").c_str(), "wb") == 0 && fo) {
        fwrite(doc.data(), 1, doc.size(), fo);
        fclose(fo);
    }
}

bool read_text_file(const std::string& path, std::string& out, std::string& err) {
    std::ifstream f(path, std::ios::binary);
    if (!f) {
        err = "cannot read text payload";
        return false;
    }
    std::string t((std::istreambuf_iterator<char>(f)), std::istreambuf_iterator<char>());
    // strip UTF-8 BOM
    if (t.size() >= 3 && (unsigned char)t[0] == 0xEF && (unsigned char)t[1] == 0xBB &&
        (unsigned char)t[2] == 0xBF) {
        t.erase(0, 3);
    }
    // trim trailing whitespace-only collapse guard
    size_t a = t.find_first_not_of(" \t\r\n");
    if (a == std::string::npos) {
        err = "no extractable text (empty)";
        return false;
    }
    out = t;
    return true;
}
std::mutex g_mu;
std::queue<Job> g_q;
std::thread g_worker;
std::atomic<bool> g_running{false};
// Global pop counter: every popped file gets a fresh index, monotonically
// across worker runs. The Rust side keeps an append-only pop history, so
// `order[i]` always equals the i-th popped path — including across runs.
// Reset ONLY in pv_queue_clear, which the shell pairs with a full order
// clear + epoch bump (abort_all). Never reset per worker run: that
// misattributed every second run's events to stale paths.
std::atomic<int> g_file_idx{0};
PvProgressCb g_cb = nullptr;

void emit(int idx, PvStage s, float f, const std::string& m) {
    if (g_cb) g_cb(idx, (int)s, f, m.c_str());
}

// Write a whole file, verifying the stream. Returns false (already logged)
// on failure so callers never emit Done for output that isn't on disk —
// the audit found ghost entries were worse than failures.
bool write_file(const std::string& path, const std::string& content) {
    std::ofstream f(path, std::ios::binary | std::ios::trunc);
    f << content;
    f.flush();
    if (!f) {
        pv::pv_log("output write FAILED " + path);
        return false;
    }
    return true;
}

}  // namespace

// ---- Worker control + progress bridge (pv::hook_begin/progress_hook/checkpoint).
// Single worker thread consumes these; the UI thread only sets the flags.
// NOTE: this is top-level ::pv and must NOT move into the anonymous
// namespace below — nested `namespace pv` would shadow ::pv and break every
// pv:: lookup in this file.
namespace pv {
namespace detail {
std::atomic<bool> pause_flag{false};
std::atomic<bool> abort_flag{false};
std::mutex hook_mu;
int hook_idx = 0;
}  // namespace detail

void hook_begin(int file_index) {
    std::lock_guard<std::mutex> l(detail::hook_mu);
    detail::hook_idx = file_index;
}

void progress_hook(int stage, float fraction, const std::string& message) {
    int idx;
    PvProgressCb cb;
    {
        std::lock_guard<std::mutex> l(g_mu);
        std::lock_guard<std::mutex> h(detail::hook_mu);
        idx = detail::hook_idx;
        cb = g_cb;
    }
    if (cb) cb(idx, stage, fraction, message.c_str());
}

bool checkpoint() {
    for (;;) {
        if (detail::abort_flag.exchange(false)) return false;  // consume-once
        if (!detail::pause_flag.load()) return true;
        std::this_thread::sleep_for(std::chrono::milliseconds(50));
    }
}

void ctl_pause(int on) { detail::pause_flag.store(on != 0); }
int ctl_paused() { return detail::pause_flag.load() ? 1 : 0; }
void ctl_abort() { detail::abort_flag.store(true); }
}  // namespace pv

namespace {
char* dup(const std::string& s) {
    char* p = (char*)malloc(s.size() + 1);
    if (p) memcpy(p, s.c_str(), s.size() + 1);
    return p;
}
// Cap titles so title+dir stay far from MAX_PATH (a voided write would
// otherwise surface as "Done but unreadable"). UTF-8 safe: never split a
// codepoint, re-trim trailing space/dot like sanitize_filename.
std::string cap_title(std::string t, size_t maxn) {
    if (t.size() > maxn) {
        t.resize(maxn);
        // unsigned comparison: char signedness is implementation-defined.
        while (!t.empty() && ((unsigned char)t.back() & 0xC0) == 0x80) t.pop_back();
    }
    // Trailing separators never read as titles ("…so far-", "…Day -").
    // Internal " - " separators are untouched — only the end is trimmed.
    while (!t.empty() && (t.back() == ' ' || t.back() == '.' || t.back() == '-')) t.pop_back();
    if (t.empty()) t = "Untitled";
    return t;
}
// Single-line, filename-safe label from free LLM text (strips newlines/control chars).
std::string clean_label(std::string s) {
    std::string o;
    bool space = false;
    for (unsigned char c : s) {
        if (c < 0x20 || c == 0x7f) {
            space = !o.empty();
            continue;
        }
        if (c == ' ' || c == '\t') {
            space = !o.empty();
            continue;
        }
        if (space && !o.empty()) o += ' ';
        space = false;
        o += (char)c;
    }
    while (!o.empty() && o.back() == ' ') o.pop_back();
    if (o.size() > 80) {  // cut at word boundary
        size_t p = o.rfind(' ', 80);
        o.resize(p == std::string::npos ? 80 : p);
    }
    if (o.empty()) o = "Untitled";
    return o;
}
// Strip LLM summary boilerplate from the front of a would-be title topic:
// markdown headers ("## …"), bold leads ("**Executive Summary**:"), bullets
// ("- ", "* ", "• "), video section headers, and generic lead words.
// Case-insensitive; repeats.
std::string strip_topic_lead(std::string s) {
    static const char* leads[] = {"executive summary", "summary", "key points",
                                  "overview", "tldr", "tl;dr", "what happens",
                                  "what it is about", "what the ai makes of it",
                                  "related", "the compressed whole"};
    // Leading unicode punctuation the ASCII pass can't see: em/en dashes,
    // curly quotes, guillemets, ellipsis (models echo prompt scaffolding
    // with these attached — observed live in video section headers).
    // NOTE: no brackets/parens here — strip_bracketed owns [...] groups and
    // needs both halves intact (eating "[" orphans the stamp instead).
    static const char* upunct[] = {"\xE2\x80\x94", "\xE2\x80\x93", "\xE2\x80\x9C",
                                   "\xE2\x80\x9D", "\xE2\x80\x98", "\xE2\x80\x99",
                                   "\xC2\xAB", "\xC2\xBB", "\xE2\x80\xA6",
                                   "\xC2\xAC", "\"", "'"};
    for (;;) {
        size_t i = 0;
        // Newlines lead too: video topics start right after a "## Summary"
        // header ("\n\n…"), and without these the whole strip never engages
        // (observed live: "## What happens" survived into a filename).
        while (i < s.size() && (s[i] == ' ' || s[i] == '\t' || s[i] == '\r' || s[i] == '\n' ||
                                s[i] == '#' || s[i] == '*' ||
                                s[i] == '-' || s[i] == '+' || s[i] == '>' || s[i] == ':' ||
                                s[i] == ',' || s[i] == '.' || s[i] == ';')) {
            // Careful: a leading '-' could be a real hyphenated word; only
            // strip it when followed by space (bullet) or at string end.
            if (s[i] == '-' && !(i + 1 >= s.size() || s[i + 1] == ' ')) break;
            ++i;
        }
        // UTF-8 bullet '•' (E2 80 A2).
        while (i + 2 < s.size() && (unsigned char)s[i] == 0xE2 &&
               (unsigned char)s[i + 1] == 0x80 && (unsigned char)s[i + 2] == 0xA2) {
            i += 3;
            while (i < s.size() && s[i] == ' ') ++i;
        }
        bool cut = i > 0;
        s.erase(0, i);
        // Unicode punctuation front (see above): strip + trailing spaces.
        for (;;) {
            bool ucut = false;
            for (const char* u : upunct) {
                size_t n = strlen(u);
                if (s.compare(0, n, u) == 0) {
                    s.erase(0, n);
                    ucut = true;
                    break;
                }
            }
            if (!ucut) break;
            cut = true;
            size_t j = 0;
            while (j < s.size() && (s[j] == ' ' || s[j] == '\t')) ++j;
            s.erase(0, j);
        }
        std::string low = s;
        for (char& c : low) c = (char)tolower((unsigned char)c);
        for (const char* lead : leads) {
            size_t n = strlen(lead);
            if (low.compare(0, n, lead) == 0 &&
                (low.size() == n || low[n] == ' ' || low[n] == ':' || low[n] == '-' ||
                 low[n] == ',' || low[n] == '.')) {
                s.erase(0, n);
                low.erase(0, n);
                cut = true;
                break;
            }
        }
        if (!cut) break;
    }
    return s;
}
// Drop bracketed fragments ("[t=00:10]", "[00-00]") from title topics:
// video summaries/captions are full of timestamp tags that read as noise
// in filenames. Balanced brackets only; unbalanced text is kept verbatim.
std::string strip_bracketed(std::string s) {
    std::string o;
    for (size_t i = 0; i < s.size();) {
        if (s[i] == '[') {
            size_t j = s.find(']', i + 1);
            if (j != std::string::npos && j - i <= 24) {
                i = j + 1;
                continue;
            }
        }
        o += s[i++];
    }
        return o;
}

// ---- SRT sidecar: see srt.cpp (standalone TU so the harness executes it).
// ---- JSON escaping: pv::json_escape in internal.h (shared with diff.cpp).


int run_one(const Job& j, int idx, PvResult* out) {
    pv::hook_begin(idx);
    if (!pv::checkpoint()) return -3;  // aborted before start
    const std::string& shown = j.display.empty() ? j.audio : j.display;
    pv::pv_log("start [" + std::to_string(idx) + "] " + j.audio);
    std::string err;
    std::string raw;
    std::string pcm_src = j.audio;
    double media_secs = -1.0;  // STT input length; feeds the SRT last-cue end
    if (!j.transcript_path.empty()) {
        // Phase-2 video pass: pass-1 transcript supplied, skip the entire
        // audio front-end (decode+denoise+STT). Frames sample from j.audio
        // (the video, still read in place) and grammar keeps video stamps.
        emit(idx, PV_DECODE, 0.05f, "Transcript provided, skipping STT.");
        if (!read_text_file(j.transcript_path, raw, err)) {
            pv::pv_log("transcript FAILED [" + std::to_string(idx) + "] " +
                       j.transcript_path + ": " + err);
            emit(idx, PV_ERROR, 0.0f, "transcript failed: " + err);
            return -1;
        }
        emit(idx, PV_TRANSCRIBE, 1.0f, "Transcript ready, skipping STT.");
    } else if (j.is_text) {
        // Document / merged payload: no decode, no STT.
        emit(idx, PV_DECODE, 0.05f, "Reading text " + pv::pv_basename(shown));
        if (!read_text_file(j.audio, raw, err)) {
            pv::pv_log("text FAILED [" + std::to_string(idx) + "] " + j.audio + ": " + err);
            emit(idx, PV_ERROR, 0.0f, "text failed: " + err);
            return -1;
        }
        emit(idx, PV_TRANSCRIBE, 1.0f, "Text ready, skipping STT.");
    } else {
        emit(idx, PV_DECODE, 0.05f, "Preparing audio " + pv::pv_basename(shown));
        pv::Audio audio;
        if (!pv::load_audio_16k(j.audio, audio, err, idx, pcm_src)) {
            pv::pv_log("audio FAILED [" + std::to_string(idx) + "] " + j.audio + ": " + err);
            emit(idx, PV_ERROR, 0.0f, "audio failed: " + err);
            return err == "aborted" ? -3 : -1;
        }
        if (!pv::checkpoint()) return -3;
        // Pre-STT denoise on the in-RAM copy (archival file + convert cache
        // untouched). Abort here surfaces as -3 like any other safe point.
        emit(idx, PV_TRANSCRIBE, 0.15f, "Reducing noise...");
        if (!pv::denoise_for_stt(audio.pcm, j.denoise, idx)) return -3;

        // VRAM guard before loading STT.
        if (pv::vram_over_budget(j.budget)) emit(idx, PV_TRANSCRIBE, 0.0f, "VRAM high, waiting...");

        emit(idx, PV_TRANSCRIBE, 0.3f, "Loading STT model...");
        pv::SttConfig sc{j.stt, j.window_sec};
        sc.cpu_only = j.cpu_only;
        sc.timestamps = j.is_video;  // video: sentence-grouped [MM:SS] lines
        // Gapless-split takes: seed Part N>1 with Part N-1's transcript tail
        // (already on disk in FIFO order); absent = standalone, never fatal.
        sc.initial_prompt = part_seed_prompt(shown, j.out.empty() ? "output" : j.out);
        if (!sc.initial_prompt.empty())
            pv::pv_log("seeding STT prompt from previous part (" +
                       std::to_string(sc.initial_prompt.size()) + " chars)");
        // Portion-wise long audio: past SPILL_FLOATS (~33 min at 16k mono)
        // the full PCM no longer fits comfortably beside the model, so spill
        // it to a raw sidecar and stream window-by-window from disk. Peak
        // drops from ~2GB (3h film + decode double-buffer) to one window.
        const size_t total_floats = audio.pcm.size();
        std::string spill;
        bool spilled = false;
        if (total_floats > SPILL_FLOATS) {
            spill = spill_path_for(shown, idx);
            if (write_spill(spill, audio.pcm, err)) {
                audio.pcm.clear();
                audio.pcm.shrink_to_fit();
                spilled = true;
                pv::pv_log("spilled PCM [" + std::to_string(idx) + "] " + spill);
            } else {
                pv::pv_log("spill FAILED [" + std::to_string(idx) + "], in-RAM fallback: " + err);
                err.clear();
            }
        }
        bool stt_ok;
        if (spilled) {
            stt_ok = pv::stt_transcribe_spilled(spill, total_floats, sc, raw, err);
            std::remove(spill.c_str());  // best-effort; orphans swept by Clean caches
        } else {
            stt_ok = pv::stt_transcribe(audio, sc, raw, err);
        }
        if (!stt_ok) {
            // Aborts surface as err == "aborted" from inside the window loop.
            pv::pv_log("stt FAILED [" + std::to_string(idx) + "] " + j.audio + ": " + err);
            emit(idx, PV_ERROR, 0.0f, "transcribe failed: " + err);
            return err == "aborted" ? -3 : -2;
        }
        media_secs = (double)total_floats / 16000.0;
        audio.pcm.clear();
        audio.pcm.shrink_to_fit();  // STT freed inside; RAM back before LLM loads
        emit(idx, PV_TRANSCRIBE, 1.0f, "Transcript done, STT unloaded.");
    }
    {
        // Lifecycle proof for multi-file runs: VRAM must fall back here,
        // proving the STT model is really gone before the LLM loads.
        float v = pv::vram_usage_fraction();
        char b[64];
        if (v < 0)
            snprintf(b, sizeof(b), "vram after stt unload: unknown");
        else
            snprintf(b, sizeof(b), "vram after stt unload: %.2f", (double)v);
        pv::pv_log(b);
    }
    if (!pv::checkpoint()) return -3;

    // ---- Video pre-summarize: frame sampling (Section 5a). The source is
    // read in place; failures degrade to transcript-only (never fatal).
    // VLM captions (5b) will consume frames_dir before the sweep lands.
    std::string frames_dir;
    size_t n_frames = 0;
    std::vector<pv::SampledFrame> sampled;
    if (j.is_video && !j.skip) {
        std::vector<pv::SampledFrame> frames;
        if (pv::sample_video_frames(j.audio, shown, idx, j.attention, frames_dir, frames,
                                    err)) {
            n_frames = frames.size();
            sampled = frames;
        } else {
            if (err == "aborted") return -3;
            pv::pv_log("frames FAILED [" + std::to_string(idx) + "], transcript-only: " + err);
            err.clear();
            frames_dir.clear();
        }
        if (!pv::checkpoint()) return -3;
    }

    // ---- Research handoff (Section 5a): pre-fetched notes from the Rust
    // side (`<data>/research/<stem>/notes.md`, written BEFORE queueing).
    // Film-grounded sections always render first; these stay fenced.
    std::string research_notes;
    size_t n_sources = 0;
    if (j.is_video && j.web_research && !j.skip) {
        research_notes = read_research_notes(shown);
        if (!research_notes.empty()) {
            size_t p = 0;
            while ((p = research_notes.find("\n## ", p)) != std::string::npos) {
                ++n_sources;
                ++p;
            }
            char rb[96];
            snprintf(rb, sizeof(rb), "research: %u source(s)", (unsigned)n_sources);
            pv::pv_log(rb);
        }
    }

    // ---- Folder context (Section 5a, video-only trigger): newest ≤20 .md
    // siblings' summaries for the ## Related proposals (never merges).
    // User-declared pairs (declared.md sidecar) lead, marked confirmed.
    std::string folder_context;
    if (j.is_video && !j.skip) {
        folder_context = gather_folder_context(j.out.empty() ? "output" : j.out);
        std::string declared_path = pv::pv_data_dir() + "\\research\\" +
                                    research_stem_for(shown) + "\\declared.md";
        std::ifstream df(declared_path, std::ios::binary);
        if (df) {
            std::string dt((std::istreambuf_iterator<char>(df)),
                           std::istreambuf_iterator<char>());
            if (!dt.empty()) {
                if (dt.size() > 2000) dt.resize(2000);
                folder_context = "USER-DECLARED similar pairs (confirmed by the user — "
                                 "compare directly, citing timestamps):\n" +
                                 dt + "\n" + folder_context;
            }
        }
    }

    // ---- Vision captions (Section 5b, VLM session, vision runs LAST).
    // Subset policy: Academic captions everything (≤512); Balanced takes
    // cuts + every 4th base frame; Overview takes cuts + every 10th base
    // frame (min 8, evenly spaced). frames.md lists ALL sampled frames;
    // only the subset is captioned. VLM failures degrade to
    // transcript+research (logged, never fatal); user abort returns -3.
    std::string frame_captions;
    size_t n_captioned = 0;
    bool captions_ok = false;
    if (j.is_video && !j.skip && !frames_dir.empty()) {
        std::vector<pv::SampledFrame> subset = caption_subset(sampled, j.attention);
        for (const auto& f : subset) {
            char sb[96];
            snprintf(sb, sizeof(sb), "subset %s t=%s%s", f.file.c_str(),
                     pv::fmt_t(f.t).c_str(), f.cut ? " (cut)" : "");
            pv::pv_log(sb);
        }
        if (!subset.empty() && !j.vlm_text.empty() && !j.vlm_mmproj.empty()) {
            emit(idx, PV_SUMMARIZE, 0.32f, "Loading vision model...");
            pv::VlmConfig vc;
            vc.text_path = j.vlm_text;
            vc.mmproj_path = j.vlm_mmproj;
            vc.cpu_only = j.cpu_only;
            std::string verr;
            pv::VlmSession* vsess = pv::vlm_session_open(vc, verr);
            if (!vsess) {
                pv::pv_log("vlm FAILED [" + std::to_string(idx) + "], transcript-only: " + verr);
            } else {
                struct VCloser {
                    pv::VlmSession* s;
                    ~VCloser() { pv::vlm_session_close(s); }
                } vc_{vsess};
                const int detail = j.attention >= 67 ? 2 : (j.attention >= 34 ? 1 : 0);
                std::vector<std::pair<double, std::string>> caps;
                bool aborted = false;
                for (size_t k = 0; k < subset.size(); ++k) {
                    const auto& f = subset[k];
                    emit(idx, PV_SUMMARIZE,
                         0.32f + 0.08f * (float)k / (float)subset.size(),
                         "Watching frames (" + std::to_string(k + 1) + "/" +
                             std::to_string(subset.size()) + ")...");
                    if (!pv::checkpoint()) {
                        aborted = true;
                        break;
                    }
                    std::string cap, cerr;
                    if (pv::vlm_caption(vsess, frames_dir + "\\" + f.file, detail, cap,
                                        cerr)) {
                        caps.push_back({f.t, cap});
                        char cb2[128];
                        snprintf(cb2, sizeof(cb2), "captioned %s t=%s (%u chars)",
                                 f.file.c_str(), pv::fmt_t(f.t).c_str(),
                                 (unsigned)cap.size());
                        pv::pv_log(cb2);
                    } else if (cerr == "aborted") {
                        aborted = true;
                        break;
                    } else {
                        pv::pv_log("frame caption skipped " + f.file + ": " + cerr);
                    }
                }
                if (aborted) return -3;
                if (!caps.empty()) {
                    for (const auto& c : caps)
                        frame_captions += "[t=" + pv::fmt_t(c.first) + "] " + c.second + "\n";
                    n_captioned = caps.size();
                    captions_ok = true;
                    rewrite_frames_captions(frames_dir, sampled, caps);
                    char cb[128];
                    snprintf(cb, sizeof(cb), "captioned %u/%u frames",
                             (unsigned)caps.size(), (unsigned)sampled.size());
                    pv::pv_log(cb);
                }
                emit(idx, PV_SUMMARIZE, 0.4f, "Summary done prep, VLM unloaded.");
                {
                    float v = pv::vram_usage_fraction();
                    char b[64];
                    if (v < 0)
                        snprintf(b, sizeof(b), "vram after vlm unload: unknown");
                    else
                        snprintf(b, sizeof(b), "vram after vlm unload: %.2f", (double)v);
                    pv::pv_log(b);
                }
            }
        } else if (subset.empty()) {
            pv::pv_log("no frames to caption, transcript-only");
        } else {
            pv::pv_log("no active vision model, transcript+research only");
        }
        if (!pv::checkpoint()) return -3;
    }

    std::string summary;
    pv::SummaryStats cov;  // portion accounting for the sidecar + status line
    if (j.skip) {
        // Skip map-reduce: straight to Output with raw text only.
        emit(idx, PV_SUMMARIZE, 1.0f, "Summary skipped by user.");
    } else {
        emit(idx, PV_SUMMARIZE, 0.4f, "Loading LLM...");
        pv::LlmConfig lc{j.llm};
        lc.cpu_only = j.cpu_only;
        lc.summary_tier = j.summary_tier.empty() ? "standard" : j.summary_tier;
        lc.guide_name = lc.summary_tier;  // tier guide rides along by design
        lc.include_guide = true;
        // Batch/context derive from the chunk size (Small fits everywhere;
        // Large needs headroom): prompt est + margin, context = batch + tail.
        // NOTE: no n_batch shrink when hot. The persistent session fixes
        // batch at open and the per-call prompt guard degrades oversized
        // inputs; forcing 128 only produced silent extractive garbage while
        // barely denting VRAM (weights + KV dominate, not batch).
        const int est = (j.chunk_tokens > 0 ? j.chunk_tokens : 1000) + 512;
        int nb = est + 2048;
        if (nb < 2048) nb = 2048;
        if (nb > 16384) nb = 16384;
        lc.n_batch = nb;
        int nc = nb + 2048;
        if (nc < 8192) nc = 8192;
        if (nc > 32768) nc = 32768;
        lc.n_ctx = nc;
        pv::SummaryResult sr = pv::summarize_map_reduce(
            raw, lc, j.chunk_tokens > 0 ? j.chunk_tokens : 1000, j.is_video, j.attention,
            research_notes, folder_context, frame_captions, j.is_text);
        cov = sr.stats;
        summary = sr.text;
        if (summary == "__ABORTED__") {
            pv::pv_log("llm ABORTED [" + std::to_string(idx) + "] " + j.audio);
            return -3;
        }
        // Grammar stage output becomes the transcript from here on (summary,
        // diff, titles, class basis all read readable-space). Skip-summary
        // jobs never reach this branch, so they stay verbatim by design.
        if (!sr.polished.empty()) raw = sr.polished;
        emit(idx, PV_SUMMARIZE, 1.0f, "Summary done, LLM unloaded.");
        {
            // Lifecycle proof: VRAM must fall back here too, proving the
            // LLM is really gone before the next file loads STT again.
            float v = pv::vram_usage_fraction();
            char b[64];
            if (v < 0)
                snprintf(b, sizeof(b), "vram after llm unload: unknown");
            else
                snprintf(b, sizeof(b), "vram after llm unload: %.2f", (double)v);
            pv::pv_log(b);
        }
        if (!pv::checkpoint()) return -3;
    }

    emit(idx, PV_TITLE, 0.8f, "Resolving title...");
    std::string day, hint;
    // Display name for tokens/memory, real payload path for the file-date
    // fallback (doc caches + merges pass a bare filename as display).
    pv::resolve_day_class(shown, j.audio, j.db, day, hint);
    // Class: prefer LLM inference constrained by known list; fallback folder hint.
    // This doubles as the tiny filename check for skip-summary jobs (raw text in).
    std::string cls = clean_label(hint);
    const std::string& basis_full = summary.empty() ? raw : summary;
    // Titles/class come from prose, never the vision appendix: captions are
    // timestamped fragments ("[t=00:10] red frame") that would otherwise leak
    // markers into filenames.
    std::string basis = basis_full;
    {
        size_t cut = basis.find("## Frame captions");
        if (cut != std::string::npos) basis = basis.substr(0, cut);
    }
    if (!j.classes.empty()) {
        std::string llm_cls, e2;
        pv::LlmConfig lc2{j.llm};
        lc2.cpu_only = j.cpu_only;
        lc2.include_guide = false;  // content only: 800-char basis would drown
                                    // in the 6KB guide and leak guide text
                                    // into the class label (observed live).
        // Head+tail sample (not head-only): conclusions live at the end.
        bool ok = pv::llm_generate("Pick exactly one class from the allowed list. Reply with only the class name.",
                                   "Allowed: " + j.classes + "\nSummary:\n" +
                                       sample_head_tail(basis, 400, 400),
                                   lc2, llm_cls, e2);
        if (!ok && e2 == "aborted") return -3;
        if (ok && !llm_cls.empty()) cls = clean_label(llm_cls);
    }
    // Topics come from LLM prose, which often leads with markdown headers
    // ("**Executive Summary**: …", "## Summary", "- …"). Strip those plus
    // generic lead words so filenames stay meaningful ("Day - - …" never).
    // Head+tail sample so cold opens don't dominate the title.
    // Video: topic from the compressed-whole "## Summary" section (last),
    // never the timestamped/sectioned head (headers + [t=MM:SS] tags leak
    // into filenames otherwise — observed live).
    std::string topic_src = basis;
    if (j.is_video) {
        size_t ls = basis.rfind("## Summary");
        if (ls != std::string::npos) {
            std::string tail = basis.substr(ls + 11);
            if (tail.size() > 40) topic_src = tail;
        }
    }
    std::string topic =
        clean_label(strip_topic_lead(sample_head_tail(topic_src, 150, 150)));
    if (j.is_video) topic = clean_label(strip_bracketed(topic));
    std::string title = pv::sanitize_filename(
        cls + " - " + day + (topic.empty() || topic == "Untitled" ? "" : " - " + topic));
    title = cap_title(title, 100);
    if (!pv::checkpoint()) return -3;

    emit(idx, PV_DIFF, 0.9f, "Computing diff...");
    std::string diff = summary.empty() ? std::string("[]") : pv::diff_to_json(raw, summary);

    pv::pv_make_dirs(j.out.empty() ? "output" : j.out);
    std::string dir = j.out.empty() ? "output" : j.out;
    // Uniquify: never silently overwrite a previous output with the same title.
    // (Counter named dup_n: a `dup()` helper lives in this file — see above.)
    std::string md = dir + "/" + title + ".md";
    for (int dup_n = 2; pv::pv_file_exists(md); ++dup_n) {
        std::string t2 = cls + " - " + day;
        if (topic != "Untitled") t2 += " - " + topic;
        title = cap_title(pv::sanitize_filename(t2 + " - " + std::to_string(dup_n)), 100);
        md = dir + "/" + title + ".md";
    }
    // Coverage accounting: portion stats for the sidecar, the in-summary
    // Coverage line, and the GUI status — degraded portions are never
    // silent. Computed here (not in summarize) so the achieved ratio uses
    // the final on-disk summary text.
    int achieved_pct = 0;
    {
        std::istringstream rsw(raw), ssw(summary);
        size_t rw = 0, sw = 0;
        std::string w;
        while (rsw >> w) ++rw;
        while (ssw >> w) ++sw;
        // Vision evidence inflates the summary side: count caption words on
        // the source side too, or long films report absurd ratios.
        if (!frame_captions.empty()) {
            std::istringstream csw(frame_captions);
            while (csw >> w) ++rw;
        }
        if (rw > 0) achieved_pct = (int)(sw * 100 / rw);
        if (!summary.empty()) {
            const int good = cov.chunks - cov.fallbacks < 0 ? 0 : cov.chunks - cov.fallbacks;
            char cb[384];
            snprintf(cb, sizeof(cb),
                     "\nCoverage: %d of %d source words (%d%%); %d/%d portions fully "
                     "summarized%s%s%s%s%s.\n",
                     (int)sw, (int)rw, achieved_pct, good, cov.chunks,
                     cov.fallbacks > 0 ? ", some portions used extractive fallback" : "",
                     cov.grammar_fallbacks > 0 ? ", some transcript kept verbatim (grammar guard)" : "",
                     cov.relaxed ? ", heavy repetition — length target relaxed" : "",
                     cov.verbatim ? ", output is verbatim notes (context overflow)" : "",
                     j.is_video ? ", video path" : "");
            summary += cb;
            if (j.is_video) {
                char vb[224];
                snprintf(vb, sizeof(vb),
                         "Video: %u frame(s) sampled, %u captioned (attention %d)%s%s.\n",
                         (unsigned)n_frames, (unsigned)n_captioned, j.attention,
                         n_sources > 0 ? ", web research cited" : ", no web research",
                         frames_dir.empty() ? ", transcript-only (no frames)" : "");
                summary += vb;
            }
        }
    }
    // Source retention (Settings → Recordings & sources): on "archive", copy
    // the queued source beside the .md as "<stem>.src<ext>" BEFORE writing
    // the body so the ## Audio line can name it. Videos are never archived
    // here (Section 5 exempts them at the call site via is_video).
    std::string archived_name;
    if (j.retention == "archive" && !j.is_video && !j.audio.empty()) {
        std::string ext;
        auto dot = j.audio.rfind('.');
        auto slash = j.audio.find_last_of("/\\");
        if (dot != std::string::npos && (slash == std::string::npos || dot > slash))
            ext = j.audio.substr(dot);
        for (char& c : ext) c = (char)tolower((unsigned char)c);
        if (ext.empty() || ext.size() > 6) ext = ".wav";
        std::string dest = dir + "/" + title + ".src" + ext;
        for (int dup_n = 2; pv::pv_file_exists(dest); ++dup_n)
            dest = dir + "/" + title + ".src" + ext + " (" + std::to_string(dup_n) + ")";
        // Never archive a file onto itself (source already beside output).
        if (dest != j.audio) {
            std::ifstream in(j.audio, std::ios::binary);
            std::ofstream out_f(dest, std::ios::binary | std::ios::trunc);
            if (in && out_f) {
                out_f << in.rdbuf();
                out_f.flush();
            }
            if (in && out_f) {
                auto nm = dest.rfind('/');
                archived_name = (nm == std::string::npos) ? dest : dest.substr(nm + 1);
                pv::pv_log("archived source [" + std::to_string(idx) + "] " + dest);
            } else {
                std::remove(dest.c_str());
                pv::pv_log("archive FAILED [" + std::to_string(idx) + "] " + j.audio);
            }
        }
    }
    {
        std::string body = "# " + title + "\n\n## Summary\n";
        if (summary.empty())
            body += "*(summary skipped — raw transcript only)*\n";
        else
            body += summary + "\n";
        body += "\n## Raw Transcript\n" + raw + "\n";
        if (!archived_name.empty())
            body += "\n## Audio\n" + archived_name + "\n";
        if (!write_file(md, body)) return -4;  // surfaces as Error, never a ghost
    }
    {  // sidecar for audit/repro (escaped: titles may contain quotes)
        std::string side = "{\"title\":\"" + pv::json_escape(title) + "\",\"day\":\"" +
                           pv::json_escape(day) + "\",\"class\":\"" + pv::json_escape(cls) +
                           "\",\"stt\":\"" + pv::json_escape(j.stt) + "\",\"llm\":\"" +
                           pv::json_escape(j.llm) + "\",\"audio\":\"" + pv::json_escape(shown) +
                           "\",\"coverage\":{\"chunks\":" + std::to_string(cov.chunks) +
                           ",\"fallbacks\":" + std::to_string(cov.fallbacks) +
                           ",\"achieved_pct\":" + std::to_string(achieved_pct) +
                           ",\"relaxed\":" + (cov.relaxed ? "true" : "false") + "}}\n";
        write_file(md + ".json", side);  // best-effort: logged on failure
    }
    {  // diff sidecar consumed by the GUI Diff tab
        write_file(md + ".diff.json", diff);  // best-effort: logged on failure
    }
    // SRT sidecar (video jobs only, full + skip-summary runs alike): the
    // timestamped transcript lines become numbered cues beside the .md.
    // Non-video and unstamped transcripts yield "" — never a stray file.
    if (j.is_video && !raw.empty()) {
        std::string srt = pv::srt_from_transcript(raw, media_secs);
        if (!srt.empty()) {
            write_file(dir + "/" + title + ".srt", srt);  // best-effort
            pv::pv_log("srt written " + title + ".srt");
        }
    }
    pv::db_remember(j.db, shown, day, cls);

    if (out) {
        out->out_md_path = dup(md);
        out->raw_text = dup(raw);
        out->summary_text = dup(summary);
        out->diff_json = dup(diff);
    }
    pv::pv_log("done [" + std::to_string(idx) + "] " + md);
    // Temp frames sweep (Section 5b): once captions are consumed into the
    // summary, the JPEGs + frames.md go. Kept when vision never ran (retry
    // reuses them) or failed (forensics + Clean caches backstop).
    if (j.is_video && captions_ok && !frames_dir.empty()) {
        pv::sweep_frames_dir(frames_dir);
        pv::pv_log("frames swept " + frames_dir);
    }
    // Source retention on success only (failures/aborts return earlier, so
    // their sources always survive). Guards: never the .md itself, never
    // anything inside the output dir, never an empty path — and NEVER video
    // sources (read in place from any path; temp frames are swept instead).
    if (j.retention == "delete" && !j.is_video && !j.audio.empty() && j.audio != md &&
        j.audio.rfind(dir + "/", 0) != 0 && j.audio.rfind(dir + "\\", 0) != 0) {
        if (std::remove(j.audio.c_str()) == 0)
            pv::pv_log("source deleted " + j.audio);
        else
            pv::pv_log("source delete FAILED " + j.audio);
    }
    if (j.delete_converted && pcm_src != j.audio) {
        if (std::remove(pcm_src.c_str()) == 0)
            pv::pv_log("cache cleaned " + pcm_src);
        else
            pv::pv_log("cache clean FAILED " + pcm_src);
    }
    emit(idx, PV_DONE, 1.0f, md);
    return 0;
}

void worker() {
    for (;;) {
        if (!pv::checkpoint()) {
            // Abort consumed between files. With a non-empty queue the drain
            // CONTINUES by design: abort_current means "stop the active
            // file", and remove/retry flows depend on the worker moving on
            // (the Rust epoch retires the aborted file's events). Full stops
            // pair abort with pv_queue_clear (abort_all), which empties the
            // queue and breaks below. Do not "fix" this into a break: retry
            // after drain would stall.
            std::lock_guard<std::mutex> l(g_mu);
            if (g_q.empty()) break;
            continue;
        }
        Job j;
        {
            std::lock_guard<std::mutex> l(g_mu);
            if (g_q.empty()) break;
            j = g_q.front();
            g_q.pop();
        }
        PvResult r{};
        // Global pop index (see g_file_idx): never per-run, so the shell's
        // append-only order history maps every event correctly across runs.
        const int idx = g_file_idx.fetch_add(1);
        // Vendor throws must never cross into the Rust caller (foreign
        // exceptions through extern "C" fast-fail the whole process with no
        // dump, no popup: see __rust_foreign_exception). Convert to rc -4.
        int rc = 0;
        try {
            rc = run_one(j, idx, &r);
        } catch (const std::exception& e) {
            pv::pv_log(std::string("run_one NATIVE EXCEPTION on ") + j.audio + ": " + e.what());
            rc = -4;
        } catch (...) {
            pv::pv_log(std::string("run_one NATIVE EXCEPTION (unknown) on ") + j.audio);
            rc = -4;
        }
        pv_free_result(&r);
        if (rc == -3)
            emit(idx, PV_ERROR, 0.0f, "aborted");
        else if (rc == -4)
            // run_one emits specific PV_ERROR for -1/-2/text failures already;
            // only unexpected/native-exception paths land here with detail.
            emit(idx, PV_ERROR, 0.0f, "file failed (native exception, see backend.log), continuing FIFO");
    }
    g_running = false;
}

Job from_opts(const PvJobOptions* o) {
    Job j;
    j.audio = o->audio_path ? o->audio_path : "";
    j.stt = o->stt_model ? o->stt_model : "";
    j.llm = o->llm_model ? o->llm_model : "";
    j.out = o->out_dir ? o->out_dir : "output";
    j.classes = o->known_classes ? o->known_classes : "";
    j.db = o->db_path ? o->db_path : "app.db";
    if (o->chunk_tokens > 0) j.chunk_tokens = o->chunk_tokens;
    if (o->audio_window_sec > 0) j.window_sec = o->audio_window_sec;
    if (o->vram_budget_pct > 0) j.budget = o->vram_budget_pct;
    j.skip = o->skip_summary != 0;
    j.cpu_only = o->cpu_only != 0;
    j.delete_converted = o->delete_converted != 0;
    j.is_text = o->is_text != 0;
    j.display = o->display_name ? o->display_name : "";
    j.summary_tier = o->summary_tier ? o->summary_tier : "";
    j.retention = o->audio_retention ? o->audio_retention : "";
    if (j.retention != "delete" && j.retention != "archive") j.retention = "keep";
    j.denoise = o->denoise_mode ? o->denoise_mode : "";
    if (j.denoise != "off" && j.denoise != "aggressive") j.denoise = "recommended";
    j.is_video = o->is_video != 0;
    j.web_research = o->web_research != 0;
    // 0 is a valid floor (Overview), not "unset": the Rust side clamps to
    // 0..100 and defaults 50, so every value here is deliberate.
    j.attention = o->attention;
    if (j.attention < 0) j.attention = 0;
    if (j.attention > 100) j.attention = 100;
    j.vlm_text = o->vlm_text_model ? o->vlm_text_model : "";
    j.vlm_mmproj = o->vlm_mmproj ? o->vlm_mmproj : "";
    j.transcript_path = o->transcript_path ? o->transcript_path : "";
    return j;
}
}  // namespace

extern "C" {
int pv_abi_version(void) { return PV_ABI_VERSION; }
int pv_queue_add(const PvJobOptions* o) {
    if (!o || !o->audio_path) return -1;
    std::lock_guard<std::mutex> l(g_mu);
    g_q.push(from_opts(o));
    return (int)g_q.size();
}
int pv_queue_pending(void) {
    std::lock_guard<std::mutex> l(g_mu);
    return (int)g_q.size() + (g_running ? 1 : 0);
}
void pv_queue_clear(void) {
    std::lock_guard<std::mutex> l(g_mu);
    std::queue<Job> e;
    g_q.swap(e);
    // Paired with the shell's full order clear + epoch bump (abort_all):
    // both sides restart indexing together, so post-abort events map cleanly.
    g_file_idx.store(0);
}
void pv_queue_pause(int on) { pv::ctl_pause(on); }
int pv_queue_paused(void) { return pv::ctl_paused(); }
void pv_abort_current(void) { pv::ctl_abort(); }
int pv_queue_remove(const char* audio_path) {
    if (!audio_path || !*audio_path) return 0;
    std::lock_guard<std::mutex> l(g_mu);
    std::queue<Job> keep;
    bool dropped = false;
    while (!g_q.empty()) {
        Job j = g_q.front();
        g_q.pop();
        if (!dropped && j.audio == audio_path) {
            dropped = true;  // drop it; the active job is never in g_q
            continue;
        }
        keep.push(std::move(j));
    }
    g_q.swap(keep);
    return dropped ? 1 : 0;
}
int pv_run_async(PvProgressCb cb) {
    std::lock_guard<std::mutex> l(g_mu);
    g_cb = cb;
    if (g_running) return 0;  // already draining
    if (g_worker.joinable()) g_worker.join();
    g_running = true;
    g_worker = std::thread(worker);
    return 0;
}
void pv_wait_idle(void) {
    for (;;) {
        {
            std::lock_guard<std::mutex> l(g_mu);
            if (g_q.empty() && !g_running) break;
        }
        std::this_thread::sleep_for(std::chrono::milliseconds(100));
    }
    if (g_worker.joinable() && std::this_thread::get_id() != g_worker.get_id())
        g_worker.join();
}
// Graceful worker shutdown for quit paths: signal abort, wait bounded for
// the drain (safe-point stops + model unloads), then JOIN the thread.
// The join is the load-bearing part: g_worker is a process-global
// std::thread, and destroying it joinable (the old quit path via bare
// exit()) terminates via SIGABRT on EVERY session with a prior run —
// the "FATAL abort" close-crash. Returns 0 joined, 1 on timeout (caller
// exits anyway), -1 if misused from the worker thread itself. Additive
// export: no ABI struct change.
int pv_abort_and_join(int timeout_ms) {
    if (std::this_thread::get_id() == g_worker.get_id()) return -1;
    pv::ctl_abort();
    if (timeout_ms < 0) timeout_ms = 0;
    const auto deadline =
        std::chrono::steady_clock::now() + std::chrono::milliseconds(timeout_ms);
    for (;;) {
        bool quiet = false;
        {
            std::lock_guard<std::mutex> l(g_mu);
            quiet = g_q.empty() && !g_running;
        }
        if (quiet) break;
        if (std::chrono::steady_clock::now() >= deadline) {
            pv::pv_log("abort_and_join: timeout, worker still draining");
            // Detach so a later exit can never destroy it joinable
            // (std::terminate). The OS reaps the stuck thread at exit.
            if (g_worker.joinable()) g_worker.detach();
            return 1;
        }
        std::this_thread::sleep_for(std::chrono::milliseconds(50));
    }
    if (g_worker.joinable()) g_worker.join();
    return 0;
}
int pv_process_file(const PvJobOptions* o, PvProgressCb cb, int idx, PvResult* out) {
    if (!o) return -1;
    g_cb = cb;
    // Same foreign-exception guard as the worker loop (see above).
    try {
        return run_one(from_opts(o), idx, out);
    } catch (const std::exception& e) {
        pv::pv_log(std::string("pv_process_file NATIVE EXCEPTION: ") + e.what());
        return -4;
    } catch (...) {
        pv::pv_log("pv_process_file NATIVE EXCEPTION (unknown)");
        return -4;
    }
}
void pv_free_result(PvResult* r) {
    if (!r) return;
    free(r->out_md_path);
    free(r->raw_text);
    free(r->summary_text);
    free(r->diff_json);
    *r = PvResult{};
}
void pv_free(void* p) { free(p); }
int pv_resolve_day_class(const char* audio, const char* db, char* day, int day_cap,
                         char* hint, int hint_cap) {
    if (!day || day_cap <= 0 || !hint || hint_cap <= 0) return -1;
    std::string d, h;
    // Sync helper: display and real path coincide (real audio files).
    const std::string a = audio ? audio : "";
    pv::resolve_day_class(a, a, db ? db : "", d, h);
    strncpy_s(day, day_cap, d.c_str(), _TRUNCATE);
    strncpy_s(hint, hint_cap, h.c_str(), _TRUNCATE);
    return 0;
}
int pv_detect_tier(float* vram_gb, float* ram_gb, long long* disk_free_bytes,
                   const char* path_for_disk) {
    pv::HwInfo hw;
    pv::query_hw(hw, path_for_disk ? path_for_disk : "");
    if (vram_gb) *vram_gb = hw.vram_gb;
    if (ram_gb) *ram_gb = hw.ram_gb;
    if (disk_free_bytes) *disk_free_bytes = hw.disk_known ? (long long)hw.disk_free_bytes : -1;
    return pv::pick_tier(hw);
}
}  // extern "C"
