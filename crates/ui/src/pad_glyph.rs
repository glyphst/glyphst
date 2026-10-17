//! Gamepad glyphs and hints (frame 01b: pad/face, pad/button, pad/dpad,
//! pad-hint).
//!
//! Hints follow the last input, as games show them: pad glyphs after a pad,
//! key caps after the keyboard. The app says which it was each frame with
//! [`set_input`].

/// Where the last input came from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum InputDevice {
    #[default]
    Pointer,
    Keyboard,
    Pad,
}

fn input_id() -> egui::Id {
    egui::Id::new("glyphst-pad-input")
}

pub fn set_input(ctx: &egui::Context, device: InputDevice) {
    ctx.data_mut(|data| data.insert_temp(input_id(), device));
}

pub fn input(ctx: &egui::Context) -> InputDevice {
    ctx.data(|data| data.get_temp(input_id()))
        .unwrap_or_default()
}
