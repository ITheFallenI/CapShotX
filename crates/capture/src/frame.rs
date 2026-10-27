//! A frozen copy of the desktop.

use platform_windows::monitors::PixelRect;
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CAPTUREBLT, CreateCompatibleDC, CreateDIBSection,
    DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC, ReleaseDC, SRCCOPY, SelectObject,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
};
use windows::core::Result;

/// Top-down BGRA pixels covering `bounds` in virtual-desktop coordinates.
/// Alpha is undefined (treat as opaque).
pub struct Frame {
    pub bounds: PixelRect,
    pub bgra: Vec<u8>,
}

impl Frame {
    pub fn width(&self) -> u32 {
        self.bounds.width() as u32
    }

    pub fn height(&self) -> u32 {
        self.bounds.height() as u32
    }

    pub fn stride(&self) -> usize {
        self.width() as usize * 4
    }

    /// Byte offset of the virtual-desktop pixel (x, y).
    pub fn offset(&self, x: i32, y: i32) -> usize {
        (y - self.bounds.top) as usize * self.stride() + (x - self.bounds.left) as usize * 4
    }

    /// RGB at a virtual-desktop pixel, if inside the frame.
    pub fn rgb(&self, x: i32, y: i32) -> Option<[u8; 3]> {
        self.bounds.contains(x, y).then(|| {
            let i = self.offset(x, y);
            [self.bgra[i + 2], self.bgra[i + 1], self.bgra[i]]
        })
    }

    /// Copies out `rect` (clipped to the frame).
    pub fn crop(&self, rect: PixelRect) -> Option<Frame> {
        let rect = rect.intersect(&self.bounds)?;
        let row = rect.width() as usize * 4;
        let mut bgra = Vec::with_capacity(row * rect.height() as usize);
        for y in rect.top..rect.bottom {
            let start = self.offset(rect.left, y);
            bgra.extend_from_slice(&self.bgra[start..start + row]);
        }
        Some(Frame { bounds: rect, bgra })
    }
}

/// Captures every monitor at once with GDI.
///
/// Must run in a per-monitor-DPI-aware process so coordinates are physical.
/// Content protected with `WDA_EXCLUDEFROMCAPTURE` comes back black, as
/// Windows intends.
pub fn grab_virtual_screen() -> Result<Frame> {
    // SAFETY: GDI objects are created, used and released within this
    // function; the DIB section's memory is copied out before deletion.
    unsafe {
        let bounds = PixelRect {
            left: GetSystemMetrics(SM_XVIRTUALSCREEN),
            top: GetSystemMetrics(SM_YVIRTUALSCREEN),
            right: GetSystemMetrics(SM_XVIRTUALSCREEN) + GetSystemMetrics(SM_CXVIRTUALSCREEN),
            bottom: GetSystemMetrics(SM_YVIRTUALSCREEN) + GetSystemMetrics(SM_CYVIRTUALSCREEN),
        };
        let (w, h) = (bounds.width(), bounds.height());

        let screen = GetDC(None);
        let mem = CreateCompatibleDC(Some(screen));
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h, // top-down
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let result = CreateDIBSection(Some(mem), &info, DIB_RGB_COLORS, &mut bits, None, 0)
            .and_then(|dib| {
                let old = SelectObject(mem, dib.into());
                let blit = BitBlt(mem, 0, 0, w, h, Some(screen), bounds.left, bounds.top, SRCCOPY | CAPTUREBLT);
                let pixels = blit.map(|()| {
                    std::slice::from_raw_parts(bits as *const u8, w as usize * h as usize * 4).to_vec()
                });
                SelectObject(mem, old);
                let _ = DeleteObject(dib.into());
                pixels
            });
        let _ = DeleteDC(mem);
        ReleaseDC(None, screen);
        Ok(Frame { bounds, bgra: result? })
    }
}
