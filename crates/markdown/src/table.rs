//! Tables whose cells wrap, after Zeron's port of mugen-markdown's
//! TableBlock: each column wants its widest cell unwrapped, keeps at least
//! min(that, 96pt), and shares out the width above those floors in
//! proportion to what it wants. Only when the floors don't fit does the
//! table scroll sideways.

use egui::{Align, Pos2, Rect, ScrollArea, Sense, Ui, UiBuilder, Vec2, vec2};
use pulldown_cmark::Alignment;

use crate::{
    MarkdownStyle,
    inline::{self, Base},
    parse::Inline,
};

const FLOOR: f32 = 96.0;
const PAD: Vec2 = vec2(10.0, 6.0);

pub(crate) fn show(
    ui: &mut Ui,
    style: &MarkdownStyle,
    base: &Base,
    align: &[Alignment],
    head: &[Vec<Inline>],
    rows: &[Vec<Vec<Inline>>],
) {
    let columns = rows
        .iter()
        .map(Vec::len)
        .chain([head.len()])
        .max()
        .unwrap_or(0);
    if columns == 0 {
        return;
    }
    let head_base = Base {
        all_strong: true,
        color: style.strong_text,
        ..base.clone()
    };
    let mut wants = vec![0.0_f32; columns];
    let mut mins = vec![0.0_f32; columns];
    for (row, base) in
        std::iter::once((head, &head_base)).chain(rows.iter().map(|row| (row.as_slice(), base)))
    {
        for (column, cell) in row.iter().enumerate() {
            let width = inline::natural_width(ui, style, base, cell) + 2.0 * PAD.x;
            wants[column] = wants[column].max(width.ceil());
            let min = inline::min_width(ui, style, base, cell) + 2.0 * PAD.x;
            mins[column] = mins[column].max(min.ceil());
        }
    }
    let widths = column_widths(&wants, &mins, ui.available_width());
    let width: f32 = widths.iter().sum();
    if width > ui.available_width() + 0.5 {
        ScrollArea::horizontal()
            .id_salt(ui.next_auto_id())
            .show(ui, |ui| {
                grid(ui, style, base, &head_base, align, head, rows, &widths)
            });
    } else {
        grid(ui, style, base, &head_base, align, head, rows, &widths);
    }
}

/// Column widths for `wants` in `available` points, none narrower than its
/// widest unbreakable piece in `mins`.
fn column_widths(wants: &[f32], mins: &[f32], available: f32) -> Vec<f32> {
    let total: f32 = wants.iter().sum();
    if total <= available {
        return wants.to_vec();
    }
    let floors: Vec<f32> = wants
        .iter()
        .zip(mins)
        .map(|(want, min)| want.min(FLOOR.max(*min)))
        .collect();
    let floor: f32 = floors.iter().sum();
    if floor >= available {
        return floors;
    }
    let flex = total - floor;
    wants
        .iter()
        .zip(&floors)
        .map(|(want, floor_w)| floor_w + (want - floor_w) * (available - floor) / flex)
        .map(f32::floor)
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn grid(
    ui: &mut Ui,
    style: &MarkdownStyle,
    base: &Base,
    head_base: &Base,
    align: &[Alignment],
    head: &[Vec<Inline>],
    rows: &[Vec<Vec<Inline>>],
    widths: &[f32],
) {
    let width: f32 = widths.iter().sum();
    let mut lines = Vec::new();
    for (index, (row, base)) in std::iter::once((head, head_base))
        .chain(rows.iter().map(|row| (row.as_slice(), base)))
        .enumerate()
    {
        let cells: Vec<_> = widths
            .iter()
            .enumerate()
            .map(|(column, width)| {
                let empty = Vec::new();
                let cell = row.get(column).unwrap_or(&empty);
                inline::layout(ui, style, base, cell, width - 2.0 * PAD.x)
            })
            .collect();
        let height = cells
            .iter()
            .map(|laid| inline::size(laid).y)
            .fold(0.0, f32::max)
            + 2.0 * PAD.y;
        let (rect, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
        let mut x = rect.left();
        for ((laid, column_width), column) in cells.into_iter().zip(widths).zip(0..) {
            let size = inline::size(&laid);
            let inner = column_width - 2.0 * PAD.x;
            let offset = match align.get(column) {
                Some(Alignment::Right) => inner - size.x,
                Some(Alignment::Center) => (inner - size.x) / 2.0,
                _ => 0.0,
            };
            let cell = Rect::from_min_size(
                Pos2::new(x + PAD.x + offset.max(0.0), rect.top() + PAD.y),
                size,
            );
            // A child that doesn't allocate in `ui`: the row already has.
            let mut cell_ui = ui.new_child(
                UiBuilder::new()
                    .max_rect(cell)
                    .layout(egui::Layout::left_to_right(Align::TOP)),
            );
            inline::show(&mut cell_ui, laid);
            x += column_width;
        }
        lines.push((rect.x_range(), rect.bottom(), index == 0));
    }
    // Hairlines under the rows; a stronger one under the header.
    for (x, y, header) in lines.iter().take(lines.len().saturating_sub(1)) {
        let color = if *header {
            style.rule_strong
        } else {
            style.rule
        };
        ui.painter().hline(*x, *y, egui::Stroke::new(1.0, color));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn narrow_tables_keep_their_natural_widths() {
        assert_eq!(
            column_widths(&[50.0, 80.0], &[30.0, 40.0], 300.0),
            vec![50.0, 80.0]
        );
    }

    #[test]
    fn wide_columns_shrink_toward_their_floor() {
        let widths = column_widths(&[40.0, 400.0, 200.0], &[30.0, 60.0, 60.0], 340.0);
        assert_eq!(widths[0], 40.0);
        assert!(widths[1] > widths[2] && widths[2] >= FLOOR);
        assert!(widths.iter().sum::<f32>() <= 340.0);
    }

    #[test]
    fn floors_that_overflow_scroll() {
        assert_eq!(
            column_widths(&[300.0, 300.0, 300.0], &[50.0; 3], 200.0),
            vec![FLOOR; 3]
        );
    }

    #[test]
    fn a_long_word_widens_its_floor() {
        let widths = column_widths(&[300.0, 300.0], &[140.0, 50.0], 260.0);
        assert!(widths[0] >= 140.0, "{widths:?}");
    }
}
