//! Timing of the presentation-mode page transition (the shader is the
//! composite pass in `spike.wgsl`).

use std::time::{Duration, Instant};

pub const DURATION: Duration = Duration::from_millis(300);

#[derive(Clone, Copy, Debug)]
pub struct Transition {
    pub from: usize,
    pub to: usize,
    started: Instant,
}

impl Transition {
    pub fn new(from: usize, to: usize, now: Instant) -> Self {
        Self {
            from,
            to,
            started: now,
        }
    }

    /// Progress from 0 to 1.
    pub fn progress(&self, now: Instant) -> f32 {
        (now.saturating_duration_since(self.started).as_secs_f32() / DURATION.as_secs_f32())
            .clamp(0.0, 1.0)
    }

    /// +1 when the incoming page comes from the right (moving forward).
    pub fn direction(&self) -> f32 {
        if self.to >= self.from { 1.0 } else { -1.0 }
    }

    pub fn finished(&self, now: Instant) -> bool {
        self.progress(now) >= 1.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_runs_over_the_duration_in_either_direction() {
        let start = Instant::now();
        let forward = Transition::new(2, 3, start);
        assert_eq!(forward.progress(start), 0.0);
        assert!((forward.progress(start + DURATION / 2) - 0.5).abs() < 1e-3);
        assert!(forward.finished(start + DURATION));
        assert_eq!(forward.direction(), 1.0);
        assert_eq!(Transition::new(3, 2, start).direction(), -1.0);
    }
}
