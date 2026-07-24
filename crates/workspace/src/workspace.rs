use std::path::{Path, PathBuf};

use anyhow::Result;
use glyphst_translation_ui::{
    DocumentTranslationJobEvent, DocumentTranslationJobId, DocumentTranslationJobManager,
};
use glyphst_ui::notification_center::NotificationCenter;
use glyphst_ui::root::render_overlays;
use gpui::{
    AnyElement, App, Context, DismissEvent, DragMoveEvent, Entity, FocusHandle, Focusable,
    IntoElement, MouseButton, MouseUpEvent, PathPromptOptions, Pixels, Render, SharedString,
    Window, WindowAppearance, div, point, prelude::*, px, rgba,
};
use gpui_component::{menu::PopupMenu, status_bar::StatusBar};

mod chrome;

pub use glyphst_actions::workspace::{
    ActivateNextPane, ActivatePaneDown, ActivatePaneLeft, ActivatePaneRight, ActivatePaneUp,
    ActivatePreviousPane, CloseOtherPanes, MovePaneDown, MovePaneLeft, MovePaneRight, MovePaneUp,
    ResetPaneSizes, ResizePaneDown, ResizePaneLeft, ResizePaneRight, ResizePaneUp, SwapPaneDown,
    SwapPaneLeft, SwapPaneRight, SwapPaneUp, ToggleMode, ToggleThemeSelector,
};

use crate::{
    command_palette::{CommandPalette, CommandPaletteDelegate},
    library::{DocumentPosition, normalize_path},
    pane::{
        DraggedTab, Pane, PaneDropTarget, PaneEvent, PaneGroup, SplitDirection, SplitDown,
        SplitHorizontal, SplitLeft, SplitRight, SplitUp, SplitVertical, TabTransfer,
    },
    pdf_view::{
        CloseTab, OpenFile, OpenKeybindings, OpenNewTab, OpenSettings, PdfView, PdfViewEvent,
        ToggleCommandPalette,
    },
    picker::{Picker, PickerEvent},
    settings::{SettingsStore, setup_ui_font, sync_component_theme},
    theme::{Theme, ThemeAppearance, ThemeAppearanceMode},
    theme_selector::{ThemeSelector, ThemeSelectorDelegate},
};

use chrome::{WorkspaceMenu, WorkspaceTitleBar, build_popup_menu};

pub struct Workspace {
    pane_group: PaneGroup,
    active_pane: Entity<Pane>,
    callbacks: WorkspaceCallbacks,
    focus_handle: FocusHandle,
    document_translation_jobs: Entity<DocumentTranslationJobManager>,
    pending_translation_splits: Vec<PendingTranslationSplit>,
    command_palette: Option<Entity<CommandPalette>>,
    command_palette_previous_focus_handle: Option<FocusHandle>,
    theme_selector: Option<Entity<ThemeSelector>>,
    theme_selector_previous_focus_handle: Option<FocusHandle>,
    title_bar_menu_expanded: bool,
    title_bar_open_menu: Option<WorkspaceMenu>,
    title_bar_popup_menu: Option<Entity<PopupMenu>>,
    dragged_tab_outside_window: Option<DraggedTab>,
}

pub type WorkspaceWindowHandle = glyphst_ui::root::RootWindowHandle<Workspace>;

struct PendingTranslationSplit {
    job_id: DocumentTranslationJobId,
    stage: PendingTranslationSplitStage,
}

enum PendingTranslationSplitStage {
    OpeningSource {
        source_path: PathBuf,
        translated_path: PathBuf,
        source_pane: Entity<Pane>,
    },
    OpeningTranslated {
        original: Entity<PdfView>,
        translated_pane: Entity<Pane>,
    },
}

#[derive(Clone, Copy)]
pub struct WorkspaceWindowRequest {
    pub bounds: gpui::Bounds<Pixels>,
}

#[derive(Clone, Copy)]
pub struct WorkspaceCallbacks {
    pub open_workspace_window:
        fn(WorkspaceWindowRequest, &mut App) -> Result<WorkspaceWindowHandle>,
    pub open_settings_window: fn(&mut App),
}

#[derive(Clone)]
struct TabDestination {
    pane: Entity<Pane>,
    target: PaneDropTarget,
    index: usize,
}

impl Workspace {
    pub fn new(
        initial_path: Option<PathBuf>,
        initial_position: Option<DocumentPosition>,
        callbacks: WorkspaceCallbacks,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let document_translation_jobs = cx.new(DocumentTranslationJobManager::new);
        let jobs = document_translation_jobs.clone();
        let pane = cx.new(|cx| Pane::empty(cx.focus_handle(), jobs));
        let mut workspace = Self::with_pane(
            pane.clone(),
            document_translation_jobs,
            callbacks,
            window,
            cx,
        );
        match initial_path {
            Some(path) => {
                if let Err(error) =
                    workspace.focus_or_open_document(path, initial_position, true, window, cx)
                {
                    eprintln!("failed to open initial document: {error:#}");
                    pane.update(cx, |pane, cx| pane.ensure_new_tab(window, cx));
                }
            }
            None => pane.update(cx, |pane, cx| pane.ensure_new_tab(window, cx)),
        }
        workspace
    }

    pub fn new_empty(
        callbacks: WorkspaceCallbacks,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let document_translation_jobs = cx.new(DocumentTranslationJobManager::new);
        let jobs = document_translation_jobs.clone();
        let pane = cx.new(|cx| Pane::empty(cx.focus_handle(), jobs));
        Self::with_pane(pane, document_translation_jobs, callbacks, window, cx)
    }

    fn with_pane(
        pane: Entity<Pane>,
        document_translation_jobs: Entity<DocumentTranslationJobManager>,
        callbacks: WorkspaceCallbacks,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        let workspace = Self {
            pane_group: PaneGroup::new(pane.clone()),
            active_pane: pane.clone(),
            callbacks,
            focus_handle,
            document_translation_jobs: document_translation_jobs.clone(),
            pending_translation_splits: Vec::new(),
            command_palette: None,
            command_palette_previous_focus_handle: None,
            theme_selector: None,
            theme_selector_previous_focus_handle: None,
            title_bar_menu_expanded: false,
            title_bar_open_menu: None,
            title_bar_popup_menu: None,
            dragged_tab_outside_window: None,
        };

        workspace.subscribe_to_pane(&pane, window, cx);
        cx.observe(&document_translation_jobs, |_, _, cx| cx.notify())
            .detach();
        if let Some(notification_center) = NotificationCenter::try_global(cx) {
            cx.observe(&notification_center, |_, _, cx| cx.notify())
                .detach();
        }
        cx.subscribe_in(
            &document_translation_jobs,
            window,
            |this, _, event: &DocumentTranslationJobEvent, window, cx| match event {
                DocumentTranslationJobEvent::OpenDocument { id, path } => {
                    let replace_active_new_tab = this.active_pane.read(cx).active_tab_is_new_tab();
                    if let Err(error) = this.focus_or_open_document_for_job(
                        path.clone(),
                        None,
                        replace_active_new_tab,
                        *id,
                        window,
                        cx,
                    ) {
                        this.document_translation_jobs.update(cx, |jobs, cx| {
                            jobs.finish_open(*id, Err(format!("{error:#}").into()), cx);
                        });
                    }
                }
                DocumentTranslationJobEvent::OpenSplit {
                    id,
                    source_path,
                    translated_path,
                } => {
                    if let Err(error) = this.begin_translation_split_open(
                        *id,
                        source_path.clone(),
                        translated_path.clone(),
                        window,
                        cx,
                    ) {
                        this.document_translation_jobs.update(cx, |jobs, cx| {
                            jobs.finish_open(*id, Err(format!("{error:#}").into()), cx);
                        });
                    }
                }
            },
        )
        .detach();
        workspace.focus_active_pane(window, cx);
        cx.observe_global::<SettingsStore>(|_, cx| cx.notify())
            .detach();

        let system_appearance = match window.appearance() {
            WindowAppearance::Light | WindowAppearance::VibrantLight => ThemeAppearance::Light,
            WindowAppearance::Dark | WindowAppearance::VibrantDark => ThemeAppearance::Dark,
        };
        cx.global_mut::<SettingsStore>()
            .set_system_appearance(system_appearance);
        sync_component_theme(cx);
        cx.observe_window_appearance(window, |_, window, cx| {
            let appearance = match window.appearance() {
                WindowAppearance::Light | WindowAppearance::VibrantLight => ThemeAppearance::Light,
                WindowAppearance::Dark | WindowAppearance::VibrantDark => ThemeAppearance::Dark,
            };
            cx.global_mut::<SettingsStore>()
                .set_system_appearance(appearance);
            sync_component_theme(cx);
        })
        .detach();
        cx.on_release(|this, cx| {
            for pane in this.pane_group.panes() {
                pane.update(cx, |pane, cx| pane.stage_all_tab_positions(cx));
            }
        })
        .detach();

        workspace
    }

    pub fn open_document_external(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        let replace_active_new_tab = self.active_pane.read(cx).active_tab_is_new_tab();
        self.focus_or_open_document(path, None, replace_active_new_tab, window, cx)
    }

    fn open_settings_window(&mut self, cx: &mut Context<Self>) {
        self.set_title_bar_menu_expanded(false, cx);
        (self.callbacks.open_settings_window)(cx);
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
                        position.clone(),
                        *replace_active_new_tab,
                        window,
                        cx,
                    ) {
                        eprintln!("failed to open document: {error:#}");
                    }
                }
                PaneEvent::DocumentOpenFinished { job_id, result } => {
                    if !this.continue_translation_split_open(*job_id, result.clone(), window, cx) {
                        this.document_translation_jobs.update(cx, |jobs, cx| {
                            jobs.finish_open(*job_id, result.clone(), cx);
                        });
                    }
                }
                PaneEvent::OpenSettings => this.open_settings_window(cx),
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

    fn create_empty_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<Pane> {
        let document_translation_jobs = self.document_translation_jobs.clone();
        let pane = cx.new(|cx| Pane::empty(cx.focus_handle(), document_translation_jobs));
        self.subscribe_to_pane(&pane, window, cx);
        pane
    }

    fn open_new_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.set_title_bar_menu_expanded(false, cx);
        let active_pane = self.active_pane.clone();
        active_pane.update(cx, |pane, cx| pane.open_new_tab(window, cx));
    }

    pub fn open_keymap_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let active_pane = self.active_pane.clone();
        active_pane.update(cx, |pane, cx| pane.open_keymap_tab(window, cx));
    }

    fn open_file(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.set_title_bar_menu_expanded(false, cx);
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

    fn focus_or_open_document_for_job(
        &mut self,
        path: PathBuf,
        position: Option<DocumentPosition>,
        replace_active_new_tab: bool,
        job_id: DocumentTranslationJobId,
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
                pane.associate_open_job(tab_ix, job_id, cx);
            });
            cx.notify();
            return Ok(());
        }

        let active_pane = self.active_pane.clone();
        active_pane.update(cx, |pane, cx| {
            pane.focus_or_open_document_for_job(
                path,
                position,
                replace_active_new_tab,
                job_id,
                window,
                cx,
            )
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
        self.set_title_bar_menu_expanded(false, cx);
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
        self.cancel_pending_translation_splits_for_pane(pane, cx);
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

    fn cancel_pending_translation_splits_for_pane(
        &mut self,
        pane: &Entity<Pane>,
        cx: &mut Context<Self>,
    ) {
        let mut cancelled = Vec::new();
        self.pending_translation_splits.retain(|pending| {
            let matches = match &pending.stage {
                PendingTranslationSplitStage::OpeningSource { source_pane, .. } => {
                    source_pane == pane
                }
                PendingTranslationSplitStage::OpeningTranslated {
                    translated_pane, ..
                } => translated_pane == pane,
            };
            if matches {
                cancelled.push(pending.job_id);
            }
            !matches
        });
        for job_id in cancelled {
            self.document_translation_jobs.update(cx, |jobs, cx| {
                jobs.finish_open(
                    job_id,
                    Err("Opening the translated document was cancelled.".into()),
                    cx,
                );
            });
        }
    }

    fn split_active_pane(
        &mut self,
        direction: SplitDirection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_title_bar_menu_expanded(false, cx);
        let active_pane = self.active_pane.clone();
        let (initial_path, initial_position) = active_pane
            .read(cx)
            .active_document_state(cx)
            .map(|(path, position)| (Some(path), Some(position)))
            .unwrap_or((None, None));
        let new_pane = self.create_empty_pane(window, cx);
        self.pane_group.split(&active_pane, &new_pane, direction);
        self.active_pane = new_pane.clone();
        self.sync_tab_bar_visibility(cx);
        if let Some(path) = initial_path
            && let Err(error) = new_pane.update(cx, |pane, cx| {
                pane.focus_or_open_document(path, initial_position, true, window, cx)
            })
        {
            eprintln!("failed to open document in split pane: {error:#}");
            new_pane.update(cx, |pane, cx| pane.ensure_new_tab(window, cx));
        }
        new_pane.update(cx, |pane, cx| pane.focus(window, cx));
        cx.notify();
    }

    fn begin_translation_split_open(
        &mut self,
        job_id: DocumentTranslationJobId,
        source_path: PathBuf,
        translated_path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        let source_path = normalize_path(&source_path);
        if let Some((source_pane, tab_ix)) = self.document_location(&source_path, cx) {
            self.active_pane = source_pane.clone();
            source_pane.update(cx, |pane, cx| {
                pane.activate_existing_document(tab_ix, None, false, window, cx);
            });
            let Some(original) = source_pane.read(cx).active_pdf_view() else {
                return Err(anyhow::anyhow!(
                    "translation source is still opening; retry when it is ready"
                ));
            };
            return self.start_translated_split_open(
                job_id,
                original,
                translated_path,
                source_pane,
                window,
                cx,
            );
        }

        let source_pane = self.active_pane.clone();
        self.pending_translation_splits
            .push(PendingTranslationSplit {
                job_id,
                stage: PendingTranslationSplitStage::OpeningSource {
                    source_path: source_path.clone(),
                    translated_path,
                    source_pane: source_pane.clone(),
                },
            });
        let replace_active_new_tab = source_pane.read(cx).active_tab_is_new_tab();
        if let Err(error) = source_pane.update(cx, |pane, cx| {
            pane.focus_or_open_document_for_job(
                source_path,
                None,
                replace_active_new_tab,
                job_id,
                window,
                cx,
            )
        }) {
            self.pending_translation_splits
                .retain(|pending| pending.job_id != job_id);
            return Err(error);
        }
        Ok(())
    }

    fn start_translated_split_open(
        &mut self,
        job_id: DocumentTranslationJobId,
        original: Entity<PdfView>,
        translated_path: PathBuf,
        source_pane: Entity<Pane>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        let translated_pane = self.create_empty_pane(window, cx);
        self.pane_group
            .split(&source_pane, &translated_pane, SplitDirection::Right);
        self.active_pane = translated_pane.clone();
        self.sync_tab_bar_visibility(cx);

        let position = original.read(cx).current_document_position();
        self.pending_translation_splits
            .push(PendingTranslationSplit {
                job_id,
                stage: PendingTranslationSplitStage::OpeningTranslated {
                    original,
                    translated_pane: translated_pane.clone(),
                },
            });
        if let Err(error) = translated_pane.update(cx, |pane, cx| {
            pane.focus_or_open_document_for_job(
                translated_path,
                Some(position),
                true,
                job_id,
                window,
                cx,
            )
        }) {
            self.pending_translation_splits
                .retain(|pending| pending.job_id != job_id);
            self.remove_pane(&translated_pane, window, cx);
            return Err(error);
        }
        cx.notify();
        Ok(())
    }

    fn continue_translation_split_open(
        &mut self,
        job_id: DocumentTranslationJobId,
        result: std::result::Result<(), SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(index) = self
            .pending_translation_splits
            .iter()
            .position(|pending| pending.job_id == job_id)
        else {
            return false;
        };
        let pending = self.pending_translation_splits.remove(index);
        if let Err(error) = result {
            if let PendingTranslationSplitStage::OpeningTranslated {
                translated_pane, ..
            } = pending.stage
            {
                self.remove_pane(&translated_pane, window, cx);
            }
            self.document_translation_jobs.update(cx, |jobs, cx| {
                jobs.finish_open(job_id, Err(error), cx);
            });
            return true;
        }

        match pending.stage {
            PendingTranslationSplitStage::OpeningSource {
                source_path,
                translated_path,
                source_pane: _,
            } => {
                let result = self
                    .document_location(&source_path, cx)
                    .and_then(|(source_pane, tab_ix)| {
                        source_pane.update(cx, |pane, cx| {
                            pane.activate_existing_document(tab_ix, None, false, window, cx);
                        });
                        source_pane
                            .read(cx)
                            .active_pdf_view()
                            .map(|original| (original, source_pane))
                    })
                    .ok_or_else(|| anyhow::anyhow!("translation source did not finish opening"))
                    .and_then(|(original, source_pane)| {
                        self.start_translated_split_open(
                            job_id,
                            original,
                            translated_path,
                            source_pane,
                            window,
                            cx,
                        )
                    });
                if let Err(error) = result {
                    self.document_translation_jobs.update(cx, |jobs, cx| {
                        jobs.finish_open(job_id, Err(format!("{error:#}").into()), cx);
                    });
                }
            }
            PendingTranslationSplitStage::OpeningTranslated {
                original,
                translated_pane,
            } => {
                let Some(translated) = translated_pane.read(cx).active_pdf_view() else {
                    self.remove_pane(&translated_pane, window, cx);
                    self.document_translation_jobs.update(cx, |jobs, cx| {
                        jobs.finish_open(
                            job_id,
                            Err("translated document did not finish opening".into()),
                            cx,
                        );
                    });
                    return true;
                };
                self.link_translation_views(original, translated, window, cx);
                translated_pane.update(cx, |pane, cx| pane.focus(window, cx));
                self.document_translation_jobs.update(cx, |jobs, cx| {
                    jobs.finish_open(job_id, Ok(()), cx);
                });
                cx.notify();
            }
        };
        true
    }

    fn link_translation_views(
        &self,
        original: Entity<PdfView>,
        translated: Entity<PdfView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let translated_target = translated.downgrade();
        cx.subscribe_in(
            &original,
            window,
            move |_, _, event: &PdfViewEvent, _, cx| match event {
                PdfViewEvent::ViewportChanged(viewport) => {
                    if let Some(translated_target) = translated_target.upgrade() {
                        translated_target.update(cx, |view, cx| {
                            view.apply_linked_viewport(*viewport, cx);
                        });
                    }
                }
                PdfViewEvent::BlockHovered(block_id) => {
                    if let Some(translated_target) = translated_target.upgrade() {
                        translated_target.update(cx, |view, cx| {
                            view.apply_linked_hover(*block_id, cx);
                        });
                    }
                }
                PdfViewEvent::BlockSelected(selection) => {
                    if let Some(translated_target) = translated_target.upgrade() {
                        translated_target.update(cx, |view, cx| {
                            view.apply_linked_selection(*selection, cx);
                        });
                    }
                }
                PdfViewEvent::OpenDocument { .. } | PdfViewEvent::OpenSettings => {}
            },
        )
        .detach();

        let original_target = original.downgrade();
        cx.subscribe_in(
            &translated,
            window,
            move |_, _, event: &PdfViewEvent, _, cx| match event {
                PdfViewEvent::ViewportChanged(viewport) => {
                    if let Some(original_target) = original_target.upgrade() {
                        original_target.update(cx, |view, cx| {
                            view.apply_linked_viewport(*viewport, cx);
                        });
                    }
                }
                PdfViewEvent::BlockHovered(block_id) => {
                    if let Some(original_target) = original_target.upgrade() {
                        original_target.update(cx, |view, cx| {
                            view.apply_linked_hover(*block_id, cx);
                        });
                    }
                }
                PdfViewEvent::BlockSelected(selection) => {
                    if let Some(original_target) = original_target.upgrade() {
                        original_target.update(cx, |view, cx| {
                            view.apply_linked_selection(*selection, cx);
                        });
                    }
                }
                PdfViewEvent::OpenDocument { .. } | PdfViewEvent::OpenSettings => {}
            },
        )
        .detach();
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
                self.cancel_pending_translation_splits_for_pane(&pane, cx);
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
            ThemeAppearanceMode::Light => ThemeAppearanceMode::Dark,
            ThemeAppearanceMode::Dark => ThemeAppearanceMode::Light,
            ThemeAppearanceMode::System => {
                let active_name = store.settings.theme.name(store.system_appearance);
                match store.theme_catalog.appearance(active_name) {
                    ThemeAppearance::Light => ThemeAppearanceMode::Dark,
                    ThemeAppearance::Dark => ThemeAppearanceMode::Light,
                }
            }
        };

        let mut settings = store.settings.clone();
        settings.theme.set_mode(next_mode);
        let _ = cx.global_mut::<SettingsStore>().set_settings(settings);
        sync_component_theme(cx);
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

        let activity = NotificationCenter::try_global(cx)
            .and_then(|center| center.update(cx, |center, cx| center.render_activity(theme, cx)));
        if let Some(activity) = activity {
            div()
                .size_full()
                .flex()
                .flex_col()
                .child(div().flex_1().min_h_0().overflow_hidden().child(center))
                .child(StatusBar::new().right(activity))
                .into_any_element()
        } else {
            center
        }
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

        let destination = TabDestination {
            pane: target_pane.clone(),
            target: target.clone(),
            index: self.drop_index(target_pane, &target, cx),
        };

        if dragged_tab.source_window.window_id() == window.window_handle().window_id() {
            if self.move_tab_to_destination(
                &dragged_tab.source_pane,
                dragged_tab.tab_id,
                destination,
                window,
                cx,
            ) {
                self.complete_transferred_tab_removal(&dragged_tab.source_pane, window, cx);
            }
        } else {
            let Some(source_window) =
                WorkspaceWindowHandle::from_root(dragged_tab.source_window, cx)
            else {
                eprintln!("failed to move tab: source workspace is unavailable");
                return;
            };
            let result = source_window.update(cx, |source, source_window, cx| {
                let transfer = dragged_tab.source_pane.update(cx, |pane, cx| {
                    pane.take_tab_for_transfer(dragged_tab.tab_id, cx)
                });
                if transfer.is_some() {
                    source.complete_transferred_tab_removal(
                        &dragged_tab.source_pane,
                        source_window,
                        cx,
                    );
                }
                transfer
            });
            let transfer = match result {
                Ok(Some(transfer)) => transfer,
                Ok(None) => return,
                Err(error) => {
                    eprintln!("failed to move tab from source window: {error:#}");
                    return;
                }
            };
            self.insert_transferred_tab(
                &destination.pane,
                destination.target,
                destination.index,
                transfer,
                window,
                cx,
            );
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

    fn move_tab_to_destination(
        &mut self,
        source_pane: &Entity<Pane>,
        tab_id: gpui::EntityId,
        destination: TabDestination,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let transfer = source_pane.update(cx, |pane, cx| pane.take_tab_for_transfer(tab_id, cx));
        let Some(transfer) = transfer else {
            return false;
        };

        self.insert_transferred_tab(
            &destination.pane,
            destination.target,
            destination.index,
            transfer,
            window,
            cx,
        );
        true
    }

    fn insert_transferred_tab(
        &mut self,
        target_pane: &Entity<Pane>,
        target: PaneDropTarget,
        to: usize,
        transfer: TabTransfer,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match target {
            PaneDropTarget::Content(Some(direction)) => {
                let new_pane = self.create_empty_pane(window, cx);
                new_pane.update(cx, |pane, cx| {
                    pane.insert_transferred_tab(transfer, to, window, cx)
                });
                self.pane_group.split(target_pane, &new_pane, direction);
                self.active_pane = new_pane;
            }
            PaneDropTarget::Tab(_) | PaneDropTarget::Append | PaneDropTarget::Content(None) => {
                target_pane.update(cx, |pane, cx| {
                    pane.insert_transferred_tab(transfer, to, window, cx);
                });
                self.active_pane = target_pane.clone();
            }
        }
        self.sync_tab_bar_visibility(cx);
        cx.notify();
    }

    fn complete_transferred_tab_removal(
        &mut self,
        pane: &Entity<Pane>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if pane.read(cx).len() == 0 {
            if self.pane_group.panes().len() <= 1 {
                window.remove_window();
            } else {
                self.remove_pane(pane, window, cx);
            }
        } else {
            self.sync_tab_bar_visibility(cx);
            cx.notify();
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

        let bounds = Self::detached_window_bounds(event, window);
        let destination =
            match (self.callbacks.open_workspace_window)(WorkspaceWindowRequest { bounds }, cx) {
                Ok(window) => window,
                Err(error) => {
                    eprintln!("failed to tear tab into new window: {error:#}");
                    return;
                }
            };

        let transfer = dragged_tab.source_pane.update(cx, |pane, cx| {
            pane.take_tab_for_transfer(dragged_tab.tab_id, cx)
        });
        let Some(transfer) = transfer else {
            return;
        };
        let mut transfer = Some(transfer);
        let inserted = destination.update(cx, |destination, destination_window, cx| {
            let Some(transfer) = transfer.take() else {
                return false;
            };
            destination.accept_detached_tab(transfer, destination_window, cx);
            true
        });
        if !matches!(inserted, Ok(true)) {
            if let Some(transfer) = transfer {
                dragged_tab
                    .source_pane
                    .update(cx, |pane, cx| pane.restore_transferred_tab(transfer, cx));
            }
            if let Err(error) = inserted {
                eprintln!("failed to insert detached tab: {error:#}");
            }
            return;
        }

        self.complete_transferred_tab_removal(&dragged_tab.source_pane, window, cx);
        cx.stop_propagation();
    }

    fn accept_detached_tab(
        &mut self,
        transfer: TabTransfer,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let pane = self.active_pane.clone();
        self.insert_transferred_tab(&pane, PaneDropTarget::Append, 0, transfer, window, cx);
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
            let themes = store.theme_catalog.metadata();
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

    fn active_tab_is_new_tab(&self, cx: &Context<Self>) -> bool {
        self.active_pane.read(cx).active_tab_is_new_tab()
    }

    fn set_title_bar_menu_expanded(&mut self, expanded: bool, cx: &mut Context<Self>) {
        let changed = self.title_bar_menu_expanded != expanded
            || (!expanded
                && (self.title_bar_open_menu.is_some() || self.title_bar_popup_menu.is_some()));
        self.title_bar_menu_expanded = expanded;
        if !expanded {
            self.title_bar_open_menu = None;
            self.title_bar_popup_menu = None;
        }
        if changed {
            cx.notify();
        }
    }

    fn open_title_bar_menu(
        &mut self,
        menu: WorkspaceMenu,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let popup_menu = build_popup_menu(menu, self.focus_handle.clone(), window, cx);
        cx.subscribe_in(
            &popup_menu,
            window,
            move |this, popup, _: &DismissEvent, _, cx| {
                if this.title_bar_popup_menu.as_ref() == Some(popup) {
                    this.title_bar_menu_expanded = false;
                    this.title_bar_open_menu = None;
                    this.title_bar_popup_menu = None;
                    cx.notify();
                }
            },
        )
        .detach();

        self.title_bar_menu_expanded = true;
        self.title_bar_open_menu = Some(menu);
        self.title_bar_popup_menu = Some(popup_menu.clone());
        popup_menu.focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    fn toggle_title_bar_menu(
        &mut self,
        menu: WorkspaceMenu,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.title_bar_open_menu == Some(menu) {
            self.title_bar_open_menu = None;
            self.title_bar_popup_menu = None;
            self.focus_handle.focus(window, cx);
            cx.notify();
        } else {
            self.open_title_bar_menu(menu, window, cx);
        }
    }

    fn hover_title_bar_menu(
        &mut self,
        menu: WorkspaceMenu,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.title_bar_open_menu.is_some() && self.title_bar_open_menu != Some(menu) {
            self.open_title_bar_menu(menu, window, cx);
        }
    }

    fn active_tab_label(&self, cx: &Context<Self>) -> Option<SharedString> {
        self.active_pane.read(cx).active_tab_label(cx)
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
        let root_overlays = render_overlays(window, cx);
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
        let title_bar = (!active_pdf_is_presentation).then(|| {
            WorkspaceTitleBar::new(
                cx.entity(),
                self.title_bar_menu_expanded || self.active_tab_is_new_tab(cx),
                self.title_bar_open_menu,
                self.title_bar_popup_menu.clone(),
                self.active_tab_label(cx),
            )
        });
        let document_translation_notification = (!active_pdf_is_presentation)
            .then(|| {
                self.document_translation_jobs
                    .update(cx, |jobs, cx| jobs.render_notification(theme, cx))
            })
            .flatten();
        let center_notifications = (!active_pdf_is_presentation && window.is_window_active())
            .then(|| {
                NotificationCenter::try_global(cx).and_then(|center| {
                    center.update(cx, |center, cx| center.render_notifications(theme, cx))
                })
            })
            .flatten();
        let notification_stack = {
            let mut notifications = Vec::new();
            if let Some(notification) = document_translation_notification {
                notifications.push(notification);
            }
            if let Some(notification) = center_notifications {
                notifications.push(notification);
            }
            (!notifications.is_empty()).then(|| {
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .items_end()
                    .children(notifications)
            })
        };

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
            .on_action(cx.listener(|this, _: &OpenSettings, _, cx| {
                this.open_settings_window(cx);
            }))
            .on_action(cx.listener(|this, _: &OpenKeybindings, window, cx| {
                this.open_keymap_tab(window, cx);
            }))
            .when_some(title_bar, |this, title_bar| this.child(title_bar))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .overflow_hidden()
                    .child(self.render_body(theme, dim_inactive_panes, window, cx)),
            )
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
            .when_some(notification_stack, |this, notifications| {
                this.child(
                    div()
                        .absolute()
                        .right_3()
                        .bottom(px(38.0))
                        .child(notifications),
                )
            })
            .children(root_overlays)
    }
}

#[cfg(test)]
mod tests {
    use gpui::{AppContext as _, Modifiers, TestAppContext};
    use gpui_component::Root;

    use super::{
        Workspace, WorkspaceCallbacks, WorkspaceMenu, WorkspaceWindowHandle, WorkspaceWindowRequest,
    };
    use crate::{library::LibraryStore, pdf_view::RenderCache, settings::SettingsStore};

    fn unavailable_workspace_window(
        _: WorkspaceWindowRequest,
        _: &mut gpui::App,
    ) -> anyhow::Result<WorkspaceWindowHandle> {
        anyhow::bail!("not available in tests")
    }

    fn ignore_settings_window(_: &mut gpui::App) {}

    #[gpui::test]
    fn documentless_workspace_starts_on_a_new_tab(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            cx.set_global(SettingsStore::memory());
            cx.set_global(LibraryStore::memory());
            cx.set_global(RenderCache::new());
        });
        let callbacks = WorkspaceCallbacks {
            open_workspace_window: unavailable_workspace_window,
            open_settings_window: ignore_settings_window,
        };

        let (root, cx) = cx.add_window_view(|window, cx| {
            let workspace = cx.new(|cx| Workspace::new(None, None, callbacks, window, cx));
            Root::new(workspace, window, cx)
        });
        let workspace = root.read_with(cx, |root, _| {
            root.view().clone().downcast::<Workspace>().unwrap()
        });

        workspace.read_with(cx, |workspace, cx| {
            assert!(workspace.active_pane.read(cx).active_tab_is_new_tab());
        });
    }

    #[gpui::test]
    fn hamburger_expands_the_inline_application_menu(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            cx.set_global(SettingsStore::memory());
            cx.set_global(LibraryStore::memory());
            cx.set_global(RenderCache::new());
        });
        let callbacks = WorkspaceCallbacks {
            open_workspace_window: unavailable_workspace_window,
            open_settings_window: ignore_settings_window,
        };

        let (root, cx) = cx.add_window_view(|window, cx| {
            let workspace = cx.new(|cx| Workspace::new_empty(callbacks, window, cx));
            Root::new(workspace, window, cx)
        });
        let workspace = root.read_with(cx, |root, _| {
            root.view().clone().downcast::<Workspace>().unwrap()
        });
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });

        assert!(cx.debug_bounds("workspace-menu-toggle").is_some());
        assert!(cx.debug_bounds("workspace-menu-glyphst").is_none());

        let toggle = cx.debug_bounds("workspace-menu-toggle").unwrap().center();
        cx.simulate_click(toggle, Modifiers::default());
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });

        assert!(cx.debug_bounds("workspace-menu-toggle").is_none());
        assert!(cx.debug_bounds("workspace-menu-glyphst").is_some());
        assert!(cx.debug_bounds("workspace-menu-file").is_some());
        assert!(cx.debug_bounds("workspace-menu-tab").is_some());
        let glyphst_popup = workspace.read_with(cx, |workspace, _| {
            assert!(workspace.title_bar_menu_expanded);
            assert_eq!(workspace.title_bar_open_menu, Some(WorkspaceMenu::Glyphst));
            workspace.title_bar_popup_menu.as_ref().unwrap().entity_id()
        });

        workspace.update_in(cx, |workspace, window, cx| {
            workspace.hover_title_bar_menu(WorkspaceMenu::File, window, cx);
        });
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        workspace.read_with(cx, |workspace, _| {
            assert_eq!(workspace.title_bar_open_menu, Some(WorkspaceMenu::File));
            assert_ne!(
                workspace.title_bar_popup_menu.as_ref().unwrap().entity_id(),
                glyphst_popup
            );
        });

        let drag_region = cx
            .debug_bounds("workspace-title-bar-drag-region")
            .unwrap()
            .center();
        cx.simulate_click(drag_region, Modifiers::default());
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });

        assert!(cx.debug_bounds("workspace-menu-toggle").is_some());
        assert!(cx.debug_bounds("workspace-menu-glyphst").is_none());
    }

    #[gpui::test]
    fn root_hosted_workspace_tabs_render_and_interact(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            cx.set_global(SettingsStore::memory());
            cx.set_global(LibraryStore::memory());
            cx.set_global(RenderCache::new());
        });
        let callbacks = WorkspaceCallbacks {
            open_workspace_window: unavailable_workspace_window,
            open_settings_window: ignore_settings_window,
        };

        let (root, cx) = cx.add_window_view(|window, cx| {
            let workspace = cx.new(|cx| Workspace::new(None, None, callbacks, window, cx));
            Root::new(workspace, window, cx)
        });
        let workspace = root.read_with(cx, |root, _| {
            root.view().clone().downcast::<Workspace>().unwrap()
        });
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.open_new_tab(window, cx);
        });
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });

        assert!(cx.debug_bounds("pane-tab-0").is_some());
        assert!(cx.debug_bounds("pane-tab-1").is_some());

        let first_tab = cx.debug_bounds("pane-tab-0").unwrap().center();
        cx.simulate_click(first_tab, Modifiers::default());
        workspace.read_with(cx, |workspace, cx| {
            assert_eq!(workspace.active_pane.read(cx).active_tab_ix(), 0);
        });

        let new_tab = cx.debug_bounds("pane-new-tab").unwrap().center();
        cx.simulate_click(new_tab, Modifiers::default());
        workspace.read_with(cx, |workspace, cx| {
            let pane = workspace.active_pane.read(cx);
            assert_eq!(pane.len(), 3);
            assert_eq!(pane.active_tab_ix(), 2);
        });
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });

        let close_tab = cx.debug_bounds("pane-tab-close-2").unwrap().center();
        cx.simulate_click(close_tab, Modifiers::default());
        workspace.read_with(cx, |workspace, cx| {
            assert_eq!(workspace.active_pane.read(cx).len(), 2);
        });
    }
}
