//! The reading activity tab (frame 04c): the reading of the last twelve
//! months, this year or last year as a heatmap, with the totals and
//! streaks above it and the time read in each document under it; at the
//! right, the chosen day's hours, sessions and documents.

use chrono::{Datelike, Local, NaiveDate};
use egui::{Rect, Sense, pos2, vec2};
use glyphst_library::LibraryStore;
use glyphst_ui::{
    button::Button,
    heatmap::{self, Heatmap},
    key::Key,
    motif,
    pane::Header,
    row::ListRow,
    segmented,
};

use crate::{
    home::{self, Doc},
    reading_days::{self, ReadingDays},
    theme::{self, Tokens},
};

/// The day pane's width.
const DAY_WIDTH: f32 = 380.0;
/// The narrowest a number of the strip gets.
const STAT_WIDTH: f32 = 128.0;
const COLUMN: f32 = 980.0;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Range {
    #[default]
    Year,
    ThisYear,
    LastYear,
}

pub enum ActivityAction {
    Open(std::path::PathBuf),
}

#[derive(Default)]
pub struct ActivityView {
    range: Range,
    /// The day the right pane shows; today when none was picked.
    selected: Option<NaiveDate>,
    cache: Option<Cache>,
}

struct Cache {
    key: (usize, Range, NaiveDate),
    days: ReadingDays,
    levels: Vec<Option<u8>>,
    months: Vec<(usize, &'static str)>,
    streaks: (usize, usize),
    documents: Vec<(std::path::PathBuf, u64)>,
}

impl ActivityView {
    pub fn at(day: Option<NaiveDate>) -> Self {
        Self {
            selected: day,
            ..Self::default()
        }
    }

    /// Shows `day`, in a range that has it.
    pub fn select(&mut self, day: Option<NaiveDate>) {
        if let Some(day) = day {
            let today = Local::now().date_naive();
            if (today - day).num_days() > 52 * 7 {
                self.range = if day.year() == today.year() {
                    Range::ThisYear
                } else {
                    Range::LastYear
                };
            }
        }
        self.selected = day;
    }

    fn cache(&mut self, library: &LibraryStore, today: NaiveDate) -> &Cache {
        let sessions = library.reading_sessions();
        let key = (sessions.len(), self.range, today);
        if self.cache.as_ref().is_none_or(|cache| cache.key != key) {
            let days = match self.range {
                Range::Year => ReadingDays::weeks_to(sessions, today, 53),
                Range::ThisYear => ReadingDays::year(sessions, today.year(), today),
                Range::LastYear => ReadingDays::year(sessions, today.year() - 1, today),
            };
            self.cache = Some(Cache {
                key,
                levels: days.levels(),
                months: days.months(),
                streaks: reading_days::streaks(sessions, today),
                documents: reading_days::by_document(sessions, &days),
                days,
            });
        }
        self.cache.as_ref().expect("filled above")
    }

    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        library: &LibraryStore,
        docs: &[Doc],
    ) -> Option<ActivityAction> {
        let tokens = Tokens::get(ui.ctx());
        let full = ui.max_rect();
        ui.painter().rect_filled(full, 0, tokens.bg);
        let today = Local::now().date_naive();
        let mut action = None;
        let show_day = full.width() > 900.0;
        let main = if show_day {
            full.with_max_x(full.right() - DAY_WIDTH)
        } else {
            full
        };
        let mut main_ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(main)
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        egui::ScrollArea::vertical()
            .id_salt("activity")
            .auto_shrink(false)
            .show(&mut main_ui, |ui| {
                let width = ui.available_width();
                let column = (width - 96.0).clamp(240.0, COLUMN);
                ui.horizontal(|ui| {
                    ui.add_space(((width - column) / 2.0).max(16.0));
                    ui.vertical(|ui| {
                        ui.set_width(column);
                        ui.spacing_mut().item_spacing.y = 20.0;
                        ui.add_space(8.0);
                        if let Some(chosen) = self.main(ui, &tokens, library, docs, today) {
                            action = Some(chosen);
                        }
                        ui.add_space(28.0);
                    });
                });
            });
        if show_day {
            let rect = full.with_min_x(main.right());
            let mut pane_ui = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(rect)
                    .layout(egui::Layout::top_down(egui::Align::Min)),
            );
            let day = self.selected.unwrap_or(today);
            if let Some(chosen) = day_pane(&mut pane_ui, &tokens, library, docs, day, today) {
                action = Some(chosen);
            }
        }
        action
    }

    fn main(
        &mut self,
        ui: &mut egui::Ui,
        tokens: &Tokens,
        library: &LibraryStore,
        docs: &[Doc],
        today: NaiveDate,
    ) -> Option<ActivityAction> {
        let mut action = None;
        let this_year = today.year().to_string();
        let last_year = (today.year() - 1).to_string();
        let range_name = match self.range {
            Range::Year => "last 12 months".to_owned(),
            Range::ThisYear => this_year.clone(),
            Range::LastYear => last_year.clone(),
        };
        let mut range = self.range;
        let selected = self.selected;
        let cache = self.cache(library, today);
        // The title and the range.
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 8.0;
                let (rect, _) = ui.allocate_exact_size(vec2(360.0, 18.0), Sense::hover());
                motif::paint_rule_title(
                    ui,
                    rect,
                    &format!("reading activity  ──  {range_name}"),
                    None,
                    true,
                );
                let days = cache.days.reading_days();
                let heading = if days == 0 {
                    "No reading yet".to_owned()
                } else {
                    format!(
                        "{} over {days} {} of reading",
                        reading_days::duration_label(cache.days.total()),
                        if days == 1 { "day" } else { "days" }
                    )
                };
                ui.label(
                    egui::RichText::new(heading)
                        .font(theme::semibold(22.0))
                        .color(tokens.text),
                );
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Max), |ui| {
                segmented::labels(
                    ui,
                    &mut range,
                    &[
                        (Range::Year, "12 months"),
                        (Range::ThisYear, &this_year),
                        (Range::LastYear, &last_year),
                    ],
                );
            });
        });
        let days = cache.days.reading_days();
        let (current, longest) = cache.streaks;
        let plural = |n: usize| if n == 1 { "day" } else { "days" };
        let busiest = busiest_weekday(&cache.days);
        let stats = [
            (
                reading_days::duration_label(cache.days.total()),
                "read".to_owned(),
            ),
            (days.to_string(), "reading days".to_owned()),
            (
                format!("{current} {}", plural(current)),
                "current streak".to_owned(),
            ),
            (
                format!("{longest} {}", plural(longest)),
                "longest streak".to_owned(),
            ),
            (
                reading_days::duration_label(cache.days.total() / days.max(1) as u64),
                "a reading day".to_owned(),
            ),
            (busiest.to_owned(), "most read day".to_owned()),
        ];
        // The numbers, between hairlines, in as many rows as the width
        // needs.
        let width = ui.available_width();
        let per_row = ((width / STAT_WIDTH).floor() as usize).clamp(1, stats.len());
        let rows = stats.len().div_ceil(per_row);
        let (strip, _) =
            ui.allocate_exact_size(vec2(width, 16.0 + rows as f32 * 48.0), Sense::hover());
        ui.painter()
            .hline(strip.x_range(), strip.top() + 0.5, tokens.hairline());
        ui.painter()
            .hline(strip.x_range(), strip.bottom() - 0.5, tokens.hairline());
        let step = strip.width() / per_row as f32;
        for (index, (value, key)) in stats.iter().enumerate() {
            let x = strip.left() + (index % per_row) as f32 * step;
            let top = strip.top() + (index / per_row) as f32 * 48.0;
            ui.painter().text(
                pos2(x, top + 16.0),
                egui::Align2::LEFT_TOP,
                value,
                theme::mono_strong(17.0),
                tokens.text,
            );
            ui.painter().text(
                pos2(x, top + 40.0),
                egui::Align2::LEFT_TOP,
                key,
                theme::mono(10.0),
                tokens.faint,
            );
        }
        // The heatmap and its legend.
        let mut clicked = None;
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 8.0;
            let selected = selected.and_then(|day| cache.days.index(day));
            // 14pt cells, smaller when the weeks need it.
            let weeks = cache.levels.len().div_ceil(7) as f32;
            let cell = ((ui.available_width() - 27.0) / weeks - 3.0).clamp(6.0, 14.0);
            let map = Heatmap::new(&cache.levels)
                .months(&cache.months)
                .cell(cell.floor())
                .today(cache.days.index(today).unwrap_or(usize::MAX))
                .selected(selected)
                .show(ui);
            clicked = map.clicked.map(|day| cache.days.date(day));
            ui.horizontal(|ui| {
                ui.add_space(30.0);
                let hint = match map.hovered {
                    Some(day) => {
                        let date = cache.days.date(day);
                        format!(
                            "{} · {}",
                            reading_days::day_label(date),
                            match cache.days.seconds[day] {
                                0 => "no reading".to_owned(),
                                seconds => reading_days::duration_label(seconds),
                            }
                        )
                    }
                    None => "click a day for its sessions".to_owned(),
                };
                ui.label(
                    egui::RichText::new(hint)
                        .font(theme::mono(10.0))
                        .color(tokens.faint),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    heatmap::legend(ui);
                });
            });
        });
        // Time by document.
        motif::rule_title(ui, &format!("by document  ──  {range_name}"), None);
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            let most = cache
                .documents
                .first()
                .map_or(1, |(_, seconds)| *seconds)
                .max(1);
            if cache.documents.is_empty() {
                ui.label(
                    egui::RichText::new("reading time shows here as you read")
                        .font(theme::mono(11.0))
                        .color(tokens.faint),
                );
            }
            for (path, seconds) in cache.documents.iter().take(12) {
                let doc = docs.iter().find(|doc| &doc.path == path);
                let title = doc.map_or_else(|| file_name(path), |doc| doc.title.clone());
                let kind = home::kind_of(path);
                let (row, response) =
                    ui.allocate_exact_size(vec2(ui.available_width(), 28.0), Sense::click());
                if response.hovered() {
                    ui.painter().rect_filled(row, 0, tokens.hover);
                }
                let y = row.center().y;
                let title_width = (row.width() * 0.4).min(380.0);
                glyphst_ui::text::row(
                    ui,
                    pos2(row.left() + 4.0, y),
                    title,
                    theme::mono(12.0),
                    tokens.text_2,
                    title_width - 8.0,
                );
                ui.painter().text(
                    pos2(row.left() + title_width + 12.0, y),
                    egui::Align2::LEFT_CENTER,
                    kind,
                    theme::mono(11.0),
                    tokens.faint,
                );
                let bar = Rect::from_min_max(
                    pos2(row.left() + title_width + 60.0, y - 3.0),
                    pos2(row.right() - 64.0, y + 3.0),
                );
                ui.painter().rect_filled(bar, 1.0, tokens.heat(0));
                let filled = bar.width() * (*seconds as f32 / most as f32);
                ui.painter().rect_filled(
                    Rect::from_min_size(bar.min, vec2(filled, bar.height())),
                    1.0,
                    tokens.accent,
                );
                ui.painter().text(
                    pos2(row.right() - 4.0, y),
                    egui::Align2::RIGHT_CENTER,
                    reading_days::duration_label(*seconds),
                    theme::mono(11.0),
                    tokens.muted,
                );
                if response
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .clicked()
                {
                    action = Some(ActivityAction::Open(path.clone()));
                }
            }
        });
        if range != self.range {
            self.range = range;
        }
        if let Some(day) = clicked {
            self.selected = Some(day);
        }
        action
    }
}

/// The weekday with the most reading, `sun`.
fn busiest_weekday(days: &ReadingDays) -> &'static str {
    let mut totals = [0u64; 7];
    for (index, seconds) in days.seconds.iter().enumerate() {
        totals[index % 7] += seconds;
    }
    let names = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];
    match totals.iter().enumerate().max_by_key(|(_, total)| **total) {
        Some((index, total)) if *total > 0 => names[index],
        _ => "—",
    }
}

fn file_name(path: &std::path::Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// The chosen day: its reading by the hour, its sessions with the pages
/// they went over, its documents, and `[ open … ]`.
fn day_pane(
    ui: &mut egui::Ui,
    tokens: &Tokens,
    library: &LibraryStore,
    docs: &[Doc],
    day: NaiveDate,
    today: NaiveDate,
) -> Option<ActivityAction> {
    let mut action = None;
    let rect = ui.max_rect();
    ui.painter()
        .rect_filled(rect, 0, tokens.surface.to_opaque());
    ui.painter()
        .vline(rect.left() + 0.5, rect.y_range(), tokens.hairline());
    let sessions = reading_days::sessions_on(library.reading_sessions(), day);
    let seconds: u64 = sessions.iter().map(|session| session.seconds).sum();
    let label = reading_days::day_label(day);
    let total = reading_days::duration_label(seconds);
    Header::new(&label).trailing(total.as_str()).show(ui);
    let mut documents: Vec<(&std::path::Path, u64)> = Vec::new();
    for session in &sessions {
        match documents.iter_mut().find(|(path, _)| *path == session.path) {
            Some((_, total)) => *total += session.seconds,
            None => documents.push((&session.path, session.seconds)),
        }
    }
    let pages: usize = sessions
        .iter()
        .filter_map(|session| session.pages.map(|(first, last)| last.abs_diff(first) + 1))
        .sum();
    egui::Frame::new()
        .inner_margin(egui::Margin::symmetric(14, 12))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 6.0;
            let when = if day == today {
                "today".to_owned()
            } else {
                label.clone()
            };
            let plural = |n: usize, one: &str, many: &str| {
                format!("{n} {}", if n == 1 { one } else { many })
            };
            let mut line = format!(
                "{when} · {} · {}",
                plural(sessions.len(), "session", "sessions"),
                plural(documents.len(), "document", "documents"),
            );
            if pages > 0 {
                line.push_str(&format!(" · {pages} pages"));
            }
            ui.label(
                egui::RichText::new(line)
                    .font(theme::mono(11.0))
                    .color(tokens.muted),
            );
            hours_strip(ui, tokens, &reading_days::hours_on(&sessions));
        });
    // The lists scroll between the numbers and the foot.
    let foot = 52.0;
    let list = Rect::from_min_max(
        pos2(rect.left(), ui.cursor().top()),
        pos2(rect.right(), rect.bottom() - foot),
    );
    let mut list_ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(list)
            .layout(egui::Layout::top_down(egui::Align::Min)),
    );
    egui::ScrollArea::vertical()
        .id_salt("activity-day")
        .auto_shrink(false)
        .show(&mut list_ui, |ui| {
            if let Some(chosen) = day_lists(ui, tokens, docs, &sessions, &documents) {
                action = Some(chosen);
            }
        });
    if let Some((path, _)) = documents.iter().max_by_key(|(_, seconds)| *seconds) {
        let bottom = Rect::from_min_max(pos2(rect.left(), rect.bottom() - foot), rect.max);
        let mut foot = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(bottom.shrink(14.0))
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        foot.spacing_mut().item_spacing.x = 8.0;
        let name = short(&file_name(path), 28);
        if foot.add(Button::bracket(&format!("open {name}"))).clicked() {
            action = Some(ActivityAction::Open(path.to_path_buf()));
        }
        foot.add(Key("enter"));
        if ui.input(|i| i.key_pressed(egui::Key::Enter))
            && ui.ctx().memory(|memory| memory.focused().is_none())
        {
            action = Some(ActivityAction::Open(path.to_path_buf()));
        }
    }
    action
}

/// The day by the hour: a bar an hour, over `00 06 12 18 24`.
fn hours_strip(ui: &mut egui::Ui, tokens: &Tokens, hours: &[u64; 24]) {
    let most = hours.iter().copied().max().unwrap_or(0).max(1);
    let (strip, _) = ui.allocate_exact_size(vec2(ui.available_width(), 22.0), Sense::hover());
    let step = strip.width() / 24.0;
    for (hour, &seconds) in hours.iter().enumerate() {
        let share = seconds as f32 / most as f32;
        let (height, level) = if seconds == 0 {
            (2.0, 0)
        } else {
            (4.0 + 18.0 * share, 1 + (3.0 * share).round() as u8)
        };
        let bar = Rect::from_min_max(
            pos2(strip.left() + hour as f32 * step, strip.bottom() - height),
            pos2(
                strip.left() + (hour + 1) as f32 * step - 1.0,
                strip.bottom(),
            ),
        );
        ui.painter().rect_filled(bar, 0.0, tokens.heat(level));
    }
    let (axis, _) = ui.allocate_exact_size(vec2(ui.available_width(), 12.0), Sense::hover());
    for (hour, label) in [(0, "00"), (6, "06"), (12, "12"), (18, "18"), (24, "24")] {
        let align = match hour {
            0 => egui::Align2::LEFT_TOP,
            24 => egui::Align2::RIGHT_TOP,
            _ => egui::Align2::CENTER_TOP,
        };
        ui.painter().text(
            pos2(axis.left() + hour as f32 * step, axis.top()),
            align,
            label,
            theme::mono(9.0),
            tokens.faint,
        );
    }
}

/// `name` cut to `limit` characters, with `…`.
fn short(name: &str, limit: usize) -> String {
    if name.chars().count() <= limit {
        return name.to_owned();
    }
    let mut cut: String = name.chars().take(limit - 1).collect();
    cut.push('…');
    cut
}

/// The day's sessions with the pages they went over, then its documents.
fn day_lists(
    ui: &mut egui::Ui,
    tokens: &Tokens,
    docs: &[Doc],
    sessions: &[&glyphst_library::ReadingSession],
    documents: &[(&std::path::Path, u64)],
) -> Option<ActivityAction> {
    let mut action = None;
    egui::Frame::new()
        .inner_margin(egui::Margin::symmetric(14, 0))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 10.0;
            motif::rule_title(ui, "sessions", None);
            if sessions.is_empty() {
                ui.label(
                    egui::RichText::new("no reading this day")
                        .font(theme::mono(11.0))
                        .color(tokens.faint),
                );
            }
            for session in sessions {
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 3.0;
                    // The time, the document cut to fit, and the minutes.
                    let (row, _) =
                        ui.allocate_exact_size(vec2(ui.available_width(), 16.0), Sense::hover());
                    let time = ui.painter().text(
                        row.left_center(),
                        egui::Align2::LEFT_CENTER,
                        reading_days::span_label(session),
                        theme::mono(11.0),
                        tokens.faint,
                    );
                    let minutes = ui.painter().text(
                        row.right_center(),
                        egui::Align2::RIGHT_CENTER,
                        reading_days::duration_label(session.seconds),
                        theme::mono(11.0),
                        tokens.accent_text,
                    );
                    glyphst_ui::text::row(
                        ui,
                        pos2(time.right() + 10.0, row.center().y),
                        file_name(&session.path),
                        theme::mono(12.0),
                        tokens.text_2,
                        (minutes.left() - time.right() - 20.0).max(0.0),
                    );
                    if let Some((first, last)) = session.pages {
                        ui.label(
                            egui::RichText::new(format!("p.{first} → p.{last}"))
                                .font(theme::mono(11.0))
                                .color(tokens.muted),
                        );
                    }
                });
            }
            ui.add_space(4.0);
            motif::rule_title(ui, "that day", None);
        });
    ui.spacing_mut().item_spacing.y = 2.0;
    for (path, seconds) in documents {
        let name = docs
            .iter()
            .find(|doc| doc.path == *path)
            .map_or_else(|| file_name(path), |doc| file_name(&doc.path));
        let minutes = reading_days::duration_label(*seconds);
        if ListRow::new(&name)
            .icon("file-text")
            .meta(minutes.as_str())
            .show(ui)
            .clicked()
        {
            action = Some(ActivityAction::Open(path.to_path_buf()));
        }
    }
    ui.add_space(12.0);
    action
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picking_a_day_over_a_year_ago_shows_its_year() {
        let today = Local::now().date_naive();
        let mut view = ActivityView::default();
        view.select(Some(today - chrono::Duration::days(3)));
        assert_eq!(view.range, Range::Year);
        let long_ago = NaiveDate::from_ymd_opt(today.year() - 1, 1, 2).unwrap();
        view.select(Some(long_ago));
        assert_eq!(view.range, Range::LastYear);
        assert_eq!(view.selected, Some(long_ago));
    }
}
