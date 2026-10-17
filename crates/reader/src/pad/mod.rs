//! Gamepads through gilrs.
//!
//! A thread owns gilrs and wakes the app on every pad event ([`thread`]).
//! Before each frame the events are drained into a [`PadFrame`], and
//! [`route`] hands it to whoever takes pad input: the keys of a window in
//! front, or the document. Screens and tests drive a scripted pad instead
//! ([`Script`]).

mod repeat;
pub mod route;
mod script;
mod thread;

use egui::Vec2;

pub use repeat::{Dir, Repeat};
pub use script::Script;

/// Stick positions below this magnitude are ignored.
const DEADZONE: f32 = 0.15;
/// View pixels per second at full stick deflection.
pub const MAX_SCROLL_SPEED: f32 = 2400.0;
/// Natural-log zoom change per second at a fully pressed trigger.
pub const MAX_ZOOM_RATE: f32 = 2.5;

/// A pad button by position, so one name serves every brand: the face
/// buttons are south (A on an Xbox pad, ✕ on a PlayStation one), east,
/// west and north.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum PadButton {
    South,
    East,
    West,
    North,
    LeftBumper,
    RightBumper,
    LeftTrigger,
    RightTrigger,
    Select,
    Start,
    Guide,
    LeftStick,
    RightStick,
    Up,
    Down,
    Left,
    Right,
}

impl PadButton {
    pub const ALL: [Self; 17] = [
        Self::South,
        Self::East,
        Self::West,
        Self::North,
        Self::LeftBumper,
        Self::RightBumper,
        Self::LeftTrigger,
        Self::RightTrigger,
        Self::Select,
        Self::Start,
        Self::Guide,
        Self::LeftStick,
        Self::RightStick,
        Self::Up,
        Self::Down,
        Self::Left,
        Self::Right,
    ];

    pub fn from_gilrs(button: gilrs::Button) -> Option<Self> {
        use gilrs::Button as B;
        Some(match button {
            B::South => Self::South,
            B::East => Self::East,
            B::West => Self::West,
            B::North => Self::North,
            // gilrs calls the bumpers triggers, and the triggers triggers 2.
            B::LeftTrigger => Self::LeftBumper,
            B::RightTrigger => Self::RightBumper,
            B::LeftTrigger2 => Self::LeftTrigger,
            B::RightTrigger2 => Self::RightTrigger,
            B::Select => Self::Select,
            B::Start => Self::Start,
            B::Mode => Self::Guide,
            B::LeftThumb => Self::LeftStick,
            B::RightThumb => Self::RightStick,
            B::DPadUp => Self::Up,
            B::DPadDown => Self::Down,
            B::DPadLeft => Self::Left,
            B::DPadRight => Self::Right,
            _ => return None,
        })
    }

    /// The name bindings and scripts use.
    pub fn name(self) -> &'static str {
        match self {
            Self::South => "south",
            Self::East => "east",
            Self::West => "west",
            Self::North => "north",
            Self::LeftBumper => "lb",
            Self::RightBumper => "rb",
            Self::LeftTrigger => "lt",
            Self::RightTrigger => "rt",
            Self::Select => "select",
            Self::Start => "start",
            Self::Guide => "guide",
            Self::LeftStick => "ls",
            Self::RightStick => "rs",
            Self::Up => "up",
            Self::Down => "down",
            Self::Left => "left",
            Self::Right => "right",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|button| button.name().eq_ignore_ascii_case(name))
    }

    /// The d-pad direction the button is.
    pub fn dir(self) -> Option<Dir> {
        Some(match self {
            Self::Up => Dir::Up,
            Self::Down => Dir::Down,
            Self::Left => Dir::Left,
            Self::Right => Dir::Right,
            _ => return None,
        })
    }
}

/// Whose pad it is, from its USB vendor: it picks the letters on the
/// face-button glyphs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Family {
    #[default]
    Generic,
    Xbox,
    PlayStation,
    Nintendo,
    Steam,
}

impl Family {
    pub fn from_vendor(vendor: Option<u16>) -> Self {
        match vendor {
            Some(0x045e) => Self::Xbox,
            Some(0x054c) => Self::PlayStation,
            Some(0x057e) => Self::Nintendo,
            Some(0x28de) => Self::Steam,
            _ => Self::Generic,
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "generic" => Self::Generic,
            "xbox" => Self::Xbox,
            "ps" | "playstation" => Self::PlayStation,
            "nintendo" => Self::Nintendo,
            "steam" => Self::Steam,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Power {
    #[default]
    Unknown,
    Wired,
    /// The battery's charge in percent.
    Battery {
        percent: u8,
        charging: bool,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PadInfo {
    pub id: usize,
    pub name: String,
    pub family: Family,
    pub power: Power,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PadEvent {
    Pressed(PadButton),
    Released(PadButton),
    /// `initial` for the pads already connected when the app started.
    Connected {
        pad: PadInfo,
        initial: bool,
    },
    Disconnected(usize),
}

/// The sticks and triggers of every pad, added up.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Axes {
    /// The left stick, -1..=1 with y down.
    pub left: Vec2,
    pub right: Vec2,
    /// How far the triggers are pressed, 0..=1.
    pub left_trigger: f32,
    pub right_trigger: f32,
}

impl Axes {
    pub fn add(&mut self, other: Self) {
        let clamp = |v: Vec2| v.clamp(Vec2::splat(-1.0), Vec2::splat(1.0));
        self.left = clamp(self.left + other.left);
        self.right = clamp(self.right + other.right);
        self.left_trigger = (self.left_trigger + other.left_trigger).min(1.0);
        self.right_trigger = (self.right_trigger + other.right_trigger).min(1.0);
    }
}

/// What the pads did since the last frame.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PadFrame {
    pub events: Vec<PadEvent>,
    pub axes: Axes,
}

impl PadFrame {
    /// Whether a button or a stick was used.
    pub fn used(&self) -> bool {
        self.events
            .iter()
            .any(|event| matches!(event, PadEvent::Pressed(_)))
            || axis_speed(self.axes.left.length()) != 0.0
            || axis_speed(self.axes.right.length()) != 0.0
    }
}

/// The document's share of a frame of pad input.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GamepadInput {
    /// Scroll velocity in view pixels per second; positive is down/right.
    pub scroll: [f32; 2],
    /// Zoom rate in natural-log units per second; positive zooms in.
    pub zoom: f32,
}

impl GamepadInput {
    pub fn is_active(&self) -> bool {
        self.scroll != [0.0, 0.0] || self.zoom != 0.0
    }

    /// The left stick scrolls and the triggers zoom.
    pub fn motion(axes: &Axes) -> Self {
        Self {
            scroll: [
                axis_speed(axes.left.x) * MAX_SCROLL_SPEED,
                axis_speed(axes.left.y) * MAX_SCROLL_SPEED,
            ],
            zoom: (axes.right_trigger - axes.left_trigger) * MAX_ZOOM_RATE,
        }
    }
}

/// Maps a stick axis to a speed in -1..=1: a deadzone, then a squared curve
/// for fine control near the center.
pub fn axis_speed(value: f32) -> f32 {
    let magnitude = value.abs().min(1.0);
    if magnitude <= DEADZONE {
        return 0.0;
    }
    let scaled = (magnitude - DEADZONE) / (1.0 - DEADZONE);
    scaled * scaled * value.signum()
}

/// The pads: gilrs on its thread, or a script.
pub struct Pads {
    source: Result<Source, String>,
}

enum Source {
    Gilrs(thread::PadThread),
    Script(Script),
}

impl Pads {
    /// Reads the real pads; `ctx` repaints on their events.
    pub fn start(ctx: &egui::Context) -> Self {
        Self {
            source: thread::spawn(ctx.clone()).map(Source::Gilrs),
        }
    }

    /// A scripted pad (an empty script is no pad at all).
    pub fn scripted(script: Script) -> Self {
        Self {
            source: Ok(Source::Script(script)),
        }
    }

    /// Lets a scripted pad play: screens start it once the document is
    /// drawn.
    pub fn play(&mut self) {
        if let Ok(Source::Script(script)) = &mut self.source {
            script.play();
        }
    }

    /// The events since the last call, with the axes as they are now.
    pub fn frame(&mut self) -> PadFrame {
        match &mut self.source {
            Ok(Source::Gilrs(pads)) => PadFrame {
                events: pads.drain(),
                axes: pads.axes(),
            },
            Ok(Source::Script(script)) => script.frame(),
            Err(_) => PadFrame::default(),
        }
    }

    pub fn connected(&self) -> Vec<PadInfo> {
        match &self.source {
            Ok(Source::Gilrs(pads)) => pads.connected(),
            Ok(Source::Script(script)) => script.connected(),
            Err(_) => Vec::new(),
        }
    }

    /// For the app menu: the connected pads, or why there are none.
    pub fn status(&self) -> String {
        if let Err(error) = &self.source {
            return format!("unavailable ({error})");
        }
        let names: Vec<String> = self.connected().into_iter().map(|pad| pad.name).collect();
        if names.is_empty() {
            "none connected".to_string()
        } else {
            names.join(", ")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sticks_have_a_deadzone_and_a_smooth_curve() {
        assert_eq!(axis_speed(0.1), 0.0);
        assert_eq!(axis_speed(-0.15), 0.0);
        assert_eq!(axis_speed(1.0), 1.0);
        assert_eq!(axis_speed(-1.0), -1.0);
        assert_eq!(axis_speed(2.0), 1.0, "out-of-range values clamp");
        let half = axis_speed(0.575);
        assert!(
            (half - 0.25).abs() < 1e-6,
            "halfway out is a quarter speed: {half}"
        );
        assert!(axis_speed(0.5) < axis_speed(0.6));
    }

    #[test]
    fn button_names_round_trip() {
        for button in PadButton::ALL {
            assert_eq!(PadButton::parse(button.name()), Some(button));
        }
        assert_eq!(PadButton::parse("A"), None, "buttons are named by position");
        assert_eq!(
            PadButton::from_gilrs(gilrs::Button::LeftTrigger),
            Some(PadButton::LeftBumper)
        );
    }

    #[test]
    fn vendors_name_the_family() {
        assert_eq!(Family::from_vendor(Some(0x054c)), Family::PlayStation);
        assert_eq!(Family::from_vendor(Some(0x045e)), Family::Xbox);
        assert_eq!(Family::from_vendor(Some(0x057e)), Family::Nintendo);
        assert_eq!(Family::from_vendor(Some(0x2dc8)), Family::Generic);
        assert_eq!(Family::from_vendor(None), Family::Generic);
    }

    #[test]
    fn the_left_stick_scrolls_and_the_triggers_zoom() {
        let axes = Axes {
            left: egui::vec2(0.0, 1.0),
            right_trigger: 0.5,
            ..Axes::default()
        };
        let input = GamepadInput::motion(&axes);
        assert_eq!(input.scroll, [0.0, MAX_SCROLL_SPEED]);
        assert_eq!(input.zoom, 0.5 * MAX_ZOOM_RATE);
        assert!(input.is_active());
        assert!(!GamepadInput::motion(&Axes::default()).is_active());
    }
}
