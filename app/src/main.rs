#![windows_subsystem = "windows"]
// Present Voice — native GPUI entry point.
// Boot mirrors the gpui-kit hello_world exactly: facade application() +
// init() before any component use, window opened from a spawned task, view
// wrapped in the kit Root. M2: nav actions + keymap. Action listeners that
// need page views (pause/abort/continue et al.) land with their pages in M3/M4.

mod store;
mod theme;
mod views;
mod views_input;
mod views_models;
mod views_output;
mod views_record;
mod views_settings;
mod views_wizard;

use gpui_kit::component::Root;
use gpui_kit::prelude::*;
use gpui_kit::{
    actions, application, init, px, size, Bounds, KeyBinding, Point, TitlebarOptions, WindowBounds,
    WindowOptions,
};

use crate::store::Store;
use crate::views::RootView;
use crate::views_input::InputView;
use crate::views_models::ModelsView;
use crate::views_output::OutputView;
use crate::views_record::RecordView;
use crate::views_settings::SettingsView;
use crate::views_wizard::WizardView;

actions!(
    app,
    [
        Quit,
        GotoInput,
        GotoOutput,
        PauseProc,
        ToggleDarkMode
    ]
);

// A GUI-subsystem exe has no console, so a panic would vanish silently.
// Every panic is appended to `<data>/crash.log` (with a backtrace) AND
// shown in a native popup, since the GPUI event loop may already be dead
// when the hook runs — Win32 MessageBoxW works from any thread with no
// framework involved. Dev-terminal runs still print via the prior hook.
// Raw Win32 import (user32 is always present): no new crates for one dialog.
#[link(name = "user32")]
extern "system" {
    fn MessageBoxW(
        h_wnd: *mut std::ffi::c_void,
        lp_text: *const u16,
        lp_caption: *const u16,
        u_type: u32,
    ) -> i32;
}

const MB_OK: u32 = 0x0;
const MB_ICONERROR: u32 = 0x10;
const MB_TOPMOST: u32 = 0x40000;

/// Popup + log body for a panic. Pure (unit-tested); the hook only does
/// I/O around it. Hostile inputs are truncated so the dialog stays readable.
fn crash_report_text(info: &str, log_path: &str) -> String {
    const MAX_INFO: usize = 600;
    let mut short = info.chars().take(MAX_INFO + 1).collect::<String>();
    if short.chars().count() > MAX_INFO {
        short.truncate(short.char_indices().nth(MAX_INFO).map(|(i, _)| i).unwrap_or(MAX_INFO));
        short.push('…');
    }
    format!(
        "Present Voice stopped unexpectedly.\n\n{short}\n\nDetails (including a technical backtrace) were saved to:\n{log_path}\n\nTip: press Ctrl+C in this dialog to copy this message."
    )
}

fn show_crash_popup(text: &str) {
    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }
    let text = wide(text);
    let caption = wide("Present Voice has stopped working");
    // Best-effort by construction: return value ignored, never panics.
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            text.as_ptr(),
            caption.as_ptr(),
            MB_OK | MB_ICONERROR | MB_TOPMOST,
        );
    }
}

/// Bound crash.log like the backend logs (5MB + one `.1` generation).
/// Without this the panic hook + SEH filter append forever on crashy
/// machines. Best-effort: failures fall through to plain append.
fn rotate_crash_log_if_huge(log: &std::path::Path) {
    const LIMIT: u64 = 5 * 1024 * 1024;
    if std::fs::metadata(log).map(|m| m.len()).unwrap_or(0) < LIMIT {
        return;
    }
    let prev = log.with_extension("log.1");
    let _ = std::fs::remove_file(&prev);
    let _ = std::fs::rename(log, &prev);
}

fn install_crash_log() {
    install_native_crash_handler();
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let info_str = format!("{info}");
        let dir = pv_backend::dirs::data_dir();
        let _ = std::fs::create_dir_all(&dir);
        let log = dir.join("crash.log");
        rotate_crash_log_if_huge(&log);
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log)
        {
            use std::io::Write;
            let _ = writeln!(f, "{:?} | panic: {info_str}", std::time::SystemTime::now());
            // Cap the backtrace: first 48 frames are the diagnosis, the rest is noise.
            let bt = format!("{}", std::backtrace::Backtrace::force_capture());
            for line in bt.lines().take(48) {
                let _ = writeln!(f, "  {line}");
            }
            let _ = writeln!(f, "---");
        }
        show_crash_popup(&crash_report_text(
            &info_str,
            &log.to_string_lossy(),
        ));
        prev(info);
    }));
}

// Native crashes (access violation, stack overflow, illegal instruction —
// the kind a GPU/STT backend dies with mid-transcription) never reach the
// Rust panic hook. This process-wide SEH filter catches them on ANY thread,
// writes a minidump + log line, and shows the same popup. Raw Win32 imports
// only (kernel32/dbghelp/user32 are always present): no new crates.
#[repr(C)]
struct SehRecord {
    code: u32,
    _flags: u32,
    _next: *mut SehRecord,
    address: *mut std::ffi::c_void,
    _nparams: u32,
    _info: [usize; 15],
}

#[repr(C)]
struct SehPointers {
    record: *mut SehRecord,
    _context: *mut std::ffi::c_void,
}

#[repr(C)]
struct MinidumpExceptionInfo {
    thread_id: u32,
    exception_pointers: *const SehPointers,
    client_pointers: i32,
}

#[link(name = "kernel32")]
extern "system" {
    fn SetUnhandledExceptionFilter(
        filter: Option<unsafe extern "system" fn(*mut SehPointers) -> i32>,
    ) -> Option<unsafe extern "system" fn(*mut SehPointers) -> i32>;
    fn GetCurrentThreadId() -> u32;
    fn GetLastError() -> u32;
}

#[link(name = "dbghelp")]
extern "system" {
    fn MiniDumpWriteDump(
        h_process: *mut std::ffi::c_void,
        process_id: u32,
        h_file: *mut std::ffi::c_void,
        dump_type: u32,
        exception_param: *const MinidumpExceptionInfo,
        user_stream_param: *const std::ffi::c_void,
        callback_param: *const std::ffi::c_void,
    ) -> i32;
}

const EXCEPTION_EXECUTE_HANDLER: i32 = 1;

/// Human names for the codes we can actually meet. Pure (unit-tested).
fn seh_code_name(code: u32) -> &'static str {
    match code {
        0xC0000005 => "access violation",
        0xC00000FD => "stack overflow",
        0xC0000094 => "integer divide-by-zero",
        0xC0000095 => "integer overflow",
        0xC000001D => "illegal instruction",
        0xC0000008 => "invalid handle",
        0xC0000409 => "stack buffer overrun (/GS)",
        0xC0000374 => "heap corruption",
        0xE06D7363 => "uncaught C++ exception",
        _ => "unknown native fault",
    }
}

unsafe extern "system" fn seh_filter(info: *mut SehPointers) -> i32 {
    // Best-effort only: every step is guarded, nothing here may panic.
    if !info.is_null() {
        let rec = (*info).record;
        if !rec.is_null() {
            let code = (*rec).code;
            let addr = (*rec).address as usize;
            let dir = pv_backend::dirs::data_dir();
            let _ = std::fs::create_dir_all(&dir);
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let dump_path = dir.join(format!("crash-{stamp}.dmp"));
            let log = dir.join("crash.log");
            rotate_crash_log_if_huge(&log);
            let mut dump_note = "minidump NOT written";
            let mut dump_winerr: u32 = 0;
            // Write the dump via a raw handle; Never a File across FFI.
            if let Ok(f) = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&dump_path)
            {
                use std::os::windows::io::IntoRawHandle;
                let h_file = f.into_raw_handle() as *mut std::ffi::c_void;
                let ex = MinidumpExceptionInfo {
                    thread_id: GetCurrentThreadId(),
                    exception_pointers: info as *const SehPointers,
                    client_pointers: 0,
                };
                // MiniDumpNormal (0) + pseudo process handle (-1): small file.
                let ok = MiniDumpWriteDump(
                    -1isize as *mut std::ffi::c_void,
                    std::process::id(),
                    h_file,
                    0,
                    &ex,
                    std::ptr::null(),
                    std::ptr::null(),
                );
                // The OS handle leaks here on purpose: the process is about
                // to terminate, and the dump is fully flushed on return.
                // A 0 result used to leave silent 0-byte .dmp files (seen
                // live); capture GetLastError so the crash.log line below
                // records the reason.
                if ok != 0 {
                    dump_note = "minidump written";
                } else {
                    dump_winerr = GetLastError();
                }
            }
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log)
            {
                use std::io::Write;
                let _ = writeln!(
                    f,
                    "{:?} | native crash 0x{code:08X} ({} @ 0x{addr:X}), {dump_note} (winerr={dump_winerr}): {}",
                    std::time::SystemTime::now(),
                    seh_code_name(code),
                    dump_path.display(),
                );
            }
            let text = format!(
                "Present Voice stopped unexpectedly.\n\nNative crash 0x{code:08X} ({}).\n\nDump + log saved to:\n{}\n\nTip: press Ctrl+C in this dialog to copy this message.",
                seh_code_name(code),
                dir.display(),
            );
            show_crash_popup(&text);
        }
    }
    EXCEPTION_EXECUTE_HANDLER
}

fn install_native_crash_handler() {
    unsafe {
        SetUnhandledExceptionFilter(Some(seh_filter));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crash_report_keeps_short_messages_intact() {
        let t = crash_report_text("boom", "C:\\log");
        assert!(t.contains("boom") && t.contains("C:\\log") && !t.contains('…'));
    }

    #[test]
    fn crash_report_truncates_hostile_inputs() {
        let big = "x".repeat(5000);
        let t = crash_report_text(&big, "C:\\log");
        assert!(t.contains('…'));
        assert!(t.len() < big.len());
    }

    #[test]
    fn seh_names_cover_the_usual_native_faults() {
        assert_eq!(seh_code_name(0xC0000005), "access violation");
        assert_eq!(seh_code_name(0xC00000FD), "stack overflow");
        assert_eq!(seh_code_name(0xE06D7363), "uncaught C++ exception");
        assert_eq!(seh_code_name(0xDEADBEEF), "unknown native fault");
    }
}

/// Store handle for the shutdown hook (set once the window/store exists).
/// Lets every quit path — X button included — stop the backend before exit.
static QUIT_STORE: std::sync::Mutex<Option<gpui_kit::Entity<Store>>> =
    std::sync::Mutex::new(None);

/// One-line shutdown tracer. The close-crash leaves NO entries in
/// crash.log/backend.log (teardown crashes bypass every hook), so this
/// dedicated file is the only witness: its last line tells us exactly how
/// far shutdown got (hook fired → backend aborted → exiting).
pub(crate) fn shutdown_trace(step: &str) {
    let p = pv_backend::dirs::data_dir().join("shutdown.log");
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&p)
    {
        use std::io::Write;
        let _ = writeln!(f, "{:?} | {step}", std::time::SystemTime::now());
    }
}

fn install_quit_hook(cx: &mut gpui_kit::App) {
    // Route ALL quits through abort + exit: framework teardown would drop
    // CoreLib (FreeLibrary) while the backend worker may still run inside
    // the DLL — the observed close-crash, which left no log entries because
    // destructors, not guarded code, were executing. State is already
    // persisted incrementally (prefs/drafts/outputs), so skipping teardown
    // loses nothing. Leaked intentionally: process-lifetime subscription.
    std::mem::forget(cx.on_app_quit(|cx| {
        shutdown_trace("quit hook fired");
        // Poison-safe: a poisoned store lock must still attempt the abort —
        // skipping it would reopen the FreeLibrary-while-worker-runs crash.
        let store_opt = QUIT_STORE
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(store) = store_opt {
            let _ = store.update(cx, |st, _| {
                st.shutdown_prepare();
            });
            shutdown_trace("drafts+prefs+downloads flushed");
        } else {
            shutdown_trace("no store published, skipping flush");
        }
        shutdown_trace("backend aborted, exiting");
        async {
            std::process::exit(0);
        }
    }));
}

fn main() {
    install_crash_log();
    application().run(move |cx| {
        // Required before any GPUI Component features are used.
        init(cx);
        install_quit_hook(cx);
        cx.bind_keys([
            KeyBinding::new("alt-q", Quit, None),
            KeyBinding::new("ctrl-1", GotoInput, None),
            KeyBinding::new("ctrl-3", GotoOutput, None),
            KeyBinding::new("ctrl-space", PauseProc, None),
            KeyBinding::new("ctrl-t", ToggleDarkMode, None),
        ]);
        cx.spawn(async move |cx| {
            let opts = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds {
                    origin: Point::default(),
                    size: size(px(1100.), px(750.)),
                })),
                // Title-bar collapse is a feature: the layout is built from
                // bounded flex slots + scroll containers, so near-zero sizes
                // never panic (scroll regions simply empty out).
                window_min_size: Some(size(px(240.), px(64.))),
                titlebar: Some(TitlebarOptions {
                    title: Some("Present Voice".into()),
                    ..Default::default()
                }),
                app_id: Some("com.presentvoice.app".to_string()),
                ..Default::default()
            };
            cx.open_window(opts, |window, cx| {
                let store = cx.new(|_| Store::new());
                // Publish for the shutdown hook (all quit paths stop here).
                if let Ok(mut guard) = QUIT_STORE.lock() {
                    *guard = Some(store.clone());
                }
                // X-button / Alt+F4 path: the App-level on_app_quit hook does
                // NOT fire for window chrome closes on this backend (observed
                // live: close-crash with zero shutdown.log lines), so the
                // window gets its own abort + exit. Same sequence as every
                // other quit path — never fall through to framework teardown
                // with a live backend worker inside the DLL.
                window.on_window_should_close(cx, |_, cx| {
                    crate::shutdown_trace("window close requested");
                    let store_opt = QUIT_STORE
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .clone();
                    if let Some(store) = store_opt {
                        let _ = store.update(cx, |st, _| {
                            st.shutdown_prepare();
                        });
                        crate::shutdown_trace("drafts+prefs+downloads flushed");
                    } else {
                        crate::shutdown_trace("no store published, skipping flush");
                    }
                    crate::shutdown_trace("backend aborted, exiting");
                    std::process::exit(0);
                });
                // Bridge the persisted compute mode into the backend loaders
                // before anything can load a model (constructors stay env-clean
                // so parallel tests never race on process-global state).
                store.update(cx, |s, _| s.apply_compute_env());
                let input = cx.new(|_| InputView::assemble(store.clone()));
                let record = cx.new(|cx| RecordView::assemble(window, cx, store.clone()));
                let output = OutputView::new(window, cx, store.clone());
                let models = ModelsView::new(cx, store.clone());
                let settings = SettingsView::new(&mut *window, cx, store.clone());
                let wizard = WizardView::new(cx, store.clone());
                // Boot inventory: refresh models, repair takes, decide wizard
                // visibility, then fire the background integrity worker over
                // its own pump (Start blocks only while it is in flight).
                let (btx, brx) = std::sync::mpsc::channel();
                store.update(cx, |s, _| {
                    s.refresh_models();
                    s.repair_stale_takes();
                    s.maybe_wizard();
                    s.spawn_boot_verify(btx);
                });
                crate::store::spawn_pump(store.clone(), brx, cx);
                let dark = store.read(cx).dark_mode;
                crate::theme::apply_theme(dark, cx);
                let view = cx
                    .new(|_| RootView::assemble(store, input, record, output, models, settings, wizard));
                cx.new(|cx| Root::new(view, window, cx))
            })
            .expect("Failed to open window");
        })
        .detach();
    });
}