//! Bringing files into the library (frame 04g): dropped files and folders,
//! and a document as it is opened. A worker thread walks the folders and
//! copies (or hashes) each file; the UI thread adds them to the catalog a
//! few a frame, filing a folder's files into collections named after its
//! subfolders, and keeps count for the import strip.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{Receiver, channel},
    },
};

use glyphst_core::DocumentFormat;
use glyphst_library::{
    DocumentId, LibraryCollectionId, LibraryImportOutcome, LibraryItemMetadata, LibraryStore,
    PreparedLinkedImport, PreparedManagedImport, prepare_linked_import, prepare_managed_import,
};

use crate::prefs::ImportMode;

/// Files added to the catalog per frame, so a large import keeps the
/// window responsive.
const PER_FRAME: usize = 8;
/// How deep a dropped folder is walked.
const MAX_DEPTH: usize = 8;

/// One file the worker made ready.
enum Prepared {
    Managed(PreparedManagedImport),
    Linked(PreparedLinkedImport),
}

enum Event {
    /// The walk found this many files.
    Found(usize),
    Ready {
        source: PathBuf,
        prepared: Prepared,
        /// The collection path under the import's target, from the folders.
        folders: Vec<String>,
    },
    Failed {
        source: PathBuf,
        error: String,
    },
    Done,
}

/// An import's tally, for the strip over the library table.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImportProgress {
    pub total: usize,
    pub processed: usize,
    pub added: Vec<DocumentId>,
    /// The file and the item it duplicates.
    pub duplicates: Vec<(PathBuf, DocumentId)>,
    pub failed: Vec<(PathBuf, String)>,
    pub mode: Option<ImportMode>,
    /// The folder or file named in the strip.
    pub source: String,
    pub finished: bool,
    /// Added without a strip (a document as it is opened).
    pub quiet: bool,
}

struct Running {
    events: Receiver<Event>,
    cancel: Arc<AtomicBool>,
    target: Option<LibraryCollectionId>,
}

/// The import under way, if any, and the last one's tally.
#[derive(Default)]
pub struct Importer {
    running: Vec<Running>,
    progress: Option<ImportProgress>,
    /// Items added since the caller last took them, to queue their
    /// metadata and AI passes.
    added: Vec<DocumentId>,
    /// Each file brought in and the item it is now (added, or already
    /// there), so what was kept about the file follows its copy.
    adopted: Vec<(PathBuf, DocumentId)>,
}

impl Importer {
    /// Starts bringing `paths` (files and folders) into the library under
    /// `target`. Folders become collections. `quiet` adds without the
    /// import strip.
    pub fn start(
        &mut self,
        library: &LibraryStore,
        paths: Vec<PathBuf>,
        mode: ImportMode,
        target: Option<LibraryCollectionId>,
        quiet: bool,
        ctx: &egui::Context,
    ) {
        let root = library.managed_library_root().map(Path::to_path_buf);
        // Without a managed folder (a memory library) files are linked.
        let mode = if root.is_none() {
            ImportMode::Link
        } else {
            mode
        };
        let source = match paths.as_slice() {
            [one] => one
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| one.display().to_string()),
            many => format!("{} items", many.len()),
        };
        let (sender, events) = channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancel.clone();
        let repaint = ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("library-import".into())
            .spawn(move || {
                let files = walk(&paths);
                if sender.send(Event::Found(files.len())).is_err() {
                    return;
                }
                for (file, folders) in files {
                    if worker_cancel.load(Ordering::Acquire) {
                        break;
                    }
                    let prepared = match (mode, &root) {
                        (ImportMode::Copy, Some(root)) => {
                            prepare_managed_import(root, &file, &worker_cancel)
                                .map(Prepared::Managed)
                        }
                        _ => prepare_linked_import(&file, &worker_cancel).map(Prepared::Linked),
                    };
                    let event = match prepared {
                        Ok(prepared) => Event::Ready {
                            source: file,
                            prepared,
                            folders,
                        },
                        Err(error) => Event::Failed {
                            source: file,
                            error: format!("{error:#}"),
                        },
                    };
                    if sender.send(event).is_err() {
                        return;
                    }
                    repaint.request_repaint();
                }
                let _ = sender.send(Event::Done);
                repaint.request_repaint();
            });
        if let Err(error) = spawned {
            log::warn!("no import thread: {error}");
            return;
        }
        if !quiet
            || self
                .progress
                .as_ref()
                .is_none_or(|progress| progress.finished)
        {
            self.progress = Some(ImportProgress {
                mode: Some(mode),
                source,
                quiet,
                ..ImportProgress::default()
            });
        }
        self.running.push(Running {
            events,
            cancel,
            target,
        });
    }

    /// Adds what the worker made ready since the last frame.
    pub fn poll(&mut self, library: &mut LibraryStore) {
        let mut budget = PER_FRAME;
        let mut finished = Vec::new();
        for (index, running) in self.running.iter().enumerate() {
            while budget > 0 {
                let Ok(event) = running.events.try_recv() else {
                    break;
                };
                let progress = self.progress.get_or_insert_with(ImportProgress::default);
                match event {
                    Event::Found(count) => progress.total += count,
                    Event::Ready {
                        source,
                        prepared,
                        folders,
                    } => {
                        budget -= 1;
                        progress.processed += 1;
                        match add(library, prepared, &source, running.target, &folders) {
                            Ok(LibraryImportOutcome::Added(id)) => {
                                progress.added.push(id);
                                self.added.push(id);
                                self.adopted.push((source, id));
                            }
                            Ok(
                                LibraryImportOutcome::Duplicate(id)
                                | LibraryImportOutcome::DuplicateInTrash(id),
                            ) => {
                                self.adopted.push((source.clone(), id));
                                progress.duplicates.push((source, id));
                            }
                            Err(error) => progress.failed.push((source, format!("{error:#}"))),
                        }
                    }
                    Event::Failed { source, error } => {
                        progress.processed += 1;
                        progress.failed.push((source, error));
                    }
                    Event::Done => {
                        finished.push(index);
                        break;
                    }
                }
            }
        }
        for index in finished.into_iter().rev() {
            self.running.remove(index);
        }
        if self.running.is_empty()
            && let Some(progress) = &mut self.progress
        {
            progress.finished = true;
        }
    }

    /// Stops the imports under way; files already added stay.
    pub fn cancel(&mut self) {
        for running in self.running.drain(..) {
            running.cancel.store(true, Ordering::Release);
        }
        if let Some(progress) = &mut self.progress {
            progress.finished = true;
        }
    }

    pub fn is_running(&self) -> bool {
        !self.running.is_empty()
    }

    /// The current or last import, for the strip.
    pub fn progress(&self) -> Option<&ImportProgress> {
        self.progress.as_ref().filter(|progress| !progress.quiet)
    }

    /// Hides the finished import's strip.
    pub fn dismiss(&mut self) {
        if !self.is_running() {
            self.progress = None;
        }
    }

    /// The items added since the last call.
    pub fn take_added(&mut self) -> Vec<DocumentId> {
        std::mem::take(&mut self.added)
    }

    /// The files brought in since the last call, with their items.
    pub fn take_adopted(&mut self) -> Vec<(PathBuf, DocumentId)> {
        std::mem::take(&mut self.adopted)
    }
}

fn add(
    library: &mut LibraryStore,
    prepared: Prepared,
    source: &Path,
    target: Option<LibraryCollectionId>,
    folders: &[String],
) -> anyhow::Result<LibraryImportOutcome> {
    let collection = if folders.is_empty() {
        target
    } else {
        library.ensure_library_collection_path(target, folders)?
    };
    let metadata = LibraryItemMetadata::for_path(source);
    match prepared {
        Prepared::Managed(prepared) => {
            library.finalize_managed_import(prepared, metadata, collection)
        }
        Prepared::Linked(prepared) => {
            library.finalize_linked_import(prepared, metadata, collection)
        }
    }
}

/// The library's documents under `paths`, with the folder path each lies
/// in below the folder that was dropped (that folder's own name first).
/// Hidden entries and symbolic links are skipped.
pub fn walk(paths: &[PathBuf]) -> Vec<(PathBuf, Vec<String>)> {
    let mut files = Vec::new();
    for path in paths {
        let Ok(metadata) = fs::symlink_metadata(path) else {
            continue;
        };
        if metadata.is_file() {
            if is_library_document(path) {
                files.push((path.clone(), Vec::new()));
            }
        } else if metadata.is_dir() {
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            walk_folder(path, vec![name], 0, &mut files);
        }
    }
    files
}

fn walk_folder(
    folder: &Path,
    folders: Vec<String>,
    depth: usize,
    files: &mut Vec<(PathBuf, Vec<String>)>,
) {
    let Ok(entries) = fs::read_dir(folder) else {
        return;
    };
    let mut entries = entries.flatten().collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        if file_type.is_file() && is_library_document(&path) {
            files.push((path, folders.clone()));
        } else if file_type.is_dir() && depth < MAX_DEPTH {
            let mut inner = folders.clone();
            inner.push(name);
            walk_folder(&path, inner, depth + 1, files);
        }
    }
}

/// A PDF or EPUB, the formats the library keeps.
pub fn is_library_document(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .and_then(DocumentFormat::from_extension)
        .is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dropped_folder_gives_its_documents_and_their_collection_paths() {
        let root = std::env::temp_dir().join(format!("glyphst-import-walk-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let papers = root.join("papers");
        fs::create_dir_all(papers.join("llms")).unwrap();
        fs::create_dir_all(papers.join(".hidden")).unwrap();
        fs::write(papers.join("a.pdf"), b"a").unwrap();
        fs::write(papers.join("notes.txt"), b"n").unwrap();
        fs::write(papers.join("llms").join("b.EPUB"), b"b").unwrap();
        fs::write(papers.join(".hidden").join("c.pdf"), b"c").unwrap();
        let loose = root.join("loose.pdf");
        fs::write(&loose, b"l").unwrap();

        let found = walk(&[papers.clone(), loose.clone()]);
        let _ = fs::remove_dir_all(&root);
        assert_eq!(
            found,
            vec![
                (papers.join("a.pdf"), vec!["papers".to_string()]),
                (
                    papers.join("llms").join("b.EPUB"),
                    vec!["papers".to_string(), "llms".to_string()]
                ),
                (loose, Vec::new()),
            ]
        );
    }

    #[test]
    fn an_import_adds_files_into_collections_and_counts_duplicates() {
        let root = std::env::temp_dir().join(format!("glyphst-import-run-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let folder = root.join("reading");
        fs::create_dir_all(&folder).unwrap();
        fs::write(folder.join("one.pdf"), b"%PDF-1.4 one").unwrap();
        fs::write(folder.join("two.pdf"), b"%PDF-1.4 one").unwrap();
        let mut library = LibraryStore::memory();
        let ctx = egui::Context::default();
        let mut importer = Importer::default();
        importer.start(
            &library,
            vec![folder.clone()],
            ImportMode::Copy,
            None,
            false,
            &ctx,
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while importer.is_running() && std::time::Instant::now() < deadline {
            importer.poll(&mut library);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let progress = importer.progress().unwrap().clone();
        let _ = fs::remove_dir_all(&root);
        assert!(progress.finished);
        assert_eq!(progress.total, 2);
        assert_eq!(progress.added.len(), 1);
        assert_eq!(progress.duplicates.len(), 1, "the same bytes twice");
        assert_eq!(
            progress.mode,
            Some(ImportMode::Link),
            "a memory library links"
        );
        let id = progress.added[0];
        assert_eq!(
            library
                .collections_for_item(id)
                .into_iter()
                .map(|collection| collection.name.clone())
                .collect::<Vec<_>>(),
            vec!["reading".to_string()]
        );
        assert_eq!(importer.take_added(), vec![id]);
    }
}
