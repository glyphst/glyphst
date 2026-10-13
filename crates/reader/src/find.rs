//! Find in the document, after pdfcraft's `Find` and `find_bar`
//! (artcraft, `ui-egui/src/canvas.rs`) and its search panel
//! (`search_ui.rs`): matches fill in page by page as text is read, the
//! find bar steps through them and the sidebar lists them with context.

use std::{
    ops::Range,
    time::{Duration, Instant},
};

use glyphst_pdf::text::PageText;

use crate::{doc_text::DocText, icons, theme::Tokens, widgets};

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
    pub matches: Vec<Match>,
    pub current: Option<usize>,
    /// The search the matches are for: query and options.
    applied: Option<(String, bool, bool)>,
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

    fn reset(&mut self, applied: Option<(String, bool, bool)>) {
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
        let wanted = (
            self.query.trim().to_string(),
            self.case_sensitive,
            self.whole_words,
        );
        let settled = self
            .edited
            .is_none_or(|edited| now.saturating_duration_since(edited) >= DEBOUNCE);
        if settled && self.applied.as_ref() != Some(&wanted) {
            self.edited = None;
            if wanted.0.is_empty() {
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
        let Some((query, case_sensitive, whole_words)) = &self.applied else {
            return;
        };
        let found: Vec<Match> = text
            .find(query, *case_sensitive, *whole_words)
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

    pub fn status(&self) -> String {
        if self.applied.is_none() {
            return String::new();
        }
        let pages = self.searched.len();
        let searched = self.searched_pages();
        if self.matches.is_empty() {
            if searched < pages {
                format!("Searching… {searched}/{pages}")
            } else {
                "No matches".into()
            }
        } else {
            let more = if searched < pages { "+" } else { "" };
            format!(
                "{} of {}{more}",
                self.current.map_or(0, |current| current + 1),
                self.matches.len()
            )
        }
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

/// The floating find bar at the top right of the document view.
pub fn find_bar(
    find: &mut Find,
    area: egui::Rect,
    ctx: &egui::Context,
    now: Instant,
) -> Option<FindBarAction> {
    let tokens = Tokens::get(ctx);
    let mut action = None;
    egui::Area::new(egui::Id::new("find-bar"))
        .order(egui::Order::Middle)
        .pivot(egui::Align2::RIGHT_TOP)
        .fixed_pos(area.right_top() + egui::vec2(-18.0, 12.0))
        .show(ctx, |ui| {
            widgets::card_frame(ctx)
                .shadow(tokens.popup_shadow())
                .inner_margin(egui::Margin::symmetric(10, 6))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        ui.add(icons::image("search", 16.0, tokens.text_muted));
                        let response = ui.add(
                            egui::TextEdit::singleline(&mut find.query)
                                .id(egui::Id::new("find-input"))
                                .hint_text("Find text")
                                .desired_width(220.0)
                                .frame(egui::Frame::NONE),
                        );
                        if find.focus {
                            response.request_focus();
                            find.focus = false;
                        }
                        if response.changed() {
                            find.edited(now);
                        }
                        if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            action = Some(FindBarAction::Step(!ui.input(|i| i.modifiers.shift)));
                            response.request_focus();
                        }
                        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                            action = Some(FindBarAction::Close);
                        }
                        ui.label(
                            egui::RichText::new(find.status())
                                .size(12.0)
                                .color(tokens.text_muted),
                        );
                        if icons::button(ui, "chevron-up", 26.0, false, "Previous (Shift+Enter)")
                            .clicked()
                        {
                            action = Some(FindBarAction::Step(false));
                        }
                        if icons::button(ui, "chevron-down", 26.0, false, "Next (Enter)").clicked()
                        {
                            action = Some(FindBarAction::Step(true));
                        }
                        let options = icons::button(
                            ui,
                            "sliders-horizontal",
                            26.0,
                            find.case_sensitive || find.whole_words,
                            "Find options",
                        );
                        egui::Popup::menu(&options).show(|ui| {
                            let words = ui.checkbox(&mut find.whole_words, "Whole words only");
                            let case = ui.checkbox(&mut find.case_sensitive, "Case-sensitive");
                            if words.changed() || case.changed() {
                                find.edited(now);
                            }
                        });
                        if icons::button(ui, "x", 26.0, false, "Close (Esc)").clicked() {
                            action = Some(FindBarAction::Close);
                        }
                    });
                });
        });
    action
}

/// The search panel: the query, options and every match with its context.
/// Returns the index of a match to go to.
pub fn search_panel(find: &mut Find, ui: &mut egui::Ui, now: Instant) -> Option<usize> {
    let tokens = Tokens::get(ui.ctx());
    let response = widgets::filter_field(ui, &mut find.query, "Search the document");
    if response.changed() {
        find.edited(now);
    }
    ui.horizontal(|ui| {
        let words = ui.checkbox(&mut find.whole_words, "Whole words");
        let case = ui.checkbox(&mut find.case_sensitive, "Match case");
        if words.changed() || case.changed() {
            find.edited(now);
        }
    });
    ui.label(egui::RichText::new(find.status()).color(tokens.text_muted));
    ui.separator();
    let mut go = None;
    let line = ui.text_style_height(&egui::TextStyle::Body);
    let row_height = line * 2.0 + 12.0;
    egui::ScrollArea::vertical().auto_shrink(false).show_rows(
        ui,
        row_height,
        find.matches.len(),
        |ui, rows| {
            for index in rows {
                let found = &find.matches[index];
                let (rect, response) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), row_height),
                    egui::Sense::click(),
                );
                let selected = find.current == Some(index);
                if selected || response.hovered() {
                    let fill = if selected {
                        tokens.accent_soft
                    } else {
                        tokens.hover
                    };
                    ui.painter().rect_filled(rect, 4.0, fill);
                }
                let normal = egui::TextFormat {
                    color: tokens.text_muted,
                    ..Default::default()
                };
                let strong = egui::TextFormat {
                    color: tokens.text,
                    background: MATCH_TINT,
                    ..Default::default()
                };
                let mut job = egui::text::LayoutJob::default();
                job.append(&format!("p. {}  ", found.page + 1), 0.0, normal.clone());
                let chars: Vec<char> = found.context.chars().collect();
                let part = |range: Range<usize>| chars[range].iter().collect::<String>();
                let matched = found.context_match.start.min(chars.len())
                    ..found.context_match.end.min(chars.len());
                job.append(&part(0..matched.start), 0.0, normal.clone());
                job.append(&part(matched.clone()), 0.0, strong);
                job.append(&part(matched.end..chars.len()), 0.0, normal);
                job.wrap.max_rows = 2;
                job.wrap.max_width = rect.width() - 12.0;
                let galley = ui.painter().layout_job(job);
                ui.painter()
                    .galley(rect.min + egui::vec2(6.0, 6.0), galley, tokens.text_muted);
                if response.clicked() {
                    go = Some(index);
                }
            }
        },
    );
    if let Some(index) = go {
        find.current = Some(index);
    }
    go
}

/// The tint of search matches, and of the current one.
pub const MATCH_TINT: egui::Color32 = egui::Color32::from_rgba_premultiplied(90, 69, 0, 90);
pub const CURRENT_MATCH_TINT: egui::Color32 =
    egui::Color32::from_rgba_premultiplied(160, 80, 0, 160);

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use glyphst_pdf::text::Glyph;

    use super::*;

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
        assert_eq!(find.status(), "1 of 2+");

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
        assert_eq!(find.status(), "2 of 4");
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
}
