//! The frozen-screen selection overlay: one borderless window per monitor,
//! drawn with Direct2D in physical pixels, sharing one selection state in
//! virtual-desktop coordinates.

use std::ffi::c_void;
use std::sync::{Arc, Once};

use platform_windows::d2d::{self, Factories};
use platform_windows::monitors::{Monitor, PixelRect};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::Graphics::Direct2D::{
    D2D1_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR, D2D1_ROUNDED_RECT, ID2D1Bitmap,
    ID2D1HwndRenderTarget, ID2D1RenderTarget,
};
use windows::Win32::Graphics::DirectWrite::IDWriteTextFormat;
use windows::Win32::Graphics::Dwm::{
    DWMWA_TRANSITIONS_FORCEDISABLED, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_DONOTROUND,
    DwmSetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, InvalidateRect, PAINTSTRUCT};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, ReleaseCapture, SetCapture, VK_CONTROL, VK_DOWN, VK_ESCAPE, VK_LEFT, VK_RETURN,
    VK_RIGHT, VK_SHIFT, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CREATESTRUCTW, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GWLP_USERDATA,
    GetCursorPos, GetMessageW, GetWindowLongPtrW, IDC_CROSS, IDC_HAND, IDC_SIZEALL, IDC_SIZENESW,
    IDC_SIZENS, IDC_SIZENWSE, IDC_SIZEWE, KillTimer, LoadCursorW, MSG, RegisterClassExW,
    SW_SHOW, SetCursor, SetCursorPos, SetForegroundWindow, SetTimer, SetWindowLongPtrW, ShowWindow,
    TranslateMessage, WM_ACTIVATEAPP, WM_ERASEBKGND, WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MOUSEMOVE, WM_NCCREATE, WM_PAINT, WM_RBUTTONUP, WM_SETCURSOR, WM_TIMER, WNDCLASSEXW,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};
use windows::core::{BOOL, PCWSTR, Result, w};

use crate::frame::Frame;
use crate::windows_list::WindowInfo;
use crate::{AfterCapture, CaptureMode};

const CLASS_NAME: PCWSTR = w!("CapShotX.CaptureOverlay");
const DIM: f32 = 0.45;
const DRAG_THRESHOLD: i32 = 4;
const MAG_CELLS: i32 = 15;
const MAG_CELL: f32 = 8.0;
const WATCHDOG_TIMER: usize = 1;
const WATCHDOG_MS: u32 = 3000;

/// What to do with a committed selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Delivery {
    Copy,
    Save,
    CopyAndSave,
    Edit,
}

pub enum OverlayResult {
    Cancelled,
    Failed(String),
    CopyColor(String),
    Commit { rect: PixelRect, delivery: Delivery },
    /// Selection-only purposes (recording): the area, and the window (plus
    /// title) if a window was clicked rather than dragged.
    Selected {
        rect: PixelRect,
        window: Option<isize>,
        window_title: Option<String>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Purpose {
    Screenshot,
    /// Pick an area to record; no copy/save, no colour picking.
    Record,
}

pub struct OverlayOptions {
    pub mode: CaptureMode,
    pub after: AfterCapture,
    pub magnifier: bool,
    pub highlight: [u8; 3],
    /// Selection border thickness in pixels at 100% scale.
    pub border_width: u8,
    pub purpose: Purpose,
    pub editor_installed: bool,
}

/// Runs the overlay on the calling thread until the user commits or
/// cancels. Returns the result and the final magnifier state.
pub fn run(
    frame: Arc<Frame>,
    windows: Vec<WindowInfo>,
    monitors: Vec<Monitor>,
    options: OverlayOptions,
) -> (OverlayResult, bool) {
    let magnifier = options.magnifier;
    let mut overlay = match Factories::new() {
        Ok(factories) => Box::new(Overlay {
            frame,
            windows,
            options,
            factories,
            surfaces: Vec::new(),
            phase: Phase::Hover,
            cursor: cursor_pos(),
            pressed_at: None,
            magnifier,
            toolbar_hover: None,
            last_highlight: None,
            activated: false,
            painted: false,
            result: None,
        }),
        Err(err) => return (OverlayResult::Failed(format!("Direct2D unavailable: {err}")), magnifier),
    };

    if let Err(err) = overlay.create_surfaces(monitors) {
        overlay.destroy_surfaces();
        return (OverlayResult::Failed(format!("Couldn't open the capture overlay: {err}")), magnifier);
    }

    // SAFETY: standard message loop; the overlay outlives its windows, which
    // are destroyed (and their user data cleared) before it is dropped.
    unsafe {
        let mut msg = MSG::default();
        while overlay.result.is_none() && GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    overlay.destroy_surfaces();
    let result = overlay.result.take().unwrap_or(OverlayResult::Cancelled);
    (result, overlay.magnifier)
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

#[derive(Clone, Copy, PartialEq)]
enum Phase {
    /// Nothing selected; hovering highlights a window or monitor.
    Hover,
    Dragging { anchor: (i32, i32) },
    /// "Ask" mode: a finished selection with handles and a toolbar.
    Selected { rect: PixelRect },
    Adjusting { grip: Grip, start: PixelRect, from: (i32, i32) },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ToolbarButton {
    Copy,
    Edit,
    Save,
    Cancel,
}

struct SurfaceGfx {
    target: ID2D1HwndRenderTarget,
    frozen: ID2D1Bitmap,
    hint: IDWriteTextFormat,
    label: IDWriteTextFormat,
    button: IDWriteTextFormat,
    small: IDWriteTextFormat,
    hint_width: f32,
}

struct Surface {
    hwnd: HWND,
    monitor: Monitor,
    scale: f32,
    gfx: Option<SurfaceGfx>,
}

struct Overlay {
    frame: Arc<Frame>,
    windows: Vec<WindowInfo>,
    options: OverlayOptions,
    factories: Factories,
    surfaces: Vec<Surface>,
    phase: Phase,
    cursor: (i32, i32),
    pressed_at: Option<(i32, i32)>,
    magnifier: bool,
    toolbar_hover: Option<ToolbarButton>,
    /// Highlight as last painted, to skip repainting unaffected monitors.
    last_highlight: Option<PixelRect>,
    activated: bool,
    painted: bool,
    result: Option<OverlayResult>,
}

fn cursor_pos() -> (i32, i32) {
    platform_windows::shell::cursor_pos()
}

fn mouse_pos(surface_origin: Option<(i32, i32)>, lparam: LPARAM) -> (i32, i32) {
    let mut p = POINT::default();
    // SAFETY: valid out-pointer; falls back to surface-local lparam if UIPI blocks GetCursorPos.
    if unsafe { GetCursorPos(&mut p) }.is_ok() {
        (p.x, p.y)
    } else if let Some((ox, oy)) = surface_origin {
        let lx = (lparam.0 & 0xffff) as i16 as i32;
        let ly = ((lparam.0 >> 16) & 0xffff) as i16 as i32;
        (ox + lx, oy + ly)
    } else {
        platform_windows::shell::cursor_pos()
    }
}

fn key_down(vk: u16) -> bool {
    // SAFETY: GetKeyState has no preconditions.
    unsafe { GetKeyState(vk as i32) < 0 }
}

impl Overlay {
    fn create_surfaces(&mut self, monitors: Vec<Monitor>) -> Result<()> {
        register_class()?;
        let state = self as *mut Overlay as *const c_void;
        // SAFETY: windows store a pointer to this boxed overlay, which
        // outlives them (see `run`).
        unsafe {
            let instance = GetModuleHandleW(None)?;
            for monitor in monitors {
                let b = monitor.bounds;
                let hwnd = CreateWindowExW(
                    WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
                    CLASS_NAME,
                    w!("CapShotX capture"),
                    WS_POPUP,
                    b.left,
                    b.top,
                    b.width(),
                    b.height(),
                    None,
                    None,
                    Some(instance.into()),
                    Some(state),
                )?;
                let disable = BOOL::from(true);
                let _ = DwmSetWindowAttribute(
                    hwnd,
                    DWMWA_TRANSITIONS_FORCEDISABLED,
                    &disable as *const _ as *const c_void,
                    size_of_val(&disable) as u32,
                );
                let corner = DWMWCP_DONOTROUND;
                let _ = DwmSetWindowAttribute(
                    hwnd,
                    DWMWA_WINDOW_CORNER_PREFERENCE,
                    &corner as *const _ as *const c_void,
                    size_of_val(&corner) as u32,
                );
                self.surfaces.push(Surface {
                    hwnd,
                    scale: monitor.scale(),
                    monitor,
                    gfx: None,
                });
            }

            let (cx, cy) = self.cursor;
            let start = self.surface_at(cx, cy).unwrap_or(0);
            for s in &self.surfaces {
                let _ = ShowWindow(s.hwnd, SW_SHOW);
            }
            if let Some(s) = self.surfaces.get(start) {
                let _ = SetForegroundWindow(s.hwnd);
                SetTimer(Some(s.hwnd), WATCHDOG_TIMER, WATCHDOG_MS, None);
            }
        }
        Ok(())
    }

    fn destroy_surfaces(&mut self) {
        for s in self.surfaces.drain(..) {
            // SAFETY: detach our state before destroying so late messages
            // don't reach a dropped overlay.
            unsafe {
                SetWindowLongPtrW(s.hwnd, GWLP_USERDATA, 0);
                let _ = DestroyWindow(s.hwnd);
            }
        }
    }

    fn surface_at(&self, x: i32, y: i32) -> Option<usize> {
        self.surfaces.iter().position(|s| s.monitor.bounds.contains(x, y))
    }

    fn invalidate(&self) {
        for s in &self.surfaces {
            // SAFETY: live windows owned by this overlay.
            unsafe {
                let _ = InvalidateRect(Some(s.hwnd), None, false);
            }
        }
    }

    /// After a pointer move: repaint everything only if the highlight or
    /// toolbar changed; otherwise just the monitors the cursor left and
    /// entered (magnifier and hint live there).
    fn invalidate_after_move(&mut self, previous: (i32, i32)) {
        let highlight = self.highlight();
        let toolbar = self.toolbar_button_at_cursor();
        if highlight != self.last_highlight || toolbar != self.toolbar_hover {
            self.last_highlight = highlight;
            self.toolbar_hover = toolbar;
            self.invalidate();
            return;
        }
        let mut dirty = [self.surface_at(previous.0, previous.1), self.surface_at(self.cursor.0, self.cursor.1)];
        if dirty[0] == dirty[1] {
            dirty[1] = None;
        }
        for i in dirty.into_iter().flatten() {
            // SAFETY: live window owned by this overlay.
            unsafe {
                let _ = InvalidateRect(Some(self.surfaces[i].hwnd), None, false);
            }
        }
    }

    fn finish(&mut self, result: OverlayResult) {
        self.result.get_or_insert(result);
    }

    // ----- targets and geometry -------------------------------------------------

    fn monitor_at_cursor(&self) -> Option<PixelRect> {
        let (x, y) = self.cursor;
        self.surface_at(x, y).map(|i| self.surfaces[i].monitor.bounds)
    }

    /// The front-most window under the cursor (visible Z-order).
    fn hover_window(&self) -> Option<&WindowInfo> {
        if self.options.mode == CaptureMode::Monitor {
            return None;
        }
        let (x, y) = self.cursor;
        self.windows.iter().find(|w| w.rect.contains(x, y))
    }

    /// The window (or monitor) a click would capture.
    fn hover_target(&self) -> Option<PixelRect> {
        self.hover_window()
            .and_then(|w| w.rect.intersect(&self.frame.bounds))
            .or_else(|| self.monitor_at_cursor())
    }

    fn drag_rect(&self, anchor: (i32, i32)) -> PixelRect {
        let limit = if self.options.purpose == Purpose::Record {
            self.surface_at(anchor.0, anchor.1)
                .map_or(self.frame.bounds, |i| self.surfaces[i].monitor.bounds)
        } else {
            self.frame.bounds
        };
        let cx = self.cursor.0.clamp(limit.left, limit.right - 1);
        let cy = self.cursor.1.clamp(limit.top, limit.bottom - 1);
        let mut r = PixelRect::spanning(anchor, (cx, cy));
        if self.options.purpose == Purpose::Record {
            const MIN_REC: i32 = 32;
            let w = (r.width().max(MIN_REC) & !1).min(limit.width() & !1);
            let h = (r.height().max(MIN_REC) & !1).min(limit.height() & !1);
            let left = r.left.clamp(limit.left, (limit.right - w).max(limit.left));
            let top = r.top.clamp(limit.top, (limit.bottom - h).max(limit.top));
            r = PixelRect { left, top, right: left + w, bottom: top + h };
        }
        r.intersect(&limit).unwrap_or(r)
    }

    fn is_drag(&self, anchor: (i32, i32)) -> bool {
        (self.cursor.0 - anchor.0).abs() >= DRAG_THRESHOLD
            || (self.cursor.1 - anchor.1).abs() >= DRAG_THRESHOLD
    }

    /// The rectangle currently shown undimmed.
    fn highlight(&self) -> Option<PixelRect> {
        match self.phase {
            Phase::Hover => self.hover_target(),
            Phase::Dragging { anchor } if self.is_drag(anchor) => Some(self.drag_rect(anchor)),
            Phase::Dragging { .. } => self.hover_target(),
            Phase::Selected { rect } => Some(rect),
            Phase::Adjusting { grip, start, from } => Some(self.adjusted(grip, start, from)),
        }
    }

    fn adjusted(&self, grip: Grip, r: PixelRect, from: (i32, i32)) -> PixelRect {
        let (dx, dy) = (self.cursor.0 - from.0, self.cursor.1 - from.1);
        let b = self.frame.bounds;
        if grip == Grip::Move {
            let dx = dx.clamp(b.left - r.left, b.right - r.right);
            let dy = dy.clamp(b.top - r.top, b.bottom - r.bottom);
            return r.offset(dx, dy);
        }
        let (mut l, mut t, mut rr, mut bb) = (r.left, r.top, r.right, r.bottom);
        if matches!(grip, Grip::W | Grip::NW | Grip::SW) {
            l += dx;
        }
        if matches!(grip, Grip::E | Grip::NE | Grip::SE) {
            rr += dx;
        }
        if matches!(grip, Grip::N | Grip::NE | Grip::NW) {
            t += dy;
        }
        if matches!(grip, Grip::S | Grip::SE | Grip::SW) {
            bb += dy;
        }
        let rect = PixelRect {
            left: l.min(rr - 1),
            top: t.min(bb - 1),
            right: rr.max(l + 1),
            bottom: bb.max(t + 1),
        };
        rect.intersect(&b).unwrap_or(r)
    }

    fn grip_at(&self, r: PixelRect) -> Option<Grip> {
        let (x, y) = self.cursor;
        let scale = self.surface_at(x, y).map_or(1.0, |i| self.surfaces[i].scale);
        let tol = (8.0 * scale) as i32;
        let near = |a: i32, b: i32| (a - b).abs() <= tol;
        let in_x = x >= r.left - tol && x <= r.right + tol;
        let in_y = y >= r.top - tol && y <= r.bottom + tol;
        let (l, rr, t, b) = (near(x, r.left), near(x, r.right), near(y, r.top), near(y, r.bottom));
        Some(match () {
            _ if l && t => Grip::NW,
            _ if rr && t => Grip::NE,
            _ if l && b => Grip::SW,
            _ if rr && b => Grip::SE,
            _ if t && in_x => Grip::N,
            _ if b && in_x => Grip::S,
            _ if l && in_y => Grip::W,
            _ if rr && in_y => Grip::E,
            _ if r.contains(x, y) => Grip::Move,
            _ => return None,
        })
    }

    /// The monitor showing the largest part of `r`, and that part. Labels and
    /// the toolbar go there, so a selection spanning screens shows them once.
    fn dominant_part(&self, r: PixelRect) -> Option<(usize, PixelRect)> {
        self.surfaces
            .iter()
            .enumerate()
            .filter_map(|(i, s)| s.monitor.bounds.intersect(&r).map(|part| (i, part)))
            .max_by_key(|(_, part)| part.width() as i64 * part.height() as i64)
    }

    /// Toolbar container and buttons (virtual coordinates) for a selection.
    fn toolbar(&self, r: PixelRect) -> Option<(usize, PixelRect, Vec<(ToolbarButton, PixelRect)>)> {
        let (si, r) = self.dominant_part(r)?;
        let cx = (r.left + r.right) / 2;
        let s = &self.surfaces[si];
        let m = s.monitor.bounds;
        let px = |v: f32| (v * s.scale).round() as i32;
        let (pad, gap, h) = (px(4.0), px(10.0), px(40.0));
        let specs: &[(ToolbarButton, i32)] = if self.options.editor_installed {
            &[
                (ToolbarButton::Copy, px(78.0)),
                (ToolbarButton::Edit, px(68.0)),
                (ToolbarButton::Save, px(72.0)),
                (ToolbarButton::Cancel, px(40.0)),
            ]
        } else {
            &[
                (ToolbarButton::Copy, px(78.0)),
                (ToolbarButton::Save, px(72.0)),
                (ToolbarButton::Cancel, px(40.0)),
            ]
        };
        let w = specs.iter().map(|(_, bw)| *bw).sum::<i32>() + pad * (specs.len() as i32 + 1);

        let left = (cx - w / 2).clamp(m.left + gap, (m.right - w - gap).max(m.left + gap));
        let below = r.bottom + gap;
        let above = r.top - gap - h;
        let top = if below + h <= m.bottom - gap {
            below
        } else if above >= m.top + gap {
            above
        } else {
            r.bottom - gap - h
        };
        let container = PixelRect { left, top, right: left + w, bottom: top + h };
        let mut x = left + pad;
        let mut buttons = Vec::with_capacity(specs.len());
        for &(kind, width) in specs {
            buttons.push((kind, PixelRect { left: x, top: top + pad, right: x + width, bottom: top + h - pad }));
            x += width + pad;
        }
        Some((si, container, buttons))
    }

    fn toolbar_button_at_cursor(&self) -> Option<ToolbarButton> {
        let Phase::Selected { rect } = self.phase else { return None };
        let (_, _, buttons) = self.toolbar(rect)?;
        let (x, y) = self.cursor;
        buttons.iter().find(|(_, r)| r.contains(x, y)).map(|(b, _)| *b)
    }

    // ----- actions ------------------------------------------------------------

    fn after_delivery(&self) -> Delivery {
        match self.options.after {
            AfterCapture::Copy => Delivery::Copy,
            AfterCapture::Edit if self.options.editor_installed => Delivery::Edit,
            AfterCapture::Edit => Delivery::Copy,
            AfterCapture::CopyAndSave | AfterCapture::Ask => Delivery::CopyAndSave,
        }
    }

    /// A selection was made (drag released or target clicked).
    fn complete(&mut self, rect: Option<PixelRect>, allow_window: bool) {
        let Some(rect) = rect.filter(|r| !r.is_empty()) else { return };
        if self.options.purpose == Purpose::Record {
            // A click on a window records that window (follows it if moved);
            // dragging a box or pressing F records the monitor region.
            let win = allow_window
                .then(|| self.hover_window())
                .flatten()
                .filter(|w| w.rect.intersect(&self.frame.bounds) == Some(rect));
            let window = win.map(|w| w.hwnd);
            let window_title = win
                .map(|w| w.title.trim().to_owned())
                .filter(|t| !t.is_empty());
            self.finish(OverlayResult::Selected { rect, window, window_title });
        } else if self.options.after == AfterCapture::Ask {
            self.phase = Phase::Selected { rect };
        } else {
            let delivery = self.after_delivery();
            self.finish(OverlayResult::Commit { rect, delivery });
        }
    }

    fn commit_current(&mut self, delivery: Delivery) {
        let rect = match self.phase {
            Phase::Selected { rect } => Some(rect),
            _ => self.highlight(),
        };
        if self.options.purpose == Purpose::Record {
            let allow_window = matches!(self.phase, Phase::Hover);
            self.complete(rect, allow_window);
        } else if let Some(rect) = rect {
            self.finish(OverlayResult::Commit { rect, delivery });
        }
    }

    fn on_mouse_down(&mut self, hwnd: HWND) {
        // SAFETY: capture on our own window so drags across monitors keep
        // delivering messages here.
        unsafe { SetCapture(hwnd) };
        self.pressed_at = Some(self.cursor);
        match self.phase {
            Phase::Hover if self.options.mode == CaptureMode::Region => {
                self.phase = Phase::Dragging { anchor: self.cursor };
            }
            Phase::Selected { rect } => {
                if self.toolbar_button_at_cursor().is_some() {
                    return;
                }
                self.phase = match self.grip_at(rect) {
                    Some(grip) => Phase::Adjusting { grip, start: rect, from: self.cursor },
                    None => Phase::Dragging { anchor: self.cursor },
                };
            }
            _ => {}
        }
    }

    fn on_mouse_up(&mut self) {
        // SAFETY: releases the capture taken in on_mouse_down.
        let _ = unsafe { ReleaseCapture() };
        let pressed = self.pressed_at.take();
        match self.phase {
            Phase::Dragging { anchor } => {
                let dragged = self.is_drag(anchor);
                let target = if dragged {
                    Some(self.drag_rect(anchor))
                } else {
                    self.hover_target()
                };
                self.phase = Phase::Hover;
                self.complete(target, !dragged);
            }
            Phase::Hover if pressed.is_some() => {
                let target = self.hover_target();
                self.complete(target, true);
            }
            Phase::Adjusting { grip, start, from } => {
                self.phase = Phase::Selected { rect: self.adjusted(grip, start, from) };
            }
            Phase::Selected { .. } => match self.toolbar_button_at_cursor() {
                Some(ToolbarButton::Copy) => self.commit_current(Delivery::Copy),
                Some(ToolbarButton::Edit) => self.commit_current(Delivery::Edit),
                Some(ToolbarButton::Save) => self.commit_current(Delivery::Save),
                Some(ToolbarButton::Cancel) => self.finish(OverlayResult::Cancelled),
                None => {}
            },
            _ => {}
        }
    }

    fn on_right_up(&mut self) {
        // Right-click steps back; from the idle state it cancels.
        match self.phase {
            Phase::Hover => self.finish(OverlayResult::Cancelled),
            _ => {
                let _ = unsafe { ReleaseCapture() };
                self.pressed_at = None;
                self.phase = Phase::Hover;
            }
        }
    }

    fn on_key(&mut self, vk: u16) {
        let ctrl = key_down(VK_CONTROL.0);
        let step = if key_down(VK_SHIFT.0) { 10 } else { 1 };
        match vk {
            v if v == VK_ESCAPE.0 => self.finish(OverlayResult::Cancelled),
            v if v == VK_RETURN.0 => self.commit_current(Delivery::Copy),
            0x43 if ctrl => self.commit_current(Delivery::Copy), // Ctrl+C
            0x53 if ctrl => self.commit_current(Delivery::Save), // Ctrl+S
            0x45 if self.options.purpose == Purpose::Screenshot && self.options.editor_installed => {
                // E / Ctrl+E: open selection in Image Editor.
                self.commit_current(Delivery::Edit);
            }
            0x43 if self.options.purpose == Purpose::Screenshot => {
                // C: copy the colour under the cursor.
                let (x, y) = self.cursor;
                if let Some([r, g, b]) = self.frame.rgb(x, y) {
                    self.finish(OverlayResult::CopyColor(format!("#{r:02X}{g:02X}{b:02X}")));
                }
            }
            0x46 => {
                // F: the whole monitor under the cursor.
                let target = self.monitor_at_cursor();
                self.phase = Phase::Hover;
                self.complete(target, false);
            }
            0x5A => self.magnifier = !self.magnifier, // Z
            v if [VK_LEFT.0, VK_RIGHT.0, VK_UP.0, VK_DOWN.0].contains(&v) => {
                let (dx, dy) = match v {
                    v if v == VK_LEFT.0 => (-step, 0),
                    v if v == VK_RIGHT.0 => (step, 0),
                    v if v == VK_UP.0 => (0, -step),
                    _ => (0, step),
                };
                if let Phase::Selected { rect } = self.phase {
                    let b = self.frame.bounds;
                    let dx = dx.clamp(b.left - rect.left, b.right - rect.right);
                    let dy = dy.clamp(b.top - rect.top, b.bottom - rect.bottom);
                    self.phase = Phase::Selected { rect: rect.offset(dx, dy) };
                } else {
                    // Nudge the pointer for pixel-precise starts.
                    let (x, y) = (self.cursor.0 + dx, self.cursor.1 + dy);
                    // SAFETY: plain cursor move.
                    let _ = unsafe { SetCursorPos(x, y) };
                    self.cursor = cursor_pos();
                }
            }
            _ => return,
        }
        self.invalidate();
    }

    fn cursor_shape(&self) -> PCWSTR {
        if self.toolbar_button_at_cursor().is_some() {
            return IDC_HAND;
        }
        let grip = match self.phase {
            Phase::Selected { rect } => self.grip_at(rect),
            Phase::Adjusting { grip, .. } => Some(grip),
            _ => None,
        };
        match grip {
            Some(Grip::NW | Grip::SE) => IDC_SIZENWSE,
            Some(Grip::NE | Grip::SW) => IDC_SIZENESW,
            Some(Grip::N | Grip::S) => IDC_SIZENS,
            Some(Grip::E | Grip::W) => IDC_SIZEWE,
            Some(Grip::Move) => IDC_SIZEALL,
            None => IDC_CROSS,
        }
    }

    // ----- drawing ------------------------------------------------------------

    fn ensure_gfx(&mut self, index: usize) -> Result<()> {
        if self.surfaces[index].gfx.is_some() {
            return Ok(());
        }
        let s = &self.surfaces[index];
        let b = s.monitor.bounds;
        let f = &self.factories;
        let target = f.hwnd_target_immediate(s.hwnd, (b.width() as u32, b.height() as u32), 96.0)?;
        let frame = &self.frame;
        let offset = frame.offset(b.left, b.top);
        let frozen = d2d::opaque_bitmap(
            &target,
            &frame.bgra[offset..],
            b.width() as u32,
            b.height() as u32,
            frame.stride() as u32,
        )?;
        let k = s.scale;
        let hint = f.text_format(13.0 * k, d2d::DWRITE_FONT_WEIGHT_NORMAL, d2d::DWRITE_PARAGRAPH_ALIGNMENT_CENTER, d2d::DWRITE_TEXT_ALIGNMENT_CENTER)?;
        let hint_width = f.text_width(hint_text(self.options.mode, self.options.purpose, self.options.editor_installed), &hint);
        let gfx = SurfaceGfx {
            hint_width,
            hint,
            label: f.text_format(12.0 * k, d2d::DWRITE_FONT_WEIGHT_SEMI_BOLD, d2d::DWRITE_PARAGRAPH_ALIGNMENT_CENTER, d2d::DWRITE_TEXT_ALIGNMENT_CENTER)?,
            button: f.text_format(13.5 * k, d2d::DWRITE_FONT_WEIGHT_SEMI_BOLD, d2d::DWRITE_PARAGRAPH_ALIGNMENT_CENTER, d2d::DWRITE_TEXT_ALIGNMENT_CENTER)?,
            small: f.text_format(11.5 * k, d2d::DWRITE_FONT_WEIGHT_NORMAL, d2d::DWRITE_PARAGRAPH_ALIGNMENT_CENTER, d2d::DWRITE_TEXT_ALIGNMENT_LEADING)?,
            target,
            frozen,
        };
        self.surfaces[index].gfx = Some(gfx);
        Ok(())
    }

    fn paint(&mut self, index: usize) -> Result<()> {
        self.ensure_gfx(index)?;
        let s = &self.surfaces[index];
        let g = s.gfx.as_ref().expect("graphics just created");
        let t: &ID2D1RenderTarget = &g.target;
        let b = s.monitor.bounds;
        let k = s.scale;
        let (w, h) = (b.width() as f32, b.height() as f32);
        // Virtual → surface-local coordinates.
        let local = |r: PixelRect| {
            d2d::rect(
                (r.left - b.left) as f32,
                (r.top - b.top) as f32,
                (r.right - b.left) as f32,
                (r.bottom - b.top) as f32,
            )
        };

        let [hr, hg, hb] = self.options.highlight;
        let accent = d2d::rgb(hr, hg, hb);
        let white = d2d::rgb(255, 255, 255);
        let pill = d2d::rgba(20, 22, 27, 0.92);

        // SAFETY: drawing between BeginDraw/EndDraw with this target's resources.
        unsafe {
            let dim = d2d::brush(t, d2d::rgba(0, 0, 0, DIM))?;
            let accent_brush = d2d::brush(t, accent)?;
            let white_brush = d2d::brush(t, white)?;
            let pill_brush = d2d::brush(t, pill)?;
            let muted_brush = d2d::brush(t, d2d::rgb(170, 176, 188))?;

            t.BeginDraw();
            t.DrawBitmap(&g.frozen, Some(&d2d::rect(0.0, 0.0, w, h)), 1.0, D2D1_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR, None);

            let highlight = self.highlight();
            match highlight.and_then(|r| r.intersect(&b)) {
                Some(hl) => {
                    let r = local(hl);
                    for part in [
                        d2d::rect(0.0, 0.0, w, r.top),
                        d2d::rect(0.0, r.bottom, w, h),
                        d2d::rect(0.0, r.top, r.left, r.bottom),
                        d2d::rect(r.right, r.top, w, r.bottom),
                    ] {
                        if part.right > part.left && part.bottom > part.top {
                            t.FillRectangle(&part, &dim);
                        }
                    }
                }
                None => t.FillRectangle(&d2d::rect(0.0, 0.0, w, h), &dim),
            }

            if let Some(hl) = highlight {
                let r = local(hl);
                let stroke = (f32::from(self.options.border_width) * k).round().max(1.0);
                let inset = stroke / 2.0;
                t.DrawRectangle(&d2d::rect(r.left + inset, r.top + inset, r.right - inset, r.bottom - inset), &accent_brush, stroke, None);

                // Size label above the top-left corner (inside if no room).
                if let Some((li, part)) = self.dominant_part(hl)
                    && li == index
                {
                    let r = local(part);
                    let size = format!("{} × {}", hl.width(), hl.height());
                    let title = match self.phase {
                        Phase::Hover => self.hover_window().map(|w| w.title.trim()).filter(|t| !t.is_empty()),
                        _ => None,
                    };
                    let text = match title {
                        Some(title) => {
                            let short: String = title.chars().take(48).collect();
                            let ellipsis = if title.chars().count() > 48 { "…" } else { "" };
                            format!("{short}{ellipsis}   {size}")
                        }
                        None => size,
                    };
                    let tw = self.factories.text_width(&text, &g.label) + 16.0 * k;
                    let th = 22.0 * k;
                    let x = r.left.max(0.0).min(w - tw);
                    let y = if r.top - th - 6.0 * k >= 0.0 { r.top - th - 6.0 * k } else { r.top.max(0.0) + 6.0 * k };
                    let lr = d2d::rect(x, y, x + tw, y + th);
                    t.FillRoundedRectangle(&rounded(lr, 6.0 * k), &pill_brush);
                    d2d::draw_text(t, &text, &g.label, lr, &white_brush);
                }

                if matches!(self.phase, Phase::Selected { .. } | Phase::Adjusting { .. }) {
                    let (cx, cy) = ((r.left + r.right) / 2.0, (r.top + r.bottom) / 2.0);
                    let radius = 4.5 * k;
                    for (x, y) in [
                        (r.left, r.top), (cx, r.top), (r.right, r.top),
                        (r.left, cy), (r.right, cy),
                        (r.left, r.bottom), (cx, r.bottom), (r.right, r.bottom),
                    ] {
                        let e = windows::Win32::Graphics::Direct2D::D2D1_ELLIPSE {
                            point: windows_numerics_point(x, y),
                            radiusX: radius,
                            radiusY: radius,
                        };
                        t.FillEllipse(&e, &white_brush);
                        t.DrawEllipse(&e, &accent_brush, 1.5 * k, None);
                    }
                }
            }

            if let Phase::Selected { rect } = self.phase
                && let Some((si, container, buttons)) = self.toolbar(rect)
                && si == index
            {
                t.FillRoundedRectangle(&rounded(local(container), 10.0 * k), &pill_brush);
                let hover = self.toolbar_hover;
                let hover_brush = d2d::brush(t, d2d::rgba(255, 255, 255, 0.12))?;
                for (kind, r) in buttons {
                    let lr = local(r);
                    if kind == ToolbarButton::Copy {
                        t.FillRoundedRectangle(&rounded(lr, 7.0 * k), &accent_brush);
                    } else if hover == Some(kind) {
                        t.FillRoundedRectangle(&rounded(lr, 7.0 * k), &hover_brush);
                    }
                    let label = match kind {
                        ToolbarButton::Copy => "Copy",
                        ToolbarButton::Edit => "Edit",
                        ToolbarButton::Save => "Save",
                        ToolbarButton::Cancel => "✕",
                    };
                    d2d::draw_text(t, label, &g.button, lr, &white_brush);
                }
            }

            if self.phase == Phase::Hover && self.surface_at(self.cursor.0, self.cursor.1) == Some(index) {
                let hint = hint_text(self.options.mode, self.options.purpose, self.options.editor_installed);
                let tw = g.hint_width + 36.0 * k;
                let th = 38.0 * k;
                let x = (w - tw) / 2.0;
                let y = 24.0 * k;
                let r = d2d::rect(x, y, x + tw, y + th);
                t.FillRoundedRectangle(&rounded(r, th / 2.0), &pill_brush);
                d2d::draw_text(t, hint, &g.hint, r, &white_brush);
            }

            let (cx, cy) = self.cursor;
            if self.magnifier && !matches!(self.phase, Phase::Selected { .. }) && b.contains(cx, cy) {
                self.draw_magnifier(t, g, s, cx, cy, &white_brush, &pill_brush, &muted_brush)?;
            }

            t.EndDraw(None, None)?;
        }
        self.painted = true;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    unsafe fn draw_magnifier(
        &self,
        t: &ID2D1RenderTarget,
        g: &SurfaceGfx,
        s: &Surface,
        cx: i32,
        cy: i32,
        white: &windows::Win32::Graphics::Direct2D::ID2D1SolidColorBrush,
        pill: &windows::Win32::Graphics::Direct2D::ID2D1SolidColorBrush,
        muted: &windows::Win32::Graphics::Direct2D::ID2D1SolidColorBrush,
    ) -> Result<()> {
        let b = s.monitor.bounds;
        let k = s.scale;
        let cell = (MAG_CELL * k).round();
        let size = cell * MAG_CELLS as f32;
        let info_h = (30.0 * k).round();
        let (w, h) = (b.width() as f32, b.height() as f32);
        let (lx, ly) = ((cx - b.left) as f32, (cy - b.top) as f32);
        let gap = 22.0 * k;
        let mut x = lx + gap;
        let mut y = ly + gap;
        if x + size > w {
            x = lx - gap - size;
        }
        if y + size + info_h > h {
            y = ly - gap - size - info_h;
        }
        let (x, y) = (x.round(), y.round());

        // SAFETY: caller is between BeginDraw/EndDraw.
        unsafe {
            let black = d2d::brush(t, d2d::rgb(0, 0, 0))?;
            let grid = d2d::brush(t, d2d::rgba(0, 0, 0, 0.22))?;
            let zoom = d2d::rect(x, y, x + size, y + size);
            t.FillRectangle(&zoom, &black);

            // Visible part of the source square on this monitor's bitmap.
            let half = MAG_CELLS / 2;
            let src = PixelRect { left: cx - half, top: cy - half, right: cx + half + 1, bottom: cy + half + 1 };
            if let Some(vis) = src.intersect(&b) {
                let dest = d2d::rect(
                    x + (vis.left - src.left) as f32 * cell,
                    y + (vis.top - src.top) as f32 * cell,
                    x + (vis.right - src.left) as f32 * cell,
                    y + (vis.bottom - src.top) as f32 * cell,
                );
                let source = d2d::rect(
                    (vis.left - b.left) as f32,
                    (vis.top - b.top) as f32,
                    (vis.right - b.left) as f32,
                    (vis.bottom - b.top) as f32,
                );
                t.DrawBitmap(&g.frozen, Some(&dest), 1.0, D2D1_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR, Some(&source));
            }
            for i in 1..MAG_CELLS {
                let o = i as f32 * cell;
                t.FillRectangle(&d2d::rect(x + o, y, x + o + 1.0, y + size), &grid);
                t.FillRectangle(&d2d::rect(x, y + o, x + size, y + o + 1.0), &grid);
            }
            let c0 = half as f32 * cell;
            let center = d2d::rect(x + c0, y + c0, x + c0 + cell, y + c0 + cell);
            t.DrawRectangle(&center, &black, 3.0, None);
            t.DrawRectangle(&center, white, 1.0, None);
            t.DrawRectangle(&d2d::rect(x - 0.5, y - 0.5, x + size + 0.5, y + size + 0.5), white, 1.0, None);

            // Info strip: swatch, hex, coordinates.
            let info = d2d::rect(x, y + size, x + size, y + size + info_h);
            t.FillRectangle(&info, pill);
            if let Some([r, gg, bb]) = self.frame.rgb(cx, cy) {
                let sw = 12.0 * k;
                let swatch = d2d::rect(x + 8.0 * k, info.top + (info_h - sw) / 2.0, x + 8.0 * k + sw, info.top + (info_h + sw) / 2.0);
                let color = d2d::brush(t, d2d::rgb(r, gg, bb))?;
                t.FillRectangle(&swatch, &color);
                t.DrawRectangle(&swatch, white, 1.0, None);
                let hex = format!("#{r:02X}{gg:02X}{bb:02X}");
                d2d::draw_text(t, &hex, &g.small, d2d::rect(swatch.right + 6.0 * k, info.top, info.right, info.bottom), white);
            }
            let coords = format!("{cx}, {cy}");
            let cw = self.factories.text_width(&coords, &g.small);
            d2d::draw_text(t, &coords, &g.small, d2d::rect(info.right - cw - 8.0 * k, info.top, info.right, info.bottom), muted);
        }
        Ok(())
    }
}

fn hint_text(mode: CaptureMode, purpose: Purpose, editor_installed: bool) -> &'static str {
    if purpose == Purpose::Record {
        return "Drag an area or click a window to record   ·   F  Full screen   ·   Esc  Cancel";
    }
    match (mode, editor_installed) {
        (CaptureMode::Region, true) => "Drag to select   ·   Click a window   ·   E  Edit   ·   F  Full screen   ·   Z  Magnifier   ·   C  Copy colour   ·   Esc  Cancel",
        (CaptureMode::Region, false) => "Drag to select   ·   Click a window   ·   F  Full screen   ·   Z  Magnifier   ·   C  Copy colour   ·   Esc  Cancel",
        (CaptureMode::Window, true) => "Click a window to capture   ·   E  Edit   ·   Z  Magnifier   ·   Esc  Cancel",
        (CaptureMode::Window, false) => "Click a window to capture   ·   Z  Magnifier   ·   Esc  Cancel",
        (CaptureMode::Monitor, true) => "Click a display to capture   ·   E  Edit   ·   Esc  Cancel",
        (CaptureMode::Monitor, false) => "Click a display to capture   ·   Esc  Cancel",
    }
}

fn rounded(rect: D2D_RECT_F, radius: f32) -> D2D1_ROUNDED_RECT {
    D2D1_ROUNDED_RECT { rect, radiusX: radius, radiusY: radius }
}

fn windows_numerics_point(x: f32, y: f32) -> windows_numerics::Vector2 {
    windows_numerics::Vector2 { X: x, Y: y }
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
                    lpfnWndProc: Some(wndproc),
                    hInstance: GetModuleHandleW(None)?.into(),
                    hCursor: LoadCursorW(None, IDC_CROSS)?,
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
    // SAFETY: every surface's GWLP_USERDATA points at the boxed Overlay from
    // WM_NCCREATE until `destroy_surfaces` clears it; all on this thread.
    unsafe {
        if msg == WM_NCCREATE {
            let create = &*(lparam.0 as *const CREATESTRUCTW);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }
        let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Overlay;
        if ptr.is_null() {
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }
        let o = &mut *ptr;
        let index = o.surfaces.iter().position(|s| s.hwnd == hwnd);

        match msg {
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                BeginPaint(hwnd, &mut ps);
                if let Some(i) = index
                    && let Err(err) = o.paint(i)
                {
                    tracing::warn!("overlay paint failed: {err}");
                    o.surfaces[i].gfx = None;
                }
                let _ = EndPaint(hwnd, &ps);
                LRESULT(0)
            }
            WM_ERASEBKGND => LRESULT(1),
            WM_MOUSEMOVE => {
                let origin = index.map(|i| (o.surfaces[i].monitor.bounds.left, o.surfaces[i].monitor.bounds.top));
                let cursor = mouse_pos(origin, lparam);
                if cursor != o.cursor {
                    let previous = std::mem::replace(&mut o.cursor, cursor);
                    o.invalidate_after_move(previous);
                }
                LRESULT(0)
            }
            WM_LBUTTONDOWN => {
                let origin = index.map(|i| (o.surfaces[i].monitor.bounds.left, o.surfaces[i].monitor.bounds.top));
                o.cursor = mouse_pos(origin, lparam);
                o.on_mouse_down(hwnd);
                o.invalidate();
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                let origin = index.map(|i| (o.surfaces[i].monitor.bounds.left, o.surfaces[i].monitor.bounds.top));
                o.cursor = mouse_pos(origin, lparam);
                o.on_mouse_up();
                o.invalidate();
                LRESULT(0)
            }
            WM_RBUTTONUP => {
                o.on_right_up();
                o.invalidate();
                LRESULT(0)
            }
            WM_KEYDOWN => {
                o.on_key(wparam.0 as u16);
                LRESULT(0)
            }
            WM_SETCURSOR => {
                if let Ok(cursor) = LoadCursorW(None, o.cursor_shape()) {
                    SetCursor(Some(cursor));
                }
                LRESULT(1)
            }
            WM_ACTIVATEAPP => {
                if wparam.0 != 0 {
                    o.activated = true;
                } else if o.activated {
                    // Alt-Tab or similar: never leave an orphaned overlay.
                    o.finish(OverlayResult::Cancelled);
                }
                LRESULT(0)
            }
            WM_TIMER if wparam.0 == WATCHDOG_TIMER => {
                let _ = KillTimer(Some(hwnd), WATCHDOG_TIMER);
                if !o.painted {
                    o.finish(OverlayResult::Failed("The capture overlay didn't render in time.".into()));
                }
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}
