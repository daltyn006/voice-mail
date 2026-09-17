// LLM via llama.cpp (static, Vulkan). Map-reduce summarization:
// 1) split transcript into ~chunk_words pieces
// 2) per-chunk extract (forces stats/dates/numbers retention)
// 3) final reduce pass over the chunk notes.
//
// API seam: written against llama.cpp's sampler-era C API
// (llama_model_load_from_file / llama_init_from_model / vocab-first
// token helpers / llama_sampler_chain_init / llama_batch_get_one /
// llama_decode). If a future checkout renames one of these, only the
// marked lines below change.
#include "internal.h"

#include <algorithm>
#include <cctype>
#include <cstdlib>

#ifdef PV_HAVE_LLAMA
#include "llama.h"
#endif
#include <set>
#include <sstream>
#include <vector>

namespace pv {
namespace {
// Greedy decoding sometimes echoes the system prompt's behavior supplement
// into content output (observed live: "AGENTS.md - Present Voice …" inside
// summaries and filenames). Strip known guide fingerprints line-wise.
std::string strip_guide_echo(const std::string& t) {
    static const char* marks[] = {"AGENTS.md", "behavior supplement", "MAP (excerpt)",
                                  "REDUCE (summary)", "[Program behavior]"};
    std::string out;
    size_t i = 0;
    while (i < t.size()) {
        size_t j = t.find('\n', i);
        if (j == std::string::npos) j = t.size();
        std::string line = t.substr(i, j - i);
        std::string low = line;
        for (char& c : low) c = (char)tolower((unsigned char)c);
        bool drop = false;
        for (const char* m : marks) {
            std::string ml = m;
            for (char& c : ml) c = (char)tolower((unsigned char)c);
            if (low.find(ml) != std::string::npos) {
                drop = true;
                break;
            }
        }
        if (!drop) {
            out += line;
            out += '\n';
        }
        i = j + 1;
    }
    while (!out.empty() && (out.back() == '\n')) out.pop_back();
    return out.empty() ? t : out;
}
// Split into portions of ~target AI tokens (chars/4 estimate, consistent
// with est_tokens). Word-boundary cuts only, never mid-word.
std::vector<std::string> split_by_tokens(const std::string& t, int target) {
    if (target <= 0) target = 1000;
    std::istringstream in(t);
    std::vector<std::string> chunks;
    std::string w, cur;
    size_t cur_est = 0;
    const auto flush = [&] {
        if (!cur.empty()) {
            chunks.push_back(cur);
            cur.clear();
            cur_est = 0;
        }
    };
    while (in >> w) {
        const size_t w_est = w.size() / 4 + 1;
        if (!cur.empty() && cur_est + 1 + w_est > (size_t)target) flush();
        if (!cur.empty()) {
            cur += ' ';
            cur_est += 1;
        }
        cur += w;
        cur_est += w_est;
    }
    flush();
    if (chunks.empty()) chunks.emplace_back("");
    return chunks;
}

size_t count_words(const std::string& t) {
    std::istringstream in(t);
    size_t n = 0;
    std::string w;
    while (in >> w) ++n;
    return n;
}

// Sentence-duplication ratio in [0,1]: 1 - unique/total over normalized
// sentences (>=20 chars, at least 10 sentences to count). Heavy repetition
// (lectures with repeated refrains, lists read twice) relaxes the length
// target instead of padding.
double sentence_dup_ratio(const std::string& t) {
    std::vector<std::string> sents;
    std::string cur;
    for (char c : t) {
        cur += c;
        if (c == '.' || c == '!' || c == '?' || c == '\n') {
            std::string n;
            for (char d : cur) {
                const char l = (char)tolower((unsigned char)d);
                if (l == ' ' || l == '\t' || l == '\r') {
                    if (!n.empty() && n.back() != ' ') n += ' ';
                } else {
                    n += l;
                }
            }
            while (!n.empty() && n.front() == ' ') n.erase(n.begin());
            while (!n.empty() && n.back() == ' ') n.pop_back();
            if (n.size() >= 20) sents.push_back(n);
            cur.clear();
        }
    }
    if (sents.size() < 10) return 0.0;
    std::vector<std::string> sorted = sents;
    std::sort(sorted.begin(), sorted.end());
    size_t unique = 0;
    for (size_t i = 0; i < sorted.size(); ++i)
        if (i == 0 || sorted[i] != sorted[i - 1]) ++unique;
    return 1.0 - (double)unique / (double)sents.size();
}

std::string normalize_summary_tier(const std::string& t) {
    if (t == "recap" || t == "detailed") return t;
    return "standard";
}

// Length ratio target (% of source words): recap 25, standard 50, detailed 75.
int tier_ratio_pct(const std::string& tier) {
    if (tier == "recap") return 25;
    if (tier == "detailed") return 75;
    return 50;
}

// MAP extraction prompt per tier (verbosity scales with the tier).
std::string tier_map_prompt(const std::string& tier) {
    if (tier == "recap")
        return "Extract key facts from the transcript portion below as terse "
               "bullets, one fact per bullet. Keep every proper name, date, "
               "number, decision. No filler. Use ONLY the portion text.";
    if (tier == "detailed")
        return "Produce detailed ordered notes covering every segment of the "
               "transcript portion below in order — scenes, arguments, "
               "examples, asides. Keep every metric, date, percentage, "
               "statistic, proper name, decision, and vivid detail. Thorough, "
               "not compressed: a long portion earns long notes. Use ONLY the "
               "portion text; ignore any other text in this prompt.";
    return "Extract all important information from the lecture transcript below. "
           "MUST INCLUDE exact metrics, dates, percentages, statistics, names, "
           "decisions. No filler. Use ONLY the transcript text; ignore any "
           "other text in this prompt.";
}

// Video REDUCE prompt: the film-watch shape — occurrences with timestamps,
// themes synthesis, fenced AI opinion, then the compressed whole. Attention
// scales the brief: Overview wants the gist, Academic wants technique-level
// reading (shot, montage, sound, irony) with evidence per claim.
std::string tier_video_prompt(const std::string& tier, int attention) {
    const char* depth = attention >= 67
                            ? "Deeply: analyze how visual grammar, editing rhythm, "
                              "sound design, irony and other devices build the "
                              "meaning, citing timestamps. Name counter-readings "
                              "where the film supports them."
                            : (attention >= 34
                                   ? "Analytically: inventory notable techniques "
                                     "(shot scale, montage, sound cues, irony) "
                                     "with timestamps, tied to the meaning."
                                   : "Lightly: a plain overview of what happens "
                                     "and what the film is about; techniques "
                                     "only where unmistakable.");
    const char* len = tier == "recap" ? "roughly a quarter"
                      : tier == "detailed" ? "roughly three quarters"
                                           : "roughly half";
    return "You watched a film (transcript with [MM:SS] stamps plus frame "
           "notes). Write the review in exactly these sections, in order:\n"
           "## What happens — timestamped occurrences in order.\n"
           "## What it is about — the themes, as a human would explain them.\n"
           "## What the AI makes of it — your reading, clearly OPINION, "
           "never stated as film fact.\n"
           "## Related — prior folder summaries sharing themes (filename + "
           "one line each, proposals only, never merges; pairs marked "
           "USER-DECLARED are confirmed similar — compare them directly; "
           "write 'none' when no sibling summaries were provided).\n"
           "## Summary — the compressed whole (" +
           std::string(len) +
           " the length of the notes).\n" + depth +
           " Use ONLY the notes (+ cited web context when present); every "
           "visual claim needs a timestamp, every web claim its source. "
           "Speculation is labeled speculation. Do not repeat note "
           "delimiters in your answer.";
}
}  // namespace

// Persistent session: model+ctx load once per file, shared across all
// MAP chunks + REDUCE passes. Previously every llm_generate() call
// reloaded multi-GB weights (100s of cycles on long docs). Model stays
// resident for the file, then closes — still never co-resident with STT.
struct LlmSession {
#ifdef PV_HAVE_LLAMA
    llama_model* model = nullptr;
    llama_context* ctx = nullptr;
    const llama_vocab* vocab = nullptr;
#endif
    LlmConfig cfg;
    int n_batch = 0;  // effective batch (from cfg at open)
    int n_ctx = 0;    // effective context (from cfg at open)
    bool gpu = false;
    bool broken = false;  // vendor threw mid-session: fail fast, no leak
};

#ifdef PV_HAVE_LLAMA
namespace {
// (full prompt assembly without chat-template API: templates churn; plain
// system+prompt works across Gemma/Llama/Qwen instruct GGUFs.)
std::string session_full(const LlmConfig& cfg, const std::string& system,
                         const std::string& prompt) {
    // The runtime behavior supplement rides along only when the caller
    // wants it. "" selects the legacy AGENTS.md; otherwise the length
    // tier's guide (config/guides/<tier>.md).
    std::string guide;
    if (cfg.include_guide)
        guide = "\n\n[Program behavior]\n" + agent_guide(cfg.guide_name) + "\n\n";
    return system + guide + prompt + "\n\nAnswer:";
}

// Clamp generation headroom so prompt + predicted tokens fit BOTH n_batch
// and n_ctx. A just-fitting prompt plus a full tail trips
// GGML_ASSERT(n_tokens_all <= n_batch) mid-generation and kills the process
// (no catch can save an assert). Returns clamped n_predict, or -1 when even
// a stub generation cannot fit (caller degrades to extractive fallback).
int fit_predict(int n, int n_batch, int n_ctx, int want, std::string& err) {
    const int room_batch = n_batch - n;
    const int room_ctx = n_ctx - n;
    const int room = room_batch < room_ctx ? room_batch : room_ctx;
    if (room < 8) {
        err = "no generation headroom (" + std::to_string(n) + " prompt tokens)";
        return -1;
    }
    return want < room ? want : room;
}

bool session_load(LlmSession* s, bool gpu_try, std::string& err) {
    llama_model_params mp = llama_model_default_params();
    // Explicit flag wins; PV_CPU_ONLY env remains as a debug fallback
    // (see stt_whisper.cpp: the CRT getenv cache is unreliable).
    const bool cpu_only =
        s->cfg.cpu_only || std::getenv("PV_CPU_ONLY") != nullptr || !gpu_try;
    mp.n_gpu_layers = cpu_only ? 0 : 99;
    // SEAM: loader name. Old checkouts: llama_load_model_from_file.
    s->model = llama_model_load_from_file(s->cfg.model_path.c_str(), mp);
    if (!s->model) {
        err = "llama load failed: " + s->cfg.model_path;
        return false;
    }
    llama_context_params cp = llama_context_default_params();
    s->n_ctx = s->cfg.n_ctx > 0 ? s->cfg.n_ctx : 8192;
    s->n_batch = s->cfg.n_batch > 0 ? s->cfg.n_batch : 512;
    cp.n_ctx = (uint32_t)s->n_ctx;
    cp.n_batch = (uint32_t)s->n_batch;
    // SEAM: context ctor (llama_init_from_model supersedes the
    // deprecated llama_new_context_with_model).
    s->ctx = llama_init_from_model(s->model, cp);
    if (!s->ctx) {
        err = "llama context failed";
        llama_model_free(s->model);
        s->model = nullptr;
        return false;
    }
    // SEAM: token helpers take the vocab (not the model) in newest checkouts.
    s->vocab = llama_model_get_vocab(s->model);
    s->gpu = !cpu_only;
    return true;
}

void session_unload(LlmSession* s) {
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
}  // namespace
#endif

LlmSession* llm_session_open(const LlmConfig& cfg, std::string& err) {
#ifdef PV_HAVE_LLAMA
    LlmSession* s = nullptr;
    try {
        s = new LlmSession();
        s->cfg = cfg;
        // Attempt 0 = Vulkan, 1 = CPU retry (mirrors the old per-call loop,
        // now once per file instead of once per chunk).
        if (session_load(s, true, err)) return s;
        if (session_load(s, false, err)) return s;
        delete s;
        return nullptr;
    } catch (const std::exception& e) {
        err = std::string("llm session failed: ") + e.what();
        if (s) {
            session_unload(s);
            delete s;
        }
        return nullptr;
    } catch (...) {
        err = "llm session failed (unknown)";
        if (s) {
            session_unload(s);
            delete s;
        }
        return nullptr;
    }
#else
    (void)cfg;
    (void)err;
    return new LlmSession();
#endif
}

bool llm_session_generate(LlmSession* s, const std::string& system,
                          const std::string& prompt, std::string& out,
                          std::string& err, int n_predict_want) {
    if (!s) {
        err = "no llm session";
        return false;
    }
#ifdef PV_HAVE_LLAMA
    if (s->broken || !s->ctx || !s->model || !s->vocab) {
        err = "llm session broken";
        return false;
    }
    try {
        const std::string full = session_full(s->cfg, system, prompt);
        std::vector<llama_token> toks(full.size() + 16);
        int n = llama_tokenize(s->vocab, full.c_str(), (int)full.size(), toks.data(),
                               (int)toks.size(), true, true);
        if (n < 0) {  // buffer too small; retry sized
            toks.resize((size_t)-n);
            n = llama_tokenize(s->vocab, full.c_str(), (int)full.size(), toks.data(),
                               (int)toks.size(), true, true);
        }
        bool ok = n > 0;
        // Hard guarantee: oversized prompts degrade to the extractive
        // fallback instead of tripping GGML_ASSERT (process abort).
        if (ok && n > s->n_batch) {
            err = "prompt exceeds batch (" + std::to_string(n) + " > " +
                  std::to_string(s->n_batch) + ")";
            return false;
        }
        int want = n_predict_want > 0 ? n_predict_want
                                      : (s->cfg.n_predict > 0 ? s->cfg.n_predict : 1024);
        int n_predict = want;
        if (ok) {
            n_predict = fit_predict(n, s->n_batch, s->n_ctx, want, err);
            if (n_predict < 0) return false;
        }
        std::string gen;
        if (ok) {
            // SEAM: chain ctor renamed (was llama_sampler_chain_new).
            llama_sampler* smp =
                llama_sampler_chain_init(llama_sampler_chain_default_params());
            llama_sampler_chain_add(smp, llama_sampler_init_greedy());  // temp-0
            llama_batch b = llama_batch_get_one(toks.data(), n);
            if (llama_decode(s->ctx, b) != 0) {
                ok = false;
            } else {
                for (int i = 0; i < n_predict; ++i) {
                    if ((i & 63) == 0 && !pv::checkpoint()) {
                        ok = false;
                        err = "aborted";  // safe stop: session stays open
                        break;
                    }
                    llama_token t = llama_sampler_sample(smp, s->ctx, -1);
                    if (llama_vocab_is_eog(s->vocab, t)) break;
                    char piece[256]{};
                    int pl =
                        llama_token_to_piece(s->vocab, t, piece, sizeof(piece), 0, true);
                    if (pl > 0) gen.append(piece, (size_t)pl);
                    llama_batch nb = llama_batch_get_one(&t, 1);
                    if (llama_decode(s->ctx, nb) != 0) break;
                }
            }
            llama_sampler_free(smp);
        }
        if (ok && !gen.empty()) {
            out = gen;
            return true;
        }
        if (err == "aborted") return false;  // never retry an abort on CPU
        if (!ok) {
            // Decode-loop failure on GPU: reopen CPU-only once and retry the
            // same prompt (preserves the old per-call Vulkan->CPU behavior
            // across the persistent session).
            if (s->gpu) {
                session_unload(s);
                if (session_load(s, false, err)) {
                    s->gpu = false;
                    return llm_session_generate(s, system, prompt, out, err,
                                                n_predict_want);
                }
                s->broken = true;
                return false;
            }
            err = "llama generation failed";
        } else {
            err = "llama generation failed";
        }
        return false;
    } catch (const std::exception& e) {
        // Vendor threw mid-generation: tear down internals (no leak) and
        // mark broken so later chunks fail fast into extractive fallbacks.
        session_unload(s);
        s->broken = true;
        err = std::string("llm internal error: ") + e.what();
        return false;
    } catch (...) {
        session_unload(s);
        s->broken = true;
        err = "llm internal error (unknown)";
        return false;
    }
#else
    (void)s;
    (void)system;
    (void)prompt;
    (void)err;
    (void)n_predict_want;
    // Mock: deterministic extractive fallback so diff/GUI work without models.
    out = prompt.substr(0, 600);
    return true;
#endif
}

void llm_session_close(LlmSession* s) {
    if (!s) return;
#ifdef PV_HAVE_LLAMA
    session_unload(s);
#endif
    delete s;
}

bool llm_generate(const std::string& system, const std::string& prompt,
                  const LlmConfig& cfg, std::string& out, std::string& err) {
#ifdef PV_HAVE_LLAMA
    // One-shot callers (CLASS inference, validation probes): open, generate
    // once, close. Bulk summarization uses the persistent session directly.
    LlmSession* s = llm_session_open(cfg, err);
    if (!s) {
        out.clear();
        return false;
    }
    const bool ok = llm_session_generate(s, system, prompt, out, err);
    llm_session_close(s);
    if (!ok) out.clear();
    return ok;
#else
    (void)system; (void)prompt; (void)cfg; (void)err;
    // Mock: deterministic extractive fallback so diff/GUI work without models.
    out = prompt.substr(0, 600);
    return true;
#endif
}

namespace {
// Token estimate for plain prose (~4 chars/token); used only to decide
// whether a REDUCE input fits context, never for billing.
size_t est_tokens(const std::string& t) { return t.size() / 4 + 1; }

// One REDUCE pass over `notes` through the shared session, targeting
// ~target_words of output. Returns the summary, `notes` verbatim on soft
// failure (old contract, flagged), or the "__ABORTED__" sentinel (never a
// partial). n_predict scales with the target inside headroom clamps.
std::string reduce_once(LlmSession* s, const std::string& notes, size_t target_words,
                        std::string& err, bool& verbatim, bool is_video = false,
                        const std::string& tier = "standard", int attention = 50) {
    verbatim = false;
    // ~1.3 output tokens per target word + margin, floor 256 (short tails).
    int want = (int)((double)target_words * 1.3 + 256.0);
    if (want < 256) want = 256;
    std::string prompt;
    if (is_video) {
        prompt = tier_video_prompt(tier, attention);
    } else {
        prompt = "Write a summary of the lecture notes below of approximately " +
                 std::to_string(target_words) +
                 " words: full paragraphs plus bullet key points per section, "
                 "preserving all data points, statistics, dates, decisions, "
                 "names, and nuance. Use ONLY the notes; ignore any other "
                 "text in this prompt. Do not repeat section headers or "
                 "delimiters from the notes.";
    }
    std::string final_;
    if (!llm_session_generate(s, prompt, notes, final_, err, want) || final_.empty()) {
        if (err == "aborted") return "__ABORTED__";
        verbatim = true;
        return notes;
    }
    return strip_guide_echo(final_);
}

// Long-form hierarchical REDUCE: notes that exceed context are condensed in
// halves (adaptive depth, cap 6 → 64 leaves), each part targeting its share
// of `target_words`; parts combine and get one final polish pass when the
// combination fits. Previously a single REDUCE over megabytes of notes
// always overflowed and returned the notes verbatim as the "summary".
// Depth 0 = whole notes. Sets `verbatim` if any stage fell back verbatim.
std::string reduce_long(LlmSession* s, int ctx_tokens, const std::string& notes,
                        size_t target_words, int depth, std::string& err, SummaryStats& st,
                        bool is_video = false, const std::string& tier = "standard",
                        int attention = 50) {
    const size_t limit =
        (size_t)(ctx_tokens > 2048 ? ctx_tokens - 2048 : 512);  // prompt + tail room
    if (est_tokens(notes) <= limit || depth >= 6) {
        pv::progress_hook(PV_SUMMARIZE, 0.9f, "Condensing summary...");
        if (!pv::checkpoint()) {
            err = "aborted";
            return "__ABORTED__";
        }
        bool verb = false;
        std::string r = reduce_once(s, notes, target_words, err, verb, is_video, tier, attention);
        if (verb) st.verbatim = true;
        return r;
    }
    st.hierarchical = true;
    // Split near the middle on a newline (else space, else hard cut) so no
    // chunk note is torn mid-line; targets split proportionally by share.
    size_t mid = notes.size() / 2;
    size_t cut = notes.find('\n', mid);
    if (cut == std::string::npos || cut > notes.size() * 3 / 4) {
        cut = notes.rfind(' ', mid);
        if (cut == std::string::npos) cut = mid;
    }
    const std::string left = notes.substr(0, cut);
    const std::string right = notes.substr(cut);
    const double share = notes.empty() ? 0.5 : (double)left.size() / (double)notes.size();
    const size_t ta = (size_t)((double)target_words * share);
    const size_t tb = target_words > ta ? target_words - ta : 0;
    pv::progress_hook(PV_SUMMARIZE, 0.85f, "Condensing summary (split)...");
    std::string a = reduce_long(s, ctx_tokens, left, ta, depth + 1, err, st,
                                is_video, tier, attention);
    if (a == "__ABORTED__") return a;
    std::string b = reduce_long(s, ctx_tokens, right, tb, depth + 1, err, st,
                                is_video, tier, attention);
    if (b == "__ABORTED__") return b;
    std::string combined = a + "\n\n## Part\n\n" + b;
    if (est_tokens(combined) <= limit) {
        // Final polish pass for flow when the combination fits.
        bool verb = false;
        std::string r = reduce_once(s, combined, target_words, err, verb,
                                    is_video, tier, attention);
        if (r == "__ABORTED__") return r;
        if (verb) st.verbatim = true;
        return r;
    }
    return combined;
}

// Extractive fallback notes when the model cannot load at all (same shape
// the old per-chunk fallbacks produced).
std::string fallback_notes(const std::vector<std::string>& chunks, SummaryStats& st) {
    st.fallbacks = (int)chunks.size();
    std::string notes;
    for (size_t i = 0; i < chunks.size(); ++i)
        notes += "\n\n[chunk " + std::to_string(i + 1) + "] " + chunks[i].substr(0, 400);
    return notes;
}

// Count alphanumeric words (punctuation-proof: "hello," and "hello" match).
size_t alpha_words(const std::string& t) {
    size_t n = 0;
    bool in = false;
    for (char c : t) {
        const bool w = (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') ||
                       (c >= '0' && c <= '9');
        if (w && !in) {
            ++n;
            in = true;
        } else if (!w) {
            in = false;
        }
    }
    return n;
}

// Grammar stage: punctuate, capitalize, and split the verbatim STT into one
// sentence per line, keeping every `Speaker N:` label on its sentences.
// Video transcripts carry leading [MM:SS] stamps: kept byte-exact when
// keep_stamps (the word guard counts stamp digits equally on both sides).
// The WHOLE transcript is preserved — the word guard below rejects any chunk
// whose word count drifts (paraphrase/hallucination) and keeps it verbatim.
// Runs on the shared session before MAP; MAP consumes the readable copy.
std::string grammar_polish(LlmSession* s, const std::string& raw, SummaryStats& st,
                           bool keep_stamps = false) {
    std::string sys =
        "Fix punctuation, capitalization, and sentence boundaries only. Split "
        "into one sentence per line. Keep every existing `Speaker N:` label at "
        "the start of its sentences. Never add, drop, reorder, or reword any "
        "words; never invent speaker names; never summarize. Output the FULL "
        "chunk and nothing else.";
    if (keep_stamps)
        sys += " Keep any leading [MM:SS] or [H:MM:SS] timestamp exactly as-is "
               "at the start of its line.";
    auto chunks = split_by_tokens(raw, 1000);
    std::string out;
    for (size_t i = 0; i < chunks.size(); ++i) {
        pv::progress_hook(PV_SUMMARIZE, 0.3f + 0.1f * (float)i / (float)chunks.size(),
                          "Polishing transcript...");
        if (!pv::checkpoint()) return "__ABORTED__";
        std::string piece, err;
        bool keep_verbatim = false;
        if (!llm_session_generate(s, sys,
                                  "Transcript chunk " + std::to_string(i + 1) + "/" +
                                      std::to_string(chunks.size()) + ":\n" + chunks[i],
                                  piece, err) ||
            piece.empty()) {
            if (err == "aborted") return "__ABORTED__";
            keep_verbatim = true;
        } else {
            piece = strip_guide_echo(piece);
            // Word guard: punctuation-only edits keep the count (near-)equal.
            const size_t a = alpha_words(chunks[i]);
            const size_t b = alpha_words(piece);
            const size_t tol = a / 20 > 8 ? a / 20 : 8;  // 5%, floor 8
            if (b == 0 || (a > b ? a - b : b - a) > tol) keep_verbatim = true;
        }
        if (keep_verbatim) {
            ++st.grammar_fallbacks;
            piece = chunks[i];
        }
        if (!out.empty()) out += "\n";
        out += piece;
    }
    if (st.grammar_fallbacks > 0) {
        pv::pv_log("grammar fallbacks: " + std::to_string(st.grammar_fallbacks) + "/" +
                   std::to_string(chunks.size()) + " chunks kept verbatim");
    }
    return out;
}
}  // namespace

SummaryResult summarize_map_reduce(const std::string& transcript, const LlmConfig& cfg,
                                   int chunk_tokens, bool is_video, int attention,
                                   const std::string& research_notes,
                                   const std::string& folder_context,
                                   const std::string& frame_captions,
                                   bool is_doc) {
    SummaryResult res;
    res.stats.src_words = count_words(transcript);
    const std::string tier = normalize_summary_tier(cfg.summary_tier);
    int ratio = tier_ratio_pct(tier);
    // Persistent session: ONE model load per file shared by the grammar
    // pass, every MAP chunk, and the REDUCE passes (previously hundreds of
    // load/unload cycles on long docs). Tier guides ride along by design
    // (short, purpose-written); the old per-chunk `n_batch = 128` shrink is
    // gone on purpose: batch is fixed at session open and the per-call
    // prompt guard degrades oversized inputs instead; 128 only produced
    // silent extractive garbage.
    LlmConfig sc = cfg;
    sc.include_guide = true;
    sc.guide_name = tier;
    sc.summary_tier = tier;
    std::string serr;
    LlmSession* sess = llm_session_open(sc, serr);
    if (!sess) {
        pv::pv_log(std::string("llm session failed, extractive fallback: ") + serr);
        auto fb_chunks =
            split_by_tokens(transcript, chunk_tokens > 0 ? chunk_tokens : 1000);
        res.stats.chunks = (int)fb_chunks.size();
        res.polished = transcript;  // no session: readable == verbatim
        res.text = fallback_notes(fb_chunks, res.stats);
        // Vision evidence survives the fallback: captions are already-paid
        // observations, not generations — dropping them would waste the VLM.
        if (!frame_captions.empty())
            res.text += "\n\n## Frame captions (vision — timestamped, visible-only)\n" +
                        frame_captions;
        res.stats.verbatim = true;
        return res;
    }
    struct Closer {
        LlmSession* s;
        ~Closer() { llm_session_close(s); }
    } closer{sess};
    // Grammar stage first: MAP consumes the readable copy. Skip-summary jobs
    // never reach here (verbatim by design), so this is summary-path only.
    // Video transcripts keep their [MM:SS] stamps through the polish.
    res.polished = grammar_polish(sess, transcript, res.stats, is_video);
    if (res.polished == "__ABORTED__") {
        res.polished.clear();
        res.text = "__ABORTED__";
        return res;
    }
    res.stats.src_words = count_words(res.polished);
    auto chunks =
        split_by_tokens(res.polished, chunk_tokens > 0 ? chunk_tokens : 1000);
    res.stats.chunks = (int)chunks.size();
    // Documents are prose, not dialogue: forbid invented speaker labels
    // (observed live: "Speaker 1:" prefixed onto document sentences).
    std::string map_sys = tier_map_prompt(tier);
    if (is_doc)
        map_sys += " The portion is a written document, not a transcript: never "
                   "add speaker labels or dialogue formatting.";
    const int ctx_tokens = sess->cfg.n_ctx > 0 ? sess->cfg.n_ctx : 8192;
    std::string notes;
    for (size_t i = 0; i < chunks.size(); ++i) {
        pv::progress_hook(PV_SUMMARIZE,
                          0.4f + 0.5f * (float)i / (float)chunks.size(),
                          "Summarizing...");
        if (!pv::checkpoint()) {
            res.text = "__ABORTED__";
            return res;
        }
        std::string piece;
        std::string err;
        if (!llm_session_generate(sess, map_sys,
                                  "Transcript block " + std::to_string(i + 1) + "/" +
                                      std::to_string(chunks.size()) + ":\n" + chunks[i],
                                  piece, err) ||
            piece.empty()) {
            if (err == "aborted") {
                res.text = "__ABORTED__";
                return res;
            }
            piece = chunks[i].substr(0, 400);
            ++res.stats.fallbacks;
        }
        piece = strip_guide_echo(piece);
        notes += "\n\n[chunk " + std::to_string(i + 1) + "] " + piece;
    }
    if (res.stats.fallbacks > 0) {
        pv::pv_log("map fallbacks: " + std::to_string(res.stats.fallbacks) + "/" +
                   std::to_string(res.stats.chunks) + " chunks extractive");
    }
    // Length target: tier ratio of source words; heavy sentence-level
    // repetition relaxes it instead of padding (25/50/75% of repeated
    // refrains would just echo). Floor keeps tiny inputs sensible.
    // Video scales with attention (Overview 0.5x … Academic 1.5x).
    size_t target = res.stats.src_words * (size_t)ratio / 100;
    if (is_video) {
        if (attention < 0) attention = 0;
        if (attention > 100) attention = 100;
        target = target * (size_t)(50 + attention) / 100;
    }
    if (sentence_dup_ratio(notes) > 0.5) {
        res.stats.relaxed = true;
        target = target * 2 / 5;
        pv::pv_log("heavy repetition detected: length target relaxed");
    }
    if (target < 50) target = 50;
    // External research + folder context ride into REDUCE fenced off from
    // film-grounded notes (the video prompt renders them cited/separate).
    // Frame captions land first: vision is the primary witness after audio.
    if (!frame_captions.empty())
        notes += "\n\n## Frame captions (vision — timestamped, visible-only)\n" +
                 frame_captions;
    if (!research_notes.empty())
        notes += "\n\n## External research (web — cite per claim, never film fact)\n" +
                 research_notes;
    if (!folder_context.empty())
        notes += "\n\n## Sibling summaries (same folder — compare themes only)\n" +
                 folder_context;
    std::string err;
    std::string final_ =
        reduce_long(sess, ctx_tokens, notes, target, 0, err, res.stats,
                    is_video, tier, attention);
    if (final_ == "__ABORTED__") {
        res.text = final_;
        return res;
    }
    // Degenerate-output guard: greedy decoding can lock into a repeat
    // attractor on thin inputs (observed live: one transcript line echoed
    // 30x as the whole "summary"). Two detectors: sentence-level dup ratio
    // (needs terminators) and word-uniqueness for terminator-free loops.
    // Falls back to the notes themselves — real content, flagged verbatim.
    bool degenerate = sentence_dup_ratio(final_) > 0.75;
    if (!degenerate) {
        std::istringstream win(final_);
        std::string w;
        size_t total = 0;
        std::set<std::string> uniq;
        while (win >> w) {
            std::string n;
            for (char c : w) {
                const char l = (char)tolower((unsigned char)c);
                if ((l >= 'a' && l <= 'z') || (l >= '0' && l <= '9')) n += l;
            }
            if (!n.empty()) {
                ++total;
                uniq.insert(n);
            }
        }
        degenerate = total > 40 && (double)uniq.size() / (double)total < 0.20;
    }
    if (degenerate) {
        pv::pv_log("degenerate summary detected (repetition), verbatim notes kept");
        res.stats.verbatim = true;
        res.text = strip_guide_echo(notes);
        return res;
    }
    res.text = strip_guide_echo(final_);
    return res;
}

}  // namespace pv
