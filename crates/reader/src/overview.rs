//! The page overview: a zoomable grid of a document's pages in place of
//! its view, after pdfcraft's page grid (`ui-egui/src/canvas.rs`,
//! `organize_grid`). Cells show the sidebar's thumbnails, and sharp renders
//! at the size drawn when they are larger than a thumbnail.

use std::ops::{Range, RangeInclusive};

use crate::{
    document_tab::{DocumentTab, ViewAction},
    theme::{self, Tokens},
};

/// The grid's zoom range and the step of its keys, as in pdfcraft.
pub const GRID_ZOOM_RANGE: RangeInclusive<f32> = 0.5..=3.0;
const GRID_ZOOM_STEP: f32 = 1.25;
/// How far a sharp render may be from the scale drawn before it is redone:
/// a pinch changes the size every frame, and a slightly soft or slightly
/// large image is fine meanwhile.
const GRID_SHARP: RangeInclusive<f32> = 0.9..=1.6;
/// A cell's width at zoom 1, in points.
const CELL_WIDTH: f32 = 160.0;
const GAP: f32 = 24.0;
/// Below each page: its number.
const LABEL: f32 = 22.0;
/// The longest side a sharp render may have, in pixels.
const MAX_RENDER: f32 = 2048.0;

/// An open overview of a tab.
#[derive(Clone, Debug, PartialEq)]
pub struct Overview {
    /// The page the keys move and Enter opens.
    pub cursor: usize,
    /// Columns and whole rows shown at the last frame, for the keys.
    columns: usize,
    rows_shown: usize,
    /// Scroll the cursor into view on the next frame.
    reveal: bool,
    /// The grid's scroll offset at the last frame; `None` before the first,
    /// which centres the cursor whatever egui remembers of the scroll.
    offset: Option<f32>,
    /// Cells' height over width: the pages' typical shape.
    aspect: f32,
}

impl Overview {
    pub fn new(cursor: usize) -> Self {
        Self {
            cursor,
            columns: 1,
            rows_shown: 1,
            reveal: true,
            offset: None,
            aspect: 0.0,
        }
    }

    /// Handles a view action while the grid shows: the keys and search
    /// matches move the cursor and the zoom keys size the cells. Returns
    /// the action back when it is for the document instead (a link, Back),
    /// which closes the grid.
    pub fn apply(
        &mut self,
        action: ViewAction,
        count: usize,
        zoom: &mut f32,
    ) -> Option<ViewAction> {
        let (columns, page_rows) = (self.columns.max(1), self.rows_shown.max(1));
        let delta = match action {
            ViewAction::Step(steps) => steps as isize * columns as isize,
            ViewAction::Sideways(steps) => steps as isize,
            ViewAction::NextPage => (page_rows * columns) as isize,
            ViewAction::PreviousPage => -((page_rows * columns) as isize),
            ViewAction::HalfPage(direction) => {
                direction as isize * ((page_rows / 2).max(1) * columns) as isize
            }
            // A search match comes into view as the cursor's page, and the
            // grid stays.
            ViewAction::GoToPage(page) | ViewAction::Reveal { page, .. } => {
                self.cursor = page.min(count.saturating_sub(1));
                self.reveal = true;
                return None;
            }
            ViewAction::ZoomIn => {
                *zoom = step_zoom(*zoom, true);
                self.reveal = true;
                return None;
            }
            ViewAction::ZoomOut => {
                *zoom = step_zoom(*zoom, false);
                self.reveal = true;
                return None;
            }
            ViewAction::FitWidth => {
                *zoom = 1.0;
                self.reveal = true;
                return None;
            }
            // Presenting and the document's own moves leave the grid.
            ViewAction::TogglePresent
            | ViewAction::GoTo { .. }
            | ViewAction::GoToEntry(_)
            | ViewAction::Back
            | ViewAction::Forward => return Some(action),
        };
        self.cursor = move_cursor(self.cursor, delta, count);
        self.reveal = true;
        None
    }
}

/// The zoom one key press away, kept in range.
pub fn step_zoom(zoom: f32, larger: bool) -> f32 {
    let zoom = if larger {
        zoom * GRID_ZOOM_STEP
    } else {
        zoom / GRID_ZOOM_STEP
    };
    zoom.clamp(*GRID_ZOOM_RANGE.start(), *GRID_ZOOM_RANGE.end())
}

/// The median height over width of pages sized `sizes`, so a cover or a
/// fold-out of another shape doesn't size every cell.
pub fn typical_aspect(sizes: impl Iterator<Item = [f64; 2]>) -> f32 {
    let mut aspects: Vec<f32> = sizes
        .filter(|[width, _]| *width > 0.0)
        .map(|[width, height]| (height / width) as f32)
        .collect();
    if aspects.is_empty() {
        return 1.0;
    }
    aspects.sort_by(f32::total_cmp);
    aspects[aspects.len() / 2]
}

/// How many cells of `cell` points fit across `width`, at least one.
pub fn columns(width: f32, cell: f32) -> usize {
    (((width - GAP) / (cell + GAP)).floor() as usize).max(1)
}

/// The cursor moved by `delta` cells, kept on a page.
pub fn move_cursor(cursor: usize, delta: isize, count: usize) -> usize {
    cursor
        .saturating_add_signed(delta)
        .min(count.saturating_sub(1))
}

/// Whether a render at `have` pixels per point is good enough to draw at
/// `want`.
pub fn sharp_enough(have: f32, want: f32) -> bool {
    want > 0.0 && GRID_SHARP.contains(&(have / want))
}

/// Rows `first..last` partly in a `height`-point window scrolled to `top`,
/// with `row` points per row.
pub fn rows_in_view(top: f32, height: f32, row: f32, rows: usize) -> Range<usize> {
    if row <= 0.0 {
        return 0..0;
    }
    let first = ((top / row).floor().max(0.0) as usize).min(rows);
    let last = (((top + height) / row).ceil().max(0.0) as usize).min(rows);
    first..last
}

/// The sharpest texture of `page` drawn as `paper` at `ppp` pixels per
/// point: its sharp render, else its thumbnail. When neither is sharp
/// enough, a render at the paper's size is added to `wanted`.
pub fn page_texture(
    tab: &DocumentTab,
    page: usize,
    paper: egui::Rect,
    ppp: f32,
    wanted: &mut Vec<(usize, f32)>,
) -> Option<egui::TextureId> {
    let [w, h] = tab.layout.page_size(page);
    // Pixels per page point the paper is drawn at.
    let want = (paper.width() / w as f32 * ppp).min(MAX_RENDER / w.max(h) as f32);
    let sharp = tab
        .grid_image(page)
        .filter(|(have, _)| sharp_enough(*have, want))
        .map(|(_, texture)| texture.id());
    let thumbnail_sharp = tab
        .thumbnail(page)
        .is_some_and(|(_, size)| size.x >= paper.width() * ppp * 0.9);
    if sharp.is_none() && !thumbnail_sharp && !tab.dense_pages.contains(&page) {
        wanted.push((page, want));
    }
    sharp
        .or_else(|| tab.grid_image(page).map(|(_, texture)| texture.id()))
        .or_else(|| tab.thumbnail(page).map(|(id, _)| id))
}

/// Draws tab's overview in `rect`; returns the page clicked to open.
pub fn show(ui: &mut egui::Ui, rect: egui::Rect, tab: &mut DocumentTab) -> Option<usize> {
    let mut state = tab.overview.take()?;
    let tokens = Tokens::get(ui.ctx());
    let ppp = ui.ctx().pixels_per_point();
    let count = tab.layout.len();
    let hovered = ui.rect_contains_pointer(rect);
    // Ctrl+wheel and pinches size the cells.
    let pinch = ui.input(|i| i.zoom_delta());
    if hovered && pinch != 1.0 {
        tab.overview_zoom =
            (tab.overview_zoom * pinch).clamp(*GRID_ZOOM_RANGE.start(), *GRID_ZOOM_RANGE.end());
    }
    let cell = CELL_WIDTH * tab.overview_zoom;
    let columns = columns(rect.width(), cell);
    if state.aspect <= 0.0 {
        state.aspect = typical_aspect((0..count).map(|page| tab.layout.page_size(page)));
    }
    let box_height = cell * state.aspect;
    let row_height = box_height + LABEL + GAP;
    let rows = count.div_ceil(columns);
    let left = rect.left() + (rect.width() - columns as f32 * (cell + GAP) + GAP) / 2.0;
    let current = tab.current_page();
    // Notes and highlights per page, for the labels and marks.
    let reflowed = tab.is_reflowed();
    let mut notes: std::collections::HashMap<usize, usize> = std::collections::HashMap::new();
    for item in &tab.annotations.items {
        if let Some((page, _)) = tab.annotations.page_of(item, reflowed) {
            *notes.entry(page).or_default() += 1;
        }
    }
    let width = count.to_string().len().max(2);
    let mut open = None;
    let mut wanted = Vec::new();
    let mut shown = 0..0;

    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
    let mut scroll = egui::ScrollArea::vertical()
        .id_salt(("overview", &tab.path))
        .auto_shrink(false);
    if state.reveal {
        let top = (state.cursor / columns) as f32 * row_height;
        // Only when the cursor's row is out of view.
        let hidden = state
            .offset
            .is_none_or(|view| top < view || top + row_height > view + rect.height());
        if hidden {
            let centred = top - (rect.height() - row_height) / 2.0;
            scroll = scroll.vertical_scroll_offset(centred.max(0.0));
        }
        state.reveal = false;
    }
    let output = scroll.show_viewport(&mut child, |ui, viewport| {
        ui.set_height(rows as f32 * row_height + GAP);
        let origin = ui.min_rect().min;
        shown = rows_in_view(viewport.top(), viewport.height(), row_height, rows);
        for row in shown.clone() {
            for column in 0..columns {
                let page = row * columns + column;
                if page >= count {
                    break;
                }
                let cell_rect = egui::Rect::from_min_size(
                    egui::pos2(
                        left + column as f32 * (cell + GAP),
                        origin.y + GAP + row as f32 * row_height,
                    ),
                    egui::vec2(cell, box_height),
                );
                let response = ui.interact(
                    cell_rect,
                    ui.id().with(("overview-page", page)),
                    egui::Sense::click(),
                );
                let [w, h] = tab.layout.page_size(page);
                let scale = (cell / w as f32).min(box_height / h as f32);
                let paper = egui::Rect::from_center_size(
                    cell_rect.center(),
                    egui::vec2(w as f32 * scale, h as f32 * scale),
                );
                let painter = ui.painter();
                painter.rect_filled(paper, 0, tokens.page);
                if let Some(texture) = page_texture(tab, page, paper, ppp, &mut wanted) {
                    glyphst_ui::paint::paint_turned_image(
                        painter,
                        texture,
                        paper,
                        tab.layout.arrangement.rotation,
                    );
                } else if tab.dense_pages.contains(&page) {
                    painter.text(
                        paper.center(),
                        egui::Align2::CENTER_CENTER,
                        "dense page",
                        theme::mono(10.0),
                        tokens.faint,
                    );
                } else {
                    glyphst_ui::thumb::paint_placeholder_lines(ui, paper);
                }
                // Search matches, tinted as on the page.
                let found = tab.find.on_page(page);
                let matches = found.len();
                if let Some(text) = tab.text.get(page) {
                    let offset = tab.layout.offsets[page];
                    let to_paper = |point: [f32; 2]| {
                        let at = tab.layout.to_document(page, point);
                        paper.min
                            + egui::vec2(
                                (at[0] - offset[0]) as f32 * scale,
                                (at[1] - offset[1]) as f32 * scale,
                            )
                    };
                    for index in found {
                        let (tint, edge) =
                            crate::find::match_tint(&tokens, tab.find.current == Some(index));
                        for rect in text.selection_rects(tab.find.matches[index].range.clone()) {
                            let rect = egui::Rect::from_two_pos(
                                to_paper([rect[0], rect[1]]),
                                to_paper([rect[2], rect[3]]),
                            )
                            .intersect(paper);
                            painter.rect_filled(rect, 0, tint);
                            if let Some(edge) = edge {
                                painter.rect_stroke(
                                    rect.expand(1.0),
                                    0,
                                    edge,
                                    egui::StrokeKind::Outside,
                                );
                            }
                        }
                    }
                }
                // A small accent mark on pages with notes.
                let noted = notes.get(&page).copied().unwrap_or(0);
                if noted > 0 {
                    painter.rect_filled(
                        egui::Rect::from_min_size(
                            egui::pos2(paper.right() - 14.0, paper.top() + 6.0),
                            egui::vec2(8.0, 8.0),
                        ),
                        0,
                        tokens.accent,
                    );
                }
                if page == state.cursor {
                    glyphst_ui::motif::corner_ticks(ui, paper, 8.0, 10.0, tokens.accent);
                } else if response.hovered() {
                    painter.rect_stroke(paper, 0, tokens.edge(), egui::StrokeKind::Outside);
                }
                let number = format!("{:0width$}", page + 1);
                let found = match matches {
                    0 => String::new(),
                    1 => "1 match".to_owned(),
                    _ => format!("{matches} matches"),
                };
                let (label, color) = if page == current && matches > 0 {
                    (format!("{number} ● current · {found}"), tokens.accent_text)
                } else if page == current {
                    (format!("{number} ● current"), tokens.accent_text)
                } else if matches > 0 {
                    (format!("{number} {found}"), tokens.accent_text)
                } else if noted > 0 {
                    let unit = if noted == 1 { "note" } else { "notes" };
                    (format!("{number} {noted} {unit}"), tokens.faint)
                } else if page < current {
                    (format!("{number} read"), tokens.faint)
                } else {
                    (number, tokens.faint)
                };
                painter.text(
                    egui::pos2(cell_rect.center().x, cell_rect.bottom() + LABEL / 2.0 + 2.0),
                    egui::Align2::CENTER_CENTER,
                    label,
                    theme::mono(10.0),
                    color,
                );
                if response.clicked() {
                    open = Some(page);
                }
                if response.hovered() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                }
            }
        }
    });
    state.offset = Some(output.state.offset.y);
    state.columns = columns;
    state.rows_shown = ((rect.height() / row_height).floor() as usize).max(1);
    let pages = shown.start * columns..(shown.end * columns).min(count);
    let ahead = (pages.start.saturating_sub(columns))..(pages.end + columns).min(count);
    tab.want_thumbnails(ahead);
    tab.want_grid_images(wanted);
    tab.overview = Some(state);
    open
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn columns_fill_the_width_and_never_drop_to_none() {
        assert_eq!(columns(1000.0, 160.0), 5);
        assert_eq!(columns(100.0, 160.0), 1);
        assert_eq!(columns(1000.0, 480.0), 1, "two need their gaps too");
        assert_eq!(columns(1100.0, 480.0), 2);
    }

    #[test]
    fn keys_move_the_cursor_by_cells_rows_and_screens() {
        let mut zoom = 1.0;
        let mut grid = Overview::new(5);
        (grid.columns, grid.rows_shown) = (4, 3);
        assert_eq!(grid.apply(ViewAction::Step(1), 30, &mut zoom), None);
        assert_eq!(grid.cursor, 9, "down a row");
        grid.apply(ViewAction::Sideways(-1), 30, &mut zoom);
        assert_eq!(grid.cursor, 8);
        grid.apply(ViewAction::NextPage, 30, &mut zoom);
        assert_eq!(grid.cursor, 20, "a screen is three rows");
        grid.apply(ViewAction::HalfPage(-1), 30, &mut zoom);
        assert_eq!(grid.cursor, 16, "half a screen is a row here");
        grid.apply(ViewAction::NextPage, 30, &mut zoom);
        grid.apply(ViewAction::NextPage, 30, &mut zoom);
        assert_eq!(grid.cursor, 29, "stops at the last page");
        grid.apply(ViewAction::GoToPage(0), 30, &mut zoom);
        assert_eq!(grid.cursor, 0);
        grid.apply(ViewAction::Step(-1), 30, &mut zoom);
        assert_eq!(grid.cursor, 0, "stops at the first page");
        assert_eq!(
            grid.apply(ViewAction::Back, 30, &mut zoom),
            Some(ViewAction::Back)
        );
    }

    #[test]
    fn a_search_match_moves_the_cursor_and_keeps_the_grid() {
        let mut zoom = 1.0;
        let mut grid = Overview::new(0);
        let reveal = ViewAction::Reveal {
            page: 12,
            rect: [0.0, 0.0, 10.0, 10.0],
        };
        assert_eq!(grid.apply(reveal, 30, &mut zoom), None);
        assert_eq!(grid.cursor, 12);
        assert!(grid.reveal);
    }

    #[test]
    fn zoom_keys_step_within_the_range() {
        let mut zoom = 1.0;
        let mut grid = Overview::new(0);
        grid.apply(ViewAction::ZoomIn, 3, &mut zoom);
        assert_eq!(zoom, 1.25);
        for _ in 0..20 {
            grid.apply(ViewAction::ZoomIn, 3, &mut zoom);
        }
        assert_eq!(zoom, 3.0);
        grid.apply(ViewAction::FitWidth, 3, &mut zoom);
        assert_eq!(zoom, 1.0);
        for _ in 0..20 {
            grid.apply(ViewAction::ZoomOut, 3, &mut zoom);
        }
        assert_eq!(zoom, 0.5);
    }

    #[test]
    fn cells_take_the_shape_most_pages_have() {
        let cover = [600.0, 800.0];
        let spread = [1200.0, 800.0];
        let sizes = [cover, spread, spread, spread, cover];
        assert!((typical_aspect(sizes.into_iter()) - 2.0 / 3.0).abs() < 1e-6);
        assert_eq!(typical_aspect(std::iter::empty()), 1.0);
    }

    #[test]
    fn rows_in_view_cover_partly_shown_rows() {
        assert_eq!(rows_in_view(0.0, 500.0, 200.0, 10), 0..3);
        assert_eq!(rows_in_view(250.0, 500.0, 200.0, 10), 1..4);
        assert_eq!(rows_in_view(1900.0, 500.0, 200.0, 10), 9..10);
    }

    #[test]
    fn sharp_renders_tolerate_a_pinch() {
        assert!(sharp_enough(2.0, 2.0));
        assert!(sharp_enough(2.0, 1.5), "a little large is fine");
        assert!(!sharp_enough(1.0, 2.0), "too soft");
        assert!(!sharp_enough(4.0, 1.0), "wastefully large");
        assert!(!sharp_enough(1.0, 0.0));
    }
}
