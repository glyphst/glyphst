//! Design tokens and egui style of the glyph design (`design/main.pen`,
//! frame 00 Direction): mono chrome, sans prose, hairline panes, one accent,
//! dark first with a paper light theme, radius 0 / 2 / 4 and no shadows.
//! Field names follow the pen variables (`text-2` is `text_2`). Every
//! component reads [`Tokens::get`]. A [`Palette`] generated from a seed
//! colour tints the same tokens (see [`crate::scheme`]).

use craft_ui::theme::WidgetPalette;
use egui::{Color32, CornerRadius, FontFamily, FontId, Shadow, Stroke, TextStyle, Visuals};
use serde::{Deserialize, Serialize};

use crate::{
    fonts,
    scheme::{self, Palettes, Scheme},
};

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
            Self::System => "system",
            Self::Light => "light",
            Self::Dark => "dark",
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

/// The accent swatches of Settings → Appearance (frame 07a).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Accent {
    #[default]
    Amber,
    Blue,
    Green,
    Mono,
    Red,
}

impl Accent {
    pub const ALL: [Self; 5] = [Self::Amber, Self::Blue, Self::Green, Self::Mono, Self::Red];

    pub fn label(self) -> &'static str {
        match self {
            Self::Amber => "amber",
            Self::Blue => "blue",
            Self::Green => "green",
            Self::Mono => "mono",
            Self::Red => "red",
        }
    }

    /// (accent, accent-text, accent-soft, on-accent) in `kind`.
    fn colors(self, kind: ThemeKind) -> [Color32; 4] {
        let c = |hex: u32| Color32::from_rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8);
        let [a, t, s, on] = match (self, kind) {
            (Self::Amber, ThemeKind::Dark) => [0xFFB000, 0xFFC24D, 0x2E2208, 0x0E0F11],
            (Self::Amber, ThemeKind::Light) => [0xB36B00, 0x8F5600, 0xF6E7C8, 0xFFFFFF],
            (Self::Blue, ThemeKind::Dark) => [0x7AA2F7, 0x9BB8FA, 0x16203A, 0x0E0F11],
            (Self::Blue, ThemeKind::Light) => [0x2F5FD0, 0x2550B8, 0xE1E9FA, 0xFFFFFF],
            (Self::Green, ThemeKind::Dark) => [0x7BD88F, 0x9BE3A9, 0x12261A, 0x0E0F11],
            (Self::Green, ThemeKind::Light) => [0x267A3C, 0x23733A, 0xDFF1E4, 0xFFFFFF],
            (Self::Mono, ThemeKind::Dark) => [0xE8EBEF, 0xE8EBEF, 0x2A2E34, 0x0E0F11],
            (Self::Mono, ThemeKind::Light) => [0x1E2125, 0x1E2125, 0xE9E6DE, 0xFFFFFF],
            (Self::Red, ThemeKind::Dark) => [0xFF6B6B, 0xFF8E8E, 0x2E1416, 0x0E0F11],
            (Self::Red, ThemeKind::Light) => [0xC42B1C, 0xA82316, 0xFBE3E0, 0xFFFFFF],
        };
        [c(a), c(t), c(s), c(on)]
    }

    /// The swatch shown in Settings: the dark theme's accent.
    pub fn swatch(self) -> Color32 {
        self.colors(ThemeKind::Dark)[0]
    }
}

/// Where the colours come from: a preset accent over the design's neutrals,
/// or a scheme generated from a seed colour (Settings → Appearance →
/// colours).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Palette {
    Preset(Accent),
    Generated(Scheme),
}

impl Default for Palette {
    fn default() -> Self {
        Self::Preset(Accent::default())
    }
}

/// Row height and padding (Settings → Appearance → density).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Density {
    Compact,
    #[default]
    Default,
    Airy,
}

impl Density {
    pub const ALL: [Self; 3] = [Self::Compact, Self::Default, Self::Airy];

    pub fn label(self) -> &'static str {
        match self {
            Self::Compact => "compact",
            Self::Default => "default",
            Self::Airy => "airy",
        }
    }

    pub fn factor(self) -> f32 {
        match self {
            Self::Compact => 0.85,
            Self::Default => 1.0,
            Self::Airy => 1.2,
        }
    }
}

/// Everything Settings → Appearance changes, applied together by [`apply`].
#[derive(Clone, Debug, PartialEq)]
pub struct Appearance {
    pub kind: ThemeKind,
    pub palette: Palette,
    /// How opaque the window's base layer is: 1 unless transparent.
    pub opacity: f32,
    pub density: Density,
    /// The chrome's base size in points; 13 is the design's.
    pub ui_scale: f32,
    /// Rule titles, tree lines and dot fields; off draws plain labels.
    pub ornaments: bool,
    pub fonts: fonts::FontChoice,
}

impl Default for Appearance {
    fn default() -> Self {
        Self {
            kind: ThemeKind::Light,
            palette: Palette::default(),
            opacity: 1.0,
            density: Density::Default,
            ui_scale: BASE_SCALE,
            ornaments: true,
            fonts: fonts::FontChoice::default(),
        }
    }
}

/// The design's chrome size, which [`Appearance::ui_scale`] is relative to.
pub const BASE_SCALE: f32 = 13.0;
pub const UI_SCALE_RANGE: std::ops::RangeInclusive<f32> = 11.0..=16.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tokens {
    pub kind: ThemeKind,
    /// The window's base: the tab bar and the area around pages.
    pub bg: Color32,
    /// Docked panes, bars and the selected tab.
    pub surface: Color32,
    /// Popovers, menus, fields, cards and messages.
    pub raised: Color32,
    pub hover: Color32,
    /// Hairlines between panes and rows.
    pub rule: Color32,
    /// Edges of controls and popovers, and pressed fills.
    pub rule_strong: Color32,
    pub text: Color32,
    /// Secondary text: rows, icons.
    pub text_2: Color32,
    pub muted: Color32,
    /// Metadata, rules, tree lines and disabled text only: 3.2:1 (dark) and
    /// 2.6:1 (light) on panes, too dim for reading.
    pub faint: Color32,
    /// Fills, meters and focus rings.
    pub accent: Color32,
    /// Accent-coloured text and links.
    pub accent_text: Color32,
    /// Selected rows and toggled buttons.
    pub accent_soft: Color32,
    /// Text on accent fills.
    pub on_accent: Color32,
    pub ok: Color32,
    pub err: Color32,
    pub info: Color32,
    /// Retries, cautions and limits close to full.
    pub warn: Color32,
    pub ok_soft: Color32,
    pub err_soft: Color32,
    /// Behind modals.
    pub scrim: Color32,
    /// Dot fields of empty and loading states.
    pub dot: Color32,
    pub page: Color32,
    /// Ink of placeholder text lines on pages.
    pub page_ink: Color32,
    /// Controls: buttons, keys, fields, switches.
    pub r_ctl: u8,
    /// Popovers, menus, modals and toasts.
    pub r_pop: u8,
    pub density: Density,
    pub ornaments: bool,
    /// How opaque the window's base layer is: 1 unless the window is
    /// transparent.
    pub opacity: f32,
}

const fn rgb(hex: u32) -> Color32 {
    Color32::from_rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

impl Tokens {
    pub fn for_kind(kind: ThemeKind) -> Self {
        Self::new(kind, Accent::Amber)
    }

    pub fn new(kind: ThemeKind, accent: Accent) -> Self {
        let [accent_fill, accent_text, accent_soft, on_accent] = accent.colors(kind);
        match kind {
            ThemeKind::Dark => Self {
                kind,
                bg: rgb(0x0E0F11),
                surface: rgb(0x131518),
                raised: rgb(0x1A1D21),
                hover: rgb(0x22262B),
                rule: rgb(0x2A2E34),
                rule_strong: rgb(0x3A3F46),
                text: rgb(0xE8EBEF),
                text_2: rgb(0xC3C9D1),
                muted: rgb(0x8C95A1),
                faint: rgb(0x5F6873),
                accent: accent_fill,
                accent_text,
                accent_soft,
                on_accent,
                ok: rgb(0x7BD88F),
                err: rgb(0xFF6B6B),
                info: rgb(0x7AA2F7),
                warn: rgb(0xFFC24D),
                ok_soft: rgb(0x12261A),
                err_soft: rgb(0x2E1416),
                scrim: Color32::from_rgba_unmultiplied(0x0E, 0x0F, 0x11, 0xB3),
                dot: rgb(0x3A3F46),
                page: Color32::WHITE,
                page_ink: rgb(0x1E2125),
                r_ctl: 2,
                r_pop: 4,
                density: Density::Default,
                ornaments: true,
                opacity: 1.0,
            },
            ThemeKind::Light => Self {
                kind,
                bg: rgb(0xEFEDE7),
                surface: rgb(0xF7F6F2),
                raised: rgb(0xFFFFFF),
                hover: rgb(0xE9E6DE),
                rule: rgb(0xDAD6CC),
                rule_strong: rgb(0xC4BFB2),
                text: rgb(0x1E2125),
                text_2: rgb(0x3B4149),
                muted: rgb(0x5F6873),
                faint: rgb(0x8C95A1),
                accent: accent_fill,
                accent_text,
                accent_soft,
                on_accent,
                ok: rgb(0x2E8B46),
                err: rgb(0xC42B1C),
                info: rgb(0x2F5FD0),
                warn: rgb(0x8F5600),
                ok_soft: rgb(0xDFF1E4),
                err_soft: rgb(0xFBE3E0),
                scrim: Color32::from_rgba_unmultiplied(0x1E, 0x21, 0x25, 0x66),
                dot: rgb(0xC4BFB2),
                page: Color32::WHITE,
                page_ink: rgb(0x1E2125),
                r_ctl: 2,
                r_pop: 4,
                density: Density::Default,
                ornaments: true,
                opacity: 1.0,
            },
        }
    }

    /// The tokens of `palette`: a preset's, or the design's tokens drawn
    /// from a generated scheme.
    pub fn from_palette(kind: ThemeKind, palette: Palette) -> Self {
        match palette {
            Palette::Preset(accent) => Self::new(kind, accent),
            Palette::Generated(scheme) => Self::generated(kind, &scheme),
        }
    }

    /// The amber tokens with each colour replaced by the scheme's palette
    /// at the same tone: surfaces and text from the neutral palette,
    /// hairlines and secondary text from the neutral variant, the accent
    /// from the primary. Tones (L*) fix luminance, so every pair keeps the
    /// design's contrast; [`scheme::Contrast`] moves secondary text and
    /// hairlines towards the text. Status colours keep their meaning.
    fn generated(kind: ThemeKind, scheme: &Scheme) -> Self {
        let base = Self::new(kind, Accent::Amber);
        let Palettes {
            primary,
            neutral,
            neutral_variant,
            ..
        } = scheme.palettes(kind == ThemeKind::Dark);
        let spread = scheme.contrast.spread();
        let text = scheme::tone(base.text);
        let toward_text = |color: Color32| {
            let tone = scheme::tone(color);
            tone + (text - tone) * spread
        };
        let n = |color: Color32| Palettes::at(&neutral, scheme::tone(color));
        let nv = |color: Color32| Palettes::at(&neutral_variant, toward_text(color));
        let a = |color: Color32| Palettes::at(&primary, scheme::tone(color));
        let scrim = {
            let [r, g, b, alpha] = base.scrim.to_srgba_unmultiplied();
            let [r, g, b, _] = n(Color32::from_rgb(r, g, b)).to_array();
            Color32::from_rgba_unmultiplied(r, g, b, alpha)
        };
        Self {
            bg: n(base.bg),
            surface: n(base.surface),
            raised: n(base.raised),
            hover: n(base.hover),
            rule: nv(base.rule),
            rule_strong: nv(base.rule_strong),
            text: n(base.text),
            text_2: nv(base.text_2),
            muted: nv(base.muted),
            faint: nv(base.faint),
            accent: a(base.accent),
            accent_text: a(base.accent_text),
            accent_soft: a(base.accent_soft),
            on_accent: if kind == ThemeKind::Dark {
                n(base.on_accent)
            } else {
                base.on_accent
            },
            scrim,
            dot: nv(base.dot),
            ..base
        }
    }

    /// A heatmap cell at `level` (0 to 4; pen tokens `heat-0` to `heat-4`):
    /// an empty day, then the accent over the base at 25, 50, 75 and 100%.
    pub fn heat(&self, level: u8) -> Color32 {
        let empty = match self.kind {
            ThemeKind::Dark => self.hover,
            ThemeKind::Light => self.rule,
        };
        match level {
            0 => empty.to_opaque(),
            level => {
                let t = f32::from(level.min(4)) / 4.0;
                let [br, bg, bb, _] = self.bg.to_opaque().to_array();
                let [ar, ag, ab, _] = self.accent.to_array();
                let mix =
                    |b: u8, a: u8| (f32::from(b) + (f32::from(a) - f32::from(b)) * t).round() as u8;
                Color32::from_rgb(mix(br, ar), mix(bg, ag), mix(bb, ab))
            }
        }
    }

    /// The tokens for a transparent window `opacity` opaque: the base layer
    /// and the docked panes show the desktop through. Everything raised
    /// over them (popovers, menus, dialogs, fields), highlights, text, lines
    /// and pages stays opaque, so content never shows through it.
    pub fn with_opacity(mut self, opacity: f32) -> Self {
        let opacity = if opacity.is_finite() {
            opacity.clamp(0.0, 1.0)
        } else {
            1.0
        };
        if opacity >= 1.0 {
            return self;
        }
        for surface in [&mut self.bg, &mut self.surface] {
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

    /// `base` scaled by the density: row heights and paddings.
    pub fn row(&self, base: f32) -> f32 {
        (base * self.density.factor()).round()
    }

    pub fn ctl_radius(&self) -> CornerRadius {
        CornerRadius::same(self.r_ctl)
    }

    pub fn pop_radius(&self) -> CornerRadius {
        CornerRadius::same(self.r_pop)
    }

    /// A 1px hairline of panes and rows.
    pub fn hairline(&self) -> Stroke {
        Stroke::new(1.0, self.rule)
    }

    /// The 1px edge of controls and popovers.
    pub fn edge(&self) -> Stroke {
        Stroke::new(1.0, self.rule_strong)
    }

    /// RGB of the base layer in 0..=1, for the solid background effect
    /// (which takes the window's opacity separately).
    pub fn bg_rgb(&self) -> [f32; 3] {
        let [r, g, b, _] = self.bg.to_srgba_unmultiplied();
        [r, g, b].map(|channel| f32::from(channel) / 255.0)
    }

    /// The text colour at `alpha`: white in the dark theme, black in the
    /// light one, for tints over any surface.
    pub fn ink(&self, alpha: f32) -> Color32 {
        let alpha = (alpha.clamp(0.0, 1.0) * 255.0).round() as u8;
        if self.dark() {
            Color32::from_white_alpha(alpha)
        } else {
            Color32::from_black_alpha(alpha)
        }
    }

    /// `color` faded to `alpha` of its opacity.
    pub fn fade(color: Color32, alpha: f32) -> Color32 {
        color.gamma_multiply(alpha.clamp(0.0, 1.0))
    }

    /// The inverted fill and text of tooltips (component `tooltip`).
    pub fn inverted(&self) -> (Color32, Color32) {
        (self.text, self.bg)
    }
}

const TOKENS_ID: &str = "glyphst-reader-theme";

/// `a` moved `t` of the way to `b`, opaque.
pub fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let channel = |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * t).round() as u8;
    Color32::from_rgb(
        channel(a.r(), b.r()),
        channel(a.g(), b.g()),
        channel(a.b(), b.b()),
    )
}

/// The prose style: answers, help and descriptions (Inter 14, line height
/// 1.55).
pub const PROSE: &str = "prose";
pub const PROSE_LINE_HEIGHT: f32 = 1.55;

/// Chrome text: tabs, rows, menus, labels and numbers (JetBrains Mono).
pub fn mono(size: f32) -> FontId {
    FontId::monospace(size)
}

/// Mono at weight 600: primary buttons, the selected tab, titles.
pub fn mono_strong(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(fonts::MONO_SEMIBOLD.into()))
}

/// Mono at weight 700: the logo and wordmarks.
pub fn mono_bold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(fonts::MONO_BOLD.into()))
}

/// Prose: Inter regular.
pub fn regular(size: f32) -> FontId {
    FontId::proportional(size)
}

pub fn medium(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(fonts::MEDIUM.into()))
}

/// Headings: Inter SemiBold.
pub fn semibold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(fonts::SEMIBOLD.into()))
}

/// The prose style's font in `ui`.
pub fn prose(ui: &egui::Ui) -> FontId {
    // A style made before `apply` (the first frame) has no prose style.
    ui.style()
        .text_styles
        .get(&TextStyle::Name(PROSE.into()))
        .cloned()
        .unwrap_or_else(|| regular(14.0))
}

/// Applies `appearance` to egui's style: tokens, visuals, spacing, text
/// styles and the zoom of [`Appearance::ui_scale`]. Fonts are installed
/// separately by [`fonts::install`], since that is slow.
pub fn apply(ctx: &egui::Context, appearance: &Appearance) {
    // Popups and menus take their style from egui's theme, so it must
    // match the colours drawn.
    ctx.set_theme(match appearance.kind {
        ThemeKind::Light => egui::Theme::Light,
        ThemeKind::Dark => egui::Theme::Dark,
    });
    let mut t =
        Tokens::from_palette(appearance.kind, appearance.palette).with_opacity(appearance.opacity);
    t.density = appearance.density;
    t.ornaments = appearance.ornaments;
    ctx.data_mut(|data| data.insert_temp(egui::Id::new(TOKENS_ID), t));
    let mut v = if t.dark() {
        Visuals::dark()
    } else {
        Visuals::light()
    };
    v.panel_fill = t.surface;
    v.window_fill = t.raised;
    v.window_stroke = t.edge();
    v.extreme_bg_color = t.raised;
    v.faint_bg_color = t.hover;
    v.code_bg_color = t.raised;
    v.selection.bg_fill = t.accent_soft;
    v.selection.stroke = Stroke::new(1.0, t.accent);
    v.hyperlink_color = t.accent_text;
    v.override_text_color = Some(t.text);
    v.error_fg_color = t.err;
    v.warn_fg_color = t.warn;
    v.window_corner_radius = t.pop_radius();
    v.menu_corner_radius = t.pop_radius();
    v.window_shadow = Shadow::NONE;
    v.popup_shadow = Shadow::NONE;
    v.text_cursor.stroke = Stroke::new(2.0, t.accent);
    WidgetPalette {
        noninteractive: t.surface,
        inactive: t.raised,
        hovered: t.hover,
        active: t.rule_strong,
        open: t.hover,
    }
    .apply_to(&mut v.widgets, t.ctl_radius(), Stroke::new(1.0, t.text));
    v.widgets.noninteractive.bg_stroke = t.hairline();
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, t.text_2);
    // Buttons are flat until hovered; fields and checkboxes keep an edge.
    v.widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
    v.widgets.inactive.bg_stroke = t.edge();
    v.widgets.hovered.bg_stroke = t.edge();
    v.widgets.active.bg_stroke = Stroke::new(1.0, t.accent);
    v.widgets.open.bg_stroke = t.edge();
    // Crisper text: glyphs on whole pixels, and in the light theme a mild
    // gamma that darkens the antialiased edges of dark text (pdfcraft #76).
    v.text_options.subpixel_binning = false;
    if !t.dark() {
        v.text_options.color_transfer_function =
            egui::epaint::FontColorTransferFunction::Gamma(0.75);
    }
    ctx.set_visuals(v);
    let density = appearance.density.factor();
    ctx.all_styles_mut(|style| {
        style.spacing.item_spacing = egui::vec2(8.0, 6.0) * density;
        style.spacing.button_padding = egui::vec2(10.0, 5.0) * density;
        style.spacing.menu_margin = egui::Margin::same(4);
        style.spacing.interact_size.y = (20.0 * density).round();
        style.spacing.scroll.bar_width = 6.0;
        style.spacing.scroll.floating = true;
        style.text_styles.insert(TextStyle::Body, mono(12.0));
        style.text_styles.insert(TextStyle::Button, mono(12.0));
        style.text_styles.insert(TextStyle::Small, mono(11.0));
        style.text_styles.insert(TextStyle::Heading, semibold(17.0));
        style.text_styles.insert(TextStyle::Monospace, mono(12.0));
        style
            .text_styles
            .insert(TextStyle::Name(PROSE.into()), regular(14.0));
        style.interaction.tooltip_delay = 0.35;
    });
    let scale = appearance
        .ui_scale
        .clamp(*UI_SCALE_RANGE.start(), *UI_SCALE_RANGE.end());
    let zoom = scale / BASE_SCALE;
    if (ctx.zoom_factor() - zoom).abs() > f32::EPSILON {
        ctx.set_zoom_factor(zoom);
    }
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

    /// The levels are the design's `heat-0` to `heat-4`.
    #[test]
    fn heat_levels_match_the_design() {
        let dark = Tokens::for_kind(ThemeKind::Dark);
        let light = Tokens::for_kind(ThemeKind::Light);
        let hex = |tokens: &Tokens| -> Vec<u32> {
            (0..5)
                .map(|level| {
                    let [r, g, b, _] = tokens.heat(level).to_array();
                    u32::from_be_bytes([0, r, g, b])
                })
                .collect()
        };
        assert_eq!(
            hex(&dark),
            [0x22262B, 0x4A370D, 0x876009, 0xC38804, 0xFFB000]
        );
        assert_eq!(
            hex(&light),
            [0xDAD6CC, 0xE0CDAD, 0xD1AC74, 0xC28C3A, 0xB36B00]
        );
    }

    #[test]
    fn opacity_makes_the_base_and_panes_translucent_and_keeps_raised_layers_opaque() {
        for kind in [ThemeKind::Light, ThemeKind::Dark] {
            let opaque = Tokens::for_kind(kind);
            assert_eq!(opaque.with_opacity(1.0), opaque);
            assert_eq!(opaque.with_opacity(f32::NAN), opaque);
            let half = opaque.with_opacity(0.5);
            assert_eq!(half.opacity, 0.5);
            for (surface, was) in [(half.bg, opaque.bg), (half.surface, opaque.surface)] {
                assert!((126..=129).contains(&surface.a()), "{surface:?}");
                // The same colour, only less opaque.
                let [r, g, b, _] = surface.to_srgba_unmultiplied();
                let [r0, g0, b0, _] = was.to_srgba_unmultiplied();
                assert!(r.abs_diff(r0) <= 2 && g.abs_diff(g0) <= 2 && b.abs_diff(b0) <= 2);
            }
            // What floats over content, and what is drawn on it, stays
            // opaque.
            for fixed in [
                half.raised,
                half.hover,
                half.rule,
                half.rule_strong,
                half.accent_soft,
                half.text,
                half.text_2,
                half.accent,
                half.page,
            ] {
                assert_eq!(fixed.a(), 255);
            }
            // The solid background takes the colour, not its opacity.
            for (h, o) in half.bg_rgb().into_iter().zip(opaque.bg_rgb()) {
                assert!((h - o).abs() <= 2.0 / 255.0);
            }
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

    /// Every preset, and schemes from saturated, pastel, near-grey,
    /// yellow and blue seeds in every style and contrast.
    fn palettes() -> Vec<Palette> {
        let mut palettes: Vec<Palette> = Accent::ALL.into_iter().map(Palette::Preset).collect();
        for seed in [
            [0xE0, 0x30, 0x30],
            [0xF4, 0xC6, 0xD8],
            [0x80, 0x82, 0x85],
            [0xF2, 0xD0, 0x2A],
            [0x1A, 0x4F, 0xD0],
            [0x10, 0x60, 0x40],
        ] {
            for style in scheme::SchemeStyle::ALL {
                for contrast in scheme::Contrast::ALL {
                    palettes.push(Palette::Generated(Scheme {
                        seed,
                        style,
                        contrast,
                    }));
                }
            }
        }
        palettes
    }

    #[test]
    fn reading_text_is_readable_on_every_surface() {
        for kind in [ThemeKind::Light, ThemeKind::Dark] {
            for accent in palettes() {
                let t = Tokens::from_palette(kind, accent);
                for (name, fg) in [("text", t.text), ("text_2", t.text_2), ("muted", t.muted)] {
                    for bg in [t.bg, t.surface, t.raised, t.hover, t.accent_soft] {
                        let ratio = contrast(fg, bg);
                        assert!(
                            ratio >= 4.5,
                            "{kind:?} {accent:?} {name} on {bg:?}: {ratio:.2}:1, WCAG AA needs 4.5:1"
                        );
                    }
                }
                for bg in [t.bg, t.surface, t.raised, t.accent_soft] {
                    let ratio = contrast(t.accent_text, bg);
                    assert!(
                        ratio >= 4.5,
                        "{kind:?} {accent:?} accent_text: {ratio:.2}:1"
                    );
                }
                let (fill, text) = t.inverted();
                assert!(contrast(text, fill) >= 4.5);
            }
        }
    }

    /// `faint` follows the design (#5F6873 / #8C95A1) and is kept to
    /// metadata, rules, tree lines and disabled text, never to reading
    /// text; this pins it at the design's contrast (light 2.6:1 on `bg`,
    /// dark 3.2:1) so it cannot drift dimmer.
    #[test]
    fn faint_keeps_the_designs_contrast_on_panes() {
        for kind in [ThemeKind::Light, ThemeKind::Dark] {
            for palette in palettes() {
                let t = Tokens::from_palette(kind, palette);
                for bg in [t.bg, t.surface] {
                    let ratio = contrast(t.faint, bg);
                    assert!(ratio >= 2.5, "{kind:?} {palette:?} faint: {ratio:.2}:1");
                }
            }
        }
    }

    /// Labels on accent fills (primary buttons, the send button). The
    /// design's light amber with white labels reaches 4.2:1.
    #[test]
    fn labels_on_accent_fills_are_readable() {
        for kind in [ThemeKind::Light, ThemeKind::Dark] {
            for accent in palettes() {
                let t = Tokens::from_palette(kind, accent);
                let ratio = contrast(t.on_accent, t.accent);
                assert!(ratio >= 4.0, "{kind:?} {accent:?} on_accent: {ratio:.2}:1");
            }
        }
    }

    #[test]
    fn applying_an_appearance_stores_its_tokens() {
        let ctx = egui::Context::default();
        let mut appearance = Appearance {
            kind: ThemeKind::Dark,
            palette: Palette::Preset(Accent::Blue),
            density: Density::Compact,
            ornaments: false,
            ..Appearance::default()
        };
        apply(&ctx, &appearance);
        let t = Tokens::get(&ctx);
        assert_eq!(t.kind, ThemeKind::Dark);
        assert_eq!(t.accent, Accent::Blue.swatch());
        assert_eq!(t.density, Density::Compact);
        assert!(!t.ornaments);
        assert!(ctx.global_style().visuals.dark_mode);
        assert_eq!(ctx.global_style().visuals.window_shadow, Shadow::NONE);

        // Popups and menus are over content: opaque.
        appearance.kind = ThemeKind::Light;
        appearance.opacity = 0.6;
        apply(&ctx, &appearance);
        let visuals = &ctx.global_style().visuals;
        assert_eq!(visuals.window_fill.a(), 255);
        assert_eq!(Tokens::get(&ctx).opacity, 0.6);
    }

    /// A generated theme keeps the design's lightness and takes its hue
    /// from the seed.
    #[test]
    fn a_generated_theme_tints_the_designs_tones() {
        let blue = Palette::Generated(Scheme {
            seed: [0x1A, 0x4F, 0xD0],
            style: scheme::SchemeStyle::Vibrant,
            contrast: scheme::Contrast::Standard,
        });
        for kind in [ThemeKind::Light, ThemeKind::Dark] {
            let (preset, generated) = (Tokens::for_kind(kind), Tokens::from_palette(kind, blue));
            for (name, a, b) in [
                ("bg", preset.bg, generated.bg),
                ("text", preset.text, generated.text),
                ("accent", preset.accent, generated.accent),
            ] {
                let (a, b) = (scheme::tone(a), scheme::tone(b));
                assert!((a - b).abs() < 1.0, "{kind:?} {name}: {a:.1} vs {b:.1}");
            }
            let [r, _, b, _] = generated.accent.to_array();
            assert!(b > r, "{kind:?} accent {:?} is blue", generated.accent);
            assert_eq!(generated.err, preset.err);
        }
    }

    #[test]
    fn density_scales_rows() {
        let mut t = Tokens::for_kind(ThemeKind::Dark);
        assert_eq!(t.row(28.0), 28.0);
        t.density = Density::Compact;
        assert!(t.row(28.0) < 28.0);
        t.density = Density::Airy;
        assert!(t.row(28.0) > 28.0);
    }
}
