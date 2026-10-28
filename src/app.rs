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
use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

use crate::config::{self, Shortcut};
use crate::hotkeys::{Hotkeys, Sink};
use crate::platform::{self, Outcome};

#[derive(Debug)]
pub enum Event {
    Hotkey(u32),
    Menu(MenuId),
    /// Left click on the tray icon: opens settings (the menu is on right click).
    TrayClick,
    /// Periodic: check whether the config file changed.
    Tick,
}

/// How often the config file is checked for changes (e.g. saved by the settings window).
const WATCH_INTERVAL: Duration = Duration::from_secs(1);
/// How often to retry when the desktop's shortcut mechanism isn't available yet.
const HOTKEY_RETRY: Duration = Duration::from_secs(5);

#[derive(Debug, PartialEq, Eq)]
pub enum Flow {
    Continue,
    Quit,
}

pub struct App {
    config_path: PathBuf,
    /// `None` while the desktop's shortcut mechanism isn't available yet (e.g. the
    /// GNOME helper waits for a re-login); retried periodically.
    hotkeys: Option<Box<dyn Hotkeys>>,
    hotkeys_error: String,
    sink: Sink,
    next_retry: std::time::Instant,
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
    #[cfg(target_os = "linux")]
    memory_watch: config::MemoryWatch,
    #[cfg(target_os = "linux")]
    memory: crate::memory::Watcher,
    /// The `[memory_limit]` last set on the slice, and why setting it failed.
    #[cfg(target_os = "linux")]
    applied_limit: Option<config::MemoryLimit>,
    #[cfg(target_os = "linux")]
    limit_error: Option<String>,
}

impl std::fmt::Debug for App {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("App").field("config_path", &self.config_path).finish_non_exhaustive()
    }
}

impl App {
    pub fn new(config_path: PathBuf, sink: Sink) -> Result<Self> {
        let (hotkeys, hotkeys_error) = match platform::hotkeys(sink.clone()) {
            Ok(h) => (Some(h), String::new()),
            Err(e) => (None, format!("{e:#}")),
        };
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
        // Linux: whether a left click shows the menu is up to the tray host; Cinnamon,
        // KDE and the GNOME AppIndicator extension send it to us as a click instead.
        let tray = TrayIconBuilder::new()
            .with_menu(Box::new(menu.clone()))
            .with_menu_on_left_click(false)
            .with_tooltip("appkey");
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
            hotkeys_error,
            sink,
            next_retry: std::time::Instant::now() + HOTKEY_RETRY,
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
            #[cfg(target_os = "linux")]
            memory_watch: config::MemoryWatch::default(),
            #[cfg(target_os = "linux")]
            applied_limit: None,
            #[cfg(target_os = "linux")]
            limit_error: None,
            #[cfg(target_os = "linux")]
            memory: crate::memory::Watcher::default(),
        };
        app.reload();
        Ok(app)
    }

    pub fn handle(&mut self, event: Event) -> Flow {
        match event {
            Event::Hotkey(id) => {
                if let Some(s) = self.active.get(&id) {
                    match crate::perform(&s.app, s.launch.as_deref()) {
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
            Event::TrayClick => self.open_settings(),
            Event::Tick => {
                if self.hotkeys.is_none() && std::time::Instant::now() >= self.next_retry {
                    self.next_retry = std::time::Instant::now() + HOTKEY_RETRY;
                    if let Ok(h) = platform::hotkeys(self.sink.clone()) {
                        self.hotkeys = Some(h);
                        self.reload();
                    }
                } else if config_mtime(&self.config_path) != self.loaded_mtime {
                    self.reload();
                }
                #[cfg(target_os = "linux")]
                if let Err(e) = self.memory.tick(self.memory_watch) {
                    self.set_status(&format!("{e:#}"));
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
        // Opened another way (`appkey memory`, a warning).
        #[cfg(target_os = "linux")]
        if let Some(p) = crate::memory::find_window() {
            let _ = platform::focus_pid(p.pid);
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
        self.active.clear();
        self.loaded_mtime = config_mtime(&self.config_path);
        let cfg = config::load_or_init(&self.config_path);
        #[cfg(target_os = "linux")]
        {
            self.memory_watch = cfg.as_ref().map(|c| c.memory_watch).unwrap_or_default();
            let limit = cfg.as_ref().map(|c| c.memory_limit).unwrap_or_default();
            crate::memory::use_limit(limit);
            if self.applied_limit != Some(limit) {
                self.limit_error =
                    crate::memory::apply_limit(limit).err().map(|e| format!("{e:#}"));
                self.applied_limit = self.limit_error.is_none().then_some(limit);
            }
        }

        let Some(hotkeys) = self.hotkeys.as_mut() else {
            let msg = format!("shortcuts unavailable: {}", self.hotkeys_error);
            return self.set_status(&msg);
        };
        let cfg = match cfg {
            Ok(cfg) => cfg,
            Err(e) => {
                hotkeys.set(&[]);
                return self.set_status(&format!("config error: {e:#}"));
            }
        };
        let failed = hotkeys.set(&cfg.shortcuts);
        let count = cfg.shortcuts.len() - failed.len();
        self.active = cfg.shortcuts.into_iter().map(|s| (s.hotkey.id(), s)).collect();
        let mut msg = format!("{count} shortcut(s) active");
        if !failed.is_empty() {
            let _ = write!(msg, "; unavailable: {}", failed.join(", "));
        }
        self.set_status(&msg);
    }

    fn set_status(&mut self, msg: &str) {
        #[cfg(target_os = "linux")]
        let msg = &match &self.limit_error {
            Some(e) => format!("{msg}; memory limit not set: {e}"),
            None => msg.to_owned(),
        };
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

pub(crate) fn log(msg: &str) {
    eprintln!("appkey: {msg}");
}

/// Routes menu callbacks (fired on library-owned threads) into our loop and returns
/// the sink hotkey backends report presses to.
fn forward_events(send: impl Fn(Event) + Send + Sync + Clone + 'static) -> Sink {
    let on_hotkey = send.clone();
    let on_click = send.clone();
    MenuEvent::set_event_handler(Some(move |e: MenuEvent| send(Event::Menu(e.id))));
    TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| {
        if let TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        } = e
        {
            on_click(Event::TrayClick);
        }
    }));
    std::sync::Arc::new(move |id| on_hotkey(Event::Hotkey(id)))
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
    let sink = forward_events(move |e| {
        if let Ok(p) = proxy.lock() {
            let _ = p.send_event(e);
        }
    });

    let mut app: Option<App> = None;
    event_loop.run(move |event, _, flow| {
        *flow = ControlFlow::WaitUntil(std::time::Instant::now() + WATCH_INTERVAL);
        match event {
            // The tray must be created once the loop is running (macOS requirement).
            LoopEvent::NewEvents(StartCause::Init) => {
                match App::new(config_path.clone(), sink.clone()) {
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
                }
            }
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
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::RecvTimeoutError;

    // Shut down cleanly on logout/kill so desktop-side state (e.g. Hyprland binds)
    // is removed: the loop notices within WATCH_INTERVAL and drops `App`.
    static STOP: AtomicBool = AtomicBool::new(false);
    for signal in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
        // SAFETY: the handler only stores to an atomic, which is async-signal-safe.
        unsafe { signal_hook_registry::register(signal, || STOP.store(true, Ordering::SeqCst))? };
    }

    platform::ensure_supported()?;
    let first_run = config::ensure_exists(&config_path)?;
    let (tx, rx) = std::sync::mpsc::channel();
    let sink = forward_events(move |e| {
        let _ = tx.send(e);
    });
    let mut app = App::new(config_path, sink)?;
    if first_run {
        app.open_settings();
    }
    while !STOP.load(Ordering::SeqCst) {
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
