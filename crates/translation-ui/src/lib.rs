pub use glyphst_translation as service;

use std::collections::VecDeque;
use std::path::PathBuf;

use futures::{StreamExt as _, channel::mpsc};
use glyphst_pdf_translation::{
    CancellationToken, PdfTranslationError, PdfTranslationProgress, PdfTranslationRequest,
    PdfTranslationResult, PdfTranslationService, PdfTranslationStage,
};
use glyphst_settings::theme::Theme;
use glyphst_settings::{Settings, SettingsStore, ui_rems};
use glyphst_translation::TARGET_LANGUAGES;
use glyphst_ui::{
    icons::{Icon, IconName},
    searchable_dropdown::SearchableItem,
};
use gpui::{
    AnyElement, Context, EventEmitter, IntoElement, MouseButton, ParentElement as _, SharedString,
    Styled as _, Task, div, prelude::*, px, relative, rgb, rgba,
};

const FOOTBAR_ICON_SIZE: f32 = 14.0;
const FOOTBAR_BUTTON_HEIGHT: f32 = 22.0;
const NOTIFICATION_WIDTH: f32 = 380.0;
const MANAGER_WIDTH: f32 = 420.0;

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
    manager_open: bool,
}

impl EventEmitter<DocumentTranslationJobEvent> for DocumentTranslationJobManager {}

impl DocumentTranslationJobManager {
    pub fn new(cx: &mut Context<Self>) -> Self {
        cx.on_release(|this, _| {
            for job in &this.jobs {
                job.cancellation.cancel();
            }
        })
        .detach();
        Self {
            jobs: VecDeque::new(),
            next_id: 1,
            active_job_id: None,
            popup_job_id: None,
            manager_open: false,
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
        self.show_popup(id);
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
        let mut show_popup = false;
        {
            let Some(job) = self.job_mut(id) else {
                return;
            };
            match job.snapshot.status.clone() {
                DocumentTranslationJobStatus::Queued => {
                    job.snapshot.status = DocumentTranslationJobStatus::Cancelled;
                    job.foregrounded = false;
                    show_popup = true;
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
        if show_popup {
            self.show_popup(id);
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
                self.show_popup(id);
                cx.notify();
                return;
            }
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
        self.show_popup(id);
        self.start_next(cx);
        cx.notify();
    }

    pub fn dismiss(&mut self, id: DocumentTranslationJobId, cx: &mut Context<Self>) {
        let can_dismiss = self.job(id).is_some_and(|job| !job.snapshot.is_active());
        if !can_dismiss {
            return;
        }
        self.jobs.retain(|job| job.snapshot.id != id);
        if self.popup_job_id == Some(id) {
            self.popup_job_id = None;
        }
        if self.jobs.is_empty() {
            self.manager_open = false;
        }
        cx.notify();
    }

    pub fn open(&mut self, id: DocumentTranslationJobId, cx: &mut Context<Self>) {
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
        match result {
            Ok(()) => {
                self.jobs.retain(|job| job.snapshot.id != id);
                if self.popup_job_id == Some(id) {
                    self.popup_job_id = None;
                }
                if self.jobs.is_empty() {
                    self.manager_open = false;
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
                self.show_popup(id);
            }
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

    pub fn render_footbar_activity(
        &mut self,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if self.jobs.is_empty() {
            return None;
        }
        let snapshots = self.snapshots();
        let primary = snapshots
            .iter()
            .rev()
            .find(|job| job.is_active())
            .or_else(|| snapshots.last())?
            .clone();
        let additional = snapshots.len().saturating_sub(1);
        let label = if additional > 0 {
            format!("{} + {additional} more", footbar_summary(&primary))
        } else {
            footbar_summary(&primary)
        };
        let manager_open = self.manager_open;
        let cards = manager_open.then(|| {
            snapshots
                .iter()
                .rev()
                .cloned()
                .map(|job| self.render_job_card(job, false, theme, cx))
                .collect::<Vec<_>>()
        });
        let popover = cards.map(|cards| {
            div()
                .id("document-translation-manager-popover")
                .occlude()
                .absolute()
                .right_0()
                .bottom(px(FOOTBAR_BUTTON_HEIGHT + 4.0))
                .mb_2()
                .w(px(MANAGER_WIDTH))
                .max_h(px(420.0))
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .gap_2()
                .p_2()
                .rounded(px(8.0))
                .border_1()
                .border_color(rgb(theme.border))
                .bg(rgb(theme.surface_background))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .children(cards)
        });
        Some(
            div()
                .relative()
                .flex()
                .items_center()
                .child(
                    div()
                        .id("document-translation-activity")
                        .flex()
                        .items_center()
                        .gap_1()
                        .h(ui_rems(FOOTBAR_BUTTON_HEIGHT))
                        .max_w(px(260.0))
                        .px_2()
                        .rounded(px(4.0))
                        .cursor_pointer()
                        .text_color(rgb(if manager_open {
                            theme.accent
                        } else {
                            theme.footer_text
                        }))
                        .bg(if manager_open {
                            rgb(theme.element_selected)
                        } else {
                            rgba(0x00000000)
                        })
                        .hover(move |style| style.bg(rgb(theme.element_hover)))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                this.manager_open = !this.manager_open;
                                cx.notify();
                                cx.stop_propagation();
                            }),
                        )
                        .child(Icon::new(
                            if primary.is_active() {
                                IconName::Circle
                            } else if matches!(
                                &primary.status,
                                DocumentTranslationJobStatus::Failed { .. }
                                    | DocumentTranslationJobStatus::Cancelled
                            ) {
                                IconName::TriangleAlert
                            } else {
                                IconName::Check
                            },
                            ui_rems(FOOTBAR_ICON_SIZE),
                            rgb(if primary.is_active() {
                                theme.accent
                            } else {
                                theme.footer_text
                            }),
                        ))
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_size(ui_rems(11.0))
                                .child(label),
                        ),
                )
                .children(popover)
                .into_any_element(),
        )
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
            self.show_popup(id);
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
        let (auto_open, show_popup) = {
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
                    (was_foregrounded, !was_foregrounded)
                }
                Err(_) if job.cancellation.is_cancelled() => {
                    job.snapshot.status = DocumentTranslationJobStatus::Cancelled;
                    job.foregrounded = false;
                    (false, true)
                }
                Err(error) => {
                    job.snapshot.status = DocumentTranslationJobStatus::Failed {
                        stage: error.stage,
                        message: error.message.into(),
                        retryable: error.retryable,
                    };
                    job.foregrounded = false;
                    (false, true)
                }
            }
        };
        if auto_open {
            self.open(id, cx);
            self.popup_job_id = None;
        } else if show_popup {
            self.show_popup(id);
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

fn footbar_summary(snapshot: &DocumentTranslationJobSnapshot) -> String {
    match &snapshot.status {
        DocumentTranslationJobStatus::Queued => format!("Queued: {}", snapshot.title()),
        DocumentTranslationJobStatus::Running { progress }
        | DocumentTranslationJobStatus::Cancelling { progress } => {
            let verb = if matches!(
                &snapshot.status,
                DocumentTranslationJobStatus::Cancelling { .. }
            ) {
                "Cancelling"
            } else {
                "Translating"
            };
            if let Some(progress) = progress {
                format!(
                    "{verb}: {} ({:.0}%)",
                    snapshot.title(),
                    progress_fraction(progress) * 100.0
                )
            } else {
                format!("{verb}: {}", snapshot.title())
            }
        }
        DocumentTranslationJobStatus::Cancelled => {
            format!("Cancelled: {}", snapshot.title())
        }
        DocumentTranslationJobStatus::Failed { .. } => format!("Failed: {}", snapshot.title()),
        DocumentTranslationJobStatus::Succeeded { .. } => format!("Ready: {}", snapshot.title()),
        DocumentTranslationJobStatus::Opening { .. } => {
            format!("Opening: {}", snapshot.title())
        }
    }
}

pub fn target_language_items(selected: &str) -> Vec<SearchableItem> {
    let mut items = TARGET_LANGUAGES
        .iter()
        .map(|language| SearchableItem {
            value: language.code.to_string(),
            label: language.label.to_string(),
        })
        .collect::<Vec<_>>();
    if !items.iter().any(|item| item.value == selected) {
        items.insert(
            0,
            SearchableItem {
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
        let manager = cx.update(|cx| cx.new(DocumentTranslationJobManager::new));

        manager.update(cx, |manager, cx| {
            let id = manager.report_preflight_error(
                "/tmp/source.pdf".into(),
                DocumentTranslationOpenMode::Document,
                "Invalid target language",
                cx,
            );

            assert_eq!(manager.jobs.len(), 1);
            assert_eq!(manager.popup_job_id, Some(id));
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
    fn minimized_success_reopens_terminal_popup(cx: &mut TestAppContext) {
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

            assert_eq!(manager.popup_job_id, Some(id));
            assert!(matches!(
                manager.job(id).map(|job| &job.snapshot.status),
                Some(DocumentTranslationJobStatus::Succeeded { .. })
            ));
        });
    }

    #[gpui::test]
    fn failed_output_open_returns_to_terminal_notification(cx: &mut TestAppContext) {
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
            assert_eq!(manager.popup_job_id, Some(id));
            assert!(matches!(
                manager.job(id).map(|job| &job.snapshot.status),
                Some(DocumentTranslationJobStatus::Succeeded {
                    open_error: Some(_),
                    ..
                })
            ));
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
