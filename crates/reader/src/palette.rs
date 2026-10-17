//! The command palette (Ctrl+K), after pdfcraft's `palette.rs`: commands
//! with their keys, open tabs, recent documents, settings sections and
//! "Go to page N", ranked by a substring-then-subsequence match. Open
//! document turns it into a path prompt, after Zed's: the field holds a
//! path and the folders and documents matching it are listed below.

use std::path::{Path, PathBuf};

use egui::{
    Align2, Key, Rect, Sense,
    text::{CCursor, CCursorRange},
    vec2,
};

use crate::{
    commands::Command,
    icons,
    pickers::is_document,
    settings_window::Section,
    theme::{self, Tokens},
};

/// Results shown at most.
const MAX_HITS: usize = 12;
/// Every presenting command fits (frame 02i).
const MAX_PRESENT_HITS: usize = 16;
/// The path prompt's results at most: a whole folder, scrolled.
const MAX_PATH_HITS: usize = 500;
/// The results' list grows to this height before it scrolls.
const MAX_LIST_HEIGHT: f32 = 440.0;
/// Commands remembered as recently run.
const MAX_USED: usize = 8;
/// Sections among the other hits of a query.
const MAX_MIXED_SECTIONS: usize = 6;
/// Sections an `@` query lists.
const MAX_SECTION_HITS: usize = 500;

#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    Command(Command),
    /// Index into the app's tabs.
    Tab(usize),
    Open(PathBuf),
    /// A folder in the path prompt: the query that lists it.
    Browse(String),
    /// A page index (from 0).
    GoToPage(usize),
    Settings(Section),
    /// An entry of the active document's outline, by index.
    Section(usize),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Hit {
    pub item: Item,
    /// Its Lucide icon.
    pub icon: &'static str,
    pub label: String,
    /// The key, or what kind of item it is.
    pub detail: String,
    /// Why it can't run now.
    pub blocked: Option<&'static str>,
}

/// What the palette can offer, gathered by the app.
#[derive(Default)]
pub struct Sources {
    /// Each command with its key label and why it can't run now.
    pub commands: Vec<(Command, Option<String>, Option<&'static str>)>,
    pub tabs: Vec<String>,
    pub recent: Vec<PathBuf>,
    /// Pages of the active document.
    pub pages: Option<usize>,
    /// Labels that follow the state (`End draw`), by command.
    pub labels: std::collections::HashMap<Command, &'static str>,
    /// While presenting the palette holds commands, page numbers and
    /// sections only: no tabs, files or settings sections.
    pub presenting: bool,
    /// The active document's outline entries that have a page.
    pub sections: Vec<Heading>,
}

/// An outline entry the palette can go to.
pub struct Heading {
    /// Its index in the outline.
    pub index: usize,
    pub title: String,
    /// Its page (from 0).
    pub page: usize,
    pub depth: usize,
}

/// The hit that goes to `heading`; `indent` shows its depth.
fn section_hit(heading: &Heading, indent: bool) -> Hit {
    let indent = if indent { heading.depth } else { 0 };
    Hit {
        item: Item::Section(heading.index),
        icon: "list-tree",
        label: format!("{}{}", "  ".repeat(indent), heading.title),
        detail: format!("p.{}", heading.page + 1),
        blocked: None,
    }
}

/// The sections whose titles match `needle`, best first, then in reading
/// order.
fn matching_sections<'a>(
    needle: &str,
    sections: &'a [Heading],
) -> impl Iterator<Item = (usize, &'a Heading)> {
    let mut found: Vec<(usize, usize, &Heading)> = sections
        .iter()
        .enumerate()
        .filter_map(|(order, heading)| {
            score(&heading.title, needle).map(|found| (found, order, heading))
        })
        .collect();
    found.sort_by_key(|(found, order, _)| (*found, *order));
    found
        .into_iter()
        .map(|(found, _, heading)| (found, heading))
}

/// What the query looks for once a leading `@` (sections only) is taken
/// off, lower case.
pub fn needle(query: &str) -> String {
    let query = query.trim();
    query
        .strip_prefix('@')
        .unwrap_or(query)
        .trim()
        .to_lowercase()
}

/// Where `needle` (lower case) matches `hay`: lower is better.
fn score(hay: &str, needle: &str) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    let hay = hay.to_lowercase();
    if let Some(position) = hay.find(needle) {
        return Some(position);
    }
    let mut chars = hay.chars();
    needle
        .chars()
        .filter(|c| !c.is_whitespace())
        .all(|c| chars.any(|h| h == c))
        .then_some(100)
}

/// The hits for `query`, best first; `used` holds recently run commands,
/// latest first.
pub fn hits(query: &str, sources: &Sources, used: &[Command]) -> Vec<Hit> {
    // `@` lists the outline: all of it in reading order, or the sections
    // that match.
    if query.trim_start().starts_with('@') {
        let needle = needle(query);
        let indent = needle.is_empty();
        return matching_sections(&needle, &sources.sections)
            .take(MAX_SECTION_HITS)
            .map(|(_, heading)| section_hit(heading, indent))
            .collect();
    }
    let needle = query.trim().to_lowercase();
    // (score, kind order, hit)
    let mut ranked: Vec<(usize, usize, Hit)> = Vec::new();
    if let (Some(pages), Ok(page)) = (sources.pages, needle.parse::<usize>())
        && (1..=pages).contains(&page)
    {
        ranked.push((
            0,
            0,
            Hit {
                item: Item::GoToPage(page - 1),
                icon: "file-text",
                label: format!("Go to page {page}"),
                detail: format!("of {pages}"),
                blocked: None,
            },
        ));
    }
    for (command, key, blocked) in &sources.commands {
        let label = sources
            .labels
            .get(command)
            .copied()
            .unwrap_or(command.label());
        let Some(found) = score(label, &needle)
            .or_else(|| score(command.category().label(), &needle).map(|score| score + 50))
        else {
            continue;
        };
        let recency = used
            .iter()
            .position(|used| used == command)
            .unwrap_or(MAX_USED);
        // While presenting, its own commands come before leaving and the
        // others (frame 02i).
        let found = found
            + usize::from(
                sources.presenting && command.category() != crate::commands::Category::Present,
            );
        ranked.push((
            found,
            1 + recency,
            Hit {
                item: Item::Command(*command),
                icon: command.icon(),
                label: label.to_string(),
                detail: key.clone().unwrap_or_default(),
                blocked: *blocked,
            },
        ));
    }
    if !needle.is_empty() {
        for (found, heading) in
            matching_sections(&needle, &sources.sections).take(MAX_MIXED_SECTIONS)
        {
            ranked.push((found, MAX_USED + 2, section_hit(heading, false)));
        }
    }
    if sources.presenting {
        return finish(ranked, MAX_PRESENT_HITS);
    }
    for (index, title) in sources.tabs.iter().enumerate() {
        if let Some(found) = score(title, &needle) {
            ranked.push((
                found,
                MAX_USED + 3,
                Hit {
                    item: Item::Tab(index),
                    icon: "book-open",
                    label: format!("Switch to {title}"),
                    detail: "Tab".into(),
                    blocked: None,
                },
            ));
        }
    }
    for section in Section::ALL {
        let label = format!("Settings: {}", section.label());
        if let Some(found) = score(&label, &needle) {
            ranked.push((
                found,
                MAX_USED + 4,
                Hit {
                    item: Item::Settings(section),
                    icon: section.icon(),
                    label,
                    detail: "Settings".into(),
                    blocked: None,
                },
            ));
        }
    }
    for path in &sources.recent {
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        if let Some(found) = score(&name, &needle) {
            ranked.push((
                found,
                MAX_USED + 5,
                Hit {
                    item: Item::Open(path.clone()),
                    icon: "clock",
                    label: format!("Open {name}"),
                    detail: "Recent".into(),
                    blocked: None,
                },
            ));
        }
    }
    finish(ranked, MAX_HITS)
}

/// The best `limit` of `ranked`, grouped by kind.
fn finish(mut ranked: Vec<(usize, usize, Hit)>, limit: usize) -> Vec<Hit> {
    ranked.sort_by_key(|(score, kind, hit)| (*score, hit.blocked.is_some(), *kind));
    ranked.truncate(limit);
    let hits: Vec<Hit> = ranked.into_iter().map(|(_, _, hit)| hit).collect();
    grouped(hits)
}

/// `hits` with each kind together under one title, the kinds in the order
/// of their best hit and the hits of a kind in their own order.
fn grouped(hits: Vec<Hit>) -> Vec<Hit> {
    let mut order: Vec<&'static str> = Vec::new();
    for hit in &hits {
        let group = group_of(&hit.item);
        if !order.contains(&group) {
            order.push(group);
        }
    }
    let mut hits = hits;
    hits.sort_by_key(|hit| order.iter().position(|group| *group == group_of(&hit.item)));
    hits
}

/// The folder `query` names, with `~` and a bare name taken from `home`,
/// the text naming it (up to its last `/`) and the name typed after it.
fn split_path<'a>(query: &'a str, home: &Path) -> (PathBuf, &'a str, &'a str) {
    if query == "~" {
        return (home.to_path_buf(), "~/", "");
    }
    let (folder, name) = query
        .rfind('/')
        .map_or(("", query), |slash| (&query[..=slash], &query[slash + 1..]));
    let path = if let Some(rest) = folder.strip_prefix("~/") {
        home.join(rest)
    } else {
        home.join(folder)
    };
    (path, folder, name)
}

/// What a folder holds: each entry's name and whether it is a folder.
pub type Listing = Vec<(String, bool)>;

/// The path prompt's hits for `query` over its folder's `entries`: the
/// folders and documents whose names match, best first, folders before
/// documents that match as well.
pub fn path_hits(query: &str, home: &Path, entries: &[(String, bool)]) -> Vec<Hit> {
    let (path, folder, name) = split_path(query, home);
    let needle = name.to_lowercase();
    let mut ranked: Vec<(usize, bool, String, Hit)> = entries
        .iter()
        .filter(|(entry, is_folder)| {
            (needle.starts_with('.') || !entry.starts_with('.'))
                && (*is_folder || is_document(Path::new(entry)))
        })
        .filter_map(|(entry, is_folder)| {
            let found = score(entry, &needle)?;
            let hit = if *is_folder {
                Hit {
                    item: Item::Browse(format!("{folder}{entry}/")),
                    icon: "folder-open",
                    label: format!("{entry}/"),
                    detail: "Folder".into(),
                    blocked: None,
                }
            } else {
                Hit {
                    item: Item::Open(path.join(entry)),
                    icon: "file-text",
                    label: entry.clone(),
                    detail: Path::new(entry)
                        .extension()
                        .map(|extension| extension.to_string_lossy().to_uppercase())
                        .unwrap_or_default(),
                    blocked: None,
                }
            };
            Some((found, !is_folder, entry.to_lowercase(), hit))
        })
        .collect();
    ranked.sort_by(|a, b| (a.0, a.1, &a.2).cmp(&(b.0, b.1, &b.2)));
    ranked.truncate(MAX_PATH_HITS);
    ranked.into_iter().map(|(_, _, _, hit)| hit).collect()
}

/// The query that completes `query` to `hit`: its folder, or its document
/// in the same folder.
fn completion(query: &str, home: &Path, hit: &Hit) -> Option<String> {
    match &hit.item {
        Item::Browse(next) => Some(next.clone()),
        Item::Open(path) => {
            let (_, folder, _) = split_path(query, home);
            Some(format!("{folder}{}", path.file_name()?.to_string_lossy()))
        }
        _ => None,
    }
}

/// What the palette lists.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Commands,
    /// Open document's path prompt.
    OpenPath,
}

#[derive(Default)]
pub struct Palette {
    pub open: bool,
    pub query: String,
    pub mode: Mode,
    highlight: usize,
    used: Vec<Command>,
    /// Where it was drawn last frame, to close on a click outside.
    rect: Option<Rect>,
    /// The path prompt's folder as last read, or `None` if it can't be.
    listing: Option<(PathBuf, Option<Listing>)>,
    /// The section being read: an `@` query starts on it.
    pub current: Option<Item>,
    /// The query the highlight was last placed for.
    placed: Option<String>,
}

impl Palette {
    pub fn toggle(&mut self) {
        if self.open {
            self.close();
        } else {
            self.open = true;
            self.highlight = 0;
        }
    }

    /// Opens the path prompt at `start`, a folder ending in `/`.
    pub fn open_path(&mut self, ctx: &egui::Context, start: String) {
        self.open = true;
        self.mode = Mode::OpenPath;
        self.highlight = 0;
        self.set_query(ctx, start);
    }

    pub fn close(&mut self) {
        self.open = false;
        self.mode = Mode::Commands;
        self.query.clear();
        self.rect = None;
        self.listing = None;
        self.placed = None;
    }

    /// Replaces the query, with the caret after it.
    fn set_query(&mut self, ctx: &egui::Context, query: String) {
        let end = query.chars().count();
        self.query = query;
        self.highlight = 0;
        if let Some(mut state) = egui::TextEdit::load_state(ctx, field_id()) {
            state
                .cursor
                .set_char_range(Some(CCursorRange::one(CCursor::new(end))));
            state.store(ctx, field_id());
        }
    }

    /// The path prompt's hits, reading the query's folder when it changed.
    pub fn path_hits(&mut self, home: &Path) -> Vec<Hit> {
        let (folder, _, _) = split_path(&self.query, home);
        if self
            .listing
            .as_ref()
            .is_none_or(|(read, _)| *read != folder)
        {
            let entries = std::fs::read_dir(&folder).ok().map(|entries| {
                entries
                    .flatten()
                    .map(|entry| {
                        let name = entry.file_name().to_string_lossy().into_owned();
                        (name, entry.path().is_dir())
                    })
                    .collect()
            });
            self.listing = Some((folder, entries));
        }
        match &self.listing {
            Some((_, Some(entries))) => path_hits(&self.query, home, entries),
            _ => vec![Hit {
                item: Item::Browse(self.query.clone()),
                icon: "folder-open",
                label: split_path(&self.query, home).0.display().to_string(),
                detail: String::new(),
                blocked: Some("Can't read this folder"),
            }],
        }
    }

    pub fn used(&self) -> &[Command] {
        &self.used
    }

    /// Draws the palette over `hits`; returns the item chosen.
    pub fn show(&mut self, ctx: &egui::Context, hits: &[Hit], home: &Path) -> Option<Item> {
        // The window dims behind the palette (frame 06a), and the palette
        // rises into place.
        let shown = glyphst_ui::motion::smooth_bool(
            ctx,
            egui::Id::new("command-palette-shown"),
            self.open,
            glyphst_ui::motion::PANEL,
        );
        glyphst_ui::overlay::fading_scrim(ctx, "command-palette-scrim", shown);
        if !self.open {
            return None;
        }
        let tokens = Tokens::get(ctx);
        let path_mode = self.mode == Mode::OpenPath;
        let (escape, up, down, tab, outside) = ctx.input_mut(|input| {
            let outside = input.pointer.button_pressed(egui::PointerButton::Primary)
                && input
                    .pointer
                    .interact_pos()
                    .is_some_and(|pos| self.rect.is_some_and(|rect| !rect.contains(pos)));
            let (up, down) = glyphst_ui::row::take_moves(input);
            (
                input.consume_key(egui::Modifiers::NONE, Key::Escape),
                up,
                down,
                path_mode && input.consume_key(egui::Modifiers::NONE, Key::Tab),
                outside,
            )
        });
        glyphst_ui::row::mark_list_open(ctx);
        if escape || outside {
            self.close();
            return None;
        }
        // A new `@` query starts on the section being read.
        let mut placed = false;
        if self.placed.as_deref() != Some(self.query.as_str()) {
            self.placed = Some(self.query.clone());
            if self.query.trim_start().starts_with('@')
                && let Some(index) = self
                    .current
                    .as_ref()
                    .and_then(|current| hits.iter().position(|hit| &hit.item == current))
            {
                self.highlight = index;
                placed = true;
            }
        }
        let keyed = self.highlight;
        if down {
            self.highlight += 1;
        }
        if up {
            self.highlight = self.highlight.saturating_sub(1);
        }
        self.highlight = self.highlight.min(hits.len().saturating_sub(1));
        // The list follows the highlight the keys moved.
        let follow = placed || ((up || down) && self.highlight != keyed);
        if tab
            && let Some(query) = hits
                .get(self.highlight)
                .and_then(|hit| completion(&self.query, home, hit))
        {
            self.set_query(ctx, query);
            ctx.request_repaint();
        }
        let screen = ctx.content_rect();
        let mut chosen = None;
        let previous_query = self.query.clone();
        let response = egui::Area::new(egui::Id::new("command-palette"))
            .order(egui::Order::Foreground)
            .pivot(Align2::CENTER_TOP)
            .fixed_pos(egui::pos2(
                screen.center().x,
                screen.top() + 72.0 + 8.0 * (1.0 - shown),
            ))
            .show(ctx, |ui| {
                glyphst_ui::pane::popover(ctx)
                    .inner_margin(egui::Margin::ZERO)
                    .show(ui, |ui| {
                        ui.set_width((screen.width() - 32.0).clamp(240.0, 600.0));
                        ui.spacing_mut().item_spacing.y = 0.0;
                        let field_row = ui.horizontal(|ui| {
                            ui.set_min_height(44.0);
                            ui.add_space(14.0);
                            let (prompt, hint) = if path_mode {
                                ("~", "type a path to a document…")
                            } else {
                                (
                                    ">",
                                    "type a command, a page number, a document or @section…",
                                )
                            };
                            ui.label(
                                egui::RichText::new(prompt)
                                    .font(theme::mono(14.0))
                                    .color(tokens.accent),
                            );
                            let count = if hits.len() == 1 {
                                "1 result".to_owned()
                            } else {
                                format!("{} results", hits.len())
                            };
                            let count_width =
                                glyphst_ui::text::width(ui, &count, &theme::mono(10.0)) + 28.0;
                            let field = ui.add(
                                egui::TextEdit::singleline(&mut self.query)
                                    .id(field_id())
                                    .hint_text(egui::RichText::new(hint).color(tokens.faint))
                                    // Tab completes a path instead of
                                    // moving the focus.
                                    .lock_focus(true)
                                    .font(theme::mono(14.0))
                                    .frame(egui::Frame::NONE)
                                    .margin(egui::Margin::symmetric(4, 8))
                                    .desired_width((ui.available_width() - count_width).max(40.0)),
                            );
                            ui.label(
                                egui::RichText::new(count)
                                    .font(theme::mono(10.0))
                                    .color(tokens.faint),
                            );
                            field
                        });
                        let field = field_row.inner;
                        if (field.has_focus() || field.lost_focus())
                            && ui.input(|i| i.key_pressed(Key::Enter))
                            && let Some(hit) = hits.get(self.highlight)
                        {
                            chosen = Some(hit.item.clone());
                        }
                        // Focus once, not on every frame: egui's
                        // request_focus also interrupts an input method's
                        // composition, which made Chinese and Japanese
                        // typing flicker and reset.
                        if !field.has_focus() && !field.lost_focus() {
                            field.request_focus();
                        }
                        let line = ui.available_rect_before_wrap();
                        ui.painter()
                            .hline(line.x_range(), line.top(), tokens.hairline());
                        ui.add_space(4.0);
                        let typed = needle(&self.query);
                        // The room under the field and over the foot. A new
                        // query starts at the top, with its first hit.
                        let room = screen.bottom() - ui.cursor().top() - 28.0 - 32.0;
                        egui::ScrollArea::vertical()
                            .id_salt(("palette-hits", &typed))
                            .max_height(room.clamp(60.0, MAX_LIST_HEIGHT))
                            .show(ui, |ui| {
                                let slide = glyphst_ui::row::Slide::under(
                                    ui,
                                    egui::Id::new(("palette-highlight", &typed)),
                                );
                                let mut highlighted_rect = None;
                                for (index, hit) in hits.iter().enumerate() {
                                    // A rule title where a kind of result starts.
                                    let group = group_of(&hit.item);
                                    if index == 0 || group_of(&hits[index - 1].item) != group {
                                        let count = hits[index..]
                                            .iter()
                                            .take_while(|other| group_of(&other.item) == group)
                                            .count();
                                        let (rect, _) = ui.allocate_exact_size(
                                            vec2(ui.available_width(), 24.0),
                                            Sense::hover(),
                                        );
                                        glyphst_ui::motif::paint_rule_title(
                                            ui,
                                            rect.shrink2(vec2(14.0, 0.0)),
                                            group,
                                            Some(&count.to_string()),
                                            true,
                                        );
                                    }
                                    let (rect, row) = ui.allocate_exact_size(
                                        vec2(ui.available_width(), 30.0),
                                        Sense::click(),
                                    );
                                    if row.hovered()
                                        && ui.input(|i| i.pointer.delta() != egui::Vec2::ZERO)
                                    {
                                        self.highlight = index;
                                    }
                                    let highlighted = index == self.highlight;
                                    if highlighted && follow {
                                        row.scroll_to_me(None);
                                    }
                                    if highlighted {
                                        highlighted_rect = Some(rect);
                                    }
                                    let blocked = hit.blocked.is_some();
                                    let (color, icon_color) = if blocked {
                                        (tokens.faint, tokens.faint)
                                    } else if highlighted {
                                        (tokens.text, tokens.accent_text)
                                    } else {
                                        (tokens.text_2, tokens.muted)
                                    };
                                    icons::paint(
                                        ui,
                                        egui::Rect::from_center_size(
                                            rect.left_center() + vec2(21.0, 0.0),
                                            vec2(14.0, 14.0),
                                        ),
                                        hit.icon,
                                        14.0,
                                        icon_color,
                                    );
                                    // The key chip or the reason at the right.
                                    let mut right = rect.right() - 14.0;
                                    if let Some(reason) = hit.blocked {
                                        let galley = ui.painter().layout_no_wrap(
                                            reason.to_lowercase(),
                                            theme::mono(10.0),
                                            tokens.faint,
                                        );
                                        right -= galley.size().x;
                                        ui.painter().galley(
                                            egui::pos2(
                                                right,
                                                rect.center().y - galley.size().y / 2.0,
                                            ),
                                            galley,
                                            tokens.faint,
                                        );
                                    } else if !hit.detail.is_empty() {
                                        let is_key = matches!(hit.item, Item::Command(_));
                                        if is_key {
                                            let label = glyphst_ui::key::display(&hit.detail);
                                            let size = glyphst_ui::key::size(ui, &label);
                                            right -= size.x;
                                            glyphst_ui::key::paint(
                                                ui,
                                                egui::Rect::from_min_size(
                                                    egui::pos2(
                                                        right,
                                                        rect.center().y - size.y / 2.0,
                                                    ),
                                                    size,
                                                ),
                                                &label,
                                                None,
                                            );
                                        } else {
                                            let galley = ui.painter().layout_no_wrap(
                                                hit.detail.to_lowercase(),
                                                theme::mono(10.0),
                                                tokens.faint,
                                            );
                                            right -= galley.size().x;
                                            ui.painter().galley(
                                                egui::pos2(
                                                    right,
                                                    rect.center().y - galley.size().y / 2.0,
                                                ),
                                                galley,
                                                tokens.faint,
                                            );
                                        }
                                    }
                                    let left = rect.left() + 38.0;
                                    let label = hit.label.to_lowercase();
                                    let mut job = egui::text::LayoutJob::default();
                                    let font = theme::mono(13.0);
                                    let found =
                                        (!typed.is_empty()).then(|| label.find(&typed)).flatten();
                                    match found {
                                        Some(start) => {
                                            let end = start + typed.len();
                                            let plain =
                                                egui::TextFormat::simple(font.clone(), color);
                                            let strong =
                                                egui::TextFormat::simple(font, tokens.accent_text);
                                            job.append(&label[..start], 0.0, plain.clone());
                                            job.append(&label[start..end], 0.0, strong);
                                            job.append(&label[end..], 0.0, plain);
                                        }
                                        None => job.append(
                                            &label,
                                            0.0,
                                            egui::TextFormat::simple(font, color),
                                        ),
                                    }
                                    job.wrap = egui::text::TextWrapping::truncate_at_width(
                                        (right - left - 12.0).max(0.0),
                                    );
                                    let galley = ui.painter().layout_job(job);
                                    ui.painter().galley(
                                        egui::pos2(left, rect.center().y - galley.size().y / 2.0),
                                        galley,
                                        color,
                                    );
                                    if row.clicked() {
                                        chosen = Some(hit.item.clone());
                                    }
                                }
                                slide.paint(ui, highlighted_rect, 0, tokens.accent_soft);
                            });
                        if hits.is_empty() {
                            ui.add_space(10.0);
                            let empty = if path_mode {
                                "no documents or folders match"
                            } else if self.query.trim() == "@" {
                                "this document has no sections"
                            } else if self.query.trim_start().starts_with('@') {
                                "no sections match"
                            } else {
                                "nothing matches"
                            };
                            ui.horizontal(|ui| {
                                ui.add_space(14.0);
                                ui.label(
                                    egui::RichText::new(empty)
                                        .font(theme::mono(11.0))
                                        .color(tokens.muted),
                                );
                            });
                            ui.add_space(10.0);
                        }
                        // The keys, along the foot.
                        ui.add_space(4.0);
                        let (foot, _) = ui
                            .allocate_exact_size(vec2(ui.available_width(), 28.0), Sense::hover());
                        ui.painter()
                            .rect_filled(foot, 0, tokens.surface.to_opaque());
                        ui.painter()
                            .hline(foot.x_range(), foot.top(), tokens.hairline());
                        let sections = self.query.trim_start().starts_with('@');
                        let keys = if path_mode {
                            "↑↓ move   ↲ open   tab complete   esc close"
                        } else if sections {
                            "↑↓ move   ↲ go   esc close"
                        } else {
                            "↑↓ move   ↲ run   esc close"
                        };
                        ui.painter().text(
                            foot.left_center() + vec2(14.0, 0.0),
                            Align2::LEFT_CENTER,
                            keys,
                            theme::mono(10.0),
                            tokens.faint,
                        );
                        if !path_mode {
                            ui.painter().text(
                                foot.right_center() - vec2(14.0, 0.0),
                                Align2::RIGHT_CENTER,
                                if sections {
                                    "@ sections only"
                                } else {
                                    "@ sections   a number goes to a page"
                                },
                                theme::mono(10.0),
                                tokens.faint,
                            );
                        }
                    });
            });
        self.rect = Some(response.response.rect);
        if self.query != previous_query {
            self.highlight = 0;
        }
        // A folder lists its contents instead of closing the prompt.
        if let Some(Item::Browse(query)) = chosen {
            self.set_query(ctx, query);
            ctx.request_repaint();
            return None;
        }
        if let Some(Item::Command(command)) = &chosen {
            self.used.retain(|used| used != command);
            self.used.insert(0, *command);
            self.used.truncate(MAX_USED);
        }
        if chosen.is_some() {
            self.close();
        }
        chosen
    }
}

/// The rule title over a kind of result.
fn group_of(item: &Item) -> &'static str {
    match item {
        Item::Command(_) => "commands",
        Item::Tab(_) => "tabs",
        Item::Open(_) | Item::Browse(_) => "files",
        Item::GoToPage(_) => "pages",
        Item::Settings(_) => "settings",
        Item::Section(_) => "sections",
    }
}

/// The query field's id, to move its caret after replacing the query.
fn field_id() -> egui::Id {
    egui::Id::new("command-palette-field")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sources(document: bool) -> Sources {
        Sources {
            commands: Command::ALL
                .into_iter()
                .map(|command| {
                    let blocked = (!document && command.needs() != crate::commands::Needs::Nothing)
                        .then(|| command.needs().reason());
                    (command, None, blocked)
                })
                .collect(),
            tabs: if document {
                vec!["War and Peace".into()]
            } else {
                Vec::new()
            },
            recent: vec![PathBuf::from("/books/brochure.pdf")],
            pages: document.then_some(1259),
            ..Default::default()
        }
    }

    #[test]
    fn presenting_offers_its_commands_with_their_labels_and_no_files() {
        let mut sources = sources(true);
        sources.presenting = true;
        sources.commands.retain(|(command, ..)| {
            command.category() == crate::commands::Category::Present || *command == Command::Present
        });
        sources.labels.insert(Command::PresentDraw, "End draw");
        let found = hits("", &sources, &[]);
        assert!(found.iter().all(|hit| matches!(hit.item, Item::Command(_))));
        assert!(found.iter().any(|hit| hit.label == "End draw"));
        assert_eq!(
            found.last().map(|hit| &hit.item),
            Some(&Item::Command(Command::Present)),
            "presenting's own commands first"
        );
        assert!(hits("end dr", &sources, &[])[0].item == Item::Command(Command::PresentDraw));
        assert!(hits("brochure", &sources, &[]).is_empty());
        assert_eq!(hits("12", &sources, &[])[0].item, Item::GoToPage(11));
    }

    #[test]
    fn a_number_goes_to_that_page_of_the_active_document() {
        let found = hits("12", &sources(true), &[]);
        assert_eq!(found[0].item, Item::GoToPage(11));
        assert!(
            hits("2000", &sources(true), &[])
                .iter()
                .all(|hit| !matches!(hit.item, Item::GoToPage(_)))
        );
        assert!(
            hits("12", &sources(false), &[])
                .iter()
                .all(|hit| !matches!(hit.item, Item::GoToPage(_)))
        );
    }

    #[test]
    fn hits_rank_matches_and_say_why_commands_cannot_run() {
        let found = hits("zoom in", &sources(false), &[]);
        assert_eq!(found[0].item, Item::Command(Command::ZoomIn));
        assert_eq!(found[0].blocked, Some("Open a document first"));

        let found = hits("war", &sources(true), &[]);
        assert_eq!(found[0].item, Item::Tab(0));
        let found = hits("broch", &sources(true), &[]);
        assert_eq!(
            found[0].item,
            Item::Open(PathBuf::from("/books/brochure.pdf"))
        );
        let found = hits("shortcuts", &sources(true), &[]);
        assert_eq!(found[0].item, Item::Command(Command::KeyboardShortcuts));
        let found = hits("appearance", &sources(true), &[]);
        assert_eq!(found[0].item, Item::Settings(Section::Appearance));
        // A subsequence still finds it.
        let found = hits("fdnx", &sources(true), &[]);
        assert_eq!(found[0].item, Item::Command(Command::FindNext));
    }

    #[test]
    fn kinds_of_hits_stay_together() {
        let hit = |item: Item| Hit {
            item,
            icon: "x",
            label: String::new(),
            detail: String::new(),
            blocked: None,
        };
        let hits = grouped(vec![
            hit(Item::Command(Command::Find)),
            hit(Item::Tab(0)),
            hit(Item::Command(Command::Palette)),
            hit(Item::GoToPage(1)),
        ]);
        let groups: Vec<&str> = hits.iter().map(|hit| group_of(&hit.item)).collect();
        assert_eq!(groups, ["commands", "commands", "tabs", "pages"]);
        assert_eq!(hits[1].item, Item::Command(Command::Palette));
    }

    #[test]
    fn recently_run_commands_come_first() {
        let found = hits("", &sources(true), &[Command::Assistant, Command::Present]);
        let commands: Vec<&Item> = found.iter().map(|hit| &hit.item).take(2).collect();
        assert_eq!(
            commands,
            [
                &Item::Command(Command::Assistant),
                &Item::Command(Command::Present)
            ]
        );
    }

    fn entries() -> Listing {
        [
            ("Papers", true),
            ("papers.pdf", false),
            ("notes.txt", false),
            ("Novel.epub", false),
            (".cache", true),
            ("pictures", true),
        ]
        .into_iter()
        .map(|(name, folder)| (name.to_string(), folder))
        .collect()
    }

    fn labels(hits: &[Hit]) -> Vec<&str> {
        hits.iter().map(|hit| hit.label.as_str()).collect()
    }

    #[test]
    fn the_path_prompt_lists_folders_and_documents() {
        let home = Path::new("/home/reader");
        // Only folders and documents, without hidden ones.
        let found = path_hits("~/", home, &entries());
        assert_eq!(
            labels(&found),
            ["Papers/", "pictures/", "Novel.epub", "papers.pdf"]
        );
        assert_eq!(found[0].item, Item::Browse("~/Papers/".into()));
        assert_eq!(
            found[3].item,
            Item::Open(PathBuf::from("/home/reader/papers.pdf"))
        );
        assert_eq!(found[3].detail, "PDF");
        // Folders come before documents that match as well.
        assert_eq!(
            labels(&path_hits("/books/pap", home, &entries())),
            ["Papers/", "papers.pdf"]
        );
        assert_eq!(
            path_hits("/books/pap", home, &entries())[1].item,
            Item::Open(PathBuf::from("/books/papers.pdf"))
        );
        // A subsequence matches; a dot shows hidden folders.
        assert_eq!(
            labels(&path_hits("~/nvl", home, &entries())),
            ["Novel.epub"]
        );
        assert_eq!(labels(&path_hits("~/.c", home, &entries())), [".cache/"]);
    }

    #[test]
    fn paths_resolve_from_home() {
        let home = Path::new("/home/reader");
        assert_eq!(
            split_path("~/Books/war", home),
            (PathBuf::from("/home/reader/Books/"), "~/Books/", "war")
        );
        assert_eq!(
            split_path("~", home),
            (PathBuf::from("/home/reader"), "~/", "")
        );
        assert_eq!(split_path("/tmp/a", home).0, PathBuf::from("/tmp/"));
        assert_eq!(split_path("war", home).0, PathBuf::from("/home/reader"));
        // Tab completes a folder with its slash and a document in place.
        let found = path_hits("~/Books/pa", home, &entries());
        assert_eq!(
            completion("~/Books/pa", home, &found[0]).as_deref(),
            Some("~/Books/Papers/")
        );
        assert_eq!(
            completion("~/Books/pa", home, &found[1]).as_deref(),
            Some("~/Books/papers.pdf")
        );
    }

    /// One frame of the palette over `hits` with `events`.
    fn frame(
        ctx: &egui::Context,
        palette: &mut Palette,
        hits: &[Hit],
        events: Vec<egui::Event>,
    ) -> Option<Item> {
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(1000.0, 700.0))),
            events,
            ..Default::default()
        };
        let mut chosen = None;
        let mut output = ctx.run_ui(input, |ui| {
            chosen = palette.show(ui.ctx(), hits, Path::new("/home/reader"));
        });
        output.textures_delta.clear();
        chosen
    }

    fn key(key: Key) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }
    }

    #[test]
    fn ctrl_n_and_ctrl_p_move_the_highlight_like_the_arrows() {
        let ctx = egui::Context::default();
        ctx.set_fonts(crate::fonts::embedded());
        let listing: Listing = (0..5)
            .map(|index| (format!("book-{index}.pdf"), false))
            .collect();
        let hits = path_hits("~/", Path::new("/home/reader"), &listing);
        let mut palette = Palette::default();
        palette.open_path(&ctx, "~/".into());
        frame(&ctx, &mut palette, &hits, Vec::new());
        let ctrl = |key| egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::CTRL,
        };
        for pressed in [Key::N, Key::N, Key::P] {
            frame(&ctx, &mut palette, &hits, vec![ctrl(pressed)]);
        }
        let chosen = frame(&ctx, &mut palette, &hits, vec![key(Key::Enter)]);
        assert_eq!(chosen, Some(hits[1].item.clone()));
    }

    /// An outline whose odd entries have no page, so indices skip.
    fn outline() -> Vec<Heading> {
        [
            ("Abstract", 0, 0),
            ("1 Introduction", 0, 1),
            ("3 Model architecture", 0, 2),
            ("3.2 Attention", 1, 3),
            ("4 Why self-attention", 0, 6),
        ]
        .into_iter()
        .enumerate()
        .map(|(order, (title, depth, page))| Heading {
            index: order * 2,
            title: title.into(),
            page,
            depth,
        })
        .collect()
    }

    #[test]
    fn an_at_lists_the_outline_and_filters_it() {
        let mut sources = sources(true);
        sources.sections = outline();
        let all = hits("@", &sources, &[]);
        assert_eq!(all.len(), 5);
        assert!(all.iter().all(|hit| group_of(&hit.item) == "sections"));
        // In reading order, indented by depth, with the outline's index.
        assert_eq!(all[3].label, "  3.2 Attention");
        assert_eq!(all[3].item, Item::Section(6));
        assert_eq!(all[3].detail, "p.4");
        // Matches first, then the looser ones in reading order.
        let labels: Vec<String> = hits("@ att", &sources, &[])
            .into_iter()
            .map(|hit| hit.label)
            .collect();
        assert_eq!(
            labels,
            [
                "3.2 Attention",
                "4 Why self-attention",
                "Abstract",
                "3 Model architecture"
            ]
        );
        assert!(hits("@zzz", &sources, &[]).is_empty());
    }

    #[test]
    fn a_query_finds_a_few_sections_among_the_commands() {
        let mut sources = sources(true);
        sources.sections = outline();
        let found = hits("attention", &sources, &[]);
        let sections: Vec<&Hit> = found
            .iter()
            .filter(|hit| matches!(hit.item, Item::Section(_)))
            .collect();
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[0].label, "3.2 Attention");
        assert!(
            !hits("", &sources, &[])
                .iter()
                .any(|hit| matches!(hit.item, Item::Section(_)))
        );
        sources.sections = (0..20)
            .map(|index| Heading {
                index,
                title: format!("Attention {index}"),
                page: index,
                depth: 0,
            })
            .collect();
        let found = hits("attention", &sources, &[]);
        assert_eq!(
            found
                .iter()
                .filter(|hit| matches!(hit.item, Item::Section(_)))
                .count(),
            MAX_MIXED_SECTIONS
        );
    }

    #[test]
    fn an_at_starts_on_the_section_being_read() {
        let ctx = egui::Context::default();
        ctx.set_fonts(crate::fonts::embedded());
        let mut sources = sources(true);
        sources.sections = outline();
        let mut palette = Palette::default();
        palette.toggle();
        palette.query = "@".into();
        palette.current = Some(Item::Section(6));
        let hits = hits(&palette.query, &sources, &[]);
        frame(&ctx, &mut palette, &hits, Vec::new());
        assert_eq!(
            frame(&ctx, &mut palette, &hits, vec![key(Key::Enter)]),
            Some(Item::Section(6))
        );
    }

    #[test]
    fn a_folder_opens_in_the_prompt_and_a_document_closes_it() {
        let ctx = egui::Context::default();
        ctx.set_fonts(crate::fonts::embedded());
        let mut palette = Palette::default();
        palette.open_path(&ctx, "~/".into());
        let hits = path_hits("~/", Path::new("/home/reader"), &entries());
        for _ in 0..2 {
            frame(&ctx, &mut palette, &hits, Vec::new());
        }
        // Enter on a folder lists it instead of closing.
        assert_eq!(
            frame(&ctx, &mut palette, &hits, vec![key(Key::Enter)]),
            None
        );
        assert!(palette.open);
        assert_eq!(palette.query, "~/Papers/");
        // Tab completes the highlighted hit.
        palette.set_query(&ctx, "~/".into());
        frame(&ctx, &mut palette, &hits, vec![key(Key::ArrowDown)]);
        frame(&ctx, &mut palette, &hits, vec![key(Key::Tab)]);
        assert_eq!(palette.query, "~/pictures/");
        assert_eq!(palette.mode, Mode::OpenPath);
        // Enter on a document opens it and closes the prompt.
        let documents = &hits[2..];
        for _ in 0..2 {
            frame(&ctx, &mut palette, documents, Vec::new());
        }
        let chosen = frame(&ctx, &mut palette, documents, vec![key(Key::Enter)]);
        assert_eq!(chosen, Some(documents[0].item.clone()));
        assert!(!palette.open);
        assert_eq!(palette.mode, Mode::Commands);
    }

    #[test]
    fn a_large_folder_scrolls_and_every_document_is_reachable() {
        let ctx = egui::Context::default();
        ctx.set_fonts(crate::fonts::embedded());
        let listing: Listing = (0..40)
            .map(|index| (format!("book-{index:02}.pdf"), false))
            .collect();
        let hits = path_hits("~/", Path::new("/home/reader"), &listing);
        assert_eq!(hits.len(), 40);
        let mut palette = Palette::default();
        palette.open_path(&ctx, "~/".into());
        for _ in 0..2 {
            frame(&ctx, &mut palette, &hits, Vec::new());
        }
        // The list scrolls within the window instead of running past it.
        assert!(palette.rect.unwrap().bottom() <= 700.0);
        for _ in 0..30 {
            frame(&ctx, &mut palette, &hits, vec![key(Key::ArrowDown)]);
        }
        let chosen = frame(&ctx, &mut palette, &hits, vec![key(Key::Enter)]);
        assert_eq!(chosen, Some(hits[30].item.clone()));
    }

    /// One frame of the palette with `events`; returns the IME output.
    fn ime_frame(
        ctx: &egui::Context,
        palette: &mut Palette,
        events: Vec<egui::Event>,
    ) -> Option<egui::output::IMEOutput> {
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(1000.0, 700.0))),
            events,
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| {
            palette.show(ui.ctx(), &[], Path::new("/home/reader"));
        });
        output.textures_delta.clear();
        output.platform_output.ime
    }

    #[test]
    fn typing_with_an_input_method_is_not_interrupted() {
        let ctx = egui::Context::default();
        ctx.set_fonts(crate::fonts::embedded());
        let mut palette = Palette::default();
        palette.toggle();
        // The first frames focus the field.
        for _ in 0..2 {
            ime_frame(&ctx, &mut palette, Vec::new());
        }
        for _ in 0..3 {
            let ime = ime_frame(&ctx, &mut palette, Vec::new()).expect("the field owns the IME");
            assert!(!ime.should_interrupt_composition, "composition interrupted");
        }
        let preedit = egui::Event::Ime(egui::ImeEvent::Preedit {
            text: "かんじ".into(),
            active_range_chars: None,
        });
        let ime = ime_frame(&ctx, &mut palette, vec![preedit]).unwrap();
        assert!(!ime.should_interrupt_composition);
        let commit = egui::Event::Ime(egui::ImeEvent::Commit("漢字".into()));
        let ime = ime_frame(&ctx, &mut palette, vec![commit]).unwrap();
        assert!(!ime.should_interrupt_composition);
        assert_eq!(palette.query, "漢字");
    }
}
