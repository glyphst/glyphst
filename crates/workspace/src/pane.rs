use std::{
    cell::RefCell,
    cmp::Ordering,
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    rc::Rc,
    time::{Duration, Instant},
};

#[cfg(all(debug_assertions, not(test)))]
use std::sync::OnceLock;

use anyhow::{Result, bail};
use glyphst_core::{DocumentColors, DocumentFormat};
use glyphst_pdf::{DocumentAuthenticationError, DocumentPassword};
use glyphst_translation_ui::{DocumentTranslationId, DocumentTranslationManager};
use gpui::{
    Anchor, Animation, AnimationExt, AnyElement, App, AppContext, Axis, Bounds, Context,
    DismissEvent, DragMoveEvent, Entity, EntityId, EventEmitter, ExternalPaths, FocusHandle,
    Focusable, IntoElement, MouseButton, MouseDownEvent, ParentElement, Pixels, Point, Render,
    SharedString, Styled, Task, Window, WindowHandle, anchored, deferred, div, point, prelude::*,
    px, relative,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, ElementExt as _, Root, Selectable as _, Sizable as _,
    ThemeColor as Theme, WindowExt as _,
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
    h_flex,
    input::{Input, InputContentType, InputEvent, InputState},
    menu::{PopupMenu, PopupMenuItem},
    notification::Notification,
    resizable::{ResizablePanelGroup, ResizableState, resizable_panel},
    tab::{Tab as ComponentTab, TabBar as ComponentTabBar},
    v_flex,
};

pub use glyphst_actions::pane::{
    SplitDown, SplitHorizontal, SplitLeft, SplitRight, SplitUp, SplitVertical,
};

use crate::{
    KeymapEditor,
    document_view::{
        DocumentAnnotations, DocumentOpenOptions, DocumentView, DocumentViewEvent,
        PreparedDocumentOpen, SynchronizedScrollDelta,
    },
    external_drop::{
        ExternalDocumentDropFeedback, classify_external_document_paths,
        render_external_document_drop_overlay_for_pane,
    },
    icons::{Icon, IconName},
    library::{
        DocumentPosition, LibraryStore, ReaderNewTabSession, ReaderPaneSession, ReaderPaneTree,
        ReaderSplitAxis, ReaderTabKind, ReaderTabSession, normalize_path,
    },
    new_tab::{NewTab, NewTabEvent},
    pdf_password_store::{PdfPasswordStore, StoredPdfPassword},
    settings::{SettingsStore, TabBarVisibility, ui_rems},
    synchronized_tabs::{SynchronizedGroupId, SynchronizedGroupSummary},
};

pub(crate) const PANE_TAB_BAR_HEIGHT: f32 = 32.0;
const PANE_TAB_MAX_WIDTH: f32 = 220.0;
const PANE_TAB_MIN_WIDTH: f32 = 120.0;
const PANE_DIRECTION_PROBE_DISTANCE: f32 = glyphst_ui::style::SURFACE_GAP + 1.0;
const HORIZONTAL_MIN_SIZE: f32 = 120.0;
const VERTICAL_MIN_SIZE: f32 = 140.0;
const DROP_TARGET_SIZE: f32 = 0.25;
const RESIZE_STEP: f32 = 80.0;
const LOADING_GRID_SQUARE_SIZE: f32 = 18.0;
const LOADING_GRID_GAP: f32 = 4.0;
const LOADING_GRID_PHASES: [f32; 9] = [0.80, 0.05, 0.50, 0.30, 0.95, 0.15, 0.65, 0.40, 0.70];
const LOADING_GRID_DURATION: Duration = Duration::from_millis(1200);

#[cfg(all(debug_assertions, not(test)))]
const DEBUG_FILE_OPEN_MIN_MS_ENV: &str = "GLYPHST_DEBUG_FILE_OPEN_MIN_MS";

fn loading_grid_opacity(delta: f32, phase: f32) -> f32 {
    let t = (delta + phase) % 1.0;
    let wave = (1.0 - (std::f32::consts::TAU * t).cos()) / 2.0;
    0.2 + 0.8 * wave
}

#[cfg(any(debug_assertions, test))]
fn parse_debug_file_open_min_duration(value: &str) -> Option<Duration> {
    value
        .trim()
        .parse::<u64>()
        .ok()
        .filter(|milliseconds| *milliseconds > 0)
        .map(Duration::from_millis)
}

#[cfg(all(debug_assertions, not(test)))]
fn debug_file_open_min_duration() -> Option<Duration> {
    static DURATION: OnceLock<Option<Duration>> = OnceLock::new();

    *DURATION.get_or_init(|| match std::env::var(DEBUG_FILE_OPEN_MIN_MS_ENV) {
        Ok(value) if value.trim().is_empty() || value.trim() == "0" => None,
        Ok(value) => match parse_debug_file_open_min_duration(&value) {
            Some(duration) => Some(duration),
            None => {
                eprintln!(
                    "ignoring invalid {DEBUG_FILE_OPEN_MIN_MS_ENV} value {value:?}; expected positive milliseconds"
                );
                None
            }
        },
        Err(std::env::VarError::NotPresent) => None,
        Err(std::env::VarError::NotUnicode(_)) => {
            eprintln!(
                "ignoring non-Unicode {DEBUG_FILE_OPEN_MIN_MS_ENV} value; expected positive milliseconds"
            );
            None
        }
    })
}

#[cfg(any(not(debug_assertions), test))]
fn debug_file_open_min_duration() -> Option<Duration> {
    None
}

fn remaining_file_open_min_duration(
    minimum: Option<Duration>,
    elapsed: Duration,
) -> Option<Duration> {
    minimum
        .and_then(|minimum| minimum.checked_sub(elapsed))
        .filter(|remaining| !remaining.is_zero())
}

#[derive(Clone)]
#[allow(clippy::enum_variant_names)]
pub(crate) enum Tab {
    NewTab(Entity<NewTab>),
    LoadingDocument(Entity<LoadingDocumentView>),
    Document(Entity<DocumentView>),
    Keymap(Entity<KeymapEditor>),
}

pub(crate) struct LoadingDocumentView {
    path: PathBuf,
    initial_position: DocumentPosition,
    restore_tab_id: Option<u64>,
    focus_handle: FocusHandle,
    job_ids: Vec<DocumentTranslationId>,
    password_input: Entity<InputState>,
    remember_password: bool,
    status: LoadingDocumentStatus,
    pending_open: Option<PendingDocumentOpen>,
    _task: Option<Task<()>>,
}

struct PendingDocumentOpen {
    behavior: glyphst_settings::BehaviorSettings,
    options: DocumentOpenOptions,
    epub_preferences: glyphst_core::EpubReaderPreferences,
    page_gap: f32,
    document_colors: Option<DocumentColors>,
}

#[derive(Clone)]
enum LoadingDocumentStatus {
    Loading,
    CheckingSavedPassword,
    Unlocking,
    PasswordRequired {
        error: Option<SharedString>,
        keychain_warning: Option<SharedString>,
    },
    Error(SharedString),
}

enum LoadingDocumentEvent {
    SubmitPassword {
        password: DocumentPassword,
        remember: bool,
    },
    Cancel,
}

impl EventEmitter<LoadingDocumentEvent> for LoadingDocumentView {}

#[derive(Clone)]
enum DocumentOpenAttempt {
    Initial,
    Saved(DocumentPassword),
    Shared(DocumentPassword),
    Entered {
        password: DocumentPassword,
        remember: bool,
    },
}

impl DocumentOpenAttempt {
    fn password(&self) -> Option<DocumentPassword> {
        match self {
            Self::Initial => None,
            Self::Saved(password) | Self::Shared(password) | Self::Entered { password, .. } => {
                Some(password.clone())
            }
        }
    }
}

enum DocumentOpenFailure {
    Authentication,
    Other(String),
}

fn classify_document_open_error(error: anyhow::Error) -> DocumentOpenFailure {
    if error
        .downcast_ref::<DocumentAuthenticationError>()
        .is_some()
    {
        DocumentOpenFailure::Authentication
    } else {
        DocumentOpenFailure::Other(format!("{error:#}"))
    }
}

struct PdfPasswordSaveNotification;
struct PdfPasswordForgetNotification;

impl LoadingDocumentView {
    #[allow(clippy::too_many_arguments)]
    fn new(
        path: PathBuf,
        initial_position: DocumentPosition,
        restore_tab_id: Option<u64>,
        focus_handle: FocusHandle,
        job_ids: Vec<DocumentTranslationId>,
        pending_open: PendingDocumentOpen,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let password_input = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder(crate::fl!("pdf-password-placeholder"))
        });
        cx.subscribe_in(
            &password_input,
            window,
            |this, _, event, window, cx| match event {
                InputEvent::PressEnter { .. } => this.submit_password(window, cx),
                InputEvent::Change => cx.notify(),
                InputEvent::CompletionAccepted { .. }
                | InputEvent::SpanHoverChanged { .. }
                | InputEvent::Focus
                | InputEvent::Blur => {}
            },
        )
        .detach();

        Self {
            path,
            initial_position,
            restore_tab_id,
            focus_handle,
            job_ids,
            password_input,
            remember_password: true,
            status: LoadingDocumentStatus::Loading,
            pending_open: Some(pending_open),
            _task: None,
        }
    }

    fn submit_password(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(self.status, LoadingDocumentStatus::PasswordRequired { .. }) {
            return;
        }
        let password = self.password_input.read(cx).value().to_string();
        if password.is_empty() {
            return;
        }
        self.password_input
            .update(cx, |input, cx| input.set_value("", window, cx));
        cx.emit(LoadingDocumentEvent::SubmitPassword {
            password: DocumentPassword::new(password),
            remember: self.remember_password,
        });
    }

    fn render_progress(&self, label: SharedString, cx: &App) -> AnyElement {
        let theme = cx.theme().colors;
        v_flex()
            .items_center()
            .gap_2()
            .text_align(gpui::TextAlign::Center)
            .debug_selector(|| "document-open-loading".to_string())
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(px(LOADING_GRID_GAP))
                    .w(px(LOADING_GRID_SQUARE_SIZE * 3.0 + LOADING_GRID_GAP * 3.0))
                    .items_center()
                    .justify_center()
                    .debug_selector(|| "document-open-loading-grid".to_string())
                    .children((0..9).map(|index| {
                        let phase = LOADING_GRID_PHASES[index];
                        div()
                            .w(px(LOADING_GRID_SQUARE_SIZE))
                            .h(px(LOADING_GRID_SQUARE_SIZE))
                            .bg(theme.primary)
                            .with_animation(
                                ("document-open-loading-square", index),
                                Animation::new(LOADING_GRID_DURATION).repeat(),
                                move |element, delta| {
                                    element.opacity(loading_grid_opacity(delta, phase))
                                },
                            )
                    })),
            )
            .child(
                div()
                    .text_size(ui_rems(14.0))
                    .text_color(theme.muted_foreground)
                    .child(label),
            )
            .into_any_element()
    }
}

impl Render for LoadingDocumentView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().colors;
        let file_name = self
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("document");
        let content = match &self.status {
            LoadingDocumentStatus::Error(error) => div()
                .flex()
                .items_center()
                .gap_2()
                .text_color(theme.danger)
                .debug_selector(|| "document-open-loading-error".to_string())
                .child(
                    Icon::new(IconName::TriangleAlert)
                        .size(ui_rems(14.0))
                        .text_color(theme.danger),
                )
                .child(error.clone())
                .into_any_element(),
            LoadingDocumentStatus::Loading => {
                self.render_progress(crate::fl!("pdf-loading", name = file_name).into(), cx)
            }
            LoadingDocumentStatus::CheckingSavedPassword => self.render_progress(
                crate::fl!("pdf-password-checking", name = file_name).into(),
                cx,
            ),
            LoadingDocumentStatus::Unlocking => self.render_progress(
                crate::fl!("pdf-password-unlocking", name = file_name).into(),
                cx,
            ),
            LoadingDocumentStatus::PasswordRequired {
                error,
                keychain_warning,
            } => {
                let can_submit = !self.password_input.read(cx).value().is_empty();
                let loading = cx.entity();
                let checkbox_loading = loading.clone();
                v_flex()
                    .w(px(420.0))
                    .max_w_full()
                    .gap_3()
                    .p_4()
                    .rounded(cx.theme().radius)
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.background)
                    .debug_selector(|| "document-password-prompt".to_string())
                    .child(
                        v_flex()
                            .gap_1()
                            .child(
                                div()
                                    .text_size(ui_rems(16.0))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child(crate::fl!("pdf-password-title")),
                            )
                            .child(
                                div()
                                    .text_size(ui_rems(13.0))
                                    .text_color(theme.muted_foreground)
                                    .child(crate::fl!("pdf-password-detail", name = file_name)),
                            ),
                    )
                    .child(
                        Input::new(&self.password_input)
                            .w_full()
                            .content_type(InputContentType::Password)
                            .mask_toggle(),
                    )
                    .when_some(error.clone(), |this, error| {
                        this.child(
                            div()
                                .text_size(ui_rems(12.0))
                                .text_color(theme.danger)
                                .debug_selector(|| "document-password-error".to_string())
                                .child(error),
                        )
                    })
                    .when_some(keychain_warning.clone(), |this, warning| {
                        this.child(
                            div()
                                .text_size(ui_rems(12.0))
                                .text_color(theme.warning)
                                .debug_selector(|| "document-password-keychain-warning".to_string())
                                .child(warning),
                        )
                    })
                    .child(
                        Checkbox::new("document-password-remember")
                            .label(crate::fl!("pdf-password-remember"))
                            .checked(self.remember_password)
                            .on_click(move |checked, _, cx| {
                                checkbox_loading.update(cx, |loading, cx| {
                                    loading.remember_password = *checked;
                                    cx.notify();
                                });
                            }),
                    )
                    .child(
                        h_flex()
                            .w_full()
                            .justify_end()
                            .gap_2()
                            .child(
                                Button::new("document-password-cancel")
                                    .label(crate::fl!("cancel"))
                                    .outline()
                                    .on_click({
                                        let loading = loading.clone();
                                        move |_, _, cx| {
                                            loading.update(cx, |_, cx| {
                                                cx.emit(LoadingDocumentEvent::Cancel)
                                            });
                                        }
                                    }),
                            )
                            .child(
                                Button::new("document-password-unlock")
                                    .label(crate::fl!("pdf-password-unlock"))
                                    .primary()
                                    .disabled(!can_submit)
                                    .on_click(move |_, window, cx| {
                                        loading.update(cx, |loading, cx| {
                                            loading.submit_password(window, cx)
                                        });
                                    }),
                            ),
                    )
                    .into_any_element()
            }
        };

        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(theme.background)
            .p_4()
            .child(content)
    }
}

#[derive(Clone)]
pub(crate) struct DraggedTab {
    pub(crate) source_window: WindowHandle<Root>,
    pub(crate) source_pane: Entity<Pane>,
    pub(crate) ix: usize,
    pub(crate) tab_id: EntityId,
    pub(crate) label: SharedString,
}

pub(crate) struct TabTransfer {
    tab: Tab,
    index: usize,
    active_tab: usize,
    focus_handle: Option<FocusHandle>,
}

impl Render for DraggedTab {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        ComponentTab::new()
            .label(self.label.clone())
            .selected(true)
            .corner_radius(cx.theme().radius)
            .max_w(ui_rems(PANE_TAB_MAX_WIDTH))
            .min_w(ui_rems(PANE_TAB_MIN_WIDTH))
    }
}

#[derive(Clone)]
pub(crate) enum PaneDropTarget {
    Tab(usize),
    Append,
    Content(Option<SplitDirection>),
}

#[derive(Clone)]
pub(crate) enum PaneEvent {
    Activate,
    OpenFile,
    OpenDocument {
        path: PathBuf,
        position: Option<DocumentPosition>,
        replace_active_new_tab: bool,
    },
    ExternalPathsDropped(Vec<PathBuf>),
    DocumentOpenFinished {
        job_id: DocumentTranslationId,
        result: std::result::Result<(), SharedString>,
    },
    RestoredDocumentFinished {
        session_tab_id: u64,
        document_tab_id: Option<EntityId>,
    },
    DocumentPasswordAuthenticated {
        path: PathBuf,
        password: DocumentPassword,
    },
    OpenSettings,
    DocumentSidebarNavigation(EntityId),
    TabDrop {
        dragged_tab: Box<DraggedTab>,
        target: PaneDropTarget,
    },
    CloseTab(EntityId),
    DuplicateTab(usize),
    TabClicked {
        tab_id: EntityId,
        was_active: bool,
    },
    CreateSynchronizedGroup(EntityId),
    ConnectMoreSynchronizedTabs(EntityId),
    SetSynchronizedScrollPaused {
        tab_id: EntityId,
        paused: bool,
    },
    LeaveSynchronizedGroup(EntityId),
    FinishSynchronizedConnection,
    DocumentScrolled {
        tab_id: EntityId,
        delta: SynchronizedScrollDelta,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SynchronizedConnectionInvalidReason {
    NonDocument,
    AlreadyInGroup { group_number: u64 },
    PaneAlreadyRepresented { group_number: u64 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SynchronizedConnectionTabState {
    Source,
    Member,
    Eligible,
    Invalid(SynchronizedConnectionInvalidReason),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RecentlyAddedSynchronizedTab {
    pub(crate) tab_id: EntityId,
    pub(crate) group_id: SynchronizedGroupId,
    pub(crate) sequence: u64,
}

#[derive(Clone, Default)]
pub(crate) struct PaneSynchronizedPresentation {
    pub(crate) group_by_tab: HashMap<EntityId, SynchronizedGroupSummary>,
    pub(crate) paused_tabs: HashSet<EntityId>,
    pub(crate) pause_locked_tabs: HashSet<EntityId>,
    pub(crate) connection_tabs: HashMap<EntityId, SynchronizedConnectionTabState>,
    pub(crate) connection_color_slot: Option<usize>,
    pub(crate) recently_added: Option<RecentlyAddedSynchronizedTab>,
}

#[derive(Clone, Copy, Default)]
struct PaneTabBounds {
    body: Option<Bounds<Pixels>>,
    suffix: Option<Bounds<Pixels>>,
}

impl PaneTabBounds {
    fn visual(self) -> Option<Bounds<Pixels>> {
        match (self.body, self.suffix) {
            (Some(body), Some(suffix)) => Some(body.union(&suffix)),
            (Some(body), None) => Some(body),
            (None, Some(suffix)) => Some(suffix),
            (None, None) => None,
        }
    }
}

pub(crate) struct Pane {
    tabs: Vec<Tab>,
    active_tab: usize,
    focus_handle: FocusHandle,
    document_translation_jobs: Entity<DocumentTranslationManager>,
    tab_focus_handles: HashMap<EntityId, FocusHandle>,
    drag_split_direction: Option<SplitDirection>,
    external_document_drop_feedback: Option<ExternalDocumentDropFeedback>,
    force_show_tab_bar: bool,
    synchronized_presentation: PaneSynchronizedPresentation,
    tab_bounds: Rc<RefCell<HashMap<EntityId, PaneTabBounds>>>,
    context_menu_position: Option<Point<Pixels>>,
    context_menu_popup: Option<Entity<PopupMenu>>,
}

#[derive(Default)]
struct RestoredPaneTabs {
    tab_ids: Vec<(u64, EntityId)>,
    pending_documents: usize,
    missing_documents: usize,
}

pub(crate) struct RestoredPaneGroup {
    pub(crate) pane_group: PaneGroup,
    pub(crate) panes: Vec<(u64, Entity<Pane>)>,
    pub(crate) tab_ids: Vec<(u64, EntityId, EntityId)>,
    pub(crate) pending_documents: usize,
    pub(crate) missing_documents: usize,
}

impl Pane {
    pub(crate) fn focus_handle(&self) -> FocusHandle {
        self.active_tab_focus_handle()
            .unwrap_or_else(|| self.focus_handle.clone())
    }

    pub(crate) fn empty(
        focus_handle: FocusHandle,
        document_translation_jobs: Entity<DocumentTranslationManager>,
    ) -> Self {
        Self {
            tabs: Vec::new(),
            active_tab: 0,
            focus_handle,
            document_translation_jobs,
            tab_focus_handles: HashMap::new(),
            drag_split_direction: None,
            external_document_drop_feedback: None,
            force_show_tab_bar: false,
            synchronized_presentation: PaneSynchronizedPresentation::default(),
            tab_bounds: Rc::new(RefCell::new(HashMap::new())),
            context_menu_position: None,
            context_menu_popup: None,
        }
    }

    fn session_snapshot(
        &self,
        pane_id: u64,
        next_tab_id: &mut u64,
        runtime_tab_ids: &mut HashMap<EntityId, u64>,
        cx: &App,
    ) -> ReaderPaneSession {
        let mut tabs = Vec::with_capacity(self.tabs.len());
        let mut active_tab = None;
        for (tab_ix, tab) in self.tabs.iter().enumerate() {
            *next_tab_id = next_tab_id.saturating_add(1);
            let id = *next_tab_id;
            runtime_tab_ids.insert(Self::tab_entity_id(tab), id);
            if tab_ix == self.active_tab {
                active_tab = Some(id);
            }
            let kind = match tab {
                Tab::NewTab(new_tab) => ReaderTabKind::NewTab(new_tab.read(cx).session_state()),
                Tab::LoadingDocument(loading) => {
                    let loading = loading.read(cx);
                    ReaderTabKind::Document {
                        path: loading.path.clone(),
                        position: loading.initial_position.clone(),
                    }
                }
                Tab::Document(document_view) => {
                    let document_view = document_view.read(cx);
                    ReaderTabKind::Document {
                        path: document_view.path().to_path_buf(),
                        position: document_view.current_document_position(),
                    }
                }
                Tab::Keymap(_) => ReaderTabKind::Keymap,
            };
            tabs.push(ReaderTabSession { id, kind });
        }
        ReaderPaneSession {
            id: pane_id,
            tabs,
            active_tab,
        }
    }

    fn restore_tabs(
        &mut self,
        session: &ReaderPaneSession,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> RestoredPaneTabs {
        let mut restored = RestoredPaneTabs::default();
        for tab in &session.tabs {
            let runtime_tab = match &tab.kind {
                ReaderTabKind::NewTab(state) => {
                    Some(self.create_new_tab_with_session(state.clone(), window, cx))
                }
                ReaderTabKind::Keymap => Some(self.create_keymap_tab(window, cx)),
                ReaderTabKind::Document { path, position } => {
                    if !path.exists() || !Self::is_supported_document_path(path) {
                        restored.missing_documents += 1;
                        None
                    } else {
                        match self.append_restored_document(
                            tab.id,
                            path.clone(),
                            position.clone(),
                            window,
                            cx,
                        ) {
                            Ok(tab) => {
                                restored.pending_documents += 1;
                                Some(tab)
                            }
                            Err(error) => {
                                eprintln!(
                                    "failed to restore document {}: {error:#}",
                                    path.display()
                                );
                                restored.missing_documents += 1;
                                None
                            }
                        }
                    }
                }
            };
            let Some(runtime_tab) = runtime_tab else {
                continue;
            };
            let runtime_id = Self::tab_entity_id(&runtime_tab);
            self.tabs.push(runtime_tab);
            restored.tab_ids.push((tab.id, runtime_id));
        }

        if self.tabs.is_empty() {
            let tab = self.create_new_tab(window, cx);
            self.tabs.push(tab);
        }
        self.active_tab = session
            .active_tab
            .and_then(|active| {
                restored
                    .tab_ids
                    .iter()
                    .position(|(session_id, _)| *session_id == active)
            })
            .unwrap_or(0)
            .min(self.tabs.len().saturating_sub(1));
        self.sync_active_focus_handle();
        cx.notify();
        restored
    }

    fn append_restored_document(
        &mut self,
        session_tab_id: u64,
        path: PathBuf,
        position: DocumentPosition,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<Tab> {
        let path = normalize_path(&path);
        if !path.exists() {
            bail!("document no longer exists: {}", path.display());
        }
        if !Self::is_supported_document_path(&path) {
            bail!("unsupported file type: {}", path.display());
        }
        let (behavior, page_gap, document_colors) = {
            let store = cx.global::<SettingsStore>();
            (
                store.settings.behavior,
                store.resolved_theme.page_gap,
                store
                    .settings
                    .document_theme
                    .follow_app_theme
                    .then(|| store.resolved_theme.document_colors()),
            )
        };
        let annotations = DocumentAnnotations {
            highlights: cx.global::<LibraryStore>().highlights_for(&path),
            notes: cx.global::<LibraryStore>().notes_for(&path),
        };
        let target_language = cx
            .global::<LibraryStore>()
            .document_target_language(&path)
            .unwrap_or_else(|| {
                cx.global::<SettingsStore>()
                    .settings
                    .translation
                    .resolved_target_language()
            });
        let epub_preferences = cx
            .global::<LibraryStore>()
            .epub_reader_preferences(&path)
            .unwrap_or_else(|| cx.global::<SettingsStore>().settings.reading);
        let options = DocumentOpenOptions {
            initial_position: Some(position.clone()),
            annotations,
            target_language,
            epub_preferences,
            rotation: cx.global::<LibraryStore>().document_rotation(&path),
            page_layout: cx.global::<LibraryStore>().document_page_layout(&path),
            document_translation_jobs: self.document_translation_jobs.clone(),
        };
        let pending_open = PendingDocumentOpen {
            behavior,
            options,
            epub_preferences,
            page_gap,
            document_colors,
        };
        let loading = cx.new(|cx| {
            LoadingDocumentView::new(
                path,
                position,
                Some(session_tab_id),
                self.focus_handle.clone(),
                Vec::new(),
                pending_open,
                window,
                cx,
            )
        });
        let tab = Tab::LoadingDocument(loading.clone());
        self.register_tab_focus_handle(&tab, loading.read(cx).focus_handle.clone());
        self.subscribe_to_tab(&tab, window, cx);
        self.start_document_prepare(
            loading,
            DocumentOpenAttempt::Initial,
            debug_file_open_min_duration(),
            window,
            cx,
        );
        Ok(tab)
    }

    pub(crate) fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_handle().focus(window, cx);
    }

    fn active_tab_focus_handle(&self) -> Option<FocusHandle> {
        self.tabs
            .get(self.active_tab)
            .and_then(|tab| self.tab_focus_handles.get(&Self::tab_entity_id(tab)))
            .cloned()
    }

    fn sync_active_focus_handle(&mut self) {
        if let Some(focus_handle) = self.active_tab_focus_handle() {
            self.focus_handle = focus_handle;
        }
    }

    fn register_tab_focus_handle(&mut self, tab: &Tab, focus_handle: FocusHandle) {
        self.tab_focus_handles
            .insert(Self::tab_entity_id(tab), focus_handle);
    }

    fn remove_tab_focus_handle(&mut self, tab: &Tab) {
        self.tab_focus_handles.remove(&Self::tab_entity_id(tab));
    }

    pub(crate) fn len(&self) -> usize {
        self.tabs.len()
    }

    pub(crate) fn active_tab_ix(&self) -> usize {
        self.active_tab
    }

    pub(crate) fn active_tab_is_new_tab(&self) -> bool {
        matches!(self.tabs.get(self.active_tab), Some(Tab::NewTab(_)))
    }

    pub(crate) fn active_tab_label(&self, cx: &App) -> Option<SharedString> {
        self.tabs
            .get(self.active_tab)
            .map(|_| self.tab_label(self.active_tab, cx))
    }

    pub(crate) fn active_document_state(&self, cx: &App) -> Option<(PathBuf, DocumentPosition)> {
        match self.tabs.get(self.active_tab) {
            Some(Tab::Document(document_view)) => {
                let document_view = document_view.read(cx);
                Some((
                    document_view.path().to_path_buf(),
                    document_view.current_document_position(),
                ))
            }
            Some(Tab::NewTab(_)) | Some(Tab::LoadingDocument(_)) | Some(Tab::Keymap(_)) | None => {
                None
            }
        }
    }

    pub(crate) fn duplicate_document_tab(
        &mut self,
        tab_ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<Option<EntityId>> {
        let Some(Tab::Document(source)) = self.tabs.get(tab_ix) else {
            return Ok(None);
        };
        let (path, position, prepared) = {
            let source = source.read(cx);
            (
                source.path().to_path_buf(),
                source.current_document_position(),
                source.prepared_duplicate(),
            )
        };
        let behavior = cx.global::<SettingsStore>().settings.behavior;
        let annotations = DocumentAnnotations {
            highlights: cx.global::<LibraryStore>().highlights_for(&path),
            notes: cx.global::<LibraryStore>().notes_for(&path),
        };
        let target_language = cx
            .global::<LibraryStore>()
            .document_target_language(&path)
            .unwrap_or_else(|| {
                cx.global::<SettingsStore>()
                    .settings
                    .translation
                    .resolved_target_language()
            });
        let epub_preferences = cx
            .global::<LibraryStore>()
            .epub_reader_preferences(&path)
            .unwrap_or_else(|| cx.global::<SettingsStore>().settings.reading);
        let options = DocumentOpenOptions {
            initial_position: Some(position.clone()),
            annotations,
            target_language,
            epub_preferences,
            rotation: cx.global::<LibraryStore>().document_rotation(&path),
            page_layout: cx.global::<LibraryStore>().document_page_layout(&path),
            document_translation_jobs: self.document_translation_jobs.clone(),
        };
        let mut duplicate =
            DocumentView::from_prepared(prepared, self.focus_handle.clone(), behavior, options)?;
        let duplicate = cx.new(|cx| {
            duplicate.register_cache_eviction(cx);
            duplicate.initialize_runtime(window, cx);
            duplicate
        });
        self.subscribe_to_document_view(&duplicate, window, cx);

        self.flush_active_tab(cx);
        let insert_ix = (tab_ix + 1).min(self.tabs.len());
        let tab = Tab::Document(duplicate);
        let tab_id = Self::tab_entity_id(&tab);
        self.register_tab_focus_handle(&tab, self.focus_handle.clone());
        self.tabs.insert(insert_ix, tab);
        self.active_tab = insert_ix;
        self.sync_active_focus_handle();
        self.focus(window, cx);
        cx.global_mut::<LibraryStore>()
            .record_history(&path, position);
        cx.notify();
        Ok(Some(tab_id))
    }

    pub(crate) fn open_translation_duplicate(
        &mut self,
        source: &Entity<DocumentView>,
        session_id: DocumentTranslationId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<Entity<DocumentView>> {
        let (path, position, prepared) = {
            let source = source.read(cx);
            (
                source.path().to_path_buf(),
                source.current_document_position(),
                source.prepared_duplicate(),
            )
        };
        let behavior = cx.global::<SettingsStore>().settings.behavior;
        let annotations = DocumentAnnotations {
            highlights: cx.global::<LibraryStore>().highlights_for(&path),
            notes: cx.global::<LibraryStore>().notes_for(&path),
        };
        let target_language = self
            .document_translation_jobs
            .read(cx)
            .session_snapshot(session_id)
            .map(|snapshot| snapshot.request.target_language.as_str().to_string())
            .unwrap_or_else(|| {
                cx.global::<SettingsStore>()
                    .settings
                    .translation
                    .resolved_target_language()
            });
        let epub_preferences = cx
            .global::<LibraryStore>()
            .epub_reader_preferences(&path)
            .unwrap_or_else(|| cx.global::<SettingsStore>().settings.reading);
        let options = DocumentOpenOptions {
            initial_position: Some(position),
            annotations,
            target_language,
            epub_preferences,
            rotation: cx.global::<LibraryStore>().document_rotation(&path),
            page_layout: cx.global::<LibraryStore>().document_page_layout(&path),
            document_translation_jobs: self.document_translation_jobs.clone(),
        };
        let duplicate =
            DocumentView::from_prepared(prepared, self.focus_handle.clone(), behavior, options)?;
        let duplicate = cx.new(|cx| {
            let mut duplicate = duplicate;
            duplicate.register_cache_eviction(cx);
            duplicate.initialize_runtime(window, cx);
            duplicate
        });
        duplicate.update(cx, |duplicate, cx| {
            duplicate.attach_translation_session(session_id, cx);
        });
        self.subscribe_to_document_view(&duplicate, window, cx);

        self.flush_active_tab(cx);
        let tab = Tab::Document(duplicate.clone());
        self.register_tab_focus_handle(&tab, self.focus_handle.clone());
        self.tabs.push(tab);
        self.active_tab = self.tabs.len().saturating_sub(1);
        self.sync_active_focus_handle();
        self.focus(window, cx);
        cx.notify();
        Ok(duplicate)
    }

    pub(crate) fn active_document_view(&self) -> Option<Entity<DocumentView>> {
        match self.tabs.get(self.active_tab) {
            Some(Tab::Document(document_view)) => Some(document_view.clone()),
            Some(Tab::NewTab(_)) | Some(Tab::LoadingDocument(_)) | Some(Tab::Keymap(_)) | None => {
                None
            }
        }
    }

    pub(crate) fn active_tab_supports_zen_mode(&self) -> bool {
        matches!(
            self.tabs.get(self.active_tab),
            Some(Tab::Document(_) | Tab::LoadingDocument(_))
        )
    }

    pub(crate) fn active_zen_mode_content(&self) -> Option<AnyElement> {
        match self.tabs.get(self.active_tab) {
            Some(Tab::Document(document_view)) => Some(document_view.clone().into_any_element()),
            Some(Tab::LoadingDocument(loading)) => Some(loading.clone().into_any_element()),
            Some(Tab::NewTab(_)) | Some(Tab::Keymap(_)) | None => None,
        }
    }

    pub(crate) fn document_view_at(&self, tab_ix: usize) -> Option<Entity<DocumentView>> {
        match self.tabs.get(tab_ix) {
            Some(Tab::Document(document_view)) => Some(document_view.clone()),
            _ => None,
        }
    }

    pub(crate) fn translation_sessions(&self, cx: &App) -> Vec<(DocumentTranslationId, bool)> {
        self.tabs
            .iter()
            .filter_map(|tab| match tab {
                Tab::Document(document_view) => {
                    let document_view = document_view.read(cx);
                    document_view
                        .translation_session_id()
                        .map(|id| (id, document_view.translation_session_is_running()))
                }
                _ => None,
            })
            .collect()
    }

    pub(crate) fn active_tab_is_loading(&self) -> bool {
        matches!(
            self.tabs.get(self.active_tab),
            Some(Tab::LoadingDocument(_))
        )
    }

    pub(crate) fn unlock_pending_documents_with_password(
        &mut self,
        path: &Path,
        password: DocumentPassword,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let path = normalize_path(path);
        let loading_documents = self
            .tabs
            .iter()
            .filter_map(|tab| {
                let Tab::LoadingDocument(loading) = tab else {
                    return None;
                };
                let pending = loading.read(cx);
                (pending.path == path && pending.pending_open.is_some()).then(|| loading.clone())
            })
            .collect::<Vec<_>>();

        for loading in loading_documents {
            self.start_document_prepare(
                loading,
                DocumentOpenAttempt::Shared(password.clone()),
                None,
                window,
                cx,
            );
        }
    }

    #[cfg(test)]
    pub(crate) fn submit_document_password_for_test(
        &mut self,
        path: &Path,
        password: &str,
        remember: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let path = normalize_path(path);
        let loading = self.tabs.iter().find_map(|tab| {
            let Tab::LoadingDocument(loading) = tab else {
                return None;
            };
            (loading.read(cx).path == path).then(|| loading.clone())
        });
        let Some(loading) = loading else {
            return false;
        };
        loading.update(cx, |loading, cx| {
            loading.remember_password = remember;
            loading.password_input.update(cx, |input, cx| {
                input.set_value(password, window, cx);
            });
            loading.submit_password(window, cx);
        });
        true
    }

    pub(crate) fn document_views(&self) -> Vec<Entity<DocumentView>> {
        self.tabs
            .iter()
            .filter_map(|tab| match tab {
                Tab::Document(document_view) => Some(document_view.clone()),
                Tab::NewTab(_) | Tab::LoadingDocument(_) | Tab::Keymap(_) => None,
            })
            .collect()
    }

    pub(crate) fn tab_ids(&self) -> Vec<EntityId> {
        self.tabs.iter().map(Self::tab_entity_id).collect()
    }

    pub(crate) fn active_tab_id(&self) -> Option<EntityId> {
        self.tabs.get(self.active_tab).map(Self::tab_entity_id)
    }

    pub(crate) fn tab_id_at(&self, tab_ix: usize) -> Option<EntityId> {
        self.tabs.get(tab_ix).map(Self::tab_entity_id)
    }

    pub(crate) fn tab_is_document(&self, tab_id: EntityId) -> bool {
        self.tab_ix_for_entity_id(tab_id)
            .and_then(|tab_ix| self.tabs.get(tab_ix))
            .is_some_and(|tab| matches!(tab, Tab::Document(_)))
    }

    pub(crate) fn document_view_for_id(&self, tab_id: EntityId) -> Option<Entity<DocumentView>> {
        let tab_ix = self.tab_ix_for_entity_id(tab_id)?;
        match self.tabs.get(tab_ix) {
            Some(Tab::Document(document_view)) => Some(document_view.clone()),
            Some(Tab::NewTab(_)) | Some(Tab::LoadingDocument(_)) | Some(Tab::Keymap(_)) | None => {
                None
            }
        }
    }

    pub(crate) fn activate_tab_by_id(
        &mut self,
        tab_id: EntityId,
        focus: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(tab_ix) = self.tab_ix_for_entity_id(tab_id) else {
            return false;
        };
        if self.active_tab != tab_ix {
            self.flush_active_tab(cx);
            self.active_tab = tab_ix;
        }
        self.sync_active_focus_handle();
        if focus {
            self.focus(window, cx);
        }
        cx.notify();
        true
    }

    pub(crate) fn set_synchronized_presentation(
        &mut self,
        presentation: PaneSynchronizedPresentation,
        cx: &mut Context<Self>,
    ) {
        self.synchronized_presentation = presentation;
        cx.notify();
    }

    pub(crate) fn tab_bounds(&self, tab_id: EntityId) -> Option<Bounds<Pixels>> {
        self.tab_bounds
            .borrow()
            .get(&tab_id)
            .copied()
            .and_then(PaneTabBounds::visual)
    }

    #[cfg(test)]
    pub(crate) fn synchronized_connection_tab_state(
        &self,
        tab_id: EntityId,
    ) -> Option<SynchronizedConnectionTabState> {
        self.synchronized_presentation
            .connection_tabs
            .get(&tab_id)
            .copied()
    }

    #[cfg(test)]
    pub(crate) fn synchronized_pause_locked(&self, tab_id: EntityId) -> bool {
        self.synchronized_presentation
            .pause_locked_tabs
            .contains(&tab_id)
    }

    pub(crate) fn set_force_show_tab_bar(
        &mut self,
        force_show_tab_bar: bool,
        cx: &mut Context<Self>,
    ) {
        if self.force_show_tab_bar != force_show_tab_bar {
            self.force_show_tab_bar = force_show_tab_bar;
            cx.notify();
        }
    }

    pub(crate) fn document_tab_ix(&self, path: &Path, cx: &App) -> Option<usize> {
        let path = normalize_path(path);
        let matches = |tab: &Tab| match tab {
            Tab::Document(document_view) => document_view.read(cx).path() == path.as_path(),
            Tab::LoadingDocument(loading) => loading.read(cx).path == path,
            Tab::NewTab(_) | Tab::Keymap(_) => false,
        };
        if self.tabs.get(self.active_tab).is_some_and(matches) {
            return Some(self.active_tab);
        }
        self.tabs.iter().position(matches)
    }

    fn document_tab_occurrence(&self, tab_ix: usize, cx: &App) -> Option<(usize, usize)> {
        let Tab::Document(document_view) = self.tabs.get(tab_ix)? else {
            return None;
        };
        let path = document_view.read(cx).path().to_path_buf();
        let matching = self
            .tabs
            .iter()
            .enumerate()
            .filter_map(|(ix, tab)| match tab {
                Tab::Document(candidate) if candidate.read(cx).path() == path => Some(ix),
                Tab::NewTab(_) | Tab::LoadingDocument(_) | Tab::Document(_) | Tab::Keymap(_) => {
                    None
                }
            })
            .collect::<Vec<_>>();
        if matching.len() < 2 {
            return None;
        }
        let ordinal = matching.iter().position(|ix| *ix == tab_ix)? + 1;
        Some((ordinal, matching.len()))
    }

    pub(crate) fn activate_existing_document(
        &mut self,
        tab_ix: usize,
        position: Option<DocumentPosition>,
        replace_active_new_tab: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let remove_tab_ix =
            (replace_active_new_tab && self.active_tab_is_new_tab() && self.tabs.len() > 1)
                .then_some(self.active_tab);

        if self.active_tab != tab_ix {
            self.flush_active_tab(cx);
        }
        self.active_tab = tab_ix;

        if let Some(remove_tab_ix) = remove_tab_ix {
            let removed_tab = self.tabs.remove(remove_tab_ix);
            self.remove_tab_focus_handle(&removed_tab);
            if remove_tab_ix < self.active_tab {
                self.active_tab -= 1;
            }
        }

        if let Some(position) = position
            && let Some(Tab::Document(document_view)) = self.tabs.get(self.active_tab)
        {
            document_view.update(cx, |document_view, cx| {
                document_view.navigate_to_document_position(position, cx);
            });
        }

        self.sync_active_focus_handle();
        self.focus(window, cx);
        cx.notify();
    }

    pub(crate) fn associate_open_job(
        &mut self,
        tab_ix: usize,
        job_id: DocumentTranslationId,
        cx: &mut Context<Self>,
    ) {
        match self.tabs.get(tab_ix) {
            Some(Tab::Document(_)) => cx.emit(PaneEvent::DocumentOpenFinished {
                job_id,
                result: Ok(()),
            }),
            Some(Tab::LoadingDocument(loading)) => {
                loading.update(cx, |loading, _| {
                    if !loading.job_ids.contains(&job_id) {
                        loading.job_ids.push(job_id);
                    }
                });
            }
            Some(Tab::NewTab(_)) | Some(Tab::Keymap(_)) | None => {}
        }
    }

    pub(crate) fn focus_or_open_document(
        &mut self,
        path: PathBuf,
        position: Option<DocumentPosition>,
        replace_active_new_tab: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        self.focus_or_open_document_inner(path, position, replace_active_new_tab, None, window, cx)
    }

    pub(crate) fn focus_or_open_document_for_job(
        &mut self,
        path: PathBuf,
        position: Option<DocumentPosition>,
        replace_active_new_tab: bool,
        job_id: DocumentTranslationId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        self.focus_or_open_document_inner(
            path,
            position,
            replace_active_new_tab,
            Some(job_id),
            window,
            cx,
        )
    }

    fn focus_or_open_document_inner(
        &mut self,
        path: PathBuf,
        position: Option<DocumentPosition>,
        replace_active_new_tab: bool,
        job_id: Option<DocumentTranslationId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        let path = normalize_path(&path);
        if let Some(tab_ix) = self.document_tab_ix(&path, cx) {
            self.activate_existing_document(tab_ix, position, replace_active_new_tab, window, cx);
            if let Some(job_id) = job_id {
                self.associate_open_job(tab_ix, job_id, cx);
            }
            return Ok(());
        }

        if !path.exists() {
            bail!("document no longer exists: {}", path.display());
        }
        if !Self::is_supported_document_path(&path) {
            bail!("unsupported file type: {}", path.display());
        }
        let (behavior, page_gap, document_colors) = {
            let store = cx.global::<SettingsStore>();
            (
                store.settings.behavior,
                store.resolved_theme.page_gap,
                store
                    .settings
                    .document_theme
                    .follow_app_theme
                    .then(|| store.resolved_theme.document_colors()),
            )
        };
        let initial_position = position
            .or_else(|| cx.global::<LibraryStore>().history_position(&path))
            .unwrap_or_else(|| DocumentPosition::fixed(0.0, 0.0, behavior.default_zoom, 1));
        let annotations = DocumentAnnotations {
            highlights: cx.global::<LibraryStore>().highlights_for(&path),
            notes: cx.global::<LibraryStore>().notes_for(&path),
        };
        let target_language = cx
            .global::<LibraryStore>()
            .document_target_language(&path)
            .unwrap_or_else(|| {
                cx.global::<SettingsStore>()
                    .settings
                    .translation
                    .resolved_target_language()
            });
        let epub_preferences = cx
            .global::<LibraryStore>()
            .epub_reader_preferences(&path)
            .unwrap_or_else(|| cx.global::<SettingsStore>().settings.reading);
        let rotation = cx.global::<LibraryStore>().document_rotation(&path);
        let options = DocumentOpenOptions {
            initial_position: Some(initial_position.clone()),
            annotations,
            target_language,
            epub_preferences,
            rotation,
            page_layout: cx.global::<LibraryStore>().document_page_layout(&path),
            document_translation_jobs: self.document_translation_jobs.clone(),
        };
        let pending_open = PendingDocumentOpen {
            behavior,
            options,
            epub_preferences,
            page_gap,
            document_colors,
        };
        let loading = cx.new(|cx| {
            LoadingDocumentView::new(
                path,
                initial_position,
                None,
                self.focus_handle.clone(),
                job_id.into_iter().collect(),
                pending_open,
                window,
                cx,
            )
        });
        let tab = Tab::LoadingDocument(loading.clone());
        self.register_tab_focus_handle(&tab, loading.read(cx).focus_handle.clone());
        self.subscribe_to_tab(&tab, window, cx);
        if replace_active_new_tab && self.active_tab_is_new_tab() {
            let replaced_tab = std::mem::replace(&mut self.tabs[self.active_tab], tab);
            self.remove_tab_focus_handle(&replaced_tab);
        } else {
            self.flush_active_tab(cx);
            self.tabs.push(tab);
            self.active_tab = self.tabs.len() - 1;
        }
        self.sync_active_focus_handle();
        self.focus(window, cx);
        cx.notify();
        self.start_document_prepare(
            loading,
            DocumentOpenAttempt::Initial,
            debug_file_open_min_duration(),
            window,
            cx,
        );
        Ok(())
    }

    pub(crate) fn open_new_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.flush_active_tab(cx);
        let tab = self.create_new_tab(window, cx);
        self.tabs.push(tab);
        self.active_tab = self.tabs.len() - 1;
        self.sync_active_focus_handle();
        self.focus(window, cx);
        cx.notify();
    }

    pub(crate) fn open_keymap_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(tab_ix) = self
            .tabs
            .iter()
            .position(|tab| matches!(tab, Tab::Keymap(_)))
        {
            self.activate_tab(tab_ix, window, cx);
            if let Some(Tab::Keymap(editor)) = self.tabs.get(tab_ix) {
                editor.update(cx, |editor, cx| editor.focus_filter(window, cx));
            }
            return;
        }

        self.flush_active_tab(cx);
        let tab = self.create_keymap_tab(window, cx);
        self.tabs.push(tab);
        self.active_tab = self.tabs.len() - 1;
        self.sync_active_focus_handle();
        if let Some(Tab::Keymap(editor)) = self.tabs.get(self.active_tab) {
            editor.update(cx, |editor, cx| editor.focus_filter(window, cx));
        } else {
            self.focus_handle.focus(window, cx);
        }
        cx.notify();
    }

    pub(crate) fn close_tab_at(&mut self, tab_ix: usize, cx: &mut Context<Self>) -> bool {
        if tab_ix >= self.tabs.len() {
            return self.tabs.is_empty();
        }

        self.flush_tab(tab_ix, cx);
        let removed_tab = self.tabs.remove(tab_ix);
        self.tab_bounds
            .borrow_mut()
            .remove(&Self::tab_entity_id(&removed_tab));
        if let Tab::LoadingDocument(loading) = &removed_tab {
            let (job_ids, restore_tab_id) = {
                let loading = loading.read(cx);
                (loading.job_ids.clone(), loading.restore_tab_id)
            };
            for job_id in job_ids {
                cx.emit(PaneEvent::DocumentOpenFinished {
                    job_id,
                    result: Err("Opening the translated document was cancelled.".into()),
                });
            }
            if let Some(session_tab_id) = restore_tab_id {
                cx.emit(PaneEvent::RestoredDocumentFinished {
                    session_tab_id,
                    document_tab_id: None,
                });
            }
        }
        self.remove_tab_focus_handle(&removed_tab);

        if self.active_tab > tab_ix {
            self.active_tab -= 1;
        } else if self.active_tab >= self.tabs.len() && !self.tabs.is_empty() {
            self.active_tab = self.tabs.len() - 1;
        }

        self.sync_active_focus_handle();
        cx.notify();
        self.tabs.is_empty()
    }

    pub(crate) fn close_active_tab(&mut self, cx: &mut Context<Self>) -> bool {
        self.close_tab_at(self.active_tab, cx)
    }

    pub(crate) fn ensure_new_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.tabs.is_empty() {
            let tab = self.create_new_tab(window, cx);
            self.tabs.push(tab);
            self.active_tab = 0;
            self.sync_active_focus_handle();
            cx.notify();
        }
    }

    pub(crate) fn take_tab_for_transfer(
        &mut self,
        tab_id: EntityId,
        cx: &mut Context<Self>,
    ) -> Option<TabTransfer> {
        let tab_ix = self.tab_ix_for_entity_id(tab_id)?;
        self.flush_tab(tab_ix, cx);
        let tab = self.tabs.remove(tab_ix);
        self.tab_bounds.borrow_mut().remove(&tab_id);
        let focus_handle = self.tab_focus_handles.remove(&tab_id);
        let active_tab = self.active_tab;
        if self.active_tab > tab_ix {
            self.active_tab -= 1;
        } else if self.active_tab >= self.tabs.len() && !self.tabs.is_empty() {
            self.active_tab = self.tabs.len() - 1;
        }
        self.sync_active_focus_handle();
        cx.notify();
        Some(TabTransfer {
            tab,
            index: tab_ix,
            active_tab,
            focus_handle,
        })
    }

    pub(crate) fn restore_transferred_tab(
        &mut self,
        transfer: TabTransfer,
        cx: &mut Context<Self>,
    ) {
        let tab_ix = transfer.index.min(self.tabs.len());
        if let Some(focus_handle) = transfer.focus_handle {
            self.register_tab_focus_handle(&transfer.tab, focus_handle);
        }
        self.tabs.insert(tab_ix, transfer.tab);
        self.active_tab = transfer.active_tab.min(self.tabs.len().saturating_sub(1));
        self.sync_active_focus_handle();
        cx.notify();
    }

    pub(crate) fn insert_transferred_tab(
        &mut self,
        transfer: TabTransfer,
        to: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.flush_active_tab(cx);
        if let Tab::Document(document_view) = &transfer.tab {
            let jobs = self.document_translation_jobs.clone();
            document_view.update(cx, |document_view, cx| {
                document_view.set_document_translation_jobs(jobs, cx);
            });
        }
        self.subscribe_to_tab(&transfer.tab, window, cx);
        let tab_ix = to.min(self.tabs.len());
        if let Some(focus_handle) = transfer.focus_handle {
            self.register_tab_focus_handle(&transfer.tab, focus_handle);
        }
        self.tabs.insert(tab_ix, transfer.tab);
        self.active_tab = tab_ix;
        self.sync_active_focus_handle();
        self.focus(window, cx);
        cx.notify();
    }

    pub(crate) fn has_tab_id(&self, tab_id: EntityId) -> bool {
        self.tab_ix_for_entity_id(tab_id).is_some()
    }

    pub(crate) fn tab_ix_for_entity_id(&self, tab_id: EntityId) -> Option<usize> {
        self.tabs
            .iter()
            .position(|tab| Self::tab_entity_id(tab) == tab_id)
    }

    pub(crate) fn move_dragged_tab(
        &mut self,
        dragged_tab: &DraggedTab,
        to: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(from) = self.tab_ix_for_entity_id(dragged_tab.tab_id) else {
            return;
        };
        if from == to.min(self.tabs.len().saturating_sub(1)) {
            self.activate_tab(from, window, cx);
            return;
        }
        self.move_tab(from, to, cx);
    }

    pub(crate) fn activate_dragged_tab(
        &mut self,
        dragged_tab: &DraggedTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab_ix) = self.tab_ix_for_entity_id(dragged_tab.tab_id) else {
            return;
        };
        self.activate_tab(tab_ix, window, cx);
    }

    pub(crate) fn flush_all_tabs(&mut self, cx: &mut Context<Self>) {
        for ix in 0..self.tabs.len() {
            self.flush_tab(ix, cx);
        }
    }

    fn start_document_prepare(
        &mut self,
        loading: Entity<LoadingDocumentView>,
        attempt: DocumentOpenAttempt,
        minimum_loading_duration: Option<Duration>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (path, epub_preferences, page_gap, document_colors) = {
            let loading = loading.read(cx);
            let Some(pending) = loading.pending_open.as_ref() else {
                return;
            };
            (
                loading.path.clone(),
                pending.epub_preferences,
                pending.page_gap,
                pending.document_colors,
            )
        };
        loading.update(cx, |loading, cx| {
            loading.status = match &attempt {
                DocumentOpenAttempt::Initial => LoadingDocumentStatus::Loading,
                DocumentOpenAttempt::Saved(_)
                | DocumentOpenAttempt::Shared(_)
                | DocumentOpenAttempt::Entered { .. } => LoadingDocumentStatus::Unlocking,
            };
            cx.notify();
        });

        let loading_id = loading.entity_id();
        let loading_started_at = Instant::now();
        let password = attempt.password();
        let task = cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    DocumentView::prepare_open_with_password(
                        path,
                        epub_preferences,
                        page_gap,
                        document_colors,
                        password,
                    )
                    .map_err(classify_document_open_error)
                })
                .await;
            if let Some(remaining) = remaining_file_open_min_duration(
                minimum_loading_duration,
                loading_started_at.elapsed(),
            ) {
                cx.background_executor().timer(remaining).await;
            }
            _ = this.update_in(cx, |this, window, cx| {
                this.finish_document_open(loading_id, attempt, result, window, cx);
            });
        });
        loading.update(cx, |loading, _| loading._task = Some(task));
    }

    fn finish_document_open(
        &mut self,
        loading_id: EntityId,
        attempt: DocumentOpenAttempt,
        result: std::result::Result<PreparedDocumentOpen, DocumentOpenFailure>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab_ix) = self.tab_ix_for_entity_id(loading_id) else {
            return;
        };
        let loading = match self.tabs.get(tab_ix) {
            Some(Tab::LoadingDocument(loading)) => loading.clone(),
            _ => return,
        };

        let prepared = match result {
            Ok(prepared) => prepared,
            Err(DocumentOpenFailure::Authentication) => {
                match attempt {
                    DocumentOpenAttempt::Initial => {
                        self.start_password_lookup(loading, window, cx);
                    }
                    DocumentOpenAttempt::Saved(_) => {
                        self.discard_saved_password_and_prompt(
                            loading,
                            crate::fl!("pdf-password-saved-invalid").into(),
                            window,
                            cx,
                        );
                    }
                    DocumentOpenAttempt::Shared(_) | DocumentOpenAttempt::Entered { .. } => {
                        self.show_password_prompt(
                            loading,
                            Some(crate::fl!("pdf-password-invalid").into()),
                            None,
                            window,
                            cx,
                        );
                    }
                }
                return;
            }
            Err(DocumentOpenFailure::Other(error)) => {
                self.fail_document_open(loading, error, cx);
                return;
            }
        };

        let (authenticated_password, save_password) = match attempt {
            DocumentOpenAttempt::Saved(password) => (Some(password), None),
            DocumentOpenAttempt::Entered { password, remember } => {
                let save_password = remember.then(|| password.clone());
                (Some(password), save_password)
            }
            DocumentOpenAttempt::Initial | DocumentOpenAttempt::Shared(_) => (None, None),
        };
        let path = loading.read(cx).path.clone();
        if !self.complete_document_open(loading, prepared, window, cx) {
            return;
        }
        if let Some(password) = authenticated_password {
            cx.emit(PaneEvent::DocumentPasswordAuthenticated {
                path: path.clone(),
                password,
            });
        }
        if let Some(password) = save_password {
            self.save_document_password(path, password, window, cx);
        }
    }

    fn start_password_lookup(
        &mut self,
        loading: Entity<LoadingDocumentView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let path = loading.read(cx).path.clone();
        loading.update(cx, |loading, cx| {
            loading.status = LoadingDocumentStatus::CheckingSavedPassword;
            cx.notify();
        });
        let store = PdfPasswordStore::ensure(cx);
        let lookup = store.read(&path, cx);
        let loading_id = loading.entity_id();
        let task = cx.spawn_in(window, async move |this, cx| {
            let result = lookup.await;
            _ = this.update_in(cx, |this, window, cx| {
                let Some(Tab::LoadingDocument(loading)) = this
                    .tab_ix_for_entity_id(loading_id)
                    .and_then(|tab_ix| this.tabs.get(tab_ix))
                else {
                    return;
                };
                let loading = loading.clone();
                match result {
                    Ok(StoredPdfPassword::Found(password)) => this.start_document_prepare(
                        loading,
                        DocumentOpenAttempt::Saved(password),
                        None,
                        window,
                        cx,
                    ),
                    Ok(StoredPdfPassword::Missing) => {
                        this.show_password_prompt(loading, None, None, window, cx)
                    }
                    Ok(StoredPdfPassword::Corrupt) => this.discard_saved_password_and_prompt(
                        loading,
                        crate::fl!("pdf-password-saved-corrupt").into(),
                        window,
                        cx,
                    ),
                    Err(error) => {
                        eprintln!("failed to read a saved PDF password: {error:#}");
                        this.show_password_prompt(
                            loading,
                            None,
                            Some(crate::fl!("pdf-password-keychain-read-failed").into()),
                            window,
                            cx,
                        );
                    }
                }
            });
        });
        loading.update(cx, |loading, _| loading._task = Some(task));
    }

    fn discard_saved_password_and_prompt(
        &mut self,
        loading: Entity<LoadingDocumentView>,
        error: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let path = loading.read(cx).path.clone();
        loading.update(cx, |loading, cx| {
            loading.status = LoadingDocumentStatus::CheckingSavedPassword;
            cx.notify();
        });
        let store = PdfPasswordStore::ensure(cx);
        let deletion = store.delete(&path, cx);
        let loading_id = loading.entity_id();
        let task = cx.spawn_in(window, async move |this, cx| {
            let result = deletion.await;
            _ = this.update_in(cx, |this, window, cx| {
                let Some(Tab::LoadingDocument(loading)) = this
                    .tab_ix_for_entity_id(loading_id)
                    .and_then(|tab_ix| this.tabs.get(tab_ix))
                else {
                    return;
                };
                let loading = loading.clone();
                let warning = result.err().map(|delete_error| {
                    eprintln!("failed to remove a stale PDF password: {delete_error:#}");
                    SharedString::from(crate::fl!("pdf-password-keychain-delete-failed"))
                });
                this.show_password_prompt(loading, Some(error), warning, window, cx);
            });
        });
        loading.update(cx, |loading, _| loading._task = Some(task));
    }

    fn show_password_prompt(
        &mut self,
        loading: Entity<LoadingDocumentView>,
        error: Option<SharedString>,
        keychain_warning: Option<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab_ix) = self.tab_ix_for_entity_id(loading.entity_id()) else {
            return;
        };
        let input = loading.read(cx).password_input.clone();
        input.update(cx, |input, cx| input.set_value("", window, cx));
        loading.update(cx, |loading, cx| {
            loading.status = LoadingDocumentStatus::PasswordRequired {
                error,
                keychain_warning,
            };
            loading._task = None;
            cx.notify();
        });
        if self.active_tab == tab_ix {
            input.read(cx).focus_handle(cx).focus(window, cx);
        }
    }

    fn fail_document_open(
        &mut self,
        loading: Entity<LoadingDocumentView>,
        error: String,
        cx: &mut Context<Self>,
    ) {
        let (job_ids, restore_tab_id) = {
            let loading = loading.read(cx);
            (loading.job_ids.clone(), loading.restore_tab_id)
        };
        for job_id in job_ids.iter().copied() {
            cx.emit(PaneEvent::DocumentOpenFinished {
                job_id,
                result: Err(error.clone().into()),
            });
        }
        loading.update(cx, |loading, cx| {
            loading.status = LoadingDocumentStatus::Error(error.into());
            loading.restore_tab_id = None;
            loading.pending_open = None;
            loading._task = None;
            cx.notify();
        });
        if let Some(session_tab_id) = restore_tab_id {
            cx.emit(PaneEvent::RestoredDocumentFinished {
                session_tab_id,
                document_tab_id: None,
            });
        }
    }

    fn complete_document_open(
        &mut self,
        loading: Entity<LoadingDocumentView>,
        prepared: PreparedDocumentOpen,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(tab_ix) = self.tab_ix_for_entity_id(loading.entity_id()) else {
            return false;
        };
        let (path, initial_position, job_ids, restore_tab_id, pending) =
            loading.update(cx, |loading, _| {
                loading._task = None;
                (
                    loading.path.clone(),
                    loading.initial_position.clone(),
                    loading.job_ids.clone(),
                    loading.restore_tab_id,
                    loading.pending_open.take(),
                )
            });
        let Some(pending) = pending else {
            return false;
        };
        let mut document_view = match DocumentView::from_prepared(
            prepared,
            self.focus_handle.clone(),
            pending.behavior,
            pending.options,
        ) {
            Ok(document_view) => document_view,
            Err(error) => {
                self.fail_document_open(loading, format!("{error:#}"), cx);
                return false;
            }
        };
        let document_view = cx.new(|cx| {
            document_view.register_cache_eviction(cx);
            document_view.initialize_runtime(window, cx);
            document_view
        });
        self.subscribe_to_document_view(&document_view, window, cx);

        let document_tab_id = document_view.entity_id();
        let tab = Tab::Document(document_view);
        let replaced_tab = std::mem::replace(&mut self.tabs[tab_ix], tab.clone());
        self.tab_bounds
            .borrow_mut()
            .remove(&Self::tab_entity_id(&replaced_tab));
        self.remove_tab_focus_handle(&replaced_tab);
        self.register_tab_focus_handle(&tab, self.focus_handle.clone());
        if self.active_tab == tab_ix {
            self.sync_active_focus_handle();
            self.focus(window, cx);
        }

        let library = cx.global_mut::<LibraryStore>();
        library.record_history(&path, initial_position);
        if let Err(error) = library.mark_library_item_opened(&path) {
            eprintln!("failed to update library reading status: {error:#}");
        }
        for job_id in job_ids {
            cx.emit(PaneEvent::DocumentOpenFinished {
                job_id,
                result: Ok(()),
            });
        }
        if let Some(session_tab_id) = restore_tab_id {
            cx.emit(PaneEvent::RestoredDocumentFinished {
                session_tab_id,
                document_tab_id: Some(document_tab_id),
            });
        }
        cx.notify();
        true
    }

    fn save_document_password(
        &mut self,
        path: PathBuf,
        password: DocumentPassword,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let store = PdfPasswordStore::ensure(cx);
        let save = store.write(&path, password, cx);
        cx.spawn_in(window, async move |this, cx| {
            if let Err(error) = save.await {
                eprintln!("failed to save a PDF password: {error:#}");
                _ = this.update_in(cx, |_, window, cx| {
                    window.push_notification(
                        Notification::warning(crate::fl!("pdf-password-keychain-save-failed"))
                            .title(crate::fl!("pdf-password-keychain-save-failed-title"))
                            .id::<PdfPasswordSaveNotification>(),
                        cx,
                    );
                });
            }
        })
        .detach();
    }

    fn forget_saved_document_password(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let store = PdfPasswordStore::ensure(cx);
        let deletion = store.delete(&path, cx);
        cx.spawn_in(window, async move |this, cx| {
            let result = deletion.await;
            _ = this.update_in(cx, |_, window, cx| match result {
                Ok(()) => window.push_notification(
                    Notification::success(crate::fl!("pdf-password-forgot"))
                        .title(crate::fl!("pdf-password-forgot-title"))
                        .id::<PdfPasswordForgetNotification>(),
                    cx,
                ),
                Err(error) => {
                    eprintln!("failed to forget a saved PDF password: {error:#}");
                    window.push_notification(
                        Notification::error(crate::fl!("pdf-password-forget-failed"))
                            .title(crate::fl!("pdf-password-forget-failed-title"))
                            .id::<PdfPasswordForgetNotification>(),
                        cx,
                    );
                }
            });
        })
        .detach();
    }

    fn create_new_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Tab {
        let focus_handle = self.focus_handle.clone();
        let new_tab = cx.new(|cx| NewTab::new(focus_handle.clone(), window, cx));
        self.subscribe_to_new_tab(&new_tab, window, cx);
        let tab = Tab::NewTab(new_tab);
        self.register_tab_focus_handle(&tab, focus_handle);
        tab
    }

    fn create_new_tab_with_session(
        &mut self,
        state: ReaderNewTabSession,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Tab {
        let focus_handle = self.focus_handle.clone();
        let new_tab =
            cx.new(|cx| NewTab::new_with_session(focus_handle.clone(), state, window, cx));
        self.subscribe_to_new_tab(&new_tab, window, cx);
        let tab = Tab::NewTab(new_tab);
        self.register_tab_focus_handle(&tab, focus_handle);
        tab
    }

    fn create_keymap_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Tab {
        let focus_handle = self.focus_handle.clone();
        let tab = Tab::Keymap(cx.new(|cx| KeymapEditor::new(focus_handle.clone(), window, cx)));
        self.register_tab_focus_handle(&tab, focus_handle);
        tab
    }

    fn subscribe_to_new_tab(
        &self,
        new_tab: &Entity<NewTab>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let new_tab_id = new_tab.entity_id();
        cx.subscribe_in(
            new_tab,
            window,
            move |pane, _, event: &NewTabEvent, _, cx| {
                if !pane.has_tab_id(new_tab_id) {
                    return;
                }
                match event {
                    NewTabEvent::OpenFile => cx.emit(PaneEvent::OpenFile),
                    NewTabEvent::OpenDocument(path) => {
                        cx.emit(PaneEvent::OpenDocument {
                            path: path.clone(),
                            position: None,
                            replace_active_new_tab: true,
                        });
                    }
                }
            },
        )
        .detach();
    }

    fn subscribe_to_document_view(
        &self,
        document_view: &Entity<DocumentView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let document_view_id = document_view.entity_id();
        cx.subscribe_in(
            document_view,
            window,
            move |pane, _document_view, event: &DocumentViewEvent, _, cx| {
                if !pane.has_tab_id(document_view_id) {
                    return;
                }
                match event {
                    DocumentViewEvent::OpenDocument { path, position } => {
                        cx.emit(PaneEvent::OpenDocument {
                            path: path.clone(),
                            position: position.clone(),
                            replace_active_new_tab: false,
                        });
                    }
                    DocumentViewEvent::OpenSettings => cx.emit(PaneEvent::OpenSettings),
                    DocumentViewEvent::SidebarNavigationRequested => {
                        cx.emit(PaneEvent::DocumentSidebarNavigation(document_view_id))
                    }
                    DocumentViewEvent::SynchronizedScroll(delta) => {
                        cx.emit(PaneEvent::DocumentScrolled {
                            tab_id: document_view_id,
                            delta: *delta,
                        });
                    }
                    DocumentViewEvent::TranslationRegionHovered { .. }
                    | DocumentViewEvent::TranslationOverlayVisibilityChanged { .. }
                    | DocumentViewEvent::TranslationViewportChanged { .. } => {}
                }
            },
        )
        .detach();
        cx.observe(document_view, |_, _, cx| cx.notify()).detach();
    }

    fn subscribe_to_tab(&self, tab: &Tab, window: &mut Window, cx: &mut Context<Self>) {
        match tab {
            Tab::NewTab(new_tab) => self.subscribe_to_new_tab(new_tab, window, cx),
            Tab::LoadingDocument(loading) => {
                let loading_id = loading.entity_id();
                cx.subscribe_in(
                    loading,
                    window,
                    move |pane, loading, event: &LoadingDocumentEvent, window, cx| {
                        if !pane.has_tab_id(loading_id) {
                            return;
                        }
                        match event {
                            LoadingDocumentEvent::SubmitPassword { password, remember } => {
                                pane.start_document_prepare(
                                    loading.clone(),
                                    DocumentOpenAttempt::Entered {
                                        password: password.clone(),
                                        remember: *remember,
                                    },
                                    None,
                                    window,
                                    cx,
                                );
                            }
                            LoadingDocumentEvent::Cancel => {
                                cx.emit(PaneEvent::CloseTab(loading_id));
                            }
                        }
                    },
                )
                .detach();
                cx.observe(loading, |_, _, cx| cx.notify()).detach();
            }
            Tab::Document(document_view) => {
                self.subscribe_to_document_view(document_view, window, cx)
            }
            Tab::Keymap(_) => {}
        }
    }

    fn activate_tab(&mut self, tab_ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if tab_ix >= self.tabs.len() {
            return;
        }
        if self.active_tab != tab_ix {
            self.flush_active_tab(cx);
            self.active_tab = tab_ix;
        }
        self.sync_active_focus_handle();
        let password_input = self.tabs.get(tab_ix).and_then(|tab| match tab {
            Tab::LoadingDocument(loading)
                if matches!(
                    loading.read(cx).status,
                    LoadingDocumentStatus::PasswordRequired { .. }
                ) =>
            {
                Some(loading.read(cx).password_input.clone())
            }
            _ => None,
        });
        if let Some(password_input) = password_input {
            password_input.read(cx).focus_handle(cx).focus(window, cx);
        } else {
            self.focus(window, cx);
        }
        cx.notify();
    }

    fn move_tab(&mut self, from: usize, to: usize, cx: &mut Context<Self>) {
        if from == to || from >= self.tabs.len() || to > self.tabs.len() {
            return;
        }

        let active_tab_id = self.tabs.get(self.active_tab).map(Self::tab_entity_id);
        let tab = self.tabs.remove(from);
        let to = to.min(self.tabs.len());
        self.tabs.insert(to, tab);

        if let Some(active_tab_id) = active_tab_id
            && let Some(active_tab) = self
                .tabs
                .iter()
                .position(|tab| Self::tab_entity_id(tab) == active_tab_id)
        {
            self.active_tab = active_tab;
        }

        self.sync_active_focus_handle();
        cx.notify();
    }

    fn flush_active_tab(&mut self, cx: &mut Context<Self>) {
        self.flush_tab(self.active_tab, cx);
    }

    fn flush_tab(&mut self, tab_ix: usize, cx: &mut Context<Self>) {
        if let Some(Tab::Document(document_view)) = self.tabs.get(tab_ix) {
            document_view.update(cx, |document_view, cx| {
                document_view.flush_current_history_position(cx);
                document_view.deactivate_render_cache(cx);
            });
        }
    }

    fn is_supported_document_path(path: &Path) -> bool {
        path.extension()
            .and_then(|extension| extension.to_str())
            .and_then(DocumentFormat::from_extension)
            .is_some()
    }

    fn tab_label(&self, tab_ix: usize, cx: &App) -> SharedString {
        match self.tabs.get(tab_ix) {
            Some(Tab::Document(document_view)) => document_view.read(cx).file_name(),
            Some(Tab::LoadingDocument(loading)) => loading
                .read(cx)
                .path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("Loading…")
                .to_string()
                .into(),
            Some(Tab::Keymap(_)) => crate::fl!("keymap-title").into(),
            Some(Tab::NewTab(_)) | None => crate::fl!("new-tab-title").into(),
        }
    }

    fn tab_is_draggable(&self, tab_ix: usize, cx: &App) -> bool {
        match self.tabs.get(tab_ix) {
            Some(Tab::Document(document_view)) => {
                let document_view = document_view.read(cx);
                document_view.translation_session_id().is_none()
                    && !self
                        .document_translation_jobs
                        .read(cx)
                        .has_job_for_source(document_view.path())
            }
            Some(Tab::NewTab(_)) => true,
            Some(Tab::LoadingDocument(_)) | Some(Tab::Keymap(_)) | None => false,
        }
    }

    fn open_tab_context_menu(
        &mut self,
        tab_ix: usize,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.synchronized_presentation.connection_tabs.is_empty() {
            cx.emit(PaneEvent::FinishSynchronizedConnection);
            window.prevent_default();
            cx.stop_propagation();
            return;
        }
        let Some(tab_id) = self.tab_id_at(tab_ix) else {
            return;
        };
        let is_document = self.tab_is_document(tab_id);
        let saved_password_path = self.tabs.get(tab_ix).and_then(|tab| match tab {
            Tab::Document(document_view) => {
                let document_view = document_view.read(cx);
                let path = document_view.path();
                (document_view.is_password_protected()
                    && cx.has_global::<PdfPasswordStore>()
                    && cx.global::<PdfPasswordStore>().is_known_saved(path))
                .then(|| path.to_path_buf())
            }
            _ => None,
        });
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
        let pane = cx.entity();
        let popup = PopupMenu::build(window, cx, move |mut menu, _, _| {
            if is_document {
                let duplicate_pane = pane.clone();
                menu = menu.item(
                    PopupMenuItem::new(crate::fl!("toolbar-duplicate-tab"))
                        .icon(IconName::Copy)
                        .on_click(move |_, _, cx| {
                            duplicate_pane.update(cx, |pane, cx| {
                                if let Some(tab_ix) = pane.tab_ix_for_entity_id(tab_id) {
                                    cx.emit(PaneEvent::DuplicateTab(tab_ix));
                                }
                            });
                        }),
                );
                if let Some(path) = saved_password_path.clone() {
                    let forget_pane = pane.clone();
                    menu = menu.item(
                        PopupMenuItem::new(crate::fl!("pdf-password-forget"))
                            .icon(IconName::Delete)
                            .on_click(move |_, window, cx| {
                                forget_pane.update(cx, |pane, cx| {
                                    pane.forget_saved_document_password(path.clone(), window, cx);
                                });
                            }),
                    );
                }
                menu = menu.separator();
                if group.is_some() {
                    let connect_pane = pane.clone();
                    menu = menu.item(
                        PopupMenuItem::new(crate::fl!("synchronized-tabs-connect-more"))
                            .icon(IconName::Link2)
                            .on_click(move |_, _, cx| {
                                connect_pane.update(cx, |_, cx| {
                                    cx.emit(PaneEvent::ConnectMoreSynchronizedTabs(tab_id));
                                });
                            }),
                    );

                    if !pause_locked {
                        let pause_pane = pane.clone();
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
                                    pause_pane.update(cx, |_, cx| {
                                        cx.emit(PaneEvent::SetSynchronizedScrollPaused {
                                            tab_id,
                                            paused,
                                        });
                                    });
                                },
                            ));
                    }

                    let leave_pane = pane.clone();
                    menu = menu.separator().item(
                        PopupMenuItem::new(crate::fl!("synchronized-tabs-leave"))
                            .icon(IconName::Unlink2)
                            .on_click(move |_, _, cx| {
                                leave_pane.update(cx, |_, cx| {
                                    cx.emit(PaneEvent::LeaveSynchronizedGroup(tab_id));
                                });
                            }),
                    );
                } else {
                    let create_pane = pane.clone();
                    menu = menu.item(
                        PopupMenuItem::new(crate::fl!("synchronized-tabs-create"))
                            .icon(IconName::Link2)
                            .on_click(move |_, _, cx| {
                                create_pane.update(cx, |_, cx| {
                                    cx.emit(PaneEvent::CreateSynchronizedGroup(tab_id));
                                });
                            }),
                    );
                }
                menu = menu.separator();
            }

            let close_pane = pane.clone();
            menu.item(
                PopupMenuItem::new(crate::fl!("toolbar-close-tab"))
                    .icon(IconName::X)
                    .on_click(move |_, _, cx| {
                        close_pane.update(cx, |pane, cx| {
                            if pane.has_tab_id(tab_id) {
                                cx.emit(PaneEvent::CloseTab(tab_id));
                            }
                        });
                    }),
            )
        });
        cx.subscribe_in(&popup, window, |this, popup, _: &DismissEvent, _, cx| {
            if this.context_menu_popup.as_ref() == Some(popup) {
                this.context_menu_popup = None;
                this.context_menu_position = None;
                cx.notify();
            }
        })
        .detach();
        self.context_menu_position = Some(position);
        self.context_menu_popup = Some(popup.clone());
        popup.focus_handle(cx).focus(window, cx);
        window.prevent_default();
        cx.stop_propagation();
        cx.notify();
    }

    fn tab_entity_id(tab: &Tab) -> EntityId {
        match tab {
            Tab::NewTab(new_tab) => new_tab.entity_id(),
            Tab::LoadingDocument(loading) => loading.entity_id(),
            Tab::Document(document_view) => document_view.entity_id(),
            Tab::Keymap(keymap_editor) => keymap_editor.entity_id(),
        }
    }

    fn active_tab_content(&self) -> AnyElement {
        match self.tabs.get(self.active_tab) {
            Some(Tab::Document(document_view)) => document_view.clone().into_any_element(),
            Some(Tab::LoadingDocument(loading)) => loading.clone().into_any_element(),
            Some(Tab::NewTab(new_tab)) => new_tab.clone().into_any_element(),
            Some(Tab::Keymap(keymap_editor)) => keymap_editor.clone().into_any_element(),
            None => div().into_any_element(),
        }
    }

    fn render_tab(
        &self,
        tab_ix: usize,
        source_window: Option<WindowHandle<Root>>,
        source_pane: Entity<Pane>,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> ComponentTab {
        let label = self.tab_label(tab_ix, cx);
        let draggable = self.tab_is_draggable(tab_ix, cx);
        let tab_id = self
            .tabs
            .get(tab_ix)
            .map(Self::tab_entity_id)
            .unwrap_or_else(|| cx.entity_id());
        let group = self
            .synchronized_presentation
            .group_by_tab
            .get(&tab_id)
            .cloned();
        let is_paused = self.synchronized_presentation.paused_tabs.contains(&tab_id);
        let synchronized_theme = cx.global::<SettingsStore>().resolved_theme;
        let current_color = group
            .as_ref()
            .filter(|group| group.is_current)
            .map(|group| crate::synchronized_group_color(synchronized_theme, group.color_slot));
        let connection_color = self
            .synchronized_presentation
            .connection_color_slot
            .map(|color_slot| crate::synchronized_group_color(synchronized_theme, color_slot));
        let recently_added = self.synchronized_presentation.recently_added;
        let connection_state = self
            .synchronized_presentation
            .connection_tabs
            .get(&tab_id)
            .copied();
        let occurrence = self.document_tab_occurrence(tab_ix, cx);
        let accessible_label = occurrence.map(|(ordinal, total)| {
            crate::fl!(
                "tab-document-view-label",
                name = label.to_string(),
                index = ordinal as i64,
                count = total as i64
            )
        });
        let close_pane = source_pane.clone();
        let close_button = Button::new(("pane-tab-close", tab_id))
            .small()
            .compact()
            .ghost()
            .icon(IconName::X)
            .tooltip(crate::fl!("toolbar-close-tab"))
            .debug_selector(move || format!("pane-tab-close-{tab_ix}"))
            .on_click(move |_, _, cx| {
                cx.stop_propagation();
                close_pane.update(cx, |pane, cx| {
                    if pane.has_tab_id(tab_id) {
                        cx.emit(PaneEvent::CloseTab(tab_id));
                    }
                });
            });

        let group_marker = group.map(|group| {
            let color = crate::synchronized_group_color(synchronized_theme, group.color_slot);
            let marker_width = 16.0;
            let marker_height = 20.0;
            let marker = div()
                .id(("synchronized-tab-group-link", tab_id))
                .w(px(marker_width))
                .h(px(marker_height))
                .flex()
                .items_center()
                .justify_center()
                .debug_selector(|| "synchronized-tab-group-link".to_string())
                .tooltip(glyphst_ui::tooltip::text_tooltip(crate::fl!(
                    "synchronized-tabs-group-summary",
                    number = group.display_number,
                    count = group.member_count
                )))
                .child(
                    Icon::new(IconName::Link2)
                        .size(ui_rems(12.0))
                        .text_color(color),
                );
            let is_recently_added = recently_added
                .is_some_and(|recent| recent.tab_id == tab_id && recent.group_id == group.id);
            if is_recently_added {
                let sequence = recently_added.map_or(0, |recent| recent.sequence);
                marker
                    .with_animation(
                        ("synchronized-tab-added", sequence as usize),
                        glyphst_ui::motion::HOVER.animation(),
                        move |element, progress| {
                            let scale = if progress < 0.6 {
                                0.6 + progress / 0.6 * 0.55
                            } else {
                                1.15 - (progress - 0.6) / 0.4 * 0.15
                            };
                            element
                                .w(px(marker_width * scale))
                                .h(px(marker_height * scale))
                                .opacity(progress)
                        },
                    )
                    .into_any_element()
            } else {
                marker.into_any_element()
            }
        });
        let group_marker = h_flex()
            .children(group_marker)
            .when(is_paused, |this| this.opacity(0.45));
        let suffix_bounds = self.tab_bounds.clone();
        let suffix = h_flex()
            .flex_none()
            .gap_1()
            .children(occurrence.map(|(ordinal, total)| {
                div()
                    .id(("pane-tab-occurrence", tab_id))
                    .min_w(px(16.0))
                    .h(px(16.0))
                    .px_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .bg(theme.muted)
                    .text_size(ui_rems(10.0))
                    .text_color(theme.muted_foreground)
                    .debug_selector(move || format!("pane-tab-copy-index-{tab_ix}"))
                    .tooltip(glyphst_ui::tooltip::text_tooltip(crate::fl!(
                        "tab-document-view-label",
                        name = label.to_string(),
                        index = ordinal as i64,
                        count = total as i64
                    )))
                    .child(ordinal.to_string())
            }))
            .child(group_marker)
            .when(is_paused, |this| {
                this.child(
                    div()
                        .w(px(12.0))
                        .h(px(12.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .debug_selector(|| "synchronized-tab-paused".to_string())
                        .child(
                            Icon::new(IconName::Pause)
                                .size(ui_rems(10.0))
                                .text_color(theme.muted_foreground),
                        ),
                )
            })
            .child(close_button)
            .on_prepaint(move |bounds, _, _| {
                suffix_bounds.borrow_mut().entry(tab_id).or_default().suffix = Some(bounds);
            });
        let tab_bounds = self.tab_bounds.clone();

        ComponentTab::new()
            .label(label.clone())
            .when_some(accessible_label, |this, label| this.aria_label(label))
            .suffix(suffix)
            .corner_radius(cx.theme().radius)
            .max_w(ui_rems(PANE_TAB_MAX_WIDTH))
            .min_w(ui_rems(PANE_TAB_MIN_WIDTH))
            .debug_selector(move || format!("pane-tab-{tab_ix}"))
            .on_prepaint(move |bounds, _, _| {
                tab_bounds.borrow_mut().entry(tab_id).or_default().body = Some(bounds);
            })
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |pane, event: &MouseDownEvent, window, cx| {
                    pane.open_tab_context_menu(tab_ix, event.position, window, cx);
                }),
            )
            .when_some(current_color, |this, color| {
                this.border_b(px(2.0)).border_color(color)
            })
            .when_some(connection_state, |this, state| match state {
                SynchronizedConnectionTabState::Source | SynchronizedConnectionTabState::Member => {
                    this.when_some(connection_color, |this, color| {
                        this.border_b(px(2.0)).border_color(color)
                    })
                }
                SynchronizedConnectionTabState::Eligible => {
                    let color = connection_color.unwrap_or(theme.primary);
                    let hover_group = format!("synchronized-candidate-{}", tab_id.as_u64());
                    let highlight = div()
                        .absolute()
                        .top_0()
                        .right_0()
                        .bottom_0()
                        .left_0()
                        .rounded_sm()
                        .group_hover(hover_group.clone(), move |style| {
                            style.bg(color.opacity(0.12)).border_2().border_color(color)
                        });
                    this.opacity(0.72)
                        .cursor_pointer()
                        .group(hover_group)
                        .prefix(highlight)
                }
                SynchronizedConnectionTabState::Invalid(_) => {
                    this.opacity(0.25).cursor_not_allowed()
                }
            })
            .when_some(
                source_window.filter(|_| draggable && connection_state.is_none()),
                |this, source_window| {
                    let drag_source_pane = source_pane.clone();
                    this.on_drag(
                        DraggedTab {
                            source_window,
                            source_pane: source_pane.clone(),
                            ix: tab_ix,
                            tab_id,
                            label: label.clone(),
                        },
                        |tab, _, _, cx| cx.new(|_| tab.clone()),
                    )
                    .drag_over::<DraggedTab>(move |style, dragged_tab, _, _| {
                        let style = style.bg(theme.list_hover);
                        if dragged_tab.source_pane != drag_source_pane || tab_ix < dragged_tab.ix {
                            style.border_l(px(2.0)).border_color(theme.primary)
                        } else if tab_ix > dragged_tab.ix {
                            style.border_r(px(2.0)).border_color(theme.primary)
                        } else {
                            style
                        }
                    })
                    .on_drop(cx.listener(
                        move |_, dragged_tab: &DraggedTab, _, cx| {
                            cx.emit(PaneEvent::TabDrop {
                                dragged_tab: Box::new(dragged_tab.clone()),
                                target: PaneDropTarget::Tab(tab_ix),
                            });
                        },
                    ))
                },
            )
    }

    fn render_tab_bar(&self, theme: Theme, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let source_window = window.window_handle().downcast::<Root>();
        let source_pane = cx.entity();
        let pane_id = cx.entity_id();
        let tabs = (0..self.tabs.len())
            .map(|tab_ix| self.render_tab(tab_ix, source_window, source_pane.clone(), theme, cx))
            .collect::<Vec<_>>();

        let activate_pane = source_pane.clone();
        let new_tab_pane = source_pane.clone();
        let new_tab_button = Button::new(("pane-new-tab", pane_id))
            .small()
            .compact()
            .ghost()
            .icon(IconName::Plus)
            .tooltip(crate::fl!("toolbar-new-tab"))
            .w(ui_rems(32.0))
            .h_full()
            .debug_selector(|| "pane-new-tab".to_string())
            .on_click(move |_, window, cx| {
                cx.stop_propagation();
                new_tab_pane.update(cx, |pane, cx| {
                    pane.open_new_tab(window, cx);
                    cx.emit(PaneEvent::Activate);
                });
            });

        let append_target = div()
            .id(("pane-tab-append", pane_id))
            .flex_1()
            .min_w(ui_rems(24.0))
            .h_full()
            .drag_over::<DraggedTab>(move |style, _, _, _| {
                style
                    .bg(theme.list_hover)
                    .border_l(px(2.0))
                    .border_color(theme.primary)
            })
            .on_drop(cx.listener(move |_, dragged_tab: &DraggedTab, _, cx| {
                cx.emit(PaneEvent::TabDrop {
                    dragged_tab: Box::new(dragged_tab.clone()),
                    target: PaneDropTarget::Append,
                });
            }));

        ComponentTabBar::new(("pane-tab-bar", pane_id))
            .flex_none()
            .w_full()
            .h(ui_rems(PANE_TAB_BAR_HEIGHT))
            .px(px(2.0))
            .tab_gap(px(2.0))
            .selected_index(self.active_tab)
            .menu(true)
            .children(tabs)
            .last_empty_space(append_target)
            .suffix(new_tab_button)
            .on_click(move |tab_ix, window, cx| {
                activate_pane.update(cx, |pane, cx| {
                    let was_active = pane.active_tab == *tab_ix;
                    let tab_id = pane.tab_id_at(*tab_ix);
                    pane.activate_tab(*tab_ix, window, cx);
                    if let Some(tab_id) = tab_id {
                        cx.emit(PaneEvent::TabClicked { tab_id, was_active });
                    }
                });
            })
            .into_any_element()
    }

    fn handle_content_drag_move(
        &mut self,
        event: &DragMoveEvent<DraggedTab>,
        cx: &mut Context<Self>,
    ) {
        let rect = event.bounds.size;
        let horizontal_threshold = rect.width * DROP_TARGET_SIZE;
        let vertical_threshold = rect.height * DROP_TARGET_SIZE;
        let relative_cursor = Point::new(
            event.event.position.x - event.bounds.left(),
            event.event.position.y - event.bounds.top(),
        );

        let direction = if relative_cursor.x < horizontal_threshold
            || relative_cursor.x > rect.width - horizontal_threshold
            || relative_cursor.y < vertical_threshold
            || relative_cursor.y > rect.height - vertical_threshold
        {
            [
                SplitDirection::Up,
                SplitDirection::Right,
                SplitDirection::Down,
                SplitDirection::Left,
            ]
            .iter()
            .min_by(|left, right| {
                let distance = |side: SplitDirection| match side {
                    SplitDirection::Up => relative_cursor.y,
                    SplitDirection::Right => rect.width - relative_cursor.x,
                    SplitDirection::Down => rect.height - relative_cursor.y,
                    SplitDirection::Left => relative_cursor.x,
                };
                distance(**left)
                    .partial_cmp(&distance(**right))
                    .unwrap_or(Ordering::Equal)
            })
            .copied()
        } else {
            None
        };

        if self.drag_split_direction != direction {
            self.drag_split_direction = direction;
            cx.notify();
        }
    }

    fn handle_external_document_drag_move(
        &mut self,
        event: &DragMoveEvent<ExternalPaths>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let is_inside = event.bounds.contains(&event.event.position);
        let feedback = is_inside
            .then(|| classify_external_document_paths(event.drag(cx).paths()).feedback())
            .flatten();
        if self.external_document_drop_feedback != feedback {
            self.external_document_drop_feedback = feedback;
            cx.notify();
        }
        if is_inside {
            cx.stop_propagation();
        }
    }

    fn handle_external_document_drop(
        &mut self,
        paths: &ExternalPaths,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.external_document_drop_feedback = None;
        cx.emit(PaneEvent::ExternalPathsDropped(paths.paths().to_vec()));
        cx.stop_propagation();
        cx.notify();
    }

    fn render_drop_overlay(&self, theme: Theme, cx: &mut Context<Self>) -> AnyElement {
        let direction = self.drag_split_direction;
        let half = relative(0.5);
        let preview = match direction {
            None => div().absolute().top_0().right_0().bottom_0().left_0(),
            Some(SplitDirection::Up) => div().absolute().top_0().left_0().right_0().h(half),
            Some(SplitDirection::Down) => div().absolute().left_0().bottom_0().right_0().h(half),
            Some(SplitDirection::Left) => div().absolute().top_0().left_0().bottom_0().w(half),
            Some(SplitDirection::Right) => div().absolute().top_0().bottom_0().right_0().w(half),
        };

        div()
            .invisible()
            .absolute()
            .top_0()
            .right_0()
            .bottom_0()
            .left_0()
            .drag_over::<DraggedTab>(|style, _, _, _| style.visible())
            .on_drop(cx.listener(move |this, dragged_tab: &DraggedTab, _, cx| {
                let split_direction = this.drag_split_direction.take();
                cx.emit(PaneEvent::TabDrop {
                    dragged_tab: Box::new(dragged_tab.clone()),
                    target: PaneDropTarget::Content(split_direction),
                });
                cx.notify();
            }))
            .on_drag_move::<DraggedTab>(cx.listener(
                |this, event: &DragMoveEvent<DraggedTab>, _, cx| {
                    this.handle_content_drag_move(event, cx);
                },
            ))
            .child(preview.bg(theme.overlay))
            .into_any_element()
    }
}

impl EventEmitter<PaneEvent> for Pane {}

impl Render for Pane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !cx.has_active_drag() {
            self.external_document_drop_feedback = None;
        }
        let theme = cx.theme().colors;
        let pane_id = cx.entity_id().as_u64();
        let external_drop_overlay = self.external_document_drop_feedback.map(|feedback| {
            render_external_document_drop_overlay_for_pane(feedback, theme, pane_id)
        });
        let tab_bar_visibility = cx.global::<SettingsStore>().settings.tab_bar_visibility;
        let show_tab_bar = tab_bar_visibility == TabBarVisibility::Always
            || self.force_show_tab_bar
            || self.tabs.len() >= 2;
        let context_menu = self
            .context_menu_position
            .zip(self.context_menu_popup.clone())
            .map(|(position, popup)| {
                deferred(
                    anchored()
                        .snap_to_window_with_margin(px(8.0))
                        .position(position)
                        .anchor(Anchor::TopLeft)
                        .child(popup),
                )
                .with_priority(20)
            });

        div()
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .overflow_hidden()
            .track_focus(&self.focus_handle)
            .on_drag_move::<ExternalPaths>(cx.listener(Self::handle_external_document_drag_move))
            .on_drop(cx.listener(Self::handle_external_document_drop))
            .when(show_tab_bar, |this| {
                this.child(self.render_tab_bar(theme, window, cx))
            })
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .overflow_hidden()
                    .capture_any_mouse_down(cx.listener(|this, _: &MouseDownEvent, window, cx| {
                        this.focus_handle.focus(window, cx);
                        cx.emit(PaneEvent::Activate);
                    }))
                    .child(self.active_tab_content())
                    .child(self.render_drop_overlay(theme, cx)),
            )
            .children(context_menu)
            .children(external_drop_overlay)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SplitDirection {
    Up,
    Down,
    Left,
    Right,
}

impl SplitDirection {
    pub(crate) fn axis(self) -> Axis {
        match self {
            Self::Up | Self::Down => Axis::Vertical,
            Self::Left | Self::Right => Axis::Horizontal,
        }
    }

    pub(crate) fn increasing(self) -> bool {
        matches!(self, Self::Down | Self::Right)
    }
}

#[derive(Clone)]
pub(crate) struct PaneGroup {
    root: Member,
}

pub(crate) struct PaneGroupSessionSnapshot {
    pub(crate) pane_tree: ReaderPaneTree,
    pub(crate) active_pane: u64,
    pub(crate) runtime_tab_ids: HashMap<EntityId, u64>,
}

impl PaneGroup {
    pub(crate) fn new(pane: Entity<Pane>) -> Self {
        Self {
            root: Member::Pane(pane),
        }
    }

    pub(crate) fn session_snapshot(
        &self,
        active_pane: &Entity<Pane>,
        cx: &App,
    ) -> PaneGroupSessionSnapshot {
        let mut next_pane_id = 0_u64;
        let mut next_tab_id = 0_u64;
        let mut active_pane_id = 0_u64;
        let mut runtime_tab_ids = HashMap::new();
        let pane_tree = self.root.session_snapshot(
            active_pane,
            &mut next_pane_id,
            &mut next_tab_id,
            &mut active_pane_id,
            &mut runtime_tab_ids,
            cx,
        );
        PaneGroupSessionSnapshot {
            pane_tree,
            active_pane: active_pane_id,
            runtime_tab_ids,
        }
    }

    pub(crate) fn restore(
        session: &ReaderPaneTree,
        document_translation_jobs: Entity<DocumentTranslationManager>,
        window: &mut Window,
        cx: &mut App,
    ) -> RestoredPaneGroup {
        let mut panes = Vec::new();
        let mut tab_ids = Vec::new();
        let mut pending_documents = 0;
        let mut missing_documents = 0;
        let root = Member::restore(
            session,
            &document_translation_jobs,
            &mut panes,
            &mut tab_ids,
            &mut pending_documents,
            &mut missing_documents,
            window,
            cx,
        );
        RestoredPaneGroup {
            pane_group: Self { root },
            panes,
            tab_ids,
            pending_documents,
            missing_documents,
        }
    }

    pub(crate) fn split(
        &mut self,
        old_pane: &Entity<Pane>,
        new_pane: &Entity<Pane>,
        direction: SplitDirection,
        cx: &mut App,
    ) {
        let found = match &mut self.root {
            Member::Pane(pane) => {
                if pane == old_pane {
                    self.root = Member::new_axis(old_pane.clone(), new_pane.clone(), direction, cx);
                    true
                } else {
                    false
                }
            }
            Member::Axis(axis) => axis.split(old_pane, new_pane, direction, cx),
        };

        if !found {
            let first_pane = self.root.first_pane();
            match &mut self.root {
                Member::Pane(_) => {
                    self.root = Member::new_axis(first_pane, new_pane.clone(), direction, cx);
                }
                Member::Axis(axis) => {
                    let _ = axis.split(&first_pane, new_pane, direction, cx);
                }
            }
        }
    }

    pub(crate) fn remove(&mut self, pane: &Entity<Pane>, cx: &mut App) -> bool {
        match &mut self.root {
            Member::Pane(_) => false,
            Member::Axis(axis) => match axis.remove(pane, cx) {
                Ok(Some(last_member)) => {
                    self.root = last_member;
                    true
                }
                Ok(None) => true,
                Err(_) => false,
            },
        }
    }

    pub(crate) fn panes(&self) -> Vec<Entity<Pane>> {
        let mut panes = Vec::new();
        self.root.collect_panes(&mut panes);
        panes
    }

    pub(crate) fn bounding_box_for_pane(&self, pane: &Entity<Pane>) -> Option<Bounds<Pixels>> {
        match &self.root {
            Member::Pane(_) => None,
            Member::Axis(axis) => axis.bounding_box_for_pane(pane),
        }
    }

    pub(crate) fn pane_at_pixel_position(&self, coordinate: Point<Pixels>) -> Option<Entity<Pane>> {
        match &self.root {
            Member::Pane(pane) => Some(pane.clone()),
            Member::Axis(axis) => axis.pane_at_pixel_position(coordinate).cloned(),
        }
    }

    pub(crate) fn find_pane_in_direction(
        &self,
        active_pane: &Entity<Pane>,
        direction: SplitDirection,
    ) -> Option<Entity<Pane>> {
        let bounding_box = self.bounding_box_for_pane(active_pane)?;
        let center = bounding_box.center();
        let distance = px(PANE_DIRECTION_PROBE_DISTANCE);
        let target = match direction {
            SplitDirection::Left => point(bounding_box.left() - distance, center.y),
            SplitDirection::Right => point(bounding_box.right() + distance, center.y),
            SplitDirection::Up => point(center.x, bounding_box.top() - distance),
            SplitDirection::Down => point(center.x, bounding_box.bottom() + distance),
        };
        self.pane_at_pixel_position(target)
    }

    pub(crate) fn swap(&mut self, from: &Entity<Pane>, to: &Entity<Pane>) {
        match &mut self.root {
            Member::Pane(_) => {}
            Member::Axis(axis) => axis.swap(from, to),
        }
    }

    pub(crate) fn move_to_border(
        &mut self,
        active_pane: &Entity<Pane>,
        direction: SplitDirection,
        cx: &mut App,
    ) -> bool {
        if let Some(pane) = self.find_pane_at_border(direction)
            && pane == *active_pane
        {
            return false;
        }

        if !self.remove(active_pane, cx) {
            return false;
        }

        if let Member::Axis(root) = &mut self.root
            && root.axis == direction.axis()
        {
            let idx = if direction.increasing() {
                root.members.len()
            } else {
                0
            };
            root.insert_pane(idx, active_pane, cx);
            return true;
        }

        let members = if direction.increasing() {
            vec![self.root.clone(), Member::Pane(active_pane.clone())]
        } else {
            vec![Member::Pane(active_pane.clone()), self.root.clone()]
        };
        self.root = Member::Axis(PaneAxis::new(direction.axis(), members, cx));
        true
    }

    pub(crate) fn resize(
        &mut self,
        pane: &Entity<Pane>,
        direction: SplitDirection,
        window: &mut Window,
        cx: &mut App,
    ) {
        let axis = direction.axis();
        let amount = if direction.increasing() {
            px(RESIZE_STEP)
        } else {
            px(-RESIZE_STEP)
        };
        if let Member::Axis(axis_group) = &mut self.root {
            let _ = axis_group.resize(pane, axis, amount, window, cx);
        }
    }

    pub(crate) fn reset_pane_sizes(&mut self, cx: &mut App) {
        self.root.reset_pane_sizes(cx);
    }

    pub(crate) fn retain_only(&mut self, pane: Entity<Pane>) {
        self.root = Member::Pane(pane);
    }

    pub(crate) fn render(
        &self,
        active_pane: &Entity<Pane>,
        theme: Theme,
        dim_inactive_panes: bool,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        let show_active_outline = matches!(&self.root, Member::Axis(_));
        self.root
            .render(
                0,
                active_pane,
                theme,
                dim_inactive_panes,
                show_active_outline,
                window,
                cx,
            )
            .element
    }

    fn find_pane_at_border(&self, direction: SplitDirection) -> Option<Entity<Pane>> {
        match &self.root {
            Member::Pane(pane) => Some(pane.clone()),
            Member::Axis(axis) => axis.find_pane_at_border(direction).cloned(),
        }
    }
}

#[derive(Clone)]
pub(crate) enum Member {
    Axis(PaneAxis),
    Pane(Entity<Pane>),
}

struct PaneRenderResult {
    element: AnyElement,
    contains_active_pane: bool,
}

impl Member {
    #[allow(clippy::too_many_arguments)]
    fn session_snapshot(
        &self,
        active_pane: &Entity<Pane>,
        next_pane_id: &mut u64,
        next_tab_id: &mut u64,
        active_pane_id: &mut u64,
        runtime_tab_ids: &mut HashMap<EntityId, u64>,
        cx: &App,
    ) -> ReaderPaneTree {
        match self {
            Self::Pane(pane) => {
                *next_pane_id = next_pane_id.saturating_add(1);
                let pane_id = *next_pane_id;
                if pane == active_pane {
                    *active_pane_id = pane_id;
                }
                ReaderPaneTree::Pane(pane.read(cx).session_snapshot(
                    pane_id,
                    next_tab_id,
                    runtime_tab_ids,
                    cx,
                ))
            }
            Self::Axis(axis) => {
                let children = axis
                    .members
                    .iter()
                    .map(|member| {
                        member.session_snapshot(
                            active_pane,
                            next_pane_id,
                            next_tab_id,
                            active_pane_id,
                            runtime_tab_ids,
                            cx,
                        )
                    })
                    .collect::<Vec<_>>();
                let sizes = axis.state.read(cx).sizes();
                let total = sizes.iter().map(|size| size.as_f32()).sum::<f32>();
                let ratios =
                    if sizes.len() == children.len() && total.is_finite() && total > f32::EPSILON {
                        sizes.iter().map(|size| size.as_f32() / total).collect()
                    } else {
                        vec![1.0 / children.len().max(1) as f32; children.len()]
                    };
                ReaderPaneTree::Split {
                    axis: match axis.axis {
                        Axis::Horizontal => ReaderSplitAxis::Horizontal,
                        Axis::Vertical => ReaderSplitAxis::Vertical,
                    },
                    ratios,
                    children,
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn restore(
        session: &ReaderPaneTree,
        document_translation_jobs: &Entity<DocumentTranslationManager>,
        panes: &mut Vec<(u64, Entity<Pane>)>,
        tab_ids: &mut Vec<(u64, EntityId, EntityId)>,
        pending_documents: &mut usize,
        missing_documents: &mut usize,
        window: &mut Window,
        cx: &mut App,
    ) -> Self {
        match session {
            ReaderPaneTree::Pane(pane_session) => {
                let pane =
                    cx.new(|cx| Pane::empty(cx.focus_handle(), document_translation_jobs.clone()));
                let restored =
                    pane.update(cx, |pane, cx| pane.restore_tabs(pane_session, window, cx));
                let pane_entity_id = pane.entity_id();
                tab_ids.extend(
                    restored
                        .tab_ids
                        .into_iter()
                        .map(|(session_id, tab_id)| (session_id, tab_id, pane_entity_id)),
                );
                *pending_documents += restored.pending_documents;
                *missing_documents += restored.missing_documents;
                panes.push((pane_session.id, pane.clone()));
                Self::Pane(pane)
            }
            ReaderPaneTree::Split {
                axis,
                ratios,
                children,
            } if !children.is_empty() => {
                let members = children
                    .iter()
                    .map(|child| {
                        Self::restore(
                            child,
                            document_translation_jobs,
                            panes,
                            tab_ids,
                            pending_documents,
                            missing_documents,
                            window,
                            cx,
                        )
                    })
                    .collect::<Vec<_>>();
                let axis = PaneAxis::new(
                    match axis {
                        ReaderSplitAxis::Horizontal => Axis::Horizontal,
                        ReaderSplitAxis::Vertical => Axis::Vertical,
                    },
                    members,
                    cx,
                );
                axis.state.update(cx, |state, cx| {
                    state.restore_panel_ratios(ratios, cx);
                });
                Self::Axis(axis)
            }
            ReaderPaneTree::Split { .. } => {
                let pane_session = ReaderPaneSession {
                    id: 1,
                    tabs: Vec::new(),
                    active_tab: None,
                };
                Self::restore(
                    &ReaderPaneTree::Pane(pane_session),
                    document_translation_jobs,
                    panes,
                    tab_ids,
                    pending_documents,
                    missing_documents,
                    window,
                    cx,
                )
            }
        }
    }

    fn new_axis(
        old_pane: Entity<Pane>,
        new_pane: Entity<Pane>,
        direction: SplitDirection,
        cx: &mut App,
    ) -> Self {
        let axis = direction.axis();
        let members = match direction {
            SplitDirection::Up | SplitDirection::Left => {
                vec![Member::Pane(new_pane), Member::Pane(old_pane)]
            }
            SplitDirection::Down | SplitDirection::Right => {
                vec![Member::Pane(old_pane), Member::Pane(new_pane)]
            }
        };

        Member::Axis(PaneAxis::new(axis, members, cx))
    }

    fn first_pane(&self) -> Entity<Pane> {
        match self {
            Member::Axis(axis) => axis.members[0].first_pane(),
            Member::Pane(pane) => pane.clone(),
        }
    }

    fn collect_panes(&self, panes: &mut Vec<Entity<Pane>>) {
        match self {
            Member::Axis(axis) => {
                for member in &axis.members {
                    member.collect_panes(panes);
                }
            }
            Member::Pane(pane) => panes.push(pane.clone()),
        }
    }

    fn reset_pane_sizes(&self, cx: &mut App) {
        match self {
            Member::Axis(axis) => axis.reset_pane_sizes(cx),
            Member::Pane(_) => {}
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn render(
        &self,
        basis: usize,
        active_pane: &Entity<Pane>,
        theme: Theme,
        dim_inactive_panes: bool,
        show_active_outline: bool,
        window: &mut Window,
        cx: &mut App,
    ) -> PaneRenderResult {
        match self {
            Member::Pane(pane) => {
                let is_active_pane = pane == active_pane;
                let pane_id = pane.entity_id().as_u64();
                let radius = cx.theme().radius;
                PaneRenderResult {
                    element: div()
                        .relative()
                        .flex_1()
                        .size_full()
                        .overflow_hidden()
                        .rounded(radius)
                        .border_1()
                        .border_color(if show_active_outline && is_active_pane {
                            theme.primary
                        } else {
                            theme.border
                        })
                        .bg(theme.background)
                        .debug_selector(move || format!("pane-surface-{pane_id}"))
                        .when(show_active_outline && is_active_pane, |this| {
                            this.debug_selector(|| "active-pane-surface".to_string())
                        })
                        .child(pane.clone())
                        .when(dim_inactive_panes && !is_active_pane, |this| {
                            this.child(
                                div()
                                    .absolute()
                                    .top_0()
                                    .right_0()
                                    .bottom_0()
                                    .left_0()
                                    .bg(theme.overlay),
                            )
                        })
                        .into_any_element(),
                    contains_active_pane: is_active_pane,
                }
            }
            Member::Axis(axis) => axis.render(
                basis + 1,
                active_pane,
                theme,
                dim_inactive_panes,
                show_active_outline,
                window,
                cx,
            ),
        }
    }
}

#[derive(Clone)]
pub(crate) struct PaneAxis {
    axis: Axis,
    members: Vec<Member>,
    state: Entity<ResizableState>,
    bounding_boxes: Rc<RefCell<Vec<Option<Bounds<Pixels>>>>>,
}

impl PaneAxis {
    fn new(axis: Axis, members: Vec<Member>, cx: &mut App) -> Self {
        let len = members.len();
        Self {
            axis,
            members,
            state: cx.new(|_| ResizableState::default()),
            bounding_boxes: Rc::new(RefCell::new(vec![None; len])),
        }
    }

    fn split(
        &mut self,
        old_pane: &Entity<Pane>,
        new_pane: &Entity<Pane>,
        direction: SplitDirection,
        cx: &mut App,
    ) -> bool {
        for (mut idx, member) in self.members.iter_mut().enumerate() {
            match member {
                Member::Axis(axis) => {
                    if axis.split(old_pane, new_pane, direction, cx) {
                        return true;
                    }
                }
                Member::Pane(pane) => {
                    if pane == old_pane {
                        if direction.axis() == self.axis {
                            if direction.increasing() {
                                idx += 1;
                            }
                            self.insert_pane(idx, new_pane, cx);
                        } else {
                            *member =
                                Member::new_axis(old_pane.clone(), new_pane.clone(), direction, cx);
                        }
                        return true;
                    }
                }
            }
        }
        false
    }

    fn insert_pane(&mut self, idx: usize, new_pane: &Entity<Pane>, cx: &mut App) {
        self.members.insert(idx, Member::Pane(new_pane.clone()));
        self.state
            .update(cx, |state, cx| state.insert_panel(None, Some(idx), cx));
        *self.bounding_boxes.borrow_mut() = vec![None; self.members.len()];
    }

    fn remove(&mut self, pane_to_remove: &Entity<Pane>, cx: &mut App) -> Result<Option<Member>> {
        let mut remove_member = None;
        let mut found = false;

        for (idx, member) in self.members.iter_mut().enumerate() {
            match member {
                Member::Axis(axis) => {
                    if let Ok(last_member) = axis.remove(pane_to_remove, cx) {
                        if let Some(last_member) = last_member {
                            *member = last_member;
                        }
                        found = true;
                        break;
                    }
                }
                Member::Pane(pane) => {
                    if pane == pane_to_remove {
                        remove_member = Some(idx);
                        found = true;
                        break;
                    }
                }
            }
        }

        if !found {
            bail!("Pane not found");
        }

        if let Some(idx) = remove_member {
            self.members.remove(idx);
            self.state
                .update(cx, |state, cx| state.remove_panel(idx, cx));
        }
        *self.bounding_boxes.borrow_mut() = vec![None; self.members.len()];

        if self.members.len() == 1 {
            Ok(self.members.pop())
        } else {
            Ok(None)
        }
    }

    fn find_pane_at_border(&self, direction: SplitDirection) -> Option<&Entity<Pane>> {
        if self.axis != direction.axis() {
            return None;
        }
        let member = if direction.increasing() {
            self.members.last()
        } else {
            self.members.first()
        };
        member.and_then(|member| match member {
            Member::Pane(pane) => Some(pane),
            Member::Axis(axis) => axis.find_pane_at_border(direction),
        })
    }

    fn reset_pane_sizes(&self, cx: &mut App) {
        self.state
            .update(cx, |state, cx| state.reset_panel_sizes(cx));
        for member in &self.members {
            member.reset_pane_sizes(cx);
        }
    }

    fn resize(
        &mut self,
        pane: &Entity<Pane>,
        axis: Axis,
        amount: Pixels,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<bool> {
        let found_pane = self
            .members
            .iter()
            .any(|member| matches!(member, Member::Pane(p) if p == pane));

        if found_pane && self.axis != axis {
            return Some(false);
        }

        let mut found_axis_index = None;
        if !found_pane {
            for (idx, member) in self.members.iter_mut().enumerate() {
                if let Member::Axis(child_axis) = member
                    && let Some(done) = child_axis.resize(pane, axis, amount, window, cx)
                {
                    if done {
                        return Some(true);
                    } else if self.axis != axis {
                        return Some(false);
                    } else {
                        found_axis_index = Some(idx);
                    }
                }
            }
            found_axis_index?;
        }

        let ix = if found_pane {
            self.members.iter().position(|member| {
                if let Member::Pane(p) = member {
                    p == pane
                } else {
                    false
                }
            })
        } else {
            found_axis_index
        }
        .unwrap_or(0);

        let current_size = self.state.read(cx).sizes().get(ix).copied();
        if let Some(current_size) = current_size {
            self.state.update(cx, |state, cx| {
                state.resize_panel(ix, current_size + amount, window, cx);
            });
        }
        Some(true)
    }

    fn swap(&mut self, from: &Entity<Pane>, to: &Entity<Pane>) {
        for member in &mut self.members {
            match member {
                Member::Axis(axis) => axis.swap(from, to),
                Member::Pane(pane) => {
                    if pane == from {
                        *member = Member::Pane(to.clone());
                    } else if pane == to {
                        *member = Member::Pane(from.clone());
                    }
                }
            }
        }
    }

    pub(crate) fn bounding_box_for_pane(&self, pane: &Entity<Pane>) -> Option<Bounds<Pixels>> {
        for (idx, member) in self.members.iter().enumerate() {
            match member {
                Member::Pane(found) if found == pane => return self.bounding_boxes.borrow()[idx],
                Member::Axis(axis) => {
                    if let Some(bounds) = axis.bounding_box_for_pane(pane) {
                        return Some(bounds);
                    }
                }
                Member::Pane(_) => {}
            }
        }
        None
    }

    fn pane_at_pixel_position(&self, coordinate: Point<Pixels>) -> Option<&Entity<Pane>> {
        let bounding_boxes = self.bounding_boxes.borrow();
        for (idx, member) in self.members.iter().enumerate() {
            if let Some(bounds) = bounding_boxes.get(idx).copied().flatten()
                && bounds.contains(&coordinate)
            {
                return match member {
                    Member::Pane(pane) => Some(pane),
                    Member::Axis(axis) => axis.pane_at_pixel_position(coordinate),
                };
            }
        }
        None
    }

    #[allow(clippy::too_many_arguments)]
    fn render(
        &self,
        basis: usize,
        active_pane: &Entity<Pane>,
        theme: Theme,
        dim_inactive_panes: bool,
        show_active_outline: bool,
        window: &mut Window,
        cx: &mut App,
    ) -> PaneRenderResult {
        let mut contains_active_pane = false;
        let min_size = match self.axis {
            Axis::Horizontal => px(HORIZONTAL_MIN_SIZE),
            Axis::Vertical => px(VERTICAL_MIN_SIZE),
        };
        let panels = self
            .members
            .iter()
            .enumerate()
            .map(|(ix, member)| {
                let result = member.render(
                    (basis + ix) * 10,
                    active_pane,
                    theme,
                    dim_inactive_panes,
                    show_active_outline,
                    window,
                    cx,
                );
                if result.contains_active_pane {
                    contains_active_pane = true;
                }
                let bounding_boxes = self.bounding_boxes.clone();
                let child = div()
                    .relative()
                    .size_full()
                    .on_prepaint(move |bounds, _, _| {
                        if let Some(slot) = bounding_boxes.borrow_mut().get_mut(ix) {
                            *slot = Some(bounds);
                        }
                    })
                    .child(result.element);

                resizable_panel()
                    .size_range(min_size..Pixels::MAX)
                    .child(child)
            })
            .collect::<Vec<_>>();

        let element = ResizablePanelGroup::new(("pane-axis", basis))
            .with_state(&self.state)
            .axis(self.axis)
            .gap(px(glyphst_ui::style::SURFACE_GAP))
            .children(panels)
            .into_any_element();

        PaneRenderResult {
            element,
            contains_active_pane,
        }
    }
}

#[cfg(test)]
mod tests;
