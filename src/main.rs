// Release builds on Windows are GUI-subsystem so no console window appears at login.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod config;
mod hotkeys;
mod matcher;
#[cfg(any(target_os = "linux", test))]
mod memory;
mod platform;
mod ui;

use std::path::Path;
use std::process::ExitCode;

use anyhow::{Result, bail};

const HELP: &str = "\
appkey: global shortcuts that open an app, or focus it if it's already running.

USAGE:
    appkey [COMMAND]

COMMANDS:
    run           Start in the tray and listen for shortcuts (default)
    settings      Open the settings window
    check         Validate the config and show platform notes
    list          Show running apps and the names to use as `app` in the config
    open <app>    Focus or launch <app> once, exactly like a shortcut would
    memory        Open settings on Processes: what uses memory, and close it (Ubuntu/Mint)
    config        Print the config file path
    help          Show this help
    version       Show the version
";

fn main() -> ExitCode {
    #[cfg(windows)]
    platform::attach_console();

    match cli(&std::env::args().skip(1).collect::<Vec<_>>()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("appkey: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn cli(args: &[String]) -> Result<()> {
    let path = config::path()?;
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        [] | ["run"] => app::run(path),
        ["check"] => check(&path),
        ["list"] => {
            for a in platform::running_apps()? {
                println!("{:<28} app = {:?}", a.name, a.app);
            }
            Ok(())
        }
        ["open", app] => open(&path, app),
        ["settings"] => ui::run(path),
        ["memory"] => memory_window(path, None),
        // Opened by the daemon when memory runs low / a background program is busy
        // (see memory::Watcher).
        ["memory", flag @ ("--alert" | "--busy")] => memory_window(path, Some(flag)),
        // Run by desktops that execute a command per shortcut (Hyprland binds).
        ["__hotkey", id] => run_hotkey(&path, id),
        // Test helper (undocumented): the id desktops know a shortcut by.
        ["__hotkey-id", keys] => {
            let hotkey: global_hotkey::hotkey::HotKey =
                keys.parse().map_err(|e| anyhow::anyhow!("{e}"))?;
            println!("{}", hotkey.id());
            Ok(())
        }
        ["config"] => {
            println!("{}", path.display());
            Ok(())
        }
        ["help" | "-h" | "--help"] => {
            print!("{HELP}");
            Ok(())
        }
        ["version" | "-V" | "--version"] => {
            println!("appkey {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        _ => bail!("unknown command {:?}; see `appkey help`", args.join(" ")),
    }
}

fn check(path: &Path) -> Result<()> {
    println!("config: {}", path.display());
    let cfg = config::load_or_init(path)?;
    println!("{} shortcut(s) for {}:", cfg.shortcuts.len(), config::CURRENT_OS);
    for s in &cfg.shortcuts {
        let launch = s.launch.as_ref().map(|l| format!("  (launch: {})", l.join(" ")));
        println!("  {:<20} -> {}{}", s.keys, s.app, launch.unwrap_or_default());
    }
    for note in platform::diagnostics() {
        println!("{note}");
    }
    platform::ensure_supported()
}

#[cfg(target_os = "linux")]
fn memory_window(path: std::path::PathBuf, flag: Option<&str>) -> Result<()> {
    let warning = flag
        .map(|f| if f == "--alert" { memory::Warning::LowMemory } else { memory::Warning::Busy });
    ui::memory_window(path, warning)
}

#[cfg(not(target_os = "linux"))]
fn memory_window(_path: std::path::PathBuf, _flag: Option<&str>) -> Result<()> {
    bail!("`appkey memory` is only available on Ubuntu and Linux Mint")
}

/// What a shortcut does: open appkey's memory window, or focus / launch its app.
pub(crate) fn perform(app: &str, launch: Option<&[String]>) -> Result<platform::Outcome> {
    if app == config::MEMORY_ACTION {
        #[cfg(target_os = "linux")]
        {
            memory::show_window()?;
            return Ok(platform::Outcome::Focused);
        }
        #[cfg(not(target_os = "linux"))]
        bail!("the memory window is only available on Ubuntu and Linux Mint");
    }
    platform::focus_or_launch(app, launch)
}

/// Does what the shortcut with this `HotKey` id is configured to do.
fn run_hotkey(path: &Path, id: &str) -> Result<()> {
    let id: u32 = id.parse()?;
    let cfg = config::load_or_init(path)?;
    #[cfg(target_os = "linux")]
    memory::use_limit(cfg.memory_limit);
    let Some(s) = cfg.shortcuts.iter().find(|s| s.hotkey.id() == id) else {
        bail!("no shortcut with id {id} in the config");
    };
    perform(&s.app, s.launch.as_deref())?;
    Ok(())
}

/// Uses the config's `launch` for this app if there is one, so `open` behaves like the hotkey.
fn open(path: &Path, app: &str) -> Result<()> {
    let cfg = config::load_or_init(path).ok();
    #[cfg(target_os = "linux")]
    memory::use_limit(cfg.as_ref().map(|c| c.memory_limit).unwrap_or_default());
    let launch = cfg
        .and_then(|c| c.shortcuts.into_iter().find(|s| matcher::matches(app, &s.app)))
        .and_then(|s| s.launch);
    let outcome = perform(app, launch.as_deref())?;
    println!("{outcome:?} {app}");
    Ok(())
}
