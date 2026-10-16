//! The centred column of views without a document (design/README.md,
//! "Layout"): home, the new tab, the reading activity and the library keep
//! their content in one column, `clamp(available − 2 × gutter, 560,
//! 1040)` wide (the new tab's at most 720), and every section spans
//! exactly that width.

use egui::{Rect, Ui};

pub const MIN_COLUMN: f32 = 560.0;
pub const MAX_COLUMN: f32 = 1040.0;
/// The room on each side of the column, and below [`NARROW`] the smaller.
pub const GUTTER: f32 = 48.0;
pub const NARROW_GUTTER: f32 = 16.0;
pub const NARROW: f32 = 720.0;

/// The column's width in `available`: the gutters taken off, at most
/// `max`, and at least [`MIN_COLUMN`] while there is room for it.
pub fn column_width(available: f32, max: f32) -> f32 {
    let gutter = if available < NARROW {
        NARROW_GUTTER
    } else {
        GUTTER
    };
    (available - 2.0 * gutter)
        .min(max)
        .max(MIN_COLUMN.min(available))
        .max(0.0)
}

/// Lays `add` out in the column (at most `max` wide) centred in `ui`'s
/// width, with the column's width; returns what `add` returned and the
/// rect it used.
pub fn column<R>(ui: &mut Ui, max: f32, add: impl FnOnce(&mut Ui, f32) -> R) -> (R, Rect) {
    let available = ui.available_width();
    let width = column_width(available, max);
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        ui.add_space(((available - width) / 2.0).max(0.0));
        let inner = ui.vertical(|ui| {
            ui.set_min_width(width);
            ui.set_max_width(width);
            add(ui, width)
        });
        (inner.inner, inner.response.rect)
    })
    .inner
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_column_keeps_its_range_and_gutters() {
        let width = |available| column_width(available, MAX_COLUMN);
        assert_eq!(width(1440.0), MAX_COLUMN);
        assert_eq!(width(1060.0), 1060.0 - 96.0);
        assert_eq!(width(700.0), 700.0 - 32.0);
        // Too narrow for the gutters: the minimum, while it fits.
        assert_eq!(width(580.0), MIN_COLUMN);
        assert_eq!(width(400.0), 400.0);
        assert_eq!(column_width(1440.0, 720.0), 720.0);
    }
}
