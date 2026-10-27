//! Media Foundation encode/decode: MP4 (H.264 + AAC) writing, probing,
//! audio decoding for tests and poster frames for the gallery.

#![cfg(windows)]

mod reader;
mod writer;

use std::sync::OnceLock;

use windows::Win32::Media::MediaFoundation::{MF_VERSION, MFSTARTUP_FULL, MFStartup};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};

pub use reader::{MediaInfo, decode_audio, poster_frame, probe};
pub use writer::{AudioConfig, Mp4Writer, VideoConfig};

/// 100 ns units per second (Media Foundation's timebase).
pub const TICKS: i64 = 10_000_000;

/// Starts Media Foundation once per process and COM on the calling thread.
pub fn init() -> windows::core::Result<()> {
    static STARTED: OnceLock<Result<(), windows::core::HRESULT>> = OnceLock::new();
    // SAFETY: COM init is per-thread and idempotent; MFStartup runs once.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        STARTED
            .get_or_init(|| MFStartup(MF_VERSION, MFSTARTUP_FULL).map_err(|e| e.code()))
            .map_err(windows::core::Error::from)
    }
}

#[cfg(test)]
mod tests;
