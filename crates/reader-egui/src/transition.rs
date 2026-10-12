//! Timing of view transitions: tab switches, opening a document and
//! presentation page turns. The pixels come from a transition effect.

use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub struct ViewTransition {
    pub effect: String,
    /// +1 forward (next page, a tab to the right), -1 backward.
    pub direction: f32,
    started: Instant,
    duration: Duration,
    /// The tab whose last image is the outgoing view; taken on the first
    /// frame, when the snapshot is made.
    capture_from: Option<u64>,
}

impl ViewTransition {
    pub fn new(
        effect: String,
        from_tab: u64,
        direction: f32,
        duration_ms: u32,
        now: Instant,
    ) -> Self {
        Self {
            effect,
            direction,
            started: now,
            duration: Duration::from_millis(u64::from(duration_ms.max(1))),
            capture_from: Some(from_tab),
        }
    }

    /// Progress from 0 to 1.
    pub fn progress(&self, now: Instant) -> f32 {
        (now.saturating_duration_since(self.started).as_secs_f32() / self.duration.as_secs_f32())
            .clamp(0.0, 1.0)
    }

    pub fn finished(&self, now: Instant) -> bool {
        self.progress(now) >= 1.0
    }

    /// The tab to snapshot, on the first call only.
    pub fn take_capture(&mut self) -> Option<u64> {
        self.capture_from.take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_runs_over_the_duration_and_the_capture_happens_once() {
        let start = Instant::now();
        let mut transition = ViewTransition::new("slide-over".into(), 7, 1.0, 300, start);
        assert_eq!(transition.progress(start), 0.0);
        assert!((transition.progress(start + Duration::from_millis(150)) - 0.5).abs() < 1e-3);
        assert!(transition.finished(start + Duration::from_millis(300)));
        assert_eq!(transition.take_capture(), Some(7));
        assert_eq!(transition.take_capture(), None);
    }
}
