use super::edits::*;
use super::*;

impl LibraryStore {
    pub fn bookmarks(&self) -> &[Bookmark] {
        &self.library.bookmarks
    }
    pub fn add_bookmark(
        &mut self,
        path: &Path,
        label: impl Into<String>,
        position: DocumentPosition,
    ) -> Result<u64> {
        let mut bookmark = Bookmark {
            id: self.library.next_bookmark_id.max(1),
            path: normalize_path(path),
            label: label.into(),
            position,
            created: current_timestamp(),
        };
        bookmark.normalize();
        self.replace_bookmark_snapshot(None, Some(&bookmark))?;
        self.record_document_edit(DocumentEdit::Bookmark {
            before: None,
            after: Some(bookmark.clone()),
        });
        Ok(bookmark.id)
    }

    pub fn remove_bookmark(&mut self, id: u64) -> Result<bool> {
        let Some(bookmark) = self
            .library
            .bookmarks
            .iter()
            .find(|bookmark| bookmark.id == id)
            .cloned()
        else {
            return Ok(false);
        };

        self.replace_bookmark_snapshot(Some(&bookmark), None)?;
        self.record_document_edit(DocumentEdit::Bookmark {
            before: Some(bookmark),
            after: None,
        });
        Ok(true)
    }
    pub(super) fn replace_bookmark_snapshot(
        &mut self,
        expected: Option<&Bookmark>,
        target: Option<&Bookmark>,
    ) -> Result<()> {
        let id = validate_snapshot_replacement(
            &self.library.bookmarks,
            expected,
            target,
            |bookmark| bookmark.id,
            |bookmark| bookmark.path.as_path(),
            validate_bookmark_snapshot,
            "bookmark",
        )?;

        if let Some(bookmark) = target {
            self.write_bookmark(bookmark)?;
        } else {
            self.remove_bookmark_from_database(id)?;
        }
        self.library.apply_bookmark_snapshot(id, target.cloned());
        Ok(())
    }
}
