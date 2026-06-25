use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use gpui::{
    AnyElement, Context, DragMoveEvent, Entity, EntityId, FocusHandle, Focusable, IntoElement,
    MouseButton, MouseUpEvent, PathPromptOptions, Pixels, Render, SharedString, Window,
    WindowHandle, div, point, prelude::*, px, rgb, rgba,
};

use crate::{
    command_palette::{CommandPalette, CommandPaletteDelegate},
    icons::{Icon, IconName},
    library::{DocumentPosition, LibraryStore, normalize_path},
    new_tab::{NewTab, NewTabEvent},
    open_workspace_window_at,
    pdf_view::{
        CloseTab, OpenFile, OpenNewTab, OpenSettings, PdfAnnotations, PdfOpenOptions, PdfView,
        PdfViewEvent, ToggleCommandPalette,
    },
    picker::{Picker, PickerEvent},
    settings::{SettingsStore, TabBarVisibility, setup_ui_font, ui_rems},
    settings_window::open_settings_window,
    theme::Theme,
};

#[derive(Clone)]
pub(crate) enum Tab {
    NewTab(Entity<NewTab>),
    Document(Entity<PdfView>),
}

#[derive(Clone)]
enum DraggedTabPayload {
    NewTab,
    Document(PathBuf),
}

#[derive(Clone)]
struct DraggedTab {
    source_window: WindowHandle<Workspace>,
    ix: usize,
    tab_id: EntityId,
    payload: DraggedTabPayload,
    label: SharedString,
    theme: Theme,
}

impl Render for DraggedTab {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .h(ui_rems(TOOLBAR_HEIGHT))
            .max_w(ui_rems(220.0))
            .min_w(ui_rems(120.0))
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum ToolbarMenu {
    Glyphst,
    File,
    Tab,
}

const TOOLBAR_HEIGHT: f32 = 32.0;
const TOOLBAR_MENU_TEXT_SIZE: f32 = 12.0;
const TOOLBAR_DROPDOWN_TEXT_SIZE: f32 = 13.0;
const TOOLBAR_DROPDOWN_WIDTH: f32 = 190.0;
const TOOLBAR_ICON_SIZE: f32 = 16.0;
const TOOLBAR_BUTTON_HEIGHT: f32 = 24.0;
const TOOLBAR_PADDING_X: f32 = 4.0;
const TOOLBAR_GAP: f32 = 4.0;
const TOOLBAR_GLYPHST_WIDTH: f32 = 84.0;
const TOOLBAR_FILE_WIDTH: f32 = 56.0;
const TOOLBAR_TAB_WIDTH: f32 = 52.0;

pub(crate) struct Workspace {
    tabs: Vec<Tab>,
    active_tab: usize,
    focus_handle: FocusHandle,
    command_palette: Option<Entity<CommandPalette>>,
    command_palette_previous_focus_handle: Option<FocusHandle>,
    toolbar_menu_open: bool,
    toolbar_open_menu: Option<ToolbarMenu>,
    dragged_tab_outside_window: Option<DraggedTab>,
}

impl Workspace {
    pub(crate) fn new(
        initial_path: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut workspace = Self {
            tabs: Vec::new(),
            active_tab: 0,
            focus_handle: cx.focus_handle(),
            command_palette: None,
            command_palette_previous_focus_handle: None,
            toolbar_menu_open: false,
            toolbar_open_menu: None,
            dragged_tab_outside_window: None,
        };

        let initial_tab = initial_path
            .and_then(
                |path| match workspace.create_document_tab(path, None, window, cx) {
                    Ok(tab) => Some(tab),
                    Err(error) => {
                        eprintln!("failed to open PDF: {error:#}");
                        None
                    }
                },
            )
            .unwrap_or_else(|| workspace.create_new_tab(window, cx));

        workspace.tabs.push(initial_tab);
        workspace.focus_handle.focus(window, cx);
        cx.observe_global::<SettingsStore>(|_, cx| cx.notify())
            .detach();
        workspace
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
        cx.subscribe_in(
            new_tab,
            window,
            |this, _, event: &NewTabEvent, window, cx| {
                let NewTabEvent::OpenDocument(path) = event;
                if let Err(error) =
                    this.focus_or_open_document(path.clone(), None, true, window, cx)
                {
                    eprintln!("failed to open PDF: {error:#}");
                }
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
        cx.subscribe_in(
            pdf_view,
            window,
            |this, _, event: &PdfViewEvent, window, cx| match event {
                PdfViewEvent::OpenDocument { path, position } => {
                    if let Err(error) =
                        this.focus_or_open_document(path.clone(), *position, false, window, cx)
                    {
                        eprintln!("failed to open PDF: {error:#}");
                    }
                }
            },
        )
        .detach();
    }

    fn toggle_command_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.command_palette.is_some() {
            self.close_command_palette(window, cx);
            return;
        }

        let Some(previous_focus_handle) = window.focused(cx) else {
            return;
        };
        let actions = window
            .available_actions(cx)
            .into_iter()
            .filter(|action| !action.partial_eq(&ToggleCommandPalette))
            .collect::<Vec<_>>();
        let delegate = CommandPaletteDelegate::new(actions);
        let picker_previous_focus_handle = previous_focus_handle.clone();
        let palette = cx.new(|cx| Picker::new(picker_previous_focus_handle, delegate, cx));

        cx.subscribe_in(&palette, window, |this, _, _: &PickerEvent, window, cx| {
            this.close_command_palette(window, cx);
        })
        .detach();

        palette.focus_handle(cx).focus(window, cx);
        self.command_palette_previous_focus_handle = Some(previous_focus_handle);
        self.command_palette = Some(palette);
        cx.notify();
    }

    fn close_command_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.command_palette.take().is_some() {
            self.command_palette_previous_focus_handle
                .take()
                .unwrap_or_else(|| self.focus_handle.clone())
                .focus(window, cx);
            cx.notify();
        }
    }

    fn close_toolbar_menu(&mut self, cx: &mut Context<Self>) {
        if self.toolbar_menu_open || self.toolbar_open_menu.is_some() {
            self.toolbar_menu_open = false;
            self.toolbar_open_menu = None;
            cx.notify();
        }
    }

    fn open_new_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.flush_active_tab(cx);
        let tab = self.create_new_tab(window, cx);
        self.tabs.push(tab);
        self.active_tab = self.tabs.len() - 1;
        self.focus_handle.focus(window, cx);
        cx.notify();
    }

    fn open_file(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let replace_active_new_tab = self.active_tab_is_new_tab();
        let dialog = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Open PDF".into()),
        });

        cx.spawn(async move |this, cx| match dialog.await {
            Ok(Ok(Some(paths))) => {
                let Some(path) = paths.into_iter().next() else {
                    return;
                };
                match this.update_in(cx, |this, window, cx| {
                    this.focus_or_open_document(path, None, replace_active_new_tab, window, cx)
                }) {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => eprintln!("failed to open selected PDF: {error:#}"),
                    Err(error) => eprintln!("failed to update workspace: {error:#}"),
                }
            }
            Ok(Err(error)) => eprintln!("failed to open file dialog: {error:#}"),
            Ok(Ok(None)) | Err(_) => {}
        })
        .detach();
    }

    pub(crate) fn open_document_external(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        let replace_active_new_tab = self.active_tab_is_new_tab();
        self.focus_or_open_document(path, None, replace_active_new_tab, window, cx)
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

    fn activate_existing_document(
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

    fn close_active_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_tab(self.active_tab, window, cx);
    }

    fn close_tab(&mut self, tab_ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if tab_ix >= self.tabs.len() {
            return;
        }

        self.flush_tab(tab_ix, cx);
        self.tabs.remove(tab_ix);

        if self.tabs.is_empty() {
            let tab = self.create_new_tab(window, cx);
            self.tabs.push(tab);
            self.active_tab = 0;
        } else if self.active_tab > tab_ix {
            self.active_tab -= 1;
        } else if self.active_tab >= self.tabs.len() {
            self.active_tab = self.tabs.len() - 1;
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

    fn handle_dragged_tab_drop(
        &mut self,
        dragged_tab: &DraggedTab,
        to: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.clear_dragged_tab_outside_window();

        if dragged_tab.source_window.window_id() == window.window_handle().window_id() {
            self.move_dragged_tab(dragged_tab, to, cx);
            return;
        }

        if let Err(error) = self.import_dragged_tab(dragged_tab, to, window, cx) {
            eprintln!("failed to move tab to window: {error:#}");
        }
    }

    fn move_dragged_tab(&mut self, dragged_tab: &DraggedTab, to: usize, cx: &mut Context<Self>) {
        let Some(from) = self.tab_ix_for_entity_id(dragged_tab.tab_id) else {
            return;
        };
        self.move_tab(from, to, cx);
    }

    fn import_dragged_tab(
        &mut self,
        dragged_tab: &DraggedTab,
        to: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        let source_has_tab = dragged_tab.source_window.update(cx, |source, _, cx| {
            if let Some(tab_ix) = source.tab_ix_for_entity_id(dragged_tab.tab_id) {
                source.flush_tab(tab_ix, cx);
                true
            } else {
                false
            }
        })?;

        if !source_has_tab {
            return Ok(());
        }

        self.import_dragged_tab_payload(&dragged_tab.payload, to, window, cx)?;

        dragged_tab
            .source_window
            .update(cx, |source, source_window, cx| {
                source.clear_dragged_tab_outside_window();
                if let Some(tab_ix) = source.tab_ix_for_entity_id(dragged_tab.tab_id) {
                    source.remove_detached_tab(tab_ix, source_window, cx);
                }
            })?;

        Ok(())
    }

    fn import_dragged_tab_payload(
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
            DraggedTabPayload::Document(path) => {
                let path = normalize_path(path);
                if let Some(existing_ix) = self.document_tab_ix(&path, cx) {
                    self.activate_existing_tab_at(existing_ix, to, window, cx);
                } else {
                    let tab = self.create_document_tab(path, None, window, cx)?;
                    self.insert_imported_tab(tab, to, window, cx);
                }
            }
        }

        Ok(())
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

    fn tab_ix_for_entity_id(&self, tab_id: EntityId) -> Option<usize> {
        self.tabs
            .iter()
            .position(|tab| Self::tab_entity_id(tab) == tab_id)
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

        if self.tab_ix_for_entity_id(dragged_tab.tab_id).is_none() {
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

        let Some(tab_ix) = self.tab_ix_for_entity_id(dragged_tab.tab_id) else {
            return;
        };

        let path = match &self.tabs[tab_ix] {
            Tab::Document(pdf_view) => Some(pdf_view.read(cx).path().to_path_buf()),
            Tab::NewTab(_) => None,
        };
        self.flush_tab(tab_ix, cx);

        let bounds = Self::detached_window_bounds(event, window);
        if let Err(error) = open_workspace_window_at(path, Some(bounds), cx) {
            eprintln!("failed to tear tab into new window: {error:#}");
            return;
        }

        self.remove_detached_tab(tab_ix, window, cx);
        cx.stop_propagation();
    }

    fn detached_window_bounds(event: &MouseUpEvent, window: &Window) -> gpui::Bounds<Pixels> {
        let source_bounds = window.bounds();
        let origin = point(
            source_bounds.origin.x + event.position.x - px(160.0),
            source_bounds.origin.y + event.position.y - px(16.0),
        );

        gpui::Bounds::new(origin, source_bounds.size)
    }

    fn remove_detached_tab(&mut self, tab_ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if tab_ix >= self.tabs.len() {
            return;
        }

        self.clear_dragged_tab_outside_window();
        self.tabs.remove(tab_ix);
        if self.tabs.is_empty() {
            window.remove_window();
            return;
        }

        if self.active_tab > tab_ix {
            self.active_tab -= 1;
        } else if self.active_tab >= self.tabs.len() {
            self.active_tab = self.tabs.len() - 1;
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

    fn document_tab_ix(&self, path: &Path, cx: &Context<Self>) -> Option<usize> {
        let path = normalize_path(path);
        self.tabs.iter().position(|tab| match tab {
            Tab::Document(pdf_view) => pdf_view.read(cx).path() == path.as_path(),
            Tab::NewTab(_) => false,
        })
    }

    fn active_tab_is_new_tab(&self) -> bool {
        matches!(self.tabs.get(self.active_tab), Some(Tab::NewTab(_)))
    }

    fn is_supported_document_path(path: &Path) -> bool {
        path.extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("pdf"))
    }

    fn tab_label(&self, tab_ix: usize, cx: &Context<Self>) -> SharedString {
        match self.tabs.get(tab_ix) {
            Some(Tab::Document(pdf_view)) => pdf_view.read(cx).file_name(),
            Some(Tab::NewTab(_)) | None => "New Tab".into(),
        }
    }

    fn dragged_tab_payload(&self, tab_ix: usize, cx: &Context<Self>) -> Option<DraggedTabPayload> {
        match self.tabs.get(tab_ix) {
            Some(Tab::Document(pdf_view)) => Some(DraggedTabPayload::Document(
                pdf_view.read(cx).path().to_path_buf(),
            )),
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

    fn active_document_label(&self, cx: &Context<Self>) -> Option<SharedString> {
        match self.tabs.get(self.active_tab) {
            Some(Tab::Document(pdf_view)) => Some(pdf_view.read(cx).file_name()),
            Some(Tab::NewTab(_)) | None => None,
        }
    }

    fn toolbar_icon_button(
        icon: IconName,
        active: bool,
        theme: Theme,
        on_click: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .flex_none()
            .w(ui_rems(28.0))
            .h(ui_rems(TOOLBAR_BUTTON_HEIGHT))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(4.0))
            .text_color(rgb(if active { theme.text } else { theme.text_muted }))
            .when(active, |this| this.bg(rgb(theme.element_hover)))
            .hover(move |style| {
                style
                    .bg(rgb(theme.element_hover))
                    .text_color(rgb(theme.text))
            })
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    on_click(this, window, cx);
                    cx.notify();
                    cx.stop_propagation();
                }),
            )
            .child(Icon::new(
                icon,
                ui_rems(TOOLBAR_ICON_SIZE),
                rgb(if active { theme.text } else { theme.text_muted }),
            ))
            .into_any_element()
    }

    fn render_toolbar_menu_button(
        &self,
        label: &'static str,
        icon: IconName,
        menu: ToolbarMenu,
        width: f32,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let active = self.toolbar_open_menu == Some(menu);

        div()
            .w(ui_rems(width))
            .h(ui_rems(TOOLBAR_BUTTON_HEIGHT))
            .px_2()
            .flex()
            .gap_1()
            .items_center()
            .justify_center()
            .rounded(px(4.0))
            .text_size(ui_rems(TOOLBAR_MENU_TEXT_SIZE))
            .text_color(rgb(if active { theme.text } else { theme.text_muted }))
            .when(active, |this| this.bg(rgb(theme.element_hover)))
            .hover(move |style| {
                style
                    .bg(rgb(theme.element_hover))
                    .text_color(rgb(theme.text))
            })
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    this.toolbar_menu_open = true;
                    this.toolbar_open_menu = if this.toolbar_open_menu == Some(menu) {
                        None
                    } else {
                        Some(menu)
                    };
                    cx.notify();
                    cx.stop_propagation();
                }),
            )
            .child(Icon::new(
                icon,
                ui_rems(TOOLBAR_ICON_SIZE),
                rgb(if active { theme.text } else { theme.text_muted }),
            ))
            .child(label)
            .into_any_element()
    }

    fn render_toolbar(&self, theme: Theme, cx: &mut Context<Self>) -> AnyElement {
        let expanded = self.toolbar_menu_open || self.active_tab_is_new_tab();
        let mut toolbar = div()
            .flex_none()
            .h(ui_rems(TOOLBAR_HEIGHT))
            .flex()
            .items_center()
            .gap_1()
            .px_1()
            .border_b(px(1.0))
            .border_color(rgb(theme.border))
            .bg(rgb(theme.tab_bar_background))
            .text_size(ui_rems(TOOLBAR_MENU_TEXT_SIZE))
            .text_color(rgb(theme.text))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.close_toolbar_menu(cx);
                    cx.stop_propagation();
                }),
            );

        if expanded {
            toolbar = toolbar
                .child(self.render_toolbar_menu_button(
                    "Glyphst",
                    IconName::AppWindow,
                    ToolbarMenu::Glyphst,
                    TOOLBAR_GLYPHST_WIDTH,
                    theme,
                    cx,
                ))
                .child(self.render_toolbar_menu_button(
                    "File",
                    IconName::File,
                    ToolbarMenu::File,
                    TOOLBAR_FILE_WIDTH,
                    theme,
                    cx,
                ))
                .child(self.render_toolbar_menu_button(
                    "Tab",
                    IconName::Plus,
                    ToolbarMenu::Tab,
                    TOOLBAR_TAB_WIDTH,
                    theme,
                    cx,
                ));
        } else {
            toolbar = toolbar.child(Self::toolbar_icon_button(
                IconName::Menu,
                false,
                theme,
                |this, _, cx| {
                    this.toolbar_menu_open = true;
                    this.toolbar_open_menu = Some(ToolbarMenu::Glyphst);
                    cx.notify();
                },
                cx,
            ));
            if let Some(label) = self.active_document_label(cx) {
                toolbar = toolbar.child(
                    div()
                        .min_w_0()
                        .max_w(px(360.0))
                        .truncate()
                        .px_1()
                        .text_color(rgb(theme.text_muted))
                        .child(label),
                );
            }
        }

        toolbar.child(div().flex_1()).into_any_element()
    }

    fn render_toolbar_menu_item(
        label: &'static str,
        theme: Theme,
        handler: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .flex()
            .items_center()
            .h(ui_rems(30.0))
            .px_3()
            .text_size(ui_rems(TOOLBAR_DROPDOWN_TEXT_SIZE))
            .text_color(rgb(theme.text))
            .cursor_pointer()
            .hover(move |style| style.bg(rgb(theme.element_hover)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    handler(this, window, cx);
                    this.close_toolbar_menu(cx);
                    cx.stop_propagation();
                }),
            )
            .child(label)
            .into_any_element()
    }

    fn toolbar_menu_left(menu: ToolbarMenu) -> gpui::Rems {
        match menu {
            ToolbarMenu::Glyphst => ui_rems(TOOLBAR_PADDING_X),
            ToolbarMenu::File => ui_rems(TOOLBAR_PADDING_X + TOOLBAR_GLYPHST_WIDTH + TOOLBAR_GAP),
            ToolbarMenu::Tab => ui_rems(
                TOOLBAR_PADDING_X
                    + TOOLBAR_GLYPHST_WIDTH
                    + TOOLBAR_GAP
                    + TOOLBAR_FILE_WIDTH
                    + TOOLBAR_GAP,
            ),
        }
    }

    fn render_toolbar_dropdown(
        &self,
        menu: ToolbarMenu,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut dropdown = div()
            .occlude()
            .absolute()
            .top_0()
            .left(Self::toolbar_menu_left(menu))
            .w(ui_rems(TOOLBAR_DROPDOWN_WIDTH))
            .py_1()
            .border_1()
            .border_color(rgb(theme.border))
            .bg(rgb(theme.surface_background))
            .text_size(ui_rems(TOOLBAR_DROPDOWN_TEXT_SIZE))
            .text_color(rgb(theme.text))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _, _, cx| {
                    cx.stop_propagation();
                }),
            );

        match menu {
            ToolbarMenu::Glyphst => {
                dropdown = dropdown
                    .child(Self::render_toolbar_menu_item(
                        "About Glyphst",
                        theme,
                        |_, _, _| {},
                        cx,
                    ))
                    .child(Self::render_toolbar_menu_item(
                        "Check for Updates",
                        theme,
                        |_, _, _| {},
                        cx,
                    ))
                    .child(Self::render_toolbar_menu_item(
                        "Open Settings",
                        theme,
                        |_, _, cx| open_settings_window(cx),
                        cx,
                    ))
                    .child(Self::render_toolbar_menu_item(
                        "Quit Glyphst",
                        theme,
                        |_, _, cx| cx.quit(),
                        cx,
                    ));
            }
            ToolbarMenu::File => {
                dropdown = dropdown.child(Self::render_toolbar_menu_item(
                    "Open File",
                    theme,
                    |this, window, cx| this.open_file(window, cx),
                    cx,
                ));
            }
            ToolbarMenu::Tab => {
                dropdown = dropdown
                    .child(Self::render_toolbar_menu_item(
                        "New Tab",
                        theme,
                        |this, window, cx| this.open_new_tab(window, cx),
                        cx,
                    ))
                    .child(Self::render_toolbar_menu_item(
                        "Close Tab",
                        theme,
                        |this, window, cx| this.close_active_tab(window, cx),
                        cx,
                    ));
            }
        }

        div()
            .absolute()
            .top(ui_rems(TOOLBAR_HEIGHT))
            .left_0()
            .size_full()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.close_toolbar_menu(cx);
                    cx.stop_propagation();
                }),
            )
            .child(dropdown)
            .into_any_element()
    }

    fn render_tab(
        &self,
        tab_ix: usize,
        source_window: WindowHandle<Workspace>,
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
            .id(("tab", tab_ix))
            .h(ui_rems(TOOLBAR_HEIGHT))
            .max_w(ui_rems(220.0))
            .min_w(ui_rems(120.0))
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
                if dragged_tab.source_window != source_window || tab_ix < dragged_tab.ix {
                    style.border_l(px(2.0)).border_color(rgb(theme.accent))
                } else if tab_ix > dragged_tab.ix {
                    style.border_r(px(2.0)).border_color(rgb(theme.accent))
                } else {
                    style
                }
            })
            .on_drop(
                cx.listener(move |this, dragged_tab: &DraggedTab, window, cx| {
                    this.handle_dragged_tab_drop(dragged_tab, tab_ix, window, cx);
                }),
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
                    .flex_none()
                    .w(ui_rems(22.0))
                    .h(ui_rems(22.0))
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
                        cx.listener(move |this, _, window, cx| {
                            this.close_tab(tab_ix, window, cx);
                            cx.stop_propagation();
                        }),
                    )
                    .child(Icon::new(IconName::X, ui_rems(14.0), rgb(theme.text_muted))),
            )
            .into_any_element()
    }

    fn render_tab_bar(&self, theme: Theme, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let source_window = window
            .window_handle()
            .downcast::<Workspace>()
            .expect("workspace window should have Workspace root view");
        let append_ix = self.tabs.len();
        let tabs = (0..self.tabs.len())
            .map(|tab_ix| self.render_tab(tab_ix, source_window, theme, cx))
            .collect::<Vec<_>>();

        div()
            .flex_none()
            .h(ui_rems(TOOLBAR_HEIGHT))
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
                    .on_drop(
                        cx.listener(move |this, dragged_tab: &DraggedTab, window, cx| {
                            this.handle_dragged_tab_drop(dragged_tab, append_ix, window, cx);
                        }),
                    ),
            )
            .into_any_element()
    }

    fn render_top_aligned_overlay(
        content: impl IntoElement + 'static,
        close: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
        theme: Theme,
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
            .bg(rgba(theme.overlay))
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
        let ui_font = setup_ui_font(window, cx);
        let (theme, tab_bar_visibility) = {
            let store = cx.global::<SettingsStore>();
            (store.resolved_theme, store.settings.tab_bar_visibility)
        };
        let show_tab_bar = tab_bar_visibility == TabBarVisibility::Always || self.tabs.len() >= 2;

        div()
            .id("workspace")
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .key_context("Workspace")
            .track_focus(&self.focus_handle)
            .font(ui_font)
            .on_drag_move::<DraggedTab>(cx.listener(
                |this, event: &DragMoveEvent<DraggedTab>, window, cx| {
                    this.track_dragged_tab_outside_window(event, window, cx);
                },
            ))
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
            .on_drop(cx.listener(|this, dragged_tab: &DraggedTab, window, cx| {
                this.handle_dragged_tab_drop(dragged_tab, this.tabs.len(), window, cx);
            }))
            .on_action(cx.listener(|this, _: &OpenFile, window, cx| {
                this.open_file(window, cx);
            }))
            .on_action(cx.listener(|this, _: &OpenNewTab, window, cx| {
                this.open_new_tab(window, cx);
            }))
            .on_action(cx.listener(|this, _: &CloseTab, window, cx| {
                this.close_active_tab(window, cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleCommandPalette, window, cx| {
                this.toggle_command_palette(window, cx);
            }))
            .on_action(|_: &OpenSettings, _, cx| open_settings_window(cx))
            .child(self.render_toolbar(theme, cx))
            .when(show_tab_bar, |this| {
                this.child(self.render_tab_bar(theme, window, cx))
            })
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .child(self.active_tab_content()),
            )
            .when_some(self.toolbar_open_menu, |this, menu| {
                this.child(self.render_toolbar_dropdown(menu, theme, cx))
            })
            .when_some(self.command_palette.clone(), |this, palette| {
                this.child(Self::render_top_aligned_overlay(
                    palette,
                    |this, window, cx| this.close_command_palette(window, cx),
                    theme,
                    cx,
                ))
            })
    }
}
