// Pre-run model validation: prove each backend loads AND discharges before
// the FIFO run starts, so a broken model fails at a labeled "Loading…"
// step instead of mid-run with partial outputs. STT runs one real
// whisper_full on synthetic audio (content ignored — silence-safe);
// LLM runs a tiny real generate. Never co-resident, mirrors run_one order.
// Mock builds (no PV_HAVE_*) trivially pass: mocks always "work".
#include "../include/present_core.h"
#include "internal.h"

#include <algorithm>
#include <cmath>
#include <cstdio>
#include <cstring>
#include <exception>
#include <string>
#include <thread>
#include <vector>

#ifdef PV_HAVE_WHISPER
#include "whisper.h"
#endif
#ifdef PV_HAVE_LLAMA
#include "llama.h"
#endif

namespace {

void set_v_err(char* dst, int cap, const std::string& s) {
    if (!dst || cap <= 0) return;
    snprintf(dst, (size_t)cap, "%s", s.c_str());
}

}  // namespace

extern "C" {
// NOTE: every vendor call below sits inside try/catch. A C++ exception
// (e.g. ggml/whisper throwing on init or first dispatch) must NEVER cross
// the extern "C" boundary: Rust cannot unwind foreign exceptions and
// fast-fails the whole process (silent, no dump, no popup). Caught throws
// become ordinary error returns with the message preserved.
// `flags`: PV_VALIDATE_CPU_ONLY skips GPU paths. Explicit parameter — the
// backend must never depend on env state (Rust set_var is invisible to the
// CRT getenv cache, which silently ran GPU code on "CPU" runs).
int pv_validate_stt(const char* stt_model, char* err, int err_cap, int flags) {
    if (!stt_model || !*stt_model) {
        set_v_err(err, err_cap, "no STT model configured");
        return 1;
    }
    pv::pv_log(std::string("validate stt begin: ") + stt_model);
#ifdef PV_HAVE_WHISPER
    try {
    const bool cpu_only = (flags & PV_VALIDATE_CPU_ONLY) != 0
        || std::getenv("PV_CPU_ONLY") != nullptr;
    pv::pv_log(std::string("validate stt mode: ") + (cpu_only ? "CPU" : "GPU"));
    int rc = 1;
    for (int attempt = 0; attempt < 2; ++attempt) {
        // Mirror stt_whisper.cpp: GPU first, CPU fallback on clean failure.
        // Markers bracket the init call itself: a death between "enter" and
        // "loaded" is inside whisper/ggml/driver init; after "loaded" it is
        // in first inference (shader compile / dispatch).
        whisper_context_params cparams = whisper_context_default_params();
        cparams.use_gpu = !cpu_only && (attempt == 0);
        pv::pv_log(std::string("validate stt init enter (gpu=") +
                      (cparams.use_gpu ? "1" : "0") + ")");
        whisper_context* ctx =
            whisper_init_from_file_with_params(stt_model, cparams);
        pv::pv_log(std::string("validate stt init exit: ") +
                      (ctx ? "loaded" : "null"));
        if (!ctx) continue;
        // 1s 440Hz sine: exercises load + dispatch without needing audio.
        std::vector<float> pcm(16000);
        for (size_t i = 0; i < pcm.size(); ++i)
            pcm[i] = 0.5f * sinf(2.0f * 3.14159265f * 440.0f * (float)i / 16000.0f);
        whisper_full_params wparams = whisper_full_default_params(WHISPER_SAMPLING_GREEDY);
        wparams.language = "en";
        // Never 0: this snapshot throws length_error on a zero thread pool;
        // mirror upstream's own default (min(4, hardware_concurrency), and
        // hardware_concurrency itself may return 0).
        wparams.n_threads = std::max(1, std::min(4, (int)std::thread::hardware_concurrency()));
        wparams.print_progress = false;
        wparams.print_special = false;
        wparams.print_realtime = false;
        wparams.print_timestamps = false;
        rc = (whisper_full(ctx, wparams, pcm.data(), (int)pcm.size()) == 0) ? 0 : 1;
        pv::pv_log(std::string("validate stt inference exit: ") + (rc == 0 ? "ok" : "failed"));
        whisper_free(ctx);
        if (rc == 0) break;
    }
    if (rc != 0) set_v_err(err, err_cap, "STT validation failed (load or test inference)");
    pv::pv_log(std::string("validate stt ") + (rc == 0 ? "done" : "FAILED"));
    return rc;
    } catch (const std::exception& e) {
        set_v_err(err, err_cap, std::string("STT native exception: ") + e.what());
        pv::pv_log(std::string("validate stt NATIVE EXCEPTION: ") + e.what());
        return 1;
    } catch (...) {
        set_v_err(err, err_cap, "STT native exception (unknown)");
        pv::pv_log("validate stt NATIVE EXCEPTION (unknown)");
        return 1;
    }
#else
    (void)err;
    (void)err_cap;
    pv::pv_log("validate stt done (mock)");
    return 0;
#endif
}

int pv_validate_llm(const char* llm_model, char* err, int err_cap, int flags) {
    if (!llm_model || !*llm_model) {
        set_v_err(err, err_cap, "no LLM model configured");
        return 1;
    }
    pv::pv_log(std::string("validate llm begin: ") + llm_model);
#ifdef PV_HAVE_LLAMA
    try {
    const bool cpu_only = (flags & PV_VALIDATE_CPU_ONLY) != 0
        || std::getenv("PV_CPU_ONLY") != nullptr;
    pv::pv_log(std::string("validate llm mode: ") + (cpu_only ? "CPU" : "GPU"));
    pv::LlmConfig lc;
    lc.model_path = llm_model;
    lc.n_ctx = 2048;    // fits the ~1600-token guide + tiny prompt
    lc.n_batch = 2048;  // must exceed prompt tokens (else GGML_ASSERT aborts)
    lc.n_predict = 8;  // a few tokens prove the dispatch path
    lc.cpu_only = cpu_only;
    std::string out, e;
    // llm_generate itself retries Vulkan->CPU on clean failure.
    bool ok = pv::llm_generate("Reply with exactly: ok", "ok", lc, out, e);
    if (!ok) set_v_err(err, err_cap, "LLM validation failed: " + (e.empty() ? "no output" : e));
    pv::pv_log(std::string("validate llm ") + (ok ? "done" : "FAILED"));
    return ok ? 0 : 1;
    } catch (const std::exception& e2) {
        set_v_err(err, err_cap, std::string("LLM native exception: ") + e2.what());
        pv::pv_log(std::string("validate llm NATIVE EXCEPTION: ") + e2.what());
        return 1;
    } catch (...) {
        set_v_err(err, err_cap, "LLM native exception (unknown)");
        pv::pv_log("validate llm NATIVE EXCEPTION (unknown)");
        return 1;
    }
#else
    (void)err;
    (void)err_cap;
    pv::pv_log("validate llm done (mock)");
    return 0;
#endif
}

int pv_validate_vlm(const char* text_model, const char* mmproj, int flags) {
    if (!text_model || !*text_model || !mmproj || !*mmproj) {
        pv::pv_log("validate vlm: no vision pair configured");
        return 1;
    }
    pv::pv_log(std::string("validate vlm begin: ") + text_model);
#ifdef PV_HAVE_MTMD
    try {
    const bool cpu_only = (flags & PV_VALIDATE_CPU_ONLY) != 0
        || std::getenv("PV_CPU_ONLY") != nullptr;
    pv::pv_log(std::string("validate vlm mode: ") + (cpu_only ? "CPU" : "GPU"));
    std::string e;
    // Open proves both halves (text weights + projector init with vision
    // support check); unload immediately — captioning loads per film.
    bool ok = pv::vlm_validate(text_model, mmproj, cpu_only, e);
    if (!ok) pv::pv_log(std::string("validate vlm FAILED: ") + e);
    else pv::pv_log("validate vlm done");
    return ok ? 0 : 1;
    } catch (const std::exception& e2) {
        pv::pv_log(std::string("validate vlm NATIVE EXCEPTION: ") + e2.what());
        return 1;
    } catch (...) {
        pv::pv_log("validate vlm NATIVE EXCEPTION (unknown)");
        return 1;
    }
#else
    pv::pv_log("validate vlm done (mock)");
    return 0;
#endif
}
}  // extern "C"
