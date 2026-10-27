//! A modern owner-drawn popup menu, used for the tray icon.
//!
//! Drawn with Direct2D/DirectWrite in a borderless popup that gets Windows 11
//! rounded corners and shadow from DWM. Runs on the calling thread's message
//! loop, so it works whether or not the main window exists.

use std::cell::Cell;
use std::ffi::c_void;
use std::sync::Once;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D_SIZE_U, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F,
    D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, D2D1_BITMAP_PROPERTIES, D2D1_DRAW_TEXT_OPTIONS_NONE,
    D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_FEATURE_LEVEL_DEFAULT,
    D2D1_HWND_RENDER_TARGET_PROPERTIES, D2D1_PRESENT_OPTIONS_NONE, D2D1_RENDER_TARGET_PROPERTIES,
    D2D1_RENDER_TARGET_TYPE_DEFAULT, D2D1_RENDER_TARGET_USAGE_NONE, D2D1_ROUNDED_RECT,
    D2D1CreateFactory, ID2D1Bitmap, ID2D1Factory, ID2D1HwndRenderTarget,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
    DWRITE_FONT_WEIGHT, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD,
    DWRITE_MEASURING_MODE_NATURAL, DWRITE_PARAGRAPH_ALIGNMENT, DWRITE_PARAGRAPH_ALIGNMENT_CENTER,
    DWRITE_PARAGRAPH_ALIGNMENT_FAR, DWRITE_PARAGRAPH_ALIGNMENT_NEAR, DWRITE_WORD_WRAPPING_NO_WRAP,
    DWriteCreateFactory, IDWriteFactory, IDWriteTextFormat,
};
use windows::Win32::Graphics::Dwm::{
    DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
    DwmSetWindowAttribute,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{
    BeginPaint, EndPaint, GetMonitorInfoW, InvalidateRect, MONITOR_DEFAULTTONEAREST, MONITORINFO,
    MonitorFromPoint, PAINTSTRUCT,
};
use windows::Win32::System::LibraryLoader::{
    GetModuleHandleW, GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, VIRTUAL_KEY, VK_DOWN, VK_ESCAPE, VK_RETURN,
    VK_SPACE, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CREATESTRUCTW, CS_DROPSHADOW, CreatePopupMenu, CreateWindowExW, DefWindowProcW,
    DestroyMenu, DestroyWindow, GWLP_USERDATA, GetCursorPos, GetMessagePos, GetWindowLongPtrW,
    IDC_ARROW, LoadCursorW, MF_SEPARATOR, MF_STRING, PostMessageW, RegisterClassExW, SW_SHOW,
    SetForegroundWindow, SetMenuDefaultItem, SetWindowLongPtrW, ShowWindow, TPM_BOTTOMALIGN,
    TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenuEx, WA_INACTIVE, WM_ACTIVATE,
    WM_ERASEBKGND, WM_KEYDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_NCCREATE, WM_NCDESTROY, WM_NULL,
    WM_PAINT, WNDCLASSEXW, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_OVERLAPPED, WS_POPUP,
};
use windows::core::{BOOL, HSTRING, PCSTR, PCWSTR, Result, w};

/// One row of the menu.
pub enum MenuEntry {
    Item { id: String, label: String },
    Separator,
}

impl MenuEntry {
    pub fn item(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self::Item {
            id: id.into(),
            label: label.into(),
        }
    }
}

/// Straight-alpha RGBA8 image.
pub struct RgbaImage {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// Optional app identity row at the top of the menu.
pub struct MenuHeader {
    pub title: String,
    pub subtitle: String,
    pub icon: Option<RgbaImage>,
}

#[derive(Clone, Copy, Debug)]
pub struct MenuTheme {
    pub dark: bool,
}

// Layout, in device-independent pixels.
const WIDTH: f32 = 248.0;
const PAD: f32 = 6.0;
const HEADER_H: f32 = 54.0;
const ITEM_H: f32 = 34.0;
const SEPARATOR_H: f32 = 9.0;
const ITEM_RADIUS: f32 = 6.0;
const TEXT_INSET: f32 = 14.0;
const ICON_SIZE: f32 = 28.0;
/// Gap between the menu and the taskbar / screen edge.
const SCREEN_GAP: f32 = 8.0;

/// Not exported by the `windows` crate's WindowsAndMessaging module.
const WM_MOUSELEAVE: u32 = 0x02A3;

const CLASS_NAME: PCWSTR = w!("CapShotX.PopupMenu");

thread_local! {
    static OPEN_MENU: Cell<HWND> = Cell::new(HWND::default());
}

/// Shows the menu near the mouse cursor. `on_select` receives the chosen
/// item's id; it is not called if the menu is dismissed.
///
/// Must be called on a thread that runs a Win32 message loop.
pub fn show_popup_menu(
    header: Option<MenuHeader>,
    entries: Vec<MenuEntry>,
    theme: MenuTheme,
    on_select: impl FnOnce(&str) + 'static,
) -> Result<()> {
    close_popup_menu();
    register_class()?;

    // SAFETY: plain Win32 queries with valid out-pointers.
    let (cursor, work, dpi) = unsafe {
        let cursor = cursor_pos();
        let monitor = MonitorFromPoint(cursor, MONITOR_DEFAULTTONEAREST);
        let (mut dpi_x, mut dpi_y) = (96, 96);
        GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y)?;
        let mut info = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        GetMonitorInfoW(monitor, &mut info).ok()?;
        (cursor, info.rcWork, dpi_x as f32)
    };

    let scale = dpi / 96.0;
    let (rows, height) = layout(header.is_some(), &entries);
    let size = ((WIDTH * scale).round() as i32, (height * scale).round() as i32);
    let (x, y) = place(cursor, work, size, (SCREEN_GAP * scale).round() as i32);

    let state = Box::new(MenuState {
        header,
        entries,
        theme,
        on_select: Some(Box::new(on_select)),
        rows,
        hover: None,
        tracking_leave: false,
        closing: false,
        dpi,
        size_px: (size.0 as u32, size.1 as u32),
        gfx: None,
    });

    // SAFETY: the state pointer is handed to the window in WM_NCCREATE and
    // freed in WM_NCDESTROY.
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            CLASS_NAME,
            w!("CapShotX menu"),
            WS_POPUP,
            x,
            y,
            size.0,
            size.1,
            None,
            None,
            Some(instance.into()),
            Some(Box::into_raw(state) as *const c_void),
        )?;

        let corner = DWMWCP_ROUND;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &corner as *const _ as *const c_void,
            size_of_val(&corner) as u32,
        );
        let dark = BOOL::from(theme.dark);
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            &dark as *const _ as *const c_void,
            size_of_val(&dark) as u32,
        );

        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        OPEN_MENU.set(hwnd);
    }
    Ok(())
}

/// Closes the menu opened on this thread, if any.
pub fn close_popup_menu() {
    let hwnd = OPEN_MENU.get();
    if !hwnd.is_invalid() {
        // SAFETY: OPEN_MENU only holds windows created by this module and is
        // cleared in WM_NCDESTROY.
        unsafe {
            let _ = DestroyWindow(hwnd);
        }
    }
}

/// Whether a menu is currently open on this thread.
pub fn is_popup_menu_open() -> bool {
    !OPEN_MENU.get().is_invalid()
}

fn register_class() -> Result<()> {
    static REGISTER: Once = Once::new();
    let mut result = Ok(());
    REGISTER.call_once(|| {
        // SAFETY: the class struct is fully initialised and the wndproc has the
        // correct signature.
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

enum RowKind {
    Header,
    Item(usize),
    Separator,
}

struct Row {
    rect: D2D_RECT_F,
    kind: RowKind,
}

fn layout(has_header: bool, entries: &[MenuEntry]) -> (Vec<Row>, f32) {
    let mut rows = Vec::new();
    let mut y = PAD;
    let mut push = |height: f32, kind: RowKind| {
        rows.push(Row {
            rect: D2D_RECT_F {
                left: PAD,
                top: y,
                right: WIDTH - PAD,
                bottom: y + height,
            },
            kind,
        });
        y += height;
    };
    if has_header {
        push(HEADER_H, RowKind::Header);
        push(SEPARATOR_H, RowKind::Separator);
    }
    for (i, entry) in entries.iter().enumerate() {
        match entry {
            MenuEntry::Item { .. } => push(ITEM_H, RowKind::Item(i)),
            MenuEntry::Separator => push(SEPARATOR_H, RowKind::Separator),
        }
    }
    (rows, y + PAD)
}

/// Centres the menu horizontally on the cursor and puts it above the taskbar,
/// falling back to below the cursor when the taskbar is at the top.
fn place(cursor: POINT, work: RECT, (w, h): (i32, i32), gap: i32) -> (i32, i32) {
    let x = (cursor.x - w / 2)
        .min(work.right - w - gap)
        .max(work.left + gap);
    let above = cursor.y.min(work.bottom) - h - gap;
    let y = if above >= work.top + gap {
        above
    } else {
        cursor.y.max(work.top) + gap
    };
    (x, y.min(work.bottom - h - gap).max(work.top))
}

struct Palette {
    background: D2D1_COLOR_F,
    text: D2D1_COLOR_F,
    muted: D2D1_COLOR_F,
    hover: D2D1_COLOR_F,
    separator: D2D1_COLOR_F,
}

fn rgb(r: u8, g: u8, b: u8) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: r as f32 / 255.0,
        g: g as f32 / 255.0,
        b: b as f32 / 255.0,
        a: 1.0,
    }
}

impl Palette {
    fn of(theme: MenuTheme) -> Self {
        if theme.dark {
            Self {
                background: rgb(32, 35, 41),
                text: rgb(230, 232, 235),
                muted: rgb(138, 147, 160),
                hover: rgb(45, 50, 59),
                separator: rgb(48, 53, 62),
            }
        } else {
            Self {
                background: rgb(255, 255, 255),
                text: rgb(26, 29, 33),
                muted: rgb(107, 114, 128),
                hover: rgb(240, 241, 244),
                separator: rgb(230, 232, 236),
            }
        }
    }
}

struct Graphics {
    target: ID2D1HwndRenderTarget,
    item_text: IDWriteTextFormat,
    title_text: IDWriteTextFormat,
    subtitle_text: IDWriteTextFormat,
    icon: Option<ID2D1Bitmap>,
}

impl Graphics {
    fn new(hwnd: HWND, state: &MenuState) -> Result<Self> {
        // SAFETY: COM factories and resources are created with valid
        // descriptors and owned by the returned smart pointers.
        unsafe {
            let factory: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let pixel_format = D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            };
            let target = factory.CreateHwndRenderTarget(
                &D2D1_RENDER_TARGET_PROPERTIES {
                    r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
                    pixelFormat: pixel_format,
                    dpiX: state.dpi,
                    dpiY: state.dpi,
                    usage: D2D1_RENDER_TARGET_USAGE_NONE,
                    minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
                },
                &D2D1_HWND_RENDER_TARGET_PROPERTIES {
                    hwnd,
                    pixelSize: D2D_SIZE_U {
                        width: state.size_px.0,
                        height: state.size_px.1,
                    },
                    presentOptions: D2D1_PRESENT_OPTIONS_NONE,
                },
            )?;

            let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let format = |size: f32, weight: DWRITE_FONT_WEIGHT, align: DWRITE_PARAGRAPH_ALIGNMENT| {
                let format = dwrite.CreateTextFormat(
                    w!("Segoe UI"),
                    None,
                    weight,
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    size,
                    w!("en-us"),
                )?;
                format.SetParagraphAlignment(align)?;
                format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
                Result::Ok(format)
            };

            let icon = match state.header.as_ref().and_then(|h| h.icon.as_ref()) {
                Some(image) => {
                    let bgra = premultiplied_bgra(&image.rgba);
                    Some(target.CreateBitmap(
                        D2D_SIZE_U {
                            width: image.width,
                            height: image.height,
                        },
                        Some(bgra.as_ptr() as *const c_void),
                        image.width * 4,
                        &D2D1_BITMAP_PROPERTIES {
                            pixelFormat: pixel_format,
                            dpiX: 96.0,
                            dpiY: 96.0,
                        },
                    )?)
                }
                None => None,
            };

            Ok(Self {
                item_text: format(14.0, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?,
                title_text: format(14.5, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_PARAGRAPH_ALIGNMENT_FAR)?,
                subtitle_text: format(12.0, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_PARAGRAPH_ALIGNMENT_NEAR)?,
                target,
                icon,
            })
        }
    }
}

fn premultiplied_bgra(rgba: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(rgba.len());
    for px in rgba.chunks_exact(4) {
        let a = px[3] as u32;
        let mul = |c: u8| ((c as u32 * a + 127) / 255) as u8;
        out.extend_from_slice(&[mul(px[2]), mul(px[1]), mul(px[0]), px[3]]);
    }
    out
}

struct MenuState {
    header: Option<MenuHeader>,
    entries: Vec<MenuEntry>,
    theme: MenuTheme,
    on_select: Option<Box<dyn FnOnce(&str)>>,
    rows: Vec<Row>,
    hover: Option<usize>,
    tracking_leave: bool,
    closing: bool,
    dpi: f32,
    size_px: (u32, u32),
    gfx: Option<Graphics>,
}

impl MenuState {
    fn paint(&mut self, hwnd: HWND) -> Result<()> {
        if self.gfx.is_none() {
            self.gfx = Some(Graphics::new(hwnd, self)?);
        }
        let g = self.gfx.as_ref().expect("graphics just created");
        let t = &g.target;
        let p = Palette::of(self.theme);

        // SAFETY: all Direct2D calls happen between BeginDraw and EndDraw on
        // resources created from this target.
        unsafe {
            let text = t.CreateSolidColorBrush(&p.text, None)?;
            let muted = t.CreateSolidColorBrush(&p.muted, None)?;
            let hover = t.CreateSolidColorBrush(&p.hover, None)?;
            let separator = t.CreateSolidColorBrush(&p.separator, None)?;

            t.BeginDraw();
            t.Clear(Some(&p.background));
            for row in &self.rows {
                let r = row.rect;
                match row.kind {
                    RowKind::Header => {
                        let Some(header) = &self.header else { continue };
                        let mid = (r.top + r.bottom) / 2.0;
                        let icon_left = r.left + 8.0;
                        if let Some(icon) = &g.icon {
                            let dest = D2D_RECT_F {
                                left: icon_left,
                                top: mid - ICON_SIZE / 2.0,
                                right: icon_left + ICON_SIZE,
                                bottom: mid + ICON_SIZE / 2.0,
                            };
                            t.DrawBitmap(icon, Some(&dest), 1.0, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, None);
                        }
                        let text_left = icon_left + ICON_SIZE + 12.0;
                        let title = D2D_RECT_F { left: text_left, top: r.top, right: r.right, bottom: mid + 1.0 };
                        let subtitle = D2D_RECT_F { left: text_left, top: mid + 2.0, right: r.right, bottom: r.bottom };
                        draw_text(t, &header.title, &g.title_text, title, &text);
                        draw_text(t, &header.subtitle, &g.subtitle_text, subtitle, &muted);
                    }
                    RowKind::Separator => {
                        let y = ((r.top + r.bottom) / 2.0).floor();
                        let line = D2D_RECT_F { left: r.left + 6.0, top: y, right: r.right - 6.0, bottom: y + 1.0 };
                        t.FillRectangle(&line, &separator);
                    }
                    RowKind::Item(i) => {
                        let MenuEntry::Item { label, .. } = &self.entries[i] else { continue };
                        if self.hover == Some(i) {
                            t.FillRoundedRectangle(
                                &D2D1_ROUNDED_RECT { rect: r, radiusX: ITEM_RADIUS, radiusY: ITEM_RADIUS },
                                &hover,
                            );
                        }
                        let text_rect = D2D_RECT_F { left: r.left + TEXT_INSET, ..r };
                        draw_text(t, label, &g.item_text, text_rect, &text);
                    }
                }
            }
            t.EndDraw(None, None)
        }
    }

    fn hit(&self, lparam: LPARAM) -> Option<usize> {
        let scale = self.dpi / 96.0;
        let x = (lparam.0 & 0xFFFF) as i16 as f32 / scale;
        let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as f32 / scale;
        self.rows.iter().find_map(|row| match row.kind {
            RowKind::Item(i)
                if x >= row.rect.left
                    && x < row.rect.right
                    && y >= row.rect.top
                    && y < row.rect.bottom =>
            {
                Some(i)
            }
            _ => None,
        })
    }

    fn set_hover(&mut self, hwnd: HWND, hover: Option<usize>) {
        if self.hover != hover {
            self.hover = hover;
            // SAFETY: hwnd is this menu's live window.
            unsafe {
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
        }
    }

    /// Moves keyboard highlight by `delta` items, wrapping around.
    fn step(&mut self, hwnd: HWND, delta: isize) {
        let items: Vec<usize> = self
            .entries
            .iter()
            .enumerate()
            .filter_map(|(i, e)| matches!(e, MenuEntry::Item { .. }).then_some(i))
            .collect();
        if items.is_empty() {
            return;
        }
        let len = items.len() as isize;
        let next = match self.hover.and_then(|h| items.iter().position(|&i| i == h)) {
            Some(pos) => (pos as isize + delta).rem_euclid(len),
            None if delta > 0 => 0,
            None => len - 1,
        };
        self.set_hover(hwnd, Some(items[next as usize]));
    }
}

fn draw_text(
    target: &ID2D1HwndRenderTarget,
    text: &str,
    format: &IDWriteTextFormat,
    rect: D2D_RECT_F,
    brush: &windows::Win32::Graphics::Direct2D::ID2D1SolidColorBrush,
) {
    let wide: Vec<u16> = text.encode_utf16().collect();
    // SAFETY: called between BeginDraw/EndDraw with live resources.
    unsafe {
        target.DrawText(
            &wide,
            format,
            &rect,
            brush,
            D2D1_DRAW_TEXT_OPTIONS_NONE,
            DWRITE_MEASURING_MODE_NATURAL,
        );
    }
}

/// Destroys the menu. Must not touch `state` afterwards: it is freed during
/// `DestroyWindow`.
unsafe fn close(hwnd: HWND, state: *mut MenuState) {
    // SAFETY: caller guarantees state is live.
    unsafe {
        if (*state).closing {
            return;
        }
        (*state).closing = true;
        let _ = DestroyWindow(hwnd);
    }
}

unsafe fn select(hwnd: HWND, state: *mut MenuState, index: usize) {
    // SAFETY: caller guarantees state is live; it is not used after close().
    unsafe {
        let MenuEntry::Item { id, .. } = &(&(*state).entries)[index] else { return };
        let id = id.clone();
        let on_select = (*state).on_select.take();
        close(hwnd, state);
        if let Some(on_select) = on_select {
            on_select(&id);
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // SAFETY: GWLP_USERDATA holds the Box<MenuState> pointer from WM_NCCREATE
    // until WM_NCDESTROY, and all messages arrive on the creating thread.
    unsafe {
        if msg == WM_NCCREATE {
            let create = &*(lparam.0 as *const CREATESTRUCTW);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }
        let state = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut MenuState;
        if state.is_null() {
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }

        match msg {
            WM_NCDESTROY => {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                drop(Box::from_raw(state));
                if OPEN_MENU.get() == hwnd {
                    OPEN_MENU.set(HWND::default());
                }
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                BeginPaint(hwnd, &mut ps);
                if let Err(err) = (*state).paint(hwnd) {
                    // Device loss etc.: rebuild resources on the next paint.
                    let _ = err;
                    (*state).gfx = None;
                }
                let _ = EndPaint(hwnd, &ps);
                LRESULT(0)
            }
            WM_ERASEBKGND => LRESULT(1),
            WM_MOUSEMOVE => {
                if !(*state).tracking_leave {
                    let mut track = TRACKMOUSEEVENT {
                        cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: hwnd,
                        dwHoverTime: 0,
                    };
                    let _ = TrackMouseEvent(&mut track);
                    (*state).tracking_leave = true;
                }
                let hit = (*state).hit(lparam);
                (*state).set_hover(hwnd, hit);
                LRESULT(0)
            }
            WM_MOUSELEAVE => {
                (*state).tracking_leave = false;
                (*state).set_hover(hwnd, None);
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                if let Some(index) = (*state).hit(lparam) {
                    select(hwnd, state, index);
                }
                LRESULT(0)
            }
            WM_KEYDOWN => {
                match VIRTUAL_KEY(wparam.0 as u16) {
                    VK_ESCAPE => close(hwnd, state),
                    VK_DOWN => (*state).step(hwnd, 1),
                    VK_UP => (*state).step(hwnd, -1),
                    VK_RETURN | VK_SPACE => {
                        if let Some(index) = (*state).hover {
                            select(hwnd, state, index);
                        }
                    }
                    _ => {}
                }
                LRESULT(0)
            }
            WM_ACTIVATE => {
                if (wparam.0 & 0xFFFF) as u32 == WA_INACTIVE {
                    close(hwnd, state);
                }
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

/// Shows a lightweight native Win32 popup menu (`CreatePopupMenu` +
/// `TrackPopupMenuEx`) near the mouse cursor without initialising Direct2D,
/// DirectWrite or any GPU surface. Used in Low memory mode.
pub fn show_native_popup_menu(
    entries: Vec<MenuEntry>,
    theme: MenuTheme,
    on_select: impl FnOnce(&str),
) -> Result<()> {
    close_popup_menu();
    // PreferredAppMode: 1 = AllowDark, 2 = ForceDark, 3 = ForceLight.
    set_uxtheme_app_mode(if theme.dark { 2 } else { 3 });

    // SAFETY: creates a temporary invisible Win32 owner window and HMENU on
    // the calling thread; both are unconditionally destroyed before returning.
    unsafe {
        let cursor = cursor_pos();

        let instance = GetModuleHandleW(None)?;
        let owner = CreateWindowExW(
            WS_EX_TOOLWINDOW,
            w!("STATIC"),
            w!(""),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(instance.into()),
            None,
        )?;

        let hmenu = match CreatePopupMenu() {
            Ok(m) => m,
            Err(err) => {
                let _ = DestroyWindow(owner);
                set_uxtheme_app_mode(1);
                return Err(err);
            }
        };

        let mut ids = Vec::new();
        for entry in entries {
            match entry {
                MenuEntry::Item { id, label } => {
                    ids.push(id);
                    let cmd_id = ids.len();
                    let wide = HSTRING::from(label);
                    let _ = AppendMenuW(hmenu, MF_STRING, cmd_id, &wide);
                    if cmd_id == 1 {
                        let _ = SetMenuDefaultItem(hmenu, 1, 0);
                    }
                }
                MenuEntry::Separator => {
                    let _ = AppendMenuW(hmenu, MF_SEPARATOR, 0, PCWSTR::null());
                }
            }
        }

        // Microsoft Shell_NotifyIcon requirement (KB135788): foreground the
        // owner before TrackPopupMenuEx and post WM_NULL afterwards so clicking
        // outside the menu dismisses it immediately.
        let _ = SetForegroundWindow(owner);
        let flags = (TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON | TPM_BOTTOMALIGN).0;
        let chosen = TrackPopupMenuEx(hmenu, flags, cursor.x, cursor.y, owner, None).0 as usize;
        let _ = PostMessageW(Some(owner), WM_NULL, WPARAM(0), LPARAM(0));

        let _ = DestroyMenu(hmenu);
        let _ = DestroyWindow(owner);
        set_uxtheme_app_mode(1);

        if let Some(id) = chosen.checked_sub(1).and_then(|i| ids.get(i)) {
            on_select(id);
        }
        Ok(())
    }
}

/// Queries the cursor position, falling back to `GetMessagePos` if UIPI blocks
/// `GetCursorPos` while an elevated window (e.g. Task Manager) has focus.
fn cursor_pos() -> POINT {
    let mut pt = POINT::default();
    // SAFETY: valid out-pointer; GetMessagePos has no preconditions.
    unsafe {
        if GetCursorPos(&mut pt).is_ok() {
            pt
        } else {
            let packed = GetMessagePos();
            POINT {
                x: (packed & 0xffff) as i16 as i32,
                y: ((packed >> 16) & 0xffff) as i16 as i32,
            }
        }
    }
}

/// Applies Windows 10 1903+ / Windows 11 native menu dark/light styling via
/// `uxtheme.dll` ordinals 135 (`SetPreferredAppMode`) and 136 (`FlushMenuThemes`).
fn set_uxtheme_app_mode(mode: i32) {
    // SAFETY: loads uxtheme from System32 only and calls stable ordinals if present.
    unsafe {
        let Ok(uxtheme) = LoadLibraryExW(w!("uxtheme.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32)
        else {
            return;
        };
        if let Some(set_mode) = GetProcAddress(uxtheme, PCSTR(135usize as *const u8)) {
            let f: unsafe extern "system" fn(i32) -> i32 = std::mem::transmute(set_mode);
            let _ = f(mode);
        }
        if let Some(flush) = GetProcAddress(uxtheme, PCSTR(136usize as *const u8)) {
            let f: unsafe extern "system" fn() = std::mem::transmute(flush);
            f();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uxtheme_mode_and_hmenu_build_cleanly() {
        set_uxtheme_app_mode(2);
        set_uxtheme_app_mode(3);
        set_uxtheme_app_mode(1);
        unsafe {
            let hmenu = CreatePopupMenu().unwrap();
            AppendMenuW(hmenu, MF_STRING, 1, &HSTRING::from("Open CapShotX")).unwrap();
            AppendMenuW(hmenu, MF_SEPARATOR, 0, PCWSTR::null()).unwrap();
            SetMenuDefaultItem(hmenu, 1, 0).unwrap();
            DestroyMenu(hmenu).unwrap();
        }
    }
}
