use std::sync::atomic::{AtomicBool, Ordering};

use platform_windows::popup_menu::{
    MenuEntry, MenuHeader, MenuTheme, RgbaImage, show_native_popup_menu, show_popup_menu,
};
use tray_icon::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

use crate::commands::{Action, AppCommand, Waker};

const MENU_OPEN: &str = "open";
const MENU_EDITOR: &str = "editor";
const MENU_CAPTURE_REGION: &str = "capture_region";
const MENU_CAPTURE_EDIT: &str = "capture_edit";
const MENU_CAPTURE_WINDOW: &str = "capture_window";
const MENU_CAPTURE_MONITOR: &str = "capture_monitor";
const MENU_SETTINGS: &str = "settings";
const MENU_QUIT: &str = "quit";
const MENU_RECORD: &str = "record";
const MENU_RECORD_FAST: &str = "record_fast";

/// A recording is starting, running or saving (menu shows "Stop").
static RECORDING: AtomicBool = AtomicBool::new(false);

/// Keeps the tray icon alive; dropping it removes the icon.
pub struct Tray {
    icon: TrayIcon,
}

impl Tray {
    /// Shows a red dot on the icon while recording (REC-04: never an
    /// invisible active capture).
    pub fn set_recording(&self, on: bool) {
        if RECORDING.swap(on, Ordering::Relaxed) == on {
            return;
        }
        let rgba = if on { recording_rgba() } else { crate::icon::rgba(32) };
        if let Ok(icon) = tray_icon::Icon::from_rgba(rgba, 32, 32) {
            let _ = self.icon.set_icon(Some(icon));
        }
        let _ = self.icon.set_tooltip(Some(if on { "CapShotX — recording" } else { "CapShotX" }));
    }
}

/// The app icon with a red recording dot in the bottom-right corner.
fn recording_rgba() -> Vec<u8> {
    let mut px = crate::icon::rgba(32);
    let (cx, cy, r) = (24.0f32, 24.0f32, 7.5f32);
    for y in 0..32 {
        for x in 0..32 {
            let d = ((x as f32 + 0.5 - cx).powi(2) + (y as f32 + 0.5 - cy).powi(2)).sqrt();
            let o = (y * 32 + x) * 4;
            if d <= r + 1.5 {
                // White ring, then red fill, antialiased at the edge.
                let (c, a) = if d <= r { ([239, 68, 68], (r + 0.5 - d).clamp(0.0, 1.0)) } else { ([255, 255, 255], (r + 1.5 - d).clamp(0.0, 1.0)) };
                for i in 0..3 {
                    px[o + i] = (px[o + i] as f32 * (1.0 - a) + c[i] as f32 * a) as u8;
                }
                px[o + 3] = px[o + 3].max((a * 255.0) as u8);
            }
        }
    }
    px
}

/// Creates the tray icon on the main thread. Works both in pure tray mode
/// (no `eframe` window or GPU device loaded) and while the UI window is open.
///
/// Right-click opens CapShotX's own popup menu; left-click is forwarded as
/// [`AppCommand::TrayLeftClick`] so the app can apply the user's preference.
pub fn install(waker: Waker) -> anyhow::Result<Tray> {
    let icon = tray_icon::Icon::from_rgba(crate::icon::rgba(32), 32, 32)?;
    let tray = TrayIconBuilder::new()
        .with_tooltip("CapShotX")
        .with_icon(icon)
        .build()?;

    TrayIconEvent::set_event_handler(Some(move |event| {
        let TrayIconEvent::Click {
            button,
            button_state: MouseButtonState::Up,
            ..
        } = event
        else {
            return;
        };
        match button {
            MouseButton::Left => waker.send(AppCommand::TrayLeftClick),
            MouseButton::Right => show_menu(&waker),
            MouseButton::Middle => {}
        }
    }));

    Ok(Tray { icon: tray })
}

fn show_menu(waker: &Waker) {
    let is_recording = RECORDING.load(Ordering::Relaxed);
    let editor_installed = crate::editor_addon::is_installed();
    let mut entries = vec![MenuEntry::item(MENU_OPEN, "Open CapShotX")];
    if editor_installed {
        entries.push(MenuEntry::item(MENU_EDITOR, "Open Image Editor"));
    }
    entries.push(MenuEntry::Separator);
    entries.push(MenuEntry::item(MENU_CAPTURE_REGION, "Capture region"));
    if editor_installed {
        entries.push(MenuEntry::item(MENU_CAPTURE_EDIT, "Capture & edit"));
    }
    entries.extend([
        MenuEntry::item(MENU_CAPTURE_WINDOW, "Capture window"),
        MenuEntry::item(MENU_CAPTURE_MONITOR, "Capture display"),
        MenuEntry::Separator,
    ]);
    if is_recording {
        entries.push(MenuEntry::item(MENU_RECORD, "Stop recording"));
    } else {
        entries.push(MenuEntry::item(MENU_RECORD, "Record screen"));
        entries.push(MenuEntry::item(MENU_RECORD_FAST, "Fast record"));
    }
    entries.extend([
        MenuEntry::Separator,
        MenuEntry::item(MENU_SETTINGS, "Settings"),
        MenuEntry::Separator,
        MenuEntry::item(MENU_QUIT, "Quit CapShotX"),
    ]);
    let theme = MenuTheme {
        dark: waker.is_dark(),
    };
    let use_native = waker.low_memory_mode();

    let waker = waker.clone();
    let on_select = move |id: &str| {
        if id == MENU_EDITOR {
            waker.send(AppCommand::OpenEditor);
            return;
        }
        if let Some(action) = match id {
            MENU_CAPTURE_REGION => Some(Action::CaptureRegion),
            MENU_CAPTURE_EDIT => Some(Action::CaptureEdit),
            MENU_CAPTURE_WINDOW => Some(Action::CaptureWindow),
            MENU_CAPTURE_MONITOR => Some(Action::CaptureMonitor),
            _ => None,
        } {
            let waker = waker.clone();
            std::thread::spawn(move || {
                // Let the tray popup menu finish its DWM fade-out before freezing the desktop.
                std::thread::sleep(std::time::Duration::from_millis(85));
                waker.send(AppCommand::Hotkey(action));
            });
            return;
        }
        let cmd = match id {
            MENU_OPEN => AppCommand::Open,
            MENU_SETTINGS => AppCommand::OpenSettings,
            MENU_QUIT => AppCommand::Quit,
            MENU_RECORD if is_recording => AppCommand::RecordToggle,
            MENU_RECORD => AppCommand::RecordStart { fast: false, from_app: false },
            MENU_RECORD_FAST => AppCommand::RecordStart { fast: true, from_app: false },
            _ => return,
        };
        waker.send(cmd);
    };

    let result = if use_native {
        let mut opened_ui = false;
        let res = show_native_popup_menu(entries, theme, |id| {
            opened_ui = matches!(id, MENU_OPEN | MENU_SETTINGS | MENU_EDITOR);
            on_select(id);
        });
        if !opened_ui && !RECORDING.load(Ordering::Relaxed) {
            platform_windows::shell::trim_working_set();
        }
        res
    } else {
        let header = MenuHeader {
            title: "CapShotX".into(),
            subtitle: if is_recording { "Recording…".into() } else { "Running in the tray".into() },
            icon: Some(RgbaImage {
                rgba: crate::icon::rgba(64),
                width: 64,
                height: 64,
            }),
        };
        show_popup_menu(Some(header), entries, theme, on_select)
    };
    if let Err(err) = result {
        tracing::error!("could not show tray menu: {err}");
    }
}
