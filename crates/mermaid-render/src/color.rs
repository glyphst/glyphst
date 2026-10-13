//! The two color spaces the post-processing needs: HSLA for themes and
//! lightness changes, RGBA for blending and contrast. Components are 0..=1,
//! hue included.

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Hsla {
    pub h: f32,
    pub s: f32,
    pub l: f32,
    pub a: f32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rgba {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

pub fn hsla(h: f32, s: f32, l: f32, a: f32) -> Hsla {
    Hsla { h, s, l, a }
}

/// An opaque color from `0xRRGGBB`.
pub fn rgb(hex: u32) -> Rgba {
    let channel = |shift: u32| ((hex >> shift) & 0xFF) as f32 / 255.0;
    Rgba {
        r: channel(16),
        g: channel(8),
        b: channel(0),
        a: 1.0,
    }
}

impl From<Hsla> for Rgba {
    fn from(color: Hsla) -> Self {
        let Hsla { h, s, l, a } = color;
        let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
        let h6 = (h.rem_euclid(1.0)) * 6.0;
        let x = c * (1.0 - (h6 % 2.0 - 1.0).abs());
        let (r, g, b) = match h6 as u32 {
            0 => (c, x, 0.0),
            1 => (x, c, 0.0),
            2 => (0.0, c, x),
            3 => (0.0, x, c),
            4 => (x, 0.0, c),
            _ => (c, 0.0, x),
        };
        let m = l - c / 2.0;
        Rgba {
            r: r + m,
            g: g + m,
            b: b + m,
            a,
        }
    }
}

impl From<Rgba> for Hsla {
    fn from(color: Rgba) -> Self {
        let Rgba { r, g, b, a } = color;
        let max = r.max(g).max(b);
        let min = r.min(g).min(b);
        let l = (max + min) / 2.0;
        let delta = max - min;
        if delta == 0.0 {
            return Hsla {
                h: 0.0,
                s: 0.0,
                l,
                a,
            };
        }
        let s = delta / (1.0 - (2.0 * l - 1.0).abs());
        let h = if max == r {
            ((g - b) / delta).rem_euclid(6.0)
        } else if max == g {
            (b - r) / delta + 2.0
        } else {
            (r - g) / delta + 4.0
        } / 6.0;
        Hsla { h, s, l, a }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_round_trip_between_the_spaces() {
        for hex in [0x000000, 0xFFFFFF, 0xDC2626, 0x0F172A, 0x94A3B8, 0x00FF7F] {
            let rgba = rgb(hex);
            let back = Rgba::from(Hsla::from(rgba));
            for (a, b) in [(rgba.r, back.r), (rgba.g, back.g), (rgba.b, back.b)] {
                assert!((a - b).abs() < 1e-5, "{hex:06x}");
            }
        }
        let red = Rgba::from(hsla(0.0, 1.0, 0.5, 1.0));
        assert_eq!((red.r, red.g, red.b), (1.0, 0.0, 0.0));
        let blue = Hsla::from(rgb(0x0000FF));
        assert!((blue.h - 2.0 / 3.0).abs() < 1e-6 && blue.s == 1.0 && blue.l == 0.5);
    }
}
