// Decode any audio/video file to 16kHz mono float via the bundled ffmpeg build.
// Contract: ffmpeg.exe + its DLLs live beside present_core.dll / the app exe
// (scripts/setup-windows.ps1 copies both into ffmpeg-dlls/, build.ps1 stages them).
// Big inputs are transcoded ONCE to 16k mono WAV in the cache dir (see below)
// so retries/re-runs never re-decode multi-GB files.
#include "internal.h"
// NOTE: WIN32_LEAN_AND_MEAN comes from CMake compile definitions; do not
// #define it here (MSVC C4005).
#include <windows.h>

#include <algorithm>
#include <cmath>
#include <cstdio>

namespace pv {
namespace {
// (qarg lives in internal.h so the harness executes the same quoting.)

// FNV-1a 64: cache keys without any third-party hash dependency.
uint64_t fnv(const void* data, size_t len, uint64_t h = 1469598103934665603ull) {
    const unsigned char* p = (const unsigned char*)data;
    for (size_t i = 0; i < len; ++i) {
        h ^= p[i];
        h *= 1099511628211ull;
    }
    return h;
}

bool file_identity(const std::string& path, uint64_t& size, uint64_t& stamp) {
    WIN32_FILE_ATTRIBUTE_DATA fad{};
    if (!GetFileAttributesExA(path.c_str(), GetFileExInfoStandard, &fad)) return false;
    size = ((uint64_t)fad.nFileSizeHigh << 32) | fad.nFileSizeLow;
    stamp = ((uint64_t)fad.ftLastWriteTime.dwHighDateTime << 32) |
            fad.ftLastWriteTime.dwLowDateTime;
    return true;
}

std::string ffmpeg_exe() {
    char exe[MAX_PATH]{};
    GetModuleFileNameA(nullptr, exe, MAX_PATH);
    std::string dir = exe;
    auto sl = dir.find_last_of("\\/");
    if (sl != std::string::npos) dir.resize(sl);
    std::string ff = dir + "\\ffmpeg.exe";
    // NSIS bundle layout: bundled files land in $RESOURCE (= resources/ subdir).
    if (GetFileAttributesA(ff.c_str()) == INVALID_FILE_ATTRIBUTES)
        ff = dir + "\\resources\\ffmpeg.exe";
    return ff;
}

static uint32_t rd_le32(const unsigned char* p) {
    return (uint32_t)p[0] | ((uint32_t)p[1] << 8) | ((uint32_t)p[2] << 16) |
           ((uint32_t)p[3] << 24);
}

static uint16_t rd_le16(const unsigned char* p) {
    return (uint16_t)(p[0] | (p[1] << 8));
}

// Minimal RIFF/RF64 fmt parser: fills fmt + data offset/len. False on any
// malformed/truncated header (caller falls back to the ffmpeg pipe).
// u64 throughout: RF64/ds64 sizes exceed 32 bits and standard headers can
// lie (crashed takes) — lengths are always cross-checked against the file.
struct WavInfo {
    uint16_t audio = 0;  // 1 = PCM int, 3 = IEEE float
    uint16_t channels = 0;
    uint32_t rate = 0;
    uint16_t bits = 0;
    uint64_t data_pos = 0;
    uint64_t data_len = 0;
    bool rf64 = false;
};

static uint64_t rd_le64(const unsigned char* p) {
    uint64_t v = 0;
    for (int i = 7; i >= 0; --i) v = (v << 8) | p[i];
    return v;
}

bool parse_wav(const std::string& path, WavInfo& info) {
    FILE* f = nullptr;
    if (fopen_s(&f, path.c_str(), "rb") != 0 || !f) return false;
    unsigned char h[12];
    bool ok = fread(h, 1, 12, f) == 12 &&
              (memcmp(h, "RIFF", 4) == 0 || memcmp(h, "RF64", 4) == 0) &&
              memcmp(h + 8, "WAVE", 4) == 0;
    info.rf64 = ok && memcmp(h, "RF64", 4) == 0;
    uint64_t ds64_data = 0;
    bool have_ds64 = false;
    bool fmt_seen = false;
    while (ok) {
        unsigned char ch[8];
        if (fread(ch, 1, 8, f) != 8) {
            ok = false;
            break;
        }
        uint64_t len = rd_le32(ch + 4);
        __int64 pos = _ftelli64(f);
        if (pos < 0) {
            ok = false;
            break;
        }
        if (memcmp(ch, "ds64", 4) == 0) {
            // ds64 payload: riff_size u64, data_size u64, sample_count u64, table_len u32.
            unsigned char d[32] = {};
            size_t want = len < sizeof(d) ? (size_t)len : sizeof(d);
            if (fread(d, 1, want, f) != want || want < 24) {
                ok = false;
                break;
            }
            ds64_data = rd_le64(d + 8);
            have_ds64 = true;
            if (len > want && _fseeki64(f, pos + (__int64)len, SEEK_SET) != 0) {
                ok = false;
                break;
            }
            continue;
        }
        if (memcmp(ch, "fmt ", 4) == 0) {
            if (len < 16) {
                ok = false;
                break;
            }
            unsigned char fmt[40] = {};
            size_t want = len < sizeof(fmt) ? (size_t)len : sizeof(fmt);
            if (fread(fmt, 1, want, f) != want) {
                ok = false;
                break;
            }
            info.audio = rd_le16(fmt);
            info.channels = rd_le16(fmt + 2);
            info.rate = rd_le32(fmt + 4);
            info.bits = rd_le16(fmt + 14);
            fmt_seen = true;
            if (len > want && _fseeki64(f, pos + (__int64)len, SEEK_SET) != 0) {
                ok = false;
                break;
            }
            continue;
        }
        if (memcmp(ch, "data", 4) == 0) {
            // fmt must precede data in valid files — enforce it explicitly
            // instead of relying on the zero-channel check below.
            if (!fmt_seen) {
                ok = false;
                break;
            }
            info.data_pos = (uint64_t)pos;
            info.data_len = len;
            // RF64 sentinel: real size lives in ds64.
            if (info.rf64 && len == 0xFFFFFFFF && have_ds64) info.data_len = ds64_data;
            break;  // fmt must have preceded data in valid files
        }
        // Skip unknown chunks (fact, LIST, cue…), word-aligned.
        uint64_t skip = len + (len & 1);
        if (_fseeki64(f, pos + (__int64)skip, SEEK_SET) != 0) {
            ok = false;
            break;
        }
    }
    fclose(f);
    if (!(ok && info.channels != 0 && info.data_pos != 0)) return false;
    // Cross-check against the file: crashed-take placeholders
    // (0xFFFFFFFFFFFFFFFF) and lying headers clamp to EOF instead of
    // driving a wild allocation in the reader.
    WIN32_FILE_ATTRIBUTE_DATA fad{};
    if (GetFileAttributesExA(path.c_str(), GetFileExInfoStandard, &fad)) {
        uint64_t flen = ((uint64_t)fad.nFileSizeHigh << 32) | fad.nFileSizeLow;
        if (info.data_pos > flen) return false;
        uint64_t avail = flen - info.data_pos;
        if (info.data_len == 0xFFFFFFFFFFFFFFFFull || info.data_len > avail)
            info.data_len = avail;
    }
    // Sanity: refuse absurd single allocations (32 GiB hard cap mirrors takes).
    if (info.data_len > (32ull << 30)) return false;
    return true;
}

}  // namespace

std::string convert_cache_dir() {
    std::string d = pv_data_dir() + "\\cache\\audio";
    pv_make_dirs(d);
    return d;
}
std::string convert_cache_path(const std::string& src) {
    uint64_t size = 0, stamp = 0;
    file_identity(src, size, stamp);  // zeroes on failure; key stays deterministic
    uint64_t h = fnv(&size, sizeof(size));
    h = fnv(&stamp, sizeof(stamp), h);
    h = fnv(src.data(), src.size(), h);
    char hex[17];
    snprintf(hex, sizeof(hex), "%016llx", (unsigned long long)h);
    std::string stem = pv_basename(src);
    auto dot = stem.rfind('.');
    if (dot != std::string::npos) stem.resize(dot);
    if (stem.size() > 40) stem.resize(40);
    stem = sanitize_filename(stem);
    if (stem.empty()) stem = "audio";
    return convert_cache_dir() + "\\" + stem + "-" + hex + ".wav";
}

bool wav_is_16k_mono(const std::string& path) {
    WavInfo info;
    if (!parse_wav(path, info)) return false;
    return (info.audio == 1 || info.audio == 3) && info.channels == 1 &&
           info.rate == 16000;
}

bool read_wav_16k_mono(const std::string& path, Audio& out, std::string& err) {
    WavInfo info;
    if (!parse_wav(path, info) || info.channels == 0) {
        err = "not a readable WAV file";
        return false;
    }
    if (info.channels != 1 || info.rate != 16000 ||
        !(info.audio == 1 || info.audio == 3)) {
        err = "WAV is not 16kHz mono";
        return false;
    }
    size_t frame = 0;
    if (info.audio == 1) {
        if (info.bits != 8 && info.bits != 16 && info.bits != 24 && info.bits != 32) {
            err = "unsupported WAV bit depth";
            return false;
        }
        frame = (info.bits + 7) / 8;
    } else {
        if (info.bits != 32) {
            err = "unsupported WAV float width";
            return false;
        }
        frame = 4;
    }
    if (frame == 0 || info.data_len % frame != 0) {
        err = "corrupt WAV data chunk";
        return false;
    }
    FILE* f = nullptr;
    if (fopen_s(&f, path.c_str(), "rb") != 0 || !f) {
        err = "cannot open WAV file";
        return false;
    }
    // Sample data starts at an absolute offset past the header chunks.
    // Chunked reads (64 MiB): flat I/O latency on multi-hour takes instead
    // of one giant fread, and no single allocation beyond the caller's PCM.
    // NOTE: the pipeline spills past ~33 min to disk and streams STT windows
    // from the sidecar, so peak RAM stays O(1 window) regardless of length.
    if (_fseeki64(f, (__int64)info.data_pos, SEEK_SET) != 0) {
        fclose(f);
        err = "cannot seek WAV data";
        return false;
    }
    // data_len is already clamped to ≤32 GiB + file size in parse_wav; the
    // resize below can still throw on RAM-starved machines — convert to a
    // clean error instead of a terminate.
    std::vector<unsigned char> raw;
    try {
        raw.resize((size_t)info.data_len);
    } catch (...) {
        fclose(f);
        err = "WAV too large for memory — convert path required";
        return false;
    }
    size_t got = 0;
    bool ok = true;
    while (got < raw.size()) {
        size_t want = raw.size() - got;
        if (want > (64u << 20)) want = (64u << 20);
        size_t k = fread(raw.data() + got, 1, want, f);
        if (k == 0) {
            ok = false;
            break;
        }
        got += k;
    }
    fclose(f);
    if (!ok || got != raw.size()) {
        err = "short WAV read";
        return false;
    }
    const size_t n = raw.size() / frame;
    out.pcm.resize(n);
    const unsigned char* d = raw.data();
    if (info.audio == 3) {  // IEEE float32
        memcpy(out.pcm.data(), d, raw.size());
    } else if (info.bits == 8) {
        for (size_t i = 0; i < n; ++i)
            out.pcm[i] = ((int)d[i] - 128) / 128.0f;
    } else if (info.bits == 16) {
        for (size_t i = 0; i < n; ++i) {
            int v = (int)(int16_t)(d[2 * i] | (d[2 * i + 1] << 8));
            out.pcm[i] = v / 32768.0f;
        }
    } else if (info.bits == 24) {
        for (size_t i = 0; i < n; ++i) {
            int v = (int)(d[3 * i] | (d[3 * i + 1] << 8) | (d[3 * i + 2] << 16));
            if (v & 0x800000) v |= ~0xffffff;
            out.pcm[i] = v / 8388608.0f;
        }
    } else {  // PCM32
        for (size_t i = 0; i < n; ++i) {
            int32_t v;
            memcpy(&v, d + 4 * i, 4);
            out.pcm[i] = (float)((double)v / 2147483648.0);
        }
    }
    out.sample_rate = 16000;
    return true;
}

// 0 = unknown (conversion still proceeds, with heuristic progress).
long long probe_duration_us(const std::string& src, const std::string& ff) {
    SECURITY_ATTRIBUTES sa{sizeof(sa), nullptr, TRUE};
    HANDLE r = nullptr, w = nullptr;
    if (!CreatePipe(&r, &w, &sa, 1 << 16)) return 0;
    SetHandleInformation(r, HANDLE_FLAG_INHERIT, 0);
    std::string qff, qsrc;
    if (!qarg(ff, qff) || !qarg(src, qsrc)) return 0;
    std::string cmd = qff + " -hide_banner -i " + qsrc;
    STARTUPINFOA si{sizeof(si)};
    si.dwFlags = STARTF_USESTDHANDLES;
    si.hStdOutput = w;
    si.hStdError = w;  // info lines land on stderr; merge both
    si.hStdInput = nullptr;
    PROCESS_INFORMATION pi{};
    std::string cmdmut = cmd;
    long long total = 0;
    if (CreateProcessA(nullptr, cmdmut.data(), nullptr, nullptr, TRUE,
                       CREATE_NO_WINDOW, nullptr, nullptr, &si, &pi)) {
        CloseHandle(w);
        w = nullptr;
        char tmp[4096];
        DWORD n = 0;
        std::string out;
        while (ReadFile(r, tmp, sizeof(tmp) - 1, &n, nullptr) && n) {
            tmp[n] = 0;
            out += tmp;
            if (out.size() > 65536) break;
        }
        auto at = out.find("Duration: ");
        int h = 0, m = 0;
        double s = 0;
        if (at != std::string::npos &&
            sscanf_s(out.c_str() + at, "Duration: %d:%d:%lf", &h, &m, &s) == 3)
            total = (long long)(((h * 3600 + m * 60) * 1000000LL + s * 1000000.0));
        WaitForSingleObject(pi.hProcess, INFINITE);
        CloseHandle(pi.hProcess);
        CloseHandle(pi.hThread);
    }
    if (r) CloseHandle(r);
    if (w) CloseHandle(w);
    return total;
}

bool convert_to_wav_16k(const std::string& src, const std::string& dst, std::string& err) {
    const std::string ff = ffmpeg_exe();
    if (GetFileAttributesA(ff.c_str()) == INVALID_FILE_ATTRIBUTES) {
        err = "ffmpeg.exe not found beside exe (scripts/setup-windows.ps1 fetches it)";
        return false;
    }
    const long long total = probe_duration_us(src, ff);
    SECURITY_ATTRIBUTES sa{sizeof(sa), nullptr, TRUE};
    HANDLE r = nullptr, w = nullptr;
    if (!CreatePipe(&r, &w, &sa, 1 << 20)) { err = "CreatePipe failed"; return false; }
    SetHandleInformation(r, HANDLE_FLAG_INHERIT, 0);
    std::string qff, qsrc, qdst;
    if (!qarg(ff, qff) || !qarg(src, qsrc) || !qarg(dst, qdst)) {
        err = "unsupported characters in file path";
        return false;
    }
    std::string cmd = qff +
                      " -v error -nostats -hide_banner -y -i " + qsrc +
                      " -ar 16000 -ac 1 -c:a pcm_s16le -f wav -progress pipe:2 " + qdst;
    STARTUPINFOA si{sizeof(si)};
    si.dwFlags = STARTF_USESTDHANDLES;
    si.hStdOutput = w;  // nearly empty (output goes to file); parsed defensively
    si.hStdError = w;   // -progress lines land here
    si.hStdInput = nullptr;
    PROCESS_INFORMATION pi{};
    std::string cmdmut = cmd;
    if (!CreateProcessA(nullptr, cmdmut.data(), nullptr, nullptr, TRUE,
                        CREATE_NO_WINDOW, nullptr, nullptr, &si, &pi)) {
        CloseHandle(r);
        CloseHandle(w);
        err = "ffmpeg convert launch failed";
        return false;
    }
    CloseHandle(w);
    w = nullptr;
    char tmp[4096];
    DWORD n = 0;
    std::string pending;
    float last_f = 0.0f;
    bool failed = false;
    while (ReadFile(r, tmp, sizeof(tmp) - 1, &n, nullptr) && n) {
        tmp[n] = 0;
        pending += tmp;
        size_t pos = 0;
        while ((pos = pending.find('\n')) != std::string::npos) {
            std::string line = pending.substr(0, pos);
            pending.erase(0, pos + 1);
            if (!line.empty() && line.back() == '\r') line.pop_back();
            const char* k = "out_time_ms=";
            if (line.compare(0, 12, k) == 0 && total > 0) {
                long long us = atoll(line.c_str() + 12) * 1000LL;
                float f = 0.9f * (float)((double)us / (double)total);
                if (f > 0.9f) f = 0.9f;
                if (f - last_f >= 0.01f) {
                    last_f = f;
                    pv::progress_hook(PV_DECODE, f, "Converting…");
                }
            }
        }
        if (!pv::checkpoint()) {  // pause aborts the wait below via sleep loop
            TerminateProcess(pi.hProcess, 1);
            failed = true;
            err = "aborted";
            break;
        }
        if (pending.size() > 65536) pending.erase(0, pending.size() - 65536);
    }
    CloseHandle(r);
    WaitForSingleObject(pi.hProcess, INFINITE);
    DWORD code = 1;
    GetExitCodeProcess(pi.hProcess, &code);
    CloseHandle(pi.hProcess);
    CloseHandle(pi.hThread);
    if (failed) {
        std::remove(dst.c_str());
        return false;
    }
    if (code != 0) {
        std::remove(dst.c_str());
        err = "ffmpeg convert failed";
        return false;
    }
    uint64_t ds = 0, dstamp = 0;
    if (!file_identity(dst, ds, dstamp) || ds <= 44) {
        std::remove(dst.c_str());
        err = "ffmpeg convert produced no audio";
        return false;
    }
    return true;
}

bool load_audio_16k(const std::string& src, Audio& out, std::string& err, int idx,
                    std::string& used_path) {
    // Audacity projects: render the mixdown once into the convert cache,
    // then flow through the normal read/convert path below.
    if (is_audacity_project(src)) {
        std::string dst = convert_cache_path(src);
        // A -wal companion can advance without touching the main file's
        // mtime: a newer WAL invalidates the cached render.
        std::string wal = src + "-wal";
        uint64_t ws = 0, wt = 0;
        if (file_identity(wal, ws, wt) && pv_file_exists(dst)) {
            uint64_t ds = 0, dt = 0;
            if (file_identity(dst, ds, dt) && wt > dt) std::remove(dst.c_str());
        }
        if (!pv_file_exists(dst)) {
            pv::progress_hook(PV_DECODE, 0.02f, "Reading Audacity project…");
            if (!render_audacity_to_wav(src, dst, err)) {
                if (err == "aborted") return false;
                return false;  // project errors are terminal (not pipe-fallback)
            }
        }
        used_path = dst;
        // Rendered mix is project-rate float32: convert via the pipe.
        return decode_to_16k_mono(dst, out, err);
    }
    // Fast path: already exactly what the pipeline eats.
    if (wav_is_16k_mono(src)) {
        used_path = src;
        return read_wav_16k_mono(src, out, err);
    }
    // Cached conversion (key embeds size+mtime, so existence == fresh).
    std::string dst = convert_cache_path(src);
    if (!pv_file_exists(dst)) {
        pv::progress_hook(PV_DECODE, 0.02f, "Converting…");
        if (!convert_to_wav_16k(src, dst, err)) {
            if (err == "aborted") return false;
            pv::pv_log("convert FAILED, pipe fallback: " + src + ": " + err);
            used_path = src;
            return decode_to_16k_mono(src, out, err);
        }
    }
    if (!read_wav_16k_mono(dst, out, err)) {
        // Corrupt cache entry: drop it and fall back to the legacy pipe.
        std::remove(dst.c_str());
        pv::pv_log("cache read FAILED, pipe fallback: " + dst + ": " + err);
        used_path = src;
        return decode_to_16k_mono(src, out, err);
    }
    used_path = dst;
    return true;
}

bool decode_to_16k_mono(const std::string& path, Audio& out, std::string& err) {
    // Shells the bundled ffmpeg.exe over a pipe: no temp files, all formats,
    // output is raw f32le 16kHz mono straight into RAM.
    // (Legacy pipe path: kept as the fallback when cache conversion fails.)
    const std::string ff = ffmpeg_exe();

    SECURITY_ATTRIBUTES sa{sizeof(sa), nullptr, TRUE};
    HANDLE r = nullptr, w = nullptr;
    if (!CreatePipe(&r, &w, &sa, 1 << 20)) { err = "CreatePipe failed"; return false; }
    SetHandleInformation(r, HANDLE_FLAG_INHERIT, 0);

    std::string qff, qpath;
    if (!qarg(ff, qff) || !qarg(path, qpath)) {
        err = "unsupported characters in file path";
        return false;
    }
    std::string cmd = qff + " -v error -i " + qpath +
                      " -ar 16000 -ac 1 -c:a pcm_f32le -f f32le -";
    STARTUPINFOA si{sizeof(si)};
    si.dwFlags = STARTF_USESTDHANDLES;
    si.hStdOutput = w;
    si.hStdError = GetStdHandle(STD_ERROR_HANDLE);
    PROCESS_INFORMATION pi{};
    std::string cmdmut = cmd;
    if (!CreateProcessA(nullptr, cmdmut.data(), nullptr, nullptr, TRUE,
                        CREATE_NO_WINDOW, nullptr, nullptr, &si, &pi)) {
        CloseHandle(r);
        CloseHandle(w);
        err = "ffmpeg.exe not found beside exe (scripts/setup-windows.ps1 fetches it)";
        return false;
    }
    CloseHandle(w);
    std::vector<char> buf;
    char tmp[1 << 16];
    DWORD n = 0;
    // Decode progress: the pipe yields no percentages, so pulse from bytes
    // streamed (throttled, asymptotic — never stalls, never claims 1.0).
    long long total = 0;
    {
        WIN32_FILE_ATTRIBUTE_DATA fad{};
        if (GetFileAttributesExA(path.c_str(), GetFileExInfoStandard, &fad))
            total = (static_cast<long long>(fad.nFileSizeHigh) << 32) | fad.nFileSizeLow;
    }
    float last_f = 0.0f;
    while (ReadFile(r, tmp, sizeof(tmp), &n, nullptr) && n) {
        buf.insert(buf.end(), tmp, tmp + n);
        double x = (double)buf.size() / (double)(total > 0 ? total : (1 << 20));
        float f = 0.05f + 0.85f * (float)(x / (1.0 + x));
        if (f - last_f >= 0.02f) {
            last_f = f;
            pv::progress_hook(PV_DECODE, f, "Decoding...");
        }
    }
    CloseHandle(r);
    WaitForSingleObject(pi.hProcess, INFINITE);
    DWORD code = 1;
    GetExitCodeProcess(pi.hProcess, &code);
    CloseHandle(pi.hProcess);
    CloseHandle(pi.hThread);
    if (code != 0 || buf.empty()) { err = "ffmpeg decode failed"; return false; }
    out.pcm.resize(buf.size() / 4);
    memcpy(out.pcm.data(), buf.data(), out.pcm.size() * 4);
    out.sample_rate = 16000;
    return true;
}

// Adaptive pre-STT denoise: DC removal, background-learned room-tone gate
// (first ~60 s calibration, slow follower after, bounded re-learn on drastic
// floor jumps), and a light 2:1 compressor above -12 dBFS. Operates on the
// in-RAM STT copy only — archival takes and the convert cache stay intact.
// Deterministic (no RNG, fixed block math); false only on abort.
bool denoise_for_stt(std::vector<float>& pcm, const std::string& mode, int idx) {
    if (mode == "off" || pcm.empty()) return true;
    const double gate_db = (mode == "aggressive") ? 12.0 : 8.0;
    const double gate_ratio = pow(10.0, gate_db / 20.0);
    const size_t SR = 16000;
    // DC removal (global mean — one pass, branchless).
    double mean = 0;
    for (float v : pcm) mean += v;
    mean /= (double)pcm.size();
    if (mean != 0.0) {
        const float m = (float)mean;
        for (float& v : pcm) v -= m;
    }
    // 20 ms analysis frames; RMS per frame.
    const size_t F = 320;
    const size_t nframes = (pcm.size() + F - 1) / F;
    std::vector<float> rms(nframes, 0.0f);
    for (size_t f = 0; f < nframes; ++f) {
        double acc = 0;
        const size_t n = (f + 1) * F <= pcm.size() ? F : pcm.size() - f * F;
        for (size_t i = 0; i < n; ++i) {
            const float v = pcm[f * F + i];
            acc += (double)v * v;
        }
        rms[f] = (float)sqrt(acc / (double)(n > 0 ? n : 1));
    }
    auto block_floor = [&](size_t a, size_t b) {
        // 10th-percentile RMS over [a,b): the room tone under the speech.
        if (a >= b || a >= rms.size()) return 1e-6f;
        if (b > rms.size()) b = rms.size();
        std::vector<float> q(rms.begin() + a, rms.begin() + b);
        const size_t k = q.size() / 10;
        std::nth_element(q.begin(), q.begin() + k, q.end());
        float v = q[k];
        return v > 1e-6f ? v : 1e-6f;
    };
    // Calibration: first ~60 s (or the whole take when shorter).
    const size_t cal_frames = nframes < 3000 ? nframes : 3000;
    float floor_rms = block_floor(0, cal_frames);
    float thresh = floor_rms * (float)gate_ratio;
    int readapt_logged = 0;
    // Gate state: envelope follower + hangover + click-free ramps.
    float env = 0.0f, gain = 0.0f;
    size_t hang = 0;
    const size_t HANG = 3200;  // 200 ms release tail
    bool relearn = false;
    size_t relearn_left = 0;  // conservative window after a drastic change
    size_t sec_floor_hits = 0;
    const size_t FR1S = SR / F;  // frames per second (50)
    for (size_t f = 0; f < nframes; ++f) {
        // 1 s-block floor tracking with drastic-change detection (>10 dB up,
        // 2 consecutive seconds) → 3 s conservative re-learn.
        if (FR1S > 0 && f % FR1S == 0 && f > 0) {
            const size_t b0 = (f >= FR1S) ? f - FR1S : 0;
            const float bf = block_floor(b0, f);
            if (bf > floor_rms * 3.16f) {  // +10 dB
                if (++sec_floor_hits >= 2 && !relearn) {
                    // NOTE: a second +10 dB jump arriving mid-relearn starts
                    // nothing new (guard above) and extends unlogged past the
                    // 3-line readapt cap — accepted: the gate errs open, and
                    // speech is never the casualty.
                    relearn = true;
                    relearn_left = 3 * FR1S;
                    if (readapt_logged < 3) {
                        char b[128];
                        snprintf(b, sizeof(b), "denoise re-adapted at %02u:%02u",
                                 (unsigned)(f / FR1S / 60), (unsigned)((f / FR1S) % 60));
                        pv::pv_log(b);
                        ++readapt_logged;
                    }
                }
            } else {
                sec_floor_hits = 0;
                // Slow follower for gradual drift (rooms warm up, HVAC hums).
                floor_rms = floor_rms * 0.95f + bf * 0.05f;
            }
            thresh = floor_rms * (float)gate_ratio;
        }
        if (relearn) {
            // Conservative: halve the threshold so speech survives while the
            // new floor is learned; refresh the floor from this window.
            if (--relearn_left == 0) {
                relearn = false;
                const size_t rb = (f >= 3 * FR1S) ? f - 3 * FR1S : 0;
                floor_rms = block_floor(rb, f + 1);
                sec_floor_hits = 0;
            }
        }
        const float thr = relearn ? thresh * 0.5f : thresh;
        const float fr = rms[f];
        // Envelope: 1 ms attack, 100 ms release.
        if (fr > env)
            env += (fr - env) * 0.9f;
        else
            env += (fr - env) * 0.002f;
        if (env > thr) {
            hang = HANG;
        } else if (hang > 0) {
            hang -= (hang > F) ? F : hang;
        }
        const float target = (hang > 0) ? 1.0f : 0.0f;
        const float coeff = (target > gain) ? 0.05f : 0.0008f;  // 5 ms / ~80 ms
        gain += (target - gain) * coeff;
        // Light 2:1 compression above -12 dBFS on the gated signal.
        const size_t base = f * F;
        const size_t n = (base + F <= pcm.size()) ? F : pcm.size() - base;
        for (size_t i = 0; i < n; ++i) {
            float v = pcm[base + i] * gain;
            const float a = v < 0 ? -v : v;
            if (a > 0.251f) {  // -12 dBFS knee
                const float over = a / 0.251f;
                const float comp = 0.251f * (1.0f + (over - 1.0f) * 0.5f);
                v = (v < 0 ? -comp : comp);
            }
            pcm[base + i] = v;
        }
        if ((f & 1023) == 0) {
            pv::progress_hook(PV_DECODE, 0.15f + 0.1f * (float)f / (float)nframes,
                              "Reducing noise…");
            if (!pv::checkpoint()) return false;
        }
    }
    return true;
}

// ---- Video frame sampling (Section 5): exact timestamps, never guessed.
// Base pass at an attention-driven rate plus an analysis pass that parses
// `showinfo` pts_time values for real scene cuts, then seeks one frame per
// cut. Every extracted frame lands in frames.md with its t=MM:SS so captions
// and transcript sentences confirm against each other by interval.
// Caps: attention-scaled base (96/192/384) + 128 cuts = 512 frames max per film (coverage logged).
// The frames dir is temp: swept on success, kept for forensics on failure
// (Clean caches sweeps orphans). The SOURCE video is only ever read.

std::string frames_dir_for(const std::string& shown, int idx) {
    std::string stem = pv_basename(shown);
    auto dot = stem.rfind('.');
    if (dot != std::string::npos) stem.resize(dot);
    if (stem.size() > 40) stem.resize(40);
    stem = sanitize_filename(stem);
    if (stem.empty()) stem = "video";
    std::string d = pv_data_dir() + "\\frames\\" + stem + "__" + std::to_string(idx);
    pv_make_dirs(d);
    return d;
}

std::string fmt_t(double secs) {
    if (secs < 0) secs = 0;
    long s = (long)secs;
    char b[32];
    if (s >= 3600)
        snprintf(b, sizeof(b), "%ld:%02ld:%02ld", s / 3600, (s % 3600) / 60, s % 60);
    else
        snprintf(b, sizeof(b), "%02ld:%02ld", s / 60, s % 60);
    return b;
}

bool run_ffmpeg_quiet(const std::string& cmdline, std::string* capture) {
    // Fire-and-wait with optional merged-output capture (for showinfo).
    SECURITY_ATTRIBUTES sa{sizeof(sa), nullptr, TRUE};
    HANDLE r = nullptr, w = nullptr;
    const bool cap = capture != nullptr;
    if (cap) {
        if (!CreatePipe(&r, &w, &sa, 1 << 16)) return false;
        SetHandleInformation(r, HANDLE_FLAG_INHERIT, 0);
    }
    STARTUPINFOA si{sizeof(si)};
    if (cap) {
        // GUI-subsystem processes may have null std handles: only request
        // handle inheritance when we actually redirect.
        si.dwFlags = STARTF_USESTDHANDLES;
        si.hStdOutput = w;
        si.hStdError = w;
        si.hStdInput = nullptr;
    }
    PROCESS_INFORMATION pi{};
    std::string cmdmut = cmdline;
    if (!CreateProcessA(nullptr, cmdmut.data(), nullptr, nullptr, TRUE,
                        CREATE_NO_WINDOW, nullptr, nullptr, &si, &pi)) {
        if (r) CloseHandle(r);
        if (w) CloseHandle(w);
        return false;
    }
    if (w) {
        CloseHandle(w);
        w = nullptr;
    }
    if (cap) {
        char tmp[4096];
        DWORD n = 0;
        while (ReadFile(r, tmp, sizeof(tmp) - 1, &n, nullptr) && n) {
            tmp[n] = 0;
            *capture += tmp;
            if (capture->size() > (1 << 20)) break;
        }
    }
    if (r) CloseHandle(r);
    WaitForSingleObject(pi.hProcess, INFINITE);
    DWORD code = 1;
    GetExitCodeProcess(pi.hProcess, &code);
    CloseHandle(pi.hProcess);
    CloseHandle(pi.hThread);
    return code == 0;
}

bool sample_video_frames(const std::string& src, const std::string& shown, int idx,
                         int attention, std::string& frames_dir,
                         std::vector<SampledFrame>& frames, std::string& err) {
    const std::string ff = ffmpeg_exe();
    if (GetFileAttributesA(ff.c_str()) == INVALID_FILE_ATTRIBUTES) {
        err = "ffmpeg.exe not found beside exe (scripts/setup-windows.ps1 fetches it)";
        return false;
    }
    frames_dir = frames_dir_for(shown, idx);
    std::string qff, qsrc, qpat;
    if (!qarg(ff, qff) || !qarg(src, qsrc) ||
        !qarg(frames_dir + "\\f_%05d.jpg", qpat)) {
        err = "unsupported characters in file path";
        return false;
    }
    // Attention → base rate: Academic ≈ every 24th frame @24fps (1 fps),
    // Balanced ≈ every ~100th (1/4 fps), Overview ≈ sparse (1/10 fps).
    const char* fps = attention >= 67 ? "fps=1" : (attention >= 34 ? "fps=1/4" : "fps=1/10");
    const double period = attention >= 67 ? 1.0 : (attention >= 34 ? 4.0 : 10.0);
    pv::progress_hook(PV_DECODE, 0.2f, "Sampling frames…");
    if (!pv::checkpoint()) {
        err = "aborted";
        return false;
    }
    // Pass 1: base-rate frames (timestamps exact by construction: t = k/R).
    // The extraction cap scales with attention: Overview captions a sparse
    // subset, so pulling 384 JPEGs just to discard most is wasted time+disk.
    const int base_cap = attention >= 67 ? 384 : (attention >= 34 ? 192 : 96);
    char capbuf[16];
    snprintf(capbuf, sizeof(capbuf), "%d", base_cap);
    std::string cmd = qff + " -v error -y -i " + qsrc + " -vf \"" +
                      std::string(fps) + ",scale=480:-1\" -frames:v " + capbuf + " -q:v 4 " +
                      qpat;
    if (!run_ffmpeg_quiet(cmd, nullptr)) {
        err = "frame sampling failed";
        return false;
    }
    for (int k = 0; k < base_cap; ++k) {
        char name[32];
        snprintf(name, sizeof(name), "f_%05d.jpg", k + 1);
        if (GetFileAttributesA((frames_dir + "\\" + name).c_str()) ==
            INVALID_FILE_ATTRIBUTES)
            break;
        frames.push_back({name, k * period, false});
    }
    // Pass 2: scene-cut analysis (showinfo pts_time = exact cut times).
    pv::progress_hook(PV_DECODE, 0.24f, "Detecting scene cuts…");
    if (!pv::checkpoint()) {
        err = "aborted";
        return false;
    }
    std::string info;
    // NOTE: the comma inside gt() must be backslash-escaped (it separates
    // filters, not expression args); showinfo chains as its own filter.
    std::string acmd = qff + " -hide_banner -i " + qsrc +
                       " -vf select='gt(scene\\,0.4)',showinfo -vsync 0 -f null -";
    std::vector<double> cuts;
    if (run_ffmpeg_quiet(acmd, &info)) {
        size_t p = 0;
        while ((p = info.find("pts_time:", p)) != std::string::npos && cuts.size() < 128) {
            p += 9;
            cuts.push_back(atof(info.c_str() + p));
        }
    }
    // Pass 3: one frame per cut (exact seeks).
    size_t ticks = 0;
    for (double t : cuts) {
        if (!pv::checkpoint()) {
            err = "aborted";
            return false;
        }
        char ts[32];
        snprintf(ts, sizeof(ts), "%.3f", t);
        std::string safe = "cut_" + fmt_t(t) + ".jpg";
        for (char& c : safe) {
            if (c == ':') c = '-';
        }
        std::string qcut;
        if (!qarg(frames_dir + "\\" + safe, qcut)) continue;  // safe is time-derived; skip on failure
        std::string cmd2 = qff + " -v error -y -ss " + ts + " -i " + qsrc +
                           " -frames:v 1 -q:v 4 " + qcut;
        if (run_ffmpeg_quiet(cmd2, nullptr)) {
            // Skip cuts duplicating an already-sampled base time (within
            // half the base period): fewer frames, same coverage.
            bool dup = false;
            for (const auto& f : frames) {
                double dt = f.t - t;
                if (dt < 0) dt = -dt;
                if (dt < period / 2) {
                    dup = true;
                    break;
                }
            }
            if (!dup) {
                frames.push_back({safe, t, true});
                if (++ticks % 16 == 0)
                    pv::progress_hook(PV_DECODE, 0.26f, "Sampling frames…");
            }
        }
        if (frames.size() >= 512) break;
    }
    std::sort(frames.begin(), frames.end(),
              [](const SampledFrame& a, const SampledFrame& b) { return a.t < b.t; });
    for (const auto& f : frames) {
        char fb[96];
        snprintf(fb, sizeof(fb), "frame %s t=%s%s", f.file.c_str(), fmt_t(f.t).c_str(),
                 f.cut ? " (cut)" : "");
        pv::pv_log(fb);
    }
    // frames.md: the audit of what the AI saw (swept on success).
    {
        std::string doc = "# Frames\n\nSampled " + std::to_string(frames.size()) +
                          " frame(s) at base " + fps + " + " + std::to_string(cuts.size()) +
                          " cut(s) (attention " + std::to_string(attention) + ").\n\n";
        for (const auto& f : frames)
            doc += (f.cut ? "- [CUT " : "- [") + fmt_t(f.t) + "] " + f.file + "\n";
        doc += "\n## Captions\n\npending VLM captioning.\n";
        FILE* fo = nullptr;
        if (fopen_s(&fo, (frames_dir + "\\frames.md").c_str(), "wb") == 0 && fo) {
            fwrite(doc.data(), 1, doc.size(), fo);
            fclose(fo);
        }
    }
    char lb[160];
    snprintf(lb, sizeof(lb), "sampled %u frames (%u cuts)",
             (unsigned)frames.size(), (unsigned)cuts.size());
    pv::pv_log(lb);
    return true;
}

void sweep_frames_dir(const std::string& dir) {
    // Best-effort recursive remove of the temp frames dir (success path).
    if (dir.empty()) return;
    std::string pat = dir + "\\*";
    WIN32_FIND_DATAA fd{};
    HANDLE h = FindFirstFileA(pat.c_str(), &fd);
    if (h != INVALID_HANDLE_VALUE) {
        do {
            if (strcmp(fd.cFileName, ".") != 0 && strcmp(fd.cFileName, "..") != 0)
                std::remove((dir + "\\" + fd.cFileName).c_str());
        } while (FindNextFileA(h, &fd));
        FindClose(h);
    }
    RemoveDirectoryA(dir.c_str());
}

}  // namespace pv
