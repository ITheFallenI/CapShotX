//! File actions on library items.

use std::path::{Path, PathBuf};

/// Renames the file's stem, keeping its extension and folder.
pub fn rename(path: &Path, new_stem: &str) -> Result<PathBuf, String> {
    let stem = new_stem.trim();
    if stem.is_empty() {
        return Err("Name can't be empty".into());
    }
    if stem.chars().any(|c| "<>:\"/\\|?*".contains(c) || c.is_control()) || stem.ends_with('.') {
        return Err("Name can't contain < > : \" / \\ | ? * or end with a dot".into());
    }
    let mut target = path.with_file_name(stem);
    if let Some(ext) = path.extension() {
        target.set_extension(ext);
    }
    if target == path {
        return Ok(target);
    }
    if target.exists() {
        return Err(format!("\"{}\" already exists", target.file_name().unwrap_or_default().to_string_lossy()));
    }
    std::fs::rename(path, &target).map_err(|e| format!("Couldn't rename: {e}"))?;
    Ok(target)
}

/// Decodes a file to straight-alpha RGBA, scaled down so neither side
/// exceeds `max_side` (GPU texture limits).
pub fn load_full(path: &Path, max_side: u32) -> Result<(u32, u32, Vec<u8>), String> {
    let mut img = crate::open_visual(path)?;
    if img.width() > max_side || img.height() > max_side {
        img = img.resize(max_side, max_side, image::imageops::FilterType::Triangle);
    }
    let rgba = img.to_rgba8();
    Ok((rgba.width(), rgba.height(), rgba.into_raw()))
}

/// Puts the image file on the clipboard (as DIB + PNG).
pub fn copy_image(path: &Path) -> Result<(), String> {
    let img = image::open(path).map_err(|e| e.to_string())?.to_rgba8();
    let (w, h) = img.dimensions();
    let is_png = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("png"));
    let png = if is_png {
        std::fs::read(path).map_err(|e| e.to_string())?
    } else {
        let mut out = std::io::Cursor::new(Vec::new());
        img.write_to(&mut out, image::ImageFormat::Png).map_err(|e| e.to_string())?;
        out.into_inner()
    };
    let mut bgra = img.into_raw();
    for px in bgra.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
    platform_windows::clipboard::set_image(w, h, &bgra, &png).map_err(|e| format!("Couldn't copy: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rename_keeps_extension_and_validates() {
        let dir = std::env::temp_dir().join(format!("capshotx-rename-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.png");
        std::fs::write(&a, b"x").unwrap();
        std::fs::write(dir.join("taken.png"), b"x").unwrap();

        assert!(rename(&a, "  ").is_err());
        assert!(rename(&a, "bad:name").is_err());
        assert!(rename(&a, "taken").is_err());
        let b = rename(&a, "Bug report").unwrap();
        assert_eq!(b, dir.join("Bug report.png"));
        assert!(b.exists() && !a.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
