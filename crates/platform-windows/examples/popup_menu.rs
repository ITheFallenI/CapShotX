//! Shows the tray popup menu at the cursor, for visual testing.
//!
//! `cargo run -p platform-windows --example popup_menu [dark]`

use platform_windows::popup_menu::{
    MenuEntry, MenuHeader, MenuTheme, is_popup_menu_open, show_popup_menu,
};
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::WindowsAndMessaging::{DispatchMessageW, GetMessageW, MSG, TranslateMessage};

fn main() -> windows::core::Result<()> {
    let dark = std::env::args().any(|a| a == "dark");
    // SAFETY: called once at startup before any window exists.
    unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)? };

    show_popup_menu(
        Some(MenuHeader {
            title: "CapShotX".into(),
            subtitle: "Running in the tray".into(),
            icon: None,
        }),
        vec![
            MenuEntry::item("open", "Open CapShotX"),
            MenuEntry::item("settings", "Settings"),
            MenuEntry::Separator,
            MenuEntry::item("quit", "Quit CapShotX"),
        ],
        MenuTheme { dark },
        |id| println!("selected: {id}"),
    )?;

    let mut msg = MSG::default();
    // SAFETY: standard message loop on the thread that owns the menu.
    unsafe {
        while is_popup_menu_open() && GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    Ok(())
}
