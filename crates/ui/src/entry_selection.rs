use std::collections::BTreeSet;

use gpui::Modifiers;

/// Platform-standard selection state for an ordered collection of entries.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EntrySelection<T> {
    selected: BTreeSet<T>,
    anchor: Option<T>,
}

impl<T> EntrySelection<T>
where
    T: Clone + Ord,
{
    pub fn new(selected: BTreeSet<T>, anchor: Option<T>) -> Self {
        Self { selected, anchor }
    }

    pub fn selected(&self) -> &BTreeSet<T> {
        &self.selected
    }

    pub fn anchor(&self) -> Option<&T> {
        self.anchor.as_ref()
    }

    pub fn into_parts(self) -> (BTreeSet<T>, Option<T>) {
        (self.selected, self.anchor)
    }

    pub fn clear(&mut self) {
        self.selected.clear();
        self.anchor = None;
    }

    pub fn click(&mut self, ordered: &[T], target: T, modifiers: Modifiers) {
        let secondary = modifiers.secondary();
        if modifiers.shift {
            let anchor = self
                .anchor
                .as_ref()
                .filter(|anchor| ordered.contains(anchor))
                .cloned()
                .unwrap_or_else(|| target.clone());
            let range = inclusive_range(ordered, &anchor, &target)
                .unwrap_or_else(|| BTreeSet::from([target.clone()]));
            if secondary {
                self.selected.extend(range);
            } else {
                self.selected = range;
            }
            self.anchor = Some(anchor);
        } else if secondary {
            if !self.selected.insert(target.clone()) {
                self.selected.remove(&target);
            }
            self.anchor = Some(target);
        } else {
            self.selected.clear();
            self.selected.insert(target.clone());
            self.anchor = Some(target);
        }
    }
}

/// Apply [`EntrySelection`] semantics to state stored directly by an existing view.
pub fn apply_entry_click<T>(
    selected: &mut BTreeSet<T>,
    anchor: &mut Option<T>,
    ordered: &[T],
    target: T,
    modifiers: Modifiers,
) where
    T: Clone + Ord,
{
    let mut state = EntrySelection::new(std::mem::take(selected), anchor.take());
    state.click(ordered, target, modifiers);
    (*selected, *anchor) = state.into_parts();
}

fn inclusive_range<T>(ordered: &[T], anchor: &T, target: &T) -> Option<BTreeSet<T>>
where
    T: Clone + Ord,
{
    let anchor_ix = ordered.iter().position(|entry| entry == anchor)?;
    let target_ix = ordered.iter().position(|entry| entry == target)?;
    let start = anchor_ix.min(target_ix);
    let end = anchor_ix.max(target_ix);
    Some(ordered[start..=end].iter().cloned().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secondary() -> Modifiers {
        Modifiers::secondary_key()
    }

    #[test]
    fn pointer_selection_uses_desktop_semantics() {
        let ordered = [1, 2, 3, 4, 5];
        let mut selection = EntrySelection::default();

        selection.click(&ordered, 2, Modifiers::default());
        assert_eq!(selection.selected, BTreeSet::from([2]));
        assert_eq!(selection.anchor, Some(2));

        selection.click(&ordered, 4, secondary());
        assert_eq!(selection.selected, BTreeSet::from([2, 4]));
        assert_eq!(selection.anchor, Some(4));

        selection.click(
            &ordered,
            2,
            Modifiers {
                shift: true,
                ..Modifiers::default()
            },
        );
        assert_eq!(selection.selected, BTreeSet::from([2, 3, 4]));
        assert_eq!(selection.anchor, Some(4));

        selection.click(
            &ordered,
            1,
            Modifiers {
                shift: true,
                ..secondary()
            },
        );
        assert_eq!(selection.selected, BTreeSet::from([1, 2, 3, 4]));
    }

    #[test]
    fn missing_anchor_falls_back_to_target() {
        let mut selection = EntrySelection::new(BTreeSet::from([9]), Some(9));
        selection.click(
            &[1, 2, 3],
            2,
            Modifiers {
                shift: true,
                ..Modifiers::default()
            },
        );
        assert_eq!(selection.selected, BTreeSet::from([2]));
        assert_eq!(selection.anchor, Some(2));
    }
}
