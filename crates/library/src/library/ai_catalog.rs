//! What the AI and the metadata lookups did to each catalog item: the tags
//! and collections they applied or suggested (and what the user made of
//! them), the item's summary, and where its metadata came from.
//!
//! This lives beside [`Library`] rather than in it, so a catalog change
//! does not copy it, and is written one item at a time.

use super::storage_helpers::{decode, encode};
use super::*;

pub(super) const LIBRARY_AI_ASSIGNMENTS_V1_TABLE: TableDefinition<u64, &[u8]> =
    TableDefinition::new("library_ai_assignments_v1");
pub(super) const LIBRARY_ENRICHMENTS_V1_TABLE: TableDefinition<u64, &[u8]> =
    TableDefinition::new("library_enrichments_v1");
pub(super) const LIBRARY_METADATA_RECORDS_V1_TABLE: TableDefinition<u64, &[u8]> =
    TableDefinition::new("library_metadata_records_v1");
const AI_CATALOG_STORAGE_VERSION: u8 = 1;

/// A tag or collection an assistant gave an item.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiAssignment {
    pub target: AssignmentTarget,
    /// The tag's name or the collection's path, as shown, so a rejected one
    /// is recognised when it is proposed again.
    pub label: String,
    pub origin: AssignmentOrigin,
    pub state: AssignmentState,
    pub at: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AssignmentTarget {
    Tag(LibraryTagId),
    Collection(LibraryCollectionId),
    /// A collection that does not exist yet, by its path.
    NewCollection(Vec<String>),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AssignmentOrigin {
    /// The library model's pass over the item.
    Enrichment { run: u64 },
    /// An assistant's tool call.
    Agent { name: String },
    /// Brought in by an import (automatic tags from Zotero).
    Import { source: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AssignmentState {
    /// On the item, not yet looked at.
    Applied,
    /// Proposed only (a new collection).
    Suggested,
    /// Kept by the user.
    Accepted,
    /// Removed or dismissed by the user; never proposed again.
    Rejected,
}

impl AssignmentState {
    pub fn needs_review(self) -> bool {
        matches!(self, Self::Applied | Self::Suggested)
    }
}

/// The library model's pass over an item.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ItemEnrichment {
    pub prompt_version: u32,
    pub model: String,
    /// Of the prompt version, model, metadata and text it was given; the
    /// same input is not sent again unless forced.
    pub input_sha256: [u8; 32],
    pub status: EnrichmentStatus,
    pub attempts: u32,
    pub finished_at: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub tldr: String,
    pub key_points: Vec<String>,
    pub suggested_tags: Vec<String>,
    pub suggested_collection: Option<Vec<String>>,
    pub kind: Option<LibraryItemKind>,
    pub language: Option<String>,
    pub confidence: Option<f32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EnrichmentStatus {
    Done,
    Failed { error: String },
}

/// Where an item's metadata came from.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MetadataRecord {
    pub source: MetadataSource,
    /// The identifier the lookup matched, such as `doi:10.1/x`.
    pub matched_identifier: Option<String>,
    pub fetched_at: u64,
    /// Fields the user edited; lookups and the AI leave them alone.
    pub locked_fields: Vec<MetadataField>,
    pub possible_duplicate_of: Option<DocumentId>,
    /// Ids in other managers, such as `zotero:1/ABCD2345`, so an import run
    /// again updates instead of adding.
    pub external_ids: Vec<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum MetadataSource {
    #[default]
    Filename,
    Embedded,
    Crossref,
    Arxiv,
    OpenAlex,
    OpenLibrary,
    Llm,
    Manual,
    Zotero,
}

impl MetadataSource {
    pub fn label(self) -> &'static str {
        match self {
            Self::Filename => "filename",
            Self::Embedded => "file",
            Self::Crossref => "crossref",
            Self::Arxiv => "arxiv",
            Self::OpenAlex => "openalex",
            Self::OpenLibrary => "openlibrary",
            Self::Llm => "ai",
            Self::Manual => "you",
            Self::Zotero => "zotero",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MetadataField {
    Kind,
    Title,
    Creators,
    Year,
    Publication,
    Publisher,
    Abstract,
    Subject,
    Language,
    Identifiers,
    Keywords,
}

impl MetadataField {
    pub const ALL: [Self; 11] = [
        Self::Kind,
        Self::Title,
        Self::Creators,
        Self::Year,
        Self::Publication,
        Self::Publisher,
        Self::Abstract,
        Self::Subject,
        Self::Language,
        Self::Identifiers,
        Self::Keywords,
    ];

    /// Whether the two items differ in this field.
    pub fn differs(self, left: &LibraryItem, right: &LibraryItem) -> bool {
        match self {
            Self::Kind => left.kind != right.kind,
            Self::Title => left.title != right.title,
            Self::Creators => left.creators != right.creators,
            Self::Year => left.year != right.year,
            Self::Publication => left.publication != right.publication,
            Self::Publisher => left.publisher != right.publisher,
            Self::Abstract => left.abstract_text != right.abstract_text,
            Self::Subject => left.subject != right.subject,
            Self::Language => left.language != right.language,
            Self::Identifiers => left.identifiers != right.identifiers,
            Self::Keywords => left.embedded_keywords != right.embedded_keywords,
        }
    }

    /// Copies this field from `source` into `target`.
    pub fn copy(self, source: &LibraryItem, target: &mut LibraryItem) {
        match self {
            Self::Kind => target.kind = source.kind,
            Self::Title => target.title = source.title.clone(),
            Self::Creators => target.creators = source.creators.clone(),
            Self::Year => target.year = source.year,
            Self::Publication => target.publication = source.publication.clone(),
            Self::Publisher => target.publisher = source.publisher.clone(),
            Self::Abstract => target.abstract_text = source.abstract_text.clone(),
            Self::Subject => target.subject = source.subject.clone(),
            Self::Language => target.language = source.language.clone(),
            Self::Identifiers => target.identifiers = source.identifiers.clone(),
            Self::Keywords => target.embedded_keywords = source.embedded_keywords.clone(),
        }
    }
}

/// What one enrichment run gives an item.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EnrichmentApply {
    pub run: u64,
    /// Tags to apply, by name; new names become tags only with
    /// `create_tags`.
    pub tags: Vec<String>,
    pub create_tags: bool,
    /// Existing collections to file the item into, by path.
    pub collections: Vec<Vec<String>>,
    /// A collection to propose, by path; it is created only when accepted.
    pub suggested_collection: Option<Vec<String>>,
    /// Metadata to fill into fields that are empty and not locked.
    pub metadata: Option<LibraryItem>,
    pub enrichment: Option<ItemEnrichment>,
}

/// What [`LibraryStore::apply_enrichment`] changed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EnrichmentApplied {
    pub tags_applied: Vec<String>,
    pub collections_applied: Vec<String>,
    pub suggested: Vec<String>,
    pub fields_filled: Vec<MetadataField>,
}

/// One item's rows in the AI catalog, moved together by a merge.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(super) struct AiRows {
    pub(super) metadata: Option<MetadataRecord>,
    pub(super) assignments: Vec<AiAssignment>,
    pub(super) enrichment: Option<ItemEnrichment>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct AiCatalogState {
    assignments: HashMap<DocumentId, Vec<AiAssignment>>,
    enrichments: HashMap<DocumentId, ItemEnrichment>,
    metadata: HashMap<DocumentId, MetadataRecord>,
    next_run: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredAiAssignmentsV1 {
    version: u8,
    entries: Vec<AiAssignment>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredEnrichmentV1 {
    version: u8,
    enrichment: ItemEnrichment,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredMetadataRecordV1 {
    version: u8,
    record: MetadataRecord,
}

pub(super) fn ensure_ai_catalog_tables(write_txn: &redb::WriteTransaction) -> Result<()> {
    write_txn.open_table(LIBRARY_AI_ASSIGNMENTS_V1_TABLE)?;
    write_txn.open_table(LIBRARY_ENRICHMENTS_V1_TABLE)?;
    write_txn.open_table(LIBRARY_METADATA_RECORDS_V1_TABLE)?;
    Ok(())
}

pub(super) fn load_ai_catalog(database: &Database) -> Result<AiCatalogState> {
    let mut state = AiCatalogState::default();
    let read_txn = database.begin_read()?;
    {
        let table = read_txn.open_table(LIBRARY_AI_ASSIGNMENTS_V1_TABLE)?;
        for entry in table.iter()? {
            let (id, value) = entry?;
            match decode::<StoredAiAssignmentsV1>(value.value()).and_then(|stored| {
                require_storage_version(stored.version, AI_CATALOG_STORAGE_VERSION)?;
                Ok(stored.entries)
            }) {
                Ok(entries) => {
                    state.assignments.insert(DocumentId(id.value()), entries);
                }
                Err(error) => eprintln!("failed to decode ai assignments: {error:#}"),
            }
        }
    }
    {
        let table = read_txn.open_table(LIBRARY_ENRICHMENTS_V1_TABLE)?;
        for entry in table.iter()? {
            let (id, value) = entry?;
            match decode::<StoredEnrichmentV1>(value.value()).and_then(|stored| {
                require_storage_version(stored.version, AI_CATALOG_STORAGE_VERSION)?;
                Ok(stored.enrichment)
            }) {
                Ok(enrichment) => {
                    state.enrichments.insert(DocumentId(id.value()), enrichment);
                }
                Err(error) => eprintln!("failed to decode item enrichment: {error:#}"),
            }
        }
    }
    {
        let table = read_txn.open_table(LIBRARY_METADATA_RECORDS_V1_TABLE)?;
        for entry in table.iter()? {
            let (id, value) = entry?;
            match decode::<StoredMetadataRecordV1>(value.value()).and_then(|stored| {
                require_storage_version(stored.version, AI_CATALOG_STORAGE_VERSION)?;
                Ok(stored.record)
            }) {
                Ok(record) => {
                    state.metadata.insert(DocumentId(id.value()), record);
                }
                Err(error) => eprintln!("failed to decode metadata record: {error:#}"),
            }
        }
    }
    state.next_run = state
        .assignments
        .values()
        .flatten()
        .filter_map(|assignment| match assignment.origin {
            AssignmentOrigin::Enrichment { run } => Some(run),
            _ => None,
        })
        .max()
        .unwrap_or(0)
        .saturating_add(1);
    Ok(state)
}

impl AiCatalogState {
    /// Drops what refers to items, tags or collections that are gone.
    /// Returns the items whose rows changed.
    fn sanitize(&mut self, library: &Library) -> Vec<DocumentId> {
        let items = library
            .library_items
            .iter()
            .map(|item| item.id)
            .collect::<HashSet<_>>();
        let tags = library
            .library_tags
            .iter()
            .map(|tag| tag.id)
            .collect::<HashSet<_>>();
        let collections = library
            .library_collections
            .iter()
            .map(|collection| collection.id)
            .collect::<HashSet<_>>();
        let mut changed = Vec::new();
        self.assignments.retain(|id, entries| {
            if !items.contains(id) {
                changed.push(*id);
                return false;
            }
            let before = entries.len();
            entries.retain(|assignment| match &assignment.target {
                AssignmentTarget::Tag(tag) => tags.contains(tag),
                AssignmentTarget::Collection(collection) => collections.contains(collection),
                AssignmentTarget::NewCollection(path) => !path.is_empty(),
            });
            if entries.len() != before {
                changed.push(*id);
            }
            !entries.is_empty()
        });
        self.enrichments.retain(|id, _| {
            let keep = items.contains(id);
            if !keep {
                changed.push(*id);
            }
            keep
        });
        self.metadata.retain(|id, record| {
            let keep = items.contains(id);
            if !keep {
                changed.push(*id);
            } else if record
                .possible_duplicate_of
                .is_some_and(|other| !items.contains(&other))
            {
                record.possible_duplicate_of = None;
                changed.push(*id);
            }
            keep
        });
        changed.sort_by_key(|id| id.0);
        changed.dedup();
        changed
    }
}

fn normalize_label(label: &str) -> String {
    label
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn path_label(path: &[String]) -> String {
    path.join(" / ")
}

impl LibraryStore {
    pub fn ai_assignments(&self, id: DocumentId) -> &[AiAssignment] {
        self.ai_catalog
            .assignments
            .get(&id)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// Whether the item has AI changes the user has not looked at.
    pub fn item_needs_review(&self, id: DocumentId) -> bool {
        self.ai_assignments(id)
            .iter()
            .any(|assignment| assignment.state.needs_review())
    }

    pub fn item_enrichment(&self, id: DocumentId) -> Option<&ItemEnrichment> {
        self.ai_catalog.enrichments.get(&id)
    }

    pub fn metadata_record(&self, id: DocumentId) -> Option<&MetadataRecord> {
        self.ai_catalog.metadata.get(&id)
    }

    /// The item whose metadata record has this external id.
    pub fn item_with_external_id(&self, external_id: &str) -> Option<DocumentId> {
        self.ai_catalog
            .metadata
            .iter()
            .find(|(_, record)| record.external_ids.iter().any(|id| id == external_id))
            .map(|(id, _)| *id)
    }

    /// A number for a new enrichment run.
    pub fn next_enrichment_run(&mut self) -> u64 {
        let run = self.ai_catalog.next_run.max(1);
        self.ai_catalog.next_run = run.saturating_add(1);
        run
    }

    pub fn set_item_enrichment(
        &mut self,
        id: DocumentId,
        enrichment: ItemEnrichment,
    ) -> Result<()> {
        if self.library_item(id).is_none() {
            return Err(anyhow!("library item {} does not exist", id.0));
        }
        let previous = self.ai_catalog.enrichments.insert(id, enrichment);
        if let Err(error) = self.write_ai_rows(&[id]) {
            match previous {
                Some(previous) => self.ai_catalog.enrichments.insert(id, previous),
                None => self.ai_catalog.enrichments.remove(&id),
            };
            return Err(error);
        }
        Ok(())
    }

    pub fn set_metadata_record(&mut self, id: DocumentId, record: MetadataRecord) -> Result<()> {
        if self.library_item(id).is_none() {
            return Err(anyhow!("library item {} does not exist", id.0));
        }
        let previous = self.ai_catalog.metadata.insert(id, record);
        if let Err(error) = self.write_ai_rows(&[id]) {
            match previous {
                Some(previous) => self.ai_catalog.metadata.insert(id, previous),
                None => self.ai_catalog.metadata.remove(&id),
            };
            return Err(error);
        }
        Ok(())
    }

    /// Marks fields as the user's, so lookups and the AI leave them alone.
    pub fn lock_metadata_fields(&mut self, id: DocumentId, fields: &[MetadataField]) -> Result<()> {
        let mut record = self.metadata_record(id).cloned().unwrap_or_default();
        for field in fields {
            if !record.locked_fields.contains(field) {
                record.locked_fields.push(*field);
            }
        }
        self.set_metadata_record(id, record)
    }

    /// Edits an item's metadata as the user: the fields that changed are
    /// locked against lookups and the AI.
    pub fn edit_library_item_metadata(&mut self, item: LibraryItem) -> Result<()> {
        let before = self
            .library_item(item.id)
            .cloned()
            .ok_or_else(|| anyhow!("library item {} does not exist", item.id.0))?;
        let changed = MetadataField::ALL
            .into_iter()
            .filter(|field| field.differs(&before, &item))
            .collect::<Vec<_>>();
        let id = item.id;
        self.update_library_item_metadata(item)?;
        if !changed.is_empty() {
            self.lock_metadata_fields(id, &changed)?;
        }
        Ok(())
    }

    /// Replaces an item's metadata fields (not its status, note or
    /// favorite), recording the edit.
    pub fn update_library_item_metadata(&mut self, item: LibraryItem) -> Result<()> {
        let id = item.id;
        let changed = self.persist_catalog_change(|library| {
            let existing = library
                .library_items
                .iter_mut()
                .find(|candidate| candidate.id == id)
                .ok_or_else(|| anyhow!("library item {} does not exist", id.0))?;
            let before = existing.clone();
            for field in MetadataField::ALL {
                field.copy(&item, existing);
            }
            existing.updated = current_timestamp();
            Ok((before, existing.clone()))
        })?;
        if let Some(recorder) = &mut self.edit_recorder
            && MetadataField::ALL
                .into_iter()
                .any(|field| field.differs(&changed.0, &changed.1))
        {
            recorder.push(LibraryEdit::metadata(changed.0, changed.1));
        }
        Ok(())
    }

    /// Gives an item what one enrichment run produced: tags and existing
    /// collections are applied (and marked as the AI's), a new collection is
    /// only proposed, and metadata fills empty, unlocked fields. Anything the
    /// user rejected before is left out.
    pub fn apply_enrichment(
        &mut self,
        id: DocumentId,
        apply: EnrichmentApply,
    ) -> Result<EnrichmentApplied> {
        let item = self
            .library_item(id)
            .cloned()
            .ok_or_else(|| anyhow!("library item {} does not exist", id.0))?;
        let rejected = self
            .ai_assignments(id)
            .iter()
            .filter(|assignment| assignment.state == AssignmentState::Rejected)
            .map(|assignment| normalize_label(&assignment.label))
            .collect::<HashSet<_>>();
        let origin = AssignmentOrigin::Enrichment { run: apply.run };
        let now = current_timestamp();
        let mut applied = EnrichmentApplied::default();

        // Tags.
        let current_tags = self
            .tags_for_item(id)
            .into_iter()
            .map(|tag| tag.normalized_name.clone())
            .collect::<HashSet<_>>();
        let mut tag_names = Vec::new();
        for name in &apply.tags {
            let normalized = normalize_label(name);
            if normalized.is_empty()
                || rejected.contains(&normalized)
                || current_tags.contains(&normalized)
                || tag_names
                    .iter()
                    .any(|existing: &String| normalize_label(existing) == normalized)
            {
                continue;
            }
            let exists = self
                .library_tags()
                .iter()
                .any(|tag| tag.normalized_name == normalized);
            if exists || apply.create_tags {
                tag_names.push(name.trim().to_string());
            }
        }

        // Collections that exist, by path.
        let current_collections = item_collection_ids_of(self, id);
        let mut collection_ids = Vec::new();
        for path in &apply.collections {
            let label = path_label(path);
            if rejected.contains(&normalize_label(&label)) {
                continue;
            }
            if let Some(collection_id) = self.collection_at_path(path)
                && !current_collections.contains(&collection_id)
                && !collection_ids.contains(&collection_id)
            {
                collection_ids.push(collection_id);
                applied.collections_applied.push(label);
            }
        }
        let suggestion = apply.suggested_collection.as_ref().and_then(|path| {
            let label = path_label(path);
            (!path.is_empty()
                && !rejected.contains(&normalize_label(&label))
                && self.collection_at_path(path).is_none()
                && !self.ai_assignments(id).iter().any(|assignment| {
                    assignment.target == AssignmentTarget::NewCollection(path.clone())
                }))
            .then(|| (path.clone(), label))
        });

        // Metadata, into empty fields the user has not locked.
        let locked = self
            .metadata_record(id)
            .map(|record| record.locked_fields.clone())
            .unwrap_or_default();
        // A title and kind guessed from the file name count as unset.
        let guessed = self
            .metadata_record(id)
            .is_none_or(|record| record.source == MetadataSource::Filename);
        let mut filled = item.clone();
        if let Some(source) = &apply.metadata {
            for field in MetadataField::ALL {
                let weak = guessed && matches!(field, MetadataField::Title | MetadataField::Kind);
                if locked.contains(&field)
                    || (!weak && !field_is_empty(field, &item))
                    || !field.differs(source, &item)
                {
                    continue;
                }
                if field_is_empty(field, source) {
                    continue;
                }
                field.copy(source, &mut filled);
                applied.fields_filled.push(field);
            }
        }

        let tag_ids = tag_names
            .iter()
            .map(|name| self.ensure_library_tag(name))
            .collect::<Result<Vec<_>>>()?;
        let new_tag_ids = tag_ids.clone();
        let new_collection_ids = collection_ids.clone();
        let fields_filled = applied.fields_filled.clone();
        self.persist_catalog_change(|library| {
            for tag_id in new_tag_ids {
                library.library_item_tags.push(LibraryItemTag {
                    item_id: id,
                    tag_id,
                });
            }
            for collection_id in new_collection_ids {
                library
                    .library_item_collections
                    .push(LibraryItemCollection {
                        item_id: id,
                        collection_id,
                    });
            }
            if !fields_filled.is_empty()
                && let Some(existing) = library.library_items.iter_mut().find(|item| item.id == id)
            {
                for field in &fields_filled {
                    field.copy(&filled, existing);
                }
                existing.updated = now;
            }
            Ok(())
        })?;

        let entries = self.ai_catalog.assignments.entry(id).or_default();
        for (tag_id, name) in tag_ids.into_iter().zip(&tag_names) {
            entries.push(AiAssignment {
                target: AssignmentTarget::Tag(tag_id),
                label: name.clone(),
                origin: origin.clone(),
                state: AssignmentState::Applied,
                at: now,
            });
            applied.tags_applied.push(name.clone());
        }
        for (collection_id, label) in collection_ids
            .into_iter()
            .zip(applied.collections_applied.clone())
        {
            entries.push(AiAssignment {
                target: AssignmentTarget::Collection(collection_id),
                label,
                origin: origin.clone(),
                state: AssignmentState::Applied,
                at: now,
            });
        }
        if let Some((path, label)) = suggestion {
            entries.push(AiAssignment {
                target: AssignmentTarget::NewCollection(path),
                label: label.clone(),
                origin: origin.clone(),
                state: AssignmentState::Suggested,
                at: now,
            });
            applied.suggested.push(label);
        }
        if entries.is_empty() {
            self.ai_catalog.assignments.remove(&id);
        }
        if let Some(enrichment) = apply.enrichment {
            self.ai_catalog.enrichments.insert(id, enrichment);
        }
        if !applied.fields_filled.is_empty() {
            let record = self.ai_catalog.metadata.entry(id).or_default();
            if record.source == MetadataSource::Filename {
                record.source = MetadataSource::Llm;
                record.fetched_at = now;
            }
        }
        self.write_ai_rows(&[id])?;
        Ok(applied)
    }

    /// Keeps an AI tag or collection, or creates a proposed collection and
    /// files the item there.
    pub fn accept_assignment(&mut self, id: DocumentId, target: &AssignmentTarget) -> Result<()> {
        let assignment = self
            .ai_assignments(id)
            .iter()
            .find(|assignment| &assignment.target == target)
            .cloned()
            .ok_or_else(|| anyhow!("no such suggestion for item {}", id.0))?;
        let mut accepted_target = assignment.target.clone();
        if let AssignmentTarget::NewCollection(path) = &assignment.target {
            let collection_id = self
                .ensure_library_collection_path(None, path)?
                .ok_or_else(|| anyhow!("the suggested collection has no name"))?;
            self.add_items_to_collection(&[id], collection_id)?;
            accepted_target = AssignmentTarget::Collection(collection_id);
        }
        self.set_assignment_state(id, target, AssignmentState::Accepted, Some(accepted_target))
    }

    /// Takes an AI tag or collection off the item, or dismisses a proposed
    /// collection; it is not proposed again.
    pub fn reject_assignment(&mut self, id: DocumentId, target: &AssignmentTarget) -> Result<()> {
        match target {
            AssignmentTarget::Tag(tag_id) => {
                let remaining = self
                    .tags_for_item(id)
                    .into_iter()
                    .map(|tag| tag.id)
                    .filter(|candidate| candidate != tag_id)
                    .collect::<Vec<_>>();
                self.replace_item_tags(id, &remaining)?;
            }
            AssignmentTarget::Collection(collection_id) => {
                self.remove_items_from_collection(&[id], *collection_id)?;
            }
            AssignmentTarget::NewCollection(_) => {}
        }
        self.set_assignment_state(id, target, AssignmentState::Rejected, None)
    }

    /// Rejects everything one run applied or proposed and is still unreviewed.
    pub fn undo_enrichment_run(&mut self, id: DocumentId, run: u64) -> Result<usize> {
        let targets = self
            .ai_assignments(id)
            .iter()
            .filter(|assignment| {
                assignment.origin == AssignmentOrigin::Enrichment { run }
                    && assignment.state.needs_review()
            })
            .map(|assignment| assignment.target.clone())
            .collect::<Vec<_>>();
        for target in &targets {
            self.reject_assignment(id, target)?;
        }
        Ok(targets.len())
    }

    fn set_assignment_state(
        &mut self,
        id: DocumentId,
        target: &AssignmentTarget,
        state: AssignmentState,
        new_target: Option<AssignmentTarget>,
    ) -> Result<()> {
        let previous = self.ai_catalog.assignments.get(&id).cloned();
        if let Some(entries) = self.ai_catalog.assignments.get_mut(&id) {
            for assignment in entries
                .iter_mut()
                .filter(|assignment| &assignment.target == target)
            {
                assignment.state = state;
                assignment.at = current_timestamp();
                if let Some(new_target) = &new_target {
                    assignment.target = new_target.clone();
                }
            }
        }
        if let Err(error) = self.write_ai_rows(&[id]) {
            match previous {
                Some(previous) => self.ai_catalog.assignments.insert(id, previous),
                None => self.ai_catalog.assignments.remove(&id),
            };
            return Err(error);
        }
        Ok(())
    }

    /// Records what a manual change to an item's tags means for the AI's:
    /// one taken off is rejected, one the user keeps by adding it is accepted.
    pub(super) fn note_manual_tag_change(
        &mut self,
        id: DocumentId,
        before: &[LibraryTagId],
        after: &[LibraryTagId],
    ) {
        let Some(entries) = self.ai_catalog.assignments.get_mut(&id) else {
            return;
        };
        let mut changed = false;
        for assignment in entries.iter_mut() {
            let AssignmentTarget::Tag(tag_id) = assignment.target else {
                continue;
            };
            let state = if before.contains(&tag_id) && !after.contains(&tag_id) {
                AssignmentState::Rejected
            } else if !before.contains(&tag_id)
                && after.contains(&tag_id)
                && assignment.state == AssignmentState::Rejected
            {
                AssignmentState::Accepted
            } else {
                continue;
            };
            if assignment.state != state {
                assignment.state = state;
                assignment.at = current_timestamp();
                changed = true;
            }
        }
        if changed && let Err(error) = self.write_ai_rows(&[id]) {
            eprintln!("failed to save ai tag review: {error:#}");
        }
    }

    /// Records where an imported item came from, and the tags its source
    /// added by itself (kept, marked as the import's). The caller writes
    /// the rows.
    pub(super) fn note_imported(
        &mut self,
        id: DocumentId,
        record: MetadataRecord,
        tags: &[(LibraryTagId, String)],
        source: &str,
    ) {
        self.ai_catalog.metadata.insert(id, record);
        if tags.is_empty() {
            return;
        }
        let now = current_timestamp();
        let entries = self.ai_catalog.assignments.entry(id).or_default();
        for (tag_id, label) in tags {
            if entries
                .iter()
                .any(|assignment| assignment.target == AssignmentTarget::Tag(*tag_id))
            {
                continue;
            }
            entries.push(AiAssignment {
                target: AssignmentTarget::Tag(*tag_id),
                label: label.clone(),
                origin: AssignmentOrigin::Import {
                    source: source.to_string(),
                },
                state: AssignmentState::Accepted,
                at: now,
            });
        }
    }

    pub(super) fn sanitize_ai_catalog(&mut self) {
        let changed = self.ai_catalog.sanitize(&self.library);
        if !changed.is_empty()
            && let Err(error) = self.write_ai_rows(&changed)
        {
            eprintln!("failed to save the ai catalog: {error:#}");
        }
    }

    fn collection_at_path(&self, path: &[String]) -> Option<LibraryCollectionId> {
        let mut parent = None;
        for component in path {
            let normalized = normalize_label(component);
            if normalized.is_empty() {
                continue;
            }
            parent = Some(
                self.library_collections()
                    .iter()
                    .find(|collection| {
                        collection.parent_id == parent && collection.normalized_name == normalized
                    })?
                    .id,
            );
        }
        parent
    }

    pub(super) fn write_ai_rows(&self, ids: &[DocumentId]) -> Result<()> {
        self.synced_write();
        let Some(database) = &self.database else {
            return Ok(());
        };
        let write_txn = database.begin_write()?;
        self.write_ai_rows_in(&write_txn, ids)?;
        write_txn.commit()?;
        Ok(())
    }

    /// Writes these items' rows inside a transaction someone else commits.
    pub(super) fn write_ai_rows_in(
        &self,
        write_txn: &redb::WriteTransaction,
        ids: &[DocumentId],
    ) -> Result<()> {
        self.synced_write();
        {
            let mut assignments = write_txn.open_table(LIBRARY_AI_ASSIGNMENTS_V1_TABLE)?;
            let mut enrichments = write_txn.open_table(LIBRARY_ENRICHMENTS_V1_TABLE)?;
            let mut metadata = write_txn.open_table(LIBRARY_METADATA_RECORDS_V1_TABLE)?;
            for id in ids {
                match self.ai_catalog.assignments.get(id) {
                    Some(entries) if !entries.is_empty() => {
                        let encoded = encode(&StoredAiAssignmentsV1 {
                            version: AI_CATALOG_STORAGE_VERSION,
                            entries: entries.clone(),
                        })?;
                        assignments.insert(id.0, encoded.as_slice())?;
                    }
                    _ => {
                        assignments.remove(id.0)?;
                    }
                }
                match self.ai_catalog.enrichments.get(id) {
                    Some(enrichment) => {
                        let encoded = encode(&StoredEnrichmentV1 {
                            version: AI_CATALOG_STORAGE_VERSION,
                            enrichment: enrichment.clone(),
                        })?;
                        enrichments.insert(id.0, encoded.as_slice())?;
                    }
                    None => {
                        enrichments.remove(id.0)?;
                    }
                }
                match self.ai_catalog.metadata.get(id) {
                    Some(record) => {
                        let encoded = encode(&StoredMetadataRecordV1 {
                            version: AI_CATALOG_STORAGE_VERSION,
                            record: record.clone(),
                        })?;
                        metadata.insert(id.0, encoded.as_slice())?;
                    }
                    None => {
                        metadata.remove(id.0)?;
                    }
                }
            }
        }
        Ok(())
    }

    pub(super) fn ai_rows(&self, id: DocumentId) -> AiRows {
        AiRows {
            metadata: self.ai_catalog.metadata.get(&id).cloned(),
            assignments: self.ai_assignments(id).to_vec(),
            enrichment: self.ai_catalog.enrichments.get(&id).cloned(),
        }
    }

    /// Replaces the item's rows in memory; the caller writes them.
    pub(super) fn put_ai_rows(&mut self, id: DocumentId, rows: AiRows) {
        let catalog = &mut self.ai_catalog;
        match rows.metadata {
            Some(record) => catalog.metadata.insert(id, record),
            None => catalog.metadata.remove(&id),
        };
        if rows.assignments.is_empty() {
            catalog.assignments.remove(&id);
        } else {
            catalog.assignments.insert(id, rows.assignments);
        }
        match rows.enrichment {
            Some(enrichment) => catalog.enrichments.insert(id, enrichment),
            None => catalog.enrichments.remove(&id),
        };
    }

    /// Clears `possible_duplicate_of` in memory wherever `(item, other)`
    /// matches; returns the items to write.
    pub(super) fn clear_possible_duplicates_in_memory(
        &mut self,
        matches: impl Fn(DocumentId, DocumentId) -> bool,
    ) -> Vec<DocumentId> {
        let mut changed = Vec::new();
        for (id, record) in &mut self.ai_catalog.metadata {
            if record
                .possible_duplicate_of
                .is_some_and(|other| matches(*id, other))
            {
                record.possible_duplicate_of = None;
                changed.push(*id);
            }
        }
        changed
    }

    /// Clears `possible_duplicate_of` wherever `(item, other)` matches.
    pub(super) fn clear_possible_duplicates(
        &mut self,
        matches: impl Fn(DocumentId, DocumentId) -> bool,
    ) -> Result<()> {
        let previous = self.ai_catalog.metadata.clone();
        let changed = self.clear_possible_duplicates_in_memory(matches);
        if changed.is_empty() {
            return Ok(());
        }
        if let Err(error) = self.write_ai_rows(&changed) {
            self.ai_catalog.metadata = previous;
            return Err(error);
        }
        Ok(())
    }
}

fn item_collection_ids_of(store: &LibraryStore, id: DocumentId) -> Vec<LibraryCollectionId> {
    store
        .library_item_collections()
        .iter()
        .filter(|membership| membership.item_id == id)
        .map(|membership| membership.collection_id)
        .collect()
}

pub(super) fn field_is_empty(field: MetadataField, item: &LibraryItem) -> bool {
    match field {
        // The kind always has a value; see `guessed` in `apply_enrichment`.
        MetadataField::Kind => false,
        MetadataField::Title => item.title.trim().is_empty() || item.title == "Untitled",
        MetadataField::Creators => item.creators.is_empty(),
        MetadataField::Year => item.year.is_none(),
        MetadataField::Publication => item.publication.is_empty(),
        MetadataField::Publisher => item.publisher.is_empty(),
        MetadataField::Abstract => item.abstract_text.is_empty(),
        MetadataField::Subject => item.subject.is_empty(),
        MetadataField::Language => item.language.is_empty(),
        MetadataField::Identifiers => item.identifiers == LibraryIdentifiers::default(),
        MetadataField::Keywords => item.embedded_keywords.is_empty(),
    }
}
