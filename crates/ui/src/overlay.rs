//! Small floating pieces (pen components `tooltip`, `toast` and
//! `pending-keys`): inverted tooltips with their key, the toast with an
//! optional `[ action ]`, and the chip that shows a key sequence being
//! typed.

use std::time::Duration;

use egui::{Align2, Rect, Response, Sense, StrokeKind, Vec2, vec2};

use crate::{
    icons, text,
    theme::{self, Tokens},
};

/// Shows `text` (and `key`, faint after it) on an inverted tooltip while
/// `response` is hovered.
pub fn tooltip(response: Response, label: &str, key: Option<&str>) -> Response {
    if label.is_empty() {
        return response;
    }
    let t = Tokens::get(&response.ctx);
    let (fill, color) = t.inverted();
    let mut tip = egui::Tooltip::for_enabled(&response);
    tip.popup = tip.popup.frame(
        egui::Frame::new()
            .fill(fill)
            .corner_radius(t.ctl_radius())
            .inner_margin(egui::Margin::symmetric(6, 3)),
    );
    tip.show(|ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            ui.label(
                egui::RichText::new(label)
                    .font(theme::mono(11.0))
                    .color(color),
            );
            if let Some(key) = key.filter(|key| !key.is_empty()) {
                ui.label(
                    egui::RichText::new(key)
                        .font(theme::mono(10.0))
                        .color(t.faint),
                );
            }
        });
    });
    response
}

/// Tooltips in the design's style for any response.
pub trait Tip {
    /// An inverted tooltip; replaces `on_hover_text`.
    fn tip(self, label: &str) -> Self;
    /// An inverted tooltip with its key.
    fn tip_key(self, label: &str, key: &str) -> Self;
}

impl Tip for Response {
    fn tip(self, label: &str) -> Self {
        tooltip(self, label, None)
    }

    fn tip_key(self, label: &str, key: &str) -> Self {
        tooltip(self, label, Some(key))
    }
}

/// How long a toast stays.
pub const TOAST_SECONDS: f64 = 6.0;

/// The kind of a toast, which picks its icon.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tone {
    #[default]
    Info,
    Ok,
    Err,
}

#[derive(Clone, Debug)]
struct Toast {
    message: String,
    tone: Tone,
    action: Option<String>,
}

/// Transient messages at the bottom centre, after pdfcraft's `toast`: the
/// latest shows for a few seconds, longer while hovered; a click on its
/// `[ action ]` is reported by [`Toasts::show`], a click elsewhere
/// dismisses it. After a pad the action shows X's glyph (frame 09b), and
/// X runs it.
#[derive(Default)]
pub struct Toasts {
    messages: Vec<Toast>,
    /// When the current message was first shown, in egui time.
    shown_at: Option<f64>,
    /// It has had its time and is fading out.
    leaving: bool,
}

impl Toasts {
    pub fn push(&mut self, message: impl Into<String>) {
        self.push_with(message, Tone::Info, None);
    }

    /// A message with a tone and an optional `[ action ]` (`undo`).
    pub fn push_with(&mut self, message: impl Into<String>, tone: Tone, action: Option<&str>) {
        self.messages.push(Toast {
            message: message.into(),
            tone,
            action: action.map(str::to_owned),
        });
        self.shown_at = None;
        self.leaving = false;
    }

    /// Whether the message showing has an action (X runs it then).
    pub fn has_action(&self) -> bool {
        !self.leaving
            && self
                .messages
                .last()
                .is_some_and(|toast| toast.action.is_some())
    }

    pub fn latest(&self) -> Option<&str> {
        self.messages.last().map(|toast| toast.message.as_str())
    }

    pub fn clear(&mut self) {
        self.messages.clear();
        self.shown_at = None;
        self.leaving = false;
    }

    /// Draws the latest message `lift` points above the bottom; returns the
    /// action clicked.
    pub fn show(&mut self, ctx: &egui::Context, lift: f32) -> Option<String> {
        let toast = self.messages.last().cloned()?;
        let now = ctx.input(|i| i.time);
        let shown_at = *self.shown_at.get_or_insert(now);
        // It fades out when its time is up, and follows the status bar
        // as it comes and goes.
        let shown = crate::motion::smooth_bool(
            ctx,
            egui::Id::unique("toast-shown"),
            !self.leaving,
            crate::motion::PANEL,
        );
        if self.leaving && shown == 0.0 {
            self.clear();
            return None;
        }
        let lift = crate::motion::smooth(
            ctx,
            egui::Id::unique("toast-lift"),
            lift,
            crate::motion::PANEL,
        );
        let t = Tokens::get(ctx);
        let screen = ctx.content_rect();
        let pad = crate::pad_glyph::input(ctx) == crate::pad_glyph::InputDevice::Pad;
        let mut action_clicked = toast.action.is_some()
            && !self.leaving
            && crate::pad_glyph::pressed_faces(ctx).contains(&crate::pad_glyph::Face::West);
        let area = egui::Area::new(egui::Id::unique("toast"))
            .order(egui::Order::Tooltip)
            .sense(Sense::click())
            .interactable(!self.leaving)
            .pivot(Align2::CENTER_BOTTOM)
            .fixed_pos(screen.center_bottom() - vec2(0.0, lift - 8.0 * (1.0 - shown)))
            .show(ctx, |ui| {
                ui.set_opacity(shown);
                egui::Frame::new()
                    .fill(t.raised)
                    .stroke(t.edge())
                    .corner_radius(t.pop_radius())
                    .inner_margin(egui::Margin::symmetric(12, 8))
                    .show(ui, |ui| {
                        ui.set_max_width((screen.width() - 64.0).clamp(1.0, 560.0));
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 10.0;
                            let (icon, color) = match toast.tone {
                                Tone::Info => ("info", t.info),
                                Tone::Ok => ("check", t.ok),
                                Tone::Err => ("circle-alert", t.err),
                            };
                            ui.add(icons::image(icon, 14.0, color));
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(&toast.message)
                                        .font(theme::mono(12.0))
                                        .color(t.text),
                                )
                                .wrap(),
                            );
                            if let Some(action) = &toast.action {
                                action_clicked |= if pad {
                                    let glyph = [crate::pad_glyph::Glyph::Face(
                                        crate::pad_glyph::Face::West,
                                    )];
                                    ui.add(
                                        crate::pad_glyph::Hint::new(&glyph, action)
                                            .color(t.accent_text),
                                    )
                                    .interact(Sense::click())
                                    .clicked()
                                } else {
                                    ui.add(crate::button::Button::bracket(action)).clicked()
                                };
                            }
                        });
                    });
            });
        let response = area.response;
        if action_clicked {
            self.clear();
            return toast.action;
        }
        if response.clicked() {
            self.leaving = true;
            return None;
        }
        if response.hovered() {
            // Hovering restarts the clock, so a long message can be read.
            self.shown_at = Some(now);
        } else if now - shown_at > TOAST_SECONDS {
            self.leaving = true;
        }
        ctx.request_repaint_after(Duration::from_millis(250));
        None
    }
}

/// A task's progress at the bottom right while it runs (component
/// `progress-toast`, frame 03h): an icon and title, a 2 px bar, a muted line
/// and `[ actions ]`, with `×` to dismiss. The caller keeps its state and
/// stops drawing it when done.
pub struct ProgressCard<'a> {
    pub id: &'a str,
    pub icon: &'a str,
    pub tone: Tone,
    pub title: &'a str,
    /// 0 to 1; the bar is empty without a fraction.
    pub fraction: Option<f32>,
    pub line: &'a str,
    pub actions: &'a [&'a str],
}

/// What a click on a [`ProgressCard`] asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProgressAnswer {
    Action(usize),
    Dismiss,
}

impl ProgressCard<'_> {
    /// Draws the card `lift` points above the bottom right.
    pub fn show(self, ctx: &egui::Context, lift: f32) -> Option<ProgressAnswer> {
        let t = Tokens::get(ctx);
        let screen = ctx.content_rect();
        let width = (screen.width() - 32.0).clamp(1.0, 440.0);
        let (icon_color, bar_color, line_color) = match self.tone {
            Tone::Info => (t.accent_text, t.accent, t.muted),
            Tone::Ok => (t.ok, t.ok, t.muted),
            Tone::Err => (t.err, t.err, t.err),
        };
        let mut answer = None;
        egui::Area::new(egui::Id::unique("progress-card").with(self.id))
            .order(egui::Order::Tooltip)
            .pivot(Align2::RIGHT_BOTTOM)
            .fixed_pos(screen.right_bottom() - vec2(16.0, lift))
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(t.raised)
                    .stroke(t.edge())
                    .corner_radius(t.pop_radius())
                    .inner_margin(egui::Margin::symmetric(12, 10))
                    .show(ui, |ui| {
                        ui.set_width(width - 24.0);
                        ui.spacing_mut().item_spacing.y = 8.0;
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 8.0;
                            ui.add(icons::image(self.icon, 14.0, icon_color));
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(self.title)
                                        .font(theme::mono(12.0))
                                        .color(t.text),
                                )
                                .truncate(),
                            );
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if icons::button(ui, "x", 14.0, false, "dismiss").clicked() {
                                        answer = Some(ProgressAnswer::Dismiss);
                                    }
                                },
                            );
                        });
                        let (rail, _) =
                            ui.allocate_exact_size(vec2(ui.available_width(), 2.0), Sense::hover());
                        ui.painter().rect_filled(rail, 0.0, t.rule_strong);
                        if let Some(fraction) = self.fraction {
                            let mut bar = rail;
                            bar.set_width(rail.width() * fraction.clamp(0.0, 1.0));
                            ui.painter().rect_filled(bar, 0.0, bar_color);
                        }
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 10.0;
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(self.line)
                                        .font(theme::mono(11.0))
                                        .color(line_color),
                                )
                                .truncate(),
                            );
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    for (index, action) in self.actions.iter().enumerate().rev() {
                                        if ui.add(crate::button::Button::bracket(action)).clicked()
                                        {
                                            answer = Some(ProgressAnswer::Action(index));
                                        }
                                    }
                                },
                            );
                        });
                    });
            });
        answer
    }
}

/// The keys of a sequence typed so far (`g`), with a block cursor, on an
/// accent-edged chip whose bottom left is at `at`; `hints` (what can
/// follow: `gg top · gt tab`) follow it in faint mono.
pub fn pending_keys(ctx: &egui::Context, keys: &str, hints: &str, at: egui::Pos2) {
    let t = Tokens::get(ctx);
    egui::Area::new(egui::Id::unique("pending-keys"))
        .order(egui::Order::Tooltip)
        .interactable(false)
        .pivot(Align2::LEFT_BOTTOM)
        .fixed_pos(at)
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                let font = theme::mono(12.0);
                let width = text::width(ui, keys, &font) + 4.0 + 7.0 + 16.0;
                let (rect, _) = ui.allocate_exact_size(vec2(width, 24.0), Sense::hover());
                ui.painter().rect(
                    rect,
                    t.ctl_radius(),
                    t.raised,
                    egui::Stroke::new(1.0, t.accent),
                    StrokeKind::Inside,
                );
                let galley = ui.painter().layout_no_wrap(keys.to_owned(), font, t.text);
                let text_width = galley.size().x;
                ui.painter().galley(
                    egui::pos2(rect.left() + 8.0, rect.center().y - galley.size().y / 2.0),
                    galley,
                    t.text,
                );
                let blink = (ui.input(|i| i.time) * 2.0) as i64 % 2 == 0;
                if blink {
                    ui.painter().rect_filled(
                        Rect::from_center_size(
                            egui::pos2(rect.left() + 8.0 + text_width + 4.0 + 3.5, rect.center().y),
                            vec2(7.0, 14.0),
                        ),
                        0,
                        t.accent,
                    );
                }
                if !hints.is_empty() {
                    ui.label(
                        egui::RichText::new(hints)
                            .font(theme::mono(10.0))
                            .color(t.faint),
                    );
                }
            });
            ui.ctx().request_repaint_after(Duration::from_millis(500));
        });
}

/// A modal scrim over the whole window.
pub fn scrim(ctx: &egui::Context, id: &str) -> Response {
    let t = Tokens::get(ctx);
    let screen = ctx.content_rect();
    egui::Area::new(egui::Id::unique(id))
        .order(egui::Order::Middle)
        .fixed_pos(screen.min)
        .show(ctx, |ui| {
            let (rect, response) = ui.allocate_exact_size(screen.size(), Sense::click());
            ui.painter().rect_filled(rect, 0, t.scrim);
            response
        })
        .inner
}

/// A scrim that fades in and out with `shown` (0..=1); while it shows, a
/// click on it is its response.
pub fn fading_scrim(ctx: &egui::Context, id: &str, shown: f32) -> Option<Response> {
    if shown <= 0.0 {
        return None;
    }
    let t = Tokens::get(ctx);
    let screen = ctx.content_rect();
    let response = egui::Area::new(egui::Id::unique(id))
        .order(egui::Order::Middle)
        .fade_in(false)
        .interactable(shown >= 1.0)
        .fixed_pos(screen.min)
        .show(ctx, |ui| {
            let (rect, response) = ui.allocate_exact_size(screen.size(), Sense::click());
            ui.painter()
                .rect_filled(rect, 0, t.scrim.gamma_multiply(shown));
            response
        })
        .inner;
    Some(response)
}

/// An icon sized for inline use in overlays.
pub fn icon_rect(center: egui::Pos2) -> Rect {
    Rect::from_center_size(center, Vec2::splat(14.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(ctx: &egui::Context, time: f64, toasts: &mut Toasts) {
        let input = egui::RawInput {
            time: Some(time),
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| {
            toasts.show(ui.ctx(), 28.0);
        });
        output.textures_delta.clear();
    }

    #[test]
    fn a_toast_leaves_after_a_few_seconds() {
        let ctx = egui::Context::default();
        ctx.set_fonts(crate::fonts::embedded());
        let mut toasts = Toasts::default();
        toasts.push_with("highlight saved", Tone::Ok, Some("undo"));
        run(&ctx, 1.0, &mut toasts);
        assert_eq!(toasts.latest(), Some("highlight saved"));
        run(&ctx, 1.0 + TOAST_SECONDS - 0.5, &mut toasts);
        assert!(toasts.latest().is_some());
        let mut time = 1.0 + TOAST_SECONDS + 0.5;
        run(&ctx, time, &mut toasts);
        assert!(toasts.latest().is_some(), "fading out");
        for _ in 0..30 {
            time += 1.0 / 60.0;
            run(&ctx, time, &mut toasts);
        }
        assert_eq!(toasts.latest(), None);
    }
}
