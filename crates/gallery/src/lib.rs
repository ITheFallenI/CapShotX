//! Screenshot library: folder index, lazy thumbnails and file actions.
//!
//! Works on the original files in the capture folder — nothing is imported
//! into a database (GAL-02). UI-agnostic: thumbnails come back as RGBA.

mod mp4_dur;
mod ops;
mod thumbs;

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use platform_windows::shell::{LocalTime, local_time, local_time_of};

pub use ops::{copy_image, load_full, rename};
pub use thumbs::{Thumbnail, Thumbnailer, warm_cache};

const EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "bmp", "webp", "mp4"];
const VIDEO_EXTENSIONS: &[&str] = &["mp4"];

/// Recordings (shown with their first frame; opened in the video player).
pub fn is_video(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| VIDEO_EXTENSIONS.iter().any(|x| e.eq_ignore_ascii_case(x)))
}

/// Probes the duration of a video file from its MP4 header (fast, pure Rust
/// first so Media Foundation codecs aren't loaded just for cached thumbnails).
pub fn video_duration(path: &Path) -> Option<std::time::Duration> {
    if !is_video(path) {
        return None;
    }
    mp4_dur::parse(path).or_else(|| {
        media::probe(path)
            .ok()
            .filter(|i| i.duration > 0)
            .map(|i| std::time::Duration::from_nanos((i.duration * 100) as u64))
    })
}

/// Decodes an image, or a video's first frame.
fn open_visual(path: &Path) -> Result<image::DynamicImage, String> {
    if is_video(path) {
        let (w, h, rgba) = media::poster_frame(path).map_err(|e| e.message())?;
        image::RgbaImage::from_raw(w, h, rgba)
            .map(image::DynamicImage::ImageRgba8)
            .ok_or_else(|| "bad video frame".to_string())
    } else {
        image::open(path).map_err(|e| e.to_string())
    }
}
/// Captures use dated + Images/Video sub-folders; don't wander deeper than this.
const MAX_DEPTH: usize = 6;

#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub path: PathBuf,
    pub name: String,
    pub modified: SystemTime,
    pub size: u64,
    pub local: LocalTime,
}

impl Entry {
    /// Cache key that changes whenever the file does.
    pub fn key(&self) -> ThumbKey {
        ThumbKey {
            path: self.path.clone(),
            modified: self.modified,
            size: self.size,
        }
    }

    pub fn day(&self) -> (u16, u16, u16) {
        (self.local.year, self.local.month, self.local.day)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ThumbKey {
    pub path: PathBuf,
    pub modified: SystemTime,
    pub size: u64,
}

/// Lists images under `folder`, newest first. Missing folder → empty.
/// Cheap (directory metadata only); run it off the UI thread anyway.
pub fn scan(folder: &Path) -> Vec<Entry> {
    let mut entries = Vec::new();
    walk(folder, 0, &mut entries);
    entries.sort_by(|a, b| b.modified.cmp(&a.modified).then_with(|| a.name.cmp(&b.name)));
    entries
}

fn walk(dir: &Path, depth: usize, out: &mut Vec<Entry>) {
    let Ok(read) = std::fs::read_dir(dir) else { return };
    for item in read.flatten() {
        let path = item.path();
        let Ok(meta) = item.metadata() else { continue };
        if meta.is_dir() {
            if depth < MAX_DEPTH {
                walk(&path, depth + 1, out);
            }
            continue;
        }
        let is_image = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| EXTENSIONS.iter().any(|x| e.eq_ignore_ascii_case(x)));
        if !is_image {
            continue;
        }
        let modified = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        let Some(local) = local_time_of(modified) else { continue };
        out.push(Entry {
            name: path.file_name().unwrap_or_default().to_string_lossy().into_owned(),
            path,
            modified,
            size: meta.len(),
            local,
        });
    }
}

const MONTHS: [&str; 12] = [
    "January", "February", "March", "April", "May", "June", "July", "August", "September",
    "October", "November", "December",
];
const WEEKDAYS: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];

/// "Today", "Yesterday", "Monday 26 October" or "Monday 26 October 2025".
pub fn day_label(t: &LocalTime) -> String {
    let now = local_time();
    let days_ago = days_from_civil(now.year, now.month, now.day) - days_from_civil(t.year, t.month, t.day);
    match days_ago {
        0 => "Today".into(),
        1 => "Yesterday".into(),
        _ => {
            let weekday = WEEKDAYS.get(t.weekday as usize).copied().unwrap_or("");
            let month = MONTHS.get(t.month.saturating_sub(1) as usize).copied().unwrap_or("");
            if t.year == now.year {
                format!("{weekday} {} {month}", t.day)
            } else {
                format!("{weekday} {} {month} {}", t.day, t.year)
            }
        }
    }
}

/// Days since 1970-01-01 for a proleptic Gregorian date.
fn days_from_civil(y: u16, m: u16, d: u16) -> i64 {
    let (y, m, d) = (y as i64 - (m <= 2) as i64, m as i64, d as i64);
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// "556 KB", "1.4 MB".
pub fn human_size(bytes: u64) -> String {
    match bytes {
        b if b < 1024 => format!("{b} B"),
        b if b < 1024 * 1024 => format!("{} KB", b / 1024),
        b => format!("{:.1} MB", b as f64 / (1024.0 * 1024.0)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_days() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2000, 3, 1), 11_017);
        assert_eq!(days_from_civil(2026, 10, 27) - days_from_civil(2026, 10, 26), 1);
        assert_eq!(days_from_civil(2026, 1, 1) - days_from_civil(2025, 12, 31), 1);
    }

    #[test]
    fn sizes() {
        assert_eq!(human_size(900), "900 B");
        assert_eq!(human_size(556_803), "543 KB");
        assert_eq!(human_size(3 * 1024 * 1024 + 400_000), "3.4 MB");
    }

    #[test]
    fn scan_finds_nested_images_and_videos_newest_first() {
        let dir = std::env::temp_dir().join(format!("capshotx-gallery-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("2026-10").join("Images")).unwrap();
        std::fs::create_dir_all(dir.join("2026-10").join("Video")).unwrap();
        std::fs::write(dir.join("old.png"), b"x").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(dir.join("2026-10").join("Images").join("new.png"), b"x").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(dir.join("2026-10").join("Video").join("clip.mp4"), b"x").unwrap();
        std::fs::write(dir.join("notes.txt"), b"x").unwrap();
        std::fs::write(dir.join("half.png.partial"), b"x").unwrap();
        std::fs::write(dir.join("2026-10").join("Video").join("crash.mp4.partial"), b"x").unwrap();

        let names: Vec<String> = scan(&dir).into_iter().map(|e| e.name).collect();
        assert_eq!(names, ["clip.mp4", "new.png", "old.png"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
