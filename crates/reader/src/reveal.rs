//! Translated blocks decoding in place as they arrive (frames 02m, 02o).
//! When a block's translation lands, its original fades out while
//! scrambled accent characters fade in at the translation's own character
//! positions; then each line resolves left to right, later lines starting
//! later, until the block reads as its translation. Blocks arriving
//! together start one after another in reading order. With Reduce motion
//! the original fades out, then the translation fades in.
//!
//! The translation never shows before its character resolves: the GPU
//! keeps the stencil reveal bit over every unresolved character, where the
//! page's base (the page without its translation) is drawn instead, and
//! the replaced drawing fades out above it (see `gpu::PageReveal`).

use std::collections::HashMap;

use egui::{Color32, Painter};
use glyphst_pdf::vector::PageRect;

use crate::{gpu::PageReveal, text_overlay::PageMap, theme};

/// Seconds the original takes to fade out before any character resolves.
const FADE_SECONDS: f64 = 0.12;
/// Seconds the decoding takes, plus some for each line.
const BLOCK_SECONDS: f64 = 0.45;
const LINE_SECONDS: f64 = 0.04;
/// Seconds between the starts of blocks arriving together.
const STAGGER_SECONDS: f64 = 0.08;
/// Reduce motion: the original fades out, then the translation fades in.
const REDUCED_SECONDS: f64 = 0.2;
/// How long a block whose translation is drawn waits for its page's base
/// before it simply shows, unanimated.
const HOLD_SECONDS: f64 = 4.0;
/// How long blocks wait for their version to be drawn.
const WAIT_SECONDS: f64 = 10.0;
/// The scrambled characters' set, re-rolled every `SCRAMBLE_SECONDS`.
const SCRAMBLE: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz0123456789$#&%@*+=<>/";
const SCRAMBLE_SECONDS: f64 = 0.05;
/// The parts of a cell's resolve time from its line, its place in the
/// line and chance; they add up to under 1.
const LINE_SHARE: f32 = 0.35;
const PLACE_SHARE: f32 = 0.5;
const CHANCE_SHARE: f32 = 0.15;

/// A line of a translated block: its box (a little larger than its
/// glyphs) and its characters' cells, each with the glyph inside it, in
/// page points. Cells cover the line between the midpoints of the gaps
/// around their glyph.
#[derive(Clone, Debug, PartialEq)]
pub struct RevealLine {
    pub rect: PageRect,
    pub cells: Vec<PageRect>,
    pub glyphs: Vec<PageRect>,
}

/// A translated block to reveal: the area it covers on the page (its
/// translation and its original), its original text's box and its
/// translated lines.
#[derive(Clone, Debug, PartialEq)]
pub struct RevealBlock {
    pub area: PageRect,
    pub original: PageRect,
    pub lines: Vec<RevealLine>,
}

impl RevealBlock {
    /// The block's lines from its glyphs: `lines` lists, for each text
    /// line, the glyph boxes in reading order.
    pub fn from_glyphs(area: PageRect, original: PageRect, lines: Vec<Vec<PageRect>>) -> Self {
        let mut lines: Vec<(PageRect, Vec<PageRect>)> = lines
            .into_iter()
            .filter(|glyphs| !glyphs.is_empty())
            .map(|mut glyphs| {
                glyphs.sort_by(|a, b| a[0].total_cmp(&b[0]));
                (line_box(&glyphs), glyphs)
            })
            .collect();
        // Lines set close together meet halfway: a cell never reaches
        // into the next line's characters.
        let bounds: Vec<PageRect> = lines
            .iter()
            .map(|(_, glyphs)| glyphs.iter().copied().reduce(union).unwrap_or([0.0; 4]))
            .collect();
        for upper in 0..lines.len() {
            for lower in 0..lines.len() {
                let (a, b) = (bounds[upper], bounds[lower]);
                let above = (a[1] + a[3]) < (b[1] + b[3]);
                let across = a[0] < b[2] && b[0] < a[2];
                if upper == lower || !above || !across || lines[upper].0[3] <= lines[lower].0[1] {
                    continue;
                }
                let middle = (a[3] + b[1]) / 2.0;
                lines[upper].0[3] = lines[upper].0[3].min(middle);
                lines[lower].0[1] = lines[lower].0[1].max(middle);
            }
        }
        let lines: Vec<RevealLine> = lines
            .into_iter()
            .map(|(rect, glyphs)| line_cells(rect, glyphs))
            .collect();
        let area = lines.iter().fold(area, |area, line| union(area, line.rect));
        Self {
            area,
            original,
            lines,
        }
    }
}

/// A line's box: its glyphs', a little larger.
fn line_box(glyphs: &[PageRect]) -> PageRect {
    let mut heights: Vec<f32> = glyphs.iter().map(|g| g[3] - g[1]).collect();
    heights.sort_by(f32::total_cmp);
    let em = heights.get(heights.len() / 2).copied().unwrap_or(10.0);
    let bounds = glyphs.iter().copied().reduce(union).unwrap_or([0.0; 4]);
    [
        bounds[0] - 0.08 * em,
        bounds[1] - 0.15 * em,
        bounds[2] + 0.08 * em,
        bounds[3] + 0.15 * em,
    ]
}

/// A line's cells, from its box and its glyphs left to right.
fn line_cells(rect: PageRect, glyphs: Vec<PageRect>) -> RevealLine {
    let cells = (0..glyphs.len())
        .map(|index| {
            let left = match index {
                0 => rect[0],
                _ => (glyphs[index - 1][2] + glyphs[index][0]) / 2.0,
            };
            let right = match glyphs.get(index + 1) {
                Some(next) => (glyphs[index][2] + next[0]) / 2.0,
                None => rect[2],
            };
            [left, rect[1], right.max(left), rect[3]]
        })
        .collect();
    RevealLine {
        rect,
        cells,
        glyphs,
    }
}

fn union(a: PageRect, b: PageRect) -> PageRect {
    [
        a[0].min(b[0]),
        a[1].min(b[1]),
        a[2].max(b[2]),
        a[3].max(b[3]),
    ]
}

/// How scripted checks show reveals: as they are, without the scrambled
/// characters (and the statistics), or with every character masked.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RevealView {
    #[default]
    Normal,
    Bare,
    Base,
}

/// What the GPU has of a page, for starting its blocks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PageState {
    /// The source version of the page's drawing.
    pub drawn: Option<u64>,
    /// The version of the page's base.
    pub base: Option<u64>,
    /// Whether the page can be masked (its clips are shallow enough).
    pub maskable: bool,
}

/// Where a block is at a moment.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Phase {
    /// Translated but not started: its original still shows.
    Hold,
    /// The original fading out (0 to 1) while the scramble fades in.
    Fade(f32),
    /// Seconds into decoding.
    Decode(f64),
    /// Reduce motion: the original fading out, then the translation in.
    Out(f32),
    In(f32),
    Done,
}

struct Block {
    block: RevealBlock,
    /// The page's source version that shows this block translated.
    version: u64,
    /// When it was added, when its translation was first drawn, and when
    /// its reveal starts.
    added: f64,
    held: Option<f64>,
    start: Option<f64>,
    /// A painted view saw it waiting, so its page's old drawing is kept.
    watched: bool,
}

impl Block {
    fn decode_seconds(&self) -> f64 {
        BLOCK_SECONDS + LINE_SECONDS * self.block.lines.len() as f64
    }

    fn seconds(&self, reduce_motion: bool) -> f64 {
        if reduce_motion {
            REDUCED_SECONDS
        } else {
            FADE_SECONDS + self.decode_seconds()
        }
    }

    fn phase(&self, now: f64, reduce_motion: bool) -> Phase {
        let start = match self.start {
            None if self.held.is_some() => return Phase::Hold,
            None => return Phase::Done,
            Some(start) => start,
        };
        if now < start {
            return Phase::Hold;
        }
        let elapsed = now - start;
        if elapsed >= self.seconds(reduce_motion) {
            return Phase::Done;
        }
        if reduce_motion {
            let half = REDUCED_SECONDS / 2.0;
            return if elapsed < half {
                Phase::Out((elapsed / half) as f32)
            } else {
                Phase::In(((elapsed - half) / half) as f32)
            };
        }
        if elapsed < FADE_SECONDS {
            Phase::Fade((elapsed / FADE_SECONDS) as f32)
        } else {
            Phase::Decode(elapsed - FADE_SECONDS)
        }
    }

    /// Whether cell `cell` of line `line` has resolved `decoding` seconds
    /// into decoding.
    fn resolved(&self, line: usize, cell: usize, decoding: f64) -> bool {
        let lines = self.block.lines.len().max(1) as f32;
        let cells = self.block.lines[line].cells.len().max(1) as f32;
        let at = LINE_SHARE * line as f32 / lines
            + PLACE_SHARE * cell as f32 / cells
            + CHANCE_SHARE * hash((line as u32) << 16 | cell as u32, 11);
        decoding / self.decode_seconds() >= f64::from(at)
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
                held: None,
                start: None,
                watched: false,
            }));
    }

    pub fn is_active(&self) -> bool {
        !self.pages.is_empty()
    }

    /// When the first reveal under way began.
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
    /// keep when it is replaced. Blocks no view saw waiting are not
    /// revealed.
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

    /// Starts the blocks whose translation and base are drawn (`state`
    /// gives a page's) and drops the finished ones. A block whose version
    /// arrives while its page is off screen, or whose base never comes, is
    /// not animated. Returns the pages done with their reveals, whose kept
    /// drawings can go.
    pub fn step(
        &mut self,
        now: f64,
        reduce_motion: bool,
        state: impl Fn(usize) -> PageState,
        visible: impl Fn(usize) -> bool,
    ) -> Vec<usize> {
        let mut done = Vec::new();
        self.pages.retain(|&page, blocks| {
            let state = state(page);
            let shown = visible(page);
            let mut order: Vec<usize> = (0..blocks.len())
                .filter(|&index| {
                    blocks[index].start.is_none()
                        && state
                            .drawn
                            .is_some_and(|drawn| drawn >= blocks[index].version)
                })
                .collect();
            order.sort_by(|&a, &b| {
                let (a, b) = (&blocks[a].block.area, &blocks[b].block.area);
                (a[1], a[0])
                    .partial_cmp(&(b[1], b[0]))
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            let mut next = now;
            for index in order {
                let block = &mut blocks[index];
                // Off screen, the block is simply there when it comes back.
                if !shown || !block.watched {
                    block.start = Some(f64::NEG_INFINITY);
                    continue;
                }
                let held = *block.held.get_or_insert(now);
                let based = state.base.is_some_and(|base| base >= block.version);
                if based && state.maskable {
                    block.start = Some(next);
                    next += STAGGER_SECONDS;
                } else if now - held >= HOLD_SECONDS || !state.maskable {
                    block.start = Some(f64::NEG_INFINITY);
                }
            }
            // A version that never comes (the original shown again) lets
            // its blocks go.
            blocks.retain(|block| match block.start {
                None => now - block.added < WAIT_SECONDS,
                Some(_) => block.phase(now, reduce_motion) != Phase::Done,
            });
            if blocks.is_empty() {
                done.push(page);
            }
            !blocks.is_empty()
        });
        done
    }

    /// What the GPU draws for each page's reveals at `now`.
    pub fn frame(&self, now: f64, reduce_motion: bool) -> Vec<PageReveal> {
        let mut reveals: Vec<PageReveal> = self
            .pages
            .iter()
            .map(|(&page, blocks)| {
                let mut reveal = PageReveal {
                    page,
                    ..PageReveal::default()
                };
                for block in blocks {
                    let (area, original) = (block.block.area, block.block.original);
                    let lines = || block.block.lines.iter().map(|line| line.rect);
                    match block.phase(now, reduce_motion) {
                        Phase::Hold => {
                            reveal.mask.extend(lines());
                            reveal.hold.push(original);
                        }
                        Phase::Fade(fade) | Phase::Out(fade) => {
                            reveal.mask.extend(lines());
                            reveal.fade_out.push((original, 1.0 - smooth(fade)));
                        }
                        Phase::In(fade) => {
                            reveal.mask.extend(lines());
                            reveal.fade_in.push((area, smooth(fade)));
                        }
                        Phase::Decode(decoding) => {
                            reveal.mask.extend(lines());
                            for (index, line) in block.block.lines.iter().enumerate() {
                                reveal.unmask.extend(
                                    line.cells
                                        .iter()
                                        .enumerate()
                                        .filter(|(cell, _)| block.resolved(index, *cell, decoding))
                                        .map(|(_, cell)| *cell),
                                );
                            }
                        }
                        Phase::Done => {}
                    }
                }
                reveal
            })
            .filter(|reveal| !reveal.is_empty())
            .collect();
        reveals.sort_by_key(|reveal| reveal.page);
        reveals
    }

    /// Whether every block has started (or been let go): nothing waits for
    /// its drawing or base.
    pub fn all_started(&self) -> bool {
        self.pages
            .values()
            .flatten()
            .all(|block| block.start.is_some())
    }

    /// [`Reveals::frame`] with every block's characters masked and nothing
    /// fading: the pages' bases where the translations go (for checking
    /// reveals against them).
    pub fn frame_masked(&self) -> Vec<PageReveal> {
        let mut reveals: Vec<PageReveal> = self
            .pages
            .iter()
            .map(|(&page, blocks)| PageReveal {
                page,
                mask: blocks
                    .iter()
                    .flat_map(|block| block.block.lines.iter().map(|line| line.rect))
                    .collect(),
                ..PageReveal::default()
            })
            .filter(|reveal| !reveal.is_empty())
            .collect();
        reveals.sort_by_key(|reveal| reveal.page);
        reveals
    }

    /// The cells of the blocks decoding at `now`: page, cell and whether it
    /// has resolved.
    pub fn cells_at(&self, now: f64) -> Vec<(usize, PageRect, bool)> {
        let mut cells = Vec::new();
        for (&page, blocks) in &self.pages {
            for block in blocks {
                let Phase::Decode(decoding) = block.phase(now, false) else {
                    continue;
                };
                for (index, line) in block.block.lines.iter().enumerate() {
                    for (cell, rect) in line.cells.iter().enumerate() {
                        cells.push((page, *rect, block.resolved(index, cell, decoding)));
                    }
                }
            }
        }
        cells
    }

    /// The scrambled characters over the unresolved cells of the pages in
    /// `maps`. Returns whether anything moves.
    pub fn paint(
        &self,
        painter: &Painter,
        maps: &[PageMap],
        now: f64,
        reduce_motion: bool,
        accent: Color32,
    ) -> bool {
        let mut moving = false;
        let tick = (now / SCRAMBLE_SECONDS).floor() as u32;
        for map in maps {
            let Some(blocks) = self.pages.get(&map.page) else {
                continue;
            };
            for block in blocks {
                let phase = block.phase(now, reduce_motion);
                moving |= phase != Phase::Done;
                let (alpha, decoding) = match phase {
                    Phase::Fade(fade) => (smooth(fade), None),
                    Phase::Decode(decoding) => (1.0, Some(decoding)),
                    _ => continue,
                };
                for (index, line) in block.block.lines.iter().enumerate() {
                    for (cell, glyph) in line.glyphs.iter().enumerate() {
                        if decoding.is_some_and(|decoding| block.resolved(index, cell, decoding)) {
                            continue;
                        }
                        let rect = map.rect_to_screen(*glyph);
                        let seed = hash((index as u32) << 16 | cell as u32, tick);
                        let character =
                            SCRAMBLE[(seed * SCRAMBLE.len() as f32) as usize % SCRAMBLE.len()];
                        painter.text(
                            rect.center(),
                            egui::Align2::CENTER_CENTER,
                            character as char,
                            theme::mono_strong((rect.height() * 0.8).max(4.0)),
                            accent.gamma_multiply(alpha),
                        );
                    }
                }
            }
        }
        moving
    }
}

fn smooth(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
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

    /// A block of `lines` lines of `cells` glyphs from `y0`.
    fn block(y0: f32, lines: usize, cells: usize) -> RevealBlock {
        let glyphs = (0..lines)
            .map(|line| {
                let y = y0 + 12.0 * line as f32;
                (0..cells)
                    .map(|cell| {
                        let x = 10.0 + 8.0 * cell as f32;
                        [x, y, x + 6.0, y + 10.0]
                    })
                    .collect()
            })
            .collect();
        let area = [10.0, y0, 200.0, y0 + 12.0 * lines as f32];
        RevealBlock::from_glyphs(area, area, glyphs)
    }

    const READY: PageState = PageState {
        drawn: Some(5),
        base: Some(5),
        maskable: true,
    };

    fn started(reveals: &mut Reveals, at: f64, reduce: bool) {
        reveals.watch();
        reveals.step(at, reduce, |_| READY, |_| true);
    }

    #[test]
    fn cells_cover_their_line_between_the_gaps() {
        let block = block(100.0, 1, 3);
        let line = &block.lines[0];
        assert_eq!(line.cells.len(), 3);
        assert_eq!(line.cells[0][0], line.rect[0]);
        assert_eq!(line.cells[0][2], 17.0, "midway between 16 and 18");
        assert_eq!(line.cells[1][0], line.cells[0][2]);
        assert_eq!(line.cells[2][2], line.rect[2]);
        assert!(line.rect[1] < 100.0 && line.rect[3] > 110.0);
    }

    #[test]
    fn lines_set_close_together_meet_halfway() {
        // Glyph boxes 10 pt tall on lines 9 pt apart: they overlap by 1 pt.
        let glyphs = vec![
            vec![[10.0, 100.0, 16.0, 110.0]],
            vec![[10.0, 109.0, 16.0, 119.0]],
        ];
        let block = RevealBlock::from_glyphs([0.0; 4], [0.0; 4], glyphs);
        let (upper, lower) = (&block.lines[0], &block.lines[1]);
        assert_eq!(upper.rect[3], 109.5);
        assert_eq!(lower.rect[1], 109.5);
        assert_eq!(upper.cells[0][3], 109.5);
        assert_eq!(lower.cells[0][1], 109.5);
    }

    #[test]
    fn nothing_resolves_during_the_fade_and_everything_by_the_end() {
        for lines in 1..=12 {
            let mut reveals = Reveals::default();
            reveals.add(0, 5, vec![block(100.0, lines, 9)], 0.0);
            started(&mut reveals, 1.0, false);
            let decode = BLOCK_SECONDS + LINE_SECONDS * lines as f64;
            let total = FADE_SECONDS + decode;
            let mut t = 1.0;
            while t < 1.0 + total + 0.05 {
                let frame = reveals.frame(t, false);
                let elapsed = t - 1.0;
                let reveal = frame.first();
                if elapsed < FADE_SECONDS {
                    let reveal = reveal.unwrap();
                    assert!(reveal.unmask.is_empty(), "{lines} lines at {elapsed}");
                    assert_eq!(reveal.mask.len(), lines);
                    assert_eq!(reveal.fade_out.len(), 1);
                } else if elapsed < total {
                    let reveal = reveal.unwrap();
                    assert!(
                        reveal.fade_out.is_empty(),
                        "the original is gone once characters resolve"
                    );
                    assert!(reveal.unmask.len() <= lines * 9);
                } else {
                    assert!(reveal.is_none_or(|reveal| reveal.unmask.len() == lines * 9));
                }
                t += 0.001;
            }
            // Just before the end every cell has resolved.
            let last = reveals.frame(1.0 + total - 0.0005, false);
            assert_eq!(last[0].unmask.len(), lines * 9, "{lines} lines");
        }
    }

    #[test]
    fn blocks_wait_for_the_translation_and_its_base_then_go_in_reading_order() {
        let mut reveals = Reveals::default();
        reveals.add(3, 5, vec![block(200.0, 2, 4), block(100.0, 1, 4)], 0.0);
        assert_eq!(reveals.watch(), vec![3]);
        // The new version is not drawn yet: the old drawing shows as it is.
        let old = PageState {
            drawn: Some(4),
            ..READY
        };
        assert!(reveals.step(0.0, false, |_| old, |_| true).is_empty());
        assert!(reveals.frame(0.0, false).is_empty());
        // Drawn, but no base yet: both hold the original.
        let unbased = PageState {
            base: None,
            ..READY
        };
        reveals.step(0.5, false, |_| unbased, |_| true);
        assert_eq!(reveals.frame(0.5, false)[0].hold.len(), 2);
        // With the base, the upper block starts; the lower one a stagger
        // later.
        reveals.step(0.6, false, |_| READY, |_| true);
        let frame = &reveals.frame(0.61, false)[0];
        assert_eq!(frame.fade_out.len(), 1);
        assert!(frame.fade_out[0].0[1] < 150.0, "the upper block goes first");
        assert_eq!(frame.hold.len(), 1);
        assert_eq!(reveals.step(3.0, false, |_| READY, |_| true), vec![3]);
        assert!(!reveals.is_active());
    }

    #[test]
    fn a_base_that_never_comes_lets_the_block_show_unanimated() {
        let mut reveals = Reveals::default();
        reveals.add(1, 5, vec![block(50.0, 2, 4)], 0.0);
        reveals.watch();
        let unbased = PageState {
            base: None,
            ..READY
        };
        reveals.step(0.1, false, |_| unbased, |_| true);
        assert_eq!(reveals.frame(0.2, false)[0].hold.len(), 1);
        assert_eq!(
            reveals.step(0.2 + HOLD_SECONDS, false, |_| unbased, |_| true),
            vec![1]
        );
        // Nor is a page too deeply clipped to mask.
        let later = 0.3 + HOLD_SECONDS;
        reveals.add(1, 5, vec![block(50.0, 2, 4)], later);
        reveals.watch();
        let unmaskable = PageState {
            maskable: false,
            ..READY
        };
        assert_eq!(
            reveals.step(later + 0.1, false, |_| unmaskable, |_| true),
            vec![1]
        );
    }

    #[test]
    fn a_block_landing_off_screen_or_unwatched_is_not_animated() {
        let mut reveals = Reveals::default();
        reveals.add(0, 5, vec![block(50.0, 3, 4)], 0.0);
        reveals.watch();
        assert_eq!(reveals.step(1.0, false, |_| READY, |_| false), vec![0]);
        reveals.add(0, 5, vec![block(50.0, 3, 4)], 1.0);
        assert_eq!(reveals.step(2.0, false, |_| READY, |_| true), vec![0]);
    }

    #[test]
    fn blocks_waiting_for_a_version_that_never_comes_are_let_go() {
        let mut reveals = Reveals::default();
        reveals.add(1, 4, vec![block(50.0, 1, 4)], 0.0);
        reveals.watch();
        let old = PageState {
            drawn: Some(0),
            ..READY
        };
        assert!(reveals.step(5.0, false, |_| old, |_| true).is_empty());
        assert_eq!(
            reveals.step(WAIT_SECONDS + 0.1, false, |_| old, |_| true),
            vec![1]
        );
    }

    #[test]
    fn reduced_motion_never_shows_both_texts() {
        let mut reveals = Reveals::default();
        reveals.add(0, 5, vec![block(50.0, 3, 4)], 0.0);
        started(&mut reveals, 0.0, true);
        let mut t = 0.0;
        while t < REDUCED_SECONDS {
            let frame = &reveals.frame(t, true)[0];
            assert!(
                frame.fade_out.is_empty() || frame.fade_in.is_empty(),
                "at {t}"
            );
            assert!(frame.unmask.is_empty());
            assert_eq!(frame.mask.len(), 3);
            t += 0.001;
        }
        assert!(reveals.frame(REDUCED_SECONDS + 0.001, true).is_empty());
    }
}
