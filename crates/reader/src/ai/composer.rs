//! The composer, after Zed's message editor: the text with its chips, a
//! toolbar under it (`+` for context, a suggestion, the context used, the
//! model or agent, and one round button that sends, queues or stops), the
//! messages queued while an answer runs, and the agent's changes stacked
//! on top as one sheet.

use egui::{Align2, CornerRadius, Rect, Sense, Stroke, StrokeKind, vec2};
use glyphst_agent_host::external::AgentSpec;
use glyphst_settings::{ModelLimits, ModelRef};

use super::{Assistant, PanelAction};
use crate::{
    ai_changes::{ChangeAction, ChangeLog},
    ai_editor::{ChipKind, EditorStyle},
    ai_mentions::{self, Mention, Sources},
    ai_menu::{MenuAction, MenuEntry, Slash},
    theme::{self, Tokens},
};

const SEND: f32 = 28.0;
/// The editor's font.
pub(super) const TEXT_SIZE: f32 = 13.5;

/// What the composer shows besides the draft.
pub(super) struct Composer<'a> {
    pub document: Option<&'a str>,
    pub page: Option<usize>,
    pub selection_words: Option<usize>,
    pub models: &'a [(ModelRef, String)],
    pub model: Option<ModelRef>,
    pub agents: &'a [AgentSpec],
}

impl Assistant {
    /// The composer at the bottom of the panel (or in the middle of an
    /// empty chat).
    pub(super) fn composer_ui(
        &mut self,
        ui: &mut egui::Ui,
        composer: &Composer<'_>,
        changes: &mut ChangeLog,
        sources: &mut dyn FnMut() -> Sources,
    ) -> Option<PanelAction> {
        let tokens = Tokens::get(ui.ctx());
        let ctx = ui.ctx().clone();
        let mut action = None;
        self.poll_paste(&ctx);
        let focused = ctx.memory(|memory| memory.has_focus(self.editor.id));
        if focused
            && ui.input(|i| {
                i.events.iter().any(|event| {
                    matches!(event, egui::Event::Key { key: egui::Key::V, pressed: true, modifiers, .. } if modifiers.command)
                })
            })
        {
            self.start_paste(&ctx);
        }
        self.editor.track_input_method(ui);

        // The menu takes its keys before the editor sees them.
        let (query, entries) = self.menu.entries(&ctx, &self.editor, sources);
        let menu_open = !entries.is_empty() || (self.menu.forced && query.is_none());
        let mut accepted = None;
        if menu_open && !self.editor.composing {
            let (chosen, close) = self.menu.keys(ui, entries.len());
            accepted = chosen;
            if close {
                self.menu.forced = false;
                if let Some(range) = &query {
                    // Esc keeps the typed text but ends the query.
                    self.editor.replace_text(&ctx, range.end..range.end, " ");
                }
            }
        } else {
            self.menu.selected = 0;
        }
        let mut send = false;
        if focused && !self.editor.composing && !menu_open {
            send = ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter));
            if self.editor.is_empty()
                && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp))
                && let Some(draft) = self.queue.pop()
            {
                self.editor.set(&ctx, draft);
            }
            if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
                && self.run.is_some()
            {
                self.stop();
            }
        }
        if focused
            && ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Semicolon))
        {
            self.menu.forced = !self.menu.forced;
        }

        let width = ui.available_width();
        // The agent's changes and the queue, stacked on the card.
        let sheet = !self.queue.is_empty() || changes.has_changes();
        if sheet {
            egui::Frame::new()
                .fill(tokens.ink(0.03))
                .stroke(Stroke::new(1.0, tokens.hairline(0.1)))
                .corner_radius(CornerRadius {
                    nw: 12,
                    ne: 12,
                    sw: 0,
                    se: 0,
                })
                .inner_margin(egui::Margin {
                    left: 10,
                    right: 8,
                    top: 6,
                    bottom: 18,
                })
                .show(ui, |ui| {
                    ui.set_width(width - 36.0);
                    if let Some(change) = changes.ui(ui, &tokens) {
                        action = Some(PanelAction::Change(change));
                    }
                    if let Some(chosen) = self.queue_ui(ui, &tokens) {
                        action = Some(chosen);
                    }
                });
            ui.add_space(-14.0);
        }
        let card = egui::Frame::new()
            .fill(tokens.field)
            .stroke(Stroke::new(
                1.0,
                tokens.hairline(if focused { 0.22 } else { 0.14 }),
            ))
            .corner_radius(CornerRadius::same(14))
            .shadow(egui::Shadow {
                offset: [0, 2],
                blur: 10,
                spread: 0,
                color: egui::Color32::from_black_alpha(if tokens.dark() { 60 } else { 14 }),
            })
            .inner_margin(egui::Margin {
                left: 12,
                right: 8,
                top: 10,
                bottom: 8,
            })
            .show(ui, |ui| {
                ui.set_width(width - 22.0);
                let hint = match self.agent.as_deref() {
                    Some(id) => {
                        let name = composer
                            .agents
                            .iter()
                            .find(|agent| agent.id() == id)
                            .map_or(id, |agent| agent.name.as_str());
                        format!("Message {name} — @ for context")
                    }
                    None => "Ask anything — @ for context, / for commands".into(),
                };
                let max_height = if self.expanded {
                    (ui.ctx().content_rect().height() * 0.55).max(200.0)
                } else {
                    TEXT_SIZE * 1.45 * 10.0
                };
                let editor_top = ui.cursor().top();
                let room = if self.messages.is_empty() { 0.0 } else { 24.0 };
                let output = ui
                    .allocate_ui(vec2(ui.available_width() - room, 0.0), |ui| {
                        self.editor.show(
                            ui,
                            &EditorStyle {
                                hint: &hint,
                                rows: if self.expanded { 14 } else { 2 },
                                max_height,
                                font: theme::regular(TEXT_SIZE),
                            },
                        )
                    })
                    .inner;
                let hovered = output.hovered.or_else(|| {
                    self.demo_hover
                        .then(|| output.plates.first().copied())
                        .flatten()
                });
                if let Some((index, rect)) = hovered
                    && let Some(chip) = self.editor.chips().get(index)
                {
                    self.previews.hovered(&chip.kind, rect);
                }
                // Expanding, once the chat has messages, as Zed's.
                if !self.messages.is_empty() {
                    let at = Rect::from_min_size(
                        egui::pos2(ui.max_rect().right() - 22.0, editor_top - 4.0),
                        vec2(22.0, 22.0),
                    );
                    let hovered = ui.rect_contains_pointer(at);
                    let icon = if self.expanded {
                        "minimize-2"
                    } else {
                        "maximize-2"
                    };
                    let response = ui.put(at, |ui: &mut egui::Ui| {
                        ui.set_opacity(if hovered { 1.0 } else { 0.5 });
                        crate::icons::button(
                            ui,
                            icon,
                            22.0,
                            false,
                            if self.expanded { "Shrink" } else { "Expand" },
                        )
                    });
                    if response.clicked() {
                        self.expanded = !self.expanded;
                    }
                }
                ui.add_space(6.0);
                let toolbar = self.toolbar(ui, composer, &mut send, sources);
                if toolbar.is_some() {
                    action = toolbar;
                }
            });
        let card_rect = card.response.rect;

        if menu_open && !self.editor.composing {
            let typed = query
                .as_ref()
                .map(|range| self.editor.text()[range.start + 1..range.end].to_string())
                .unwrap_or_default();
            if let Some(clicked) =
                self.menu
                    .show(ui, card_rect, &entries, &typed, &mut self.previews)
            {
                accepted = Some(clicked);
            }
        }
        if let Some(index) = accepted
            && let Some(entry) = entries.into_iter().nth(index)
            && let Some(chosen) = self.accept(&ctx, query, entry)
        {
            action = Some(chosen);
        }
        if send && !self.editor.is_empty() {
            action = Some(PanelAction::Send(self.editor.take()));
            self.expanded = false;
        }
        action
    }

    /// Does what a menu entry stands for.
    fn accept(
        &mut self,
        ctx: &egui::Context,
        query: Option<std::ops::Range<usize>>,
        entry: MenuEntry,
    ) -> Option<PanelAction> {
        self.menu.forced = false;
        self.menu.selected = 0;
        ctx.memory_mut(|memory| memory.request_focus(self.editor.id));
        let clear = |assistant: &mut Self| {
            if let Some(range) = &query {
                assistant.editor.replace_text(ctx, range.clone(), "");
            }
        };
        match entry {
            MenuEntry::Mention(candidate) => {
                self.editor
                    .insert_chip(ctx, query, ChipKind::Mention(candidate.mention));
                None
            }
            MenuEntry::Mode { keyword, .. } => {
                let range = query.unwrap_or_else(|| {
                    let caret = self.editor.caret(ctx);
                    caret..caret
                });
                self.editor.replace_text(ctx, range, &format!("@{keyword}"));
                None
            }
            MenuEntry::Slash(slash) => {
                clear(self);
                match slash {
                    Slash::New => Some(PanelAction::NewSession),
                    Slash::Compact => {
                        self.compact_next = !self.messages.is_empty();
                        self.status =
                            Some("The next message starts by summarizing the chat so far.".into());
                        None
                    }
                    Slash::Undo => Some(PanelAction::Change(ChangeAction::UndoAll)),
                    Slash::Model => {
                        self.selector_open = true;
                        None
                    }
                }
            }
            MenuEntry::Action(MenuAction::PictureFile) => {
                clear(self);
                Some(PanelAction::ChoosePicture)
            }
            MenuEntry::Action(MenuAction::Region) => {
                clear(self);
                Some(PanelAction::SelectRegion)
            }
        }
    }

    /// The row under the text.
    fn toolbar(
        &mut self,
        ui: &mut egui::Ui,
        composer: &Composer<'_>,
        send: &mut bool,
        sources: &mut dyn FnMut() -> Sources,
    ) -> Option<PanelAction> {
        let tokens = Tokens::get(ui.ctx());
        let ctx = ui.ctx().clone();
        let mut action = None;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            if crate::icons::button(ui, "plus", 26.0, self.menu.forced, "Add context (Ctrl+;)")
                .clicked()
            {
                self.menu.forced = !self.menu.forced;
                ctx.memory_mut(|memory| memory.request_focus(self.editor.id));
            }
            if let Some((label, title)) = self.suggestion(composer)
                && suggestion_chip(ui, &label, &tokens).clicked()
                && let Some(candidate) = ai_mentions::candidates(&sources(), "")
                    .into_iter()
                    .find(|candidate| candidate.title == title)
            {
                self.editor
                    .insert_chip(&ctx, None, ChipKind::Mention(candidate.mention));
                ctx.memory_mut(|memory| memory.request_focus(self.editor.id));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                let running = self.run.is_some();
                let empty = self.editor.is_empty();
                let (icon, tooltip) = match (running, empty) {
                    (true, true) => ("square", "Stop (Esc)"),
                    (true, false) => ("arrow-up", "Queue: send when this answer ends"),
                    (false, _) => ("arrow-up", "Send (Enter)"),
                };
                let enabled = running || !empty;
                let button = round_button(ui, icon, enabled, &tokens).on_hover_text(tooltip);
                if button.clicked() && enabled {
                    if running && empty {
                        self.stop();
                    } else {
                        *send = true;
                    }
                }
                if let Some(chosen) = self.selector(ui, composer) {
                    action = Some(chosen);
                }
                self.context_ring(ui, &tokens);
            });
        });
        action
    }

    /// What to suggest attaching: (chip text, menu title).
    fn suggestion(&self, composer: &Composer<'_>) -> Option<(String, &'static str)> {
        let has = |test: &dyn Fn(&Mention) -> bool| {
            self.editor.chips().iter().any(|chip| match &chip.kind {
                ChipKind::Mention(mention) => test(mention),
                ChipKind::Image(_) => false,
            })
        };
        if let Some(words) = composer.selection_words
            && !has(&|mention| {
                matches!(
                    mention,
                    Mention::Text {
                        kind: "selection",
                        ..
                    }
                )
            })
        {
            return Some((
                format!(
                    "Selection · {words} word{}",
                    if words == 1 { "" } else { "s" }
                ),
                "Selection",
            ));
        }
        let page = composer.page?;
        (!has(&|mention| {
            matches!(mention, Mention::Pages { first, last, .. } if *first <= page && page <= *last)
        }))
        .then(|| (format!("Page {}", page + 1), "This page"))
    }

    /// How much of the model's context the chat uses, as a ring.
    fn context_ring(&self, ui: &mut egui::Ui, tokens: &Tokens) {
        let (used, model) = &self.context_tokens;
        if *used == 0 || self.agent.is_some() {
            return;
        }
        let window = ModelLimits::for_model(model).context_window.max(1);
        let fraction = *used as f32 / window as f32;
        let (rect, response) = ui.allocate_exact_size(vec2(20.0, 20.0), Sense::hover());
        let color = if fraction >= 0.85 {
            tokens.warning
        } else {
            tokens.text_muted
        };
        crate::widgets::progress_ring(
            ui.painter(),
            rect.center(),
            6.5,
            fraction,
            color,
            tokens.hairline(0.26),
        );
        response.on_hover_text(format!(
            "{:.0}% of the context used ({} of {} tokens). Long chats are summarized when it fills.",
            fraction * 100.0,
            short_count(*used),
            short_count(window)
        ));
    }

    /// The model or agent that answers, and its menu.
    fn selector(&mut self, ui: &mut egui::Ui, composer: &Composer<'_>) -> Option<PanelAction> {
        let tokens = Tokens::get(ui.ctx());
        let mut action = None;
        let current = match self.agent.as_deref() {
            Some(id) => composer
                .agents
                .iter()
                .find(|agent| agent.id() == id)
                .map_or_else(|| id.to_string(), |agent| agent.name.clone()),
            None => composer
                .model
                .as_ref()
                .and_then(|chosen| composer.models.iter().find(|(model, _)| model == chosen))
                .map(|(_, label)| model_name(label).to_string())
                .unwrap_or_else(|| "Choose a model".into()),
        };
        let font = theme::regular(12.5);
        let galley = ui.painter().layout_no_wrap(
            current,
            font,
            if self.selector_open {
                tokens.accent_text
            } else {
                tokens.text_muted
            },
        );
        let width = (galley.size().x + 40.0).min(ui.available_width().max(60.0));
        let (rect, response) = ui.allocate_exact_size(vec2(width, 26.0), Sense::click());
        if response.hovered() || self.selector_open {
            ui.painter()
                .rect_filled(rect, CornerRadius::same(7), tokens.ink(0.06));
        }
        let icon = if self.agent.is_some() {
            "terminal"
        } else {
            "sparkles"
        };
        crate::icons::paint(
            ui,
            Rect::from_center_size(rect.left_center() + vec2(12.0, 0.0), vec2(13.0, 13.0)),
            icon,
            13.0,
            tokens.text_muted,
        );
        let text_rect = Rect::from_min_max(
            rect.left_top() + vec2(22.0, 0.0),
            rect.right_bottom() - vec2(16.0, 0.0),
        );
        ui.painter().with_clip_rect(text_rect).galley(
            egui::pos2(text_rect.left(), rect.center().y - galley.size().y / 2.0),
            galley,
            tokens.text_muted,
        );
        crate::widgets::chevron(
            ui.painter(),
            rect.right_center() - vec2(9.0, 0.0),
            8.0,
            if self.selector_open { 3.0 } else { 1.0 },
            tokens.text_faint,
        );
        let response = response.on_hover_text("Model or coding agent");
        if response.clicked() {
            self.selector_open = !self.selector_open;
        }
        if !self.selector_open {
            return None;
        }
        let area = egui::Area::new(egui::Id::new("assistant-selector"))
            .order(egui::Order::Foreground)
            .pivot(Align2::RIGHT_BOTTOM)
            .fixed_pos(rect.right_top() + vec2(0.0, -6.0))
            .show(ui.ctx(), |ui| {
                egui::Frame::new()
                    .fill(tokens.card)
                    .stroke(Stroke::new(1.0, tokens.hairline(0.14)))
                    .corner_radius(CornerRadius::same(12))
                    .shadow(tokens.popup_shadow())
                    .inner_margin(egui::Margin::same(5))
                    .show(ui, |ui| {
                        ui.set_width(280.0);
                        ui.spacing_mut().item_spacing.y = 0.0;
                        egui::ScrollArea::vertical()
                            .id_salt("assistant-selector-scroll")
                            .max_height(360.0)
                            .show(ui, |ui| {
                                let mut provider = "";
                                for (model, label) in composer.models {
                                    let (group, name) = label.split_once(" · ").unwrap_or(("Models", label));
                                    if group != provider {
                                        provider = group;
                                        group_title(ui, group, &tokens);
                                    }
                                    let selected = self.agent.is_none() && composer.model.as_ref() == Some(model);
                                    if selector_row(ui, "sparkles", name, "", selected, &tokens).clicked() {
                                        if !selected {
                                            action = Some(PanelAction::SetModel(model.clone()));
                                        }
                                        self.selector_open = false;
                                    }
                                }
                                if composer.models.is_empty() {
                                    group_title(ui, "Built-in", &tokens);
                                    if selector_row(ui, "settings", "Set up a model…", "Settings", false, &tokens).clicked() {
                                        action = Some(PanelAction::OpenAiSettings);
                                        self.selector_open = false;
                                    }
                                }
                                if !composer.agents.is_empty() {
                                    group_title(ui, "Coding agents", &tokens);
                                    for agent in composer.agents {
                                        let id = agent.id();
                                        let selected = self.agent.as_deref() == Some(id.as_str());
                                        let icon = if id.starts_with("acp:") { "bot" } else { "terminal" };
                                        if selector_row(ui, icon, &agent.name, "new chat", selected, &tokens)
                                            .on_hover_text("A coding agent on this computer, with its own model and sign-in. It works in a folder for this document and reaches the Reader through MCP.")
                                            .clicked()
                                        {
                                            if !selected {
                                                action = Some(PanelAction::SetAgent(Some(id)));
                                            }
                                            self.selector_open = false;
                                        }
                                    }
                                }
                            });
                    });
            });
        if area.response.clicked_elsewhere() && !response.clicked() && !response.hovered() {
            self.selector_open = false;
        }
        action
    }

    /// The messages waiting for the answer to end.
    fn queue_ui(&mut self, ui: &mut egui::Ui, tokens: &Tokens) -> Option<PanelAction> {
        let mut remove = None;
        let mut edit = None;
        for (index, draft) in self.queue.iter().enumerate() {
            let (rect, response) =
                ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::hover());
            crate::icons::paint(
                ui,
                Rect::from_center_size(rect.left_center() + vec2(7.0, 0.0), vec2(12.0, 12.0)),
                "corner-down-left",
                12.0,
                if index == 0 {
                    tokens.accent
                } else {
                    tokens.text_faint
                },
            );
            let (text, _) = draft.prompt();
            crate::widgets::row_text(
                ui,
                rect.left_center() + vec2(20.0, 0.0),
                format!("Queued · {}", text.replace('\n', " ")),
                theme::regular(12.0),
                tokens.text_muted,
                rect.width() - 80.0,
            );
            if response.contains_pointer() {
                let at = |offset: f32| {
                    Rect::from_center_size(
                        rect.right_center() - vec2(offset, 0.0),
                        vec2(22.0, 22.0),
                    )
                };
                if ui
                    .put(at(12.0), |ui: &mut egui::Ui| {
                        crate::icons::button(ui, "x", 22.0, false, "Remove")
                    })
                    .clicked()
                {
                    remove = Some(index);
                }
                if ui
                    .put(at(36.0), |ui: &mut egui::Ui| {
                        crate::icons::button(ui, "pencil", 22.0, false, "Edit")
                    })
                    .clicked()
                {
                    edit = Some(index);
                }
            }
        }
        if let Some(index) = remove {
            self.queue.remove(index);
        }
        if let Some(index) = edit {
            let draft = self.queue.remove(index);
            let ctx = ui.ctx().clone();
            self.compose(&ctx, draft);
        }
        None
    }
}

/// The send/queue/stop button: a filled circle in the text colour.
fn round_button(ui: &mut egui::Ui, icon: &str, enabled: bool, tokens: &Tokens) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(SEND, SEND), Sense::click());
    let fill = if response.hovered() && enabled {
        Tokens::fade(tokens.text, 0.85)
    } else {
        tokens.text
    };
    let alpha = if enabled { 1.0 } else { 0.3 };
    ui.painter()
        .circle_filled(rect.center(), SEND / 2.0, Tokens::fade(fill, alpha));
    if icon == "square" {
        ui.painter().rect_filled(
            Rect::from_center_size(rect.center(), vec2(10.0, 10.0)),
            CornerRadius::same(2),
            tokens.field,
        );
    } else {
        crate::icons::paint(ui, rect, icon, 15.0, tokens.field);
    }
    response
}

/// A faint pill offering context to attach.
fn suggestion_chip(ui: &mut egui::Ui, label: &str, tokens: &Tokens) -> egui::Response {
    let font = theme::regular(12.0);
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_string(), font, tokens.text_muted);
    let width = (galley.size().x + 30.0)
        .min(ui.available_width() - 120.0)
        .max(40.0);
    let (rect, response) = ui.allocate_exact_size(vec2(width, 24.0), Sense::click());
    let fill = if response.hovered() {
        tokens.ink(0.07)
    } else {
        egui::Color32::TRANSPARENT
    };
    ui.painter().rect(
        rect,
        CornerRadius::same(12),
        fill,
        Stroke::new(1.0, tokens.hairline(0.16)),
        StrokeKind::Inside,
    );
    crate::icons::paint(
        ui,
        Rect::from_center_size(rect.left_center() + vec2(12.0, 0.0), vec2(11.0, 11.0)),
        "plus",
        11.0,
        tokens.text_muted,
    );
    let text = Rect::from_min_max(
        rect.left_top() + vec2(22.0, 0.0),
        rect.right_bottom() - vec2(8.0, 0.0),
    );
    ui.painter().with_clip_rect(text).galley(
        egui::pos2(text.left(), rect.center().y - galley.size().y / 2.0),
        galley,
        tokens.text_muted,
    );
    response.on_hover_text("Attach it to the message")
}

fn group_title(ui: &mut egui::Ui, title: &str, tokens: &Tokens) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 24.0), Sense::hover());
    ui.painter().text(
        rect.left_bottom() + vec2(10.0, -6.0),
        Align2::LEFT_BOTTOM,
        title,
        theme::medium(11.0),
        tokens.text_faint,
    );
}

fn selector_row(
    ui: &mut egui::Ui,
    icon: &str,
    label: &str,
    detail: &str,
    selected: bool,
    tokens: &Tokens,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::click());
    if response.hovered() {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(7), tokens.ink(0.07));
    }
    crate::icons::paint(
        ui,
        Rect::from_center_size(rect.left_center() + vec2(16.0, 0.0), vec2(14.0, 14.0)),
        icon,
        14.0,
        if selected {
            tokens.accent
        } else {
            tokens.text_muted
        },
    );
    crate::widgets::row_text(
        ui,
        rect.left_center() + vec2(32.0, 0.0),
        label,
        theme::regular(13.0),
        tokens.text,
        rect.width() - 70.0,
    );
    if selected {
        crate::icons::paint(
            ui,
            Rect::from_center_size(rect.right_center() - vec2(16.0, 0.0), vec2(14.0, 14.0)),
            "check",
            14.0,
            tokens.accent,
        );
    } else if !detail.is_empty() && response.hovered() {
        ui.painter().text(
            rect.right_center() - vec2(10.0, 0.0),
            Align2::RIGHT_CENTER,
            detail,
            theme::regular(11.0),
            tokens.text_faint,
        );
    }
    response
}

/// The model part of a "Provider · Model" label.
fn model_name(label: &str) -> &str {
    label.split_once(" · ").map_or(label, |(_, model)| model)
}

/// "950", "12k", "1.2M".
fn short_count(count: u64) -> String {
    match count {
        0..1_000 => count.to_string(),
        1_000..1_000_000 => format!("{}k", count / 1_000),
        _ => format!("{:.1}M", count as f64 / 1_000_000.0),
    }
}
