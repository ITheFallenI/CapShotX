//! A small non-activating notification shown after a capture.

use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::Once;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Direct2D::{
    D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, D2D1_ROUNDED_RECT, ID2D1Bitmap, ID2D1HwndRenderTarget,
    ID2D1RenderTarget,
};
use windows::Win32::Graphics::DirectWrite::IDWriteTextFormat;
use windows::Win32::Graphics::Dwm::{
    DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
    DwmSetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, EndPaint, GetMonitorInfoW, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromPoint,
    PAINTSTRUCT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::{
    CREATESTRUCTW, CS_DROPSHADOW, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
    GWLP_USERDATA, GetMessageW, GetWindowLongPtrW, IDC_HAND, KillTimer, LoadCursorW, MSG,
    PostQuitMessage, RegisterClassExW, SW_SHOWNOACTIVATE, SetTimer, SetWindowLongPtrW, ShowWindow,
    TranslateMessage, WM_ERASEBKGND, WM_LBUTTONUP, WM_NCCREATE, WM_NCDESTROY, WM_PAINT, WM_RBUTTONUP,
    WM_TIMER, WNDCLASSEXW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};
use windows::core::{BOOL, Interface, PCWSTR, Result, w};

use crate::d2d::{self, Factories};

pub struct Toast {
    pub title: String,
    pub subtitle: String,
    /// Opaque BGRA thumbnail source (any size; it is scaled down).
    pub image: Option<ToastImage>,
    /// Opened when the toast is clicked.
    pub open_on_click: Option<PathBuf>,
    pub error: bool,
    pub dark: bool,
}

pub struct ToastImage {
    pub bgra: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

const WIDTH: f32 = 360.0;
const HEIGHT: f32 = 76.0;
const MARGIN: f32 = 16.0;
const THUMB_W: f32 = 76.0;
const DURATION_MS: u32 = 3500;
const CLASS_NAME: PCWSTR = w!("CapShotX.Toast");
const TIMER_ID: usize = 1;

/// Shows the toast at the bottom-right of the primary display and blocks
/// until it closes (timeout or click). Runs its own message loop, so call it
/// from a worker thread.
pub fn show_toast(toast: Toast) -> Result<()> {
    register_class()?;
    // SAFETY: Win32 queries with valid out-pointers; the state box is owned
    // by the window between WM_NCCREATE and WM_NCDESTROY.
    unsafe {
        let monitor = MonitorFromPoint(POINT::default(), MONITOR_DEFAULTTOPRIMARY);
        let mut info = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        GetMonitorInfoW(monitor, &mut info).ok()?;
        let (mut dpi, mut dpi_y) = (96, 96);
        let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi, &mut dpi_y);
        let scale = dpi as f32 / 96.0;
        let (w, h) = ((WIDTH * scale) as i32, (HEIGHT * scale) as i32);
        let margin = (MARGIN * scale) as i32;
        let x = info.rcWork.right - w - margin;
        let y = info.rcWork.bottom - h - margin;

        let dark = toast.dark;
        let state = Box::new(State {
            toast,
            dpi: dpi as f32,
            size: (w as u32, h as u32),
            gfx: None,
        });
        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
            CLASS_NAME,
            w!("CapShotX notification"),
            WS_POPUP,
            x,
            y,
            w,
            h,
            None,
            None,
            Some(GetModuleHandleW(None)?.into()),
            Some(Box::into_raw(state) as *const c_void),
        )?;
        let corner = DWMWCP_ROUND;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &corner as *const _ as *const c_void,
            size_of_val(&corner) as u32,
        );
        let dark = BOOL::from(dark);
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            &dark as *const _ as *const c_void,
            size_of_val(&dark) as u32,
        );
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        SetTimer(Some(hwnd), TIMER_ID, DURATION_MS, None);

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    Ok(())
}

fn register_class() -> Result<()> {
    static REGISTER: Once = Once::new();
    let mut result = Ok(());
    REGISTER.call_once(|| {
        // SAFETY: fully initialised class with a matching wndproc.
        result = unsafe {
            (|| {
                let class = WNDCLASSEXW {
                    cbSize: size_of::<WNDCLASSEXW>() as u32,
                    style: CS_DROPSHADOW,
                    lpfnWndProc: Some(wndproc),
                    hInstance: GetModuleHandleW(None)?.into(),
                    hCursor: LoadCursorW(None, IDC_HAND)?,
                    lpszClassName: CLASS_NAME,
                    ..Default::default()
                };
                if RegisterClassExW(&class) == 0 {
                    return Err(windows::core::Error::from_thread());
                }
                Ok(())
            })()
        };
    });
    result
}

struct Graphics {
    target: ID2D1HwndRenderTarget,
    title: IDWriteTextFormat,
    subtitle: IDWriteTextFormat,
    thumb: Option<ID2D1Bitmap>,
}

struct State {
    toast: Toast,
    dpi: f32,
    size: (u32, u32),
    gfx: Option<Graphics>,
}

impl State {
    fn paint(&mut self, hwnd: HWND) -> Result<()> {
        if self.gfx.is_none() {
            let f = Factories::new()?;
            let target = f.hwnd_target(hwnd, self.size, self.dpi)?;
            let thumb = match &self.toast.image {
                Some(img) => Some(d2d::opaque_bitmap(&target, &img.bgra, img.width, img.height, img.width * 4)?),
                None => None,
            };
            self.gfx = Some(Graphics {
                title: f.text_format(14.0, d2d::DWRITE_FONT_WEIGHT_SEMI_BOLD, d2d::DWRITE_PARAGRAPH_ALIGNMENT_FAR, d2d::DWRITE_TEXT_ALIGNMENT_LEADING)?,
                subtitle: f.text_format(12.0, d2d::DWRITE_FONT_WEIGHT_NORMAL, d2d::DWRITE_PARAGRAPH_ALIGNMENT_NEAR, d2d::DWRITE_TEXT_ALIGNMENT_LEADING)?,
                target,
                thumb,
            });
        }
        let g = self.gfx.as_ref().expect("graphics just created");
        let t: ID2D1RenderTarget = g.target.cast()?;
        let dark = self.toast.dark;
        let (bg, text, muted, border) = if dark {
            (d2d::rgb(32, 35, 41), d2d::rgb(230, 232, 235), d2d::rgb(138, 147, 160), d2d::rgb(48, 53, 62))
        } else {
            (d2d::rgb(255, 255, 255), d2d::rgb(26, 29, 33), d2d::rgb(107, 114, 128), d2d::rgb(226, 229, 235))
        };
        let accent = if self.toast.error { d2d::rgb(239, 68, 68) } else { d2d::rgb(110, 123, 255) };

        // SAFETY: drawing between BeginDraw/EndDraw with this target's resources.
        unsafe {
            let text_brush = d2d::brush(&t, text)?;
            let muted_brush = d2d::brush(&t, muted)?;
            let border_brush = d2d::brush(&t, border)?;
            let accent_brush = d2d::brush(&t, accent)?;
            t.BeginDraw();
            t.Clear(Some(&bg));

            let pad = 12.0;
            let mut text_left = pad + 6.0;
            if let (Some(thumb), Some(img)) = (&g.thumb, &self.toast.image) {
                // Fit the capture into the thumbnail box, preserving aspect.
                let box_h = HEIGHT - 2.0 * pad;
                let aspect = img.width as f32 / img.height.max(1) as f32;
                let (w, h) = if aspect > THUMB_W / box_h {
                    (THUMB_W, THUMB_W / aspect)
                } else {
                    (box_h * aspect, box_h)
                };
                let x = pad + (THUMB_W - w) / 2.0;
                let y = (HEIGHT - h) / 2.0;
                let dest = d2d::rect(x, y, x + w, y + h);
                t.DrawBitmap(thumb, Some(&dest), 1.0, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, None);
                t.DrawRectangle(&dest, &border_brush, 1.0, None);
                text_left = pad + THUMB_W + 14.0;
            } else {
                let bar = D2D1_ROUNDED_RECT { rect: d2d::rect(pad, pad + 6.0, pad + 4.0, HEIGHT - pad - 6.0), radiusX: 2.0, radiusY: 2.0 };
                t.FillRoundedRectangle(&bar, &accent_brush);
            }

            let mid = HEIGHT / 2.0;
            d2d::draw_text(&t, &self.toast.title, &g.title, d2d::rect(text_left, 0.0, WIDTH - pad, mid + 1.0), &text_brush);
            d2d::draw_text(&t, &self.toast.subtitle, &g.subtitle, d2d::rect(text_left, mid + 3.0, WIDTH - pad, HEIGHT), &muted_brush);
            t.EndDraw(None, None)
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // SAFETY: GWLP_USERDATA holds the Box<State> from WM_NCCREATE to WM_NCDESTROY.
    unsafe {
        if msg == WM_NCCREATE {
            let create = &*(lparam.0 as *const CREATESTRUCTW);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }
        let state = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut State;
        if state.is_null() {
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }
        match msg {
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                BeginPaint(hwnd, &mut ps);
                if (*state).paint(hwnd).is_err() {
                    (*state).gfx = None;
                }
                let _ = EndPaint(hwnd, &ps);
                LRESULT(0)
            }
            WM_ERASEBKGND => LRESULT(1),
            WM_LBUTTONUP => {
                if let Some(path) = &(*state).toast.open_on_click {
                    crate::shell::open(path);
                }
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            WM_RBUTTONUP | WM_TIMER => {
                let _ = KillTimer(Some(hwnd), TIMER_ID);
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            WM_NCDESTROY => {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                drop(Box::from_raw(state));
                PostQuitMessage(0);
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}
