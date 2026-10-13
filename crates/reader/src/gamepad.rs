//! Gamepad navigation through gilrs.
//!
//! Left stick and D-pad scroll, triggers zoom, bumpers change page, A fits
//! the page width and Start toggles presentation mode.

use gilrs::{Axis, Button, EventType, Gilrs};

use crate::document_tab::ViewAction;

/// Stick positions below this magnitude are ignored.
const DEADZONE: f32 = 0.15;
/// View pixels per second at full stick deflection.
pub const MAX_SCROLL_SPEED: f32 = 2400.0;
/// Natural-log zoom change per second at a fully pressed trigger.
pub const MAX_ZOOM_RATE: f32 = 2.5;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct GamepadInput {
    /// Scroll velocity in view pixels per second; positive is down/right.
    pub scroll: [f32; 2],
    /// Zoom rate in natural-log units per second; positive zooms in.
    pub zoom: f32,
    pub actions: Vec<ViewAction>,
}

impl GamepadInput {
    pub fn is_active(&self) -> bool {
        self.scroll != [0.0, 0.0] || self.zoom != 0.0 || !self.actions.is_empty()
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

pub fn action_for(button: Button) -> Option<ViewAction> {
    Some(match button {
        Button::LeftTrigger => ViewAction::PreviousPage,
        Button::RightTrigger => ViewAction::NextPage,
        Button::South => ViewAction::FitWidth,
        Button::Start => ViewAction::TogglePresent,
        Button::DPadUp => ViewAction::Step(-1),
        Button::DPadDown => ViewAction::Step(1),
        _ => return None,
    })
}

pub struct Gamepads {
    gilrs: Gilrs,
}

impl Gamepads {
    /// `None` when no gamepad backend is available (for example without
    /// access to input devices).
    pub fn new() -> Result<Self, String> {
        Gilrs::new()
            .map(|gilrs| Self { gilrs })
            .map_err(|error| error.to_string())
    }

    pub fn connected(&self) -> Vec<String> {
        self.gilrs
            .gamepads()
            .map(|(_, gamepad)| gamepad.name().to_string())
            .collect()
    }

    /// Drains pending events and reads the current stick and trigger state.
    pub fn poll(&mut self) -> GamepadInput {
        let mut input = GamepadInput::default();
        while let Some(event) = self.gilrs.next_event() {
            if let EventType::ButtonPressed(button, _) = event.event
                && let Some(action) = action_for(button)
            {
                input.actions.push(action);
            }
        }
        for (_, gamepad) in self.gilrs.gamepads() {
            input.scroll[0] += axis_speed(gamepad.value(Axis::LeftStickX)) * MAX_SCROLL_SPEED;
            // gilrs reports up as positive.
            input.scroll[1] -= axis_speed(gamepad.value(Axis::LeftStickY)) * MAX_SCROLL_SPEED;
            let trigger = |button| gamepad.button_data(button).map_or(0.0, |data| data.value());
            input.zoom +=
                (trigger(Button::RightTrigger2) - trigger(Button::LeftTrigger2)) * MAX_ZOOM_RATE;
        }
        input
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
    fn buttons_map_to_reader_actions() {
        assert_eq!(action_for(Button::RightTrigger), Some(ViewAction::NextPage));
        assert_eq!(action_for(Button::DPadUp), Some(ViewAction::Step(-1)));
        assert_eq!(action_for(Button::North), None);
        assert!(!GamepadInput::default().is_active());
    }
}
