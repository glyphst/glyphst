//! Prompts (pen components `prompt` and `prompt/choice`, frames 01b, 09a,
//! 09j): a modal card with a title, a line of prose and its choices. Each
//! choice shows how to pick it from the last input: its pad face after a
//! pad, its key after the keyboard. Enter picks the South choice, Escape
//! the East one; the pad's other faces come through
//! [`pad_glyph::push_faces`].

use egui::{Align2, Rect, Sense, StrokeKind, Ui, Vec2, vec2};

use crate::{
    icons, key,
    pad_glyph::{self, Face, Glyph, InputDevice, Scale},
    text,
    theme::{self, Tokens},
};

#[derive(Clone, Copy, Debug)]
pub struct Choice<'a> {
    pub label: &'a str,
    pub face: Face,
    /// The key that picks it, as `key::display` writes keys.
    pub key: Option<&'a str>,
    /// The accent one: what pressing A does.
    pub primary: bool,
}

impl<'a> Choice<'a> {
    pub fn new(label: &'a str, face: Face) -> Self {
        let key = match face {
            Face::South => Some("enter"),
            Face::East => Some("esc"),
            _ => None,
        };
        Self {
            label,
            face,
            key,
            primary: face == Face::South,
        }
    }
}

pub struct Prompt<'a> {
    pub id: egui::Id,
    pub icon: Option<&'a str>,
    pub title: &'a str,
    pub body: &'a str,
    /// Shown from the left: the first ones sit apart, the last at the
    /// right edge (`don't ask again … not now  [gamepad mode]`).
    pub choices: &'a [Choice<'a>],
    /// How many choices sit at the left.
    pub apart: usize,
    pub scale: Scale,
}

struct Sizes {
    width: f32,
    title: f32,
    body: f32,
    label: f32,
    padding: f32,
    gap: f32,
    icon: f32,
}

impl Scale {
    fn prompt(self) -> Sizes {
        match self {
            Scale::Chrome => Sizes {
                width: 440.0,
                title: 17.0,
                body: 14.0,
                label: 12.0,
                padding: 20.0,
                gap: 12.0,
                icon: 18.0,
            },
            Scale::Couch => Sizes {
                width: 640.0,
                title: 28.0,
                body: 19.0,
                label: 17.0,
                padding: 36.0,
                gap: 18.0,
                icon: 28.0,
            },
        }
    }
}

impl Prompt<'_> {
    /// Shows it; the index of the choice picked this frame.
    pub fn show(self, ctx: &egui::Context) -> Option<usize> {
        let t = Tokens::get(ctx);
        let sizes = self.scale.prompt();
        let mut picked = None;
        // Keys and faces pick before the frame draws.
        let by_face = |face| self.choices.iter().position(|choice| choice.face == face);
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter)) {
            picked = by_face(Face::South);
        }
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            picked = picked.or(by_face(Face::East));
        }
        for face in pad_glyph::pressed_faces(ctx) {
            if !matches!(face, Face::South | Face::East) {
                picked = picked.or(by_face(face));
            }
        }
        let pad = pad_glyph::input(ctx) == InputDevice::Pad;
        egui::Modal::new(self.id)
            .backdrop_color(t.scrim)
            .frame(crate::pane::popover(ctx).inner_margin(egui::Margin::same(sizes.padding as i8)))
            .show(ctx, |ui| {
                ui.set_width(sizes.width - 2.0 * sizes.padding);
                ui.spacing_mut().item_spacing.y = sizes.gap;
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = sizes.gap * 0.8;
                    if let Some(icon) = self.icon {
                        let (rect, _) =
                            ui.allocate_exact_size(Vec2::splat(sizes.icon), Sense::hover());
                        icons::paint(ui, rect, icon, sizes.icon, t.accent_text);
                    }
                    ui.label(
                        egui::RichText::new(self.title)
                            .font(theme::semibold(sizes.title))
                            .color(t.text),
                    );
                });
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(self.body)
                            .font(theme::regular(sizes.body))
                            .color(t.text_2),
                    )
                    .wrap(),
                );
                ui.add_space(sizes.gap * 0.4);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = sizes.gap * 0.7;
                    let (left, right) = self.choices.split_at(self.apart.min(self.choices.len()));
                    for (index, choice) in left.iter().enumerate() {
                        if choice_button(ui, choice, pad, self.scale, false).clicked() {
                            picked = Some(index);
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        for (offset, choice) in right.iter().enumerate().rev() {
                            if choice_button(ui, choice, pad, self.scale, true).clicked() {
                                picked = Some(left.len() + offset);
                            }
                        }
                    });
                });
            });
        picked
    }
}

/// A choice: its glyph or key, then its label; outlined, or `accent` for
/// the primary one. `outlined` is false for the quiet ones at the left.
fn choice_button(
    ui: &mut Ui,
    choice: &Choice<'_>,
    pad: bool,
    scale: Scale,
    outlined: bool,
) -> egui::Response {
    let t = Tokens::get(ui.ctx());
    let sizes = scale.prompt();
    let font = if choice.primary {
        theme::mono_strong(sizes.label)
    } else {
        theme::mono(sizes.label)
    };
    let (fill, color) = match (choice.primary, outlined) {
        (true, _) => (t.accent, t.on_accent),
        (false, true) => (egui::Color32::TRANSPARENT, t.text),
        (false, false) => (egui::Color32::TRANSPARENT, t.muted),
    };
    let glyph = Glyph::Face(choice.face);
    let mark = if pad {
        pad_glyph::size(ui, glyph, scale)
    } else {
        choice.key.map_or(Vec2::ZERO, |label| key::size(ui, label))
    };
    let (pad_x, pad_y) = match scale {
        Scale::Chrome => (12.0, 6.0),
        Scale::Couch => (18.0, 10.0),
    };
    let gap = if mark.x > 0.0 { sizes.gap * 0.6 } else { 0.0 };
    let label_width = text::width(ui, choice.label, &font);
    let size = vec2(
        mark.x + gap + label_width + 2.0 * pad_x,
        mark.y.max(sizes.label * 1.3) + 2.0 * pad_y,
    );
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let painter = ui.painter();
    let hovered = response.hovered();
    if fill != egui::Color32::TRANSPARENT {
        painter.rect_filled(rect, t.ctl_radius(), fill);
    } else if outlined || hovered {
        painter.rect_stroke(rect, t.ctl_radius(), t.edge(), StrokeKind::Inside);
    }
    let mark_rect = Rect::from_min_size(rect.left_center() + vec2(pad_x, -mark.y / 2.0), mark);
    let on = choice.primary.then_some(t.on_accent);
    if pad {
        pad_glyph::paint(ui, mark_rect, glyph, scale, on);
    } else if let Some(label) = choice.key {
        key::paint(ui, mark_rect, label, on);
    }
    painter.text(
        rect.left_center() + vec2(pad_x + mark.x + gap, 0.0),
        Align2::LEFT_CENTER,
        choice.label,
        font,
        color,
    );
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}
