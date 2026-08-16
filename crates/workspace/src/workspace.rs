use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use anyhow::Result;
use glyphst_ai_ui::{AiContextProvider as _, AiPanel, AiPanelEvent};
use glyphst_translation_ui::service::LanguageCode;
use glyphst_translation_ui::{
    DocumentTranslationJobEvent, DocumentTranslationJobId, DocumentTranslationJobManager,
    DocumentTranslationNotification, DocumentTranslationSessionNotification,
    PdfTranslationSessionCacheKey, PdfTranslationSessionRequest, document_translation_notification,
    document_translation_session_notification,
};
use glyphst_ui::{
    activity::ActivityCenter,
    root::render_overlays,
    style::{apply_window_background, shell_background},
};
use gpui::{
    AnyElement, App, Bounds, Context, DismissEvent, DragMoveEvent, Entity, EntityId, ExternalPaths,
    FocusHandle, Focusable, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, PathPromptOptions, Pixels, Point, PromptButton, PromptLevel, Render,
    SharedString, WeakEntity, Window, WindowAppearance, bounds, canvas, div, fill, point,
    prelude::*, px, size,
};
use gpui_component::{
    ActiveTheme as _, ThemeColor, WindowExt as _, menu::PopupMenu, notification::Notification,
};

mod ai;
mod chrome;
mod docks;

pub use glyphst_actions::workspace::{
    ActivateNextPane, ActivatePaneDown, ActivatePaneLeft, ActivatePaneRight, ActivatePaneUp,
    ActivatePreviousPane, CloseOtherPanes, MovePaneDown, MovePaneLeft, MovePaneRight, MovePaneUp,
    ResetPaneSizes, ResizePaneDown, ResizePaneLeft, ResizePaneRight, ResizePaneUp, SwapPaneDown,
    SwapPaneLeft, SwapPaneRight, SwapPaneUp, ToggleMode, ToggleThemeSelector, ToggleZenMode,
};

use crate::{
    command_palette::{CommandPalette, CommandPaletteDelegate},
    document_browser::{DocumentBrowser, DocumentBrowserEvent},
    document_view::{
        CloseTab, DocumentSidebar, DocumentView, DocumentViewEvent, DuplicateTab, OpenFile,
        OpenKeybindings, OpenNewTab, OpenQuickAsk, OpenSettings, OpenWithSystemPicker, ResetZoom,
        ToggleAiPanel, ToggleCommandPalette, ToggleHome, ToggleNotes, ToggleThumbnailSidebar,
        ToggleToc, ToggleTransparency, TranslationViewportAnchor, ZoomIn, ZoomOut,
    },
    external_drop::{
        ExternalDocumentDropFeedback, classify_external_document_paths,
        render_external_document_drop_overlay,
    },
    library::{
        DocumentPosition, ReaderSidebar, ReaderSynchronizedGroup, ReaderTranslationSession,
        ReaderWindowBounds, ReaderWindowSession, ReaderWindowState, normalize_path,
    },
    new_tab::{NewTab, NewTabEvent},
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

use ai::WorkspaceAiContextProvider;
use chrome::{WorkspaceMenu, WorkspaceTitleBar, build_popup_menu};
use docks::WorkspaceDockState;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WorkspaceSurfaceMode {
    Home,
    #[default]
    Panes,
}

pub struct Workspace {
    pane_group: PaneGroup,
    active_pane: Entity<Pane>,
    surface_mode: WorkspaceSurfaceMode,
    zen_mode: bool,
    home_view: Entity<NewTab>,
    home_previous_focus_handle: Option<FocusHandle>,
    callbacks: WorkspaceCallbacks,
    window_close_handler: Option<fn(&Entity<Workspace>, &mut Window, &mut App)>,
    focus_handle: FocusHandle,
    document_translation_jobs: Entity<DocumentTranslationJobManager>,
    pending_translation_splits: Vec<PendingTranslationSplit>,
    live_translation_pairs: HashMap<DocumentTranslationJobId, LiveTranslationPair>,
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
    external_document_drop_feedback: Option<ExternalDocumentDropFeedback>,
    synchronized_tabs: SynchronizedTabRegistry,
    restored_tab_ids: HashMap<u64, (EntityId, EntityId)>,
    pending_restored_documents: usize,
    pending_dock_document_tab: Option<u64>,
    pending_synchronized_groups: Vec<ReaderSynchronizedGroup>,
    pending_restored_translation_sessions: Vec<ReaderTranslationSession>,
    restored_missing_documents: usize,
    synchronized_connection: Option<SynchronizedConnection>,
    fading_synchronized_connection: Option<FadingSynchronizedConnection>,
    document_context_mru: Vec<EntityId>,
    ai_panel: Entity<AiPanel>,
    quick_ask: Option<Entity<AiPanel>>,
    quick_ask_visible: bool,
    quick_ask_previous_focus: Option<FocusHandle>,
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

struct ExternalDocumentDropNotification;

struct LiveTranslationPair {
    source_id: EntityId,
    translated_id: EntityId,
    source: WeakEntity<DocumentView>,
    translated: WeakEntity<DocumentView>,
    active: bool,
}

fn live_translation_pair_is_active(
    source_id: EntityId,
    translated_id: EntityId,
    active_tabs: &HashSet<EntityId>,
) -> bool {
    active_tabs.contains(&source_id) && active_tabs.contains(&translated_id)
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
            None => {
                pane.update(cx, |pane, cx| pane.ensure_new_tab(window, cx));
                workspace.enter_home(window, cx);
            }
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

    pub fn from_reader_session(
        session: ReaderWindowSession,
        callbacks: WorkspaceCallbacks,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let document_translation_jobs = cx.new(DocumentTranslationJobManager::new);
        let restored = PaneGroup::restore(
            &session.pane_tree,
            document_translation_jobs.clone(),
            window,
            cx,
        );
        let first_pane = restored
            .panes
            .first()
            .map(|(_, pane)| pane.clone())
            .expect("restoring a pane tree always creates at least one pane");
        let mut workspace = Self::with_pane(
            first_pane.clone(),
            document_translation_jobs,
            callbacks,
            window,
            cx,
        );
        workspace.pane_group = restored.pane_group;
        workspace.active_pane = restored
            .panes
            .iter()
            .find(|(id, _)| *id == session.active_pane)
            .map(|(_, pane)| pane.clone())
            .unwrap_or_else(|| first_pane.clone());
        for (_, pane) in restored
            .panes
            .iter()
            .filter(|(_, pane)| *pane != first_pane)
        {
            workspace.subscribe_to_pane(pane, window, cx);
        }
        workspace.restored_tab_ids = restored
            .tab_ids
            .into_iter()
            .map(|(session_id, tab_id, pane_id)| (session_id, (tab_id, pane_id)))
            .collect();
        workspace.pending_restored_documents = restored.pending_documents;
        workspace.pending_dock_document_tab = session.dock_document_tab;
        workspace.pending_synchronized_groups = session.synchronized_groups;
        workspace.pending_restored_translation_sessions = session.translation_sessions;
        workspace.restored_missing_documents = restored.missing_documents;
        workspace.docks.left_sidebar = session.left_sidebar.map(|sidebar| match sidebar {
            ReaderSidebar::Thumbnails => DocumentSidebar::Thumbnails,
            ReaderSidebar::TableOfContents => DocumentSidebar::Toc,
            ReaderSidebar::Notes => DocumentSidebar::Notes,
        });
        workspace.docks.ai_panel_open = session.ai_panel_open;
        workspace.transparency_enabled = session.transparency_enabled;
        apply_window_background(
            window,
            workspace.window_settings,
            workspace.transparency_enabled,
        );
        if workspace.pending_restored_documents == 0 {
            workspace.finish_reader_session_restore(window, cx);
        }
        workspace.sync_tab_bar_visibility(cx);
        workspace.focus_active_pane(window, cx);
        workspace
    }

    fn with_pane(
        pane: Entity<Pane>,
        document_translation_jobs: Entity<DocumentTranslationJobManager>,
        callbacks: WorkspaceCallbacks,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        let home_focus_handle = cx.focus_handle();
        let home_view = cx.new(|cx| NewTab::new_home(home_focus_handle, window, cx));
        let docks = WorkspaceDockState::new(cx);
        let ai_context_provider = WorkspaceAiContextProvider::new(cx.weak_entity());
        let initial_ai_page_state = pane
            .read(cx)
            .active_document_view()
            .and_then(|document| {
                crate::document_view::DocumentViewAiContextProvider::new(&document).page_state(cx)
            })
            .unwrap_or_default();
        let ai_panel = cx.new(|cx| {
            AiPanel::new(
                focus_handle.clone(),
                ai_context_provider.clone(),
                initial_ai_page_state,
                docks.preferred_widths.ai,
                window,
                cx,
            )
        });
        let window_settings = cx.global::<SettingsStore>().settings.window;
        let transparency_enabled = window_settings.transparency_enabled_by_default();
        apply_window_background(window, window_settings, transparency_enabled);
        let workspace = Self {
            pane_group: PaneGroup::new(pane.clone()),
            active_pane: pane.clone(),
            surface_mode: WorkspaceSurfaceMode::Panes,
            zen_mode: false,
            home_view: home_view.clone(),
            home_previous_focus_handle: None,
            callbacks,
            window_close_handler: None,
            focus_handle,
            document_translation_jobs: document_translation_jobs.clone(),
            pending_translation_splits: Vec::new(),
            live_translation_pairs: HashMap::new(),
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
            external_document_drop_feedback: None,
            synchronized_tabs: SynchronizedTabRegistry::default(),
            restored_tab_ids: HashMap::new(),
            pending_restored_documents: 0,
            pending_dock_document_tab: None,
            pending_synchronized_groups: Vec::new(),
            pending_restored_translation_sessions: Vec::new(),
            restored_missing_documents: 0,
            synchronized_connection: None,
            fading_synchronized_connection: None,
            document_context_mru: Vec::new(),
            ai_panel: ai_panel.clone(),
            quick_ask: None,
            quick_ask_visible: false,
            quick_ask_previous_focus: None,
            docks,
            transparency_enabled,
            window_settings,
        };

        cx.subscribe_in(
            &home_view,
            window,
            |this, _, event: &NewTabEvent, window, cx| match event {
                NewTabEvent::OpenFile => this.open_file(window, cx),
                NewTabEvent::OpenDocument(path) => {
                    if let Err(error) =
                        this.open_document_in_active_pane(path.clone(), None, window, cx)
                    {
                        eprintln!("failed to open Home document: {error:#}");
                    }
                }
            },
        )
        .detach();

        cx.subscribe_in(
            &ai_panel,
            window,
            |this, _, event: &AiPanelEvent, window, cx| match event {
                AiPanelEvent::Dismissed => this.close_ai_dock(cx),
                AiPanelEvent::Changed => {}
                AiPanelEvent::ExternalPathsDropped(paths) => {
                    this.open_external_document_paths(paths, window, cx);
                }
            },
        )
        .detach();

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
            |_this, _, event: &DocumentTranslationJobEvent, window, cx| match event {
                DocumentTranslationJobEvent::OpenDocument { id, path } => {
                    let id = *id;
                    let path = path.clone();
                    cx.spawn_in(window, async move |this, cx| {
                        _ = this.update_in(cx, |this, window, cx| {
                            let replace_active_new_tab =
                                this.active_pane.read(cx).active_tab_is_new_tab();
                            if let Err(error) = this.focus_or_open_document_for_job(
                                path,
                                None,
                                replace_active_new_tab,
                                id,
                                window,
                                cx,
                            ) {
                                this.document_translation_jobs.update(cx, |jobs, cx| {
                                    jobs.finish_open(id, Err(format!("{error:#}").into()), cx);
                                });
                            }
                        });
                    })
                    .detach();
                }
                DocumentTranslationJobEvent::OpenSplit {
                    id,
                    source_path,
                    translated_path,
                } => {
                    let id = *id;
                    let source_path = source_path.clone();
                    let translated_path = translated_path.clone();
                    cx.spawn_in(window, async move |this, cx| {
                        _ = this.update_in(cx, |this, window, cx| {
                            if let Err(error) = this.begin_translation_split_open(
                                id,
                                source_path,
                                translated_path,
                                window,
                                cx,
                            ) {
                                this.document_translation_jobs.update(cx, |jobs, cx| {
                                    jobs.finish_open(id, Err(format!("{error:#}").into()), cx);
                                });
                            }
                        });
                    })
                    .detach();
                }
                DocumentTranslationJobEvent::ShowNotification { snapshot } => {
                    let snapshot = snapshot.clone();
                    cx.spawn_in(window, async move |this, cx| {
                        _ = this.update_in(cx, |this, window, cx| {
                            window.push_notification(
                                document_translation_notification(
                                    snapshot,
                                    this.document_translation_jobs.downgrade(),
                                ),
                                cx,
                            );
                        });
                    })
                    .detach();
                }
                DocumentTranslationJobEvent::RemoveNotification { id } => {
                    let id = id.to_string();
                    cx.spawn_in(window, async move |this, cx| {
                        _ = this.update_in(cx, |_, window, cx| {
                            window.remove_notification1::<DocumentTranslationNotification>(id, cx);
                        });
                    })
                    .detach();
                }
                DocumentTranslationJobEvent::OpenSession { id, source_path } => {
                    let id = *id;
                    let source_path = source_path.clone();
                    cx.spawn_in(window, async move |this, cx| {
                        _ = this.update_in(cx, |this, window, cx| {
                            if let Err(error) =
                                this.open_translation_session(id, &source_path, window, cx)
                            {
                                eprintln!("failed to open live translation split: {error:#}");
                                this.document_translation_jobs.update(cx, |manager, cx| {
                                    manager.set_session_backgrounded(id, cx);
                                });
                            }
                        });
                    })
                    .detach();
                }
                DocumentTranslationJobEvent::SaveSession { id } => {
                    let id = *id;
                    cx.spawn_in(window, async move |this, cx| {
                        _ = this.update_in(cx, |this, window, cx| {
                            this.save_translation_session(id, window, cx);
                        });
                    })
                    .detach();
                }
                DocumentTranslationJobEvent::OpenSessionExport { path } => {
                    let path = path.clone();
                    cx.spawn_in(window, async move |this, cx| {
                        _ = this.update_in(cx, |this, window, cx| {
                            let replace_active_new_tab =
                                this.active_pane.read(cx).active_tab_is_new_tab();
                            if let Err(error) = this.focus_or_open_document(
                                path,
                                None,
                                replace_active_new_tab,
                                window,
                                cx,
                            ) {
                                eprintln!("failed to open saved translation: {error:#}");
                            }
                        });
                    })
                    .detach();
                }
                DocumentTranslationJobEvent::ShowSessionNotification { snapshot } => {
                    let snapshot = snapshot.clone();
                    cx.spawn_in(window, async move |this, cx| {
                        _ = this.update_in(cx, |this, window, cx| {
                            window.push_notification(
                                document_translation_session_notification(
                                    snapshot,
                                    this.document_translation_jobs.downgrade(),
                                ),
                                cx,
                            );
                        });
                    })
                    .detach();
                }
                DocumentTranslationJobEvent::RemoveSessionNotification { id } => {
                    let id = id.to_string();
                    cx.spawn_in(window, async move |this, cx| {
                        _ = this.update_in(cx, |_, window, cx| {
                            window.remove_notification1::<DocumentTranslationSessionNotification>(
                                id, cx,
                            );
                        });
                    })
                    .detach();
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
            this.ai_panel
                .update(cx, |panel, cx| panel.save_current_session(cx));
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
        self.focus_or_open_document(path, None, replace_active_new_tab, window, cx)?;
        self.surface_mode = WorkspaceSurfaceMode::Panes;
        self.home_previous_focus_handle = None;
        cx.notify();
        Ok(())
    }

    fn open_external_document_paths(
        &mut self,
        paths: &[PathBuf],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let target_pane = self.active_pane.clone();
        self.open_external_document_paths_in_pane(paths, &target_pane, window, cx);
    }

    fn open_external_document_paths_in_pane(
        &mut self,
        paths: &[PathBuf],
        target_pane: &Entity<Pane>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.external_document_drop_feedback = None;
        if paths.is_empty() {
            cx.notify();
            return;
        }

        let classified = classify_external_document_paths(paths);
        let target_pane = target_pane.clone();
        let mut opened_count = 0usize;
        let mut failed_count = 0usize;
        let mut last_opened_path = None;

        if !classified.supported.is_empty() {
            self.set_title_bar_menu_expanded(false, cx);
            self.close_command_palette(window, cx);
            self.close_theme_selector(window, cx);
            self.close_document_browser(window, cx);
        }

        for path in classified.supported {
            self.active_pane = target_pane.clone();
            match self.open_document_in_active_pane(path.clone(), None, window, cx) {
                Ok(()) => {
                    opened_count += 1;
                    last_opened_path = Some(path);
                }
                Err(error) => {
                    failed_count += 1;
                    eprintln!("failed to open dropped document: {error:#}");
                }
            }
        }

        if let Some(path) = last_opened_path
            && let Some((pane, tab_ix)) = self.document_location(&path, cx)
        {
            self.active_pane = pane.clone();
            pane.update(cx, |pane, cx| {
                pane.activate_existing_document(tab_ix, None, false, window, cx);
            });
            self.surface_mode = WorkspaceSurfaceMode::Panes;
            self.home_previous_focus_handle = None;
        }

        let skipped_count = classified.rejected.len() + failed_count;
        if opened_count == 0 {
            window.push_notification(
                Notification::warning(crate::fl!("external-drop-none-message"))
                    .title(crate::fl!("external-drop-none-title"))
                    .id::<ExternalDocumentDropNotification>(),
                cx,
            );
        } else if skipped_count > 0 {
            window.push_notification(
                Notification::warning(crate::fl!(
                    "external-drop-skipped-message",
                    count = skipped_count as i64
                ))
                .title(crate::fl!("external-drop-skipped-title"))
                .id::<ExternalDocumentDropNotification>(),
                cx,
            );
        }
        cx.notify();
    }

    pub(super) fn track_external_document_drag(
        &mut self,
        event: &DragMoveEvent<ExternalPaths>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let feedback = event
            .bounds
            .contains(&event.event.position)
            .then(|| classify_external_document_paths(event.drag(cx).paths()).feedback())
            .flatten();
        if self.external_document_drop_feedback != feedback {
            self.external_document_drop_feedback = feedback;
            cx.notify();
        }
    }

    pub(super) fn external_document_drop_overlay(&self, theme: ThemeColor) -> Option<AnyElement> {
        self.external_document_drop_feedback
            .map(|feedback| render_external_document_drop_overlay(feedback, theme))
    }

    pub fn reader_session_restore_pending(&self) -> bool {
        self.pending_restored_documents > 0
    }

    /// Install host-specific work that must run before the custom Linux title
    /// bar removes this workspace window.
    pub fn set_window_close_handler(
        &mut self,
        handler: fn(&Entity<Workspace>, &mut Window, &mut App),
    ) {
        self.window_close_handler = Some(handler);
    }

    pub fn restored_missing_document_count(&self) -> usize {
        self.restored_missing_documents
    }

    pub fn reader_window_session(
        &self,
        window: &Window,
        cx: &Context<Self>,
    ) -> ReaderWindowSession {
        let panes = self.pane_group.session_snapshot(&self.active_pane, cx);
        let window_bounds = window.window_bounds();
        let state = match window_bounds {
            gpui::WindowBounds::Windowed(_) => ReaderWindowState::Windowed,
            gpui::WindowBounds::Maximized(_) => ReaderWindowState::Maximized,
            gpui::WindowBounds::Fullscreen(_) => ReaderWindowState::Fullscreen,
        };
        let bounds = window_bounds.get_bounds();
        let mut translation_sessions = self
            .live_translation_pairs
            .iter()
            .filter_map(|(session_id, pair)| {
                let source_tab = panes.runtime_tab_ids.get(&pair.source_id).copied()?;
                let translated_tab = panes.runtime_tab_ids.get(&pair.translated_id).copied()?;
                let snapshot = self
                    .document_translation_jobs
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
                .dock_document_target(cx)
                .and_then(|(_, document_view)| {
                    panes
                        .runtime_tab_ids
                        .get(&document_view.entity_id())
                        .copied()
                }),
            left_sidebar: self.docks.left_sidebar.map(|sidebar| match sidebar {
                DocumentSidebar::Thumbnails => ReaderSidebar::Thumbnails,
                DocumentSidebar::Toc => ReaderSidebar::TableOfContents,
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

    fn finish_reader_session_restore(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let groups = std::mem::take(&mut self.pending_synchronized_groups);
        for group in groups {
            let members = group
                .tabs
                .iter()
                .filter_map(|session_id| {
                    let (tab_id, pane_id) = self.restored_tab_ids.get(session_id).copied()?;
                    let is_document = self
                        .pane_group
                        .panes()
                        .into_iter()
                        .find(|pane| pane.entity_id() == pane_id)
                        .is_some_and(|pane| pane.read(cx).tab_is_document(tab_id));
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
                if let Some((tab_id, _)) = self.restored_tab_ids.get(paused_session_id) {
                    self.synchronized_tabs.set_paused(*tab_id, true);
                }
            }
        }
        let translation_sessions = std::mem::take(&mut self.pending_restored_translation_sessions);
        for session in translation_sessions {
            let Some((source_tab_id, source_pane_id)) =
                self.restored_tab_ids.get(&session.source_tab).copied()
            else {
                continue;
            };
            let Some((translated_tab_id, translated_pane_id)) =
                self.restored_tab_ids.get(&session.translated_tab).copied()
            else {
                continue;
            };
            let source_pane = self
                .pane_group
                .panes()
                .into_iter()
                .find(|pane| pane.entity_id() == source_pane_id);
            let translated_pane = self
                .pane_group
                .panes()
                .into_iter()
                .find(|pane| pane.entity_id() == translated_pane_id);
            let (Some(source_pane), Some(translated_pane)) = (source_pane, translated_pane) else {
                continue;
            };
            let Some(source) = source_pane.read(cx).document_view_for_id(source_tab_id) else {
                continue;
            };
            let Some(translated) = translated_pane
                .read(cx)
                .document_view_for_id(translated_tab_id)
            else {
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
            let session_id = self.document_translation_jobs.update(cx, |manager, cx| {
                manager.restore_session(
                    settings,
                    request,
                    source_snapshot,
                    mode,
                    cache_key,
                    session.original_unit_ids.into_iter().collect(),
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
            self.register_live_translation_pair(session_id, source, translated, window, cx);
        }
        if let Some(session_tab_id) = self.pending_dock_document_tab.take()
            && let Some((tab_id, _)) = self.restored_tab_ids.get(&session_tab_id).copied()
            && self.document_target_for_id(tab_id, cx).is_some()
        {
            self.document_context_mru
                .retain(|candidate| *candidate != tab_id);
            self.document_context_mru.insert(0, tab_id);
        } else {
            let active_pane = self.active_pane.clone();
            self.record_document_context(&active_pane, cx);
        }
        self.prune_document_context(cx);
        self.restored_tab_ids.clear();
        self.refresh_synchronized_tab_presentation(cx);
        cx.notify();
    }

    fn open_settings_window(&mut self, cx: &mut Context<Self>) {
        self.leave_zen_mode(cx);
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
                    }
                    this.exit_zen_mode_for_unsupported_active_tab(cx);
                    this.record_document_context(&pane_for_events, cx);
                    cx.notify();
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
                    let result = if *replace_active_new_tab {
                        this.open_document_in_active_pane(
                            path.clone(),
                            position.clone(),
                            window,
                            cx,
                        )
                    } else {
                        this.focus_or_open_document(
                            path.clone(),
                            position.clone(),
                            false,
                            window,
                            cx,
                        )
                    };
                    if let Err(error) = result {
                        eprintln!("failed to open document: {error:#}");
                    }
                }
                PaneEvent::ExternalPathsDropped(paths) => {
                    this.open_external_document_paths_in_pane(paths, &pane_for_events, window, cx);
                }
                PaneEvent::DocumentOpenFinished { job_id, result } => {
                    if !this.continue_translation_split_open(*job_id, result.clone(), window, cx) {
                        this.document_translation_jobs.update(cx, |jobs, cx| {
                            jobs.finish_open(*job_id, result.clone(), cx);
                        });
                    }
                }
                PaneEvent::RestoredDocumentFinished {
                    session_tab_id,
                    document_tab_id,
                } => {
                    if let Some(document_tab_id) = document_tab_id {
                        this.restored_tab_ids.insert(
                            *session_tab_id,
                            (*document_tab_id, pane_for_events.entity_id()),
                        );
                    } else {
                        this.restored_tab_ids.remove(session_tab_id);
                    }
                    this.pending_restored_documents =
                        this.pending_restored_documents.saturating_sub(1);
                    if this.pending_restored_documents == 0 {
                        this.finish_reader_session_restore(window, cx);
                    } else {
                        cx.notify();
                    }
                }
                PaneEvent::DocumentPasswordAuthenticated { path, password } => {
                    let workspace = cx.entity().downgrade();
                    let path = path.clone();
                    let password = password.clone();
                    window.defer(cx, move |window, cx| {
                        let Some(workspace) = workspace.upgrade() else {
                            return;
                        };
                        workspace.update(cx, |workspace, cx| {
                            for pane in workspace.pane_group.panes() {
                                pane.update(cx, |pane, cx| {
                                    pane.unlock_pending_documents_with_password(
                                        &path,
                                        password.clone(),
                                        window,
                                        cx,
                                    );
                                });
                            }
                        });
                    });
                }
                PaneEvent::OpenSettings => this.open_settings_window(cx),
                PaneEvent::DocumentSidebarNavigation(document_view_id) => {
                    if pane_for_events.update(cx, |pane, cx| {
                        pane.activate_tab_by_id(*document_view_id, true, window, cx)
                    }) {
                        this.active_pane = pane_for_events.clone();
                        this.record_document_context(&pane_for_events, cx);
                        cx.notify();
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
                PaneEvent::CloseTab(tab_id) => {
                    if let Some(tab_ix) = pane_for_events.read(cx).tab_ix_for_entity_id(*tab_id) {
                        this.close_tab_in_pane(&pane_for_events, Some(tab_ix), window, cx);
                    }
                }
                PaneEvent::DuplicateTab(tab_ix) => {
                    this.active_pane = pane_for_events.clone();
                    this.duplicate_tab_in_pane(&pane_for_events, *tab_ix, window, cx);
                }
                PaneEvent::TabClicked { tab_id, was_active } => {
                    this.active_pane = pane_for_events.clone();
                    this.record_document_context(&pane_for_events, cx);
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
                    if !this.is_live_translation_pair_tab(*tab_id)
                        && this.synchronized_tabs.set_paused(*tab_id, *paused)
                    {
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
            }
            this.exit_zen_mode_for_unsupported_active_tab(cx);
            this.record_document_context(&pane_for_focus, cx);
            cx.notify();
        })
        .detach();

        let pane_for_observe = pane.clone();
        cx.observe(pane, move |this, _, cx| {
            this.refresh_live_translation_pair_activity(cx);
            if this.active_pane == pane_for_observe {
                this.exit_zen_mode_for_unsupported_active_tab(cx);
                this.record_document_context(&pane_for_observe, cx);
                cx.notify();
            }
        })
        .detach();
    }

    fn focus_active_pane(&self, window: &mut Window, cx: &mut Context<Self>) {
        let active_pane = self.active_pane.clone();
        active_pane.update(cx, |pane, cx| pane.focus(window, cx));
    }

    fn update_active_document_view(
        &self,
        cx: &mut Context<Self>,
        update: impl FnOnce(&mut DocumentView, &mut Context<DocumentView>),
    ) {
        if self.surface_mode != WorkspaceSurfaceMode::Panes {
            return;
        }
        if let Some(document_view) = self.active_pane.read(cx).active_document_view() {
            document_view.update(cx, update);
        }
    }

    pub fn surface_mode(&self) -> WorkspaceSurfaceMode {
        self.surface_mode
    }

    fn leave_zen_mode(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.zen_mode {
            return false;
        }
        self.zen_mode = false;
        cx.notify();
        true
    }

    fn exit_zen_mode(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.leave_zen_mode(cx) {
            self.focus_active_pane(window, cx);
        }
    }

    fn exit_zen_mode_for_unsupported_active_tab(&mut self, cx: &mut Context<Self>) {
        if self.zen_mode && !self.active_pane.read(cx).active_tab_supports_zen_mode() {
            self.leave_zen_mode(cx);
        }
    }

    fn toggle_zen_mode(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.zen_mode {
            self.exit_zen_mode(window, cx);
            return;
        }
        if self.surface_mode != WorkspaceSurfaceMode::Panes
            || self.active_pane.read(cx).active_document_view().is_none()
        {
            return;
        }

        self.set_title_bar_menu_expanded(false, cx);
        self.finish_synchronized_connection(cx);
        self.fading_synchronized_connection = None;
        self.zen_mode = true;
        self.focus_active_pane(window, cx);
        cx.notify();
    }

    fn enter_home(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.leave_zen_mode(cx);
        if self.surface_mode == WorkspaceSurfaceMode::Home {
            return;
        }
        self.set_title_bar_menu_expanded(false, cx);
        self.close_command_palette(window, cx);
        self.close_theme_selector(window, cx);
        if self.document_browser.is_some() {
            self.close_document_browser(window, cx);
        }
        self.home_previous_focus_handle = window
            .focused(cx)
            .or_else(|| Some(self.active_pane.read(cx).focus_handle()));
        self.surface_mode = WorkspaceSurfaceMode::Home;
        self.home_view.read(cx).focus_handle().focus(window, cx);
        cx.notify();
    }

    fn leave_home(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.surface_mode == WorkspaceSurfaceMode::Panes {
            return;
        }
        self.surface_mode = WorkspaceSurfaceMode::Panes;
        if let Some(previous_focus) = self.home_previous_focus_handle.take() {
            previous_focus.focus(window, cx);
        } else {
            self.focus_active_pane(window, cx);
        }
        cx.notify();
    }

    pub(crate) fn toggle_home(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.surface_mode {
            WorkspaceSurfaceMode::Home => self.leave_home(window, cx),
            WorkspaceSurfaceMode::Panes => self.enter_home(window, cx),
        }
    }

    fn open_document_in_active_pane(
        &mut self,
        path: PathBuf,
        position: Option<DocumentPosition>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        if self.document_browser.is_some() {
            self.close_document_browser(window, cx);
        }
        let path = normalize_path(&path);
        let active_pane = self.active_pane.clone();
        let replace_active_new_tab = active_pane.read(cx).active_tab_is_new_tab();
        if let Some(tab_ix) = active_pane.read(cx).document_tab_ix(&path, cx) {
            active_pane.update(cx, |pane, cx| {
                pane.activate_existing_document(
                    tab_ix,
                    position,
                    replace_active_new_tab,
                    window,
                    cx,
                );
            });
        } else {
            active_pane.update(cx, |pane, cx| {
                pane.focus_or_open_document(path, position, replace_active_new_tab, window, cx)
            })?;
        }
        self.surface_mode = WorkspaceSurfaceMode::Panes;
        self.home_previous_focus_handle = None;
        cx.notify();
        Ok(())
    }

    fn record_document_context(&mut self, pane: &Entity<Pane>, cx: &mut Context<Self>) {
        let Some(document_view) = pane.read(cx).active_document_view() else {
            return;
        };
        let tab_id = document_view.entity_id();
        if self.document_context_mru.first().copied() == Some(tab_id) {
            return;
        }
        self.document_context_mru
            .retain(|candidate| *candidate != tab_id);
        self.document_context_mru.insert(0, tab_id);
    }

    fn document_target_for_id(
        &self,
        tab_id: EntityId,
        cx: &App,
    ) -> Option<(Entity<Pane>, Entity<DocumentView>)> {
        self.pane_group.panes().into_iter().find_map(|pane| {
            let document_view = pane.read(cx).active_document_view()?;
            (document_view.entity_id() == tab_id).then_some((pane, document_view))
        })
    }

    fn dock_document_target(&self, cx: &App) -> Option<(Entity<Pane>, Entity<DocumentView>)> {
        self.document_context_mru
            .iter()
            .find_map(|tab_id| self.document_target_for_id(*tab_id, cx))
            .or_else(|| {
                self.active_pane
                    .read(cx)
                    .active_document_view()
                    .map(|document_view| (self.active_pane.clone(), document_view))
            })
            .or_else(|| {
                self.pane_group.panes().into_iter().find_map(|pane| {
                    pane.read(cx)
                        .active_document_view()
                        .map(|document_view| (pane, document_view))
                })
            })
    }

    fn prune_document_context(&mut self, cx: &mut Context<Self>) {
        let document_ids = self
            .pane_group
            .panes()
            .into_iter()
            .flat_map(|pane| {
                pane.read(cx)
                    .document_views()
                    .into_iter()
                    .map(|document_view| document_view.entity_id())
            })
            .collect::<HashSet<_>>();
        self.document_context_mru
            .retain(|tab_id| document_ids.contains(tab_id));
        if self.document_context_mru.is_empty()
            && let Some((_, document_view)) = self.dock_document_target(cx)
        {
            self.document_context_mru.push(document_view.entity_id());
        }
    }

    fn focus_dock_document(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((pane, document_view)) = self.dock_document_target(cx) else {
            return;
        };
        let tab_id = document_view.entity_id();
        if pane.update(cx, |pane, cx| {
            pane.activate_tab_by_id(tab_id, true, window, cx)
        }) {
            self.active_pane = pane.clone();
            self.record_document_context(&pane, cx);
            cx.notify();
        }
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
        let pause_locked_tabs = self
            .live_translation_pairs
            .values()
            .flat_map(|pair| [pair.source_id, pair.translated_id])
            .collect::<HashSet<_>>();
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
                if pause_locked_tabs.contains(&tab_id) {
                    presentation.pause_locked_tabs.insert(tab_id);
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

    fn live_translation_pair_for_tab(
        &self,
        tab_id: EntityId,
    ) -> Option<(DocumentTranslationJobId, EntityId, EntityId)> {
        self.live_translation_pairs
            .iter()
            .find_map(|(session_id, pair)| {
                (pair.source_id == tab_id || pair.translated_id == tab_id).then_some((
                    *session_id,
                    pair.source_id,
                    pair.translated_id,
                ))
            })
    }

    fn live_translation_counterpart(&self, tab_id: EntityId) -> Option<EntityId> {
        self.live_translation_pair_for_tab(tab_id)
            .map(|(_, source_id, translated_id)| {
                if source_id == tab_id {
                    translated_id
                } else {
                    source_id
                }
            })
    }

    fn is_live_translation_pair_tab(&self, tab_id: EntityId) -> bool {
        self.live_translation_counterpart(tab_id).is_some()
    }

    fn tab_is_displayed(&self, tab_id: EntityId, cx: &App) -> bool {
        self.pane_group
            .panes()
            .into_iter()
            .any(|pane| pane.read(cx).active_tab_id() == Some(tab_id))
    }

    fn document_view_for_tab(&self, tab_id: EntityId, cx: &App) -> Option<Entity<DocumentView>> {
        self.pane_group
            .panes()
            .into_iter()
            .find_map(|pane| pane.read(cx).document_view_for_id(tab_id))
    }

    fn synchronize_live_translation_viewport_anchor(
        &self,
        session_id: DocumentTranslationJobId,
        changed_tab: EntityId,
        anchor: TranslationViewportAnchor,
        cx: &mut Context<Self>,
    ) {
        let Some((pair_session_id, source_id, translated_id)) =
            self.live_translation_pair_for_tab(changed_tab)
        else {
            return;
        };
        if pair_session_id != session_id {
            return;
        }
        let counterpart = if changed_tab == source_id {
            translated_id
        } else {
            source_id
        };
        let Some(changed_view) = self.document_view_for_tab(changed_tab, cx) else {
            return;
        };
        let Some(counterpart_view) = self.document_view_for_tab(counterpart, cx) else {
            return;
        };
        counterpart_view.update(cx, |view, cx| {
            view.apply_live_translation_viewport_anchor(session_id, anchor, cx);
        });
        let Some(applied) = counterpart_view
            .read(cx)
            .live_translation_viewport_anchor(anchor.mode)
        else {
            return;
        };
        let applied_exactly = applied.page_index == anchor.page_index
            && (applied.pdf_x - anchor.pdf_x).abs() < 0.01
            && (applied.pdf_y - anchor.pdf_y).abs() < 0.01
            && (applied.viewport_y_fraction - anchor.viewport_y_fraction).abs() < 0.001;
        if !applied_exactly {
            // Near a document edge, the counterpart may be unable to center
            // the requested point at its independent zoom. Move the initiating
            // view to the closest point both viewports can represent.
            changed_view.update(cx, |view, cx| {
                view.apply_live_translation_viewport_anchor(session_id, applied, cx);
            });
        }
    }

    fn align_live_translation_counterpart(
        &self,
        changed_tab: EntityId,
        mode: crate::document_view::SynchronizedScrollMode,
        cx: &mut Context<Self>,
    ) {
        let Some((session_id, _, _)) = self.live_translation_pair_for_tab(changed_tab) else {
            return;
        };
        let Some(changed_view) = self.document_view_for_tab(changed_tab, cx) else {
            return;
        };
        let Some(anchor) = changed_view.read(cx).live_translation_viewport_anchor(mode) else {
            return;
        };
        self.synchronize_live_translation_viewport_anchor(session_id, changed_tab, anchor, cx);
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
        if !self.tab_is_displayed(source_tab, cx) {
            return;
        }

        let translation_counterpart = self.live_translation_counterpart(source_tab);
        if translation_counterpart.is_some() {
            self.align_live_translation_counterpart(source_tab, delta.mode, cx);
        }
        let mut aligned_translation_tabs = HashSet::new();
        for member_tab in self.synchronized_tabs.member_tabs(group_id) {
            if member_tab == source_tab
                || Some(member_tab) == translation_counterpart
                || aligned_translation_tabs.contains(&member_tab)
                || self.synchronized_tabs.is_paused(member_tab)
            {
                continue;
            }
            let translation_pair = self.live_translation_pair_for_tab(member_tab);
            if let Some((_, pair_source, pair_translated)) = translation_pair {
                let preferred = if self.tab_is_displayed(pair_source, cx) {
                    pair_source
                } else {
                    pair_translated
                };
                if member_tab != preferred {
                    continue;
                }
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
                if let Some((_, pair_source, pair_translated)) = translation_pair {
                    aligned_translation_tabs.insert(pair_source);
                    aligned_translation_tabs.insert(pair_translated);
                    self.align_live_translation_counterpart(member_tab, delta.mode, cx);
                }
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
        self.exit_zen_mode(window, cx);
        self.set_title_bar_menu_expanded(false, cx);
        if self.surface_mode == WorkspaceSurfaceMode::Home
            && self.active_pane.read(cx).active_tab_is_new_tab()
        {
            self.leave_home(window, cx);
            return;
        }
        self.leave_home(window, cx);
        let active_pane = self.active_pane.clone();
        active_pane.update(cx, |pane, cx| pane.open_new_tab(window, cx));
    }

    pub fn open_keymap_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.exit_zen_mode(window, cx);
        self.leave_home(window, cx);
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
                    let result = if this.surface_mode == WorkspaceSurfaceMode::Home {
                        this.open_document_in_active_pane(path, None, window, cx)
                    } else {
                        this.focus_or_open_document(path, None, replace_active_new_tab, window, cx)
                    };
                    if let Err(error) = result {
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
        let opened_from_home = self.surface_mode == WorkspaceSurfaceMode::Home;
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
                    if opened_from_home {
                        this.open_document_in_active_pane(path, None, window, cx)
                    } else {
                        this.focus_or_open_document(path, None, replace_active_new_tab, window, cx)
                    }
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
            self.surface_mode = WorkspaceSurfaceMode::Panes;
            self.home_previous_focus_handle = None;
            cx.notify();
            return Ok(());
        }

        let active_pane = self.active_pane.clone();
        active_pane.update(cx, |pane, cx| {
            pane.focus_or_open_document(path, position, replace_active_new_tab, window, cx)
        })?;
        self.surface_mode = WorkspaceSurfaceMode::Panes;
        self.home_previous_focus_handle = None;
        cx.notify();
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
        if self.surface_mode == WorkspaceSurfaceMode::Home {
            return;
        }
        self.set_title_bar_menu_expanded(false, cx);
        let active_pane = self.active_pane.clone();
        self.close_tab_in_pane(&active_pane, None, window, cx);
    }

    fn duplicate_active_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.surface_mode != WorkspaceSurfaceMode::Panes {
            return;
        }
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
        let resolved_ix = tab_ix.unwrap_or_else(|| pane.read(cx).active_tab_ix());
        let document_view = pane.read(cx).document_view_at(resolved_ix);
        if let Some(document_view) = document_view
            && let Some(session_id) = document_view.read(cx).translation_session_id()
        {
            let running = document_view.read(cx).translation_session_is_running();
            let tab_id = document_view.entity_id();
            if running {
                let answer = window.prompt(
                    PromptLevel::Warning,
                    "Translation is still running",
                    Some(
                        "Cancel the translation, continue it in the background, or keep this tab open.",
                    ),
                    &[
                        PromptButton::ok("Cancel translation"),
                        PromptButton::new("Continue in background"),
                        PromptButton::cancel("Keep tab open"),
                    ],
                    cx,
                );
                let pane = pane.clone();
                cx.spawn_in(window, async move |this, cx| {
                    let Ok(choice) = answer.await else {
                        return;
                    };
                    if choice == 2 {
                        return;
                    }
                    _ = this.update_in(cx, |this, window, cx| {
                        if choice == 0 {
                            this.document_translation_jobs
                                .update(cx, |manager, cx| manager.discard_session(session_id, cx));
                        } else {
                            this.document_translation_jobs.update(cx, |manager, cx| {
                                manager.set_session_backgrounded(session_id, cx)
                            });
                        }
                        this.unregister_live_translation_pair(session_id, cx);
                        if let Some(tab_ix) = pane.read(cx).tab_ix_for_entity_id(tab_id) {
                            this.close_tab_in_pane_now(&pane, Some(tab_ix), window, cx);
                        }
                    });
                })
                .detach();
                return;
            }
            self.document_translation_jobs
                .update(cx, |manager, cx| manager.discard_session(session_id, cx));
            self.unregister_live_translation_pair(session_id, cx);
        }
        self.close_tab_in_pane_now(pane, Some(resolved_ix), window, cx);
    }

    fn close_tab_in_pane_now(
        &mut self,
        pane: &Entity<Pane>,
        tab_ix: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let pane_was_active = self.active_pane == *pane;
        let closed_tab_id = match tab_ix {
            Some(tab_ix) => pane.read(cx).tab_id_at(tab_ix),
            None => pane.read(cx).active_tab_id(),
        };
        if let Some(tab_id) = closed_tab_id {
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
        let empty = pane.update(cx, |pane, cx| match tab_ix {
            Some(tab_ix) => pane.close_tab_at(tab_ix, cx),
            None => pane.close_active_tab(cx),
        });
        if empty {
            self.handle_empty_pane(pane, window, cx);
        } else if pane_was_active {
            pane.update(cx, |pane, cx| pane.focus(window, cx));
        }
        self.exit_zen_mode_for_unsupported_active_tab(cx);
        self.prune_document_context(cx);
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
            self.enter_home(window, cx);
            return;
        }

        self.remove_pane(pane, window, cx);
    }

    fn remove_pane(&mut self, pane: &Entity<Pane>, window: &mut Window, cx: &mut Context<Self>) {
        let pane_was_active = self.active_pane == *pane;
        let panes = self.pane_group.panes();
        let remove_ix = panes
            .iter()
            .position(|candidate| candidate == pane)
            .unwrap_or(0);
        let removed_tab_ids = pane.read(cx).tab_ids();
        for tab_id in removed_tab_ids {
            self.unregister_live_translation_pairs_for_tab(tab_id, cx);
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

        self.sync_tab_bar_visibility(cx);
        if pane_was_active {
            let active_ix = remove_ix.min(remaining.len() - 1);
            self.active_pane = remaining[active_ix].clone();
            self.active_pane
                .update(cx, |pane, cx| pane.focus(window, cx));
        }
        self.exit_zen_mode_for_unsupported_active_tab(cx);
        self.prune_document_context(cx);
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
        self.exit_zen_mode(window, cx);
        if self.surface_mode != WorkspaceSurfaceMode::Panes {
            return;
        }
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

    fn open_translation_session(
        &mut self,
        session_id: DocumentTranslationJobId,
        source_path: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        self.exit_zen_mode(window, cx);
        let source_path = normalize_path(source_path);
        let Some((source_pane, tab_ix)) = self.document_location(&source_path, cx) else {
            return Err(anyhow::anyhow!(
                "open the source document before opening its live translation"
            ));
        };
        source_pane.update(cx, |pane, cx| {
            pane.activate_existing_document(tab_ix, None, false, window, cx);
        });
        let Some(original) = source_pane.read(cx).active_document_view() else {
            return Err(anyhow::anyhow!("translation source is still opening"));
        };
        let snapshot = self
            .document_translation_jobs
            .read(cx)
            .session_snapshot(session_id)
            .ok_or_else(|| anyhow::anyhow!("translation session is no longer available"))?;
        let viewport = window.viewport_size();
        let direction = match snapshot.mode {
            glyphst_settings::DocumentTranslationMode::Vertical => SplitDirection::Right,
            glyphst_settings::DocumentTranslationMode::Horizontal => SplitDirection::Down,
            glyphst_settings::DocumentTranslationMode::Auto
            | glyphst_settings::DocumentTranslationMode::Background
                if viewport.width >= viewport.height =>
            {
                SplitDirection::Right
            }
            glyphst_settings::DocumentTranslationMode::Auto
            | glyphst_settings::DocumentTranslationMode::Background => SplitDirection::Down,
        };
        let translated_pane = self.create_empty_pane(window, cx);
        self.pane_group
            .split(&source_pane, &translated_pane, direction, cx);
        let translated = translated_pane.update(cx, |pane, cx| {
            pane.open_translation_duplicate(&original, session_id, window, cx)
        });
        let translated = match translated {
            Ok(translated) => translated,
            Err(error) => {
                self.remove_pane(&translated_pane, window, cx);
                return Err(error);
            }
        };
        _ = self.synchronized_tabs.connect_tab_to_source_group(
            original.entity_id(),
            source_pane.entity_id(),
            translated.entity_id(),
            translated_pane.entity_id(),
        );
        original.update(cx, |view, cx| {
            view.attach_translation_source_mirror(session_id, cx);
        });
        self.register_live_translation_pair(session_id, original, translated, window, cx);
        self.active_pane = translated_pane.clone();
        self.sync_tab_bar_visibility(cx);
        translated_pane.update(cx, |pane, cx| pane.focus(window, cx));
        self.refresh_synchronized_tab_presentation(cx);
        cx.notify();
        Ok(())
    }

    fn save_translation_session(
        &mut self,
        session_id: DocumentTranslationJobId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(snapshot) = self
            .document_translation_jobs
            .read(cx)
            .session_snapshot(session_id)
        else {
            return;
        };
        if !snapshot.can_save() {
            return;
        }
        if snapshot.failures.is_empty() {
            self.choose_translation_export_path(session_id, cx);
            return;
        }
        let answer = window.prompt(
            PromptLevel::Warning,
            "Save a partial translation?",
            Some(
                "Some translation units failed. They will remain in the original language in the saved PDF.",
            ),
            &[
                PromptButton::ok("Save partial translation"),
                PromptButton::cancel("Cancel"),
            ],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            if answer.await != Ok(0) {
                return;
            }
            _ = this.update_in(cx, |this, _, cx| {
                this.choose_translation_export_path(session_id, cx);
            });
        })
        .detach();
    }

    fn choose_translation_export_path(
        &mut self,
        session_id: DocumentTranslationJobId,
        cx: &mut Context<Self>,
    ) {
        let Some(snapshot) = self
            .document_translation_jobs
            .read(cx)
            .session_snapshot(session_id)
        else {
            return;
        };
        let source_path = &snapshot.request.source_path;
        let stem = source_path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .filter(|stem| !stem.is_empty())
            .unwrap_or("document");
        let suggested_name = format!("{stem}.{}.pdf", snapshot.request.target_language.as_str());
        let directory = source_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        let dialog = cx.prompt_for_new_path(&directory, Some(&suggested_name));
        let manager = self.document_translation_jobs.clone();
        cx.spawn(async move |_, cx| match dialog.await {
            Ok(Ok(Some(output_path))) => {
                manager.update(cx, |manager, cx| {
                    manager.export_session(session_id, output_path, cx);
                });
            }
            Ok(Err(error)) => {
                eprintln!("failed to choose translated PDF output path: {error:#}");
            }
            Ok(Ok(None)) | Err(_) => {}
        })
        .detach();
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
        self.exit_zen_mode(window, cx);
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

    fn register_live_translation_pair(
        &mut self,
        session_id: DocumentTranslationJobId,
        original: Entity<DocumentView>,
        translated: Entity<DocumentView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let source_id = original.entity_id();
        let translated_id = translated.entity_id();
        let conflicts = self
            .live_translation_pairs
            .iter()
            .filter_map(|(id, pair)| {
                (*id != session_id
                    && (pair.source_id == source_id
                        || pair.source_id == translated_id
                        || pair.translated_id == source_id
                        || pair.translated_id == translated_id))
                    .then_some(*id)
            })
            .collect::<Vec<_>>();
        for conflict in conflicts {
            self.unregister_live_translation_pair(conflict, cx);
        }
        if self.live_translation_pairs.contains_key(&session_id) {
            self.unregister_live_translation_pair(session_id, cx);
        }

        let original_target = original.downgrade();
        cx.subscribe_in(
            &translated,
            window,
            move |workspace, _, event: &DocumentViewEvent, _, cx| {
                if let Some(original_target) = original_target.upgrade() {
                    match event {
                        DocumentViewEvent::TranslationRegionHovered {
                            session_id: event_session_id,
                            region_id,
                        } if *event_session_id == session_id => {
                            original_target.update(cx, |view, cx| {
                                view.apply_live_translation_hover(
                                    session_id,
                                    region_id.clone(),
                                    cx,
                                );
                            });
                        }
                        DocumentViewEvent::TranslationOverlayVisibilityChanged {
                            session_id: event_session_id,
                            enabled,
                        } if *event_session_id == session_id => {
                            original_target.update(cx, |view, cx| {
                                view.apply_live_translation_overlay_visibility(
                                    session_id, *enabled, cx,
                                );
                            });
                        }
                        DocumentViewEvent::TranslationViewportChanged {
                            session_id: event_session_id,
                            anchor,
                        } if *event_session_id == session_id => {
                            workspace.synchronize_live_translation_viewport_anchor(
                                session_id,
                                translated_id,
                                *anchor,
                                cx,
                            );
                        }
                        _ => {}
                    }
                }
            },
        )
        .detach();

        cx.subscribe_in(
            &original,
            window,
            move |workspace, _, event: &DocumentViewEvent, _, cx| {
                let DocumentViewEvent::TranslationViewportChanged {
                    session_id: event_session_id,
                    anchor,
                } = event
                else {
                    return;
                };
                if *event_session_id != session_id {
                    return;
                }
                workspace.synchronize_live_translation_viewport_anchor(
                    session_id, source_id, *anchor, cx,
                );
            },
        )
        .detach();

        self.live_translation_pairs.insert(
            session_id,
            LiveTranslationPair {
                source_id,
                translated_id,
                source: original.downgrade(),
                translated: translated.downgrade(),
                active: false,
            },
        );
        self.synchronized_tabs.set_paused(source_id, false);
        self.synchronized_tabs.set_paused(translated_id, false);
        if let Some(anchor) = original.read(cx).live_translation_viewport_anchor(
            crate::document_view::SynchronizedScrollMode::Immediate,
        ) {
            self.synchronize_live_translation_viewport_anchor(session_id, source_id, anchor, cx);
        }
        self.refresh_live_translation_pair_activity(cx);
        self.refresh_synchronized_tab_presentation(cx);
    }

    fn unregister_live_translation_pair(
        &mut self,
        session_id: DocumentTranslationJobId,
        cx: &mut Context<Self>,
    ) {
        if let Some(pair) = self.live_translation_pairs.remove(&session_id) {
            if let Some(source) = pair.source.upgrade() {
                source.update(cx, |view, cx| {
                    view.set_translation_pair_active(session_id, false, cx);
                    view.detach_translation_source_mirror(session_id, cx);
                });
            }
            if let Some(translated) = pair.translated.upgrade() {
                translated.update(cx, |view, cx| {
                    view.set_translation_pair_active(session_id, false, cx);
                });
            }
            self.refresh_synchronized_tab_presentation(cx);
            return;
        }

        // A session can be interrupted between attaching its source mirror
        // and registering the pair. Clean that narrow partial-open state too.
        for pane in self.pane_group.panes() {
            for view in pane.read(cx).document_views() {
                view.update(cx, |view, cx| {
                    view.detach_translation_source_mirror(session_id, cx);
                });
            }
        }
    }

    fn unregister_live_translation_pairs_for_tab(
        &mut self,
        tab_id: EntityId,
        cx: &mut Context<Self>,
    ) {
        let session_ids = self
            .live_translation_pairs
            .iter()
            .filter_map(|(session_id, pair)| {
                (pair.source_id == tab_id || pair.translated_id == tab_id).then_some(*session_id)
            })
            .collect::<Vec<_>>();
        for session_id in session_ids {
            self.unregister_live_translation_pair(session_id, cx);
        }
    }

    fn refresh_live_translation_pair_activity(&mut self, cx: &mut Context<Self>) {
        if self.live_translation_pairs.is_empty() {
            return;
        }
        let active_tabs = self
            .pane_group
            .panes()
            .into_iter()
            .filter_map(|pane| {
                pane.read(cx)
                    .active_document_view()
                    .map(|view| view.entity_id())
            })
            .collect::<HashSet<_>>();
        let stale = self
            .live_translation_pairs
            .iter()
            .filter_map(|(session_id, pair)| {
                (pair.source.upgrade().is_none() || pair.translated.upgrade().is_none())
                    .then_some(*session_id)
            })
            .collect::<Vec<_>>();
        for session_id in stale {
            self.unregister_live_translation_pair(session_id, cx);
        }

        let mut updates = Vec::new();
        for (session_id, pair) in &mut self.live_translation_pairs {
            let active =
                live_translation_pair_is_active(pair.source_id, pair.translated_id, &active_tabs);
            if pair.active == active {
                continue;
            }
            pair.active = active;
            if let (Some(source), Some(translated)) =
                (pair.source.upgrade(), pair.translated.upgrade())
            {
                updates.push((*session_id, active, source, translated));
            }
        }
        for (session_id, active, source, translated) in updates {
            source.update(cx, |view, cx| {
                view.set_translation_pair_active(session_id, active, cx);
            });
            translated.update(cx, |view, cx| {
                view.set_translation_pair_active(session_id, active, cx);
            });
        }
    }

    fn activate_pane(&mut self, pane: Entity<Pane>, window: &mut Window, cx: &mut Context<Self>) {
        self.active_pane = pane.clone();
        self.exit_zen_mode_for_unsupported_active_tab(cx);
        pane.update(cx, |pane, cx| pane.focus(window, cx));
        cx.notify();
    }

    fn activate_pane_in_direction(
        &mut self,
        direction: SplitDirection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.surface_mode != WorkspaceSurfaceMode::Panes {
            return;
        }
        if let Some(pane) = self
            .pane_group
            .find_pane_in_direction(&self.active_pane, direction)
        {
            self.activate_pane(pane, window, cx);
        }
    }

    fn activate_next_pane(&mut self, reverse: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.surface_mode != WorkspaceSurfaceMode::Panes {
            return;
        }
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
        self.leave_zen_mode(cx);
        if self.surface_mode != WorkspaceSurfaceMode::Panes {
            return;
        }
        if let Some(target) = self
            .pane_group
            .find_pane_in_direction(&self.active_pane, direction)
        {
            self.pane_group.swap(&self.active_pane, &target);
            cx.notify();
        }
    }

    fn move_pane_to_border(&mut self, direction: SplitDirection, cx: &mut Context<Self>) {
        self.leave_zen_mode(cx);
        if self.surface_mode != WorkspaceSurfaceMode::Panes {
            return;
        }
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
        self.exit_zen_mode(window, cx);
        if self.surface_mode != WorkspaceSurfaceMode::Panes {
            return;
        }
        self.pane_group
            .resize(&self.active_pane, direction, window, cx);
        cx.notify();
    }

    fn close_other_panes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.exit_zen_mode(window, cx);
        if self.surface_mode != WorkspaceSurfaceMode::Panes {
            return;
        }
        let sessions = self
            .pane_group
            .panes()
            .into_iter()
            .filter(|pane| *pane != self.active_pane)
            .flat_map(|pane| pane.read(cx).translation_sessions(cx))
            .collect::<Vec<_>>();
        let has_running = sessions.iter().any(|(_, running)| *running);
        if has_running {
            let answer = window.prompt(
                PromptLevel::Warning,
                "Translations are still running",
                Some(
                    "Closing the other panes would close live translation tabs. Cancel those translations, continue them in the background, or keep the panes open.",
                ),
                &[
                    PromptButton::ok("Cancel translations"),
                    PromptButton::new("Continue in background"),
                    PromptButton::cancel("Keep panes open"),
                ],
                cx,
            );
            cx.spawn_in(window, async move |this, cx| {
                let Ok(choice) = answer.await else {
                    return;
                };
                if choice == 2 {
                    return;
                }
                _ = this.update_in(cx, |this, window, cx| {
                    this.dispose_translation_sessions_for_close(&sessions, choice == 1, cx);
                    this.close_other_panes_now(window, cx);
                });
            })
            .detach();
            return;
        }
        self.dispose_translation_sessions_for_close(&sessions, false, cx);
        self.close_other_panes_now(window, cx);
    }

    fn dispose_translation_sessions_for_close(
        &mut self,
        sessions: &[(DocumentTranslationJobId, bool)],
        keep_running_in_background: bool,
        cx: &mut Context<Self>,
    ) {
        let mut seen = HashSet::new();
        for (session_id, running) in sessions.iter().copied() {
            if !seen.insert(session_id) {
                continue;
            }
            self.document_translation_jobs.update(cx, |manager, cx| {
                if running && keep_running_in_background {
                    manager.set_session_backgrounded(session_id, cx);
                } else {
                    manager.discard_session(session_id, cx);
                }
            });
            self.unregister_live_translation_pair(session_id, cx);
        }
    }

    fn close_other_panes_now(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for pane in self.pane_group.panes() {
            if pane != self.active_pane {
                self.cancel_pending_translation_splits_for_pane(&pane, cx);
                for tab_id in pane.read(cx).tab_ids() {
                    self.unregister_live_translation_pairs_for_tab(tab_id, cx);
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
        self.leave_zen_mode(cx);
        if self.surface_mode != WorkspaceSurfaceMode::Panes {
            return;
        }
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
                self.exit_zen_mode(window, cx);
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
        self.exit_zen_mode(window, cx);
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
        self.exit_zen_mode_for_unsupported_active_tab(cx);
        let zen_mode = self.zen_mode;
        let active_document_view = (self.surface_mode == WorkspaceSurfaceMode::Panes)
            .then(|| self.active_pane.read(cx).active_document_view())
            .flatten();
        let active_pdf_is_presentation = active_document_view
            .as_ref()
            .is_some_and(|document_view| document_view.read(cx).is_presentation_mode());
        let zen_action_available = zen_mode || active_document_view.is_some();
        let title_bar = (!active_pdf_is_presentation && !zen_mode).then(|| {
            WorkspaceTitleBar::new(
                cx.entity(),
                self.title_bar_open_menu,
                self.title_bar_popup_menu.clone(),
                if self.surface_mode == WorkspaceSurfaceMode::Home {
                    Some("Home".into())
                } else {
                    self.active_tab_label(cx)
                },
                self.surface_mode == WorkspaceSurfaceMode::Home,
                shell,
                self.window_close_handler,
            )
        });
        let synchronized_connection = (self.surface_mode == WorkspaceSurfaceMode::Panes
            && !zen_mode)
            .then(|| self.render_synchronized_connection(theme, window, cx))
            .flatten();
        let body = if self.surface_mode == WorkspaceSurfaceMode::Home {
            self.render_home_body(theme, window, cx)
        } else {
            self.render_body(theme, dim_inactive_panes, window, cx)
        };
        div()
            .id("workspace")
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .bg(shell)
            .key_context(if zen_mode {
                "Workspace ZenMode"
            } else {
                "Workspace"
            })
            .track_focus(&self.focus_handle)
            .capture_action(cx.listener(|this, _: &ZoomIn, _, cx| {
                this.update_active_document_view(cx, |document_view, cx| {
                    document_view.zoom_in(cx);
                });
                cx.stop_propagation();
            }))
            .capture_action(cx.listener(|this, _: &ZoomOut, _, cx| {
                this.update_active_document_view(cx, |document_view, cx| {
                    document_view.zoom_out(cx);
                });
                cx.stop_propagation();
            }))
            .capture_action(cx.listener(|this, _: &ResetZoom, _, cx| {
                this.update_active_document_view(cx, |document_view, cx| {
                    document_view.reset_zoom(cx);
                });
                cx.stop_propagation();
            }))
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
            .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| {
                this.open_external_document_paths(paths.paths(), window, cx);
            }))
            .when(zen_action_available, |this| {
                this.on_action(cx.listener(|this, _: &ToggleZenMode, window, cx| {
                    this.toggle_zen_mode(window, cx);
                }))
            })
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
            .on_action(cx.listener(|this, _: &ToggleHome, window, cx| {
                this.toggle_home(window, cx);
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
            .on_action(cx.listener(|this, _: &OpenQuickAsk, window, cx| {
                this.toggle_quick_ask(window, cx);
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
                    .child(body),
            )
            .children(synchronized_connection)
            .when(self.quick_ask_visible, |this| {
                this.when_some(self.quick_ask.clone(), |this, quick_ask| {
                    this.child(self.render_quick_ask_overlay(quick_ask, window, cx))
                })
            })
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
        collections::HashSet,
        fs,
        path::Path,
        time::{Duration, Instant, SystemTime},
    };

    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use glyphst_ai::{
        agent::{AgentCancellationToken, AgentToolCall, AgentToolResultPart},
        ai_chat::DocumentContextScope,
    };
    use glyphst_ai_ui::AiContextProvider as _;
    use glyphst_pdf::DocumentPassword;
    use glyphst_translation_ui::DocumentTranslationOpenMode;
    use gpui::{
        AppContext as _, EntityId, ExternalPaths, FileDropEvent, Modifiers, MouseButton,
        MouseUpEvent, TestAppContext, bounds, point, px, size,
    };
    use gpui_component::{Root, WindowExt as _};

    use super::{
        PaneEvent, SplitDirection, SynchronizedConnectionAnchor, SynchronizedConnectionAnchorEdge,
        SynchronizedConnectionCurve, SynchronizedConnectionGroup,
        SynchronizedConnectionInvalidReason, SynchronizedConnectionTabState, ToggleZenMode,
        Workspace, WorkspaceAiContextProvider, WorkspaceCallbacks, WorkspaceMenu,
        WorkspaceSurfaceMode, WorkspaceWindowHandle, WorkspaceWindowRequest,
        live_translation_pair_is_active,
    };
    use crate::{
        document_view::{
            DocumentSidebar, RenderCache, SynchronizedScrollDelta, SynchronizedScrollMode,
        },
        library::{
            DocumentPosition, LibraryStore, ReaderNewTabSession, ReaderPaneSession, ReaderPaneTree,
            ReaderSidebar, ReaderSplitAxis, ReaderSynchronizedGroup, ReaderTabKind,
            ReaderTabSession, ReaderTranslationSession, ReaderWindowBounds, ReaderWindowSession,
            ReaderWindowState,
        },
        pdf_password_store::PdfPasswordStore,
        settings::SettingsStore,
    };

    fn unavailable_workspace_window(
        _: WorkspaceWindowRequest,
        _: &mut gpui::App,
    ) -> anyhow::Result<WorkspaceWindowHandle> {
        anyhow::bail!("not available in tests")
    }

    fn ignore_settings_window(_: &mut gpui::App) {}

    #[test]
    fn live_translation_pair_requires_both_selected_tabs() {
        let source = EntityId::from(1001);
        let translated = EntityId::from(1002);
        assert!(!live_translation_pair_is_active(
            source,
            translated,
            &HashSet::new()
        ));
        assert!(!live_translation_pair_is_active(
            source,
            translated,
            &HashSet::from([translated])
        ));
        assert!(live_translation_pair_is_active(
            source,
            translated,
            &HashSet::from([source, translated])
        ));
    }

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

    fn write_minimal_pdf_with_height(path: &Path, height: u32) {
        let page = format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 {height}] /Resources << >> /Contents 4 0 R >>"
        );
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>",
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
            page.as_str(),
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

    fn write_minimal_pdf(path: &Path) {
        write_minimal_pdf_with_height(path, 200);
    }

    fn write_encrypted_pdf(path: &Path) {
        let encoded = include_str!("../../pdf/tests/fixtures/dummy-encrypted.pdf.b64")
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect::<String>();
        fs::write(path, STANDARD.decode(encoded).unwrap()).unwrap();
    }

    fn encrypted_translation_reader_session(path: &Path) -> ReaderWindowSession {
        let document_tab = |id, zoom| ReaderTabSession {
            id,
            kind: ReaderTabKind::Document {
                path: path.to_path_buf(),
                position: DocumentPosition::fixed(0.0, 0.0, zoom, 1),
            },
        };
        ReaderWindowSession {
            bounds: ReaderWindowBounds {
                x: 0.0,
                y: 0.0,
                width: 1200.0,
                height: 800.0,
                state: ReaderWindowState::Windowed,
            },
            pane_tree: ReaderPaneTree::Split {
                axis: ReaderSplitAxis::Horizontal,
                ratios: vec![0.5, 0.5],
                children: vec![
                    ReaderPaneTree::Pane(ReaderPaneSession {
                        id: 1,
                        tabs: vec![document_tab(1, 1.0)],
                        active_tab: Some(1),
                    }),
                    ReaderPaneTree::Pane(ReaderPaneSession {
                        id: 2,
                        tabs: vec![document_tab(2, 1.5)],
                        active_tab: Some(2),
                    }),
                ],
            },
            active_pane: 1,
            dock_document_tab: Some(1),
            left_sidebar: None,
            ai_panel_open: false,
            transparency_enabled: true,
            synchronized_groups: Vec::new(),
            translation_sessions: vec![ReaderTranslationSession {
                source_tab: 1,
                translated_tab: 2,
                source_language: Some("en".to_string()),
                target_language: "zh-hans".to_string(),
                mode: "auto".to_string(),
                cache_key: None,
                original_unit_ids: vec!["encrypted-unit".to_string()],
            }],
        }
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
    fn documentless_workspace_starts_on_home_with_a_backing_new_tab(cx: &mut TestAppContext) {
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
            assert_eq!(workspace.surface_mode, WorkspaceSurfaceMode::Home);
            assert!(workspace.active_pane.read(cx).active_tab_is_new_tab());
        });
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        assert!(cx.debug_bounds("home-view").is_some());
        assert!(cx.debug_bounds("home-navigation-rail").is_some());
        assert!(cx.debug_bounds("workspace-home").is_some());
        assert!(cx.debug_bounds("workspace-pane-column").is_none());
    }

    #[gpui::test]
    fn zoom_actions_target_the_active_pane_and_show_a_local_transient_popup(
        cx: &mut TestAppContext,
    ) {
        let document_path = temporary_pdf_path("active-pane-zoom");
        write_minimal_pdf(&document_path);
        cx.update(|cx| {
            gpui_component::init(cx);
            crate::keybindings::capture_framework_bindings(cx);
            crate::keybindings::apply(cx, glyphst_settings::KeymapPreset::Modern, &[]);
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

        let (left_pane, right_pane, left_document, right_document, zoom_step) = workspace
            .update_in(cx, |workspace, window, cx| {
                let panes = workspace.pane_group.panes();
                let left_pane = panes[0].clone();
                let right_pane = panes[1].clone();
                let left_document = left_pane.read(cx).active_document_view().unwrap();
                let right_document = right_pane.read(cx).active_document_view().unwrap();

                // Keep descendant focus on the right to reproduce the stale focus path that
                // previously consumed document actions, while the workspace source of truth is
                // the left pane.
                right_pane.update(cx, |pane, cx| pane.focus(window, cx));
                workspace.active_pane = left_pane.clone();
                cx.notify();

                (
                    left_pane,
                    right_pane,
                    left_document,
                    right_document,
                    cx.global::<SettingsStore>().settings.behavior.zoom_step,
                )
            });
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });

        let zooms = |cx: &TestAppContext| {
            (
                left_document.read_with(cx, |view, _| view.current_document_position().zoom),
                right_document.read_with(cx, |view, _| view.current_document_position().zoom),
            )
        };
        assert_eq!(zooms(cx), (1.0, 1.0));

        cx.simulate_keystrokes("+");
        let (left_zoom, right_zoom) = zooms(cx);
        assert!((left_zoom - zoom_step).abs() < 0.0001);
        assert_eq!(right_zoom, 1.0);

        cx.simulate_keystrokes("=");
        assert_eq!(zooms(cx), (1.0, 1.0));

        cx.simulate_keystrokes("-");
        let (left_zoom, right_zoom) = zooms(cx);
        assert!((left_zoom - 1.0 / zoom_step).abs() < 0.0001);
        assert_eq!(right_zoom, 1.0);

        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        let popup = cx
            .debug_bounds("document-zoom-popup")
            .expect("zoom popup after a keyboard zoom");
        let left_bounds = workspace.read_with(cx, |workspace, _| {
            workspace
                .pane_group
                .bounding_box_for_pane(&left_pane)
                .expect("left pane bounds")
        });
        let right_bounds = workspace.read_with(cx, |workspace, _| {
            workspace
                .pane_group
                .bounding_box_for_pane(&right_pane)
                .expect("right pane bounds")
        });
        assert!(left_bounds.contains(&popup.center()));
        assert!(!right_bounds.contains(&popup.center()));
        assert!(cx.debug_bounds("zoom-popup-out").is_some());
        assert!(cx.debug_bounds("zoom-popup-reset").is_some());
        assert!(cx.debug_bounds("zoom-popup-in").is_some());
        let fit_width = cx
            .debug_bounds("zoom-popup-fit-width")
            .expect("fit-width control for a fixed-layout document");
        assert!(cx.debug_bounds("zoom-popup-fit-height").is_some());

        cx.simulate_click(fit_width.center(), Modifiers::default());
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        assert_eq!(zooms(cx), (1.0, 1.0));

        let popup = cx.debug_bounds("document-zoom-popup").unwrap();
        cx.simulate_mouse_move(popup.center(), None, Modifiers::default());
        cx.executor()
            .advance_clock(glyphst_ui::TRANSIENT_FEEDBACK_DURATION);
        cx.run_until_parked();
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        assert!(cx.debug_bounds("document-zoom-popup").is_some());

        cx.simulate_mouse_move(right_bounds.center(), None, Modifiers::default());
        cx.executor()
            .advance_clock(glyphst_ui::TRANSIENT_FEEDBACK_DURATION);
        cx.run_until_parked();
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        assert!(cx.debug_bounds("document-zoom-popup").is_none());

        fs::remove_file(document_path).unwrap();
    }

    #[gpui::test]
    fn tab_toggles_quick_ask_without_reentering_or_replacing_it(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            crate::keybindings::capture_framework_bindings(cx);
            crate::keybindings::apply(cx, glyphst_settings::KeymapPreset::Modern, &[]);
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
            workspace.focus_handle.focus(window, cx);
        });
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });

        cx.simulate_keystrokes("tab");
        let first_quick_ask = workspace.read_with(cx, |workspace, _| {
            assert!(workspace.quick_ask_visible);
            workspace.quick_ask.as_ref().unwrap().entity_id()
        });
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });

        cx.simulate_keystrokes("tab");
        workspace.read_with(cx, |workspace, _| {
            assert!(!workspace.quick_ask_visible);
            assert_eq!(
                workspace.quick_ask.as_ref().unwrap().entity_id(),
                first_quick_ask
            );
        });
        cx.update(|window, cx| {
            assert!(workspace.read(cx).focus_handle.is_focused(window));
            _ = window.draw(cx);
        });

        cx.simulate_keystrokes("tab");
        workspace.read_with(cx, |workspace, _| {
            assert!(workspace.quick_ask_visible);
            assert_eq!(
                workspace.quick_ask.as_ref().unwrap().entity_id(),
                first_quick_ask
            );
        });
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });

        cx.simulate_keystrokes("escape");
        workspace.read_with(cx, |workspace, _| {
            assert!(!workspace.quick_ask_visible);
            assert!(workspace.quick_ask.is_none());
        });
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });

        cx.simulate_keystrokes("tab");
        workspace.read_with(cx, |workspace, _| {
            assert!(workspace.quick_ask_visible);
            assert_ne!(
                workspace.quick_ask.as_ref().unwrap().entity_id(),
                first_quick_ask
            );
        });
    }

    #[gpui::test]
    fn quick_ask_positions_use_the_expected_workspace_bounds(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            crate::keybindings::capture_framework_bindings(cx);
            crate::keybindings::apply(cx, glyphst_settings::KeymapPreset::Modern, &[]);
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
        cx.simulate_resize(size(px(900.0), px(640.0)));
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.focus_handle.focus(window, cx);
        });
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        cx.simulate_keystrokes("tab");

        for (position, expected_top, expected_bottom) in [
            (glyphst_settings::QuickAskPosition::Top, Some(px(0.0)), None),
            (
                glyphst_settings::QuickAskPosition::Bottom,
                None,
                Some(px(640.0)),
            ),
        ] {
            workspace.update_in(cx, |_, _, cx| {
                cx.global_mut::<SettingsStore>()
                    .settings
                    .ai
                    .quick_ask
                    .position = position;
                cx.notify();
            });
            cx.update(|window, cx| {
                _ = window.draw(cx);
            });

            let bounds = cx
                .debug_bounds("workspace-quick-ask-panel")
                .expect("Quick Ask panel bounds");
            assert_eq!(bounds.left(), px(0.0));
            assert_eq!(bounds.size.width, px(900.0));
            if let Some(expected_top) = expected_top {
                assert_eq!(bounds.top(), expected_top);
            }
            if let Some(expected_bottom) = expected_bottom {
                assert_eq!(bounds.bottom(), expected_bottom);
            }
        }

        for (position, expected_left, expected_right) in [
            (
                glyphst_settings::QuickAskPosition::Left,
                Some(px(0.0)),
                None,
            ),
            (
                glyphst_settings::QuickAskPosition::Right,
                None,
                Some(px(900.0)),
            ),
        ] {
            workspace.update_in(cx, |_, _, cx| {
                cx.global_mut::<SettingsStore>()
                    .settings
                    .ai
                    .quick_ask
                    .position = position;
                cx.notify();
            });
            cx.update(|window, cx| {
                _ = window.draw(cx);
            });

            let bounds = cx
                .debug_bounds("workspace-quick-ask-panel")
                .expect("Quick Ask panel bounds");
            assert_eq!(bounds.top(), px(0.0));
            assert_eq!(bounds.size, size(px(360.0), px(640.0)));
            if let Some(expected_left) = expected_left {
                assert_eq!(bounds.left(), expected_left);
            }
            if let Some(expected_right) = expected_right {
                assert_eq!(bounds.right(), expected_right);
            }
        }

        workspace.update_in(cx, |_, _, cx| {
            cx.global_mut::<SettingsStore>()
                .settings
                .ai
                .quick_ask
                .position = glyphst_settings::QuickAskPosition::Center;
            cx.notify();
        });
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        let bounds = cx
            .debug_bounds("workspace-quick-ask-panel")
            .expect("Quick Ask panel bounds");
        assert_eq!(bounds.size.width, px(640.0));
        assert_eq!(bounds.center().x, px(450.0));
        assert_eq!(bounds.center().y, px(320.0));
    }

    #[gpui::test]
    fn workspace_ai_snapshot_prefers_document_focus_mru_over_active_pane(cx: &mut TestAppContext) {
        let left_path = temporary_pdf_path("ai-context-left");
        let right_path = temporary_pdf_path("ai-context-right");
        write_minimal_pdf(&left_path);
        write_minimal_pdf(&right_path);
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
        let initial_path = left_path.clone();
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

        let (left_pane, right_pane) = workspace.update_in(cx, |workspace, window, cx| {
            let panes = workspace.pane_group.panes();
            let left_pane = panes[0].clone();
            let right_pane = panes[1].clone();
            workspace.active_pane = right_pane.clone();
            right_pane
                .update(cx, |pane, cx| {
                    pane.focus_or_open_document(right_path.clone(), None, false, window, cx)
                })
                .unwrap();
            (left_pane, right_pane)
        });
        cx.run_until_parked();

        workspace.update_in(cx, |workspace, _, cx| {
            workspace.record_document_context(&right_pane, cx);
            workspace.active_pane = left_pane.clone();
            let snapshot = workspace.ai_context_snapshot(cx);
            assert_eq!(
                snapshot
                    .current_document
                    .as_ref()
                    .map(|document| document.read(cx).path().to_path_buf()),
                Some(right_path.clone())
            );
            assert_eq!(
                snapshot
                    .open_documents
                    .first()
                    .map(|document| document.read(cx).path().to_path_buf()),
                Some(right_path.clone())
            );
        });

        let provider = WorkspaceAiContextProvider::new(workspace.downgrade());
        let (scope, documents) = cx.update(|_, cx| {
            let scope = provider.document_scope(cx).expect("current document scope");
            let tools = provider.agent_tools(cx).expect("workspace document tools");
            let result = tools.execute(
                &AgentToolCall {
                    id: "list-current".to_string(),
                    name: "list_open_documents".to_string(),
                    arguments: "{}".to_string(),
                },
                &AgentCancellationToken::default(),
            );
            let documents = result
                .content
                .into_iter()
                .find_map(|part| match part {
                    AgentToolResultPart::Text(text) => Some(text),
                    AgentToolResultPart::Image(_) => None,
                })
                .expect("document list text");
            (scope, documents)
        });
        assert_eq!(
            scope,
            DocumentContextScope {
                path: right_path.clone(),
                label: right_path
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .to_string(),
            }
        );
        let current_line = documents
            .lines()
            .find(|line| line.contains("current context"))
            .expect("one current-context document");
        assert!(current_line.contains(right_path.file_name().unwrap().to_str().unwrap()));
        assert_eq!(
            documents
                .lines()
                .filter(|line| line.contains("current context"))
                .count(),
            1
        );

        workspace.update_in(cx, |workspace, window, cx| {
            workspace.close_tab_in_pane_now(&right_pane, None, window, cx);
        });
        cx.run_until_parked();
        workspace.read_with(cx, |workspace, cx| {
            assert_eq!(
                workspace
                    .ai_context_snapshot(cx)
                    .current_document
                    .map(|document| document.read(cx).path().to_path_buf()),
                Some(left_path.clone())
            );
        });

        fs::remove_file(left_path).unwrap();
        fs::remove_file(right_path).unwrap();
    }

    #[gpui::test]
    fn zen_mode_hides_workspace_chrome_preserves_state_and_restores_with_escape(
        cx: &mut TestAppContext,
    ) {
        let first_path = temporary_pdf_path("zen-mode-first");
        let second_path = temporary_pdf_path("zen-mode-second");
        write_minimal_pdf(&first_path);
        write_minimal_pdf(&second_path);
        cx.update(|cx| {
            gpui_component::init(cx);
            crate::keybindings::capture_framework_bindings(cx);
            crate::keybindings::apply(cx, glyphst_settings::KeymapPreset::Modern, &[]);
            cx.set_reduce_motion(true);
            cx.set_global(SettingsStore::memory());
            cx.set_global(LibraryStore::memory());
            cx.set_global(RenderCache::new());
        });
        let callbacks = WorkspaceCallbacks {
            open_workspace_window: unavailable_workspace_window,
            open_settings_window: ignore_settings_window,
        };
        let initial_path = first_path.clone();
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
        cx.update(|window, cx| {
            _ = window.draw(cx);
            assert!(
                window
                    .available_actions(cx)
                    .iter()
                    .any(|action| action.partial_eq(&ToggleZenMode))
            );
        });

        let fullscreen_before = workspace.update_in(cx, |workspace, window, cx| {
            workspace.toggle_left_sidebar(DocumentSidebar::Toc, window, cx);
            workspace.toggle_ai_dock(window, cx);
            let fullscreen = window.is_fullscreen();
            workspace.toggle_zen_mode(window, cx);
            assert!(workspace.zen_mode);
            assert_eq!(workspace.pane_group.panes().len(), 2);
            assert_eq!(workspace.docks.left_sidebar, Some(DocumentSidebar::Toc));
            assert!(workspace.docks.ai_panel_open);
            fullscreen
        });
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });

        assert!(cx.debug_bounds("workspace-zen-surface").is_some());
        assert!(cx.debug_bounds("workspace-title-bar-drag-region").is_none());
        assert!(cx.debug_bounds("workspace-pane-column").is_none());
        assert!(cx.debug_bounds("active-pane-surface").is_none());
        assert!(cx.debug_bounds("workspace-left-dock").is_none());
        assert!(cx.debug_bounds("workspace-ai-dock").is_none());
        assert!(cx.debug_bounds("workspace-pane-footbar").is_none());
        cx.update(|window, _| assert_eq!(window.is_fullscreen(), fullscreen_before));

        cx.simulate_keystrokes("tab");
        workspace.read_with(cx, |workspace, _| {
            assert!(workspace.zen_mode);
            assert!(workspace.quick_ask_visible);
        });
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        assert!(cx.debug_bounds("workspace-quick-ask-panel").is_some());
        assert!(cx.debug_bounds("workspace-zen-surface").is_some());
        assert!(cx.debug_bounds("workspace-pane-column").is_none());

        cx.simulate_keystrokes("escape");
        workspace.read_with(cx, |workspace, _| {
            assert!(workspace.zen_mode);
            assert!(!workspace.quick_ask_visible);
            assert!(workspace.quick_ask.is_none());
        });
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        assert!(cx.debug_bounds("workspace-quick-ask-panel").is_none());
        assert!(cx.debug_bounds("workspace-zen-surface").is_some());

        workspace.update_in(cx, |workspace, window, cx| {
            workspace
                .open_document_in_active_pane(second_path.clone(), None, window, cx)
                .unwrap();
            assert!(workspace.zen_mode);
            assert!(workspace.active_pane.read(cx).active_tab_is_loading());
        });
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        assert!(cx.debug_bounds("workspace-zen-surface").is_some());
        assert!(cx.debug_bounds("document-open-loading").is_some());
        cx.update(|window, cx| {
            assert!(
                window
                    .available_actions(cx)
                    .iter()
                    .any(|action| action.partial_eq(&ToggleZenMode))
            );
        });

        cx.run_until_parked();
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        assert!(cx.debug_bounds("document-open-loading").is_none());

        cx.simulate_keystrokes("escape");
        workspace.read_with(cx, |workspace, _| {
            assert!(!workspace.zen_mode);
            assert_eq!(workspace.pane_group.panes().len(), 2);
            assert_eq!(workspace.docks.left_sidebar, Some(DocumentSidebar::Toc));
            assert!(workspace.docks.ai_panel_open);
        });
        cx.update(|window, cx| {
            assert_eq!(window.is_fullscreen(), fullscreen_before);
            _ = window.draw(cx);
        });

        assert!(cx.debug_bounds("workspace-zen-surface").is_none());
        assert!(cx.debug_bounds("workspace-title-bar-drag-region").is_some());
        assert!(cx.debug_bounds("workspace-pane-column").is_some());
        assert!(cx.debug_bounds("active-pane-surface").is_some());
        assert!(cx.debug_bounds("workspace-left-dock").is_some());
        assert!(cx.debug_bounds("workspace-ai-dock").is_some());
        assert!(cx.debug_bounds("workspace-pane-footbar").is_some());

        workspace.update_in(cx, |workspace, window, cx| {
            workspace.enter_home(window, cx);
            workspace.toggle_zen_mode(window, cx);
            assert!(!workspace.zen_mode);
        });
        cx.update(|window, cx| {
            _ = window.draw(cx);
            assert!(
                window
                    .available_actions(cx)
                    .iter()
                    .all(|action| !action.partial_eq(&ToggleZenMode))
            );
        });

        fs::remove_file(first_path).unwrap();
        fs::remove_file(second_path).unwrap();
    }

    #[gpui::test]
    fn home_toggle_preserves_the_document_workspace_and_requested_docks(cx: &mut TestAppContext) {
        let document_path = temporary_pdf_path("home-toggle-preserves-workspace");
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
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        let document_rail = cx
            .debug_bounds("workspace-document-tool-rail")
            .expect("document navigation rail");
        let document_id = workspace.update_in(cx, |workspace, window, cx| {
            workspace.toggle_left_sidebar(DocumentSidebar::Toc, window, cx);
            let document_id = workspace
                .active_pane
                .read(cx)
                .active_document_view()
                .unwrap()
                .entity_id();
            workspace.enter_home(window, cx);
            document_id
        });
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        workspace.read_with(cx, |workspace, _| {
            assert_eq!(workspace.surface_mode, WorkspaceSurfaceMode::Home);
            assert_eq!(workspace.docks.left_sidebar, Some(DocumentSidebar::Toc));
        });
        assert!(cx.debug_bounds("home-view").is_some());
        assert!(cx.debug_bounds("toc-sidebar-content").is_none());
        let home_rail = cx
            .debug_bounds("home-navigation-rail")
            .expect("Home navigation rail");
        assert_eq!(home_rail.left(), document_rail.left());
        assert_eq!(home_rail.size.width, document_rail.size.width);
        assert_eq!(home_rail.left(), px(glyphst_ui::style::SURFACE_INSET));

        workspace.update_in(cx, |workspace, window, cx| {
            workspace.leave_home(window, cx);
        });
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        workspace.read_with(cx, |workspace, cx| {
            assert_eq!(workspace.surface_mode, WorkspaceSurfaceMode::Panes);
            assert_eq!(
                workspace.active_pane.read(cx).active_tab_id(),
                Some(document_id)
            );
        });
        assert!(cx.debug_bounds("toc-sidebar-content").is_some());

        fs::remove_file(document_path).unwrap();
    }

    #[gpui::test]
    fn closing_the_last_tab_enters_home_and_keeps_a_quick_open_tab(cx: &mut TestAppContext) {
        let document_path = temporary_pdf_path("last-tab-enters-home");
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
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.close_active_tab(window, cx);
        });
        workspace.read_with(cx, |workspace, cx| {
            assert_eq!(workspace.surface_mode, WorkspaceSurfaceMode::Home);
            assert_eq!(workspace.pane_group.panes().len(), 1);
            assert!(workspace.active_pane.read(cx).active_tab_is_new_tab());
        });

        fs::remove_file(document_path).unwrap();
    }

    #[gpui::test]
    fn home_open_targets_the_previous_active_pane_even_when_document_exists_elsewhere(
        cx: &mut TestAppContext,
    ) {
        let document_path = temporary_pdf_path("home-strict-active-pane");
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
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.split_active_pane(SplitDirection::Right, window, cx);
        });
        cx.run_until_parked();

        let (left_pane, right_pane) = workspace.update_in(cx, |workspace, window, cx| {
            let panes = workspace.pane_group.panes();
            let left_pane = panes[0].clone();
            let right_pane = panes[1].clone();
            right_pane.update(cx, |pane, cx| pane.open_new_tab(window, cx));
            workspace.close_tab_in_pane_now(&right_pane, Some(0), window, cx);
            workspace.active_pane = right_pane.clone();
            workspace.enter_home(window, cx);
            (left_pane, right_pane)
        });
        workspace.update_in(cx, |workspace, window, cx| {
            workspace
                .open_document_in_active_pane(document_path.clone(), None, window, cx)
                .unwrap();
        });
        cx.run_until_parked();

        workspace.read_with(cx, |workspace, cx| {
            assert_eq!(workspace.surface_mode, WorkspaceSurfaceMode::Panes);
            assert_eq!(workspace.active_pane, right_pane);
            assert!(
                left_pane
                    .read(cx)
                    .document_tab_ix(&document_path, cx)
                    .is_some()
            );
            assert!(
                right_pane
                    .read(cx)
                    .document_tab_ix(&document_path, cx)
                    .is_some()
            );
        });

        fs::remove_file(document_path).unwrap();
    }

    #[gpui::test]
    fn reader_session_restores_split_tabs_active_pane_and_docks(cx: &mut TestAppContext) {
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
        let session = ReaderWindowSession {
            bounds: ReaderWindowBounds {
                x: 0.0,
                y: 0.0,
                width: 1200.0,
                height: 800.0,
                state: ReaderWindowState::Windowed,
            },
            pane_tree: ReaderPaneTree::Split {
                axis: ReaderSplitAxis::Horizontal,
                ratios: vec![0.3, 0.7],
                children: vec![
                    ReaderPaneTree::Pane(ReaderPaneSession {
                        id: 1,
                        tabs: vec![ReaderTabSession {
                            id: 1,
                            kind: ReaderTabKind::NewTab(ReaderNewTabSession::default()),
                        }],
                        active_tab: Some(1),
                    }),
                    ReaderPaneTree::Pane(ReaderPaneSession {
                        id: 2,
                        tabs: vec![ReaderTabSession {
                            id: 2,
                            kind: ReaderTabKind::Keymap,
                        }],
                        active_tab: Some(2),
                    }),
                ],
            },
            active_pane: 2,
            dock_document_tab: None,
            left_sidebar: Some(ReaderSidebar::Notes),
            ai_panel_open: true,
            transparency_enabled: false,
            synchronized_groups: Vec::new(),
            translation_sessions: Vec::new(),
        };

        let (root, cx) = cx.add_window_view(move |window, cx| {
            let workspace =
                cx.new(|cx| Workspace::from_reader_session(session, callbacks, window, cx));
            Root::new(workspace, window, cx)
        });
        let workspace = root.read_with(cx, |root, _| {
            root.view().clone().downcast::<Workspace>().unwrap()
        });

        workspace.read_with(cx, |workspace, cx| {
            assert_eq!(workspace.pane_group.panes().len(), 2);
            assert_eq!(workspace.active_pane, workspace.pane_group.panes()[1]);
            assert_eq!(workspace.active_pane.read(cx).active_tab_ix(), 0);
            assert_eq!(workspace.docks.left_sidebar, Some(DocumentSidebar::Notes));
            assert!(workspace.docks.ai_panel_open);
            assert!(!workspace.transparency_enabled);
            assert!(!workspace.reader_session_restore_pending());
        });

        workspace.update_in(cx, |workspace, window, cx| {
            let snapshot = workspace.reader_window_session(window, cx);
            assert_eq!(snapshot.active_pane, 2);
            assert_eq!(snapshot.left_sidebar, Some(ReaderSidebar::Notes));
            let ReaderPaneTree::Split {
                axis,
                ratios,
                children,
            } = snapshot.pane_tree
            else {
                panic!("expected restored split tree");
            };
            assert_eq!(axis, ReaderSplitAxis::Horizontal);
            assert_eq!(children.len(), 2);
            assert_eq!(ratios.len(), 2);
            assert!((ratios[0] - 0.3).abs() < 0.001);
        });
    }

    #[gpui::test]
    fn encrypted_reader_session_unlocks_once_and_restores_its_translation(cx: &mut TestAppContext) {
        let document_path = temporary_pdf_path("restore-encrypted-translation");
        write_encrypted_pdf(&document_path);
        cx.update(|cx| {
            gpui_component::init(cx);
            cx.set_global(SettingsStore::memory());
            cx.set_global(LibraryStore::memory());
            cx.set_global(RenderCache::new());
            cx.set_global(PdfPasswordStore::memory());
        });
        let callbacks = WorkspaceCallbacks {
            open_workspace_window: unavailable_workspace_window,
            open_settings_window: ignore_settings_window,
        };
        let session = encrypted_translation_reader_session(&document_path);
        let (root, cx) = cx.add_window_view(move |window, cx| {
            let workspace =
                cx.new(|cx| Workspace::from_reader_session(session, callbacks, window, cx));
            Root::new(workspace, window, cx)
        });
        let workspace = root.read_with(cx, |root, _| {
            root.view().clone().downcast::<Workspace>().unwrap()
        });
        cx.run_until_parked();

        workspace.read_with(cx, |workspace, cx| {
            assert!(workspace.reader_session_restore_pending());
            assert!(workspace.live_translation_pairs.is_empty());
            assert!(
                workspace
                    .pane_group
                    .panes()
                    .iter()
                    .all(|pane| pane.read(cx).active_tab_is_loading())
            );
        });

        let source_pane =
            workspace.read_with(cx, |workspace, _| workspace.pane_group.panes()[0].clone());
        source_pane.update_in(cx, |pane, window, cx| {
            assert!(pane.submit_document_password_for_test(
                &document_path,
                "wrong-password",
                false,
                window,
                cx,
            ));
        });
        cx.run_until_parked();
        workspace.read_with(cx, |workspace, cx| {
            assert!(workspace.reader_session_restore_pending());
            assert!(workspace.live_translation_pairs.is_empty());
            assert!(
                workspace
                    .pane_group
                    .panes()
                    .iter()
                    .all(|pane| pane.read(cx).active_tab_is_loading())
            );
        });

        source_pane.update_in(cx, |pane, window, cx| {
            assert!(pane.submit_document_password_for_test(
                &document_path,
                "123456",
                false,
                window,
                cx,
            ));
        });
        cx.run_until_parked();
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        cx.run_until_parked();

        workspace.read_with(cx, |workspace, cx| {
            assert!(!workspace.reader_session_restore_pending());
            assert_eq!(workspace.live_translation_pairs.len(), 1);
            let panes = workspace.pane_group.panes();
            let source = panes[0].read(cx).document_views()[0].clone();
            let translated = panes[1].read(cx).document_views()[0].clone();
            assert!(source.read(cx).is_password_protected());
            assert!(translated.read(cx).is_password_protected());
            let (&session_id, pair) = workspace.live_translation_pairs.iter().next().unwrap();
            assert_eq!(pair.source_id, source.entity_id());
            assert_eq!(pair.translated_id, translated.entity_id());
            assert_eq!(
                translated.read(cx).translation_session_id(),
                Some(session_id)
            );
            assert_eq!(source.read(cx).translation_session_id(), None);
            assert_eq!(
                workspace
                    .synchronized_tabs
                    .group_for_tab(source.entity_id()),
                workspace
                    .synchronized_tabs
                    .group_for_tab(translated.entity_id())
            );
            assert!(
                workspace
                    .synchronized_tabs
                    .group_for_tab(source.entity_id())
                    .is_some()
            );
        });
        workspace.update_in(cx, |workspace, window, cx| {
            let snapshot = workspace.reader_window_session(window, cx);
            assert_eq!(snapshot.translation_sessions.len(), 1);
            assert_eq!(
                snapshot.translation_sessions[0].source_language.as_deref(),
                Some("en")
            );
            assert_eq!(snapshot.translation_sessions[0].target_language, "zh-hans");
        });
        assert!(cx.read(|cx| {
            !cx.global::<PdfPasswordStore>()
                .is_known_saved(&document_path)
        }));

        fs::remove_file(document_path).ok();
    }

    #[gpui::test]
    fn encrypted_reader_session_uses_a_saved_password_for_both_translation_tabs(
        cx: &mut TestAppContext,
    ) {
        let document_path = temporary_pdf_path("restore-encrypted-translation-saved");
        write_encrypted_pdf(&document_path);
        let password_store = PdfPasswordStore::memory();
        cx.update(|cx| {
            gpui_component::init(cx);
            cx.set_global(SettingsStore::memory());
            cx.set_global(LibraryStore::memory());
            cx.set_global(RenderCache::new());
            cx.set_global(password_store.clone());
        });
        let save = cx
            .update(|cx| password_store.write(&document_path, DocumentPassword::new("123456"), cx));
        cx.foreground_executor().block_test(save).unwrap();

        let callbacks = WorkspaceCallbacks {
            open_workspace_window: unavailable_workspace_window,
            open_settings_window: ignore_settings_window,
        };
        let session = encrypted_translation_reader_session(&document_path);
        let (root, cx) = cx.add_window_view(move |window, cx| {
            let workspace =
                cx.new(|cx| Workspace::from_reader_session(session, callbacks, window, cx));
            Root::new(workspace, window, cx)
        });
        let workspace = root.read_with(cx, |root, _| {
            root.view().clone().downcast::<Workspace>().unwrap()
        });
        cx.run_until_parked();
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        cx.run_until_parked();

        workspace.read_with(cx, |workspace, cx| {
            assert!(!workspace.reader_session_restore_pending());
            assert_eq!(workspace.live_translation_pairs.len(), 1);
            assert!(
                workspace.pane_group.panes().iter().all(|pane| pane
                    .read(cx)
                    .document_views()
                    .len()
                    == 1)
            );
        });
        assert!(password_store.is_known_saved(&document_path));

        fs::remove_file(document_path).ok();
    }

    #[gpui::test]
    fn reader_session_restores_duplicate_documents_synchronization_and_translation(
        cx: &mut TestAppContext,
    ) {
        let document_path = temporary_pdf_path("restore-duplicates");
        write_minimal_pdf_with_height(&document_path, 1_000);
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
        let document_tab = |id, zoom| ReaderTabSession {
            id,
            kind: ReaderTabKind::Document {
                path: document_path.clone(),
                position: DocumentPosition::fixed(0.0, 0.0, zoom, 1),
            },
        };
        let session = ReaderWindowSession {
            bounds: ReaderWindowBounds {
                x: 0.0,
                y: 0.0,
                width: 1200.0,
                height: 800.0,
                state: ReaderWindowState::Windowed,
            },
            pane_tree: ReaderPaneTree::Split {
                axis: ReaderSplitAxis::Horizontal,
                ratios: vec![0.5, 0.5],
                children: vec![
                    ReaderPaneTree::Pane(ReaderPaneSession {
                        id: 1,
                        tabs: vec![document_tab(1, 1.0), document_tab(2, 1.5)],
                        active_tab: Some(2),
                    }),
                    ReaderPaneTree::Pane(ReaderPaneSession {
                        id: 2,
                        tabs: vec![document_tab(3, 2.0)],
                        active_tab: Some(3),
                    }),
                ],
            },
            active_pane: 1,
            dock_document_tab: Some(2),
            left_sidebar: None,
            ai_panel_open: false,
            transparency_enabled: true,
            synchronized_groups: vec![ReaderSynchronizedGroup {
                tabs: vec![2, 3],
                paused_tabs: vec![3],
            }],
            translation_sessions: vec![ReaderTranslationSession {
                source_tab: 2,
                translated_tab: 3,
                source_language: Some("en".to_string()),
                target_language: "zh-hans".to_string(),
                mode: "auto".to_string(),
                cache_key: None,
                original_unit_ids: vec!["unit-1".to_string()],
            }],
        };

        let (root, cx) = cx.add_window_view(move |window, cx| {
            let workspace =
                cx.new(|cx| Workspace::from_reader_session(session, callbacks, window, cx));
            Root::new(workspace, window, cx)
        });
        let workspace = root.read_with(cx, |root, _| {
            root.view().clone().downcast::<Workspace>().unwrap()
        });
        cx.run_until_parked();
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        cx.run_until_parked();

        workspace.read_with(cx, |workspace, cx| {
            assert!(!workspace.reader_session_restore_pending());
            let panes = workspace.pane_group.panes();
            let left = panes[0].read(cx);
            let right = panes[1].read(cx);
            let left_views = left.document_views();
            let right_views = right.document_views();
            assert_eq!(left_views.len(), 2);
            assert_eq!(right_views.len(), 1);
            assert_ne!(left_views[0], left_views[1]);
            assert_eq!(left.active_tab_ix(), 1);
            assert_eq!(
                workspace
                    .dock_document_target(cx)
                    .map(|(_, document)| document.entity_id()),
                Some(left_views[1].entity_id())
            );
            let left_group = workspace
                .synchronized_tabs
                .group_for_tab(left_views[1].entity_id())
                .unwrap();
            assert_eq!(
                workspace
                    .synchronized_tabs
                    .group_for_tab(right_views[0].entity_id()),
                Some(left_group)
            );
            assert!(
                !workspace
                    .synchronized_tabs
                    .is_paused(right_views[0].entity_id())
            );
            assert!(right.synchronized_pause_locked(right_views[0].entity_id()));
            assert!(left.synchronized_pause_locked(left_views[1].entity_id()));
            assert_eq!(workspace.live_translation_pairs.len(), 1);
            let (&session_id, pair) = workspace.live_translation_pairs.iter().next().unwrap();
            assert_eq!(pair.source_id, left_views[1].entity_id());
            assert_eq!(pair.translated_id, right_views[0].entity_id());
            assert_eq!(
                right_views[0].read(cx).translation_session_id(),
                Some(session_id)
            );
            assert_eq!(left_views[1].read(cx).translation_session_id(), None);
            let translation = workspace
                .document_translation_jobs
                .read(cx)
                .session_snapshot(session_id)
                .unwrap();
            assert!(translation.can_resume());
            assert_eq!(
                translation
                    .request
                    .source_language
                    .as_ref()
                    .unwrap()
                    .as_str(),
                "en"
            );
            assert_eq!(translation.request.target_language.as_str(), "zh-hans");
            assert!(translation.original_unit_ids.contains("unit-1"));

            let source_anchor = left_views[1]
                .read(cx)
                .live_translation_viewport_anchor(SynchronizedScrollMode::Immediate)
                .unwrap();
            let translated_anchor = right_views[0]
                .read(cx)
                .live_translation_viewport_anchor(SynchronizedScrollMode::Immediate)
                .unwrap();
            assert_eq!(source_anchor.page_index, translated_anchor.page_index);
            assert!((source_anchor.pdf_x - translated_anchor.pdf_x).abs() < 0.01);
            assert!((source_anchor.pdf_y - translated_anchor.pdf_y).abs() < 0.01);
            assert_eq!(left_views[1].read(cx).current_document_position().zoom, 1.5);
            assert_eq!(
                right_views[0].read(cx).current_document_position().zoom,
                2.0
            );
        });

        let (source, translated) = workspace.read_with(cx, |workspace, _| {
            let pair = workspace.live_translation_pairs.values().next().unwrap();
            (
                pair.source.upgrade().unwrap(),
                pair.translated.upgrade().unwrap(),
            )
        });
        let overlay_button_bounds = cx
            .debug_bounds("translation-session-toggle-overlays")
            .unwrap();
        cx.simulate_click(overlay_button_bounds.center(), Modifiers::default());
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        assert!(!translated.read_with(cx, |view, _| view.translation_overlay_enabled()));
        assert!(!source.read_with(cx, |view, _| view.translation_overlay_enabled()));
        let overlay_button_bounds = cx
            .debug_bounds("translation-session-toggle-overlays")
            .unwrap();
        cx.simulate_click(overlay_button_bounds.center(), Modifiers::default());
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        assert!(translated.read_with(cx, |view, _| view.translation_overlay_enabled()));
        assert!(source.read_with(cx, |view, _| view.translation_overlay_enabled()));

        cx.update(|_, cx| {
            let source_anchor = source
                .read(cx)
                .live_translation_viewport_anchor(SynchronizedScrollMode::Immediate)
                .unwrap();
            let translated_anchor = translated
                .read(cx)
                .live_translation_viewport_anchor(SynchronizedScrollMode::Immediate)
                .unwrap();
            assert!(
                (source_anchor.pdf_y - translated_anchor.pdf_y).abs() < 0.01,
                "translation anchors diverged before resize: source={source_anchor:?}, translated={translated_anchor:?}"
            );
        });

        cx.simulate_resize(size(px(900.0), px(640.0)));
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        cx.update(|_, cx| {
            let source_anchor = source
                .read(cx)
                .live_translation_viewport_anchor(SynchronizedScrollMode::Immediate)
                .unwrap();
            let translated_anchor = translated
                .read(cx)
                .live_translation_viewport_anchor(SynchronizedScrollMode::Immediate)
                .unwrap();
            assert!((source_anchor.pdf_x - translated_anchor.pdf_x).abs() < 0.01);
            assert!(
                (source_anchor.pdf_y - translated_anchor.pdf_y).abs() < 0.01,
                "translation anchors diverged after resize: source={source_anchor:?} at {:?}, translated={translated_anchor:?} at {:?}",
                source.read(cx).current_document_position(),
                translated.read(cx).current_document_position(),
            );
        });

        workspace.update_in(cx, |workspace, window, cx| {
            workspace.split_active_pane(SplitDirection::Right, window, cx);
        });
        cx.run_until_parked();
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        workspace.update_in(cx, |workspace, _, cx| {
            let third_pane = workspace.active_pane.clone();
            let third = third_pane.read(cx).active_document_view().unwrap();
            let group_id = workspace
                .synchronized_tabs
                .group_for_tab(source.entity_id())
                .unwrap();
            assert!(workspace.synchronized_tabs.add_member(
                group_id,
                third.entity_id(),
                third_pane.entity_id(),
            ));
            workspace.refresh_synchronized_tab_presentation(cx);
            let delta = SynchronizedScrollDelta {
                x_pixels: 0.0,
                y_pixels: 48.0,
                mode: SynchronizedScrollMode::Immediate,
            };
            third.update(cx, |view, cx| {
                view.apply_synchronized_scroll_delta(delta, cx);
            });
            workspace.propagate_synchronized_scroll(third.entity_id(), delta, cx);
            assert_eq!(
                workspace
                    .synchronized_tabs
                    .group_summary(group_id)
                    .unwrap()
                    .member_count,
                3
            );
            let source_anchor = source
                .read(cx)
                .live_translation_viewport_anchor(SynchronizedScrollMode::Immediate)
                .unwrap();
            let translated_anchor = translated
                .read(cx)
                .live_translation_viewport_anchor(SynchronizedScrollMode::Immediate)
                .unwrap();
            assert!((source_anchor.pdf_x - translated_anchor.pdf_x).abs() < 0.01);
            assert!(
                (source_anchor.pdf_y - translated_anchor.pdf_y).abs() < 0.01,
                "translation anchors diverged after a third synchronized tab moved: source={source_anchor:?}, translated={translated_anchor:?}"
            );
            assert!(
                workspace
                    .synchronized_tabs
                    .set_paused(third.entity_id(), true)
            );
            assert!(workspace.synchronized_tabs.is_paused(third.entity_id()));
            workspace
                .synchronized_tabs
                .set_paused(third.entity_id(), false);
        });

        let (source, translated, translated_pane, translated_id, before) =
            workspace.update_in(cx, |workspace, window, cx| {
                let pair = workspace.live_translation_pairs.values().next().unwrap();
                let source = pair.source.upgrade().unwrap();
                let translated = pair.translated.upgrade().unwrap();
                let translated_id = translated.entity_id();
                let translated_pane = workspace
                    .pane_group
                    .panes()
                    .into_iter()
                    .find(|pane| pane.read(cx).has_tab_id(translated_id))
                    .unwrap();
                translated_pane.update(cx, |pane, cx| pane.open_new_tab(window, cx));

                let before = translated.read(cx).current_document_position();
                source.update(cx, |view, cx| {
                    view.navigate_to_document_position(
                        DocumentPosition::fixed(0.0, 80.0, 4.0, 1),
                        cx,
                    );
                });
                (source, translated, translated_pane, translated_id, before)
            });
        cx.run_until_parked();
        let after = translated.read_with(cx, |view, _| view.current_document_position());
        assert!(
            (after.scroll_y_doc - before.scroll_y_doc).abs() > 0.01,
            "hidden translated position did not update: before={before:?}, after={after:?}"
        );
        cx.update(|_, cx| {
            let source_anchor = source
                .read(cx)
                .live_translation_viewport_anchor(SynchronizedScrollMode::Immediate)
                .unwrap();
            let translated_anchor = translated
                .read(cx)
                .live_translation_viewport_anchor(SynchronizedScrollMode::Immediate)
                .unwrap();
            assert!((source_anchor.pdf_x - translated_anchor.pdf_x).abs() < 0.01);
            assert!((source_anchor.pdf_y - translated_anchor.pdf_y).abs() < 0.01);
        });

        translated_pane.update(cx, |_, cx| {
            cx.emit(PaneEvent::SetSynchronizedScrollPaused {
                tab_id: translated_id,
                paused: true,
            });
        });
        cx.run_until_parked();
        workspace.update_in(cx, |workspace, window, cx| {
            assert!(!workspace.synchronized_tabs.is_paused(translated_id));
            let snapshot = workspace.reader_window_session(window, cx);
            assert!(
                snapshot
                    .synchronized_groups
                    .iter()
                    .all(|group| group.paused_tabs.is_empty())
            );
        });

        fs::remove_file(document_path).ok();
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
    fn documentless_tabs_suspend_document_docks_but_keep_global_ai(cx: &mut TestAppContext) {
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
            workspace.leave_home(window, cx);
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
        assert!(cx.debug_bounds("workspace-ai-dock").is_some());
    }

    #[gpui::test]
    fn global_ai_dock_omits_the_focused_document_label(cx: &mut TestAppContext) {
        let document_path = temporary_pdf_path("ai-dock-without-document-label");
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
            assert!(workspace.ai_context_document(cx).is_some());
            workspace.toggle_ai_dock(window, cx);
        });
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });

        assert!(cx.debug_bounds("workspace-ai-dock").is_some());
        assert!(cx.debug_bounds("workspace-dock-document-context").is_none());

        fs::remove_file(document_path).unwrap();
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
    fn dropped_documents_open_in_order_and_report_skipped_items(cx: &mut TestAppContext) {
        let first_path = temporary_pdf_path("external-drop-first");
        let second_path = temporary_pdf_path("external-drop-second");
        let unsupported_path =
            temporary_pdf_path("external-drop-unsupported").with_extension("txt");
        write_minimal_pdf(&first_path);
        write_minimal_pdf(&second_path);
        fs::write(&unsupported_path, b"not a document").unwrap();

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

        workspace.update_in(cx, |workspace, window, cx| {
            workspace.open_external_document_paths(
                &[
                    first_path.clone(),
                    unsupported_path.clone(),
                    second_path.clone(),
                ],
                window,
                cx,
            );
            assert_eq!(window.notifications(cx).len(), 1);
        });
        cx.run_until_parked();

        workspace.read_with(cx, |workspace, cx| {
            assert_eq!(workspace.surface_mode, WorkspaceSurfaceMode::Panes);
            let pane = workspace.active_pane.read(cx);
            assert_eq!(pane.len(), 2);
            assert_eq!(pane.active_tab_ix(), 1);
            let paths = pane
                .document_views()
                .into_iter()
                .map(|view| view.read(cx).path().to_path_buf())
                .collect::<Vec<_>>();
            assert_eq!(paths, vec![first_path.clone(), second_path.clone()]);
        });

        fs::remove_file(first_path).unwrap();
        fs::remove_file(second_path).unwrap();
        fs::remove_file(unsupported_path).unwrap();
    }

    #[gpui::test]
    fn pane_drop_opens_documents_in_the_hovered_inactive_pane(cx: &mut TestAppContext) {
        let first_path = temporary_pdf_path("pane-drop-first");
        let second_path = temporary_pdf_path("pane-drop-second");
        let unsupported_path = temporary_pdf_path("pane-drop-unsupported").with_extension("txt");
        write_minimal_pdf(&first_path);
        write_minimal_pdf(&second_path);
        fs::write(&unsupported_path, b"not a document").unwrap();

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
        let hovered_pane = workspace.read_with(cx, |workspace, _| workspace.active_pane.clone());
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.leave_home(window, cx);
            workspace.split_active_pane(SplitDirection::Right, window, cx);
        });
        let previously_active_pane =
            workspace.read_with(cx, |workspace, _| workspace.active_pane.clone());
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        let drop_position = workspace.read_with(cx, |workspace, _| {
            workspace
                .pane_group
                .bounding_box_for_pane(&hovered_pane)
                .expect("hovered pane bounds")
                .center()
        });
        cx.simulate_event(FileDropEvent::Entered {
            position: drop_position,
            paths: ExternalPaths(
                [
                    first_path.clone(),
                    unsupported_path.clone(),
                    second_path.clone(),
                ]
                .into_iter()
                .collect(),
            ),
        });
        cx.simulate_event(FileDropEvent::Submit {
            position: drop_position,
        });
        cx.run_until_parked();

        workspace.update_in(cx, |workspace, window, cx| {
            assert_eq!(workspace.active_pane, hovered_pane);
            assert_eq!(window.notifications(cx).len(), 1);
            let hovered = hovered_pane.read(cx);
            assert_eq!(hovered.len(), 2);
            assert_eq!(hovered.active_tab_ix(), 1);
            let paths = hovered
                .document_views()
                .into_iter()
                .map(|view| view.read(cx).path().to_path_buf())
                .collect::<Vec<_>>();
            assert_eq!(paths, vec![first_path.clone(), second_path.clone()]);
            let previous = previously_active_pane.read(cx);
            assert_eq!(previous.len(), 0);
        });

        fs::remove_file(first_path).unwrap();
        fs::remove_file(second_path).unwrap();
        fs::remove_file(unsupported_path).unwrap();
    }

    #[gpui::test]
    fn unsupported_pane_drop_does_not_activate_the_hovered_pane(cx: &mut TestAppContext) {
        let unsupported_path = temporary_pdf_path("pane-drop-invalid").with_extension("txt");
        fs::write(&unsupported_path, b"not a document").unwrap();

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
        let hovered_pane = workspace.read_with(cx, |workspace, _| workspace.active_pane.clone());
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.leave_home(window, cx);
            workspace.split_active_pane(SplitDirection::Right, window, cx);
        });
        let active_pane = workspace.read_with(cx, |workspace, _| workspace.active_pane.clone());
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        let drop_position = workspace.read_with(cx, |workspace, _| {
            workspace
                .pane_group
                .bounding_box_for_pane(&hovered_pane)
                .expect("hovered pane bounds")
                .center()
        });
        cx.simulate_event(FileDropEvent::Entered {
            position: drop_position,
            paths: ExternalPaths([unsupported_path.clone()].into_iter().collect()),
        });
        cx.simulate_event(FileDropEvent::Submit {
            position: drop_position,
        });
        cx.run_until_parked();

        workspace.update_in(cx, |workspace, window, cx| {
            assert_eq!(workspace.active_pane, active_pane);
            assert_eq!(window.notifications(cx).len(), 1);
            assert!(hovered_pane.read(cx).active_tab_is_new_tab());
        });

        fs::remove_file(unsupported_path).unwrap();
    }

    #[gpui::test]
    fn external_drop_overlay_is_scoped_to_the_hovered_pane(cx: &mut TestAppContext) {
        let document_path = temporary_pdf_path("pane-drop-overlay");
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
        let (root, cx) = cx.add_window_view(|window, cx| {
            let workspace = cx.new(|cx| Workspace::new(None, None, callbacks, window, cx));
            Root::new(workspace, window, cx)
        });
        let workspace = root.read_with(cx, |root, _| {
            root.view().clone().downcast::<Workspace>().unwrap()
        });
        let hovered_pane = workspace.read_with(cx, |workspace, _| workspace.active_pane.clone());
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.leave_home(window, cx);
            workspace.split_active_pane(SplitDirection::Right, window, cx);
        });
        let other_pane = workspace.read_with(cx, |workspace, _| workspace.active_pane.clone());
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        let drop_position = workspace.read_with(cx, |workspace, _| {
            workspace
                .pane_group
                .bounding_box_for_pane(&hovered_pane)
                .expect("hovered pane bounds")
                .center()
        });
        cx.simulate_event(FileDropEvent::Entered {
            position: drop_position,
            paths: ExternalPaths([document_path.clone()].into_iter().collect()),
        });
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });

        let hovered_selector = Box::leak(
            format!(
                "pane-external-document-drop-overlay-{}-accepted",
                hovered_pane.entity_id().as_u64()
            )
            .into_boxed_str(),
        );
        let other_selector = Box::leak(
            format!(
                "pane-external-document-drop-overlay-{}-accepted",
                other_pane.entity_id().as_u64()
            )
            .into_boxed_str(),
        );
        let overlay_bounds = cx
            .debug_bounds(hovered_selector)
            .expect("hovered pane drop overlay");
        let (hovered_bounds, other_bounds) = workspace.read_with(cx, |workspace, _| {
            (
                workspace
                    .pane_group
                    .bounding_box_for_pane(&hovered_pane)
                    .expect("hovered pane bounds"),
                workspace
                    .pane_group
                    .bounding_box_for_pane(&other_pane)
                    .expect("other pane bounds"),
            )
        });
        assert!(overlay_bounds.left() >= hovered_bounds.left());
        assert!(overlay_bounds.top() >= hovered_bounds.top());
        assert!(overlay_bounds.right() <= hovered_bounds.right());
        assert!(overlay_bounds.bottom() <= hovered_bounds.bottom());
        assert!(!overlay_bounds.intersects(&other_bounds));
        assert!(cx.debug_bounds(other_selector).is_none());
        assert_ne!(
            overlay_bounds,
            cx.debug_bounds("workspace-pane-column").unwrap()
        );

        cx.simulate_event(FileDropEvent::Pending {
            position: other_bounds.center(),
        });
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        assert!(cx.debug_bounds(hovered_selector).is_none());
        let other_overlay_bounds = cx
            .debug_bounds(other_selector)
            .expect("newly hovered pane drop overlay");
        assert!(other_overlay_bounds.left() >= other_bounds.left());
        assert!(other_overlay_bounds.top() >= other_bounds.top());
        assert!(other_overlay_bounds.right() <= other_bounds.right());
        assert!(other_overlay_bounds.bottom() <= other_bounds.bottom());
        assert!(!other_overlay_bounds.intersects(&hovered_bounds));

        cx.simulate_event(FileDropEvent::Exited);
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        assert!(cx.debug_bounds(other_selector).is_none());
        fs::remove_file(document_path).unwrap();
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
        let home = cx.debug_bounds("workspace-home").unwrap();
        let glyphst_menu = cx.debug_bounds("workspace-menu-glyphst").unwrap();
        assert!(home.right() <= glyphst_menu.left());

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
            workspace.leave_home(window, cx);
            workspace.split_active_pane(SplitDirection::Right, window, cx);
        });
        let top_right = workspace.read_with(cx, |workspace, _| workspace.active_pane.clone());
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });

        let (initial_left_bounds, initial_right_bounds) =
            workspace.read_with(cx, |workspace, _| {
                (
                    workspace
                        .pane_group
                        .bounding_box_for_pane(&top_left)
                        .expect("left pane bounds"),
                    workspace
                        .pane_group
                        .bounding_box_for_pane(&top_right)
                        .expect("right pane bounds"),
                )
            });
        let horizontal_indicator = cx
            .debug_bounds("resizable-horizontal-handle-indicator")
            .expect("horizontal resize indicator");
        let active_surface = cx
            .debug_bounds("active-pane-surface")
            .expect("right active pane surface");
        assert_eq!(active_surface, initial_right_bounds);
        assert!(horizontal_indicator.left() > initial_left_bounds.right());
        assert!(horizontal_indicator.right() < active_surface.left());

        workspace.update_in(cx, |workspace, window, cx| {
            workspace.split_active_pane(SplitDirection::Down, window, cx);
        });
        let bottom_right = workspace.read_with(cx, |workspace, _| workspace.active_pane.clone());
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });

        let (right_top_bounds, right_bottom_bounds) = workspace.read_with(cx, |workspace, _| {
            (
                workspace
                    .pane_group
                    .bounding_box_for_pane(&top_right)
                    .expect("top-right pane bounds"),
                workspace
                    .pane_group
                    .bounding_box_for_pane(&bottom_right)
                    .expect("bottom-right pane bounds"),
            )
        });
        let vertical_indicator = cx
            .debug_bounds("resizable-vertical-handle-indicator")
            .expect("vertical resize indicator");
        let active_surface = cx
            .debug_bounds("active-pane-surface")
            .expect("bottom active pane surface");
        assert_eq!(active_surface, right_bottom_bounds);
        assert!(vertical_indicator.top() > right_top_bounds.bottom());
        assert!(vertical_indicator.bottom() < active_surface.top());

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

    #[gpui::test]
    fn single_pane_new_tab_suspends_document_sidebar_but_keeps_global_ai(cx: &mut TestAppContext) {
        let document_path = temporary_pdf_path("new-tab-sticky-docks");
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

        let document_id = workspace.update_in(cx, |workspace, window, cx| {
            let document_id = workspace
                .active_pane
                .read(cx)
                .active_document_view()
                .unwrap()
                .entity_id();
            workspace.toggle_left_sidebar(DocumentSidebar::Toc, window, cx);
            workspace.toggle_ai_dock(window, cx);
            workspace.open_new_tab(window, cx);
            document_id
        });
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });

        workspace.read_with(cx, |workspace, cx| {
            assert!(workspace.active_pane.read(cx).active_tab_is_new_tab());
            assert!(workspace.dock_document_target(cx).is_none());
            assert_eq!(workspace.docks.left_sidebar, Some(DocumentSidebar::Toc));
            assert!(workspace.docks.ai_panel_open);
        });
        assert!(cx.debug_bounds("workspace-left-dock").is_none());
        assert!(cx.debug_bounds("workspace-ai-dock").is_some());
        assert!(cx.debug_bounds("toc-sidebar-content").is_none());

        workspace.update_in(cx, |workspace, window, cx| {
            let pane = workspace.active_pane.clone();
            assert!(pane.update(cx, |pane, cx| {
                pane.activate_tab_by_id(document_id, true, window, cx)
            }));
            workspace.record_document_context(&pane, cx);
        });
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        workspace.read_with(cx, |workspace, cx| {
            assert_eq!(
                workspace.active_pane.read(cx).active_tab_id(),
                Some(document_id)
            );
        });
        assert!(cx.debug_bounds("workspace-left-dock").is_some());
        assert!(cx.debug_bounds("workspace-ai-dock").is_some());
        assert!(cx.debug_bounds("toc-sidebar-content").is_some());

        fs::remove_file(document_path).unwrap();
    }

    #[gpui::test]
    fn closing_new_tab_in_inactive_pane_does_not_activate_it_or_hide_sidebar(
        cx: &mut TestAppContext,
    ) {
        let document_path = temporary_pdf_path("inactive-pane-new-tab-close");
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
            workspace.split_active_pane(SplitDirection::Right, window, cx);
        });
        cx.run_until_parked();

        let (left_pane, right_pane, left_document_id) =
            workspace.update_in(cx, |workspace, window, cx| {
                let panes = workspace.pane_group.panes();
                let left_pane = panes[0].clone();
                let right_pane = panes[1].clone();
                right_pane.update(cx, |pane, cx| pane.open_new_tab(window, cx));
                workspace.active_pane = left_pane.clone();
                left_pane.update(cx, |pane, cx| pane.focus(window, cx));
                workspace.record_document_context(&left_pane, cx);
                workspace.toggle_left_sidebar(DocumentSidebar::Toc, window, cx);
                let left_document_id = left_pane
                    .read(cx)
                    .active_document_view()
                    .unwrap()
                    .entity_id();
                (left_pane, right_pane, left_document_id)
            });
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        assert!(right_pane.read_with(cx, |pane, _| pane.active_tab_is_new_tab()));
        assert!(cx.debug_bounds("toc-sidebar-content").is_some());

        let close = cx
            .debug_bounds("pane-tab-close-1")
            .expect("inactive pane New Tab close button")
            .center();
        cx.simulate_mouse_down(close, MouseButton::Left, Modifiers::default());
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        workspace.read_with(cx, |workspace, cx| {
            assert_eq!(workspace.active_pane, left_pane);
            assert_eq!(
                workspace
                    .dock_document_target(cx)
                    .map(|(_, document)| document.entity_id()),
                Some(left_document_id)
            );
        });
        assert!(cx.debug_bounds("toc-sidebar-content").is_some());

        cx.simulate_mouse_up(close, MouseButton::Left, Modifiers::default());
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        workspace.read_with(cx, |workspace, cx| {
            assert_eq!(workspace.active_pane, left_pane);
            assert_eq!(right_pane.read(cx).len(), 1);
            assert!(!right_pane.read(cx).active_tab_is_new_tab());
            assert_eq!(
                workspace
                    .dock_document_target(cx)
                    .map(|(_, document)| document.entity_id()),
                Some(left_document_id)
            );
        });
        assert!(cx.debug_bounds("toc-sidebar-content").is_some());

        fs::remove_file(document_path).unwrap();
    }

    #[gpui::test]
    fn removing_dock_owner_pane_does_not_fall_back_to_hidden_document(cx: &mut TestAppContext) {
        let document_path = temporary_pdf_path("removed-owner-hidden-document");
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
            workspace.split_active_pane(SplitDirection::Right, window, cx);
        });
        cx.run_until_parked();

        let (left_pane, right_pane, right_document_id) =
            workspace.update_in(cx, |workspace, window, cx| {
                let panes = workspace.pane_group.panes();
                let left_pane = panes[0].clone();
                let right_pane = panes[1].clone();
                let right_document_id = right_pane
                    .read(cx)
                    .active_document_view()
                    .expect("split document")
                    .entity_id();
                right_pane.update(cx, |pane, cx| pane.open_new_tab(window, cx));
                workspace.active_pane = left_pane.clone();
                left_pane.update(cx, |pane, cx| pane.focus(window, cx));
                workspace.record_document_context(&left_pane, cx);
                workspace.toggle_left_sidebar(DocumentSidebar::Toc, window, cx);
                workspace.active_pane = right_pane.clone();
                right_pane.update(cx, |pane, cx| pane.focus(window, cx));
                (left_pane, right_pane, right_document_id)
            });
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        assert!(cx.debug_bounds("toc-sidebar-content").is_some());

        workspace.update_in(cx, |workspace, window, cx| {
            workspace.close_tab_in_pane_now(&left_pane, None, window, cx);
        });
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        workspace.read_with(cx, |workspace, cx| {
            assert_eq!(workspace.pane_group.panes().len(), 1);
            assert_eq!(workspace.active_pane, right_pane);
            assert!(right_pane.read(cx).active_tab_is_new_tab());
            assert!(workspace.dock_document_target(cx).is_none());
            assert_eq!(workspace.docks.left_sidebar, Some(DocumentSidebar::Toc));
        });
        assert!(cx.debug_bounds("workspace-left-dock").is_none());
        assert!(cx.debug_bounds("toc-sidebar-content").is_none());

        workspace.update_in(cx, |workspace, window, cx| {
            assert!(right_pane.update(cx, |pane, cx| {
                pane.activate_tab_by_id(right_document_id, true, window, cx)
            }));
            workspace.record_document_context(&right_pane, cx);
        });
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        workspace.read_with(cx, |workspace, cx| {
            assert_eq!(
                workspace
                    .dock_document_target(cx)
                    .map(|(_, document)| document.entity_id()),
                Some(right_document_id)
            );
        });
        assert!(cx.debug_bounds("toc-sidebar-content").is_some());

        fs::remove_file(document_path).unwrap();
    }
}
