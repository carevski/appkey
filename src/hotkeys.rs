//! Global shortcut registration, behind one interface so each platform/desktop
//! can use whatever mechanism it allows. Every implementation reports presses
//! through a [`Sink`] as the shortcut's `HotKey::id()`.

use std::sync::Arc;

use anyhow::Result;
use global_hotkey::hotkey::HotKey;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};

use crate::config::Shortcut;

/// Called (from any thread) with the id of the shortcut that was pressed.
pub type Sink = Arc<dyn Fn(u32) + Send + Sync>;

pub trait Hotkeys {
    /// Makes exactly `shortcuts` active, replacing whatever was registered before.
    /// Returns a message for each shortcut that couldn't be registered.
    fn set(&mut self, shortcuts: &[Shortcut]) -> Vec<String>;
}

/// macOS (Carbon), Windows (`RegisterHotKey`) and X11 (`XGrabKey`) via global-hotkey.
pub struct GlobalHotkeys {
    manager: GlobalHotKeyManager,
    registered: Vec<HotKey>,
}

impl std::fmt::Debug for GlobalHotkeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GlobalHotkeys")
            .field("registered", &self.registered)
            .finish_non_exhaustive()
    }
}

impl GlobalHotkeys {
    pub fn new(sink: Sink) -> Result<Self> {
        GlobalHotKeyEvent::set_event_handler(Some(move |e: GlobalHotKeyEvent| {
            if e.state() == HotKeyState::Pressed {
                sink(e.id());
            }
        }));
        Ok(Self { manager: GlobalHotKeyManager::new()?, registered: Vec::new() })
    }
}

impl Hotkeys for GlobalHotkeys {
    fn set(&mut self, shortcuts: &[Shortcut]) -> Vec<String> {
        for h in self.registered.drain(..) {
            let _ = self.manager.unregister(h);
        }
        let mut failed = Vec::new();
        for s in shortcuts {
            match self.manager.register(s.hotkey) {
                Ok(()) => self.registered.push(s.hotkey),
                // Usually: another app already owns this combination.
                Err(e) => failed.push(format!("{} ({e})", s.keys)),
            }
        }
        failed
    }
}
