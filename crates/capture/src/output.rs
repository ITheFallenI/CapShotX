//! Encoding and saving captures.

use std::io::{BufWriter, Write as _};
use std::path::{Component, Path, PathBuf};

use platform_windows::shell::LocalTime;

use crate::frame::Frame;

pub const DEFAULT_FILE_TEMPLATE: &str = "{year}-{month}-{day}/{type}/CapShotX_{date}_{time}";
pub const LEGACY_DEFAULT_FILE_TEMPLATE: &str = "{year}-{month}/CapShotX_{date}_{time}";
pub const LEGACY_DEFAULT_FILE_TEMPLATES: &[&str] = &[
    "{year}-{month}/CapShotX_{date}_{time}",
    "{year}-{month}/{type}/CapShotX_{date}_{time}",
];

/// Media category used to route captures into `Images` or `Video` sub-folders.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputKind {
    Image,
    Video,
}

impl OutputKind {
    pub fn folder(self) -> &'static str {
        match self {
            Self::Image => "Images",
            Self::Video => "Video",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Self::Image => "png",
            Self::Video => "mp4",
        }
    }
}

/// Encodes as an opaque RGB PNG with fast compression (screenshots compress
/// well already; speed matters more for the copy path).
pub fn encode_png(frame: &Frame) -> Result<Vec<u8>, png::EncodingError> {
    let mut rgb = Vec::with_capacity(frame.bgra.len() / 4 * 3);
    for px in frame.bgra.chunks_exact(4) {
        rgb.extend_from_slice(&[px[2], px[1], px[0]]);
    }
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, frame.width(), frame.height());
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_compression(png::Compression::Fast);
        let mut writer = encoder.write_header()?;
        writer.write_image_data(&rgb)?;
    }
    Ok(out)
}

/// Expands a file-name template for `kind` into a relative path (with
/// extension), routing images into `Images` and recordings into `Video`
/// inside the configured date folder.
///
/// Tokens: `{type}` (`Images` or `Video`), `{date}` (2026-10-27), `{time}`
/// (14-05-09), `{year}`, `{month}`, `{day}`, `{hour}`, `{minute}`, `{second}`,
/// `{ms}`. `/` or `\` create sub-folders. If `{type}` is omitted from the
/// template, `Images` / `Video` is placed automatically inside the template's
/// folder. Characters Windows forbids in names become `_`.
pub fn render_output_path(template: &str, kind: OutputKind, t: LocalTime) -> Result<PathBuf, String> {
    let has_type = template.contains("{type}");
    let expanded = template
        .replace("{type}", kind.folder())
        .replace("{date}", &format!("{:04}-{:02}-{:02}", t.year, t.month, t.day))
        .replace("{time}", &format!("{:02}-{:02}-{:02}", t.hour, t.minute, t.second))
        .replace("{year}", &format!("{:04}", t.year))
        .replace("{month}", &format!("{:02}", t.month))
        .replace("{day}", &format!("{:02}", t.day))
        .replace("{hour}", &format!("{:02}", t.hour))
        .replace("{minute}", &format!("{:02}", t.minute))
        .replace("{second}", &format!("{:02}", t.second))
        .replace("{ms}", &format!("{:03}", t.millisecond));

    let mut parts = Vec::new();
    for part in expanded.split(['/', '\\']) {
        if part.trim() == ".." {
            return Err("File name can't contain \"..\"".into());
        }
        let clean: String = part
            .chars()
            .map(|c| if "<>:\"|?*".contains(c) || c.is_control() { '_' } else { c })
            .collect();
        let clean = clean.trim().trim_end_matches('.').to_owned();
        if clean.is_empty() {
            continue;
        }
        parts.push(clean);
    }
    let Some(stem) = parts.pop() else {
        return Err("File name is empty".into());
    };
    if !has_type {
        let already_typed = parts
            .last()
            .is_some_and(|p| p.eq_ignore_ascii_case("images") || p.eq_ignore_ascii_case("video") || p.eq_ignore_ascii_case("videos"));
        if !already_typed {
            parts.push(kind.folder().to_owned());
        }
    }

    let mut path = PathBuf::new();
    for dir in parts {
        path.push(dir);
    }
    path.push(format!("{stem}.{}", kind.extension()));
    debug_assert!(path.components().all(|c| matches!(c, Component::Normal(_))));
    Ok(path)
}

/// Expands a file-name template for an image (without extension).
pub fn render_template(template: &str, t: LocalTime) -> Result<PathBuf, String> {
    let mut p = render_output_path(template, OutputKind::Image, t)?;
    p.set_extension("");
    Ok(p)
}

/// Writes `png` under `folder` using `template`, never overwriting an
/// existing file. Writes to a temporary file first and renames on success.
pub fn save_png(png: &[u8], folder: &Path, template: &str, t: LocalTime) -> Result<PathBuf, String> {
    let relative = render_output_path(template, OutputKind::Image, t)?;
    let base = folder.join(relative);
    let dir = base.parent().unwrap_or(folder);
    std::fs::create_dir_all(dir).map_err(|e| format!("Can't create {}: {e}", dir.display()))?;

    let stem = base.file_stem().unwrap_or_default().to_string_lossy().into_owned();
    let path = (1..)
        .map(|n| {
            if n == 1 {
                dir.join(format!("{stem}.png"))
            } else {
                dir.join(format!("{stem} ({n}).png"))
            }
        })
        .find(|p| !p.exists())
        .expect("unbounded iterator");

    let partial = path.with_extension("png.partial");
    let write = || -> std::io::Result<()> {
        let mut file = BufWriter::new(std::fs::File::create(&partial)?);
        file.write_all(png)?;
        file.into_inner()?.sync_all()?;
        std::fs::rename(&partial, &path)
    };
    write().map_err(|e| {
        let _ = std::fs::remove_file(&partial);
        format!("Can't save to {}: {e}", path.display())
    })?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: LocalTime = LocalTime {
        year: 2026,
        month: 3,
        day: 7,
        hour: 9,
        minute: 5,
        second: 2,
        millisecond: 42,
        weekday: 6,
    };

    #[test]
    fn default_template_routes_images_and_videos_under_date_folder() {
        assert_eq!(
            render_output_path(DEFAULT_FILE_TEMPLATE, OutputKind::Image, T).unwrap(),
            PathBuf::from("2026-03-07").join("Images").join("CapShotX_2026-03-07_09-05-02.png")
        );
        assert_eq!(
            render_output_path(DEFAULT_FILE_TEMPLATE, OutputKind::Video, T).unwrap(),
            PathBuf::from("2026-03-07").join("Video").join("CapShotX_2026-03-07_09-05-02.mp4")
        );
    }

    #[test]
    fn templates_without_type_token_still_route_into_media_subfolders() {
        assert_eq!(
            render_output_path(LEGACY_DEFAULT_FILE_TEMPLATE, OutputKind::Image, T).unwrap(),
            PathBuf::from("2026-03").join("Images").join("CapShotX_2026-03-07_09-05-02.png")
        );
        assert_eq!(
            render_output_path("{date}/CapShotX_{time}", OutputKind::Video, T).unwrap(),
            PathBuf::from("2026-03-07").join("Video").join("CapShotX_09-05-02.mp4")
        );
    }

    #[test]
    fn sanitises_and_rejects() {
        assert_eq!(
            render_output_path("a:b?{ms}", OutputKind::Image, T).unwrap(),
            PathBuf::from("Images").join("a_b_042.png")
        );
        assert!(render_output_path("../x", OutputKind::Image, T).is_err());
        assert!(render_output_path(" / ", OutputKind::Video, T).is_err());
    }
}
