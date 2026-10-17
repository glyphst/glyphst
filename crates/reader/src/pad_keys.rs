//! Gamepad bindings: a button, a button held, or select with a button run
//! a command, as keys do in [`crate::keys`]. Each command has default
//! buttons ([`Command::default_pad`]); the user's choices replace them.
//!
//! The sticks scroll and the triggers zoom, and B is Escape (its cancel
//! chain); those stay fixed.
//!
//! A button fires as it is pressed, unless it also starts something
//! longer: a button with a hold binding waits for release (a tap) or
//! [`HOLD`] (the hold), and select, which starts chords, runs its own
//! command on release when no chord used it.

use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use crate::{
    commands::Command,
    keys::Overrides,
    pad::{PadButton, PadEvent},
};

/// How long a press lasts before it is a hold.
pub const HOLD: Duration = Duration::from_millis(400);

/// The button chords start with.
const MODIFIER: PadButton = PadButton::Select;

/// What a binding is: `south`, `hold north`, `select+lb`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PadChord {
    pub modifier: bool,
    pub button: PadButton,
    pub hold: bool,
}

impl PadChord {
    pub const fn tap(button: PadButton) -> Self {
        Self {
            modifier: false,
            button,
            hold: false,
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim().to_ascii_lowercase();
        let (hold, rest) = match text.strip_prefix("hold ") {
            Some(rest) => (true, rest.trim()),
            None => (false, text.as_str()),
        };
        let (modifier, name) = match rest.split_once('+') {
            Some((first, name)) if PadButton::parse(first.trim()) == Some(MODIFIER) => {
                (true, name.trim())
            }
            Some(_) => return None,
            None => (false, rest),
        };
        let button = PadButton::parse(name)?;
        let chord = Self {
            modifier,
            button,
            hold,
        };
        chord.is_bindable().then_some(chord)
    }

    pub fn notation(self) -> String {
        format!(
            "{}{}{}",
            if self.hold { "hold " } else { "" },
            if self.modifier { "select+" } else { "" },
            self.button.name()
        )
    }

    /// B is Escape; select alone can be bound, but not to itself; the
    /// guide button belongs to the system.
    fn is_bindable(self) -> bool {
        !matches!(self.button, PadButton::East | PadButton::Guide)
            && !(self.modifier && (self.button == MODIFIER || self.hold))
    }
}

/// Drops what storage shouldn't hold: unknown commands and buttons that
/// don't parse, as [`crate::keys::sanitize`] does for keys.
pub fn sanitize(overrides: &Overrides) -> Overrides {
    overrides
        .iter()
        .filter(|(id, _)| Command::from_id(id).is_some())
        .filter_map(|(id, chords)| {
            let mut parsed: Vec<PadChord> = Vec::new();
            for chord in chords.iter().filter_map(|chord| PadChord::parse(chord)) {
                if !parsed.contains(&chord) {
                    parsed.push(chord);
                }
            }
            if parsed.is_empty() && !chords.is_empty() {
                return None;
            }
            Some((id.clone(), parsed.iter().map(|c| c.notation()).collect()))
        })
        .collect()
}

/// The pad bindings in effect: the user's, then the defaults that don't
/// clash with them.
#[derive(Clone, Debug, Default)]
pub struct PadKeymap {
    bindings: HashMap<PadChord, Command>,
    chords: HashMap<Command, Vec<PadChord>>,
    couch: bool,
}

impl PadKeymap {
    /// The bindings for `overrides`, over the defaults of gamepad mode
    /// (`couch`) or of the desktop.
    pub fn new(overrides: &Overrides, couch: bool) -> Self {
        let chosen: HashMap<Command, Vec<PadChord>> = overrides
            .iter()
            .filter_map(|(id, chords)| {
                let command = Command::from_id(id)?;
                Some((
                    command,
                    chords.iter().filter_map(|c| PadChord::parse(c)).collect(),
                ))
            })
            .collect();
        let mut keymap = Self {
            couch,
            ..Self::default()
        };
        // The user's buttons claim their bindings first, then defaults take
        // what is left.
        for pass_chosen in [true, false] {
            for command in Command::ALL {
                let chords = match chosen.get(&command) {
                    Some(chords) if pass_chosen => chords.clone(),
                    Some(_) => continue,
                    None if pass_chosen => continue,
                    None => command
                        .default_pad(couch)
                        .iter()
                        .filter_map(|chord| PadChord::parse(chord))
                        .collect(),
                };
                for chord in chords {
                    if keymap.bindings.contains_key(&chord) {
                        continue;
                    }
                    keymap.bindings.insert(chord, command);
                    keymap.chords.entry(command).or_default().push(chord);
                }
            }
        }
        keymap
    }

    pub fn command(&self, chord: PadChord) -> Option<Command> {
        self.bindings.get(&chord).copied()
    }

    pub fn chords(&self, command: Command) -> &[PadChord] {
        self.chords.get(&command).map_or(&[], Vec::as_slice)
    }

    /// Whether the button waits for its release before it fires.
    fn waits(&self, button: PadButton) -> bool {
        button == MODIFIER
            || self.bindings.contains_key(&PadChord {
                hold: true,
                ..PadChord::tap(button)
            })
    }
}

/// The glyphs that show `chord`: select first for a select chord. A hold
/// is said in words beside them.
pub fn glyphs(chord: PadChord) -> Vec<glyphst_ui::pad_glyph::Glyph> {
    use glyphst_ui::pad_glyph::{Arm, Glyph};
    let glyph = |button: PadButton| match button {
        PadButton::Up => Glyph::Dpad(Some(Arm::Up)),
        PadButton::Down => Glyph::Dpad(Some(Arm::Down)),
        PadButton::Left => Glyph::Dpad(Some(Arm::Left)),
        PadButton::Right => Glyph::Dpad(Some(Arm::Right)),
        PadButton::Select => Glyph::Button("select"),
        PadButton::Start => Glyph::Button("≡"),
        PadButton::Guide => Glyph::Button("guide"),
        button => match button.face() {
            Some(face) => Glyph::Face(face),
            None => Glyph::Button(button.name()),
        },
    };
    let mut glyphs = Vec::new();
    if chord.modifier {
        glyphs.push(glyph(MODIFIER));
    }
    glyphs.push(glyph(chord.button));
    glyphs
}

/// Binds `chord` to `command`, taking it from the command that had it.
pub fn assign(overrides: &mut Overrides, keymap: &PadKeymap, command: Command, chord: PadChord) {
    match keymap.command(chord) {
        Some(owner) if owner == command => return,
        Some(owner) => {
            let kept = keymap.chords(owner).iter().copied().filter(|c| *c != chord);
            set(overrides, keymap.couch, owner, kept.collect());
        }
        None => {}
    }
    let mut kept = keymap.chords(command).to_vec();
    kept.push(chord);
    set(overrides, keymap.couch, command, kept);
}

/// Takes `chord` away from `command`.
pub fn unbind(overrides: &mut Overrides, keymap: &PadKeymap, command: Command, chord: PadChord) {
    let kept = keymap
        .chords(command)
        .iter()
        .copied()
        .filter(|c| *c != chord);
    set(overrides, keymap.couch, command, kept.collect());
}

/// Keeps `chords` for `command`, or forgets the choice when they are its
/// defaults.
fn set(overrides: &mut Overrides, couch: bool, command: Command, chords: Vec<PadChord>) {
    let defaults: Vec<PadChord> = command
        .default_pad(couch)
        .iter()
        .filter_map(|chord| PadChord::parse(chord))
        .collect();
    if chords == defaults {
        overrides.remove(command.id());
    } else {
        overrides.insert(
            command.id().to_string(),
            chords.iter().map(|chord| chord.notation()).collect(),
        );
    }
}

/// Buttons held, as the bindings see them.
#[derive(Debug, Default)]
pub struct PadPresses {
    held: Vec<Held>,
}

#[derive(Debug)]
struct Held {
    button: PadButton,
    since: Instant,
    /// Whether it already fired (a hold, or a chord it started).
    used: bool,
}

/// The commands a frame of pad events runs.
#[derive(Debug, Default, PartialEq)]
pub struct Fired {
    pub commands: Vec<Command>,
    /// When a held button turns into a hold.
    pub wake: Option<Duration>,
}

impl PadPresses {
    pub fn update(&mut self, keymap: &PadKeymap, events: &[PadEvent], now: Instant) -> Fired {
        let mut fired = Fired::default();
        for event in events {
            match event {
                PadEvent::Pressed(button) => {
                    let modifier = self
                        .held
                        .iter_mut()
                        .find(|held| held.button == MODIFIER && *button != MODIFIER);
                    if let Some(modifier) = modifier
                        && let Some(command) = keymap.command(PadChord {
                            modifier: true,
                            ..PadChord::tap(*button)
                        })
                    {
                        modifier.used = true;
                        fired.commands.push(command);
                        self.held.push(Held {
                            button: *button,
                            since: now,
                            used: true,
                        });
                        continue;
                    }
                    let waits = keymap.waits(*button);
                    if !waits && let Some(command) = keymap.command(PadChord::tap(*button)) {
                        fired.commands.push(command);
                    }
                    self.held.retain(|held| held.button != *button);
                    self.held.push(Held {
                        button: *button,
                        since: now,
                        used: !waits,
                    });
                }
                PadEvent::Released(button) => {
                    if let Some(index) = self.held.iter().position(|h| h.button == *button) {
                        let held = self.held.remove(index);
                        if !held.used
                            && let Some(command) = keymap.command(PadChord::tap(*button))
                        {
                            fired.commands.push(command);
                        }
                    }
                }
                PadEvent::Disconnected(_) => self.held.clear(),
                PadEvent::Connected { .. } => {}
            }
        }
        for held in &mut self.held {
            if held.used {
                continue;
            }
            let hold = PadChord {
                hold: true,
                ..PadChord::tap(held.button)
            };
            let Some(command) = keymap.command(hold) else {
                continue;
            };
            let waited = now.saturating_duration_since(held.since);
            if waited >= HOLD {
                held.used = true;
                fired.commands.push(command);
            } else {
                let wait = HOLD - waited;
                fired.wake = Some(fired.wake.map_or(wait, |other| other.min(wait)));
            }
        }
        fired
    }

    /// Whether `button` is held and fired as it was pressed, so a held
    /// d-pad may repeat its command.
    pub fn repeats(&self, button: PadButton) -> bool {
        self.held
            .iter()
            .any(|held| held.button == button && held.used)
            && !self.held.iter().any(|held| held.button == MODIFIER)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keymap() -> PadKeymap {
        PadKeymap::new(&Overrides::new(), false)
    }

    fn press(button: PadButton) -> PadEvent {
        PadEvent::Pressed(button)
    }

    fn release(button: PadButton) -> PadEvent {
        PadEvent::Released(button)
    }

    #[test]
    fn chords_parse_and_print() {
        for text in ["south", "hold north", "select+lb", "select", "up"] {
            assert_eq!(PadChord::parse(text).unwrap().notation(), text);
        }
        assert_eq!(
            PadChord::parse(" Select + RB ").unwrap().notation(),
            "select+rb"
        );
        for bad in [
            "east",
            "guide",
            "select+select",
            "lb+rb",
            "hold select+lb",
            "a",
        ] {
            assert_eq!(PadChord::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn defaults_parse_and_never_share_a_chord() {
        let mut seen = HashMap::new();
        for command in Command::ALL {
            for text in command.default_pad(false) {
                let chord = PadChord::parse(text)
                    .unwrap_or_else(|| panic!("{}: {text} doesn't parse", command.id()));
                if let Some(other) = seen.insert(chord, command) {
                    panic!("{text} is {} and {}", other.id(), command.id());
                }
            }
        }
        let keymap = keymap();
        assert_eq!(
            keymap.command(PadChord::tap(PadButton::RightBumper)),
            Some(Command::NextPage)
        );
    }

    #[test]
    fn gamepad_mode_s_start_opens_its_menu() {
        let couch = PadKeymap::new(&Overrides::new(), true);
        assert_eq!(
            couch.command(PadChord::tap(PadButton::Start)),
            Some(Command::GamepadMenu)
        );
        assert_eq!(
            keymap().command(PadChord::tap(PadButton::Start)),
            Some(Command::Present)
        );
        for keymap in [couch, keymap()] {
            let chord = PadChord::parse("select+start").unwrap();
            assert_eq!(keymap.command(chord), Some(Command::GamepadMode));
        }
        let mut seen = HashMap::new();
        for command in Command::ALL {
            for text in command.default_pad(true) {
                let chord = PadChord::parse(text).unwrap();
                assert!(seen.insert(chord, command).is_none(), "{text} twice");
            }
        }
    }

    #[test]
    fn a_plain_button_fires_on_press() {
        let mut presses = PadPresses::default();
        let fired = presses.update(&keymap(), &[press(PadButton::RightBumper)], Instant::now());
        assert_eq!(fired.commands, [Command::NextPage]);
        assert!(presses.repeats(PadButton::RightBumper));
    }

    #[test]
    fn select_runs_its_chords_or_its_own_command_on_release() {
        let keymap = keymap();
        let now = Instant::now();
        let mut presses = PadPresses::default();
        assert_eq!(
            presses
                .update(&keymap, &[press(PadButton::Select)], now)
                .commands,
            []
        );
        let chord = presses.update(&keymap, &[press(PadButton::RightBumper)], now);
        assert_eq!(chord.commands, [Command::LastPage], "not the next page");
        let released = presses.update(
            &keymap,
            &[release(PadButton::RightBumper), release(PadButton::Select)],
            now,
        );
        assert_eq!(released.commands, [], "select was used by the chord");
        let tapped = presses.update(
            &keymap,
            &[press(PadButton::Select), release(PadButton::Select)],
            now,
        );
        assert_eq!(tapped.commands, [Command::Outline]);
    }

    #[test]
    fn a_button_with_a_hold_taps_on_release_or_holds_after_a_while() {
        let mut overrides = Overrides::new();
        overrides.insert(
            Command::Bookmark.id().to_string(),
            vec!["hold north".to_string()],
        );
        let keymap = PadKeymap::new(&overrides, false);
        let start = Instant::now();
        let mut presses = PadPresses::default();
        let pressed = presses.update(&keymap, &[press(PadButton::North)], start);
        assert_eq!(pressed.commands, []);
        assert_eq!(pressed.wake, Some(HOLD));
        let tapped = presses.update(
            &keymap,
            &[release(PadButton::North)],
            start + Duration::from_millis(100),
        );
        assert_eq!(tapped.commands, [Command::PageOverview]);
        presses.update(&keymap, &[press(PadButton::North)], start);
        let held = presses.update(&keymap, &[], start + HOLD);
        assert_eq!(held.commands, [Command::Bookmark]);
        let released = presses.update(&keymap, &[release(PadButton::North)], start + HOLD * 2);
        assert_eq!(released.commands, [], "a hold doesn't also tap");
    }

    #[test]
    fn storage_keeps_only_buttons_that_parse() {
        let mut overrides = Overrides::new();
        overrides.insert(
            "view.zoom_in".to_string(),
            vec!["Select + RB".into(), "select+rb".into(), "east".into()],
        );
        overrides.insert("text.bookmark".to_string(), vec!["nonsense".into()]);
        overrides.insert("gone".to_string(), vec!["south".into()]);
        overrides.insert("view.zoom_out".to_string(), Vec::new());
        let kept = sanitize(&overrides);
        assert_eq!(kept["view.zoom_in"], ["select+rb"]);
        assert!(!kept.contains_key("text.bookmark"), "keeps the defaults");
        assert!(!kept.contains_key("gone"));
        assert_eq!(kept["view.zoom_out"], [] as [String; 0], "unbound");
    }

    #[test]
    fn the_users_buttons_take_a_binding_from_a_default() {
        let mut overrides = Overrides::new();
        overrides.insert(
            Command::ZoomIn.id().to_string(),
            vec!["rb".to_string(), "nonsense".to_string()],
        );
        overrides.insert(Command::Bookmark.id().to_string(), Vec::new());
        let keymap = PadKeymap::new(&overrides, false);
        assert_eq!(
            keymap.command(PadChord::tap(PadButton::RightBumper)),
            Some(Command::ZoomIn)
        );
        assert_eq!(
            keymap.command(PadChord::tap(PadButton::Right)),
            Some(Command::NextPage),
            "next page keeps its other button"
        );
        assert_eq!(
            keymap.command(PadChord::tap(PadButton::West)),
            None,
            "bookmark is unbound"
        );
        assert_eq!(keymap.chords(Command::Bookmark), []);
    }

    #[test]
    fn assigning_a_button_takes_it_from_its_command() {
        let keymap = keymap();
        let mut overrides = Overrides::new();
        assign(
            &mut overrides,
            &keymap,
            Command::ZoomIn,
            PadChord::tap(PadButton::West),
        );
        assert_eq!(overrides["view.zoom_in"], ["west"]);
        assert_eq!(overrides["text.bookmark"], [] as [String; 0]);
        let keymap = PadKeymap::new(&overrides, false);
        unbind(
            &mut overrides,
            &keymap,
            Command::ZoomIn,
            PadChord::tap(PadButton::West),
        );
        assert!(
            !overrides.contains_key("view.zoom_in"),
            "back to its defaults (none)"
        );
    }
}
