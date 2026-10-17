//! The library as a table (frames 04, 04b and 04d): filter, kind and
//! order, then one row per document with its title, authors, type,
//! progress and when it was opened. Home and the new tab embed it under
//! their other sections; the library tab gives it the whole view, with a
//! heading, a pages column and its own scrolling under a fixed header.

use std::path::PathBuf;

use egui::{Align2, Rect, Sense, pos2, vec2};
use glyphst_library::current_timestamp;
use glyphst_ui::{button::Button, field::TextField, key::Key, meter::Meter, motif, segmented};

use crate::{
    home::{Doc, day},
    theme::{self, Tokens},
};

const ROW: f32 = 30.0;
/// Below this width the controls go under the title; without a filter
/// field (in the new tab) they need less.
const NARROW: f32 = 780.0;
const NARROW_UNFILTERED: f32 = 520.0;

pub enum LibraryAction {
    Open(PathBuf),
    /// `[ library → ]`: the library tab.
    Tab,
    OpenDialog,
}

/// Which documents the table lists.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Kind {
    #[default]
    All,
    Pdf,
    Epub,
}

/// The table's order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Sort {
    #[default]
    Opened,
    Title,
    Progress,
}

impl Sort {
    const ALL: [Self; 3] = [Self::Opened, Self::Title, Self::Progress];

    fn label(self) -> &'static str {
        match self {
            Self::Opened => "opened",
            Self::Title => "title",
            Self::Progress => "progress",
        }
    }
}

/// Where the table is shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Under home's sections: a rule title, its own filter, and
    /// `[ library → ]`.
    Home,
    /// In the new tab, whose field filters it.
    Launcher,
    /// The library tab: a heading, and the table filling the view.
    Tab,
}

pub struct LibraryView {
    /// The words rows must hold (title or authors).
    pub filter: String,
    kind: Kind,
    sort: Sort,
    id: &'static str,
}

/// What the library tab's heading says, and its open-file key.
pub struct TabContext<'a> {
    pub picking: bool,
    pub open_key: Option<&'a str>,
}

impl LibraryView {
    pub fn new(id: &'static str) -> Self {
        Self {
            filter: String::new(),
            kind: Kind::default(),
            sort: Sort::default(),
            id,
        }
    }

    /// The documents listed, in the table's order.
    pub fn rows<'d>(&self, docs: &'d [Doc]) -> Vec<&'d Doc> {
        let words: Vec<String> = self
            .filter
            .split_whitespace()
            .map(str::to_lowercase)
            .collect();
        let mut rows: Vec<&Doc> = docs
            .iter()
            .filter(|doc| match self.kind {
                Kind::All => true,
                Kind::Pdf => doc.kind == "pdf",
                Kind::Epub => doc.kind == "epub",
            })
            .filter(|doc| {
                let haystack = format!(
                    "{} {} {}",
                    doc.title,
                    doc.authors,
                    doc.path.file_name().unwrap_or_default().to_string_lossy()
                )
                .to_lowercase();
                words.iter().all(|word| haystack.contains(word))
            })
            .collect();
        // The sorts are stable over a fixed order, so ties stay put.
        match self.sort {
            Sort::Opened => rows.sort_by_key(|doc| std::cmp::Reverse(doc.opened)),
            Sort::Title => rows.sort_by_key(|doc| doc.title.to_lowercase()),
            Sort::Progress => rows.sort_by(|a, b| {
                b.progress()
                    .unwrap_or(-1.0)
                    .total_cmp(&a.progress().unwrap_or(-1.0))
            }),
        }
        rows
    }

    /// The table in `width`, as `mode` shows it.
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        docs: &[Doc],
        width: f32,
        mode: Mode,
        tab: Option<TabContext<'_>>,
    ) -> Option<LibraryAction> {
        let tokens = Tokens::get(ui.ctx());
        let mut action = None;
        if mode == Mode::Tab {
            if let Some(chosen) = heading(ui, &tokens, docs, width, tab) {
                action = Some(chosen);
            }
            ui.add_space(14.0);
        }
        let rows = self.rows(docs);
        if let Some(chosen) = self.controls(ui, docs.len(), rows.len(), width, mode) {
            action = Some(chosen);
        }
        ui.add_space(12.0);
        let full = mode == Mode::Tab;
        let columns = |x: egui::Rangef| table_columns(x, full);
        let (header, _) = ui.allocate_exact_size(vec2(width, 24.0), Sense::hover());
        self.header(ui, &tokens, header, &columns(header.x_range()), full);
        if rows.is_empty() {
            ui.add_space(16.0);
            ui.label(
                egui::RichText::new("nothing matches")
                    .font(theme::mono(11.0))
                    .color(tokens.muted),
            );
            return action;
        }
        let mut table = |ui: &mut egui::Ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            let now = current_timestamp();
            for doc in &rows {
                let (rect, response) = ui.allocate_exact_size(vec2(width, ROW), Sense::click());
                if ui.is_rect_visible(rect) {
                    // The keyboard's (and the pad's) focus, as list rows show it.
                    if glyphst_ui::paint::has_focus(&response) {
                        ui.painter().rect_filled(rect, 0, tokens.hover);
                        ui.painter().rect_filled(
                            egui::Rect::from_min_size(rect.min, vec2(2.0, rect.height())),
                            0,
                            tokens.accent,
                        );
                    }
                    paint_row(ui, &tokens, rect, &columns(rect.x_range()), doc, full, now);
                }
                if response
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .clicked()
                {
                    action = Some(LibraryAction::Open(doc.path.clone()));
                }
            }
        };
        if full {
            // The header stays; the rows scroll under it.
            egui::ScrollArea::vertical()
                .id_salt((self.id, "rows"))
                .auto_shrink(false)
                .show(ui, |ui| table(ui));
        } else {
            table(ui);
        }
        action
    }

    /// `── library ── 128` (or the count shown in the tab), the filter,
    /// kind and order, and `[ library → ]` outside the tab.
    fn controls(
        &mut self,
        ui: &mut egui::Ui,
        total: usize,
        shown: usize,
        width: f32,
        mode: Mode,
    ) -> Option<LibraryAction> {
        let mut action = None;
        let narrow = width
            < if mode == Mode::Launcher {
                NARROW_UNFILTERED
            } else {
                NARROW
            };
        let count = if shown == total {
            total.to_string()
        } else {
            format!("{shown} of {total}")
        };
        let title = |ui: &mut egui::Ui, width: f32| {
            let (rect, _) = ui.allocate_exact_size(vec2(width, 20.0), Sense::hover());
            if mode == Mode::Tab {
                ui.painter().text(
                    rect.left_center(),
                    Align2::LEFT_CENTER,
                    format!("{count} shown"),
                    theme::mono(11.0),
                    Tokens::get(ui.ctx()).faint,
                );
            } else {
                motif::paint_rule_title(ui, rect, "library", Some(&count), true);
            }
        };
        if narrow {
            title(ui, width);
            ui.add_space(8.0);
        }
        let filter_id = egui::Id::new((self.id, "filter"));
        let (row, _) = ui.allocate_exact_size(vec2(width, 28.0), Sense::hover());
        let mut row_ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(row)
                .layout(egui::Layout::right_to_left(egui::Align::Center)),
        );
        let ui_row = &mut row_ui;
        ui_row.spacing_mut().item_spacing.x = 10.0;
        if mode != Mode::Tab && ui_row.add(Button::bracket("library →")).clicked() {
            action = Some(LibraryAction::Tab);
        }
        let sort = ui_row.add(Button::plain(&format!("{} ▾", self.sort.label())).small());
        egui::Popup::menu(&sort).show(|ui| {
            glyphst_ui::menu::style(ui);
            for option in Sort::ALL {
                if glyphst_ui::menu::Item::new(option.label())
                    .checked(self.sort == option)
                    .show(ui)
                    .clicked()
                {
                    self.sort = option;
                }
            }
        });
        segmented::labels(
            ui_row,
            &mut self.kind,
            &[(Kind::All, "all"), (Kind::Pdf, "pdf"), (Kind::Epub, "epub")],
        );
        if mode != Mode::Launcher {
            let field_width = if mode == Mode::Tab { 360.0 } else { 260.0 };
            TextField::search(&mut self.filter, "filter by title or author")
                .shortcut("/")
                .width((ui_row.available_width() - 16.0).clamp(140.0, field_width))
                .id(filter_id)
                .show(ui_row);
        }
        if !narrow {
            // Up to the leftmost control laid out from the right.
            let left = (ui_row.min_rect().left() - row.left() - 16.0).max(0.0);
            let mut title_ui = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(Rect::from_min_size(row.min, vec2(left, row.height())))
                    .layout(egui::Layout::left_to_right(egui::Align::Center)),
            );
            title(&mut title_ui, left);
        }
        if mode != Mode::Launcher
            && ui.input(|i| i.key_pressed(egui::Key::Slash))
            && ui.ctx().memory(|memory| memory.focused().is_none())
        {
            ui.ctx()
                .memory_mut(|memory| memory.request_focus(filter_id));
        }
        action
    }

    /// The column names, the sorted one marked `↓`.
    fn header(&self, ui: &egui::Ui, tokens: &Tokens, header: Rect, columns: &[f32], full: bool) {
        let names: &[&str] = if full {
            &["title", "authors", "type", "pages", "progress", "opened"]
        } else {
            &["title", "authors", "type", "progress", "opened"]
        };
        for (name, x) in names.iter().zip(columns) {
            let sorted = matches!(
                (*name, self.sort),
                ("title", Sort::Title) | ("progress", Sort::Progress) | ("opened", Sort::Opened)
            );
            let label = if sorted {
                format!("{name} ↓")
            } else {
                (*name).to_owned()
            };
            ui.painter().text(
                pos2(x + 8.0, header.center().y),
                Align2::LEFT_CENTER,
                label,
                theme::mono(10.0),
                if sorted { tokens.text_2 } else { tokens.faint },
            );
        }
        ui.painter()
            .hline(header.x_range(), header.bottom() - 0.5, tokens.hairline());
    }
}

/// The library tab's heading: `── library ──`, `128 documents · 3 in
/// progress`, and `[ open file… ]` with its key at the right.
fn heading(
    ui: &mut egui::Ui,
    tokens: &Tokens,
    docs: &[Doc],
    width: f32,
    tab: Option<TabContext<'_>>,
) -> Option<LibraryAction> {
    let mut action = None;
    let (rect, _) = ui.allocate_exact_size(vec2(width, 56.0), Sense::hover());
    motif::paint_rule_title(
        ui,
        Rect::from_min_size(rect.min, vec2(120.0, 18.0)),
        "library",
        None,
        true,
    );
    let going = docs
        .iter()
        .filter(|doc| {
            doc.progress()
                .is_some_and(|progress| progress > 0.0 && progress < 0.999)
        })
        .count();
    let documents = if docs.len() == 1 {
        "document"
    } else {
        "documents"
    };
    glyphst_ui::text::row(
        ui,
        pos2(rect.left(), rect.bottom() - 14.0),
        format!("{} {documents} · {going} in progress", docs.len()),
        theme::semibold(22.0),
        tokens.text,
        width - 240.0,
    );
    let actions = Rect::from_min_max(pos2(rect.right() - 240.0, rect.bottom() - 28.0), rect.max);
    let mut actions_ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(actions)
            .layout(egui::Layout::right_to_left(egui::Align::Center)),
    );
    actions_ui.spacing_mut().item_spacing.x = 6.0;
    let (picking, key) = tab.map_or((false, None), |tab| (tab.picking, tab.open_key));
    if let Some(key) = key {
        actions_ui.add(Key(&glyphst_ui::key::display(key)));
    }
    if actions_ui
        .add_enabled(!picking, Button::bracket("open file…"))
        .clicked()
    {
        action = Some(LibraryAction::OpenDialog);
    }
    action
}

/// The columns' left edges across `x`: title, authors, type, (pages,)
/// progress, opened.
fn table_columns(x: egui::Rangef, full: bool) -> Vec<f32> {
    let width = x.span();
    let at = |fraction: f32| x.min + width * fraction;
    if full {
        vec![x.min, at(0.40), at(0.57), at(0.635), at(0.70), at(0.88)]
    } else {
        vec![x.min, at(0.44), at(0.64), at(0.72), at(0.88)]
    }
}

fn paint_row(
    ui: &egui::Ui,
    tokens: &Tokens,
    rect: Rect,
    columns: &[f32],
    doc: &Doc,
    full: bool,
    now: u64,
) {
    if ui.rect_contains_pointer(rect) {
        ui.painter().rect_filled(rect, 0, tokens.hover);
    }
    ui.painter()
        .hline(rect.x_range(), rect.bottom() - 0.5, tokens.hairline());
    let y = rect.center().y;
    let cell = |column: usize, text: &str, color: egui::Color32, size: f32| {
        let right = columns.get(column + 1).copied().unwrap_or(rect.right());
        glyphst_ui::text::row(
            ui,
            pos2(columns[column] + 8.0, y),
            text.to_owned(),
            theme::mono(size),
            color,
            right - columns[column] - 16.0,
        );
    };
    cell(0, &doc.title, tokens.text, 12.0);
    cell(1, &doc.authors, tokens.muted, 11.0);
    cell(2, &doc.kind, tokens.muted, 11.0);
    let mut next = 3;
    if full {
        let pages = doc
            .page_count()
            .map_or_else(String::new, |pages| pages.to_string());
        cell(3, &pages, tokens.muted, 11.0);
        next = 4;
    }
    match doc.progress() {
        Some(progress) if progress < 0.999 => {
            let label = doc.progress_label();
            let width = (columns[next + 1] - columns[next] - 16.0).min(116.0);
            Meter::new(progress)
                .bar()
                .width((width - 6.0).max(40.0))
                .label(&label)
                .accent_label(true)
                .paint(
                    ui,
                    Rect::from_min_size(pos2(columns[next] + 8.0, y - 7.0), vec2(width, 14.0)),
                );
        }
        _ => {
            let label = doc.progress_label();
            let label = if label == "done" {
                "done ✓".to_owned()
            } else {
                label
            };
            cell(next, &label, tokens.muted, 11.0);
        }
    }
    let opened = doc
        .opened
        .map_or_else(|| "new".to_owned(), |at| day(at, now));
    cell(next + 1, &opened, tokens.muted, 11.0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tab_adds_a_pages_column_inside_the_same_width() {
        let x = egui::Rangef::new(0.0, 1000.0);
        let home = table_columns(x, false);
        let tab = table_columns(x, true);
        assert_eq!((home.len(), tab.len()), (5, 6));
        assert!(tab.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(*tab.last().unwrap() < 1000.0);
    }
}
