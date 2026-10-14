//! Edits recorded for an assistant's change log: each is a before/after
//! snapshot that can be reverted and re-applied on its own, and refuses
//! when the data changed since.

use super::*;

/// One change to the library, as a before/after snapshot.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LibraryEdit(EditKind);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
enum EditKind {
    Document(Box<DocumentEdit>),
    Item {
        before: Box<LibraryItem>,
        after: Box<LibraryItem>,
    },
}

impl LibraryEdit {
    pub(super) fn document(edit: DocumentEdit) -> Self {
        Self(EditKind::Document(Box::new(edit)))
    }

    pub(super) fn item(before: LibraryItem, after: LibraryItem) -> Self {
        Self(EditKind::Item {
            before: Box::new(before),
            after: Box::new(after),
        })
    }

    /// The document the edit belongs to, for annotations and bookmarks.
    pub fn path(&self) -> Option<&Path> {
        match &self.0 {
            EditKind::Document(edit) => Some(edit.path()),
            EditKind::Item { .. } => None,
        }
    }

    /// The 0-based page the edit is on, when it has one.
    pub fn page(&self) -> Option<usize> {
        let first_rect = |rects: &[HighlightRect]| rects.first().map(|rect| rect.page_idx);
        let EditKind::Document(edit) = &self.0 else {
            return None;
        };
        match edit.as_ref() {
            DocumentEdit::Highlight { before, after } => before
                .as_ref()
                .or(after.as_ref())
                .and_then(|highlight| first_rect(&highlight.rects)),
            DocumentEdit::HighlightColor { before, .. } => first_rect(&before.rects),
            DocumentEdit::Note { before, after } => before
                .as_ref()
                .or(after.as_ref())
                .and_then(|note| first_rect(&note.rects)),
            DocumentEdit::NoteUpdate { before, .. } => first_rect(&before.rects),
            DocumentEdit::Bookmark { before, after } => before
                .as_ref()
                .or(after.as_ref())
                .map(|bookmark| bookmark.position.page.saturating_sub(1)),
            _ => None,
        }
    }

    /// A short description, such as "Highlighted “a quote”".
    pub fn describe(&self) -> String {
        let quote = |text: &str| {
            let short = text.chars().take(40).collect::<String>();
            if text.chars().count() > 40 {
                format!("“{short}…”")
            } else {
                format!("“{short}”")
            }
        };
        match &self.0 {
            EditKind::Document(edit) => match edit.as_ref() {
                DocumentEdit::Highlight {
                    after: Some(highlight),
                    before: None,
                } => format!("Highlighted {}", quote(&highlight.text)),
                DocumentEdit::Highlight {
                    before: Some(highlight),
                    ..
                } => format!("Removed the highlight {}", quote(&highlight.text)),
                DocumentEdit::HighlightColor { before, .. } => {
                    format!("Recolored the highlight {}", quote(&before.text))
                }
                DocumentEdit::Note {
                    after: Some(note),
                    before: None,
                } => format!("Added a note on {}", quote(&note.text)),
                DocumentEdit::Note {
                    before: Some(note), ..
                } => format!("Removed the note on {}", quote(&note.text)),
                DocumentEdit::NoteUpdate { before, .. } => {
                    format!("Edited the note on {}", quote(&before.text))
                }
                DocumentEdit::Bookmark {
                    after: Some(bookmark),
                    before: None,
                } => format!("Bookmarked page {}", bookmark.position.page),
                DocumentEdit::Bookmark {
                    before: Some(bookmark),
                    ..
                } => format!("Removed the bookmark on page {}", bookmark.position.page),
                DocumentEdit::Rotation { .. } => "Rotated the document".to_string(),
                _ => "Changed an annotation".to_string(),
            },
            EditKind::Item { before, after } => {
                if before.status != after.status {
                    format!("Marked “{}” as {:?}", after.title, after.status)
                } else if before.work_note != after.work_note {
                    format!("Edited the note of “{}”", after.title)
                } else {
                    format!("Changed “{}”", after.title)
                }
            }
        }
    }
}

/// Whether an error says the data changed since the edit, so it can no
/// longer be reverted or re-applied.
pub fn is_edit_conflict(error: &anyhow::Error) -> bool {
    error.downcast_ref::<DocumentEditConflict>().is_some()
}

impl LibraryStore {
    /// Runs `change` and returns the edits it made, besides recording them
    /// in the documents' undo histories as usual.
    pub fn record_edits<R>(
        &mut self,
        change: impl FnOnce(&mut Self) -> R,
    ) -> (R, Vec<LibraryEdit>) {
        let outer = self.edit_recorder.replace(Vec::new());
        let result = change(self);
        let edits = std::mem::replace(&mut self.edit_recorder, outer).unwrap_or_default();
        if let Some(outer) = &mut self.edit_recorder {
            outer.extend(edits.iter().cloned());
        }
        (result, edits)
    }

    /// Puts back what was there before the edit.
    pub fn revert_edit(&mut self, edit: &LibraryEdit) -> Result<()> {
        self.apply_agent_edit(edit, DocumentEditDirection::Undo)
    }

    /// Makes the edit again after [`Self::revert_edit`].
    pub fn reapply_edit(&mut self, edit: &LibraryEdit) -> Result<()> {
        self.apply_agent_edit(edit, DocumentEditDirection::Redo)
    }

    fn apply_agent_edit(
        &mut self,
        edit: &LibraryEdit,
        direction: DocumentEditDirection,
    ) -> Result<()> {
        match &edit.0 {
            EditKind::Document(edit) => {
                self.apply_document_edit(edit, direction)?;
                if let Some(history) = self
                    .document_edit_histories
                    .get_mut(&normalize_path(edit.path()))
                {
                    edit.bump_revision(&mut history.revision);
                }
                if edit.is_bookmark() {
                    self.bookmarks_revision = self.bookmarks_revision.wrapping_add(1);
                }
                Ok(())
            }
            EditKind::Item { before, after } => {
                let (expected, target) = match direction {
                    DocumentEditDirection::Undo => (after, before),
                    DocumentEditDirection::Redo => (before, after),
                };
                let current = self
                    .library_item(expected.id)
                    .ok_or_else(|| anyhow!("library item {} does not exist", expected.id.0))?;
                if current.status != expected.status
                    || current.work_note != expected.work_note
                    || current.favorite != expected.favorite
                {
                    return Err(DocumentEditConflict("library item").into());
                }
                let (status, note, favorite) =
                    (target.status, target.work_note.clone(), target.favorite);
                self.persist_item_fields(expected.id, status, note, favorite)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(page_idx: usize) -> HighlightRect {
        HighlightRect {
            page_idx,
            x0: 1.0,
            y0: 2.0,
            x1: 3.0,
            y1: 4.0,
        }
    }

    #[test]
    fn recorded_edits_revert_reapply_and_detect_conflicts() {
        let database = std::env::temp_dir().join(format!(
            "glyphst-agent-edits-{}-{}.redb",
            std::process::id(),
            current_timestamp()
        ));
        let document = Path::new("/tmp/agent-edits.pdf");
        let mut store = LibraryStore::open_for_test(database.clone()).unwrap();

        let (highlight, edits) = store.record_edits(|store| {
            store
                .add_highlight(
                    document,
                    HighlightColor::Yellow,
                    vec![rect(2)],
                    "a quote",
                    "",
                )
                .unwrap()
        });
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].describe(), "Highlighted “a quote”");
        assert_eq!(edits[0].page(), Some(2));
        assert_eq!(edits[0].path(), Some(document));
        // Edits outside a recording are not collected.
        store
            .add_bookmark(
                document,
                None,
                DocumentPosition {
                    scroll_x_doc: 0.0,
                    scroll_y_doc: 0.0,
                    zoom: 1.0,
                    page: 1,
                    semantic: None,
                },
            )
            .unwrap();

        store.revert_edit(&edits[0]).unwrap();
        assert!(store.highlights_for(document).is_empty());
        store.reapply_edit(&edits[0]).unwrap();
        assert_eq!(store.highlights_for(document)[0].id, highlight.id);

        // The user recolors it: the agent's edit no longer matches.
        store
            .set_highlight_color(highlight.id, HighlightColor::Blue)
            .unwrap();
        let error = store.revert_edit(&edits[0]).unwrap_err();
        assert!(is_edit_conflict(&error), "{error:#}");
        assert_eq!(store.highlights_for(document).len(), 1);

        let serialized = serde_json::to_string(&edits[0]).unwrap();
        assert_eq!(
            serde_json::from_str::<LibraryEdit>(&serialized).unwrap(),
            edits[0]
        );
        let _ = std::fs::remove_file(database);
    }
}
