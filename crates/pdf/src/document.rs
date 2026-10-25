use std::{
    error::Error,
    fmt, fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use anyhow::{Context as _, Result, bail};
use glyphst_core::{DocumentCapabilities, DocumentFormat, DocumentLayout, EpubLayout};
use mupdf::pdf::{Encryption, PdfDocument as MuPdfDocument, PdfWriteOptions};
use zeroize::Zeroizing;

#[derive(Clone, PartialEq, Eq)]
pub struct DocumentPassword(Arc<Zeroizing<String>>);

impl DocumentPassword {
    pub fn new(password: impl Into<String>) -> Self {
        Self(Arc::new(Zeroizing::new(password.into())))
    }

    pub fn expose_secret(&self) -> &str {
        self.0.as_str()
    }
}

impl fmt::Debug for DocumentPassword {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("DocumentPassword([REDACTED])")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DocumentAuthenticationError {
    PasswordRequired,
    InvalidPassword,
}

impl fmt::Display for DocumentAuthenticationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PasswordRequired => formatter.write_str("document password required"),
            Self::InvalidPassword => formatter.write_str("document password is invalid"),
        }
    }
}

impl Error for DocumentAuthenticationError {}

#[derive(Clone, PartialEq, Eq)]
pub struct DocumentSnapshot {
    path: PathBuf,
    bytes: Arc<[u8]>,
    format: DocumentFormat,
    password: Option<DocumentPassword>,
}

impl fmt::Debug for DocumentSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DocumentSnapshot")
            .field("path", &self.path)
            .field("bytes", &format_args!("{} bytes", self.bytes.len()))
            .field("format", &self.format)
            .field("password", &self.password.as_ref().map(|_| "[REDACTED]"))
            .finish()
    }
}

impl DocumentSnapshot {
    #[tracing::instrument(skip_all)]
    pub fn read(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let format = document_format(path)?;
        let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
        Ok(Self {
            path: path.to_path_buf(),
            bytes: Arc::from(bytes),
            format,
            password: None,
        })
    }

    /// Construct an immutable render snapshot from in-memory document bytes.
    /// `path` is a diagnostic/name hint only; opening never reads it again.
    pub fn from_bytes(
        path: impl Into<PathBuf>,
        format: DocumentFormat,
        bytes: impl Into<Arc<[u8]>>,
    ) -> Self {
        Self {
            path: path.into(),
            bytes: bytes.into(),
            format,
            password: None,
        }
    }

    pub fn from_pdf_bytes(path: impl Into<PathBuf>, bytes: impl Into<Arc<[u8]>>) -> Self {
        Self::from_bytes(path, DocumentFormat::Pdf, bytes)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Clone the immutable source allocation without copying its bytes.
    pub fn shared_bytes(&self) -> Arc<[u8]> {
        Arc::clone(&self.bytes)
    }

    pub fn format(&self) -> DocumentFormat {
        self.format
    }

    pub fn with_password(mut self, password: DocumentPassword) -> Self {
        self.password = Some(password);
        self
    }

    pub fn password(&self) -> Option<&DocumentPassword> {
        self.password.as_ref()
    }

    /// Return PDF bytes that can be reopened without a password.
    ///
    /// Unencrypted snapshots reuse their immutable source bytes. Encrypted
    /// snapshots are authenticated with the in-memory password and rewritten
    /// without an encryption dictionary.
    #[tracing::instrument(skip_all)]
    pub fn unencrypted_pdf_bytes(&self) -> Result<Arc<[u8]>> {
        if self.format != DocumentFormat::Pdf {
            bail!("{} is not a PDF document", self.path.display());
        }

        let mut document = MuPdfDocument::from_bytes(self.bytes())
            .with_context(|| format!("failed to open {}", self.path.display()))?;
        let password_protected = authenticate_snapshot_document(self, &mut document)?;
        if !password_protected {
            return Ok(self.bytes.clone());
        }

        let mut options = PdfWriteOptions::default();
        options.set_encryption(Encryption::None);
        let mut bytes = Vec::new();
        document
            .write_to_with_options(&mut bytes, options)
            .with_context(|| {
                format!(
                    "failed to create an unencrypted copy of {}",
                    self.path.display()
                )
            })?;
        validate_unencrypted_pdf_bytes(&bytes, &self.path)?;
        Ok(Arc::from(bytes))
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
    password_protected: bool,
    reflowable: bool,
    layout: DocumentLayout,
}

impl GlyphstDocument {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let snapshot = DocumentSnapshot::read(path)?;
        snapshot.open()
    }

    #[tracing::instrument(skip_all)]
    pub fn from_snapshot(snapshot: &DocumentSnapshot, layout: DocumentLayout) -> Result<Self> {
        configure_context(snapshot.format(), layout)?;
        let mut inner =
            mupdf::Document::from_bytes(snapshot.bytes(), snapshot.format().mime_type())
                .with_context(|| format!("failed to open {}", snapshot.path().display()))?;
        let password_protected = authenticate_snapshot_document(snapshot, &mut inner)?;
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
            password_protected,
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

    pub fn is_password_protected(&self) -> bool {
        self.password_protected
    }

    pub fn layout(&self) -> DocumentLayout {
        self.layout
    }

    pub fn capabilities(&self) -> DocumentCapabilities {
        DocumentCapabilities::for_document(self.format, self.reflowable)
    }
}

pub type PdfDocument = GlyphstDocument;

fn authenticate_snapshot_document(
    snapshot: &DocumentSnapshot,
    document: &mut mupdf::Document,
) -> Result<bool> {
    let password_protected = document.needs_password().with_context(|| {
        format!(
            "failed to inspect document security for {}",
            snapshot.path().display()
        )
    })?;
    if !password_protected {
        return Ok(false);
    }

    let (password, authentication_error) = match snapshot.password() {
        Some(password) => (
            password.expose_secret(),
            DocumentAuthenticationError::InvalidPassword,
        ),
        None => ("", DocumentAuthenticationError::PasswordRequired),
    };
    let authenticated = document.authenticate(password).with_context(|| {
        format!(
            "failed to authenticate document {}",
            snapshot.path().display()
        )
    })?;
    if !authenticated {
        return Err(authentication_error.into());
    }
    Ok(true)
}

fn validate_unencrypted_pdf_bytes(bytes: &[u8], source_path: &Path) -> Result<()> {
    let document = MuPdfDocument::from_bytes(bytes).with_context(|| {
        format!(
            "failed to validate the unencrypted copy of {}",
            source_path.display()
        )
    })?;
    if document.needs_password().with_context(|| {
        format!(
            "failed to inspect the unencrypted copy of {}",
            source_path.display()
        )
    })? {
        bail!(
            "the generated copy of {} still requires a password",
            source_path.display()
        );
    }
    let trailer = document.trailer().with_context(|| {
        format!(
            "failed to inspect the generated copy of {}",
            source_path.display()
        )
    })?;
    if trailer
        .get_dict("Encrypt")
        .with_context(|| {
            format!(
                "failed to inspect the encryption dictionary of {}",
                source_path.display()
            )
        })?
        .is_some()
    {
        bail!(
            "the generated copy of {} still contains an encryption dictionary",
            source_path.display()
        );
    }
    Ok(())
}

fn document_format(path: &Path) -> Result<DocumentFormat> {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .with_context(|| format!("unsupported document type: {}", path.display()))?;
    DocumentFormat::from_extension(extension)
        .with_context(|| format!("unsupported document type: {}", path.display()))
}

// Cloned MuPDF contexts share both stylesheet settings. Serialize configuration
// and avoid rewriting an unchanged flag while other PDF threads consume it.
static STYLE_CONFIGURATION_LOCK: Mutex<()> = Mutex::new(());

/// Held by tests that change the shared stylesheet, or that open a document
/// other than a PDF (which sets it): PDF tests run alongside them and must
/// preserve its allocation.
#[cfg(test)]
pub(crate) static EPUB_STYLE_TEST_LOCK: Mutex<()> = Mutex::new(());

fn configure_context(format: DocumentFormat, layout: DocumentLayout) -> Result<()> {
    let _configuration = STYLE_CONFIGURATION_LOCK
        .lock()
        .map_err(|_| anyhow::anyhow!("MuPDF style configuration lock poisoned"))?;
    let mut context = mupdf::Context::get();
    // PDF rich-text appearances need document styles, but use their own CSS.
    // Replacing the shared user stylesheet here can free an allocation being
    // consumed by EPUB layout. EPUB settings still have process-wide ownership.
    if format == DocumentFormat::Pdf {
        if !context.use_document_css() {
            context.set_use_document_css(true);
        }
        return Ok(());
    }
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

    use base64::{Engine as _, engine::general_purpose::STANDARD};

    use super::*;

    fn encrypted_pdf_bytes() -> Vec<u8> {
        let encoded = include_str!("../tests/fixtures/dummy-encrypted.pdf.b64")
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect::<String>();
        STANDARD.decode(encoded).unwrap()
    }

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
    fn snapshot_accepts_revisioned_in_memory_pdf_bytes() {
        let bytes: Arc<[u8]> = Arc::from(&b"%PDF-1.7\n% preview revision"[..]);
        let snapshot = DocumentSnapshot::from_pdf_bytes("page-1-revision-7.pdf", bytes.clone());

        assert_eq!(snapshot.format(), DocumentFormat::Pdf);
        assert_eq!(snapshot.path(), Path::new("page-1-revision-7.pdf"));
        assert_eq!(snapshot.bytes(), bytes.as_ref());
    }

    #[test]
    fn encrypted_snapshot_reports_password_required_and_invalid_password() {
        let snapshot = DocumentSnapshot::from_pdf_bytes("encrypted.pdf", encrypted_pdf_bytes());
        let missing = snapshot.open().unwrap_err();
        assert_eq!(
            missing.downcast_ref::<DocumentAuthenticationError>(),
            Some(&DocumentAuthenticationError::PasswordRequired)
        );

        let invalid = snapshot
            .clone()
            .with_password(DocumentPassword::new("wrong-password"))
            .open()
            .unwrap_err();
        assert_eq!(
            invalid.downcast_ref::<DocumentAuthenticationError>(),
            Some(&DocumentAuthenticationError::InvalidPassword)
        );
    }

    #[test]
    fn encrypted_snapshot_opens_with_password_without_exposing_it_in_debug_output() {
        let password = DocumentPassword::new("123456");
        assert_eq!(format!("{password:?}"), "DocumentPassword([REDACTED])");

        let snapshot = DocumentSnapshot::from_pdf_bytes("encrypted.pdf", encrypted_pdf_bytes())
            .with_password(password);
        let debug = format!("{snapshot:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains("123456"));

        let document = snapshot.open().unwrap();
        assert!(document.is_password_protected());
        assert_eq!(document.inner().page_count().unwrap(), 1);
    }

    #[test]
    fn encrypted_snapshot_produces_an_unencrypted_pdf_copy() {
        let snapshot = DocumentSnapshot::from_pdf_bytes("encrypted.pdf", encrypted_pdf_bytes())
            .with_password(DocumentPassword::new("123456"));

        let bytes = snapshot.unencrypted_pdf_bytes().unwrap();
        let document = MuPdfDocument::from_bytes(&bytes).unwrap();
        assert!(!document.needs_password().unwrap());
        assert!(
            document
                .trailer()
                .unwrap()
                .get_dict("Encrypt")
                .unwrap()
                .is_none()
        );
        assert_eq!(document.page_count().unwrap(), 1);
    }

    #[test]
    fn concurrent_pdf_opens_preserve_user_css_and_enable_pdf_rich_text_styles() {
        let _styles = EPUB_STYLE_TEST_LOCK.lock().unwrap();
        let mut context = mupdf::Context::get();
        let css = "body { font-size: 19pt; }";
        {
            let _configuration = STYLE_CONFIGURATION_LOCK.lock().unwrap();
            context.set_user_css(css).unwrap();
            context.set_use_document_css(false);
        }
        let snapshot = DocumentSnapshot::from_pdf_bytes("encrypted.pdf", encrypted_pdf_bytes())
            .with_password(DocumentPassword::new("123456"));

        assert_eq!(snapshot.open().unwrap().inner().page_count().unwrap(), 1);
        assert_eq!(context.user_css().as_deref(), Some(css));
        assert!(context.use_document_css());

        std::thread::scope(|scope| {
            for _ in 0..8 {
                let snapshot = &snapshot;
                scope.spawn(move || {
                    for _ in 0..16 {
                        let document = snapshot.open().unwrap();
                        assert_eq!(document.inner().page_count().unwrap(), 1);
                    }
                });
            }
        });
        assert_eq!(context.user_css().as_deref(), Some(css));
        assert!(context.use_document_css());
    }

    #[test]
    fn opens_and_reflows_epub_fixture() {
        let _styles = EPUB_STYLE_TEST_LOCK.lock().unwrap();
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
        let mut display_list = mupdf::DisplayList::new(bounds).unwrap();
        let list_device = mupdf::Device::from_display_list(&mut display_list).unwrap();
        page.run(&list_device, &mupdf::Matrix::IDENTITY).unwrap();
        drop(list_device);
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
                .as_chunks::<4>()
                .0
                .iter()
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
