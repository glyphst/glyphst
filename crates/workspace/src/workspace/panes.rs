//! Panes and tabs on the GPUI Kit dock: queries, opening and closing tabs,
//! splits, drag and drop between panes and windows, and rearrangements.

use super::*;

/// Every live workspace's dock area, by area entity id, so a tab dragged out
/// of one window can be found from the window it is dropped on.
#[derive(Default)]
pub(super) struct WorkspaceDockAreas(HashMap<EntityId, gpui::AnyWindowHandle>);

impl gpui::Global for WorkspaceDockAreas {}

impl WorkspaceDockAreas {
    pub(super) fn contains(&self, area: EntityId) -> bool {
        self.0.contains_key(&area)
    }

    pub(super) fn remove(&mut self, area: EntityId) {
        self.0.remove(&area);
    }
}

pub(super) struct TabContextMenu {
    pub(super) position: Point<Pixels>,
    pub(super) popup: Entity<PopupMenu>,
}

impl Workspace {
    pub(super) fn create_dock_area(
        geometry: Rc<RefCell<PaneGeometry>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<DockArea> {
        let skin = ReaderDockSkin::new(cx.weak_entity(), geometry);
        let area = cx.new(|cx| {
            let mut area = DockArea::new("workspace-panes", None, window, cx).with_renderer(skin);
            area.set_host_panel_drops(true);
            area
        });
        let window_handle = window.window_handle();
        cx.default_global::<WorkspaceDockAreas>()
            .0
            .insert(area.entity_id(), window_handle);
        area
    }

    // Queries.

    pub(crate) fn active_pane_id(&self) -> PaneId {
        self.active_pane
    }

    pub(crate) fn synchronized_presentation(&self) -> &SynchronizedPresentation {
        &self.synchronized_presentation
    }

    pub(crate) fn panes(&self, cx: &App) -> Vec<Pane> {
        crate::pane::panes(&self.dock_area, cx)
    }

    #[cfg(test)]
    pub(crate) fn submit_document_password_for_test(
        &mut self,
        pane: PaneId,
        path: &Path,
        password: &str,
        remember: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let path = normalize_path(path);
        let Some(item) = self
            .pane(pane, cx)
            .tabs()
            .iter()
            .find(|tab| {
                let tab = tab.read(cx);
                tab.is_loading() && tab.document_path(cx).as_deref() == Some(path.as_path())
            })
            .cloned()
        else {
            return false;
        };
        item.update(cx, |item, cx| {
            item.submit_document_password_for_test(password, remember, window, cx)
        })
    }

    #[cfg(test)]
    pub(crate) fn connection_tab_state_for_test(
        &self,
        tab_id: EntityId,
    ) -> Option<SynchronizedConnectionTabState> {
        self.synchronized_presentation
            .connection_tabs
            .get(&tab_id)
            .copied()
    }

    pub(crate) fn pane_count(&self, cx: &App) -> usize {
        self.panes(cx).len()
    }

    /// A snapshot of `pane`, empty when the pane no longer exists.
    pub(crate) fn pane(&self, pane: PaneId, cx: &App) -> Pane {
        self.panes(cx)
            .into_iter()
            .find(|candidate| candidate.id() == pane)
            .unwrap_or_else(|| Pane::missing(pane))
    }

    pub(crate) fn active_pane(&self, cx: &App) -> Pane {
        self.pane(self.active_pane, cx)
    }

    pub(crate) fn pane_for_tab(&self, tab_id: EntityId, cx: &App) -> Option<PaneId> {
        self.dock_area
            .read(cx)
            .layout(DockPlacement::Center)?
            .find_panel_node(crate::pane::panel_id(tab_id))
            .map(PaneId::from)
    }

    pub(crate) fn tab_item(&self, tab_id: EntityId, cx: &App) -> Option<Entity<TabItem>> {
        crate::pane::tab_item_for(self.dock_area.read(cx), crate::pane::panel_id(tab_id))
    }

    pub(super) fn all_tab_items(&self, cx: &App) -> Vec<Entity<TabItem>> {
        self.panes(cx)
            .into_iter()
            .flat_map(|pane| pane.tabs().to_vec())
            .collect()
    }

    pub(super) fn all_document_views(&self, cx: &App) -> Vec<Entity<DocumentView>> {
        self.panes(cx)
            .into_iter()
            .flat_map(|pane| pane.document_views(cx))
            .collect()
    }

    pub(crate) fn pane_external_drop_feedback(
        &self,
        pane: PaneId,
    ) -> Option<ExternalDocumentDropFeedback> {
        self.pane_external_drop
            .filter(|(target, _)| *target == pane)
            .map(|(_, feedback)| feedback)
    }

    pub(super) fn tab_bounds(&self, tab_id: EntityId) -> Option<Bounds<Pixels>> {
        self.pane_geometry
            .borrow()
            .tabs
            .get(&tab_id)
            .copied()
            .and_then(PaneTabBounds::visual)
    }

    pub(super) fn bounding_box_for_pane(&self, pane: PaneId) -> Option<Bounds<Pixels>> {
        self.pane_geometry.borrow().panes.get(&pane).copied()
    }

    fn pane_at_pixel_position(&self, position: Point<Pixels>, cx: &App) -> Option<PaneId> {
        let geometry = self.pane_geometry.borrow();
        self.panes(cx)
            .into_iter()
            .map(|pane| pane.id())
            .find(|pane| {
                geometry
                    .panes
                    .get(pane)
                    .is_some_and(|bounds| bounds.contains(&position))
            })
    }

    pub(super) fn find_pane_in_direction(
        &self,
        pane: PaneId,
        direction: SplitDirection,
        cx: &App,
    ) -> Option<PaneId> {
        if self.pane_count(cx) <= 1 {
            return None;
        }
        let bounds = self.bounding_box_for_pane(pane)?;
        let center = bounds.center();
        let distance = px(PANE_DIRECTION_PROBE_DISTANCE);
        let target = match direction {
            SplitDirection::Left => point(bounds.left() - distance, center.y),
            SplitDirection::Right => point(bounds.right() + distance, center.y),
            SplitDirection::Up => point(center.x, bounds.top() - distance),
            SplitDirection::Down => point(center.x, bounds.bottom() + distance),
        };
        self.pane_at_pixel_position(target, cx)
            .filter(|target| *target != pane)
    }

    // Tabs joining and leaving this workspace.

    /// Listen to a tab that now belongs to this workspace.
    pub(super) fn adopt_tab(
        &mut self,
        item: &Entity<TabItem>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let tab_id = item.entity_id();
        let events = cx.subscribe_in(
            item,
            window,
            |this, item, event: &TabItemEvent, window, cx| {
                this.handle_tab_event(item.clone(), event, window, cx);
            },
        );
        let focus_handle = item.read(cx).focus_handle(cx);
        let focus = cx.on_focus_in(&focus_handle, window, move |this, _, cx| {
            this.tab_focused(tab_id, cx);
        });
        self.tab_subscriptions.insert(tab_id, [events, focus]);
    }

    /// Forget a tab that left this workspace.
    pub(super) fn release_tab(&mut self, tab_id: EntityId) {
        self.tab_subscriptions.remove(&tab_id);
        self.pane_geometry.borrow_mut().tabs.remove(&tab_id);
    }

    fn handle_tab_event(
        &mut self,
        item: Entity<TabItem>,
        event: &TabItemEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let tab_id = item.entity_id();
        let pane = self.pane_for_tab(tab_id, cx);
        match event {
            TabItemEvent::OpenFile => {
                if let Some(pane) = pane {
                    self.active_pane = pane;
                }
                self.open_file(window, cx);
            }
            TabItemEvent::OpenDocument {
                path,
                position,
                replace_active_new_tab,
            } => {
                if let Some(pane) = pane {
                    self.active_pane = pane;
                }
                let result = if *replace_active_new_tab {
                    self.open_document_in_active_pane(path.clone(), position.clone(), window, cx)
                } else {
                    self.focus_or_open_document(path.clone(), position.clone(), false, window, cx)
                };
                if let Err(error) = result {
                    eprintln!("failed to open document: {error:#}");
                }
            }
            TabItemEvent::OpenSettings => self.open_settings_window(cx),
            TabItemEvent::SidebarNavigationRequested => {
                if let Some(pane) = pane {
                    self.activate_tab(pane, tab_id, true, window, cx);
                    self.active_pane = pane;
                    self.record_document_context(pane, cx);
                    cx.notify();
                }
            }
            TabItemEvent::DocumentOpenFinished { job_id, result } => {
                if !self.continue_translation_split_open(*job_id, result.clone(), window, cx) {
                    self.translations.manager.update(cx, |jobs, cx| {
                        jobs.finish_open(*job_id, result.clone(), cx);
                    });
                }
            }
            TabItemEvent::RestoredDocumentFinished {
                session_tab_id,
                opened,
            } => {
                if !*opened {
                    self.restore.tab_ids.remove(session_tab_id);
                }
                self.restore.pending_documents = self.restore.pending_documents.saturating_sub(1);
                if self.restore.pending_documents == 0 {
                    self.finish_reader_session_restore(window, cx);
                } else {
                    cx.notify();
                }
            }
            TabItemEvent::DocumentPasswordAuthenticated { path, password } => {
                let workspace = cx.entity().downgrade();
                let path = path.clone();
                let password = password.clone();
                window.defer(cx, move |window, cx| {
                    let Some(workspace) = workspace.upgrade() else {
                        return;
                    };
                    let items = workspace.read(cx).all_tab_items(cx);
                    for item in items {
                        item.update(cx, |item, cx| {
                            item.unlock_with_password(&path, password.clone(), window, cx);
                        });
                    }
                });
            }
            TabItemEvent::CloseRequested => self.close_tab_by_id(tab_id, window, cx),
            TabItemEvent::ContentChanged => {
                self.refresh_live_translation_pair_activity(cx);
                if pane == Some(self.active_pane) {
                    self.exit_zen_mode_for_unsupported_active_tab(cx);
                    self.record_document_context(self.active_pane, cx);
                }
                self.refresh_synchronized_tab_presentation(cx);
            }
            TabItemEvent::DocumentScrolled(delta) => {
                self.propagate_synchronized_scroll(tab_id, *delta, cx);
            }
        }
    }

    fn tab_focused(&mut self, tab_id: EntityId, cx: &mut Context<Self>) {
        let Some(pane) = self.pane_for_tab(tab_id, cx) else {
            return;
        };
        self.active_pane = pane;
        self.exit_zen_mode_for_unsupported_active_tab(cx);
        self.record_document_context(pane, cx);
        cx.notify();
    }

    pub(super) fn handle_dock_event(
        &mut self,
        event: &DockEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            DockEvent::LayoutChanged => self.layout_changed(cx),
            DockEvent::PanelDrop {
                panel,
                area,
                target,
                ..
            } => self.handle_panel_drop(*panel, *area, *target, window, cx),
            DockEvent::DragDrop { .. } => {}
        }
    }

    /// Keep the workspace's view of the panes in step with the dock.
    fn layout_changed(&mut self, cx: &mut Context<Self>) {
        let panes = self.panes(cx);
        if !panes.iter().any(|pane| pane.id() == self.active_pane)
            && let Some(first) = panes.first()
        {
            self.active_pane = first.id();
        }
        let pane_of_tab = panes
            .iter()
            .flat_map(|pane| {
                pane.tab_ids()
                    .into_iter()
                    .map(move |tab_id| (tab_id, pane.id()))
            })
            .collect::<HashMap<_, _>>();
        self.synchronized_tabs
            .reassign_panes(|tab_id| pane_of_tab.get(&tab_id).copied());
        {
            let mut geometry = self.pane_geometry.borrow_mut();
            geometry
                .panes
                .retain(|pane, _| panes.iter().any(|candidate| candidate.id() == *pane));
            geometry
                .tabs
                .retain(|tab_id, _| pane_of_tab.contains_key(tab_id));
        }
        self.exit_zen_mode_for_unsupported_active_tab(cx);
        self.record_document_context(self.active_pane, cx);
        self.refresh_live_translation_pair_activity(cx);
        self.refresh_synchronized_tab_presentation(cx);
        cx.notify();
    }

    // Placing tabs.

    /// Persist and release the tab `pane` displays, before another takes its
    /// place. Done synchronously, ahead of the switch, so nothing the switch
    /// sets in motion (a linked scroll, say) is undone by it afterwards.
    fn flush_displayed_tab(&self, pane: PaneId, except: EntityId, cx: &mut Context<Self>) {
        if let Some(item) = self.pane(pane, cx).active_tab().cloned()
            && item.entity_id() != except
        {
            crate::tab_item::flush_tab(&item, cx);
        }
    }

    /// Put `item` into `pane` at `ix` (the end when `None`) and show it.
    pub(super) fn insert_tab(
        &mut self,
        pane: PaneId,
        item: &Entity<TabItem>,
        ix: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.flush_displayed_tab(pane, item.entity_id(), cx);
        let view: Arc<dyn PanelView> = Arc::new(item.clone());
        let pane_exists = self.pane(pane, cx).len() > 0
            || self
                .panes(cx)
                .iter()
                .any(|candidate| candidate.id() == pane);
        self.dock_area.update(cx, |area, cx| {
            if pane_exists {
                area.insert_panel_view(
                    view,
                    InsertTarget::Tabs {
                        node: pane.node(),
                        ix,
                        activate: true,
                    },
                    window,
                    cx,
                );
            } else {
                // An empty center has no group to put a tab in yet.
                area.add_panel_view(view, DockPlacement::Center, None, window, cx);
            }
        });
        if let Some(pane) = self.pane_for_tab(item.entity_id(), cx) {
            self.active_pane = pane;
        }
    }

    /// Put `item` in a new pane beside `pane`.
    pub(super) fn split_with_tab(
        &mut self,
        pane: PaneId,
        item: &Entity<TabItem>,
        direction: SplitDirection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<PaneId> {
        let view: Arc<dyn PanelView> = Arc::new(item.clone());
        self.dock_area.update(cx, |area, cx| {
            area.insert_panel_view(
                view,
                InsertTarget::Split {
                    node: pane.node(),
                    placement: direction.placement(),
                    size: None,
                },
                window,
                cx,
            );
        });
        let new_pane = self.pane_for_tab(item.entity_id(), cx)?;
        self.active_pane = new_pane;
        Some(new_pane)
    }

    /// Take `item` out of the dock without closing it.
    fn detach_tab(&mut self, item: &Entity<TabItem>, window: &mut Window, cx: &mut Context<Self>) {
        let item = item.clone();
        self.dock_area.update(cx, |area, cx| {
            area.remove_panel(item, window, cx);
        });
    }

    pub(super) fn activate_tab(
        &mut self,
        pane: PaneId,
        tab_id: EntityId,
        focus: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(item) = self.pane(pane, cx).tab(tab_id).cloned() else {
            return false;
        };
        self.flush_displayed_tab(pane, tab_id, cx);
        self.dock_area.update(cx, |area, cx| {
            area.select_panel(crate::pane::panel_id(tab_id), window, cx);
        });
        if focus {
            crate::tab_item::focus_tab(&item, window, cx);
        }
        cx.notify();
        true
    }

    pub(super) fn focus_pane(&self, pane: PaneId, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(item) = self.pane(pane, cx).active_tab().cloned() {
            crate::tab_item::focus_tab(&item, window, cx);
        } else {
            self.focus_handle.focus(window, cx);
        }
    }

    pub(super) fn new_tab_item(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<TabItem> {
        let item = TabItem::new_tab(self.translations.manager.clone(), window, cx);
        self.adopt_tab(&item, window, cx);
        item
    }

    pub(crate) fn open_new_tab_in_pane(
        &mut self,
        pane: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.leave_home(window, cx);
        let item = self.new_tab_item(window, cx);
        self.insert_tab(pane, &item, None, window, cx);
        crate::tab_item::focus_tab(&item, window, cx);
        cx.notify();
    }

    pub(super) fn open_keymap_tab_in_pane(
        &mut self,
        pane: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let existing = self
            .pane(pane, cx)
            .tabs()
            .iter()
            .find(|tab| tab.read(cx).is_keymap())
            .cloned();
        let item = match existing {
            Some(item) => {
                self.activate_tab(pane, item.entity_id(), false, window, cx);
                item
            }
            None => {
                let item = TabItem::keymap(self.translations.manager.clone(), window, cx);
                self.adopt_tab(&item, window, cx);
                self.insert_tab(pane, &item, None, window, cx);
                item
            }
        };
        crate::tab_item::focus_tab(&item, window, cx);
        cx.notify();
    }

    /// Show the document at `tab_ix`, optionally at `position`, dropping the
    /// pane's displayed new tab when asked to replace it.
    pub(super) fn activate_existing_document(
        &mut self,
        pane: PaneId,
        tab_ix: usize,
        position: Option<DocumentPosition>,
        replace_active_new_tab: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let snapshot = self.pane(pane, cx);
        let Some(item) = snapshot.tabs().get(tab_ix).cloned() else {
            return;
        };
        let replaced =
            (replace_active_new_tab && snapshot.active_tab_is_new_tab(cx) && snapshot.len() > 1)
                .then(|| snapshot.active_tab().cloned())
                .flatten()
                .filter(|replaced| *replaced != item);
        self.activate_tab(pane, item.entity_id(), false, window, cx);
        if let Some(replaced) = replaced {
            self.remove_tab_now(&replaced, window, cx);
        }
        if let Some(position) = position
            && let Some(document_view) = item.read(cx).document_view()
        {
            document_view.update(cx, |document_view, cx| {
                document_view.navigate_to_document_position(position, cx);
            });
        }
        self.active_pane = pane;
        crate::tab_item::focus_tab(&item, window, cx);
        cx.notify();
    }

    /// Open `path` in `pane`, or show it where it already is in that pane.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn open_document_in_pane(
        &mut self,
        pane: PaneId,
        path: PathBuf,
        position: Option<DocumentPosition>,
        replace_active_new_tab: bool,
        job_id: Option<DocumentTranslationId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        let path = normalize_path(&path);
        let snapshot = self.pane(pane, cx);
        if let Some(tab_ix) = snapshot.document_tab_ix(&path, cx) {
            self.activate_existing_document(
                pane,
                tab_ix,
                position,
                replace_active_new_tab,
                window,
                cx,
            );
            if let Some(job_id) = job_id
                && let Some(item) = snapshot.tabs().get(tab_ix)
            {
                item.update(cx, |item, cx| item.associate_open_job(job_id, cx));
            }
            return Ok(());
        }

        let item = TabItem::open_document(
            &path,
            position,
            None,
            job_id,
            self.translations.manager.clone(),
            window,
            cx,
        )?;
        self.adopt_tab(&item, window, cx);
        let replaced = (replace_active_new_tab && snapshot.active_tab_is_new_tab(cx))
            .then(|| snapshot.active_tab().cloned())
            .flatten();
        match replaced {
            Some(replaced) => {
                self.insert_tab(pane, &item, Some(snapshot.active_tab_ix()), window, cx);
                self.remove_tab_now(&replaced, window, cx);
            }
            None => self.insert_tab(pane, &item, None, window, cx),
        }
        crate::tab_item::focus_tab(&item, window, cx);
        cx.notify();
        Ok(())
    }

    /// Remove a tab from the dock and close it, with no further bookkeeping.
    fn remove_tab_now(
        &mut self,
        item: &Entity<TabItem>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.detach_tab(item, window, cx);
        item.update(cx, |item, cx| item.close(cx));
        self.release_tab(item.entity_id());
    }

    pub(super) fn duplicate_tab_in_pane(
        &mut self,
        pane: PaneId,
        tab_ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_title_bar_menu_expanded(false, cx);
        let Some(source) = self.pane(pane, cx).document_view_at(tab_ix, cx) else {
            return;
        };
        match TabItem::duplicate_document(
            &source,
            None,
            self.translations.manager.clone(),
            window,
            cx,
        ) {
            Ok(item) => {
                self.adopt_tab(&item, window, cx);
                self.insert_tab(pane, &item, Some(tab_ix + 1), window, cx);
                crate::tab_item::focus_tab(&item, window, cx);
                self.refresh_synchronized_tab_presentation(cx);
                cx.notify();
            }
            Err(error) => eprintln!("failed to duplicate document tab: {error:#}"),
        }
    }

    pub(crate) fn close_tab_by_id(
        &mut self,
        tab_id: EntityId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(pane) = self.pane_for_tab(tab_id, cx) else {
            return;
        };
        let tab_ix = self.pane(pane, cx).tab_ix_for_entity_id(tab_id);
        if tab_ix.is_some() {
            self.close_tab_in_pane(pane, tab_ix, window, cx);
        }
    }

    pub(super) fn close_tab_in_pane_now(
        &mut self,
        pane: PaneId,
        tab_ix: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let snapshot = self.pane(pane, cx);
        let tab_ix = tab_ix.unwrap_or(snapshot.active_tab_ix());
        let Some(item) = snapshot.tabs().get(tab_ix).cloned() else {
            return;
        };
        let tab_id = item.entity_id();
        let pane_was_active = self.active_pane == pane;
        self.unregister_live_translation_pairs_for_tab(tab_id, cx);
        self.synchronized_tabs.remove_tab(tab_id);
        if self
            .synchronized_connection
            .as_ref()
            .is_some_and(|connection| connection.source_tab == tab_id)
        {
            self.synchronized_connection = None;
        }

        let panes = self.panes(cx);
        if snapshot.len() <= 1 {
            if panes.len() <= 1 {
                // The last tab of the last pane leaves a new tab behind, and
                // the window shows Home.
                let new_tab = self.new_tab_item(window, cx);
                self.insert_tab(pane, &new_tab, Some(tab_ix), window, cx);
                self.remove_tab_now(&item, window, cx);
                self.active_pane = self.pane_for_tab(new_tab.entity_id(), cx).unwrap_or(pane);
                crate::tab_item::focus_tab(&new_tab, window, cx);
                self.enter_home(window, cx);
            } else {
                self.remove_pane(pane, window, cx);
                return;
            }
        } else {
            self.remove_tab_now(&item, window, cx);
            if pane_was_active {
                self.focus_pane(pane, window, cx);
            }
        }
        self.exit_zen_mode_for_unsupported_active_tab(cx);
        self.prune_document_context(cx);
        self.refresh_synchronized_tab_presentation(cx);
        cx.notify();
    }

    /// Close a whole pane and every tab in it.
    pub(super) fn remove_pane(
        &mut self,
        pane: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let panes = self.panes(cx);
        let Some(remove_ix) = panes.iter().position(|candidate| candidate.id() == pane) else {
            return;
        };
        let pane_was_active = self.active_pane == pane;
        let snapshot = panes[remove_ix].clone();
        for tab_id in snapshot.tab_ids() {
            self.unregister_live_translation_pairs_for_tab(tab_id, cx);
            self.synchronized_tabs.remove_tab(tab_id);
            if self
                .synchronized_connection
                .as_ref()
                .is_some_and(|connection| connection.source_tab == tab_id)
            {
                self.synchronized_connection = None;
            }
        }
        self.cancel_pending_translation_splits_for_pane(pane, cx);
        for item in snapshot.tabs() {
            crate::tab_item::flush_tab(item, cx);
            self.detach_tab(item, window, cx);
            self.release_tab(item.entity_id());
        }

        let remaining = self.panes(cx);
        if remaining.is_empty() {
            window.remove_window();
            return;
        }
        if pane_was_active {
            let next = remaining[remove_ix.min(remaining.len() - 1)].id();
            self.active_pane = next;
            self.focus_pane(next, window, cx);
        }
        self.exit_zen_mode_for_unsupported_active_tab(cx);
        self.prune_document_context(cx);
        self.refresh_synchronized_tab_presentation(cx);
        cx.notify();
    }

    // Pane interactions from the dock skin.

    pub(crate) fn click_tab(
        &mut self,
        pane: PaneId,
        tab_id: EntityId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let was_active = self.pane(pane, cx).active_tab_id() == Some(tab_id);
        self.activate_tab(pane, tab_id, true, window, cx);
        self.active_pane = pane;
        self.record_document_context(pane, cx);
        self.handle_synchronized_tab_click(pane, tab_id, was_active, window, cx);
        cx.notify();
    }

    pub(crate) fn activate_pane_from_click(
        &mut self,
        pane: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(item) = self.pane(pane, cx).active_tab().cloned() {
            let focus_handle = item.read(cx).focus_handle(cx);
            if !focus_handle.contains_focused(window, cx) {
                crate::tab_item::focus_tab(&item, window, cx);
            }
        }
        self.active_pane = pane;
        self.exit_zen_mode_for_unsupported_active_tab(cx);
        self.record_document_context(pane, cx);
        cx.notify();
    }

    pub(crate) fn track_pane_external_drag(
        &mut self,
        pane: PaneId,
        event: &DragMoveEvent<ExternalPaths>,
        cx: &mut Context<Self>,
    ) {
        let is_inside = event.bounds.contains(&event.event.position);
        let feedback = is_inside
            .then(|| classify_external_document_paths(event.drag(cx).paths()).feedback())
            .flatten()
            .map(|feedback| (pane, feedback));
        let current = self.pane_external_drop;
        if (is_inside || current.is_some_and(|(target, _)| target == pane)) && current != feedback {
            self.pane_external_drop = feedback;
            cx.notify();
        }
        if is_inside {
            cx.stop_propagation();
        }
    }

    pub(crate) fn drop_external_paths_on_pane(
        &mut self,
        pane: PaneId,
        paths: &[PathBuf],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.pane_external_drop = None;
        self.open_external_document_paths_in_pane(paths, pane, window, cx);
    }

    pub(crate) fn open_tab_context_menu(
        &mut self,
        pane: PaneId,
        tab_id: EntityId,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.synchronized_presentation.connection_tabs.is_empty() {
            self.finish_synchronized_connection(cx);
            window.prevent_default();
            cx.stop_propagation();
            return;
        }
        let Some(item) = self.pane(pane, cx).tab(tab_id).cloned() else {
            return;
        };
        let is_document = item.read(cx).is_document();
        let saved_password_path = item.read(cx).saved_password_path(cx);
        let group = self
            .synchronized_presentation
            .group_by_tab
            .get(&tab_id)
            .cloned();
        let is_paused = self.synchronized_presentation.paused_tabs.contains(&tab_id);
        let pause_locked = self
            .synchronized_presentation
            .pause_locked_tabs
            .contains(&tab_id);
        let workspace = cx.entity().downgrade();
        let popup = PopupMenu::build(window, cx, move |mut menu, _, _| {
            if is_document {
                let duplicate = workspace.clone();
                menu = menu.item(
                    PopupMenuItem::new(crate::fl!("toolbar-duplicate-tab"))
                        .icon(IconName::Copy)
                        .on_click(move |_, window, cx| {
                            _ = duplicate.update(cx, |workspace, cx| {
                                if let Some(pane) = workspace.pane_for_tab(tab_id, cx)
                                    && let Some(tab_ix) =
                                        workspace.pane(pane, cx).tab_ix_for_entity_id(tab_id)
                                {
                                    workspace.duplicate_tab_in_pane(pane, tab_ix, window, cx);
                                }
                            });
                        }),
                );
                if let Some(path) = saved_password_path.clone() {
                    let item = item.clone();
                    menu = menu.item(
                        PopupMenuItem::new(crate::fl!("pdf-password-forget"))
                            .icon(IconName::Delete)
                            .on_click(move |_, window, cx| {
                                item.update(cx, |item, cx| {
                                    item.forget_saved_document_password(path.clone(), window, cx);
                                });
                            }),
                    );
                }
                menu = menu.separator();
                if group.is_some() {
                    let connect = workspace.clone();
                    menu = menu.item(
                        PopupMenuItem::new(crate::fl!("synchronized-tabs-connect-more"))
                            .icon(IconName::Link2)
                            .on_click(move |_, _, cx| {
                                _ = connect.update(cx, |workspace, cx| {
                                    if let Some(group_id) =
                                        workspace.synchronized_tabs.group_for_tab(tab_id)
                                    {
                                        workspace.start_connecting_synchronized_group(
                                            tab_id, group_id, cx,
                                        );
                                    }
                                });
                            }),
                    );

                    if !pause_locked {
                        let pause = workspace.clone();
                        let (pause_label, pause_icon, paused) = if is_paused {
                            (
                                crate::fl!("synchronized-tabs-resume-scroll"),
                                IconName::Play,
                                false,
                            )
                        } else {
                            (
                                crate::fl!("synchronized-tabs-pause-scroll"),
                                IconName::Pause,
                                true,
                            )
                        };
                        menu =
                            menu.item(PopupMenuItem::new(pause_label).icon(pause_icon).on_click(
                                move |_, _, cx| {
                                    _ = pause.update(cx, |workspace, cx| {
                                        workspace
                                            .set_synchronized_scroll_paused(tab_id, paused, cx);
                                    });
                                },
                            ));
                    }

                    let leave = workspace.clone();
                    menu = menu.separator().item(
                        PopupMenuItem::new(crate::fl!("synchronized-tabs-leave"))
                            .icon(IconName::Unlink2)
                            .on_click(move |_, _, cx| {
                                _ = leave.update(cx, |workspace, cx| {
                                    workspace.synchronized_tabs.leave_group(tab_id);
                                    workspace.refresh_synchronized_tab_presentation(cx);
                                });
                            }),
                    );
                } else {
                    let create = workspace.clone();
                    menu = menu.item(
                        PopupMenuItem::new(crate::fl!("synchronized-tabs-create"))
                            .icon(IconName::Link2)
                            .on_click(move |_, _, cx| {
                                _ = create.update(cx, |workspace, cx| {
                                    workspace.start_new_synchronized_group(tab_id, cx);
                                });
                            }),
                    );
                }
                menu = menu.separator();
            }

            let close = workspace.clone();
            menu.item(
                PopupMenuItem::new(crate::fl!("toolbar-close-tab"))
                    .icon(IconName::X)
                    .on_click(move |_, window, cx| {
                        _ = close.update(cx, |workspace, cx| {
                            workspace.close_tab_by_id(tab_id, window, cx);
                        });
                    }),
            )
        });
        cx.subscribe_in(&popup, window, |this, popup, _: &DismissEvent, _, cx| {
            if this
                .tab_context_menu
                .as_ref()
                .is_some_and(|menu| menu.popup == *popup)
            {
                this.tab_context_menu = None;
                cx.notify();
            }
        })
        .detach();
        popup.focus_handle(cx).focus(window, cx);
        self.tab_context_menu = Some(TabContextMenu { position, popup });
        window.prevent_default();
        cx.stop_propagation();
        cx.notify();
    }

    /// Pause or resume a tab's synchronized scrolling. A live translation
    /// pair always scrolls together.
    pub(super) fn set_synchronized_scroll_paused(
        &mut self,
        tab_id: EntityId,
        paused: bool,
        cx: &mut Context<Self>,
    ) {
        if !self.is_live_translation_pair_tab(tab_id)
            && self.synchronized_tabs.set_paused(tab_id, paused)
        {
            self.refresh_synchronized_tab_presentation(cx);
        }
    }

    pub(super) fn render_tab_context_menu(&self) -> Option<impl IntoElement> {
        let menu = self.tab_context_menu.as_ref()?;
        Some(
            deferred(
                anchored()
                    .snap_to_window_with_margin(px(8.0))
                    .position(menu.position)
                    .anchor(Anchor::TopLeft)
                    .child(menu.popup.clone()),
            )
            .with_priority(20),
        )
    }

    // Drag and drop.

    pub(super) fn handle_panel_drop(
        &mut self,
        panel: PanelId,
        area: Option<EntityId>,
        target: InsertTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.dragged_tab_outside_window = None;
        let tab_id = EntityId::from(panel.as_u64());
        if area.is_none_or(|area| area == self.dock_area.entity_id()) {
            self.move_tab_within_window(tab_id, target, window, cx);
        } else if let Some(area) = area {
            self.move_tab_from_other_window(tab_id, area, target, window, cx);
        }
    }

    fn move_tab_within_window(
        &mut self,
        tab_id: EntityId,
        target: InsertTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(item) = self.tab_item(tab_id, cx) else {
            return;
        };
        if let InsertTarget::Tabs { node, activate, .. } = target {
            let destination = PaneId::from(node);
            if self.pane_for_tab(tab_id, cx) != Some(destination)
                && !self.synchronized_tabs.can_move_tab(tab_id, destination)
            {
                return;
            }
            if activate {
                self.flush_displayed_tab(destination, tab_id, cx);
            }
        } else {
            self.exit_zen_mode(window, cx);
        }
        self.dock_area.update(cx, |area, cx| {
            area.move_panel(crate::pane::panel_id(tab_id), target, window, cx);
        });
        if let Some(pane) = self.pane_for_tab(tab_id, cx) {
            self.active_pane = pane;
        }
        crate::tab_item::focus_tab(&item, window, cx);
        self.refresh_synchronized_tab_presentation(cx);
        cx.notify();
    }

    fn move_tab_from_other_window(
        &mut self,
        tab_id: EntityId,
        source_area: EntityId,
        target: InsertTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(source_window) = cx
            .try_global::<WorkspaceDockAreas>()
            .and_then(|areas| areas.0.get(&source_area).copied())
            .and_then(|window| WorkspaceWindowHandle::from_any(window, cx))
        else {
            eprintln!("failed to move tab: source workspace is unavailable");
            return;
        };
        let taken = source_window.update(cx, |source, source_window, cx| {
            source.take_tab_for_transfer(tab_id, source_window, cx)
        });
        let item = match taken {
            Ok(Some(item)) => item,
            Ok(None) => return,
            Err(error) => {
                eprintln!("failed to move tab from source window: {error:#}");
                return;
            }
        };
        self.accept_transferred_tab(&item, Some(target), window, cx);
    }

    /// Take a tab out of this workspace so another window can adopt it.
    pub(super) fn take_tab_for_transfer(
        &mut self,
        tab_id: EntityId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<TabItem>> {
        let item = self.tab_item(tab_id, cx)?;
        if !item.read(cx).is_draggable(cx) {
            return None;
        }
        crate::tab_item::flush_tab(&item, cx);
        self.synchronized_tabs.remove_tab(tab_id);
        if self
            .synchronized_connection
            .as_ref()
            .is_some_and(|connection| connection.source_tab == tab_id)
        {
            self.synchronized_connection = None;
        }
        self.detach_tab(&item, window, cx);
        self.release_tab(tab_id);
        if self.panes(cx).is_empty() {
            window.remove_window();
        } else {
            self.prune_document_context(cx);
            self.refresh_synchronized_tab_presentation(cx);
            cx.notify();
        }
        Some(item)
    }

    /// Adopt a tab from another window, at `target` or in the active pane.
    pub(super) fn accept_transferred_tab(
        &mut self,
        item: &Entity<TabItem>,
        target: Option<InsertTarget>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let jobs = self.translations.manager.clone();
        item.update(cx, |item, cx| item.attach_to_window(jobs, window, cx));
        self.adopt_tab(item, window, cx);
        match target {
            Some(target @ InsertTarget::Split { .. }) => {
                self.exit_zen_mode(window, cx);
                let view: Arc<dyn PanelView> = Arc::new(item.clone());
                self.dock_area.update(cx, |area, cx| {
                    area.insert_panel_view(view, target, window, cx);
                });
            }
            Some(InsertTarget::Tabs { node, ix, .. }) => {
                self.insert_tab(PaneId::from(node), item, ix, window, cx);
            }
            None => {
                let pane = self.active_pane;
                self.insert_tab(pane, item, None, window, cx);
            }
        }
        if let Some(pane) = self.pane_for_tab(item.entity_id(), cx) {
            self.active_pane = pane;
        }
        self.leave_home(window, cx);
        crate::tab_item::focus_tab(item, window, cx);
        self.refresh_synchronized_tab_presentation(cx);
        cx.notify();
    }

    pub(super) fn track_dragged_tab_outside_window(
        &mut self,
        event: &DragMoveEvent<DragPanel>,
        cx: &mut Context<Self>,
    ) {
        let drag = event.drag(cx).clone();
        if drag.area() != Some(self.dock_area.entity_id())
            || self
                .tab_item(EntityId::from(drag.panel().as_u64()), cx)
                .is_none()
        {
            self.dragged_tab_outside_window = None;
            return;
        }
        self.dragged_tab_outside_window =
            (!event.bounds.contains(&event.event.position)).then_some(drag);
    }

    pub(super) fn clear_dragged_tab_outside_window(&mut self) {
        self.dragged_tab_outside_window = None;
    }

    /// A tab dropped outside every window becomes a window of its own.
    pub(super) fn detach_dragged_tab(
        &mut self,
        event: &MouseUpEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(drag) = self.dragged_tab_outside_window.take() else {
            return;
        };
        let tab_id = EntityId::from(drag.panel().as_u64());
        if self.tab_item(tab_id, cx).is_none() {
            return;
        }

        let bounds = Self::detached_window_bounds(event, window);
        let destination =
            match (self.callbacks.open_workspace_window)(WorkspaceWindowRequest { bounds }, cx) {
                Ok(window) => window,
                Err(error) => {
                    eprintln!("failed to tear tab into new window: {error:#}");
                    return;
                }
            };
        let Some(item) = self.take_tab_for_transfer(tab_id, window, cx) else {
            return;
        };
        let inserted = destination.update(cx, |destination, destination_window, cx| {
            destination.accept_transferred_tab(&item, None, destination_window, cx);
        });
        if let Err(error) = inserted {
            eprintln!("failed to insert detached tab: {error:#}");
        }
        cx.stop_propagation();
    }

    fn detached_window_bounds(event: &MouseUpEvent, window: &Window) -> gpui::Bounds<Pixels> {
        let source_bounds = window.bounds();
        let origin = point(
            source_bounds.origin.x + event.position.x - px(160.0),
            source_bounds.origin.y + event.position.y - px(16.0),
        );

        gpui::Bounds::new(origin, source_bounds.size)
    }

    // Rearranging panes.

    /// Rebuild the center from an edited copy of its layout, for the
    /// rearrangements the dock has no edit of its own for.
    fn rearrange_panes(
        &mut self,
        edit: impl FnOnce(&mut PaneLayout) -> bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(mut layout) = PaneLayout::of(&self.dock_area, cx) else {
            return false;
        };
        if !edit(&mut layout) {
            return false;
        }
        let active_tab = self.active_pane(cx).active_tab_id();
        let dock_layout = layout.into_dock_layout(cx);
        self.dock_area.update(cx, |area, cx| {
            area.set_center(dock_layout, window, cx);
        });
        if let Some(pane) = active_tab.and_then(|tab_id| self.pane_for_tab(tab_id, cx)) {
            self.active_pane = pane;
        }
        true
    }

    pub(super) fn swap_pane_in_direction(
        &mut self,
        direction: SplitDirection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.leave_zen_mode(cx);
        if self.surface_mode != WorkspaceSurfaceMode::Panes {
            return;
        }
        let Some(target) = self.find_pane_in_direction(self.active_pane, direction, cx) else {
            return;
        };
        let (Some(active_tab), Some(target_tab)) = (
            self.active_pane(cx).active_tab_id(),
            self.pane(target, cx).active_tab_id(),
        ) else {
            return;
        };
        if self.rearrange_panes(
            |layout| layout.swap_panes(active_tab, target_tab),
            window,
            cx,
        ) {
            cx.notify();
        }
    }

    pub(super) fn move_pane_to_border(
        &mut self,
        direction: SplitDirection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.leave_zen_mode(cx);
        if self.surface_mode != WorkspaceSurfaceMode::Panes {
            return;
        }
        let Some(active_tab) = self.active_pane(cx).active_tab_id() else {
            return;
        };
        if self.rearrange_panes(
            |layout| layout.move_pane_to_border(active_tab, direction),
            window,
            cx,
        ) {
            cx.notify();
        }
    }

    /// The path of splits from the center's root down to `pane`, each with
    /// the index of the child the path continues through.
    fn split_path(&self, pane: PaneId, cx: &App) -> Vec<(NodeId, Axis, usize)> {
        fn walk(node: &PaneNode, target: NodeId, path: &mut Vec<(NodeId, Axis, usize)>) -> bool {
            if node.id() == target {
                return true;
            }
            let PaneRef::Split { axis, children, .. } = node.kind() else {
                return false;
            };
            for (ix, child) in children.iter().enumerate() {
                path.push((node.id(), axis, ix));
                if walk(child, target, path) {
                    return true;
                }
                path.pop();
            }
            false
        }
        let mut path = Vec::new();
        if let Some(tree) = self.dock_area.read(cx).layout(DockPlacement::Center) {
            walk(tree.root(), pane.node(), &mut path);
        }
        path
    }

    pub(super) fn resize_active_pane(
        &mut self,
        direction: SplitDirection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.exit_zen_mode(window, cx);
        if self.surface_mode != WorkspaceSurfaceMode::Panes {
            return;
        }
        let Some((split, _, ix)) = self
            .split_path(self.active_pane, cx)
            .into_iter()
            .rev()
            .find(|(_, axis, _)| *axis == direction.axis())
        else {
            return;
        };
        let Some(mut sizes) = self.dock_area.read(cx).split_sizes(split, cx) else {
            return;
        };
        if sizes.len() < 2 {
            return;
        }
        let amount = if direction.increasing() {
            px(RESIZE_STEP)
        } else {
            px(-RESIZE_STEP)
        };
        let neighbour = if ix + 1 < sizes.len() { ix + 1 } else { ix - 1 };
        let min = px(PANE_MIN_SIZE);
        let grown = (sizes[ix] + amount)
            .max(min)
            .min(sizes[ix] + sizes[neighbour] - min);
        let delta = grown - sizes[ix];
        sizes[ix] += delta;
        sizes[neighbour] -= delta;
        self.dock_area.update(cx, |area, cx| {
            area.set_split_sizes(split, sizes, window, cx);
        });
        cx.notify();
    }

    pub(super) fn reset_pane_sizes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.leave_zen_mode(cx);
        if self.surface_mode != WorkspaceSurfaceMode::Panes {
            return;
        }
        fn splits(node: &PaneNode, out: &mut Vec<NodeId>) {
            if let PaneRef::Split { children, .. } = node.kind() {
                out.push(node.id());
                for child in children {
                    splits(child, out);
                }
            }
        }
        let mut nodes = Vec::new();
        if let Some(tree) = self.dock_area.read(cx).layout(DockPlacement::Center) {
            splits(tree.root(), &mut nodes);
        }
        for node in nodes {
            let Some(sizes) = self.dock_area.read(cx).split_sizes(node, cx) else {
                continue;
            };
            let total = sizes.iter().map(|size| size.as_f32()).sum::<f32>();
            let equal = vec![px(total / sizes.len().max(1) as f32); sizes.len()];
            self.dock_area.update(cx, |area, cx| {
                area.set_split_sizes(node, equal, window, cx);
            });
        }
        cx.notify();
    }
}
