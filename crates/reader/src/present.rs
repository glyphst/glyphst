//! What presenting keeps beside the page (frames 02g–i): the pointer
//! (hidden, or the laser), the tool in use, ink on each page, the blank
//! screen, the tool ring's gesture and the clock. `app/present.rs` takes
//! the input and draws them.

use std::{
    collections::{HashMap, VecDeque},
    time::{Duration, Instant},
};

use egui::{Color32, Pos2};

use crate::commands::Command;

/// The pen's colours in the toolbox's order; red is the default.
pub const INK: [(&str, Color32); 5] = [
    ("red", Color32::from_rgb(0xE5, 0x48, 0x4D)),
    ("blue", Color32::from_rgb(0x3E, 0x7B, 0xFA)),
    ("green", Color32::from_rgb(0x2F, 0xA3, 0x5A)),
    ("amber", Color32::from_rgb(0xFF, 0xB0, 0x00)),
    ("black", Color32::from_rgb(0x1E, 0x21, 0x25)),
];
/// The highlighter's yellow, at 40%.
pub const HIGHLIGHTER: Color32 = Color32::from_rgba_premultiplied(102, 86, 4, 102);
pub const LASER: Color32 = Color32::from_rgb(0xFF, 0x3B, 0x30);
/// Widths in page points.
pub const PEN_WIDTH: f32 = 2.0;
pub const HIGHLIGHTER_WIDTH: f32 = 10.0;
/// The eraser's radius in screen points.
pub const ERASER_RADIUS: f32 = 12.0;
/// How long the laser's trail lasts.
pub const TRAIL: Duration = Duration::from_millis(300);

/// The pointer while presenting: hidden (the default; nothing hovers), or
/// shown as the laser.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Pointer {
    #[default]
    Hidden,
    Laser,
}

/// What a primary drag on the page does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    Pen,
    Highlighter,
    Eraser,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Blank {
    Black,
    White,
}

/// A line drawn on a page, in page points so it stays put through zooms
/// and resizes.
#[derive(Clone, Debug, PartialEq)]
pub struct Stroke {
    pub points: Vec<[f32; 2]>,
    pub color: Color32,
    /// In page points.
    pub width: f32,
}

/// A document's ink, by page; kept while its tab is open, never saved.
#[derive(Clone, Debug, Default)]
pub struct Ink {
    pages: HashMap<usize, Vec<Stroke>>,
}

impl Ink {
    pub fn page(&self, page: usize) -> &[Stroke] {
        self.pages.get(&page).map_or(&[], Vec::as_slice)
    }

    pub fn start(&mut self, page: usize, stroke: Stroke) {
        self.pages.entry(page).or_default().push(stroke);
    }

    /// Adds `point` to the page's latest stroke, unless it is within
    /// `min` page points of its last point.
    pub fn extend(&mut self, page: usize, point: [f32; 2], min: f32) {
        let Some(stroke) = self
            .pages
            .get_mut(&page)
            .and_then(|strokes| strokes.last_mut())
        else {
            return;
        };
        if stroke
            .points
            .last()
            .is_none_or(|last| distance(*last, point) >= min)
        {
            stroke.points.push(point);
        }
    }

    /// Removes the page's latest stroke; whether there was one.
    pub fn undo(&mut self, page: usize) -> bool {
        self.pages
            .get_mut(&page)
            .and_then(|strokes| strokes.pop())
            .is_some()
    }

    /// Removes the page's strokes; whether it had any.
    pub fn clear(&mut self, page: usize) -> bool {
        self.pages
            .remove(&page)
            .is_some_and(|strokes| !strokes.is_empty())
    }

    /// Removes the page's strokes that pass within `radius` page points of
    /// `point`, as whole strokes; whether any went.
    pub fn erase(&mut self, page: usize, point: [f32; 2], radius: f32) -> bool {
        let Some(strokes) = self.pages.get_mut(&page) else {
            return false;
        };
        let before = strokes.len();
        strokes.retain(|stroke| !touches(stroke, point, radius));
        strokes.len() != before
    }
}

fn distance(a: [f32; 2], b: [f32; 2]) -> f32 {
    (a[0] - b[0]).hypot(a[1] - b[1])
}

/// Whether `stroke` passes within `radius` of `point`, counting its width.
fn touches(stroke: &Stroke, point: [f32; 2], radius: f32) -> bool {
    let reach = radius + stroke.width / 2.0;
    match stroke.points.as_slice() {
        [] => false,
        [only] => distance(*only, point) <= reach,
        points => points
            .windows(2)
            .any(|pair| segment_distance(pair[0], pair[1], point) <= reach),
    }
}

fn segment_distance(a: [f32; 2], b: [f32; 2], p: [f32; 2]) -> f32 {
    let along = [b[0] - a[0], b[1] - a[1]];
    let length = along[0] * along[0] + along[1] * along[1];
    let t = if length == 0.0 {
        0.0
    } else {
        (((p[0] - a[0]) * along[0] + (p[1] - a[1]) * along[1]) / length).clamp(0.0, 1.0)
    };
    distance([a[0] + along[0] * t, a[1] + along[1] * t], p)
}

/// The presentation's clock: it can be paused and reset.
#[derive(Clone, Copy, Debug)]
pub struct Clock {
    started: Instant,
    paused_at: Option<Instant>,
    paused: Duration,
}

impl Clock {
    pub fn new(now: Instant) -> Self {
        Self {
            started: now,
            paused_at: None,
            paused: Duration::ZERO,
        }
    }

    pub fn elapsed(&self, now: Instant) -> Duration {
        let end = self.paused_at.unwrap_or(now);
        end.saturating_duration_since(self.started)
            .saturating_sub(self.paused)
    }

    pub fn is_paused(&self) -> bool {
        self.paused_at.is_some()
    }

    pub fn toggle_pause(&mut self, now: Instant) {
        match self.paused_at.take() {
            Some(at) => self.paused += now.saturating_duration_since(at),
            None => self.paused_at = Some(now),
        }
    }

    /// Back to zero, keeping whether it is paused.
    pub fn reset(&mut self, now: Instant) {
        let paused = self.is_paused();
        *self = Self::new(now);
        if paused {
            self.paused_at = Some(now);
        }
    }
}

/// How the clock reads against the talk's length.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pace {
    OnTime,
    /// The last tenth of the talk.
    Closing,
    Over,
}

/// The clock's text: time elapsed, or with a talk length the time left,
/// and `+m:ss` once over (after pdfpc).
pub fn clock_text(elapsed: Duration, talk: Option<Duration>) -> (String, Pace) {
    let Some(talk) = talk.filter(|talk| !talk.is_zero()) else {
        return (hms(elapsed), Pace::OnTime);
    };
    if elapsed > talk {
        let over = (elapsed - talk).as_secs();
        let text = if over >= 3600 {
            format!("+{}", hms(elapsed - talk))
        } else {
            format!("+{:02}:{:02}", over / 60, over % 60)
        };
        return (text, Pace::Over);
    }
    let left = talk - elapsed;
    let pace = if left * 10 <= talk {
        Pace::Closing
    } else {
        Pace::OnTime
    };
    // Whole seconds left, rounded up, so 0:00 shows only at the end.
    let left = Duration::from_secs(left.as_secs() + u64::from(left.subsec_nanos() > 0));
    (hms(left), pace)
}

fn hms(duration: Duration) -> String {
    let seconds = duration.as_secs();
    format!(
        "{:02}:{:02}:{:02}",
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60
    )
}

/// A right-button press on the page: a click goes to the previous page,
/// holding or dragging opens the tool ring.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RingGesture {
    Pressed { at: Pos2, since: Instant },
    Open { center: Pos2 },
}

/// The ring's default tools, from up clockwise.
pub const RING_DEFAULTS: [Command; glyphst_ui::ring::SLOTS] = [
    Command::PresentDraw,
    Command::PresentHighlight,
    Command::PresentCursor,
    Command::PresentUndo,
    Command::PresentErase,
    Command::PresentClear,
    Command::PresentBlack,
    Command::PresentTimer,
];

/// The ring's slots as preferences keep them: command ids, or `None`.
pub type RingSlots = [Option<String>; glyphst_ui::ring::SLOTS];

pub fn default_ring() -> RingSlots {
    RING_DEFAULTS.map(|command| Some(command.id().to_string()))
}

/// The presentation's state, new each time presenting starts (ink stays
/// with the tab).
#[derive(Debug)]
pub struct Presenting {
    pub pointer: Pointer,
    pub tool: Option<Tool>,
    /// Index into [`INK`].
    pub color: usize,
    /// The laser's recent positions.
    pub trail: VecDeque<(Pos2, Instant)>,
    pub ring: Option<RingGesture>,
    pub blank: Option<Blank>,
    pub clock: Clock,
    /// The app made the window fullscreen and puts it back on leaving.
    pub made_fullscreen: bool,
    /// Wheel movement not yet turned into a page turn.
    pub wheel: f32,
    /// The page a stroke is being drawn on.
    pub drawing: Option<usize>,
    /// When the pointer last moved, for the toolbox's fade.
    pub moved: Instant,
    /// Where the toolbox and the pen's colours were last frame: the
    /// pointer over them is an arrow, not the laser.
    pub toolbox: Vec<egui::Rect>,
}

impl Presenting {
    pub fn new(now: Instant) -> Self {
        Self {
            pointer: Pointer::Hidden,
            tool: None,
            color: 0,
            trail: VecDeque::new(),
            ring: None,
            blank: None,
            clock: Clock::new(now),
            made_fullscreen: false,
            wheel: 0.0,
            drawing: None,
            moved: now,
            toolbox: Vec::new(),
        }
    }

    /// Keeps the trail to its last [`TRAIL`].
    pub fn trail_to(&mut self, pos: Option<Pos2>, now: Instant) {
        while self
            .trail
            .front()
            .is_some_and(|(_, at)| now.saturating_duration_since(*at) > TRAIL)
        {
            self.trail.pop_front();
        }
        if let Some(pos) = pos
            && self.pointer == Pointer::Laser
            && self.trail.back().is_none_or(|(last, _)| *last != pos)
        {
            self.trail.push_back((pos, now));
        }
    }

    /// Turns `tool` on, or off when it is on.
    pub fn toggle_tool(&mut self, tool: Tool) {
        self.tool = (self.tool != Some(tool)).then_some(tool);
        self.drawing = None;
    }

    pub fn toggle_blank(&mut self, blank: Blank) {
        self.blank = (self.blank != Some(blank)).then_some(blank);
    }

    /// Takes wheel movement; returns a page turn, forward when positive,
    /// once it has gone far enough.
    pub fn wheel_turn(&mut self, delta: f32) -> Option<bool> {
        const STEP: f32 = 48.0;
        if delta == 0.0 {
            return None;
        }
        if self.wheel.signum() != delta.signum() {
            self.wheel = 0.0;
        }
        self.wheel += delta;
        (self.wheel.abs() >= STEP).then(|| {
            let forward = self.wheel > 0.0;
            self.wheel = 0.0;
            forward
        })
    }
}

impl Presenting {
    /// `command`'s label now, when it follows the state: `End draw` while
    /// drawing.
    pub fn label(&self, command: Command) -> Option<&'static str> {
        let on = |tool| self.tool == Some(tool);
        Some(match command {
            Command::Present => "Exit presentation",
            Command::PresentDraw if on(Tool::Pen) => "End draw",
            Command::PresentHighlight if on(Tool::Highlighter) => "End highlight",
            Command::PresentErase if on(Tool::Eraser) => "Exit eraser",
            Command::PresentCursor if self.pointer == Pointer::Laser => "Hide cursor",
            Command::PresentBlack if self.blank == Some(Blank::Black) => "End black screen",
            Command::PresentWhite if self.blank == Some(Blank::White) => "End white screen",
            Command::PresentTimer if self.clock.is_paused() => "Resume the timer",
            _ => return None,
        })
    }

    /// The ring's short label and icon for `command`.
    pub fn slot(&self, command: Command) -> (&'static str, &'static str) {
        let on = |tool| self.tool == Some(tool);
        let label = match command {
            Command::PresentDraw if on(Tool::Pen) => "end draw",
            Command::PresentDraw => "draw",
            Command::PresentHighlight if on(Tool::Highlighter) => "end highlight",
            Command::PresentHighlight => "highlight",
            Command::PresentErase if on(Tool::Eraser) => "exit eraser",
            Command::PresentErase => "eraser",
            Command::PresentClear => "clear",
            Command::PresentUndo => "undo",
            Command::PresentCursor if self.pointer == Pointer::Laser => "hide cursor",
            Command::PresentCursor => "cursor",
            Command::PresentBlack => "black",
            Command::PresentWhite => "white",
            Command::PresentTimer if self.clock.is_paused() => "resume",
            Command::PresentTimer => "timer",
            Command::PresentTimerReset => "reset",
            other => other.label(),
        };
        let icon = match command {
            Command::PresentTimer if self.clock.is_paused() => "play",
            other => other.icon(),
        };
        (label, icon)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(points: &[[f32; 2]]) -> Stroke {
        Stroke {
            points: points.to_vec(),
            color: INK[0].1,
            width: 2.0,
        }
    }

    #[test]
    fn the_eraser_takes_whole_strokes_it_touches() {
        let mut ink = Ink::default();
        ink.start(3, line(&[[0.0, 0.0], [100.0, 0.0]]));
        ink.start(3, line(&[[0.0, 50.0], [100.0, 50.0]]));
        ink.start(4, line(&[[50.0, 0.0]]));
        // Near the first line's middle, not its ends.
        assert!(ink.erase(3, [50.0, 6.0], 5.0));
        assert_eq!(ink.page(3).len(), 1);
        assert_eq!(ink.page(3)[0].points[0], [0.0, 50.0]);
        // Too far from the second.
        assert!(!ink.erase(3, [50.0, 30.0], 5.0));
        // Other pages keep theirs.
        assert_eq!(ink.page(4).len(), 1);
        assert!(ink.erase(4, [52.0, 0.0], 1.0));
    }

    #[test]
    fn undo_and_clear_work_on_one_page() {
        let mut ink = Ink::default();
        assert!(!ink.undo(0));
        ink.start(0, line(&[[0.0, 0.0]]));
        ink.extend(0, [0.2, 0.0], 0.5);
        ink.extend(0, [3.0, 0.0], 0.5);
        assert_eq!(ink.page(0)[0].points.len(), 2, "close points are skipped");
        ink.start(0, line(&[[9.0, 9.0]]));
        ink.start(1, line(&[[9.0, 9.0]]));
        assert!(ink.undo(0));
        assert_eq!(ink.page(0).len(), 1);
        assert!(ink.clear(0));
        assert!(!ink.clear(0));
        assert_eq!(ink.page(1).len(), 1);
    }

    #[test]
    fn the_clock_pauses_and_resets() {
        let start = Instant::now();
        let at = |seconds: u64| start + Duration::from_secs(seconds);
        let mut clock = Clock::new(start);
        assert_eq!(clock.elapsed(at(10)), Duration::from_secs(10));
        clock.toggle_pause(at(10));
        assert!(clock.is_paused());
        assert_eq!(clock.elapsed(at(25)), Duration::from_secs(10));
        clock.toggle_pause(at(30));
        assert_eq!(clock.elapsed(at(35)), Duration::from_secs(15));
        clock.reset(at(40));
        assert_eq!(clock.elapsed(at(42)), Duration::from_secs(2));
        clock.toggle_pause(at(42));
        clock.reset(at(50));
        assert!(clock.is_paused(), "a reset keeps the pause");
        assert_eq!(clock.elapsed(at(60)), Duration::ZERO);
    }

    #[test]
    fn the_clock_counts_down_a_talk_and_then_over() {
        let minutes = |m: u64| Duration::from_secs(m * 60);
        assert_eq!(
            clock_text(Duration::from_secs(761), None),
            ("00:12:41".into(), Pace::OnTime)
        );
        let talk = Some(minutes(20));
        assert_eq!(
            clock_text(minutes(5), talk),
            ("00:15:00".into(), Pace::OnTime)
        );
        assert_eq!(
            clock_text(minutes(18), talk),
            ("00:02:00".into(), Pace::Closing)
        );
        assert_eq!(
            clock_text(Duration::from_millis(1_199_500), talk),
            ("00:00:01".into(), Pace::Closing)
        );
        assert_eq!(
            clock_text(minutes(20) + Duration::from_secs(42), talk),
            ("+00:42".into(), Pace::Over)
        );
        assert_eq!(
            clock_text(minutes(5), Some(Duration::ZERO)).1,
            Pace::OnTime,
            "no length counts up"
        );
    }

    #[test]
    fn the_wheel_turns_once_it_has_gone_far_enough() {
        let mut presenting = Presenting::new(Instant::now());
        assert_eq!(presenting.wheel_turn(20.0), None);
        assert_eq!(presenting.wheel_turn(30.0), Some(true));
        assert_eq!(presenting.wheel_turn(-20.0), None);
        assert_eq!(
            presenting.wheel_turn(10.0),
            None,
            "turning back starts over"
        );
        assert_eq!(presenting.wheel_turn(-60.0), Some(false));
    }

    #[test]
    fn labels_follow_the_state() {
        let now = Instant::now();
        let mut presenting = Presenting::new(now);
        assert_eq!(presenting.label(Command::PresentDraw), None);
        assert_eq!(presenting.slot(Command::PresentDraw).0, "draw");
        presenting.toggle_tool(Tool::Pen);
        assert_eq!(presenting.label(Command::PresentDraw), Some("End draw"));
        assert_eq!(presenting.slot(Command::PresentDraw).0, "end draw");
        presenting.toggle_tool(Tool::Eraser);
        assert_eq!(presenting.label(Command::PresentDraw), None);
        assert_eq!(presenting.label(Command::PresentErase), Some("Exit eraser"));
        presenting.clock.toggle_pause(now);
        assert_eq!(presenting.slot(Command::PresentTimer), ("resume", "play"));
        assert_eq!(
            presenting.label(Command::Present),
            Some("Exit presentation")
        );
    }

    #[test]
    fn tools_and_blanks_toggle() {
        let mut presenting = Presenting::new(Instant::now());
        presenting.toggle_tool(Tool::Pen);
        assert_eq!(presenting.tool, Some(Tool::Pen));
        presenting.toggle_tool(Tool::Eraser);
        assert_eq!(presenting.tool, Some(Tool::Eraser));
        presenting.toggle_tool(Tool::Eraser);
        assert_eq!(presenting.tool, None);
        presenting.toggle_blank(Blank::Black);
        presenting.toggle_blank(Blank::White);
        assert_eq!(presenting.blank, Some(Blank::White));
        presenting.toggle_blank(Blank::White);
        assert_eq!(presenting.blank, None);
    }
}
