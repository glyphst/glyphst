//! Sliders (pen component `slider`): a 2px accent rail with a thin upright
//! thumb and the value in mono at the right.

use std::ops::RangeInclusive;

use egui::{Align2, Rect, Response, Sense, Ui, Widget, vec2};

use crate::{
    text,
    theme::{self, Tokens},
};

/// A slider over `range`; the response is changed while the value moves.
#[must_use = "call `ui.add(...)`"]
pub struct Slider<'a> {
    value: &'a mut f32,
    range: RangeInclusive<f32>,
    step: Option<f32>,
    width: f32,
    format: Option<Box<dyn Fn(f32) -> String + 'a>>,
    label: &'a str,
}

impl<'a> Slider<'a> {
    pub fn new(value: &'a mut f32, range: RangeInclusive<f32>) -> Self {
        Self {
            value,
            range,
            step: None,
            width: 160.0,
            format: Some(Box::new(|value| format!("{value:.2}"))),
            label: "",
        }
    }

    pub fn step(mut self, step: f32) -> Self {
        self.step = Some(step).filter(|step| *step > 0.0);
        self
    }

    /// The rail's width; the value label comes after it.
    pub fn width(mut self, width: f32) -> Self {
        self.width = width;
        self
    }

    /// How the value is written; `None` hides it.
    pub fn format(mut self, format: Option<Box<dyn Fn(f32) -> String + 'a>>) -> Self {
        self.format = format;
        self
    }

    /// The accessible name.
    pub fn label(mut self, label: &'a str) -> Self {
        self.label = label;
        self
    }

    fn snap(&self, value: f32) -> f32 {
        let (low, high) = (*self.range.start(), *self.range.end());
        let value = value.clamp(low.min(high), high.max(low));
        match self.step {
            Some(step) => {
                (low + ((value - low) / step).round() * step).clamp(low.min(high), high.max(low))
            }
            None => value,
        }
    }
}

impl Widget for Slider<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let t = Tokens::get(ui.ctx());
        let font = theme::mono(11.0);
        let label = self.format.as_ref().map(|format| format(*self.value));
        let label_width = label
            .as_ref()
            .map_or(0.0, |label| text::width(ui, label, &font).max(28.0) + 10.0);
        let (rect, mut response) = ui.allocate_exact_size(
            vec2(self.width + label_width, 16.0),
            Sense::click_and_drag(),
        );
        let rail =
            Rect::from_min_size(rect.min, vec2(self.width, rect.height())).shrink2(vec2(2.0, 0.0));
        let (low, high) = (*self.range.start(), *self.range.end());
        let span = high - low;
        let enabled = ui.is_enabled();
        if enabled
            && (response.dragged() || response.clicked())
            && let Some(pointer) = response.interact_pointer_pos()
        {
            let fraction = ((pointer.x - rail.left()) / rail.width()).clamp(0.0, 1.0);
            let value = self.snap(low + fraction * span);
            if value != *self.value {
                *self.value = value;
                response.mark_changed();
            }
        }
        if response.has_focus() {
            let step = self.step.unwrap_or(span / 100.0);
            let delta = ui.input(|input| {
                if input.key_pressed(egui::Key::ArrowRight) || input.key_pressed(egui::Key::ArrowUp)
                {
                    step
                } else if input.key_pressed(egui::Key::ArrowLeft)
                    || input.key_pressed(egui::Key::ArrowDown)
                {
                    -step
                } else {
                    0.0
                }
            });
            if delta != 0.0 {
                *self.value = self.snap(*self.value + delta);
                response.mark_changed();
            }
        }
        let value = *self.value;
        response.widget_info(|| egui::WidgetInfo::slider(enabled, f64::from(value), self.label));
        let fraction = if span == 0.0 {
            0.0
        } else {
            ((value - low) / span).clamp(0.0, 1.0)
        };
        let x = rail.left() + fraction * rail.width();
        let y = rect.center().y;
        let (fill, rest, thumb) = if enabled {
            (t.accent, t.rule_strong, t.text)
        } else {
            (t.faint, t.rule, t.faint)
        };
        ui.painter().rect_filled(
            Rect::from_min_max(egui::pos2(rail.left(), y - 1.0), egui::pos2(x, y + 1.0)),
            0,
            fill,
        );
        ui.painter().rect_filled(
            Rect::from_min_max(egui::pos2(x, y - 1.0), egui::pos2(rail.right(), y + 1.0)),
            0,
            rest,
        );
        let thumb_rect = Rect::from_center_size(egui::pos2(x, y), vec2(4.0, 14.0));
        ui.painter().rect_filled(
            if response.hovered() || response.dragged() {
                thumb_rect.expand2(vec2(1.0, 0.0))
            } else {
                thumb_rect
            },
            0,
            if response.dragged() {
                t.accent_text
            } else {
                thumb
            },
        );
        if response.has_focus() {
            ui.painter().rect_stroke(
                rail.expand(3.0),
                t.ctl_radius(),
                egui::Stroke::new(1.0, t.accent),
                egui::StrokeKind::Outside,
            );
        }
        if let Some(label) = label {
            ui.painter().text(
                egui::pos2(rect.right(), y),
                Align2::RIGHT_CENTER,
                label,
                font,
                t.muted,
            );
        }
        response
    }
}
