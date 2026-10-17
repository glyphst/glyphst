//! The docked shell of a document (frame 02a): under the 36pt tab bar and
//! above the 24pt status bar, a 40pt icon rail, the sidebar, the reading
//! column (a 28pt breadcrumb over the pages) and the assistant, side by
//! side and split by hairlines. Below [`NARROW`] the sidebar and the
//! assistant float over the pages instead. Plain geometry, tested without
//! a window.

use egui::{Rect, pos2, vec2};

pub use glyphst_ui::pane::BREADCRUMB_HEIGHT;
pub use glyphst_ui::rail::WIDTH as RAIL_WIDTH;

/// Below this width the sidebar and the assistant float over the pages
/// instead of pushing them aside.
pub const NARROW: f32 = 900.0;
pub const SIDEBAR_MIN: f32 = 220.0;
pub const ASSISTANT_MIN: f32 = 300.0;
/// The design's widths.
pub const SIDEBAR_WIDTH: f32 = 300.0;
pub const ASSISTANT_WIDTH: f32 = 380.0;

/// What is open.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShellInput {
    /// The window between the tab bar and the status bar.
    pub area: Rect,
    /// The sidebar's width, when open.
    pub sidebar: Option<f32>,
    /// How much of the sidebar shows (0..=1) while it slides in or out.
    pub sidebar_shown: f32,
    /// The assistant's width, when open.
    pub assistant: Option<f32>,
    /// How much of the assistant shows while it slides.
    pub assistant_shown: f32,
    /// Present and zen modes give everything to the pages.
    pub present: bool,
}

/// Where everything goes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShellLayout {
    pub rail: Option<Rect>,
    /// The panes at their whole width; while sliding they reach past
    /// `inner`, which clips them.
    pub sidebar: Option<Rect>,
    pub assistant: Option<Rect>,
    /// Right of the rail: what the panes show in.
    pub inner: Rect,
    /// The breadcrumb over the reading column.
    pub breadcrumb: Option<Rect>,
    /// Where pages are laid out and drawn.
    pub reading: Rect,
    /// The sidebar and the assistant float over the pages (a narrow
    /// window).
    pub overlay: bool,
}

impl ShellLayout {
    pub fn compute(input: ShellInput) -> Self {
        let area = input.area;
        if input.present {
            return Self {
                rail: None,
                sidebar: None,
                assistant: None,
                inner: area,
                breadcrumb: None,
                reading: area,
                overlay: false,
            };
        }
        let overlay = area.width() < NARROW;
        let rail = Rect::from_min_size(area.min, vec2(RAIL_WIDTH.min(area.width()), area.height()));
        // Neither pane takes more than 45 % of the window.
        let widest = area.width() * 0.45;
        let inner = Rect::from_min_max(pos2(rail.right(), area.top()), area.max);
        let shown = |shown: f32| shown.clamp(0.0, 1.0);
        let sidebar = input
            .sidebar
            .filter(|_| input.sidebar_shown > 0.0)
            .map(|width| {
                let width = width
                    .clamp(SIDEBAR_MIN, widest.max(SIDEBAR_MIN))
                    .min(inner.width());
                let hidden = width * (1.0 - shown(input.sidebar_shown));
                Rect::from_min_size(
                    pos2(rail.right() - hidden, area.top()),
                    vec2(width, area.height()),
                )
            });
        let assistant = input
            .assistant
            .filter(|_| input.assistant_shown > 0.0)
            .map(|width| {
                let width = width
                    .clamp(ASSISTANT_MIN, widest.max(ASSISTANT_MIN))
                    .min(inner.width());
                let hidden = width * (1.0 - shown(input.assistant_shown));
                Rect::from_min_size(
                    pos2(area.right() - width + hidden, area.top()),
                    vec2(width, area.height()),
                )
            });
        let left = match sidebar {
            Some(sidebar) if !overlay => sidebar.right(),
            _ => rail.right(),
        };
        let right = match assistant {
            Some(assistant) if !overlay => assistant.left(),
            _ => area.right(),
        };
        let right = right.max(left);
        let breadcrumb = Rect::from_min_max(
            pos2(left, area.top()),
            pos2(right, (area.top() + BREADCRUMB_HEIGHT).min(area.bottom())),
        );
        let reading =
            Rect::from_min_max(pos2(left, breadcrumb.bottom()), pos2(right, area.bottom()));
        Self {
            rail: Some(rail),
            sidebar,
            assistant,
            inner,
            breadcrumb: Some(breadcrumb),
            reading,
            overlay,
        }
    }
}

/// Where along the progress meter (0..=1) each top-level outline entry
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

/// The page under `x` on a meter spanning `left..right`.
pub fn page_at(x: f32, left: f32, right: f32, pages: usize) -> usize {
    if pages == 0 || right <= left {
        return 0;
    }
    let fraction = ((x - left) / (right - left)).clamp(0.0, 1.0);
    ((fraction * pages as f32) as usize).min(pages - 1)
}

/// The outline path to the page `page` (from 0): the titles of the deepest
/// entry starting at or before it and its ancestors, outermost first.
/// `entries` are (depth, start page, title) in document order.
pub fn section_path<'a>(
    entries: impl IntoIterator<Item = (usize, Option<usize>, &'a str)>,
    page: usize,
) -> Vec<&'a str> {
    let mut path: Vec<(usize, &str)> = Vec::new();
    for (depth, start, title) in entries {
        let Some(start) = start else {
            continue;
        };
        if start > page {
            break;
        }
        while path.last().is_some_and(|&(last, _)| last >= depth) {
            path.pop();
        }
        path.push((depth, title));
    }
    path.into_iter().map(|(_, title)| title).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(width: f32) -> ShellInput {
        ShellInput {
            area: Rect::from_min_size(pos2(0.0, 36.0), vec2(width, 800.0)),
            sidebar: None,
            sidebar_shown: 1.0,
            assistant: None,
            assistant_shown: 1.0,
            present: false,
        }
    }

    #[test]
    fn open_panes_dock_beside_the_pages() {
        let closed = ShellLayout::compute(input(1440.0));
        let rail = closed.rail.unwrap();
        assert_eq!(rail.width(), RAIL_WIDTH);
        assert_eq!(closed.reading.left(), rail.right());
        assert_eq!(closed.reading.right(), 1440.0);
        let breadcrumb = closed.breadcrumb.unwrap();
        assert_eq!(breadcrumb.height(), BREADCRUMB_HEIGHT);
        assert_eq!(closed.reading.top(), breadcrumb.bottom());

        let open = ShellLayout::compute(ShellInput {
            sidebar: Some(SIDEBAR_WIDTH),
            assistant: Some(ASSISTANT_WIDTH),
            ..input(1440.0)
        });
        let (sidebar, assistant) = (open.sidebar.unwrap(), open.assistant.unwrap());
        assert!(!open.overlay);
        assert_eq!(sidebar.left(), rail.right());
        assert_eq!(open.reading.left(), sidebar.right());
        assert_eq!(open.reading.right(), assistant.left());
        assert_eq!(sidebar.width(), 300.0);
        assert_eq!(assistant.width(), 380.0);
        // Nothing overlaps the pages.
        assert!(!open.reading.intersects(sidebar.shrink(0.5)));
        assert!(!open.reading.intersects(assistant.shrink(0.5)));
        assert_eq!(open.breadcrumb.unwrap().x_range(), open.reading.x_range());
    }

    #[test]
    fn sliding_panes_push_the_pages_by_the_part_that_shows() {
        let layout = ShellLayout::compute(ShellInput {
            sidebar: Some(SIDEBAR_WIDTH),
            sidebar_shown: 0.25,
            assistant: Some(ASSISTANT_WIDTH),
            assistant_shown: 0.5,
            ..input(1440.0)
        });
        let (sidebar, assistant) = (layout.sidebar.unwrap(), layout.assistant.unwrap());
        assert_eq!(sidebar.width(), SIDEBAR_WIDTH, "laid out whole");
        assert_eq!(layout.reading.left(), RAIL_WIDTH + 75.0);
        assert_eq!(sidebar.right(), layout.reading.left());
        assert_eq!(layout.reading.right(), 1440.0 - 190.0);
        assert_eq!(assistant.left(), layout.reading.right());
        assert_eq!(layout.inner.left(), RAIL_WIDTH);
        let hidden = ShellLayout::compute(ShellInput {
            sidebar: Some(SIDEBAR_WIDTH),
            sidebar_shown: 0.0,
            ..input(1440.0)
        });
        assert_eq!(hidden.sidebar, None);
    }

    #[test]
    fn a_narrow_window_floats_the_panes_over_the_pages() {
        let layout = ShellLayout::compute(ShellInput {
            sidebar: Some(300.0),
            assistant: Some(380.0),
            ..input(800.0)
        });
        assert!(layout.overlay);
        assert_eq!(layout.reading.left(), RAIL_WIDTH);
        assert_eq!(layout.reading.right(), 800.0);
        assert!(layout.sidebar.unwrap().width() >= SIDEBAR_MIN);
    }

    #[test]
    fn panes_are_kept_to_a_share_of_the_window() {
        let layout = ShellLayout::compute(ShellInput {
            sidebar: Some(5000.0),
            assistant: Some(10.0),
            ..input(1400.0)
        });
        assert!(layout.sidebar.unwrap().width() <= 1400.0 * 0.45 + 0.5);
        assert_eq!(layout.assistant.unwrap().width(), ASSISTANT_MIN);
        assert!(layout.reading.width() >= 0.0);
    }

    #[test]
    fn present_mode_gives_everything_to_the_pages() {
        let layout = ShellLayout::compute(ShellInput {
            sidebar: Some(300.0),
            present: true,
            ..input(1400.0)
        });
        assert_eq!(layout.reading, input(1400.0).area);
        assert_eq!(
            (layout.rail, layout.sidebar, layout.breadcrumb),
            (None, None, None)
        );
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

    #[test]
    fn the_breadcrumb_follows_the_outline_to_the_page() {
        let outline = [
            (0, Some(0), "abstract"),
            (0, Some(2), "3 model architecture"),
            (1, Some(2), "3.1 encoder and decoder"),
            (1, Some(3), "3.2 attention"),
            (2, Some(3), "3.2.1 scaled dot-product"),
            (1, Some(5), "3.3 feed-forward"),
            (0, Some(9), "7 conclusion"),
        ];
        assert_eq!(
            section_path(outline, 4),
            [
                "3 model architecture",
                "3.2 attention",
                "3.2.1 scaled dot-product"
            ]
        );
        assert_eq!(
            section_path(outline, 6),
            ["3 model architecture", "3.3 feed-forward"]
        );
        assert_eq!(section_path(outline, 0), ["abstract"]);
        assert!(section_path(outline[1..].iter().copied(), 0).is_empty());
    }
}
