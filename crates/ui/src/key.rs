//! Key chips (pen component `key`): `ctrl k` in mono 10 on a hairline
//! outline, at the end of menu rows, fields and buttons.

use egui::{Align2, Color32, Rect, Response, Sense, StrokeKind, Ui, Vec2, Widget, vec2};

use crate::{
    text,
    theme::{self, Tokens},
};

fn font() -> egui::FontId {
    theme::mono(10.0)
}

/// The size of the chip for `label`.
pub fn size(ui: &Ui, label: &str) -> Vec2 {
    vec2(text::width(ui, label, &font()) + 10.0, 15.0)
}

/// Paints the chip for `label` at `rect`; `color` overrides the muted label
/// (on accent fills).
pub fn paint(ui: &Ui, rect: Rect, label: &str, color: Option<Color32>) {
    let t = Tokens::get(ui.ctx());
    let color = color.unwrap_or(t.muted);
    let stroke = if color == t.muted {
        t.edge()
    } else {
        egui::Stroke::new(1.0, Tokens::fade(color, 0.5))
    };
    ui.painter()
        .rect_stroke(rect, t.ctl_radius(), stroke, StrokeKind::Inside);
    ui.painter()
        .text(rect.center(), Align2::CENTER_CENTER, label, font(), color);
}

/// A key chip as a widget.
pub struct Key<'a>(pub &'a str);

impl Widget for Key<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let (rect, response) = ui.allocate_exact_size(size(ui, self.0), Sense::hover());
        paint(ui, rect, self.0, None);
        response
    }
}

/// Keys as the design writes them: lower case, `+` as a space
/// (`Ctrl+Shift+K` → `ctrl shift k`).
pub fn display(keys: &str) -> String {
    keys.split('+')
        .map(|part| part.trim().to_lowercase())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    #[test]
    fn keys_display_in_lower_case_with_spaces() {
        assert_eq!(super::display("Ctrl+Shift+K"), "ctrl shift k");
        assert_eq!(super::display("F"), "f");
        assert_eq!(super::display(""), "");
    }
}
