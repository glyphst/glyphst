//! Who takes the pad this frame. A window in front (settings, the palette)
//! and the views without a document get keys, so their lists and buttons
//! work as they do from the keyboard. A document gets the pad itself: the
//! sticks and triggers scroll and zoom, and its buttons run the commands
//! bound to them ([`crate::pad_keys`]). B is Escape everywhere, and runs
//! Escape's cancel chain.

use std::time::{Duration, Instant};

use egui::Vec2;

use super::{Dir, GamepadInput, PadButton, PadEvent, PadFrame, Repeat, repeat::stick_dir};
use crate::{
    commands::Command,
    pad_keys::{PadChord, PadKeymap, PadPresses},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Owner {
    /// A shortcut is being recorded: the pad does nothing.
    Nobody,
    /// The pad presses keys.
    Keys,
    Document,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct RouteState {
    pub recording: bool,
    /// A window stands in front: settings or the palette.
    pub modal: bool,
    pub document: bool,
    /// The document shows its page overview.
    pub overview: bool,
    /// A toast with an action shows: X runs it.
    pub toast_action: bool,
}

pub fn owner(state: &RouteState) -> Owner {
    if state.recording {
        Owner::Nobody
    } else if state.modal || !state.document {
        Owner::Keys
    } else {
        Owner::Document
    }
}

/// How far the right stick goes before the ring opens, how far it aims a
/// slot, and how far back it comes to let go (and pick).
const RING_OPEN: f32 = 0.6;
const RING_AIM: f32 = 0.5;
const RING_LET_GO: f32 = 0.25;

/// What the right stick does to the ring.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RingStep {
    /// Opens it, aimed so, with the slot aimed at (it stays aimed while
    /// the stick comes back to let go).
    Open(Vec2, Option<usize>),
    Aim(Vec2, Option<usize>),
    /// Letting go (or A) picks the slot last aimed at.
    Pick(usize),
    /// B, or letting go before aiming at one.
    Cancel,
}

/// A frame of pad input, routed.
#[derive(Debug, Default, PartialEq)]
pub struct Routed {
    /// Keys to press, before the frame reads its input.
    pub keys: Vec<(egui::Key, egui::Modifiers)>,
    pub document: GamepadInput,
    /// Commands the document's buttons ran.
    pub commands: Vec<Command>,
    /// When a held direction repeats, or a held button becomes a hold.
    pub wake: Option<Duration>,
    pub ring: Option<RingStep>,
}

#[derive(Debug, Default)]
pub struct Router {
    /// The buttons held, in the order they were pressed.
    held: Vec<PadButton>,
    repeat: Repeat,
    stick: Option<Dir>,
    presses: PadPresses,
    /// The ring is open, with the slot last aimed at.
    ring: Option<Option<usize>>,
    /// The ring closed with the stick still out: it opens again only once
    /// the stick has come back.
    ring_spent: bool,
}

impl Router {
    pub fn route(
        &mut self,
        frame: &PadFrame,
        state: &RouteState,
        keymap: &PadKeymap,
        now: Instant,
    ) -> Routed {
        let mut pressed = Vec::new();
        for event in &frame.events {
            match event {
                PadEvent::Pressed(button) => {
                    pressed.push(*button);
                    if !self.held.contains(button) {
                        self.held.push(*button);
                    }
                }
                PadEvent::Released(button) => self.held.retain(|held| held != button),
                PadEvent::Disconnected(_) => self.held.clear(),
                PadEvent::Connected { .. } => {}
            }
        }
        let owner = owner(state);
        if let Some(step) = self.ring_step(frame, owner, &pressed) {
            // The ring takes the pad while it is open.
            self.presses
                .update(&PadKeymap::default(), &frame.events, now);
            return Routed {
                ring: Some(step),
                ..Routed::default()
            };
        }
        // The stick moves through lists and the page overview; on a
        // document it scrolls.
        let grid = owner == Owner::Document && state.overview;
        self.stick = if owner == Owner::Keys || grid {
            stick_dir(frame.axes.left, self.stick)
        } else {
            None
        };
        let dpad = self.held.iter().rev().find_map(|button| button.dir());
        let (repeated, wake) = self.repeat.update(dpad.or(self.stick), now);
        // A press moves once even when released in the same frame; holding
        // it (or the stick) repeats.
        let mut dirs: Vec<Dir> = pressed.iter().filter_map(|button| button.dir()).collect();
        if let Some(dir) = repeated
            && !dirs.contains(&dir)
        {
            dirs.push(dir);
        }
        let mut routed = Routed {
            wake,
            ..Routed::default()
        };
        match owner {
            Owner::Nobody | Owner::Keys => {
                // The bindings still see releases, so nothing stays held.
                self.presses
                    .update(&PadKeymap::default(), &frame.events, now);
                if owner == Owner::Keys {
                    routed.keys = keys(&dirs, &pressed);
                }
            }
            Owner::Document => {
                routed.document = GamepadInput::motion(&frame.axes);
                let none = egui::Modifiers::NONE;
                let mut events = Vec::new();
                for event in &frame.events {
                    match event {
                        PadEvent::Pressed(PadButton::East) => {
                            routed.keys.push((egui::Key::Escape, none));
                        }
                        // A opens the overview's page, as Enter does.
                        PadEvent::Pressed(PadButton::South) if grid => {
                            routed.keys.push((egui::Key::Enter, none));
                        }
                        // X is the toast's.
                        PadEvent::Pressed(PadButton::West) if state.toast_action => {}
                        // On the overview the d-pad moves a cell at a time.
                        PadEvent::Pressed(button) if grid && button.dir().is_some() => {}
                        event => events.push(event.clone()),
                    }
                }
                let fired = self.presses.update(keymap, &events, now);
                routed.commands = fired.commands;
                routed.wake = earliest(routed.wake, fired.wake);
                if grid {
                    // The triggers size the cells instead.
                    routed.document.scroll = [0.0, 0.0];
                    routed.commands.extend(dirs.iter().map(|&dir| match dir {
                        Dir::Up => Command::ScrollUp,
                        Dir::Down => Command::ScrollDown,
                        Dir::Left => Command::ScrollLeft,
                        Dir::Right => Command::ScrollRight,
                    }));
                    return routed;
                }
                // A held d-pad repeats what it scrolls or turns.
                if let Some(dir) = repeated
                    && !pressed.contains(&dir.button())
                    && self.presses.repeats(dir.button())
                    && let Some(command) = keymap.command(PadChord::tap(dir.button()))
                    && command.counted()
                {
                    routed.commands.push(command);
                }
            }
        }
        routed
    }
}

impl Router {
    /// The right stick opens the ring and aims it; letting go picks.
    fn ring_step(
        &mut self,
        frame: &PadFrame,
        owner: Owner,
        pressed: &[PadButton],
    ) -> Option<RingStep> {
        let stick = frame.axes.right;
        let reach = stick.length();
        self.ring_spent &= reach >= RING_LET_GO;
        let Some(aimed) = self.ring else {
            if owner == Owner::Document && reach > RING_OPEN && !self.ring_spent {
                let slot = glyphst_ui::ring::slot_toward(stick);
                self.ring = Some(Some(slot));
                return Some(RingStep::Open(stick, Some(slot)));
            }
            return None;
        };
        let close = |router: &mut Self, step| {
            router.ring = None;
            router.ring_spent = reach >= RING_LET_GO;
            Some(step)
        };
        if owner != Owner::Document || pressed.contains(&PadButton::East) {
            return close(self, RingStep::Cancel);
        }
        let aimed = if reach >= RING_AIM {
            Some(glyphst_ui::ring::slot_toward(stick))
        } else {
            aimed
        };
        self.ring = Some(aimed);
        if pressed.contains(&PadButton::South) || reach < RING_LET_GO {
            return close(self, aimed.map_or(RingStep::Cancel, RingStep::Pick));
        }
        Some(RingStep::Aim(stick, aimed))
    }
}

/// The keys a frame presses: arrows, A Enter, B Escape and the bumpers
/// Tab.
fn keys(dirs: &[Dir], pressed: &[PadButton]) -> Vec<(egui::Key, egui::Modifiers)> {
    let none = egui::Modifiers::NONE;
    let mut keys: Vec<_> = dirs.iter().map(|&dir| (arrow(dir), none)).collect();
    for button in pressed {
        match button {
            PadButton::South => keys.push((egui::Key::Enter, none)),
            PadButton::East => keys.push((egui::Key::Escape, none)),
            PadButton::RightBumper => keys.push((egui::Key::Tab, none)),
            PadButton::LeftBumper => keys.push((egui::Key::Tab, egui::Modifiers::SHIFT)),
            _ => {}
        }
    }
    keys
}

fn earliest(a: Option<Duration>, b: Option<Duration>) -> Option<Duration> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

fn arrow(dir: Dir) -> egui::Key {
    match dir {
        Dir::Up => egui::Key::ArrowUp,
        Dir::Down => egui::Key::ArrowDown,
        Dir::Left => egui::Key::ArrowLeft,
        Dir::Right => egui::Key::ArrowRight,
    }
}

/// The key presses `keys` stands for, as egui events: a press and a
/// release each.
pub fn key_events(keys: &[(egui::Key, egui::Modifiers)]) -> impl Iterator<Item = egui::Event> + '_ {
    keys.iter().flat_map(|&(key, modifiers)| {
        [true, false].map(|pressed| egui::Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers,
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{keys::Overrides, pad::Axes};

    fn frame(events: Vec<PadEvent>) -> PadFrame {
        PadFrame {
            events,
            axes: Axes::default(),
        }
    }

    fn tap(button: PadButton) -> Vec<PadEvent> {
        vec![PadEvent::Pressed(button), PadEvent::Released(button)]
    }

    fn keymap() -> PadKeymap {
        PadKeymap::new(&Overrides::new(), false)
    }

    const DOCUMENT: RouteState = RouteState {
        recording: false,
        modal: false,
        document: true,
        overview: false,
        toast_action: false,
    };

    #[test]
    fn windows_in_front_and_recording_come_before_the_document() {
        assert_eq!(owner(&DOCUMENT), Owner::Document);
        let modal = RouteState {
            modal: true,
            ..DOCUMENT
        };
        assert_eq!(
            owner(&modal),
            Owner::Keys,
            "settings or the palette in front"
        );
        let home = RouteState {
            document: false,
            ..DOCUMENT
        };
        assert_eq!(owner(&home), Owner::Keys);
        let recording = RouteState {
            recording: true,
            ..modal
        };
        assert_eq!(owner(&recording), Owner::Nobody);
    }

    #[test]
    fn under_a_window_the_pad_presses_keys_and_leaves_the_document_alone() {
        let mut router = Router::default();
        let state = RouteState {
            modal: true,
            ..DOCUMENT
        };
        let mut events = tap(PadButton::Down);
        events.extend(tap(PadButton::South));
        events.extend(tap(PadButton::RightBumper));
        let mut frame = frame(events);
        frame.axes.left = egui::vec2(0.0, 0.3);
        let routed = router.route(&frame, &state, &keymap(), Instant::now());
        assert_eq!(
            routed.keys,
            [
                (egui::Key::ArrowDown, egui::Modifiers::NONE),
                (egui::Key::Enter, egui::Modifiers::NONE),
                (egui::Key::Tab, egui::Modifiers::NONE),
            ]
        );
        assert_eq!(routed.document, GamepadInput::default(), "no scrolling");
        assert_eq!(routed.commands, []);
    }

    #[test]
    fn the_stick_moves_through_lists_and_repeats() {
        let start = Instant::now();
        let mut router = Router::default();
        let state = RouteState::default();
        let keymap = keymap();
        let mut pushed = frame(Vec::new());
        pushed.axes.left = egui::vec2(0.0, -0.9);
        let up = vec![(egui::Key::ArrowUp, egui::Modifiers::NONE)];
        assert_eq!(router.route(&pushed, &state, &keymap, start).keys, up);
        let held = router.route(&pushed, &state, &keymap, start + Duration::from_millis(100));
        assert_eq!(held.keys, []);
        assert_eq!(held.wake, Some(Duration::from_millis(250)));
        let later = start + Duration::from_millis(360);
        assert_eq!(router.route(&pushed, &state, &keymap, later).keys, up);
    }

    #[test]
    fn on_a_document_buttons_run_their_commands_and_b_is_escape() {
        let mut router = Router::default();
        let keymap = keymap();
        let mut events = tap(PadButton::RightBumper);
        events.extend(tap(PadButton::Down));
        events.extend(tap(PadButton::East));
        let routed = router.route(&frame(events), &DOCUMENT, &keymap, Instant::now());
        assert_eq!(routed.commands, [Command::NextPage, Command::ScrollDown]);
        assert_eq!(routed.keys, [(egui::Key::Escape, egui::Modifiers::NONE)]);
        let overview = RouteState {
            overview: true,
            ..DOCUMENT
        };
        let routed = router.route(
            &frame(tap(PadButton::South)),
            &overview,
            &keymap,
            Instant::now(),
        );
        assert_eq!(
            routed.keys,
            [(egui::Key::Enter, egui::Modifiers::NONE)],
            "A opens the overview's page"
        );
        assert_eq!(routed.commands, [], "rather than fitting the width");
    }

    #[test]
    fn on_the_overview_the_stick_and_dpad_move_a_cell_at_a_time() {
        let mut router = Router::default();
        let overview = RouteState {
            overview: true,
            ..DOCUMENT
        };
        let mut pushed = frame(tap(PadButton::Right));
        pushed.axes.left = egui::vec2(0.0, 0.9);
        pushed.axes.right_trigger = 1.0;
        let routed = router.route(&pushed, &overview, &keymap(), Instant::now());
        assert_eq!(routed.commands, [Command::ScrollRight, Command::ScrollDown]);
        assert_eq!(
            routed.document.scroll,
            [0.0, 0.0],
            "the stick doesn't scroll"
        );
        assert!(routed.document.zoom > 0.0, "the triggers zoom the grid");
    }

    #[test]
    fn a_held_dpad_repeats_and_a_disconnect_lets_go() {
        let start = Instant::now();
        let mut router = Router::default();
        let keymap = keymap();
        let pressed = router.route(
            &frame(vec![PadEvent::Pressed(PadButton::Down)]),
            &DOCUMENT,
            &keymap,
            start,
        );
        assert_eq!(pressed.commands, [Command::ScrollDown]);
        let later = start + Duration::from_millis(400);
        let repeated = router.route(&frame(Vec::new()), &DOCUMENT, &keymap, later);
        assert_eq!(repeated.commands, [Command::ScrollDown]);
        let gone = router.route(
            &frame(vec![PadEvent::Disconnected(0)]),
            &DOCUMENT,
            &keymap,
            later + Duration::from_millis(400),
        );
        assert_eq!(gone.commands, []);
    }

    #[test]
    fn a_press_held_into_a_window_is_let_go_there() {
        let mut router = Router::default();
        let keymap = keymap();
        let now = Instant::now();
        router.route(
            &frame(vec![PadEvent::Pressed(PadButton::Select)]),
            &DOCUMENT,
            &keymap,
            now,
        );
        let modal = RouteState {
            modal: true,
            ..DOCUMENT
        };
        router.route(
            &frame(vec![PadEvent::Released(PadButton::Select)]),
            &modal,
            &keymap,
            now,
        );
        let routed = router.route(&frame(tap(PadButton::RightBumper)), &DOCUMENT, &keymap, now);
        assert_eq!(
            routed.commands,
            [Command::NextPage],
            "select is no longer held, so this is no chord"
        );
    }

    fn right_stick(at: Vec2) -> PadFrame {
        PadFrame {
            events: Vec::new(),
            axes: Axes {
                right: at,
                ..Axes::default()
            },
        }
    }

    #[test]
    fn the_right_stick_opens_the_ring_and_letting_go_picks() {
        let mut router = Router::default();
        let keymap = keymap();
        let now = Instant::now();
        let mut route = |frame: &PadFrame| router.route(frame, &DOCUMENT, &keymap, now);
        assert_eq!(route(&right_stick(egui::vec2(0.3, 0.0))).ring, None);
        assert_eq!(
            route(&right_stick(egui::vec2(0.0, -0.9))).ring,
            Some(RingStep::Open(egui::vec2(0.0, -0.9), Some(0)))
        );
        let aimed = route(&right_stick(egui::vec2(0.9, 0.0)));
        assert_eq!(
            aimed.ring,
            Some(RingStep::Aim(egui::vec2(0.9, 0.0), Some(2)))
        );
        // On the way back the slot stays the one last aimed at.
        route(&right_stick(egui::vec2(0.3, 0.0)));
        assert_eq!(
            route(&right_stick(Vec2::ZERO)).ring,
            Some(RingStep::Pick(2))
        );
        assert_eq!(route(&right_stick(Vec2::ZERO)).ring, None, "closed");
    }

    #[test]
    fn the_ring_takes_the_pad_and_b_cancels_it() {
        let mut router = Router::default();
        let keymap = keymap();
        let now = Instant::now();
        router.route(&right_stick(egui::vec2(0.9, 0.0)), &DOCUMENT, &keymap, now);
        let mut pressed = right_stick(egui::vec2(0.9, 0.0));
        pressed.events = tap(PadButton::RightBumper);
        let routed = router.route(&pressed, &DOCUMENT, &keymap, now);
        assert_eq!(routed.commands, [], "no page turns under the ring");
        pressed.events = tap(PadButton::East);
        let routed = router.route(&pressed, &DOCUMENT, &keymap, now);
        assert_eq!(routed.ring, Some(RingStep::Cancel));
        assert_eq!(
            routed.keys,
            [],
            "B closes the ring, not the document's things"
        );
        let modal = RouteState {
            modal: true,
            ..DOCUMENT
        };
        let still_out = router.route(&right_stick(egui::vec2(0.9, 0.0)), &DOCUMENT, &keymap, now);
        assert_eq!(
            still_out.ring, None,
            "it opens again only once the stick is back"
        );
        router.route(&right_stick(Vec2::ZERO), &DOCUMENT, &keymap, now);
        router.route(&right_stick(egui::vec2(0.9, 0.0)), &DOCUMENT, &keymap, now);
        let routed = router.route(&right_stick(egui::vec2(0.9, 0.0)), &modal, &keymap, now);
        assert_eq!(
            routed.ring,
            Some(RingStep::Cancel),
            "a window in front closes it"
        );
    }

    #[test]
    fn key_events_press_and_release() {
        let events: Vec<_> = key_events(&[(egui::Key::Enter, egui::Modifiers::NONE)]).collect();
        assert!(matches!(
            events[..],
            [
                egui::Event::Key {
                    key: egui::Key::Enter,
                    pressed: true,
                    ..
                },
                egui::Event::Key {
                    key: egui::Key::Enter,
                    pressed: false,
                    ..
                }
            ]
        ));
    }
}
