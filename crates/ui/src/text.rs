//! Text helpers the components share: measuring, and one line cut with "…".

use egui::{Align2, Color32, FontId, Pos2, Rect};

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
