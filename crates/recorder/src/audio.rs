//! One WASAPI capture thread per audio source (REC-02).
//!
//! Each thread places its packets on the session timeline by their QPC
//! timestamps, publishes live levels, and survives device loss: it records a
//! warning, keeps retrying, and resumes (on the new default device when
//! following the default) without stopping the recording.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use platform_windows::audio::{self, CaptureStream, Direction, ReadError};
use platform_windows::clock;

use crate::clock::SessionClock;
use crate::mixer::{Mixer, RATE};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Desktop,
    Microphone,
}

impl Kind {
    fn direction(self) -> Direction {
        match self {
            Kind::Desktop => Direction::Output,
            Kind::Microphone => Direction::Input,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Kind::Desktop => "Desktop audio",
            Kind::Microphone => "Microphone",
        }
    }
}

/// Where captured audio goes while recording.
#[derive(Clone)]
pub(crate) struct Sink {
    pub mixer: Arc<Mutex<Mixer>>,
    pub track: usize,
    /// `None` until the recording actually starts.
    pub clock: Arc<Mutex<Option<SessionClock>>>,
}

/// Live state of a source, readable from any thread.
#[derive(Default)]
pub struct SourceStatus {
    peak: AtomicU32,
    rms: AtomicU32,
    packets: AtomicU64,
    /// Media-clock time of the latest packet.
    last_packet: AtomicU64,
    connected: AtomicBool,
    device_name: Mutex<String>,
    /// Problems worth telling the user, oldest first.
    warnings: Mutex<Vec<String>>,
}

impl SourceStatus {
    /// Peak and RMS (0..=1) of the most recent packet; read-and-reset peak.
    pub fn levels(&self) -> (f32, f32) {
        (f32::from_bits(self.peak.swap(0, Ordering::Relaxed)), f32::from_bits(self.rms.load(Ordering::Relaxed)))
    }

    pub fn device_name(&self) -> String {
        self.device_name.lock().unwrap().clone()
    }

    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }

    pub fn packets(&self) -> u64 {
        self.packets.load(Ordering::Relaxed)
    }

    /// Time since the last packet arrived, if any did.
    pub fn since_last_packet(&self) -> Option<Duration> {
        let t = self.last_packet.load(Ordering::Relaxed) as i64;
        (t > 0).then(|| Duration::from_nanos(((clock::now() - t).max(0) * 100) as u64))
    }

    pub fn warnings(&self) -> Vec<String> {
        self.warnings.lock().unwrap().clone()
    }

    fn warn(&self, msg: String) {
        tracing::warn!("{msg}");
        self.warnings.lock().unwrap().push(msg);
    }
}

pub struct AudioSource {
    pub kind: Kind,
    pub status: Arc<SourceStatus>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl AudioSource {
    /// Opens the device (default if `device_id` is `None`) and starts
    /// capturing. Fails with a user-facing message if it can't open.
    pub(crate) fn start(kind: Kind, device_id: Option<String>, sink: Option<Sink>) -> Result<Self, String> {
        let status = Arc::new(SourceStatus::default());
        let stop = Arc::new(AtomicBool::new(false));
        let (opened_tx, opened_rx) = mpsc::channel();
        let thread = {
            let (status, stop) = (status.clone(), stop.clone());
            std::thread::Builder::new()
                .name(format!("audio-{kind:?}").to_lowercase())
                .spawn(move || run(kind, device_id, sink, &status, &stop, opened_tx))
                .map_err(|e| e.to_string())?
        };
        match opened_rx.recv() {
            Ok(Ok(())) => Ok(Self { kind, status, stop, thread: Some(thread) }),
            Ok(Err(msg)) => {
                let _ = thread.join();
                Err(msg)
            }
            Err(_) => Err(format!("{} couldn't start.", kind.label())),
        }
    }

    /// Level monitoring only (before recording / audio test).
    pub fn monitor(kind: Kind, device_id: Option<String>) -> Result<Self, String> {
        Self::start(kind, device_id, None)
    }

    /// Reads what's left, then stops. Packets stamped before the session
    /// clock was paused for stopping still reach the mixer.
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for AudioSource {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn open(kind: Kind, device_id: Option<&str>, status: &SourceStatus) -> Result<CaptureStream, String> {
    let stream = CaptureStream::open(kind.direction(), device_id)
        .map_err(|e| audio::describe_open_error(kind.direction(), &e))?;
    stream.start().map_err(|e| audio::describe_open_error(kind.direction(), &e))?;
    *status.device_name.lock().unwrap() = stream.device_name.clone();
    status.connected.store(true, Ordering::Relaxed);
    Ok(stream)
}

fn default_id(kind: Kind) -> Option<String> {
    audio::list_devices(kind.direction()).ok()?.into_iter().find(|d| d.is_default).map(|d| d.id)
}

fn run(
    kind: Kind,
    device_id: Option<String>,
    sink: Option<Sink>,
    status: &SourceStatus,
    stop: &AtomicBool,
    opened: mpsc::Sender<Result<(), String>>,
) {
    audio::init_thread();
    let mut stream = match open(kind, device_id.as_deref(), status) {
        Ok(s) => {
            let _ = opened.send(Ok(()));
            Some(s)
        }
        Err(msg) => {
            let _ = opened.send(Err(msg));
            return;
        }
    };
    // Loopback follows the default output device (REC-02: rebind safely).
    let follow_default = device_id.is_none() && kind == Kind::Desktop;
    let mut current_default = if follow_default { default_id(kind) } else { None };
    let mut last_check = Instant::now();
    let mut packets = Vec::new();

    loop {
        let stopping = stop.load(Ordering::Acquire);
        if let Some(s) = &stream {
            match s.read(&mut packets) {
                Ok(()) => {}
                Err(ReadError::DeviceLost) => {
                    status.warn(format!(
                        "{}: \"{}\" was disconnected at {}; audio is missing from there until it reconnects.",
                        kind.label(),
                        status.device_name(),
                        timestamp(&sink),
                    ));
                    status.connected.store(false, Ordering::Relaxed);
                    stream = None;
                }
                Err(ReadError::Other(e)) => {
                    status.warn(format!("{}: capture error at {}: {}", kind.label(), timestamp(&sink), e.message()));
                    status.connected.store(false, Ordering::Relaxed);
                    stream = None;
                }
            }
            if let Some(s) = &stream {
                for p in packets.drain(..) {
                    deliver(&p, s.format.rate, s.format.channels, status, sink.as_ref());
                }
            }
            packets.clear();
        }
        if stopping {
            break;
        }

        // Once a second: reconnect a lost device, follow default changes.
        if last_check.elapsed() >= Duration::from_secs(1) {
            last_check = Instant::now();
            if follow_default {
                let now_default = default_id(kind);
                if stream.is_some() && now_default.is_some() && now_default != current_default {
                    tracing::info!("default output changed; rebinding desktop audio");
                    stream = None;
                }
                current_default = now_default;
            }
            if stream.is_none() {
                if let Ok(s) = open(kind, device_id.as_deref(), status) {
                    status.warn(format!("{}: recording from \"{}\" again since {}.", kind.label(), s.device_name, timestamp(&sink)));
                    stream = Some(s);
                }
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn deliver(p: &audio::Packet, rate: u32, channels: u16, status: &SourceStatus, sink: Option<&Sink>) {
    let (mut peak, mut sum) = (0f32, 0f64);
    for &s in &p.samples {
        peak = peak.max(s.abs());
        sum += (s as f64) * (s as f64);
    }
    let rms = (sum / p.samples.len().max(1) as f64).sqrt() as f32;
    status.peak.fetch_max(peak.to_bits(), Ordering::Relaxed); // positive floats order like their bits
    status.rms.store(rms.to_bits(), Ordering::Relaxed);
    status.packets.fetch_add(1, Ordering::Relaxed);
    status.last_packet.store(clock::now() as u64, Ordering::Relaxed);

    let Some(sink) = sink else { return };
    let at = sink.clock.lock().unwrap().as_ref().and_then(|c| c.active_at(p.time));
    if let Some(t) = at {
        let pos = t * RATE as i64 / clock::TICKS_PER_SECOND;
        sink.mixer.lock().unwrap().push(sink.track, pos, &p.samples, rate, channels);
    }
}

/// Recording time "now" as m:ss, for warnings.
fn timestamp(sink: &Option<Sink>) -> String {
    let t = sink
        .as_ref()
        .and_then(|s| s.clock.lock().unwrap().as_ref().map(|c| c.elapsed(clock::now())))
        .unwrap_or(0);
    let secs = t / clock::TICKS_PER_SECOND;
    format!("{}:{:02}", secs / 60, secs % 60)
}
