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
    glyphst_core::paths::state_dir().join("library.redb")
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
    write_txn.commit()?;
    Ok(())
}

pub(super) fn load_library(database: &Database) -> Result<Library> {
    let mut library = Library::default();
    let (
        has_v1_storage,
        has_v2_storage,
        has_document_preferences_v3,
        has_ai_sessions_v2,
        has_ai_sessions_v3,
        has_ai_sessions_v4,
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
        )
    };

    let read_txn = database.begin_read()?;
    if has_v2_storage {
        load_storage_v2(
            &read_txn,
            &mut library,
            has_ai_sessions_v2,
            has_ai_sessions_v3,
            has_ai_sessions_v4,
        )?;
    } else if has_v1_storage {
        load_storage_v1(
            &read_txn,
            &mut library,
            has_ai_sessions_v2,
            has_ai_sessions_v3,
            has_ai_sessions_v4,
        )?;
    } else {
        load_legacy_storage(
            &read_txn,
            &mut library,
            has_ai_sessions_v2,
            has_ai_sessions_v3,
            has_ai_sessions_v4,
        )?;
    }
    load_document_preferences(
        &read_txn,
        &mut library,
        has_document_preferences_v3,
        has_v2_storage,
        has_v1_storage,
    )?;
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
    Ok(library)
}

fn load_storage_v2(
    read_txn: &redb::ReadTransaction,
    library: &mut Library,
    has_ai_sessions_v2: bool,
    has_ai_sessions_v3: bool,
    has_ai_sessions_v4: bool,
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
    load_ai_sessions(
        read_txn,
        library,
        has_ai_sessions_v2,
        has_ai_sessions_v3,
        has_ai_sessions_v4,
    )?;
    load_shared_v1_storage(read_txn, library)?;
    Ok(())
}

fn load_storage_v1(
    read_txn: &redb::ReadTransaction,
    library: &mut Library,
    has_ai_sessions_v2: bool,
    has_ai_sessions_v3: bool,
    has_ai_sessions_v4: bool,
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

    load_ai_sessions(
        read_txn,
        library,
        has_ai_sessions_v2,
        has_ai_sessions_v3,
        has_ai_sessions_v4,
    )?;
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
    Ok(())
}

fn load_ai_sessions(
    read_txn: &redb::ReadTransaction,
    library: &mut Library,
    has_ai_sessions_v2: bool,
    has_ai_sessions_v3: bool,
    has_ai_sessions_v4: bool,
) -> Result<()> {
    if has_ai_sessions_v4 {
        let table = read_txn.open_table(AI_SESSIONS_V4_TABLE)?;
        for entry in table.iter()? {
            let (_, value) = entry?;
            match decode::<StoredAiSessionV4>(value.value())
                .and_then(StoredAiSessionV4::into_domain)
            {
                Ok(mut entry) => {
                    entry.path = normalize_path(&entry.path);
                    library.ai_sessions.push(entry);
                }
                Err(error) => eprintln!("failed to decode AI session: {error:#}"),
            }
        }
    } else if has_ai_sessions_v3 {
        let table = read_txn.open_table(AI_SESSIONS_V3_TABLE)?;
        for entry in table.iter()? {
            let (_, value) = entry?;
            match decode::<StoredAiSessionV3>(value.value())
                .and_then(StoredAiSessionV3::into_domain)
            {
                Ok(mut entry) => {
                    entry.path = normalize_path(&entry.path);
                    library.ai_sessions.push(entry);
                }
                Err(error) => eprintln!("failed to decode AI session: {error:#}"),
            }
        }
    } else if has_ai_sessions_v2 {
        let table = read_txn.open_table(AI_SESSIONS_V2_TABLE)?;
        for entry in table.iter()? {
            let (_, value) = entry?;
            match decode::<StoredAiSessionV2>(value.value())
                .and_then(StoredAiSessionV2::into_domain)
            {
                Ok(mut entry) => {
                    entry.path = normalize_path(&entry.path);
                    library.ai_sessions.push(entry);
                }
                Err(error) => eprintln!("failed to decode AI session: {error:#}"),
            }
        }
    } else {
        let table = read_txn.open_table(AI_SESSIONS_V1_TABLE)?;
        for entry in table.iter()? {
            let (_, value) = entry?;
            match decode::<StoredAiSessionV1>(value.value())
                .and_then(StoredAiSessionV1::into_domain)
            {
                Ok(mut entry) => {
                    entry.path = normalize_path(&entry.path);
                    library.ai_sessions.push(entry);
                }
                Err(error) => eprintln!("failed to decode AI session: {error:#}"),
            }
        }
    }
    Ok(())
}

fn load_legacy_storage(
    read_txn: &redb::ReadTransaction,
    library: &mut Library,
    has_ai_sessions_v2: bool,
    has_ai_sessions_v3: bool,
    has_ai_sessions_v4: bool,
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

    if has_ai_sessions_v2 || has_ai_sessions_v3 || has_ai_sessions_v4 {
        load_ai_sessions(
            read_txn,
            library,
            has_ai_sessions_v2,
            has_ai_sessions_v3,
            has_ai_sessions_v4,
        )?;
    } else {
        let table = read_txn.open_table(AI_SESSIONS_TABLE)?;
        for entry in table.iter()? {
            let (_, value) = entry?;
            match decode::<LegacyAiSession>(value.value()) {
                Ok(entry) => {
                    let mut entry = entry.into_domain();
                    entry.path = normalize_path(&entry.path);
                    library.ai_sessions.push(entry);
                }
                Err(error) => eprintln!("failed to decode AI session: {error:#}"),
            }
        }
    }

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
