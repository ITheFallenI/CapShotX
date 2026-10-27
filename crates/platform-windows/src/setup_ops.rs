//! Win32 setup & uninstallation helpers: Known Folders, `.lnk` Shell shortcuts,
//! process shutdown for in-place upgrades, and `Add or Remove Programs` registry
//! registration.

use std::os::windows::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, WPARAM};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree, IPersistFile};
use windows::Win32::System::ProcessStatus::K32EnumProcesses;
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, REG_DWORD, REG_OPTION_NON_VOLATILE, REG_SZ,
    RRF_RT_REG_SZ, RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegGetValueW, RegSetValueExW,
};
use windows::Win32::System::Threading::{
    CREATE_NO_WINDOW, GetCurrentProcessId, OpenProcess, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
    QueryFullProcessImageNameW, TerminateProcess, WaitForSingleObject,
};
use windows::Win32::UI::Shell::{
    FOLDERID_Desktop, FOLDERID_LocalAppData, FOLDERID_Programs, IShellLinkW, KF_FLAG_DEFAULT,
    SHGetKnownFolderPath, ShellLink,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowThreadProcessId, PostMessageW, WM_CLOSE,
};
use windows::core::{BOOL, GUID, HSTRING, Interface as _, PCWSTR, PWSTR, w};

const UNINSTALL_SUBKEY: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Uninstall\CapShotX");

fn known_folder(id: &GUID) -> Option<PathBuf> {
    // SAFETY: valid known-folder GUID; returned PWSTR is freed with CoTaskMemFree.
    unsafe {
        let raw: PWSTR = SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None).ok()?;
        let s = raw.to_string().ok();
        CoTaskMemFree(Some(raw.0 as *const _));
        s.map(PathBuf::from)
    }
}

/// Default per-user install location (`%LOCALAPPDATA%\Programs\CapShotX`), or
/// the previous `InstallLocation` if CapShotX is already installed.
pub fn default_install_dir() -> PathBuf {
    if let Some(prev) = existing_install_dir() {
        return prev;
    }
    known_folder(&FOLDERID_LocalAppData)
        .or_else(|| std::env::var_os("LOCALAPPDATA").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from(r"C:\CapShotX"))
        .join("Programs")
        .join("CapShotX")
}

/// Returns the `InstallLocation` recorded by a previous CapShotX installation, if any.
pub fn existing_install_dir() -> Option<PathBuf> {
    let mut buf = [0u16; 1024];
    let mut bytes = (buf.len() * size_of::<u16>()) as u32;
    // SAFETY: valid stack buffer and byte length for REG_SZ query.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            UNINSTALL_SUBKEY,
            w!("InstallLocation"),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr() as *mut _),
            Some(&mut bytes),
        )
    };
    if status.is_err() || bytes < 2 {
        return None;
    }
    let units = (bytes as usize / 2).saturating_sub(1);
    let s = String::from_utf16_lossy(&buf[..units]);
    let trimmed = s.trim_end_matches('\0').trim();
    (!trimmed.is_empty()).then(|| PathBuf::from(trimmed))
}

/// Path to `CapShotX.lnk` on the user's Desktop.
pub fn desktop_shortcut_path() -> Option<PathBuf> {
    known_folder(&FOLDERID_Desktop).map(|d| d.join("CapShotX.lnk"))
}

/// Path to `CapShotX.lnk` in the user's Start Menu Programs folder.
pub fn start_menu_shortcut_path() -> Option<PathBuf> {
    known_folder(&FOLDERID_Programs).map(|d| d.join("CapShotX.lnk"))
}

/// Creates or overwrites a Windows `.lnk` shortcut via `IShellLinkW`.
pub fn create_shortcut(
    lnk_path: &Path,
    target_exe: &Path,
    working_dir: &Path,
    description: &str,
) -> Result<(), String> {
    crate::shell::init_process();
    if let Some(parent) = lnk_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Could not create {}: {e}", parent.display()))?;
    }
    let target = HSTRING::from(target_exe.as_os_str());
    let work = HSTRING::from(working_dir.as_os_str());
    let desc = HSTRING::from(description);
    let lnk = HSTRING::from(lnk_path.as_os_str());

    // SAFETY: COM apartment is initialised by `shell::init_process()`; all
    // HSTRINGs outlive their synchronous interface calls.
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)
            .map_err(|e| format!("Could not create ShellLink: {e}"))?;
        link.SetPath(&target)
            .map_err(|e| format!("SetPath failed: {e}"))?;
        link.SetWorkingDirectory(&work)
            .map_err(|e| format!("SetWorkingDirectory failed: {e}"))?;
        link.SetDescription(&desc)
            .map_err(|e| format!("SetDescription failed: {e}"))?;
        link.SetIconLocation(&target, 0)
            .map_err(|e| format!("SetIconLocation failed: {e}"))?;
        let persist: IPersistFile = link
            .cast()
            .map_err(|e| format!("IPersistFile cast failed: {e}"))?;
        persist
            .Save(&lnk, true)
            .map_err(|e| format!("Saving shortcut {} failed: {e}", lnk_path.display()))?;
    }
    Ok(())
}

/// Stops any running process whose executable file name matches `exe_file_name`
/// (case-insensitive, excluding the current process) so upgrades and uninstalls
/// can replace or delete the binary cleanly.
pub fn stop_running_processes(exe_file_name: &str) -> Result<(), String> {
    let mut pids = vec![0u32; 2048];
    let mut bytes_returned = 0u32;
    // SAFETY: valid buffer and byte length.
    let ok = unsafe {
        K32EnumProcesses(
            pids.as_mut_ptr(),
            (pids.len() * size_of::<u32>()) as u32,
            &mut bytes_returned,
        )
        .as_bool()
    };
    if !ok {
        return Ok(());
    }
    let count = bytes_returned as usize / size_of::<u32>();
    let self_pid = unsafe { GetCurrentProcessId() };

    for &pid in &pids[..count] {
        if pid == 0 || pid == self_pid {
            continue;
        }
        // SAFETY: queries image name on `pid` and closes the handle.
        unsafe {
            let Ok(query_handle) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
                continue;
            };
            let mut buf = [0u16; 1024];
            let mut len = buf.len() as u32;
            let got = QueryFullProcessImageNameW(
                query_handle,
                PROCESS_NAME_WIN32,
                PWSTR(buf.as_mut_ptr()),
                &mut len,
            )
            .is_ok();
            let _ = CloseHandle(query_handle);
            if !got {
                continue;
            }
            let path = PathBuf::from(String::from_utf16_lossy(&buf[..len as usize]));
            let matches = path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.eq_ignore_ascii_case(exe_file_name));
            if !matches {
                continue;
            }

            post_close_to_pid(pid);
            let Ok(term_handle) =
                OpenProcess(PROCESS_TERMINATE | PROCESS_SYNCHRONIZE, false, pid)
            else {
                return Err(format!(
                    "{exe_file_name} is currently running as Administrator. Please right-click its tray icon and choose 'Quit CapShotX', then click Install again."
                ));
            };
            if WaitForSingleObject(term_handle, 500).0 != 0 {
                let _ = TerminateProcess(term_handle, 0);
                let _ = WaitForSingleObject(term_handle, 2000);
            }
            let _ = CloseHandle(term_handle);
        }
    }
    Ok(())
}

unsafe fn post_close_to_pid(target_pid: u32) {
    unsafe extern "system" fn cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
        // SAFETY: posts WM_CLOSE to windows owned by `target_pid`.
        unsafe {
            let mut pid = 0u32;
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            if pid == lparam.0 as u32 {
                let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
            }
            BOOL::from(true)
        }
    }
    // SAFETY: synchronous callback during EnumWindows.
    unsafe {
        let _ = EnumWindows(Some(cb), LPARAM(target_pid as isize));
    }
}

/// Registers CapShotX under `HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\CapShotX`
/// so it appears in **Settings › Apps › Installed apps**.
pub fn register_uninstaller(
    install_dir: &Path,
    main_exe: &Path,
    uninstaller_exe: &Path,
    version: &str,
    estimated_size_kb: u32,
) -> Result<(), String> {
    let icon = format!("{},0", main_exe.display());
    let uninstall_cmd = format!("\"{}\" --uninstall", uninstaller_exe.display());
    let quiet_cmd = format!("\"{}\" --uninstall --silent", uninstaller_exe.display());
    let loc = install_dir.to_string_lossy();

    // SAFETY: creates/opens HKCU Uninstall\CapShotX, writes values, closes handle.
    unsafe {
        let mut hkey = HKEY::default();
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            UNINSTALL_SUBKEY,
            Some(0),
            None,
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            None,
            &mut hkey,
            None,
        )
        .ok()
        .map_err(|e| format!("Could not open Uninstall registry key: {e}"))?;

        let res = (|| -> Result<(), String> {
            set_sz(hkey, w!("DisplayName"), "CapShotX")?;
            set_sz(hkey, w!("DisplayVersion"), version)?;
            set_sz(hkey, w!("Publisher"), "CapShotX")?;
            set_sz(hkey, w!("DisplayIcon"), &icon)?;
            set_sz(hkey, w!("InstallLocation"), &loc)?;
            set_sz(hkey, w!("UninstallString"), &uninstall_cmd)?;
            set_sz(hkey, w!("QuietUninstallString"), &quiet_cmd)?;
            set_dword(hkey, w!("NoModify"), 1)?;
            set_dword(hkey, w!("NoRepair"), 1)?;
            set_dword(hkey, w!("EstimatedSize"), estimated_size_kb)?;
            Ok(())
        })();
        let _ = RegCloseKey(hkey);
        res
    }
}

/// Removes the `Installed apps` registry entry for CapShotX.
pub fn unregister_uninstaller() {
    // SAFETY: deletes HKCU Uninstall\CapShotX if present.
    unsafe {
        let _ = RegDeleteTreeW(HKEY_CURRENT_USER, UNINSTALL_SUBKEY);
    }
}

/// Launches `<exe>` cleanly detached from the setup process.
pub fn spawn_detached(exe: &Path) -> Result<(), String> {
    Command::new(exe)
        .current_dir(exe.parent().unwrap_or_else(|| Path::new(".")))
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("Could not launch {}: {e}", exe.display()))
}

/// Deletes a temporary uninstaller copy and empty install folder a moment after
/// the current process exits.
pub fn schedule_self_cleanup(temp_exe: &Path, install_dir: &Path) {
    let cmd_str = format!(
        "ping 127.0.0.1 -n 2 > nul & del /F /Q \"{}\" > nul 2>&1 & rmdir \"{}\" > nul 2>&1",
        temp_exe.display(),
        install_dir.display()
    );
    let _ = Command::new("cmd")
        .args(["/C", &cmd_str])
        .creation_flags(CREATE_NO_WINDOW.0)
        .spawn();
}

unsafe fn set_sz(hkey: HKEY, name: PCWSTR, value: &str) -> Result<(), String> {
    let mut wide: Vec<u16> = value.encode_utf16().collect();
    wide.push(0);
    // SAFETY: `wide` lives across `RegSetValueExW`.
    unsafe {
        let bytes = std::slice::from_raw_parts(wide.as_ptr() as *const u8, wide.len() * 2);
        RegSetValueExW(hkey, name, Some(0), REG_SZ, Some(bytes))
            .ok()
            .map_err(|e| format!("Registry write failed: {e}"))
    }
}

unsafe fn set_dword(hkey: HKEY, name: PCWSTR, value: u32) -> Result<(), String> {
    let bytes = value.to_le_bytes();
    // SAFETY: `bytes` is a 4-byte little-endian DWORD valid for this call.
    unsafe {
        RegSetValueExW(hkey, name, Some(0), REG_DWORD, Some(&bytes))
            .ok()
            .map_err(|e| format!("Registry DWORD write failed: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_default_install_and_shortcut_paths() {
        let dir = default_install_dir();
        assert!(dir.ends_with(r"Programs\CapShotX"));
        assert!(start_menu_shortcut_path().is_some());
        assert!(desktop_shortcut_path().is_some());
    }
}
