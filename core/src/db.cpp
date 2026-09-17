// sqlite sidecar memory: remembers Day/Class per file path.
// Schema: files(path TEXT PRIMARY KEY, day TEXT, class TEXT).
// Builds only when thirdparty/sqlite/sqlite3.c is present; else no-ops.
#include "internal.h"

#ifdef PV_HAVE_SQLITE
#include "sqlite3.h"
#endif

namespace pv {

#ifdef PV_HAVE_SQLITE
namespace {
bool exec_simple(sqlite3* db, const char* sql) {
    char* msg = nullptr;
    int rc = sqlite3_exec(db, sql, nullptr, nullptr, &msg);
    if (msg) sqlite3_free(msg);
    return rc == SQLITE_OK;
}
}  // namespace
#endif

bool db_lookup(const std::string& db_path, const std::string& audio_path, std::string& day_label,
               std::string& class_label) {
#ifdef PV_HAVE_SQLITE
    if (db_path.empty() || audio_path.empty()) return false;
    sqlite3* db = nullptr;
    if (sqlite3_open(db_path.c_str(), &db) != SQLITE_OK) return false;
    // Defensive busy timeout: the GUI may one day resolve Day/Class while a
    // job holds the DB. Single-worker today, so this never fires — cheap.
    sqlite3_busy_timeout(db, 2000);
    exec_simple(db, "CREATE TABLE IF NOT EXISTS files(path TEXT PRIMARY KEY, day TEXT, class TEXT)");
    sqlite3_stmt* st = nullptr;
    bool found = false;
    if (sqlite3_prepare_v2(db, "SELECT day, class FROM files WHERE path=?1", -1, &st, nullptr) ==
        SQLITE_OK) {
        sqlite3_bind_text(st, 1, audio_path.c_str(), -1, SQLITE_TRANSIENT);
        if (sqlite3_step(st) == SQLITE_ROW) {
            const char* d = (const char*)sqlite3_column_text(st, 0);
            const char* c = (const char*)sqlite3_column_text(st, 1);
            if (d && *d) {
                day_label = d;
                found = true;
            }
            if (c && *c) {
                class_label = c;
                found = true;
            }
        }
        sqlite3_finalize(st);
    }
    sqlite3_close(db);
    return found;
#else
    (void)db_path; (void)audio_path; (void)day_label; (void)class_label;
    return false;
#endif
}

void db_remember(const std::string& db_path, const std::string& audio_path,
                 const std::string& day_label, const std::string& class_label) {
#ifdef PV_HAVE_SQLITE
    if (db_path.empty() || audio_path.empty()) return;
    sqlite3* db = nullptr;
    if (sqlite3_open(db_path.c_str(), &db) != SQLITE_OK) return;
    sqlite3_busy_timeout(db, 2000);  // see db_lookup above
    if (!exec_simple(db, "CREATE TABLE IF NOT EXISTS files(path TEXT PRIMARY KEY, day TEXT, class TEXT)")) {
        sqlite3_close(db);
        return;
    }
    sqlite3_stmt* st = nullptr;
    if (sqlite3_prepare_v2(db,
                           "INSERT OR REPLACE INTO files(path, day, class) VALUES(?1, ?2, ?3)", -1,
                           &st, nullptr) == SQLITE_OK) {
        sqlite3_bind_text(st, 1, audio_path.c_str(), -1, SQLITE_TRANSIENT);
        sqlite3_bind_text(st, 2, day_label.c_str(), -1, SQLITE_TRANSIENT);
        sqlite3_bind_text(st, 3, class_label.c_str(), -1, SQLITE_TRANSIENT);
        sqlite3_step(st);
        sqlite3_finalize(st);
    }
    sqlite3_close(db);
#else
    (void)db_path; (void)audio_path; (void)day_label; (void)class_label;
#endif
}

}  // namespace pv
