//! Screen recording (Phase 2): WGC video + WASAPI audio on one media clock,
//! encoded straight to MP4.

#![cfg(windows)]

pub mod clock;
pub mod mixer;
pub mod video;
pub mod audio;
mod session;

pub use audio::{AudioSource, Kind, SourceStatus};
pub use session::{AudioInput, Recorder, RecorderHandle, Settings, Summary, VideoQuality, partial_path, recover_partials};
pub use video::Target;
