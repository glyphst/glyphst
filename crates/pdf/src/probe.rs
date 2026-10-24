//! What a document says about itself, for the library to recognise it:
//! its info dictionary (or an EPUB's package metadata) and the text of its
//! first pages, where a paper prints its DOI or arXiv id and a book its
//! ISBN. Runs on a worker thread.

use std::{collections::HashMap, path::Path};

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
        // Character by character, not `to_text`: one glyph mapped to a code
        // point that isn't text (half a surrogate pair) would make the whole
        // page's text unreadable.
        let mut text = String::new();
        if let Ok(page) = inner
            .load_page(index as i32)
            .and_then(|page| page.to_text_page(TextPageFlags::empty()))
        {
            for block in page.blocks() {
                for line in block.lines() {
                    text.extend(line.chars().filter_map(|character| character.char()));
                    text.push('\n');
                }
                text.push('\n');
            }
        }
        page_text.push(text);
    }
    Ok(DocumentProbe {
        info,
        page_text,
        page_count,
    })
}

/// A page's crop box in PDF user space (origin at the bottom left) and its
/// rotation, for placing what other tools saved in user space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PageBox {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
    /// `/Rotate`, in degrees.
    pub rotate: i32,
}

/// The boxes of `pages` (from 0) of the PDF at `path`; pages that cannot be
/// read are left out.
pub fn page_boxes(path: &Path, pages: &[usize]) -> Result<HashMap<usize, PageBox>> {
    let document = GlyphstDocument::open(path)?;
    let mut boxes = HashMap::new();
    for &index in pages {
        if boxes.contains_key(&index) {
            continue;
        }
        let Ok(page) = document.inner().load_page(index as i32) else {
            continue;
        };
        let Ok(page) = mupdf::pdf::PdfPage::try_from(page) else {
            continue;
        };
        let object = page.object();
        let rect = ["CropBox", "MediaBox"].into_iter().find_map(|key| {
            let array = object.get_dict_inheritable(key).ok()??;
            let at = |i: i32| {
                array
                    .get_array(i)
                    .ok()?
                    .and_then(|value| value.as_float().ok())
            };
            Some([at(0)?, at(1)?, at(2)?, at(3)?])
        });
        let Some([ax, ay, bx, by]) = rect else {
            continue;
        };
        let rotate = object
            .get_dict_inheritable("Rotate")
            .ok()
            .flatten()
            .and_then(|value| value.as_int().ok())
            .unwrap_or(0)
            .rem_euclid(360);
        boxes.insert(
            index,
            PageBox {
                x0: ax.min(bx),
                y0: ay.min(by),
                x1: ax.max(bx),
                y1: ay.max(by),
                rotate,
            },
        );
    }
    Ok(boxes)
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
    fn a_page_box_comes_from_the_crop_box_else_the_media_box() {
        let dir = std::env::temp_dir();
        let plain = dir.join(format!("glyphst-box-{}.pdf", std::process::id()));
        let cropped = dir.join(format!("glyphst-box-crop-{}.pdf", std::process::id()));
        std::fs::write(&plain, crate::test_pdf::pdf_with("", "<< >>", "", &[])).unwrap();
        std::fs::write(
            &cropped,
            crate::test_pdf::pdf_with("", "<< >>", "/CropBox [10 20 190 90] /Rotate 90", &[]),
        )
        .unwrap();
        let plain_box = page_boxes(&plain, &[0, 0, 7]).unwrap();
        let cropped_box = page_boxes(&cropped, &[0]).unwrap();
        let _ = std::fs::remove_file(&plain);
        let _ = std::fs::remove_file(&cropped);
        assert_eq!(
            plain_box.get(&0),
            Some(&PageBox {
                x0: 0.0,
                y0: 0.0,
                x1: 200.0,
                y1: 100.0,
                rotate: 0
            })
        );
        assert_eq!(plain_box.len(), 1, "a page past the end is left out");
        assert_eq!(
            cropped_box.get(&0),
            Some(&PageBox {
                x0: 10.0,
                y0: 20.0,
                x1: 190.0,
                y1: 90.0,
                rotate: 90
            })
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
