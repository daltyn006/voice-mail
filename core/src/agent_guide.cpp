// Agent guide: runtime behavior supplement for the on-device instruction
// model (AGENTS.md at repo root / beside the exe). Loaded once, cached,
// truncated to a bounded excerpt so prompt budgets stay predictable.
// Missing file -> short built-in fallback; never fatal.
#include "internal.h"
// NOTE: WIN32_LEAN_AND_MEAN comes from CMake compile definitions (see above).
#include <windows.h>

#include <fstream>
#include <map>
#include <mutex>
#include <sstream>

namespace pv {
namespace {

const char* kFallback =
    "Study-note assistant. Preserve every statistic, date, name, decision. "
    "Never invent facts. Reply shapes: exactly what each prompt asks for, "
    "nothing else.";

std::string exe_dir() {
    char exe[MAX_PATH]{};
    GetModuleFileNameA(nullptr, exe, MAX_PATH);
    std::string dir = exe;
    auto sl = dir.find_last_of("\\/");
    if (sl != std::string::npos) dir.resize(sl);
    return dir;
}

std::string read_file(const std::string& path) {
    std::ifstream f(path, std::ios::binary);
    if (!f) return "";
    std::ostringstream ss;
    ss << f.rdbuf();
    std::string s = ss.str();
    if (s.size() > 2000) s.resize(2000);
    return s;
}

}  // namespace

// Bounded runtime supplement (~2KB) for every LLM system prompt.
std::string agent_guide() { return agent_guide(""); }

std::string agent_guide(const std::string& name) {
    static std::mutex mu;
    static std::map<std::string, std::string> cache;
    std::lock_guard<std::mutex> l(mu);
    auto it = cache.find(name);
    if (it != cache.end()) return it->second;
    std::string out;
    const std::string dir = exe_dir();
    if (name.empty()) {
        // Installed layout, dev layout (target/debug), cmake build tree.
        for (const char* rel : {"\\AGENTS.md", "\\..\\AGENTS.md", "\\..\\..\\AGENTS.md"}) {
            out = read_file(dir + rel);
            if (!out.empty()) break;
        }
    } else {
        // Tier guides ship beside the exe (packager resources) with the
        // same dev-tree fallbacks as the legacy file.
        const std::string rel = "\\guides\\" + name + ".md";
        const char* cands[] = {"", "\\..", "\\..\\..\\config"};
        for (const char* base : cands) {
            out = read_file(dir + base + rel);
            if (!out.empty()) break;
        }
        // Dev-repo direct fallback (running from target/debug).
        if (out.empty()) {
            for (const char* base : {"\\..\\..\\config\\guides\\"}) {
                out = read_file(dir + base + name + ".md");
                if (!out.empty()) break;
            }
        }
    }
    if (out.empty()) out = kFallback;
    cache[name] = out;
    return out;
}

}  // namespace pv
