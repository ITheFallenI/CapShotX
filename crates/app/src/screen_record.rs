//! Screen recording lifecycle: pick initial bounds → (optional Camtasia-style
//! live bounds & audio staging) → countdown → record (with floating toolbar)
//! → stop → finalise off the UI thread → toast + gallery.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use platform_windows::rec_outline::{OutlinePhase, OutlineStatus, RecordingOutline};
use platform_windows::rec_staging::{
    SIGNAL_CANCEL, SIGNAL_NONE, SIGNAL_START, StagingConfig, StagingLevels, StagingOutcome,
    run_staging,
};
use platform_windows::rec_toolbar::{RecordingToolbar, ToolbarAction, ToolbarStatus};
use platform_windows::toast::{Toast, ToastImage, show_toast};
use recorder::{AudioInput, AudioSource, Kind, Recorder, RecorderHandle, Summary, Target};

use crate::commands::{AppCommand, Waker};
use crate::settings::{RecordStartMode, Settings};

/// A running recording and its floating chrome. Moved between threads as a whole.
pub struct Active {
    recorder: Recorder,
    toolbar: RecordingToolbar,
    outline: Option<RecordingOutline>,
    show_outline: Arc<AtomicBool>,
}

impl std::fmt::Debug for Active {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Active recording")
    }
}

struct StartingControl {
    cancel: Arc<AtomicBool>,
    staging_signal: Arc<AtomicU8>,
    in_staging: Arc<AtomicBool>,
    show_outline: Arc<AtomicBool>,
    restore_on_cancel: bool,
}

enum Phase {
    Idle,
    /// Selecting bounds, adjusting in staging, or counting down.
    Starting(StartingControl),
    Recording(Active),
    /// Finalising the file.
    Saving,
}

pub struct ScreenRecorder {
    phase: Phase,
    waker: Waker,
}

impl ScreenRecorder {
    pub fn new(waker: Waker) -> Self {
        Self { phase: Phase::Idle, waker }
    }

    pub fn is_recording(&self) -> bool {
        matches!(self.phase, Phase::Recording(_))
    }

    /// Busy in any way (selecting, staging, recording or saving).
    pub fn is_busy(&self) -> bool {
        !matches!(self.phase, Phase::Idle)
    }

    /// The hotkey action: start if idle (using configured default mode), start
    /// recording if currently adjusting bounds in staging, otherwise stop/cancel.
    pub fn toggle(&mut self, settings: &Settings, dark: bool) {
        let fast = settings.record.start_mode == RecordStartMode::Fast;
        self.start_or_toggle(settings, dark, fast, false);
    }

    /// Starts with an explicit mode (`fast = false` for Camtasia bounds
    /// adjustment, `fast = true` for immediate start), or toggles if active.
    pub fn start_or_toggle(&mut self, settings: &Settings, dark: bool, fast: bool, from_app: bool) {
        match &self.phase {
            Phase::Idle => self.start(settings, dark, fast, from_app),
            Phase::Starting(ctrl) => {
                if ctrl.in_staging.load(Ordering::Acquire) {
                    ctrl.staging_signal.store(SIGNAL_START, Ordering::Release);
                } else {
                    ctrl.cancel.store(true, Ordering::Release);
                    ctrl.staging_signal.store(SIGNAL_CANCEL, Ordering::Release);
                }
            }
            Phase::Recording(_) => self.stop(),
            Phase::Saving => {}
        }
    }

    pub fn toggle_pause(&self) {
        if let Phase::Recording(active) = &self.phase {
            if active.recorder.is_paused() {
                active.recorder.resume();
            } else {
                active.recorder.pause();
            }
        }
    }

    pub fn cancel_starting(&mut self) {
        if let Phase::Starting(ctrl) = &self.phase {
            ctrl.cancel.store(true, Ordering::Release);
            ctrl.staging_signal.store(SIGNAL_CANCEL, Ordering::Release);
        }
    }

    pub fn set_show_outline(&self, show: bool) {
        match &self.phase {
            Phase::Starting(ctrl) => ctrl.show_outline.store(show, Ordering::Release),
            Phase::Recording(active) => active.show_outline.store(show, Ordering::Release),
            Phase::Idle | Phase::Saving => {}
        }
    }

    fn start(&mut self, settings: &Settings, dark: bool, fast: bool, restore_on_cancel: bool) {
        let cancel = Arc::new(AtomicBool::new(false));
        let staging_signal = Arc::new(AtomicU8::new(SIGNAL_NONE));
        let in_staging = Arc::new(AtomicBool::new(false));
        let show_outline = Arc::new(AtomicBool::new(settings.record.show_outline));
        self.phase = Phase::Starting(StartingControl {
            cancel: cancel.clone(),
            staging_signal: staging_signal.clone(),
            in_staging: in_staging.clone(),
            show_outline: show_outline.clone(),
            restore_on_cancel,
        });
        let job = StartJob {
            settings: settings.clone(),
            dark,
            fast,
            cancel,
            staging_signal,
            in_staging,
            show_outline,
            waker: self.waker.clone(),
        };
        std::thread::Builder::new()
            .name("record-start".into())
            .spawn(move || job.run())
            .expect("spawn recording start thread");
    }

    /// Returns `true` if the main window was hidden for this start and should
    /// be shown again because the user cancelled (or start failed).
    pub fn on_started(&mut self, result: Result<Active, String>, dark: bool) -> bool {
        let restore = matches!(&self.phase, Phase::Starting(c) if c.restore_on_cancel);
        match result {
            Ok(active) => {
                if let Phase::Starting(ctrl) = &self.phase
                    && ctrl.cancel.load(Ordering::Acquire)
                {
                    // Cancelled while it was starting: save what little there is.
                    self.phase = Phase::Recording(active);
                    self.stop();
                } else {
                    self.phase = Phase::Recording(active);
                }
                false
            }
            Err(msg) => {
                self.phase = Phase::Idle;
                if !msg.is_empty() {
                    tracing::warn!("recording didn't start: {msg}");
                    toast(Toast { title: "Recording didn't start".into(), subtitle: msg, image: None, open_on_click: None, error: true, dark });
                }
                restore
            }
        }
    }

    pub fn stop(&mut self) {
        let Phase::Recording(active) = std::mem::replace(&mut self.phase, Phase::Saving) else { return };
        let waker = self.waker.clone();
        std::thread::Builder::new()
            .name("record-stop".into())
            .spawn(move || {
                let Active { recorder, toolbar, outline, .. } = active;
                drop(toolbar);
                drop(outline);
                let result = recorder.stop();
                waker.send(AppCommand::RecordingFinished(result));
            })
            .expect("spawn recording stop thread");
    }

    pub fn on_finished(&mut self, result: Result<Summary, String>, dark: bool) -> Option<PathBuf> {
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
                let image = poster_toast_image(&path);
                let (title, subtitle) = match summary.warnings.first() {
                    None => (
                        "Recording saved".to_string(),
                        format!("{} · {}×{} · Click to play", format_duration(summary.duration), summary.width, summary.height),
                    ),
                    Some(first) => ("Recording saved, with problems".to_string(), first.clone()),
                };
                let error = !summary.warnings.is_empty();
                toast(Toast { title, subtitle, image, open_on_click: Some(path.clone()), error, dark });
                Some(path)
            }
            Err(msg) => {
                tracing::error!("recording failed: {msg}");
                toast(Toast { title: "Recording failed".into(), subtitle: msg, image: None, open_on_click: None, error: true, dark });
                None
            }
        }
    }
}

struct StartJob {
    settings: Settings,
    dark: bool,
    fast: bool,
    cancel: Arc<AtomicBool>,
    staging_signal: Arc<AtomicU8>,
    in_staging: Arc<AtomicBool>,
    show_outline: Arc<AtomicBool>,
    waker: Waker,
}

impl StartJob {
    fn run(self) {
        let result = self.start();
        self.waker.send(AppCommand::RecordingStarted(result));
    }

    /// `Err("")` = cancelled by the user (no message needed).
    fn start(&self) -> Result<Active, String> {
        let c = &self.settings.capture;
        let r = &self.settings.record;
        let mut desktop_audio = r.desktop_audio;
        let mut microphone = r.microphone;

        let (rect, window) = loop {
            if self.cancel.load(Ordering::Acquire) {
                return Err(String::new());
            }
            let (rect, window, window_title) = match capture::select_area(c.magnifier, r.highlight, r.border_width)? {
                capture::Selection::Cancelled => return Err(String::new()),
                capture::Selection::Area { rect, window, window_title } => (rect, window, window_title),
            };
            if self.fast {
                break (rect, window);
            }

            // Camtasia-style bounds staging: live screen, resizable handles,
            // size presets and live audio toggles/meters before recording starts.
            self.staging_signal.store(SIGNAL_NONE, Ordering::Release);
            self.in_staging.store(true, Ordering::Release);
            let outcome = {
                let mut desk_mon: Option<Result<AudioSource, ()>> = None;
                let mut mic_mon: Option<Result<AudioSource, ()>> = None;
                let desk_dev = r.desktop_device.clone();
                let mic_dev = r.microphone_device.clone();
                run_staging(
                    StagingConfig {
                        rect,
                        window,
                        window_title,
                        highlight: r.highlight,
                        border_width: r.border_width,
                        dark: self.dark,
                        desktop_audio,
                        microphone,
                    },
                    self.staging_signal.clone(),
                    |want_desk, want_mic| {
                        if !want_desk {
                            desk_mon = None;
                        } else if desk_mon.is_none() {
                            desk_mon = Some(AudioSource::monitor(Kind::Desktop, desk_dev.clone()).map_err(|_| ()));
                        }
                        if !want_mic {
                            mic_mon = None;
                        } else if mic_mon.is_none() {
                            mic_mon = Some(AudioSource::monitor(Kind::Microphone, mic_dev.clone()).map_err(|_| ()));
                        }
                        let (desktop_peak, desktop_err) = match &desk_mon {
                            Some(Ok(s)) if s.status.is_connected() => (Some(s.status.levels().0), false),
                            Some(_) => (None, true),
                            None => (None, false),
                        };
                        let (mic_peak, mic_err) = match &mic_mon {
                            Some(Ok(s)) if s.status.is_connected() => (Some(s.status.levels().0), false),
                            Some(_) => (None, true),
                            None => (None, false),
                        };
                        StagingLevels { desktop_peak, desktop_err, mic_peak, mic_err }
                    },
                )
            };
            self.in_staging.store(false, Ordering::Release);

            match outcome.map_err(|e| format!("Couldn't show the bounds controls: {}", e.message()))? {
                StagingOutcome::Cancelled => return Err(String::new()),
                StagingOutcome::Reselect { desktop_audio: d, microphone: m } => {
                    desktop_audio = d;
                    microphone = m;
                }
                StagingOutcome::Start { rect, window, desktop_audio: d, microphone: m } => {
                    desktop_audio = d;
                    microphone = m;
                    if desktop_audio != r.desktop_audio || microphone != r.microphone {
                        self.waker.send(AppCommand::RecordSyncAudio { desktop_audio, microphone });
                    }
                    break (rect, window);
                }
            }
        };

        let target = window.map_or(Target::Area(rect), Target::Window);
        let output = output_path(&self.settings)?;

        // The outline and toolbar show the countdown, then the live recording.
        let handle: Arc<Mutex<Option<RecorderHandle>>> = Arc::default();
        let starts_at = Instant::now() + Duration::from_secs(r.countdown as u64);
        let has_audio = desktop_audio || microphone;
        let outline = {
            let handle = handle.clone();
            let visible = self.show_outline.clone();
            match RecordingOutline::show(rect, window, r.highlight, r.border_width, move || {
                let phase = match &*handle.lock().unwrap() {
                    None => OutlinePhase::Countdown,
                    Some(h) if h.is_paused() => OutlinePhase::Paused,
                    Some(_) => OutlinePhase::Recording,
                };
                OutlineStatus { phase, visible: visible.load(Ordering::Acquire) }
            }) {
                Ok(o) => Some(o),
                Err(e) => {
                    tracing::warn!("couldn't show recording outline: {e}");
                    None
                }
            }
        };
        let toolbar = {
            let (handle, handle2) = (handle.clone(), handle.clone());
            let (cancel, waker) = (self.cancel.clone(), self.waker.clone());
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
                    ToolbarAction::ToggleMute => {
                        if let Some(h) = &*handle2.lock().unwrap() {
                            h.set_muted(!h.is_muted());
                        }
                    }
                    ToolbarAction::Stop => {
                        cancel.store(true, Ordering::Release);
                        waker.send(AppCommand::RecordingStop);
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

        let input = |on: bool, device: &Option<String>, gain: f32| {
            on.then(|| AudioInput { device_id: device.clone(), gain: gain.clamp(0.0, 2.0) })
        };
        let recorder = Recorder::start(recorder::Settings {
            target,
            fps: r.fps,
            quality: r.quality.into(),
            cursor: r.cursor,
            desktop: input(desktop_audio, &r.desktop_device, r.desktop_gain),
            microphone: input(microphone, &r.microphone_device, r.microphone_gain),
            output,
        })?;
        *handle.lock().unwrap() = Some(recorder.handle());
        Ok(Active {
            recorder,
            toolbar,
            outline,
            show_outline: self.show_outline.clone(),
        })
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
            muted: h.is_muted(),
            level: has_audio.then(|| h.peak()),
            warning: h.warning(),
            countdown: None,
        },
    }
}

fn output_path(settings: &Settings) -> Result<PathBuf, String> {
    let now = platform_windows::shell::local_time();
    let relative = capture::render_output_path(
        &settings.capture.file_template,
        capture::OutputKind::Video,
        now,
    )?;
    Ok(settings.capture.save_folder().join(relative))
}

pub fn notify_recovered(paths: Vec<PathBuf>, dark: bool) {
    let cache_dir = Settings::thumbnail_cache_dir();
    for p in paths.iter().skip(1) {
        let _ = gallery::warm_cache(p, cache_dir.as_deref());
    }
    let Some(first) = paths.first().cloned() else { return };
    let title = if paths.len() == 1 {
        "Recovered interrupted recording".to_string()
    } else {
        format!("Recovered {} interrupted recordings", paths.len())
    };
    let subtitle = first
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let image = poster_toast_image(&first);
    toast(Toast {
        title,
        subtitle: format!("{subtitle} · Click to play"),
        image,
        open_on_click: Some(first),
        error: false,
        dark,
    });
}

fn poster_toast_image(path: &std::path::Path) -> Option<ToastImage> {
    let cache_dir = Settings::thumbnail_cache_dir();
    gallery::warm_cache(path, cache_dir.as_deref())
        .or_else(|| media::poster_frame(path).ok())
        .map(|(width, height, mut px)| {
            px.chunks_exact_mut(4).for_each(|p| p.swap(0, 2)); // RGBA → BGRA
            ToastImage { bgra: px, width, height }
        })
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
