//! The command palette (Ctrl+K), after pdfcraft's `palette.rs`: commands
//! with their keys, open tabs, recent documents, settings sections and
//! "Go to page N", ranked by a substring-then-subsequence match.

use std::path::PathBuf;

use egui::{Align2, CornerRadius, Key, Rect, Sense, vec2};

use crate::{commands::Command, icons, settings_window::Section, theme::Tokens, widgets};

/// Results shown at most.
const MAX_HITS: usize = 12;
/// Commands remembered as recently run.
const MAX_USED: usize = 8;

#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    Command(Command),
    /// Index into the app's tabs.
    Tab(usize),
    Open(PathBuf),
    /// A page index (from 0).
    GoToPage(usize),
    Settings(Section),
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
        let Some(found) = score(command.label(), &needle)
            .or_else(|| score(command.category().label(), &needle).map(|score| score + 50))
        else {
            continue;
        };
        let recency = used
            .iter()
            .position(|used| used == command)
            .unwrap_or(MAX_USED);
        ranked.push((
            found,
            1 + recency,
            Hit {
                item: Item::Command(*command),
                icon: command.icon(),
                label: command.label().to_string(),
                detail: key.clone().unwrap_or_default(),
                blocked: *blocked,
            },
        ));
    }
    for (index, title) in sources.tabs.iter().enumerate() {
        if let Some(found) = score(title, &needle) {
            ranked.push((
                found,
                MAX_USED + 2,
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
                MAX_USED + 3,
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
                MAX_USED + 4,
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
    ranked.sort_by_key(|(score, kind, hit)| (*score, hit.blocked.is_some(), *kind));
    ranked.truncate(MAX_HITS);
    ranked.into_iter().map(|(_, _, hit)| hit).collect()
}

#[derive(Default)]
pub struct Palette {
    pub open: bool,
    pub query: String,
    highlight: usize,
    used: Vec<Command>,
    /// Where it was drawn last frame, to close on a click outside.
    rect: Option<Rect>,
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

    pub fn close(&mut self) {
        self.open = false;
        self.query.clear();
        self.rect = None;
    }

    pub fn used(&self) -> &[Command] {
        &self.used
    }

    /// Draws the palette over `hits`; returns the item chosen.
    pub fn show(&mut self, ctx: &egui::Context, hits: &[Hit]) -> Option<Item> {
        if !self.open {
            return None;
        }
        let tokens = Tokens::get(ctx);
        let (escape, up, down, outside) = ctx.input_mut(|input| {
            let outside = input.pointer.button_pressed(egui::PointerButton::Primary)
                && input
                    .pointer
                    .interact_pos()
                    .is_some_and(|pos| self.rect.is_some_and(|rect| !rect.contains(pos)));
            (
                input.consume_key(egui::Modifiers::NONE, Key::Escape),
                input.consume_key(egui::Modifiers::NONE, Key::ArrowUp),
                input.consume_key(egui::Modifiers::NONE, Key::ArrowDown),
                outside,
            )
        });
        if escape || outside {
            self.close();
            return None;
        }
        if down {
            self.highlight += 1;
        }
        if up {
            self.highlight = self.highlight.saturating_sub(1);
        }
        self.highlight = self.highlight.min(hits.len().saturating_sub(1));
        let screen = ctx.content_rect();
        let mut chosen = None;
        let previous_query = self.query.clone();
        let response = egui::Area::new(egui::Id::new("command-palette"))
            .order(egui::Order::Foreground)
            .pivot(Align2::CENTER_TOP)
            .fixed_pos(egui::pos2(screen.center().x, screen.top() + 72.0))
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style())
                    .fill(tokens.card)
                    .corner_radius(CornerRadius::same(12))
                    .inner_margin(egui::Margin::same(8))
                    .show(ui, |ui| {
                        ui.set_width((screen.width() - 32.0).clamp(240.0, 600.0));
                        let field_row = ui.horizontal(|ui| {
                            ui.add_space(6.0);
                            ui.add(icons::image("search", 16.0, tokens.text_muted));
                            ui.add(
                                egui::TextEdit::singleline(&mut self.query)
                                    .hint_text("Type a command, a page number or a document…")
                                    .font(crate::theme::regular(15.0))
                                    .frame(egui::Frame::NONE)
                                    .margin(egui::Margin::symmetric(4, 8))
                                    .desired_width(f32::INFINITY),
                            )
                        });
                        let field = field_row.inner;
                        if (field.has_focus() || field.lost_focus())
                            && ui.input(|i| i.key_pressed(Key::Enter))
                            && let Some(hit) = hits.get(self.highlight)
                        {
                            chosen = Some(hit.item.clone());
                        }
                        field.request_focus();
                        let line = ui.available_rect_before_wrap();
                        ui.painter().hline(
                            line.x_range(),
                            line.top(),
                            egui::Stroke::new(1.0, tokens.divider),
                        );
                        ui.add_space(6.0);
                        for (index, hit) in hits.iter().enumerate() {
                            let (rect, row) = ui.allocate_exact_size(
                                vec2(ui.available_width(), 34.0),
                                Sense::click(),
                            );
                            if row.hovered() && ui.input(|i| i.pointer.delta() != egui::Vec2::ZERO)
                            {
                                self.highlight = index;
                            }
                            let highlighted = index == self.highlight;
                            if highlighted {
                                ui.painter().rect_filled(
                                    rect,
                                    CornerRadius::same(8),
                                    tokens.accent_soft,
                                );
                            }
                            let blocked = hit.blocked.is_some();
                            let (color, icon_color) = if blocked {
                                (tokens.text_faint, tokens.text_faint)
                            } else if highlighted {
                                (tokens.text, tokens.accent_text)
                            } else {
                                (tokens.text, tokens.icon)
                            };
                            icons::paint(
                                ui,
                                egui::Rect::from_center_size(
                                    rect.left_center() + vec2(18.0, 0.0),
                                    vec2(16.0, 16.0),
                                ),
                                hit.icon,
                                16.0,
                                icon_color,
                            );
                            // The key chip or the reason at the right.
                            let mut right = rect.right() - 8.0;
                            if let Some(reason) = hit.blocked {
                                let galley = ui.painter().layout_no_wrap(
                                    reason.to_string(),
                                    crate::theme::regular(12.0),
                                    tokens.text_faint,
                                );
                                right -= galley.size().x;
                                ui.painter().galley(
                                    egui::pos2(right, rect.center().y - galley.size().y / 2.0),
                                    galley,
                                    tokens.text_faint,
                                );
                            } else if !hit.detail.is_empty() {
                                let is_key = matches!(hit.item, Item::Command(_));
                                if is_key {
                                    let size = widgets::kbd_size(ui, &hit.detail);
                                    right -= size.x;
                                    widgets::paint_kbd(
                                        ui,
                                        egui::Rect::from_min_size(
                                            egui::pos2(right, rect.center().y - size.y / 2.0),
                                            size,
                                        ),
                                        &hit.detail,
                                    );
                                } else {
                                    let galley = ui.painter().layout_no_wrap(
                                        hit.detail.clone(),
                                        crate::theme::regular(12.0),
                                        tokens.text_faint,
                                    );
                                    right -= galley.size().x;
                                    ui.painter().galley(
                                        egui::pos2(right, rect.center().y - galley.size().y / 2.0),
                                        galley,
                                        tokens.text_faint,
                                    );
                                }
                            }
                            let left = rect.left() + 36.0;
                            widgets::row_text(
                                ui,
                                egui::pos2(left, rect.center().y),
                                hit.label.as_str(),
                                crate::theme::regular(13.5),
                                color,
                                right - left - 12.0,
                            );
                            if row.clicked() {
                                chosen = Some(hit.item.clone());
                            }
                        }
                        if hits.is_empty() {
                            ui.add_space(4.0);
                            ui.label(
                                egui::RichText::new("Nothing matches").color(tokens.text_muted),
                            );
                            ui.add_space(4.0);
                        }
                    });
            });
        self.rect = Some(response.response.rect);
        if self.query != previous_query {
            self.highlight = 0;
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
        }
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
        assert_eq!(found[0].item, Item::Settings(Section::Shortcuts));
        // A subsequence still finds it.
        let found = hits("fdnx", &sources(true), &[]);
        assert_eq!(found[0].item, Item::Command(Command::FindNext));
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
}
