use super::*;

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
    }

    pub(super) fn remove_bookmark(&mut self, id: u64, cx: &mut Context<Self>) {
        match cx.update_global::<LibraryStore, _>(|store, _| store.remove_bookmark(id)) {
            Ok(_) => self.refresh_bookmarks(cx),
            Err(error) => eprintln!("failed to remove bookmark: {error:#}"),
        }
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
    }

    pub(super) fn close_bookmark_title_editor(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.bookmark_title_editor.take().is_some() {
            self.bookmark_title_edit = None;
            self.focus_handle.focus(window, cx);
            cx.notify();
        }
    }
}
