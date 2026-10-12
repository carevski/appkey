//! Debug builds only: `APPKEY_SCREENSHOT=out.png appkey settings` renders the
//! window, saves a PNG of it and exits. `APPKEY_SCREENSHOT_SCENE=picker` opens
//! the app picker first. For reviewing UI changes and README screenshots
//! without screen-recording permission.

use std::io::Write as _;
use std::path::PathBuf;

use eframe::egui::{self, ColorImage, UserData, ViewportCommand};

#[derive(Debug)]
pub struct DevShot {
    path: PathBuf,
    pub scene: String,
    frame: u32,
}

impl DevShot {
    pub fn from_env() -> Option<Self> {
        let path = std::env::var_os("APPKEY_SCREENSHOT")?.into();
        let scene = std::env::var("APPKEY_SCREENSHOT_SCENE").unwrap_or_default();
        Some(Self { path, scene, frame: 0 })
    }

    /// Call once per frame. Requests the shot after layout has settled, then saves and closes.
    pub fn tick(&mut self, ctx: &egui::Context) {
        self.frame += 1;
        if self.frame == 15 {
            ctx.send_viewport_cmd(ViewportCommand::Screenshot(UserData::default()));
        }
        ctx.request_repaint();
        let shot = ctx.input(|i| {
            i.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(image) = shot {
            if let Err(e) = write_png(&self.path, &image) {
                eprintln!("appkey: screenshot failed: {e}");
            }
            ctx.send_viewport_cmd(ViewportCommand::Close);
        }
    }
}

/// Uncompressed PNG (stored deflate blocks): no image crate needed for a dev tool.
fn write_png(path: &std::path::Path, img: &ColorImage) -> std::io::Result<()> {
    let [width, height] = img.size;
    let mut raw = Vec::with_capacity(height * (width * 4 + 1));
    for row in img.pixels.chunks(width) {
        raw.push(0); // filter: none
        for p in row {
            raw.extend_from_slice(&p.to_srgba_unmultiplied());
        }
    }
    let mut zlib = vec![0x78, 0x01];
    let mut blocks = raw.chunks(65_535).peekable();
    while let Some(block) = blocks.next() {
        let len = u16::try_from(block.len()).unwrap_or(u16::MAX);
        zlib.push(u8::from(blocks.peek().is_none()));
        zlib.extend_from_slice(&len.to_le_bytes());
        zlib.extend_from_slice(&(!len).to_le_bytes());
        zlib.extend_from_slice(block);
    }
    let (mut sum1, mut sum2) = (1u32, 0u32); // Adler-32
    for &byte in &raw {
        sum1 = (sum1 + u32::from(byte)) % 65_521;
        sum2 = (sum2 + sum1) % 65_521;
    }
    zlib.extend_from_slice(&((sum2 << 16) | sum1).to_be_bytes());

    let mut out = std::fs::File::create(path)?;
    out.write_all(b"\x89PNG\r\n\x1a\n")?;
    let dims = |v: usize| u32::try_from(v).unwrap_or_default().to_be_bytes();
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&dims(width));
    ihdr.extend_from_slice(&dims(height));
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    for (kind, data) in [(&b"IHDR"[..], &ihdr[..]), (b"IDAT", &zlib), (b"IEND", &[])] {
        out.write_all(&u32::try_from(data.len()).unwrap_or_default().to_be_bytes())?;
        out.write_all(kind)?;
        out.write_all(data)?;
        out.write_all(&crc32(kind.iter().chain(data)).to_be_bytes())?;
    }
    Ok(())
}

fn crc32<'a>(bytes: impl Iterator<Item = &'a u8>) -> u32 {
    let mut crc = !0u32;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}
