//! Native file pickers that never block a frame, following pdfcraft's
//! `pickers.rs`: an `rfd::AsyncFileDialog` is awaited on a worker thread
//! and the chosen paths are taken on a later frame. (A blocking dialog runs
//! a nested event loop inside winit's handler, which crashes on macOS.)

use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

/// Extensions MuPDF opens that the reader offers in the picker.
pub const DOCUMENT_EXTENSIONS: [&str; 7] = ["pdf", "epub", "xps", "oxps", "cbz", "fb2", "mobi"];

#[derive(Clone, Default)]
pub struct Pickers {
    showing: Arc<AtomicBool>,
    picked: Arc<Mutex<Vec<PathBuf>>>,
}

impl Pickers {
    /// Shows the open dialog unless one is already showing.
    pub fn open_documents(&self, ctx: &egui::Context) {
        if self.showing.swap(true, Ordering::SeqCst) {
            return;
        }
        let dialog = rfd::AsyncFileDialog::new()
            .set_title("Open documents")
            .add_filter("Documents", &DOCUMENT_EXTENSIONS)
            .pick_files();
        let (showing, picked, ctx) = (self.showing.clone(), self.picked.clone(), ctx.clone());
        let spawned = std::thread::Builder::new()
            .name("open-dialog".into())
            .spawn(move || {
                let files = pollster::block_on(dialog).unwrap_or_default();
                if let Ok(mut picked) = picked.lock() {
                    picked.extend(files.into_iter().map(|file| file.path().to_path_buf()));
                }
                showing.store(false, Ordering::SeqCst);
                ctx.request_repaint();
            });
        if spawned.is_err() {
            self.showing.store(false, Ordering::SeqCst);
        }
    }

    pub fn is_showing(&self) -> bool {
        self.showing.load(Ordering::SeqCst)
    }

    /// Paths chosen since the last call.
    pub fn take(&self) -> Vec<PathBuf> {
        self.picked
            .lock()
            .map(|mut picked| std::mem::take(&mut *picked))
            .unwrap_or_default()
    }
}

/// Whether `path` looks like a document the reader opens.
pub fn is_document(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            DOCUMENT_EXTENSIONS
                .iter()
                .any(|known| known.eq_ignore_ascii_case(extension))
        })
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn documents_are_recognised_by_extension() {
        assert!(is_document(Path::new("/a/b.PDF")));
        assert!(is_document(Path::new("book.epub")));
        assert!(!is_document(Path::new("notes.txt")));
        assert!(!is_document(Path::new("README")));
        assert!(Pickers::default().take().is_empty());
    }
}
