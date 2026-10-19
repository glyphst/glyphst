//! Reading a document's pages for the index. MuPDF documents stay on the
//! thread that opened them, so the index's thread opens its own. An EPUB is
//! laid out once in the default layout; its chunks carry anchors that the
//! Reader finds again in whatever layout it shows.

use std::path::Path;

use anyhow::{Context as _, Result};
use glyphst_core::{DocumentFormat, DocumentLayout, EpubLayout};
use glyphst_pdf::{DocumentSnapshot, GlyphstDocument, text::PageText};

/// Bumped when extraction changes what it gives, so documents are read again.
pub const EXTRACTOR_VERSION: i64 = 1;

pub struct DocReader {
    document: GlyphstDocument,
    pub format: DocumentFormat,
    pub page_count: usize,
    /// Outline entries that name a page, in reading order.
    sections: Vec<(usize, String)>,
}

impl DocReader {
    pub fn open(path: &Path) -> Result<Self> {
        let snapshot = DocumentSnapshot::read(path)?;
        let format = snapshot.format();
        let layout = match format {
            DocumentFormat::Epub => DocumentLayout::Reflow(EpubLayout::default()),
            DocumentFormat::Pdf => DocumentLayout::Fixed,
        };
        let document = snapshot.open_with_layout(layout)?;
        let page_count = document
            .inner()
            .page_count()
            .context("failed to count pages")? as usize;
        let mut sections = Vec::new();
        // A broken outline does not stop indexing.
        if let Ok(entries) = document.inner().outlines() {
            flatten(&entries, page_count, &mut sections);
        }
        sections.sort_by_key(|(page, _)| *page);
        Ok(Self {
            document,
            format,
            page_count,
            sections,
        })
    }

    pub fn page(&self, index: usize) -> Result<PageText> {
        let page = self
            .document
            .inner()
            .load_page(index as i32)
            .with_context(|| format!("failed to load page {}", index + 1))?;
        PageText::from_page(&page)
    }

    /// The title of the section `page` is in, from the outline.
    pub fn section(&self, page: usize) -> &str {
        self.sections
            .iter()
            .rev()
            .find(|(start, _)| *start <= page)
            .map_or("", |(_, title)| title.as_str())
    }
}

fn flatten(entries: &[mupdf::Outline], pages: usize, out: &mut Vec<(usize, String)>) {
    for entry in entries {
        let title = entry.title.trim();
        if let Some(dest) = &entry.dest {
            let page = dest.loc.page_number as usize;
            if page < pages && !title.is_empty() {
                out.push((page, title.to_string()));
            }
        }
        flatten(&entry.down, pages, out);
    }
}

pub fn format_name(format: DocumentFormat) -> &'static str {
    match format {
        DocumentFormat::Pdf => "pdf",
        DocumentFormat::Epub => "epub",
    }
}
