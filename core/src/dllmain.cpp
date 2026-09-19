#include "../include/present_core.h"
#include "internal.h"
// NOTE: WIN32_LEAN_AND_MEAN comes from CMake compile definitions; do not
// #define it here (MSVC C4005).
#include <windows.h>

#include <csignal>
#include <cstdio>
#include <cstdlib>
#include <string>

namespace {

// Last-resort reporter for crash classes the SEH filter and Rust panic hook
// can never see: CRT invalid-parameter, purecall, and abort(). Best-effort
// only — runs on the dying thread, never returns to the caller.
void pv_fail(const char* kind) {
    pv::pv_log(std::string("FATAL ") + kind);
    std::string dir = pv::pv_data_dir();
    std::string msg =
        std::string("voice mail backend stopped unexpectedly (") + kind +
        ").\n\nBackend diagnostics (including the abort reason, if the\nbackend printed one) are in:\n" +
        dir + "\\backend.log\n" + dir + "\\stderr.log";
    // Narrow->wide by widening: crash text is ASCII by construction.
    std::wstring w(msg.begin(), msg.end());
    MessageBoxW(nullptr, w.c_str(), L"voice mail backend",
                MB_OK | MB_ICONERROR | MB_TOPMOST);
    ExitProcess(3);
}

void __cdecl pv_invalid_param(const wchar_t*, const wchar_t*, const wchar_t*, unsigned int,
                              uintptr_t) {
    pv_fail("invalid-parameter");
}

void __cdecl pv_purecall() { pv_fail("purecall"); }

void __cdecl pv_sigabrt(int) { pv_fail("abort"); }

}  // namespace

BOOL APIENTRY DllMain(HMODULE, DWORD reason, LPVOID) {
    if (reason == DLL_PROCESS_ATTACH) {
        // Process-wide UCRT state (shared CRT): covers every module, not
        // just this DLL. Only sets globals — safe under the loader lock.
        // NOTE: /GS fail-fast and GPU TDR kills still bypass everything;
        // those are caught by WER LocalDumps / ProcDump, not here.
        _set_invalid_parameter_handler(pv_invalid_param);
        _set_purecall_handler(pv_purecall);
        std::signal(SIGABRT, pv_sigabrt);
    }
    return TRUE;
}
