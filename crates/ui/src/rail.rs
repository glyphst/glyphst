//! The icon rail (pen components `rail-btn` and `rail-btn/active`): a 40pt
//! column of 36pt buttons; the active one is on the hover fill with an
//! accent edge at its left, others faint.

use egui::{Rect, Response, Sense, Ui, Vec2, vec2};

use crate::{badge::Badge, icons, overlay::Tip, theme::Tokens};

/// The rail's width.
pub const WIDTH: f32 = 40.0;

/// One rail button; `badge` is drawn at its top right.
pub fn button(
    ui: &mut Ui,
    icon: &str,
    active: bool,
    tooltip: &str,
    badge: Option<&str>,
) -> Response {
    let t = Tokens::get(ui.ctx());
    let (rect, response) = ui.allocate_exact_size(vec2(WIDTH, 36.0), Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Button, ui.is_enabled(), active, tooltip)
    });
    let hovered = response.hovered();
    if active {
        ui.painter().rect_filled(rect, 0, t.hover);
        ui.painter().rect_filled(
            Rect::from_min_size(rect.min, vec2(2.0, rect.height())),
            0,
            t.accent,
        );
    } else if hovered {
        ui.painter().rect_filled(rect, 0, t.hover);
    }
    let color = if active || hovered { t.text } else { t.faint };
    icons::paint(
        ui,
        Rect::from_center_size(rect.center(), Vec2::splat(16.0)),
        icon,
        16.0,
        color,
    );
    if let Some(badge) = badge.filter(|badge| !badge.is_empty()) {
        let badge = Badge(badge);
        let size = badge.size(ui);
        badge.paint(
            ui,
            Rect::from_min_size(
                egui::pos2(rect.right() - size.x - 3.0, rect.top() + 3.0),
                size,
            ),
        );
    }
    response.tip(tooltip)
}
