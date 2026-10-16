//! Home (frame 04): the day and a greeting with the week's reading, the
//! wordmark, a heatmap of the last year's reading, the document to
//! continue, one row of recent documents as their first pages
//! (`crate::covers`), the library as a table to filter and sort, and at
//! the right the activity log over a drop zone.

use std::{
    collections::{BTreeMap, HashSet},
    path::{Path, PathBuf},
};

use chrono::{Datelike, Local, TimeZone, Timelike};
use egui::{Align2, Rect, Sense, StrokeKind, pos2, vec2};
use glyphst_library::{ActivityKind, LibraryStore, current_timestamp};
use glyphst_ui::{
    Tip,
    button::Button,
    cover::PageCover,
    heatmap::{self, Heatmap},
    key::Key,
    layout,
    meter::Meter,
    motif,
    pane::{self, Action as PaneAction, Header},
    thumb::PageThumb,
};

use crate::{
    covers::Covers,
    icons,
    library_view::{LibraryAction, LibraryView, Mode},
    reading_days::{self, ReadingDays},
    theme::{self, Tokens},
};

/// A recent document's cover, and the room between covers.
pub const COVER_WIDTH: f32 = 148.0;
pub const COVER_GAP: f32 = 16.0;
/// Room for the numbers beside home's heatmap, and the gap before them.
const HEAT_STATS: f32 = 140.0;
const HEAT_GAP: f32 = 24.0;
/// With fewer months than this beside the numbers, the numbers go under
/// the map.
const HEAT_MIN_MONTHS: u32 = 4;
/// The activity pane's width.
const ACTIVITY_WIDTH: f32 = 380.0;

pub enum HomeAction {
    Open(PathBuf),
    OpenDialog,
    /// Opens a document with the assistant beside it.
    AskAbout(PathBuf),
    /// Opens the reading activity tab, at a day.
    Activity(Option<chrono::NaiveDate>),
    /// Opens the library tab.
    Library,
}

pub struct Home {
    library: LibraryView,
    /// The activity pane is shown.
    activity_hidden: bool,
    heat: Option<Heat>,
    /// The column's rect last frame (for tests of the layout).
    pub(crate) content: Rect,
}

impl Default for Home {
    fn default() -> Self {
        Self {
            library: LibraryView::new("home-library"),
            activity_hidden: false,
            heat: None,
            content: Rect::NOTHING,
        }
    }
}

/// The heatmap's days, worked out again when a session is added, the day
/// changes or the map's width does.
struct Heat {
    key: (usize, chrono::NaiveDate, u32),
    /// The months shown: 12 for the year of weeks.
    months_shown: u32,
    days: ReadingDays,
    levels: Vec<Option<u8>>,
    months: Vec<(usize, &'static str)>,
    streaks: (usize, usize),
}

impl Heat {
    fn new(library: &LibraryStore, today: chrono::NaiveDate, months: u32) -> Self {
        let sessions = library.reading_sessions();
        let days = if months >= 12 {
            ReadingDays::weeks_to(sessions, today, 53)
        } else {
            ReadingDays::months_to(sessions, today, months)
        };
        Self {
            key: (sessions.len(), today, months),
            months_shown: months,
            levels: days.levels(),
            months: days.months(),
            streaks: reading_days::streaks(sessions, today),
            days,
        }
    }
}

/// What home shows besides the library.
pub struct HomeContext<'a> {
    pub library: &'a LibraryStore,
    /// Page counts of documents opened before, by path.
    pub page_counts: &'a BTreeMap<String, usize>,
    pub picking: bool,
    /// The key of "open with system dialog", as preferences keep it.
    pub open_key: Option<String>,
    pub covers: &'a mut Covers,
}

/// A document as home and the launcher list it.
#[derive(Clone, Debug)]
pub struct Doc {
    pub path: PathBuf,
    pub title: String,
    pub authors: String,
    year: Option<i32>,
    pub kind: String,
    /// The page last read (from 1) and the page count, when known.
    page: usize,
    pages: Option<usize>,
    pub opened: Option<u64>,
}

impl Doc {
    pub fn progress(&self) -> Option<f32> {
        let pages = self.pages.filter(|&pages| pages > 0)?;
        Some((self.page as f32 / pages as f32).clamp(0.0, 1.0))
    }

    pub fn progress_label(&self) -> String {
        match self.progress() {
            Some(progress) if progress >= 0.999 => "done".into(),
            Some(progress) => format!("{:.0}%", progress * 100.0),
            None if self.opened.is_none() => "new".into(),
            None => "—".into(),
        }
    }

    pub fn page_count(&self) -> Option<usize> {
        self.pages
    }

    /// `pdf · 15p`.
    pub fn kind_line(&self) -> String {
        match self.pages {
            Some(pages) => format!("{} · {pages}p", self.kind),
            None => self.kind.clone(),
        }
    }
}

impl Home {
    pub fn ui(&mut self, ui: &mut egui::Ui, mut home: HomeContext<'_>) -> Option<HomeAction> {
        let tokens = Tokens::get(ui.ctx());
        let mut action = None;
        let full = ui.max_rect();
        ui.painter().rect_filled(full, 0, tokens.bg);
        let docs = documents(home.library, home.page_counts);
        // The activity pane shows while the column keeps its minimum.
        let show_activity = !self.activity_hidden
            && full.width() - ACTIVITY_WIDTH >= layout::MIN_COLUMN + 2.0 * layout::GUTTER;
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
                let (chosen, content) = layout::column(ui, layout::MAX_COLUMN, |ui, column| {
                    self.column(ui, &tokens, &mut home, &docs, column)
                });
                self.content = content;
                if chosen.is_some() {
                    action = chosen;
                }
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

    /// Home's sections in the column, `column` wide.
    fn column(
        &mut self,
        ui: &mut egui::Ui,
        tokens: &Tokens,
        home: &mut HomeContext<'_>,
        docs: &[Doc],
        column: f32,
    ) -> Option<HomeAction> {
        let mut action = None;
        ui.add_space(28.0);
        greeting(ui, tokens, home.library, docs);
        if let Some(error) = home.library.startup_error() {
            ui.add_space(16.0);
            notice(ui, tokens, error);
        }
        ui.add_space(20.0);
        if let Some(chosen) = self.reading(ui, tokens, home.library, docs, column) {
            action = Some(chosen);
        }
        // The documents come most recently opened first.
        let continuing = docs
            .iter()
            .find(|doc| doc.opened.is_some() && doc.path.exists());
        if let Some(doc) = continuing {
            ui.add_space(24.0);
            let cover = home.covers.get(&doc.path);
            if let Some(chosen) = continue_card(ui, tokens, home.library, doc, cover, column) {
                action = Some(chosen);
            }
        }
        let recent: Vec<&Doc> = docs
            .iter()
            .filter(|doc| doc.opened.is_some())
            .filter(|doc| Some(&doc.path) != continuing.map(|doc| &doc.path))
            .collect();
        if !recent.is_empty() {
            ui.add_space(24.0);
            // One row: as many covers as the column holds.
            let shown = covers_in(column).min(recent.len());
            motif::rule_title(ui, "recent", Some(&format!("{shown} of {}", recent.len())));
            ui.add_space(8.0);
            if let Some(chosen) = cover_row(ui, home.covers, &recent[..shown], column) {
                action = Some(chosen);
            }
        }
        if docs.is_empty() {
            ui.add_space(48.0);
            empty(ui, tokens);
        } else {
            ui.add_space(28.0);
            match self.library.ui(ui, docs, column, Mode::Home, None) {
                Some(LibraryAction::Open(path)) => action = Some(HomeAction::Open(path)),
                Some(LibraryAction::Tab) => action = Some(HomeAction::Library),
                Some(LibraryAction::OpenDialog) => action = Some(HomeAction::OpenDialog),
                None => {}
            }
        }
        ui.add_space(48.0);
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
        if drop_zone(ui, tokens, zone, picking, open_key, "home-open-row") {
            action = Some(HomeAction::OpenDialog);
        }
        action
    }
}

/// A drop zone in `zone`: a dot field around an arrow, `drop pdf · epub ·
/// cbz · djvu` and `or [ open file ]` with its key. True when `open file`
/// is clicked.
pub fn drop_zone(
    ui: &mut egui::Ui,
    tokens: &Tokens,
    zone: Rect,
    picking: bool,
    open_key: Option<&str>,
    id: &str,
) -> bool {
    let mut open = false;
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
    let width_id = egui::Id::new(id);
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
        open = true;
    }
    if let Some(key) = open_key {
        row_ui.add(Key(&glyphst_ui::key::display(key)));
    }
    let taken = row_ui.min_rect().width();
    if (taken - width).abs() > 0.5 {
        ui.ctx().data_mut(|data| data.insert_temp(width_id, taken));
        ui.ctx().request_repaint();
    }
    open
}

/// The documents home lists, with what is known of each: those opened
/// before, most recent first, then the rest of the library by path. The
/// order is fixed, so equal times never trade places between frames.
pub fn documents(library: &LibraryStore, page_counts: &BTreeMap<String, usize>) -> Vec<Doc> {
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
    let mut history: Vec<_> = library.history_entries().iter().collect();
    history.sort_by(|a, b| b.opened.cmp(&a.opened).then_with(|| a.path.cmp(&b.path)));
    let mut seen = HashSet::new();
    let mut docs = Vec::new();
    for entry in history {
        if seen.insert(entry.path.as_path()) {
            let mut found = doc(&entry.path);
            found.page = entry.position.page.max(1);
            found.opened = Some(entry.opened);
            docs.push(found);
        }
    }
    let mut attachments: Vec<&Path> = library
        .library_attachments()
        .iter()
        .map(|attachment| attachment.path.as_path())
        .filter(|path| !seen.contains(path))
        .collect();
    attachments.sort();
    attachments.dedup();
    docs.extend(attachments.into_iter().map(doc));
    docs
}

/// How many covers fit across `width`.
pub fn covers_in(width: f32) -> usize {
    (((width + COVER_GAP) / (COVER_WIDTH + COVER_GAP)).floor() as usize).max(1)
}

/// `docs` as one row of first-page covers filling `width`.
pub fn cover_row(
    ui: &mut egui::Ui,
    covers: &mut Covers,
    docs: &[&Doc],
    width: f32,
) -> Option<HomeAction> {
    let mut action = None;
    let count = covers_in(width).max(docs.len()) as f32;
    let cover_width = ((width - (count - 1.0) * COVER_GAP) / count).max(60.0);
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = COVER_GAP;
        for doc in docs {
            let label = doc.progress_label();
            let kind = doc.kind_line();
            let response = PageCover::new(&doc.title, &kind)
                .progress(doc.progress(), &label)
                .page(covers.get(&doc.path))
                .width(cover_width)
                .show(ui)
                .tip(&doc.path.display().to_string());
            if response.clicked() {
                action = Some(HomeAction::Open(doc.path.clone()));
            }
        }
    });
    action
}

impl Home {
    /// The reading (frame 04): `── reading ── last 12 months` and
    /// `[ activity → ]` on the column's right edge, the heatmap with its
    /// numbers at the right (under it when even a few months would not fit
    /// beside them), and under it the day pointed at and the legend. A
    /// column too narrow for a year of weeks shows the last whole months
    /// that fit (frame 04e).
    fn reading(
        &mut self,
        ui: &mut egui::Ui,
        tokens: &Tokens,
        library: &LibraryStore,
        docs: &[Doc],
        width: f32,
    ) -> Option<HomeAction> {
        let mut action = None;
        let today = Local::now().date_naive();
        let (months, beside) = heat_months(today, width);
        if self
            .heat
            .as_ref()
            .is_none_or(|heat| heat.key != (library.reading_sessions().len(), today, months))
        {
            self.heat = Some(Heat::new(library, today, months));
        }
        let heat = self.heat.as_ref()?;
        let span = if heat.months_shown == 1 {
            "this month".to_owned()
        } else {
            format!("last {} months", heat.months_shown)
        };
        // The rule, and `[ activity → ]` on the right edge.
        let (row, _) = ui.allocate_exact_size(vec2(width, 20.0), Sense::hover());
        let mut button_ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(row)
                .layout(egui::Layout::right_to_left(egui::Align::Center)),
        );
        let button = button_ui.add(Button::bracket("activity →"));
        if button.clicked() {
            action = Some(HomeAction::Activity(None));
        }
        motif::paint_rule_title(
            ui,
            row.with_max_x(button.rect.left() - 12.0),
            &format!("reading  ──  {span}"),
            None,
            true,
        );
        ui.add_space(10.0);
        let map = Heatmap::new(&heat.levels)
            .months(&heat.months)
            .today(heat.days.index(today).unwrap_or(usize::MAX));
        let map_size = map.size();
        let stats_height = 3.0 * 47.0;
        let height = if beside {
            map_size.y.max(stats_height)
        } else {
            map_size.y
        };
        let (area, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
        let mut map_ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(Rect::from_min_size(area.min, map_size))
                .layout(egui::Layout::left_to_right(egui::Align::Min)),
        );
        let shown = map.show(&mut map_ui);
        if let Some(day) = shown.clicked {
            action = Some(HomeAction::Activity(Some(heat.days.date(day))));
        }
        let (current, longest) = heat.streaks;
        let days = heat.days.reading_days().max(1) as u64;
        let stats = [
            (
                hours(heat.days.total()),
                format!("in {}", span.trim_start_matches("last ")),
            ),
            (
                format!("{current} {}", if current == 1 { "day" } else { "days" }),
                format!("streak · best {longest}"),
            ),
            (
                minutes(heat.days.total() / days),
                "a reading day".to_owned(),
            ),
        ];
        let stat = |ui: &egui::Ui, at: egui::Pos2, value: &str, key: &str| {
            ui.painter().text(
                at,
                Align2::LEFT_TOP,
                value,
                theme::mono_strong(15.0),
                tokens.text,
            );
            ui.painter().text(
                at + vec2(0.0, 22.0),
                Align2::LEFT_TOP,
                key,
                theme::mono(10.0),
                tokens.faint,
            );
        };
        if beside {
            let x = area.right() - HEAT_STATS;
            for (index, (value, key)) in stats.iter().enumerate() {
                stat(
                    ui,
                    pos2(x, area.top() + 18.0 + index as f32 * 47.0),
                    value,
                    key,
                );
            }
        }
        ui.add_space(6.0);
        // The day pointed at (today otherwise), and the legend on the
        // column's right edge.
        let (foot, _) = ui.allocate_exact_size(vec2(width, 12.0), Sense::hover());
        let mut legend_ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(foot)
                .layout(egui::Layout::right_to_left(egui::Align::Center)),
        );
        let legend = heatmap::legend(&mut legend_ui).rect;
        let day = shown.hovered.map_or(today, |day| heat.days.date(day));
        glyphst_ui::text::row(
            ui,
            pos2(foot.left() + 30.0, foot.center().y),
            day_summary(library, docs, day),
            theme::mono(9.0),
            tokens.faint,
            (legend.left() - foot.left() - 46.0).max(0.0),
        );
        if !beside {
            ui.add_space(12.0);
            let (under, _) = ui.allocate_exact_size(vec2(width, 40.0), Sense::hover());
            let step = width / stats.len() as f32;
            for (index, (value, key)) in stats.iter().enumerate() {
                stat(
                    ui,
                    pos2(under.left() + index as f32 * step, under.top()),
                    value,
                    key,
                );
            }
        }
        action
    }
}

/// The months home's heatmap shows in a column `width` wide, and whether
/// its numbers sit beside it: a year of weeks when it fits beside them,
/// else the last whole months that fit, under them when fewer than
/// [`HEAT_MIN_MONTHS`] would fit beside.
fn heat_months(today: chrono::NaiveDate, width: f32) -> (u32, bool) {
    let beside = Heatmap::weeks_in(width - HEAT_STATS - HEAT_GAP, 12.0);
    if beside >= 53 {
        return (12, true);
    }
    let months = reading_days::months_in(today, beside);
    if months >= HEAT_MIN_MONTHS {
        return (months, true);
    }
    let alone = Heatmap::weeks_in(width, 12.0);
    if alone >= 53 {
        (12, false)
    } else {
        (reading_days::months_in(today, alone), false)
    }
}

/// `wed 15 oct · 38 min · attention.pdf, bert-2018.pdf`.
fn day_summary(library: &LibraryStore, docs: &[Doc], day: chrono::NaiveDate) -> String {
    let sessions = reading_days::sessions_on(library.reading_sessions(), day);
    let seconds: u64 = sessions.iter().map(|session| session.seconds).sum();
    let mut names: Vec<String> = Vec::new();
    for session in &sessions {
        let name = docs
            .iter()
            .find(|doc| doc.path == session.path)
            .map(|doc| doc.title.clone())
            .unwrap_or_else(|| title_of(&session.path));
        if !names.contains(&name) {
            names.push(name);
        }
    }
    let mut line = reading_days::day_label(day);
    if seconds == 0 {
        line.push_str(" · no reading");
    } else {
        line.push_str(&format!(" · {}", reading_days::duration_label(seconds)));
        line.push_str(&format!(" · {}", names.join(", ").to_lowercase()));
    }
    line
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

/// The document to continue: its cover, title (two lines at most) and
/// place, and at the right edge `continue →` and `[ ask about it ]`.
fn continue_card(
    ui: &mut egui::Ui,
    tokens: &Tokens,
    library: &LibraryStore,
    doc: &Doc,
    cover: Option<crate::covers::CoverTexture>,
    width: f32,
) -> Option<HomeAction> {
    const COVER: f32 = 96.0;
    const ACTIONS: f32 = 170.0;
    const GAP: f32 = 20.0;
    let mut action = None;
    pane::card(ui.ctx())
        .inner_margin(egui::Margin::same(16))
        .show(ui, |ui| {
            // The margins and the card's 1pt edge on each side.
            let inner = width - 34.0;
            ui.set_width(inner);
            let info = (inner - COVER - ACTIONS - 2.0 * GAP).max(120.0);
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = GAP;
                let cover = PageThumb {
                    label: "",
                    current: false,
                    texture: cover.map(|(texture, _)| texture),
                    aspect: cover.map_or(96.0 / 124.0, |(_, aspect)| aspect.max(96.0 / 192.0)),
                    width: COVER,
                }
                .show(ui)
                .on_hover_cursor(egui::CursorIcon::PointingHand);
                if cover.clicked() {
                    action = Some(HomeAction::Open(doc.path.clone()));
                }
                ui.vertical(|ui| {
                    ui.set_width(info);
                    ui.spacing_mut().item_spacing.y = 6.0;
                    ui.add_space(8.0);
                    ui.label(
                        egui::RichText::new("continue reading")
                            .font(theme::mono(10.0))
                            .color(tokens.accent_text),
                    );
                    let mut title = egui::text::LayoutJob::simple(
                        doc.title.clone(),
                        theme::semibold(20.0),
                        tokens.text,
                        info,
                    );
                    title.wrap.max_rows = 2;
                    title.wrap.break_anywhere = false;
                    title.wrap.overflow_character = Some('…');
                    ui.label(title);
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
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(meta.join(" · "))
                                .font(theme::mono(11.0))
                                .color(tokens.muted),
                        )
                        .truncate(),
                    );
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 12.0;
                        if let Some(progress) = doc.progress() {
                            ui.add(Meter::new(progress).bar().width((info * 0.4).min(180.0)));
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
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(place)
                                    .font(theme::mono(11.0))
                                    .color(tokens.muted),
                            )
                            .truncate(),
                        );
                    });
                });
                // The actions on the card's right edge.
                ui.vertical(|ui| {
                    ui.set_width(ACTIONS);
                    ui.with_layout(egui::Layout::top_down(egui::Align::Max), |ui| {
                        ui.add_space(36.0);
                        ui.horizontal(|ui| {
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui
                                        .add(
                                            Button::primary("continue")
                                                .trailing_icon("arrow-right"),
                                        )
                                        .clicked()
                                    {
                                        action = Some(HomeAction::Open(doc.path.clone()));
                                    }
                                    ui.add(Key("enter"));
                                },
                            );
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

/// `pdf`, `epub`… from the file's extension.
pub fn kind_of(path: &Path) -> String {
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
pub(crate) fn day(at: u64, now: u64) -> String {
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
    fn documents_keep_one_order_when_times_tie() {
        use glyphst_library::{DocumentPosition, HistoryEntry};
        let mut library = LibraryStore::memory();
        let entry = |path: &str, opened| HistoryEntry {
            path: PathBuf::from(path),
            opened,
            position: DocumentPosition::fixed(0.0, 0.0, 1.0, 1),
        };
        library
            .replace_history_entries(vec![
                entry("/c.pdf", 5),
                entry("/b.pdf", 9),
                entry("/a.pdf", 5),
            ])
            .unwrap();
        let paths = |library: &LibraryStore| -> Vec<PathBuf> {
            documents(library, &BTreeMap::new())
                .into_iter()
                .map(|doc| doc.path)
                .collect()
        };
        let first = paths(&library);
        assert_eq!(first, ["/b.pdf", "/a.pdf", "/c.pdf"].map(PathBuf::from));
        for _ in 0..20 {
            assert_eq!(paths(&library), first);
        }
    }

    /// A document with a very long name keeps home in its column: the
    /// title wraps and ends in `…` instead of widening the sections.
    #[test]
    fn a_long_title_keeps_home_in_its_column() {
        use glyphst_library::{DocumentPosition, HistoryEntry};
        let dir = std::env::temp_dir().join(format!("glyphst-home-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let name = format!(
            "{} Just for Fun the Story of an Accidental Revolutionary.pdf",
            "Linus Torvalds David Diamond ".repeat(6)
        );
        let path = dir.join(name);
        std::fs::write(&path, b"").unwrap();
        let mut library = LibraryStore::memory();
        library
            .replace_history_entries(vec![HistoryEntry {
                path: path.clone(),
                opened: 5,
                position: DocumentPosition::fixed(0.0, 0.0, 1.0, 3),
            }])
            .unwrap();
        let ctx = egui::Context::default();
        ctx.set_fonts(crate::fonts::embedded());
        theme::apply(&ctx, &theme::Appearance::default());
        let mut covers = Covers::new(&ctx);
        let mut home = Home::default();
        for width in [1440.0, 900.0] {
            for _ in 0..3 {
                let input = egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(width, 900.0))),
                    ..Default::default()
                };
                let mut output = ctx.run_ui(input, |ui| {
                    home.ui(
                        ui,
                        HomeContext {
                            library: &library,
                            page_counts: &BTreeMap::new(),
                            picking: false,
                            open_key: None,
                            covers: &mut covers,
                        },
                    );
                });
                output.textures_delta.clear();
            }
            let main = if width - ACTIVITY_WIDTH >= layout::MIN_COLUMN + 2.0 * layout::GUTTER {
                width - ACTIVITY_WIDTH
            } else {
                width
            };
            let column = layout::column_width(main, layout::MAX_COLUMN);
            assert!(
                home.content.width() <= column + 0.5,
                "at {width}: {} wide in a {column} column",
                home.content.width()
            );
            assert!(home.content.right() <= main, "at {width}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn home_shows_a_year_or_the_last_whole_months_that_fit() {
        let today = chrono::NaiveDate::from_ymd_opt(2025, 10, 15).unwrap();
        assert_eq!(heat_months(today, 1040.0), (12, true));
        // 960 window: 864 column, 44 weeks beside the numbers.
        assert_eq!(heat_months(today, 864.0), (10, true));
        // Too narrow for four months beside: the numbers go under.
        assert!(!heat_months(today, 400.0).1);
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
