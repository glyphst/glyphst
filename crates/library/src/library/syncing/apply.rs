//! Other machines' changes, merged into the registers field by field (the
//! later one winning) and made into this machine's records: new ones get
//! local numbers, references are looked up by uid, and what refers to
//! something not here yet (a highlight whose file has not arrived) waits.
//!
//! Applied only when this machine has nothing unexported (the app exports
//! first), so a local edit is never overwritten by an older remote one.
//! Merges and removals then go through the library's own operations,
//! which are the same on every machine, so all of them converge.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use glyphst_sync::{Batch, DELETED, Observed, RecordKey, Register, wall_ms, wins};
use serde::de::DeserializeOwned;
use serde_json::Value;

use super::canonical::kinds;
use super::ids::IdRows;
use super::registers::{StoredRecordV1, row_key};
use super::*;
use crate::library::knowledge::{KNOWLEDGE_STORAGE_VERSION, StoredPageV1, StoredSuggestionsV1};

/// What applying others' changes did.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ApplyReport {
    /// Fields taken from other machines.
    pub fields: usize,
    /// Records waiting for what they refer to (an item, a file).
    pub parked: usize,
    /// What could not be kept, in words.
    pub conflicts: Vec<String>,
    /// Copied files an attachment names that are not in the library
    /// folder yet.
    pub files_expected: Vec<PathBuf>,
    /// Machines whose clocks run more than a day ahead, by how much (ms).
    pub clock_ahead: BTreeMap<DeviceId, u64>,
}

enum Outcome {
    Done,
    Parked,
    Aliased,
}

/// A record's winning values (no nulls) and whether it was removed.
struct Winning {
    fields: BTreeMap<String, Value>,
    deleted: bool,
}

impl Winning {
    fn of(stored: &StoredRecordV1) -> Self {
        let deleted = stored
            .fields
            .get(DELETED)
            .is_some_and(|register| register.value == Value::Bool(true));
        Self {
            fields: stored
                .fields
                .iter()
                .filter(|(field, register)| *field != DELETED && !register.value.is_null())
                .map(|(field, register)| (field.clone(), register.value.clone()))
                .collect(),
            deleted,
        }
    }

    fn get<T: DeserializeOwned>(&self, field: &str) -> Option<T> {
        self.fields
            .get(field)
            .and_then(|value| serde_json::from_value(value.clone()).ok())
    }

    fn text(&self, field: &str) -> String {
        self.get::<String>(field).unwrap_or_default()
    }

    fn uid(&self, field: &str) -> Option<Uuid> {
        self.get::<String>(field)
            .and_then(|text| Uuid::parse_str(&text).ok())
    }

    /// The members of a set field (`tags/…`), in order.
    fn members(&self, prefix: &str) -> Vec<String> {
        self.fields
            .iter()
            .filter(|(_, value)| **value == Value::Bool(true))
            .filter_map(|(field, _)| field.strip_prefix(prefix))
            .map(str::to_string)
            .collect()
    }
}

/// A part of a JSON object, or its default.
fn part<T: DeserializeOwned + Default>(value: &Value, name: &str) -> T {
    value
        .get(name)
        .and_then(|part| serde_json::from_value(part.clone()).ok())
        .unwrap_or_default()
}

fn unhex(text: &str) -> Option<[u8; 32]> {
    if text.len() != 64 {
        return None;
    }
    let mut bytes = [0; 32];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(text.get(index * 2..index * 2 + 2)?, 16).ok()?;
    }
    Some(bytes)
}

/// The library as it is being changed, and what to write.
struct Work {
    library: Library,
    ai: ai_catalog::AiCatalogState,
    knowledge: knowledge::KnowledgeState,
    vault_notes: HashMap<DocumentId, VaultNoteRecord>,
    ids: SyncIds,
    id_rows: IdRows,
    root: Option<PathBuf>,
    device: DeviceId,
    ai_items: BTreeSet<DocumentId>,
    annotation_paths: BTreeSet<PathBuf>,
    highlights: BTreeSet<u64>,
    notes: BTreeSet<u64>,
    bookmarks: BTreeSet<u64>,
    pages: BTreeSet<u64>,
    suggestion_rows: BTreeSet<u64>,
    topics: BTreeMap<String, bool>,
    vault_rows: BTreeSet<DocumentId>,
    /// Items another machine merged: (absorbed, into), by uid.
    merges: Vec<(Uuid, Uuid)>,
    /// Items removed on another machine, with how many highlights and
    /// notes this machine has on their files.
    removals: Vec<(DocumentId, usize)>,
    /// Items whose file another machine has under another item.
    same_file: Vec<(DocumentId, DocumentId)>,
    report: ApplyReport,
}

impl Work {
    fn local(&self, kind: SyncKind, uid: Uuid) -> Option<u64> {
        self.ids.local(kind, uid)
    }

    fn item(&self, uid: Uuid) -> Option<DocumentId> {
        let id = DocumentId(self.local(SyncKind::Item, uid)?);
        self.library
            .library_items
            .iter()
            .any(|item| item.id == id)
            .then_some(id)
    }

    fn item_text(&self, text: &str) -> Option<DocumentId> {
        Uuid::parse_str(text).ok().and_then(|uid| self.item(uid))
    }

    fn collection(&self, uid: Uuid) -> Option<LibraryCollectionId> {
        let id = LibraryCollectionId(self.local(SyncKind::Collection, uid)?);
        self.library
            .library_collections
            .iter()
            .any(|collection| collection.id == id)
            .then_some(id)
    }

    fn map(&mut self, kind: SyncKind, local: u64, uid: Uuid) {
        self.ids.insert(kind, local, uid);
        self.id_rows.uid(kind, local, uid);
    }

    fn alias(&mut self, kind: SyncKind, uid: Uuid, local: u64) {
        self.ids.insert_alias(kind, uid, local);
        self.id_rows.alias(kind, uid, local);
    }

    /// The tag with this normalized name, made if there is none.
    fn tag(&mut self, normalized: &str, name: Option<&str>) -> LibraryTagId {
        if let Some(tag) = self
            .library
            .library_tags
            .iter()
            .find(|tag| tag.normalized_name == normalized)
        {
            return tag.id;
        }
        let id = LibraryTagId(self.library.next_library_tag_id.max(1));
        self.library.next_library_tag_id = id.0 + 1;
        let name = name.unwrap_or(normalized).to_string();
        self.library.library_tags.push(LibraryTag {
            id,
            normalized_name: normalize_tag_name(&name),
            name,
        });
        id
    }

    fn tag_named(&self, normalized: &str) -> Option<LibraryTagId> {
        self.library
            .library_tags
            .iter()
            .find(|tag| tag.normalized_name == normalized)
            .map(|tag| tag.id)
    }

    fn apply_tag(&mut self, name: &str, record: &Winning) -> Outcome {
        if record.deleted {
            if let Some(id) = self.tag_named(name) {
                let library = &mut self.library;
                library.library_tags.retain(|tag| tag.id != id);
                library.library_item_tags.retain(|row| row.tag_id != id);
                library
                    .library_tag_colors
                    .retain(|color| color.tag_id != id);
            }
            return Outcome::Done;
        }
        let display = record.get::<String>("name");
        let id = self.tag(name, display.as_deref());
        if let Some(display) = display
            && let Some(tag) = self
                .library
                .library_tags
                .iter_mut()
                .find(|tag| tag.id == id)
        {
            tag.name = display;
        }
        self.library
            .library_tag_colors
            .retain(|color| color.tag_id != id);
        if let Some(color) = record.get::<HighlightColor>("color") {
            self.library
                .library_tag_colors
                .push(LibraryTagColor { tag_id: id, color });
        }
        Outcome::Done
    }

    fn apply_collection(&mut self, uid: Uuid, record: &Winning) -> Outcome {
        let local = self.collection(uid);
        if record.deleted {
            if let Some(id) = local {
                let library = &mut self.library;
                library
                    .library_collections
                    .retain(|collection| collection.id != id);
                library
                    .library_item_collections
                    .retain(|row| row.collection_id != id);
            }
            return Outcome::Done;
        }
        let parent = match record.uid("parent") {
            Some(parent) => match self.collection(parent) {
                Some(parent) => Some(parent),
                None => return Outcome::Parked,
            },
            None => None,
        };
        let name = clean_text(&record.text("name"));
        let normalized = normalize_collection_name(&name);
        let created = record.get::<u64>("created").unwrap_or(0);
        let updated = record.get::<u64>("updated").unwrap_or(created);
        if let Some(id) = local {
            if let Some(collection) = self
                .library
                .library_collections
                .iter_mut()
                .find(|collection| collection.id == id)
            {
                collection.name = name;
                collection.normalized_name = normalized;
                collection.parent_id = parent;
                collection.created = created;
                collection.updated = updated;
            }
            return Outcome::Done;
        }
        // Made here too, under the same name: the same collection.
        if let Some(existing) = self.library.library_collections.iter().find(|collection| {
            collection.parent_id == parent && collection.normalized_name == normalized
        }) {
            let existing = existing.id.0;
            self.alias(SyncKind::Collection, uid, existing);
            return Outcome::Aliased;
        }
        let id = LibraryCollectionId(self.library.next_library_collection_id.max(1));
        self.library.next_library_collection_id = id.0 + 1;
        self.library.library_collections.push(LibraryCollection {
            id,
            parent_id: parent,
            name,
            normalized_name: normalized,
            created,
            updated,
        });
        self.map(SyncKind::Collection, id.0, uid);
        Outcome::Done
    }

    fn apply_item(&mut self, uid: Uuid, record: &Winning) -> Outcome {
        let mapped = self.local(SyncKind::Item, uid).map(DocumentId);
        let local = self.item(uid);
        if record.deleted {
            if let Some(id) = local {
                let library = &self.library;
                let paths = library
                    .library_attachments
                    .iter()
                    .filter(|attachment| attachment.item_id == id)
                    .map(|attachment| normalize_path(&attachment.path))
                    .collect::<HashSet<_>>();
                let annotations = library
                    .highlights
                    .iter()
                    .filter(|highlight| paths.contains(&highlight.path))
                    .count()
                    + library
                        .notes
                        .iter()
                        .filter(|note| paths.contains(&note.path))
                        .count();
                self.removals.push((id, annotations));
            }
            return Outcome::Done;
        }
        if let Some(into) = record.uid("merged_into") {
            self.merges.push((uid, into));
            return Outcome::Done;
        }
        let id = match local.or(mapped) {
            Some(id) => id,
            None => {
                let id = DocumentId(self.library.next_library_item_id.max(1));
                self.library.next_library_item_id = id.0 + 1;
                self.map(SyncKind::Item, id.0, uid);
                id
            }
        };
        let item = LibraryItem {
            id,
            kind: record.get("kind").unwrap_or_default(),
            title: record.text("title"),
            creators: record.get("creators").unwrap_or_default(),
            year: record.get("year"),
            publication: record.text("publication"),
            publisher: record.text("publisher"),
            abstract_text: record.text("abstract"),
            subject: record.text("subject"),
            language: record.text("language"),
            identifiers: record.get("identifiers").unwrap_or_default(),
            embedded_keywords: record.get("keywords").unwrap_or_default(),
            favorite: record.get("favorite").unwrap_or(false),
            status: record.get("status").unwrap_or_default(),
            work_note: record.text("work_note"),
            created: record.get("created").unwrap_or(0),
            updated: record.get("updated").unwrap_or(0),
            trashed_at: record.get("trashed_at"),
        };
        let library = &mut self.library;
        match library
            .library_items
            .iter_mut()
            .find(|known| known.id == id)
        {
            Some(known) => *known = item,
            None => library.library_items.push(item),
        }

        let tags = record
            .members("tags/")
            .iter()
            .map(|name| self.tag(name, None))
            .collect::<Vec<_>>();
        let collections = record
            .members("collections/")
            .iter()
            .filter_map(|uid| Uuid::parse_str(uid).ok())
            .filter_map(|uid| self.collection(uid))
            .collect::<BTreeSet<_>>();
        let trash_origin = record.fields.get("trash_origin").and_then(|origin| {
            let parent = origin
                .get("parent")
                .and_then(Value::as_str)
                .and_then(|uid| Uuid::parse_str(uid).ok())
                .and_then(|uid| self.collection(uid));
            let path = serde_json::from_value(origin.get("path")?.clone()).ok()?;
            Some(LibraryTrashLocation {
                parent_id: parent,
                path,
            })
        });
        let library = &mut self.library;
        library.library_item_tags.retain(|row| row.item_id != id);
        library
            .library_item_tags
            .extend(tags.into_iter().map(|tag_id| LibraryItemTag {
                item_id: id,
                tag_id,
            }));
        library
            .library_item_collections
            .retain(|row| row.item_id != id);
        library
            .library_item_collections
            .extend(
                collections
                    .iter()
                    .map(|collection_id| LibraryItemCollection {
                        item_id: id,
                        collection_id: *collection_id,
                    }),
            );
        let parent_id = library
            .library_item_locations
            .iter()
            .find(|location| location.item_id == id)
            .and_then(|location| location.parent_id)
            .filter(|parent| collections.contains(parent))
            .or_else(|| collections.first().copied());
        library
            .library_item_locations
            .retain(|location| location.item_id != id);
        library.library_item_locations.push(LibraryItemLocation {
            item_id: id,
            parent_id,
            trash_origin,
        });
        Outcome::Done
    }

    fn apply_attachment(&mut self, uid: Uuid, record: &Winning) -> Outcome {
        let local = self
            .local(SyncKind::Attachment, uid)
            .map(LibraryAttachmentId)
            .filter(|id| {
                self.library
                    .library_attachments
                    .iter()
                    .any(|attachment| attachment.id == *id)
            });
        if record.deleted {
            if let Some(id) = local {
                self.library
                    .library_attachments
                    .retain(|attachment| attachment.id != id);
            }
            return Outcome::Done;
        }
        let Some(item_id) = record.uid("item").and_then(|item| self.item(item)) else {
            return Outcome::Parked;
        };
        let Some(sha) = record.get::<String>("sha256").and_then(|sha| unhex(&sha)) else {
            return Outcome::Parked;
        };
        let size = record.get::<u64>("size").unwrap_or(0);
        let storage = record
            .get::<AttachmentStorage>("storage")
            .unwrap_or_default();
        let primary = record.get::<bool>("primary").unwrap_or(true);
        if let Some(id) = local {
            if let Some(attachment) = self
                .library
                .library_attachments
                .iter_mut()
                .find(|attachment| attachment.id == id)
            {
                attachment.item_id = item_id;
                attachment.primary = primary;
            }
            return Outcome::Done;
        }
        // The same file is here already, under another item: the items
        // are one, merged once both are in.
        if let Some(existing) = self
            .library
            .library_attachments
            .iter()
            .find(|attachment| attachment.content_sha256 == sha && attachment.file_size == size)
        {
            let (existing_id, existing_item) = (existing.id, existing.item_id);
            self.alias(SyncKind::Attachment, uid, existing_id.0);
            if existing_item != item_id {
                self.same_file.push((existing_item, item_id));
            }
            return Outcome::Aliased;
        }
        let path = match storage {
            AttachmentStorage::Managed => {
                let (Some(root), Some(file)) = (&self.root, record.get::<String>("file")) else {
                    return Outcome::Parked;
                };
                // Only `files/<key>/<name>`: never a path out of the folder.
                let parts = file.split('/').collect::<Vec<_>>();
                if parts.len() != 3
                    || parts[0] != "files"
                    || parts
                        .iter()
                        .any(|part| part.is_empty() || *part == ".." || *part == ".")
                {
                    return Outcome::Parked;
                }
                root.join(parts[0]).join(parts[1]).join(parts[2])
            }
            AttachmentStorage::Linked => match record.get::<String>("origin_path") {
                Some(path) if Path::new(&path).is_absolute() => PathBuf::from(path),
                _ => return Outcome::Parked,
            },
        };
        let id = LibraryAttachmentId(self.library.next_library_attachment_id.max(1));
        self.library.next_library_attachment_id = id.0 + 1;
        let storage_key = match storage {
            AttachmentStorage::Managed => path
                .parent()
                .and_then(Path::file_name)
                .map(|key| key.to_string_lossy().into_owned())
                .unwrap_or_default(),
            AttachmentStorage::Linked => linked_storage_key(id),
        };
        let modified = fs::metadata(&path).ok();
        if modified.is_none() && storage == AttachmentStorage::Managed {
            self.report.files_expected.push(path.clone());
        }
        self.library.library_attachments.push(LibraryAttachment {
            id,
            item_id,
            path,
            storage_key,
            content_sha256: sha,
            format: record.get("format").unwrap_or(DocumentFormat::Pdf),
            file_size: size,
            modified: modified.as_ref().map_or(0, modified_seconds),
            primary,
            storage,
            file_identity: None,
        });
        self.map(SyncKind::Attachment, id.0, uid);
        Outcome::Done
    }

    fn apply_view(&mut self, uid: Uuid, record: &Winning) -> Outcome {
        let local = self.local(SyncKind::SavedView, uid).map(LibrarySavedViewId);
        if record.deleted {
            if let Some(id) = local {
                self.library
                    .library_saved_views
                    .retain(|view| view.id != id);
            }
            return Outcome::Done;
        }
        let filter = record.fields.get("filter").cloned().unwrap_or(Value::Null);
        let field = |name: &str| filter.get(name).cloned().unwrap_or(Value::Null);
        let tag_names: Vec<String> = part(&filter, "tags");
        let tag_ids = tag_names.iter().map(|name| self.tag(name, None)).collect();
        let collection_id = field("collection")
            .as_str()
            .and_then(|uid| Uuid::parse_str(uid).ok())
            .and_then(|uid| self.collection(uid));
        let filter = LibraryFilter {
            text: part(&filter, "text"),
            tag_ids,
            kinds: part(&filter, "kinds"),
            statuses: part(&filter, "statuses"),
            favorite: part(&filter, "favorite"),
            missing: part(&filter, "missing"),
            year_min: part(&filter, "year_min"),
            year_max: part(&filter, "year_max"),
            trash: part(&filter, "trash"),
            collection_id,
            unfiled: part(&filter, "unfiled"),
        };
        let id = match local {
            Some(id) => id,
            None => {
                let id = LibrarySavedViewId(self.library.next_library_saved_view_id.max(1));
                self.library.next_library_saved_view_id = id.0 + 1;
                self.map(SyncKind::SavedView, id.0, uid);
                id
            }
        };
        let view = LibrarySavedView {
            id,
            name: record.text("name"),
            filter,
            sort: record.get("sort").unwrap_or_default(),
            view_mode: record.get("view_mode").unwrap_or_default(),
            created: record.get("created").unwrap_or(0),
            updated: record.get("updated").unwrap_or(0),
        };
        let views = &mut self.library.library_saved_views;
        match views.iter_mut().find(|known| known.id == id) {
            Some(known) => *known = view,
            None => views.push(view),
        }
        Outcome::Done
    }

    fn pair(&self, key: &str) -> Option<LibraryItemPair> {
        let (a, b) = key.split_once('|')?;
        Some(LibraryItemPair::new(self.item_text(a)?, self.item_text(b)?))
    }

    fn apply_pair(&mut self, key: &str, record: &Winning) -> Outcome {
        let Some(pair) = self.pair(key) else {
            return Outcome::Parked;
        };
        let library = &mut self.library;
        for (list, field) in [
            (&mut library.library_related_items, "related"),
            (&mut library.library_distinct_items, "distinct"),
        ] {
            list.retain(|known| *known != pair);
            if !record.deleted && record.get::<bool>(field).unwrap_or(false) {
                list.push(pair);
            }
        }
        Outcome::Done
    }

    /// The path of the file an annotation is on, by its SHA-256, the
    /// item's own copy first.
    fn file_path(&self, record: &Winning) -> Option<PathBuf> {
        let sha = record.get::<String>("file").and_then(|sha| unhex(&sha))?;
        let item = record.uid("item").and_then(|item| self.item(item));
        let mut matching = self
            .library
            .library_attachments
            .iter()
            .filter(|attachment| attachment.content_sha256 == sha)
            .collect::<Vec<_>>();
        matching.sort_by_key(|attachment| Some(attachment.item_id) != item);
        matching
            .first()
            .map(|attachment| normalize_path(&attachment.path))
    }

    fn apply_annotation(&mut self, kind: &str, uid: Uuid, record: &Winning) -> Outcome {
        let sync_kind = match kind {
            kinds::HIGHLIGHT => SyncKind::Highlight,
            kinds::NOTE => SyncKind::Note,
            _ => SyncKind::Bookmark,
        };
        let local = self.local(sync_kind, uid);
        let library = &mut self.library;
        let known_path = local.and_then(|id| match sync_kind {
            SyncKind::Highlight => library
                .highlights
                .iter()
                .find(|known| known.id == id)
                .map(|known| known.path.clone()),
            SyncKind::Note => library
                .notes
                .iter()
                .find(|known| known.id == id)
                .map(|known| known.path.clone()),
            _ => library
                .bookmarks
                .iter()
                .find(|known| known.id == id)
                .map(|known| known.path.clone()),
        });
        if record.deleted {
            if let (Some(id), Some(path)) = (local, known_path) {
                match sync_kind {
                    SyncKind::Highlight => library.highlights.retain(|known| known.id != id),
                    SyncKind::Note => library.notes.retain(|known| known.id != id),
                    _ => library.bookmarks.retain(|known| known.id != id),
                }
                self.annotation_paths.insert(path);
                self.rows(sync_kind).insert(id);
            }
            return Outcome::Done;
        }
        let Some(path) = self.file_path(record) else {
            return Outcome::Parked;
        };
        let library = &mut self.library;
        let id = match local {
            Some(id) => id,
            None => {
                let next = match sync_kind {
                    SyncKind::Highlight => &mut library.next_highlight_id,
                    SyncKind::Note => &mut library.next_note_id,
                    _ => &mut library.next_bookmark_id,
                };
                let id = (*next).max(1);
                *next = id + 1;
                self.map(sync_kind, id, uid);
                id
            }
        };
        let geometry = record
            .fields
            .get("geometry")
            .cloned()
            .unwrap_or(Value::Null);
        let part = |name: &str| geometry.get(name).cloned().unwrap_or(Value::Null);
        let rects = serde_json::from_value(part("rects")).unwrap_or_default();
        let semantic_range = serde_json::from_value(part("range")).unwrap_or_default();
        let text = serde_json::from_value(part("text")).unwrap_or_default();
        let color = record.get("color").unwrap_or(HighlightColor::Yellow);
        let created = record.get("created").unwrap_or(0);
        let library = &mut self.library;
        match sync_kind {
            SyncKind::Highlight => {
                let highlight = Highlight {
                    id,
                    path: path.clone(),
                    color,
                    rects,
                    semantic_range,
                    text,
                    note: record.text("note"),
                    created,
                };
                library.highlights.retain(|known| known.id != id);
                library.highlights.push(highlight);
            }
            SyncKind::Note => {
                let note = Note {
                    id,
                    path: path.clone(),
                    color,
                    rects,
                    semantic_range,
                    text,
                    body: record.text("body"),
                    created,
                };
                library.notes.retain(|known| known.id != id);
                library.notes.push(note);
            }
            _ => {
                let Some(position) = record.get::<DocumentPosition>("position") else {
                    return Outcome::Parked;
                };
                let bookmark = Bookmark {
                    id,
                    path: path.clone(),
                    title: record.get("title"),
                    position,
                    created,
                };
                library.bookmarks.retain(|known| known.id != id);
                library.bookmarks.push(bookmark);
            }
        }
        if let Some(previous) = known_path {
            self.annotation_paths.insert(previous);
        }
        self.annotation_paths.insert(path);
        self.rows(sync_kind).insert(id);
        Outcome::Done
    }

    fn rows(&mut self, kind: SyncKind) -> &mut BTreeSet<u64> {
        match kind {
            SyncKind::Highlight => &mut self.highlights,
            SyncKind::Note => &mut self.notes,
            _ => &mut self.bookmarks,
        }
    }

    fn apply_enrichment(&mut self, uid: Uuid, record: &Winning) -> Outcome {
        let Some(id) = self.item(uid) else {
            return if record.deleted {
                Outcome::Done
            } else {
                Outcome::Parked
            };
        };
        match record.get::<ItemEnrichment>("value") {
            Some(enrichment) if !record.deleted => {
                self.ai.enrichments.insert(id, enrichment);
            }
            _ => {
                self.ai.enrichments.remove(&id);
            }
        }
        self.ai_items.insert(id);
        Outcome::Done
    }

    fn apply_assignment(&mut self, key: &str, record: &Winning) -> Outcome {
        let Some((item, target)) = key.split_once('|') else {
            return Outcome::Done;
        };
        let Some(id) = self.item_text(item) else {
            return if record.deleted {
                Outcome::Done
            } else {
                Outcome::Parked
            };
        };
        let target = if let Some(tag) = target.strip_prefix("tag:") {
            AssignmentTarget::Tag(self.tag(tag, None))
        } else if let Some(collection) = target.strip_prefix("col:") {
            match Uuid::parse_str(collection)
                .ok()
                .and_then(|uid| self.collection(uid))
            {
                Some(collection) => AssignmentTarget::Collection(collection),
                None if record.deleted => return Outcome::Done,
                None => return Outcome::Parked,
            }
        } else if let Some(path) = target.strip_prefix("new:") {
            match serde_json::from_str(path) {
                Ok(path) => AssignmentTarget::NewCollection(path),
                Err(_) => return Outcome::Done,
            }
        } else {
            return Outcome::Done;
        };
        let entries = self.ai.assignments.entry(id).or_default();
        entries.retain(|assignment| assignment.target != target);
        if !record.deleted {
            let origin = match record.fields.get("origin") {
                Some(origin) if origin.get("enrichment").is_some() => {
                    let token = origin["enrichment"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string();
                    let own = format!("{}:", self.device);
                    let run = match token.strip_prefix(&own).and_then(|run| run.parse().ok()) {
                        Some(run) => run,
                        None => match self.ids.run_for(&token) {
                            Some(run) => run,
                            None => {
                                let run = self.ai.next_run.max(1);
                                self.ai.next_run = run + 1;
                                self.ids.insert_run(run, token.clone());
                                self.id_rows.run(run, &token);
                                run
                            }
                        },
                    };
                    AssignmentOrigin::Enrichment { run }
                }
                Some(origin) if origin.get("agent").is_some() => AssignmentOrigin::Agent {
                    name: origin["agent"].as_str().unwrap_or_default().to_string(),
                },
                Some(origin) => AssignmentOrigin::Import {
                    source: origin
                        .get("import")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                },
                None => AssignmentOrigin::Import {
                    source: String::new(),
                },
            };
            let entries = self.ai.assignments.entry(id).or_default();
            entries.push(AiAssignment {
                target,
                label: record.text("label"),
                origin,
                state: record.get("state").unwrap_or(AssignmentState::Applied),
                at: record.get("at").unwrap_or(0),
            });
        }
        self.ai_items.insert(id);
        Outcome::Done
    }

    fn apply_metadata(&mut self, uid: Uuid, record: &Winning) -> Outcome {
        let Some(id) = self.item(uid) else {
            return if record.deleted {
                Outcome::Done
            } else {
                Outcome::Parked
            };
        };
        if record.deleted {
            self.ai.metadata.remove(&id);
        } else {
            self.ai.metadata.insert(
                id,
                MetadataRecord {
                    source: record.get("source").unwrap_or_default(),
                    matched_identifier: record.get("matched_identifier"),
                    fetched_at: record.get("fetched_at").unwrap_or(0),
                    locked_fields: record.get("locked_fields").unwrap_or_default(),
                    possible_duplicate_of: record
                        .uid("possible_duplicate_of")
                        .and_then(|uid| self.item(uid)),
                    external_ids: record.get("external_ids").unwrap_or_default(),
                },
            );
        }
        self.ai_items.insert(id);
        Outcome::Done
    }

    fn items_of(&self, value: Option<&Value>) -> Vec<DocumentId> {
        value
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter_map(|uid| self.item_text(uid))
            .collect()
    }

    fn apply_page(&mut self, uid: Uuid, record: &Winning) -> Outcome {
        let local = self.local(SyncKind::Page, uid);
        if record.deleted {
            if let Some(id) = local
                && self.knowledge.pages.remove(&id).is_some()
            {
                self.pages.insert(id);
            }
            return Outcome::Done;
        }
        let topic = record.fields.get("topic").cloned().unwrap_or(Value::Null);
        let topic = if let Some(tag) = topic.get("tag").and_then(Value::as_str) {
            PageTopic::Tag(self.tag(tag, None))
        } else if let Some(items) = topic.get("items") {
            PageTopic::Items(self.items_of(Some(items)))
        } else {
            PageTopic::Asked(
                topic
                    .get("asked")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            )
        };
        let id = match local {
            Some(id) => id,
            None => {
                let id = self
                    .knowledge
                    .pages
                    .keys()
                    .next_back()
                    .map_or(1, |id| id + 1)
                    .max(self.ids.highest(SyncKind::Page) + 1);
                self.map(SyncKind::Page, id, uid);
                id
            }
        };
        let known = self.knowledge.pages.get(&id);
        let page = PageRecord {
            id,
            title: record.text("title"),
            relative_path: record.text("path"),
            topic,
            sources: self.items_of(record.fields.get("sources")),
            // What this machine wrote, if it did: the file is the same.
            frontmatter_sha256: known.map_or([0; 32], |page| page.frontmatter_sha256),
            blocks: known.map(|page| page.blocks.clone()).unwrap_or_default(),
            modified: known.map_or(0, |page| page.modified),
            written_at: record.get("written_at").unwrap_or(0),
            model: record.text("model"),
            tokens_in: record.get("tokens_in").unwrap_or(0),
            tokens_out: record.get("tokens_out").unwrap_or(0),
            previous: known.and_then(|page| page.previous.clone()),
            unreviewed: record.get("unreviewed").unwrap_or(false),
        };
        self.knowledge.pages.insert(id, page);
        self.pages.insert(id);
        Outcome::Done
    }

    fn apply_suggestion(&mut self, key: &str, record: &Winning) -> Outcome {
        let Some(pair) = self.pair(key) else {
            return if record.deleted {
                Outcome::Done
            } else {
                Outcome::Parked
            };
        };
        let row = self.knowledge.suggestions.entry(pair.0.0).or_default();
        row.retain(|suggestion| suggestion.other != pair.1);
        if !record.deleted {
            row.push(RelatedSuggestion {
                other: pair.1,
                score: record.get("score").unwrap_or(0.0),
                state: record.get("state").unwrap_or(AssignmentState::Suggested),
                at: record.get("at").unwrap_or(0),
            });
        }
        self.suggestion_rows.insert(pair.0.0);
        Outcome::Done
    }

    fn apply_topic(&mut self, key: &str, record: &Winning) -> Outcome {
        let local = if let Some(tag) = key.strip_prefix("tag:") {
            match self.tag_named(tag) {
                Some(tag) => format!("tag:{}", tag.0),
                None => return Outcome::Parked,
            }
        } else if let Some(items) = key.strip_prefix("items:") {
            let mut ids = Vec::new();
            for uid in items.split(',') {
                match self.item_text(uid) {
                    Some(id) => ids.push(id.0),
                    None => return Outcome::Parked,
                }
            }
            ids.sort_unstable();
            format!(
                "items:{}",
                ids.iter().map(u64::to_string).collect::<Vec<_>>().join(",")
            )
        } else {
            key.to_string()
        };
        let dismissed = !record.deleted && record.get::<bool>("dismissed").unwrap_or(false);
        if dismissed {
            self.knowledge.dismissed.insert(local.clone());
        } else {
            self.knowledge.dismissed.remove(&local);
        }
        self.topics.insert(local, dismissed);
        Outcome::Done
    }

    fn apply_vault_note(&mut self, uid: Uuid, record: &Winning) -> Outcome {
        let Some(id) = self.item(uid) else {
            return if record.deleted {
                Outcome::Done
            } else {
                Outcome::Parked
            };
        };
        if record.deleted {
            self.vault_notes.remove(&id);
        } else {
            let path = record.text("path");
            let note = self
                .vault_notes
                .entry(id)
                .or_insert_with(|| VaultNoteRecord {
                    relative_path: path.clone(),
                    written_tags: Vec::new(),
                    frontmatter_sha256: [0; 32],
                    blocks: Vec::new(),
                    modified: 0,
                    written_at: 0,
                });
            note.relative_path = path;
            note.written_at = record.get("written_at").unwrap_or(note.written_at);
        }
        self.vault_rows.insert(id);
        Outcome::Done
    }

    fn apply(&mut self, key: &RecordKey, record: &Winning) -> Outcome {
        let uid = Uuid::parse_str(&key.key).ok();
        match (key.kind.as_str(), uid) {
            (kinds::TAG, _) => self.apply_tag(&key.key, record),
            (kinds::COLLECTION, Some(uid)) => self.apply_collection(uid, record),
            (kinds::ITEM, Some(uid)) => self.apply_item(uid, record),
            (kinds::ATTACHMENT, Some(uid)) => self.apply_attachment(uid, record),
            (kinds::VIEW, Some(uid)) => self.apply_view(uid, record),
            (kinds::PAIR, _) => self.apply_pair(&key.key, record),
            (kinds::HIGHLIGHT | kinds::NOTE | kinds::BOOKMARK, Some(uid)) => {
                self.apply_annotation(&key.kind, uid, record)
            }
            (kinds::ENRICHMENT, Some(uid)) => self.apply_enrichment(uid, record),
            (kinds::ASSIGNMENT, _) => self.apply_assignment(&key.key, record),
            (kinds::METADATA, Some(uid)) => self.apply_metadata(uid, record),
            (kinds::PAGE, Some(uid)) => self.apply_page(uid, record),
            (kinds::SUGGESTION, _) => self.apply_suggestion(&key.key, record),
            (kinds::TOPIC, _) => self.apply_topic(&key.key, record),
            (kinds::VAULT_NOTE, Some(uid)) => self.apply_vault_note(uid, record),
            // A kind this version does not know: kept in the registers.
            _ => Outcome::Done,
        }
    }
}

/// Records in the order they can be made: what is referred to first.
fn rank(kind: &str) -> usize {
    [
        kinds::TAG,
        kinds::COLLECTION,
        kinds::ITEM,
        kinds::ATTACHMENT,
        kinds::VIEW,
        kinds::PAIR,
        kinds::HIGHLIGHT,
        kinds::NOTE,
        kinds::BOOKMARK,
        kinds::ENRICHMENT,
        kinds::ASSIGNMENT,
        kinds::METADATA,
        kinds::PAGE,
        kinds::SUGGESTION,
        kinds::TOPIC,
        kinds::VAULT_NOTE,
    ]
    .iter()
    .position(|known| *known == kind)
    .unwrap_or(usize::MAX)
}

impl LibraryStore {
    /// Merges other machines' changes and makes them into this library's
    /// records; records waiting for something are tried again each time.
    pub fn apply_sync(&mut self, batches: &[Batch]) -> Result<ApplyReport> {
        let Some(reader) = self.sync_reader() else {
            return Ok(ApplyReport::default());
        };
        let mut registers = reader.registers()?;
        let mut clock = self.sync_clock();
        let wall = wall_ms();
        let mut state = self.sync.state.clone();
        let mut report = ApplyReport::default();
        let mut changed = BTreeSet::new();
        for batch in batches {
            for change in &batch.changes {
                if let Observed::Ahead(by) = clock.observe(change.hlc, wall) {
                    let ahead = report.clock_ahead.entry(change.hlc.device).or_default();
                    *ahead = (*ahead).max(by);
                }
                let record = change.record();
                let stored = registers.entry(record.clone()).or_default();
                for (field, value) in &change.fields {
                    if wins(
                        change.hlc,
                        stored.fields.get(field).map(|register| register.hlc),
                    ) {
                        stored.fields.insert(
                            field.clone(),
                            Register {
                                hlc: change.hlc,
                                value: value.clone(),
                            },
                        );
                        report.fields += 1;
                        changed.insert(record.clone());
                    }
                }
            }
            batch.advance(&mut state.marks, state.device);
        }
        // What changed, and whatever is still waiting.
        let mut pending = registers
            .iter()
            .filter(|(key, stored)| {
                changed.contains(*key)
                    || (!stored.materialized && !stored.aliased && !Winning::of(stored).deleted)
            })
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        pending.sort_by(|a, b| rank(&a.kind).cmp(&rank(&b.kind)).then(a.cmp(b)));

        let mut work = Work {
            library: self.library.clone(),
            ai: self.ai_catalog.clone(),
            knowledge: self.knowledge.clone(),
            vault_notes: self.vault_notes.clone(),
            ids: self.sync.ids.clone(),
            id_rows: IdRows::default(),
            root: self.managed_library_root.clone(),
            device: state.device,
            ai_items: BTreeSet::new(),
            annotation_paths: BTreeSet::new(),
            highlights: BTreeSet::new(),
            notes: BTreeSet::new(),
            bookmarks: BTreeSet::new(),
            pages: BTreeSet::new(),
            suggestion_rows: BTreeSet::new(),
            topics: BTreeMap::new(),
            vault_rows: BTreeSet::new(),
            merges: Vec::new(),
            removals: Vec::new(),
            same_file: Vec::new(),
            report,
        };
        // Collections wait for their parents: go round until none moves.
        let mut written = BTreeSet::new();
        loop {
            let mut progressed = false;
            for key in &pending {
                if written.contains(key) {
                    continue;
                }
                let stored = registers
                    .get_mut(key)
                    .expect("pending records are registered");
                match work.apply(key, &Winning::of(stored)) {
                    Outcome::Done => {
                        stored.materialized = true;
                        written.insert(key.clone());
                        progressed = true;
                    }
                    Outcome::Aliased => {
                        stored.aliased = true;
                        written.insert(key.clone());
                        progressed = true;
                    }
                    Outcome::Parked => {}
                }
            }
            if !progressed || written.len() == pending.len() {
                break;
            }
        }
        // What still waits stays unmaterialized, so it is never taken for
        // a removal, and is tried again next time.
        for key in &pending {
            if !written.contains(key)
                && let Some(stored) = registers.get_mut(key)
            {
                stored.materialized = false;
                work.report.parked += 1;
            }
        }
        let touched = pending
            .iter()
            .filter(|key| changed.contains(*key) || written.contains(*key))
            .collect::<Vec<_>>();
        work.library.sanitize_catalog();
        work.library.reserve_synced_ids(&work.ids);
        state.hlc = Some(clock.last());

        let previous = (
            std::mem::replace(&mut self.library, work.library),
            std::mem::replace(&mut self.ai_catalog, work.ai),
            std::mem::replace(&mut self.knowledge, work.knowledge),
            std::mem::replace(&mut self.vault_notes, work.vault_notes),
        );
        let written = self.write_applied(
            &registers,
            &touched,
            &work.id_rows,
            &state,
            Rows {
                ai_items: &work.ai_items,
                highlights: &work.highlights,
                notes: &work.notes,
                bookmarks: &work.bookmarks,
                pages: &work.pages,
                suggestions: &work.suggestion_rows,
                topics: &work.topics,
                vault: &work.vault_rows,
            },
        );
        if let Err(error) = written {
            (
                self.library,
                self.ai_catalog,
                self.knowledge,
                self.vault_notes,
            ) = previous;
            return Err(error);
        }
        self.sync.ids = work.ids;
        self.sync.state = state;
        self.sync.generation += 1;
        self.catalog_revision = self.catalog_revision.wrapping_add(1);
        self.bookmarks_revision = self.bookmarks_revision.wrapping_add(1);
        for path in &work.annotation_paths {
            let history = self
                .document_edit_histories
                .entry(normalize_path(path))
                .or_default();
            // Undo would replay edits over what another machine changed.
            history.undo.clear();
            history.redo.clear();
            history.revision.annotations = history.revision.annotations.wrapping_add(1);
        }
        self.sanitize_ai_catalog();

        let mut report = work.report;
        self.finish_apply(work.merges, work.same_file, work.removals, &mut report);
        Ok(report)
    }

    #[allow(clippy::too_many_arguments)]
    fn write_applied(
        &self,
        registers: &HashMap<RecordKey, StoredRecordV1>,
        touched: &[&RecordKey],
        id_rows: &IdRows,
        state: &SyncState,
        rows: Rows,
    ) -> Result<()> {
        self.synced_write();
        let Some(database) = &self.database else {
            return Ok(());
        };
        let root = self
            .managed_library_root
            .as_deref()
            .ok_or_else(|| anyhow!("managed library storage is unavailable"))?;
        let write_txn = database.begin_write()?;
        storage::write_catalog_tables(&write_txn, &self.library, root)?;
        {
            let mut table = write_txn.open_table(HIGHLIGHTS_V2_TABLE)?;
            for id in rows.highlights {
                match self.library.highlights.iter().find(|known| known.id == *id) {
                    Some(highlight) => {
                        table
                            .insert(*id, encode(&StoredHighlightV2::from(highlight))?.as_slice())?;
                    }
                    None => {
                        table.remove(*id)?;
                    }
                }
            }
            let mut table = write_txn.open_table(NOTES_V2_TABLE)?;
            for id in rows.notes {
                match self.library.notes.iter().find(|known| known.id == *id) {
                    Some(note) => {
                        table.insert(*id, encode(&StoredNoteV2::from(note))?.as_slice())?;
                    }
                    None => {
                        table.remove(*id)?;
                    }
                }
            }
            let mut table = write_txn.open_table(BOOKMARKS_V2_TABLE)?;
            for id in rows.bookmarks {
                match self.library.bookmarks.iter().find(|known| known.id == *id) {
                    Some(bookmark) => {
                        table.insert(*id, encode(&StoredBookmarkV2::from(bookmark))?.as_slice())?;
                    }
                    None => {
                        table.remove(*id)?;
                    }
                }
            }
        }
        let ai_items = rows.ai_items.iter().copied().collect::<Vec<_>>();
        self.write_ai_rows_in(&write_txn, &ai_items)?;
        {
            let mut table = write_txn.open_table(knowledge::LIBRARY_PAGES_V1_TABLE)?;
            for id in rows.pages {
                match self.knowledge.pages.get(id) {
                    Some(page) => {
                        let encoded = encode(&StoredPageV1 {
                            version: KNOWLEDGE_STORAGE_VERSION,
                            page: page.clone(),
                        })?;
                        table.insert(*id, encoded.as_slice())?;
                    }
                    None => {
                        table.remove(*id)?;
                    }
                }
            }
            let mut table =
                write_txn.open_table(knowledge::LIBRARY_RELATED_SUGGESTIONS_V1_TABLE)?;
            for id in rows.suggestions {
                let encoded = encode(&StoredSuggestionsV1 {
                    version: KNOWLEDGE_STORAGE_VERSION,
                    suggestions: self
                        .knowledge
                        .suggestions
                        .get(id)
                        .cloned()
                        .unwrap_or_default(),
                })?;
                table.insert(*id, encoded.as_slice())?;
            }
            let mut table = write_txn.open_table(knowledge::LIBRARY_PAGE_TOPICS_V1_TABLE)?;
            for (key, dismissed) in rows.topics {
                if *dismissed {
                    table.insert(key.as_str(), current_timestamp())?;
                } else {
                    table.remove(key.as_str())?;
                }
            }
        }
        for id in rows.vault {
            vault_notes::write_vault_note_in(&write_txn, *id, self.vault_notes.get(id))?;
        }
        id_rows.write_in(&write_txn)?;
        {
            let mut table = write_txn.open_table(SYNC_REGISTERS_V1_TABLE)?;
            for key in touched {
                if let Some(stored) = registers.get(*key) {
                    let mut stored = stored.clone();
                    stored.version = SYNC_STORAGE_VERSION;
                    table.insert(row_key(key).as_str(), encode(&stored)?.as_slice())?;
                }
            }
        }
        write_state_in(&write_txn, state)?;
        write_txn.commit()?;
        Ok(())
    }

    /// Merges and removals, through the library's own operations.
    fn finish_apply(
        &mut self,
        merges: Vec<(Uuid, Uuid)>,
        same_file: Vec<(DocumentId, DocumentId)>,
        removals: Vec<(DocumentId, usize)>,
        report: &mut ApplyReport,
    ) {
        let item = |store: &Self, uid: Uuid| {
            store
                .sync
                .ids
                .local(SyncKind::Item, uid)
                .map(DocumentId)
                .filter(|id| store.library_item(*id).is_some())
        };
        let mut pairs = Vec::new();
        for (absorbed, into) in merges {
            if let (Some(absorbed), Some(into)) = (item(self, absorbed), item(self, into))
                && absorbed != into
            {
                pairs.push((into, absorbed));
            }
        }
        // The same file added on two machines: kept under the lower uid,
        // which both machines choose alike.
        for (a, b) in same_file {
            let (Some(ua), Some(ub)) = (
                self.sync.ids.uid(SyncKind::Item, a.0),
                self.sync.ids.uid(SyncKind::Item, b.0),
            ) else {
                continue;
            };
            pairs.push(if ua <= ub { (a, b) } else { (b, a) });
        }
        for (keep, absorb) in pairs {
            if self.library_item(keep).is_none() || self.library_item(absorb).is_none() {
                continue;
            }
            if let Err(error) = self.merge_items(keep, absorb) {
                report.conflicts.push(format!(
                    "Could not merge two items another machine made one: {error:#}"
                ));
            }
        }
        for (id, annotations) in removals {
            let Some(item) = self.library_item(id).cloned() else {
                continue;
            };
            let removed = (|| -> Result<()> {
                if item.trashed_at.is_none() {
                    self.trash_library_items(&[id])?;
                }
                self.permanently_delete_library_items(&[id])?;
                Ok(())
            })();
            match removed {
                Err(error) => report.conflicts.push(format!(
                    "Could not remove “{}”, deleted on another machine: {error:#}",
                    item.title
                )),
                Ok(()) if annotations > 0 => report.conflicts.push(format!(
                    "“{}” was deleted on another machine; its {annotations} highlights and notes here are no longer in the library.",
                    item.title
                )),
                Ok(()) => {}
            }
        }
    }
}

/// The rows an apply writes beside the catalog.
struct Rows<'a> {
    ai_items: &'a BTreeSet<DocumentId>,
    highlights: &'a BTreeSet<u64>,
    notes: &'a BTreeSet<u64>,
    bookmarks: &'a BTreeSet<u64>,
    pages: &'a BTreeSet<u64>,
    suggestions: &'a BTreeSet<u64>,
    topics: &'a BTreeMap<String, bool>,
    vault: &'a BTreeSet<DocumentId>,
}
