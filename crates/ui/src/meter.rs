//! Meters (pen component `meter`): progress as `━━━━━──── 58%`, in glyphs
//! when ornaments are on and as a 2px bar otherwise, and a bar variant for
//! tables and the status bar.

use egui::{Align2, Color32, Rect, Response, Sense, Ui, Widget, vec2};

use crate::{
    motif, text,
    theme::{self, Tokens},
};

/// A meter `width` points wide (bar and label) at `fraction`, with an
/// optional label after it (`58%`, `done`).
#[must_use = "call `ui.add(...)`"]
pub struct Meter<'a> {
    fraction: f32,
    width: f32,
    label: Option<&'a str>,
    /// Label in accent, as unread progress in the library.
    accent_label: bool,
    /// The fill colour; accent by default.
    color: Option<Color32>,
    bar: bool,
}

impl<'a> Meter<'a> {
    pub fn new(fraction: f32) -> Self {
        Self {
            fraction: if fraction.is_finite() {
                fraction.clamp(0.0, 1.0)
            } else {
                0.0
            },
            width: 108.0,
            label: None,
            accent_label: false,
            color: None,
            bar: false,
        }
    }

    pub fn width(mut self, width: f32) -> Self {
        self.width = width;
        self
    }

    pub fn label(mut self, label: &'a str) -> Self {
        self.label = Some(label);
        self
    }

    pub fn accent_label(mut self, accent: bool) -> Self {
        self.accent_label = accent;
        self
    }

    pub fn color(mut self, color: Color32) -> Self {
        self.color = Some(color);
        self
    }

    /// Always a 2px bar, never glyphs.
    pub fn bar(mut self) -> Self {
        self.bar = true;
        self
    }

    /// Paints the meter in `rect` (the label right-aligned in it).
    pub fn paint(&self, ui: &Ui, rect: Rect) {
        let t = Tokens::get(ui.ctx());
        let font = theme::mono(11.0);
        let label_width = self
            .label
            .map_or(0.0, |label| text::width(ui, label, &font) + 8.0);
        let track = Rect::from_min_max(
            rect.min,
            egui::pos2(rect.right() - label_width, rect.bottom()),
        );
        let fill = self.color.unwrap_or(t.accent);
        if t.ornaments && !self.bar {
            let advance = text::width(ui, "━", &font).max(1.0);
            let cells = (track.width() / advance).floor().max(1.0) as usize;
            let (done, rest) = motif::meter_text(self.fraction, cells);
            let galley = ui.painter().layout_no_wrap(done, font.clone(), fill);
            let done_width = galley.size().x;
            let y = track.center().y - galley.size().y / 2.0;
            ui.painter()
                .galley(egui::pos2(track.left(), y), galley, fill);
            ui.painter().text(
                egui::pos2(track.left() + done_width, track.center().y),
                Align2::LEFT_CENTER,
                rest,
                font.clone(),
                t.rule_strong,
            );
        } else {
            let y = track.center().y;
            let x = track.left() + track.width() * self.fraction;
            ui.painter().rect_filled(
                Rect::from_min_max(egui::pos2(track.left(), y - 1.0), egui::pos2(x, y + 1.0)),
                0,
                fill,
            );
            ui.painter().rect_filled(
                Rect::from_min_max(egui::pos2(x, y - 1.0), egui::pos2(track.right(), y + 1.0)),
                0,
                t.rule_strong,
            );
        }
        if let Some(label) = self.label {
            ui.painter().text(
                egui::pos2(rect.right(), rect.center().y),
                Align2::RIGHT_CENTER,
                label,
                font,
                if self.accent_label {
                    t.accent_text
                } else {
                    t.muted
                },
            );
        }
    }
}

impl Widget for Meter<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let (rect, response) = ui.allocate_exact_size(vec2(self.width, 15.0), Sense::hover());
        self.paint(ui, rect);
        response
    }
}
