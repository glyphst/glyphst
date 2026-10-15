//! Glyphst's design system on egui: the tokens, type and icons of the glyph
//! design (`design/main.pen`) and one reusable component per pen component.
//! Components read [`theme::Tokens::get`] only, so callers never pick
//! colours, radii or fonts.

pub mod fonts;
mod icon_data;
pub mod icons;
pub mod theme;
