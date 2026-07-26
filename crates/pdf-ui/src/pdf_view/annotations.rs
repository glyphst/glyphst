use super::*;

impl PdfView {
    pub(super) fn refresh_semantic_annotations(&mut self) {
        let highlight_ranges = self
            .highlights
            .iter()
            .enumerate()
            .filter_map(|(ix, highlight)| {
                let range = highlight.semantic_range.as_ref()?;
                let start = Self::resolve_semantic_anchor(&self.glyphs, &range.start)?;
                let end = Self::resolve_semantic_anchor(&self.glyphs, &range.end)?;
                Some((ix, start.min(end)..start.max(end).saturating_add(1)))
            })
            .collect::<Vec<_>>();
        for (ix, range) in highlight_ranges {
            self.highlights[ix].rects = self.highlight_rects_for_range(range);
        }

        let note_ranges = self
            .notes
            .iter()
            .enumerate()
            .filter_map(|(ix, note)| {
                let range = note.semantic_range.as_ref()?;
                let start = Self::resolve_semantic_anchor(&self.glyphs, &range.start)?;
                let end = Self::resolve_semantic_anchor(&self.glyphs, &range.end)?;
                Some((ix, start.min(end)..start.max(end).saturating_add(1)))
            })
            .collect::<Vec<_>>();
        for (ix, range) in note_ranges {
            self.notes[ix].rects = self.highlight_rects_for_range(range);
        }
    }

    pub(super) fn sync_document_edits_from_store(&mut self, cx: &mut Context<Self>) {
        let (document_edit_revision, bookmarks_revision) = {
            let store = cx.global::<LibraryStore>();
            (
                store.document_edit_revision(&self.path),
                store.bookmarks_revision(),
            )
        };
        let annotations_changed =
            document_edit_revision.annotations != self.document_edit_revision.annotations;
        let rotation_changed =
            document_edit_revision.rotation != self.document_edit_revision.rotation;
        let bookmarks_changed = bookmarks_revision != self.bookmarks_revision;
        if !annotations_changed && !rotation_changed && !bookmarks_changed {
            return;
        }

        self.document_edit_revision = document_edit_revision;
        self.bookmarks_revision = bookmarks_revision;
        let mut visible_state_changed = false;

        if annotations_changed {
            visible_state_changed = true;
            let (highlights, notes) = {
                let store = cx.global::<LibraryStore>();
                (
                    store.highlights_for(&self.path),
                    store.notes_for(&self.path),
                )
            };
            self.highlights = highlights;
            self.notes = notes;
            if self.capabilities.reflow && !self.glyphs.is_empty() {
                self.refresh_semantic_annotations();
            }

            let highlight_ids = self
                .highlights
                .iter()
                .map(|highlight| highlight.id)
                .collect::<HashSet<_>>();
            let note_ids = self
                .notes
                .iter()
                .map(|note| note.id)
                .collect::<HashSet<_>>();
            retain_valid_annotation_id(&mut self.selected_highlight, &highlight_ids);
            let stale_context_target = self
                .context_menu_highlight
                .is_some_and(|id| !highlight_ids.contains(&id))
                || self
                    .context_menu_note
                    .is_some_and(|id| !note_ids.contains(&id));
            if stale_context_target {
                self.close_context_menu(cx);
            }
            if retain_valid_annotation_id(&mut self.hovered_note, &note_ids) {
                self.hovered_note_position = None;
            }
            retain_valid_annotation_id(&mut self.notes_sidebar_selected, &note_ids);
            if matches!(
                self.note_target,
                Some(NoteTarget::Edit { id }) if !note_ids.contains(&id)
            ) {
                self.note_target = None;
            }
            self.refresh_notes_sidebar_entries();
            if let Some(palette) = self.notes_palette.clone() {
                let entries = self.note_palette_entries();
                palette.update(cx, |picker, cx| {
                    picker.update_delegate(move |delegate| delegate.replace_entries(entries), cx);
                });
            }
        }

        if rotation_changed {
            visible_state_changed = true;
            let rotation = cx.global::<LibraryStore>().document_rotation(&self.path);
            self.apply_stored_rotation(rotation, cx);
        }

        if bookmarks_changed && let Some(palette) = self.bookmark_palette.clone() {
            visible_state_changed = true;
            let (bookmarks, current_path) = {
                let store = cx.global::<LibraryStore>();
                (store.bookmarks().to_vec(), self.path.clone())
            };
            palette.update(cx, |picker, cx| {
                picker.update_delegate(
                    move |delegate| delegate.replace_bookmarks(bookmarks, current_path),
                    cx,
                );
            });
        }

        if visible_state_changed {
            cx.notify();
        }
    }

    pub(super) fn undo_document_edit(&mut self, cx: &mut Context<Self>) {
        match cx.update_global::<LibraryStore, _>(|store, _| store.undo_document(&self.path)) {
            Ok(_) => {}
            Err(error) => eprintln!("failed to undo document edit: {error:#}"),
        }
    }

    pub(super) fn redo_document_edit(&mut self, cx: &mut Context<Self>) {
        match cx.update_global::<LibraryStore, _>(|store, _| store.redo_document(&self.path)) {
            Ok(_) => {}
            Err(error) => eprintln!("failed to redo document edit: {error:#}"),
        }
    }

    pub(super) fn select_all(&mut self) {
        self.selected_highlight = None;
        if self.glyphs.is_empty() {
            self.selection = None;
            return;
        }

        self.selection = Some(Selection {
            anchor: 0,
            head: self.glyphs.len() - 1,
        });
    }

    pub(super) fn create_highlight_from_selection(
        &mut self,
        color: HighlightColor,
        cx: &mut Context<Self>,
    ) {
        let Some((start, end)) = self.selection_bounds() else {
            return;
        };
        let Some(text) = self.selected_text() else {
            return;
        };
        let rects = self.highlight_rects_for_range(start..end + 1);
        if rects.is_empty() {
            return;
        }

        let path = self.path.clone();
        let result = if self.capabilities.reflow {
            let semantic_range = self.semantic_range_for_selection(start, end);
            cx.update_global::<LibraryStore, _>(move |store, _| {
                store.add_reflow_highlight(&path, color, semantic_range, rects, text, "")
            })
        } else {
            cx.update_global::<LibraryStore, _>(move |store, _| {
                store.add_highlight(&path, color, rects, text, "")
            })
        };
        match result {
            Ok(highlight) => {
                self.selected_highlight = Some(highlight.id);
                self.highlights.push(highlight);
                self.selection = None;
                cx.notify();
            }
            Err(error) => eprintln!("failed to add PDF highlight: {error:#}"),
        }
    }

    pub(super) fn set_context_highlight_color(
        &mut self,
        color: HighlightColor,
        cx: &mut Context<Self>,
    ) {
        let Some(id) = self.context_menu_highlight.or(self.selected_highlight) else {
            return;
        };

        match cx.update_global::<LibraryStore, _>(|store, _| store.set_highlight_color(id, color)) {
            Ok(true) => {
                if let Some(highlight) = self
                    .highlights
                    .iter_mut()
                    .find(|highlight| highlight.id == id)
                {
                    highlight.color = color;
                }
                self.selected_highlight = Some(id);
                cx.notify();
            }
            Ok(false) => {}
            Err(error) => eprintln!("failed to update PDF highlight: {error:#}"),
        }
    }

    pub(super) fn remove_context_highlight(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.context_menu_highlight.or(self.selected_highlight) else {
            return;
        };

        match cx.update_global::<LibraryStore, _>(|store, _| store.remove_highlight(id)) {
            Ok(true) => {
                self.highlights.retain(|highlight| highlight.id != id);
                if self.selected_highlight == Some(id) {
                    self.selected_highlight = None;
                }
                self.context_menu_highlight = None;
                cx.notify();
            }
            Ok(false) => {}
            Err(error) => eprintln!("failed to remove PDF highlight: {error:#}"),
        }
    }

    pub(super) fn begin_add_note(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((start, end)) = self.selection_bounds() else {
            return;
        };
        let Some(text) = self.selected_text() else {
            return;
        };
        let rects = self.highlight_rects_for_range(start..end + 1);
        if rects.is_empty() {
            return;
        }

        self.open_note_editor(
            "".into(),
            HighlightColor::Yellow,
            NoteTarget::New {
                rects,
                semantic_range: self
                    .capabilities
                    .reflow
                    .then(|| self.semantic_range_for_selection(start, end)),
                text,
            },
            window,
            cx,
        );
    }

    pub(super) fn begin_edit_context_note(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.context_menu_note.or(self.hovered_note) else {
            return;
        };
        let Some((body, color)) = self
            .notes
            .iter()
            .find(|note| note.id == id)
            .map(|note| (note.body.clone(), note.color))
        else {
            return;
        };

        self.open_note_editor(body.into(), color, NoteTarget::Edit { id }, window, cx);
    }

    fn open_note_editor(
        &mut self,
        content: SharedString,
        color: HighlightColor,
        target: NoteTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_context_menu(cx);
        self.close_all_overlays(window, cx);

        let previous_focus_handle = self.focus_handle.clone();
        let editor =
            cx.new(|cx| NoteEditor::new(previous_focus_handle.clone(), content, color, window, cx));

        cx.subscribe_in(&editor, window, |this, _, event, window, cx| match event {
            NoteEditorEvent::Save { body, color } => {
                this.save_note_editor(body.clone(), *color, window, cx);
            }
            NoteEditorEvent::Cancel => {
                this.close_note_editor(window, cx);
            }
        })
        .detach();

        editor.focus_handle(cx).focus(window, cx);
        self.note_editor = Some(editor);
        self.note_target = Some(target);
        cx.notify();
    }

    fn save_note_editor(
        &mut self,
        body: SharedString,
        color: HighlightColor,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let body = body.as_ref().trim().to_string();
        if body.is_empty() {
            self.close_note_editor(window, cx);
            return;
        }

        let Some(target) = self.note_target.take() else {
            self.close_note_editor(window, cx);
            return;
        };

        match target {
            NoteTarget::New {
                rects,
                semantic_range,
                text,
            } => {
                let path = self.path.clone();
                let result = if let Some(semantic_range) = semantic_range {
                    cx.update_global::<LibraryStore, _>(move |store, _| {
                        store.add_reflow_note(&path, color, semantic_range, rects, text, body)
                    })
                } else {
                    cx.update_global::<LibraryStore, _>(move |store, _| {
                        store.add_note(&path, color, rects, text, body)
                    })
                };
                match result {
                    Ok(note) => {
                        let note_id = note.id;
                        self.notes.push(note);
                        self.notes_sidebar_selected = Some(note_id);
                        self.refresh_notes_sidebar_entries();
                        self.selection = None;
                    }
                    Err(error) => eprintln!("failed to add PDF note: {error:#}"),
                }
            }
            NoteTarget::Edit { id } => {
                match cx.update_global::<LibraryStore, _>(|store, _| {
                    store.update_note(id, body.clone(), color)
                }) {
                    Ok(true) => {
                        if let Some(note) = self.notes.iter_mut().find(|note| note.id == id) {
                            note.body = body;
                            note.color = color;
                            self.notes_sidebar_selected = Some(id);
                            self.refresh_notes_sidebar_entries();
                        }
                    }
                    Ok(false) => {}
                    Err(error) => eprintln!("failed to update PDF note: {error:#}"),
                }
            }
        }

        self.note_editor = None;
        self.focus_handle.focus(window, cx);
        cx.notify();
    }

    pub(super) fn close_note_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.note_editor.take().is_some() {
            self.note_target = None;
            self.focus_handle.focus(window, cx);
            cx.notify();
        }
    }

    pub(super) fn request_close_note_editor(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(editor) = self.note_editor.clone() else {
            return;
        };
        editor.update(cx, |editor, cx| editor.request_close(window, cx));
    }

    pub(super) fn remove_context_note(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.context_menu_note.or(self.hovered_note) else {
            return;
        };

        match cx.update_global::<LibraryStore, _>(|store, _| store.remove_note(id)) {
            Ok(true) => {
                self.notes.retain(|note| note.id != id);
                if self.notes_sidebar_selected == Some(id) {
                    self.notes_sidebar_selected = None;
                }
                self.refresh_notes_sidebar_entries();
                if self.hovered_note == Some(id) {
                    self.hovered_note = None;
                    self.hovered_note_position = None;
                }
                self.context_menu_note = None;
                cx.notify();
            }
            Ok(false) => {}
            Err(error) => eprintln!("failed to remove PDF note: {error:#}"),
        }
    }
}
