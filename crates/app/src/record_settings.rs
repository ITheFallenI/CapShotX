//! Settings › Recording: frame rate, cursor, countdown and audio sources
//! with device pickers and live level meters (REC-02: show real capture
//! activity and tell silence apart from a failed source).

use std::time::{Duration, Instant};

use egui::{Color32, RichText, Sense, Ui, vec2};
use platform_windows::audio::{AudioDevice, Direction, list_devices};
use recorder::{AudioSource, Kind};

use crate::settings::RecordSettings;
use crate::widgets::{self, palette};

const DEVICE_REFRESH: Duration = Duration::from_secs(3);
const RETRY: Duration = Duration::from_secs(3);

#[derive(Default)]
struct Meter {
    /// Device being monitored (`None` inside = default device).
    key: Option<Option<String>>,
    source: Option<AudioSource>,
    error: Option<String>,
    failed_at: Option<Instant>,
    started_at: Option<Instant>,
    level: f32,
    /// Last time real sound came through (holds the status steady).
    heard_at: Option<Instant>,
}

impl Meter {
    fn sync(&mut self, kind: Kind, want: Option<Option<String>>) {
        let retry = self.failed_at.is_some_and(|t| t.elapsed() >= RETRY);
        if self.key == want && !retry {
            return;
        }
        self.source = None;
        self.error = None;
        self.failed_at = None;
        self.key = want.clone();
        if let Some(device) = want {
            match AudioSource::monitor(kind, device) {
                Ok(s) => {
                    self.source = Some(s);
                    self.started_at = Some(Instant::now());
                }
                Err(e) => {
                    self.error = Some(e);
                    self.failed_at = Some(Instant::now());
                }
            }
        }
    }

    fn ui(&mut self, ui: &mut Ui, kind: Kind) {
        let p = palette(ui);
        let (status, color) = if let Some(err) = &self.error {
            (err.clone(), p.danger)
        } else if let Some(s) = &self.source {
            let (peak, _) = s.status.levels();
            self.level = peak.max(self.level * 0.85);
            if peak > 0.002 {
                self.heard_at = Some(Instant::now());
            }
            let hearing = self.heard_at.is_some_and(|t| t.elapsed() < Duration::from_millis(1500));
            let settled = self.started_at.is_some_and(|t| t.elapsed() > Duration::from_millis(800));
            if !s.status.is_connected() {
                ("Device disconnected".into(), p.danger)
            } else if kind == Kind::Microphone && settled && s.status.packets() == 0 {
                ("No signal from this microphone".into(), p.danger)
            } else if hearing {
                ("Receiving audio".into(), p.success)
            } else {
                (if kind == Kind::Desktop { "Ready · nothing playing".into() } else { "Ready · silent".into() }, p.muted)
            }
        } else {
            (String::new(), p.muted)
        };

        let (rect, _) = ui.allocate_exact_size(vec2(120.0, 6.0), Sense::hover());
        let painter = ui.painter();
        painter.rect_filled(rect, 3, p.control_strong);
        let w = (self.level.sqrt() * rect.width()).min(rect.width());
        if w > 0.5 {
            let c = if self.level > 0.9 { p.danger } else if self.level > 0.5 { Color32::from_rgb(245, 158, 11) } else { p.success };
            painter.rect_filled(egui::Rect::from_min_size(rect.min, vec2(w, rect.height())), 3, c);
        }
        // Fixed-width slot so changing text never resizes the row.
        let slot = vec2(190.0, 18.0);
        ui.allocate_ui_with_layout(slot, egui::Layout::left_to_right(egui::Align::Center), |ui| {
            ui.set_min_size(slot);
            ui.set_max_width(slot.x);
            ui.add(egui::Label::new(RichText::new(status).size(12.0).color(color)).truncate());
        });
    }
}

#[derive(Default)]
pub struct RecordPanel {
    outputs: Vec<AudioDevice>,
    inputs: Vec<AudioDevice>,
    listed_at: Option<Instant>,
    desktop: Meter,
    mic: Meter,
}

impl RecordPanel {
    /// Stops level monitoring (leaving Settings, or a recording starts).
    pub fn release(&mut self) {
        self.desktop = Meter::default();
        self.mic = Meter::default();
    }

    pub fn ui(&mut self, ui: &mut Ui, r: &mut RecordSettings, recording: bool) {
        if self.listed_at.is_none_or(|t| t.elapsed() >= DEVICE_REFRESH) {
            self.outputs = list_devices(Direction::Output).unwrap_or_default();
            self.inputs = list_devices(Direction::Input).unwrap_or_default();
            self.listed_at = Some(Instant::now());
        }
        let p = palette(ui);

        widgets::setting_row(ui, "Frame rate", "60 FPS is smoother but makes larger files", |ui| {
            widgets::segmented(ui, &mut r.fps, &[(60, "60 FPS"), (30, "30 FPS")]);
        });
        ui.separator();
        widgets::setting_row(ui, "Countdown", "Time to get ready before recording starts", |ui| {
            widgets::segmented(ui, &mut r.countdown, &[(5, "5 s"), (3, "3 s"), (0, "Off")]);
        });
        ui.separator();
        widgets::setting_row(ui, "Record cursor", "Include the mouse pointer in the video", |ui| {
            widgets::toggle(ui, &mut r.cursor);
        });

        ui.separator();
        widgets::setting_row(ui, "Desktop audio", "Sound playing on your speakers or headphones", |ui| {
            widgets::toggle(ui, &mut r.desktop_audio);
        });
        if r.desktop_audio {
            device_row(ui, "desktop-device", &self.outputs, &mut r.desktop_device, "Follow Windows default", |ui| {
                self.desktop.ui(ui, Kind::Desktop)
            });
        }
        ui.separator();
        widgets::setting_row(ui, "Microphone", "Your voice, from the selected input", |ui| {
            widgets::toggle(ui, &mut r.microphone);
        });
        if r.microphone {
            device_row(ui, "mic-device", &self.inputs, &mut r.microphone_device, "Windows default", |ui| {
                self.mic.ui(ui, Kind::Microphone)
            });
        }
        if recording {
            ui.label(RichText::new("Level meters pause while a recording is running.").size(12.0).color(p.muted));
        }

        // Monitor the chosen devices while this page is open.
        let want = |on: bool, dev: &Option<String>| (on && !recording).then(|| dev.clone());
        self.desktop.sync(Kind::Desktop, want(r.desktop_audio, &r.desktop_device));
        self.mic.sync(Kind::Microphone, want(r.microphone, &r.microphone_device));
        if self.desktop.source.is_some() || self.mic.source.is_some() {
            ui.ctx().request_repaint_after(Duration::from_millis(50));
        }
    }
}

fn device_row(
    ui: &mut Ui,
    id: &str,
    devices: &[AudioDevice],
    selected: &mut Option<String>,
    default_label: &str,
    meter: impl FnOnce(&mut Ui),
) {
    let default_name = devices.iter().find(|d| d.is_default).map(|d| d.name.as_str());
    let label_default = match default_name {
        Some(n) => format!("{default_label} ({n})"),
        None => default_label.to_string(),
    };
    let current = match selected {
        None => label_default.clone(),
        Some(id) => devices
            .iter()
            .find(|d| &d.id == id)
            .map_or_else(|| "Disconnected device".to_string(), |d| d.name.clone()),
    };
    ui.horizontal(|ui| {
        ui.add_space(4.0);
        egui::ComboBox::from_id_salt(id)
            .width(300.0)
            .selected_text(current)
            .show_ui(ui, |ui| {
                ui.selectable_value(selected, None, label_default);
                for d in devices {
                    ui.selectable_value(selected, Some(d.id.clone()), &d.name);
                }
            });
        ui.add_space(8.0);
        meter(ui);
    });
    ui.add_space(4.0);
}
