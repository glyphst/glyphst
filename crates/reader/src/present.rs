//! What presenting keeps beside the page (frames 02g–j): the mode (the
//! hidden cursor, the laser, or a drawing tool), ink on each page with its
//! undo history, the blank screen, the tool ring's gesture and the clock.
//! `app/present.rs` takes the input and draws them.

use std::{
    collections::{HashMap, VecDeque},
    time::{Duration, Instant},
};

use egui::{Color32, Pos2, Vec2};

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
/// How long the laser's tail lasts (frame 02j, L1).
pub const TRAIL: Duration = Duration::from_millis(250);
/// The tail is kept at least this many points between samples, and longer
/// jumps are filled in.
const TRAIL_STEP: f32 = 3.0;
/// Changes kept to undo, per page.
const HISTORY: usize = 100;

/// What the pointer is while presenting; one at a time. Hidden is the
/// default: the cursor waits at the centre and nothing hovers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Hidden,
    /// The cursor shown, as the laser.
    Laser,
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

/// A page's strokes and what undo and redo bring back.
#[derive(Clone, Debug, Default)]
struct PageInk {
    strokes: Vec<Stroke>,
    undo: Vec<Vec<Stroke>>,
    redo: Vec<Vec<Stroke>>,
}

impl PageInk {
    /// Keeps the strokes as they are for undo, before a change.
    fn checkpoint(&mut self) {
        self.undo.push(self.strokes.clone());
        if self.undo.len() > HISTORY {
            self.undo.remove(0);
        }
        self.redo.clear();
    }
}

/// A document's ink, by page; kept while its tab is open, never saved.
#[derive(Clone, Debug, Default)]
pub struct Ink {
    pages: HashMap<usize, PageInk>,
}

impl Ink {
    pub fn page(&self, page: usize) -> &[Stroke] {
        self.pages
            .get(&page)
            .map_or(&[], |ink| ink.strokes.as_slice())
    }

    pub fn start(&mut self, page: usize, stroke: Stroke) {
        let ink = self.pages.entry(page).or_default();
        ink.checkpoint();
        ink.strokes.push(stroke);
    }

    /// Adds `point` to the page's latest stroke, unless it is within
    /// `min` page points of its last point.
    pub fn extend(&mut self, page: usize, point: [f32; 2], min: f32) {
        let Some(stroke) = self
            .pages
            .get_mut(&page)
            .and_then(|ink| ink.strokes.last_mut())
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

    /// Puts the page back as it was before its latest change; whether
    /// there was one.
    pub fn undo(&mut self, page: usize) -> bool {
        let Some(ink) = self.pages.get_mut(&page) else {
            return false;
        };
        let Some(before) = ink.undo.pop() else {
            return false;
        };
        ink.redo.push(std::mem::replace(&mut ink.strokes, before));
        true
    }

    /// Makes again the change undo took back; whether there was one.
    pub fn redo(&mut self, page: usize) -> bool {
        let Some(ink) = self.pages.get_mut(&page) else {
            return false;
        };
        let Some(after) = ink.redo.pop() else {
            return false;
        };
        ink.undo.push(std::mem::replace(&mut ink.strokes, after));
        true
    }

    /// Removes the page's strokes, undoably; whether it had any.
    pub fn clear(&mut self, page: usize) -> bool {
        let Some(ink) = self
            .pages
            .get_mut(&page)
            .filter(|ink| !ink.strokes.is_empty())
        else {
            return false;
        };
        ink.checkpoint();
        ink.strokes.clear();
        true
    }

    /// Removes the page's strokes that pass within `radius` page points of
    /// `point`, as whole strokes; whether any went. A drag keeps one
    /// undo step: `checkpoint` is true for its first removal only.
    pub fn erase(&mut self, page: usize, point: [f32; 2], radius: f32, checkpoint: bool) -> bool {
        let Some(ink) = self.pages.get_mut(&page) else {
            return false;
        };
        if !ink
            .strokes
            .iter()
            .any(|stroke| touches(stroke, point, radius))
        {
            return false;
        }
        if checkpoint {
            ink.checkpoint();
        }
        ink.strokes.retain(|stroke| !touches(stroke, point, radius));
        true
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

/// The presentation's clock, started by the timer tool; it can be paused.
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
/// holding or dragging opens the tool ring at `center` (the screen's
/// centre while the cursor is hidden, else where the button went down).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RingGesture {
    Pressed {
        at: Pos2,
        center: Pos2,
        since: Instant,
    },
    Open {
        at: Pos2,
        center: Pos2,
    },
}

/// Where the ring's pointer is: the ring's centre moved as the mouse has
/// moved since the button went down. `raw` is the mouse's own motion
/// (sent while the cursor is locked); without it the pointer's change
/// stands in.
pub fn virtual_pointer(center: Pos2, raw: Option<Vec2>, at: Pos2, latest: Pos2) -> Pos2 {
    center + raw.unwrap_or(latest - at)
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

/// The reading ring's defaults (frame 09b), from up clockwise.
pub const READING_RING_DEFAULTS: [Option<Command>; glyphst_ui::ring::SLOTS] = [
    Some(Command::Outline),
    Some(Command::Bookmark),
    Some(Command::FitWidth),
    Some(Command::FitPage),
    Some(Command::PageOverview),
    None,
    Some(Command::Back),
    Some(Command::Home),
];

pub fn default_reading_ring() -> RingSlots {
    READING_RING_DEFAULTS.map(|command| command.map(|command| command.id().to_string()))
}

/// The presentation's state, new each time presenting starts (ink stays
/// with the tab).
#[derive(Debug)]
pub struct Presenting {
    pub mode: Mode,
    /// Index into [`INK`].
    pub color: usize,
    /// The laser's recent positions.
    pub trail: VecDeque<(Pos2, Instant)>,
    pub ring: Option<RingGesture>,
    /// The mouse's own motion since the ring's button went down, when any
    /// came.
    pub ring_motion: Option<Vec2>,
    pub blank: Option<Blank>,
    /// Off until the timer tool starts it.
    pub clock: Option<Clock>,
    /// The app made the window fullscreen and puts it back on leaving.
    pub made_fullscreen: bool,
    /// The cursor is locked at the centre (while hidden).
    pub locked: bool,
    /// Wheel movement not yet turned into a page turn.
    pub wheel: f32,
    /// The page a stroke is being drawn on (or erased on).
    pub drawing: Option<usize>,
    /// The eraser's drag has removed a stroke (one undo step per drag).
    pub erased: bool,
    /// When the pointer last moved, for the toolbox's fade.
    pub moved: Instant,
    /// Where the toolbox and the pen's colours were last frame: the
    /// pointer over them is an arrow, not the laser.
    pub toolbox: Vec<egui::Rect>,
}

impl Presenting {
    pub fn new(now: Instant) -> Self {
        Self {
            mode: Mode::Hidden,
            color: 0,
            trail: VecDeque::new(),
            ring: None,
            ring_motion: None,
            blank: None,
            clock: None,
            made_fullscreen: false,
            locked: false,
            wheel: 0.0,
            drawing: None,
            erased: false,
            moved: now,
            toolbox: Vec::new(),
        }
    }

    /// Ages the trail and adds the positions the pointer passed this frame,
    /// filling long jumps so the tail stays whole.
    pub fn trail_to(&mut self, positions: &[Pos2], now: Instant) {
        while self
            .trail
            .front()
            .is_some_and(|(_, at)| now.saturating_duration_since(*at) > TRAIL)
        {
            self.trail.pop_front();
        }
        if self.mode != Mode::Laser {
            self.trail.clear();
            return;
        }
        for &pos in positions {
            let Some(&(last, _)) = self.trail.back() else {
                self.trail.push_back((pos, now));
                continue;
            };
            let gap = (pos - last).length();
            if gap < TRAIL_STEP {
                continue;
            }
            let steps = (gap / (TRAIL_STEP * 2.0)).ceil() as usize;
            for step in 1..=steps {
                self.trail
                    .push_back((last + (pos - last) * (step as f32 / steps as f32), now));
            }
        }
    }

    /// Turns `mode` on, or back to the hidden cursor when it is on.
    pub fn toggle_mode(&mut self, mode: Mode) {
        self.mode = if self.mode == mode {
            Mode::Hidden
        } else {
            mode
        };
        self.drawing = None;
        self.trail.clear();
    }

    pub fn toggle_blank(&mut self, blank: Blank) {
        self.blank = (self.blank != Some(blank)).then_some(blank);
    }

    /// The timer tool: starts the clock, then pauses and resumes it.
    pub fn timer(&mut self, now: Instant) {
        match &mut self.clock {
            Some(clock) => clock.toggle_pause(now),
            None => self.clock = Some(Clock::new(now)),
        }
    }

    /// Whether the corner chip shows: when asked for, or while the clock
    /// is on.
    pub fn chip_shows(&self, page_number: bool) -> bool {
        page_number || self.clock.is_some()
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

    /// `command`'s label now, when it follows the state: `End draw` while
    /// drawing.
    pub fn label(&self, command: Command) -> Option<&'static str> {
        let on = |mode| self.mode == mode;
        Some(match command {
            Command::Present => "Exit presentation",
            Command::PresentDraw if on(Mode::Pen) => "End draw",
            Command::PresentHighlight if on(Mode::Highlighter) => "End highlight",
            Command::PresentErase if on(Mode::Eraser) => "Exit eraser",
            Command::PresentCursor if on(Mode::Laser) => "Hide cursor",
            Command::PresentBlack if self.blank == Some(Blank::Black) => "End black screen",
            Command::PresentWhite if self.blank == Some(Blank::White) => "End white screen",
            Command::PresentTimer => match self.clock {
                None => return None,
                Some(clock) if clock.is_paused() => "Resume the timer",
                Some(_) => "Pause the timer",
            },
            _ => return None,
        })
    }

    /// The ring's short label and icon for `command`.
    pub fn slot(&self, command: Command) -> (&'static str, &'static str) {
        let on = |mode| self.mode == mode;
        let paused = self.clock.map(|clock| clock.is_paused());
        let label = match command {
            Command::PresentDraw if on(Mode::Pen) => "end draw",
            Command::PresentDraw => "draw",
            Command::PresentHighlight if on(Mode::Highlighter) => "end highlight",
            Command::PresentHighlight => "highlight",
            Command::PresentErase if on(Mode::Eraser) => "exit eraser",
            Command::PresentErase => "eraser",
            Command::PresentClear => "clear",
            Command::PresentUndo => "undo",
            Command::PresentRedo => "redo",
            Command::PresentCursor if on(Mode::Laser) => "hide cursor",
            Command::PresentCursor => "cursor",
            Command::PresentBlack => "black",
            Command::PresentWhite => "white",
            Command::PresentTimer => match paused {
                None => "timer",
                Some(false) => "pause",
                Some(true) => "resume",
            },
            Command::PresentTimerStop => "stop timer",
            other => other.label(),
        };
        let icon = match (command, paused) {
            (Command::PresentTimer, Some(false)) => "pause",
            (Command::PresentTimer, Some(true)) => "play",
            (other, _) => other.icon(),
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
        assert!(ink.erase(3, [50.0, 6.0], 5.0, true));
        assert_eq!(ink.page(3).len(), 1);
        assert_eq!(ink.page(3)[0].points[0], [0.0, 50.0]);
        // Too far from the second.
        assert!(!ink.erase(3, [50.0, 30.0], 5.0, true));
        // Other pages keep theirs.
        assert_eq!(ink.page(4).len(), 1);
        assert!(ink.erase(4, [52.0, 0.0], 1.0, true));
    }

    #[test]
    fn undo_and_redo_step_through_strokes_erasing_and_clearing() {
        let mut ink = Ink::default();
        assert!(!ink.undo(0) && !ink.redo(0));
        ink.start(0, line(&[[0.0, 0.0]]));
        ink.extend(0, [0.2, 0.0], 0.5);
        ink.extend(0, [3.0, 0.0], 0.5);
        assert_eq!(ink.page(0)[0].points.len(), 2, "close points are skipped");
        ink.start(0, line(&[[9.0, 9.0]]));
        ink.start(1, line(&[[9.0, 9.0]]));
        assert!(ink.undo(0));
        assert_eq!(ink.page(0).len(), 1);
        assert!(ink.redo(0));
        assert_eq!(ink.page(0).len(), 2);
        assert!(!ink.redo(0), "nothing more to redo");

        // Clearing and erasing are undone too.
        assert!(ink.clear(0));
        assert!(!ink.clear(0));
        assert!(ink.page(0).is_empty());
        assert!(ink.undo(0));
        assert_eq!(ink.page(0).len(), 2);
        // A drag erasing twice is one step.
        assert!(ink.erase(0, [9.0, 9.0], 1.0, true));
        assert!(ink.erase(0, [1.0, 0.0], 1.0, false));
        assert!(ink.page(0).is_empty());
        assert!(ink.undo(0));
        assert_eq!(ink.page(0).len(), 2);

        // A new change drops what redo had.
        assert!(ink.undo(0));
        ink.start(0, line(&[[5.0, 5.0]]));
        assert!(!ink.redo(0));
        assert_eq!(ink.page(1).len(), 1, "other pages are left alone");
    }

    #[test]
    fn the_timer_starts_then_pauses_and_resumes() {
        let start = Instant::now();
        let at = |seconds: u64| start + Duration::from_secs(seconds);
        let mut presenting = Presenting::new(start);
        assert!(presenting.clock.is_none());
        assert!(!presenting.chip_shows(false));
        assert!(presenting.chip_shows(true));
        assert_eq!(presenting.slot(Command::PresentTimer), ("timer", "timer"));
        assert_eq!(presenting.label(Command::PresentTimer), None);
        presenting.timer(at(5));
        assert!(presenting.chip_shows(false), "the timer shows the chip");
        assert_eq!(presenting.slot(Command::PresentTimer), ("pause", "pause"));
        assert_eq!(
            presenting.label(Command::PresentTimer),
            Some("Pause the timer")
        );
        let clock = |p: &Presenting, s| p.clock.unwrap().elapsed(at(s));
        assert_eq!(clock(&presenting, 15), Duration::from_secs(10));
        presenting.timer(at(15));
        assert_eq!(presenting.slot(Command::PresentTimer), ("resume", "play"));
        assert_eq!(clock(&presenting, 40), Duration::from_secs(10));
        presenting.timer(at(40));
        assert_eq!(clock(&presenting, 45), Duration::from_secs(15));
        // Stopping hides it.
        presenting.clock = None;
        assert!(!presenting.chip_shows(false));
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
    fn labels_follow_the_mode() {
        let mut presenting = Presenting::new(Instant::now());
        assert_eq!(presenting.label(Command::PresentDraw), None);
        assert_eq!(presenting.slot(Command::PresentDraw).0, "draw");
        presenting.toggle_mode(Mode::Pen);
        assert_eq!(presenting.label(Command::PresentDraw), Some("End draw"));
        assert_eq!(presenting.slot(Command::PresentDraw).0, "end draw");
        presenting.toggle_mode(Mode::Eraser);
        assert_eq!(presenting.label(Command::PresentDraw), None);
        assert_eq!(presenting.label(Command::PresentErase), Some("Exit eraser"));
        assert_eq!(
            presenting.label(Command::Present),
            Some("Exit presentation")
        );
    }

    #[test]
    fn modes_exclude_each_other() {
        let mut presenting = Presenting::new(Instant::now());
        assert_eq!(presenting.mode, Mode::Hidden);
        presenting.toggle_mode(Mode::Laser);
        assert_eq!(presenting.mode, Mode::Laser);
        presenting.toggle_mode(Mode::Pen);
        assert_eq!(presenting.mode, Mode::Pen, "drawing ends the laser");
        presenting.toggle_mode(Mode::Laser);
        assert_eq!(presenting.mode, Mode::Laser, "the laser ends drawing");
        presenting.toggle_mode(Mode::Laser);
        assert_eq!(presenting.mode, Mode::Hidden);
        presenting.toggle_blank(Blank::Black);
        presenting.toggle_blank(Blank::White);
        assert_eq!(presenting.blank, Some(Blank::White));
        presenting.toggle_blank(Blank::White);
        assert_eq!(presenting.blank, None);
    }

    #[test]
    fn the_tail_fills_fast_moves_and_fades() {
        let now = Instant::now();
        let mut presenting = Presenting::new(now);
        presenting.trail_to(&[Pos2::ZERO], now);
        assert!(presenting.trail.is_empty(), "only the laser leaves a tail");
        presenting.toggle_mode(Mode::Laser);
        presenting.trail_to(&[Pos2::ZERO, egui::pos2(60.0, 0.0)], now);
        let gaps: Vec<f32> = presenting
            .trail
            .iter()
            .zip(presenting.trail.iter().skip(1))
            .map(|((a, _), (b, _))| (*b - *a).length())
            .collect();
        assert!(gaps.len() >= 9, "a fast move is filled in: {gaps:?}");
        assert!(gaps.iter().all(|gap| *gap <= TRAIL_STEP * 2.0 + 0.01));
        presenting.trail_to(&[], now + TRAIL + Duration::from_millis(1));
        assert!(presenting.trail.is_empty(), "the tail fades away");
    }

    #[test]
    fn the_rings_pointer_follows_the_mouse_from_the_centre() {
        let center = egui::pos2(720.0, 460.0);
        let at = egui::pos2(100.0, 100.0);
        // Locked: the mouse's own motion.
        assert_eq!(
            virtual_pointer(center, Some(egui::vec2(0.0, -40.0)), at, at),
            egui::pos2(720.0, 420.0)
        );
        // Not locked: how far the pointer went.
        assert_eq!(
            virtual_pointer(center, None, at, egui::pos2(130.0, 100.0)),
            egui::pos2(750.0, 460.0)
        );
    }
}
