use super::*;

pub(super) fn bookmark_pull_offset(delta_y: f32) -> f32 {
    if delta_y <= 0.0 {
        return 0.0;
    }
    if delta_y <= BOOKMARK_DRAG_COMMIT_PX {
        return delta_y;
    }
    (BOOKMARK_DRAG_COMMIT_PX + (delta_y - BOOKMARK_DRAG_COMMIT_PX) * BOOKMARK_DRAG_DAMPING)
        .min(BOOKMARK_DRAG_MAX_PX)
}

pub(super) fn should_activate_bookmark_drag(delta_x: f32, delta_y: f32) -> bool {
    delta_y >= BOOKMARK_DRAG_ACTIVATION_PX && delta_y > delta_x.abs()
}

pub(super) fn bookmark_ribbon_notch_y(height: f32) -> f32 {
    (height - BOOKMARK_RIBBON_NOTCH_PX).max(0.0)
}

impl DocumentView {
    pub(super) fn refresh_bookmarks(&mut self, cx: &mut Context<Self>) {
        self.bookmarks = cx.global::<LibraryStore>().bookmarks_for(&self.path);
        cx.notify();
    }

    pub(super) fn resolved_bookmark_page_index(&self, bookmark: &Bookmark) -> usize {
        let fallback = bookmark
            .position
            .page
            .saturating_sub(1)
            .min(self.last_page_index());
        let Some(reflow) = bookmark.position.semantic.as_ref() else {
            return fallback;
        };
        if reflow.anchor.is_empty() {
            return fallback;
        }
        let Some(glyph_ix) = Self::resolve_semantic_anchor(&self.glyphs, &reflow.anchor) else {
            return fallback;
        };

        self.pages
            .iter()
            .position(|page| page.glyph_range.contains(&glyph_ix))
            .unwrap_or(fallback)
    }

    pub(super) fn resolved_document_bookmarks(&self) -> Vec<(Bookmark, usize)> {
        let mut bookmarks = self
            .bookmarks
            .iter()
            .cloned()
            .map(|bookmark| {
                let page_idx = self.resolved_bookmark_page_index(&bookmark);
                (bookmark, page_idx)
            })
            .collect::<Vec<_>>();
        bookmarks.sort_by(|(left, left_page), (right, right_page)| {
            left_page
                .cmp(right_page)
                .then_with(|| left.created.cmp(&right.created))
                .then_with(|| left.id.cmp(&right.id))
        });
        bookmarks
    }

    pub(super) fn bookmark_for_page(&self, page_idx: usize) -> Option<&Bookmark> {
        self.bookmarks
            .iter()
            .find(|bookmark| self.resolved_bookmark_page_index(bookmark) == page_idx)
    }

    pub(super) fn is_page_bookmarked(&self, page_idx: usize) -> bool {
        self.bookmark_for_page(page_idx).is_some()
    }

    pub(super) fn current_bookmark(&self) -> Option<&Bookmark> {
        self.bookmark_for_page(self.current_page_index())
    }

    pub(super) fn automatic_bookmark_title(page_idx: usize) -> String {
        crate::fl!("bookmark-page-label", page = (page_idx + 1) as i64)
    }

    pub(super) fn effective_bookmark_title(&self, bookmark: &Bookmark, page_idx: usize) -> String {
        bookmark
            .title
            .clone()
            .unwrap_or_else(|| Self::automatic_bookmark_title(page_idx))
    }

    fn bookmark_position_for_current_page(&self) -> DocumentPosition {
        let page_idx = self.current_page_index();
        if !self.capabilities.reflow {
            return DocumentPosition::fixed(
                self.scroll_x_doc,
                self.page_scroll_y_doc(page_idx),
                self.zoom,
                page_idx + 1,
            );
        }

        let Some(page) = self.pages.get(page_idx) else {
            return self.current_document_position();
        };
        let Some(glyph_ix) = page
            .glyph_range
            .clone()
            .next()
            .filter(|glyph_ix| *glyph_ix < self.glyphs.len())
        else {
            let mut position = self.current_document_position();
            position.page = page_idx + 1;
            return position;
        };
        let progress = if self.max_scroll_y_doc() > 0.0 {
            page.offset_y / self.max_scroll_y_doc()
        } else {
            0.0
        };
        DocumentPosition::reflow(
            Self::semantic_anchor_for_glyph(&self.glyphs, glyph_ix),
            0.0,
            progress.clamp(0.0, 1.0),
            self.zoom,
            page_idx + 1,
        )
    }

    pub(super) fn toggle_current_bookmark(&mut self, cx: &mut Context<Self>) {
        let result = if let Some(id) = self.current_bookmark().map(|bookmark| bookmark.id) {
            cx.update_global::<LibraryStore, _>(|store, _| store.remove_bookmark(id))
                .map(|_| ())
        } else {
            let position = self.bookmark_position_for_current_page();
            cx.update_global::<LibraryStore, _>(|store, _| {
                store.add_bookmark(&self.path, None, position)
            })
            .map(|_| ())
        };
        if let Err(error) = result {
            eprintln!("failed to toggle bookmark: {error:#}");
            return;
        }
        self.refresh_bookmarks(cx);
        self.show_document_toolbar(cx);
        self.schedule_document_toolbar_hide(cx);
    }

    pub(super) fn remove_bookmark(&mut self, id: u64, cx: &mut Context<Self>) {
        match cx.update_global::<LibraryStore, _>(|store, _| store.remove_bookmark(id)) {
            Ok(_) => self.refresh_bookmarks(cx),
            Err(error) => eprintln!("failed to remove bookmark: {error:#}"),
        }
    }

    pub(super) fn begin_bookmark_drag(
        &mut self,
        position: GpuiPoint<Pixels>,
        cx: &mut Context<Self>,
    ) {
        self.bookmark_ribbon_retract_generation =
            self.bookmark_ribbon_retract_generation.wrapping_add(1);
        self.bookmark_ribbon_retract_task = None;
        self.bookmark_ribbon_retract = None;
        self.bookmark_drag = Some(BookmarkDragState {
            origin: position,
            current: position,
            active: false,
        });
        self.suppress_bookmark_click = false;
        self.show_document_toolbar(cx);
    }

    pub(super) fn update_bookmark_drag(
        &mut self,
        position: GpuiPoint<Pixels>,
        dragging: bool,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(mut state) = self.bookmark_drag else {
            return false;
        };
        if !dragging {
            let offset = state
                .active
                .then(|| bookmark_pull_offset((state.current.y - state.origin.y).as_f32()))
                .unwrap_or(0.0);
            self.bookmark_drag = None;
            self.start_bookmark_ribbon_retract(offset, cx);
            return false;
        }
        state.current = position;
        let delta_y = (state.current.y - state.origin.y).as_f32();
        let delta_x = (state.current.x - state.origin.x).as_f32().abs();
        if should_activate_bookmark_drag(delta_x, delta_y) {
            state.active = true;
        }
        self.bookmark_drag = Some(state);
        if state.active {
            cx.notify();
        }
        state.active
    }

    pub(super) fn finish_bookmark_drag(
        &mut self,
        position: GpuiPoint<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(mut state) = self.bookmark_drag else {
            return false;
        };
        state.current = position;
        if !state.active {
            self.bookmark_drag = None;
            self.suppress_bookmark_click = true;
            self.toggle_current_bookmark(cx);
            return true;
        }

        let raw_delta_y = (state.current.y - state.origin.y).as_f32();
        let offset = bookmark_pull_offset(raw_delta_y);
        self.bookmark_drag = None;
        self.suppress_bookmark_click = true;
        self.start_bookmark_ribbon_retract(offset, cx);
        if raw_delta_y >= BOOKMARK_DRAG_COMMIT_PX {
            self.open_current_bookmark_title_editor(window, cx);
        } else {
            self.schedule_document_toolbar_hide(cx);
        }
        true
    }

    pub(super) fn activate_bookmark_button(&mut self, cx: &mut Context<Self>) -> bool {
        if self.bookmark_drag.is_some_and(|state| state.active) {
            return false;
        }
        self.bookmark_drag = None;
        if std::mem::take(&mut self.suppress_bookmark_click) {
            return true;
        }
        self.toggle_current_bookmark(cx);
        true
    }

    pub(super) fn bookmark_drag_offset(&self, now: Instant) -> f32 {
        if let Some(offset) = self
            .bookmark_drag
            .filter(|state| state.active)
            .map(|state| bookmark_pull_offset((state.current.y - state.origin.y).as_f32()))
        {
            return offset;
        }
        self.bookmark_ribbon_retract
            .map(|retract| retract.value(now))
            .unwrap_or(0.0)
    }

    pub(super) fn bookmark_drag_armed(&self) -> bool {
        self.bookmark_drag.is_some_and(|state| {
            state.active && (state.current.y - state.origin.y).as_f32() >= BOOKMARK_DRAG_COMMIT_PX
        })
    }

    pub(super) fn bookmark_ribbon_animating(&self, now: Instant) -> bool {
        self.bookmark_ribbon_retract
            .is_some_and(|retract| retract.is_animating(now))
    }

    fn start_bookmark_ribbon_retract(&mut self, offset: f32, cx: &mut Context<Self>) {
        self.bookmark_ribbon_retract_generation =
            self.bookmark_ribbon_retract_generation.wrapping_add(1);
        self.bookmark_ribbon_retract_task = None;
        if offset <= 0.0 || cx.reduce_motion() {
            self.bookmark_ribbon_retract = None;
            cx.notify();
            return;
        }

        let mut retract = ScalarTween::new(offset, BOOKMARK_RIBBON_RETRACT);
        retract.retarget(0.0, Instant::now(), false);
        self.bookmark_ribbon_retract = Some(retract);
        let generation = self.bookmark_ribbon_retract_generation;
        self.bookmark_ribbon_retract_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(BOOKMARK_RIBBON_RETRACT.duration)
                .await;
            _ = this.update(cx, |this, cx| {
                if this.bookmark_ribbon_retract_generation != generation {
                    return;
                }
                this.bookmark_ribbon_retract = None;
                this.bookmark_ribbon_retract_task = None;
                this.schedule_document_toolbar_hide(cx);
                cx.notify();
            });
        }));
        cx.notify();
    }

    pub(super) fn open_current_bookmark_title_editor(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let target = self
            .current_bookmark()
            .map(|bookmark| BookmarkTitleTarget::Existing(bookmark.id))
            .unwrap_or_else(|| BookmarkTitleTarget::New(self.bookmark_position_for_current_page()));
        self.open_bookmark_title_editor(target, window, cx);
    }

    pub(super) fn open_existing_bookmark_title_editor(
        &mut self,
        id: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.bookmarks.iter().any(|bookmark| bookmark.id == id) {
            self.open_bookmark_title_editor(BookmarkTitleTarget::Existing(id), window, cx);
        }
    }

    fn open_bookmark_title_editor(
        &mut self,
        target: BookmarkTitleTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (automatic_title, initial, initially_automatic) = match &target {
            BookmarkTitleTarget::New(position) => {
                let automatic_title = Self::automatic_bookmark_title(
                    position.page.saturating_sub(1).min(self.last_page_index()),
                );
                (automatic_title.clone(), automatic_title, true)
            }
            BookmarkTitleTarget::Existing(id) => {
                let Some(bookmark) = self.bookmarks.iter().find(|bookmark| bookmark.id == *id)
                else {
                    return;
                };
                let automatic_title =
                    Self::automatic_bookmark_title(self.resolved_bookmark_page_index(bookmark));
                (
                    automatic_title.clone(),
                    bookmark
                        .title
                        .clone()
                        .unwrap_or_else(|| automatic_title.clone()),
                    bookmark.title.is_none(),
                )
            }
        };
        let editor = cx.new(|cx| BookmarkTitleEditor::new(initial.into(), window, cx));
        cx.subscribe_in(
            &editor,
            window,
            |this, _, event: &BookmarkTitleEditorEvent, window, cx| match event {
                BookmarkTitleEditorEvent::Save(title) => {
                    this.save_bookmark_title(title.clone(), window, cx)
                }
                BookmarkTitleEditorEvent::Cancel => this.close_bookmark_title_editor(window, cx),
            },
        )
        .detach();
        self.bookmark_title_edit = Some(BookmarkTitleEdit {
            target,
            automatic_title,
            initially_automatic,
        });
        self.bookmark_title_editor = Some(editor);
        self.show_document_toolbar(cx);
        cx.notify();
    }

    fn save_bookmark_title(
        &mut self,
        title: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(edit) = self.bookmark_title_edit.take() else {
            self.close_bookmark_title_editor(window, cx);
            return;
        };
        let retry_edit = edit.clone();
        let title = title.trim().to_string();
        let title =
            if title.is_empty() || (edit.initially_automatic && title == edit.automatic_title) {
                None
            } else {
                Some(title)
            };
        let result = match edit.target {
            BookmarkTitleTarget::Existing(id) => cx
                .update_global::<LibraryStore, _>(|store, _| store.update_bookmark_title(id, title))
                .map(|_| ()),
            BookmarkTitleTarget::New(position) => {
                let page_idx = position.page.saturating_sub(1);
                if let Some(id) = self
                    .bookmarks
                    .iter()
                    .find(|bookmark| self.resolved_bookmark_page_index(bookmark) == page_idx)
                    .map(|bookmark| bookmark.id)
                {
                    cx.update_global::<LibraryStore, _>(|store, _| {
                        store.update_bookmark_title(id, title)
                    })
                    .map(|_| ())
                } else {
                    cx.update_global::<LibraryStore, _>(|store, _| {
                        store.add_bookmark(&self.path, title, position)
                    })
                    .map(|_| ())
                }
            }
        };
        if let Err(error) = result {
            eprintln!("failed to save bookmark title: {error:#}");
            self.bookmark_title_edit = Some(retry_edit);
            return;
        }
        self.bookmark_title_editor = None;
        self.refresh_bookmarks(cx);
        self.focus_handle.focus(window, cx);
        self.schedule_document_toolbar_hide(cx);
    }

    pub(super) fn close_bookmark_title_editor(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.bookmark_title_editor.take().is_some() {
            self.bookmark_title_edit = None;
            self.focus_handle.focus(window, cx);
            self.schedule_document_toolbar_hide(cx);
            cx.notify();
        }
    }

    pub(super) fn show_document_toolbar(&mut self, cx: &mut Context<Self>) {
        self.document_toolbar_hide_generation =
            self.document_toolbar_hide_generation.wrapping_add(1);
        self.document_toolbar_hide_task = None;
        if !self.document_toolbar_visible {
            self.document_toolbar_visible = true;
            cx.notify();
        }
    }

    pub(super) fn set_document_toolbar_hovered(&mut self, hovered: bool, cx: &mut Context<Self>) {
        self.document_toolbar_hovered = hovered;
        if hovered {
            self.show_document_toolbar(cx);
        } else {
            self.schedule_document_toolbar_hide(cx);
        }
    }

    pub(super) fn schedule_document_toolbar_hide(&mut self, cx: &mut Context<Self>) {
        self.document_toolbar_hide_generation =
            self.document_toolbar_hide_generation.wrapping_add(1);
        if !self.document_toolbar_visible
            || self.document_toolbar_hovered
            || self.bookmark_drag.is_some()
            || self.bookmark_ribbon_retract.is_some()
            || self.bookmark_title_editor.is_some()
            || self.epub_style_panel_position.is_some()
        {
            return;
        }
        let generation = self.document_toolbar_hide_generation;
        self.document_toolbar_hide_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(220))
                .await;
            _ = this.update(cx, |this, cx| {
                if this.document_toolbar_hide_generation != generation
                    || this.document_toolbar_hovered
                    || this.bookmark_drag.is_some()
                    || this.bookmark_ribbon_retract.is_some()
                    || this.bookmark_title_editor.is_some()
                    || this.epub_style_panel_position.is_some()
                {
                    return;
                }
                this.document_toolbar_visible = false;
                this.document_toolbar_hide_task = None;
                cx.notify();
            });
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bookmark_drag_activates_only_for_a_deliberate_downward_pull() {
        assert!(!should_activate_bookmark_drag(
            0.0,
            BOOKMARK_DRAG_ACTIVATION_PX - 0.1
        ));
        assert!(should_activate_bookmark_drag(
            0.0,
            BOOKMARK_DRAG_ACTIVATION_PX
        ));
        assert!(!should_activate_bookmark_drag(12.0, 12.0));
        assert!(!should_activate_bookmark_drag(-18.0, 12.0));
        assert!(!should_activate_bookmark_drag(0.0, -20.0));
    }

    #[test]
    fn bookmark_pull_tracks_directly_then_rubber_bands_and_caps() {
        assert_eq!(bookmark_pull_offset(-20.0), 0.0);
        assert_eq!(bookmark_pull_offset(24.0), 24.0);
        assert_eq!(
            bookmark_pull_offset(BOOKMARK_DRAG_COMMIT_PX),
            BOOKMARK_DRAG_COMMIT_PX
        );
        assert_eq!(bookmark_pull_offset(76.0), 63.0);
        assert_eq!(bookmark_pull_offset(10_000.0), BOOKMARK_DRAG_MAX_PX);
    }

    #[test]
    fn bookmark_ribbon_keeps_a_fixed_notch_depth() {
        assert_eq!(bookmark_ribbon_notch_y(36.0), 30.0);
        assert_eq!(bookmark_ribbon_notch_y(96.0), 90.0);
        assert_eq!(bookmark_ribbon_notch_y(4.0), 0.0);
    }
}
