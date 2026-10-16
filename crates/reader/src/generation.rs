//! The generation animation over pages that take a while to load: a dot
//! field, construction guides and the page's own structure traced in small
//! flowing dots, then the page revealed behind a scan line.
//!
//! A port of transitions.dev's UI generation (skeleton mode) to egui, with
//! its phases, easings, spacing and palette. The original plays a fixed
//! 2.6 s build over finished UI; here the page's end is unknown, so the
//! build holds (the dots keep flowing, the reticle visits the key parts)
//! until the page can be drawn, and the reveal starts then. A page drawn
//! within [`GRACE`], or with a thumbnail to show, gets no animation.
//!
//! The structure comes from [`PageSketch`], read from the captured vectors
//! before the slow tessellation.

use std::{
    collections::{HashMap, HashSet},
    f32::consts::TAU,
    sync::Arc,
    time::{Duration, Instant},
};

use egui::{Color32, Pos2, Rect, Vec2, pos2, vec2};
use glyphst_pdf::vector::{PagePath, PageRect, PathVerb, VectorItem, VectorPage};

use crate::{gpu::PageKey, text_overlay::PageMap, theme::Tokens};

/// A page still loading after this long starts the animation.
pub const GRACE: Duration = Duration::from_millis(150);
/// Pages smaller than this on screen (either side, in points) get none:
/// the animation is a moment for a page being read, not for a grid.
const MIN_SIDE: f32 = 120.0;

// ─── the structure of a page ───

/// Most parts traced; the largest are kept, in drawing order.
const MAX_PARTS: usize = 60;
/// Key parts get their own guides, corner handles and size label.
const KEYS: usize = 5;
/// Longest stroked subpaths traced as paths.
const MAX_PATHS: usize = 24;
/// Points kept of a traced path.
const PATH_POINTS: usize = 64;
/// Fills shorter than this share of the page's height (and narrower than
/// [`GLYPH_WIDTH`]) are taken as glyphs and merged into text lines.
const GLYPH_HEIGHT: f32 = 0.03;
const GLYPH_WIDTH: f32 = 0.1;
/// Boxes over this share of the page are backgrounds.
const BACKGROUND: f32 = 0.6;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PartKind {
    Box,
    Text,
    Path,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SketchPart {
    pub kind: PartKind,
    /// "image", "frame", "text" or "path", for the reticle's tag.
    pub name: &'static str,
    /// In page points.
    pub rect: PageRect,
    /// Text: one segment per line, a little below its middle. Path: its
    /// polylines. Boxes are traced from `rect`.
    pub lines: Vec<Vec<[f32; 2]>>,
    pub key: bool,
}

/// What the animation traces of a page: its boxes (images and filled
/// areas), text lines and longest strokes, in page points.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PageSketch {
    pub parts: Vec<SketchPart>,
}

impl PageSketch {
    pub fn from_vector(page: &VectorPage) -> Self {
        let [px0, py0, px1, py1] = page.bounds;
        let (page_width, page_height) = (px1 - px0, py1 - py0);
        let page_area = (page_width * page_height).max(1.0);
        // Candidates with their drawing order.
        let mut boxes: Vec<(usize, PageRect, &'static str)> = Vec::new();
        let mut glyphs: Vec<(usize, PageRect)> = Vec::new();
        let mut paths: Vec<(usize, f32, Vec<[f32; 2]>)> = Vec::new();
        for (order, draw) in page.draws.iter().enumerate() {
            let clipped = |rect: PageRect| match draw.clip {
                Some(clip) => intersect(rect, clip),
                None => (rect[2] > rect[0] || rect[3] > rect[1]).then_some(rect),
            };
            match &draw.item {
                VectorItem::Fill { path, .. } => {
                    let Some(rect) = path_bounds(path).and_then(clipped) else {
                        continue;
                    };
                    let (width, height) = (rect[2] - rect[0], rect[3] - rect[1]);
                    if height < page_height * GLYPH_HEIGHT && width < page_width * GLYPH_WIDTH {
                        glyphs.push((order, rect));
                    } else {
                        boxes.push((order, rect, "frame"));
                    }
                }
                VectorItem::Stroke { path, .. } => {
                    for line in polylines(path) {
                        let length = polyline_length(&line);
                        if length >= 2.0 {
                            paths.push((order, length, line));
                        }
                    }
                }
                VectorItem::Image { transform, .. } => {
                    let [a, b, c, d, e, f] = *transform;
                    let corners = [
                        [e, f],
                        [a + e, b + f],
                        [c + e, d + f],
                        [a + c + e, b + d + f],
                    ];
                    if let Some(rect) = points_bounds(&corners).and_then(clipped) {
                        boxes.push((order, rect, "image"));
                    }
                }
                VectorItem::Raster { bounds, .. } => {
                    if let Some(rect) = clipped(*bounds) {
                        boxes.push((order, rect, "image"));
                    }
                }
                VectorItem::ClipPush { .. } | VectorItem::ClipPop => {}
            }
        }

        let mut parts: Vec<(usize, SketchPart)> = Vec::new();
        for (order, rect, name) in boxes {
            let (width, height) = (rect[2] - rect[0], rect[3] - rect[1]);
            let area = width * height;
            let small = width < 2.0 && height < 2.0;
            // A hairline or a long thin fill (a wall, a rule) is traced
            // as a line.
            let thin = width.min(height) < 2.0 || width.max(height) > 20.0 * width.min(height);
            if thin && !small {
                let line = if width > height {
                    let y = (rect[1] + rect[3]) / 2.0;
                    vec![[rect[0], y], [rect[2], y]]
                } else {
                    let x = (rect[0] + rect[2]) / 2.0;
                    vec![[x, rect[1]], [x, rect[3]]]
                };
                paths.push((order, width.max(height), line));
                continue;
            }
            // The same box twice (an image and its frame) is drawn once.
            let seen = parts
                .iter()
                .any(|(_, part)| part.rect.iter().zip(rect).all(|(a, b)| (a - b).abs() < 1.5));
            if area > page_area * BACKGROUND || small || seen {
                continue;
            }
            parts.push((
                order,
                SketchPart {
                    kind: PartKind::Box,
                    name,
                    rect,
                    lines: Vec::new(),
                    key: false,
                },
            ));
        }
        parts.extend(text_parts(&glyphs));
        paths.sort_by(|a, b| b.1.total_cmp(&a.1));
        for (order, _, line) in paths.into_iter().take(MAX_PATHS) {
            let Some(rect) = points_bounds(&line) else {
                continue;
            };
            parts.push((
                order,
                SketchPart {
                    kind: PartKind::Path,
                    name: "path",
                    rect,
                    lines: vec![decimate(line, PATH_POINTS)],
                    key: false,
                },
            ));
        }

        // Too many parts: keep the largest, in drawing order.
        let extent = |part: &SketchPart| {
            let [x0, y0, x1, y1] = part.rect;
            (x1 - x0).max(1.0) * (y1 - y0).max(1.0)
        };
        if parts.len() > MAX_PARTS {
            parts.sort_by(|a, b| extent(&b.1).total_cmp(&extent(&a.1)));
            parts.truncate(MAX_PARTS);
        }
        parts.sort_by_key(|(order, _)| *order);
        let mut parts: Vec<SketchPart> = parts.into_iter().map(|(_, part)| part).collect();

        // Key parts: images first, then the largest boxes.
        let mut keys: Vec<usize> = (0..parts.len())
            .filter(|&i| parts[i].kind == PartKind::Box)
            .collect();
        keys.sort_by(|&a, &b| {
            let image = |i: usize| parts[i].name == "image";
            image(b)
                .cmp(&image(a))
                .then(extent(&parts[b]).total_cmp(&extent(&parts[a])))
        });
        for i in keys.into_iter().take(KEYS) {
            parts[i].key = true;
        }
        Self { parts }
    }
}

/// Glyph boxes merged into lines, and lines into paragraphs: one text part
/// per paragraph, at the order of its first glyph.
fn text_parts(glyphs: &[(usize, PageRect)]) -> Vec<(usize, SketchPart)> {
    let height = |r: &PageRect| (r[3] - r[1]).max(0.1);
    let mut lines: Vec<(usize, PageRect)> = Vec::new();
    for &(order, glyph) in glyphs {
        if let Some((_, line)) = lines.last_mut() {
            let overlap = line[3].min(glyph[3]) - line[1].max(glyph[1]);
            let h = height(&glyph);
            if overlap > 0.5 * height(&*line).min(h)
                && glyph[0] - line[2] < 1.5 * h
                && glyph[2] > line[0] - h
            {
                *line = union(*line, glyph);
                continue;
            }
        }
        lines.push((order, glyph));
    }
    let mut parts: Vec<(usize, SketchPart)> = Vec::new();
    let mut previous: Option<PageRect> = None;
    for (order, line) in lines {
        let h = height(&line);
        let joins = previous.is_some_and(|prev| {
            let gap = line[1] - prev[3];
            (line[0] - prev[0]).abs() < 2.0 * h
                && gap > -0.2 * h
                && gap < h
                && (h / height(&prev)).max(height(&prev) / h) < 1.5
        });
        let y = line[1] + h * 0.56;
        let segment = vec![[line[0], y], [line[2], y]];
        match parts.last_mut() {
            Some((_, part)) if joins => {
                part.rect = union(part.rect, line);
                part.lines.push(segment);
            }
            _ => parts.push((
                order,
                SketchPart {
                    kind: PartKind::Text,
                    name: "text",
                    rect: line,
                    lines: vec![segment],
                    key: false,
                },
            )),
        }
        previous = Some(line);
    }
    parts
}

fn path_bounds(path: &PagePath) -> Option<PageRect> {
    points_bounds(&path.points)
}

fn points_bounds(points: &[[f32; 2]]) -> Option<PageRect> {
    let mut rect = [
        f32::INFINITY,
        f32::INFINITY,
        f32::NEG_INFINITY,
        f32::NEG_INFINITY,
    ];
    for &[x, y] in points {
        rect = [
            rect[0].min(x),
            rect[1].min(y),
            rect[2].max(x),
            rect[3].max(y),
        ];
    }
    (rect[0].is_finite() && rect[1].is_finite()).then_some(rect)
}

fn intersect(a: PageRect, b: PageRect) -> Option<PageRect> {
    let rect = [
        a[0].max(b[0]),
        a[1].max(b[1]),
        a[2].min(b[2]),
        a[3].min(b[3]),
    ];
    (rect[2] >= rect[0] && rect[3] >= rect[1] && (rect[2] > rect[0] || rect[3] > rect[1]))
        .then_some(rect)
}

fn union(a: PageRect, b: PageRect) -> PageRect {
    [
        a[0].min(b[0]),
        a[1].min(b[1]),
        a[2].max(b[2]),
        a[3].max(b[3]),
    ]
}

/// The subpaths of `path` as polylines; curves keep their end points.
fn polylines(path: &PagePath) -> Vec<Vec<[f32; 2]>> {
    let mut lines: Vec<Vec<[f32; 2]>> = Vec::new();
    let mut points = path.points.iter().copied();
    let mut start = None;
    for verb in &path.verbs {
        match verb {
            PathVerb::MoveTo => {
                let Some(point) = points.next() else { break };
                lines.push(vec![point]);
                start = Some(point);
            }
            PathVerb::LineTo => {
                let Some(point) = points.next() else { break };
                if let Some(line) = lines.last_mut() {
                    line.push(point);
                }
            }
            PathVerb::CubicTo => {
                let (Some(_), Some(_), Some(point)) = (points.next(), points.next(), points.next())
                else {
                    break;
                };
                if let Some(line) = lines.last_mut() {
                    line.push(point);
                }
            }
            PathVerb::Close => {
                if let (Some(line), Some(start)) = (lines.last_mut(), start) {
                    line.push(start);
                }
            }
        }
    }
    lines.retain(|line| line.len() >= 2);
    lines
}

fn polyline_length(line: &[[f32; 2]]) -> f32 {
    line.windows(2)
        .map(|w| (w[1][0] - w[0][0]).hypot(w[1][1] - w[0][1]))
        .sum()
}

/// At most `max` points of `line`, evenly picked, keeping both ends.
fn decimate(line: Vec<[f32; 2]>, max: usize) -> Vec<[f32; 2]> {
    if line.len() <= max {
        return line;
    }
    let last = line.len() - 1;
    (0..max)
        .map(|i| line[(i * last + (max - 1) / 2) / (max - 1)])
        .collect()
}

// ─── timing ───

/// Seconds of one build (the original's `duration`).
const DURATION: f32 = 2.4;
/// Phases as fractions of [`DURATION`] (the original's `PH`).
const DOTS: [f32; 2] = [0.0, 0.3];
const PROBE: [f32; 2] = [0.02, 0.24];
const PARTS: [f32; 2] = [0.28, 0.74];
const REVEAL: [f32; 2] = [0.74, 1.055];
const CLEAN: [f32; 2] = [0.84, 1.04];
/// The build is over at this progress of its ending.
const END: f32 = 1.09;
/// A run waiting for its page holds its ending here, before any of the
/// fades that lead into the reveal (the first starts at 0.62).
const HOLD: f32 = 0.6;
/// The reveal starts no earlier than this, so the first parts are traced
/// (they start at 0.28).
const MIN_BUILD: f32 = 0.3;
/// The reticle's stay on each key part while a run holds.
const RETICLE_STAY: f32 = 0.25;
/// The plain fade when motion is reduced, in seconds.
const REDUCED_FADE: f32 = 0.2;

/// One page's animation.
struct Run {
    started: Instant,
    seed: f32,
    /// The page's sketch and the progress it arrived at.
    sketch: Option<(Arc<PageSketch>, f32)>,
    /// The progress the page became ready at.
    ready: Option<f32>,
    /// Whether anything is under the animation yet (the page or its
    /// thumbnail). Until then there is no paper to hide, and the
    /// background shows through.
    drawn: bool,
    seen: u64,
}

impl Run {
    fn progress(&self, now: Instant) -> f32 {
        now.saturating_duration_since(self.started).as_secs_f32() / DURATION
    }

    /// Where the reveal starts: once the end-of-build fades have played
    /// from the hold, or at [`MIN_BUILD`].
    fn reveal(&self) -> Option<f32> {
        self.ready
            .map(|ready| (ready + REVEAL[0] - HOLD).max(MIN_BUILD))
    }

    /// The original's progress for everything that leads into and follows
    /// the reveal: it stops at [`HOLD`] while the page is loading.
    fn ending(&self, p: f32) -> f32 {
        match self.reveal() {
            Some(reveal) => p - reveal + REVEAL[0],
            None => p.min(HOLD),
        }
    }

    fn finished(&self, p: f32, reduce_motion: bool) -> bool {
        match self.ready {
            Some(ready) if reduce_motion => (p - ready) * DURATION >= REDUCED_FADE + 0.02,
            Some(_) => self.ending(p) >= END,
            None => false,
        }
    }

    /// The parts phase: the original's, or as long from the sketch's
    /// arrival when it came late.
    fn parts_window(&self) -> Option<(f32, f32)> {
        self.sketch.as_ref().map(|(_, arrived)| {
            let start = PARTS[0].max(*arrived);
            (start, start + PARTS[1] - PARTS[0])
        })
    }
}

/// What the app knows of a visible page this frame.
#[derive(Clone, Copy, Debug)]
pub struct PageState {
    /// Not drawable on the GPU yet.
    pub loading: bool,
    pub thumbnail: bool,
    /// The page's size on screen, in points.
    pub size: Vec2,
}

/// A scripted screenshot's replay over pages already drawn: each visible
/// page builds as if loading, and is ready from `release` on (never when
/// `None`). Its clock advances [`REPLAY_STEP`] a frame, so the shot does
/// not depend on how fast frames come.
struct Replay {
    release: Option<f32>,
    started: HashSet<PageKey>,
    origin: Instant,
    steps: u32,
    paused: bool,
}

const REPLAY_STEP: Duration = Duration::from_millis(100);

/// The animations of the visible pages.
#[derive(Default)]
pub struct Generations {
    runs: HashMap<PageKey, Run>,
    /// Pages loading without an animation yet: since when, and the frame
    /// they were last seen.
    pending: HashMap<PageKey, (Instant, u64)>,
    frame: u64,
    count: u32,
    reduce_motion: bool,
    replay: Option<Replay>,
}

impl Generations {
    /// Starts a frame; with `reduce_motion`, pages fade in instead.
    pub fn begin_frame(&mut self, reduce_motion: bool) {
        self.frame += 1;
        self.reduce_motion = reduce_motion;
        if let Some(replay) = &mut self.replay
            && !replay.paused
        {
            replay.steps += 1;
        }
    }

    /// The time animations are at: `now`, or a replay's own clock.
    fn clock(&self, now: Instant) -> Instant {
        self.replay
            .as_ref()
            .map_or(now, |replay| replay.origin + REPLAY_STEP * replay.steps)
    }

    /// Drops the animations of pages not shown this frame.
    pub fn end_frame(&mut self) {
        let frame = self.frame;
        self.runs.retain(|_, run| run.seen == frame);
        self.pending.retain(|_, (_, seen)| *seen == frame);
    }

    /// Whether frames must keep coming: an animation runs or is about to.
    pub fn animating(&self) -> bool {
        !self.runs.is_empty() || !self.pending.is_empty()
    }

    /// Follows a visible page: starts its animation once it has been
    /// loading for [`GRACE`], marks it ready, and ends it after the reveal.
    pub fn update(
        &mut self,
        key: PageKey,
        state: PageState,
        sketch: Option<&Arc<PageSketch>>,
        now: Instant,
    ) {
        let now = self.clock(now);
        let frame = self.frame;
        let ready = !state.loading || state.thumbnail;
        if let Some(run) = self.runs.get_mut(&key) {
            run.seen = frame;
            run.drawn = ready;
            let p = run.progress(now);
            if run.sketch.is_none()
                && let Some(sketch) = sketch
            {
                run.sketch = Some((sketch.clone(), p));
            }
            let held = self
                .replay
                .as_ref()
                .is_some_and(|replay| replay.release.is_none_or(|at| p < at));
            if ready && !held && run.ready.is_none() {
                run.ready = Some(p);
            }
            if run.finished(p, self.reduce_motion) {
                self.runs.remove(&key);
            }
            return;
        }
        let large = state.size.x >= MIN_SIDE && state.size.y >= MIN_SIDE;
        let replayed = large
            && self
                .replay
                .as_mut()
                .is_some_and(|replay| replay.started.insert(key));
        if !replayed {
            if ready || !large {
                self.pending.remove(&key);
                return;
            }
            let (since, seen) = self.pending.entry(key).or_insert((now, frame));
            *seen = frame;
            if now.saturating_duration_since(*since) < GRACE {
                return;
            }
            self.pending.remove(&key);
        }
        self.count += 1;
        self.runs.insert(
            key,
            Run {
                started: now,
                seed: self.count as f32,
                sketch: sketch.map(|sketch| (sketch.clone(), 0.0)),
                ready: None,
                drawn: ready,
                seen: frame,
            },
        );
    }

    /// Plays the animation over the visible pages as if they were loading,
    /// for a screenshot; they are ready from progress `release` on.
    pub fn replay(&mut self, release: Option<f32>, now: Instant) {
        self.replay = Some(Replay {
            release,
            started: HashSet::new(),
            origin: now,
            steps: 0,
            paused: false,
        });
    }

    /// Stops a replay's clock, so the screenshot shows the frame asked for.
    pub fn pause_replay(&mut self) {
        if let Some(replay) = &mut self.replay {
            replay.paused = true;
        }
    }

    /// The progress and ending progress of the most advanced animation.
    pub fn progress(&self, now: Instant) -> Option<(f32, f32)> {
        let now = self.clock(now);
        self.runs
            .values()
            .map(|run| {
                let p = run.progress(now);
                (p, run.ending(p))
            })
            .max_by(|a, b| a.0.total_cmp(&b.0))
    }

    /// Seconds `key`'s page has been generating, while its animation runs;
    /// the count stops where the reveal starts.
    pub fn loading(&self, key: PageKey, now: Instant) -> Option<f32> {
        let run = self.runs.get(&key)?;
        let p = run.progress(self.clock(now));
        Some(run.reveal().map_or(p, |reveal| p.min(reveal)) * DURATION)
    }

    /// Draws `map`'s page animation, if it has one, over the page.
    /// `papers` are the visible pages of the view, which the margin around
    /// the page keeps clear of.
    pub fn paint(
        &self,
        painter: &egui::Painter,
        key: PageKey,
        map: &PageMap,
        papers: &[Rect],
        tokens: &Tokens,
        now: Instant,
    ) {
        let Some(run) = self.runs.get(&key) else {
            return;
        };
        let p = run.progress(self.clock(now));
        if self.reduce_motion {
            if !run.drawn {
                return;
            }
            let shown = run.ready.map_or(0.0, |ready| {
                ((p - ready) * DURATION / REDUCED_FADE).clamp(0.0, 1.0)
            });
            painter.rect_filled(
                map.paper,
                0.0,
                tokens.bg.to_opaque().gamma_multiply(1.0 - shown),
            );
            return;
        }
        Build::new(painter, run, map, papers, tokens, p).paint();
    }
}

// ─── easing ───

fn clamp01(x: f32) -> f32 {
    x.clamp(0.0, 1.0)
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

fn smooth(a: f32, b: f32, x: f32) -> f32 {
    let q = clamp01((x - a) / (b - a));
    q * q * (3.0 - 2.0 * q)
}

fn window(range: [f32; 2], p: f32) -> f32 {
    clamp01((p - range[0]) / (range[1] - range[0]))
}

fn hash(x: f32) -> f32 {
    let v = (f64::from(x) * 127.1 + 311.7).sin() * 43758.5453;
    (v - v.floor()) as f32
}

/// A CSS cubic-bezier timing function at `x`.
fn bezier(x1: f32, y1: f32, x2: f32, y2: f32, x: f32) -> f32 {
    if x <= 0.0 {
        return 0.0;
    }
    if x >= 1.0 {
        return 1.0;
    }
    let curve = |a: f32, b: f32, t: f32| {
        3.0 * a * t * (1.0 - t).powi(2) + 3.0 * b * t * t * (1.0 - t) + t.powi(3)
    };
    let (mut lo, mut hi, mut m) = (0.0, 1.0, 0.5);
    for _ in 0..28 {
        m = (lo + hi) / 2.0;
        if curve(x1, x2, m) < x {
            lo = m;
        } else {
            hi = m;
        }
    }
    curve(y1, y2, m)
}

/// The drawing's ease: soft and quick to settle.
fn out(x: f32) -> f32 {
    bezier(0.16, 1.0, 0.3, 1.0, x)
}

/// The reveal's ease (`--ease-smooth-out`).
fn smooth_out(x: f32) -> f32 {
    bezier(0.22, 1.0, 0.36, 1.0, x)
}

// ─── drawing ───

/// Room around the page for the dot field and guides, in points.
const MARGIN: f32 = 44.0;
/// Dot field spacing; it grows on large pages to stay under [`MAX_DOTS`].
const SPACING: f32 = 15.0;
const MAX_DOTS: f32 = 1600.0;
const DOT_GAP: f32 = 4.8;
const GUIDE_GAP: f32 = 6.5;
const PATH_GAP: f32 = 3.6;
/// How fast dots flow along the lines, in points per millisecond.
const FLOW: f32 = 0.016;
/// Radii of the dots on traced lines, guides and paths (half their width).
const WIRE_DOT: f32 = 1.05;
const GUIDE_DOT: f32 = 0.8;
const PATH_DOT: f32 = 0.9;
const HEAD: f32 = 2.4;
const KEY_EXTEND: f32 = 16.0;
/// Parts smaller than this on screen (longer side, in points) are left out.
const MIN_PART: f32 = 8.0;

/// The palette of the glyph design: the page's structure traced in the
/// accent over the dot field (frame 02b).
#[derive(Clone, Copy)]
struct Palette {
    line: Color32,
    strong: Color32,
    ink: Color32,
}

impl Palette {
    fn of(tokens: &Tokens) -> Self {
        Self {
            line: tokens.accent,
            strong: tokens.accent_text,
            ink: tokens.dot,
        }
    }
}

/// Dots as one mesh of small octagons; those outside `cull` are left out.
struct Dots {
    mesh: egui::Mesh,
    cull: Rect,
}

impl Dots {
    fn new(cull: Rect) -> Self {
        Self {
            mesh: egui::Mesh::default(),
            cull,
        }
    }

    fn dot(&mut self, center: Pos2, radius: f32, color: Color32) {
        const SIDES: u32 = 8;
        if color.a() == 0 || radius <= 0.0 || !self.cull.expand(radius).contains(center) {
            return;
        }
        let mesh = &mut self.mesh;
        let first = mesh.vertices.len() as u32;
        mesh.colored_vertex(center, color);
        for i in 0..SIDES {
            let angle = i as f32 * TAU / SIDES as f32;
            mesh.colored_vertex(center + vec2(angle.cos(), angle.sin()) * radius, color);
        }
        for i in 0..SIDES {
            mesh.add_triangle(first, first + 1 + i, first + 1 + (i + 1) % SIDES);
        }
    }

    fn paint(self, painter: &egui::Painter) {
        if !self.mesh.is_empty() {
            painter.add(egui::Shape::mesh(self.mesh));
        }
    }
}

/// A line drawn as flowing dots.
struct Stroke {
    points: Vec<Pos2>,
    length: f32,
    gap: f32,
    radius: f32,
    head: f32,
}

impl Stroke {
    fn new(points: Vec<Pos2>, gap: f32, radius: f32, head: f32) -> Self {
        let length = points.windows(2).map(|w| w[0].distance(w[1])).sum();
        Self {
            points,
            length,
            gap,
            radius,
            head,
        }
    }

    fn point_at(&self, at: f32) -> Pos2 {
        let mut left = at.max(0.0);
        for w in self.points.windows(2) {
            let d = w[0].distance(w[1]);
            if left <= d && d > 0.0 {
                return w[0] + (w[1] - w[0]) * (left / d);
            }
            left -= d;
        }
        self.points.last().copied().unwrap_or_default()
    }

    /// Draws the first `q` of the line, its dots flowing along it at time
    /// `t` (ms). Returns the moving head while it is being drawn.
    fn draw(&self, dots: &mut Dots, q: f32, t: f32, color: Color32, head: Color32) -> Option<Pos2> {
        if q <= 0.0 || self.points.len() < 2 {
            return None;
        }
        let drawn = self.length * q.min(1.0);
        let offset = (t * FLOW) % self.gap;
        let end = (drawn + offset).min(self.length);
        let mut at = offset;
        let mut segments = self.points.windows(2);
        let mut segment = segments.next();
        // Length of the line before `segment`.
        let mut start = 0.0;
        while at <= end {
            // Walk to the segment holding `at`.
            while let Some(w) = segment
                && at > start + w[0].distance(w[1])
            {
                start += w[0].distance(w[1]);
                segment = segments.next();
            }
            let Some(w) = segment else { break };
            let d = w[0].distance(w[1]).max(f32::EPSILON);
            dots.dot(
                w[0] + (w[1] - w[0]) * ((at - start) / d),
                self.radius,
                color,
            );
            at += self.gap;
        }
        (q < 1.0).then(|| {
            let point = self.point_at((drawn / self.gap).floor() * self.gap);
            dots.dot(point, self.head, head);
            point
        })
    }
}

/// A part's lines, traced one after another (the halves of a key box
/// together).
struct PartStrokes<'a> {
    part: &'a SketchPart,
    rect: Rect,
    strokes: Vec<(Stroke, usize)>,
    count: usize,
    caps: bool,
    t0: f32,
    t1: f32,
}

/// Half of a rectangle's outline from its top centre to its bottom centre,
/// clockwise or counter-clockwise.
fn half_outline(rect: Rect, clockwise: bool) -> Vec<Pos2> {
    let (top, bottom) = (rect.center_top(), rect.center_bottom());
    if clockwise {
        vec![top, rect.right_top(), rect.right_bottom(), bottom]
    } else {
        vec![top, rect.left_top(), rect.left_bottom(), bottom]
    }
}

fn mix_rect(a: Rect, b: Rect, t: f32) -> Rect {
    Rect::from_min_max(a.min.lerp(b.min, t), a.max.lerp(b.max, t))
}

/// One frame of a run over a page.
struct Build<'a> {
    painter: &'a egui::Painter,
    run: &'a Run,
    map: &'a PageMap,
    paper: Rect,
    tokens: &'a Tokens,
    /// Inside the page and outside it.
    inside: Palette,
    outside: Palette,
    /// Free room on each side: left, top, right, bottom.
    margins: [f32; 4],
    p: f32,
    /// The ending progress (see [`Run::ending`]).
    e: f32,
    /// Milliseconds since the start.
    t: f32,
}

impl<'a> Build<'a> {
    fn new(
        painter: &'a egui::Painter,
        run: &'a Run,
        map: &'a PageMap,
        papers: &[Rect],
        tokens: &'a Tokens,
        p: f32,
    ) -> Self {
        let paper = map.paper;
        // Half the free room up to each neighbouring page.
        let mut margins = [MARGIN; 4];
        for other in papers.iter().filter(|other| **other != paper) {
            let beside = other.y_range().intersects(paper.y_range());
            let above_below = other.x_range().intersects(paper.x_range());
            if beside && other.right() <= paper.left() {
                margins[0] = margins[0].min((paper.left() - other.right()) / 2.0);
            }
            if above_below && other.bottom() <= paper.top() {
                margins[1] = margins[1].min((paper.top() - other.bottom()) / 2.0);
            }
            if beside && other.left() >= paper.right() {
                margins[2] = margins[2].min((other.left() - paper.right()) / 2.0);
            }
            if above_below && other.top() >= paper.bottom() {
                margins[3] = margins[3].min((other.top() - paper.bottom()) / 2.0);
            }
        }
        Self {
            painter,
            run,
            map,
            paper,
            tokens,
            inside: Palette::of(tokens),
            outside: Palette::of(tokens),
            margins,
            p,
            e: run.ending(p),
            t: p * DURATION * 1000.0,
        }
    }

    fn at(&self, x: f32, y: f32) -> Pos2 {
        self.paper.min + vec2(x, y)
    }

    fn paint(&self) {
        let (p, e, t) = (self.p, self.e, self.t);
        let (w, h) = (self.paper.width(), self.paper.height());
        let [ml, mt, mr, mb] = self.margins;
        let field = Rect::from_min_max(self.at(-ml, -mt), self.at(w + mr, h + mb));
        // Only the part on screen is drawn: a page zoomed in is larger
        // than the view.
        let visible = self.painter.clip_rect().intersect(field);
        if !visible.is_positive() {
            return;
        }
        let painter = self.painter.with_clip_rect(visible);
        let (line, strong) = (self.inside.line, self.inside.strong);

        // The page: covered until its reveal, wiped in behind the scan
        // line, fading in.
        let sq = smooth_out(window(REVEAL, e));
        let scan_y = sq * (h + 6.0) - 3.0;
        let fq = smooth_out(window([REVEAL[0], REVEAL[1] + 0.03], e));
        let revealing = e >= REVEAL[0];
        let cover_from = if e >= REVEAL[1] {
            h
        } else {
            scan_y.clamp(0.0, h)
        };
        // Until its reveal a page already drawn is covered with the base,
        // as a page being generated is in the design; one not drawn yet
        // has nothing under it to cover.
        if self.run.drawn {
            if revealing {
                painter.rect_filled(
                    Rect::from_min_max(self.paper.min, self.at(w, cover_from)),
                    0.0,
                    self.tokens.bg.to_opaque().gamma_multiply(1.0 - fq),
                );
            }
            painter.rect_filled(
                Rect::from_min_max(
                    self.at(0.0, if revealing { cover_from } else { 0.0 }),
                    self.paper.max,
                ),
                0.0,
                self.tokens.bg.to_opaque(),
            );
        }

        let parts = self.part_strokes();
        let clean = 1.0 - smooth(CLEAN[0], CLEAN[1], e);

        // The reticle: on the part being drawn, then visiting the key parts
        // while the page is loading.
        let reticle = self.reticle(&parts);
        let reticle_on = parts.first().map_or(0.0, |first| {
            smooth(first.t0, first.t0 + 0.04, p) * (1.0 - smooth(REVEAL[0], REVEAL[0] + 0.06, e))
        });

        // Dot field: a ripple from the centre, rain down the columns, lit
        // by the probe and the reticle.
        let mut field_dots = Dots::new(visible);
        let spacing = SPACING.max((visible.area() / MAX_DOTS).sqrt());
        let shown = visible.translate(-self.paper.min.to_vec2());
        let probe_w = window(PROBE, p);
        let probe_x = lerp(shown.left(), shown.right(), out(probe_w));
        let probe_on = if probe_w > 0.0 && probe_w < 1.0 {
            smooth(0.0, 0.1, probe_w) * (1.0 - smooth(0.85, 1.0, probe_w))
        } else {
            0.0
        };
        let rain_on = smooth(0.04, 0.2, p) * (1.0 - smooth(0.62, 0.86, e));
        let span = shown.height();
        let seed = self.run.seed;
        // The ripple spreads from the middle of what is shown.
        let center = shown.center();
        let max_d = shown.size().length() / 2.0;
        let ripple = out(window(DOTS, p)) * (max_d + 60.0);
        let lit_box = reticle.map(|(rect, _)| rect.expand(5.0));
        let mut x = (shown.left() / spacing).ceil() * spacing;
        while x <= shown.right() {
            let column = (x / spacing).round();
            let speed = 0.07 + 0.09 * hash(column * 7.13 + seed * 31.7);
            let phase = hash(column * 3.71 + seed * 11.3) * 900.0;
            let trail = 50.0 + 70.0 * hash(column * 1.91 + seed);
            let head_y = (t * speed + phase) % (span + trail) + shown.top();
            let mut y = (shown.top() / spacing).ceil() * spacing;
            while y <= shown.bottom() {
                let d = pos2(x, y).distance(center);
                let outside = 0f32.max(-x).max(x - w).max(-y).max(y - h);
                let edge = 1.0 - clamp01(outside / MARGIN);
                let inside = outside == 0.0;
                let lag = ripple - d;
                let envelope = clamp01(lag / 50.0);
                let front = if p < DOTS[1] + 0.05 {
                    (-((lag - 6.0) / 14.0).powi(2)).exp()
                } else {
                    0.0
                };
                let back = head_y - y;
                let rain = if (0.0..trail).contains(&back) {
                    (1.0 - back / trail).powf(1.6) * rain_on * envelope * edge
                } else {
                    0.0
                };
                let pos = self.at(x, y);
                let near = lit_box.map_or(0.0, |rect| {
                    let distance = rect.distance_to_pos(pos);
                    (1.0 - distance / 16.0).max(0.0) * if rect.contains(pos) { 1.0 } else { 0.6 }
                });
                let probe = if probe_on > 0.0 {
                    (-((x - probe_x) / 9.0).powi(2)).exp() * probe_on * envelope
                } else {
                    0.0
                };
                let lit = front.max(rain).max(near * reticle_on * 0.8).max(probe);
                // Gone where the page is in.
                let under = if inside && revealing && y < scan_y {
                    0.0
                } else {
                    1.0
                };
                let opacity = (envelope * 0.5 * edge).max(lit * edge) * clean * under;
                if opacity > 0.01 {
                    let palette = if inside { self.inside } else { self.outside };
                    let color = if lit > 0.35 {
                        palette.strong
                    } else {
                        palette.ink
                    };
                    field_dots.dot(pos, 1.05 + lit * 0.85, color.gamma_multiply(opacity));
                }
                y += spacing;
            }
            x += spacing;
        }
        field_dots.paint(&painter);

        // The probe, edge guides and size label.
        let mut guides = Dots::new(visible);
        if probe_on > 0.0 {
            let probe = Stroke::new(
                vec![self.at(probe_x, -mt), self.at(probe_x, h + mb)],
                5.0,
                GUIDE_DOT,
                0.0,
            );
            probe.draw(
                &mut guides,
                1.0,
                0.0,
                line.gamma_multiply(probe_on * 0.8),
                Color32::TRANSPARENT,
            );
        }
        guides.paint(&painter);

        // The wireframe, wiped away behind the scan line.
        let wire_painter = if revealing {
            painter.with_clip_rect(painter.clip_rect().intersect(Rect::from_min_max(
                pos2(field.left() - 10.0, self.paper.top() + scan_y),
                field.max,
            )))
        } else {
            painter.clone()
        };
        let wire_on = 1.0 - 0.6 * fq;
        let (wire_line, wire_strong) =
            (line.gamma_multiply(wire_on), strong.gamma_multiply(wire_on));
        let mut wire = Dots::new(visible);
        let mut live: Option<Pos2> = None;
        for part in &parts {
            let q = clamp01((p - part.t0) / (part.t1 - part.t0));
            for (stroke, index) in &part.strokes {
                let sq = out(clamp01(q * part.count as f32 - *index as f32 * 0.75));
                live = stroke
                    .draw(&mut wire, sq, t, wire_line, wire_strong)
                    .or(live);
                if part.caps && sq > 0.0 {
                    let (first, last) = (stroke.points[0], stroke.points[stroke.points.len() - 1]);
                    wire.dot(first, 1.7, wire_strong);
                    if sq >= 1.0 {
                        wire.dot(last, 1.7, wire_strong);
                    }
                }
            }
        }
        wire.paint(&wire_painter);

        // Key parts: their own guides, corner handles and size label.
        let key_on = 1.0 - smooth(REVEAL[0] - 0.02, REVEAL[0] + 0.1, e);
        let mut key_guides = Dots::new(visible);
        for part in parts.iter().filter(|part| part.part.key) {
            let r = part.rect;
            let t0 = part.t0 - 0.03;
            let lines = [
                [
                    pos2(r.left() - KEY_EXTEND, r.top()),
                    pos2(r.right() + KEY_EXTEND, r.top()),
                ],
                [
                    pos2(r.right(), r.top() - KEY_EXTEND),
                    pos2(r.right(), r.bottom() + KEY_EXTEND),
                ],
                [
                    pos2(r.right() + KEY_EXTEND, r.bottom()),
                    pos2(r.left() - KEY_EXTEND, r.bottom()),
                ],
                [
                    pos2(r.left(), r.bottom() + KEY_EXTEND),
                    pos2(r.left(), r.top() - KEY_EXTEND),
                ],
            ];
            for (i, [a, b]) in lines.into_iter().enumerate() {
                let q = out(clamp01((p - t0 - i as f32 * 0.012) / 0.1));
                Stroke::new(vec![a, b], GUIDE_GAP, GUIDE_DOT, 0.0).draw(
                    &mut key_guides,
                    q,
                    t,
                    line.gamma_multiply(0.5 * key_on),
                    Color32::TRANSPARENT,
                );
            }
            let handle = out(clamp01((p - part.t0 - 0.04) / 0.08)) * key_on;
            for corner in [
                r.left_top(),
                r.right_top(),
                r.right_bottom(),
                r.left_bottom(),
            ] {
                self.handle(&painter, corner, 4.0 * handle);
            }
            if p > t0 && key_on > 0.0 {
                let traced = out(clamp01((p - part.t0) / (part.t1 - part.t0)));
                let [x0, y0, x1, y1] = part.part.rect;
                // Under the box: the reticle's tag is above it.
                painter.text(
                    r.left_bottom() + vec2(0.0, 5.0),
                    egui::Align2::LEFT_TOP,
                    format!(
                        "{} × {}",
                        ((x1 - x0) * traced).round(),
                        ((y1 - y0) * traced).round()
                    ),
                    egui::FontId::monospace(7.5),
                    strong.gamma_multiply(0.9 * key_on),
                );
            }
        }
        key_guides.paint(&painter);

        // The reticle and its tag.
        if let Some((rect, (name, size))) = reticle
            && reticle_on > 0.0
        {
            let r = rect.expand(5.0);
            let l = 7f32.min(r.width() / 3.0).min(r.height() / 3.0);
            let stroke = egui::Stroke::new(1.2, strong.gamma_multiply(reticle_on));
            for (corner, dx, dy) in [
                (r.left_top(), 1.0, 1.0),
                (r.right_top(), -1.0, 1.0),
                (r.right_bottom(), -1.0, -1.0),
                (r.left_bottom(), 1.0, -1.0),
            ] {
                painter.line(
                    vec![
                        corner + vec2(0.0, dy * l),
                        corner,
                        corner + vec2(dx * l, 0.0),
                    ],
                    stroke,
                );
            }
            painter.text(
                r.left_top() - vec2(0.0, 4.0),
                egui::Align2::LEFT_BOTTOM,
                format!(
                    "{} {}×{}",
                    name.to_uppercase(),
                    size[0].round(),
                    size[1].round()
                ),
                egui::FontId::monospace(7.5),
                strong.gamma_multiply(reticle_on),
            );
        }

        // The coordinate readout on the line being drawn.
        if let Some(head) = live {
            let [x, y] = self.map.page_point(head);
            painter.text(
                head + vec2(6.0, -5.0),
                egui::Align2::LEFT_BOTTOM,
                format!("{:03}·{:03}", x.round() as i32, y.round() as i32),
                egui::FontId::monospace(7.5),
                strong,
            );
        }

        // The scan line and its glow.
        let scan_on = if revealing && e < REVEAL[1] {
            smooth(0.0, 0.12, sq) * (1.0 - smooth(0.85, 1.0, sq))
        } else {
            0.0
        };
        if scan_on > 0.0 {
            let glow = line.gamma_multiply(0.12 * scan_on);
            let mut mesh = egui::Mesh::default();
            let top = Rect::from_min_max(self.at(0.0, scan_y - 28.0), self.at(w, scan_y));
            mesh.colored_vertex(top.left_top(), Color32::TRANSPARENT);
            mesh.colored_vertex(top.right_top(), Color32::TRANSPARENT);
            mesh.colored_vertex(top.right_bottom(), glow);
            mesh.colored_vertex(top.left_bottom(), glow);
            mesh.add_triangle(0, 1, 2);
            mesh.add_triangle(0, 2, 3);
            painter.add(egui::Shape::mesh(mesh));
            let mut scan = Dots::new(visible);
            Stroke::new(
                vec![self.at(-18.0, scan_y), self.at(w + 18.0, scan_y)],
                5.0,
                GUIDE_DOT,
                0.0,
            )
            .draw(
                &mut scan,
                1.0,
                t * 2.0,
                line.gamma_multiply(scan_on),
                Color32::TRANSPARENT,
            );
            scan.dot(self.at(0.0, scan_y), 2.3, strong.gamma_multiply(scan_on));
            scan.dot(self.at(w, scan_y), 2.3, strong.gamma_multiply(scan_on));
            scan.paint(&painter);
        }
    }

    /// A corner handle: a small accent square.
    fn handle(&self, painter: &egui::Painter, center: Pos2, size: f32) {
        if size <= 0.1 {
            return;
        }
        let rect = Rect::from_center_size(center, Vec2::splat(size));
        painter.rect(
            rect,
            0.0,
            self.tokens.accent,
            egui::Stroke::new(1.0, self.inside.strong),
            egui::StrokeKind::Middle,
        );
    }

    /// The sketch's parts on screen with their lines and timing.
    fn part_strokes(&self) -> Vec<PartStrokes<'a>> {
        let (Some((sketch, _)), Some((start, end))) = (&self.run.sketch, self.run.parts_window())
        else {
            return Vec::new();
        };
        // Parts a few points across on screen would only be blots.
        let shown: Vec<(&SketchPart, Rect)> = sketch
            .parts
            .iter()
            .map(|part| (part, self.map.rect_to_screen(part.rect)))
            .filter(|(_, rect)| rect.width().max(rect.height()) >= MIN_PART)
            .collect();
        let count = shown.len();
        let span = end - start;
        let duration = 0.2f32.min(span * 0.6);
        shown
            .into_iter()
            .enumerate()
            .map(|(i, (part, rect))| {
                let screen = |line: &Vec<[f32; 2]>| -> Vec<Pos2> {
                    line.iter()
                        .map(|&point| self.map.screen_pos(point))
                        .collect()
                };
                let mut strokes = Vec::new();
                match part.kind {
                    PartKind::Box if part.key => {
                        for clockwise in [true, false] {
                            strokes.push((
                                Stroke::new(half_outline(rect, clockwise), DOT_GAP, WIRE_DOT, HEAD),
                                0,
                            ));
                        }
                    }
                    PartKind::Box => {
                        let outline = vec![
                            rect.left_top(),
                            rect.right_top(),
                            rect.right_bottom(),
                            rect.left_bottom(),
                            rect.left_top(),
                        ];
                        strokes.push((Stroke::new(outline, DOT_GAP, WIRE_DOT, HEAD), 0));
                    }
                    PartKind::Text => {
                        for (index, line) in part.lines.iter().enumerate() {
                            strokes
                                .push((Stroke::new(screen(line), DOT_GAP, WIRE_DOT, HEAD), index));
                        }
                    }
                    PartKind::Path => {
                        for (index, line) in part.lines.iter().enumerate() {
                            strokes
                                .push((Stroke::new(screen(line), PATH_GAP, PATH_DOT, HEAD), index));
                        }
                    }
                }
                let count_strokes = strokes
                    .iter()
                    .map(|(_, index)| index + 1)
                    .max()
                    .unwrap_or(1);
                let t0 = start
                    + if count > 1 {
                        (span - duration) * (i as f32 / (count - 1) as f32)
                    } else {
                        0.0
                    };
                PartStrokes {
                    part,
                    rect,
                    strokes,
                    count: count_strokes,
                    caps: part.kind == PartKind::Text,
                    t0,
                    t1: t0 + duration,
                }
            })
            .collect()
    }

    /// Where the reticle is: moving from the part before to the part being
    /// drawn, then from key part to key part while the run holds. With the
    /// part's name and size in page points.
    fn reticle(&self, parts: &[PartStrokes]) -> Option<(Rect, (&'static str, [f32; 2]))> {
        let p = self.p;
        let current = parts.iter().rposition(|part| p >= part.t0)?;
        let size = |part: &PartStrokes| {
            let [x0, y0, x1, y1] = part.part.rect;
            (part.part.name, [x1 - x0, y1 - y0])
        };
        let (_, end) = self.run.parts_window()?;
        let holding = p - end;
        let cycle: Vec<&PartStrokes> = {
            let keys: Vec<&PartStrokes> = parts.iter().filter(|part| part.part.key).collect();
            if keys.is_empty() {
                parts.iter().collect()
            } else {
                keys
            }
        };
        if holding > 0.0 && cycle.len() > 1 {
            let step = (holding / RETICLE_STAY).floor() as usize;
            let to = cycle[step % cycle.len()];
            let from = match step {
                0 => &parts[current],
                _ => cycle[(step - 1) % cycle.len()],
            };
            let q = out(clamp01((holding % RETICLE_STAY) / 0.05));
            return Some((mix_rect(from.rect, to.rect, q), size(to)));
        }
        let part = &parts[current];
        let from = if current > 0 {
            parts[current - 1].rect
        } else {
            self.paper
        };
        let q = out(clamp01((p - part.t0) / 0.05));
        Some((mix_rect(from, part.rect, q), size(part)))
    }
}

#[cfg(test)]
mod tests {
    use glyphst_pdf::vector::{StrokeStyle, VectorDraw};

    use super::*;

    fn rect_path([x0, y0, x1, y1]: PageRect) -> PagePath {
        PagePath {
            verbs: vec![
                PathVerb::MoveTo,
                PathVerb::LineTo,
                PathVerb::LineTo,
                PathVerb::LineTo,
                PathVerb::Close,
            ],
            points: vec![[x0, y0], [x1, y0], [x1, y1], [x0, y1]],
        }
    }

    fn fill(rect: PageRect) -> VectorDraw {
        VectorDraw {
            item: VectorItem::Fill {
                path: rect_path(rect),
                even_odd: false,
                color: [0, 0, 0, 255],
            },
            clip: None,
        }
    }

    fn page(draws: Vec<VectorDraw>) -> VectorPage {
        VectorPage {
            bounds: [0.0, 0.0, 600.0, 800.0],
            draws,
            stats: Default::default(),
        }
    }

    /// A word of `count` 10 pt glyphs at `x`, `y`.
    fn word(x: f32, y: f32, count: usize) -> Vec<VectorDraw> {
        (0..count)
            .map(|i| {
                let x = x + i as f32 * 6.0;
                fill([x, y, x + 5.0, y + 10.0])
            })
            .collect()
    }

    #[test]
    fn glyphs_merge_into_lines_and_paragraphs() {
        let mut draws = word(50.0, 100.0, 10);
        draws.extend(word(50.0, 112.0, 8));
        // A heading far below starts a new part.
        draws.extend(word(50.0, 400.0, 4));
        let sketch = PageSketch::from_vector(&page(draws));
        let texts: Vec<&SketchPart> = sketch
            .parts
            .iter()
            .filter(|part| part.kind == PartKind::Text)
            .collect();
        assert_eq!(texts.len(), 2);
        assert_eq!(texts[0].lines.len(), 2, "two lines in one paragraph");
        assert_eq!(texts[0].rect, [50.0, 100.0, 109.0, 122.0]);
        let [[x0, y0], [x1, y1]] = texts[0].lines[0][..] else {
            panic!("a line is a segment");
        };
        assert_eq!([x0, x1], [50.0, 109.0]);
        assert!(
            (y0 - 105.6).abs() < 1e-3 && y0 == y1,
            "a little below the middle"
        );
        assert_eq!(texts[1].lines.len(), 1);
    }

    #[test]
    fn backgrounds_are_skipped_and_images_found_by_transform() {
        let image = VectorDraw {
            item: VectorItem::Image {
                rgba: glyphst_pdf::vector::ImagePixels(Arc::from(vec![0u8; 4])),
                size: [1, 1],
                transform: [200.0, 0.0, 0.0, 100.0, 50.0, 300.0],
                interpolate: true,
            },
            clip: None,
        };
        let draws = vec![
            fill([0.0, 0.0, 600.0, 800.0]),
            image,
            fill([60.0, 500.0, 300.0, 560.0]),
        ];
        let sketch = PageSketch::from_vector(&page(draws));
        let rects: Vec<(&str, PageRect, bool)> = sketch
            .parts
            .iter()
            .map(|part| (part.name, part.rect, part.key))
            .collect();
        assert_eq!(
            rects,
            [
                ("image", [50.0, 300.0, 250.0, 400.0], true),
                ("frame", [60.0, 500.0, 300.0, 560.0], true),
            ]
        );
    }

    #[test]
    fn hairline_fills_are_traced_as_lines() {
        let sketch = PageSketch::from_vector(&page(vec![fill([10.0, 100.0, 500.0, 101.0])]));
        assert_eq!(sketch.parts.len(), 1);
        assert_eq!(sketch.parts[0].kind, PartKind::Path);
        assert_eq!(sketch.parts[0].lines, [vec![[10.0, 100.5], [500.0, 100.5]]]);
    }

    #[test]
    fn many_parts_keep_the_largest_in_drawing_order() {
        let mut draws: Vec<VectorDraw> = (0..80)
            .map(|i| {
                let x = 10.0 + (i % 10) as f32 * 58.0;
                let y = 10.0 + (i / 10) as f32 * 95.0;
                // Later boxes are larger.
                let size = 30.0 + i as f32 * 0.3;
                fill([x, y, x + size, y + size])
            })
            .collect();
        draws.push(VectorDraw {
            item: VectorItem::Stroke {
                path: rect_path([5.0, 5.0, 595.0, 795.0]),
                style: StrokeStyle {
                    width: 1.0,
                    start_cap: glyphst_pdf::vector::LineCap::Butt,
                    end_cap: glyphst_pdf::vector::LineCap::Butt,
                    dash_cap: glyphst_pdf::vector::LineCap::Butt,
                    dashes: Vec::new(),
                    dash_phase: 0.0,
                },
                color: [0, 0, 0, 255],
            },
            clip: None,
        });
        let sketch = PageSketch::from_vector(&page(draws));
        assert_eq!(sketch.parts.len(), MAX_PARTS);
        // The border stroke is the largest part, traced last.
        let last = sketch.parts.last().unwrap();
        assert_eq!(last.kind, PartKind::Path);
        assert_eq!(last.lines[0].len(), 5, "closed back to its start");
        // The 21 smallest (earliest) boxes are dropped.
        assert_eq!(sketch.parts[0].rect[..2], [10.0 + 58.0, 10.0 + 2.0 * 95.0]);
        assert_eq!(sketch.parts.iter().filter(|part| part.key).count(), KEYS);
    }

    #[test]
    fn long_paths_are_decimated_keeping_their_ends() {
        let line: Vec<[f32; 2]> = (0..1000).map(|i| [i as f32, 0.0]).collect();
        let kept = decimate(line, PATH_POINTS);
        assert_eq!(kept.len(), PATH_POINTS);
        assert_eq!(kept[0], [0.0, 0.0]);
        assert_eq!(kept[PATH_POINTS - 1], [999.0, 0.0]);
    }

    fn key() -> PageKey {
        PageKey { tab: 1, page: 0 }
    }

    fn state(loading: bool) -> PageState {
        PageState {
            loading,
            thumbnail: false,
            size: vec2(600.0, 800.0),
        }
    }

    fn at(start: Instant, seconds: f32) -> Instant {
        start + Duration::from_secs_f32(seconds)
    }

    /// Runs one frame of `generations` over the page.
    fn frame(generations: &mut Generations, state: PageState, now: Instant) {
        generations.begin_frame(false);
        generations.update(key(), state, None, now);
        generations.end_frame();
    }

    #[test]
    fn a_run_starts_only_after_the_grace_period() {
        let start = Instant::now();
        let mut generations = Generations::default();
        frame(&mut generations, state(true), start);
        assert!(generations.runs.is_empty() && generations.animating());
        frame(&mut generations, state(false), at(start, 0.1));
        assert!(!generations.animating(), "drawn in time: no animation");

        frame(&mut generations, state(true), at(start, 1.0));
        frame(&mut generations, state(true), at(start, 1.2));
        assert_eq!(generations.runs.len(), 1);
    }

    #[test]
    fn thumbnails_and_small_pages_get_no_run() {
        let start = Instant::now();
        let mut generations = Generations::default();
        let thumbnail = PageState {
            thumbnail: true,
            ..state(true)
        };
        let small = PageState {
            size: vec2(100.0, 140.0),
            ..state(true)
        };
        for seconds in [0.0, 0.5, 1.0] {
            frame(&mut generations, thumbnail, at(start, seconds));
        }
        for seconds in [1.5, 2.0, 2.5] {
            frame(&mut generations, small, at(start, seconds));
        }
        assert!(!generations.animating());
    }

    /// Starts a run at `start`, past the grace period.
    fn started(start: Instant) -> Generations {
        let mut generations = Generations::default();
        frame(&mut generations, state(true), start - GRACE);
        frame(&mut generations, state(true), start);
        assert_eq!(generations.runs.len(), 1);
        generations
    }

    fn run(generations: &Generations) -> &Run {
        generations.runs.get(&key()).unwrap()
    }

    #[test]
    fn an_early_page_waits_for_the_build_before_its_reveal() {
        let start = Instant::now();
        let mut generations = started(start);
        frame(&mut generations, state(false), at(start, 0.05 * DURATION));
        assert_eq!(run(&generations).reveal(), Some(MIN_BUILD));
        frame(&mut generations, state(false), at(start, 0.5 * DURATION));
        assert_eq!(generations.runs.len(), 1, "revealing");
        frame(
            &mut generations,
            state(false),
            at(start, (MIN_BUILD + 0.36) * DURATION),
        );
        assert!(!generations.animating());
    }

    #[test]
    fn a_page_is_covered_only_once_it_is_drawn() {
        let start = Instant::now();
        let mut generations = started(start);
        assert!(!run(&generations).drawn, "no paper to hide while loading");
        frame(&mut generations, state(false), at(start, 0.2 * DURATION));
        assert!(run(&generations).drawn);
    }

    #[test]
    fn a_late_page_holds_then_reveals() {
        let start = Instant::now();
        let mut generations = started(start);
        frame(&mut generations, state(true), at(start, 4.0 * DURATION));
        let held = run(&generations);
        assert_eq!(held.ending(4.0), HOLD, "holding before the fades");
        frame(&mut generations, state(false), at(start, 5.0 * DURATION));
        let reveal = run(&generations).reveal().unwrap();
        assert!((reveal - (5.0 + REVEAL[0] - HOLD)).abs() < 1e-4);
        assert!(
            (run(&generations).ending(5.0) - HOLD).abs() < 1e-4,
            "no jump"
        );
        frame(
            &mut generations,
            state(false),
            at(start, (reveal + END - REVEAL[0] - 0.01) * DURATION),
        );
        assert_eq!(generations.runs.len(), 1);
        frame(
            &mut generations,
            state(false),
            at(start, (reveal + END - REVEAL[0] + 0.01) * DURATION),
        );
        assert!(!generations.animating());
    }

    #[test]
    fn reduced_motion_ends_after_a_short_fade() {
        let start = Instant::now();
        let mut generations = started(start);
        let reduced = |generations: &mut Generations, loading, seconds| {
            generations.begin_frame(true);
            generations.update(key(), state(loading), None, at(start, seconds));
            generations.end_frame();
        };
        reduced(&mut generations, false, 1.0);
        reduced(&mut generations, false, 1.1);
        assert_eq!(generations.runs.len(), 1);
        reduced(&mut generations, false, 1.3);
        assert!(!generations.animating());
    }

    #[test]
    fn a_run_ends_when_its_page_leaves_the_view() {
        let start = Instant::now();
        let mut generations = started(start);
        generations.begin_frame(false);
        generations.end_frame();
        assert!(!generations.animating());
    }

    #[test]
    fn a_replay_holds_drawn_pages_until_released() {
        let start = Instant::now();
        let mut generations = Generations::default();
        generations.replay(Some(0.8), start);
        frame(&mut generations, state(false), start);
        assert_eq!(generations.runs.len(), 1);
        // Its clock steps a frame at a time, however long frames take.
        let steps = |p: f32| (p * DURATION / REPLAY_STEP.as_secs_f32()).round() as usize;
        for _ in 0..steps(0.7) {
            frame(&mut generations, state(false), at(start, 3600.0));
        }
        assert_eq!(run(&generations).ready, None);
        for _ in 0..steps(0.15) {
            frame(&mut generations, state(false), at(start, 3600.0));
        }
        assert!(run(&generations).ready.is_some());
        generations.pause_replay();
        let (progress, _) = generations.progress(start).unwrap();
        frame(&mut generations, state(false), start);
        assert_eq!(generations.progress(start).unwrap().0, progress, "paused");
    }

    #[test]
    fn eases_match_the_css_curves() {
        assert_eq!(out(0.0), 0.0);
        assert_eq!(out(1.0), 1.0);
        assert!(out(0.5) > 0.85, "a quick ease-out");
        assert!(smooth_out(0.3) > 0.5);
    }
}
