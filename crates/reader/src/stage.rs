//! The reading stage: the window below the top bar, where the background
//! effect runs and the pages, the rail, the drawers, the dock and the
//! progress line float. Everything here is plain geometry and timing, so
//! it is tested without a window.

use egui::{Rect, pos2, vec2};

/// Space between the stage's edge and floating cards, and between cards.
pub const GAP: f32 = 12.0;
pub const RAIL_WIDTH: f32 = 44.0;
/// One rail button and the space under it.
pub const RAIL_STEP: f32 = 36.0;
/// Below this stage width, drawers float over the pages instead of pushing
/// them aside.
pub const NARROW: f32 = 900.0;
pub const DRAWER_MIN: f32 = 220.0;
pub const ASSISTANT_MIN: f32 = 300.0;
/// The progress line's height, and its height while hovered.
pub const PROGRESS: f32 = 3.0;
pub const PROGRESS_HOVER: f32 = 8.0;
/// The dock's distance from the stage's bottom.
pub const DOCK_LIFT: f32 = 20.0;
/// Seconds without activity before the dock and the rail fade.
pub const IDLE_SECONDS: f64 = 2.5;

/// What is open on the stage.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StageInput {
    pub stage: Rect,
    /// The rail has this many buttons.
    pub rail_buttons: usize,
    /// The left drawer's width, when open.
    pub drawer: Option<f32>,
    /// The assistant's width, when open.
    pub assistant: Option<f32>,
    /// Present mode hides everything but the pages.
    pub present: bool,
}

/// Where everything goes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StageLayout {
    pub rail: Option<Rect>,
    pub drawer: Option<Rect>,
    pub assistant: Option<Rect>,
    /// Where pages are laid out.
    pub reading: Rect,
    /// Drawers float over the pages (a narrow window).
    pub overlay: bool,
    /// The progress line along the stage's bottom.
    pub progress: Option<Rect>,
    /// The dock's bottom centre.
    pub dock_anchor: Option<egui::Pos2>,
}

impl StageLayout {
    pub fn compute(input: StageInput) -> Self {
        let stage = input.stage;
        if input.present {
            return Self {
                rail: None,
                drawer: None,
                assistant: None,
                reading: stage,
                overlay: false,
                progress: None,
                dock_anchor: None,
            };
        }
        let overlay = stage.width() < NARROW;
        let top = stage.top() + GAP;
        let bottom = (stage.bottom() - GAP).max(top);
        let rail_height = (input.rail_buttons as f32 * RAIL_STEP + 8.0).min(bottom - top);
        let rail =
            Rect::from_min_size(pos2(stage.left() + GAP, top), vec2(RAIL_WIDTH, rail_height));
        // Neither drawer takes more than 45 % of the stage.
        let widest = (stage.width() * 0.45).max(DRAWER_MIN);
        let drawer = input.drawer.map(|width| {
            let width = width.clamp(DRAWER_MIN, widest.max(DRAWER_MIN));
            Rect::from_min_max(
                pos2(rail.right() + 8.0, top),
                pos2(rail.right() + 8.0 + width, bottom),
            )
        });
        let assistant = input.assistant.map(|width| {
            let width = width.clamp(ASSISTANT_MIN, widest.max(ASSISTANT_MIN));
            Rect::from_min_max(
                pos2(stage.right() - GAP - width, top),
                pos2(stage.right() - GAP, bottom),
            )
        });
        let left = match drawer {
            Some(drawer) if !overlay => drawer.right() + GAP / 2.0,
            _ => rail.right() + GAP / 2.0,
        };
        let right = match assistant {
            Some(assistant) if !overlay => assistant.left() - GAP / 2.0,
            _ => stage.right(),
        };
        let reading = Rect::from_min_max(
            pos2(left.min(stage.right()), stage.top()),
            pos2(right.max(left.min(stage.right())), stage.bottom()),
        );
        let progress = Rect::from_min_max(
            pos2(stage.left(), stage.bottom() - PROGRESS),
            stage.right_bottom(),
        );
        Self {
            rail: Some(rail),
            drawer,
            assistant,
            reading,
            overlay,
            progress: Some(progress),
            dock_anchor: Some(pos2(reading.center().x, stage.bottom() - DOCK_LIFT)),
        }
    }
}

/// Whether the dock and the rail show: they fade after a pause in
/// activity, unless something keeps them.
#[derive(Clone, Copy, Debug, Default)]
pub struct ChromeFade {
    last_activity: Option<f64>,
}

impl ChromeFade {
    /// Notes this frame's `activity` (pointer movement, or anything that
    /// keeps the controls: a focused field, an open menu or find bar, the
    /// pointer over the dock) and returns whether the controls show.
    pub fn visible(&mut self, now: f64, activity: bool) -> bool {
        if activity || self.last_activity.is_none() {
            self.last_activity = Some(now);
        }
        self.last_activity
            .is_some_and(|last| now - last < IDLE_SECONDS)
    }

    /// Seconds until the controls fade, for scheduling a repaint.
    pub fn remaining(&self, now: f64) -> Option<f64> {
        let left = IDLE_SECONDS - (now - self.last_activity?);
        (left > 0.0).then_some(left)
    }
}

/// Where along the progress line (0..=1) each top-level outline entry
/// starts, for `pages` pages.
pub fn chapter_ticks(
    entries: impl IntoIterator<Item = (usize, Option<usize>)>,
    pages: usize,
) -> Vec<f32> {
    if pages == 0 {
        return Vec::new();
    }
    let mut ticks: Vec<f32> = entries
        .into_iter()
        .filter(|&(depth, _)| depth == 0)
        .filter_map(|(_, page)| page)
        .filter(|&page| page > 0 && page < pages)
        .map(|page| page as f32 / pages as f32)
        .collect();
    ticks.dedup();
    ticks
}

/// How much of the document is read when `page` (from 0) is current.
pub fn read_fraction(page: usize, pages: usize) -> f32 {
    if pages == 0 {
        0.0
    } else {
        ((page + 1) as f32 / pages as f32).clamp(0.0, 1.0)
    }
}

/// The page under `x` on a progress line spanning `left..right`.
pub fn page_at(x: f32, left: f32, right: f32, pages: usize) -> usize {
    if pages == 0 || right <= left {
        return 0;
    }
    let fraction = ((x - left) / (right - left)).clamp(0.0, 1.0);
    ((fraction * pages as f32) as usize).min(pages - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(width: f32) -> StageInput {
        StageInput {
            stage: Rect::from_min_size(pos2(0.0, 40.0), vec2(width, 800.0)),
            rail_buttons: 6,
            drawer: None,
            assistant: None,
            present: false,
        }
    }

    #[test]
    fn open_drawers_push_the_pages_aside() {
        let closed = StageLayout::compute(input(1400.0));
        assert!(closed.reading.left() > closed.rail.unwrap().right());
        assert_eq!(closed.reading.right(), 1400.0);

        let open = StageLayout::compute(StageInput {
            drawer: Some(300.0),
            assistant: Some(380.0),
            ..input(1400.0)
        });
        let (drawer, assistant) = (open.drawer.unwrap(), open.assistant.unwrap());
        assert!(!open.overlay);
        assert!(open.reading.left() > drawer.right());
        assert!(open.reading.right() < assistant.left());
        assert_eq!(drawer.width(), 300.0);
        assert_eq!(assistant.width(), 380.0);
        // The dock stays centred on the pages.
        assert_eq!(open.dock_anchor.unwrap().x, open.reading.center().x);
    }

    #[test]
    fn a_narrow_window_floats_drawers_over_the_pages() {
        let layout = StageLayout::compute(StageInput {
            drawer: Some(300.0),
            ..input(600.0)
        });
        assert!(layout.overlay);
        assert!(layout.reading.left() < layout.drawer.unwrap().right());
        assert!(layout.drawer.unwrap().width() >= DRAWER_MIN);
    }

    #[test]
    fn drawers_are_kept_to_a_share_of_the_stage() {
        let layout = StageLayout::compute(StageInput {
            drawer: Some(5000.0),
            assistant: Some(10.0),
            ..input(1400.0)
        });
        assert!(layout.drawer.unwrap().width() <= 1400.0 * 0.45 + 0.5);
        assert_eq!(layout.assistant.unwrap().width(), ASSISTANT_MIN);
    }

    #[test]
    fn present_mode_gives_the_whole_stage_to_the_pages() {
        let layout = StageLayout::compute(StageInput {
            drawer: Some(300.0),
            present: true,
            ..input(1400.0)
        });
        assert_eq!(layout.reading, input(1400.0).stage);
        assert_eq!(
            (
                layout.rail,
                layout.drawer,
                layout.progress,
                layout.dock_anchor
            ),
            (None, None, None, None)
        );
    }

    #[test]
    fn controls_fade_after_a_pause_and_return_on_activity() {
        let mut fade = ChromeFade::default();
        assert!(fade.visible(10.0, false), "shown at first");
        assert!(fade.visible(10.0 + IDLE_SECONDS - 0.1, false));
        assert!(fade.remaining(10.0 + IDLE_SECONDS - 0.1).is_some());
        assert!(!fade.visible(10.0 + IDLE_SECONDS + 0.1, false));
        assert_eq!(fade.remaining(10.0 + IDLE_SECONDS + 0.1), None);
        assert!(fade.visible(20.0, true), "activity brings them back");
    }

    #[test]
    fn chapters_and_scrubbing_map_to_pages() {
        let ticks = chapter_ticks(
            [
                (0, Some(0)),
                (0, Some(10)),
                (1, Some(15)),
                (0, None),
                (0, Some(50)),
            ],
            100,
        );
        assert_eq!(ticks, vec![0.1, 0.5]);
        assert_eq!(read_fraction(49, 100), 0.5);
        assert_eq!(page_at(0.0, 0.0, 200.0, 100), 0);
        assert_eq!(page_at(100.0, 0.0, 200.0, 100), 50);
        assert_eq!(page_at(500.0, 0.0, 200.0, 100), 99);
        assert_eq!(page_at(10.0, 0.0, 200.0, 0), 0);
    }
}
