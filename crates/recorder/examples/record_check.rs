//! End-to-end recording check (REC-02/03): tone while recording, a tone
//! during a pause (must be absent), a quiet still stretch, stop after resume.
//! Then decodes the MP4 and checks duration and where audio is audible.
//!
//! `cargo run -p recorder --example record_check [--mic]`

use std::time::Duration;

use recorder::{AudioInput, Recorder, Settings, Target};
use windows::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_MEMORY, SND_NODEFAULT};
use windows::core::PCWSTR;

fn tone_wav(secs: f32, hz: f32) -> Vec<u8> {
    let rate = 48_000u32;
    let n = (rate as f32 * secs) as u32;
    let mut w = Vec::new();
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&(36 + n * 2).to_le_bytes());
    w.extend_from_slice(b"WAVEfmt ");
    w.extend_from_slice(&16u32.to_le_bytes());
    w.extend_from_slice(&1u16.to_le_bytes());
    w.extend_from_slice(&1u16.to_le_bytes());
    w.extend_from_slice(&rate.to_le_bytes());
    w.extend_from_slice(&(rate * 2).to_le_bytes());
    w.extend_from_slice(&2u16.to_le_bytes());
    w.extend_from_slice(&16u16.to_le_bytes());
    w.extend_from_slice(b"data");
    w.extend_from_slice(&(n * 2).to_le_bytes());
    for i in 0..n {
        let v = (2.0 * std::f32::consts::PI * hz * i as f32 / rate as f32).sin() * 0.4;
        w.extend_from_slice(&((v * 32767.0) as i16).to_le_bytes());
    }
    w
}

fn play(wav: &'static [u8]) {
    // SAFETY: complete WAV image in a leaked buffer (outlives playback).
    unsafe {
        let _ = PlaySoundW(PCWSTR(wav.as_ptr() as *const u16), None, SND_MEMORY | SND_ASYNC | SND_NODEFAULT);
    }
}

fn rms(pcm: &[i16], rate: u32, from: f64, to: f64) -> f64 {
    let a = ((from * rate as f64) as usize * 2).min(pcm.len());
    let b = ((to * rate as f64) as usize * 2).min(pcm.len());
    let s = &pcm[a..b];
    (s.iter().map(|&x| (x as f64 / 32768.0).powi(2)).sum::<f64>() / s.len().max(1) as f64).sqrt()
}

fn main() {
    tracing_subscriber::fmt().with_env_filter("info").init();
    let mic = std::env::args().any(|a| a == "--mic");
    let tone: &'static [u8] = Box::leak(tone_wav(1.0, 440.0).into_boxed_slice());

    let primary = platform_windows::monitors::enumerate().into_iter().find(|m| m.primary).unwrap();
    let b = primary.bounds;
    let area = platform_windows::monitors::PixelRect { left: b.left, top: b.top, right: b.left + 1280, bottom: b.top + 720 };
    let output = std::env::temp_dir().join("capshotx-record-check.mp4");
    let _ = std::fs::remove_file(&output);

    let rec = Recorder::start(Settings {
        target: Target::Area(area),
        fps: 30,
        quality: recorder::VideoQuality::Balanced,
        cursor: true,
        desktop: Some(AudioInput { device_id: None, gain: 1.0 }),
        microphone: mic.then_some(AudioInput { device_id: None, gain: 1.0 }),
        output: output.clone(),
    })
    .expect("start");
    for (kind, s) in rec.audio() {
        println!("{kind:?}: {}", s.device_name());
    }

    std::thread::sleep(Duration::from_millis(300));
    play(tone); // audible 0.3–1.3 s
    std::thread::sleep(Duration::from_millis(1700));
    rec.pause(); // at 2.0 s
    play(tone); // must NOT be recorded
    std::thread::sleep(Duration::from_millis(1500));
    rec.resume();
    std::thread::sleep(Duration::from_millis(2000)); // quiet, still screen
    let elapsed = rec.elapsed();
    let summary = rec.stop().expect("stop");
    println!("{summary:#?}\nelapsed at stop ≈ {elapsed:?}");

    let info = media::probe(&summary.path).expect("probe");
    let (pcm, rate, _) = media::decode_audio(&summary.path).expect("decode");
    let secs = pcm.len() as f64 / 2.0 / rate as f64;
    let tone_rms = rms(&pcm, rate, 0.5, 1.2);
    let _pause_rms = rms(&pcm, rate, 2.1, 3.9);
    println!(
        "container {:.3} s, audio {:.3} s, {}x{}\nRMS during tone {:.4}, after pause {:.4}",
        info.seconds(), secs, info.width, info.height, tone_rms, _pause_rms
    );
    let profile: Vec<String> = (0..(secs * 10.0) as usize)
        .map(|i| format!("{:.3}", rms(&pcm, rate, i as f64 / 10.0, (i + 1) as f64 / 10.0)))
        .collect();
    println!("RMS per 100 ms: {}", profile.join(" "));
    let mut ok = true;
    let mut check = |cond: bool, what: &str| {
        println!("{} {what}", if cond { "PASS" } else { "FAIL" });
        ok &= cond;
    };
    check((info.seconds() - 4.0).abs() < 0.15, "duration ≈ 4.0 s (pause excluded)");
    check((secs - info.seconds()).abs() < 0.15, "audio as long as video");
    check(tone_rms > 0.05, "tone before pause is audible");
    // Other desktop sound may be playing; the paused tone would be as loud as
    // the first one, so no window after the pause may come near it.
    let loudest_after = (21..40).map(|i| rms(&pcm, rate, i as f64 / 10.0, (i + 1) as f64 / 10.0)).fold(0.0, f64::max);
    check(loudest_after < tone_rms / 2.0, "tone played while paused is absent");
    // Onset: the tone started 0.3 s in; first window over half level.
    let onset = (0..20).find(|&i| rms(&pcm, rate, i as f64 / 50.0, (i + 1) as f64 / 50.0) > tone_rms / 2.0);
    check(onset.is_some_and(|i| (14..=19).contains(&i)), "tone lands at 0.30 s (±0.1 s)");
    std::process::exit(if ok { 0 } else { 1 });
}
