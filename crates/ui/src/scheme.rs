//! Colour schemes generated from a seed colour (Settings › Appearance ›
//! colours): Material You's palettes (material-color-utilities, spec 2025)
//! drawn at the design's tones. A generated theme keeps every token's
//! lightness, and with it the design's contrast, and takes only hue and
//! chroma from the seed.

use egui::Color32;
use material_colors::{
    color::Rgb,
    dynamic_color::{
        DynamicScheme, SpecVersion, Variant, dynamic_scheme::Platform as SchemePlatform,
    },
    hct::Hct,
    palette::TonalPalette,
    quantize::{Quantizer, QuantizerCelebi},
    score::Score,
};
use serde::{Deserialize, Serialize};

use crate::theme::{Palette, ThemeKind, Tokens};

/// Below this chroma a seed is grey: it has no hue to build on, so it
/// gives grey palettes rather than a hue picked by rounding.
const GREY_CHROMA: f64 = 5.0;

/// How the palettes are built from the seed, in the picker's order: the
/// newest and most balanced first.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SchemeStyle {
    #[default]
    TonalSpot,
    Expressive,
    Vibrant,
    Content,
    Fidelity,
    Rainbow,
    FruitSalad,
    Neutral,
    Monochrome,
}

impl SchemeStyle {
    pub const ALL: [Self; 9] = [
        Self::TonalSpot,
        Self::Expressive,
        Self::Vibrant,
        Self::Content,
        Self::Fidelity,
        Self::Rainbow,
        Self::FruitSalad,
        Self::Neutral,
        Self::Monochrome,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::TonalSpot => "tonal spot",
            Self::Expressive => "expressive",
            Self::Vibrant => "vibrant",
            Self::Content => "content",
            Self::Fidelity => "fidelity",
            Self::Rainbow => "rainbow",
            Self::FruitSalad => "fruit salad",
            Self::Neutral => "neutral",
            Self::Monochrome => "monochrome",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::TonalSpot => "calm, softened colours; Material You's default",
            Self::Expressive => "playful, with hues turned away from the seed",
            Self::Vibrant => "the most colourful accent, tinted surfaces",
            Self::Content => "close to the seed itself, as a wallpaper shows it",
            Self::Fidelity => "the seed's own chroma, kept as is",
            Self::Rainbow => "a colourful accent over grey surfaces",
            Self::FruitSalad => "bright, with the hue turned far from the seed",
            Self::Neutral => "barely tinted, nearly grey",
            Self::Monochrome => "greys only",
        }
    }

    fn variant(self) -> Variant {
        match self {
            Self::TonalSpot => Variant::TonalSpot,
            Self::Expressive => Variant::Expressive,
            Self::Vibrant => Variant::Vibrant,
            Self::Content => Variant::Content,
            Self::Fidelity => Variant::Fidelity,
            Self::Rainbow => Variant::Rainbow,
            Self::FruitSalad => Variant::FruitSalad,
            Self::Neutral => Variant::Neutral,
            Self::Monochrome => Variant::Monochrome,
        }
    }
}

/// How far secondary text and hairlines move from the design's tones
/// towards the main text's.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Contrast {
    #[default]
    Standard,
    Medium,
    High,
}

impl Contrast {
    pub const ALL: [Self; 3] = [Self::Standard, Self::Medium, Self::High];

    pub fn label(self) -> &'static str {
        match self {
            Self::Standard => "standard",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }

    /// Material's contrast level, -1 to 1.
    fn level(self) -> f64 {
        match self {
            Self::Standard => 0.0,
            Self::Medium => 0.5,
            Self::High => 1.0,
        }
    }

    /// The share of the way from a token's tone to the text's.
    pub(crate) fn spread(self) -> f64 {
        match self {
            Self::Standard => 0.0,
            Self::Medium => 0.3,
            Self::High => 0.55,
        }
    }
}

/// A generated scheme: the seed and how to build on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Scheme {
    /// sRGB.
    pub seed: [u8; 3],
    pub style: SchemeStyle,
    pub contrast: Contrast,
}

/// The tonal palettes tokens are drawn from.
pub(crate) struct Palettes {
    pub primary: TonalPalette,
    pub secondary: TonalPalette,
    pub tertiary: TonalPalette,
    pub neutral: TonalPalette,
    pub neutral_variant: TonalPalette,
}

impl Palettes {
    /// `palette`'s colour at `tone` (L*, 0 to 100).
    pub fn at(palette: &TonalPalette, tone: f64) -> Color32 {
        let rgb: Rgb = palette.get_hct(tone.clamp(0.0, 100.0)).into();
        Color32::from_rgb(rgb.red, rgb.green, rgb.blue)
    }
}

impl Scheme {
    pub(crate) fn palettes(&self, dark: bool) -> Palettes {
        let [r, g, b] = self.seed;
        let seed = Hct::new(Rgb::new(r, g, b));
        let variant = if seed.get_chroma() < GREY_CHROMA {
            Variant::Monochrome
        } else {
            self.style.variant()
        };
        let scheme = DynamicScheme::from_spec(
            seed,
            variant,
            dark,
            Some(self.contrast.level()),
            SchemePlatform::Phone,
            SpecVersion::Spec2025,
        );
        Palettes {
            primary: scheme.primary_palette,
            secondary: scheme.secondary_palette,
            tertiary: scheme.tertiary_palette,
            neutral: scheme.neutral_palette,
            neutral_variant: scheme.neutral_variant_palette,
        }
    }
}

impl Scheme {
    /// The scheme at a glance in `kind`: the accent, the secondary and
    /// tertiary palettes at the accent's tone, the base, a raised surface
    /// and a hairline.
    pub fn swatches(&self, kind: ThemeKind) -> [Color32; 6] {
        let t = Tokens::from_palette(kind, Palette::Generated(*self));
        let palettes = self.palettes(kind == ThemeKind::Dark);
        let accent = tone(t.accent);
        [
            t.accent,
            Palettes::at(&palettes.secondary, accent),
            Palettes::at(&palettes.tertiary, accent),
            t.bg,
            t.raised,
            t.rule_strong,
        ]
    }
}

/// A colour's tone: its L* (0 black to 100 white).
pub(crate) fn tone(color: Color32) -> f64 {
    let [r, g, b, _] = color.to_srgba_unmultiplied();
    Hct::new(Rgb::new(r, g, b)).get_tone()
}

/// The colour a theme is best built on from an image's pixels (sRGB, not
/// premultiplied; a few thousand are plenty): Material's quantizer and
/// score. An image with no colourful area gives its mean colour, which is
/// grey enough to give a grey scheme. `None` without opaque pixels.
pub fn seed_from_pixels(rgba: &[u8]) -> Option<[u8; 3]> {
    let pixels: Vec<Rgb> = rgba
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|pixel| pixel[3] >= 128)
        .map(|pixel| Rgb::new(pixel[0], pixel[1], pixel[2]))
        .collect();
    if pixels.is_empty() {
        return None;
    }
    // Grey, so never a candidate: returned only when nothing qualifies.
    let none = Rgb::new(1, 2, 3);
    let quantized = QuantizerCelebi::quantize(&pixels, 128);
    let best = Score::score(&quantized.color_to_count, Some(1), Some(none), None)[0];
    if best != none {
        return Some([best.red, best.green, best.blue]);
    }
    let mut sum = [0u64; 3];
    for pixel in &pixels {
        sum[0] += u64::from(pixel.red);
        sum[1] += u64::from(pixel.green);
        sum[2] += u64::from(pixel.blue);
    }
    let count = pixels.len() as u64;
    Some(sum.map(|channel| (channel / count) as u8))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(colors: &[([u8; 3], usize)]) -> Vec<u8> {
        colors
            .iter()
            .flat_map(|(rgb, count)| std::iter::repeat_n([rgb[0], rgb[1], rgb[2], 255], *count))
            .flatten()
            .collect()
    }

    #[test]
    fn a_solid_colour_is_its_own_seed() {
        let seed = seed_from_pixels(&image(&[([30, 90, 200], 400)])).unwrap();
        let [r, g, b] = seed;
        assert!(
            r.abs_diff(30) <= 2 && g.abs_diff(90) <= 2 && b.abs_diff(200) <= 2,
            "{seed:?}"
        );
    }

    #[test]
    fn the_colourful_part_of_an_image_wins_over_a_large_grey_one() {
        let seed = seed_from_pixels(&image(&[([128, 128, 128], 900), ([220, 60, 40], 100)]));
        let [r, g, b] = seed.unwrap();
        assert!(r > 150 && g < 100 && b < 100, "{r} {g} {b}");
    }

    #[test]
    fn a_grey_image_gives_a_grey_seed_and_grey_palettes() {
        let seed =
            seed_from_pixels(&image(&[([40, 40, 40], 300), ([200, 200, 200], 100)])).unwrap();
        assert_eq!(seed, [80, 80, 80]);
        let scheme = Scheme {
            seed,
            style: SchemeStyle::Vibrant,
            contrast: Contrast::Standard,
        };
        let palettes = scheme.palettes(true);
        assert!(palettes.primary.chroma() < 1.0);
        assert!(palettes.neutral.chroma() < 1.0);
    }

    #[test]
    fn transparent_pixels_are_ignored() {
        assert_eq!(seed_from_pixels(&[255, 0, 0, 0]), None);
        assert_eq!(seed_from_pixels(&[]), None);
    }
}
