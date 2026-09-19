//! Desktop conveniences: reveal-in-folder + clipboard copy.
//!
//! Portable-shaped from day one: `reveal_in_folder` dispatches per OS
//! (Windows `explorer /select`, macOS `open -R`, Linux `xdg-open` on the
//! parent). Clipboard is Win32-native here (no new crates); other platforms
//! return an honest error until their backend lands.

use std::path::Path;

/// Reveal `path` in the OS file manager (selects the file). Errors when the
/// path does not exist or the launcher fails — callers surface it in status.
pub fn reveal_in_folder(path: &Path) -> Result<(), String> {
    if !path.exists() {
        return Err("file no longer on disk".to_string());
    }
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("explorer")
            .arg("/select,")
            .arg(path)
            .spawn()
            .map_err(|e| format!("cannot open Explorer: {e}"))?;
        return Ok(());
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg("-R")
            .arg(path)
            .spawn()
            .map_err(|e| format!("cannot open Finder: {e}"))?;
        return Ok(());
    }
    #[cfg(target_os = "linux")]
    {
        let parent = path.parent().ok_or_else(|| "no parent folder".to_string())?;
        std::process::Command::new("xdg-open")
            .arg(parent)
            .spawn()
            .map_err(|e| format!("cannot open file manager: {e}"))?;
        return Ok(());
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    {
        let _ = path;
        return Err("reveal not supported on this platform yet".to_string());
    }
}

#[cfg(target_os = "windows")]
#[link(name = "user32")]
extern "system" {
    fn OpenClipboard(h: *mut std::ffi::c_void) -> i32;
    fn EmptyClipboard() -> i32;
    fn SetClipboardData(fmt: u32, mem: *mut std::ffi::c_void) -> *mut std::ffi::c_void;
    fn CloseClipboard() -> i32;
}

#[cfg(target_os = "windows")]
#[link(name = "kernel32")]
extern "system" {
    fn GlobalAlloc(flags: u32, bytes: usize) -> *mut std::ffi::c_void;
    fn GlobalLock(mem: *mut std::ffi::c_void) -> *mut std::ffi::c_void;
    fn GlobalUnlock(mem: *mut std::ffi::c_void) -> i32;
}

/// Copy UTF-8 `text` to the system clipboard. Truncates past ~4 M chars
/// (clipboard owners should stay small; transcripts past that belong in the
/// file, and the Output row already links it).
pub fn copy_text(text: &str) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        const CF_UNICODETEXT: u32 = 13;
        const GMEM_MOVEABLE: u32 = 0x0002;
        let capped: String = text.chars().take(4_000_000).collect();
        let wide: Vec<u16> = capped.encode_utf16().chain(Some(0)).collect();
        let bytes = wide.len() * 2;
        unsafe {
            if OpenClipboard(std::ptr::null_mut()) == 0 {
                return Err("clipboard busy — try again".to_string());
            }
            let mut ok: Result<(), String> = Err("clipboard failed".to_string());
            if EmptyClipboard() != 0 {
                let mem = GlobalAlloc(GMEM_MOVEABLE, bytes);
                if !mem.is_null() {
                    let dst = GlobalLock(mem) as *mut u16;
                    if !dst.is_null() {
                        std::ptr::copy_nonoverlapping(wide.as_ptr(), dst, wide.len());
                        GlobalUnlock(mem);
                        if !SetClipboardData(CF_UNICODETEXT, mem).is_null() {
                            ok = Ok(());
                        }
                    }
                }
            }
            CloseClipboard();
            // On success the system owns `mem`; on failure it is intentionally
            // leaked (one small block per failed copy; the process is
            // long-lived and freeing a handed-out handle would corrupt).
            ok
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = text;
        return Err("clipboard copy not supported on this platform yet".to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reveal_missing_path_errors() {
        assert!(reveal_in_folder(Path::new("C:/no/such/file-xyz.md")).is_err());
    }
}
