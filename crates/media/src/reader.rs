//! Reading media files with the Media Foundation source reader.

use std::path::Path;

use windows::Win32::Media::MediaFoundation::{
    IMFAttributes, IMFSourceReader, MF_MT_AUDIO_BITS_PER_SAMPLE, MF_MT_AUDIO_NUM_CHANNELS,
    MF_MT_AUDIO_SAMPLES_PER_SECOND, MF_MT_DEFAULT_STRIDE, MF_MT_FRAME_SIZE, MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE,
    MF_PD_DURATION, MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, MF_SOURCE_READER_FIRST_AUDIO_STREAM,
    MF_SOURCE_READER_FIRST_VIDEO_STREAM, MF_SOURCE_READER_MEDIASOURCE, MF_SOURCE_READERF_ENDOFSTREAM,
    MFAudioFormat_PCM, MFCreateAttributes, MFCreateMediaType, MFCreateSourceReaderFromURL, MFMediaType_Audio,
    MFMediaType_Video, MFVideoFormat_RGB32,
};
use windows::core::{HSTRING, Result};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MediaInfo {
    /// Container duration in 100 ns units.
    pub duration: i64,
    pub has_video: bool,
    pub has_audio: bool,
    pub width: u32,
    pub height: u32,
}

impl MediaInfo {
    pub fn seconds(&self) -> f64 {
        self.duration as f64 / crate::TICKS as f64
    }
}

fn open(path: &Path, video_processing: bool) -> Result<IMFSourceReader> {
    crate::init()?;
    let url = HSTRING::from(path.as_os_str());
    // SAFETY: attribute store and reader creation with valid inputs.
    unsafe {
        let mut attrs: Option<IMFAttributes> = None;
        MFCreateAttributes(&mut attrs, 1)?;
        let attrs = attrs.expect("MFCreateAttributes succeeded");
        if video_processing {
            attrs.SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1)?;
        }
        MFCreateSourceReaderFromURL(&url, &attrs)
    }
}

const VIDEO: u32 = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
const AUDIO: u32 = MF_SOURCE_READER_FIRST_AUDIO_STREAM.0 as u32;

/// Duration, streams and frame size.
pub fn probe(path: &Path) -> Result<MediaInfo> {
    let reader = open(path, false)?;
    // SAFETY: read-only queries on an open reader.
    unsafe {
        let duration = reader
            .GetPresentationAttribute(MF_SOURCE_READER_MEDIASOURCE.0 as u32, &MF_PD_DURATION)
            .ok()
            .and_then(|v| u64::try_from(&v).ok())
            .unwrap_or(0) as i64;
        let video = reader.GetCurrentMediaType(VIDEO).ok();
        let (width, height) = video
            .as_ref()
            .and_then(|t| t.GetUINT64(&MF_MT_FRAME_SIZE).ok())
            .map_or((0, 0), |s| ((s >> 32) as u32, s as u32));
        Ok(MediaInfo {
            duration,
            has_video: video.is_some(),
            has_audio: reader.GetCurrentMediaType(AUDIO).is_ok(),
            width,
            height,
        })
    }
}

/// Decodes the first audio stream to interleaved 16-bit PCM.
/// Returns (samples, sample rate, channels).
pub fn decode_audio(path: &Path) -> Result<(Vec<i16>, u32, u16)> {
    let reader = open(path, false)?;
    // SAFETY: standard synchronous ReadSample loop; buffers are locked only
    // while copying.
    unsafe {
        let pcm = MFCreateMediaType()?;
        pcm.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
        pcm.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_PCM)?;
        pcm.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
        reader.SetCurrentMediaType(AUDIO, None, &pcm)?;
        let actual = reader.GetCurrentMediaType(AUDIO)?;
        let rate = actual.GetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND)?;
        let channels = actual.GetUINT32(&MF_MT_AUDIO_NUM_CHANNELS)? as u16;

        let mut out = Vec::new();
        loop {
            let mut flags = 0u32;
            let mut sample = None;
            reader.ReadSample(AUDIO, 0, None, Some(&mut flags), None, Some(&mut sample))?;
            if let Some(sample) = sample {
                let buffer = sample.ConvertToContiguousBuffer()?;
                let mut ptr = std::ptr::null_mut();
                let mut len = 0u32;
                buffer.Lock(&mut ptr, None, Some(&mut len))?;
                let bytes = std::slice::from_raw_parts(ptr, len as usize);
                out.extend(bytes.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])));
                buffer.Unlock()?;
            }
            if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                break;
            }
        }
        Ok((out, rate, channels))
    }
}

/// The first video frame as straight-alpha RGBA (width, height, pixels).
pub fn poster_frame(path: &Path) -> Result<(u32, u32, Vec<u8>)> {
    let reader = open(path, true)?;
    // SAFETY: as in `decode_audio`; stride comes from the negotiated type.
    unsafe {
        let rgb = MFCreateMediaType()?;
        rgb.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        rgb.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32)?;
        reader.SetCurrentMediaType(VIDEO, None, &rgb)?;
        let actual = reader.GetCurrentMediaType(VIDEO)?;
        let size = actual.GetUINT64(&MF_MT_FRAME_SIZE)?;
        let (w, h) = ((size >> 32) as u32, size as u32);
        let stride = actual.GetUINT32(&MF_MT_DEFAULT_STRIDE).map_or(w as i32 * 4, |s| s as i32);

        for _ in 0..30 {
            let mut flags = 0u32;
            let mut sample = None;
            reader.ReadSample(VIDEO, 0, None, Some(&mut flags), None, Some(&mut sample))?;
            if let Some(sample) = sample {
                let buffer = sample.ConvertToContiguousBuffer()?;
                let mut ptr = std::ptr::null_mut();
                let mut len = 0u32;
                buffer.Lock(&mut ptr, None, Some(&mut len))?;
                let bytes = std::slice::from_raw_parts(ptr, len as usize);
                let row = w as usize * 4;
                let pitch = stride.unsigned_abs() as usize;
                let mut rgba = Vec::with_capacity(row * h as usize);
                for y in 0..h as usize {
                    // Negative stride = bottom-up rows.
                    let src_y = if stride < 0 { h as usize - 1 - y } else { y };
                    let start = src_y * pitch;
                    if start + row > bytes.len() {
                        break;
                    }
                    for px in bytes[start..start + row].chunks_exact(4) {
                        rgba.extend_from_slice(&[px[2], px[1], px[0], 255]);
                    }
                }
                buffer.Unlock()?;
                if rgba.len() == row * h as usize {
                    return Ok((w, h, rgba));
                }
            }
            if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                break;
            }
        }
        Err(windows::core::Error::new(windows::core::HRESULT(0x8000_4005u32 as i32), "no video frame"))
    }
}
