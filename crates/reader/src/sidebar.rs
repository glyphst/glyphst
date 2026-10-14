//! The document sidebar, after pdfcraft's side panels (artcraft,
//! `ui-egui/src/panels.rs`, `comments_panel.rs`, `search_ui.rs`): outline,
//! page thumbnails, bookmarks, highlights and notes, and search results.

use std::{
    collections::{HashMap, HashSet},
    time::Instant,
};

use glyphst_library::HighlightColor;
use serde::{Deserialize, Serialize};

use crate::{
    annotations::{self, AnnotationId},
    document::OutlineEntry,
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
    /// Follow the section being read again and show it.
    LocateOutline,
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

/// A tab's outline and thumbnail lists, after the GPUI Reader's: the
/// outline follows the section being read until it is scrolled away from
/// by hand, and the thumbnails follow the page.
#[derive(Default)]
pub struct OutlineView {
    collapsed: HashSet<usize>,
    /// Scrolling the current entry out of the list by hand stops following
    /// it until it is located again or an entry is chosen.
    paused: bool,
    /// The current entry when the outline was last drawn.
    current: Option<usize>,
    /// A reveal of the current entry waiting for the next frame; strict
    /// ones centre it even when it shows.
    reveal: Option<bool>,
    list: ListState,
    pages: ListState,
    /// The page the thumbnails last followed.
    page: Option<usize>,
}

impl OutlineView {
    /// Whether the outline follows the section being read.
    pub fn following(&self) -> bool {
        !self.paused
    }

    /// Follows the section being read again and centres it.
    pub fn locate(&mut self) {
        self.paused = false;
        self.reveal = Some(true);
    }
}

/// Where a list was scrolled when last drawn.
#[derive(Default)]
struct ListState {
    offset: f32,
    viewport: f32,
    /// The frame it was last drawn in.
    frame: Option<u64>,
}

impl ListState {
    /// Marks the list drawn this frame; true when it was not drawn in the
    /// last one (the sidebar or the tab was just opened).
    fn shown(&mut self, ctx: &egui::Context) -> bool {
        let frame = ctx.cumulative_frame_nr();
        let fresh = self.frame.is_none_or(|last| last + 1 < frame);
        self.frame = Some(frame);
        fresh
    }

    fn viewport_or(&self, ui: &egui::Ui) -> f32 {
        if self.viewport > 0.0 {
            self.viewport
        } else {
            ui.available_height()
        }
    }
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
            if which == SidebarTab::Outline && !tab.outline.is_empty() {
                let following = tab.outline_view.following();
                let tip = if following {
                    "Following the current section"
                } else {
                    "Locate the current section"
                };
                if icons::button(ui, "locate-fixed", 26.0, following, tip).clicked() {
                    actions.push(SidebarAction::LocateOutline);
                }
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
    /// The outline's expand and collapse toggle.
    disclosure: Disclosure,
    selected: bool,
    enabled: bool,
    /// Icon buttons (icon, tooltip) shown over the detail while hovered.
    actions: &'a [(&'a str, &'a str)],
}

/// The toggle before a nested outline entry's title.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Disclosure {
    /// A flat list: no room for toggles.
    None,
    /// Room for a toggle, but no children.
    Leaf,
    Open,
    Closed,
}

const DEPTH_INDENT: f32 = 14.0;
const DISCLOSURE_WIDTH: f32 = 18.0;

/// Where a row's disclosure toggle is, at `depth`.
fn disclosure_rect(rect: egui::Rect, depth: usize) -> egui::Rect {
    let left = rect.left() + 4.0 + depth as f32 * DEPTH_INDENT;
    egui::Rect::from_min_size(
        egui::pos2(left, rect.top() + 2.0),
        egui::vec2(DISCLOSURE_WIDTH, rect.height() - 4.0),
    )
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
    let mut left = rect.left() + 8.0 + row.depth as f32 * DEPTH_INDENT;
    if row.disclosure != Disclosure::None {
        let toggle = disclosure_rect(rect, row.depth);
        let icon = match row.disclosure {
            Disclosure::Open => Some("chevron-down"),
            Disclosure::Closed => Some("chevron-right"),
            _ => None,
        };
        if let Some(icon) = icon {
            icons::paint(ui, toggle, icon, 14.0, tokens.text_muted);
        }
        left = toggle.right() + 2.0;
    }
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

const OUTLINE_ROW: f32 = 28.0;

fn outline(
    ui: &mut egui::Ui,
    tab: &mut DocumentTab,
    filter: &str,
    actions: &mut Vec<SidebarAction>,
) {
    if tab.outline.is_empty() {
        muted(ui, "This document has no outline.");
        return;
    }
    let current = tab.current_outline_entry();
    let entries = &tab.outline;
    let view = &mut tab.outline_view;
    let filtering = !filter.is_empty();
    if view.list.shown(ui.ctx()) {
        view.locate();
    }
    let changed = std::mem::replace(&mut view.current, current) != current;
    if !view.paused
        && let Some(entry) = current
    {
        let expanded = !filtering && expand_ancestors(entries, &mut view.collapsed, entry);
        if changed || expanded {
            view.reveal.get_or_insert(false);
        }
    }
    let rows = visible_rows(entries, &view.collapsed, filter);
    if rows.is_empty() {
        muted(ui, "No matches.");
        return;
    }
    let nested = !filtering && entries.iter().any(|entry| entry.parent.is_some());
    let current_row = current.and_then(|entry| rows.iter().position(|&row| row == entry));

    let mut scroll = egui::ScrollArea::vertical()
        .id_salt(("outline", tab.id))
        .auto_shrink(false);
    let mut scrolled = false;
    if let Some(strict) = view.reveal.take()
        && let (Some(entry), Some(row)) = (current, current_row)
    {
        let sticky = if filtering {
            0
        } else {
            ancestors(entries, entry).len()
        };
        let viewport = view.list.viewport_or(ui);
        if let Some(offset) =
            reveal_offset(row, sticky, OUTLINE_ROW, viewport, view.list.offset, strict)
        {
            scroll = scroll.vertical_scroll_offset(offset);
            scrolled = true;
        }
    }

    let mut chosen = None;
    let mut toggled = None;
    let output = scroll.show_viewport(ui, |ui, viewport| {
        ui.set_height(rows.len() as f32 * OUTLINE_ROW);
        let origin = ui.max_rect().min;
        let width = ui.available_width();
        let row_rect = |y: f32| {
            egui::Rect::from_min_size(
                egui::pos2(origin.x, origin.y + y),
                egui::vec2(width, OUTLINE_ROW),
            )
        };
        let first = (viewport.min.y / OUTLINE_ROW).floor().max(0.0) as usize;
        let last = ((viewport.max.y / OUTLINE_ROW).ceil().max(0.0) as usize).min(rows.len());
        let disclosure = |index: usize, sticky: bool| {
            if !nested {
                Disclosure::None
            } else if !has_children(entries, index) {
                Disclosure::Leaf
            } else if !sticky && view.collapsed.contains(&index) {
                Disclosure::Closed
            } else {
                Disclosure::Open
            }
        };
        let stuck = if filtering {
            Vec::new()
        } else {
            stuck_rows(entries, &rows, first..last, viewport.min.y)
        };
        // Rows scroll under the sticky ones clipped at their bottom, so
        // nothing shows through them (the drawer may be translucent) and a
        // covered part takes no clicks.
        let band_bottom = origin.y + viewport.min.y + stuck.len() as f32 * OUTLINE_ROW;
        let mut draw = |ui: &mut egui::Ui, rect: egui::Rect, index: usize, sticky: bool| {
            let entry = &entries[index];
            let mut child = ui.new_child(
                egui::UiBuilder::new()
                    .id_salt((index, sticky))
                    .max_rect(rect)
                    .layout(egui::Layout::top_down(egui::Align::Min)),
            );
            if !sticky {
                let clip = child.clip_rect();
                child.set_clip_rect(clip.intersect(below_band(rect, band_bottom)));
            }
            let disclosure = disclosure(index, sticky);
            let (response, _) = row(
                &mut child,
                Row {
                    title: &entry.title,
                    detail: entry.page.map(|page| (page + 1).to_string()),
                    depth: entry.depth,
                    dot: None,
                    disclosure,
                    selected: current == Some(index),
                    enabled: entry.page.is_some(),
                    actions: &[],
                },
            );
            // A toggle drawn over its row takes its clicks.
            if !sticky && matches!(disclosure, Disclosure::Open | Disclosure::Closed) {
                let toggle = child.interact(
                    disclosure_rect(rect, entry.depth),
                    child.id().with("disclosure"),
                    egui::Sense::click(),
                );
                if toggle.clicked() {
                    toggled = Some(index);
                    return;
                }
            }
            if response.clicked() && entry.page.is_some() {
                chosen = Some(index);
            }
        };
        for (position, &index) in rows.iter().enumerate().take(last).skip(first) {
            draw(ui, row_rect(position as f32 * OUTLINE_ROW), index, false);
        }
        for (position, &index) in stuck.iter().enumerate() {
            draw(
                ui,
                row_rect(viewport.min.y + position as f32 * OUTLINE_ROW),
                index,
                true,
            );
        }
        if !stuck.is_empty() {
            fade_shadow(ui.painter(), origin.x, origin.x + width, band_bottom);
        }
    });

    let offset = output.state.offset.y;
    // Scrolling by hand stops following once the current row is out of
    // the list.
    if !scrolled
        && (offset - view.list.offset).abs() > 0.5
        && !row_is_visible(current_row, offset, output.inner_rect.height(), OUTLINE_ROW)
    {
        view.paused = true;
    }
    view.list.offset = offset;
    view.list.viewport = output.inner_rect.height();
    if let Some(index) = toggled
        && !view.collapsed.remove(&index)
    {
        view.collapsed.insert(index);
    }
    if let Some(index) = chosen
        && let Some(page) = entries[index].page
    {
        actions.push(SidebarAction::View(ViewAction::GoTo {
            page,
            y: entries[index].y,
        }));
        view.current = Some(index);
        view.locate();
    }
}

/// The entries that stick at the top of the outline (Zed's sticky scroll):
/// the parents of the top row of `shown` (indices into `rows`), scrolled
/// `scroll` points down.
fn stuck_rows(
    entries: &[OutlineEntry],
    rows: &[usize],
    shown: std::ops::Range<usize>,
    scroll: f32,
) -> Vec<usize> {
    if scroll <= 0.0 || shown.is_empty() {
        return Vec::new();
    }
    let first = shown.start;
    let depths: Vec<usize> = rows[shown]
        .iter()
        .map(|&index| entries[index].depth)
        .collect();
    let Some(anchor) = sticky_anchor(&depths, first) else {
        return Vec::new();
    };
    let mut stuck = ancestors(entries, rows[anchor.index]);
    if anchor.drifting && drift(anchor.index, stuck.len(), OUTLINE_ROW, scroll) < 0.0 {
        // The deepest parent goes as a whole, never half covered.
        stuck.pop();
    }
    stuck
}

/// The part of a scrolling row drawn and clickable: below the sticky rows,
/// whose band ends at `band_bottom`.
fn below_band(rect: egui::Rect, band_bottom: f32) -> egui::Rect {
    rect.with_min_y(rect.top().max(band_bottom).min(rect.bottom()))
}

/// A soft shadow under the outline's sticky rows.
fn fade_shadow(painter: &egui::Painter, left: f32, right: f32, top: f32) {
    let dark = egui::Color32::from_black_alpha(26);
    let clear = egui::Color32::TRANSPARENT;
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(egui::pos2(left, top), dark);
    mesh.colored_vertex(egui::pos2(right, top), dark);
    mesh.colored_vertex(egui::pos2(right, top + 6.0), clear);
    mesh.colored_vertex(egui::pos2(left, top + 6.0), clear);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    painter.add(mesh);
}

/// The outline entries listed: the matches when filtering, else every
/// entry not inside a collapsed one.
fn visible_rows(entries: &[OutlineEntry], collapsed: &HashSet<usize>, filter: &str) -> Vec<usize> {
    if !filter.is_empty() {
        return (0..entries.len())
            .filter(|&index| matches(filter, &entries[index].title))
            .collect();
    }
    let mut rows = Vec::new();
    // Entries nest in reading order: a collapsed entry's children follow
    // it, deeper than it.
    let mut hidden_below = None;
    for (index, entry) in entries.iter().enumerate() {
        if hidden_below.is_some_and(|depth| entry.depth > depth) {
            continue;
        }
        hidden_below = collapsed.contains(&index).then_some(entry.depth);
        rows.push(index);
    }
    rows
}

fn has_children(entries: &[OutlineEntry], index: usize) -> bool {
    entries
        .get(index + 1)
        .is_some_and(|next| next.parent == Some(index))
}

/// The entries `index` is nested in, outermost first.
fn ancestors(entries: &[OutlineEntry], index: usize) -> Vec<usize> {
    let mut chain = Vec::new();
    let mut parent = entries.get(index).and_then(|entry| entry.parent);
    while let Some(index) = parent {
        // Parents come first, so a broken chain cannot loop.
        if chain.len() >= entries.len() {
            break;
        }
        chain.push(index);
        parent = entries.get(index).and_then(|entry| entry.parent);
    }
    chain.reverse();
    chain
}

/// Expands the entries `index` is nested in; true when one was collapsed.
fn expand_ancestors(
    entries: &[OutlineEntry],
    collapsed: &mut HashSet<usize>,
    index: usize,
) -> bool {
    let mut changed = false;
    for ancestor in ancestors(entries, index) {
        changed |= collapsed.remove(&ancestor);
    }
    changed
}

/// The row whose parents stick at the top of a list (Zed's sticky scroll,
/// as the GPUI Reader had it), found from the depths of the rows shown from
/// row `first`. A drifting anchor is the last child before an outdent: its
/// deepest parent leaves with it.
#[derive(Debug, PartialEq, Eq)]
struct StickyAnchor {
    index: usize,
    drifting: bool,
}

fn sticky_anchor(depths: &[usize], first: usize) -> Option<StickyAnchor> {
    for (ix, &depth) in depths.iter().enumerate() {
        if depth < ix {
            return Some(StickyAnchor {
                index: first + ix,
                drifting: false,
            });
        }
        if let Some(&next) = depths.get(ix + 1)
            && next + 1 == depth
            && (depth == ix || depth == ix + 1)
        {
            return Some(StickyAnchor {
                index: first + ix,
                drifting: depth == ix + 1,
            });
        }
    }
    None
}

/// How far a drifting anchor's deepest parent is pushed up (zero or less)
/// with `stuck` sticky rows at scroll offset `offset`.
fn drift(anchor: usize, stuck: usize, row: f32, offset: f32) -> f32 {
    let anchor_bottom = row * (anchor + 1) as f32;
    (anchor_bottom - offset - row * stuck as f32).min(0.0)
}

/// Whether any of row `row` shows at scroll offset `offset`.
fn row_is_visible(row: Option<usize>, offset: f32, viewport: f32, height: f32) -> bool {
    row.is_some_and(|row| {
        let top = row as f32 * height;
        top + height > offset && top < offset + viewport
    })
}

/// The scroll offset that centres row `row` below `sticky` sticky rows,
/// or `None` when it shows there already and `strict` is not asked.
fn reveal_offset(
    row: usize,
    sticky: usize,
    height: f32,
    viewport: f32,
    offset: f32,
    strict: bool,
) -> Option<f32> {
    let top = row as f32 * height;
    let covered = sticky as f32 * height;
    if !strict && top >= offset + covered && top + height <= offset + viewport {
        return None;
    }
    let free = (viewport - covered).max(height);
    Some((top - covered - (free - height) / 2.0).max(0.0))
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
    // The current page's thumbnail is brought into view when the page
    // changes, centred when the list opens.
    let view = &mut tab.outline_view;
    let fresh = view.pages.shown(ui.ctx());
    let mut scroll = egui::ScrollArea::vertical()
        .id_salt(("pages", tab.id))
        .auto_shrink(false);
    if fresh || view.page != Some(current) {
        view.page = Some(current);
        let stride = row_height + ui.spacing().item_spacing.y;
        let viewport = view.pages.viewport_or(ui);
        if let Some(offset) = reveal_offset(current, 0, stride, viewport, view.pages.offset, fresh)
        {
            scroll = scroll.vertical_scroll_offset(offset);
        }
    }
    let output = scroll.show_rows(ui, row_height, count, |ui, rows| {
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
    tab.outline_view.pages.offset = output.state.offset.y;
    tab.outline_view.pages.viewport = output.inner_rect.height();
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
                                disclosure: Disclosure::None,
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
                                disclosure: Disclosure::None,
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

    fn entry(depth: usize, parent: Option<usize>) -> OutlineEntry {
        OutlineEntry {
            title: format!("entry {depth}"),
            page: Some(0),
            y: None,
            depth,
            parent,
        }
    }

    /// An outline from its entries' depths, in reading order.
    fn outline(depths: &[usize]) -> Vec<OutlineEntry> {
        let mut entries: Vec<OutlineEntry> = Vec::new();
        for &depth in depths {
            let parent = entries.iter().rposition(|entry| entry.depth + 1 == depth);
            entries.push(entry(depth, parent.filter(|_| depth > 0)));
        }
        entries
    }

    #[test]
    fn rows_under_the_sticky_parents_show_only_below_them() {
        // A chapter with a long section: scrolled into the section's
        // children, the chapter and the section stick.
        let entries = outline(&[0, 1, 2, 2, 2, 2, 2, 2, 2, 2]);
        let rows: Vec<usize> = (0..entries.len()).collect();
        let scroll = 3.5 * OUTLINE_ROW;
        let first = 3;
        let stuck = stuck_rows(&entries, &rows, first..rows.len(), scroll);
        assert_eq!(stuck, [0, 1]);
        assert!(stuck_rows(&entries, &rows, 0..rows.len(), 0.0).is_empty());

        let band_bottom = scroll + stuck.len() as f32 * OUTLINE_ROW;
        for row in first..rows.len() {
            let rect = egui::Rect::from_min_size(
                egui::pos2(0.0, row as f32 * OUTLINE_ROW),
                egui::vec2(200.0, OUTLINE_ROW),
            );
            let shown = below_band(rect, band_bottom);
            assert!(shown.top() >= band_bottom - f32::EPSILON || shown.height() == 0.0);
            assert!(shown.bottom() == rect.bottom() && shown.height() >= 0.0);
        }
        // A row wholly under the band shows nothing; one below it, all.
        let covered =
            egui::Rect::from_min_size(egui::pos2(0.0, scroll), egui::vec2(200.0, OUTLINE_ROW));
        assert_eq!(below_band(covered, band_bottom).height(), 0.0);
        let free = covered.translate(egui::vec2(0.0, 3.0 * OUTLINE_ROW));
        assert_eq!(below_band(free, band_bottom), free);
    }

    #[test]
    fn ancestors_are_outermost_first_without_a_depth_cap() {
        let entries = outline(&(0..12).collect::<Vec<_>>());
        assert_eq!(ancestors(&entries, 11), (0..11).collect::<Vec<_>>());
        assert!(ancestors(&entries, 0).is_empty());
    }

    #[test]
    fn following_expands_only_the_current_entrys_ancestors() {
        let entries = outline(&[0, 1, 2, 0]);
        let mut collapsed = HashSet::from([0, 1, 3]);
        assert!(expand_ancestors(&entries, &mut collapsed, 2));
        assert_eq!(collapsed, HashSet::from([3]));
        assert!(!expand_ancestors(&entries, &mut collapsed, 2));
    }

    #[test]
    fn collapsed_entries_hide_their_children() {
        let entries = outline(&[0, 1, 2, 1, 0, 1]);
        assert_eq!(
            visible_rows(&entries, &HashSet::new(), ""),
            vec![0, 1, 2, 3, 4, 5]
        );
        assert_eq!(
            visible_rows(&entries, &HashSet::from([1]), ""),
            vec![0, 1, 3, 4, 5]
        );
        assert_eq!(
            visible_rows(&entries, &HashSet::from([0, 2]), ""),
            vec![0, 4, 5]
        );
        let parents: Vec<usize> = (0..entries.len())
            .filter(|&index| has_children(&entries, index))
            .collect();
        assert_eq!(parents, vec![0, 1, 4]);
    }

    #[test]
    fn finds_stationary_anchor_after_visible_ancestors() {
        assert_eq!(
            sticky_anchor(&[3, 3, 1], 8),
            Some(StickyAnchor {
                index: 10,
                drifting: false
            })
        );
    }

    #[test]
    fn marks_last_parent_as_drifting_at_an_outdent() {
        assert_eq!(
            sticky_anchor(&[1, 2, 1], 4),
            Some(StickyAnchor {
                index: 5,
                drifting: true
            })
        );
    }

    #[test]
    fn omits_sticky_anchor_at_the_top_level() {
        assert_eq!(sticky_anchor(&[0, 1, 2], 0), None);
    }

    #[test]
    fn sticky_rows_stack_and_the_deepest_leaves_whole_at_an_outdent() {
        // Rows of 20 points, three ancestors of row 5 stuck above it.
        let depths = [0, 1, 2, 3, 3, 3, 2, 1, 0, 0, 0, 0];
        let shown = |offset: f32| {
            let first = (offset / 20.0) as usize;
            let anchor = sticky_anchor(&depths[first..first + 5], first).unwrap();
            // Each level of nesting is one parent.
            let stuck = depths[anchor.index];
            let leaves = anchor.drifting && drift(anchor.index, stuck, 20.0, offset) < 0.0;
            (anchor.index, stuck - usize::from(leaves))
        };
        assert_eq!(shown(60.0), (5, 3));
        assert_eq!(shown(61.0), (5, 2));
    }

    #[test]
    fn scrolling_by_hand_keeps_following_while_the_row_partly_shows() {
        let (top, bottom) = (56.0, 84.0);
        assert!(row_is_visible(Some(2), bottom - 1.0, 28.0, 28.0));
        assert!(!row_is_visible(Some(2), bottom, 28.0, 28.0));
        assert!(row_is_visible(Some(2), top - 28.0 + 1.0, 28.0, 28.0));
        assert!(!row_is_visible(Some(2), top - 28.0, 28.0, 28.0));
        assert!(!row_is_visible(None, 0.0, 28.0, 28.0));
    }

    #[test]
    fn reveals_centre_below_the_sticky_rows() {
        // Rows of 10 in a 100-point list scrolled to 0.
        assert_eq!(reveal_offset(3, 0, 10.0, 100.0, 0.0, false), None);
        assert_eq!(reveal_offset(3, 0, 10.0, 100.0, 0.0, true), Some(0.0));
        // Row 50 is centred: its top 45 points down.
        assert_eq!(reveal_offset(50, 0, 10.0, 100.0, 0.0, false), Some(455.0));
        // Under two sticky rows, row 50 centres in the 80 points left.
        assert_eq!(reveal_offset(50, 2, 10.0, 100.0, 0.0, false), Some(445.0));
        // A row hidden under the sticky rows is revealed.
        assert_eq!(reveal_offset(41, 2, 10.0, 100.0, 400.0, false), Some(355.0));
        assert_eq!(reveal_offset(42, 2, 10.0, 100.0, 400.0, false), None);
    }

    #[test]
    fn sidebar_tabs_parse_by_name() {
        assert_eq!(SidebarTab::parse("pages"), Some(SidebarTab::Pages));
        assert_eq!(SidebarTab::parse("Search"), Some(SidebarTab::Search));
        assert_eq!(SidebarTab::parse("nope"), None);
    }
}
