// SRT sidecar builder (video jobs): timestamped transcript lines
// ([MM:SS] / [H:MM:SS] sentence starts) become numbered cues. Standalone TU
// so the harness executes the real code (see tests/fixtures/test_audacity.cpp).
#include <cstdio>
#include <string>
#include <utility>
#include <vector>

namespace pv {

// ---- SRT sidecar (video jobs only): timestamped transcript lines
// ([MM:SS] / [H:MM:SS] sentence starts from the STT path) become numbered
// cues. Cue end = next cue start; the last cue ends at the media duration
// (or +4 s when unknown, e.g. phase-2 transcript inputs). Unstamped lines
// join the previous cue (wrapped sentences). Empty when no stamps found.
long long parse_srt_stamp(const std::string& line, size_t& end_off) {
    // Expects "...[H:MM:SS]..." or "...[MM:SS]..."; returns ms. end_off is
    // the offset just past "]" (callers substr from there — it is an end
    // offset, not a width, despite the old name).
    size_t a = line.find('[');
    if (a == std::string::npos) return -1;
    size_t b = line.find(']', a + 1);
    if (b == std::string::npos || b - a > 16) return -1;
    std::string t = line.substr(a + 1, b - a - 1);
    int h = 0, m = 0, s = 0;
    int n = 0;
    for (char c : t) {
        if (c == ':') {
            ++n;
        } else if (c < '0' || c > '9') {
            return -1;
        }
    }
    if (n == 1) {
#ifdef _WIN32
        if (sscanf_s(t.c_str(), "%d:%d", &m, &s) != 2) return -1;
#else
        if (sscanf(t.c_str(), "%d:%d", &m, &s) != 2) return -1;
#endif
    } else if (n == 2) {
#ifdef _WIN32
        if (sscanf_s(t.c_str(), "%d:%d:%d", &h, &m, &s) != 3) return -1;
#else
        if (sscanf(t.c_str(), "%d:%d:%d", &h, &m, &s) != 3) return -1;
#endif
    } else {
        return -1;
    }
    if (m < 0 || m >= 60 || s < 0 || s >= 60 || h < 0) return -1;
    end_off = b + 1;  // through "]"
    return ((long long)h * 3600 + m * 60 + s) * 1000LL;
}

std::string fmt_srt_ts(long long ms) {
    if (ms < 0) ms = 0;
    long long h = ms / 3600000;
    long long m = (ms % 3600000) / 60000;
    long long s = (ms % 60000) / 1000;
    long long mm = ms % 1000;
    char b[32];
    snprintf(b, sizeof(b), "%02lld:%02lld:%02lld,%03lld", h, m, s, mm);
    return b;
}

std::string srt_from_transcript(const std::string& raw, double duration_secs) {
    std::vector<std::pair<long long, std::string>> cues;
    size_t pos = 0;
    while (pos < raw.size() && cues.size() < 20000) {
        size_t e = raw.find('\n', pos);
        std::string line = raw.substr(pos, e == std::string::npos ? std::string::npos : e - pos);
        pos = (e == std::string::npos) ? raw.size() : e + 1;
        while (!line.empty() && (line.back() == '\r' || line.back() == ' ' || line.back() == '\t'))
            line.pop_back();
        if (line.empty()) continue;
        size_t w = 0;
        long long ms = parse_srt_stamp(line, w);
        if (ms < 0) {
            // Unstamped continuation: join the previous cue.
            if (!cues.empty() && cues.back().second.size() < 2000) {
                cues.back().second += " " + line;
                if (cues.back().second.size() > 2000)
                    cues.back().second.resize(2000);
            }
            continue;
        }
        std::string text = line.substr(w);
        size_t f = text.find_first_not_of(" \t");
        text = (f == std::string::npos) ? "" : text.substr(f);
        if (!cues.empty() && ms < cues.back().first) continue;  // non-monotonic: drop
        cues.push_back({ms, text});
    }
    if (cues.empty()) return "";
    long long last_end = duration_secs > 0
                             ? (long long)(duration_secs * 1000.0)
                             : cues.back().first + 4000;
    std::string o;
    for (size_t i = 0; i < cues.size(); ++i) {
        long long end = (i + 1 < cues.size()) ? cues[i + 1].first : last_end;
        if (end <= cues[i].first) end = cues[i].first + 1000;
        char nb[32];
        snprintf(nb, sizeof(nb), "%u\n", (unsigned)(i + 1));
        o += nb;
        o += fmt_srt_ts(cues[i].first) + " --> " + fmt_srt_ts(end) + "\n";
        o += cues[i].second.empty() ? "(silence)" : cues[i].second;
        o += "\n\n";
    }
    return o;
}

}  // namespace pv
