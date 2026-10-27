//! Visual check of the recording toolbar: `cargo run -p platform-windows
//! --example rec_toolbar [dark] [warn] [paused]`. Capture exclusion is lifted
//! for the demo so it can be screenshotted.

use std::time::{Duration, Instant};

use platform_windows::rec_toolbar::{RecordingToolbar, ToolbarStatus};
use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, SetWindowDisplayAffinity, WDA_NONE};
use windows::core::w;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let has = |a: &str| args.iter().any(|x| x == a);
    let (warn, paused, muted) = (has("warn"), has("paused"), has("muted"));
    let start = Instant::now();
    let _bar = RecordingToolbar::show(
        (100, 100),
        has("dark"),
        move || {
            let t = start.elapsed().as_secs_f32();
            ToolbarStatus {
                elapsed: Duration::from_secs(83) + start.elapsed(),
                paused,
                muted,
                level: Some(((t * 7.0).sin() * 0.5 + 0.5) * 0.6),
                warning: warn.then(|| "Microphone: \"Headset\" was disconnected at 1:12".into()),
                countdown: None,
            }
        },
        |a| println!("action: {a:?}"),
    )
    .unwrap();
    std::thread::sleep(Duration::from_millis(300));
    // SAFETY: demo only — make our own toolbar visible to screenshots.
    unsafe {
        if let Ok(h) = FindWindowW(w!("CapShotX.RecordingToolbar"), None) {
            let _ = SetWindowDisplayAffinity(h, WDA_NONE);
        }
    }
    std::thread::sleep(Duration::from_secs(std::env::var("SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(4)));
}
