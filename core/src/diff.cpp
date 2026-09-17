// Line-level LCS diff -> JSON for the GUI Diff tab.
// Emits [{t:"+/-/ ", text, chunk}] so JS renders green/red like GitHub,
// and clicking a "+" line scrolls to its source chunk.
#include "internal.h"
#include <cstdio>
#include <sstream>

namespace pv {
namespace {
std::vector<std::string> lines_of(const std::string& s) {
    std::vector<std::string> v;
    std::istringstream in(s);
    std::string l;
    while (std::getline(in, l)) v.push_back(l);
    if (v.empty()) v.emplace_back("");
    return v;
}
}  // namespace

std::string diff_to_json(const std::string& raw, const std::string& summary) {
    auto a = lines_of(raw), b = lines_of(summary);
    size_t n = a.size(), m = b.size();
    // Cap DP to keep memory flat on huge transcripts (banded fallback: tail diff).
    const size_t CAP = 2000;
    if (n > CAP || m > CAP) {
        std::string j = "[";
        for (size_t i = 0; i < n && i < CAP; ++i)
            j += "{\"t\":\"-\",\"text\":\"" + json_escape(a[i]) + "\",\"chunk\":0},";
        for (size_t i = 0; i < m && i < CAP; ++i)
            j += "{\"t\":\"+\",\"text\":\"" + json_escape(b[i]) + "\",\"chunk\":0},";
        j.back() = ']';
        return j;
    }
    std::vector<std::vector<int>> dp(n + 1, std::vector<int>(m + 1, 0));
    for (size_t i = n; i-- > 0;)
        for (size_t j = m; j-- > 0;)
            dp[i][j] = (a[i] == b[j]) ? dp[i + 1][j + 1] + 1 : (dp[i + 1][j] > dp[i][j + 1] ? dp[i + 1][j] : dp[i][j + 1]);
    std::string j = "[";
    size_t i = 0, k = 0;
    while (i < n && k < m) {
        if (a[i] == b[k]) {
            j += "{\"t\":\" \",\"text\":\"" + json_escape(a[i]) + "\",\"chunk\":0},";
            ++i; ++k;
        } else if (dp[i + 1][k] >= dp[i][k + 1]) {
            j += "{\"t\":\"-\",\"text\":\"" + json_escape(a[i]) + "\",\"chunk\":0},";
            ++i;
        } else {
            j += "{\"t\":\"+\",\"text\":\"" + json_escape(b[k]) + "\",\"chunk\":0},";
            ++k;
        }
    }
    while (i < n) { j += "{\"t\":\"-\",\"text\":\"" + json_escape(a[i++]) + "\",\"chunk\":0},"; }
    while (k < m) { j += "{\"t\":\"+\",\"text\":\"" + json_escape(b[k++]) + "\",\"chunk\":0},"; }
    if (j.size() > 1 && j.back() == ',') j.back() = ']';
    else j += ']';
    return j;
}

}  // namespace pv
