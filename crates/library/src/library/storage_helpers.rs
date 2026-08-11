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
    glyphst_core::paths::data_dir().join("library")
}

pub(super) fn ensure_managed_library_directories(root: &Path) -> Result<()> {
    create_private_directory(root)?;
    create_private_directory(&root.join("files"))?;
    create_private_directory(&root.join("ai-images"))?;
    create_private_directory(&root.join(".staging"))?;
    create_private_directory(&root.join(".deleting"))?;
    Ok(())
}

pub(super) fn migrate_legacy_library_database(target: &Path) -> Result<()> {
    if target.exists() {
        return Ok(());
    }
    let legacy = glyphst_core::paths::state_dir().join("library.redb");
    if !legacy.is_file() {
        return Ok(());
    }
    let parent = target
        .parent()
        .ok_or_else(|| anyhow!("library database path has no parent"))?;
    create_private_directory(parent)?;
    let staging = parent.join("library.redb.migrating");
    if staging.exists() {
        fs::remove_file(&staging)
            .with_context(|| format!("failed to remove stale {}", staging.display()))?;
    }
    fs::copy(&legacy, &staging).with_context(|| {
        format!(
            "failed to copy legacy library database from {} to {}",
            legacy.display(),
            staging.display()
        )
    })?;
    restrict_library_file(&staging)?;
    fs::rename(&staging, target).with_context(|| {
        format!(
            "failed to install migrated database at {}",
            target.display()
        )
    })?;
    Ok(())
}

pub(super) fn ensure_tables(database: &Database) -> Result<()> {
    let write_txn = database.begin_write()?;
    {
        write_txn.open_table(HISTORY_TABLE)?;
    }
    {
        write_txn.open_table(BOOKMARKS_TABLE)?;
    }
    {
        write_txn.open_table(HIGHLIGHTS_TABLE)?;
    }
    {
        write_txn.open_table(NOTES_TABLE)?;
    }
    {
        write_txn.open_table(AI_SESSIONS_TABLE)?;
    }
    {
        write_txn.open_table(UI_STATE_TABLE)?;
    }
    {
        write_txn.open_table(STORAGE_MIGRATIONS_TABLE)?;
    }
    {
        write_txn.open_table(HISTORY_V1_TABLE)?;
    }
    {
        write_txn.open_table(BOOKMARKS_V1_TABLE)?;
    }
    {
        write_txn.open_table(HIGHLIGHTS_V1_TABLE)?;
    }
    {
        write_txn.open_table(NOTES_V1_TABLE)?;
    }
    {
        write_txn.open_table(AI_SESSIONS_V1_TABLE)?;
    }
    {
        write_txn.open_table(UI_STATE_V1_TABLE)?;
    }
    {
        write_txn.open_table(DOCUMENT_PREFERENCES_V1_TABLE)?;
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
        write_txn.open_table(DOCUMENT_PREFERENCES_V2_TABLE)?;
    }
    {
        write_txn.open_table(DOCUMENT_PREFERENCES_V3_TABLE)?;
    }
    {
        write_txn.open_table(AI_SESSIONS_V2_TABLE)?;
    }
    {
        write_txn.open_table(AI_SESSIONS_V3_TABLE)?;
    }
    {
        write_txn.open_table(AI_SESSIONS_V4_TABLE)?;
    }
    {
        write_txn.open_table(AI_SESSIONS_V5_TABLE)?;
    }
    {
        write_txn.open_table(AI_SESSIONS_V6_TABLE)?;
    }
    {
        write_txn.open_table(LIBRARY_ITEMS_V1_TABLE)?;
    }
    {
        write_txn.open_table(LIBRARY_ATTACHMENTS_V1_TABLE)?;
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
        write_txn.open_table(LIBRARY_ITEM_COLLECTIONS_V1_TABLE)?;
    }
    {
        write_txn.open_table(LIBRARY_ITEM_LOCATIONS_V2_TABLE)?;
    }
    write_txn.commit()?;
    Ok(())
}

pub(super) fn initialize_managed_catalog(database: &Database) -> Result<()> {
    let write_txn = database.begin_write()?;
    {
        let migrations = write_txn.open_table(STORAGE_MIGRATIONS_TABLE)?;
        if migrations.get(MANAGED_LIBRARY_MIGRATION_KEY)?.is_some() {
            return Ok(());
        }
    }

    macro_rules! clear_u64_table {
        ($definition:expr) => {{
            let mut table = write_txn.open_table($definition)?;
            let keys = table
                .iter()?
                .filter_map(|entry| entry.ok().map(|(key, _)| key.value()))
                .collect::<Vec<_>>();
            for key in keys {
                table.remove(key)?;
            }
        }};
    }

    clear_u64_table!(LIBRARY_ITEMS_V1_TABLE);
    clear_u64_table!(LIBRARY_ATTACHMENTS_V1_TABLE);
    clear_u64_table!(LIBRARY_ATTACHMENTS_V2_TABLE);
    clear_u64_table!(LIBRARY_TAGS_V1_TABLE);
    clear_u64_table!(LIBRARY_ITEM_TAGS_V1_TABLE);
    clear_u64_table!(LIBRARY_SAVED_VIEWS_V1_TABLE);
    clear_u64_table!(LIBRARY_COLLECTIONS_V1_TABLE);
    clear_u64_table!(LIBRARY_ITEM_COLLECTIONS_V1_TABLE);
    clear_u64_table!(LIBRARY_ITEM_LOCATIONS_V2_TABLE);

    {
        let mut migrations = write_txn.open_table(STORAGE_MIGRATIONS_TABLE)?;
        migrations.insert(
            MANAGED_LIBRARY_MIGRATION_KEY,
            [MANAGED_LIBRARY_STORAGE_VERSION].as_slice(),
        )?;
    }
    write_txn.commit()?;
    Ok(())
}

pub(super) fn load_library(database: &Database, managed_root: &Path) -> Result<Library> {
    let mut library = Library::default();
    let (
        has_v1_storage,
        has_v2_storage,
        has_document_preferences_v3,
        has_ai_sessions_v2,
        has_ai_sessions_v3,
        has_ai_sessions_v4,
        has_ai_sessions_v5,
        has_ai_sessions_v6,
    ) = {
        let read_txn = database.begin_read()?;
        let table = read_txn.open_table(STORAGE_MIGRATIONS_TABLE)?;
        (
            table.get(STORAGE_V1_MIGRATION_KEY)?.is_some(),
            table.get(STORAGE_V2_MIGRATION_KEY)?.is_some(),
            table.get(DOCUMENT_PREFERENCES_V3_MIGRATION_KEY)?.is_some(),
            table.get(AI_SESSIONS_V2_MIGRATION_KEY)?.is_some(),
            table.get(AI_SESSIONS_V3_MIGRATION_KEY)?.is_some(),
            table.get(AI_SESSIONS_V4_MIGRATION_KEY)?.is_some(),
            table.get(AI_SESSIONS_V5_MIGRATION_KEY)?.is_some(),
            table.get(AI_SESSIONS_V6_MIGRATION_KEY)?.is_some(),
        )
    };

    let read_txn = database.begin_read()?;
    if has_v2_storage {
        load_storage_v2(&read_txn, &mut library, has_ai_sessions_v6)?;
    } else if has_v1_storage {
        load_storage_v1(&read_txn, &mut library, has_ai_sessions_v6)?;
    } else {
        load_legacy_storage(&read_txn, &mut library, has_ai_sessions_v6)?;
    }
    load_document_preferences(
        &read_txn,
        &mut library,
        has_document_preferences_v3,
        has_v2_storage,
        has_v1_storage,
    )?;
    load_catalog(&read_txn, &mut library, managed_root)?;
    drop(read_txn);

    library.sanitize();
    if !has_v1_storage {
        migrate_legacy_storage_v1(database, &library)?;
    }
    if !has_v2_storage {
        migrate_storage_v2(database, &library)?;
    }
    if !has_document_preferences_v3 {
        migrate_document_preferences_v3(database, &library)?;
    }
    if !has_ai_sessions_v2 {
        migrate_ai_sessions_v2(database, &library)?;
    }
    if !has_ai_sessions_v3 {
        migrate_ai_sessions_v3(database, &library)?;
    }
    if !has_ai_sessions_v4 {
        migrate_ai_sessions_v4(database, &library)?;
    }
    if !has_ai_sessions_v5 {
        migrate_ai_sessions_v5(database, &library)?;
    }
    if !has_ai_sessions_v6 {
        // V6 deliberately starts a fresh, global conversation store. Older
        // per-document rows remain available for downgrade but are not copied.
        migrate_ai_sessions_v6(database)?;
    }
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

fn load_storage_v2(
    read_txn: &redb::ReadTransaction,
    library: &mut Library,
    has_ai_sessions_v6: bool,
) -> Result<()> {
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
    load_ai_sessions(read_txn, library, has_ai_sessions_v6)?;
    load_shared_v1_storage(read_txn, library)?;
    Ok(())
}

fn load_storage_v1(
    read_txn: &redb::ReadTransaction,
    library: &mut Library,
    has_ai_sessions_v6: bool,
) -> Result<()> {
    {
        let table = read_txn.open_table(HISTORY_V1_TABLE)?;
        for entry in table.iter()? {
            let (_, value) = entry?;
            match decode::<StoredHistoryEntryV1>(value.value())
                .and_then(StoredHistoryEntryV1::into_domain)
            {
                Ok(mut entry) => {
                    entry.path = normalize_path(&entry.path);
                    library.history.push(entry);
                }
                Err(error) => eprintln!("failed to decode PDF history entry: {error:#}"),
            }
        }
    }

    {
        let table = read_txn.open_table(BOOKMARKS_V1_TABLE)?;
        for entry in table.iter()? {
            let (_, value) = entry?;
            match decode::<StoredBookmarkV1>(value.value()).and_then(StoredBookmarkV1::into_domain)
            {
                Ok(mut entry) => {
                    entry.path = normalize_path(&entry.path);
                    library.bookmarks.push(entry);
                }
                Err(error) => eprintln!("failed to decode PDF bookmark: {error:#}"),
            }
        }
    }

    {
        let table = read_txn.open_table(HIGHLIGHTS_V1_TABLE)?;
        for entry in table.iter()? {
            let (_, value) = entry?;
            match decode::<StoredHighlightV1>(value.value())
                .and_then(StoredHighlightV1::into_domain)
            {
                Ok(mut entry) => {
                    entry.path = normalize_path(&entry.path);
                    library.highlights.push(entry);
                }
                Err(error) => eprintln!("failed to decode PDF highlight: {error:#}"),
            }
        }
    }

    {
        let table = read_txn.open_table(NOTES_V1_TABLE)?;
        for entry in table.iter()? {
            let (_, value) = entry?;
            match decode::<StoredNoteV1>(value.value()).and_then(StoredNoteV1::into_domain) {
                Ok(mut entry) => {
                    entry.path = normalize_path(&entry.path);
                    library.notes.push(entry);
                }
                Err(error) => eprintln!("failed to decode PDF note: {error:#}"),
            }
        }
    }

    load_ai_sessions(read_txn, library, has_ai_sessions_v6)?;
    load_shared_v1_storage(read_txn, library)?;
    Ok(())
}

fn load_document_preferences(
    read_txn: &redb::ReadTransaction,
    library: &mut Library,
    has_v3: bool,
    has_v2: bool,
    has_v1: bool,
) -> Result<()> {
    if has_v3 {
        let table = read_txn.open_table(DOCUMENT_PREFERENCES_V3_TABLE)?;
        for entry in table.iter()? {
            let (_, value) = entry?;
            match decode::<StoredDocumentPreferencesV3>(value.value())
                .and_then(StoredDocumentPreferencesV3::into_domain)
            {
                Ok(mut preferences) => {
                    preferences.path = normalize_path(&preferences.path);
                    library.document_preferences.push(preferences);
                }
                Err(error) => eprintln!("failed to decode document preferences: {error:#}"),
            }
        }
    } else if has_v2 {
        let table = read_txn.open_table(DOCUMENT_PREFERENCES_V2_TABLE)?;
        for entry in table.iter()? {
            let (_, value) = entry?;
            match decode::<StoredDocumentPreferencesV2>(value.value())
                .and_then(StoredDocumentPreferencesV2::into_domain)
            {
                Ok(mut preferences) => {
                    preferences.path = normalize_path(&preferences.path);
                    library.document_preferences.push(preferences);
                }
                Err(error) => eprintln!("failed to decode document preferences: {error:#}"),
            }
        }
    } else if has_v1 {
        let table = read_txn.open_table(DOCUMENT_PREFERENCES_V1_TABLE)?;
        for entry in table.iter()? {
            let (_, value) = entry?;
            match decode::<StoredDocumentPreferencesV1>(value.value())
                .and_then(StoredDocumentPreferencesV1::into_domain)
            {
                Ok(mut preferences) => {
                    preferences.path = normalize_path(&preferences.path);
                    library.document_preferences.push(preferences);
                }
                Err(error) => eprintln!("failed to decode document preferences: {error:#}"),
            }
        }
    }
    Ok(())
}

fn load_shared_v1_storage(read_txn: &redb::ReadTransaction, library: &mut Library) -> Result<()> {
    let table = read_txn.open_table(UI_STATE_V1_TABLE)?;
    if let Some(value) = table.get(SIDEBAR_WIDTHS_KEY)? {
        match decode::<StoredSidebarWidthsV1>(value.value())
            .and_then(StoredSidebarWidthsV1::into_domain)
        {
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

fn load_ai_sessions(
    read_txn: &redb::ReadTransaction,
    library: &mut Library,
    has_ai_sessions_v6: bool,
) -> Result<()> {
    if !has_ai_sessions_v6 {
        return Ok(());
    }
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

fn load_legacy_storage(
    read_txn: &redb::ReadTransaction,
    library: &mut Library,
    has_ai_sessions_v6: bool,
) -> Result<()> {
    {
        let table = read_txn.open_table(HISTORY_TABLE)?;
        for entry in table.iter()? {
            let (_, value) = entry?;
            match decode::<LegacyHistoryEntry>(value.value()) {
                Ok(entry) => {
                    let mut entry = entry.into_domain();
                    entry.path = normalize_path(&entry.path);
                    library.history.push(entry);
                }
                Err(error) => eprintln!("failed to decode PDF history entry: {error:#}"),
            }
        }
    }

    {
        let table = read_txn.open_table(BOOKMARKS_TABLE)?;
        for entry in table.iter()? {
            let (_, value) = entry?;
            match decode::<LegacyBookmark>(value.value()) {
                Ok(entry) => {
                    let mut entry = entry.into_domain();
                    entry.path = normalize_path(&entry.path);
                    library.bookmarks.push(entry);
                }
                Err(error) => eprintln!("failed to decode PDF bookmark: {error:#}"),
            }
        }
    }

    {
        let table = read_txn.open_table(HIGHLIGHTS_TABLE)?;
        for entry in table.iter()? {
            let (_, value) = entry?;
            match decode::<LegacyHighlight>(value.value()) {
                Ok(entry) => {
                    let mut entry = entry.into_domain();
                    entry.path = normalize_path(&entry.path);
                    library.highlights.push(entry);
                }
                Err(error) => eprintln!("failed to decode PDF highlight: {error:#}"),
            }
        }
    }

    {
        let table = read_txn.open_table(NOTES_TABLE)?;
        for entry in table.iter()? {
            let (_, value) = entry?;
            match decode::<LegacyNote>(value.value()) {
                Ok(entry) => {
                    let mut entry = entry.into_domain();
                    entry.path = normalize_path(&entry.path);
                    library.notes.push(entry);
                }
                Err(error) => eprintln!("failed to decode PDF note: {error:#}"),
            }
        }
    }

    load_ai_sessions(read_txn, library, has_ai_sessions_v6)?;

    {
        let table = read_txn.open_table(UI_STATE_TABLE)?;
        if let Some(value) = table.get(SIDEBAR_WIDTHS_KEY)? {
            match decode::<SidebarWidths>(value.value()) {
                Ok(widths) => library.sidebar_widths = Some(widths),
                Err(error) => eprintln!("failed to decode sidebar widths: {error:#}"),
            }
        }
    }
    Ok(())
}

fn migrate_legacy_storage_v1(database: &Database, library: &Library) -> Result<()> {
    let write_txn = database.begin_write()?;
    {
        let migrations = write_txn.open_table(STORAGE_MIGRATIONS_TABLE)?;
        if migrations.get(STORAGE_V1_MIGRATION_KEY)?.is_some() {
            return Ok(());
        }
    }

    {
        let mut table = write_txn.open_table(HISTORY_V1_TABLE)?;
        for entry in &library.history {
            let key = history_key(&entry.path);
            let encoded = encode(&StoredHistoryEntryV1::from(entry))?;
            table.insert(key.as_slice(), encoded.as_slice())?;
        }
    }
    {
        let mut table = write_txn.open_table(BOOKMARKS_V1_TABLE)?;
        for bookmark in &library.bookmarks {
            let encoded = encode(&StoredBookmarkV1::from(bookmark))?;
            table.insert(bookmark.id, encoded.as_slice())?;
        }
    }
    {
        let mut table = write_txn.open_table(HIGHLIGHTS_V1_TABLE)?;
        for highlight in &library.highlights {
            let encoded = encode(&StoredHighlightV1::from(highlight))?;
            table.insert(highlight.id, encoded.as_slice())?;
        }
    }
    {
        let mut table = write_txn.open_table(NOTES_V1_TABLE)?;
        for note in &library.notes {
            let encoded = encode(&StoredNoteV1::from(note))?;
            table.insert(note.id, encoded.as_slice())?;
        }
    }
    {
        let mut table = write_txn.open_table(AI_SESSIONS_V1_TABLE)?;
        for session in &library.ai_sessions {
            let encoded = encode(&StoredAiSessionV1::from(session))?;
            table.insert(session.id, encoded.as_slice())?;
        }
    }
    {
        let mut table = write_txn.open_table(UI_STATE_V1_TABLE)?;
        if let Some(widths) = library.sidebar_widths {
            let encoded = encode(&StoredSidebarWidthsV1::from(widths))?;
            table.insert(SIDEBAR_WIDTHS_KEY, encoded.as_slice())?;
        }
        if let Some(size) = library.document_browser_size {
            let encoded = encode(&StoredDocumentBrowserSizeV1::from(size))?;
            table.insert(DOCUMENT_BROWSER_SIZE_KEY, encoded.as_slice())?;
        }
        let encoded = encode(&StoredHomePreferencesV1::from(&library.home_preferences))?;
        table.insert(HOME_PREFERENCES_KEY, encoded.as_slice())?;
        if let Some(session) = &library.reader_session {
            let encoded = encode(&StoredReaderSessionV2::from(session))?;
            table.insert(READER_SESSION_KEY, encoded.as_slice())?;
        }
    }
    {
        let mut table = write_txn.open_table(DOCUMENT_PREFERENCES_V1_TABLE)?;
        for preferences in &library.document_preferences {
            let key = history_key(&preferences.path);
            let encoded = encode(&StoredDocumentPreferencesV1::from(preferences))?;
            table.insert(key.as_slice(), encoded.as_slice())?;
        }
    }
    {
        let mut migrations = write_txn.open_table(STORAGE_MIGRATIONS_TABLE)?;
        migrations.insert(
            STORAGE_V1_MIGRATION_KEY,
            [STORAGE_SCHEMA_VERSION].as_slice(),
        )?;
    }
    write_txn.commit()?;
    Ok(())
}

fn migrate_storage_v2(database: &Database, library: &Library) -> Result<()> {
    let write_txn = database.begin_write()?;
    {
        let migrations = write_txn.open_table(STORAGE_MIGRATIONS_TABLE)?;
        if migrations.get(STORAGE_V2_MIGRATION_KEY)?.is_some() {
            return Ok(());
        }
    }
    {
        let mut table = write_txn.open_table(HISTORY_V2_TABLE)?;
        for entry in &library.history {
            let key = history_key(&entry.path);
            let encoded = encode(&StoredHistoryEntryV2::from(entry))?;
            table.insert(key.as_slice(), encoded.as_slice())?;
        }
    }
    {
        let mut table = write_txn.open_table(BOOKMARKS_V2_TABLE)?;
        for bookmark in &library.bookmarks {
            let encoded = encode(&StoredBookmarkV2::from(bookmark))?;
            table.insert(bookmark.id, encoded.as_slice())?;
        }
    }
    {
        let mut table = write_txn.open_table(HIGHLIGHTS_V2_TABLE)?;
        for highlight in &library.highlights {
            let encoded = encode(&StoredHighlightV2::from(highlight))?;
            table.insert(highlight.id, encoded.as_slice())?;
        }
    }
    {
        let mut table = write_txn.open_table(NOTES_V2_TABLE)?;
        for note in &library.notes {
            let encoded = encode(&StoredNoteV2::from(note))?;
            table.insert(note.id, encoded.as_slice())?;
        }
    }
    {
        let mut table = write_txn.open_table(DOCUMENT_PREFERENCES_V2_TABLE)?;
        for preferences in &library.document_preferences {
            let key = history_key(&preferences.path);
            let encoded = encode(&StoredDocumentPreferencesV2::from(preferences))?;
            table.insert(key.as_slice(), encoded.as_slice())?;
        }
    }
    {
        let mut migrations = write_txn.open_table(STORAGE_MIGRATIONS_TABLE)?;
        migrations.insert(STORAGE_V2_MIGRATION_KEY, [STORAGE_V2_VERSION].as_slice())?;
    }
    write_txn.commit()?;
    Ok(())
}

fn migrate_document_preferences_v3(database: &Database, library: &Library) -> Result<()> {
    let write_txn = database.begin_write()?;
    {
        let migrations = write_txn.open_table(STORAGE_MIGRATIONS_TABLE)?;
        if migrations
            .get(DOCUMENT_PREFERENCES_V3_MIGRATION_KEY)?
            .is_some()
        {
            return Ok(());
        }
    }
    {
        let mut table = write_txn.open_table(DOCUMENT_PREFERENCES_V3_TABLE)?;
        for preferences in &library.document_preferences {
            let key = history_key(&preferences.path);
            let encoded = encode(&StoredDocumentPreferencesV3::from(preferences))?;
            table.insert(key.as_slice(), encoded.as_slice())?;
        }
    }
    {
        let mut migrations = write_txn.open_table(STORAGE_MIGRATIONS_TABLE)?;
        migrations.insert(
            DOCUMENT_PREFERENCES_V3_MIGRATION_KEY,
            [DOCUMENT_PREFERENCES_V3_VERSION].as_slice(),
        )?;
    }
    write_txn.commit()?;
    Ok(())
}

fn migrate_ai_sessions_v2(database: &Database, library: &Library) -> Result<()> {
    let write_txn = database.begin_write()?;
    {
        let migrations = write_txn.open_table(STORAGE_MIGRATIONS_TABLE)?;
        if migrations.get(AI_SESSIONS_V2_MIGRATION_KEY)?.is_some() {
            return Ok(());
        }
    }
    {
        let mut table = write_txn.open_table(AI_SESSIONS_V2_TABLE)?;
        for session in &library.ai_sessions {
            let encoded = encode(&StoredAiSessionV2::from(session))?;
            table.insert(session.id, encoded.as_slice())?;
        }
    }
    {
        let mut migrations = write_txn.open_table(STORAGE_MIGRATIONS_TABLE)?;
        migrations.insert(
            AI_SESSIONS_V2_MIGRATION_KEY,
            [STORAGE_V2_VERSION].as_slice(),
        )?;
    }
    write_txn.commit()?;
    Ok(())
}

fn migrate_ai_sessions_v3(database: &Database, library: &Library) -> Result<()> {
    let write_txn = database.begin_write()?;
    {
        let migrations = write_txn.open_table(STORAGE_MIGRATIONS_TABLE)?;
        if migrations.get(AI_SESSIONS_V3_MIGRATION_KEY)?.is_some() {
            return Ok(());
        }
    }
    {
        let mut table = write_txn.open_table(AI_SESSIONS_V3_TABLE)?;
        for session in &library.ai_sessions {
            let encoded = encode(&StoredAiSessionV3::from(session))?;
            table.insert(session.id, encoded.as_slice())?;
        }
    }
    {
        let mut migrations = write_txn.open_table(STORAGE_MIGRATIONS_TABLE)?;
        migrations.insert(
            AI_SESSIONS_V3_MIGRATION_KEY,
            [AI_SESSIONS_V3_VERSION].as_slice(),
        )?;
    }
    write_txn.commit()?;
    Ok(())
}

fn migrate_ai_sessions_v4(database: &Database, library: &Library) -> Result<()> {
    let write_txn = database.begin_write()?;
    {
        let migrations = write_txn.open_table(STORAGE_MIGRATIONS_TABLE)?;
        if migrations.get(AI_SESSIONS_V4_MIGRATION_KEY)?.is_some() {
            return Ok(());
        }
    }
    {
        let mut table = write_txn.open_table(AI_SESSIONS_V4_TABLE)?;
        for session in &library.ai_sessions {
            let encoded = encode(&StoredAiSessionV4::from(session))?;
            table.insert(session.id, encoded.as_slice())?;
        }
    }
    {
        let mut migrations = write_txn.open_table(STORAGE_MIGRATIONS_TABLE)?;
        migrations.insert(
            AI_SESSIONS_V4_MIGRATION_KEY,
            [AI_SESSIONS_V4_VERSION].as_slice(),
        )?;
    }
    write_txn.commit()?;
    Ok(())
}

fn migrate_ai_sessions_v5(database: &Database, library: &Library) -> Result<()> {
    let write_txn = database.begin_write()?;
    {
        let migrations = write_txn.open_table(STORAGE_MIGRATIONS_TABLE)?;
        if migrations.get(AI_SESSIONS_V5_MIGRATION_KEY)?.is_some() {
            return Ok(());
        }
    }
    {
        let mut table = write_txn.open_table(AI_SESSIONS_V5_TABLE)?;
        for session in &library.ai_sessions {
            let encoded = encode(&StoredAiSessionV5::from(session))?;
            table.insert(session.id, encoded.as_slice())?;
        }
    }
    {
        let mut migrations = write_txn.open_table(STORAGE_MIGRATIONS_TABLE)?;
        migrations.insert(
            AI_SESSIONS_V5_MIGRATION_KEY,
            [AI_SESSIONS_V5_VERSION].as_slice(),
        )?;
    }
    write_txn.commit()?;
    Ok(())
}

fn migrate_ai_sessions_v6(database: &Database) -> Result<()> {
    let write_txn = database.begin_write()?;
    {
        let migrations = write_txn.open_table(STORAGE_MIGRATIONS_TABLE)?;
        if migrations.get(AI_SESSIONS_V6_MIGRATION_KEY)?.is_some() {
            return Ok(());
        }
    }
    // Opening the table is intentional: V6 is a compatibility boundary and
    // starts empty instead of importing document-bound V1–V5 conversations.
    {
        write_txn.open_table(AI_SESSIONS_V6_TABLE)?;
    }
    {
        let mut migrations = write_txn.open_table(STORAGE_MIGRATIONS_TABLE)?;
        migrations.insert(
            AI_SESSIONS_V6_MIGRATION_KEY,
            [AI_SESSIONS_V6_VERSION].as_slice(),
        )?;
    }
    write_txn.commit()?;
    Ok(())
}

pub(super) fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    bincode::serialize(value).context("failed to encode library value")
}

pub(super) fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    bincode::deserialize(bytes).context("failed to decode library value")
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
