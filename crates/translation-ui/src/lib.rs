pub use glyphst_pdf_translation_service::{
    PdfTranslationSessionCacheKey, PdfTranslationSessionRequest,
};
pub use glyphst_translation as service;

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures::{StreamExt as _, channel::mpsc};
use glyphst_pdf::DocumentSnapshot;
use glyphst_pdf_translation_service::{
    CancellationToken, DocumentMetadata, DocumentV1, Page, PdfTranslationConfig,
    PdfTranslationError, PdfTranslationExportRequest, PdfTranslationPageFailure,
    PdfTranslationPagePreview, PdfTranslationProgress, PdfTranslationRequest,
    PdfTranslationRestoredSession, PdfTranslationResult, PdfTranslationService,
    PdfTranslationSessionResult, PdfTranslationSessionUpdate, PdfTranslationStage,
    PdfTranslationWarning, TranslatedUnit, TranslationFailureDiagnostic, TranslationFailurePhase,
    TranslationRunProgress,
};
use glyphst_settings::{
    DocumentTranslationMode, Settings, SettingsStore, TranslationTargetLanguage,
};
use glyphst_translation::{LanguageCode, TARGET_LANGUAGES, TranslationService};
use glyphst_ui::activity::{
    ActivityCenter, ActivityNoticeEntry, ActivityNoticeId, ActivityNoticeLevel,
    BackgroundTaskEntry, BackgroundTaskId, CenterAction, TaskProgress,
};
use gpui::{
    App, ClipboardItem, Context, EventEmitter, IntoElement, ParentElement as _, SharedString,
    Styled, Task, WeakEntity, div, prelude::*,
};
use gpui_component::{
    ActiveTheme as _, Sizable as _, Size,
    button::{Button, ButtonVariants as _},
    notification::{Notification, NotificationType},
    searchable_list::SearchableListItem,
    v_flex,
};

/// Application-owned translation services. Domain crates remain independent of
/// GPUI; only this UI composition root participates in the global registry.
#[derive(Clone)]
pub struct TranslationServices {
    text: TranslationService,
    pdf: PdfTranslationService,
}

impl TranslationServices {
    pub fn new(text: TranslationService) -> Self {
        Self {
            pdf: PdfTranslationService::new(text.clone()),
            text,
        }
    }

    pub fn text(&self) -> &TranslationService {
        &self.text
    }

    pub fn pdf(&self) -> &PdfTranslationService {
        &self.pdf
    }
}

impl gpui::Global for TranslationServices {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DocumentTranslationJobAction {
    Retry,
    Open,
    CopyError,
}

impl DocumentTranslationJobAction {
    fn id(self) -> &'static str {
        match self {
            Self::Retry => "retry",
            Self::Open => "open",
            Self::CopyError => "copy-error",
        }
    }

    fn label(
        self,
        open_mode: DocumentTranslationOpenMode,
        open_error: bool,
        has_warnings: bool,
    ) -> &'static str {
        match self {
            Self::Retry => "Retry",
            Self::Open if open_error => "Open again",
            Self::Open => match open_mode {
                DocumentTranslationOpenMode::Document => "Open output",
                DocumentTranslationOpenMode::Split => "Open in synchronized tabs",
            },
            Self::CopyError if has_warnings && !open_error => "Copy warning",
            Self::CopyError => "Copy error",
        }
    }
}

pub type DocumentTranslationId = u64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DocumentTranslationOpenMode {
    Document,
    Split,
}

#[derive(Clone, Debug, PartialEq)]
pub enum DocumentTranslationJobStatus {
    Queued,
    Running {
        progress: Option<PdfTranslationProgress>,
    },
    Cancelling {
        progress: Option<PdfTranslationProgress>,
    },
    Cancelled,
    Failed {
        stage: PdfTranslationStage,
        message: SharedString,
        detailed_message: SharedString,
        retryable: bool,
    },
    Succeeded {
        output_path: PathBuf,
        open_error: Option<SharedString>,
        warnings: Vec<PdfTranslationWarning>,
    },
    Opening {
        output_path: PathBuf,
        warnings: Vec<PdfTranslationWarning>,
    },
}

fn job_actions(status: &DocumentTranslationJobStatus) -> Vec<DocumentTranslationJobAction> {
    match status {
        DocumentTranslationJobStatus::Queued
        | DocumentTranslationJobStatus::Running { .. }
        | DocumentTranslationJobStatus::Cancelling { .. }
        | DocumentTranslationJobStatus::Opening { .. }
        | DocumentTranslationJobStatus::Cancelled => Vec::new(),
        DocumentTranslationJobStatus::Failed { retryable, .. } => {
            let mut actions = Vec::with_capacity(2);
            if *retryable {
                actions.push(DocumentTranslationJobAction::Retry);
            }
            actions.push(DocumentTranslationJobAction::CopyError);
            actions
        }
        DocumentTranslationJobStatus::Succeeded { open_error, .. } => {
            let mut actions = vec![DocumentTranslationJobAction::Open];
            if matches!(status, DocumentTranslationJobStatus::Succeeded { warnings, .. } if !warnings.is_empty())
            {
                actions.push(DocumentTranslationJobAction::Retry);
            }
            if open_error.is_some()
                || matches!(status, DocumentTranslationJobStatus::Succeeded { warnings, .. } if !warnings.is_empty())
            {
                actions.push(DocumentTranslationJobAction::CopyError);
            }
            actions
        }
    }
}

#[derive(Clone, Debug)]
pub struct DocumentTranslationJobSnapshot {
    pub id: DocumentTranslationId,
    pub request: PdfTranslationRequest,
    pub open_mode: DocumentTranslationOpenMode,
    pub status: DocumentTranslationJobStatus,
}

/// A document translation that can be rendered before an output PDF exists.
///
/// Session snapshots are intentionally cheap to clone. Large pipeline values are
/// shared so document views can read a coherent update without touching the job
/// manager while it is processing a worker message.
#[derive(Clone, Debug)]
pub struct DocumentTranslationSessionSnapshot {
    pub id: DocumentTranslationId,
    pub request: PdfTranslationSessionRequest,
    pub mode: DocumentTranslationMode,
    pub status: DocumentTranslationSessionStatus,
    pub prepared_document: Option<Arc<DocumentV1>>,
    pub resolved_units: Arc<Vec<TranslatedUnit>>,
    pub failures: Arc<Vec<TranslationFailureDiagnostic>>,
    pub page_previews: Arc<BTreeMap<usize, Arc<PdfTranslationPagePreview>>>,
    pub page_failures: Arc<Vec<PdfTranslationPageFailure>>,
    pub ready_prefix: usize,
    pub original_unit_ids: Arc<HashSet<String>>,
    pub cache_key: Option<PdfTranslationSessionCacheKey>,
    pub attached_views: usize,
    pub last_export_path: Option<PathBuf>,
    pub last_export_error: Option<SharedString>,
}

impl DocumentTranslationSessionSnapshot {
    pub fn title(&self) -> String {
        self.request
            .source_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("Document")
            .to_string()
    }

    pub fn is_translating(&self) -> bool {
        matches!(
            self.status,
            DocumentTranslationSessionStatus::Queued
                | DocumentTranslationSessionStatus::Running { .. }
                | DocumentTranslationSessionStatus::Cancelling { .. }
        )
    }

    pub fn can_save(&self) -> bool {
        matches!(
            self.status,
            DocumentTranslationSessionStatus::Completed { .. }
        )
    }

    pub fn can_resume(&self) -> bool {
        matches!(self.status, DocumentTranslationSessionStatus::Paused { .. })
    }

    pub fn has_failures(&self) -> bool {
        self.failure_count() > 0
    }

    fn failure_count(&self) -> usize {
        // OCR fallbacks are represented both as a stable diagnostic (for the
        // artifact manifest) and as a page failure (for page-aware UI). Count
        // that paired representation once while retaining independent layout
        // and preview page failures.
        self.failures.len()
            + self
                .page_failures
                .iter()
                .filter(|failure| failure.phase != TranslationFailurePhase::Ocr)
                .count()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum DocumentTranslationSessionStatus {
    Queued,
    Running {
        progress: Option<PdfTranslationProgress>,
        units: TranslationRunProgress,
    },
    Cancelling {
        progress: Option<PdfTranslationProgress>,
    },
    Paused {
        progress: Option<PdfTranslationProgress>,
        units: TranslationRunProgress,
        cache_available: bool,
    },
    Cancelled,
    Completed {
        failed_units: usize,
    },
    Exporting {
        output_path: PathBuf,
        progress: Option<PdfTranslationProgress>,
    },
    Failed {
        stage: PdfTranslationStage,
        message: SharedString,
        detailed_message: SharedString,
        retryable: bool,
    },
}

impl DocumentTranslationJobSnapshot {
    pub fn title(&self) -> String {
        self.request
            .source_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("Document")
            .to_string()
    }

    pub fn is_active(&self) -> bool {
        matches!(
            self.status,
            DocumentTranslationJobStatus::Queued
                | DocumentTranslationJobStatus::Running { .. }
                | DocumentTranslationJobStatus::Cancelling { .. }
                | DocumentTranslationJobStatus::Opening { .. }
        )
    }
}

#[derive(Clone, Debug)]
pub enum DocumentTranslationJobEvent {
    OpenDocument {
        id: DocumentTranslationId,
        path: PathBuf,
    },
    OpenSplit {
        id: DocumentTranslationId,
        source_path: PathBuf,
        translated_path: PathBuf,
    },
    ShowNotification {
        snapshot: DocumentTranslationJobSnapshot,
    },
    RemoveNotification {
        id: DocumentTranslationId,
    },
    OpenSession {
        id: DocumentTranslationId,
        source_path: PathBuf,
    },
    SaveSession {
        id: DocumentTranslationId,
    },
    OpenSessionExport {
        path: PathBuf,
    },
    ShowSessionNotification {
        snapshot: DocumentTranslationSessionSnapshot,
    },
    RemoveSessionNotification {
        id: DocumentTranslationId,
    },
}

/// Stable notification kind for terminal translation results.
pub struct DocumentTranslationNotification;

/// Stable notification kind for terminal in-memory translation sessions.
pub struct DocumentTranslationSessionNotification;

#[derive(Clone)]
pub struct BatchTranslationSubmission {
    pub settings: Settings,
    pub request: PdfTranslationRequest,
    pub open_mode: DocumentTranslationOpenMode,
}

#[derive(Clone)]
pub struct LiveTranslationSubmission {
    pub settings: Settings,
    pub request: PdfTranslationSessionRequest,
    pub source_snapshot: Arc<DocumentSnapshot>,
    pub mode: DocumentTranslationMode,
}

#[derive(Clone)]
pub struct RestoredTranslationSubmission {
    pub settings: Settings,
    pub request: PdfTranslationSessionRequest,
    pub source_snapshot: Arc<DocumentSnapshot>,
    pub mode: DocumentTranslationMode,
    pub cache_key: Option<PdfTranslationSessionCacheKey>,
    pub original_unit_ids: HashSet<String>,
}

struct DocumentTranslationJobRecord {
    snapshot: DocumentTranslationJobSnapshot,
    settings: Settings,
    cancellation: CancellationToken,
    task: Option<Task<()>>,
}

struct DocumentTranslationSessionRecord {
    snapshot: DocumentTranslationSessionSnapshot,
    source_snapshot: Arc<DocumentSnapshot>,
    settings: Settings,
    cancellation: CancellationToken,
    task: Option<Task<()>>,
    result: Option<Arc<PdfTranslationSessionResult>>,
    pending_restart: Option<LanguageCode>,
    pending_export: Option<PathBuf>,
    notify_when_finished: bool,
    prepared_metadata: Option<DocumentMetadata>,
    prepared_pages: BTreeMap<usize, Page>,
    prepared_page_count: usize,
    preview_generation: u64,
    pending_cache_restore: bool,
}

impl DocumentTranslationSessionRecord {
    fn should_publish_completion_notification(&self) -> bool {
        self.notify_when_finished || self.snapshot.attached_views == 0
    }
}

enum DocumentTranslationSessionWorkerUpdate {
    Progress(PdfTranslationProgress),
    Pipeline(PdfTranslationSessionUpdate),
}

#[derive(Default)]
struct BatchTranslationState {
    records: VecDeque<DocumentTranslationJobRecord>,
    active_id: Option<DocumentTranslationId>,
    background_tasks: HashMap<DocumentTranslationId, BackgroundTaskId>,
    activity_notices: HashMap<DocumentTranslationId, ActivityNoticeId>,
}

#[derive(Default)]
struct LiveTranslationState {
    records: VecDeque<DocumentTranslationSessionRecord>,
    active_id: Option<DocumentTranslationId>,
    background_tasks: HashMap<DocumentTranslationId, BackgroundTaskId>,
    activity_notices: HashMap<DocumentTranslationId, ActivityNoticeId>,
}

pub struct DocumentTranslationManager {
    batch: BatchTranslationState,
    live: LiveTranslationState,
    next_id: DocumentTranslationId,
    credential_wait_task: Option<Task<()>>,
}

impl EventEmitter<DocumentTranslationJobEvent> for DocumentTranslationManager {}

impl DocumentTranslationManager {
    pub fn new(cx: &mut Context<Self>) -> Self {
        cx.on_release(|this, cx| {
            for job in &this.batch.records {
                job.cancellation.cancel();
            }
            for session in &this.live.records {
                session.cancellation.cancel();
            }
            if let Some(center) = ActivityCenter::try_global(cx) {
                for task_id in this.batch.background_tasks.values().copied() {
                    center.update(cx, |center, cx| center.remove_task(task_id, cx));
                }
                for task_id in this.live.background_tasks.values().copied() {
                    center.update(cx, |center, cx| center.remove_task(task_id, cx));
                }
            }
        })
        .detach();
        Self {
            batch: BatchTranslationState::default(),
            live: LiveTranslationState::default(),
            next_id: 1,
            credential_wait_task: None,
        }
    }

    fn wait_for_provider_credentials(&mut self, cx: &mut Context<Self>) {
        if self.credential_wait_task.is_some() {
            return;
        }
        self.credential_wait_task = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(50))
                    .await;
                let still_loading = cx.update(|cx| {
                    cx.has_global::<SettingsStore>()
                        && cx
                            .global::<SettingsStore>()
                            .document_translation_credentials_loading()
                });
                if !still_loading {
                    break;
                }
            }
            let _ = this.update(cx, |this, cx| {
                this.credential_wait_task = None;
                this.start_next(cx);
                this.start_next_session(cx);
            });
        }));
    }

    fn refresh_provider_secrets(settings: &mut Settings, cx: &App) {
        if cx.has_global::<SettingsStore>() {
            settings.copy_provider_secrets_from(&cx.global::<SettingsStore>().settings);
        }
    }

    pub fn submit(
        &mut self,
        submission: BatchTranslationSubmission,
        cx: &mut Context<Self>,
    ) -> DocumentTranslationId {
        let BatchTranslationSubmission {
            settings,
            request,
            open_mode,
        } = submission;
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        self.batch.records.push_back(DocumentTranslationJobRecord {
            snapshot: DocumentTranslationJobSnapshot {
                id,
                request,
                open_mode,
                status: DocumentTranslationJobStatus::Queued,
            },
            settings,
            cancellation: CancellationToken::new(),
            task: None,
        });
        self.register_background_task(id, cx);
        self.start_next(cx);
        cx.notify();
        id
    }

    /// Start a translation whose primary result remains in memory until the
    /// user explicitly saves it.
    pub fn submit_session(
        &mut self,
        submission: LiveTranslationSubmission,
        cx: &mut Context<Self>,
    ) -> DocumentTranslationId {
        let LiveTranslationSubmission {
            settings,
            request,
            source_snapshot,
            mode,
        } = submission;
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        let foreground = mode != DocumentTranslationMode::Background;
        let source_path = request.source_path.clone();
        self.live
            .records
            .push_back(DocumentTranslationSessionRecord {
                snapshot: DocumentTranslationSessionSnapshot {
                    id,
                    request,
                    mode,
                    status: DocumentTranslationSessionStatus::Queued,
                    prepared_document: None,
                    resolved_units: Arc::new(Vec::new()),
                    failures: Arc::new(Vec::new()),
                    page_previews: Arc::new(BTreeMap::new()),
                    page_failures: Arc::new(Vec::new()),
                    ready_prefix: 0,
                    original_unit_ids: Arc::new(HashSet::new()),
                    cache_key: None,
                    attached_views: usize::from(foreground),
                    last_export_path: None,
                    last_export_error: None,
                },
                source_snapshot,
                settings,
                cancellation: CancellationToken::new(),
                task: None,
                result: None,
                pending_restart: None,
                pending_export: None,
                notify_when_finished: !foreground,
                prepared_metadata: None,
                prepared_pages: BTreeMap::new(),
                prepared_page_count: 0,
                preview_generation: 0,
                pending_cache_restore: false,
            });
        self.register_session_background_task(id, cx);
        self.start_next_session(cx);
        if foreground {
            cx.emit(DocumentTranslationJobEvent::OpenSession { id, source_path });
        }
        cx.notify();
        id
    }

    /// Recreate a reader-owned live session without opening or splitting any
    /// panes. Cached pages are restored in the worker queue and the session is
    /// left paused unless its durable manifest says it had completed.
    pub fn restore_session(
        &mut self,
        submission: RestoredTranslationSubmission,
        cx: &mut Context<Self>,
    ) -> DocumentTranslationId {
        let RestoredTranslationSubmission {
            settings,
            request,
            source_snapshot,
            mode,
            cache_key,
            original_unit_ids,
        } = submission;
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        self.live
            .records
            .push_back(DocumentTranslationSessionRecord {
                snapshot: DocumentTranslationSessionSnapshot {
                    id,
                    request,
                    mode,
                    status: DocumentTranslationSessionStatus::Queued,
                    prepared_document: None,
                    resolved_units: Arc::new(Vec::new()),
                    failures: Arc::new(Vec::new()),
                    page_previews: Arc::new(BTreeMap::new()),
                    page_failures: Arc::new(Vec::new()),
                    ready_prefix: 0,
                    original_unit_ids: Arc::new(original_unit_ids),
                    cache_key,
                    attached_views: 1,
                    last_export_path: None,
                    last_export_error: None,
                },
                source_snapshot,
                settings,
                cancellation: CancellationToken::new(),
                task: None,
                result: None,
                pending_restart: None,
                pending_export: None,
                notify_when_finished: false,
                prepared_metadata: None,
                prepared_pages: BTreeMap::new(),
                prepared_page_count: 0,
                preview_generation: 0,
                pending_cache_restore: true,
            });
        self.register_session_background_task(id, cx);
        self.start_next_session(cx);
        cx.notify();
        id
    }

    pub fn has_active_session_for_source(&self, source_path: &std::path::Path) -> bool {
        self.live.records.iter().any(|session| {
            session.snapshot.request.source_path == source_path && session.snapshot.is_translating()
        })
    }

    pub fn session_snapshot(
        &self,
        id: DocumentTranslationId,
    ) -> Option<DocumentTranslationSessionSnapshot> {
        self.session(id).map(|session| session.snapshot.clone())
    }

    pub fn session_result(
        &self,
        id: DocumentTranslationId,
    ) -> Option<Arc<PdfTranslationSessionResult>> {
        self.session(id).and_then(|session| session.result.clone())
    }

    pub fn request_open_session(&mut self, id: DocumentTranslationId, cx: &mut Context<Self>) {
        let Some(source_path) = self
            .session(id)
            .map(|session| session.snapshot.request.source_path.clone())
        else {
            return;
        };
        if let Some(session) = self.session_mut(id) {
            session.snapshot.attached_views = session.snapshot.attached_views.saturating_add(1);
            session.notify_when_finished = false;
        }
        self.remove_session_activity_notice(id, cx);
        cx.emit(DocumentTranslationJobEvent::RemoveSessionNotification { id });
        cx.emit(DocumentTranslationJobEvent::OpenSession { id, source_path });
        cx.notify();
    }

    pub fn request_save_session(&self, id: DocumentTranslationId, cx: &mut Context<Self>) {
        if self
            .session(id)
            .is_some_and(|session| session.result.is_some())
        {
            cx.emit(DocumentTranslationJobEvent::SaveSession { id });
        }
    }

    pub fn request_open_session_export(&self, id: DocumentTranslationId, cx: &mut Context<Self>) {
        if let Some(path) = self
            .session(id)
            .and_then(|session| session.snapshot.last_export_path.clone())
        {
            cx.emit(DocumentTranslationJobEvent::OpenSessionExport { path });
        }
    }

    pub fn set_session_backgrounded(&mut self, id: DocumentTranslationId, cx: &mut Context<Self>) {
        let Some(session) = self.session_mut(id) else {
            return;
        };
        session.snapshot.attached_views = session.snapshot.attached_views.saturating_sub(1);
        session.notify_when_finished = true;
        self.sync_session_background_task(id, cx);
        cx.notify();
    }

    pub fn cancel_session(&mut self, id: DocumentTranslationId, cx: &mut Context<Self>) {
        let mut remove_queued = false;
        let mut settle_without_worker = false;
        let is_active = self.live.active_id == Some(id);
        {
            let Some(session) = self.session_mut(id) else {
                return;
            };
            match session.snapshot.status.clone() {
                DocumentTranslationSessionStatus::Queued => {
                    if session.snapshot.attached_views == 0 {
                        remove_queued = true;
                    } else {
                        session.snapshot.status = DocumentTranslationSessionStatus::Cancelled;
                        settle_without_worker = true;
                    }
                }
                DocumentTranslationSessionStatus::Running { progress, .. } => {
                    session.cancellation.cancel();
                    session.snapshot.status =
                        DocumentTranslationSessionStatus::Cancelling { progress };
                }
                DocumentTranslationSessionStatus::Exporting { .. } => {
                    if is_active {
                        session.cancellation.cancel();
                    } else {
                        session.pending_export = None;
                        session.snapshot.status = DocumentTranslationSessionStatus::Completed {
                            failed_units: session.snapshot.failure_count(),
                        };
                        settle_without_worker = true;
                    }
                }
                DocumentTranslationSessionStatus::Cancelling { .. }
                | DocumentTranslationSessionStatus::Paused { .. }
                | DocumentTranslationSessionStatus::Cancelled
                | DocumentTranslationSessionStatus::Completed { .. }
                | DocumentTranslationSessionStatus::Failed { .. } => {}
            }
        }
        if remove_queued {
            self.live
                .records
                .retain(|session| session.snapshot.id != id);
            self.remove_session_background_task(id, cx);
            self.start_next_session(cx);
        } else if settle_without_worker {
            self.remove_session_background_task(id, cx);
            self.start_next_session(cx);
        } else {
            self.sync_session_background_task(id, cx);
        }
        cx.notify();
    }

    pub fn discard_session(&mut self, id: DocumentTranslationId, cx: &mut Context<Self>) {
        if let Some(session) = self.session(id) {
            session.cancellation.cancel();
        }
        if self.live.active_id == Some(id) {
            if let Some(session) = self.session_mut(id) {
                session.snapshot.attached_views = 0;
                session.notify_when_finished = false;
                session.snapshot.status =
                    DocumentTranslationSessionStatus::Cancelling { progress: None };
            }
        } else {
            self.live
                .records
                .retain(|session| session.snapshot.id != id);
            self.remove_session_background_task(id, cx);
            self.remove_session_activity_notice(id, cx);
        }
        cx.emit(DocumentTranslationJobEvent::RemoveSessionNotification { id });
        cx.notify();
    }

    pub fn toggle_session_unit_original(
        &mut self,
        id: DocumentTranslationId,
        unit_id: &str,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = self.session_mut(id) else {
            return;
        };
        let original_ids = Arc::make_mut(&mut session.snapshot.original_unit_ids);
        if !original_ids.remove(unit_id) {
            original_ids.insert(unit_id.to_string());
        }
        let unit_id = unit_id.to_string();
        let _ = session;
        self.rebuild_session_original_previews(id, Some(&unit_id), cx);
        cx.notify();
    }

    fn rebuild_session_original_previews(
        &mut self,
        id: DocumentTranslationId,
        changed_unit_id: Option<&str>,
        cx: &mut Context<Self>,
    ) {
        if !cx.has_global::<TranslationServices>() {
            return;
        }
        let Some(session) = self.session_mut(id) else {
            return;
        };
        session.preview_generation = session.preview_generation.wrapping_add(1);
        let generation = session.preview_generation;
        let Some(result) = session.result.clone() else {
            return;
        };
        let original_unit_ids = session
            .snapshot
            .original_unit_ids
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        let selected_units = changed_unit_id.map_or_else(
            || {
                original_unit_ids
                    .iter()
                    .map(String::as_str)
                    .collect::<HashSet<_>>()
            },
            |unit_id| std::iter::once(unit_id).collect(),
        );
        let selected_blocks = result
            .translated
            .units
            .iter()
            .filter(|unit| selected_units.contains(unit.id.as_str()))
            .flat_map(|unit| unit.member_block_ids.iter().map(String::as_str))
            .collect::<HashSet<_>>();
        let mut pages = result
            .translated
            .document
            .pages
            .iter()
            .filter(|page| {
                page.blocks
                    .iter()
                    .any(|block| selected_blocks.contains(block.id.as_str()))
            })
            .map(|page| page.page_index)
            .collect::<Vec<_>>();
        pages.sort_unstable();
        pages.dedup();
        if pages.is_empty() {
            return;
        }
        let revisions = pages
            .iter()
            .map(|page_index| {
                let revision = session
                    .snapshot
                    .page_previews
                    .get(page_index)
                    .map(|preview| preview.revision)
                    .unwrap_or_default()
                    .wrapping_add(1)
                    .max(1);
                (*page_index, revision)
            })
            .collect::<Vec<_>>();
        let service = cx.global::<TranslationServices>().pdf().clone();
        let cancellation = CancellationToken::new();
        cx.spawn(async move |this: WeakEntity<Self>, cx| {
            let rendered = cx
                .background_spawn(async move {
                    revisions
                        .into_iter()
                        .map(|(page_index, revision)| {
                            let preview = service.render_session_page_preview(
                                &result,
                                page_index,
                                revision,
                                &original_unit_ids,
                                &cancellation,
                            );
                            (page_index, preview)
                        })
                        .collect::<Vec<_>>()
                })
                .await;
            _ = this.update(cx, |this, cx| {
                let Some(session) = this.session_mut(id) else {
                    return;
                };
                if session.preview_generation != generation {
                    return;
                }
                let mut previews = (*session.snapshot.page_previews).clone();
                let mut page_failures = (*session.snapshot.page_failures).clone();
                for (page_index, result) in rendered {
                    match result {
                        Ok(preview) => {
                            let layout_failures = preview.layout_failures.clone();
                            previews.insert(page_index, Arc::new(preview));
                            page_failures.retain(|failure| {
                                failure.page_index != page_index
                                    || !matches!(
                                        failure.phase,
                                        TranslationFailurePhase::Layout
                                            | TranslationFailurePhase::Preview
                                    )
                            });
                            if !layout_failures.is_empty() {
                                let mut failures = (*session.snapshot.failures).clone();
                                failures.extend(layout_failures);
                                failures.sort_by(|left, right| left.id.cmp(&right.id));
                                failures.dedup_by(|left, right| {
                                    left.id == right.id && left.phase == right.phase
                                });
                                session.snapshot.failures = Arc::new(failures);
                            }
                        }
                        Err(error) => {
                            page_failures.retain(|failure| {
                                failure.page_index != page_index
                                    || failure.phase != TranslationFailurePhase::Preview
                            });
                            page_failures.push(PdfTranslationPageFailure {
                                page_index,
                                phase: TranslationFailurePhase::Preview,
                                message: error.message,
                                retryable: error.retryable,
                            });
                        }
                    }
                }
                session.snapshot.page_previews = Arc::new(previews);
                session.snapshot.page_failures = Arc::new(page_failures);
                cx.notify();
            });
        })
        .detach();
    }

    /// Retry terminal failed units. The pipeline checkpoint supplies all
    /// successful units, so only unresolved work is sent to the provider.
    pub fn retry_session_failures(&mut self, id: DocumentTranslationId, cx: &mut Context<Self>) {
        let can_retry = self.session(id).is_some_and(|session| {
            session.snapshot.has_failures()
                && !session.snapshot.is_translating()
                && self.live.active_id != Some(id)
        });
        if !can_retry {
            return;
        }
        self.reset_session_for_retry(id, None);
        self.remove_session_activity_notice(id, cx);
        cx.emit(DocumentTranslationJobEvent::RemoveSessionNotification { id });
        self.register_session_background_task(id, cx);
        self.start_next_session(cx);
        cx.notify();
    }

    pub fn resume_session(&mut self, id: DocumentTranslationId, cx: &mut Context<Self>) {
        let can_resume = self.session(id).is_some_and(|session| {
            matches!(
                session.snapshot.status,
                DocumentTranslationSessionStatus::Paused { .. }
            ) && self.live.active_id != Some(id)
        });
        if !can_resume {
            return;
        }
        let original_unit_ids = self
            .session(id)
            .map(|session| session.snapshot.original_unit_ids.clone())
            .unwrap_or_default();
        self.reset_session_for_retry(id, None);
        if let Some(session) = self.session_mut(id) {
            session.snapshot.original_unit_ids = original_unit_ids;
            session.pending_cache_restore = false;
        }
        self.register_session_background_task(id, cx);
        self.start_next_session(cx);
        cx.notify();
    }

    pub fn restart_session_in_language(
        &mut self,
        id: DocumentTranslationId,
        target_language: LanguageCode,
        cx: &mut Context<Self>,
    ) {
        if self.live.active_id == Some(id) {
            let Some(session) = self.session_mut(id) else {
                return;
            };
            session.pending_restart = Some(target_language);
            session.cancellation.cancel();
            let progress = match &session.snapshot.status {
                DocumentTranslationSessionStatus::Running { progress, .. } => progress.clone(),
                _ => None,
            };
            session.snapshot.status = DocumentTranslationSessionStatus::Cancelling { progress };
            self.sync_session_background_task(id, cx);
        } else {
            self.reset_session_for_retry(id, Some(target_language));
            self.register_session_background_task(id, cx);
            self.start_next_session(cx);
        }
        cx.notify();
    }

    pub fn export_session(
        &mut self,
        id: DocumentTranslationId,
        output_path: PathBuf,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = self.session_mut(id) else {
            return;
        };
        if session.result.is_none() || session.snapshot.is_translating() {
            return;
        }
        session.pending_export = Some(output_path.clone());
        session.snapshot.last_export_error = None;
        session.snapshot.status = DocumentTranslationSessionStatus::Exporting {
            output_path,
            progress: None,
        };
        self.register_session_background_task(id, cx);
        self.start_next_session(cx);
        cx.notify();
    }

    fn start_session_export(&mut self, id: DocumentTranslationId, cx: &mut Context<Self>) {
        let Some((settings, result, output_path, original_unit_ids)) =
            self.session_mut(id).and_then(|session| {
                Some((
                    session.settings.clone(),
                    session.result.as_ref()?.as_ref().clone(),
                    session.pending_export.take()?,
                    session
                        .snapshot
                        .original_unit_ids
                        .iter()
                        .cloned()
                        .collect::<Vec<_>>(),
                ))
            })
        else {
            return;
        };
        if !cx.has_global::<TranslationServices>() {
            return;
        }
        let cancellation = CancellationToken::new();
        let service = cx.global::<TranslationServices>().pdf().clone();
        let request = PdfTranslationExportRequest {
            output_path,
            original_unit_ids,
        };
        let config = PdfTranslationConfig::from(&settings);
        if let Some(session) = self.session_mut(id) {
            session.cancellation = cancellation.clone();
        }
        self.live.active_id = Some(id);
        let (progress_tx, mut progress_rx) = mpsc::unbounded::<PdfTranslationProgress>();
        let task = cx.spawn(async move |this: gpui::WeakEntity<Self>, cx| {
            let result_task = cx.background_spawn(async move {
                service.export_session(config, result, request, cancellation, move |progress| {
                    _ = progress_tx.unbounded_send(progress);
                })
            });
            while let Some(progress) = progress_rx.next().await {
                if this
                    .update(cx, |this, cx| {
                        this.update_session_progress(id, progress, cx)
                    })
                    .is_err()
                {
                    return;
                }
            }
            let result = result_task.await;
            _ = this.update(cx, |this, cx| this.finish_session_export(id, result, cx));
        });
        if let Some(session) = self.session_mut(id) {
            session.task = Some(task);
        }
    }

    pub fn report_preflight_error(
        &mut self,
        source_path: PathBuf,
        open_mode: DocumentTranslationOpenMode,
        message: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) -> DocumentTranslationId {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        let message = message.into();
        let target_language =
            glyphst_translation::LanguageCode::new("und").expect("und is a valid language code");
        self.batch.records.push_back(DocumentTranslationJobRecord {
            snapshot: DocumentTranslationJobSnapshot {
                id,
                request: PdfTranslationRequest {
                    output_path: source_path.with_extension("translated.pdf"),
                    source_path,
                    source_language: None,
                    target_language,
                },
                open_mode,
                status: DocumentTranslationJobStatus::Failed {
                    stage: PdfTranslationStage::Preparing,
                    message: message.clone(),
                    detailed_message: message,
                    retryable: false,
                },
            },
            settings: Settings::default(),
            cancellation: CancellationToken::new(),
            task: None,
        });
        self.publish_terminal_notification(id, cx);
        cx.notify();
        id
    }

    pub fn open_cached_output(
        &mut self,
        settings: Settings,
        request: PdfTranslationRequest,
        open_mode: DocumentTranslationOpenMode,
        result: PdfTranslationResult,
        cx: &mut Context<Self>,
    ) -> DocumentTranslationId {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        let has_warnings = !result.warnings.is_empty();
        self.batch.records.push_back(DocumentTranslationJobRecord {
            snapshot: DocumentTranslationJobSnapshot {
                id,
                request,
                open_mode,
                status: DocumentTranslationJobStatus::Succeeded {
                    output_path: result.output_path,
                    open_error: None,
                    warnings: result.warnings,
                },
            },
            settings,
            cancellation: CancellationToken::new(),
            task: None,
        });
        if has_warnings {
            self.publish_terminal_notification(id, cx);
        } else {
            self.open(id, cx);
        }
        id
    }

    pub fn has_job_for_source(&self, source_path: &std::path::Path) -> bool {
        self.batch
            .records
            .iter()
            .any(|job| job.snapshot.request.source_path == source_path && job.snapshot.is_active())
    }

    pub fn cancel_for_source(&mut self, source_path: &std::path::Path, cx: &mut Context<Self>) {
        let Some(id) = self
            .batch
            .records
            .iter()
            .rev()
            .find(|job| job.snapshot.request.source_path == source_path && job.snapshot.is_active())
            .map(|job| job.snapshot.id)
        else {
            return;
        };
        self.cancel(id, cx);
    }

    pub fn snapshots(&self) -> Vec<DocumentTranslationJobSnapshot> {
        self.batch
            .records
            .iter()
            .map(|job| job.snapshot.clone())
            .collect()
    }

    pub fn cancel(&mut self, id: DocumentTranslationId, cx: &mut Context<Self>) {
        let mut remove_job = false;
        {
            let Some(job) = self.job_mut(id) else {
                return;
            };
            match job.snapshot.status.clone() {
                DocumentTranslationJobStatus::Queued => {
                    remove_job = true;
                }
                DocumentTranslationJobStatus::Running { progress } => {
                    job.cancellation.cancel();
                    job.snapshot.status = DocumentTranslationJobStatus::Cancelling { progress };
                }
                DocumentTranslationJobStatus::Cancelling { .. }
                | DocumentTranslationJobStatus::Cancelled
                | DocumentTranslationJobStatus::Failed { .. }
                | DocumentTranslationJobStatus::Succeeded { .. }
                | DocumentTranslationJobStatus::Opening { .. } => {}
            }
        }
        if remove_job {
            self.batch.records.retain(|job| job.snapshot.id != id);
            self.remove_background_task(id, cx);
            self.start_next(cx);
        } else {
            self.sync_background_task(id, cx);
        }
        cx.notify();
    }

    pub fn retry(&mut self, id: DocumentTranslationId, cx: &mut Context<Self>) {
        let latest_settings = cx
            .has_global::<SettingsStore>()
            .then(|| cx.global::<SettingsStore>().settings.clone());
        let latest_languages = latest_settings.as_ref().map(|settings| {
            let source = settings
                .translation
                .source_language
                .as_deref()
                .map(glyphst_translation::LanguageCode::new)
                .transpose()
                .map_err(|error| format!("Invalid source language: {error}"))?;
            let target = glyphst_translation::LanguageCode::new(
                settings.translation.resolved_target_language(),
            )
            .map_err(|error| format!("Invalid target language: {error}"))?;
            Ok::<_, String>((source, target))
        });
        let mut invalid_languages = false;
        {
            let Some(job) = self.job_mut(id) else {
                return;
            };
            let can_retry = matches!(
                &job.snapshot.status,
                DocumentTranslationJobStatus::Failed {
                    retryable: true,
                    ..
                } | DocumentTranslationJobStatus::Cancelled
            ) || matches!(
                &job.snapshot.status,
                DocumentTranslationJobStatus::Succeeded { warnings, .. } if !warnings.is_empty()
            );
            if !can_retry {
                return;
            }
            if let Some(Err(message)) = &latest_languages {
                job.snapshot.status = DocumentTranslationJobStatus::Failed {
                    stage: PdfTranslationStage::Preparing,
                    message: message.clone().into(),
                    detailed_message: message.clone().into(),
                    retryable: false,
                };
                invalid_languages = true;
            } else {
                job.cancellation = CancellationToken::new();
                if let Some(settings) = latest_settings {
                    job.settings = settings;
                }
                if let Some(Ok((source_language, target_language))) = latest_languages {
                    job.snapshot.request.source_language = source_language;
                    job.snapshot.request.target_language = target_language;
                }
                job.snapshot.status = DocumentTranslationJobStatus::Queued;
            }
        }
        if invalid_languages {
            self.publish_terminal_notification(id, cx);
            cx.notify();
            return;
        }
        self.remove_activity_notice(id, cx);
        self.remove_job_notification(id, cx);
        self.register_background_task(id, cx);
        self.start_next(cx);
        cx.notify();
    }

    pub fn open(&mut self, id: DocumentTranslationId, cx: &mut Context<Self>) {
        let Some((open_mode, source_path, output_path, warnings)) = self.job(id).and_then(|job| {
            let DocumentTranslationJobStatus::Succeeded {
                output_path,
                warnings,
                ..
            } = &job.snapshot.status
            else {
                return None;
            };
            Some((
                job.snapshot.open_mode,
                job.snapshot.request.source_path.clone(),
                output_path.clone(),
                warnings.clone(),
            ))
        }) else {
            return;
        };
        self.remove_activity_notice(id, cx);
        if let Some(job) = self.job_mut(id) {
            job.snapshot.status = DocumentTranslationJobStatus::Opening {
                output_path: output_path.clone(),
                warnings,
            };
        }
        self.remove_job_notification(id, cx);
        match open_mode {
            DocumentTranslationOpenMode::Document => {
                cx.emit(DocumentTranslationJobEvent::OpenDocument {
                    id,
                    path: output_path,
                });
            }
            DocumentTranslationOpenMode::Split => {
                cx.emit(DocumentTranslationJobEvent::OpenSplit {
                    id,
                    source_path,
                    translated_path: output_path,
                });
            }
        }
        cx.notify();
    }

    fn copy_error(&self, id: DocumentTranslationId, cx: &mut Context<Self>) {
        let Some(message) = self.job(id).and_then(|job| match &job.snapshot.status {
            DocumentTranslationJobStatus::Failed {
                detailed_message, ..
            } => Some(detailed_message.clone()),
            DocumentTranslationJobStatus::Succeeded {
                open_error: Some(message),
                ..
            } => Some(message.clone()),
            DocumentTranslationJobStatus::Succeeded { warnings, .. } if !warnings.is_empty() => {
                Some(
                    warnings
                        .iter()
                        .map(|warning| {
                            if warning.affected_block_ids.is_empty() {
                                warning.message.clone()
                            } else {
                                format!(
                                    "{}\nAffected blocks: {}",
                                    warning.message,
                                    warning.affected_block_ids.join(", ")
                                )
                            }
                        })
                        .collect::<Vec<_>>()
                        .join("\n\n")
                        .into(),
                )
            }
            _ => None,
        }) else {
            return;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(message.to_string()));
    }

    pub fn finish_open(
        &mut self,
        id: DocumentTranslationId,
        result: Result<(), SharedString>,
        cx: &mut Context<Self>,
    ) {
        let mut publish_error = false;
        match result {
            Ok(()) => {
                self.remove_background_task(id, cx);
                self.batch.records.retain(|job| job.snapshot.id != id);
            }
            Err(message) => {
                let Some(job) = self.job_mut(id) else {
                    return;
                };
                let DocumentTranslationJobStatus::Opening {
                    output_path,
                    warnings,
                } = &job.snapshot.status
                else {
                    return;
                };
                let output_path = output_path.clone();
                let warnings = warnings.clone();
                job.snapshot.status = DocumentTranslationJobStatus::Succeeded {
                    output_path,
                    open_error: Some(message),
                    warnings,
                };
                publish_error = true;
            }
        }
        if publish_error {
            self.publish_terminal_notification(id, cx);
        }
        cx.notify();
    }

    fn start_next(&mut self, cx: &mut Context<Self>) {
        if self.batch.active_id.is_some() {
            return;
        }
        let Some(index) =
            self.batch.records.iter().position(|job| {
                matches!(job.snapshot.status, DocumentTranslationJobStatus::Queued)
            })
        else {
            return;
        };
        if cx.has_global::<SettingsStore>()
            && cx
                .global::<SettingsStore>()
                .document_translation_credentials_loading()
        {
            self.wait_for_provider_credentials(cx);
            return;
        }
        if !cx.has_global::<TranslationServices>() {
            let id = self.batch.records[index].snapshot.id;
            self.batch.records[index].snapshot.status = DocumentTranslationJobStatus::Failed {
                stage: PdfTranslationStage::Preparing,
                message: "Document translation is unavailable.".into(),
                detailed_message: "Document translation is unavailable.".into(),
                retryable: false,
            };
            self.publish_terminal_notification(id, cx);
            return;
        }

        Self::refresh_provider_secrets(&mut self.batch.records[index].settings, cx);
        let id = self.batch.records[index].snapshot.id;
        let settings = self.batch.records[index].settings.clone();
        let config = PdfTranslationConfig::from(&settings);
        let request = self.batch.records[index].snapshot.request.clone();
        let cancellation = self.batch.records[index].cancellation.clone();
        let service = cx.global::<TranslationServices>().pdf().clone();
        let (progress_tx, mut progress_rx) = mpsc::unbounded::<PdfTranslationProgress>();
        self.batch.records[index].snapshot.status =
            DocumentTranslationJobStatus::Running { progress: None };
        self.batch.active_id = Some(id);
        self.sync_background_task(id, cx);

        let task = cx.spawn(async move |this: gpui::WeakEntity<Self>, cx| {
            let result_task = cx.background_spawn(async move {
                service.run(config, request, cancellation, move |progress| {
                    _ = progress_tx.unbounded_send(progress);
                })
            });

            while let Some(progress) = progress_rx.next().await {
                if this
                    .update(cx, |this, cx| this.update_progress(id, progress, cx))
                    .is_err()
                {
                    return;
                }
            }

            let result = result_task.await;
            _ = this.update(cx, |this, cx| this.finish(id, result, cx));
        });
        self.batch.records[index].task = Some(task);
        cx.notify();
    }

    fn update_progress(
        &mut self,
        id: DocumentTranslationId,
        progress: PdfTranslationProgress,
        cx: &mut Context<Self>,
    ) {
        let Some(job) = self.job_mut(id) else {
            return;
        };
        match &mut job.snapshot.status {
            DocumentTranslationJobStatus::Running { progress: current }
            | DocumentTranslationJobStatus::Cancelling { progress: current } => {
                *current = Some(progress)
            }
            _ => return,
        }
        self.sync_background_task(id, cx);
        cx.notify();
    }

    fn finish(
        &mut self,
        id: DocumentTranslationId,
        result: Result<PdfTranslationResult, PdfTranslationError>,
        cx: &mut Context<Self>,
    ) {
        if self.batch.active_id != Some(id) {
            return;
        }
        self.batch.active_id = None;
        let was_cancelled = self.job(id).is_some_and(|job| {
            job.cancellation.is_cancelled()
                || matches!(
                    job.snapshot.status,
                    DocumentTranslationJobStatus::Cancelling { .. }
                )
        });
        if was_cancelled {
            self.remove_background_task(id, cx);
            self.batch.records.retain(|job| job.snapshot.id != id);
            self.start_next(cx);
            cx.notify();
            return;
        }

        {
            let Some(job) = self.job_mut(id) else {
                self.start_next(cx);
                return;
            };
            job.task = None;
            match result {
                Ok(result) => {
                    job.snapshot.status = DocumentTranslationJobStatus::Succeeded {
                        output_path: result.output_path,
                        open_error: None,
                        warnings: result.warnings,
                    };
                }
                Err(error) => {
                    let detailed_message = error
                        .detailed_message
                        .unwrap_or_else(|| error.message.clone());
                    job.snapshot.status = DocumentTranslationJobStatus::Failed {
                        stage: error.stage,
                        message: error.message.into(),
                        detailed_message: detailed_message.into(),
                        retryable: error.retryable,
                    };
                }
            }
        }
        self.publish_terminal_notification(id, cx);
        self.start_next(cx);
        cx.notify();
    }

    fn start_next_session(&mut self, cx: &mut Context<Self>) {
        if self.live.active_id.is_some() {
            return;
        }
        if let Some(id) = self
            .live
            .records
            .iter()
            .find(|session| session.pending_export.is_some())
            .map(|session| session.snapshot.id)
        {
            self.start_session_export(id, cx);
            return;
        }
        let Some(index) = self.live.records.iter().position(|session| {
            matches!(
                session.snapshot.status,
                DocumentTranslationSessionStatus::Queued
            )
        }) else {
            return;
        };
        let id = self.live.records[index].snapshot.id;
        if self.live.records[index].pending_cache_restore {
            self.start_cached_session_restore(index, cx);
            return;
        }
        if cx.has_global::<SettingsStore>()
            && cx
                .global::<SettingsStore>()
                .document_translation_credentials_loading()
        {
            self.wait_for_provider_credentials(cx);
            return;
        }
        if !cx.has_global::<TranslationServices>() {
            self.live.records[index].snapshot.status = DocumentTranslationSessionStatus::Failed {
                stage: PdfTranslationStage::Preparing,
                message: "Document translation is unavailable.".into(),
                detailed_message: "Document translation is unavailable.".into(),
                retryable: false,
            };
            self.publish_session_terminal_notification(id, cx);
            return;
        }

        Self::refresh_provider_secrets(&mut self.live.records[index].settings, cx);
        let settings = self.live.records[index].settings.clone();
        let config = PdfTranslationConfig::from(&settings);
        let request = self.live.records[index].snapshot.request.clone();
        let source_snapshot = self.live.records[index].source_snapshot.clone();
        let cancellation = self.live.records[index].cancellation.clone();
        let service = cx.global::<TranslationServices>().pdf().clone();
        let (update_tx, mut update_rx) =
            mpsc::unbounded::<DocumentTranslationSessionWorkerUpdate>();
        let progress_tx = update_tx.clone();
        self.live.records[index].snapshot.status = DocumentTranslationSessionStatus::Running {
            progress: None,
            units: TranslationRunProgress::default(),
        };
        self.live.active_id = Some(id);
        self.sync_session_background_task(id, cx);

        let task = cx.spawn(async move |this: gpui::WeakEntity<Self>, cx| {
            let result_task = cx.background_spawn(async move {
                service.run_session_from_snapshot(
                    config,
                    request,
                    source_snapshot,
                    cancellation,
                    move |progress| {
                        _ = progress_tx.unbounded_send(
                            DocumentTranslationSessionWorkerUpdate::Progress(progress),
                        );
                    },
                    move |update| {
                        _ = update_tx.unbounded_send(
                            DocumentTranslationSessionWorkerUpdate::Pipeline(update),
                        );
                    },
                )
            });

            while let Some(update) = update_rx.next().await {
                if this
                    .update(cx, |this, cx| this.update_session(id, update, cx))
                    .is_err()
                {
                    return;
                }
            }
            let result = result_task.await;
            _ = this.update(cx, |this, cx| this.finish_session(id, result, cx));
        });
        self.live.records[index].task = Some(task);
        cx.notify();
    }

    fn start_cached_session_restore(&mut self, index: usize, cx: &mut Context<Self>) {
        let id = self.live.records[index].snapshot.id;
        let Some(cache_key) = self.live.records[index].snapshot.cache_key.clone() else {
            self.live.records[index].pending_cache_restore = false;
            self.live.records[index].snapshot.status = DocumentTranslationSessionStatus::Paused {
                progress: None,
                units: TranslationRunProgress::default(),
                cache_available: false,
            };
            self.remove_session_background_task(id, cx);
            self.start_next_session(cx);
            cx.notify();
            return;
        };
        if !cx.has_global::<TranslationServices>() {
            self.live.records[index].pending_cache_restore = false;
            self.live.records[index].snapshot.status = DocumentTranslationSessionStatus::Paused {
                progress: None,
                units: TranslationRunProgress::default(),
                cache_available: false,
            };
            self.remove_session_background_task(id, cx);
            self.start_next_session(cx);
            cx.notify();
            return;
        }
        let request = self.live.records[index].snapshot.request.clone();
        let source_snapshot = self.live.records[index].source_snapshot.clone();
        let original_unit_ids = self.live.records[index]
            .snapshot
            .original_unit_ids
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        let cancellation = self.live.records[index].cancellation.clone();
        let service = cx.global::<TranslationServices>().pdf().clone();
        let (update_tx, mut update_rx) =
            mpsc::unbounded::<DocumentTranslationSessionWorkerUpdate>();
        let progress_tx = update_tx.clone();
        self.live.records[index].snapshot.status = DocumentTranslationSessionStatus::Running {
            progress: None,
            units: TranslationRunProgress::default(),
        };
        self.live.active_id = Some(id);
        self.sync_session_background_task(id, cx);
        let task = cx.spawn(async move |this: gpui::WeakEntity<Self>, cx| {
            let result_task = cx.background_spawn(async move {
                service.restore_cached_session_from_snapshot(
                    &cache_key,
                    &request,
                    source_snapshot,
                    &original_unit_ids,
                    cancellation,
                    move |progress| {
                        _ = progress_tx.unbounded_send(
                            DocumentTranslationSessionWorkerUpdate::Progress(progress),
                        );
                    },
                    move |update| {
                        _ = update_tx.unbounded_send(
                            DocumentTranslationSessionWorkerUpdate::Pipeline(update),
                        );
                    },
                )
            });
            while let Some(update) = update_rx.next().await {
                if this
                    .update(cx, |this, cx| this.update_session(id, update, cx))
                    .is_err()
                {
                    return;
                }
            }
            let result = result_task.await;
            _ = this.update(cx, |this, cx| {
                this.finish_cached_session_restore(id, result, cx)
            });
        });
        self.live.records[index].task = Some(task);
        cx.notify();
    }

    fn update_session(
        &mut self,
        id: DocumentTranslationId,
        update: DocumentTranslationSessionWorkerUpdate,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = self.session_mut(id) else {
            return;
        };
        match update {
            DocumentTranslationSessionWorkerUpdate::Pipeline(
                PdfTranslationSessionUpdate::CacheReady(cache_key),
            ) => {
                session.snapshot.cache_key = Some(cache_key);
            }
            DocumentTranslationSessionWorkerUpdate::Progress(progress) => {
                match &mut session.snapshot.status {
                    DocumentTranslationSessionStatus::Running {
                        progress: current, ..
                    }
                    | DocumentTranslationSessionStatus::Cancelling { progress: current }
                    | DocumentTranslationSessionStatus::Exporting {
                        progress: current, ..
                    } => *current = Some(progress),
                    _ => return,
                }
            }
            DocumentTranslationSessionWorkerUpdate::Pipeline(
                PdfTranslationSessionUpdate::PagePrepared {
                    metadata,
                    page,
                    page_count,
                    ready_prefix,
                },
            ) => {
                session.prepared_metadata = Some(metadata.clone());
                session.prepared_page_count = page_count;
                session.prepared_pages.insert(page.page_index, page);
                session.snapshot.ready_prefix = ready_prefix;
                let pages = (0..ready_prefix)
                    .map(|page_index| session.prepared_pages.get(&page_index).cloned())
                    .collect::<Option<Vec<_>>>();
                if let Some(pages) = pages {
                    session.snapshot.prepared_document =
                        Some(Arc::new(DocumentV1::new(metadata, pages)));
                }
            }
            DocumentTranslationSessionWorkerUpdate::Pipeline(
                PdfTranslationSessionUpdate::UnitsChanged(snapshot),
            ) => {
                session.snapshot.resolved_units = Arc::new(snapshot.resolved_units);
                let mut failures = snapshot.failures;
                failures.extend(
                    session
                        .snapshot
                        .failures
                        .iter()
                        .filter(|failure| failure.phase != TranslationFailurePhase::Translation)
                        .cloned(),
                );
                failures.sort_by(|left, right| left.id.cmp(&right.id));
                failures.dedup_by(|left, right| left.id == right.id && left.phase == right.phase);
                session.snapshot.failures = Arc::new(failures);
                if let DocumentTranslationSessionStatus::Running { units, .. } =
                    &mut session.snapshot.status
                {
                    *units = snapshot.progress;
                }
            }
            DocumentTranslationSessionWorkerUpdate::Pipeline(
                PdfTranslationSessionUpdate::PagePreviewReady(preview),
            ) => {
                let mut previews = (*session.snapshot.page_previews).clone();
                let page_index = preview.page_index;
                let layout_failures = preview.layout_failures.clone();
                let replace = previews
                    .get(&page_index)
                    .is_none_or(|current| preview.revision > current.revision);
                if replace {
                    previews.insert(page_index, Arc::new(preview));
                    session.snapshot.page_previews = Arc::new(previews);
                    let page_block_ids = session
                        .snapshot
                        .prepared_document
                        .as_ref()
                        .and_then(|document| document.pages.get(page_index))
                        .map(|page| {
                            page.blocks
                                .iter()
                                .map(|block| block.id.as_str())
                                .collect::<HashSet<_>>()
                        })
                        .unwrap_or_default();
                    let mut failures = (*session.snapshot.failures).clone();
                    failures.retain(|failure| {
                        failure.phase != TranslationFailurePhase::Layout
                            || !failure
                                .member_block_ids
                                .iter()
                                .any(|block_id| page_block_ids.contains(block_id.as_str()))
                    });
                    failures.extend(layout_failures);
                    failures.sort_by(|left, right| left.id.cmp(&right.id));
                    failures
                        .dedup_by(|left, right| left.id == right.id && left.phase == right.phase);
                    session.snapshot.failures = Arc::new(failures);

                    let mut page_failures = (*session.snapshot.page_failures).clone();
                    page_failures.retain(|failure| {
                        failure.page_index != page_index
                            || !matches!(
                                failure.phase,
                                TranslationFailurePhase::Layout | TranslationFailurePhase::Preview
                            )
                    });
                    session.snapshot.page_failures = Arc::new(page_failures);
                }
            }
            DocumentTranslationSessionWorkerUpdate::Pipeline(
                PdfTranslationSessionUpdate::PageFailed(failure),
            ) => {
                let mut failures = (*session.snapshot.page_failures).clone();
                failures.retain(|current| {
                    current.page_index != failure.page_index || current.phase != failure.phase
                });
                failures.push(failure);
                failures.sort_by_key(|failure| (failure.page_index, failure.phase as u8));
                session.snapshot.page_failures = Arc::new(failures);
            }
            DocumentTranslationSessionWorkerUpdate::Pipeline(
                PdfTranslationSessionUpdate::Reconciled {
                    ready_prefix,
                    invalidated_unit_ids,
                    ..
                },
            ) => {
                session.snapshot.ready_prefix = ready_prefix;
                if !invalidated_unit_ids.is_empty() {
                    let invalidated = invalidated_unit_ids.into_iter().collect::<HashSet<_>>();
                    let mut units = (*session.snapshot.resolved_units).clone();
                    units.retain(|unit| !invalidated.contains(&unit.id));
                    session.snapshot.resolved_units = Arc::new(units);
                }
            }
            DocumentTranslationSessionWorkerUpdate::Pipeline(
                PdfTranslationSessionUpdate::Finished,
            ) => {}
        }
        self.sync_session_background_task(id, cx);
        cx.notify();
    }

    fn update_session_progress(
        &mut self,
        id: DocumentTranslationId,
        progress: PdfTranslationProgress,
        cx: &mut Context<Self>,
    ) {
        self.update_session(
            id,
            DocumentTranslationSessionWorkerUpdate::Progress(progress),
            cx,
        );
    }

    fn finish_cached_session_restore(
        &mut self,
        id: DocumentTranslationId,
        restored: Result<Option<PdfTranslationRestoredSession>, PdfTranslationError>,
        cx: &mut Context<Self>,
    ) {
        if self.live.active_id == Some(id) {
            self.live.active_id = None;
        }
        let Some(session) = self.session_mut(id) else {
            self.start_next_session(cx);
            return;
        };
        session.task = None;
        session.pending_cache_restore = false;
        let current_progress = match &session.snapshot.status {
            DocumentTranslationSessionStatus::Running { progress, .. }
            | DocumentTranslationSessionStatus::Cancelling { progress }
            | DocumentTranslationSessionStatus::Paused { progress, .. } => progress.clone(),
            _ => None,
        };
        match restored {
            Ok(Some(restored)) => {
                let result = Arc::new(restored.result);
                session.snapshot.prepared_document =
                    Some(Arc::new(result.translated.document.clone()));
                session.snapshot.ready_prefix = result.translated.document.pages.len();
                session.snapshot.resolved_units = Arc::new(result.translated.units.clone());
                let mut failures = result.failures.clone();
                failures.extend(
                    session
                        .snapshot
                        .failures
                        .iter()
                        .filter(|failure| failure.phase != TranslationFailurePhase::Translation)
                        .cloned(),
                );
                failures.sort_by(|left, right| left.id.cmp(&right.id));
                failures.dedup_by(|left, right| left.id == right.id && left.phase == right.phase);
                session.snapshot.failures = Arc::new(failures);
                let mut page_failures = (*session.snapshot.page_failures).clone();
                page_failures.extend(restored.page_failures);
                page_failures.sort_by_key(|failure| (failure.page_index, failure.phase as u8));
                page_failures.dedup_by(|left, right| {
                    left.page_index == right.page_index && left.phase == right.phase
                });
                session.snapshot.page_failures = Arc::new(page_failures);
                let failed_units = session.snapshot.failure_count();
                session.snapshot.status = if restored.completed {
                    DocumentTranslationSessionStatus::Completed { failed_units }
                } else {
                    DocumentTranslationSessionStatus::Paused {
                        progress: current_progress,
                        units: result.progress.clone(),
                        cache_available: true,
                    }
                };
                session.result = Some(result);
            }
            Ok(None) => {
                session.snapshot.status = DocumentTranslationSessionStatus::Paused {
                    progress: None,
                    units: TranslationRunProgress::default(),
                    cache_available: false,
                };
            }
            Err(error) => {
                session.snapshot.status = DocumentTranslationSessionStatus::Paused {
                    progress: Some(PdfTranslationProgress {
                        stage: error.stage,
                        detail: error.message,
                        completed: None,
                        total: None,
                        translated: None,
                        cached: None,
                        retrying: None,
                        failed: None,
                        overall_fraction: 0.0,
                        indeterminate: false,
                    }),
                    units: TranslationRunProgress::default(),
                    cache_available: false,
                };
            }
        }
        self.remove_session_background_task(id, cx);
        self.start_next_session(cx);
        cx.notify();
    }

    fn finish_session(
        &mut self,
        id: DocumentTranslationId,
        result: Result<PdfTranslationSessionResult, PdfTranslationError>,
        cx: &mut Context<Self>,
    ) {
        if self.live.active_id != Some(id) {
            return;
        }
        self.live.active_id = None;
        let pending_restart = self
            .session_mut(id)
            .and_then(|session| session.pending_restart.take());
        if let Some(target_language) = pending_restart {
            self.reset_session_for_retry(id, Some(target_language));
            self.sync_session_background_task(id, cx);
            self.start_next_session(cx);
            cx.notify();
            return;
        }

        let was_cancelled = self.session(id).is_some_and(|session| {
            session.cancellation.is_cancelled()
                || matches!(
                    session.snapshot.status,
                    DocumentTranslationSessionStatus::Cancelling { .. }
                )
        });
        if was_cancelled {
            let remove = self.session(id).is_some_and(|session| {
                session.snapshot.attached_views == 0 && !session.notify_when_finished
            });
            if remove {
                self.live
                    .records
                    .retain(|session| session.snapshot.id != id);
            } else if let Some(session) = self.session_mut(id) {
                session.task = None;
                session.snapshot.status = DocumentTranslationSessionStatus::Cancelled;
            }
            self.remove_session_background_task(id, cx);
            self.start_next_session(cx);
            cx.notify();
            return;
        }

        let mut publish_notification = false;
        let mut rebuild_original_previews = false;
        if let Some(session) = self.session_mut(id) {
            session.task = None;
            match result {
                Ok(result) => {
                    session.snapshot.prepared_document =
                        Some(Arc::new(result.translated.document.clone()));
                    session.snapshot.ready_prefix = result.translated.document.pages.len();
                    session.snapshot.resolved_units = Arc::new(result.translated.units.clone());
                    session.snapshot.failures = Arc::new(result.failures.clone());
                    let failed_units = session.snapshot.failure_count();
                    session.snapshot.status =
                        DocumentTranslationSessionStatus::Completed { failed_units };
                    session.result = Some(Arc::new(result));
                    rebuild_original_previews = !session.snapshot.original_unit_ids.is_empty();
                    publish_notification = session.should_publish_completion_notification();
                }
                Err(error) => {
                    let detailed_message = error
                        .detailed_message
                        .unwrap_or_else(|| error.message.clone());
                    session.snapshot.status = DocumentTranslationSessionStatus::Failed {
                        stage: error.stage,
                        message: error.message.into(),
                        detailed_message: detailed_message.into(),
                        retryable: error.retryable,
                    };
                    publish_notification = true;
                }
            }
        }
        if rebuild_original_previews {
            self.rebuild_session_original_previews(id, None, cx);
        }
        if publish_notification {
            self.publish_session_terminal_notification(id, cx);
        } else {
            self.remove_session_background_task(id, cx);
        }
        self.start_next_session(cx);
        cx.notify();
    }

    fn finish_session_export(
        &mut self,
        id: DocumentTranslationId,
        result: Result<PdfTranslationResult, PdfTranslationError>,
        cx: &mut Context<Self>,
    ) {
        if self.live.active_id == Some(id) {
            self.live.active_id = None;
        }
        let pending_restart = self
            .session_mut(id)
            .and_then(|session| session.pending_restart.take());
        if let Some(target_language) = pending_restart {
            self.reset_session_for_retry(id, Some(target_language));
            self.sync_session_background_task(id, cx);
            self.start_next_session(cx);
            cx.notify();
            return;
        }
        let was_cancelled = self
            .session(id)
            .is_some_and(|session| session.cancellation.is_cancelled());
        {
            let Some(session) = self.session_mut(id) else {
                return;
            };
            session.task = None;
            if was_cancelled {
                let failed_units = session.snapshot.failure_count();
                session.snapshot.status =
                    DocumentTranslationSessionStatus::Completed { failed_units };
                self.remove_session_background_task(id, cx);
                self.start_next_session(cx);
                cx.notify();
                return;
            }
            match result {
                Ok(result) => {
                    let failed_units = session.snapshot.failure_count();
                    session.snapshot.last_export_path = Some(result.output_path);
                    session.snapshot.last_export_error = None;
                    session.snapshot.status =
                        DocumentTranslationSessionStatus::Completed { failed_units };
                }
                Err(error) => {
                    let detailed_message = error
                        .detailed_message
                        .unwrap_or_else(|| error.message.clone());
                    session.snapshot.last_export_error = Some(detailed_message.into());
                    session.snapshot.status = DocumentTranslationSessionStatus::Completed {
                        failed_units: session.snapshot.failure_count(),
                    };
                }
            }
        }
        // Export is an explicit user action (from the live toolbar or the
        // activity center), not a newly completed background translation.
        // Reflect the result in session state without raising the translation
        // completion notification a second time.
        self.remove_session_background_task(id, cx);
        self.start_next_session(cx);
        cx.notify();
    }

    fn reset_session_for_retry(
        &mut self,
        id: DocumentTranslationId,
        target_language: Option<LanguageCode>,
    ) {
        let Some(session) = self.session_mut(id) else {
            return;
        };
        if let Some(target_language) = target_language {
            session.snapshot.request.target_language = target_language.clone();
            session.settings.translation.target_language =
                TranslationTargetLanguage::Explicit(target_language.as_str().to_string());
        }
        session.cancellation = CancellationToken::new();
        session.task = None;
        session.pending_export = None;
        session.result = None;
        session.snapshot.status = DocumentTranslationSessionStatus::Queued;
        session.snapshot.prepared_document = None;
        session.snapshot.resolved_units = Arc::new(Vec::new());
        session.snapshot.failures = Arc::new(Vec::new());
        session.snapshot.page_previews = Arc::new(BTreeMap::new());
        session.snapshot.page_failures = Arc::new(Vec::new());
        session.snapshot.ready_prefix = 0;
        session.snapshot.original_unit_ids = Arc::new(HashSet::new());
        session.snapshot.cache_key = None;
        session.snapshot.last_export_path = None;
        session.snapshot.last_export_error = None;
        session.prepared_metadata = None;
        session.prepared_pages.clear();
        session.prepared_page_count = 0;
        session.preview_generation = session.preview_generation.wrapping_add(1);
        session.pending_cache_restore = false;
    }

    fn session(&self, id: DocumentTranslationId) -> Option<&DocumentTranslationSessionRecord> {
        self.live
            .records
            .iter()
            .find(|session| session.snapshot.id == id)
    }

    fn session_mut(
        &mut self,
        id: DocumentTranslationId,
    ) -> Option<&mut DocumentTranslationSessionRecord> {
        self.live
            .records
            .iter_mut()
            .find(|session| session.snapshot.id == id)
    }

    fn job(&self, id: DocumentTranslationId) -> Option<&DocumentTranslationJobRecord> {
        self.batch.records.iter().find(|job| job.snapshot.id == id)
    }

    fn job_mut(&mut self, id: DocumentTranslationId) -> Option<&mut DocumentTranslationJobRecord> {
        self.batch
            .records
            .iter_mut()
            .find(|job| job.snapshot.id == id)
    }

    fn register_background_task(&mut self, id: DocumentTranslationId, cx: &mut Context<Self>) {
        if self.batch.background_tasks.contains_key(&id) {
            self.sync_background_task(id, cx);
            return;
        }
        let Some(snapshot) = self.job(id).map(|job| job.snapshot.clone()) else {
            return;
        };
        let Some(center) = ActivityCenter::try_global(cx) else {
            return;
        };
        let weak_manager = cx.weak_entity();
        let entry = background_task_entry(
            &snapshot,
            Some(CenterAction::new("Cancel", move |_, cx| {
                _ = weak_manager.update(cx, |manager, cx| manager.cancel(id, cx));
            })),
        );
        let task_id = center.update(cx, |center, cx| center.add_task(entry, cx));
        self.batch.background_tasks.insert(id, task_id);
    }

    fn sync_background_task(&mut self, id: DocumentTranslationId, cx: &mut Context<Self>) {
        let Some(task_id) = self.batch.background_tasks.get(&id).copied() else {
            self.register_background_task(id, cx);
            return;
        };
        let Some(snapshot) = self.job(id).map(|job| job.snapshot.clone()) else {
            self.remove_background_task(id, cx);
            return;
        };
        let Some(center) = ActivityCenter::try_global(cx) else {
            return;
        };
        let weak_manager = cx.weak_entity();
        let cancel_action = matches!(
            snapshot.status,
            DocumentTranslationJobStatus::Queued | DocumentTranslationJobStatus::Running { .. }
        )
        .then(|| {
            CenterAction::new("Cancel", move |_, cx| {
                _ = weak_manager.update(cx, |manager, cx| manager.cancel(id, cx));
            })
        });
        let entry = background_task_entry(&snapshot, cancel_action);
        center.update(cx, |center, cx| {
            center.update_task(task_id, |task| *task = entry, cx)
        });
    }

    fn remove_background_task(&mut self, id: DocumentTranslationId, cx: &mut Context<Self>) {
        let Some(task_id) = self.batch.background_tasks.remove(&id) else {
            return;
        };
        if let Some(center) = ActivityCenter::try_global(cx) {
            center.update(cx, |center, cx| center.remove_task(task_id, cx));
        }
    }

    fn remove_job_notification(&mut self, id: DocumentTranslationId, cx: &mut Context<Self>) {
        cx.emit(DocumentTranslationJobEvent::RemoveNotification { id });
    }

    fn remove_activity_notice(&mut self, id: DocumentTranslationId, cx: &mut Context<Self>) {
        let Some(notice_id) = self.batch.activity_notices.remove(&id) else {
            return;
        };
        if let Some(center) = ActivityCenter::try_global(cx) {
            center.update(cx, |center, cx| {
                center.dismiss_notice_by_owner(notice_id, cx)
            });
        }
    }

    fn activity_notice_removed(&mut self, id: DocumentTranslationId, cx: &mut Context<Self>) {
        if self.batch.activity_notices.remove(&id).is_none() {
            return;
        }
        if self.job(id).is_some_and(|job| !job.snapshot.is_active()) {
            self.batch.records.retain(|job| job.snapshot.id != id);
            cx.notify();
        }
    }

    fn publish_terminal_notification(&mut self, id: DocumentTranslationId, cx: &mut Context<Self>) {
        let Some(snapshot) = self.job(id).map(|job| job.snapshot.clone()) else {
            return;
        };
        self.remove_activity_notice(id, cx);
        let task_id = self.batch.background_tasks.remove(&id);
        if let Some(center) = ActivityCenter::try_global(cx) {
            let notice = document_translation_activity_notice(&snapshot, cx.weak_entity());
            let notice_id = center.update(cx, |center, cx| match task_id {
                Some(task_id) => center.finish_task(task_id, notice, cx),
                None => center.add_notice(notice, cx),
            });
            self.batch.activity_notices.insert(id, notice_id);
        }
        cx.emit(DocumentTranslationJobEvent::ShowNotification { snapshot });
    }

    fn notification_closed(&mut self, id: DocumentTranslationId, cx: &mut Context<Self>) {
        let Some(job) = self.job(id) else {
            return;
        };
        if !job.snapshot.is_active() && !self.batch.activity_notices.contains_key(&id) {
            self.remove_background_task(id, cx);
            self.batch.records.retain(|job| job.snapshot.id != id);
            cx.notify();
        }
    }

    fn register_session_background_task(
        &mut self,
        id: DocumentTranslationId,
        cx: &mut Context<Self>,
    ) {
        if self.live.background_tasks.contains_key(&id) {
            self.sync_session_background_task(id, cx);
            return;
        }
        let Some(snapshot) = self.session_snapshot(id) else {
            return;
        };
        let Some(center) = ActivityCenter::try_global(cx) else {
            return;
        };
        let weak_manager = cx.weak_entity();
        let entry = session_background_task_entry(
            &snapshot,
            Some(CenterAction::new("Cancel", move |_, cx| {
                _ = weak_manager.update(cx, |manager, cx| manager.cancel_session(id, cx));
            })),
        );
        let task_id = center.update(cx, |center, cx| center.add_task(entry, cx));
        self.live.background_tasks.insert(id, task_id);
    }

    fn sync_session_background_task(&mut self, id: DocumentTranslationId, cx: &mut Context<Self>) {
        let Some(task_id) = self.live.background_tasks.get(&id).copied() else {
            self.register_session_background_task(id, cx);
            return;
        };
        let Some(snapshot) = self.session_snapshot(id) else {
            self.remove_session_background_task(id, cx);
            return;
        };
        let Some(center) = ActivityCenter::try_global(cx) else {
            return;
        };
        let weak_manager = cx.weak_entity();
        let cancel_action = matches!(
            snapshot.status,
            DocumentTranslationSessionStatus::Queued
                | DocumentTranslationSessionStatus::Running { .. }
                | DocumentTranslationSessionStatus::Exporting { .. }
        )
        .then(|| {
            CenterAction::new("Cancel", move |_, cx| {
                _ = weak_manager.update(cx, |manager, cx| manager.cancel_session(id, cx));
            })
        });
        let entry = session_background_task_entry(&snapshot, cancel_action);
        center.update(cx, |center, cx| {
            center.update_task(task_id, |task| *task = entry, cx)
        });
    }

    fn remove_session_background_task(
        &mut self,
        id: DocumentTranslationId,
        cx: &mut Context<Self>,
    ) {
        let Some(task_id) = self.live.background_tasks.remove(&id) else {
            return;
        };
        if let Some(center) = ActivityCenter::try_global(cx) {
            center.update(cx, |center, cx| center.remove_task(task_id, cx));
        }
    }

    fn remove_session_activity_notice(
        &mut self,
        id: DocumentTranslationId,
        cx: &mut Context<Self>,
    ) {
        let Some(notice_id) = self.live.activity_notices.remove(&id) else {
            return;
        };
        if let Some(center) = ActivityCenter::try_global(cx) {
            center.update(cx, |center, cx| {
                center.dismiss_notice_by_owner(notice_id, cx)
            });
        }
    }

    fn session_activity_notice_removed(
        &mut self,
        id: DocumentTranslationId,
        cx: &mut Context<Self>,
    ) {
        self.live.activity_notices.remove(&id);
        let remove = self.session(id).is_some_and(|session| {
            session.snapshot.attached_views == 0 && !session.snapshot.is_translating()
        });
        if remove {
            self.live
                .records
                .retain(|session| session.snapshot.id != id);
        }
        cx.notify();
    }

    fn publish_session_terminal_notification(
        &mut self,
        id: DocumentTranslationId,
        cx: &mut Context<Self>,
    ) {
        let Some(snapshot) = self.session_snapshot(id) else {
            return;
        };
        self.remove_session_activity_notice(id, cx);
        let task_id = self.live.background_tasks.remove(&id);
        if let Some(center) = ActivityCenter::try_global(cx) {
            let notice = document_translation_session_activity_notice(&snapshot, cx.weak_entity());
            let notice_id = center.update(cx, |center, cx| match task_id {
                Some(task_id) => center.finish_task(task_id, notice, cx),
                None => center.add_notice(notice, cx),
            });
            self.live.activity_notices.insert(id, notice_id);
        }
        cx.emit(DocumentTranslationJobEvent::ShowSessionNotification { snapshot });
    }

    fn session_notification_closed(&mut self, id: DocumentTranslationId, cx: &mut Context<Self>) {
        let remove = self.session(id).is_some_and(|session| {
            session.snapshot.attached_views == 0
                && !session.snapshot.is_translating()
                && !self.live.activity_notices.contains_key(&id)
        });
        if remove {
            self.live
                .records
                .retain(|session| session.snapshot.id != id);
            cx.notify();
        }
    }
}

fn document_translation_activity_notice(
    snapshot: &DocumentTranslationJobSnapshot,
    manager: WeakEntity<DocumentTranslationManager>,
) -> ActivityNoticeEntry {
    let id = snapshot.id;
    let level = match &snapshot.status {
        DocumentTranslationJobStatus::Succeeded {
            open_error: None,
            warnings,
            ..
        } if warnings.is_empty() => ActivityNoticeLevel::Success,
        DocumentTranslationJobStatus::Succeeded {
            open_error: None, ..
        } => ActivityNoticeLevel::Warning,
        DocumentTranslationJobStatus::Failed { .. }
        | DocumentTranslationJobStatus::Cancelled
        | DocumentTranslationJobStatus::Succeeded {
            open_error: Some(_),
            ..
        } => ActivityNoticeLevel::Error,
        _ => ActivityNoticeLevel::Info,
    };
    let (status, detail, _, _) = job_status_content(snapshot);
    let open_error = matches!(
        snapshot.status,
        DocumentTranslationJobStatus::Succeeded {
            open_error: Some(_),
            ..
        }
    );
    let has_warnings = matches!(
        &snapshot.status,
        DocumentTranslationJobStatus::Succeeded { warnings, .. } if !warnings.is_empty()
    );
    let mut notice = ActivityNoticeEntry::new(
        level,
        format!("Translate {}", snapshot.title()),
        format!("{status} · {detail}"),
    );
    notice.actions = job_actions(&snapshot.status)
        .into_iter()
        .map(|action| {
            let manager = manager.clone();
            let label = action.label(snapshot.open_mode, open_error, has_warnings);
            CenterAction::new(label, move |_, cx| {
                _ = manager.update(cx, |manager, cx| match action {
                    DocumentTranslationJobAction::Retry => manager.retry(id, cx),
                    DocumentTranslationJobAction::Open => manager.open(id, cx),
                    DocumentTranslationJobAction::CopyError => manager.copy_error(id, cx),
                });
            })
        })
        .collect();
    let cleanup_manager = manager;
    notice.on_remove(move |cx| {
        _ = cleanup_manager.update(cx, |manager, cx| manager.activity_notice_removed(id, cx));
    })
}

fn document_translation_session_activity_notice(
    snapshot: &DocumentTranslationSessionSnapshot,
    manager: WeakEntity<DocumentTranslationManager>,
) -> ActivityNoticeEntry {
    let id = snapshot.id;
    let level = match snapshot.status {
        _ if snapshot.last_export_error.is_some() => ActivityNoticeLevel::Error,
        DocumentTranslationSessionStatus::Completed { failed_units: 0 } => {
            ActivityNoticeLevel::Success
        }
        DocumentTranslationSessionStatus::Completed { .. } => ActivityNoticeLevel::Warning,
        DocumentTranslationSessionStatus::Failed { .. } => ActivityNoticeLevel::Error,
        _ => ActivityNoticeLevel::Info,
    };
    let (status, detail, _, _) = session_status_content(snapshot);
    let mut notice = ActivityNoticeEntry::new(
        level,
        format!("Translate {}", snapshot.title()),
        format!("{status} · {detail}"),
    );
    if matches!(
        snapshot.status,
        DocumentTranslationSessionStatus::Completed { .. }
    ) {
        if snapshot.attached_views == 0 {
            let open_manager = manager.clone();
            notice
                .actions
                .push(CenterAction::new("Open translation", move |_, cx| {
                    _ = open_manager.update(cx, |manager, cx| manager.request_open_session(id, cx));
                }));
        }
        let save_manager = manager.clone();
        notice.actions.push(CenterAction::new("Save", move |_, cx| {
            _ = save_manager.update(cx, |manager, cx| manager.request_save_session(id, cx));
        }));
        if snapshot.last_export_path.is_some() {
            let output_manager = manager.clone();
            notice
                .actions
                .push(CenterAction::new("Open output", move |_, cx| {
                    _ = output_manager.update(cx, |manager, cx| {
                        manager.request_open_session_export(id, cx)
                    });
                }));
        }
        if snapshot.has_failures() {
            let retry_manager = manager.clone();
            notice
                .actions
                .push(CenterAction::new("Retry failed", move |_, cx| {
                    _ = retry_manager
                        .update(cx, |manager, cx| manager.retry_session_failures(id, cx));
                }));
        }
    }
    let cleanup_manager = manager;
    notice.on_remove(move |cx| {
        _ = cleanup_manager.update(cx, |manager, cx| {
            manager.session_activity_notice_removed(id, cx)
        });
    })
}

/// Build the window notification for an in-memory translation session.
pub fn document_translation_session_notification(
    snapshot: DocumentTranslationSessionSnapshot,
    manager: WeakEntity<DocumentTranslationManager>,
) -> Notification {
    let id = snapshot.id;
    let type_ = match snapshot.status {
        _ if snapshot.last_export_error.is_some() => NotificationType::Error,
        DocumentTranslationSessionStatus::Completed { failed_units: 0 } => {
            NotificationType::Success
        }
        DocumentTranslationSessionStatus::Completed { .. } => NotificationType::Warning,
        DocumentTranslationSessionStatus::Failed { .. } => NotificationType::Error,
        _ => NotificationType::Info,
    };
    let (status, detail, _, _) = session_status_content(&snapshot);
    let content_snapshot = snapshot.clone();
    let close_manager = manager.clone();
    Notification::new()
        .id1::<DocumentTranslationSessionNotification>(id.to_string())
        .with_type(type_)
        .title(snapshot.title())
        .message(status)
        .content(move |_, _, cx| {
            let completed = matches!(
                content_snapshot.status,
                DocumentTranslationSessionStatus::Completed { .. }
            );
            let mut actions = Vec::new();
            if completed {
                if content_snapshot.attached_views == 0 {
                    actions.push(session_notification_button(
                        manager.clone(),
                        id,
                        "open",
                        "Open translation",
                        SessionNotificationAction::Open,
                        true,
                    ));
                }
                actions.push(session_notification_button(
                    manager.clone(),
                    id,
                    "save",
                    "Save",
                    SessionNotificationAction::Save,
                    content_snapshot.attached_views > 0,
                ));
                if content_snapshot.last_export_path.is_some() {
                    actions.push(session_notification_button(
                        manager.clone(),
                        id,
                        "open-output",
                        "Open output",
                        SessionNotificationAction::OpenOutput,
                        false,
                    ));
                }
                if content_snapshot.has_failures() {
                    actions.push(session_notification_button(
                        manager.clone(),
                        id,
                        "retry",
                        "Retry failed",
                        SessionNotificationAction::Retry,
                        false,
                    ));
                }
            }
            v_flex()
                .gap_2()
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(detail.clone()),
                )
                .when(!actions.is_empty(), |this| {
                    this.child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .justify_end()
                            .gap_1()
                            .children(actions),
                    )
                })
                .into_any_element()
        })
        .on_close(move |_, cx| {
            _ = close_manager.update(cx, |manager, cx| {
                manager.session_notification_closed(id, cx)
            });
        })
        .autohide(false)
}

#[derive(Clone, Copy)]
enum SessionNotificationAction {
    Open,
    Save,
    OpenOutput,
    Retry,
}

fn session_notification_button(
    manager: WeakEntity<DocumentTranslationManager>,
    id: DocumentTranslationId,
    action_id: &'static str,
    label: &'static str,
    action: SessionNotificationAction,
    primary: bool,
) -> Button {
    let button = Button::new(format!("document-translation-session-{id}-{action_id}"))
        .label(label)
        .with_size(Size::XSmall)
        .on_click(move |_, _, cx| {
            _ = manager.update(cx, |manager, cx| match action {
                SessionNotificationAction::Open => manager.request_open_session(id, cx),
                SessionNotificationAction::Save => manager.request_save_session(id, cx),
                SessionNotificationAction::OpenOutput => {
                    manager.request_open_session_export(id, cx)
                }
                SessionNotificationAction::Retry => manager.retry_session_failures(id, cx),
            });
            cx.stop_propagation();
        });
    if primary {
        button.primary()
    } else {
        button.ghost()
    }
}

/// Build the window-scoped gpui-component notification for a terminal
/// translation result.
pub fn document_translation_notification(
    snapshot: DocumentTranslationJobSnapshot,
    manager: WeakEntity<DocumentTranslationManager>,
) -> Notification {
    let id = snapshot.id;
    let type_ = match &snapshot.status {
        DocumentTranslationJobStatus::Succeeded {
            open_error: None,
            warnings,
            ..
        } if warnings.is_empty() => NotificationType::Success,
        DocumentTranslationJobStatus::Succeeded {
            open_error: None, ..
        } => NotificationType::Warning,
        DocumentTranslationJobStatus::Failed { .. }
        | DocumentTranslationJobStatus::Cancelled
        | DocumentTranslationJobStatus::Succeeded {
            open_error: Some(_),
            ..
        } => NotificationType::Error,
        _ => NotificationType::Info,
    };
    let title = snapshot.title();
    let (status, detail, _, _) = job_status_content(&snapshot);
    let autohide = matches!(
        &snapshot.status,
        DocumentTranslationJobStatus::Succeeded {
            open_error: None,
            warnings,
            ..
        } if warnings.is_empty()
    );
    let content_snapshot = snapshot.clone();
    let close_manager = manager.clone();

    let notification = Notification::new()
        .id1::<DocumentTranslationNotification>(id.to_string())
        .with_type(type_)
        .title(title)
        .message(status)
        .content(move |_, _, cx| {
            let open_error = matches!(
                content_snapshot.status,
                DocumentTranslationJobStatus::Succeeded {
                    open_error: Some(_),
                    ..
                }
            );
            let has_warnings = matches!(
                &content_snapshot.status,
                DocumentTranslationJobStatus::Succeeded { warnings, .. } if !warnings.is_empty()
            );
            let actions = job_actions(&content_snapshot.status)
                .into_iter()
                .map(|action| {
                    document_translation_action_button(
                        manager.clone(),
                        id,
                        content_snapshot.open_mode,
                        action,
                        open_error,
                        has_warnings,
                    )
                })
                .collect::<Vec<_>>();

            v_flex()
                .gap_2()
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(detail.clone()),
                )
                .when(!actions.is_empty(), |this| {
                    this.child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .justify_end()
                            .gap_1()
                            .children(actions),
                    )
                })
                .into_any_element()
        })
        .on_close(move |_, cx| {
            _ = close_manager.update(cx, |manager, cx| {
                manager.notification_closed(id, cx);
            });
        });

    if autohide {
        notification.autohide_after(glyphst_ui::LONG_FEEDBACK_DURATION)
    } else {
        notification.autohide(false)
    }
}

fn document_translation_action_button(
    manager: WeakEntity<DocumentTranslationManager>,
    id: DocumentTranslationId,
    open_mode: DocumentTranslationOpenMode,
    action: DocumentTranslationJobAction,
    open_error: bool,
    has_warnings: bool,
) -> Button {
    let button = Button::new(format!("document-translation-job-{id}-{}", action.id()))
        .label(action.label(open_mode, open_error, has_warnings))
        .with_size(Size::XSmall)
        .on_click(move |_, _, cx| {
            _ = manager.update(cx, |manager, cx| match action {
                DocumentTranslationJobAction::Retry => manager.retry(id, cx),
                DocumentTranslationJobAction::Open => manager.open(id, cx),
                DocumentTranslationJobAction::CopyError => manager.copy_error(id, cx),
            });
            cx.stop_propagation();
        });

    match action {
        DocumentTranslationJobAction::Open | DocumentTranslationJobAction::Retry => {
            button.primary()
        }
        DocumentTranslationJobAction::CopyError => button.ghost(),
    }
}

fn background_task_entry(
    snapshot: &DocumentTranslationJobSnapshot,
    cancel_action: Option<CenterAction>,
) -> BackgroundTaskEntry {
    let (status, detail, fraction, indeterminate) = job_status_content(snapshot);
    let mut entry = BackgroundTaskEntry::new(
        format!("Translate {}", snapshot.title()),
        format!("{status} · {detail}"),
    );
    entry.progress = Some(TaskProgress {
        fraction,
        indeterminate,
    });
    if let Some(action) = cancel_action {
        entry.actions.push(action);
    }
    entry
}

fn session_background_task_entry(
    snapshot: &DocumentTranslationSessionSnapshot,
    cancel_action: Option<CenterAction>,
) -> BackgroundTaskEntry {
    let (status, detail, fraction, indeterminate) = session_status_content(snapshot);
    let mut entry = BackgroundTaskEntry::new(
        format!("Translate {}", snapshot.title()),
        format!("{status} · {detail}"),
    );
    entry.progress = Some(TaskProgress {
        fraction,
        indeterminate,
    });
    if let Some(action) = cancel_action {
        entry.actions.push(action);
    }
    entry
}

fn stage_label(stage: PdfTranslationStage) -> &'static str {
    match stage {
        PdfTranslationStage::Preparing => "Preparing",
        PdfTranslationStage::Ocr => "Reading text",
        PdfTranslationStage::Normalizing => "Normalizing",
        PdfTranslationStage::Translating => "Translating",
        PdfTranslationStage::Layout => "Laying out",
        PdfTranslationStage::Compiling => "Compiling",
        PdfTranslationStage::Composing => "Composing",
        PdfTranslationStage::Validating => "Validating",
    }
}

fn progress_fraction(progress: &PdfTranslationProgress) -> f32 {
    if progress.overall_fraction.is_finite() {
        progress.overall_fraction.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

fn progress_count(progress: &PdfTranslationProgress) -> Option<String> {
    let (completed, total) = progress.completed.zip(progress.total)?;
    let unit = match progress.stage {
        PdfTranslationStage::Ocr => "pages",
        PdfTranslationStage::Translating => "units",
        _ => "steps",
    };
    Some(format!("{completed} / {total} {unit}"))
}

fn session_status_content(
    snapshot: &DocumentTranslationSessionSnapshot,
) -> (String, String, f32, bool) {
    if let Some(error) = &snapshot.last_export_error {
        return (
            "Could not save translation".to_string(),
            error.to_string(),
            1.0,
            false,
        );
    }
    match &snapshot.status {
        DocumentTranslationSessionStatus::Queued => (
            "Queued".to_string(),
            "Waiting for another document translation to finish.".to_string(),
            0.0,
            true,
        ),
        DocumentTranslationSessionStatus::Running { progress, units } => {
            if let Some(progress) = progress {
                let mut status = format!("Translating · {}", stage_label(progress.stage));
                if let Some(count) = progress_count(progress) {
                    status.push_str(" · ");
                    status.push_str(&count);
                }
                (
                    status,
                    progress.detail.clone(),
                    progress_fraction(progress),
                    progress.indeterminate,
                )
            } else if units.total > 0 {
                let resolved = units.translated + units.cached;
                (
                    format!("Translating · {resolved} / {} units", units.total),
                    format!("{} retrying, {} failed", units.retrying, units.failed),
                    resolved as f32 / units.total as f32,
                    false,
                )
            } else {
                (
                    "Translating".to_string(),
                    "Preparing the document translation.".to_string(),
                    0.0,
                    true,
                )
            }
        }
        DocumentTranslationSessionStatus::Cancelling { progress } => progress.as_ref().map_or_else(
            || {
                (
                    "Cancelling".to_string(),
                    "Stopping document translation.".to_string(),
                    0.0,
                    true,
                )
            },
            |progress| {
                (
                    format!("Cancelling · {}", stage_label(progress.stage)),
                    progress.detail.clone(),
                    progress_fraction(progress),
                    progress.indeterminate,
                )
            },
        ),
        DocumentTranslationSessionStatus::Paused {
            progress,
            units,
            cache_available,
        } => {
            let resolved = units.translated + units.cached;
            let detail = if !cache_available {
                "The cached translation is unavailable. Resume to rebuild it.".to_string()
            } else if units.total > 0 {
                format!(
                    "{resolved} / {} units cached · Resume to continue.",
                    units.total
                )
            } else {
                progress.as_ref().map_or_else(
                    || "Resume to continue this translation.".to_string(),
                    |progress| progress.detail.clone(),
                )
            };
            (
                "Translation paused".to_string(),
                detail,
                progress.as_ref().map_or(0.0, progress_fraction),
                false,
            )
        }
        DocumentTranslationSessionStatus::Cancelled => (
            "Cancelled".to_string(),
            "Document translation was cancelled.".to_string(),
            0.0,
            false,
        ),
        DocumentTranslationSessionStatus::Completed { failed_units: 0 } => (
            "Translation complete".to_string(),
            snapshot.last_export_path.as_ref().map_or_else(
                || "Ready to save.".to_string(),
                |path| format!("Saved to {}", path.display()),
            ),
            1.0,
            false,
        ),
        DocumentTranslationSessionStatus::Completed { failed_units } => (
            "Translation complete with warnings".to_string(),
            format!(
                "{failed_units} unit(s) remain untranslated. You can retry them or save the partial result."
            ),
            1.0,
            false,
        ),
        DocumentTranslationSessionStatus::Exporting {
            output_path,
            progress,
        } => progress.as_ref().map_or_else(
            || {
                (
                    "Saving translation".to_string(),
                    format!("Writing {}", output_path.display()),
                    0.0,
                    true,
                )
            },
            |progress| {
                (
                    format!("Saving · {}", stage_label(progress.stage)),
                    progress.detail.clone(),
                    progress_fraction(progress),
                    progress.indeterminate,
                )
            },
        ),
        DocumentTranslationSessionStatus::Failed { stage, message, .. } => (
            format!("Failed · {}", stage_label(*stage)),
            message.to_string(),
            0.0,
            false,
        ),
    }
}

fn job_status_content(snapshot: &DocumentTranslationJobSnapshot) -> (String, String, f32, bool) {
    match &snapshot.status {
        DocumentTranslationJobStatus::Queued => (
            "Queued".to_string(),
            "Waiting for another document translation to finish.".to_string(),
            0.0,
            true,
        ),
        DocumentTranslationJobStatus::Running { progress }
        | DocumentTranslationJobStatus::Cancelling { progress } => {
            let prefix = if matches!(
                &snapshot.status,
                DocumentTranslationJobStatus::Cancelling { .. }
            ) {
                "Cancelling"
            } else {
                "Translating"
            };
            if let Some(progress) = progress {
                let mut status = format!("{prefix} · {}", stage_label(progress.stage));
                if let Some(count) = progress_count(progress) {
                    status.push_str(" · ");
                    status.push_str(&count);
                }
                (
                    status,
                    progress.detail.clone(),
                    progress_fraction(progress),
                    progress.indeterminate,
                )
            } else {
                (
                    prefix.to_string(),
                    "Preparing the document translation.".to_string(),
                    0.0,
                    true,
                )
            }
        }
        DocumentTranslationJobStatus::Cancelled => (
            "Cancelled".to_string(),
            "Document translation was cancelled.".to_string(),
            0.0,
            false,
        ),
        DocumentTranslationJobStatus::Failed { stage, message, .. } => (
            format!("Failed · {}", stage_label(*stage)),
            message.to_string(),
            0.0,
            false,
        ),
        DocumentTranslationJobStatus::Succeeded {
            output_path,
            open_error,
            warnings,
        } => {
            if let Some(error) = open_error {
                (
                    "Could not open output".to_string(),
                    error.to_string(),
                    1.0,
                    false,
                )
            } else {
                let detail = warnings.first().map_or_else(
                    || {
                        output_path
                            .file_name()
                            .and_then(|name| name.to_str())
                            .unwrap_or("Translated PDF")
                            .to_string()
                    },
                    |warning| warning.message.clone(),
                );
                (
                    if warnings.is_empty() {
                        "Complete".to_string()
                    } else {
                        format!("Complete with {} warning(s)", warnings.len())
                    },
                    detail,
                    1.0,
                    false,
                )
            }
        }
        DocumentTranslationJobStatus::Opening { output_path, .. } => (
            "Opening output".to_string(),
            output_path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("Translated PDF")
                .to_string(),
            1.0,
            true,
        ),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TargetLanguageOption {
    pub value: String,
    pub label: String,
}

impl SearchableListItem for TargetLanguageOption {
    type Value = String;

    fn title(&self) -> SharedString {
        self.label.clone().into()
    }

    fn value(&self) -> &Self::Value {
        &self.value
    }

    fn matches(&self, query: &str) -> bool {
        let query = query.to_lowercase();
        self.label.to_lowercase().contains(&query) || self.value.to_lowercase().contains(&query)
    }
}

pub fn target_language_options(selected: &str) -> Vec<TargetLanguageOption> {
    let mut items = TARGET_LANGUAGES
        .iter()
        .map(|language| TargetLanguageOption {
            value: language.code.to_string(),
            label: language.label.to_string(),
        })
        .collect::<Vec<_>>();
    if !items.iter().any(|item| item.value == selected) {
        items.insert(
            0,
            TargetLanguageOption {
                value: selected.to_string(),
                label: selected.to_string(),
            },
        );
    }
    items
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Entity, TestAppContext};
    use std::{cell::RefCell, rc::Rc};

    struct JobEventRecorder {
        events: Rc<RefCell<Vec<DocumentTranslationJobEvent>>>,
    }

    fn test_source_snapshot() -> Arc<DocumentSnapshot> {
        Arc::new(DocumentSnapshot::from_pdf_bytes(
            "/tmp/source.pdf",
            Arc::<[u8]>::from(&b"%PDF-1.7\n%%EOF\n"[..]),
        ))
    }

    fn record_job_events(
        manager: &Entity<DocumentTranslationManager>,
        cx: &mut TestAppContext,
    ) -> (
        Entity<JobEventRecorder>,
        Rc<RefCell<Vec<DocumentTranslationJobEvent>>>,
    ) {
        let events = Rc::new(RefCell::new(Vec::new()));
        let recorded = events.clone();
        let manager = manager.clone();
        let recorder = cx.update(|cx| {
            cx.new(|cx| {
                cx.subscribe(&manager, |this: &mut JobEventRecorder, _, event, _| {
                    this.events.borrow_mut().push(event.clone());
                })
                .detach();
                JobEventRecorder { events: recorded }
            })
        });
        (recorder, events)
    }

    fn show_notification_count(events: &[DocumentTranslationJobEvent]) -> usize {
        events
            .iter()
            .filter(|event| matches!(event, DocumentTranslationJobEvent::ShowNotification { .. }))
            .count()
    }

    fn show_session_notification_count(events: &[DocumentTranslationJobEvent]) -> usize {
        events
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    DocumentTranslationJobEvent::ShowSessionNotification { .. }
                )
            })
            .count()
    }

    fn exporting_session_record(
        id: DocumentTranslationId,
        attached_views: usize,
        notify_when_finished: bool,
    ) -> DocumentTranslationSessionRecord {
        DocumentTranslationSessionRecord {
            snapshot: DocumentTranslationSessionSnapshot {
                id,
                request: PdfTranslationSessionRequest {
                    source_path: "/tmp/source.pdf".into(),
                    source_language: None,
                    target_language: LanguageCode::new("zh-hans").unwrap(),
                },
                mode: DocumentTranslationMode::Auto,
                status: DocumentTranslationSessionStatus::Exporting {
                    output_path: "/tmp/translated.pdf".into(),
                    progress: None,
                },
                prepared_document: None,
                resolved_units: Arc::new(Vec::new()),
                failures: Arc::new(Vec::new()),
                page_previews: Arc::new(BTreeMap::new()),
                page_failures: Arc::new(Vec::new()),
                ready_prefix: 0,
                original_unit_ids: Arc::new(HashSet::new()),
                attached_views,
                last_export_path: None,
                last_export_error: None,
                cache_key: None,
            },
            source_snapshot: test_source_snapshot(),
            settings: Settings::default(),
            cancellation: CancellationToken::new(),
            task: None,
            result: None,
            pending_restart: None,
            pending_export: None,
            notify_when_finished,
            prepared_metadata: None,
            prepared_pages: BTreeMap::new(),
            prepared_page_count: 0,
            preview_generation: 0,
            pending_cache_restore: false,
        }
    }

    fn remove_notification_count(events: &[DocumentTranslationJobEvent]) -> usize {
        events
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    DocumentTranslationJobEvent::RemoveNotification { .. }
                )
            })
            .count()
    }

    #[test]
    fn target_language_options_are_searchable_and_preserve_custom_values() {
        let options = target_language_options("custom-language");
        assert_eq!(options[0].value, "custom-language");
        assert_eq!(options[0].label, "custom-language");

        let english = options
            .iter()
            .find(|option| option.value == "en")
            .expect("English should be present in the target-language catalog");
        assert!(english.matches("english"));
        assert!(english.matches("EN"));
    }

    #[test]
    fn session_failure_count_deduplicates_ocr_page_diagnostics() {
        let mut snapshot = DocumentTranslationSessionSnapshot {
            id: 1,
            request: PdfTranslationSessionRequest {
                source_path: "/tmp/source.pdf".into(),
                source_language: None,
                target_language: LanguageCode::new("zh-hans").unwrap(),
            },
            mode: DocumentTranslationMode::Auto,
            status: DocumentTranslationSessionStatus::Completed { failed_units: 0 },
            prepared_document: None,
            resolved_units: Arc::new(Vec::new()),
            failures: Arc::new(vec![TranslationFailureDiagnostic {
                id: "ocr-page-0001".to_string(),
                phase: TranslationFailurePhase::Ocr,
                reason: "OCR failed; source page preserved".to_string(),
                attempts: 1,
                retryable: true,
                category: Some("ocr".to_string()),
                status: None,
                member_block_ids: Vec::new(),
                role: None,
                source_excerpt: None,
                rejected_output_excerpt: None,
            }]),
            page_previews: Arc::new(BTreeMap::new()),
            page_failures: Arc::new(vec![
                PdfTranslationPageFailure {
                    page_index: 0,
                    phase: TranslationFailurePhase::Ocr,
                    message: "OCR failed; source page preserved".to_string(),
                    retryable: true,
                },
                PdfTranslationPageFailure {
                    page_index: 1,
                    phase: TranslationFailurePhase::Preview,
                    message: "preview failed".to_string(),
                    retryable: true,
                },
            ]),
            ready_prefix: 2,
            original_unit_ids: Arc::new(HashSet::new()),
            cache_key: None,
            attached_views: 1,
            last_export_path: None,
            last_export_error: None,
        };

        assert_eq!(snapshot.failure_count(), 2);
        assert!(snapshot.has_failures());
        snapshot.failures = Arc::new(Vec::new());
        snapshot.page_failures = Arc::new(Vec::new());
        assert!(!snapshot.has_failures());
    }

    #[test]
    fn background_activity_names_the_translation_and_document() {
        let snapshot = running_record(1).snapshot;

        let entry = background_task_entry(&snapshot, None);

        assert_eq!(entry.title, "Translate source.pdf");
    }

    fn running_record(id: DocumentTranslationId) -> DocumentTranslationJobRecord {
        DocumentTranslationJobRecord {
            snapshot: DocumentTranslationJobSnapshot {
                id,
                request: PdfTranslationRequest {
                    source_path: "/tmp/source.pdf".into(),
                    output_path: "/tmp/translated.pdf".into(),
                    source_language: None,
                    target_language: glyphst_translation::LanguageCode::new("zh-hans").unwrap(),
                },
                open_mode: DocumentTranslationOpenMode::Split,
                status: DocumentTranslationJobStatus::Running { progress: None },
            },
            settings: Settings::default(),
            cancellation: CancellationToken::new(),
            task: None,
        }
    }

    #[gpui::test]
    fn preflight_errors_remain_until_activity_notice_is_dismissed(cx: &mut TestAppContext) {
        cx.update(glyphst_ui::activity::init);
        let center = cx.update(|cx| ActivityCenter::global(cx));
        let manager = cx.update(|cx| cx.new(DocumentTranslationManager::new));
        let (_recorder, events) = record_job_events(&manager, cx);

        let (id, notice_id) = manager.update(cx, |manager, cx| {
            let id = manager.report_preflight_error(
                "/tmp/source.pdf".into(),
                DocumentTranslationOpenMode::Document,
                "Invalid target language",
                cx,
            );

            assert_eq!(manager.batch.records.len(), 1);
            manager.retry(id, cx);
            assert!(matches!(
                manager.job(id).map(|job| &job.snapshot.status),
                Some(DocumentTranslationJobStatus::Failed {
                    retryable: false,
                    ..
                })
            ));

            manager.notification_closed(id, cx);
            assert_eq!(manager.batch.records.len(), 1);
            assert_eq!(center.read(cx).active_task_count(), 0);
            assert_eq!(center.read(cx).notice_count(), 1);
            (id, manager.batch.activity_notices[&id])
        });
        center.update(cx, |center, cx| center.dismiss_notice(notice_id, cx));
        assert!(cx.update(|cx| manager.read(cx).job(id).is_none()));
        cx.run_until_parked();
        assert_eq!(show_notification_count(&events.borrow()), 1);
    }

    #[gpui::test]
    fn live_session_opens_only_for_split_modes(cx: &mut TestAppContext) {
        cx.update(glyphst_ui::activity::init);
        let manager = cx.update(|cx| cx.new(DocumentTranslationManager::new));
        let (_recorder, events) = record_job_events(&manager, cx);
        let request = PdfTranslationSessionRequest {
            source_path: "/tmp/source.pdf".into(),
            source_language: None,
            target_language: LanguageCode::new("zh-hans").unwrap(),
        };

        let (foreground, background) = manager.update(cx, |manager, cx| {
            let foreground = manager.submit_session(
                LiveTranslationSubmission {
                    settings: Settings::default(),
                    request: request.clone(),
                    source_snapshot: test_source_snapshot(),
                    mode: DocumentTranslationMode::Auto,
                },
                cx,
            );
            let background = manager.submit_session(
                LiveTranslationSubmission {
                    settings: Settings::default(),
                    request,
                    source_snapshot: test_source_snapshot(),
                    mode: DocumentTranslationMode::Background,
                },
                cx,
            );
            (foreground, background)
        });
        cx.run_until_parked();

        let events = events.borrow();
        assert!(events.iter().any(|event| matches!(
            event,
            DocumentTranslationJobEvent::OpenSession { id, .. } if *id == foreground
        )));
        assert!(!events.iter().any(|event| matches!(
            event,
            DocumentTranslationJobEvent::OpenSession { id, .. } if *id == background
        )));
    }

    #[gpui::test]
    fn restored_session_without_cache_waits_paused_for_explicit_resume(cx: &mut TestAppContext) {
        cx.update(glyphst_ui::activity::init);
        let manager = cx.update(|cx| cx.new(DocumentTranslationManager::new));
        let (_recorder, events) = record_job_events(&manager, cx);
        let id = manager.update(cx, |manager, cx| {
            manager.restore_session(
                RestoredTranslationSubmission {
                    settings: Settings::default(),
                    request: PdfTranslationSessionRequest {
                        source_path: "/tmp/source.pdf".into(),
                        source_language: Some(LanguageCode::new("en").unwrap()),
                        target_language: LanguageCode::new("zh-hans").unwrap(),
                    },
                    source_snapshot: test_source_snapshot(),
                    mode: DocumentTranslationMode::Auto,
                    cache_key: None,
                    original_unit_ids: HashSet::from(["unit-1".to_string()]),
                },
                cx,
            )
        });
        cx.run_until_parked();

        manager.read_with(cx, |manager, _| {
            let session = manager.session_snapshot(id).unwrap();
            assert!(matches!(
                session.status,
                DocumentTranslationSessionStatus::Paused {
                    cache_available: false,
                    ..
                }
            ));
            assert!(session.can_resume());
            assert!(session.original_unit_ids.contains("unit-1"));
            assert_eq!(manager.live.active_id, None);
        });
        assert!(!events.borrow().iter().any(|event| matches!(
            event,
            DocumentTranslationJobEvent::OpenSession { id: event_id, .. } if *event_id == id
        )));
    }

    #[gpui::test]
    fn progress_stays_in_activity_center_and_success_notifies_once(cx: &mut TestAppContext) {
        cx.update(glyphst_ui::activity::init);
        let center = cx.update(|cx| ActivityCenter::global(cx));
        let manager = cx.update(|cx| cx.new(DocumentTranslationManager::new));
        let (_recorder, events) = record_job_events(&manager, cx);

        manager.update(cx, |manager, cx| {
            let id = 13;
            manager.batch.records.push_back(running_record(id));
            manager.batch.active_id = Some(id);
            manager.register_background_task(id, cx);
            manager.update_progress(
                id,
                PdfTranslationProgress {
                    stage: PdfTranslationStage::Translating,
                    detail: "Translating page 1".to_string(),
                    completed: Some(1),
                    total: Some(1),
                    translated: Some(1),
                    cached: Some(0),
                    retrying: Some(0),
                    failed: Some(0),
                    overall_fraction: 1.0,
                    indeterminate: false,
                },
                cx,
            );
            assert_eq!(show_notification_count(&events.borrow()), 0);
            assert_eq!(center.read(cx).active_task_count(), 1);

            manager.finish(
                id,
                Ok(PdfTranslationResult {
                    output_path: "/tmp/translated.pdf".into(),
                    manifest_path: "/tmp/manifest.json".into(),
                    job_directory: "/tmp/job".into(),
                    fingerprint: "test".to_string(),
                    warnings: Vec::new(),
                }),
                cx,
            );

            assert!(matches!(
                manager.job(id).map(|job| &job.snapshot.status),
                Some(DocumentTranslationJobStatus::Succeeded { .. })
            ));
            assert_eq!(center.read(cx).active_task_count(), 0);
        });
        {
            let events = events.borrow();
            assert_eq!(show_notification_count(&events), 1);
            assert_eq!(remove_notification_count(&events), 0);
            assert!(!events.iter().any(|event| matches!(
                event,
                DocumentTranslationJobEvent::OpenDocument { .. }
                    | DocumentTranslationJobEvent::OpenSplit { .. }
            )));
        }

        cx.run_until_parked();
        cx.executor()
            .advance_clock(glyphst_ui::LONG_FEEDBACK_DURATION);
        cx.run_until_parked();
        assert_eq!(remove_notification_count(&events.borrow()), 0);
    }

    #[gpui::test]
    fn explicit_session_export_never_replays_translation_completion_notification(
        cx: &mut TestAppContext,
    ) {
        cx.update(glyphst_ui::activity::init);
        let manager = cx.update(|cx| cx.new(DocumentTranslationManager::new));
        let (_recorder, events) = record_job_events(&manager, cx);

        manager.update(cx, |manager, cx| {
            for (id, attached_views, notify_when_finished) in [(31, 1, false), (32, 0, true)] {
                manager.live.records.push_back(exporting_session_record(
                    id,
                    attached_views,
                    notify_when_finished,
                ));
                manager.live.active_id = Some(id);
                manager.finish_session_export(
                    id,
                    Ok(PdfTranslationResult {
                        output_path: format!("/tmp/translated-{id}.pdf").into(),
                        manifest_path: format!("/tmp/manifest-{id}.json").into(),
                        job_directory: format!("/tmp/job-{id}").into(),
                        fingerprint: format!("export-{id}"),
                        warnings: Vec::new(),
                    }),
                    cx,
                );
                let session = manager.session(id).unwrap();
                assert!(matches!(
                    session.snapshot.status,
                    DocumentTranslationSessionStatus::Completed { .. }
                ));
                assert_eq!(
                    session.snapshot.last_export_path.as_deref(),
                    Some(std::path::Path::new(&format!("/tmp/translated-{id}.pdf")))
                );
            }
        });
        cx.run_until_parked();

        assert_eq!(show_session_notification_count(&events.borrow()), 0);
    }

    #[gpui::test]
    fn failed_output_open_returns_to_terminal_notification(cx: &mut TestAppContext) {
        cx.update(glyphst_ui::activity::init);
        let manager = cx.update(|cx| cx.new(DocumentTranslationManager::new));
        let (_recorder, events) = record_job_events(&manager, cx);

        manager.update(cx, |manager, cx| {
            let id = 17;
            let mut record = running_record(id);
            record.snapshot.status = DocumentTranslationJobStatus::Succeeded {
                output_path: "/tmp/translated.pdf".into(),
                open_error: None,
                warnings: Vec::new(),
            };
            manager.batch.records.push_back(record);

            manager.publish_terminal_notification(id, cx);
            assert_eq!(ActivityCenter::global(cx).read(cx).notice_count(), 1);
            manager.open(id, cx);
            assert_eq!(ActivityCenter::global(cx).read(cx).notice_count(), 0);
            assert!(matches!(
                manager.job(id).map(|job| &job.snapshot.status),
                Some(DocumentTranslationJobStatus::Opening { .. })
            ));

            manager.finish_open(id, Err("Could not read the output".into()), cx);
            assert!(matches!(
                manager.job(id).map(|job| &job.snapshot.status),
                Some(DocumentTranslationJobStatus::Succeeded {
                    open_error: Some(_),
                    ..
                })
            ));
        });
        cx.run_until_parked();
        assert_eq!(show_notification_count(&events.borrow()), 2);
    }

    #[gpui::test]
    fn queued_cancellation_removes_job_without_notification(cx: &mut TestAppContext) {
        cx.update(glyphst_ui::activity::init);
        let center = cx.update(|cx| ActivityCenter::global(cx));
        let manager = cx.update(|cx| cx.new(DocumentTranslationManager::new));
        let (_recorder, events) = record_job_events(&manager, cx);

        manager.update(cx, |manager, cx| {
            let id = 19;
            let mut record = running_record(id);
            record.snapshot.status = DocumentTranslationJobStatus::Queued;
            manager.batch.records.push_back(record);
            manager.register_background_task(id, cx);

            manager.cancel(id, cx);

            assert!(manager.job(id).is_none());
            assert_eq!(center.read(cx).active_task_count(), 0);
        });
        cx.run_until_parked();
        assert_eq!(show_notification_count(&events.borrow()), 0);
    }

    #[gpui::test]
    fn running_cancellation_finishes_silently(cx: &mut TestAppContext) {
        cx.update(glyphst_ui::activity::init);
        let center = cx.update(|cx| ActivityCenter::global(cx));
        let manager = cx.update(|cx| cx.new(DocumentTranslationManager::new));
        let (_recorder, events) = record_job_events(&manager, cx);

        manager.update(cx, |manager, cx| {
            let id = 21;
            manager.batch.records.push_back(running_record(id));
            manager.batch.active_id = Some(id);
            manager.register_background_task(id, cx);

            manager.cancel(id, cx);
            assert_eq!(center.read(cx).active_task_count(), 1);

            manager.finish(
                id,
                Ok(PdfTranslationResult {
                    output_path: "/tmp/translated.pdf".into(),
                    manifest_path: "/tmp/manifest.json".into(),
                    job_directory: "/tmp/job".into(),
                    fingerprint: "completed-after-cancel".to_string(),
                    warnings: Vec::new(),
                }),
                cx,
            );

            assert!(manager.job(id).is_none());
            assert_eq!(center.read(cx).active_task_count(), 0);
        });
        cx.run_until_parked();
        assert_eq!(show_notification_count(&events.borrow()), 0);
    }

    #[test]
    fn notification_actions_only_apply_to_terminal_results() {
        let running = DocumentTranslationJobStatus::Running { progress: None };
        assert!(job_actions(&running).is_empty());
        assert!(
            job_actions(&DocumentTranslationJobStatus::Cancelling { progress: None }).is_empty()
        );
        assert_eq!(
            job_actions(&DocumentTranslationJobStatus::Failed {
                stage: PdfTranslationStage::Translating,
                message: "failed".into(),
                detailed_message: "detailed failure".into(),
                retryable: true,
            }),
            vec![
                DocumentTranslationJobAction::Retry,
                DocumentTranslationJobAction::CopyError,
            ]
        );
        assert_eq!(
            job_actions(&DocumentTranslationJobStatus::Succeeded {
                output_path: "/tmp/translated.pdf".into(),
                open_error: None,
                warnings: Vec::new(),
            }),
            vec![DocumentTranslationJobAction::Open]
        );
        let warning = PdfTranslationWarning {
            code: "content_review_failed".to_string(),
            message: "Ambiguous text was preserved.".to_string(),
            affected_block_ids: vec!["block-1".to_string()],
        };
        let warning_status = DocumentTranslationJobStatus::Succeeded {
            output_path: "/tmp/translated.pdf".into(),
            open_error: None,
            warnings: vec![warning],
        };
        assert_eq!(
            job_actions(&warning_status),
            vec![
                DocumentTranslationJobAction::Open,
                DocumentTranslationJobAction::Retry,
                DocumentTranslationJobAction::CopyError,
            ]
        );
        let mut snapshot = running_record(1).snapshot;
        snapshot.status = warning_status;
        let (status, detail, _, _) = job_status_content(&snapshot);
        assert_eq!(status, "Complete with 1 warning(s)");
        assert_eq!(detail, "Ambiguous text was preserved.");
    }

    #[gpui::test]
    fn copy_error_uses_detailed_failure_message(cx: &mut TestAppContext) {
        let manager = cx.update(|cx| cx.new(DocumentTranslationManager::new));

        manager.update(cx, |manager, cx| {
            let id = 22;
            let mut record = running_record(id);
            record.snapshot.status = DocumentTranslationJobStatus::Failed {
                stage: PdfTranslationStage::Translating,
                message: "7 translation units remain unresolved".into(),
                detailed_message: "unit-p015-b0002 · page 15 · source excerpt".into(),
                retryable: true,
            };
            manager.batch.records.push_back(record);
            manager.copy_error(id, cx);
        });

        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some("unit-p015-b0002 · page 15 · source excerpt".to_string())
        );
    }

    #[gpui::test]
    fn retry_uses_current_translation_languages(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let mut settings = SettingsStore::memory();
            settings.settings.translation.source_language = Some("fr".to_string());
            settings.settings.translation.target_language =
                glyphst_settings::TranslationTargetLanguage::Explicit("de".to_string());
            cx.set_global(settings);
        });
        let manager = cx.update(|cx| cx.new(DocumentTranslationManager::new));

        manager.update(cx, |manager, cx| {
            let id = 23;
            let mut record = running_record(id);
            record.snapshot.status = DocumentTranslationJobStatus::Failed {
                stage: PdfTranslationStage::Translating,
                message: "temporary failure".into(),
                detailed_message: "temporary failure".into(),
                retryable: true,
            };
            manager.batch.records.push_back(record);

            manager.retry(id, cx);

            let request = &manager.job(id).unwrap().snapshot.request;
            assert_eq!(
                request
                    .source_language
                    .as_ref()
                    .map(|language| language.as_str()),
                Some("fr")
            );
            assert_eq!(request.target_language.as_str(), "de");
        });
    }
}
