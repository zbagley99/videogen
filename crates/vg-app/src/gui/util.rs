//! Small helpers shared across the GUI: file names, times, sizes, thumbnails
//! and the extensions each kind of media file has.

use eframe::egui;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// The longest side of a card or pose thumbnail, in pixels.
pub const THUMB: u32 = 160;

/// `img` shrunk to fit a [`THUMB`] square, ready to upload as a texture.
pub fn thumbnail(img: &image::DynamicImage) -> egui::ColorImage {
    let img = img.thumbnail(THUMB, THUMB).to_rgba8();
    egui::ColorImage::from_rgba_unmultiplied([img.width() as usize, img.height() as usize], img.as_raw())
}

pub const IMAGE_EXTS: &[&str] = &["png", "jpg", "jpeg", "webp", "gif", "bmp"];
pub const VIDEO_EXTS: &[&str] = &["mp4", "mov", "mkv", "webm", "avi", "m4v"];
pub const AUDIO_EXTS: &[&str] = &["wav", "flac", "mp3", "ogg", "m4a", "opus"];

/// The last part of `p` as text ("" when it has none).
pub fn file_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// "s" unless there's exactly one: `format!("{n} file{}", plural(n))`.
pub fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

pub fn mtime(p: &Path) -> Option<SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

pub fn human_bytes(b: u64) -> String {
    let b = b as f64;
    if b >= 1e9 {
        format!("{:.2} GB", b / 1e9)
    } else if b >= 1e6 {
        format!("{:.1} MB", b / 1e6)
    } else if b >= 1e3 {
        format!("{:.0} KB", b / 1e3)
    } else {
        format!("{b:.0} B")
    }
}

/// "just now", "3 min ago", "5 h ago", "yesterday", "4 days ago" ("—" when unknown).
pub fn ago(t: Option<SystemTime>) -> String {
    let Some(t) = t else { return "—".into() };
    let secs = SystemTime::now().duration_since(t).map(|d| d.as_secs()).unwrap_or(0);
    match secs {
        0..60 => "just now".into(),
        60..3600 => format!("{} min ago", secs / 60),
        3600..86_400 => format!("{} h ago", secs / 3600),
        86_400..172_800 => "yesterday".into(),
        _ => format!("{} days ago", secs / 86_400),
    }
}

/// A path shown short: relative to the working directory when inside it, else
/// with `~` for the home folder.
pub fn short(p: &Path) -> String {
    let cwd = std::env::current_dir().ok().and_then(|c| std::fs::canonicalize(c).ok());
    let abs = vg_core::paths::canon(p);
    match cwd.as_ref().and_then(|c| abs.strip_prefix(c).ok()) {
        Some(r) if r.as_os_str().is_empty() => ".".into(),
        Some(r) => r.display().to_string(),
        None => match std::env::var_os("HOME").map(PathBuf::from).and_then(|h| abs.strip_prefix(&h).ok().map(|r| r.to_path_buf())) {
            Some(r) => format!("~/{}", r.display()),
            None => abs.display().to_string(),
        },
    }
}
