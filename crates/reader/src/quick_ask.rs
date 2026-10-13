//! Quick ask: a small floating composer for one question about the page,
//! the selection or a region, answered in a card under it without the
//! assistant's chat history. "Continue in assistant" carries the exchange
//! into the assistant panel. Placed, sized and faded by the AI settings'
//! quick ask section, as the GPUI Reader's was.

use glyphst_settings::{QuickAskPosition, QuickAskSettings};

use crate::{
    ai::{self, Assistant},
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

#[derive(Default)]
pub struct QuickAsk {
    pub open: bool,
    pub composer: String,
    /// Answers one question at a time; holds the attachments too.
    pub assistant: Assistant,
    /// Focus the composer on the next frame (once: focusing every frame
    /// resets an input method's composition).
    focus: bool,
    markdown: egui_commonmark::CommonMarkCache,
}

/// What the quick ask card asked for.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    Send(String),
    Continue,
    Close,
}

impl QuickAsk {
    pub fn show_composer(&mut self) {
        self.open = true;
        self.focus = true;
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
            .chrome
            .to_opaque()
            .gamma_multiply(1.0 - settings.transparency.clamp(0.0, 0.9));
        egui::Area::new(egui::Id::new("quick-ask"))
            .order(egui::Order::Foreground)
            .fixed_pos(pos)
            .pivot(pivot)
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(fill)
                    .stroke(egui::Stroke::new(1.0, tokens.border))
                    .corner_radius(14)
                    .shadow(egui::Shadow {
                        offset: [0, 6],
                        blur: 24,
                        spread: 0,
                        color: egui::Color32::from_black_alpha(40),
                    })
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
        ui.horizontal(|ui| {
            ui.add(crate::icons::image("sparkles", 15.0, tokens.accent));
            ui.label(egui::RichText::new("Ask").font(crate::theme::semibold(14.0)));
            if let Some(selection) = selection {
                ui.label(
                    egui::RichText::new(format!("· {}", short(selection, 56)))
                        .small()
                        .color(tokens.text_muted),
                );
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if crate::icons::button(ui, "x", 24.0, false, "Close (Esc)").clicked() {
                    action = Some(Action::Close);
                }
            });
        });
        ai::attachment_chips(ui, &mut self.assistant.attachments, tokens);
        let font = egui::FontId::proportional(settings.text_size.clamp(10.0, 24.0));
        let input = ui.add(
            egui::TextEdit::multiline(&mut self.composer)
                .id(egui::Id::new("quick-ask-input"))
                .hint_text("Ask about this page")
                .font(font)
                .frame(egui::Frame::NONE)
                .desired_rows(1)
                .desired_width(f32::INFINITY),
        );
        if std::mem::take(&mut self.focus) {
            input.request_focus();
        }
        let (enter, escape) = ui.input(|i| {
            (
                i.key_pressed(egui::Key::Enter) && !i.modifiers.shift,
                i.key_pressed(egui::Key::Escape),
            )
        });
        // A text field gives up its focus on Escape.
        if escape && (input.has_focus() || input.lost_focus()) {
            action = Some(Action::Close);
        }
        if input.has_focus() && enter && !self.composer.trim().is_empty() {
            // Enter inserted a newline into the text before it was seen.
            let text = std::mem::take(&mut self.composer);
            action = Some(Action::Send(text.trim_end().to_string()));
        }
        let running = self.assistant.is_running();
        let answer = self.assistant.latest_answer();
        if running || answer.is_some() || self.assistant.error.is_some() {
            ui.separator();
            egui::ScrollArea::vertical()
                .max_height(ANSWER_HEIGHT)
                .auto_shrink([false, true])
                .show(ui, |ui| match &answer {
                    Some(answer) if !answer.is_empty() => {
                        egui_commonmark::CommonMarkViewer::new().show(
                            ui,
                            &mut self.markdown,
                            answer,
                        );
                    }
                    _ if running => {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(egui::RichText::new("Thinking…").color(tokens.text_muted));
                        });
                    }
                    _ => {}
                });
            if let Some(error) = &self.assistant.error {
                ui.label(egui::RichText::new(error).color(tokens.danger));
            }
            if !running && answer.is_some() {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if crate::widgets::ghost_button(ui, "message-square", "Continue in assistant")
                        .clicked()
                    {
                        action = Some(Action::Continue);
                    }
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
        assert_eq!(frame(&mut quick, &ctx, Vec::new()), None);
        assert_eq!(
            frame(&mut quick, &ctx, vec![egui::Event::Text("Why?".into())]),
            None
        );
        assert_eq!(quick.composer, "Why?");
        assert_eq!(
            frame(&mut quick, &ctx, vec![key(egui::Key::Enter)]),
            Some(Action::Send("Why?".into()))
        );
        assert!(quick.composer.is_empty());
        assert_eq!(
            frame(&mut quick, &ctx, vec![key(egui::Key::Escape)]),
            Some(Action::Close)
        );
    }
}
