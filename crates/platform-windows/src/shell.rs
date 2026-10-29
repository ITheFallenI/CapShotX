//! Shell helpers: local time, opening files, Recycle Bin, folder picker,
//! process initialisation, elevation, tray message pumping and working-set trimming.

use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use windows::Win32::Foundation::{CloseHandle, FILETIME, HANDLE, LPARAM, POINT, SYSTEMTIME, WPARAM};
use windows::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree};
use windows::Win32::System::Memory::{GetProcessHeap, HEAP_FLAGS, HeapCompact};
use windows::Win32::System::Ole::OleInitialize;
use windows::Win32::System::ProcessStatus::{
    K32EmptyWorkingSet, K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
};
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::System::Threading::{GetCurrentProcess, GetCurrentThreadId, OpenProcessToken};
use windows::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
use windows::Win32::UI::Shell::{
    FO_DELETE, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, FOS_FORCEFILESYSTEM,
    FOS_OVERWRITEPROMPT, FOS_PATHMUSTEXIST, FOS_PICKFOLDERS, FileOpenDialog, FileSaveDialog,
    IFileOpenDialog, IFileSaveDialog, IShellItem, SHCreateItemFromParsingName, SHELLEXECUTEINFOW,
    SHFILEOPSTRUCTW, SHFileOperationW, SIGDN_FILESYSPATH, ShellExecuteExW, ShellExecuteW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetCursorPos, GetMessagePos, GetMessageW, MSG, PM_REMOVE, PeekMessageW,
    PostThreadMessageW, SW_SHOWNORMAL, TranslateMessage, WM_NULL, WM_QUIT,
};
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

/// Shows the standard Open file dialog filtered to image files (`*.png`,
/// `*.jpg`, `*.jpeg`, `*.bmp`, `*.webp`).
pub fn pick_open_image(initial: Option<&Path>) -> Option<PathBuf> {
    unsafe {
        let dialog: IFileOpenDialog =
            CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let options = dialog.GetOptions().ok()?;
        dialog
            .SetOptions(options | FOS_FORCEFILESYSTEM | FOS_PATHMUSTEXIST)
            .ok()?;
        let filters = [
            COMDLG_FILTERSPEC {
                pszName: w!("Images (*.png;*.jpg;*.jpeg;*.bmp;*.webp)"),
                pszSpec: w!("*.png;*.jpg;*.jpeg;*.bmp;*.webp"),
            },
            COMDLG_FILTERSPEC {
                pszName: w!("All files (*.*)"),
                pszSpec: w!("*.*"),
            },
        ];
        let _ = dialog.SetFileTypes(&filters);
        let _ = dialog.SetFileTypeIndex(1);
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

/// Shows the standard Save As dialog for an image (`PNG` or `JPG`).
pub fn pick_save_image(
    initial_dir: Option<&Path>,
    default_name: &str,
    is_jpg: bool,
) -> Option<PathBuf> {
    unsafe {
        let dialog: IFileSaveDialog =
            CoCreateInstance(&FileSaveDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let options = dialog.GetOptions().ok()?;
        dialog
            .SetOptions(options | FOS_FORCEFILESYSTEM | FOS_OVERWRITEPROMPT | FOS_PATHMUSTEXIST)
            .ok()?;
        let filters = if is_jpg {
            [
                COMDLG_FILTERSPEC {
                    pszName: w!("JPEG image (*.jpg)"),
                    pszSpec: w!("*.jpg;*.jpeg"),
                },
                COMDLG_FILTERSPEC {
                    pszName: w!("PNG image (*.png)"),
                    pszSpec: w!("*.png"),
                },
            ]
        } else {
            [
                COMDLG_FILTERSPEC {
                    pszName: w!("PNG image (*.png)"),
                    pszSpec: w!("*.png"),
                },
                COMDLG_FILTERSPEC {
                    pszName: w!("JPEG image (*.jpg)"),
                    pszSpec: w!("*.jpg;*.jpeg"),
                },
            ]
        };
        let _ = dialog.SetFileTypes(&filters);
        let _ = dialog.SetFileTypeIndex(1);
        let _ = dialog.SetDefaultExtension(if is_jpg { w!("jpg") } else { w!("png") });
        let fname = HSTRING::from(default_name);
        let _ = dialog.SetFileName(&fname);
        if let Some(initial) = initial_dir.filter(|p| p.is_dir()) {
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

/// Screen cursor coordinates `(x, y)`, falling back to `GetMessagePos` if UIPI
/// blocks `GetCursorPos` while an elevated window has focus.
pub fn cursor_pos() -> (i32, i32) {
    let mut pt = POINT::default();
    // SAFETY: valid out-pointer; GetMessagePos has no preconditions.
    unsafe {
        if GetCursorPos(&mut pt).is_ok() {
            (pt.x, pt.y)
        } else {
            let packed = GetMessagePos();
            ((packed & 0xffff) as i16 as i32, ((packed >> 16) & 0xffff) as i16 as i32)
        }
    }
}

/// Returns `true` when this process holds an elevated (Administrator / High IL) token.
pub fn is_elevated() -> bool {
    // SAFETY: queries TokenElevation on the current process token and closes the handle.
    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION::default();
        let mut ret_len = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            Some(&mut elevation as *mut _ as *mut c_void),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut ret_len,
        )
        .is_ok();
        let _ = CloseHandle(token);
        ok && elevation.TokenIsElevated != 0
    }
}

/// Spawns a new instance of the current executable with Administrator privileges
/// (`ShellExecuteExW` `"runas"`). Returns `Err` if the UAC prompt is cancelled.
pub fn relaunch_as_admin(args: &[&str]) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| format!("current_exe failed: {e}"))?;
    let file = HSTRING::from(exe.as_os_str());
    let joined = args
        .iter()
        .map(|a| {
            if a.contains(' ') || a.contains('"') {
                format!("\"{}\"", a.replace('"', "\\\""))
            } else {
                (*a).to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
    let params = HSTRING::from(joined);
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        lpVerb: w!("runas"),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: if args.is_empty() { PCWSTR::null() } else { PCWSTR(params.as_ptr()) },
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    // SAFETY: `file` and `params` outlive the synchronous ShellExecuteExW call.
    unsafe { ShellExecuteExW(&mut info).map_err(|e| e.message()) }
}

/// Sets Per-Monitor V2 DPI awareness and apartment COM on the main thread
/// before any hidden tray or single-instance window is created, so starting
/// straight to the tray behaves identically to opening the UI immediately.
pub fn init_process() {
    // SAFETY: both calls have no pointer preconditions and harmlessly report
    // an error if already set on this process/thread.
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let _ = OleInitialize(None);
    }
}

/// OS identifier of the calling thread, for [`wake_thread`].
pub fn current_thread_id() -> u32 {
    // SAFETY: GetCurrentThreadId has no preconditions.
    unsafe { GetCurrentThreadId() }
}

/// Posts a harmless `WM_NULL` to wake a thread blocked in [`pump_messages_once`].
pub fn wake_thread(thread_id: u32) {
    // SAFETY: posting WM_NULL to a thread ID is safe even if it has no queue yet.
    unsafe {
        let _ = PostThreadMessageW(thread_id, WM_NULL, WPARAM(0), LPARAM(0));
    }
}

/// Sleeps until at least one Win32 message arrives on this thread, then
/// dispatches all queued messages (tray icon clicks, popup menu events,
/// `TaskbarCreated`, wakeups). Returns `false` only on `WM_QUIT`.
pub fn pump_messages_once() -> bool {
    // SAFETY: standard Win32 message pump on the owning thread's queue.
    unsafe {
        let mut msg = MSG::default();
        if GetMessageW(&mut msg, None, 0, 0).0 <= 0 {
            return false;
        }
        let _ = TranslateMessage(&msg);
        DispatchMessageW(&msg);
        while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
            if msg.message == WM_QUIT {
                return false;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        true
    }
}

/// Whether Windows is currently set to Dark app mode.
pub fn system_prefers_dark() -> bool {
    let mut light: u32 = 0;
    let mut size = size_of::<u32>() as u32;
    // SAFETY: `light` and `size` outlive the synchronous registry read.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"),
            w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut light as *mut u32 as *mut c_void),
            Some(&mut size),
        )
    };
    if status.is_ok() { light == 0 } else { true }
}

/// Compacts the process heap and asks Windows to reclaim unused pages from the
/// working set (e.g. after closing the UI window to the tray or finishing a
/// capture). Active pages fault softly back from RAM if touched again.
pub fn trim_working_set() {
    // SAFETY: operates on the current process and its default heap.
    unsafe {
        if let Ok(heap) = GetProcessHeap() {
            let _ = HeapCompact(heap, HEAP_FLAGS(0));
        }
        let _ = K32EmptyWorkingSet(GetCurrentProcess());
    }
}

/// Current resident working set of this process in bytes, if queryable.
pub fn working_set_bytes() -> Option<usize> {
    let mut pmc = PROCESS_MEMORY_COUNTERS {
        cb: size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
        ..Default::default()
    };
    // SAFETY: `pmc` is valid and `cb` matches its exact byte size.
    unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut pmc, pmc.cb).ok().ok()? };
    Some(pmc.WorkingSetSize)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wake_and_pump_and_trim_work() {
        let tid = current_thread_id();
        assert_ne!(tid, 0);
        assert!(working_set_bytes().unwrap_or(0) > 0);
        let _ = system_prefers_dark();
        let _ = is_elevated();
        let _ = cursor_pos();
        wake_thread(tid);
        assert!(pump_messages_once());
        trim_working_set();
    }
}
