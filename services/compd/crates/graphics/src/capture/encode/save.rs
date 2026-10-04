//! PNG image save + default save destinations.

use std::path::{Path, PathBuf};

use crate::capture::encode::frame::Frame;

/// Encode `frame` as a PNG at `path` (BGRA→RGBA swap). Creates parent dirs.
pub fn save_png(frame: &Frame, path: &Path) -> std::io::Result<()> {
    let mut rgba = frame.bgra.clone();
    for px in rgba.chunks_exact_mut(4) {
        px.swap(0, 2); // BGRA -> RGBA
    }
    let img = image::RgbaImage::from_raw(frame.width, frame.height, rgba).ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, "frame dims/buffer mismatch")
    })?;
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    img.save_with_format(path, image::ImageFormat::Png)
        .map_err(|e| std::io::Error::other(e.to_string()))
}

/// Default save directory for the media kind (XDG user dir, else ~/Pictures|Videos).
pub fn default_dir(video: bool) -> PathBuf {
    let key = if video {
        "XDG_VIDEOS_DIR"
    } else {
        "XDG_PICTURES_DIR"
    };
    if let Ok(d) = std::env::var(key) {
        if !d.is_empty() {
            return PathBuf::from(d);
        }
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    PathBuf::from(home).join(if video { "Videos" } else { "Pictures" })
}

/// Timestamped default save path, e.g. `~/Pictures/compd-capture-1718000000.png`.
pub fn default_path(video: bool) -> PathBuf {
    let ext = if video { "mp4" } else { "png" };
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    default_dir(video).join(format!("compd-capture-{ts}.{ext}"))
}
