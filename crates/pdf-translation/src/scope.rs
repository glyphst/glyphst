//! Which pages a translation may work on, and in what order.
//!
//! A scope either covers the whole document (from the page being read
//! forward, then the earlier pages) or follows the reader: the page being
//! read and the next few, plus every page the reader has already been on.
//! The reader moves the focus while the scheduler and OCR read the scope, so
//! it is shared and wakes waiting work when it changes.

use std::collections::BTreeSet;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

/// Ranks start here for whole-document pages after the focus…
const WHOLE_FORWARD: u32 = 1_000;
/// …and here for the ones before it.
const WHOLE_BACKWARD: u32 = 100_000;

#[derive(Debug)]
pub struct TranslationScope {
    state: Mutex<ScopeState>,
    changed: Condvar,
}

#[derive(Debug, Clone, Default)]
struct ScopeState {
    focus: usize,
    ahead: usize,
    whole: bool,
    touched: BTreeSet<usize>,
    /// Only the pages given stay in scope: the reader moving doesn't add any.
    fixed: bool,
    generation: u64,
}

/// A snapshot of a scope, taken once per scheduling step.
#[derive(Debug, Clone, Default)]
pub struct ScopeView {
    state: ScopeState,
}

impl TranslationScope {
    /// The whole document, the first page first.
    pub fn whole() -> Arc<Self> {
        Self::new(ScopeState {
            whole: true,
            ..ScopeState::default()
        })
    }

    /// The page being read and the `ahead` pages after it.
    pub fn following(focus: usize, ahead: usize) -> Arc<Self> {
        Self::new(ScopeState {
            focus,
            ahead,
            touched: (focus..=focus + ahead).collect(),
            ..ScopeState::default()
        })
    }

    /// Exactly `pages` (a kept translation shown again), until it is
    /// expanded to the whole document.
    pub fn pages(pages: impl IntoIterator<Item = usize>) -> Arc<Self> {
        let touched: BTreeSet<usize> = pages.into_iter().collect();
        Self::new(ScopeState {
            focus: touched.first().copied().unwrap_or_default(),
            touched,
            fixed: true,
            ..ScopeState::default()
        })
    }

    fn new(state: ScopeState) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(state),
            changed: Condvar::new(),
        })
    }

    fn lock(&self) -> MutexGuard<'_, ScopeState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn bump(&self, mut state: MutexGuard<'_, ScopeState>) {
        state.generation += 1;
        drop(state);
        self.changed.notify_all();
    }

    /// Moves the focus to `page`; its window joins the pages kept in scope.
    pub fn set_focus(&self, page: usize) {
        let mut state = self.lock();
        if state.focus == page || state.fixed {
            return;
        }
        state.focus = page;
        let ahead = state.ahead;
        state.touched.extend(page..=page + ahead);
        self.bump(state);
    }

    /// From now on every page is in scope.
    pub fn expand_to_whole(&self) {
        let mut state = self.lock();
        if state.whole {
            return;
        }
        state.whole = true;
        self.bump(state);
    }

    pub fn is_whole(&self) -> bool {
        self.lock().whole
    }

    pub fn focus(&self) -> usize {
        self.lock().focus
    }

    /// The page being read and the last page translating with it.
    pub fn window(&self) -> (usize, usize) {
        let state = self.lock();
        (state.focus, state.focus + state.ahead)
    }

    /// Wakes waiters without changing the scope, when the readiness of pages
    /// changed (an OCR result arrived).
    pub fn notify(&self) {
        let state = self.lock();
        self.bump(state);
    }

    pub fn view(&self) -> ScopeView {
        ScopeView {
            state: self.lock().clone(),
        }
    }

    pub fn generation(&self) -> u64 {
        self.lock().generation
    }

    /// Waits until the generation passes `seen` or `timeout` elapses, and
    /// returns the generation then.
    pub fn wait_changed(&self, seen: u64, timeout: Duration) -> u64 {
        let state = self.lock();
        let (state, _) = self
            .changed
            .wait_timeout_while(state, timeout, |state| state.generation == seen)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.generation
    }
}

impl ScopeView {
    pub fn generation(&self) -> u64 {
        self.state.generation
    }

    pub fn is_whole(&self) -> bool {
        self.state.whole
    }

    /// How urgent `page` is, lower first; `None` while it is out of scope.
    pub fn rank(&self, page: usize) -> Option<u32> {
        self.rank_within(page, self.state.focus, self.state.focus + self.state.ahead)
    }

    /// The rank of work spanning `pages`: that of its most urgent page.
    pub fn rank_pages(&self, pages: impl IntoIterator<Item = usize>) -> Option<(u32, usize)> {
        pages
            .into_iter()
            .filter_map(|page| self.rank(page).map(|rank| (rank, page)))
            .min()
    }

    /// The rank for recognising `page`: the window widened by a page on each
    /// side, so paragraphs running across its edges can join up.
    pub fn ocr_rank(&self, page: usize) -> Option<u32> {
        let ScopeState { focus, ahead, .. } = self.state;
        self.rank_within(page, focus.saturating_sub(1), focus + ahead + 1)
    }

    fn rank_within(&self, page: usize, first: usize, last: usize) -> Option<u32> {
        let ScopeState {
            focus,
            ahead,
            whole,
            ..
        } = self.state;
        let distance = page.abs_diff(focus) as u32;
        if (first..=last).contains(&page) {
            return Some(distance);
        }
        if self.state.touched.contains(&page) {
            return Some(ahead as u32 + 1 + distance);
        }
        if whole {
            return Some(if page > focus {
                WHOLE_FORWARD + distance
            } else {
                WHOLE_BACKWARD + distance
            });
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_following_scope_ranks_its_window_and_parks_the_rest() {
        let scope = TranslationScope::following(4, 2);
        let view = scope.view();
        assert_eq!(view.rank(4), Some(0));
        assert_eq!(view.rank(6), Some(2));
        assert_eq!(view.rank(7), None);
        assert_eq!(view.rank(3), None);
        assert_eq!(view.ocr_rank(3), Some(1));
        assert_eq!(view.ocr_rank(7), Some(3));
    }

    #[test]
    fn a_scope_of_pages_keeps_to_them_until_it_takes_the_whole() {
        let scope = TranslationScope::pages([2, 5]);
        scope.set_focus(9);
        let view = scope.view();
        assert!(view.rank(2).is_some() && view.rank(5).is_some());
        assert_eq!(view.rank(9), None);
        assert_eq!(view.rank(3), None);
        scope.expand_to_whole();
        assert!(scope.view().rank(9).is_some());
    }

    #[test]
    fn pages_once_in_view_stay_in_scope_after_the_reader_moves_on() {
        let scope = TranslationScope::following(0, 2);
        scope.set_focus(10);
        let view = scope.view();
        assert_eq!(view.rank(10), Some(0));
        assert_eq!(view.rank(1), Some(3 + 9));
        assert_eq!(view.rank(5), None);
    }

    #[test]
    fn the_whole_document_goes_forward_from_the_focus_then_back() {
        let scope = TranslationScope::following(5, 0);
        scope.expand_to_whole();
        let view = scope.view();
        assert!(view.rank(9).unwrap() < view.rank(2).unwrap());
        assert!(view.rank(5).unwrap() < view.rank(9).unwrap());
        assert_eq!(view.rank_pages([2, 9]), Some((view.rank(9).unwrap(), 9)));
        // A whole scope from the start is earliest-page-first.
        let whole = TranslationScope::whole().view();
        assert!(whole.rank(0) < whole.rank(1) && whole.rank(1) < whole.rank(2));
    }

    #[test]
    fn the_generation_moves_only_on_change() {
        let scope = TranslationScope::following(3, 2);
        let start = scope.generation();
        scope.set_focus(3);
        assert_eq!(scope.generation(), start);
        scope.set_focus(4);
        scope.expand_to_whole();
        scope.expand_to_whole();
        assert_eq!(scope.generation(), start + 2);
        assert_eq!(
            scope.wait_changed(start, Duration::from_millis(1)),
            start + 2
        );
    }
}
