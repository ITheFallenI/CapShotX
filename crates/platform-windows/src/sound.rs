//! Short UI sounds, synthesised in code (no audio assets).

use std::sync::OnceLock;

use windows::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_MEMORY, SND_NODEFAULT};
use windows::core::PCWSTR;

const SAMPLE_RATE: u32 = 44_100;

/// Plays a soft camera-shutter click without blocking.
pub fn play_shutter() {
    static WAV: OnceLock<Vec<u8>> = OnceLock::new();
    let wav = WAV.get_or_init(shutter_wav);
    // SAFETY: with SND_MEMORY the pointer is a complete WAV image; it lives
    // in a static, so it outlives asynchronous playback.
    unsafe {
        let _ = PlaySoundW(PCWSTR(wav.as_ptr() as *const u16), None, SND_MEMORY | SND_ASYNC | SND_NODEFAULT);
    }
}

/// Two quick mechanical clicks (shutter open/close): filtered noise bursts
/// with a low thump, ~140 ms, mono 16-bit.
fn shutter_wav() -> Vec<u8> {
    let len = (SAMPLE_RATE as f32 * 0.14) as usize;
    let mut seed: u32 = 0x1234_5678;
    let mut noise = move || {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (seed >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0
    };

    let mut samples = Vec::with_capacity(len);
    let mut lowpass = 0.0f32;
    for i in 0..len {
        let t = i as f32 / SAMPLE_RATE as f32;
        let mut v = 0.0;
        for (start, gain) in [(0.0f32, 1.0f32), (0.055, 0.7)] {
            let dt = t - start;
            if dt >= 0.0 {
                let env = (-dt / 0.006).exp();
                let thump = (2.0 * std::f32::consts::PI * 170.0 * dt).sin() * (-dt / 0.012).exp();
                v += gain * (noise() * env * 0.8 + thump * 0.5);
            }
        }
        // Gentle low-pass takes the harsh edge off the noise.
        lowpass += (v - lowpass) * 0.35;
        samples.push((lowpass * 0.32 * i16::MAX as f32) as i16);
    }

    let data_len = (samples.len() * 2) as u32;
    let mut wav = Vec::with_capacity(44 + data_len as usize);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes()); // fmt chunk size
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&1u16.to_le_bytes()); // mono
    wav.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    wav.extend_from_slice(&(SAMPLE_RATE * 2).to_le_bytes()); // byte rate
    wav.extend_from_slice(&2u16.to_le_bytes()); // block align
    wav.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        wav.extend_from_slice(&s.to_le_bytes());
    }
    wav
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shutter_is_a_valid_pcm_wav() {
        let wav = shutter_wav();
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..16], b"WAVEfmt ");
        let riff_len = u32::from_le_bytes(wav[4..8].try_into().unwrap()) as usize;
        assert_eq!(riff_len + 8, wav.len());
        let data_len = u32::from_le_bytes(wav[40..44].try_into().unwrap()) as usize;
        assert_eq!(data_len + 44, wav.len());
        // Audible but not clipping.
        let peak = wav[44..]
            .chunks_exact(2)
            .map(|b| i16::from_le_bytes([b[0], b[1]]).unsigned_abs())
            .max()
            .unwrap();
        assert!(peak > 3000 && peak < i16::MAX as u16, "peak {peak}");
    }
}
