//! Keeping the Markdown vault and the catalog in step. A worker thread
//! writes each item's note when what it shows changes (merging, so the
//! user's text and edits stay), and reads it again when the file changes;
//! tags the user changed in a note come back to the catalog. The same
//! thread keeps an index of every note's links, for backlinks.

use std::{
    collections::{HashMap, HashSet},
    fs,
    hash::{DefaultHasher, Hash, Hasher},
    path::{Path, PathBuf},
    sync::{
        Arc,
        mpsc::{Receiver, Sender, channel},
    },
    time::{Duration, Instant, UNIX_EPOCH},
};

use glyphst_library::{
    AssignmentState, AssignmentTarget, DocumentId, LibraryItemKind, LibraryStore, ReadingStatus,
    VaultNoteRecord,
};
use glyphst_vault::{ITEMS_FOLDER, Merged, NoteSnapshot, Summary, Written, index::VaultIndex};

/// How often snapshots are compared and notes' files checked.
const CHECK_EVERY: Duration = Duration::from_secs(2);

struct NoteRequest {
    root: PathBuf,
    id: DocumentId,
    snapshot: NoteSnapshot,
    record: Option<VaultNoteRecord>,
    /// Note names already used by other items.
    taken: HashSet<String>,
}

enum Work {
    Note(Box<NoteRequest>),
    /// Index the vault's links again, reusing `previous` for notes that
    /// did not change.
    Scan {
        root: PathBuf,
        previous: Arc<VaultIndex>,
    },
}

enum NoteResult {
    Scanned {
        root: PathBuf,
        index: VaultIndex,
    },
    Written {
        id: DocumentId,
        record: VaultNoteRecord,
        user_tags: Option<Vec<String>>,
    },
    Failed {
        id: DocumentId,
        error: String,
    },
}

pub struct VaultSync {
    requests: Sender<Work>,
    results: Receiver<NoteResult>,
    /// The hash of each item's last snapshot sent.
    sent: HashMap<DocumentId, u64>,
    in_flight: HashSet<DocumentId>,
    root: Option<PathBuf>,
    last_check: Option<Instant>,
    last_revision: Option<u64>,
    /// The vault's links, as of the last scan, and whether one is running.
    index: Arc<VaultIndex>,
    scanning: bool,
}

impl VaultSync {
    pub fn new(ctx: &egui::Context) -> Self {
        let (requests, wanted) = channel::<Work>();
        let (finished, results) = channel();
        let ctx = ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("vault".into())
            .spawn(move || {
                for work in wanted {
                    let result = match work {
                        Work::Note(request) => write_note(*request),
                        Work::Scan { root, previous } => NoteResult::Scanned {
                            index: VaultIndex::scan(&root, &previous),
                            root,
                        },
                    };
                    if finished.send(result).is_err() {
                        break;
                    }
                    ctx.request_repaint();
                }
            });
        if let Err(error) = spawned {
            log::warn!("no vault thread: {error}");
        }
        Self {
            requests,
            results,
            sent: HashMap::new(),
            in_flight: HashSet::new(),
            root: None,
            last_check: None,
            last_revision: None,
            index: Arc::default(),
            scanning: false,
        }
    }

    /// The vault's notes and their links, as of the last scan.
    pub fn index(&self) -> Arc<VaultIndex> {
        self.index.clone()
    }

    /// Takes in written notes, and sends the notes that changed (or whose
    /// files changed) to be written. `root` is the vault, `None` while
    /// there is none. Returns notices for the user.
    pub fn poll(&mut self, library: &mut LibraryStore, root: Option<PathBuf>) -> Vec<String> {
        let mut notices = Vec::new();
        if root != self.root {
            self.root = root;
            self.sent.clear();
            self.last_check = None;
            self.index = Arc::default();
        }
        while let Ok(result) = self.results.try_recv() {
            match result {
                NoteResult::Scanned { root, index } => {
                    self.scanning = false;
                    if Some(&root) == self.root.as_ref() && *self.index != index {
                        self.index = Arc::new(index);
                    }
                }
                NoteResult::Written {
                    id,
                    record,
                    user_tags,
                } => {
                    self.in_flight.remove(&id);
                    if let Err(error) = library.set_vault_note(id, Some(record)) {
                        notices.push(format!("Could not record a note: {error:#}"));
                    }
                    if let Some(tags) = user_tags {
                        let names = catalog_tag_names(library, &tags);
                        if let Err(error) = library.set_item_tag_names(id, &names) {
                            notices.push(format!("Could not take a note's tags: {error:#}"));
                        }
                        // Written again with the catalog's tags.
                        self.sent.remove(&id);
                    }
                }
                NoteResult::Failed { id, error } => {
                    self.in_flight.remove(&id);
                    notices.push(format!("Could not write a note: {error}"));
                }
            }
        }
        let Some(root) = self.root.clone() else {
            return notices;
        };
        let now = Instant::now();
        let revision = library.catalog_revision();
        if self.last_revision == Some(revision)
            && self
                .last_check
                .is_some_and(|last| now.duration_since(last) < CHECK_EVERY)
        {
            return notices;
        }
        self.last_revision = Some(revision);
        self.last_check = Some(now);
        let taken = library
            .vault_notes()
            .map(|(_, record)| note_name(&record.relative_path))
            .collect::<HashSet<_>>();
        let ids = library
            .library_items()
            .iter()
            .filter(|item| item.trashed_at.is_none())
            .map(|item| item.id)
            .collect::<Vec<_>>();
        for id in ids {
            if self.in_flight.contains(&id) {
                continue;
            }
            let Some(snapshot) = snapshot(library, id) else {
                continue;
            };
            let hash = {
                let mut hasher = DefaultHasher::new();
                snapshot.hash(&mut hasher);
                hasher.finish()
            };
            let record = library.vault_note(id).cloned();
            // The user saved the note since it was last read or written.
            let file_changed = record.as_ref().is_some_and(|record| {
                modified(&root.join(&record.relative_path)) != Some(record.modified)
            });
            if self.sent.get(&id) == Some(&hash) && !file_changed {
                continue;
            }
            let request = NoteRequest {
                root: root.clone(),
                id,
                snapshot,
                record,
                taken: taken.clone(),
            };
            if self.requests.send(Work::Note(Box::new(request))).is_ok() {
                self.sent.insert(id, hash);
                self.in_flight.insert(id);
            }
        }
        // After the notes, so the index has those just written.
        if !self.scanning {
            let scan = Work::Scan {
                root,
                previous: self.index.clone(),
            };
            self.scanning = self.requests.send(scan).is_ok();
        }
        notices
    }

    /// Whether notes are being written (screenshots wait).
    /// Whether the worker is writing item `id`'s note now.
    pub fn in_flight(&self, id: DocumentId) -> bool {
        self.in_flight.contains(&id)
    }

    pub fn busy(&self) -> bool {
        !self.in_flight.is_empty() || self.scanning
    }
}

/// What the item's note shows.
fn snapshot(library: &LibraryStore, id: DocumentId) -> Option<NoteSnapshot> {
    let item = library.library_item(id)?;
    let mut tags = library
        .tags_for_item(id)
        .into_iter()
        .map(|tag| tag.name.clone())
        .collect::<Vec<_>>();
    tags.sort_by_key(|tag| tag.to_lowercase());
    let mut collections = library
        .collections_for_item(id)
        .into_iter()
        .map(|collection| library.collection_path_label(collection.id))
        .collect::<Vec<_>>();
    collections.sort();
    let mut ai_tags = library
        .ai_assignments(id)
        .iter()
        .filter(|assignment| {
            matches!(assignment.target, AssignmentTarget::Tag(_))
                && matches!(
                    assignment.state,
                    AssignmentState::Applied | AssignmentState::Accepted
                )
        })
        .map(|assignment| assignment.label.clone())
        .collect::<Vec<_>>();
    ai_tags.sort();
    let summary = library.item_enrichment(id).map(|enrichment| Summary {
        tldr: enrichment.tldr.clone(),
        key_points: enrichment.key_points.clone(),
        model: enrichment.model.clone(),
        date: chrono::DateTime::from_timestamp(enrichment.finished_at as i64, 0)
            .map(|time| time.date_naive().to_string())
            .unwrap_or_default(),
    });
    Some(NoteSnapshot {
        id: id.0,
        title: item.title.clone(),
        authors: item
            .creators
            .iter()
            .map(|creator| creator.name.clone())
            .collect(),
        year: item.year,
        kind: match item.kind {
            LibraryItemKind::Paper => "paper",
            LibraryItemKind::Book => "book",
            LibraryItemKind::Other => "other",
        }
        .to_string(),
        doi: item.identifiers.doi.clone(),
        arxiv: item.identifiers.arxiv.clone(),
        isbn: item.identifiers.isbn.clone(),
        publication: item.publication.clone(),
        tags,
        collections,
        status: match item.status {
            ReadingStatus::ToRead => "to-read",
            ReadingStatus::Reading => "reading",
            ReadingStatus::Read => "read",
        }
        .to_string(),
        ai_tags,
        summary,
        file: library
            .primary_attachment(id)
            .map(|attachment| attachment.path.display().to_string()),
        seed_notes: item.work_note.clone(),
    })
}

/// The catalog's names for tags as written in a note (where spaces became
/// hyphens): an existing tag when one matches, else the name as written.
fn catalog_tag_names(library: &LibraryStore, tags: &[String]) -> Vec<String> {
    tags.iter()
        .map(|tag| {
            library
                .library_tags()
                .iter()
                .find(|existing| glyphst_vault::vault_tag(&existing.name).eq_ignore_ascii_case(tag))
                .map(|existing| existing.name.clone())
                .unwrap_or_else(|| tag.clone())
        })
        .collect()
}

fn modified(path: &Path) -> Option<u64> {
    fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_secs())
}

fn note_name(relative_path: &str) -> String {
    relative_path
        .rsplit('/')
        .next()
        .unwrap_or(relative_path)
        .trim_end_matches(".md")
        .to_string()
}

/// Writes one note on the worker thread.
fn write_note(request: NoteRequest) -> NoteResult {
    let id = request.id;
    match write_note_inner(request) {
        Ok((record, user_tags)) => NoteResult::Written {
            id,
            record,
            user_tags,
        },
        Err(error) => NoteResult::Failed {
            id,
            error: format!("{error:#}"),
        },
    }
}

fn write_note_inner(
    request: NoteRequest,
) -> anyhow::Result<(VaultNoteRecord, Option<Vec<String>>)> {
    let folder = request.root.join(ITEMS_FOLDER);
    fs::create_dir_all(&folder)?;
    // Where the note is: as recorded, else found by its id (the user
    // renamed it), else a new name.
    let recorded = request
        .record
        .as_ref()
        .map(|record| request.root.join(&record.relative_path))
        .filter(|path| path.is_file());
    let path = match recorded {
        Some(path) => path,
        None => find_by_id(&folder, request.id.0).unwrap_or_else(|| {
            let mut taken = request.taken.clone();
            if let Ok(entries) = fs::read_dir(&folder) {
                taken.extend(entries.flatten().map(|entry| {
                    entry
                        .file_name()
                        .to_string_lossy()
                        .trim_end_matches(".md")
                        .to_string()
                }));
            }
            let slug = glyphst_vault::slug(
                &request.snapshot.authors,
                request.snapshot.year,
                &request.snapshot.title,
            );
            folder.join(format!("{}.md", glyphst_vault::unique_slug(&slug, &taken)))
        }),
    };
    let existing = fs::read_to_string(&path).ok();
    let merged: Merged = match (&existing, &request.record) {
        (Some(existing), record) => {
            let written = record
                .as_ref()
                .map(|record| Written {
                    tags: record.written_tags.clone(),
                    frontmatter_sha256: record.frontmatter_sha256,
                    blocks: record.blocks.clone(),
                })
                .unwrap_or_default();
            glyphst_vault::merge(existing, &request.snapshot, &written)
        }
        (None, _) => glyphst_vault::render(&request.snapshot),
    };
    if existing.as_deref() != Some(merged.text.as_str()) {
        let temporary = path.with_extension("md.glyphst-writing");
        fs::write(&temporary, &merged.text)?;
        fs::rename(&temporary, &path)?;
    }
    let relative_path = path
        .strip_prefix(&request.root)
        .unwrap_or(&path)
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/");
    let record = VaultNoteRecord {
        relative_path,
        written_tags: merged.written.tags,
        frontmatter_sha256: merged.written.frontmatter_sha256,
        blocks: merged.written.blocks,
        modified: modified(&path).unwrap_or(0),
        written_at: glyphst_library::current_timestamp(),
    };
    Ok((record, merged.user_tags))
}

fn find_by_id(folder: &Path, id: u64) -> Option<PathBuf> {
    fs::read_dir(folder)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "md"))
        .find(|path| {
            fs::read_to_string(path)
                .ok()
                .and_then(|text| glyphst_vault::note_id(&text))
                == Some(id)
        })
}

#[cfg(test)]
mod tests {
    use glyphst_library::{LibraryImportOutcome, LibraryItemMetadata};

    use super::*;

    fn wait(sync: &mut VaultSync, library: &mut LibraryStore, root: &Path) {
        let deadline = Instant::now() + Duration::from_secs(10);
        // One check now; the rest only take what the worker finished.
        sync.last_check = None;
        loop {
            let notices = sync.poll(library, Some(root.to_path_buf()));
            assert!(notices.is_empty(), "{notices:?}");
            if !sync.busy() {
                break;
            }
            assert!(Instant::now() < deadline, "the vault never settled");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn notes_are_written_and_tags_edited_in_them_reach_the_catalog() {
        let root = std::env::temp_dir().join(format!("glyphst-vault-sync-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let document = root.with_extension("pdf");
        fs::write(&document, b"%PDF-1.4 vault").unwrap();
        let mut library = LibraryStore::memory();
        let LibraryImportOutcome::Added(id) = library
            .link_item(&document, LibraryItemMetadata::for_path(&document), None)
            .unwrap()
        else {
            panic!("expected item");
        };
        let mut item = library.library_item(id).unwrap().clone();
        item.title = "Attention Is All You Need".to_string();
        item.year = Some(2017);
        item.creators = vec![glyphst_library::LibraryCreator {
            name: "Ashish Vaswani".to_string(),
            role: glyphst_library::LibraryCreatorRole::Author,
        }];
        library.update_library_item_metadata(item).unwrap();
        library
            .set_item_tag_names(id, &["to read".to_string()])
            .unwrap();

        let ctx = egui::Context::default();
        let mut sync = VaultSync::new(&ctx);
        wait(&mut sync, &mut library, &root);
        let record = library.vault_note(id).unwrap().clone();
        assert_eq!(record.relative_path, "items/vaswani2017attention.md");
        let path = root.join(&record.relative_path);
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("  - to-read\n"), "{text}");

        // The user adds a tag in Obsidian.
        std::thread::sleep(Duration::from_millis(1_100));
        let edited = text.replace("  - to-read\n", "  - to-read\n  - classics\n");
        fs::write(&path, edited).unwrap();
        wait(&mut sync, &mut library, &root);
        wait(&mut sync, &mut library, &root);
        let mut names = library
            .tags_for_item(id)
            .into_iter()
            .map(|tag| tag.name.clone())
            .collect::<Vec<_>>();
        names.sort();
        assert_eq!(names, vec!["classics".to_string(), "to read".to_string()]);
        let text = fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("  - classics\n") && text.contains("  - to-read\n"),
            "{text}"
        );

        // A note of the user's links the paper: its backlink is indexed.
        fs::create_dir_all(root.join("thesis")).unwrap();
        fs::write(
            root.join("thesis/chapter 2.md"),
            "# Chapter 2\nAs [[vaswani2017attention]] shows…\n",
        )
        .unwrap();
        wait(&mut sync, &mut library, &root);
        wait(&mut sync, &mut library, &root);
        let index = sync.index();
        let backlinks = index.backlinks(glyphst_vault::index::Owner::Item(id.0));
        assert_eq!(backlinks.len(), 1, "{index:#?}");
        assert_eq!(backlinks[0].title, "Chapter 2");

        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_file(document);
    }
}
