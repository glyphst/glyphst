//! Reading heatmaps (frames 04 and 04c): one cell a day in columns of
//! weeks, Monday at the top, coloured by level (`Tokens::heat`), with
//! month names above, `mon` / `wed` / `fri` at the left, today outlined in
//! `text` and the selected day in `accent_text`. The caller works out the
//! days; the map lays them out, and says which one is hovered or clicked.
//! [`legend`] draws `less ▪▪▪▪▪ more`.

use egui::{Align2, Rect, Response, Sense, Stroke, StrokeKind, Ui, pos2, vec2};

use crate::theme::{self, Tokens};

/// Width of the weekday labels, and the gap after them.
const LABELS: f32 = 22.0;
const LABELS_GAP: f32 = 8.0;
/// Height of the month names, and the gap under them.
const MONTHS: f32 = 12.0;
const MONTHS_GAP: f32 = 6.0;

/// A heatmap of `levels`, seven a week (Monday first): `None` for days
/// outside the range (after today).
#[must_use = "call `show`"]
pub struct Heatmap<'a> {
    levels: &'a [Option<u8>],
    /// Month names and the week each starts in.
    months: &'a [(usize, &'a str)],
    today: Option<usize>,
    selected: Option<usize>,
    cell: f32,
    gap: f32,
}

/// What the pointer did over a heatmap.
pub struct HeatmapResponse {
    pub response: Response,
    /// The day under the pointer.
    pub hovered: Option<usize>,
    pub clicked: Option<usize>,
}

impl<'a> Heatmap<'a> {
    pub fn new(levels: &'a [Option<u8>]) -> Self {
        Self {
            levels,
            months: &[],
            today: None,
            selected: None,
            cell: 12.0,
            gap: 3.0,
        }
    }

    pub fn months(mut self, months: &'a [(usize, &'a str)]) -> Self {
        self.months = months;
        self
    }

    pub fn today(mut self, day: usize) -> Self {
        self.today = Some(day);
        self
    }

    pub fn selected(mut self, day: Option<usize>) -> Self {
        self.selected = day;
        self
    }

    /// The side of a day's cell (12 on home, 14 in the activity tab).
    pub fn cell(mut self, cell: f32) -> Self {
        self.cell = cell;
        self
    }

    /// How many weeks of `cell` fit in `width`.
    pub fn weeks_in(width: f32, cell: f32) -> usize {
        let gap = 3.0;
        (((width - LABELS - LABELS_GAP + gap) / (cell + gap)).floor()).max(1.0) as usize
    }

    fn weeks(&self) -> usize {
        self.levels.len().div_ceil(7)
    }

    pub fn size(&self) -> egui::Vec2 {
        let step = self.cell + self.gap;
        vec2(
            LABELS + LABELS_GAP + self.weeks() as f32 * step - self.gap,
            MONTHS + MONTHS_GAP + 7.0 * step - self.gap,
        )
    }

    pub fn show(self, ui: &mut Ui) -> HeatmapResponse {
        let tokens = Tokens::get(ui.ctx());
        let (rect, response) = ui.allocate_exact_size(self.size(), Sense::click());
        let step = self.cell + self.gap;
        let grid = pos2(
            rect.left() + LABELS + LABELS_GAP,
            rect.top() + MONTHS + MONTHS_GAP,
        );
        let cell_rect = |day: usize| {
            Rect::from_min_size(
                grid + vec2((day / 7) as f32 * step, (day % 7) as f32 * step),
                vec2(self.cell, self.cell),
            )
        };
        let painter = ui.painter_at(rect.expand(2.0));
        let font = theme::mono(9.0);
        for &(week, name) in self.months {
            painter.text(
                pos2(grid.x + week as f32 * step, rect.top()),
                Align2::LEFT_TOP,
                name,
                font.clone(),
                tokens.faint,
            );
        }
        for (row, name) in [(0, "mon"), (2, "wed"), (4, "fri")] {
            painter.text(
                pos2(rect.left(), grid.y + row as f32 * step + self.cell / 2.0),
                Align2::LEFT_CENTER,
                name,
                font.clone(),
                tokens.faint,
            );
        }
        let hovered = response.hover_pos().and_then(|pointer| {
            let at = pointer - grid;
            let (week, row) = ((at.x / step).floor(), (at.y / step).floor());
            let day = (week >= 0.0 && (0.0..7.0).contains(&row))
                .then(|| week as usize * 7 + row as usize)?;
            (cell_rect(day).contains(pointer) && self.levels.get(day).copied().flatten().is_some())
                .then_some(day)
        });
        let radius = if self.cell >= 14.0 { 2.5 } else { 2.0 };
        for (day, level) in self.levels.iter().enumerate() {
            let Some(level) = level else { continue };
            let cell = cell_rect(day);
            painter.rect_filled(cell, radius, tokens.heat(*level));
            let stroke = if self.selected == Some(day) {
                Some(Stroke::new(2.0, tokens.accent_text))
            } else if self.today == Some(day) || hovered == Some(day) {
                Some(Stroke::new(1.0, tokens.text))
            } else {
                None
            };
            if let Some(stroke) = stroke {
                painter.rect_stroke(cell, radius, stroke, StrokeKind::Outside);
            }
        }
        if hovered.is_some() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        let clicked = response.clicked().then_some(hovered).flatten();
        HeatmapResponse {
            response,
            hovered,
            clicked,
        }
    }
}

/// `less ▪▪▪▪▪ more`, the levels from empty to full.
pub fn legend(ui: &mut Ui) -> Response {
    let tokens = Tokens::get(ui.ctx());
    let font = theme::mono(9.0);
    let galley = |text: &str| {
        ui.painter()
            .layout_no_wrap(text.to_owned(), font.clone(), tokens.faint)
    };
    let (less, more) = (galley("less"), galley("more"));
    let cells = 5.0 * 10.0 + 4.0 * 3.0;
    let width = less.size().x + 6.0 + cells + 6.0 + more.size().x;
    let (rect, response) = ui.allocate_exact_size(vec2(width, 12.0), Sense::hover());
    let painter = ui.painter();
    let mut x = rect.left();
    painter.galley(
        pos2(x, rect.center().y - less.size().y / 2.0),
        less,
        tokens.faint,
    );
    x = rect.left() + width - cells - 6.0 - more.size().x;
    for level in 0..5 {
        let cell = Rect::from_min_size(pos2(x, rect.center().y - 5.0), vec2(10.0, 10.0));
        painter.rect_filled(cell, 2.0, tokens.heat(level));
        x += 13.0;
    }
    painter.galley(
        pos2(
            rect.right() - more.size().x,
            rect.center().y - more.size().y / 2.0,
        ),
        more,
        tokens.faint,
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_year_of_weeks_fits_the_home_column() {
        // Home's column is 964 wide with 140 for the stats beside the map.
        assert_eq!(Heatmap::weeks_in(824.0, 12.0), 53);
        let levels = vec![Some(0); 53 * 7];
        let size = Heatmap::new(&levels).size();
        assert_eq!(
            size,
            vec2(22.0 + 8.0 + 53.0 * 15.0 - 3.0, 18.0 + 7.0 * 15.0 - 3.0)
        );
    }
}
