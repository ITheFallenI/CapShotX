//! WASAPI capture: desktop audio (loopback) and microphones.
//!
//! Every packet carries the QPC timestamp WASAPI reports for its first frame,
//! in 100 ns units — the same clock as [`crate::clock::now`] — so callers can
//! place audio on a shared session timeline.

use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Media::Audio::{
    AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_E_DEVICE_INVALIDATED, AUDCLNT_SHAREMODE_SHARED,
    AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM, AUDCLNT_STREAMFLAGS_LOOPBACK,
    AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY, DEVICE_STATE_ACTIVE, IAudioCaptureClient, IAudioClient,
    IAudioRenderClient, IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator, WAVE_FORMAT_PCM,
    WAVEFORMATEX, WAVEFORMATEXTENSIBLE, eCapture, eConsole, eRender,
};
use windows::Win32::Media::KernelStreaming::WAVE_FORMAT_EXTENSIBLE;
use windows::Win32::Media::Multimedia::{KSDATAFORMAT_SUBTYPE_IEEE_FLOAT, WAVE_FORMAT_IEEE_FLOAT};
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree, STGM_READ,
};
use windows::core::{HSTRING, PWSTR, Result};

/// The format we ask Windows to deliver (it resamples/downmixes for us).
pub const RATE: u32 = 48_000;
pub const CHANNELS: u16 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    /// Speakers/headphones, captured via loopback ("desktop audio").
    Output,
    /// Microphones and line-in.
    Input,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioDevice {
    pub id: String,
    pub name: String,
    pub is_default: bool,
}

/// Initialises COM (multithreaded) on the calling thread; idempotent.
pub fn init_thread() {
    // SAFETY: may be called repeatedly; S_FALSE/RPC_E_CHANGED_MODE are fine.
    let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
}

fn enumerator() -> Result<IMMDeviceEnumerator> {
    init_thread();
    // SAFETY: standard COM activation.
    unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
}

fn flow(direction: Direction) -> windows::Win32::Media::Audio::EDataFlow {
    match direction {
        Direction::Output => eRender,
        Direction::Input => eCapture,
    }
}

unsafe fn device_id(device: &IMMDevice) -> Result<String> {
    // SAFETY: GetId returns a CoTaskMem string we free.
    unsafe {
        let id: PWSTR = device.GetId()?;
        let s = id.to_string().unwrap_or_default();
        CoTaskMemFree(Some(id.0 as *const _));
        Ok(s)
    }
}

unsafe fn device_name(device: &IMMDevice) -> String {
    // SAFETY: read-only property store access.
    unsafe {
        device
            .OpenPropertyStore(STGM_READ)
            .and_then(|store| store.GetValue(&PKEY_Device_FriendlyName))
            .map(|value| value.to_string())
            .unwrap_or_else(|_| "Unknown device".into())
    }
}

/// Active devices for `direction`, default first.
pub fn list_devices(direction: Direction) -> Result<Vec<AudioDevice>> {
    let e = enumerator()?;
    // SAFETY: COM calls on objects we own.
    unsafe {
        let default_id = e
            .GetDefaultAudioEndpoint(flow(direction), eConsole)
            .and_then(|d| device_id(&d))
            .ok();
        let collection = e.EnumAudioEndpoints(flow(direction), DEVICE_STATE_ACTIVE)?;
        let mut out = Vec::new();
        for i in 0..collection.GetCount()? {
            let device = collection.Item(i)?;
            let id = device_id(&device)?;
            out.push(AudioDevice {
                is_default: Some(&id) == default_id.as_ref(),
                name: device_name(&device),
                id,
            });
        }
        out.sort_by_key(|d| !d.is_default);
        Ok(out)
    }
}

/// A block of captured audio.
pub struct Packet {
    /// QPC time of the first frame, 100 ns units.
    pub time: i64,
    /// Interleaved f32 samples at `format.rate` × `format.channels`.
    pub samples: Vec<f32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Format {
    pub rate: u32,
    pub channels: u16,
}

#[derive(Debug)]
pub enum ReadError {
    /// Unplugged or disabled mid-stream.
    DeviceLost,
    Other(windows::core::Error),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SampleKind {
    F32,
    I16,
    I32,
}

/// An open WASAPI capture stream. Not `Send`: use it on the thread that
/// opened it (after [`init_thread`]).
pub struct CaptureStream {
    client: IAudioClient,
    capture: IAudioCaptureClient,
    pub format: Format,
    pub device_name: String,
    kind: SampleKind,
    block_align: usize,
}

impl CaptureStream {
    /// Opens `device_id` (or the default device). For [`Direction::Output`]
    /// this captures what the device is playing.
    pub fn open(direction: Direction, device_id: Option<&str>) -> Result<Self> {
        let e = enumerator()?;
        // SAFETY: COM calls with valid format structures; the mix format is
        // freed after use.
        unsafe {
            let device = match device_id {
                Some(id) => e.GetDevice(&HSTRING::from(id))?,
                None => e.GetDefaultAudioEndpoint(flow(direction), eConsole)?,
            };
            let device_name = device_name(&device);
            let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
            let loopback = if direction == Direction::Output { AUDCLNT_STREAMFLAGS_LOOPBACK } else { 0 };
            const BUFFER_100NS: i64 = 2_000_000; // 200 ms

            // Preferred: let Windows convert to 48 kHz float stereo.
            let wanted = float_format(RATE, CHANNELS);
            let preferred = client.Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                loopback | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
                BUFFER_100NS,
                0,
                &wanted,
                None,
            );
            let (format, kind, block_align) = match preferred {
                Ok(()) => (Format { rate: RATE, channels: CHANNELS }, SampleKind::F32, 8),
                Err(err) => {
                    tracing::info!("{device_name}: auto-convert unavailable ({err}); using mix format");
                    // Fall back to the device's own mix format on a fresh client.
                    let client2: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
                    let mix = client2.GetMixFormat()?;
                    let described = describe(&*mix);
                    let result = client2.Initialize(AUDCLNT_SHAREMODE_SHARED, loopback, BUFFER_100NS, 0, mix, None);
                    CoTaskMemFree(Some(mix as *const _));
                    result?;
                    let (format, kind, align) = described.ok_or_else(|| {
                        windows::core::Error::new(windows::core::HRESULT(0x8889_0008u32 as i32), "unsupported mix format")
                    })?;
                    return Ok(Self {
                        capture: client2.GetService()?,
                        client: client2,
                        format,
                        device_name,
                        kind,
                        block_align: align,
                    });
                }
            };
            Ok(Self { capture: client.GetService()?, client, format, device_name, kind, block_align })
        }
    }

    pub fn start(&self) -> Result<()> {
        // SAFETY: initialised client.
        unsafe { self.client.Start() }
    }

    pub fn stop(&self) {
        // SAFETY: initialised client.
        let _ = unsafe { self.client.Stop() };
    }

    /// Appends every packet currently available.
    pub fn read(&self, out: &mut Vec<Packet>) -> std::result::Result<(), ReadError> {
        let map = |e: windows::core::Error| {
            if e.code() == AUDCLNT_E_DEVICE_INVALIDATED { ReadError::DeviceLost } else { ReadError::Other(e) }
        };
        // SAFETY: GetBuffer/ReleaseBuffer are paired; the data pointer is
        // valid for `frames` frames until ReleaseBuffer.
        unsafe {
            loop {
                let next = self.capture.GetNextPacketSize().map_err(map)?;
                if next == 0 {
                    return Ok(());
                }
                let mut data = std::ptr::null_mut();
                let mut frames = 0u32;
                let mut flags = 0u32;
                let mut qpc = 0u64;
                self.capture
                    .GetBuffer(&mut data, &mut frames, &mut flags, None, Some(&mut qpc))
                    .map_err(map)?;
                let count = frames as usize * self.format.channels as usize;
                let samples = if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 || data.is_null() {
                    vec![0.0; count]
                } else {
                    let bytes = std::slice::from_raw_parts(data, frames as usize * self.block_align);
                    convert(bytes, self.kind, count)
                };
                self.capture.ReleaseBuffer(frames).map_err(map)?;
                out.push(Packet { time: qpc as i64, samples });
            }
        }
    }
}

impl Drop for CaptureStream {
    fn drop(&mut self) {
        self.stop();
    }
}

fn float_format(rate: u32, channels: u16) -> WAVEFORMATEX {
    WAVEFORMATEX {
        wFormatTag: WAVE_FORMAT_IEEE_FLOAT as u16,
        nChannels: channels,
        nSamplesPerSec: rate,
        nAvgBytesPerSec: rate * channels as u32 * 4,
        nBlockAlign: channels * 4,
        wBitsPerSample: 32,
        cbSize: 0,
    }
}

/// Interprets a mix format we can convert from.
unsafe fn describe(fmt: &WAVEFORMATEX) -> Option<(Format, SampleKind, usize)> {
    let format = Format { rate: fmt.nSamplesPerSec, channels: fmt.nChannels };
    let tag = fmt.wFormatTag as u32;
    let is_float = tag == WAVE_FORMAT_IEEE_FLOAT
        || (tag == WAVE_FORMAT_EXTENSIBLE && {
            // SAFETY: EXTENSIBLE tag guarantees the larger struct.
            let ext = unsafe { &*(fmt as *const WAVEFORMATEX as *const WAVEFORMATEXTENSIBLE) };
            let sub = ext.SubFormat;
            sub == KSDATAFORMAT_SUBTYPE_IEEE_FLOAT
        });
    let kind = match (is_float, fmt.wBitsPerSample) {
        (true, 32) => SampleKind::F32,
        (false, 16) if tag == WAVE_FORMAT_PCM || tag == WAVE_FORMAT_EXTENSIBLE => SampleKind::I16,
        (false, 32) => SampleKind::I32,
        _ => return None,
    };
    Some((format, kind, fmt.nBlockAlign as usize))
}

fn convert(bytes: &[u8], kind: SampleKind, count: usize) -> Vec<f32> {
    match kind {
        SampleKind::F32 => bytes.chunks_exact(4).take(count).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect(),
        SampleKind::I16 => bytes
            .chunks_exact(2)
            .take(count)
            .map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0)
            .collect(),
        SampleKind::I32 => bytes
            .chunks_exact(4)
            .take(count)
            .map(|b| i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f32 / 2_147_483_648.0)
            .collect(),
    }
}

/// A user-facing explanation for a failure to open a capture stream.
pub fn describe_open_error(direction: Direction, err: &windows::core::Error) -> String {
    const E_ACCESSDENIED: i32 = 0x8007_0005u32 as i32;
    const E_NOTFOUND: i32 = 0x8007_0490u32 as i32;
    const AUDCLNT_E_DEVICE_IN_USE: i32 = 0x8889_000Au32 as i32;
    let what = match direction {
        Direction::Output => "Desktop audio",
        Direction::Input => "Microphone",
    };
    match err.code().0 {
        E_ACCESSDENIED if direction == Direction::Input => {
            "Microphone access is blocked. Turn on Settings › Privacy & security › Microphone › \
             \"Let desktop apps access your microphone\"."
                .into()
        }
        E_NOTFOUND => format!("{what}: the selected device isn't connected."),
        AUDCLNT_E_DEVICE_IN_USE => format!("{what}: the device is in exclusive use by another app."),
        _ if err.code() == AUDCLNT_E_DEVICE_INVALIDATED => format!("{what}: the device was disconnected."),
        _ => format!("{what} couldn't start: {}", err.message()),
    }
}

/// Plays a short two-note stereo chime (`C5` → `G5`, ~520 ms) on `device_id`
/// (or the default Windows output device when `None`) so the user can hear the
/// selected output and watch the Desktop audio level meter respond immediately.
pub fn play_test_sound(device_id: Option<String>) {
    let _ = std::thread::Builder::new()
        .name("desktop-test-sound".into())
        .spawn(move || {
            if play_wasapi_chime(device_id.as_deref()).is_err() {
                crate::sound::play_shutter();
            }
        });
}

fn play_wasapi_chime(device_id: Option<&str>) -> Result<()> {
    let e = enumerator()?;
    const DURATION_SECS: f32 = 0.52;
    let total_frames = (RATE as f32 * DURATION_SECS) as u32;
    let buffer_100ns = ((DURATION_SECS + 0.15) * 10_000_000.0) as i64;

    // SAFETY: standard WASAPI shared-mode render session on the chosen endpoint.
    unsafe {
        let device = match device_id {
            Some(id) => e.GetDevice(&HSTRING::from(id))?,
            None => e.GetDefaultAudioEndpoint(eRender, eConsole)?,
        };
        let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
        let wanted = float_format(RATE, CHANNELS);
        client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
            buffer_100ns,
            0,
            &wanted,
            None,
        )?;
        let buf_frames = client.GetBufferSize()?;
        let write_frames = total_frames.min(buf_frames);
        let render: IAudioRenderClient = client.GetService()?;
        let data = render.GetBuffer(write_frames)?;
        let out = std::slice::from_raw_parts_mut(data as *mut f32, write_frames as usize * CHANNELS as usize);

        for i in 0..write_frames as usize {
            let t = i as f32 / RATE as f32;
            // Warm two-note chime: C5 (523.25 Hz) then G5 (783.99 Hz) with soft attack/decay.
            let mut sample = 0.0f32;
            for (start, freq, amp) in [(0.0f32, 523.25f32, 0.22f32), (0.14f32, 783.99f32, 0.25f32)] {
                let dt = t - start;
                if dt >= 0.0 {
                    let attack = (dt / 0.012).min(1.0);
                    let decay = (-dt / 0.14).exp();
                    let fundamental = (std::f32::consts::TAU * freq * dt).sin();
                    let overtone = 0.25 * (std::f32::consts::TAU * (freq * 2.0) * dt).sin();
                    sample += amp * attack * decay * (fundamental + overtone);
                }
            }
            let s = sample.clamp(-0.9, 0.9);
            out[i * 2] = s;
            out[i * 2 + 1] = s;
        }
        render.ReleaseBuffer(write_frames, 0)?;
        client.Start()?;
        std::thread::sleep(std::time::Duration::from_millis(550));
        let _ = client.Stop();
    }
    Ok(())
}
