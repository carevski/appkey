//! The long-running part: tray icon, hotkey registration and event dispatch.
//!
//! Event loops differ per OS but all feed the same [`App::handle`]:
//! - macOS/Windows: a `tao` loop on the main thread (required by Carbon/Win32 hotkeys and the tray).
//! - Linux: hotkeys and the tray run on their own threads, so the main thread just
//!   reads a channel. This keeps GTK out of the Linux build.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::process::Child;
use std::time::{Duration, SystemTime};

use anyhow::Result;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

use crate::config::{self, Shortcut};
use crate::platform::{self, Outcome};

#[derive(Debug)]
pub enum Event {
    Hotkey(u32),
    Menu(MenuId),
    /// Periodic: check whether the config file changed.
    Tick,
}

/// How often the config file is checked for changes (e.g. saved by the settings window).
const WATCH_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Debug, PartialEq, Eq)]
pub enum Flow {
    Continue,
    Quit,
}

pub struct App {
    config_path: PathBuf,
    hotkeys: GlobalHotKeyManager,
    active: HashMap<u32, Shortcut>,
    /// `None` when the desktop has no tray (e.g. no `StatusNotifier` host): shortcuts still work.
    tray: Option<TrayIcon>,
    menu: Menu,
    status: MenuItem,
    settings: MenuId,
    edit: MenuId,
    reload: MenuId,
    quit: MenuId,
    /// Modification time of the config we last loaded.
    loaded_mtime: Option<SystemTime>,
    settings_window: Option<Child>,
}

impl std::fmt::Debug for App {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("App").field("config_path", &self.config_path).finish_non_exhaustive()
    }
}

impl App {
    pub fn new(config_path: PathBuf) -> Result<Self> {
        let hotkeys = GlobalHotKeyManager::new()?;
        let status = MenuItem::new("appkey", false, None);
        let settings = MenuItem::new("Settings…", true, None);
        let edit = MenuItem::new("Edit config file…", true, None);
        let reload = MenuItem::new("Reload config", true, None);
        let quit = MenuItem::new("Quit appkey", true, None);
        let menu = Menu::with_items(&[
            &status,
            &PredefinedMenuItem::separator(),
            &settings,
            &edit,
            &reload,
            &PredefinedMenuItem::separator(),
            &quit,
        ])?;
        let tray = TrayIconBuilder::new().with_menu(Box::new(menu.clone())).with_tooltip("appkey");
        #[cfg(target_os = "macos")]
        let tray = tray.with_icon_templated(icon());
        #[cfg(not(target_os = "macos"))]
        let tray = tray.with_icon(icon());
        let tray = tray
            .build()
            .inspect_err(|e| {
                log(&format!("no tray icon ({e}); shortcuts still work, use `appkey settings`"));
            })
            .ok();

        let mut app = Self {
            config_path,
            hotkeys,
            active: HashMap::new(),
            tray,
            menu,
            status,
            settings: settings.id().clone(),
            edit: edit.id().clone(),
            reload: reload.id().clone(),
            quit: quit.id().clone(),
            loaded_mtime: None,
            settings_window: None,
        };
        app.reload();
        Ok(app)
    }

    pub fn handle(&mut self, event: Event) -> Flow {
        match event {
            Event::Hotkey(id) => {
                if let Some(s) = self.active.get(&id) {
                    match platform::focus_or_launch(&s.app, s.launch.as_deref()) {
                        Ok(Outcome::Focused) => log(&format!("{}: focused {}", s.keys, s.app)),
                        Ok(Outcome::Launched) => log(&format!("{}: launched {}", s.keys, s.app)),
                        Err(e) => self.set_status(&format!("{}: {e:#}", s.keys)),
                    }
                }
            }
            Event::Menu(id) if id == self.quit => return Flow::Quit,
            Event::Menu(id) if id == self.reload => self.reload(),
            Event::Menu(id) if id == self.settings => self.open_settings(),
            Event::Menu(id) if id == self.edit => {
                if let Err(e) = platform::open_in_editor(&self.config_path) {
                    self.set_status(&format!("could not open editor: {e:#}"));
                }
            }
            Event::Menu(_) => {}
            Event::Tick => {
                if config_mtime(&self.config_path) != self.loaded_mtime {
                    self.reload();
                }
            }
        }
        Flow::Continue
    }

    /// Opens the settings window (a separate `appkey settings` process), or brings
    /// it forward if it's already open.
    pub fn open_settings(&mut self) {
        if let Some(child) = &mut self.settings_window
            && matches!(child.try_wait(), Ok(None))
        {
            let _ = platform::focus_pid(child.id());
            return;
        }
        let spawned = std::env::current_exe()
            .and_then(|exe| std::process::Command::new(exe).arg("settings").spawn());
        match spawned {
            Ok(child) => self.settings_window = Some(child),
            Err(e) => self.set_status(&format!("could not open settings: {e}")),
        }
    }

    /// Re-reads the config and swaps the registered hotkeys. A broken config leaves
    /// no hotkeys active and says why, rather than silently keeping stale ones.
    fn reload(&mut self) {
        for s in self.active.values() {
            let _ = self.hotkeys.unregister(s.hotkey);
        }
        self.active.clear();
        self.loaded_mtime = config_mtime(&self.config_path);

        let cfg = match config::load_or_init(&self.config_path) {
            Ok(cfg) => cfg,
            Err(e) => return self.set_status(&format!("config error: {e:#}")),
        };
        let mut failed = Vec::new();
        for s in cfg.shortcuts {
            match self.hotkeys.register(s.hotkey) {
                Ok(()) => {
                    self.active.insert(s.hotkey.id(), s);
                }
                // Usually: another app already owns this combination.
                Err(e) => failed.push(format!("{} ({e})", s.keys)),
            }
        }
        let mut msg = format!("{} shortcut(s) active", self.active.len());
        if !failed.is_empty() {
            let _ = write!(msg, "; unavailable: {}", failed.join(", "));
        }
        self.set_status(&msg);
    }

    fn set_status(&mut self, msg: &str) {
        log(msg);
        let first_line = msg.lines().next().unwrap_or_default();
        self.status.set_text(first_line);
        if let Some(tray) = &self.tray {
            let _ = tray.set_tooltip(Some(format!("appkey: {first_line}")));
            // Some tray backends (Linux SNI) snapshot the menu; push the change.
            tray.set_menu(Some(Box::new(self.menu.clone())));
        }
    }
}

fn config_mtime(path: &std::path::Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

fn log(msg: &str) {
    eprintln!("appkey: {msg}");
}

/// Routes hotkey and menu callbacks (which fire on library-owned threads) into our loop.
fn forward_events(send: impl Fn(Event) + Send + Sync + Clone + 'static) {
    let on_hotkey = send.clone();
    GlobalHotKeyEvent::set_event_handler(Some(move |e: GlobalHotKeyEvent| {
        if e.state() == HotKeyState::Pressed {
            on_hotkey(Event::Hotkey(e.id()));
        }
    }));
    MenuEvent::set_event_handler(Some(move |e: MenuEvent| send(Event::Menu(e.id))));
}

#[cfg(any(target_os = "macos", windows))]
pub fn run(config_path: PathBuf) -> Result<()> {
    use std::sync::{Arc, Mutex};
    use tao::event::{Event as LoopEvent, StartCause};
    use tao::event_loop::{ControlFlow, EventLoopBuilder};

    platform::ensure_supported()?;
    let first_run = config::ensure_exists(&config_path)?;
    #[allow(unused_mut)]
    let mut event_loop = EventLoopBuilder::<Event>::with_user_event().build();
    #[cfg(target_os = "macos")]
    {
        use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS};
        // Menu-bar only: no Dock icon, never steals focus by itself.
        event_loop.set_activation_policy(ActivationPolicy::Accessory);
    }

    let proxy = Arc::new(Mutex::new(event_loop.create_proxy()));
    forward_events(move |e| {
        if let Ok(p) = proxy.lock() {
            let _ = p.send_event(e);
        }
    });

    let mut app: Option<App> = None;
    event_loop.run(move |event, _, flow| {
        *flow = ControlFlow::WaitUntil(std::time::Instant::now() + WATCH_INTERVAL);
        match event {
            // The tray must be created once the loop is running (macOS requirement).
            LoopEvent::NewEvents(StartCause::Init) => match App::new(config_path.clone()) {
                Ok(mut a) => {
                    if first_run {
                        a.open_settings();
                    }
                    app = Some(a);
                }
                Err(e) => {
                    eprintln!("appkey: {e:#}");
                    std::process::exit(1);
                }
            },
            LoopEvent::NewEvents(StartCause::ResumeTimeReached { .. }) => {
                if let Some(a) = app.as_mut() {
                    a.handle(Event::Tick);
                }
            }
            LoopEvent::UserEvent(e) => {
                if app.as_mut().is_some_and(|a| a.handle(e) == Flow::Quit) {
                    *flow = ControlFlow::Exit;
                }
            }
            _ => {}
        }
    })
}

#[cfg(target_os = "linux")]
pub fn run(config_path: PathBuf) -> Result<()> {
    use std::sync::mpsc::RecvTimeoutError;

    platform::ensure_supported()?;
    let first_run = config::ensure_exists(&config_path)?;
    let (tx, rx) = std::sync::mpsc::channel();
    forward_events(move |e| {
        let _ = tx.send(e);
    });
    let mut app = App::new(config_path)?;
    if first_run {
        app.open_settings();
    }
    loop {
        let event = match rx.recv_timeout(WATCH_INTERVAL) {
            Ok(e) => e,
            Err(RecvTimeoutError::Timeout) => Event::Tick,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        if app.handle(event) == Flow::Quit {
            break;
        }
    }
    Ok(())
}

/// A 32×32 keycap drawn in code, so there are no image assets to ship.
/// macOS uses it as a template image (alpha only; the system picks the colour).
fn icon() -> Icon {
    const N: u32 = 32;
    #[allow(clippy::cast_precision_loss)]
    fn inside(x: u32, y: u32, inset: f32, radius: f32) -> bool {
        let n = N as f32;
        let (cx, cy) = (x as f32 + 0.5, y as f32 + 0.5);
        if cx < inset || cy < inset || cx > n - inset || cy > n - inset {
            return false;
        }
        let (lo, hi) = (inset + radius, n - inset - radius);
        let dx = (lo - cx).max(cx - hi).max(0.0);
        let dy = (lo - cy).max(cy - hi).max(0.0);
        dx * dx + dy * dy <= radius * radius
    }
    let color: [u8; 3] = if cfg!(target_os = "macos") { [0, 0, 0] } else { [0x3b, 0x82, 0xf6] };
    let mut rgba = Vec::with_capacity((N * N * 4) as usize);
    for y in 0..N {
        for x in 0..N {
            let ring = inside(x, y, 2.0, 7.0) && !inside(x, y, 5.0, 4.5);
            let dot = inside(x, y, 11.0, 2.5);
            rgba.extend_from_slice(&color);
            rgba.push(if ring || dot { 255 } else { 0 });
        }
    }
    Icon::from_rgba(rgba, N, N).expect("icon buffer matches its dimensions")
}
