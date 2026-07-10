use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocumentSnapshot {
    path: PathBuf,
    bytes: Vec<u8>,
}

impl DocumentSnapshot {
    pub fn read(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
        Ok(Self {
            path: path.to_path_buf(),
            bytes,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn open(&self) -> Result<PdfDocument> {
        PdfDocument::from_snapshot(self)
    }
}

#[derive(Debug)]
pub struct PdfDocument {
    inner: mupdf::Document,
}

impl PdfDocument {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let inner = mupdf::Document::open(path)
            .with_context(|| format!("failed to open {}", path.display()))?;
        Ok(Self { inner })
    }

    pub fn from_snapshot(snapshot: &DocumentSnapshot) -> Result<Self> {
        let inner = mupdf::Document::from_bytes(snapshot.bytes(), "application/pdf")
            .with_context(|| format!("failed to open {}", snapshot.path().display()))?;
        Ok(Self { inner })
    }

    pub fn inner(&self) -> &mupdf::Document {
        &self.inner
    }

    pub fn into_inner(self) -> mupdf::Document {
        self.inner
    }
}
