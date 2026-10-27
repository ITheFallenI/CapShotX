//! Screenshot capture: frozen-screen overlay, region/window/monitor
//! selection, clipboard and PNG output. Independent of every editor.

#![cfg(windows)]

mod frame;
mod output;
mod overlay;
mod windows_list;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use platform_windows::{clipboard, monitors, shell, toast};

pub use frame::Frame;
pub use output::{DEFAULT_FILE_TEMPLATE, render_template};
use overlay::{Delivery, OverlayOptions, OverlayResult, Purpose};
pub use platform_windows::monitors::PixelRect;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureMode {
    /// Drag a region, or click a window / the desktop.
    Region,
    Window,
    Monitor,
}

/// What happens once a region is chosen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AfterCapture {
    Copy,
    CopyAndSave,
    /// Keep the selection adjustable and show Copy / Save buttons.
    Ask,
}

#[derive(Clone, Debug)]
pub struct CaptureOptions {
    pub mode: CaptureMode,
    pub after: AfterCapture,
    pub magnifier: bool,
    pub save_folder: PathBuf,
    pub file_template: String,
    /// Selection border / handle colour (sRGB).
    pub highlight: [u8; 3],
    /// Play a shutter sound when a capture is taken.
    pub sound: bool,
    /// Theme for the confirmation toast.
    pub dark: bool,
}

#[derive(Debug)]
pub enum CaptureOutcome {
    Cancelled,
    Captured {
        width: u32,
        height: u32,
        copied: bool,
        saved: Option<PathBuf>,
    },
    ColorCopied(String),
    Failed(String),
}

#[derive(Debug)]
pub struct Finished {
    pub outcome: CaptureOutcome,
    /// Magnifier state when the overlay closed (toggled with Z).
    pub magnifier: bool,
}

/// Another capture is already in progress.
#[derive(Debug)]
pub struct Busy;

static ACTIVE: AtomicBool = AtomicBool::new(false);

/// Starts a capture session on its own thread. `on_finish` runs on that
/// thread once the overlay closes and output is delivered (before the
/// confirmation toast). Repeated calls during a session return [`Busy`].
pub fn start(options: CaptureOptions, on_finish: impl FnOnce(Finished) + Send + 'static) -> Result<(), Busy> {
    if ACTIVE.swap(true, Ordering::AcqRel) {
        return Err(Busy);
    }
    let spawned = std::thread::Builder::new()
        .name("capture".into())
        .spawn(move || session(options, on_finish));
    if spawned.is_err() {
        ACTIVE.store(false, Ordering::Release);
        return Err(Busy);
    }
    Ok(())
}

fn session(options: CaptureOptions, on_finish: impl FnOnce(Finished)) {
    let started = Instant::now();
    let frame = match frame::grab_virtual_screen() {
        Ok(frame) => Arc::new(frame),
        Err(err) => {
            ACTIVE.store(false, Ordering::Release);
            let message = format!("Couldn't read the screen: {err}");
            tracing::error!("{message}");
            on_finish(Finished { outcome: CaptureOutcome::Failed(message.clone()), magnifier: options.magnifier });
            notify_error(&message, options.dark);
            return;
        }
    };
    let windows = windows_list::visible_windows();
    let monitors = monitors::enumerate();
    tracing::info!(
        "froze {}x{} desktop ({} windows) in {} ms",
        frame.width(),
        frame.height(),
        windows.len(),
        started.elapsed().as_millis()
    );

    let (result, magnifier) = overlay::run(
        frame.clone(),
        windows,
        monitors,
        OverlayOptions {
            mode: options.mode,
            after: options.after,
            magnifier: options.magnifier,
            highlight: options.highlight,
            purpose: Purpose::Screenshot,
        },
    );
    // A new capture may start as soon as the overlay is gone.
    ACTIVE.store(false, Ordering::Release);

    let finish = |outcome| Finished { outcome, magnifier };
    match result {
        OverlayResult::Cancelled | OverlayResult::Selected { .. } => on_finish(finish(CaptureOutcome::Cancelled)),
        OverlayResult::Failed(message) => {
            tracing::error!("capture overlay failed: {message}");
            on_finish(finish(CaptureOutcome::Failed(message.clone())));
            notify_error(&message, options.dark);
        }
        OverlayResult::CopyColor(hex) => match clipboard::set_text(&hex) {
            Ok(()) => {
                on_finish(finish(CaptureOutcome::ColorCopied(hex.clone())));
                notify(toast::Toast {
                    title: format!("Copied {hex}"),
                    subtitle: "Colour copied to the clipboard".into(),
                    image: None,
                    open_on_click: None,
                    error: false,
                    dark: options.dark,
                });
            }
            Err(err) => {
                let message = format!("Couldn't copy the colour: {err}");
                on_finish(finish(CaptureOutcome::Failed(message.clone())));
                notify_error(&message, options.dark);
            }
        },
        OverlayResult::Commit { rect, delivery } => {
            let Some(image) = frame.crop(rect) else {
                on_finish(finish(CaptureOutcome::Cancelled));
                return;
            };
            drop(frame);
            if options.sound {
                platform_windows::sound::play_shutter();
            }
            match deliver(&image, delivery, &options) {
                Ok((copied, saved)) => {
                    tracing::info!(
                        "captured {}x{} (copied: {copied}, saved: {:?}) in {} ms",
                        image.width(),
                        image.height(),
                        saved,
                        started.elapsed().as_millis()
                    );
                    let title = match (copied, &saved) {
                        (true, Some(_)) => "Copied and saved",
                        (true, None) => "Copied to clipboard",
                        (false, _) => "Screenshot saved",
                    };
                    let subtitle = match &saved {
                        Some(path) => path.file_name().unwrap_or_default().to_string_lossy().into_owned(),
                        None => format!("{} × {} px", image.width(), image.height()),
                    };
                    on_finish(finish(CaptureOutcome::Captured {
                        width: image.width(),
                        height: image.height(),
                        copied,
                        saved: saved.clone(),
                    }));
                    notify(toast::Toast {
                        title: title.into(),
                        subtitle,
                        open_on_click: saved,
                        image: Some(toast::ToastImage { width: image.width(), height: image.height(), bgra: image.bgra }),
                        error: false,
                        dark: options.dark,
                    });
                }
                Err(message) => {
                    tracing::error!("capture output failed: {message}");
                    on_finish(finish(CaptureOutcome::Failed(message.clone())));
                    notify_error(&message, options.dark);
                }
            }
        }
    }
}

/// What the user picked to record.
#[derive(Clone, Copy, Debug)]
pub enum Selection {
    Cancelled,
    Area {
        /// Virtual-desktop pixels.
        rect: PixelRect,
        /// Set when a window was clicked (record the window itself).
        window: Option<isize>,
    },
}

/// Shows the selection overlay for choosing a recording area. Blocks the
/// calling thread until the user picks or cancels.
pub fn select_area(magnifier: bool, highlight: [u8; 3]) -> Result<Selection, String> {
    if ACTIVE.swap(true, Ordering::AcqRel) {
        return Err("A capture is already in progress".into());
    }
    let result = (|| {
        let frame = Arc::new(frame::grab_virtual_screen().map_err(|e| format!("Couldn't read the screen: {e}"))?);
        let (result, _) = overlay::run(
            frame,
            windows_list::visible_windows(),
            monitors::enumerate(),
            OverlayOptions {
                mode: CaptureMode::Region,
                after: AfterCapture::Copy,
                magnifier,
                highlight,
                purpose: Purpose::Record,
            },
        );
        match result {
            OverlayResult::Selected { rect, window } => Ok(Selection::Area { rect, window }),
            OverlayResult::Failed(err) => Err(err),
            _ => Ok(Selection::Cancelled),
        }
    })();
    ACTIVE.store(false, Ordering::Release);
    result
}

/// Encodes once, then copies and/or saves. Returns (copied, saved path).
fn deliver(image: &Frame, delivery: Delivery, options: &CaptureOptions) -> Result<(bool, Option<PathBuf>), String> {
    let png = output::encode_png(image).map_err(|e| format!("Couldn't encode the PNG: {e}"))?;
    let copy = matches!(delivery, Delivery::Copy | Delivery::CopyAndSave);
    let save = matches!(delivery, Delivery::Save | Delivery::CopyAndSave);
    if copy {
        clipboard::set_image(image.width(), image.height(), &image.bgra, &png)
            .map_err(|e| format!("Couldn't copy to the clipboard: {e}"))?;
    }
    let saved = if save {
        Some(output::save_png(&png, &options.save_folder, &options.file_template, shell::local_time())?)
    } else {
        None
    };
    Ok((copy, saved))
}

fn notify(t: toast::Toast) {
    if let Err(err) = toast::show_toast(t) {
        tracing::warn!("couldn't show toast: {err}");
    }
}

fn notify_error(message: &str, dark: bool) {
    notify(toast::Toast {
        title: "Capture failed".into(),
        subtitle: message.into(),
        image: None,
        open_on_click: None,
        error: true,
        dark,
    });
}
