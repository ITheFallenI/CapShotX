use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;

use platform_windows::popup_menu::{
    MenuEntry, MenuHeader, MenuTheme, RgbaImage, show_popup_menu,
};
use tray_icon::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

use crate::commands::AppCommand;

const MENU_OPEN: &str = "open";
const MENU_SETTINGS: &str = "settings";
const MENU_QUIT: &str = "quit";
const MENU_RECORD: &str = "record";

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

/// Creates the tray icon. Must be called on the UI thread after the event loop starts.
///
/// Right-click opens CapShotX's own popup menu; left-click is forwarded as
/// [`AppCommand::TrayLeftClick`] so the app can apply the user's preference.
pub fn install(ctx: egui::Context, tx: Sender<AppCommand>) -> anyhow::Result<Tray> {
    let icon = tray_icon::Icon::from_rgba(crate::icon::rgba(32), 32, 32)?;
    let tray = TrayIconBuilder::new()
        .with_tooltip("CapShotX")
        .with_icon(icon)
        .build()?;

    // The handler runs inside the UI thread's Win32 message loop, even while
    // the window is hidden; request_repaint wakes App::logic to drain the channel.
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
            MouseButton::Left => send(&ctx, &tx, AppCommand::TrayLeftClick),
            MouseButton::Right => show_menu(&ctx, &tx),
            MouseButton::Middle => {}
        }
    }));

    Ok(Tray { icon: tray })
}

fn send(ctx: &egui::Context, tx: &Sender<AppCommand>, cmd: AppCommand) {
    let _ = tx.send(cmd);
    ctx.request_repaint();
}

fn show_menu(ctx: &egui::Context, tx: &Sender<AppCommand>) {
    let header = MenuHeader {
        title: "CapShotX".into(),
        subtitle: if RECORDING.load(Ordering::Relaxed) { "Recording…".into() } else { "Running in the tray".into() },
        icon: Some(RgbaImage {
            rgba: crate::icon::rgba(64),
            width: 64,
            height: 64,
        }),
    };
    let entries = vec![
        MenuEntry::item(MENU_OPEN, "Open CapShotX"),
        MenuEntry::item(MENU_RECORD, if RECORDING.load(Ordering::Relaxed) { "Stop recording" } else { "Record screen" }),
        MenuEntry::item(MENU_SETTINGS, "Settings"),
        MenuEntry::Separator,
        MenuEntry::item(MENU_QUIT, "Quit CapShotX"),
    ];
    let theme = MenuTheme {
        dark: ctx.theme() == egui::Theme::Dark,
    };

    let (ctx, tx) = (ctx.clone(), tx.clone());
    let result = show_popup_menu(Some(header), entries, theme, move |id| {
        let cmd = match id {
            MENU_OPEN => AppCommand::Open,
            MENU_SETTINGS => AppCommand::OpenSettings,
            MENU_QUIT => AppCommand::Quit,
            MENU_RECORD => AppCommand::RecordToggle,
            _ => return,
        };
        send(&ctx, &tx, cmd);
    });
    if let Err(err) = result {
        tracing::error!("could not show tray menu: {err}");
    }
}
