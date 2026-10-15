//! The composer, after Zed's message editor: the text with its chips, a
//! toolbar under it (`+` for context, a suggestion, the context used, the
//! model or agent, and one round button that sends, queues or stops), the
//! messages queued while an answer runs, and the agent's changes stacked
//! on top as one sheet.

use egui::{Align2, Rect, Sense, Stroke, vec2};
use glyphst_agent_host::external::AgentSpec;
use glyphst_settings::{ModelLimits, ModelRef};
use glyphst_ui::{Tip, badge::Chip, button::Button, menu as ui_menu, meter::Meter};

use super::{Assistant, PanelAction};
use crate::{
    ai_changes::{ChangeAction, ChangeLog},
    ai_editor::{ChipKind, EditorStyle},
    ai_mentions::{self, Mention, Sources},
    ai_menu::{MenuAction, MenuEntry, Slash},
    theme::{self, Tokens},
};

const SEND: f32 = 24.0;
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

        // The menu takes its keys before the editor sees them, and acts on
        // the text it was found in: the editor may change it this frame.
        let (query, entries) = self.menu.entries(&ctx, &self.editor, sources);
        let menu_open = !entries.is_empty() || (self.menu.forced && query.is_none());
        if menu_open && !self.editor.composing {
            let (chosen, close) = self.menu.keys(ui, entries.len());
            if let Some(index) = chosen
                && let Some(entry) = entries.into_iter().nth(index)
                && let Some(chosen) = self.accept(&ctx, query.clone(), entry)
            {
                action = Some(chosen);
            }
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
                .fill(tokens.raised)
                .stroke(tokens.hairline())
                .corner_radius(tokens.ctl_radius())
                .inner_margin(egui::Margin::symmetric(10, 6))
                .show(ui, |ui| {
                    ui.set_width(width - 22.0);
                    if let Some(change) = changes.ui(ui, &tokens) {
                        action = Some(PanelAction::Change(change));
                    }
                    if let Some(chosen) = self.queue_ui(ui, &tokens) {
                        action = Some(chosen);
                    }
                });
            ui.add_space(6.0);
        }
        let focused_edge = if focused {
            Stroke::new(1.0, tokens.accent)
        } else {
            tokens.edge()
        };
        let card = egui::Frame::new()
            .fill(tokens.raised)
            .stroke(focused_edge)
            .corner_radius(tokens.pop_radius())
            .inner_margin(egui::Margin {
                left: 10,
                right: 8,
                top: 9,
                bottom: 7,
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
                        format!("message {name} · @ to mention")
                    }
                    None => "ask anything · @ to mention".into(),
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
                            if self.expanded { "shrink" } else { "expand" },
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

        // The menu for the text as edited this frame.
        let (query, entries) = self.menu.entries(&ctx, &self.editor, sources);
        let menu_open = !entries.is_empty() || (self.menu.forced && query.is_none());
        if menu_open && !self.editor.composing {
            let typed = query
                .as_ref()
                .and_then(|range| self.editor.text().get(range.start + 1..range.end))
                .unwrap_or_default()
                .to_string();
            if let Some(clicked) =
                self.menu
                    .show(ui, card_rect, &entries, &typed, &mut self.previews)
                && let Some(entry) = entries.into_iter().nth(clicked)
                && let Some(chosen) = self.accept(&ctx, query, entry)
            {
                action = Some(chosen);
            }
        } else {
            self.menu.selected = 0;
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

    /// The row under the text: `+ / @` and a suggestion at the left; the
    /// context meter, the model and the send button at the right.
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
            ui.spacing_mut().item_spacing.x = 2.0;
            if crate::icons::button(ui, "plus", 22.0, self.menu.forced, "add context (ctrl ;)")
                .clicked()
            {
                self.menu.forced = !self.menu.forced;
                ctx.memory_mut(|memory| memory.request_focus(self.editor.id));
            }
            // `/` and `@` start a command or a mention in the text.
            for (glyph, tip) in [("/", "run a command"), ("@", "mention context")] {
                if ui.add(Button::plain(glyph).small()).tip(tip).clicked() {
                    let caret = self.editor.caret(&ctx);
                    self.editor.replace_text(&ctx, caret..caret, glyph);
                    ctx.memory_mut(|memory| memory.request_focus(self.editor.id));
                }
            }
            if let Some((label, title)) = self.suggestion(composer) {
                ui.add_space(4.0);
                let chip = Chip::new(&label).icon("plus").muted(true).show(ui);
                if chip.response.tip("attach it to the message").clicked()
                    && let Some(candidate) = ai_mentions::candidates(&sources(), "")
                        .into_iter()
                        .find(|candidate| candidate.title == title)
                {
                    self.editor
                        .insert_chip(&ctx, None, ChipKind::Mention(candidate.mention));
                    ctx.memory_mut(|memory| memory.request_focus(self.editor.id));
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                let running = self.run.is_some();
                let empty = self.editor.is_empty();
                let (icon, tooltip) = match (running, empty) {
                    (true, true) => ("square", "stop (esc)"),
                    (true, false) => ("arrow-up", "queue: send when this answer ends"),
                    (false, _) => ("arrow-up", "send (enter)"),
                };
                let enabled = running || !empty;
                let button = send_button(ui, icon, enabled, &tokens).tip(tooltip);
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
                self.context_meter(ui, &tokens);
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
                    "selection · {words} word{}",
                    if words == 1 { "" } else { "s" }
                ),
                "Selection",
            ));
        }
        let page = composer.page?;
        (!has(&|mention| {
            matches!(mention, Mention::Pages { first, last, .. } if *first <= page && page <= *last)
        }))
        .then(|| (format!("p.{}", page + 1), "This page"))
    }

    /// How much of the model's context the chat uses, as a meter.
    fn context_meter(&self, ui: &mut egui::Ui, tokens: &Tokens) {
        let (used, model) = &self.context_tokens;
        if *used == 0 || self.agent.is_some() {
            return;
        }
        let window = ModelLimits::for_model(model).context_window.max(1);
        let fraction = *used as f32 / window as f32;
        let color = if fraction >= 0.85 {
            tokens.warn
        } else {
            tokens.muted
        };
        let label = format!("{:.0}%", fraction * 100.0);
        let response = ui.add(
            Meter::new(fraction)
                .bar()
                .width(64.0)
                .color(color)
                .label(&label),
        );
        response.tip(&format!(
            "{:.0}% of the context used ({} of {} tokens). long chats are summarized when it fills.",
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
                .map(|(_, label)| model_name(label).to_lowercase())
                .unwrap_or_else(|| "choose a model".into()),
        };
        let font = theme::mono(11.0);
        let galley = ui.painter().layout_no_wrap(
            current,
            font,
            if self.selector_open {
                tokens.accent_text
            } else {
                tokens.muted
            },
        );
        let width = (galley.size().x + 22.0).min(ui.available_width().max(60.0));
        let (rect, response) = ui.allocate_exact_size(vec2(width, 22.0), Sense::click());
        if response.hovered() || self.selector_open {
            ui.painter()
                .rect_filled(rect, tokens.ctl_radius(), tokens.hover);
        }
        let text_rect = Rect::from_min_max(
            rect.left_top() + vec2(4.0, 0.0),
            rect.right_bottom() - vec2(14.0, 0.0),
        );
        ui.painter().with_clip_rect(text_rect).galley(
            egui::pos2(text_rect.left(), rect.center().y - galley.size().y / 2.0),
            galley,
            tokens.muted,
        );
        ui.painter().text(
            rect.right_center() - vec2(6.0, 0.0),
            Align2::RIGHT_CENTER,
            if self.selector_open { "▴" } else { "▾" },
            theme::mono(10.0),
            tokens.faint,
        );
        let response = response.tip("model or coding agent");
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
                glyphst_ui::pane::popover(ui.ctx())
                    .show(ui, |ui| {
                        ui.set_width(280.0);
                        ui_menu::style(ui);
                        egui::ScrollArea::vertical()
                            .id_salt("assistant-selector-scroll")
                            .max_height(360.0)
                            .show(ui, |ui| {
                                let mut provider = "";
                                for (model, label) in composer.models {
                                    let (group, name) = label.split_once(" · ").unwrap_or(("Models", label));
                                    if group != provider {
                                        provider = group;
                                        ui_menu::group(ui, &group.to_lowercase());
                                    }
                                    let selected = self.agent.is_none() && composer.model.as_ref() == Some(model);
                                    if ui_menu::Item::new(&name.to_lowercase()).icon("sparkles").checked(selected).show(ui).clicked() {
                                        if !selected {
                                            action = Some(PanelAction::SetModel(model.clone()));
                                        }
                                        self.selector_open = false;
                                    }
                                }
                                if composer.models.is_empty() {
                                    ui_menu::group(ui, "built-in");
                                    if ui_menu::Item::new("set up a model…").icon("settings").show(ui).clicked() {
                                        action = Some(PanelAction::OpenAiSettings);
                                        self.selector_open = false;
                                    }
                                }
                                if !composer.agents.is_empty() {
                                    ui_menu::group(ui, "coding agents");
                                    for agent in composer.agents {
                                        let id = agent.id();
                                        let selected = self.agent.as_deref() == Some(id.as_str());
                                        let icon = if id.starts_with("acp:") { "bot" } else { "terminal" };
                                        if ui_menu::Item::new(&agent.name.to_lowercase())
                                            .icon(icon)
                                            .checked(selected)
                                            .show(ui)
                                            .tip("a coding agent on this computer, with its own model and sign-in. it works in a folder for this document and reaches the reader through mcp.")
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
                    tokens.faint
                },
            );
            let (text, _) = draft.prompt();
            let label = ui.painter().text(
                rect.left_center() + vec2(20.0, 0.0),
                Align2::LEFT_CENTER,
                "queued",
                theme::mono(10.0),
                tokens.accent_text,
            );
            crate::widgets::row_text(
                ui,
                egui::pos2(label.right() + 8.0, rect.center().y),
                text.replace('\n', " "),
                theme::mono(11.0),
                tokens.text_2,
                rect.right() - label.right() - 64.0,
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
                        crate::icons::button(ui, "x", 22.0, false, "remove")
                    })
                    .clicked()
                {
                    remove = Some(index);
                }
                if ui
                    .put(at(36.0), |ui: &mut egui::Ui| {
                        crate::icons::button(ui, "pencil", 22.0, false, "edit")
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

/// The send, queue or stop button: an accent square with its glyph on
/// the accent.
fn send_button(ui: &mut egui::Ui, icon: &str, enabled: bool, tokens: &Tokens) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(SEND, SEND), Sense::click());
    let fill = if !enabled {
        tokens.hover
    } else if response.hovered() {
        tokens.accent_text
    } else {
        tokens.accent
    };
    ui.painter().rect_filled(rect, tokens.ctl_radius(), fill);
    let ink = if enabled {
        tokens.on_accent
    } else {
        tokens.faint
    };
    if icon == "square" {
        ui.painter().rect_filled(
            Rect::from_center_size(rect.center(), vec2(9.0, 9.0)),
            1,
            ink,
        );
    } else {
        crate::icons::paint(ui, rect, icon, 14.0, ink);
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
