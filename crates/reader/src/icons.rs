//! Vector icons, after pdfcraft's `icons.rs`: embedded Lucide SVGs recoloured
//! to white, rasterized by egui_extras at the on-screen pixel size and tinted
//! per use. Unicode glyphs are never used as icons.

use std::sync::OnceLock;

use craft_ui::buttons::{ButtonVisuals, IconButton, IconButtonStyle};
use craft_ui::icons::{SvgIconSet, SvgStyle};
use egui::{Color32, Rect, Response, Stroke, Vec2};

use crate::{icon_data::ICONS, theme::Tokens};

fn white_icons() -> &'static SvgIconSet {
    static SET: OnceLock<SvgIconSet> = OnceLock::new();
    SET.get_or_init(|| {
        SvgIconSet::new(
            ICONS,
            SvgStyle {
                current_color: "#ffffff",
                stroke_width: Some("1.75"),
            },
            "bytes://glyphst-icons/",
            "square",
        )
    })
}

#[cfg(test)]
pub fn exists(name: &str) -> bool {
    white_icons().contains(name)
}

pub fn image(name: &str, size: f32, tint: Color32) -> egui::Image<'static> {
    egui::Image::new(white_icons().source(name))
        .fit_to_exact_size(Vec2::splat(size))
        .tint(tint)
}

/// Paints `name` centred in `rect`.
pub fn paint(ui: &egui::Ui, rect: Rect, name: &str, size: f32, tint: Color32) {
    image(name, size, tint).paint_at(ui, Rect::from_center_size(rect.center(), Vec2::splat(size)));
}

/// A square icon button, transparent until hovered; `selected` gets the
/// accent treatment. `tooltip` is also its accessible name.
pub fn button(ui: &mut egui::Ui, name: &str, size: f32, selected: bool, tooltip: &str) -> Response {
    let t = Tokens::get(ui.ctx());
    let icon = if ui.is_enabled() {
        t.icon
    } else {
        t.text_faint
    };
    let style = IconButtonStyle {
        normal: ButtonVisuals {
            fill: Color32::TRANSPARENT,
            stroke: Stroke::NONE,
            icon,
        },
        hovered: ButtonVisuals {
            fill: t.hover,
            stroke: Stroke::NONE,
            icon,
        },
        pressed: Some(ButtonVisuals {
            fill: t.pressed,
            stroke: Stroke::NONE,
            icon,
        }),
        selected: ButtonVisuals {
            fill: t.accent_soft,
            stroke: Stroke::NONE,
            icon: t.accent_text,
        },
        corner_radius: t.radius.into(),
        focus_stroke: Stroke::new(1.0, t.accent),
    };
    let label = if tooltip.is_empty() { name } else { tooltip };
    let response = IconButton::new(label, Vec2::splat(size), &style)
        .selected(selected)
        .show(ui, |ui, rect, tint| {
            paint(ui, rect, name, (size * 0.55).round(), tint);
        });
    if tooltip.is_empty() {
        response
    } else {
        response.on_hover_text(tooltip)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{commands::Command, settings_window::Section, sidebar::SidebarTab};

    #[test]
    fn every_icon_in_use_exists() {
        let names = Command::ALL
            .iter()
            .map(|command| command.icon())
            .chain(Section::ALL.iter().map(|section| section.icon()))
            .chain(SidebarTab::ALL.iter().map(|tab| tab.icon()))
            .chain(crate::widgets::ICONS_IN_USE.iter().copied());
        for name in names {
            assert!(exists(name), "missing icon {name}");
        }
    }
}
