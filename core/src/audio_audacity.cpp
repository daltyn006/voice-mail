// Native Audacity project import (.aup3 / .aup4 / .aup3unsaved).
//
// Clean-room implementation written against the published file-format facts:
// the SQLite schema (project/autosave/sampleblocks tables, 'AUDY'
// application_id) and the binary-XML field layout documented in Audacity's
// ProjectFileIO / ProjectSerializer sources plus third-party format notes.
// No Audacity code is vendored, linked, or copied here: we open the file
// with our own SQLite copy, decode the project document with our own parser,
// stitch sample blocks ourselves, and render one mono mixdown WAV that flows
// through the normal ffmpeg -> 16k pipeline. Tenacity / Saucedacity /
// Audacium share the AUP3 format, so their projects import identically.
//
// Simplifications vs a full DAW export (documented, transcription-scoped):
// - envelopes: linear interpolation, t relative to clip start, default 1.0.
// - pan: equal-power mono fold, unity at center, then peak-normalize if > 1.
// - realtime effects / time-stretch / labels / time tracks: ignored.
// - missing sample blocks: silence + log (recovery semantics).
// - muted tracks skipped (solo honored); render capped at 5 h.
#include "internal.h"

#ifdef PV_HAVE_SQLITE
#include "sqlite3.h"
#endif

#include <windows.h>

#include <cctype>
#include <cmath>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <map>
#include <string>
#include <vector>

namespace pv {
namespace {

// ---- Audacity sample formats (SizeT codes in sequence + sampleblocks rows).
constexpr uint32_t SF_INT16 = 0x20001;
constexpr uint32_t SF_INT24 = 0x40001;
constexpr uint32_t SF_FLOAT = 0x4000F;

// Binary-XML field types (ProjectSerializer layout, little-endian).
enum BxField : unsigned char {
    BX_CHAR_SIZE = 0,
    BX_START_TAG = 1,
    BX_END_TAG = 2,
    BX_STRING = 3,
    BX_INT = 4,
    BX_BOOL = 5,
    BX_LONG = 6,
    BX_LONGLONG = 7,
    BX_SIZET = 8,
    BX_FLOAT = 9,
    BX_DOUBLE = 10,
    BX_DATA = 11,
    BX_RAW = 12,
    BX_PUSH = 13,
    BX_POP = 14,
    BX_NAME = 15,
};

struct XAttr {
    std::string name;
    std::string value;
};

struct XNode {
    std::string tag;
    std::vector<XAttr> attrs;
    std::vector<XNode> children;
};

const char* xattr(const XNode& n, const char* key, const char* dflt = "") {
    for (const auto& a : n.attrs)
        if (a.name == key) return a.value.c_str();
    return dflt;
}

double xdouble(const XNode& n, const char* key, double dflt) {
    const char* v = xattr(n, key, nullptr);
    if (!v || !*v) return dflt;
    return atof(v);
}

long long xint64(const XNode& n, const char* key, long long dflt) {
    const char* v = xattr(n, key, nullptr);
    if (!v || !*v) return dflt;
    return atoll(v);
}

// ---- Minimal binary-XML decoder (dict + doc blobs concatenated). ----

struct BxCursor {
    const unsigned char* p;
    size_t len;
    size_t pos = 0;
    bool err = false;

    bool need(size_t n) {
        if (pos + n > len) {
            err = true;
            return false;
        }
        return true;
    }
    unsigned char u8() {
        if (!need(1)) return 0;
        return p[pos++];
    }
    uint16_t u16() {
        if (!need(2)) return 0;
        uint16_t v = (uint16_t)p[pos] | ((uint16_t)p[pos + 1] << 8);
        pos += 2;
        return v;
    }
    int32_t i32() {
        if (!need(4)) return 0;
        uint32_t v = (uint32_t)p[pos] | ((uint32_t)p[pos + 1] << 8) |
                     ((uint32_t)p[pos + 2] << 16) | ((uint32_t)p[pos + 3] << 24);
        pos += 4;
        return (int32_t)v;
    }
    uint32_t u32() { return (uint32_t)i32(); }
    int64_t i64() {
        if (!need(8)) return 0;
        uint64_t v = 0;
        for (int i = 7; i >= 0; --i) v = (v << 8) | p[pos + i];
        pos += 8;
        return (int64_t)v;
    }
    float f32() {
        int32_t b = i32();
        float f;
        memcpy(&f, &b, 4);
        return f;
    }
    double f64() {
        int64_t b = i64();
        double d;
        memcpy(&d, &b, 8);
        return d;
    }
};

// UTF-16LE/UTF-32LE -> UTF-8 (ASCII fast path inline).
std::string to_utf8(const unsigned char* s, size_t bytes, int char_size) {
    std::string o;
    if (char_size == 1) {
        o.assign((const char*)s, bytes);
        return o;
    }
    o.reserve(bytes);
    if (char_size == 2) {
        for (size_t i = 0; i + 1 < bytes; i += 2) {
            uint16_t c = (uint16_t)s[i] | ((uint16_t)s[i + 1] << 8);
            if (c < 0x80) {
                o += (char)c;
            } else if (c < 0x800) {
                o += (char)(0xC0 | (c >> 6));
                o += (char)(0x80 | (c & 0x3F));
            } else {
                o += (char)(0xE0 | (c >> 12));
                o += (char)(0x80 | ((c >> 6) & 0x3F));
                o += (char)(0x80 | (c & 0x3F));
            }
        }
    } else if (char_size == 4) {
        for (size_t i = 0; i + 3 < bytes; i += 4) {
            uint32_t c = (uint32_t)s[i] | ((uint32_t)s[i + 1] << 8) |
                         ((uint32_t)s[i + 2] << 16) | ((uint32_t)s[i + 3] << 24);
            if (c < 0x80) {
                o += (char)c;
            } else if (c < 0x800) {
                o += (char)(0xC0 | (c >> 6));
                o += (char)(0x80 | (c & 0x3F));
            } else if (c < 0x10000) {
                o += (char)(0xE0 | (c >> 12));
                o += (char)(0x80 | ((c >> 6) & 0x3F));
                o += (char)(0x80 | (c & 0x3F));
            } else {
                o += (char)(0xF0 | (c >> 18));
                o += (char)(0x80 | ((c >> 12) & 0x3F));
                o += (char)(0x80 | ((c >> 6) & 0x3F));
                o += (char)(0x80 | (c & 0x3F));
            }
        }
    }
    return o;
}

std::string fmt_double(double d) {
    char b[32];
    snprintf(b, sizeof(b), "%.17g", d);
    return b;
}

// Decodes dict+doc into a tiny DOM. Returns false on corrupt input.
// Budgets: 200 k nodes / depth 256 — a crafted project with millions of
// nested tags must fail cheap, never exhaust RAM (zip-bomb class).
bool decode_binx(const std::vector<unsigned char>& blob, XNode& root) {
    BxCursor c{blob.data(), blob.size()};
    int char_size = 0;
    std::map<uint16_t, std::string> dict;
    std::vector<std::map<uint16_t, std::string>> stack;

    // Open-tag stack: each entry is the node being built + child index path.
    struct Open {
        XNode node;
    };
    std::vector<Open> open;
    bool have_root = false;
    size_t node_count = 0;
    constexpr size_t kMaxNodes = 200000;
    constexpr size_t kMaxDepth = 256;

    auto read_string = [&](int32_t len) -> std::string {
        if (len < 0 || (size_t)len > 256 * 1024 * 1024 || !c.need((size_t)len))
            return "";
        std::string s = to_utf8(c.p + c.pos, (size_t)len, char_size);
        c.pos += (size_t)len;
        return s;
    };

    while (c.pos < c.len && !c.err) {
        unsigned char ft = c.u8();
        if (c.err) break;
        switch ((BxField)ft) {
            case BX_CHAR_SIZE:
                char_size = c.u8();
                if (char_size != 1 && char_size != 2 && char_size != 4) return false;
                break;
            case BX_NAME: {
                uint16_t id = c.u16();
                uint16_t len = c.u16();
                if (c.err || (size_t)len * (size_t)(char_size ? char_size : 1) > blob.size())
                    return false;
                size_t bytes = (size_t)len;
                if (!c.need(bytes)) return false;
                dict[id] = to_utf8(c.p + c.pos, bytes, char_size ? char_size : 1);
                c.pos += bytes;
                break;
            }
            case BX_PUSH:
                stack.push_back(dict);
                dict.clear();
                break;
            case BX_POP:
                if (stack.empty()) return false;
                dict = stack.back();
                stack.pop_back();
                break;
            case BX_START_TAG: {
                uint16_t id = c.u16();
                if (c.err) return false;
                auto it = dict.find(id);
                if (it == dict.end()) return false;
                if (++node_count > kMaxNodes || open.size() >= kMaxDepth) return false;
                Open o;
                o.node.tag = it->second;
                open.push_back(std::move(o));
                break;
            }
            case BX_END_TAG: {
                uint16_t id = c.u16();
                if (c.err) return false;
                auto it = dict.find(id);
                if (it == dict.end() || open.empty()) return false;
                // (Tag-name match not enforced: encoder always balances.)
                XNode done = std::move(open.back().node);
                open.pop_back();
                if (open.empty()) {
                    if (have_root) return false;
                    root = std::move(done);
                    have_root = true;
                } else {
                    open.back().node.children.push_back(std::move(done));
                }
                break;
            }
            case BX_STRING: {
                uint16_t id = c.u16();
                int32_t len = c.i32();
                if (c.err) return false;
                auto it = dict.find(id);
                if (it == dict.end() || open.empty()) return false;
                open.back().node.attrs.push_back({it->second, read_string(len)});
                break;
            }
            case BX_INT: {
                uint16_t id = c.u16();
                int32_t v = c.i32();
                if (c.err) return false;
                auto it = dict.find(id);
                if (it == dict.end() || open.empty()) return false;
                open.back().node.attrs.push_back({it->second, std::to_string(v)});
                break;
            }
            case BX_BOOL: {
                uint16_t id = c.u16();
                unsigned char v = c.u8();
                if (c.err) return false;
                auto it = dict.find(id);
                if (it == dict.end() || open.empty()) return false;
                open.back().node.attrs.push_back({it->second, v ? "1" : "0"});
                break;
            }
            case BX_LONG: {
                uint16_t id = c.u16();
                int32_t v = c.i32();
                if (c.err) return false;
                auto it = dict.find(id);
                if (it == dict.end() || open.empty()) return false;
                open.back().node.attrs.push_back({it->second, std::to_string(v)});
                break;
            }
            case BX_LONGLONG: {
                uint16_t id = c.u16();
                int64_t v = c.i64();
                if (c.err) return false;
                auto it = dict.find(id);
                if (it == dict.end() || open.empty()) return false;
                open.back().node.attrs.push_back({it->second, std::to_string(v)});
                break;
            }
            case BX_SIZET: {
                uint16_t id = c.u16();
                uint32_t v = c.u32();
                if (c.err) return false;
                auto it = dict.find(id);
                if (it == dict.end() || open.empty()) return false;
                open.back().node.attrs.push_back({it->second, std::to_string(v)});
                break;
            }
            case BX_FLOAT: {
                uint16_t id = c.u16();
                float v = c.f32();
                int32_t digits = c.i32();
                (void)digits;
                if (c.err) return false;
                auto it = dict.find(id);
                if (it == dict.end() || open.empty()) return false;
                open.back().node.attrs.push_back({it->second, fmt_double(v)});
                break;
            }
            case BX_DOUBLE: {
                uint16_t id = c.u16();
                double v = c.f64();
                int32_t digits = c.i32();
                (void)digits;
                if (c.err) return false;
                auto it = dict.find(id);
                if (it == dict.end() || open.empty()) return false;
                open.back().node.attrs.push_back({it->second, fmt_double(v)});
                break;
            }
            case BX_DATA:
            case BX_RAW: {
                int32_t len = c.i32();
                if (c.err || len < 0 || !c.need((size_t)len)) return false;
                c.pos += (size_t)len;  // boilerplate / text runs: ignored
                break;
            }
            default:
                return false;
        }
    }
    return !c.err && have_root && open.empty();
}

// ---- Project model (transcription-relevant subset). ----

struct ClipBlock {
    long long start = 0;  // sequence samples, includes trimmed region
    long long blockid = 0;
    long long length = -1;  // AUP4 explicit length; -1 = derive from tiling
};

struct Clip {
    double offset = 0.0;  // seconds on the track timeline
    double trim_left = 0.0;
    double trim_right = 0.0;
    long long numsamples = 0;  // full sequence length incl. hidden audio
    std::vector<ClipBlock> blocks;
    std::vector<std::pair<double, double>> envelope;  // (t seconds, val)
};

struct Track {
    int channel = 0;
    bool mute = false;
    bool solo = false;
    double rate = -1.0;  // <=0 = project rate
    double gain = 1.0;
    double pan = 0.0;
    std::vector<Clip> clips;
};

double envelope_at(const std::vector<std::pair<double, double>>& pts, double t) {
    if (pts.empty()) return 1.0;
    if (t <= pts.front().first) return pts.front().second;
    for (size_t i = 1; i < pts.size(); ++i) {
        if (t <= pts[i].first) {
            double t0 = pts[i - 1].first, v0 = pts[i - 1].second;
            double t1 = pts[i].first, v1 = pts[i].second;
            if (t1 <= t0) return v1;
            double k = (t - t0) / (t1 - t0);
            return v0 + k * (v1 - v0);
        }
    }
    return pts.back().second;
}

void collect_envelope(const XNode& n, std::vector<std::pair<double, double>>& pts) {
    if (n.tag == "envelope") {
        for (const auto& ch : n.children) {
            if (ch.tag == "controlpoint")
                pts.push_back({xdouble(ch, "t", 0.0), xdouble(ch, "val", 1.0)});
        }
        return;
    }
    for (const auto& ch : n.children) collect_envelope(ch, pts);
}

bool parse_project(const XNode& root, double& proj_rate, std::vector<Track>& tracks) {
    if (root.tag != "project") return false;
    proj_rate = xdouble(root, "rate", 44100.0);
    if (!(proj_rate > 0) || proj_rate > 768000.0) proj_rate = 44100.0;
    for (const auto& tn : root.children) {
        if (tn.tag != "wavetrack") continue;  // labels/time tracks ignored
        Track tr;
        tr.channel = (int)xint64(tn, "channel", 0);
        tr.mute = xint64(tn, "mute", 0) != 0;
        tr.solo = xint64(tn, "solo", 0) != 0;
        tr.rate = xdouble(tn, "rate", -1.0);
        tr.gain = xdouble(tn, "gain", 1.0);
        tr.pan = xdouble(tn, "pan", 0.0);
        if (!(tr.gain >= 0) || tr.gain > 100.0) tr.gain = 1.0;
        if (!(tr.pan >= -1.0 && tr.pan <= 1.0)) tr.pan = 0.0;
        for (const auto& cn : tn.children) {
            if (cn.tag != "waveclip") continue;
            Clip cl;
            cl.offset = xdouble(cn, "offset", 0.0);
            cl.trim_left = xdouble(cn, "trimLeft", 0.0);
            cl.trim_right = xdouble(cn, "trimRight", 0.0);
            if (!(cl.offset >= 0)) cl.offset = 0;
            if (!(cl.trim_left >= 0)) cl.trim_left = 0;
            if (!(cl.trim_right >= 0)) cl.trim_right = 0;
            for (const auto& sn : cn.children) {
                if (sn.tag == "sequence") {
                    cl.numsamples = xint64(sn, "numsamples", 0);
                    for (const auto& bn : sn.children) {
                        if (bn.tag != "waveblock") continue;
                        ClipBlock b;
                        b.start = xint64(bn, "start", 0);
                        b.blockid = xint64(bn, "blockid", 0);
                        b.length = xint64(bn, "length", -1);  // AUP4 explicit
                        if (b.start >= 0 && b.blockid > 0) cl.blocks.push_back(b);
                    }
                } else if (sn.tag == "envelope") {
                    collect_envelope(sn, cl.envelope);
                }
            }
            if (cl.numsamples > 0 && !cl.blocks.empty()) tr.clips.push_back(std::move(cl));
        }
        tracks.push_back(std::move(tr));
    }
    // NOTE: no channel sort — the mixdown is a commutative sum, so track
    // order has no audible effect. (Channel is parsed for format fidelity.)
    return true;
}

#ifdef PV_HAVE_SQLITE
// ---- sampleblocks access (read-only handle, blocks cached small). ----

struct BlockCache {
    sqlite3* db = nullptr;
    std::map<long long, std::vector<float>> floats;
    size_t bytes = 0;
    int missing = 0;
    int bad_format = 0;

    bool fetch(long long blockid, const std::vector<float>*& out) {
        auto it = floats.find(blockid);
        if (it != floats.end()) {
            out = &it->second;
            return true;
        }
        sqlite3_stmt* st = nullptr;
        if (sqlite3_prepare_v2(db, "SELECT sampleformat, samples FROM sampleblocks WHERE blockid=?1",
                               -1, &st, nullptr) != SQLITE_OK)
            return false;
        sqlite3_bind_int64(st, 1, blockid);
        bool ok = false;
        if (sqlite3_step(st) == SQLITE_ROW) {
            uint32_t fmt = (uint32_t)sqlite3_column_int64(st, 0);
            const void* blob = sqlite3_column_blob(st, 1);
            int nbytes = sqlite3_column_bytes(st, 1);
            if (blob && nbytes > 0 && nbytes <= 64 * 1024 * 1024) {
                std::vector<float> v;
                const unsigned char* d = (const unsigned char*)blob;
                if (fmt == SF_FLOAT && nbytes % 4 == 0) {
                    v.resize((size_t)nbytes / 4);
                    memcpy(v.data(), d, (size_t)nbytes);
                    ok = true;
                } else if (fmt == SF_INT16 && nbytes % 2 == 0) {
                    v.resize((size_t)nbytes / 2);
                    for (size_t i = 0; i < v.size(); ++i) {
                        int s = (int)(int16_t)(d[2 * i] | (d[2 * i + 1] << 8));
                        v[i] = s / 32768.0f;
                    }
                    ok = true;
                } else if (fmt == SF_INT24 && nbytes % 3 == 0) {
                    v.resize((size_t)nbytes / 3);
                    for (size_t i = 0; i < v.size(); ++i) {
                        int s = (int)(d[3 * i] | (d[3 * i + 1] << 8) | (d[3 * i + 2] << 16));
                        if (s & 0x800000) s |= ~0xffffff;
                        v[i] = s / 8388608.0f;
                    }
                    ok = true;
                } else {
                    ++bad_format;
                }
                if (ok) {
                    if (bytes + (size_t)nbytes > 32 * 1024 * 1024) {
                        floats.clear();  // cap: drop the cache, keep rolling
                        bytes = 0;
                    }
                    bytes += (size_t)nbytes;
                    out = &floats.emplace(blockid, std::move(v)).first->second;
                }
            }
        }
        sqlite3_finalize(st);
        if (!ok) ++missing;
        return ok;
    }
};

bool read_blob(sqlite3* db, const char* table, const char* col, std::vector<unsigned char>& out) {
    std::string sql = "SELECT ";
    sql += col;
    sql += " FROM ";
    sql += table;
    sql += " WHERE id=1";
    sqlite3_stmt* st = nullptr;
    if (sqlite3_prepare_v2(db, sql.c_str(), -1, &st, nullptr) != SQLITE_OK) return false;
    bool ok = false;
    if (sqlite3_step(st) == SQLITE_ROW) {
        const void* b = sqlite3_column_blob(st, 0);
        int n = sqlite3_column_bytes(st, 0);
        if (b && n > 0 && n <= 512 * 1024 * 1024) {
            out.assign((const unsigned char*)b, (const unsigned char*)b + n);
            ok = true;
        }
    }
    sqlite3_finalize(st);
    return ok;
}
#endif  // PV_HAVE_SQLITE

}  // namespace

bool is_audacity_project(const std::string& path) {
    auto dot = path.rfind('.');
    if (dot == std::string::npos) return false;
    std::string ext = path.substr(dot + 1);
    for (char& c : ext) c = (char)tolower((unsigned char)c);
    if (ext != "aup3" && ext != "aup4" && ext != "aup3unsaved") return false;
    // SQLite magic + 'AUDY' application_id (offset 68, big-endian).
    FILE* f = nullptr;
    if (fopen_s(&f, path.c_str(), "rb") != 0 || !f) return false;
    unsigned char h[72] = {};
    bool ok = fread(h, 1, sizeof(h), f) == sizeof(h) &&
              memcmp(h, "SQLite format 3\0", 16) == 0 && h[68] == 'A' && h[69] == 'U' &&
              h[70] == 'D' && h[71] == 'Y';
    fclose(f);
    return ok;
}

bool render_audacity_to_wav(const std::string& src, const std::string& dst, std::string& err) {
#ifdef PV_HAVE_SQLITE
    if (!is_audacity_project(src)) {
        err = "not an Audacity project file";
        return false;
    }
    pv::progress_hook(PV_DECODE, 0.02f, "Reading Audacity project…");
    sqlite3* db = nullptr;
    // Read-only in place: never locks, never touches -wal/-shm companions.
    if (sqlite3_open_v2(src.c_str(), &db, SQLITE_OPEN_READONLY, nullptr) != SQLITE_OK) {
        err = "cannot open project database";
        if (db) sqlite3_close(db);
        return false;
    }
    sqlite3_exec(db, "PRAGMA query_only=ON;", nullptr, nullptr, nullptr);

    // Layout gate: the three tables we read must exist. A future Audacity
    // that reorganizes storage fails here with a clean error — never silent
    // wrong-audio. user_version is logged for forensics (accepted broadly:
    // AUP3 3.x and AUP4 share this layout).
    {
        uint32_t uver = 0;
        sqlite3_stmt* vs = nullptr;
        if (sqlite3_prepare_v2(db, "PRAGMA user_version;", -1, &vs, nullptr) == SQLITE_OK) {
            if (sqlite3_step(vs) == SQLITE_ROW) uver = (uint32_t)sqlite3_column_int64(vs, 0);
            sqlite3_finalize(vs);
        }
        const char* need[] = {"project", "autosave", "sampleblocks"};
        for (const char* t : need) {
            std::string q = "SELECT 1 FROM sqlite_master WHERE type='table' AND name='";
            q += t;
            q += "' LIMIT 1";
            sqlite3_stmt* st = nullptr;
            bool yes = false;
            if (sqlite3_prepare_v2(db, q.c_str(), -1, &st, nullptr) == SQLITE_OK) {
                yes = sqlite3_step(st) == SQLITE_ROW;
                sqlite3_finalize(st);
            }
            if (!yes) {
                char lb[160];
                snprintf(lb, sizeof(lb),
                         "unsupported Audacity project layout (missing %s table, user_version %u) — "
                         "this project may be newer than the app supports",
                         t, uver);
                err = lb;
                sqlite3_close(db);
                return false;
            }
        }
        char lb[96];
        snprintf(lb, sizeof(lb), "audacity: user_version %u", uver);
        pv::pv_log(lb);
    }

    // Prefer the saved project doc; fall back to the autosave (crash recovery).
    std::vector<unsigned char> dict, doc;
    bool have_project = read_blob(db, "project", "dict", dict) && read_blob(db, "project", "doc", doc) &&
                        !doc.empty();
    const char* which = "project";
    if (!have_project) {
        dict.clear();
        doc.clear();
        if (!(read_blob(db, "autosave", "dict", dict) && read_blob(db, "autosave", "doc", doc) &&
              !doc.empty())) {
            err = "project has no saved audio (empty project/autosave)";
            sqlite3_close(db);
            return false;
        }
        which = "autosave";
    }
    {
        char lb[128];
        snprintf(lb, sizeof(lb), "audacity: using %s doc (%u dict + %u doc bytes)", which,
                 (unsigned)dict.size(), (unsigned)doc.size());
        pv::pv_log(lb);
    }
    std::vector<unsigned char> blob;
    blob.reserve(dict.size() + doc.size());
    blob.insert(blob.end(), dict.begin(), dict.end());
    blob.insert(blob.end(), doc.begin(), doc.end());

    XNode root;
    if (!decode_binx(blob, root)) {
        err = "cannot decode project document (unsupported Audacity version?)";
        sqlite3_close(db);
        return false;
    }
    double proj_rate = 44100.0;
    std::vector<Track> tracks;
    if (!parse_project(root, proj_rate, tracks) || tracks.empty()) {
        err = "project has no audio tracks";
        sqlite3_close(db);
        return false;
    }

    // Solo semantics: any solo (and not muted) track mutes the rest.
    bool any_solo = false;
    for (const auto& t : tracks)
        if (t.solo && !t.mute) any_solo = true;

    // Timeline length in mix samples.
    const double mix_rate = proj_rate;
    long long total = 0;
    struct Audible {
        const Track* tr;
        const Clip* cl;
        double rate;
        long long start;  // mix samples
        long long len;    // mix samples of audible region
        long long a0;     // sequence-sample offset of audible start
    };
    std::vector<Audible> aud;
    for (const auto& t : tracks) {
        if (t.mute) continue;
        if (any_solo && !t.solo) continue;
        double rate = t.rate > 0 ? t.rate : proj_rate;
        if (!(rate > 0) || rate > 768000.0) continue;
        for (const auto& cl : t.clips) {
            long long skip = (long long)llround(cl.trim_left * rate);
            long long keep = cl.numsamples - (long long)llround(cl.trim_right * rate) - skip;
            if (keep <= 0) continue;
            long long start = (long long)llround(cl.offset * mix_rate);
            // Resample span: clip seconds at mix rate.
            long long len = (long long)llround((keep / rate) * mix_rate);
            if (len <= 0) continue;
            aud.push_back({&t, &cl, rate, start, len, skip});
            if (start + len > total) total = start + len;
        }
    }
    if (aud.empty() || total <= 0) {
        err = "project has no audible audio (all tracks muted?)";
        sqlite3_close(db);
        return false;
    }
    const long long kMaxSamples = (long long)(5 * 3600) * (long long)mix_rate;  // 5 h cap
    // Standard WAV caps data at u32 bytes (float32 mono): refuse rather than wrap.
    const long long kMaxBytes = ((long long)0xFFFFFFFF - 44) / 4;
    if (total > kMaxSamples || total > kMaxBytes) {
        err = "project too long — export stems from Audacity instead";
        sqlite3_close(db);
        return false;
    }

    // Chunked mixdown (1 M mix samples per chunk): flat RAM at any length.
    BlockCache cache{db};
    std::vector<float> mix;
    mix.reserve(1 << 20);
    FILE* out = nullptr;
    if (fopen_s(&out, dst.c_str(), "wb") != 0 || !out) {
        err = "cannot write rendered WAV";
        sqlite3_close(db);
        return false;
    }
    // WAV header placeholder (patched at the end).
    unsigned char hdr[44] = {};
    fwrite(hdr, 1, 44, out);
    long long written = 0;
    float peak = 0.0f;
    const double kPi = 3.141592653589793;
    long long done = 0;
    bool fail = false;

    // Single render pass tracking peak; when the mix clips, a second
    // IN-PLACE pass multiplies the file (no re-decode, no re-stitch —
    // sequential file IO only, far cheaper than a second render).
    {
        done = 0;
        // Per-clip cursor state (deterministic single walk).
        struct Cur {
            size_t block_idx = 0;
        };
        std::vector<Cur> curs(aud.size());
        // Block sample cursor: (block vector, position) cached across chunks.
        struct BCur {
            const std::vector<float>* v = nullptr;
            long long base = 0;  // sequence-sample index of v[0]
            long long blockid = 0;
            bool silent = true;
        };
        std::vector<BCur> bcurs(aud.size());
        while (done < total && !fail) {
            long long want = total - done;
            if (want > (long long)(1 << 20)) want = (1 << 20);
            mix.assign((size_t)want, 0.0f);
            for (size_t k = 0; k < aud.size(); ++k) {
                const Audible& a = aud[k];
                if (done + want <= a.start || done >= a.start + a.len) continue;
                long long s0 = done > a.start ? done - a.start : 0;
                long long s1 = done + want < a.start + a.len ? done + want - a.start
                                                             : a.start + a.len - a.start;
                const Track* t = a.tr;
                // Equal-power mono fold, unity at center (divide by √2 so a
                // center-panned track keeps its level; hard-panned content
                // lands -3 dB, preserving power). Global peak-normalize
                // afterwards only if the mix clips.
                double ang = (t->pan + 1.0) * kPi / 4.0;
                float g = (float)(t->gain * (cos(ang) + sin(ang)) * 0.7071067811865475);
                for (long long s = s0; s < s1; ++s) {
                    // Clip-local time (seconds, for envelope).
                    double ct = (double)s / mix_rate;
                    float env = (float)envelope_at(a.cl->envelope, ct);
                    // Sequence position: audible offset + resampled index.
                    double seq_pos = (double)a.a0 + (double)s * (a.rate / mix_rate);
                    long long si = (long long)seq_pos;
                    double frac = seq_pos - si;
                    // Locate the waveblock tiling [0, numsamples).
                    const auto& blocks = a.cl->blocks;
                    while (curs[k].block_idx + 1 < blocks.size()) {
                        const ClipBlock& nb = blocks[curs[k].block_idx + 1];
                        if (si >= nb.start) {
                            ++curs[k].block_idx;
                            bcurs[k].v = nullptr;
                        } else {
                            break;
                        }
                    }
                    if (curs[k].block_idx >= blocks.size()) continue;
                    const ClipBlock& b = blocks[curs[k].block_idx];
                    long long blen;
                    // AUP4 writes an explicit length: trust but verify — it
                    // must fit the tiling, else a lying value would read past
                    // the block (the sampler below clamps anyway; belt first).
                    if (b.length > 0 && b.length <= a.cl->numsamples - b.start) {
                        blen = b.length;
                    } else if (curs[k].block_idx + 1 < blocks.size()) {
                        blen = blocks[curs[k].block_idx + 1].start - b.start;
                    } else {
                        blen = a.cl->numsamples - b.start;
                    }
                    if (blen <= 0) continue;
                    if (si < b.start || si >= b.start + blen) continue;
                    BCur& bc = bcurs[k];
                    if (!bc.v || bc.blockid != b.blockid) {
                        const std::vector<float>* v = nullptr;
                        if (cache.fetch(b.blockid, v) && v && !v->empty()) {
                            bc.v = v;
                            bc.base = b.start;
                            bc.blockid = b.blockid;
                            bc.silent = false;
                        } else {
                            bc.v = nullptr;
                            bc.silent = true;
                            bc.blockid = b.blockid;
                        }
                    }
                    float sample = 0.0f;
                    if (!bc.silent && bc.v) {
                        long long i0 = si - bc.base;
                        // Linear interpolation between block samples.
                        auto at = [&](long long i) -> float {
                            if (i < 0 || i >= (long long)bc.v->size()) return 0.0f;
                            return (*bc.v)[(size_t)i];
                        };
                        sample = (float)(at(i0) * (1.0 - frac) + at(i0 + 1) * frac);
                    }
                    float& m = mix[(size_t)(a.start + s - done)];
                    m += sample * g * env;
                }
            }
            for (float v : mix) {
                float a = v < 0 ? -v : v;
                if (a > peak) peak = a;
            }
            if (fwrite(mix.data(), 4, mix.size(), out) != mix.size()) {
                fail = true;
                break;
            }
            written += (long long)mix.size();
            done += want;
            float f = 0.05f + 0.85f * (float)((double)done / (double)total);
            if (f > 1.0f) f = 1.0f;
            pv::progress_hook(PV_DECODE, f, "Mixing Audacity project…");
            if (!pv::checkpoint()) {
                err = "aborted";
                fclose(out);
                std::remove(dst.c_str());
                sqlite3_close(db);
                return false;
            }
        }
    }
    // In-place normalize (only when the render clipped): rewrite the data
    // chunk scaled to 0.99 peak. No second render pass.
    if (!fail && peak > 1.0f) {
        const float norm = 0.99f / peak;
        _fseeki64(out, 44, SEEK_SET);
        std::vector<float> amp(1 << 20);
        long long left = written;
        while (left > 0 && !fail) {
            size_t want = left > (long long)amp.size() ? amp.size() : (size_t)left;
            if (fread(amp.data(), 4, want, out) != want) {
                fail = true;
                break;
            }
            for (size_t i = 0; i < want; ++i) amp[i] *= norm;
            _fseeki64(out, -(__int64)(want * 4), SEEK_CUR);
            if (fwrite(amp.data(), 4, want, out) != want) {
                fail = true;
                break;
            }
            left -= (long long)want;
        }
    }
    // Patch the WAV header (float32 mono).
    if (!fail) {
        uint32_t u_rate = (uint32_t)mix_rate;
        uint32_t data_bytes = (uint32_t)(written * 4);
        _fseeki64(out, 0, SEEK_SET);
        unsigned char h[44] = {};
        memcpy(h, "RIFF", 4);
        uint32_t riff = data_bytes + 36;
        memcpy(h + 4, &riff, 4);
        memcpy(h + 8, "WAVEfmt ", 8);
        uint32_t sixteen = 16;
        memcpy(h + 16, &sixteen, 4);
        uint16_t three = 3, one = 1, thirtytwo = 32;
        memcpy(h + 20, &three, 2);
        memcpy(h + 22, &one, 2);
        memcpy(h + 24, &u_rate, 4);
        uint32_t br = u_rate * 4;
        memcpy(h + 28, &br, 4);
        uint16_t ba = 4;
        memcpy(h + 32, &ba, 2);
        memcpy(h + 34, &thirtytwo, 2);
        memcpy(h + 36, "data", 4);
        memcpy(h + 40, &data_bytes, 4);
        fail = fwrite(h, 1, 44, out) != 44;
    }
    fclose(out);
    char lb2[192];
    snprintf(lb2, sizeof(lb2),
             "audacity: rendered %lld samples @%uHz (%d missing blocks, %d bad formats)%s",
             written, (unsigned)mix_rate, cache.missing, cache.bad_format,
             peak > 1.0f ? " [peak-normalized]" : "");
    pv::pv_log(lb2);
    sqlite3_close(db);
    if (fail) {
        std::remove(dst.c_str());
        if (err.empty()) err = "failed writing rendered WAV";
        return false;
    }
    return true;
#else
    (void)src;
    (void)dst;
    err = "SQLite support not built (run scripts/setup-windows.ps1)";
    return false;
#endif
}

}  // namespace pv
