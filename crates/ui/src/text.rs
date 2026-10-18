//! Text helpers the components share: measuring, and one line cut with "…".

use egui::{Align2, Color32, FontId, Margin, Pos2, Rect};

/// The padding inside a framed text field, as egui 0.36 had it; 0.37's
/// theme pads fields like buttons.
pub const FIELD_MARGIN: Margin = Margin::symmetric(4, 2);

/// Runs `add` with text fields as tall as their text and margins, as
/// egui 0.36 laid them out. egui 0.37's theme makes a field at least as
/// tall as a button (`interact_size.y`) with its text at the top, so a
/// field in a row sat higher than the icons and labels beside it.
pub fn fitted<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let height = ui.spacing().interact_size.y;
    ui.spacing_mut().interact_size.y = 0.0;
    let inner = add(ui);
    ui.spacing_mut().interact_size.y = height;
    inner
}

/// The width of `text` in `font` on one line.
pub fn width(ui: &egui::Ui, text: &str, font: &FontId) -> f32 {
    ui.painter()
        .layout_no_wrap(text.to_owned(), font.clone(), Color32::WHITE)
        .size()
        .x
}

/// One line of `text` anchored at `pos` by `align`, cut with "…" to at most
/// `max_width`. Returns the painted rect.
pub fn line(
    ui: &egui::Ui,
    pos: Pos2,
    align: Align2,
    text: impl Into<String>,
    font: FontId,
    color: Color32,
    max_width: f32,
) -> Rect {
    let mut job = egui::text::LayoutJob::simple_singleline(text.into(), font, color);
    job.wrap = egui::text::TextWrapping::truncate_at_width(max_width.max(0.0));
    let galley = ui.painter().layout_job(job);
    let rect = align.anchor_size(pos, galley.size());
    ui.painter().galley(rect.min, galley, color);
    rect
}

/// One line of `text`, left-aligned and vertically centred on `pos`, cut
/// with "…" to at most `max_width`.
pub fn row(
    ui: &egui::Ui,
    pos: Pos2,
    text: impl Into<String>,
    font: FontId,
    color: Color32,
    max_width: f32,
) {
    line(ui, pos, Align2::LEFT_CENTER, text, font, color, max_width);
}
