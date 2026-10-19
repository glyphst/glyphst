use std::cmp::Ordering;

use super::storage_helpers::{
    create_private_directory, ensure_managed_library_directories, restrict_library_file,
};
use super::*;

pub(super) const LIBRARY_ITEMS_V1_TABLE: TableDefinition<u64, &[u8]> =
    TableDefinition::new("library_items_v1");
pub(super) const LIBRARY_ATTACHMENTS_V2_TABLE: TableDefinition<u64, &[u8]> =
    TableDefinition::new("library_attachments_v2");
pub(super) const LIBRARY_TAGS_V1_TABLE: TableDefinition<u64, &[u8]> =
    TableDefinition::new("library_tags_v1");
pub(super) const LIBRARY_ITEM_TAGS_V1_TABLE: TableDefinition<u64, &[u8]> =
    TableDefinition::new("library_item_tags_v1");
pub(super) const LIBRARY_SAVED_VIEWS_V1_TABLE: TableDefinition<u64, &[u8]> =
    TableDefinition::new("library_saved_views_v1");
pub(super) const LIBRARY_COLLECTIONS_V1_TABLE: TableDefinition<u64, &[u8]> =
    TableDefinition::new("library_collections_v1");
pub(super) const LIBRARY_ITEM_LOCATIONS_V2_TABLE: TableDefinition<u64, &[u8]> =
    TableDefinition::new("library_item_locations_v2");
/// An item's collections (it can be in several); the location's parent is
/// kept as the first of them.
pub(super) const LIBRARY_ITEM_COLLECTIONS_V1_TABLE: TableDefinition<u64, &[u8]> =
    TableDefinition::new("library_item_collections_v1");
/// Attachments that stay where the user keeps them (not copied in).
pub(super) const LIBRARY_LINKED_ATTACHMENTS_V1_TABLE: TableDefinition<u64, &[u8]> =
    TableDefinition::new("library_linked_attachments_v1");
pub(super) const LIBRARY_CATALOG_STORAGE_VERSION: u8 = 1;
pub(super) const MANAGED_LIBRARY_STORAGE_VERSION: u8 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LibraryAttachmentId(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LibraryTagId(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LibrarySavedViewId(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct LibraryCollectionId(pub u64);

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
    pub storage_key: String,
    pub content_sha256: [u8; 32],
    pub format: DocumentFormat,
    pub file_size: u64,
    pub modified: u64,
    pub primary: bool,
    #[serde(default)]
    pub storage: AttachmentStorage,
    /// A linked file's device and inode when it was added, to find it again
    /// after the user moves it.
    #[serde(default)]
    pub file_identity: Option<FileIdentity>,
}

impl LibraryAttachment {
    pub fn is_missing(&self) -> bool {
        !self.path.exists()
    }

    pub fn is_linked(&self) -> bool {
        self.storage == AttachmentStorage::Linked
    }
}

/// Where an attachment's file lives.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum AttachmentStorage {
    /// Copied into the managed library's `files/`.
    #[default]
    Managed,
    /// Left in place; the library only records its path.
    Linked,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FileIdentity {
    pub device: u64,
    pub inode: u64,
}

impl FileIdentity {
    pub fn of(metadata: &fs::Metadata) -> Option<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;

            Some(Self {
                device: metadata.dev(),
                inode: metadata.ino(),
            })
        }

        #[cfg(not(unix))]
        {
            let _ = metadata;
            None
        }
    }
}

/// An item's membership in one collection.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LibraryItemCollection {
    pub item_id: DocumentId,
    pub collection_id: LibraryCollectionId,
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryCollection {
    pub id: LibraryCollectionId,
    pub parent_id: Option<LibraryCollectionId>,
    pub name: String,
    pub normalized_name: String,
    pub created: u64,
    pub updated: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LibraryNodeId {
    Collection(LibraryCollectionId),
    Item(DocumentId),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryTrashLocation {
    pub parent_id: Option<LibraryCollectionId>,
    pub path: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryItemLocation {
    pub item_id: DocumentId,
    pub parent_id: Option<LibraryCollectionId>,
    pub trash_origin: Option<LibraryTrashLocation>,
}

#[derive(Debug)]
pub struct PreparedManagedImport {
    source_path: PathBuf,
    staged_directory: PathBuf,
    staged_path: PathBuf,
    storage_key: String,
    content_sha256: [u8; 32],
    format: DocumentFormat,
    file_size: u64,
    modified: u64,
}

impl Drop for PreparedManagedImport {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.staged_path);
        let _ = fs::remove_dir(&self.staged_directory);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LibraryImportOutcome {
    Added(DocumentId),
    Duplicate(DocumentId),
    DuplicateInTrash(DocumentId),
}

#[derive(Clone, Debug)]
struct QuarantinedManagedDirectory {
    original: PathBuf,
    quarantined: PathBuf,
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
    /// Items in this collection or any collection under it.
    #[serde(default)]
    pub collection_id: Option<LibraryCollectionId>,
    /// Items in no collection.
    #[serde(default)]
    pub unfiled: bool,
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
            collection_id: None,
            unfiled: false,
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
pub(super) struct StoredLibraryAttachmentV2 {
    version: u8,
    id: LibraryAttachmentId,
    item_id: DocumentId,
    relative_path: StoredPathV1,
    storage_key: String,
    content_sha256: [u8; 32],
    format: DocumentFormat,
    file_size: u64,
    modified: u64,
    primary: bool,
}

impl StoredLibraryAttachmentV2 {
    pub(super) fn new(attachment: &LibraryAttachment, root: &Path) -> Result<Self> {
        let relative_path = attachment.path.strip_prefix(root).with_context(|| {
            format!(
                "managed attachment {} is outside {}",
                attachment.path.display(),
                root.display()
            )
        })?;
        validate_managed_relative_path(relative_path, &attachment.storage_key)?;
        Ok(Self {
            version: MANAGED_LIBRARY_STORAGE_VERSION,
            id: attachment.id,
            item_id: attachment.item_id,
            relative_path: StoredPathV1::from_path(relative_path),
            storage_key: attachment.storage_key.clone(),
            content_sha256: attachment.content_sha256,
            format: attachment.format,
            file_size: attachment.file_size,
            modified: attachment.modified,
            primary: attachment.primary,
        })
    }

    pub(super) fn into_domain(self, root: &Path) -> Result<LibraryAttachment> {
        require_storage_version(self.version, MANAGED_LIBRARY_STORAGE_VERSION)?;
        let relative_path = self.relative_path.into_path()?;
        validate_managed_relative_path(&relative_path, &self.storage_key)?;
        Ok(LibraryAttachment {
            id: self.id,
            item_id: self.item_id,
            path: root.join(relative_path),
            storage_key: self.storage_key,
            content_sha256: self.content_sha256,
            format: self.format,
            file_size: self.file_size,
            modified: self.modified,
            primary: self.primary,
            storage: AttachmentStorage::Managed,
            file_identity: None,
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct StoredLinkedAttachmentV1 {
    version: u8,
    id: LibraryAttachmentId,
    item_id: DocumentId,
    path: StoredPathV1,
    content_sha256: [u8; 32],
    format: DocumentFormat,
    file_size: u64,
    modified: u64,
    primary: bool,
    file_identity: Option<FileIdentity>,
}

impl StoredLinkedAttachmentV1 {
    pub(super) fn new(attachment: &LibraryAttachment) -> Result<Self> {
        if !attachment.path.is_absolute() {
            return Err(anyhow!(
                "linked attachment path is not absolute: {}",
                attachment.path.display()
            ));
        }
        Ok(Self {
            version: LIBRARY_CATALOG_STORAGE_VERSION,
            id: attachment.id,
            item_id: attachment.item_id,
            path: StoredPathV1::from_path(&attachment.path),
            content_sha256: attachment.content_sha256,
            format: attachment.format,
            file_size: attachment.file_size,
            modified: attachment.modified,
            primary: attachment.primary,
            file_identity: attachment.file_identity,
        })
    }

    pub(super) fn into_domain(self) -> Result<LibraryAttachment> {
        require_storage_version(self.version, LIBRARY_CATALOG_STORAGE_VERSION)?;
        let path = self.path.into_path()?;
        if !path.is_absolute() {
            return Err(anyhow!(
                "linked attachment path is not absolute: {}",
                path.display()
            ));
        }
        Ok(LibraryAttachment {
            id: self.id,
            item_id: self.item_id,
            path,
            storage_key: linked_storage_key(self.id),
            content_sha256: self.content_sha256,
            format: self.format,
            file_size: self.file_size,
            modified: self.modified,
            primary: self.primary,
            storage: AttachmentStorage::Linked,
            file_identity: self.file_identity,
        })
    }
}

fn linked_storage_key(id: LibraryAttachmentId) -> String {
    format!("linked-{}", id.0)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct StoredLibraryItemCollectionsV1 {
    version: u8,
    collection_ids: Vec<LibraryCollectionId>,
}

impl StoredLibraryItemCollectionsV1 {
    pub(super) fn new(collection_ids: Vec<LibraryCollectionId>) -> Self {
        Self {
            version: LIBRARY_CATALOG_STORAGE_VERSION,
            collection_ids,
        }
    }

    pub(super) fn into_domain(self) -> Result<Vec<LibraryCollectionId>> {
        require_storage_version(self.version, LIBRARY_CATALOG_STORAGE_VERSION)?;
        Ok(self.collection_ids)
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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct StoredLibraryCollectionV1 {
    version: u8,
    collection: LibraryCollection,
}

impl StoredLibraryCollectionV1 {
    pub(super) fn new(collection: &LibraryCollection) -> Self {
        Self {
            version: LIBRARY_CATALOG_STORAGE_VERSION,
            collection: collection.clone(),
        }
    }

    pub(super) fn into_domain(self) -> Result<LibraryCollection> {
        require_storage_version(self.version, LIBRARY_CATALOG_STORAGE_VERSION)?;
        Ok(self.collection)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct StoredLibraryItemLocationV2 {
    version: u8,
    location: LibraryItemLocation,
}

impl StoredLibraryItemLocationV2 {
    pub(super) fn new(location: &LibraryItemLocation) -> Self {
        Self {
            version: MANAGED_LIBRARY_STORAGE_VERSION,
            location: location.clone(),
        }
    }

    pub(super) fn into_domain(self) -> Result<LibraryItemLocation> {
        require_storage_version(self.version, MANAGED_LIBRARY_STORAGE_VERSION)?;
        Ok(self.location)
    }
}

fn validate_managed_relative_path(path: &Path, storage_key: &str) -> Result<()> {
    use std::path::Component;

    let mut components = path.components();
    if !matches!(components.next(), Some(Component::Normal(component)) if component == "files")
        || !matches!(components.next(), Some(Component::Normal(component)) if component == storage_key)
        || !matches!(components.next(), Some(Component::Normal(_)))
        || components.next().is_some()
    {
        return Err(anyhow!(
            "invalid managed attachment path {}",
            path.display()
        ));
    }
    Ok(())
}

fn clean_text(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn normalize_tag_name(value: &str) -> String {
    clean_text(value).to_lowercase()
}

fn normalize_collection_name(value: &str) -> String {
    clean_text(value).to_lowercase()
}

fn collection_path_names_in(library: &Library, id: LibraryCollectionId) -> Vec<String> {
    let mut path = Vec::new();
    let mut cursor = Some(id);
    let mut visited = HashSet::new();
    while let Some(collection_id) = cursor {
        if !visited.insert(collection_id) {
            break;
        }
        let Some(collection) = library
            .library_collections
            .iter()
            .find(|collection| collection.id == collection_id)
        else {
            break;
        };
        path.push(collection.name.clone());
        cursor = collection.parent_id;
    }
    path.reverse();
    path
}

fn ensure_collection_path_in(
    library: &mut Library,
    path: &[String],
) -> Option<LibraryCollectionId> {
    let mut parent_id = None;
    for component in path {
        let name = clean_text(component);
        let normalized_name = normalize_collection_name(&name);
        if normalized_name.is_empty() {
            continue;
        }
        if let Some(existing) = library.library_collections.iter().find(|collection| {
            collection.parent_id == parent_id && collection.normalized_name == normalized_name
        }) {
            parent_id = Some(existing.id);
            continue;
        }
        let id = LibraryCollectionId(library.next_library_collection_id.max(1));
        library.next_library_collection_id = id.0.saturating_add(1);
        let now = current_timestamp();
        library.library_collections.push(LibraryCollection {
            id,
            parent_id,
            name,
            normalized_name,
            created: now,
            updated: now,
        });
        parent_id = Some(id);
    }
    parent_id
}

fn optional_clean(value: Option<String>) -> Option<String> {
    value
        .map(|value| clean_text(&value))
        .filter(|value| !value.is_empty())
}

fn document_format_for_path(path: &Path) -> Result<DocumentFormat> {
    path.extension()
        .and_then(|extension| extension.to_str())
        .and_then(DocumentFormat::from_extension)
        .ok_or_else(|| anyhow!("unsupported library attachment: {}", path.display()))
}

fn modified_seconds(metadata: &fs::Metadata) -> u64 {
    metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

pub(super) fn hash_file(path: &Path, cancelled: &AtomicBool) -> Result<[u8; 32]> {
    let mut file =
        fs::File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 1024 * 1024];
    loop {
        if cancelled.load(AtomicOrdering::Acquire) {
            return Err(anyhow!("library import cancelled"));
        }
        let read = file
            .read(&mut buffer)
            .with_context(|| format!("failed to read {}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize().into())
}

pub fn prepare_managed_import(
    managed_root: &Path,
    source: &Path,
    cancelled: &AtomicBool,
) -> Result<PreparedManagedImport> {
    ensure_managed_library_directories(managed_root)?;
    let source = normalize_path(source);
    let source_metadata = fs::symlink_metadata(&source)
        .with_context(|| format!("failed to inspect {}", source.display()))?;
    if source_metadata.file_type().is_symlink() {
        return Err(anyhow!(
            "symbolic links are not imported: {}",
            source.display()
        ));
    }
    if !source_metadata.is_file() {
        return Err(anyhow!(
            "library import is not a file: {}",
            source.display()
        ));
    }
    let format = document_format_for_path(&source)?;
    let file_name = source
        .file_name()
        .filter(|name| !name.is_empty())
        .ok_or_else(|| anyhow!("library import has no filename: {}", source.display()))?;
    let storage_key = Uuid::new_v4().simple().to_string();
    let staged_directory = managed_root.join(".staging").join(&storage_key);
    create_private_directory(&staged_directory)?;
    let staged_path = staged_directory.join(file_name);

    let result = (|| -> Result<([u8; 32], u64)> {
        let mut input = fs::File::open(&source)
            .with_context(|| format!("failed to open {}", source.display()))?;
        let mut output = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&staged_path)
            .with_context(|| format!("failed to create {}", staged_path.display()))?;
        let mut hasher = Sha256::new();
        let mut size = 0_u64;
        let mut buffer = vec![0; 1024 * 1024];
        loop {
            if cancelled.load(AtomicOrdering::Acquire) {
                return Err(anyhow!("library import cancelled"));
            }
            let read = input
                .read(&mut buffer)
                .with_context(|| format!("failed to read {}", source.display()))?;
            if read == 0 {
                break;
            }
            output
                .write_all(&buffer[..read])
                .with_context(|| format!("failed to write {}", staged_path.display()))?;
            hasher.update(&buffer[..read]);
            size = size.saturating_add(read as u64);
        }
        output
            .sync_all()
            .with_context(|| format!("failed to sync {}", staged_path.display()))?;
        restrict_library_file(&staged_path)?;
        Ok((hasher.finalize().into(), size))
    })();

    let (content_sha256, file_size) = match result {
        Ok(result) => result,
        Err(error) => {
            let _ = fs::remove_file(&staged_path);
            let _ = fs::remove_dir(&staged_directory);
            return Err(error);
        }
    };
    Ok(PreparedManagedImport {
        source_path: source,
        staged_directory,
        staged_path,
        storage_key,
        content_sha256,
        format,
        file_size,
        modified: modified_seconds(&source_metadata),
    })
}

/// A file to link in place, hashed off the UI thread.
#[derive(Clone, Debug)]
pub struct PreparedLinkedImport {
    path: PathBuf,
    content_sha256: [u8; 32],
    format: DocumentFormat,
    file_size: u64,
    modified: u64,
    file_identity: Option<FileIdentity>,
}

impl PreparedLinkedImport {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

pub fn prepare_linked_import(
    source: &Path,
    cancelled: &AtomicBool,
) -> Result<PreparedLinkedImport> {
    let path = normalize_path(source);
    if !path.is_absolute() {
        return Err(anyhow!(
            "linked files need an absolute path: {}",
            path.display()
        ));
    }
    let metadata =
        fs::metadata(&path).with_context(|| format!("failed to inspect {}", path.display()))?;
    if !metadata.is_file() {
        return Err(anyhow!("library import is not a file: {}", path.display()));
    }
    let format = document_format_for_path(&path)?;
    let content_sha256 = hash_file(&path, cancelled)?;
    Ok(PreparedLinkedImport {
        content_sha256,
        format,
        file_size: metadata.len(),
        modified: modified_seconds(&metadata),
        file_identity: FileIdentity::of(&metadata),
        path,
    })
}

fn cleanup_prepared_import(prepared: &PreparedManagedImport) {
    let _ = fs::remove_file(&prepared.staged_path);
    let _ = fs::remove_dir(&prepared.staged_directory);
}

fn restore_quarantined_directories(directories: &[QuarantinedManagedDirectory]) {
    for directory in directories.iter().rev() {
        if directory.quarantined.exists() && !directory.original.exists() {
            let _ = fs::rename(&directory.quarantined, &directory.original);
        }
    }
}

fn remove_quarantined_directory(directory: &Path) -> Result<()> {
    if !directory.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(directory)
        .with_context(|| format!("failed to inspect {}", directory.display()))?
    {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_symlink() || !file_type.is_file() {
            return Err(anyhow!(
                "refusing to delete unexpected entry {}",
                entry.path().display()
            ));
        }
        fs::remove_file(entry.path())?;
    }
    fs::remove_dir(directory)?;
    Ok(())
}

fn allocate_library_ids(library: &mut Library) -> (DocumentId, LibraryAttachmentId) {
    let item_id = DocumentId(library.next_library_item_id.max(1));
    library.next_library_item_id = item_id.0.saturating_add(1);
    let attachment_id = LibraryAttachmentId(library.next_library_attachment_id.max(1));
    library.next_library_attachment_id = attachment_id.0.saturating_add(1);
    (item_id, attachment_id)
}

#[allow(clippy::too_many_arguments)]
fn push_library_item(
    library: &mut Library,
    item_id: DocumentId,
    attachment_id: LibraryAttachmentId,
    path: PathBuf,
    storage_key: String,
    content_sha256: [u8; 32],
    format: DocumentFormat,
    file_size: u64,
    modified: u64,
    metadata: LibraryItemMetadata,
    parent_id: Option<LibraryCollectionId>,
    storage: AttachmentStorage,
    file_identity: Option<FileIdentity>,
) {
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
        storage_key,
        content_sha256,
        format,
        file_size,
        modified,
        primary: true,
        storage,
        file_identity,
    });
    library.library_item_locations.push(LibraryItemLocation {
        item_id,
        parent_id,
        trash_origin: None,
    });
    if let Some(collection_id) = parent_id {
        library
            .library_item_collections
            .push(LibraryItemCollection {
                item_id,
                collection_id,
            });
    }
}

/// The collections an item is in, in id order.
fn item_collection_ids(library: &Library, item_id: DocumentId) -> Vec<LibraryCollectionId> {
    library
        .library_item_collections
        .iter()
        .filter(|membership| membership.item_id == item_id)
        .map(|membership| membership.collection_id)
        .collect()
}

/// Puts an item in exactly these collections.
fn replace_item_collections(
    library: &mut Library,
    item_id: DocumentId,
    collection_ids: impl IntoIterator<Item = LibraryCollectionId>,
) {
    library
        .library_item_collections
        .retain(|membership| membership.item_id != item_id);
    library
        .library_item_collections
        .extend(
            collection_ids
                .into_iter()
                .map(|collection_id| LibraryItemCollection {
                    item_id,
                    collection_id,
                }),
        );
}

/// Whether `collection_id` is `ancestor` or under it.
fn collection_is_within(
    library: &Library,
    collection_id: LibraryCollectionId,
    ancestor: LibraryCollectionId,
) -> bool {
    let mut cursor = Some(collection_id);
    let mut visited = HashSet::new();
    while let Some(candidate) = cursor {
        if candidate == ancestor {
            return true;
        }
        if !visited.insert(candidate) {
            return false;
        }
        cursor = library
            .library_collections
            .iter()
            .find(|collection| collection.id == candidate)
            .and_then(|collection| collection.parent_id);
    }
    false
}

pub(super) fn migrate_document_path_in_library(
    library: &mut Library,
    old_path: &Path,
    new_path: &Path,
) {
    for entry in &mut library.history {
        if entry.path == old_path {
            entry.path = new_path.to_path_buf();
        }
    }
    for bookmark in &mut library.bookmarks {
        if bookmark.path == old_path {
            bookmark.path = new_path.to_path_buf();
        }
    }
    for highlight in &mut library.highlights {
        if highlight.path == old_path {
            highlight.path = new_path.to_path_buf();
        }
    }
    for note in &mut library.notes {
        if note.path == old_path {
            note.path = new_path.to_path_buf();
        }
    }
    for session in &mut library.ai_sessions {
        for message in session
            .messages
            .iter_mut()
            .chain(session.queued_messages.iter_mut())
        {
            if message.document_path.as_deref() == Some(old_path) {
                message.document_path = Some(new_path.to_path_buf());
            }
        }
    }
    for preferences in &mut library.document_preferences {
        if preferences.path == old_path {
            preferences.path = new_path.to_path_buf();
        }
    }
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

        self.library_collections.retain_mut(|collection| {
            collection.name = clean_text(&collection.name);
            collection.normalized_name = normalize_collection_name(&collection.name);
            collection.updated = collection.updated.max(collection.created);
            collection.id.0 > 0 && !collection.normalized_name.is_empty()
        });
        self.library_collections
            .sort_by_key(|collection| collection.id.0);
        self.library_collections
            .dedup_by_key(|collection| collection.id.0);

        let mut collection_ids = self
            .library_collections
            .iter()
            .map(|collection| collection.id)
            .collect::<HashSet<_>>();
        for collection in &mut self.library_collections {
            if collection.parent_id == Some(collection.id)
                || collection
                    .parent_id
                    .is_some_and(|parent_id| !collection_ids.contains(&parent_id))
            {
                collection.parent_id = None;
            }
        }

        let mut sibling_names = HashSet::new();
        self.library_collections.retain(|collection| {
            sibling_names.insert((collection.parent_id, collection.normalized_name.clone()))
        });
        collection_ids = self
            .library_collections
            .iter()
            .map(|collection| collection.id)
            .collect();
        for collection in &mut self.library_collections {
            if collection
                .parent_id
                .is_some_and(|parent_id| !collection_ids.contains(&parent_id))
            {
                collection.parent_id = None;
            }
        }

        let mut parents = self
            .library_collections
            .iter()
            .map(|collection| (collection.id, collection.parent_id))
            .collect::<HashMap<_, _>>();
        let ordered_ids = self
            .library_collections
            .iter()
            .map(|collection| collection.id)
            .collect::<Vec<_>>();
        for id in ordered_ids {
            let mut visited = HashSet::new();
            let mut cursor = Some(id);
            let mut cyclic = false;
            while let Some(candidate) = cursor {
                if !visited.insert(candidate) {
                    cyclic = true;
                    break;
                }
                cursor = parents.get(&candidate).copied().flatten();
            }
            if cyclic {
                if let Some(collection) = self
                    .library_collections
                    .iter_mut()
                    .find(|collection| collection.id == id)
                {
                    collection.parent_id = None;
                }
                parents.insert(id, None);
            }
        }

        collection_ids = self
            .library_collections
            .iter()
            .map(|collection| collection.id)
            .collect();
        self.library_item_locations.retain_mut(|location| {
            if location
                .parent_id
                .is_some_and(|parent_id| !collection_ids.contains(&parent_id))
            {
                location.parent_id = None;
            }
            if let Some(origin) = &mut location.trash_origin {
                if origin
                    .parent_id
                    .is_some_and(|parent_id| !collection_ids.contains(&parent_id))
                {
                    origin.parent_id = None;
                }
                origin.path = origin
                    .path
                    .drain(..)
                    .map(|component| clean_text(&component))
                    .filter(|component| !component.is_empty())
                    .collect();
            }
            item_ids.contains(&location.item_id)
        });
        self.library_item_locations
            .sort_by_key(|location| location.item_id.0);
        self.library_item_locations
            .dedup_by_key(|location| location.item_id.0);
        for item_id in item_ids.iter().copied() {
            if !self
                .library_item_locations
                .iter()
                .any(|location| location.item_id == item_id)
            {
                self.library_item_locations.push(LibraryItemLocation {
                    item_id,
                    parent_id: None,
                    trash_origin: None,
                });
            }
        }
        self.library_item_locations
            .sort_by_key(|location| location.item_id.0);

        self.library_item_collections.retain(|membership| {
            item_ids.contains(&membership.item_id)
                && collection_ids.contains(&membership.collection_id)
        });
        self.library_item_collections
            .sort_by_key(|membership| (membership.item_id.0, membership.collection_id.0));
        self.library_item_collections.dedup();
        for location in &mut self.library_item_locations {
            location.parent_id = self
                .library_item_collections
                .iter()
                .find(|membership| membership.item_id == location.item_id)
                .map(|membership| membership.collection_id);
        }

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
        self.next_library_collection_id = self.next_library_collection_id.max(
            self.library_collections
                .iter()
                .map(|collection| collection.id.0)
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

    pub fn library_attachments(&self) -> &[LibraryAttachment] {
        &self.library.library_attachments
    }

    pub fn library_items(&self) -> &[LibraryItem] {
        &self.library.library_items
    }

    pub fn library_tags(&self) -> &[LibraryTag] {
        &self.library.library_tags
    }

    pub fn library_item_tags(&self) -> &[LibraryItemTag] {
        &self.library.library_item_tags
    }

    pub fn library_saved_views(&self) -> &[LibrarySavedView] {
        &self.library.library_saved_views
    }

    pub fn library_collections(&self) -> &[LibraryCollection] {
        &self.library.library_collections
    }

    pub fn library_collection(&self, id: LibraryCollectionId) -> Option<&LibraryCollection> {
        self.library
            .library_collections
            .iter()
            .find(|collection| collection.id == id)
    }

    pub fn child_collections(
        &self,
        parent_id: Option<LibraryCollectionId>,
    ) -> Vec<&LibraryCollection> {
        let mut collections = self
            .library
            .library_collections
            .iter()
            .filter(|collection| collection.parent_id == parent_id)
            .collect::<Vec<_>>();
        collections.sort_by(|left, right| {
            left.normalized_name
                .cmp(&right.normalized_name)
                .then_with(|| left.id.0.cmp(&right.id.0))
        });
        collections
    }

    pub fn library_item_collections(&self) -> &[LibraryItemCollection] {
        &self.library.library_item_collections
    }

    pub fn collections_for_item(&self, id: DocumentId) -> Vec<&LibraryCollection> {
        item_collection_ids(&self.library, id)
            .into_iter()
            .filter_map(|collection_id| self.library_collection(collection_id))
            .collect()
    }

    /// The item's first collection.
    pub fn item_parent(&self, id: DocumentId) -> Option<LibraryCollectionId> {
        self.library
            .library_item_locations
            .iter()
            .find(|location| location.item_id == id)
            .and_then(|location| location.parent_id)
    }

    /// The items in a collection (not its subcollections), or with `None`
    /// the items in no collection.
    pub fn direct_collection_items(
        &self,
        collection_id: Option<LibraryCollectionId>,
    ) -> Vec<DocumentId> {
        self.library
            .library_items
            .iter()
            .filter(|item| {
                let mut memberships = self
                    .library
                    .library_item_collections
                    .iter()
                    .filter(|membership| membership.item_id == item.id);
                match collection_id {
                    Some(collection_id) => {
                        memberships.any(|membership| membership.collection_id == collection_id)
                    }
                    None => memberships.next().is_none(),
                }
            })
            .map(|item| item.id)
            .collect()
    }

    pub fn collection_ancestors(&self, id: LibraryCollectionId) -> Vec<&LibraryCollection> {
        let mut path = Vec::new();
        let mut cursor = Some(id);
        let mut visited = HashSet::new();
        while let Some(collection_id) = cursor {
            if !visited.insert(collection_id) {
                break;
            }
            let Some(collection) = self.library_collection(collection_id) else {
                break;
            };
            path.push(collection);
            cursor = collection.parent_id;
        }
        path.reverse();
        path
    }

    pub fn collection_path_label(&self, id: LibraryCollectionId) -> String {
        self.collection_ancestors(id)
            .into_iter()
            .map(|collection| collection.name.as_str())
            .collect::<Vec<_>>()
            .join("/")
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

    pub(super) fn persist_catalog_change<T>(
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
        self.sanitize_ai_catalog();
        self.prune_library_jobs();
        Ok(result)
    }

    pub fn add_linked_item(
        &mut self,
        path: &Path,
        metadata: LibraryItemMetadata,
    ) -> Result<LibraryAddOutcome> {
        if let Some(root) = self.managed_library_root.clone() {
            let cancelled = AtomicBool::new(false);
            let prepared = prepare_managed_import(&root, path, &cancelled)?;
            return Ok(
                match self.finalize_managed_import(prepared, metadata, None)? {
                    LibraryImportOutcome::Added(id) => LibraryAddOutcome::Added(id),
                    LibraryImportOutcome::Duplicate(id) => LibraryAddOutcome::Existing(id),
                    LibraryImportOutcome::DuplicateInTrash(id) => {
                        LibraryAddOutcome::ExistingInTrash(id)
                    }
                },
            );
        }

        self.add_memory_item(path, metadata)
    }

    pub fn managed_library_root(&self) -> Option<&Path> {
        self.managed_library_root.as_deref()
    }

    fn add_memory_item(
        &mut self,
        path: &Path,
        mut metadata: LibraryItemMetadata,
    ) -> Result<LibraryAddOutcome> {
        let path = normalize_path(path);
        let format = document_format_for_path(&path)?;
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
        let content_sha256 = hash_file(&path, &AtomicBool::new(false))?;
        let modified = modified_seconds(&file_metadata);
        self.persist_catalog_change(|library| {
            let (item_id, attachment_id) = allocate_library_ids(library);
            push_library_item(
                library,
                item_id,
                attachment_id,
                path,
                format!("memory-{}", item_id.0),
                content_sha256,
                format,
                file_metadata.len(),
                modified,
                metadata,
                None,
                AttachmentStorage::Linked,
                FileIdentity::of(&file_metadata),
            );
            Ok(LibraryAddOutcome::Added(item_id))
        })
    }

    /// Adds a file where it is, without copying it in. History, highlights
    /// and notes stay keyed to its path. Hashes the file on this thread; use
    /// [`prepare_linked_import`] on a worker for large files.
    pub fn link_item(
        &mut self,
        path: &Path,
        metadata: LibraryItemMetadata,
        parent_id: Option<LibraryCollectionId>,
    ) -> Result<LibraryImportOutcome> {
        let prepared = prepare_linked_import(path, &AtomicBool::new(false))?;
        self.finalize_linked_import(prepared, metadata, parent_id)
    }

    pub fn finalize_linked_import(
        &mut self,
        prepared: PreparedLinkedImport,
        mut metadata: LibraryItemMetadata,
        parent_id: Option<LibraryCollectionId>,
    ) -> Result<LibraryImportOutcome> {
        if parent_id.is_some_and(|id| self.library_collection(id).is_none()) {
            return Err(anyhow!("target folder does not exist"));
        }
        let duplicate = self
            .library
            .library_attachments
            .iter()
            .find(|attachment| {
                attachment.path == prepared.path
                    || (attachment.content_sha256 == prepared.content_sha256
                        && attachment.file_size == prepared.file_size)
            })
            .map(|attachment| attachment.item_id);
        if let Some(item_id) = duplicate {
            return Ok(
                if self
                    .library_item(item_id)
                    .is_some_and(|item| item.trashed_at.is_some())
                {
                    LibraryImportOutcome::DuplicateInTrash(item_id)
                } else {
                    LibraryImportOutcome::Duplicate(item_id)
                },
            );
        }
        if metadata.title.trim().is_empty() {
            metadata.title = LibraryItemMetadata::for_path(&prepared.path).title;
        }
        self.persist_catalog_change(|library| {
            let (item_id, attachment_id) = allocate_library_ids(library);
            push_library_item(
                library,
                item_id,
                attachment_id,
                prepared.path,
                linked_storage_key(attachment_id),
                prepared.content_sha256,
                prepared.format,
                prepared.file_size,
                prepared.modified,
                metadata,
                parent_id,
                AttachmentStorage::Linked,
                prepared.file_identity,
            );
            Ok(LibraryImportOutcome::Added(item_id))
        })
    }

    pub fn finalize_managed_import(
        &mut self,
        prepared: PreparedManagedImport,
        mut metadata: LibraryItemMetadata,
        parent_id: Option<LibraryCollectionId>,
    ) -> Result<LibraryImportOutcome> {
        if parent_id.is_some_and(|id| self.library_collection(id).is_none()) {
            cleanup_prepared_import(&prepared);
            return Err(anyhow!("target folder does not exist"));
        }
        if let Some(attachment) = self.library.library_attachments.iter().find(|attachment| {
            attachment.content_sha256 == prepared.content_sha256
                && attachment.file_size == prepared.file_size
        }) {
            let item_id = attachment.item_id;
            cleanup_prepared_import(&prepared);
            return Ok(
                if self
                    .library_item(item_id)
                    .is_some_and(|item| item.trashed_at.is_some())
                {
                    LibraryImportOutcome::DuplicateInTrash(item_id)
                } else {
                    LibraryImportOutcome::Duplicate(item_id)
                },
            );
        }
        let Some(root) = self.managed_library_root.clone() else {
            cleanup_prepared_import(&prepared);
            return Err(anyhow!("managed library storage is unavailable"));
        };
        let final_directory = root.join("files").join(&prepared.storage_key);
        let Some(file_name) = prepared.staged_path.file_name() else {
            cleanup_prepared_import(&prepared);
            return Err(anyhow!("staged import has no filename"));
        };
        let final_path = final_directory.join(file_name);
        if final_directory.exists() {
            cleanup_prepared_import(&prepared);
            return Err(anyhow!(
                "managed storage key already exists: {}",
                prepared.storage_key
            ));
        }
        if let Err(error) = fs::rename(&prepared.staged_directory, &final_directory) {
            cleanup_prepared_import(&prepared);
            return Err(error).with_context(|| {
                format!(
                    "failed to install managed attachment at {}",
                    final_directory.display()
                )
            });
        }

        if metadata.title.trim().is_empty() {
            metadata.title = LibraryItemMetadata::for_path(&prepared.source_path).title;
        }
        let previous = self.library.clone();
        let (item_id, attachment_id) = allocate_library_ids(&mut self.library);
        push_library_item(
            &mut self.library,
            item_id,
            attachment_id,
            final_path.clone(),
            prepared.storage_key.clone(),
            prepared.content_sha256,
            prepared.format,
            prepared.file_size,
            prepared.modified,
            metadata,
            parent_id,
            AttachmentStorage::Managed,
            None,
        );
        migrate_document_path_in_library(&mut self.library, &prepared.source_path, &final_path);
        self.library.sanitize();
        if let Err(error) = self.write_relinked_library() {
            self.library = previous;
            let _ = fs::rename(&final_directory, &prepared.staged_directory);
            cleanup_prepared_import(&prepared);
            return Err(error);
        }
        self.document_edit_histories.remove(&prepared.source_path);
        self.document_edit_histories.remove(&final_path);
        self.history_entry_revisions
            .lock()
            .expect("history persistence revisions lock poisoned")
            .clear();
        self.catalog_revision = self.catalog_revision.wrapping_add(1);
        Ok(LibraryImportOutcome::Added(item_id))
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
        let changed = self.persist_catalog_change(|library| {
            let item = library
                .library_items
                .iter_mut()
                .find(|item| item.id == id)
                .ok_or_else(|| anyhow!("library item {} does not exist", id.0))?;
            let before = item.clone();
            update(item);
            item.updated = current_timestamp();
            Ok((before, item.clone()))
        })?;
        if let Some(recorder) = &mut self.edit_recorder {
            recorder.push(LibraryEdit::item(changed.0, changed.1));
        }
        Ok(())
    }

    /// Sets an item's status, note and favorite without recording an edit
    /// (reverting a recorded one).
    pub(super) fn persist_item_fields(
        &mut self,
        id: DocumentId,
        status: ReadingStatus,
        note: String,
        favorite: bool,
    ) -> Result<()> {
        self.persist_catalog_change(|library| {
            let item = library
                .library_items
                .iter_mut()
                .find(|item| item.id == id)
                .ok_or_else(|| anyhow!("library item {} does not exist", id.0))?;
            item.status = status;
            item.work_note = note;
            item.favorite = favorite;
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

    /// Sets an item's tags as the user: taking off one the AI added rejects
    /// it, so it is not proposed again.
    pub fn set_item_tags(&mut self, id: DocumentId, tag_ids: &[LibraryTagId]) -> Result<()> {
        let before = self.item_tag_ids(id);
        self.replace_item_tags(id, tag_ids)?;
        let after = self.item_tag_ids(id);
        self.note_manual_tag_change(id, &before, &after);
        Ok(())
    }

    pub(super) fn item_tag_ids(&self, id: DocumentId) -> Vec<LibraryTagId> {
        self.library
            .library_item_tags
            .iter()
            .filter(|relation| relation.item_id == id)
            .map(|relation| relation.tag_id)
            .collect()
    }

    pub(super) fn item_tag_names(&self, id: DocumentId) -> Vec<String> {
        let mut names = self
            .tags_for_item(id)
            .into_iter()
            .map(|tag| tag.name.clone())
            .collect::<Vec<_>>();
        names.sort_by_key(|name| name.to_lowercase());
        names
    }

    pub(super) fn item_collection_paths(&self, id: DocumentId) -> Vec<Vec<String>> {
        item_collection_ids(&self.library, id)
            .into_iter()
            .map(|collection_id| collection_path_names_in(&self.library, collection_id))
            .collect()
    }

    fn record_tags_edit(&mut self, id: DocumentId, before: Vec<String>) {
        let after = self.item_tag_names(id);
        if before == after {
            return;
        }
        let title = self
            .library_item(id)
            .map(|item| item.title.clone())
            .unwrap_or_default();
        if let Some(recorder) = &mut self.edit_recorder {
            recorder.push(LibraryEdit::tags(id, title, before, after));
        }
    }

    fn record_collections_edit(&mut self, id: DocumentId, before: Vec<Vec<String>>) {
        let after = self.item_collection_paths(id);
        if before == after {
            return;
        }
        let title = self
            .library_item(id)
            .map(|item| item.title.clone())
            .unwrap_or_default();
        if let Some(recorder) = &mut self.edit_recorder {
            recorder.push(LibraryEdit::collections(id, title, before, after));
        }
    }

    pub(super) fn replace_item_tags(
        &mut self,
        id: DocumentId,
        tag_ids: &[LibraryTagId],
    ) -> Result<()> {
        let recording = self.edit_recorder.is_some();
        let before = recording.then(|| self.item_tag_names(id));
        self.persist_raw_item_tags(id, tag_ids)?;
        if let Some(before) = before {
            self.record_tags_edit(id, before);
        }
        Ok(())
    }

    fn persist_raw_item_tags(&mut self, id: DocumentId, tag_ids: &[LibraryTagId]) -> Result<()> {
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

    pub fn create_library_collection(
        &mut self,
        parent_id: Option<LibraryCollectionId>,
        name: &str,
    ) -> Result<LibraryCollectionId> {
        let name = clean_text(name);
        let normalized_name = normalize_collection_name(&name);
        if normalized_name.is_empty() {
            return Err(anyhow!("folder name cannot be empty"));
        }
        if parent_id.is_some_and(|id| self.library_collection(id).is_none()) {
            return Err(anyhow!("parent folder does not exist"));
        }
        if self.library.library_collections.iter().any(|collection| {
            collection.parent_id == parent_id && collection.normalized_name == normalized_name
        }) {
            return Err(anyhow!("a folder named {name:?} already exists here"));
        }
        self.persist_catalog_change(|library| {
            let id = LibraryCollectionId(library.next_library_collection_id.max(1));
            library.next_library_collection_id = id.0.saturating_add(1);
            let now = current_timestamp();
            library.library_collections.push(LibraryCollection {
                id,
                parent_id,
                name,
                normalized_name,
                created: now,
                updated: now,
            });
            Ok(id)
        })
    }

    pub fn ensure_library_collection_path(
        &mut self,
        parent_id: Option<LibraryCollectionId>,
        path: &[String],
    ) -> Result<Option<LibraryCollectionId>> {
        if parent_id.is_some_and(|id| self.library_collection(id).is_none()) {
            return Err(anyhow!("parent folder does not exist"));
        }
        self.persist_catalog_change(|library| {
            let mut cursor = parent_id;
            for component in path {
                let name = clean_text(component);
                let normalized_name = normalize_collection_name(&name);
                if normalized_name.is_empty() {
                    continue;
                }
                if let Some(existing) = library.library_collections.iter().find(|collection| {
                    collection.parent_id == cursor && collection.normalized_name == normalized_name
                }) {
                    cursor = Some(existing.id);
                    continue;
                }
                let id = LibraryCollectionId(library.next_library_collection_id.max(1));
                library.next_library_collection_id = id.0.saturating_add(1);
                let now = current_timestamp();
                library.library_collections.push(LibraryCollection {
                    id,
                    parent_id: cursor,
                    name,
                    normalized_name,
                    created: now,
                    updated: now,
                });
                cursor = Some(id);
            }
            Ok(cursor)
        })
    }

    pub fn rename_library_collection(&mut self, id: LibraryCollectionId, name: &str) -> Result<()> {
        let name = clean_text(name);
        let normalized_name = normalize_collection_name(&name);
        if normalized_name.is_empty() {
            return Err(anyhow!("folder name cannot be empty"));
        }
        let parent_id = self
            .library_collection(id)
            .ok_or_else(|| anyhow!("folder {} does not exist", id.0))?
            .parent_id;
        if self.library.library_collections.iter().any(|collection| {
            collection.id != id
                && collection.parent_id == parent_id
                && collection.normalized_name == normalized_name
        }) {
            return Err(anyhow!("a folder named {name:?} already exists here"));
        }
        self.persist_catalog_change(|library| {
            let collection = library
                .library_collections
                .iter_mut()
                .find(|collection| collection.id == id)
                .ok_or_else(|| anyhow!("folder {} does not exist", id.0))?;
            collection.name = name;
            collection.normalized_name = normalized_name;
            collection.updated = current_timestamp();
            Ok(())
        })
    }

    pub fn move_library_collection(
        &mut self,
        id: LibraryCollectionId,
        parent_id: Option<LibraryCollectionId>,
    ) -> Result<()> {
        let collection = self
            .library_collection(id)
            .ok_or_else(|| anyhow!("folder {} does not exist", id.0))?;
        if parent_id == Some(id) {
            return Err(anyhow!("a folder cannot contain itself"));
        }
        if parent_id.is_some_and(|parent_id| self.library_collection(parent_id).is_none()) {
            return Err(anyhow!("parent folder does not exist"));
        }
        let mut cursor = parent_id;
        let mut visited = HashSet::new();
        while let Some(candidate) = cursor {
            if candidate == id {
                return Err(anyhow!("a folder cannot be moved into its descendant"));
            }
            if !visited.insert(candidate) {
                return Err(anyhow!("folder hierarchy contains a cycle"));
            }
            cursor = self
                .library_collection(candidate)
                .and_then(|collection| collection.parent_id);
        }
        if self.library.library_collections.iter().any(|candidate| {
            candidate.id != id
                && candidate.parent_id == parent_id
                && candidate.normalized_name == collection.normalized_name
        }) {
            return Err(anyhow!(
                "a folder named {:?} already exists there",
                collection.name
            ));
        }
        if collection.parent_id == parent_id {
            return Ok(());
        }
        self.persist_catalog_change(|library| {
            let collection = library
                .library_collections
                .iter_mut()
                .find(|collection| collection.id == id)
                .ok_or_else(|| anyhow!("folder {} does not exist", id.0))?;
            collection.parent_id = parent_id;
            collection.updated = current_timestamp();
            Ok(())
        })
    }

    pub fn delete_library_collection(&mut self, id: LibraryCollectionId) -> Result<usize> {
        if self.library_collection(id).is_none() {
            return Ok(0);
        }
        self.persist_catalog_change(|library| {
            let mut removed = HashSet::from([id]);
            loop {
                let before = removed.len();
                for collection in &library.library_collections {
                    if collection
                        .parent_id
                        .is_some_and(|parent_id| removed.contains(&parent_id))
                    {
                        removed.insert(collection.id);
                    }
                }
                if removed.len() == before {
                    break;
                }
            }
            let now = current_timestamp();
            let paths = removed
                .iter()
                .copied()
                .map(|collection_id| {
                    (
                        collection_id,
                        collection_path_names_in(library, collection_id),
                    )
                })
                .collect::<HashMap<_, _>>();
            // An item left in no collection goes to the trash, remembering
            // where it was; one still in another collection stays there.
            let affected = library
                .library_item_collections
                .iter()
                .filter(|membership| removed.contains(&membership.collection_id))
                .map(|membership| (membership.item_id, membership.collection_id))
                .collect::<Vec<_>>();
            library
                .library_item_collections
                .retain(|membership| !removed.contains(&membership.collection_id));
            for (item_id, collection_id) in affected {
                if !item_collection_ids(library, item_id).is_empty() {
                    continue;
                }
                if let Some(location) = library
                    .library_item_locations
                    .iter_mut()
                    .find(|location| location.item_id == item_id)
                    && location.trash_origin.is_none()
                {
                    location.trash_origin = Some(LibraryTrashLocation {
                        parent_id: Some(collection_id),
                        path: paths.get(&collection_id).cloned().unwrap_or_default(),
                    });
                    location.parent_id = None;
                }
                if let Some(item) = library
                    .library_items
                    .iter_mut()
                    .find(|item| item.id == item_id)
                    && item.trashed_at.is_none()
                {
                    item.trashed_at = Some(now);
                    item.updated = now;
                }
            }
            library
                .library_collections
                .retain(|collection| !removed.contains(&collection.id));
            Ok(removed.len())
        })
    }

    pub fn add_items_to_collection(
        &mut self,
        ids: &[DocumentId],
        collection_id: LibraryCollectionId,
    ) -> Result<usize> {
        if self.library_collection(collection_id).is_none() {
            return Err(anyhow!("folder {} does not exist", collection_id.0));
        }
        let ids = ids.iter().copied().collect::<HashSet<_>>();
        self.persist_catalog_change(|library| {
            let valid_ids = library
                .library_items
                .iter()
                .filter(|item| ids.contains(&item.id))
                .map(|item| item.id)
                .collect::<Vec<_>>();
            let mut changed = 0;
            for item_id in valid_ids {
                let membership = LibraryItemCollection {
                    item_id,
                    collection_id,
                };
                if !library.library_item_collections.contains(&membership) {
                    library.library_item_collections.push(membership);
                    changed += 1;
                }
            }
            Ok(changed)
        })
    }

    pub fn remove_items_from_collection(
        &mut self,
        ids: &[DocumentId],
        collection_id: LibraryCollectionId,
    ) -> Result<usize> {
        let ids = ids.iter().copied().collect::<HashSet<_>>();
        self.persist_catalog_change(|library| {
            let before = library.library_item_collections.len();
            library.library_item_collections.retain(|membership| {
                !(ids.contains(&membership.item_id) && membership.collection_id == collection_id)
            });
            Ok(before - library.library_item_collections.len())
        })
    }

    /// Puts an item in exactly these collections (none files it nowhere).
    pub fn set_item_collections(
        &mut self,
        id: DocumentId,
        collection_ids: &[LibraryCollectionId],
    ) -> Result<()> {
        if self.library_item(id).is_none() {
            return Err(anyhow!("library item {} does not exist", id.0));
        }
        if collection_ids
            .iter()
            .any(|collection_id| self.library_collection(*collection_id).is_none())
        {
            return Err(anyhow!("folder does not exist"));
        }
        let collection_ids = collection_ids.to_vec();
        let before = self.item_collection_paths(id);
        self.persist_catalog_change(|library| {
            replace_item_collections(library, id, collection_ids);
            Ok(())
        })?;
        self.record_collections_edit(id, before);
        Ok(())
    }

    /// Puts an item in the collections at these paths, creating the ones
    /// that do not exist.
    pub fn set_item_collection_paths(
        &mut self,
        id: DocumentId,
        paths: &[Vec<String>],
    ) -> Result<()> {
        let mut collection_ids = Vec::new();
        for path in paths {
            if let Some(collection_id) = self.ensure_library_collection_path(None, path)? {
                collection_ids.push(collection_id);
            }
        }
        self.set_item_collections(id, &collection_ids)
    }

    /// Sets an item's tags by name, creating the ones that do not exist.
    pub fn set_item_tag_names(&mut self, id: DocumentId, names: &[String]) -> Result<()> {
        let tag_ids = names
            .iter()
            .filter(|name| !name.trim().is_empty())
            .map(|name| self.ensure_library_tag(name))
            .collect::<Result<Vec<_>>>()?;
        self.set_item_tags(id, &tag_ids)
    }

    pub fn disjoint_library_nodes(&self, nodes: &[LibraryNodeId]) -> Vec<LibraryNodeId> {
        let selected_collections = nodes
            .iter()
            .filter_map(|node| match node {
                LibraryNodeId::Collection(id) => Some(*id),
                LibraryNodeId::Item(_) => None,
            })
            .collect::<HashSet<_>>();
        let mut result = nodes
            .iter()
            .copied()
            .filter(|node| {
                let mut cursor = match node {
                    LibraryNodeId::Collection(id) => self
                        .library_collection(*id)
                        .and_then(|collection| collection.parent_id),
                    LibraryNodeId::Item(id) => {
                        // An item inside a selected collection moves with it.
                        return !item_collection_ids(&self.library, *id).iter().any(
                            |collection_id| {
                                selected_collections.iter().any(|selected| {
                                    collection_is_within(&self.library, *collection_id, *selected)
                                })
                            },
                        );
                    }
                };
                let mut visited = HashSet::new();
                while let Some(parent_id) = cursor {
                    if !visited.insert(parent_id) {
                        break;
                    }
                    if selected_collections.contains(&parent_id) {
                        return false;
                    }
                    cursor = self
                        .library_collection(parent_id)
                        .and_then(|collection| collection.parent_id);
                }
                true
            })
            .collect::<Vec<_>>();
        result.sort_unstable();
        result.dedup();
        result
    }

    pub fn move_library_nodes(
        &mut self,
        nodes: &[LibraryNodeId],
        target_parent: Option<LibraryCollectionId>,
    ) -> Result<usize> {
        if target_parent.is_some_and(|id| self.library_collection(id).is_none()) {
            return Err(anyhow!("target folder does not exist"));
        }
        let nodes = self.disjoint_library_nodes(nodes);
        let moving_collections = nodes
            .iter()
            .filter_map(|node| match node {
                LibraryNodeId::Collection(id) => Some(*id),
                LibraryNodeId::Item(_) => None,
            })
            .collect::<HashSet<_>>();
        for collection_id in &moving_collections {
            if target_parent == Some(*collection_id)
                || target_parent.is_some_and(|target| {
                    self.collection_ancestors(target)
                        .iter()
                        .any(|ancestor| ancestor.id == *collection_id)
                })
            {
                return Err(anyhow!("a folder cannot be moved into its descendant"));
            }
        }

        let mut moving_names = HashSet::new();
        for collection_id in &moving_collections {
            let collection = self
                .library_collection(*collection_id)
                .ok_or_else(|| anyhow!("folder {} does not exist", collection_id.0))?;
            if collection.parent_id == target_parent {
                continue;
            }
            if !moving_names.insert(collection.normalized_name.clone())
                || self.library.library_collections.iter().any(|candidate| {
                    candidate.parent_id == target_parent
                        && !moving_collections.contains(&candidate.id)
                        && candidate.normalized_name == collection.normalized_name
                })
            {
                return Err(anyhow!(
                    "a folder named {:?} already exists there",
                    collection.name
                ));
            }
        }

        for node in &nodes {
            if let LibraryNodeId::Item(id) = node {
                let item = self
                    .library_item(*id)
                    .ok_or_else(|| anyhow!("library item {} does not exist", id.0))?;
                if item.trashed_at.is_some() {
                    return Err(anyhow!("trashed library items cannot be moved"));
                }
            }
        }

        self.persist_catalog_change(|library| {
            let mut changed = 0;
            for node in nodes {
                match node {
                    LibraryNodeId::Collection(id) => {
                        let collection = library
                            .library_collections
                            .iter_mut()
                            .find(|collection| collection.id == id)
                            .ok_or_else(|| anyhow!("folder {} does not exist", id.0))?;
                        if collection.parent_id != target_parent {
                            collection.parent_id = target_parent;
                            collection.updated = current_timestamp();
                            changed += 1;
                        }
                    }
                    LibraryNodeId::Item(id) => {
                        // Moving an item takes it out of its other collections.
                        let target = target_parent.into_iter().collect::<Vec<_>>();
                        if item_collection_ids(library, id) != target {
                            replace_item_collections(library, id, target);
                            changed += 1;
                        }
                    }
                }
            }
            Ok(changed)
        })
    }

    pub fn validate_library_move(
        &self,
        nodes: &[LibraryNodeId],
        target_parent: Option<LibraryCollectionId>,
    ) -> Result<bool> {
        let mut validation_store = LibraryStore::memory();
        validation_store.library = self.library.clone();
        Ok(validation_store.move_library_nodes(nodes, target_parent)? > 0)
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
            let origin_by_id = library
                .library_item_locations
                .iter()
                .filter(|location| ids.contains(&location.item_id))
                .map(|location| {
                    (
                        location.item_id,
                        LibraryTrashLocation {
                            parent_id: location.parent_id,
                            path: location
                                .parent_id
                                .map(|id| collection_path_names_in(library, id))
                                .unwrap_or_default(),
                        },
                    )
                })
                .collect::<HashMap<_, _>>();
            for item in &mut library.library_items {
                if ids.contains(&item.id) && item.trashed_at.is_none() {
                    item.trashed_at = Some(now);
                    item.updated = now;
                    if let Some(location) = library
                        .library_item_locations
                        .iter_mut()
                        .find(|location| location.item_id == item.id)
                    {
                        location.trash_origin = origin_by_id.get(&item.id).cloned();
                    }
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
            let restoring = library
                .library_items
                .iter()
                .filter(|item| ids.contains(&item.id) && item.trashed_at.is_some())
                .map(|item| item.id)
                .collect::<Vec<_>>();
            for item_id in restoring {
                let origin = library
                    .library_item_locations
                    .iter()
                    .find(|location| location.item_id == item_id)
                    .and_then(|location| location.trash_origin.clone());
                // An item that kept its collections goes back to them; one
                // whose collection was deleted gets the collection's path again.
                let parent_id = if item_collection_ids(library, item_id).is_empty() {
                    origin.as_ref().and_then(|origin| {
                        origin
                            .parent_id
                            .filter(|id| {
                                library
                                    .library_collections
                                    .iter()
                                    .any(|collection| collection.id == *id)
                            })
                            .or_else(|| ensure_collection_path_in(library, &origin.path))
                    })
                } else {
                    None
                };
                if let Some(collection_id) = parent_id {
                    library
                        .library_item_collections
                        .push(LibraryItemCollection {
                            item_id,
                            collection_id,
                        });
                }
                if let Some(location) = library
                    .library_item_locations
                    .iter_mut()
                    .find(|location| location.item_id == item_id)
                {
                    location.trash_origin = None;
                }
                if let Some(item) = library
                    .library_items
                    .iter_mut()
                    .find(|item| item.id == item_id)
                {
                    item.trashed_at = None;
                    item.updated = now;
                    changed += 1;
                }
            }
            Ok(changed)
        })
    }

    pub fn permanently_delete_library_items(&mut self, ids: &[DocumentId]) -> Result<usize> {
        let ids = ids.iter().copied().collect::<HashSet<_>>();
        let deletable_ids = self
            .library
            .library_items
            .iter()
            .filter(|item| ids.contains(&item.id) && item.trashed_at.is_some())
            .map(|item| item.id)
            .collect::<HashSet<_>>();
        let mut quarantined = Vec::new();
        if let Some(root) = &self.managed_library_root {
            // A linked file is the user's own; deleting the item leaves it.
            for attachment in self
                .library
                .library_attachments
                .iter()
                .filter(|attachment| {
                    deletable_ids.contains(&attachment.item_id) && !attachment.is_linked()
                })
            {
                let expected = root.join("files").join(&attachment.storage_key);
                if attachment.path.parent() != Some(expected.as_path()) {
                    restore_quarantined_directories(&quarantined);
                    return Err(anyhow!(
                        "managed attachment path is outside its item directory: {}",
                        attachment.path.display()
                    ));
                }
                if !expected.exists() {
                    continue;
                }
                let quarantined_path = root.join(".deleting").join(&attachment.storage_key);
                if quarantined_path.exists() {
                    restore_quarantined_directories(&quarantined);
                    return Err(anyhow!(
                        "managed deletion quarantine already exists: {}",
                        quarantined_path.display()
                    ));
                }
                if let Err(error) = fs::rename(&expected, &quarantined_path) {
                    restore_quarantined_directories(&quarantined);
                    return Err(error)
                        .with_context(|| format!("failed to quarantine {}", expected.display()));
                }
                quarantined.push(QuarantinedManagedDirectory {
                    original: expected,
                    quarantined: quarantined_path,
                });
            }
        }

        let result = self.persist_catalog_change(|library| {
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
            library
                .library_item_locations
                .retain(|location| retained.contains(&location.item_id));
            Ok(before - library.library_items.len())
        });
        let deleted = match result {
            Ok(deleted) => deleted,
            Err(error) => {
                restore_quarantined_directories(&quarantined);
                return Err(error);
            }
        };
        for directory in quarantined {
            if let Err(error) = remove_quarantined_directory(&directory.quarantined) {
                eprintln!(
                    "failed to remove managed attachment quarantine {}: {error:#}",
                    directory.quarantined.display()
                );
            }
        }
        Ok(deleted)
    }

    pub fn relink_library_attachment(&mut self, id: DocumentId, new_path: &Path) -> Result<()> {
        let linked = self
            .primary_attachment(id)
            .is_some_and(LibraryAttachment::is_linked);
        if self.managed_library_root.is_some() && !linked {
            return self.replace_managed_attachment(id, new_path);
        }
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
        attachment.content_sha256 = hash_file(&new_path, &AtomicBool::new(false))?;
        attachment.format = format;
        attachment.file_size = file_metadata.len();
        attachment.modified = modified;
        attachment.file_identity = FileIdentity::of(&file_metadata);
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
            for message in session
                .messages
                .iter_mut()
                .chain(session.queued_messages.iter_mut())
            {
                if message.document_path.as_deref() == Some(old_path.as_path()) {
                    message.document_path = Some(new_path.clone());
                }
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

    pub fn replace_managed_attachment(&mut self, id: DocumentId, source: &Path) -> Result<()> {
        let root = self
            .managed_library_root
            .clone()
            .ok_or_else(|| anyhow!("managed library storage is unavailable"))?;
        let cancelled = AtomicBool::new(false);
        let prepared = prepare_managed_import(&root, source, &cancelled)?;
        self.finalize_managed_replacement(id, prepared)
    }

    pub fn finalize_managed_replacement(
        &mut self,
        id: DocumentId,
        prepared: PreparedManagedImport,
    ) -> Result<()> {
        let Some(root) = self.managed_library_root.clone() else {
            cleanup_prepared_import(&prepared);
            return Err(anyhow!("managed library storage is unavailable"));
        };
        if self.library_item(id).is_none() {
            cleanup_prepared_import(&prepared);
            return Err(anyhow!("library item {} does not exist", id.0));
        }
        let previous_attachment = self
            .primary_attachment(id)
            .cloned()
            .ok_or_else(|| anyhow!("library item {} has no primary attachment", id.0));
        let previous_attachment = match previous_attachment {
            Ok(attachment) => attachment,
            Err(error) => {
                cleanup_prepared_import(&prepared);
                return Err(error);
            }
        };
        if let Some(existing) = self.library.library_attachments.iter().find(|attachment| {
            attachment.item_id != id
                && attachment.content_sha256 == prepared.content_sha256
                && attachment.file_size == prepared.file_size
        }) {
            let duplicate_id = existing.item_id;
            cleanup_prepared_import(&prepared);
            return Err(anyhow!(
                "replacement duplicates library item {}",
                duplicate_id.0
            ));
        }
        if previous_attachment.content_sha256 == prepared.content_sha256
            && previous_attachment.file_size == prepared.file_size
        {
            cleanup_prepared_import(&prepared);
            return Ok(());
        }

        let item_directory = root.join("files").join(&previous_attachment.storage_key);
        if previous_attachment.path.parent() != Some(item_directory.as_path()) {
            cleanup_prepared_import(&prepared);
            return Err(anyhow!(
                "managed attachment path is outside its item directory: {}",
                previous_attachment.path.display()
            ));
        }
        let quarantined_directory = root
            .join(".deleting")
            .join(&previous_attachment.storage_key);
        if quarantined_directory.exists() {
            cleanup_prepared_import(&prepared);
            return Err(anyhow!(
                "managed replacement quarantine already exists: {}",
                quarantined_directory.display()
            ));
        }
        let Some(file_name) = prepared.staged_path.file_name().map(ToOwned::to_owned) else {
            cleanup_prepared_import(&prepared);
            return Err(anyhow!("staged replacement has no filename"));
        };
        let had_item_directory = item_directory.exists();
        if had_item_directory
            && let Err(error) = fs::rename(&item_directory, &quarantined_directory)
        {
            cleanup_prepared_import(&prepared);
            return Err(error)
                .with_context(|| format!("failed to quarantine {}", item_directory.display()));
        }
        if let Err(error) = fs::rename(&prepared.staged_directory, &item_directory) {
            if had_item_directory {
                let _ = fs::rename(&quarantined_directory, &item_directory);
            }
            cleanup_prepared_import(&prepared);
            return Err(error).with_context(|| {
                format!(
                    "failed to install replacement at {}",
                    item_directory.display()
                )
            });
        }
        let new_path = item_directory.join(file_name);

        let previous = self.library.clone();
        let attachment = self
            .library
            .library_attachments
            .iter_mut()
            .find(|attachment| attachment.item_id == id && attachment.primary)
            .ok_or_else(|| anyhow!("library item {} has no primary attachment", id.0))?;
        attachment.path = new_path.clone();
        attachment.content_sha256 = prepared.content_sha256;
        attachment.format = prepared.format;
        attachment.file_size = prepared.file_size;
        attachment.modified = prepared.modified;
        migrate_document_path_in_library(&mut self.library, &previous_attachment.path, &new_path);
        self.library.sanitize();
        if let Err(error) = self.write_relinked_library() {
            self.library = previous;
            let _ = fs::rename(&item_directory, &prepared.staged_directory);
            if had_item_directory {
                let _ = fs::rename(&quarantined_directory, &item_directory);
            }
            cleanup_prepared_import(&prepared);
            return Err(error);
        }
        if had_item_directory
            && let Err(error) = remove_quarantined_directory(&quarantined_directory)
        {
            eprintln!(
                "failed to remove replaced attachment quarantine {}: {error:#}",
                quarantined_directory.display()
            );
        }
        self.document_edit_histories
            .remove(&previous_attachment.path);
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
            .filter(|item| {
                filter.collection_id.is_none_or(|ancestor| {
                    item_collection_ids(&self.library, item.id)
                        .into_iter()
                        .any(|collection_id| {
                            collection_is_within(&self.library, collection_id, ancestor)
                        })
                })
            })
            .filter(|item| {
                !filter.unfiled || item_collection_ids(&self.library, item.id).is_empty()
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
