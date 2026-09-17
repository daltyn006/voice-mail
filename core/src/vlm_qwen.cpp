// Vision captions via llama.cpp mtmd (Qwen2-VL family: 2B/7B, any quant).
// One VLM session per film: text GGUF + mmproj projector, mirroring the
// LlmSession lifecycle (GPU-try then CPU retry at open, greedy temp-0
// decode, RAII close). The VLM is NEVER co-resident with STT or the
// summarizer: pipeline opens it after STT unloads and closes it before the
// LLM loads, with the same VRAM-after-unload logging.
#include "internal.h"

#include <algorithm>
#include <cctype>
#include <cstdlib>

#ifdef PV_HAVE_MTMD
#include "llama.h"
#include "mtmd-helper.h"
#include "mtmd.h"
#endif

namespace pv {

struct VlmSession {
#ifdef PV_HAVE_MTMD
    llama_model* model = nullptr;
    llama_context* ctx = nullptr;
    const llama_vocab* vocab = nullptr;
    mtmd_context* mctx = nullptr;
#endif
    VlmConfig cfg;
    int n_ctx = 8192;
    int n_batch = 4096;
    bool gpu = false;
    bool broken = false;  // vendor threw mid-session: fail fast, no leak
};

#ifdef PV_HAVE_MTMD
namespace {

bool vlm_load(VlmSession* s, bool gpu_try, std::string& err) {
    llama_model_params mp = llama_model_default_params();
    const bool cpu_only =
        s->cfg.cpu_only || std::getenv("PV_CPU_ONLY") != nullptr || !gpu_try;
    mp.n_gpu_layers = cpu_only ? 0 : 99;
    s->model = llama_model_load_from_file(s->cfg.text_path.c_str(), mp);
    if (!s->model) {
        err = "vlm text load failed: " + s->cfg.text_path;
        return false;
    }
    llama_context_params cp = llama_context_default_params();
    s->n_ctx = s->cfg.n_ctx > 0 ? s->cfg.n_ctx : 8192;
    s->n_batch = s->cfg.n_batch > 0 ? s->cfg.n_batch : 4096;
    cp.n_ctx = (uint32_t)s->n_ctx;
    cp.n_batch = (uint32_t)s->n_batch;
    s->ctx = llama_init_from_model(s->model, cp);
    if (!s->ctx) {
        err = "vlm context failed";
        llama_model_free(s->model);
        s->model = nullptr;
        return false;
    }
    s->vocab = llama_model_get_vocab(s->model);
    mtmd_context_params mparams = mtmd_context_params_default();
    mparams.use_gpu = !cpu_only;
    mparams.print_timings = false;
    mparams.n_threads = 4;
    s->mctx = mtmd_init_from_file(s->cfg.mmproj_path.c_str(), s->model, mparams);
    if (!s->mctx) {
        err = "vlm projector failed: " + s->cfg.mmproj_path;
        llama_free(s->ctx);
        s->ctx = nullptr;
        llama_model_free(s->model);
        s->model = nullptr;
        s->vocab = nullptr;
        return false;
    }
    if (!mtmd_support_vision(s->mctx)) {
        err = "projector has no vision support: " + s->cfg.mmproj_path;
        mtmd_free(s->mctx);
        s->mctx = nullptr;
        llama_free(s->ctx);
        s->ctx = nullptr;
        llama_model_free(s->model);
        s->model = nullptr;
        s->vocab = nullptr;
        return false;
    }
    s->gpu = !cpu_only;
    return true;
}

void vlm_unload(VlmSession* s) {
    if (s->mctx) {
        mtmd_free(s->mctx);
        s->mctx = nullptr;
    }
    if (s->ctx) {
        llama_free(s->ctx);
        s->ctx = nullptr;
    }
    if (s->model) {
        llama_model_free(s->model);
        s->model = nullptr;
    }
    s->vocab = nullptr;
}

const char* caption_instruction(int detail) {
    if (detail >= 2)
        return "Analyze this film frame in detail: subjects, composition, "
               "lighting, setting, any on-screen text, and mood. Only what is "
               "visible — no speculation, no story guessing.";
    if (detail == 1)
        return "Describe this film frame in at most 3 sentences: subjects, "
               "setting, and any on-screen text. Only what is visible.";
    return "Describe this film frame in one short sentence: only what is visible.";
}

}  // namespace
#endif

VlmSession* vlm_session_open(const VlmConfig& cfg, std::string& err) {
#ifdef PV_HAVE_MTMD
    VlmSession* s = nullptr;
    try {
        s = new VlmSession();
        s->cfg = cfg;
        // Attempt 0 = Vulkan, 1 = CPU retry (same discipline as the LLM).
        if (vlm_load(s, true, err)) return s;
        if (vlm_load(s, false, err)) return s;
        delete s;
    } catch (const std::exception& e) {
        err = std::string("vlm internal error: ") + e.what();
        delete s;
    } catch (...) {
        err = "vlm internal error (unknown)";
        delete s;
    }
    return nullptr;
#else
    (void)cfg;
    (void)err;
    return nullptr;  // mock builds have no vision: caller degrades honestly
#endif
}

void vlm_session_close(VlmSession* s) {
    if (!s) return;
#ifdef PV_HAVE_MTMD
    vlm_unload(s);
#endif
    delete s;
}

bool vlm_validate(const std::string& text_path, const std::string& mmproj_path,
                  bool cpu_only, std::string& err) {
#ifdef PV_HAVE_MTMD
    VlmConfig cfg;
    cfg.text_path = text_path;
    cfg.mmproj_path = mmproj_path;
    cfg.cpu_only = cpu_only;
    VlmSession* s = vlm_session_open(cfg, err);
    if (!s) return false;
    vlm_session_close(s);
    return true;
#else
    (void)text_path;
    (void)mmproj_path;
    (void)cpu_only;
    err = "vision unavailable in this build (no mtmd)";
    return false;
#endif
}

bool vlm_caption(VlmSession* s, const std::string& image_path, int detail,
                 std::string& out, std::string& err) {
    if (!s) {
        err = "no vlm session";
        return false;
    }
#ifdef PV_HAVE_MTMD
    if (s->broken || !s->ctx || !s->model || !s->mctx) {
        err = "vlm session broken";
        return false;
    }
    // Fresh KV per caption: one session serves hundreds of frames.
    llama_memory_clear(llama_get_memory(s->ctx), true);
    mtmd_helper_bitmap_wrapper wrap{};
    try {
        wrap = mtmd_helper_bitmap_init_from_file(
            s->mctx, image_path.c_str(), false, mtmd_helper_init_opt_default());
    } catch (...) {
        err = "frame decode failed";
        return false;
    }
    if (!wrap.bitmap) {
        err = "frame decode failed";
        return false;
    }
    struct BitmapGuard {
        mtmd_bitmap* b;
        ~BitmapGuard() {
            if (b) mtmd_bitmap_free(b);
        }
    } guard{wrap.bitmap};

    const std::string prompt =
        std::string(mtmd_get_marker(s->mctx)) + "\n" + caption_instruction(detail);
    // Qwen2-VL-Instruct chat framing (matches its Jinja template exactly):
    // without the assistant header the model ends the turn immediately
    // (observed live: zero-token generations). parse_special binds both the
    // chat control tokens and the media marker; add_special stays off since
    // the template is already complete (no BOS).
    const std::string chat =
        std::string("<|im_start|>system\nYou are a helpful assistant.<|im_end|>\n"
                    "<|im_start|>user\n") +
        prompt +
        std::string("<|im_end|>\n<|im_start|>assistant\n");
    mtmd_input_text text{chat.c_str(), chat.size(), false, true};
    mtmd_input_chunks* raw_chunks = mtmd_input_chunks_init();
    if (!raw_chunks) {
        err = "vlm chunk alloc failed";
        return false;
    }
    struct ChunksGuard {
        mtmd_input_chunks* c;
        ~ChunksGuard() {
            if (c) mtmd_input_chunks_free(c);
        }
    } cguard{raw_chunks};
    const mtmd_bitmap* bitmaps[1] = {wrap.bitmap};
    if (mtmd_tokenize(s->mctx, raw_chunks, &text, bitmaps, 1) != 0) {
        err = "vlm tokenize failed";
        return false;
    }
    llama_pos n_past = 0;
    if (mtmd_helper_eval_chunks(s->mctx, s->ctx, raw_chunks, 0, 0, s->n_batch, true,
                                &n_past) != 0) {
        err = "vlm prompt eval failed";
        return false;
    }
    // Generation budget by detail inside headroom clamps (never an assert).
    // Detail caps stay tight: the first sentences carry the observation,
    // the rest is restatement (Small models especially).
    int want = detail >= 2 ? 384 : (detail == 1 ? 160 : 96);
    const int room = s->n_ctx - (int)n_past - 64;
    if (room < 8) {
        err = "vlm context full";
        return false;
    }
    if (want > room) want = room;
    bool ok = true;
    std::string gen;
    llama_sampler* smp =
        llama_sampler_chain_init(llama_sampler_chain_default_params());
    // Repetition penalty: Small VLMs loop sentences until the token cap
    // (observed live: the same sentence 6x). 1.15 breaks loops without
    // changing greedy determinism on first-token choice.
    llama_sampler_chain_add(
        smp, llama_sampler_init_penalties(llama_vocab_n_tokens(s->vocab), 64,
                                          1.15f, 0.0f, 0.0f));
    llama_sampler_chain_add(smp, llama_sampler_init_greedy());  // temp-0
    for (int i = 0; i < want; ++i) {
        if ((i & 15) == 0 && !pv::checkpoint()) {
            ok = false;
            err = "aborted";  // safe stop: session stays open for later frames
            break;
        }
        llama_token t = llama_sampler_sample(smp, s->ctx, -1);
        if (llama_vocab_is_eog(s->vocab, t)) break;
        char piece[256]{};
        int pl = llama_token_to_piece(s->vocab, t, piece, sizeof(piece), 0, true);
        if (pl > 0) gen.append(piece, (size_t)pl);
        llama_batch nb = llama_batch_get_one(&t, 1);
        if (llama_decode(s->ctx, nb) != 0) {
            ok = false;
            break;
        }
    }
    llama_sampler_free(smp);
    if (!ok) {
        if (err == "aborted") return false;
        // GPU decode failure: reopen CPU-only once and retry the frame.
        if (s->gpu) {
            vlm_unload(s);
            std::string e2;
            if (vlm_load(s, false, e2)) {
                s->gpu = false;
                return vlm_caption(s, image_path, detail, out, err);
            }
        }
        err = "vlm generation failed";
        return false;
    }
    if (gen.empty()) {
        err = "vlm empty caption";
        return false;
    }
    out = gen;
    // Trim to a tight caption (models ramble past the frame).
    while (!out.empty() && (out.back() == '\n' || out.back() == ' ')) out.pop_back();
    size_t head = out.find_first_not_of(" \n");
    if (head != std::string::npos && head > 0) out = out.substr(head);
    return true;
#else
    (void)image_path;
    (void)detail;
    err = "vision unavailable in this build (no mtmd)";
    return false;
#endif
}

}  // namespace pv
