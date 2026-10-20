//! Each record a machine syncs has a uid, the same on every machine, beside
//! its local number (which differs between machines, and may be given out
//! again after the record is gone). A mapping is never removed, and local
//! numbers are not given out again below the highest one mapped.

use std::collections::{BTreeMap, HashMap};

use super::*;

/// `<kind>/<local id>` → the uid's 16 bytes.
pub(in crate::library) const SYNC_IDS_V1_TABLE: TableDefinition<&str, &[u8]> =
    TableDefinition::new("sync_ids_v1");
/// This machine's part in syncing (its device, clock and journal), by key.
pub(in crate::library) const SYNC_STATE_V1_TABLE: TableDefinition<&str, &[u8]> =
    TableDefinition::new("sync_state_v1");

/// The kinds of record that have uids. Tags have none: their normalized
/// name is the same everywhere.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SyncKind {
    Item,
    Attachment,
    Collection,
    SavedView,
    Highlight,
    Note,
    Bookmark,
    Page,
}

impl SyncKind {
    pub const ALL: [Self; 8] = [
        Self::Item,
        Self::Attachment,
        Self::Collection,
        Self::SavedView,
        Self::Highlight,
        Self::Note,
        Self::Bookmark,
        Self::Page,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Item => "item",
            Self::Attachment => "attachment",
            Self::Collection => "collection",
            Self::SavedView => "view",
            Self::Highlight => "highlight",
            Self::Note => "note",
            Self::Bookmark => "bookmark",
            Self::Page => "page",
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.name() == name)
    }
}

/// Uids by local id and back.
#[derive(Clone, Debug, Default)]
pub struct SyncIds {
    by_local: HashMap<(SyncKind, u64), Uuid>,
    by_uid: HashMap<(SyncKind, Uuid), u64>,
    highest: BTreeMap<SyncKind, u64>,
    /// Enrichment runs taken from other machines: the local run → the
    /// run as the journal names it (`<device>:<run>`).
    runs: HashMap<u64, String>,
}

impl SyncIds {
    pub fn uid(&self, kind: SyncKind, local: u64) -> Option<Uuid> {
        self.by_local.get(&(kind, local)).copied()
    }

    pub fn local(&self, kind: SyncKind, uid: Uuid) -> Option<u64> {
        self.by_uid.get(&(kind, uid)).copied()
    }

    /// The highest local id of `kind` ever mapped.
    pub fn highest(&self, kind: SyncKind) -> u64 {
        self.highest.get(&kind).copied().unwrap_or(0)
    }

    /// How the journal names a local enrichment run of this machine's
    /// (`device`) or one taken from another.
    pub fn run_token(&self, device: glyphst_sync::DeviceId, run: u64) -> String {
        self.runs
            .get(&run)
            .cloned()
            .unwrap_or_else(|| format!("{device}:{run}"))
    }

    fn insert(&mut self, kind: SyncKind, local: u64, uid: Uuid) {
        self.by_local.insert((kind, local), uid);
        self.by_uid.insert((kind, uid), local);
        let highest = self.highest.entry(kind).or_default();
        *highest = (*highest).max(local);
    }
}

fn row_key(kind: SyncKind, local: u64) -> String {
    format!("{}/{local}", kind.name())
}

pub(super) fn load_sync_ids(database: &Database) -> Result<SyncIds> {
    let read_txn = database.begin_read()?;
    let table = read_txn.open_table(SYNC_IDS_V1_TABLE)?;
    let mut ids = SyncIds::default();
    for entry in table.iter()? {
        let (key, value) = entry?;
        if let Some(run) = key
            .value()
            .strip_prefix("run/")
            .and_then(|run| run.parse::<u64>().ok())
        {
            ids.runs
                .insert(run, String::from_utf8_lossy(value.value()).into_owned());
            continue;
        }
        let parsed = key.value().split_once('/').and_then(|(kind, local)| {
            Some((SyncKind::from_name(kind)?, local.parse::<u64>().ok()?))
        });
        match (parsed, Uuid::from_slice(value.value())) {
            (Some((kind, local)), Ok(uid)) => ids.insert(kind, local, uid),
            _ => eprintln!("failed to decode a sync id: {}", key.value()),
        }
    }
    Ok(ids)
}

impl LibraryStore {
    /// The local ids of every record that has (or should have) a uid.
    fn sync_locals(&self) -> Vec<(SyncKind, u64)> {
        let library = &self.library;
        let mut locals = Vec::new();
        locals.extend(
            library
                .library_items
                .iter()
                .map(|item| (SyncKind::Item, item.id.0)),
        );
        locals.extend(
            library
                .library_attachments
                .iter()
                .map(|attachment| (SyncKind::Attachment, attachment.id.0)),
        );
        locals.extend(
            library
                .library_collections
                .iter()
                .map(|collection| (SyncKind::Collection, collection.id.0)),
        );
        locals.extend(
            library
                .library_saved_views
                .iter()
                .map(|view| (SyncKind::SavedView, view.id.0)),
        );
        locals.extend(
            library
                .highlights
                .iter()
                .map(|highlight| (SyncKind::Highlight, highlight.id)),
        );
        locals.extend(library.notes.iter().map(|note| (SyncKind::Note, note.id)));
        locals.extend(
            library
                .bookmarks
                .iter()
                .map(|bookmark| (SyncKind::Bookmark, bookmark.id)),
        );
        locals.extend(self.knowledge.pages.keys().map(|id| (SyncKind::Page, *id)));
        locals
    }

    /// Gives a uid to every record without one, and keeps it.
    pub fn assign_sync_ids(&mut self) -> Result<()> {
        let missing = self
            .sync_locals()
            .into_iter()
            .filter(|(kind, local)| self.sync.ids.uid(*kind, *local).is_none())
            .map(|(kind, local)| (kind, local, Uuid::new_v4()))
            .collect::<Vec<_>>();
        if missing.is_empty() {
            return Ok(());
        }
        if let Some(database) = &self.database {
            let write_txn = database.begin_write()?;
            {
                let mut table = write_txn.open_table(SYNC_IDS_V1_TABLE)?;
                for (kind, local, uid) in &missing {
                    table.insert(row_key(*kind, *local).as_str(), uid.as_bytes().as_slice())?;
                }
            }
            write_txn.commit()?;
        }
        for (kind, local, uid) in missing {
            self.sync.ids.insert(kind, local, uid);
        }
        self.library.reserve_synced_ids(&self.sync.ids);
        Ok(())
    }

    /// Uids, every record given one first.
    pub fn sync_ids(&mut self) -> Result<&SyncIds> {
        self.assign_sync_ids()?;
        Ok(&self.sync.ids)
    }

    /// The uid of a record, if it has one yet.
    pub fn sync_uid(&self, kind: SyncKind, local: u64) -> Option<Uuid> {
        self.sync.ids.uid(kind, local)
    }

    /// Counts every write of something synced (the catalog, annotations,
    /// AI results, pages, vault notes), so the sync knows when to look.
    pub fn sync_revision(&self) -> u64 {
        self.sync.revision.load(AtomicOrdering::Relaxed)
    }

    pub(in crate::library) fn synced_write(&self) {
        self.sync.revision.fetch_add(1, AtomicOrdering::Relaxed);
    }
}

impl Library {
    /// Local ids are not given out again below the highest one mapped: a
    /// new record must not take a removed one's uid.
    pub(in crate::library) fn reserve_synced_ids(&mut self, ids: &SyncIds) {
        let next = |kind| ids.highest(kind).saturating_add(1);
        self.next_library_item_id = self.next_library_item_id.max(next(SyncKind::Item));
        self.next_library_attachment_id = self
            .next_library_attachment_id
            .max(next(SyncKind::Attachment));
        self.next_library_collection_id = self
            .next_library_collection_id
            .max(next(SyncKind::Collection));
        self.next_library_saved_view_id = self
            .next_library_saved_view_id
            .max(next(SyncKind::SavedView));
        self.next_highlight_id = self.next_highlight_id.max(next(SyncKind::Highlight));
        self.next_note_id = self.next_note_id.max(next(SyncKind::Note));
        self.next_bookmark_id = self.next_bookmark_id.max(next(SyncKind::Bookmark));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("glyphst-sync-ids-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn item(store: &mut LibraryStore, dir: &Path, name: &str) -> (DocumentId, PathBuf) {
        let path = dir.join(format!("{name}.pdf"));
        fs::write(&path, format!("%PDF-1.4 {name}")).unwrap();
        match store
            .link_item(&path, LibraryItemMetadata::for_path(&path), None)
            .unwrap()
        {
            LibraryImportOutcome::Added(id) => (id, path),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn uids_stay_and_a_removed_records_number_is_not_given_out_again() {
        let dir = folder("stable");
        let database = dir.join("library.redb");
        let (kept, removed, kept_uid, removed_uid) = {
            let mut store = LibraryStore::open_for_test(database.clone()).unwrap();
            let (kept, _) = item(&mut store, &dir, "kept");
            let (removed, _) = item(&mut store, &dir, "removed");
            let ids = store.sync_ids().unwrap();
            let kept_uid = ids.uid(SyncKind::Item, kept.0).unwrap();
            let removed_uid = ids.uid(SyncKind::Item, removed.0).unwrap();
            assert_ne!(kept_uid, removed_uid);
            assert_eq!(ids.local(SyncKind::Item, kept_uid), Some(kept.0));
            store.trash_library_items(&[removed]).unwrap();
            store.permanently_delete_library_items(&[removed]).unwrap();
            (kept, removed, kept_uid, removed_uid)
        };

        let mut store = LibraryStore::open_for_test(database).unwrap();
        assert_eq!(store.sync_uid(SyncKind::Item, kept.0), Some(kept_uid));
        let (added, _) = item(&mut store, &dir, "added");
        assert!(added.0 > removed.0, "{added:?} took {removed:?}'s number");
        let added_uid = store
            .sync_ids()
            .unwrap()
            .uid(SyncKind::Item, added.0)
            .unwrap();
        assert_ne!(added_uid, removed_uid);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn synced_writes_count_and_reading_state_does_not() {
        let dir = folder("revision");
        let mut store = LibraryStore::open_for_test(dir.join("library.redb")).unwrap();
        let (id, path) = item(&mut store, &dir, "paper");
        let rect = HighlightRect {
            page_idx: 0,
            x0: 0.0,
            y0: 0.0,
            x1: 1.0,
            y1: 1.0,
        };

        type Mutation = Box<dyn Fn(&mut LibraryStore, DocumentId, &Path)>;
        let synced: Vec<(&str, Mutation)> = vec![
            (
                "favourite",
                Box::new(|store, id, _| store.set_library_item_favorite(id, true).unwrap()),
            ),
            (
                "tags",
                Box::new(|store, id, _| store.set_item_tag_names(id, &["ml".to_string()]).unwrap()),
            ),
            (
                "highlight",
                Box::new(move |store, _, path| {
                    store
                        .add_highlight(path, HighlightColor::Yellow, vec![rect], "text", "")
                        .unwrap();
                }),
            ),
            (
                "bookmark",
                Box::new(|store, _, path| {
                    store
                        .add_bookmark(path, None, DocumentPosition::fixed(0.0, 0.0, 1.0, 0))
                        .unwrap();
                }),
            ),
            (
                "dismissed topic",
                Box::new(|store, _, _| store.dismiss_topic("tag:ml").unwrap()),
            ),
            (
                "vault note",
                Box::new(|store, id, _| store.set_vault_note(id, None).unwrap()),
            ),
        ];
        for (name, mutation) in &synced {
            let before = store.sync_revision();
            mutation(&mut store, id, &path);
            assert!(store.sync_revision() > before, "{name} did not count");
        }

        let local: Vec<(&str, Mutation)> = vec![
            (
                "history",
                Box::new(|store, _, path| {
                    store
                        .replace_history_entries(vec![HistoryEntry {
                            path: path.to_path_buf(),
                            opened: 1,
                            position: DocumentPosition::fixed(0.0, 0.0, 1.0, 3),
                        }])
                        .unwrap()
                }),
            ),
            (
                "rotation",
                Box::new(|store, _, path| {
                    store
                        .set_document_rotation(path, DocumentRotation::Degrees90)
                        .unwrap();
                }),
            ),
            (
                "activity",
                Box::new(|store, _, path| {
                    store
                        .record_activity(ActivityKind::Open, path, "", 0)
                        .unwrap()
                }),
            ),
        ];
        for (name, mutation) in &local {
            let before = store.sync_revision();
            mutation(&mut store, id, &path);
            assert_eq!(store.sync_revision(), before, "{name} counted");
        }
        fs::remove_dir_all(dir).unwrap();
    }
}
