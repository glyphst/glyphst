//! The document sidebar, after pdfcraft's side panels (artcraft,
//! `ui-egui/src/panels.rs`, `comments_panel.rs`, `search_ui.rs`): outline,
//! page thumbnails, bookmarks, highlights and notes, and search results.

use std::time::Instant;

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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
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
    match which {
        SidebarTab::Outline => outline(ui, tab, &mut actions),
        SidebarTab::Pages => pages(ui, tab, &mut actions),
        SidebarTab::Bookmarks => bookmarks(ui, tab, state, &mut actions),
        SidebarTab::Notes => notes(ui, tab, state, &mut actions),
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

/// A list row with `title` indented by `depth` and an optional detail
/// (a page number) at the right.
fn list_entry(
    ui: &mut egui::Ui,
    title: &str,
    detail: Option<String>,
    depth: usize,
    selected: bool,
    enabled: bool,
) -> egui::Response {
    let tokens = Tokens::get(ui.ctx());
    let (rect, response) = widgets::list_row(ui, 28.0, selected);
    let mut right = rect.right() - 8.0;
    if let Some(detail) = detail {
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
    let left = rect.left() + 8.0 + depth as f32 * 14.0;
    let color = if !enabled {
        tokens.text_faint
    } else if selected {
        tokens.accent_text
    } else {
        tokens.text
    };
    let font = if depth == 0 {
        theme::medium(13.0)
    } else {
        theme::regular(13.0)
    };
    widgets::row_text(
        ui,
        egui::pos2(left, rect.center().y),
        title,
        font,
        color,
        right - left - 8.0,
    );
    response.on_hover_text(title)
}

fn outline(ui: &mut egui::Ui, tab: &DocumentTab, actions: &mut Vec<SidebarAction>) {
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
    egui::ScrollArea::vertical()
        .auto_shrink(false)
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for (index, entry) in tab.outline.iter().enumerate() {
                let response = list_entry(
                    ui,
                    &entry.title,
                    entry.page.map(|page| (page + 1).to_string()),
                    entry.depth,
                    reading == Some(index),
                    entry.page.is_some(),
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
    actions: &mut Vec<SidebarAction>,
) {
    if ui.button("＋ Add bookmark").clicked() {
        actions.push(SidebarAction::AddBookmark);
    }
    ui.separator();
    if tab.annotations.bookmarks.is_empty() {
        muted(ui, "No bookmarks yet.");
        return;
    }
    egui::ScrollArea::vertical()
        .auto_shrink(false)
        .show(ui, |ui| {
            for bookmark in &tab.annotations.bookmarks {
                let page = bookmark.position.page.max(1);
                let title = bookmark
                    .title
                    .clone()
                    .unwrap_or_else(|| format!("Page {page}"));
                ui.horizontal(|ui| {
                    if let Some((id, name)) = &mut state.bookmark
                        && *id == bookmark.id
                    {
                        let response = ui.text_edit_singleline(name);
                        if std::mem::take(&mut state.focus_rename) {
                            response.request_focus();
                        }
                        if response.lost_focus() {
                            if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                                actions.push(SidebarAction::RenameBookmark(*id, name.clone()));
                            }
                            state.bookmark = None;
                        }
                        return;
                    }
                    let response = ui
                        .add(egui::Button::new(&title).frame(false))
                        .on_hover_text(format!("Page {page}"));
                    if response.clicked() {
                        actions.push(SidebarAction::GoToBookmark(bookmark.id));
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if icons::button(ui, "trash-2", 24.0, false, "Delete").clicked() {
                            actions.push(SidebarAction::RemoveBookmark(bookmark.id));
                        }
                        if icons::button(ui, "pen-line", 24.0, false, "Rename").clicked() {
                            state.bookmark = Some((bookmark.id, title.clone()));
                            state.focus_rename = true;
                        }
                        muted(ui, &page.to_string());
                    });
                });
            }
        });
}

fn notes(
    ui: &mut egui::Ui,
    tab: &DocumentTab,
    state: &mut SidebarState,
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
    let reflowed = tab.is_reflowed();
    egui::ScrollArea::vertical()
        .auto_shrink(false)
        .show(ui, |ui| {
            for item in &tab.annotations.items {
                let place = tab.annotations.page_of(item, reflowed);
                egui::Frame::new()
                    .fill(tokens.card)
                    .stroke(egui::Stroke::new(1.0, tokens.border))
                    .corner_radius(6)
                    .inner_margin(8)
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            let (dot, _) = ui
                                .allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
                            ui.painter().circle_filled(
                                dot.center(),
                                5.0,
                                annotations::color(item.color),
                            );
                            let kind = match item.id {
                                AnnotationId::Highlight(_) => "Highlight",
                                AnnotationId::Note(_) => "Note",
                            };
                            let label = match place {
                                Some((page, _)) => format!("{kind} · p. {}", page + 1),
                                None => format!("{kind} · not found in this layout"),
                            };
                            muted(ui, &label);
                        });
                        if !item.text.is_empty() {
                            let quote: String = item.text.chars().take(QUOTE_CHARS).collect();
                            let ellipsis = if item.text.chars().count() > QUOTE_CHARS {
                                "…"
                            } else {
                                ""
                            };
                            let response = ui.add(
                                egui::Label::new(
                                    egui::RichText::new(format!("“{quote}{ellipsis}”")).italics(),
                                )
                                .sense(egui::Sense::click()),
                            );
                            if response.clicked()
                                && let Some((page, y)) = place
                            {
                                actions.push(SidebarAction::View(ViewAction::GoTo {
                                    page,
                                    y: Some(y),
                                }));
                            }
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
                                        (ui.button("Save").clicked(), ui.button("Cancel").clicked())
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
                            for color in HighlightColor::ALL {
                                let (rect, response) = ui.allocate_exact_size(
                                    egui::vec2(16.0, 16.0),
                                    egui::Sense::click(),
                                );
                                let radius = if color == item.color { 7.0 } else { 5.0 };
                                ui.painter().circle_filled(
                                    rect.center(),
                                    radius,
                                    annotations::color(color),
                                );
                                if response
                                    .on_hover_text(annotations::color_name(color))
                                    .clicked()
                                {
                                    actions.push(SidebarAction::SetColor(item.id, color));
                                }
                            }
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.small_button("Delete").clicked() {
                                        actions.push(SidebarAction::Remove(item.id));
                                    }
                                    if matches!(item.id, AnnotationId::Note(_))
                                        && ui.small_button("Edit note").clicked()
                                    {
                                        state.note = Some((item.id, item.body.clone()));
                                    }
                                },
                            );
                        });
                    });
                ui.add_space(6.0);
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sidebar_tabs_parse_by_name() {
        assert_eq!(SidebarTab::parse("pages"), Some(SidebarTab::Pages));
        assert_eq!(SidebarTab::parse("Search"), Some(SidebarTab::Search));
        assert_eq!(SidebarTab::parse("nope"), None);
    }
}
