use super::*;

pub(super) fn create_private_directory(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};

        let already_exists = path.exists();
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true).mode(0o700);
        builder
            .create(path)
            .with_context(|| format!("failed to create {}", path.display()))?;
        if !already_exists {
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))
                .with_context(|| format!("failed to restrict {}", path.display()))?;
        }
    }

    #[cfg(not(unix))]
    {
        fs::create_dir_all(path).with_context(|| format!("failed to create {}", path.display()))?;
    }

    Ok(())
}

pub(super) fn restrict_library_file(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .with_context(|| format!("failed to restrict {}", path.display()))?;
    }

    Ok(())
}

pub(super) fn library_path() -> PathBuf {
    managed_library_root().join("library.redb")
}

pub(super) fn managed_library_root() -> PathBuf {
    glyphst_core::paths::data_dir().join("library-v2")
}

pub(super) fn ensure_managed_library_directories(root: &Path) -> Result<()> {
    create_private_directory(root)?;
    create_private_directory(&root.join("files"))?;
    create_private_directory(&root.join("ai-images"))?;
    create_private_directory(&root.join(".staging"))?;
    create_private_directory(&root.join(".deleting"))?;
    Ok(())
}

pub(super) fn ensure_current_tables(database: &Database) -> Result<()> {
    let write_txn = database.begin_write()?;
    {
        let mut metadata = write_txn.open_table(STORAGE_METADATA_TABLE)?;
        let stored_version = metadata
            .get(STORAGE_SCHEMA_VERSION_KEY)?
            .map(|version| version.value());
        match stored_version {
            Some(version) if version != CURRENT_STORAGE_SCHEMA_VERSION => {
                return Err(anyhow!(
                    "unsupported library schema version: expected {}, found {}",
                    CURRENT_STORAGE_SCHEMA_VERSION,
                    version
                ));
            }
            Some(_) => {}
            None => {
                metadata.insert(STORAGE_SCHEMA_VERSION_KEY, CURRENT_STORAGE_SCHEMA_VERSION)?;
            }
        }
    }
    {
        write_txn.open_table(HISTORY_V2_TABLE)?;
    }
    {
        write_txn.open_table(BOOKMARKS_V2_TABLE)?;
    }
    {
        write_txn.open_table(HIGHLIGHTS_V2_TABLE)?;
    }
    {
        write_txn.open_table(NOTES_V2_TABLE)?;
    }
    {
        write_txn.open_table(ACTIVITY_V1_TABLE)?;
    }
    {
        write_txn.open_table(READING_SESSIONS_V1_TABLE)?;
    }
    {
        write_txn.open_table(AI_SESSIONS_V6_TABLE)?;
    }
    {
        write_txn.open_table(UI_STATE_V1_TABLE)?;
    }
    {
        write_txn.open_table(DOCUMENT_PREFERENCES_V4_TABLE)?;
    }
    {
        write_txn.open_table(LIBRARY_ITEMS_V1_TABLE)?;
    }
    {
        write_txn.open_table(LIBRARY_ATTACHMENTS_V2_TABLE)?;
    }
    {
        write_txn.open_table(LIBRARY_TAGS_V1_TABLE)?;
    }
    {
        write_txn.open_table(LIBRARY_ITEM_TAGS_V1_TABLE)?;
    }
    {
        write_txn.open_table(LIBRARY_SAVED_VIEWS_V1_TABLE)?;
    }
    {
        write_txn.open_table(LIBRARY_COLLECTIONS_V1_TABLE)?;
    }
    {
        write_txn.open_table(LIBRARY_ITEM_LOCATIONS_V2_TABLE)?;
    }
    write_txn.commit()?;
    Ok(())
}

pub(super) fn load_current_library(database: &Database, managed_root: &Path) -> Result<Library> {
    let mut library = Library::default();
    let read_txn = database.begin_read()?;
    load_storage_v2(&read_txn, &mut library)?;
    load_document_preferences(&read_txn, &mut library)?;
    load_catalog(&read_txn, &mut library, managed_root)?;
    drop(read_txn);
    library.sanitize();
    Ok(library)
}

fn load_catalog(
    read_txn: &redb::ReadTransaction,
    library: &mut Library,
    managed_root: &Path,
) -> Result<()> {
    {
        let table = read_txn.open_table(LIBRARY_ITEMS_V1_TABLE)?;
        for entry in table.iter()? {
            let (_, value) = entry?;
            match decode::<StoredLibraryItemV1>(value.value())
                .and_then(StoredLibraryItemV1::into_domain)
            {
                Ok(item) => library.library_items.push(item),
                Err(error) => eprintln!("failed to decode library item: {error:#}"),
            }
        }
    }
    {
        let table = read_txn.open_table(LIBRARY_ATTACHMENTS_V2_TABLE)?;
        for entry in table.iter()? {
            let (_, value) = entry?;
            match decode::<StoredLibraryAttachmentV2>(value.value())
                .and_then(|attachment| attachment.into_domain(managed_root))
            {
                Ok(attachment) => library.library_attachments.push(attachment),
                Err(error) => eprintln!("failed to decode library attachment: {error:#}"),
            }
        }
    }
    {
        let table = read_txn.open_table(LIBRARY_TAGS_V1_TABLE)?;
        for entry in table.iter()? {
            let (_, value) = entry?;
            match decode::<StoredLibraryTagV1>(value.value())
                .and_then(StoredLibraryTagV1::into_domain)
            {
                Ok(tag) => library.library_tags.push(tag),
                Err(error) => eprintln!("failed to decode library tag: {error:#}"),
            }
        }
    }
    {
        let table = read_txn.open_table(LIBRARY_ITEM_TAGS_V1_TABLE)?;
        for entry in table.iter()? {
            let (item_id, value) = entry?;
            match decode::<StoredLibraryItemTagsV1>(value.value())
                .and_then(StoredLibraryItemTagsV1::into_domain)
            {
                Ok(tag_ids) => {
                    library
                        .library_item_tags
                        .extend(tag_ids.into_iter().map(|tag_id| LibraryItemTag {
                            item_id: DocumentId(item_id.value()),
                            tag_id,
                        }));
                }
                Err(error) => eprintln!("failed to decode library item tags: {error:#}"),
            }
        }
    }
    {
        let table = read_txn.open_table(LIBRARY_SAVED_VIEWS_V1_TABLE)?;
        for entry in table.iter()? {
            let (_, value) = entry?;
            match decode::<StoredLibrarySavedViewV1>(value.value())
                .and_then(StoredLibrarySavedViewV1::into_domain)
            {
                Ok(view) => library.library_saved_views.push(view),
                Err(error) => eprintln!("failed to decode library saved view: {error:#}"),
            }
        }
    }
    {
        let table = read_txn.open_table(LIBRARY_COLLECTIONS_V1_TABLE)?;
        for entry in table.iter()? {
            let (_, value) = entry?;
            match decode::<StoredLibraryCollectionV1>(value.value())
                .and_then(StoredLibraryCollectionV1::into_domain)
            {
                Ok(collection) => library.library_collections.push(collection),
                Err(error) => eprintln!("failed to decode library folder: {error:#}"),
            }
        }
    }
    {
        let table = read_txn.open_table(LIBRARY_ITEM_LOCATIONS_V2_TABLE)?;
        for entry in table.iter()? {
            let (_, value) = entry?;
            match decode::<StoredLibraryItemLocationV2>(value.value())
                .and_then(StoredLibraryItemLocationV2::into_domain)
            {
                Ok(location) => library.library_item_locations.push(location),
                Err(error) => eprintln!("failed to decode library item location: {error:#}"),
            }
        }
    }
    Ok(())
}

fn load_storage_v2(read_txn: &redb::ReadTransaction, library: &mut Library) -> Result<()> {
    {
        let table = read_txn.open_table(HISTORY_V2_TABLE)?;
        for entry in table.iter()? {
            let (_, value) = entry?;
            match decode::<StoredHistoryEntryV2>(value.value())
                .and_then(StoredHistoryEntryV2::into_domain)
            {
                Ok(mut entry) => {
                    entry.path = normalize_path(&entry.path);
                    library.history.push(entry);
                }
                Err(error) => eprintln!("failed to decode document history entry: {error:#}"),
            }
        }
    }
    {
        let table = read_txn.open_table(BOOKMARKS_V2_TABLE)?;
        for entry in table.iter()? {
            let (_, value) = entry?;
            match decode::<StoredBookmarkV2>(value.value()).and_then(StoredBookmarkV2::into_domain)
            {
                Ok(mut entry) => {
                    entry.path = normalize_path(&entry.path);
                    library.bookmarks.push(entry);
                }
                Err(error) => eprintln!("failed to decode document bookmark: {error:#}"),
            }
        }
    }
    {
        let table = read_txn.open_table(HIGHLIGHTS_V2_TABLE)?;
        for entry in table.iter()? {
            let (_, value) = entry?;
            match decode::<StoredHighlightV2>(value.value())
                .and_then(StoredHighlightV2::into_domain)
            {
                Ok(mut entry) => {
                    entry.path = normalize_path(&entry.path);
                    library.highlights.push(entry);
                }
                Err(error) => eprintln!("failed to decode document highlight: {error:#}"),
            }
        }
    }
    {
        let table = read_txn.open_table(NOTES_V2_TABLE)?;
        for entry in table.iter()? {
            let (_, value) = entry?;
            match decode::<StoredNoteV2>(value.value()).and_then(StoredNoteV2::into_domain) {
                Ok(mut entry) => {
                    entry.path = normalize_path(&entry.path);
                    library.notes.push(entry);
                }
                Err(error) => eprintln!("failed to decode document note: {error:#}"),
            }
        }
    }
    load_ai_sessions(read_txn, library)?;
    load_shared_v1_storage(read_txn, library)?;
    super::activity::load_activity(read_txn, library)?;
    super::reading::load_reading_sessions(read_txn, library)?;
    Ok(())
}

fn load_document_preferences(
    read_txn: &redb::ReadTransaction,
    library: &mut Library,
) -> Result<()> {
    let table = read_txn.open_table(DOCUMENT_PREFERENCES_V4_TABLE)?;
    for entry in table.iter()? {
        let (_, value) = entry?;
        match decode::<StoredDocumentPreferencesV4>(value.value())
            .and_then(StoredDocumentPreferencesV4::into_domain)
        {
            Ok(mut preferences) => {
                preferences.path = normalize_path(&preferences.path);
                library.document_preferences.push(preferences);
            }
            Err(error) => eprintln!("failed to decode document preferences: {error:#}"),
        }
    }
    Ok(())
}

fn load_shared_v1_storage(read_txn: &redb::ReadTransaction, library: &mut Library) -> Result<()> {
    let table = read_txn.open_table(UI_STATE_V1_TABLE)?;
    if let Some(value) = table.get(SIDEBAR_WIDTHS_KEY)? {
        match decode::<StoredSidebarWidthsV2>(value.value())
            .and_then(StoredSidebarWidthsV2::into_domain)
            .or_else(|_| {
                decode::<StoredSidebarWidthsV1>(value.value())
                    .and_then(StoredSidebarWidthsV1::into_domain)
            }) {
            Ok(widths) => library.sidebar_widths = Some(widths),
            Err(error) => eprintln!("failed to decode sidebar widths: {error:#}"),
        }
    }
    if let Some(value) = table.get(DOCUMENT_BROWSER_SIZE_KEY)? {
        match decode::<StoredDocumentBrowserSizeV1>(value.value())
            .and_then(StoredDocumentBrowserSizeV1::into_domain)
        {
            Ok(size) => library.document_browser_size = Some(size),
            Err(error) => eprintln!("failed to decode document browser size: {error:#}"),
        }
    }
    if let Some(value) = table.get(HOME_PREFERENCES_KEY)? {
        match decode::<StoredHomePreferencesV1>(value.value())
            .and_then(StoredHomePreferencesV1::into_domain)
        {
            Ok(preferences) => library.home_preferences = preferences,
            Err(error) => eprintln!("failed to decode Home preferences: {error:#}"),
        }
    }
    if let Some(value) = table.get(READER_SESSION_KEY)? {
        let bytes = value.value();
        let session = decode::<StoredReaderSessionV4>(bytes)
            .and_then(StoredReaderSessionV4::into_domain)
            .or_else(|_| {
                decode::<StoredReaderSessionV3>(bytes).and_then(StoredReaderSessionV3::into_domain)
            })
            .or_else(|_| {
                decode::<StoredReaderSessionV2>(bytes).and_then(StoredReaderSessionV2::into_domain)
            })
            .or_else(|_| {
                decode::<StoredReaderSessionV1>(bytes).and_then(StoredReaderSessionV1::into_domain)
            });
        match session {
            Ok(session) => library.reader_session = Some(session),
            Err(error) => eprintln!("failed to decode reader session: {error:#}"),
        }
    }
    Ok(())
}

fn load_ai_sessions(read_txn: &redb::ReadTransaction, library: &mut Library) -> Result<()> {
    let table = read_txn.open_table(AI_SESSIONS_V6_TABLE)?;
    for entry in table.iter()? {
        let (_, value) = entry?;
        match decode::<StoredAiSessionV6>(value.value()).and_then(StoredAiSessionV6::into_domain) {
            Ok(entry) => library.ai_sessions.push(entry),
            Err(error) => eprintln!("failed to decode global AI session: {error:#}"),
        }
    }
    Ok(())
}

pub(super) fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    serde_json::to_vec(value).context("failed to encode library value as JSON")
}

pub(super) fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    serde_json::from_slice(bytes).context("failed to decode library JSON value")
}

pub(super) fn require_storage_version(version: u8, expected: u8) -> Result<()> {
    if version == expected {
        Ok(())
    } else {
        Err(anyhow!(
            "unsupported library storage schema version: expected {expected}, found {version}"
        ))
    }
}

pub(super) fn history_key(path: &Path) -> Vec<u8> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;

        let mut key = Vec::with_capacity(path.as_os_str().as_bytes().len() + 1);
        key.push(0);
        key.extend_from_slice(path.as_os_str().as_bytes());
        key
    }

    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt as _;

        let units = path.as_os_str().encode_wide().collect::<Vec<_>>();
        let mut key = Vec::with_capacity(units.len() * 2 + 1);
        key.push(1);
        for unit in units {
            key.extend_from_slice(&unit.to_le_bytes());
        }
        key
    }
}
