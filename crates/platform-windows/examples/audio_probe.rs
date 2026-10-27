//! Lists audio devices and checks loopback really captures sound by playing
//! a quiet 440 Hz tone. `cargo run -p platform-windows --example audio_probe`

use std::time::{Duration, Instant};

use platform_windows::audio::{CaptureStream, Direction, Packet, init_thread, list_devices};
use windows::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_MEMORY};
use windows::core::PCWSTR;

fn main() {
    init_thread();
    for dir in [Direction::Output, Direction::Input] {
        println!("{dir:?} devices:");
        for d in list_devices(dir).unwrap_or_default() {
            println!("  {} {}", if d.is_default { "*" } else { " " }, d.name);
        }
    }

    let wav = tone_wav(440.0, 1.2, 0.15);
    let loopback = CaptureStream::open(Direction::Output, None).expect("open loopback");
    println!("loopback: {} {:?}", loopback.device_name, loopback.format);
    loopback.start().unwrap();
    // SAFETY: wav outlives playback (we sleep past its end below).
    unsafe { let _ = PlaySoundW(PCWSTR(wav.as_ptr() as *const u16), None, SND_MEMORY | SND_ASYNC); }
    report("loopback", &loopback, Duration::from_millis(1500));

    match CaptureStream::open(Direction::Input, None) {
        Ok(mic) => {
            println!("mic: {} {:?}", mic.device_name, mic.format);
            mic.start().unwrap();
            report("mic", &mic, Duration::from_millis(1000));
        }
        Err(e) => println!("mic: unavailable ({e})"),
    }
}

fn report(label: &str, stream: &CaptureStream, dur: Duration) {
    let started = Instant::now();
    let mut packets: Vec<Packet> = Vec::new();
    while started.elapsed() < dur {
        if let Err(e) = stream.read(&mut packets) { println!("{label}: read error {e:?}"); return; }
        std::thread::sleep(Duration::from_millis(10));
    }
    let samples: Vec<f32> = packets.iter().flat_map(|p| p.samples.iter().copied()).collect();
    let peak = samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    let secs = samples.len() as f32 / (stream.format.rate as f32 * stream.format.channels as f32);
    let (t0, t1) = (packets.first().map(|p| p.time), packets.last().map(|p| p.time));
    println!(
        "{label}: {} packets, {secs:.2}s of audio, peak {peak:.3}, timestamps span {:?} ms",
        packets.len(),
        t0.zip(t1).map(|(a, b)| (b - a) / 10_000)
    );
}

fn tone_wav(freq: f32, secs: f32, amp: f32) -> Vec<u8> {
    let rate = 48_000u32;
    let n = (rate as f32 * secs) as usize;
    let mut data = Vec::with_capacity(n * 2);
    for i in 0..n {
        let t = i as f32 / rate as f32;
        let fade = (t / 0.02).min(1.0).min((secs - t) / 0.02);
        let v = (2.0 * std::f32::consts::PI * freq * t).sin() * amp * fade;
        data.extend_from_slice(&((v * 32767.0) as i16).to_le_bytes());
    }
    let mut w = Vec::new();
    w.extend_from_slice(b"RIFF"); w.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
    w.extend_from_slice(b"WAVEfmt "); w.extend_from_slice(&16u32.to_le_bytes());
    w.extend_from_slice(&1u16.to_le_bytes()); w.extend_from_slice(&1u16.to_le_bytes());
    w.extend_from_slice(&rate.to_le_bytes()); w.extend_from_slice(&(rate * 2).to_le_bytes());
    w.extend_from_slice(&2u16.to_le_bytes()); w.extend_from_slice(&16u16.to_le_bytes());
    w.extend_from_slice(b"data"); w.extend_from_slice(&(data.len() as u32).to_le_bytes());
    w.extend_from_slice(&data);
    w
}
