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
            startup_error: None,
            document_edit_histories: HashMap::new(),
            next_document_edit_sequence: 0,
            bookmarks_revision: 0,
            history_entry_revisions: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn load() -> Self {
        let path = library_path();
        match Self::open(path) {
            Ok(store) => store,
            Err(error) => {
                eprintln!("failed to open document library database: {error:#}");
                let mut store = Self::memory();
                store.startup_error = Some(format!("{error:#}"));
                store
            }
        }
    }

    pub(super) fn open(path: PathBuf) -> Result<Self> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            create_private_directory(parent)?;
        }

        let database = if path.exists() {
            Database::open(&path).with_context(|| format!("failed to open {}", path.display()))?
        } else {
            Database::create(&path)
                .with_context(|| format!("failed to create {}", path.display()))?
        };
        restrict_library_file(&path)?;
        ensure_tables(&database)?;
        let library = load_library(&database)?;

        let store = Self {
            library,
            database: Some(Arc::new(database)),
            startup_error: None,
            document_edit_histories: HashMap::new(),
            next_document_edit_sequence: 0,
            bookmarks_revision: 0,
            history_entry_revisions: Arc::new(Mutex::new(HashMap::new())),
        };
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
            let mut table = write_txn.open_table(AI_SESSIONS_V3_TABLE)?;
            let encoded = encode(&StoredAiSessionV3::from(session))?;
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
            let mut table = write_txn.open_table(AI_SESSIONS_V3_TABLE)?;
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
            let encoded = encode(&StoredSidebarWidthsV1::from(widths))?;
            table.insert(SIDEBAR_WIDTHS_KEY, encoded.as_slice())?;
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
        let encoded = encode(&StoredDocumentPreferencesV3::from(preferences))?;
        let write_txn = database.begin_write()?;
        {
            let mut table = write_txn.open_table(DOCUMENT_PREFERENCES_V3_TABLE)?;
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
            let mut table = write_txn.open_table(DOCUMENT_PREFERENCES_V3_TABLE)?;
            table.remove(key.as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }
}
