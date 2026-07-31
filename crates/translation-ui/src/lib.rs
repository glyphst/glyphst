pub use glyphst_translation as service;

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;

use futures::{StreamExt as _, channel::mpsc};
use glyphst_pdf_translation_app::{
    CancellationToken, PdfTranslationError, PdfTranslationProgress, PdfTranslationRequest,
    PdfTranslationResult, PdfTranslationService, PdfTranslationStage, PdfTranslationWarning,
};
use glyphst_settings::{Settings, SettingsStore};
use glyphst_translation::TARGET_LANGUAGES;
use glyphst_ui::activity::{
    ActivityCenter, BackgroundTaskEntry, BackgroundTaskId, CenterAction, TaskProgress,
};
use gpui::{
    ClipboardItem, Context, EventEmitter, IntoElement, ParentElement as _, SharedString, Styled,
    Task, WeakEntity, div, prelude::*,
};
use gpui_component::{
    ActiveTheme as _, Sizable as _, Size,
    button::{Button, ButtonVariants as _},
    notification::{Notification, NotificationType},
    searchable_list::SearchableListItem,
    v_flex,
};

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
                DocumentTranslationOpenMode::Split => "Open split",
            },
            Self::CopyError if has_warnings && !open_error => "Copy warning",
            Self::CopyError => "Copy error",
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

/// Stable notification kind for terminal translation results.
pub struct DocumentTranslationNotification;

struct DocumentTranslationJobRecord {
    snapshot: DocumentTranslationJobSnapshot,
    settings: Settings,
    cancellation: CancellationToken,
    task: Option<Task<()>>,
}

pub struct DocumentTranslationJobManager {
    jobs: VecDeque<DocumentTranslationJobRecord>,
    next_id: DocumentTranslationJobId,
    active_job_id: Option<DocumentTranslationJobId>,
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
            task: None,
        });
        self.register_background_task(id, cx);
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
        let message = message.into();
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
    ) -> DocumentTranslationJobId {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        let has_warnings = !result.warnings.is_empty();
        self.jobs.push_back(DocumentTranslationJobRecord {
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
        self.jobs
            .iter()
            .any(|job| job.snapshot.request.source_path == source_path && job.snapshot.is_active())
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
                }
                DocumentTranslationJobStatus::Cancelling { .. }
                | DocumentTranslationJobStatus::Cancelled
                | DocumentTranslationJobStatus::Failed { .. }
                | DocumentTranslationJobStatus::Succeeded { .. }
                | DocumentTranslationJobStatus::Opening { .. } => {}
            }
        }
        if remove_job {
            self.jobs.retain(|job| job.snapshot.id != id);
            self.remove_background_task(id, cx);
            self.start_next(cx);
        } else {
            self.sync_background_task(id, cx);
        }
        cx.notify();
    }

    pub fn retry(&mut self, id: DocumentTranslationJobId, cx: &mut Context<Self>) {
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
        self.remove_job_notification(id, cx);
        self.register_background_task(id, cx);
        self.start_next(cx);
        cx.notify();
    }

    pub fn open(&mut self, id: DocumentTranslationJobId, cx: &mut Context<Self>) {
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

    fn copy_error(&self, id: DocumentTranslationJobId, cx: &mut Context<Self>) {
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
        id: DocumentTranslationJobId,
        result: Result<(), SharedString>,
        cx: &mut Context<Self>,
    ) {
        let mut publish_error = false;
        match result {
            Ok(()) => {
                self.remove_background_task(id, cx);
                self.jobs.retain(|job| job.snapshot.id != id);
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
                detailed_message: "Document translation is unavailable.".into(),
                retryable: false,
            };
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
        self.remove_background_task(id, cx);
        self.publish_terminal_notification(id, cx);
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
        cx.emit(DocumentTranslationJobEvent::ShowNotification { snapshot });
    }

    fn notification_closed(&mut self, id: DocumentTranslationJobId, cx: &mut Context<Self>) {
        let Some(job) = self.job(id) else {
            return;
        };
        if !job.snapshot.is_active() {
            self.remove_background_task(id, cx);
            self.jobs.retain(|job| job.snapshot.id != id);
            cx.notify();
        }
    }
}

/// Build the window-scoped gpui-component notification for a terminal
/// translation result.
pub fn document_translation_notification(
    snapshot: DocumentTranslationJobSnapshot,
    manager: WeakEntity<DocumentTranslationJobManager>,
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
    manager: WeakEntity<DocumentTranslationJobManager>,
    id: DocumentTranslationJobId,
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
    fn background_activity_names_the_translation_and_document() {
        let snapshot = running_record(1).snapshot;

        let entry = background_task_entry(&snapshot, None);

        assert_eq!(entry.title, "Translate source.pdf");
    }

    fn running_record(id: DocumentTranslationJobId) -> DocumentTranslationJobRecord {
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
    fn preflight_errors_remain_until_notification_is_closed(cx: &mut TestAppContext) {
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
            manager.retry(id, cx);
            assert!(matches!(
                manager.job(id).map(|job| &job.snapshot.status),
                Some(DocumentTranslationJobStatus::Failed {
                    retryable: false,
                    ..
                })
            ));

            manager.notification_closed(id, cx);
            assert!(manager.jobs.is_empty());
            assert_eq!(center.read(cx).active_task_count(), 0);
        });
        cx.run_until_parked();
        assert_eq!(show_notification_count(&events.borrow()), 1);
    }

    #[gpui::test]
    fn progress_stays_in_activity_center_and_success_notifies_once(cx: &mut TestAppContext) {
        cx.update(glyphst_ui::activity::init);
        let center = cx.update(|cx| ActivityCenter::global(cx));
        let manager = cx.update(|cx| cx.new(DocumentTranslationJobManager::new));
        let (_recorder, events) = record_job_events(&manager, cx);

        manager.update(cx, |manager, cx| {
            let id = 13;
            manager.jobs.push_back(running_record(id));
            manager.active_job_id = Some(id);
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
    fn failed_output_open_returns_to_terminal_notification(cx: &mut TestAppContext) {
        cx.update(glyphst_ui::activity::init);
        let manager = cx.update(|cx| cx.new(DocumentTranslationJobManager::new));
        let (_recorder, events) = record_job_events(&manager, cx);

        manager.update(cx, |manager, cx| {
            let id = 17;
            let mut record = running_record(id);
            record.snapshot.status = DocumentTranslationJobStatus::Succeeded {
                output_path: "/tmp/translated.pdf".into(),
                open_error: None,
                warnings: Vec::new(),
            };
            manager.jobs.push_back(record);

            manager.open(id, cx);
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
            let mut record = running_record(id);
            record.snapshot.status = DocumentTranslationJobStatus::Queued;
            manager.jobs.push_back(record);
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
        let manager = cx.update(|cx| cx.new(DocumentTranslationJobManager::new));
        let (_recorder, events) = record_job_events(&manager, cx);

        manager.update(cx, |manager, cx| {
            let id = 21;
            manager.jobs.push_back(running_record(id));
            manager.active_job_id = Some(id);
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
        let manager = cx.update(|cx| cx.new(DocumentTranslationJobManager::new));

        manager.update(cx, |manager, cx| {
            let id = 22;
            let mut record = running_record(id);
            record.snapshot.status = DocumentTranslationJobStatus::Failed {
                stage: PdfTranslationStage::Translating,
                message: "7 translation units remain unresolved".into(),
                detailed_message: "unit-p015-b0002 · page 15 · source excerpt".into(),
                retryable: true,
            };
            manager.jobs.push_back(record);
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
            settings.settings.translation.target_language = "de".to_string();
            cx.set_global(settings);
        });
        let manager = cx.update(|cx| cx.new(DocumentTranslationJobManager::new));

        manager.update(cx, |manager, cx| {
            let id = 23;
            let mut record = running_record(id);
            record.snapshot.status = DocumentTranslationJobStatus::Failed {
                stage: PdfTranslationStage::Translating,
                message: "temporary failure".into(),
                detailed_message: "temporary failure".into(),
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
