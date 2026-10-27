#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod commands;
mod gallery_view;
mod icon;
mod record_settings;
mod screen_record;
mod settings;
mod theme;
mod tray;
mod widgets;

use std::sync::Arc;
use std::time::{Duration, Instant};

use platform_windows::single_instance::{Acquire, SingleInstance};
use tracing_subscriber::Layer as _;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;
use tracing_subscriber::{EnvFilter, fmt};

const APP_ID: &str = "CapShotX";

fn main() -> anyhow::Result<()> {
    // Set Per-Monitor V2 DPI and apartment COM before any hidden tray or
    // single-instance window is created, so launching straight to the tray
    // behaves identically to opening the UI window immediately.
    platform_windows::shell::init_process();

    let raw_args: Vec<String> = std::env::args().skip(1).collect();
    let has_flag = |flag: &str| raw_args.iter().any(|a| a == flag);
    let autostart = has_flag("--autostart");
    let no_elevate = has_flag("--no-elevate");
    let wait_restart = has_flag("--wait-restart");
    let open_settings = has_flag("--open-settings");
    let no_startup_sync = has_flag("--no-startup-sync");

    // Fast path: if an instance (elevated or not) is already running, ask it to
    // show itself and exit before prompting UAC or opening log files.
    if !wait_restart && SingleInstance::try_activate_existing(APP_ID) {
        return Ok(());
    }

    let settings = settings::Settings::load();
    let mut elevation_declined = None;
    if settings.run_as_admin && !platform_windows::shell::is_elevated() && !no_elevate {
        let forward: Vec<&str> = raw_args.iter().map(String::as_str).collect();
        match platform_windows::shell::relaunch_as_admin(&forward) {
            Ok(()) => return Ok(()),
            Err(err) => elevation_declined = Some(err),
        }
    }

    let _log_guard = init_logging();
    if let Some(err) = elevation_declined {
        tracing::warn!("continuing without administrator elevation: {err}");
    }

    let instance = acquire_instance(wait_restart)?;
    let Some(instance) = instance else {
        tracing::info!("CapShotX is already running; asked it to show itself");
        return Ok(());
    };
    tracing::info!(
        "CapShotX {} starting (elevated={}, autostart={autostart})",
        env!("CARGO_PKG_VERSION"),
        platform_windows::shell::is_elevated()
    );

    let mut initial_visible = open_settings || (!settings.start_hidden && !autostart);
    let mut open_ui = initial_visible || !settings.low_memory_mode;
    let mut core = app::CoreState::new(settings, instance, !no_startup_sync)?;
    if open_settings {
        core.page = app::Page::Settings;
    }
    if !open_ui {
        core.trim_if_idle();
    }

    loop {
        if open_ui {
            let visible = std::mem::replace(&mut initial_visible, true);
            match run_ui_session(&mut core, visible)? {
                app::UiExit::Quit => break,
                app::UiExit::HideToTray => {
                    open_ui = false;
                    core.trim_if_idle();
                }
            }
        } else {
            match core.poll_tray() {
                app::TrayStep::OpenWindow => {
                    open_ui = true;
                    continue;
                }
                app::TrayStep::Quit => break,
                app::TrayStep::StayInTray => {}
            }
            if !platform_windows::shell::pump_messages_once() {
                break;
            }
        }
    }

    tracing::info!("CapShotX exited");
    Ok(())
}

fn acquire_instance(wait_restart: bool) -> anyhow::Result<Option<SingleInstance>> {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match SingleInstance::acquire(APP_ID)? {
            Acquire::Primary(instance) => return Ok(Some(instance)),
            Acquire::AlreadyRunning if wait_restart && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Acquire::AlreadyRunning => return Ok(None),
        }
    }
}

/// Opens the `eframe` UI window on demand and tears down the window, GL context
/// and all GPU textures as soon as it closes to the tray (in Low memory mode).
fn run_ui_session(core: &mut app::CoreState, visible: bool) -> anyhow::Result<app::UiExit> {
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("CapShotX")
        .with_app_id(APP_ID)
        .with_visible(visible)
        .with_inner_size(core.window_size)
        .with_min_inner_size([820.0, 540.0])
        .with_maximized(core.window_maximized)
        .with_icon(Arc::new(egui::IconData {
            rgba: icon::rgba(64),
            width: 64,
            height: 64,
        }));

    if let Some([px, py]) = core.window_pos_px {
        let displays = platform_windows::monitors::enumerate();
        let on_screen = displays
            .iter()
            .any(|m| m.bounds.contains(px as i32 + 60, py as i32 + 24));
        if on_screen {
            let primary_scale = displays
                .iter()
                .find(|m| m.primary)
                .map_or(1.0, |m| m.scale())
                .max(0.5);
            viewport = viewport.with_position([px / primary_scale, py / primary_scale]);
        }
    }

    let options = eframe::NativeOptions {
        viewport,
        run_and_return: true,
        ..Default::default()
    };

    let mut exit = app::UiExit::HideToTray;
    eframe::run_native("CapShotX", options, {
        let exit_ref = &mut exit;
        Box::new(move |cc| Ok(Box::new(app::CapShotApp::new(cc, core, exit_ref, visible)?)))
    })
    .map_err(|err| anyhow::anyhow!("{err}"))?;
    Ok(exit)
}

/// Logs to a daily-rotated file (7 days kept) and, in debug builds, stderr.
/// `RUST_LOG` overrides the default level. Never log captured content.
fn init_logging() -> Option<tracing_appender::non_blocking::WorkerGuard> {
    // Renderer crates can be chatty at info; keep only their problems.
    const DEFAULT_FILTER: &str = "info,egui_glow=warn,glutin=warn";
    let filter =
        || EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(DEFAULT_FILTER));
    let stderr = cfg!(debug_assertions).then(|| fmt::layer().with_filter(filter()));

    let (file, guard) = match settings::Settings::logs_dir().and_then(|dir| {
        tracing_appender::rolling::Builder::new()
            .rotation(tracing_appender::rolling::Rotation::DAILY)
            .filename_prefix("capshotx")
            .filename_suffix("log")
            .max_log_files(7)
            .build(dir)
            .ok()
    }) {
        Some(appender) => {
            let (writer, guard) = tracing_appender::non_blocking(appender);
            let layer = fmt::layer().with_ansi(false).with_writer(writer).with_filter(filter());
            (Some(layer), Some(guard))
        }
        None => (None, None),
    };

    tracing_subscriber::registry().with(stderr).with(file).init();

    std::panic::set_hook(Box::new(|info| {
        tracing::error!("panic: {info}");
    }));
    guard
}
