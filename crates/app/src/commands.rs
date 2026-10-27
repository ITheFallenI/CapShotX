use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::settings::ThemeChoice;

/// User-triggerable actions that can be bound to global shortcuts.
///
/// Only actions whose feature exists are listed; later phases add more.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Action {
    #[serde(rename = "capture.region")]
    CaptureRegion,
    #[serde(rename = "capture.window")]
    CaptureWindow,
    #[serde(rename = "capture.monitor")]
    CaptureMonitor,
    #[serde(rename = "record.toggle")]
    RecordToggle,
    #[serde(rename = "record.fast")]
    RecordFast,
    #[serde(rename = "record.pause")]
    RecordPause,
    #[serde(rename = "app.open")]
    OpenApp,
}

impl Action {
    pub const ALL: &[Action] = &[
        Action::CaptureRegion,
        Action::CaptureWindow,
        Action::CaptureMonitor,
        Action::RecordToggle,
        Action::RecordFast,
        Action::RecordPause,
        Action::OpenApp,
    ];

    /// Stable id used for `RegisterHotKey`.
    pub fn hotkey_id(self) -> u32 {
        match self {
            Action::OpenApp => 1,
            Action::CaptureRegion => 2,
            Action::CaptureWindow => 3,
            Action::CaptureMonitor => 4,
            Action::RecordToggle => 5,
            Action::RecordFast => 6,
            Action::RecordPause => 7,
        }
    }

    pub fn from_hotkey_id(id: u32) -> Option<Self> {
        Self::ALL.iter().copied().find(|a| a.hotkey_id() == id)
    }

    pub fn label(self) -> &'static str {
        match self {
            Action::CaptureRegion => "Capture region",
            Action::CaptureWindow => "Capture window",
            Action::CaptureMonitor => "Capture display",
            Action::RecordToggle => "Start / stop recording",
            Action::RecordFast => "Fast record",
            Action::RecordPause => "Pause / resume recording",
            Action::OpenApp => "Open CapShotX",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Action::CaptureRegion => "Drag an area, or click a window or the desktop",
            Action::CaptureWindow => "Click the window to capture",
            Action::CaptureMonitor => "Click the display to capture",
            Action::RecordToggle => "Pick bounds to record; press again to start (when staging) or stop",
            Action::RecordFast => "Pick an area or window and start recording immediately",
            Action::RecordPause => "Pause or resume an active screen recording",
            Action::OpenApp => "Show the home window from anywhere",
        }
    }

    /// Default binding; empty means unbound by default.
    pub fn default_shortcut(self) -> &'static str {
        match self {
            Action::CaptureRegion => "Ctrl+Shift+2",
            Action::CaptureWindow | Action::CaptureMonitor | Action::RecordFast | Action::RecordPause => "",
            Action::RecordToggle => "Ctrl+Shift+5",
            Action::OpenApp => "Ctrl+Shift+G",
        }
    }
}

/// Requests delivered to the main thread from the tray, hotkeys, capture
/// sessions and second-instance launches.
#[derive(Debug)]
pub enum AppCommand {
    TrayLeftClick,
    Open,
    OpenSettings,
    Quit,
    Hotkey(Action),
    CaptureFinished {
        finished: capture::Finished,
        /// The main window was hidden for the capture; show it again.
        restore_window: bool,
    },
    RecordToggle,
    RecordStart {
        fast: bool,
        from_app: bool,
    },
    RecordSyncAudio {
        desktop_audio: bool,
        microphone: bool,
    },
    /// Stop button on the recording toolbar.
    RecordingStop,
    RecordingStarted(Result<crate::screen_record::Active, String>),
    RecordingFinished(Result<recorder::Summary, String>),
    RecoveredRecordings(Vec<std::path::PathBuf>),
}

/// Sends [`AppCommand`]s and wakes whichever loop is currently active on the
/// main thread (either the lightweight Win32 tray pump or `eframe`), without
/// keeping an `egui::Context` or GPU renderer alive in tray mode.
#[derive(Clone)]
pub struct Waker {
    tx: Sender<AppCommand>,
    main_thread_id: u32,
    egui_ctx: Arc<Mutex<Option<egui::Context>>>,
    theme_choice: Arc<AtomicU8>,
    low_memory: Arc<AtomicBool>,
}

impl Waker {
    pub fn new(
        tx: Sender<AppCommand>,
        main_thread_id: u32,
        theme: ThemeChoice,
        low_memory_mode: bool,
    ) -> Self {
        Self {
            tx,
            main_thread_id,
            egui_ctx: Arc::new(Mutex::new(None)),
            theme_choice: Arc::new(AtomicU8::new(encode_theme(theme))),
            low_memory: Arc::new(AtomicBool::new(low_memory_mode)),
        }
    }

    pub fn send(&self, cmd: AppCommand) {
        let _ = self.tx.send(cmd);
        if let Some(ctx) = self.egui_ctx.lock().expect("waker mutex poisoned").as_ref() {
            ctx.request_repaint();
        }
        platform_windows::shell::wake_thread(self.main_thread_id);
    }

    pub fn attach_ctx(&self, ctx: egui::Context) {
        *self.egui_ctx.lock().expect("waker mutex poisoned") = Some(ctx);
    }

    pub fn detach_ctx(&self) {
        *self.egui_ctx.lock().expect("waker mutex poisoned") = None;
    }

    pub fn set_theme_choice(&self, theme: ThemeChoice) {
        self.theme_choice.store(encode_theme(theme), Ordering::Relaxed);
    }

    pub fn set_low_memory_mode(&self, on: bool) {
        self.low_memory.store(on, Ordering::Relaxed);
    }

    pub fn low_memory_mode(&self) -> bool {
        self.low_memory.load(Ordering::Relaxed)
    }

    /// Resolves dark vs light theme even when no `egui` window exists.
    pub fn is_dark(&self) -> bool {
        if let Some(ctx) = self.egui_ctx.lock().expect("waker mutex poisoned").as_ref() {
            return ctx.theme() == egui::Theme::Dark;
        }
        match self.theme_choice.load(Ordering::Relaxed) {
            1 => false,
            2 => true,
            _ => platform_windows::shell::system_prefers_dark(),
        }
    }
}

fn encode_theme(theme: ThemeChoice) -> u8 {
    match theme {
        ThemeChoice::System => 0,
        ThemeChoice::Light => 1,
        ThemeChoice::Dark => 2,
    }
}
