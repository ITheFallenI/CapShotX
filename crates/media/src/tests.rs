//! Golden-file tests (spec §12.2): write deterministic media, decode it back
//! and check timing and audibility — a non-empty audio stream alone is not
//! proof of audible recording.

use super::*;

const FPS: u32 = 30;
const W: u32 = 320;
const H: u32 = 180;
const RATE: u32 = 48_000;

fn temp(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("capshotx-media-{}-{name}", std::process::id()))
}

/// A moving bar so the encoder has real motion.
fn frame(i: u32) -> Vec<u8> {
    let mut px = vec![0u8; (W * H * 4) as usize];
    let bar = (i * 8) % W;
    for y in 0..H {
        for x in 0..W {
            let o = ((y * W + x) * 4) as usize;
            let on = x >= bar && x < bar + 20;
            px[o..o + 4].copy_from_slice(if on { &[255, 255, 255, 255] } else { &[40, 30, 20, 255] });
        }
    }
    px
}

/// Writes `secs` of video with a 440 Hz tone, the tone only during
/// `tone_from..tone_to` seconds (silence elsewhere).
fn write_clip(path: &std::path::Path, secs: u32, tone: (f32, f32)) {
    let video = VideoConfig { width: W, height: H, fps: FPS, bitrate: 1_000_000 };
    let mut w = Mp4Writer::create(path, video, Some(AudioConfig::default())).expect("create writer");
    eprintln!("fragmented container: {}", w.fragmented);
    let frame_dur = TICKS / FPS as i64;
    for i in 0..secs * FPS {
        w.write_video(&frame(i), i as i64 * frame_dur, frame_dur).unwrap();
    }
    // Audio in 1024-frame chunks, like the recorder.
    let total = (secs * RATE) as usize;
    let mut n = 0;
    while n < total {
        let len = 1024.min(total - n);
        let mut pcm = Vec::with_capacity(len * 2);
        for k in n..n + len {
            let t = k as f32 / RATE as f32;
            let v = if t >= tone.0 && t < tone.1 { (2.0 * std::f32::consts::PI * 440.0 * t).sin() * 0.5 } else { 0.0 };
            let s = (v * 32767.0) as i16;
            pcm.extend_from_slice(&[s, s]);
        }
        w.write_audio(&pcm, n as i64 * TICKS / RATE as i64).unwrap();
        n += len;
    }
    w.finish().unwrap();
}

fn rms(pcm: &[i16]) -> f64 {
    (pcm.iter().map(|&s| (s as f64 / 32768.0).powi(2)).sum::<f64>() / pcm.len().max(1) as f64).sqrt()
}

#[test]
fn mp4_round_trip_keeps_duration_and_audible_audio() {
    let path = temp("roundtrip.mp4");
    write_clip(&path, 3, (1.0, 2.0));

    let info = probe(&path).unwrap();
    assert!(info.has_video && info.has_audio, "{info:?}");
    assert_eq!((info.width, info.height), (W, H));
    // Spec: duration within one frame plus container rounding.
    assert!((info.seconds() - 3.0).abs() <= 1.0 / FPS as f64 + 0.03, "duration {}", info.seconds());

    let (pcm, rate, channels) = decode_audio(&path).unwrap();
    assert_eq!((rate, channels), (RATE, 2));
    let decoded_secs = pcm.len() as f64 / (rate as f64 * channels as f64);
    assert!((decoded_secs - 3.0).abs() < 0.1, "decoded {decoded_secs}s");

    // The tone is where we put it, and silence is silent (AAC has ~20 ms of
    // priming/latency, so sample well inside each region).
    let at = |a: f64, b: f64| {
        let (s, e) = ((a * rate as f64) as usize * 2, (b * rate as f64) as usize * 2);
        rms(&pcm[s.min(pcm.len())..e.min(pcm.len())])
    };
    let (before, during, after) = (at(0.2, 0.8), at(1.2, 1.8), at(2.2, 2.8));
    assert!(during > 0.25, "tone rms {during}");
    assert!(before < 0.01 && after < 0.01, "silence rms {before} / {after}");

    let (pw, ph, rgba) = poster_frame(&path).unwrap();
    assert_eq!((pw, ph), (W, H));
    assert_eq!(rgba.len(), (W * H * 4) as usize);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn sparse_video_frames_still_give_full_duration() {
    // Static screen: only a few frames, but timestamps cover 4 s. Duration
    // must follow the timeline, not the count of frames (REC-03).
    let path = temp("sparse.mp4");
    let video = VideoConfig { width: W, height: H, fps: FPS, bitrate: 500_000 };
    let mut w = Mp4Writer::create(&path, video, None).unwrap();
    let f = frame(0);
    for (t, d) in [(0.0, 1.0), (1.0, 2.5), (3.5, 0.5)] {
        w.write_video(&f, (t * TICKS as f64) as i64, (d * TICKS as f64) as i64).unwrap();
    }
    w.finish().unwrap();
    let info = probe(&path).unwrap();
    assert!((info.seconds() - 4.0).abs() < 0.1, "duration {}", info.seconds());
    let _ = std::fs::remove_file(&path);
}
