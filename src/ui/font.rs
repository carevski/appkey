//! Uses the OS's own UI font instead of bundling fonts (saves ~1.4 MB and looks native).

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Result, bail};
use eframe::egui::{Context, FontData, FontDefinitions, FontFamily};

fn candidates() -> Vec<PathBuf> {
    let fixed: &[&str] = if cfg!(target_os = "macos") {
        &[
            "/System/Library/Fonts/SFNS.ttf",
            "/System/Library/Fonts/Helvetica.ttc",
            "/System/Library/Fonts/Supplemental/Arial.ttf",
        ]
    } else if cfg!(windows) {
        &[r"C:\Windows\Fonts\segoeui.ttf", r"C:\Windows\Fonts\arial.ttf"]
    } else {
        &[
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
            "/usr/share/fonts/TTF/DejaVuSans.ttf",
            "/usr/share/fonts/dejavu-sans-fonts/DejaVuSans.ttf",
            "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf",
            "/usr/share/fonts/noto/NotoSans-Regular.ttf",
            "/usr/share/fonts/google-noto/NotoSans-Regular.ttf",
            "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
        ]
    };
    let mut out = Vec::new();
    // Linux: ask fontconfig for the desktop's actual sans font first.
    if cfg!(target_os = "linux")
        && let Ok(o) =
            std::process::Command::new("fc-match").args(["-f", "%{file}", "sans-serif"]).output()
        && o.status.success()
    {
        let file = String::from_utf8_lossy(&o.stdout).trim().to_owned();
        let ext = std::path::Path::new(&file).extension().map(std::ffi::OsStr::to_ascii_lowercase);
        if ext.is_some_and(|e| e == "ttf" || e == "otf") {
            out.push(PathBuf::from(file));
        }
    }
    out.extend(fixed.iter().map(PathBuf::from));
    out
}

pub fn install(ctx: &Context) -> Result<()> {
    let Some(bytes) = candidates().into_iter().find_map(|p| std::fs::read(p).ok()) else {
        bail!("no usable system font found (looked for DejaVu Sans, Noto Sans, Segoe UI, SF)");
    };
    let mut fonts = FontDefinitions::empty();
    fonts.font_data.insert("system".into(), Arc::new(FontData::from_owned(bytes)));
    for family in [FontFamily::Proportional, FontFamily::Monospace] {
        fonts.families.insert(family, vec!["system".into()]);
    }
    ctx.set_fonts(fonts);
    Ok(())
}
