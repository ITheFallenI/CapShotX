//! Screen recording lifecycle: pick an area → countdown → record (with the
//! floating toolbar) → stop → finalise off the UI thread → toast + gallery.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use platform_windows::rec_toolbar::{RecordingToolbar, ToolbarAction, ToolbarStatus};
use platform_windows::toast::{Toast, ToastImage, show_toast};
use recorder::{AudioInput, Recorder, RecorderHandle, Summary, Target};

use crate::commands::AppCommand;
use crate::settings::Settings;

/// A running recording and its toolbar. Moved between threads as a whole.
pub struct Active {
    recorder: Recorder,
    toolbar: RecordingToolbar,
}

impl std::fmt::Debug for Active {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Active recording")
    }
}

enum Phase {
    Idle,
    /// Selecting an area or counting down; set the flag to cancel.
    Starting(Arc<AtomicBool>),
    Recording(Active),
    /// Finalising the file.
    Saving,
}

pub struct ScreenRecorder {
    phase: Phase,
    tx: Sender<AppCommand>,
    ctx: egui::Context,
}

impl ScreenRecorder {
    pub fn new(ctx: egui::Context, tx: Sender<AppCommand>) -> Self {
        Self { phase: Phase::Idle, tx, ctx }
    }

    pub fn is_recording(&self) -> bool {
        matches!(self.phase, Phase::Recording(_))
    }

    /// Busy in any way (selecting, recording or saving).
    pub fn is_busy(&self) -> bool {
        !matches!(self.phase, Phase::Idle)
    }

    /// The hotkey/tray action: start if idle, otherwise stop or cancel.
    pub fn toggle(&mut self, settings: &Settings, dark: bool) {
        match &self.phase {
            Phase::Idle => self.start(settings, dark),
            Phase::Starting(cancel) => cancel.store(true, Ordering::Release),
            Phase::Recording(_) => self.stop(),
            Phase::Saving => {}
        }
    }

    fn start(&mut self, settings: &Settings, dark: bool) {
        let cancel = Arc::new(AtomicBool::new(false));
        self.phase = Phase::Starting(cancel.clone());
        let job = StartJob {
            settings: settings.clone(),
            dark,
            cancel,
            tx: self.tx.clone(),
            ctx: self.ctx.clone(),
        };
        std::thread::Builder::new()
            .name("record-start".into())
            .spawn(move || job.run())
            .expect("spawn recording start thread");
    }

    pub fn on_started(&mut self, result: Result<Active, String>, dark: bool) {
        match result {
            Ok(active) => {
                if let Phase::Starting(cancel) = &self.phase
                    && cancel.load(Ordering::Acquire)
                {
                    // Cancelled while it was starting: save what little there is.
                    self.phase = Phase::Recording(active);
                    self.stop();
                } else {
                    self.phase = Phase::Recording(active);
                }
            }
            Err(msg) => {
                self.phase = Phase::Idle;
                if !msg.is_empty() {
                    tracing::warn!("recording didn't start: {msg}");
                    toast(Toast { title: "Recording didn't start".into(), subtitle: msg, image: None, open_on_click: None, error: true, dark });
                }
            }
        }
    }

    pub fn stop(&mut self) {
        let Phase::Recording(active) = std::mem::replace(&mut self.phase, Phase::Saving) else { return };
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        std::thread::Builder::new()
            .name("record-stop".into())
            .spawn(move || {
                let Active { recorder, toolbar } = active;
                drop(toolbar);
                let result = recorder.stop();
                let _ = tx.send(AppCommand::RecordingFinished(result));
                ctx.request_repaint();
            })
            .expect("spawn recording stop thread");
    }

    pub fn on_finished(&mut self, result: Result<Summary, String>, dark: bool) {
        self.phase = Phase::Idle;
        match result {
            Ok(summary) => {
                for w in &summary.warnings {
                    tracing::warn!("recording: {w}");
                }
                tracing::info!(
                    "recording saved: {} ({:.1} s, {} frames, {} repeated)",
                    summary.path.display(),
                    summary.duration.as_secs_f64(),
                    summary.frames_written,
                    summary.frames_repeated
                );
                let path = summary.path.clone();
                let image = media::poster_frame(&path)
                    .ok()
                    .map(|(width, height, mut px)| {
                        px.chunks_exact_mut(4).for_each(|p| p.swap(0, 2)); // RGBA → BGRA
                        ToastImage { bgra: px, width, height }
                    });
                let (title, subtitle) = match summary.warnings.first() {
                    None => (
                        "Recording saved".to_string(),
                        format!("{} · {}×{} · Click to play", format_duration(summary.duration), summary.width, summary.height),
                    ),
                    Some(first) => ("Recording saved, with problems".to_string(), first.clone()),
                };
                let error = !summary.warnings.is_empty();
                toast(Toast { title, subtitle, image, open_on_click: Some(path), error, dark });
            }
            Err(msg) => {
                tracing::error!("recording failed: {msg}");
                toast(Toast { title: "Recording failed".into(), subtitle: msg, image: None, open_on_click: None, error: true, dark });
            }
        }
    }
}

struct StartJob {
    settings: Settings,
    dark: bool,
    cancel: Arc<AtomicBool>,
    tx: Sender<AppCommand>,
    ctx: egui::Context,
}

impl StartJob {
    fn run(self) {
        let result = self.start();
        let _ = self.tx.send(AppCommand::RecordingStarted(result));
        self.ctx.request_repaint();
    }

    /// `Err("")` = cancelled by the user (no message needed).
    fn start(&self) -> Result<Active, String> {
        let c = &self.settings.capture;
        let r = &self.settings.record;
        let (rect, window) = match capture::select_area(c.magnifier, c.highlight)? {
            capture::Selection::Cancelled => return Err(String::new()),
            capture::Selection::Area { rect, window } => (rect, window),
        };
        let target = window.map_or(Target::Area(rect), Target::Window);
        let output = output_path(&self.settings)?;

        // The toolbar shows the countdown, then the live recording.
        let handle: Arc<Mutex<Option<RecorderHandle>>> = Arc::default();
        let starts_at = Instant::now() + Duration::from_secs(r.countdown as u64);
        let has_audio = r.desktop_audio || r.microphone;
        let toolbar = {
            let (handle, handle2) = (handle.clone(), handle.clone());
            let (cancel, tx, ctx) = (self.cancel.clone(), self.tx.clone(), self.ctx.clone());
            let centre = ((rect.left + rect.right) / 2, (rect.top + rect.bottom) / 2);
            RecordingToolbar::show(
                centre,
                self.dark,
                move || status(&handle.lock().unwrap(), starts_at, has_audio),
                move |action| match action {
                    ToolbarAction::TogglePause => {
                        if let Some(h) = &*handle2.lock().unwrap() {
                            if h.is_paused() { h.resume() } else { h.pause() }
                        }
                    }
                    ToolbarAction::Stop => {
                        cancel.store(true, Ordering::Release);
                        let _ = tx.send(AppCommand::RecordingStop);
                        ctx.request_repaint();
                    }
                },
            )
            .map_err(|e| format!("Couldn't show the recording controls: {}", e.message()))?
        };

        while Instant::now() < starts_at {
            if self.cancel.load(Ordering::Acquire) {
                return Err(String::new());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        if self.cancel.load(Ordering::Acquire) {
            return Err(String::new());
        }

        let input = |on: bool, device: &Option<String>| on.then(|| AudioInput { device_id: device.clone(), gain: 1.0 });
        let recorder = Recorder::start(recorder::Settings {
            target,
            fps: r.fps,
            cursor: r.cursor,
            desktop: input(r.desktop_audio, &r.desktop_device),
            microphone: input(r.microphone, &r.microphone_device),
            output,
        })?;
        *handle.lock().unwrap() = Some(recorder.handle());
        Ok(Active { recorder, toolbar })
    }
}

fn status(handle: &Option<RecorderHandle>, starts_at: Instant, has_audio: bool) -> ToolbarStatus {
    match handle {
        None => {
            let left = starts_at.saturating_duration_since(Instant::now());
            ToolbarStatus {
                countdown: Some(left.as_secs_f32().ceil().max(1.0) as u32),
                ..Default::default()
            }
        }
        Some(h) => ToolbarStatus {
            elapsed: h.elapsed(),
            paused: h.is_paused(),
            level: has_audio.then(|| h.peak()),
            warning: h.warning(),
            countdown: None,
        },
    }
}

fn output_path(settings: &Settings) -> Result<PathBuf, String> {
    let now = platform_windows::shell::local_time();
    let relative = capture::render_template(&settings.capture.file_template, now)?;
    let mut path = settings.capture.save_folder().join(relative);
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".mp4");
    path.set_file_name(name);
    Ok(path)
}

pub fn format_duration(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 3600 { format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60) } else { format!("{}:{:02}", s / 60, s % 60) }
}

fn toast(t: Toast) {
    std::thread::spawn(move || {
        if let Err(e) = show_toast(t) {
            tracing::warn!("toast failed: {e}");
        }
    });
}
