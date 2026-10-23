//! Translated blocks revealing themselves as they arrive (frame 02m). An
//! accent scan line sweeps down each block. Just above it the translation's
//! glyphs are still scrambled accent characters settling into the text;
//! below it the page the translation replaces shows through (the GPU keeps
//! that drawing, see `gpu::RevealClip`). Blocks arriving together start one
//! after another in reading order. With Reduce motion a block fades in.

use std::collections::HashMap;

use egui::{Color32, Painter, Pos2, Rect, Stroke, pos2};
use glyphst_pdf::vector::PageRect;

use crate::{gpu::RevealClip, text_overlay::PageMap, theme};

/// Seconds a block takes, plus some for each of its lines.
const BLOCK_SECONDS: f64 = 0.45;
const LINE_SECONDS: f64 = 0.04;
/// Seconds between the starts of blocks arriving together.
const STAGGER_SECONDS: f64 = 0.08;
/// The plain fade when motion is reduced, in seconds.
const REDUCED_FADE: f64 = 0.2;
/// The scramble band's height, in the block's line heights.
const BAND_LINES: f32 = 1.4;
/// The scrambled characters' set, re-rolled every `SCRAMBLE_SECONDS`.
const SCRAMBLE: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz0123456789$#&%@*+=<>/";
const SCRAMBLE_SECONDS: f64 = 0.05;
/// How long blocks wait for their version to be drawn.
const WAIT_SECONDS: f64 = 10.0;

/// A translated block to reveal: the area it covers on the page (its
/// translation and its original) and its translated glyphs, in page points.
#[derive(Clone, Debug, PartialEq)]
pub struct RevealBlock {
    pub area: PageRect,
    pub glyphs: Vec<PageRect>,
    pub lines: usize,
}

struct Block {
    block: RevealBlock,
    /// The page's source version that shows this block translated.
    version: u64,
    /// When it was added, and when the sweep starts, once that version is
    /// drawn.
    added: f64,
    start: Option<f64>,
    /// A painted view saw it waiting, so its page's old drawing is kept.
    watched: bool,
}

impl Block {
    fn seconds(&self, reduce_motion: bool) -> f64 {
        if reduce_motion {
            REDUCED_FADE
        } else {
            BLOCK_SECONDS + LINE_SECONDS * self.block.lines as f64
        }
    }

    /// 0 to 1 through the sweep; `None` before it is due to start.
    fn progress(&self, now: f64, reduce_motion: bool) -> Option<f32> {
        let start = self.start?;
        Some(((now - start) / self.seconds(reduce_motion)).clamp(0.0, 1.0) as f32)
    }

    /// The band's height in points: a little over a line of the block.
    fn band(&self) -> f32 {
        let mut heights: Vec<f32> = self.block.glyphs.iter().map(|g| g[3] - g[1]).collect();
        heights.sort_by(f32::total_cmp);
        let line = heights.get(heights.len() / 2).copied().unwrap_or(10.0);
        line * BAND_LINES
    }

    /// Where the scan line is, in page points.
    fn scan(&self, t: f32) -> f32 {
        let [_, y0, _, y1] = self.block.area;
        y0 + ease(t) * (y1 - y0 + self.band())
    }
}

/// The reveals of one document tab, by page.
#[derive(Default)]
pub struct Reveals {
    pages: HashMap<usize, Vec<Block>>,
}

impl Reveals {
    /// Blocks of `page` that `version` of its source shows translated.
    pub fn add(&mut self, page: usize, version: u64, blocks: Vec<RevealBlock>, now: f64) {
        self.pages
            .entry(page)
            .or_default()
            .extend(blocks.into_iter().map(|block| Block {
                block,
                version,
                added: now,
                start: None,
                watched: false,
            }));
    }

    pub fn is_active(&self) -> bool {
        !self.pages.is_empty()
    }

    /// When the first sweep under way began.
    pub fn first_start(&self) -> Option<f64> {
        self.pages
            .values()
            .flatten()
            .filter_map(|block| block.start)
            .filter(|start| start.is_finite())
            .min_by(f64::total_cmp)
    }

    /// The pages with blocks to reveal.
    pub fn pages(&self) -> Vec<usize> {
        self.pages.keys().copied().collect()
    }

    /// Marks the blocks waiting for their version as seen by a painted
    /// view and returns their pages, whose current drawing the GPU should
    /// keep when it is replaced. Blocks no view saw waiting are not swept.
    pub fn watch(&mut self) -> Vec<usize> {
        let mut pages = Vec::new();
        for (&page, blocks) in &mut self.pages {
            let mut waiting = false;
            for block in blocks.iter_mut().filter(|block| block.start.is_none()) {
                block.watched = true;
                waiting = true;
            }
            if waiting {
                pages.push(page);
            }
        }
        pages
    }

    /// Starts the blocks whose version is drawn (`drawn` gives a page's
    /// drawn version) and drops the finished ones; a block whose version
    /// arrives while its page is off screen is not animated. Returns the
    /// pages done with their reveals, whose kept drawing can go.
    pub fn step(
        &mut self,
        now: f64,
        reduce_motion: bool,
        drawn: impl Fn(usize) -> Option<u64>,
        visible: impl Fn(usize) -> bool,
    ) -> Vec<usize> {
        let mut done = Vec::new();
        self.pages.retain(|&page, blocks| {
            let drawn = drawn(page);
            let mut next = now;
            let mut order: Vec<usize> = (0..blocks.len())
                .filter(|&index| {
                    blocks[index].start.is_none()
                        && drawn.is_some_and(|version| version >= blocks[index].version)
                })
                .collect();
            order.sort_by(|&a, &b| {
                let (a, b) = (&blocks[a].block.area, &blocks[b].block.area);
                (a[1], a[0])
                    .partial_cmp(&(b[1], b[0]))
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            let shown = visible(page);
            for index in order {
                // Off screen, the block is simply there when it comes back.
                let swept = shown && blocks[index].watched;
                blocks[index].start = Some(if swept { next } else { f64::NEG_INFINITY });
                if swept && !reduce_motion {
                    next += STAGGER_SECONDS;
                }
            }
            // A version that never comes (the original shown again) lets
            // its blocks go.
            blocks.retain(|block| match block.start {
                None => now - block.added < WAIT_SECONDS,
                Some(_) => block.progress(now, reduce_motion).is_none_or(|t| t < 1.0),
            });
            if blocks.is_empty() {
                done.push(page);
            }
            !blocks.is_empty()
        });
        done
    }

    /// What the GPU draws for the reveals: for each started block, the part
    /// below the scan line, where the replaced drawing shows.
    pub fn clips(&self, now: f64, reduce_motion: bool) -> Vec<RevealClip> {
        if reduce_motion {
            return Vec::new();
        }
        self.pages
            .iter()
            .flat_map(|(&page, blocks)| {
                blocks.iter().filter_map(move |block| {
                    let t = block.progress(now, false)?;
                    let [x0, y0, x1, y1] = block.block.area;
                    // Due but not yet started: still all original.
                    let scan = if now < block.start? {
                        y0
                    } else {
                        block.scan(t)
                    };
                    (scan < y1).then_some(RevealClip {
                        page,
                        below: [x0 - 1.0, scan.max(y0 - 1.0), x1 + 1.0, y1 + 1.0],
                    })
                })
            })
            .collect()
    }

    /// The scan lines and scrambled glyphs over the pages in `maps`;
    /// `paper` hides the translation's glyphs not yet settled. Returns
    /// whether anything moves.
    pub fn paint(
        &self,
        painter: &Painter,
        maps: &[PageMap],
        now: f64,
        reduce_motion: bool,
        accent: Color32,
        paper: Color32,
    ) -> bool {
        let mut moving = false;
        for map in maps {
            let Some(blocks) = self.pages.get(&map.page) else {
                continue;
            };
            for block in blocks {
                let Some(t) = block.progress(now, reduce_motion) else {
                    continue;
                };
                let started = block.start.is_some_and(|start| now >= start);
                moving = true;
                if reduce_motion {
                    let area = map.rect_to_screen(block.block.area);
                    painter.rect_filled(area, 0, paper.gamma_multiply(1.0 - smooth(t)));
                    continue;
                }
                if !started {
                    continue;
                }
                paint_sweep(painter, map, block, t, now, accent, paper);
            }
        }
        moving
    }
}

fn paint_sweep(
    painter: &Painter,
    map: &PageMap,
    block: &Block,
    t: f32,
    now: f64,
    accent: Color32,
    paper: Color32,
) {
    let scan = block.scan(t);
    let band = block.band();
    let tick = (now / SCRAMBLE_SECONDS).floor() as u32;
    // The glyphs in the band, scrambled until the scan has passed far
    // enough above them.
    for (index, glyph) in block.block.glyphs.iter().enumerate() {
        let middle = (glyph[1] + glyph[3]) / 2.0;
        if middle >= scan || middle < scan - band {
            continue;
        }
        let settled = (scan - middle) / band;
        if hash(index as u32, 7) < settled * 1.05 - 0.05 {
            continue;
        }
        let rect = map.rect_to_screen(*glyph).expand(0.5);
        painter.rect_filled(rect, 0, paper);
        let character = SCRAMBLE
            [(hash(index as u32, tick) * SCRAMBLE.len() as f32) as usize % SCRAMBLE.len()]
            as char;
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            character,
            theme::mono_strong((rect.height() * 0.8).max(4.0)),
            accent,
        );
    }
    // The scan line and its glow, fading out as it leaves the block.
    let [x0, y0, x1, y1] = block.block.area;
    let leaving = 1.0 - smooth(((scan - y1) / band.max(1.0)).clamp(0.0, 1.0));
    if leaving <= 0.0 {
        return;
    }
    let line = map.rect_to_screen([x0 - 4.0, y0, x1 + 4.0, scan.min(y1 + band)]);
    let glow_height = (map.rect_to_screen([0.0, 0.0, 1.0, band]).height() * 1.4).max(8.0);
    let bottom = line.bottom();
    let glow = Rect::from_min_max(
        pos2(line.left(), bottom - glow_height),
        pos2(line.right(), bottom),
    );
    vertical_glow(painter, glow, accent.gamma_multiply(0.22 * leaving));
    painter.line_segment(
        [pos2(line.left(), bottom), pos2(line.right(), bottom)],
        Stroke::new(1.5, accent.gamma_multiply(leaving)),
    );
}

/// A rectangle fading from clear at the top to `color` at the bottom.
fn vertical_glow(painter: &Painter, rect: Rect, color: Color32) {
    let mut mesh = egui::Mesh::default();
    let clear = Color32::TRANSPARENT;
    let corners: [(Pos2, Color32); 4] = [
        (rect.left_top(), clear),
        (rect.right_top(), clear),
        (rect.right_bottom(), color),
        (rect.left_bottom(), color),
    ];
    for (pos, color) in corners {
        mesh.colored_vertex(pos, color);
    }
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    painter.add(mesh);
}

fn ease(t: f32) -> f32 {
    if t < 0.5 {
        4.0 * t * t * t
    } else {
        1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
    }
}

fn smooth(t: f32) -> f32 {
    t * t * (3.0 - 2.0 * t)
}

/// A repeatable value in 0..1 for `index` at `salt`.
fn hash(index: u32, salt: u32) -> f32 {
    let mut h = index.wrapping_mul(374_761_393) ^ salt.wrapping_mul(668_265_263);
    h = (h ^ (h >> 13)).wrapping_mul(1_274_126_177);
    (h ^ (h >> 16)) as f32 / u32::MAX as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(y0: f32, lines: usize) -> RevealBlock {
        RevealBlock {
            area: [10.0, y0, 200.0, y0 + 12.0 * lines as f32],
            glyphs: (0..lines)
                .map(|line| {
                    let y = y0 + 12.0 * line as f32;
                    [10.0, y, 18.0, y + 10.0]
                })
                .collect(),
            lines,
        }
    }

    #[test]
    fn blocks_start_once_drawn_one_after_another_and_end() {
        let mut reveals = Reveals::default();
        reveals.add(3, 5, vec![block(200.0, 2), block(100.0, 1)], 0.0);
        assert_eq!(reveals.watch(), vec![3]);
        // The new version is not drawn yet: nothing moves, the page shows
        // its old drawing as it is.
        assert!(reveals.step(0.0, false, |_| Some(4), |_| true).is_empty());
        assert!(reveals.clips(0.0, false).is_empty());

        reveals.step(1.0, false, |_| Some(5), |_| true);
        assert!(reveals.watch().is_empty());
        let clips = reveals.clips(1.0, false);
        assert_eq!(clips.len(), 2, "both blocks show their original at first");
        // The upper block goes first.
        let upper = clips.iter().find(|clip| clip.below[1] < 150.0).unwrap();
        assert!(upper.below[1] <= 100.0);
        let later = reveals.clips(1.0 + BLOCK_SECONDS * 0.6, false);
        let upper = later.iter().find(|clip| clip.below[3] < 150.0);
        assert!(upper.is_none_or(|clip| clip.below[1] > 100.0), "{later:?}");

        assert!(reveals.step(1.5, false, |_| Some(5), |_| true).is_empty());
        assert_eq!(reveals.step(3.0, false, |_| Some(5), |_| true), vec![3]);
        assert!(!reveals.is_active());
    }

    #[test]
    fn a_block_landing_off_screen_is_not_animated() {
        let mut reveals = Reveals::default();
        reveals.add(0, 2, vec![block(50.0, 3)], 0.0);
        reveals.watch();
        assert_eq!(reveals.step(1.0, false, |_| Some(2), |_| false), vec![0]);
        assert!(reveals.clips(1.0, false).is_empty());
        // Nor is one no view saw waiting (its tab was in the background).
        reveals.add(0, 3, vec![block(50.0, 3)], 1.0);
        assert_eq!(reveals.step(2.0, false, |_| Some(3), |_| true), vec![0]);
    }

    #[test]
    fn blocks_waiting_for_a_version_that_never_comes_are_let_go() {
        let mut reveals = Reveals::default();
        reveals.add(1, 4, vec![block(50.0, 1)], 0.0);
        reveals.watch();
        assert!(reveals.step(5.0, false, |_| Some(0), |_| true).is_empty());
        assert_eq!(
            reveals.step(WAIT_SECONDS + 0.1, false, |_| Some(0), |_| true),
            vec![1]
        );
    }

    #[test]
    fn reduced_motion_fades_without_keeping_the_old_page() {
        let mut reveals = Reveals::default();
        reveals.add(0, 2, vec![block(50.0, 3)], 0.0);
        reveals.watch();
        reveals.step(0.0, true, |_| Some(2), |_| true);
        assert!(reveals.clips(0.05, true).is_empty());
        assert!(reveals.step(0.1, true, |_| Some(2), |_| true).is_empty());
        assert_eq!(
            reveals.step(REDUCED_FADE + 0.01, true, |_| Some(2), |_| true),
            vec![0]
        );
    }

    #[test]
    fn the_scan_passes_the_whole_block() {
        let block = Block {
            block: block(100.0, 3),
            version: 1,
            added: 0.0,
            start: Some(0.0),
            watched: true,
        };
        assert_eq!(block.scan(0.0), 100.0);
        assert!(block.scan(1.0) >= 136.0 + block.band() - 0.01);
        assert!(block.scan(0.5) > 100.0 && block.scan(0.5) < 136.0 + block.band());
    }
}
