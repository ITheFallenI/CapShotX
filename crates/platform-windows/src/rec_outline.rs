//! Live capture-excluded recording outline shown around the recorded region
//! or window so the user always sees what is in frame without any border
//! appearing in the saved MP4.
//!
//! Two independent layers keep the outline out of the video:
//! 1. `SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE)` instructs DWM
//!    to omit the window from all screen/window captures.
//! 2. Whenever display space exists outside an edge, the stroke and its subtle
//!    contrast halo are placed strictly *outside* `[rect.left, rect.right) ×
//!    [rect.top, rect.bottom)`, so zero recorded pixels are covered on screen.
//!    When an edge is flush with the display border (e.g. full-screen capture)
//!    and `WDA_EXCLUDEFROMCAPTURE` is active, that edge hugs the inside of the
//!    bezel so the user still sees the indicator.

use std::ffi::c_void;
use std::sync::{Once, mpsc};

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Direct2D::{
    D2D1_ROUNDED_RECT, ID2D1DCRenderTarget, ID2D1RenderTarget,
};
use windows::Win32::Graphics::Dwm::{DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute};
use windows::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION,
    CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, HBITMAP, HDC,
    HGDIOBJ, SelectObject,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CS_HREDRAW, CS_VREDRAW, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
    GWLP_USERDATA, GetMessageW, GetWindowLongPtrW, IDC_ARROW, IsIconic, IsWindow, KillTimer,
    LoadCursorW, MSG, PostMessageW, PostQuitMessage, RegisterClassExW, SW_HIDE, SW_SHOWNOACTIVATE,
    SetTimer, SetWindowDisplayAffinity, SetWindowLongPtrW, ShowWindow, TranslateMessage, ULW_ALPHA,
    UpdateLayeredWindow, WDA_EXCLUDEFROMCAPTURE, WM_CLOSE, WM_ERASEBKGND, WM_NCDESTROY, WM_TIMER,
    WNDCLASSEXW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    WS_EX_TRANSPARENT, WS_POPUP,
};
use windows::core::{Interface, PCWSTR, Result, w};

use crate::d2d::{self, Factories};
use crate::monitors::{self, Monitor, PixelRect};

const CLASS_NAME: PCWSTR = w!("CapShotX.RecordingOutline");
const TIMER_ID: usize = 1;
const POLL_MS: u32 = 80;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutlinePhase {
    Countdown,
    Recording,
    Paused,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OutlineStatus {
    pub phase: OutlinePhase,
    pub visible: bool,
}

type StatusFn = Box<dyn Fn() -> OutlineStatus + Send>;

/// Keeps the live recording outline open; dropping it destroys the window.
pub struct RecordingOutline {
    hwnd: isize,
    excluded: bool,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl RecordingOutline {
    /// Opens a click-through, non-activating, capture-excluded outline around
    /// `rect` (tracking `window` if `Some`).
    pub fn show(
        rect: PixelRect,
        window: Option<isize>,
        highlight: [u8; 3],
        status: impl Fn() -> OutlineStatus + Send + 'static,
    ) -> Result<Self> {
        let (tx, rx) = mpsc::channel();
        let status: StatusFn = Box::new(status);
        let thread = std::thread::Builder::new()
            .name("rec-outline".into())
            .spawn(move || run(rect, window, highlight, status, tx))
            .map_err(|_| windows::core::Error::from_thread())?;
        match rx.recv() {
            Ok(Ok((hwnd, excluded))) => Ok(Self { hwnd, excluded, thread: Some(thread) }),
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(e)
            }
            Err(_) => {
                let _ = thread.join();
                Err(windows::core::Error::from_thread())
            }
        }
    }

    /// Whether Windows accepted `WDA_EXCLUDEFROMCAPTURE` on this window.
    pub fn is_excluded_from_capture(&self) -> bool {
        self.excluded
    }
}

impl Drop for RecordingOutline {
    fn drop(&mut self) {
        // SAFETY: posting WM_CLOSE to our thread's window; harmless if already closed.
        let _ = unsafe { PostMessageW(Some(HWND(self.hwnd as _)), WM_CLOSE, WPARAM(0), LPARAM(0)) };
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

struct DibSurface {
    dc: HDC,
    bmp: HBITMAP,
    old: HGDIOBJ,
    size: (i32, i32),
}

impl DibSurface {
    unsafe fn new(w: i32, h: i32) -> Result<Self> {
        // SAFETY: paired GDI allocations released in Drop.
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
        // SAFETY: restores original object before deleting DIB and DC.
        unsafe {
            let _ = SelectObject(self.dc, self.old);
            let _ = DeleteObject(self.bmp.into());
            let _ = DeleteDC(self.dc);
        }
    }
}

struct State {
    factories: Factories,
    target: ID2D1DCRenderTarget,
    dib: Option<DibSurface>,
    monitors: Vec<Monitor>,
    rect: PixelRect,
    window: Option<isize>,
    highlight: [u8; 3],
    excluded: bool,
    status_fn: StatusFn,
    shown: bool,
    last_drawn: Option<(PixelRect, OutlinePhase)>,
}

fn run(
    rect: PixelRect,
    window: Option<isize>,
    highlight: [u8; 3],
    status_fn: StatusFn,
    ready: mpsc::Sender<Result<(isize, bool)>>,
) {
    // SAFETY: standard top-level layered window + message loop on this thread;
    // `Box<State>` is attached after creation and freed in WM_NCDESTROY.
    unsafe {
        let created = (|| -> Result<(HWND, bool)> {
            register_class()?;
            let factories = Factories::new()?;
            let target = factories.dc_target(96.0)?;
            let monitors = monitors::enumerate();

            let hwnd = CreateWindowExW(
                WS_EX_LAYERED
                    | WS_EX_TRANSPARENT
                    | WS_EX_TOPMOST
                    | WS_EX_TOOLWINDOW
                    | WS_EX_NOACTIVATE,
                CLASS_NAME,
                w!("CapShotX recording outline"),
                WS_POPUP,
                rect.left,
                rect.top,
                rect.width().max(1),
                rect.height().max(1),
                None,
                None,
                Some(GetModuleHandleW(None)?.into()),
                None,
            )?;

            let excluded = SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE).is_ok();
            if !excluded {
                tracing::warn!("recording outline could not set WDA_EXCLUDEFROMCAPTURE; keeping strictly outside bounds");
            }

            let mut state = Box::new(State {
                factories,
                target,
                dib: None,
                monitors,
                rect,
                window,
                highlight,
                excluded,
                status_fn,
                shown: false,
                last_drawn: None,
            });
            state.tick(hwnd);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(state) as isize);
            SetTimer(Some(hwnd), TIMER_ID, POLL_MS, None);
            Ok((hwnd, excluded))
        })();

        match created {
            Ok((hwnd, excluded)) => {
                let _ = ready.send(Ok((hwnd.0 as isize, excluded)));
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

fn best_monitor<'a>(rect: &PixelRect, monitors: &'a [Monitor]) -> Option<&'a Monitor> {
    let area = |m: &Monitor| {
        rect.intersect(&m.bounds)
            .map_or(0i64, |r| r.width() as i64 * r.height() as i64)
    };
    monitors.iter().max_by_key(|m| (area(m), m.primary))
}

impl State {
    fn set_shown(&mut self, hwnd: HWND, want: bool) {
        if self.shown == want {
            return;
        }
        self.shown = want;
        // SAFETY: toggling visibility on our own non-activating window.
        unsafe {
            let _ = ShowWindow(hwnd, if want { SW_SHOWNOACTIVATE } else { SW_HIDE });
        }
    }

    fn tick(&mut self, hwnd: HWND) {
        let status = (self.status_fn)();
        if !status.visible {
            self.set_shown(hwnd, false);
            return;
        }

        if let Some(raw) = self.window {
            let target = HWND(raw as _);
            // SAFETY: plain window state queries on target HWND.
            let (alive, iconic) = unsafe {
                (IsWindow(Some(target)).as_bool(), IsIconic(target).as_bool())
            };
            if !alive || iconic {
                self.set_shown(hwnd, false);
                return;
            }
            let mut wr = RECT::default();
            // SAFETY: querying extended frame bounds into local RECT.
            let ok = unsafe {
                DwmGetWindowAttribute(
                    target,
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
                }
            }
        }

        if self.rect.is_empty() {
            self.set_shown(hwnd, false);
            return;
        }

        let key = (self.rect, status.phase);
        if self.last_drawn != Some(key) {
            if let Err(e) = self.paint(hwnd, status.phase) {
                tracing::debug!("recording outline paint failed: {e}");
                if let Ok(t) = self.factories.dc_target(96.0) {
                    self.target = t;
                }
                return;
            }
            self.last_drawn = Some(key);
        }
        self.set_shown(hwnd, true);
    }

    fn paint(&mut self, hwnd: HWND, phase: OutlinePhase) -> Result<()> {
        let (k, mon_bounds) = match best_monitor(&self.rect, &self.monitors) {
            Some(m) => (m.scale(), Some(m.bounds)),
            None => (1.0, None),
        };
        let stroke = (2.0 * k).round().max(2.0);
        let half = stroke / 2.0;
        let thick = (stroke * 0.5).max(1.0);
        let gap = 1.0;
        let pad = (stroke as i32 * 2 + 6).max(12);

        let fw = PixelRect {
            left: self.rect.left - pad,
            top: self.rect.top - pad,
            right: self.rect.right + pad,
            bottom: self.rect.bottom + pad,
        };
        let (ww, hh) = (fw.width().max(1), fw.height().max(1));
        if self.dib.as_ref().is_none_or(|d| d.size != (ww, hh)) {
            self.dib = Some(unsafe { DibSurface::new(ww, hh)? });
        }
        let dib = self.dib.as_ref().expect("dib created");

        let rx = (self.rect.left - fw.left) as f32;
        let ry = (self.rect.top - fw.top) as f32;
        let rw = self.rect.width() as f32;
        let rh = self.rect.height() as f32;

        // Default placement: strictly outside [rx, rx + rw) × [ry, ry + rh) so
        // even the 1 px contrast halo stops at rx / ry / rx+rw / ry+rh.
        let mut left = rx - gap - half;
        let mut top = ry - gap - half;
        let mut right = rx + rw + gap + half;
        let mut bottom = ry + rh + gap + half;

        // When WDA_EXCLUDEFROMCAPTURE is active and an edge touches the monitor
        // boundary (e.g. full-screen recording), pull that edge inside the bezel
        // so the user still sees the outline while DWM hides it from capture.
        if self.excluded
            && let Some(mb) = mon_bounds
        {
            let ml = (mb.left - fw.left) as f32;
            let mt = (mb.top - fw.top) as f32;
            let mr = (mb.right - fw.left) as f32;
            let mbot = (mb.bottom - fw.top) as f32;
            let inset = half + thick + 1.0;
            left = left.max(ml + inset);
            top = top.max(mt + inset);
            right = right.min(mr - inset);
            bottom = bottom.min(mbot - inset);
        }

        let [hr, hg, hb] = self.highlight;
        let color = match phase {
            OutlinePhase::Countdown => d2d::rgba(hr, hg, hb, 0.88),
            OutlinePhase::Recording => d2d::rgba(239, 68, 68, 0.92),
            OutlinePhase::Paused => d2d::rgba(245, 158, 11, 0.92),
        };
        let radius = gap + half;

        // SAFETY: BindDC -> BeginDraw -> EndDraw -> UpdateLayeredWindow on our memory DC.
        unsafe {
            self.target
                .BindDC(dib.dc, &RECT { left: 0, top: 0, right: ww, bottom: hh })?;
            let t: ID2D1RenderTarget = self.target.cast()?;
            let halo_b = d2d::brush(&t, d2d::rgba(0, 0, 0, 0.45))?;
            let border_b = d2d::brush(&t, color)?;

            t.BeginDraw();
            t.Clear(Some(&d2d::rgba(0, 0, 0, 0.0)));

            let rr = D2D1_ROUNDED_RECT {
                rect: d2d::rect(left, top, right, bottom),
                radiusX: radius,
                radiusY: radius,
            };
            t.DrawRoundedRectangle(&rr, &halo_b, stroke + 2.0, None);
            t.DrawRoundedRectangle(&rr, &border_b, stroke, None);

            // Subtle corner L-accents outside the stroke so corners read at a glance.
            let arm = (12.0 * k).round().min(rw * 0.25).min(rh * 0.25);
            if arm >= 6.0 {
                let l0 = left - half - thick;
                let t0 = top - half - thick;
                let r0 = right + half + thick;
                let b0 = bottom + half + thick;
                for bar in [
                    d2d::rect(l0, t0, l0 + arm, t0 + thick),
                    d2d::rect(l0, t0, l0 + thick, t0 + arm),
                    d2d::rect(r0 - arm, t0, r0, t0 + thick),
                    d2d::rect(r0 - thick, t0, r0, t0 + arm),
                    d2d::rect(l0, b0 - thick, l0 + arm, b0),
                    d2d::rect(l0, b0 - arm, l0 + thick, b0),
                    d2d::rect(r0 - arm, b0 - thick, r0, b0),
                    d2d::rect(r0 - thick, b0 - arm, r0, b0),
                ] {
                    t.FillRectangle(&bar, &border_b);
                }
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
                hwnd,
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
}

fn register_class() -> Result<()> {
    static REGISTER: Once = Once::new();
    let mut result = Ok(());
    REGISTER.call_once(|| {
        // SAFETY: valid WNDCLASSEXW registration for this process.
        result = unsafe {
            (|| {
                let class = WNDCLASSEXW {
                    cbSize: size_of::<WNDCLASSEXW>() as u32,
                    style: CS_HREDRAW | CS_VREDRAW,
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

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // SAFETY: GWLP_USERDATA owns Box<State> on this thread until WM_NCDESTROY.
    unsafe {
        let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut State;
        match msg {
            WM_ERASEBKGND => LRESULT(1),
            WM_TIMER if !ptr.is_null() => {
                (*ptr).tick(hwnd);
                LRESULT(0)
            }
            WM_CLOSE => {
                let _ = KillTimer(Some(hwnd), TIMER_ID);
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            WM_NCDESTROY => {
                if !ptr.is_null() {
                    SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                    drop(Box::from_raw(ptr));
                }
                PostQuitMessage(0);
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
    use std::time::Duration;

    use windows::Win32::UI::WindowsAndMessaging::GetWindowDisplayAffinity;

    use super::*;

    #[test]
    fn creates_excludes_toggles_and_drops_cleanly() {
        let mon = monitors::enumerate().into_iter().next().expect("monitor");
        let b = mon.bounds;
        let rect = PixelRect {
            left: b.left + 80,
            top: b.top + 80,
            right: (b.left + 400).min(b.right),
            bottom: (b.top + 280).min(b.bottom),
        };
        let visible = Arc::new(AtomicBool::new(true));
        let phase = Arc::new(AtomicU8::new(0));
        let (v2, p2) = (visible.clone(), phase.clone());

        let outline = RecordingOutline::show(rect, None, [110, 123, 255], move || {
            let ph = match p2.load(Ordering::Acquire) {
                0 => OutlinePhase::Countdown,
                1 => OutlinePhase::Recording,
                _ => OutlinePhase::Paused,
            };
            OutlineStatus { phase: ph, visible: v2.load(Ordering::Acquire) }
        })
        .expect("show outline");

        assert!(outline.is_excluded_from_capture());
        let mut affinity = 0u32;
        unsafe {
            GetWindowDisplayAffinity(HWND(outline.hwnd as _), &mut affinity)
                .expect("GetWindowDisplayAffinity");
        }
        assert_eq!(affinity, WDA_EXCLUDEFROMCAPTURE.0);

        phase.store(1, Ordering::Release);
        std::thread::sleep(Duration::from_millis(110));
        visible.store(false, Ordering::Release);
        std::thread::sleep(Duration::from_millis(110));
        phase.store(2, Ordering::Release);
        visible.store(true, Ordering::Release);
        std::thread::sleep(Duration::from_millis(110));

        drop(outline);
    }
}
