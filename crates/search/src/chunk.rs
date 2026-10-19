//! A page's text in chunks of about 1,800 characters that overlap by 200
//! and never leave the page, each with the anchor the Reader uses to find
//! text again (`reader/src/doc_text.rs`): the document-wide glyph offset,
//! the next 32 glyphs, and the 24 on either side.

use glyphst_pdf::text::PageText;

/// Characters a chunk aims for, and how many the next one repeats.
pub const CHUNK_CHARS: usize = 1_800;
pub const OVERLAP_CHARS: usize = 200;
const QUOTE_LEN: usize = 32;
const CONTEXT_LEN: usize = 24;
/// Fewer letters than this and a page counts as having no text.
pub const MIN_PAGE_CHARS: usize = 20;

#[derive(Clone, Debug, PartialEq)]
pub struct Chunk {
    pub page: usize,
    /// Glyphs of the page the chunk covers.
    pub glyph_start: usize,
    pub glyph_end: usize,
    /// The first glyph's offset in the whole document.
    pub doc_offset: usize,
    pub quote: String,
    pub prefix: String,
    pub suffix: String,
    /// The first line's boxes in page space.
    pub rects: Vec<[f32; 4]>,
    pub section: String,
    pub text: String,
}

/// The page's text, its letters, and whether it has almost none.
pub struct PageChunks {
    pub chunks: Vec<Chunk>,
    pub chars: usize,
    pub image_only: bool,
}

/// Chunks `text`, page `page` of a document whose earlier pages hold
/// `page_offset` glyphs and end with `before` (for the first prefix).
pub fn chunk_page(
    text: &PageText,
    page: usize,
    page_offset: usize,
    before: &str,
    section: &str,
) -> PageChunks {
    let chars = text
        .glyphs
        .iter()
        .filter(|glyph| glyph.ch.is_alphanumeric())
        .count();
    let (normal, glyph_of) = normalize(text);
    let mut chunks = Vec::new();
    if normal.iter().any(|ch| !ch.is_whitespace()) {
        for (start, end) in windows(&normal) {
            let glyph_start = glyph_of[start];
            let glyph_end = glyph_of
                .get(end)
                .copied()
                .unwrap_or(text.glyphs.len())
                .max(glyph_start + 1)
                .min(text.glyphs.len());
            let raw = |range: std::ops::Range<usize>| -> String {
                text.glyphs[range].iter().map(|glyph| glyph.ch).collect()
            };
            let quote_end = (glyph_start + QUOTE_LEN).min(text.glyphs.len());
            let quote = raw(glyph_start..quote_end);
            let suffix = raw(quote_end..(quote_end + CONTEXT_LEN).min(text.glyphs.len()));
            let prefix = {
                let on_page = raw(glyph_start.saturating_sub(CONTEXT_LEN)..glyph_start);
                let missing = CONTEXT_LEN.saturating_sub(on_page.chars().count());
                let carried: String = before
                    .chars()
                    .rev()
                    .take(missing)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect();
                format!("{carried}{on_page}")
            };
            let first_line = text.line_of.get(glyph_start).copied();
            let line_end = (glyph_start..glyph_end)
                .find(|index| text.line_of.get(*index).copied() != first_line)
                .unwrap_or(glyph_end);
            chunks.push(Chunk {
                page,
                glyph_start,
                glyph_end,
                doc_offset: page_offset + glyph_start,
                quote,
                prefix,
                suffix,
                rects: text.selection_rects(glyph_start..line_end),
                section: section.to_string(),
                text: normal[start..end]
                    .iter()
                    .collect::<String>()
                    .trim()
                    .to_string(),
            });
        }
    }
    PageChunks {
        chunks,
        chars,
        image_only: chars < MIN_PAGE_CHARS,
    }
}

/// The page's text for search: runs of whitespace as one space, a line
/// break between words a space, and a word hyphenated across lines joined.
/// Returns its characters and, for each, the glyph it came from.
fn normalize(text: &PageText) -> (Vec<char>, Vec<usize>) {
    let glyphs = &text.glyphs;
    let line = |index: usize| text.line_of.get(index).copied();
    let mut out = Vec::with_capacity(glyphs.len());
    let mut glyph_of = Vec::with_capacity(glyphs.len());
    let space = |out: &mut Vec<char>, glyph_of: &mut Vec<usize>, at: usize| {
        if out.last().is_some_and(|last: &char| !last.is_whitespace()) {
            out.push(' ');
            glyph_of.push(at);
        }
    };
    for (index, glyph) in glyphs.iter().enumerate() {
        let ends_line = index + 1 < glyphs.len() && line(index) != line(index + 1);
        if glyph.ch.is_whitespace() {
            space(&mut out, &mut glyph_of, index);
            continue;
        }
        // "atten-" at a line's end, "tion" on the next: one word.
        if ends_line
            && glyph.ch == '-'
            && out.last().is_some_and(|last| last.is_alphabetic())
            && glyphs[index + 1].ch.is_lowercase()
        {
            continue;
        }
        out.push(glyph.ch);
        glyph_of.push(index);
        if ends_line && !glyphs[index + 1].ch.is_whitespace() {
            space(&mut out, &mut glyph_of, index + 1);
        }
    }
    (out, glyph_of)
}

/// Character ranges of the chunks: up to `CHUNK_CHARS`, ending at a space
/// when one is near, each starting `OVERLAP_CHARS` before the last ended.
fn windows(text: &[char]) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let mut start = 0;
    // Leading spaces are not a chunk.
    while start < text.len() && text[start].is_whitespace() {
        start += 1;
    }
    while start < text.len() {
        let mut end = (start + CHUNK_CHARS).min(text.len());
        if end < text.len()
            && let Some(space) = (end.saturating_sub(OVERLAP_CHARS).max(start + 1)..end)
                .rev()
                .find(|index| text[*index].is_whitespace())
        {
            end = space;
        }
        ranges.push((start, end));
        if end >= text.len() {
            break;
        }
        let mut next = end.saturating_sub(OVERLAP_CHARS).max(start + 1);
        // Start the overlap at a word.
        while next < end && !text[next - 1].is_whitespace() {
            next += 1;
        }
        start = next;
    }
    ranges
}

#[cfg(test)]
mod tests {
    use glyphst_pdf::text::Glyph;

    use super::*;

    /// A page of `lines`, one glyph per character, each line its own.
    fn page(lines: &[&str]) -> PageText {
        let mut text = PageText::default();
        for (line, content) in lines.iter().enumerate() {
            for (column, ch) in content.chars().enumerate() {
                let x = column as f32 * 6.0;
                let y = line as f32 * 14.0;
                text.glyphs.push(Glyph {
                    ch,
                    rect: [x, y, x + 6.0, y + 12.0],
                });
                text.line_of.push(line as u32);
            }
        }
        text
    }

    #[test]
    fn lines_join_with_spaces_and_hyphenated_words_mend() {
        let text = page(&["The atten-", "tion is all", "you  need."]);
        let chunks = chunk_page(&text, 0, 0, "", "");
        assert_eq!(chunks.chunks.len(), 1);
        assert_eq!(chunks.chunks[0].text, "The attention is all you need.");
        assert!(!chunks.image_only);
    }

    #[test]
    fn long_pages_overlap_at_words_and_carry_anchors() {
        let words = (0..800).map(|n| format!("w{n}")).collect::<Vec<_>>();
        let line = words.join(" ");
        let text = page(&[&line]);
        let chunks = chunk_page(&text, 3, 1_000, "the end of page two", "Results").chunks;
        assert!(chunks.len() >= 2, "{}", chunks.len());
        for chunk in &chunks {
            assert!(chunk.text.chars().count() <= CHUNK_CHARS);
            assert!(
                chunk.text.starts_with('w'),
                "starts at a word: {}",
                &chunk.text[..10]
            );
            assert_eq!(chunk.page, 3);
            assert_eq!(chunk.section, "Results");
        }
        // The second starts inside the first: they overlap.
        assert!(chunks[1].glyph_start < chunks[0].glyph_end);
        assert_eq!(chunks[0].doc_offset, 1_000);
        assert_eq!(chunks[0].quote, line.chars().take(32).collect::<String>());
        assert_eq!(
            chunks[0].prefix, "the end of page two",
            "carried from before"
        );
        assert_eq!(
            chunks[1].prefix.chars().count(),
            24,
            "on the page when there is room"
        );
        assert!(!chunks[0].rects.is_empty());
    }

    #[test]
    fn a_page_with_almost_no_text_is_marked() {
        let chunks = chunk_page(&page(&["Fig. 3"]), 0, 0, "", "");
        assert!(chunks.image_only);
        assert_eq!(chunks.chunks.len(), 1, "what text there is still counts");
        let blank = chunk_page(&PageText::default(), 0, 0, "", "");
        assert!(blank.image_only);
        assert!(blank.chunks.is_empty());
    }
}
