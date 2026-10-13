//! Text on top of the GPU pages: mapping between page points and the
//! screen, the text selection (pdfcraft's `Selection`, artcraft
//! `ui-egui/src/canvas.rs`), and painting selections, search matches and
//! highlights as egui shapes over the page image.

use std::ops::{Range, RangeInclusive};

use glyphst_pdf::{text::PageText, vector::PageRect};
use glyphst_pdf_gpu::Camera;

use crate::gpu::PageDraw;

/// A glyph of a page.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct TextPos {
    pub page: usize,
    pub glyph: usize,
}

/// A text selection: a run of glyphs, possibly over several pages, or a
/// column of one page.
#[derive(Clone, Debug, PartialEq)]
pub enum Selection {
    /// Glyphs in reading order from `anchor` to `head`, both included;
    /// pages between them are selected whole.
    Run { anchor: TextPos, head: TextPos },
    /// Glyphs whose centres lie in a page rectangle (Alt+drag).
    Column {
        page: usize,
        from: [f32; 2],
        to: [f32; 2],
    },
}

impl Selection {
    /// Glyphs `range` of `page`.
    pub fn run(page: usize, range: Range<usize>) -> Self {
        Self::Run {
            anchor: TextPos {
                page,
                glyph: range.start,
            },
            head: TextPos {
                page,
                glyph: range.end.saturating_sub(1).max(range.start),
            },
        }
    }

    /// A run's first and last glyph in reading order.
    pub fn ordered(&self) -> Option<(TextPos, TextPos)> {
        match *self {
            Self::Run { anchor, head } => Some((anchor.min(head), anchor.max(head))),
            Self::Column { .. } => None,
        }
    }

    /// The pages it covers.
    pub fn pages(&self) -> RangeInclusive<usize> {
        match *self {
            Self::Run { anchor, head } => anchor.min(head).page..=anchor.max(head).page,
            Self::Column { page, .. } => page..=page,
        }
    }

    pub fn touches(&self, page: usize) -> bool {
        self.pages().contains(&page)
    }

    /// The selected glyphs of `page`, whose text is `text`.
    pub fn glyphs_on(&self, page: usize, text: &PageText) -> Vec<usize> {
        match self {
            Self::Run { .. } => {
                let Some((start, end)) = self.ordered().filter(|_| self.touches(page)) else {
                    return Vec::new();
                };
                let from = if page == start.page { start.glyph } else { 0 };
                let to = if page == end.page {
                    end.glyph + 1
                } else {
                    text.len()
                };
                (from..to.min(text.len())).collect()
            }
            Self::Column {
                page: on, from, to, ..
            } if *on == page => text.glyphs_in([from[0], from[1], to[0], to[1]]),
            Self::Column { .. } => Vec::new(),
        }
    }

    /// One rectangle per selected line of `page`.
    pub fn rects_on(&self, page: usize, text: &PageText) -> Vec<PageRect> {
        text.rects_of_glyphs(&self.glyphs_on(page, text))
    }

    /// The selected text, pages joined by a line break; `None` while a
    /// page's text is not read yet.
    pub fn text<'a>(&self, text_of: impl Fn(usize) -> Option<&'a PageText>) -> Option<String> {
        let mut parts = Vec::new();
        for page in self.pages() {
            let text = text_of(page)?;
            let part = text.text_of_glyphs(&self.glyphs_on(page, text));
            if !part.is_empty() {
                parts.push(part);
            }
        }
        Some(parts.join("\n"))
    }
}

/// Where a page is on screen: maps page points to egui points and back.
#[derive(Clone, Copy, Debug)]
pub struct PageMap {
    pub page: usize,
    /// The page's camera, in device pixels from the view's top-left.
    camera: Camera,
    pixels_per_point: f32,
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
            camera: draw.camera,
            pixels_per_point,
            view_min: view.min,
            paper,
        }
    }

    pub fn screen_pos(&self, point: [f32; 2]) -> egui::Pos2 {
        let [x, y] = self.camera.to_pixels(point);
        self.view_min + egui::vec2(x, y) / self.pixels_per_point
    }

    /// The screen rectangle covering a page rectangle (turned with the
    /// page).
    pub fn rect_to_screen(&self, rect: PageRect) -> egui::Rect {
        egui::Rect::from_two_pos(
            self.screen_pos([rect[0], rect[1]]),
            self.screen_pos([rect[2], rect[3]]),
        )
    }

    pub fn page_point(&self, pos: egui::Pos2) -> [f32; 2] {
        let pixels = (pos - self.view_min) * self.pixels_per_point;
        self.camera.to_page([pixels.x, pixels.y])
    }

    /// Page points per egui point, for hit slop given in egui points.
    pub fn points(&self, screen: f32) -> f32 {
        screen * self.pixels_per_point / self.camera.scale
    }
}

/// The page under a screen position.
pub fn page_at(maps: &[PageMap], pos: egui::Pos2) -> Option<&PageMap> {
    maps.iter().find(|map| map.paper.contains(pos))
}

/// The page under a screen position, or else the nearest one (between
/// pages, or past the last), for extending a selection.
pub fn nearest_page(maps: &[PageMap], pos: egui::Pos2) -> Option<&PageMap> {
    page_at(maps, pos).or_else(|| {
        maps.iter().min_by(|a, b| {
            a.paper
                .distance_sq_to_pos(pos)
                .total_cmp(&b.paper.distance_sq_to_pos(pos))
        })
    })
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

    use super::*;

    #[test]
    fn page_points_map_to_the_screen_and_back() {
        let draw = PageDraw {
            page: 3,
            camera: Camera {
                origin: [-10.0, 50.0],
                scale: 4.0,
                viewport: [800, 600],
                rotation: 0,
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

        // A page turned a quarter: its points still map there and back,
        // and its rectangles turn on their side.
        let turned = PageMap::new(
            &PageDraw {
                camera: Camera::centered_rotated([50.0, 100.0], 4.0, [800, 600], 1),
                ..draw
            },
            view,
            2.0,
        );
        for point in [[0.0, 0.0], [100.0, 0.0], [30.0, 170.0]] {
            let back = turned.page_point(turned.screen_pos(point));
            assert!((back[0] - point[0]).abs() < 1e-3 && (back[1] - point[1]).abs() < 1e-3);
        }
        let wide = turned.rect_to_screen([0.0, 0.0, 100.0, 10.0]);
        assert!(wide.height() > wide.width());
        assert!(turned.points(2.0) - 1.0 < 1e-6);
    }

    fn line(text: &str) -> PageText {
        PageText {
            glyphs: text
                .chars()
                .enumerate()
                .map(|(i, ch)| Glyph {
                    ch,
                    rect: [i as f32 * 10.0, 0.0, i as f32 * 10.0 + 10.0, 10.0],
                })
                .collect(),
            line_of: vec![0; text.chars().count()],
        }
    }

    #[test]
    fn runs_select_in_either_direction_and_columns_by_area() {
        let text = line("ab cd");
        let backwards = Selection::Run {
            anchor: TextPos { page: 0, glyph: 4 },
            head: TextPos { page: 0, glyph: 1 },
        };
        assert_eq!(backwards.glyphs_on(0, &text), vec![1, 2, 3, 4]);
        assert_eq!(backwards.text(|_| Some(&text)).unwrap(), "b cd");
        assert_eq!(backwards.rects_on(0, &text), vec![[10.0, 0.0, 50.0, 10.0]]);
        assert_eq!(Selection::run(0, 3..5).text(|_| Some(&text)).unwrap(), "cd");
        let column = Selection::Column {
            page: 0,
            from: [12.0, 0.0],
            to: [38.0, 10.0],
        };
        assert_eq!(column.text(|_| Some(&text)).unwrap(), "b c");
        assert!(
            column.glyphs_on(1, &text).is_empty(),
            "a column stays on its page"
        );
    }

    #[test]
    fn runs_cross_pages_in_either_direction() {
        let pages = [line("one two"), line("middle"), line("end here")];
        let text_of = |page: usize| pages.get(page);
        // From "two" on the first page to "end" on the third, dragged up.
        let run = Selection::Run {
            anchor: TextPos { page: 2, glyph: 2 },
            head: TextPos { page: 0, glyph: 4 },
        };
        assert_eq!(run.pages(), 0..=2);
        assert!(run.touches(1) && !run.touches(3));
        assert_eq!(run.glyphs_on(0, &pages[0]), vec![4, 5, 6]);
        assert_eq!(
            run.glyphs_on(1, &pages[1]).len(),
            6,
            "a page between, whole"
        );
        assert_eq!(run.glyphs_on(2, &pages[2]), vec![0, 1, 2]);
        assert!(run.glyphs_on(3, &pages[2]).is_empty());
        assert_eq!(run.text(text_of).unwrap(), "two\nmiddle\nend");
        assert_eq!(run.rects_on(2, &pages[2]), vec![[0.0, 0.0, 30.0, 10.0]]);
        // A page whose text is not read yet: no text rather than part.
        assert_eq!(run.text(|page| (page != 1).then(|| &pages[page])), None);
    }

    #[test]
    fn a_selection_extends_to_the_nearest_page() {
        let draw = |page: usize, y: f32| PageDraw {
            page,
            camera: Camera {
                origin: [0.0, 0.0],
                scale: 1.0,
                viewport: [800, 600],
                rotation: 0,
            },
            paper: [0.0, y, 400.0, y + 200.0],
        };
        let view = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));
        let maps = [
            PageMap::new(&draw(4, 0.0), view, 1.0),
            PageMap::new(&draw(5, 220.0), view, 1.0),
        ];
        let page = |x: f32, y: f32| nearest_page(&maps, egui::pos2(x, y)).map(|map| map.page);
        assert_eq!(page(100.0, 100.0), Some(4), "on a page");
        assert_eq!(page(100.0, 215.0), Some(5), "in the gap, nearer the next");
        assert_eq!(page(100.0, 205.0), Some(4));
        assert_eq!(page(100.0, 590.0), Some(5), "past the last");
        assert_eq!(page(600.0, 300.0), Some(5), "beside it");
    }
}
