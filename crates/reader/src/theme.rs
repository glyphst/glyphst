//! Design tokens and egui style, taken from pdfcraft's `theme.rs`: neutral
//! chrome, one blue accent, white pages in both modes, Inter type, soft
//! shadows. Every custom widget reads [`Tokens::get`].

use craft_ui::theme::WidgetPalette;
use egui::{Color32, CornerRadius, FontFamily, FontId, Shadow, Stroke, TextStyle, Visuals};
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
    /// it gives none; then System keeps `last_system`, the desktop's last
    /// answer, never the Light or Dark chosen before it.
    pub fn resolve(self, system: Option<egui::Theme>, last_system: Option<ThemeKind>) -> ThemeKind {
        match self {
            Self::Light => ThemeKind::Light,
            Self::Dark => ThemeKind::Dark,
            Self::System => match system {
                Some(egui::Theme::Dark) => ThemeKind::Dark,
                Some(egui::Theme::Light) => ThemeKind::Light,
                None => last_system.unwrap_or_default(),
            },
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tokens {
    pub kind: ThemeKind,
    /// The top bar.
    pub titlebar: Color32,
    /// Bars and docks.
    pub chrome: Color32,
    /// The drawers and the assistant docked at the stage's sides.
    pub panel: Color32,
    /// The document area behind pages.
    pub pasteboard: Color32,
    pub page: Color32,
    /// Popups, cards and dialogs.
    pub card: Color32,
    /// Groups of rows on a card, as in Settings.
    pub group: Color32,
    pub border: Color32,
    /// Hairlines between groups.
    pub divider: Color32,
    pub text: Color32,
    pub text_muted: Color32,
    /// Hints, captions and section titles.
    pub text_faint: Color32,
    pub icon: Color32,
    pub hover: Color32,
    pub pressed: Color32,
    pub accent: Color32,
    /// Accent-coloured text and links.
    pub accent_text: Color32,
    /// Selected rows and toggled buttons.
    pub accent_soft: Color32,
    pub field: Color32,
    pub danger: Color32,
    pub page_shadow: Color32,
    pub radius: u8,
    /// How opaque the window's surfaces are: 1 unless the window is
    /// transparent.
    pub opacity: f32,
}

impl Tokens {
    pub fn for_kind(kind: ThemeKind) -> Self {
        match kind {
            ThemeKind::Light => Self {
                kind,
                titlebar: Color32::from_rgb(0xE9, 0xE9, 0xEB),
                chrome: Color32::from_rgb(0xFF, 0xFF, 0xFF),
                panel: Color32::from_rgb(0xFF, 0xFF, 0xFF),
                pasteboard: Color32::from_rgb(0xF1, 0xF1, 0xF3),
                page: Color32::WHITE,
                card: Color32::from_rgb(0xFF, 0xFF, 0xFF),
                group: Color32::from_rgb(0xF6, 0xF6, 0xF8),
                border: Color32::from_rgb(0xDA, 0xDA, 0xDE),
                divider: Color32::from_rgb(0xE8, 0xE8, 0xEB),
                text: Color32::from_rgb(0x22, 0x22, 0x26),
                text_muted: Color32::from_rgb(0x5E, 0x5E, 0x66),
                text_faint: Color32::from_rgb(0x66, 0x66, 0x6E),
                icon: Color32::from_rgb(0x44, 0x44, 0x4B),
                hover: Color32::from_rgb(0xF0, 0xF0, 0xF3),
                pressed: Color32::from_rgb(0xE4, 0xE4, 0xE9),
                accent: Color32::from_rgb(0x1B, 0x63, 0xE0),
                accent_text: Color32::from_rgb(0x17, 0x55, 0xC4),
                accent_soft: Color32::from_rgb(0xE3, 0xEC, 0xFD),
                field: Color32::from_rgb(0xFF, 0xFF, 0xFF),
                danger: Color32::from_rgb(0xC4, 0x2B, 0x1C),
                page_shadow: Color32::from_black_alpha(34),
                radius: 6,
                opacity: 1.0,
            },
            ThemeKind::Dark => Self {
                kind,
                titlebar: Color32::from_rgb(0x1B, 0x1B, 0x1E),
                chrome: Color32::from_rgb(0x26, 0x26, 0x2A),
                panel: Color32::from_rgb(0x2E, 0x2E, 0x33),
                pasteboard: Color32::from_rgb(0x19, 0x19, 0x1C),
                page: Color32::WHITE,
                card: Color32::from_rgb(0x2E, 0x2E, 0x33),
                group: Color32::from_rgb(0x28, 0x28, 0x2D),
                border: Color32::from_rgb(0x3C, 0x3C, 0x43),
                divider: Color32::from_rgb(0x33, 0x33, 0x39),
                text: Color32::from_rgb(0xEC, 0xEC, 0xEF),
                text_muted: Color32::from_rgb(0xAE, 0xAE, 0xB6),
                text_faint: Color32::from_rgb(0xA0, 0xA0, 0xA7),
                icon: Color32::from_rgb(0xD4, 0xD4, 0xDA),
                hover: Color32::from_rgb(0x34, 0x34, 0x3A),
                pressed: Color32::from_rgb(0x3E, 0x3E, 0x45),
                accent: Color32::from_rgb(0x4B, 0x8B, 0xF5),
                accent_text: Color32::from_rgb(0x8C, 0xB6, 0xFA),
                accent_soft: Color32::from_rgb(0x24, 0x36, 0x57),
                field: Color32::from_rgb(0x1E, 0x1E, 0x22),
                danger: Color32::from_rgb(0xF2, 0x8B, 0x82),
                page_shadow: Color32::from_black_alpha(120),
                radius: 6,
                opacity: 1.0,
            },
        }
    }

    /// The tokens for a transparent window `opacity` opaque: the base layer
    /// (the title bar and its selected tab, the background around the
    /// pages) shows the desktop through. Everything over the pages (the
    /// docked panels, cards, popups, menus, dialogs), fields, highlights,
    /// text, lines and pages stays opaque, so content never shows through
    /// it and it never shows the desktop instead of the document.
    pub fn with_opacity(mut self, opacity: f32) -> Self {
        let opacity = if opacity.is_finite() {
            opacity.clamp(0.0, 1.0)
        } else {
            1.0
        };
        if opacity >= 1.0 {
            return self;
        }
        for surface in [&mut self.titlebar, &mut self.chrome, &mut self.pasteboard] {
            *surface = surface.gamma_multiply(opacity);
        }
        self.opacity = opacity;
        self
    }

    pub fn get(ctx: &egui::Context) -> Self {
        ctx.data(|data| data.get_temp::<Tokens>(egui::Id::new(TOKENS_ID)))
            .unwrap_or_else(|| Self::for_kind(ThemeKind::Light))
    }

    pub fn dark(&self) -> bool {
        self.kind == ThemeKind::Dark
    }

    /// RGB of the pasteboard in 0..=1, for the solid background effect
    /// (which takes the window's opacity separately).
    pub fn pasteboard_rgb(&self) -> [f32; 3] {
        let [r, g, b, _] = self.pasteboard.to_srgba_unmultiplied();
        [r, g, b].map(|channel| f32::from(channel) / 255.0)
    }

    /// The inverted card of toasts: (fill, text).
    pub fn toast(&self) -> (Color32, Color32) {
        if self.dark() {
            (
                Color32::from_rgb(0xEC, 0xEC, 0xEF),
                Color32::from_rgb(0x22, 0x22, 0x26),
            )
        } else {
            (Color32::from_rgb(0x2A, 0x2A, 0x2F), Color32::WHITE)
        }
    }

    /// The shadow under cards and dialogs.
    pub fn card_shadow(&self) -> Shadow {
        Shadow {
            offset: [0, 8],
            blur: 28,
            spread: 0,
            color: Color32::from_black_alpha(if self.dark() { 110 } else { 38 }),
        }
    }

    /// The shadow under popups and menus.
    pub fn popup_shadow(&self) -> Shadow {
        Shadow {
            offset: [0, 4],
            blur: 16,
            spread: 0,
            color: Color32::from_black_alpha(if self.dark() { 90 } else { 30 }),
        }
    }
}

const TOKENS_ID: &str = "glyphst-reader-theme";

pub fn regular(size: f32) -> FontId {
    FontId::proportional(size)
}

pub fn medium(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(crate::fonts::MEDIUM.into()))
}

pub fn semibold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(crate::fonts::SEMIBOLD.into()))
}

pub fn mono(size: f32) -> FontId {
    FontId::monospace(size)
}

/// Applies the theme's tokens to egui's style, after pdfcraft's `apply`;
/// `opacity` below 1 makes the surfaces translucent (see
/// [`Tokens::with_opacity`]).
pub fn apply(ctx: &egui::Context, kind: ThemeKind, opacity: f32) {
    // Popups and menus take their style from egui's theme, so it must
    // match the colours drawn.
    ctx.set_theme(match kind {
        ThemeKind::Light => egui::Theme::Light,
        ThemeKind::Dark => egui::Theme::Dark,
    });
    let t = Tokens::for_kind(kind).with_opacity(opacity);
    ctx.data_mut(|data| data.insert_temp(egui::Id::new(TOKENS_ID), t));
    let mut v = if t.dark() {
        Visuals::dark()
    } else {
        Visuals::light()
    };
    v.panel_fill = t.panel;
    v.window_fill = t.card;
    v.window_stroke = Stroke::new(1.0, t.border);
    v.extreme_bg_color = t.field;
    v.faint_bg_color = t.hover;
    v.code_bg_color = t.hover;
    v.selection.bg_fill = t.accent_soft;
    v.selection.stroke = Stroke::new(1.0, t.accent);
    v.hyperlink_color = t.accent_text;
    v.override_text_color = Some(t.text);
    v.error_fg_color = t.danger;
    v.window_corner_radius = CornerRadius::same(10);
    v.menu_corner_radius = CornerRadius::same(8);
    v.window_shadow = t.card_shadow();
    v.popup_shadow = t.popup_shadow();
    let radius = CornerRadius::same(t.radius);
    WidgetPalette {
        noninteractive: t.chrome,
        inactive: t.field,
        hovered: t.hover,
        active: t.pressed,
        open: t.hover,
    }
    .apply_to(&mut v.widgets, radius, Stroke::new(1.0, t.text));
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, t.divider);
    // Buttons are flat until hovered; fields and checkboxes keep a fill.
    v.widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
    v.widgets.inactive.bg_stroke = Stroke::NONE;
    v.widgets.hovered.bg_stroke = Stroke::NONE;
    v.widgets.active.bg_stroke = Stroke::NONE;
    v.widgets.open.bg_stroke = Stroke::NONE;
    // Crisper text: glyphs on whole pixels, and in the light theme a mild
    // gamma that darkens the antialiased edges of dark text (pdfcraft #76).
    v.text_options.subpixel_binning = false;
    if !t.dark() {
        v.text_options.color_transfer_function =
            egui::epaint::FontColorTransferFunction::Gamma(0.75);
    }
    ctx.set_visuals(v);
    ctx.all_styles_mut(|style| {
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(10.0, 5.0);
        style.spacing.menu_margin = egui::Margin::same(6);
        style.spacing.scroll.bar_width = 8.0;
        style.spacing.scroll.floating = true;
        style.text_styles.insert(TextStyle::Body, regular(13.0));
        style.text_styles.insert(TextStyle::Button, regular(13.0));
        style.text_styles.insert(TextStyle::Small, regular(11.0));
        style.text_styles.insert(TextStyle::Heading, semibold(17.0));
        style.text_styles.insert(TextStyle::Monospace, mono(12.0));
        style.interaction.tooltip_delay = 0.35;
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_preference_follows_the_desktop_and_keeps_its_last_answer() {
        let system = ThemePreference::System;
        assert_eq!(
            system.resolve(Some(egui::Theme::Dark), Some(ThemeKind::Light)),
            ThemeKind::Dark
        );
        assert_eq!(system.resolve(None, Some(ThemeKind::Dark)), ThemeKind::Dark);
        assert_eq!(
            ThemePreference::Light.resolve(Some(egui::Theme::Dark), Some(ThemeKind::Dark)),
            ThemeKind::Light
        );
    }

    /// The bug where System kept the Dark picked before it: with no desktop
    /// answer ever, System is Light, whatever was drawn.
    #[test]
    fn system_without_a_desktop_answer_is_light_not_the_previous_choice() {
        assert_eq!(
            ThemePreference::System.resolve(None, None),
            ThemeKind::Light
        );
    }

    #[test]
    fn opacity_makes_the_base_translucent_and_keeps_everything_over_content_opaque() {
        for kind in [ThemeKind::Light, ThemeKind::Dark] {
            let opaque = Tokens::for_kind(kind);
            assert_eq!(opaque.with_opacity(1.0), opaque);
            assert_eq!(opaque.with_opacity(f32::NAN), opaque);
            let half = opaque.with_opacity(0.5);
            assert_eq!(half.opacity, 0.5);
            for (surface, was) in [
                (half.titlebar, opaque.titlebar),
                (half.chrome, opaque.chrome),
                (half.pasteboard, opaque.pasteboard),
            ] {
                assert!((126..=129).contains(&surface.a()), "{surface:?}");
                // The same colour, only less opaque.
                let [r, g, b, _] = surface.to_srgba_unmultiplied();
                let [r0, g0, b0, _] = was.to_srgba_unmultiplied();
                assert!(r.abs_diff(r0) <= 2 && g.abs_diff(g0) <= 2 && b.abs_diff(b0) <= 2);
            }
            // What floats over content, and what is drawn on it, stays
            // opaque.
            for fixed in [
                half.panel,
                half.card,
                half.group,
                half.field,
                half.hover,
                half.pressed,
                half.accent_soft,
                half.text,
                half.icon,
                half.border,
                half.accent,
                half.page,
            ] {
                assert_eq!(fixed.a(), 255);
            }
            // The solid background takes the colour, not its opacity.
            for (h, o) in half
                .pasteboard_rgb()
                .into_iter()
                .zip(opaque.pasteboard_rgb())
            {
                assert!((h - o).abs() <= 2.0 / 255.0);
            }
            assert_eq!(half.card_shadow(), opaque.card_shadow());
            // The docked panels look like cards while the window is opaque.
            assert_eq!(opaque.panel, opaque.card);
        }
    }

    /// WCAG 2 contrast ratio between two opaque colours.
    fn contrast(a: Color32, b: Color32) -> f32 {
        let luminance = |c: Color32| {
            let linear = |v: u8| {
                let s = f32::from(v) / 255.0;
                if s <= 0.04045 {
                    s / 12.92
                } else {
                    ((s + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * linear(c.r()) + 0.7152 * linear(c.g()) + 0.0722 * linear(c.b())
        };
        let (x, y) = (luminance(a) + 0.05, luminance(b) + 0.05);
        x.max(y) / x.min(y)
    }

    #[test]
    fn text_is_readable_on_every_surface() {
        for kind in [ThemeKind::Light, ThemeKind::Dark] {
            let t = Tokens::for_kind(kind);
            for (name, fg) in [
                ("text", t.text),
                ("text_muted", t.text_muted),
                ("text_faint", t.text_faint),
            ] {
                for bg in [
                    t.titlebar,
                    t.chrome,
                    t.panel,
                    t.card,
                    t.group,
                    t.pasteboard,
                    t.hover,
                    t.accent_soft,
                ] {
                    let ratio = contrast(fg, bg);
                    assert!(
                        ratio >= 4.5,
                        "{kind:?} {name} on {bg:?}: {ratio:.2}:1, WCAG AA needs 4.5:1"
                    );
                }
            }
            let (fill, text) = t.toast();
            assert!(contrast(text, fill) >= 4.5);
        }
    }

    #[test]
    fn applying_a_theme_stores_its_tokens() {
        let ctx = egui::Context::default();
        apply(&ctx, ThemeKind::Dark, 1.0);
        assert_eq!(Tokens::get(&ctx).kind, ThemeKind::Dark);
        assert!(ctx.global_style().visuals.dark_mode);
        assert_eq!(ctx.global_style().visuals.window_fill.a(), 255);

        // Popups, menus and panels are over content: opaque.
        apply(&ctx, ThemeKind::Light, 0.6);
        let visuals = &ctx.global_style().visuals;
        assert_eq!(visuals.window_fill.a(), 255);
        assert_eq!(visuals.panel_fill.a(), 255);
        assert_eq!(Tokens::get(&ctx).opacity, 0.6);
    }
}
