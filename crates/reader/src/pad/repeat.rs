//! Held directions repeat, as keys do: the d-pad, and a stick pushed far
//! enough to count as one.

use std::time::{Duration, Instant};

use egui::Vec2;

/// The wait before a held direction repeats.
pub const FIRST_DELAY: Duration = Duration::from_millis(350);
/// The wait between repeats.
pub const RATE: Duration = Duration::from_millis(80);
/// How far a stick goes before it counts as a direction, and how far back
/// it comes before it lets go: the gap keeps it from chattering.
const ENGAGE: f32 = 0.6;
const RELEASE: f32 = 0.4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    Up,
    Down,
    Left,
    Right,
}

/// A stick as a direction: its stronger axis once it is pushed far
/// enough. `held` is the direction it gave last frame.
pub fn stick_dir(stick: Vec2, held: Option<Dir>) -> Option<Dir> {
    let along = |dir| match dir {
        Dir::Up => -stick.y,
        Dir::Down => stick.y,
        Dir::Left => -stick.x,
        Dir::Right => stick.x,
    };
    if let Some(dir) = held
        && along(dir) > RELEASE
    {
        return Some(dir);
    }
    let dir = if stick.x.abs() > stick.y.abs() {
        if stick.x > 0.0 { Dir::Right } else { Dir::Left }
    } else if stick.y > 0.0 {
        Dir::Down
    } else {
        Dir::Up
    };
    (along(dir) > ENGAGE).then_some(dir)
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Repeat {
    /// The direction held and when it fires next.
    held: Option<(Dir, Instant)>,
}

impl Repeat {
    /// The direction held now. Returns it when it fires this frame (as it
    /// is first held, then after [`FIRST_DELAY`] every [`RATE`]), and the
    /// wait before it fires again.
    pub fn update(&mut self, dir: Option<Dir>, now: Instant) -> (Option<Dir>, Option<Duration>) {
        let Some(dir) = dir else {
            self.held = None;
            return (None, None);
        };
        match self.held {
            Some((held, next)) if held == dir => {
                if now >= next {
                    self.held = Some((dir, now + RATE));
                    (Some(dir), Some(RATE))
                } else {
                    (None, Some(next - now))
                }
            }
            _ => {
                self.held = Some((dir, now + FIRST_DELAY));
                (Some(dir), Some(FIRST_DELAY))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_held_direction_fires_then_waits_then_repeats() {
        let start = Instant::now();
        let mut repeat = Repeat::default();
        let at = |ms| start + Duration::from_millis(ms);
        assert_eq!(repeat.update(Some(Dir::Down), at(0)).0, Some(Dir::Down));
        assert_eq!(repeat.update(Some(Dir::Down), at(100)).0, None);
        assert_eq!(
            repeat.update(Some(Dir::Down), at(200)).1,
            Some(Duration::from_millis(150)),
            "it says when to wake for the next repeat"
        );
        assert_eq!(repeat.update(Some(Dir::Down), at(350)).0, Some(Dir::Down));
        assert_eq!(repeat.update(Some(Dir::Down), at(400)).0, None);
        assert_eq!(repeat.update(Some(Dir::Down), at(430)).0, Some(Dir::Down));
        assert_eq!(
            repeat.update(Some(Dir::Left), at(440)).0,
            Some(Dir::Left),
            "a new direction fires at once"
        );
        assert_eq!(repeat.update(None, at(450)), (None, None));
        assert_eq!(repeat.update(Some(Dir::Left), at(460)).0, Some(Dir::Left));
    }

    #[test]
    fn sticks_engage_far_out_and_let_go_nearer_in() {
        assert_eq!(stick_dir(egui::vec2(0.5, 0.0), None), None);
        assert_eq!(stick_dir(egui::vec2(0.7, 0.2), None), Some(Dir::Right));
        assert_eq!(
            stick_dir(egui::vec2(0.5, 0.0), Some(Dir::Right)),
            Some(Dir::Right),
            "it holds until it comes back past the release point"
        );
        assert_eq!(stick_dir(egui::vec2(0.3, 0.0), Some(Dir::Right)), None);
        assert_eq!(stick_dir(egui::vec2(0.1, -0.9), None), Some(Dir::Up));
        assert_eq!(stick_dir(egui::vec2(0.0, 0.65), None), Some(Dir::Down));
    }
}
