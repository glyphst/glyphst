//! On/off controls (pen components `checkbox`, `switch/on`, `switch/off`):
//! a `[x] label` text checkbox for lists and a square-knobbed switch for
//! settings rows.

use egui::{Align2, Rect, Response, Sense, StrokeKind, Ui, Vec2, Widget, vec2};

use crate::{
    text,
    theme::{self, Tokens},
};

/// `[x] label`; the response is changed when it flips.
pub struct Checkbox<'a> {
    on: &'a mut bool,
    label: &'a str,
}

impl<'a> Checkbox<'a> {
    pub fn new(on: &'a mut bool, label: &'a str) -> Self {
        Self { on, label }
    }
}

impl Widget for Checkbox<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let t = Tokens::get(ui.ctx());
        let font = theme::mono(12.0);
        let mark = if *self.on { "[x]" } else { "[ ]" };
        let mark_width = text::width(ui, "[x]", &font);
        let width = mark_width + 8.0 + text::width(ui, self.label, &font);
        let (rect, mut response) = ui.allocate_exact_size(vec2(width, t.row(20.0)), Sense::click());
        if response.clicked() {
            *self.on = !*self.on;
            response.mark_changed();
        }
        let enabled = ui.is_enabled();
        let on = *self.on;
        response.widget_info(|| {
            egui::WidgetInfo::selected(egui::WidgetType::Checkbox, enabled, on, self.label)
        });
        let (mark_color, label_color) = match (enabled, on, response.hovered()) {
            (false, ..) => (t.faint, t.faint),
            (true, true, _) => (t.accent, t.text_2),
            (true, false, true) => (t.text_2, t.text),
            (true, false, false) => (t.faint, t.text_2),
        };
        let y = rect.center().y;
        ui.painter().text(
            egui::pos2(rect.left(), y),
            Align2::LEFT_CENTER,
            mark,
            font.clone(),
            mark_color,
        );
        ui.painter().text(
            egui::pos2(rect.left() + mark_width + 8.0, y),
            Align2::LEFT_CENTER,
            self.label,
            font,
            label_color,
        );
        response
    }
}

/// An on/off switch 30 × 16 with a square knob; the response is changed
/// when it flips.
pub struct Switch<'a>(pub &'a mut bool);

impl Widget for Switch<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let t = Tokens::get(ui.ctx());
        let (rect, mut response) = ui.allocate_exact_size(vec2(30.0, 16.0), Sense::click());
        if response.clicked() {
            *self.0 = !*self.0;
            response.mark_changed();
        }
        let on = *self.0;
        let enabled = ui.is_enabled();
        response.widget_info(|| {
            egui::WidgetInfo::selected(egui::WidgetType::Checkbox, enabled, on, "")
        });
        let how_on = ui.ctx().animate_bool_responsive(response.id, on);
        let off_fill = if response.hovered() && enabled {
            t.rule_strong
        } else {
            t.hover
        };
        let mut fill = off_fill.lerp_to_gamma(t.accent, how_on);
        let mut knob = t.muted.lerp_to_gamma(t.on_accent, how_on);
        if !enabled {
            fill = fill.gamma_multiply(0.5);
            knob = knob.gamma_multiply(0.6);
        }
        let stroke = if on { egui::Stroke::NONE } else { t.edge() };
        ui.painter()
            .rect(rect, t.ctl_radius(), fill, stroke, StrokeKind::Inside);
        let x = egui::lerp((rect.left() + 8.0)..=(rect.right() - 8.0), how_on);
        ui.painter().rect_filled(
            Rect::from_center_size(egui::pos2(x, rect.center().y), Vec2::splat(12.0)),
            1,
            knob,
        );
        if response.has_focus() {
            ui.painter().rect_stroke(
                rect.expand(2.0),
                t.ctl_radius(),
                egui::Stroke::new(1.0, t.accent),
                StrokeKind::Outside,
            );
        }
        response
    }
}
