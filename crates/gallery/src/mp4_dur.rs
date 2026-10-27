//! Pure-Rust ISO BMFF / MP4 duration parser (standard `moov` + fragmented `moof`).
//!
//! Avoids initialising Media Foundation (`Windows.Media.dll`, `media_bin_64.dll`
//! and hardware DXVA drivers — ~25–40 MB + worker threads) just to show a
//! duration badge on cached video thumbnails in Home and Gallery.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::time::Duration;

const MAX_MOOV_BYTES: u64 = 8 * 1024 * 1024;
const MAX_MOOF_BYTES: u64 = 1024 * 1024;

#[derive(Default)]
struct TrackMeta {
    timescale: u32,
    mdhd_duration: u64,
    default_sample_duration: u32,
    frag_ticks: u64,
}

/// Reads an MP4 or fragmented-MP4 container's duration without loading codecs.
pub fn parse(path: &Path) -> Option<Duration> {
    let mut file = File::open(path).ok()?;
    let file_len = file.metadata().ok()?.len();
    if file_len < 16 {
        return None;
    }

    let mut movie_timescale: u32 = 0;
    let mut tracks: HashMap<u32, TrackMeta> = HashMap::new();
    let mut moof_offsets: Vec<(u64, u64)> = Vec::new();
    let mut buf = Vec::new();

    let mut pos = 0u64;
    while pos + 8 <= file_len {
        file.seek(SeekFrom::Start(pos)).ok()?;
        let (fourcc, payload_pos, box_end) = read_box_header(&mut file, pos, file_len)?;
        let payload_len = box_end.saturating_sub(payload_pos);

        match &fourcc {
            b"moov" if payload_len <= MAX_MOOV_BYTES => {
                buf.resize(payload_len as usize, 0);
                file.read_exact(&mut buf).ok()?;
                if let Some(dur) = parse_moov(&buf, &mut movie_timescale, &mut tracks) {
                    return Some(dur);
                }
            }
            b"moof" if payload_len <= MAX_MOOF_BYTES => {
                moof_offsets.push((payload_pos, payload_len));
            }
            _ => {}
        }
        pos = box_end;
    }

    // Fragmented MP4 (FMPEG4): sum sample durations across `moof` headers while
    // skipping `mdat` media payloads entirely.
    if tracks.is_empty() || moof_offsets.is_empty() {
        return None;
    }
    for (payload_pos, payload_len) in moof_offsets {
        file.seek(SeekFrom::Start(payload_pos)).ok()?;
        buf.resize(payload_len as usize, 0);
        file.read_exact(&mut buf).ok()?;
        parse_moof(&buf, &mut tracks);
    }

    tracks
        .values()
        .filter_map(|t| ticks_to_duration(t.frag_ticks, t.timescale))
        .max()
}

fn read_box_header(file: &mut File, pos: u64, file_len: u64) -> Option<([u8; 4], u64, u64)> {
    let mut hdr = [0u8; 8];
    file.read_exact(&mut hdr).ok()?;
    let size32 = u32::from_be_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]);
    let fourcc = [hdr[4], hdr[5], hdr[6], hdr[7]];

    let (header_len, box_end) = match size32 {
        0 => (8u64, file_len),
        1 => {
            if pos + 16 > file_len {
                return None;
            }
            let mut ext = [0u8; 8];
            file.read_exact(&mut ext).ok()?;
            let size64 = u64::from_be_bytes(ext);
            if size64 < 16 {
                return None;
            }
            (16u64, pos.checked_add(size64)?)
        }
        n if n < 8 => return None,
        n => (8u64, pos.checked_add(n as u64)?),
    };
    if box_end > file_len {
        return None;
    }
    Some((fourcc, pos + header_len, box_end))
}

fn iter_boxes(mut slice: &[u8]) -> impl Iterator<Item = ([u8; 4], &[u8])> {
    std::iter::from_fn(move || {
        if slice.len() < 8 {
            slice = &[];
            return None;
        }
        let size32 = u32::from_be_bytes([slice[0], slice[1], slice[2], slice[3]]);
        let fourcc = [slice[4], slice[5], slice[6], slice[7]];
        let (hdr, total) = match size32 {
            0 => (8usize, slice.len()),
            1 => {
                if slice.len() < 16 {
                    slice = &[];
                    return None;
                }
                let size64 = usize::try_from(u64::from_be_bytes(slice[8..16].try_into().ok()?)).ok()?;
                if size64 < 16 || size64 > slice.len() {
                    slice = &[];
                    return None;
                }
                (16usize, size64)
            }
            n if (n as usize) < 8 || (n as usize) > slice.len() => {
                slice = &[];
                return None;
            }
            n => (8usize, n as usize),
        };
        let payload = &slice[hdr..total];
        slice = &slice[total..];
        Some((fourcc, payload))
    })
}

fn parse_moov(
    moov: &[u8],
    movie_timescale: &mut u32,
    tracks: &mut HashMap<u32, TrackMeta>,
) -> Option<Duration> {
    let mut mvhd_dur: Option<u64> = None;
    let mut mehd_dur: Option<u64> = None;

    for (fourcc, body) in iter_boxes(moov) {
        match &fourcc {
            b"mvhd" => {
                if let Some((ts, dur)) = parse_mvhd_or_mdhd(body) {
                    *movie_timescale = ts;
                    if dur > 0 {
                        mvhd_dur = Some(dur);
                    }
                }
            }
            b"mvex" => {
                for (sub, sbody) in iter_boxes(body) {
                    match &sub {
                        b"mehd" => mehd_dur = parse_mehd(sbody).filter(|&d| d > 0),
                        b"trex" if sbody.len() >= 20 => {
                            let track_id = u32::from_be_bytes(sbody[4..8].try_into().unwrap());
                            let def_dur = u32::from_be_bytes(sbody[12..16].try_into().unwrap());
                            tracks.entry(track_id).or_default().default_sample_duration = def_dur;
                        }
                        _ => {}
                    }
                }
            }
            b"trak" => {
                let mut track_id = None;
                let mut mdhd = None;
                for (sub, sbody) in iter_boxes(body) {
                    match &sub {
                        b"tkhd" => track_id = parse_tkhd_id(sbody),
                        b"mdia" => {
                            for (msub, mbody) in iter_boxes(sbody) {
                                if &msub == b"mdhd" {
                                    mdhd = parse_mvhd_or_mdhd(mbody);
                                }
                            }
                        }
                        _ => {}
                    }
                }
                if let (Some(id), Some((ts, dur))) = (track_id, mdhd) {
                    let meta = tracks.entry(id).or_default();
                    meta.timescale = ts;
                    meta.mdhd_duration = dur;
                }
            }
            _ => {}
        }
    }

    if let Some(d) = mvhd_dur.and_then(|d| ticks_to_duration(d, *movie_timescale)) {
        return Some(d);
    }
    if let Some(d) = mehd_dur.and_then(|d| ticks_to_duration(d, *movie_timescale)) {
        return Some(d);
    }
    tracks
        .values()
        .filter_map(|t| ticks_to_duration(t.mdhd_duration, t.timescale))
        .max()
}

fn parse_mvhd_or_mdhd(body: &[u8]) -> Option<(u32, u64)> {
    let &version = body.first()?;
    match version {
        0 if body.len() >= 20 => {
            let ts = u32::from_be_bytes(body[12..16].try_into().ok()?);
            let dur = u32::from_be_bytes(body[16..20].try_into().ok()?);
            Some((ts, if dur == u32::MAX { 0 } else { dur as u64 }))
        }
        1 if body.len() >= 32 => {
            let ts = u32::from_be_bytes(body[20..24].try_into().ok()?);
            let dur = u64::from_be_bytes(body[24..32].try_into().ok()?);
            Some((ts, if dur == u64::MAX { 0 } else { dur }))
        }
        _ => None,
    }
}

fn parse_mehd(body: &[u8]) -> Option<u64> {
    let &version = body.first()?;
    match version {
        0 if body.len() >= 8 => {
            let d = u32::from_be_bytes(body[4..8].try_into().ok()?);
            Some(if d == u32::MAX { 0 } else { d as u64 })
        }
        1 if body.len() >= 12 => {
            let d = u64::from_be_bytes(body[4..12].try_into().ok()?);
            Some(if d == u64::MAX { 0 } else { d })
        }
        _ => None,
    }
}

fn parse_tkhd_id(body: &[u8]) -> Option<u32> {
    let &version = body.first()?;
    match version {
        0 if body.len() >= 16 => Some(u32::from_be_bytes(body[12..16].try_into().ok()?)),
        1 if body.len() >= 24 => Some(u32::from_be_bytes(body[20..24].try_into().ok()?)),
        _ => None,
    }
}

fn parse_moof(moof: &[u8], tracks: &mut HashMap<u32, TrackMeta>) {
    for (fourcc, traf) in iter_boxes(moof) {
        if &fourcc != b"traf" {
            continue;
        }
        let mut track_id: Option<u32> = None;
        let mut tfhd_default_dur: Option<u32> = None;

        for (sub, body) in iter_boxes(traf) {
            if &sub == b"tfhd" && body.len() >= 8 {
                let flags = u32::from_be_bytes([0, body[1], body[2], body[3]]);
                track_id = Some(u32::from_be_bytes(body[4..8].try_into().unwrap()));
                let mut off = 8usize;
                if flags & 0x000001 != 0 {
                    off += 8;
                }
                if flags & 0x000002 != 0 {
                    off += 4;
                }
                if flags & 0x000008 != 0 && off + 4 <= body.len() {
                    tfhd_default_dur = Some(u32::from_be_bytes(body[off..off + 4].try_into().unwrap()));
                }
            }
        }

        let Some(id) = track_id else { continue };
        let Some(track) = tracks.get_mut(&id) else { continue };
        let default_dur = tfhd_default_dur.unwrap_or(track.default_sample_duration);

        for (sub, body) in iter_boxes(traf) {
            if &sub == b"trun" && body.len() >= 8 {
                let flags = u32::from_be_bytes([0, body[1], body[2], body[3]]);
                let count = u32::from_be_bytes(body[4..8].try_into().unwrap());
                let mut off = 8usize;
                if flags & 0x000001 != 0 {
                    off += 4;
                }
                if flags & 0x000004 != 0 {
                    off += 4;
                }
                if flags & 0x000100 != 0 {
                    let stride = 4
                        + if flags & 0x000200 != 0 { 4 } else { 0 }
                        + if flags & 0x000400 != 0 { 4 } else { 0 }
                        + if flags & 0x000800 != 0 { 4 } else { 0 };
                    for _ in 0..count {
                        if off + stride > body.len() {
                            break;
                        }
                        let d = u32::from_be_bytes(body[off..off + 4].try_into().unwrap());
                        track.frag_ticks = track.frag_ticks.saturating_add(d as u64);
                        off += stride;
                    }
                } else {
                    track.frag_ticks = track
                        .frag_ticks
                        .saturating_add(count as u64 * default_dur as u64);
                }
            }
        }
    }
}

fn ticks_to_duration(ticks: u64, timescale: u32) -> Option<Duration> {
    if ticks == 0 || timescale == 0 {
        return None;
    }
    let nanos = (ticks as u128 * 1_000_000_000) / timescale as u128;
    u64::try_from(nanos).ok().filter(|&n| n > 0).map(Duration::from_nanos)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mp4_box(fourcc: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let len = (8 + payload.len()) as u32;
        let mut out = Vec::with_capacity(len as usize);
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(fourcc);
        out.extend_from_slice(payload);
        out
    }

    #[test]
    fn parses_standard_mvhd_and_fragmented_moof() {
        let dir = std::env::temp_dir().join(format!("capshotx-mp4dur-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);

        // 1. Standard MP4 with mvhd v0 (timescale 1000, duration 4500 = 4.5s).
        let std_path = dir.join("std.mp4");
        let mut mvhd = vec![0u8; 20];
        mvhd[12..16].copy_from_slice(&1000u32.to_be_bytes());
        mvhd[16..20].copy_from_slice(&4500u32.to_be_bytes());
        let mut file_bytes = mp4_box(b"ftyp", b"isom\0\0\x02\0");
        file_bytes.extend_from_slice(&mp4_box(b"moov", &mp4_box(b"mvhd", &mvhd)));
        std::fs::write(&std_path, &file_bytes).unwrap();
        assert_eq!(parse(&std_path), Some(Duration::from_millis(4500)));

        // 2. Fragmented MP4 with 0 duration in mvhd/mdhd and 3 samples × 500 ticks in moof/trun.
        let frag_path = dir.join("frag.mp4");
        let mut mvhd0 = vec![0u8; 20];
        mvhd0[12..16].copy_from_slice(&1000u32.to_be_bytes());
        let mut tkhd = vec![0u8; 16];
        tkhd[12..16].copy_from_slice(&1u32.to_be_bytes());
        let mut mdhd = vec![0u8; 20];
        mdhd[12..16].copy_from_slice(&1000u32.to_be_bytes());
        let trak = {
            let mut b = mp4_box(b"tkhd", &tkhd);
            b.extend_from_slice(&mp4_box(b"mdia", &mp4_box(b"mdhd", &mdhd)));
            mp4_box(b"trak", &b)
        };
        let mut trex = vec![0u8; 20];
        trex[4..8].copy_from_slice(&1u32.to_be_bytes());
        trex[12..16].copy_from_slice(&500u32.to_be_bytes());
        let moov = {
            let mut b = mp4_box(b"mvhd", &mvhd0);
            b.extend_from_slice(&trak);
            b.extend_from_slice(&mp4_box(b"mvex", &mp4_box(b"trex", &trex)));
            mp4_box(b"moov", &b)
        };
        let mut tfhd = vec![0u8; 8];
        tfhd[4..8].copy_from_slice(&1u32.to_be_bytes());
        let mut trun = vec![0u8; 8];
        trun[4..8].copy_from_slice(&3u32.to_be_bytes());
        let traf = {
            let mut b = mp4_box(b"tfhd", &tfhd);
            b.extend_from_slice(&mp4_box(b"trun", &trun));
            mp4_box(b"traf", &b)
        };
        let mut frag_bytes = mp4_box(b"ftyp", b"mp42\0\0\0\0");
        frag_bytes.extend_from_slice(&moov);
        frag_bytes.extend_from_slice(&mp4_box(b"moof", &traf));
        frag_bytes.extend_from_slice(&mp4_box(b"mdat", &[0u8; 64]));
        std::fs::write(&frag_path, &frag_bytes).unwrap();
        assert_eq!(parse(&frag_path), Some(Duration::from_millis(1500)));

        // 3. Real Media Foundation Mp4Writer file: verify pure-Rust parser matches probe!
        let mf_path = dir.join("mf.mp4");
        let mut writer = media::Mp4Writer::create(
            &mf_path,
            media::VideoConfig { width: 64, height: 64, fps: 30, bitrate: 200_000 },
            None,
        )
        .unwrap();
        let frame = vec![0u8; 64 * 64 * 4];
        for i in 0..30i64 {
            writer.write_video(&frame, i * 333_333, 333_333).unwrap();
        }
        writer.finish().unwrap();
        let pure_dur = parse(&mf_path).expect("pure-Rust parser must read MF Mp4Writer output");
        assert!(
            pure_dur >= Duration::from_millis(900) && pure_dur <= Duration::from_millis(1100),
            "unexpected MF duration: {pure_dur:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
