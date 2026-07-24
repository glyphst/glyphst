use std::{
    cell::RefCell,
    cmp::Ordering,
    collections::HashMap,
    path::{Path, PathBuf},
    rc::Rc,
};

use anyhow::{Result, bail};
use glyphst_core::DocumentFormat;
use glyphst_translation_ui::{DocumentTranslationJobId, DocumentTranslationJobManager};
use gpui::{
    Along, AnyElement, App, AppContext, Axis, BorderStyle, Bounds, Context, CursorStyle,
    DispatchPhase, DragMoveEvent, Element, ElementId, Entity, EntityId, EventEmitter, FocusHandle,
    GlobalElementId, Hitbox, HitboxBehavior, IntoElement, LayoutId, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, ParentElement, Pixels, Point, Render, SharedString, Style,
    Styled, Task, Window, WindowHandle, div, fill, point, prelude::*, px, quad, relative, rgb,
    rgba, size, transparent_black,
};
use gpui_component::Root;
use smallvec::SmallVec;

pub use glyphst_actions::pane::{
    SplitDown, SplitHorizontal, SplitLeft, SplitRight, SplitUp, SplitVertical,
};

use crate::{
    KeymapEditor,
    icons::{Icon, IconName},
    library::{DocumentPosition, LibraryStore, normalize_path},
    new_tab::{NewTab, NewTabEvent},
    pdf_view::{PdfAnnotations, PdfOpenOptions, PdfView, PdfViewEvent, PreparedPdfOpen},
    settings::{SettingsStore, TabBarVisibility, ui_rems},
    theme::Theme,
};

pub(crate) const PANE_TAB_BAR_HEIGHT: f32 = 32.0;
const PANE_TAB_MAX_WIDTH: f32 = 220.0;
const PANE_TAB_MIN_WIDTH: f32 = 120.0;
const PANE_TAB_CLOSE_SIZE: f32 = 22.0;
const PANE_TAB_ICON_SIZE: f32 = 14.0;
const HANDLE_HITBOX_SIZE: f32 = 4.0;
const DIVIDER_SIZE: f32 = 1.0;
const HORIZONTAL_MIN_SIZE: f32 = 120.0;
const VERTICAL_MIN_SIZE: f32 = 140.0;
const DROP_TARGET_SIZE: f32 = 0.25;
const RESIZE_STEP: f32 = 80.0;

#[derive(Clone)]
#[allow(clippy::enum_variant_names)]
pub(crate) enum Tab {
    NewTab(Entity<NewTab>),
    LoadingDocument(Entity<LoadingDocumentView>),
    Document(Entity<PdfView>),
    Keymap(Entity<KeymapEditor>),
}

pub(crate) struct LoadingDocumentView {
    path: PathBuf,
    focus_handle: FocusHandle,
    job_ids: Vec<DocumentTranslationJobId>,
    error: Option<SharedString>,
    _task: Option<Task<()>>,
}

impl Render for LoadingDocumentView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<SettingsStore>().resolved_theme;
        let detail = self.error.clone().unwrap_or_else(|| {
            format!(
                "Loading {}…",
                self.path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("document")
            )
            .into()
        });
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .gap_2()
            .bg(rgb(theme.background))
            .text_color(rgb(if self.error.is_some() {
                theme.error
            } else {
                theme.text_muted
            }))
            .child(Icon::new(
                if self.error.is_some() {
                    IconName::TriangleAlert
                } else {
                    IconName::Circle
                },
                ui_rems(14.0),
                rgb(if self.error.is_some() {
                    theme.error
                } else {
                    theme.accent
                }),
            ))
            .child(detail)
    }
}

#[derive(Clone)]
pub(crate) struct DraggedTab {
    pub(crate) source_window: WindowHandle<Root>,
    pub(crate) source_pane: Entity<Pane>,
    pub(crate) ix: usize,
    pub(crate) tab_id: EntityId,
    pub(crate) label: SharedString,
    pub(crate) theme: Theme,
}

pub(crate) struct TabTransfer {
    tab: Tab,
    index: usize,
    active_tab: usize,
    focus_handle: Option<FocusHandle>,
}

impl Render for DraggedTab {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .h(ui_rems(PANE_TAB_BAR_HEIGHT))
            .max_w(ui_rems(PANE_TAB_MAX_WIDTH))
            .min_w(ui_rems(PANE_TAB_MIN_WIDTH))
            .px_2()
            .flex()
            .items_center()
            .border_1()
            .border_color(rgb(self.theme.border))
            .bg(rgb(self.theme.tab_active_background))
            .text_size(ui_rems(12.0))
            .text_color(rgb(self.theme.text))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(self.label.clone()),
            )
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
    OpenDocument {
        path: PathBuf,
        position: Option<DocumentPosition>,
        replace_active_new_tab: bool,
    },
    DocumentOpenFinished {
        job_id: DocumentTranslationJobId,
        result: std::result::Result<(), SharedString>,
    },
    OpenSettings,
    TabDrop {
        dragged_tab: Box<DraggedTab>,
        target: PaneDropTarget,
    },
    CloseTab(usize),
}

pub(crate) struct Pane {
    tabs: Vec<Tab>,
    active_tab: usize,
    focus_handle: FocusHandle,
    document_translation_jobs: Entity<DocumentTranslationJobManager>,
    tab_focus_handles: HashMap<EntityId, FocusHandle>,
    drag_split_direction: Option<SplitDirection>,
    force_show_tab_bar: bool,
}

impl Pane {
    pub(crate) fn focus_handle(&self) -> FocusHandle {
        self.active_tab_focus_handle()
            .unwrap_or_else(|| self.focus_handle.clone())
    }

    pub(crate) fn empty(
        focus_handle: FocusHandle,
        document_translation_jobs: Entity<DocumentTranslationJobManager>,
    ) -> Self {
        Self {
            tabs: Vec::new(),
            active_tab: 0,
            focus_handle,
            document_translation_jobs,
            tab_focus_handles: HashMap::new(),
            drag_split_direction: None,
            force_show_tab_bar: false,
        }
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

    pub(crate) fn active_tab_is_new_tab(&self) -> bool {
        matches!(self.tabs.get(self.active_tab), Some(Tab::NewTab(_)))
    }

    pub(crate) fn active_document_label(&self, cx: &App) -> Option<SharedString> {
        match self.tabs.get(self.active_tab) {
            Some(Tab::Document(pdf_view)) => Some(pdf_view.read(cx).file_name()),
            Some(Tab::NewTab(_)) | Some(Tab::LoadingDocument(_)) | Some(Tab::Keymap(_)) | None => {
                None
            }
        }
    }

    pub(crate) fn active_document_state(&self, cx: &App) -> Option<(PathBuf, DocumentPosition)> {
        match self.tabs.get(self.active_tab) {
            Some(Tab::Document(pdf_view)) => {
                let pdf_view = pdf_view.read(cx);
                Some((
                    pdf_view.path().to_path_buf(),
                    pdf_view.current_document_position(),
                ))
            }
            Some(Tab::NewTab(_)) | Some(Tab::LoadingDocument(_)) | Some(Tab::Keymap(_)) | None => {
                None
            }
        }
    }

    pub(crate) fn active_pdf_view(&self) -> Option<Entity<PdfView>> {
        match self.tabs.get(self.active_tab) {
            Some(Tab::Document(pdf_view)) => Some(pdf_view.clone()),
            Some(Tab::NewTab(_)) | Some(Tab::LoadingDocument(_)) | Some(Tab::Keymap(_)) | None => {
                None
            }
        }
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
        self.tabs.iter().position(|tab| match tab {
            Tab::Document(pdf_view) => pdf_view.read(cx).path() == path.as_path(),
            Tab::LoadingDocument(loading) => loading.read(cx).path == path,
            Tab::NewTab(_) | Tab::Keymap(_) => false,
        })
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
            && let Some(Tab::Document(pdf_view)) = self.tabs.get(self.active_tab)
        {
            pdf_view.update(cx, |pdf_view, cx| {
                pdf_view.navigate_to_document_position(position, cx);
            });
        }

        self.sync_active_focus_handle();
        self.focus(window, cx);
        cx.notify();
    }

    pub(crate) fn associate_open_job(
        &mut self,
        tab_ix: usize,
        job_id: DocumentTranslationJobId,
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
        job_id: DocumentTranslationJobId,
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
        job_id: Option<DocumentTranslationJobId>,
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
        let (behavior, window_settings, theme) = {
            let store = cx.global::<SettingsStore>();
            (
                store.settings.behavior,
                store.settings.window,
                store.resolved_theme,
            )
        };
        let initial_position = position
            .or_else(|| cx.global::<LibraryStore>().history_position(&path))
            .unwrap_or_else(|| DocumentPosition::fixed(0.0, 0.0, behavior.default_zoom, 1));
        let annotations = PdfAnnotations {
            highlights: cx.global::<LibraryStore>().highlights_for(&path),
            notes: cx.global::<LibraryStore>().notes_for(&path),
        };
        let sidebar_widths = cx.global::<LibraryStore>().sidebar_widths();
        let target_language = cx
            .global::<LibraryStore>()
            .document_target_language(&path)
            .unwrap_or_else(|| {
                cx.global::<SettingsStore>()
                    .settings
                    .translation
                    .target_language
                    .clone()
            });
        let epub_preferences = cx
            .global::<LibraryStore>()
            .epub_reader_preferences(&path)
            .unwrap_or_else(|| cx.global::<SettingsStore>().settings.reading);
        let rotation = cx.global::<LibraryStore>().document_rotation(&path);
        let options = PdfOpenOptions {
            initial_position: Some(initial_position.clone()),
            sidebar_widths,
            annotations,
            target_language,
            epub_preferences,
            rotation,
            document_translation_jobs: self.document_translation_jobs.clone(),
        };
        let loading = cx.new(|_cx| LoadingDocumentView {
            path: path.clone(),
            focus_handle: self.focus_handle.clone(),
            job_ids: job_id.into_iter().collect(),
            error: None,
            _task: None,
        });
        let loading_id = loading.entity_id();
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

        let prepare_path = path.clone();
        let prepare_preferences = epub_preferences;
        let task = cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    PdfView::prepare_open(prepare_path, prepare_preferences, theme.page_gap)
                        .map_err(|error| format!("{error:#}"))
                })
                .await;
            _ = this.update_in(cx, |this, window, cx| {
                this.finish_document_open(
                    loading_id,
                    path,
                    initial_position,
                    behavior,
                    window_settings,
                    options,
                    result,
                    window,
                    cx,
                );
            });
        });
        loading.update(cx, |loading, _| loading._task = Some(task));
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
        let tab = self.create_keymap_tab(cx);
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
        if let Tab::LoadingDocument(loading) = &removed_tab {
            let job_ids = loading.read(cx).job_ids.clone();
            for job_id in job_ids {
                cx.emit(PaneEvent::DocumentOpenFinished {
                    job_id,
                    result: Err("Opening the translated document was cancelled.".into()),
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
        if let Tab::Document(pdf_view) = &transfer.tab {
            let jobs = self.document_translation_jobs.clone();
            pdf_view.update(cx, |pdf_view, cx| {
                pdf_view.set_document_translation_jobs(jobs, cx);
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
        cx: &mut Context<Self>,
    ) {
        let Some(from) = self.tab_ix_for_entity_id(dragged_tab.tab_id) else {
            return;
        };
        self.move_tab(from, to, cx);
    }

    pub(crate) fn flush_all_tabs(&mut self, cx: &mut Context<Self>) {
        for ix in 0..self.tabs.len() {
            self.flush_tab(ix, cx);
        }
    }

    pub(crate) fn stage_all_tab_positions(&mut self, cx: &mut Context<Self>) {
        for tab in &self.tabs {
            if let Tab::Document(pdf_view) = tab {
                pdf_view.update(cx, |pdf_view, cx| {
                    pdf_view.stage_current_history_position(cx);
                });
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn finish_document_open(
        &mut self,
        loading_id: EntityId,
        path: PathBuf,
        initial_position: DocumentPosition,
        behavior: glyphst_settings::BehaviorSettings,
        window_settings: glyphst_settings::WindowSettings,
        options: PdfOpenOptions,
        result: std::result::Result<PreparedPdfOpen, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab_ix) = self.tab_ix_for_entity_id(loading_id) else {
            return;
        };
        let job_ids = match self.tabs.get(tab_ix) {
            Some(Tab::LoadingDocument(loading)) => loading.read(cx).job_ids.clone(),
            _ => return,
        };
        let prepared = match result {
            Ok(prepared) => prepared,
            Err(error) => {
                for job_id in job_ids.iter().copied() {
                    cx.emit(PaneEvent::DocumentOpenFinished {
                        job_id,
                        result: Err(error.clone().into()),
                    });
                }
                if let Some(Tab::LoadingDocument(loading)) = self.tabs.get(tab_ix) {
                    loading.update(cx, |loading, cx| {
                        loading.error = Some(error.into());
                        loading._task = None;
                        cx.notify();
                    });
                }
                return;
            }
        };
        let mut pdf_view = match PdfView::from_prepared(
            prepared,
            self.focus_handle.clone(),
            behavior,
            window_settings,
            options,
        ) {
            Ok(pdf_view) => pdf_view,
            Err(error) => {
                for job_id in job_ids.iter().copied() {
                    cx.emit(PaneEvent::DocumentOpenFinished {
                        job_id,
                        result: Err(format!("{error:#}").into()),
                    });
                }
                if let Some(Tab::LoadingDocument(loading)) = self.tabs.get(tab_ix) {
                    loading.update(cx, |loading, cx| {
                        loading.error = Some(format!("{error:#}").into());
                        loading._task = None;
                        cx.notify();
                    });
                }
                return;
            }
        };
        let pdf_view = cx.new(|cx| {
            pdf_view.register_cache_eviction(cx);
            pdf_view.start_loading(cx);
            pdf_view
        });
        self.subscribe_to_pdf_view(&pdf_view, window, cx);

        let tab = Tab::Document(pdf_view);
        let replaced_tab = std::mem::replace(&mut self.tabs[tab_ix], tab.clone());
        self.remove_tab_focus_handle(&replaced_tab);
        self.register_tab_focus_handle(&tab, self.focus_handle.clone());
        if self.active_tab == tab_ix {
            self.sync_active_focus_handle();
            self.focus(window, cx);
        }

        cx.global_mut::<LibraryStore>()
            .record_history(&path, initial_position);
        for job_id in job_ids {
            cx.emit(PaneEvent::DocumentOpenFinished {
                job_id,
                result: Ok(()),
            });
        }
        cx.notify();
    }

    fn create_new_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Tab {
        let focus_handle = self.focus_handle.clone();
        let new_tab = cx.new(|cx| NewTab::new(focus_handle.clone(), cx));
        self.subscribe_to_new_tab(&new_tab, window, cx);
        let tab = Tab::NewTab(new_tab);
        self.register_tab_focus_handle(&tab, focus_handle);
        tab
    }

    fn create_keymap_tab(&mut self, cx: &mut Context<Self>) -> Tab {
        let focus_handle = self.focus_handle.clone();
        let tab = Tab::Keymap(cx.new(|cx| KeymapEditor::new(focus_handle.clone(), cx)));
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
                let NewTabEvent::OpenDocument(path) = event;
                cx.emit(PaneEvent::OpenDocument {
                    path: path.clone(),
                    position: None,
                    replace_active_new_tab: true,
                });
            },
        )
        .detach();
    }

    fn subscribe_to_pdf_view(
        &self,
        pdf_view: &Entity<PdfView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let pdf_view_id = pdf_view.entity_id();
        cx.subscribe_in(
            pdf_view,
            window,
            move |pane, _pdf_view, event: &PdfViewEvent, _, cx| {
                if !pane.has_tab_id(pdf_view_id) {
                    return;
                }
                match event {
                    PdfViewEvent::OpenDocument { path, position } => {
                        cx.emit(PaneEvent::OpenDocument {
                            path: path.clone(),
                            position: position.clone(),
                            replace_active_new_tab: false,
                        });
                    }
                    PdfViewEvent::OpenSettings => cx.emit(PaneEvent::OpenSettings),
                    PdfViewEvent::ViewportChanged(_)
                    | PdfViewEvent::BlockHovered(_)
                    | PdfViewEvent::BlockSelected(_) => {}
                }
            },
        )
        .detach();
        cx.observe(pdf_view, |_, _, cx| cx.notify()).detach();
    }

    fn subscribe_to_tab(&self, tab: &Tab, window: &mut Window, cx: &mut Context<Self>) {
        match tab {
            Tab::NewTab(new_tab) => self.subscribe_to_new_tab(new_tab, window, cx),
            Tab::LoadingDocument(loading) => {
                cx.observe(loading, |_, _, cx| cx.notify()).detach();
            }
            Tab::Document(pdf_view) => self.subscribe_to_pdf_view(pdf_view, window, cx),
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
        self.focus(window, cx);
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
        if let Some(Tab::Document(pdf_view)) = self.tabs.get(tab_ix) {
            pdf_view.update(cx, |pdf_view, cx| {
                pdf_view.flush_current_history_position(cx);
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
            Some(Tab::Document(pdf_view)) => pdf_view.read(cx).file_name(),
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
            Some(Tab::Document(pdf_view)) => !self
                .document_translation_jobs
                .read(cx)
                .has_job_for_source(pdf_view.read(cx).path()),
            Some(Tab::NewTab(_)) => true,
            Some(Tab::LoadingDocument(_)) | Some(Tab::Keymap(_)) | None => false,
        }
    }

    fn tab_entity_id(tab: &Tab) -> EntityId {
        match tab {
            Tab::NewTab(new_tab) => new_tab.entity_id(),
            Tab::LoadingDocument(loading) => loading.entity_id(),
            Tab::Document(pdf_view) => pdf_view.entity_id(),
            Tab::Keymap(keymap_editor) => keymap_editor.entity_id(),
        }
    }

    fn active_tab_content(&self) -> AnyElement {
        match self.tabs.get(self.active_tab) {
            Some(Tab::Document(pdf_view)) => pdf_view.clone().into_any_element(),
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
    ) -> AnyElement {
        let selected = tab_ix == self.active_tab;
        let label = self.tab_label(tab_ix, cx);
        let draggable = self.tab_is_draggable(tab_ix, cx);
        let tab_id = self
            .tabs
            .get(tab_ix)
            .map(Self::tab_entity_id)
            .unwrap_or_else(|| cx.entity_id());

        div()
            .id(("tab", tab_id))
            .h(ui_rems(PANE_TAB_BAR_HEIGHT))
            .max_w(ui_rems(PANE_TAB_MAX_WIDTH))
            .min_w(ui_rems(PANE_TAB_MIN_WIDTH))
            .px_2()
            .flex()
            .items_center()
            .justify_between()
            .gap_2()
            .border_r(px(1.0))
            .border_color(rgb(theme.border))
            .when(!selected, |this| this.border_b(px(1.0)))
            .bg(rgb(if selected {
                theme.tab_active_background
            } else {
                theme.tab_inactive_background
            }))
            .text_color(rgb(if selected {
                theme.text
            } else {
                theme.text_muted
            }))
            .hover(move |style| style.bg(rgb(theme.element_hover)))
            .cursor_pointer()
            .debug_selector(move || format!("pane-tab-{tab_ix}"))
            .when_some(
                source_window.filter(|_| draggable),
                |this, source_window| {
                    let drag_source_pane = source_pane.clone();
                    this.on_drag(
                        DraggedTab {
                            source_window,
                            source_pane: source_pane.clone(),
                            ix: tab_ix,
                            tab_id,
                            label: label.clone(),
                            theme,
                        },
                        |tab, _, _, cx| cx.new(|_| tab.clone()),
                    )
                    .drag_over::<DraggedTab>(move |style, dragged_tab, _, _| {
                        let style = style.bg(rgb(theme.element_hover));
                        if dragged_tab.source_pane != drag_source_pane || tab_ix < dragged_tab.ix {
                            style.border_l(px(2.0)).border_color(rgb(theme.accent))
                        } else if tab_ix > dragged_tab.ix {
                            style.border_r(px(2.0)).border_color(rgb(theme.accent))
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
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    this.activate_tab(tab_ix, window, cx);
                    cx.stop_propagation();
                }),
            )
            .child(div().flex_1().min_w_0().truncate().child(label))
            .child(
                div()
                    .id(("tab-close", tab_id))
                    .flex_none()
                    .w(ui_rems(PANE_TAB_CLOSE_SIZE))
                    .h(ui_rems(PANE_TAB_CLOSE_SIZE))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(4.0))
                    .text_color(rgb(theme.text_muted))
                    .hover(move |style| {
                        style
                            .bg(rgb(theme.element_hover))
                            .text_color(rgb(theme.text))
                    })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |_, _, _, cx| {
                            cx.emit(PaneEvent::CloseTab(tab_ix));
                            cx.stop_propagation();
                        }),
                    )
                    .child(Icon::new(
                        IconName::X,
                        ui_rems(PANE_TAB_ICON_SIZE),
                        rgb(theme.text_muted),
                    )),
            )
            .into_any_element()
    }

    fn render_tab_bar(&self, theme: Theme, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let source_window = window.window_handle().downcast::<Root>();
        let source_pane = cx.entity();
        let tabs = (0..self.tabs.len())
            .map(|tab_ix| self.render_tab(tab_ix, source_window, source_pane.clone(), theme, cx))
            .collect::<Vec<_>>();

        div()
            .flex_none()
            .h(ui_rems(PANE_TAB_BAR_HEIGHT))
            .flex()
            .items_stretch()
            .bg(rgb(theme.tab_bar_background))
            .children(tabs)
            .child(
                div()
                    .id("pane-new-tab")
                    .w(ui_rems(32.0))
                    .h_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .border_r(px(1.0))
                    .border_b(px(1.0))
                    .border_color(rgb(theme.border))
                    .text_color(rgb(theme.text_muted))
                    .hover(move |style| {
                        style
                            .bg(rgb(theme.element_hover))
                            .text_color(rgb(theme.text))
                    })
                    .cursor_pointer()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, window, cx| {
                            this.open_new_tab(window, cx);
                            cx.stop_propagation();
                        }),
                    )
                    .child(Icon::new(
                        IconName::Plus,
                        ui_rems(16.0),
                        rgb(theme.text_muted),
                    )),
            )
            .child(
                div()
                    .flex_1()
                    .h_full()
                    .border_b(px(1.0))
                    .border_color(rgb(theme.border))
                    .drag_over::<DraggedTab>(move |style, _, _, _| {
                        style
                            .bg(rgb(theme.element_hover))
                            .border_l(px(2.0))
                            .border_color(rgb(theme.accent))
                    })
                    .on_drop(cx.listener(move |_, dragged_tab: &DraggedTab, _, cx| {
                        cx.emit(PaneEvent::TabDrop {
                            dragged_tab: Box::new(dragged_tab.clone()),
                            target: PaneDropTarget::Append,
                        });
                    })),
            )
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
            .child(preview.bg(rgba(theme.overlay)))
            .into_any_element()
    }
}

impl EventEmitter<PaneEvent> for Pane {}

impl Render for Pane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (theme, tab_bar_visibility) = {
            let store = cx.global::<SettingsStore>();
            (store.resolved_theme, store.settings.tab_bar_visibility)
        };
        let show_tab_bar = tab_bar_visibility == TabBarVisibility::Always
            || self.force_show_tab_bar
            || self.tabs.len() >= 2;

        div()
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .overflow_hidden()
            .track_focus(&self.focus_handle)
            .capture_any_mouse_down(cx.listener(|this, _: &MouseDownEvent, window, cx| {
                this.focus_handle.focus(window, cx);
                cx.emit(PaneEvent::Activate);
            }))
            .when(show_tab_bar, |this| {
                this.child(self.render_tab_bar(theme, window, cx))
            })
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .overflow_hidden()
                    .child(self.active_tab_content())
                    .child(self.render_drop_overlay(theme, cx)),
            )
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

impl PaneGroup {
    pub(crate) fn new(pane: Entity<Pane>) -> Self {
        Self {
            root: Member::Pane(pane),
        }
    }

    pub(crate) fn split(
        &mut self,
        old_pane: &Entity<Pane>,
        new_pane: &Entity<Pane>,
        direction: SplitDirection,
    ) {
        let found = match &mut self.root {
            Member::Pane(pane) => {
                if pane == old_pane {
                    self.root = Member::new_axis(old_pane.clone(), new_pane.clone(), direction);
                    true
                } else {
                    false
                }
            }
            Member::Axis(axis) => axis.split(old_pane, new_pane, direction),
        };

        if !found {
            let first_pane = self.root.first_pane();
            match &mut self.root {
                Member::Pane(_) => {
                    self.root = Member::new_axis(first_pane, new_pane.clone(), direction);
                }
                Member::Axis(axis) => {
                    let _ = axis.split(&first_pane, new_pane, direction);
                }
            }
        }
    }

    pub(crate) fn remove(&mut self, pane: &Entity<Pane>) -> bool {
        match &mut self.root {
            Member::Pane(_) => false,
            Member::Axis(axis) => match axis.remove(pane) {
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
        let distance = px(HANDLE_HITBOX_SIZE);
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
    ) -> bool {
        if let Some(pane) = self.find_pane_at_border(direction)
            && pane == *active_pane
        {
            return false;
        }

        if !self.remove(active_pane) {
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
            root.insert_pane(idx, active_pane);
            return true;
        }

        let members = if direction.increasing() {
            vec![self.root.clone(), Member::Pane(active_pane.clone())]
        } else {
            vec![Member::Pane(active_pane.clone()), self.root.clone()]
        };
        self.root = Member::Axis(PaneAxis::new(direction.axis(), members));
        true
    }

    pub(crate) fn resize(&mut self, pane: &Entity<Pane>, direction: SplitDirection) {
        let axis = direction.axis();
        let amount = if direction.increasing() {
            px(RESIZE_STEP)
        } else {
            px(-RESIZE_STEP)
        };
        if let Member::Axis(axis_group) = &mut self.root {
            let _ = axis_group.resize(pane, axis, amount);
        }
    }

    pub(crate) fn reset_pane_sizes(&mut self) {
        self.root.reset_pane_sizes();
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
        self.root
            .render(0, active_pane, theme, dim_inactive_panes, window, cx)
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
    fn new_axis(old_pane: Entity<Pane>, new_pane: Entity<Pane>, direction: SplitDirection) -> Self {
        let axis = direction.axis();
        let members = match direction {
            SplitDirection::Up | SplitDirection::Left => {
                vec![Member::Pane(new_pane), Member::Pane(old_pane)]
            }
            SplitDirection::Down | SplitDirection::Right => {
                vec![Member::Pane(old_pane), Member::Pane(new_pane)]
            }
        };

        Member::Axis(PaneAxis::new(axis, members))
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

    fn reset_pane_sizes(&self) {
        match self {
            Member::Axis(axis) => axis.reset_pane_sizes(),
            Member::Pane(_) => {}
        }
    }

    fn render(
        &self,
        basis: usize,
        active_pane: &Entity<Pane>,
        theme: Theme,
        dim_inactive_panes: bool,
        window: &mut Window,
        cx: &mut App,
    ) -> PaneRenderResult {
        match self {
            Member::Pane(pane) => PaneRenderResult {
                element: div()
                    .relative()
                    .flex_1()
                    .size_full()
                    .child(pane.clone())
                    .into_any_element(),
                contains_active_pane: pane == active_pane,
            },
            Member::Axis(axis) => axis.render(
                basis + 1,
                active_pane,
                theme,
                dim_inactive_panes,
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
    flexes: Rc<RefCell<Vec<f32>>>,
    bounding_boxes: Rc<RefCell<Vec<Option<Bounds<Pixels>>>>>,
}

impl PaneAxis {
    fn new(axis: Axis, members: Vec<Member>) -> Self {
        let len = members.len();
        Self {
            axis,
            members,
            flexes: Rc::new(RefCell::new(vec![1.0; len])),
            bounding_boxes: Rc::new(RefCell::new(vec![None; len])),
        }
    }

    fn split(
        &mut self,
        old_pane: &Entity<Pane>,
        new_pane: &Entity<Pane>,
        direction: SplitDirection,
    ) -> bool {
        for (mut idx, member) in self.members.iter_mut().enumerate() {
            match member {
                Member::Axis(axis) => {
                    if axis.split(old_pane, new_pane, direction) {
                        return true;
                    }
                }
                Member::Pane(pane) => {
                    if pane == old_pane {
                        if direction.axis() == self.axis {
                            if direction.increasing() {
                                idx += 1;
                            }
                            self.insert_pane(idx, new_pane);
                        } else {
                            *member =
                                Member::new_axis(old_pane.clone(), new_pane.clone(), direction);
                        }
                        return true;
                    }
                }
            }
        }
        false
    }

    fn insert_pane(&mut self, idx: usize, new_pane: &Entity<Pane>) {
        self.members.insert(idx, Member::Pane(new_pane.clone()));
        *self.flexes.borrow_mut() = vec![1.0; self.members.len()];
        *self.bounding_boxes.borrow_mut() = vec![None; self.members.len()];
    }

    fn remove(&mut self, pane_to_remove: &Entity<Pane>) -> Result<Option<Member>> {
        let mut remove_member = None;
        let mut found = false;

        for (idx, member) in self.members.iter_mut().enumerate() {
            match member {
                Member::Axis(axis) => {
                    if let Ok(last_member) = axis.remove(pane_to_remove) {
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
        }
        *self.flexes.borrow_mut() = vec![1.0; self.members.len()];
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

    fn reset_pane_sizes(&self) {
        *self.flexes.borrow_mut() = vec![1.0; self.members.len()];
        for member in &self.members {
            member.reset_pane_sizes();
        }
    }

    fn resize(&mut self, pane: &Entity<Pane>, axis: Axis, amount: Pixels) -> Option<bool> {
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
                    && let Some(done) = child_axis.resize(pane, axis, amount)
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

        let bounds = self
            .bounding_boxes
            .borrow()
            .iter()
            .filter_map(|bounds| *bounds)
            .reduce(|acc, bounds| acc.union(&bounds))?;
        let container_size = bounds.size;
        let container_along = container_size.along(axis);
        if container_along <= px(0.0) {
            return Some(true);
        }

        let min_size = match axis {
            Axis::Horizontal => px(HORIZONTAL_MIN_SIZE),
            Axis::Vertical => px(VERTICAL_MIN_SIZE),
        };
        let mut flexes = self.flexes.borrow_mut();
        if flexes.len() < 2 {
            return Some(true);
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

        let size = |ix: usize, flexes: &[f32]| container_along * (flexes[ix] / flexes.len() as f32);
        if min_size - px(1.0) > size(ix, flexes.as_slice()) {
            return Some(true);
        }

        let apply_changes = |current_ix: usize, amount: Pixels, flexes: &mut [f32]| {
            let total = size(current_ix, flexes) + size(current_ix + 1, flexes);
            let next_size = Pixels::max(size(current_ix + 1, flexes) - amount, min_size);
            let current_size = Pixels::max(total - next_size, min_size);
            let current_delta = current_size - size(current_ix, flexes);
            let flex_delta = flexes.len() as f32 * current_delta / container_along;
            flexes[current_ix] += flex_delta;
            flexes[current_ix + 1] -= flex_delta;
        };

        if ix + 1 == flexes.len() {
            if ix > 0 {
                apply_changes(ix - 1, -amount, flexes.as_mut_slice());
            }
        } else {
            apply_changes(ix, amount, flexes.as_mut_slice());
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

    fn bounding_box_for_pane(&self, pane: &Entity<Pane>) -> Option<Bounds<Pixels>> {
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

    fn render(
        &self,
        basis: usize,
        active_pane: &Entity<Pane>,
        theme: Theme,
        dim_inactive_panes: bool,
        window: &mut Window,
        cx: &mut App,
    ) -> PaneRenderResult {
        let mut active_pane_ix = None;
        let mut contains_active_pane = false;
        let mut is_leaf_pane = vec![false; self.members.len()];
        let rendered_children = self
            .members
            .iter()
            .enumerate()
            .map(|(ix, member)| {
                if let Member::Pane(pane) = member {
                    is_leaf_pane[ix] = true;
                    if pane == active_pane {
                        active_pane_ix = Some(ix);
                    }
                }
                let result = member.render(
                    (basis + ix) * 10,
                    active_pane,
                    theme,
                    dim_inactive_panes,
                    window,
                    cx,
                );
                if result.contains_active_pane {
                    contains_active_pane = true;
                }
                result.element
            })
            .collect::<Vec<_>>();

        let element = pane_axis(
            self.axis,
            basis,
            self.flexes.clone(),
            self.bounding_boxes.clone(),
            theme,
            dim_inactive_panes,
        )
        .with_active_pane(active_pane_ix)
        .with_is_leaf_pane_mask(is_leaf_pane)
        .children(rendered_children)
        .into_any_element();

        PaneRenderResult {
            element,
            contains_active_pane,
        }
    }
}

fn pane_axis(
    axis: Axis,
    basis: usize,
    flexes: Rc<RefCell<Vec<f32>>>,
    bounding_boxes: Rc<RefCell<Vec<Option<Bounds<Pixels>>>>>,
    theme: Theme,
    dim_inactive_panes: bool,
) -> PaneAxisElement {
    PaneAxisElement {
        axis,
        basis,
        flexes,
        bounding_boxes,
        children: SmallVec::new(),
        active_pane_ix: None,
        is_leaf_pane_mask: Vec::new(),
        theme,
        dim_inactive_panes,
    }
}

struct PaneAxisElement {
    axis: Axis,
    basis: usize,
    flexes: Rc<RefCell<Vec<f32>>>,
    bounding_boxes: Rc<RefCell<Vec<Option<Bounds<Pixels>>>>>,
    children: SmallVec<[AnyElement; 2]>,
    active_pane_ix: Option<usize>,
    is_leaf_pane_mask: Vec<bool>,
    theme: Theme,
    dim_inactive_panes: bool,
}

struct PaneAxisLayout {
    drag: Rc<RefCell<Option<ResizeDrag>>>,
    children: Vec<PaneAxisChildLayout>,
}

struct PaneAxisChildLayout {
    bounds: Bounds<Pixels>,
    element: AnyElement,
    handle: Option<PaneAxisHandleLayout>,
    is_leaf_pane: bool,
}

struct PaneAxisHandleLayout {
    hitbox: Hitbox,
    divider_bounds: Bounds<Pixels>,
}

struct ResizeComputation<'a> {
    flexes: Rc<RefCell<Vec<f32>>>,
    event: &'a MouseMoveEvent,
    axis: Axis,
    drag: ResizeDrag,
}

#[derive(Clone)]
struct ResizeDrag {
    ix: usize,
    pointer_origin: Point<Pixels>,
    sizes: Vec<f32>,
    container_along: f32,
}

fn resize_axis_sizes(sizes: &mut [f32], ix: usize, requested_delta: f32, min_size: f32) -> f32 {
    if ix + 1 >= sizes.len() || requested_delta == 0.0 {
        return 0.0;
    }

    if requested_delta.is_sign_positive() {
        let available = sizes[ix + 1..]
            .iter()
            .map(|size| (size - min_size).max(0.0))
            .sum::<f32>();
        let applied = requested_delta.min(available);
        let mut remaining = applied;
        for size in &mut sizes[ix + 1..] {
            let reduction = (*size - min_size).max(0.0).min(remaining);
            *size -= reduction;
            remaining -= reduction;
            if remaining <= f32::EPSILON {
                break;
            }
        }
        sizes[ix] += applied;
        applied
    } else {
        let applied = (-requested_delta).min((sizes[ix] - min_size).max(0.0));
        sizes[ix] -= applied;
        sizes[ix + 1] += applied;
        -applied
    }
}

impl PaneAxisElement {
    fn with_active_pane(mut self, active_pane_ix: Option<usize>) -> Self {
        self.active_pane_ix = active_pane_ix;
        self
    }

    fn with_is_leaf_pane_mask(mut self, mask: Vec<bool>) -> Self {
        self.is_leaf_pane_mask = mask;
        self
    }

    fn compute_resize(args: ResizeComputation<'_>, window: &mut Window, cx: &mut App) {
        let ResizeComputation {
            flexes,
            event,
            axis,
            drag,
        } = args;
        let min_size = match axis {
            Axis::Horizontal => HORIZONTAL_MIN_SIZE,
            Axis::Vertical => VERTICAL_MIN_SIZE,
        };
        let container_along = drag.container_along;
        if container_along <= 0.0 {
            return;
        }

        let mut flexes = flexes.borrow_mut();
        if flexes.len() != drag.sizes.len() || drag.ix + 1 >= flexes.len() {
            return;
        }

        let len = flexes.len();
        let mut sizes = drag.sizes;
        let requested_delta = (event.position - drag.pointer_origin).along(axis).as_f32();
        resize_axis_sizes(&mut sizes, drag.ix, requested_delta, min_size);

        for (flex, size) in flexes.iter_mut().zip(sizes) {
            *flex = len as f32 * size / container_along;
        }

        window.refresh();
        cx.stop_propagation();
    }

    fn layout_handle(
        axis: Axis,
        pane_bounds: Bounds<Pixels>,
        window: &mut Window,
    ) -> PaneAxisHandleLayout {
        let handle_bounds = Bounds {
            origin: pane_bounds.origin.apply_along(axis, |origin| {
                origin + pane_bounds.size.along(axis) - px(HANDLE_HITBOX_SIZE / 2.0)
            }),
            size: pane_bounds
                .size
                .apply_along(axis, |_| px(HANDLE_HITBOX_SIZE)),
        };
        let divider_bounds = Bounds {
            origin: pane_bounds
                .origin
                .apply_along(axis, |origin| origin + pane_bounds.size.along(axis)),
            size: pane_bounds.size.apply_along(axis, |_| px(DIVIDER_SIZE)),
        };

        PaneAxisHandleLayout {
            hitbox: window.insert_hitbox(handle_bounds, HitboxBehavior::BlockMouse),
            divider_bounds,
        }
    }
}

impl IntoElement for PaneAxisElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for PaneAxisElement {
    type RequestLayoutState = ();
    type PrepaintState = PaneAxisLayout;

    fn id(&self) -> Option<ElementId> {
        Some(self.basis.into())
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _global_id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let style = Style {
            flex_grow: 1.0,
            flex_shrink: 1.0,
            flex_basis: relative(0.0).into(),
            size: size(relative(1.0), relative(1.0)).map(Into::into),
            ..Style::default()
        };
        (window.request_layout(style, None, cx), ())
    }

    fn prepaint(
        &mut self,
        global_id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _state: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let drag = window.with_element_state::<Rc<RefCell<Option<ResizeDrag>>>, _>(
            global_id.unwrap(),
            |state, _window| {
                let state = state.unwrap_or_else(|| Rc::new(RefCell::new(None)));
                (state.clone(), state)
            },
        );
        let flexes = self.flexes.borrow().clone();
        let len = self.children.len();
        let total_flex = len.max(1) as f32;
        let mut origin = bounds.origin;
        let space_per_flex = bounds.size.along(self.axis) / total_flex;
        let mut bounding_boxes = self.bounding_boxes.borrow_mut();
        bounding_boxes.clear();

        let mut layout = PaneAxisLayout {
            drag,
            children: Vec::new(),
        };

        for (ix, mut child) in std::mem::take(&mut self.children).into_iter().enumerate() {
            let child_flex = flexes.get(ix).copied().unwrap_or(1.0);
            let child_size = bounds
                .size
                .apply_along(self.axis, |_| space_per_flex * child_flex)
                .map(|dimension| dimension.round());
            let child_bounds = Bounds {
                origin,
                size: child_size,
            };
            bounding_boxes.push(Some(child_bounds));
            child.layout_as_root(child_size.into(), window, cx);
            child.prepaint_at(origin, window, cx);
            origin = origin.apply_along(self.axis, |value| value + child_size.along(self.axis));
            let is_leaf_pane = self.is_leaf_pane_mask.get(ix).copied().unwrap_or(true);
            layout.children.push(PaneAxisChildLayout {
                bounds: child_bounds,
                element: child,
                handle: None,
                is_leaf_pane,
            });
        }

        for (ix, child_layout) in layout.children.iter_mut().enumerate() {
            if ix < len.saturating_sub(1) {
                child_layout.handle =
                    Some(Self::layout_handle(self.axis, child_layout.bounds, window));
            }
        }

        layout
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        layout: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        for child in &mut layout.children {
            child.element.paint(window, cx);
        }

        for (ix, child) in layout.children.iter_mut().enumerate() {
            let overlay_bounds = Bounds {
                origin: child
                    .bounds
                    .origin
                    .apply_along(Axis::Horizontal, |value| value + px(1.0)),
                size: child
                    .bounds
                    .size
                    .apply_along(Axis::Horizontal, |value| value - px(1.0)),
            };

            if self.dim_inactive_panes && child.is_leaf_pane && self.active_pane_ix != Some(ix) {
                window.paint_quad(fill(overlay_bounds, rgba(self.theme.overlay)));
            }

            if child.is_leaf_pane && self.active_pane_ix == Some(ix) {
                window.paint_quad(quad(
                    overlay_bounds,
                    0.0,
                    transparent_black(),
                    px(1.0),
                    rgb(self.theme.accent),
                    BorderStyle::Solid,
                ));
            }

            if let Some(handle) = child.handle.as_mut() {
                let cursor_style = match self.axis {
                    Axis::Vertical => CursorStyle::ResizeRow,
                    Axis::Horizontal => CursorStyle::ResizeColumn,
                };

                if layout
                    .drag
                    .borrow()
                    .as_ref()
                    .is_some_and(|drag| drag.ix == ix)
                {
                    window.set_window_cursor_style(cursor_style);
                } else {
                    window.set_cursor_style(cursor_style, &handle.hitbox);
                }

                window.paint_quad(fill(handle.divider_bounds, rgb(self.theme.border)));

                window.on_mouse_event({
                    let drag = layout.drag.clone();
                    let flexes = self.flexes.clone();
                    let handle_hitbox = handle.hitbox.clone();
                    let axis = self.axis;
                    let container_size = bounds.size;
                    move |event: &MouseDownEvent, phase: DispatchPhase, window, cx| {
                        if phase.bubble() && handle_hitbox.is_hovered(window) {
                            if event.click_count >= 2 {
                                let len = flexes.borrow().len();
                                *flexes.borrow_mut() = vec![1.0; len];
                                window.refresh();
                            } else {
                                let flexes = flexes.borrow();
                                let len = flexes.len();
                                let container_along = container_size.along(axis).as_f32();
                                if len < 2 || container_along <= 0.0 {
                                    return;
                                }
                                let sizes = flexes
                                    .iter()
                                    .map(|flex| container_along * *flex / len as f32)
                                    .collect();
                                drag.replace(Some(ResizeDrag {
                                    ix,
                                    pointer_origin: event.position,
                                    sizes,
                                    container_along,
                                }));
                            }
                            cx.stop_propagation();
                        }
                    }
                });

                window.on_mouse_event({
                    let drag = layout.drag.clone();
                    let flexes = self.flexes.clone();
                    let axis = self.axis;
                    move |event: &MouseMoveEvent, phase: DispatchPhase, window, cx| {
                        let Some(drag_state) = drag.borrow().clone() else {
                            return;
                        };
                        if phase.bubble() && drag_state.ix == ix {
                            Self::compute_resize(
                                ResizeComputation {
                                    flexes: flexes.clone(),
                                    event,
                                    axis,
                                    drag: drag_state,
                                },
                                window,
                                cx,
                            );
                        }
                    }
                });
            }
        }

        window.on_mouse_event({
            let drag = layout.drag.clone();
            move |_: &MouseUpEvent, phase: DispatchPhase, _, _| {
                if phase.bubble() {
                    drag.replace(None);
                }
            }
        });
    }
}

impl ParentElement for PaneAxisElement {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use gpui::{AppContext as _, TestAppContext};
    use gpui_component::Root;

    use super::{Pane, Tab, resize_axis_sizes};
    use crate::{library::LibraryStore, pdf_view::RenderCache, settings::SettingsStore};
    use glyphst_translation_ui::DocumentTranslationJobManager;

    fn temporary_pdf_path(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "glyphst-{label}-{}-{}.pdf",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn write_minimal_pdf(path: &std::path::Path) {
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
    fn resizing_a_divider_preserves_unrelated_panes() {
        let mut sizes = [300.0, 300.0, 300.0];

        assert_eq!(resize_axis_sizes(&mut sizes, 0, 80.0, 200.0), 80.0);
        assert_eq!(sizes, [380.0, 220.0, 300.0]);
    }

    #[test]
    fn resizing_cascades_toward_the_constrained_side() {
        let mut sizes = [300.0, 240.0, 240.0];

        assert_eq!(resize_axis_sizes(&mut sizes, 0, 100.0, 200.0), 80.0);
        assert_eq!(sizes, [380.0, 200.0, 200.0]);
    }

    #[test]
    fn resizing_cannot_shrink_the_dragged_pane_below_minimum() {
        let mut sizes = [220.0, 300.0, 300.0];

        assert_eq!(resize_axis_sizes(&mut sizes, 0, -80.0, 200.0), -20.0);
        assert_eq!(sizes, [200.0, 320.0, 300.0]);
    }

    #[gpui::test]
    fn document_open_stages_a_loading_tab_without_blocking_tab_switches(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            cx.set_global(SettingsStore::memory());
            cx.set_global(LibraryStore::memory());
            cx.set_global(RenderCache::new());
        });
        let jobs = cx.update(|cx| cx.new(DocumentTranslationJobManager::new));
        let path = temporary_pdf_path("loading-tab");
        fs::create_dir(&path).unwrap();

        let (root, cx) = cx.add_window_view(|window, cx| {
            let pane = cx.new(|cx| Pane::empty(cx.focus_handle(), jobs.clone()));
            Root::new(pane, window, cx)
        });
        let pane = root.read_with(cx, |root, _| {
            root.view().clone().downcast::<Pane>().unwrap()
        });
        pane.update_in(cx, |pane, window, cx| {
            pane.focus_or_open_document(path.clone(), None, true, window, cx)
                .unwrap();
            assert!(matches!(pane.tabs.as_slice(), [Tab::LoadingDocument(_)]));

            pane.open_new_tab(window, cx);
            assert!(matches!(
                pane.tabs.get(pane.active_tab),
                Some(Tab::NewTab(_))
            ));
            assert!(
                pane.tabs
                    .iter()
                    .any(|tab| matches!(tab, Tab::LoadingDocument(_)))
            );
        });

        cx.run_until_parked();
        pane.read_with(cx, |pane, cx| {
            assert!(matches!(
                pane.tabs.get(pane.active_tab),
                Some(Tab::NewTab(_))
            ));
            let loading = pane
                .tabs
                .iter()
                .find_map(|tab| match tab {
                    Tab::LoadingDocument(loading) => Some(loading),
                    _ => None,
                })
                .unwrap();
            assert!(loading.read(cx).error.is_some());
        });

        fs::remove_dir(path).unwrap();
    }

    #[gpui::test]
    fn successful_document_preparation_replaces_the_loading_tab(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            cx.set_global(SettingsStore::memory());
            cx.set_global(LibraryStore::memory());
            cx.set_global(RenderCache::new());
        });
        let jobs = cx.update(|cx| cx.new(DocumentTranslationJobManager::new));
        let path = temporary_pdf_path("loading-success");
        write_minimal_pdf(&path);

        let (root, cx) = cx.add_window_view(|window, cx| {
            let pane = cx.new(|cx| Pane::empty(cx.focus_handle(), jobs.clone()));
            Root::new(pane, window, cx)
        });
        let pane = root.read_with(cx, |root, _| {
            root.view().clone().downcast::<Pane>().unwrap()
        });
        pane.update_in(cx, |pane, window, cx| {
            pane.focus_or_open_document(path.clone(), None, true, window, cx)
                .unwrap();
            assert!(matches!(pane.tabs.as_slice(), [Tab::LoadingDocument(_)]));
            pane.associate_open_job(0, 42, cx);
            let Tab::LoadingDocument(loading) = &pane.tabs[0] else {
                unreachable!();
            };
            assert_eq!(loading.read(cx).job_ids, vec![42]);
        });

        cx.run_until_parked();
        pane.read_with(cx, |pane, _| {
            assert!(matches!(pane.tabs.as_slice(), [Tab::Document(_)]));
        });

        fs::remove_file(path).unwrap();
    }
}
