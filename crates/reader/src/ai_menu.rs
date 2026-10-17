//! The composer's menu, after Zed's completion menu: `@` mentions grouped
//! by where they come from (with the letters typed in bold), the words that
//! narrow it to one group, `/` commands and the `+` actions; beside it a
//! preview of the highlighted entry, as Zed's documentation aside.

use std::ops::Range;

use egui::{Align2, Rect, Sense, text::LayoutJob, vec2};

use crate::{
    ai_editor::{ChipKind, MentionEditor},
    ai_mentions::{self, Candidate, Group, Sources},
    ai_preview::{CARD_WIDTH, Previews, TEXT_HEIGHT},
    theme::{self, Tokens},
};

const ROW: f32 = 30.0;
const HEADER: f32 = 24.0;
/// Rows the menu shows before it scrolls.
const VISIBLE_ROWS: f32 = 11.0;

/// The composer's slash commands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Slash {
    New,
    Compact,
    Undo,
    Model,
}

impl Slash {
    pub const ALL: [Self; 4] = [Self::New, Self::Compact, Self::Undo, Self::Model];

    pub fn name(self) -> &'static str {
        match self {
            Self::New => "/new",
            Self::Compact => "/compact",
            Self::Undo => "/undo",
            Self::Model => "/model",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::New => "Start a new chat. This one stays in the history.",
            Self::Compact => {
                "Summarize the chat so far before the next message, to make room in the model's context."
            }
            Self::Undo => {
                "Undo every change agents made: highlights, notes, bookmarks, library edits and files."
            }
            Self::Model => "Choose the model or the coding agent that answers.",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Self::New => "message-square-plus",
            Self::Compact => "list",
            Self::Undo => "undo-2",
            Self::Model => "sparkles",
        }
    }
}

/// What the `+` menu does besides mentions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuAction {
    /// Choose a picture file to attach.
    PictureFile,
    /// Draw a region of a page to attach.
    Region,
}

/// An entry of the menu.
pub enum MenuEntry {
    Mention(Box<Candidate>),
    /// A word that narrows the menu to one group (`@section `).
    Mode {
        keyword: &'static str,
        title: &'static str,
        icon: &'static str,
        detail: &'static str,
    },
    Slash(Slash),
    Action(MenuAction),
}

impl MenuEntry {
    fn title(&self) -> String {
        match self {
            Self::Mention(candidate) => candidate.title.clone(),
            Self::Mode { title, .. } => (*title).to_string(),
            Self::Slash(slash) => slash.name().to_string(),
            Self::Action(MenuAction::PictureFile) => "Picture file…".into(),
            Self::Action(MenuAction::Region) => "Region of a page".into(),
        }
    }

    fn detail(&self) -> String {
        match self {
            Self::Mention(candidate) => candidate.detail.clone(),
            Self::Mode { detail, .. } => (*detail).to_string(),
            Self::Slash(_) => String::new(),
            Self::Action(MenuAction::PictureFile) => "PNG, JPEG, WebP".into(),
            Self::Action(MenuAction::Region) => "Draw it".into(),
        }
    }

    fn icon(&self) -> &'static str {
        match self {
            Self::Mention(candidate) => candidate.mention.icon(),
            Self::Mode { icon, .. } => icon,
            Self::Slash(slash) => slash.icon(),
            Self::Action(MenuAction::PictureFile) => "paperclip",
            Self::Action(MenuAction::Region) => "scan",
        }
    }

    fn group(&self) -> &'static str {
        match self {
            Self::Mention(candidate) => candidate.group.title(),
            Self::Mode { .. } => "Add context",
            Self::Slash(_) => "Commands",
            Self::Action(_) => "Attach",
        }
    }
}

/// The modes offered when nothing is typed after `@`.
const MODES: [(&str, &str, &str, &str); 5] = [
    ("page ", "Pages…", "file-text", "@page 12, @pages 3-5"),
    ("section ", "Section…", "list", "From the outline"),
    (
        "note ",
        "Annotation…",
        "highlighter",
        "Highlights and notes",
    ),
    ("doc ", "Open document…", "book-open", "Another tab"),
    ("book ", "Library book…", "library", "Its details"),
];

/// The menu's state.
#[derive(Default)]
pub struct Menu {
    pub selected: usize,
    /// `+` opened it, without an `@`.
    pub forced: bool,
    /// Where the pointer was, to move the highlight only when it moves.
    pointer: Option<egui::Pos2>,
}

impl Menu {
    /// The entries for what is typed before the caret, with the typed
    /// range they replace.
    pub fn entries(
        &self,
        ctx: &egui::Context,
        editor: &MentionEditor,
        sources: &mut dyn FnMut() -> Sources,
    ) -> (Option<Range<usize>>, Vec<MenuEntry>) {
        let text = editor.text();
        if text.starts_with('/') && !text.contains(char::is_whitespace) && editor.chips().is_empty()
        {
            let typed = &text[1..];
            let entries = Slash::ALL
                .into_iter()
                .filter(|slash| ai_mentions::match_positions(typed, &slash.name()[1..]).is_some())
                .map(MenuEntry::Slash)
                .collect();
            return (Some(0..text.len()), entries);
        }
        let range = editor.active_query(ctx);
        let query = match &range {
            Some(range) => text[range.start + 1..range.end].to_string(),
            None if self.forced => String::new(),
            None => return (None, Vec::new()),
        };
        let sources = sources();
        let mut entries: Vec<MenuEntry> = Vec::new();
        if query.trim().is_empty() {
            entries.extend(
                ai_mentions::candidates(&sources, "")
                    .into_iter()
                    .filter(|candidate| {
                        matches!(candidate.group, Group::ThisDocument | Group::Documents)
                    })
                    .map(|candidate| MenuEntry::Mention(Box::new(candidate))),
            );
            entries.extend(
                MODES
                    .iter()
                    .filter(|(keyword, ..)| {
                        // Only what there is.
                        match keyword.trim() {
                            "section" => !sources.outline.is_empty(),
                            "note" => !sources.annotations.is_empty(),
                            "doc" => sources.documents.len() > 1,
                            "book" => !sources.books.is_empty(),
                            _ => !sources.documents.is_empty(),
                        }
                    })
                    .map(|&(keyword, title, icon, detail)| MenuEntry::Mode {
                        keyword,
                        title,
                        icon,
                        detail,
                    }),
            );
            entries.push(MenuEntry::Action(MenuAction::PictureFile));
            if sources
                .documents
                .first()
                .is_some_and(|document| document.renders())
            {
                entries.push(MenuEntry::Action(MenuAction::Region));
            }
            // Documents already listed as tabs need no mode.
            entries.retain(|entry| {
                !matches!(
                    entry,
                    MenuEntry::Mode {
                        keyword: "doc ",
                        ..
                    }
                )
            });
        } else {
            entries.extend(
                ai_mentions::candidates(&sources, &query)
                    .into_iter()
                    .map(|candidate| MenuEntry::Mention(Box::new(candidate))),
            );
        }
        (range, entries)
    }

    /// Moves the highlight with the arrow keys or Ctrl+P and Ctrl+N;
    /// returns the entry Enter or Tab chose, and whether Esc closed the
    /// menu.
    pub fn keys(&mut self, ui: &egui::Ui, count: usize) -> (Option<usize>, bool) {
        if count == 0 {
            self.selected = 0;
            return (None, false);
        }
        self.selected = self.selected.min(count - 1);
        let key = |i: &mut egui::InputState, key| i.consume_key(egui::Modifiers::NONE, key);
        let (up, down, accept, close) = ui.input_mut(|i| {
            let (up, down) = glyphst_ui::row::take_moves(i);
            (
                up,
                down,
                key(i, egui::Key::Enter) || key(i, egui::Key::Tab),
                key(i, egui::Key::Escape),
            )
        });
        glyphst_ui::row::mark_list_open(ui.ctx());
        if up {
            self.selected = (self.selected + count - 1) % count;
        }
        if down {
            self.selected = (self.selected + 1) % count;
        }
        (accept.then_some(self.selected), close)
    }

    /// The menu above `anchor` (the composer), with the highlighted
    /// entry's preview beside it; returns the entry clicked.
    pub fn show(
        &mut self,
        ui: &egui::Ui,
        anchor: Rect,
        entries: &[MenuEntry],
        query: &str,
        previews: &mut Previews,
    ) -> Option<usize> {
        let tokens = Tokens::get(ui.ctx());
        let mut clicked = None;
        let width = anchor.width().clamp(260.0, 440.0);
        let headers = query.trim().is_empty();
        let pointer = ui.input(|i| i.pointer.hover_pos());
        let moved = pointer != self.pointer;
        self.pointer = pointer;
        let opened = ui.ctx().animate_bool_with_time(
            egui::Id::new("assistant-menu-open"),
            true,
            glyphst_ui::motion::MENU_IN,
        );
        let area = egui::Area::new(egui::Id::new("assistant-menu"))
            .order(egui::Order::Foreground)
            .pivot(Align2::LEFT_BOTTOM)
            .fixed_pos(anchor.left_top() + vec2(0.0, -6.0 + 6.0 * (1.0 - opened)))
            .show(ui.ctx(), |ui| {
                ui.set_opacity(opened);
                glyphst_ui::pane::popover(ui.ctx()).show(ui, |ui| {
                    ui.set_width(width - 10.0);
                    ui.spacing_mut().item_spacing.y = 0.0;
                    if entries.is_empty() {
                        ui.add_space(6.0);
                        ui.label(
                            egui::RichText::new("nothing matches")
                                .font(theme::mono(11.0))
                                .color(tokens.faint),
                        );
                        ui.add_space(6.0);
                        return;
                    }
                    egui::ScrollArea::vertical()
                        .id_salt("assistant-menu-scroll")
                        .max_height(ROW * VISIBLE_ROWS)
                        .show(ui, |ui| {
                            let mut group = "";
                            for (index, entry) in entries.iter().enumerate() {
                                if headers && entry.group() != group {
                                    group = entry.group();
                                    let (rect, _) = ui.allocate_exact_size(
                                        vec2(ui.available_width(), HEADER),
                                        Sense::hover(),
                                    );
                                    glyphst_ui::motif::paint_rule_title(
                                        ui,
                                        rect.shrink2(vec2(8.0, 0.0)),
                                        &group.to_lowercase(),
                                        None,
                                        true,
                                    );
                                }
                                let selected = index == self.selected;
                                let response = row(ui, entry, selected, query, headers, &tokens);
                                if selected && !moved {
                                    response.scroll_to_me(None);
                                }
                                if moved && response.hovered() {
                                    self.selected = index;
                                }
                                if response.clicked() {
                                    clicked = Some(index);
                                }
                            }
                        });
                    // How to drive it.
                    ui.add_space(2.0);
                    ui.painter().hline(
                        ui.max_rect().x_range(),
                        ui.cursor().top(),
                        tokens.hairline(),
                    );
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new("↑↓ move   ↵ insert   esc close")
                            .font(theme::mono(9.0))
                            .color(tokens.faint),
                    );
                });
            });
        let menu = area.response.rect;
        if let Some(entry) = entries.get(self.selected) {
            aside(ui.ctx(), menu, entry, previews, &tokens);
        }
        clicked
    }
}

/// One entry: its icon, its title with the typed letters in bold, and its
/// detail on the right.
fn row(
    ui: &mut egui::Ui,
    entry: &MenuEntry,
    selected: bool,
    query: &str,
    grouped: bool,
    tokens: &Tokens,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), ROW), Sense::click());
    if selected {
        ui.painter()
            .rect_filled(rect, tokens.ctl_radius(), tokens.accent_soft);
    }
    crate::icons::paint(
        ui,
        Rect::from_center_size(rect.left_center() + vec2(16.0, 0.0), vec2(13.0, 13.0)),
        entry.icon(),
        13.0,
        if selected {
            tokens.accent_text
        } else {
            tokens.faint
        },
    );
    let title = entry.title();
    let typed = query.rsplit(' ').next().unwrap_or(query);
    let matched = match entry {
        MenuEntry::Mention(_) => ai_mentions::match_positions(typed, &title).unwrap_or_default(),
        MenuEntry::Slash(_) => {
            ai_mentions::match_positions(typed.trim_start_matches('/'), &title[1..])
                .unwrap_or_default()
                .into_iter()
                .map(|index| index + 1)
                .collect()
        }
        _ => Vec::new(),
    };
    let mut job = LayoutJob::default();
    for (index, ch) in title.chars().enumerate() {
        let font = if matched.contains(&index) {
            theme::mono_strong(12.0)
        } else {
            theme::mono(12.0)
        };
        let color = if selected {
            tokens.accent_text
        } else {
            tokens.text_2
        };
        job.append(&ch.to_string(), 0.0, egui::TextFormat::simple(font, color));
    }
    let detail = if grouped || !matches!(entry, MenuEntry::Mention(_)) {
        entry.detail()
    } else {
        format!("{} · {}", entry.detail(), entry.group())
    };
    let detail_width = if detail.is_empty() {
        0.0
    } else {
        (rect.width() * 0.42).min(170.0)
    };
    job.wrap.max_width = rect.width() - 46.0 - detail_width;
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    job.wrap.overflow_character = Some('…');
    let galley = ui.painter().layout_job(job);
    ui.painter().galley(
        egui::pos2(rect.left() + 34.0, rect.center().y - galley.size().y / 2.0),
        galley,
        tokens.text,
    );
    if detail_width > 0.0 {
        let mut job =
            LayoutJob::simple_singleline(detail.to_lowercase(), theme::mono(10.0), tokens.faint);
        job.wrap.max_width = detail_width;
        job.wrap.max_rows = 1;
        job.wrap.break_anywhere = true;
        job.wrap.overflow_character = Some('…');
        let galley = ui.painter().layout_job(job);
        ui.painter().galley(
            egui::pos2(
                rect.right() - 10.0 - galley.size().x,
                rect.center().y - galley.size().y / 2.0,
            ),
            galley,
            tokens.faint,
        );
    }
    response
}

/// The preview of the highlighted entry, left of the menu when there is
/// room there, otherwise above it.
fn aside(
    ctx: &egui::Context,
    menu: Rect,
    entry: &MenuEntry,
    previews: &mut Previews,
    tokens: &Tokens,
) {
    let screen = ctx.content_rect();
    let width = CARD_WIDTH.min(screen.width() - 16.0);
    let (pivot, at) = if menu.left() - screen.left() >= width + 14.0 {
        (Align2::RIGHT_BOTTOM, menu.left_bottom() + vec2(-6.0, 0.0))
    } else if screen.right() - menu.right() >= width + 14.0 {
        (Align2::LEFT_BOTTOM, menu.right_bottom() + vec2(6.0, 0.0))
    } else {
        (Align2::LEFT_BOTTOM, menu.left_top() + vec2(0.0, -6.0))
    };
    egui::Area::new(egui::Id::new("assistant-menu-aside"))
        .order(egui::Order::Foreground)
        .pivot(pivot)
        .fixed_pos(at)
        .constrain(true)
        .interactable(false)
        .show(ctx, |ui| match entry {
            MenuEntry::Mention(candidate) => {
                previews.card_ui(
                    ui,
                    &ChipKind::Mention(candidate.mention.clone()),
                    width,
                    TEXT_HEIGHT,
                );
            }
            other => {
                let text = match other {
                    MenuEntry::Slash(slash) => slash.description().to_string(),
                    MenuEntry::Mode { keyword, .. } => format!(
                        "Type to find one, as @{keyword}…; Enter puts it in the message."
                    ),
                    MenuEntry::Action(MenuAction::PictureFile) => {
                        "Choose a picture to send with the message. You can also paste one or drop it here.".into()
                    }
                    MenuEntry::Action(MenuAction::Region) => {
                        "Draw a rectangle on the page; its picture goes with the message.".into()
                    }
                    MenuEntry::Mention(_) => String::new(),
                };
                glyphst_ui::pane::popover(ui.ctx())
                    .inner_margin(egui::Margin::symmetric(12, 10))
                    .show(ui, |ui| {
                        ui.set_max_width(width.min(260.0));
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(text)
                                    .font(theme::regular(12.0))
                                    .color(tokens.muted),
                            )
                            .wrap(),
                        );
                    });
            }
        });
}
