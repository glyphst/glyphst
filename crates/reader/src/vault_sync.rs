//! Keeping the Markdown vault and the catalog in step. A worker thread
//! writes each item's note when what it shows changes (merging, so the
//! user's text and edits stay), and reads it again when the file changes;
//! tags the user changed in a note come back to the catalog. The same
//! thread keeps an index of every note's links, for backlinks.
//!
//! When the library syncs, notes come from other computers too, in their
//! own time: a note written from a newer state of the catalog than this
//! computer has (`glyphst_rev`) is left alone until that state arrives,
//! and a note another computer is writing is not made here meanwhile.
//! Written from the same state, a note is the same on every computer.

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
    SyncKind, VaultNoteRecord,
};
use glyphst_sync::{DeviceId, Hlc};
use glyphst_vault::{
    ITEMS_FOLDER, Merged, NoteSnapshot, Summary, Written,
    index::{Owner, Owners, VaultIndex},
};

/// How often snapshots are compared and notes' files checked.
const CHECK_EVERY: Duration = Duration::from_secs(2);
/// How long a note another computer made may take to arrive before this
/// one makes it.
const ARRIVE_WITHIN_MS: u64 = 10 * 60 * 1000;

struct NoteRequest {
    root: PathBuf,
    id: DocumentId,
    snapshot: NoteSnapshot,
    record: Option<VaultNoteRecord>,
    /// Note names already used by other items.
    taken: HashSet<String>,
    /// This computer, and when (and where) the item last changed.
    own: DeviceId,
    rev: Option<Hlc>,
}

enum Work {
    Note(Box<NoteRequest>),
    /// Index the vault's links again, reusing `previous` for notes that
    /// did not change.
    Scan {
        root: PathBuf,
        previous: Arc<VaultIndex>,
        owners: Owners,
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
    /// Left alone until this computer has what the note was written from
    /// (or the note another computer made arrives).
    Deferred {
        id: DocumentId,
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
                        Work::Scan {
                            root,
                            previous,
                            owners,
                        } => NoteResult::Scanned {
                            index: VaultIndex::scan(&root, &previous, &owners),
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
    /// there is none; while not `quiet` (the library sync has changes to
    /// export or take in) nothing new is written. Returns notices for the
    /// user.
    pub fn poll(
        &mut self,
        library: &mut LibraryStore,
        root: Option<PathBuf>,
        quiet: bool,
    ) -> Vec<String> {
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
                NoteResult::Deferred { id } => {
                    self.in_flight.remove(&id);
                    // Looked at again next check.
                    self.sent.remove(&id);
                }
            }
        }
        let Some(root) = self.root.clone() else {
            return notices;
        };
        if !quiet {
            return notices;
        }
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
                own: library.sync_device(),
                rev: library.sync_item_rev(id),
            };
            if self.requests.send(Work::Note(Box::new(request))).is_ok() {
                self.sent.insert(id, hash);
                self.in_flight.insert(id);
            }
        }
        // After the notes, so the index has those just written.
        if !self.scanning {
            let mut owners = Owners::new();
            for item in library.library_items() {
                if let Some(uid) = library.sync_uid(SyncKind::Item, item.id.0) {
                    owners.insert(uid.to_string(), Owner::Item(item.id.0));
                }
            }
            for page in library.pages() {
                if let Some(uid) = library.sync_uid(SyncKind::Page, page.id) {
                    owners.insert(uid.to_string(), Owner::Page(page.id));
                }
            }
            let scan = Work::Scan {
                root,
                previous: self.index.clone(),
                owners,
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
                && matches!(assignment.state, AssignmentState::Applied)
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
        uid: library
            .sync_uid(SyncKind::Item, id.0)
            .map(|uid| uid.to_string())
            .unwrap_or_default(),
        rev: library
            .sync_item_rev(id)
            .map(|rev| rev.to_string())
            .unwrap_or_default(),
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
        // Its name only: where it is differs between computers.
        file: library.primary_attachment(id).and_then(|attachment| {
            attachment
                .path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
        }),
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
        Ok(None) => NoteResult::Deferred { id },
        Ok(Some((record, user_tags))) => NoteResult::Written {
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

/// Whether the item's newest change is another computer's, made so
/// recently that the note it writes may still be on its way.
fn others_note_on_its_way(request: &NoteRequest) -> bool {
    request.rev.is_some_and(|rev| {
        rev.device != request.own
            && glyphst_sync::wall_ms().saturating_sub(rev.millis) < ARRIVE_WITHIN_MS
    })
}

/// `None` when the note is left alone for now.
fn write_note_inner(
    request: NoteRequest,
) -> anyhow::Result<Option<(VaultNoteRecord, Option<Vec<String>>)>> {
    let folder = request.root.join(ITEMS_FOLDER);
    fs::create_dir_all(&folder)?;
    // Where the note is: as recorded, else found by its id (the user
    // renamed it), else a new name.
    let recorded = request
        .record
        .as_ref()
        .map(|record| request.root.join(&record.relative_path))
        .filter(|path| path.is_file());
    let found = recorded.or_else(|| find_note(&folder, &request.snapshot.uid, request.id.0));
    if found.is_none() && others_note_on_its_way(&request) {
        return Ok(None);
    }
    let path = match found {
        Some(path) => path,
        None => {
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
        }
    };
    let existing = fs::read_to_string(&path).ok();
    // Written by another computer from a newer catalog than this one has.
    if let Some(existing) = &existing
        && !request.snapshot.rev.is_empty()
        && let Some(theirs) = glyphst_vault::note_rev(existing)
        && theirs > request.snapshot.rev
    {
        return Ok(None);
    }
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
    Ok(Some((record, merged.user_tags)))
}

/// The item's note, renamed by the user: by its uid, else (an older note)
/// its local number. A sync tool's copy of a note is not it.
fn find_note(folder: &Path, uid: &str, id: u64) -> Option<PathBuf> {
    let notes = fs::read_dir(folder)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "md"))
        .filter(|path| !glyphst_vault::index::is_conflict_name(&path.to_string_lossy()))
        .filter_map(|path| Some((fs::read_to_string(&path).ok()?, path)))
        .collect::<Vec<_>>();
    notes
        .iter()
        .find(|(text, _)| !uid.is_empty() && glyphst_vault::note_uid(text).as_deref() == Some(uid))
        .or_else(|| {
            notes.iter().find(|(text, _)| {
                glyphst_vault::note_uid(text).is_none() && glyphst_vault::note_id(text) == Some(id)
            })
        })
        .map(|(_, path)| path.clone())
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
            let notices = sync.poll(library, Some(root.to_path_buf()), true);
            assert!(notices.is_empty(), "{notices:?}");
            if !sync.busy() {
                break;
            }
            assert!(Instant::now() < deadline, "the vault never settled");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Two computers' libraries, synced by hand through a journal.
    struct Pair {
        a: LibraryStore,
        b: LibraryStore,
        journal: glyphst_sync::Journal,
    }

    impl Pair {
        fn export(store: &mut LibraryStore, journal: &glyphst_sync::Journal) {
            store.export_now().unwrap();
            for (seq, bytes) in store.sync_outbox().unwrap() {
                journal
                    .write_segment(store.sync_device(), seq, &bytes)
                    .unwrap();
                let digest = glyphst_sync::Journal::digest_of(&bytes).unwrap();
                store.sync_segment_written(seq, digest).unwrap();
            }
        }

        fn take(store: &mut LibraryStore, journal: &glyphst_sync::Journal) {
            let batches = journal
                .pending(store.sync_device(), store.sync_watermarks(), usize::MAX)
                .unwrap();
            store.apply_sync(&batches).unwrap();
        }

        fn exchange(&mut self) {
            Self::export(&mut self.a, &self.journal);
            Self::export(&mut self.b, &self.journal);
            Self::take(&mut self.a, &self.journal);
            Self::take(&mut self.b, &self.journal);
        }
    }

    #[test]
    fn a_note_is_the_same_on_two_computers_and_never_written_back_and_forth() {
        let dir = std::env::temp_dir().join(format!("glyphst-vault-two-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let root = dir.join("notes");
        let document = dir.join("attention.pdf");
        fs::write(&document, b"%PDF-1.4 two").unwrap();
        let mut pair = Pair {
            a: LibraryStore::open_for_test(dir.join("a.redb")).unwrap(),
            b: LibraryStore::open_for_test(dir.join("b.redb")).unwrap(),
            journal: glyphst_sync::Journal::in_library(&dir.join("shared")),
        };
        let LibraryImportOutcome::Added(id) = pair
            .a
            .link_item(&document, LibraryItemMetadata::for_path(&document), None)
            .unwrap()
        else {
            panic!("expected item");
        };
        let mut item = pair.a.library_item(id).unwrap().clone();
        item.title = "Attention Is All You Need".to_string();
        item.year = Some(2017);
        pair.a.update_library_item_metadata(item).unwrap();
        pair.a
            .set_item_tag_names(id, &["to read".to_string()])
            .unwrap();
        pair.exchange();

        let ctx = egui::Context::default();
        let (mut on_a, mut on_b) = (VaultSync::new(&ctx), VaultSync::new(&ctx));
        wait(&mut on_a, &mut pair.a, &root);
        let path = root.join(&pair.a.vault_note(id).unwrap().relative_path);
        let written = fs::read_to_string(&path).unwrap();
        assert!(written.contains("glyphst_uid: "), "{written}");
        assert!(!written.contains("glyphst_id"), "{written}");
        assert!(
            !written.contains(&dir.display().to_string()),
            "no paths: {written}"
        );
        // b, from the same catalog, writes the same note: nothing changes.
        wait(&mut on_b, &mut pair.b, &root);
        assert_eq!(fs::read_to_string(&path).unwrap(), written);
        let on_b_id = pair.b.library_items()[0].id;
        assert!(pair.b.vault_note(on_b_id).is_some());

        // a retitles; its note reaches b before the change does.
        let mut item = pair.a.library_item(id).unwrap().clone();
        item.title = "Attention, revisited".to_string();
        pair.a.update_library_item_metadata(item).unwrap();
        Pair::export(&mut pair.a, &pair.journal);
        wait(&mut on_a, &mut pair.a, &root);
        let retitled = fs::read_to_string(&path).unwrap();
        assert!(
            retitled.contains("title: Attention, revisited"),
            "{retitled}"
        );
        wait(&mut on_b, &mut pair.b, &root);
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            retitled,
            "b left it alone"
        );

        // Once b has the change, nothing goes back and forth.
        for _ in 0..3 {
            pair.exchange();
            wait(&mut on_b, &mut pair.b, &root);
            wait(&mut on_a, &mut pair.a, &root);
            assert_eq!(fs::read_to_string(&path).unwrap(), retitled);
        }
        assert_eq!(
            pair.b.library_item(on_b_id).unwrap().title,
            "Attention, revisited"
        );
        fs::remove_dir_all(dir).unwrap();
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
