use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::Context as _;
use serde::{Deserialize, Serialize};

use crate::commands::Action;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum CloseAction {
    #[default]
    HideToTray,
    Quit,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrayLeftClick {
    #[default]
    OpenApp,
    OpenLatestCaptureFolder,
    CaptureRegion,
    RecordRegion,
    FastRecord,
    OpenGallery,
    OpenSettings,
    OpenCapturesFolder,
    Nothing,
}

impl TrayLeftClick {
    pub const ALL: &[TrayLeftClick] = &[
        TrayLeftClick::OpenApp,
        TrayLeftClick::OpenLatestCaptureFolder,
        TrayLeftClick::CaptureRegion,
        TrayLeftClick::RecordRegion,
        TrayLeftClick::FastRecord,
        TrayLeftClick::OpenGallery,
        TrayLeftClick::OpenSettings,
        TrayLeftClick::OpenCapturesFolder,
        TrayLeftClick::Nothing,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::OpenApp => "Open CapShotX",
            Self::OpenLatestCaptureFolder => "Open latest capture folder",
            Self::CaptureRegion => "Capture a region",
            Self::RecordRegion => "Record a region",
            Self::FastRecord => "Fast record",
            Self::OpenGallery => "Open Gallery",
            Self::OpenSettings => "Open Settings",
            Self::OpenCapturesFolder => "Open captures folder",
            Self::Nothing => "Do nothing",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::OpenApp => "Opens the CapShotX window. Right-click always opens the tray menu.",
            Self::OpenLatestCaptureFolder => {
                "Opens the last saved file's folder. Before your first save, opens the captures folder."
            }
            Self::CaptureRegion => "Starts a region or window screenshot immediately from the tray.",
            Self::RecordRegion => "Starts screen recording (or stops an active recording).",
            Self::FastRecord => "Picks an area or window and starts recording immediately.",
            Self::OpenGallery => "Opens CapShotX directly to the Gallery.",
            Self::OpenSettings => "Opens CapShotX directly to Settings.",
            Self::OpenCapturesFolder => "Opens your captures folder in File Explorer.",
            Self::Nothing => "Ignores left-clicks on the tray icon; right-click opens the tray menu.",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThemeChoice {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum AfterCapture {
    Copy,
    #[default]
    CopyAndSave,
    Ask,
}

impl From<AfterCapture> for capture::AfterCapture {
    fn from(value: AfterCapture) -> Self {
        match value {
            AfterCapture::Copy => Self::Copy,
            AfterCapture::CopyAndSave => Self::CopyAndSave,
            AfterCapture::Ask => Self::Ask,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CaptureSettings {
    pub after: AfterCapture,
    pub magnifier: bool,
    /// `None` means the default (Pictures\CapShotX).
    pub save_folder: Option<PathBuf>,
    pub file_template: String,
    /// Selection highlight colour (sRGB).
    pub highlight: [u8; 3],
    pub sound: bool,
    pub show_notification: bool,
}

pub const DEFAULT_HIGHLIGHT: [u8; 3] = [110, 123, 255];

impl Default for CaptureSettings {
    fn default() -> Self {
        Self {
            after: AfterCapture::default(),
            magnifier: true,
            save_folder: None,
            file_template: capture::DEFAULT_FILE_TEMPLATE.to_owned(),
            highlight: DEFAULT_HIGHLIGHT,
            sound: true,
            show_notification: true,
        }
    }
}

impl CaptureSettings {
    pub fn save_folder(&self) -> PathBuf {
        self.save_folder.clone().unwrap_or_else(default_save_folder)
    }
}

fn default_save_folder() -> PathBuf {
    directories::UserDirs::new()
        .and_then(|dirs| dirs.picture_dir().map(|p| p.join("CapShotX")))
        .or_else(|| directories::BaseDirs::new().map(|d| d.home_dir().join("Pictures").join("CapShotX")))
        .unwrap_or_else(|| PathBuf::from("CapShotX"))
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum RecordStartMode {
    /// Camtasia-style: select initial bounds, adjust on the live desktop, then click Start.
    #[default]
    ConfirmBounds,
    /// Start countdown / recording immediately after selecting bounds.
    Fast,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum VideoQuality {
    Compact,
    #[default]
    Balanced,
    High,
}

impl From<VideoQuality> for recorder::VideoQuality {
    fn from(value: VideoQuality) -> Self {
        match value {
            VideoQuality::Compact => Self::Compact,
            VideoQuality::Balanced => Self::Balanced,
            VideoQuality::High => Self::High,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RecordSettings {
    pub start_mode: RecordStartMode,
    /// 30 or 60.
    pub fps: u32,
    pub quality: VideoQuality,
    pub cursor: bool,
    /// Show a capture-excluded border around the live recorded area.
    pub show_outline: bool,
    pub desktop_audio: bool,
    /// `None` = follow the Windows default output.
    pub desktop_device: Option<String>,
    pub desktop_gain: f32,
    pub microphone: bool,
    /// `None` = the Windows default input.
    pub microphone_device: Option<String>,
    pub microphone_gain: f32,
    /// Seconds before recording starts (0 = immediately).
    pub countdown: u32,
}

impl Default for RecordSettings {
    fn default() -> Self {
        Self {
            start_mode: RecordStartMode::default(),
            fps: 30,
            quality: VideoQuality::default(),
            cursor: true,
            show_outline: true,
            desktop_audio: true,
            desktop_device: None,
            desktop_gain: 1.0,
            microphone: false,
            microphone_device: None,
            microphone_gain: 1.0,
            countdown: 3,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub close_action: CloseAction,
    pub tray_left_click: TrayLeftClick,
    pub theme: ThemeChoice,
    pub launch_at_startup: bool,
    pub start_hidden: bool,
    pub low_memory_mode: bool,
    pub run_as_admin: bool,
    pub check_updates_on_startup: bool,
    /// Overrides of default shortcuts; an empty string disables the shortcut.
    pub shortcuts: BTreeMap<Action, String>,
    pub capture: CaptureSettings,
    pub record: RecordSettings,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            close_action: CloseAction::default(),
            tray_left_click: TrayLeftClick::default(),
            theme: ThemeChoice::default(),
            launch_at_startup: true,
            start_hidden: false,
            low_memory_mode: true,
            run_as_admin: false,
            check_updates_on_startup: true,
            shortcuts: BTreeMap::new(),
            capture: CaptureSettings::default(),
            record: RecordSettings::default(),
        }
    }
}

impl Settings {
    /// The effective shortcut text for `action` ("" when disabled).
    pub fn shortcut(&self, action: Action) -> &str {
        self.shortcuts
            .get(&action)
            .map_or(action.default_shortcut(), String::as_str)
    }

    pub fn set_shortcut(&mut self, action: Action, shortcut: String) {
        if shortcut == action.default_shortcut() {
            self.shortcuts.remove(&action);
        } else {
            self.shortcuts.insert(action, shortcut);
        }
    }

    pub fn thumbnail_cache_dir() -> Option<PathBuf> {
        directories::ProjectDirs::from("", "", "CapShotX")
            .map(|dirs| dirs.data_local_dir().join("thumbs"))
    }

    pub fn logs_dir() -> Option<PathBuf> {
        directories::ProjectDirs::from("", "", "CapShotX")
            .map(|dirs| dirs.data_local_dir().join("logs"))
    }

    pub fn path() -> Option<PathBuf> {
        directories::ProjectDirs::from("", "", "CapShotX")
            .map(|dirs| dirs.config_dir().join("settings.json"))
    }

    /// Loads settings, falling back to defaults if the file is missing or invalid.
    pub fn load() -> Self {
        let Some(path) = Self::path() else {
            return Self::default();
        };
        let mut s: Self = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|err| {
                tracing::warn!("ignoring invalid settings at {}: {err}", path.display());
                Self::default()
            }),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(err) => {
                tracing::warn!("could not read settings at {}: {err}", path.display());
                Self::default()
            }
        };
        s.migrate();
        s
    }

    fn migrate(&mut self) {
        if capture::LEGACY_DEFAULT_FILE_TEMPLATES.contains(&self.capture.file_template.as_str()) {
            self.capture.file_template = capture::DEFAULT_FILE_TEMPLATE.to_owned();
        }
        self.record.desktop_gain = self.record.desktop_gain.clamp(0.0, 2.0);
        self.record.microphone_gain = self.record.microphone_gain.clamp(0.0, 2.0);
    }

    /// Writes settings atomically (temp file + rename).
    pub fn save(&self) -> anyhow::Result<()> {
        let path = Self::path().context("no settings directory available")?;
        let dir = path.parent().context("settings path has no parent")?;
        std::fs::create_dir_all(dir)
            .with_context(|| format!("creating {}", dir.display()))?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)
            .with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, &path).with_context(|| format!("replacing {}", path.display()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrates_legacy_template_and_fills_new_record_fields() {
        let raw = r#"{
            "capture": { "file_template": "{year}-{month}/CapShotX_{date}_{time}" },
            "record": { "fps": 60, "desktop_audio": true }
        }"#;
        let mut s: Settings = serde_json::from_str(raw).unwrap();
        s.migrate();
        assert_eq!(s.capture.file_template, capture::DEFAULT_FILE_TEMPLATE);
        assert!(s.launch_at_startup);
        assert!(s.low_memory_mode);
        assert!(!s.run_as_admin);
        assert_eq!(s.tray_left_click, TrayLeftClick::OpenApp);
        assert_eq!(s.record.fps, 60);
        assert_eq!(s.record.start_mode, RecordStartMode::ConfirmBounds);
        assert_eq!(s.record.quality, VideoQuality::Balanced);
        assert!(s.record.show_outline);
        assert_eq!(s.record.desktop_gain, 1.0);
        assert_eq!(s.record.microphone_gain, 1.0);
    }
}
