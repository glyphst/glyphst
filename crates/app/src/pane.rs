use std::{
    cell::RefCell,
    cmp::Ordering,
    iter,
    path::{Path, PathBuf},
    rc::Rc,
};

use anyhow::{Result, bail};
use gpui::{
    Along, AnyElement, App, AppContext, Axis, BorderStyle, Bounds, Context, CursorStyle,
    DispatchPhase, DragMoveEvent, Element, ElementId, Entity, EntityId, EventEmitter, FocusHandle,
    GlobalElementId, Hitbox, HitboxBehavior, IntoElement, LayoutId, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, ParentElement, Pixels, Point, Render, SharedString, Size, Style,
    Styled, Window, WindowHandle, div, fill, point, prelude::*, px, quad, relative, rgb, rgba,
    size, transparent_black,
};
use smallvec::SmallVec;

use crate::{
    icons::{Icon, IconName},
    library::{DocumentPosition, LibraryStore, normalize_path},
    new_tab::{NewTab, NewTabEvent},
    pdf_view::{PdfAnnotations, PdfOpenOptions, PdfView, PdfViewEvent},
    settings::{SettingsStore, TabBarVisibility, ui_rems},
    theme::Theme,
    workspace::Workspace,
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

gpui::actions!(
    pane,
    [
        SplitLeft,
        SplitRight,
        SplitUp,
        SplitDown,
        SplitVertical,
        SplitHorizontal,
    ]
);

#[derive(Clone)]
pub(crate) enum Tab {
    NewTab(Entity<NewTab>),
    Document(Entity<PdfView>),
}

#[derive(Clone)]
pub(crate) enum DraggedTabPayload {
    NewTab,
    Document {
        path: PathBuf,
        position: Option<DocumentPosition>,
    },
}

#[derive(Clone)]
pub(crate) struct DraggedTab {
    pub(crate) source_window: WindowHandle<Workspace>,
    pub(crate) source_pane: Entity<Pane>,
    pub(crate) ix: usize,
    pub(crate) tab_id: EntityId,
    pub(crate) payload: DraggedTabPayload,
    pub(crate) label: SharedString,
    pub(crate) theme: Theme,
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
    OpenDocument {
        path: PathBuf,
        position: Option<DocumentPosition>,
        replace_active_new_tab: bool,
    },
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
    drag_split_direction: Option<SplitDirection>,
}

impl Pane {
    pub(crate) fn new(
        initial_path: Option<PathBuf>,
        initial_position: Option<DocumentPosition>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut pane = Self {
            tabs: Vec::new(),
            active_tab: 0,
            focus_handle: cx.focus_handle(),
            drag_split_direction: None,
        };

        let initial_tab = initial_path
            .and_then(
                |path| match pane.create_document_tab(path, initial_position, window, cx) {
                    Ok(tab) => Some(tab),
                    Err(error) => {
                        eprintln!("failed to open PDF: {error:#}");
                        None
                    }
                },
            )
            .unwrap_or_else(|| pane.create_new_tab(window, cx));
        pane.tabs.push(initial_tab);
        pane
    }

    pub(crate) fn focus_handle(&self) -> FocusHandle {
        self.focus_handle.clone()
    }

    pub(crate) fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_handle.focus(window, cx);
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
            Some(Tab::NewTab(_)) | None => None,
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
            Some(Tab::NewTab(_)) | None => None,
        }
    }

    pub(crate) fn document_tab_ix(&self, path: &Path, cx: &App) -> Option<usize> {
        let path = normalize_path(path);
        self.tabs.iter().position(|tab| match tab {
            Tab::Document(pdf_view) => pdf_view.read(cx).path() == path.as_path(),
            Tab::NewTab(_) => false,
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
            self.tabs.remove(remove_tab_ix);
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

        self.focus_handle.focus(window, cx);
        cx.notify();
    }

    pub(crate) fn focus_or_open_document(
        &mut self,
        path: PathBuf,
        position: Option<DocumentPosition>,
        replace_active_new_tab: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        let path = normalize_path(&path);
        if let Some(tab_ix) = self.document_tab_ix(&path, cx) {
            self.activate_existing_document(tab_ix, position, replace_active_new_tab, window, cx);
            return Ok(());
        }

        let tab = self.create_document_tab(path, position, window, cx)?;
        if replace_active_new_tab && self.active_tab_is_new_tab() {
            self.tabs[self.active_tab] = tab;
        } else {
            self.flush_active_tab(cx);
            self.tabs.push(tab);
            self.active_tab = self.tabs.len() - 1;
        }
        self.focus_handle.focus(window, cx);
        cx.notify();
        Ok(())
    }

    pub(crate) fn open_new_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.flush_active_tab(cx);
        let tab = self.create_new_tab(window, cx);
        self.tabs.push(tab);
        self.active_tab = self.tabs.len() - 1;
        self.focus_handle.focus(window, cx);
        cx.notify();
    }

    pub(crate) fn close_tab_at(&mut self, tab_ix: usize, cx: &mut Context<Self>) -> bool {
        if tab_ix >= self.tabs.len() {
            return self.tabs.is_empty();
        }

        self.flush_tab(tab_ix, cx);
        self.tabs.remove(tab_ix);

        if self.active_tab > tab_ix {
            self.active_tab -= 1;
        } else if self.active_tab >= self.tabs.len() && !self.tabs.is_empty() {
            self.active_tab = self.tabs.len() - 1;
        }

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
            cx.notify();
        }
    }

    pub(crate) fn remove_tab_for_move(&mut self, tab_id: EntityId, cx: &mut Context<Self>) -> bool {
        let Some(tab_ix) = self.tab_ix_for_entity_id(tab_id) else {
            return self.tabs.is_empty();
        };
        self.flush_tab(tab_ix, cx);
        self.tabs.remove(tab_ix);
        if self.active_tab > tab_ix {
            self.active_tab -= 1;
        } else if self.active_tab >= self.tabs.len() && !self.tabs.is_empty() {
            self.active_tab = self.tabs.len() - 1;
        }
        cx.notify();
        self.tabs.is_empty()
    }

    pub(crate) fn has_tab_id(&self, tab_id: EntityId) -> bool {
        self.tab_ix_for_entity_id(tab_id).is_some()
    }

    pub(crate) fn tab_ix_for_entity_id(&self, tab_id: EntityId) -> Option<usize> {
        self.tabs
            .iter()
            .position(|tab| Self::tab_entity_id(tab) == tab_id)
    }

    pub(crate) fn import_dragged_tab_payload(
        &mut self,
        payload: &DraggedTabPayload,
        to: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        match payload {
            DraggedTabPayload::NewTab => {
                let tab = self.create_new_tab(window, cx);
                self.insert_imported_tab(tab, to, window, cx);
            }
            DraggedTabPayload::Document { path, position } => {
                let path = normalize_path(path);
                if let Some(existing_ix) = self.document_tab_ix(&path, cx) {
                    self.activate_existing_tab_at(existing_ix, to, window, cx);
                } else {
                    let tab = self.create_document_tab(path, *position, window, cx)?;
                    self.insert_imported_tab(tab, to, window, cx);
                }
            }
        }

        Ok(())
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

    fn create_new_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Tab {
        let focus_handle = self.focus_handle.clone();
        let new_tab = cx.new(|cx| NewTab::new(focus_handle, cx));
        self.subscribe_to_new_tab(&new_tab, window, cx);
        Tab::NewTab(new_tab)
    }

    fn create_document_tab(
        &mut self,
        path: PathBuf,
        position: Option<DocumentPosition>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<Tab> {
        let path = normalize_path(&path);
        if !path.exists() {
            bail!("PDF no longer exists: {}", path.display());
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
            .unwrap_or(DocumentPosition {
                scroll_x_doc: 0.0,
                scroll_y_doc: 0.0,
                zoom: behavior.default_zoom,
                page: 1,
            });
        let annotations = PdfAnnotations {
            highlights: cx.global::<LibraryStore>().highlights_for(&path),
            notes: cx.global::<LibraryStore>().notes_for(&path),
        };
        let sidebar_widths = cx.global::<LibraryStore>().sidebar_widths();
        let mut pdf_view = PdfView::open(
            &path,
            self.focus_handle.clone(),
            behavior,
            window_settings,
            theme,
            PdfOpenOptions {
                initial_position: Some(initial_position),
                sidebar_widths,
                annotations,
            },
        )?;
        let pdf_view = cx.new(|cx| {
            PdfView::register_cache_eviction(cx);
            pdf_view.start_loading(cx);
            pdf_view
        });
        self.subscribe_to_pdf_view(&pdf_view, window, cx);

        cx.global_mut::<LibraryStore>()
            .record_history(&path, initial_position);
        if let Err(error) = cx.global::<LibraryStore>().save() {
            eprintln!("failed to save PDF history: {error:#}");
        }

        Ok(Tab::Document(pdf_view))
    }

    fn subscribe_to_new_tab(
        &self,
        new_tab: &Entity<NewTab>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.subscribe_in(new_tab, window, |_, _, event: &NewTabEvent, _, cx| {
            let NewTabEvent::OpenDocument(path) = event;
            cx.emit(PaneEvent::OpenDocument {
                path: path.clone(),
                position: None,
                replace_active_new_tab: true,
            });
        })
        .detach();
    }

    fn subscribe_to_pdf_view(
        &self,
        pdf_view: &Entity<PdfView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.subscribe_in(
            pdf_view,
            window,
            |_, _, event: &PdfViewEvent, _, cx| match event {
                PdfViewEvent::OpenDocument { path, position } => {
                    cx.emit(PaneEvent::OpenDocument {
                        path: path.clone(),
                        position: *position,
                        replace_active_new_tab: false,
                    });
                }
            },
        )
        .detach();
    }

    fn activate_tab(&mut self, tab_ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if tab_ix >= self.tabs.len() {
            return;
        }
        if self.active_tab != tab_ix {
            self.flush_active_tab(cx);
            self.active_tab = tab_ix;
        }
        self.focus_handle.focus(window, cx);
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

        cx.notify();
    }

    fn insert_imported_tab(
        &mut self,
        tab: Tab,
        to: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.flush_active_tab(cx);
        let tab_ix = to.min(self.tabs.len());
        self.tabs.insert(tab_ix, tab);
        self.active_tab = tab_ix;
        self.focus_handle.focus(window, cx);
        cx.notify();
    }

    fn activate_existing_tab_at(
        &mut self,
        existing_ix: usize,
        to: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab_id) = self.tabs.get(existing_ix).map(Self::tab_entity_id) else {
            return;
        };

        self.move_tab(existing_ix, to, cx);
        if let Some(active_ix) = self.tab_ix_for_entity_id(tab_id) {
            self.active_tab = active_ix;
        }
        self.focus_handle.focus(window, cx);
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
            .is_some_and(|extension| extension.eq_ignore_ascii_case("pdf"))
    }

    fn tab_label(&self, tab_ix: usize, cx: &App) -> SharedString {
        match self.tabs.get(tab_ix) {
            Some(Tab::Document(pdf_view)) => pdf_view.read(cx).file_name(),
            Some(Tab::NewTab(_)) | None => "New Tab".into(),
        }
    }

    fn dragged_tab_payload(&self, tab_ix: usize, cx: &App) -> Option<DraggedTabPayload> {
        match self.tabs.get(tab_ix) {
            Some(Tab::Document(pdf_view)) => {
                let pdf_view = pdf_view.read(cx);
                Some(DraggedTabPayload::Document {
                    path: pdf_view.path().to_path_buf(),
                    position: Some(pdf_view.current_document_position()),
                })
            }
            Some(Tab::NewTab(_)) => Some(DraggedTabPayload::NewTab),
            None => None,
        }
    }

    fn tab_entity_id(tab: &Tab) -> EntityId {
        match tab {
            Tab::NewTab(new_tab) => new_tab.entity_id(),
            Tab::Document(pdf_view) => pdf_view.entity_id(),
        }
    }

    fn active_tab_content(&self) -> AnyElement {
        match self.tabs.get(self.active_tab) {
            Some(Tab::Document(pdf_view)) => pdf_view.clone().into_any_element(),
            Some(Tab::NewTab(new_tab)) => new_tab.clone().into_any_element(),
            None => div().into_any_element(),
        }
    }

    fn render_tab(
        &self,
        tab_ix: usize,
        source_window: WindowHandle<Workspace>,
        source_pane: Entity<Pane>,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let selected = tab_ix == self.active_tab;
        let label = self.tab_label(tab_ix, cx);
        let Some(payload) = self.dragged_tab_payload(tab_ix, cx) else {
            return div().into_any_element();
        };
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
            .on_drag(
                DraggedTab {
                    source_window,
                    source_pane: source_pane.clone(),
                    ix: tab_ix,
                    tab_id,
                    payload,
                    label: label.clone(),
                    theme,
                },
                |tab, _, _, cx| cx.new(|_| tab.clone()),
            )
            .drag_over::<DraggedTab>(move |style, dragged_tab, _, _| {
                let style = style.bg(rgb(theme.element_hover));
                if dragged_tab.source_pane != source_pane || tab_ix < dragged_tab.ix {
                    style.border_l(px(2.0)).border_color(rgb(theme.accent))
                } else if tab_ix > dragged_tab.ix {
                    style.border_r(px(2.0)).border_color(rgb(theme.accent))
                } else {
                    style
                }
            })
            .on_drop(cx.listener(move |_, dragged_tab: &DraggedTab, _, cx| {
                cx.emit(PaneEvent::TabDrop {
                    dragged_tab: Box::new(dragged_tab.clone()),
                    target: PaneDropTarget::Tab(tab_ix),
                });
            }))
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
        let source_window = window
            .window_handle()
            .downcast::<Workspace>()
            .expect("workspace window should have Workspace root view");
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
        let threshold = rect.width.min(rect.height) * DROP_TARGET_SIZE;
        let relative_cursor = Point::new(
            event.event.position.x - event.bounds.left(),
            event.event.position.y - event.bounds.top(),
        );

        let direction = if relative_cursor.x < threshold
            || relative_cursor.x > rect.width - threshold
            || relative_cursor.y < threshold
            || relative_cursor.y > rect.height - threshold
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
        let mut overlay = div()
            .invisible()
            .absolute()
            .bg(rgba(theme.overlay))
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
            ));

        let half = relative(0.5);
        overlay = match direction {
            None => overlay.top_0().right_0().bottom_0().left_0(),
            Some(SplitDirection::Up) => overlay.top_0().left_0().right_0().h(half),
            Some(SplitDirection::Down) => overlay.left_0().bottom_0().right_0().h(half),
            Some(SplitDirection::Left) => overlay.top_0().left_0().bottom_0().w(half),
            Some(SplitDirection::Right) => overlay.top_0().bottom_0().right_0().w(half),
        };

        overlay.into_any_element()
    }
}

impl EventEmitter<PaneEvent> for Pane {}

impl Render for Pane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (theme, tab_bar_visibility) = {
            let store = cx.global::<SettingsStore>();
            (store.resolved_theme, store.settings.tab_bar_visibility)
        };
        let show_tab_bar = tab_bar_visibility == TabBarVisibility::Always || self.tabs.len() >= 2;

        div()
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .overflow_hidden()
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
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        self.root.render(0, active_pane, theme, window, cx).element
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
            Member::Axis(axis) => axis.render(basis + 1, active_pane, theme, window, cx),
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
                let result = member.render((basis + ix) * 10, active_pane, theme, window, cx);
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
}

struct PaneAxisLayout {
    dragged_handle: Rc<RefCell<Option<usize>>>,
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
    ix: usize,
    axis: Axis,
    child_start: Point<Pixels>,
    container_size: Size<Pixels>,
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
            ix,
            axis,
            child_start,
            container_size,
        } = args;
        let min_size = match axis {
            Axis::Horizontal => px(HORIZONTAL_MIN_SIZE),
            Axis::Vertical => px(VERTICAL_MIN_SIZE),
        };
        let container_along = container_size.along(axis);
        if container_along <= px(0.0) {
            return;
        }

        let mut flexes = flexes.borrow_mut();
        if flexes.len() < 2 || ix + 1 >= flexes.len() {
            return;
        }

        let size = |ix: usize, flexes: &[f32]| container_along * (flexes[ix] / flexes.len() as f32);
        if min_size - px(1.0) > size(ix, flexes.as_slice()) {
            return;
        }

        let mut proposed_current_pixel_change =
            (event.position - child_start).along(axis) - size(ix, flexes.as_slice());

        let mut successors = iter::from_fn({
            let forward = proposed_current_pixel_change > px(0.0);
            let mut offset = 0usize;
            let len = flexes.len();
            move || {
                let result = if forward {
                    (ix + 1 + offset < len).then_some(ix + offset)
                } else {
                    (ix >= offset).then_some(ix - offset)
                };
                offset += 1;
                result
            }
        });

        while proposed_current_pixel_change.abs() > px(0.0) {
            let Some(current_ix) = successors.next() else {
                break;
            };
            if current_ix + 1 >= flexes.len() {
                break;
            }

            let next_target_size = Pixels::max(
                size(current_ix + 1, flexes.as_slice()) - proposed_current_pixel_change,
                min_size,
            );
            let current_target_size = Pixels::max(
                size(current_ix, flexes.as_slice()) + size(current_ix + 1, flexes.as_slice())
                    - next_target_size,
                min_size,
            );
            let current_pixel_change = current_target_size - size(current_ix, flexes.as_slice());
            let flex_change = flexes.len() as f32 * current_pixel_change / container_along;
            flexes[current_ix] += flex_change;
            flexes[current_ix + 1] -= flex_change;
            proposed_current_pixel_change -= current_pixel_change;
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
        let dragged_handle = window.with_element_state::<Rc<RefCell<Option<usize>>>, _>(
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
            dragged_handle,
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

            if child.is_leaf_pane && self.active_pane_ix != Some(ix) {
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
                    .dragged_handle
                    .borrow()
                    .is_some_and(|dragged_ix| dragged_ix == ix)
                {
                    window.set_window_cursor_style(cursor_style);
                } else {
                    window.set_cursor_style(cursor_style, &handle.hitbox);
                }

                window.paint_quad(fill(handle.divider_bounds, rgb(self.theme.border)));

                window.on_mouse_event({
                    let dragged_handle = layout.dragged_handle.clone();
                    let flexes = self.flexes.clone();
                    let handle_hitbox = handle.hitbox.clone();
                    move |event: &MouseDownEvent, phase: DispatchPhase, window, cx| {
                        if phase.bubble() && handle_hitbox.is_hovered(window) {
                            dragged_handle.replace(Some(ix));
                            if event.click_count >= 2 {
                                let len = flexes.borrow().len();
                                *flexes.borrow_mut() = vec![1.0; len];
                                window.refresh();
                            }
                            cx.stop_propagation();
                        }
                    }
                });

                window.on_mouse_event({
                    let dragged_handle = layout.dragged_handle.clone();
                    let flexes = self.flexes.clone();
                    let child_bounds = child.bounds;
                    let axis = self.axis;
                    move |event: &MouseMoveEvent, phase: DispatchPhase, window, cx| {
                        if phase.bubble() && *dragged_handle.borrow() == Some(ix) {
                            Self::compute_resize(
                                ResizeComputation {
                                    flexes: flexes.clone(),
                                    event,
                                    ix,
                                    axis,
                                    child_start: child_bounds.origin,
                                    container_size: bounds.size,
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
            let dragged_handle = layout.dragged_handle.clone();
            move |_: &MouseUpEvent, phase: DispatchPhase, _, _| {
                if phase.bubble() {
                    dragged_handle.replace(None);
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
