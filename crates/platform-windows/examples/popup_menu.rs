//! Shows the tray popup menu at the cursor, for visual testing.
//!
//! `cargo run -p platform-windows --example popup_menu [dark] [native]`

use platform_windows::popup_menu::{
    MenuEntry, MenuHeader, MenuTheme, RgbaImage, is_popup_menu_open, show_native_popup_menu,
    show_popup_menu,
};
use windows::Win32::UI::WindowsAndMessaging::{DispatchMessageW, GetMessageW, MSG, TranslateMessage};

fn main() -> windows::core::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let dark = args.iter().any(|a| a == "dark");
    let native = args.iter().any(|a| a == "native");
    let hold = args.iter().any(|a| a == "hold");

    platform_windows::shell::init_process();
    if hold {
        platform_windows::shell::trim_working_set();
    }

    let entries = vec![
        MenuEntry::item("open", "Open CapShotX"),
        MenuEntry::item("record", "Record screen"),
        MenuEntry::item("record_fast", "Fast record"),
        MenuEntry::item("settings", "Settings"),
        MenuEntry::Separator,
        MenuEntry::item("quit", "Quit CapShotX"),
    ];
    let theme = MenuTheme { dark };

    if native {
        let mut selected = false;
        show_native_popup_menu(entries, theme, |id| {
            selected = true;
            println!("selected: {id}");
        })?;
        if hold && !selected {
            platform_windows::shell::trim_working_set();
        }
    } else {
        show_popup_menu(
            Some(MenuHeader {
                title: "CapShotX".into(),
                subtitle: "Running in the tray".into(),
                icon: Some(RgbaImage {
                    rgba: vec![110; 64 * 64 * 4],
                    width: 64,
                    height: 64,
                }),
            }),
            entries,
            theme,
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
    }

    if hold {
        std::thread::sleep(std::time::Duration::from_millis(400));
    }
    Ok(())
}
