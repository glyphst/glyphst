//! The records as the journal carries them. Every reference is a uid (or a
//! tag's normalized name), never a local number; paths are relative to
//! the library folder, never absolute; and what only means something on
//! one machine (reading positions, file times, the undo text of a page)
//! is left out. Two machines with the same library produce the same
//! records whatever their local numbers.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use glyphst_sync::{DeviceId, Fields, RecordKey};
use serde_json::{Value, json};

use super::*;

/// The kinds of record in the journal.
pub mod kinds {
    pub const ITEM: &str = "item";
    pub const ATTACHMENT: &str = "attachment";
    pub const TAG: &str = "tag";
    pub const COLLECTION: &str = "collection";
    pub const VIEW: &str = "view";
    /// Two items related, or kept apart, by their uids in order.
    pub const PAIR: &str = "pair";
    pub const HIGHLIGHT: &str = "highlight";
    pub const NOTE: &str = "note";
    pub const BOOKMARK: &str = "bookmark";
    pub const ENRICHMENT: &str = "enrichment";
    /// An AI assignment: `<item uid>|<target>`.
    pub const ASSIGNMENT: &str = "assignment";
    pub const METADATA: &str = "metadata";
    pub const VAULT_NOTE: &str = "vault-note";
    pub const PAGE: &str = "page";
    /// A related item suggested: `<uid>|<uid>`, in order.
    pub const SUGGESTION: &str = "suggestion";
    /// A page topic dismissed.
    pub const TOPIC: &str = "topic";
}

/// What an export is worked out from, copied from the store.
#[derive(Clone, Debug)]
pub struct SyncSnapshot {
    pub(super) items: Vec<LibraryItem>,
    pub(super) attachments: Vec<LibraryAttachment>,
    pub(super) tags: Vec<LibraryTag>,
    pub(super) item_tags: Vec<LibraryItemTag>,
    pub(super) views: Vec<LibrarySavedView>,
    pub(super) collections: Vec<LibraryCollection>,
    pub(super) locations: Vec<LibraryItemLocation>,
    pub(super) item_collections: Vec<LibraryItemCollection>,
    pub(super) tag_colors: Vec<LibraryTagColor>,
    pub(super) related: Vec<LibraryItemPair>,
    pub(super) distinct: Vec<LibraryItemPair>,
    pub(super) merges: Vec<LibraryItemMerge>,
    pub(super) highlights: Vec<Highlight>,
    pub(super) notes: Vec<Note>,
    pub(super) bookmarks: Vec<Bookmark>,
    pub(super) ai: ai_catalog::AiCatalogState,
    pub(super) pages: BTreeMap<u64, PageRecord>,
    pub(super) suggestions: BTreeMap<u64, Vec<RelatedSuggestion>>,
    pub(super) dismissed: BTreeSet<String>,
    pub(super) vault_notes: HashMap<DocumentId, VaultNoteRecord>,
    pub(super) ids: SyncIds,
    pub(super) device: DeviceId,
    pub(super) root: Option<PathBuf>,
    /// Annotation paths not an attachment's as written, resolved.
    pub(super) resolved_paths: HashMap<PathBuf, PathBuf>,
    pub(super) generation: u64,
}

impl LibraryStore {
    /// A copy of what syncs, every record given a uid first: cheap enough
    /// for the UI thread; [`canonical`] and the diff run elsewhere.
    pub fn sync_snapshot(&mut self) -> Result<SyncSnapshot> {
        self.assign_sync_ids()?;
        let library = &self.library;
        let attachment_paths = library
            .library_attachments
            .iter()
            .map(|attachment| attachment.path.as_path())
            .collect::<HashSet<_>>();
        // Annotations name the path as it was opened; when that is not an
        // attachment's path as written (a symlink, `..`), resolve both.
        let mut resolved_paths = HashMap::new();
        let annotation_paths = library
            .highlights
            .iter()
            .map(|highlight| &highlight.path)
            .chain(library.notes.iter().map(|note| &note.path))
            .chain(library.bookmarks.iter().map(|bookmark| &bookmark.path));
        for path in annotation_paths {
            if !attachment_paths.contains(path.as_path()) && !resolved_paths.contains_key(path) {
                resolved_paths.insert(path.clone(), normalize_path(path));
            }
        }
        if !resolved_paths.is_empty() {
            for attachment in &library.library_attachments {
                let normalized = normalize_path(&attachment.path);
                if normalized != attachment.path {
                    resolved_paths.insert(normalized, attachment.path.clone());
                }
            }
        }
        Ok(SyncSnapshot {
            items: library.library_items.clone(),
            attachments: library.library_attachments.clone(),
            tags: library.library_tags.clone(),
            item_tags: library.library_item_tags.clone(),
            views: library.library_saved_views.clone(),
            collections: library.library_collections.clone(),
            locations: library.library_item_locations.clone(),
            item_collections: library.library_item_collections.clone(),
            tag_colors: library.library_tag_colors.clone(),
            related: library.library_related_items.clone(),
            distinct: library.library_distinct_items.clone(),
            merges: library.library_item_merges.clone(),
            highlights: library.highlights.clone(),
            notes: library.notes.clone(),
            bookmarks: library.bookmarks.clone(),
            ai: self.ai_catalog.clone(),
            pages: self.knowledge.pages.clone(),
            suggestions: self.knowledge.suggestions.clone(),
            dismissed: self.knowledge.dismissed.clone(),
            vault_notes: self.vault_notes.clone(),
            ids: self.sync.ids.clone(),
            device: self.sync.state.device,
            root: self.managed_library_root.clone(),
            resolved_paths,
            generation: self.sync.generation,
        })
    }
}

fn value<T: Serialize>(value: &T) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// `a|b` with the two uids in order: a pair's key.
pub(in crate::library) fn pair_key(a: Uuid, b: Uuid) -> String {
    let (a, b) = if a <= b { (a, b) } else { (b, a) };
    format!("{a}|{b}")
}

struct Names<'a> {
    snapshot: &'a SyncSnapshot,
    tags: HashMap<LibraryTagId, &'a LibraryTag>,
}

impl Names<'_> {
    fn uid(&self, kind: SyncKind, local: u64) -> Option<Uuid> {
        self.snapshot.ids.uid(kind, local)
    }

    fn item(&self, id: DocumentId) -> Option<Uuid> {
        self.uid(SyncKind::Item, id.0)
    }

    fn collection(&self, id: LibraryCollectionId) -> Option<Uuid> {
        self.uid(SyncKind::Collection, id.0)
    }

    fn tag(&self, id: LibraryTagId) -> Option<&str> {
        self.tags.get(&id).map(|tag| tag.normalized_name.as_str())
    }

    fn items(&self, ids: &[DocumentId]) -> Vec<String> {
        ids.iter()
            .filter_map(|id| self.item(*id))
            .map(|uid| uid.to_string())
            .collect()
    }
}

/// Every record that syncs, by key.
pub fn canonical(snapshot: &SyncSnapshot) -> BTreeMap<RecordKey, Fields> {
    let names = Names {
        snapshot,
        tags: snapshot.tags.iter().map(|tag| (tag.id, tag)).collect(),
    };
    let mut records = BTreeMap::new();
    let mut put = |kind: &str, key: String, fields: Fields| {
        records.insert(RecordKey::new(kind, key), fields);
    };

    // Items, with their tags and collections as fields.
    let mut memberships: HashMap<DocumentId, Fields> = HashMap::new();
    for row in &snapshot.item_tags {
        if let Some(tag) = names.tag(row.tag_id) {
            memberships
                .entry(row.item_id)
                .or_default()
                .insert(format!("tags/{tag}"), json!(true));
        }
    }
    for row in &snapshot.item_collections {
        if let Some(collection) = names.collection(row.collection_id) {
            memberships
                .entry(row.item_id)
                .or_default()
                .insert(format!("collections/{collection}"), json!(true));
        }
    }
    let locations = snapshot
        .locations
        .iter()
        .map(|location| (location.item_id, location))
        .collect::<HashMap<_, _>>();
    for item in &snapshot.items {
        let Some(uid) = names.item(item.id) else {
            continue;
        };
        let trash_origin = locations
            .get(&item.id)
            .and_then(|location| location.trash_origin.as_ref())
            .map(|origin| {
                json!({
                    "parent": origin.parent_id.and_then(|id| names.collection(id)),
                    "path": origin.path,
                })
            });
        let mut fields = Fields::from([
            ("kind".into(), value(&item.kind)),
            ("title".into(), json!(item.title)),
            ("creators".into(), value(&item.creators)),
            ("year".into(), json!(item.year)),
            ("publication".into(), json!(item.publication)),
            ("publisher".into(), json!(item.publisher)),
            ("abstract".into(), json!(item.abstract_text)),
            ("subject".into(), json!(item.subject)),
            ("language".into(), json!(item.language)),
            ("identifiers".into(), value(&item.identifiers)),
            ("keywords".into(), json!(item.embedded_keywords)),
            ("favorite".into(), json!(item.favorite)),
            ("status".into(), value(&item.status)),
            ("work_note".into(), json!(item.work_note)),
            ("created".into(), json!(item.created)),
            ("updated".into(), json!(item.updated)),
            ("trashed_at".into(), json!(item.trashed_at)),
            ("trash_origin".into(), trash_origin.unwrap_or(Value::Null)),
        ]);
        fields.extend(memberships.remove(&item.id).unwrap_or_default());
        put(kinds::ITEM, uid.to_string(), fields);
    }
    // A merged-away item points at the one it went into.
    for merge in &snapshot.merges {
        if let (Some(absorbed), Some(into)) = (names.item(merge.absorbed), names.item(merge.into)) {
            put(
                kinds::ITEM,
                absorbed.to_string(),
                Fields::from([
                    ("merged_into".into(), json!(into.to_string())),
                    ("merged_at".into(), json!(merge.at)),
                ]),
            );
        }
    }

    // Files: a copied file by its place in the library folder, a linked
    // one by where it was (its path differs on every machine).
    let mut files: HashMap<&Path, (&LibraryAttachment, String)> = HashMap::new();
    for attachment in &snapshot.attachments {
        let (Some(uid), Some(item)) = (
            names.uid(SyncKind::Attachment, attachment.id.0),
            names.item(attachment.item_id),
        ) else {
            continue;
        };
        let sha = hex(&attachment.content_sha256);
        let mut fields = Fields::from([
            ("item".into(), json!(item.to_string())),
            ("sha256".into(), json!(sha)),
            ("size".into(), json!(attachment.file_size)),
            ("format".into(), value(&attachment.format)),
            ("primary".into(), json!(attachment.primary)),
            ("storage".into(), value(&attachment.storage)),
        ]);
        if attachment.is_linked() {
            fields.insert(
                "origin_path".into(),
                json!(attachment.path.to_string_lossy()),
            );
        } else {
            let relative = snapshot
                .root
                .as_deref()
                .and_then(|root| attachment.path.strip_prefix(root).ok())
                .map(|relative| {
                    relative
                        .components()
                        .map(|part| part.as_os_str().to_string_lossy())
                        .collect::<Vec<_>>()
                        .join("/")
                });
            fields.insert("file".into(), json!(relative));
        }
        put(kinds::ATTACHMENT, uid.to_string(), fields);
        files.insert(attachment.path.as_path(), (attachment, sha));
    }
    let file_of = |path: &Path| {
        files.get(path).or_else(|| {
            snapshot.resolved_paths.get(path).and_then(|resolved| {
                files.get(resolved.as_path()).or_else(|| {
                    snapshot
                        .resolved_paths
                        .get(resolved)
                        .and_then(|attachment| files.get(attachment.as_path()))
                })
            })
        })
    };

    // Tags by name, with their colours.
    let colors = snapshot
        .tag_colors
        .iter()
        .map(|color| (color.tag_id, color.color))
        .collect::<HashMap<_, _>>();
    for tag in &snapshot.tags {
        put(
            kinds::TAG,
            tag.normalized_name.clone(),
            Fields::from([
                ("name".into(), json!(tag.name)),
                (
                    "color".into(),
                    colors.get(&tag.id).map_or(Value::Null, value),
                ),
            ]),
        );
    }

    for collection in &snapshot.collections {
        let Some(uid) = names.collection(collection.id) else {
            continue;
        };
        put(
            kinds::COLLECTION,
            uid.to_string(),
            Fields::from([
                ("name".into(), json!(collection.name)),
                (
                    "parent".into(),
                    json!(collection.parent_id.and_then(|id| names.collection(id))),
                ),
                ("created".into(), json!(collection.created)),
                ("updated".into(), json!(collection.updated)),
            ]),
        );
    }

    for view in &snapshot.views {
        let Some(uid) = names.uid(SyncKind::SavedView, view.id.0) else {
            continue;
        };
        let filter = &view.filter;
        let filter = json!({
            "text": filter.text,
            "tags": filter.tag_ids.iter().filter_map(|id| names.tag(*id)).collect::<Vec<_>>(),
            "kinds": filter.kinds,
            "statuses": filter.statuses,
            "favorite": filter.favorite,
            "missing": filter.missing,
            "year_min": filter.year_min,
            "year_max": filter.year_max,
            "trash": filter.trash,
            "collection": filter.collection_id.and_then(|id| names.collection(id)),
            "unfiled": filter.unfiled,
        });
        put(
            kinds::VIEW,
            uid.to_string(),
            Fields::from([
                ("name".into(), json!(view.name)),
                ("filter".into(), filter),
                ("sort".into(), value(&view.sort)),
                ("view_mode".into(), value(&view.view_mode)),
                ("created".into(), json!(view.created)),
                ("updated".into(), json!(view.updated)),
            ]),
        );
    }

    let mut pairs: BTreeMap<String, Fields> = BTreeMap::new();
    for (list, field) in [
        (&snapshot.related, "related"),
        (&snapshot.distinct, "distinct"),
    ] {
        for pair in list {
            if let (Some(a), Some(b)) = (names.item(pair.0), names.item(pair.1)) {
                pairs
                    .entry(pair_key(a, b))
                    .or_default()
                    .insert(field.into(), json!(true));
            }
        }
    }
    for (key, fields) in pairs {
        put(kinds::PAIR, key, fields);
    }

    // Annotations, by the file they are on. Those on files outside the
    // library stay on this machine.
    for highlight in &snapshot.highlights {
        let (Some(uid), Some((attachment, sha))) = (
            names.uid(SyncKind::Highlight, highlight.id),
            file_of(&highlight.path),
        ) else {
            continue;
        };
        put(
            kinds::HIGHLIGHT,
            uid.to_string(),
            Fields::from([
                ("file".into(), json!(sha)),
                ("item".into(), json!(names.item(attachment.item_id))),
                ("color".into(), value(&highlight.color)),
                (
                    "geometry".into(),
                    json!({
                        "rects": highlight.rects,
                        "range": highlight.semantic_range,
                        "text": highlight.text,
                    }),
                ),
                ("note".into(), json!(highlight.note)),
                ("created".into(), json!(highlight.created)),
            ]),
        );
    }
    for note in &snapshot.notes {
        let (Some(uid), Some((attachment, sha))) =
            (names.uid(SyncKind::Note, note.id), file_of(&note.path))
        else {
            continue;
        };
        put(
            kinds::NOTE,
            uid.to_string(),
            Fields::from([
                ("file".into(), json!(sha)),
                ("item".into(), json!(names.item(attachment.item_id))),
                ("color".into(), value(&note.color)),
                (
                    "geometry".into(),
                    json!({
                        "rects": note.rects,
                        "range": note.semantic_range,
                        "text": note.text,
                    }),
                ),
                ("body".into(), json!(note.body)),
                ("created".into(), json!(note.created)),
            ]),
        );
    }
    for bookmark in &snapshot.bookmarks {
        let (Some(uid), Some((attachment, sha))) = (
            names.uid(SyncKind::Bookmark, bookmark.id),
            file_of(&bookmark.path),
        ) else {
            continue;
        };
        put(
            kinds::BOOKMARK,
            uid.to_string(),
            Fields::from([
                ("file".into(), json!(sha)),
                ("item".into(), json!(names.item(attachment.item_id))),
                ("title".into(), json!(bookmark.title)),
                ("position".into(), value(&bookmark.position)),
                ("created".into(), json!(bookmark.created)),
            ]),
        );
    }

    // What the AI and lookups did.
    for (id, enrichment) in &snapshot.ai.enrichments {
        if let Some(uid) = names.item(*id) {
            put(
                kinds::ENRICHMENT,
                uid.to_string(),
                Fields::from([("value".into(), value(enrichment))]),
            );
        }
    }
    for (id, assignments) in &snapshot.ai.assignments {
        let Some(item) = names.item(*id) else {
            continue;
        };
        for assignment in assignments {
            let target = match &assignment.target {
                AssignmentTarget::Tag(tag) => names.tag(*tag).map(|tag| format!("tag:{tag}")),
                AssignmentTarget::Collection(collection) => names
                    .collection(*collection)
                    .map(|collection| format!("col:{collection}")),
                AssignmentTarget::NewCollection(path) => Some(format!("new:{}", value(path))),
            };
            let Some(target) = target else {
                continue;
            };
            let origin = match &assignment.origin {
                AssignmentOrigin::Enrichment { run } => {
                    json!({ "enrichment": snapshot.ids.run_token(snapshot.device, *run) })
                }
                AssignmentOrigin::Agent { name } => json!({ "agent": name }),
                AssignmentOrigin::Import { source } => json!({ "import": source }),
            };
            put(
                kinds::ASSIGNMENT,
                format!("{item}|{target}"),
                Fields::from([
                    ("label".into(), json!(assignment.label)),
                    ("origin".into(), origin),
                    ("state".into(), value(&assignment.state)),
                    ("at".into(), json!(assignment.at)),
                ]),
            );
        }
    }
    for (id, record) in &snapshot.ai.metadata {
        let Some(uid) = names.item(*id) else {
            continue;
        };
        put(
            kinds::METADATA,
            uid.to_string(),
            Fields::from([
                ("source".into(), value(&record.source)),
                (
                    "matched_identifier".into(),
                    json!(record.matched_identifier),
                ),
                ("fetched_at".into(), json!(record.fetched_at)),
                ("locked_fields".into(), value(&record.locked_fields)),
                (
                    "possible_duplicate_of".into(),
                    json!(record.possible_duplicate_of.and_then(|id| names.item(id))),
                ),
                ("external_ids".into(), json!(record.external_ids)),
            ]),
        );
    }

    // The knowledge layer.
    for (id, page) in &snapshot.pages {
        let Some(uid) = names.uid(SyncKind::Page, *id) else {
            continue;
        };
        let topic = match &page.topic {
            PageTopic::Tag(tag) => json!({ "tag": names.tag(*tag) }),
            PageTopic::Items(items) => json!({ "items": names.items(items) }),
            PageTopic::Asked(text) => json!({ "asked": text }),
        };
        put(
            kinds::PAGE,
            uid.to_string(),
            Fields::from([
                ("title".into(), json!(page.title)),
                ("path".into(), json!(page.relative_path)),
                ("topic".into(), topic),
                ("sources".into(), json!(names.items(&page.sources))),
                ("model".into(), json!(page.model)),
                ("tokens_in".into(), json!(page.tokens_in)),
                ("tokens_out".into(), json!(page.tokens_out)),
                ("written_at".into(), json!(page.written_at)),
                ("unreviewed".into(), json!(page.unreviewed)),
            ]),
        );
    }
    for (lower, suggestions) in &snapshot.suggestions {
        let Some(a) = names.item(DocumentId(*lower)) else {
            continue;
        };
        for suggestion in suggestions {
            let Some(b) = names.item(suggestion.other) else {
                continue;
            };
            put(
                kinds::SUGGESTION,
                pair_key(a, b),
                Fields::from([
                    ("score".into(), json!(suggestion.score)),
                    ("state".into(), value(&suggestion.state)),
                    ("at".into(), json!(suggestion.at)),
                ]),
            );
        }
    }
    for key in &snapshot.dismissed {
        if let Some(key) = canonical_topic(&names, key) {
            put(
                kinds::TOPIC,
                key,
                Fields::from([("dismissed".into(), json!(true))]),
            );
        }
    }

    for (id, note) in &snapshot.vault_notes {
        if let Some(uid) = names.item(*id) {
            put(
                kinds::VAULT_NOTE,
                uid.to_string(),
                Fields::from([
                    ("path".into(), json!(note.relative_path)),
                    ("written_at".into(), json!(note.written_at)),
                ]),
            );
        }
    }
    records
}

/// A dismissed topic's key without local numbers: `tag:<id>` names the
/// tag, `items:<ids>` the items' uids in order.
fn canonical_topic(names: &Names, key: &str) -> Option<String> {
    if let Some(tag) = key.strip_prefix("tag:") {
        let tag = tag.parse::<u64>().ok()?;
        return Some(format!("tag:{}", names.tag(LibraryTagId(tag))?));
    }
    if let Some(items) = key.strip_prefix("items:") {
        let ids = items
            .split(',')
            .filter_map(|id| id.parse::<u64>().ok().map(DocumentId))
            .collect::<Vec<_>>();
        let mut uids = names.items(&ids);
        if uids.len() != ids.len() {
            return None;
        }
        uids.sort();
        return Some(format!("items:{}", uids.join(",")));
    }
    Some(key.to_string())
}
