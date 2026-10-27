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

use platform_windows::single_instance::{Acquire, SingleInstance};
use tracing_subscriber::Layer as _;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;
use tracing_subscriber::{EnvFilter, fmt};

const APP_ID: &str = "CapShotX";

fn main() -> anyhow::Result<()> {
    let _log_guard = init_logging();

    let instance = match SingleInstance::acquire(APP_ID)? {
        Acquire::Primary(instance) => instance,
        Acquire::AlreadyRunning => {
            tracing::info!("CapShotX is already running; asked it to show itself");
            return Ok(());
        }
    };
    tracing::info!("CapShotX {} starting", env!("CARGO_PKG_VERSION"));

    let settings = settings::Settings::load();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("CapShotX")
            .with_app_id(APP_ID)
            .with_inner_size([1040.0, 680.0])
            .with_min_inner_size([820.0, 540.0])
            .with_icon(Arc::new(egui::IconData {
                rgba: icon::rgba(64),
                width: 64,
                height: 64,
            }))
            .with_visible(!settings.start_hidden),
        ..Default::default()
    };

    let result = eframe::run_native(
        "CapShotX",
        options,
        Box::new(move |cc| Ok(Box::new(app::CapShotApp::new(cc, settings, instance)?))),
    )
    .map_err(|err| anyhow::anyhow!("{err}"));
    match &result {
        Ok(()) => tracing::info!("CapShotX exited"),
        Err(err) => tracing::error!("CapShotX failed: {err:#}"),
    }
    result
}

/// Logs to a daily-rotated file (7 days kept) and, in debug builds, stderr.
/// `RUST_LOG` overrides the default level. Never log captured content.
fn init_logging() -> Option<tracing_appender::non_blocking::WorkerGuard> {
    // GPU-stack crates are chatty at info; keep only their problems.
    const DEFAULT_FILTER: &str = "info,wgpu=error,wgpu_hal=error,wgpu_core=error,naga=warn,egui_wgpu=warn";
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
