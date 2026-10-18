//! Find in the document, after pdfcraft's `Find` and `find_bar`
//! (artcraft, `ui-egui/src/canvas.rs`) and its search panel
//! (`search_ui.rs`): matches fill in page by page as text is read, the
//! find bar steps through them and the sidebar lists them with context.

use std::{
    ops::Range,
    time::{Duration, Instant},
};

use glyphst_pdf::text::PageText;

use glyphst_ui::Tip;

use crate::{
    doc_text::DocText,
    icons,
    theme::{self, Tokens},
};

/// A query is searched once typing pauses this long.
const DEBOUNCE: Duration = Duration::from_millis(150);
/// Characters of context on each side of a match in the search panel.
const CONTEXT_CHARS: usize = 40;

#[derive(Clone, Debug, PartialEq)]
pub struct Match {
    pub page: usize,
    pub range: Range<usize>,
    /// The match with some text around it, and the match's char range in it.
    pub context: String,
    pub context_match: Range<usize>,
}

#[derive(Default)]
pub struct Find {
    /// The find bar is showing.
    pub open: bool,
    pub query: String,
    pub case_sensitive: bool,
    pub whole_words: bool,
    /// The query is a regular expression.
    pub pattern: bool,
    pub matches: Vec<Match>,
    pub current: Option<usize>,
    /// The search the matches are for: query and options.
    applied: Option<Applied>,
    /// The applied pattern, compiled; `Err` when it does not parse.
    compiled: Option<Result<regex::Regex, String>>,
    edited: Option<Instant>,
    searched: Vec<bool>,
    /// Ask the find bar to take keyboard focus.
    pub focus: bool,
    /// The page the view showed when the search started; the first match
    /// at or after it becomes current.
    start_page: usize,
    /// The match last brought into view.
    revealed: Option<(usize, usize)>,
}

impl Find {
    pub fn show(&mut self, current_page: usize) {
        self.open = true;
        self.focus = true;
        self.start_page = current_page;
    }

    /// Whether a search is running or showing results.
    pub fn is_active(&self) -> bool {
        self.applied.is_some()
    }

    /// How many pages have matches.
    pub fn matched_pages(&self) -> usize {
        let mut pages: Vec<usize> = self.matches.iter().map(|found| found.page).collect();
        pages.dedup();
        pages.len()
    }

    pub fn searched_pages(&self) -> usize {
        self.searched.iter().filter(|done| **done).count()
    }

    pub fn is_complete(&self) -> bool {
        !self.searched.is_empty() && self.searched.iter().all(|done| *done)
    }

    /// Records an edit of the query or options; the search runs once the
    /// edits pause.
    pub fn edited(&mut self, now: Instant) {
        self.edited = Some(now);
    }

    /// Forgets a page's matches, so its new text is searched again.
    pub fn invalidate_page(&mut self, page: usize) {
        let Some(searched) = self.searched.get_mut(page) else {
            return;
        };
        *searched = false;
        let range = self.on_page(page);
        if range.is_empty() {
            return;
        }
        let removed = range.len();
        self.current = match self.current {
            Some(current) if range.contains(&current) => None,
            Some(current) if current >= range.end => Some(current - removed),
            other => other,
        };
        self.matches.drain(range);
    }

    /// Searches again from scratch, as after a relayout.
    pub fn restart(&mut self) {
        self.reset(None);
    }

    pub fn close(&mut self) {
        self.open = false;
        self.query.clear();
        self.reset(None);
    }

    fn reset(&mut self, applied: Option<Applied>) {
        self.compiled = applied
            .as_ref()
            .filter(|applied| applied.pattern)
            .map(|applied| applied.compile());
        self.applied = applied;
        self.revealed = None;
        self.matches.clear();
        self.current = None;
        self.searched.iter_mut().for_each(|done| *done = false);
    }

    /// Starts the search once the query has settled, then searches every
    /// page whose text is known and not searched yet. Returns whether the
    /// search wants more page text.
    pub fn refresh(&mut self, text: &DocText, now: Instant, current_page: usize) -> bool {
        if self.searched.len() != text.page_count() {
            self.searched = vec![false; text.page_count()];
        }
        let wanted = Applied {
            query: self.query.trim().to_string(),
            case_sensitive: self.case_sensitive,
            whole_words: self.whole_words,
            pattern: self.pattern,
        };
        let settled = self
            .edited
            .is_none_or(|edited| now.saturating_duration_since(edited) >= DEBOUNCE);
        if settled && self.applied.as_ref() != Some(&wanted) {
            self.edited = None;
            if wanted.query.is_empty() {
                self.reset(None);
            } else {
                self.start_page = current_page;
                self.reset(Some(wanted));
            }
        }
        if self.applied.is_none() {
            return false;
        }
        for page in 0..self.searched.len() {
            if !self.searched[page]
                && let Some(page_text) = text.get(page)
            {
                self.add_page(page, page_text);
            }
        }
        !self.is_complete()
    }

    /// Adds a page's matches in page order, keeping the current match.
    fn add_page(&mut self, page: usize, text: &PageText) {
        self.searched[page] = true;
        let Some(applied) = &self.applied else {
            return;
        };
        let ranges = match &self.compiled {
            Some(Ok(pattern)) => text.find_pattern(pattern),
            Some(Err(_)) => Vec::new(),
            None => text.find(&applied.query, applied.case_sensitive, applied.whole_words),
        };
        let found: Vec<Match> = ranges
            .into_iter()
            .map(|range| context(page, text, range))
            .collect();
        if found.is_empty() {
            return;
        }
        let at = self.matches.partition_point(|m| m.page < page);
        let count = found.len();
        self.matches.splice(at..at, found);
        match self.current {
            Some(current) if current >= at => self.current = Some(current + count),
            Some(_) => {}
            None => {
                // The first match at or after where the search started.
                let first = self.matches.partition_point(|m| m.page < self.start_page);
                if first < self.matches.len() {
                    self.current = Some(first);
                }
            }
        }
    }

    /// Moves to the next or previous match, wrapping around.
    pub fn step(&mut self, forward: bool) -> Option<&Match> {
        let count = self.matches.len();
        if count == 0 {
            return None;
        }
        let next = match self.current {
            None if forward => 0,
            None => count - 1,
            Some(current) if forward => (current + 1) % count,
            Some(current) => (current + count - 1) % count,
        };
        self.current = Some(next);
        self.matches.get(next)
    }

    pub fn current_match(&self) -> Option<&Match> {
        self.matches.get(self.current?)
    }

    /// Indices of the matches on a page.
    pub fn on_page(&self, page: usize) -> Range<usize> {
        let start = self.matches.partition_point(|m| m.page < page);
        let end = self.matches.partition_point(|m| m.page <= page);
        start..end
    }

    /// The current match when it changed since the last call, to bring it
    /// into view.
    pub fn take_reveal(&mut self) -> Option<&Match> {
        let current = self.current_match()?;
        let key = (current.page, current.range.start);
        if self.revealed == Some(key) {
            return None;
        }
        self.revealed = Some(key);
        self.current_match()
    }

    /// Why the pattern does not parse, while it does not.
    pub fn pattern_error(&self) -> Option<&str> {
        match &self.compiled {
            Some(Err(error)) => Some(error),
            _ => None,
        }
    }

    pub fn status(&self) -> String {
        if self.applied.is_none() {
            return String::new();
        }
        if self.pattern_error().is_some() {
            return "invalid pattern".into();
        }
        let pages = self.searched.len();
        let searched = self.searched_pages();
        if self.matches.is_empty() {
            if searched < pages {
                format!("searching… {searched}/{pages}")
            } else {
                "no matches".into()
            }
        } else {
            let more = if searched < pages { "+" } else { "" };
            format!(
                "{}/{}{more}",
                self.current.map_or(0, |current| current + 1),
                self.matches.len()
            )
        }
    }
}

/// A search as applied: the query and its options.
#[derive(Clone, Debug, PartialEq)]
struct Applied {
    query: String,
    case_sensitive: bool,
    whole_words: bool,
    pattern: bool,
}

impl Applied {
    /// The query as a regular expression, with the options.
    fn compile(&self) -> Result<regex::Regex, String> {
        let source = if self.whole_words {
            format!(r"\b(?:{})\b", self.query)
        } else {
            self.query.clone()
        };
        regex::RegexBuilder::new(&source)
            .case_insensitive(!self.case_sensitive)
            .size_limit(1 << 20)
            .build()
            .map_err(|error| error.to_string())
    }
}

fn context(page: usize, text: &PageText, range: Range<usize>) -> Match {
    // Whether a word gap or line break comes before glyph `at`.
    let gap = |at: usize| {
        at > 0
            && at < text.len()
            && (text.glyphs[at - 1].ch.is_whitespace()
                || text.glyphs[at].ch.is_whitespace()
                || text.line_of[at - 1] != text.line_of[at])
    };
    let flat = |s: String| s.replace('\n', " ");
    // Context starts at a word: a cut word at its start is left out.
    let mut from = range.start.saturating_sub(CONTEXT_CHARS);
    if from > 0 && !gap(from) {
        while from < range.start && !text.glyphs[from].ch.is_whitespace() {
            from += 1;
        }
    }
    let mut context = flat(text.text_of(from..range.start));
    if !context.is_empty() && gap(range.start) {
        context.push(' ');
    }
    let start = context.chars().count();
    context.push_str(&flat(text.text_of(range.clone())));
    let end = context.chars().count();
    let after = flat(text.text_of(range.end..range.end + CONTEXT_CHARS));
    if !after.is_empty() && gap(range.end) {
        context.push(' ');
    }
    context.push_str(&after);
    Match {
        page,
        range,
        context,
        context_match: start..end,
    }
}

/// What the find bar asks for.
#[derive(Debug, PartialEq)]
pub enum FindBarAction {
    Step(bool),
    Close,
}

/// The find bar (frame 02d): a popover at the top right of the pages with
/// the query, the count, the options (`Aa` case, `.*` pattern, `ab` whole
/// words), previous, next and close. Enter finds the next match and keeps
/// the field focused for another, unless `enter_leaves` (the Vim preset,
/// where n and N go on from the page).
pub fn find_bar(
    find: &mut Find,
    area: egui::Rect,
    ctx: &egui::Context,
    now: Instant,
    enter_leaves: bool,
) -> Option<FindBarAction> {
    let tokens = Tokens::get(ctx);
    let mut action = None;
    egui::Area::new(egui::Id::unique("find-bar"))
        .order(egui::Order::Middle)
        .pivot(egui::Align2::RIGHT_TOP)
        .fixed_pos(area.right_top() + egui::vec2(-24.0, 8.0))
        .show(ctx, |ui| {
            glyphst_ui::pane::popover(ctx)
                .inner_margin(egui::Margin::symmetric(8, 3))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        ui.add(icons::image("search", 14.0, tokens.faint));
                        let response = glyphst_ui::text::fitted(ui, |ui| {
                            ui.add(
                                egui::TextEdit::singleline(&mut find.query)
                                    .id(egui::Id::unique("find-input"))
                                    .font(theme::mono(12.0))
                                    .hint_text(
                                        egui::RichText::new("find in document").color(tokens.faint),
                                    )
                                    .desired_width(150.0)
                                    .frame(egui::Frame::NONE),
                            )
                        });
                        if find.focus {
                            response.request_focus();
                            find.focus = false;
                        }
                        if response.changed() {
                            find.edited(now);
                        }
                        if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            action = Some(FindBarAction::Step(!ui.input(|i| i.modifiers.shift)));
                            if !enter_leaves {
                                response.request_focus();
                            }
                        }
                        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                            action = Some(FindBarAction::Close);
                        }
                        let status = find.status();
                        let color = if find.pattern_error().is_some() {
                            tokens.err
                        } else {
                            tokens.accent_text
                        };
                        let status = ui.label(
                            egui::RichText::new(status)
                                .font(theme::mono(11.0))
                                .color(color),
                        );
                        if let Some(error) = find.pattern_error() {
                            status.tip(error);
                        }
                        let mut changed = false;
                        changed |= option(ui, "Aa", &mut find.case_sensitive, "match case");
                        changed |= option(ui, ".*", &mut find.pattern, "regular expression");
                        changed |= option(ui, "ab", &mut find.whole_words, "whole words");
                        if changed {
                            find.edited(now);
                        }
                        if icons::button(ui, "chevron-up", 22.0, false, "previous (shift enter)")
                            .clicked()
                        {
                            action = Some(FindBarAction::Step(false));
                        }
                        if icons::button(ui, "chevron-down", 22.0, false, "next (enter)").clicked()
                        {
                            action = Some(FindBarAction::Step(true));
                        }
                        if icons::button(ui, "x", 22.0, false, "close (esc)").clicked() {
                            action = Some(FindBarAction::Close);
                        }
                    });
                });
        });
    action
}

/// A search option toggled by its glyph (`Aa`, `.*`, `ab`); returns
/// whether it flipped.
fn option(ui: &mut egui::Ui, label: &str, on: &mut bool, tip: &str) -> bool {
    let tokens = Tokens::get(ui.ctx());
    let font = theme::mono(10.0);
    let size = egui::vec2(glyphst_ui::text::width(ui, label, &font) + 8.0, 17.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    let response = response.tip(tip);
    if response.clicked() {
        *on = !*on;
    }
    let (fill, color) = if *on {
        (tokens.accent_soft, tokens.accent_text)
    } else if response.hovered() {
        (tokens.hover, tokens.text)
    } else {
        (egui::Color32::TRANSPARENT, tokens.faint)
    };
    ui.painter().rect_filled(rect, tokens.ctl_radius(), fill);
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        label,
        font,
        color,
    );
    response.clicked()
}

/// Where to start the context `before` a match so that, after a leading
/// "…", it fits in `room` (`measure` gives a text's width): as much of
/// its end as fits.
fn centred_start(before: &[char], room: f32, measure: impl Fn(&str) -> f32) -> usize {
    let fits = |start: usize| {
        let text: String = std::iter::once('…')
            .chain(before[start..].iter().copied())
            .collect();
        measure(&text) <= room
    };
    // Widths shrink as the start moves right: find the first that fits.
    let (mut low, mut high) = (0, before.len());
    while low < high {
        let middle = (low + high) / 2;
        if fits(middle) {
            high = middle;
        } else {
            low = middle + 1;
        }
    }
    low
}

/// The Search tab (frame 03): the query with its options, then the
/// matches grouped by page under `── p.4 ──── 3`, each a line of context
/// with the match in accent. Returns the index of a match to go to.
pub fn search_panel(find: &mut Find, ui: &mut egui::Ui, now: Instant) -> Option<usize> {
    let tokens = Tokens::get(ui.ctx());
    ui.add_space(4.0);
    let mut case = find.case_sensitive;
    let mut pattern = find.pattern;
    let response = glyphst_ui::field::TextField::search(&mut find.query, "search the document")
        .toggle(glyphst_ui::field::FieldToggle {
            label: "Aa",
            on: &mut case,
            tooltip: "match case",
        })
        .toggle(glyphst_ui::field::FieldToggle {
            label: ".*",
            on: &mut pattern,
            tooltip: "regular expression",
        })
        .show(ui);
    if response.changed() || case != find.case_sensitive || pattern != find.pattern {
        find.case_sensitive = case;
        find.pattern = pattern;
        find.edited(now);
    }
    let status = find.status();
    if !status.is_empty() && find.matches.is_empty() {
        ui.add_space(6.0);
        ui.label(
            egui::RichText::new(find.pattern_error().unwrap_or(&status))
                .font(theme::mono(11.0))
                .color(tokens.muted),
        );
    }
    ui.add_space(6.0);
    let mut go = None;
    // Rows: a page's title row, then its matches.
    #[derive(Clone, Copy)]
    enum Line {
        Page(usize, usize),
        Match(usize),
    }
    let mut lines = Vec::new();
    let mut index = 0;
    while index < find.matches.len() {
        let page = find.matches[index].page;
        let range = find.on_page(page);
        lines.push(Line::Page(page, range.len()));
        lines.extend(range.clone().map(Line::Match));
        index = range.end.max(index + 1);
    }
    let row_height = 24.0;
    egui::ScrollArea::vertical().auto_shrink(false).show_rows(
        ui,
        row_height,
        lines.len(),
        |ui, rows| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for line in &lines[rows] {
                match *line {
                    Line::Page(page, count) => {
                        let (rect, _) = ui.allocate_exact_size(
                            egui::vec2(ui.available_width(), row_height),
                            egui::Sense::hover(),
                        );
                        glyphst_ui::motif::paint_rule_title(
                            ui,
                            rect.shrink2(egui::vec2(0.0, 2.0)),
                            &format!("p.{}", page + 1),
                            Some(&count.to_string()),
                            true,
                        );
                    }
                    Line::Match(index) => {
                        let found = &find.matches[index];
                        let (rect, response) = ui.allocate_exact_size(
                            egui::vec2(ui.available_width(), row_height),
                            egui::Sense::click(),
                        );
                        let selected = find.current == Some(index);
                        if selected {
                            ui.painter().rect_filled(rect, 0, tokens.accent_soft);
                            ui.painter().rect_filled(
                                egui::Rect::from_min_size(rect.min, egui::vec2(2.0, rect.height())),
                                0,
                                tokens.accent,
                            );
                        } else if response.hovered() {
                            ui.painter().rect_filled(rect, 0, tokens.hover);
                        }
                        let font = theme::mono(11.0);
                        let normal = egui::TextFormat {
                            font_id: font.clone(),
                            color: if selected {
                                tokens.accent_text
                            } else {
                                tokens.text_2
                            },
                            ..Default::default()
                        };
                        let strong = egui::TextFormat {
                            font_id: theme::mono_strong(11.0),
                            color: tokens.accent_text,
                            ..Default::default()
                        };
                        let chars: Vec<char> = found.context.chars().collect();
                        let part = |range: Range<usize>| chars[range].iter().collect::<String>();
                        let matched = found.context_match.start.min(chars.len())
                            ..found.context_match.end.min(chars.len());
                        let text = rect.shrink2(egui::vec2(4.0, 0.0));
                        let painter = ui.painter();
                        let middle = text.center().y;
                        let shown = painter.layout_no_wrap(
                            part(matched.clone()),
                            strong.font_id.clone(),
                            strong.color,
                        );
                        let half = (text.width() - shown.size().x) / 2.0;
                        if half > 0.0 {
                            // The match in the middle of the row, whatever
                            // its width: the context before it is cut from
                            // the left and the context after from the right.
                            let before = &chars[..matched.start];
                            let measure = |text: &str| glyphst_ui::text::width(ui, text, &font);
                            let start = centred_start(before, half, measure);
                            let before: String = std::iter::once('…')
                                .chain(before[start..].iter().copied())
                                .collect();
                            let left = text.center().x - shown.size().x / 2.0;
                            let galley = painter.layout_no_wrap(before, font.clone(), normal.color);
                            painter.galley(
                                egui::pos2(left - galley.size().x, middle - galley.size().y / 2.0),
                                galley,
                                normal.color,
                            );
                            let mut after = egui::text::LayoutJob::single_section(
                                part(matched.end..chars.len()),
                                normal.clone(),
                            );
                            after.wrap = egui::text::TextWrapping::truncate_at_width(half);
                            let galley = painter.layout_job(after);
                            painter.galley(
                                egui::pos2(left + shown.size().x, middle - galley.size().y / 2.0),
                                galley,
                                normal.color,
                            );
                            painter.galley(
                                egui::pos2(left, middle - shown.size().y / 2.0),
                                shown,
                                strong.color,
                            );
                        } else {
                            // A match wider than the row starts at its left.
                            let mut job = egui::text::LayoutJob::default();
                            job.append("…", 0.0, normal.clone());
                            job.append(&part(0..matched.start), 0.0, normal.clone());
                            job.append(&part(matched.clone()), 0.0, strong);
                            job.append(&part(matched.end..chars.len()), 0.0, normal);
                            job.wrap = egui::text::TextWrapping::truncate_at_width(text.width());
                            let galley = painter.layout_job(job);
                            painter.galley(
                                egui::pos2(text.left(), middle - galley.size().y / 2.0),
                                galley,
                                tokens.text_2,
                            );
                        }
                        if response.clicked() {
                            go = Some(index);
                        }
                    }
                }
            }
        },
    );
    if let Some(index) = go {
        find.current = Some(index);
    }
    go
}

/// The tint of a search match on the page and, for the current one, its
/// accent outline: the theme's accent, see-through over the text.
pub fn match_tint(tokens: &Tokens, current: bool) -> (egui::Color32, Option<egui::Stroke>) {
    if current {
        (
            Tokens::fade(tokens.accent, 0.45),
            Some(egui::Stroke::new(1.0, tokens.accent)),
        )
    } else {
        (Tokens::fade(tokens.accent, 0.28), None)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use glyphst_pdf::text::Glyph;

    use super::*;

    #[test]
    fn the_context_before_a_centred_match_keeps_what_fits() {
        // One unit a character, "…" included.
        let measure = |text: &str| text.chars().count() as f32;
        let before: Vec<char> = "and apply a ".chars().collect();
        // Half the row holds "…" and the last seven characters.
        assert_eq!(centred_start(&before, 8.0, measure), before.len() - 7);
        // A short context is kept whole.
        assert_eq!(centred_start(&before, 40.0, measure), 0);
        // No room keeps none of it.
        assert_eq!(centred_start(&before, 0.5, measure), before.len());
    }

    fn page(text: &str) -> Arc<PageText> {
        Arc::new(PageText {
            glyphs: text
                .chars()
                .map(|ch| Glyph { ch, rect: [0.0; 4] })
                .collect(),
            line_of: vec![0; text.chars().count()],
        })
    }

    #[test]
    fn matches_arrive_in_page_order_and_keep_the_current_one() {
        let mut text = DocText::new(4);
        let mut find = Find::default();
        let start = Instant::now();
        find.query = "cat".into();
        find.edited(start);
        // The view is on page 2 when the search starts.
        text.insert(2, page("a cat and a cat"));
        assert!(find.refresh(&text, start + DEBOUNCE, 2));
        assert_eq!(find.matches.len(), 2);
        assert_eq!(find.current, Some(0));
        assert_eq!(find.status(), "1/2+");

        text.insert(0, page("cat"));
        find.refresh(&text, start + DEBOUNCE, 2);
        assert_eq!(find.matches[0].page, 0, "kept in page order");
        assert_eq!(
            find.current_match().unwrap().page,
            2,
            "still the same match"
        );

        text.insert(1, page("dog"));
        text.insert(3, page("Cat"));
        assert!(
            !find.refresh(&text, start + DEBOUNCE, 2),
            "every page searched"
        );
        assert_eq!(find.status(), "2/4");
        assert_eq!(find.on_page(2).len(), 2);
        assert_eq!(find.take_reveal().unwrap().page, 2);
        assert!(find.take_reveal().is_none(), "revealed once");
        find.step(true);
        find.step(true);
        assert_eq!(find.current_match().unwrap().page, 3);
        assert_eq!(find.step(true).unwrap().page, 0, "wraps around");
        assert_eq!(find.step(false).unwrap().page, 3);
    }

    #[test]
    fn edits_wait_for_the_pause_and_options_search_again() {
        let mut text = DocText::new(1);
        text.insert(0, page("Cat cat category"));
        let mut find = Find::default();
        let start = Instant::now();
        find.query = "cat".into();
        find.edited(start);
        find.refresh(&text, start, 0);
        assert!(find.matches.is_empty(), "still typing");
        find.refresh(&text, start + DEBOUNCE, 0);
        assert_eq!(find.matches.len(), 3);

        find.whole_words = true;
        find.case_sensitive = true;
        find.edited(start + DEBOUNCE);
        find.refresh(&text, start + DEBOUNCE * 2, 0);
        assert_eq!(find.matches.len(), 1);
        assert_eq!(find.matches[0].range, 4..7);
        let found = &find.matches[0];
        let chars: Vec<char> = found.context.chars().collect();
        assert_eq!(
            chars[found.context_match.clone()]
                .iter()
                .collect::<String>(),
            "cat"
        );
        assert_eq!(found.context, "Cat cat category");

        find.close();
        assert!(!find.is_active());
        assert!(find.matches.is_empty());
    }

    #[test]
    fn pages_whose_text_changed_are_searched_again() {
        let mut text = DocText::new(2);
        text.insert(0, page("cat cat"));
        text.insert(1, page("cat"));
        let mut find = Find {
            query: "cat".into(),
            ..Default::default()
        };
        let now = Instant::now();
        find.refresh(&text, now, 1);
        assert_eq!(find.matches.len(), 3);
        assert_eq!(find.current_match().unwrap().page, 1);

        // Page 0 now shows its translation.
        find.invalidate_page(0);
        text.remove(0);
        assert_eq!(find.matches.len(), 1);
        assert_eq!(
            find.current_match().unwrap().page,
            1,
            "the current match is kept"
        );
        text.insert(0, page("chat"));
        find.refresh(&text, now, 1);
        assert_eq!(find.matches.len(), 1, "the new text has no match");
        assert!(find.is_complete());
    }

    #[test]
    fn patterns_search_and_report_bad_ones() {
        let mut text = DocText::new(1);
        text.insert(0, page("attention is all you need"));
        let mut find = Find {
            query: "a[a-z]+".into(),
            pattern: true,
            ..Default::default()
        };
        let start = Instant::now();
        find.refresh(&text, start + DEBOUNCE, 0);
        assert_eq!(find.matches.len(), 2, "attention, all");
        find.query = "a(".into();
        find.edited(start);
        find.refresh(&text, start + DEBOUNCE * 2, 0);
        assert!(find.matches.is_empty());
        assert!(find.pattern_error().is_some());
        assert_eq!(find.status(), "invalid pattern");
    }
}
