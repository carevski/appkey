// Release builds on Windows are GUI-subsystem so no console window appears at login.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod config;
mod hotkeys;
mod matcher;
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

/// Uses the config's `launch` for this app if there is one, so `open` behaves like the hotkey.
fn open(path: &Path, app: &str) -> Result<()> {
    let launch = config::load_or_init(path)
        .ok()
        .and_then(|c| c.shortcuts.into_iter().find(|s| matcher::matches(app, &s.app)))
        .and_then(|s| s.launch);
    let outcome = platform::focus_or_launch(app, launch.as_deref())?;
    println!("{outcome:?} {app}");
    Ok(())
}
