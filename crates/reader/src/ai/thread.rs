//! The chat itself, after Lody's and Zeron's threads and ChatGPT's empty
//! state: the user's messages as soft bubbles on the right with their
//! chips, copy, edit and restore under them on hover; answers as Markdown
//! after a quiet timeline of the work (verbs that turn past tense, a
//! shimmer on what runs), folded into one line once the answer is done.

use std::time::Duration;

use egui::{Color32, CornerRadius, Rect, Sense, Stroke, vec2};

use super::{Assistant, Editing, PanelAction, Segment, ToolRow, ToolState, composer::Composer};
use crate::{
    ai_changes::ChangeLog,
    ai_editor::{ChipKind, Draft, EditorStyle, MentionEditor},
    ai_mentions::{self, Sources},
    ai_tools::summarize_steps,
    theme::{self, Tokens},
    widgets::motion,
};

/// The reserved lane under a message for its actions.
const LANE: f32 = 22.0;
/// A step of the work.
const STEP: f32 = 24.0;
/// The rail's distance from the left.
const RAIL_X: f32 = 6.0;
/// User messages taller than this many lines fold.
const CLAMP_LINES: f32 = 8.0;
/// The pill shows past this distance from the bottom, and hides within the
/// second.
const PILL_SHOW: f32 = 320.0;
const PILL_HIDE: f32 = 2.0;
const FADE: f32 = 18.0;

impl Assistant {
    /// The thread and the composer under it, or the empty state.
    pub(super) fn body_ui(
        &mut self,
        ui: &mut egui::Ui,
        composer: &Composer<'_>,
        changes: &mut ChangeLog,
        sources: &mut dyn FnMut() -> Sources,
    ) -> Option<PanelAction> {
        let mut action = None;
        if self.messages.is_empty() {
            let available = ui.available_height();
            let content = 132.0 + self.composer_height + 56.0;
            ui.add_space(((available - content) * 0.42).max(8.0));
            self.greeting(ui, composer);
            ui.add_space(18.0);
            let top = ui.cursor().top();
            action = self.composer_ui(ui, composer, changes, sources);
            self.composer_height = ui.cursor().top() - top;
            ui.add_space(12.0);
            if let Some(chosen) = self.suggestions(ui, composer, sources) {
                action = Some(chosen);
            }
            return action;
        }
        // The thread scrolls with Page Up and Down while typing.
        let page = ui.input_mut(|i| {
            if i.consume_key(egui::Modifiers::NONE, egui::Key::PageUp) {
                1.0
            } else if i.consume_key(egui::Modifiers::NONE, egui::Key::PageDown) {
                -1.0
            } else {
                0.0
            }
        });
        let height = (ui.available_height() - self.composer_height - 8.0).max(80.0);
        if let Some(chosen) = self.thread_ui(ui, height, page, changes) {
            action = Some(chosen);
        }
        ui.add_space(8.0);
        let top = ui.cursor().top();
        if let Some(chosen) = self.composer_ui(ui, composer, changes, sources) {
            action = Some(chosen);
        }
        self.composer_height = ui.cursor().top() - top;
        action
    }

    /// The badge, the question and what the assistant can do.
    fn greeting(&self, ui: &mut egui::Ui, composer: &Composer<'_>) {
        let tokens = Tokens::get(ui.ctx());
        ui.vertical_centered(|ui| {
            let (rect, _) = ui.allocate_exact_size(vec2(44.0, 44.0), Sense::hover());
            ui.painter()
                .circle_filled(rect.center(), 22.0, tokens.accent_soft);
            let icon = if self.agent.is_some() { "terminal" } else { "sparkles" };
            crate::icons::paint(ui, rect, icon, 21.0, tokens.accent);
            ui.add_space(12.0);
            let title = match composer.document {
                Some(document) => format!("Ask about {}", short(document, 40)),
                None => "What can I help with?".into(),
            };
            ui.label(
                egui::RichText::new(title)
                    .font(theme::semibold(17.0))
                    .color(tokens.text),
            );
            ui.add_space(4.0);
            let detail = if self.agent.is_some() {
                "It works in a folder for this document and reaches the reader through its tools; you can undo what it changes."
            } else {
                "It can search the document, read and look at pages, and highlight what matters; you can undo what it changes."
            };
            ui.add(
                egui::Label::new(
                    egui::RichText::new(detail)
                        .size(12.5)
                        .color(tokens.muted),
                )
                .wrap(),
            );
        });
    }

    /// Questions to start with, under the composer.
    fn suggestions(
        &mut self,
        ui: &mut egui::Ui,
        composer: &Composer<'_>,
        sources: &mut dyn FnMut() -> Sources,
    ) -> Option<PanelAction> {
        composer.document?;
        let mut chosen = None;
        let mut items: Vec<(&str, &str, &str, Option<&str>)> = Vec::new();
        if composer.selection_words.is_some() {
            items.push((
                "type",
                "Explain the selection",
                "Explain ",
                Some("Selection"),
            ));
        }
        if composer.page.is_some() {
            items.push((
                "file-text",
                "Summarize this page",
                "Summarize ",
                Some("This page"),
            ));
        }
        items.push((
            "list",
            "Key points of the document",
            "What are the key points of this document?",
            None,
        ));
        items.push((
            "highlighter",
            "Highlight the main claims",
            "Highlight the main claims on this page.",
            None,
        ));
        ui.vertical_centered(|ui| {
            ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
            for (icon, label, text, chip) in items {
                if crate::widgets::pill_button(ui, Some(icon), label, false).clicked() {
                    chosen = Some((text, chip));
                }
            }
        });
        let (text, chip) = chosen?;
        let mut draft = Draft::plain(text);
        if let Some(title) = chip {
            let candidate = ai_mentions::candidates(&sources(), "")
                .into_iter()
                .find(|candidate| candidate.title == title)?;
            draft.push_chip(ChipKind::Mention(candidate.mention));
        }
        Some(PanelAction::Send(draft))
    }

    /// The messages, scrolling, with a pill back to the bottom.
    fn thread_ui(
        &mut self,
        ui: &mut egui::Ui,
        height: f32,
        page: f32,
        changes: &ChangeLog,
    ) -> Option<PanelAction> {
        let tokens = Tokens::get(ui.ctx());
        let mut action = None;
        let id = egui::Id::new("assistant-thread");
        let jump = std::mem::take(&mut self.jump_to_bottom);
        let mut area = egui::ScrollArea::vertical()
            .id_salt(id)
            .auto_shrink(false)
            .stick_to_bottom(true)
            .max_height(height);
        if jump {
            area = area.vertical_scroll_offset(f32::MAX);
        }
        let output = area.show(ui, |ui| {
            if page != 0.0 {
                ui.scroll_with_delta(vec2(0.0, page * height * 0.8));
            }
            ui.add_space(6.0);
            let count = self.messages.len();
            let editing = self.editing.as_ref().map(|editing| editing.index);
            for index in 0..count {
                if self
                    .summary
                    .as_ref()
                    .is_some_and(|summary| summary.until == index && index > 0)
                {
                    divider(ui, "Earlier messages summarized", &tokens);
                }
                let dim = editing.is_some_and(|editing| index > editing);
                let chosen = ui
                    .scope(|ui| {
                        if dim {
                            ui.set_opacity(0.4);
                        }
                        match self.messages[index].role {
                            glyphst_ai::ai_chat::Role::User => {
                                self.user_message(ui, index, changes)
                            }
                            glyphst_ai::ai_chat::Role::Assistant => self.answer(ui, index),
                        }
                    })
                    .inner;
                if chosen.is_some() {
                    action = chosen;
                }
            }
            if self.run.is_none() {
                if let Some(error) = self.error.clone()
                    && let Some(chosen) = self.error_callout(ui, &error)
                {
                    action = Some(chosen);
                }
                if let Some(status) = &self.status {
                    ui.add_space(4.0);
                    ui.vertical_centered(|ui| {
                        ui.label(egui::RichText::new(status).size(12.0).color(tokens.faint));
                    });
                }
            }
            ui.add_space(10.0);
        });
        // Fades where the thread goes on past its edges.
        let rect = output.inner_rect;
        let offset = output.state.offset.y;
        let rest = (output.content_size.y - rect.height() - offset).max(0.0);
        if offset > 1.0 {
            fade(ui.painter(), rect, tokens.raised, true);
        }
        if rest > 1.0 {
            fade(ui.painter(), rect, tokens.raised, false);
        }
        if rest > PILL_SHOW {
            self.pill = true;
        } else if rest < PILL_HIDE {
            self.pill = false;
        }
        if self.pill && scroll_pill(ui, rect, &tokens).clicked() {
            self.jump_to_bottom = true;
            self.pill = false;
            ui.ctx().request_repaint();
        }
        action
    }

    /// A message of the user: its pictures, the bubble with its chips (or
    /// the editor when edited), and its actions on hover.
    fn user_message(
        &mut self,
        ui: &mut egui::Ui,
        index: usize,
        changes: &ChangeLog,
    ) -> Option<PanelAction> {
        let tokens = Tokens::get(ui.ctx());
        let mut action = None;
        ui.add_space(if index == 0 { 2.0 } else { 10.0 });
        let width = ui.available_width();
        let view = &mut self.views[index];
        // Pictures of a reopened chat become thumbnails when first drawn.
        for image in std::mem::take(&mut view.pending_images) {
            if let Ok(decoded) = image::load_from_memory(&image.bytes) {
                view.images
                    .push(super::thumbnail_texture(ui.ctx(), &decoded));
            }
        }
        if !view.images.is_empty() {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                for image in view.images.iter().rev() {
                    ui.add(
                        egui::Image::new(image)
                            .fit_to_exact_size(vec2(112.0, 80.0))
                            .maintain_aspect_ratio(true)
                            .corner_radius(8),
                    );
                }
            });
            ui.add_space(6.0);
        }
        if self
            .editing
            .as_ref()
            .is_some_and(|editing| editing.index == index)
        {
            return self.edit_ui(ui, index, changes);
        }
        let draft = view
            .draft
            .clone()
            .unwrap_or_else(|| Draft::plain(self.messages[index].display_text()));
        let expanded_id = egui::Id::new(("assistant-expanded", self.chat_id, index));
        let expanded = ui.data(|data| data.get_temp::<bool>(expanded_id).unwrap_or(false));
        let line = TEXT_LINE;
        let bubble = ui
            .with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                egui::Frame::new()
                    .fill(tokens.raised)
                    .corner_radius(CornerRadius::same(16))
                    .inner_margin(egui::Margin::symmetric(14, 9))
                    .show(ui, |ui| {
                        let max = width * 0.82 - 28.0;
                        let limit = (!expanded).then_some(line * CLAMP_LINES);
                        let shown = crate::ai_editor::show_static(
                            ui,
                            &draft,
                            max,
                            &theme::regular(super::composer::TEXT_SIZE),
                            tokens.text,
                            limit,
                        );
                        if let Some((chip, rect)) = shown.hovered
                            && let Some(chip) = draft.chips.get(chip)
                        {
                            self.previews.hovered(&chip.kind, rect);
                        }
                        if shown.clipped || expanded {
                            let label = if expanded { "Show less" } else { "Show more" };
                            if ui
                                .add(
                                    egui::Label::new(
                                        egui::RichText::new(label)
                                            .size(12.0)
                                            .color(tokens.accent_text),
                                    )
                                    .sense(Sense::click()),
                                )
                                .clicked()
                            {
                                ui.data_mut(|data| data.insert_temp(expanded_id, !expanded));
                            }
                        }
                    })
                    .response
                    .rect
            })
            .inner;
        // Copy, edit, restore and the time, on hover.
        let (lane, _) = ui.allocate_exact_size(vec2(width, LANE), Sense::hover());
        let hovered = ui.rect_contains_pointer(bubble.union(lane));
        let shown = ui.ctx().animate_bool_with_time(
            egui::Id::new(("assistant-lane", self.chat_id, index)),
            hovered || self.confirm_restore == Some(index),
            motion::HOVER_FADE,
        );
        if shown > 0.02 {
            let mut child = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(lane)
                    .layout(egui::Layout::right_to_left(egui::Align::Center)),
            );
            child.set_opacity(shown);
            child.spacing_mut().item_spacing.x = 2.0;
            let idle = self.run.is_none();
            if crate::icons::button(&mut child, "copy", 24.0, false, "Copy").clicked() {
                child.ctx().copy_text(draft.prompt().0);
            }
            if idle && crate::icons::button(&mut child, "pencil", 24.0, false, "Edit").clicked() {
                let mut editor = MentionEditor::new(egui::Id::new(("assistant-edit", index)));
                editor.set(child.ctx(), draft.clone());
                child
                    .ctx()
                    .memory_mut(|memory| memory.request_focus(editor.id));
                self.editing = Some(Editing { index, editor });
            }
            if idle
                && crate::icons::button(
                    &mut child,
                    "rotate-ccw",
                    24.0,
                    self.confirm_restore == Some(index),
                    "Restore to here: rewind the chat to this message and undo what the agent changed since",
                )
                .clicked()
            {
                self.confirm_restore = Some(index);
            }
            if let Some(at) = self.views[index].sent_at {
                child.add_space(6.0);
                child.label(
                    egui::RichText::new(clock(at))
                        .size(11.0)
                        .color(tokens.faint),
                );
            }
        }
        if self.confirm_restore == Some(index) {
            let undone = changes
                .count_where(|change| change.thread == Some(self.chat_id) && change.turn >= index);
            let detail = match undone {
                0 => "The chat goes back to before this message, which returns to the composer.".to_string(),
                1 => "The chat goes back to before this message and the agent's change since is undone.".to_string(),
                n => format!("The chat goes back to before this message and the agent's {n} changes since are undone."),
            };
            match callout(
                ui,
                Severity::Warning,
                "Restore to here?",
                &detail,
                &["Restore", "Cancel"],
            ) {
                Some(0) => action = Some(PanelAction::Restore(index, None)),
                Some(_) => self.confirm_restore = None,
                None => {}
            }
        }
        action
    }

    /// A sent message being edited: the editor in place of its bubble,
    /// with what sending it undoes.
    fn edit_ui(
        &mut self,
        ui: &mut egui::Ui,
        index: usize,
        changes: &ChangeLog,
    ) -> Option<PanelAction> {
        let tokens = Tokens::get(ui.ctx());
        let mut action = None;
        let undone = changes
            .count_where(|change| change.thread == Some(self.chat_id) && change.turn >= index);
        let editing = self.editing.as_mut()?;
        let focused = ui
            .ctx()
            .memory(|memory| memory.has_focus(editing.editor.id));
        editing.editor.track_input_method(ui);
        let (send, cancel) = if focused && !editing.editor.composing {
            ui.input_mut(|i| {
                (
                    i.consume_key(egui::Modifiers::NONE, egui::Key::Enter),
                    i.consume_key(egui::Modifiers::NONE, egui::Key::Escape),
                )
            })
        } else {
            (false, false)
        };
        let mut hovered = None;
        let mut close = false;
        egui::Frame::new()
            .fill(tokens.raised)
            .stroke(Stroke::new(1.0, tokens.accent))
            .corner_radius(CornerRadius::same(14))
            .inner_margin(egui::Margin::symmetric(12, 9))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                let output = editing.editor.show(
                    ui,
                    &EditorStyle {
                        hint: "Edit the message — @ to include context",
                        rows: 1,
                        max_height: 220.0,
                        font: theme::regular(super::composer::TEXT_SIZE),
                    },
                );
                hovered = output.hovered.and_then(|(chip, rect)| {
                    editing
                        .editor
                        .chips()
                        .get(chip)
                        .map(|chip| (chip.kind.clone(), rect))
                });
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    let note = match undone {
                        0 => "Sending restarts the chat from here.".to_string(),
                        1 => "Sending restarts from here and undoes 1 agent change.".to_string(),
                        n => format!("Sending restarts from here and undoes {n} agent changes."),
                    };
                    ui.label(egui::RichText::new(note).size(11.5).color(tokens.faint));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if crate::widgets::pill_button(ui, None, "Send", true).clicked() || send {
                            let draft = editing.editor.take();
                            if !draft.is_empty() {
                                action = Some(PanelAction::Restore(index, Some(draft)));
                            }
                        }
                        if crate::widgets::pill_button(ui, None, "Cancel", false).clicked()
                            || cancel
                        {
                            close = true;
                        }
                    });
                });
            });
        if close {
            self.editing = None;
        }
        if let Some((kind, rect)) = hovered {
            self.previews.hovered(&kind, rect);
        }
        ui.add_space(LANE);
        action
    }

    /// An answer: the work (live, or folded once done), the text, and
    /// copy and retry under it.
    fn answer(&mut self, ui: &mut egui::Ui, index: usize) -> Option<PanelAction> {
        let tokens = Tokens::get(ui.ctx());
        let mut action = None;
        let running = self.run.as_ref().is_some_and(|run| run.answer == index);
        let start = self.views[index].answer_start();
        let work = start > 0;
        if work {
            if running {
                self.timeline(ui, index, start, true);
            } else {
                let steps = self.views[index]
                    .tools()
                    .map(ToolRow::step)
                    .collect::<Vec<_>>();
                let thought = self.views[index]
                    .segments
                    .iter()
                    .any(|segment| matches!(segment, Segment::Thought(_)));
                let mut summary = summarize_steps(&steps);
                if thought {
                    summary = if summary.is_empty() {
                        "thought it through".into()
                    } else {
                        format!("thought, {summary}")
                    };
                }
                let label = match self.views[index].duration_ms {
                    Some(ms) => format!("Worked for {}", duration(ms)),
                    None => "Worked".into(),
                };
                let open = self.views[index].unfolded;
                if disclosure(ui, &label, &summary, open, &tokens).clicked() {
                    self.views[index].unfolded = !open;
                }
                let openness = ui.ctx().animate_bool_with_time(
                    egui::Id::new(("assistant-work", self.chat_id, index)),
                    open,
                    motion::COLLAPSE,
                );
                if openness > 0.0 {
                    ui.scope(|ui| {
                        ui.set_opacity(openness);
                        self.timeline(ui, index, start, false);
                    });
                }
            }
        }
        let text = self.views[index].segments[start..]
            .iter()
            .filter_map(|segment| match segment {
                Segment::Text(text) => Some(text.as_str()),
                _ => None,
            })
            .collect::<String>();
        if running {
            if !text.is_empty() {
                self.markdown_ui(ui, &text);
            }
            self.working_row(ui);
            return None;
        }
        if text.trim().is_empty() && !work {
            return None;
        }
        if !text.is_empty() {
            if work {
                ui.add_space(4.0);
            }
            self.markdown_ui(ui, &text);
        }
        // Copy and retry, with who answered and how long it took.
        let last = index + 1 == self.messages.len();
        let top = ui.min_rect().top();
        let (lane, _) = ui.allocate_exact_size(vec2(ui.available_width(), LANE), Sense::hover());
        let block = Rect::from_min_max(egui::pos2(lane.left(), top), lane.max);
        let hovered = ui.rect_contains_pointer(block);
        let shown = ui.ctx().animate_bool_with_time(
            egui::Id::new(("assistant-answer-lane", self.chat_id, index)),
            hovered,
            motion::HOVER_FADE,
        );
        let opacity = if last { 0.55 + 0.45 * shown } else { shown };
        if opacity > 0.02 {
            let mut child = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(lane)
                    .layout(egui::Layout::left_to_right(egui::Align::Center)),
            );
            child.set_opacity(opacity);
            child.spacing_mut().item_spacing.x = 2.0;
            if crate::icons::button(&mut child, "copy", 24.0, false, "Copy the answer").clicked() {
                child.ctx().copy_text(text.clone());
            }
            if self.run.is_none()
                && index > 0
                && crate::icons::button(&mut child, "refresh-cw", 24.0, false, "Ask again")
                    .clicked()
                && let Some(draft) = self.views[index - 1].draft.clone()
            {
                action = Some(PanelAction::Restore(index - 1, Some(draft)));
            }
            let mut detail = Vec::new();
            if last && !self.answerer.is_empty() {
                detail.push(self.answerer.clone());
            }
            if let Some(ms) = self.views[index].duration_ms {
                detail.push(duration(ms));
            }
            if !detail.is_empty() {
                child.add_space(4.0);
                child.label(
                    egui::RichText::new(detail.join(" · "))
                        .size(11.0)
                        .color(tokens.faint),
                );
            }
        }
        action
    }

    /// The answer's Markdown. egui draws bold in a stronger colour only,
    /// so the body is a little softer than the text colour.
    fn markdown_ui(&mut self, ui: &mut egui::Ui, text: &str) {
        let tokens = Tokens::get(ui.ctx());
        ui.scope(|ui| {
            let style = ui.style_mut();
            style
                .text_styles
                .insert(egui::TextStyle::Body, theme::regular(13.5));
            style.spacing.item_spacing.y = 7.0;
            let strong = if tokens.dark() {
                Color32::WHITE
            } else {
                Color32::BLACK
            };
            style.visuals.override_text_color = Some(theme::mix(tokens.text, tokens.raised, 0.14));
            style.visuals.widgets.active.fg_stroke.color = strong;
            egui_commonmark::CommonMarkViewer::new().show(ui, &mut self.markdown, text);
        });
    }

    /// The live row at the end of a running answer.
    fn working_row(&self, ui: &mut egui::Ui) {
        let tokens = Tokens::get(ui.ctx());
        let elapsed = self
            .run
            .as_ref()
            .map_or(0, |run| run.started.elapsed().as_secs());
        let label = match &self.status {
            Some(status) => status.clone(),
            None if elapsed >= 2 => format!("Working · {elapsed}s"),
            None => "Working".into(),
        };
        ui.add_space(4.0);
        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), STEP), Sense::hover());
        crate::widgets::loading_dots(
            ui,
            Rect::from_center_size(
                rect.left_center() + vec2(RAIL_X + 6.0, 0.0),
                vec2(24.0, 8.0),
            ),
            tokens.accent,
        );
        let galley = ui
            .painter()
            .layout_no_wrap(label, theme::regular(12.5), tokens.muted);
        let pos = egui::pos2(rect.left() + 28.0, rect.center().y - galley.size().y / 2.0);
        crate::widgets::shimmer_text(ui, pos, galley, tokens.faint, tokens.text);
        ui.ctx().request_repaint_after(Duration::from_millis(500));
    }

    /// The steps before an answer, on a rail; `live` while they run.
    fn timeline(&mut self, ui: &mut egui::Ui, index: usize, end: usize, live: bool) {
        let tokens = Tokens::get(ui.ctx());
        let top = ui.cursor().top();
        let mut nodes = Vec::new();
        for segment in 0..end {
            let last = live && segment + 1 == end;
            let item = self.views[index].segments[segment].clone();
            let open = self.views[index].open_steps.contains(&segment);
            let (rect, response) =
                ui.allocate_exact_size(vec2(ui.available_width(), STEP), Sense::click());
            let hovered = response.hovered();
            let (label, color, running, detail): (String, Color32, bool, Option<String>) =
                match &item {
                    Segment::Tool(row) => {
                        let step = row.step();
                        match row.state {
                            ToolState::Running => {
                                (format!("{}…", step.doing()), tokens.muted, true, None)
                            }
                            ToolState::Done => (step.done(), tokens.muted, false, None),
                            ToolState::Failed => {
                                (format!("{} — failed", step.done()), tokens.err, false, None)
                            }
                        }
                    }
                    Segment::Thought(text) => {
                        if last {
                            ("Thinking…".into(), tokens.muted, true, None)
                        } else {
                            (
                                "Thought".into(),
                                tokens.muted,
                                false,
                                Some(first_line(text)),
                            )
                        }
                    }
                    Segment::Text(text) => (first_line(text), tokens.muted, false, None),
                };
            nodes.push((
                rect.center().y,
                running,
                matches!(&item, Segment::Tool(row) if row.state == ToolState::Failed),
            ));
            let x = rect.left() + 22.0;
            let font = theme::regular(12.5);
            let room = rect.right() - x - 20.0;
            if running {
                let galley = ui.painter().layout_no_wrap(label, font, color);
                let pos = egui::pos2(x, rect.center().y - galley.size().y / 2.0);
                crate::widgets::shimmer_text(ui, pos, galley, tokens.faint, tokens.text);
            } else {
                let mut job = egui::text::LayoutJob::default();
                job.append(
                    &label,
                    0.0,
                    egui::TextFormat::simple(
                        font.clone(),
                        if hovered { tokens.text } else { color },
                    ),
                );
                if let Some(detail) = &detail {
                    job.append(
                        &format!("  {detail}"),
                        0.0,
                        egui::TextFormat::simple(font, tokens.faint),
                    );
                }
                job.wrap.max_width = room;
                job.wrap.max_rows = 1;
                job.wrap.break_anywhere = true;
                job.wrap.overflow_character = Some('…');
                let galley = ui.painter().layout_job(job);
                ui.painter().galley(
                    egui::pos2(x, rect.center().y - galley.size().y / 2.0),
                    galley,
                    color,
                );
            }
            let expandable = match &item {
                Segment::Tool(row) => !running && (row.output.is_some() || row.arguments.len() > 2),
                Segment::Thought(text) => !text.trim().is_empty() && !running,
                Segment::Text(text) => text.lines().count() > 1 || text.len() > 80,
            };
            if expandable && (hovered || open) {
                crate::widgets::chevron(
                    ui.painter(),
                    rect.right_center() - vec2(10.0, 0.0),
                    9.0,
                    if open { 1.0 } else { 0.0 },
                    tokens.faint,
                );
            }
            if expandable && response.clicked() {
                let steps = &mut self.views[index].open_steps;
                if !steps.remove(&segment) {
                    steps.insert(segment);
                }
            }
            if open && expandable {
                details(ui, &item, &tokens, &mut self.markdown);
            }
        }
        // The rail: a hairline through the steps, a dot at each.
        if let (Some(first), Some(last)) = (nodes.first(), nodes.last()) {
            let x = ui.min_rect().left() + RAIL_X + 3.0;
            let painter = ui.painter();
            if nodes.len() > 1 {
                painter.vline(x, first.0..=last.0, Stroke::new(1.0, tokens.rule));
            }
            for (y, running, failed) in &nodes {
                let color = if *failed {
                    tokens.err
                } else if *running {
                    Tokens::fade(tokens.accent, crate::widgets::pulse(ui, 0.45, 1.0, 1.2))
                } else {
                    tokens.rule
                };
                painter.circle_filled(egui::pos2(x, *y), 3.0, tokens.raised);
                painter.circle_filled(egui::pos2(x, *y), 2.5, color);
            }
        }
        let _ = top;
    }

    /// An error with retry and copy, after Zed's callouts.
    fn error_callout(&mut self, ui: &mut egui::Ui, error: &str) -> Option<PanelAction> {
        let question = (0..self.messages.len())
            .rev()
            .find(|&index| self.views[index].draft.is_some());
        let buttons: &[&str] = if question.is_some() {
            &["Retry", "Copy"]
        } else {
            &["Copy"]
        };
        ui.add_space(8.0);
        match callout(ui, Severity::Error, "Something went wrong", error, buttons) {
            Some(0) if question.is_some() => {
                let index = question?;
                let draft = self.views[index].draft.clone()?;
                Some(PanelAction::Restore(index, Some(draft)))
            }
            Some(_) => {
                ui.ctx().copy_text(error.to_string());
                None
            }
            None => None,
        }
    }
}

/// The line height of messages.
const TEXT_LINE: f32 = 19.0;

#[derive(Clone, Copy)]
enum Severity {
    Error,
    Warning,
}

/// A tinted card with an icon, a title, a description and buttons; returns
/// the button clicked.
fn callout(
    ui: &mut egui::Ui,
    severity: Severity,
    title: &str,
    detail: &str,
    buttons: &[&str],
) -> Option<usize> {
    let tokens = Tokens::get(ui.ctx());
    let (color, icon) = match severity {
        Severity::Error => (tokens.err, "circle-alert"),
        Severity::Warning => (tokens.warn, "triangle-alert"),
    };
    let mut clicked = None;
    egui::Frame::new()
        .fill(Tokens::fade(color, if tokens.dark() { 0.12 } else { 0.08 }))
        .stroke(Stroke::new(1.0, Tokens::fade(color, 0.25)))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(egui::Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_top(|ui| {
                ui.add(crate::icons::image(icon, 15.0, color));
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 3.0;
                    ui.label(
                        egui::RichText::new(title)
                            .font(theme::medium(13.0))
                            .color(tokens.text),
                    );
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(detail).size(12.0).color(tokens.muted),
                        )
                        .wrap(),
                    );
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        for (index, label) in buttons.iter().enumerate() {
                            if crate::widgets::pill_button(
                                ui,
                                None,
                                label,
                                index == 0 && buttons.len() > 1,
                            )
                            .clicked()
                            {
                                clicked = Some(index);
                            }
                        }
                    });
                });
            });
        });
    clicked
}

/// A finished answer's work, folded: "Worked for 12s · searched 2×".
fn disclosure(
    ui: &mut egui::Ui,
    label: &str,
    summary: &str,
    open: bool,
    tokens: &Tokens,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), STEP), Sense::click());
    let hovered = response.hovered();
    let mut job = egui::text::LayoutJob::default();
    job.append(
        label,
        0.0,
        egui::TextFormat::simple(
            theme::medium(12.5),
            if hovered { tokens.text } else { tokens.muted },
        ),
    );
    if !summary.is_empty() {
        job.append(
            &format!(" · {summary}"),
            0.0,
            egui::TextFormat::simple(theme::regular(12.5), tokens.faint),
        );
    }
    job.wrap.max_width = rect.width() - 24.0;
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    job.wrap.overflow_character = Some('…');
    let galley = ui.painter().layout_job(job);
    let size = galley.size();
    ui.painter().galley(
        egui::pos2(rect.left(), rect.center().y - size.y / 2.0),
        galley,
        tokens.muted,
    );
    let turn = ui
        .ctx()
        .animate_bool_with_time(response.id, open, motion::CHEVRON);
    if hovered || open {
        crate::widgets::chevron(
            ui.painter(),
            egui::pos2(rect.left() + size.x + 10.0, rect.center().y),
            9.0,
            turn,
            tokens.faint,
        );
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A step's details: a thought's text, or a tool's input and output.
fn details(
    ui: &mut egui::Ui,
    item: &Segment,
    tokens: &Tokens,
    markdown: &mut egui_commonmark::CommonMarkCache,
) {
    egui::Frame::new()
        .fill(tokens.ink(0.03))
        .stroke(Stroke::new(1.0, tokens.rule))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(egui::Margin::symmetric(10, 8))
        .outer_margin(egui::Margin {
            left: 22,
            right: 0,
            top: 2,
            bottom: 6,
        })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            egui::ScrollArea::vertical()
                .id_salt(ui.next_auto_id())
                .max_height(220.0)
                .show(ui, |ui| match item {
                    Segment::Thought(text) | Segment::Text(text) => {
                        ui.scope(|ui| {
                            ui.style_mut()
                                .text_styles
                                .insert(egui::TextStyle::Body, theme::regular(12.5));
                            ui.visuals_mut().override_text_color = Some(tokens.muted);
                            egui_commonmark::CommonMarkViewer::new().show(
                                ui,
                                markdown,
                                text.trim(),
                            );
                        });
                    }
                    Segment::Tool(row) => {
                        let input = serde_json::from_str::<serde_json::Value>(&row.arguments)
                            .ok()
                            .and_then(|value| serde_json::to_string_pretty(&value).ok())
                            .unwrap_or_else(|| row.arguments.clone());
                        section(ui, "Input", &input, tokens);
                        if let Some(output) = &row.output {
                            ui.add_space(6.0);
                            let output: String = output.chars().take(4_000).collect();
                            section(ui, "Output", &output, tokens);
                        }
                    }
                });
        });
}

fn section(ui: &mut egui::Ui, title: &str, text: &str, tokens: &Tokens) {
    if text.trim().is_empty() || text.trim() == "{}" {
        return;
    }
    ui.label(
        egui::RichText::new(title)
            .font(theme::medium(11.0))
            .color(tokens.faint),
    );
    ui.add(
        egui::Label::new(
            egui::RichText::new(text)
                .font(theme::mono(11.0))
                .color(tokens.muted),
        )
        .wrap(),
    );
}

/// A line across the thread with a label in the middle.
fn divider(ui: &mut egui::Ui, label: &str, tokens: &Tokens) {
    ui.add_space(12.0);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 16.0), Sense::hover());
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_string(), theme::regular(11.0), tokens.faint);
    let half = galley.size().x / 2.0 + 10.0;
    let stroke = Stroke::new(1.0, tokens.rule);
    ui.painter().hline(
        rect.left()..=rect.center().x - half,
        rect.center().y,
        stroke,
    );
    ui.painter().hline(
        rect.center().x + half..=rect.right(),
        rect.center().y,
        stroke,
    );
    ui.painter()
        .galley(rect.center() - galley.size() / 2.0, galley, tokens.faint);
}

/// A soft band where the thread scrolls on past `rect`'s top or bottom.
fn fade(painter: &egui::Painter, rect: Rect, color: Color32, top: bool) {
    let band = if top {
        Rect::from_min_size(rect.min, vec2(rect.width(), FADE))
    } else {
        Rect::from_min_max(egui::pos2(rect.left(), rect.bottom() - FADE), rect.max)
    };
    let clear = Color32::from_rgba_premultiplied(0, 0, 0, 0);
    let (upper, lower) = if top { (color, clear) } else { (clear, color) };
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(band.left_top(), upper);
    mesh.colored_vertex(band.right_top(), upper);
    mesh.colored_vertex(band.left_bottom(), lower);
    mesh.colored_vertex(band.right_bottom(), lower);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(1, 2, 3);
    painter.add(egui::Shape::mesh(mesh));
}

/// "↓ Scroll to bottom", floating over the thread's bottom.
fn scroll_pill(ui: &mut egui::Ui, thread: Rect, tokens: &Tokens) -> egui::Response {
    let label = "Scroll to bottom";
    let galley = ui
        .painter()
        .layout_no_wrap(label.into(), theme::medium(12.0), tokens.text);
    let size = vec2(galley.size().x + 40.0, 30.0);
    let rect = Rect::from_center_size(
        egui::pos2(thread.center().x, thread.bottom() - 14.0 - size.y / 2.0),
        size,
    );
    let response = ui.interact(rect, egui::Id::new("assistant-scroll-pill"), Sense::click());
    let painter = ui.painter();
    painter.add(egui::Shadow::NONE.as_shape(rect, CornerRadius::same(15)));
    painter.rect(
        rect,
        CornerRadius::same(15),
        if response.hovered() {
            tokens.hover
        } else {
            tokens.raised
        },
        Stroke::new(1.0, tokens.rule),
        egui::StrokeKind::Inside,
    );
    crate::icons::paint(
        ui,
        Rect::from_center_size(rect.left_center() + vec2(17.0, 0.0), vec2(14.0, 14.0)),
        "arrow-down",
        14.0,
        tokens.muted,
    );
    ui.painter().galley(
        egui::pos2(rect.left() + 30.0, rect.center().y - galley.size().y / 2.0),
        galley,
        tokens.text,
    );
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// The first line of `text`, as a preview.
fn first_line(text: &str) -> String {
    text.trim()
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or_default()
        .trim_start_matches(['#', '*', '-', ' '])
        .to_string()
}

fn short(text: &str, chars: usize) -> String {
    let short: String = text.chars().take(chars).collect();
    if text.chars().count() > chars {
        format!("{short}…")
    } else {
        short
    }
}

/// "0.9s", "12s", "1m 12s".
pub(super) fn duration(ms: u64) -> String {
    match ms {
        0..1_000 => format!("{:.1}s", ms as f64 / 1_000.0),
        1_000..60_000 => format!("{}s", ms / 1_000),
        _ => format!("{}m {}s", ms / 60_000, ms / 1_000 % 60),
    }
}

/// The local time of a message: "3:45 PM", or "Oct 12, 3:45 PM" before
/// today.
fn clock(at: u64) -> String {
    use chrono::{Local, TimeZone as _};
    let Some(time) = Local.timestamp_opt(at as i64, 0).single() else {
        return String::new();
    };
    if time.date_naive() == Local::now().date_naive() {
        time.format("%-I:%M %p").to_string()
    } else {
        time.format("%b %-d, %-I:%M %p").to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_and_previews_read_well() {
        assert_eq!(duration(870), "0.9s");
        assert_eq!(duration(12_400), "12s");
        assert_eq!(duration(72_000), "1m 12s");
        assert_eq!(first_line("\n## Plan\nthen"), "Plan");
        assert_eq!(short("abcdef", 3), "abc…");
    }
}
