//! The book switcher (frames 10a–f): Ctrl+Tab through the open books in
//! the order they were last read. Holding Ctrl, each Tab moves on and the
//! view behind already shows that book (a peek); letting go keeps it, and a
//! quick tap is a switch to the previous book before anything shows.
//! Escape goes back to the book it started from. Opened from a command
//! instead, it stays until a book is picked.
//!
//! This is the state alone; `app::switcher` draws it and runs the switches.

pub mod layout;

use std::time::{Duration, Instant};

/// How long Ctrl+Tab is held before the cards show: a quicker tap switches
/// without them.
pub const SHOW_AFTER: Duration = Duration::from_millis(180);
/// How much faster than a switch a peek's transition runs, so moving
/// along quickly stays responsive.
pub const PEEK_SPEED: f32 = 1.0 / 0.6;
/// How long the pointer rests on a card before it is highlighted.
pub const HOVER_DELAY: Duration = Duration::from_millis(120);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Opened by Ctrl+Tab: letting go of the modifier picks.
    Held,
    /// Opened by a command: Enter or a click picks.
    Sticky,
}

/// What the app does after a change.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Step {
    /// Show this book behind the cards; +1 moving on, -1 back.
    Peek { tab: u64, direction: f32 },
    /// Keep this book (the one peeked last).
    Keep(u64),
    /// Back to the book it started from (`None`: Home).
    Back(Option<u64>),
}

#[derive(Clone, Debug)]
struct Open {
    mode: Mode,
    /// The open books in recent order, as they were when it opened.
    list: Vec<u64>,
    highlight: usize,
    /// The book it started from.
    origin: Option<u64>,
    opened: Instant,
}

#[derive(Clone, Debug, Default)]
pub struct Switcher {
    open: Option<Open>,
    /// Open books, the one read last first.
    recent: Vec<u64>,
}

impl Switcher {
    pub fn is_open(&self) -> bool {
        self.open.is_some()
    }

    pub fn mode(&self) -> Option<Mode> {
        self.open.as_ref().map(|open| open.mode)
    }

    /// Whether the cards show: at once when sticky, after
    /// [`SHOW_AFTER`] when held.
    pub fn shows(&self, now: Instant) -> bool {
        self.open.as_ref().is_some_and(|open| {
            open.mode == Mode::Sticky || now.saturating_duration_since(open.opened) >= SHOW_AFTER
        })
    }

    /// The books it lists, in recent order.
    pub fn list(&self) -> &[u64] {
        self.open.as_ref().map_or(&[], |open| &open.list)
    }

    pub fn highlight(&self) -> Option<usize> {
        self.open.as_ref().map(|open| open.highlight)
    }

    /// The book it started from.
    pub fn origin(&self) -> Option<u64> {
        self.open.as_ref().and_then(|open| open.origin)
    }

    /// Books in recent order.
    #[cfg(test)]
    pub fn recent(&self) -> &[u64] {
        &self.recent
    }

    /// Opens on the book read before `origin` (the last one with `back`);
    /// from Home (`origin` `None`) on the book read last. `None` when there
    /// is no other book to go to.
    pub fn open(
        &mut self,
        mode: Mode,
        origin: Option<u64>,
        back: bool,
        now: Instant,
    ) -> Option<Step> {
        let list = self.recent.clone();
        let others = list.len() - usize::from(origin.is_some_and(|tab| list.contains(&tab)));
        if others == 0 {
            return None;
        }
        let highlight = match (origin, back) {
            (_, true) => list.len() - 1,
            (Some(tab), false) if list.first() == Some(&tab) => 1,
            _ => 0,
        };
        let tab = list[highlight];
        self.open = Some(Open {
            mode,
            list,
            highlight,
            origin,
            opened: now,
        });
        Some(Step::Peek {
            tab,
            direction: if back { -1.0 } else { 1.0 },
        })
    }

    /// Moves the highlight `delta` places, wrapping.
    pub fn step(&mut self, delta: i32) -> Option<Step> {
        let open = self.open.as_mut()?;
        let count = open.list.len() as i32;
        let index = (open.highlight as i32 + delta).rem_euclid(count) as usize;
        Self::highlight_at(open, index, delta.signum() as f32)
    }

    /// Highlights card `index` (pointed at).
    pub fn point(&mut self, index: usize) -> Option<Step> {
        let open = self.open.as_mut()?;
        let direction = if index >= open.highlight { 1.0 } else { -1.0 };
        Self::highlight_at(open, index, direction)
    }

    fn highlight_at(open: &mut Open, index: usize, direction: f32) -> Option<Step> {
        if index == open.highlight || index >= open.list.len() {
            return None;
        }
        open.highlight = index;
        Some(Step::Peek {
            tab: open.list[index],
            direction,
        })
    }

    /// Keeps the highlighted book and closes.
    pub fn keep(&mut self) -> Option<Step> {
        let open = self.open.take()?;
        Some(Step::Keep(open.list[open.highlight]))
    }

    /// Goes back to the book it started from and closes.
    pub fn back(&mut self) -> Option<Step> {
        let open = self.open.take()?;
        Some(Step::Back(open.origin))
    }

    /// Once a frame: the active book becomes the most recent (not while
    /// the switcher peeks), closed books leave and new ones join last.
    pub fn settle(&mut self, active: Option<u64>, open_tabs: &[u64]) {
        self.recent.retain(|tab| open_tabs.contains(tab));
        for tab in open_tabs {
            if !self.recent.contains(tab) {
                self.recent.push(*tab);
            }
        }
        if let Some(open) = &mut self.open {
            open.list.retain(|tab| open_tabs.contains(tab));
            if open.list.is_empty() {
                self.open = None;
            } else {
                open.highlight = open.highlight.min(open.list.len() - 1);
            }
            return;
        }
        if let Some(active) = active
            && let Some(at) = self.recent.iter().position(|tab| *tab == active)
        {
            let tab = self.recent.remove(at);
            self.recent.insert(0, tab);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Books 1, 2, 3 read in that order, 3 last.
    fn switcher() -> Switcher {
        let mut switcher = Switcher::default();
        for active in [1, 2, 3] {
            switcher.settle(Some(active), &[1, 2, 3]);
        }
        assert_eq!(switcher.recent(), [3, 2, 1]);
        switcher
    }

    #[test]
    fn ctrl_tab_peeks_at_the_previous_book_and_letting_go_keeps_it() {
        let mut switcher = switcher();
        let now = Instant::now();
        assert_eq!(
            switcher.open(Mode::Held, Some(3), false, now),
            Some(Step::Peek {
                tab: 2,
                direction: 1.0
            })
        );
        assert!(!switcher.shows(now), "a quick tap shows nothing");
        assert!(switcher.shows(now + SHOW_AFTER));
        // The peek doesn't reorder: the list is as it opened.
        switcher.settle(Some(2), &[1, 2, 3]);
        assert_eq!(switcher.recent(), [3, 2, 1]);
        assert_eq!(switcher.keep(), Some(Step::Keep(2)));
        assert!(!switcher.is_open());
        switcher.settle(Some(2), &[1, 2, 3]);
        assert_eq!(switcher.recent(), [2, 3, 1]);
    }

    #[test]
    fn tab_moves_on_and_wraps_and_escape_goes_back() {
        let mut switcher = switcher();
        let now = Instant::now();
        switcher.open(Mode::Held, Some(3), false, now);
        assert_eq!(
            switcher.step(1),
            Some(Step::Peek {
                tab: 1,
                direction: 1.0
            })
        );
        assert_eq!(
            switcher.step(1),
            Some(Step::Peek {
                tab: 3,
                direction: 1.0
            })
        );
        assert_eq!(
            switcher.step(-1),
            Some(Step::Peek {
                tab: 1,
                direction: -1.0
            })
        );
        assert_eq!(switcher.back(), Some(Step::Back(Some(3))));
        assert_eq!(switcher.keep(), None, "a release after Escape does nothing");
    }

    #[test]
    fn shift_opens_on_the_least_recent_and_home_on_the_most_recent() {
        let mut switcher = switcher();
        let now = Instant::now();
        assert_eq!(
            switcher.open(Mode::Held, Some(3), true, now),
            Some(Step::Peek {
                tab: 1,
                direction: -1.0
            })
        );
        switcher.back();
        assert_eq!(
            switcher.open(Mode::Sticky, None, false, now),
            Some(Step::Peek {
                tab: 3,
                direction: 1.0
            })
        );
        assert!(switcher.shows(now), "sticky shows at once");
        assert_eq!(switcher.back(), Some(Step::Back(None)));
    }

    #[test]
    fn one_book_has_nowhere_to_go_unless_from_home() {
        let mut switcher = Switcher::default();
        switcher.settle(Some(7), &[7]);
        let now = Instant::now();
        assert_eq!(switcher.open(Mode::Held, Some(7), false, now), None);
        assert!(!switcher.is_open());
        assert_eq!(
            switcher.open(Mode::Held, None, false, now),
            Some(Step::Peek {
                tab: 7,
                direction: 1.0
            })
        );
    }

    #[test]
    fn pointing_highlights_and_closing_a_book_leaves_the_list() {
        let mut switcher = switcher();
        let now = Instant::now();
        switcher.open(Mode::Sticky, Some(3), false, now);
        assert_eq!(
            switcher.point(2),
            Some(Step::Peek {
                tab: 1,
                direction: 1.0
            })
        );
        assert_eq!(switcher.point(2), None, "already highlighted");
        switcher.settle(Some(1), &[2, 3]);
        assert_eq!(switcher.list(), [3, 2]);
        assert_eq!(switcher.highlight(), Some(1));
        switcher.keep();
        switcher.settle(None, &[2, 3, 4]);
        assert_eq!(switcher.recent(), [3, 2, 4], "a new book joins last");
    }
}
