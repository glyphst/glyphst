use super::edits::*;
use super::*;

impl LibraryStore {
    pub fn highlights_for(&self, path: &Path) -> Vec<Highlight> {
        let path = normalize_path(path);
        self.library
            .highlights
            .iter()
            .filter(|highlight| highlight.path == path)
            .cloned()
            .collect()
    }

    pub fn notes_for(&self, path: &Path) -> Vec<Note> {
        let path = normalize_path(path);
        self.library
            .notes
            .iter()
            .filter(|note| note.path == path)
            .cloned()
            .collect()
    }
    pub fn add_highlight(
        &mut self,
        path: &Path,
        color: HighlightColor,
        rects: Vec<HighlightRect>,
        text: impl Into<String>,
        note: impl Into<String>,
    ) -> Result<Highlight> {
        self.add_highlight_with_geometry(
            path,
            color,
            AnnotationGeometry::fixed(rects),
            text.into(),
            note.into(),
        )
    }

    pub fn add_reflow_highlight(
        &mut self,
        path: &Path,
        color: HighlightColor,
        semantic_range: SemanticTextRange,
        rects: Vec<HighlightRect>,
        text: impl Into<String>,
        note: impl Into<String>,
    ) -> Result<Highlight> {
        self.add_highlight_with_geometry(
            path,
            color,
            AnnotationGeometry::reflow(rects, semantic_range),
            text.into(),
            note.into(),
        )
    }

    fn add_highlight_with_geometry(
        &mut self,
        path: &Path,
        color: HighlightColor,
        geometry: AnnotationGeometry,
        text: String,
        note: String,
    ) -> Result<Highlight> {
        let mut highlight = Highlight {
            id: self.library.next_highlight_id.max(1),
            path: normalize_path(path),
            color,
            rects: geometry.rects,
            semantic_range: geometry.semantic_range,
            text,
            note,
            created: current_timestamp(),
        };
        highlight.normalize();
        self.replace_highlight_snapshot(None, Some(&highlight))?;
        self.record_document_edit(DocumentEdit::Highlight {
            before: None,
            after: Some(highlight.clone()),
        });
        Ok(highlight)
    }

    pub fn add_note(
        &mut self,
        path: &Path,
        color: HighlightColor,
        rects: Vec<HighlightRect>,
        text: impl Into<String>,
        body: impl Into<String>,
    ) -> Result<Note> {
        self.add_note_with_geometry(
            path,
            color,
            AnnotationGeometry::fixed(rects),
            text.into(),
            body.into(),
        )
    }

    pub fn add_reflow_note(
        &mut self,
        path: &Path,
        color: HighlightColor,
        semantic_range: SemanticTextRange,
        rects: Vec<HighlightRect>,
        text: impl Into<String>,
        body: impl Into<String>,
    ) -> Result<Note> {
        self.add_note_with_geometry(
            path,
            color,
            AnnotationGeometry::reflow(rects, semantic_range),
            text.into(),
            body.into(),
        )
    }

    fn add_note_with_geometry(
        &mut self,
        path: &Path,
        color: HighlightColor,
        geometry: AnnotationGeometry,
        text: String,
        body: String,
    ) -> Result<Note> {
        let mut note = Note {
            id: self.library.next_note_id.max(1),
            path: normalize_path(path),
            color,
            rects: geometry.rects,
            semantic_range: geometry.semantic_range,
            text,
            body,
            created: current_timestamp(),
        };
        note.normalize();
        self.replace_note_snapshot(None, Some(&note))?;
        self.record_document_edit(DocumentEdit::Note {
            before: None,
            after: Some(note.clone()),
        });
        Ok(note)
    }

    pub fn remove_highlight(&mut self, id: u64) -> Result<bool> {
        let Some(highlight) = self
            .library
            .highlights
            .iter()
            .find(|highlight| highlight.id == id)
            .cloned()
        else {
            return Ok(false);
        };

        self.replace_highlight_snapshot(Some(&highlight), None)?;
        self.record_document_edit(DocumentEdit::Highlight {
            before: Some(highlight),
            after: None,
        });
        Ok(true)
    }

    pub fn remove_note(&mut self, id: u64) -> Result<bool> {
        let Some(note) = self
            .library
            .notes
            .iter()
            .find(|note| note.id == id)
            .cloned()
        else {
            return Ok(false);
        };

        self.replace_note_snapshot(Some(&note), None)?;
        self.record_document_edit(DocumentEdit::Note {
            before: Some(note),
            after: None,
        });
        Ok(true)
    }

    pub fn set_highlight_color(&mut self, id: u64, color: HighlightColor) -> Result<bool> {
        let Some(before) = self
            .library
            .highlights
            .iter()
            .find(|highlight| highlight.id == id)
            .cloned()
        else {
            return Ok(false);
        };

        let mut after = before.clone();
        after.color = color;
        if after == before {
            return Ok(false);
        }
        self.replace_highlight_snapshot(Some(&before), Some(&after))?;
        self.record_document_edit(DocumentEdit::HighlightColor {
            before,
            after: color,
        });
        Ok(true)
    }

    pub fn update_note(
        &mut self,
        id: u64,
        body: impl Into<String>,
        color: HighlightColor,
    ) -> Result<bool> {
        let Some(before) = self
            .library
            .notes
            .iter()
            .find(|note| note.id == id)
            .cloned()
        else {
            return Ok(false);
        };

        let mut after = before.clone();
        after.body = body.into();
        after.color = color;
        after.normalize();
        if after == before {
            return Ok(false);
        }
        self.replace_note_snapshot(Some(&before), Some(&after))?;
        self.record_document_edit(DocumentEdit::NoteUpdate {
            before,
            after_body: after.body,
            after_color: after.color,
        });
        Ok(true)
    }
    pub(super) fn replace_highlight_snapshot(
        &mut self,
        expected: Option<&Highlight>,
        target: Option<&Highlight>,
    ) -> Result<()> {
        let id = validate_snapshot_replacement(
            &self.library.highlights,
            expected,
            target,
            |highlight| highlight.id,
            |highlight| highlight.path.as_path(),
            validate_highlight_snapshot,
            "highlight",
        )?;

        if let Some(highlight) = target {
            self.write_highlight(highlight)?;
        } else {
            self.remove_highlight_from_database(id)?;
        }
        self.library.apply_highlight_snapshot(id, target.cloned());
        Ok(())
    }

    pub(super) fn replace_note_snapshot(
        &mut self,
        expected: Option<&Note>,
        target: Option<&Note>,
    ) -> Result<()> {
        let id = validate_snapshot_replacement(
            &self.library.notes,
            expected,
            target,
            |note| note.id,
            |note| note.path.as_path(),
            validate_note_snapshot,
            "note",
        )?;

        if let Some(note) = target {
            self.write_note(note)?;
        } else {
            self.remove_note_from_database(id)?;
        }
        self.library.apply_note_snapshot(id, target.cloned());
        Ok(())
    }
}
