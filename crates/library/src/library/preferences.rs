use super::*;

impl LibraryStore {
    pub fn document_target_language(&self, path: &Path) -> Option<String> {
        let path = normalize_path(path);
        self.library
            .document_preferences
            .iter()
            .find(|preferences| preferences.path == path)
            .map(|preferences| preferences.target_language.clone())
            .filter(|target_language| !target_language.is_empty())
    }

    pub fn epub_reader_preferences(&self, path: &Path) -> Option<EpubReaderPreferences> {
        let path = normalize_path(path);
        self.library
            .document_preferences
            .iter()
            .find(|preferences| preferences.path == path)
            .and_then(|preferences| preferences.epub)
    }

    pub fn document_rotation(&self, path: &Path) -> DocumentRotation {
        let path = normalize_path(path);
        self.library
            .document_preferences
            .iter()
            .find(|preferences| preferences.path == path)
            .map(|preferences| preferences.rotation)
            .unwrap_or_default()
    }

    pub(super) fn update_document_preferences(
        &mut self,
        path: &Path,
        update: impl FnOnce(&mut DocumentPreferences),
    ) -> Result<()> {
        let path = normalize_path(path);
        let existing_index = self
            .library
            .document_preferences
            .iter()
            .position(|preferences| preferences.path == path);
        let mut preferences = existing_index
            .map(|index| self.library.document_preferences[index].clone())
            .unwrap_or_else(|| DocumentPreferences {
                path: path.clone(),
                target_language: String::new(),
                epub: None,
                rotation: DocumentRotation::default(),
            });
        let previous = preferences.clone();
        update(&mut preferences);
        if preferences == previous {
            return Ok(());
        }
        if preferences.is_empty() {
            if let Some(index) = existing_index {
                self.delete_document_preferences(&path)?;
                self.library.document_preferences.remove(index);
            }
            return Ok(());
        }

        self.write_document_preferences(&preferences)?;
        if let Some(index) = existing_index {
            self.library.document_preferences[index] = preferences;
        } else {
            let index = self
                .library
                .document_preferences
                .binary_search_by(|existing| existing.path.cmp(&path))
                .unwrap_or_else(|index| index);
            self.library.document_preferences.insert(index, preferences);
        }
        Ok(())
    }

    pub fn set_document_target_language(
        &mut self,
        path: &Path,
        target_language: impl Into<String>,
    ) -> Result<()> {
        let path = normalize_path(path);
        let target_language = target_language.into().trim().to_ascii_lowercase();
        if target_language.is_empty() {
            return Err(anyhow!("document target language cannot be empty"));
        }
        self.update_document_preferences(&path, |preferences| {
            preferences.target_language = target_language;
        })
    }

    pub fn set_epub_reader_preferences(
        &mut self,
        path: &Path,
        preferences: EpubReaderPreferences,
    ) -> Result<()> {
        let preferences = preferences.sanitized();
        self.update_document_preferences(path, |document_preferences| {
            document_preferences.epub = Some(preferences);
        })
    }

    pub fn set_document_rotation(
        &mut self,
        path: &Path,
        rotation: DocumentRotation,
    ) -> Result<bool> {
        let path = normalize_path(path);
        let before = self.document_rotation(&path);
        if before == rotation {
            return Ok(false);
        }
        self.update_document_preferences(&path, |preferences| {
            preferences.rotation = rotation;
        })?;
        self.record_document_edit(DocumentEdit::Rotation {
            path,
            before,
            after: rotation,
        });
        Ok(true)
    }

    pub fn clear_epub_reader_preferences(&mut self, path: &Path) -> Result<()> {
        self.update_document_preferences(path, |preferences| {
            preferences.epub = None;
        })
    }
}
