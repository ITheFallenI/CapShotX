//! Session timeline: maps the monotonic media clock to "recording time",
//! which excludes paused intervals (REC-03).

/// All times are 100 ns ticks on the QPC media clock.
#[derive(Clone, Debug)]
pub struct SessionClock {
    start: i64,
    /// Closed and (at most one trailing) open pause intervals.
    pauses: Vec<(i64, Option<i64>)>,
}

impl SessionClock {
    pub fn new(start: i64) -> Self {
        Self { start, pauses: Vec::new() }
    }

    pub fn is_paused(&self) -> bool {
        matches!(self.pauses.last(), Some((_, None)))
    }

    pub fn pause(&mut self, now: i64) {
        if !self.is_paused() {
            self.pauses.push((now, None));
        }
    }

    pub fn resume(&mut self, now: i64) {
        if let Some((_, end @ None)) = self.pauses.last_mut() {
            *end = Some(now);
        }
    }

    /// Recording time of media-clock instant `t`, or `None` if `t` falls
    /// before the start or inside a pause (such audio/video is discarded).
    pub fn active_at(&self, t: i64) -> Option<i64> {
        if t < self.start {
            return None;
        }
        let mut paused = 0;
        for &(s, e) in &self.pauses {
            let e = e.unwrap_or(i64::MAX);
            if t >= s && t < e {
                return None;
            }
            if t >= e {
                paused += e - s;
            }
        }
        Some(t - self.start - paused)
    }

    /// Recording time "now": while paused it stays where the pause began.
    pub fn elapsed(&self, now: i64) -> i64 {
        match self.pauses.last() {
            Some(&(s, None)) => self.active_at(s - 1).map_or(0, |t| t + 1),
            _ => self.active_at(now).unwrap_or(0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn excludes_pauses() {
        let mut c = SessionClock::new(1_000);
        assert_eq!(c.active_at(999), None);
        assert_eq!(c.active_at(1_500), Some(500));
        c.pause(2_000);
        assert!(c.is_paused());
        assert_eq!(c.active_at(2_500), None);
        assert_eq!(c.elapsed(9_999), 1_000);
        c.resume(5_000);
        assert_eq!(c.active_at(5_000), Some(1_000));
        assert_eq!(c.active_at(6_000), Some(2_000));
        assert_eq!(c.elapsed(6_000), 2_000);
    }

    #[test]
    fn stop_while_paused_keeps_duration_at_pause() {
        let mut c = SessionClock::new(0);
        c.pause(3_000);
        // Spec scenario: stopping while paused must not include the pause.
        assert_eq!(c.elapsed(50_000), 3_000);
    }
}
