use super::*;

impl DocumentView {
    pub(super) fn epub_layout_for_viewport(
        &self,
        preferences: EpubReaderPreferences,
        viewport_width: f32,
        viewport_height: f32,
    ) -> glyphst_core::EpubLayout {
        let mut layout = preferences.layout_for_viewport(viewport_width, viewport_height);
        layout.reader_colors = self.document_colors;
        layout
    }

    pub(super) fn sync_document_colors(
        &mut self,
        document_colors: Option<DocumentColors>,
        cx: &mut Context<Self>,
    ) {
        let document_colors = document_colors.map(DocumentColors::sanitized);
        if self.document_colors == document_colors {
            return;
        }

        self.document_colors = document_colors;
        self.pending_renders.clear();
        self.deactivate_render_cache(cx);

        if self.capabilities.reflow {
            let position = self
                .pending_semantic_position
                .clone()
                .or_else(|| self.pending_initial_position.clone())
                .filter(DocumentPosition::is_reflow)
                .unwrap_or_else(|| self.current_document_position());
            self.pending_initial_position = Some(position);
            self.document_layout = DocumentLayout::Reflow(self.epub_layout_for_viewport(
                self.epub_preferences,
                self.viewport_width.max(240.0),
                960.0,
            ));
            self.request_reload(cx);
        }

        cx.notify();
    }

    fn update_epub_preferences(
        &mut self,
        update: impl FnOnce(&mut EpubReaderPreferences),
        cx: &mut Context<Self>,
    ) {
        if !self.capabilities.reflow {
            return;
        }
        let mut preferences = self.epub_preferences;
        update(&mut preferences);
        preferences = preferences.sanitized();
        if preferences == self.epub_preferences {
            return;
        }
        if let Err(error) = cx
            .global_mut::<LibraryStore>()
            .set_epub_reader_preferences(&self.path, preferences)
        {
            eprintln!("failed to save EPUB reader preferences: {error:#}");
            return;
        }
        self.epub_preferences = preferences;
        let position = self
            .pending_semantic_position
            .clone()
            .or_else(|| self.pending_initial_position.clone())
            .filter(DocumentPosition::is_reflow)
            .unwrap_or_else(|| self.current_document_position());
        self.pending_initial_position = Some(position);
        self.pending_renders.clear();
        self.document_layout = DocumentLayout::Reflow(self.epub_layout_for_viewport(
            preferences,
            self.viewport_width.max(240.0),
            960.0,
        ));
        self.request_reload(cx);
        cx.notify();
    }

    pub(super) fn increase_epub_font_size(&mut self, cx: &mut Context<Self>) {
        self.update_epub_preferences(|preferences| preferences.font_size += 1.0, cx);
    }

    pub(super) fn decrease_epub_font_size(&mut self, cx: &mut Context<Self>) {
        self.update_epub_preferences(|preferences| preferences.font_size -= 1.0, cx);
    }

    pub(super) fn increase_epub_line_height(&mut self, cx: &mut Context<Self>) {
        self.update_epub_preferences(|preferences| preferences.line_height += 0.1, cx);
    }

    pub(super) fn decrease_epub_line_height(&mut self, cx: &mut Context<Self>) {
        self.update_epub_preferences(|preferences| preferences.line_height -= 0.1, cx);
    }

    pub(super) fn increase_epub_horizontal_margin(&mut self, cx: &mut Context<Self>) {
        self.update_epub_preferences(|preferences| preferences.horizontal_margin += 4.0, cx);
    }

    pub(super) fn decrease_epub_horizontal_margin(&mut self, cx: &mut Context<Self>) {
        self.update_epub_preferences(|preferences| preferences.horizontal_margin -= 4.0, cx);
    }

    pub(super) fn increase_epub_max_content_width(&mut self, cx: &mut Context<Self>) {
        self.update_epub_preferences(|preferences| preferences.max_content_width += 40.0, cx);
    }

    pub(super) fn decrease_epub_max_content_width(&mut self, cx: &mut Context<Self>) {
        self.update_epub_preferences(|preferences| preferences.max_content_width -= 40.0, cx);
    }

    pub(super) fn toggle_epub_publisher_styles(&mut self, cx: &mut Context<Self>) {
        self.update_epub_preferences(
            |preferences| preferences.use_publisher_styles = !preferences.use_publisher_styles,
            cx,
        );
    }

    pub(super) fn set_epub_theme(&mut self, theme: EpubTheme, cx: &mut Context<Self>) {
        self.update_epub_preferences(|preferences| preferences.theme = theme, cx);
    }

    pub(super) fn reset_epub_preferences(&mut self, cx: &mut Context<Self>) {
        if !self.capabilities.reflow {
            return;
        }
        let preferences = cx.global::<SettingsStore>().settings.reading.sanitized();
        if let Err(error) = cx
            .global_mut::<LibraryStore>()
            .clear_epub_reader_preferences(&self.path)
        {
            eprintln!("failed to clear EPUB reader preferences: {error:#}");
            return;
        }
        if preferences == self.epub_preferences {
            cx.notify();
            return;
        }
        let position = self
            .pending_semantic_position
            .clone()
            .or_else(|| self.pending_initial_position.clone())
            .filter(DocumentPosition::is_reflow)
            .unwrap_or_else(|| self.current_document_position());
        self.pending_initial_position = Some(position);
        self.epub_preferences = preferences;
        self.document_layout = DocumentLayout::Reflow(self.epub_layout_for_viewport(
            preferences,
            self.viewport_width.max(240.0),
            960.0,
        ));
        self.request_reload(cx);
        cx.notify();
    }

    pub(super) fn toggle_epub_style_panel(
        &mut self,
        position: GpuiPoint<Pixels>,
        cx: &mut Context<Self>,
    ) {
        if self.epub_style_panel_position.take().is_none() {
            self.close_context_menu(cx);
            self.epub_style_panel_position = Some(point(
                position.x,
                position.y - px(DOCUMENT_FOOTER_HEIGHT / 2.0 + 4.0),
            ));
        }
        cx.notify();
    }

    pub(super) fn close_epub_style_panel(&mut self, cx: &mut Context<Self>) {
        if self.epub_style_panel_position.take().is_some() {
            cx.notify();
        }
    }
}
