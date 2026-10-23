//! Arranging the document tabs (frame 02l): moving one along the bar,
//! pinning, duplicating, and reopening the ones closed. Pinned tabs stay
//! together at the left; a tab moves only among its own kind.

use super::*;

/// Documents remembered for reopening.
const CLOSED_KEPT: usize = 20;

impl ReaderApp {
    /// How many tabs are pinned: they are the first ones.
    pub(super) fn pinned_count(&self) -> usize {
        self.tabs.iter().take_while(|tab| tab.pinned).count()
    }

    /// Moves tab `from` to before slot `slot` of the bar (`tabs.len()`:
    /// the end), kept among the pinned tabs or the others as it is.
    pub(crate) fn move_tab(&mut self, from: usize, slot: usize) {
        if from >= self.tabs.len() {
            return;
        }
        let to = move_target(
            from,
            slot,
            self.pinned_count(),
            self.tabs[from].pinned,
            self.tabs.len(),
        );
        self.place_tab(from, to);
    }

    /// Moves page tab `from` (the library, shortcuts…) to before slot
    /// `slot` among the page tabs.
    pub(crate) fn move_page(&mut self, from: usize, slot: usize) {
        if from >= self.pages.len() {
            return;
        }
        let to = move_target(from, slot, 0, false, self.pages.len());
        if to == from {
            return;
        }
        let page = self.pages.remove(from);
        self.pages.insert(to, page);
        self.page_focused = self
            .page_focused
            .map(|focused| moved_index(focused, from, to));
    }

    /// Puts tab `from` at index `to`; the active tab stays active.
    fn place_tab(&mut self, from: usize, to: usize) {
        if from == to || from >= self.tabs.len() || to >= self.tabs.len() {
            return;
        }
        let tab = self.tabs.remove(from);
        self.tabs.insert(to, tab);
        self.active = self.active.map(|active| moved_index(active, from, to));
    }

    /// Pins tab `index` after the pinned tabs, or unpins it to the first
    /// place after them.
    pub(crate) fn toggle_pin(&mut self, index: usize) {
        let Some(tab) = self.tabs.get_mut(index) else {
            return;
        };
        tab.pinned = !tab.pinned;
        let pinned = tab.pinned;
        let others = (0..self.tabs.len())
            .filter(|&other| other != index && self.tabs[other].pinned)
            .count();
        // With the others pinned before it, a pin lands right after them
        // and an unpin right after them too.
        let to = if pinned {
            others
        } else {
            others.min(self.tabs.len() - 1)
        };
        self.place_tab(index, to);
    }

    /// A second tab on tab `index`'s document at its place, right after
    /// it (after the pinned tabs, as it is not pinned).
    pub(crate) fn duplicate_tab(&mut self, index: usize, ctx: &egui::Context) {
        let Some(tab) = self.tabs.get(index) else {
            return;
        };
        let (path, position) = (tab.path.clone(), tab.position());
        let count = self.tabs.len();
        self.open_new(&path, ctx, None, Some(position));
        if self.tabs.len() > count {
            let to = (index + 1).max(self.pinned_count());
            self.place_tab(self.tabs.len() - 1, to);
        }
    }

    /// Keeps `path` for reopening.
    pub(super) fn remember_closed(&mut self, path: &Path) {
        self.closed_tabs.retain(|closed| closed != path);
        self.closed_tabs.push(path.to_path_buf());
        if self.closed_tabs.len() > CLOSED_KEPT {
            self.closed_tabs.remove(0);
        }
    }

    pub(crate) fn can_reopen_tab(&self) -> bool {
        !self.closed_tabs.is_empty()
    }

    /// Opens the document closed last again, where it was left.
    pub(crate) fn reopen_closed_tab(&mut self, ctx: &egui::Context) {
        while let Some(path) = self.closed_tabs.pop() {
            if path.exists() {
                self.open(&path, ctx, None);
                return;
            }
        }
    }
}

/// The index a tab moved from `from` to before slot `slot` (of `count`
/// slots) takes, kept among the `pinned` first tabs when it is pinned and
/// after them when not.
pub(super) fn move_target(
    from: usize,
    slot: usize,
    pinned: usize,
    is_pinned: bool,
    count: usize,
) -> usize {
    let to = if slot > from { slot - 1 } else { slot };
    let (first, last) = if is_pinned {
        (0, pinned.saturating_sub(1))
    } else {
        (pinned, count.saturating_sub(1))
    };
    to.clamp(first, last.max(first))
}

/// Where the tab at `index` is after the tab at `from` moved to `to`.
pub(super) fn moved_index(index: usize, from: usize, to: usize) -> usize {
    if index == from {
        to
    } else if from < index && index <= to {
        index - 1
    } else if to <= index && index < from {
        index + 1
    } else {
        index
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tab_lands_in_the_slot_it_is_dropped_on() {
        // Four tabs, none pinned: dropping the first before the third
        // puts it second.
        assert_eq!(move_target(0, 2, 0, false, 4), 1);
        assert_eq!(move_target(0, 4, 0, false, 4), 3, "the end");
        assert_eq!(move_target(3, 0, 0, false, 4), 0);
        assert_eq!(move_target(2, 2, 0, false, 4), 2, "its own slot");
        assert_eq!(move_target(2, 3, 0, false, 4), 2, "just after itself");
    }

    #[test]
    fn pinned_and_other_tabs_keep_to_their_side() {
        // Two pinned tabs, then three others.
        assert_eq!(move_target(3, 0, 2, false, 5), 2, "not among the pinned");
        assert_eq!(move_target(0, 5, 2, true, 5), 1, "not among the others");
        assert_eq!(move_target(1, 0, 2, true, 5), 0);
    }

    #[test]
    fn the_other_tabs_shift_around_a_move() {
        // [a b c d], b moved to the end: [a c d b].
        let after: Vec<usize> = (0..4).map(|index| moved_index(index, 1, 3)).collect();
        assert_eq!(after, [0, 3, 1, 2]);
        // d moved to the front: [d a b c].
        let after: Vec<usize> = (0..4).map(|index| moved_index(index, 3, 0)).collect();
        assert_eq!(after, [1, 2, 3, 0]);
    }
}
