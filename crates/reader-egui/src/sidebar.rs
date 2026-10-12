//! The document sidebar, after pdfcraft's side panels (artcraft,
//! `ui-egui/src/panels.rs`, `comments_panel.rs`, `search_ui.rs`): outline,
//! page thumbnails, bookmarks, highlights and notes, and search results.

use std::time::Instant;

use glyphst_library::HighlightColor;
use serde::{Deserialize, Serialize};

use crate::{
    annotations::{self, AnnotationId},
    document_tab::{DocumentTab, ViewAction},
    find,
    theme::Tokens,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SidebarTab {
    #[default]
    Outline,
    Pages,
    Bookmarks,
    Notes,
    Search,
}

impl SidebarTab {
    pub const ALL: [Self; 5] = [
        Self::Outline,
        Self::Pages,
        Self::Bookmarks,
        Self::Notes,
        Self::Search,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Outline => "Outline",
            Self::Pages => "Pages",
            Self::Bookmarks => "Bookmarks",
            Self::Notes => "Notes",
            Self::Search => "Search",
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
}

/// Edits in progress in the sidebar.
#[derive(Default)]
pub struct SidebarState {
    bookmark: Option<(u64, String)>,
    note: Option<(AnnotationId, String)>,
}

const QUOTE_CHARS: usize = 240;

pub fn show(
    ui: &mut egui::Ui,
    tab: &mut DocumentTab,
    which: &mut SidebarTab,
    state: &mut SidebarState,
    now: Instant,
) -> Vec<SidebarAction> {
    ui.horizontal_wrapped(|ui| {
        for candidate in SidebarTab::ALL {
            ui.selectable_value(which, candidate, candidate.label());
        }
    });
    ui.separator();
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
    }
    actions
}

fn muted(ui: &mut egui::Ui, text: &str) {
    let tokens = Tokens::get(ui.ctx());
    ui.label(egui::RichText::new(text).color(tokens.text_muted));
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
            for (index, entry) in tab.outline.iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.add_space(entry.depth as f32 * 12.0);
                    let response = ui.add_enabled(
                        entry.page.is_some(),
                        egui::Button::selectable(reading == Some(index), entry.title.as_str())
                            .frame(false),
                    );
                    if response.clicked()
                        && let Some(page) = entry.page
                    {
                        actions.push(SidebarAction::View(ViewAction::GoToPage(page)));
                    }
                });
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
                        painter.image(
                            texture.id(),
                            paper,
                            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                            egui::Color32::WHITE,
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
    if ui.button("＋ Add bookmark  Ctrl+D").clicked() {
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
                        response.request_focus();
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
                        if ui.small_button("×").on_hover_text("Delete").clicked() {
                            actions.push(SidebarAction::RemoveBookmark(bookmark.id));
                        }
                        if ui.small_button("✎").on_hover_text("Rename").clicked() {
                            state.bookmark = Some((bookmark.id, title.clone()));
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
