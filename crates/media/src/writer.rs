//! MP4 writing through the Media Foundation sink writer.

use std::path::Path;

use windows::Win32::Media::MediaFoundation::{
    IMFAttributes, IMFMediaType, IMFSinkWriter, MF_MT_AAC_PAYLOAD_TYPE, MF_MT_AUDIO_AVG_BYTES_PER_SECOND,
    MF_MT_AUDIO_BITS_PER_SAMPLE, MF_MT_AUDIO_BLOCK_ALIGNMENT, MF_MT_AUDIO_NUM_CHANNELS,
    MF_MT_AUDIO_SAMPLES_PER_SECOND, MF_MT_AVG_BITRATE, MF_MT_DEFAULT_STRIDE, MF_MT_FRAME_RATE,
    MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE, MF_MT_MPEG2_PROFILE,
    MF_MT_PIXEL_ASPECT_RATIO, MF_MT_SUBTYPE, MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS,
    MF_SINK_WRITER_DISABLE_THROTTLING, MF_TRANSCODE_CONTAINERTYPE, MFAudioFormat_AAC, MFAudioFormat_PCM,
    MFCreateAttributes, MFCreateMediaType, MFCreateMemoryBuffer, MFCreateSample,
    MFCreateSinkWriterFromURL, MFMediaType_Audio, MFMediaType_Video, MFTranscodeContainerType_FMPEG4,
    MFTranscodeContainerType_MPEG4, MFVideoFormat_H264, MFVideoFormat_RGB32, MFVideoInterlace_Progressive,
    eAVEncH264VProfile_High,
};
use windows::core::{HSTRING, Result};

#[derive(Clone, Copy, Debug)]
pub struct VideoConfig {
    /// Must be even (H.264).
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate: u32,
}

#[derive(Clone, Copy, Debug)]
pub struct AudioConfig {
    pub sample_rate: u32,
    pub channels: u16,
    /// AAC bitrate in bits/s (96k, 128k, 160k or 192k).
    pub bitrate: u32,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self { sample_rate: 48_000, channels: 2, bitrate: 192_000 }
    }
}

/// Writes H.264 video (from top-down BGRA frames) and optional AAC audio
/// (from interleaved 16-bit PCM) into an MP4.
///
/// Uses fragmented MP4 where available, so an interrupted recording still
/// leaves playable media up to the last fragment.
pub struct Mp4Writer {
    writer: IMFSinkWriter,
    video_stream: u32,
    audio_stream: Option<u32>,
    video: VideoConfig,
    audio: Option<AudioConfig>,
    pub fragmented: bool,
}

impl Mp4Writer {
    pub fn create(path: &Path, video: VideoConfig, audio: Option<AudioConfig>) -> Result<Self> {
        crate::init()?;
        let url = HSTRING::from(path.as_os_str());
        // Fragmented first (crash-tolerant); plain MP4 as a fallback.
        let mut last_err = None;
        for (container, fragmented) in [(MFTranscodeContainerType_FMPEG4, true), (MFTranscodeContainerType_MPEG4, false)] {
            // SAFETY: attribute store and writer creation with valid inputs.
            let created = unsafe {
                (|| {
                    let mut attrs: Option<IMFAttributes> = None;
                    MFCreateAttributes(&mut attrs, 3)?;
                    let attrs = attrs.expect("MFCreateAttributes succeeded");
                    attrs.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1)?;
                    attrs.SetUINT32(&MF_SINK_WRITER_DISABLE_THROTTLING, 1)?;
                    attrs.SetGUID(&MF_TRANSCODE_CONTAINERTYPE, &container)?;
                    MFCreateSinkWriterFromURL(&url, None, &attrs)
                })()
            };
            match created.and_then(|writer| Self::configure(writer, video, audio, fragmented)) {
                Ok(writer) => return Ok(writer),
                Err(err) => {
                    tracing::info!("MP4 writer (fragmented: {fragmented}) unavailable: {err}");
                    let _ = std::fs::remove_file(path);
                    last_err = Some(err);
                }
            }
        }
        Err(last_err.expect("at least one attempt"))
    }

    fn configure(writer: IMFSinkWriter, video: VideoConfig, audio: Option<AudioConfig>, fragmented: bool) -> Result<Self> {
        // SAFETY: media types are fully described before use.
        unsafe {
            let size = ((video.width as u64) << 32) | video.height as u64;
            let rate = ((video.fps as u64) << 32) | 1;
            let square = (1u64 << 32) | 1;

            let out = new_type()?;
            out.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            out.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
            out.SetUINT32(&MF_MT_AVG_BITRATE, video.bitrate)?;
            out.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            out.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_High.0 as u32)?;
            out.SetUINT64(&MF_MT_FRAME_SIZE, size)?;
            out.SetUINT64(&MF_MT_FRAME_RATE, rate)?;
            out.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, square)?;
            let video_stream = writer.AddStream(&out)?;

            let input = new_type()?;
            input.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            input.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32)?;
            input.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            input.SetUINT64(&MF_MT_FRAME_SIZE, size)?;
            input.SetUINT64(&MF_MT_FRAME_RATE, rate)?;
            input.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, square)?;
            // Positive stride = top-down rows, as captured.
            input.SetUINT32(&MF_MT_DEFAULT_STRIDE, video.width * 4)?;
            writer.SetInputMediaType(video_stream, &input, None)?;

            let audio_stream = match audio {
                Some(a) => {
                    let out = new_type()?;
                    out.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
                    out.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_AAC)?;
                    out.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
                    out.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, a.sample_rate)?;
                    out.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, a.channels as u32)?;
                    out.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, a.bitrate / 8)?;
                    out.SetUINT32(&MF_MT_AAC_PAYLOAD_TYPE, 0)?;
                    let stream = writer.AddStream(&out)?;

                    let input = new_type()?;
                    let block = a.channels as u32 * 2;
                    input.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
                    input.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_PCM)?;
                    input.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
                    input.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, a.sample_rate)?;
                    input.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, a.channels as u32)?;
                    input.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, block)?;
                    input.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, block * a.sample_rate)?;
                    writer.SetInputMediaType(stream, &input, None)?;
                    Some(stream)
                }
                None => None,
            };

            writer.BeginWriting()?;
            Ok(Self { writer, video_stream, audio_stream, video, audio, fragmented })
        }
    }

    pub fn video_config(&self) -> VideoConfig {
        self.video
    }

    pub fn audio_config(&self) -> Option<AudioConfig> {
        self.audio
    }

    /// Writes one frame. `bgra` is top-down, `width * 4` bytes per row.
    /// Times are 100 ns units from the start of the recording.
    pub fn write_video(&mut self, bgra: &[u8], time: i64, duration: i64) -> Result<()> {
        let expected = (self.video.width * self.video.height * 4) as usize;
        assert_eq!(bgra.len(), expected, "frame size mismatch");
        self.write(self.video_stream, bgra, time, duration)
    }

    /// Writes interleaved 16-bit PCM starting at `time` (100 ns units).
    pub fn write_audio(&mut self, pcm: &[i16], time: i64) -> Result<()> {
        let (Some(stream), Some(cfg)) = (self.audio_stream, self.audio) else { return Ok(()) };
        if pcm.is_empty() {
            return Ok(());
        }
        let frames = pcm.len() as i64 / cfg.channels as i64;
        let duration = frames * crate::TICKS / cfg.sample_rate as i64;
        // SAFETY: i16 slice viewed as bytes for the copy.
        let bytes = unsafe { std::slice::from_raw_parts(pcm.as_ptr() as *const u8, pcm.len() * 2) };
        self.write(stream, bytes, time, duration)
    }

    fn write(&mut self, stream: u32, bytes: &[u8], time: i64, duration: i64) -> Result<()> {
        // SAFETY: the buffer is locked for exactly the copy, then handed to
        // the sample, which the writer takes a reference to.
        unsafe {
            let buffer = MFCreateMemoryBuffer(bytes.len() as u32)?;
            let mut ptr = std::ptr::null_mut();
            buffer.Lock(&mut ptr, None, None)?;
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, bytes.len());
            buffer.Unlock()?;
            buffer.SetCurrentLength(bytes.len() as u32)?;
            let sample = MFCreateSample()?;
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime(time)?;
            sample.SetSampleDuration(duration)?;
            self.writer.WriteSample(stream, &sample)
        }
    }

    /// Flushes encoders and closes the file. Without this the MP4 may be
    /// incomplete (fragmented files remain playable up to the last fragment).
    pub fn finish(self) -> Result<()> {
        // SAFETY: final call on a writer that has begun writing.
        unsafe { self.writer.Finalize() }
    }
}

fn new_type() -> Result<IMFMediaType> {
    // SAFETY: plain object creation.
    unsafe { MFCreateMediaType() }
}
