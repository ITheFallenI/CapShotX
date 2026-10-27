//! The monotonic media clock: QPC in 100 ns units, the same timebase
//! WASAPI uses for packet timestamps and WGC for frame times.

use std::sync::OnceLock;

use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};

/// 100 ns ticks per second.
pub const TICKS_PER_SECOND: i64 = 10_000_000;

/// Current time in 100 ns units (arbitrary epoch, monotonic).
pub fn now() -> i64 {
    static FREQ: OnceLock<i64> = OnceLock::new();
    let freq = *FREQ.get_or_init(|| {
        let mut f = 0;
        // SAFETY: valid out-pointer; never fails on XP+.
        let _ = unsafe { QueryPerformanceFrequency(&mut f) };
        f.max(1)
    });
    let mut count = 0;
    // SAFETY: valid out-pointer.
    let _ = unsafe { QueryPerformanceCounter(&mut count) };
    // Split to avoid overflow: whole seconds, then remainder.
    (count / freq) * TICKS_PER_SECOND + (count % freq) * TICKS_PER_SECOND / freq
}
