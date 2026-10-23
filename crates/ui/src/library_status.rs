//! The library slot at the right of a document's header bar (frames 02h):
//! `[ + add to library ]`, `[ adding… ]` while the file is copied, then a
//! ring and a check draw in and `✓ in library` stays. Every state is drawn
//! in one slot as wide as the button, so nothing beside it moves.

use std::f32::consts::{FRAC_PI_2, TAU};

use egui::{Align2, Color32, Pos2, Rect, Response, Sense, Shape, Stroke, Ui, Vec2, pos2, vec2};

use crate::{
    motion, paint, text,
    theme::{self, Tokens},
};

/// The button's label, which sets the slot's width.
pub const ADD: &str = "[ + add to library ]";
const ADDING: &str = "[ adding… ]";
const IN_LIBRARY: &str = "in library";
/// Seconds for the ring and the check to draw in.
const DRAW: f64 = 0.7;
/// The check circle's diameter.
const RING: f32 = 14.0;
const GAP: f32 = 6.0;
/// The bracket button's own padding at each side.
const PAD: f32 = 2.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum State {
    /// Outside the library: the button.
    Add,
    /// Being copied in; takes no clicks.
    Adding,
    /// In the library; `since` is when it came in (egui time), so the
    /// check draws in, or `None` to show it at rest.
    InLibrary { since: Option<f64> },
}

/// The slot's width: the button's.
pub fn width(ui: &Ui) -> f32 {
    text::width(ui, ADD, &theme::mono(12.0)) + PAD * 2.0
}

/// How far the ring and the check have drawn in, 0 to 1.
pub fn progress(ui: &Ui, since: Option<f64>) -> f32 {
    match since {
        Some(since) if !motion::is_reduced(ui.ctx()) => {
            ((ui.input(|i| i.time) - since) / DRAW).clamp(0.0, 1.0) as f32
        }
        _ => 1.0,
    }
}

/// Draws the slot in `ui`'s right-to-left or left-to-right flow.
pub fn show(ui: &mut Ui, state: State) -> Response {
    let t = Tokens::get(ui.ctx());
    let sense = if state == State::Adding {
        Sense::hover()
    } else {
        Sense::click()
    };
    let (rect, response) = ui.allocate_exact_size(vec2(width(ui), 24.0), sense);
    let hovered = response.hovered() && state != State::Adding;
    let label = match state {
        State::Add => ADD,
        State::Adding => ADDING,
        State::InLibrary { .. } => IN_LIBRARY,
    };
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, label));
    let y = rect.center().y;
    let right = rect.right() - PAD;
    match state {
        State::Add => {
            let fill = if hovered {
                t.accent_soft
            } else {
                Color32::TRANSPARENT
            };
            let fill = motion::color(ui.ctx(), response.id, fill);
            ui.painter().rect_filled(rect, t.ctl_radius(), fill);
            ui.painter().text(
                pos2(right, y),
                Align2::RIGHT_CENTER,
                ADD,
                theme::mono(12.0),
                t.accent_text,
            );
        }
        State::Adding => {
            let galley = ui
                .painter()
                .layout_no_wrap(ADDING.into(), theme::mono(12.0), t.faint);
            let pos = pos2(right - galley.size().x, y - galley.size().y / 2.0);
            paint::shimmer_text(ui, pos, galley, t.faint, t.accent_text);
        }
        State::InLibrary { since } => {
            let done = progress(ui, since);
            // The adding label fades as the ring starts.
            let fading = 1.0 - (done / 0.25).min(1.0);
            if fading > 0.0 {
                ui.painter().text(
                    pos2(right, y),
                    Align2::RIGHT_CENTER,
                    ADDING,
                    theme::mono(12.0),
                    t.faint.gamma_multiply(fading),
                );
            }
            let label_color = if hovered { t.text } else { t.muted };
            let galley =
                ui.painter()
                    .layout_no_wrap(IN_LIBRARY.into(), theme::mono(11.0), label_color);
            let content = Rect::from_min_max(
                pos2(right - galley.size().x - GAP - RING, rect.top()),
                pos2(right, rect.bottom()),
            )
            .expand2(vec2(PAD + 2.0, 0.0));
            let fill = if hovered {
                t.hover
            } else {
                Color32::TRANSPARENT
            };
            let fill = motion::color(ui.ctx(), response.id, fill);
            ui.painter().rect_filled(content, t.ctl_radius(), fill);
            let center = pos2(right - galley.size().x - GAP - RING / 2.0, y);
            paint_check(ui, center, done, t.ok);
            let shown = ((done - 0.6) / 0.4).clamp(0.0, 1.0);
            if shown > 0.0 {
                ui.painter().galley_with_override_text_color(
                    pos2(right - galley.size().x, y - galley.size().y / 2.0),
                    galley,
                    label_color.gamma_multiply(shown),
                );
            }
            if done < 1.0 {
                ui.ctx().request_repaint();
            }
        }
    }
    response
}

/// The ring (the first 60% of `done`), then the check inside it.
fn paint_check(ui: &Ui, center: Pos2, done: f32, color: Color32) {
    let stroke = Stroke::new(1.4, color);
    let ring = (done / 0.6).clamp(0.0, 1.0);
    if ring > 0.0 {
        let radius = RING / 2.0 - 0.7;
        let steps = (40.0 * ring).ceil().max(2.0) as usize;
        let points = (0..=steps)
            .map(|step| {
                let angle = -FRAC_PI_2 + TAU * ring * step as f32 / steps as f32;
                center + radius * Vec2::angled(angle)
            })
            .collect();
        ui.painter().add(Shape::line(points, stroke));
    }
    let tick = ((done - 0.45) / 0.55).clamp(0.0, 1.0);
    if tick > 0.0 {
        let corners = [vec2(-3.0, 0.2), vec2(-1.0, 2.2), vec2(3.0, -2.1)].map(|v| center + v);
        let lengths = [
            corners[0].distance(corners[1]),
            corners[1].distance(corners[2]),
        ];
        let mut left = tick * (lengths[0] + lengths[1]);
        let mut points = vec![corners[0]];
        for (index, length) in lengths.into_iter().enumerate() {
            let (from, to) = (corners[index], corners[index + 1]);
            if left >= length {
                points.push(to);
                left -= length;
            } else {
                points.push(from + (to - from) * (left / length));
                break;
            }
        }
        ui.painter().add(Shape::line(points, stroke));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No state moves what is beside it: each takes the button's width.
    #[test]
    fn every_state_keeps_the_buttons_width() {
        let ctx = egui::Context::default();
        ctx.set_fonts(crate::fonts::embedded());
        let mut widths = Vec::new();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            let button = ui
                .add(crate::button::Button::bracket("+ add to library"))
                .rect
                .width();
            widths.push(button);
            for state in [
                State::Add,
                State::Adding,
                State::InLibrary { since: Some(0.0) },
                State::InLibrary { since: None },
            ] {
                widths.push(show(ui, state).rect.width());
            }
        });
        output.textures_delta.clear();
        assert!(
            widths
                .windows(2)
                .all(|pair| (pair[0] - pair[1]).abs() < 0.01),
            "{widths:?}"
        );
    }
}
