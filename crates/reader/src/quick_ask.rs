//! Quick ask: a small floating composer for one question about the page,
//! the selection or a region, answered in a card under it without the
//! assistant's chat history. "Continue in assistant" carries the exchange
//! into the assistant panel. Placed, sized and faded by the AI settings'
//! quick ask section, as the GPUI Reader's was.

use glyphst_settings::{QuickAskPosition, QuickAskSettings};

use crate::{
    ai::{Assistant, Attachment},
    ai_editor::{ChipKind, Draft, EditorStyle, MentionEditor},
    theme::Tokens,
};

/// Distance from the stage's edges.
const MARGIN: f32 = 24.0;
/// Room for the dock under a bottom composer.
const DOCK_ROOM: f32 = 84.0;
const SIDE_WIDTH: f32 = 360.0;
const WIDE_MAX: f32 = 760.0;
/// The answer scrolls past this height.
const ANSWER_HEIGHT: f32 = 320.0;

pub struct QuickAsk {
    pub open: bool,
    /// The question, with regions as chips.
    pub editor: MentionEditor,
    /// Answers one question at a time.
    pub assistant: Assistant,
    /// Focus the composer on the next frame (once: focusing every frame
    /// resets an input method's composition).
    focus: bool,
    markdown: egui_commonmark::CommonMarkCache,
}

impl Default for QuickAsk {
    fn default() -> Self {
        Self {
            open: false,
            editor: MentionEditor::new(egui::Id::new("quick-ask-input")),
            assistant: Assistant::default(),
            focus: false,
            markdown: egui_commonmark::CommonMarkCache::default(),
        }
    }
}

/// What the quick ask card asked for.
#[derive(Clone, Debug)]
pub enum Action {
    Send(Draft),
    Continue,
    Close,
}

impl QuickAsk {
    pub fn show_composer(&mut self) {
        self.open = true;
        self.focus = true;
    }

    /// Attaches a region of a page to the question.
    pub fn attach(&mut self, ctx: &egui::Context, attachment: Attachment) {
        self.editor
            .insert_chip(ctx, None, ChipKind::Image(attachment));
    }

    pub fn close(&mut self) {
        self.open = false;
        self.assistant.stop();
    }

    /// Draws the card over `stage`; `selection` is the selected text that
    /// goes with the question.
    pub fn ui(
        &mut self,
        ctx: &egui::Context,
        stage: egui::Rect,
        settings: &QuickAskSettings,
        selection: Option<&str>,
    ) -> Option<Action> {
        if !self.open {
            return None;
        }
        let tokens = Tokens::get(ctx);
        let (pos, pivot, width) = placement(stage, settings.position);
        let mut action = None;
        // The theme's chrome is see-through already; the setting alone
        // decides how much the page shows through.
        let fill = tokens
            .raised
            .to_opaque()
            .gamma_multiply(1.0 - settings.transparency.clamp(0.0, 0.9));
        egui::Area::new(egui::Id::new("quick-ask"))
            .order(egui::Order::Foreground)
            .fixed_pos(pos)
            .pivot(pivot)
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(fill)
                    .stroke(tokens.edge())
                    .corner_radius(tokens.pop_radius())
                    .inner_margin(egui::Margin::symmetric(12, 10))
                    .show(ui, |ui| {
                        ui.set_width(width);
                        action = self.contents(ui, settings, selection, &tokens);
                    });
            });
        action
    }

    fn contents(
        &mut self,
        ui: &mut egui::Ui,
        settings: &QuickAskSettings,
        selection: Option<&str>,
        tokens: &Tokens,
    ) -> Option<Action> {
        let mut action = None;
        // The selection as a context chip, then the question.
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            match selection {
                Some(selection) => {
                    let label = format!("@{}", short(selection, 24).to_lowercase());
                    glyphst_ui::badge::Chip::new(&label)
                        .icon("text-cursor")
                        .show(ui);
                }
                None => {
                    ui.label(
                        egui::RichText::new("ask")
                            .font(crate::theme::mono(11.0))
                            .color(tokens.text_2),
                    );
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if crate::icons::button(ui, "x", 22.0, false, "close (esc)").clicked() {
                    action = Some(Action::Close);
                }
            });
        });
        let font = egui::FontId::proportional(settings.text_size.clamp(10.0, 24.0));
        self.editor.track_input_method(ui);
        let focused = ui.ctx().memory(|memory| memory.has_focus(self.editor.id));
        let enter = focused
            && !self.editor.composing
            && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter));
        let input = self
            .editor
            .show(
                ui,
                &EditorStyle {
                    hint: "ask about this page",
                    rows: 1,
                    max_height: 160.0,
                    font,
                },
            )
            .response;
        if std::mem::take(&mut self.focus) {
            input.request_focus();
        }
        let escape = ui.input(|i| i.key_pressed(egui::Key::Escape));
        // A text field gives up its focus on Escape.
        if escape && (input.has_focus() || input.lost_focus()) {
            action = Some(Action::Close);
        }
        if enter && !self.editor.is_empty() {
            action = Some(Action::Send(self.editor.take()));
        }
        let running = self.assistant.is_running();
        let answer = self.assistant.latest_answer();
        if running || answer.is_some() || self.assistant.error.is_some() {
            let line = ui.available_rect_before_wrap();
            ui.painter()
                .hline(line.x_range(), line.top() + 3.0, tokens.hairline());
            ui.add_space(8.0);
            egui::ScrollArea::vertical()
                .max_height(ANSWER_HEIGHT)
                .auto_shrink([false, true])
                .show(ui, |ui| match &answer {
                    Some(answer) if !answer.is_empty() => {
                        ui.style_mut()
                            .text_styles
                            .insert(egui::TextStyle::Body, crate::theme::regular(14.0));
                        egui_commonmark::CommonMarkViewer::new().show(
                            ui,
                            &mut self.markdown,
                            answer,
                        );
                    }
                    _ if running => {
                        ui.horizontal(|ui| {
                            let (rect, _) = ui
                                .allocate_exact_size(egui::vec2(24.0, 14.0), egui::Sense::hover());
                            crate::widgets::loading_dots(ui, rect, tokens.accent);
                            ui.label(
                                egui::RichText::new("thinking")
                                    .font(crate::theme::mono(11.0))
                                    .color(tokens.muted),
                            );
                        });
                    }
                    _ => {}
                });
            if let Some(error) = &self.assistant.error {
                ui.label(
                    egui::RichText::new(error)
                        .font(crate::theme::mono(11.0))
                        .color(tokens.err),
                );
            }
            if !running && let Some(answer) = &answer {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    if ui
                        .add(glyphst_ui::button::Button::bracket("continue in assistant"))
                        .clicked()
                    {
                        action = Some(Action::Continue);
                    }
                    if ui
                        .add(glyphst_ui::button::Button::bracket_muted("copy"))
                        .clicked()
                    {
                        ui.ctx().copy_text(answer.clone());
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            egui::RichText::new("esc")
                                .font(crate::theme::mono(10.0))
                                .color(tokens.faint),
                        );
                    });
                });
            }
        }
        action
    }
}

/// Where the card goes: its anchor, the anchor's place on the card, and
/// its width.
pub fn placement(stage: egui::Rect, position: QuickAskPosition) -> (egui::Pos2, egui::Align2, f32) {
    let wide = (stage.width() - 2.0 * MARGIN).clamp(200.0, WIDE_MAX);
    let side = SIDE_WIDTH.min(stage.width() - 2.0 * MARGIN).max(200.0);
    match position {
        QuickAskPosition::Top => (
            egui::pos2(stage.center().x, stage.top() + MARGIN),
            egui::Align2::CENTER_TOP,
            wide,
        ),
        QuickAskPosition::Bottom => (
            egui::pos2(stage.center().x, stage.bottom() - DOCK_ROOM),
            egui::Align2::CENTER_BOTTOM,
            wide,
        ),
        QuickAskPosition::Left => (
            egui::pos2(stage.left() + MARGIN, stage.center().y),
            egui::Align2::LEFT_CENTER,
            side,
        ),
        QuickAskPosition::Right => (
            egui::pos2(stage.right() - MARGIN, stage.center().y),
            egui::Align2::RIGHT_CENTER,
            side,
        ),
        QuickAskPosition::Center => (
            stage.center(),
            egui::Align2::CENTER_CENTER,
            wide.clamp(280.0, 640.0).min(stage.width() - 2.0 * MARGIN),
        ),
    }
}

/// `text` on one line, cut to `limit` characters.
fn short(text: &str, limit: usize) -> String {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() <= limit {
        return line;
    }
    let mut cut: String = line.chars().take(limit - 1).collect();
    cut.push('…');
    cut
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_card_goes_where_the_settings_say() {
        let stage = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1200.0, 800.0));
        let (pos, pivot, width) = placement(stage, QuickAskPosition::Bottom);
        assert_eq!((pos.x, pivot), (600.0, egui::Align2::CENTER_BOTTOM));
        assert!(pos.y < 800.0 - 60.0, "above the dock");
        assert_eq!(width, WIDE_MAX);
        let (pos, _, width) = placement(stage, QuickAskPosition::Left);
        assert_eq!((pos.x, width), (MARGIN, SIDE_WIDTH));
        let (_, _, width) = placement(stage, QuickAskPosition::Center);
        assert_eq!(width, 640.0);
        let narrow = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(300.0, 800.0));
        assert!(placement(narrow, QuickAskPosition::Right).2 <= 300.0);
    }

    #[test]
    fn selections_are_quoted_on_one_short_line() {
        assert_eq!(short("a\n  b", 10), "a b");
        assert_eq!(short("abcdefghij", 5), "abcd…");
    }

    /// One frame of the card with `events`, focused as a user's would be.
    fn frame(
        quick: &mut QuickAsk,
        ctx: &egui::Context,
        events: Vec<egui::Event>,
    ) -> Option<Action> {
        let input = egui::RawInput {
            events,
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1200.0, 800.0),
            )),
            ..Default::default()
        };
        let mut action = None;
        let mut output = ctx.run_ui(input, |ui| {
            action = quick.ui(
                ui.ctx(),
                ui.max_rect(),
                &QuickAskSettings::default(),
                Some("text"),
            );
        });
        output.textures_delta.clear();
        action
    }

    fn key(key: egui::Key) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }
    }

    #[test]
    fn typing_and_enter_send_the_question_and_escape_closes() {
        let ctx = egui::Context::default();
        ctx.set_fonts(crate::fonts::embedded());
        let mut quick = QuickAsk::default();
        quick.show_composer();
        assert!(frame(&mut quick, &ctx, Vec::new()).is_none());
        assert!(frame(&mut quick, &ctx, vec![egui::Event::Text("Why?".into())]).is_none());
        assert_eq!(quick.editor.text(), "Why?");
        match frame(&mut quick, &ctx, vec![key(egui::Key::Enter)]) {
            Some(Action::Send(draft)) => assert_eq!(draft.prompt().0, "Why?"),
            other => panic!("sent, not {other:?}"),
        }
        assert!(quick.editor.is_empty());
        assert!(matches!(
            frame(&mut quick, &ctx, vec![key(egui::Key::Escape)]),
            Some(Action::Close)
        ));
    }
}
