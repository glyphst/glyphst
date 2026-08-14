use super::*;

const FILE_WATCH_POLL_INTERVAL: Duration = Duration::from_millis(500);
const FILE_WATCH_STABLE_DELAY: Duration = Duration::from_millis(300);
const PREVIOUS_DOCUMENT_EVICT_DELAY: Duration = Duration::from_millis(1500);

impl DocumentView {
    pub fn prepare_open(
        path: impl AsRef<Path>,
        epub_preferences: EpubReaderPreferences,
        page_gap: f32,
    ) -> Result<PreparedDocumentOpen> {
        let path = normalize_path(path.as_ref());
        let mut file_name: SharedString = path
            .file_name()
            .and_then(|name| name.to_str())
            .map(ToString::to_string)
            .unwrap_or_else(|| crate::fl!("untitled"))
            .into();

        let document_snapshot = Arc::new(DocumentSnapshot::read(&path)?);
        let format = document_snapshot.format();
        let epub_preferences = epub_preferences.sanitized();
        let requested_layout = if format == DocumentFormat::Epub {
            DocumentLayout::Reflow(epub_preferences.layout_for_viewport(720.0, 960.0))
        } else {
            DocumentLayout::Fixed
        };
        let opened_document = document_snapshot.open_with_layout(requested_layout)?;
        let capabilities = opened_document.capabilities();
        let document_layout = opened_document.layout();
        if format == DocumentFormat::Epub
            && let Ok(title) = opened_document.inner().metadata(mupdf::MetadataName::Title)
            && !title.trim().is_empty()
        {
            file_name = title.into();
        }
        let document = opened_document.into_inner();
        let page_count = document.page_count()?;
        if page_count <= 0 {
            bail!("document has no readable content");
        }
        let outlines = match document.outlines() {
            Ok(outlines) => outlines,
            Err(error) => {
                eprintln!("failed to read document table of contents: {error:#}");
                Vec::new()
            }
        };
        let toc = Self::flatten_toc(outlines, page_count as usize);

        let page_count = page_count as usize;
        let loaded = Self::load_pages_from_document(&document, page_count, page_gap)?;
        drop(document);
        let render_session =
            DocumentRenderSession::open(document_snapshot.clone(), document_layout)?;

        Ok(PreparedDocumentOpen {
            path,
            file_name,
            format,
            capabilities,
            document_layout,
            epub_preferences,
            document_snapshot,
            render_session,
            pages: loaded.pages,
            page_count,
            max_page_width: loaded.max_page_width.max(1.0),
            content_height_doc: loaded.content_height_doc,
            page_gap,
            toc,
        })
    }

    /// Capture the immutable document data needed to create another view.
    ///
    /// This intentionally excludes all view-local interaction state. The new
    /// view receives fresh search, selection, overlay, navigation, and AI
    /// state through [`Self::from_prepared`], while sharing the source
    /// snapshot and renderer resources.
    pub fn prepared_duplicate(&self) -> PreparedDocumentOpen {
        PreparedDocumentOpen {
            path: self.path.clone(),
            file_name: self.file_name.clone(),
            format: self.format,
            capabilities: self.capabilities,
            document_layout: self.document_layout,
            epub_preferences: self.epub_preferences,
            document_snapshot: self.document_snapshot.clone(),
            render_session: self.render_session.clone(),
            pages: self.pages.clone(),
            page_count: self.page_count,
            max_page_width: self.max_page_width,
            content_height_doc: self.content_height_doc,
            page_gap: self.page_gap,
            toc: self.toc.clone(),
        }
    }

    pub fn from_prepared(
        prepared: PreparedDocumentOpen,
        focus_handle: FocusHandle,
        behavior: BehaviorSettings,
        options: DocumentOpenOptions,
    ) -> Result<Self> {
        let PreparedDocumentOpen {
            path,
            file_name,
            format,
            capabilities,
            document_layout,
            epub_preferences,
            document_snapshot,
            render_session,
            pages,
            page_count,
            max_page_width,
            content_height_doc,
            page_gap,
            toc,
        } = prepared;
        let rotation = if capabilities.reflow {
            DocumentRotation::default()
        } else {
            options.rotation
        };
        let page_layout = if capabilities.two_column_layout {
            options.page_layout
        } else {
            DocumentPageLayout::default()
        };
        let page_image_bounds = (0..pages.len()).map(|_| None).collect();
        let mut view = Self {
            doc_id: next_document_id(),
            path,
            file_name,
            format,
            capabilities,
            document_layout,
            epub_preferences,
            epub_style_panel_position: None,
            document_snapshot,
            render_session,
            page_render_sources: HashMap::new(),
            pages,
            page_count,
            max_page_width,
            content_height_doc,
            glyphs: Vec::new(),
            text_blocks: Vec::new(),
            text_lines: Vec::new(),
            page_line_ranges: vec![0..0; page_count],
            page_block_ranges: vec![0..0; page_count],
            view_language: DocumentViewLanguage::Original,
            translation_view_role: TranslationViewRole::None,
            translation_pair_active: false,
            translation_overlay_enabled: true,
            translation_position_sync_ready: false,
            pending_translation_viewport_anchor: None,
            suppress_linked_events: false,
            external_hovered_translation_region: None,
            hovered_translation_region: None,
            links: Vec::new(),
            highlights: options.annotations.highlights,
            notes: options.annotations.notes,
            selected_highlight: None,
            selection: None,
            region_tool_active: false,
            region_drag: None,
            selected_region: None,
            translation_popup: None,
            pinned_translation: None,
            translation_popup_scroll: ScrollHandle::new(),
            pinned_translation_scroll: ScrollHandle::new(),
            target_language: options.target_language,
            translation_popup_language_select: None,
            pinned_translation_language_select: None,
            translation_request_generation: 0,
            document_translation_jobs: options.document_translation_jobs,
            translation_session_id: None,
            translation_pair_session_id: None,
            translation_session_snapshot: None,
            translation_hover_regions: HashMap::new(),
            translation_failed_hover_regions: HashSet::new(),
            translation_hover_animation: None,
            translation_hover_clear_generation: 0,
            translation_hover_clear_task: None,
            translation_hover_tools_hovered: false,
            translation_hover_tools_bounds: None,
            translation_panel_placement: TranslationPanelPlacement::Default,
            translation_panel_drag: None,
            translation_panel_size: size(
                px(TRANSLATION_PANEL_PREFERRED_WIDTH),
                px(TRANSLATION_PANEL_DEFAULT_HEIGHT),
            ),
            translation_panel_initial_dock_armed: false,
            translation_panel_initial_dock_generation: 0,
            translation_panel_initial_dock_task: None,
            translation_panel_dock_generation: 0,
            translation_panel_dock_task: None,
            translation_panel_retract_generation: 0,
            translation_panel_retract_task: None,
            translation_panel_dock_panel_hovered: false,
            translation_panel_dock_edge_hovered: false,
            pending_selection_anchor: None,
            dragging: false,
            hovered_link: None,
            hovered_note: None,
            hovered_note_position: None,
            pending_link: None,
            panning: None,
            page_image_bounds,
            focus_handle,
            pending_renders: Vec::new(),
            rotation,
            page_layout,
            page_gap,
            zoom: behavior.default_zoom,
            base_scale: 1.0,
            viewport_origin: point(px(0.0), px(0.0)),
            viewport_width: 0.0,
            viewport_height: 0.0,
            scroll_x_doc: 0.0,
            scroll_y_doc: 0.0,
            target_scroll_x_doc: 0.0,
            target_scroll_y_doc: 0.0,
            document_scroll: ManualScrollHandle::new(),
            search_bar: None,
            toc_palette: None,
            notes_palette: None,
            history_palette: None,
            bookmark_palette: None,
            bookmark_prompt: None,
            go_to_page_prompt: None,
            note_editor: None,
            note_target: None,
            toc,
            toc_collapsed: HashSet::new(),
            toc_sidebar_scroll: UniformListScrollHandle::new(),
            toc_sidebar_visible_entry_ixs: Vec::new(),
            toc_sidebar_entries_dirty: true,
            toc_sidebar_selected: None,
            toc_sidebar_follow_current: true,
            toc_sidebar_manual_scroll: Rc::new(Cell::new(false)),
            toc_sidebar_last_scroll_offset: point(px(0.0), px(0.0)),
            notes_sidebar_scroll: UniformListScrollHandle::new(),
            notes_sidebar_entries: Vec::new(),
            notes_sidebar_selected: None,
            thumbnail_sidebar_scroll: UniformListScrollHandle::new(),
            thumbnail_sidebar_current_page: 0,
            thumbnail_sidebar_selecting: false,
            thumbnail_sidebar_selection: BTreeSet::new(),
            viewport_render_keys: HashSet::new(),
            thumbnail_sidebar_render_keys: SurfaceRenderKeys::default(),
            dock_snapshot: DocumentDockSnapshot::default(),
            grid_overview_open: false,
            grid_overview_scroll: UniformListScrollHandle::new(),
            grid_overview_zoom: GRID_OVERVIEW_DEFAULT_ZOOM,
            grid_overview_cols: GRID_OVERVIEW_DEFAULT_COLS,
            grid_overview_cursor: 0,
            grid_overview_viewport_width: 0.0,
            grid_overview_selecting: false,
            grid_overview_selection: BTreeSet::new(),
            grid_overview_render_keys: SurfaceRenderKeys::default(),
            presentation_page: None,
            jump_back: Vec::new(),
            jump_forward: Vec::new(),
            position_from_search: false,
            search: SearchState::default(),
            pending_initial_position: options.initial_position,
            pending_semantic_position: None,
            reflow_resize_task: None,
            file_watch_task: None,
            reload_task: None,
            reload_coordinator: ReloadCoordinator::default(),
            previous_doc_id: None,
            previous_doc_evict_task: None,
            diff_flashes: Vec::new(),
            diff_pending_pages: BTreeSet::new(),
            diff_flash_clear_task: None,
            text_index_state: TextIndexState::Pending,
            text_indexing_task: None,
            position_save_task: None,
            context_menu_position: None,
            context_menu_popup: None,
            context_menu_page: None,
            context_menu_image: None,
            context_menu_highlight: None,
            context_menu_note: None,
            context_menu_region: false,
            document_edit_revision: DocumentEditRevision::default(),
            bookmarks_revision: 0,
            _image_clipboard: None,
        };
        view.recompute_layout(page_gap);
        if let Some(position) = view.pending_initial_position.take() {
            view.restore_position(position);
        }
        Ok(view)
    }

    pub fn register_cache_eviction(&mut self, cx: &mut Context<Self>) {
        let store = cx.global::<LibraryStore>();
        self.document_edit_revision = store.document_edit_revision(&self.path);
        self.bookmarks_revision = store.bookmarks_revision();

        cx.on_release(|this, cx| {
            if cx.has_global::<RenderCache>() {
                let mut doc_ids = Vec::with_capacity(2);
                doc_ids.push(this.doc_id);
                if let Some(previous_doc_id) = this.previous_doc_id
                    && previous_doc_id != this.doc_id
                {
                    doc_ids.push(previous_doc_id);
                }

                for doc_id in doc_ids {
                    let removed_images = cx.global_mut::<RenderCache>().evict_document(doc_id);
                    for image in removed_images {
                        cx.drop_image(image, None);
                    }
                }
            }
        })
        .detach();

        cx.observe_global::<SettingsStore>(|this, cx| {
            let page_gap = cx.global::<SettingsStore>().resolved_theme.page_gap;
            this.recompute_layout(page_gap);
            if !this.capabilities.reflow {
                this.base_scale = this.viewport_width.max(1.0) / this.max_page_width.max(1.0);
            }
            cx.notify();
        })
        .detach();

        cx.observe_global::<LibraryStore>(|this, cx| {
            this.sync_document_edits_from_store(cx);
        })
        .detach();
    }

    fn initialize_translation_selects(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.translation_popup_language_select.is_some() {
            return;
        }
        let selected = self.target_language.clone();
        let options = target_language_options(&selected);
        let selected_index = options
            .iter()
            .position(|option| option.value == selected)
            .map(IndexPath::new);
        let popup_state = cx.new(|cx| {
            SelectState::new(SearchableVec::new(options), selected_index, window, cx)
                .searchable(true)
        });
        let popup = TargetLanguageSelect {
            trigger_focus: popup_state.read(cx).focus_handle(cx),
            state: popup_state,
        };

        let selected = self.target_language.clone();
        let options = target_language_options(&selected);
        let selected_index = options
            .iter()
            .position(|option| option.value == selected)
            .map(IndexPath::new);
        let pinned_state = cx.new(|cx| {
            SelectState::new(SearchableVec::new(options), selected_index, window, cx)
                .searchable(true)
        });
        let pinned = TargetLanguageSelect {
            trigger_focus: pinned_state.read(cx).focus_handle(cx),
            state: pinned_state,
        };

        cx.subscribe_in(&popup.state, window, |this, source, event, window, cx| {
            let SelectEvent::Confirm(language) = event;
            if let Some(language) = language {
                this.change_target_language(language.clone(), source, window, cx);
            }
        })
        .detach();
        cx.subscribe_in(&pinned.state, window, |this, source, event, window, cx| {
            let SelectEvent::Confirm(language) = event;
            if let Some(language) = language {
                this.change_target_language(language.clone(), source, window, cx);
            }
        })
        .detach();
        cx.observe(&popup.state, |_, _, cx| cx.notify()).detach();
        cx.observe(&pinned.state, |this, _, cx| {
            this.sync_translation_panel_owned_surfaces(cx);
            cx.notify();
        })
        .detach();
        self.translation_popup_language_select = Some(popup);
        self.pinned_translation_language_select = Some(pinned);
    }

    pub fn initialize_runtime(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.initialize_translation_selects(window, cx);
        let translation_jobs = self.document_translation_jobs.clone();
        cx.observe(&translation_jobs, |this, _, cx| {
            this.refresh_translation_session(cx);
        })
        .detach();
        self.start_file_watch(cx);
    }

    fn file_signature(path: &Path) -> Option<FileSignature> {
        let metadata = fs::metadata(path).ok()?;
        Some(FileSignature {
            modified: metadata.modified().ok(),
            len: metadata.len(),
        })
    }

    fn start_file_watch(&mut self, cx: &mut Context<Self>) {
        if self.file_watch_task.is_some() {
            return;
        }

        let path = self.path.clone();
        self.file_watch_task = Some(cx.spawn(async move |this, cx| {
            let mut last_signature = cx
                .background_spawn({
                    let path = path.clone();
                    async move { Self::file_signature(&path) }
                })
                .await;
            let mut pending_since: Option<Instant> = None;

            loop {
                cx.background_executor()
                    .timer(FILE_WATCH_POLL_INTERVAL)
                    .await;

                let signature = cx
                    .background_spawn({
                        let path = path.clone();
                        async move { Self::file_signature(&path) }
                    })
                    .await;

                let Some(signature) = signature else {
                    continue;
                };

                if last_signature != Some(signature) {
                    last_signature = Some(signature);
                    pending_since = Some(Instant::now());
                    continue;
                }

                if let Some(since) = pending_since
                    && since.elapsed() >= FILE_WATCH_STABLE_DELAY
                {
                    pending_since = None;
                    if this.update(cx, |this, cx| this.request_reload(cx)).is_err() {
                        break;
                    }
                }
            }
        }));
    }

    pub(super) fn request_reload(&mut self, cx: &mut Context<Self>) {
        let Some(generation) = self.reload_coordinator.request() else {
            return;
        };
        self.start_reload(generation, cx);
    }

    fn start_reload(&mut self, generation: u64, cx: &mut Context<Self>) {
        let path = self.path.clone();
        let document_layout = self.document_layout;
        let page_gap = cx.global::<SettingsStore>().resolved_theme.page_gap;

        self.reload_task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn({
                    let path = path.clone();
                    async move {
                        Self::load_reload_data(&path, document_layout, page_gap)
                            .map_err(|error| format!("{error:#}"))
                    }
                })
                .await;

            Self::finish_reload(this, cx, generation, result);
        }));
    }

    fn load_reload_data(
        path: &Path,
        document_layout: DocumentLayout,
        page_gap: f32,
    ) -> Result<ReloadData> {
        let snapshot = Arc::new(DocumentSnapshot::read(path)?);
        let document = snapshot.open_with_layout(document_layout)?.into_inner();
        let page_count = document.page_count()?;
        if page_count <= 0 {
            bail!("document has no readable content");
        }
        let page_count = page_count as usize;
        let toc = match document.outlines() {
            Ok(outlines) => Self::flatten_toc(outlines, page_count),
            Err(error) => {
                eprintln!("failed to read PDF table of contents: {error:#}");
                Vec::new()
            }
        };
        let loaded = Self::load_pages_from_document(&document, page_count, page_gap)?;
        drop(document);
        let render_session = DocumentRenderSession::open(snapshot.clone(), document_layout)?;

        Ok(ReloadData {
            snapshot,
            render_session,
            page_count,
            loaded,
            toc,
        })
    }

    fn finish_reload(
        this: gpui::WeakEntity<Self>,
        cx: &mut AsyncApp,
        generation: u64,
        result: std::result::Result<ReloadData, String>,
    ) {
        let update_result = this.update(cx, |this, cx| {
            this.reload_task = None;
            match this.reload_coordinator.finish_reload(generation) {
                ReloadDisposition::Apply => match result {
                    Ok(reload) => {
                        if let Err(error) = this.apply_reloaded_document(reload, cx) {
                            eprintln!("failed to reload PDF: {error:#}");
                        }
                    }
                    Err(error) => eprintln!("failed to reload PDF: {error}"),
                },
                ReloadDisposition::DiscardAndStart(next_generation) => {
                    this.start_reload(next_generation, cx);
                }
                ReloadDisposition::Discard => {}
            }
        });
        if let Err(error) = update_result
            && error.to_string() != "entity released"
        {
            eprintln!("failed to update reloaded PDF: {error:#}");
        }
    }

    fn apply_reloaded_document(
        &mut self,
        reload: ReloadData,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        let pending_position = self.pending_initial_position.take();
        let diff_pending_pages = if self.capabilities.reload_diff {
            self.page_image_bounds
                .iter()
                .enumerate()
                .filter_map(|(page_idx, bounds)| bounds.as_ref().map(|_| page_idx))
                .filter(|page_idx| *page_idx < reload.loaded.pages.len())
                .collect::<BTreeSet<_>>()
        } else {
            BTreeSet::new()
        };

        self.evict_previous_document(cx);
        self.text_indexing_task = None;
        self.pending_renders.clear();
        self.diff_flash_clear_task = None;
        self.diff_flashes.clear();

        let previous_doc_id = self.doc_id;
        self.previous_doc_id = Some(previous_doc_id);
        self.doc_id = next_document_id();
        self.viewport_render_keys.clear();
        self.thumbnail_sidebar_render_keys.clear();
        self.grid_overview_render_keys.clear();
        cx.global_mut::<RenderCache>()
            .clear_protected_renders(previous_doc_id);
        self.schedule_previous_document_eviction(previous_doc_id, cx);

        self.document_snapshot = reload.snapshot;
        self.render_session = reload.render_session;
        self.page_count = reload.page_count;
        self.toc = reload.toc;
        self.toc_collapsed
            .retain(|entry_ix| *entry_ix < self.toc.len());
        self.toc_sidebar_selected = None;
        self.toc_sidebar_manual_scroll.set(false);
        self.toc_sidebar_entries_dirty = true;
        self.pages = reload.loaded.pages;
        self.recompute_layout(cx.global::<SettingsStore>().resolved_theme.page_gap);
        self.glyphs.clear();
        self.text_blocks.clear();
        self.text_lines.clear();
        self.page_line_ranges = vec![0..0; self.pages.len()];
        self.page_block_ranges = vec![0..0; self.pages.len()];
        self.replace_translation_document();
        self.links.clear();
        self.search.matches.clear();
        self.search.active = None;
        self.selection = None;
        self.region_tool_active = false;
        self.region_drag = None;
        self.selected_region = None;
        self.context_menu_region = false;
        self.pending_selection_anchor = None;
        self.hovered_link = None;
        self.pending_link = None;
        self.page_image_bounds = (0..self.pages.len()).map(|_| None).collect();
        self.text_index_state = TextIndexState::Pending;
        self.search.cancel_reveal_after_indexing();
        self.diff_pending_pages = diff_pending_pages;
        self.grid_overview_cursor = self.grid_overview_cursor.min(self.last_page_index());
        self.thumbnail_sidebar_current_page = self
            .thumbnail_sidebar_current_page
            .min(self.last_page_index());
        self.grid_overview_selection
            .retain(|page_idx| *page_idx < self.pages.len());
        self.thumbnail_sidebar_selection
            .retain(|page_idx| *page_idx < self.pages.len());
        if self
            .context_menu_page
            .is_some_and(|page_idx| page_idx >= self.pages.len())
        {
            self.context_menu_page = None;
        }
        if let Some(position) = pending_position {
            self.restore_position(position);
        } else {
            self.clamp_scroll();
        }
        self.refresh_notes_sidebar_entries();
        self.sync_search_bar(cx);
        cx.notify();

        Ok(())
    }

    fn evict_cached_document(doc_id: u64, cx: &mut Context<Self>) {
        let removed_images = cx.global_mut::<RenderCache>().evict_document(doc_id);
        for image in removed_images {
            cx.drop_image(image, None);
        }
    }

    fn evict_previous_document(&mut self, cx: &mut Context<Self>) {
        self.previous_doc_evict_task = None;
        if let Some(previous_doc_id) = self.previous_doc_id.take() {
            Self::evict_cached_document(previous_doc_id, cx);
        }
    }

    fn schedule_previous_document_eviction(&mut self, doc_id: u64, cx: &mut Context<Self>) {
        self.previous_doc_evict_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(PREVIOUS_DOCUMENT_EVICT_DELAY)
                .await;

            let update_result = this.update(cx, |this, cx| {
                if this.previous_doc_id != Some(doc_id) {
                    return;
                }

                if !this.pending_renders.is_empty() {
                    this.previous_doc_evict_task = None;
                    this.schedule_previous_document_eviction(doc_id, cx);
                    return;
                }

                this.previous_doc_id = None;
                this.previous_doc_evict_task = None;
                Self::evict_cached_document(doc_id, cx);
                cx.notify();
            });
            if let Err(error) = update_result
                && error.to_string() != "entity released"
            {
                eprintln!("failed to evict previous PDF render cache: {error:#}");
            }
        }));
    }

    fn load_pages_from_document(
        document: &mupdf::Document,
        page_count: usize,
        page_gap: f32,
    ) -> Result<LoadedPages> {
        let mut pages = Vec::with_capacity(page_count);
        let mut max_page_width = 0.0_f32;

        for page_idx in 0..page_count {
            let page = document.load_page(page_idx as i32)?;
            let page_bounds = page.bounds()?;
            let page_width = page_bounds.x1 - page_bounds.x0;
            let page_height = page_bounds.y1 - page_bounds.y0;

            pages.push(DocumentPage {
                bounds: page_bounds,
                width: page_width,
                height: page_height,
                offset_x: 0.0,
                offset_y: 0.0,
                glyph_range: 0..0,
            });

            max_page_width = max_page_width.max(page_width);
        }

        let loaded = Self::merge_page_chunks(
            vec![PageChunk {
                start_page_idx: 0,
                pages,
                max_page_width,
            }],
            page_gap,
        )
        .map_err(anyhow::Error::msg)?;
        Ok(loaded)
    }

    #[cfg(test)]
    pub(super) fn provisional_reflow_pages(
        page_count: usize,
        page_bounds: Rect,
        page_gap: f32,
    ) -> Result<LoadedPages> {
        let page_width = page_bounds.x1 - page_bounds.x0;
        let page_height = page_bounds.y1 - page_bounds.y0;
        if page_width <= 0.0 || page_height <= 0.0 {
            bail!("reflowable document page has invalid bounds");
        }
        let pages = (0..page_count)
            .map(|_| DocumentPage {
                bounds: page_bounds,
                width: page_width,
                height: page_height,
                offset_x: 0.0,
                offset_y: 0.0,
                glyph_range: 0..0,
            })
            .collect();
        Self::merge_page_chunks(
            vec![PageChunk {
                start_page_idx: 0,
                pages,
                max_page_width: page_width,
            }],
            page_gap,
        )
        .map_err(anyhow::Error::msg)
    }

    fn merge_page_chunks(
        mut chunks: Vec<PageChunk>,
        page_gap: f32,
    ) -> std::result::Result<LoadedPages, String> {
        chunks.sort_by_key(|chunk| chunk.start_page_idx);

        let page_count = chunks.iter().map(|chunk| chunk.pages.len()).sum();
        let mut pages = Vec::with_capacity(page_count);
        let mut offset_y = 0.0;
        let mut max_page_width = 0.0_f32;

        for mut chunk in chunks {
            max_page_width = max_page_width.max(chunk.max_page_width);

            for page in &mut chunk.pages {
                page.offset_x = 0.0;
                page.offset_y = offset_y;
                offset_y += page.height + page_gap;
            }

            pages.extend(chunk.pages);
        }

        let content_height_doc = if pages.is_empty() {
            0.0
        } else {
            offset_y - page_gap
        };

        Ok(LoadedPages {
            pages,
            max_page_width,
            content_height_doc,
        })
    }
}
