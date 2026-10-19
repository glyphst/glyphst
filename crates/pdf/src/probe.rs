//! What a document says about itself, for the library to recognise it:
//! its info dictionary (or an EPUB's package metadata) and the text of its
//! first pages, where a paper prints its DOI or arXiv id and a book its
//! ISBN. Runs on a worker thread.

use std::path::Path;

use anyhow::{Context as _, Result};
use mupdf::{MetadataName, TextPageFlags};

use crate::GlyphstDocument;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DocumentProbe {
    /// The info entries that have a value, as `(key, value)`: `title`,
    /// `author`, `subject`, `keywords`, `creator`, `producer`, `created`,
    /// `modified`.
    pub info: Vec<(&'static str, String)>,
    /// The plain text of the first pages, one string a page.
    pub page_text: Vec<String>,
    pub page_count: usize,
}

impl DocumentProbe {
    pub fn info(&self, key: &str) -> Option<&str> {
        self.info
            .iter()
            .find(|(candidate, _)| *candidate == key)
            .map(|(_, value)| value.as_str())
    }
}

/// Reads the info and the text of up to `pages` first pages of the
/// document at `path` (an EPUB at MuPDF's default layout).
pub fn probe_document(path: &Path, pages: usize) -> Result<DocumentProbe> {
    let document = GlyphstDocument::open(path)?;
    let inner = document.inner();
    let info = [
        ("title", MetadataName::Title),
        ("author", MetadataName::Author),
        ("subject", MetadataName::Subject),
        ("keywords", MetadataName::Keywords),
        ("creator", MetadataName::Creator),
        ("producer", MetadataName::Producer),
        ("created", MetadataName::CreationDate),
        ("modified", MetadataName::ModDate),
    ]
    .into_iter()
    .filter_map(|(key, name)| {
        let value = inner.metadata(name).ok()?.trim().to_string();
        (!value.is_empty()).then_some((key, value))
    })
    .collect();
    let page_count = usize::try_from(
        inner
            .page_count()
            .with_context(|| format!("failed to count the pages of {}", path.display()))?,
    )
    .unwrap_or(0);
    let mut page_text = Vec::new();
    for index in 0..page_count.min(pages) {
        let text = inner
            .load_page(index as i32)
            .and_then(|page| page.to_text_page(TextPageFlags::empty()))
            .and_then(|text| text.to_text())
            .unwrap_or_default();
        page_text.push(text);
    }
    Ok(DocumentProbe {
        info,
        page_text,
        page_count,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_pages_text_is_read() {
        let path = std::env::temp_dir().join(format!("glyphst-probe-{}.pdf", std::process::id()));
        std::fs::write(
            &path,
            crate::test_pdf::text_pdf(&["Attention Is All You Need", "arXiv:1706.03762v7"]),
        )
        .unwrap();
        let probe = probe_document(&path, 3).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(probe.page_count, 1);
        assert_eq!(probe.page_text.len(), 1);
        assert!(
            probe.page_text[0].contains("arXiv:1706.03762v7"),
            "{:?}",
            probe.page_text
        );
    }

    #[test]
    fn an_epub_gives_its_package_title_and_author() {
        let path =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.epub");
        let probe = probe_document(&path, 2).unwrap();
        assert_eq!(probe.info("title"), Some("Glyphst EPUB Sample"));
        assert_eq!(probe.info("author"), Some("Glyphst"));
        assert!(probe.page_count >= 1);
        assert!(!probe.page_text.is_empty());
    }
}
