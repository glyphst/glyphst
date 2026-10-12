//! Highlights, notes and bookmarks of a document, from the shared library.
//!
//! Fixed-layout highlights draw from their stored page rectangles, which
//! both readers write in MuPDF page space. Reflowed ones are found again
//! from their semantic range in the current layout, since rectangles from
//! another layout no longer fit the text.

use std::{collections::HashMap, path::Path};

use glyphst_library::{
    Bookmark, Highlight, HighlightColor, HighlightRect, LibraryStore, Note, SemanticTextRange,
};
use glyphst_pdf::vector::PageRect;

use crate::doc_text::DocText;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AnnotationId {
    Highlight(u64),
    Note(u64),
}

/// A highlight or a note, as shown.
#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub id: AnnotationId,
    pub color: HighlightColor,
    pub text: String,
    /// The note's text: a highlight's note or a note's body.
    pub body: String,
    pub semantic: Option<SemanticTextRange>,
    pub stored: Vec<HighlightRect>,
}

#[derive(Default)]
pub struct Annotations {
    pub items: Vec<Item>,
    pub bookmarks: Vec<Bookmark>,
    /// Rectangles of reflowed items in the current layout; `None` when their
    /// text was not found.
    resolved: HashMap<AnnotationId, Option<Vec<HighlightRect>>>,
}

impl Annotations {
    pub fn load(library: &LibraryStore, path: &Path) -> Self {
        let highlights = library.highlights_for(path).into_iter().map(Item::from);
        let notes = library.notes_for(path).into_iter().map(Item::from);
        let mut items: Vec<Item> = highlights.chain(notes).collect();
        items.sort_by(|a, b| {
            let key = |item: &Item| {
                item.stored
                    .first()
                    .map_or((usize::MAX, 0.0), |rect| (rect.page_idx, rect.y0))
            };
            let (a, b) = (key(a), key(b));
            a.0.cmp(&b.0).then(a.1.total_cmp(&b.1))
        });
        Self {
            items,
            bookmarks: library.bookmarks_for(path),
            resolved: HashMap::new(),
        }
    }

    /// Forgets where reflowed items were, after a relayout.
    pub fn clear_resolved(&mut self) {
        self.resolved.clear();
    }

    /// Finds reflowed items in the current layout as their text arrives.
    pub fn resolve(&mut self, text: &DocText) {
        for item in &self.items {
            let Some(range) = &item.semantic else {
                continue;
            };
            if self.resolved.get(&item.id).is_some_and(Option::is_some) {
                continue;
            }
            let found = text
                .resolve(&range.start)
                .zip(text.resolve(&range.end))
                .map(|(start, end)| rects_between(text, start.min(end), start.max(end)));
            if found.is_some() || text.is_complete() {
                self.resolved.insert(item.id, found);
            }
        }
    }

    /// The rectangles an item covers in the current layout.
    pub fn rects<'a>(&'a self, item: &'a Item, reflowed: bool) -> &'a [HighlightRect] {
        if reflowed && item.semantic.is_some() {
            self.resolved
                .get(&item.id)
                .and_then(Option::as_deref)
                .unwrap_or_default()
        } else {
            &item.stored
        }
    }

    /// Items on a page with their rectangles there.
    pub fn on_page(&self, page: usize, reflowed: bool) -> Vec<(&Item, Vec<PageRect>)> {
        self.items
            .iter()
            .filter_map(|item| {
                let rects: Vec<PageRect> = self
                    .rects(item, reflowed)
                    .iter()
                    .filter(|rect| rect.page_idx == page)
                    .map(|rect| [rect.x0, rect.y0, rect.x1, rect.y1])
                    .collect();
                (!rects.is_empty()).then_some((item, rects))
            })
            .collect()
    }

    /// The item under a page point, the last drawn first.
    pub fn hit(&self, page: usize, point: [f32; 2], reflowed: bool) -> Option<AnnotationId> {
        self.on_page(page, reflowed)
            .into_iter()
            .rev()
            .find(|(_, rects)| {
                rects.iter().any(|r| {
                    point[0] >= r[0] && point[0] <= r[2] && point[1] >= r[1] && point[1] <= r[3]
                })
            })
            .map(|(item, _)| item.id)
    }

    pub fn get(&self, id: AnnotationId) -> Option<&Item> {
        self.items.iter().find(|item| item.id == id)
    }

    /// The first page an item is on in the current layout.
    pub fn page_of(&self, item: &Item, reflowed: bool) -> Option<(usize, f32)> {
        self.rects(item, reflowed)
            .first()
            .map(|rect| (rect.page_idx, rect.y0))
    }
}

/// Rectangles from glyph `start` to `end` (both included), across pages.
fn rects_between(text: &DocText, start: (usize, usize), end: (usize, usize)) -> Vec<HighlightRect> {
    let mut rects = Vec::new();
    for page in start.0..=end.0 {
        let Some(page_text) = text.get(page) else {
            continue;
        };
        let from = if page == start.0 { start.1 } else { 0 };
        let to = if page == end.0 {
            end.1 + 1
        } else {
            page_text.len()
        };
        rects.extend(
            page_text
                .selection_rects(from..to)
                .into_iter()
                .map(|r| HighlightRect {
                    page_idx: page,
                    x0: r[0],
                    y0: r[1],
                    x1: r[2],
                    y1: r[3],
                }),
        );
    }
    rects
}

impl From<Highlight> for Item {
    fn from(highlight: Highlight) -> Self {
        Self {
            id: AnnotationId::Highlight(highlight.id),
            color: highlight.color,
            text: highlight.text,
            body: highlight.note,
            semantic: highlight.semantic_range,
            stored: highlight.rects,
        }
    }
}

impl From<Note> for Item {
    fn from(note: Note) -> Self {
        Self {
            id: AnnotationId::Note(note.id),
            color: note.color,
            text: note.text,
            body: note.body,
            semantic: note.semantic_range,
            stored: note.rects,
        }
    }
}

/// The colour of a highlight, opaque.
pub fn color(color: HighlightColor) -> egui::Color32 {
    match color {
        HighlightColor::Yellow => egui::Color32::from_rgb(250, 204, 21),
        HighlightColor::Green => egui::Color32::from_rgb(74, 222, 128),
        HighlightColor::Blue => egui::Color32::from_rgb(96, 165, 250),
        HighlightColor::Pink => egui::Color32::from_rgb(244, 114, 182),
        HighlightColor::Orange => egui::Color32::from_rgb(251, 146, 60),
    }
}

/// The translucent fill drawn over page text.
pub fn fill(color_of: HighlightColor) -> egui::Color32 {
    color(color_of).gamma_multiply(0.38)
}

pub fn color_name(color: HighlightColor) -> &'static str {
    match color {
        HighlightColor::Yellow => "Yellow",
        HighlightColor::Green => "Green",
        HighlightColor::Blue => "Blue",
        HighlightColor::Pink => "Pink",
        HighlightColor::Orange => "Orange",
    }
}

/// Converts selection rectangles on a page for the library.
pub fn library_rects(page: usize, rects: &[PageRect]) -> Vec<HighlightRect> {
    rects
        .iter()
        .map(|r| HighlightRect {
            page_idx: page,
            x0: r[0],
            y0: r[1],
            x1: r[2],
            y1: r[3],
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use glyphst_pdf::text::{Glyph, PageText};

    use super::*;

    fn page(text: &str, y: f32) -> Arc<PageText> {
        Arc::new(PageText {
            glyphs: text
                .chars()
                .enumerate()
                .map(|(i, ch)| Glyph {
                    ch,
                    rect: [i as f32 * 6.0, y, i as f32 * 6.0 + 6.0, y + 12.0],
                })
                .collect(),
            line_of: vec![0; text.chars().count()],
        })
    }

    fn document(pages: &[&str]) -> DocText {
        let mut text = DocText::new(pages.len());
        for (index, content) in pages.iter().enumerate() {
            text.insert(index, page(content, 100.0));
        }
        text
    }

    #[test]
    fn highlights_round_trip_through_the_library_in_both_layouts() {
        let dir = std::env::temp_dir().join(format!(
            "glyphst-reader-egui-annotations-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let mut library = LibraryStore::open_for_test(dir.join("library.redb")).unwrap();
        let path = Path::new("/books/pride.epub");
        // Written in one layout: "truth universally" from glyph 8 to 24.
        let before = document(&["It is a truth ", "universally acknowledged"]);
        let range = SemanticTextRange {
            start: before.anchor(0, 8).unwrap(),
            end: before.anchor(1, 10).unwrap(),
        };
        let rects = library_rects(0, &[[48.0, 100.0, 78.0, 112.0]]);
        library
            .add_reflow_highlight(
                path,
                HighlightColor::Green,
                range,
                rects,
                "truth universally",
                "",
            )
            .unwrap();
        library
            .add_highlight(
                Path::new("/books/paper.pdf"),
                HighlightColor::Yellow,
                library_rects(2, &[[1.0, 2.0, 3.0, 4.0]]),
                "fixed",
                "",
            )
            .unwrap();

        // Read back after a relayout that moved the text to the next page.
        let after = document(&["Preface ", "It is a truth universally acknowledged"]);
        let mut annotations = Annotations::load(&library, path);
        assert_eq!(annotations.items.len(), 1);
        annotations.resolve(&after);
        let item = annotations.items[0].clone();
        let shown = annotations.on_page(1, true);
        assert_eq!(shown.len(), 1);
        assert_eq!(shown[0].1, vec![[48.0, 100.0, 150.0, 112.0]]);
        assert!(
            annotations.on_page(0, true).is_empty(),
            "the stored rectangle is not used"
        );
        assert_eq!(annotations.hit(1, [60.0, 105.0], true), Some(item.id));
        assert_eq!(annotations.hit(1, [10.0, 105.0], true), None);

        let fixed = Annotations::load(&library, Path::new("/books/paper.pdf"));
        assert_eq!(fixed.on_page(2, false)[0].1, vec![[1.0, 2.0, 3.0, 4.0]]);
        drop(library);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
