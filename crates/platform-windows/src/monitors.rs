//! Display enumeration in physical pixels, with per-monitor DPI.

use windows::Win32::Devices::Display::{
    DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
    DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO, DISPLAYCONFIG_SOURCE_DEVICE_NAME,
    DISPLAYCONFIG_TARGET_DEVICE_NAME, DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes,
    QDC_ONLY_ACTIVE_PATHS, QueryDisplayConfig,
};
use windows::Win32::Foundation::{ERROR_SUCCESS, LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFOEXW,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::MONITORINFOF_PRIMARY;
use windows::core::BOOL;

/// A rectangle in virtual-desktop physical pixels. Coordinates may be
/// negative for displays left of / above the primary display.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PixelRect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl PixelRect {
    pub fn width(&self) -> i32 {
        self.right - self.left
    }

    pub fn height(&self) -> i32 {
        self.bottom - self.top
    }

    pub fn is_empty(&self) -> bool {
        self.width() <= 0 || self.height() <= 0
    }

    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.left && x < self.right && y >= self.top && y < self.bottom
    }

    pub fn intersect(&self, other: &PixelRect) -> Option<PixelRect> {
        let r = PixelRect {
            left: self.left.max(other.left),
            top: self.top.max(other.top),
            right: self.right.min(other.right),
            bottom: self.bottom.min(other.bottom),
        };
        (!r.is_empty()).then_some(r)
    }

    /// The rectangle covering both pixels `a` and `b` (inclusive).
    pub fn spanning(a: (i32, i32), b: (i32, i32)) -> PixelRect {
        PixelRect {
            left: a.0.min(b.0),
            top: a.1.min(b.1),
            right: a.0.max(b.0) + 1,
            bottom: a.1.max(b.1) + 1,
        }
    }

    pub fn offset(&self, dx: i32, dy: i32) -> PixelRect {
        PixelRect {
            left: self.left + dx,
            top: self.top + dy,
            right: self.right + dx,
            bottom: self.bottom + dy,
        }
    }
}

impl From<RECT> for PixelRect {
    fn from(r: RECT) -> Self {
        Self {
            left: r.left,
            top: r.top,
            right: r.right,
            bottom: r.bottom,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Monitor {
    /// GDI device name, e.g. `\\.\DISPLAY1`.
    pub device: String,
    /// Monitor model name from EDID (e.g. "LG ULTRAGEAR"), if available.
    /// Prefer this over `device`, which doesn't match Windows Settings numbering.
    pub friendly_name: Option<String>,
    pub bounds: PixelRect,
    /// Bounds minus the taskbar and docked toolbars.
    pub work_area: PixelRect,
    pub dpi: u32,
    pub primary: bool,
}

impl Monitor {
    /// Human-readable name: the model name, or a generic fallback.
    pub fn name(&self) -> &str {
        self.friendly_name.as_deref().unwrap_or("Display")
    }

    /// Windows display scale, e.g. 1.5 for 150%.
    pub fn scale(&self) -> f32 {
        self.dpi as f32 / 96.0
    }
}

/// Lists all active displays. Requires a per-monitor-DPI-aware process for
/// correct physical coordinates (eframe/winit sets this up).
pub fn enumerate() -> Vec<Monitor> {
    unsafe extern "system" fn collect(
        monitor: HMONITOR,
        _hdc: HDC,
        _rect: *mut RECT,
        data: LPARAM,
    ) -> BOOL {
        // SAFETY: data is the &mut Vec passed below, valid for the
        // synchronous EnumDisplayMonitors call.
        unsafe {
            let monitors = &mut *(data.0 as *mut Vec<Monitor>);
            let mut info = MONITORINFOEXW::default();
            info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
            if GetMonitorInfoW(monitor, &mut info.monitorInfo).as_bool() {
                let (mut dpi, mut dpi_y) = (96, 96);
                let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi, &mut dpi_y);
                let len = info.szDevice.iter().position(|&c| c == 0).unwrap_or(info.szDevice.len());
                monitors.push(Monitor {
                    device: String::from_utf16_lossy(&info.szDevice[..len]),
                    friendly_name: None,
                    bounds: info.monitorInfo.rcMonitor.into(),
                    work_area: info.monitorInfo.rcWork.into(),
                    dpi,
                    primary: info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
                });
            }
            BOOL::from(true)
        }
    }

    let mut monitors: Vec<Monitor> = Vec::new();
    // SAFETY: the callback only runs during this call and writes to `monitors`.
    unsafe {
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(collect),
            LPARAM(&mut monitors as *mut Vec<Monitor> as isize),
        );
    }
    let names = friendly_names();
    for monitor in &mut monitors {
        monitor.friendly_name = names
            .iter()
            .find(|(device, _)| device.eq_ignore_ascii_case(&monitor.device))
            .map(|(_, name)| name.clone());
    }
    // Primary first, then left-to-right, top-to-bottom.
    monitors.sort_by_key(|m| (!m.primary, m.bounds.left, m.bounds.top));
    monitors
}

/// Maps GDI device names to monitor model names via the DisplayConfig API.
fn friendly_names() -> Vec<(String, String)> {
    let mut names = Vec::new();
    // SAFETY: buffers are sized by GetDisplayConfigBufferSizes and the
    // device-info requests use correctly sized, tagged headers.
    unsafe {
        let (mut path_count, mut mode_count) = (0u32, 0u32);
        if GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut path_count, &mut mode_count)
            != ERROR_SUCCESS
        {
            return names;
        }
        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); path_count as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); mode_count as usize];
        if QueryDisplayConfig(
            QDC_ONLY_ACTIVE_PATHS,
            &mut path_count,
            paths.as_mut_ptr(),
            &mut mode_count,
            modes.as_mut_ptr(),
            None,
        ) != ERROR_SUCCESS
        {
            return names;
        }

        for path in &paths[..path_count as usize] {
            let mut source = DISPLAYCONFIG_SOURCE_DEVICE_NAME::default();
            source.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME;
            source.header.size = size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32;
            source.header.adapterId = path.sourceInfo.adapterId;
            source.header.id = path.sourceInfo.id;

            let mut target = DISPLAYCONFIG_TARGET_DEVICE_NAME::default();
            target.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME;
            target.header.size = size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>() as u32;
            target.header.adapterId = path.targetInfo.adapterId;
            target.header.id = path.targetInfo.id;

            if DisplayConfigGetDeviceInfo(&mut source.header) != 0
                || DisplayConfigGetDeviceInfo(&mut target.header) != 0
            {
                continue;
            }
            let device = wide_to_string(&source.viewGdiDeviceName);
            let name = wide_to_string(&target.monitorFriendlyDeviceName);
            if !name.is_empty() {
                names.push((device, name));
            }
        }
    }
    names
}

fn wide_to_string(wide: &[u16]) -> String {
    let len = wide.iter().position(|&c| c == 0).unwrap_or(wide.len());
    String::from_utf16_lossy(&wide[..len])
}
