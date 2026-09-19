// Standalone harness for audio_audacity.cpp (no pipeline/DLL needed).
// Build: cl /EHsc /std:c++17 /I core\include /I core\thirdparty\sqlite
//        /DPV_HAVE_SQLITE core\src\audio_audacity.cpp
//        core\thirdparty\sqlite\sqlite3.c tests\fixtures\test_audacity.cpp
// Run from the repo root; fixtures via tests\fixtures\make_aup3.py.
#include <cmath>
#include <cstdio>
#include <string>
#include <vector>

#include "../../core/src/internal.h"

namespace pv {
void hook_begin(int) {}
void progress_hook(int, float, const std::string&) {}
bool checkpoint() { return true; }
}  // namespace pv

static int failures = 0;
#define CHECK(cond, msg)                                        \
    do {                                                        \
        if (!(cond)) {                                          \
            printf("FAIL: %s\n", msg);                          \
            ++failures;                                         \
        } else {                                                \
            printf("ok: %s\n", msg);                            \
        }                                                     \
    } while (0)

// Minimal WAV reader for the float32 mono renders.
static bool read_wav_f32(const std::string& path, std::vector<float>& pcm, uint32_t& rate) {
    FILE* f = nullptr;
    if (fopen_s(&f, path.c_str(), "rb") != 0 || !f) return false;
    unsigned char h[44];
    bool ok = fread(h, 1, 44, f) == 44 && memcmp(h, "RIFF", 4) == 0;
    uint32_t n = 0;
    if (ok) {
        memcpy(&n, h + 40, 4);
        memcpy(&rate, h + 24, 4);
        n /= 4;
        pcm.resize(n);
        ok = fread(pcm.data(), 4, n, f) == n;
    }
    fclose(f);
    return ok;
}

static double rms_of(const std::vector<float>& pcm) {
    double acc = 0;
    for (float v : pcm) acc += (double)v * v;
    return sqrt(acc / (pcm.empty() ? 1 : pcm.size()));
}

int main() {
    const std::string fix = "tests/fixtures/";
    std::string err;

    CHECK(pv::is_audacity_project(fix + "basic.aup3"), "detect basic.aup3");
    CHECK(pv::is_audacity_project(fix + "missing.aup3"), "detect missing.aup3");
    CHECK(!pv::is_audacity_project(fix + "corrupt.aup3"), "reject random bytes");
    CHECK(!pv::is_audacity_project(fix + "empty.aup3") == false, "empty shell detected by magic");
    CHECK(!pv::is_audacity_project("README.md"), "reject non-project");

    // basic: 2 s lecture @gain 0.5 (0.5-amplitude sine -> mix RMS ~0.177);
    // the muted 0.9-DC track must NOT leak in (else RMS >> 0.3).
    err.clear();
    CHECK(pv::render_audacity_to_wav(fix + "basic.aup3", fix + "basic.wav", err),
          ("render basic (" + err + ")").c_str());
    std::vector<float> pcm;
    uint32_t rate = 0;
    CHECK(read_wav_f32(fix + "basic.wav", pcm, rate), "read rendered wav");
    CHECK(pcm.size() == 16000, "rendered length 16000 samples");
    CHECK(rate == 8000, "rendered rate 8000");
    double r = rms_of(pcm);
    printf("info: basic RMS=%f\n", r);
    CHECK(r > 0.12 && r < 0.24, "gain applied + muted track excluded");

    // missing block -> silence, still renders.
    err.clear();
    CHECK(pv::render_audacity_to_wav(fix + "missing.aup3", fix + "missing.wav", err),
          ("render missing-block (" + err + ")").c_str());
    pcm.clear();
    CHECK(read_wav_f32(fix + "missing.wav", pcm, rate) && pcm.size() == 8000,
          "missing-block length intact");
    CHECK(rms_of(pcm) == 0.0, "missing block is silence");

    // corrupt + empty shells fail with actionable errors.
    err.clear();
    CHECK(!pv::render_audacity_to_wav(fix + "corrupt.aup3", fix + "x.wav", err) && !err.empty(),
          "corrupt rejected");
    err.clear();
    CHECK(!pv::render_audacity_to_wav(fix + "empty.aup3", fix + "y.wav", err) && !err.empty(),
          "empty shell rejected");

    // qarg quoting (same function the ffmpeg spawn sites use).
    {
        std::string q;
        CHECK(pv::qarg("C:\\plain\\file.wav", q) && q == "\"C:\\plain\\file.wav\"", "qarg plain");
        q.clear();
        CHECK(pv::qarg("C:\\we\"ird\\a.wav", q) &&
                  q == "\"C:\\we\\\"ird\\a.wav\"",
              "qarg embedded quote");
        q.clear();
        CHECK(pv::qarg("C:\\trail\\", q) && q == "\"C:\\trail\\\\\"",
              "qarg trailing slash");
        q.clear();
        // Backslashes immediately before a quote double up (MS round-trip rule).
        CHECK(pv::qarg("a\\\"b", q) && q == "\"a\\\\\\\"b\"",
              "qarg slash-before-quote");
        q.clear();
        CHECK(!pv::qarg("bad\nname.wav", q), "qarg rejects control chars");
    }

    // SRT builder (same TU the pipeline links).
    {
        CHECK(pv::srt_from_transcript("no stamps here", 60.0).empty(), "srt empty without stamps");
        std::string raw = "[00:01] hello\ncontinued line\n[00:05] world\n[1:02:03] far\n";
        std::string srt = pv::srt_from_transcript(raw, 3723.0 + 10.0);
        CHECK(srt.find("1\n00:00:01,000 --> 00:00:05,000\nhello continued line\n\n") !=
                  std::string::npos,
              "srt cue 1 + continuation join");
        CHECK(srt.find("3\n01:02:03,000 --> 01:02:13,000\nfar\n\n") != std::string::npos,
              "srt hour stamp + duration end");
        CHECK(pv::fmt_srt_ts(3723000) == "01:02:03,000", "srt timestamp format");
    }

    if (failures == 0) printf("ALL HARNESS TESTS PASSED\n");
    return failures ? 1 : 0;
}
