//! The text of a document's pages as they arrive, with document-wide glyph
//! offsets for semantic anchors.
//!
//! Anchors follow the GPUI Reader's (`document_view/position.rs`): the
//! logical offset is the glyph's index in the document's glyphs (whitespace
//! included), the quote its next 32 glyphs, and the prefix and suffix the 24
//! glyphs around them. Reflowed positions, highlights and notes saved by
//! either reader resolve in the other.

use std::sync::Arc;

use glyphst_core::SemanticTextAnchor;
use glyphst_pdf::text::PageText;

const QUOTE_LEN: usize = 32;
const CONTEXT_LEN: usize = 24;

#[derive(Default)]
pub struct DocText {
    pages: Vec<Option<Arc<PageText>>>,
}

impl DocText {
    pub fn new(pages: usize) -> Self {
        Self {
            pages: vec![None; pages],
        }
    }

    /// Stores a page's text; returns whether it was new.
    pub fn insert(&mut self, page: usize, text: Arc<PageText>) -> bool {
        match self.pages.get_mut(page) {
            Some(slot @ None) => {
                *slot = Some(text);
                true
            }
            _ => false,
        }
    }

    pub fn get(&self, page: usize) -> Option<&Arc<PageText>> {
        self.pages.get(page)?.as_ref()
    }

    pub fn page_count(&self) -> usize {
        self.pages.len()
    }

    pub fn is_complete(&self) -> bool {
        self.pages.iter().all(Option::is_some)
    }

    pub fn remove(&mut self, page: usize) {
        if let Some(text) = self.pages.get_mut(page) {
            *text = None;
        }
    }

    /// Drops the text of pages `keep` rejects.
    pub fn retain(&mut self, keep: impl Fn(usize) -> bool) {
        for (page, text) in self.pages.iter_mut().enumerate() {
            if !keep(page) {
                *text = None;
            }
        }
    }

    /// Document offset of `page`'s first glyph, when every page before it is
    /// known.
    pub fn start_of(&self, page: usize) -> Option<usize> {
        self.pages
            .get(..page)?
            .iter()
            .map(|text| text.as_ref().map(|text| text.len()))
            .sum()
    }

    /// The page and glyph at a document offset, when the pages up to it are
    /// known.
    fn locate(&self, offset: usize) -> Option<(usize, usize)> {
        let mut start = 0;
        for (page, text) in self.pages.iter().enumerate() {
            let len = text.as_ref()?.len();
            if offset < start + len {
                return Some((page, offset - start));
            }
            start += len;
        }
        None
    }

    /// Glyph characters from a document offset on, across pages, while the
    /// pages are known.
    fn chars_from(&self, page: usize, glyph: usize) -> impl Iterator<Item = char> + '_ {
        self.pages[page..]
            .iter()
            .map_while(|text| text.as_deref())
            .enumerate()
            .flat_map(move |(index, text)| {
                let skip = if index == 0 { glyph } else { 0 };
                text.glyphs[skip.min(text.len())..].iter().map(|g| g.ch)
            })
    }

    /// Glyph characters before a position, nearest first.
    fn chars_before(&self, page: usize, glyph: usize) -> impl Iterator<Item = char> + '_ {
        self.pages[..=page]
            .iter()
            .rev()
            .map_while(|text| text.as_deref())
            .enumerate()
            .flat_map(move |(index, text)| {
                let end = if index == 0 {
                    glyph.min(text.len())
                } else {
                    text.len()
                };
                text.glyphs[..end].iter().rev().map(|g| g.ch)
            })
    }

    /// The anchor of a glyph, when the pages before it are known.
    pub fn anchor(&self, page: usize, glyph: usize) -> Option<SemanticTextAnchor> {
        let offset = self.start_of(page)? + glyph;
        let quote: String = self.chars_from(page, glyph).take(QUOTE_LEN).collect();
        let quote_len = quote.chars().count();
        let suffix: String = self
            .chars_from(page, glyph)
            .skip(quote_len)
            .take(CONTEXT_LEN)
            .collect();
        let mut prefix: Vec<char> = self.chars_before(page, glyph).take(CONTEXT_LEN).collect();
        prefix.reverse();
        Some(SemanticTextAnchor::new(
            offset,
            quote,
            prefix.into_iter().collect::<String>(),
            suffix,
        ))
    }

    /// Finds an anchor's glyph: at its offset when the quote is there,
    /// otherwise where the quote, prefix and suffix match (or the quote's
    /// first match). Searching needs every page.
    pub fn resolve(&self, anchor: &SemanticTextAnchor) -> Option<(usize, usize)> {
        let quote: Vec<char> = anchor.quote.chars().collect();
        let offset = usize::try_from(anchor.logical_offset).ok()?;
        if let Some((page, glyph)) = self.locate(offset)
            && self
                .chars_from(page, glyph)
                .take(quote.len())
                .eq(quote.iter().copied())
        {
            return Some((page, glyph));
        }
        if !self.is_complete() {
            return None;
        }
        let all: Vec<char> = self
            .pages
            .iter()
            .flatten()
            .flat_map(|text| text.glyphs.iter().map(|g| g.ch))
            .collect();
        if all.is_empty() {
            return None;
        }
        if quote.is_empty() {
            return self.locate(offset.min(all.len() - 1));
        }
        let prefix: Vec<char> = anchor.prefix.chars().collect();
        let suffix: Vec<char> = anchor.suffix.chars().collect();
        let mut fallback = None;
        for start in 0..all.len().saturating_sub(quote.len() - 1) {
            if all[start..start + quote.len()] != quote[..] {
                continue;
            }
            fallback.get_or_insert(start);
            let before = &all[start.saturating_sub(prefix.len())..start];
            let after_start = start + quote.len();
            let after = &all[after_start..(after_start + suffix.len()).min(all.len())];
            if before.ends_with(&prefix) && after.starts_with(&suffix) {
                return self.locate(start);
            }
        }
        self.locate(fallback.unwrap_or(offset.min(all.len() - 1)))
    }
}

#[cfg(test)]
mod tests {
    use glyphst_pdf::text::Glyph;

    use super::*;

    fn page(text: &str) -> Arc<PageText> {
        Arc::new(PageText {
            glyphs: text
                .chars()
                .map(|ch| Glyph { ch, rect: [0.0; 4] })
                .collect(),
            line_of: vec![0; text.chars().count()],
        })
    }

    fn document(pages: &[&str]) -> DocText {
        let mut text = DocText::new(pages.len());
        for (index, content) in pages.iter().enumerate() {
            text.insert(index, page(content));
        }
        text
    }

    #[test]
    fn anchors_span_pages_and_resolve_at_their_offset() {
        let text = document(&[
            "It is a truth ",
            "universally acknowledged, that a single man",
        ]);
        let anchor = text.anchor(0, 11).unwrap();
        assert_eq!(anchor.logical_offset, 11);
        assert_eq!(anchor.quote, "th universally acknowledged, tha");
        assert_eq!(anchor.prefix, "It is a tru");
        assert_eq!(text.resolve(&anchor), Some((0, 11)));
    }

    #[test]
    fn anchors_survive_a_relayout_that_moves_their_text() {
        let before = document(&[
            "It is a truth ",
            "universally acknowledged, that a single man",
        ]);
        let anchor = before.anchor(1, 12).unwrap();
        let after = document(&[
            "Preface. ",
            "It is a truth universally ack",
            "nowledged, that a single man",
        ]);
        let (page, glyph) = after.resolve(&anchor).unwrap();
        assert_eq!((page, glyph), (1, 26));
        assert_eq!(after.anchor(page, glyph).unwrap().quote, anchor.quote);
    }

    #[test]
    fn unknown_pages_block_offsets_but_not_storage() {
        let mut text = DocText::new(3);
        text.insert(2, page("tail"));
        assert_eq!(text.start_of(2), None);
        assert!(text.anchor(2, 0).is_none());
        text.insert(0, page("ab"));
        text.insert(1, page("cd"));
        assert_eq!(text.start_of(2), Some(4));
        assert!(text.is_complete());
        text.retain(|page| page == 1);
        assert!(text.get(0).is_none() && text.get(1).is_some() && text.get(2).is_none());
    }
}
