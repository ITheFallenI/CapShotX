//! Manages the optional standalone `CapShotX_Editor.exe` add-on binary from
//! the main `CapShotX.exe` app, keeping `CapShotX.exe` capture/record-only.

use std::path::{Path, PathBuf};

pub const EDITOR_EXE_NAME: &str = "CapShotX_Editor.exe";

/// What the user wanted to open in the Image Editor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditorLaunchTarget {
    Empty,
    Paste,
    File(PathBuf),
    TempBgra(PathBuf),
}

/// Resolves the path where `CapShotX_Editor.exe` lives (or should be installed):
/// right next to the running `CapShotX.exe`.
pub fn editor_exe_path() -> Option<PathBuf> {
    let cur = std::env::current_exe().ok()?;
    let dir = cur.parent()?;
    let adjacent = dir.join(EDITOR_EXE_NAME);
    if adjacent.exists() {
        return Some(adjacent);
    }
    if let Some(install_dir) = platform_windows::setup_ops::existing_install_dir() {
        let installed = install_dir.join(EDITOR_EXE_NAME);
        if installed.exists() {
            return Some(installed);
        }
    }
    Some(adjacent)
}

/// Returns `true` if `CapShotX_Editor.exe` is currently installed on disk.
pub fn is_installed() -> bool {
    editor_exe_path().is_some_and(|p| p.exists())
}

/// Returns the file size in bytes of `CapShotX_Editor.exe` if installed.
pub fn installed_size_bytes() -> Option<u64> {
    let path = editor_exe_path()?;
    std::fs::metadata(path).ok().map(|m| m.len())
}

/// Removes `CapShotX_Editor.exe` from disk if installed.
pub fn uninstall_editor() -> Result<(), String> {
    if let Some(path) = editor_exe_path()
        && path.exists()
    {
        std::fs::remove_file(&path)
            .map_err(|e| format!("Could not remove {}: {e}", path.display()))?;
    }
    Ok(())
}

/// Writes a captured top-down `bgra` buffer to `%TEMP%\CapShotX_edit_<pid>_<ts>.bgra`
/// so it can be passed instantaneously to `CapShotX_Editor.exe` (even after an
/// on-demand download completes).
pub fn write_temp_bgra(width: u32, height: u32, bgra: &[u8]) -> Option<PathBuf> {
    let mut temp_path = std::env::temp_dir();
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    temp_path.push(format!(
        "CapShotX_edit_{}_{}.bgra",
        std::process::id(),
        stamp
    ));
    let mut payload = Vec::with_capacity(8 + bgra.len());
    payload.extend_from_slice(&width.to_le_bytes());
    payload.extend_from_slice(&height.to_le_bytes());
    payload.extend_from_slice(bgra);
    if let Err(err) = std::fs::write(&temp_path, &payload) {
        tracing::error!("failed to write temp editor buffer: {err}");
        return None;
    }
    Some(temp_path)
}

/// Spawns `CapShotX_Editor.exe` with `target` if it is installed on disk.
/// Returns `true` if launched, or `false` if `CapShotX_Editor.exe` is not installed yet.
pub fn launch_if_installed(target: &EditorLaunchTarget) -> bool {
    let Some(exe) = editor_exe_path() else {
        return false;
    };
    if !exe.exists() {
        return false;
    }
    let mut cmd = std::process::Command::new(&exe);
    match target {
        EditorLaunchTarget::Empty => {
            cmd.arg("--editor");
        }
        EditorLaunchTarget::Paste => {
            cmd.arg("--editor-paste");
        }
        EditorLaunchTarget::File(path) => {
            cmd.arg("--editor-file").arg(path);
        }
        EditorLaunchTarget::TempBgra(temp_path) => {
            cmd.arg("--editor-bgra").arg(temp_path);
        }
    }
    cmd.spawn().is_ok()
}

/// Convenience helper for building a file target.
pub fn target_for_path(path: &Path) -> EditorLaunchTarget {
    EditorLaunchTarget::File(path.to_path_buf())
}
