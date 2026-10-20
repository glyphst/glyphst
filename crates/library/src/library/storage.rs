use super::storage_helpers::*;
use super::*;

impl HistoryEntryPersistenceSnapshot {
    pub fn persist(self) -> Result<()> {
        let write_txn = self.database.begin_write()?;
        let is_current = self
            .latest_revisions
            .lock()
            .expect("history persistence revisions lock poisoned")
            .get(&self.entry.path)
            .is_some_and(|revision| *revision == self.revision);
        if !is_current {
            return Ok(());
        }
        {
            let mut table = write_txn.open_table(HISTORY_V2_TABLE)?;
            let key = history_key(&self.entry.path);
            let encoded = encode(&StoredHistoryEntryV2::from(&self.entry))?;
            table.insert(key.as_slice(), encoded.as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }
}

impl LibraryStore {
    pub fn memory() -> Self {
        Self {
            library: Library::default(),
            database: None,
            managed_library_root: None,
            ai_image_memory: HashMap::new(),
            startup_error: None,
            document_edit_histories: HashMap::new(),
            next_document_edit_sequence: 0,
            bookmarks_revision: 0,
            edit_recorder: None,
            history_entry_revisions: Arc::new(Mutex::new(HashMap::new())),
            catalog_revision: 0,
            ai_catalog: Default::default(),
            jobs: Vec::new(),
            next_job_id: 1,
            vault_notes: HashMap::new(),
            knowledge: Default::default(),
            files_root_chosen: false,
            sync: Default::default(),
        }
    }

    pub fn load() -> Self {
        let path = library_path();
        match Self::open_current_with_root(path, managed_library_root()) {
            Ok(store) => store,
            Err(error) => {
                eprintln!("failed to open document library database: {error:#}");
                let mut store = Self::memory();
                store.startup_error = Some(format!("{error:#}"));
                store
            }
        }
    }

    /// A library in `path` with its files under `root`, apart from the
    /// user's (for the screenshots' demos).
    pub fn open_apart(path: PathBuf, root: PathBuf) -> Result<Self> {
        Self::open_current_with_root(path, root)
    }

    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn open_for_test(path: PathBuf) -> Result<Self> {
        let managed_root = path.with_extension("managed-library");
        Self::open_current_with_root(path, managed_root)
    }

    pub(super) fn open_current_with_root(
        path: PathBuf,
        managed_library_root: PathBuf,
    ) -> Result<Self> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            create_private_directory(parent)?;
        }

        let (database, replaced) = open_replacing_old_format(&path)?;
        if let Some(aside) = replaced {
            eprintln!(
                "document library used an older database format; kept it at {} and started a new library",
                aside.display()
            );
        }
        restrict_library_file(&path)?;
        ensure_current_tables(&database)?;
        // The folder the user chose for the files, when they did.
        let chosen_root = super::files_root::read_files_root(&database)?;
        let files_root_chosen = chosen_root.is_some();
        let managed_library_root = chosen_root.unwrap_or(managed_library_root);
        ensure_managed_library_directories(&managed_library_root)?;
        let library = load_current_library(&database, &managed_library_root)?;
        let ai_catalog = super::ai_catalog::load_ai_catalog(&database)?;
        let jobs = super::jobs::load_library_jobs(&database)?;
        let next_job_id = jobs
            .iter()
            .map(|job| job.id.0)
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        let vault_notes = super::vault_notes::load_vault_notes(&database)?;
        let knowledge = super::knowledge::load_knowledge(&database)?;
        let mut sync = super::syncing::load_sync(&database)?;
        sync.database_path = Some(path.clone());

        let mut store = Self {
            library,
            database: Some(Arc::new(database)),
            managed_library_root: Some(managed_library_root),
            ai_image_memory: HashMap::new(),
            startup_error: None,
            document_edit_histories: HashMap::new(),
            next_document_edit_sequence: 0,
            bookmarks_revision: 0,
            edit_recorder: None,
            history_entry_revisions: Arc::new(Mutex::new(HashMap::new())),
            catalog_revision: 0,
            ai_catalog,
            jobs,
            next_job_id,
            vault_notes,
            knowledge,
            files_root_chosen,
            sync,
        };
        store.library.reserve_synced_ids(&store.sync.ids);
        store.assign_sync_ids()?;
        store.sanitize_ai_catalog();
        store.prune_library_jobs();
        if let Err(error) = store.prune_ai_session_rows() {
            eprintln!("failed to prune expired AI sessions: {error:#}");
        }
        Ok(store)
    }

    pub fn startup_error(&self) -> Option<&str> {
        self.startup_error.as_deref()
    }
    pub(super) fn write_history(&self) -> Result<()> {
        self.write_history_entries(&self.library.history)
    }

    pub(super) fn write_history_entries(&self, history: &[HistoryEntry]) -> Result<()> {
        let Some(database) = &self.database else {
            return Err(anyhow!("document library database is unavailable"));
        };
        self.history_entry_revisions
            .lock()
            .expect("history persistence revisions lock poisoned")
            .clear();
        Self::write_history_entries_to_database(database, history)
    }

    fn write_history_entries_to_database(
        database: &Database,
        history: &[HistoryEntry],
    ) -> Result<()> {
        let valid_paths = history
            .iter()
            .map(|entry| history_key(&entry.path))
            .collect::<HashSet<_>>();
        let write_txn = database.begin_write()?;
        {
            let mut table = write_txn.open_table(HISTORY_V2_TABLE)?;
            let mut existing_paths = Vec::new();
            for entry in table.iter()? {
                let (path, _) = entry?;
                existing_paths.push(path.value().to_vec());
            }

            for path in existing_paths {
                if !valid_paths.contains(&path) {
                    table.remove(path.as_slice())?;
                }
            }

            for entry in history {
                let key = history_key(&entry.path);
                let encoded = encode(&StoredHistoryEntryV2::from(entry))?;
                table.insert(key.as_slice(), encoded.as_slice())?;
            }
        }
        write_txn.commit()?;
        Ok(())
    }

    pub(super) fn write_bookmark(&self, bookmark: &Bookmark) -> Result<()> {
        self.synced_write();
        let Some(database) = &self.database else {
            return Err(anyhow!("document library database is unavailable"));
        };
        let write_txn = database.begin_write()?;
        {
            let mut table = write_txn.open_table(BOOKMARKS_V2_TABLE)?;
            let encoded = encode(&StoredBookmarkV2::from(bookmark))?;
            table.insert(bookmark.id, encoded.as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }

    pub(super) fn remove_bookmark_from_database(&self, id: u64) -> Result<()> {
        self.synced_write();
        let Some(database) = &self.database else {
            return Err(anyhow!("document library database is unavailable"));
        };
        let write_txn = database.begin_write()?;
        {
            let mut table = write_txn.open_table(BOOKMARKS_V2_TABLE)?;
            table.remove(id)?;
        }
        write_txn.commit()?;
        Ok(())
    }

    pub(super) fn write_highlight(&self, highlight: &Highlight) -> Result<()> {
        self.synced_write();
        let Some(database) = &self.database else {
            return Err(anyhow!("document library database is unavailable"));
        };
        let write_txn = database.begin_write()?;
        {
            let mut table = write_txn.open_table(HIGHLIGHTS_V2_TABLE)?;
            let encoded = encode(&StoredHighlightV2::from(highlight))?;
            table.insert(highlight.id, encoded.as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }

    pub(super) fn remove_highlight_from_database(&self, id: u64) -> Result<()> {
        self.synced_write();
        let Some(database) = &self.database else {
            return Err(anyhow!("document library database is unavailable"));
        };
        let write_txn = database.begin_write()?;
        {
            let mut table = write_txn.open_table(HIGHLIGHTS_V2_TABLE)?;
            table.remove(id)?;
        }
        write_txn.commit()?;
        Ok(())
    }

    pub(super) fn write_note(&self, note: &Note) -> Result<()> {
        self.synced_write();
        let Some(database) = &self.database else {
            return Err(anyhow!("document library database is unavailable"));
        };
        let write_txn = database.begin_write()?;
        {
            let mut table = write_txn.open_table(NOTES_V2_TABLE)?;
            let encoded = encode(&StoredNoteV2::from(note))?;
            table.insert(note.id, encoded.as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }

    pub(super) fn remove_note_from_database(&self, id: u64) -> Result<()> {
        self.synced_write();
        let Some(database) = &self.database else {
            return Err(anyhow!("document library database is unavailable"));
        };
        let write_txn = database.begin_write()?;
        {
            let mut table = write_txn.open_table(NOTES_V2_TABLE)?;
            table.remove(id)?;
        }
        write_txn.commit()?;
        Ok(())
    }

    pub(super) fn write_ai_session_changes(
        &self,
        session: &AiSession,
        removed_ids: &[u64],
    ) -> Result<()> {
        let Some(database) = &self.database else {
            return Ok(());
        };
        let write_txn = database.begin_write()?;
        {
            let mut table = write_txn.open_table(AI_SESSIONS_V6_TABLE)?;
            let encoded = encode(&StoredAiSessionV6::from(session))?;
            table.insert(session.id, encoded.as_slice())?;
            for id in removed_ids {
                table.remove(*id)?;
            }
        }
        write_txn.commit()?;
        Ok(())
    }

    fn prune_ai_session_rows(&self) -> Result<()> {
        let Some(database) = &self.database else {
            return Ok(());
        };
        let retained_ids = self
            .library
            .ai_sessions
            .iter()
            .map(|session| session.id)
            .collect::<HashSet<_>>();
        let write_txn = database.begin_write()?;
        {
            let mut table = write_txn.open_table(AI_SESSIONS_V6_TABLE)?;
            let stale_ids = table
                .iter()?
                .filter_map(|entry| entry.ok().map(|(id, _)| id.value()))
                .filter(|id| !retained_ids.contains(id))
                .collect::<Vec<_>>();
            for id in stale_ids {
                table.remove(id)?;
            }
        }
        write_txn.commit()?;
        Ok(())
    }

    pub(super) fn write_sidebar_widths(&self, widths: SidebarWidths) -> Result<()> {
        let Some(database) = &self.database else {
            return Ok(());
        };
        let write_txn = database.begin_write()?;
        {
            let mut table = write_txn.open_table(UI_STATE_V1_TABLE)?;
            let encoded = encode(&StoredSidebarWidthsV2::from(widths))?;
            table.insert(SIDEBAR_WIDTHS_KEY, encoded.as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }

    pub(super) fn write_document_browser_size(&self, size: DocumentBrowserSize) -> Result<()> {
        let Some(database) = &self.database else {
            return Ok(());
        };
        let write_txn = database.begin_write()?;
        {
            let mut table = write_txn.open_table(UI_STATE_V1_TABLE)?;
            let encoded = encode(&StoredDocumentBrowserSizeV1::from(size))?;
            table.insert(DOCUMENT_BROWSER_SIZE_KEY, encoded.as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }

    pub(super) fn write_home_preferences(&self, preferences: &HomePreferences) -> Result<()> {
        let Some(database) = &self.database else {
            return Ok(());
        };
        let write_txn = database.begin_write()?;
        {
            let mut table = write_txn.open_table(UI_STATE_V1_TABLE)?;
            let encoded = encode(&StoredHomePreferencesV1::from(preferences))?;
            table.insert(HOME_PREFERENCES_KEY, encoded.as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }

    pub(super) fn write_reader_session(&self, session: &ReaderSessionSnapshot) -> Result<()> {
        let Some(database) = &self.database else {
            return Ok(());
        };
        let write_txn = database.begin_write()?;
        {
            let mut table = write_txn.open_table(UI_STATE_V1_TABLE)?;
            let encoded = encode(&StoredReaderSessionV4::from(session))?;
            table.insert(READER_SESSION_KEY, encoded.as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }

    pub(super) fn write_document_preferences(
        &self,
        preferences: &DocumentPreferences,
    ) -> Result<()> {
        let Some(database) = &self.database else {
            return Err(anyhow!("document library database is unavailable"));
        };
        let key = history_key(&preferences.path);
        let encoded = encode(&StoredDocumentPreferencesV4::from(preferences))?;
        let write_txn = database.begin_write()?;
        {
            let mut table = write_txn.open_table(DOCUMENT_PREFERENCES_V4_TABLE)?;
            table.insert(key.as_slice(), encoded.as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }

    pub(super) fn delete_document_preferences(&self, path: &Path) -> Result<()> {
        let Some(database) = &self.database else {
            return Err(anyhow!("document library database is unavailable"));
        };
        let key = history_key(path);
        let write_txn = database.begin_write()?;
        {
            let mut table = write_txn.open_table(DOCUMENT_PREFERENCES_V4_TABLE)?;
            table.remove(key.as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }

    pub(super) fn write_catalog(&self) -> Result<()> {
        self.synced_write();
        let Some(database) = &self.database else {
            return Ok(());
        };
        let write_txn = database.begin_write()?;
        let root = self
            .managed_library_root
            .as_deref()
            .ok_or_else(|| anyhow!("managed library storage is unavailable"))?;
        write_catalog_tables(&write_txn, &self.library, root)?;
        write_txn.commit()?;
        Ok(())
    }

    pub(super) fn write_relinked_library(&self) -> Result<()> {
        self.synced_write();
        let Some(database) = &self.database else {
            return Ok(());
        };
        let write_txn = database.begin_write()?;
        let root = self
            .managed_library_root
            .as_deref()
            .ok_or_else(|| anyhow!("managed library storage is unavailable"))?;
        write_catalog_tables(&write_txn, &self.library, root)?;
        {
            let mut table = write_txn.open_table(HISTORY_V2_TABLE)?;
            let keys = table
                .iter()?
                .filter_map(|entry| entry.ok().map(|(key, _)| key.value().to_vec()))
                .collect::<Vec<_>>();
            for key in keys {
                table.remove(key.as_slice())?;
            }
            for entry in &self.library.history {
                let key = history_key(&entry.path);
                let encoded = encode(&StoredHistoryEntryV2::from(entry))?;
                table.insert(key.as_slice(), encoded.as_slice())?;
            }
        }
        {
            let mut table = write_txn.open_table(BOOKMARKS_V2_TABLE)?;
            let ids = table
                .iter()?
                .filter_map(|entry| entry.ok().map(|(id, _)| id.value()))
                .collect::<Vec<_>>();
            for id in ids {
                table.remove(id)?;
            }
            for entry in &self.library.bookmarks {
                let encoded = encode(&StoredBookmarkV2::from(entry))?;
                table.insert(entry.id, encoded.as_slice())?;
            }
        }
        {
            let mut table = write_txn.open_table(HIGHLIGHTS_V2_TABLE)?;
            let ids = table
                .iter()?
                .filter_map(|entry| entry.ok().map(|(id, _)| id.value()))
                .collect::<Vec<_>>();
            for id in ids {
                table.remove(id)?;
            }
            for entry in &self.library.highlights {
                let encoded = encode(&StoredHighlightV2::from(entry))?;
                table.insert(entry.id, encoded.as_slice())?;
            }
        }
        {
            let mut table = write_txn.open_table(NOTES_V2_TABLE)?;
            let ids = table
                .iter()?
                .filter_map(|entry| entry.ok().map(|(id, _)| id.value()))
                .collect::<Vec<_>>();
            for id in ids {
                table.remove(id)?;
            }
            for entry in &self.library.notes {
                let encoded = encode(&StoredNoteV2::from(entry))?;
                table.insert(entry.id, encoded.as_slice())?;
            }
        }
        {
            let mut table = write_txn.open_table(AI_SESSIONS_V6_TABLE)?;
            let ids = table
                .iter()?
                .filter_map(|entry| entry.ok().map(|(id, _)| id.value()))
                .collect::<Vec<_>>();
            for id in ids {
                table.remove(id)?;
            }
            for entry in &self.library.ai_sessions {
                let encoded = encode(&StoredAiSessionV6::from(entry))?;
                table.insert(entry.id, encoded.as_slice())?;
            }
        }
        {
            let mut table = write_txn.open_table(DOCUMENT_PREFERENCES_V4_TABLE)?;
            let keys = table
                .iter()?
                .filter_map(|entry| entry.ok().map(|(key, _)| key.value().to_vec()))
                .collect::<Vec<_>>();
            for key in keys {
                table.remove(key.as_slice())?;
            }
            for entry in &self.library.document_preferences {
                let key = history_key(&entry.path);
                let encoded = encode(&StoredDocumentPreferencesV4::from(entry))?;
                table.insert(key.as_slice(), encoded.as_slice())?;
            }
        }
        write_txn.commit()?;
        Ok(())
    }
}

pub(super) fn write_catalog_tables(
    write_txn: &redb::WriteTransaction,
    library: &Library,
    managed_root: &Path,
) -> Result<()> {
    {
        let retained = library
            .library_items
            .iter()
            .map(|item| item.id.0)
            .collect::<HashSet<_>>();
        let mut table = write_txn.open_table(LIBRARY_ITEMS_V1_TABLE)?;
        let stale = table
            .iter()?
            .filter_map(|entry| entry.ok().map(|(id, _)| id.value()))
            .filter(|id| !retained.contains(id))
            .collect::<Vec<_>>();
        for id in stale {
            table.remove(id)?;
        }
        for item in &library.library_items {
            let encoded = encode(&StoredLibraryItemV1::new(item))?;
            table.insert(item.id.0, encoded.as_slice())?;
        }
    }
    {
        let retained = library
            .library_attachments
            .iter()
            .filter(|attachment| !attachment.is_linked())
            .map(|attachment| attachment.id.0)
            .collect::<HashSet<_>>();
        let mut table = write_txn.open_table(LIBRARY_ATTACHMENTS_V2_TABLE)?;
        let stale = table
            .iter()?
            .filter_map(|entry| entry.ok().map(|(id, _)| id.value()))
            .filter(|id| !retained.contains(id))
            .collect::<Vec<_>>();
        for id in stale {
            table.remove(id)?;
        }
        for attachment in library
            .library_attachments
            .iter()
            .filter(|attachment| !attachment.is_linked())
        {
            let encoded = encode(&StoredLibraryAttachmentV2::new(attachment, managed_root)?)?;
            table.insert(attachment.id.0, encoded.as_slice())?;
        }
    }
    {
        let retained = library
            .library_attachments
            .iter()
            .filter(|attachment| attachment.is_linked())
            .map(|attachment| attachment.id.0)
            .collect::<HashSet<_>>();
        let mut table = write_txn.open_table(LIBRARY_LINKED_ATTACHMENTS_V1_TABLE)?;
        let stale = table
            .iter()?
            .filter_map(|entry| entry.ok().map(|(id, _)| id.value()))
            .filter(|id| !retained.contains(id))
            .collect::<Vec<_>>();
        for id in stale {
            table.remove(id)?;
        }
        for attachment in library
            .library_attachments
            .iter()
            .filter(|attachment| attachment.is_linked())
        {
            let encoded = encode(&StoredLinkedAttachmentV1::new(attachment)?)?;
            table.insert(attachment.id.0, encoded.as_slice())?;
        }
    }
    {
        let retained = library
            .library_tags
            .iter()
            .map(|tag| tag.id.0)
            .collect::<HashSet<_>>();
        let mut table = write_txn.open_table(LIBRARY_TAGS_V1_TABLE)?;
        let stale = table
            .iter()?
            .filter_map(|entry| entry.ok().map(|(id, _)| id.value()))
            .filter(|id| !retained.contains(id))
            .collect::<Vec<_>>();
        for id in stale {
            table.remove(id)?;
        }
        for tag in &library.library_tags {
            let encoded = encode(&StoredLibraryTagV1::new(tag))?;
            table.insert(tag.id.0, encoded.as_slice())?;
        }
    }
    {
        let mut grouped = HashMap::<u64, Vec<LibraryTagId>>::new();
        for relation in &library.library_item_tags {
            grouped
                .entry(relation.item_id.0)
                .or_default()
                .push(relation.tag_id);
        }
        let retained = grouped.keys().copied().collect::<HashSet<_>>();
        let mut table = write_txn.open_table(LIBRARY_ITEM_TAGS_V1_TABLE)?;
        let stale = table
            .iter()?
            .filter_map(|entry| entry.ok().map(|(id, _)| id.value()))
            .filter(|id| !retained.contains(id))
            .collect::<Vec<_>>();
        for id in stale {
            table.remove(id)?;
        }
        for (item_id, mut tag_ids) in grouped {
            tag_ids.sort_by_key(|tag_id| tag_id.0);
            tag_ids.dedup();
            let encoded = encode(&StoredLibraryItemTagsV1::new(tag_ids))?;
            table.insert(item_id, encoded.as_slice())?;
        }
    }
    {
        let retained = library
            .library_saved_views
            .iter()
            .map(|view| view.id.0)
            .collect::<HashSet<_>>();
        let mut table = write_txn.open_table(LIBRARY_SAVED_VIEWS_V1_TABLE)?;
        let stale = table
            .iter()?
            .filter_map(|entry| entry.ok().map(|(id, _)| id.value()))
            .filter(|id| !retained.contains(id))
            .collect::<Vec<_>>();
        for id in stale {
            table.remove(id)?;
        }
        for view in &library.library_saved_views {
            let encoded = encode(&StoredLibrarySavedViewV1::new(view))?;
            table.insert(view.id.0, encoded.as_slice())?;
        }
    }
    {
        let retained = library
            .library_collections
            .iter()
            .map(|collection| collection.id.0)
            .collect::<HashSet<_>>();
        let mut table = write_txn.open_table(LIBRARY_COLLECTIONS_V1_TABLE)?;
        let stale = table
            .iter()?
            .filter_map(|entry| entry.ok().map(|(id, _)| id.value()))
            .filter(|id| !retained.contains(id))
            .collect::<Vec<_>>();
        for id in stale {
            table.remove(id)?;
        }
        for collection in &library.library_collections {
            let encoded = encode(&StoredLibraryCollectionV1::new(collection))?;
            table.insert(collection.id.0, encoded.as_slice())?;
        }
    }
    {
        let retained = library
            .library_item_locations
            .iter()
            .map(|location| location.item_id.0)
            .collect::<HashSet<_>>();
        let mut table = write_txn.open_table(LIBRARY_ITEM_LOCATIONS_V2_TABLE)?;
        let stale = table
            .iter()?
            .filter_map(|entry| entry.ok().map(|(id, _)| id.value()))
            .filter(|id| !retained.contains(id))
            .collect::<Vec<_>>();
        for id in stale {
            table.remove(id)?;
        }
        for location in &library.library_item_locations {
            let encoded = encode(&StoredLibraryItemLocationV2::new(location))?;
            table.insert(location.item_id.0, encoded.as_slice())?;
        }
    }
    {
        // Every item gets a row, an empty one when it is in no collection,
        // so an empty table only ever means a library from before this one.
        let mut grouped = library
            .library_items
            .iter()
            .map(|item| (item.id.0, Vec::new()))
            .collect::<HashMap<u64, Vec<LibraryCollectionId>>>();
        for membership in &library.library_item_collections {
            grouped
                .entry(membership.item_id.0)
                .or_default()
                .push(membership.collection_id);
        }
        let mut table = write_txn.open_table(LIBRARY_ITEM_COLLECTIONS_V1_TABLE)?;
        let stale = table
            .iter()?
            .filter_map(|entry| entry.ok().map(|(id, _)| id.value()))
            .filter(|id| !grouped.contains_key(id))
            .collect::<Vec<_>>();
        for id in stale {
            table.remove(id)?;
        }
        for (item_id, collection_ids) in grouped {
            let encoded = encode(&StoredLibraryItemCollectionsV1::new(collection_ids))?;
            table.insert(item_id, encoded.as_slice())?;
        }
    }
    super::item_links::write_item_link_tables(write_txn, library)?;
    Ok(())
}

/// Open a database, or replace one written in an older redb file format.
///
/// redb 4 cannot read files from redb 2 and Glyphst carries no migration, so
/// such a file is renamed aside (never deleted) and a fresh database is
/// created in its place. Returns the renamed file's path when that happened.
pub(super) fn open_replacing_old_format(path: &Path) -> Result<(Database, Option<PathBuf>)> {
    let opened = if path.exists() {
        Database::open(path)
    } else {
        Database::create(path)
    };
    match opened {
        Ok(database) => Ok((database, None)),
        Err(redb::DatabaseError::UpgradeRequired(version)) => {
            let mut aside = path.with_extension(format!("redb{version}-format"));
            let mut suffix = 1;
            while aside.exists() {
                aside = path.with_extension(format!("redb{version}-format-{suffix}"));
                suffix += 1;
            }
            fs::rename(path, &aside).with_context(|| {
                format!(
                    "failed to move {} aside to {}",
                    path.display(),
                    aside.display()
                )
            })?;
            let database = Database::create(path)
                .with_context(|| format!("failed to create {}", path.display()))?;
            Ok((database, Some(aside)))
        }
        Err(error) => Err(error).with_context(|| format!("failed to open {}", path.display())),
    }
}
