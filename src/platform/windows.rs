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
    EnumWindows, GW_OWNER, GWL_EXSTYLE, GetForegroundWindow, GetWindow, GetWindowLongW,
    GetWindowThreadProcessId, IsIconic, IsWindowVisible, SW_RESTORE, SetForegroundWindow,
    ShowWindow, WS_EX_TOOLWINDOW,
};
use windows::core::{BOOL, PWSTR};

use super::{AppInfo, Outcome, lnk, spawn};
use crate::matcher::matches;

/// An app window and the full path of the executable that owns it.
struct AppWindow {
    hwnd: HWND,
    pid: u32,
    exe: String,
}

/// All user-facing top-level windows, front to back.
fn app_windows() -> Vec<AppWindow> {
    unsafe extern "system" fn collect(hwnd: HWND, lparam: LPARAM) -> BOOL {
        // SAFETY: `lparam` is the `&mut Vec` passed to `EnumWindows` below, which
        // outlives the (synchronous) enumeration.
        let out = unsafe { &mut *(lparam.0 as *mut Vec<AppWindow>) };
        if is_app_window(hwnd)
            && let Some((pid, exe)) = exe_path(hwnd)
        {
            out.push(AppWindow { hwnd, pid, exe });
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

/// The owning process id and its executable's full path.
fn exe_path(hwnd: HWND) -> Option<(u32, String)> {
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
        Some((pid, OsString::from_wide(&buf[..len as usize]).to_string_lossy().into_owned()))
    }
}

fn exe_name(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

/// `app` may be an exe name (`code.exe`, `code`) or a full path.
fn window_matches(app: &str, exe: &str) -> bool {
    if app.contains(['\\', '/']) { matches(app, exe) } else { matches(app, exe_name(exe)) }
}

/// Restores (if minimized) and focuses a window. `false` if Windows refused.
fn activate(hwnd: HWND) -> bool {
    // SAFETY: `hwnd` came from EnumWindows; a stale handle just makes these fail.
    unsafe {
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        SetForegroundWindow(hwnd).as_bool()
    }
}

pub fn focus_or_launch(app: &str, launch: Option<&[String]>) -> Result<Outcome> {
    if let Some(w) = app_windows().into_iter().find(|w| window_matches(app, &w.exe)) {
        if !activate(w.hwnd) {
            bail!(
                "Windows refused to focus {} (if it runs as administrator, appkey must too)",
                exe_name(&w.exe)
            );
        }
        return Ok(Outcome::Focused);
    }
    match launch {
        Some(argv) => spawn(argv)?,
        None => spawn(&[app.to_owned()])?,
    }
    Ok(Outcome::Launched)
}

/// `C:\\…\\Code.exe` → name "Code", app "Code.exe", launch the full path
/// (exe names usually aren't on PATH, so the default launch wouldn't find them).
fn info_for_exe(path: &str) -> AppInfo {
    let file = exe_name(path);
    let name = file.strip_suffix(".exe").or_else(|| file.strip_suffix(".EXE")).unwrap_or(file);
    AppInfo { name: name.to_owned(), app: file.to_owned(), launch: Some(vec![path.to_owned()]) }
}

#[allow(clippy::unnecessary_wraps)] // same signature on every platform
pub fn running_apps() -> Result<Vec<AppInfo>> {
    let mut apps: Vec<AppInfo> = app_windows().iter().map(|w| info_for_exe(&w.exe)).collect();
    apps.sort_by_key(|a| a.name.to_lowercase());
    apps.dedup_by(|a, b| a.app.eq_ignore_ascii_case(&b.app));
    Ok(apps)
}

/// Start Menu shortcuts (all users + current user) that point at an `.exe`.
pub fn installed_apps() -> Vec<AppInfo> {
    let roots = [
        ("ProgramData", r"Microsoft\Windows\Start Menu\Programs"),
        ("APPDATA", r"Microsoft\Windows\Start Menu\Programs"),
    ];
    let mut links = Vec::new();
    for (var, sub) in roots {
        if let Some(base) = std::env::var_os(var) {
            collect_links(&std::path::PathBuf::from(base).join(sub), &mut links);
        }
    }
    let mut apps: Vec<AppInfo> = links
        .into_iter()
        .filter_map(|link| {
            let target = lnk::target_path(&std::fs::read(&link).ok()?)?;
            let name = link.file_stem()?.to_string_lossy().into_owned();
            let lower = name.to_lowercase();
            if !target.to_lowercase().ends_with(".exe") || lower.contains("uninstall") {
                return None;
            }
            Some(AppInfo { name, ..info_for_exe(&target) })
        })
        .collect();
    apps.sort_by_key(|a| a.name.to_lowercase());
    apps.dedup_by(|a, b| a.app.eq_ignore_ascii_case(&b.app));
    apps
}

fn collect_links(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for path in entries.flatten().map(|e| e.path()) {
        if path.is_dir() {
            collect_links(&path, out);
        } else if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("lnk")) {
            out.push(path);
        }
    }
}

#[allow(clippy::unnecessary_wraps)] // same signature on every platform
pub fn frontmost_app() -> Result<Option<AppInfo>> {
    // SAFETY: no preconditions; may return a null handle.
    let hwnd = unsafe { GetForegroundWindow() };
    Ok(exe_path(hwnd)
        .filter(|(pid, _)| *pid != std::process::id())
        .map(|(_, exe)| info_for_exe(&exe)))
}

#[allow(clippy::unnecessary_wraps)] // same signature on every platform
pub fn focus_pid(pid: u32) -> Result<bool> {
    Ok(app_windows().into_iter().find(|w| w.pid == pid).is_some_and(|w| activate(w.hwnd)))
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
