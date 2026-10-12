//! Design tokens and egui style, following pdfcraft's `theme.rs`: a
//! System/Light/Dark preference, one accent, white pages in both modes.
//! Widgets get their state colours through craft-ui's `WidgetPalette`.

use craft_ui::theme::WidgetPalette;
use egui::{Color32, CornerRadius, Stroke, Visuals};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThemeKind {
    #[default]
    Light,
    Dark,
}

/// The saved choice, independent of the colours currently drawn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThemePreference {
    #[default]
    System,
    Light,
    Dark,
}

impl ThemePreference {
    pub const ALL: [Self; 3] = [Self::System, Self::Light, Self::Dark];

    pub fn label(self) -> &'static str {
        match self {
            Self::System => "System",
            Self::Light => "Light",
            Self::Dark => "Dark",
        }
    }

    /// The colours to draw. `system` is the desktop's answer, `None` when
    /// it gives none; `fallback` is what the app last drew.
    pub fn resolve(self, system: Option<egui::Theme>, fallback: ThemeKind) -> ThemeKind {
        match self {
            Self::Light => ThemeKind::Light,
            Self::Dark => ThemeKind::Dark,
            Self::System => match system {
                Some(egui::Theme::Dark) => ThemeKind::Dark,
                Some(egui::Theme::Light) => ThemeKind::Light,
                None => fallback,
            },
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tokens {
    pub kind: ThemeKind,
    /// Tab strip.
    pub titlebar: Color32,
    /// Panels and bars.
    pub chrome: Color32,
    /// The document area behind pages.
    pub pasteboard: Color32,
    pub page: Color32,
    pub card: Color32,
    pub border: Color32,
    pub text: Color32,
    pub text_muted: Color32,
    pub hover: Color32,
    pub pressed: Color32,
    pub selected: Color32,
    pub accent: Color32,
    pub field: Color32,
    pub radius: u8,
}

impl Tokens {
    pub fn for_kind(kind: ThemeKind) -> Self {
        match kind {
            ThemeKind::Light => Self {
                kind,
                titlebar: Color32::from_rgb(0xE9, 0xE9, 0xEB),
                chrome: Color32::from_rgb(0xFF, 0xFF, 0xFF),
                pasteboard: Color32::from_rgb(0xE4, 0xE4, 0xE8),
                page: Color32::WHITE,
                card: Color32::from_rgb(0xF6, 0xF6, 0xF8),
                border: Color32::from_rgb(0xDA, 0xDA, 0xDE),
                text: Color32::from_rgb(0x22, 0x22, 0x26),
                text_muted: Color32::from_rgb(0x5E, 0x5E, 0x66),
                hover: Color32::from_rgb(0xF0, 0xF0, 0xF3),
                pressed: Color32::from_rgb(0xE4, 0xE4, 0xE9),
                selected: Color32::from_rgb(0xE6, 0xEE, 0xFD),
                accent: Color32::from_rgb(0x1B, 0x63, 0xE0),
                field: Color32::WHITE,
                radius: 6,
            },
            ThemeKind::Dark => Self {
                kind,
                titlebar: Color32::from_rgb(0x1B, 0x1B, 0x1E),
                chrome: Color32::from_rgb(0x26, 0x26, 0x2A),
                pasteboard: Color32::from_rgb(0x19, 0x19, 0x1C),
                page: Color32::WHITE,
                card: Color32::from_rgb(0x2E, 0x2E, 0x33),
                border: Color32::from_rgb(0x3C, 0x3C, 0x43),
                text: Color32::from_rgb(0xEC, 0xEC, 0xEF),
                text_muted: Color32::from_rgb(0xAE, 0xAE, 0xB6),
                hover: Color32::from_rgb(0x34, 0x34, 0x3A),
                pressed: Color32::from_rgb(0x3E, 0x3E, 0x45),
                selected: Color32::from_rgb(0x23, 0x3A, 0x63),
                accent: Color32::from_rgb(0x4B, 0x8B, 0xF5),
                field: Color32::from_rgb(0x1E, 0x1E, 0x22),
                radius: 6,
            },
        }
    }

    pub fn get(ctx: &egui::Context) -> Self {
        ctx.data(|data| data.get_temp::<Tokens>(egui::Id::new(TOKENS_ID)))
            .unwrap_or_else(|| Self::for_kind(ThemeKind::Light))
    }

    /// RGB of the pasteboard in 0..=1, for the solid background effect.
    pub fn pasteboard_rgb(&self) -> [f32; 3] {
        let [r, g, b, _] = self.pasteboard.to_array();
        [r, g, b].map(|channel| f32::from(channel) / 255.0)
    }
}

const TOKENS_ID: &str = "glyphst-reader-theme";

/// Applies the theme's tokens to egui's style.
pub fn apply(ctx: &egui::Context, kind: ThemeKind) {
    let tokens = Tokens::for_kind(kind);
    let mut visuals = match kind {
        ThemeKind::Light => Visuals::light(),
        ThemeKind::Dark => Visuals::dark(),
    };
    visuals.panel_fill = tokens.chrome;
    visuals.window_fill = tokens.chrome;
    visuals.extreme_bg_color = tokens.field;
    visuals.faint_bg_color = tokens.card;
    visuals.override_text_color = Some(tokens.text);
    visuals.hyperlink_color = tokens.accent;
    visuals.selection.bg_fill = tokens.selected;
    visuals.selection.stroke = Stroke::new(1.0, tokens.accent);
    visuals.window_stroke = Stroke::new(1.0, tokens.border);
    let radius = CornerRadius::same(tokens.radius);
    visuals.window_corner_radius = radius;
    visuals.menu_corner_radius = radius;
    WidgetPalette {
        noninteractive: tokens.chrome,
        inactive: tokens.card,
        hovered: tokens.hover,
        active: tokens.pressed,
        open: tokens.pressed,
    }
    .apply_to(&mut visuals.widgets, radius, Stroke::new(1.0, tokens.text));
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, tokens.border);
    ctx.set_visuals(visuals);
    ctx.data_mut(|data| data.insert_temp(egui::Id::new(TOKENS_ID), tokens));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_preference_follows_the_desktop_and_keeps_the_last_answer() {
        let system = ThemePreference::System;
        assert_eq!(
            system.resolve(Some(egui::Theme::Dark), ThemeKind::Light),
            ThemeKind::Dark
        );
        assert_eq!(system.resolve(None, ThemeKind::Dark), ThemeKind::Dark);
        assert_eq!(
            ThemePreference::Light.resolve(Some(egui::Theme::Dark), ThemeKind::Dark),
            ThemeKind::Light
        );
    }

    #[test]
    fn applying_a_theme_stores_its_tokens() {
        let ctx = egui::Context::default();
        apply(&ctx, ThemeKind::Dark);
        assert_eq!(Tokens::get(&ctx).kind, ThemeKind::Dark);
        assert!(ctx.global_style().visuals.dark_mode);
    }
}
