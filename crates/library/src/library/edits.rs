use super::*;

impl LibraryStore {
    pub fn document_edit_revision(&self, path: &Path) -> DocumentEditRevision {
        let path = normalize_path(path);
        self.document_edit_histories
            .get(&path)
            .map(|history| history.revision)
            .unwrap_or_default()
    }

    pub fn bookmarks_revision(&self) -> u64 {
        self.bookmarks_revision
    }

    pub fn can_undo_document(&self, path: &Path) -> bool {
        let path = normalize_path(path);
        self.document_edit_histories
            .get(&path)
            .is_some_and(|history| !history.undo.is_empty())
    }

    pub fn can_redo_document(&self, path: &Path) -> bool {
        let path = normalize_path(path);
        self.document_edit_histories
            .get(&path)
            .is_some_and(|history| !history.redo.is_empty())
    }

    pub fn undo_document(&mut self, path: &Path) -> Result<bool> {
        self.apply_history_edit(path, DocumentEditDirection::Undo)
    }

    pub fn redo_document(&mut self, path: &Path) -> Result<bool> {
        self.apply_history_edit(path, DocumentEditDirection::Redo)
    }
    pub(super) fn record_document_edit(&mut self, edit: DocumentEdit) {
        if let Some(recorder) = &mut self.edit_recorder {
            recorder.push(LibraryEdit::document(edit.clone()));
        }
        let path = normalize_path(edit.path());
        let is_bookmark = edit.is_bookmark();
        let sequence = self.next_document_edit_sequence();
        let history = self.document_edit_histories.entry(path).or_default();
        if history.undo.len() == MAX_DOCUMENT_EDIT_HISTORY_ENTRIES {
            history.undo.pop_front();
        }
        edit.bump_revision(&mut history.revision);
        history.undo.push_back(edit);
        history.redo.clear();
        history.last_used = sequence;
        if is_bookmark {
            self.bookmarks_revision = self.bookmarks_revision.wrapping_add(1);
        }
        self.trim_document_edit_histories();
    }

    fn apply_history_edit(
        &mut self,
        path: &Path,
        direction: DocumentEditDirection,
    ) -> Result<bool> {
        let path = normalize_path(path);
        let Some(edit) = self
            .document_edit_histories
            .get(&path)
            .and_then(|history| match direction {
                DocumentEditDirection::Undo => history.undo.back(),
                DocumentEditDirection::Redo => history.redo.last(),
            })
            .cloned()
        else {
            return Ok(false);
        };
        if normalize_path(edit.path()) != path {
            return Err(anyhow!("document edit history path mismatch"));
        }

        if let Err(error) = self.apply_document_edit(&edit, direction) {
            if error.downcast_ref::<DocumentEditConflict>().is_some() {
                self.document_edit_histories.remove(&path);
            }
            return Err(error);
        }

        let is_bookmark = edit.is_bookmark();
        let sequence = self.next_document_edit_sequence();
        let history = self
            .document_edit_histories
            .get_mut(&path)
            .expect("document edit history must still exist");
        match direction {
            DocumentEditDirection::Undo => {
                let moved = history
                    .undo
                    .pop_back()
                    .expect("undo history must contain the applied edit");
                debug_assert_eq!(moved, edit);
                history.redo.push(moved);
            }
            DocumentEditDirection::Redo => {
                let moved = history
                    .redo
                    .pop()
                    .expect("redo history must contain the applied edit");
                debug_assert_eq!(moved, edit);
                if history.undo.len() == MAX_DOCUMENT_EDIT_HISTORY_ENTRIES {
                    history.undo.pop_front();
                }
                history.undo.push_back(moved);
            }
        }
        edit.bump_revision(&mut history.revision);
        history.last_used = sequence;
        if is_bookmark {
            self.bookmarks_revision = self.bookmarks_revision.wrapping_add(1);
        }
        Ok(true)
    }

    pub(super) fn apply_document_edit(
        &mut self,
        edit: &DocumentEdit,
        direction: DocumentEditDirection,
    ) -> Result<()> {
        match edit {
            DocumentEdit::Bookmark { before, after } => {
                let (expected, target) = match direction {
                    DocumentEditDirection::Undo => (after.as_ref(), before.as_ref()),
                    DocumentEditDirection::Redo => (before.as_ref(), after.as_ref()),
                };
                self.replace_bookmark_snapshot(expected, target)
            }
            DocumentEdit::Highlight { before, after } => {
                let (expected, target) = match direction {
                    DocumentEditDirection::Undo => (after.as_ref(), before.as_ref()),
                    DocumentEditDirection::Redo => (before.as_ref(), after.as_ref()),
                };
                self.replace_highlight_snapshot(expected, target)
            }
            DocumentEdit::HighlightColor { before, after } => {
                let mut after_snapshot = before.clone();
                after_snapshot.color = *after;
                let (expected, target) = match direction {
                    DocumentEditDirection::Undo => (&after_snapshot, before),
                    DocumentEditDirection::Redo => (before, &after_snapshot),
                };
                self.replace_highlight_snapshot(Some(expected), Some(target))
            }
            DocumentEdit::Note { before, after } => {
                let (expected, target) = match direction {
                    DocumentEditDirection::Undo => (after.as_ref(), before.as_ref()),
                    DocumentEditDirection::Redo => (before.as_ref(), after.as_ref()),
                };
                self.replace_note_snapshot(expected, target)
            }
            DocumentEdit::NoteUpdate {
                before,
                after_body,
                after_color,
            } => {
                let mut after_snapshot = before.clone();
                after_snapshot.body.clone_from(after_body);
                after_snapshot.color = *after_color;
                let (expected, target) = match direction {
                    DocumentEditDirection::Undo => (&after_snapshot, before),
                    DocumentEditDirection::Redo => (before, &after_snapshot),
                };
                self.replace_note_snapshot(Some(expected), Some(target))
            }
            DocumentEdit::Rotation {
                path,
                before,
                after,
            } => {
                let (expected, target) = match direction {
                    DocumentEditDirection::Undo => (*after, *before),
                    DocumentEditDirection::Redo => (*before, *after),
                };
                if self.document_rotation(path) != expected {
                    return Err(DocumentEditConflict("document rotation").into());
                }
                self.update_document_preferences(path, |preferences| {
                    preferences.rotation = target;
                })
            }
        }
    }
    pub(super) fn next_document_edit_sequence(&mut self) -> u64 {
        self.next_document_edit_sequence = self.next_document_edit_sequence.wrapping_add(1);
        self.next_document_edit_sequence
    }

    fn trim_document_edit_histories(&mut self) {
        while self.document_edit_histories.len() > MAX_DOCUMENT_EDIT_HISTORIES {
            let Some(path) = self
                .document_edit_histories
                .iter()
                .min_by_key(|(_, history)| history.last_used)
                .map(|(path, _)| path.clone())
            else {
                break;
            };
            self.document_edit_histories.remove(&path);
        }
    }
}

fn snapshot_id(expected: Option<u64>, target: Option<u64>) -> Result<u64> {
    match (expected, target) {
        (Some(expected), Some(target)) if expected != target => {
            Err(anyhow!("document edit snapshot id mismatch"))
        }
        (Some(id), _) | (_, Some(id)) => Ok(id),
        (None, None) => Err(anyhow!("document edit must contain a snapshot")),
    }
}

fn validate_snapshot_path(expected: Option<&Path>, target: Option<&Path>) -> Result<()> {
    if let (Some(expected), Some(target)) = (expected, target)
        && normalize_path(expected) != normalize_path(target)
    {
        return Err(anyhow!("document edit snapshot path mismatch"));
    }
    Ok(())
}

pub(super) fn validate_bookmark_snapshot(bookmark: &Bookmark) -> Result<()> {
    if !bookmark.is_valid() {
        return Err(anyhow!("invalid bookmark snapshot"));
    }
    Ok(())
}

pub(super) fn validate_highlight_snapshot(highlight: &Highlight) -> Result<()> {
    if !highlight.is_valid() {
        return Err(anyhow!("invalid highlight snapshot"));
    }
    Ok(())
}

pub(super) fn validate_note_snapshot(note: &Note) -> Result<()> {
    if !note.is_valid() {
        return Err(anyhow!("invalid note snapshot"));
    }
    Ok(())
}

pub(super) fn validate_snapshot_replacement<T: PartialEq>(
    snapshots: &[T],
    expected: Option<&T>,
    target: Option<&T>,
    id_of: impl Fn(&T) -> u64,
    path_of: impl for<'a> Fn(&'a T) -> &'a Path,
    validate: impl Fn(&T) -> Result<()>,
    kind: &'static str,
) -> Result<u64> {
    let id = snapshot_id(expected.map(&id_of), target.map(&id_of))?;
    validate_snapshot_path(expected.map(&path_of), target.map(&path_of))?;
    if let Some(target) = target {
        validate(target)?;
    }
    let current = snapshots.iter().find(|snapshot| id_of(snapshot) == id);
    if current != expected {
        return Err(DocumentEditConflict(kind).into());
    }
    Ok(id)
}

pub(super) fn replace_snapshot_by_id<T>(
    snapshots: &mut Vec<T>,
    id: u64,
    target: Option<T>,
    snapshot_id: impl Fn(&T) -> u64,
) {
    snapshots.retain(|snapshot| snapshot_id(snapshot) != id);
    if let Some(target) = target {
        snapshots.push(target);
    }
}
