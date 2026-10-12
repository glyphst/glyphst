//! Small widgets on the design tokens, after pdfcraft's `widgets.rs`: pill,
//! ghost and search-box buttons, section titles, key chips, segmented
//! choices, card frames and toasts.

use std::time::Duration;

use egui::{Align2, Color32, CornerRadius, Rect, Response, Sense, Stroke, StrokeKind, vec2};

use crate::{
    icons,
    theme::{self, Tokens},
};

/// Icons the app names directly rather than through a command, section or
/// sidebar tab; a test checks that each exists.
#[cfg(test)]
pub const ICONS_IN_USE: &[&str] = &[
    "house",
    "file-text",
    "book-open",
    "book-open-text",
    "plus",
    "x",
    "chevron-down",
    "chevron-left",
    "chevron-right",
    "ellipsis",
    "search",
    "send",
    "message-square-plus",
    "clock",
    "copy",
    "trash-2",
    "check",
    "eye",
    "eye-off",
    "rotate-ccw",
    "refresh-cw",
    "download",
    "file-down",
    "pin",
    "triangle-alert",
    "info",
    "gamepad-2",
    "library",
    "pen-line",
    "highlighter",
    "sticky-note",
];

fn text_width(ui: &egui::Ui, text: &str, font: &egui::FontId) -> f32 {
    ui.painter()
        .layout_no_wrap(text.to_owned(), font.clone(), Color32::WHITE)
        .size()
        .x
}

/// A rounded pill; `primary` fills it with the accent. With an icon, the
/// icon comes first.
pub fn pill_button(ui: &mut egui::Ui, icon: Option<&str>, label: &str, primary: bool) -> Response {
    let t = Tokens::get(ui.ctx());
    let font = theme::medium(12.5);
    let icon_room = if icon.is_some() { 22.0 } else { 0.0 };
    let width = text_width(ui, label, &font) + 26.0 + icon_room;
    let (rect, response) = ui.allocate_exact_size(vec2(width, 30.0), Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), label)
    });
    let enabled = ui.is_enabled();
    let (fill, stroke, text) = if primary {
        let fill = if response.hovered() && enabled {
            t.accent_text
        } else {
            t.accent
        };
        (fill, Stroke::NONE, Color32::WHITE)
    } else {
        let fill = if response.hovered() && enabled {
            t.hover
        } else {
            t.card
        };
        (fill, Stroke::new(1.0, t.border), t.text)
    };
    let text = if enabled {
        text
    } else {
        text.gamma_multiply(0.5)
    };
    ui.painter().rect(
        rect,
        CornerRadius::same(15),
        fill,
        stroke,
        StrokeKind::Inside,
    );
    let mut x = rect.left() + 13.0;
    if let Some(icon) = icon {
        icons::paint(
            ui,
            Rect::from_center_size(egui::pos2(x + 8.0, rect.center().y), vec2(16.0, 16.0)),
            icon,
            15.0,
            text,
        );
        x += icon_room;
    }
    ui.painter().text(
        egui::pos2(x, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        font,
        text,
    );
    response
}

/// Icon and label, transparent until hovered.
pub fn ghost_button(ui: &mut egui::Ui, icon: &str, label: &str) -> Response {
    let t = Tokens::get(ui.ctx());
    let font = theme::medium(13.0);
    let width = text_width(ui, label, &font) + 38.0;
    let (rect, response) = ui.allocate_exact_size(vec2(width, 30.0), Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), label)
    });
    if response.hovered() && ui.is_enabled() {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(t.radius), t.hover);
    }
    let color = if ui.is_enabled() {
        t.text
    } else {
        t.text_faint
    };
    icons::paint(
        ui,
        Rect::from_min_size(rect.min + vec2(6.0, 6.0), vec2(18.0, 18.0)),
        icon,
        17.0,
        if ui.is_enabled() {
            t.icon
        } else {
            t.text_faint
        },
    );
    ui.painter().text(
        rect.left_center() + vec2(30.0, 0.0),
        Align2::LEFT_CENTER,
        label,
        font,
        color,
    );
    response
}

/// A search-field lookalike that opens the command palette, with its key at
/// the right. The placeholder is cut with "…" before it reaches the key.
pub fn search_box(ui: &mut egui::Ui, placeholder: &str, shortcut: &str, width: f32) -> Response {
    let t = Tokens::get(ui.ctx());
    let (rect, response) = ui.allocate_exact_size(vec2(width, 30.0), Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), placeholder)
    });
    let fill = if response.hovered() { t.hover } else { t.field };
    ui.painter().rect(
        rect,
        CornerRadius::same(15),
        fill,
        Stroke::new(1.0, t.border),
        StrokeKind::Inside,
    );
    icons::paint(
        ui,
        Rect::from_min_size(rect.min + vec2(10.0, 7.0), vec2(16.0, 16.0)),
        "search",
        15.0,
        t.text_muted,
    );
    let shortcut =
        ui.painter()
            .layout_no_wrap(shortcut.to_owned(), theme::regular(11.5), t.text_faint);
    let shortcut_pos =
        rect.right_center() - vec2(12.0 + shortcut.size().x, shortcut.size().y * 0.5);
    let text_right = (shortcut_pos.x - 10.0).max(rect.left());
    let text_left = (rect.left() + 34.0).min(text_right);
    row_text(
        ui,
        egui::pos2(text_left, rect.center().y),
        placeholder,
        theme::regular(13.0),
        t.text_faint,
        text_right - text_left,
    );
    ui.painter()
        .with_clip_rect(rect)
        .galley(shortcut_pos, shortcut, t.text_faint);
    response.on_hover_cursor(egui::CursorIcon::Text)
}

/// A small upper-case title over a group of controls or rows.
pub fn section_title(ui: &mut egui::Ui, text: &str) {
    let t = Tokens::get(ui.ctx());
    ui.add_space(10.0);
    ui.label(
        egui::RichText::new(text.to_uppercase())
            .font(theme::semibold(10.5))
            .color(t.text_faint)
            .extra_letter_spacing(0.6),
    );
    ui.add_space(2.0);
}

/// One line of `text`, left-aligned and vertically centred on `pos`, cut
/// with "…" to at most `max_width`.
pub fn row_text(
    ui: &egui::Ui,
    pos: egui::Pos2,
    text: impl Into<String>,
    font: egui::FontId,
    color: Color32,
    max_width: f32,
) {
    let mut job = egui::text::LayoutJob::simple_singleline(text.into(), font, color);
    job.wrap = egui::text::TextWrapping::truncate_at_width(max_width.max(0.0));
    let galley = ui.painter().layout_job(job);
    let rect = Align2::LEFT_CENTER.anchor_size(pos, galley.size());
    ui.painter().galley(rect.min, galley, color);
}

/// The size of a key chip for `label`.
pub fn kbd_size(ui: &egui::Ui, label: &str) -> egui::Vec2 {
    vec2(text_width(ui, label, &theme::medium(11.0)) + 12.0, 20.0)
}

/// Paints a key chip ("Ctrl+K") at `rect`.
pub fn paint_kbd(ui: &egui::Ui, rect: Rect, label: &str) {
    let t = Tokens::get(ui.ctx());
    ui.painter().rect(
        rect,
        CornerRadius::same(5),
        t.hover,
        Stroke::new(1.0, t.border),
        StrokeKind::Inside,
    );
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        label,
        theme::medium(11.0),
        t.text_muted,
    );
}

/// One choice of several, drawn as a row of joined buttons. Returns whether
/// the choice changed.
pub fn segmented<T: PartialEq + Copy>(
    ui: &mut egui::Ui,
    value: &mut T,
    options: &[(T, &str)],
) -> bool {
    let t = Tokens::get(ui.ctx());
    let font = theme::medium(12.5);
    let widths: Vec<f32> = options
        .iter()
        .map(|(_, label)| text_width(ui, label, &font) + 24.0)
        .collect();
    let (rect, _) =
        ui.allocate_exact_size(vec2(widths.iter().sum::<f32>() + 4.0, 30.0), Sense::hover());
    ui.painter().rect(
        rect,
        CornerRadius::same(8),
        t.hover,
        Stroke::new(1.0, t.divider),
        StrokeKind::Inside,
    );
    let mut x = rect.left() + 2.0;
    let mut changed = false;
    for ((option, label), width) in options.iter().zip(widths) {
        let cell = Rect::from_min_size(egui::pos2(x, rect.top() + 2.0), vec2(width, 26.0));
        x += width;
        let response = ui.interact(cell, ui.id().with(("segment", label)), Sense::click());
        let selected = *value == *option;
        response.widget_info(|| {
            egui::WidgetInfo::selected(
                egui::WidgetType::RadioButton,
                ui.is_enabled(),
                selected,
                *label,
            )
        });
        if selected {
            ui.painter().rect(
                cell,
                CornerRadius::same(6),
                t.card,
                Stroke::new(1.0, t.border),
                StrokeKind::Inside,
            );
        } else if response.hovered() {
            ui.painter()
                .rect_filled(cell, CornerRadius::same(6), t.pressed);
        }
        ui.painter().text(
            cell.center(),
            Align2::CENTER_CENTER,
            *label,
            font.clone(),
            if selected { t.text } else { t.text_muted },
        );
        if response.clicked() && !selected {
            *value = *option;
            changed = true;
        }
    }
    changed
}

/// The frame of floating cards: popovers, the find bar, drawers.
pub fn card_frame(ctx: &egui::Context) -> egui::Frame {
    let t = Tokens::get(ctx);
    egui::Frame::new()
        .fill(t.card)
        .stroke(Stroke::new(1.0, t.border))
        .corner_radius(CornerRadius::same(10))
        .shadow(t.card_shadow())
        .inner_margin(egui::Margin::same(12))
}

/// A row that fills the width, highlighted on hover (and when `selected`),
/// for lists in panels. Returns its rect and response.
pub fn list_row(ui: &mut egui::Ui, height: f32, selected: bool) -> (Rect, Response) {
    let t = Tokens::get(ui.ctx());
    let (rect, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::click());
    if selected {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(t.radius), t.accent_soft);
    } else if response.hovered() {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(t.radius), t.hover);
    }
    (rect, response)
}

/// How long a toast stays.
const TOAST_SECONDS: f64 = 6.0;

/// Transient messages at the bottom centre, after pdfcraft's `toast`: the
/// latest shows on an inverted card for a few seconds, longer while hovered,
/// and a click dismisses it.
#[derive(Default)]
pub struct Toasts {
    messages: Vec<String>,
    /// When the current message was first shown, in egui time.
    shown_at: Option<f64>,
}

impl Toasts {
    pub fn push(&mut self, message: impl Into<String>) {
        self.messages.push(message.into());
        self.shown_at = None;
    }

    #[cfg(test)]
    pub fn latest(&self) -> Option<&str> {
        self.messages.last().map(String::as_str)
    }

    pub fn clear(&mut self) {
        self.messages.clear();
        self.shown_at = None;
    }

    /// Draws the latest message `lift` points above the bottom.
    pub fn show(&mut self, ctx: &egui::Context, lift: f32) {
        let Some(message) = self.messages.last().cloned() else {
            return;
        };
        let now = ctx.input(|i| i.time);
        let shown_at = *self.shown_at.get_or_insert(now);
        let t = Tokens::get(ctx);
        let (fill, text) = t.toast();
        let screen = ctx.content_rect();
        let area = egui::Area::new(egui::Id::new("toast"))
            .order(egui::Order::Tooltip)
            .pivot(Align2::CENTER_BOTTOM)
            .fixed_pos(screen.center_bottom() - vec2(0.0, lift))
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(fill)
                    .corner_radius(CornerRadius::same(8))
                    .shadow(t.popup_shadow())
                    .inner_margin(egui::Margin::symmetric(16, 10))
                    .show(ui, |ui| {
                        ui.set_max_width((screen.width() - 64.0).clamp(1.0, 560.0));
                        ui.add(egui::Label::new(egui::RichText::new(message).color(text)).wrap());
                    });
            });
        let response = area.response.interact(Sense::click());
        if response.clicked() {
            self.clear();
            return;
        }
        if response.hovered() {
            // Hovering restarts the clock, so a long message can be read.
            self.shown_at = Some(now);
        } else if now - shown_at > TOAST_SECONDS {
            self.clear();
            return;
        }
        ctx.request_repaint_after(Duration::from_millis(250));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(ctx: &egui::Context, time: f64, toasts: &mut Toasts) {
        let input = egui::RawInput {
            time: Some(time),
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| toasts.show(ui.ctx(), 28.0));
        output.textures_delta.clear();
    }

    #[test]
    fn a_toast_leaves_after_a_few_seconds() {
        let ctx = egui::Context::default();
        let mut toasts = Toasts::default();
        toasts.push("Could not save");
        run(&ctx, 1.0, &mut toasts);
        assert_eq!(toasts.latest(), Some("Could not save"));
        run(&ctx, 1.0 + TOAST_SECONDS - 0.5, &mut toasts);
        assert!(toasts.latest().is_some());
        run(&ctx, 1.0 + TOAST_SECONDS + 0.5, &mut toasts);
        assert_eq!(toasts.latest(), None);
    }

    #[test]
    fn segmented_choice_changes_on_click() {
        let ctx = egui::Context::default();
        ctx.set_fonts(crate::fonts::embedded());
        let mut value = 0;
        let mut changed = false;
        let mut frame = |events: Vec<egui::Event>, value: &mut i32| {
            let input = egui::RawInput {
                events,
                ..Default::default()
            };
            let mut output = ctx.run_ui(input, |ui| {
                changed |= segmented(ui, value, &[(0, "System"), (1, "Light"), (2, "Dark")]);
            });
            output.textures_delta.clear();
        };
        frame(Vec::new(), &mut value);
        // Well right of the first cell, which is about 70 points wide.
        let pos = egui::pos2(150.0, 15.0);
        let press = |pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        frame(
            vec![egui::Event::PointerMoved(pos), press(true)],
            &mut value,
        );
        frame(vec![press(false)], &mut value);
        assert!(changed);
        assert_ne!(value, 0);
    }
}
