pub use glyphst_translation as service;

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;

use futures::{StreamExt as _, channel::mpsc};
use glyphst_pdf_translation_app::{
    CancellationToken, PdfTranslationError, PdfTranslationProgress, PdfTranslationRequest,
    PdfTranslationResult, PdfTranslationService, PdfTranslationStage,
};
use glyphst_settings::theme::Theme;
use glyphst_settings::{Settings, SettingsStore, ui_rems};
use glyphst_translation::TARGET_LANGUAGES;
use glyphst_ui::notification_center::{
    BackgroundTaskEntry, BackgroundTaskId, CenterAction, NotificationCenter, NotificationEntry,
    NotificationId, NotificationLevel, TaskProgress,
};
use gpui::{
    AnyElement, Context, EventEmitter, IntoElement, MouseButton, ParentElement as _, SharedString,
    Styled as _, Task, div, prelude::*, px, relative, rgb,
};
use gpui_component::searchable_list::SearchableListItem;

const NOTIFICATION_WIDTH: f32 = 380.0;

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
}

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
    notifications: HashMap<DocumentTranslationJobId, NotificationId>,
}

impl EventEmitter<DocumentTranslationJobEvent> for DocumentTranslationJobManager {}

impl DocumentTranslationJobManager {
    pub fn new(cx: &mut Context<Self>) -> Self {
        cx.on_release(|this, cx| {
            for job in &this.jobs {
                job.cancellation.cancel();
            }
            if let Some(center) = NotificationCenter::try_global(cx) {
                for task_id in this.background_tasks.values().copied() {
                    center.update(cx, |center, cx| center.remove_task(task_id, cx));
                }
                for notification_id in this.notifications.values().copied() {
                    center.update(cx, |center, cx| {
                        center.dismiss_notification(notification_id, cx)
                    });
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
            notifications: HashMap::new(),
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
        self.show_popup(id);
        self.start_next(cx);
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
        self.show_popup(id);
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
        self.show_popup(id);
        self.start_next(cx);
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

    pub fn render_notification(
        &mut self,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let id = self.popup_job_id?;
        let snapshot = self.job(id)?.snapshot.clone();
        Some(self.render_job_card(snapshot, true, theme, cx))
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
        let Some(center) = NotificationCenter::try_global(cx) else {
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
        let Some(center) = NotificationCenter::try_global(cx) else {
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
        if let Some(center) = NotificationCenter::try_global(cx) {
            center.update(cx, |center, cx| center.remove_task(task_id, cx));
        }
    }

    fn remove_job_notification(&mut self, id: DocumentTranslationJobId, cx: &mut Context<Self>) {
        let Some(notification_id) = self.notifications.remove(&id) else {
            return;
        };
        if let Some(center) = NotificationCenter::try_global(cx) {
            center.update(cx, |center, cx| {
                center.dismiss_notification(notification_id, cx)
            });
        }
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
        let Some(center) = NotificationCenter::try_global(cx) else {
            return;
        };
        let (status, detail, _, _) = job_status_content(&snapshot);
        let level = if matches!(
            snapshot.status,
            DocumentTranslationJobStatus::Succeeded { .. }
        ) {
            NotificationLevel::Success
        } else {
            NotificationLevel::Error
        };
        let weak_manager = cx.weak_entity();
        let mut notification = NotificationEntry::new(level, snapshot.title());
        notification.detail = Some(format!("{status} · {detail}").into());
        match snapshot.status {
            DocumentTranslationJobStatus::Succeeded { .. } => {
                let manager = weak_manager.clone();
                notification
                    .actions
                    .push(CenterAction::new("Open", move |_, cx| {
                        _ = manager.update(cx, |manager, cx| manager.open(id, cx));
                    }));
            }
            DocumentTranslationJobStatus::Failed {
                retryable: true, ..
            } => {
                let manager = weak_manager.clone();
                notification
                    .actions
                    .push(CenterAction::new("Retry", move |_, cx| {
                        _ = manager.update(cx, |manager, cx| manager.retry(id, cx));
                    }));
            }
            _ => {}
        }
        notification
            .actions
            .push(CenterAction::new("Dismiss", move |_, cx| {
                _ = weak_manager.update(cx, |manager, cx| manager.dismiss(id, cx));
            }));
        self.remove_job_notification(id, cx);
        let notification_id = center.update(cx, |center, cx| {
            center.show_notification(notification, None, cx)
        });
        self.notifications.insert(id, notification_id);
    }

    fn show_popup(&mut self, id: DocumentTranslationJobId) {
        if let Some(previous_id) = self.popup_job_id
            && previous_id != id
            && let Some(previous) = self.job_mut(previous_id)
        {
            previous.foregrounded = false;
        }
        self.popup_job_id = Some(id);
    }

    fn render_job_card(
        &mut self,
        snapshot: DocumentTranslationJobSnapshot,
        notification: bool,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = snapshot.id;
        let (status, detail, fraction, indeterminate) = job_status_content(&snapshot);
        let progress_label = if indeterminate {
            "Working…".to_string()
        } else {
            format!("{:.0}%", fraction * 100.0)
        };
        let actions = job_actions(&snapshot.status, notification)
            .into_iter()
            .map(|action| {
                self.job_action_button(id, action.label(snapshot.open_mode), action, theme, cx)
            })
            .collect::<Vec<_>>();

        div()
            .occlude()
            .when(notification, |this| this.w(px(NOTIFICATION_WIDTH)))
            .when(!notification, |this| this.w_full())
            .max_w_full()
            .flex()
            .flex_col()
            .gap_2()
            .p_3()
            .rounded(px(8.0))
            .border_1()
            .border_color(rgb(theme.border))
            .bg(rgb(theme.surface_background))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(
                        div()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .truncate()
                                    .text_size(ui_rems(13.0))
                                    .text_color(rgb(theme.text))
                                    .child(snapshot.title()),
                            )
                            .child(
                                div()
                                    .text_size(ui_rems(11.0))
                                    .text_color(rgb(theme.text_muted))
                                    .child(status),
                            ),
                    )
                    .child(div().flex().items_center().gap_1().children(actions)),
            )
            .child(
                div()
                    .truncate()
                    .text_size(ui_rems(12.0))
                    .text_color(rgb(theme.text_muted))
                    .child(detail),
            )
            .when(snapshot.is_active(), |this| {
                this.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .flex_1()
                                .h(px(4.0))
                                .overflow_hidden()
                                .rounded(px(2.0))
                                .bg(rgb(theme.border_variant))
                                .child(
                                    div()
                                        .h_full()
                                        .w(relative(fraction))
                                        .rounded(px(2.0))
                                        .bg(rgb(theme.accent)),
                                ),
                        )
                        .child(
                            div()
                                .flex_none()
                                .min_w(px(48.0))
                                .text_align(gpui::TextAlign::Right)
                                .text_size(ui_rems(11.0))
                                .text_color(rgb(theme.text_muted))
                                .child(progress_label),
                        ),
                )
            })
            .into_any_element()
    }

    fn job_action_button(
        &mut self,
        id: DocumentTranslationJobId,
        label: &'static str,
        action: DocumentTranslationJobAction,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .id(SharedString::from(format!(
                "document-translation-job-{id}-{}",
                action.id()
            )))
            .flex()
            .items_center()
            .justify_center()
            .h(ui_rems(22.0))
            .px_2()
            .rounded(px(4.0))
            .border_1()
            .border_color(rgb(theme.control_border))
            .bg(rgb(theme.control_background))
            .hover(move |style| style.bg(rgb(theme.control_hover)))
            .cursor_pointer()
            .text_size(ui_rems(11.0))
            .text_color(rgb(theme.text))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    match action {
                        DocumentTranslationJobAction::Cancel => this.cancel(id, cx),
                        DocumentTranslationJobAction::Minimize => this.minimize(id, cx),
                        DocumentTranslationJobAction::Retry => this.retry(id, cx),
                        DocumentTranslationJobAction::Open => this.open(id, cx),
                        DocumentTranslationJobAction::Dismiss => this.dismiss(id, cx),
                    }
                    cx.stop_propagation();
                }),
            )
            .child(label)
            .into_any_element()
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
    use gpui::TestAppContext;

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
        cx.update(glyphst_ui::notification_center::init);
        let center = cx.update(|cx| NotificationCenter::global(cx));
        let manager = cx.update(|cx| cx.new(DocumentTranslationJobManager::new));

        manager.update(cx, |manager, cx| {
            let id = manager.report_preflight_error(
                "/tmp/source.pdf".into(),
                DocumentTranslationOpenMode::Document,
                "Invalid target language",
                cx,
            );

            assert_eq!(manager.jobs.len(), 1);
            assert_eq!(manager.popup_job_id, None);
            assert_eq!(center.read(cx).notification_count(), 1);
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
            assert_eq!(center.read(cx).notification_count(), 0);
        });
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
    fn minimized_success_moves_to_notification_center(cx: &mut TestAppContext) {
        cx.update(glyphst_ui::notification_center::init);
        let center = cx.update(|cx| NotificationCenter::global(cx));
        let manager = cx.update(|cx| cx.new(DocumentTranslationJobManager::new));

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
            assert_eq!(center.read(cx).notification_count(), 1);
            assert!(matches!(
                manager.job(id).map(|job| &job.snapshot.status),
                Some(DocumentTranslationJobStatus::Succeeded { .. })
            ));
        });
    }

    #[gpui::test]
    fn failed_output_open_returns_to_terminal_notification(cx: &mut TestAppContext) {
        cx.update(glyphst_ui::notification_center::init);
        let center = cx.update(|cx| NotificationCenter::global(cx));
        let manager = cx.update(|cx| cx.new(DocumentTranslationJobManager::new));

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
            assert_eq!(center.read(cx).notification_count(), 1);
            assert!(matches!(
                manager.job(id).map(|job| &job.snapshot.status),
                Some(DocumentTranslationJobStatus::Succeeded {
                    open_error: Some(_),
                    ..
                })
            ));
        });
    }

    #[gpui::test]
    fn queued_cancellation_removes_job_without_notification(cx: &mut TestAppContext) {
        cx.update(glyphst_ui::notification_center::init);
        let center = cx.update(|cx| NotificationCenter::global(cx));
        let manager = cx.update(|cx| cx.new(DocumentTranslationJobManager::new));

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
            assert_eq!(center.read(cx).notification_count(), 0);
        });
    }

    #[gpui::test]
    fn running_cancellation_finishes_silently(cx: &mut TestAppContext) {
        cx.update(glyphst_ui::notification_center::init);
        let center = cx.update(|cx| NotificationCenter::global(cx));
        let manager = cx.update(|cx| cx.new(DocumentTranslationJobManager::new));

        manager.update(cx, |manager, cx| {
            let id = 21;
            manager.jobs.push_back(running_record(id, true));
            manager.active_job_id = Some(id);
            manager.popup_job_id = Some(id);
            manager.register_background_task(id, cx);

            manager.cancel(id, cx);
            assert_eq!(manager.popup_job_id, None);
            assert_eq!(center.read(cx).active_task_count(), 1);
            assert_eq!(center.read(cx).notification_count(), 0);

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
            assert_eq!(center.read(cx).notification_count(), 0);
        });
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
