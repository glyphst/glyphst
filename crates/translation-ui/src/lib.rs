pub use glyphst_translation as service;

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;

use futures::{StreamExt as _, channel::mpsc};
use glyphst_pdf_translation_app::{
    CancellationToken, PdfTranslationError, PdfTranslationProgress, PdfTranslationRequest,
    PdfTranslationResult, PdfTranslationService, PdfTranslationStage,
};
use glyphst_settings::{Settings, SettingsStore};
use glyphst_translation::TARGET_LANGUAGES;
use glyphst_ui::activity::{
    ActivityCenter, BackgroundTaskEntry, BackgroundTaskId, CenterAction, TaskProgress,
};
use gpui::{
    Context, EventEmitter, IntoElement, ParentElement as _, SharedString, Styled, Task, WeakEntity,
    div, prelude::*,
};
use gpui_component::{
    ActiveTheme as _, Sizable as _, Size,
    button::{Button, ButtonVariants as _},
    notification::{Notification, NotificationType},
    progress::Progress,
    searchable_list::SearchableListItem,
    v_flex,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DocumentTranslationJobAction {
    Cancel,
    Minimize,
    Retry,
    Open,
    Dismiss,
}

impl DocumentTranslationJobAction {
    fn id(self) -> &'static str {
        match self {
            Self::Cancel => "cancel",
            Self::Minimize => "minimize",
            Self::Retry => "retry",
            Self::Open => "open",
            Self::Dismiss => "dismiss",
        }
    }

    fn label(self, open_mode: DocumentTranslationOpenMode) -> &'static str {
        match self {
            Self::Cancel => "Cancel",
            Self::Minimize => "Minimize",
            Self::Retry => "Retry",
            Self::Open => match open_mode {
                DocumentTranslationOpenMode::Document => "Open output",
                DocumentTranslationOpenMode::Split => "Open split",
            },
            Self::Dismiss => "Dismiss",
        }
    }
}

pub type DocumentTranslationJobId = u64;

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
        retryable: bool,
    },
    Succeeded {
        output_path: PathBuf,
        open_error: Option<SharedString>,
    },
    Opening {
        output_path: PathBuf,
    },
}

fn job_actions(
    status: &DocumentTranslationJobStatus,
    notification: bool,
) -> Vec<DocumentTranslationJobAction> {
    match status {
        DocumentTranslationJobStatus::Queued | DocumentTranslationJobStatus::Running { .. } => {
            let mut actions = vec![DocumentTranslationJobAction::Cancel];
            if notification {
                actions.push(DocumentTranslationJobAction::Minimize);
            }
            actions
        }
        DocumentTranslationJobStatus::Cancelling { .. } if notification => {
            vec![DocumentTranslationJobAction::Minimize]
        }
        DocumentTranslationJobStatus::Cancelling { .. }
        | DocumentTranslationJobStatus::Opening { .. } => Vec::new(),
        DocumentTranslationJobStatus::Cancelled => vec![
            DocumentTranslationJobAction::Retry,
            DocumentTranslationJobAction::Dismiss,
        ],
        DocumentTranslationJobStatus::Failed { retryable, .. } => {
            let mut actions = Vec::new();
            if *retryable {
                actions.push(DocumentTranslationJobAction::Retry);
            }
            actions.push(DocumentTranslationJobAction::Dismiss);
            actions
        }
        DocumentTranslationJobStatus::Succeeded { .. } => vec![
            DocumentTranslationJobAction::Open,
            DocumentTranslationJobAction::Dismiss,
        ],
    }
}

#[derive(Clone, Debug)]
pub struct DocumentTranslationJobSnapshot {
    pub id: DocumentTranslationJobId,
    pub request: PdfTranslationRequest,
    pub open_mode: DocumentTranslationOpenMode,
    pub status: DocumentTranslationJobStatus,
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
        id: DocumentTranslationJobId,
        path: PathBuf,
    },
    OpenSplit {
        id: DocumentTranslationJobId,
        source_path: PathBuf,
        translated_path: PathBuf,
    },
    ShowNotification {
        snapshot: DocumentTranslationJobSnapshot,
    },
    RemoveNotification {
        id: DocumentTranslationJobId,
    },
}

/// Stable notification kind used to replace a translation notification as its
/// job progresses.
pub struct DocumentTranslationNotification;

struct DocumentTranslationJobRecord {
    snapshot: DocumentTranslationJobSnapshot,
    settings: Settings,
    cancellation: CancellationToken,
    foregrounded: bool,
    task: Option<Task<()>>,
}

pub struct DocumentTranslationJobManager {
    jobs: VecDeque<DocumentTranslationJobRecord>,
    next_id: DocumentTranslationJobId,
    active_job_id: Option<DocumentTranslationJobId>,
    popup_job_id: Option<DocumentTranslationJobId>,
    background_tasks: HashMap<DocumentTranslationJobId, BackgroundTaskId>,
}

impl EventEmitter<DocumentTranslationJobEvent> for DocumentTranslationJobManager {}

impl DocumentTranslationJobManager {
    pub fn new(cx: &mut Context<Self>) -> Self {
        cx.on_release(|this, cx| {
            for job in &this.jobs {
                job.cancellation.cancel();
            }
            if let Some(center) = ActivityCenter::try_global(cx) {
                for task_id in this.background_tasks.values().copied() {
                    center.update(cx, |center, cx| center.remove_task(task_id, cx));
                }
            }
        })
        .detach();
        Self {
            jobs: VecDeque::new(),
            next_id: 1,
            active_job_id: None,
            popup_job_id: None,
            background_tasks: HashMap::new(),
        }
    }

    pub fn submit(
        &mut self,
        settings: Settings,
        request: PdfTranslationRequest,
        open_mode: DocumentTranslationOpenMode,
        cx: &mut Context<Self>,
    ) -> DocumentTranslationJobId {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        self.jobs.push_back(DocumentTranslationJobRecord {
            snapshot: DocumentTranslationJobSnapshot {
                id,
                request,
                open_mode,
                status: DocumentTranslationJobStatus::Queued,
            },
            settings,
            cancellation: CancellationToken::new(),
            foregrounded: true,
            task: None,
        });
        self.register_background_task(id, cx);
        self.start_next(cx);
        if self.job(id).is_some_and(|job| job.snapshot.is_active()) {
            self.show_popup(id, cx);
        }
        cx.notify();
        id
    }

    pub fn report_preflight_error(
        &mut self,
        source_path: PathBuf,
        open_mode: DocumentTranslationOpenMode,
        message: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) -> DocumentTranslationJobId {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        let target_language =
            glyphst_translation::LanguageCode::new("und").expect("und is a valid language code");
        self.jobs.push_back(DocumentTranslationJobRecord {
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
                    message: message.into(),
                    retryable: false,
                },
            },
            settings: Settings::default(),
            cancellation: CancellationToken::new(),
            foregrounded: false,
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
        output_path: PathBuf,
        cx: &mut Context<Self>,
    ) -> DocumentTranslationJobId {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        self.jobs.push_back(DocumentTranslationJobRecord {
            snapshot: DocumentTranslationJobSnapshot {
                id,
                request,
                open_mode,
                status: DocumentTranslationJobStatus::Succeeded {
                    output_path,
                    open_error: None,
                },
            },
            settings,
            cancellation: CancellationToken::new(),
            foregrounded: true,
            task: None,
        });
        self.open(id, cx);
        id
    }

    pub fn has_job_for_source(&self, source_path: &std::path::Path) -> bool {
        self.jobs
            .iter()
            .any(|job| job.snapshot.request.source_path == source_path && job.snapshot.is_active())
    }

    pub fn reveal_for_source(
        &mut self,
        source_path: &std::path::Path,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(id) = self
            .jobs
            .iter()
            .rev()
            .find(|job| job.snapshot.request.source_path == source_path && job.snapshot.is_active())
            .map(|job| job.snapshot.id)
        else {
            return false;
        };
        self.show_popup(id, cx);
        cx.notify();
        true
    }

    pub fn cancel_for_source(&mut self, source_path: &std::path::Path, cx: &mut Context<Self>) {
        let Some(id) = self
            .jobs
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
        self.jobs.iter().map(|job| job.snapshot.clone()).collect()
    }

    pub fn cancel(&mut self, id: DocumentTranslationJobId, cx: &mut Context<Self>) {
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
                    job.foregrounded = false;
                }
                DocumentTranslationJobStatus::Cancelling { .. }
                | DocumentTranslationJobStatus::Cancelled
                | DocumentTranslationJobStatus::Failed { .. }
                | DocumentTranslationJobStatus::Succeeded { .. }
                | DocumentTranslationJobStatus::Opening { .. } => {}
            }
        }
        if self.popup_job_id == Some(id) {
            self.popup_job_id = None;
        }
        self.remove_job_notification(id, cx);
        if remove_job {
            self.jobs.retain(|job| job.snapshot.id != id);
            self.remove_background_task(id, cx);
            self.start_next(cx);
        } else {
            self.sync_background_task(id, cx);
        }
        cx.notify();
    }

    pub fn minimize(&mut self, id: DocumentTranslationJobId, cx: &mut Context<Self>) {
        if let Some(job) = self.job_mut(id) {
            job.foregrounded = false;
        }
        if self.popup_job_id == Some(id) {
            self.popup_job_id = None;
        }
        self.remove_job_notification(id, cx);
        cx.notify();
    }

    pub fn retry(&mut self, id: DocumentTranslationJobId, cx: &mut Context<Self>) {
        self.remove_job_notification(id, cx);
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
            let target =
                glyphst_translation::LanguageCode::new(&settings.translation.target_language)
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
            );
            if !can_retry {
                return;
            }
            if let Some(Err(message)) = &latest_languages {
                job.snapshot.status = DocumentTranslationJobStatus::Failed {
                    stage: PdfTranslationStage::Preparing,
                    message: message.clone().into(),
                    retryable: false,
                };
                job.foregrounded = false;
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
                job.foregrounded = true;
            }
        }
        if invalid_languages {
            self.publish_terminal_notification(id, cx);
            cx.notify();
            return;
        }
        self.register_background_task(id, cx);
        self.start_next(cx);
        if self.job(id).is_some_and(|job| job.snapshot.is_active()) {
            self.show_popup(id, cx);
        }
        cx.notify();
    }

    pub fn dismiss(&mut self, id: DocumentTranslationJobId, cx: &mut Context<Self>) {
        let can_dismiss = self.job(id).is_some_and(|job| !job.snapshot.is_active());
        if !can_dismiss {
            return;
        }
        self.remove_job_notification(id, cx);
        self.remove_background_task(id, cx);
        self.jobs.retain(|job| job.snapshot.id != id);
        if self.popup_job_id == Some(id) {
            self.popup_job_id = None;
        }
        cx.notify();
    }

    pub fn open(&mut self, id: DocumentTranslationJobId, cx: &mut Context<Self>) {
        self.remove_job_notification(id, cx);
        let Some((open_mode, source_path, output_path)) = self.job(id).and_then(|job| {
            let DocumentTranslationJobStatus::Succeeded { output_path, .. } = &job.snapshot.status
            else {
                return None;
            };
            Some((
                job.snapshot.open_mode,
                job.snapshot.request.source_path.clone(),
                output_path.clone(),
            ))
        }) else {
            return;
        };
        if let Some(job) = self.job_mut(id) {
            job.snapshot.status = DocumentTranslationJobStatus::Opening {
                output_path: output_path.clone(),
            };
        }
        if self.popup_job_id == Some(id) {
            self.popup_job_id = None;
        }
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

    pub fn finish_open(
        &mut self,
        id: DocumentTranslationJobId,
        result: Result<(), SharedString>,
        cx: &mut Context<Self>,
    ) {
        let mut publish_error = false;
        match result {
            Ok(()) => {
                self.remove_background_task(id, cx);
                self.jobs.retain(|job| job.snapshot.id != id);
                if self.popup_job_id == Some(id) {
                    self.popup_job_id = None;
                }
            }
            Err(message) => {
                let Some(job) = self.job_mut(id) else {
                    return;
                };
                let DocumentTranslationJobStatus::Opening { output_path } = &job.snapshot.status
                else {
                    return;
                };
                let output_path = output_path.clone();
                job.snapshot.status = DocumentTranslationJobStatus::Succeeded {
                    output_path,
                    open_error: Some(message),
                };
                job.foregrounded = false;
                publish_error = true;
            }
        }
        if publish_error {
            self.remove_background_task(id, cx);
            self.publish_terminal_notification(id, cx);
        }
        cx.notify();
    }

    fn start_next(&mut self, cx: &mut Context<Self>) {
        if self.active_job_id.is_some() {
            return;
        }
        let Some(index) = self
            .jobs
            .iter()
            .position(|job| matches!(job.snapshot.status, DocumentTranslationJobStatus::Queued))
        else {
            return;
        };
        if !cx.has_global::<PdfTranslationService>() {
            let id = self.jobs[index].snapshot.id;
            self.jobs[index].snapshot.status = DocumentTranslationJobStatus::Failed {
                stage: PdfTranslationStage::Preparing,
                message: "Document translation is unavailable.".into(),
                retryable: false,
            };
            self.jobs[index].foregrounded = false;
            self.remove_background_task(id, cx);
            self.publish_terminal_notification(id, cx);
            return;
        }

        let id = self.jobs[index].snapshot.id;
        let settings = self.jobs[index].settings.clone();
        let request = self.jobs[index].snapshot.request.clone();
        let cancellation = self.jobs[index].cancellation.clone();
        let service = cx.global::<PdfTranslationService>().clone();
        let (progress_tx, mut progress_rx) = mpsc::unbounded::<PdfTranslationProgress>();
        self.jobs[index].snapshot.status = DocumentTranslationJobStatus::Running { progress: None };
        self.active_job_id = Some(id);
        self.sync_background_task(id, cx);
        self.sync_popup_notification(id, cx);

        let task = cx.spawn(async move |this: gpui::WeakEntity<Self>, cx| {
            let result_task = cx.background_spawn(async move {
                service.run(settings, request, cancellation, move |progress| {
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
        self.jobs[index].task = Some(task);
        cx.notify();
    }

    fn update_progress(
        &mut self,
        id: DocumentTranslationJobId,
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
        self.sync_popup_notification(id, cx);
        cx.notify();
    }

    fn finish(
        &mut self,
        id: DocumentTranslationJobId,
        result: Result<PdfTranslationResult, PdfTranslationError>,
        cx: &mut Context<Self>,
    ) {
        if self.active_job_id != Some(id) {
            return;
        }
        self.active_job_id = None;
        let was_cancelled = self.job(id).is_some_and(|job| {
            job.cancellation.is_cancelled()
                || matches!(
                    job.snapshot.status,
                    DocumentTranslationJobStatus::Cancelling { .. }
                )
        });
        if was_cancelled {
            self.remove_background_task(id, cx);
            self.jobs.retain(|job| job.snapshot.id != id);
            if self.popup_job_id == Some(id) {
                self.popup_job_id = None;
            }
            self.start_next(cx);
            cx.notify();
            return;
        }

        let auto_open = {
            let Some(job) = self.job_mut(id) else {
                self.start_next(cx);
                return;
            };
            job.task = None;
            let was_foregrounded = job.foregrounded;
            match result {
                Ok(result) => {
                    job.snapshot.status = DocumentTranslationJobStatus::Succeeded {
                        output_path: result.output_path,
                        open_error: None,
                    };
                    was_foregrounded
                }
                Err(error) => {
                    job.snapshot.status = DocumentTranslationJobStatus::Failed {
                        stage: error.stage,
                        message: error.message.into(),
                        retryable: error.retryable,
                    };
                    job.foregrounded = false;
                    false
                }
            }
        };
        if auto_open {
            self.open(id, cx);
            self.popup_job_id = None;
            self.sync_background_task(id, cx);
        } else {
            self.remove_background_task(id, cx);
            if self.popup_job_id == Some(id) {
                self.popup_job_id = None;
            }
            self.publish_terminal_notification(id, cx);
        }
        self.start_next(cx);
        cx.notify();
    }

    fn job(&self, id: DocumentTranslationJobId) -> Option<&DocumentTranslationJobRecord> {
        self.jobs.iter().find(|job| job.snapshot.id == id)
    }

    fn job_mut(
        &mut self,
        id: DocumentTranslationJobId,
    ) -> Option<&mut DocumentTranslationJobRecord> {
        self.jobs.iter_mut().find(|job| job.snapshot.id == id)
    }

    fn register_background_task(&mut self, id: DocumentTranslationJobId, cx: &mut Context<Self>) {
        if self.background_tasks.contains_key(&id) {
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
        self.background_tasks.insert(id, task_id);
    }

    fn sync_background_task(&mut self, id: DocumentTranslationJobId, cx: &mut Context<Self>) {
        let Some(task_id) = self.background_tasks.get(&id).copied() else {
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

    fn remove_background_task(&mut self, id: DocumentTranslationJobId, cx: &mut Context<Self>) {
        let Some(task_id) = self.background_tasks.remove(&id) else {
            return;
        };
        if let Some(center) = ActivityCenter::try_global(cx) {
            center.update(cx, |center, cx| center.remove_task(task_id, cx));
        }
    }

    fn remove_job_notification(&mut self, id: DocumentTranslationJobId, cx: &mut Context<Self>) {
        cx.emit(DocumentTranslationJobEvent::RemoveNotification { id });
    }

    fn publish_terminal_notification(
        &mut self,
        id: DocumentTranslationJobId,
        cx: &mut Context<Self>,
    ) {
        let Some(snapshot) = self.job(id).map(|job| job.snapshot.clone()) else {
            return;
        };
        if self.popup_job_id == Some(id) {
            self.popup_job_id = None;
        }
        self.remove_job_notification(id, cx);
        cx.emit(DocumentTranslationJobEvent::ShowNotification { snapshot });
    }

    fn show_popup(&mut self, id: DocumentTranslationJobId, cx: &mut Context<Self>) {
        if let Some(previous_id) = self.popup_job_id
            && previous_id != id
            && let Some(previous) = self.job_mut(previous_id)
        {
            previous.foregrounded = false;
            self.remove_job_notification(previous_id, cx);
        }
        if let Some(job) = self.job_mut(id) {
            job.foregrounded = true;
        }
        self.popup_job_id = Some(id);
        self.sync_popup_notification(id, cx);
    }

    fn sync_popup_notification(&mut self, id: DocumentTranslationJobId, cx: &mut Context<Self>) {
        if self.popup_job_id != Some(id) {
            return;
        }
        let Some(snapshot) = self.job(id).map(|job| job.snapshot.clone()) else {
            return;
        };
        cx.emit(DocumentTranslationJobEvent::ShowNotification { snapshot });
    }

    fn notification_closed(
        &mut self,
        id: DocumentTranslationJobId,
        terminal: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(job) = self.job(id) else {
            return;
        };
        if terminal {
            if !job.snapshot.is_active() {
                self.remove_background_task(id, cx);
                self.jobs.retain(|job| job.snapshot.id != id);
                if self.popup_job_id == Some(id) {
                    self.popup_job_id = None;
                }
                cx.notify();
            }
        } else if job.snapshot.is_active() && job.foregrounded && self.popup_job_id == Some(id) {
            if let Some(job) = self.job_mut(id) {
                job.foregrounded = false;
            }
            self.popup_job_id = None;
            cx.notify();
        }
    }
}

/// Build the window-scoped gpui-component notification for a translation job.
///
/// Pushing another notification with the same job id replaces the existing
/// notification, so progress updates keep one stable card in the window's
/// component notification layer.
pub fn document_translation_notification(
    snapshot: DocumentTranslationJobSnapshot,
    manager: WeakEntity<DocumentTranslationJobManager>,
) -> Notification {
    let id = snapshot.id;
    let terminal = !snapshot.is_active();
    let type_ = match &snapshot.status {
        DocumentTranslationJobStatus::Succeeded {
            open_error: None, ..
        } => NotificationType::Success,
        DocumentTranslationJobStatus::Failed { .. }
        | DocumentTranslationJobStatus::Cancelled
        | DocumentTranslationJobStatus::Succeeded {
            open_error: Some(_),
            ..
        } => NotificationType::Error,
        _ => NotificationType::Info,
    };
    let title = snapshot.title();
    let (status, detail, fraction, indeterminate) = job_status_content(&snapshot);
    let content_snapshot = snapshot.clone();
    let close_manager = manager.clone();

    Notification::new()
        .id1::<DocumentTranslationNotification>(id.to_string())
        .with_type(type_)
        .title(title)
        .message(status)
        .autohide(false)
        .content(move |_, _, cx| {
            let progress_label = if indeterminate {
                "Working…".to_string()
            } else {
                format!("{:.0}%", fraction * 100.0)
            };
            let actions = job_actions(&content_snapshot.status, true)
                .into_iter()
                .map(|action| {
                    document_translation_action_button(
                        manager.clone(),
                        id,
                        content_snapshot.open_mode,
                        action,
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
                .when(content_snapshot.is_active(), |this| {
                    this.child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                Progress::new(format!("document-translation-progress-{id}"))
                                    .with_size(Size::XSmall)
                                    .loading(indeterminate)
                                    .value(fraction * 100.0),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .min_w_12()
                                    .text_right()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(progress_label),
                            ),
                    )
                })
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
                manager.notification_closed(id, terminal, cx);
            });
        })
}

fn document_translation_action_button(
    manager: WeakEntity<DocumentTranslationJobManager>,
    id: DocumentTranslationJobId,
    open_mode: DocumentTranslationOpenMode,
    action: DocumentTranslationJobAction,
) -> Button {
    let button = Button::new(format!("document-translation-job-{id}-{}", action.id()))
        .label(action.label(open_mode))
        .with_size(Size::XSmall)
        .on_click(move |_, _, cx| {
            _ = manager.update(cx, |manager, cx| match action {
                DocumentTranslationJobAction::Cancel => manager.cancel(id, cx),
                DocumentTranslationJobAction::Minimize => manager.minimize(id, cx),
                DocumentTranslationJobAction::Retry => manager.retry(id, cx),
                DocumentTranslationJobAction::Open => manager.open(id, cx),
                DocumentTranslationJobAction::Dismiss => manager.dismiss(id, cx),
            });
            cx.stop_propagation();
        });

    match action {
        DocumentTranslationJobAction::Open | DocumentTranslationJobAction::Retry => {
            button.primary()
        }
        DocumentTranslationJobAction::Cancel => button.danger(),
        DocumentTranslationJobAction::Minimize | DocumentTranslationJobAction::Dismiss => {
            button.ghost()
        }
    }
}

fn background_task_entry(
    snapshot: &DocumentTranslationJobSnapshot,
    cancel_action: Option<CenterAction>,
) -> BackgroundTaskEntry {
    let (status, detail, fraction, indeterminate) = job_status_content(snapshot);
    let mut entry = BackgroundTaskEntry::new(snapshot.title(), format!("{status} · {detail}"));
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
        } => {
            if let Some(error) = open_error {
                (
                    "Could not open output".to_string(),
                    error.to_string(),
                    1.0,
                    false,
                )
            } else {
                (
                    "Complete".to_string(),
                    output_path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("Translated PDF")
                        .to_string(),
                    1.0,
                    false,
                )
            }
        }
        DocumentTranslationJobStatus::Opening { output_path } => (
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

    fn record_job_events(
        manager: &Entity<DocumentTranslationJobManager>,
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

    fn running_record(
        id: DocumentTranslationJobId,
        foregrounded: bool,
    ) -> DocumentTranslationJobRecord {
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
            foregrounded,
            task: None,
        }
    }

    #[gpui::test]
    fn preflight_errors_remain_until_dismissed(cx: &mut TestAppContext) {
        cx.update(glyphst_ui::activity::init);
        let center = cx.update(|cx| ActivityCenter::global(cx));
        let manager = cx.update(|cx| cx.new(DocumentTranslationJobManager::new));
        let (_recorder, events) = record_job_events(&manager, cx);

        manager.update(cx, |manager, cx| {
            let id = manager.report_preflight_error(
                "/tmp/source.pdf".into(),
                DocumentTranslationOpenMode::Document,
                "Invalid target language",
                cx,
            );

            assert_eq!(manager.jobs.len(), 1);
            assert_eq!(manager.popup_job_id, None);
            manager.retry(id, cx);
            assert!(matches!(
                manager.job(id).map(|job| &job.snapshot.status),
                Some(DocumentTranslationJobStatus::Failed {
                    retryable: false,
                    ..
                })
            ));

            manager.dismiss(id, cx);
            assert!(manager.jobs.is_empty());
            assert_eq!(manager.popup_job_id, None);
            assert_eq!(center.read(cx).active_task_count(), 0);
        });
        cx.run_until_parked();
        assert_eq!(show_notification_count(&events.borrow()), 1);
    }

    #[gpui::test]
    fn minimizing_active_job_keeps_it_in_manager(cx: &mut TestAppContext) {
        let manager = cx.update(|cx| cx.new(DocumentTranslationJobManager::new));

        manager.update(cx, |manager, cx| {
            let id = 7;
            manager.jobs.push_back(running_record(id, true));
            manager.popup_job_id = Some(id);

            manager.minimize(id, cx);

            assert_eq!(manager.jobs.len(), 1);
            assert_eq!(manager.popup_job_id, None);
            assert!(!manager.job(id).unwrap().foregrounded);
            assert!(manager.job(id).unwrap().snapshot.is_active());
        });
    }

    #[gpui::test]
    fn foreground_success_auto_opens_without_terminal_popup(cx: &mut TestAppContext) {
        let manager = cx.update(|cx| cx.new(DocumentTranslationJobManager::new));

        manager.update(cx, |manager, cx| {
            let id = 11;
            manager.jobs.push_back(running_record(id, true));
            manager.active_job_id = Some(id);
            manager.popup_job_id = Some(id);

            manager.finish(
                id,
                Ok(PdfTranslationResult {
                    output_path: "/tmp/translated.pdf".into(),
                    manifest_path: "/tmp/manifest.json".into(),
                    job_directory: "/tmp/job".into(),
                    fingerprint: "test".to_string(),
                }),
                cx,
            );

            assert_eq!(manager.popup_job_id, None);
            assert!(matches!(
                manager.job(id).map(|job| &job.snapshot.status),
                Some(DocumentTranslationJobStatus::Opening { .. })
            ));

            manager.finish_open(id, Ok(()), cx);
            assert!(manager.job(id).is_none());
            assert!(manager.jobs.is_empty());
        });
    }

    #[gpui::test]
    fn minimized_success_emits_component_notification(cx: &mut TestAppContext) {
        cx.update(glyphst_ui::activity::init);
        let manager = cx.update(|cx| cx.new(DocumentTranslationJobManager::new));
        let (_recorder, events) = record_job_events(&manager, cx);

        manager.update(cx, |manager, cx| {
            let id = 13;
            manager.jobs.push_back(running_record(id, false));
            manager.active_job_id = Some(id);

            manager.finish(
                id,
                Ok(PdfTranslationResult {
                    output_path: "/tmp/translated.pdf".into(),
                    manifest_path: "/tmp/manifest.json".into(),
                    job_directory: "/tmp/job".into(),
                    fingerprint: "test".to_string(),
                }),
                cx,
            );

            assert_eq!(manager.popup_job_id, None);
            assert!(matches!(
                manager.job(id).map(|job| &job.snapshot.status),
                Some(DocumentTranslationJobStatus::Succeeded { .. })
            ));
        });
        cx.run_until_parked();
        assert_eq!(show_notification_count(&events.borrow()), 1);
    }

    #[gpui::test]
    fn failed_output_open_returns_to_terminal_notification(cx: &mut TestAppContext) {
        cx.update(glyphst_ui::activity::init);
        let manager = cx.update(|cx| cx.new(DocumentTranslationJobManager::new));
        let (_recorder, events) = record_job_events(&manager, cx);

        manager.update(cx, |manager, cx| {
            let id = 17;
            let mut record = running_record(id, false);
            record.snapshot.status = DocumentTranslationJobStatus::Succeeded {
                output_path: "/tmp/translated.pdf".into(),
                open_error: None,
            };
            manager.jobs.push_back(record);

            manager.open(id, cx);
            assert!(matches!(
                manager.job(id).map(|job| &job.snapshot.status),
                Some(DocumentTranslationJobStatus::Opening { .. })
            ));

            manager.finish_open(id, Err("Could not read the output".into()), cx);
            assert_eq!(manager.popup_job_id, None);
            assert!(matches!(
                manager.job(id).map(|job| &job.snapshot.status),
                Some(DocumentTranslationJobStatus::Succeeded {
                    open_error: Some(_),
                    ..
                })
            ));
        });
        cx.run_until_parked();
        assert_eq!(show_notification_count(&events.borrow()), 1);
    }

    #[gpui::test]
    fn queued_cancellation_removes_job_without_notification(cx: &mut TestAppContext) {
        cx.update(glyphst_ui::activity::init);
        let center = cx.update(|cx| ActivityCenter::global(cx));
        let manager = cx.update(|cx| cx.new(DocumentTranslationJobManager::new));
        let (_recorder, events) = record_job_events(&manager, cx);

        manager.update(cx, |manager, cx| {
            let id = 19;
            let mut record = running_record(id, true);
            record.snapshot.status = DocumentTranslationJobStatus::Queued;
            manager.jobs.push_back(record);
            manager.popup_job_id = Some(id);
            manager.register_background_task(id, cx);

            manager.cancel(id, cx);

            assert!(manager.job(id).is_none());
            assert_eq!(manager.popup_job_id, None);
            assert_eq!(center.read(cx).active_task_count(), 0);
        });
        cx.run_until_parked();
        assert_eq!(show_notification_count(&events.borrow()), 0);
    }

    #[gpui::test]
    fn running_cancellation_finishes_silently(cx: &mut TestAppContext) {
        cx.update(glyphst_ui::activity::init);
        let center = cx.update(|cx| ActivityCenter::global(cx));
        let manager = cx.update(|cx| cx.new(DocumentTranslationJobManager::new));
        let (_recorder, events) = record_job_events(&manager, cx);

        manager.update(cx, |manager, cx| {
            let id = 21;
            manager.jobs.push_back(running_record(id, true));
            manager.active_job_id = Some(id);
            manager.popup_job_id = Some(id);
            manager.register_background_task(id, cx);

            manager.cancel(id, cx);
            assert_eq!(manager.popup_job_id, None);
            assert_eq!(center.read(cx).active_task_count(), 1);

            manager.finish(
                id,
                Ok(PdfTranslationResult {
                    output_path: "/tmp/translated.pdf".into(),
                    manifest_path: "/tmp/manifest.json".into(),
                    job_directory: "/tmp/job".into(),
                    fingerprint: "completed-after-cancel".to_string(),
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
    fn minimized_manager_cards_do_not_offer_minimize_again() {
        let running = DocumentTranslationJobStatus::Running { progress: None };
        assert_eq!(
            job_actions(&running, true),
            vec![
                DocumentTranslationJobAction::Cancel,
                DocumentTranslationJobAction::Minimize,
            ]
        );
        assert_eq!(
            job_actions(&running, false),
            vec![DocumentTranslationJobAction::Cancel]
        );
        assert!(
            job_actions(
                &DocumentTranslationJobStatus::Cancelling { progress: None },
                false
            )
            .is_empty()
        );
    }

    #[gpui::test]
    fn retry_uses_current_translation_languages(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let mut settings = SettingsStore::memory();
            settings.settings.translation.source_language = Some("fr".to_string());
            settings.settings.translation.target_language = "de".to_string();
            cx.set_global(settings);
        });
        let manager = cx.update(|cx| cx.new(DocumentTranslationJobManager::new));

        manager.update(cx, |manager, cx| {
            let id = 23;
            let mut record = running_record(id, false);
            record.snapshot.status = DocumentTranslationJobStatus::Failed {
                stage: PdfTranslationStage::Translating,
                message: "temporary failure".into(),
                retryable: true,
            };
            manager.jobs.push_back(record);

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
