//! Snapshot of capturable top-level windows, in Z-order.

use std::ffi::c_void;

use platform_windows::monitors::PixelRect;
use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute};
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GWL_EXSTYLE, GetClassNameW, GetWindowLongW, GetWindowRect, GetWindowTextW,
    GetWindowThreadProcessId, IsIconic, IsWindowVisible, WS_EX_TRANSPARENT,
};
use windows::core::BOOL;

#[derive(Clone, Debug)]
pub struct WindowInfo {
    /// Visible frame (excludes the invisible resize border), physical pixels.
    pub rect: PixelRect,
    pub title: String,
    /// Raw `HWND`, for capturing the window itself (e.g. recording).
    pub hwnd: isize,
}

/// Visible, uncloaked top-level windows from front to back, excluding this
/// process, click-through overlays and the desktop itself.
pub fn visible_windows() -> Vec<WindowInfo> {
    unsafe extern "system" fn collect(hwnd: HWND, data: LPARAM) -> BOOL {
        // SAFETY: data is the &mut Vec passed by `visible_windows`, valid for
        // the synchronous EnumWindows call.
        unsafe {
            let out = &mut *(data.0 as *mut Vec<WindowInfo>);
            if let Some(info) = inspect(hwnd) {
                out.push(info);
            }
            BOOL::from(true)
        }
    }

    let mut windows: Vec<WindowInfo> = Vec::new();
    // SAFETY: the callback only runs during this call.
    unsafe {
        let _ = EnumWindows(Some(collect), LPARAM(&mut windows as *mut _ as isize));
    }
    windows
}

unsafe fn inspect(hwnd: HWND) -> Option<WindowInfo> {
    // SAFETY: read-only queries on a window handle from EnumWindows.
    unsafe {
        if !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() {
            return None;
        }
        let mut pid = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == GetCurrentProcessId() {
            return None;
        }
        let ex_style = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
        if ex_style & WS_EX_TRANSPARENT.0 != 0 {
            return None; // click-through overlays (game/GPU overlays etc.)
        }
        let mut cloaked = 0u32;
        let _ = DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            &mut cloaked as *mut _ as *mut c_void,
            size_of::<u32>() as u32,
        );
        if cloaked != 0 {
            return None; // other virtual desktops, suspended UWP apps
        }

        let mut class = [0u16; 64];
        let len = GetClassNameW(hwnd, &mut class) as usize;
        let class = String::from_utf16_lossy(&class[..len]);
        if class == "Progman" || class == "WorkerW" {
            return None; // the desktop: callers fall back to the monitor
        }

        let mut rect = RECT::default();
        if DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut rect as *mut _ as *mut c_void,
            size_of::<RECT>() as u32,
        )
        .is_err()
        {
            GetWindowRect(hwnd, &mut rect).ok()?;
        }
        let rect = PixelRect::from(rect);
        if rect.width() < 8 || rect.height() < 8 {
            return None;
        }

        let mut title = [0u16; 256];
        let len = GetWindowTextW(hwnd, &mut title) as usize;
        Some(WindowInfo {
            hwnd: hwnd.0 as isize,
            rect,
            title: String::from_utf16_lossy(&title[..len]),
        })
    }
}
