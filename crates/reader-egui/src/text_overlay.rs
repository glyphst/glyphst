//! Text on top of the GPU pages: mapping between page points and the
//! screen, the text selection (pdfcraft's `Selection`, artcraft
//! `ui-egui/src/canvas.rs`), and painting selections, search matches and
//! highlights as egui shapes over the page image.

use std::ops::Range;

use glyphst_pdf::{text::PageText, vector::PageRect};

use crate::gpu::PageDraw;

/// A selection on one page.
#[derive(Clone, Debug, PartialEq)]
pub struct Selection {
    pub page: usize,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Span {
    /// Glyphs in reading order from `anchor` to `head`, both included.
    Run { anchor: usize, head: usize },
    /// Glyphs whose centres lie in a page rectangle (Alt+drag).
    Column { from: [f32; 2], to: [f32; 2] },
}

impl Selection {
    pub fn run(page: usize, range: Range<usize>) -> Self {
        Self {
            page,
            span: Span::Run {
                anchor: range.start,
                head: range.end.saturating_sub(1).max(range.start),
            },
        }
    }

    /// The selected glyph range of a run, end excluded.
    pub fn range(&self) -> Option<Range<usize>> {
        match self.span {
            Span::Run { anchor, head } => Some(anchor.min(head)..anchor.max(head) + 1),
            Span::Column { .. } => None,
        }
    }

    pub fn glyphs(&self, text: &PageText) -> Vec<usize> {
        match &self.span {
            Span::Run { .. } => self
                .range()
                .map(|range| (range.start..range.end.min(text.len())).collect())
                .unwrap_or_default(),
            Span::Column { from, to } => text.glyphs_in([from[0], from[1], to[0], to[1]]),
        }
    }

    pub fn text(&self, text: &PageText) -> String {
        text.text_of_glyphs(&self.glyphs(text))
    }

    pub fn rects(&self, text: &PageText) -> Vec<PageRect> {
        text.rects_of_glyphs(&self.glyphs(text))
    }
}

/// Where a page is on screen: maps page points to egui points and back.
#[derive(Clone, Copy, Debug)]
pub struct PageMap {
    pub page: usize,
    /// Page point at the view's top-left corner.
    origin: [f32; 2],
    /// egui points per page point.
    scale: f32,
    view_min: egui::Pos2,
    /// The paper in egui points.
    pub paper: egui::Rect,
}

impl PageMap {
    pub fn new(draw: &PageDraw, view: egui::Rect, pixels_per_point: f32) -> Self {
        let paper = egui::Rect::from_min_max(
            view.min + egui::vec2(draw.paper[0], draw.paper[1]) / pixels_per_point,
            view.min + egui::vec2(draw.paper[2], draw.paper[3]) / pixels_per_point,
        );
        Self {
            page: draw.page,
            origin: draw.camera.origin,
            scale: draw.camera.scale / pixels_per_point,
            view_min: view.min,
            paper,
        }
    }

    pub fn screen_pos(&self, point: [f32; 2]) -> egui::Pos2 {
        self.view_min
            + egui::vec2(
                (point[0] - self.origin[0]) * self.scale,
                (point[1] - self.origin[1]) * self.scale,
            )
    }

    pub fn rect_to_screen(&self, rect: PageRect) -> egui::Rect {
        egui::Rect::from_min_max(
            self.screen_pos([rect[0], rect[1]]),
            self.screen_pos([rect[2], rect[3]]),
        )
    }

    pub fn page_point(&self, pos: egui::Pos2) -> [f32; 2] {
        [
            self.origin[0] + (pos.x - self.view_min.x) / self.scale,
            self.origin[1] + (pos.y - self.view_min.y) / self.scale,
        ]
    }

    /// Page points per egui point, for hit slop given in egui points.
    pub fn points(&self, screen: f32) -> f32 {
        screen / self.scale
    }
}

/// The page under a screen position.
pub fn page_at(maps: &[PageMap], pos: egui::Pos2) -> Option<&PageMap> {
    maps.iter().find(|map| map.paper.contains(pos))
}

/// Fills page rectangles on screen, clipped to the paper.
pub fn fill_rects(
    painter: &egui::Painter,
    map: &PageMap,
    rects: impl IntoIterator<Item = PageRect>,
    color: egui::Color32,
) {
    let painter = painter.with_clip_rect(painter.clip_rect().intersect(map.paper));
    for rect in rects {
        painter.rect_filled(map.rect_to_screen(rect), 1.0, color);
    }
}

#[cfg(test)]
mod tests {
    use glyphst_pdf::text::Glyph;
    use glyphst_pdf_gpu::Camera;

    use super::*;

    #[test]
    fn page_points_map_to_the_screen_and_back() {
        let draw = PageDraw {
            page: 3,
            camera: Camera {
                origin: [-10.0, 50.0],
                scale: 4.0,
                viewport: [800, 600],
            },
            paper: [40.0, 0.0, 440.0, 600.0],
        };
        let view = egui::Rect::from_min_size(egui::pos2(100.0, 30.0), egui::vec2(400.0, 300.0));
        let map = PageMap::new(&draw, view, 2.0);
        let pos = map.screen_pos([0.0, 50.0]);
        assert_eq!(pos, egui::pos2(120.0, 30.0));
        assert_eq!(map.paper.min, egui::pos2(120.0, 30.0));
        assert_eq!(map.page_point(egui::pos2(122.0, 34.0)), [1.0, 52.0]);
        assert!(page_at(&[map], egui::pos2(130.0, 40.0)).is_some());
        assert!(page_at(&[map], egui::pos2(110.0, 40.0)).is_none());
    }

    #[test]
    fn runs_select_in_either_direction_and_columns_by_area() {
        let text = PageText {
            glyphs: "ab cd"
                .chars()
                .enumerate()
                .map(|(i, ch)| Glyph {
                    ch,
                    rect: [i as f32 * 10.0, 0.0, i as f32 * 10.0 + 10.0, 10.0],
                })
                .collect(),
            line_of: vec![0; 5],
        };
        let backwards = Selection {
            page: 0,
            span: Span::Run { anchor: 4, head: 1 },
        };
        assert_eq!(backwards.range(), Some(1..5));
        assert_eq!(backwards.text(&text), "b cd");
        assert_eq!(backwards.rects(&text), vec![[10.0, 0.0, 50.0, 10.0]]);
        assert_eq!(Selection::run(0, 3..5).text(&text), "cd");
        let column = Selection {
            page: 0,
            span: Span::Column {
                from: [12.0, 0.0],
                to: [38.0, 10.0],
            },
        };
        assert_eq!(column.text(&text), "b c");
    }
}
