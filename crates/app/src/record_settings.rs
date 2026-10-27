//! Settings › Recording: start mode, quality, frame rate, cursor, countdown,
//! audio sources with device pickers, gain sliders, live level meters and a
//! short "Record audio test" check (REC-01 / REC-02).

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use egui::{Color32, RichText, Sense, Ui, vec2};
use platform_windows::audio::{AudioDevice, Direction, list_devices};
use platform_windows::{monitors, shell};
use recorder::{AudioInput, AudioSource, Kind, Recorder, Target};

use crate::settings::{RecordSettings, RecordStartMode, VideoQuality};
use crate::widgets::{self, palette};

const DEVICE_REFRESH: Duration = Duration::from_secs(3);
const RETRY: Duration = Duration::from_secs(3);
const AUDIO_TEST_SECS: u64 = 4;

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

    fn ui(&mut self, ui: &mut Ui, kind: Kind, gain: f32) {
        let p = palette(ui);
        let (status, color) = if let Some(err) = &self.error {
            (err.clone(), p.danger)
        } else if let Some(s) = &self.source {
            let (raw_peak, _) = s.status.levels();
            let peak = (raw_peak * gain).clamp(0.0, 1.0);
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

struct AudioTestRun {
    started_at: Instant,
    cancel: Arc<AtomicBool>,
    rx: Receiver<Result<AudioTestOutcome, String>>,
}

struct AudioTestOutcome {
    path: PathBuf,
    peak: f32,
    warning: Option<String>,
}

#[derive(Default)]
pub struct RecordPanel {
    outputs: Vec<AudioDevice>,
    inputs: Vec<AudioDevice>,
    listed_at: Option<Instant>,
    desktop: Meter,
    mic: Meter,
    test_run: Option<AudioTestRun>,
    last_test: Option<Result<AudioTestOutcome, String>>,
}

impl RecordPanel {
    /// Stops level monitoring (leaving Settings, or a recording starts).
    pub fn release(&mut self) {
        self.desktop = Meter::default();
        self.mic = Meter::default();
        if let Some(run) = self.test_run.take() {
            run.cancel.store(true, Ordering::Release);
        }
    }

    pub fn matches_query(query: &str) -> bool {
        const HAYSTACK: &str = "recording before recording adjust bounds fast start video quality high balanced compact mp4 frame rate 60 fps 30 fps countdown timer record cursor mouse pointer recording outline border desktop audio system sound speakers headphones gain microphone voice input audio check test";
        query.is_empty() || HAYSTACK.contains(query)
    }

    pub fn ui_filtered(&mut self, ui: &mut Ui, r: &mut RecordSettings, recording: bool, query: &str) {
        if self.listed_at.is_none_or(|t| t.elapsed() >= DEVICE_REFRESH) {
            self.outputs = list_devices(Direction::Output).unwrap_or_default();
            self.inputs = list_devices(Direction::Input).unwrap_or_default();
            self.listed_at = Some(Instant::now());
        }
        if let Some(run) = &self.test_run
            && let Ok(res) = run.rx.try_recv()
        {
            self.test_run = None;
            if !matches!(&res, Err(m) if m.is_empty()) {
                self.last_test = Some(res);
            }
        }
        let p = palette(ui);
        let q = query.trim().to_ascii_lowercase();
        let show = |keywords: &str| {
            q.is_empty() || q == "recording" || q == "record" || keywords.contains(&q)
        };
        let mut any = false;
        let sep = |ui: &mut Ui, any: &mut bool| {
            if *any {
                ui.separator();
            }
            *any = true;
        };

        if show("before recording adjust bounds fast start camtasia mode") {
            sep(ui, &mut any);
            widgets::setting_row(
                ui,
                "Before recording",
                "Adjust bounds on the live screen first (like Camtasia), or start right away",
                |ui| {
                    widgets::segmented(
                        ui,
                        &mut r.start_mode,
                        &[
                            (RecordStartMode::ConfirmBounds, "Adjust bounds"),
                            (RecordStartMode::Fast, "Fast start"),
                        ],
                    );
                },
            );
        }
        if show("video quality high balanced compact bitrate mp4") {
            sep(ui, &mut any);
            widgets::setting_row(
                ui,
                "Video quality",
                "High preserves fine text and gradients; Compact makes smaller MP4s",
                |ui| {
                    widgets::segmented(
                        ui,
                        &mut r.quality,
                        &[
                            (VideoQuality::High, "High"),
                            (VideoQuality::Balanced, "Balanced"),
                            (VideoQuality::Compact, "Compact"),
                        ],
                    );
                },
            );
        }
        if show("frame rate 60 fps 30 fps smooth") {
            sep(ui, &mut any);
            widgets::setting_row(ui, "Frame rate", "60 FPS is smoother but makes larger files", |ui| {
                widgets::segmented(ui, &mut r.fps, &[(60, "60 FPS"), (30, "30 FPS")]);
            });
        }
        if show("countdown timer seconds delay before start") {
            sep(ui, &mut any);
            widgets::setting_row(ui, "Countdown", "Time to get ready before recording starts", |ui| {
                widgets::segmented(ui, &mut r.countdown, &[(5, "5 s"), (3, "3 s"), (0, "Off")]);
            });
        }
        if show("record cursor mouse pointer video") {
            sep(ui, &mut any);
            widgets::setting_row(ui, "Record cursor", "Include the mouse pointer in the video", |ui| {
                widgets::toggle(ui, &mut r.cursor);
            });
        }
        if show("recording outline border area frame") {
            sep(ui, &mut any);
            widgets::setting_row(
                ui,
                "Recording outline",
                "Show a border around the recorded area (never visible in the video)",
                |ui| {
                    widgets::toggle(ui, &mut r.show_outline);
                },
            );
        }

        let show_desktop = show("desktop audio system sound speakers headphones volume gain test sound");
        if show_desktop {
            sep(ui, &mut any);
            let selected_output = r.desktop_device.clone();
            widgets::setting_row(ui, "Desktop audio", "Sound playing on your speakers or headphones", |ui| {
                widgets::toggle(ui, &mut r.desktop_audio);
                ui.add_space(4.0);
                if ui
                    .button("Test sound")
                    .on_hover_text("Play a short chime on the selected output device")
                    .clicked()
                {
                    platform_windows::audio::play_test_sound(selected_output);
                }
            });
            if r.desktop_audio {
                let gain = r.desktop_gain;
                device_row(
                    ui,
                    "desktop-device",
                    &self.outputs,
                    &mut r.desktop_device,
                    &mut r.desktop_gain,
                    "Follow Windows default",
                    |ui| self.desktop.ui(ui, Kind::Desktop, gain),
                );
            }
        }
        let show_mic = show("microphone mic voice input volume gain");
        if show_mic {
            sep(ui, &mut any);
            widgets::setting_row(ui, "Microphone", "Your voice, from the selected input", |ui| {
                widgets::toggle(ui, &mut r.microphone);
            });
            if r.microphone {
                let gain = r.microphone_gain;
                device_row(
                    ui,
                    "mic-device",
                    &self.inputs,
                    &mut r.microphone_device,
                    &mut r.microphone_gain,
                    "Windows default",
                    |ui| self.mic.ui(ui, Kind::Microphone, gain),
                );
            }
        }

        if show("audio check test clip level sync") {
            sep(ui, &mut any);
            self.audio_test_ui(ui, r, recording);
        }

        let busy = recording || self.test_run.is_some();
        if recording && (show_desktop || show_mic) {
            ui.add_space(4.0);
            ui.label(RichText::new("Level meters pause while a recording is running.").size(12.0).color(p.muted));
        }

        // Monitor the chosen devices while this page is open and idle.
        let want = |on: bool, vis: bool, dev: &Option<String>| (on && vis && !busy).then(|| dev.clone());
        self.desktop.sync(Kind::Desktop, want(r.desktop_audio, show_desktop, &r.desktop_device));
        self.mic.sync(Kind::Microphone, want(r.microphone, show_mic, &r.microphone_device));
        if self.desktop.source.is_some() || self.mic.source.is_some() || self.test_run.is_some() {
            ui.ctx().request_repaint_after(Duration::from_millis(50));
        }
    }

    fn audio_test_ui(&mut self, ui: &mut Ui, r: &RecordSettings, recording: bool) {
        let p = palette(ui);
        let any_audio = r.desktop_audio || r.microphone;
        let desc = if !any_audio {
            "Enable Desktop audio or Microphone above to record a quick test clip.".to_string()
        } else if let Some(run) = &self.test_run {
            let left = AUDIO_TEST_SECS.saturating_sub(run.started_at.elapsed().as_secs()).max(1);
            format!("Recording {AUDIO_TEST_SECS} s audio check ({left} s left)… speak or play sound now.")
        } else {
            "Record a 4 s test clip to check device levels and sync before a long session.".to_string()
        };

        let mut start_test = false;
        let mut cancel_test = false;
        widgets::setting_row(ui, "Audio check", &desc, |ui| {
            if self.test_run.is_some() {
                if ui.button("Cancel").clicked() {
                    cancel_test = true;
                }
            } else {
                if let Some(Ok(done)) = &self.last_test
                    && done.path.exists()
                    && ui.button("▶ Play test").clicked()
                {
                    shell::open(&done.path);
                }
                if ui
                    .add_enabled(any_audio && !recording, egui::Button::new("Record 4 s test"))
                    .clicked()
                {
                    start_test = true;
                }
            }
        });

        if cancel_test && let Some(run) = self.test_run.take() {
            run.cancel.store(true, Ordering::Release);
        }
        if start_test {
            self.desktop = Meter::default();
            self.mic = Meter::default();
            self.last_test = None;
            let cancel = Arc::new(AtomicBool::new(false));
            let (tx, rx) = mpsc::channel();
            let cfg = r.clone();
            let cancel2 = cancel.clone();
            let ctx = ui.ctx().clone();
            std::thread::Builder::new()
                .name("audio-test".into())
                .spawn(move || {
                    let res = run_audio_test(cfg, cancel2);
                    let _ = tx.send(res);
                    ctx.request_repaint();
                })
                .expect("spawn audio test thread");
            self.test_run = Some(AudioTestRun { started_at: Instant::now(), cancel, rx });
        }

        if self.test_run.is_none()
            && let Some(result) = &self.last_test
        {
            ui.add_space(4.0);
            match result {
                Ok(done) => {
                    if let Some(warn) = &done.warning {
                        ui.label(RichText::new(format!("Test saved with warning: {warn}")).size(12.0).color(p.danger));
                    } else if done.peak < 0.003 {
                        ui.label(
                            RichText::new("Test clip recorded, but the audio was silent — check your input or volume.")
                                .size(12.0)
                                .color(Color32::from_rgb(245, 158, 11)),
                        );
                    } else {
                        let pct = (done.peak * 100.0).round().clamp(1.0, 100.0) as u32;
                        ui.label(
                            RichText::new(format!("Audio check passed · peak {pct}% · click Play test to listen."))
                                .size(12.0)
                                .color(p.success),
                        );
                    }
                }
                Err(err) => {
                    ui.label(RichText::new(format!("Audio test failed: {err}")).size(12.0).color(p.danger));
                }
            }
        }
    }
}

fn run_audio_test(r: RecordSettings, cancel: Arc<AtomicBool>) -> Result<AudioTestOutcome, String> {
    let mon = monitors::enumerate()
        .into_iter()
        .find(|m| m.primary)
        .or_else(|| monitors::enumerate().into_iter().next())
        .ok_or_else(|| "No display found".to_string())?;
    let b = mon.bounds;
    let area = monitors::PixelRect {
        left: b.left,
        top: b.top,
        right: (b.left + 320).min(b.right),
        bottom: (b.top + 180).min(b.bottom),
    };
    let output = std::env::temp_dir().join("CapShotX_audio_test.mp4");
    let _ = std::fs::remove_file(&output);

    let input = |on: bool, dev: &Option<String>, gain: f32| {
        on.then(|| AudioInput { device_id: dev.clone(), gain: gain.clamp(0.0, 2.0) })
    };
    let rec = Recorder::start(recorder::Settings {
        target: Target::Area(area),
        fps: 30,
        quality: recorder::VideoQuality::Compact,
        cursor: false,
        desktop: input(r.desktop_audio, &r.desktop_device, r.desktop_gain),
        microphone: input(r.microphone, &r.microphone_device, r.microphone_gain),
        output,
    })?;

    let until = Instant::now() + Duration::from_secs(AUDIO_TEST_SECS);
    while Instant::now() < until {
        if cancel.load(Ordering::Acquire) {
            let summary = rec.stop();
            if let Ok(s) = summary {
                let _ = std::fs::remove_file(s.path);
            }
            return Err(String::new());
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    let summary = rec.stop()?;
    let peak = media::decode_audio(&summary.path)
        .ok()
        .map_or(0.0, |(pcm, _, _)| {
            pcm.iter()
                .map(|&s| (s as f32 / 32768.0).abs())
                .fold(0.0f32, f32::max)
        });
    Ok(AudioTestOutcome {
        path: summary.path,
        peak,
        warning: summary.warnings.into_iter().next(),
    })
}

fn device_row(
    ui: &mut Ui,
    id: &str,
    devices: &[AudioDevice],
    selected: &mut Option<String>,
    gain: &mut f32,
    default_label: &str,
    meter: impl FnOnce(&mut Ui),
) {
    let p = palette(ui);
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
            .width(290.0)
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
    ui.horizontal(|ui| {
        ui.add_space(4.0);
        ui.label(RichText::new("Gain").size(12.0).color(p.muted));
        let mut pct = (*gain * 100.0).round() as i32;
        let slider = egui::Slider::new(&mut pct, 0..=200)
            .suffix("%")
            .smart_aim(false);
        if ui.add_sized([180.0, 18.0], slider).changed() {
            *gain = (pct as f32 / 100.0).clamp(0.0, 2.0);
        }
        if (*gain - 1.0).abs() > 0.005
            && ui
                .add(egui::Button::new(RichText::new("100%").size(11.5)).frame(false))
                .on_hover_text("Reset gain to 100%")
                .clicked()
        {
            *gain = 1.0;
        }
    });
    ui.add_space(4.0);
}
