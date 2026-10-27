//! A recording session: video + audio sources on one clock, encoded live to
//! `<name>.mp4.partial` and renamed on a successful finish (REC-03).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::Duration;

use media::{AudioConfig, Mp4Writer, TICKS, VideoConfig};
use platform_windows::clock;

use crate::audio::{AudioSource, Kind, Sink, SourceStatus};
use crate::clock::SessionClock;
use crate::mixer::{Mixer, RATE};
use crate::video::{Target, VideoSource};

/// Audio still in flight is allowed this long before it's mixed.
const AUDIO_LATENCY: i64 = TICKS / 5;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum VideoQuality {
    Compact,
    #[default]
    Balanced,
    High,
}

#[derive(Clone, Debug)]
pub struct AudioInput {
    /// `None` = system default.
    pub device_id: Option<String>,
    pub gain: f32,
}

#[derive(Clone, Debug)]
pub struct Settings {
    pub target: Target,
    pub fps: u32,
    pub quality: VideoQuality,
    pub cursor: bool,
    pub desktop: Option<AudioInput>,
    pub microphone: Option<AudioInput>,
    /// Final path of the `.mp4`.
    pub output: PathBuf,
}

#[derive(Clone, Debug)]
pub struct Summary {
    pub path: PathBuf,
    pub duration: Duration,
    pub width: u32,
    pub height: u32,
    pub frames_written: u64,
    /// Frames written again because the screen didn't change or capture lagged.
    pub frames_repeated: u64,
    /// What went wrong along the way (device loss, etc.); empty if nothing.
    pub warnings: Vec<String>,
}

#[derive(Default)]
struct Counters {
    written: AtomicU64,
    repeated: AtomicU64,
}

/// Live view and pause control of a running recording; cheap to clone and
/// usable from any thread (e.g. the control toolbar).
#[derive(Clone)]
pub struct RecorderHandle {
    clock: Arc<Mutex<Option<SessionClock>>>,
    audio: Vec<(Kind, Arc<SourceStatus>)>,
    mixer: Arc<Mutex<Mixer>>,
    muted: Arc<AtomicBool>,
    counters: Arc<Counters>,
    video_lost: Arc<AtomicBool>,
}

pub struct Recorder {
    handle: RecorderHandle,
    sources: Vec<AudioSource>,
    finish: mpsc::Sender<i64>,
    encoder: Option<JoinHandle<Result<Summary, String>>>,
}

impl std::ops::Deref for Recorder {
    type Target = RecorderHandle;

    fn deref(&self) -> &RecorderHandle {
        &self.handle
    }
}

impl Recorder {
    /// Starts recording. Fails — never silently records without them — if
    /// a requested audio device or the video source can't start.
    pub fn start(settings: Settings) -> Result<Self, String> {
        let clock_slot = Arc::new(Mutex::new(None));
        let inputs: Vec<(Kind, AudioInput)> = [
            settings.desktop.clone().map(|a| (Kind::Desktop, a)),
            settings.microphone.clone().map(|a| (Kind::Microphone, a)),
        ]
        .into_iter()
        .flatten()
        .collect();
        let mixer = Arc::new(Mutex::new(Mixer::new(inputs.len())));
        let mut sources = Vec::new();
        for (track, (kind, input)) in inputs.iter().enumerate() {
            mixer.lock().unwrap().set_gain(track, input.gain);
            let sink = Sink { mixer: mixer.clone(), track, clock: clock_slot.clone() };
            sources.push(AudioSource::start(*kind, input.device_id.clone(), Some(sink))?);
        }

        let (started_tx, started_rx) = mpsc::channel();
        let (finish_tx, finish_rx) = mpsc::channel();
        let counters = Arc::new(Counters::default());
        let video_lost = Arc::new(AtomicBool::new(false));
        let encoder = {
            let ctx = EncoderCtx {
                settings: settings.clone(),
                has_audio: !sources.is_empty(),
                clock: clock_slot.clone(),
                mixer: mixer.clone(),
                counters: counters.clone(),
                video_lost: video_lost.clone(),
            };
            std::thread::Builder::new()
                .name("recorder-encode".into())
                .spawn(move || encode(ctx, started_tx, finish_rx))
                .map_err(|e| e.to_string())?
        };
        match started_rx.recv() {
            Ok(Ok(())) => {}
            Ok(Err(msg)) => {
                let _ = encoder.join();
                return Err(msg);
            }
            Err(_) => return Err(encoder.join().ok().and_then(|r| r.err()).unwrap_or_else(|| "Recording failed to start.".into())),
        }
        let handle = RecorderHandle {
            clock: clock_slot,
            audio: sources.iter().map(|s| (s.kind, s.status.clone())).collect(),
            mixer,
            muted: Arc::new(AtomicBool::new(false)),
            counters,
            video_lost,
        };
        Ok(Self { handle, sources, finish: finish_tx, encoder: Some(encoder) })
    }

    pub fn handle(&self) -> RecorderHandle {
        self.handle.clone()
    }

}

impl RecorderHandle {
    pub fn pause(&self) {
        if let Some(c) = self.clock.lock().unwrap().as_mut() {
            c.pause(clock::now());
        }
    }

    pub fn resume(&self) {
        if let Some(c) = self.clock.lock().unwrap().as_mut() {
            c.resume(clock::now());
        }
    }

    pub fn is_paused(&self) -> bool {
        self.clock.lock().unwrap().as_ref().is_some_and(|c| c.is_paused())
    }

    /// Recording time so far (excludes pauses).
    pub fn elapsed(&self) -> Duration {
        let t = self.clock.lock().unwrap().as_ref().map_or(0, |c| c.elapsed(clock::now()));
        Duration::from_nanos((t.max(0) * 100) as u64)
    }

    pub fn audio(&self) -> impl Iterator<Item = (Kind, &SourceStatus)> {
        self.audio.iter().map(|(k, s)| (*k, &**s))
    }

    /// Loudest current peak across sources (0..=1), for a single meter.
    pub fn peak(&self) -> f32 {
        if self.is_muted() {
            return 0.0;
        }
        self.audio.iter().map(|(_, s)| s.levels().0).fold(0.0, f32::max)
    }

    /// Mutes or unmutes all recorded audio tracks while preserving each
    /// track's configured gain.
    pub fn set_muted(&self, muted: bool) {
        self.muted.store(muted, Ordering::Release);
        self.mixer.lock().unwrap().set_muted(muted);
    }

    pub fn is_muted(&self) -> bool {
        self.muted.load(Ordering::Acquire)
    }

    /// Something needs the user's attention (device lost, window closed).
    pub fn warning(&self) -> Option<String> {
        if self.video_lost() {
            return Some("The recorded window was closed.".into());
        }
        self.audio.iter().find_map(|(_, s)| (!s.is_connected()).then(|| s.warnings().pop()).flatten())
    }

    pub fn set_gain(&self, kind: Kind, gain: f32) {
        if let Some(track) = self.audio.iter().position(|(k, _)| *k == kind) {
            self.mixer.lock().unwrap().set_gain(track, gain);
        }
    }

    pub fn frames_repeated(&self) -> u64 {
        self.counters.repeated.load(Ordering::Relaxed)
    }

    /// The recorded window was closed; the recording keeps its last image.
    pub fn video_lost(&self) -> bool {
        self.video_lost.load(Ordering::Relaxed)
    }

}

impl Recorder {
    /// Stops, finalises the MP4 and returns what was recorded. Correct when
    /// called while paused: the paused interval is never included.
    pub fn stop(mut self) -> Result<Summary, String> {
        // Freeze the timeline first so everything after "now" is excluded.
        let end = {
            let mut c = self.handle.clock.lock().unwrap();
            let c = c.as_mut().expect("clock set once started");
            let now = clock::now();
            c.pause(now);
            c.elapsed(now)
        };
        let mut warnings = Vec::new();
        for s in self.sources.drain(..) {
            warnings.extend(s.status.warnings());
            if s.status.packets() == 0 && s.kind == Kind::Microphone {
                warnings.push(format!("Microphone \"{}\" delivered no audio.", s.status.device_name()));
            }
            s.stop();
        }
        let _ = self.finish.send(end);
        let mut summary = self.encoder.take().expect("encoder runs until stop").join().map_err(|_| "The encoder crashed.".to_string())??;
        warnings.append(&mut summary.warnings);
        summary.warnings = warnings;
        Ok(summary)
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        // Dropped without stop(): still finalise rather than leak a thread.
        if let Some(encoder) = self.encoder.take() {
            let end = self.handle.clock.lock().unwrap().as_mut().map_or(0, |c| {
                let now = clock::now();
                c.pause(now);
                c.elapsed(now)
            });
            self.sources.clear();
            let _ = self.finish.send(end);
            let _ = encoder.join();
        }
    }
}

struct EncoderCtx {
    settings: Settings,
    has_audio: bool,
    clock: Arc<Mutex<Option<SessionClock>>>,
    mixer: Arc<Mutex<Mixer>>,
    counters: Arc<Counters>,
    video_lost: Arc<AtomicBool>,
}

pub fn partial_path(output: &Path) -> PathBuf {
    let mut name = output.file_name().unwrap_or_default().to_os_string();
    name.push(".partial");
    output.with_file_name(name)
}

/// Video bitrate scaled by resolution, frame rate and chosen quality preset.
fn bitrate(width: u32, height: u32, fps: u32, quality: VideoQuality) -> u32 {
    let (bpp, min, max) = match quality {
        VideoQuality::Compact => (if fps > 30 { 0.042 } else { 0.06 }, 1_200_000, 35_000_000),
        VideoQuality::Balanced => (if fps > 30 { 0.07 } else { 0.10 }, 2_000_000, 60_000_000),
        VideoQuality::High => (if fps > 30 { 0.12 } else { 0.18 }, 3_500_000, 90_000_000),
    };
    ((width as f64 * height as f64 * fps as f64 * bpp) as u32).clamp(min, max)
}

fn encode(ctx: EncoderCtx, started: mpsc::Sender<Result<(), String>>, finish: mpsc::Receiver<i64>) -> Result<Summary, String> {
    let fail = |msg: String| {
        let _ = started.send(Err(msg.clone()));
        msg
    };
    let s = &ctx.settings;
    let video = VideoSource::start(s.target, s.cursor).map_err(|e| fail(format!("Couldn't start screen capture: {}", e.message())))?;
    let (width, height, fps) = (video.width, video.height, s.fps.clamp(1, 120));
    let partial = partial_path(&s.output);
    if let Some(dir) = partial.parent() {
        std::fs::create_dir_all(dir).map_err(|e| fail(format!("Couldn't create {}: {e}", dir.display())))?;
    }
    let audio_cfg = ctx.has_audio.then(|| {
        let mut cfg = AudioConfig::default();
        if s.quality == VideoQuality::Compact {
            cfg.bitrate = 128_000;
        }
        cfg
    });
    let mut writer = Mp4Writer::create(
        &partial,
        VideoConfig { width, height, fps, bitrate: bitrate(width, height, fps, s.quality) },
        audio_cfg,
    )
    .map_err(|e| fail(format!("Couldn't create the video file: {}", e.message())))?;

    // Give WGC a moment to deliver the first real frame (avoids a black start).
    let wait_until = std::time::Instant::now() + Duration::from_millis(500);
    let mut frame = Vec::new();
    let mut seen = 0;
    while seen == 0 && std::time::Instant::now() < wait_until {
        seen = video.latest_into(&mut frame, 0);
        std::thread::sleep(Duration::from_millis(5));
    }
    if seen != 0 {
        seen = video.latest_into(&mut frame, 0);
    } else {
        frame = vec![0; (width * height * 4) as usize];
    }

    *ctx.clock.lock().unwrap() = Some(SessionClock::new(clock::now()));
    let _ = started.send(Ok(()));
    tracing::info!(width, height, fps, path = %partial.display(), "recording started");

    let frame_dur = TICKS / fps as i64;
    let mut next_frame: i64 = 0;
    let mut warnings = Vec::new();
    let mut result: Result<(), String> = Ok(());
    let end = loop {
        let stop_at = finish
            .try_recv()
            .ok()
            .map(|e| e.max(frame_dur).max(next_frame * frame_dur));
        let now = stop_at.unwrap_or_else(|| ctx.clock.lock().unwrap().as_ref().unwrap().elapsed(clock::now()));

        if video.is_closed() && !ctx.video_lost.swap(true, Ordering::Relaxed) {
            warnings.push(format!("The recorded window closed at {}; the rest shows its last image.", mmss(now)));
        }
        if let Some(err) = video.take_error() {
            tracing::warn!("capture: {err}");
        }

        // Video: one frame per slot up to "now", repeating the latest image
        // when nothing changed. If far behind, one long frame covers the gap.
        let due = if stop_at.is_some() {
            // Cover exactly [0, end): every slot starting before `end`.
            (now + frame_dur - 1) / frame_dur - next_frame
        } else {
            now / frame_dur + 1 - next_frame
        }
        .max(0);
        if due > 0 && result.is_ok() {
            let newer = video.latest_into(&mut frame, seen);
            let repeated = (due - 1) as u64 + u64::from(newer == seen);
            seen = newer;
            let (count, dur) = if due > 2 { (1, frame_dur * due) } else { (due, frame_dur) };
            for i in 0..count {
                let t = (next_frame + i) * frame_dur;
                // The final frame ends exactly at the session end.
                let d = if stop_at.is_some() { dur.min(now - t) } else { dur };
                if let Err(e) = writer.write_video(&frame, t, d) {
                    result = Err(format!("Video encoding failed at {}: {}", mmss(t), e.message()));
                    break;
                }
                ctx.counters.written.fetch_add(1, Ordering::Relaxed);
            }
            if due > 2 {
                tracing::debug!(due, "encoder behind; covering slots with one frame");
            }
            ctx.counters.repeated.fetch_add(repeated, Ordering::Relaxed);
            next_frame += due;
        }

        // Audio: mix what can no longer change.
        if ctx.has_audio && result.is_ok() {
            let until = if stop_at.is_some() { now } else { now - AUDIO_LATENCY };
            let mut mixer = ctx.mixer.lock().unwrap();
            let end_frame = until.max(0) * RATE as i64 / TICKS;
            if end_frame - mixer.written() >= 1024 || stop_at.is_some() {
                let at = mixer.written() * TICKS / RATE as i64;
                let pcm = mixer.mix_until(end_frame);
                drop(mixer);
                if let Err(e) = writer.write_audio(&pcm, at) {
                    result = Err(format!("Audio encoding failed at {}: {}", mmss(at), e.message()));
                }
            }
        }

        if let Some(end) = stop_at {
            break end;
        }
        std::thread::sleep(Duration::from_millis(4));
    };
    drop(video);

    let finished = writer.finish().map_err(|e| format!("Couldn't finalise the video: {}", e.message()));
    let result = result.and(finished);
    if let Err(msg) = result {
        // Keep the .partial: fragmented MP4 is playable up to the failure.
        return Err(format!("{msg}\nWhat was recorded is kept in {}", partial.display()));
    }
    let path = unique(&s.output);
    std::fs::rename(&partial, &path).map_err(|e| format!("Couldn't save {}: {e} (recording kept as {})", path.display(), partial.display()))?;
    tracing::info!(path = %path.display(), seconds = end as f64 / TICKS as f64, "recording saved");
    Ok(Summary {
        path,
        duration: Duration::from_nanos((end * 100) as u64),
        width,
        height,
        frames_written: ctx.counters.written.load(Ordering::Relaxed),
        frames_repeated: ctx.counters.repeated.load(Ordering::Relaxed),
        warnings,
    })
}

/// `path`, or `path (2)`… if it already exists — never overwrite.
fn unique(path: &Path) -> PathBuf {
    if !path.exists() {
        return path.to_path_buf();
    }
    let stem = path.file_stem().unwrap_or_default().to_string_lossy().into_owned();
    let ext = path.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    (2..)
        .map(|n| path.with_file_name(format!("{stem} ({n}){ext}")))
        .find(|p| !p.exists())
        .expect("some free name")
}

fn mmss(t: i64) -> String {
    let s = t.max(0) / TICKS;
    format!("{}:{:02}", s / 60, s % 60)
}

/// Scans `folder` (up to depth 6) for interrupted `*.mp4.partial` recordings
/// left behind by a crash or power loss. Playable fragmented MP4s are renamed
/// to `<stem> (recovered).mp4`; unreadable or empty partials are left on disk
/// untouched (never deleted before verification, spec §9.5).
pub fn recover_partials(folder: &Path) -> Vec<PathBuf> {
    let mut partials = Vec::new();
    collect_partials(folder, 0, &mut partials);
    let mut recovered = Vec::new();
    for partial in partials {
        let Ok(meta) = std::fs::metadata(&partial) else { continue };
        if meta.len() == 0 {
            continue;
        }
        let Some(name) = partial.file_name().and_then(|n| n.to_str()) else { continue };
        let Some(mp4_name) = strip_partial_suffix(name) else { continue };
        let base = partial.with_file_name(mp4_name);
        let stem = base.file_stem().unwrap_or_default().to_string_lossy();
        let candidate = unique(&partial.with_file_name(format!("{stem} (recovered).mp4")));
        if std::fs::rename(&partial, &candidate).is_err() {
            // Likely still open by an active writer; leave alone.
            continue;
        }
        let playable = media::probe(&candidate)
            .ok()
            .filter(|info| info.has_video && info.width > 0 && info.height > 0)
            .and_then(|_| media::poster_frame(&candidate).ok())
            .is_some();
        if playable {
            tracing::info!(path = %candidate.display(), "recovered interrupted recording");
            recovered.push(candidate);
        } else {
            tracing::warn!(path = %partial.display(), "unreadable .partial left untouched");
            let _ = std::fs::rename(&candidate, &partial);
        }
    }
    recovered
}

fn strip_partial_suffix(name: &str) -> Option<&str> {
    if name.len() > ".mp4.partial".len() && name[name.len() - 8..].eq_ignore_ascii_case(".partial") {
        let trimmed = &name[..name.len() - 8];
        if trimmed.len() > 4 && trimmed[trimmed.len() - 4..].eq_ignore_ascii_case(".mp4") {
            return Some(trimmed);
        }
    }
    None
}

fn collect_partials(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth > 6 {
        return;
    }
    let Ok(read) = std::fs::read_dir(dir) else { return };
    for entry in read.flatten() {
        let Ok(ft) = entry.file_type() else { continue };
        let path = entry.path();
        if ft.is_dir() {
            collect_partials(&path, depth + 1, out);
        } else if ft.is_file()
            && path
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(strip_partial_suffix)
                .is_some()
        {
            out.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovers_unfinalised_fragmented_partial_and_keeps_corrupt_partial() {
        let dir = std::env::temp_dir().join(format!("capshotx-recover-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let sub = dir.join("2026-10").join("Video");
        std::fs::create_dir_all(&sub).unwrap();

        let partial = sub.join("CapShotX_crash.mp4.partial");
        {
            let mut w = Mp4Writer::create(
                &partial,
                VideoConfig { width: 320, height: 240, fps: 30, bitrate: 1_500_000 },
                None,
            )
            .expect("create writer");
            assert!(w.fragmented, "Windows 11 should provide fragmented MP4");
            let frame_dur = TICKS / 30;
            let mut bgra = vec![0u8; 320 * 240 * 4];
            for px in bgra.chunks_exact_mut(4) {
                px.copy_from_slice(&[40, 180, 90, 255]);
            }
            // Write 3 s of video (>1 keyframe interval) so fMP4 flushes a fragment,
            // give async hardware MFT a moment to drain into the sink, then drop WITHOUT finish().
            for i in 0..90 {
                w.write_video(&bgra, i * frame_dur, frame_dur).unwrap();
            }
            std::thread::sleep(Duration::from_millis(150));
        }
        // In-process drop without Finalize() releases IMFByteStream on an MF work-queue thread.
        for _ in 0..40 {
            if std::fs::OpenOptions::new().write(true).open(&partial).is_ok() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let partial_bytes = std::fs::metadata(&partial).map(|m| m.len()).unwrap_or(0);
        assert!(partial_bytes > 0, "unfinalised fMP4 should have flushed bytes, got 0");

        let corrupt = sub.join("CapShotX_corrupt.mp4.partial");
        std::fs::write(&corrupt, b"not a valid mp4 container").unwrap();

        let recovered = recover_partials(&dir);
        assert_eq!(recovered.len(), 1, "expected 1 recovered file, got {recovered:?}");
        assert!(recovered[0].ends_with("CapShotX_crash (recovered).mp4"));
        assert!(!partial.exists());
        assert!(corrupt.exists(), "corrupt partial must never be deleted");
        assert!(media::poster_frame(&recovered[0]).is_ok());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn instant_stop_produces_valid_mp4() {
        let dir = std::env::temp_dir().join(format!("capshotx-instant-stop-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mon = platform_windows::monitors::enumerate().into_iter().next().unwrap();
        let b = mon.bounds;
        let area = platform_windows::monitors::PixelRect {
            left: b.left,
            top: b.top,
            right: (b.left + 320).min(b.right),
            bottom: (b.top + 180).min(b.bottom),
        };
        let out = dir.join("2026-10").join("Video").join("instant.mp4");
        let rec = Recorder::start(Settings {
            target: Target::Area(area),
            fps: 30,
            quality: VideoQuality::Compact,
            cursor: false,
            desktop: None,
            microphone: None,
            output: out,
        })
        .expect("start");
        let summary = rec.stop().expect("instant stop must still write at least one frame");
        assert!(summary.frames_written >= 1);
        let info = media::probe(&summary.path).expect("probe");
        assert!(info.has_video && info.duration > 0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
