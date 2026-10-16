//! The presentation's toolbox (pen component `toolbox`, frames 01 and
//! 02h): one row of 28-point icon buttons in groups split by hairlines, on
//! `surface` with a `rule` edge. A tool that is on gets the `accent-soft`
//! fill.

use egui::{Response, Sense, Stroke, StrokeKind, Ui, Vec2, vec2};

use crate::{icons, overlay, theme::Tokens};

pub const BUTTON: f32 = 28.0;

/// The bar's frame; lay buttons out inside it with [`button`] and
/// [`separator`].
pub fn frame(ctx: &egui::Context) -> egui::Frame {
    let t = Tokens::get(ctx);
    egui::Frame::new()
        .fill(t.surface)
        .stroke(t.hairline())
        .corner_radius(t.pop_radius())
        .inner_margin(egui::Margin::same(4))
}

/// An icon button of the bar; `on` marks the tool in use.
pub fn button(ui: &mut Ui, icon: &str, on: bool, tip: &str, key: Option<&str>) -> Response {
    let t = Tokens::get(ui.ctx());
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(BUTTON), Sense::click());
    let hovered = response.hovered();
    let fill = if on {
        t.accent_soft
    } else if response.is_pointer_button_down_on() {
        t.rule_strong
    } else if hovered {
        t.hover
    } else {
        egui::Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, t.ctl_radius(), fill);
    if response.has_focus() {
        ui.painter().rect_stroke(
            rect,
            t.ctl_radius(),
            Stroke::new(1.0, t.accent),
            StrokeKind::Inside,
        );
    }
    let tint = if on {
        t.accent_text
    } else if hovered {
        t.text
    } else {
        t.muted
    };
    icons::paint(ui, rect, icon, 16.0, tint);
    overlay::tooltip(response, tip, key)
}

/// The hairline between groups of buttons.
pub fn separator(ui: &mut Ui) {
    let t = Tokens::get(ui.ctx());
    let (rect, _) = ui.allocate_exact_size(vec2(1.0, 16.0), Sense::hover());
    ui.painter().rect_filled(rect, 0, t.rule);
}

/// The gallery's sample: the bar with the pen on.
pub fn sample(ui: &mut Ui) {
    let groups: [&[(&str, &str)]; 4] = [
        &[
            ("chevron-left", "previous page"),
            ("chevron-right", "next page"),
        ],
        &[
            ("mouse-pointer-2", "show cursor"),
            ("pencil", "end draw"),
            ("highlighter", "highlight"),
            ("eraser", "eraser"),
        ],
        &[
            ("undo-2", "undo stroke"),
            ("trash-2", "clear page"),
            ("square", "black screen"),
        ],
        &[("ellipsis", "commands")],
    ];
    frame(ui.ctx()).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            for (index, group) in groups.iter().enumerate() {
                if index > 0 {
                    separator(ui);
                }
                for (icon, tip) in group.iter() {
                    button(ui, icon, *icon == "pencil", tip, None);
                }
            }
        });
    });
}
