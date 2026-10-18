//! The assistant's pieces (pen components `msg/user`, `timeline-step` and
//! `suggestion`): the user's message on `raised` with an accent left edge,
//! one line per tool step with its state glyph and time, and `> prompt`
//! suggestions with their command.

use egui::{Align2, Rect, Response, Sense, StrokeKind, Ui, vec2};

use crate::{
    text,
    theme::{self, Tokens},
};

/// The frame of the user's message: `raised`, a 1px accent edge at the
/// left, radius 0 / 4 / 4 / 0.
pub fn user_frame(ctx: &egui::Context, accent: bool) -> egui::Frame {
    let t = Tokens::get(ctx);
    egui::Frame::new()
        .fill(t.raised)
        .corner_radius(egui::CornerRadius {
            nw: 0,
            ne: t.r_pop,
            se: t.r_pop,
            sw: 0,
        })
        .inner_margin(egui::Margin {
            left: 13,
            right: 12,
            top: 8,
            bottom: 8,
        })
        .stroke(if accent {
            egui::Stroke::NONE
        } else {
            t.hairline()
        })
}

/// Paints the accent edge of a user message in `rect`.
pub fn paint_user_edge(ui: &Ui, rect: Rect, color: egui::Color32) {
    ui.painter().rect_filled(
        Rect::from_min_size(rect.min, vec2(1.0, rect.height())),
        0,
        color,
    );
}

/// The user's message: `who` faint over `body` in prose.
pub fn user_message(ui: &mut Ui, who: &str, body: &str) -> Response {
    let t = Tokens::get(ui.ctx());
    let inner = user_frame(ui.ctx(), true).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = 6.0;
        ui.label(
            egui::RichText::new(who)
                .font(theme::mono(10.0))
                .color(t.faint),
        );
        ui.add(
            egui::Label::new(
                egui::RichText::new(body)
                    .font(theme::prose(ui))
                    .color(t.text),
            )
            .wrap(),
        );
    });
    paint_user_edge(ui, inner.response.rect, t.accent);
    inner.response
}

/// The state of a timeline step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepState {
    Done,
    Running,
    Failed,
    Thinking,
}

/// One step: `✓ read_page  p.4 · 2.1k tok          0.4s`.
pub struct TimelineStep<'a> {
    pub state: StepState,
    pub tool: &'a str,
    pub detail: &'a str,
    pub time: &'a str,
    /// Dimmed, as steps under an edited message.
    pub faded: bool,
}

impl TimelineStep<'_> {
    pub fn show(self, ui: &mut Ui) -> Response {
        let t = Tokens::get(ui.ctx());
        let (rect, response) =
            ui.allocate_exact_size(vec2(ui.available_width(), 24.0), Sense::hover());
        self.paint(ui, rect);
        let _ = t;
        response
    }

    pub fn paint(&self, ui: &Ui, rect: Rect) {
        let t = Tokens::get(ui.ctx());
        let fade = |color: egui::Color32| {
            if self.faded {
                Tokens::fade(color, 0.45)
            } else {
                color
            }
        };
        let font = theme::mono(11.0);
        let y = rect.center().y;
        let (glyph, color) = match self.state {
            StepState::Done => ("✓", t.ok),
            StepState::Running => ("◐", t.accent),
            StepState::Failed => ("✕", t.err),
            StepState::Thinking => ("◐", t.accent),
        };
        ui.painter().text(
            egui::pos2(rect.left(), y),
            Align2::LEFT_CENTER,
            glyph,
            font.clone(),
            fade(color),
        );
        let mut x = rect.left() + 18.0;
        let time_rect = text::line(
            ui,
            egui::pos2(rect.right(), y),
            Align2::RIGHT_CENTER,
            self.time,
            theme::mono(10.0),
            fade(t.faint),
            60.0,
        );
        let tool = text::line(
            ui,
            egui::pos2(x, y),
            Align2::LEFT_CENTER,
            self.tool,
            font.clone(),
            fade(t.text_2),
            (time_rect.left() - x) * 0.6,
        );
        x = tool.right() + 8.0;
        text::row(
            ui,
            egui::pos2(x, y),
            self.detail,
            font,
            fade(t.faint),
            time_rect.left() - 8.0 - x,
        );
    }
}

/// A suggestion: `> summarize this page        /sum`. Returns its response.
pub fn suggestion(ui: &mut Ui, prompt: &str, command: &str) -> Response {
    let t = Tokens::get(ui.ctx());
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 32.0), Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, ui.is_enabled(), prompt));
    let hovered = response.hovered();
    ui.painter().rect(
        rect,
        t.ctl_radius(),
        if hovered {
            t.hover
        } else {
            egui::Color32::TRANSPARENT
        },
        if hovered { t.edge() } else { t.hairline() },
        StrokeKind::Inside,
    );
    let y = rect.center().y;
    ui.painter().text(
        egui::pos2(rect.left() + 10.0, y),
        Align2::LEFT_CENTER,
        ">",
        theme::mono(12.0),
        t.accent,
    );
    let command_rect = text::line(
        ui,
        egui::pos2(rect.right() - 10.0, y),
        Align2::RIGHT_CENTER,
        command,
        theme::mono(10.0),
        t.faint,
        80.0,
    );
    text::row(
        ui,
        egui::pos2(rect.left() + 28.0, y),
        prompt,
        theme::mono(12.0),
        if hovered { t.text } else { t.text_2 },
        command_rect.left() - 10.0 - rect.left() - 28.0,
    );
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A folding header of a group of steps: `▾ worked · 3 steps · 2.4s ────`.
pub fn fold_header(ui: &mut Ui, label: &str, open: bool, running: bool) -> Response {
    let t = Tokens::get(ui.ctx());
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 20.0), Sense::click());
    let y = rect.center().y;
    let glyph = if open { "▾" } else { "▸" };
    let color = if response.hovered() { t.text } else { t.faint };
    ui.painter().text(
        egui::pos2(rect.left(), y),
        Align2::LEFT_CENTER,
        glyph,
        theme::mono(10.0),
        color,
    );
    let font = theme::mono(11.0);
    let label_rect = if running {
        let galley = ui
            .painter()
            .layout_no_wrap(label.to_owned(), font.clone(), t.muted);
        let pos = egui::pos2(rect.left() + 14.0, y - galley.size().y / 2.0);
        let size = galley.size();
        crate::paint::shimmer_text(ui, pos, galley, t.muted, t.text);
        Rect::from_min_size(pos, size)
    } else {
        text::line(
            ui,
            egui::pos2(rect.left() + 14.0, y),
            Align2::LEFT_CENTER,
            label,
            font,
            if response.hovered() { t.text } else { t.muted },
            rect.width() - 14.0,
        )
    };
    if t.ornaments && rect.right() > label_rect.right() + 20.0 {
        ui.painter().hline(
            label_rect.right() + 8.0..=rect.right(),
            y,
            egui::Stroke::new(1.0, t.rule),
        );
    }
    response
}
