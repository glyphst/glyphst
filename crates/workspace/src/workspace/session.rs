//! Reader session snapshots and ordered restoration.

use super::*;

impl Workspace {
    pub fn from_reader_session(
        session: ReaderWindowSession,
        callbacks: WorkspaceCallbacks,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let document_translation_jobs = cx.new(DocumentTranslationManager::new);
        let mut restored = RestoredLayout::default();
        let layout = restored.layout(&session.pane_tree, &document_translation_jobs, window, cx);
        let active_tab = restored
            .pane_tabs
            .iter()
            .find(|(pane_id, _)| *pane_id == session.active_pane)
            .or_else(|| restored.pane_tabs.first())
            .map(|(_, tab)| tab.clone());
        let mut workspace = Self::with_layout(
            Some(layout),
            document_translation_jobs,
            callbacks,
            window,
            cx,
        );
        if let Some(pane) = active_tab.and_then(|tab| workspace.pane_for_tab(tab.entity_id(), cx)) {
            workspace.active_pane = pane;
        }
        workspace.restore.tab_ids = restored.tab_ids;
        workspace.restore.pending_documents = restored.pending_documents;
        workspace.restore.dock_document_tab = session.dock_document_tab;
        workspace.restore.synchronized_groups = session.synchronized_groups;
        workspace.restore.translation_sessions = session.translation_sessions;
        workspace.restore.missing_documents = restored.missing_documents;
        workspace.docks.left_sidebar = session.left_sidebar.map(|sidebar| match sidebar {
            ReaderSidebar::Thumbnails => DocumentSidebar::Thumbnails,
            ReaderSidebar::TableOfContents => DocumentSidebar::Toc,
            ReaderSidebar::Bookmarks => DocumentSidebar::Bookmarks,
            ReaderSidebar::Notes => DocumentSidebar::Notes,
        });
        workspace.docks.ai_panel_open = session.ai_panel_open;
        workspace.transparency_enabled = session.transparency_enabled;
        apply_window_background(
            window,
            workspace.window_settings,
            workspace.transparency_enabled,
        );
        if workspace.restore.pending_documents == 0 {
            workspace.finish_reader_session_restore(window, cx);
        }
        workspace.focus_active_pane(window, cx);
        workspace
    }

    pub fn reader_session_restore_pending(&self) -> bool {
        self.restore.pending_documents > 0
    }

    pub fn restored_missing_document_count(&self) -> usize {
        self.restore.missing_documents
    }

    pub fn reader_window_session(
        &self,
        window: &Window,
        cx: &Context<Self>,
    ) -> ReaderWindowSession {
        let panes = self.pane_session_snapshot(cx);
        let window_bounds = window.window_bounds();
        let state = match window_bounds {
            gpui::WindowBounds::Windowed(_) => ReaderWindowState::Windowed,
            gpui::WindowBounds::Maximized(_) => ReaderWindowState::Maximized,
            gpui::WindowBounds::Fullscreen(_) => ReaderWindowState::Fullscreen,
        };
        let bounds = window_bounds.get_bounds();
        let mut translation_sessions = self
            .translations
            .live_pairs
            .iter()
            .filter_map(|(session_id, pair)| {
                let source_tab = panes.runtime_tab_ids.get(&pair.source_id).copied()?;
                let translated_tab = panes.runtime_tab_ids.get(&pair.translated_id).copied()?;
                let snapshot = self
                    .translations
                    .manager
                    .read(cx)
                    .session_snapshot(*session_id)?;
                let mut original_unit_ids = snapshot
                    .original_unit_ids
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>();
                original_unit_ids.sort();
                Some(ReaderTranslationSession {
                    source_tab,
                    translated_tab,
                    source_language: snapshot
                        .request
                        .source_language
                        .as_ref()
                        .map(|language| language.as_str().to_string()),
                    target_language: snapshot.request.target_language.as_str().to_string(),
                    mode: snapshot.mode.as_str().to_string(),
                    cache_key: snapshot
                        .cache_key
                        .as_ref()
                        .map(|key| key.as_str().to_string()),
                    original_unit_ids,
                })
            })
            .collect::<Vec<_>>();
        translation_sessions.sort_by_key(|session| session.translated_tab);
        ReaderWindowSession {
            bounds: ReaderWindowBounds {
                x: bounds.origin.x.as_f32(),
                y: bounds.origin.y.as_f32(),
                width: bounds.size.width.as_f32(),
                height: bounds.size.height.as_f32(),
                state,
            },
            pane_tree: panes.pane_tree,
            active_pane: panes.active_pane,
            dock_document_tab: self
                .dock_document_tab_id(cx)
                .and_then(|tab_id| panes.runtime_tab_ids.get(&tab_id).copied()),
            left_sidebar: self.docks.left_sidebar.map(|sidebar| match sidebar {
                DocumentSidebar::Thumbnails => ReaderSidebar::Thumbnails,
                DocumentSidebar::Toc => ReaderSidebar::TableOfContents,
                DocumentSidebar::Bookmarks => ReaderSidebar::Bookmarks,
                DocumentSidebar::Notes => ReaderSidebar::Notes,
            }),
            ai_panel_open: self.docks.ai_panel_open,
            transparency_enabled: self.transparency_enabled,
            synchronized_groups: self
                .synchronized_tabs
                .reader_session_groups(&panes.runtime_tab_ids),
            translation_sessions,
        }
    }

    pub(super) fn finish_reader_session_restore(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let groups = std::mem::take(&mut self.restore.synchronized_groups);
        for group in groups {
            let members = group
                .tabs
                .iter()
                .filter_map(|session_id| {
                    let tab_id = self.restore.tab_ids.get(session_id).copied()?;
                    let pane_id = self.pane_for_tab(tab_id, cx)?;
                    let is_document = self.pane(pane_id, cx).tab_is_document(tab_id, cx);
                    is_document.then_some((*session_id, tab_id, pane_id))
                })
                .collect::<Vec<_>>();
            let Some((_, source_tab, source_pane)) = members.first().copied() else {
                continue;
            };
            let Some((_, target_tab, target_pane)) = members.get(1).copied() else {
                continue;
            };
            let Some(group_id) = self.synchronized_tabs.create_group(
                source_tab,
                source_pane,
                target_tab,
                target_pane,
            ) else {
                continue;
            };
            for (_, tab_id, pane_id) in members.iter().skip(2).copied() {
                self.synchronized_tabs.add_member(group_id, tab_id, pane_id);
            }
            for paused_session_id in &group.paused_tabs {
                if let Some(tab_id) = self.restore.tab_ids.get(paused_session_id) {
                    self.synchronized_tabs.set_paused(*tab_id, true);
                }
            }
        }
        let translation_sessions = std::mem::take(&mut self.restore.translation_sessions);
        for session in translation_sessions {
            let Some(source_tab_id) = self.restore.tab_ids.get(&session.source_tab).copied() else {
                continue;
            };
            let Some(translated_tab_id) =
                self.restore.tab_ids.get(&session.translated_tab).copied()
            else {
                continue;
            };
            let (Some(source_pane_id), Some(translated_pane_id)) = (
                self.pane_for_tab(source_tab_id, cx),
                self.pane_for_tab(translated_tab_id, cx),
            ) else {
                continue;
            };
            let Some(source) = self.document_view_for_tab(source_tab_id, cx) else {
                continue;
            };
            let Some(translated) = self.document_view_for_tab(translated_tab_id, cx) else {
                continue;
            };
            let source_language = match session.source_language.as_deref() {
                Some(language) => match LanguageCode::new(language) {
                    Ok(language) => Some(language),
                    Err(error) => {
                        eprintln!("failed to restore translation source language: {error:#}");
                        continue;
                    }
                },
                None => None,
            };
            let target_language = match LanguageCode::new(&session.target_language) {
                Ok(language) => language,
                Err(error) => {
                    eprintln!("failed to restore translation target language: {error:#}");
                    continue;
                }
            };
            let mode = glyphst_settings::DocumentTranslationMode::parse(&session.mode)
                .unwrap_or(glyphst_settings::DocumentTranslationMode::Auto);
            let cache_key = session
                .cache_key
                .and_then(PdfTranslationSessionCacheKey::parse);
            let request = PdfTranslationSessionRequest {
                source_path: source.read(cx).path().to_path_buf(),
                source_language,
                target_language,
            };
            let source_snapshot = source.read(cx).document_snapshot();
            let settings = cx.global::<SettingsStore>().settings.clone();
            let session_id = self.translations.manager.update(cx, |manager, cx| {
                manager.restore_session(
                    RestoredTranslationSubmission {
                        settings,
                        request,
                        source_snapshot,
                        mode,
                        cache_key,
                        original_unit_ids: session.original_unit_ids.into_iter().collect(),
                    },
                    cx,
                )
            });
            source.update(cx, |view, cx| {
                view.attach_translation_source_mirror(session_id, cx);
            });
            translated.update(cx, |view, cx| {
                view.attach_translation_session(session_id, cx);
            });
            if self
                .synchronized_tabs
                .group_for_tab(source_tab_id)
                .is_none()
                && self
                    .synchronized_tabs
                    .group_for_tab(translated_tab_id)
                    .is_none()
            {
                _ = self.synchronized_tabs.connect_tab_to_source_group(
                    source_tab_id,
                    source_pane_id,
                    translated_tab_id,
                    translated_pane_id,
                );
            }
            self.register_live_translation_pair(
                session_id,
                (source_tab_id, source),
                (translated_tab_id, translated),
                window,
                cx,
            );
        }
        if let Some(session_tab_id) = self.restore.dock_document_tab.take()
            && let Some(tab_id) = self.restore.tab_ids.get(&session_tab_id).copied()
            && self.document_target_for_id(tab_id, cx).is_some()
        {
            self.document_context_mru
                .retain(|candidate| *candidate != tab_id);
            self.document_context_mru.insert(0, tab_id);
        } else {
            let active_pane = self.active_pane;
            self.record_document_context(active_pane, cx);
        }
        self.prune_document_context(cx);
        self.restore.tab_ids.clear();
        self.refresh_synchronized_tab_presentation(cx);
        cx.notify();
    }
}

/// What a session's pane tree restores to, before the dock installs it.
#[derive(Default)]
struct RestoredLayout {
    /// Each restored pane's session id and its first tab.
    pane_tabs: Vec<(u64, Entity<TabItem>)>,
    tab_ids: HashMap<u64, EntityId>,
    pending_documents: usize,
    missing_documents: usize,
}

impl RestoredLayout {
    fn layout(
        &mut self,
        tree: &ReaderPaneTree,
        jobs: &Entity<DocumentTranslationManager>,
        window: &mut Window,
        cx: &mut App,
    ) -> PaneLayout {
        match tree {
            ReaderPaneTree::Pane(pane) => self.pane(pane, jobs, window, cx),
            ReaderPaneTree::Split {
                axis,
                ratios,
                children,
            } if !children.is_empty() => {
                let children = children
                    .iter()
                    .enumerate()
                    .map(|(ix, child)| {
                        let share = ratios
                            .get(ix)
                            .copied()
                            .filter(|ratio| ratio.is_finite() && *ratio > 0.0)
                            .unwrap_or(1.0 / ratios.len().max(1) as f32);
                        (self.layout(child, jobs, window, cx), share)
                    })
                    .collect::<Vec<_>>();
                if children.len() == 1 {
                    return children.into_iter().next().map(|(child, _)| child).unwrap();
                }
                PaneLayout::Split {
                    axis: match axis {
                        ReaderSplitAxis::Horizontal => Axis::Horizontal,
                        ReaderSplitAxis::Vertical => Axis::Vertical,
                    },
                    children,
                }
            }
            ReaderPaneTree::Split { .. } => self.pane(
                &ReaderPaneSession {
                    id: 1,
                    tabs: Vec::new(),
                    active_tab: None,
                },
                jobs,
                window,
                cx,
            ),
        }
    }

    fn pane(
        &mut self,
        session: &ReaderPaneSession,
        jobs: &Entity<DocumentTranslationManager>,
        window: &mut Window,
        cx: &mut App,
    ) -> PaneLayout {
        let mut tabs = Vec::new();
        let mut session_ids = Vec::new();
        for tab in &session.tabs {
            let item = match &tab.kind {
                ReaderTabKind::NewTab(state) => Some(TabItem::new_tab_with_session(
                    state.clone(),
                    jobs.clone(),
                    window,
                    cx,
                )),
                ReaderTabKind::Keymap => Some(TabItem::keymap(jobs.clone(), window, cx)),
                ReaderTabKind::Document { path, position } => {
                    if !path.exists() || !crate::tab_item::is_supported_document_path(path) {
                        self.missing_documents += 1;
                        None
                    } else {
                        match TabItem::open_document(
                            path,
                            Some(position.clone()),
                            Some(tab.id),
                            None,
                            jobs.clone(),
                            window,
                            cx,
                        ) {
                            Ok(item) => {
                                self.pending_documents += 1;
                                Some(item)
                            }
                            Err(error) => {
                                eprintln!(
                                    "failed to restore document {}: {error:#}",
                                    path.display()
                                );
                                self.missing_documents += 1;
                                None
                            }
                        }
                    }
                }
            };
            if let Some(item) = item {
                self.tab_ids.insert(tab.id, item.entity_id());
                session_ids.push(tab.id);
                tabs.push(item);
            }
        }
        if tabs.is_empty() {
            tabs.push(TabItem::new_tab(jobs.clone(), window, cx));
        }
        let active_ix = session
            .active_tab
            .and_then(|active| session_ids.iter().position(|id| *id == active))
            .unwrap_or(0)
            .min(tabs.len() - 1);
        self.pane_tabs.push((session.id, tabs[active_ix].clone()));
        PaneLayout::Pane { tabs, active_ix }
    }
}

pub(super) struct PaneSessionSnapshot {
    pub(super) pane_tree: ReaderPaneTree,
    pub(super) active_pane: u64,
    pub(super) runtime_tab_ids: HashMap<EntityId, u64>,
}

impl Workspace {
    pub(super) fn pane_session_snapshot(&self, cx: &App) -> PaneSessionSnapshot {
        let mut next_pane_id = 0_u64;
        let mut next_tab_id = 0_u64;
        let mut active_pane = 0_u64;
        let mut runtime_tab_ids = HashMap::new();
        let active_tab = self.active_pane(cx).active_tab_id();
        let pane_tree = match PaneLayout::of(&self.dock_area, cx) {
            Some(layout) => layout.session_tree(
                active_tab,
                &mut next_pane_id,
                &mut next_tab_id,
                &mut active_pane,
                &mut runtime_tab_ids,
                cx,
            ),
            None => ReaderPaneTree::Pane(ReaderPaneSession {
                id: 1,
                tabs: Vec::new(),
                active_tab: None,
            }),
        };
        PaneSessionSnapshot {
            pane_tree,
            active_pane,
            runtime_tab_ids,
        }
    }
}
