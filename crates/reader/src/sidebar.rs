//! The document sidebar, after pdfcraft's side panels (artcraft,
//! `ui-egui/src/panels.rs`, `comments_panel.rs`, `search_ui.rs`): outline,
//! page thumbnails, bookmarks, highlights and notes, and search results.

use std::{collections::HashMap, time::Instant};

use glyphst_library::HighlightColor;
use serde::{Deserialize, Serialize};

use crate::{
    annotations::{self, AnnotationId},
    document_tab::{DocumentTab, ViewAction},
    find, icons,
    theme::{self, Tokens},
    translation::{Export, SessionStatus},
    widgets,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SidebarTab {
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
    pub const ALL: [Self; 7] = [
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
    CancelTranslation,
    ExportTranslation,
    ShowTranslated(bool),
    Unpin(usize),
}

/// What the Translation panel shows besides the tab's session.
pub struct TranslationInfo<'a> {
    pub target: Option<String>,
    pub pinned: &'a [(String, String)],
    pub warnings: &'a [String],
    /// The settings and keys are loaded.
    pub ready: bool,
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
}

const QUOTE_CHARS: usize = 240;

pub fn show(
    ui: &mut egui::Ui,
    tab: &mut DocumentTab,
    which: &mut SidebarTab,
    state: &mut SidebarState,
    translation: &TranslationInfo<'_>,
    now: Instant,
) -> Vec<SidebarAction> {
    let mut actions = Vec::new();
    // Lists with entries can be filtered.
    let hint = match which {
        SidebarTab::Outline if !tab.outline.is_empty() => Some("Filter the outline"),
        SidebarTab::Bookmarks if !tab.annotations.bookmarks.is_empty() => Some("Filter bookmarks"),
        SidebarTab::Notes if !tab.annotations.items.is_empty() => {
            Some("Filter highlights and notes")
        }
        _ => None,
    };
    let filter = match hint {
        Some(hint) => {
            let text = state.filters.entry(*which).or_default();
            widgets::filter_field(ui, text, hint);
            ui.add_space(6.0);
            text.trim().to_lowercase()
        }
        None => String::new(),
    };
    match which {
        SidebarTab::Outline => outline(ui, tab, &filter, &mut actions),
        SidebarTab::Pages => pages(ui, tab, &mut actions),
        SidebarTab::Bookmarks => bookmarks(ui, tab, state, &filter, &mut actions),
        SidebarTab::Notes => notes(ui, tab, state, &filter, &mut actions),
        // Choosing a match makes it current; the view brings it in.
        SidebarTab::Search => {
            find::search_panel(&mut tab.find, ui, now);
        }
        SidebarTab::Translation => translation_panel(ui, tab, translation, &mut actions),
        SidebarTab::Effects => {}
    }
    actions
}

fn translation_panel(
    ui: &mut egui::Ui,
    tab: &DocumentTab,
    info: &TranslationInfo<'_>,
    actions: &mut Vec<SidebarAction>,
) {
    let language = info
        .target
        .as_deref()
        .map(language_label)
        .unwrap_or("the settings' language");
    let is_pdf = tab
        .path
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("pdf"));
    ui.strong("Document");
    match &tab.translation {
        None if !is_pdf => muted(ui, "Only PDFs can be translated as documents."),
        None => {
            muted(
                ui,
                &format!(
                    "Translate this PDF into {language}; translated pages replace the originals as they are ready."
                ),
            );
            let start = ui.add_enabled(info.ready, egui::Button::new("Translate document"));
            if start.clicked() {
                actions.push(SidebarAction::StartTranslation);
            }
            if !info.ready {
                muted(ui, "Reading the translation settings…");
            }
        }
        Some(session) => {
            ui.label(session.status_line());
            if let Some(progress) = &session.progress
                && let (Some(done), Some(total)) = (progress.completed, progress.total)
                && total > 0
                && session.is_running()
            {
                ui.add(egui::ProgressBar::new(done as f32 / total as f32).desired_height(6.0));
            }
            if session.page_count > 0 {
                muted(
                    ui,
                    &format!(
                        "{} of {} pages translated",
                        session.previews.len(),
                        session.page_count
                    ),
                );
            }
            if !session.failures.is_empty() {
                muted(
                    ui,
                    &format!("{} pages could not be translated", session.failures.len()),
                );
            }
            let mut shown = session.show_translated;
            if widgets::segmented(ui, &mut shown, &[(false, "Original"), (true, "Translated")]) {
                actions.push(SidebarAction::ShowTranslated(shown));
            }
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                if session.is_running() {
                    if widgets::pill_button(ui, Some("x"), "Cancel", false).clicked() {
                        actions.push(SidebarAction::CancelTranslation);
                    }
                } else if widgets::pill_button(ui, Some("refresh-cw"), "Translate again", false)
                    .clicked()
                {
                    actions.push(SidebarAction::StartTranslation);
                }
                let can_export = session.status == SessionStatus::Finished
                    && !matches!(session.export, Some(Export::Running(_)));
                if ui
                    .add_enabled_ui(can_export, |ui| {
                        widgets::pill_button(ui, Some("file-down"), "Export PDF…", false)
                    })
                    .inner
                    .clicked()
                {
                    actions.push(SidebarAction::ExportTranslation);
                }
            });
            match &session.export {
                Some(Export::Running(_)) => muted(ui, "Exporting…"),
                Some(Export::Done(Ok(path))) => muted(ui, &format!("Saved {}", path.display())),
                Some(Export::Done(Err(error))) => muted(ui, &format!("Export failed: {error}")),
                None => {}
            }
        }
    }
    for warning in info.warnings {
        muted(ui, warning);
    }
    widgets::section_title(ui, "Pinned");
    if info.pinned.is_empty() {
        muted(
            ui,
            "Translate a selection (right-click it) and pin it to keep it here.",
        );
        return;
    }
    let tokens = Tokens::get(ui.ctx());
    egui::ScrollArea::vertical()
        .auto_shrink(false)
        .show(ui, |ui| {
            for (index, (source, translated)) in info.pinned.iter().enumerate() {
                egui::Frame::new()
                    .fill(tokens.card)
                    .stroke(egui::Stroke::new(1.0, tokens.divider))
                    .corner_radius(8)
                    .inner_margin(10)
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        let quote: String = source.chars().take(QUOTE_CHARS).collect();
                        ui.label(
                            egui::RichText::new(quote)
                                .italics()
                                .color(tokens.text_muted),
                        );
                        ui.label(translated);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.spacing_mut().item_spacing.x = 2.0;
                            ui.push_id(index, |ui| {
                                if icons::button(ui, "trash-2", 24.0, false, "Remove").clicked() {
                                    actions.push(SidebarAction::Unpin(index));
                                }
                                if icons::button(ui, "copy", 24.0, false, "Copy").clicked() {
                                    ui.ctx().copy_text(translated.clone());
                                }
                            });
                        });
                    });
                ui.add_space(6.0);
            }
        });
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
    ui.label(egui::RichText::new(text).color(tokens.text_muted));
}

/// The number shown beside a section's title.
pub fn count(tab: &DocumentTab, which: SidebarTab) -> Option<usize> {
    match which {
        SidebarTab::Outline => Some(tab.outline.len()),
        SidebarTab::Pages => Some(tab.layout.len()),
        SidebarTab::Bookmarks => Some(tab.annotations.bookmarks.len()),
        SidebarTab::Notes => Some(tab.annotations.items.len()),
        SidebarTab::Search => (!tab.find.query.is_empty()).then_some(tab.find.matches.len()),
        SidebarTab::Translation | SidebarTab::Effects => None,
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

/// The open section's title, its count and its actions.
pub fn header(ui: &mut egui::Ui, tab: &DocumentTab, which: SidebarTab) -> Vec<SidebarAction> {
    let tokens = Tokens::get(ui.ctx());
    let mut actions = Vec::new();
    ui.horizontal(|ui| {
        ui.set_min_height(28.0);
        ui.spacing_mut().item_spacing.x = 8.0;
        ui.label(egui::RichText::new(which.label()).font(theme::semibold(15.0)));
        if let Some(count) = count(tab, which).filter(|&count| count > 0) {
            count_pill(ui, &tokens, count);
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if which == SidebarTab::Bookmarks
                && icons::button(ui, "plus", 26.0, false, "Add a bookmark here").clicked()
            {
                actions.push(SidebarAction::AddBookmark);
            }
        });
    });
    ui.add_space(6.0);
    actions
}

fn count_pill(ui: &mut egui::Ui, tokens: &Tokens, count: usize) {
    let galley =
        ui.painter()
            .layout_no_wrap(count.to_string(), theme::medium(11.5), tokens.text_muted);
    let size = egui::vec2((galley.size().x + 12.0).max(20.0), 18.0);
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    ui.painter().rect_filled(rect, 9.0, tokens.hover);
    ui.painter().galley(
        rect.center() - galley.size() / 2.0,
        galley,
        tokens.text_muted,
    );
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
    depth: usize,
    /// A colour dot before the title.
    dot: Option<egui::Color32>,
    selected: bool,
    enabled: bool,
    /// Icon buttons (icon, tooltip) shown over the detail while hovered.
    actions: &'a [(&'a str, &'a str)],
}

/// Draws `row`; returns its response and the action button clicked.
fn row(ui: &mut egui::Ui, row: Row<'_>) -> (egui::Response, Option<usize>) {
    let tokens = Tokens::get(ui.ctx());
    let (rect, response) = widgets::list_row(ui, 28.0, row.selected);
    let hovered = response.contains_pointer() && !row.actions.is_empty();
    let mut right = rect.right() - 8.0;
    let mut clicked = None;
    if hovered {
        let width = row.actions.len() as f32 * 26.0;
        let strip = egui::Rect::from_min_max(
            egui::pos2(rect.right() - 2.0 - width, rect.top()),
            egui::pos2(rect.right() - 2.0, rect.bottom()),
        );
        right = strip.left() - 4.0;
        ui.scope_builder(
            egui::UiBuilder::new()
                .max_rect(strip)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
            |ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                for (index, (icon, tip)) in row.actions.iter().enumerate() {
                    if icons::button(ui, icon, 26.0, false, tip).clicked() {
                        clicked = Some(index);
                    }
                }
            },
        );
    } else if let Some(detail) = row.detail {
        let galley = ui
            .painter()
            .layout_no_wrap(detail, theme::regular(11.5), tokens.text_faint);
        right -= galley.size().x;
        ui.painter().galley(
            egui::pos2(right, rect.center().y - galley.size().y / 2.0),
            galley,
            tokens.text_faint,
        );
    }
    let mut left = rect.left() + 8.0 + row.depth as f32 * 14.0;
    if let Some(dot) = row.dot {
        ui.painter()
            .circle_filled(egui::pos2(left + 4.0, rect.center().y), 4.0, dot);
        left += 16.0;
    }
    let color = if !row.enabled {
        tokens.text_faint
    } else if row.selected {
        tokens.accent_text
    } else {
        tokens.text
    };
    let font = if row.depth == 0 {
        theme::medium(13.0)
    } else {
        theme::regular(13.0)
    };
    widgets::row_text(
        ui,
        egui::pos2(left, rect.center().y),
        row.title,
        font,
        color,
        right - left - 8.0,
    );
    (response.on_hover_text(row.title), clicked)
}

fn outline(ui: &mut egui::Ui, tab: &DocumentTab, filter: &str, actions: &mut Vec<SidebarAction>) {
    if tab.outline.is_empty() {
        muted(ui, "This document has no outline.");
        return;
    }
    let current = tab.current_page();
    // The section being read: the last entry starting at or before the page.
    let reading = tab
        .outline
        .iter()
        .rposition(|entry| entry.page.is_some_and(|page| page <= current));
    if !tab
        .outline
        .iter()
        .any(|entry| matches(filter, &entry.title))
    {
        muted(ui, "No matches.");
        return;
    }
    egui::ScrollArea::vertical()
        .auto_shrink(false)
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for (index, entry) in tab.outline.iter().enumerate() {
                if !matches(filter, &entry.title) {
                    continue;
                }
                let (response, _) = row(
                    ui,
                    Row {
                        title: &entry.title,
                        detail: entry.page.map(|page| (page + 1).to_string()),
                        depth: entry.depth,
                        dot: None,
                        selected: reading == Some(index),
                        enabled: entry.page.is_some(),
                        actions: &[],
                    },
                );
                if response.clicked()
                    && let Some(page) = entry.page
                {
                    actions.push(SidebarAction::View(ViewAction::GoToPage(page)));
                }
            }
        });
}

fn pages(ui: &mut egui::Ui, tab: &mut DocumentTab, actions: &mut Vec<SidebarAction>) {
    let tokens = Tokens::get(ui.ctx());
    let width = (ui.available_width() - 24.0).clamp(60.0, 200.0);
    let [page_width, page_height] = tab.layout.page_size(0);
    let box_height = width * (page_height / page_width.max(1.0)) as f32;
    let row_height = box_height + 26.0;
    let current = tab.current_page();
    let count = tab.layout.len();
    let mut shown = 0..0;
    egui::ScrollArea::vertical()
        .auto_shrink(false)
        .show_rows(ui, row_height, count, |ui, rows| {
            shown = rows.clone();
            for page in rows {
                ui.vertical_centered(|ui| {
                    let (rect, response) =
                        ui.allocate_exact_size(egui::vec2(width, box_height), egui::Sense::click());
                    let [w, h] = tab.layout.page_size(page);
                    let scale = (width / w as f32).min(box_height / h as f32);
                    let paper = egui::Rect::from_center_size(
                        rect.center(),
                        egui::vec2(w as f32 * scale, h as f32 * scale),
                    );
                    let painter = ui.painter();
                    painter.rect_filled(paper, 2.0, tokens.page);
                    if let Some(texture) = tab.thumbnail(page) {
                        widgets::paint_turned_image(
                            painter,
                            texture.id(),
                            paper,
                            tab.layout.arrangement.rotation,
                        );
                    } else if tab.dense_pages.contains(&page) {
                        painter.text(
                            paper.center(),
                            egui::Align2::CENTER_CENTER,
                            "dense page",
                            egui::FontId::proportional(11.0),
                            egui::Color32::GRAY,
                        );
                    }
                    let stroke = if page == current {
                        egui::Stroke::new(2.5, tokens.accent)
                    } else if response.hovered() {
                        egui::Stroke::new(1.0, tokens.text_muted)
                    } else {
                        egui::Stroke::new(1.0, tokens.border)
                    };
                    painter.rect_stroke(paper, 2.0, stroke, egui::StrokeKind::Outside);
                    if response.clicked() {
                        actions.push(SidebarAction::View(ViewAction::GoToPage(page)));
                    }
                    ui.label(
                        egui::RichText::new((page + 1).to_string())
                            .size(11.0)
                            .color(tokens.text_muted),
                    );
                });
            }
        });
    let ahead = shown.start.saturating_sub(2)..(shown.end + 2).min(count);
    tab.want_thumbnails(ahead);
}

fn bookmarks(
    ui: &mut egui::Ui,
    tab: &DocumentTab,
    state: &mut SidebarState,
    filter: &str,
    actions: &mut Vec<SidebarAction>,
) {
    if tab.annotations.bookmarks.is_empty() {
        muted(ui, "No bookmarks yet. Add one with + or Ctrl+D.");
        return;
    }
    let title = |bookmark: &glyphst_library::Bookmark| {
        bookmark
            .title
            .clone()
            .unwrap_or_else(|| format!("Page {}", bookmark.position.page.max(1)))
    };
    if !tab
        .annotations
        .bookmarks
        .iter()
        .any(|bookmark| matches(filter, &title(bookmark)))
    {
        muted(ui, "No matches.");
        return;
    }
    egui::ScrollArea::vertical()
        .auto_shrink(false)
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for bookmark in &tab.annotations.bookmarks {
                let name = title(bookmark);
                if !matches(filter, &name) {
                    continue;
                }
                if let Some((id, text)) = &mut state.bookmark
                    && *id == bookmark.id
                {
                    let response =
                        ui.add(egui::TextEdit::singleline(text).desired_width(f32::INFINITY));
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
                let (response, clicked) = ui
                    .push_id(bookmark.id, |ui| {
                        row(
                            ui,
                            Row {
                                title: &name,
                                detail: Some(page.to_string()),
                                depth: 0,
                                dot: None,
                                selected: false,
                                enabled: true,
                                actions: &[("pen-line", "Rename"), ("trash-2", "Delete")],
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
        });
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
            "No highlights or notes yet. Select text and right-click to add one.",
        );
        return;
    }
    let shown =
        |item: &annotations::Item| matches(filter, &item.text) || matches(filter, &item.body);
    if !tab.annotations.items.iter().any(shown) {
        muted(ui, "No matches.");
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
                let title = if item.text.is_empty() {
                    item.body.as_str()
                } else {
                    item.text.as_str()
                };
                let title = title.lines().next().unwrap_or_default();
                let (response, _) = ui
                    .push_id(item.id, |ui| {
                        row(
                            ui,
                            Row {
                                title,
                                detail: Some(match place {
                                    Some((page, _)) => (page + 1).to_string(),
                                    None => "–".to_owned(),
                                }),
                                depth: 0,
                                dot: Some(annotations::color(item.color)),
                                selected: open,
                                enabled: true,
                                actions: &[],
                            },
                        )
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

/// An open note under its row: the whole quote, the note, its colours and
/// its actions.
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
            left: 24,
            right: 8,
            top: 4,
            bottom: 10,
        })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 6.0;
            if !placed {
                muted(ui, "Not found in this layout.");
            }
            if !item.text.is_empty() {
                let quote: String = item.text.chars().take(QUOTE_CHARS).collect();
                let ellipsis = if item.text.chars().count() > QUOTE_CHARS {
                    "…"
                } else {
                    ""
                };
                ui.label(
                    egui::RichText::new(format!("“{quote}{ellipsis}”"))
                        .italics()
                        .color(tokens.text_muted),
                );
            }
            match &mut state.note {
                Some((id, body)) if *id == item.id => {
                    ui.add(
                        egui::TextEdit::multiline(body)
                            .desired_rows(3)
                            .desired_width(f32::INFINITY)
                            .hint_text("Note"),
                    );
                    let (save, cancel) = ui
                        .horizontal(|ui| {
                            (
                                widgets::pill_button(ui, None, "Save", true).clicked(),
                                widgets::pill_button(ui, None, "Cancel", false).clicked(),
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
                _ => {
                    if !item.body.is_empty() {
                        ui.label(&item.body);
                    }
                }
            }
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                for color in HighlightColor::ALL {
                    let (rect, response) =
                        ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::click());
                    let radius = if color == item.color { 7.0 } else { 5.0 };
                    ui.painter()
                        .circle_filled(rect.center(), radius, annotations::color(color));
                    if response
                        .on_hover_text(annotations::color_name(color))
                        .clicked()
                    {
                        actions.push(SidebarAction::SetColor(item.id, color));
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    if icons::button(ui, "trash-2", 26.0, false, "Delete").clicked() {
                        actions.push(SidebarAction::Remove(item.id));
                    }
                    if matches!(item.id, AnnotationId::Note(_))
                        && icons::button(ui, "pen-line", 26.0, false, "Edit the note").clicked()
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

    #[test]
    fn sidebar_tabs_parse_by_name() {
        assert_eq!(SidebarTab::parse("pages"), Some(SidebarTab::Pages));
        assert_eq!(SidebarTab::parse("Search"), Some(SidebarTab::Search));
        assert_eq!(SidebarTab::parse("nope"), None);
    }
}
