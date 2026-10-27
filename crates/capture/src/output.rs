//! Encoding and saving captures.

use std::io::{BufWriter, Write as _};
use std::path::{Component, Path, PathBuf};

use platform_windows::shell::LocalTime;

use crate::frame::Frame;

pub const DEFAULT_FILE_TEMPLATE: &str = "{year}-{month}/CapShotX_{date}_{time}";

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

/// Expands a file-name template into a relative path (without extension).
///
/// Tokens: `{date}` (2026-10-27), `{time}` (14-05-09), `{year}`, `{month}`,
/// `{day}`, `{hour}`, `{minute}`, `{second}`, `{ms}`. `/` or `\` create
/// sub-folders. Characters Windows forbids in names become `_`.
pub fn render_template(template: &str, t: LocalTime) -> Result<PathBuf, String> {
    let expanded = template
        .replace("{date}", &format!("{:04}-{:02}-{:02}", t.year, t.month, t.day))
        .replace("{time}", &format!("{:02}-{:02}-{:02}", t.hour, t.minute, t.second))
        .replace("{year}", &format!("{:04}", t.year))
        .replace("{month}", &format!("{:02}", t.month))
        .replace("{day}", &format!("{:02}", t.day))
        .replace("{hour}", &format!("{:02}", t.hour))
        .replace("{minute}", &format!("{:02}", t.minute))
        .replace("{second}", &format!("{:02}", t.second))
        .replace("{ms}", &format!("{:03}", t.millisecond));

    let mut path = PathBuf::new();
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
        path.push(clean);
    }
    if path.as_os_str().is_empty() {
        return Err("File name is empty".into());
    }
    debug_assert!(path.components().all(|c| matches!(c, Component::Normal(_))));
    Ok(path)
}

/// Writes `png` under `folder` using `template`, never overwriting an
/// existing file. Writes to a temporary file first and renames on success.
pub fn save_png(png: &[u8], folder: &Path, template: &str, t: LocalTime) -> Result<PathBuf, String> {
    let relative = render_template(template, t)?;
    let base = folder.join(relative);
    let dir = base.parent().unwrap_or(folder);
    std::fs::create_dir_all(dir).map_err(|e| format!("Can't create {}: {e}", dir.display()))?;

    let stem = base.file_name().unwrap_or_default().to_string_lossy().into_owned();
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
    fn default_template() {
        assert_eq!(
            render_template(DEFAULT_FILE_TEMPLATE, T).unwrap(),
            PathBuf::from("2026-03").join("CapShotX_2026-03-07_09-05-02")
        );
    }

    #[test]
    fn sanitises_and_rejects() {
        assert_eq!(render_template("a:b?{ms}", T).unwrap(), PathBuf::from("a_b_042"));
        assert!(render_template("../x", T).is_err());
        assert!(render_template(" / ", T).is_err());
    }
}
