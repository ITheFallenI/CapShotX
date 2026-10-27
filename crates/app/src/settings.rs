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
    Nothing,
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RecordSettings {
    /// 30 or 60.
    pub fps: u32,
    pub cursor: bool,
    pub desktop_audio: bool,
    /// `None` = follow the Windows default output.
    pub desktop_device: Option<String>,
    pub microphone: bool,
    /// `None` = the Windows default input.
    pub microphone_device: Option<String>,
    /// Seconds before recording starts (0 = immediately).
    pub countdown: u32,
}

impl Default for RecordSettings {
    fn default() -> Self {
        Self {
            fps: 30,
            cursor: true,
            desktop_audio: true,
            desktop_device: None,
            microphone: false,
            microphone_device: None,
            countdown: 3,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub close_action: CloseAction,
    pub tray_left_click: TrayLeftClick,
    pub theme: ThemeChoice,
    pub start_hidden: bool,
    /// Overrides of default shortcuts; an empty string disables the shortcut.
    pub shortcuts: BTreeMap<Action, String>,
    pub capture: CaptureSettings,
    pub record: RecordSettings,
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
        match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|err| {
                tracing::warn!("ignoring invalid settings at {}: {err}", path.display());
                Self::default()
            }),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(err) => {
                tracing::warn!("could not read settings at {}: {err}", path.display());
                Self::default()
            }
        }
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
