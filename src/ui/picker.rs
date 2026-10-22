//! "Choose an application" dialog: running apps, installed apps, pick a window
//! by switching to it, or type the details by hand.

use std::time::{Duration, Instant};

use eframe::egui::{self, Id, Modal, RichText, ScrollArea, TextEdit};

use crate::platform::{self, AppInfo};

const CAPTURE_DELAY: Duration = Duration::from_secs(3);

#[derive(Debug)]
pub struct Picker {
    search: String,
    running: Result<Vec<AppInfo>, String>,
    installed: Vec<AppInfo>,
    /// appkey's own actions (the memory window), where available.
    builtin: Vec<AppInfo>,
    capture_until: Option<Instant>,
    message: Option<String>,
    manual_app: String,
    manual_launch: String,
    first_frame: bool,
}

/// What the dialog decided this frame.
#[derive(Debug)]
pub enum Outcome {
    Open,
    Chosen(AppInfo),
    Cancelled,
}

impl Picker {
    pub fn new(current: Option<&AppInfo>) -> Self {
        Self {
            search: String::new(),
            // Hide ourselves (the settings window is a regular app while it's open).
            running: platform::running_apps()
                .map(|apps| {
                    apps.into_iter().filter(|a| !a.app.eq_ignore_ascii_case("appkey")).collect()
                })
                .map_err(|e| format!("{e:#}")),
            installed: platform::installed_apps(),
            builtin: builtin_actions(),
            capture_until: None,
            message: None,
            manual_app: current.map(|a| a.app.clone()).unwrap_or_default(),
            manual_launch: current
                .and_then(|a| a.launch.as_ref())
                .map(|l| join_command(l))
                .unwrap_or_default(),
            first_frame: true,
        }
    }

    pub fn show(&mut self, ctx: &egui::Context) -> Outcome {
        if let Some(info) = self.poll_capture(ctx) {
            return Outcome::Chosen(info);
        }
        let mut outcome = Outcome::Open;
        let modal = Modal::new(Id::new("app-picker")).show(ctx, |ui| {
            ui.set_width(460.0);
            ui.heading("Choose an application");
            ui.add_space(6.0);

            let search = ui.add(
                TextEdit::singleline(&mut self.search)
                    .hint_text("Search…")
                    .desired_width(f32::INFINITY),
            );
            if std::mem::take(&mut self.first_frame) {
                search.request_focus();
            }
            ui.add_space(4.0);

            let needle = self.search.to_lowercase();
            let hit = |a: &AppInfo| {
                needle.is_empty()
                    || a.name.to_lowercase().contains(&needle)
                    || a.app.to_lowercase().contains(&needle)
            };
            ScrollArea::vertical().max_height(280.0).auto_shrink([false, true]).show(ui, |ui| {
                if self.builtin.iter().any(&hit) {
                    ui.label(RichText::new("appkey").strong());
                    list(ui, self.builtin.iter().filter(|a| hit(a)), &mut outcome);
                    ui.add_space(6.0);
                }
                ui.label(RichText::new("Open now").strong());
                match &self.running {
                    Ok(apps) => list(ui, apps.iter().filter(|a| hit(a)), &mut outcome),
                    Err(e) => {
                        ui.label(RichText::new(e).weak());
                    }
                }
                ui.add_space(6.0);
                ui.label(RichText::new("Installed").strong());
                list(ui, self.installed.iter().filter(|a| hit(a)), &mut outcome);
            });

            ui.separator();
            ui.horizontal(|ui| {
                match self.capture_until {
                    Some(until) => {
                        let left = until.saturating_duration_since(Instant::now()).as_secs() + 1;
                        ui.label(RichText::new(format!("Switch to the app now… {left}")).strong());
                    }
                    None => {
                        if ui
                            .button("Pick a window…")
                            .on_hover_text("Click, then switch to the app within 3 seconds")
                            .clicked()
                        {
                            self.capture_until = Some(Instant::now() + CAPTURE_DELAY);
                            self.message = None;
                        }
                    }
                }
                if let Some(m) = &self.message {
                    ui.label(RichText::new(m).color(ui.visuals().warn_fg_color));
                }
            });

            egui::CollapsingHeader::new("Enter details manually").show(ui, |ui| {
                egui::Grid::new("manual").num_columns(2).show(ui, |ui| {
                    ui.label("App id");
                    ui.add(TextEdit::singleline(&mut self.manual_app).hint_text(platform_hint()));
                    ui.end_row();
                    ui.label("Launch command");
                    ui.add(TextEdit::singleline(&mut self.manual_launch).hint_text("optional"));
                    ui.end_row();
                });
                let app = self.manual_app.trim();
                if ui.add_enabled(!app.is_empty(), egui::Button::new("Use these details")).clicked()
                {
                    let launch = split_command(&self.manual_launch);
                    outcome = Outcome::Chosen(AppInfo {
                        name: app.to_owned(),
                        app: app.to_owned(),
                        launch: (!launch.is_empty()).then_some(launch),
                    });
                }
            });

            ui.add_space(4.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Cancel").clicked() {
                    outcome = Outcome::Cancelled;
                }
            });
        });
        if modal.should_close() && matches!(outcome, Outcome::Open) {
            outcome = Outcome::Cancelled;
        }
        outcome
    }

    /// Ticks the "switch to the app" countdown; when it ends, reads the frontmost app
    /// and brings this window back.
    fn poll_capture(&mut self, ctx: &egui::Context) -> Option<AppInfo> {
        let until = self.capture_until?;
        if Instant::now() < until {
            ctx.request_repaint_after(Duration::from_millis(200));
            return None;
        }
        self.capture_until = None;
        let found = platform::frontmost_app();
        let _ = platform::focus_pid(std::process::id());
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        match found {
            Ok(Some(info)) => Some(info),
            Ok(None) => {
                self.message =
                    Some("No app caught. Switch to it before the countdown ends.".into());
                None
            }
            Err(e) => {
                self.message = Some(format!("{e:#}"));
                None
            }
        }
    }
}

fn shorten(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_owned()
    } else {
        format!("{}…", s.chars().take(max - 1).collect::<String>())
    }
}

fn builtin_actions() -> Vec<AppInfo> {
    #[cfg(target_os = "linux")]
    if crate::memory::available() {
        return vec![AppInfo {
            name: "Memory window".into(),
            app: crate::config::MEMORY_ACTION.into(),
            launch: None,
        }];
    }
    Vec::new()
}

fn list<'a>(ui: &mut egui::Ui, apps: impl Iterator<Item = &'a AppInfo>, outcome: &mut Outcome) {
    let mut any = false;
    for a in apps {
        any = true;
        // Some names are long (Chromium with a custom profile puts its path in WM_CLASS):
        // cut them so the dialog keeps its size.
        let text = format!("{}   ", shorten(&a.name, 44));
        let r = ui
            .horizontal(|ui| {
                let r = ui.selectable_label(false, text);
                ui.label(RichText::new(shorten(&a.app, 36)).weak().small());
                r
            })
            .inner
            .on_hover_text(format!("{}\n{}", a.name, a.app));
        if r.clicked() {
            *outcome = Outcome::Chosen(a.clone());
        }
    }
    if !any {
        ui.label(RichText::new("Nothing found").weak());
    }
}

fn platform_hint() -> &'static str {
    if cfg!(target_os = "macos") {
        "app name or bundle id"
    } else if cfg!(windows) {
        "program file, e.g. chrome.exe"
    } else {
        "window class, e.g. firefox"
    }
}

/// Splits a command line on spaces; double quotes group words with spaces.
pub fn split_command(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let (mut quoted, mut any) = (false, false);
    for c in s.chars() {
        match c {
            '"' => (quoted, any) = (!quoted, true),
            c if c.is_whitespace() && !quoted => {
                if any || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            c => cur.push(c),
        }
    }
    if any || !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Inverse of [`split_command`] for display.
pub fn join_command(argv: &[String]) -> String {
    argv.iter()
        .map(|a| {
            if a.is_empty() || a.contains(char::is_whitespace) {
                format!("\"{a}\"")
            } else {
                a.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_round_trip() {
        let argv =
            vec![r"C:\Program Files\App\app.exe".to_owned(), "--new".to_owned(), String::new()];
        assert_eq!(split_command(&join_command(&argv)), argv);
        assert_eq!(split_command("  a   b "), ["a", "b"]);
        assert_eq!(split_command(""), Vec::<String>::new());
    }
}
