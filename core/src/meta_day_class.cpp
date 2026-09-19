// Day N / lecture-number parsing + Class hint resolution.
// Priority: explicit filename tokens > sqlite DB memory > file creation date > folder name.
#include "internal.h"
// NOTE: WIN32_LEAN_AND_MEAN comes from CMake compile definitions (see above).
#include <windows.h>
#include <regex>

namespace pv {

std::string sanitize_filename(std::string s) {
    for (char& c : s)
        if (c == '<' || c == '>' || c == ':' || c == '"' || c == '/' || c == '\\' ||
            c == '|' || c == '?' || c == '*')
            c = '-';
    while (!s.empty() && (s.back() == ' ' || s.back() == '.')) s.pop_back();
    if (s.empty()) s = "Untitled";
    return s;
}

static bool parse_day(const std::string& name, std::string& day) {
    static const std::regex pats[] = {
        std::regex(R"(day[\s_\-]*(\d{1,3}))", std::regex::icase),
        std::regex(R"(lec(?:ture)?[\s_\-]*(\d{1,3}))", std::regex::icase),
        std::regex(R"((\d{4}-\d{2}-\d{2}))"),
        std::regex(R"((?:^|[\s_\-\[\(])(\d{1,3})(?:[\s_\-\]\)]|$))"),
    };
    std::smatch m;
    if (std::regex_search(name, m, pats[0]) || std::regex_search(name, m, pats[1])) {
        day = "Day " + m[1].str();
        return true;
    }
    if (std::regex_search(name, m, pats[2])) {
        day = m[1].str();
        return true;
    }
    if (std::regex_search(name, m, pats[3])) {  // bare number: "lecture 4", "[03]", "04 - topic"
        day = "Day " + m[1].str();
        return true;
    }
    return false;
}

bool resolve_day_class(const std::string& display_path, const std::string& real_path,
                       const std::string& db_path, std::string& day_label,
                       std::string& class_hint) {
    // Document/merge jobs pass the ORIGINAL filename as `display_path` (for
    // human-meaningful tokens) while the payload lives at `real_path` (the
    // UTF-8 cache). Token parsing uses the display name; the creation-date
    // fallback MUST use a real on-disk path — a bare filename makes the
    // date lookup fail and the day used to degrade to a placeholder that
    // sanitized into ugly "Day -" titles (that placeholder is gone).
    std::string stem = pv_stem(display_path);
    if (stem.empty()) stem = display_path;
    std::string db_day, db_class;
    const bool remembered = db_lookup(db_path, display_path, db_day, db_class);

    auto file_date = [](const std::string& p, std::string& out) {
        if (p.empty()) return false;
        WIN32_FILE_ATTRIBUTE_DATA fad{};
        if (!GetFileAttributesExA(p.c_str(), GetFileExInfoStandard, &fad)) return false;
        SYSTEMTIME st;
        if (!FileTimeToSystemTime(&fad.ftCreationTime, &st)) return false;
        char b[32];
        snprintf(b, sizeof(b), "%04u-%02u-%02u", st.wYear, st.wMonth, st.wDay);
        out = b;
        return true;
    };

    // Priority: explicit filename tokens > sqlite memory > file creation
    // date (real payload path first, display path second) > "Day 1".
    if (!parse_day(stem, day_label)) {
        if (remembered && !db_day.empty()) {
            day_label = db_day;
        } else if (!file_date(real_path, day_label) && !file_date(display_path, day_label)) {
            day_label = "Day 1";
        }
    }
    // Class hint: remembered label wins, else parent folder (the LLM pass in
    // pipeline.cpp later constrains/infers the real class from known_classes).
    if (remembered && !db_class.empty()) {
        class_hint = db_class;
        return true;
    }
    class_hint = pv_parent_name(display_path);
    if (class_hint.empty()) class_hint = "General";
    return true;
}

}  // namespace pv
