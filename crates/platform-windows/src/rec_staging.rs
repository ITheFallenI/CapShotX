//! Camtasia-style pre-recording bounds adjustment and control bar.
//!
//! Opens two topmost capture-excluded windows on a live desktop:
//! 1. A per-pixel-alpha (`WS_EX_LAYERED` + `UpdateLayeredWindow`) frame around
//!    the selected bounds with 8 resize handles, a move/dimension badge, and a
//!    click-through (`alpha == 0`) interior so the user can still interact
//!    with windows inside the area before starting.
//! 2. A Direct2D control bar with quick size presets (`Full`, `1080p`, `720p`,
//!    `Reselect`), live Desktop & Microphone toggles with level meters, a
//!    prominent `Start` button (`Enter`) and `Cancel` (`Esc`).

use std::ffi::c_void;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Once};

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::Graphics::Direct2D::{
    D2D1_ELLIPSE, D2D1_ROUNDED_RECT, ID2D1DCRenderTarget, ID2D1HwndRenderTarget, ID2D1RenderTarget,
};
use windows::Win32::Graphics::DirectWrite::IDWriteTextFormat;
use windows::Win32::Graphics::Dwm::{
    DWMWA_EXTENDED_FRAME_BOUNDS, DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWA_WINDOW_CORNER_PREFERENCE,
    DWMWCP_ROUND, DwmGetWindowAttribute, DwmSetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION, BeginPaint,
    CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, EndPaint,
    HBITMAP, HDC, HGDIOBJ, InvalidateRect, PAINTSTRUCT, ScreenToClient, SelectObject,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, ReleaseCapture, SetCapture, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent,
    VK_CONTROL, VK_DOWN, VK_ESCAPE, VK_LEFT, VK_RETURN, VK_RIGHT, VK_SHIFT, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CREATESTRUCTW, CS_DROPSHADOW, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
    GWLP_USERDATA, GetMessageW, GetWindowLongPtrW, HTCAPTION, HTCLIENT, IDC_ARROW,
    IDC_HAND, IDC_SIZEALL, IDC_SIZENESW, IDC_SIZENS, IDC_SIZENWSE, IDC_SIZEWE, IsIconic, IsWindow,
    KillTimer, LoadCursorW, MSG, RegisterClassExW, SW_SHOW, SW_SHOWNOACTIVATE, SWP_NOACTIVATE,
    SWP_NOZORDER, SetCursor, SetForegroundWindow, SetTimer, SetWindowDisplayAffinity,
    SetWindowLongPtrW, SetWindowPos, ShowWindow, TranslateMessage, ULW_ALPHA, UpdateLayeredWindow,
    WDA_EXCLUDEFROMCAPTURE, WM_CLOSE, WM_ERASEBKGND, WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MOUSEMOVE, WM_NCCREATE, WM_NCHITTEST, WM_PAINT, WM_RBUTTONUP, WM_SETCURSOR, WM_TIMER,
    WNDCLASSEXW, WS_EX_LAYERED, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};
use windows::core::{BOOL, Interface, PCWSTR, Result, w};

use crate::d2d::{self, Factories};
use crate::monitors::{self, Monitor, PixelRect};

const FRAME_CLASS: PCWSTR = w!("CapShotX.RecordStagingFrame");
const BAR_CLASS: PCWSTR = w!("CapShotX.RecordStagingBar");

const BAR_W: f32 = 490.0;
const BAR_H: f32 = 48.0;
const BTN_H: f32 = 32.0;
const BTN_Y: f32 = 8.0;
const MIN_SIDE: i32 = 64;
const TIMER_ID: usize = 1;
const WM_MOUSELEAVE: u32 = 0x02A3;
const WM_ENTERSIZEMOVE: u32 = 0x0231;
const WM_DPICHANGED: u32 = 0x02E0;

/// External signal values stored in the `Arc<AtomicU8>` passed to [`run_staging`].
pub const SIGNAL_NONE: u8 = 0;
pub const SIGNAL_START: u8 = 1;
pub const SIGNAL_CANCEL: u8 = 2;

#[derive(Clone, Debug)]
pub struct StagingConfig {
    pub rect: PixelRect,
    pub window: Option<isize>,
    pub window_title: Option<String>,
    pub highlight: [u8; 3],
    /// Border thickness in pixels at 100% scale.
    pub border_width: u8,
    pub dark: bool,
    pub desktop_audio: bool,
    pub microphone: bool,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct StagingLevels {
    pub desktop_peak: Option<f32>,
    pub desktop_err: bool,
    pub mic_peak: Option<f32>,
    pub mic_err: bool,
}

#[derive(Clone, Debug)]
pub enum StagingOutcome {
    Start {
        rect: PixelRect,
        window: Option<isize>,
        desktop_audio: bool,
        microphone: bool,
    },
    Reselect {
        desktop_audio: bool,
        microphone: bool,
    },
    Cancelled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Grip {
    N,
    S,
    E,
    W,
    NE,
    NW,
    SE,
    SW,
    Move,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Btn {
    Reselect,
    Full,
    P1080,
    P720,
    Desktop,
    Mic,
    Start,
    Cancel,
}

struct DibSurface {
    dc: HDC,
    bmp: HBITMAP,
    old: HGDIOBJ,
    size: (i32, i32),
}

impl DibSurface {
    unsafe fn new(w: i32, h: i32) -> Result<Self> {
        // SAFETY: paired GDI allocations cleaned up in Drop.
        unsafe {
            let dc = CreateCompatibleDC(None);
            if dc.is_invalid() {
                return Err(windows::core::Error::from_thread());
            }
            let bmi = BITMAPINFO {
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
            let bmp = match CreateDIBSection(Some(dc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0) {
                Ok(b) => b,
                Err(e) => {
                    let _ = DeleteDC(dc);
                    return Err(e);
                }
            };
            let old = SelectObject(dc, bmp.into());
            Ok(Self { dc, bmp, old, size: (w, h) })
        }
    }
}

impl Drop for DibSurface {
    fn drop(&mut self) {
        // SAFETY: restores original object before deleting DIB and memory DC.
        unsafe {
            let _ = SelectObject(self.dc, self.old);
            let _ = DeleteObject(self.bmp.into());
            let _ = DeleteDC(self.dc);
        }
    }
}

struct FrameGfx {
    target: ID2D1DCRenderTarget,
    label: IDWriteTextFormat,
    scale: f32,
    dib: Option<DibSurface>,
}

struct BarGfx {
    target: ID2D1HwndRenderTarget,
    text: IDWriteTextFormat,
    bold: IDWriteTextFormat,
    small: IDWriteTextFormat,
}

type PollAudio<'a> = &'a mut dyn FnMut(bool, bool) -> StagingLevels;

struct Staging<'a> {
    factories: Factories,
    monitors: Vec<Monitor>,
    rect: PixelRect,
    window: Option<isize>,
    window_title: Option<String>,
    highlight: [u8; 3],
    border_width: u8,
    dark: bool,
    desktop_audio: bool,
    microphone: bool,
    levels: StagingLevels,
    desktop_meter: f32,
    mic_meter: f32,
    signal: Arc<AtomicU8>,
    poll_audio: PollAudio<'a>,

    frame_hwnd: HWND,
    frame_win_rect: PixelRect,
    badge_rect: Option<PixelRect>,
    frame_gfx: Option<FrameGfx>,
    drag: Option<(Grip, PixelRect, (i32, i32))>,
    hover_grip: Option<Grip>,

    bar_hwnd: HWND,
    bar_scale: f32,
    bar_size: (u32, u32),
    bar_gfx: Option<BarGfx>,
    bar_hover: Option<Btn>,
    bar_pressed: Option<Btn>,
    bar_detached: bool,

    outcome: Option<StagingOutcome>,
}

/// Shows the live bounds-adjustment frame and pre-record control bar on the
/// calling thread and blocks until the user clicks `Start`, `Reselect` or
/// `Cancel` (or an external signal arrives).
pub fn run_staging(
    initial: StagingConfig,
    signal: Arc<AtomicU8>,
    mut poll_audio: impl FnMut(bool, bool) -> StagingLevels,
) -> Result<StagingOutcome> {
    register_classes()?;
    let factories = Factories::new()?;
    let monitors = monitors::enumerate();
    let mut rect = sanitize_rect(initial.rect, &monitors);
    let mon = best_monitor(&rect, &monitors).clone();
    if initial.window.is_none() {
        rect = clamp_even(rect, mon.bounds);
    }

    let levels = poll_audio(initial.desktop_audio, initial.microphone);
    let mut state = Box::new(Staging {
        factories,
        monitors,
        rect,
        window: initial.window,
        window_title: initial.window_title,
        highlight: initial.highlight,
        border_width: initial.border_width,
        dark: initial.dark,
        desktop_audio: initial.desktop_audio,
        microphone: initial.microphone,
        levels,
        desktop_meter: 0.0,
        mic_meter: 0.0,
        signal,
        poll_audio: &mut poll_audio,
        frame_hwnd: HWND::default(),
        frame_win_rect: rect,
        badge_rect: None,
        frame_gfx: None,
        drag: None,
        hover_grip: None,
        bar_hwnd: HWND::default(),
        bar_scale: mon.scale(),
        bar_size: ((BAR_W * mon.scale()).round() as u32, (BAR_H * mon.scale()).round() as u32),
        bar_gfx: None,
        bar_hover: None,
        bar_pressed: None,
        bar_detached: false,
        outcome: None,
    });

    let ptr = (&mut *state) as *mut Staging<'_> as *const c_void;
    // SAFETY: both windows run on this thread and have GWLP_USERDATA cleared
    // before `state` leaves scope.
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let fw = state.compute_frame_window_rect();
        state.frame_win_rect = fw;
        let frame_hwnd = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            FRAME_CLASS,
            w!("CapShotX recording bounds"),
            WS_POPUP,
            fw.left,
            fw.top,
            fw.width(),
            fw.height(),
            None,
            None,
            Some(instance.into()),
            Some(ptr),
        )?;
        let _ = SetWindowDisplayAffinity(frame_hwnd, WDA_EXCLUDEFROMCAPTURE);
        state.frame_hwnd = frame_hwnd;

        let (bx, by) = state.ideal_bar_pos();
        let (bw, bh) = (state.bar_size.0 as i32, state.bar_size.1 as i32);
        let bar_hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            BAR_CLASS,
            w!("CapShotX pre-record controls"),
            WS_POPUP,
            bx,
            by,
            bw,
            bh,
            None,
            None,
            Some(instance.into()),
            Some(ptr),
        )?;
        let _ = SetWindowDisplayAffinity(bar_hwnd, WDA_EXCLUDEFROMCAPTURE);
        let corner = DWMWCP_ROUND;
        let _ = DwmSetWindowAttribute(
            bar_hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &corner as *const _ as *const c_void,
            size_of_val(&corner) as u32,
        );
        let dark = BOOL::from(state.dark);
        let _ = DwmSetWindowAttribute(
            bar_hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            &dark as *const _ as *const c_void,
            size_of_val(&dark) as u32,
        );
        state.bar_hwnd = bar_hwnd;

        let _ = state.paint_frame();
        let _ = ShowWindow(frame_hwnd, SW_SHOWNOACTIVATE);
        let _ = ShowWindow(bar_hwnd, SW_SHOW);
        let _ = SetForegroundWindow(bar_hwnd);
        SetTimer(Some(bar_hwnd), TIMER_ID, 60, None);

        let mut msg = MSG::default();
        while state.outcome.is_none() && GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        let _ = KillTimer(Some(bar_hwnd), TIMER_ID);
        for hwnd in [bar_hwnd, frame_hwnd] {
            if !hwnd.is_invalid() {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                let _ = DestroyWindow(hwnd);
            }
        }
    }

    Ok(state.outcome.take().unwrap_or(StagingOutcome::Cancelled))
}

fn cursor_pos() -> (i32, i32) {
    crate::shell::cursor_pos()
}

fn key_down(vk: u16) -> bool {
    unsafe { GetKeyState(vk as i32) < 0 }
}

fn sanitize_rect(r: PixelRect, monitors: &[Monitor]) -> PixelRect {
    if !r.is_empty() {
        return r;
    }
    monitors
        .first()
        .map_or(PixelRect { left: 0, top: 0, right: 1280, bottom: 720 }, |m| m.bounds)
}

fn best_monitor<'a>(rect: &PixelRect, monitors: &'a [Monitor]) -> &'a Monitor {
    let area = |m: &Monitor| rect.intersect(&m.bounds).map_or(0i64, |r| r.width() as i64 * r.height() as i64);
    monitors
        .iter()
        .max_by_key(|m| (area(m), m.primary))
        .expect("at least one monitor")
}

fn monitor_at<'a>(x: i32, y: i32, monitors: &'a [Monitor]) -> &'a Monitor {
    monitors
        .iter()
        .find(|m| m.bounds.contains(x, y))
        .unwrap_or_else(|| best_monitor(&PixelRect { left: x, top: y, right: x + 2, bottom: y + 2 }, monitors))
}

fn clamp_even(r: PixelRect, bounds: PixelRect) -> PixelRect {
    let max_w = (bounds.width() & !1).max(2);
    let max_h = (bounds.height() & !1).max(2);
    let w = (r.width().max(MIN_SIDE) & !1).min(max_w);
    let h = (r.height().max(MIN_SIDE) & !1).min(max_h);
    let left = r.left.clamp(bounds.left, (bounds.right - w).max(bounds.left));
    let top = r.top.clamp(bounds.top, (bounds.bottom - h).max(bounds.top));
    PixelRect { left, top, right: left + w, bottom: top + h }
}

impl Staging<'_> {
    fn current_monitor(&self) -> Monitor {
        best_monitor(&self.rect, &self.monitors).clone()
    }

    fn compute_frame_window_rect(&self) -> PixelRect {
        let m = self.current_monitor();
        let k = m.scale();
        let pad = (14.0 * k).round() as i32;
        let top_pad = (38.0 * k).round() as i32;
        let min_badge_right = self.rect.left + (155.0 * k).round() as i32;
        PixelRect {
            left: self.rect.left - pad,
            top: self.rect.top - top_pad,
            right: (self.rect.right + pad).max(min_badge_right),
            bottom: self.rect.bottom + pad,
        }
    }

    fn ideal_bar_pos(&self) -> (i32, i32) {
        let m = self.current_monitor();
        let k = m.scale();
        let (bw, bh) = ((BAR_W * k).round() as i32, (BAR_H * k).round() as i32);
        let gap = (12.0 * k).round() as i32;
        let work = m.work_area;
        let cx = (self.rect.left + self.rect.right) / 2;
        let x = (cx - bw / 2).clamp(work.left + gap, (work.right - bw - gap).max(work.left + gap));
        let below = self.rect.bottom + gap;
        let above = self.rect.top - gap - bh;
        let y = if below + bh <= work.bottom - gap {
            below
        } else if above >= work.top + gap {
            above
        } else {
            (work.bottom - bh - (20.0 * k).round() as i32).max(work.top + gap)
        };
        (x, y)
    }

    fn sync_bar_position(&mut self) {
        if self.bar_detached {
            return;
        }
        let m = self.current_monitor();
        let k = m.scale();
        if (k - self.bar_scale).abs() > 0.01 {
            self.bar_scale = k;
            self.bar_size = ((BAR_W * k).round() as u32, (BAR_H * k).round() as u32);
            self.bar_gfx = None;
        }
        let (x, y) = self.ideal_bar_pos();
        unsafe {
            let _ = SetWindowPos(
                self.bar_hwnd,
                None,
                x,
                y,
                self.bar_size.0 as i32,
                self.bar_size.1 as i32,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
            let _ = InvalidateRect(Some(self.bar_hwnd), None, false);
        }
    }

    fn finish(&mut self, outcome: StagingOutcome) {
        self.outcome.get_or_insert(outcome);
    }

    fn commit_start(&mut self) {
        self.finish(StagingOutcome::Start {
            rect: self.rect,
            window: self.window,
            desktop_audio: self.desktop_audio,
            microphone: self.microphone,
        });
    }

    fn apply_preset(&mut self, preset: Option<(i32, i32)>) {
        let m = self.current_monitor();
        self.window = None;
        self.window_title = None;
        self.rect = match preset {
            None => m.bounds,
            Some((pw, ph)) => {
                let cx = (self.rect.left + self.rect.right) / 2;
                let cy = (self.rect.top + self.rect.bottom) / 2;
                let r = PixelRect {
                    left: cx - pw / 2,
                    top: cy - ph / 2,
                    right: cx + pw / 2,
                    bottom: cy + ph / 2,
                };
                clamp_even(r, m.bounds)
            }
        };
        let _ = self.paint_frame();
        self.sync_bar_position();
    }

    fn grip_at(&self, x: i32, y: i32) -> Option<Grip> {
        let k = self.current_monitor().scale();
        let tol = (9.0 * k).round() as i32;
        let r = self.rect;
        let near = |a: i32, b: i32| (a - b).abs() <= tol;
        let in_x = x >= r.left - tol && x <= r.right + tol;
        let in_y = y >= r.top - tol && y <= r.bottom + tol;
        let (l, rr, t, b) = (near(x, r.left), near(x, r.right), near(y, r.top), near(y, r.bottom));
        if l && t {
            Some(Grip::NW)
        } else if rr && t {
            Some(Grip::NE)
        } else if l && b {
            Some(Grip::SW)
        } else if rr && b {
            Some(Grip::SE)
        } else if self.badge_rect.is_some_and(|br| br.contains(x, y)) {
            Some(Grip::Move)
        } else if t && in_x {
            if (x - (r.left + r.right) / 2).abs() <= (16.0 * k) as i32 {
                Some(Grip::N)
            } else {
                Some(Grip::Move)
            }
        } else if b && in_x {
            if (x - (r.left + r.right) / 2).abs() <= (16.0 * k) as i32 {
                Some(Grip::S)
            } else {
                Some(Grip::Move)
            }
        } else if l && in_y {
            if (y - (r.top + r.bottom) / 2).abs() <= (16.0 * k) as i32 {
                Some(Grip::W)
            } else {
                Some(Grip::Move)
            }
        } else if rr && in_y {
            if (y - (r.top + r.bottom) / 2).abs() <= (16.0 * k) as i32 {
                Some(Grip::E)
            } else {
                Some(Grip::Move)
            }
        } else {
            None
        }
    }

    fn cursor_for_grip(grip: Option<Grip>) -> PCWSTR {
        match grip {
            Some(Grip::NW | Grip::SE) => IDC_SIZENWSE,
            Some(Grip::NE | Grip::SW) => IDC_SIZENESW,
            Some(Grip::N | Grip::S) => IDC_SIZENS,
            Some(Grip::E | Grip::W) => IDC_SIZEWE,
            Some(Grip::Move) => IDC_SIZEALL,
            None => IDC_ARROW,
        }
    }

    fn apply_drag(&mut self, grip: Grip, start: PixelRect, from: (i32, i32), cur: (i32, i32)) {
        self.window = None;
        self.window_title = None;
        let (dx, dy) = (cur.0 - from.0, cur.1 - from.1);
        if grip == Grip::Move {
            let target_mon = monitor_at(cur.0, cur.1, &self.monitors).bounds;
            let moved = start.offset(dx, dy);
            self.rect = clamp_even(moved, target_mon);
        } else {
            let m = best_monitor(&start, &self.monitors).bounds;
            let (mut l, mut t, mut rr, mut bb) = (start.left, start.top, start.right, start.bottom);
            if matches!(grip, Grip::W | Grip::NW | Grip::SW) {
                l = (l + dx).clamp(m.left, rr - MIN_SIDE);
                l = rr - ((rr - l) & !1).max(MIN_SIDE);
            }
            if matches!(grip, Grip::E | Grip::NE | Grip::SE) {
                rr = (rr + dx).clamp(l + MIN_SIDE, m.right);
                rr = l + ((rr - l) & !1).max(MIN_SIDE);
            }
            if matches!(grip, Grip::N | Grip::NE | Grip::NW) {
                t = (t + dy).clamp(m.top, bb - MIN_SIDE);
                t = bb - ((bb - t) & !1).max(MIN_SIDE);
            }
            if matches!(grip, Grip::S | Grip::SE | Grip::SW) {
                bb = (bb + dy).clamp(t + MIN_SIDE, m.bottom);
                bb = t + ((bb - t) & !1).max(MIN_SIDE);
            }
            self.rect = clamp_even(PixelRect { left: l, top: t, right: rr, bottom: bb }, m);
        }
        let _ = self.paint_frame();
        self.sync_bar_position();
    }

    fn on_key(&mut self, vk: u16) {
        let step = if key_down(VK_SHIFT.0) { 10 } else { 2 };
        let ctrl = key_down(VK_CONTROL.0);
        match vk {
            v if v == VK_ESCAPE.0 => self.finish(StagingOutcome::Cancelled),
            v if v == VK_RETURN.0 => self.commit_start(),
            0x46 => self.apply_preset(None), // F = Full screen
            v if [VK_LEFT.0, VK_RIGHT.0, VK_UP.0, VK_DOWN.0].contains(&v) => {
                self.window = None;
                self.window_title = None;
                let m = self.current_monitor().bounds;
                let (dx, dy) = match v {
                    v if v == VK_LEFT.0 => (-step, 0),
                    v if v == VK_RIGHT.0 => (step, 0),
                    v if v == VK_UP.0 => (0, -step),
                    _ => (0, step),
                };
                let next = if ctrl {
                    PixelRect {
                        left: self.rect.left,
                        top: self.rect.top,
                        right: self.rect.right + dx,
                        bottom: self.rect.bottom + dy,
                    }
                } else {
                    self.rect.offset(dx, dy)
                };
                self.rect = clamp_even(next, m);
                let _ = self.paint_frame();
                self.sync_bar_position();
            }
            _ => {}
        }
    }

    fn tick(&mut self) {
        match self.signal.swap(SIGNAL_NONE, Ordering::AcqRel) {
            SIGNAL_START => {
                self.commit_start();
                return;
            }
            SIGNAL_CANCEL => {
                self.finish(StagingOutcome::Cancelled);
                return;
            }
            _ => {}
        }

        // Follow the selected window while locked to it.
        if let Some(raw) = self.window
            && self.drag.is_none()
        {
            let hwnd = HWND(raw as _);
            let alive = unsafe { IsWindow(Some(hwnd)).as_bool() };
            if !alive {
                self.window = None;
                self.window_title = None;
                let _ = self.paint_frame();
            } else if !unsafe { IsIconic(hwnd).as_bool() } {
                let mut wr = RECT::default();
                let ok = unsafe {
                    DwmGetWindowAttribute(
                        hwnd,
                        DWMWA_EXTENDED_FRAME_BOUNDS,
                        &mut wr as *mut _ as *mut c_void,
                        size_of::<RECT>() as u32,
                    )
                    .is_ok()
                };
                if ok {
                    let tracked = PixelRect::from(wr);
                    if !tracked.is_empty() && tracked != self.rect {
                        self.monitors = monitors::enumerate();
                        self.rect = tracked;
                        let _ = self.paint_frame();
                        self.sync_bar_position();
                    }
                }
            }
        }

        self.levels = (self.poll_audio)(self.desktop_audio, self.microphone);
        let dp = self.levels.desktop_peak.unwrap_or(0.0).clamp(0.0, 1.0);
        let mp = self.levels.mic_peak.unwrap_or(0.0).clamp(0.0, 1.0);
        self.desktop_meter = dp.max(self.desktop_meter * 0.78);
        self.mic_meter = mp.max(self.mic_meter * 0.78);
        unsafe {
            let _ = InvalidateRect(Some(self.bar_hwnd), None, false);
        }
    }

    // ----- Layered frame drawing ----------------------------------------------

    fn paint_frame(&mut self) -> Result<()> {
        let m = self.current_monitor();
        let k = m.scale();
        let fw = self.compute_frame_window_rect();
        self.frame_win_rect = fw;
        let (ww, hh) = (fw.width().max(1), fw.height().max(1));

        let need_gfx = self.frame_gfx.as_ref().is_none_or(|g| (g.scale - k).abs() > 0.01);
        if need_gfx {
            let prev_dib = self.frame_gfx.take().and_then(|g| g.dib);
            self.frame_gfx = Some(FrameGfx {
                target: self.factories.dc_target(96.0)?,
                label: self.factories.text_format(
                    12.0 * k,
                    d2d::DWRITE_FONT_WEIGHT_SEMI_BOLD,
                    d2d::DWRITE_PARAGRAPH_ALIGNMENT_CENTER,
                    d2d::DWRITE_TEXT_ALIGNMENT_LEADING,
                )?,
                scale: k,
                dib: prev_dib,
            });
        }
        let g = self.frame_gfx.as_mut().expect("just created");
        if g.dib.as_ref().is_none_or(|d| d.size != (ww, hh)) {
            g.dib = Some(unsafe { DibSurface::new(ww, hh)? });
        }
        let dib = g.dib.as_ref().expect("dib created");

        let rx = (self.rect.left - fw.left) as f32;
        let ry = (self.rect.top - fw.top) as f32;
        let rw = self.rect.width() as f32;
        let rh = self.rect.height() as f32;
        let [hr, hg, hb] = self.highlight;

        let size_str = format!("{} × {}", self.rect.width() & !1, self.rect.height() & !1);
        let badge_text = match &self.window_title {
            Some(title) if self.window.is_some() => {
                let short: String = title.chars().take(34).collect();
                let ell = if title.chars().count() > 34 { "…" } else { "" };
                format!("Window · {short}{ell}   {size_str}")
            }
            _ if self.window.is_some() => format!("Window   {size_str}"),
            _ => size_str,
        };
        let text_w = self.factories.text_width(&badge_text, &g.label);
        let badge_w = (text_w + 30.0 * k).min(rw.max(140.0 * k));
        let badge_h = 24.0 * k;
        let badge_x = rx;
        let badge_y = if self.rect.top - (badge_h as i32) - (6.0 * k) as i32 >= m.bounds.top {
            ry - badge_h - 5.0 * k
        } else {
            ry + 6.0 * k
        };
        self.badge_rect = Some(PixelRect {
            left: fw.left + badge_x as i32,
            top: fw.top + badge_y as i32,
            right: fw.left + (badge_x + badge_w) as i32,
            bottom: fw.top + (badge_y + badge_h) as i32,
        });

        // SAFETY: BindDC -> BeginDraw -> EndDraw -> UpdateLayeredWindow on our memory DC.
        unsafe {
            g.target.BindDC(dib.dc, &RECT { left: 0, top: 0, right: ww, bottom: hh })?;
            let t: ID2D1RenderTarget = g.target.cast()?;
            let hit_brush = d2d::brush(&t, d2d::rgba(0, 0, 0, 0.012))?;
            let shadow_brush = d2d::brush(&t, d2d::rgba(0, 0, 0, 0.55))?;
            let accent_brush = d2d::brush(&t, d2d::rgb(hr, hg, hb))?;
            let white_brush = d2d::brush(&t, d2d::rgb(255, 255, 255))?;
            let pill_brush = d2d::brush(&t, d2d::rgba(20, 22, 27, 0.92))?;
            let dot_brush = d2d::brush(&t, d2d::rgba(255, 255, 255, 0.55))?;

            t.BeginDraw();
            t.Clear(Some(&d2d::rgba(0, 0, 0, 0.0)));

            // Nearly-invisible hit band along the 4 edges so grabbing edges is effortless,
            // while the interior stays alpha=0 (click-through to live windows!).
            let band = 8.0 * k;
            for edge in [
                d2d::rect(rx - band, ry - band, rx + rw + band, ry + band),
                d2d::rect(rx - band, ry + rh - band, rx + rw + band, ry + rh + band),
                d2d::rect(rx - band, ry + band, rx + band, ry + rh - band),
                d2d::rect(rx + rw - band, ry + band, rx + rw + band, ry + rh - band),
            ] {
                t.FillRectangle(&edge, &hit_brush);
            }

            let stroke = (f32::from(self.border_width) * k).round().max(1.0);
            t.DrawRectangle(
                &d2d::rect(rx - 1.0, ry - 1.0, rx + rw + 1.0, ry + rh + 1.0),
                &shadow_brush,
                stroke + 2.0,
                None,
            );
            t.DrawRectangle(&d2d::rect(rx, ry, rx + rw, ry + rh), &accent_brush, stroke, None);

            // Top-left move & dimensions pill.
            let br = d2d::rect(badge_x, badge_y, badge_x + badge_w, badge_y + badge_h);
            t.FillRoundedRectangle(
                &D2D1_ROUNDED_RECT { rect: br, radiusX: 6.0 * k, radiusY: 6.0 * k },
                &pill_brush,
            );
            // 6-dot move grip inside the pill.
            for row in 0..3 {
                for col in 0..2 {
                    let gx = badge_x + (8.5 + col as f32 * 4.0) * k;
                    let gy = badge_y + badge_h / 2.0 + (row as f32 - 1.0) * 4.5 * k;
                    t.FillEllipse(
                        &D2D1_ELLIPSE {
                            point: windows_numerics::Vector2 { X: gx, Y: gy },
                            radiusX: 1.2 * k,
                            radiusY: 1.2 * k,
                        },
                        &dot_brush,
                    );
                }
            }
            d2d::draw_text(
                &t,
                &badge_text,
                &g.label,
                d2d::rect(badge_x + 20.0 * k, badge_y, badge_x + badge_w - 6.0 * k, badge_y + badge_h),
                &white_brush,
            );

            // 8 resize handles (corners & mid-edges).
            let (cx, cy) = (rx + rw / 2.0, ry + rh / 2.0);
            let handles = [
                (Grip::NW, rx, ry),
                (Grip::N, cx, ry),
                (Grip::NE, rx + rw, ry),
                (Grip::W, rx, cy),
                (Grip::E, rx + rw, cy),
                (Grip::SW, rx, ry + rh),
                (Grip::S, cx, ry + rh),
                (Grip::SE, rx + rw, ry + rh),
            ];
            for (grip, hx, hy) in handles {
                let active = self.hover_grip == Some(grip)
                    || self.drag.as_ref().is_some_and(|(d, _, _)| *d == grip);
                let rad = (if active { 6.5 } else { 5.0 }) * k;
                let ell = D2D1_ELLIPSE {
                    point: windows_numerics::Vector2 { X: hx, Y: hy },
                    radiusX: rad,
                    radiusY: rad,
                };
                t.FillEllipse(&ell, &white_brush);
                t.DrawEllipse(&ell, &accent_brush, 1.8 * k, None);
            }

            t.EndDraw(None, None)?;

            let blend = BLENDFUNCTION {
                BlendOp: AC_SRC_OVER as u8,
                BlendFlags: 0,
                SourceConstantAlpha: 255,
                AlphaFormat: AC_SRC_ALPHA as u8,
            };
            let dst = POINT { x: fw.left, y: fw.top };
            let sz = SIZE { cx: ww, cy: hh };
            let src = POINT { x: 0, y: 0 };
            UpdateLayeredWindow(
                self.frame_hwnd,
                None,
                Some(&dst),
                Some(&sz),
                Some(dib.dc),
                Some(&src),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            )?;
        }
        Ok(())
    }

    // ----- Pre-record control bar ---------------------------------------------

    fn bar_buttons() -> [(Btn, D2D_RECT_F); 8] {
        let b = |x: f32, w: f32| d2d::rect(x, BTN_Y, x + w, BTN_Y + BTN_H);
        [
            (Btn::Reselect, b(22.0, 68.0)),
            (Btn::Full, b(94.0, 42.0)),
            (Btn::P1080, b(140.0, 48.0)),
            (Btn::P720, b(192.0, 44.0)),
            (Btn::Desktop, b(252.0, 54.0)),
            (Btn::Mic, b(310.0, 54.0)),
            (Btn::Start, b(380.0, 70.0)),
            (Btn::Cancel, b(454.0, 30.0)),
        ]
    }

    fn bar_hit(x: f32, y: f32) -> Option<Btn> {
        Self::bar_buttons()
            .into_iter()
            .find(|(_, r)| x >= r.left && x < r.right && y >= r.top && y < r.bottom)
            .map(|(b, _)| b)
    }

    fn on_bar_click(&mut self, btn: Btn) {
        match btn {
            Btn::Reselect => self.finish(StagingOutcome::Reselect {
                desktop_audio: self.desktop_audio,
                microphone: self.microphone,
            }),
            Btn::Full => self.apply_preset(None),
            Btn::P1080 => self.apply_preset(Some((1920, 1080))),
            Btn::P720 => self.apply_preset(Some((1280, 720))),
            Btn::Desktop => {
                self.desktop_audio = !self.desktop_audio;
                if !self.desktop_audio {
                    self.desktop_meter = 0.0;
                }
                self.levels = (self.poll_audio)(self.desktop_audio, self.microphone);
            }
            Btn::Mic => {
                self.microphone = !self.microphone;
                if !self.microphone {
                    self.mic_meter = 0.0;
                }
                self.levels = (self.poll_audio)(self.desktop_audio, self.microphone);
            }
            Btn::Start => self.commit_start(),
            Btn::Cancel => self.finish(StagingOutcome::Cancelled),
        }
        unsafe {
            let _ = InvalidateRect(Some(self.bar_hwnd), None, false);
        }
    }

    fn paint_bar(&mut self) -> Result<()> {
        if self.bar_gfx.is_none() {
            let target = self.factories.hwnd_target(self.bar_hwnd, self.bar_size, 96.0 * self.bar_scale)?;
            self.bar_gfx = Some(BarGfx {
                text: self.factories.text_format(
                    12.5,
                    d2d::DWRITE_FONT_WEIGHT_NORMAL,
                    d2d::DWRITE_PARAGRAPH_ALIGNMENT_CENTER,
                    d2d::DWRITE_TEXT_ALIGNMENT_CENTER,
                )?,
                bold: self.factories.text_format(
                    13.0,
                    d2d::DWRITE_FONT_WEIGHT_SEMI_BOLD,
                    d2d::DWRITE_PARAGRAPH_ALIGNMENT_CENTER,
                    d2d::DWRITE_TEXT_ALIGNMENT_CENTER,
                )?,
                small: self.factories.text_format(
                    11.0,
                    d2d::DWRITE_FONT_WEIGHT_SEMI_BOLD,
                    d2d::DWRITE_PARAGRAPH_ALIGNMENT_CENTER,
                    d2d::DWRITE_TEXT_ALIGNMENT_LEADING,
                )?,
                target,
            });
        }
        let mon_bounds = self.current_monitor().bounds;
        let g = self.bar_gfx.as_ref().expect("just created");
        let t: ID2D1RenderTarget = g.target.cast()?;

        let (bg, text, muted, control, hover, sep) = if self.dark {
            (
                d2d::rgb(28, 31, 37),
                d2d::rgb(232, 235, 240),
                d2d::rgb(138, 147, 160),
                d2d::rgb(42, 46, 55),
                d2d::rgb(55, 60, 71),
                d2d::rgb(52, 57, 68),
            )
        } else {
            (
                d2d::rgb(255, 255, 255),
                d2d::rgb(26, 29, 33),
                d2d::rgb(107, 114, 128),
                d2d::rgb(241, 243, 246),
                d2d::rgb(228, 232, 238),
                d2d::rgb(225, 229, 235),
            )
        };
        let [hr, hg, hb] = self.highlight;
        let accent_soft = d2d::rgba(hr, hg, hb, if self.dark { 0.24 } else { 0.16 });
        let accent = d2d::rgb(hr, hg, hb);
        let red = d2d::rgb(239, 68, 68);
        let red_hover = d2d::rgb(220, 38, 38);
        let green = d2d::rgb(16, 185, 129);
        let amber = d2d::rgb(245, 158, 11);

        let w = self.rect.width() & !1;
        let h = self.rect.height() & !1;
        let is_full = self.window.is_none() && self.rect == mon_bounds;
        let is_1080 = self.window.is_none() && !is_full && w == 1920 && h == 1080;
        let is_720 = self.window.is_none() && !is_full && w == 1280 && h == 720;

        // SAFETY: drawing between BeginDraw/EndDraw with this target's resources.
        unsafe {
            t.BeginDraw();
            t.Clear(Some(&bg));

            let text_b = d2d::brush(&t, text)?;
            let muted_b = d2d::brush(&t, muted)?;
            let sep_b = d2d::brush(&t, sep)?;
            let white_b = d2d::brush(&t, d2d::rgb(255, 255, 255))?;
            let accent_b = d2d::brush(&t, accent)?;

            // Left drag dots.
            for row in 0..3 {
                for col in 0..2 {
                    let gx = 9.0 + col as f32 * 4.5;
                    let gy = BAR_H / 2.0 + (row as f32 - 1.0) * 5.0;
                    t.FillEllipse(
                        &D2D1_ELLIPSE {
                            point: windows_numerics::Vector2 { X: gx, Y: gy },
                            radiusX: 1.3,
                            radiusY: 1.3,
                        },
                        &muted_b,
                    );
                }
            }

            // Dividers.
            for dx in [244.0, 372.0] {
                t.FillRectangle(&d2d::rect(dx, 12.0, dx + 1.0, BAR_H - 12.0), &sep_b);
            }

            for (btn, r) in Self::bar_buttons() {
                let is_hover = self.bar_hover == Some(btn);
                let active = match btn {
                    Btn::Full => is_full,
                    Btn::P1080 => is_1080,
                    Btn::P720 => is_720,
                    Btn::Desktop => self.desktop_audio,
                    Btn::Mic => self.microphone,
                    _ => false,
                };
                let fill_color = match btn {
                    Btn::Start => if is_hover { red_hover } else { red },
                    _ if active => accent_soft,
                    _ if is_hover => hover,
                    _ => control,
                };
                let rr = D2D1_ROUNDED_RECT { rect: r, radiusX: 7.0, radiusY: 7.0 };
                t.FillRoundedRectangle(&rr, &d2d::brush(&t, fill_color)?);
                if active && btn != Btn::Start {
                    t.DrawRoundedRectangle(&rr, &accent_b, 1.0, None);
                }

                match btn {
                    Btn::Reselect => d2d::draw_text(&t, "Reselect", &g.text, r, &text_b),
                    Btn::Full => d2d::draw_text(&t, "Full", if active { &g.bold } else { &g.text }, r, &text_b),
                    Btn::P1080 => d2d::draw_text(&t, "1080p", if active { &g.bold } else { &g.text }, r, &text_b),
                    Btn::P720 => d2d::draw_text(&t, "720p", if active { &g.bold } else { &g.text }, r, &text_b),
                    Btn::Desktop => {
                        draw_audio_pill(&t, r, "Sys", self.desktop_audio, self.desktop_meter, self.levels.desktop_err, &g.small, &text_b, &muted_b, sep, green, amber, red)?;
                    }
                    Btn::Mic => {
                        draw_audio_pill(&t, r, "Mic", self.microphone, self.mic_meter, self.levels.mic_err, &g.small, &text_b, &muted_b, sep, green, amber, red)?;
                    }
                    Btn::Start => {
                        let cy = (r.top + r.bottom) / 2.0;
                        t.FillEllipse(
                            &D2D1_ELLIPSE {
                                point: windows_numerics::Vector2 { X: r.left + 15.0, Y: cy },
                                radiusX: 4.5,
                                radiusY: 4.5,
                            },
                            &white_b,
                        );
                        d2d::draw_text(
                            &t,
                            "Start",
                            &g.bold,
                            d2d::rect(r.left + 21.0, r.top, r.right - 4.0, r.bottom),
                            &white_b,
                        );
                    }
                    Btn::Cancel => d2d::draw_text(&t, "✕", &g.text, r, &muted_b),
                }
            }

            t.EndDraw(None, None)
        }
    }
}

#[allow(clippy::too_many_arguments)]
unsafe fn draw_audio_pill(
    t: &ID2D1RenderTarget,
    r: D2D_RECT_F,
    label: &str,
    enabled: bool,
    meter: f32,
    err: bool,
    font: &IDWriteTextFormat,
    text_b: &windows::Win32::Graphics::Direct2D::ID2D1SolidColorBrush,
    muted_b: &windows::Win32::Graphics::Direct2D::ID2D1SolidColorBrush,
    track: windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F,
    green: windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F,
    amber: windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F,
    red: windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F,
) -> Result<()> {
    unsafe {
        let dot_c = if !enabled {
            track
        } else if err {
            amber
        } else {
            green
        };
        t.FillEllipse(
            &D2D1_ELLIPSE {
                point: windows_numerics::Vector2 { X: r.left + 11.0, Y: r.top + 12.0 },
                radiusX: 3.5,
                radiusY: 3.5,
            },
            &d2d::brush(t, dot_c)?,
        );
        d2d::draw_text(
            t,
            label,
            font,
            d2d::rect(r.left + 19.0, r.top + 2.0, r.right - 4.0, r.top + 21.0),
            if enabled { text_b } else { muted_b },
        );
        // Live mini level meter along bottom of the pill.
        let mx = r.left + 7.0;
        let mw = (r.right - r.left) - 14.0;
        let my = r.bottom - 7.5;
        let bar = |w: f32| D2D1_ROUNDED_RECT {
            rect: d2d::rect(mx, my, mx + w, my + 3.5),
            radiusX: 1.75,
            radiusY: 1.75,
        };
        t.FillRoundedRectangle(&bar(mw), &d2d::brush(t, track)?);
        if enabled && !err {
            let fill_w = (meter.sqrt() * mw).clamp(0.0, mw);
            if fill_w > 0.6 {
                let c = if meter > 0.9 { red } else if meter > 0.5 { amber } else { green };
                t.FillRoundedRectangle(&bar(fill_w), &d2d::brush(t, c)?);
            }
        }
        Ok(())
    }
}

fn register_classes() -> Result<()> {
    static REGISTER: Once = Once::new();
    let mut result = Ok(());
    REGISTER.call_once(|| {
        // SAFETY: window classes with valid wndprocs.
        result = unsafe {
            (|| {
                let hinst = GetModuleHandleW(None)?.into();
                let cursor = LoadCursorW(None, IDC_ARROW)?;
                let frame = WNDCLASSEXW {
                    cbSize: size_of::<WNDCLASSEXW>() as u32,
                    lpfnWndProc: Some(frame_wndproc),
                    hInstance: hinst,
                    hCursor: cursor,
                    lpszClassName: FRAME_CLASS,
                    ..Default::default()
                };
                if RegisterClassExW(&frame) == 0 {
                    return Err(windows::core::Error::from_thread());
                }
                let bar = WNDCLASSEXW {
                    cbSize: size_of::<WNDCLASSEXW>() as u32,
                    style: CS_DROPSHADOW,
                    lpfnWndProc: Some(bar_wndproc),
                    hInstance: hinst,
                    hCursor: cursor,
                    lpszClassName: BAR_CLASS,
                    ..Default::default()
                };
                if RegisterClassExW(&bar) == 0 {
                    return Err(windows::core::Error::from_thread());
                }
                Ok(())
            })()
        };
    });
    result
}

unsafe extern "system" fn frame_wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // SAFETY: GWLP_USERDATA points to live Staging on this thread.
    unsafe {
        if msg == WM_NCCREATE {
            let create = &*(lparam.0 as *const CREATESTRUCTW);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }
        let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Staging<'_>;
        if ptr.is_null() {
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }
        let st = &mut *ptr;
        match msg {
            WM_ERASEBKGND => LRESULT(1),
            WM_SETCURSOR => {
                let grip = st.drag.map(|(g, _, _)| g).or_else(|| {
                    let (cx, cy) = cursor_pos();
                    st.grip_at(cx, cy)
                });
                if let Ok(c) = LoadCursorW(None, Staging::cursor_for_grip(grip)) {
                    SetCursor(Some(c));
                }
                LRESULT(1)
            }
            WM_MOUSEMOVE => {
                let cur = cursor_pos();
                if let Some((grip, start, from)) = st.drag {
                    st.apply_drag(grip, start, from, cur);
                } else {
                    let g = st.grip_at(cur.0, cur.1);
                    if g != st.hover_grip {
                        st.hover_grip = g;
                        let mut tme = TRACKMOUSEEVENT {
                            cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                            dwFlags: TME_LEAVE,
                            hwndTrack: hwnd,
                            dwHoverTime: 0,
                        };
                        let _ = TrackMouseEvent(&mut tme);
                        let _ = st.paint_frame();
                    }
                }
                LRESULT(0)
            }
            WM_MOUSELEAVE => {
                if st.drag.is_none() && st.hover_grip.take().is_some() {
                    let _ = st.paint_frame();
                }
                LRESULT(0)
            }
            WM_LBUTTONDOWN => {
                let cur = cursor_pos();
                if let Some(grip) = st.grip_at(cur.0, cur.1) {
                    SetCapture(hwnd);
                    st.drag = Some((grip, st.rect, cur));
                    let _ = st.paint_frame();
                }
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                if st.drag.take().is_some() {
                    let _ = ReleaseCapture();
                    let _ = st.paint_frame();
                }
                LRESULT(0)
            }
            WM_RBUTTONUP => {
                st.finish(StagingOutcome::Cancelled);
                LRESULT(0)
            }
            WM_KEYDOWN => {
                st.on_key(wparam.0 as u16);
                LRESULT(0)
            }
            WM_CLOSE => {
                st.finish(StagingOutcome::Cancelled);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

unsafe extern "system" fn bar_wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // SAFETY: GWLP_USERDATA points to live Staging on this thread.
    unsafe {
        if msg == WM_NCCREATE {
            let create = &*(lparam.0 as *const CREATESTRUCTW);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }
        let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Staging<'_>;
        if ptr.is_null() {
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }
        let st = &mut *ptr;
        let client = |lparam: LPARAM| {
            let x = (lparam.0 & 0xffff) as i16 as f32;
            let y = ((lparam.0 >> 16) & 0xffff) as i16 as f32;
            (x / st.bar_scale, y / st.bar_scale)
        };
        match msg {
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                BeginPaint(hwnd, &mut ps);
                if st.paint_bar().is_err() {
                    st.bar_gfx = None;
                }
                let _ = EndPaint(hwnd, &ps);
                LRESULT(0)
            }
            WM_ERASEBKGND => LRESULT(1),
            WM_TIMER => {
                st.tick();
                LRESULT(0)
            }
            WM_ENTERSIZEMOVE => {
                st.bar_detached = true;
                LRESULT(0)
            }
            WM_DPICHANGED => {
                let dpi = ((wparam.0 >> 16) & 0xffff) as u32;
                let k = (dpi as f32 / 96.0).max(0.5);
                if (k - st.bar_scale).abs() > 0.01 {
                    st.bar_scale = k;
                    st.bar_size = ((BAR_W * k).round() as u32, (BAR_H * k).round() as u32);
                    st.bar_gfx = None;
                    let suggested = &*(lparam.0 as *const RECT);
                    let _ = SetWindowPos(
                        hwnd,
                        None,
                        suggested.left,
                        suggested.top,
                        st.bar_size.0 as i32,
                        st.bar_size.1 as i32,
                        SWP_NOZORDER | SWP_NOACTIVATE,
                    );
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
                LRESULT(0)
            }
            WM_NCHITTEST => {
                let mut p = POINT {
                    x: (lparam.0 & 0xffff) as i16 as i32,
                    y: ((lparam.0 >> 16) & 0xffff) as i16 as i32,
                };
                let _ = ScreenToClient(hwnd, &mut p);
                if Staging::bar_hit(p.x as f32 / st.bar_scale, p.y as f32 / st.bar_scale).is_none() {
                    LRESULT(HTCAPTION as isize)
                } else {
                    LRESULT(HTCLIENT as isize)
                }
            }
            WM_SETCURSOR => {
                let cursor = if st.bar_hover.is_some() { IDC_HAND } else { IDC_ARROW };
                if let Ok(c) = LoadCursorW(None, cursor) {
                    SetCursor(Some(c));
                }
                LRESULT(1)
            }
            WM_MOUSEMOVE => {
                let (x, y) = client(lparam);
                let hit = Staging::bar_hit(x, y);
                if hit != st.bar_hover {
                    st.bar_hover = hit;
                    let mut tme = TRACKMOUSEEVENT {
                        cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: hwnd,
                        dwHoverTime: 0,
                    };
                    let _ = TrackMouseEvent(&mut tme);
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
                LRESULT(0)
            }
            WM_MOUSELEAVE => {
                if st.bar_hover.take().is_some() {
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
                LRESULT(0)
            }
            WM_LBUTTONDOWN => {
                let (x, y) = client(lparam);
                st.bar_pressed = Staging::bar_hit(x, y);
                if st.bar_pressed.is_some() {
                    SetCapture(hwnd);
                }
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                let _ = ReleaseCapture();
                let (x, y) = client(lparam);
                if let (Some(pressed), Some(btn)) = (st.bar_pressed.take(), Staging::bar_hit(x, y))
                    && pressed == btn
                {
                    st.on_bar_click(btn);
                }
                LRESULT(0)
            }
            WM_KEYDOWN => {
                st.on_key(wparam.0 as u16);
                LRESULT(0)
            }
            WM_CLOSE => {
                st.finish(StagingOutcome::Cancelled);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}
