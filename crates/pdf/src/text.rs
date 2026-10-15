//! Page text and links for selection, search and navigation.
//!
//! [`PageText`] follows pdfcraft's `PageText` (artcraft, `crates/render/src/text.rs`): glyphs in
//! reading order with their boxes and lines, from which text, search, word and line hit-testing
//! and selection rectangles are derived. The glyphs are MuPDF's structured-text characters,
//! whitespace included and ligatures expanded, as the GPUI Reader indexes them, so glyph offsets
//! in reading positions and highlight anchors mean the same in both readers. Boxes are in the
//! page space the GPU view uses (MuPDF page coordinates, unrotated, y down), which is also where
//! the library stores highlight rectangles.

use std::ops::Range;

use anyhow::{Context as _, Result};
use mupdf::{DestinationKind, DisplayList, Page, TextPage, TextPageFlags};

use crate::vector::PageRect;

#[derive(Clone, Debug, PartialEq)]
pub struct Glyph {
    pub ch: char,
    /// Box in page space: `[x0, y0, x1, y1]`.
    pub rect: PageRect,
}

/// The text of one page in reading order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PageText {
    pub glyphs: Vec<Glyph>,
    /// For each glyph, its line; lines are numbered in reading order.
    pub line_of: Vec<u32>,
}

/// Folds a character for case-insensitive search, one character for one. The dotted and
/// dotless i (`İ`, `ı`) fold with `I`/`i`, as in pdfcraft: the document's language is unknown.
fn fold_case(c: char) -> char {
    match c {
        'I' | 'i' | '\u{130}' | '\u{131}' => 'i',
        _ => c.to_lowercase().next().unwrap_or(c),
    }
}

impl PageText {
    /// Reads the structured text of a page's display list.
    pub fn from_display_list(list: &DisplayList) -> Result<Self> {
        let page = list
            .to_text_page(TextPageFlags::PRESERVE_WHITESPACE)
            .context("failed to extract page text")?;
        Ok(Self::from_text_page(&page))
    }

    /// Reads the structured text of a page without keeping a display list.
    pub fn from_page(page: &Page) -> Result<Self> {
        let page = page
            .to_text_page(TextPageFlags::PRESERVE_WHITESPACE)
            .context("failed to extract page text")?;
        Ok(Self::from_text_page(&page))
    }

    fn from_text_page(page: &TextPage) -> Self {
        let mut text = Self::default();
        let mut line = 0u32;
        for block in page.blocks() {
            for text_line in block.lines() {
                let start = text.glyphs.len();
                for character in text_line.chars() {
                    let Some(ch) = character.char() else {
                        continue;
                    };
                    let quad = character.quad();
                    let xs = [quad.ul.x, quad.ur.x, quad.ll.x, quad.lr.x];
                    let ys = [quad.ul.y, quad.ur.y, quad.ll.y, quad.lr.y];
                    let min = |v: [f32; 4]| v.into_iter().fold(f32::INFINITY, f32::min);
                    let max = |v: [f32; 4]| v.into_iter().fold(f32::NEG_INFINITY, f32::max);
                    text.glyphs.push(Glyph {
                        ch,
                        rect: [min(xs), min(ys), max(xs), max(ys)],
                    });
                    text.line_of.push(line);
                }
                if text.glyphs.len() > start {
                    line += 1;
                }
            }
        }
        text
    }

    pub fn len(&self) -> usize {
        self.glyphs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.glyphs.is_empty()
    }

    fn is_blank(&self, index: usize) -> bool {
        self.glyphs[index].ch.is_whitespace()
    }

    fn new_line(&self, index: usize) -> bool {
        index > 0 && self.line_of[index] != self.line_of[index - 1]
    }

    /// The text of glyphs `range`: runs of whitespace become one space, line ends newlines.
    pub fn text_of(&self, range: Range<usize>) -> String {
        let glyphs: Vec<usize> = (range.start..range.end.min(self.len())).collect();
        self.text_of_glyphs(&glyphs)
    }

    pub fn plain_text(&self) -> String {
        self.text_of(0..self.len())
    }

    /// The text of glyphs in reading order that need not be contiguous (a column selection
    /// skips the ends of lines): a gap within a line reads as a space.
    pub fn text_of_glyphs(&self, glyphs: &[usize]) -> String {
        let mut out = String::new();
        let mut previous: Option<usize> = None;
        for &index in glyphs {
            if let Some(previous) = previous {
                if self.line_of[index] != self.line_of[previous] {
                    out.truncate(out.trim_end_matches(' ').len());
                    out.push('\n');
                } else if index != previous + 1 && !out.ends_with(' ') {
                    out.push(' ');
                }
            }
            previous = Some(index);
            let ch = self.glyphs[index].ch;
            if ch.is_whitespace() {
                if !out.is_empty() && !out.ends_with([' ', '\n']) {
                    out.push(' ');
                }
            } else {
                out.push(ch);
            }
        }
        out.truncate(out.trim_end().len());
        out
    }

    /// Glyph ranges of every match of `needle`. Runs of whitespace in the needle match any run
    /// of whitespace or a line break; matches may cross lines.
    pub fn find(&self, needle: &str, case_sensitive: bool, whole_words: bool) -> Vec<Range<usize>> {
        let fold = |c: char| if case_sensitive { c } else { fold_case(c) };
        let needle: Vec<char> = needle
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .map(fold)
            .collect();
        if needle.is_empty() {
            return Vec::new();
        }
        // The page as characters, each with the glyph it came from; whitespace and line breaks
        // collapse to one space.
        let mut chars: Vec<(char, usize)> = Vec::with_capacity(self.len() + 64);
        for (index, glyph) in self.glyphs.iter().enumerate() {
            let space = glyph.ch.is_whitespace() || self.new_line(index);
            if space && chars.last().is_some_and(|(c, _)| *c != ' ') {
                chars.push((' ', index));
            }
            if !glyph.ch.is_whitespace() {
                chars.push((fold(glyph.ch), index));
            }
        }
        let is_word = |at: Option<&(char, usize)>| at.is_some_and(|(c, _)| c.is_alphanumeric());
        let mut out = Vec::new();
        let mut at = 0;
        while at + needle.len() <= chars.len() {
            let bounded = !whole_words
                || (!is_word(at.checked_sub(1).and_then(|before| chars.get(before)))
                    && !is_word(chars.get(at + needle.len())));
            if bounded
                && chars[at..at + needle.len()]
                    .iter()
                    .map(|(c, _)| *c)
                    .eq(needle.iter().copied())
            {
                out.push(chars[at].1..chars[at + needle.len() - 1].1 + 1);
                at += needle.len();
            } else {
                at += 1;
            }
        }
        out
    }

    /// Glyph ranges matching `pattern` in the page's text, where
    /// whitespace and line breaks read as one space.
    pub fn find_pattern(&self, pattern: &regex::Regex) -> Vec<Range<usize>> {
        // The page as a string, each byte's glyph alongside.
        let mut text = String::with_capacity(self.len() + 64);
        let mut glyph_of: Vec<usize> = Vec::with_capacity(self.len() + 64);
        let mut push = |text: &mut String, ch: char, index: usize| {
            text.push(ch);
            glyph_of.extend(std::iter::repeat_n(index, ch.len_utf8()));
        };
        for (index, glyph) in self.glyphs.iter().enumerate() {
            let space = glyph.ch.is_whitespace() || self.new_line(index);
            if space && !text.is_empty() && !text.ends_with(' ') {
                push(&mut text, ' ', index);
            }
            if !glyph.ch.is_whitespace() {
                push(&mut text, glyph.ch, index);
            }
        }
        pattern
            .find_iter(&text)
            .filter(|found| !found.is_empty())
            .map(|found| glyph_of[found.start()]..glyph_of[found.end() - 1] + 1)
            .collect()
    }

    /// The glyph nearest to a page-space point, preferring the same line.
    pub fn nearest(&self, x: f32, y: f32) -> Option<usize> {
        let distance = |rect: &PageRect| {
            let dx = (rect[0] - x).max(0.0) + (x - rect[2]).max(0.0);
            let dy = (rect[1] - y).max(0.0) + (y - rect[3]).max(0.0);
            dx + dy * 3.0
        };
        self.glyphs
            .iter()
            .enumerate()
            .map(|(index, glyph)| (index, distance(&glyph.rect)))
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(index, _)| index)
    }

    /// The glyph under a page-space point, if any, `slop` points around each box included.
    pub fn glyph_at(&self, x: f32, y: f32, slop: f32) -> Option<usize> {
        self.glyphs.iter().position(|glyph| {
            x >= glyph.rect[0] - slop
                && x <= glyph.rect[2] + slop
                && y >= glyph.rect[1] - slop
                && y <= glyph.rect[3] + slop
        })
    }

    /// The word containing glyph `index`: it stops at whitespace and line ends. A whitespace
    /// glyph is a word of its own.
    pub fn word_at(&self, index: usize) -> Option<Range<usize>> {
        self.glyphs.get(index)?;
        if self.is_blank(index) {
            return Some(index..index + 1);
        }
        let mut start = index;
        while start > 0 && !self.new_line(start) && !self.is_blank(start - 1) {
            start -= 1;
        }
        let mut end = index + 1;
        while end < self.len() && !self.new_line(end) && !self.is_blank(end) {
            end += 1;
        }
        Some(start..end)
    }

    /// The line containing glyph `index`.
    pub fn line_at(&self, index: usize) -> Option<Range<usize>> {
        let line = *self.line_of.get(index)?;
        let start = self.line_of[..index]
            .iter()
            .rposition(|&l| l != line)
            .map_or(0, |p| p + 1);
        let end = self.line_of[index..]
            .iter()
            .position(|&l| l != line)
            .map_or(self.len(), |p| index + p);
        Some(start..end)
    }

    /// One rectangle per line run of glyphs `range`, whitespace at the ends left out, for
    /// drawing a selection or storing a highlight.
    pub fn selection_rects(&self, range: Range<usize>) -> Vec<PageRect> {
        let glyphs: Vec<usize> = (range.start..range.end.min(self.len())).collect();
        self.rects_of_glyphs(&glyphs)
    }

    /// One rectangle per line of the given glyphs, whitespace left out.
    pub fn rects_of_glyphs(&self, glyphs: &[usize]) -> Vec<PageRect> {
        let mut rects: Vec<(u32, PageRect)> = Vec::new();
        for &index in glyphs {
            if self.is_blank(index) {
                continue;
            }
            let rect = self.glyphs[index].rect;
            match rects.last_mut() {
                Some((line, bounds)) if *line == self.line_of[index] => {
                    *bounds = union(*bounds, rect);
                }
                _ => rects.push((self.line_of[index], rect)),
            }
        }
        rects.into_iter().map(|(_, rect)| rect).collect()
    }

    /// Glyphs whose centres lie in a page-space rectangle (column selection), in reading order.
    pub fn glyphs_in(&self, rect: PageRect) -> Vec<usize> {
        let [x0, y0, x1, y1] = [
            rect[0].min(rect[2]),
            rect[1].min(rect[3]),
            rect[0].max(rect[2]),
            rect[1].max(rect[3]),
        ];
        self.glyphs
            .iter()
            .enumerate()
            .filter(|(_, glyph)| {
                let cx = (glyph.rect[0] + glyph.rect[2]) / 2.0;
                let cy = (glyph.rect[1] + glyph.rect[3]) / 2.0;
                cx >= x0 && cx <= x1 && cy >= y0 && cy <= y1
            })
            .map(|(index, _)| index)
            .collect()
    }
}

fn union(a: PageRect, b: PageRect) -> PageRect {
    [
        a[0].min(b[0]),
        a[1].min(b[1]),
        a[2].max(b[2]),
        a[3].max(b[3]),
    ]
}

/// Where a link goes.
#[derive(Clone, Debug, PartialEq)]
pub enum LinkTarget {
    /// A page of this document, and the page-space y to bring to the top when known.
    Page {
        page: usize,
        y: Option<f32>,
    },
    Uri(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct PageLink {
    pub rect: PageRect,
    pub target: LinkTarget,
}

/// The page-space y a destination brings to the top, when it names one.
pub fn destination_top(kind: &DestinationKind) -> Option<f32> {
    match *kind {
        DestinationKind::XYZ { top, .. }
        | DestinationKind::FitH { top }
        | DestinationKind::FitBH { top } => top,
        DestinationKind::FitR { bottom, top, .. } => Some(bottom.min(top)),
        _ => None,
    }
}

/// The links of a page; links MuPDF cannot resolve are left out.
pub fn page_links(page: &Page) -> Vec<PageLink> {
    let Ok(links) = page.links() else {
        return Vec::new();
    };
    links
        .filter_map(|link| {
            let rect = [
                link.bounds.x0,
                link.bounds.y0,
                link.bounds.x1,
                link.bounds.y1,
            ];
            let target = match link.dest {
                Some(dest) => LinkTarget::Page {
                    page: dest.loc.page_number as usize,
                    y: destination_top(&dest.kind),
                },
                None if !link.uri.is_empty() => LinkTarget::Uri(link.uri),
                None => return None,
            };
            Some(PageLink { rect, target })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use mupdf::Document;

    use super::*;
    use crate::test_pdf::single_page_pdf;

    const FONT: &str =
        "<< /Font << /F1 << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> >> >>";

    fn text(content: &str) -> PageText {
        let bytes = single_page_pdf(content, FONT, "");
        let document = Document::from_bytes(&bytes, "application/pdf").unwrap();
        let list = document
            .load_page(0)
            .unwrap()
            .to_display_list(false)
            .unwrap();
        PageText::from_display_list(&list).unwrap()
    }

    /// Two lines: "Hello World" above "second line".
    fn two_lines() -> PageText {
        text("BT /F1 12 Tf 10 80 Td (Hello World) Tj 0 -20 Td (second line) Tj ET")
    }

    #[test]
    fn glyphs_come_in_reading_order_with_word_gaps_and_lines() {
        let text = two_lines();
        assert_eq!(text.plain_text(), "Hello World\nsecond line");
        assert_eq!(text.len(), "Hello Worldsecond line".len());
        assert_eq!(text.glyphs[5].ch, ' ', "spaces are glyphs");
        assert_ne!(
            text.line_of[10], text.line_of[11],
            "'d' and 's' are on different lines"
        );
        assert!(
            text.glyphs[0].rect[1] < text.glyphs[11].rect[1],
            "y grows downward"
        );
    }

    #[test]
    fn search_folds_case_honours_whole_words_and_crosses_lines() {
        let text = two_lines();
        assert_eq!(text.find("world", false, false), vec![6..11]);
        assert!(text.find("world", true, false).is_empty());
        assert_eq!(text.find("World   second", false, false), vec![6..17]);
        assert_eq!(text.find("lin", false, false).len(), 1);
        assert!(text.find("lin", false, true).is_empty());
        assert!(text.find("   ", false, false).is_empty());
    }

    #[test]
    fn patterns_match_across_lines_and_map_to_glyphs() {
        let text = two_lines();
        let pattern = |source: &str| regex::Regex::new(source).unwrap();
        assert_eq!(text.find_pattern(&pattern("W[a-z]+")), vec![6..11]);
        // The line break reads as a space.
        assert_eq!(text.find_pattern(&pattern(r"d s")), vec![10..12]);
        assert_eq!(text.find_pattern(&pattern(r"(?i)\bhello\b")), vec![0..5]);
        assert!(text.find_pattern(&pattern("x^")).is_empty());
    }

    #[test]
    fn words_lines_and_hit_testing() {
        let text = two_lines();
        assert_eq!(text.word_at(8), Some(6..11));
        assert_eq!(text.word_at(5), Some(5..6), "a space is its own word");
        assert_eq!(text.line_at(13), Some(11..22));
        let w = &text.glyphs[6].rect;
        let (cx, cy) = ((w[0] + w[2]) / 2.0, (w[1] + w[3]) / 2.0);
        assert_eq!(text.glyph_at(cx, cy, 0.0), Some(6));
        assert_eq!(text.nearest(cx, cy - 2.0), Some(6));
        assert_eq!(text.glyph_at(500.0, 500.0, 2.0), None);
    }

    #[test]
    fn selections_give_one_rectangle_per_line_and_columns_pick_by_centre() {
        let text = two_lines();
        let rects = text.selection_rects(3..15);
        assert_eq!(rects.len(), 2);
        assert!(
            rects[0][3] <= rects[1][1] + 1.0,
            "first line above the second"
        );
        let column = [0.0, 0.0, text.glyphs[1].rect[2], 200.0];
        let glyphs = text.glyphs_in(column);
        assert_eq!(text.text_of_glyphs(&glyphs), "He\nse");
        assert_eq!(text.text_of(3..15), "lo World\nseco");
    }

    #[test]
    fn links_resolve_pages_and_uris() {
        let bytes = crate::test_pdf::pdf_with(
            "",
            "<< >>",
            "/Annots [5 0 R 6 0 R]",
            &[
                "<< /Type /Annot /Subtype /Link /Rect [10 10 50 30] /Dest [3 0 R /XYZ 0 60 0] >>",
                "<< /Type /Annot /Subtype /Link /Rect [60 10 90 30] \
                 /A << /S /URI /URI (https://example.org/) >> >>",
            ],
        );
        let document = Document::from_bytes(&bytes, "application/pdf").unwrap();
        let links = page_links(&document.load_page(0).unwrap());
        assert_eq!(links.len(), 2, "{links:?}");
        assert!(
            links.iter().any(|link| link.target
                == LinkTarget::Page {
                    page: 0,
                    y: Some(40.0)
                }),
            "/XYZ top 60 on a 100 pt page is 40 pt from the top: {links:?}"
        );
        assert!(
            links
                .iter()
                .any(|link| link.target == LinkTarget::Uri("https://example.org/".into()))
        );
    }
}
