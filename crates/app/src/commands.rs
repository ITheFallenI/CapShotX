use serde::{Deserialize, Serialize};

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
    #[serde(rename = "app.open")]
    OpenApp,
}

impl Action {
    pub const ALL: &[Action] = &[
        Action::CaptureRegion,
        Action::CaptureWindow,
        Action::CaptureMonitor,
        Action::RecordToggle,
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
            Action::OpenApp => "Open CapShotX",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Action::CaptureRegion => "Drag an area, or click a window or the desktop",
            Action::CaptureWindow => "Click the window to capture",
            Action::CaptureMonitor => "Click the display to capture",
            Action::RecordToggle => "Pick an area or window to record; press again to stop",
            Action::OpenApp => "Show the home window from anywhere",
        }
    }

    /// Default binding; empty means unbound by default.
    pub fn default_shortcut(self) -> &'static str {
        match self {
            Action::CaptureRegion => "Ctrl+Shift+2",
            Action::CaptureWindow | Action::CaptureMonitor => "",
            Action::RecordToggle => "Ctrl+Shift+5",
            Action::OpenApp => "Ctrl+Shift+G",
        }
    }
}

/// Requests delivered to the UI thread from the tray, hotkeys, capture
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
    /// Stop button on the recording toolbar.
    RecordingStop,
    RecordingStarted(Result<crate::screen_record::Active, String>),
    RecordingFinished(Result<recorder::Summary, String>),
}
