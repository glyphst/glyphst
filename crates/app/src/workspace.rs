use std::path::{Path, PathBuf};

use anyhow::Result;
use gpui::{
    AnyElement, Context, DragMoveEvent, Entity, FocusHandle, Focusable, IntoElement, MouseButton,
    MouseMoveEvent, MouseUpEvent, PathPromptOptions, Pixels, Render, SharedString, Window,
    WindowAppearance, WindowControlArea, div, point, prelude::*, px, rgb, rgba,
};

use crate::{
    command_palette::{CommandPalette, CommandPaletteDelegate},
    icons::{Icon, IconName},
    library::{DocumentPosition, normalize_path},
    open_workspace_window_at,
    pane::{
        DraggedTab, DraggedTabPayload, Pane, PaneDropTarget, PaneEvent, PaneGroup, SplitDirection,
        SplitDown, SplitHorizontal, SplitLeft, SplitRight, SplitUp, SplitVertical,
    },
    pdf_view::{
        CloseTab, OpenFile, OpenKeybindings, OpenNewTab, OpenSettings, ToggleCommandPalette,
    },
    picker::{Picker, PickerEvent},
    settings::{SettingsStore, setup_ui_font, ui_rems},
    settings_window::open_settings_window,
    style::RADIUS_SM,
    theme::{Theme, ThemeAppearance, ThemeAppearanceMode},
    theme_selector::{ThemeSelector, ThemeSelectorDelegate},
};

gpui::actions!(
    workspace,
    [
        ActivatePaneLeft,
        ActivatePaneRight,
        ActivatePaneUp,
        ActivatePaneDown,
        ActivateNextPane,
        ActivatePreviousPane,
        SwapPaneLeft,
        SwapPaneRight,
        SwapPaneUp,
        SwapPaneDown,
        MovePaneLeft,
        MovePaneRight,
        MovePaneUp,
        MovePaneDown,
        ResizePaneLeft,
        ResizePaneRight,
        ResizePaneUp,
        ResizePaneDown,
        CloseOtherPanes,
        ResetPaneSizes,
        ToggleMode,
        ToggleThemeSelector,
    ]
);

#[derive(Clone, Copy, PartialEq, Eq)]
enum ToolbarMenu {
    Glyphst,
    File,
    Tab,
}

const TOOLBAR_HEIGHT: f32 = 32.0;
const TRAFFIC_LIGHT_PADDING: f32 = 71.0;
const TOOLBAR_MENU_TEXT_SIZE: f32 = 12.0;
const TOOLBAR_DROPDOWN_TEXT_SIZE: f32 = 13.0;
const TOOLBAR_DROPDOWN_WIDTH: f32 = 210.0;
const TOOLBAR_ICON_SIZE: f32 = 16.0;
const TOOLBAR_BUTTON_HEIGHT: f32 = 24.0;
const TOOLBAR_PADDING_X: f32 = 4.0;
const TOOLBAR_GAP: f32 = 4.0;
const TOOLBAR_GLYPHST_WIDTH: f32 = 84.0;
const TOOLBAR_FILE_WIDTH: f32 = 56.0;
const TOOLBAR_TAB_WIDTH: f32 = 52.0;

pub(crate) struct Workspace {
    pane_group: PaneGroup,
    active_pane: Entity<Pane>,
    focus_handle: FocusHandle,
    command_palette: Option<Entity<CommandPalette>>,
    command_palette_previous_focus_handle: Option<FocusHandle>,
    theme_selector: Option<Entity<ThemeSelector>>,
    theme_selector_previous_focus_handle: Option<FocusHandle>,
    toolbar_menu_open: bool,
    toolbar_open_menu: Option<ToolbarMenu>,
    dragged_tab_outside_window: Option<DraggedTab>,
    should_move_window: bool,
}

impl Workspace {
    pub(crate) fn new(
        initial_path: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        let pane = cx.new(|cx| Pane::new(initial_path, None, window, cx));
        let workspace = Self {
            pane_group: PaneGroup::new(pane.clone()),
            active_pane: pane.clone(),
            focus_handle,
            command_palette: None,
            command_palette_previous_focus_handle: None,
            theme_selector: None,
            theme_selector_previous_focus_handle: None,
            toolbar_menu_open: false,
            toolbar_open_menu: None,
            dragged_tab_outside_window: None,
            should_move_window: false,
        };

        workspace.subscribe_to_pane(&pane, window, cx);
        workspace.focus_active_pane(window, cx);
        cx.observe_global::<SettingsStore>(|_, cx| cx.notify())
            .detach();

        let system_appearance = match window.appearance() {
            WindowAppearance::Light | WindowAppearance::VibrantLight => ThemeAppearance::Light,
            WindowAppearance::Dark | WindowAppearance::VibrantDark => ThemeAppearance::Dark,
        };
        cx.global_mut::<SettingsStore>()
            .set_system_appearance(system_appearance);

        workspace
    }

    pub(crate) fn open_document_external(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        let replace_active_new_tab = self.active_pane.read(cx).active_tab_is_new_tab();
        self.focus_or_open_document(path, None, replace_active_new_tab, window, cx)
    }

    fn subscribe_to_pane(&self, pane: &Entity<Pane>, window: &mut Window, cx: &mut Context<Self>) {
        let pane_for_events = pane.clone();
        cx.subscribe_in(
            pane,
            window,
            move |this, _, event: &PaneEvent, window, cx| match event {
                PaneEvent::Activate => {
                    if this.active_pane != pane_for_events {
                        this.active_pane = pane_for_events.clone();
                        cx.notify();
                    }
                }
                PaneEvent::OpenDocument {
                    path,
                    position,
                    replace_active_new_tab,
                } => {
                    this.active_pane = pane_for_events.clone();
                    if let Err(error) = this.focus_or_open_document(
                        path.clone(),
                        *position,
                        *replace_active_new_tab,
                        window,
                        cx,
                    ) {
                        eprintln!("failed to open PDF: {error:#}");
                    }
                }
                PaneEvent::TabDrop {
                    dragged_tab,
                    target,
                } => {
                    this.active_pane = pane_for_events.clone();
                    this.handle_dragged_tab_drop(
                        &pane_for_events,
                        dragged_tab.as_ref(),
                        target.clone(),
                        window,
                        cx,
                    );
                }
                PaneEvent::CloseTab(tab_ix) => {
                    this.active_pane = pane_for_events.clone();
                    this.close_tab_in_pane(&pane_for_events, Some(*tab_ix), window, cx);
                }
            },
        )
        .detach();

        let pane_for_focus = pane.clone();
        let focus_handle = pane.read(cx).focus_handle();
        cx.on_focus_in(&focus_handle, window, move |this, _, cx| {
            if this.active_pane != pane_for_focus {
                this.active_pane = pane_for_focus.clone();
                cx.notify();
            }
        })
        .detach();

        let pane_for_observe = pane.clone();
        cx.observe(pane, move |this, _, cx| {
            if this.active_pane == pane_for_observe {
                cx.notify();
            }
        })
        .detach();
    }

    fn focus_active_pane(&self, window: &mut Window, cx: &mut Context<Self>) {
        let active_pane = self.active_pane.clone();
        active_pane.update(cx, |pane, cx| pane.focus(window, cx));
    }

    fn sync_tab_bar_visibility(&mut self, cx: &mut Context<Self>) {
        let force_show_tab_bar = self.pane_group.panes().len() > 1;
        for pane in self.pane_group.panes() {
            pane.update(cx, |pane, cx| {
                pane.set_force_show_tab_bar(force_show_tab_bar, cx);
            });
        }
    }

    fn create_pane(
        &mut self,
        initial_path: Option<PathBuf>,
        initial_position: Option<DocumentPosition>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<Pane> {
        let pane = cx.new(|cx| Pane::new(initial_path, initial_position, window, cx));
        self.subscribe_to_pane(&pane, window, cx);
        pane
    }

    fn create_pane_from_payload(
        &mut self,
        payload: &DraggedTabPayload,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<Pane> {
        match payload {
            DraggedTabPayload::NewTab => self.create_pane(None, None, window, cx),
            DraggedTabPayload::Document { path, position } => {
                self.create_pane(Some(path.clone()), *position, window, cx)
            }
        }
    }

    fn open_new_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let active_pane = self.active_pane.clone();
        active_pane.update(cx, |pane, cx| pane.open_new_tab(window, cx));
    }

    pub(crate) fn open_keymap_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let active_pane = self.active_pane.clone();
        active_pane.update(cx, |pane, cx| pane.open_keymap_tab(window, cx));
    }

    fn open_file(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let replace_active_new_tab = self.active_pane.read(cx).active_tab_is_new_tab();
        let dialog = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(crate::fl!("open-pdf").into()),
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

    fn focus_or_open_document(
        &mut self,
        path: PathBuf,
        position: Option<DocumentPosition>,
        replace_active_new_tab: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        let path = normalize_path(&path);
        if let Some((pane, tab_ix)) = self.document_location(&path, cx) {
            self.active_pane = pane.clone();
            pane.update(cx, |pane, cx| {
                pane.activate_existing_document(
                    tab_ix,
                    position,
                    replace_active_new_tab,
                    window,
                    cx,
                );
            });
            cx.notify();
            return Ok(());
        }

        let active_pane = self.active_pane.clone();
        active_pane.update(cx, |pane, cx| {
            pane.focus_or_open_document(path, position, replace_active_new_tab, window, cx)
        })?;
        Ok(())
    }

    fn document_location(&self, path: &Path, cx: &Context<Self>) -> Option<(Entity<Pane>, usize)> {
        let path = normalize_path(path);
        self.pane_group.panes().into_iter().find_map(|pane| {
            let ix = pane.read(cx).document_tab_ix(&path, cx)?;
            Some((pane, ix))
        })
    }

    fn close_active_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let active_pane = self.active_pane.clone();
        self.close_tab_in_pane(&active_pane, None, window, cx);
    }

    fn close_tab_in_pane(
        &mut self,
        pane: &Entity<Pane>,
        tab_ix: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let empty = pane.update(cx, |pane, cx| match tab_ix {
            Some(tab_ix) => pane.close_tab_at(tab_ix, cx),
            None => pane.close_active_tab(cx),
        });
        if empty {
            self.handle_empty_pane(pane, window, cx);
        } else {
            pane.update(cx, |pane, cx| pane.focus(window, cx));
        }
        cx.notify();
    }

    fn handle_empty_pane(
        &mut self,
        pane: &Entity<Pane>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.pane_group.panes().len() <= 1 {
            pane.update(cx, |pane, cx| {
                pane.ensure_new_tab(window, cx);
                pane.focus(window, cx);
            });
            self.active_pane = pane.clone();
            return;
        }

        self.remove_pane(pane, window, cx);
    }

    fn remove_pane(&mut self, pane: &Entity<Pane>, window: &mut Window, cx: &mut Context<Self>) {
        let panes = self.pane_group.panes();
        let remove_ix = panes
            .iter()
            .position(|candidate| candidate == pane)
            .unwrap_or(0);
        if !self.pane_group.remove(pane) {
            return;
        }

        let remaining = self.pane_group.panes();
        if remaining.is_empty() {
            window.remove_window();
            return;
        }

        let active_ix = remove_ix.min(remaining.len() - 1);
        self.active_pane = remaining[active_ix].clone();
        self.sync_tab_bar_visibility(cx);
        self.active_pane
            .update(cx, |pane, cx| pane.focus(window, cx));
        cx.notify();
    }

    fn split_active_pane(
        &mut self,
        direction: SplitDirection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let active_pane = self.active_pane.clone();
        let (initial_path, initial_position) = active_pane
            .read(cx)
            .active_document_state(cx)
            .map(|(path, position)| (Some(path), Some(position)))
            .unwrap_or((None, None));
        let new_pane = self.create_pane(initial_path, initial_position, window, cx);
        self.pane_group.split(&active_pane, &new_pane, direction);
        self.active_pane = new_pane.clone();
        self.sync_tab_bar_visibility(cx);
        new_pane.update(cx, |pane, cx| pane.focus(window, cx));
        cx.notify();
    }

    fn activate_pane(&mut self, pane: Entity<Pane>, window: &mut Window, cx: &mut Context<Self>) {
        self.active_pane = pane.clone();
        pane.update(cx, |pane, cx| pane.focus(window, cx));
        cx.notify();
    }

    fn activate_pane_in_direction(
        &mut self,
        direction: SplitDirection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(pane) = self
            .pane_group
            .find_pane_in_direction(&self.active_pane, direction)
        {
            self.activate_pane(pane, window, cx);
        }
    }

    fn activate_next_pane(&mut self, reverse: bool, window: &mut Window, cx: &mut Context<Self>) {
        let panes = self.pane_group.panes();
        if panes.len() <= 1 {
            return;
        }
        let active_ix = panes
            .iter()
            .position(|pane| pane == &self.active_pane)
            .unwrap_or(0);
        let next_ix = if reverse {
            active_ix.checked_sub(1).unwrap_or(panes.len() - 1)
        } else {
            (active_ix + 1) % panes.len()
        };
        self.activate_pane(panes[next_ix].clone(), window, cx);
    }

    fn swap_pane_in_direction(&mut self, direction: SplitDirection, cx: &mut Context<Self>) {
        if let Some(target) = self
            .pane_group
            .find_pane_in_direction(&self.active_pane, direction)
        {
            self.pane_group.swap(&self.active_pane, &target);
            cx.notify();
        }
    }

    fn move_pane_to_border(&mut self, direction: SplitDirection, cx: &mut Context<Self>) {
        if self.pane_group.move_to_border(&self.active_pane, direction) {
            cx.notify();
        }
    }

    fn resize_active_pane(&mut self, direction: SplitDirection, cx: &mut Context<Self>) {
        self.pane_group.resize(&self.active_pane, direction);
        cx.notify();
    }

    fn close_other_panes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for pane in self.pane_group.panes() {
            if pane != self.active_pane {
                pane.update(cx, |pane, cx| pane.flush_all_tabs(cx));
            }
        }
        self.pane_group.retain_only(self.active_pane.clone());
        self.sync_tab_bar_visibility(cx);
        self.active_pane
            .update(cx, |pane, cx| pane.focus(window, cx));
        cx.notify();
    }

    fn reset_pane_sizes(&mut self, cx: &mut Context<Self>) {
        self.pane_group.reset_pane_sizes();
        cx.notify();
    }

    fn toggle_theme_mode(&mut self, _: &ToggleMode, _window: &mut Window, cx: &mut Context<Self>) {
        let store = cx.global::<SettingsStore>();
        let current_mode = store.settings.theme.mode();
        let next_mode = match current_mode {
            Some(ThemeAppearanceMode::Light) => ThemeAppearanceMode::Dark,
            Some(ThemeAppearanceMode::Dark) => ThemeAppearanceMode::Light,
            Some(ThemeAppearanceMode::System) | None => {
                let active_name = store.settings.theme.name(store.system_appearance);
                match store.registry.appearance(active_name) {
                    ThemeAppearance::Light => ThemeAppearanceMode::Dark,
                    ThemeAppearance::Dark => ThemeAppearanceMode::Light,
                }
            }
        };

        let mut settings = store.settings.clone();
        settings.theme.set_mode(next_mode);
        let _ = cx.global_mut::<SettingsStore>().set_settings(settings);
        let _ = cx.global_mut::<SettingsStore>().save();
        cx.refresh_windows();
    }

    fn render_body(
        &self,
        theme: Theme,
        dim_inactive_panes: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let active_pdf_view = self.active_pane.read(cx).active_pdf_view();
        if let Some(active_pdf_view) = active_pdf_view.clone()
            && active_pdf_view.read(cx).is_presentation_mode()
        {
            return active_pdf_view.into_any_element();
        }

        let center =
            self.pane_group
                .render(&self.active_pane, theme, dim_inactive_panes, window, cx);

        if let Some(active_pdf_view) = active_pdf_view {
            let has_global_chrome = active_pdf_view.read(cx).has_global_chrome();
            if has_global_chrome {
                return active_pdf_view.update(cx, |pdf_view, cx| {
                    pdf_view.render_workspace_body_with_chrome(center, theme, window, cx)
                });
            }
        }

        center
    }

    fn handle_dragged_tab_drop(
        &mut self,
        target_pane: &Entity<Pane>,
        dragged_tab: &DraggedTab,
        target: PaneDropTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.clear_dragged_tab_outside_window();

        if dragged_tab.source_pane == *target_pane
            && matches!(target, PaneDropTarget::Tab(_) | PaneDropTarget::Append)
        {
            let to = self.drop_index(target_pane, &target, cx);
            target_pane.update(cx, |pane, cx| pane.move_dragged_tab(dragged_tab, to, cx));
            return;
        }
        if dragged_tab.source_pane == *target_pane
            && matches!(target, PaneDropTarget::Content(None))
        {
            target_pane.update(cx, |pane, cx| pane.focus(window, cx));
            return;
        }

        if !dragged_tab
            .source_pane
            .read(cx)
            .has_tab_id(dragged_tab.tab_id)
        {
            return;
        }

        match target {
            PaneDropTarget::Content(Some(direction)) => {
                let new_pane = self.create_pane_from_payload(&dragged_tab.payload, window, cx);
                self.pane_group.split(target_pane, &new_pane, direction);
                self.active_pane = new_pane.clone();
                self.remove_dragged_source_tab(dragged_tab, window, cx);
                self.sync_tab_bar_visibility(cx);
                new_pane.update(cx, |pane, cx| pane.focus(window, cx));
            }
            target => {
                let to = self.drop_index(target_pane, &target, cx);
                let import_result = target_pane.update(cx, |pane, cx| {
                    pane.import_dragged_tab_payload(&dragged_tab.payload, to, window, cx)
                });
                if let Err(error) = import_result {
                    eprintln!("failed to move tab: {error:#}");
                    return;
                }
                self.active_pane = target_pane.clone();
                self.remove_dragged_source_tab(dragged_tab, window, cx);
            }
        }

        cx.notify();
    }

    fn drop_index(
        &self,
        target_pane: &Entity<Pane>,
        target: &PaneDropTarget,
        cx: &Context<Self>,
    ) -> usize {
        match target {
            PaneDropTarget::Tab(ix) => *ix,
            PaneDropTarget::Append
            | PaneDropTarget::Content(None)
            | PaneDropTarget::Content(Some(_)) => target_pane.read(cx).len(),
        }
    }

    fn remove_dragged_source_tab(
        &mut self,
        dragged_tab: &DraggedTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if dragged_tab.source_window.window_id() == window.window_handle().window_id() {
            self.remove_tab_from_pane(&dragged_tab.source_pane, dragged_tab.tab_id, window, cx);
            return;
        }

        if let Err(error) = dragged_tab
            .source_window
            .update(cx, |source, source_window, cx| {
                source.remove_tab_from_pane(
                    &dragged_tab.source_pane,
                    dragged_tab.tab_id,
                    source_window,
                    cx,
                );
            })
        {
            eprintln!("failed to remove moved tab from source window: {error:#}");
        }
    }

    fn remove_tab_from_pane(
        &mut self,
        pane: &Entity<Pane>,
        tab_id: gpui::EntityId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let empty = pane.update(cx, |pane, cx| pane.remove_tab_for_move(tab_id, cx));
        if empty {
            if self.pane_group.panes().len() <= 1 {
                window.remove_window();
            } else {
                self.remove_pane(pane, window, cx);
            }
        }
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

        if !dragged_tab
            .source_pane
            .read(cx)
            .has_tab_id(dragged_tab.tab_id)
        {
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
        if !dragged_tab
            .source_pane
            .read(cx)
            .has_tab_id(dragged_tab.tab_id)
        {
            return;
        }

        let path = match &dragged_tab.payload {
            DraggedTabPayload::Document { path, .. } => Some(path.clone()),
            DraggedTabPayload::NewTab => None,
        };
        let bounds = Self::detached_window_bounds(event, window);
        if let Err(error) = open_workspace_window_at(path, Some(bounds), cx) {
            eprintln!("failed to tear tab into new window: {error:#}");
            return;
        }

        self.remove_tab_from_pane(&dragged_tab.source_pane, dragged_tab.tab_id, window, cx);
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

    fn toggle_command_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.command_palette.is_some() {
            self.close_command_palette(window, cx);
            return;
        }

        let previous_focus_handle = window
            .focused(cx)
            .unwrap_or_else(|| self.active_pane.read(cx).focus_handle());
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
            if let Some(previous_focus_handle) = self.command_palette_previous_focus_handle.take() {
                previous_focus_handle.focus(window, cx);
            } else {
                self.focus_active_pane(window, cx);
            }
            cx.notify();
        }
    }

    fn toggle_theme_selector(
        &mut self,
        _: &ToggleThemeSelector,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.theme_selector.is_some() {
            self.close_theme_selector(window, cx);
            return;
        }

        let previous_focus_handle = window
            .focused(cx)
            .unwrap_or_else(|| self.active_pane.read(cx).focus_handle());

        let (themes, selection, system_appearance, selected_ix) = {
            let store = cx.global::<SettingsStore>();
            let themes = store.registry.metadata();
            let selection = store.settings.theme.clone();
            let system_appearance = store.system_appearance;
            let active_name = selection.name(system_appearance);
            let selected_ix = themes
                .iter()
                .position(|meta| meta.id == active_name)
                .unwrap_or(0);
            (themes, selection, system_appearance, selected_ix)
        };
        let delegate = ThemeSelectorDelegate::new(themes, selection, system_appearance);

        let picker_previous_focus_handle = previous_focus_handle.clone();
        let selector = cx.new(|cx| {
            Picker::new_with_selected_ix(picker_previous_focus_handle, delegate, selected_ix, cx)
        });

        cx.subscribe_in(&selector, window, |this, _, _: &PickerEvent, window, cx| {
            this.close_theme_selector(window, cx);
        })
        .detach();

        let focus_handle = selector.focus_handle(cx);
        window.defer(cx, move |window, cx| {
            focus_handle.focus(window, cx);
        });
        self.theme_selector_previous_focus_handle = Some(previous_focus_handle);
        self.theme_selector = Some(selector);
        cx.notify();
    }

    fn close_theme_selector(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.theme_selector.take().is_some() {
            if let Some(previous_focus_handle) = self.theme_selector_previous_focus_handle.take() {
                previous_focus_handle.focus(window, cx);
            } else {
                self.focus_active_pane(window, cx);
            }
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

    fn active_tab_is_new_tab(&self, cx: &Context<Self>) -> bool {
        self.active_pane.read(cx).active_tab_is_new_tab()
    }

    fn active_document_label(&self, cx: &Context<Self>) -> Option<SharedString> {
        self.active_pane.read(cx).active_document_label(cx)
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
            .rounded(px(RADIUS_SM))
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
        label: String,
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
            .rounded(px(RADIUS_SM))
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

    fn render_toolbar(
        &self,
        theme: Theme,
        is_fullscreen: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let expanded = self.toolbar_menu_open || self.active_tab_is_new_tab(cx);
        let mut toolbar = div()
            .flex_none()
            .h(ui_rems(TOOLBAR_HEIGHT))
            .flex()
            .items_center()
            .gap_1()
            .px_1()
            .when(cfg!(target_os = "macos") && !is_fullscreen, |this| {
                this.pl(px(TRAFFIC_LIGHT_PADDING))
            })
            .border_b(px(1.0))
            .border_color(rgb(theme.border))
            .bg(rgb(theme.tab_bar_background))
            .text_size(ui_rems(TOOLBAR_MENU_TEXT_SIZE))
            .text_color(rgb(theme.text))
            .window_control_area(WindowControlArea::Drag)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.should_move_window = true;
                    this.close_toolbar_menu(cx);
                    cx.stop_propagation();
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, _| {
                    this.should_move_window = false;
                }),
            )
            .on_mouse_move(cx.listener(|this, _: &MouseMoveEvent, window, _| {
                if this.should_move_window {
                    this.should_move_window = false;
                    window.start_window_move();
                }
            }))
            .id("toolbar")
            .on_click(|event, window, _| {
                if event.click_count() == 2 {
                    #[cfg(target_os = "macos")]
                    window.titlebar_double_click();
                    #[cfg(not(target_os = "macos"))]
                    window.zoom_window();
                }
            });

        if expanded {
            toolbar = toolbar
                .child(self.render_toolbar_menu_button(
                    crate::fl!("toolbar-menu-glyphst"),
                    IconName::AppWindow,
                    ToolbarMenu::Glyphst,
                    TOOLBAR_GLYPHST_WIDTH,
                    theme,
                    cx,
                ))
                .child(self.render_toolbar_menu_button(
                    crate::fl!("toolbar-menu-file"),
                    IconName::File,
                    ToolbarMenu::File,
                    TOOLBAR_FILE_WIDTH,
                    theme,
                    cx,
                ))
                .child(self.render_toolbar_menu_button(
                    crate::fl!("toolbar-menu-tab"),
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
        label: String,
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
        let base = TOOLBAR_PADDING_X;
        #[cfg(target_os = "macos")]
        let base = base + TRAFFIC_LIGHT_PADDING;
        match menu {
            ToolbarMenu::Glyphst => ui_rems(base),
            ToolbarMenu::File => ui_rems(base + TOOLBAR_GLYPHST_WIDTH + TOOLBAR_GAP),
            ToolbarMenu::Tab => ui_rems(
                base + TOOLBAR_GLYPHST_WIDTH + TOOLBAR_GAP + TOOLBAR_FILE_WIDTH + TOOLBAR_GAP,
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
                        crate::fl!("toolbar-about-glyphst"),
                        theme,
                        |_, _, _| {},
                        cx,
                    ))
                    .child(Self::render_toolbar_menu_item(
                        crate::fl!("toolbar-check-updates"),
                        theme,
                        |_, _, _| {},
                        cx,
                    ))
                    .child(Self::render_toolbar_menu_item(
                        crate::fl!("toolbar-open-settings"),
                        theme,
                        |_, _, cx| open_settings_window(cx),
                        cx,
                    ))
                    .child(Self::render_toolbar_menu_item(
                        crate::fl!("toolbar-quit-glyphst"),
                        theme,
                        |_, _, cx| cx.quit(),
                        cx,
                    ));
            }
            ToolbarMenu::File => {
                dropdown = dropdown.child(Self::render_toolbar_menu_item(
                    crate::fl!("toolbar-open-file"),
                    theme,
                    |this, window, cx| this.open_file(window, cx),
                    cx,
                ));
            }
            ToolbarMenu::Tab => {
                dropdown = dropdown
                    .child(Self::render_toolbar_menu_item(
                        crate::fl!("toolbar-new-tab"),
                        theme,
                        |this, window, cx| this.open_new_tab(window, cx),
                        cx,
                    ))
                    .child(Self::render_toolbar_menu_item(
                        crate::fl!("toolbar-close-tab"),
                        theme,
                        |this, window, cx| this.close_active_tab(window, cx),
                        cx,
                    ))
                    .child(Self::render_toolbar_menu_item(
                        crate::fl!("toolbar-split-right"),
                        theme,
                        |this, window, cx| {
                            this.split_active_pane(SplitDirection::Right, window, cx)
                        },
                        cx,
                    ))
                    .child(Self::render_toolbar_menu_item(
                        crate::fl!("toolbar-split-down"),
                        theme,
                        |this, window, cx| this.split_active_pane(SplitDirection::Down, window, cx),
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
        let (theme, dim_inactive_panes) = {
            let store = cx.global::<SettingsStore>();
            (
                store.resolved_theme,
                store.settings.behavior.dim_inactive_panes,
            )
        };
        let active_pdf_is_presentation = self
            .active_pane
            .read(cx)
            .active_pdf_view()
            .is_some_and(|pdf_view| pdf_view.read(cx).is_presentation_mode());
        let is_fullscreen = window.is_fullscreen();

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
            .on_action(cx.listener(|this, _: &OpenFile, window, cx| {
                this.open_file(window, cx);
            }))
            .on_action(cx.listener(|this, _: &OpenNewTab, window, cx| {
                this.open_new_tab(window, cx);
            }))
            .on_action(cx.listener(|this, _: &CloseTab, window, cx| {
                this.close_active_tab(window, cx);
            }))
            .on_action(cx.listener(|this, _: &SplitLeft, window, cx| {
                this.split_active_pane(SplitDirection::Left, window, cx);
            }))
            .on_action(cx.listener(|this, _: &SplitRight, window, cx| {
                this.split_active_pane(SplitDirection::Right, window, cx);
            }))
            .on_action(cx.listener(|this, _: &SplitUp, window, cx| {
                this.split_active_pane(SplitDirection::Up, window, cx);
            }))
            .on_action(cx.listener(|this, _: &SplitDown, window, cx| {
                this.split_active_pane(SplitDirection::Down, window, cx);
            }))
            .on_action(cx.listener(|this, _: &SplitVertical, window, cx| {
                this.split_active_pane(SplitDirection::Right, window, cx);
            }))
            .on_action(cx.listener(|this, _: &SplitHorizontal, window, cx| {
                this.split_active_pane(SplitDirection::Down, window, cx);
            }))
            .on_action(cx.listener(|this, _: &ActivatePaneLeft, window, cx| {
                this.activate_pane_in_direction(SplitDirection::Left, window, cx);
            }))
            .on_action(cx.listener(|this, _: &ActivatePaneRight, window, cx| {
                this.activate_pane_in_direction(SplitDirection::Right, window, cx);
            }))
            .on_action(cx.listener(|this, _: &ActivatePaneUp, window, cx| {
                this.activate_pane_in_direction(SplitDirection::Up, window, cx);
            }))
            .on_action(cx.listener(|this, _: &ActivatePaneDown, window, cx| {
                this.activate_pane_in_direction(SplitDirection::Down, window, cx);
            }))
            .on_action(cx.listener(|this, _: &ActivateNextPane, window, cx| {
                this.activate_next_pane(false, window, cx);
            }))
            .on_action(cx.listener(|this, _: &ActivatePreviousPane, window, cx| {
                this.activate_next_pane(true, window, cx);
            }))
            .on_action(cx.listener(|this, _: &SwapPaneLeft, _, cx| {
                this.swap_pane_in_direction(SplitDirection::Left, cx);
            }))
            .on_action(cx.listener(|this, _: &SwapPaneRight, _, cx| {
                this.swap_pane_in_direction(SplitDirection::Right, cx);
            }))
            .on_action(cx.listener(|this, _: &SwapPaneUp, _, cx| {
                this.swap_pane_in_direction(SplitDirection::Up, cx);
            }))
            .on_action(cx.listener(|this, _: &SwapPaneDown, _, cx| {
                this.swap_pane_in_direction(SplitDirection::Down, cx);
            }))
            .on_action(cx.listener(|this, _: &MovePaneLeft, _, cx| {
                this.move_pane_to_border(SplitDirection::Left, cx);
            }))
            .on_action(cx.listener(|this, _: &MovePaneRight, _, cx| {
                this.move_pane_to_border(SplitDirection::Right, cx);
            }))
            .on_action(cx.listener(|this, _: &MovePaneUp, _, cx| {
                this.move_pane_to_border(SplitDirection::Up, cx);
            }))
            .on_action(cx.listener(|this, _: &MovePaneDown, _, cx| {
                this.move_pane_to_border(SplitDirection::Down, cx);
            }))
            .on_action(cx.listener(|this, _: &ResizePaneLeft, _, cx| {
                this.resize_active_pane(SplitDirection::Left, cx);
            }))
            .on_action(cx.listener(|this, _: &ResizePaneRight, _, cx| {
                this.resize_active_pane(SplitDirection::Right, cx);
            }))
            .on_action(cx.listener(|this, _: &ResizePaneUp, _, cx| {
                this.resize_active_pane(SplitDirection::Up, cx);
            }))
            .on_action(cx.listener(|this, _: &ResizePaneDown, _, cx| {
                this.resize_active_pane(SplitDirection::Down, cx);
            }))
            .on_action(cx.listener(|this, _: &CloseOtherPanes, window, cx| {
                this.close_other_panes(window, cx);
            }))
            .on_action(cx.listener(|this, _: &ResetPaneSizes, _, cx| {
                this.reset_pane_sizes(cx);
            }))
            .on_action(cx.listener(Self::toggle_theme_mode))
            .on_action(cx.listener(Self::toggle_theme_selector))
            .on_action(cx.listener(|this, _: &ToggleCommandPalette, window, cx| {
                this.toggle_command_palette(window, cx);
            }))
            .on_action(|_: &OpenSettings, _, cx| open_settings_window(cx))
            .on_action(cx.listener(|this, _: &OpenKeybindings, window, cx| {
                this.open_keymap_tab(window, cx);
            }))
            .when(!active_pdf_is_presentation, |this| {
                this.child(self.render_toolbar(theme, is_fullscreen, cx))
            })
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .overflow_hidden()
                    .child(self.render_body(theme, dim_inactive_panes, window, cx)),
            )
            .when(!active_pdf_is_presentation, |this| {
                this.when_some(self.toolbar_open_menu, |this, menu| {
                    this.child(self.render_toolbar_dropdown(menu, theme, cx))
                })
            })
            .when_some(self.command_palette.clone(), |this, palette| {
                this.child(Self::render_top_aligned_overlay(
                    palette,
                    |this, window, cx| this.close_command_palette(window, cx),
                    theme,
                    cx,
                ))
            })
            .when_some(self.theme_selector.clone(), |this, selector| {
                this.child(Self::render_top_aligned_overlay(
                    selector,
                    |this, window, cx| this.close_theme_selector(window, cx),
                    theme,
                    cx,
                ))
            })
    }
}
