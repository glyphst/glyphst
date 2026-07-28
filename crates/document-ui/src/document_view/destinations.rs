use super::*;

impl DocumentView {
    pub(super) fn navigate_to_pdf_destination(
        &mut self,
        page_idx: usize,
        target_top: Option<f32>,
        target_left: Option<f32>,
        cx: &mut Context<Self>,
    ) {
        let Some(page) = self.pages.get(page_idx) else {
            return;
        };

        if self.is_presentation_mode() {
            let current_position = self.current_jump_position();
            Self::push_jump_position(&mut self.jump_back, current_position);
            self.jump_forward.clear();
            self.mark_manual_navigation();
            self.set_presentation_page(page_idx, cx);
            return;
        }

        let (page_width, _) = self.page_dimensions(page_idx);
        let target_point = Self::pdf_point_to_view(
            page.bounds,
            self.rotation,
            mupdf::Point {
                x: target_left.unwrap_or(page.bounds.x0 + page.width / 2.0),
                y: target_top.unwrap_or(page.bounds.y0),
            },
        );
        let swapped = self.rotation.swaps_width_and_height();
        let scroll_y_doc =
            if (swapped && target_left.is_some()) || (!swapped && target_top.is_some()) {
                page.offset_y + target_point.y
            } else {
                page.offset_y
            };
        let scroll_x_doc =
            if (swapped && target_top.is_some()) || (!swapped && target_left.is_some()) {
                target_point.x - page_width / 2.0
            } else {
                self.scroll_x_doc
            };

        let current_position = self.current_jump_position();
        Self::push_jump_position(&mut self.jump_back, current_position);
        self.jump_forward.clear();
        self.mark_manual_navigation();
        self.set_scroll_doc(scroll_x_doc, scroll_y_doc, cx);
    }

    pub(crate) fn navigate_to_page_number(&mut self, page_number: usize, cx: &mut Context<Self>) {
        if !(1..=self.page_count()).contains(&page_number) {
            return;
        }

        self.navigate_to_pdf_destination(page_number - 1, None, None, cx);
    }

    pub fn navigate_to_document_position(
        &mut self,
        position: DocumentPosition,
        cx: &mut Context<Self>,
    ) {
        let current_position = self.current_jump_position();
        Self::push_jump_position(&mut self.jump_back, current_position);
        self.jump_forward.clear();
        self.mark_manual_navigation();
        if self.capabilities.reflow && position.is_reflow() {
            self.restore_position(position);
            self.schedule_position_save(cx);
            cx.notify();
            return;
        }
        self.set_zoom(position.zoom, cx);
        self.set_scroll_doc(position.scroll_x_doc, position.scroll_y_doc, cx);
    }

    pub(crate) fn open_library_document(
        &mut self,
        path: PathBuf,
        position: Option<DocumentPosition>,
        cx: &mut Context<Self>,
    ) {
        cx.emit(DocumentViewEvent::OpenDocument { path, position });
    }

    pub(crate) fn navigate_to_toc_entry(&mut self, entry_ix: usize, cx: &mut Context<Self>) {
        let Some(entry) = self.toc.get(entry_ix) else {
            return;
        };
        if !entry.navigable {
            return;
        }

        let page_idx = entry.page_idx;
        let target_top = entry.target_top;
        let target_left = entry.target_left;
        self.toc_sidebar_selected = Some(entry_ix);
        cx.notify();
        self.navigate_to_pdf_destination(page_idx, target_top, target_left, cx);
    }

    pub(crate) fn navigate_to_note(&mut self, note_ix: usize, cx: &mut Context<Self>) {
        let Some((note_id, page_idx, target_top)) = self.notes.get(note_ix).and_then(|note| {
            note.rects
                .iter()
                .min_by(|left, right| {
                    left.page_idx
                        .cmp(&right.page_idx)
                        .then_with(|| left.y0.total_cmp(&right.y0))
                        .then_with(|| left.x0.total_cmp(&right.x0))
                })
                .map(|rect| (note.id, rect.page_idx, rect.y0))
        }) else {
            return;
        };

        self.notes_sidebar_selected = Some(note_id);
        cx.notify();
        self.navigate_to_pdf_destination(page_idx, Some(target_top), None, cx);
    }

    pub(super) fn activate_link(&mut self, link_ix: usize, cx: &mut Context<Self>) {
        let Some(target) = self.links.get(link_ix).map(|link| link.target.clone()) else {
            return;
        };

        match target {
            LinkTarget::Internal {
                page_idx,
                target_top,
                target_left,
            } => self.navigate_to_pdf_destination(page_idx, target_top, target_left, cx),
            LinkTarget::External(url) => cx.open_url(&url),
        }
    }
}
