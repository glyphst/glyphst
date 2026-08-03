use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use anyhow::Result;
use glyphst_translation_ui::{
    DocumentTranslationJobEvent, DocumentTranslationJobId, DocumentTranslationJobManager,
    DocumentTranslationNotification, document_translation_notification,
};
use glyphst_ui::{
    activity::ActivityCenter,
    root::render_overlays,
    style::{apply_window_background, shell_background},
};
use gpui::{
    AnyElement, App, Bounds, Context, DismissEvent, DragMoveEvent, Entity, EntityId, FocusHandle,
    Focusable, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, PathPromptOptions, Pixels, Point, Render, SharedString, Window, WindowAppearance,
    bounds, canvas, div, fill, point, prelude::*, px, size,
};
use gpui_component::{ActiveTheme as _, ThemeColor, WindowExt as _, menu::PopupMenu};

mod chrome;
mod docks;

pub use glyphst_actions::workspace::{
    ActivateNextPane, ActivatePaneDown, ActivatePaneLeft, ActivatePaneRight, ActivatePaneUp,
    ActivatePreviousPane, CloseOtherPanes, MovePaneDown, MovePaneLeft, MovePaneRight, MovePaneUp,
    ResetPaneSizes, ResizePaneDown, ResizePaneLeft, ResizePaneRight, ResizePaneUp, SwapPaneDown,
    SwapPaneLeft, SwapPaneRight, SwapPaneUp, ToggleMode, ToggleThemeSelector,
};

use crate::{
    command_palette::{CommandPalette, CommandPaletteDelegate},
    document_browser::{DocumentBrowser, DocumentBrowserEvent},
    document_view::{
        CloseTab, DocumentSidebar, DocumentView, DocumentViewEvent, DuplicateTab, OpenFile,
        OpenKeybindings, OpenNewTab, OpenSettings, OpenWithSystemPicker, ToggleAiPanel,
        ToggleCommandPalette, ToggleNotes, ToggleThumbnailSidebar, ToggleToc, ToggleTransparency,
    },
    library::{DocumentPosition, normalize_path},
    pane::{
        DraggedTab, Pane, PaneDropTarget, PaneEvent, PaneGroup, PaneSynchronizedPresentation,
        RecentlyAddedSynchronizedTab, SplitDirection, SplitDown, SplitHorizontal, SplitLeft,
        SplitRight, SplitUp, SplitVertical, SynchronizedConnectionInvalidReason,
        SynchronizedConnectionTabState, TabTransfer,
    },
    picker::{Picker, PickerEvent},
    settings::{SettingsStore, WindowSettings, setup_ui_font, sync_component_theme},
    synchronized_tabs::{
        ReservedSynchronizedGroup, SynchronizedConnectionNode, SynchronizedGroupId,
        SynchronizedTabRegistry, closest_synchronized_tab, synchronized_connection_tree,
    },
    theme::{ThemeAppearance, ThemeAppearanceMode},
    theme_selector::{ThemeSelector, ThemeSelectorDelegate},
};

use chrome::{WorkspaceMenu, WorkspaceTitleBar, build_popup_menu};
use docks::WorkspaceDockState;

pub struct Workspace {
    pane_group: PaneGroup,
    active_pane: Entity<Pane>,
    callbacks: WorkspaceCallbacks,
    focus_handle: FocusHandle,
    document_translation_jobs: Entity<DocumentTranslationJobManager>,
    pending_translation_splits: Vec<PendingTranslationSplit>,
    command_palette: Option<Entity<CommandPalette>>,
    command_palette_previous_focus_handle: Option<FocusHandle>,
    document_browser: Option<Entity<DocumentBrowser>>,
    document_browser_previous_focus_handle: Option<FocusHandle>,
    theme_selector: Option<Entity<ThemeSelector>>,
    theme_selector_previous_focus_handle: Option<FocusHandle>,
    title_bar_menu_expanded: bool,
    title_bar_open_menu: Option<WorkspaceMenu>,
    title_bar_popup_menu: Option<Entity<PopupMenu>>,
    dragged_tab_outside_window: Option<DraggedTab>,
    synchronized_tabs: SynchronizedTabRegistry,
    synchronized_connection: Option<SynchronizedConnection>,
    fading_synchronized_connection: Option<FadingSynchronizedConnection>,
    docks: WorkspaceDockState,
    transparency_enabled: bool,
    window_settings: WindowSettings,
}

#[derive(Clone, Copy)]
enum SynchronizedConnectionGroup {
    New(ReservedSynchronizedGroup),
    Existing(SynchronizedGroupId),
}

struct SynchronizedConnection {
    source_tab: EntityId,
    source_pane: Entity<Pane>,
    group: SynchronizedConnectionGroup,
    cursor: Point<Pixels>,
    animation_started_at: Instant,
    edges: Vec<SynchronizedConnectionEdge>,
    recently_added: Option<RecentlyAddedSynchronizedTab>,
    addition_sequence: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SynchronizedConnectionEdge {
    from: EntityId,
    to: EntityId,
    added_at: Instant,
}

struct FadingSynchronizedConnection {
    color_slot: usize,
    edges: Vec<SynchronizedConnectionEdge>,
    animation_started_at: Instant,
    started_at: Instant,
}

#[derive(Clone, Copy)]
struct SynchronizedConnectionAnchor {
    position: Point<Pixels>,
    edge: SynchronizedConnectionAnchorEdge,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SynchronizedConnectionAnchorEdge {
    Top,
    Right,
    Bottom,
    Left,
}

#[derive(Clone, Copy)]
struct SynchronizedConnectionCurve {
    start: SynchronizedConnectionAnchor,
    control_a: Point<Pixels>,
    control_b: Point<Pixels>,
    end: SynchronizedConnectionAnchor,
    added_at: Instant,
}

#[derive(Clone, Copy)]
struct SynchronizedConnectionHandle {
    center: Point<Pixels>,
    added_at: Instant,
}

const SYNCHRONIZED_DOT_PERIOD: Duration = Duration::from_millis(1400);
const SYNCHRONIZED_EDGE_FADE_IN: Duration = Duration::from_millis(180);
const SYNCHRONIZED_NETWORK_FADE_OUT: Duration = Duration::from_millis(160);
const SYNCHRONIZED_HANDLE_DIAMETER: f32 = 5.5;
const SYNCHRONIZED_HANDLE_OUTLINE_DIAMETER: f32 = 9.0;

pub type WorkspaceWindowHandle = glyphst_ui::root::RootWindowHandle<Workspace>;

struct PendingTranslationSplit {
    job_id: DocumentTranslationJobId,
    stage: PendingTranslationSplitStage,
}

enum PendingTranslationSplitStage {
    OpeningSource {
        source_path: PathBuf,
        translated_path: PathBuf,
        source_pane: Entity<Pane>,
    },
    OpeningTranslated {
        original: Entity<DocumentView>,
        translated_pane: Entity<Pane>,
    },
}

#[derive(Clone, Copy)]
pub struct WorkspaceWindowRequest {
    pub bounds: gpui::Bounds<Pixels>,
}

#[derive(Clone, Copy)]
pub struct WorkspaceCallbacks {
    pub open_workspace_window:
        fn(WorkspaceWindowRequest, &mut App) -> Result<WorkspaceWindowHandle>,
    pub open_settings_window: fn(&mut App),
}

#[derive(Clone)]
struct TabDestination {
    pane: Entity<Pane>,
    target: PaneDropTarget,
    index: usize,
}

impl Workspace {
    pub fn new(
        initial_path: Option<PathBuf>,
        initial_position: Option<DocumentPosition>,
        callbacks: WorkspaceCallbacks,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let document_translation_jobs = cx.new(DocumentTranslationJobManager::new);
        let jobs = document_translation_jobs.clone();
        let pane = cx.new(|cx| Pane::empty(cx.focus_handle(), jobs));
        let mut workspace = Self::with_pane(
            pane.clone(),
            document_translation_jobs,
            callbacks,
            window,
            cx,
        );
        match initial_path {
            Some(path) => {
                if let Err(error) =
                    workspace.focus_or_open_document(path, initial_position, true, window, cx)
                {
                    eprintln!("failed to open initial document: {error:#}");
                    pane.update(cx, |pane, cx| pane.ensure_new_tab(window, cx));
                }
            }
            None => pane.update(cx, |pane, cx| pane.ensure_new_tab(window, cx)),
        }
        workspace
    }

    pub fn new_empty(
        callbacks: WorkspaceCallbacks,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let document_translation_jobs = cx.new(DocumentTranslationJobManager::new);
        let jobs = document_translation_jobs.clone();
        let pane = cx.new(|cx| Pane::empty(cx.focus_handle(), jobs));
        Self::with_pane(pane, document_translation_jobs, callbacks, window, cx)
    }

    fn with_pane(
        pane: Entity<Pane>,
        document_translation_jobs: Entity<DocumentTranslationJobManager>,
        callbacks: WorkspaceCallbacks,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        let docks = WorkspaceDockState::new(cx);
        let window_settings = cx.global::<SettingsStore>().settings.window;
        let transparency_enabled = window_settings.transparency_enabled_by_default();
        apply_window_background(window, window_settings, transparency_enabled);
        let workspace = Self {
            pane_group: PaneGroup::new(pane.clone()),
            active_pane: pane.clone(),
            callbacks,
            focus_handle,
            document_translation_jobs: document_translation_jobs.clone(),
            pending_translation_splits: Vec::new(),
            command_palette: None,
            command_palette_previous_focus_handle: None,
            document_browser: None,
            document_browser_previous_focus_handle: None,
            theme_selector: None,
            theme_selector_previous_focus_handle: None,
            title_bar_menu_expanded: false,
            title_bar_open_menu: None,
            title_bar_popup_menu: None,
            dragged_tab_outside_window: None,
            synchronized_tabs: SynchronizedTabRegistry::default(),
            synchronized_connection: None,
            fading_synchronized_connection: None,
            docks,
            transparency_enabled,
            window_settings,
        };

        workspace.subscribe_to_pane(&pane, window, cx);
        cx.observe(&document_translation_jobs, |_, _, cx| cx.notify())
            .detach();
        if let Some(activity_center) = ActivityCenter::try_global(cx) {
            cx.observe(&activity_center, |_, _, cx| cx.notify())
                .detach();
        }
        cx.subscribe_in(
            &document_translation_jobs,
            window,
            |this, _, event: &DocumentTranslationJobEvent, window, cx| match event {
                DocumentTranslationJobEvent::OpenDocument { id, path } => {
                    let replace_active_new_tab = this.active_pane.read(cx).active_tab_is_new_tab();
                    if let Err(error) = this.focus_or_open_document_for_job(
                        path.clone(),
                        None,
                        replace_active_new_tab,
                        *id,
                        window,
                        cx,
                    ) {
                        this.document_translation_jobs.update(cx, |jobs, cx| {
                            jobs.finish_open(*id, Err(format!("{error:#}").into()), cx);
                        });
                    }
                }
                DocumentTranslationJobEvent::OpenSplit {
                    id,
                    source_path,
                    translated_path,
                } => {
                    if let Err(error) = this.begin_translation_split_open(
                        *id,
                        source_path.clone(),
                        translated_path.clone(),
                        window,
                        cx,
                    ) {
                        this.document_translation_jobs.update(cx, |jobs, cx| {
                            jobs.finish_open(*id, Err(format!("{error:#}").into()), cx);
                        });
                    }
                }
                DocumentTranslationJobEvent::ShowNotification { snapshot } => {
                    window.push_notification(
                        document_translation_notification(
                            snapshot.clone(),
                            this.document_translation_jobs.downgrade(),
                        ),
                        cx,
                    );
                }
                DocumentTranslationJobEvent::RemoveNotification { id } => {
                    window.remove_notification1::<DocumentTranslationNotification>(
                        id.to_string(),
                        cx,
                    );
                }
            },
        )
        .detach();
        workspace.focus_active_pane(window, cx);
        cx.observe_global::<SettingsStore>(|this, cx| {
            this.refresh_synchronized_tab_presentation(cx);
            cx.notify();
        })
        .detach();

        let system_appearance = match window.appearance() {
            WindowAppearance::Light | WindowAppearance::VibrantLight => ThemeAppearance::Light,
            WindowAppearance::Dark | WindowAppearance::VibrantDark => ThemeAppearance::Dark,
        };
        cx.global_mut::<SettingsStore>()
            .set_system_appearance(system_appearance);
        sync_component_theme(cx);
        cx.observe_window_appearance(window, |_, window, cx| {
            let appearance = match window.appearance() {
                WindowAppearance::Light | WindowAppearance::VibrantLight => ThemeAppearance::Light,
                WindowAppearance::Dark | WindowAppearance::VibrantDark => ThemeAppearance::Dark,
            };
            cx.global_mut::<SettingsStore>()
                .set_system_appearance(appearance);
            sync_component_theme(cx);
        })
        .detach();
        cx.on_release(|this, cx| {
            for pane in this.pane_group.panes() {
                pane.update(cx, |pane, cx| pane.flush_all_tabs(cx));
            }
        })
        .detach();

        workspace
    }

    pub fn open_document_external(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        let replace_active_new_tab = self.active_pane.read(cx).active_tab_is_new_tab();
        self.focus_or_open_document(path, None, replace_active_new_tab, window, cx)
    }

    fn open_settings_window(&mut self, cx: &mut Context<Self>) {
        self.set_title_bar_menu_expanded(false, cx);
        (self.callbacks.open_settings_window)(cx);
    }

    fn subscribe_to_pane(&self, pane: &Entity<Pane>, window: &mut Window, cx: &mut Context<Self>) {
        let pane_for_events = pane.clone();
        cx.subscribe_in(
            pane,
            window,
            move |this, _, event: &PaneEvent, window, cx| match event {
                PaneEvent::Activate => {
                    if this.active_pane != pane_for_events {
                        this.active_pane = pane_for_events.clone();
                        cx.notify();
                    }
                }
                PaneEvent::OpenFile => {
                    this.active_pane = pane_for_events.clone();
                    this.open_file(window, cx);
                }
                PaneEvent::OpenDocument {
                    path,
                    position,
                    replace_active_new_tab,
                } => {
                    this.active_pane = pane_for_events.clone();
                    if let Err(error) = this.focus_or_open_document(
                        path.clone(),
                        position.clone(),
                        *replace_active_new_tab,
                        window,
                        cx,
                    ) {
                        eprintln!("failed to open document: {error:#}");
                    }
                }
                PaneEvent::DocumentOpenFinished { job_id, result } => {
                    if !this.continue_translation_split_open(*job_id, result.clone(), window, cx) {
                        this.document_translation_jobs.update(cx, |jobs, cx| {
                            jobs.finish_open(*job_id, result.clone(), cx);
                        });
                    }
                }
                PaneEvent::OpenSettings => this.open_settings_window(cx),
                PaneEvent::AiPanelDismissed(document_view_id) => {
                    let active_document_id = pane_for_events
                        .read(cx)
                        .active_document_view()
                        .map(|document_view| document_view.entity_id());
                    if this.active_pane == pane_for_events
                        && active_document_id == Some(*document_view_id)
                    {
                        this.close_ai_dock(cx);
                    }
                }
                PaneEvent::TabDrop {
                    dragged_tab,
                    target,
                } => {
                    this.active_pane = pane_for_events.clone();
                    this.handle_dragged_tab_drop(
                        &pane_for_events,
                        dragged_tab.as_ref(),
                        target.clone(),
                        window,
                        cx,
                    );
                }
                PaneEvent::CloseTab(tab_ix) => {
                    this.active_pane = pane_for_events.clone();
                    this.close_tab_in_pane(&pane_for_events, Some(*tab_ix), window, cx);
                }
                PaneEvent::DuplicateTab(tab_ix) => {
                    this.active_pane = pane_for_events.clone();
                    this.duplicate_tab_in_pane(&pane_for_events, *tab_ix, window, cx);
                }
                PaneEvent::TabClicked { tab_id, was_active } => {
                    this.active_pane = pane_for_events.clone();
                    this.handle_synchronized_tab_click(
                        &pane_for_events,
                        *tab_id,
                        *was_active,
                        window,
                        cx,
                    );
                }
                PaneEvent::CreateSynchronizedGroup(tab_id) => {
                    this.start_new_synchronized_group(pane_for_events.clone(), *tab_id, cx);
                }
                PaneEvent::ConnectMoreSynchronizedTabs(tab_id) => {
                    if let Some(group_id) = this.synchronized_tabs.group_for_tab(*tab_id) {
                        this.start_connecting_synchronized_group(
                            pane_for_events.clone(),
                            *tab_id,
                            group_id,
                            cx,
                        );
                    }
                }
                PaneEvent::SetSynchronizedScrollPaused { tab_id, paused } => {
                    if this.synchronized_tabs.set_paused(*tab_id, *paused) {
                        this.refresh_synchronized_tab_presentation(cx);
                    }
                }
                PaneEvent::LeaveSynchronizedGroup(tab_id) => {
                    this.synchronized_tabs.leave_group(*tab_id);
                    this.refresh_synchronized_tab_presentation(cx);
                }
                PaneEvent::FinishSynchronizedConnection => {
                    this.finish_synchronized_connection(cx);
                }
                PaneEvent::DocumentScrolled { tab_id, delta } => {
                    this.propagate_synchronized_scroll(*tab_id, *delta, cx);
                }
            },
        )
        .detach();

        let pane_for_focus = pane.clone();
        let focus_handle = pane.read(cx).focus_handle();
        cx.on_focus_in(&focus_handle, window, move |this, _, cx| {
            if this.active_pane != pane_for_focus {
                this.active_pane = pane_for_focus.clone();
                cx.notify();
            }
        })
        .detach();

        let pane_for_observe = pane.clone();
        cx.observe(pane, move |this, _, cx| {
            if this.active_pane == pane_for_observe {
                cx.notify();
            }
        })
        .detach();
    }

    fn focus_active_pane(&self, window: &mut Window, cx: &mut Context<Self>) {
        let active_pane = self.active_pane.clone();
        active_pane.update(cx, |pane, cx| pane.focus(window, cx));
    }

    fn sync_tab_bar_visibility(&mut self, cx: &mut Context<Self>) {
        let force_show_tab_bar = self.pane_group.panes().len() > 1;
        for pane in self.pane_group.panes() {
            pane.update(cx, |pane, cx| {
                pane.set_force_show_tab_bar(force_show_tab_bar, cx);
            });
        }
    }

    fn synchronized_connection_color_slot(&self, group: SynchronizedConnectionGroup) -> usize {
        match group {
            SynchronizedConnectionGroup::New(reserved) => reserved.color_slot,
            SynchronizedConnectionGroup::Existing(group_id) => self
                .synchronized_tabs
                .group_summary(group_id)
                .map(|group| group.color_slot)
                .unwrap_or_default(),
        }
    }

    fn synchronized_connection_display_number(&self, group: SynchronizedConnectionGroup) -> u64 {
        match group {
            SynchronizedConnectionGroup::New(reserved) => reserved.display_number,
            SynchronizedConnectionGroup::Existing(group_id) => self
                .synchronized_tabs
                .group_summary(group_id)
                .map(|group| group.display_number)
                .unwrap_or_default(),
        }
    }

    fn synchronized_connection_tab_state(
        &self,
        connection: &SynchronizedConnection,
        pane_id: EntityId,
        tab_id: EntityId,
        is_document: bool,
    ) -> SynchronizedConnectionTabState {
        if tab_id == connection.source_tab {
            return SynchronizedConnectionTabState::Source;
        }

        if let SynchronizedConnectionGroup::Existing(group_id) = connection.group
            && self.synchronized_tabs.contains(group_id, tab_id)
        {
            return SynchronizedConnectionTabState::Member;
        }

        if !is_document {
            return SynchronizedConnectionTabState::Invalid(
                SynchronizedConnectionInvalidReason::NonDocument,
            );
        }

        if let Some(group_id) = self.synchronized_tabs.group_for_tab(tab_id) {
            let group_number = self
                .synchronized_tabs
                .group_summary(group_id)
                .map_or(0, |group| group.display_number);
            return SynchronizedConnectionTabState::Invalid(
                SynchronizedConnectionInvalidReason::AlreadyInGroup { group_number },
            );
        }

        let group_number = self.synchronized_connection_display_number(connection.group);
        let pane_is_represented = match connection.group {
            SynchronizedConnectionGroup::New(_) => pane_id == connection.source_pane.entity_id(),
            SynchronizedConnectionGroup::Existing(group_id) => !self
                .synchronized_tabs
                .can_add_member(group_id, tab_id, pane_id),
        };
        if pane_is_represented {
            SynchronizedConnectionTabState::Invalid(
                SynchronizedConnectionInvalidReason::PaneAlreadyRepresented { group_number },
            )
        } else {
            SynchronizedConnectionTabState::Eligible
        }
    }

    fn synchronized_connection_tab_bounds(
        &self,
        tab_id: EntityId,
        cx: &Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        for pane in self.pane_group.panes() {
            if !pane.read(cx).has_tab_id(tab_id) {
                continue;
            }
            let tab_bounds = pane.read(cx).tab_bounds(tab_id)?;
            let center = tab_bounds.center();
            if self
                .pane_group
                .bounding_box_for_pane(&pane)
                .is_some_and(|pane_bounds| !pane_bounds.contains(&center))
            {
                return None;
            }
            return Some(tab_bounds);
        }
        None
    }

    fn synchronized_connection_node(
        &self,
        tab_id: EntityId,
        cx: &Context<Self>,
    ) -> Option<SynchronizedConnectionNode> {
        let tab_bounds = self.synchronized_connection_tab_bounds(tab_id, cx)?;
        Some(SynchronizedConnectionNode {
            tab_id,
            center: tab_bounds.center(),
        })
    }

    fn synchronized_group_connection_nodes(
        &self,
        group_id: SynchronizedGroupId,
        cx: &Context<Self>,
    ) -> Vec<SynchronizedConnectionNode> {
        self.synchronized_tabs
            .member_tabs(group_id)
            .into_iter()
            .filter_map(|tab_id| self.synchronized_connection_node(tab_id, cx))
            .collect()
    }

    fn initial_synchronized_connection_edges(
        &self,
        group_id: SynchronizedGroupId,
        added_at: Instant,
        cx: &Context<Self>,
    ) -> Vec<SynchronizedConnectionEdge> {
        synchronized_connection_tree(&self.synchronized_group_connection_nodes(group_id, cx))
            .into_iter()
            .map(|(from, to)| SynchronizedConnectionEdge { from, to, added_at })
            .collect()
    }

    fn closest_synchronized_connection_edge(
        &self,
        group_id: SynchronizedGroupId,
        target_tab: EntityId,
        added_at: Instant,
        cx: &Context<Self>,
    ) -> Option<SynchronizedConnectionEdge> {
        let target = self.synchronized_connection_node(target_tab, cx)?;
        let candidates = self
            .synchronized_group_connection_nodes(group_id, cx)
            .into_iter()
            .filter(|candidate| candidate.tab_id != target_tab)
            .collect::<Vec<_>>();
        let from = closest_synchronized_tab(target, &candidates)?;
        Some(SynchronizedConnectionEdge {
            from,
            to: target_tab,
            added_at,
        })
    }

    fn repair_synchronized_connection_edges(&mut self, cx: &Context<Self>) {
        let Some(connection) = self.synchronized_connection.as_ref() else {
            return;
        };
        let SynchronizedConnectionGroup::Existing(group_id) = connection.group else {
            return;
        };
        let members = self.synchronized_tabs.member_tabs(group_id);
        let needs_repair = connection
            .edges
            .iter()
            .any(|edge| !members.contains(&edge.from) || !members.contains(&edge.to));
        if !needs_repair {
            return;
        }

        let now = Instant::now();
        let repaired = self.initial_synchronized_connection_edges(group_id, now, cx);
        if let Some(connection) = self.synchronized_connection.as_mut() {
            connection.edges = repaired;
            connection.animation_started_at = now;
        }
    }

    fn refresh_synchronized_tab_presentation(&mut self, cx: &mut Context<Self>) {
        let invalid_connection = self
            .synchronized_connection
            .as_ref()
            .is_some_and(|connection| {
                !connection
                    .source_pane
                    .read(cx)
                    .has_tab_id(connection.source_tab)
                    || matches!(
                        connection.group,
                        SynchronizedConnectionGroup::Existing(group_id)
                            if self.synchronized_tabs.group_summary(group_id).is_none()
                    )
            });
        if invalid_connection {
            self.synchronized_connection = None;
        }
        self.repair_synchronized_connection_edges(cx);
        for pane in self.pane_group.panes() {
            let pane_id = pane.entity_id();
            let tab_ids = pane.read(cx).tab_ids();
            let mut presentation = PaneSynchronizedPresentation::default();
            if let Some(connection) = &self.synchronized_connection {
                presentation.connection_color_slot =
                    Some(self.synchronized_connection_color_slot(connection.group));
                presentation.recently_added = connection.recently_added;
            }
            for tab_id in tab_ids {
                if let Some(group) = self.synchronized_tabs.summary_for_tab(tab_id) {
                    presentation.group_by_tab.insert(tab_id, group);
                }
                if self.synchronized_tabs.is_paused(tab_id) {
                    presentation.paused_tabs.insert(tab_id);
                }
                if let Some(connection) = &self.synchronized_connection {
                    let state = self.synchronized_connection_tab_state(
                        connection,
                        pane_id,
                        tab_id,
                        pane.read(cx).tab_is_document(tab_id),
                    );
                    presentation.connection_tabs.insert(tab_id, state);
                }
            }
            pane.update(cx, |pane, cx| {
                pane.set_synchronized_presentation(presentation, cx);
            });
        }
        cx.notify();
    }

    fn start_new_synchronized_group(
        &mut self,
        source_pane: Entity<Pane>,
        source_tab: gpui::EntityId,
        cx: &mut Context<Self>,
    ) {
        if !source_pane.read(cx).tab_is_document(source_tab)
            || self.synchronized_tabs.group_for_tab(source_tab).is_some()
        {
            return;
        }
        let reserved = self.synchronized_tabs.reserve_group();
        let cursor = source_pane
            .read(cx)
            .tab_bounds(source_tab)
            .map(|bounds| bounds.center())
            .unwrap_or_else(|| point(px(0.0), px(0.0)));
        let now = Instant::now();
        self.fading_synchronized_connection = None;
        self.synchronized_connection = Some(SynchronizedConnection {
            source_tab,
            source_pane,
            group: SynchronizedConnectionGroup::New(reserved),
            cursor,
            animation_started_at: now,
            edges: Vec::new(),
            recently_added: None,
            addition_sequence: 0,
        });
        self.refresh_synchronized_tab_presentation(cx);
    }

    fn start_connecting_synchronized_group(
        &mut self,
        source_pane: Entity<Pane>,
        source_tab: gpui::EntityId,
        group_id: SynchronizedGroupId,
        cx: &mut Context<Self>,
    ) {
        if self.synchronized_tabs.group_for_tab(source_tab) != Some(group_id) {
            return;
        }
        self.synchronized_tabs.select_for_tab(source_tab);
        let cursor = source_pane
            .read(cx)
            .tab_bounds(source_tab)
            .map(|bounds| bounds.center())
            .unwrap_or_else(|| point(px(0.0), px(0.0)));
        let now = Instant::now();
        let edges = self.initial_synchronized_connection_edges(group_id, now, cx);
        self.fading_synchronized_connection = None;
        self.synchronized_connection = Some(SynchronizedConnection {
            source_tab,
            source_pane,
            group: SynchronizedConnectionGroup::Existing(group_id),
            cursor,
            animation_started_at: now,
            edges,
            recently_added: None,
            addition_sequence: 0,
        });
        self.refresh_synchronized_tab_presentation(cx);
    }

    fn finish_synchronized_connection(&mut self, cx: &mut Context<Self>) {
        let Some(connection) = self.synchronized_connection.take() else {
            return;
        };
        if !cx.reduce_motion() && !connection.edges.is_empty() {
            self.fading_synchronized_connection = Some(FadingSynchronizedConnection {
                color_slot: self.synchronized_connection_color_slot(connection.group),
                edges: connection.edges,
                animation_started_at: connection.animation_started_at,
                started_at: Instant::now(),
            });
        } else {
            self.fading_synchronized_connection = None;
        }
        self.refresh_synchronized_tab_presentation(cx);
    }

    fn handle_synchronized_tab_click(
        &mut self,
        pane: &Entity<Pane>,
        tab_id: gpui::EntityId,
        was_active: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(connection) = self.synchronized_connection.as_ref() {
            let source_tab = connection.source_tab;
            let source_pane = connection.source_pane.clone();
            let connection_group = connection.group;
            let target_pane_id = pane.entity_id();
            if tab_id == source_tab || !pane.read(cx).tab_is_document(tab_id) {
                return;
            }
            let now = Instant::now();
            let (added_group, first_edge) = match connection_group {
                SynchronizedConnectionGroup::New(reserved) => {
                    if target_pane_id == source_pane.entity_id()
                        || !self.synchronized_tabs.commit_reserved_group(
                            reserved,
                            source_tab,
                            source_pane.entity_id(),
                            tab_id,
                            target_pane_id,
                        )
                    {
                        return;
                    }
                    (
                        Some(reserved.id),
                        Some(SynchronizedConnectionEdge {
                            from: source_tab,
                            to: tab_id,
                            added_at: now,
                        }),
                    )
                }
                SynchronizedConnectionGroup::Existing(group_id) => (
                    self.synchronized_tabs
                        .add_member(group_id, tab_id, target_pane_id)
                        .then_some(group_id),
                    None,
                ),
            };
            if let Some(group_id) = added_group {
                let edge = first_edge.or_else(|| {
                    self.closest_synchronized_connection_edge(group_id, tab_id, now, cx)
                });
                if let Some(connection) = self.synchronized_connection.as_mut() {
                    connection.group = SynchronizedConnectionGroup::Existing(group_id);
                    if let Some(edge) = edge
                        && !connection.edges.iter().any(|candidate| {
                            (candidate.from == edge.from && candidate.to == edge.to)
                                || (candidate.from == edge.to && candidate.to == edge.from)
                        })
                    {
                        connection.edges.push(edge);
                    }
                    connection.addition_sequence += 1;
                    connection.recently_added = Some(RecentlyAddedSynchronizedTab {
                        tab_id,
                        group_id,
                        sequence: connection.addition_sequence,
                    });
                }
                self.refresh_synchronized_tab_presentation(cx);
            }
            return;
        }

        let Some(group_id) = self.synchronized_tabs.select_for_tab(tab_id) else {
            return;
        };
        if was_active {
            self.activate_synchronized_group(group_id, pane, window, cx);
        }
        self.refresh_synchronized_tab_presentation(cx);
    }

    fn activate_synchronized_group(
        &mut self,
        group_id: SynchronizedGroupId,
        initiating_pane: &Entity<Pane>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let members = self.synchronized_tabs.member_tabs(group_id);
        for pane in self.pane_group.panes() {
            let member = members
                .iter()
                .find(|tab_id| pane.read(cx).has_tab_id(**tab_id))
                .copied();
            if let Some(tab_id) = member {
                pane.update(cx, |pane, cx| {
                    pane.activate_tab_by_id(tab_id, false, window, cx);
                });
            }
        }
        self.active_pane = initiating_pane.clone();
        initiating_pane.update(cx, |pane, cx| pane.focus(window, cx));
        cx.notify();
    }

    fn propagate_synchronized_scroll(
        &mut self,
        source_tab: gpui::EntityId,
        delta: crate::document_view::SynchronizedScrollDelta,
        cx: &mut Context<Self>,
    ) {
        let Some(group_id) = self.synchronized_tabs.group_for_tab(source_tab) else {
            return;
        };
        if self.synchronized_tabs.is_paused(source_tab) {
            return;
        }
        let source_is_displayed = self
            .pane_group
            .panes()
            .into_iter()
            .any(|pane| pane.read(cx).active_tab_id() == Some(source_tab));
        if !source_is_displayed {
            return;
        }

        for member_tab in self.synchronized_tabs.member_tabs(group_id) {
            if member_tab == source_tab || self.synchronized_tabs.is_paused(member_tab) {
                continue;
            }
            let Some(pane) = self
                .pane_group
                .panes()
                .into_iter()
                .find(|pane| pane.read(cx).has_tab_id(member_tab))
            else {
                continue;
            };
            if pane.read(cx).active_tab_id() != Some(member_tab) {
                continue;
            }
            if let Some(document_view) = pane.read(cx).document_view_for_id(member_tab) {
                document_view.update(cx, |document_view, cx| {
                    document_view.apply_synchronized_scroll_delta(delta, cx);
                });
            }
        }
    }

    fn synchronized_connection_anchor(
        tab_bounds: Bounds<Pixels>,
        toward: Point<Pixels>,
    ) -> SynchronizedConnectionAnchor {
        let center = tab_bounds.center();
        let dx = toward.x.as_f32() - center.x.as_f32();
        let dy = toward.y.as_f32() - center.y.as_f32();
        if dx.abs() >= dy.abs() {
            let (x, edge) = if dx >= 0.0 {
                (tab_bounds.right(), SynchronizedConnectionAnchorEdge::Right)
            } else {
                (tab_bounds.left(), SynchronizedConnectionAnchorEdge::Left)
            };
            SynchronizedConnectionAnchor {
                position: point(x, center.y),
                edge,
            }
        } else {
            let (y, edge) = if dy >= 0.0 {
                (
                    tab_bounds.bottom(),
                    SynchronizedConnectionAnchorEdge::Bottom,
                )
            } else {
                (tab_bounds.top(), SynchronizedConnectionAnchorEdge::Top)
            };
            SynchronizedConnectionAnchor {
                position: point(center.x, y),
                edge,
            }
        }
    }

    fn synchronized_connection_control(
        anchor: SynchronizedConnectionAnchor,
        distance: f32,
    ) -> Point<Pixels> {
        match anchor.edge {
            SynchronizedConnectionAnchorEdge::Top => {
                point(anchor.position.x, anchor.position.y - px(distance))
            }
            SynchronizedConnectionAnchorEdge::Right => {
                point(anchor.position.x + px(distance), anchor.position.y)
            }
            SynchronizedConnectionAnchorEdge::Bottom => {
                point(anchor.position.x, anchor.position.y + px(distance))
            }
            SynchronizedConnectionAnchorEdge::Left => {
                point(anchor.position.x - px(distance), anchor.position.y)
            }
        }
    }

    fn synchronized_connection_handles(
        curves: &[SynchronizedConnectionCurve],
    ) -> Vec<SynchronizedConnectionHandle> {
        let mut handles: Vec<SynchronizedConnectionHandle> = Vec::with_capacity(curves.len() * 2);
        for curve in curves {
            for center in [curve.start.position, curve.end.position] {
                if let Some(existing) = handles
                    .iter_mut()
                    .find(|handle| Self::point_distance(handle.center, center) < 0.5)
                {
                    if curve.added_at < existing.added_at {
                        existing.added_at = curve.added_at;
                    }
                } else {
                    handles.push(SynchronizedConnectionHandle {
                        center,
                        added_at: curve.added_at,
                    });
                }
            }
        }
        handles
    }

    fn synchronized_connection_curve(
        from: Bounds<Pixels>,
        to: Bounds<Pixels>,
        added_at: Instant,
    ) -> SynchronizedConnectionCurve {
        let start = Self::synchronized_connection_anchor(from, to.center());
        let end = Self::synchronized_connection_anchor(to, from.center());
        let dx = end.position.x.as_f32() - start.position.x.as_f32();
        let dy = end.position.y.as_f32() - start.position.y.as_f32();
        let bend = if matches!(
            start.edge,
            SynchronizedConnectionAnchorEdge::Left | SynchronizedConnectionAnchorEdge::Right
        ) {
            (dx.abs() * 0.35).clamp(18.0, 110.0)
        } else {
            (dy.abs() * 0.35).clamp(18.0, 110.0)
        };
        let control_a = Self::synchronized_connection_control(start, bend);
        let control_b = Self::synchronized_connection_control(end, bend);
        SynchronizedConnectionCurve {
            start,
            control_a,
            control_b,
            end,
            added_at,
        }
    }

    fn cubic_point(curve: SynchronizedConnectionCurve, t: f32) -> Point<Pixels> {
        let inverse = 1.0 - t;
        let a = inverse * inverse * inverse;
        let b = 3.0 * inverse * inverse * t;
        let c = 3.0 * inverse * t * t;
        let d = t * t * t;
        point(
            px(curve.start.position.x.as_f32() * a
                + curve.control_a.x.as_f32() * b
                + curve.control_b.x.as_f32() * c
                + curve.end.position.x.as_f32() * d),
            px(curve.start.position.y.as_f32() * a
                + curve.control_a.y.as_f32() * b
                + curve.control_b.y.as_f32() * c
                + curve.end.position.y.as_f32() * d),
        )
    }

    fn synchronized_connection_dot_parameter(index: usize, dot_count: usize) -> f32 {
        debug_assert!(index < dot_count);
        (index + 1) as f32 / (dot_count + 1) as f32
    }

    fn point_distance(left: Point<Pixels>, right: Point<Pixels>) -> f32 {
        let dx = left.x.as_f32() - right.x.as_f32();
        let dy = left.y.as_f32() - right.y.as_f32();
        (dx * dx + dy * dy).sqrt()
    }

    fn synchronized_connection_curves(
        &self,
        edges: &[SynchronizedConnectionEdge],
        viewport: Bounds<Pixels>,
        cx: &Context<Self>,
    ) -> Vec<SynchronizedConnectionCurve> {
        edges
            .iter()
            .filter_map(|edge| {
                let from = self.synchronized_connection_tab_bounds(edge.from, cx)?;
                let to = self.synchronized_connection_tab_bounds(edge.to, cx)?;
                if !viewport.contains(&from.center()) || !viewport.contains(&to.center()) {
                    return None;
                }
                Some(Self::synchronized_connection_curve(from, to, edge.added_at))
            })
            .collect()
    }

    fn hovered_synchronized_connection_tab(
        &self,
        cx: &App,
    ) -> Option<(SynchronizedConnectionTabState, Bounds<Pixels>)> {
        let connection = self.synchronized_connection.as_ref()?;
        for pane in self.pane_group.panes() {
            if self
                .pane_group
                .bounding_box_for_pane(&pane)
                .is_some_and(|pane_bounds| !pane_bounds.contains(&connection.cursor))
            {
                continue;
            }
            for tab_id in pane.read(cx).tab_ids() {
                let Some(tab_bounds) = pane.read(cx).tab_bounds(tab_id) else {
                    continue;
                };
                if !tab_bounds.contains(&connection.cursor) {
                    continue;
                }
                return Some((
                    self.synchronized_connection_tab_state(
                        connection,
                        pane.entity_id(),
                        tab_id,
                        pane.read(cx).tab_is_document(tab_id),
                    ),
                    tab_bounds,
                ));
            }
        }
        None
    }

    fn synchronized_connection_hint_origin(
        cursor: Point<Pixels>,
        hovered_bounds: Option<Bounds<Pixels>>,
        viewport: gpui::Size<Pixels>,
    ) -> Point<Pixels> {
        const HINT_WIDTH: f32 = 310.0;
        const HINT_HEIGHT: f32 = 58.0;
        const MARGIN: f32 = 8.0;
        const OFFSET: f32 = 12.0;

        let mut x = cursor.x.as_f32() + OFFSET;
        let mut y = hovered_bounds.map_or(cursor.y.as_f32() + OFFSET, |bounds| {
            bounds.bottom().as_f32() + MARGIN
        });
        if x + HINT_WIDTH + MARGIN > viewport.width.as_f32() {
            x = cursor.x.as_f32() - HINT_WIDTH - OFFSET;
        }
        if y + HINT_HEIGHT + MARGIN > viewport.height.as_f32() {
            y = hovered_bounds.map_or(cursor.y.as_f32() - HINT_HEIGHT - OFFSET, |bounds| {
                bounds.top().as_f32() - HINT_HEIGHT - MARGIN
            });
        }
        let max_x = (viewport.width.as_f32() - HINT_WIDTH - MARGIN).max(MARGIN);
        let max_y = (viewport.height.as_f32() - HINT_HEIGHT - MARGIN).max(MARGIN);
        point(px(x.clamp(MARGIN, max_x)), px(y.clamp(MARGIN, max_y)))
    }

    fn render_synchronized_connection(
        &mut self,
        theme: ThemeColor,
        window: &mut Window,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        let now = Instant::now();
        let reduce_motion = cx.reduce_motion();
        if reduce_motion
            || self
                .fading_synchronized_connection
                .as_ref()
                .is_some_and(|fade| {
                    now.saturating_duration_since(fade.started_at) >= SYNCHRONIZED_NETWORK_FADE_OUT
                })
        {
            self.fading_synchronized_connection = None;
        }

        let hovered = self.hovered_synchronized_connection_tab(cx);
        let active = self.synchronized_connection.as_ref().map(|connection| {
            (
                connection.edges.clone(),
                self.synchronized_connection_color_slot(connection.group),
                connection.animation_started_at,
                connection.cursor,
                connection.group,
            )
        });
        let (edges, color_slot, animation_started_at, overall_opacity, hint_data, fading) =
            if let Some((edges, color_slot, animation_started_at, cursor, group)) = active {
                (
                    edges,
                    color_slot,
                    animation_started_at,
                    1.0,
                    Some((cursor, group)),
                    false,
                )
            } else if let Some(fade) = &self.fading_synchronized_connection {
                let progress = now.saturating_duration_since(fade.started_at).as_secs_f32()
                    / SYNCHRONIZED_NETWORK_FADE_OUT.as_secs_f32();
                (
                    fade.edges.clone(),
                    fade.color_slot,
                    fade.animation_started_at,
                    1.0 - progress.clamp(0.0, 1.0),
                    None,
                    true,
                )
            } else {
                return None;
            };

        let viewport = bounds(point(px(0.0), px(0.0)), window.viewport_size());
        let curves = self.synchronized_connection_curves(&edges, viewport, cx);
        let synchronized_theme = cx.global::<SettingsStore>().resolved_theme;
        let color = crate::synchronized_group_color(synchronized_theme, color_slot);
        let handle_outline = theme.background;
        let network = (!curves.is_empty()).then(|| {
            canvas(
                move |_, _, _| curves,
                move |_, curves, window, _| {
                    let handles = Self::synchronized_connection_handles(&curves);
                    let period = SYNCHRONIZED_DOT_PERIOD.as_secs_f32();
                    let phase = now
                        .saturating_duration_since(animation_started_at)
                        .as_secs_f32()
                        / period
                        % 1.0;
                    for curve in curves.iter() {
                        let approximate_length =
                            Self::point_distance(curve.start.position, curve.control_a)
                                + Self::point_distance(curve.control_a, curve.control_b)
                                + Self::point_distance(curve.control_b, curve.end.position);
                        let dot_count = ((approximate_length / 9.0).ceil() as usize).clamp(2, 120);
                        let edge_opacity = if reduce_motion {
                            1.0
                        } else {
                            (now.saturating_duration_since(curve.added_at).as_secs_f32()
                                / SYNCHRONIZED_EDGE_FADE_IN.as_secs_f32())
                            .clamp(0.0, 1.0)
                        };
                        for index in 0..dot_count {
                            let t = Self::synchronized_connection_dot_parameter(index, dot_count);
                            let distance_from_end = t.min(1.0 - t) * 2.0;
                            let pulse = if reduce_motion {
                                0.0
                            } else {
                                (1.0 - (distance_from_end - phase).abs() / 0.22)
                                    .clamp(0.0, 1.0)
                                    .powi(2)
                            };
                            let diameter = if reduce_motion {
                                3.0
                            } else {
                                2.7 + pulse * 1.6
                            };
                            let opacity = if reduce_motion {
                                0.58
                            } else {
                                0.3 + pulse * 0.65
                            };
                            let center = Self::cubic_point(*curve, t);
                            let radius = diameter / 2.0;
                            window.paint_quad(
                                fill(
                                    bounds(
                                        point(center.x - px(radius), center.y - px(radius)),
                                        size(px(diameter), px(diameter)),
                                    ),
                                    color.opacity(opacity * edge_opacity * overall_opacity),
                                )
                                .corner_radii(px(radius)),
                            );
                        }
                    }

                    for handle in handles {
                        let handle_opacity = if reduce_motion {
                            1.0
                        } else {
                            (now.saturating_duration_since(handle.added_at).as_secs_f32()
                                / SYNCHRONIZED_EDGE_FADE_IN.as_secs_f32())
                            .clamp(0.0, 1.0)
                        } * overall_opacity;
                        let outline_radius = SYNCHRONIZED_HANDLE_OUTLINE_DIAMETER / 2.0;
                        window.paint_quad(
                            fill(
                                bounds(
                                    point(
                                        handle.center.x - px(outline_radius),
                                        handle.center.y - px(outline_radius),
                                    ),
                                    size(
                                        px(SYNCHRONIZED_HANDLE_OUTLINE_DIAMETER),
                                        px(SYNCHRONIZED_HANDLE_OUTLINE_DIAMETER),
                                    ),
                                ),
                                handle_outline.opacity(0.96 * handle_opacity),
                            )
                            .corner_radii(px(outline_radius)),
                        );

                        let handle_radius = SYNCHRONIZED_HANDLE_DIAMETER / 2.0;
                        window.paint_quad(
                            fill(
                                bounds(
                                    point(
                                        handle.center.x - px(handle_radius),
                                        handle.center.y - px(handle_radius),
                                    ),
                                    size(
                                        px(SYNCHRONIZED_HANDLE_DIAMETER),
                                        px(SYNCHRONIZED_HANDLE_DIAMETER),
                                    ),
                                ),
                                color.opacity(handle_opacity),
                            )
                            .corner_radii(px(handle_radius)),
                        );
                    }
                },
            )
            .absolute()
            .top_0()
            .right_0()
            .bottom_0()
            .left_0()
        });

        if !reduce_motion && (!edges.is_empty() || fading) {
            window.request_animation_frame();
        }

        let hint = hint_data.map(|(cursor, group)| {
            let group_number = self.synchronized_connection_display_number(group);
            let (primary, secondary) = match hovered.map(|(state, _)| state) {
                Some(SynchronizedConnectionTabState::Source) => (
                    crate::fl!("synchronized-tabs-connection-source"),
                    crate::fl!("synchronized-tabs-connection-finish"),
                ),
                Some(SynchronizedConnectionTabState::Member) => (
                    crate::fl!(
                        "synchronized-tabs-connection-already-member",
                        number = group_number as i64
                    ),
                    crate::fl!("synchronized-tabs-connection-finish"),
                ),
                Some(SynchronizedConnectionTabState::Invalid(
                    SynchronizedConnectionInvalidReason::NonDocument,
                )) => (
                    crate::fl!("synchronized-tabs-connection-documents-only"),
                    crate::fl!("synchronized-tabs-connection-finish"),
                ),
                Some(SynchronizedConnectionTabState::Invalid(
                    SynchronizedConnectionInvalidReason::AlreadyInGroup { group_number },
                )) => (
                    crate::fl!(
                        "synchronized-tabs-connection-already-member",
                        number = group_number as i64
                    ),
                    crate::fl!("synchronized-tabs-connection-finish"),
                ),
                Some(SynchronizedConnectionTabState::Invalid(
                    SynchronizedConnectionInvalidReason::PaneAlreadyRepresented { group_number },
                )) => (
                    crate::fl!(
                        "synchronized-tabs-connection-pane-represented",
                        number = group_number as i64
                    ),
                    crate::fl!("synchronized-tabs-connection-finish"),
                ),
                Some(SynchronizedConnectionTabState::Eligible) | None => match group {
                    SynchronizedConnectionGroup::New(_) => (
                        crate::fl!("synchronized-tabs-connection-select"),
                        crate::fl!("synchronized-tabs-connection-finish"),
                    ),
                    SynchronizedConnectionGroup::Existing(group_id) => {
                        let count = self
                            .synchronized_tabs
                            .group_summary(group_id)
                            .map_or(0, |group| group.member_count);
                        (
                            crate::fl!(
                                "synchronized-tabs-group-summary",
                                number = group_number as i64,
                                count = count as i64
                            ),
                            crate::fl!("synchronized-tabs-connection-continue"),
                        )
                    }
                },
            };
            let origin = Self::synchronized_connection_hint_origin(
                cursor,
                hovered.map(|(_, bounds)| bounds),
                window.viewport_size(),
            );
            div()
                .absolute()
                .left(origin.x)
                .top(origin.y)
                .w(px(310.0))
                .px_3()
                .py_2()
                .rounded_md()
                .border_1()
                .border_color(theme.border)
                .bg(theme.popover)
                .text_color(theme.popover_foreground)
                .shadow_lg()
                .debug_selector(|| "synchronized-connection-hint".to_string())
                .child(
                    div().flex().flex_col().gap_1().child(primary).child(
                        div()
                            .text_size(px(11.0))
                            .text_color(theme.muted_foreground)
                            .child(secondary),
                    ),
                )
        });

        Some(
            div()
                .absolute()
                .top_0()
                .right_0()
                .bottom_0()
                .left_0()
                .debug_selector(|| "synchronized-connection-overlay".to_string())
                .children(network)
                .children(hint)
                .into_any_element(),
        )
    }

    fn create_empty_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<Pane> {
        let document_translation_jobs = self.document_translation_jobs.clone();
        let pane = cx.new(|cx| Pane::empty(cx.focus_handle(), document_translation_jobs));
        self.subscribe_to_pane(&pane, window, cx);
        pane
    }

    fn open_new_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.set_title_bar_menu_expanded(false, cx);
        let active_pane = self.active_pane.clone();
        active_pane.update(cx, |pane, cx| pane.open_new_tab(window, cx));
    }

    pub fn open_keymap_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let active_pane = self.active_pane.clone();
        active_pane.update(cx, |pane, cx| pane.open_keymap_tab(window, cx));
    }

    fn open_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.set_title_bar_menu_expanded(false, cx);
        if let Some(browser) = self.document_browser.as_ref() {
            browser.focus_handle(cx).focus(window, cx);
            return;
        }

        self.close_command_palette(window, cx);
        self.close_theme_selector(window, cx);
        let previous_focus_handle = window
            .focused(cx)
            .unwrap_or_else(|| self.active_pane.read(cx).focus_handle());
        let active_path = self
            .active_pane
            .read(cx)
            .active_document_view()
            .map(|view| view.read(cx).path().to_path_buf());
        let initial_dir = DocumentBrowser::suggested_directory(active_path.as_deref(), cx);
        let browser = cx.new(|cx| DocumentBrowser::new(initial_dir, window, cx));
        cx.subscribe_in(
            &browser,
            window,
            |this, _, event: &DocumentBrowserEvent, window, cx| match event {
                DocumentBrowserEvent::Open(path) => {
                    let path = path.clone();
                    let replace_active_new_tab = this.active_pane.read(cx).active_tab_is_new_tab();
                    this.close_document_browser(window, cx);
                    if let Err(error) =
                        this.focus_or_open_document(path, None, replace_active_new_tab, window, cx)
                    {
                        eprintln!("failed to open selected document: {error:#}");
                    }
                }
                DocumentBrowserEvent::Dismissed => {
                    this.close_document_browser(window, cx);
                }
            },
        )
        .detach();
        self.document_browser_previous_focus_handle = Some(previous_focus_handle);
        self.document_browser = Some(browser);
        cx.notify();
    }

    fn close_document_browser(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.document_browser.take().is_none() {
            return;
        }
        if let Some(previous_focus_handle) = self.document_browser_previous_focus_handle.take() {
            previous_focus_handle.focus(window, cx);
        } else {
            self.focus_active_pane(window, cx);
        }
        cx.notify();
    }

    fn open_system_file_picker(&mut self, cx: &mut Context<Self>) {
        let dialog = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(crate::fl!("open-pdf").into()),
        });

        cx.spawn(async move |this, cx| match dialog.await {
            Ok(Ok(Some(paths))) => {
                let Some(path) = paths.into_iter().next() else {
                    return;
                };
                match this.update_in(cx, |this, window, cx| {
                    let replace_active_new_tab = this.active_pane.read(cx).active_tab_is_new_tab();
                    this.close_document_browser(window, cx);
                    this.focus_or_open_document(path, None, replace_active_new_tab, window, cx)
                }) {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => eprintln!("failed to open selected PDF: {error:#}"),
                    Err(error) => eprintln!("failed to update workspace: {error:#}"),
                }
            }
            Ok(Err(error)) => eprintln!("failed to open file dialog: {error:#}"),
            Ok(Ok(None)) | Err(_) => {}
        })
        .detach();
    }

    fn focus_or_open_document(
        &mut self,
        path: PathBuf,
        position: Option<DocumentPosition>,
        replace_active_new_tab: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        if self.document_browser.is_some() {
            self.close_document_browser(window, cx);
        }
        let path = normalize_path(&path);
        if let Some((pane, tab_ix)) = self.document_location(&path, cx) {
            self.active_pane = pane.clone();
            pane.update(cx, |pane, cx| {
                pane.activate_existing_document(
                    tab_ix,
                    position,
                    replace_active_new_tab,
                    window,
                    cx,
                );
            });
            cx.notify();
            return Ok(());
        }

        let active_pane = self.active_pane.clone();
        active_pane.update(cx, |pane, cx| {
            pane.focus_or_open_document(path, position, replace_active_new_tab, window, cx)
        })?;
        Ok(())
    }

    fn focus_or_open_document_for_job(
        &mut self,
        path: PathBuf,
        position: Option<DocumentPosition>,
        replace_active_new_tab: bool,
        job_id: DocumentTranslationJobId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        let path = normalize_path(&path);
        if let Some((pane, tab_ix)) = self.document_location(&path, cx) {
            self.active_pane = pane.clone();
            pane.update(cx, |pane, cx| {
                pane.activate_existing_document(
                    tab_ix,
                    position,
                    replace_active_new_tab,
                    window,
                    cx,
                );
                pane.associate_open_job(tab_ix, job_id, cx);
            });
            cx.notify();
            return Ok(());
        }

        let active_pane = self.active_pane.clone();
        active_pane.update(cx, |pane, cx| {
            pane.focus_or_open_document_for_job(
                path,
                position,
                replace_active_new_tab,
                job_id,
                window,
                cx,
            )
        })?;
        Ok(())
    }

    fn document_location(&self, path: &Path, cx: &Context<Self>) -> Option<(Entity<Pane>, usize)> {
        let path = normalize_path(path);
        if let Some(ix) = self.active_pane.read(cx).document_tab_ix(&path, cx) {
            return Some((self.active_pane.clone(), ix));
        }
        self.pane_group.panes().into_iter().find_map(|pane| {
            if pane == self.active_pane {
                return None;
            }
            let ix = pane.read(cx).document_tab_ix(&path, cx)?;
            Some((pane, ix))
        })
    }

    fn close_active_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.set_title_bar_menu_expanded(false, cx);
        let active_pane = self.active_pane.clone();
        self.close_tab_in_pane(&active_pane, None, window, cx);
    }

    fn duplicate_active_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let pane = self.active_pane.clone();
        let tab_ix = pane.read(cx).active_tab_ix();
        self.duplicate_tab_in_pane(&pane, tab_ix, window, cx);
    }

    fn duplicate_tab_in_pane(
        &mut self,
        pane: &Entity<Pane>,
        tab_ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_title_bar_menu_expanded(false, cx);
        match pane.update(cx, |pane, cx| {
            pane.duplicate_document_tab(tab_ix, window, cx)
        }) {
            Ok(Some(_)) => {
                self.active_pane = pane.clone();
                self.refresh_synchronized_tab_presentation(cx);
                cx.notify();
            }
            Ok(None) => {}
            Err(error) => eprintln!("failed to duplicate document tab: {error:#}"),
        }
    }

    fn toggle_transparency(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.transparency_enabled = !self.transparency_enabled;
        apply_window_background(window, self.window_settings, self.transparency_enabled);
        cx.notify();
    }

    fn close_tab_in_pane(
        &mut self,
        pane: &Entity<Pane>,
        tab_ix: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let closed_tab_id = match tab_ix {
            Some(tab_ix) => pane.read(cx).tab_id_at(tab_ix),
            None => pane.read(cx).active_tab_id(),
        };
        if let Some(tab_id) = closed_tab_id {
            self.synchronized_tabs.remove_tab(tab_id);
            if self
                .synchronized_connection
                .as_ref()
                .is_some_and(|connection| connection.source_tab == tab_id)
            {
                self.synchronized_connection = None;
            }
        }
        let empty = pane.update(cx, |pane, cx| match tab_ix {
            Some(tab_ix) => pane.close_tab_at(tab_ix, cx),
            None => pane.close_active_tab(cx),
        });
        if empty {
            self.handle_empty_pane(pane, window, cx);
        } else {
            pane.update(cx, |pane, cx| pane.focus(window, cx));
        }
        self.refresh_synchronized_tab_presentation(cx);
        cx.notify();
    }

    fn handle_empty_pane(
        &mut self,
        pane: &Entity<Pane>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.pane_group.panes().len() <= 1 {
            pane.update(cx, |pane, cx| {
                pane.ensure_new_tab(window, cx);
                pane.focus(window, cx);
            });
            self.active_pane = pane.clone();
            return;
        }

        self.remove_pane(pane, window, cx);
    }

    fn remove_pane(&mut self, pane: &Entity<Pane>, window: &mut Window, cx: &mut Context<Self>) {
        let panes = self.pane_group.panes();
        let remove_ix = panes
            .iter()
            .position(|candidate| candidate == pane)
            .unwrap_or(0);
        for tab_id in pane.read(cx).tab_ids() {
            self.synchronized_tabs.remove_tab(tab_id);
        }
        if self
            .synchronized_connection
            .as_ref()
            .is_some_and(|connection| connection.source_pane == *pane)
        {
            self.synchronized_connection = None;
        }
        self.cancel_pending_translation_splits_for_pane(pane, cx);
        if !self.pane_group.remove(pane, cx) {
            return;
        }

        let remaining = self.pane_group.panes();
        if remaining.is_empty() {
            window.remove_window();
            return;
        }

        let active_ix = remove_ix.min(remaining.len() - 1);
        self.active_pane = remaining[active_ix].clone();
        self.sync_tab_bar_visibility(cx);
        self.active_pane
            .update(cx, |pane, cx| pane.focus(window, cx));
        self.refresh_synchronized_tab_presentation(cx);
        cx.notify();
    }

    fn cancel_pending_translation_splits_for_pane(
        &mut self,
        pane: &Entity<Pane>,
        cx: &mut Context<Self>,
    ) {
        let mut cancelled = Vec::new();
        self.pending_translation_splits.retain(|pending| {
            let matches = match &pending.stage {
                PendingTranslationSplitStage::OpeningSource { source_pane, .. } => {
                    source_pane == pane
                }
                PendingTranslationSplitStage::OpeningTranslated {
                    translated_pane, ..
                } => translated_pane == pane,
            };
            if matches {
                cancelled.push(pending.job_id);
            }
            !matches
        });
        for job_id in cancelled {
            self.document_translation_jobs.update(cx, |jobs, cx| {
                jobs.finish_open(
                    job_id,
                    Err("Opening the translated document was cancelled.".into()),
                    cx,
                );
            });
        }
    }

    fn split_active_pane(
        &mut self,
        direction: SplitDirection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_title_bar_menu_expanded(false, cx);
        let active_pane = self.active_pane.clone();
        let (initial_path, initial_position) = active_pane
            .read(cx)
            .active_document_state(cx)
            .map(|(path, position)| (Some(path), Some(position)))
            .unwrap_or((None, None));
        let new_pane = self.create_empty_pane(window, cx);
        self.pane_group
            .split(&active_pane, &new_pane, direction, cx);
        self.active_pane = new_pane.clone();
        self.sync_tab_bar_visibility(cx);
        if let Some(path) = initial_path
            && let Err(error) = new_pane.update(cx, |pane, cx| {
                pane.focus_or_open_document(path, initial_position, true, window, cx)
            })
        {
            eprintln!("failed to open document in split pane: {error:#}");
            new_pane.update(cx, |pane, cx| pane.ensure_new_tab(window, cx));
        }
        new_pane.update(cx, |pane, cx| pane.focus(window, cx));
        cx.notify();
    }

    fn begin_translation_split_open(
        &mut self,
        job_id: DocumentTranslationJobId,
        source_path: PathBuf,
        translated_path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        let source_path = normalize_path(&source_path);
        if let Some((source_pane, tab_ix)) = self.document_location(&source_path, cx) {
            self.active_pane = source_pane.clone();
            source_pane.update(cx, |pane, cx| {
                pane.activate_existing_document(tab_ix, None, false, window, cx);
            });
            let Some(original) = source_pane.read(cx).active_document_view() else {
                return Err(anyhow::anyhow!(
                    "translation source is still opening; retry when it is ready"
                ));
            };
            return self.start_translated_split_open(
                job_id,
                original,
                translated_path,
                source_pane,
                window,
                cx,
            );
        }

        let source_pane = self.active_pane.clone();
        self.pending_translation_splits
            .push(PendingTranslationSplit {
                job_id,
                stage: PendingTranslationSplitStage::OpeningSource {
                    source_path: source_path.clone(),
                    translated_path,
                    source_pane: source_pane.clone(),
                },
            });
        let replace_active_new_tab = source_pane.read(cx).active_tab_is_new_tab();
        if let Err(error) = source_pane.update(cx, |pane, cx| {
            pane.focus_or_open_document_for_job(
                source_path,
                None,
                replace_active_new_tab,
                job_id,
                window,
                cx,
            )
        }) {
            self.pending_translation_splits
                .retain(|pending| pending.job_id != job_id);
            return Err(error);
        }
        Ok(())
    }

    fn start_translated_split_open(
        &mut self,
        job_id: DocumentTranslationJobId,
        original: Entity<DocumentView>,
        translated_path: PathBuf,
        source_pane: Entity<Pane>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        let translated_pane = self.create_empty_pane(window, cx);
        self.pane_group
            .split(&source_pane, &translated_pane, SplitDirection::Right, cx);
        self.active_pane = translated_pane.clone();
        self.sync_tab_bar_visibility(cx);

        let position = original.read(cx).current_document_position();
        self.pending_translation_splits
            .push(PendingTranslationSplit {
                job_id,
                stage: PendingTranslationSplitStage::OpeningTranslated {
                    original,
                    translated_pane: translated_pane.clone(),
                },
            });
        if let Err(error) = translated_pane.update(cx, |pane, cx| {
            pane.focus_or_open_document_for_job(
                translated_path,
                Some(position),
                true,
                job_id,
                window,
                cx,
            )
        }) {
            self.pending_translation_splits
                .retain(|pending| pending.job_id != job_id);
            self.remove_pane(&translated_pane, window, cx);
            return Err(error);
        }
        cx.notify();
        Ok(())
    }

    fn continue_translation_split_open(
        &mut self,
        job_id: DocumentTranslationJobId,
        result: std::result::Result<(), SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(index) = self
            .pending_translation_splits
            .iter()
            .position(|pending| pending.job_id == job_id)
        else {
            return false;
        };
        let pending = self.pending_translation_splits.remove(index);
        if let Err(error) = result {
            if let PendingTranslationSplitStage::OpeningTranslated {
                translated_pane, ..
            } = pending.stage
            {
                self.remove_pane(&translated_pane, window, cx);
            }
            self.document_translation_jobs.update(cx, |jobs, cx| {
                jobs.finish_open(job_id, Err(error), cx);
            });
            return true;
        }

        match pending.stage {
            PendingTranslationSplitStage::OpeningSource {
                source_path,
                translated_path,
                source_pane: _,
            } => {
                let result = self
                    .document_location(&source_path, cx)
                    .and_then(|(source_pane, tab_ix)| {
                        source_pane.update(cx, |pane, cx| {
                            pane.activate_existing_document(tab_ix, None, false, window, cx);
                        });
                        source_pane
                            .read(cx)
                            .active_document_view()
                            .map(|original| (original, source_pane))
                    })
                    .ok_or_else(|| anyhow::anyhow!("translation source did not finish opening"))
                    .and_then(|(original, source_pane)| {
                        self.start_translated_split_open(
                            job_id,
                            original,
                            translated_path,
                            source_pane,
                            window,
                            cx,
                        )
                    });
                if let Err(error) = result {
                    self.document_translation_jobs.update(cx, |jobs, cx| {
                        jobs.finish_open(job_id, Err(format!("{error:#}").into()), cx);
                    });
                }
            }
            PendingTranslationSplitStage::OpeningTranslated {
                original,
                translated_pane,
            } => {
                let Some(translated) = translated_pane.read(cx).active_document_view() else {
                    self.remove_pane(&translated_pane, window, cx);
                    self.document_translation_jobs.update(cx, |jobs, cx| {
                        jobs.finish_open(
                            job_id,
                            Err("translated document did not finish opening".into()),
                            cx,
                        );
                    });
                    return true;
                };
                if let Some(original_pane) = self
                    .pane_group
                    .panes()
                    .into_iter()
                    .find(|pane| pane.read(cx).has_tab_id(original.entity_id()))
                {
                    _ = self.synchronized_tabs.connect_tab_to_source_group(
                        original.entity_id(),
                        original_pane.entity_id(),
                        translated.entity_id(),
                        translated_pane.entity_id(),
                    );
                }
                self.link_translation_views(original, translated, window, cx);
                translated_pane.update(cx, |pane, cx| pane.focus(window, cx));
                self.document_translation_jobs.update(cx, |jobs, cx| {
                    jobs.finish_open(job_id, Ok(()), cx);
                });
                self.refresh_synchronized_tab_presentation(cx);
                cx.notify();
            }
        };
        true
    }

    fn link_translation_views(
        &self,
        original: Entity<DocumentView>,
        translated: Entity<DocumentView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let translated_target = translated.downgrade();
        cx.subscribe_in(
            &original,
            window,
            move |_, _, event: &DocumentViewEvent, _, cx| match event {
                DocumentViewEvent::BlockHovered(block_id) => {
                    if let Some(translated_target) = translated_target.upgrade() {
                        translated_target.update(cx, |view, cx| {
                            view.apply_linked_hover(*block_id, cx);
                        });
                    }
                }
                DocumentViewEvent::BlockSelected(selection) => {
                    if let Some(translated_target) = translated_target.upgrade() {
                        translated_target.update(cx, |view, cx| {
                            view.apply_linked_selection(*selection, cx);
                        });
                    }
                }
                DocumentViewEvent::OpenDocument { .. }
                | DocumentViewEvent::OpenSettings
                | DocumentViewEvent::AiPanelDismissed
                | DocumentViewEvent::SynchronizedScroll(_) => {}
            },
        )
        .detach();

        let original_target = original.downgrade();
        cx.subscribe_in(
            &translated,
            window,
            move |_, _, event: &DocumentViewEvent, _, cx| match event {
                DocumentViewEvent::BlockHovered(block_id) => {
                    if let Some(original_target) = original_target.upgrade() {
                        original_target.update(cx, |view, cx| {
                            view.apply_linked_hover(*block_id, cx);
                        });
                    }
                }
                DocumentViewEvent::BlockSelected(selection) => {
                    if let Some(original_target) = original_target.upgrade() {
                        original_target.update(cx, |view, cx| {
                            view.apply_linked_selection(*selection, cx);
                        });
                    }
                }
                DocumentViewEvent::OpenDocument { .. }
                | DocumentViewEvent::OpenSettings
                | DocumentViewEvent::AiPanelDismissed
                | DocumentViewEvent::SynchronizedScroll(_) => {}
            },
        )
        .detach();
    }

    fn activate_pane(&mut self, pane: Entity<Pane>, window: &mut Window, cx: &mut Context<Self>) {
        self.active_pane = pane.clone();
        pane.update(cx, |pane, cx| pane.focus(window, cx));
        cx.notify();
    }

    fn activate_pane_in_direction(
        &mut self,
        direction: SplitDirection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(pane) = self
            .pane_group
            .find_pane_in_direction(&self.active_pane, direction)
        {
            self.activate_pane(pane, window, cx);
        }
    }

    fn activate_next_pane(&mut self, reverse: bool, window: &mut Window, cx: &mut Context<Self>) {
        let panes = self.pane_group.panes();
        if panes.len() <= 1 {
            return;
        }
        let active_ix = panes
            .iter()
            .position(|pane| pane == &self.active_pane)
            .unwrap_or(0);
        let next_ix = if reverse {
            active_ix.checked_sub(1).unwrap_or(panes.len() - 1)
        } else {
            (active_ix + 1) % panes.len()
        };
        self.activate_pane(panes[next_ix].clone(), window, cx);
    }

    fn swap_pane_in_direction(&mut self, direction: SplitDirection, cx: &mut Context<Self>) {
        if let Some(target) = self
            .pane_group
            .find_pane_in_direction(&self.active_pane, direction)
        {
            self.pane_group.swap(&self.active_pane, &target);
            cx.notify();
        }
    }

    fn move_pane_to_border(&mut self, direction: SplitDirection, cx: &mut Context<Self>) {
        if self
            .pane_group
            .move_to_border(&self.active_pane, direction, cx)
        {
            cx.notify();
        }
    }

    fn resize_active_pane(
        &mut self,
        direction: SplitDirection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.pane_group
            .resize(&self.active_pane, direction, window, cx);
        cx.notify();
    }

    fn close_other_panes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for pane in self.pane_group.panes() {
            if pane != self.active_pane {
                self.cancel_pending_translation_splits_for_pane(&pane, cx);
                for tab_id in pane.read(cx).tab_ids() {
                    self.synchronized_tabs.remove_tab(tab_id);
                }
                if self
                    .synchronized_connection
                    .as_ref()
                    .is_some_and(|connection| connection.source_pane == pane)
                {
                    self.synchronized_connection = None;
                }
                pane.update(cx, |pane, cx| pane.flush_all_tabs(cx));
            }
        }
        self.pane_group.retain_only(self.active_pane.clone());
        self.sync_tab_bar_visibility(cx);
        self.active_pane
            .update(cx, |pane, cx| pane.focus(window, cx));
        self.refresh_synchronized_tab_presentation(cx);
        cx.notify();
    }

    fn reset_pane_sizes(&mut self, cx: &mut Context<Self>) {
        self.pane_group.reset_pane_sizes(cx);
        cx.notify();
    }

    fn toggle_theme_mode(&mut self, _: &ToggleMode, _window: &mut Window, cx: &mut Context<Self>) {
        let store = cx.global::<SettingsStore>();
        let current_mode = store.settings.theme.mode();
        let next_mode = match current_mode {
            ThemeAppearanceMode::Light => ThemeAppearanceMode::Dark,
            ThemeAppearanceMode::Dark => ThemeAppearanceMode::Light,
            ThemeAppearanceMode::System => {
                let active_name = store.settings.theme.name(store.system_appearance);
                match store.theme_catalog.appearance(active_name) {
                    ThemeAppearance::Light => ThemeAppearanceMode::Dark,
                    ThemeAppearance::Dark => ThemeAppearanceMode::Light,
                }
            }
        };

        let mut settings = store.settings.clone();
        settings.theme.set_mode(next_mode);
        let _ = cx.global_mut::<SettingsStore>().set_settings(settings);
        sync_component_theme(cx);
    }

    fn handle_dragged_tab_drop(
        &mut self,
        target_pane: &Entity<Pane>,
        dragged_tab: &DraggedTab,
        target: PaneDropTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.clear_dragged_tab_outside_window();

        if dragged_tab.source_pane == *target_pane
            && matches!(target, PaneDropTarget::Tab(_) | PaneDropTarget::Append)
        {
            let to = self.drop_index(target_pane, &target, cx);
            target_pane.update(cx, |pane, cx| {
                pane.move_dragged_tab(dragged_tab, to, window, cx)
            });
            return;
        }
        if dragged_tab.source_pane == *target_pane
            && matches!(target, PaneDropTarget::Content(None))
        {
            target_pane.update(cx, |pane, cx| {
                pane.activate_dragged_tab(dragged_tab, window, cx)
            });
            return;
        }

        if !dragged_tab
            .source_pane
            .read(cx)
            .has_tab_id(dragged_tab.tab_id)
        {
            return;
        }

        let destination = TabDestination {
            pane: target_pane.clone(),
            target: target.clone(),
            index: self.drop_index(target_pane, &target, cx),
        };

        if dragged_tab.source_window.window_id() == window.window_handle().window_id() {
            if self.move_tab_to_destination(
                &dragged_tab.source_pane,
                dragged_tab.tab_id,
                destination,
                window,
                cx,
            ) {
                self.complete_transferred_tab_removal(&dragged_tab.source_pane, window, cx);
            }
        } else {
            let Some(source_window) =
                WorkspaceWindowHandle::from_root(dragged_tab.source_window, cx)
            else {
                eprintln!("failed to move tab: source workspace is unavailable");
                return;
            };
            let result = source_window.update(cx, |source, source_window, cx| {
                let transfer = dragged_tab.source_pane.update(cx, |pane, cx| {
                    pane.take_tab_for_transfer(dragged_tab.tab_id, cx)
                });
                if transfer.is_some() {
                    source.complete_transferred_tab_removal(
                        &dragged_tab.source_pane,
                        source_window,
                        cx,
                    );
                }
                transfer
            });
            let transfer = match result {
                Ok(Some(transfer)) => transfer,
                Ok(None) => return,
                Err(error) => {
                    eprintln!("failed to move tab from source window: {error:#}");
                    return;
                }
            };
            self.insert_transferred_tab(
                &destination.pane,
                destination.target,
                destination.index,
                transfer,
                window,
                cx,
            );
            let _ = source_window.update(cx, |source, _, cx| {
                source.synchronized_tabs.remove_tab(dragged_tab.tab_id);
                if source
                    .synchronized_connection
                    .as_ref()
                    .is_some_and(|connection| connection.source_tab == dragged_tab.tab_id)
                {
                    source.synchronized_connection = None;
                }
                source.refresh_synchronized_tab_presentation(cx);
            });
        }

        cx.notify();
    }

    fn drop_index(
        &self,
        target_pane: &Entity<Pane>,
        target: &PaneDropTarget,
        cx: &Context<Self>,
    ) -> usize {
        match target {
            PaneDropTarget::Tab(ix) => *ix,
            PaneDropTarget::Append
            | PaneDropTarget::Content(None)
            | PaneDropTarget::Content(Some(_)) => target_pane.read(cx).len(),
        }
    }

    fn move_tab_to_destination(
        &mut self,
        source_pane: &Entity<Pane>,
        tab_id: gpui::EntityId,
        destination: TabDestination,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !matches!(destination.target, PaneDropTarget::Content(Some(_)))
            && !self
                .synchronized_tabs
                .can_move_tab(tab_id, destination.pane.entity_id())
        {
            return false;
        }
        let transfer = source_pane.update(cx, |pane, cx| pane.take_tab_for_transfer(tab_id, cx));
        let Some(transfer) = transfer else {
            return false;
        };

        self.insert_transferred_tab(
            &destination.pane,
            destination.target,
            destination.index,
            transfer,
            window,
            cx,
        );
        if let Some(actual_pane) = self
            .pane_group
            .panes()
            .into_iter()
            .find(|pane| pane.read(cx).has_tab_id(tab_id))
        {
            self.synchronized_tabs
                .move_tab(tab_id, actual_pane.entity_id());
            if let Some(connection) = self.synchronized_connection.as_mut()
                && connection.source_tab == tab_id
            {
                connection.source_pane = actual_pane;
            }
        }
        self.refresh_synchronized_tab_presentation(cx);
        true
    }

    fn insert_transferred_tab(
        &mut self,
        target_pane: &Entity<Pane>,
        target: PaneDropTarget,
        to: usize,
        transfer: TabTransfer,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match target {
            PaneDropTarget::Content(Some(direction)) => {
                let new_pane = self.create_empty_pane(window, cx);
                new_pane.update(cx, |pane, cx| {
                    pane.insert_transferred_tab(transfer, to, window, cx)
                });
                self.pane_group.split(target_pane, &new_pane, direction, cx);
                self.active_pane = new_pane;
            }
            PaneDropTarget::Tab(_) | PaneDropTarget::Append | PaneDropTarget::Content(None) => {
                target_pane.update(cx, |pane, cx| {
                    pane.insert_transferred_tab(transfer, to, window, cx);
                });
                self.active_pane = target_pane.clone();
            }
        }
        self.sync_tab_bar_visibility(cx);
        cx.notify();
    }

    fn complete_transferred_tab_removal(
        &mut self,
        pane: &Entity<Pane>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if pane.read(cx).len() == 0 {
            if self.pane_group.panes().len() <= 1 {
                window.remove_window();
            } else {
                self.remove_pane(pane, window, cx);
            }
        } else {
            self.sync_tab_bar_visibility(cx);
            cx.notify();
        }
    }

    fn track_dragged_tab_outside_window(
        &mut self,
        event: &DragMoveEvent<DraggedTab>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let dragged_tab = event.drag(cx).clone();
        if dragged_tab.source_window.window_id() != window.window_handle().window_id() {
            self.dragged_tab_outside_window = None;
            return;
        }

        if !dragged_tab
            .source_pane
            .read(cx)
            .has_tab_id(dragged_tab.tab_id)
        {
            self.dragged_tab_outside_window = None;
            return;
        }

        self.dragged_tab_outside_window =
            (!event.bounds.contains(&event.event.position)).then_some(dragged_tab);
    }

    fn clear_dragged_tab_outside_window(&mut self) {
        self.dragged_tab_outside_window = None;
    }

    fn detach_dragged_tab(
        &mut self,
        event: &MouseUpEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(dragged_tab) = self.dragged_tab_outside_window.take() else {
            return;
        };
        if dragged_tab.source_window.window_id() != window.window_handle().window_id() {
            return;
        }
        if !dragged_tab
            .source_pane
            .read(cx)
            .has_tab_id(dragged_tab.tab_id)
        {
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

        let transfer = dragged_tab.source_pane.update(cx, |pane, cx| {
            pane.take_tab_for_transfer(dragged_tab.tab_id, cx)
        });
        let Some(transfer) = transfer else {
            return;
        };
        let mut transfer = Some(transfer);
        let inserted = destination.update(cx, |destination, destination_window, cx| {
            let Some(transfer) = transfer.take() else {
                return false;
            };
            destination.accept_detached_tab(transfer, destination_window, cx);
            true
        });
        if !matches!(inserted, Ok(true)) {
            if let Some(transfer) = transfer {
                dragged_tab
                    .source_pane
                    .update(cx, |pane, cx| pane.restore_transferred_tab(transfer, cx));
            }
            if let Err(error) = inserted {
                eprintln!("failed to insert detached tab: {error:#}");
            }
            return;
        }

        self.complete_transferred_tab_removal(&dragged_tab.source_pane, window, cx);
        self.synchronized_tabs.remove_tab(dragged_tab.tab_id);
        self.refresh_synchronized_tab_presentation(cx);
        cx.stop_propagation();
    }

    fn accept_detached_tab(
        &mut self,
        transfer: TabTransfer,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let pane = self.active_pane.clone();
        self.insert_transferred_tab(&pane, PaneDropTarget::Append, 0, transfer, window, cx);
    }

    fn detached_window_bounds(event: &MouseUpEvent, window: &Window) -> gpui::Bounds<Pixels> {
        let source_bounds = window.bounds();
        let origin = point(
            source_bounds.origin.x + event.position.x - px(160.0),
            source_bounds.origin.y + event.position.y - px(16.0),
        );

        gpui::Bounds::new(origin, source_bounds.size)
    }

    fn toggle_command_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.command_palette.is_some() {
            self.close_command_palette(window, cx);
            return;
        }

        let previous_focus_handle = window
            .focused(cx)
            .unwrap_or_else(|| self.active_pane.read(cx).focus_handle());
        let actions = window
            .available_actions(cx)
            .into_iter()
            .filter(|action| !action.partial_eq(&ToggleCommandPalette))
            .collect::<Vec<_>>();
        let delegate = CommandPaletteDelegate::new(actions);
        let picker_previous_focus_handle = previous_focus_handle.clone();
        let palette = cx.new(|cx| Picker::new(picker_previous_focus_handle, delegate, window, cx));

        cx.subscribe_in(&palette, window, |this, _, _: &PickerEvent, window, cx| {
            this.close_command_palette(window, cx);
        })
        .detach();

        self.command_palette_previous_focus_handle = Some(previous_focus_handle);
        self.command_palette = Some(palette);
        cx.notify();
    }

    fn close_command_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.command_palette.take().is_some() {
            if let Some(previous_focus_handle) = self.command_palette_previous_focus_handle.take() {
                previous_focus_handle.focus(window, cx);
            } else {
                self.focus_active_pane(window, cx);
            }
            cx.notify();
        }
    }

    fn toggle_theme_selector(
        &mut self,
        _: &ToggleThemeSelector,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.theme_selector.is_some() {
            self.close_theme_selector(window, cx);
            return;
        }

        let previous_focus_handle = window
            .focused(cx)
            .unwrap_or_else(|| self.active_pane.read(cx).focus_handle());

        let (themes, selection, system_appearance, selected_ix) = {
            let store = cx.global::<SettingsStore>();
            let themes = store.theme_catalog.metadata();
            let selection = store.settings.theme.clone();
            let system_appearance = store.system_appearance;
            let active_name = selection.name(system_appearance);
            let selected_ix = themes
                .iter()
                .position(|meta| meta.id == active_name)
                .unwrap_or(0);
            (themes, selection, system_appearance, selected_ix)
        };
        let delegate = ThemeSelectorDelegate::new(themes, selection, system_appearance);

        let picker_previous_focus_handle = previous_focus_handle.clone();
        let selector = cx.new(|cx| {
            Picker::new_with_selected_ix(
                picker_previous_focus_handle,
                delegate,
                selected_ix,
                window,
                cx,
            )
        });

        cx.subscribe_in(&selector, window, |this, _, _: &PickerEvent, window, cx| {
            this.close_theme_selector(window, cx);
        })
        .detach();

        self.theme_selector_previous_focus_handle = Some(previous_focus_handle);
        self.theme_selector = Some(selector);
        cx.notify();
    }

    fn close_theme_selector(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.theme_selector.take().is_some() {
            if let Some(previous_focus_handle) = self.theme_selector_previous_focus_handle.take() {
                previous_focus_handle.focus(window, cx);
            } else {
                self.focus_active_pane(window, cx);
            }
            cx.notify();
        }
    }

    fn set_title_bar_menu_expanded(&mut self, expanded: bool, cx: &mut Context<Self>) {
        let changed = self.title_bar_menu_expanded != expanded
            || (!expanded
                && (self.title_bar_open_menu.is_some() || self.title_bar_popup_menu.is_some()));
        self.title_bar_menu_expanded = expanded;
        if !expanded {
            self.title_bar_open_menu = None;
            self.title_bar_popup_menu = None;
        }
        if changed {
            cx.notify();
        }
    }

    fn open_title_bar_menu(
        &mut self,
        menu: WorkspaceMenu,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let popup_menu = build_popup_menu(menu, self.focus_handle.clone(), window, cx);
        cx.subscribe_in(
            &popup_menu,
            window,
            move |this, popup, _: &DismissEvent, _, cx| {
                if this.title_bar_popup_menu.as_ref() == Some(popup) {
                    this.title_bar_menu_expanded = false;
                    this.title_bar_open_menu = None;
                    this.title_bar_popup_menu = None;
                    cx.notify();
                }
            },
        )
        .detach();

        self.title_bar_menu_expanded = true;
        self.title_bar_open_menu = Some(menu);
        self.title_bar_popup_menu = Some(popup_menu.clone());
        popup_menu.focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    fn toggle_title_bar_menu(
        &mut self,
        menu: WorkspaceMenu,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.title_bar_open_menu == Some(menu) {
            self.title_bar_open_menu = None;
            self.title_bar_popup_menu = None;
            self.focus_handle.focus(window, cx);
            cx.notify();
        } else {
            self.open_title_bar_menu(menu, window, cx);
        }
    }

    fn hover_title_bar_menu(
        &mut self,
        menu: WorkspaceMenu,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.title_bar_open_menu.is_some() && self.title_bar_open_menu != Some(menu) {
            self.open_title_bar_menu(menu, window, cx);
        }
    }

    fn active_tab_label(&self, cx: &Context<Self>) -> Option<SharedString> {
        self.active_pane.read(cx).active_tab_label(cx)
    }

    fn render_top_aligned_overlay(
        content: impl IntoElement + 'static,
        close: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
        theme: ThemeColor,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .flex()
            .items_start()
            .justify_center()
            .pt(px(96.0))
            .bg(theme.overlay)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| close(this, window, cx)),
            )
            .on_scroll_wheel(|_, _, cx| {
                cx.stop_propagation();
            })
            .child(content)
    }

    fn render_centered_overlay(
        content: impl IntoElement + 'static,
        close: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
        theme: ThemeColor,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(theme.overlay)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| close(this, window, cx)),
            )
            .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
            .child(glyphst_ui::motion::modal_in(
                "document-browser-enter",
                div().child(content),
            ))
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let root_overlays = render_overlays(window, cx);
        let ui_font = setup_ui_font(window, cx);
        let theme = cx.theme().colors;
        let window_settings = cx.global::<SettingsStore>().settings.window;
        if self.window_settings != window_settings {
            self.window_settings = window_settings;
            self.transparency_enabled = window_settings.transparency_enabled_by_default();
            apply_window_background(window, window_settings, self.transparency_enabled);
        }
        let shell = shell_background(theme, window_settings, self.transparency_enabled);
        let dim_inactive_panes = cx
            .global::<SettingsStore>()
            .settings
            .behavior
            .dim_inactive_panes;
        let active_pdf_is_presentation = self
            .active_pane
            .read(cx)
            .active_document_view()
            .is_some_and(|document_view| document_view.read(cx).is_presentation_mode());
        let title_bar = (!active_pdf_is_presentation).then(|| {
            WorkspaceTitleBar::new(
                cx.entity(),
                self.title_bar_open_menu,
                self.title_bar_popup_menu.clone(),
                self.active_tab_label(cx),
                shell,
            )
        });
        let synchronized_connection = self.render_synchronized_connection(theme, window, cx);
        div()
            .id("workspace")
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .bg(shell)
            .key_context("Workspace")
            .track_focus(&self.focus_handle)
            .font(ui_font)
            .capture_any_mouse_down(cx.listener(|this, event: &MouseDownEvent, window, cx| {
                if event.button == MouseButton::Right && this.synchronized_connection.is_some() {
                    this.finish_synchronized_connection(cx);
                    window.prevent_default();
                    cx.stop_propagation();
                }
            }))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if this.synchronized_connection.is_some()
                    && !event.keystroke.modifiers.modified()
                    && event.keystroke.key.as_str() == "escape"
                {
                    this.finish_synchronized_connection(cx);
                    window.prevent_default();
                    cx.stop_propagation();
                }
            }))
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                if let Some(connection) = this.synchronized_connection.as_mut() {
                    connection.cursor = event.position;
                    cx.notify();
                }
            }))
            .on_drag_move::<DraggedTab>(cx.listener(
                |this, event: &DragMoveEvent<DraggedTab>, window, cx| {
                    this.track_dragged_tab_outside_window(event, window, cx);
                },
            ))
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, event: &MouseUpEvent, window, cx| {
                    this.detach_dragged_tab(event, window, cx);
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, _| {
                    this.clear_dragged_tab_outside_window();
                }),
            )
            .on_action(cx.listener(|this, _: &OpenFile, window, cx| {
                this.open_file(window, cx);
            }))
            .on_action(cx.listener(|this, _: &OpenWithSystemPicker, _, cx| {
                this.open_system_file_picker(cx);
            }))
            .on_action(cx.listener(|this, _: &OpenNewTab, window, cx| {
                this.open_new_tab(window, cx);
            }))
            .on_action(cx.listener(|this, _: &DuplicateTab, window, cx| {
                this.duplicate_active_tab(window, cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleTransparency, window, cx| {
                this.toggle_transparency(window, cx);
            }))
            .on_action(cx.listener(|this, _: &CloseTab, window, cx| {
                this.close_active_tab(window, cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleToc, window, cx| {
                this.toggle_left_sidebar(DocumentSidebar::Toc, window, cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleNotes, window, cx| {
                this.toggle_left_sidebar(DocumentSidebar::Notes, window, cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleThumbnailSidebar, window, cx| {
                this.toggle_left_sidebar(DocumentSidebar::Thumbnails, window, cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleAiPanel, window, cx| {
                this.toggle_ai_dock(window, cx);
            }))
            .on_action(cx.listener(|this, _: &SplitLeft, window, cx| {
                this.split_active_pane(SplitDirection::Left, window, cx);
            }))
            .on_action(cx.listener(|this, _: &SplitRight, window, cx| {
                this.split_active_pane(SplitDirection::Right, window, cx);
            }))
            .on_action(cx.listener(|this, _: &SplitUp, window, cx| {
                this.split_active_pane(SplitDirection::Up, window, cx);
            }))
            .on_action(cx.listener(|this, _: &SplitDown, window, cx| {
                this.split_active_pane(SplitDirection::Down, window, cx);
            }))
            .on_action(cx.listener(|this, _: &SplitVertical, window, cx| {
                this.split_active_pane(SplitDirection::Right, window, cx);
            }))
            .on_action(cx.listener(|this, _: &SplitHorizontal, window, cx| {
                this.split_active_pane(SplitDirection::Down, window, cx);
            }))
            .on_action(cx.listener(|this, _: &ActivatePaneLeft, window, cx| {
                this.activate_pane_in_direction(SplitDirection::Left, window, cx);
            }))
            .on_action(cx.listener(|this, _: &ActivatePaneRight, window, cx| {
                this.activate_pane_in_direction(SplitDirection::Right, window, cx);
            }))
            .on_action(cx.listener(|this, _: &ActivatePaneUp, window, cx| {
                this.activate_pane_in_direction(SplitDirection::Up, window, cx);
            }))
            .on_action(cx.listener(|this, _: &ActivatePaneDown, window, cx| {
                this.activate_pane_in_direction(SplitDirection::Down, window, cx);
            }))
            .on_action(cx.listener(|this, _: &ActivateNextPane, window, cx| {
                this.activate_next_pane(false, window, cx);
            }))
            .on_action(cx.listener(|this, _: &ActivatePreviousPane, window, cx| {
                this.activate_next_pane(true, window, cx);
            }))
            .on_action(cx.listener(|this, _: &SwapPaneLeft, _, cx| {
                this.swap_pane_in_direction(SplitDirection::Left, cx);
            }))
            .on_action(cx.listener(|this, _: &SwapPaneRight, _, cx| {
                this.swap_pane_in_direction(SplitDirection::Right, cx);
            }))
            .on_action(cx.listener(|this, _: &SwapPaneUp, _, cx| {
                this.swap_pane_in_direction(SplitDirection::Up, cx);
            }))
            .on_action(cx.listener(|this, _: &SwapPaneDown, _, cx| {
                this.swap_pane_in_direction(SplitDirection::Down, cx);
            }))
            .on_action(cx.listener(|this, _: &MovePaneLeft, _, cx| {
                this.move_pane_to_border(SplitDirection::Left, cx);
            }))
            .on_action(cx.listener(|this, _: &MovePaneRight, _, cx| {
                this.move_pane_to_border(SplitDirection::Right, cx);
            }))
            .on_action(cx.listener(|this, _: &MovePaneUp, _, cx| {
                this.move_pane_to_border(SplitDirection::Up, cx);
            }))
            .on_action(cx.listener(|this, _: &MovePaneDown, _, cx| {
                this.move_pane_to_border(SplitDirection::Down, cx);
            }))
            .on_action(cx.listener(|this, _: &ResizePaneLeft, window, cx| {
                this.resize_active_pane(SplitDirection::Left, window, cx);
            }))
            .on_action(cx.listener(|this, _: &ResizePaneRight, window, cx| {
                this.resize_active_pane(SplitDirection::Right, window, cx);
            }))
            .on_action(cx.listener(|this, _: &ResizePaneUp, window, cx| {
                this.resize_active_pane(SplitDirection::Up, window, cx);
            }))
            .on_action(cx.listener(|this, _: &ResizePaneDown, window, cx| {
                this.resize_active_pane(SplitDirection::Down, window, cx);
            }))
            .on_action(cx.listener(|this, _: &CloseOtherPanes, window, cx| {
                this.close_other_panes(window, cx);
            }))
            .on_action(cx.listener(|this, _: &ResetPaneSizes, _, cx| {
                this.reset_pane_sizes(cx);
            }))
            .on_action(cx.listener(Self::toggle_theme_mode))
            .on_action(cx.listener(Self::toggle_theme_selector))
            .on_action(cx.listener(|this, _: &ToggleCommandPalette, window, cx| {
                this.toggle_command_palette(window, cx);
            }))
            .on_action(cx.listener(|this, _: &OpenSettings, _, cx| {
                this.open_settings_window(cx);
            }))
            .on_action(cx.listener(|this, _: &OpenKeybindings, window, cx| {
                this.open_keymap_tab(window, cx);
            }))
            .when_some(title_bar, |this, title_bar| this.child(title_bar))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .overflow_hidden()
                    .child(self.render_body(theme, dim_inactive_panes, window, cx)),
            )
            .children(synchronized_connection)
            .when_some(self.command_palette.clone(), |this, palette| {
                this.child(Self::render_top_aligned_overlay(
                    palette,
                    |this, window, cx| this.close_command_palette(window, cx),
                    theme,
                    cx,
                ))
            })
            .when_some(self.theme_selector.clone(), |this, selector| {
                this.child(Self::render_top_aligned_overlay(
                    selector,
                    |this, window, cx| this.close_theme_selector(window, cx),
                    theme,
                    cx,
                ))
            })
            .when_some(self.document_browser.clone(), |this, browser| {
                this.child(Self::render_centered_overlay(
                    browser,
                    |this, window, cx| this.close_document_browser(window, cx),
                    theme,
                    cx,
                ))
            })
            .children(root_overlays)
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::Path,
        time::{Duration, Instant, SystemTime},
    };

    use glyphst_translation_ui::DocumentTranslationOpenMode;
    use gpui::{
        AppContext as _, Modifiers, MouseButton, MouseUpEvent, TestAppContext, bounds, point, px,
        size,
    };
    use gpui_component::{Root, WindowExt as _};

    use super::{
        SplitDirection, SynchronizedConnectionAnchor, SynchronizedConnectionAnchorEdge,
        SynchronizedConnectionCurve, SynchronizedConnectionGroup,
        SynchronizedConnectionInvalidReason, SynchronizedConnectionTabState, Workspace,
        WorkspaceCallbacks, WorkspaceMenu, WorkspaceWindowHandle, WorkspaceWindowRequest,
    };
    use crate::{
        document_view::{
            DocumentSidebar, RenderCache, SynchronizedScrollDelta, SynchronizedScrollMode,
        },
        library::{DocumentPosition, LibraryStore},
        settings::SettingsStore,
    };

    fn unavailable_workspace_window(
        _: WorkspaceWindowRequest,
        _: &mut gpui::App,
    ) -> anyhow::Result<WorkspaceWindowHandle> {
        anyhow::bail!("not available in tests")
    }

    fn ignore_settings_window(_: &mut gpui::App) {}

    fn temporary_pdf_path(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "glyphst-workspace-{label}-{}-{}.pdf",
            std::process::id(),
            SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn write_minimal_pdf(path: &Path) {
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>",
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << >> /Contents 4 0 R >>",
            "<< /Length 0 >>\nstream\n\nendstream",
        ];
        let mut bytes = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::with_capacity(objects.len());
        for (index, object) in objects.iter().enumerate() {
            offsets.push(bytes.len());
            bytes.extend_from_slice(format!("{} 0 obj\n{object}\nendobj\n", index + 1).as_bytes());
        }
        let xref = bytes.len();
        bytes.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
        bytes.extend_from_slice(b"0000000000 65535 f \n");
        for offset in offsets {
            bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        bytes.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                objects.len() + 1
            )
            .as_bytes(),
        );
        fs::write(path, bytes).unwrap();
    }

    #[test]
    fn synchronized_connection_anchors_use_facing_edge_midpoints() {
        let tab = bounds(point(px(10.0), px(20.0)), size(px(100.0), px(30.0)));
        let cases = [
            (
                point(px(500.0), px(35.0)),
                point(px(110.0), px(35.0)),
                SynchronizedConnectionAnchorEdge::Right,
            ),
            (
                point(px(-500.0), px(35.0)),
                point(px(10.0), px(35.0)),
                SynchronizedConnectionAnchorEdge::Left,
            ),
            (
                point(px(60.0), px(500.0)),
                point(px(60.0), px(50.0)),
                SynchronizedConnectionAnchorEdge::Bottom,
            ),
            (
                point(px(60.0), px(-500.0)),
                point(px(60.0), px(20.0)),
                SynchronizedConnectionAnchorEdge::Top,
            ),
        ];

        for (toward, expected_position, expected_edge) in cases {
            let anchor = Workspace::synchronized_connection_anchor(tab, toward);
            assert_eq!(anchor.position, expected_position);
            assert_eq!(anchor.edge, expected_edge);
        }
    }

    #[test]
    fn synchronized_connection_curves_leave_handles_perpendicularly() {
        let now = Instant::now();
        let horizontal = Workspace::synchronized_connection_curve(
            bounds(point(px(0.0), px(10.0)), size(px(80.0), px(30.0))),
            bounds(point(px(220.0), px(70.0)), size(px(80.0), px(30.0))),
            now,
        );
        assert_eq!(
            horizontal.start.edge,
            SynchronizedConnectionAnchorEdge::Right
        );
        assert_eq!(horizontal.end.edge, SynchronizedConnectionAnchorEdge::Left);
        assert_eq!(horizontal.control_a.y, horizontal.start.position.y);
        assert!(horizontal.control_a.x > horizontal.start.position.x);
        assert_eq!(horizontal.control_b.y, horizontal.end.position.y);
        assert!(horizontal.control_b.x < horizontal.end.position.x);

        let vertical = Workspace::synchronized_connection_curve(
            bounds(point(px(20.0), px(0.0)), size(px(80.0), px(30.0))),
            bounds(point(px(100.0), px(220.0)), size(px(80.0), px(30.0))),
            now,
        );
        assert_eq!(
            vertical.start.edge,
            SynchronizedConnectionAnchorEdge::Bottom
        );
        assert_eq!(vertical.end.edge, SynchronizedConnectionAnchorEdge::Top);
        assert_eq!(vertical.control_a.x, vertical.start.position.x);
        assert!(vertical.control_a.y > vertical.start.position.y);
        assert_eq!(vertical.control_b.x, vertical.end.position.x);
        assert!(vertical.control_b.y < vertical.end.position.y);
    }

    #[test]
    fn synchronized_connection_dots_stay_between_endpoint_handles() {
        for dot_count in [2, 7, 120] {
            let parameters = (0..dot_count)
                .map(|index| Workspace::synchronized_connection_dot_parameter(index, dot_count))
                .collect::<Vec<_>>();
            assert!(parameters.first().is_some_and(|parameter| *parameter > 0.0));
            assert!(parameters.last().is_some_and(|parameter| *parameter < 1.0));
            assert!(parameters.windows(2).all(|pair| pair[0] < pair[1]));
        }
    }

    #[test]
    fn synchronized_connection_handles_deduplicate_shared_endpoints() {
        let now = Instant::now();
        let earlier = now - Duration::from_millis(40);
        let shared = SynchronizedConnectionAnchor {
            position: point(px(80.0), px(25.0)),
            edge: SynchronizedConnectionAnchorEdge::Right,
        };
        let upper = SynchronizedConnectionAnchor {
            position: point(px(220.0), px(15.0)),
            edge: SynchronizedConnectionAnchorEdge::Left,
        };
        let lower = SynchronizedConnectionAnchor {
            position: point(px(220.0), px(85.0)),
            edge: SynchronizedConnectionAnchorEdge::Left,
        };
        let curves = [
            SynchronizedConnectionCurve {
                start: shared,
                control_a: point(px(120.0), px(25.0)),
                control_b: point(px(180.0), px(15.0)),
                end: upper,
                added_at: now,
            },
            SynchronizedConnectionCurve {
                start: shared,
                control_a: point(px(120.0), px(25.0)),
                control_b: point(px(180.0), px(85.0)),
                end: lower,
                added_at: earlier,
            },
        ];

        let handles = Workspace::synchronized_connection_handles(&curves);
        assert_eq!(handles.len(), 3);
        let shared_handle = handles
            .iter()
            .find(|handle| handle.center == shared.position)
            .unwrap();
        assert_eq!(shared_handle.added_at, earlier);
    }

    #[gpui::test]
    fn documentless_workspace_starts_on_a_new_tab(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            cx.set_global(SettingsStore::memory());
            cx.set_global(LibraryStore::memory());
            cx.set_global(RenderCache::new());
        });
        let callbacks = WorkspaceCallbacks {
            open_workspace_window: unavailable_workspace_window,
            open_settings_window: ignore_settings_window,
        };

        let (root, cx) = cx.add_window_view(|window, cx| {
            let workspace = cx.new(|cx| Workspace::new(None, None, callbacks, window, cx));
            Root::new(workspace, window, cx)
        });
        let workspace = root.read_with(cx, |root, _| {
            root.view().clone().downcast::<Workspace>().unwrap()
        });

        workspace.read_with(cx, |workspace, cx| {
            assert!(workspace.active_pane.read(cx).active_tab_is_new_tab());
        });
    }

    #[gpui::test]
    fn duplicate_tab_creates_an_independent_adjacent_view_without_stale_history(
        cx: &mut TestAppContext,
    ) {
        let document_path = temporary_pdf_path("duplicate-tab");
        write_minimal_pdf(&document_path);
        cx.update(|cx| {
            gpui_component::init(cx);
            cx.set_global(SettingsStore::memory());
            cx.set_global(LibraryStore::memory());
            cx.set_global(RenderCache::new());
        });
        let callbacks = WorkspaceCallbacks {
            open_workspace_window: unavailable_workspace_window,
            open_settings_window: ignore_settings_window,
        };
        let initial_path = document_path.clone();
        let (root, cx) = cx.add_window_view(move |window, cx| {
            let workspace = cx
                .new(|cx| Workspace::new(Some(initial_path.clone()), None, callbacks, window, cx));
            Root::new(workspace, window, cx)
        });
        let workspace = root.read_with(cx, |root, _| {
            root.view().clone().downcast::<Workspace>().unwrap()
        });
        cx.run_until_parked();

        let source_pane = workspace.read_with(cx, |workspace, _| workspace.active_pane.clone());
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.split_active_pane(SplitDirection::Right, window, cx);
        });
        cx.run_until_parked();
        workspace.update_in(cx, |workspace, window, cx| {
            let target_pane = workspace.active_pane.clone();
            let source = source_pane.read(cx).active_document_view().unwrap();
            let target = target_pane.read(cx).active_document_view().unwrap();
            assert!(
                workspace
                    .synchronized_tabs
                    .create_group(
                        source.entity_id(),
                        source_pane.entity_id(),
                        target.entity_id(),
                        target_pane.entity_id(),
                    )
                    .is_some()
            );
            workspace.active_pane = source_pane.clone();
            workspace.refresh_synchronized_tab_presentation(cx);
            workspace.duplicate_active_tab(window, cx);
        });
        let (pane, source, duplicate) = workspace.read_with(cx, |workspace, cx| {
            let pane = workspace.active_pane.clone();
            let views = pane.read(cx).document_views();
            assert_eq!(pane.read(cx).len(), 2);
            assert_eq!(pane.read(cx).active_tab_ix(), 1);
            assert_eq!(views.len(), 2);
            assert_ne!(views[0].entity_id(), views[1].entity_id());
            assert_eq!(views[0].read(cx).path(), views[1].read(cx).path());
            assert_eq!(
                views[0].read(cx).current_document_position(),
                views[1].read(cx).current_document_position()
            );
            assert!(
                workspace
                    .synchronized_tabs
                    .group_for_tab(views[0].entity_id())
                    .is_some()
            );
            assert!(
                workspace
                    .synchronized_tabs
                    .group_for_tab(views[1].entity_id())
                    .is_none()
            );
            (pane, views[0].clone(), views[1].clone())
        });

        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        assert!(cx.debug_bounds("pane-tab-copy-index-0").is_some());
        assert!(cx.debug_bounds("pane-tab-copy-index-1").is_some());

        let source_position = source.read_with(cx, |source, _| source.current_document_position());
        let duplicate_position = DocumentPosition::fixed(
            source_position.scroll_x_doc,
            source_position.scroll_y_doc,
            1.6,
            source_position.page,
        );
        duplicate.update(cx, |duplicate, cx| {
            duplicate.navigate_to_document_position(duplicate_position.clone(), cx);
        });
        assert_eq!(
            source.read_with(cx, |source, _| source.current_document_position()),
            source_position
        );
        assert_eq!(
            duplicate.read_with(cx, |duplicate, _| duplicate.current_document_position()),
            duplicate_position
        );

        workspace.update_in(cx, |workspace, window, cx| {
            workspace
                .focus_or_open_document(document_path.clone(), None, false, window, cx)
                .unwrap();
            assert_eq!(pane.read(cx).len(), 2, "normal Open must still deduplicate");
            assert_eq!(
                pane.read(cx).active_tab_id(),
                Some(duplicate.entity_id()),
                "normal Open should keep the active matching copy"
            );

            workspace.close_tab_in_pane(&pane, Some(0), window, cx);
            assert_eq!(pane.read(cx).len(), 1);
            assert_eq!(pane.read(cx).active_tab_id(), Some(duplicate.entity_id()));
            assert_eq!(
                cx.global::<LibraryStore>()
                    .history_position(&document_path)
                    .unwrap(),
                duplicate_position,
                "closing a stale copy must not overwrite the newer same-path position"
            );
        });

        fs::remove_file(document_path).unwrap();
    }

    #[gpui::test]
    fn disjoint_synchronized_groups_scroll_independently(cx: &mut TestAppContext) {
        let document_path = temporary_pdf_path("independent-synchronized-groups");
        write_minimal_pdf(&document_path);
        cx.update(|cx| {
            gpui_component::init(cx);
            cx.set_global(SettingsStore::memory());
            cx.set_global(LibraryStore::memory());
            cx.set_global(RenderCache::new());
        });
        let callbacks = WorkspaceCallbacks {
            open_workspace_window: unavailable_workspace_window,
            open_settings_window: ignore_settings_window,
        };
        let initial_path = document_path.clone();
        let (root, cx) = cx.add_window_view(move |window, cx| {
            let workspace = cx
                .new(|cx| Workspace::new(Some(initial_path.clone()), None, callbacks, window, cx));
            Root::new(workspace, window, cx)
        });
        let workspace = root.read_with(cx, |root, _| {
            root.view().clone().downcast::<Workspace>().unwrap()
        });
        cx.run_until_parked();

        let top_left = workspace.read_with(cx, |workspace, _| workspace.active_pane.clone());
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.split_active_pane(SplitDirection::Right, window, cx);
        });
        cx.run_until_parked();
        let top_right = workspace.read_with(cx, |workspace, _| workspace.active_pane.clone());
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.split_active_pane(SplitDirection::Down, window, cx);
        });
        cx.run_until_parked();
        let bottom_right = workspace.read_with(cx, |workspace, _| workspace.active_pane.clone());
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.active_pane = top_left.clone();
            workspace.split_active_pane(SplitDirection::Down, window, cx);
        });
        cx.run_until_parked();
        let bottom_left = workspace.read_with(cx, |workspace, _| workspace.active_pane.clone());
        cx.simulate_resize(size(px(900.0), px(360.0)));
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });

        workspace.update_in(cx, |workspace, window, cx| {
            let top_left_document = top_left.read(cx).active_document_view().unwrap();
            let bottom_left_document = bottom_left.read(cx).active_document_view().unwrap();
            let top_right_document = top_right.read(cx).active_document_view().unwrap();
            let bottom_right_document = bottom_right.read(cx).active_document_view().unwrap();
            let first_group = workspace
                .synchronized_tabs
                .create_group(
                    top_left_document.entity_id(),
                    top_left.entity_id(),
                    bottom_left_document.entity_id(),
                    bottom_left.entity_id(),
                )
                .unwrap();
            let second_group = workspace
                .synchronized_tabs
                .create_group(
                    top_right_document.entity_id(),
                    top_right.entity_id(),
                    bottom_right_document.entity_id(),
                    bottom_right.entity_id(),
                )
                .unwrap();
            workspace.refresh_synchronized_tab_presentation(cx);
            assert_eq!(
                workspace.synchronized_tabs.current_group(),
                Some(second_group)
            );

            let first_target_before = bottom_left_document.read(cx).current_document_position();
            let second_source_before = top_right_document.read(cx).current_document_position();
            let second_target_before = bottom_right_document.read(cx).current_document_position();
            workspace.propagate_synchronized_scroll(
                top_left_document.entity_id(),
                SynchronizedScrollDelta {
                    x_pixels: 0.0,
                    y_pixels: 48.0,
                    mode: SynchronizedScrollMode::Immediate,
                },
                cx,
            );
            let first_target_after = bottom_left_document.read(cx).current_document_position();
            assert!(first_target_after.scroll_y_doc > first_target_before.scroll_y_doc);
            assert_eq!(
                top_right_document.read(cx).current_document_position(),
                second_source_before
            );
            assert_eq!(
                bottom_right_document.read(cx).current_document_position(),
                second_target_before
            );
            assert_eq!(
                workspace.synchronized_tabs.current_group(),
                Some(second_group)
            );

            workspace.handle_synchronized_tab_click(
                &top_left,
                top_left_document.entity_id(),
                true,
                window,
                cx,
            );
            assert_eq!(
                workspace.synchronized_tabs.current_group(),
                Some(first_group)
            );
            let first_source_before = top_left_document.read(cx).current_document_position();
            let first_target_before = bottom_left_document.read(cx).current_document_position();
            workspace.propagate_synchronized_scroll(
                top_right_document.entity_id(),
                SynchronizedScrollDelta {
                    x_pixels: 0.0,
                    y_pixels: 48.0,
                    mode: SynchronizedScrollMode::Immediate,
                },
                cx,
            );
            assert!(
                bottom_right_document
                    .read(cx)
                    .current_document_position()
                    .scroll_y_doc
                    > second_target_before.scroll_y_doc
            );
            assert_eq!(
                top_left_document.read(cx).current_document_position(),
                first_source_before
            );
            assert_eq!(
                bottom_left_document.read(cx).current_document_position(),
                first_target_before
            );
            assert_eq!(
                workspace.synchronized_tabs.current_group(),
                Some(first_group)
            );
        });

        fs::remove_file(document_path).unwrap();
    }

    #[gpui::test]
    fn synchronized_connection_renders_stable_network_and_supports_both_exit_paths(
        cx: &mut TestAppContext,
    ) {
        let document_path = temporary_pdf_path("synchronized-connection");
        let other_document_path = temporary_pdf_path("synchronized-connection-other");
        write_minimal_pdf(&document_path);
        write_minimal_pdf(&other_document_path);
        cx.update(|cx| {
            gpui_component::init(cx);
            cx.set_reduce_motion(true);
            cx.set_global(SettingsStore::memory());
            cx.set_global(LibraryStore::memory());
            cx.set_global(RenderCache::new());
        });
        let callbacks = WorkspaceCallbacks {
            open_workspace_window: unavailable_workspace_window,
            open_settings_window: ignore_settings_window,
        };
        let initial_path = document_path.clone();
        let (root, cx) = cx.add_window_view(move |window, cx| {
            let workspace = cx
                .new(|cx| Workspace::new(Some(initial_path.clone()), None, callbacks, window, cx));
            Root::new(workspace, window, cx)
        });
        let workspace = root.read_with(cx, |root, _| {
            root.view().clone().downcast::<Workspace>().unwrap()
        });
        cx.run_until_parked();
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.split_active_pane(SplitDirection::Right, window, cx);
        });
        cx.run_until_parked();
        workspace.update_in(cx, |workspace, window, cx| {
            workspace
                .focus_or_open_document(other_document_path.clone(), None, false, window, cx)
                .unwrap();
        });
        cx.run_until_parked();
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.split_active_pane(SplitDirection::Right, window, cx);
        });
        cx.run_until_parked();
        cx.simulate_resize(size(px(1200.0), px(300.0)));
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });

        workspace.update_in(cx, |workspace, window, cx| {
            let panes = workspace.pane_group.panes();
            assert_eq!(panes.len(), 3);
            let source = panes[0].read(cx).active_document_view().unwrap();
            let target = panes[1].read(cx).active_document_view().unwrap();
            let group_id = workspace
                .synchronized_tabs
                .create_group(
                    source.entity_id(),
                    panes[0].entity_id(),
                    target.entity_id(),
                    panes[1].entity_id(),
                )
                .unwrap();
            workspace.refresh_synchronized_tab_presentation(cx);
            panes[1].update(cx, |pane, cx| pane.open_new_tab(window, cx));
            workspace.handle_synchronized_tab_click(
                &panes[0],
                source.entity_id(),
                true,
                window,
                cx,
            );
            assert_eq!(panes[1].read(cx).active_tab_id(), Some(target.entity_id()));
            let represented_document = panes[1]
                .read(cx)
                .tab_ids()
                .into_iter()
                .find(|tab_id| {
                    *tab_id != target.entity_id() && panes[1].read(cx).tab_is_document(*tab_id)
                })
                .unwrap();
            let grouped_candidate = panes[2].read(cx).active_document_view().unwrap();
            workspace
                .synchronized_tabs
                .create_group(
                    represented_document,
                    panes[1].entity_id(),
                    grouped_candidate.entity_id(),
                    panes[2].entity_id(),
                )
                .unwrap();
            workspace.start_connecting_synchronized_group(
                panes[0].clone(),
                source.entity_id(),
                group_id,
                cx,
            );
        });
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        assert!(cx.debug_bounds("synchronized-connection-hint").is_some());
        let original_edge = workspace.read_with(cx, |workspace, cx| {
            let connection = workspace.synchronized_connection.as_ref().unwrap();
            assert_eq!(connection.edges.len(), 1);
            let panes = workspace.pane_group.panes();
            let source = panes[0].read(cx).active_document_view().unwrap();
            let target = panes[1].read(cx).active_document_view().unwrap();
            assert_eq!(
                panes[0]
                    .read(cx)
                    .synchronized_connection_tab_state(source.entity_id()),
                Some(SynchronizedConnectionTabState::Source)
            );
            assert_eq!(
                panes[1]
                    .read(cx)
                    .synchronized_connection_tab_state(target.entity_id()),
                Some(SynchronizedConnectionTabState::Member)
            );
            let non_document = panes[1]
                .read(cx)
                .tab_ids()
                .into_iter()
                .find(|tab_id| !panes[1].read(cx).tab_is_document(*tab_id))
                .unwrap();
            assert_eq!(
                panes[1]
                    .read(cx)
                    .synchronized_connection_tab_state(non_document),
                Some(SynchronizedConnectionTabState::Invalid(
                    SynchronizedConnectionInvalidReason::NonDocument
                ))
            );
            let represented_document = panes[1]
                .read(cx)
                .tab_ids()
                .into_iter()
                .find(|tab_id| {
                    *tab_id != target.entity_id() && panes[1].read(cx).tab_is_document(*tab_id)
                })
                .unwrap();
            let other_group_number = workspace
                .synchronized_tabs
                .summary_for_tab(represented_document)
                .unwrap()
                .display_number;
            assert_eq!(
                panes[1]
                    .read(cx)
                    .synchronized_connection_tab_state(represented_document),
                Some(SynchronizedConnectionTabState::Invalid(
                    SynchronizedConnectionInvalidReason::AlreadyInGroup {
                        group_number: other_group_number
                    }
                ))
            );
            let grouped_candidate = panes[2].read(cx).active_document_view().unwrap();
            assert_eq!(
                panes[2]
                    .read(cx)
                    .synchronized_connection_tab_state(grouped_candidate.entity_id()),
                Some(SynchronizedConnectionTabState::Invalid(
                    SynchronizedConnectionInvalidReason::AlreadyInGroup {
                        group_number: other_group_number
                    }
                ))
            );
            connection.edges[0]
        });

        workspace.update_in(cx, |workspace, _, cx| {
            let panes = workspace.pane_group.panes();
            let target = panes[1].read(cx).active_document_view().unwrap();
            let represented_document = panes[1]
                .read(cx)
                .tab_ids()
                .into_iter()
                .find(|tab_id| {
                    *tab_id != target.entity_id() && panes[1].read(cx).tab_is_document(*tab_id)
                })
                .unwrap();
            workspace
                .synchronized_tabs
                .leave_group(represented_document);
            workspace.refresh_synchronized_tab_presentation(cx);
        });
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        workspace.read_with(cx, |workspace, cx| {
            let panes = workspace.pane_group.panes();
            let connection = workspace.synchronized_connection.as_ref().unwrap();
            let group_number = match connection.group {
                SynchronizedConnectionGroup::Existing(group_id) => {
                    workspace
                        .synchronized_tabs
                        .group_summary(group_id)
                        .unwrap()
                        .display_number
                }
                SynchronizedConnectionGroup::New(_) => unreachable!(),
            };
            let target = panes[1].read(cx).active_document_view().unwrap();
            let represented_document = panes[1]
                .read(cx)
                .tab_ids()
                .into_iter()
                .find(|tab_id| {
                    *tab_id != target.entity_id() && panes[1].read(cx).tab_is_document(*tab_id)
                })
                .unwrap();
            assert_eq!(
                panes[1]
                    .read(cx)
                    .synchronized_connection_tab_state(represented_document),
                Some(SynchronizedConnectionTabState::Invalid(
                    SynchronizedConnectionInvalidReason::PaneAlreadyRepresented { group_number }
                ))
            );
            let eligible = panes[2].read(cx).active_document_view().unwrap();
            assert_eq!(
                panes[2]
                    .read(cx)
                    .synchronized_connection_tab_state(eligible.entity_id()),
                Some(SynchronizedConnectionTabState::Eligible)
            );
        });

        let eligible_bounds = workspace.read_with(cx, |workspace, cx| {
            let pane = &workspace.pane_group.panes()[2];
            let eligible = pane.read(cx).active_document_view().unwrap();
            pane.read(cx).tab_bounds(eligible.entity_id()).unwrap()
        });
        cx.simulate_mouse_move(eligible_bounds.center(), None, Modifiers::default());
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        workspace.read_with(cx, |workspace, cx| {
            assert!(matches!(
                workspace.hovered_synchronized_connection_tab(cx),
                Some((SynchronizedConnectionTabState::Eligible, _))
            ));
        });
        cx.simulate_click(eligible_bounds.center(), Modifiers::default());
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        workspace.read_with(cx, |workspace, cx| {
            let connection = workspace.synchronized_connection.as_ref().unwrap();
            assert_eq!(connection.edges.len(), 2);
            assert!(connection.edges.contains(&original_edge));
            let pane = &workspace.pane_group.panes()[2];
            let added = pane.read(cx).active_document_view().unwrap();
            assert_eq!(
                pane.read(cx)
                    .synchronized_connection_tab_state(added.entity_id()),
                Some(SynchronizedConnectionTabState::Member)
            );
            let group_id = match connection.group {
                SynchronizedConnectionGroup::Existing(group_id) => group_id,
                SynchronizedConnectionGroup::New(_) => unreachable!(),
            };
            assert_eq!(
                workspace
                    .synchronized_tabs
                    .group_summary(group_id)
                    .unwrap()
                    .member_count,
                3
            );
        });

        cx.simulate_keystrokes("escape");
        workspace.read_with(cx, |workspace, _| {
            assert!(workspace.synchronized_connection.is_none());
            assert!(workspace.fading_synchronized_connection.is_none());
        });

        cx.update(|_, cx| cx.set_reduce_motion(false));
        workspace.update_in(cx, |workspace, _, cx| {
            let group_id = workspace.synchronized_tabs.current_group().unwrap();
            let source_pane = workspace.pane_group.panes()[0].clone();
            let source_tab = source_pane
                .read(cx)
                .active_document_view()
                .unwrap()
                .entity_id();
            workspace.start_connecting_synchronized_group(source_pane, source_tab, group_id, cx);
        });
        let position = point(px(30.0), px(120.0));
        cx.simulate_mouse_down(position, MouseButton::Right, Modifiers::default());
        workspace.read_with(cx, |workspace, _| {
            assert!(workspace.synchronized_connection.is_none());
            assert!(workspace.fading_synchronized_connection.is_some());
        });

        workspace.update_in(cx, |workspace, window, cx| {
            let panes = workspace.pane_group.panes();
            let source = panes[0].read(cx).active_document_view().unwrap();
            let target = panes[1].read(cx).active_document_view().unwrap();
            let before = target.read(cx).current_document_position();
            workspace.propagate_synchronized_scroll(
                source.entity_id(),
                SynchronizedScrollDelta {
                    x_pixels: 0.0,
                    y_pixels: 48.0,
                    mode: SynchronizedScrollMode::Immediate,
                },
                cx,
            );
            let after = target.read(cx).current_document_position();
            assert!(after.scroll_y_doc > before.scroll_y_doc);

            assert!(
                workspace
                    .synchronized_tabs
                    .set_paused(target.entity_id(), true)
            );
            workspace.refresh_synchronized_tab_presentation(cx);
            let paused_target = target.read(cx).current_document_position();
            workspace.propagate_synchronized_scroll(
                source.entity_id(),
                SynchronizedScrollDelta {
                    x_pixels: 0.0,
                    y_pixels: 48.0,
                    mode: SynchronizedScrollMode::Immediate,
                },
                cx,
            );
            assert_eq!(target.read(cx).current_document_position(), paused_target);

            assert!(
                workspace
                    .synchronized_tabs
                    .set_paused(target.entity_id(), false)
            );
            workspace.refresh_synchronized_tab_presentation(cx);
            assert_eq!(target.read(cx).current_document_position(), paused_target);
            workspace.propagate_synchronized_scroll(
                source.entity_id(),
                SynchronizedScrollDelta {
                    x_pixels: 0.0,
                    y_pixels: 48.0,
                    mode: SynchronizedScrollMode::Immediate,
                },
                cx,
            );
            let resumed_target = target.read(cx).current_document_position();
            assert!(resumed_target.scroll_y_doc > paused_target.scroll_y_doc);

            assert!(
                workspace
                    .synchronized_tabs
                    .set_paused(source.entity_id(), true)
            );
            workspace.refresh_synchronized_tab_presentation(cx);
            workspace.propagate_synchronized_scroll(
                source.entity_id(),
                SynchronizedScrollDelta {
                    x_pixels: 0.0,
                    y_pixels: 48.0,
                    mode: SynchronizedScrollMode::Immediate,
                },
                cx,
            );
            assert_eq!(target.read(cx).current_document_position(), resumed_target);
            assert!(
                workspace
                    .synchronized_tabs
                    .set_paused(source.entity_id(), false)
            );
            workspace.refresh_synchronized_tab_presentation(cx);

            panes[1].update(cx, |pane, cx| pane.open_new_tab(window, cx));
            let hidden_before = target.read(cx).current_document_position();
            workspace.propagate_synchronized_scroll(
                source.entity_id(),
                SynchronizedScrollDelta {
                    x_pixels: 0.0,
                    y_pixels: 48.0,
                    mode: SynchronizedScrollMode::Immediate,
                },
                cx,
            );
            assert_eq!(target.read(cx).current_document_position(), hidden_before);
        });

        workspace.update_in(cx, |workspace, window, cx| {
            let panes = workspace.pane_group.panes();
            let source_pane = panes[0].clone();
            let target_pane = panes[1].clone();
            let group_id = workspace.synchronized_tabs.current_group().unwrap();
            let target_tab = workspace
                .synchronized_tabs
                .member_tabs(group_id)
                .into_iter()
                .find(|tab_id| target_pane.read(cx).has_tab_id(*tab_id))
                .unwrap();
            let target_ix = target_pane
                .read(cx)
                .tab_ix_for_entity_id(target_tab)
                .unwrap();
            workspace.close_tab_in_pane(&source_pane, None, window, cx);
            workspace.close_tab_in_pane(&target_pane, Some(target_ix), window, cx);
            assert!(workspace.synchronized_tabs.current_group().is_none());
        });
        fs::remove_file(document_path).unwrap();
        fs::remove_file(other_document_path).unwrap();
    }

    #[gpui::test]
    fn escape_closes_the_document_browser_after_a_blank_area_click(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            cx.set_global(SettingsStore::memory());
            cx.set_global(LibraryStore::memory());
            cx.set_global(RenderCache::new());
        });
        let callbacks = WorkspaceCallbacks {
            open_workspace_window: unavailable_workspace_window,
            open_settings_window: ignore_settings_window,
        };

        let (root, cx) = cx.add_window_view(|window, cx| {
            let workspace = cx.new(|cx| Workspace::new(None, None, callbacks, window, cx));
            Root::new(workspace, window, cx)
        });
        let workspace = root.read_with(cx, |root, _| {
            root.view().clone().downcast::<Workspace>().unwrap()
        });

        workspace.update_in(cx, |workspace, window, cx| {
            workspace.open_file(window, cx);
        });
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        workspace.read_with(cx, |workspace, _| {
            assert!(workspace.document_browser.is_some());
        });
        let browser_bounds = cx.debug_bounds("document-browser").unwrap();
        let blank_footer_area = point(
            browser_bounds.right() - px(80.0),
            browser_bounds.bottom() - px(20.0),
        );
        cx.simulate_click(blank_footer_area, Modifiers::default());
        cx.simulate_keystrokes("escape");
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        workspace.read_with(cx, |workspace, _| {
            assert!(workspace.document_browser.is_none());
        });
        assert!(cx.debug_bounds("document-browser").is_none());
    }

    #[gpui::test]
    fn document_browser_resize_persists_and_temporary_window_clamps_restore(
        cx: &mut TestAppContext,
    ) {
        cx.update(|cx| {
            gpui_component::init(cx);
            cx.set_reduce_motion(true);
            cx.set_global(SettingsStore::memory());
            cx.set_global(LibraryStore::memory());
            cx.set_global(RenderCache::new());
        });
        let callbacks = WorkspaceCallbacks {
            open_workspace_window: unavailable_workspace_window,
            open_settings_window: ignore_settings_window,
        };

        let (root, cx) = cx.add_window_view(|window, cx| {
            let workspace = cx.new(|cx| Workspace::new(None, None, callbacks, window, cx));
            Root::new(workspace, window, cx)
        });
        let workspace = root.read_with(cx, |root, _| {
            root.view().clone().downcast::<Workspace>().unwrap()
        });
        cx.simulate_resize(size(px(1200.0), px(800.0)));
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.open_file(window, cx);
        });
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });

        let initial = cx.debug_bounds("document-browser").unwrap();
        assert_eq!(initial.size, size(px(680.0), px(415.0)));
        let handle = cx
            .debug_bounds("document-browser-resize-corner")
            .unwrap()
            .center();
        let resized_pointer = point(handle.x + px(24.0), handle.y + px(20.0));
        cx.simulate_mouse_down(handle, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_move(
            point(handle.x + px(8.0), handle.y + px(8.0)),
            MouseButton::Left,
            Modifiers::default(),
        );
        cx.simulate_mouse_move(resized_pointer, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_up(resized_pointer, MouseButton::Left, Modifiers::default());
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });

        let resized = cx.debug_bounds("document-browser").unwrap();
        assert_eq!(resized.size, size(px(728.0), px(455.0)));
        assert_eq!(resized.center(), initial.center());
        cx.update(|_, cx| {
            assert_eq!(
                cx.global::<LibraryStore>().document_browser_size(),
                Some(crate::library::DocumentBrowserSize {
                    width: 728.0,
                    height: 455.0,
                })
            );
        });

        cx.simulate_resize(size(px(700.0), px(440.0)));
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        assert_eq!(
            cx.debug_bounds("document-browser").unwrap().size,
            size(px(668.0), px(408.0))
        );

        cx.simulate_resize(size(px(1200.0), px(800.0)));
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        assert_eq!(
            cx.debug_bounds("document-browser").unwrap().size,
            size(px(728.0), px(455.0))
        );
    }

    #[gpui::test]
    fn dock_requests_are_preserved_while_documentless_tabs_suspend_them(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            cx.set_global(SettingsStore::memory());
            cx.set_global(LibraryStore::memory());
            cx.set_global(RenderCache::new());
        });
        let callbacks = WorkspaceCallbacks {
            open_workspace_window: unavailable_workspace_window,
            open_settings_window: ignore_settings_window,
        };

        let (root, cx) = cx.add_window_view(|window, cx| {
            let workspace = cx.new(|cx| Workspace::new(None, None, callbacks, window, cx));
            Root::new(workspace, window, cx)
        });
        let workspace = root.read_with(cx, |root, _| {
            root.view().clone().downcast::<Workspace>().unwrap()
        });
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.toggle_left_sidebar(DocumentSidebar::Toc, window, cx);
            workspace.toggle_ai_dock(window, cx);
            workspace.open_new_tab(window, cx);
        });
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });

        workspace.read_with(cx, |workspace, _| {
            assert_eq!(workspace.docks.left_sidebar, Some(DocumentSidebar::Toc));
            assert!(workspace.docks.ai_panel_open);
        });
        assert!(cx.debug_bounds("workspace-left-dock").is_none());
        assert!(cx.debug_bounds("workspace-ai-dock").is_none());
    }

    #[gpui::test]
    fn dock_widths_stay_fixed_across_window_resizes_and_restore_after_constraint(
        cx: &mut TestAppContext,
    ) {
        let document_path = temporary_pdf_path("dock-widths");
        write_minimal_pdf(&document_path);
        cx.update(|cx| {
            gpui_component::init(cx);
            cx.set_reduce_motion(true);
            cx.set_global(SettingsStore::memory());
            cx.set_global(LibraryStore::memory());
            cx.set_global(RenderCache::new());
        });
        let callbacks = WorkspaceCallbacks {
            open_workspace_window: unavailable_workspace_window,
            open_settings_window: ignore_settings_window,
        };

        let initial_path = document_path.clone();
        let (root, cx) = cx.add_window_view(move |window, cx| {
            let workspace = cx
                .new(|cx| Workspace::new(Some(initial_path.clone()), None, callbacks, window, cx));
            Root::new(workspace, window, cx)
        });
        let workspace = root.read_with(cx, |root, _| {
            root.view().clone().downcast::<Workspace>().unwrap()
        });
        cx.run_until_parked();
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.docks.preferred_widths.toc = 280.0;
            workspace.docks.preferred_widths.ai = 360.0;
            workspace.toggle_left_sidebar(DocumentSidebar::Toc, window, cx);
            workspace.toggle_ai_dock(window, cx);
        });

        cx.simulate_resize(size(px(800.0), px(700.0)));
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        assert_eq!(
            cx.debug_bounds("workspace-left-dock").unwrap().size.width,
            px(280.0)
        );
        assert_eq!(
            cx.debug_bounds("workspace-ai-dock").unwrap().size.width,
            px(360.0)
        );
        let navigation = cx.debug_bounds("workspace-left-navigation").unwrap();
        let pane_column = cx.debug_bounds("workspace-pane-column").unwrap();
        let pane_area = cx.debug_bounds("workspace-dock-center").unwrap();
        let footbar = cx.debug_bounds("workspace-pane-footbar").unwrap();
        let ai = cx.debug_bounds("workspace-ai-dock").unwrap();
        assert!(cx.debug_bounds("workspace-main-card").is_none());
        assert_eq!(pane_column.left() - navigation.right(), px(4.0));
        assert_eq!(ai.left() - pane_column.right(), px(4.0));
        assert_eq!(footbar.top() - pane_area.bottom(), px(4.0));
        assert_eq!(footbar.left(), pane_area.left());
        assert_eq!(footbar.right(), pane_area.right());

        cx.simulate_resize(size(px(1_600.0), px(900.0)));
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        assert_eq!(
            cx.debug_bounds("workspace-left-dock").unwrap().size.width,
            px(280.0)
        );
        assert_eq!(
            cx.debug_bounds("workspace-ai-dock").unwrap().size.width,
            px(360.0)
        );

        cx.simulate_resize(size(px(500.0), px(700.0)));
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        let constrained_left = cx
            .debug_bounds("workspace-left-dock")
            .unwrap()
            .size
            .width
            .as_f32();
        let constrained_right = cx
            .debug_bounds("workspace-ai-dock")
            .unwrap()
            .size
            .width
            .as_f32();
        assert!(constrained_left < 280.0);
        assert!(constrained_right < 360.0);
        // Insets, the 44 px tool rail, and two 4 px surface gutters remain fixed.
        assert!((constrained_left + constrained_right - 432.0).abs() < 0.1);
        workspace.read_with(cx, |workspace, _| {
            assert_eq!(workspace.docks.preferred_widths.toc, 280.0);
            assert_eq!(workspace.docks.preferred_widths.ai, 360.0);
        });

        cx.simulate_resize(size(px(1_600.0), px(900.0)));
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        assert_eq!(
            cx.debug_bounds("workspace-left-dock").unwrap().size.width,
            px(280.0)
        );
        assert_eq!(
            cx.debug_bounds("workspace-ai-dock").unwrap().size.width,
            px(360.0)
        );
        fs::remove_file(document_path).unwrap();
    }

    #[gpui::test]
    fn double_clicking_dock_handles_restores_each_default_width(cx: &mut TestAppContext) {
        let document_path = temporary_pdf_path("dock-double-click");
        write_minimal_pdf(&document_path);
        cx.update(|cx| {
            gpui_component::init(cx);
            cx.set_reduce_motion(true);
            cx.set_global(SettingsStore::memory());
            cx.set_global(LibraryStore::memory());
            cx.set_global(RenderCache::new());
        });
        let callbacks = WorkspaceCallbacks {
            open_workspace_window: unavailable_workspace_window,
            open_settings_window: ignore_settings_window,
        };

        let initial_path = document_path.clone();
        let (root, cx) = cx.add_window_view(move |window, cx| {
            let workspace = cx
                .new(|cx| Workspace::new(Some(initial_path.clone()), None, callbacks, window, cx));
            Root::new(workspace, window, cx)
        });
        let workspace = root.read_with(cx, |root, _| {
            root.view().clone().downcast::<Workspace>().unwrap()
        });
        cx.run_until_parked();
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.docks.preferred_widths.toc = 420.0;
            workspace.docks.preferred_widths.notes = 410.0;
            workspace.docks.preferred_widths.ai = 520.0;
            workspace.toggle_left_sidebar(DocumentSidebar::Toc, window, cx);
            workspace.toggle_ai_dock(window, cx);
        });
        cx.simulate_resize(size(px(1_200.0), px(800.0)));
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });

        let left_handle = cx
            .debug_bounds("workspace-left-dock-resize-handle")
            .unwrap()
            .center();
        cx.simulate_event(MouseUpEvent {
            position: left_handle,
            modifiers: Modifiers::default(),
            button: MouseButton::Left,
            click_count: 2,
        });
        workspace.read_with(cx, |workspace, _| {
            assert_eq!(workspace.docks.preferred_widths.toc, 280.0);
            assert_eq!(workspace.docks.preferred_widths.ai, 520.0);
        });

        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        let right_handle = cx
            .debug_bounds("workspace-ai-dock-resize-handle")
            .unwrap()
            .center();
        cx.simulate_event(MouseUpEvent {
            position: right_handle,
            modifiers: Modifiers::default(),
            button: MouseButton::Left,
            click_count: 2,
        });
        workspace.read_with(cx, |workspace, _| {
            assert_eq!(workspace.docks.preferred_widths.toc, 280.0);
            assert_eq!(workspace.docks.preferred_widths.ai, 360.0);
        });

        workspace.update_in(cx, |workspace, window, cx| {
            workspace.toggle_left_sidebar(DocumentSidebar::Notes, window, cx);
        });
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        assert_eq!(
            cx.debug_bounds("workspace-left-dock").unwrap().size.width,
            px(410.0)
        );

        workspace.update_in(cx, |workspace, window, cx| {
            workspace.toggle_left_sidebar(DocumentSidebar::Toc, window, cx);
        });
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        assert_eq!(
            cx.debug_bounds("workspace-left-dock").unwrap().size.width,
            px(280.0)
        );
        fs::remove_file(document_path).unwrap();
    }

    #[gpui::test]
    fn dragging_a_dock_handle_can_exceed_half_the_window(cx: &mut TestAppContext) {
        let document_path = temporary_pdf_path("dock-drag");
        write_minimal_pdf(&document_path);
        cx.update(|cx| {
            gpui_component::init(cx);
            cx.set_reduce_motion(true);
            cx.set_global(SettingsStore::memory());
            cx.set_global(LibraryStore::memory());
            cx.set_global(RenderCache::new());
        });
        let callbacks = WorkspaceCallbacks {
            open_workspace_window: unavailable_workspace_window,
            open_settings_window: ignore_settings_window,
        };

        let initial_path = document_path.clone();
        let (root, cx) = cx.add_window_view(move |window, cx| {
            let workspace = cx
                .new(|cx| Workspace::new(Some(initial_path.clone()), None, callbacks, window, cx));
            Root::new(workspace, window, cx)
        });
        let workspace = root.read_with(cx, |root, _| {
            root.view().clone().downcast::<Workspace>().unwrap()
        });
        cx.run_until_parked();
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.toggle_left_sidebar(DocumentSidebar::Toc, window, cx);
        });
        cx.simulate_resize(size(px(1_000.0), px(700.0)));
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });

        let handle = cx
            .debug_bounds("workspace-left-dock-resize-handle")
            .unwrap()
            .center();
        cx.simulate_mouse_down(handle, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_move(
            point(px(600.0), handle.y),
            MouseButton::Left,
            Modifiers::default(),
        );
        cx.simulate_mouse_move(
            point(px(852.0), handle.y),
            MouseButton::Left,
            Modifiers::default(),
        );
        cx.simulate_mouse_up(
            point(px(852.0), handle.y),
            MouseButton::Left,
            Modifiers::default(),
        );

        workspace.read_with(cx, |workspace, _| {
            assert_eq!(workspace.docks.preferred_widths.toc, 800.0);
        });
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        assert_eq!(
            cx.debug_bounds("workspace-left-dock").unwrap().size.width,
            px(800.0)
        );
        fs::remove_file(document_path).unwrap();
    }

    #[gpui::test]
    fn translation_failures_use_component_notification_layer(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            glyphst_ui::activity::init(cx);
            cx.set_global(SettingsStore::memory());
            cx.set_global(LibraryStore::memory());
            cx.set_global(RenderCache::new());
        });
        let callbacks = WorkspaceCallbacks {
            open_workspace_window: unavailable_workspace_window,
            open_settings_window: ignore_settings_window,
        };

        let (root, cx) = cx.add_window_view(|window, cx| {
            let workspace = cx.new(|cx| Workspace::new(None, None, callbacks, window, cx));
            Root::new(workspace, window, cx)
        });
        let workspace = root.read_with(cx, |root, _| {
            root.view().clone().downcast::<Workspace>().unwrap()
        });

        workspace.update_in(cx, |workspace, _, cx| {
            workspace.document_translation_jobs.update(cx, |jobs, cx| {
                jobs.report_preflight_error(
                    "/tmp/source.pdf".into(),
                    DocumentTranslationOpenMode::Document,
                    "Invalid target language",
                    cx,
                );
            });
        });
        cx.run_until_parked();

        workspace.update_in(cx, |_, window, cx| {
            assert_eq!(window.notifications(cx).len(), 1);
        });
    }

    #[gpui::test]
    fn compact_title_bar_menus_switch_without_changing_layout(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            cx.set_global(SettingsStore::memory());
            cx.set_global(LibraryStore::memory());
            cx.set_global(RenderCache::new());
        });
        let callbacks = WorkspaceCallbacks {
            open_workspace_window: unavailable_workspace_window,
            open_settings_window: ignore_settings_window,
        };

        let (root, cx) = cx.add_window_view(|window, cx| {
            let workspace = cx.new(|cx| Workspace::new_empty(callbacks, window, cx));
            Root::new(workspace, window, cx)
        });
        let workspace = root.read_with(cx, |root, _| {
            root.view().clone().downcast::<Workspace>().unwrap()
        });
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });

        assert!(cx.debug_bounds("workspace-menu-glyphst").is_some());
        assert!(cx.debug_bounds("workspace-menu-file").is_some());
        assert!(cx.debug_bounds("workspace-menu-tab").is_some());

        let glyphst = cx.debug_bounds("workspace-menu-glyphst").unwrap().center();
        cx.simulate_click(glyphst, Modifiers::default());
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });

        assert!(cx.debug_bounds("workspace-menu-glyphst").is_some());
        assert!(cx.debug_bounds("workspace-menu-file").is_some());
        assert!(cx.debug_bounds("workspace-menu-tab").is_some());
        let glyphst_popup = workspace.read_with(cx, |workspace, _| {
            assert!(workspace.title_bar_menu_expanded);
            assert_eq!(workspace.title_bar_open_menu, Some(WorkspaceMenu::Glyphst));
            workspace.title_bar_popup_menu.as_ref().unwrap().entity_id()
        });

        workspace.update_in(cx, |workspace, window, cx| {
            workspace.hover_title_bar_menu(WorkspaceMenu::File, window, cx);
        });
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        workspace.read_with(cx, |workspace, _| {
            assert_eq!(workspace.title_bar_open_menu, Some(WorkspaceMenu::File));
            assert_ne!(
                workspace.title_bar_popup_menu.as_ref().unwrap().entity_id(),
                glyphst_popup
            );
        });

        let drag_region = cx
            .debug_bounds("workspace-title-bar-drag-region")
            .unwrap()
            .center();
        cx.simulate_click(drag_region, Modifiers::default());
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });

        assert!(cx.debug_bounds("workspace-menu-glyphst").is_some());
        workspace.read_with(cx, |workspace, _| {
            assert!(!workspace.title_bar_menu_expanded);
            assert_eq!(workspace.title_bar_open_menu, None);
        });
    }

    #[gpui::test]
    fn root_hosted_workspace_tabs_render_and_interact(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            cx.set_global(SettingsStore::memory());
            cx.set_global(LibraryStore::memory());
            cx.set_global(RenderCache::new());
        });
        let callbacks = WorkspaceCallbacks {
            open_workspace_window: unavailable_workspace_window,
            open_settings_window: ignore_settings_window,
        };

        let (root, cx) = cx.add_window_view(|window, cx| {
            let workspace = cx.new(|cx| Workspace::new(None, None, callbacks, window, cx));
            Root::new(workspace, window, cx)
        });
        let workspace = root.read_with(cx, |root, _| {
            root.view().clone().downcast::<Workspace>().unwrap()
        });
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.open_new_tab(window, cx);
        });
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });

        assert!(cx.debug_bounds("workspace-main-card").is_none());
        assert!(cx.debug_bounds("workspace-pane-column").is_some());
        assert!(cx.debug_bounds("pane-tab-0").is_some());
        assert!(cx.debug_bounds("pane-tab-1").is_some());
        assert!(cx.debug_bounds("active-pane-surface").is_none());
        let first_tab_bounds = cx.debug_bounds("pane-tab-0").unwrap();
        let second_tab_bounds = cx.debug_bounds("pane-tab-1").unwrap();
        assert_eq!(second_tab_bounds.left() - first_tab_bounds.right(), px(2.0));

        let first_tab = cx.debug_bounds("pane-tab-0").unwrap().center();
        cx.simulate_click(first_tab, Modifiers::default());
        workspace.read_with(cx, |workspace, cx| {
            assert_eq!(workspace.active_pane.read(cx).active_tab_ix(), 0);
        });

        let new_tab = cx.debug_bounds("pane-new-tab").unwrap().center();
        cx.simulate_click(new_tab, Modifiers::default());
        workspace.read_with(cx, |workspace, cx| {
            let pane = workspace.active_pane.read(cx);
            assert_eq!(pane.len(), 3);
            assert_eq!(pane.active_tab_ix(), 2);
        });
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });

        let close_tab = cx.debug_bounds("pane-tab-close-2").unwrap().center();
        cx.simulate_click(close_tab, Modifiers::default());
        workspace.read_with(cx, |workspace, cx| {
            assert_eq!(workspace.active_pane.read(cx).len(), 2);
        });
    }

    #[gpui::test]
    fn split_pane_cards_keep_nested_gutters_and_move_the_active_border(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            cx.set_global(SettingsStore::memory());
            cx.set_global(LibraryStore::memory());
            cx.set_global(RenderCache::new());
        });
        let callbacks = WorkspaceCallbacks {
            open_workspace_window: unavailable_workspace_window,
            open_settings_window: ignore_settings_window,
        };

        let (root, cx) = cx.add_window_view(|window, cx| {
            let workspace = cx.new(|cx| Workspace::new(None, None, callbacks, window, cx));
            Root::new(workspace, window, cx)
        });
        let workspace = root.read_with(cx, |root, _| {
            root.view().clone().downcast::<Workspace>().unwrap()
        });
        let top_left = workspace.read_with(cx, |workspace, _| workspace.active_pane.clone());
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.split_active_pane(SplitDirection::Right, window, cx);
        });
        let top_right = workspace.read_with(cx, |workspace, _| workspace.active_pane.clone());
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.split_active_pane(SplitDirection::Down, window, cx);
        });
        let bottom_right = workspace.read_with(cx, |workspace, _| workspace.active_pane.clone());
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.active_pane = top_left.clone();
            workspace.split_active_pane(SplitDirection::Down, window, cx);
        });
        let bottom_left = workspace.read_with(cx, |workspace, _| workspace.active_pane.clone());
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });

        let (top_left_bounds, top_right_bounds, bottom_left_bounds, bottom_right_bounds) =
            workspace.read_with(cx, |workspace, _| {
                let bounds = |pane| {
                    workspace
                        .pane_group
                        .bounding_box_for_pane(pane)
                        .expect("pane bounds")
                };
                (
                    bounds(&top_left),
                    bounds(&top_right),
                    bounds(&bottom_left),
                    bounds(&bottom_right),
                )
            });
        assert_eq!(top_right_bounds.left() - top_left_bounds.right(), px(4.0));
        assert_eq!(
            bottom_right_bounds.left() - bottom_left_bounds.right(),
            px(4.0)
        );
        assert_eq!(bottom_left_bounds.top() - top_left_bounds.bottom(), px(4.0));
        assert_eq!(
            bottom_right_bounds.top() - top_right_bounds.bottom(),
            px(4.0)
        );
        workspace.read_with(cx, |workspace, _| {
            assert_eq!(
                workspace
                    .pane_group
                    .find_pane_in_direction(&top_left, SplitDirection::Right),
                Some(top_right.clone())
            );
            assert_eq!(
                workspace
                    .pane_group
                    .find_pane_in_direction(&top_right, SplitDirection::Left),
                Some(top_left.clone())
            );
            assert_eq!(
                workspace
                    .pane_group
                    .find_pane_in_direction(&top_left, SplitDirection::Down),
                Some(bottom_left.clone())
            );
        });

        let active_surface = cx
            .debug_bounds("active-pane-surface")
            .expect("active pane surface");
        assert_eq!(active_surface, bottom_left_bounds);

        cx.simulate_click(top_right_bounds.center(), Modifiers::default());
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        workspace.read_with(cx, |workspace, _| {
            assert_eq!(workspace.active_pane, top_right);
        });

        let active_surface = cx
            .debug_bounds("active-pane-surface")
            .expect("clicked pane surface");
        assert_eq!(active_surface, top_right_bounds);
    }

    #[gpui::test]
    fn open_sidebar_survives_document_tab_changes(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            cx.set_reduce_motion(true);
            cx.set_global(SettingsStore::memory());
            cx.set_global(LibraryStore::memory());
            cx.set_global(RenderCache::new());
        });
        let callbacks = WorkspaceCallbacks {
            open_workspace_window: unavailable_workspace_window,
            open_settings_window: ignore_settings_window,
        };
        let first_path = temporary_pdf_path("sidebar-first");
        let second_path = temporary_pdf_path("sidebar-second");
        write_minimal_pdf(&first_path);
        write_minimal_pdf(&second_path);

        let (root, cx) = cx.add_window_view(|window, cx| {
            let workspace = cx.new(|cx| Workspace::new(None, None, callbacks, window, cx));
            Root::new(workspace, window, cx)
        });
        let workspace = root.read_with(cx, |root, _| {
            root.view().clone().downcast::<Workspace>().unwrap()
        });
        workspace.update_in(cx, |workspace, window, cx| {
            workspace
                .focus_or_open_document(first_path.clone(), None, true, window, cx)
                .unwrap();
        });
        cx.run_until_parked();
        workspace.update_in(cx, |workspace, window, cx| {
            workspace
                .focus_or_open_document(second_path.clone(), None, false, window, cx)
                .unwrap();
        });
        cx.run_until_parked();

        workspace.update_in(cx, |workspace, window, cx| {
            workspace.toggle_left_sidebar(DocumentSidebar::Toc, window, cx);
        });
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        assert!(cx.debug_bounds("toc-sidebar-content").is_some());

        let first_tab = cx.debug_bounds("pane-tab-0").unwrap().center();
        cx.simulate_click(first_tab, Modifiers::default());
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        workspace.read_with(cx, |workspace, cx| {
            assert_eq!(workspace.docks.left_sidebar, Some(DocumentSidebar::Toc));
            assert_eq!(workspace.active_pane.read(cx).active_tab_ix(), 0);
        });
        assert!(cx.debug_bounds("toc-sidebar-content").is_some());

        let second_tab = cx.debug_bounds("pane-tab-1").unwrap().center();
        cx.simulate_click(second_tab, Modifiers::default());
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        workspace.read_with(cx, |workspace, cx| {
            assert_eq!(workspace.docks.left_sidebar, Some(DocumentSidebar::Toc));
            assert_eq!(workspace.active_pane.read(cx).active_tab_ix(), 1);
        });
        assert!(cx.debug_bounds("toc-sidebar-content").is_some());

        fs::remove_file(first_path).unwrap();
        fs::remove_file(second_path).unwrap();
    }
}
