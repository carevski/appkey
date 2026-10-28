//! Light / dark for appkey's windows: the choice saved in the config (`theme`), else
//! the system's. winit reports the system theme on macOS and Windows; on Linux we
//! ask the desktop ourselves (`platform::dark_mode`), since winit can't on X11.

use eframe::egui;

use crate::config::Theme;
use crate::platform;

pub fn apply(ctx: &egui::Context, choice: Option<Theme>) {
    let theme = match choice {
        Some(Theme::Light) => egui::Theme::Light,
        Some(Theme::Dark) => egui::Theme::Dark,
        None => match platform::dark_mode() {
            Some(true) => egui::Theme::Dark,
            Some(false) => egui::Theme::Light,
            None => return ctx.set_theme(egui::ThemePreference::System),
        },
    };
    ctx.set_theme(theme);
}

/// "System / Light / Dark" buttons. Returns `true` when the choice changed.
pub fn picker(ui: &mut egui::Ui, choice: &mut Option<Theme>) -> bool {
    let before = *choice;
    let mut options =
        vec![(None, "System"), (Some(Theme::Light), "Light"), (Some(Theme::Dark), "Dark")];
    // In a right-to-left layout, add them in reverse so they still read left to right.
    if ui.layout().prefer_right_to_left() {
        options.reverse();
    }
    for (value, label) in options {
        ui.selectable_value(choice, value, label);
    }
    *choice != before
}
