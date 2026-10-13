//! The reader window: tabs (home and documents), chrome, the document view
//! with its effects, preferences and reading positions.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use eframe::egui_wgpu;
use glyphst_core::{DocumentFormat, DocumentLayout, EpubLayout};
use glyphst_effects::EffectDef;
use glyphst_library::{DocumentPosition, HighlightColor, LibraryStore, SemanticTextRange};
use glyphst_pdf::text::LinkTarget;

use crate::{
    ai::{AskContext, Assistant, PanelAction},
    annotations::{self, AnnotationId, Annotations},
    commands::{Command, Needs},
    document::FrameTimes,
    document_tab::{DocumentTab, ViewAction, ViewInput},
    find::{self, FindBarAction},
    gamepad::{GamepadInput, Gamepads},
    gpu::{
        EffectUse, PAGE_BUDGET_BYTES, PageKey, ReaderGpu, TransitionUse, UPLOAD_BYTES_PER_FRAME,
        ViewCallback, pixel_size,
    },
    harness::Harness,
    home::{Home, HomeAction},
    icons,
    keys::Keymap,
    palette::{self, Item, Palette},
    pickers::{Pickers, is_document},
    prefs::{LastSession, Prefs, SessionFile},
    settings_window::{Section, SettingsContext, SettingsWindow},
    sidebar::{self, SidebarAction, SidebarState, SidebarTab, TranslationInfo},
    text_overlay::{PageMap, Selection, Span, fill_rects, page_at},
    theme::{self, ThemeKind, ThemePreference, Tokens},
    transition::ViewTransition,
    translation::{CardState, Translator},
    user_settings::UserSettings,
    widgets,
};

mod chrome;

/// Positions are written to the library at most this often while reading.
const POSITION_SAVE_INTERVAL: Duration = Duration::from_secs(1);
const TAB_MIN_WIDTH: f32 = 90.0;
const TAB_MAX_LABEL: usize = 32;
const TAB_OVERFLOW_WIDTH: f32 = 28.0;
/// Room a tab needs besides its title: icon, padding and close button.
const TAB_CHROME: f32 = 62.0;
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
    pub theme: Option<ThemePreference>,
    /// Keeps the dock and the rail shown (`true`) or faded (`false`)
    /// instead of fading them after a pause.
    pub controls: Option<bool>,
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
    /// Translate through a local fake provider instead of the settings'.
    pub translate_fake: bool,
    /// Scripted runs: translate the first document, or the selection.
    pub translate_document: bool,
    pub translate_selection: bool,
    /// Ask through a local fake model; scripted runs ask this at startup.
    pub ai_fake: bool,
    pub ask: Option<String>,
    /// Scripted runs: open the palette with this query, or settings at
    /// this section.
    pub palette: Option<String>,
    pub settings: Option<Section>,
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
    notices: crate::widgets::Toasts,
    show_stats: bool,
    sidebar_state: SidebarState,
    drag: Option<Drag>,
    note_editor: Option<NoteEditor>,
    /// The page a context menu was opened over.
    context_page: Option<usize>,
    pub(crate) launch_find: Option<String>,
    pub(crate) launch_select: Option<usize>,
    pub(crate) launch_translate_document: bool,
    pub(crate) launch_translate_selection: bool,
    font_size: Option<f32>,
    pub(crate) translator: Translator,
    pub(crate) user_settings: UserSettings,
    pub(crate) assistant: Assistant,
    pub(crate) launch_ask: Option<String>,
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
    chrome: chrome::StageChrome,
    /// See [`Launch::controls`].
    controls: Option<bool>,
    /// Present mode was on last frame (to say how to leave it).
    was_presenting: bool,
    pub(crate) keymap: Keymap,
    pub(crate) palette: Palette,
    pub(crate) settings_window: SettingsWindow,
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
        egui_extras::install_image_loaders(&cc.egui_ctx);
        let persist = launch.harness.is_none();
        let mut prefs = Prefs::load(cc.storage);
        if let Some(background) = launch.background {
            prefs.background = background;
        }
        if let Some(theme) = launch.theme {
            prefs.theme = theme;
        }
        if let Some(transition) = launch.transition {
            prefs.transition = (transition != "none").then_some(transition);
        }
        if let Some(tab) = launch.sidebar {
            prefs.sidebar = tab;
            prefs.show_sidebar = true;
        }
        // Scripted runs and fakes keep settings and keys in memory, so fake
        // providers never reach the user's config or keychain.
        let user_settings = if launch.translate_fake || launch.ai_fake || !persist {
            let mut settings = glyphst_settings::Settings::default();
            if launch.translate_fake {
                let port = crate::fake_providers::start_translation_server()?;
                crate::fake_providers::use_fake_translation(&mut settings, port);
            }
            if launch.ai_fake {
                let port = crate::fake_providers::start_ai_server()?;
                crate::fake_providers::use_fake_ai(&mut settings, port);
            }
            UserSettings::in_memory(&cc.egui_ctx, settings, Default::default())
        } else {
            UserSettings::load(&cc.egui_ctx)
        };
        let translator = Translator::new(persist, false);
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
            notices: Default::default(),
            show_stats: launch.harness.is_some(),
            sidebar_state: SidebarState::default(),
            drag: None,
            note_editor: None,
            context_page: None,
            launch_find: launch.find,
            launch_select: launch.select_page,
            launch_translate_document: launch.translate_document,
            launch_translate_selection: launch.translate_selection,
            font_size: launch.font_size,
            translator,
            user_settings,
            assistant: Assistant::default(),
            launch_ask: launch.ask,
            page_input: String::new(),
            saved_positions: HashMap::new(),
            last_position_save: Instant::now(),
            persist,
            harness: launch.harness,
            first_page_ms: None,
            upload_ms: Vec::new(),
            pending_actions: Vec::new(),
            chrome: Default::default(),
            // Scripted runs show the controls unless told otherwise.
            was_presenting: false,
            controls: launch.controls.or((!persist).then_some(true)),
            keymap: Keymap::default(),
            palette: Palette::default(),
            settings_window: SettingsWindow::default(),
        };
        app.keymap = Keymap::new(&app.prefs.shortcuts);
        if let Some(query) = launch.palette {
            app.palette.toggle();
            app.palette.query = query;
        }
        if let Some(section) = launch.settings {
            app.settings_window.show_section(section);
            // Scripted runs show the fake model's list.
            if section == Section::Ai
                && launch.ai_fake
                && let Some(settings) = app.user_settings.get().cloned()
            {
                let provider = settings.ai.provider_order[0];
                app.settings_window
                    .fetch_models(&settings, provider, &cc.egui_ctx);
            }
        }
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
                    .push("The book's text is still being read; try again in a moment.");
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

    fn apply_sidebar(
        &mut self,
        action: SidebarAction,
        actions: &mut Vec<ViewAction>,
        ctx: &egui::Context,
    ) {
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
            SidebarAction::StartTranslation => self.start_document_translation(ctx),
            SidebarAction::CancelTranslation => {
                if let Some(session) = self
                    .active
                    .and_then(|active| self.tabs[active].translation.as_ref())
                {
                    session.cancel();
                }
            }
            SidebarAction::ExportTranslation => {
                if let Some(active) = self.active {
                    let tab = &self.tabs[active];
                    let stem = tab
                        .path
                        .file_stem()
                        .map(|stem| stem.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "document".into());
                    let target = tab
                        .translation
                        .as_ref()
                        .map(|session| session.target.clone())
                        .unwrap_or_default();
                    let name = format!("{stem}.{target}.pdf");
                    self.pickers.save_pdf(ctx, &name, tab.id);
                }
            }
            SidebarAction::ShowTranslated(shown) => {
                if let Some(session) = self
                    .active
                    .and_then(|active| self.tabs[active].translation.as_mut())
                {
                    session.show_translated = shown;
                }
            }
            SidebarAction::Unpin(index) => {
                if index < self.translator.pinned.len() {
                    self.translator.pinned.remove(index);
                }
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

    /// Translates the selection into a card at `at`.
    pub(crate) fn translate_selection(&mut self, at: egui::Pos2, ctx: &egui::Context) {
        let Some(text) = self
            .active
            .and_then(|active| self.tabs[active].selected_text())
        else {
            return;
        };
        let target = self.prefs.target_language.clone();
        self.translator
            .translate_selection(text, target.as_deref(), at, ctx);
    }

    /// Starts translating the active document.
    pub(crate) fn start_document_translation(&mut self, ctx: &egui::Context) {
        let Some(active) = self.active else {
            return;
        };
        let target = self.prefs.target_language.clone();
        let started =
            self.translator
                .start_session(&self.tabs[active].path, target.as_deref(), ctx);
        match started {
            Ok(session) => {
                self.tabs[active].translation = Some(session);
                self.prefs.show_sidebar = true;
                self.prefs.sidebar = SidebarTab::Translation;
            }
            Err(error) => self.notices.push(format!("Could not translate: {error}")),
        }
    }

    /// The card of a selection translation: the translation, Copy and Pin.
    fn translation_card(&mut self, ctx: &egui::Context) {
        let Some(card) = self.translator.card.clone() else {
            return;
        };
        let tokens = Tokens::get(ctx);
        let (mut close, mut pin) = (false, false);
        let area = egui::Area::new(egui::Id::new("translation-card"))
            .order(egui::Order::Foreground)
            .fixed_pos(card.at + egui::vec2(8.0, 8.0))
            .show(ctx, |ui| {
                widgets::card_frame(ctx).show(ui, |ui| {
                    ui.set_max_width(360.0);
                    ui.horizontal(|ui| {
                        ui.add(icons::image("languages", 15.0, tokens.accent));
                        ui.label(egui::RichText::new("Translation").font(theme::semibold(13.0)));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            close |= icons::button(ui, "x", 24.0, false, "Close").clicked();
                        });
                    });
                    let source: String = card.source.chars().take(160).collect();
                    ui.label(
                        egui::RichText::new(source)
                            .italics()
                            .color(tokens.text_muted),
                    );
                    ui.add_space(4.0);
                    match &card.state {
                        CardState::Loading => {
                            ui.horizontal(|ui| {
                                ui.spinner();
                                ui.label("Translating…");
                            });
                        }
                        CardState::Done(text) => {
                            egui::ScrollArea::vertical()
                                .max_height(320.0)
                                .show(ui, |ui| ui.label(text));
                            ui.add_space(6.0);
                            ui.horizontal(|ui| {
                                if widgets::pill_button(ui, Some("copy"), "Copy", false).clicked() {
                                    ui.ctx().copy_text(text.clone());
                                }
                                pin = widgets::pill_button(ui, Some("pin"), "Pin", false)
                                    .on_hover_text("Keep in the Translation panel")
                                    .clicked();
                            });
                        }
                        CardState::Failed(error) => {
                            ui.label(egui::RichText::new(error).color(tokens.danger));
                        }
                    }
                });
            });
        if pin {
            self.translator.pin_card();
            self.prefs.show_sidebar = true;
            self.prefs.sidebar = SidebarTab::Translation;
            close = true;
        }
        let fresh = ctx.input(|i| i.pointer.any_pressed())
            && area.response.rect.contains(card.at + egui::vec2(9.0, 9.0));
        if close
            || (area.response.clicked_elsewhere()
                && !fresh
                && !matches!(card.state, CardState::Loading))
        {
            self.translator.card = None;
        }
    }

    /// Shows or hides the assistant.
    fn toggle_assistant(&mut self) {
        self.assistant.open = !self.assistant.open;
        if self.assistant.open {
            self.assistant.refresh_sessions(&self.library);
        }
    }

    /// Asks the assistant about the active document.
    pub(crate) fn ask(&mut self, text: String, ctx: &egui::Context) {
        let Some(active) = self.active else {
            return;
        };
        let Some(settings) = self.user_settings.get().cloned() else {
            self.assistant.error =
                Some("Your AI settings are still loading; try again in a moment.".into());
            return;
        };
        let tab = &self.tabs[active];
        let current_page = tab.current_page();
        let ask = AskContext {
            path: tab.path.clone(),
            title: tab.title.clone(),
            layout: tab.document_layout,
            page_count: tab.layout.len(),
            current_page,
            page_text: tab.text.get(current_page).map(|text| text.plain_text()),
            selection: tab.selected_text(),
        };
        self.assistant.send(text, ask, &settings, ctx);
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

    /// Queues raster layers re-rasterized for the current view; they are
    /// written after the uploads before them, a chunk per frame.
    fn upload_ready_layers(&mut self) {
        let device = self.render_state.device.clone();
        for index in 0..self.tabs.len() {
            let tab_id = self.tabs[index].id;
            while let Some((page, layer, image)) = self.tabs[index].ready_layers.pop_front() {
                let key = PageKey { tab: tab_id, page };
                self.with_gpu(|gpu| gpu.replace_layer(&device, key, layer, image));
            }
        }
    }

    /// Writes the current upload a chunk further, starting the next loaded
    /// page (the active tab's first) when no page is being uploaded, so a
    /// heavy page costs a few ordinary frames rather than one long one.
    fn upload_ready_page(&mut self) {
        let (device, queue) = (
            self.render_state.device.clone(),
            self.render_state.queue.clone(),
        );
        self.with_gpu(|gpu| gpu.step_overview(&device, &queue));
        if !self.with_gpu(|gpu| gpu.is_uploading()).unwrap_or(false) {
            let order: Vec<usize> = self.active.into_iter().chain(0..self.tabs.len()).collect();
            let Some((key, ready)) = order.into_iter().find_map(|index| {
                let ready = self.tabs[index].ready.pop_front()?;
                let key = PageKey {
                    tab: self.tabs[index].id,
                    page: ready.page,
                };
                Some((key, ready))
            }) else {
                return;
            };
            self.with_gpu(|gpu| gpu.begin_upload(&device, key, ready.prepared, ready.layers));
        }
        if let Some(Some(ms)) =
            self.with_gpu(|gpu| gpu.step_upload(&device, &queue, UPLOAD_BYTES_PER_FRAME))
        {
            self.upload_ms.push(ms);
            if self.first_page_ms.is_none() {
                self.first_page_ms = Some(self.started.elapsed().as_secs_f64() * 1000.0);
            }
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
        let mut actions = Vec::new();
        if self.active.is_some() {
            let (back, forward) = ctx.input(|i| {
                (
                    i.pointer.button_pressed(egui::PointerButton::Extra1),
                    i.pointer.button_pressed(egui::PointerButton::Extra2),
                )
            });
            if back {
                actions.push(ViewAction::Back);
            }
            if forward {
                actions.push(ViewAction::Forward);
            }
        }
        // Key presses go to a shortcut being recorded, and the settings
        // window stands in front of the document.
        if self.settings_window.open {
            return actions;
        }
        let typing = self.palette.open || ctx.memory(|memory| memory.focused().is_some());
        for command in self.keymap.take_pressed(ctx, typing) {
            if let Err(reason) = self.run(command, ctx)
                && self.active.is_some()
                && command.needs() != Needs::Document
            {
                self.notices.push(reason.to_string());
            }
        }
        if let Some(active) = self.active
            && !typing
        {
            let copy = ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Copy)));
            if copy {
                self.copy_selection(ctx);
            }
            if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                let tab = &mut self.tabs[active];
                if self.note_editor.is_some() {
                    self.note_editor = None;
                } else if tab.selection.is_some() {
                    tab.selection = None;
                } else if tab.find.open {
                    tab.find.close();
                } else if tab.present.is_some() {
                    actions.push(ViewAction::TogglePresent);
                }
            }
        }
        actions
    }

    /// Why `command` can't run now, if it can't.
    pub(crate) fn command_blocked(&self, command: Command) -> Option<&'static str> {
        let Some(tab) = self.active.map(|active| &self.tabs[active]) else {
            return (command.needs() != Needs::Nothing).then(|| Needs::Document.reason());
        };
        let can = match command.needs() {
            Needs::Nothing | Needs::Document => true,
            Needs::Selection => tab.selection.is_some(),
            Needs::Reflow => matches!(tab.document_layout, DocumentLayout::Reflow(_)),
            Needs::Translation => tab.translation.is_some(),
        };
        (!can).then(|| command.needs().reason())
    }

    /// Runs a command: keys, the palette, menus and buttons all come here.
    pub(crate) fn run(
        &mut self,
        command: Command,
        ctx: &egui::Context,
    ) -> Result<(), &'static str> {
        if let Some(reason) = self.command_blocked(command) {
            return Err(reason);
        }
        let view = |action| Some(action);
        let action = match command {
            Command::Open => {
                self.pickers.open_documents(ctx);
                None
            }
            Command::CloseTab => {
                if let Some(active) = self.active {
                    self.close(active);
                }
                None
            }
            Command::NextTab | Command::PreviousTab => {
                let count = self.tabs.len() + 1;
                let current = self.active.map_or(0, |active| active + 1);
                let next = if command == Command::PreviousTab {
                    (current + count - 1) % count
                } else {
                    (current + 1) % count
                };
                self.activate(next.checked_sub(1));
                None
            }
            Command::Palette => {
                self.palette.toggle();
                None
            }
            Command::Settings => {
                self.settings_window.open = true;
                None
            }
            Command::ToggleSidebar => {
                self.prefs.show_sidebar = !self.prefs.show_sidebar;
                None
            }
            Command::ToggleStats => {
                self.show_stats = !self.show_stats;
                None
            }
            Command::ThemeSystem => {
                self.prefs.theme = ThemePreference::System;
                None
            }
            Command::ThemeLight => {
                self.prefs.theme = ThemePreference::Light;
                None
            }
            Command::ThemeDark => {
                self.prefs.theme = ThemePreference::Dark;
                None
            }
            Command::NextPage => view(ViewAction::NextPage),
            Command::PreviousPage => view(ViewAction::PreviousPage),
            Command::FirstPage => view(ViewAction::GoToPage(0)),
            Command::LastPage => view(ViewAction::GoToPage(usize::MAX)),
            Command::Back => view(ViewAction::Back),
            Command::Forward => view(ViewAction::Forward),
            Command::ZoomIn => view(ViewAction::ZoomIn),
            Command::ZoomOut => view(ViewAction::ZoomOut),
            Command::FitWidth => view(ViewAction::FitWidth),
            Command::Present => view(ViewAction::TogglePresent),
            Command::LargerText | Command::SmallerText => {
                let step = if command == Command::LargerText {
                    1.0
                } else {
                    -1.0
                };
                self.change_font_size(step);
                None
            }
            Command::Find => {
                if let Some(active) = self.active {
                    let tab = &mut self.tabs[active];
                    let page = tab.current_page();
                    tab.find.show(page);
                }
                None
            }
            Command::FindNext | Command::FindPrevious => {
                self.find_step(command == Command::FindNext);
                None
            }
            Command::SelectPage => {
                if let Some(active) = self.active {
                    let tab = &mut self.tabs[active];
                    let page = tab.current_page();
                    tab.select_page(page);
                }
                None
            }
            Command::Bookmark => {
                self.add_bookmark();
                None
            }
            Command::TranslateSelection => {
                let at = ctx
                    .pointer_latest_pos()
                    .unwrap_or_else(|| ctx.content_rect().center());
                self.translate_selection(at, ctx);
                None
            }
            Command::TranslateDocument => {
                self.start_document_translation(ctx);
                None
            }
            Command::ShowTranslated => {
                if let Some(session) = self
                    .active
                    .and_then(|active| self.tabs[active].translation.as_mut())
                {
                    session.show_translated = !session.show_translated;
                }
                None
            }
            Command::Assistant => {
                self.toggle_assistant();
                None
            }
        };
        if let Some(action) = action {
            self.pending_actions.push(action);
        }
        ctx.request_repaint();
        Ok(())
    }

    /// Runs a command, telling the user when it can't run.
    pub(crate) fn run_or_notify(&mut self, command: Command, ctx: &egui::Context) {
        if let Err(reason) = self.run(command, ctx) {
            self.notices.push(reason.to_string());
        }
    }

    /// A menu entry for `command`: its icon, `text` and its keys.
    fn menu_button(&self, ui: &egui::Ui, text: &str, command: Command) -> egui::Button<'static> {
        menu_entry(
            ui,
            command.icon(),
            text,
            &self.keymap.label(command).unwrap_or_default(),
        )
    }

    /// The palette and the settings window, drawn over everything.
    fn overlays(&mut self, ctx: &egui::Context) {
        if self.palette.open {
            let sources = palette::Sources {
                commands: Command::ALL
                    .into_iter()
                    .map(|command| {
                        (
                            command,
                            self.keymap.label(command),
                            self.command_blocked(command),
                        )
                    })
                    .collect(),
                tabs: self.tabs.iter().map(|tab| tab.title.clone()).collect(),
                recent: self.recent_documents(),
                pages: self.active.map(|active| self.tabs[active].layout.len()),
            };
            let hits = palette::hits(&self.palette.query, &sources, self.palette.used());
            match self.palette.show(ctx, &hits) {
                Some(Item::Command(command)) => self.run_or_notify(command, ctx),
                Some(Item::Tab(index)) => self.activate(Some(index)),
                Some(Item::Open(path)) => self.open(&path, ctx, None),
                Some(Item::GoToPage(page)) => self.pending_actions.push(ViewAction::GoToPage(page)),
                Some(Item::Settings(section)) => self.settings_window.show_section(section),
                None => {}
            }
        }
        if self.settings_window.open {
            let effects: Vec<EffectDef> = self
                .with_gpu(|gpu| gpu.effects().defs().cloned().collect())
                .unwrap_or_default();
            let changed = self.settings_window.show(
                ctx,
                SettingsContext {
                    prefs: &mut self.prefs,
                    keymap: &self.keymap,
                    effects: &effects,
                    user: &mut self.user_settings,
                },
            );
            if changed {
                self.keymap = Keymap::new(&self.prefs.shortcuts);
            }
        }
    }

    /// Recently opened documents that still exist, latest first.
    fn recent_documents(&self) -> Vec<PathBuf> {
        let mut entries: Vec<_> = self.library.history_entries().iter().collect();
        entries.sort_by_key(|entry| std::cmp::Reverse(entry.opened));
        entries
            .into_iter()
            .filter(|entry| self.tabs.iter().all(|tab| tab.path != entry.path))
            .map(|entry| entry.path.clone())
            .filter(|path| path.exists())
            .take(20)
            .collect()
    }

    /// The top bar, after pdfcraft's tab strip: Home, the document tabs,
    /// Open, then the palette's search box, theme, settings and the menu.
    fn tab_strip(&mut self, ui: &mut egui::Ui) {
        let tokens = Tokens::get(ui.ctx());
        let ctx = ui.ctx().clone();
        let font = theme::regular(13.0);
        let labels: Vec<String> = self
            .tabs
            .iter()
            .map(|tab| truncate(&tab.title, TAB_MAX_LABEL))
            .collect();
        let natural: Vec<f32> = labels
            .iter()
            .map(|label| {
                ui.painter()
                    .layout_no_wrap(label.clone(), font.clone(), tokens.text)
                    .size()
                    .x
                    + TAB_CHROME
            })
            .collect();
        let strip = ui.available_rect_before_wrap();
        let search_width = if strip.width() > 900.0 { 280.0 } else { 0.0 };
        // Home and Open on the left; search, theme, settings and the menu
        // on the right.
        let controls = 2.0 * 32.0 + search_width + 4.0 * 32.0 + 16.0;
        let fit = craft_ui::tabs::fit(
            &natural,
            self.active.unwrap_or(0),
            (strip.width() - controls).max(0.0),
            TAB_MIN_WIDTH,
            TAB_OVERFLOW_WIDTH,
        );
        let mut select = None;
        let mut close = None;
        let mut run = None;
        ui.horizontal_centered(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            if icons::button(ui, "house", 28.0, self.active.is_none(), "Home").clicked() {
                select = Some(None);
            }
            for &(index, width) in &fit.shown {
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(width, 30.0), egui::Sense::hover());
                let id = ui.id().with(("tab", self.tabs[index].id));
                let is_selected = self.active == Some(index);
                let label = &labels[index];
                let icon = if self.tabs[index].is_reflowed() {
                    "book-open"
                } else {
                    "file-text"
                };
                let response = craft_ui::tabs::Tab::new(id, label, is_selected)
                    .focus_stroke(egui::Stroke::new(1.0, tokens.accent))
                    .show_at(ui, rect, |ui, response| {
                        if is_selected {
                            ui.painter().rect(
                                rect,
                                8.0,
                                tokens.chrome,
                                egui::Stroke::new(1.0, tokens.border),
                                egui::StrokeKind::Inside,
                            );
                        } else if response.hovered() {
                            ui.painter().rect_filled(rect, 8.0, tokens.hover);
                        }
                        icons::paint(
                            ui,
                            egui::Rect::from_center_size(
                                egui::pos2(rect.left() + 16.0, rect.center().y),
                                egui::vec2(16.0, 16.0),
                            ),
                            icon,
                            15.0,
                            if is_selected {
                                tokens.accent
                            } else {
                                tokens.text_muted
                            },
                        );
                        widgets::row_text(
                            ui,
                            egui::pos2(rect.left() + 30.0, rect.center().y),
                            label.as_str(),
                            font.clone(),
                            if is_selected {
                                tokens.text
                            } else {
                                tokens.text_muted
                            },
                            rect.width() - TAB_CHROME,
                        );
                    })
                    .on_hover_text(&self.tabs[index].title);
                if response.clicked() {
                    select = Some(Some(index));
                }
                if response.middle_clicked() {
                    close = Some(index);
                }
                let close_rect = egui::Rect::from_center_size(
                    egui::pos2(rect.right() - 15.0, rect.center().y),
                    egui::vec2(20.0, 20.0),
                );
                let close_button = ui.interact(close_rect, id.with("close"), egui::Sense::click());
                if close_button.hovered() {
                    ui.painter().rect_filled(close_rect, 4.0, tokens.pressed);
                }
                if is_selected || response.hovered() || close_button.hovered() {
                    icons::paint(ui, close_rect, "x", 13.0, tokens.text_muted);
                }
                if close_button.on_hover_text("Close").clicked() {
                    close = Some(index);
                }
            }
            if !fit.overflow.is_empty() {
                let response =
                    icons::button(ui, "chevron-down", TAB_OVERFLOW_WIDTH, false, "More tabs");
                egui::Popup::menu(&response).show(|ui| {
                    for &index in &fit.overflow {
                        if ui.button(&labels[index]).clicked() {
                            select = Some(Some(index));
                        }
                    }
                });
            }
            let tip = self.keymap.tip("Open documents", Command::Open);
            if ui
                .add_enabled_ui(!self.pickers.is_showing(), |ui| {
                    icons::button(ui, "plus", 28.0, false, &tip)
                })
                .inner
                .clicked()
            {
                run = Some(Command::Open);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let response = icons::button(ui, "ellipsis", 28.0, false, "Menu");
                egui::Popup::menu(&response).show(|ui| self.menu(ui));
                let tip = self.keymap.tip("Settings", Command::Settings);
                if icons::button(ui, "settings", 28.0, self.settings_window.open, &tip).clicked() {
                    run = Some(Command::Settings);
                }
                let (icon, label) = match self.prefs.theme {
                    ThemePreference::System => ("monitor", "System"),
                    ThemePreference::Light => ("sun", "Light"),
                    ThemePreference::Dark => ("moon", "Dark"),
                };
                let response = icons::button(ui, icon, 28.0, false, &format!("Theme: {label}"));
                egui::Popup::menu(&response).show(|ui| {
                    for (command, preference) in [
                        (Command::ThemeSystem, ThemePreference::System),
                        (Command::ThemeLight, ThemePreference::Light),
                        (Command::ThemeDark, ThemePreference::Dark),
                    ] {
                        if ui
                            .add(egui::Button::selectable(
                                self.prefs.theme == preference,
                                preference.label(),
                            ))
                            .clicked()
                        {
                            run = Some(command);
                        }
                    }
                });
                let key = self.keymap.label(Command::Palette).unwrap_or_default();
                if search_width > 0.0 {
                    if widgets::search_box(ui, "Search commands, pages, books…", &key, search_width)
                        .clicked()
                    {
                        run = Some(Command::Palette);
                    }
                } else {
                    let tip = self.keymap.tip("Command palette", Command::Palette);
                    if icons::button(ui, "search", 28.0, self.palette.open, &tip).clicked() {
                        run = Some(Command::Palette);
                    }
                }
            });
        });
        if let Some(command) = run {
            self.run_or_notify(command, &ctx);
        }
        if let Some(index) = close {
            self.close(index);
        } else if let Some(index) = select {
            self.activate(index);
        }
    }

    fn menu(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let mut run = None;
        for command in [Command::Open, Command::Palette, Command::Settings] {
            let enabled = command != Command::Open || !self.pickers.is_showing();
            if ui
                .add_enabled(enabled, self.menu_button(ui, command.label(), command))
                .clicked()
            {
                run = Some(command);
                ui.close();
            }
        }
        ui.separator();
        let sidebar = self
            .keymap
            .label(Command::ToggleSidebar)
            .unwrap_or_default();
        ui.checkbox(&mut self.prefs.show_sidebar, format!("Sidebar  {sidebar}"));
        let stats = self.keymap.label(Command::ToggleStats).unwrap_or_default();
        ui.checkbox(&mut self.show_stats, format!("Statistics  {stats}"));
        if let Some(command) = run {
            self.run_or_notify(command, &ctx);
        }
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

    /// The document view: input, layout, loading, the GPU callback and
    /// loading placeholders. Returns whether it is animating and whether
    /// every visible page is drawn.
    fn document_view(
        &mut self,
        ui: &mut egui::Ui,
        rect: egui::Rect,
        actions: Vec<ViewAction>,
        pad: &GamepadInput,
        seconds: f64,
        now: Instant,
    ) -> (bool, bool) {
        let Some(active) = self.active else {
            return (false, true);
        };
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
        // A page being uploaded keeps its placeholder until it can be drawn.
        let drawable: std::collections::HashSet<usize> = frame
            .wanted
            .iter()
            .copied()
            .filter(|&page| {
                self.with_gpu(|gpu| gpu.is_drawable(PageKey { tab: tab_id, page }))
                    .unwrap_or(false)
            })
            .collect();
        self.tabs[active].placeholders(&mut frame, |page| drawable.contains(&page));

        let tokens = Tokens::get(&ctx);
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
        // Shadows under the pages, between the backdrop and the pages; a
        // transition's outgoing pages sit elsewhere, so none then.
        if transition.is_none() {
            let shadow = egui::Shadow {
                offset: [0, 3],
                blur: 18,
                spread: 0,
                color: tokens.page_shadow,
            };
            let painter = ui.painter_at(rect);
            for draw in &frame.draws {
                let [x0, y0, x1, y1] = draw.paper;
                let paper = egui::Rect::from_min_max(
                    rect.min + egui::vec2(x0, y0) / ppp,
                    rect.min + egui::vec2(x1, y1) / ppp,
                );
                painter.add(shadow.as_shape(paper, 2));
            }
        }
        ui.painter().add(egui_wgpu::Callback::new_paint_callback(
            rect,
            ViewCallback {
                rect,
                tab: tab_id,
                draws: frame.draws.clone(),
                page_color,
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
        self.translation_card(&ctx);
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

        // Over a translated block: its outline, and the original text.
        if self.drag.is_none()
            && let Some(pos) = response.hover_pos()
            && let Some((map, point)) = under(pos)
            && let Some((block, original)) = tab.original_at(map.page, point)
        {
            let painter = ctx
                .layer_painter(egui::LayerId::new(
                    egui::Order::Middle,
                    egui::Id::new("translation-hover"),
                ))
                .with_clip_rect(rect.intersect(map.paper));
            let tokens = Tokens::get(ctx);
            painter.rect_stroke(
                map.rect_to_screen(block).expand(2.0),
                3.0,
                egui::Stroke::new(1.5, tokens.accent),
                egui::StrokeKind::Outside,
            );
            response.clone().on_hover_ui_at_pointer(|ui| {
                ui.set_max_width(360.0);
                ui.label(
                    egui::RichText::new("Original")
                        .small()
                        .color(tokens.text_muted),
                );
                ui.label(original);
            });
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
        let translated = self.tabs[active]
            .selection
            .as_ref()
            .is_some_and(|selection| self.tabs[active].shows_translation(selection.page));
        if let Some(text) = &selected {
            if ui.add(menu_entry(ui, "copy", "Copy", "Ctrl+C")).clicked() {
                ctx.copy_text(text.clone());
                ui.close();
            }
            if ui
                .add(self.menu_button(ui, "Translate", Command::TranslateSelection))
                .clicked()
            {
                self.translate_selection(at, ctx);
                ui.close();
            }
        }
        if let Some(text) = selected.as_ref().filter(|_| !translated) {
            ui.separator();
            ui.label(egui::RichText::new("Highlight").color(Tokens::get(ctx).text_faint));
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
            if ui
                .add(menu_entry(ui, "sticky-note", "Add note…", ""))
                .clicked()
            {
                self.annotate_selection(HighlightColor::Yellow, true, at);
                ui.close();
            }
            if text.chars().count() <= 64
                && ui
                    .add(menu_entry(ui, "search", "Find in document", ""))
                    .clicked()
            {
                let tab = &mut self.tabs[active];
                let page = tab.current_page();
                tab.find.show(page);
                tab.find.query = text.split_whitespace().collect::<Vec<_>>().join(" ");
                ui.close();
            }
            ui.separator();
        }
        if let Some(page) = self.context_page
            && ui
                .add(self.menu_button(ui, "Select all on page", Command::SelectPage))
                .clicked()
        {
            self.tabs[active].select_page(page);
            ui.close();
        }
        if ui
            .add(self.menu_button(ui, "Add bookmark", Command::Bookmark))
            .clicked()
        {
            self.add_bookmark();
            ui.close();
        }
        let (back, forward) = (
            self.menu_button(ui, "Back", Command::Back),
            self.menu_button(ui, "Forward", Command::Forward),
        );
        let tab = &self.tabs[active];
        if ui.add_enabled(tab.can_go_back(), back).clicked() {
            actions.push(ViewAction::Back);
            ui.close();
        }
        if ui.add_enabled(tab.can_go_forward(), forward).clicked() {
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
            // Highlights belong to the original text.
            let annotations = if tab.shows_translation(map.page) {
                Vec::new()
            } else {
                tab.annotations.on_page(map.page, reflowed)
            };
            for (item, rects) in annotations {
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
                widgets::card_frame(ctx).show(ui, |ui| {
                    ui.set_max_width(280.0);
                    ui.horizontal(|ui| {
                        for candidate in HighlightColor::ALL {
                            let (rect, response) = ui
                                .allocate_exact_size(egui::vec2(22.0, 22.0), egui::Sense::click());
                            let chosen = candidate == editor.color;
                            if chosen {
                                ui.painter().circle_stroke(
                                    rect.center(),
                                    10.0,
                                    egui::Stroke::new(2.0, tokens.accent),
                                );
                            }
                            ui.painter().circle_filled(
                                rect.center(),
                                if chosen || response.hovered() {
                                    7.5
                                } else {
                                    7.0
                                },
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
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        if is_note
                            && widgets::pill_button(ui, Some("check"), "Save", true).clicked()
                        {
                            save = true;
                        }
                        if widgets::pill_button(ui, Some("trash-2"), "Delete", false).clicked() {
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
        let font = theme::mono(11.0);
        let galley = painter.layout_no_wrap(text, font, egui::Color32::WHITE);
        let at = rect.right_top() + egui::vec2(-20.0 - galley.size().x, 20.0);
        painter.rect_filled(
            egui::Rect::from_min_size(at, galley.size()).expand(10.0),
            10.0,
            egui::Color32::from_black_alpha(180),
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
        self.user_settings.poll(now);
        self.translator.set_settings(self.user_settings.get());
        self.translator.poll();
        self.assistant
            .poll(self.persist.then_some(&mut self.library));
        if let Some((tab_id, path)) = self.pickers.take_saved()
            && let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == tab_id)
            && let Some(session) = tab.translation.as_mut()
            && let Err(error) = self.translator.export(session, path, &ctx)
        {
            self.notices.push(format!("Could not export: {error}"));
        }
        for tab in &mut self.tabs {
            tab.receive(&ctx);
            tab.sync_translation();
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
        let presenting = self
            .active
            .is_some_and(|active| self.tabs[active].present.is_some());
        if presenting && !self.was_presenting {
            let key = self.keymap.label(Command::Present).unwrap_or_default();
            self.notices
                .push(format!("Presenting. Press Esc or {key} to leave."));
        }
        self.was_presenting = presenting;
        if !presenting {
            egui::Panel::top("tabs")
                .exact_size(40.0)
                .frame(
                    egui::Frame::new()
                        .fill(tokens.titlebar)
                        .stroke(egui::Stroke::new(1.0, tokens.divider))
                        .inner_margin(egui::Margin::symmetric(8, 0)),
                )
                .show(ui, |ui| self.tab_strip(ui));
        }

        let mut animating = false;
        let mut pages_drawn = true;
        egui::CentralPanel::no_frame()
            .frame(egui::Frame::new().fill(tokens.pasteboard))
            .show(ui, |ui| {
                if self.active.is_some() {
                    (animating, pages_drawn) = self.stage(ui, actions, &pad, seconds, now);
                } else if let Some(action) =
                    self.home.ui(ui, &self.library, self.pickers.is_showing())
                {
                    match action {
                        HomeAction::Open(path) => self.open(&path, &ctx, None),
                        HomeAction::OpenDialog => self.pickers.open_documents(&ctx),
                    }
                }
            });

        self.overlays(&ctx);
        // Above the dock while it shows.
        let lift = if self.active.is_some() && !presenting {
            84.0
        } else {
            28.0
        };
        self.notices.show(&ctx, lift);
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
        let overviews = self.with_gpu(|gpu| gpu.has_pending_work()).unwrap_or(false);
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

/// A menu entry with an icon, a label and a key at the right.
fn menu_entry(ui: &egui::Ui, icon: &str, text: &str, key: &str) -> egui::Button<'static> {
    let tokens = Tokens::get(ui.ctx());
    egui::Button::image_and_text(icons::image(icon, 15.0, tokens.icon), text.to_string())
        .shortcut_text(egui::RichText::new(key.to_string()).color(tokens.text_faint))
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
