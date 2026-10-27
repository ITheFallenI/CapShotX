//! Places timestamped audio from several sources on one 48 kHz stereo
//! timeline and mixes it.
//!
//! - Gaps (e.g. loopback delivers nothing while the desktop is silent) become
//!   silence, so quiet stretches can never shorten or shift the recording.
//! - Each source is kept continuous; only if its clock drifts more than
//!   [`RESYNC_FRAMES`] from where its timestamps say it should be is it
//!   re-placed (one small, rare discontinuity instead of constant jitter).

use std::collections::VecDeque;

pub const RATE: u32 = 48_000;
/// 20 ms.
pub const RESYNC_FRAMES: i64 = 960;
/// Maximum unmixed buffer per source (10 s) so anomalous timestamp jumps
/// never allocate unbounded silence (REC-05).
pub const MAX_BUFFER_FRAMES: i64 = RATE as i64 * 10;

#[derive(Default)]
struct Track {
    /// Timeline frame where the next pushed frame belongs.
    next: Option<i64>,
    /// Interleaved stereo frames starting at `start`.
    buf: VecDeque<f32>,
    start: i64,
    gain: f32,
    resampler: Resampler,
}

impl Track {
    fn end(&self) -> i64 {
        self.start + self.buf.len() as i64 / 2
    }
}

pub struct Mixer {
    tracks: Vec<Track>,
    /// Frames already mixed and handed out.
    written: i64,
    muted: bool,
    pub resyncs: u32,
}

impl Mixer {
    pub fn new(sources: usize) -> Self {
        Self {
            tracks: (0..sources).map(|_| Track { gain: 1.0, ..Default::default() }).collect(),
            written: 0,
            muted: false,
            resyncs: 0,
        }
    }

    pub fn written(&self) -> i64 {
        self.written
    }

    pub fn set_gain(&mut self, source: usize, gain: f32) {
        self.tracks[source].gain = gain;
    }

    pub fn set_muted(&mut self, muted: bool) {
        self.muted = muted;
    }

    pub fn is_muted(&self) -> bool {
        self.muted
    }

    /// Adds audio from `source` whose first frame belongs at timeline frame
    /// `pos`. `samples` are interleaved at `rate` Hz with `channels`.
    pub fn push(&mut self, source: usize, pos: i64, samples: &[f32], rate: u32, channels: u16) {
        let stereo = to_stereo(samples, channels);
        let t = &mut self.tracks[source];
        let stereo = if rate == RATE { stereo } else { t.resampler.process(&stereo, rate) };
        let frames = stereo.len() as i64 / 2;
        if frames == 0 {
            return;
        }

        let pos = pos.min(self.written + MAX_BUFFER_FRAMES);
        let mut at = match t.next {
            Some(next) if (next - pos).abs() <= RESYNC_FRAMES => next,
            Some(_) => {
                self.resyncs += 1;
                pos
            }
            None => pos,
        }
        .min(self.written + MAX_BUFFER_FRAMES);
        t.next = Some(at + frames);

        // Already mixed (very late packet): drop what's behind us.
        let mut data: &[f32] = &stereo;
        if at < self.written {
            let skip = (self.written - at).min(frames);
            data = &data[(skip * 2) as usize..];
            at += skip;
        }
        if data.is_empty() {
            return;
        }

        if t.buf.is_empty() {
            t.start = at;
        } else if at < t.end() {
            // Re-placed earlier than buffered data: drop the overlap.
            let keep = ((at - t.start).max(0) * 2) as usize;
            t.buf.truncate(keep);
            if t.buf.is_empty() {
                t.start = at;
            }
        } else if at > t.end() {
            // Gap: keep the buffer contiguous with explicit silence.
            let gap = ((at - t.end()).min(MAX_BUFFER_FRAMES) * 2) as usize;
            t.buf.extend(std::iter::repeat_n(0.0, gap));
        }
        t.buf.extend(data.iter().copied());
        let max_samples = ((MAX_BUFFER_FRAMES as usize) + (frames as usize)) * 2;
        if t.buf.len() > max_samples {
            let excess_frames = (t.buf.len() - max_samples) / 2;
            t.buf.drain(..excess_frames * 2);
            t.start += excess_frames as i64;
        }
    }

    /// Mixes timeline frames `[written, end)` into 16-bit stereo PCM.
    pub fn mix_until(&mut self, end: i64) -> Vec<i16> {
        let count = (end - self.written).max(0);
        let mut mix = vec![0.0f32; (count * 2) as usize];
        let mute_scale = if self.muted { 0.0 } else { 1.0 };
        for t in &mut self.tracks {
            let gain = t.gain * mute_scale;
            for f in 0..count {
                let frame = self.written + f;
                if frame >= t.start && frame < t.end() {
                    let i = ((frame - t.start) * 2) as usize;
                    mix[(f * 2) as usize] += t.buf[i] * gain;
                    mix[(f * 2 + 1) as usize] += t.buf[i + 1] * gain;
                }
            }
            // Drop consumed frames.
            let consumed = (end - t.start).clamp(0, t.buf.len() as i64 / 2);
            t.buf.drain(..(consumed * 2) as usize);
            t.start += consumed;
            if t.buf.is_empty() && t.start < end {
                t.start = end;
            }
        }
        self.written += count;
        mix.into_iter().map(|s| (soft_clip(s) * 32767.0) as i16).collect()
    }
}

/// Gentle limiter: linear below 0.8, smoothly approaching ±1 above.
fn soft_clip(x: f32) -> f32 {
    const KNEE: f32 = 0.8;
    let a = x.abs();
    if a <= KNEE {
        x
    } else {
        let over = (a - KNEE) / (1.0 - KNEE);
        x.signum() * (KNEE + (1.0 - KNEE) * over.tanh())
    }
}

fn to_stereo(samples: &[f32], channels: u16) -> Vec<f32> {
    match channels {
        2 => samples.to_vec(),
        1 => samples.iter().flat_map(|&s| [s, s]).collect(),
        n => samples
            .chunks_exact(n as usize)
            .flat_map(|c| [c[0], c[1]])
            .collect(),
    }
}

/// Linear-interpolating resampler for the rare device that can't deliver
/// 48 kHz itself. Keeps fractional position across calls.
#[derive(Default)]
struct Resampler {
    pos: f64,
    last: [f32; 2],
}

impl Resampler {
    fn process(&mut self, stereo: &[f32], rate: u32) -> Vec<f32> {
        let step = rate as f64 / RATE as f64;
        let frames = stereo.len() / 2;
        let at = |i: isize, c: usize, last: [f32; 2]| if i < 0 { last[c] } else { stereo[i as usize * 2 + c] };
        let mut out = Vec::with_capacity(((frames as f64 / step) as usize + 1) * 2);
        // Positions are relative to the previous call's last frame (index -1).
        while self.pos < frames as f64 - 1.0 {
            let i = self.pos.floor() as isize;
            let frac = (self.pos - i as f64) as f32;
            for c in 0..2 {
                let a = at(i, c, self.last);
                let b = at(i + 1, c, self.last);
                out.push(a + (b - a) * frac);
            }
            self.pos += step;
        }
        if frames > 0 {
            self.last = [stereo[(frames - 1) * 2], stereo[(frames - 1) * 2 + 1]];
            self.pos -= frames as f64;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(frames: usize, v: f32) -> Vec<f32> {
        vec![v; frames * 2]
    }

    #[test]
    fn gaps_become_silence_and_timing_holds() {
        let mut m = Mixer::new(1);
        m.push(0, 0, &tone(480, 0.5), RATE, 2);
        // Nothing for 1 s (silent desktop), then audio again at 1.0 s.
        m.push(0, 48_000, &tone(480, 0.5), RATE, 2);
        let pcm = m.mix_until(48_480);
        assert_eq!(pcm.len(), 48_480 * 2);
        assert!(pcm[0] > 16_000);
        assert_eq!(pcm[24_000 * 2], 0, "gap must be silent");
        assert!(pcm[48_000 * 2] > 16_000, "resumed audio must land at 1.0 s");
        assert_eq!(m.resyncs, 1);
    }

    #[test]
    fn small_jitter_stays_continuous() {
        let mut m = Mixer::new(1);
        m.push(0, 0, &tone(480, 0.25), RATE, 2);
        // Timestamp says 5 ms early (jitter) — keep it continuous.
        m.push(0, 240, &tone(480, 0.25), RATE, 2);
        let pcm = m.mix_until(960);
        assert!(pcm.iter().all(|&s| s > 7_000), "no hole from jitter");
        assert_eq!(m.resyncs, 0);
    }

    #[test]
    fn sources_are_summed_and_limited() {
        let mut m = Mixer::new(2);
        m.push(0, 0, &tone(100, 0.3), RATE, 2);
        m.push(1, 0, &tone(100, 0.3), RATE, 2);
        let pcm = m.mix_until(100);
        assert!((pcm[0] as f32 / 32767.0 - 0.6).abs() < 0.01);

        let mut loud = Mixer::new(2);
        loud.push(0, 0, &tone(100, 0.9), RATE, 2);
        loud.push(1, 0, &tone(100, 0.9), RATE, 2);
        let pcm = loud.mix_until(100);
        assert!(pcm[0] < 32767 && pcm[0] > 26_000, "limited, not wrapped: {}", pcm[0]);
    }

    #[test]
    fn late_audio_is_dropped_not_shifted() {
        let mut m = Mixer::new(1);
        let _ = m.mix_until(1_000);
        m.push(0, 500, &tone(1_000, 0.5), RATE, 2);
        let pcm = m.mix_until(2_000);
        // Frames 500..1000 were already written; 1000..1500 remain.
        assert!(pcm[0] > 16_000);
        assert_eq!(pcm[600 * 2], 0);
    }

    #[test]
    fn mono_and_resampled_input() {
        let mut m = Mixer::new(1);
        m.push(0, 0, &vec![0.5; 44_100], 44_100, 1);
        let pcm = m.mix_until(48_000);
        let filled = pcm.chunks(2).filter(|f| f[0] > 16_000 && f[1] > 16_000).count();
        assert!((47_900..=48_000).contains(&filled), "filled {filled}");
    }

    #[test]
    fn mute_preserves_gain_and_caps_huge_gaps() {
        let mut m = Mixer::new(1);
        m.set_gain(0, 0.5);
        m.set_muted(true);
        m.push(0, 0, &tone(480, 0.8), RATE, 2);
        let muted_pcm = m.mix_until(480);
        assert!(muted_pcm.iter().all(|&s| s == 0), "muted output must be silent");

        m.set_muted(false);
        m.push(0, 480, &tone(480, 0.8), RATE, 2);
        let live_pcm = m.mix_until(960);
        assert!((live_pcm[0] as f32 / 32767.0 - 0.4).abs() < 0.02, "configured gain restored");

        // Anomalous 1-hour timestamp jump must remain bounded.
        m.push(0, RATE as i64 * 3600, &tone(480, 0.5), RATE, 2);
        assert!(m.tracks[0].buf.len() <= (MAX_BUFFER_FRAMES as usize + 480) * 2);
    }
}
