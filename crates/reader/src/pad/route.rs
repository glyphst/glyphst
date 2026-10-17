//! Who takes the pad this frame. A window in front (settings, the palette)
//! and the views without a document get keys, so their lists and buttons
//! work as they do from the keyboard. A document gets the pad itself:
//! scrolling, zoom and its actions. B is Escape everywhere, and runs
//! Escape's cancel chain.

use std::time::{Duration, Instant};

use super::{Dir, GamepadInput, PadButton, PadEvent, PadFrame, Repeat, repeat::stick_dir};
use crate::document_tab::ViewAction;

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

/// A frame of pad input, routed.
#[derive(Debug, Default, PartialEq)]
pub struct Routed {
    /// Keys to press, before the frame reads its input.
    pub keys: Vec<(egui::Key, egui::Modifiers)>,
    pub document: GamepadInput,
    /// When a held direction repeats next.
    pub wake: Option<Duration>,
}

#[derive(Debug, Default)]
pub struct Router {
    /// The buttons held, in the order they were pressed.
    held: Vec<PadButton>,
    repeat: Repeat,
    stick: Option<Dir>,
}

impl Router {
    pub fn route(&mut self, frame: &PadFrame, state: &RouteState, now: Instant) -> Routed {
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
        // The stick moves through lists; on a document it scrolls.
        self.stick = match owner {
            Owner::Keys => stick_dir(frame.axes.left, self.stick),
            _ => None,
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
            Owner::Nobody => {}
            Owner::Keys => {
                let none = egui::Modifiers::NONE;
                for dir in dirs {
                    routed.keys.push((arrow(dir), none));
                }
                for button in pressed {
                    match button {
                        PadButton::South => routed.keys.push((egui::Key::Enter, none)),
                        PadButton::East => routed.keys.push((egui::Key::Escape, none)),
                        PadButton::RightBumper => routed.keys.push((egui::Key::Tab, none)),
                        PadButton::LeftBumper => {
                            routed.keys.push((egui::Key::Tab, egui::Modifiers::SHIFT));
                        }
                        _ => {}
                    }
                }
            }
            Owner::Document => {
                routed.document = GamepadInput::motion(&frame.axes);
                let actions = &mut routed.document.actions;
                for dir in dirs {
                    actions.push(match dir {
                        Dir::Up => ViewAction::Step(-1),
                        Dir::Down => ViewAction::Step(1),
                        Dir::Left => ViewAction::PreviousPage,
                        Dir::Right => ViewAction::NextPage,
                    });
                }
                for button in pressed {
                    match button {
                        PadButton::LeftBumper => actions.push(ViewAction::PreviousPage),
                        PadButton::RightBumper => actions.push(ViewAction::NextPage),
                        PadButton::Start => actions.push(ViewAction::TogglePresent),
                        // A opens the overview's page, as Enter does.
                        PadButton::South if state.overview => {
                            routed.keys.push((egui::Key::Enter, egui::Modifiers::NONE));
                        }
                        PadButton::South => actions.push(ViewAction::FitWidth),
                        PadButton::East => {
                            routed.keys.push((egui::Key::Escape, egui::Modifiers::NONE));
                        }
                        _ => {}
                    }
                }
            }
        }
        routed
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
    use crate::pad::Axes;

    fn frame(events: Vec<PadEvent>) -> PadFrame {
        PadFrame {
            events,
            axes: Axes::default(),
        }
    }

    fn tap(button: PadButton) -> Vec<PadEvent> {
        vec![PadEvent::Pressed(button), PadEvent::Released(button)]
    }

    const DOCUMENT: RouteState = RouteState {
        recording: false,
        modal: false,
        document: true,
        overview: false,
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
        let routed = router.route(&frame, &state, Instant::now());
        assert_eq!(
            routed.keys,
            [
                (egui::Key::ArrowDown, egui::Modifiers::NONE),
                (egui::Key::Enter, egui::Modifiers::NONE),
                (egui::Key::Tab, egui::Modifiers::NONE),
            ]
        );
        assert_eq!(routed.document, GamepadInput::default(), "no scrolling");
    }

    #[test]
    fn the_stick_moves_through_lists_and_repeats() {
        let start = Instant::now();
        let mut router = Router::default();
        let state = RouteState::default();
        let mut pushed = frame(Vec::new());
        pushed.axes.left = egui::vec2(0.0, -0.9);
        let up = vec![(egui::Key::ArrowUp, egui::Modifiers::NONE)];
        assert_eq!(router.route(&pushed, &state, start).keys, up);
        let held = router.route(&pushed, &state, start + Duration::from_millis(100));
        assert_eq!(held.keys, []);
        assert_eq!(held.wake, Some(Duration::from_millis(250)));
        assert_eq!(
            router
                .route(&pushed, &state, start + Duration::from_millis(360))
                .keys,
            up
        );
    }

    #[test]
    fn on_a_document_the_pad_turns_pages_and_b_is_escape() {
        let mut router = Router::default();
        let mut events = tap(PadButton::RightBumper);
        events.extend(tap(PadButton::Down));
        events.extend(tap(PadButton::East));
        let routed = router.route(&frame(events), &DOCUMENT, Instant::now());
        assert_eq!(
            routed.document.actions,
            [ViewAction::Step(1), ViewAction::NextPage]
        );
        assert_eq!(routed.keys, [(egui::Key::Escape, egui::Modifiers::NONE)]);
        let overview = RouteState {
            overview: true,
            ..DOCUMENT
        };
        let routed = router.route(&frame(tap(PadButton::South)), &overview, Instant::now());
        assert_eq!(
            routed.keys,
            [(egui::Key::Enter, egui::Modifiers::NONE)],
            "A opens the overview's page"
        );
        assert_eq!(routed.document.actions, []);
    }

    #[test]
    fn a_held_dpad_repeats_and_a_disconnect_lets_go() {
        let start = Instant::now();
        let mut router = Router::default();
        let pressed = router.route(
            &frame(vec![PadEvent::Pressed(PadButton::Down)]),
            &DOCUMENT,
            start,
        );
        assert_eq!(pressed.document.actions, [ViewAction::Step(1)]);
        let later = start + Duration::from_millis(400);
        let repeated = router.route(&frame(Vec::new()), &DOCUMENT, later);
        assert_eq!(repeated.document.actions, [ViewAction::Step(1)]);
        let gone = router.route(
            &frame(vec![PadEvent::Disconnected(0)]),
            &DOCUMENT,
            later + Duration::from_millis(400),
        );
        assert_eq!(gone.document.actions, []);
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
