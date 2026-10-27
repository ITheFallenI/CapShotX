//! The floating recording toolbar (REC-04): elapsed time, level meter,
//! pause/resume and stop, plus a warning row when a source has a problem.
//!
//! Topmost, non-activating (never steals focus from what's being recorded),
//! draggable, and excluded from screen capture so it never appears in the
//! recording. Runs its own thread and polls the caller's status closure.

use std::ffi::c_void;
use std::sync::Once;
use std::sync::mpsc;
use std::time::Duration;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::D2D_SIZE_U;
use windows::Win32::Graphics::Direct2D::{D2D1_ELLIPSE, D2D1_ROUNDED_RECT, ID2D1HwndRenderTarget, ID2D1RenderTarget};
use windows::Win32::Graphics::DirectWrite::IDWriteTextFormat;
use windows::Win32::Graphics::Dwm::{
    DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND, DwmSetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, EndPaint, GetMonitorInfoW, InvalidateRect, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint,
    PAINTSTRUCT, ScreenToClient,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent};
use windows::Win32::UI::WindowsAndMessaging::{
    CREATESTRUCTW, CS_DROPSHADOW, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GWLP_USERDATA,
    GetMessageW, GetWindowLongPtrW, HTCAPTION, HTCLIENT, IDC_ARROW, LoadCursorW, MSG,
    PostMessageW, PostQuitMessage, RegisterClassExW, SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOZORDER,
    SetTimer, SetWindowDisplayAffinity, SetWindowLongPtrW, SetWindowPos, ShowWindow, TranslateMessage,
    WDA_EXCLUDEFROMCAPTURE, WM_CLOSE, WM_ERASEBKGND, WM_LBUTTONUP, WM_MOUSEMOVE, WM_NCCREATE,
    WM_NCDESTROY, WM_NCHITTEST, WM_PAINT, WM_TIMER, WNDCLASSEXW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    WS_POPUP,
};
use windows::core::{BOOL, Interface, PCWSTR, Result, w};

use crate::d2d::{self, Factories};

#[derive(Clone, Debug, Default)]
pub struct ToolbarStatus {
    pub elapsed: Duration,
    pub paused: bool,
    /// Current audio peak, 0..=1 (`None`: no audio being recorded).
    pub level: Option<f32>,
    pub warning: Option<String>,
    /// Seconds left before recording starts.
    pub countdown: Option<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolbarAction {
    TogglePause,
    /// Stop (or cancel during the countdown).
    Stop,
}

type StatusFn = Box<dyn Fn() -> ToolbarStatus + Send>;
type ActionFn = Box<dyn Fn(ToolbarAction) + Send>;

const CLASS_NAME: PCWSTR = w!("CapShotX.RecordingToolbar");
const WIDTH: f32 = 248.0;
const BAR_H: f32 = 44.0;
const WARN_H: f32 = 26.0;
const BUTTON: f32 = 30.0;
const TIMER_ID: usize = 1;
const WM_MOUSELEAVE: u32 = 0x02A3;

/// Keeps the toolbar open; dropping it closes the window.
pub struct RecordingToolbar {
    hwnd: isize,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl RecordingToolbar {
    /// Opens the toolbar at the top centre of the display containing `near`
    /// (virtual-desktop pixels).
    pub fn show(
        near: (i32, i32),
        dark: bool,
        status: impl Fn() -> ToolbarStatus + Send + 'static,
        on_action: impl Fn(ToolbarAction) + Send + 'static,
    ) -> Result<Self> {
        let (tx, rx) = mpsc::channel();
        let (status, on_action): (StatusFn, ActionFn) = (Box::new(status), Box::new(on_action));
        let thread = std::thread::Builder::new()
            .name("rec-toolbar".into())
            .spawn(move || run(near, dark, status, on_action, tx))
            .map_err(|_| windows::core::Error::from_thread())?;
        match rx.recv() {
            Ok(Ok(hwnd)) => Ok(Self { hwnd, thread: Some(thread) }),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(windows::core::Error::from_thread()),
        }
    }
}

impl Drop for RecordingToolbar {
    fn drop(&mut self) {
        // SAFETY: posting to a window owned by our thread; harmless if gone.
        let _ = unsafe { PostMessageW(Some(HWND(self.hwnd as _)), WM_CLOSE, WPARAM(0), LPARAM(0)) };
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Hit {
    None,
    Pause,
    Stop,
}

struct Graphics {
    target: ID2D1HwndRenderTarget,
    time: IDWriteTextFormat,
    small: IDWriteTextFormat,
}

struct State {
    status_fn: StatusFn,
    on_action: ActionFn,
    status: ToolbarStatus,
    dark: bool,
    scale: f32,
    hover: Hit,
    meter: f32,
    blink: u32,
    gfx: Option<Graphics>,
    size: (u32, u32),
}

fn run(near: (i32, i32), dark: bool, status_fn: StatusFn, on_action: ActionFn, ready: mpsc::Sender<Result<isize>>) {
    // SAFETY: standard window creation and message loop on this thread; the
    // state box is owned by the window from WM_NCCREATE to WM_NCDESTROY.
    unsafe {
        let created = (|| {
            register_class()?;
            let monitor = MonitorFromPoint(POINT { x: near.0, y: near.1 }, MONITOR_DEFAULTTONEAREST);
            let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
            GetMonitorInfoW(monitor, &mut info).ok()?;
            let (mut dpi, mut dpi_y) = (96, 96);
            let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi, &mut dpi_y);
            let scale = dpi as f32 / 96.0;
            let (w, h) = ((WIDTH * scale) as i32, (BAR_H * scale) as i32);
            let work = info.rcWork;
            let x = (work.left + work.right - w) / 2;
            let y = work.top + (12.0 * scale) as i32;

            let status = status_fn();
            let state = Box::new(State {
                status_fn,
                on_action,
                status,
                dark,
                scale,
                hover: Hit::None,
                meter: 0.0,
                blink: 0,
                gfx: None,
                size: (w as u32, h as u32),
            });
            let hwnd = CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
                CLASS_NAME,
                w!("CapShotX recording"),
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
            // Keep the toolbar out of every capture, including our own.
            if SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE).is_err() {
                tracing::warn!("toolbar can't be excluded from capture on this Windows version");
            }
            let corner = DWMWCP_ROUND;
            let _ = DwmSetWindowAttribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, &corner as *const _ as *const c_void, size_of_val(&corner) as u32);
            let dark = BOOL::from(dark);
            let _ = DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark as *const _ as *const c_void, size_of_val(&dark) as u32);
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            SetTimer(Some(hwnd), TIMER_ID, 100, None);
            Ok(hwnd)
        })();
        match created {
            Ok(hwnd) => {
                let _ = ready.send(Ok(hwnd.0 as isize));
            }
            Err(e) => {
                let _ = ready.send(Err(e));
                return;
            }
        }
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
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
                    hCursor: LoadCursorW(None, IDC_ARROW)?,
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

/// Button rectangles in DIPs: (pause, stop).
fn buttons() -> ((f32, f32), (f32, f32)) {
    let top = (BAR_H - BUTTON) / 2.0;
    let stop_x = WIDTH - 7.0 - BUTTON;
    let pause_x = stop_x - 4.0 - BUTTON;
    ((pause_x, top), (stop_x, top))
}

fn hit(x: f32, y: f32) -> Hit {
    let ((px, py), (sx, sy)) = buttons();
    let inside = |bx: f32, by: f32| x >= bx && x < bx + BUTTON && y >= by && y < by + BUTTON;
    if inside(px, py) {
        Hit::Pause
    } else if inside(sx, sy) {
        Hit::Stop
    } else {
        Hit::None
    }
}

fn format_time(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 3600 { format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60) } else { format!("{}:{:02}", s / 60, s % 60) }
}

impl State {
    fn tick(&mut self, hwnd: HWND) {
        self.status = (self.status_fn)();
        self.blink = self.blink.wrapping_add(1);
        let level = self.status.level.unwrap_or(0.0).clamp(0.0, 1.0);
        self.meter = level.max(self.meter * 0.8);

        let want_h = ((if self.status.warning.is_some() { BAR_H + WARN_H } else { BAR_H }) * self.scale) as u32;
        if want_h != self.size.1 {
            self.size.1 = want_h;
            // SAFETY: resizing our own window.
            unsafe {
                let _ = SetWindowPos(hwnd, None, 0, 0, self.size.0 as i32, want_h as i32, SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE);
            }
            if let Some(g) = &self.gfx {
                // SAFETY: resize of our own target.
                let _ = unsafe { g.target.Resize(&D2D_SIZE_U { width: self.size.0, height: want_h }) };
            }
        }
        // SAFETY: invalidating our own window.
        let _ = unsafe { InvalidateRect(Some(hwnd), None, false) };
    }

    fn paint(&mut self, hwnd: HWND) -> Result<()> {
        if self.gfx.is_none() {
            let f = Factories::new()?;
            let target = f.hwnd_target(hwnd, self.size, 96.0 * self.scale)?;
            self.gfx = Some(Graphics {
                time: f.text_format(15.0, d2d::DWRITE_FONT_WEIGHT_SEMI_BOLD, d2d::DWRITE_PARAGRAPH_ALIGNMENT_CENTER, d2d::DWRITE_TEXT_ALIGNMENT_LEADING)?,
                small: f.text_format(12.0, d2d::DWRITE_FONT_WEIGHT_NORMAL, d2d::DWRITE_PARAGRAPH_ALIGNMENT_CENTER, d2d::DWRITE_TEXT_ALIGNMENT_LEADING)?,
                target,
            });
        }
        let g = self.gfx.as_ref().expect("graphics just created");
        let t: ID2D1RenderTarget = g.target.cast()?;
        let (bg, text, muted, control, hover, track) = if self.dark {
            (d2d::rgb(32, 35, 41), d2d::rgb(230, 232, 235), d2d::rgb(138, 147, 160), d2d::rgb(44, 48, 56), d2d::rgb(56, 61, 71), d2d::rgb(52, 57, 66))
        } else {
            (d2d::rgb(255, 255, 255), d2d::rgb(26, 29, 33), d2d::rgb(107, 114, 128), d2d::rgb(241, 243, 246), d2d::rgb(229, 232, 237), d2d::rgb(226, 229, 235))
        };
        let red = d2d::rgb(239, 68, 68);
        let amber = d2d::rgb(245, 158, 11);
        let green = d2d::rgb(16, 185, 129);
        let s = &self.status;
        let mid = BAR_H / 2.0;

        // SAFETY: drawing between BeginDraw/EndDraw with this target's resources.
        unsafe {
            t.BeginDraw();
            t.Clear(Some(&bg));
            let text_b = d2d::brush(&t, text)?;
            let muted_b = d2d::brush(&t, muted)?;

            // Status dot: blinking red while recording, amber when paused.
            let dot = if s.countdown.is_some() {
                d2d::brush(&t, muted)?
            } else if s.paused {
                d2d::brush(&t, amber)?
            } else {
                d2d::brush(&t, if self.blink % 10 < 7 { red } else { d2d::rgba(239, 68, 68, 0.35) })?
            };
            t.FillEllipse(&D2D1_ELLIPSE { point: windows_numerics::Vector2 { X: 18.0, Y: mid }, radiusX: 5.0, radiusY: 5.0 }, &dot);

            let label = match s.countdown {
                Some(n) => format!("Starting in {n}…"),
                None => format_time(s.elapsed),
            };
            let time_b = if s.paused { &muted_b } else { &text_b };
            d2d::draw_text(&t, &label, &g.time, d2d::rect(32.0, 0.0, 140.0, BAR_H), time_b);
            if s.paused && s.countdown.is_none() {
                d2d::draw_text(&t, "Paused", &g.small, d2d::rect(84.0, 0.0, 140.0, BAR_H), &muted_b);
            }

            // Level meter.
            if let (Some(_), None) = (s.level, s.countdown) {
                let (mx, mw) = (130.0, 40.0);
                let bar = |w: f32| D2D1_ROUNDED_RECT { rect: d2d::rect(mx, mid - 3.0, mx + w, mid + 3.0), radiusX: 3.0, radiusY: 3.0 };
                t.FillRoundedRectangle(&bar(mw), &d2d::brush(&t, track)?);
                let w = (self.meter.sqrt() * mw).max(if self.meter > 0.0005 { 3.0 } else { 0.0 });
                if w > 0.0 {
                    let c = if self.meter > 0.9 { red } else if self.meter > 0.5 { amber } else { green };
                    t.FillRoundedRectangle(&bar(w), &d2d::brush(&t, c)?);
                }
            }

            // Buttons.
            let ((px, py), (sx, sy)) = buttons();
            for (x, y, which) in [(px, py, Hit::Pause), (sx, sy, Hit::Stop)] {
                let fill = d2d::brush(&t, if self.hover == which { hover } else { control })?;
                let r = D2D1_ROUNDED_RECT { rect: d2d::rect(x, y, x + BUTTON, y + BUTTON), radiusX: 6.0, radiusY: 6.0 };
                t.FillRoundedRectangle(&r, &fill);
            }
            let (pcx, pcy) = (px + BUTTON / 2.0, py + BUTTON / 2.0);
            if s.countdown.is_some() {
                // Nothing to pause yet.
            } else if s.paused {
                // Resume = record dot.
                t.FillEllipse(&D2D1_ELLIPSE { point: windows_numerics::Vector2 { X: pcx, Y: pcy }, radiusX: 6.0, radiusY: 6.0 }, &d2d::brush(&t, red)?);
            } else {
                t.FillRectangle(&d2d::rect(pcx - 5.0, pcy - 6.0, pcx - 1.5, pcy + 6.0), &text_b);
                t.FillRectangle(&d2d::rect(pcx + 1.5, pcy - 6.0, pcx + 5.0, pcy + 6.0), &text_b);
            }
            let (scx, scy) = (sx + BUTTON / 2.0, sy + BUTTON / 2.0);
            let sq = D2D1_ROUNDED_RECT { rect: d2d::rect(scx - 6.0, scy - 6.0, scx + 6.0, scy + 6.0), radiusX: 2.0, radiusY: 2.0 };
            t.FillRoundedRectangle(&sq, &d2d::brush(&t, if s.countdown.is_some() { muted } else { red })?);

            if let Some(warning) = &s.warning {
                let y = BAR_H;
                t.FillRectangle(&d2d::rect(0.0, y, WIDTH, y + WARN_H), &d2d::brush(&t, d2d::rgba(245, 158, 11, 0.16))?);
                t.FillEllipse(&D2D1_ELLIPSE { point: windows_numerics::Vector2 { X: 18.0, Y: y + WARN_H / 2.0 }, radiusX: 4.0, radiusY: 4.0 }, &d2d::brush(&t, amber)?);
                d2d::draw_text(&t, warning, &g.small, d2d::rect(30.0, y, WIDTH - 8.0, y + WARN_H), &text_b);
            }
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
        let st = &mut *state;
        let client = |lparam: LPARAM| {
            let x = (lparam.0 & 0xffff) as i16 as f32;
            let y = ((lparam.0 >> 16) & 0xffff) as i16 as f32;
            (x / st.scale, y / st.scale)
        };
        match msg {
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                BeginPaint(hwnd, &mut ps);
                if st.paint(hwnd).is_err() {
                    st.gfx = None;
                }
                let _ = EndPaint(hwnd, &ps);
                LRESULT(0)
            }
            WM_ERASEBKGND => LRESULT(1),
            WM_TIMER => {
                st.tick(hwnd);
                LRESULT(0)
            }
            // Drag anywhere except the buttons.
            WM_NCHITTEST => {
                let mut p = POINT { x: (lparam.0 & 0xffff) as i16 as i32, y: ((lparam.0 >> 16) & 0xffff) as i16 as i32 };
                let _ = ScreenToClient(hwnd, &mut p);
                if hit(p.x as f32 / st.scale, p.y as f32 / st.scale) == Hit::None {
                    LRESULT(HTCAPTION as isize)
                } else {
                    LRESULT(HTCLIENT as isize)
                }
            }
            WM_MOUSEMOVE => {
                let (x, y) = client(lparam);
                let h = hit(x, y);
                if h != st.hover {
                    st.hover = h;
                    let mut tme = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
                    let _ = TrackMouseEvent(&mut tme);
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
                LRESULT(0)
            }
            WM_MOUSELEAVE => {
                st.hover = Hit::None;
                let _ = InvalidateRect(Some(hwnd), None, false);
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                let (x, y) = client(lparam);
                match hit(x, y) {
                    Hit::Pause if st.status.countdown.is_none() => (st.on_action)(ToolbarAction::TogglePause),
                    Hit::Stop => (st.on_action)(ToolbarAction::Stop),
                    _ => {}
                }
                st.tick(hwnd);
                LRESULT(0)
            }
            WM_CLOSE => {
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
