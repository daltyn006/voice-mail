// Whisper STT: whisper.cpp linked statically, Vulkan GPU (NVIDIA+AMD).
// Processes 30s windows sequentially so VRAM never grows with file length.
// Model freed after each file -> never co-resident with the LLM.
#include "internal.h"

#include <algorithm>
#include <cstdio>
#include <cstdlib>
#include <fstream>
#include <thread>
#include <vector>

#ifdef PV_HAVE_WHISPER
#include "whisper.h"
#endif

namespace pv {

#ifdef PV_HAVE_WHISPER
namespace {
// [MM:SS] / [H:MM:SS] stamp from whisper 10 ms units. Pure.
std::string stamp_mmss(int64_t t0) {
    long secs = (long)(t0 / 100);
    if (secs < 0) secs = 0;
    char b[32];
    if (secs >= 3600)
        snprintf(b, sizeof(b), "[%ld:%02ld:%02ld]", secs / 3600, (secs % 3600) / 60,
                 secs % 60);
    else
        snprintf(b, sizeof(b), "[%02ld:%02ld]", secs / 60, secs % 60);
    return b;
}

// Sentence accumulator for timestamped (video) transcripts: segments glue
// into sentences; each flushed sentence carries the t0 of its first segment
// so frame↔transcript confirmation is an interval check. Untimed (audio)
// behavior below is byte-identical to the old path.
struct SentAcc {
    std::string pending;
    int64_t t0 = -1;
    std::string label;  // "Speaker N: " when a turn opened mid-sentence
};

// One 30s window through an already-loaded ctx. Shared by the in-RAM and
// spilled (on-disk PCM) paths so windowing behavior can never drift.
// `base_t0` is the window's file offset in whisper 10 ms units (segment t0
// values are window-relative — without it every window restamps from zero).
bool run_window(whisper_context* ctx, const std::string& lang, bool tdrz,
                const float* samples, int n, std::string& out, int& speaker,
                bool& need_label, std::string& err, bool timestamps, SentAcc& acc,
                int64_t base_t0, const char* seed_prompt = nullptr) {
    whisper_full_params wparams = whisper_full_default_params(WHISPER_SAMPLING_GREEDY);
    wparams.language = lang.c_str();
    // Part-split continuity: the previous part's tail seeds ONLY the first
    // window (carry off — later windows condition on live context, and
    // re-seeding every window would dilute it).
    wparams.initial_prompt = seed_prompt;
    wparams.carry_initial_prompt = false;
    // Never 0 (upstream default is min(4, hw_concurrency); 0 throws
    // length_error inside this snapshot's threadpool sizing — and
    // hardware_concurrency itself may return 0).
    wparams.n_threads = std::max(1, std::min(4, (int)std::thread::hardware_concurrency()));
    wparams.print_progress = false;
    wparams.print_special = false;
    wparams.print_realtime = false;
    wparams.print_timestamps = false;
    // Keep the previous window as context so sentence boundaries survive chunking.
    wparams.no_context = false;
    wparams.single_segment = false;
    wparams.tdrz_enable = tdrz;

    if (whisper_full(ctx, wparams, samples, n) != 0) {
        // Auto-detect fails on non-speech (tones, silence, music):
        // retry the window in English instead of failing the whole file.
        whisper_full_params w2 = wparams;
        w2.language = "en";
        if (whisper_full(ctx, w2, samples, n) != 0) {
            err = "whisper_full failed";
            return false;
        }
    }
    const int nseg = whisper_full_n_segments(ctx);
    for (int s = 0; s < nseg; ++s) {
        const char* t = whisper_full_get_segment_text(ctx, s);
        if (t && *t) {
            if (!timestamps) {
                if (tdrz && need_label) {
                    if (!out.empty()) out += '\n';
                    out += "Speaker " + std::to_string(speaker) + ": ";
                    need_label = false;
                } else if (!out.empty() && out.back() != ' ' && out.back() != '\n') {
                    out += ' ';
                }
                out += t;
                if (tdrz && whisper_full_get_segment_speaker_turn_next(ctx, s)) {
                    speaker = 3 - speaker;  // 1 <-> 2
                    need_label = true;
                }
                continue;
            }
            // Timestamped path (video): sentence-grouped, stamped lines.
            // Segment times are window-relative: shift into file time.
            const int64_t t0 = whisper_full_get_segment_t0(ctx, s) + base_t0;
            if (tdrz && need_label) {
                if (!acc.pending.empty() && !acc.label.empty()) {
                    // Turn opened mid-sentence: flush what we have first.
                    if (!out.empty()) out += '\n';
                    out += stamp_mmss(acc.t0 >= 0 ? acc.t0 : t0) + " " + acc.label +
                           acc.pending;
                    acc.pending.clear();
                }
                acc.label = "Speaker " + std::to_string(speaker) + ": ";
                need_label = false;
            }
            if (acc.t0 < 0) acc.t0 = t0;
            if (!acc.pending.empty() && acc.pending.back() != ' ') acc.pending += ' ';
            acc.pending += t;
            // Flush complete sentences; the tail rides to the next segment.
            size_t at = 0;
            for (size_t i = 0; i < acc.pending.size(); ++i) {
                const char c = acc.pending[i];
                if (c == '.' || c == '?' || c == '!') {
                    std::string sent = acc.pending.substr(at, i + 1 - at);
                    // Trim leading space for clean lines.
                    size_t b = sent.find_first_not_of(' ');
                    if (b != std::string::npos) sent = sent.substr(b);
                    if (!sent.empty()) {
                        if (!out.empty()) out += '\n';
                        out += stamp_mmss(acc.t0) + " ";
                        if (!acc.label.empty()) {
                            out += acc.label;
                            acc.label.clear();
                        }
                        out += sent;
                    }
                    at = i + 1;
                    acc.t0 = t0;  // next sentence starts (approx) here
                }
            }
            if (at > 0) acc.pending = acc.pending.substr(at);
            if (tdrz && whisper_full_get_segment_speaker_turn_next(ctx, s)) {
                speaker = 3 - speaker;  // 1 <-> 2
                need_label = true;
            }
        }
    }
    return true;
}

// Flush a window/file tail: unstamped-tail sentences keep the last t0.
void flush_sent_tail(std::string& out, SentAcc& acc) {
    std::string tail = acc.pending;
    size_t b = tail.find_first_not_of(' ');
    if (b != std::string::npos) tail = tail.substr(b);
    if (!tail.empty()) {
        if (!out.empty()) out += '\n';
        out += stamp_mmss(acc.t0 >= 0 ? acc.t0 : 0) + " ";
        if (!acc.label.empty()) out += acc.label;
        out += tail;
    }
    acc.pending.clear();
    acc.label.clear();
    acc.t0 = -1;
}

whisper_context* stt_open(const SttConfig& cfg, std::string& err) {
    // NOTE: written against whisper.cpp's long-stable C API
    // (init_from_file_with_params / whisper_full / segment getters).
    // Greedy sampling is deterministic (temperature-0 equivalent); only
    // long-lived param fields are touched so this survives API drift.
    whisper_context_params cparams = whisper_context_default_params();
    // Explicit flag wins; PV_CPU_ONLY env remains as a debug fallback.
    // (Rust set_var does not reliably reach the CRT's getenv cache, so the
    // production path must never depend on env alone.)
    const bool cpu_only = cfg.cpu_only || std::getenv("PV_CPU_ONLY") != nullptr;
    cparams.use_gpu = !cpu_only;
    whisper_context* ctx = whisper_init_from_file_with_params(cfg.model_path.c_str(), cparams);
    if (!ctx) {
        // GPU init can fail on odd drivers; fall back to CPU once.
        cparams.use_gpu = false;
        ctx = whisper_init_from_file_with_params(cfg.model_path.c_str(), cparams);
    }
    if (!ctx) err = "whisper init failed: " + cfg.model_path;
    return ctx;
}
}  // namespace

bool stt_transcribe(const Audio& audio, const SttConfig& cfg, std::string& out_text,
                    std::string& err) {
    try {
        whisper_context* ctx = stt_open(cfg, err);
        if (!ctx) return false;
        const int win = (cfg.window_sec > 0 ? cfg.window_sec : 30) * 16000;
        const std::string lang = cfg.language.empty() ? "auto" : cfg.language;
        // Tinydiarize speaker turns: enabled purely by model filename (no new
        // plumbing). Only -tdrz weight files carry the turn-detection head.
        const bool tdrz = cfg.model_path.find("tdrz") != std::string::npos;
        int speaker = 1;        // alternates 1 <-> 2 on turn boundaries
        bool need_label = tdrz;  // first labeled segment opens with its speaker
        SentAcc acc;
        std::string out;
        const size_t total = audio.pcm.size();
        bool ok = true;

        for (size_t off = 0; off < total; off += (size_t)win) {
            size_t n = total - off;
            if (n > (size_t)win) n = (size_t)win;

            pv::progress_hook(PV_TRANSCRIBE, (float)off / (float)total, "Transcribing...");
            if (!pv::checkpoint()) {
                err = "aborted";  // safe stop: model freed, no partial output
                ok = false;
                break;
            }
            const char* seed = (off == 0 && !cfg.initial_prompt.empty())
                                   ? cfg.initial_prompt.c_str()
                                   : nullptr;
            if (!run_window(ctx, lang, tdrz, audio.pcm.data() + off, (int)n, out, speaker,
                            need_label, err, cfg.timestamps, acc,
                            (int64_t)(off / 160), seed)) {
                ok = false;
                break;
            }
        }
        if (ok && cfg.timestamps) flush_sent_tail(out, acc);
        whisper_free(ctx);  // VRAM/RAM back before the LLM loads
        if (!ok) return false;
        out_text = out;
        if (out_text.empty()) {
            err = "whisper produced no text";
            return false;
        }
        return true;
    } catch (const std::exception& e) {
        err = std::string("stt internal error: ") + e.what();
        return false;
    } catch (...) {
        err = "stt internal error (unknown)";
        return false;
    }
}

// Spilled variant: identical windowing, but PCM streams window-by-window
// from a raw float32LE sidecar instead of one giant RAM array. Used past
// SPILL_FLOATS (see pipeline.cpp) so 2-3h movies peak at one window plus
// the model instead of ~2GB. Model lifecycle matches stt_transcribe.
bool stt_transcribe_spilled(const std::string& pcm_path, size_t total_floats,
                            const SttConfig& cfg, std::string& out_text, std::string& err) {
    try {
        std::ifstream f(pcm_path, std::ios::binary);
        if (!f) {
            err = "cannot read spilled PCM";
            return false;
        }
        whisper_context* ctx = stt_open(cfg, err);
        if (!ctx) return false;
        const int win = (cfg.window_sec > 0 ? cfg.window_sec : 30) * 16000;
        const std::string lang = cfg.language.empty() ? "auto" : cfg.language;
        const bool tdrz = cfg.model_path.find("tdrz") != std::string::npos;
        int speaker = 1;
        bool need_label = tdrz;
        SentAcc acc;
        std::string out;
        std::vector<float> buf((size_t)win);
        bool ok = true;

        for (size_t off = 0; off < total_floats; off += (size_t)win) {
            size_t n = total_floats - off;
            if (n > (size_t)win) n = (size_t)win;

            pv::progress_hook(PV_TRANSCRIBE, (float)off / (float)total_floats,
                              "Transcribing (spilled)...");
            if (!pv::checkpoint()) {
                err = "aborted";
                ok = false;
                break;
            }
            f.clear();
            f.seekg((std::streamoff)(off * sizeof(float)), std::ios::beg);
            f.read(reinterpret_cast<char*>(buf.data()), (std::streamsize)(n * sizeof(float)));
            if ((size_t)f.gcount() != n * sizeof(float)) {
                err = "spilled PCM truncated";
                ok = false;
                break;
            }
            if (!run_window(ctx, lang, tdrz, buf.data(), (int)n, out, speaker, need_label,
                            err, cfg.timestamps, acc, (int64_t)(off / 160),
                            (off == 0 && !cfg.initial_prompt.empty())
                                ? cfg.initial_prompt.c_str()
                                : nullptr)) {
                ok = false;
                break;
            }
        }
        whisper_free(ctx);
        if (!ok) return false;
        if (cfg.timestamps) flush_sent_tail(out, acc);
        out_text = out;
        if (out_text.empty()) {
            err = "whisper produced no text";
            return false;
        }
        return true;
    } catch (const std::exception& e) {
        err = std::string("stt internal error: ") + e.what();
        return false;
    } catch (...) {
        err = "stt internal error (unknown)";
        return false;
    }
}
#else
bool stt_transcribe(const Audio& audio, const SttConfig& cfg, std::string& out_text,
                    std::string& err) {
    (void)audio; (void)cfg;
    err = "";
    // Mock keeps GUI/packaging testable without 2GB model downloads.
    out_text =
        "[mock transcript — fetch models + submodules for real STT] "
        "Lalibela Ethiopia carved eleven churches downward into volcanic rock. "
        "Bet Giyorgis Church of St George chiseled forty feet deep shaped like a Greek cross. "
        "Mount Kailash unclimbed out of respect. Pilgrims walk the thirty two mile Kora circuit.";
    return true;
}

bool stt_transcribe_spilled(const std::string& pcm_path, size_t total_floats,
                            const SttConfig& cfg, std::string& out_text,
                            std::string& err) {
    (void)pcm_path;
    (void)total_floats;
    (void)cfg;
    err.clear();
    out_text = "[mock transcript — spilled path]";
    return true;
}
#endif

}  // namespace pv
