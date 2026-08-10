use std::cmp::Ordering;

use super::*;

pub(super) const LIBRARY_ITEMS_V1_TABLE: TableDefinition<u64, &[u8]> =
    TableDefinition::new("library_items_v1");
pub(super) const LIBRARY_ATTACHMENTS_V1_TABLE: TableDefinition<u64, &[u8]> =
    TableDefinition::new("library_attachments_v1");
pub(super) const LIBRARY_TAGS_V1_TABLE: TableDefinition<u64, &[u8]> =
    TableDefinition::new("library_tags_v1");
pub(super) const LIBRARY_ITEM_TAGS_V1_TABLE: TableDefinition<u64, &[u8]> =
    TableDefinition::new("library_item_tags_v1");
pub(super) const LIBRARY_SAVED_VIEWS_V1_TABLE: TableDefinition<u64, &[u8]> =
    TableDefinition::new("library_saved_views_v1");
pub(super) const LIBRARY_CATALOG_STORAGE_VERSION: u8 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LibraryAttachmentId(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LibraryTagId(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LibrarySavedViewId(pub u64);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum LibraryItemKind {
    #[default]
    Paper,
    Book,
    Other,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum LibraryCreatorRole {
    #[default]
    Author,
    Editor,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryCreator {
    pub name: String,
    pub role: LibraryCreatorRole,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryIdentifiers {
    pub doi: Option<String>,
    pub isbn: Option<String>,
    pub arxiv: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReadingStatus {
    #[default]
    ToRead,
    Reading,
    Read,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryItem {
    pub id: DocumentId,
    pub kind: LibraryItemKind,
    pub title: String,
    pub creators: Vec<LibraryCreator>,
    pub year: Option<i32>,
    pub publication: String,
    pub publisher: String,
    pub abstract_text: String,
    pub subject: String,
    pub language: String,
    pub identifiers: LibraryIdentifiers,
    pub embedded_keywords: Vec<String>,
    pub favorite: bool,
    pub status: ReadingStatus,
    pub work_note: String,
    pub created: u64,
    pub updated: u64,
    pub trashed_at: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryAttachment {
    pub id: LibraryAttachmentId,
    pub item_id: DocumentId,
    pub path: PathBuf,
    pub format: DocumentFormat,
    pub file_size: u64,
    pub modified: u64,
    pub primary: bool,
}

impl LibraryAttachment {
    pub fn is_missing(&self) -> bool {
        !self.path.exists()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryTag {
    pub id: LibraryTagId,
    pub name: String,
    pub normalized_name: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LibraryItemTag {
    pub item_id: DocumentId,
    pub tag_id: LibraryTagId,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum LibraryViewMode {
    Grid,
    // Keep Grouped and Table in the former List and Details ordinal slots so
    // existing bincode catalog values continue to decode with their intended
    // presentation semantics.
    Grouped,
    #[default]
    Table,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum LibraryTrashFilter {
    #[default]
    Active,
    Trashed,
    All,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryFilter {
    pub text: String,
    pub tag_ids: Vec<LibraryTagId>,
    pub kinds: Vec<LibraryItemKind>,
    pub statuses: Vec<ReadingStatus>,
    pub favorite: Option<bool>,
    pub missing: Option<bool>,
    pub year_min: Option<i32>,
    pub year_max: Option<i32>,
    pub trash: LibraryTrashFilter,
}

impl Default for LibraryFilter {
    fn default() -> Self {
        Self {
            text: String::new(),
            tag_ids: Vec::new(),
            kinds: Vec::new(),
            statuses: Vec::new(),
            favorite: None,
            missing: None,
            year_min: None,
            year_max: None,
            trash: LibraryTrashFilter::Active,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum LibrarySortField {
    Title,
    Creator,
    Year,
    #[default]
    DateAdded,
    LastOpened,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum LibrarySortDirection {
    Ascending,
    #[default]
    Descending,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibrarySort {
    pub field: LibrarySortField,
    pub direction: LibrarySortDirection,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibrarySavedView {
    pub id: LibrarySavedViewId,
    pub name: String,
    pub filter: LibraryFilter,
    pub sort: LibrarySort,
    pub view_mode: LibraryViewMode,
    pub created: u64,
    pub updated: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LibraryItemMetadata {
    pub kind: LibraryItemKind,
    pub title: String,
    pub creators: Vec<LibraryCreator>,
    pub year: Option<i32>,
    pub publication: String,
    pub publisher: String,
    pub abstract_text: String,
    pub subject: String,
    pub language: String,
    pub identifiers: LibraryIdentifiers,
    pub embedded_keywords: Vec<String>,
}

impl LibraryItemMetadata {
    pub fn for_path(path: &Path) -> Self {
        let format = path
            .extension()
            .and_then(|extension| extension.to_str())
            .and_then(DocumentFormat::from_extension);
        Self {
            kind: match format {
                Some(DocumentFormat::Epub) => LibraryItemKind::Book,
                Some(DocumentFormat::Pdf) | None => LibraryItemKind::Paper,
            },
            title: path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or("Untitled")
                .to_string(),
            ..Self::default()
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LibraryAddOutcome {
    Added(DocumentId),
    Existing(DocumentId),
    ExistingInTrash(DocumentId),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct StoredLibraryItemV1 {
    version: u8,
    item: LibraryItem,
}

impl StoredLibraryItemV1 {
    pub(super) fn new(item: &LibraryItem) -> Self {
        Self {
            version: LIBRARY_CATALOG_STORAGE_VERSION,
            item: item.clone(),
        }
    }

    pub(super) fn into_domain(self) -> Result<LibraryItem> {
        require_storage_version(self.version, LIBRARY_CATALOG_STORAGE_VERSION)?;
        Ok(self.item)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct StoredLibraryAttachmentV1 {
    version: u8,
    id: LibraryAttachmentId,
    item_id: DocumentId,
    path: StoredPathV1,
    format: DocumentFormat,
    file_size: u64,
    modified: u64,
    primary: bool,
}

impl StoredLibraryAttachmentV1 {
    pub(super) fn new(attachment: &LibraryAttachment) -> Self {
        Self {
            version: LIBRARY_CATALOG_STORAGE_VERSION,
            id: attachment.id,
            item_id: attachment.item_id,
            path: StoredPathV1::from_path(&attachment.path),
            format: attachment.format,
            file_size: attachment.file_size,
            modified: attachment.modified,
            primary: attachment.primary,
        }
    }

    pub(super) fn into_domain(self) -> Result<LibraryAttachment> {
        require_storage_version(self.version, LIBRARY_CATALOG_STORAGE_VERSION)?;
        Ok(LibraryAttachment {
            id: self.id,
            item_id: self.item_id,
            path: self.path.into_path()?,
            format: self.format,
            file_size: self.file_size,
            modified: self.modified,
            primary: self.primary,
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct StoredLibraryTagV1 {
    version: u8,
    tag: LibraryTag,
}

impl StoredLibraryTagV1 {
    pub(super) fn new(tag: &LibraryTag) -> Self {
        Self {
            version: LIBRARY_CATALOG_STORAGE_VERSION,
            tag: tag.clone(),
        }
    }

    pub(super) fn into_domain(self) -> Result<LibraryTag> {
        require_storage_version(self.version, LIBRARY_CATALOG_STORAGE_VERSION)?;
        Ok(self.tag)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct StoredLibraryItemTagsV1 {
    version: u8,
    tag_ids: Vec<LibraryTagId>,
}

impl StoredLibraryItemTagsV1 {
    pub(super) fn new(tag_ids: Vec<LibraryTagId>) -> Self {
        Self {
            version: LIBRARY_CATALOG_STORAGE_VERSION,
            tag_ids,
        }
    }

    pub(super) fn into_domain(self) -> Result<Vec<LibraryTagId>> {
        require_storage_version(self.version, LIBRARY_CATALOG_STORAGE_VERSION)?;
        Ok(self.tag_ids)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct StoredLibrarySavedViewV1 {
    version: u8,
    view: LibrarySavedView,
}

impl StoredLibrarySavedViewV1 {
    pub(super) fn new(view: &LibrarySavedView) -> Self {
        Self {
            version: LIBRARY_CATALOG_STORAGE_VERSION,
            view: view.clone(),
        }
    }

    pub(super) fn into_domain(self) -> Result<LibrarySavedView> {
        require_storage_version(self.version, LIBRARY_CATALOG_STORAGE_VERSION)?;
        Ok(self.view)
    }
}

fn clean_text(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn normalize_tag_name(value: &str) -> String {
    clean_text(value).to_lowercase()
}

fn optional_clean(value: Option<String>) -> Option<String> {
    value
        .map(|value| clean_text(&value))
        .filter(|value| !value.is_empty())
}

impl LibraryItem {
    fn sanitize(&mut self) {
        self.title = clean_text(&self.title);
        if self.title.is_empty() {
            self.title = "Untitled".to_string();
        }
        self.creators.retain_mut(|creator| {
            creator.name = clean_text(&creator.name);
            !creator.name.is_empty()
        });
        self.publication = clean_text(&self.publication);
        self.publisher = clean_text(&self.publisher);
        self.abstract_text = self.abstract_text.trim().to_string();
        self.subject = clean_text(&self.subject);
        self.language = clean_text(&self.language);
        self.identifiers.doi = optional_clean(self.identifiers.doi.take());
        self.identifiers.isbn = optional_clean(self.identifiers.isbn.take());
        self.identifiers.arxiv = optional_clean(self.identifiers.arxiv.take());
        self.embedded_keywords = self
            .embedded_keywords
            .drain(..)
            .map(|keyword| clean_text(&keyword))
            .filter(|keyword| !keyword.is_empty())
            .collect();
        self.embedded_keywords
            .sort_by_key(|keyword| keyword.to_lowercase());
        self.embedded_keywords
            .dedup_by(|left, right| left.eq_ignore_ascii_case(right));
        self.work_note = self.work_note.trim().to_string();
        self.updated = self.updated.max(self.created);
    }
}

impl Library {
    pub(super) fn sanitize_catalog(&mut self) {
        self.library_items.retain_mut(|item| {
            item.sanitize();
            item.id.0 > 0
        });
        self.library_items.sort_by_key(|item| item.id.0);
        self.library_items.dedup_by_key(|item| item.id.0);
        let item_ids = self
            .library_items
            .iter()
            .map(|item| item.id)
            .collect::<HashSet<_>>();

        self.library_attachments.retain_mut(|attachment| {
            attachment.path = normalize_path(&attachment.path);
            attachment.id.0 > 0
                && item_ids.contains(&attachment.item_id)
                && !attachment.path.as_os_str().is_empty()
        });
        self.library_attachments
            .sort_by_key(|attachment| attachment.id.0);
        self.library_attachments
            .dedup_by_key(|attachment| attachment.id.0);

        self.library_tags.retain_mut(|tag| {
            tag.name = clean_text(&tag.name);
            tag.normalized_name = normalize_tag_name(&tag.name);
            tag.id.0 > 0 && !tag.normalized_name.is_empty()
        });
        self.library_tags
            .sort_by(|left, right| left.normalized_name.cmp(&right.normalized_name));
        self.library_tags
            .dedup_by(|left, right| left.normalized_name == right.normalized_name);
        let tag_ids = self
            .library_tags
            .iter()
            .map(|tag| tag.id)
            .collect::<HashSet<_>>();

        self.library_item_tags.retain(|relation| {
            item_ids.contains(&relation.item_id) && tag_ids.contains(&relation.tag_id)
        });
        self.library_item_tags
            .sort_by_key(|relation| (relation.item_id.0, relation.tag_id.0));
        self.library_item_tags.dedup();

        self.library_saved_views.retain_mut(|view| {
            view.name = clean_text(&view.name);
            view.filter
                .tag_ids
                .retain(|tag_id| tag_ids.contains(tag_id));
            view.filter.tag_ids.sort_by_key(|tag_id| tag_id.0);
            view.filter.tag_ids.dedup();
            view.id.0 > 0 && !view.name.is_empty()
        });
        self.library_saved_views.sort_by_key(|view| view.id.0);
        self.library_saved_views.dedup_by_key(|view| view.id.0);

        self.next_library_item_id = self.next_library_item_id.max(
            self.library_items
                .iter()
                .map(|item| item.id.0)
                .max()
                .unwrap_or(0)
                .saturating_add(1),
        );
        self.next_library_attachment_id = self.next_library_attachment_id.max(
            self.library_attachments
                .iter()
                .map(|attachment| attachment.id.0)
                .max()
                .unwrap_or(0)
                .saturating_add(1),
        );
        self.next_library_tag_id = self.next_library_tag_id.max(
            self.library_tags
                .iter()
                .map(|tag| tag.id.0)
                .max()
                .unwrap_or(0)
                .saturating_add(1),
        );
        self.next_library_saved_view_id = self.next_library_saved_view_id.max(
            self.library_saved_views
                .iter()
                .map(|view| view.id.0)
                .max()
                .unwrap_or(0)
                .saturating_add(1),
        );
    }
}

impl LibraryStore {
    pub fn catalog_revision(&self) -> u64 {
        self.catalog_revision
    }

    pub fn library_items(&self) -> &[LibraryItem] {
        &self.library.library_items
    }

    pub fn library_tags(&self) -> &[LibraryTag] {
        &self.library.library_tags
    }

    pub fn library_saved_views(&self) -> &[LibrarySavedView] {
        &self.library.library_saved_views
    }

    pub fn library_item(&self, id: DocumentId) -> Option<&LibraryItem> {
        self.library.library_items.iter().find(|item| item.id == id)
    }

    pub fn primary_attachment(&self, id: DocumentId) -> Option<&LibraryAttachment> {
        self.library
            .library_attachments
            .iter()
            .filter(|attachment| attachment.item_id == id)
            .min_by_key(|attachment| (!attachment.primary, attachment.id.0))
    }

    pub fn library_item_for_path(&self, path: &Path) -> Option<&LibraryItem> {
        let path = normalize_path(path);
        let item_id = self
            .library
            .library_attachments
            .iter()
            .find(|attachment| attachment.path == path)?
            .item_id;
        self.library_item(item_id)
    }

    pub fn tags_for_item(&self, id: DocumentId) -> Vec<&LibraryTag> {
        let tag_ids = self
            .library
            .library_item_tags
            .iter()
            .filter(|relation| relation.item_id == id)
            .map(|relation| relation.tag_id)
            .collect::<HashSet<_>>();
        self.library
            .library_tags
            .iter()
            .filter(|tag| tag_ids.contains(&tag.id))
            .collect()
    }

    fn persist_catalog_change<T>(
        &mut self,
        change: impl FnOnce(&mut Library) -> Result<T>,
    ) -> Result<T> {
        let previous = self.library.clone();
        let result = match change(&mut self.library) {
            Ok(result) => result,
            Err(error) => {
                self.library = previous;
                return Err(error);
            }
        };
        self.library.sanitize_catalog();
        if let Err(error) = self.write_catalog() {
            self.library = previous;
            return Err(error);
        }
        self.catalog_revision = self.catalog_revision.wrapping_add(1);
        Ok(result)
    }

    pub fn add_linked_item(
        &mut self,
        path: &Path,
        mut metadata: LibraryItemMetadata,
    ) -> Result<LibraryAddOutcome> {
        let path = normalize_path(path);
        let format = path
            .extension()
            .and_then(|extension| extension.to_str())
            .and_then(DocumentFormat::from_extension)
            .ok_or_else(|| anyhow!("unsupported library attachment: {}", path.display()))?;
        if !path.is_file() {
            return Err(anyhow!(
                "library attachment does not exist: {}",
                path.display()
            ));
        }
        if let Some(item) = self.library_item_for_path(&path) {
            return Ok(if item.trashed_at.is_some() {
                LibraryAddOutcome::ExistingInTrash(item.id)
            } else {
                LibraryAddOutcome::Existing(item.id)
            });
        }
        if metadata.title.trim().is_empty() {
            metadata.title = LibraryItemMetadata::for_path(&path).title;
        }
        let file_metadata = fs::metadata(&path)?;
        let modified = file_metadata
            .modified()
            .ok()
            .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_secs())
            .unwrap_or(0);
        self.persist_catalog_change(|library| {
            let item_id = DocumentId(library.next_library_item_id.max(1));
            library.next_library_item_id = item_id.0.saturating_add(1);
            let attachment_id = LibraryAttachmentId(library.next_library_attachment_id.max(1));
            library.next_library_attachment_id = attachment_id.0.saturating_add(1);
            let now = current_timestamp();
            library.library_items.push(LibraryItem {
                id: item_id,
                kind: metadata.kind,
                title: metadata.title,
                creators: metadata.creators,
                year: metadata.year,
                publication: metadata.publication,
                publisher: metadata.publisher,
                abstract_text: metadata.abstract_text,
                subject: metadata.subject,
                language: metadata.language,
                identifiers: metadata.identifiers,
                embedded_keywords: metadata.embedded_keywords,
                favorite: false,
                status: ReadingStatus::ToRead,
                work_note: String::new(),
                created: now,
                updated: now,
                trashed_at: None,
            });
            library.library_attachments.push(LibraryAttachment {
                id: attachment_id,
                item_id,
                path,
                format,
                file_size: file_metadata.len(),
                modified,
                primary: true,
            });
            Ok(LibraryAddOutcome::Added(item_id))
        })
    }

    pub fn update_library_item(&mut self, mut item: LibraryItem) -> Result<()> {
        let id = item.id;
        self.persist_catalog_change(|library| {
            let existing = library
                .library_items
                .iter_mut()
                .find(|candidate| candidate.id == id)
                .ok_or_else(|| anyhow!("library item {} does not exist", id.0))?;
            item.created = existing.created;
            item.updated = current_timestamp();
            *existing = item;
            Ok(())
        })
    }

    pub fn set_library_item_favorite(&mut self, id: DocumentId, favorite: bool) -> Result<()> {
        self.update_library_item_field(id, |item| item.favorite = favorite)
    }

    pub fn set_library_item_status(&mut self, id: DocumentId, status: ReadingStatus) -> Result<()> {
        self.update_library_item_field(id, |item| item.status = status)
    }

    pub fn set_library_item_note(&mut self, id: DocumentId, note: String) -> Result<()> {
        self.update_library_item_field(id, |item| item.work_note = note)
    }

    fn update_library_item_field(
        &mut self,
        id: DocumentId,
        update: impl FnOnce(&mut LibraryItem),
    ) -> Result<()> {
        self.persist_catalog_change(|library| {
            let item = library
                .library_items
                .iter_mut()
                .find(|item| item.id == id)
                .ok_or_else(|| anyhow!("library item {} does not exist", id.0))?;
            update(item);
            item.updated = current_timestamp();
            Ok(())
        })
    }

    pub fn mark_library_item_opened(&mut self, path: &Path) -> Result<bool> {
        let Some(id) = self.library_item_for_path(path).map(|item| item.id) else {
            return Ok(false);
        };
        if self
            .library_item(id)
            .is_some_and(|item| item.status != ReadingStatus::ToRead || item.trashed_at.is_some())
        {
            return Ok(false);
        }
        self.set_library_item_status(id, ReadingStatus::Reading)?;
        Ok(true)
    }

    pub fn ensure_library_tag(&mut self, name: &str) -> Result<LibraryTagId> {
        let normalized = normalize_tag_name(name);
        if normalized.is_empty() {
            return Err(anyhow!("tag name cannot be empty"));
        }
        if let Some(tag) = self
            .library
            .library_tags
            .iter()
            .find(|tag| tag.normalized_name == normalized)
        {
            return Ok(tag.id);
        }
        let display_name = clean_text(name);
        self.persist_catalog_change(|library| {
            let id = LibraryTagId(library.next_library_tag_id.max(1));
            library.next_library_tag_id = id.0.saturating_add(1);
            library.library_tags.push(LibraryTag {
                id,
                name: display_name,
                normalized_name: normalized,
            });
            Ok(id)
        })
    }

    pub fn add_tag_to_items(&mut self, ids: &[DocumentId], name: &str) -> Result<LibraryTagId> {
        let tag_id = self.ensure_library_tag(name)?;
        let ids = ids.iter().copied().collect::<HashSet<_>>();
        self.persist_catalog_change(|library| {
            for item_id in ids {
                if library.library_items.iter().any(|item| item.id == item_id)
                    && !library
                        .library_item_tags
                        .iter()
                        .any(|relation| relation.item_id == item_id && relation.tag_id == tag_id)
                {
                    library
                        .library_item_tags
                        .push(LibraryItemTag { item_id, tag_id });
                }
            }
            Ok(())
        })?;
        Ok(tag_id)
    }

    pub fn set_item_tags(&mut self, id: DocumentId, tag_ids: &[LibraryTagId]) -> Result<()> {
        let tag_ids = tag_ids.iter().copied().collect::<HashSet<_>>();
        self.persist_catalog_change(|library| {
            if !library.library_items.iter().any(|item| item.id == id) {
                return Err(anyhow!("library item {} does not exist", id.0));
            }
            library
                .library_item_tags
                .retain(|relation| relation.item_id != id);
            for tag_id in tag_ids {
                if library.library_tags.iter().any(|tag| tag.id == tag_id) {
                    library.library_item_tags.push(LibraryItemTag {
                        item_id: id,
                        tag_id,
                    });
                }
            }
            Ok(())
        })
    }

    pub fn rename_library_tag(&mut self, id: LibraryTagId, name: &str) -> Result<LibraryTagId> {
        let normalized = normalize_tag_name(name);
        if normalized.is_empty() {
            return Err(anyhow!("tag name cannot be empty"));
        }
        let merge_target = self
            .library
            .library_tags
            .iter()
            .find(|tag| tag.id != id && tag.normalized_name == normalized)
            .map(|tag| tag.id);
        let display_name = clean_text(name);
        self.persist_catalog_change(|library| {
            if let Some(target_id) = merge_target {
                for relation in &mut library.library_item_tags {
                    if relation.tag_id == id {
                        relation.tag_id = target_id;
                    }
                }
                for view in &mut library.library_saved_views {
                    for tag_id in &mut view.filter.tag_ids {
                        if *tag_id == id {
                            *tag_id = target_id;
                        }
                    }
                }
                library.library_tags.retain(|tag| tag.id != id);
                return Ok(target_id);
            }
            let tag = library
                .library_tags
                .iter_mut()
                .find(|tag| tag.id == id)
                .ok_or_else(|| anyhow!("library tag {} does not exist", id.0))?;
            tag.name = display_name;
            tag.normalized_name = normalized;
            Ok(id)
        })
    }

    pub fn delete_library_tag(&mut self, id: LibraryTagId) -> Result<bool> {
        self.persist_catalog_change(|library| {
            let before = library.library_tags.len();
            library.library_tags.retain(|tag| tag.id != id);
            library
                .library_item_tags
                .retain(|relation| relation.tag_id != id);
            for view in &mut library.library_saved_views {
                view.filter.tag_ids.retain(|tag_id| *tag_id != id);
            }
            Ok(before != library.library_tags.len())
        })
    }

    pub fn save_library_view(
        &mut self,
        name: &str,
        filter: LibraryFilter,
        sort: LibrarySort,
        view_mode: LibraryViewMode,
    ) -> Result<LibrarySavedViewId> {
        let name = clean_text(name);
        if name.is_empty() {
            return Err(anyhow!("saved view name cannot be empty"));
        }
        self.persist_catalog_change(|library| {
            let id = LibrarySavedViewId(library.next_library_saved_view_id.max(1));
            library.next_library_saved_view_id = id.0.saturating_add(1);
            let now = current_timestamp();
            library.library_saved_views.push(LibrarySavedView {
                id,
                name,
                filter,
                sort,
                view_mode,
                created: now,
                updated: now,
            });
            Ok(id)
        })
    }

    pub fn delete_library_view(&mut self, id: LibrarySavedViewId) -> Result<bool> {
        self.persist_catalog_change(|library| {
            let before = library.library_saved_views.len();
            library.library_saved_views.retain(|view| view.id != id);
            Ok(before != library.library_saved_views.len())
        })
    }

    pub fn trash_library_items(&mut self, ids: &[DocumentId]) -> Result<usize> {
        let ids = ids.iter().copied().collect::<HashSet<_>>();
        self.persist_catalog_change(|library| {
            let mut changed = 0;
            let now = current_timestamp();
            for item in &mut library.library_items {
                if ids.contains(&item.id) && item.trashed_at.is_none() {
                    item.trashed_at = Some(now);
                    item.updated = now;
                    changed += 1;
                }
            }
            Ok(changed)
        })
    }

    pub fn restore_library_items(&mut self, ids: &[DocumentId]) -> Result<usize> {
        let ids = ids.iter().copied().collect::<HashSet<_>>();
        self.persist_catalog_change(|library| {
            let mut changed = 0;
            let now = current_timestamp();
            for item in &mut library.library_items {
                if ids.contains(&item.id) && item.trashed_at.take().is_some() {
                    item.updated = now;
                    changed += 1;
                }
            }
            Ok(changed)
        })
    }

    pub fn permanently_delete_library_items(&mut self, ids: &[DocumentId]) -> Result<usize> {
        let ids = ids.iter().copied().collect::<HashSet<_>>();
        self.persist_catalog_change(|library| {
            let before = library.library_items.len();
            library
                .library_items
                .retain(|item| !ids.contains(&item.id) || item.trashed_at.is_none());
            let retained = library
                .library_items
                .iter()
                .map(|item| item.id)
                .collect::<HashSet<_>>();
            library
                .library_attachments
                .retain(|attachment| retained.contains(&attachment.item_id));
            library
                .library_item_tags
                .retain(|relation| retained.contains(&relation.item_id));
            Ok(before - library.library_items.len())
        })
    }

    pub fn relink_library_attachment(&mut self, id: DocumentId, new_path: &Path) -> Result<()> {
        let new_path = normalize_path(new_path);
        let format = new_path
            .extension()
            .and_then(|extension| extension.to_str())
            .and_then(DocumentFormat::from_extension)
            .ok_or_else(|| anyhow!("unsupported library attachment: {}", new_path.display()))?;
        if !new_path.is_file() {
            return Err(anyhow!(
                "library attachment does not exist: {}",
                new_path.display()
            ));
        }
        if let Some(existing) = self.library_item_for_path(&new_path)
            && existing.id != id
        {
            return Err(anyhow!(
                "{} is already attached to another library item",
                new_path.display()
            ));
        }
        let old_path = self
            .primary_attachment(id)
            .map(|attachment| attachment.path.clone())
            .ok_or_else(|| anyhow!("library item {} has no attachment", id.0))?;
        if old_path == new_path {
            return Ok(());
        }
        let previous = self.library.clone();
        let file_metadata = fs::metadata(&new_path)?;
        let modified = file_metadata
            .modified()
            .ok()
            .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_secs())
            .unwrap_or(0);
        let attachment = self
            .library
            .library_attachments
            .iter_mut()
            .find(|attachment| attachment.item_id == id && attachment.primary)
            .ok_or_else(|| anyhow!("library item {} has no primary attachment", id.0))?;
        attachment.path = new_path.clone();
        attachment.format = format;
        attachment.file_size = file_metadata.len();
        attachment.modified = modified;
        for entry in &mut self.library.history {
            if entry.path == old_path {
                entry.path = new_path.clone();
            }
        }
        for bookmark in &mut self.library.bookmarks {
            if bookmark.path == old_path {
                bookmark.path = new_path.clone();
            }
        }
        for highlight in &mut self.library.highlights {
            if highlight.path == old_path {
                highlight.path = new_path.clone();
            }
        }
        for note in &mut self.library.notes {
            if note.path == old_path {
                note.path = new_path.clone();
            }
        }
        for session in &mut self.library.ai_sessions {
            if session.path == old_path {
                session.path = new_path.clone();
            }
        }
        for preferences in &mut self.library.document_preferences {
            if preferences.path == old_path {
                preferences.path = new_path.clone();
            }
        }
        self.library.sanitize();
        if let Err(error) = self.write_relinked_library() {
            self.library = previous;
            return Err(error);
        }
        self.document_edit_histories.remove(&old_path);
        self.document_edit_histories.remove(&new_path);
        self.history_entry_revisions
            .lock()
            .expect("history persistence revisions lock poisoned")
            .clear();
        self.catalog_revision = self.catalog_revision.wrapping_add(1);
        Ok(())
    }

    pub fn query_library_items(
        &self,
        filter: &LibraryFilter,
        sort: LibrarySort,
    ) -> Vec<DocumentId> {
        let query = filter.text.trim().to_lowercase();
        let mut items = self
            .library
            .library_items
            .iter()
            .filter(|item| match filter.trash {
                LibraryTrashFilter::Active => item.trashed_at.is_none(),
                LibraryTrashFilter::Trashed => item.trashed_at.is_some(),
                LibraryTrashFilter::All => true,
            })
            .filter(|item| filter.kinds.is_empty() || filter.kinds.contains(&item.kind))
            .filter(|item| filter.statuses.is_empty() || filter.statuses.contains(&item.status))
            .filter(|item| {
                filter
                    .favorite
                    .is_none_or(|favorite| item.favorite == favorite)
            })
            .filter(|item| {
                filter
                    .year_min
                    .is_none_or(|year| item.year.is_some_and(|y| y >= year))
            })
            .filter(|item| {
                filter
                    .year_max
                    .is_none_or(|year| item.year.is_some_and(|y| y <= year))
            })
            .filter(|item| {
                filter.missing.is_none_or(|missing| {
                    self.primary_attachment(item.id)
                        .is_none_or(|attachment| attachment.is_missing())
                        == missing
                })
            })
            .filter(|item| {
                let tags = self.tags_for_item(item.id);
                filter
                    .tag_ids
                    .iter()
                    .all(|tag_id| tags.iter().any(|tag| tag.id == *tag_id))
            })
            .filter(|item| query.is_empty() || self.item_matches_text(item, &query))
            .collect::<Vec<_>>();
        items.sort_by(|left, right| {
            let ordering = match sort.field {
                LibrarySortField::Title => {
                    left.title.to_lowercase().cmp(&right.title.to_lowercase())
                }
                LibrarySortField::Creator => left
                    .creators
                    .first()
                    .map(|creator| creator.name.to_lowercase())
                    .cmp(
                        &right
                            .creators
                            .first()
                            .map(|creator| creator.name.to_lowercase()),
                    ),
                LibrarySortField::Year => left.year.cmp(&right.year),
                LibrarySortField::DateAdded => left.created.cmp(&right.created),
                LibrarySortField::LastOpened => {
                    self.last_opened(left.id).cmp(&self.last_opened(right.id))
                }
            };
            let ordering = if sort.direction == LibrarySortDirection::Descending {
                ordering.reverse()
            } else {
                ordering
            };
            if ordering == Ordering::Equal {
                left.id.0.cmp(&right.id.0)
            } else {
                ordering
            }
        });
        items.into_iter().map(|item| item.id).collect()
    }

    fn last_opened(&self, id: DocumentId) -> Option<u64> {
        let path = self.primary_attachment(id)?.path.as_path();
        self.library
            .history
            .iter()
            .find(|entry| entry.path == path)
            .map(|entry| entry.opened)
    }

    fn item_matches_text(&self, item: &LibraryItem, query: &str) -> bool {
        let mut values = vec![
            item.title.as_str(),
            item.publication.as_str(),
            item.publisher.as_str(),
            item.abstract_text.as_str(),
            item.subject.as_str(),
            item.language.as_str(),
        ];
        values.extend(item.creators.iter().map(|creator| creator.name.as_str()));
        values.extend(item.embedded_keywords.iter().map(String::as_str));
        values.extend(item.identifiers.doi.iter().map(String::as_str));
        values.extend(item.identifiers.isbn.iter().map(String::as_str));
        values.extend(item.identifiers.arxiv.iter().map(String::as_str));
        if values
            .iter()
            .any(|value| value.to_lowercase().contains(query))
            || item
                .year
                .is_some_and(|year| year.to_string().contains(query))
        {
            return true;
        }
        self.tags_for_item(item.id)
            .iter()
            .any(|tag| tag.normalized_name.contains(query))
    }
}
