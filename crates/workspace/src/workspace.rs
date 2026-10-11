use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use anyhow::Result;
use glyphst_ai_ui::{AiContextProvider as _, AiPanel, AiPanelEvent};
use glyphst_files::path_chooser::{
    OpenPathOptions, PathFilter, SavePathOptions, prompt_for_new_path, prompt_for_paths,
};
use glyphst_translation_ui::service::LanguageCode;
use glyphst_translation_ui::{
    DocumentTranslationId, DocumentTranslationJobEvent, DocumentTranslationManager,
    DocumentTranslationNotification, DocumentTranslationSessionNotification,
    PdfTranslationSessionCacheKey, PdfTranslationSessionRequest, RestoredTranslationSubmission,
    document_translation_notification, document_translation_session_notification,
};
use glyphst_ui::{
    activity::ActivityCenter,
    root::render_overlays,
    style::{apply_window_background, shell_background},
};
use gpui::{
    AnyElement, App, Bounds, Context, DismissEvent, DragMoveEvent, Entity, EntityId, ExternalPaths,
    FocusHandle, Focusable, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Pixels, Point, PromptButton, PromptLevel, Render, SharedString, WeakEntity,
    Window, WindowAppearance, bounds, canvas, div, fill, point, prelude::*, px, size,
};
use gpui_component::{
    ActiveTheme as _, ThemeColor, WindowExt as _, menu::PopupMenu, notification::Notification,
};
use gpui_fps::{FpsMonitor, FpsOverlay};

mod ai;
mod chrome;
mod docks;
mod render_debug;
mod session;
mod synchronized_connections;

pub use glyphst_actions::workspace::{
    ActivateNextPane, ActivatePaneDown, ActivatePaneLeft, ActivatePaneRight, ActivatePaneUp,
    ActivatePreviousPane, CloseOtherPanes, MovePaneDown, MovePaneLeft, MovePaneRight, MovePaneUp,
    ResetPaneSizes, ResizePaneDown, ResizePaneLeft, ResizePaneRight, ResizePaneUp, SwapPaneDown,
    SwapPaneLeft, SwapPaneRight, SwapPaneUp, ToggleMode, ToggleRenderingDebug, ToggleThemeSelector,
    ToggleZenMode,
};

use crate::{
    command_palette::{CommandPalette, CommandPaletteEvent},
    document_view::{
        CloseTab, DocumentSidebar, DocumentView, DocumentViewEvent, DuplicateTab, OpenFile,
        OpenKeybindings, OpenNewTab, OpenQuickAsk, OpenSettings, ResetZoom, ToggleAiPanel,
        ToggleBookmarkSidebar, ToggleCommandPalette, ToggleHome, ToggleNotes,
        ToggleThumbnailSidebar, ToggleToc, ToggleTransparency, TranslationViewportAnchor, ZoomIn,
        ZoomOut,
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
use render_debug::RenderDebugCapture;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WorkspaceSurfaceMode {
    Home,
    #[default]
    Panes,
}

fn suggested_open_directory(active_document: Option<&Path>, cx: &App) -> PathBuf {
    active_document
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .or_else(|| {
            cx.global::<crate::library::LibraryStore>()
                .history_entries()
                .first()
                .and_then(|entry| entry.path.parent())
                .map(Path::to_path_buf)
        })
        .or_else(dirs::document_dir)
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn open_document_options(initial_directory: PathBuf) -> OpenPathOptions {
    OpenPathOptions::files(initial_directory, crate::fl!("open-pdf"))
        .multiple(true)
        .filter(PathFilter::new(
            crate::fl!("external-drop-supported-hint"),
            ["pdf", "epub"],
        ))
        .allow_all_files(true)
}

struct WorkspaceTranslationState {
    manager: Entity<DocumentTranslationManager>,
    pending_splits: Vec<PendingTranslationSplit>,
    live_pairs: HashMap<DocumentTranslationId, LiveTranslationPair>,
}

#[derive(Default)]
struct WorkspaceRestoreState {
    tab_ids: HashMap<u64, (EntityId, EntityId)>,
    pending_documents: usize,
    dock_document_tab: Option<u64>,
    synchronized_groups: Vec<ReaderSynchronizedGroup>,
    translation_sessions: Vec<ReaderTranslationSession>,
    missing_documents: usize,
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
    translations: WorkspaceTranslationState,
    command_palette: Option<Entity<CommandPalette>>,
    command_palette_previous_focus_handle: Option<FocusHandle>,
    theme_selector: Option<Entity<ThemeSelector>>,
    theme_selector_previous_focus_handle: Option<FocusHandle>,
    title_bar_menu_expanded: bool,
    title_bar_open_menu: Option<WorkspaceMenu>,
    title_bar_popup_menu: Option<Entity<PopupMenu>>,
    dragged_tab_outside_window: Option<DraggedTab>,
    external_document_drop_feedback: Option<ExternalDocumentDropFeedback>,
    synchronized_tabs: SynchronizedTabRegistry,
    restore: WorkspaceRestoreState,
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
    rendering_debug: Option<Entity<FpsMonitor>>,
    rendering_debug_capture: Option<RenderDebugCapture>,
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
    job_id: DocumentTranslationId,
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
        let document_translation_jobs = cx.new(DocumentTranslationManager::new);
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
        let document_translation_jobs = cx.new(DocumentTranslationManager::new);
        let jobs = document_translation_jobs.clone();
        let pane = cx.new(|cx| Pane::empty(cx.focus_handle(), jobs));
        Self::with_pane(pane, document_translation_jobs, callbacks, window, cx)
    }

    fn with_pane(
        pane: Entity<Pane>,
        document_translation_jobs: Entity<DocumentTranslationManager>,
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
            translations: WorkspaceTranslationState {
                manager: document_translation_jobs.clone(),
                pending_splits: Vec::new(),
                live_pairs: HashMap::new(),
            },
            command_palette: None,
            command_palette_previous_focus_handle: None,
            theme_selector: None,
            theme_selector_previous_focus_handle: None,
            title_bar_menu_expanded: false,
            title_bar_open_menu: None,
            title_bar_popup_menu: None,
            dragged_tab_outside_window: None,
            external_document_drop_feedback: None,
            synchronized_tabs: SynchronizedTabRegistry::default(),
            restore: WorkspaceRestoreState::default(),
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
            rendering_debug: None,
            rendering_debug_capture: None,
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
                                this.translations.manager.update(cx, |jobs, cx| {
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
                                this.translations.manager.update(cx, |jobs, cx| {
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
                                    this.translations.manager.downgrade(),
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
                                this.translations.manager.update(cx, |manager, cx| {
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
                                    this.translations.manager.downgrade(),
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
                    eprintln!("failed to open document: {error:#}");
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

    /// Install host-specific work that must run before the custom Linux title
    /// bar removes this workspace window.
    pub fn set_window_close_handler(
        &mut self,
        handler: fn(&Entity<Workspace>, &mut Window, &mut App),
    ) {
        self.window_close_handler = Some(handler);
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
                        this.translations.manager.update(cx, |jobs, cx| {
                            jobs.finish_open(*job_id, result.clone(), cx);
                        });
                    }
                }
                PaneEvent::RestoredDocumentFinished {
                    session_tab_id,
                    document_tab_id,
                } => {
                    if let Some(document_tab_id) = document_tab_id {
                        this.restore.tab_ids.insert(
                            *session_tab_id,
                            (*document_tab_id, pane_for_events.entity_id()),
                        );
                    } else {
                        this.restore.tab_ids.remove(session_tab_id);
                    }
                    this.restore.pending_documents =
                        this.restore.pending_documents.saturating_sub(1);
                    if this.restore.pending_documents == 0 {
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

    fn create_empty_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<Pane> {
        let document_translation_jobs = self.translations.manager.clone();
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
        self.close_command_palette(window, cx);
        self.close_theme_selector(window, cx);
        let active_path = self
            .active_pane
            .read(cx)
            .active_document_view()
            .map(|view| view.read(cx).path().to_path_buf());
        let initial_dir = suggested_open_directory(active_path.as_deref(), cx);
        let dialog = prompt_for_paths(window, cx, open_document_options(initial_dir));

        cx.spawn(async move |this, cx| match dialog.await {
            Ok(Ok(Some(paths))) => {
                if paths.is_empty() {
                    return;
                }
                match this.update_in(cx, |this, window, cx| {
                    this.open_external_document_paths(&paths, window, cx);
                }) {
                    Ok(()) => {}
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
        job_id: DocumentTranslationId,
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
                            this.translations
                                .manager
                                .update(cx, |manager, cx| manager.discard_session(session_id, cx));
                        } else {
                            this.translations.manager.update(cx, |manager, cx| {
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
            self.translations
                .manager
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
        self.translations.pending_splits.retain(|pending| {
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
            self.translations.manager.update(cx, |jobs, cx| {
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
        session_id: DocumentTranslationId,
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
            .translations
            .manager
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
        session_id: DocumentTranslationId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(snapshot) = self
            .translations
            .manager
            .read(cx)
            .session_snapshot(session_id)
        else {
            return;
        };
        if !snapshot.can_save() {
            return;
        }
        if snapshot.failures.is_empty() {
            self.choose_translation_export_path(session_id, window, cx);
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
            _ = this.update_in(cx, |this, window, cx| {
                this.choose_translation_export_path(session_id, window, cx);
            });
        })
        .detach();
    }

    fn choose_translation_export_path(
        &mut self,
        session_id: DocumentTranslationId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(snapshot) = self
            .translations
            .manager
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
        let dialog = prompt_for_new_path(
            window,
            cx,
            SavePathOptions::new(
                directory,
                crate::fl!("translation-save-pdf"),
                suggested_name,
            )
            .required_extension("pdf"),
        );
        let manager = self.translations.manager.clone();
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
        job_id: DocumentTranslationId,
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
        self.translations
            .pending_splits
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
            self.translations
                .pending_splits
                .retain(|pending| pending.job_id != job_id);
            return Err(error);
        }
        Ok(())
    }

    fn start_translated_split_open(
        &mut self,
        job_id: DocumentTranslationId,
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
        self.translations
            .pending_splits
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
            self.translations
                .pending_splits
                .retain(|pending| pending.job_id != job_id);
            self.remove_pane(&translated_pane, window, cx);
            return Err(error);
        }
        cx.notify();
        Ok(())
    }

    fn continue_translation_split_open(
        &mut self,
        job_id: DocumentTranslationId,
        result: std::result::Result<(), SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(index) = self
            .translations
            .pending_splits
            .iter()
            .position(|pending| pending.job_id == job_id)
        else {
            return false;
        };
        let pending = self.translations.pending_splits.remove(index);
        if let Err(error) = result {
            if let PendingTranslationSplitStage::OpeningTranslated {
                translated_pane, ..
            } = pending.stage
            {
                self.remove_pane(&translated_pane, window, cx);
            }
            self.translations.manager.update(cx, |jobs, cx| {
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
                    self.translations.manager.update(cx, |jobs, cx| {
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
                    self.translations.manager.update(cx, |jobs, cx| {
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
                self.translations.manager.update(cx, |jobs, cx| {
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
        session_id: DocumentTranslationId,
        original: Entity<DocumentView>,
        translated: Entity<DocumentView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let source_id = original.entity_id();
        let translated_id = translated.entity_id();
        let conflicts = self
            .translations
            .live_pairs
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
        if self.translations.live_pairs.contains_key(&session_id) {
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

        self.translations.live_pairs.insert(
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
        session_id: DocumentTranslationId,
        cx: &mut Context<Self>,
    ) {
        if let Some(pair) = self.translations.live_pairs.remove(&session_id) {
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
            .translations
            .live_pairs
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
        if self.translations.live_pairs.is_empty() {
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
            .translations
            .live_pairs
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
        for (session_id, pair) in &mut self.translations.live_pairs {
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
        sessions: &[(DocumentTranslationId, bool)],
        keep_running_in_background: bool,
        cx: &mut Context<Self>,
    ) {
        let mut seen = HashSet::new();
        for (session_id, running) in sessions.iter().copied() {
            if !seen.insert(session_id) {
                continue;
            }
            self.translations.manager.update(cx, |manager, cx| {
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
        let palette_previous_focus_handle = previous_focus_handle.clone();
        let palette =
            cx.new(|cx| CommandPalette::new(actions, palette_previous_focus_handle, window, cx));

        cx.subscribe_in(&palette, window, |this, _, event, window, cx| match event {
            CommandPaletteEvent::Dismissed => this.close_command_palette(window, cx),
            CommandPaletteEvent::Confirmed(action) => {
                this.confirm_command_palette_action(action.as_ref(), window, cx);
            }
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

    fn confirm_command_palette_action(
        &mut self,
        action: &dyn gpui::Action,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.command_palette.take().is_none() {
            return;
        }

        let previous_focus_handle = self
            .command_palette_previous_focus_handle
            .take()
            .unwrap_or_else(|| self.active_pane.read(cx).focus_handle());
        let action = action.boxed_clone();
        cx.notify();
        window.defer(cx, move |window, cx| {
            previous_focus_handle.focus(window, cx);
            previous_focus_handle.dispatch_action(action.as_ref(), window, cx);
        });
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
        let rendering_debug = self
            .rendering_debug
            .clone()
            .map(|monitor| FpsOverlay::new(&monitor));
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
            .on_action(cx.listener(|this, _: &ToggleBookmarkSidebar, window, cx| {
                this.toggle_left_sidebar(DocumentSidebar::Bookmarks, window, cx);
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
            .on_action(cx.listener(|this, _: &ToggleRenderingDebug, window, cx| {
                this.toggle_rendering_debug(window, cx);
            }))
            .when_some(title_bar, |this, title_bar| this.child(title_bar))
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .overflow_hidden()
                    .child(body)
                    .children(rendering_debug),
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
            .children(root_overlays)
    }
}

#[cfg(test)]
mod tests;
