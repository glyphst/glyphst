use super::*;

impl LibraryStore {
    pub fn history_entries(&self) -> &[HistoryEntry] {
        &self.library.history
    }
    pub fn remove_missing_history_entries(&mut self) -> Result<bool> {
        let mut history = self.library.history.clone();
        history.retain(|entry| entry.path.exists());
        if history.len() == self.library.history.len() {
            return Ok(false);
        }

        self.write_history_entries(&history)?;
        self.library.history = history;
        Ok(true)
    }
    pub fn history_position(&self, path: &Path) -> Option<DocumentPosition> {
        let path = normalize_path(path);
        self.library
            .history
            .iter()
            .find(|entry| entry.path == path)
            .map(|entry| entry.position.clone())
    }

    pub fn record_history(&mut self, path: &Path, position: DocumentPosition) {
        self.library
            .record_history_at(path, position, current_timestamp());
    }

    pub fn remove_history_entries(&mut self, paths: &[PathBuf]) -> Result<usize> {
        let path_set: HashSet<PathBuf> = paths.iter().map(|p| normalize_path(p)).collect();
        let mut history = self.library.history.clone();
        history.retain(|entry| !path_set.contains(&entry.path));
        let removed = self.library.history.len() - history.len();
        if removed > 0 {
            self.write_history_entries(&history)?;
            self.library.history = history;
        }
        Ok(removed)
    }
    pub fn save(&self) -> Result<()> {
        self.write_history()
    }

    pub fn history_entry_persistence_snapshot(
        &self,
        path: &Path,
    ) -> Option<HistoryEntryPersistenceSnapshot> {
        let path = normalize_path(path);
        let entry = self
            .library
            .history
            .iter()
            .find(|entry| entry.path == path)?
            .clone();
        let revision = {
            let mut revisions = self
                .history_entry_revisions
                .lock()
                .expect("history persistence revisions lock poisoned");
            let revision = revisions.entry(path).or_default();
            *revision = revision.wrapping_add(1);
            *revision
        };
        Some(HistoryEntryPersistenceSnapshot {
            database: self.database.clone()?,
            entry,
            latest_revisions: self.history_entry_revisions.clone(),
            revision,
        })
    }
}
