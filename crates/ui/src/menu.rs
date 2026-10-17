//! Menus (pen components `menu` and `menu-item`): a `raised` popover with
//! a `rule-strong` edge and radius 4; items are an icon, a label and a key,
//! highlighted with the hover fill; separators are hairlines. The style is
//! applied to egui's own menus through [`style`], so context menus and
//! combo boxes match.

use egui::{Rect, Response, Sense, Ui, Vec2, vec2};

use crate::{
    icons, key, text,
    theme::{self, Tokens},
};

/// An item: icon, label and key, and optionally a line of detail under
/// the label. Returns its response.
#[must_use = "call `.show(ui)`"]
pub struct Item<'a> {
    label: &'a str,
    detail: Option<&'a str>,
    icon: Option<&'a str>,
    key: Option<&'a str>,
    checked: Option<bool>,
    danger: bool,
    width: Option<f32>,
}

impl<'a> Item<'a> {
    pub fn new(label: &'a str) -> Self {
        Self {
            label,
            detail: None,
            icon: None,
            key: None,
            checked: None,
            danger: false,
            width: None,
        }
    }

    /// A second line under the label, cut to the row's width: the row
    /// keeps one height whatever the text or the pointer.
    pub fn detail(mut self, detail: &'a str) -> Self {
        self.detail = Some(detail);
        self
    }

    pub fn icon(mut self, icon: impl Into<Option<&'a str>>) -> Self {
        self.icon = icon.into();
        self
    }

    pub fn key(mut self, key: impl Into<Option<&'a str>>) -> Self {
        self.key = key.into().filter(|key| !key.is_empty());
        self
    }

    /// A check mark at the right when `Some(true)`, room for one when
    /// `Some(false)`.
    pub fn checked(mut self, checked: impl Into<Option<bool>>) -> Self {
        self.checked = checked.into();
        self
    }

    /// In the error colour (delete, discard).
    pub fn danger(mut self, danger: bool) -> Self {
        self.danger = danger;
        self
    }

    pub fn width(mut self, width: f32) -> Self {
        self.width = Some(width);
        self
    }

    pub fn show(self, ui: &mut Ui) -> Response {
        let t = Tokens::get(ui.ctx());
        let font = theme::mono(12.0);
        let natural = 8.0
            + if self.icon.is_some() { 21.0 } else { 0.0 }
            + text::width(ui, self.label, &font)
            + self
                .key
                .map_or(0.0, |key| key::size(ui, &key::display(key)).x + 16.0)
            + if self.checked.is_some() { 22.0 } else { 0.0 }
            + 8.0;
        let width = self
            .width
            .unwrap_or_else(|| natural.max(ui.available_width()).max(180.0));
        let height = if self.detail.is_some() { 40.0 } else { 26.0 };
        let (rect, response) = ui.allocate_exact_size(vec2(width, t.row(height)), Sense::click());
        let enabled = ui.is_enabled();
        response.widget_info(|| match self.checked {
            Some(checked) => {
                egui::WidgetInfo::selected(egui::WidgetType::Checkbox, enabled, checked, self.label)
            }
            None => egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, self.label),
        });
        let hovered = (response.hovered() || response.has_focus()) && enabled;
        let fill = if hovered {
            t.hover
        } else {
            egui::Color32::TRANSPARENT
        };
        ui.painter().rect_filled(
            rect,
            t.ctl_radius(),
            crate::motion::color(ui.ctx(), response.id, fill),
        );
        let (icon_color, color) = if !enabled {
            (t.faint, t.faint)
        } else if self.danger {
            (t.err, t.err)
        } else if hovered {
            (t.text, t.text)
        } else {
            (t.muted, t.text)
        };
        let y = rect.center().y;
        let mut x = rect.left() + 8.0;
        if let Some(icon) = self.icon {
            icons::paint(
                ui,
                Rect::from_center_size(egui::pos2(x + 6.5, y), Vec2::splat(13.0)),
                icon,
                13.0,
                icon_color,
            );
            x += 21.0;
        }
        let mut right = rect.right() - 8.0;
        if let Some(checked) = self.checked {
            if checked {
                icons::paint(
                    ui,
                    Rect::from_center_size(egui::pos2(right - 7.0, y), Vec2::splat(13.0)),
                    "check",
                    13.0,
                    t.accent_text,
                );
            }
            right -= 22.0;
        }
        if let Some(key) = self.key {
            let key = key::display(key);
            ui.painter().text(
                egui::pos2(right, y),
                egui::Align2::RIGHT_CENTER,
                &key,
                theme::mono(10.0),
                t.faint,
            );
            right -= text::width(ui, &key, &theme::mono(10.0)) + 12.0;
        }
        // Menus write their entries in lower case, as the design does.
        let label_y = if self.detail.is_some() { y - 8.0 } else { y };
        text::row(
            ui,
            egui::pos2(x, label_y),
            self.label.to_lowercase(),
            font,
            color,
            right - x,
        );
        if let Some(detail) = self.detail {
            text::row(
                ui,
                egui::pos2(x, y + 8.0),
                detail,
                theme::mono(10.0),
                if enabled { t.muted } else { t.faint },
                right - x,
            );
        }
        response
    }
}

impl egui::Widget for Item<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        self.show(ui)
    }
}

/// A hairline between groups of items.
pub fn separator(ui: &mut Ui) {
    let t = Tokens::get(ui.ctx());
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 9.0), Sense::hover());
    ui.painter()
        .hline(rect.x_range(), rect.center().y, t.hairline());
}

/// A faint group title inside a menu (`── pages ──`).
pub fn group(ui: &mut Ui, title: &str) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 20.0), Sense::hover());
    crate::motif::paint_rule_title(ui, rect.shrink2(vec2(8.0, 0.0)), title, None, false);
}

/// Styles `ui` for menu contents: tight spacing, item-sized buttons.
pub fn style(ui: &mut Ui) {
    let spacing = ui.spacing_mut();
    spacing.item_spacing = vec2(0.0, 0.0);
    spacing.button_padding = vec2(8.0, 4.0);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bug where a picker's rows grew and shrank under the pointer:
    /// rows with a detail keep one height, hovered or not.
    #[test]
    fn rows_with_a_detail_keep_their_height_under_the_pointer() {
        let ctx = egui::Context::default();
        ctx.set_fonts(crate::fonts::embedded());
        let detail = "a long description that would wrap in a narrow menu, and then some more";
        let mut heights = Vec::new();
        for pointer in [None, Some(egui::pos2(20.0, 12.0))] {
            let input = egui::RawInput {
                events: pointer.map(egui::Event::PointerMoved).into_iter().collect(),
                ..Default::default()
            };
            let mut output = ctx.run_ui(input, |ui| {
                ui.set_width(200.0);
                for label in ["tonal spot", "expressive"] {
                    let response = Item::new(label).detail(detail).checked(false).show(ui);
                    heights.push(response.rect.height());
                }
            });
            output.textures_delta.clear();
        }
        assert!(
            heights.windows(2).all(|pair| pair[0] == pair[1]),
            "{heights:?}"
        );
    }
}
