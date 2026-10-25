//! The document sidebar, after pdfcraft's side panels (artcraft,
//! `ui-egui/src/panels.rs`, `comments_panel.rs`, `search_ui.rs`): outline,
//! page thumbnails, bookmarks, highlights and notes, and search results.

use std::{
    collections::{HashMap, HashSet},
    time::Instant,
};

use glyphst_library::HighlightColor;
use glyphst_pdf_translation_service::{ExportLayout, PageFailureCode};
use glyphst_settings::TranslationProvider;
use glyphst_ui::{
    Tip,
    button::Button,
    menu as ui_menu,
    meter::Meter,
    motif, motion,
    pane::{Action, Header},
    row::{self as ui_row, ListRow},
    segmented,
    toggle::Switch,
};
use serde::{Deserialize, Serialize};

use crate::{
    annotations::{self, AnnotationId},
    commands::Command,
    document::OutlineEntry,
    document_tab::{DocumentTab, ViewAction},
    find, icons,
    prefs::PageColumns,
    theme::{self, Tokens},
    translation::{DocSession, Export, PageProgress, SessionStatus},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SidebarTab {
    /// The document's library card (drawn by the app, not here).
    Item,
    #[default]
    Outline,
    Pages,
    Bookmarks,
    Notes,
    Search,
    Translation,
    /// This book's effects (drawn by the stage, not here).
    Effects,
}

impl SidebarTab {
    pub const ALL: [Self; 8] = [
        Self::Item,
        Self::Outline,
        Self::Pages,
        Self::Bookmarks,
        Self::Notes,
        Self::Search,
        Self::Translation,
        Self::Effects,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Item => "Item",
            Self::Outline => "Outline",
            Self::Pages => "Pages",
            Self::Bookmarks => "Bookmarks",
            Self::Notes => "Notes",
            Self::Search => "Search",
            Self::Translation => "Translation",
            Self::Effects => "Effects",
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            Self::Item => "library-big",
            Self::Outline => "list",
            Self::Pages => "layout-grid",
            Self::Bookmarks => "bookmark",
            Self::Notes => "sticky-note",
            Self::Search => "search",
            Self::Translation => "languages",
            Self::Effects => "wand-sparkles",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|tab| tab.label().eq_ignore_ascii_case(name))
    }
}

/// What the sidebar asks the app to do.
#[derive(Clone, Debug, PartialEq)]
pub enum SidebarAction {
    View(ViewAction),
    GoToBookmark(u64),
    AddBookmark,
    RenameBookmark(u64, String),
    RemoveBookmark(u64),
    SetColor(AnnotationId, HighlightColor),
    SetBody(AnnotationId, String),
    Remove(AnnotationId),
    StartTranslation,
    /// Translate every page of a session that follows the reader.
    TranslateRest,
    /// Run the translation again, recognising the failed pages again.
    RetryFailed,
    /// Install local OCR (Settings · OCR), which failed pages need.
    InstallLocalOcr,
    /// Download the CJK fonts, which failed pages need.
    DownloadCjkFonts,
    /// This document's own OCR engine and mode (`None`: the settings').
    SetDocumentOcr(Option<(String, Option<String>)>),
    /// Open Settings · Models.
    OpenModels,
    /// Show pages beside their translation, or in place.
    SetFacing(bool),
    CancelTranslation,
    ExportTranslation(ExportLayout),
    /// Ask before clearing the document's translation (03e).
    AskClearTranslation,
    /// Clear the document's translation and its cached text.
    ClearTranslation,
    /// Keep it after all.
    KeepTranslation,
    ShowTranslated(bool),
    Unpin(usize),
    /// Follow the section being read again and show it.
    LocateOutline,
    /// Hide the sidebar.
    Close,
    Command(Command),
    /// A bookmark of another document: open it there.
    OpenBookmark(u64),
    SetSourceLanguage(Option<String>),
    SetTargetLanguage(String),
    SetProvider(TranslationProvider),
    SetOriginalOnHover(bool),
    SetTranslateAsRead(bool),
}

/// What the sidebar shows besides the tab: the translation settings and
/// the other documents' bookmarks.
pub struct SidebarInfo<'a> {
    pub target: Option<String>,
    /// The language translated from; `None` detects it.
    pub source: Option<String>,
    pub provider: TranslationProvider,
    pub original_on_hover: bool,
    pub translate_as_read: bool,
    pub pinned: &'a [(String, String)],
    pub warnings: &'a [String],
    /// The settings and keys are loaded.
    pub ready: bool,
    pub other_bookmarks: &'a [OtherBookmark],
    /// The key that adds a bookmark, for the bookmarks' footer.
    pub bookmark_key: String,
    /// When the document was opened (`today 09:42`), for its card.
    pub opened: Option<String>,
    pub page_columns: PageColumns,
    /// Local OCR is installed (looked up while a page needs it).
    pub local_ocr_ready: bool,
    /// The CJK fonts are there (looked up while a page needs them).
    pub cjk_fonts_ready: bool,
    /// The document's translation waits for clearing to be confirmed.
    pub confirm_clear: bool,
    /// The document's OCR choice and what it falls back to, while the
    /// translation tab shows.
    pub ocr: Option<OcrRow>,
}

/// The translation panel's OCR row: the settings' choice, the document's
/// own, and the local models installed.
pub struct OcrRow {
    pub settings: glyphst_settings::OcrSettings,
    pub document: Option<glyphst_library::DocumentOcr>,
    pub installed: Vec<&'static str>,
}

impl OcrRow {
    /// The engine in effect: `local:<model>`, `paddle`, `unlimited`, `off`.
    fn engine(&self) -> String {
        if let Some(document) = &self.document {
            return document.engine.clone();
        }
        settings_engine(&self.settings)
    }

    /// `auto` or `always`.
    fn mode(&self) -> String {
        self.document
            .as_ref()
            .and_then(|document| document.mode.clone())
            .unwrap_or_else(|| match self.settings.mode {
                glyphst_settings::OcrMode::Always => "always".to_owned(),
                _ => "auto".to_owned(),
            })
    }

    /// `pp-ocrv6 small · auto`.
    fn label(&self) -> String {
        let engine = self.engine();
        if engine == "off" {
            return "off".to_owned();
        }
        format!("{} · {}", engine_name(&engine), self.mode())
    }
}

/// The settings' engine in the document choice's words.
fn settings_engine(settings: &glyphst_settings::OcrSettings) -> String {
    use glyphst_settings::{OcrMode, OcrProvider};
    match (settings.mode, settings.provider) {
        (OcrMode::Off, _) => "off".to_owned(),
        (_, OcrProvider::Local) => format!("local:{}", settings.local.model),
        (_, OcrProvider::Paddle) => "paddle".to_owned(),
        (_, OcrProvider::Unlimited) => "unlimited".to_owned(),
    }
}

/// `pp-ocrv6 small`, `paddle`.
fn engine_name(engine: &str) -> &str {
    match engine.strip_prefix("local:") {
        Some(model) => glyphst_models::catalog::entry(model).map_or(model, |entry| entry.name),
        None => engine,
    }
}

/// A bookmark in another document.
pub struct OtherBookmark {
    pub id: u64,
    /// The document's short name.
    pub document: String,
    pub title: String,
    /// `bert p.4`.
    pub place: String,
}

/// Which annotations the Notes tab lists.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum NotesFilter {
    #[default]
    All,
    Highlights,
    Notes,
}

/// Edits in progress in the sidebar.
#[derive(Default)]
pub struct SidebarState {
    bookmark: Option<(u64, String)>,
    /// The rename field takes the focus once, when renaming starts: asking
    /// on every frame would interrupt an input method's composition.
    focus_rename: bool,
    note: Option<(AnnotationId, String)>,
    /// The note opened under its row, with its colours and actions.
    open_note: Option<AnnotationId>,
    /// Each section's filter text.
    filters: HashMap<SidebarTab, String>,
    notes_filter: NotesFilter,
}

/// A tab's outline and thumbnail lists, after the GPUI Reader's: the
/// outline follows the section being read until it is scrolled away from
/// by hand or following is turned off, and the thumbnails follow the page.
#[derive(Default)]
pub struct OutlineView {
    collapsed: HashSet<usize>,
    /// Following is turned off (the locate toggle): the list stays where it
    /// is while the document scrolls.
    unfollowed: bool,
    /// Scrolling the current entry out of the list by hand stops following
    /// it until it is located again or an entry is chosen.
    paused: bool,
    /// The current entry when the outline was last drawn.
    current: Option<usize>,
    /// A reveal of the current entry waiting for the next frame; strict
    /// ones centre it even when it shows.
    reveal: Option<bool>,
    list: ListState,
    pages: ListState,
    /// The page the thumbnails last followed.
    page: Option<usize>,
    /// Their columns then; a change re-centres the current page.
    page_columns: usize,
}

impl OutlineView {
    /// Whether the outline follows the section being read.
    pub fn following(&self) -> bool {
        !self.paused && !self.unfollowed
    }

    /// Turns following on or off; turning it on doesn't locate.
    pub fn set_follow(&mut self, on: bool) {
        self.unfollowed = !on;
    }

    /// Follows the section being read again and centres it.
    pub fn locate(&mut self) {
        self.paused = false;
        self.reveal = Some(true);
    }
}

/// Where a list was scrolled when last drawn.
#[derive(Default)]
struct ListState {
    offset: f32,
    viewport: f32,
    /// The frame it was last drawn in.
    frame: Option<u64>,
}

impl ListState {
    /// Marks the list drawn this frame; true when it was not drawn in the
    /// last one (the sidebar or the tab was just opened).
    fn shown(&mut self, ctx: &egui::Context) -> bool {
        let frame = ctx.cumulative_frame_nr();
        let fresh = self.frame.is_none_or(|last| last + 1 < frame);
        self.frame = Some(frame);
        fresh
    }

    fn viewport_or(&self, ui: &egui::Ui) -> f32 {
        if self.viewport > 0.0 {
            self.viewport
        } else {
            ui.available_height()
        }
    }
}

const QUOTE_CHARS: usize = 240;

pub fn show(
    ui: &mut egui::Ui,
    tab: &mut DocumentTab,
    which: &mut SidebarTab,
    state: &mut SidebarState,
    info: &SidebarInfo<'_>,
    now: Instant,
) -> Vec<SidebarAction> {
    let mut actions = Vec::new();
    // Long lists can be filtered.
    let hint = match which {
        SidebarTab::Outline if tab.outline.len() > LONG_LIST => Some("filter the outline"),
        SidebarTab::Bookmarks if tab.annotations.bookmarks.len() > LONG_LIST => {
            Some("filter bookmarks")
        }
        SidebarTab::Notes if tab.annotations.items.len() > LONG_LIST => Some("filter notes"),
        _ => None,
    };
    let filter = match hint {
        Some(hint) => {
            let text = state.filters.entry(*which).or_default();
            ui.add_space(8.0);
            glyphst_ui::field::TextField::search(text, hint).show(ui);
            ui.add_space(4.0);
            text.trim().to_lowercase()
        }
        None => String::new(),
    };
    ui.add_space(4.0);
    match which {
        SidebarTab::Outline => {
            // The document's card stays at the foot, under the outline.
            let card_height = 108.0;
            let area = ui.available_rect_before_wrap();
            let list = area.with_max_y((area.bottom() - card_height).max(area.top()));
            let mut list_ui = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(list)
                    .layout(egui::Layout::top_down(egui::Align::Min)),
            );
            outline(&mut list_ui, tab, &filter, &mut actions);
            let card = area.with_min_y(list.bottom());
            let mut card_ui = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(card)
                    .layout(egui::Layout::top_down(egui::Align::Min)),
            );
            document_card(&mut card_ui, tab, info.opened.as_deref());
        }
        SidebarTab::Pages => pages(ui, tab, info.page_columns, &mut actions),
        SidebarTab::Bookmarks => bookmarks(ui, tab, state, info, &filter, &mut actions),
        SidebarTab::Notes => notes(ui, tab, state, &filter, &mut actions),
        // Choosing a match makes it current; the view brings it in.
        SidebarTab::Search => {
            find::search_panel(&mut tab.find, ui, now);
        }
        SidebarTab::Translation => translation_panel(ui, tab, info, &mut actions),
        SidebarTab::Effects | SidebarTab::Item => {}
    }
    actions
}

/// Lists longer than this get a filter field.
const LONG_LIST: usize = 40;

/// The document card under the outline: title, authors, pages and format,
/// when it was opened.
pub fn document_card(ui: &mut egui::Ui, tab: &DocumentTab, opened: Option<&str>) {
    let tokens = Tokens::get(ui.ctx());
    let meta = |label: &str| {
        tab.metadata
            .iter()
            .find(|(name, _)| *name == label)
            .map(|(_, value)| value.as_str())
    };
    let size = std::fs::metadata(&tab.path)
        .map(|metadata| format_size(metadata.len()))
        .ok();
    let mut pages = format!("{}", tab.layout.len());
    if let Some(format) = meta("Format") {
        pages.push_str(&format!(" · {}", format.to_lowercase()));
    }
    if let Some(size) = size {
        pages.push_str(&format!(" · {size}"));
    }
    ui.painter().hline(
        ui.max_rect().x_range(),
        ui.cursor().top(),
        tokens.hairline(),
    );
    ui.add_space(8.0);
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        ui.set_width(ui.available_width());
        motif::rule_title(ui, "document", None);
        ui.add_space(2.0);
        let label_width = 72.0;
        ui_row::field_line(ui, "title", &tab.title, label_width);
        if let Some(author) = meta("Author") {
            ui_row::field_line(ui, "authors", author, label_width);
        }
        ui_row::field_line(ui, "pages", &pages, label_width);
        if let Some(opened) = opened {
            ui_row::field_line(ui, "opened", opened, label_width);
        }
    });
    ui.add_space(8.0);
}

/// `2.1 MB`.
pub fn format_size(bytes: u64) -> String {
    let bytes = bytes as f64;
    if bytes >= 1024.0 * 1024.0 * 1024.0 {
        format!("{:.1} GB", bytes / 1024.0 / 1024.0 / 1024.0)
    } else if bytes >= 1024.0 * 1024.0 {
        format!("{:.1} MB", bytes / 1024.0 / 1024.0)
    } else {
        format!("{:.0} KB", (bytes / 1024.0).max(1.0))
    }
}

fn translation_panel(
    ui: &mut egui::Ui,
    tab: &DocumentTab,
    info: &SidebarInfo<'_>,
    actions: &mut Vec<SidebarAction>,
) {
    let tokens = Tokens::get(ui.ctx());
    let is_pdf = tab
        .path
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("pdf"));
    ui.add_space(6.0);
    // The three choices: from, to, engine.
    let source = info
        .source
        .as_deref()
        .map(|code| format!("{} · fixed", language_label(code).to_lowercase()))
        .unwrap_or_else(|| "detect · auto".to_owned());
    let target = info
        .target
        .as_deref()
        .map(language_label)
        .unwrap_or("the settings' language")
        .to_owned();
    if let Some(response) = select_row(ui, "from", &source) {
        egui::Popup::menu(&response).show(|ui| {
            ui_menu::style(ui);
            if ui_menu::Item::new("detect")
                .checked(info.source.is_none())
                .show(ui)
                .clicked()
            {
                actions.push(SidebarAction::SetSourceLanguage(None));
            }
            ui_menu::separator(ui);
            languages(ui, info.source.as_deref(), |code| {
                actions.push(SidebarAction::SetSourceLanguage(Some(code)));
            });
        });
    }
    ui.add_space(6.0);
    if let Some(response) = select_row(ui, "to", &target) {
        egui::Popup::menu(&response).show(|ui| {
            ui_menu::style(ui);
            languages(ui, info.target.as_deref(), |code| {
                actions.push(SidebarAction::SetTargetLanguage(code));
            });
        });
    }
    ui.add_space(6.0);
    if let Some(response) = select_row(ui, "engine", info.provider.as_str()) {
        egui::Popup::menu(&response).show(|ui| {
            ui_menu::style(ui);
            for provider in TranslationProvider::ALL {
                if ui_menu::Item::new(provider.as_str())
                    .checked(provider == info.provider)
                    .show(ui)
                    .clicked()
                {
                    actions.push(SidebarAction::SetProvider(provider));
                }
            }
        });
    }
    if let Some(ocr) = &info.ocr {
        ui.add_space(6.0);
        ocr_row(ui, ocr, actions);
    }
    ui.add_space(10.0);
    // Where translated pages go: over the originals, or beside them.
    ui.horizontal(|ui| {
        ui.set_min_height(25.0);
        ui.label(
            egui::RichText::new("view")
                .font(theme::mono(12.0))
                .color(if is_pdf { tokens.text_2 } else { tokens.faint }),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let mut facing = tab.layout.arrangement.facing;
            if ui
                .add_enabled_ui(is_pdf, |ui| {
                    segmented::labels(
                        ui,
                        &mut facing,
                        &[(false, "in place"), (true, "side by side")],
                    )
                })
                .inner
            {
                actions.push(SidebarAction::SetFacing(facing));
            }
        });
    });
    ui.add_space(4.0);
    let mut hover = info.original_on_hover;
    if switch_row(ui, "original on hover", &mut hover, true) {
        actions.push(SidebarAction::SetOriginalOnHover(hover));
    }
    let mut as_read = info.translate_as_read;
    if switch_row(ui, "translate as I read", &mut as_read, is_pdf) {
        actions.push(SidebarAction::SetTranslateAsRead(as_read));
    }
    for warning in info.warnings {
        muted(ui, warning);
    }
    if !is_pdf {
        ui.add_space(8.0);
        muted(ui, "only PDFs translate as documents.");
        return;
    }
    ui.add_space(10.0);
    scope_slot(ui, tab, info, actions);
    let footer = 44.0;
    if let Some(session) = &tab.translation {
        ui.add_space(8.0);
        let total = session.page_count.max(tab.layout.len());
        let done = (0..total)
            .filter(|page| session.page_progress(*page) == PageProgress::Done)
            .count();
        let count = if session.is_following() && session.is_running() {
            format!("following · {done}/{total}")
        } else {
            format!("{done}/{total}")
        };
        motif::rule_title(ui, "pages", Some(&count));
        let current = tab.current_page();
        let states: Vec<PageProgress> =
            (0..total).map(|page| session.page_progress(page)).collect();
        let kept = |page: usize| {
            session
                .previews
                .get(&page)
                .map_or(&[][..], |preview| preview.kept_original.as_slice())
        };
        // What a page says on hover: its state, and why.
        let why = |page: usize| {
            session
                .failure(page)
                .map(|failure| failure.message.clone())
                .or_else(|| (!kept(page).is_empty()).then(|| kept(page).join("\n")))
        };
        ui.add_space(4.0);
        progress_summary(ui, &states, (0..total).map(|page| kept(page).len()).sum());
        ui.add_space(8.0);
        let hover = |page: usize| {
            let (glyph, label, _) = progress_label(states[page], kept(page).len(), &tokens);
            let mut text = format!("p.{} · {glyph} {label}", page + 1);
            if let Some(why) = why(page) {
                text.push_str(&format!("\n{why}"));
            }
            text
        };
        if let Some(page) = page_map(ui, &states, current, hover) {
            actions.push(SidebarAction::View(ViewAction::GoToPage(page)));
        }
        ui.add_space(6.0);
        map_legend(ui);
        ui.add_space(6.0);
        // Below the map, only the pages that need a look: translating,
        // failed, or with blocks kept original.
        let mut listed: Vec<usize> = (0..total)
            .filter(|&page| matches!(states[page], PageProgress::Partial { .. }))
            .collect();
        listed.extend((0..total).filter(|&page| matches!(states[page], PageProgress::Failed(_))));
        listed.extend(
            (0..total).filter(|&page| states[page] == PageProgress::Done && !kept(page).is_empty()),
        );
        let waiting = states
            .iter()
            .filter(|state| **state == PageProgress::Waiting)
            .count();
        let height = (ui.available_height() - footer).max(60.0);
        egui::ScrollArea::vertical()
            .id_salt(("translation-pages", tab.id))
            .max_height(height)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for &page in listed.iter().take(ACTIVE_ROWS) {
                    let why = why(page);
                    page_status_row(
                        ui,
                        page,
                        page,
                        states[page],
                        page == current,
                        why.as_deref(),
                        kept(page).len(),
                    );
                }
                let more = listed.len().saturating_sub(ACTIVE_ROWS);
                let rest = match (more, waiting) {
                    (0, 0) => None,
                    (0, waiting) => Some(format!("and {waiting} waiting")),
                    (more, 0) => Some(format!("and {more} more")),
                    (more, waiting) => Some(format!("and {more} more · {waiting} waiting")),
                };
                if let Some(rest) = rest {
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(rest)
                            .font(theme::mono(10.0))
                            .color(tokens.faint),
                    );
                }
            });
        match &session.export {
            Some(Export::Running(_)) => muted(ui, "exporting…"),
            Some(Export::Done(Ok(path))) => muted(ui, &format!("saved {}", path.display())),
            Some(Export::Done(Err(error))) => muted(ui, &format!("export failed: {error}")),
            None => {}
        }
    }
    // The pinned translations of selections.
    if !info.pinned.is_empty() {
        ui.add_space(8.0);
        motif::rule_title(ui, "pinned", Some(&info.pinned.len().to_string()));
        for (index, (source, translated)) in info.pinned.iter().enumerate() {
            ui.push_id(index, |ui| {
                let quote: String = source.chars().take(QUOTE_CHARS).collect();
                let response = quote_card(ui, &tokens, tokens.accent, &quote, translated, "");
                response.context_menu(|ui| {
                    ui_menu::style(ui);
                    if ui_menu::Item::new("copy").icon("copy").show(ui).clicked() {
                        ui.ctx().copy_text(translated.clone());
                    }
                    if ui_menu::Item::new("unpin")
                        .icon("trash-2")
                        .danger(true)
                        .show(ui)
                        .clicked()
                    {
                        actions.push(SidebarAction::Unpin(index));
                    }
                });
            });
        }
    }
    // The foot of the pane: what is shown, and the export. Its controls keep
    // their size whatever the session does.
    if let Some(session) = &tab.translation {
        let bottom = ui.max_rect().bottom();
        let foot = egui::Rect::from_min_max(
            egui::pos2(ui.max_rect().left(), bottom - footer + 8.0),
            egui::pos2(ui.max_rect().right(), bottom),
        );
        if info.confirm_clear {
            clear_confirm(ui, foot, actions);
            return;
        }
        let mut foot_ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(foot)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        // Beside their translation, both are in view already.
        let facing = tab.layout.arrangement.facing;
        let mut shown = session.show_translated || facing;
        let changed = foot_ui
            .add_enabled_ui(!facing, |ui| {
                segmented::labels(ui, &mut shown, &[(true, "translated"), (false, "original")])
            })
            .inner;
        if changed {
            actions.push(SidebarAction::ShowTranslated(shown));
        }
        foot_ui.add_space(8.0);
        let can_export = session.status == SessionStatus::Finished
            && !matches!(session.export, Some(Export::Running(_)));
        let width = foot_ui.available_width();
        let export = foot_ui.add(
            Button::primary("export")
                .trailing_icon("chevron-down")
                .min_width(width),
        );
        egui::Popup::menu(&export).show(|ui| {
            ui_menu::style(ui);
            ui.add_enabled_ui(can_export, |ui| {
                if ui_menu::Item::new("side by side")
                    .icon("columns-2")
                    .detail("original | translation on one wide page")
                    .key("pdf")
                    .show(ui)
                    .clicked()
                {
                    actions.push(SidebarAction::ExportTranslation(ExportLayout::SideBySide));
                }
                if ui_menu::Item::new("translated only")
                    .icon("file-text")
                    .key("pdf")
                    .show(ui)
                    .clicked()
                {
                    actions.push(SidebarAction::ExportTranslation(ExportLayout::Translated));
                }
            });
            ui_menu::separator(ui);
            if ui_menu::Item::new("clear translation…")
                .icon("trash-2")
                .danger(true)
                .show(ui)
                .clicked()
            {
                actions.push(SidebarAction::AskClearTranslation);
            }
        });
    }
}

/// Clearing, confirmed in place (frame 03e): the question over the foot of
/// the pages, and the foot's slot turned to `[ cancel ] [ clear ]`.
fn clear_confirm(ui: &mut egui::Ui, foot: egui::Rect, actions: &mut Vec<SidebarAction>) {
    let tokens = Tokens::get(ui.ctx());
    let width = foot.width();
    let galley = |text: &str, size: f32, color| {
        egui::WidgetText::from(
            egui::RichText::new(text)
                .font(theme::mono(size))
                .color(color),
        )
        .into_galley(
            ui,
            Some(egui::TextWrapMode::Wrap),
            width - 16.0,
            egui::FontSelection::Default,
        )
    };
    let question = galley("Clear this document's translation?", 11.0, tokens.text);
    let detail = galley(
        "Its translated pages and cached text go; the next run asks the provider again. Recognised scans stay.",
        10.0,
        tokens.muted,
    );
    let height = 8.0 + question.size().y + 6.0 + detail.size().y + 8.0;
    let note = egui::Rect::from_min_size(
        egui::pos2(foot.left(), foot.top() - 8.0 - height),
        egui::vec2(width, height),
    );
    let painter = ui.painter();
    // Over the pages' foot, opaque, so nothing under it moves.
    painter.rect_filled(note, tokens.ctl_radius(), tokens.surface);
    painter.rect_filled(note, tokens.ctl_radius(), tokens.err_soft);
    let question_height = question.size().y;
    painter.galley(note.min + egui::vec2(8.0, 8.0), question, tokens.text);
    painter.galley(
        note.min + egui::vec2(8.0, 8.0 + question_height + 6.0),
        detail,
        tokens.muted,
    );
    // The foot's slot gives the two answers.
    let mut slot = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(foot)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    slot.spacing_mut().item_spacing.x = 8.0;
    let half = (width - 8.0) / 2.0;
    if slot.add(Button::ghost("cancel").min_width(half)).clicked() {
        actions.push(SidebarAction::KeepTranslation);
    }
    if slot.add(Button::ghost("clear").min_width(half)).clicked() {
        actions.push(SidebarAction::ClearTranslation);
    }
}

/// The one action that fits the session, in a slot as wide as the pane so
/// changing one's mind never moves anything: start, take in the rest of
/// the document, cancel, or a line saying it is done.
fn scope_slot(
    ui: &mut egui::Ui,
    tab: &DocumentTab,
    info: &SidebarInfo<'_>,
    actions: &mut Vec<SidebarAction>,
) {
    let tokens = Tokens::get(ui.ctx());
    let width = ui.available_width();
    let target = info
        .target
        .as_deref()
        .map(language_label)
        .unwrap_or("the settings' language");
    let (button, note) = match &tab.translation {
        None => {
            let start = ui.add_enabled(
                info.ready,
                Button::primary("translate document").min_width(width),
            );
            if start.clicked() {
                actions.push(SidebarAction::StartTranslation);
            }
            let note = if info.ready {
                format!(
                    "or switch on translate as I read: it translates the page you are on and the next two, into {target}."
                )
            } else {
                "reading the translation settings…".to_owned()
            };
            (None, note)
        }
        Some(session) if session.is_running() && session.is_following() => {
            let rest = session.untranslated_pages();
            let label = format!("translate the rest · {rest} pages");
            let note = match session.window() {
                Some((first, last)) => format!(
                    "following your page: p.{}–{} now, more as you read.",
                    first + 1,
                    last + 1
                ),
                None => "following your page.".to_owned(),
            };
            (Some((label, SidebarAction::TranslateRest)), note)
        }
        Some(session) if session.is_running() => (
            Some(("cancel".to_owned(), SidebarAction::CancelTranslation)),
            format!(
                "translating the whole document · {}",
                session.status_line().to_lowercase()
            ),
        ),
        Some(session)
            if !session.failed_pages().is_empty()
                && !matches!(session.status, SessionStatus::Failed(_)) =>
        {
            let failed = session.failed_pages();
            let total = session.page_count.max(tab.layout.len());
            let label = match failed.len() {
                1 => "retry the failed page".to_owned(),
                count => format!("retry {count} failed pages"),
            };
            let note = format!(
                "{} of {total} pages translated.",
                total.saturating_sub(session.untranslated_pages())
            );
            (Some((label, SidebarAction::RetryFailed)), note)
        }
        Some(session) => {
            let (rect, _) =
                ui.allocate_exact_size(egui::vec2(width, tokens.row(28.0)), egui::Sense::hover());
            ui.painter().rect_stroke(
                rect,
                tokens.ctl_radius(),
                tokens.edge(),
                egui::StrokeKind::Inside,
            );
            let (line, color, note) = match &session.status {
                SessionStatus::Failed(error) => {
                    ("✕ stopped".to_owned(), tokens.err, error.to_lowercase())
                }
                _ => {
                    let total = session.page_count.max(tab.layout.len());
                    let left = session.untranslated_pages();
                    if left == 0 {
                        (
                            format!("✓ all {total} pages translated"),
                            tokens.ok,
                            "kept in the cache: reopening shows them at once.".to_owned(),
                        )
                    } else {
                        (
                            format!("✓ {} of {total} pages translated", total - left),
                            tokens.ok,
                            "the rest stay in the original.".to_owned(),
                        )
                    }
                }
            };
            ui.painter().text(
                egui::pos2(rect.left() + 8.0, rect.center().y),
                egui::Align2::LEFT_CENTER,
                line,
                theme::mono(12.0),
                color,
            );
            (None, note)
        }
    };
    if let Some((label, action)) = button
        && ui.add(Button::ghost(&label).min_width(width)).clicked()
    {
        actions.push(action);
    }
    ui.add_space(6.0);
    muted(ui, &note);
    if let Some(session) = &tab.translation {
        failures_box(ui, session, info, actions);
    }
}

/// What went wrong with the pages that failed, and what fixes it (frame
/// 03d): installing local OCR, or trying again.
fn failures_box(
    ui: &mut egui::Ui,
    session: &DocSession,
    info: &SidebarInfo<'_>,
    actions: &mut Vec<SidebarAction>,
) {
    let failed = session.failed_pages();
    let Some(&first) = failed.first() else {
        return;
    };
    let tokens = Tokens::get(ui.ctx());
    let pages = match failed.as_slice() {
        [page] => format!("p.{}", page + 1),
        [a, b] => format!("p.{} and p.{}", a + 1, b + 1),
        _ => format!("{} pages", failed.len()),
    };
    let one = failed.len() == 1;
    let code = session
        .failure(first)
        .map(|failure| failure.code)
        .unwrap_or_default();
    let missing = session.needs_local_ocr() && !info.local_ocr_ready;
    let fonts_missing = session.needs_cjk_fonts() && !info.cjk_fonts_ready;
    let message = if fonts_missing {
        format!(
            "✕ {pages}: the translation is in Chinese, Japanese or Korean, and the CJK fonts are not downloaded."
        )
    } else if session.needs_cjk_fonts() {
        format!("✕ {pages} waited for the CJK fonts, now downloaded: try again to translate them.")
    } else if missing {
        if one {
            format!(
                "✕ {pages} is a scan: it needs text recognition, and no local model is installed."
            )
        } else {
            format!(
                "✕ {pages} are scans: they need text recognition, and no local model is installed."
            )
        }
    } else if session.needs_local_ocr() {
        format!("✕ {pages} waited for local OCR, now installed: try again to recognise them.")
    } else {
        let why = match code {
            PageFailureCode::Network => "the OCR service stopped answering",
            PageFailureCode::Provider => "the OCR service refused the page",
            PageFailureCode::LocalOcrFailed => "local OCR could not read the page",
            PageFailureCode::Layout => "the page could not be rebuilt",
            _ => "something went wrong",
        };
        format!("✕ {pages}: {why}.")
    };
    ui.add_space(10.0);
    egui::Frame::new()
        .fill(tokens.err_soft)
        .corner_radius(tokens.ctl_radius())
        .inner_margin(8.0)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 8.0;
            let label = ui.add(
                egui::Label::new(
                    egui::RichText::new(message)
                        .font(theme::mono(11.0))
                        .color(tokens.text),
                )
                .wrap(),
            );
            if let Some(failure) = session.failure(first) {
                label.on_hover_text(&failure.message);
            }
            let width = ui.available_width();
            if fonts_missing {
                let fonts =
                    glyphst_models::catalog::default_for(glyphst_models::catalog::Task::Fonts);
                let download = format!("download {} · {}", fonts.name, format_size(fonts.size()));
                if ui.add(Button::ghost(&download).min_width(width)).clicked() {
                    actions.push(SidebarAction::DownloadCjkFonts);
                }
                ui.add(
                    egui::Label::new(
                        egui::RichText::new("or download them in Settings · Models.")
                            .font(theme::mono(10.0))
                            .color(tokens.muted),
                    )
                    .wrap(),
                );
            } else if missing {
                // The model Settings · OCR names, as Models would download it.
                let model = info
                    .ocr
                    .as_ref()
                    .and_then(|ocr| glyphst_models::catalog::entry(&ocr.settings.local.model))
                    .unwrap_or_else(|| {
                        glyphst_models::catalog::default_for(
                            glyphst_models::catalog::Task::TextRecognition,
                        )
                    });
                let install = format!("install {} · {}", model.name, format_size(model.size()));
                if ui.add(Button::ghost(&install).min_width(width)).clicked() {
                    actions.push(SidebarAction::InstallLocalOcr);
                }
                ui.add(
                    egui::Label::new(
                        egui::RichText::new("or pick paddle or unlimited in Settings · OCR.")
                            .font(theme::mono(10.0))
                            .color(tokens.muted),
                    )
                    .wrap(),
                );
            } else if session.is_running()
                && ui
                    .add(Button::ghost("try these pages again").min_width(width))
                    .clicked()
            {
                actions.push(SidebarAction::RetryFailed);
            }
        });
}

/// The document's OCR: which engine reads its scans, and when (frame 03d).
fn ocr_row(ui: &mut egui::Ui, ocr: &OcrRow, actions: &mut Vec<SidebarAction>) {
    let Some(response) = select_row(ui, "ocr", &ocr.label()) else {
        return;
    };
    let engine = ocr.engine();
    let mode = ocr
        .document
        .as_ref()
        .and_then(|document| document.mode.clone());
    let choose = |engine: String| SidebarAction::SetDocumentOcr(Some((engine, mode.clone())));
    egui::Popup::menu(&response).show(|ui| {
        ui_menu::style(ui);
        let settings = settings_engine(&ocr.settings);
        if ui_menu::Item::new("as in settings")
            .detail(engine_name(&settings))
            .checked(ocr.document.is_none())
            .show(ui)
            .clicked()
        {
            actions.push(SidebarAction::SetDocumentOcr(None));
        }
        ui_menu::group(ui, "this computer");
        for model in
            glyphst_models::catalog::for_task(glyphst_models::catalog::Task::TextRecognition)
        {
            let id = format!("local:{}", model.id);
            if ocr.installed.contains(&model.id) {
                let size = format_size(model.size());
                if ui_menu::Item::new(model.name)
                    .detail(&size)
                    .checked(engine == id)
                    .show(ui)
                    .clicked()
                {
                    actions.push(choose(id));
                }
            } else if ui_menu::Item::new(model.name)
                .detail("install…")
                .show(ui)
                .clicked()
            {
                actions.push(SidebarAction::OpenModels);
            }
        }
        ui_menu::group(ui, "online");
        for (name, detail) in [("paddle", "paddleocr-vl"), ("unlimited", "baidu")] {
            if ui_menu::Item::new(name)
                .detail(detail)
                .checked(engine == name)
                .show(ui)
                .clicked()
            {
                actions.push(choose(name.to_owned()));
            }
        }
        if ui_menu::Item::new("off")
            .detail("native text only")
            .checked(engine == "off")
            .show(ui)
            .clicked()
        {
            actions.push(choose("off".to_owned()));
        }
        ui_menu::group(ui, "recognise");
        let keep = if engine == "off" {
            settings.clone()
        } else {
            engine.clone()
        };
        for (label, value) in [
            ("when a page has no text", "auto"),
            ("every page", "always"),
        ] {
            if ui_menu::Item::new(label)
                .checked(engine != "off" && ocr.mode() == value)
                .show(ui)
                .clicked()
            {
                actions.push(SidebarAction::SetDocumentOcr(Some((
                    keep.clone(),
                    Some(value.to_owned()),
                ))));
            }
        }
    });
}

/// The rows listed under the page map, before `and N more`.
const ACTIVE_ROWS: usize = 6;
/// The most rows the page map takes, its cells shrinking to stay within.
const MAP_ROWS: usize = 14;

/// The side of a page map's cells, the gap between them and how many fit
/// a row of `width`, for `count` pages: the largest that keeps the map
/// within [`MAP_ROWS`] rows.
fn map_cells(count: usize, width: f32) -> (f32, f32, usize) {
    let fit = |cell: f32, gap: f32| (((width + gap) / (cell + gap)).floor() as usize).max(1);
    for cell in [18.0, 16.0, 14.0, 12.0, 10.0, 8.0, 7.0, 6.0, 5.0, 4.0] {
        let gap = if cell >= 10.0 { 3.0 } else { 1.0 };
        let per_row = fit(cell, gap);
        if count.div_ceil(per_row) <= MAP_ROWS {
            return (cell, gap, per_row);
        }
    }
    (3.0, 1.0, fit(3.0, 1.0))
}

/// `━━━━━──── 42%  2 translating · 2 waiting` (frame 03e).
fn progress_summary(ui: &mut egui::Ui, states: &[PageProgress], kept: usize) {
    let tokens = Tokens::get(ui.ctx());
    let count =
        |wanted: fn(&PageProgress) -> bool| states.iter().filter(|state| wanted(state)).count();
    let done = count(|state| *state == PageProgress::Done);
    let translating = count(|state| matches!(state, PageProgress::Partial { .. }));
    let failed = count(|state| matches!(state, PageProgress::Failed(_)));
    let waiting = count(|state| *state == PageProgress::Waiting);
    let mut parts = Vec::new();
    if translating > 0 {
        parts.push(format!("{translating} translating"));
    }
    if failed > 0 {
        parts.push(format!("{failed} failed"));
    } else if waiting > 0 {
        parts.push(format!("{waiting} waiting"));
    }
    if kept > 0 {
        parts.push(format!("{kept} kept original"));
    }
    let fraction = done as f32 / states.len().max(1) as f32;
    let percent = format!("{:.0}%", fraction * 100.0);
    // One line as wide as the pane: the meter, then the counts cut to the
    // rest, whole on hover.
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 14.0), egui::Sense::hover());
    let bar = egui::Rect::from_min_size(rect.min, egui::vec2(104.0, rect.height()));
    Meter::new(fraction)
        .width(bar.width())
        .label(&percent)
        .paint(ui, bar);
    let counts = parts.join(" · ");
    let font = theme::mono(10.0);
    let room = rect.right() - bar.right() - 8.0;
    let whole = ui
        .painter()
        .layout_no_wrap(counts.clone(), font.clone(), tokens.muted)
        .size()
        .x;
    glyphst_ui::text::row(
        ui,
        egui::pos2(bar.right() + 8.0, rect.center().y),
        &counts,
        font,
        tokens.muted,
        room,
    );
    if whole > room {
        response.on_hover_text(counts);
    }
}

/// A page's colour in the map.
fn map_color(state: PageProgress, tokens: &Tokens) -> egui::Color32 {
    match state {
        PageProgress::Done => tokens.ok.gamma_multiply(0.55),
        PageProgress::Partial { .. } => tokens.accent,
        PageProgress::Failed(_) => tokens.err,
        PageProgress::Waiting => tokens.faint,
        PageProgress::NotTranslated => tokens.hover,
    }
}

/// The pages as a map (frame 03e): a cell a page, coloured by its state,
/// the current page outlined; `hover` says what a page's cell shows.
/// Returns the page clicked.
fn page_map(
    ui: &mut egui::Ui,
    states: &[PageProgress],
    current: usize,
    hover: impl Fn(usize) -> String,
) -> Option<usize> {
    let tokens = Tokens::get(ui.ctx());
    let width = ui.available_width();
    let (cell, gap, per_row) = map_cells(states.len(), width);
    let rows = states.len().div_ceil(per_row);
    let height = rows as f32 * (cell + gap) - gap;
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(width, height.max(0.0)), egui::Sense::click());
    let cell_rect = |page: usize| {
        let (row, column) = (page / per_row, page % per_row);
        egui::Rect::from_min_size(
            rect.min + egui::vec2(column as f32 * (cell + gap), row as f32 * (cell + gap)),
            egui::vec2(cell, cell),
        )
    };
    let radius = if cell > 8.0 { 2.0 } else { 1.0 };
    let painter = ui.painter();
    for (page, state) in states.iter().enumerate() {
        painter.rect_filled(cell_rect(page), radius, map_color(*state, &tokens));
    }
    if current < states.len() {
        painter.rect_stroke(
            cell_rect(current).expand(0.5),
            radius,
            egui::Stroke::new(1.5, tokens.text),
            egui::StrokeKind::Outside,
        );
    }
    let page = response.hover_pos().and_then(|pos| {
        let offset = pos - rect.min;
        let (column, row) = (
            (offset.x / (cell + gap)) as usize,
            (offset.y / (cell + gap)) as usize,
        );
        let page = row * per_row + column;
        (column < per_row && page < states.len()).then_some(page)
    });
    let clicked = response.clicked();
    if let Some(page) = page {
        response.on_hover_text(hover(page));
    }
    page.filter(|_| clicked)
}

/// `■ done ■ translating ■ waiting ■ failed`, under the map.
fn map_legend(ui: &mut egui::Ui) {
    let tokens = Tokens::get(ui.ctx());
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        for (state, label) in [
            (PageProgress::Done, "done"),
            (PageProgress::Partial { done: 0, total: 1 }, "translating"),
            (PageProgress::Waiting, "waiting"),
            (PageProgress::Failed(PageFailureCode::Other), "failed"),
        ] {
            let (swatch, _) = ui.allocate_exact_size(egui::vec2(7.0, 7.0), egui::Sense::hover());
            ui.painter()
                .rect_filled(swatch, 1.0, map_color(state, &tokens));
            ui.add_space(-4.0);
            ui.label(
                egui::RichText::new(label)
                    .font(theme::mono(10.0))
                    .color(tokens.faint),
            );
        }
    });
}

/// A page's state as `✓ translated`, `◌ translating · 3/5 blocks`, `✕
/// network`…, with its colour.
fn progress_label(
    state: PageProgress,
    kept: usize,
    tokens: &Tokens,
) -> (&'static str, String, egui::Color32) {
    match state {
        PageProgress::Done if kept > 0 => ("✓", format!("translated · {kept} kept"), tokens.ok),
        PageProgress::Done => ("✓", "translated".to_string(), tokens.ok),
        PageProgress::Partial { done, total } => (
            "◌",
            format!("translating · {done}/{total} blocks"),
            tokens.accent_text,
        ),
        PageProgress::Failed(code) => (
            "✕",
            match code {
                PageFailureCode::LocalOcrMissing => "scan · needs ocr",
                PageFailureCode::LocalOcrFailed => "ocr failed",
                PageFailureCode::Network => "network",
                PageFailureCode::Provider => "ocr service",
                PageFailureCode::Layout => "page error",
                PageFailureCode::FontsMissing => "needs cjk fonts",
                PageFailureCode::Other => "failed",
            }
            .to_string(),
            tokens.err,
        ),
        PageProgress::Waiting => ("·", "waiting".to_string(), tokens.muted),
        PageProgress::NotTranslated => ("·", "not translated".to_string(), tokens.faint),
    }
}

fn page_status_row(
    ui: &mut egui::Ui,
    first: usize,
    last: usize,
    state: PageProgress,
    current: bool,
    why: Option<&str>,
    kept: usize,
) {
    let tokens = Tokens::get(ui.ctx());
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 21.0), egui::Sense::hover());
    // A failed page, or one with blocks kept original, says why on hover.
    if let Some(why) = why {
        response.on_hover_text(why);
    }
    if current {
        ui.painter().rect_filled(rect, 0, tokens.accent_soft);
    }
    let pages = if first == last {
        format!("p.{}", first + 1)
    } else {
        format!("p.{} … p.{}", first + 1, last + 1)
    };
    let font = theme::mono(11.0);
    let y = rect.center().y;
    ui.painter().text(
        egui::pos2(rect.left() + 4.0, y),
        egui::Align2::LEFT_CENTER,
        pages,
        font.clone(),
        if current {
            tokens.accent_text
        } else {
            tokens.text_2
        },
    );
    // The state from x 76, after a page column (frame 03e), cut to the row.
    let (glyph, label, color) = progress_label(state, kept, &tokens);
    let x = rect.left() + 76.0;
    ui.painter().text(
        egui::pos2(x, y),
        egui::Align2::LEFT_CENTER,
        glyph,
        font.clone(),
        color,
    );
    glyphst_ui::text::row(
        ui,
        egui::pos2(x + 14.0, y),
        label,
        font,
        match state {
            PageProgress::Done if current => tokens.ok,
            PageProgress::Partial { .. } => tokens.accent_text,
            PageProgress::Failed(_) => tokens.err,
            PageProgress::Waiting => tokens.muted,
            _ => tokens.faint,
        },
        rect.right() - 4.0 - (x + 14.0),
    );
}

/// A choice shown as `label   value  ⇕` on a raised field; returns its
/// response for the menu, `None` while disabled.
fn select_row(ui: &mut egui::Ui, label: &str, value: &str) -> Option<egui::Response> {
    let tokens = Tokens::get(ui.ctx());
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 28.0), egui::Sense::click());
    let edge = if response.hovered() {
        egui::Stroke::new(1.0, tokens.faint)
    } else {
        tokens.edge()
    };
    ui.painter().rect(
        rect,
        tokens.ctl_radius(),
        tokens.raised,
        edge,
        egui::StrokeKind::Inside,
    );
    let y = rect.center().y;
    ui.painter().text(
        egui::pos2(rect.left() + 8.0, y),
        egui::Align2::LEFT_CENTER,
        label,
        theme::mono(11.0),
        tokens.faint,
    );
    glyphst_ui::text::row(
        ui,
        egui::pos2(rect.left() + 60.0, y),
        value,
        theme::mono(12.0),
        tokens.text,
        rect.width() - 90.0,
    );
    icons::paint(
        ui,
        egui::Rect::from_center_size(egui::pos2(rect.right() - 14.0, y), egui::vec2(12.0, 12.0)),
        "chevrons-up-down",
        12.0,
        tokens.muted,
    );
    Some(response.on_hover_cursor(egui::CursorIcon::PointingHand))
}

/// The languages, each a menu item; `choose` gets the code clicked.
fn languages(ui: &mut egui::Ui, current: Option<&str>, mut choose: impl FnMut(String)) {
    egui::ScrollArea::vertical()
        .max_height(320.0)
        .show(ui, |ui| {
            for language in glyphst_translation::TARGET_LANGUAGES {
                let chosen = current.is_some_and(|code| code.eq_ignore_ascii_case(language.code));
                if ui_menu::Item::new(language.label)
                    .checked(chosen)
                    .show(ui)
                    .clicked()
                {
                    choose(language.code.to_string());
                }
            }
        });
}

/// `label            [switch]`; returns whether it flipped.
fn switch_row(ui: &mut egui::Ui, label: &str, on: &mut bool, enabled: bool) -> bool {
    let tokens = Tokens::get(ui.ctx());
    ui.horizontal(|ui| {
        ui.set_min_height(23.0);
        ui.label(
            egui::RichText::new(label)
                .font(theme::mono(12.0))
                .color(if enabled { tokens.text_2 } else { tokens.faint }),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_enabled(enabled, Switch(on)).changed()
        })
        .inner
    })
    .inner
}

/// A quoted passage with a coloured edge, its note under it and meta at
/// the foot (notes, pinned translations). The response is the card's.
fn quote_card(
    ui: &mut egui::Ui,
    tokens: &Tokens,
    edge: egui::Color32,
    quote: &str,
    body: &str,
    meta: &str,
) -> egui::Response {
    let inner = egui::Frame::new()
        .inner_margin(egui::Margin {
            left: 10,
            right: 4,
            top: 6,
            bottom: 8,
        })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 6.0;
            if !quote.is_empty() {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(format!("“{quote}”"))
                            .font(theme::regular(12.0))
                            .italics()
                            .color(tokens.muted),
                    )
                    .wrap(),
                );
            }
            if !body.is_empty() {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(body)
                            .font(theme::regular(13.0))
                            .color(tokens.text),
                    )
                    .wrap(),
                );
            }
            if !meta.is_empty() {
                ui.label(
                    egui::RichText::new(meta)
                        .font(theme::mono(10.0))
                        .color(tokens.faint),
                );
            }
        });
    let rect = inner.response.rect;
    let response = ui.interact(rect, ui.scope_id().with("card"), egui::Sense::click());
    if response.hovered() {
        ui.painter()
            .rect_filled(rect, 0, Tokens::fade(tokens.hover, 0.5));
    }
    ui.painter().rect_filled(
        egui::Rect::from_min_size(rect.min, egui::vec2(2.0, rect.height())),
        0,
        edge,
    );
    ui.add_space(6.0);
    response
}

/// A language's name from its code, or the code itself.
pub fn language_label(code: &str) -> &str {
    glyphst_translation::TARGET_LANGUAGES
        .iter()
        .find(|language| language.code.eq_ignore_ascii_case(code))
        .map_or(code, |language| language.label)
}

fn muted(ui: &mut egui::Ui, text: &str) {
    let tokens = Tokens::get(ui.ctx());
    ui.add(
        egui::Label::new(
            egui::RichText::new(text)
                .font(theme::mono(11.0))
                .color(tokens.muted),
        )
        .wrap(),
    );
}

/// The number shown beside a section's title.
pub fn count(tab: &DocumentTab, which: SidebarTab) -> Option<usize> {
    match which {
        SidebarTab::Outline => Some(tab.outline.len()),
        SidebarTab::Pages => Some(tab.layout.len()),
        SidebarTab::Bookmarks => Some(tab.annotations.bookmarks.len()),
        SidebarTab::Notes => Some(tab.annotations.items.len()),
        SidebarTab::Search => (!tab.find.query.is_empty()).then_some(tab.find.matches.len()),
        SidebarTab::Translation | SidebarTab::Effects | SidebarTab::Item => None,
    }
}

/// The count on a rail button: what the reader made or found.
pub fn badge(tab: &DocumentTab, which: SidebarTab) -> Option<usize> {
    match which {
        SidebarTab::Bookmarks | SidebarTab::Notes | SidebarTab::Search => {
            count(tab, which).filter(|&count| count > 0)
        }
        _ => None,
    }
}

/// A count as a badge shows it.
pub fn badge_label(count: usize) -> String {
    if count > 99 {
        "99+".to_owned()
    } else {
        count.to_string()
    }
}

/// The open section's pane header, `outline ──── 24`, and its actions.
pub fn header(
    ui: &mut egui::Ui,
    tab: &DocumentTab,
    which: SidebarTab,
    columns: PageColumns,
) -> Vec<SidebarAction> {
    let trailing = match which {
        SidebarTab::Translation => tab
            .translation
            .as_ref()
            .map(|session| format!("→{}", session.target.to_lowercase())),
        SidebarTab::Effects => Some("live".to_owned()),
        _ => count(tab, which).map(|count| count.to_string()),
    };
    let title = which.label().to_lowercase();
    let mut header = Header::new(&title).trailing(trailing.as_deref());
    let mut chosen = Vec::new();
    let following = tab.outline_view.following();
    match which {
        SidebarTab::Outline if !tab.outline.is_empty() => {
            let (icon, tip) = if following {
                ("locate-fixed", "stop following the current section")
            } else {
                ("locate", "follow the current section")
            };
            header = header.action(Action::new(icon, tip).selected(following));
            chosen.push(SidebarAction::LocateOutline);
        }
        SidebarTab::Pages => {
            let (icon, tip) = match columns {
                PageColumns::Two => ("rows-2", "one column"),
                PageColumns::One => ("layout-grid", "two columns"),
            };
            header = header.action(Action::new(icon, tip));
            chosen.push(SidebarAction::Command(Command::PageColumns));
            header = header.action(Action::new("maximize-2", "page overview"));
            chosen.push(SidebarAction::Command(Command::PageOverview));
        }
        SidebarTab::Search => {
            let (icon, tip) = if tab.overview.is_some() {
                ("minimize-2", "leave page overview")
            } else {
                ("maximize-2", "show matches in page overview")
            };
            header = header.action(Action::new(icon, tip));
            chosen.push(SidebarAction::Command(Command::PageOverview));
        }
        SidebarTab::Bookmarks => {
            header = header.action(Action::new("plus", "add a bookmark here"));
            chosen.push(SidebarAction::AddBookmark);
        }
        _ => {}
    }
    header = header.action(Action::new("x", "hide the sidebar"));
    chosen.push(SidebarAction::Close);
    header
        .show(ui)
        .clicked
        .map(|index| chosen.swap_remove(index))
        .into_iter()
        .collect()
}

/// Whether `text` holds `filter` (already lower-cased; empty holds all).
fn matches(filter: &str, text: &str) -> bool {
    filter.is_empty() || text.to_lowercase().contains(filter)
}

/// A list row of the sidebar.
struct Row<'a> {
    title: &'a str,
    /// Faint, at the right: a page number.
    detail: Option<String>,
    /// The outline's tree branches (`│  ├─ `) before the title.
    prefix: Option<String>,
    icon: Option<&'a str>,
    /// A colour dot before the title.
    dot: Option<egui::Color32>,
    /// The outline's expand and collapse toggle.
    disclosure: Disclosure,
    selected: bool,
    enabled: bool,
    /// Icon buttons (icon, tooltip) shown over the detail while hovered.
    actions: &'a [(&'a str, &'a str)],
    /// The height of the list's slots, when it sets one (the outline's).
    height: Option<f32>,
}

/// Whether a nested outline entry is open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Disclosure {
    /// A flat list: no toggles.
    None,
    /// No children.
    Leaf,
    Open,
    Closed,
}

/// Where a row's disclosure toggle is: over its tree branches.
fn disclosure_rect(ui: &egui::Ui, rect: egui::Rect, prefix: &str) -> egui::Rect {
    let font = theme::mono(12.0);
    let (ancestors, branch) = own_branch(prefix);
    let start = glyphst_ui::text::width(ui, ancestors, &font);
    // The branch and the `+` after it.
    let width = glyphst_ui::text::width(ui, branch, &font) + 10.0;
    egui::Rect::from_min_size(
        egui::pos2(rect.left() + 12.0 + start, rect.top()),
        egui::vec2(width, rect.height()),
    )
}

/// A tree prefix split into its ancestors' lines and the row's own branch
/// (`├─ ` or `└─ `), which alone toggles the entry: the rest of the row
/// opens it.
fn own_branch(prefix: &str) -> (&str, &str) {
    let start = prefix
        .char_indices()
        .rev()
        .nth(2)
        .map_or(0, |(index, _)| index);
    prefix.split_at(start)
}

/// Draws `row`; returns its response and the action button clicked.
fn row(ui: &mut egui::Ui, row: Row<'_>) -> (egui::Response, Option<usize>) {
    let tokens = Tokens::get(ui.ctx());
    let height = row.height.unwrap_or_else(|| tokens.row(28.0));
    let (rect, response) = glyphst_ui::row::list_row(ui, height, row.selected);
    let hovered = response.contains_pointer() && !row.actions.is_empty();
    let mut right = rect.right() - 12.0;
    let mut clicked = None;
    if hovered {
        let width = row.actions.len() as f32 * 24.0;
        let strip = egui::Rect::from_min_max(
            egui::pos2(rect.right() - 4.0 - width, rect.top()),
            egui::pos2(rect.right() - 4.0, rect.bottom()),
        );
        right = strip.left() - 4.0;
        ui.scope_builder(
            egui::UiBuilder::new()
                .max_rect(strip)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
            |ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                for (index, (icon, tip)) in row.actions.iter().enumerate() {
                    if icons::button(ui, icon, 24.0, false, tip).clicked() {
                        clicked = Some(index);
                    }
                }
            },
        );
    } else if let Some(detail) = row.detail {
        let color = if row.selected {
            tokens.accent_text
        } else {
            tokens.faint
        };
        let galley = ui
            .painter()
            .layout_no_wrap(detail, theme::mono(11.0), color);
        right -= galley.size().x;
        ui.painter().galley(
            egui::pos2(right, rect.center().y - galley.size().y / 2.0),
            galley,
            color,
        );
    }
    let y = rect.center().y;
    let mut left = rect.left() + 12.0;
    if let Some(prefix) = &row.prefix {
        let galley = ui
            .painter()
            .layout_no_wrap(prefix.clone(), theme::mono(12.0), tokens.faint);
        let width = galley.size().x;
        ui.painter().galley(
            egui::pos2(left, y - galley.size().y / 2.0),
            galley,
            tokens.faint,
        );
        left += width;
        // A closed entry shows that it holds more.
        if row.disclosure == Disclosure::Closed {
            ui.painter().text(
                egui::pos2(left, y),
                egui::Align2::LEFT_CENTER,
                "+",
                theme::mono(12.0),
                tokens.accent_text,
            );
            left += 10.0;
        }
    }
    if let Some(icon) = row.icon {
        icons::paint(
            ui,
            egui::Rect::from_center_size(egui::pos2(left + 6.5, y), egui::vec2(13.0, 13.0)),
            icon,
            13.0,
            if row.selected {
                tokens.accent_text
            } else {
                tokens.faint
            },
        );
        left += 21.0;
    }
    if let Some(dot) = row.dot {
        ui.painter().rect_filled(
            egui::Rect::from_center_size(egui::pos2(left + 4.0, y), egui::vec2(6.0, 6.0)),
            1,
            dot,
        );
        left += 16.0;
    }
    let color = if !row.enabled {
        tokens.faint
    } else if row.selected || response.contains_pointer() {
        tokens.text
    } else {
        tokens.text_2
    };
    glyphst_ui::text::row(
        ui,
        egui::pos2(left, y),
        row.title,
        theme::mono(12.0),
        color,
        right - left - 8.0,
    );
    (response.tip(row.title), clicked)
}

const OUTLINE_ROW: f32 = 26.0;

fn outline(
    ui: &mut egui::Ui,
    tab: &mut DocumentTab,
    filter: &str,
    actions: &mut Vec<SidebarAction>,
) {
    if tab.outline.is_empty() {
        muted(ui, "This document has no outline.");
        return;
    }
    let current = tab.current_outline_entry();
    let entries = &tab.outline;
    let view = &mut tab.outline_view;
    let filtering = !filter.is_empty();
    if view.list.shown(ui.ctx()) && !view.unfollowed {
        view.locate();
    }
    let changed = std::mem::replace(&mut view.current, current) != current;
    if view.following()
        && let Some(entry) = current
    {
        let expanded = !filtering && expand_ancestors(entries, &mut view.collapsed, entry);
        if changed || expanded {
            view.reveal.get_or_insert(false);
        }
    }
    let rows = visible_rows(entries, &view.collapsed, filter);
    if rows.is_empty() {
        muted(ui, "No matches.");
        return;
    }
    let nested = !filtering && entries.iter().any(|entry| entry.parent.is_some());
    let current_row = current.and_then(|entry| rows.iter().position(|&row| row == entry));
    // The tree branches of the rows listed (none while filtering).
    let shapes = if filtering {
        Vec::new()
    } else {
        let depths: Vec<usize> = rows.iter().map(|&index| entries[index].depth).collect();
        motif::tree_shape(&depths)
    };
    let prefix = |position: usize| {
        shapes
            .get(position)
            .map(|(ancestors, last)| motif::tree_prefix(ancestors, *last))
    };

    let mut scroll = egui::ScrollArea::vertical()
        .id_salt(("outline", tab.id))
        .auto_shrink(false);
    let mut target = None;
    if let Some(strict) = view.reveal.take()
        && let (Some(entry), Some(row)) = (current, current_row)
    {
        let sticky = if filtering {
            0
        } else {
            ancestors(entries, entry).len()
        };
        let viewport = view.list.viewport_or(ui);
        let content = rows.len() as f32 * OUTLINE_ROW;
        if let Some(offset) = reveal_offset(
            row,
            sticky,
            OUTLINE_ROW,
            content,
            viewport,
            view.list.offset,
            strict,
        ) {
            // Opening the list puts it there; following the reading
            // glides.
            if strict {
                scroll = scroll.vertical_scroll_offset(offset);
            } else {
                target = Some(offset);
            }
        }
    }
    let mut scrolled = target.is_some();
    if let Some(offset) = motion::scroll_glide(
        ui.ctx(),
        egui::Id::unique(("outline", tab.id)),
        view.list.offset,
        target,
    ) {
        scroll = scroll.vertical_scroll_offset(offset);
        scrolled = true;
    }

    let mut chosen = None;
    let mut toggled = None;
    let output = scroll.show_viewport(ui, |ui, viewport| {
        ui.set_height(rows.len() as f32 * OUTLINE_ROW);
        let origin = ui.max_rect().min;
        let width = ui.available_width();
        let row_rect = |y: f32| {
            egui::Rect::from_min_size(
                egui::pos2(origin.x, origin.y + y),
                egui::vec2(width, OUTLINE_ROW),
            )
        };
        let first = (viewport.min.y / OUTLINE_ROW).floor().max(0.0) as usize;
        let last = ((viewport.max.y / OUTLINE_ROW).ceil().max(0.0) as usize).min(rows.len());
        let disclosure = |index: usize, sticky: bool| {
            if !nested {
                Disclosure::None
            } else if !has_children(entries, index) {
                Disclosure::Leaf
            } else if !sticky && view.collapsed.contains(&index) {
                Disclosure::Closed
            } else {
                Disclosure::Open
            }
        };
        let stuck = if filtering {
            Vec::new()
        } else {
            stuck_rows(entries, &rows, first..last, viewport.min.y)
        };
        // Rows scroll under the sticky ones clipped at their bottom, so
        // nothing shows through them (the drawer may be translucent) and a
        // covered part takes no clicks.
        let band_bottom = origin.y + viewport.min.y + stuck.len() as f32 * OUTLINE_ROW;
        let mut draw = |ui: &mut egui::Ui, rect: egui::Rect, position: usize, sticky: bool| {
            let index = rows[position];
            let entry = &entries[index];
            let prefix = prefix(position);
            // Ids of the row's own, not of its slot in the rows shown, so
            // a row keeps its hover and fades when the list moves.
            let mut child = ui.new_child(
                egui::UiBuilder::new()
                    .scope_id(ui.scope_id().with(("outline-row", index, sticky)))
                    .max_rect(rect)
                    .layout(egui::Layout::top_down(egui::Align::Min)),
            );
            if !sticky {
                let clip = child.clip_rect();
                child.set_clip_rect(clip.intersect(below_band(rect, band_bottom)));
            }
            let disclosure = disclosure(index, sticky);
            let (response, _) = row(
                &mut child,
                Row {
                    title: &entry.title.to_lowercase(),
                    detail: entry.page.map(|page| (page + 1).to_string()),
                    prefix: prefix.clone(),
                    icon: None,
                    dot: None,
                    disclosure,
                    selected: current == Some(index),
                    enabled: entry.page.is_some(),
                    actions: &[],
                    height: Some(OUTLINE_ROW),
                },
            );
            // A toggle drawn over its row takes its clicks.
            if !sticky
                && matches!(disclosure, Disclosure::Open | Disclosure::Closed)
                && let Some(prefix) = &prefix
            {
                let toggle = child.interact(
                    disclosure_rect(&child, rect, prefix),
                    child.scope_id().with("disclosure"),
                    egui::Sense::click(),
                );
                if toggle.clicked() {
                    toggled = Some(index);
                    return;
                }
            }
            if response.clicked() && entry.page.is_some() {
                chosen = Some(index);
            }
        };
        for position in first..last {
            draw(ui, row_rect(position as f32 * OUTLINE_ROW), position, false);
        }
        for (slot, &index) in stuck.iter().enumerate() {
            let Some(position) = rows.iter().position(|&row| row == index) else {
                continue;
            };
            let rect = row_rect(viewport.min.y + slot as f32 * OUTLINE_ROW);
            // Sticky rows cover what scrolls under them.
            ui.painter()
                .rect_filled(rect, 0, Tokens::get(ui.ctx()).surface.to_opaque());
            draw(ui, rect, position, true);
        }
        if !stuck.is_empty() {
            ui.painter().hline(
                origin.x..=origin.x + width,
                band_bottom - 0.5,
                Tokens::get(ui.ctx()).hairline(),
            );
        }
    });

    let offset = output.state.clamped_offset().y;
    // Scrolling by hand stops following once the current row is out of
    // the list.
    if !scrolled
        && (offset - view.list.offset).abs() > 0.5
        && !row_is_visible(current_row, offset, output.inner_rect.height(), OUTLINE_ROW)
    {
        view.paused = true;
    }
    view.list.offset = offset;
    view.list.viewport = output.inner_rect.height();
    if let Some(index) = toggled
        && !view.collapsed.remove(&index)
    {
        view.collapsed.insert(index);
    }
    if let Some(index) = chosen {
        actions.push(SidebarAction::View(ViewAction::GoToEntry(index)));
        view.current = Some(index);
        // The row clicked is in view: follow from there without moving the
        // list (it glides only if the row is half under the sticky rows).
        if !view.unfollowed {
            view.paused = false;
            view.reveal = Some(false);
        }
    }
}

/// The entries that stick at the top of the outline (Zed's sticky scroll):
/// the parents of the top row of `shown` (indices into `rows`), scrolled
/// `scroll` points down.
fn stuck_rows(
    entries: &[OutlineEntry],
    rows: &[usize],
    shown: std::ops::Range<usize>,
    scroll: f32,
) -> Vec<usize> {
    if scroll <= 0.0 || shown.is_empty() {
        return Vec::new();
    }
    let first = shown.start;
    let depths: Vec<usize> = rows[shown]
        .iter()
        .map(|&index| entries[index].depth)
        .collect();
    let Some(anchor) = sticky_anchor(&depths, first) else {
        return Vec::new();
    };
    let mut stuck = ancestors(entries, rows[anchor.index]);
    if anchor.drifting && drift(anchor.index, stuck.len(), OUTLINE_ROW, scroll) < 0.0 {
        // The deepest parent goes as a whole, never half covered.
        stuck.pop();
    }
    stuck
}

/// The part of a scrolling row drawn and clickable: below the sticky rows,
/// whose band ends at `band_bottom`.
fn below_band(rect: egui::Rect, band_bottom: f32) -> egui::Rect {
    rect.with_min_y(rect.top().max(band_bottom).min(rect.bottom()))
}

/// The outline entries listed: the matches when filtering, else every
/// entry not inside a collapsed one.
fn visible_rows(entries: &[OutlineEntry], collapsed: &HashSet<usize>, filter: &str) -> Vec<usize> {
    if !filter.is_empty() {
        return (0..entries.len())
            .filter(|&index| matches(filter, &entries[index].title))
            .collect();
    }
    let mut rows = Vec::new();
    // Entries nest in reading order: a collapsed entry's children follow
    // it, deeper than it.
    let mut hidden_below = None;
    for (index, entry) in entries.iter().enumerate() {
        if hidden_below.is_some_and(|depth| entry.depth > depth) {
            continue;
        }
        hidden_below = collapsed.contains(&index).then_some(entry.depth);
        rows.push(index);
    }
    rows
}

fn has_children(entries: &[OutlineEntry], index: usize) -> bool {
    entries
        .get(index + 1)
        .is_some_and(|next| next.parent == Some(index))
}

/// The entries `index` is nested in, outermost first.
fn ancestors(entries: &[OutlineEntry], index: usize) -> Vec<usize> {
    let mut chain = Vec::new();
    let mut parent = entries.get(index).and_then(|entry| entry.parent);
    while let Some(index) = parent {
        // Parents come first, so a broken chain cannot loop.
        if chain.len() >= entries.len() {
            break;
        }
        chain.push(index);
        parent = entries.get(index).and_then(|entry| entry.parent);
    }
    chain.reverse();
    chain
}

/// Expands the entries `index` is nested in; true when one was collapsed.
fn expand_ancestors(
    entries: &[OutlineEntry],
    collapsed: &mut HashSet<usize>,
    index: usize,
) -> bool {
    let mut changed = false;
    for ancestor in ancestors(entries, index) {
        changed |= collapsed.remove(&ancestor);
    }
    changed
}

/// The row whose parents stick at the top of a list (Zed's sticky scroll,
/// as the GPUI Reader had it), found from the depths of the rows shown from
/// row `first`. A drifting anchor is the last child before an outdent: its
/// deepest parent leaves with it.
#[derive(Debug, PartialEq, Eq)]
struct StickyAnchor {
    index: usize,
    drifting: bool,
}

fn sticky_anchor(depths: &[usize], first: usize) -> Option<StickyAnchor> {
    for (ix, &depth) in depths.iter().enumerate() {
        if depth < ix {
            return Some(StickyAnchor {
                index: first + ix,
                drifting: false,
            });
        }
        if let Some(&next) = depths.get(ix + 1)
            && next + 1 == depth
            && (depth == ix || depth == ix + 1)
        {
            return Some(StickyAnchor {
                index: first + ix,
                drifting: depth == ix + 1,
            });
        }
    }
    None
}

/// How far a drifting anchor's deepest parent is pushed up (zero or less)
/// with `stuck` sticky rows at scroll offset `offset`.
fn drift(anchor: usize, stuck: usize, row: f32, offset: f32) -> f32 {
    let anchor_bottom = row * (anchor + 1) as f32;
    (anchor_bottom - offset - row * stuck as f32).min(0.0)
}

/// Whether any of row `row` shows at scroll offset `offset`.
fn row_is_visible(row: Option<usize>, offset: f32, viewport: f32, height: f32) -> bool {
    row.is_some_and(|row| {
        let top = row as f32 * height;
        top + height > offset && top < offset + viewport
    })
}

/// The scroll offset that centres row `row` below `sticky` sticky rows,
/// or `None` when it shows there already and `strict` is not asked. It
/// stays within `content`, the list's height: an offset past the end
/// would show the list scrolled too far for a frame.
fn reveal_offset(
    row: usize,
    sticky: usize,
    height: f32,
    content: f32,
    viewport: f32,
    offset: f32,
    strict: bool,
) -> Option<f32> {
    let top = row as f32 * height;
    let covered = sticky as f32 * height;
    if !strict && top >= offset + covered && top + height <= offset + viewport {
        return None;
    }
    let free = (viewport - covered).max(height);
    let end = (content - viewport).max(0.0);
    Some((top - covered - (free - height) / 2.0).clamp(0.0, end))
}

fn pages(
    ui: &mut egui::Ui,
    tab: &mut DocumentTab,
    columns: PageColumns,
    actions: &mut Vec<SidebarAction>,
) {
    let tokens = Tokens::get(ui.ctx());
    let ppp = ui.ctx().pixels_per_point();
    // Columns of thumbnails, their numbers under them; one column fills
    // the drawer with sharp renders (the overview's, so not while it
    // shows and asks for its own).
    let sharp = columns == PageColumns::One && tab.overview.is_none();
    let widest = if columns == PageColumns::One {
        280.0
    } else {
        160.0
    };
    let columns = columns.count();
    let gap = 16.0;
    let width = ((ui.available_width() - gap * (columns as f32 + 1.0)) / columns as f32)
        .clamp(48.0, widest);
    let [page_width, page_height] = tab.layout.page_size(0);
    let box_height = width * (page_height / page_width.max(1.0)) as f32;
    let row_height = box_height + 34.0;
    let current = tab.current_page();
    let count = tab.layout.len();
    let rows = count.div_ceil(columns);
    let mut wanted = Vec::new();
    let mut shown = 0..0;
    // The current page's thumbnail is brought into view when the page
    // changes, centred when the list opens.
    let view = &mut tab.outline_view;
    let regrid = std::mem::replace(&mut view.page_columns, columns) != columns;
    let fresh = view.pages.shown(ui.ctx()) || regrid;
    let mut scroll = egui::ScrollArea::vertical()
        .id_salt(("pages", tab.id))
        .auto_shrink(false);
    let mut target = None;
    if fresh || view.page != Some(current) {
        view.page = Some(current);
        let viewport = view.pages.viewport_or(ui);
        // `show_rows` puts the item spacing between rows.
        let stride = row_height + ui.spacing().item_spacing.y;
        if let Some(offset) = reveal_offset(
            current / columns,
            0,
            stride,
            rows as f32 * stride - ui.spacing().item_spacing.y,
            viewport,
            view.pages.offset,
            fresh,
        ) {
            if fresh {
                scroll = scroll.vertical_scroll_offset(offset);
            } else {
                target = Some(offset);
            }
        }
    }
    if let Some(offset) = motion::scroll_glide(
        ui.ctx(),
        egui::Id::unique(("pages", tab.id)),
        view.pages.offset,
        target,
    ) {
        scroll = scroll.vertical_scroll_offset(offset);
    }
    let output = scroll.show_rows(ui, row_height, rows, |ui, range| {
        ui.spacing_mut().item_spacing.y = 0.0;
        shown = range.start * columns..(range.end * columns).min(count);
        for row_index in range {
            let (row_rect, _) = ui.allocate_exact_size(
                egui::vec2(ui.available_width(), row_height),
                egui::Sense::hover(),
            );
            for column in 0..columns {
                let page = row_index * columns + column;
                if page >= count {
                    break;
                }
                let cell = egui::Rect::from_min_size(
                    egui::pos2(
                        row_rect.left() + gap + column as f32 * (width + gap),
                        row_rect.top() + 10.0,
                    ),
                    egui::vec2(width, box_height),
                );
                let [w, h] = tab.layout.page_size(page);
                let scale = (width / w as f32).min(box_height / h as f32);
                let paper = egui::Rect::from_center_size(
                    cell.center(),
                    egui::vec2(w as f32 * scale, h as f32 * scale),
                );
                let response = ui.interact(
                    paper,
                    ui.scope_id().with(("thumb", page)),
                    egui::Sense::click(),
                );
                let painter = ui.painter();
                painter.rect_filled(paper, 0, tokens.page);
                let texture = if sharp {
                    crate::overview::page_texture(tab, page, paper, ppp, &mut wanted)
                } else {
                    tab.thumbnail(page).map(|(id, _)| id)
                };
                if let Some(texture) = texture {
                    glyphst_ui::paint::paint_turned_image(
                        painter,
                        texture,
                        paper,
                        tab.layout.arrangement.rotation,
                    );
                } else if tab.dense_pages.contains(&page) {
                    painter.text(
                        paper.center(),
                        egui::Align2::CENTER_CENTER,
                        "dense page",
                        theme::mono(10.0),
                        tokens.faint,
                    );
                } else {
                    glyphst_ui::thumb::paint_placeholder_lines(ui, paper);
                }
                let is_current = page == current;
                if is_current {
                    painter.rect_stroke(
                        paper.expand(1.0),
                        0,
                        egui::Stroke::new(2.0, tokens.accent),
                        egui::StrokeKind::Outside,
                    );
                } else if response.hovered() {
                    painter.rect_stroke(paper, 0, tokens.edge(), egui::StrokeKind::Outside);
                }
                let label = if is_current {
                    format!("{} ● current", page + 1)
                } else {
                    (page + 1).to_string()
                };
                painter.text(
                    egui::pos2(paper.center().x, paper.bottom() + 6.0),
                    egui::Align2::CENTER_TOP,
                    label,
                    theme::mono(10.0),
                    if is_current {
                        tokens.accent_text
                    } else {
                        tokens.faint
                    },
                );
                if response.clicked() {
                    actions.push(SidebarAction::View(ViewAction::GoToPage(page)));
                }
            }
        }
    });
    tab.outline_view.pages.offset = output.state.clamped_offset().y;
    tab.outline_view.pages.viewport = output.inner_rect.height();
    let ahead = shown.start.saturating_sub(4)..(shown.end + 4).min(count);
    tab.want_thumbnails(ahead);
    if sharp {
        tab.want_grid_images(wanted);
    }
}

fn bookmarks(
    ui: &mut egui::Ui,
    tab: &DocumentTab,
    state: &mut SidebarState,
    info: &SidebarInfo<'_>,
    filter: &str,
    actions: &mut Vec<SidebarAction>,
) {
    let tokens = Tokens::get(ui.ctx());
    let title = |bookmark: &glyphst_library::Bookmark| {
        bookmark
            .title
            .clone()
            .unwrap_or_else(|| format!("page {}", bookmark.position.page.max(1)))
            .to_lowercase()
    };
    let footer = 32.0;
    let list_height = (ui.available_height() - footer).max(40.0);
    egui::ScrollArea::vertical()
        .auto_shrink(false)
        .max_height(list_height)
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            motif::rule_title(ui, "this document", None);
            if tab.annotations.bookmarks.is_empty() {
                muted(ui, "no bookmarks yet.");
            }
            for bookmark in &tab.annotations.bookmarks {
                let name = title(bookmark);
                if !matches(filter, &name) {
                    continue;
                }
                if let Some((id, text)) = &mut state.bookmark
                    && *id == bookmark.id
                {
                    let response = glyphst_ui::text::fitted(ui, |ui| {
                        ui.add(
                            egui::TextEdit::singleline(text)
                                .font(theme::mono(12.0))
                                .margin(glyphst_ui::text::FIELD_MARGIN)
                                .desired_width(f32::INFINITY),
                        )
                    });
                    if std::mem::take(&mut state.focus_rename) {
                        response.request_focus();
                    }
                    if response.lost_focus() {
                        if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            actions.push(SidebarAction::RenameBookmark(*id, text.clone()));
                        }
                        state.bookmark = None;
                    }
                    continue;
                }
                let page = bookmark.position.page.max(1);
                let active = tab.current_page() + 1 == page;
                let (response, clicked) = ui
                    .push_id(bookmark.id, |ui| {
                        row(
                            ui,
                            Row {
                                title: &name,
                                detail: Some(format!("p.{page}")),
                                prefix: None,
                                icon: Some("bookmark"),
                                dot: None,
                                disclosure: Disclosure::None,
                                selected: active,
                                enabled: true,
                                actions: &[("pen-line", "rename"), ("trash-2", "delete")],
                                height: None,
                            },
                        )
                    })
                    .inner;
                match clicked {
                    Some(0) => {
                        state.bookmark = Some((bookmark.id, name.clone()));
                        state.focus_rename = true;
                    }
                    Some(_) => actions.push(SidebarAction::RemoveBookmark(bookmark.id)),
                    None if response.clicked() => {
                        actions.push(SidebarAction::GoToBookmark(bookmark.id));
                    }
                    None => {}
                }
            }
            let others: Vec<&OtherBookmark> = info
                .other_bookmarks
                .iter()
                .filter(|other| matches(filter, &other.title) || matches(filter, &other.document))
                .collect();
            if !others.is_empty() {
                ui.add_space(6.0);
                motif::rule_title(ui, "other documents", None);
                for other in others {
                    let name = format!("{} · {}", other.document, other.title).to_lowercase();
                    let response = ui
                        .push_id(("other", other.id), |ui| {
                            ListRow::new(&name)
                                .icon("bookmark")
                                .meta(other.place.as_str())
                                .show(ui)
                        })
                        .inner;
                    if response.clicked() {
                        actions.push(SidebarAction::OpenBookmark(other.id));
                    }
                }
            }
        });
    // `add bookmark  [b]` at the foot.
    let bottom = ui.max_rect().bottom();
    let foot = egui::Rect::from_min_max(
        egui::pos2(ui.max_rect().left(), bottom - footer),
        egui::pos2(ui.max_rect().right(), bottom),
    );
    let mut foot_ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(foot)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    let response = foot_ui.add(
        Button::plain("add bookmark")
            .shortcut(&glyphst_ui::key::display(&info.bookmark_key))
            .small(),
    );
    if response.clicked() {
        actions.push(SidebarAction::AddBookmark);
    }
    let _ = tokens;
}

/// When `seconds` (since the epoch) was, as the notes write it: `today
/// 09:51`, `yesterday`, `mon`, or `oct 02`.
pub fn when(seconds: u64) -> String {
    use chrono::{Local, TimeZone};
    let Some(time) = Local.timestamp_opt(seconds as i64, 0).single() else {
        return String::new();
    };
    let today = Local::now().date_naive();
    let days = (today - time.date_naive()).num_days();
    match days {
        0 => format!("today {}", time.format("%H:%M")),
        1 => "yesterday".to_owned(),
        2..=6 => time.format("%a").to_string().to_lowercase(),
        _ => time.format("%b %d").to_string().to_lowercase(),
    }
}

fn notes(
    ui: &mut egui::Ui,
    tab: &DocumentTab,
    state: &mut SidebarState,
    filter: &str,
    actions: &mut Vec<SidebarAction>,
) {
    let tokens = Tokens::get(ui.ctx());
    if tab.annotations.items.is_empty() {
        muted(
            ui,
            "no highlights or notes yet. select text and right-click to add one.",
        );
        return;
    }
    let is_note = |item: &annotations::Item| matches!(item.id, AnnotationId::Note(_));
    let total = tab.annotations.items.len();
    let notes_count = tab
        .annotations
        .items
        .iter()
        .filter(|item| is_note(item))
        .count();
    let all = format!("all {total}");
    let highlights = format!("highlights {}", total - notes_count);
    let notes_label = format!("notes {notes_count}");
    segmented::labels(
        ui,
        &mut state.notes_filter,
        &[
            (NotesFilter::All, all.as_str()),
            (NotesFilter::Highlights, highlights.as_str()),
            (NotesFilter::Notes, notes_label.as_str()),
        ],
    );
    ui.add_space(8.0);
    let kind = state.notes_filter;
    let shown = |item: &annotations::Item| {
        let kind_matches = match kind {
            NotesFilter::All => true,
            NotesFilter::Highlights => !is_note(item),
            NotesFilter::Notes => is_note(item),
        };
        kind_matches && (matches(filter, &item.text) || matches(filter, &item.body))
    };
    if !tab.annotations.items.iter().any(shown) {
        muted(ui, "no matches.");
        return;
    }
    let reflowed = tab.is_reflowed();
    egui::ScrollArea::vertical()
        .auto_shrink(false)
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for item in tab.annotations.items.iter().filter(|item| shown(item)) {
                let place = tab.annotations.page_of(item, reflowed);
                let open = state.open_note == Some(item.id);
                let quote: String = item.text.chars().take(QUOTE_CHARS).collect();
                let page = match place {
                    Some((page, _)) => format!("p.{}", page + 1),
                    None => "–".to_owned(),
                };
                let meta = format!("{page} · {}", when(item.created));
                let edge = if is_note(item) {
                    tokens.accent
                } else {
                    annotations::color(item.color)
                };
                let response = ui
                    .push_id(item.id, |ui| {
                        quote_card(ui, &tokens, edge, &quote, &item.body, &meta)
                    })
                    .inner;
                if response.clicked() {
                    state.open_note = (!open).then_some(item.id);
                    if let Some((page, y)) = place {
                        actions.push(SidebarAction::View(ViewAction::GoTo { page, y: Some(y) }));
                    }
                }
                if open {
                    note_details(ui, &tokens, item, place.is_some(), state, actions);
                }
            }
        });
}

/// An open note under its card: editing, its colours and its actions.
fn note_details(
    ui: &mut egui::Ui,
    tokens: &Tokens,
    item: &annotations::Item,
    placed: bool,
    state: &mut SidebarState,
    actions: &mut Vec<SidebarAction>,
) {
    egui::Frame::new()
        .inner_margin(egui::Margin {
            left: 10,
            right: 4,
            top: 0,
            bottom: 10,
        })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 6.0;
            if !placed {
                muted(ui, "not found in this layout.");
            }
            if let Some((id, body)) = &mut state.note
                && *id == item.id
            {
                glyphst_ui::text::fitted(ui, |ui| {
                    ui.add(
                        egui::TextEdit::multiline(body)
                            .margin(glyphst_ui::text::FIELD_MARGIN)
                            .desired_rows(3)
                            .desired_width(f32::INFINITY)
                            .hint_text("note"),
                    )
                });
                let (save, cancel) = ui
                    .horizontal(|ui| {
                        (
                            ui.add(Button::primary("save")).clicked(),
                            ui.add(Button::ghost("cancel")).clicked(),
                        )
                    })
                    .inner;
                if save {
                    actions.push(SidebarAction::SetBody(*id, body.clone()));
                }
                if save || cancel {
                    state.note = None;
                }
            }
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                for color in HighlightColor::ALL {
                    let (rect, response) =
                        ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::click());
                    ui.painter()
                        .rect_filled(rect.shrink(1.0), 1, annotations::color(color));
                    if color == item.color {
                        ui.painter().rect_stroke(
                            rect.expand(1.0),
                            1,
                            egui::Stroke::new(1.0, tokens.text),
                            egui::StrokeKind::Outside,
                        );
                    }
                    if response.tip(annotations::color_name(color)).clicked() {
                        actions.push(SidebarAction::SetColor(item.id, color));
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    if ui.add(Button::bracket_muted("delete")).clicked() {
                        actions.push(SidebarAction::Remove(item.id));
                    }
                    if matches!(item.id, AnnotationId::Note(_))
                        && ui.add(Button::bracket("edit")).clicked()
                    {
                        state.note = Some((item.id, item.body.clone()));
                    }
                });
            });
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_page_map_shrinks_its_cells_to_fit_a_book() {
        // A paper's pages are large cells on a row or two…
        assert_eq!(map_cells(12, 276.0), (18.0, 3.0, 13));
        // …a book's shrink so the map stays within its rows.
        let (cell, _, per_row) = map_cells(530, 276.0);
        assert!(530_usize.div_ceil(per_row) <= MAP_ROWS);
        assert!(cell >= 6.0, "{cell}");
        let (cell, _, per_row) = map_cells(5_000, 276.0);
        assert!(cell >= 3.0 && per_row > 0);
    }

    #[test]
    fn the_pages_section_stays_within_the_pane() {
        // A book mid-run, with every count in the summary and the longest
        // rows the list shows.
        let mut states = vec![PageProgress::Done; 530];
        for state in &mut states[100..112] {
            *state = PageProgress::Partial {
                done: 13,
                total: 24,
            };
        }
        for state in &mut states[200..230] {
            *state = PageProgress::Waiting;
        }
        states[311] = PageProgress::Failed(PageFailureCode::LocalOcrMissing);
        let width = 276.0;
        let ctx = egui::Context::default();
        let mut used = 0.0;
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(400.0, 800.0),
            )),
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| {
            let mut pane = ui.new_child(egui::UiBuilder::new().max_rect(
                egui::Rect::from_min_size(egui::pos2(12.0, 0.0), egui::vec2(width, 800.0)),
            ));
            progress_summary(&mut pane, &states, 120);
            page_map(&mut pane, &states, 140, |page| page.to_string());
            map_legend(&mut pane);
            page_status_row(&mut pane, 528, 528, PageProgress::Done, true, None, 12);
            page_status_row(&mut pane, 101, 101, states[101], false, None, 0);
            page_status_row(&mut pane, 311, 311, states[311], false, None, 0);
            used = pane.min_rect().width();
        });
        output.textures_delta.clear();
        assert!(used <= width, "the pages section is {used} wide");
    }

    #[test]
    fn turning_following_off_keeps_the_outline_where_it_is() {
        let mut tab = crate::document_tab::test_tab(60);
        tab.outline = (0..60)
            .map(|page| crate::document::OutlineEntry {
                title: format!("section {page}"),
                page: Some(page),
                y: None,
                depth: 0,
                parent: None,
            })
            .collect();
        let ctx = egui::Context::default();
        let draw = |tab: &mut DocumentTab| {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(300.0, 400.0),
                )),
                ..Default::default()
            };
            let mut output = ctx.run_ui(input, |ui| super::outline(ui, tab, "", &mut Vec::new()));
            output.textures_delta.clear();
        };
        assert!(tab.outline_view.following());
        tab.outline_view.set_follow(false);
        // Reading on to section 50 leaves the list at the top.
        tab.view.origin[1] = tab.layout.offsets[50][1];
        for _ in 0..3 {
            draw(&mut tab);
        }
        assert_eq!(tab.current_outline_entry(), Some(50));
        assert!(!tab.outline_view.following());
        assert_eq!(tab.outline_view.list.offset, 0.0);
        // Turned on again, it scrolls to the section being read.
        tab.outline_view.set_follow(true);
        tab.outline_view.locate();
        for _ in 0..3 {
            draw(&mut tab);
        }
        assert!(tab.outline_view.following());
        assert!(tab.outline_view.list.offset > 0.0);
    }

    #[test]
    fn choosing_a_section_leaves_the_outline_where_it_is() {
        let mut tab = crate::document_tab::test_tab(60);
        tab.outline = (0..60)
            .map(|page| crate::document::OutlineEntry {
                title: format!("section {page}"),
                page: Some(page),
                y: None,
                depth: 0,
                parent: None,
            })
            .collect();
        let ctx = egui::Context::default();
        let draw = |tab: &mut DocumentTab, events: Vec<egui::Event>| {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(300.0, 400.0),
                )),
                events,
                ..Default::default()
            };
            let mut actions = Vec::new();
            let mut output = ctx.run_ui(input, |ui| super::outline(ui, tab, "", &mut actions));
            output.textures_delta.clear();
            actions
        };
        for _ in 0..3 {
            draw(&mut tab, Vec::new());
        }
        let offset = tab.outline_view.list.offset;
        // Section 12 shows near the bottom of the list, far from its centre.
        let pos = egui::pos2(100.0, 12.5 * OUTLINE_ROW - offset);
        let button = |pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        draw(&mut tab, vec![egui::Event::PointerMoved(pos)]);
        draw(&mut tab, vec![button(true)]);
        let actions = draw(&mut tab, vec![button(false)]);
        assert!(matches!(
            actions.as_slice(),
            [SidebarAction::View(ViewAction::GoToEntry(12))]
        ));
        tab.apply(ViewAction::GoToEntry(12), false);
        for _ in 0..3 {
            draw(&mut tab, Vec::new());
        }
        assert_eq!(tab.current_outline_entry(), Some(12));
        assert!(tab.outline_view.following());
        assert_eq!(tab.outline_view.list.offset, offset);
    }

    #[test]
    fn only_the_branch_of_a_row_toggles_it() {
        let prefix = motif::tree_prefix(&[false, true, false], false);
        assert_eq!(own_branch(&prefix), ("│     │  ", "├─ "));
        assert_eq!(own_branch("└─ "), ("", "└─ "));
    }

    #[test]
    fn filters_match_ignoring_case() {
        assert!(matches("", "Anything"));
        assert!(matches("intro", "1.1 Introduction"));
        assert!(!matches("intro", "Chapter 2"));
    }

    #[test]
    fn badges_cap_at_99() {
        assert_eq!(badge_label(7), "7");
        assert_eq!(badge_label(99), "99");
        assert_eq!(badge_label(100), "99+");
    }

    fn entry(depth: usize, parent: Option<usize>) -> OutlineEntry {
        OutlineEntry {
            title: format!("entry {depth}"),
            page: Some(0),
            y: None,
            depth,
            parent,
        }
    }

    /// An outline from its entries' depths, in reading order.
    fn outline(depths: &[usize]) -> Vec<OutlineEntry> {
        let mut entries: Vec<OutlineEntry> = Vec::new();
        for &depth in depths {
            let parent = entries.iter().rposition(|entry| entry.depth + 1 == depth);
            entries.push(entry(depth, parent.filter(|_| depth > 0)));
        }
        entries
    }

    #[test]
    fn rows_under_the_sticky_parents_show_only_below_them() {
        // A chapter with a long section: scrolled into the section's
        // children, the chapter and the section stick.
        let entries = outline(&[0, 1, 2, 2, 2, 2, 2, 2, 2, 2]);
        let rows: Vec<usize> = (0..entries.len()).collect();
        let scroll = 3.5 * OUTLINE_ROW;
        let first = 3;
        let stuck = stuck_rows(&entries, &rows, first..rows.len(), scroll);
        assert_eq!(stuck, [0, 1]);
        assert!(stuck_rows(&entries, &rows, 0..rows.len(), 0.0).is_empty());

        let band_bottom = scroll + stuck.len() as f32 * OUTLINE_ROW;
        for row in first..rows.len() {
            let rect = egui::Rect::from_min_size(
                egui::pos2(0.0, row as f32 * OUTLINE_ROW),
                egui::vec2(200.0, OUTLINE_ROW),
            );
            let shown = below_band(rect, band_bottom);
            assert!(shown.top() >= band_bottom - f32::EPSILON || shown.height() == 0.0);
            assert!(shown.bottom() == rect.bottom() && shown.height() >= 0.0);
        }
        // A row wholly under the band shows nothing; one below it, all.
        let covered =
            egui::Rect::from_min_size(egui::pos2(0.0, scroll), egui::vec2(200.0, OUTLINE_ROW));
        assert_eq!(below_band(covered, band_bottom).height(), 0.0);
        let free = covered.translate(egui::vec2(0.0, 3.0 * OUTLINE_ROW));
        assert_eq!(below_band(free, band_bottom), free);
    }

    #[test]
    fn ancestors_are_outermost_first_without_a_depth_cap() {
        let entries = outline(&(0..12).collect::<Vec<_>>());
        assert_eq!(ancestors(&entries, 11), (0..11).collect::<Vec<_>>());
        assert!(ancestors(&entries, 0).is_empty());
    }

    #[test]
    fn following_expands_only_the_current_entrys_ancestors() {
        let entries = outline(&[0, 1, 2, 0]);
        let mut collapsed = HashSet::from([0, 1, 3]);
        assert!(expand_ancestors(&entries, &mut collapsed, 2));
        assert_eq!(collapsed, HashSet::from([3]));
        assert!(!expand_ancestors(&entries, &mut collapsed, 2));
    }

    #[test]
    fn collapsed_entries_hide_their_children() {
        let entries = outline(&[0, 1, 2, 1, 0, 1]);
        assert_eq!(
            visible_rows(&entries, &HashSet::new(), ""),
            vec![0, 1, 2, 3, 4, 5]
        );
        assert_eq!(
            visible_rows(&entries, &HashSet::from([1]), ""),
            vec![0, 1, 3, 4, 5]
        );
        assert_eq!(
            visible_rows(&entries, &HashSet::from([0, 2]), ""),
            vec![0, 4, 5]
        );
        let parents: Vec<usize> = (0..entries.len())
            .filter(|&index| has_children(&entries, index))
            .collect();
        assert_eq!(parents, vec![0, 1, 4]);
    }

    #[test]
    fn finds_stationary_anchor_after_visible_ancestors() {
        assert_eq!(
            sticky_anchor(&[3, 3, 1], 8),
            Some(StickyAnchor {
                index: 10,
                drifting: false
            })
        );
    }

    #[test]
    fn marks_last_parent_as_drifting_at_an_outdent() {
        assert_eq!(
            sticky_anchor(&[1, 2, 1], 4),
            Some(StickyAnchor {
                index: 5,
                drifting: true
            })
        );
    }

    #[test]
    fn omits_sticky_anchor_at_the_top_level() {
        assert_eq!(sticky_anchor(&[0, 1, 2], 0), None);
    }

    #[test]
    fn sticky_rows_stack_and_the_deepest_leaves_whole_at_an_outdent() {
        // Rows of 20 points, three ancestors of row 5 stuck above it.
        let depths = [0, 1, 2, 3, 3, 3, 2, 1, 0, 0, 0, 0];
        let shown = |offset: f32| {
            let first = (offset / 20.0) as usize;
            let anchor = sticky_anchor(&depths[first..first + 5], first).unwrap();
            // Each level of nesting is one parent.
            let stuck = depths[anchor.index];
            let leaves = anchor.drifting && drift(anchor.index, stuck, 20.0, offset) < 0.0;
            (anchor.index, stuck - usize::from(leaves))
        };
        assert_eq!(shown(60.0), (5, 3));
        assert_eq!(shown(61.0), (5, 2));
    }

    #[test]
    fn scrolling_by_hand_keeps_following_while_the_row_partly_shows() {
        let (top, bottom) = (56.0, 84.0);
        assert!(row_is_visible(Some(2), bottom - 1.0, 28.0, 28.0));
        assert!(!row_is_visible(Some(2), bottom, 28.0, 28.0));
        assert!(row_is_visible(Some(2), top - 28.0 + 1.0, 28.0, 28.0));
        assert!(!row_is_visible(Some(2), top - 28.0, 28.0, 28.0));
        assert!(!row_is_visible(None, 0.0, 28.0, 28.0));
    }

    #[test]
    fn reveals_centre_below_the_sticky_rows() {
        // 100 rows of 10 in a 100-point list scrolled to 0.
        let reveal = |row, sticky, offset, strict| {
            reveal_offset(row, sticky, 10.0, 1000.0, 100.0, offset, strict)
        };
        assert_eq!(reveal(3, 0, 0.0, false), None);
        assert_eq!(reveal(3, 0, 0.0, true), Some(0.0));
        // Row 50 is centred: its top 45 points down.
        assert_eq!(reveal(50, 0, 0.0, false), Some(455.0));
        // Under two sticky rows, row 50 centres in the 80 points left.
        assert_eq!(reveal(50, 2, 0.0, false), Some(445.0));
        // A row hidden under the sticky rows is revealed.
        assert_eq!(reveal(41, 2, 400.0, false), Some(355.0));
        assert_eq!(reveal(42, 2, 400.0, false), None);
        // The last rows can't be centred: the list stops at its end.
        assert_eq!(reveal(99, 0, 0.0, true), Some(900.0));
        assert_eq!(reveal(97, 0, 0.0, false), Some(900.0));
    }

    #[test]
    fn sidebar_tabs_parse_by_name() {
        assert_eq!(SidebarTab::parse("pages"), Some(SidebarTab::Pages));
        assert_eq!(SidebarTab::parse("Search"), Some(SidebarTab::Search));
        assert_eq!(SidebarTab::parse("nope"), None);
    }
}
