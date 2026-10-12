//! Windows backend.
//!
//! Hotkeys use `RegisterHotKey` (no permission needed). Windows are found with
//! `EnumWindows`, which walks top-level windows in z-order (front to back), so the
//! first match is the most recently used one.
//!
//! Security notes:
//! - `SetForegroundWindow` is only honoured for the process that received the last
//!   input event; receiving `WM_HOTKEY` qualifies us.
//! - UIPI: a non-elevated appkey cannot fully control windows of elevated (admin)
//!   processes. We report that instead of trying to bypass it.

use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::Path;

use anyhow::{Result, bail};
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM};
use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GW_OWNER, GWL_EXSTYLE, GetWindow, GetWindowLongW, GetWindowThreadProcessId,
    IsIconic, IsWindowVisible, SW_RESTORE, SetForegroundWindow, ShowWindow, WS_EX_TOOLWINDOW,
};
use windows::core::{BOOL, PWSTR};

use super::{Outcome, spawn};
use crate::matcher::matches;

/// An app window and the full path of the executable that owns it.
struct AppWindow {
    hwnd: HWND,
    exe: String,
}

/// All user-facing top-level windows, front to back.
fn app_windows() -> Vec<AppWindow> {
    unsafe extern "system" fn collect(hwnd: HWND, lparam: LPARAM) -> BOOL {
        // SAFETY: `lparam` is the `&mut Vec` passed to `EnumWindows` below, which
        // outlives the (synchronous) enumeration.
        let out = unsafe { &mut *(lparam.0 as *mut Vec<AppWindow>) };
        if is_app_window(hwnd)
            && let Some(exe) = exe_path(hwnd)
        {
            out.push(AppWindow { hwnd, exe });
        }
        BOOL::from(true)
    }

    let mut out: Vec<AppWindow> = Vec::new();
    // SAFETY: the callback only runs during this call and only touches `out`.
    let _ = unsafe { EnumWindows(Some(collect), LPARAM(&raw mut out as isize)) };
    out
}

/// Visible, unowned, not a tool window, and not cloaked (hidden UWP/virtual-desktop windows).
fn is_app_window(hwnd: HWND) -> bool {
    // SAFETY: plain Win32 queries on a window handle supplied by EnumWindows.
    unsafe {
        if !IsWindowVisible(hwnd).as_bool() {
            return false;
        }
        if GetWindow(hwnd, GW_OWNER).is_ok_and(|owner| !owner.is_invalid()) {
            return false;
        }
        #[allow(clippy::cast_sign_loss)]
        let ex_style = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
        if ex_style & WS_EX_TOOLWINDOW.0 != 0 {
            return false;
        }
        let mut cloaked: u32 = 0;
        let ok = DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            (&raw mut cloaked).cast(),
            4, // size_of::<u32>()
        );
        !(ok.is_ok() && cloaked != 0)
    }
}

fn exe_path(hwnd: HWND) -> Option<String> {
    let mut pid = 0u32;
    // SAFETY: Win32 calls with valid out-pointers; the process handle is closed below.
    unsafe {
        GetWindowThreadProcessId(hwnd, Some(&raw mut pid));
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut len: u32 = 1024;
        let res = QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &raw mut len,
        );
        let _ = CloseHandle(process);
        res.ok()?;
        Some(OsString::from_wide(&buf[..len as usize]).to_string_lossy().into_owned())
    }
}

fn exe_name(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

/// `app` may be an exe name (`code.exe`, `code`) or a full path.
fn window_matches(app: &str, exe: &str) -> bool {
    if app.contains(['\\', '/']) { matches(app, exe) } else { matches(app, exe_name(exe)) }
}

pub fn focus_or_launch(app: &str, launch: Option<&[String]>) -> Result<Outcome> {
    if let Some(w) = app_windows().into_iter().find(|w| window_matches(app, &w.exe)) {
        // SAFETY: `w.hwnd` came from EnumWindows; a stale handle just makes these fail.
        unsafe {
            if IsIconic(w.hwnd).as_bool() {
                let _ = ShowWindow(w.hwnd, SW_RESTORE);
            }
            if !SetForegroundWindow(w.hwnd).as_bool() {
                bail!(
                    "Windows refused to focus {} (if it runs as administrator, appkey must too)",
                    exe_name(&w.exe)
                );
            }
        }
        return Ok(Outcome::Focused);
    }
    match launch {
        Some(argv) => spawn(argv)?,
        None => spawn(&[app.to_owned()])?,
    }
    Ok(Outcome::Launched)
}

#[allow(clippy::unnecessary_wraps)] // same signature on every platform
pub fn running_apps() -> Result<Vec<String>> {
    let mut names: Vec<String> =
        app_windows().into_iter().map(|w| exe_name(&w.exe).to_owned()).collect();
    names.sort_by_key(|n| n.to_lowercase());
    names.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
    Ok(names)
}

pub fn diagnostics() -> Vec<String> {
    vec![
        "Windows: no extra permissions required.".into(),
        "Apps running as administrator can only be focused if appkey also runs elevated.".into(),
    ]
}

#[allow(clippy::unnecessary_wraps)]
pub fn ensure_supported() -> Result<()> {
    Ok(())
}

pub fn open_in_editor(path: &Path) -> Result<()> {
    spawn(&["notepad.exe".to_owned(), path.to_string_lossy().into_owned()])
}

/// Release builds use the GUI subsystem (no console window flashes up when started
/// from Explorer or at login). Reattach to the parent console so CLI output still
/// shows when run from a terminal.
pub fn attach_console() {
    use windows::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
    // SAFETY: no preconditions; failure (no parent console) is expected and harmless.
    let _ = unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
}

#[cfg(test)]
mod tests {
    use super::window_matches;

    #[test]
    fn matches_name_or_path() {
        let exe = r"C:\Program Files\WindowsApps\WindowsTerminal.exe";
        assert!(window_matches("windowsterminal", exe));
        assert!(window_matches(exe, exe));
        assert!(!window_matches(r"C:\Other\WindowsTerminal.exe", exe));
    }
}
