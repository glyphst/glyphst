//! Where the book switcher's cards go, in each layout Settings offers
//! (frames 10a, 10d–f): a fan along the bottom, a carousel, a grid, or
//! slanted slices. Pure geometry in points, sized to the design's
//! 1440 × 920 window and scaled down for smaller ones.

use egui::{Pos2, Rect, Vec2, pos2, vec2};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Layout {
    /// Cards on an arc along the bottom, the highlighted one upright and
    /// raised (10a).
    #[default]
    Fan,
    /// A large card in the middle, smaller ones overlapping on each side
    /// (10d).
    Carousel,
    /// Every book in rows (10e).
    Grid,
    /// Slanted slices, the highlighted one wide (10f).
    Slices,
}

impl Layout {
    pub const ALL: [Self; 4] = [Self::Fan, Self::Carousel, Self::Grid, Self::Slices];

    pub fn label(self) -> &'static str {
        match self {
            Self::Fan => "fan arc",
            Self::Carousel => "carousel",
            Self::Grid => "grid",
            Self::Slices => "slices",
        }
    }
}

/// The design's window, which the sizes below are for.
const DESIGN: Vec2 = vec2(1440.0, 920.0);
/// A card: 180 wide, its thumbnail 196 high above 60 of text.
pub const CARD: Vec2 = vec2(180.0, 256.0);
/// The thumbnail's share of a card's height.
pub const THUMB_SHARE: f32 = 196.0 / 256.0;

/// One card's place.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Place {
    pub center: Pos2,
    /// For slices, the width of each row (the slice is `skew` wider).
    pub size: Vec2,
    /// Clockwise, in radians.
    pub angle: f32,
    /// How far right of the bottom edge the top edge sits (slices).
    pub skew: f32,
    pub opacity: f32,
    /// Cards with a higher order are drawn in front.
    pub order: i32,
}

impl Place {
    /// A point of the card, from its top left as if it were upright and
    /// square-cornered, on the screen.
    pub fn at(&self, local: Vec2) -> Pos2 {
        let half = self.size / 2.0;
        let shear = self.skew * (0.5 - local.y / self.size.y.max(1.0));
        let x = local.x - half.x + shear;
        let y = local.y - half.y;
        let (sin, cos) = self.angle.sin_cos();
        self.center + vec2(x * cos - y * sin, x * sin + y * cos)
    }

    /// Top left, top right, bottom right, bottom left.
    pub fn corners(&self) -> [Pos2; 4] {
        let Vec2 { x: w, y: h } = self.size;
        [
            self.at(vec2(0.0, 0.0)),
            self.at(vec2(w, 0.0)),
            self.at(vec2(w, h)),
            self.at(vec2(0.0, h)),
        ]
    }

    pub fn contains(&self, point: Pos2) -> bool {
        let corners = self.corners();
        // Inside a convex quad: on the same side of every edge.
        let sides: Vec<f32> = (0..4)
            .map(|i| {
                let (a, b) = (corners[i], corners[(i + 1) % 4]);
                (b - a).x * (point - a).y - (b - a).y * (point - a).x
            })
            .collect();
        sides.iter().all(|side| *side >= 0.0) || sides.iter().all(|side| *side <= 0.0)
    }
}

/// The places of `count` cards with card `highlight` highlighted, in
/// `area` (the window), `zoom` times the design's size (gamepad mode draws
/// them larger).
pub fn place(layout: Layout, count: usize, highlight: usize, area: Rect, zoom: f32) -> Vec<Place> {
    let fit = (area.width() / DESIGN.x)
        .min(area.height() / DESIGN.y)
        .min(1.0);
    let scale = (fit * zoom).max(0.4);
    let offset = |index: usize| index as i32 - highlight as i32;
    match layout {
        Layout::Fan => fan(count, area, scale, offset),
        Layout::Carousel => carousel(count, area, scale, offset),
        Layout::Grid => grid(count, highlight, area, scale),
        Layout::Slices => slices(count, area, scale, offset),
    }
}

fn fan(count: usize, area: Rect, scale: f32, offset: impl Fn(usize) -> i32) -> Vec<Place> {
    const RADIUS: f32 = 1100.0;
    const STEP: f32 = 9.0;
    const LIFT: f32 = 64.0;
    let radius = RADIUS * scale;
    let raised = pos2(area.center().x, area.bottom() - 440.0 * scale);
    let hub = raised + vec2(0.0, (LIFT * scale) + radius);
    (0..count)
        .map(|index| {
            let k = offset(index);
            let angle = (k as f32 * STEP).to_radians();
            let center = if k == 0 {
                raised
            } else {
                hub + vec2(angle.sin(), -angle.cos()) * radius
            };
            Place {
                center,
                size: CARD * scale,
                angle,
                skew: 0.0,
                opacity: if k.abs() > 6 { 0.0 } else { 1.0 },
                order: 100 - k.abs(),
            }
        })
        .collect()
}

fn carousel(count: usize, area: Rect, scale: f32, offset: impl Fn(usize) -> i32) -> Vec<Place> {
    const WIDTHS: [f32; 3] = [300.0, 220.0, 170.0];
    const OVERLAP: f32 = 48.0;
    let width = |level: usize| WIDTHS[level.min(2)] * scale;
    let middle = pos2(area.center().x, area.center().y + 10.0 * scale);
    (0..count)
        .map(|index| {
            let k = offset(index);
            let level = k.unsigned_abs() as usize;
            let mut along = 0.0;
            for step in 0..level {
                along += width(step) / 2.0 + width(step + 1) / 2.0 - OVERLAP * scale;
            }
            let w = width(level);
            Place {
                center: middle + vec2(k.signum() as f32 * along, 0.0),
                size: vec2(w, w * CARD.y / CARD.x),
                angle: 0.0,
                skew: 0.0,
                opacity: match level {
                    0 => 1.0,
                    1 => 0.85,
                    2 => 0.65,
                    _ => 0.0,
                },
                order: 100 - level as i32,
            }
        })
        .collect()
}

fn grid(count: usize, highlight: usize, area: Rect, scale: f32) -> Vec<Place> {
    const WIDTH: f32 = 240.0;
    const HEIGHT: f32 = 330.0;
    const GAP: f32 = 28.0;
    let columns = match count {
        0..=3 => count.max(1),
        4 => 2,
        _ => 3,
    };
    let rows = count.div_ceil(columns);
    // Smaller still when the rows don't fit.
    let tall = rows as f32 * HEIGHT + (rows as f32 - 1.0) * GAP;
    let wide = columns as f32 * WIDTH + (columns as f32 - 1.0) * GAP;
    let scale = scale
        .min((area.height() - 160.0 * scale) / tall)
        .min((area.width() - 80.0 * scale) / wide)
        .max(0.2);
    let (width, height, gap) = (WIDTH * scale, HEIGHT * scale, GAP * scale);
    let top =
        area.center().y + 30.0 * scale - (rows as f32 * height + (rows as f32 - 1.0) * gap) / 2.0;
    (0..count)
        .map(|index| {
            let (row, column) = (index / columns, index % columns);
            let in_row = if row == rows - 1 {
                count - row * columns
            } else {
                columns
            };
            let row_width = in_row as f32 * width + (in_row as f32 - 1.0) * gap;
            let left = area.center().x - row_width / 2.0 + column as f32 * (width + gap);
            Place {
                center: pos2(
                    left + width / 2.0,
                    top + row as f32 * (height + gap) + height / 2.0,
                ),
                size: vec2(width, height),
                angle: 0.0,
                skew: 0.0,
                opacity: 1.0,
                order: if index == highlight { 100 } else { 0 },
            }
        })
        .collect()
}

fn slices(count: usize, area: Rect, scale: f32, offset: impl Fn(usize) -> i32) -> Vec<Place> {
    const SLIM: f32 = 96.0;
    const WIDE: f32 = 520.0;
    const SKEW: f32 = 44.0;
    const HEIGHT: f32 = 560.0;
    let (slim, wide, skew) = (SLIM * scale, WIDE * scale, SKEW * scale);
    let middle = area.center().y + 10.0 * scale;
    let highlight_left = area.center().x - wide / 2.0;
    (0..count)
        .map(|index| {
            let k = offset(index);
            // Slices overlap by the skew, so their slanted edges meet.
            let left = match k {
                0 => highlight_left,
                k if k < 0 => highlight_left + k as f32 * (slim - skew),
                k => highlight_left + (wide - skew) + (k - 1) as f32 * (slim - skew),
            };
            let width = if k == 0 { wide } else { slim };
            let inside = left + width > area.left() && left < area.right();
            Place {
                center: pos2(left + width / 2.0, middle),
                size: vec2(width - skew, HEIGHT * scale),
                angle: 0.0,
                skew,
                opacity: if inside { 1.0 } else { 0.0 },
                order: 100 - k.abs(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window() -> Rect {
        Rect::from_min_size(Pos2::ZERO, DESIGN)
    }

    #[test]
    fn the_highlighted_card_is_upright_in_front_and_every_card_in_the_window() {
        for layout in Layout::ALL {
            for count in 1..=5 {
                for highlight in 0..count {
                    let places = place(layout, count, highlight, window(), 1.0);
                    assert_eq!(places.len(), count);
                    let chosen = places[highlight];
                    assert_eq!(chosen.angle, 0.0, "{layout:?}");
                    assert!(
                        places
                            .iter()
                            .enumerate()
                            .all(|(i, p)| i == highlight || p.order < chosen.order),
                        "{layout:?} {count} {highlight}"
                    );
                    for (index, place) in places.iter().enumerate() {
                        if place.opacity == 0.0 {
                            continue;
                        }
                        // Fanned cards may dip off the edges, as a hand
                        // of cards does.
                        let area = if layout == Layout::Fan {
                            window().expand2(vec2(200.0, 120.0))
                        } else {
                            window()
                        };
                        for corner in place.corners() {
                            assert!(
                                area.contains(corner),
                                "{layout:?} card {index} of {count}: {corner:?}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn the_fan_tilts_cards_away_from_the_highlighted_one() {
        let places = place(Layout::Fan, 5, 1, window(), 1.0);
        assert!(places[0].angle < 0.0 && places[2].angle > 0.0);
        assert!(places[3].angle > places[2].angle);
        assert!(places[1].center.y < places[2].center.y, "raised");
    }

    #[test]
    fn pointing_hits_a_card_through_its_tilt_and_slant() {
        let fan = place(Layout::Fan, 5, 1, window(), 1.0);
        let tilted = fan[3];
        assert!(tilted.contains(tilted.center));
        assert!(tilted.contains(tilted.at(vec2(4.0, 4.0))));
        assert!(!tilted.contains(tilted.at(vec2(-4.0, 4.0))));
        let slices = place(Layout::Slices, 5, 1, window(), 1.0);
        let slice = slices[2];
        let [top_left, _, _, bottom_left] = slice.corners();
        assert!(top_left.x > bottom_left.x, "the top edge sits right");
        assert!(slice.contains(slice.at(vec2(1.0, 1.0))));
        assert!(!slice.contains(pos2(bottom_left.x, top_left.y)));
    }

    #[test]
    fn small_windows_shrink_the_cards() {
        let small = Rect::from_min_size(Pos2::ZERO, vec2(720.0, 460.0));
        let places = place(Layout::Fan, 3, 1, small, 1.0);
        assert!((places[1].size.x - CARD.x / 2.0).abs() < 0.01);
    }
}
