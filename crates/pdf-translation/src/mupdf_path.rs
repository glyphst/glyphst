//! Paths as MuPDF takes them: their bytes on Unix, UTF-8 on Windows.

use std::path::Path;

#[cfg(unix)]
pub fn mupdf_path(path: &Path) -> Result<&Path, mupdf::Error> {
    Ok(path)
}

#[cfg(not(unix))]
pub fn mupdf_path(path: &Path) -> Result<&str, mupdf::Error> {
    path.to_str().ok_or(mupdf::Error::InvalidUtf8)
}
