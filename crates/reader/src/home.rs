//! Home (frame 04): the day and a greeting with the week's reading, the
//! wordmark, the document to continue, recent documents as ASCII covers,
//! the library as a table to filter and sort, and at the right the
//! activity log over a drop zone. Covers are drawn from the title; real
//! covers are not rendered.

use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
};

use chrono::{Datelike, Local, TimeZone, Timelike};
use egui::{Align2, Rect, Sense, StrokeKind, pos2, vec2};
use glyphst_library::{ActivityKind, LibraryStore, current_timestamp};
use glyphst_ui::{
    Tip,
    button::Button,
    cover::DocCover,
    field::TextField,
    key::Key,
    meter::Meter,
    motif,
    pane::{self, Action as PaneAction, Header},
    segmented,
};

use crate::{
    icons,
    theme::{self, Tokens},
};

const RECENT_LIMIT: usize = 12;
/// The main column's widest.
const COLUMN: f32 = 980.0;
/// The activity pane's width.
const ACTIVITY_WIDTH: f32 = 380.0;
const ROW: f32 = 30.0;

pub enum HomeAction {
    Open(PathBuf),
    OpenDialog,
    /// Opens a document with the assistant beside it.
    AskAbout(PathBuf),
}

/// Which documents the library table lists.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Kind {
    #[default]
    All,
    Pdf,
    Epub,
}

/// The library table's order.
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

#[derive(Default)]
pub struct Home {
    filter: String,
    kind: Kind,
    sort: Sort,
    /// The activity pane is shown.
    activity_hidden: bool,
}

/// What home shows besides the library.
pub struct HomeContext<'a> {
    pub library: &'a LibraryStore,
    /// Page counts of documents opened before, by path.
    pub page_counts: &'a BTreeMap<String, usize>,
    pub picking: bool,
    /// The key of "open with system dialog", as preferences keep it.
    pub open_key: Option<String>,
}

/// A document as home lists it.
#[derive(Clone, Debug)]
struct Doc {
    path: PathBuf,
    title: String,
    authors: String,
    year: Option<i32>,
    kind: String,
    /// The page last read (from 1) and the page count, when known.
    page: usize,
    pages: Option<usize>,
    opened: Option<u64>,
}

impl Doc {
    fn progress(&self) -> Option<f32> {
        let pages = self.pages.filter(|&pages| pages > 0)?;
        Some((self.page as f32 / pages as f32).clamp(0.0, 1.0))
    }

    fn progress_label(&self) -> String {
        match self.progress() {
            Some(progress) if progress >= 0.999 => "done".into(),
            Some(progress) => format!("{:.0}%", progress * 100.0),
            None if self.opened.is_none() => "new".into(),
            None => "—".into(),
        }
    }

    /// `pdf · 15p`.
    fn kind_line(&self) -> String {
        match self.pages {
            Some(pages) => format!("{} · {pages}p", self.kind),
            None => self.kind.clone(),
        }
    }
}

impl Home {
    pub fn ui(&mut self, ui: &mut egui::Ui, home: HomeContext<'_>) -> Option<HomeAction> {
        let tokens = Tokens::get(ui.ctx());
        let mut action = None;
        let full = ui.max_rect();
        ui.painter().rect_filled(full, 0, tokens.bg);
        let docs = documents(home.library, home.page_counts);
        let show_activity = !self.activity_hidden && full.width() > 900.0;
        let main = if show_activity {
            full.with_max_x(full.right() - ACTIVITY_WIDTH)
        } else {
            full
        };
        let mut main_ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(main)
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        egui::ScrollArea::vertical()
            .id_salt("home")
            .auto_shrink(false)
            .show(&mut main_ui, |ui| {
                let width = ui.available_width();
                let column = (width - 96.0).clamp(240.0, COLUMN);
                ui.horizontal(|ui| {
                    ui.add_space(((width - column) / 2.0).max(16.0));
                    ui.vertical(|ui| {
                        ui.set_width(column);
                        ui.add_space(28.0);
                        greeting(ui, &tokens, home.library, &docs);
                        if let Some(error) = home.library.startup_error() {
                            ui.add_space(16.0);
                            notice(ui, &tokens, error);
                        }
                        let continuing = docs
                            .iter()
                            .filter(|doc| doc.opened.is_some() && doc.path.exists())
                            .max_by_key(|doc| doc.opened);
                        if let Some(doc) = continuing {
                            ui.add_space(24.0);
                            if let Some(chosen) = continue_card(ui, &tokens, home.library, doc) {
                                action = Some(chosen);
                            }
                        }
                        let mut recent: Vec<&Doc> = docs
                            .iter()
                            .filter(|doc| doc.opened.is_some())
                            .filter(|doc| Some(&doc.path) != continuing.map(|doc| &doc.path))
                            .collect();
                        recent.sort_by_key(|doc| std::cmp::Reverse(doc.opened));
                        recent.truncate(RECENT_LIMIT);
                        if !recent.is_empty() {
                            ui.add_space(24.0);
                            motif::rule_title(ui, "recent", Some(&recent.len().to_string()));
                            ui.add_space(8.0);
                            ui.horizontal_wrapped(|ui| {
                                ui.spacing_mut().item_spacing = vec2(16.0, 16.0);
                                for doc in recent {
                                    let label = doc.progress_label();
                                    let response = DocCover::new(&doc.title, &doc.kind_line())
                                        .progress(doc.progress(), &label)
                                        .size(vec2(148.0, 152.0))
                                        .show(ui)
                                        .tip(&doc.path.display().to_string());
                                    if response.clicked() {
                                        action = Some(HomeAction::Open(doc.path.clone()));
                                    }
                                }
                            });
                        }
                        if let Some(chosen) = self.library(ui, &tokens, &docs) {
                            action = Some(chosen);
                        }
                        if docs.is_empty() {
                            ui.add_space(48.0);
                            empty(ui, &tokens);
                        }
                        ui.add_space(48.0);
                    });
                });
            });
        if show_activity {
            let rect = full.with_min_x(main.right());
            let mut pane_ui = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(rect)
                    .layout(egui::Layout::top_down(egui::Align::Min)),
            );
            if let Some(chosen) = self.activity(
                &mut pane_ui,
                &tokens,
                home.library,
                home.picking,
                home.open_key.as_deref(),
            ) {
                action = Some(chosen);
            }
        }
        drop_hint(ui, &tokens, full);
        action
    }

    /// The library as a table: filter, kind, order, and one row per
    /// document.
    fn library(&mut self, ui: &mut egui::Ui, tokens: &Tokens, docs: &[Doc]) -> Option<HomeAction> {
        if docs.is_empty() {
            return None;
        }
        let mut action = None;
        ui.add_space(28.0);
        // Narrow columns put the controls under the title.
        let narrow = ui.available_width() < 780.0;
        if narrow {
            motif::rule_title(ui, "library", Some(&docs.len().to_string()));
            ui.add_space(8.0);
        }
        ui.horizontal(|ui| {
            ui.set_width(ui.available_width());
            if !narrow {
                let title_width = (ui.available_width() - 540.0).max(120.0);
                ui.allocate_ui(vec2(title_width, 20.0), |ui| {
                    motif::rule_title(ui, "library", Some(&docs.len().to_string()));
                });
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                let sort = ui.add(Button::plain(&format!("{} ▾", self.sort.label())).small());
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
                    ui,
                    &mut self.kind,
                    &[(Kind::All, "all"), (Kind::Pdf, "pdf"), (Kind::Epub, "epub")],
                );
                TextField::search(&mut self.filter, "filter by title or author")
                    .shortcut("/")
                    .width(ui.available_width().clamp(140.0, 260.0))
                    .id(egui::Id::new("home-filter"))
                    .show(ui);
            });
        });
        if ui.input(|i| i.key_pressed(egui::Key::Slash))
            && ui.ctx().memory(|memory| memory.focused().is_none())
        {
            ui.ctx()
                .memory_mut(|memory| memory.request_focus(egui::Id::new("home-filter")));
        }
        ui.add_space(12.0);
        let filter = self.filter.trim().to_lowercase();
        let mut rows: Vec<&Doc> = docs
            .iter()
            .filter(|doc| match self.kind {
                Kind::All => true,
                Kind::Pdf => doc.kind == "pdf",
                Kind::Epub => doc.kind == "epub",
            })
            .filter(|doc| {
                filter.is_empty()
                    || doc.title.to_lowercase().contains(&filter)
                    || doc.authors.to_lowercase().contains(&filter)
            })
            .collect();
        match self.sort {
            Sort::Opened => rows.sort_by_key(|doc| std::cmp::Reverse(doc.opened)),
            Sort::Title => rows.sort_by_key(|doc| doc.title.to_lowercase()),
            Sort::Progress => rows.sort_by(|a, b| {
                b.progress()
                    .unwrap_or(-1.0)
                    .total_cmp(&a.progress().unwrap_or(-1.0))
            }),
        }
        // Columns: title, authors, type, progress, opened.
        let (header, _) = ui.allocate_exact_size(vec2(ui.available_width(), 24.0), Sense::hover());
        let columns = table_columns(header.x_range());
        let font = theme::mono(10.0);
        for (index, (title, x)) in [
            ("title", columns[0]),
            ("authors", columns[1]),
            ("type", columns[2]),
            ("progress", columns[3]),
            ("opened", columns[4]),
        ]
        .into_iter()
        .enumerate()
        {
            let sorted = matches!(
                (index, self.sort),
                (0, Sort::Title) | (3, Sort::Progress) | (4, Sort::Opened)
            );
            let label = if sorted {
                format!("{title} ↓")
            } else {
                title.to_owned()
            };
            ui.painter().text(
                pos2(x + 8.0, header.center().y),
                Align2::LEFT_CENTER,
                label,
                font.clone(),
                if sorted { tokens.text_2 } else { tokens.faint },
            );
        }
        ui.painter()
            .hline(header.x_range(), header.bottom() - 0.5, tokens.hairline());
        if rows.is_empty() {
            ui.add_space(16.0);
            ui.label(
                egui::RichText::new("nothing matches")
                    .font(theme::mono(11.0))
                    .color(tokens.muted),
            );
        }
        ui.spacing_mut().item_spacing.y = 0.0;
        let now = current_timestamp();
        for doc in rows {
            let (rect, response) =
                ui.allocate_exact_size(vec2(ui.available_width(), ROW), Sense::click());
            if response.hovered() {
                ui.painter().rect_filled(rect, 0, tokens.hover);
            }
            ui.painter()
                .hline(rect.x_range(), rect.bottom() - 0.5, tokens.hairline());
            let y = rect.center().y;
            let cell =
                |ui: &egui::Ui, column: usize, text: &str, color: egui::Color32, size: f32| {
                    let right = if column + 1 < columns.len() {
                        columns[column + 1]
                    } else {
                        rect.right()
                    };
                    glyphst_ui::text::row(
                        ui,
                        pos2(columns[column] + 8.0, y),
                        text.to_owned(),
                        theme::mono(size),
                        color,
                        right - columns[column] - 16.0,
                    );
                };
            cell(ui, 0, &doc.title, tokens.text, 12.0);
            cell(ui, 1, &doc.authors, tokens.muted, 11.0);
            cell(ui, 2, &doc.kind, tokens.muted, 11.0);
            match doc.progress() {
                Some(progress) if progress < 0.999 => {
                    let label = doc.progress_label();
                    Meter::new(progress)
                        .bar()
                        .width(110.0)
                        .label(&label)
                        .accent_label(true)
                        .paint(
                            ui,
                            Rect::from_min_size(pos2(columns[3] + 8.0, y - 7.0), vec2(116.0, 14.0)),
                        );
                }
                _ => {
                    let label = doc.progress_label();
                    let label = if label == "done" {
                        "done ✓".to_owned()
                    } else {
                        label
                    };
                    cell(ui, 3, &label, tokens.muted, 11.0);
                }
            }
            let opened = doc
                .opened
                .map_or_else(|| "new".to_owned(), |at| day(at, now));
            cell(ui, 4, &opened, tokens.muted, 11.0);
            if response
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .clicked()
            {
                action = Some(HomeAction::Open(doc.path.clone()));
            }
        }
        action
    }

    /// The activity log grouped by day, `live`, over the drop zone.
    fn activity(
        &mut self,
        ui: &mut egui::Ui,
        tokens: &Tokens,
        library: &LibraryStore,
        picking: bool,
        open_key: Option<&str>,
    ) -> Option<HomeAction> {
        let rect = ui.max_rect();
        ui.painter()
            .rect_filled(rect, 0, tokens.surface.to_opaque());
        ui.painter()
            .vline(rect.left() + 0.5, rect.y_range(), tokens.hairline());
        let header = Header::new("activity")
            .trailing("live")
            .action(PaneAction::new("x", "hide the activity"))
            .show(ui);
        if header.clicked == Some(0) {
            self.activity_hidden = true;
        }
        let mut action = None;
        let drop_height = 210.0;
        let list = Rect::from_min_max(
            pos2(rect.left() + 12.0, ui.cursor().top() + 6.0),
            pos2(rect.right() - 12.0, rect.bottom() - drop_height - 12.0),
        );
        let mut list_ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(list)
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        egui::ScrollArea::vertical()
            .id_salt("home-activity")
            .auto_shrink(false)
            .show(&mut list_ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                let events = library.activity();
                if events.is_empty() {
                    ui.add_space(12.0);
                    ui.label(
                        egui::RichText::new("what you open, mark and ask shows here")
                            .font(theme::mono(11.0))
                            .color(tokens.faint),
                    );
                }
                let today = Local::now().date_naive();
                let mut group: Option<chrono::NaiveDate> = None;
                for event in events.iter().rev().take(200) {
                    let Some(time) = Local.timestamp_opt(event.at as i64, 0).single() else {
                        continue;
                    };
                    let date = time.date_naive();
                    if group.is_some_and(|group| group != date)
                        || (group.is_none() && date != today)
                    {
                        let label = match (today - date).num_days() {
                            0 => "today".to_owned(),
                            1 => "yesterday".to_owned(),
                            _ => time.format("%a %d %b").to_string().to_lowercase(),
                        };
                        ui.add_space(4.0);
                        motif::rule_title(ui, &label, None);
                    }
                    group = Some(date);
                    let (row, response) =
                        ui.allocate_exact_size(vec2(ui.available_width(), 22.0), Sense::click());
                    if response.hovered() {
                        ui.painter().rect_filled(row, 0, tokens.hover);
                    }
                    let y = row.center().y;
                    let font = theme::mono(11.0);
                    ui.painter().text(
                        pos2(row.left() + 2.0, y),
                        Align2::LEFT_CENTER,
                        format!("{:02}:{:02}", time.hour(), time.minute()),
                        theme::mono(10.0),
                        tokens.faint,
                    );
                    ui.painter().text(
                        pos2(row.left() + 52.0, y),
                        Align2::LEFT_CENTER,
                        event.kind.verb(),
                        font.clone(),
                        tokens.muted,
                    );
                    let name = event
                        .path
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    let (text, color) = match event.kind {
                        ActivityKind::Read => (
                            format!("{name} · {}", minutes(event.seconds)),
                            tokens.text_2,
                        ),
                        ActivityKind::Highlight | ActivityKind::Bookmark => {
                            (format!("{} · {name}", event.detail), tokens.accent_text)
                        }
                        ActivityKind::Ask => (format!("\"{}\"", event.detail), tokens.info),
                        ActivityKind::Translate => (format!("{name} {}", event.detail), tokens.ok),
                        _ => (format!("{name} {}", event.detail), tokens.text_2),
                    };
                    glyphst_ui::text::row(
                        ui,
                        pos2(row.left() + 96.0, y),
                        text,
                        font,
                        color,
                        row.right() - row.left() - 100.0,
                    );
                    if response
                        .on_hover_cursor(egui::CursorIcon::PointingHand)
                        .clicked()
                    {
                        action = Some(HomeAction::Open(event.path.clone()));
                    }
                }
            });
        // The drop zone.
        let zone = Rect::from_min_max(
            pos2(rect.left() + 12.0, rect.bottom() - drop_height),
            pos2(rect.right() - 12.0, rect.bottom() - 12.0),
        );
        ui.painter().rect(
            zone,
            tokens.ctl_radius(),
            tokens.raised,
            tokens.edge(),
            StrokeKind::Inside,
        );
        let field = Rect::from_center_size(
            zone.center() - vec2(0.0, 36.0),
            vec2(zone.width() - 80.0, 60.0),
        );
        motif::dot_field(ui, field, 12.0, 0.9, |_, _| 0.0);
        let arrow = Rect::from_center_size(field.center(), vec2(26.0, 26.0));
        ui.painter().rect(
            arrow,
            tokens.ctl_radius(),
            tokens.raised,
            tokens.edge(),
            StrokeKind::Inside,
        );
        icons::paint(ui, arrow, "arrow-down", 14.0, tokens.muted);
        ui.painter().text(
            zone.center() + vec2(0.0, 18.0),
            Align2::CENTER_CENTER,
            "drop pdf · epub · cbz · djvu",
            theme::mono(11.0),
            tokens.text_2,
        );
        // Centred by the width it took last frame: the key differs by keymap.
        let width_id = egui::Id::new("home-open-row");
        let width = ui
            .ctx()
            .data(|data| data.get_temp::<f32>(width_id))
            .unwrap_or(220.0);
        let row = Rect::from_center_size(zone.center() + vec2(0.0, 48.0), vec2(width, 24.0));
        let mut row_ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(row)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        row_ui.spacing_mut().item_spacing.x = 8.0;
        row_ui.label(
            egui::RichText::new("or")
                .font(theme::mono(11.0))
                .color(tokens.faint),
        );
        if row_ui
            .add_enabled(!picking, Button::bracket("open file"))
            .clicked()
        {
            action = Some(HomeAction::OpenDialog);
        }
        if let Some(key) = open_key {
            row_ui.add(Key(&glyphst_ui::key::display(key)));
        }
        let taken = row_ui.min_rect().width();
        if (taken - width).abs() > 0.5 {
            ui.ctx().data_mut(|data| data.insert_temp(width_id, taken));
            ui.ctx().request_repaint();
        }
        action
    }
}

/// The documents home lists: those opened before and those in the
/// library, with what is known of each.
fn documents(library: &LibraryStore, page_counts: &BTreeMap<String, usize>) -> Vec<Doc> {
    let mut docs: HashMap<PathBuf, Doc> = HashMap::new();
    let doc = |path: &Path| {
        let item = library.library_item_for_path(path);
        let authors = item
            .map(|item| {
                let names: Vec<&str> = item
                    .creators
                    .iter()
                    .map(|creator| creator.name.as_str())
                    .collect();
                match names.len() {
                    0 => String::new(),
                    1 | 2 => names.join(", "),
                    _ => format!("{} et al.", names[0]),
                }
            })
            .unwrap_or_default();
        let title = item
            .map(|item| item.title.clone())
            .filter(|title| !title.trim().is_empty())
            .unwrap_or_else(|| title_of(path));
        Doc {
            path: path.to_path_buf(),
            title,
            authors,
            year: item.and_then(|item| item.year),
            kind: kind_of(path),
            page: 1,
            pages: page_counts.get(&path.display().to_string()).copied(),
            opened: None,
        }
    };
    for entry in library.history_entries() {
        let mut found = doc(&entry.path);
        found.page = entry.position.page.max(1);
        found.opened = Some(entry.opened);
        docs.insert(entry.path.clone(), found);
    }
    for attachment in library.library_attachments() {
        docs.entry(attachment.path.clone())
            .or_insert_with(|| doc(&attachment.path));
    }
    docs.into_values().collect()
}

/// `── wed 15 oct ── 09:42 ──`, the greeting and the week's numbers, with
/// the wordmark at the right.
fn greeting(ui: &mut egui::Ui, tokens: &Tokens, library: &LibraryStore, docs: &[Doc]) {
    let now = Local::now();
    let date = now.format("%a %d %b").to_string().to_lowercase();
    let time = now.format("%H:%M").to_string();
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 96.0), Sense::hover());
    let top = Rect::from_min_size(rect.min, vec2(240.0, 18.0));
    motif::paint_rule_title(ui, top, &format!("{date}  ──  {time}"), None, true);
    let part = match now.hour() {
        5..=11 => "Good morning",
        12..=17 => "Good afternoon",
        _ => "Good evening",
    };
    let going = docs
        .iter()
        .filter(|doc| {
            doc.progress()
                .is_some_and(|progress| progress > 0.0 && progress < 0.999)
        })
        .count();
    let count = match going {
        0 => "Nothing is in progress.".to_owned(),
        1 => "One document is in progress.".to_owned(),
        n => format!("{} documents are in progress.", number_word(n)),
    };
    ui.painter().text(
        pos2(rect.left(), rect.top() + 46.0),
        Align2::LEFT_CENTER,
        format!("{part}. {count}"),
        theme::semibold(22.0),
        tokens.text,
    );
    let week_start = {
        let days = u64::from(now.weekday().num_days_from_monday());
        let midnight = now
            .date_naive()
            .and_hms_opt(0, 0, 0)
            .and_then(|time| Local.from_local_datetime(&time).single());
        midnight.map_or(0, |midnight| {
            midnight.timestamp().max(0) as u64 - days * 86_400
        })
    };
    let read = library.reading_seconds(week_start, None);
    let chats = library.ai_sessions().len();
    let stats = format!(
        "{} read this week · {} highlights · {chats} chats",
        hours(read),
        library.highlight_count()
    );
    ui.painter().text(
        pos2(rect.left(), rect.top() + 74.0),
        Align2::LEFT_CENTER,
        stats,
        theme::mono(11.0),
        tokens.muted,
    );
    // The wordmark: the name in bold mono over its offset shadow line.
    if rect.width() > 700.0 {
        let at = pos2(rect.right(), rect.top() + 44.0);
        let font = theme::mono_bold(44.0);
        ui.painter().text(
            at + vec2(3.0, 3.0),
            Align2::RIGHT_CENTER,
            "GLYPHST",
            font.clone(),
            tokens.rule_strong,
        );
        ui.painter()
            .text(at, Align2::RIGHT_CENTER, "GLYPHST", font, tokens.hover);
        let galley =
            ui.painter()
                .layout_no_wrap("GLYPHST".into(), theme::mono_bold(44.0), tokens.text);
        let text_rect = Align2::RIGHT_CENTER.anchor_size(at, galley.size());
        // Scan lines across it, as the design's banner has.
        let mut y = text_rect.top() + 6.0;
        while y < text_rect.bottom() - 4.0 {
            ui.painter().hline(
                text_rect.x_range(),
                y,
                egui::Stroke::new(1.0, Tokens::fade(tokens.bg, 0.5)),
            );
            y += 4.0;
        }
    }
}

/// The document to continue: its cover, title and place, `continue →`
/// and `[ ask about it ]`.
fn continue_card(
    ui: &mut egui::Ui,
    tokens: &Tokens,
    library: &LibraryStore,
    doc: &Doc,
) -> Option<HomeAction> {
    let mut action = None;
    pane::card(ui.ctx())
        .inner_margin(egui::Margin::same(16))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let label = doc.progress_label();
                let cover = DocCover::new(&doc.title, &doc.kind_line())
                    .progress(None, "")
                    .size(vec2(100.0, 132.0))
                    .show(ui);
                let _ = label;
                if cover.clicked() {
                    action = Some(HomeAction::Open(doc.path.clone()));
                }
                ui.add_space(16.0);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 6.0;
                    ui.add_space(12.0);
                    ui.label(
                        egui::RichText::new("continue reading")
                            .font(theme::mono(10.0))
                            .color(tokens.accent_text),
                    );
                    ui.label(
                        egui::RichText::new(&doc.title)
                            .font(theme::semibold(20.0))
                            .color(tokens.text),
                    );
                    let mut meta = Vec::new();
                    if !doc.authors.is_empty() {
                        meta.push(doc.authors.to_lowercase());
                    }
                    if let Some(year) = doc.year {
                        meta.push(year.to_string());
                    }
                    meta.push(doc.kind.clone());
                    if let Some(pages) = doc.pages {
                        meta.push(format!("{pages} pages"));
                    }
                    ui.label(
                        egui::RichText::new(meta.join(" · "))
                            .font(theme::mono(11.0))
                            .color(tokens.muted),
                    );
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 12.0;
                        if let Some(progress) = doc.progress() {
                            ui.add(Meter::new(progress).bar().width(180.0));
                        }
                        let today = {
                            let now = current_timestamp();
                            now - now % 86_400
                        };
                        let minutes_today = library.reading_seconds(today, Some(&doc.path));
                        let mut place = match doc.pages {
                            Some(pages) => format!("p.{} / {pages}", doc.page),
                            None => format!("p.{}", doc.page),
                        };
                        if minutes_today >= 60 {
                            place.push_str(&format!(" · {} today", minutes(minutes_today)));
                        }
                        ui.label(
                            egui::RichText::new(place)
                                .font(theme::mono(11.0))
                                .color(tokens.muted),
                        );
                    });
                });
                // The actions at the card's right edge.
                ui.add_space((ui.available_width() - 150.0).max(0.0));
                ui.with_layout(egui::Layout::top_down(egui::Align::Max), |ui| {
                    ui.vertical(|ui| {
                        ui.add_space(40.0);
                        ui.horizontal(|ui| {
                            if ui
                                .add(Button::primary("continue").trailing_icon("arrow-right"))
                                .clicked()
                            {
                                action = Some(HomeAction::Open(doc.path.clone()));
                            }
                            ui.add(Key("enter"));
                        });
                        if ui.add(Button::bracket_muted("ask about it")).clicked() {
                            action = Some(HomeAction::AskAbout(doc.path.clone()));
                        }
                    });
                });
            });
        });
    // Enter continues, while nothing has the keys.
    if ui.input(|i| i.key_pressed(egui::Key::Enter))
        && ui.ctx().memory(|memory| memory.focused().is_none())
    {
        action = Some(HomeAction::Open(doc.path.clone()));
    }
    action
}

fn notice(ui: &mut egui::Ui, tokens: &Tokens, error: &str) {
    egui::Frame::new()
        .fill(tokens.err_soft)
        .stroke(egui::Stroke::new(1.0, Tokens::fade(tokens.err, 0.45)))
        .corner_radius(tokens.ctl_radius())
        .inner_margin(12)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(
                egui::RichText::new("the library is unavailable")
                    .font(theme::mono(12.0))
                    .color(tokens.err),
            );
            ui.label(
                egui::RichText::new(format!(
                    "Reading positions are not remembered this session. \
                     Another Glyphst Reader may be running and holding it.\n{error}"
                ))
                .font(theme::regular(13.0))
                .color(tokens.text),
            );
        });
}

fn empty(ui: &mut egui::Ui, tokens: &Tokens) {
    ui.vertical_centered(|ui| {
        let (rect, _) = ui.allocate_exact_size(vec2(60.0, 50.0), Sense::hover());
        motif::dot_mark(ui, rect.center(), 7.0);
        ui.add_space(8.0);
        ui.label(
            egui::RichText::new("no documents yet")
                .font(theme::mono(12.0))
                .color(tokens.text_2),
        );
        ui.label(
            egui::RichText::new("drop one anywhere, or open one with ctrl o")
                .font(theme::mono(11.0))
                .color(tokens.faint),
        );
    });
}

/// While files are dragged over the window: a dotted accent outline.
fn drop_hint(ui: &egui::Ui, tokens: &Tokens, rect: Rect) {
    if ui.ctx().input(|i| i.raw.hovered_files.is_empty()) {
        return;
    }
    let area = rect.shrink(12.0);
    let painter = ui.ctx().layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::new("drop-hint"),
    ));
    painter.rect_filled(area, 0, Tokens::fade(tokens.accent_soft, 0.85));
    for (from, to) in [
        (area.left_top(), area.right_top()),
        (area.right_top(), area.right_bottom()),
        (area.right_bottom(), area.left_bottom()),
        (area.left_bottom(), area.left_top()),
    ] {
        painter.add(egui::Shape::dotted_line(
            &[from, to],
            tokens.accent,
            6.0,
            1.2,
        ));
    }
    painter.text(
        area.center(),
        Align2::CENTER_CENTER,
        "drop to open",
        theme::mono(16.0),
        tokens.accent_text,
    );
}

/// The columns' left edges across `x`.
fn table_columns(x: egui::Rangef) -> [f32; 5] {
    let width = x.span();
    [
        x.min,
        x.min + width * 0.44,
        x.min + width * 0.64,
        x.min + width * 0.72,
        x.min + width * 0.88,
    ]
}

/// `pdf`, `epub`… from the file's extension.
fn kind_of(path: &Path) -> String {
    path.extension()
        .map(|extension| extension.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

/// The file name without its extension, as a title.
fn title_of(path: &Path) -> String {
    path.file_stem()
        .or_else(|| path.file_name())
        .map(|name| name.to_string_lossy().replace(['_', '-'], " "))
        .unwrap_or_else(|| path.display().to_string())
}

/// When a document was opened: `today`, `yesterday`, `mon`, `oct 02`.
fn day(at: u64, now: u64) -> String {
    let (Some(then), Some(today)) = (
        Local.timestamp_opt(at as i64, 0).single(),
        Local.timestamp_opt(now as i64, 0).single(),
    ) else {
        return String::new();
    };
    match (today.date_naive() - then.date_naive()).num_days() {
        0 => "today".into(),
        1 => "yesterday".into(),
        2..=6 => then.format("%a").to_string().to_lowercase(),
        _ => then.format("%b %d").to_string().to_lowercase(),
    }
}

/// `38 min`, `2h 05`.
fn minutes(seconds: u64) -> String {
    let minutes = seconds / 60;
    if minutes < 60 {
        format!("{minutes} min")
    } else {
        format!("{}h {:02}", minutes / 60, minutes % 60)
    }
}

/// `12h`, `40 min`.
fn hours(seconds: u64) -> String {
    if seconds >= 3_600 {
        format!("{}h", seconds / 3_600)
    } else {
        format!("{} min", seconds / 60)
    }
}

/// `Three`, `12`.
fn number_word(n: usize) -> String {
    const WORDS: [&str; 10] = [
        "Zero", "One", "Two", "Three", "Four", "Five", "Six", "Seven", "Eight", "Nine",
    ];
    WORDS
        .get(n)
        .map_or_else(|| n.to_string(), |word| (*word).to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_and_kinds_come_from_the_file_name() {
        assert_eq!(
            title_of(Path::new("/books/zero2prod_20240903.pdf")),
            "zero2prod 20240903"
        );
        assert_eq!(kind_of(Path::new("/books/a.EPUB")), "epub");
    }

    #[test]
    fn progress_reads_from_the_page_and_count() {
        let doc = |page, pages, opened| Doc {
            path: PathBuf::from("/a.pdf"),
            title: "a".into(),
            authors: String::new(),
            year: None,
            kind: "pdf".into(),
            page,
            pages,
            opened,
        };
        assert_eq!(doc(4, Some(15), Some(1)).progress_label(), "27%");
        assert_eq!(doc(15, Some(15), Some(1)).progress_label(), "done");
        assert_eq!(doc(1, None, None).progress_label(), "new");
        assert_eq!(doc(1, None, Some(1)).progress_label(), "—");
        assert_eq!(doc(4, Some(15), Some(1)).kind_line(), "pdf · 15p");
    }

    #[test]
    fn durations_read_as_the_design_writes_them() {
        assert_eq!(minutes(38 * 60), "38 min");
        assert_eq!(minutes(125 * 60), "2h 05");
        assert_eq!(hours(12 * 3_600 + 5), "12h");
        assert_eq!(number_word(3), "Three");
        assert_eq!(number_word(12), "12");
    }
}
