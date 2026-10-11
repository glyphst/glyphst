//! One workspace tab: its content, and the document open flow that turns a
//! loading tab into a document in place.
//!
//! A tab is a GPUI Kit dock panel. Its entity id is the tab id the workspace,
//! the synchronized-tab registry and the session use, and it does not change
//! when a loading document finishes opening.

use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

#[cfg(all(debug_assertions, not(test)))]
use std::sync::OnceLock;

use anyhow::{Result, bail};
use glyphst_core::{DocumentColors, DocumentFormat};
use glyphst_pdf::{DocumentAuthenticationError, DocumentPassword};
use glyphst_translation_ui::{DocumentTranslationId, DocumentTranslationManager};
use gpui::{
    Animation, AnimationExt, AnyElement, App, AppContext, Context, Entity, EventEmitter,
    FocusHandle, Focusable, IntoElement, ParentElement, Render, SharedString, Styled, Subscription,
    Task, Window, div, prelude::*, px,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
    dock::{BasePanel, PanelEvent},
    h_flex,
    input::{Input, InputContentType, InputEvent, InputState},
    notification::Notification,
    v_flex,
};

use crate::{
    KeymapEditor,
    document_view::{
        DocumentAnnotations, DocumentOpenOptions, DocumentView, DocumentViewEvent,
        PreparedDocumentOpen, SynchronizedScrollDelta,
    },
    icons::{Icon, IconName},
    library::{DocumentPosition, LibraryStore, ReaderNewTabSession, ReaderTabKind, normalize_path},
    new_tab::{NewTab, NewTabEvent},
    pdf_password_store::{PdfPasswordStore, StoredPdfPassword},
    settings::{SettingsStore, ui_rems},
};

const LOADING_GRID_SQUARE_SIZE: f32 = 18.0;
const LOADING_GRID_GAP: f32 = 4.0;
const LOADING_GRID_PHASES: [f32; 9] = [0.80, 0.05, 0.50, 0.30, 0.95, 0.15, 0.65, 0.40, 0.70];
const LOADING_GRID_DURATION: Duration = Duration::from_millis(1200);

#[cfg(all(debug_assertions, not(test)))]
const DEBUG_FILE_OPEN_MIN_MS_ENV: &str = "GLYPHST_DEBUG_FILE_OPEN_MIN_MS";

pub(crate) fn loading_grid_opacity(delta: f32, phase: f32) -> f32 {
    let t = (delta + phase) % 1.0;
    let wave = (1.0 - (std::f32::consts::TAU * t).cos()) / 2.0;
    0.2 + 0.8 * wave
}

#[cfg(any(debug_assertions, test))]
pub(crate) fn parse_debug_file_open_min_duration(value: &str) -> Option<Duration> {
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

pub(crate) fn remaining_file_open_min_duration(
    minimum: Option<Duration>,
    elapsed: Duration,
) -> Option<Duration> {
    minimum
        .and_then(|minimum| minimum.checked_sub(elapsed))
        .filter(|remaining| !remaining.is_zero())
}

pub(crate) fn is_supported_document_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .and_then(DocumentFormat::from_extension)
        .is_some()
}

/// The open options a document gets from the library and the settings.
fn document_open_options(
    path: &Path,
    position: DocumentPosition,
    target_language: Option<String>,
    document_translation_jobs: &Entity<DocumentTranslationManager>,
    cx: &App,
) -> DocumentOpenOptions {
    let library = cx.global::<LibraryStore>();
    let settings = &cx.global::<SettingsStore>().settings;
    DocumentOpenOptions {
        initial_position: Some(position),
        annotations: DocumentAnnotations {
            highlights: library.highlights_for(path),
            notes: library.notes_for(path),
        },
        target_language: target_language
            .or_else(|| library.document_target_language(path))
            .unwrap_or_else(|| settings.translation.resolved_target_language()),
        epub_preferences: library
            .epub_reader_preferences(path)
            .unwrap_or(settings.reading),
        rotation: library.document_rotation(path),
        page_layout: library.document_page_layout(path),
        document_translation_jobs: document_translation_jobs.clone(),
    }
}

#[derive(Clone)]
#[allow(clippy::enum_variant_names)]
pub(crate) enum TabContent {
    NewTab(Entity<NewTab>),
    LoadingDocument(Entity<LoadingDocumentView>),
    Document(Entity<DocumentView>),
    Keymap(Entity<KeymapEditor>),
}

pub(crate) struct LoadingDocumentView {
    pub(crate) path: PathBuf,
    initial_position: DocumentPosition,
    restore_tab_id: Option<u64>,
    job_ids: Vec<DocumentTranslationId>,
    pub(crate) password_input: Entity<InputState>,
    pub(crate) remember_password: bool,
    pub(crate) status: LoadingDocumentStatus,
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
pub(crate) enum LoadingDocumentStatus {
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
    fn new(
        path: PathBuf,
        initial_position: DocumentPosition,
        restore_tab_id: Option<u64>,
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
                InputEvent::Focus | InputEvent::Blur => {}
            },
        )
        .detach();

        Self {
            path,
            initial_position,
            restore_tab_id,
            job_ids,
            password_input,
            remember_password: true,
            status: LoadingDocumentStatus::Loading,
            pending_open: Some(pending_open),
            _task: None,
        }
    }

    pub(crate) fn submit_password(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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

/// What a tab reports to the workspace that holds it.
#[derive(Clone)]
pub(crate) enum TabItemEvent {
    OpenFile,
    OpenDocument {
        path: PathBuf,
        position: Option<DocumentPosition>,
        replace_active_new_tab: bool,
    },
    OpenSettings,
    SidebarNavigationRequested,
    DocumentOpenFinished {
        job_id: DocumentTranslationId,
        result: std::result::Result<(), SharedString>,
    },
    RestoredDocumentFinished {
        session_tab_id: u64,
        opened: bool,
    },
    DocumentPasswordAuthenticated {
        path: PathBuf,
        password: DocumentPassword,
    },
    /// The tab asked to be closed, e.g. a cancelled password prompt.
    CloseRequested,
    /// The content changed kind, e.g. a loading tab became a document.
    ContentChanged,
    DocumentScrolled(SynchronizedScrollDelta),
}

pub(crate) struct TabItem {
    content: TabContent,
    focus_handle: FocusHandle,
    document_translation_jobs: Entity<DocumentTranslationManager>,
    content_subscriptions: Vec<Subscription>,
}

impl EventEmitter<TabItemEvent> for TabItem {}
impl EventEmitter<PanelEvent> for TabItem {}

impl TabItem {
    fn with_content(
        content: impl FnOnce(&FocusHandle, &mut Window, &mut Context<Self>) -> TabContent,
        document_translation_jobs: Entity<DocumentTranslationManager>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        let content = content(&focus_handle, window, cx);
        let mut this = Self {
            content,
            focus_handle,
            document_translation_jobs,
            content_subscriptions: Vec::new(),
        };
        this.subscribe_to_content(window, cx);
        this
    }

    pub(crate) fn new_tab(
        document_translation_jobs: Entity<DocumentTranslationManager>,
        window: &mut Window,
        cx: &mut App,
    ) -> Entity<Self> {
        cx.new(|cx| {
            Self::with_content(
                |focus_handle, window, cx| {
                    let focus_handle = focus_handle.clone();
                    TabContent::NewTab(cx.new(|cx| NewTab::new(focus_handle, window, cx)))
                },
                document_translation_jobs,
                window,
                cx,
            )
        })
    }

    pub(crate) fn new_tab_with_session(
        state: ReaderNewTabSession,
        document_translation_jobs: Entity<DocumentTranslationManager>,
        window: &mut Window,
        cx: &mut App,
    ) -> Entity<Self> {
        cx.new(|cx| {
            Self::with_content(
                |focus_handle, window, cx| {
                    let focus_handle = focus_handle.clone();
                    TabContent::NewTab(
                        cx.new(|cx| NewTab::new_with_session(focus_handle, state, window, cx)),
                    )
                },
                document_translation_jobs,
                window,
                cx,
            )
        })
    }

    pub(crate) fn keymap(
        document_translation_jobs: Entity<DocumentTranslationManager>,
        window: &mut Window,
        cx: &mut App,
    ) -> Entity<Self> {
        cx.new(|cx| {
            Self::with_content(
                |focus_handle, window, cx| {
                    let focus_handle = focus_handle.clone();
                    TabContent::Keymap(cx.new(|cx| KeymapEditor::new(focus_handle, window, cx)))
                },
                document_translation_jobs,
                window,
                cx,
            )
        })
    }

    /// A tab that opens `path`, starting in the loading state.
    ///
    /// `position` falls back to the document's history position; a restored
    /// tab passes its session id so the workspace can count it in.
    pub(crate) fn open_document(
        path: &Path,
        position: Option<DocumentPosition>,
        restore_tab_id: Option<u64>,
        job_id: Option<DocumentTranslationId>,
        document_translation_jobs: Entity<DocumentTranslationManager>,
        window: &mut Window,
        cx: &mut App,
    ) -> Result<Entity<Self>> {
        let path = normalize_path(path);
        if !path.exists() {
            bail!("document no longer exists: {}", path.display());
        }
        if !is_supported_document_path(&path) {
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
        let options = document_open_options(
            &path,
            initial_position.clone(),
            None,
            &document_translation_jobs,
            cx,
        );
        let pending_open = PendingDocumentOpen {
            behavior,
            epub_preferences: options.epub_preferences,
            options,
            page_gap,
            document_colors,
        };
        Ok(cx.new(|cx| {
            let mut this = Self::with_content(
                |_, window, cx| {
                    TabContent::LoadingDocument(cx.new(|cx| {
                        LoadingDocumentView::new(
                            path,
                            initial_position,
                            restore_tab_id,
                            job_id.into_iter().collect(),
                            pending_open,
                            window,
                            cx,
                        )
                    }))
                },
                document_translation_jobs,
                window,
                cx,
            );
            this.start_document_prepare(
                DocumentOpenAttempt::Initial,
                debug_file_open_min_duration(),
                window,
                cx,
            );
            this
        }))
    }

    /// A second view of an open document, at its current position.
    ///
    /// `session_id` attaches a live translation session to the duplicate,
    /// which then follows the session's target language.
    pub(crate) fn duplicate_document(
        source: &Entity<DocumentView>,
        session_id: Option<DocumentTranslationId>,
        document_translation_jobs: Entity<DocumentTranslationManager>,
        window: &mut Window,
        cx: &mut App,
    ) -> Result<Entity<Self>> {
        let (path, position, prepared) = {
            let source = source.read(cx);
            (
                source.path().to_path_buf(),
                source.current_document_position(),
                source.prepared_duplicate(),
            )
        };
        let behavior = cx.global::<SettingsStore>().settings.behavior;
        let target_language = session_id.and_then(|session_id| {
            document_translation_jobs
                .read(cx)
                .session_snapshot(session_id)
                .map(|snapshot| snapshot.request.target_language.as_str().to_string())
        });
        let options = document_open_options(
            &path,
            position.clone(),
            target_language,
            &document_translation_jobs,
            cx,
        );
        let focus_handle = cx.focus_handle();
        let duplicate =
            DocumentView::from_prepared(prepared, focus_handle.clone(), behavior, options)?;
        let item = cx.new(|cx| {
            let duplicate = cx.new(|cx| {
                let mut duplicate = duplicate;
                duplicate.register_cache_eviction(cx);
                duplicate.initialize_runtime(window, cx);
                if let Some(session_id) = session_id {
                    duplicate.attach_translation_session(session_id, cx);
                }
                duplicate
            });
            let mut this = Self {
                content: TabContent::Document(duplicate),
                focus_handle,
                document_translation_jobs,
                content_subscriptions: Vec::new(),
            };
            this.subscribe_to_content(window, cx);
            this
        });
        if session_id.is_none() {
            cx.global_mut::<LibraryStore>()
                .record_history(&path, position);
        }
        Ok(item)
    }

    pub(crate) fn document_view(&self) -> Option<Entity<DocumentView>> {
        match &self.content {
            TabContent::Document(document_view) => Some(document_view.clone()),
            TabContent::NewTab(_) | TabContent::LoadingDocument(_) | TabContent::Keymap(_) => None,
        }
    }

    pub(crate) fn is_document(&self) -> bool {
        matches!(self.content, TabContent::Document(_))
    }

    pub(crate) fn is_new_tab(&self) -> bool {
        matches!(self.content, TabContent::NewTab(_))
    }

    pub(crate) fn is_loading(&self) -> bool {
        matches!(self.content, TabContent::LoadingDocument(_))
    }

    pub(crate) fn is_keymap(&self) -> bool {
        matches!(self.content, TabContent::Keymap(_))
    }

    pub(crate) fn supports_zen_mode(&self) -> bool {
        matches!(
            self.content,
            TabContent::Document(_) | TabContent::LoadingDocument(_)
        )
    }

    /// The document this tab shows or is opening.
    pub(crate) fn document_path(&self, cx: &App) -> Option<PathBuf> {
        match &self.content {
            TabContent::Document(document_view) => {
                Some(document_view.read(cx).path().to_path_buf())
            }
            TabContent::LoadingDocument(loading) => Some(loading.read(cx).path.clone()),
            TabContent::NewTab(_) | TabContent::Keymap(_) => None,
        }
    }

    pub(crate) fn label(&self, cx: &App) -> SharedString {
        match &self.content {
            TabContent::Document(document_view) => document_view.read(cx).file_name(),
            TabContent::LoadingDocument(loading) => loading
                .read(cx)
                .path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("Loading…")
                .to_string()
                .into(),
            TabContent::Keymap(_) => crate::fl!("keymap-title").into(),
            TabContent::NewTab(_) => crate::fl!("new-tab-title").into(),
        }
    }

    /// Whether the tab may leave its group: a document that a translation is
    /// reading from or writing to stays put, and so does a loading one.
    pub(crate) fn is_draggable(&self, cx: &App) -> bool {
        match &self.content {
            TabContent::Document(document_view) => {
                let document_view = document_view.read(cx);
                document_view.translation_session_id().is_none()
                    && !self
                        .document_translation_jobs
                        .read(cx)
                        .has_job_for_source(document_view.path())
            }
            TabContent::NewTab(_) => true,
            TabContent::LoadingDocument(_) | TabContent::Keymap(_) => false,
        }
    }

    pub(crate) fn session_kind(&self, cx: &App) -> ReaderTabKind {
        match &self.content {
            TabContent::NewTab(new_tab) => ReaderTabKind::NewTab(new_tab.read(cx).session_state()),
            TabContent::LoadingDocument(loading) => {
                let loading = loading.read(cx);
                ReaderTabKind::Document {
                    path: loading.path.clone(),
                    position: loading.initial_position.clone(),
                }
            }
            TabContent::Document(document_view) => {
                let document_view = document_view.read(cx);
                ReaderTabKind::Document {
                    path: document_view.path().to_path_buf(),
                    position: document_view.current_document_position(),
                }
            }
            TabContent::Keymap(_) => ReaderTabKind::Keymap,
        }
    }

    /// The password the document was opened with, when it has a saved one.
    pub(crate) fn saved_password_path(&self, cx: &App) -> Option<PathBuf> {
        let document_view = self.document_view()?;
        let document_view = document_view.read(cx);
        let path = document_view.path();
        (document_view.is_password_protected()
            && cx.has_global::<PdfPasswordStore>()
            && cx.global::<PdfPasswordStore>().is_known_saved(path))
        .then(|| path.to_path_buf())
    }

    /// Persist the reading position and release the page render cache. Done
    /// when the tab stops being displayed and when it leaves.
    pub(crate) fn flush(&self, cx: &mut App) {
        if let TabContent::Document(document_view) = &self.content {
            document_view.update(cx, |document_view, cx| {
                document_view.flush_current_history_position(cx);
                document_view.deactivate_render_cache(cx);
            });
        }
    }

    /// Move the tab's listeners and translation manager to the window it was
    /// dragged into.
    pub(crate) fn attach_to_window(
        &mut self,
        jobs: Entity<DocumentTranslationManager>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.document_translation_jobs = jobs.clone();
        if let TabContent::Document(document_view) = &self.content {
            document_view.update(cx, |document_view, cx| {
                document_view.set_document_translation_jobs(jobs, cx);
            });
        }
        self.subscribe_to_content(window, cx);
    }

    /// Report the open job to `job_id` once the document is ready, or now if
    /// it already is.
    pub(crate) fn associate_open_job(
        &mut self,
        job_id: DocumentTranslationId,
        cx: &mut Context<Self>,
    ) {
        match &self.content {
            TabContent::Document(_) => cx.emit(TabItemEvent::DocumentOpenFinished {
                job_id,
                result: Ok(()),
            }),
            TabContent::LoadingDocument(loading) => {
                loading.update(cx, |loading, _| {
                    if !loading.job_ids.contains(&job_id) {
                        loading.job_ids.push(job_id);
                    }
                });
            }
            TabContent::NewTab(_) | TabContent::Keymap(_) => {}
        }
    }

    /// Retry a password-protected open of `path` with a password another tab
    /// was just unlocked with.
    pub(crate) fn unlock_with_password(
        &mut self,
        path: &Path,
        password: DocumentPassword,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let TabContent::LoadingDocument(loading) = &self.content else {
            return;
        };
        let pending = loading.read(cx);
        if pending.path != normalize_path(path) || pending.pending_open.is_none() {
            return;
        }
        self.start_document_prepare(DocumentOpenAttempt::Shared(password), None, window, cx);
    }

    #[cfg(test)]
    pub(crate) fn submit_document_password_for_test(
        &mut self,
        password: &str,
        remember: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let TabContent::LoadingDocument(loading) = &self.content else {
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

    #[cfg(test)]
    pub(crate) fn loading_view(&self) -> Option<Entity<LoadingDocumentView>> {
        match &self.content {
            TabContent::LoadingDocument(loading) => Some(loading.clone()),
            _ => None,
        }
    }

    pub(crate) fn forget_saved_document_password(
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

    /// Subscribe to the current content, replacing the previous content's
    /// subscriptions. Called again when the content changes kind.
    fn subscribe_to_content(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.content_subscriptions.clear();
        match self.content.clone() {
            TabContent::NewTab(new_tab) => {
                self.content_subscriptions.push(cx.subscribe_in(
                    &new_tab,
                    window,
                    |_, _, event: &NewTabEvent, _, cx| match event {
                        NewTabEvent::OpenFile => cx.emit(TabItemEvent::OpenFile),
                        NewTabEvent::OpenDocument(path) => {
                            cx.emit(TabItemEvent::OpenDocument {
                                path: path.clone(),
                                position: None,
                                replace_active_new_tab: true,
                            });
                        }
                    },
                ));
            }
            TabContent::LoadingDocument(loading) => {
                self.content_subscriptions.push(cx.subscribe_in(
                    &loading,
                    window,
                    |this, _, event: &LoadingDocumentEvent, window, cx| match event {
                        LoadingDocumentEvent::SubmitPassword { password, remember } => {
                            this.start_document_prepare(
                                DocumentOpenAttempt::Entered {
                                    password: password.clone(),
                                    remember: *remember,
                                },
                                None,
                                window,
                                cx,
                            );
                        }
                        LoadingDocumentEvent::Cancel => cx.emit(TabItemEvent::CloseRequested),
                    },
                ));
                self.content_subscriptions
                    .push(cx.observe(&loading, |_, _, cx| cx.notify()));
            }
            TabContent::Document(document_view) => {
                self.content_subscriptions.push(cx.subscribe_in(
                    &document_view,
                    window,
                    |_, _, event: &DocumentViewEvent, _, cx| match event {
                        DocumentViewEvent::OpenDocument { path, position } => {
                            cx.emit(TabItemEvent::OpenDocument {
                                path: path.clone(),
                                position: position.clone(),
                                replace_active_new_tab: false,
                            });
                        }
                        DocumentViewEvent::OpenSettings => cx.emit(TabItemEvent::OpenSettings),
                        DocumentViewEvent::SidebarNavigationRequested => {
                            cx.emit(TabItemEvent::SidebarNavigationRequested)
                        }
                        DocumentViewEvent::SynchronizedScroll(delta) => {
                            cx.emit(TabItemEvent::DocumentScrolled(*delta));
                        }
                        DocumentViewEvent::TranslationRegionHovered { .. }
                        | DocumentViewEvent::TranslationOverlayVisibilityChanged { .. }
                        | DocumentViewEvent::TranslationViewportChanged { .. } => {}
                    },
                ));
                self.content_subscriptions
                    .push(cx.observe(&document_view, |_, _, cx| cx.notify()));
            }
            TabContent::Keymap(_) => {}
        }
    }

    fn loading(&self) -> Option<Entity<LoadingDocumentView>> {
        match &self.content {
            TabContent::LoadingDocument(loading) => Some(loading.clone()),
            _ => None,
        }
    }

    fn start_document_prepare(
        &mut self,
        attempt: DocumentOpenAttempt,
        minimum_loading_duration: Option<Duration>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(loading) = self.loading() else {
            return;
        };
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
                this.finish_document_open(attempt, result, window, cx);
            });
        });
        loading.update(cx, |loading, _| loading._task = Some(task));
    }

    fn finish_document_open(
        &mut self,
        attempt: DocumentOpenAttempt,
        result: std::result::Result<PreparedDocumentOpen, DocumentOpenFailure>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(loading) = self.loading() else {
            return;
        };

        let prepared = match result {
            Ok(prepared) => prepared,
            Err(DocumentOpenFailure::Authentication) => {
                match attempt {
                    DocumentOpenAttempt::Initial => self.start_password_lookup(window, cx),
                    DocumentOpenAttempt::Saved(_) => self.discard_saved_password_and_prompt(
                        crate::fl!("pdf-password-saved-invalid").into(),
                        window,
                        cx,
                    ),
                    DocumentOpenAttempt::Shared(_) | DocumentOpenAttempt::Entered { .. } => self
                        .show_password_prompt(
                            Some(crate::fl!("pdf-password-invalid").into()),
                            None,
                            window,
                            cx,
                        ),
                }
                return;
            }
            Err(DocumentOpenFailure::Other(error)) => {
                self.fail_document_open(error, cx);
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
        if !self.complete_document_open(prepared, window, cx) {
            return;
        }
        if let Some(password) = authenticated_password {
            cx.emit(TabItemEvent::DocumentPasswordAuthenticated {
                path: path.clone(),
                password,
            });
        }
        if let Some(password) = save_password {
            self.save_document_password(path, password, window, cx);
        }
    }

    fn start_password_lookup(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(loading) = self.loading() else {
            return;
        };
        let path = loading.read(cx).path.clone();
        loading.update(cx, |loading, cx| {
            loading.status = LoadingDocumentStatus::CheckingSavedPassword;
            cx.notify();
        });
        let store = PdfPasswordStore::ensure(cx);
        let lookup = store.read(&path, cx);
        let task = cx.spawn_in(window, async move |this, cx| {
            let result = lookup.await;
            _ = this.update_in(cx, |this, window, cx| match result {
                Ok(StoredPdfPassword::Found(password)) => this.start_document_prepare(
                    DocumentOpenAttempt::Saved(password),
                    None,
                    window,
                    cx,
                ),
                Ok(StoredPdfPassword::Missing) => this.show_password_prompt(None, None, window, cx),
                Ok(StoredPdfPassword::Corrupt) => this.discard_saved_password_and_prompt(
                    crate::fl!("pdf-password-saved-corrupt").into(),
                    window,
                    cx,
                ),
                Err(error) => {
                    eprintln!("failed to read a saved PDF password: {error:#}");
                    this.show_password_prompt(
                        None,
                        Some(crate::fl!("pdf-password-keychain-read-failed").into()),
                        window,
                        cx,
                    );
                }
            });
        });
        loading.update(cx, |loading, _| loading._task = Some(task));
    }

    fn discard_saved_password_and_prompt(
        &mut self,
        error: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(loading) = self.loading() else {
            return;
        };
        let path = loading.read(cx).path.clone();
        loading.update(cx, |loading, cx| {
            loading.status = LoadingDocumentStatus::CheckingSavedPassword;
            cx.notify();
        });
        let store = PdfPasswordStore::ensure(cx);
        let deletion = store.delete(&path, cx);
        let task = cx.spawn_in(window, async move |this, cx| {
            let result = deletion.await;
            _ = this.update_in(cx, |this, window, cx| {
                let warning = result.err().map(|delete_error| {
                    eprintln!("failed to remove a stale PDF password: {delete_error:#}");
                    SharedString::from(crate::fl!("pdf-password-keychain-delete-failed"))
                });
                this.show_password_prompt(Some(error), warning, window, cx);
            });
        });
        loading.update(cx, |loading, _| loading._task = Some(task));
    }

    fn show_password_prompt(
        &mut self,
        error: Option<SharedString>,
        keychain_warning: Option<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(loading) = self.loading() else {
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
        // A displayed tab hands its focus to the field; one in the background
        // gets it when it is next shown (see `focus`).
        if self.focus_handle.contains_focused(window, cx) || self.focus_handle.is_focused(window) {
            input.read(cx).focus_handle(cx).focus(window, cx);
        }
    }

    fn fail_document_open(&mut self, error: String, cx: &mut Context<Self>) {
        let Some(loading) = self.loading() else {
            return;
        };
        let (job_ids, restore_tab_id) = {
            let loading = loading.read(cx);
            (loading.job_ids.clone(), loading.restore_tab_id)
        };
        for job_id in job_ids.iter().copied() {
            cx.emit(TabItemEvent::DocumentOpenFinished {
                job_id,
                result: Err(error.clone().into()),
            });
        }
        loading.update(cx, |loading, cx| {
            loading.status = LoadingDocumentStatus::Error(error.into());
            loading.restore_tab_id = None;
            loading.job_ids.clear();
            loading.pending_open = None;
            loading._task = None;
            cx.notify();
        });
        if let Some(session_tab_id) = restore_tab_id {
            cx.emit(TabItemEvent::RestoredDocumentFinished {
                session_tab_id,
                opened: false,
            });
        }
    }

    fn complete_document_open(
        &mut self,
        prepared: PreparedDocumentOpen,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(loading) = self.loading() else {
            return false;
        };
        let (path, initial_position, job_ids, restore_tab_id, pending) =
            loading.update(cx, |loading, _| {
                loading._task = None;
                (
                    loading.path.clone(),
                    loading.initial_position.clone(),
                    std::mem::take(&mut loading.job_ids),
                    loading.restore_tab_id.take(),
                    loading.pending_open.take(),
                )
            });
        let Some(pending) = pending else {
            return false;
        };
        let document_view = match DocumentView::from_prepared(
            prepared,
            self.focus_handle.clone(),
            pending.behavior,
            pending.options,
        ) {
            Ok(document_view) => document_view,
            Err(error) => {
                // Put the bookkeeping back so the failure reports to it.
                loading.update(cx, |loading, _| {
                    loading.job_ids = job_ids;
                    loading.restore_tab_id = restore_tab_id;
                });
                self.fail_document_open(format!("{error:#}"), cx);
                return false;
            }
        };
        let had_focus =
            self.focus_handle.contains_focused(window, cx) || self.focus_handle.is_focused(window);
        let document_view = cx.new(|cx| {
            let mut document_view = document_view;
            document_view.register_cache_eviction(cx);
            document_view.initialize_runtime(window, cx);
            document_view
        });
        self.content = TabContent::Document(document_view);
        self.subscribe_to_content(window, cx);
        if had_focus {
            self.focus_handle.focus(window, cx);
        }

        let library = cx.global_mut::<LibraryStore>();
        library.record_history(&path, initial_position);
        if let Err(error) = library.mark_library_item_opened(&path) {
            eprintln!("failed to update library reading status: {error:#}");
        }
        for job_id in job_ids {
            cx.emit(TabItemEvent::DocumentOpenFinished {
                job_id,
                result: Ok(()),
            });
        }
        if let Some(session_tab_id) = restore_tab_id {
            cx.emit(TabItemEvent::RestoredDocumentFinished {
                session_tab_id,
                opened: true,
            });
        }
        cx.emit(TabItemEvent::ContentChanged);
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

    /// Tell whoever waits on this tab's open that it will not happen. A
    /// loading tab that is closed reports here.
    fn cancel_pending_open(&mut self, cx: &mut Context<Self>) {
        let Some(loading) = self.loading() else {
            return;
        };
        let (job_ids, restore_tab_id) = loading.update(cx, |loading, _| {
            loading.pending_open = None;
            loading._task = None;
            (
                std::mem::take(&mut loading.job_ids),
                loading.restore_tab_id.take(),
            )
        });
        for job_id in job_ids {
            cx.emit(TabItemEvent::DocumentOpenFinished {
                job_id,
                result: Err("Opening the translated document was cancelled.".into()),
            });
        }
        if let Some(session_tab_id) = restore_tab_id {
            cx.emit(TabItemEvent::RestoredDocumentFinished {
                session_tab_id,
                opened: false,
            });
        }
    }

    /// The tab is leaving the workspace for good.
    pub(crate) fn close(&mut self, cx: &mut Context<Self>) {
        self.flush(cx);
        self.cancel_pending_open(cx);
    }
}

impl Focusable for TabItem {
    fn focus_handle(&self, _: &App) -> gpui::FocusHandle {
        self.focus_handle.clone()
    }
}

impl BasePanel for TabItem {
    fn panel_name(&self) -> &'static str {
        "ReaderTab"
    }

    fn zoomable(&self, _: &App) -> bool {
        false
    }
}

impl Render for TabItem {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        match &self.content {
            TabContent::Document(document_view) => document_view.clone().into_any_element(),
            TabContent::NewTab(new_tab) => new_tab.clone().into_any_element(),
            TabContent::Keymap(editor) => editor.clone().into_any_element(),
            TabContent::LoadingDocument(loading) => div()
                .size_full()
                .track_focus(&self.focus_handle)
                .child(loading.clone())
                .into_any_element(),
        }
    }
}

/// Focus a tab: its password field while one is asked for, the keymap's
/// filter, otherwise the tab's own focus.
pub(crate) fn focus_tab(item: &Entity<TabItem>, window: &mut Window, cx: &mut App) {
    let tab = item.read(cx);
    let focus_handle = tab.focus_handle.clone();
    match tab.content.clone() {
        TabContent::LoadingDocument(loading) => {
            let loading = loading.read(cx);
            if matches!(
                loading.status,
                LoadingDocumentStatus::PasswordRequired { .. }
            ) {
                let input = loading.password_input.clone();
                input.read(cx).focus_handle(cx).focus(window, cx);
                return;
            }
        }
        TabContent::Keymap(editor) => {
            editor.update(cx, |editor, cx| editor.focus_filter(window, cx));
            return;
        }
        TabContent::Document(_) | TabContent::NewTab(_) => {}
    }
    focus_handle.focus(window, cx);
}

/// [`TabItem::flush`] for a tab handle.
pub(crate) fn flush_tab(item: &Entity<TabItem>, cx: &mut App) {
    item.update(cx, |item, cx| item.flush(cx));
}

#[cfg(test)]
mod tests;
