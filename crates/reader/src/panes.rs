//! Split panes: the stage's reading area divided side by side or one above
//! the other, one document per pane. The tree is craft-ui's docking model
//! with tab ids as panels and exactly one panel per group; tabs in no pane
//! wait in the top bar. Plain data, tested without a window.

use egui::Rect;
use glyphst_ui::craft::docking::{Action, Layout, Node, Placement, Zone};
use glyphst_ui::craft::layout::{SplitAxis, SplitSize, split_rect};

/// Space between panes, where the divider is.
pub const GAP: f32 = 12.0;
/// The smallest pane, in points along a split.
pub const MIN_PANE: f32 = 160.0;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Panes {
    layout: Layout<u64>,
    /// The pane that shows a tab when one is chosen from the top bar.
    focused: Option<u64>,
}

/// A divider between the two sides of a split.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Divider {
    /// The split's path from the root: false is the first child.
    pub path_bits: u64,
    pub depth: usize,
    pub axis: SplitAxis,
    pub size: SplitSize,
    pub rect: Rect,
    /// The first side's extent and the space for both, along the axis.
    pub first: f32,
    pub available: f32,
}

impl Divider {
    pub fn path(&self) -> Vec<bool> {
        (0..self.depth)
            .map(|level| self.path_bits >> level & 1 == 1)
            .collect()
    }
}

impl Panes {
    /// Panes from a saved layout of tab ids; one pane for anything invalid.
    pub fn restore(layout: Layout<u64>, focused: Option<u64>) -> Self {
        let panes = Self {
            focused: focused.filter(|tab| layout.contains(tab)),
            layout,
        };
        if panes.valid() {
            panes
        } else {
            Self::default()
        }
    }

    pub fn layout(&self) -> &Layout<u64> {
        &self.layout
    }

    pub fn focused(&self) -> Option<u64> {
        self.focused
    }

    pub fn count(&self) -> usize {
        self.tabs().len()
    }

    pub fn contains(&self, tab: u64) -> bool {
        self.layout.contains(&tab)
    }

    /// The tabs shown, left to right and top to bottom.
    pub fn tabs(&self) -> Vec<u64> {
        self.layout.panels().into_iter().copied().collect()
    }

    /// Shows `tab`: focuses the pane that has it, or puts it in the
    /// focused pane in place of that pane's tab. Returns the tab replaced.
    pub fn show(&mut self, tab: u64) -> Option<u64> {
        if self.contains(tab) {
            self.focused = Some(tab);
            return None;
        }
        let target = self.focused.or_else(|| self.tabs().first().copied());
        let replaced = match (target, self.layout.root.as_mut()) {
            (Some(old), Some(root)) => replace(root, old, tab).then_some(old),
            _ => None,
        };
        if replaced.is_none() {
            self.layout = Layout {
                root: Some(single(tab)),
                floating: Vec::new(),
            };
        }
        self.focused = Some(tab);
        replaced
    }

    /// Opens `tab` in a new pane on `zone`'s side of the pane showing
    /// `beside`, moving it from its own pane if it has one. A centre
    /// `zone` shows it in that pane instead. Returns whether it changed.
    pub fn split(&mut self, tab: u64, beside: u64, zone: Zone) -> bool {
        if tab == beside || !self.contains(beside) {
            return false;
        }
        if zone == Zone::Center {
            if self.contains(tab) {
                // Swap the two panes' tabs.
                if let Some(root) = self.layout.root.as_mut() {
                    replace(root, tab, u64::MAX);
                    replace(root, beside, tab);
                    replace(root, u64::MAX, beside);
                }
                self.focused = Some(tab);
                return true;
            }
            self.focused = Some(beside);
            return self.show(tab).is_some();
        }
        let moved = self
            .layout
            .apply(Action::OpenAt {
                panel: tab,
                anchor: beside,
                placement: Placement::Split(zone),
            })
            .is_ok();
        if moved {
            self.focused = Some(tab);
        }
        moved
    }

    /// Closes the pane showing `tab`, unless it is the only one; the tab
    /// stays open. Returns whether a pane closed.
    pub fn close_pane(&mut self, tab: u64) -> bool {
        if self.count() < 2 || self.layout.apply(Action::Close { panel: tab }).is_err() {
            return false;
        }
        if self.focused == Some(tab) {
            self.focused = self.tabs().first().copied();
        }
        true
    }

    /// Forgets a closed tab; its pane closes, or, if it was the only one,
    /// the stage has no pane until the next `show`.
    pub fn remove_tab(&mut self, tab: u64) {
        if !self.contains(tab) {
            return;
        }
        if self.count() < 2 {
            *self = Self::default();
            return;
        }
        self.close_pane(tab);
    }

    /// The pane after the focused one, wrapping around.
    pub fn next(&self) -> Option<u64> {
        let tabs = self.tabs();
        let index = self
            .focused
            .and_then(|focused| tabs.iter().position(|&tab| tab == focused))
            .map_or(0, |index| index + 1);
        tabs.get(index % tabs.len().max(1)).copied()
    }

    /// Sets the size of the split at `path`.
    pub fn resize(&mut self, path: Vec<bool>, size: SplitSize) {
        let _ = self.layout.apply(Action::ResizeSplit { path, size });
    }

    /// Each pane's tab and rectangle within `area`, and the dividers.
    pub fn rects(&self, area: Rect) -> (Vec<(u64, Rect)>, Vec<Divider>) {
        let mut panes = Vec::new();
        let mut dividers = Vec::new();
        if let Some(root) = &self.layout.root {
            lay_out(root, area, 0, 0, &mut panes, &mut dividers);
        }
        (panes, dividers)
    }

    fn valid(&self) -> bool {
        if self.layout.validate().is_err() || !self.layout.floating.is_empty() {
            return false;
        }
        let mut nodes: Vec<&Node<u64>> = self.layout.root.iter().collect();
        while let Some(node) = nodes.pop() {
            match node {
                Node::Split { first, second, .. } => {
                    nodes.push(first);
                    nodes.push(second);
                }
                Node::Tabs { panels, .. } if panels.len() != 1 => return false,
                Node::Tabs { .. } => {}
                Node::Stack { .. } => return false,
            }
        }
        true
    }
}

/// A layout with every panel mapped through `f` (tab ids to session
/// indices and back); `None` if one doesn't map or the layout floats.
pub fn map_layout<A, B>(layout: &Layout<A>, f: impl Fn(&A) -> Option<B>) -> Option<Layout<B>> {
    fn map_node<A, B>(node: &Node<A>, f: &impl Fn(&A) -> Option<B>) -> Option<Node<B>> {
        Some(match node {
            Node::Split {
                axis,
                size,
                first,
                second,
            } => Node::Split {
                axis: *axis,
                size: *size,
                first: Box::new(map_node(first, f)?),
                second: Box::new(map_node(second, f)?),
            },
            Node::Tabs { panels, active } => Node::Tabs {
                panels: panels.iter().map(f).collect::<Option<_>>()?,
                active: *active,
            },
            Node::Stack { .. } => return None,
        })
    }
    if !layout.floating.is_empty() {
        return None;
    }
    Some(Layout {
        root: match &layout.root {
            Some(root) => Some(map_node(root, &f)?),
            None => None,
        },
        floating: Vec::new(),
    })
}

fn single(tab: u64) -> Node<u64> {
    Node::Tabs {
        panels: vec![tab],
        active: 0,
    }
}

/// Replaces `old` with `new` in the tree; returns whether it was found.
fn replace(node: &mut Node<u64>, old: u64, new: u64) -> bool {
    match node {
        Node::Split { first, second, .. } => replace(first, old, new) || replace(second, old, new),
        Node::Tabs { panels, .. } => match panels.iter_mut().find(|panel| **panel == old) {
            Some(panel) => {
                *panel = new;
                true
            }
            None => false,
        },
        Node::Stack { .. } => false,
    }
}

fn lay_out(
    node: &Node<u64>,
    area: Rect,
    path_bits: u64,
    depth: usize,
    panes: &mut Vec<(u64, Rect)>,
    dividers: &mut Vec<Divider>,
) {
    match node {
        Node::Tabs { panels, .. } => {
            if let Some(&tab) = panels.first() {
                panes.push((tab, area));
            }
        }
        Node::Split {
            axis,
            size,
            first,
            second,
        } => {
            let Some(split) = split_rect(area, *axis, *size, GAP, MIN_PANE) else {
                return;
            };
            dividers.push(Divider {
                path_bits,
                depth,
                axis: *axis,
                size: *size,
                rect: split.divider,
                first: split.first_size(),
                available: split.available(),
            });
            // Paths deeper than 64 splits are not addressable; such trees
            // are rejected by the layout's own depth limit anyway.
            let second_bits = path_bits | 1u64.checked_shl(depth as u32).unwrap_or(0);
            lay_out(first, split.first, path_bits, depth + 1, panes, dividers);
            lay_out(
                second,
                split.second,
                second_bits,
                depth + 1,
                panes,
                dividers,
            );
        }
        Node::Stack { .. } => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{pos2, vec2};

    fn area() -> Rect {
        Rect::from_min_size(pos2(0.0, 0.0), vec2(1000.0, 800.0))
    }

    #[test]
    fn showing_a_tab_replaces_the_focused_panes_tab() {
        let mut panes = Panes::default();
        assert_eq!(panes.show(1), None);
        assert_eq!(panes.tabs(), [1]);
        assert_eq!(panes.show(2), Some(1), "one pane: the tab is replaced");
        assert_eq!((panes.tabs(), panes.focused()), (vec![2], Some(2)));
    }

    #[test]
    fn splitting_puts_a_tab_beside_another_and_closing_restores_one_pane() {
        let mut panes = Panes::default();
        panes.show(1);
        assert!(panes.split(2, 1, Zone::Right));
        assert_eq!((panes.tabs(), panes.focused()), (vec![1, 2], Some(2)));
        let (rects, dividers) = panes.rects(area());
        assert_eq!(rects.len(), 2);
        assert!(rects[0].1.right() < rects[1].1.left(), "side by side");
        assert_eq!(dividers.len(), 1);
        assert_eq!(dividers[0].axis, SplitAxis::Horizontal);

        // A tab already shown elsewhere moves; one shown in place swaps.
        assert!(panes.split(3, 2, Zone::Bottom));
        assert_eq!(panes.tabs(), [1, 2, 3]);
        assert!(panes.split(1, 3, Zone::Center));
        assert_eq!(panes.tabs(), [3, 2, 1]);
        assert!(!panes.split(2, 2, Zone::Left), "not beside itself");

        // Showing a tab focuses its pane without moving anything.
        assert_eq!(panes.show(2), None);
        assert_eq!(panes.focused(), Some(2));
        assert_eq!(panes.show(4), Some(2));
        assert_eq!(panes.tabs(), [3, 4, 1]);

        assert!(panes.close_pane(4));
        assert!(panes.close_pane(1));
        assert!(!panes.close_pane(3), "the last pane stays");
        assert_eq!(panes.tabs(), [3]);
    }

    #[test]
    fn closing_a_tab_closes_its_pane_and_focus_moves_on() {
        let mut panes = Panes::default();
        panes.show(1);
        panes.split(2, 1, Zone::Left);
        assert_eq!(panes.tabs(), [2, 1]);
        assert_eq!(panes.next(), Some(1));
        panes.remove_tab(2);
        assert_eq!((panes.tabs(), panes.focused()), (vec![1], Some(1)));
        panes.remove_tab(1);
        assert_eq!(panes.count(), 0);
        assert_eq!(panes.show(5), None);
        assert_eq!(panes.tabs(), [5]);
    }

    #[test]
    fn dividers_resize_their_split() {
        let mut panes = Panes::default();
        panes.show(1);
        panes.split(2, 1, Zone::Right);
        panes.split(3, 2, Zone::Bottom);
        let (_, dividers) = panes.rects(area());
        let inner = dividers
            .iter()
            .find(|divider| divider.axis == SplitAxis::Vertical)
            .expect("the bottom split");
        assert_eq!(inner.path(), [true]);
        panes.resize(inner.path(), SplitSize::Ratio(0.25));
        let (rects, _) = panes.rects(area());
        let (top, bottom) = (rects[1].1, rects[2].1);
        assert!((top.height() - (800.0 - GAP) * 0.25).abs() < 0.5);
        assert!(bottom.height() > top.height());
    }

    #[test]
    fn saved_layouts_are_checked() {
        let mut panes = Panes::default();
        panes.show(1);
        panes.split(2, 1, Zone::Right);
        let restored = Panes::restore(panes.layout().clone(), Some(2));
        assert_eq!(restored, panes);
        // Saved with tab indices, read back with new ids.
        let saved = map_layout(panes.layout(), |&id| Some(id as usize - 1)).unwrap();
        let json = serde_json::to_string(&saved).unwrap();
        let read: Layout<usize> = serde_json::from_str(&json).unwrap();
        let ids = [10, 20];
        let layout = map_layout(&read, |&index| ids.get(index).copied()).unwrap();
        assert_eq!(Panes::restore(layout, Some(20)).tabs(), [10, 20]);
        assert!(map_layout(&read, |&index| (index == 0).then_some(1)).is_none());

        let shared = Layout {
            root: Some(Node::Tabs {
                panels: vec![1, 2],
                active: 0,
            }),
            floating: Vec::new(),
        };
        assert_eq!(Panes::restore(shared, Some(1)), Panes::default());
        let mut duplicate = panes.layout().clone();
        if let Some(Node::Split { second, .. }) = duplicate.root.as_mut() {
            **second = single(1);
        }
        assert_eq!(Panes::restore(duplicate, None), Panes::default());
    }
}
