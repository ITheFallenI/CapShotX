//! Shell helpers: local time, opening files, Recycle Bin, folder picker.

use std::os::windows::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use windows::Win32::Foundation::{FILETIME, SYSTEMTIME};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree};
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};
use windows::Win32::UI::Shell::{
    FO_DELETE, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, FOS_FORCEFILESYSTEM,
    FOS_PICKFOLDERS, FileOpenDialog, IFileOpenDialog, IShellItem, SHCreateItemFromParsingName,
    SHFILEOPSTRUCTW, SHFileOperationW, SIGDN_FILESYSPATH, ShellExecuteW,
};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{HSTRING, PCWSTR, w};

/// Local wall-clock time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LocalTime {
    pub year: u16,
    pub month: u16,
    pub day: u16,
    pub hour: u16,
    pub minute: u16,
    pub second: u16,
    pub millisecond: u16,
    /// 0 = Sunday … 6 = Saturday.
    pub weekday: u16,
}

impl LocalTime {
    fn from_system(t: &SYSTEMTIME) -> Self {
        Self {
            year: t.wYear,
            month: t.wMonth,
            day: t.wDay,
            hour: t.wHour,
            minute: t.wMinute,
            second: t.wSecond,
            millisecond: t.wMilliseconds,
            weekday: t.wDayOfWeek,
        }
    }
}

pub fn local_time() -> LocalTime {
    // SAFETY: GetLocalTime has no preconditions.
    LocalTime::from_system(&unsafe { GetLocalTime() })
}

/// Converts a file timestamp to local wall-clock time.
pub fn local_time_of(time: SystemTime) -> Option<LocalTime> {
    // FILETIME counts 100 ns intervals since 1601-01-01 (UTC).
    const UNIX_TO_1601: u64 = 11_644_473_600;
    let since_unix = time.duration_since(UNIX_EPOCH).ok()?;
    let ticks = (since_unix.as_secs() + UNIX_TO_1601) * 10_000_000 + since_unix.subsec_nanos() as u64 / 100;
    let file_time = FILETIME { dwLowDateTime: ticks as u32, dwHighDateTime: (ticks >> 32) as u32 };
    let (mut utc, mut local) = (SYSTEMTIME::default(), SYSTEMTIME::default());
    // SAFETY: valid in/out pointers; None selects the current time zone.
    unsafe {
        FileTimeToSystemTime(&file_time, &mut utc).ok()?;
        SystemTimeToTzSpecificLocalTime(None, &utc, &mut local).ok()?;
    }
    Some(LocalTime::from_system(&local))
}

/// Moves a file to the Recycle Bin (undoable), without Windows' own prompts.
pub fn recycle(path: &Path) -> Result<(), String> {
    // SHFileOperation wants a double-null-terminated list.
    let mut from: Vec<u16> = path.as_os_str().encode_wide().collect();
    from.extend([0, 0]);
    let mut op = SHFILEOPSTRUCTW {
        wFunc: FO_DELETE,
        pFrom: PCWSTR(from.as_ptr()),
        fFlags: (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_SILENT | FOF_NOERRORUI).0 as u16,
        ..Default::default()
    };
    // SAFETY: `from` outlives the call; the struct is fully initialised.
    let code = unsafe { SHFileOperationW(&mut op) };
    if code != 0 || op.fAnyOperationsAborted.as_bool() {
        return Err(format!("Couldn't move {} to the Recycle Bin (code {code})", path.display()));
    }
    Ok(())
}

/// Opens a file or folder with its default handler.
pub fn open(path: &Path) {
    let path = HSTRING::from(path.as_os_str());
    // SAFETY: valid null-terminated strings; failure is non-fatal.
    unsafe {
        ShellExecuteW(None, w!("open"), &path, None, None, SW_SHOWNORMAL);
    }
}

/// Opens Explorer with `path` selected.
pub fn reveal(path: &Path) {
    let _ = std::process::Command::new("explorer")
        .arg(format!("/select,{}", path.display()))
        .spawn();
}

/// Shows the standard folder picker. Blocks until closed; returns `None` if
/// cancelled. Call from a thread with COM initialised (the UI thread is).
pub fn pick_folder(initial: Option<&Path>) -> Option<PathBuf> {
    // SAFETY: COM objects are owned by smart pointers; the returned path
    // string is freed with CoTaskMemFree.
    unsafe {
        let dialog: IFileOpenDialog =
            CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let options = dialog.GetOptions().ok()?;
        dialog
            .SetOptions(options | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM)
            .ok()?;
        if let Some(initial) = initial.filter(|p| p.is_dir()) {
            let path = HSTRING::from(initial.as_os_str());
            if let Ok(item) = SHCreateItemFromParsingName::<_, _, IShellItem>(&path, None) {
                let _ = dialog.SetFolder(&item);
            }
        }
        dialog.Show(None).ok()?;
        let item = dialog.GetResult().ok()?;
        let name = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let path = name.to_string().ok();
        CoTaskMemFree(Some(name.0 as *const _));
        path.map(PathBuf::from)
    }
}
