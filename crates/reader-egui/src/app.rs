//! The reader window: tabs (home and documents), chrome, the document view
//! with its effects, preferences and reading positions.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use eframe::egui_wgpu;
use glyphst_core::{DocumentFormat, DocumentLayout, EpubLayout};
use glyphst_effects::EffectKind;
use glyphst_library::{DocumentPosition, HighlightColor, LibraryStore, SemanticTextRange};
use glyphst_pdf::text::LinkTarget;

use crate::{
    annotations::{self, AnnotationId, Annotations},
    document::FrameTimes,
    document_tab::{DocumentTab, ViewAction, ViewInput},
    find::{self, FindBarAction},
    gamepad::{GamepadInput, Gamepads},
    gpu::{
        EffectUse, PAGE_BUDGET_BYTES, PageKey, ReaderGpu, TransitionUse, ViewCallback, pixel_size,
    },
    harness::Harness,
    home::{Home, HomeAction},
    pickers::{Pickers, is_document},
    prefs::{LastSession, Prefs, SessionFile},
    sidebar::{self, SidebarAction, SidebarState, SidebarTab},
    text_overlay::{PageMap, Selection, Span, fill_rects, page_at},
    theme::{self, ThemeKind, ThemePreference, Tokens},
    transition::ViewTransition,
};

/// Positions are written to the library at most this often while reading.
const POSITION_SAVE_INTERVAL: Duration = Duration::from_secs(1);
const TAB_MIN_WIDTH: f32 = 90.0;
const TAB_MAX_LABEL: usize = 32;
const TAB_OVERFLOW_WIDTH: f32 = 28.0;
/// Margin on each side of a reflowed column, in points.
const REFLOW_MARGIN: f32 = 24.0;
/// Height of a reflowed page, in points (as the GPUI Reader lays out).
const REFLOW_PAGE_HEIGHT: f32 = 960.0;
/// The view must keep a new width this long before a reflow.
const REFLOW_SETTLE: Duration = Duration::from_millis(300);
/// Width the sidebar is assumed to take before it is first shown.
const SIDEBAR_WIDTH: f32 = 280.0;
/// Hit slop around glyphs, in egui points.
const GLYPH_SLOP: f32 = 2.0;
/// Pixels per frame the view scrolls while a selection is dragged past
/// its edge.
const SELECT_SCROLL: f64 = 14.0;

/// How the app was started.
pub struct Launch {
    pub files: Vec<PathBuf>,
    pub harness: Option<Harness>,
    pub background: Option<String>,
    pub transition: Option<String>,
    /// Opens the documents at this zoom (relative to fit width).
    pub zoom: Option<f32>,
    /// Scripted runs: search for this text, select this page's text (from
    /// 1), show this sidebar tab, lay EPUBs out at this font size.
    pub find: Option<String>,
    /// Opens the documents at this page (from 1).
    pub page: Option<usize>,
    pub select_page: Option<usize>,
    pub sidebar: Option<SidebarTab>,
    pub font_size: Option<f32>,
}

/// What a primary-button drag on the document does.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Drag {
    Pan,
    Select { page: usize },
}

/// The popover editing a highlight or note clicked on the page.
struct NoteEditor {
    id: AnnotationId,
    at: egui::Pos2,
    body: String,
    color: HighlightColor,
    /// Opened this frame: the click that opened it is not a click outside.
    fresh: bool,
}

pub struct ReaderApp {
    pub(crate) prefs: Prefs,
    theme_kind: Option<ThemeKind>,
    library: LibraryStore,
    pub(crate) tabs: Vec<DocumentTab>,
    /// Index into `tabs`; `None` shows the home tab.
    pub(crate) active: Option<usize>,
    next_tab_id: u64,
    home: Home,
    pickers: Pickers,
    pub(crate) render_state: egui_wgpu::RenderState,
    pub(crate) transition: Option<ViewTransition>,
    started: Instant,
    gamepads: Result<Gamepads, String>,
    pub(crate) frames: FrameTimes,
    continuous: bool,
    pub(crate) repaints: u64,
    last_update: Instant,
    notices: Vec<String>,
    show_stats: bool,
    sidebar_state: SidebarState,
    drag: Option<Drag>,
    note_editor: Option<NoteEditor>,
    /// The page a context menu was opened over.
    context_page: Option<usize>,
    pub(crate) launch_find: Option<String>,
    pub(crate) launch_select: Option<usize>,
    font_size: Option<f32>,
    page_input: String,
    saved_positions: HashMap<u64, DocumentPosition>,
    last_position_save: Instant,
    /// Scripted runs neither save preferences nor touch the library.
    persist: bool,
    pub(crate) harness: Option<Harness>,
    pub(crate) first_page_ms: Option<f64>,
    pub(crate) upload_ms: Vec<f64>,
    /// Actions queued for the next frame of the document view.
    pub(crate) pending_actions: Vec<ViewAction>,
}

impl ReaderApp {
    pub fn new(cc: &eframe::CreationContext<'_>, launch: Launch) -> anyhow::Result<Self> {
        let render_state = cc
            .wgpu_render_state
            .clone()
            .ok_or_else(|| anyhow::anyhow!("the reader needs eframe's wgpu renderer"))?;
        let user_effects = dirs::config_dir().map(|dir| dir.join("glyphst").join("effects"));
        ReaderGpu::install(&render_state, user_effects.as_deref());
        crate::fonts::install(&cc.egui_ctx);
        let persist = launch.harness.is_none();
        let mut prefs = Prefs::load(cc.storage);
        if let Some(background) = launch.background {
            prefs.background = background;
        }
        if let Some(transition) = launch.transition {
            prefs.transition = (transition != "none").then_some(transition);
        }
        if let Some(tab) = launch.sidebar {
            prefs.sidebar = tab;
            prefs.show_sidebar = true;
        }
        let mut app = Self {
            prefs,
            theme_kind: None,
            library: LibraryStore::load(),
            tabs: Vec::new(),
            active: None,
            next_tab_id: 1,
            home: Home::default(),
            pickers: Pickers::default(),
            render_state,
            transition: None,
            started: Instant::now(),
            gamepads: Gamepads::new(),
            frames: FrameTimes::default(),
            continuous: false,
            repaints: 0,
            last_update: Instant::now(),
            notices: Vec::new(),
            show_stats: launch.harness.is_some(),
            sidebar_state: SidebarState::default(),
            drag: None,
            note_editor: None,
            context_page: None,
            launch_find: launch.find,
            launch_select: launch.select_page,
            font_size: launch.font_size,
            page_input: String::new(),
            saved_positions: HashMap::new(),
            last_position_save: Instant::now(),
            persist,
            harness: launch.harness,
            first_page_ms: None,
            upload_ms: Vec::new(),
            pending_actions: Vec::new(),
        };
        let ctx = cc.egui_ctx.clone();
        if launch.files.is_empty() && app.prefs.reopen_last_session && persist {
            let session = app.prefs.last_session.clone();
            for file in &session.files {
                app.open_quietly(Path::new(&file.path), &ctx, Some(file.page));
            }
            app.active = session.active.filter(|&active| active < app.tabs.len());
        } else {
            let page = launch.select_page.or(launch.page).map(|page| page - 1);
            for file in &launch.files {
                app.open_quietly(file, &ctx, page);
                if let (Some(zoom), Some(tab)) = (launch.zoom, app.tabs.last_mut()) {
                    tab.set_initial_zoom(zoom);
                }
            }
        }
        app.transition = None;
        Ok(app)
    }

    pub(crate) fn with_gpu<R>(&self, f: impl FnOnce(&mut ReaderGpu) -> R) -> Option<R> {
        let mut renderer = self.render_state.renderer.write();
        renderer.callback_resources.get_mut::<ReaderGpu>().map(f)
    }

    fn is_resident(&self, key: PageKey) -> bool {
        self.with_gpu(|gpu| gpu.is_resident(key)).unwrap_or(false)
    }

    /// Opens `path` in a new tab (or shows the tab that has it) and makes it
    /// active. `page` overrides the remembered position.
    pub fn open(&mut self, path: &Path, ctx: &egui::Context, page: Option<usize>) {
        if let Some(index) = self.tabs.iter().position(|tab| tab.path == path) {
            self.activate(Some(index));
            return;
        }
        let mut restore = self.library.history_position(path);
        if let Some(page) = page {
            let mut position =
                restore.unwrap_or_else(|| DocumentPosition::fixed(0.0, 0.0, 1.0, page + 1));
            position.page = page + 1;
            restore = Some(position);
        }
        let layout = self.layout_for(path, ctx);
        match DocumentTab::open(self.next_tab_id, path, ctx, restore, layout) {
            Ok(mut tab) => {
                tab.annotations = Annotations::load(&self.library, path);
                self.next_tab_id += 1;
                self.tabs.push(tab);
                if self.persist {
                    let _ = self.library.mark_library_item_opened(path);
                }
                self.activate(Some(self.tabs.len() - 1));
            }
            Err(error) => self
                .notices
                .push(format!("Could not open {}: {error:#}", path.display())),
        }
    }

    /// Opens without a transition (startup, session restore).
    fn open_quietly(&mut self, path: &Path, ctx: &egui::Context, page: Option<usize>) {
        self.open(path, ctx, page);
        self.transition = None;
    }

    /// Makes another tab active, with the transition effect between two
    /// document views.
    pub(crate) fn activate(&mut self, next: Option<usize>) {
        if next == self.active {
            return;
        }
        let previous = self.active;
        self.active = next;
        let (Some(previous), Some(next)) = (previous, next) else {
            return;
        };
        let (Some(from), Some(effect)) = (self.tabs.get(previous), self.prefs.transition.clone())
        else {
            return;
        };
        let direction = if next >= previous { 1.0 } else { -1.0 };
        self.begin_transition(effect, from.id, direction);
    }

    /// Reflowable documents (EPUB) open laid out for the view; others keep
    /// their pages.
    fn layout_for(&self, path: &Path, ctx: &egui::Context) -> DocumentLayout {
        let epub = path
            .extension()
            .and_then(|extension| extension.to_str())
            .and_then(DocumentFormat::from_extension)
            == Some(DocumentFormat::Epub);
        if !epub {
            return DocumentLayout::Fixed;
        }
        let mut width = ctx.content_rect().width();
        if self.prefs.show_sidebar {
            width -= SIDEBAR_WIDTH;
        }
        DocumentLayout::Reflow(self.epub_layout(path, width))
    }

    /// The reflowed layout for a view `width` points wide, from the
    /// document's reading preferences in the library.
    fn epub_layout(&self, path: &Path, width: f32) -> EpubLayout {
        let mut preferences = self
            .library
            .epub_reader_preferences(path)
            .unwrap_or_default();
        if let Some(size) = self.font_size {
            preferences.font_size = size;
        }
        preferences.layout_for_viewport(width - 2.0 * REFLOW_MARGIN, REFLOW_PAGE_HEIGHT)
    }

    /// Lays a reflowed tab out again and drops its pages from the GPU.
    fn relayout(&mut self, index: usize, layout: EpubLayout, ctx: &egui::Context) {
        let tab = &mut self.tabs[index];
        let id = tab.id;
        if let Err(error) = tab.relayout(DocumentLayout::Reflow(layout), ctx) {
            self.notices
                .push(format!("Could not lay out the book: {error:#}"));
        }
        self.with_gpu(|gpu| gpu.drop_tab(id));
    }

    /// Reflows a tab once the view has kept a width that no longer fits
    /// its column.
    fn reflow_to_view(&mut self, index: usize, view_width: f32, now: Instant, ctx: &egui::Context) {
        let tab = &self.tabs[index];
        let DocumentLayout::Reflow(current) = tab.document_layout else {
            return;
        };
        let wanted = self.epub_layout(&tab.path, view_width);
        let changed = (wanted.width - current.width).abs() > current.width * 0.1
            || wanted.font_size != current.font_size;
        let tab = &mut self.tabs[index];
        if !changed {
            tab.reflow_mismatch = None;
            return;
        }
        let since = *tab.reflow_mismatch.get_or_insert(now);
        if now - since >= REFLOW_SETTLE && self.drag.is_none() {
            self.relayout(index, wanted, ctx);
        }
    }

    /// Makes the active EPUB's text larger or smaller.
    fn change_font_size(&mut self, step: f32) {
        let Some(active) = self.active else {
            return;
        };
        let path = self.tabs[active].path.clone();
        let mut preferences = self
            .library
            .epub_reader_preferences(&path)
            .unwrap_or_default();
        let size = (self.font_size.unwrap_or(preferences.font_size) + step).clamp(8.0, 48.0);
        if self.persist {
            preferences.font_size = size;
            if let Err(error) = self.library.set_epub_reader_preferences(&path, preferences) {
                self.notices
                    .push(format!("Could not save the text size: {error:#}"));
            }
            self.font_size = None;
        } else {
            self.font_size = Some(size);
        }
        // The next frame sees the layout no longer matches and reflows.
        self.tabs[active].reflow_mismatch = Some(Instant::now() - REFLOW_SETTLE);
    }

    fn reload_annotations(&mut self, index: usize) {
        let tab = &mut self.tabs[index];
        tab.annotations = Annotations::load(&self.library, &tab.path);
        let text = &tab.text;
        tab.annotations.resolve(text);
    }

    /// Highlights the selection, or adds a note on it and opens the note.
    fn annotate_selection(&mut self, color: HighlightColor, note: bool, at: egui::Pos2) {
        let Some(active) = self.active else {
            return;
        };
        let tab = &self.tabs[active];
        let Some(selection) = tab.selection.clone() else {
            return;
        };
        let Some(text) = tab.text.get(selection.page).cloned() else {
            return;
        };
        let glyphs = selection.glyphs(&text);
        let rects = annotations::library_rects(selection.page, &selection.rects(&text));
        let quote = selection.text(&text);
        let (Some(&first), Some(&last)) = (glyphs.first(), glyphs.last()) else {
            return;
        };
        if rects.is_empty() {
            return;
        }
        let semantic = if tab.is_reflowed() {
            let range = tab
                .text
                .anchor(selection.page, first)
                .zip(tab.text.anchor(selection.page, last))
                .map(|(start, end)| SemanticTextRange { start, end });
            if range.is_none() {
                self.notices
                    .push("The book's text is still being read; try again in a moment.".into());
                return;
            }
            range
        } else {
            None
        };
        let path = tab.path.clone();
        let created = match (semantic, note) {
            (Some(range), false) => self
                .library
                .add_reflow_highlight(&path, color, range, rects, quote, "")
                .map(|h| AnnotationId::Highlight(h.id)),
            (None, false) => self
                .library
                .add_highlight(&path, color, rects, quote, "")
                .map(|h| AnnotationId::Highlight(h.id)),
            (Some(range), true) => self
                .library
                .add_reflow_note(&path, color, range, rects, quote, "")
                .map(|n| AnnotationId::Note(n.id)),
            (None, true) => self
                .library
                .add_note(&path, color, rects, quote, "")
                .map(|n| AnnotationId::Note(n.id)),
        };
        match created {
            Ok(id) => {
                self.tabs[active].selection = None;
                self.reload_annotations(active);
                if note {
                    self.note_editor = Some(NoteEditor {
                        id,
                        at,
                        body: String::new(),
                        color,
                        fresh: true,
                    });
                }
            }
            Err(error) => self.notices.push(format!("Could not save: {error:#}")),
        }
    }

    fn set_annotation_color(&mut self, id: AnnotationId, color: HighlightColor) {
        let result = match id {
            AnnotationId::Highlight(id) => self.library.set_highlight_color(id, color),
            AnnotationId::Note(note) => {
                let body = self.annotation_body(id);
                self.library.update_note(note, body, color)
            }
        };
        self.after_edit(result);
    }

    fn set_note_body(&mut self, id: AnnotationId, body: String) {
        let AnnotationId::Note(note) = id else {
            return;
        };
        let color = self
            .active
            .and_then(|active| self.tabs[active].annotations.get(id).map(|item| item.color))
            .unwrap_or(HighlightColor::Yellow);
        let result = self.library.update_note(note, body, color);
        self.after_edit(result);
    }

    fn remove_annotation(&mut self, id: AnnotationId) {
        let result = match id {
            AnnotationId::Highlight(id) => self.library.remove_highlight(id),
            AnnotationId::Note(id) => self.library.remove_note(id),
        };
        self.after_edit(result);
    }

    fn annotation_body(&self, id: AnnotationId) -> String {
        self.active
            .and_then(|active| {
                self.tabs[active]
                    .annotations
                    .get(id)
                    .map(|item| item.body.clone())
            })
            .unwrap_or_default()
    }

    fn after_edit(&mut self, result: anyhow::Result<bool>) {
        if let Err(error) = result {
            self.notices.push(format!("Could not save: {error:#}"));
        }
        if let Some(active) = self.active {
            self.reload_annotations(active);
        }
    }

    fn add_bookmark(&mut self) {
        let Some(active) = self.active else {
            return;
        };
        let tab = &self.tabs[active];
        let (path, position, title) = (tab.path.clone(), tab.position(), tab.words_at_top(8));
        let result = self.library.add_bookmark(&path, title, position);
        self.after_edit(result.map(|_| true));
        self.prefs.show_sidebar = true;
        self.prefs.sidebar = SidebarTab::Bookmarks;
    }

    fn apply_sidebar(&mut self, action: SidebarAction, actions: &mut Vec<ViewAction>) {
        match action {
            SidebarAction::View(action) => actions.push(action),
            SidebarAction::GoToBookmark(id) => {
                if let Some(active) = self.active {
                    let tab = &mut self.tabs[active];
                    let position = tab
                        .annotations
                        .bookmarks
                        .iter()
                        .find(|bookmark| bookmark.id == id)
                        .map(|bookmark| bookmark.position.clone());
                    if let Some(position) = position {
                        tab.go_to_position(&position);
                    }
                }
            }
            SidebarAction::AddBookmark => self.add_bookmark(),
            SidebarAction::RenameBookmark(id, title) => {
                let result = self.library.update_bookmark_title(id, Some(title));
                self.after_edit(result);
            }
            SidebarAction::RemoveBookmark(id) => {
                let result = self.library.remove_bookmark(id);
                self.after_edit(result);
            }
            SidebarAction::SetColor(id, color) => self.set_annotation_color(id, color),
            SidebarAction::SetBody(id, body) => self.set_note_body(id, body),
            SidebarAction::Remove(id) => self.remove_annotation(id),
        }
    }

    /// Steps to the next or previous match.
    fn find_step(&mut self, forward: bool) {
        if let Some(active) = self.active {
            self.tabs[active].find.step(forward);
        }
    }

    fn copy_selection(&self, ctx: &egui::Context) {
        if let Some(text) = self
            .active
            .and_then(|active| self.tabs[active].selected_text())
        {
            ctx.copy_text(text);
        }
    }

    fn begin_transition(&mut self, effect: String, from_tab: u64, direction: f32) {
        let duration = self
            .with_gpu(|gpu| gpu.effects().find(&effect).map(|def| def.duration_ms))
            .flatten();
        if let Some(duration) = duration {
            self.transition = Some(ViewTransition::new(
                effect,
                from_tab,
                direction,
                duration,
                Instant::now(),
            ));
        }
    }

    fn close(&mut self, index: usize) {
        self.save_position(index, true);
        let tab = self.tabs.remove(index);
        self.saved_positions.remove(&tab.id);
        self.with_gpu(|gpu| gpu.drop_tab(tab.id));
        self.transition = None;
        self.active = match self.active {
            Some(active) if active == index => {
                if self.tabs.is_empty() {
                    None
                } else {
                    Some(index.min(self.tabs.len() - 1))
                }
            }
            Some(active) if active > index => Some(active - 1),
            other => other,
        };
    }

    /// Records a tab's position in the library when it moved.
    fn save_position(&mut self, index: usize, flush: bool) {
        if !self.persist {
            return;
        }
        let Some(tab) = self.tabs.get(index) else {
            return;
        };
        let position = tab.position();
        if self.saved_positions.get(&tab.id) != Some(&position) {
            self.library.record_history(&tab.path, position.clone());
            self.saved_positions.insert(tab.id, position);
            if flush {
                let _ = self.library.save();
            }
        }
    }

    fn session(&self) -> LastSession {
        LastSession {
            files: self
                .tabs
                .iter()
                .map(|tab| SessionFile {
                    path: tab.path.display().to_string(),
                    page: tab.current_page(),
                })
                .collect(),
            active: self.active,
        }
    }

    /// Uploads at most one loaded page per frame (the active tab's first),
    /// so a heavy page costs one long frame rather than several in a row.
    /// Swaps in raster layers re-rasterized for the current view.
    fn upload_ready_layers(&mut self) {
        let (device, queue) = (
            self.render_state.device.clone(),
            self.render_state.queue.clone(),
        );
        for index in 0..self.tabs.len() {
            let tab_id = self.tabs[index].id;
            while let Some((page, layer, image)) = self.tabs[index].ready_layers.pop_front() {
                let key = PageKey { tab: tab_id, page };
                self.with_gpu(|gpu| gpu.replace_layer(&device, &queue, key, layer, &image));
            }
        }
    }

    fn upload_ready_page(&mut self) {
        let (device, queue) = (
            self.render_state.device.clone(),
            self.render_state.queue.clone(),
        );
        self.with_gpu(|gpu| gpu.step_overview(&device, &queue));
        let order: Vec<usize> = self.active.into_iter().chain(0..self.tabs.len()).collect();
        for index in order {
            let Some(ready) = self.tabs[index].ready.pop_front() else {
                continue;
            };
            let key = PageKey {
                tab: self.tabs[index].id,
                page: ready.page,
            };
            let (device, queue) = (
                self.render_state.device.clone(),
                self.render_state.queue.clone(),
            );
            if let Some(ms) = self
                .with_gpu(|gpu| gpu.upload(&device, &queue, key, &ready.prepared, &ready.layers))
            {
                self.upload_ms.push(ms);
            }
            if self.first_page_ms.is_none() {
                self.first_page_ms = Some(self.started.elapsed().as_secs_f64() * 1000.0);
            }
            return;
        }
    }

    fn apply_theme(&mut self, ctx: &egui::Context) {
        let fallback = self.theme_kind.unwrap_or_default();
        let kind = self.prefs.theme.resolve(ctx.system_theme(), fallback);
        if self.theme_kind != Some(kind) {
            theme::apply(ctx, kind);
            self.theme_kind = Some(kind);
        }
    }

    fn global_actions(&mut self, ctx: &egui::Context) -> Vec<ViewAction> {
        let typing = ctx.memory(|memory| memory.focused().is_some());
        let mut actions = Vec::new();
        let (ctrl, shift, alt) =
            ctx.input(|i| (i.modifiers.command, i.modifiers.shift, i.modifiers.alt));
        let pressed = |key| ctx.input(|i| i.key_pressed(key));
        if let Some(active) = self.active {
            if ctrl && pressed(egui::Key::F) {
                let tab = &mut self.tabs[active];
                let page = tab.current_page();
                tab.find.show(page);
            }
            if ctrl && pressed(egui::Key::D) {
                self.add_bookmark();
            }
            if pressed(egui::Key::F3) {
                self.find_step(!shift);
            }
            let (back, forward) = ctx.input(|i| {
                (
                    i.pointer.button_pressed(egui::PointerButton::Extra1)
                        || (alt && i.key_pressed(egui::Key::ArrowLeft)),
                    i.pointer.button_pressed(egui::PointerButton::Extra2)
                        || (alt && i.key_pressed(egui::Key::ArrowRight)),
                )
            });
            if back {
                actions.push(ViewAction::Back);
            }
            if forward {
                actions.push(ViewAction::Forward);
            }
            if !typing {
                let copy = ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Copy)));
                if copy {
                    self.copy_selection(ctx);
                }
                if ctrl && pressed(egui::Key::A) {
                    let tab = &mut self.tabs[active];
                    let page = tab.current_page();
                    tab.select_page(page);
                }
                if pressed(egui::Key::Escape) {
                    let tab = &mut self.tabs[active];
                    if self.note_editor.is_some() {
                        self.note_editor = None;
                    } else if tab.selection.is_some() {
                        tab.selection = None;
                    } else if tab.find.open {
                        tab.find.close();
                    }
                }
            }
        }
        if ctrl && pressed(egui::Key::O) {
            self.pickers.open_documents(ctx);
        }
        if ctrl
            && pressed(egui::Key::W)
            && let Some(active) = self.active
        {
            self.close(active);
        }
        if ctrl && pressed(egui::Key::Tab) {
            let count = self.tabs.len() + 1;
            let current = self.active.map_or(0, |active| active + 1);
            let next = if shift {
                (current + count - 1) % count
            } else {
                (current + 1) % count
            };
            self.activate(next.checked_sub(1));
        }
        if typing || ctrl || alt {
            return actions;
        }
        let map = [
            (egui::Key::PageDown, ViewAction::NextPage),
            (egui::Key::Space, ViewAction::NextPage),
            (egui::Key::PageUp, ViewAction::PreviousPage),
            (egui::Key::Plus, ViewAction::ZoomIn),
            (egui::Key::Equals, ViewAction::ZoomIn),
            (egui::Key::Minus, ViewAction::ZoomOut),
            (egui::Key::Num0, ViewAction::FitWidth),
            (egui::Key::P, ViewAction::TogglePresent),
            (egui::Key::Home, ViewAction::GoToPage(0)),
            (egui::Key::End, ViewAction::GoToPage(usize::MAX)),
        ];
        for (key, action) in map {
            if pressed(key) {
                actions.push(action);
            }
        }
        if pressed(egui::Key::F9) {
            self.prefs.show_sidebar = !self.prefs.show_sidebar;
        }
        if pressed(egui::Key::F12) {
            self.show_stats = !self.show_stats;
        }
        actions
    }

    fn tab_strip(&mut self, ui: &mut egui::Ui) {
        let tokens = Tokens::get(ui.ctx());
        let labels: Vec<String> = std::iter::once("Home".to_string())
            .chain(
                self.tabs
                    .iter()
                    .map(|tab| truncate(&tab.title, TAB_MAX_LABEL)),
            )
            .collect();
        let font = egui::TextStyle::Button.resolve(ui.style());
        let natural: Vec<f32> = labels
            .iter()
            .enumerate()
            .map(|(index, label)| {
                let text = ui
                    .painter()
                    .layout_no_wrap(label.clone(), font.clone(), tokens.text)
                    .size()
                    .x;
                text + if index == 0 { 24.0 } else { 44.0 }
            })
            .collect();
        let selected = self.active.map_or(0, |active| active + 1);
        let strip = ui.available_rect_before_wrap();
        let menu_width = 36.0;
        let fit = craft_ui::tabs::fit(
            &natural,
            selected,
            strip.width() - menu_width,
            TAB_MIN_WIDTH,
            TAB_OVERFLOW_WIDTH,
        );
        let mut x = strip.min.x;
        let mut select = None;
        let mut close = None;
        for &(index, width) in &fit.shown {
            let rect = egui::Rect::from_min_size(
                egui::pos2(x, strip.min.y),
                egui::vec2(width, strip.height()),
            );
            x += width;
            let id = ui.id().with(("tab", index));
            let is_selected = index == selected;
            let label = labels[index].clone();
            let response = craft_ui::tabs::Tab::new(id, &label, is_selected)
                .focus_stroke(egui::Stroke::new(1.0, tokens.accent))
                .show_at(ui, rect, |ui, response| {
                    let fill = if is_selected {
                        tokens.chrome
                    } else if response.hovered() {
                        tokens.hover
                    } else {
                        tokens.titlebar
                    };
                    ui.painter()
                        .rect_filled(rect.shrink2(egui::vec2(1.0, 0.0)), 4.0, fill);
                    let text_rect = rect.shrink2(egui::vec2(10.0, 0.0));
                    ui.painter().with_clip_rect(text_rect).text(
                        text_rect.left_center(),
                        egui::Align2::LEFT_CENTER,
                        &label,
                        font.clone(),
                        if is_selected {
                            tokens.text
                        } else {
                            tokens.text_muted
                        },
                    );
                });
            if response.clicked() {
                select = Some(index);
            }
            if index > 0 {
                if response.middle_clicked() {
                    close = Some(index - 1);
                }
                let close_rect = egui::Rect::from_center_size(
                    egui::pos2(rect.right() - 16.0, rect.center().y),
                    egui::vec2(18.0, 18.0),
                );
                let close_button = ui.interact(close_rect, id.with("close"), egui::Sense::click());
                if close_button.hovered() {
                    ui.painter().rect_filled(close_rect, 4.0, tokens.pressed);
                }
                ui.painter().text(
                    close_rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "×",
                    font.clone(),
                    tokens.text_muted,
                );
                if close_button.clicked() {
                    close = Some(index - 1);
                }
            }
        }
        if !fit.overflow.is_empty() {
            let rect = egui::Rect::from_min_size(
                egui::pos2(x, strip.min.y),
                egui::vec2(fit.overflow_width, strip.height()),
            );
            ui.put(rect, |ui: &mut egui::Ui| {
                ui.menu_button("»", |ui| {
                    for &index in &fit.overflow {
                        if ui.button(&labels[index]).clicked() {
                            select = Some(index);
                            ui.close();
                        }
                    }
                })
                .response
            });
        }
        let menu_rect = egui::Rect::from_min_max(
            egui::pos2(strip.right() - menu_width, strip.min.y),
            strip.max,
        );
        ui.put(menu_rect, |ui: &mut egui::Ui| {
            ui.menu_button("☰", |ui| self.menu(ui)).response
        });
        if let Some(index) = close {
            self.close(index);
        } else if let Some(index) = select {
            self.activate(index.checked_sub(1));
        }
    }

    fn menu(&mut self, ui: &mut egui::Ui) {
        if ui
            .add_enabled(
                !self.pickers.is_showing(),
                egui::Button::new("Open documents…  Ctrl+O"),
            )
            .clicked()
        {
            self.pickers.open_documents(ui.ctx());
            ui.close();
        }
        ui.separator();
        ui.label("Theme");
        for preference in ThemePreference::ALL {
            ui.radio_value(&mut self.prefs.theme, preference, preference.label());
        }
        ui.separator();
        let defs: Vec<_> = self
            .with_gpu(|gpu| gpu.effects().defs().cloned().collect())
            .unwrap_or_default();
        ui.label("Transition");
        ui.radio_value(&mut self.prefs.transition, None, "None");
        for def in defs.iter().filter(|def| def.kind == EffectKind::Transition) {
            ui.radio_value(&mut self.prefs.transition, Some(def.id.clone()), &def.name);
        }
        ui.separator();
        ui.label("Background");
        for def in defs.iter().filter(|def| def.kind == EffectKind::Background) {
            let label = if def.animated {
                format!("{} (animated)", def.name)
            } else {
                def.name.clone()
            };
            ui.radio_value(&mut self.prefs.background, def.id.clone(), label);
        }
        ui.separator();
        ui.checkbox(&mut self.prefs.reopen_last_session, "Reopen last session");
        ui.checkbox(&mut self.prefs.show_sidebar, "Sidebar  F9");
        ui.checkbox(&mut self.show_stats, "Statistics  F12");
        let gamepads = match &self.gamepads {
            Ok(pads) if pads.connected().is_empty() => "none connected".to_string(),
            Ok(pads) => pads.connected().join(", "),
            Err(error) => format!("unavailable ({error})"),
        };
        ui.label(format!("Gamepad: {gamepads}"));
        let errors = self
            .with_gpu(|gpu| {
                gpu.effects()
                    .errors()
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if !errors.is_empty() {
            ui.separator();
            ui.collapsing(format!("Effect errors ({})", errors.len()), |ui| {
                for error in errors {
                    ui.label(error);
                }
            });
        }
    }

    fn toolbar(&mut self, ui: &mut egui::Ui, actions: &mut Vec<ViewAction>) {
        let Some(active) = self.active else {
            return;
        };
        let tab = &self.tabs[active];
        let pages = tab.layout.len();
        let mut find = false;
        let mut font_step = 0.0;
        ui.horizontal(|ui| {
            ui.toggle_value(&mut self.prefs.show_sidebar, "☰")
                .on_hover_text("Sidebar (F9)");
            if ui
                .add_enabled(tab.can_go_back(), egui::Button::new("⏴"))
                .on_hover_text("Back (Alt+Left)")
                .clicked()
            {
                actions.push(ViewAction::Back);
            }
            if ui
                .add_enabled(tab.can_go_forward(), egui::Button::new("⏵"))
                .on_hover_text("Forward (Alt+Right)")
                .clicked()
            {
                actions.push(ViewAction::Forward);
            }
            ui.separator();
            if ui.button("◀").clicked() {
                actions.push(ViewAction::PreviousPage);
            }
            let current = tab.current_page() + 1;
            let response = ui.add(
                egui::TextEdit::singleline(&mut self.page_input)
                    .desired_width(48.0)
                    .hint_text(current.to_string()),
            );
            if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                if let Ok(page) = self.page_input.trim().parse::<usize>() {
                    actions.push(ViewAction::GoToPage(page.saturating_sub(1)));
                }
                self.page_input.clear();
            }
            ui.label(format!("/ {pages}"));
            if ui.button("▶").clicked() {
                actions.push(ViewAction::NextPage);
            }
            ui.separator();
            if ui.button("−").clicked() {
                actions.push(ViewAction::ZoomOut);
            }
            ui.label(format!("{:.0}%", tab.view.scale / tab.fit_scale() * 100.0));
            if ui.button("+").clicked() {
                actions.push(ViewAction::ZoomIn);
            }
            if ui.button("Fit width").clicked() {
                actions.push(ViewAction::FitWidth);
            }
            if let DocumentLayout::Reflow(layout) = tab.document_layout {
                ui.separator();
                if ui.button("A−").on_hover_text("Smaller text").clicked() {
                    font_step = -1.0;
                }
                ui.label(format!("{:.0} pt", layout.font_size));
                if ui.button("A+").on_hover_text("Larger text").clicked() {
                    font_step = 1.0;
                }
            }
            ui.separator();
            let presenting = tab.present.is_some();
            if ui.selectable_label(presenting, "Present  P").clicked() {
                actions.push(ViewAction::TogglePresent);
            }
            ui.separator();
            if ui
                .selectable_label(tab.find.open, "🔍 Find")
                .on_hover_text("Find (Ctrl+F)")
                .clicked()
            {
                find = true;
            }
        });
        if find {
            let tab = &mut self.tabs[active];
            if tab.find.open {
                tab.find.close();
            } else {
                let page = tab.current_page();
                tab.find.show(page);
            }
        }
        if font_step != 0.0 {
            self.change_font_size(font_step);
        }
    }

    /// The document view: input, layout, loading, the GPU callback and
    /// loading placeholders. Returns whether it is animating and whether
    /// every visible page is drawn.
    fn document_view(
        &mut self,
        ui: &mut egui::Ui,
        actions: Vec<ViewAction>,
        pad: &GamepadInput,
        seconds: f64,
        now: Instant,
    ) -> (bool, bool) {
        let Some(active) = self.active else {
            return (false, true);
        };
        let rect = ui.max_rect();
        let response = ui.allocate_rect(rect, egui::Sense::click_and_drag());
        let ctx = ui.ctx().clone();
        let ppp = ctx.pixels_per_point();
        let size = pixel_size(rect, ppp);
        let pointer = response.hover_pos().map(|pos| {
            [
                f64::from((pos.x - rect.min.x) * ppp),
                f64::from((pos.y - rect.min.y) * ppp),
            ]
        });
        let (zoom, scroll) = ui.input(|i| (i.zoom_delta(), i.smooth_scroll_delta));
        let maps: Vec<PageMap> = self.tabs[active]
            .visible_draws(size)
            .iter()
            .map(|draw| PageMap::new(draw, rect, ppp))
            .collect();
        let mut actions = actions;
        let autoscroll = self.text_input(&ctx, &response, &maps, active, rect, &mut actions);
        let drag = if self.drag == Some(Drag::Pan) {
            response.drag_delta()
        } else {
            egui::Vec2::ZERO
        };
        let ppp64 = f64::from(ppp);
        let input = ViewInput {
            zoom: if pointer.is_some() {
                f64::from(zoom)
            } else {
                1.0
            },
            scroll: [
                -f64::from(drag.x) * ppp64
                    - if pointer.is_some() {
                        f64::from(scroll.x) * ppp64
                    } else {
                        0.0
                    },
                -f64::from(drag.y) * ppp64
                    - if pointer.is_some() {
                        f64::from(scroll.y) * ppp64
                    } else {
                        0.0
                    }
                    + autoscroll * ppp64,
            ],
            pointer,
            actions,
            gamepad: Some(pad),
            seconds,
            now: Some(now),
        };
        let turning = self.transition.is_some();
        self.tabs[active].pixels_per_point = f64::from(ppp);
        let tab_id = self.tabs[active].id;
        let mut frame = self.tabs[active].frame(size, input, turning);
        if let Some(direction) = frame.turn
            && let Some(effect) = self.prefs.transition.clone()
        {
            self.begin_transition(effect, tab_id, direction);
        }

        // Loading: ask for wanted pages, drop pages over the budget.
        let keep: Vec<PageKey> = frame
            .wanted
            .iter()
            .map(|&page| PageKey { tab: tab_id, page })
            .collect();
        let resident: std::collections::HashSet<usize> = frame
            .wanted
            .iter()
            .copied()
            .filter(|&page| self.is_resident(PageKey { tab: tab_id, page }))
            .collect();
        self.tabs[active].want(&frame.wanted, |page| resident.contains(&page));
        if let Some(dropped) = self.with_gpu(|gpu| gpu.evict(&keep, PAGE_BUDGET_BYTES)) {
            for key in dropped {
                if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == key.tab) {
                    tab.forget(key.page);
                }
            }
        }
        self.tabs[active].placeholders(&mut frame, |page| resident.contains(&page));

        let tokens = Tokens::get(&ctx);
        let background = {
            let id = self.prefs.background.clone();
            let def = self
                .with_gpu(|gpu| gpu.effects().find(&id).cloned())
                .flatten();
            let params = match (&def, id.as_str()) {
                (_, glyphst_effects::SOLID) => {
                    let [r, g, b] = tokens.pasteboard_rgb();
                    [r, g, b, 1.0]
                }
                (Some(def), _) => def.default_params(),
                (None, _) => [0.0; 4],
            };
            EffectUse { id, params }
        };
        let transition = self.transition.as_mut().map(|transition| {
            let params = self
                .render_state
                .renderer
                .write()
                .callback_resources
                .get::<ReaderGpu>()
                .and_then(|gpu| {
                    gpu.effects()
                        .find(&transition.effect)
                        .map(|def| def.default_params())
                })
                .unwrap_or_default();
            TransitionUse {
                effect: EffectUse {
                    id: transition.effect.clone(),
                    params,
                },
                progress: transition.progress(now),
                direction: transition.direction,
                capture_from: transition.take_capture(),
            }
        });
        let page_color = tokens.page.to_normalized_gamma_f32();
        ui.painter().add(egui_wgpu::Callback::new_paint_callback(
            rect,
            ViewCallback {
                rect,
                tab: tab_id,
                draws: frame.draws.clone(),
                page_color,
                background,
                transition,
                time: self.started.elapsed().as_secs_f32(),
                pointer: pointer.map(|[x, y]| [x as f32, y as f32]),
            },
        ));
        let painter = ui.painter_at(rect);
        let maps: Vec<PageMap> = frame
            .draws
            .iter()
            .map(|draw| PageMap::new(draw, rect, ppp))
            .collect();
        self.paint_text_overlays(&painter, &maps, active, &tokens);
        for (paper, thumbnail) in &frame.placeholders {
            let paper = egui::Rect::from_min_max(
                rect.min + egui::vec2(paper[0], paper[1]) / ppp,
                rect.min + egui::vec2(paper[2], paper[3]) / ppp,
            );
            if let Some(texture) = thumbnail {
                painter.image(
                    *texture,
                    paper,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    egui::Color32::WHITE,
                );
            }
        }
        if self.show_stats {
            self.stats_overlay(&painter, rect, active);
        }
        let tab = &mut self.tabs[active];
        if tab.find.open
            && let Some(action) = find::find_bar(&mut tab.find, rect, &ctx, now)
        {
            match action {
                FindBarAction::Step(forward) => self.find_step(forward),
                FindBarAction::Close => tab.find.close(),
            }
        }
        self.note_popover(&ctx);
        if self.tabs[active].is_reflowed() {
            self.reflow_to_view(active, rect.width(), now, &ctx);
        }
        (
            frame.animating || autoscroll != 0.0,
            frame.placeholders.is_empty(),
        )
    }

    /// Pointer input on page text: hover cursors, selecting by dragging,
    /// clicking links and highlights, double and triple clicks, and the
    /// context menu. Returns the autoscroll, in points per frame, while a
    /// selection is dragged past the view's edge.
    fn text_input(
        &mut self,
        ctx: &egui::Context,
        response: &egui::Response,
        maps: &[PageMap],
        active: usize,
        rect: egui::Rect,
        actions: &mut Vec<ViewAction>,
    ) -> f64 {
        let modifiers = ctx.input(|i| i.modifiers);
        let tab = &self.tabs[active];
        let reflowed = tab.is_reflowed();
        // What is under a screen position: the page map and page point.
        let under = |pos: egui::Pos2| page_at(maps, pos).map(|map| (*map, map.page_point(pos)));

        if let Some(pos) = response.hover_pos()
            && let Some((map, point)) = under(pos)
        {
            let link = tab.link_at(map.page, point).is_some();
            let text = tab.text.get(map.page).is_some_and(|text| {
                text.glyph_at(point[0], point[1], map.points(GLYPH_SLOP))
                    .is_some()
            });
            if matches!(self.drag, Some(Drag::Select { .. })) || (text && !link) {
                ctx.set_cursor_icon(egui::CursorIcon::Text);
            } else if link {
                ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
            }
        }

        if response.drag_started_by(egui::PointerButton::Primary) {
            self.drag = Some(Drag::Pan);
            let origin = ctx
                .input(|i| i.pointer.press_origin())
                .or(response.interact_pointer_pos());
            if let Some((map, point)) = origin.and_then(under)
                && let Some(text) = self.tabs[active].text.get(map.page).cloned()
            {
                let on_text = text
                    .glyph_at(point[0], point[1], map.points(GLYPH_SLOP))
                    .is_some();
                let span = if modifiers.alt {
                    Some(Span::Column {
                        from: point,
                        to: point,
                    })
                } else if on_text {
                    text.nearest(point[0], point[1]).map(|glyph| Span::Run {
                        anchor: glyph,
                        head: glyph,
                    })
                } else {
                    None
                };
                if let Some(span) = span {
                    self.drag = Some(Drag::Select { page: map.page });
                    self.tabs[active].selection = Some(Selection {
                        page: map.page,
                        span,
                    });
                }
            }
        }
        let mut autoscroll = 0.0;
        if let Some(Drag::Select { page }) = self.drag
            && let Some(pos) = response.interact_pointer_pos()
        {
            let tab = &mut self.tabs[active];
            if let Some(map) = maps.iter().find(|map| map.page == page)
                && let Some(text) = tab.text.get(page).cloned()
                && let Some(selection) = &mut tab.selection
            {
                let point = map.page_point(pos);
                match &mut selection.span {
                    Span::Run { head, .. } => {
                        if let Some(glyph) = text.nearest(point[0], point[1]) {
                            *head = glyph;
                        }
                    }
                    Span::Column { to, .. } => *to = point,
                }
            }
            if pos.y < rect.top() + 24.0 {
                autoscroll = -SELECT_SCROLL;
            } else if pos.y > rect.bottom() - 24.0 {
                autoscroll = SELECT_SCROLL;
            }
        }
        if response.drag_stopped() || !response.dragged() {
            self.drag = None;
        }

        let click = response.interact_pointer_pos().and_then(under);
        if response.clicked_by(egui::PointerButton::Primary) {
            let tab = &mut self.tabs[active];
            match click {
                Some((map, point)) => {
                    if let Some(target) = tab.link_at(map.page, point).cloned() {
                        match target {
                            LinkTarget::Page { page, y } => {
                                actions.push(ViewAction::GoTo { page, y });
                            }
                            LinkTarget::Uri(uri) => ctx.open_url(egui::OpenUrl::new_tab(uri)),
                        }
                    } else if let Some(id) = tab.annotations.hit(map.page, point, reflowed) {
                        let item = tab.annotations.get(id);
                        self.note_editor = item.map(|item| NoteEditor {
                            id,
                            at: response.interact_pointer_pos().unwrap_or_default(),
                            body: item.body.clone(),
                            color: item.color,
                            fresh: true,
                        });
                    } else {
                        tab.selection = None;
                    }
                }
                None => tab.selection = None,
            }
        }
        if let Some((map, point)) = click
            && (response.double_clicked() || response.triple_clicked())
        {
            let tab = &mut self.tabs[active];
            if let Some(text) = tab.text.get(map.page)
                && let Some(glyph) = text.glyph_at(point[0], point[1], map.points(GLYPH_SLOP))
            {
                let range = if response.triple_clicked() {
                    text.line_at(glyph)
                } else {
                    text.word_at(glyph)
                };
                tab.selection = range.map(|range| Selection::run(map.page, range));
            }
        }
        if response.secondary_clicked() {
            self.context_page = click.map(|(map, _)| map.page);
        }
        let menu_at = response.interact_pointer_pos().unwrap_or(rect.center());
        response.context_menu(|ui| self.context_menu(ui, ctx, active, menu_at, actions));
        autoscroll
    }

    fn context_menu(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        active: usize,
        at: egui::Pos2,
        actions: &mut Vec<ViewAction>,
    ) {
        let selected = self.tabs[active].selected_text();
        if let Some(text) = &selected {
            if ui.button("Copy  Ctrl+C").clicked() {
                ctx.copy_text(text.clone());
                ui.close();
            }
            ui.separator();
            ui.label("Highlight");
            ui.horizontal(|ui| {
                for color in HighlightColor::ALL {
                    let (rect, response) =
                        ui.allocate_exact_size(egui::vec2(20.0, 20.0), egui::Sense::click());
                    ui.painter()
                        .circle_filled(rect.center(), 8.0, annotations::color(color));
                    if response
                        .on_hover_text(annotations::color_name(color))
                        .clicked()
                    {
                        self.annotate_selection(color, false, at);
                        ui.close();
                    }
                }
            });
            if ui.button("Add note…").clicked() {
                self.annotate_selection(HighlightColor::Yellow, true, at);
                ui.close();
            }
            if text.chars().count() <= 64 && ui.button("Find in document").clicked() {
                let tab = &mut self.tabs[active];
                let page = tab.current_page();
                tab.find.show(page);
                tab.find.query = text.split_whitespace().collect::<Vec<_>>().join(" ");
                ui.close();
            }
            ui.separator();
        }
        if let Some(page) = self.context_page
            && ui.button("Select all on page  Ctrl+A").clicked()
        {
            self.tabs[active].select_page(page);
            ui.close();
        }
        if ui.button("Add bookmark  Ctrl+D").clicked() {
            self.add_bookmark();
            ui.close();
        }
        let tab = &self.tabs[active];
        if ui
            .add_enabled(tab.can_go_back(), egui::Button::new("Back  Alt+Left"))
            .clicked()
        {
            actions.push(ViewAction::Back);
            ui.close();
        }
        if ui
            .add_enabled(
                tab.can_go_forward(),
                egui::Button::new("Forward  Alt+Right"),
            )
            .clicked()
        {
            actions.push(ViewAction::Forward);
            ui.close();
        }
    }

    /// Library highlights, search matches and the selection over the pages.
    fn paint_text_overlays(
        &self,
        painter: &egui::Painter,
        maps: &[PageMap],
        active: usize,
        tokens: &Tokens,
    ) {
        let tab = &self.tabs[active];
        let reflowed = tab.is_reflowed();
        for map in maps {
            for (item, rects) in tab.annotations.on_page(map.page, reflowed) {
                fill_rects(
                    painter,
                    map,
                    rects.iter().copied(),
                    annotations::fill(item.color),
                );
                if let (AnnotationId::Note(_), Some(last)) = (item.id, rects.last()) {
                    let corner = map.screen_pos([last[2], last[1]]);
                    painter.circle_filled(corner, 4.0, annotations::color(item.color));
                }
            }
            let Some(text) = tab.text.get(map.page) else {
                continue;
            };
            for index in tab.find.on_page(map.page) {
                let found = &tab.find.matches[index];
                let tint = if tab.find.current == Some(index) {
                    find::CURRENT_MATCH_TINT
                } else {
                    find::MATCH_TINT
                };
                fill_rects(
                    painter,
                    map,
                    text.selection_rects(found.range.clone()),
                    tint,
                );
            }
            if let Some(selection) = &tab.selection
                && selection.page == map.page
            {
                fill_rects(
                    painter,
                    map,
                    selection.rects(text),
                    tokens.accent.gamma_multiply(0.35),
                );
            }
        }
    }

    /// The popover of a highlight or note clicked on the page: colour,
    /// note text, delete.
    fn note_popover(&mut self, ctx: &egui::Context) {
        let Some(editor) = &mut self.note_editor else {
            return;
        };
        let tokens = Tokens::get(ctx);
        let mut color = None;
        let mut save = false;
        let mut delete = false;
        let is_note = matches!(editor.id, AnnotationId::Note(_));
        let area = egui::Area::new(egui::Id::new("note-editor"))
            .order(egui::Order::Foreground)
            .fixed_pos(editor.at + egui::vec2(8.0, 8.0))
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(tokens.card)
                    .stroke(egui::Stroke::new(1.0, tokens.border))
                    .corner_radius(8)
                    .inner_margin(10)
                    .show(ui, |ui| {
                        ui.set_max_width(280.0);
                        ui.horizontal(|ui| {
                            for candidate in HighlightColor::ALL {
                                let (rect, response) = ui.allocate_exact_size(
                                    egui::vec2(20.0, 20.0),
                                    egui::Sense::click(),
                                );
                                let radius = if candidate == editor.color { 9.0 } else { 6.5 };
                                ui.painter().circle_filled(
                                    rect.center(),
                                    radius,
                                    annotations::color(candidate),
                                );
                                if response.clicked() {
                                    color = Some(candidate);
                                }
                            }
                        });
                        if is_note {
                            ui.add(
                                egui::TextEdit::multiline(&mut editor.body)
                                    .hint_text("Note")
                                    .desired_rows(3)
                                    .desired_width(260.0),
                            );
                        }
                        ui.horizontal(|ui| {
                            if is_note && ui.button("Save").clicked() {
                                save = true;
                            }
                            if ui.button("Delete").clicked() {
                                delete = true;
                            }
                        });
                    });
            });
        let outside = area.response.clicked_elsewhere() && !editor.fresh;
        editor.fresh = false;
        let (id, body) = (editor.id, editor.body.clone());
        if let Some(color) = color {
            editor.color = color;
            if is_note {
                let result = match id {
                    AnnotationId::Note(note) => self.library.update_note(note, body.clone(), color),
                    AnnotationId::Highlight(_) => Ok(false),
                };
                self.after_edit(result);
            } else {
                self.set_annotation_color(id, color);
            }
        }
        if delete {
            self.remove_annotation(id);
            self.note_editor = None;
        } else if save || outside {
            if is_note && body != self.annotation_body(id) {
                self.set_note_body(id, body);
            }
            self.note_editor = None;
        }
    }

    pub(crate) fn fit_scale(&self) -> Option<f64> {
        self.active.map(|active| self.tabs[active].fit_scale())
    }

    fn stats_overlay(&self, painter: &egui::Painter, rect: egui::Rect, active: usize) {
        let tab = &self.tabs[active];
        let stats = self.with_gpu(|gpu| gpu.stats.clone()).unwrap_or_default();
        let key = PageKey {
            tab: tab.id,
            page: tab.current_page(),
        };
        let source = self
            .with_gpu(|gpu| gpu.source(key, tab.view.scale as f32))
            .flatten()
            .unwrap_or_else(|| "loading".into());
        let text = format!(
            "frame p50 {:.1} ms  p95 {:.1} ms\nencode {:.2} ms  draws {}  segments {}\nresident {}  GPU {:.0} MiB  {source}\nrepaints {}",
            self.frames.percentile_ms(0.5),
            self.frames.percentile_ms(0.95),
            stats.encode_ms,
            stats.draw.draw_calls,
            stats.draw.segments,
            stats.resident,
            stats.gpu_bytes as f64 / 1048576.0,
            self.repaints,
        );
        let font = egui::FontId::monospace(11.0);
        let galley = painter.layout_no_wrap(text, font, egui::Color32::WHITE);
        let at = rect.right_bottom() - galley.size() - egui::vec2(12.0, 12.0);
        painter.rect_filled(
            egui::Rect::from_min_size(at, galley.size()).expand(6.0),
            6.0,
            egui::Color32::from_black_alpha(170),
        );
        painter.galley(at, galley, egui::Color32::WHITE);
    }
}

impl eframe::App for ReaderApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let now = Instant::now();
        let seconds = (now - self.last_update).as_secs_f64().min(0.1);
        self.last_update = now;
        self.repaints += 1;
        if !self.continuous {
            self.frames.idle();
        }
        self.frames.tick();
        self.apply_theme(&ctx);

        for path in self.pickers.take() {
            self.open(&path, &ctx, None);
        }
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|file| file.path().to_path_buf())
                .filter(|path| is_document(path))
                .collect()
        });
        for path in dropped {
            self.open(&path, &ctx, None);
        }
        for tab in &mut self.tabs {
            tab.receive(&ctx);
            tab.update_text(now);
        }
        self.upload_ready_page();
        self.upload_ready_layers();
        let pad = match &mut self.gamepads {
            Ok(pads) => pads.poll(),
            Err(_) => GamepadInput::default(),
        };
        let mut actions = self.global_actions(&ctx);
        actions.extend(pad.actions.iter().copied());
        actions.append(&mut self.pending_actions);
        if let Some(active) = self.active {
            let tab = &mut self.tabs[active];
            let found = tab.find.take_reveal().map(|m| (m.page, m.range.clone()));
            if let Some((page, range)) = found
                && let Some(rect) = tab
                    .text
                    .get(page)
                    .and_then(|text| text.selection_rects(range).first().copied())
            {
                actions.push(ViewAction::Reveal { page, rect });
            }
        }

        let tokens = Tokens::get(&ctx);
        egui::Panel::top("tabs")
            .exact_size(34.0)
            .frame(
                egui::Frame::new()
                    .fill(tokens.titlebar)
                    .inner_margin(egui::Margin::symmetric(6, 3)),
            )
            .show(ui, |ui| self.tab_strip(ui));
        if self.active.is_some() {
            egui::Panel::top("toolbar")
                .frame(
                    egui::Frame::new()
                        .fill(tokens.chrome)
                        .inner_margin(egui::Margin::symmetric(8, 4)),
                )
                .show(ui, |ui| self.toolbar(ui, &mut actions));
            if self.prefs.show_sidebar
                && let Some(active) = self.active
            {
                let mut requests = Vec::new();
                egui::Panel::left("sidebar")
                    .default_size(SIDEBAR_WIDTH)
                    .frame(egui::Frame::new().fill(tokens.chrome).inner_margin(8))
                    .show(ui, |ui| {
                        requests = sidebar::show(
                            ui,
                            &mut self.tabs[active],
                            &mut self.prefs.sidebar,
                            &mut self.sidebar_state,
                            now,
                        );
                    });
                for request in requests {
                    self.apply_sidebar(request, &mut actions);
                }
            }
        }
        if !self.notices.is_empty() {
            egui::Panel::bottom("notices").show(ui, |ui| {
                let mut dismiss = false;
                ui.horizontal(|ui| {
                    ui.label(self.notices.last().cloned().unwrap_or_default());
                    dismiss = ui.button("Dismiss").clicked();
                });
                if dismiss {
                    self.notices.clear();
                }
            });
        }

        let mut animating = false;
        let mut pages_drawn = true;
        egui::CentralPanel::no_frame()
            .frame(egui::Frame::new().fill(tokens.pasteboard))
            .show(ui, |ui| {
                if self.active.is_some() {
                    (animating, pages_drawn) = self.document_view(ui, actions, &pad, seconds, now);
                } else if let Some(action) =
                    self.home.ui(ui, &self.library, self.pickers.is_showing())
                {
                    match action {
                        HomeAction::Open(path) => self.open(&path, &ctx, None),
                        HomeAction::OpenDialog => self.pickers.open_documents(&ctx),
                    }
                }
            });

        self.run_harness(&ctx, pages_drawn, now);
        if self
            .transition
            .as_ref()
            .is_some_and(|transition| transition.finished(now))
        {
            self.transition = None;
        }
        if let Some(active) = self.active
            && now - self.last_position_save >= POSITION_SAVE_INTERVAL
        {
            self.save_position(active, true);
            self.last_position_save = now;
        }

        // A pending overview advances one tile per frame, so keep drawing.
        let overviews = self
            .with_gpu(|gpu| gpu.has_pending_overview())
            .unwrap_or(false);
        let continuous = animating
            || self.transition.is_some()
            || pad.is_active()
            || self.harness.is_some()
            || overviews;
        self.continuous = continuous;
        if continuous {
            ctx.request_repaint();
        } else if self.active.is_some() && self.background_is_animated() && ctx.input(|i| i.focused)
        {
            let fps = self.prefs.background_fps.max(1);
            ctx.request_repaint_after(Duration::from_secs_f64(1.0 / f64::from(fps)));
        }
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        if !self.persist {
            return;
        }
        for index in 0..self.tabs.len() {
            self.save_position(index, false);
        }
        let _ = self.library.save();
        self.prefs.last_session = if self.prefs.reopen_last_session {
            self.session()
        } else {
            LastSession::default()
        };
        self.prefs.save(storage);
    }
}

impl ReaderApp {
    fn background_is_animated(&self) -> bool {
        let id = self.prefs.background.clone();
        self.with_gpu(|gpu| gpu.effects().find(&id).is_some_and(|def| def.animated))
            .unwrap_or(false)
    }
}

fn truncate(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let mut short: String = text.chars().take(limit - 1).collect();
    short.push('…');
    short
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_tab_titles_are_shortened() {
        assert_eq!(truncate("short", 8), "short");
        assert_eq!(truncate("a rather long title", 8), "a rathe…");
        assert_eq!(truncate("中文标题很长很长很长", 4), "中文标…");
    }
}
