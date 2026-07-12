use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result};
use glyphst_core::{DocumentCapabilities, DocumentFormat, DocumentLayout, EpubLayout};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocumentSnapshot {
    path: PathBuf,
    bytes: Vec<u8>,
    format: DocumentFormat,
}

impl DocumentSnapshot {
    pub fn read(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let format = document_format(path)?;
        let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
        Ok(Self {
            path: path.to_path_buf(),
            bytes,
            format,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn format(&self) -> DocumentFormat {
        self.format
    }

    pub fn open(&self) -> Result<GlyphstDocument> {
        self.open_with_layout(DocumentLayout::Fixed)
    }

    pub fn open_with_layout(&self, layout: DocumentLayout) -> Result<GlyphstDocument> {
        GlyphstDocument::from_snapshot(self, layout)
    }
}

#[derive(Debug)]
pub struct GlyphstDocument {
    inner: mupdf::Document,
    format: DocumentFormat,
    reflowable: bool,
    layout: DocumentLayout,
}

impl GlyphstDocument {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let snapshot = DocumentSnapshot::read(path)?;
        snapshot.open()
    }

    pub fn from_snapshot(snapshot: &DocumentSnapshot, layout: DocumentLayout) -> Result<Self> {
        configure_context(snapshot.format(), layout)?;
        let mut inner =
            mupdf::Document::from_bytes(snapshot.bytes(), snapshot.format().mime_type())
                .with_context(|| format!("failed to open {}", snapshot.path().display()))?;
        let reflowable = inner.is_reflowable().with_context(|| {
            format!(
                "failed to inspect document layout for {}",
                snapshot.path().display()
            )
        })?;
        let layout = match (layout, reflowable) {
            (DocumentLayout::Reflow(layout), true) => {
                let layout = layout.sanitized();
                inner
                    .layout(layout.width, layout.height, layout.font_size)
                    .with_context(|| format!("failed to layout {}", snapshot.path().display()))?;
                DocumentLayout::Reflow(layout)
            }
            _ => DocumentLayout::Fixed,
        };
        Ok(Self {
            inner,
            format: snapshot.format(),
            reflowable,
            layout,
        })
    }

    pub fn inner(&self) -> &mupdf::Document {
        &self.inner
    }

    pub fn into_inner(self) -> mupdf::Document {
        self.inner
    }

    pub fn format(&self) -> DocumentFormat {
        self.format
    }

    pub fn is_reflowable(&self) -> bool {
        self.reflowable
    }

    pub fn layout(&self) -> DocumentLayout {
        self.layout
    }

    pub fn capabilities(&self) -> DocumentCapabilities {
        DocumentCapabilities::for_document(self.format, self.reflowable)
    }
}

pub type PdfDocument = GlyphstDocument;

fn document_format(path: &Path) -> Result<DocumentFormat> {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .with_context(|| format!("unsupported document type: {}", path.display()))?;
    DocumentFormat::from_extension(extension)
        .with_context(|| format!("unsupported document type: {}", path.display()))
}

fn configure_context(format: DocumentFormat, layout: DocumentLayout) -> Result<()> {
    let mut context = mupdf::Context::get();
    match (format, layout) {
        (DocumentFormat::Epub, DocumentLayout::Reflow(layout)) => {
            let layout: EpubLayout = layout.sanitized();
            context.set_use_document_css(layout.use_publisher_styles);
            context
                .set_user_css(&layout.user_css())
                .context("failed to configure EPUB reader styles")?;
        }
        _ => {
            context.set_use_document_css(true);
            context
                .set_user_css("")
                .context("failed to reset document styles")?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    fn temp_path(extension: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("glyphst-document-{nonce}.{extension}"))
    }

    #[test]
    fn snapshot_detects_format_case_insensitively() {
        let path = temp_path("EPUB");
        fs::write(&path, b"not-an-epub").unwrap();
        let snapshot = DocumentSnapshot::read(&path).unwrap();
        assert_eq!(snapshot.format(), DocumentFormat::Epub);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn snapshot_rejects_unsupported_extensions() {
        let path = temp_path("txt");
        fs::write(&path, b"text").unwrap();
        let error = DocumentSnapshot::read(&path).unwrap_err();
        assert!(error.to_string().contains("unsupported document type"));
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn opens_and_reflows_epub_fixture() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.epub");
        let snapshot = DocumentSnapshot::read(path).unwrap();
        let narrow = snapshot
            .open_with_layout(DocumentLayout::Reflow(EpubLayout {
                width: 320.0,
                height: 400.0,
                ..EpubLayout::default()
            }))
            .unwrap();
        assert_eq!(narrow.format(), DocumentFormat::Epub);
        assert!(narrow.is_reflowable());
        assert!(narrow.capabilities().publisher_styles);
        assert_eq!(
            narrow.inner().metadata(mupdf::MetadataName::Title).unwrap(),
            "Glyphst EPUB Sample"
        );
        let narrow_pages = narrow.inner().page_count().unwrap();
        assert!(narrow_pages > 0);
        let page = narrow.inner().load_page(0).unwrap();
        let bounds = page.bounds().unwrap();
        let display_list = mupdf::DisplayList::new(bounds).unwrap();
        let list_device = mupdf::Device::from_display_list(&display_list).unwrap();
        page.run(&list_device, &mupdf::Matrix::IDENTITY).unwrap();
        let mut pixmap = mupdf::Pixmap::new_with_w_h(
            &mupdf::Colorspace::device_rgb(),
            bounds.width().ceil() as i32,
            bounds.height().ceil() as i32,
            true,
        )
        .unwrap();
        pixmap.samples_mut().fill(255);
        let pixmap_device = mupdf::Device::from_pixmap(&pixmap).unwrap();
        display_list
            .run(&pixmap_device, &mupdf::Matrix::IDENTITY, mupdf::Rect::INF)
            .unwrap();
        assert!(
            pixmap
                .samples()
                .chunks_exact(4)
                .any(|pixel| pixel[0] < 240 || pixel[1] < 240 || pixel[2] < 240)
        );

        let wide = snapshot
            .open_with_layout(DocumentLayout::Reflow(EpubLayout {
                width: 720.0,
                height: 960.0,
                ..EpubLayout::default()
            }))
            .unwrap();
        assert!(narrow_pages >= wide.inner().page_count().unwrap());
        assert!(!wide.inner().outlines().unwrap().is_empty());
    }
}
