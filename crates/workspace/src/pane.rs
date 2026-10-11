//! Workspace panes: the tab groups of the GPUI Kit dock's center region, as
//! the workspace sees them.
//!
//! GPUI Kit's [`DockArea`] owns the layout — which tabs sit in which group,
//! the displayed tab, splits and their sizes, and panel drag and drop. A pane
//! is one of its tab groups, named by the group's node id. [`Pane`] is a
//! read-only snapshot of one, and [`PaneLayout`] a snapshot of the whole
//! center, used for sessions and for rearrangements the dock has no edit for.

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::Arc,
};

use glyphst_translation_ui::DocumentTranslationId;
use gpui::{
    App, Axis, Bounds, Context, Entity, EntityId, FocusHandle, IntoElement, Pixels, Render,
    SharedString, Styled as _, Window, px,
};
use gpui_component::{
    ActiveTheme as _, Selectable as _, Sizable as _,
    dock::{
        BasePanelView as PanelView, DockArea, DockLayout, DockPlacement, NodeId, PaneNode, PaneRef,
        PanelId,
    },
    tab::Tab as ComponentTab,
};

pub use glyphst_actions::pane::{
    SplitDown, SplitHorizontal, SplitLeft, SplitRight, SplitUp, SplitVertical,
};

use crate::{
    document_view::DocumentView,
    library::{
        DocumentPosition, ReaderPaneSession, ReaderPaneTree, ReaderSplitAxis, ReaderTabSession,
        normalize_path,
    },
    settings::ui_rems,
    synchronized_tabs::{SynchronizedGroupId, SynchronizedGroupSummary},
    tab_item::TabItem,
};

mod render;

pub(crate) use render::{PANE_SURFACE_MARGIN, PaneGeometry, ReaderDockSkin};

pub(crate) const PANE_TAB_BAR_HEIGHT: f32 = 32.0;
pub(crate) const PANE_TAB_GAP: f32 = 2.0;
pub(crate) const PANE_TAB_MAX_WIDTH: f32 = 220.0;
pub(crate) const PANE_TAB_MIN_WIDTH: f32 = 120.0;
pub(crate) const PANE_DIRECTION_PROBE_DISTANCE: f32 = glyphst_ui::style::SURFACE_GAP + 1.0;
pub(crate) const RESIZE_STEP: f32 = 80.0;
/// The smallest a pane may be resized to by keyboard.
pub(crate) const PANE_MIN_SIZE: f32 = 120.0;
/// The total a restored split's ratios are spread over. The dock re-expresses
/// slot sizes as shares of the real container on its first layout pass.
const RESTORED_SPLIT_TOTAL: f32 = 1000.0;

/// A pane: one tab group in the center of the dock.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct PaneId(NodeId);

impl PaneId {
    pub(crate) fn node(self) -> NodeId {
        self.0
    }

    pub(crate) fn as_u64(self) -> u64 {
        self.0.as_u64()
    }

    #[cfg(test)]
    pub(crate) fn from_u64(raw: u64) -> Self {
        Self(NodeId::from_u64(raw))
    }
}

impl From<NodeId> for PaneId {
    fn from(node: NodeId) -> Self {
        Self(node)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SplitDirection {
    Up,
    Down,
    Left,
    Right,
}

impl SplitDirection {
    pub(crate) fn axis(self) -> Axis {
        match self {
            Self::Up | Self::Down => Axis::Vertical,
            Self::Left | Self::Right => Axis::Horizontal,
        }
    }

    pub(crate) fn increasing(self) -> bool {
        matches!(self, Self::Down | Self::Right)
    }

    pub(crate) fn placement(self) -> gpui_component::Placement {
        match self {
            Self::Up => gpui_component::Placement::Top,
            Self::Down => gpui_component::Placement::Bottom,
            Self::Left => gpui_component::Placement::Left,
            Self::Right => gpui_component::Placement::Right,
        }
    }
}

/// A snapshot of one pane: its tabs in order and the displayed one.
#[derive(Clone)]
pub(crate) struct Pane {
    id: PaneId,
    tabs: Vec<Entity<TabItem>>,
    active_ix: usize,
}

impl Pane {
    /// The snapshot of a pane that no longer exists: no tabs.
    pub(crate) fn missing(id: PaneId) -> Self {
        Self {
            id,
            tabs: Vec::new(),
            active_ix: 0,
        }
    }

    pub(crate) fn id(&self) -> PaneId {
        self.id
    }

    pub(crate) fn tabs(&self) -> &[Entity<TabItem>] {
        &self.tabs
    }

    pub(crate) fn len(&self) -> usize {
        self.tabs.len()
    }

    pub(crate) fn active_tab_ix(&self) -> usize {
        self.active_ix
    }

    pub(crate) fn active_tab(&self) -> Option<&Entity<TabItem>> {
        self.tabs.get(self.active_ix)
    }

    pub(crate) fn active_tab_id(&self) -> Option<EntityId> {
        self.active_tab().map(Entity::entity_id)
    }

    pub(crate) fn tab_ids(&self) -> Vec<EntityId> {
        self.tabs.iter().map(Entity::entity_id).collect()
    }

    pub(crate) fn tab_id_at(&self, tab_ix: usize) -> Option<EntityId> {
        self.tabs.get(tab_ix).map(Entity::entity_id)
    }

    pub(crate) fn tab(&self, tab_id: EntityId) -> Option<&Entity<TabItem>> {
        self.tabs.iter().find(|tab| tab.entity_id() == tab_id)
    }

    pub(crate) fn has_tab_id(&self, tab_id: EntityId) -> bool {
        self.tab(tab_id).is_some()
    }

    pub(crate) fn tab_ix_for_entity_id(&self, tab_id: EntityId) -> Option<usize> {
        self.tabs.iter().position(|tab| tab.entity_id() == tab_id)
    }

    pub(crate) fn active_tab_is_new_tab(&self, cx: &App) -> bool {
        self.active_tab()
            .is_some_and(|tab| tab.read(cx).is_new_tab())
    }

    pub(crate) fn active_tab_is_loading(&self, cx: &App) -> bool {
        self.active_tab()
            .is_some_and(|tab| tab.read(cx).is_loading())
    }

    pub(crate) fn active_tab_supports_zen_mode(&self, cx: &App) -> bool {
        self.active_tab()
            .is_some_and(|tab| tab.read(cx).supports_zen_mode())
    }

    pub(crate) fn active_tab_label(&self, cx: &App) -> Option<SharedString> {
        self.active_tab().map(|tab| tab.read(cx).label(cx))
    }

    pub(crate) fn active_document_view(&self, cx: &App) -> Option<Entity<DocumentView>> {
        self.active_tab()
            .and_then(|tab| tab.read(cx).document_view())
    }

    pub(crate) fn active_document_state(&self, cx: &App) -> Option<(PathBuf, DocumentPosition)> {
        let document_view = self.active_document_view(cx)?;
        let document_view = document_view.read(cx);
        Some((
            document_view.path().to_path_buf(),
            document_view.current_document_position(),
        ))
    }

    pub(crate) fn document_view_at(&self, tab_ix: usize, cx: &App) -> Option<Entity<DocumentView>> {
        self.tabs
            .get(tab_ix)
            .and_then(|tab| tab.read(cx).document_view())
    }

    pub(crate) fn document_view_for_id(
        &self,
        tab_id: EntityId,
        cx: &App,
    ) -> Option<Entity<DocumentView>> {
        self.tab(tab_id)
            .and_then(|tab| tab.read(cx).document_view())
    }

    pub(crate) fn tab_is_document(&self, tab_id: EntityId, cx: &App) -> bool {
        self.tab(tab_id)
            .is_some_and(|tab| tab.read(cx).is_document())
    }

    pub(crate) fn document_views(&self, cx: &App) -> Vec<Entity<DocumentView>> {
        self.tabs
            .iter()
            .filter_map(|tab| tab.read(cx).document_view())
            .collect()
    }

    pub(crate) fn translation_sessions(&self, cx: &App) -> Vec<(DocumentTranslationId, bool)> {
        self.document_views(cx)
            .into_iter()
            .filter_map(|document_view| {
                let document_view = document_view.read(cx);
                document_view
                    .translation_session_id()
                    .map(|id| (id, document_view.translation_session_is_running()))
            })
            .collect()
    }

    /// The tab showing or opening `path`, preferring the displayed one.
    pub(crate) fn document_tab_ix(&self, path: &Path, cx: &App) -> Option<usize> {
        let path = normalize_path(path);
        let matches = |tab: &Entity<TabItem>| {
            tab.read(cx)
                .document_path(cx)
                .is_some_and(|candidate| candidate == path)
        };
        if self.active_tab().is_some_and(matches) {
            return Some(self.active_ix);
        }
        self.tabs.iter().position(matches)
    }

    /// Which copy of its document the tab at `tab_ix` is, and how many there
    /// are in this pane, when there is more than one.
    pub(crate) fn document_tab_occurrence(
        &self,
        tab_ix: usize,
        cx: &App,
    ) -> Option<(usize, usize)> {
        let document_view = self.document_view_at(tab_ix, cx)?;
        let path = document_view.read(cx).path().to_path_buf();
        let matching = self
            .tabs
            .iter()
            .enumerate()
            .filter_map(|(ix, tab)| {
                tab.read(cx)
                    .document_view()
                    .filter(|candidate| candidate.read(cx).path() == path)
                    .map(|_| ix)
            })
            .collect::<Vec<_>>();
        if matching.len() < 2 {
            return None;
        }
        let ordinal = matching.iter().position(|ix| *ix == tab_ix)? + 1;
        Some((ordinal, matching.len()))
    }

    /// The focus a pane hands out: its displayed tab's.
    pub(crate) fn focus_handle(&self, cx: &App) -> Option<FocusHandle> {
        self.active_tab()
            .map(|tab| gpui::Focusable::focus_handle(tab.read(cx), cx))
    }
}

/// The panes of the center, in layout order.
pub(crate) fn panes(area: &Entity<DockArea>, cx: &App) -> Vec<Pane> {
    let area = area.read(cx);
    let Some(tree) = area.layout(DockPlacement::Center) else {
        return Vec::new();
    };
    let mut panes = Vec::new();
    collect_panes(tree.root(), area, &mut panes);
    panes
}

fn collect_panes(node: &PaneNode, area: &DockArea, panes: &mut Vec<Pane>) {
    match node.kind() {
        PaneRef::Tabs { panels, active_ix } => panes.push(Pane {
            id: PaneId(node.id()),
            tabs: panels
                .iter()
                .filter_map(|panel| tab_item_for(area, *panel))
                .collect(),
            active_ix,
        }),
        PaneRef::Split { children, .. } => {
            for child in children {
                collect_panes(child, area, panes);
            }
        }
    }
}

pub(crate) fn tab_item_for(area: &DockArea, panel: PanelId) -> Option<Entity<TabItem>> {
    area.panel(panel)
        .and_then(|view| view.as_any().downcast_ref::<Entity<TabItem>>())
        .cloned()
}

pub(crate) fn panel_id(tab_id: EntityId) -> PanelId {
    PanelId::from(tab_id)
}

/// The visual extent of one tab, recorded while painting, for the
/// synchronized-tab connection lines.
#[derive(Clone, Copy, Default)]
pub(crate) struct PaneTabBounds {
    pub(crate) body: Option<Bounds<Pixels>>,
    pub(crate) suffix: Option<Bounds<Pixels>>,
}

impl PaneTabBounds {
    pub(crate) fn visual(self) -> Option<Bounds<Pixels>> {
        match (self.body, self.suffix) {
            (Some(body), Some(suffix)) => Some(body.union(&suffix)),
            (Some(body), None) => Some(body),
            (None, Some(suffix)) => Some(suffix),
            (None, None) => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SynchronizedConnectionInvalidReason {
    NonDocument,
    AlreadyInGroup { group_number: u64 },
    PaneAlreadyRepresented { group_number: u64 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SynchronizedConnectionTabState {
    Source,
    Member,
    Eligible,
    Invalid(SynchronizedConnectionInvalidReason),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RecentlyAddedSynchronizedTab {
    pub(crate) tab_id: EntityId,
    pub(crate) group_id: SynchronizedGroupId,
    pub(crate) sequence: u64,
}

/// How synchronized-tab state shows on the tabs, keyed by tab id.
#[derive(Clone, Default)]
pub(crate) struct SynchronizedPresentation {
    pub(crate) group_by_tab: HashMap<EntityId, SynchronizedGroupSummary>,
    pub(crate) paused_tabs: HashSet<EntityId>,
    pub(crate) pause_locked_tabs: HashSet<EntityId>,
    pub(crate) connection_tabs: HashMap<EntityId, SynchronizedConnectionTabState>,
    pub(crate) connection_color_slot: Option<usize>,
    pub(crate) recently_added: Option<RecentlyAddedSynchronizedTab>,
}

/// The drag preview for a tab.
pub(crate) struct TabDragPreview {
    pub(crate) label: SharedString,
}

impl Render for TabDragPreview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        ComponentTab::new()
            .label(self.label.clone())
            .selected(true)
            .small()
            .rounded(cx.theme().radius)
            .max_w(ui_rems(PANE_TAB_MAX_WIDTH))
            .min_w(ui_rems(PANE_TAB_MIN_WIDTH))
    }
}

/// The center's layout as plain data: what a session stores and what a
/// rearrangement the dock has no edit for rebuilds from.
#[derive(Clone)]
pub(crate) enum PaneLayout {
    Pane {
        tabs: Vec<Entity<TabItem>>,
        active_ix: usize,
    },
    Split {
        axis: Axis,
        /// Each child with its share of the split.
        children: Vec<(PaneLayout, f32)>,
    },
}

impl PaneLayout {
    /// Read the center as it is now, with each split's measured shares.
    pub(crate) fn of(area: &Entity<DockArea>, cx: &App) -> Option<Self> {
        let area = area.read(cx);
        let tree = area.layout(DockPlacement::Center)?;
        Self::of_node(tree.root(), area, cx)
    }

    fn of_node(node: &PaneNode, area: &DockArea, cx: &App) -> Option<Self> {
        match node.kind() {
            PaneRef::Tabs { panels, active_ix } => {
                let tabs = panels
                    .iter()
                    .filter_map(|panel| tab_item_for(area, *panel))
                    .collect::<Vec<_>>();
                (!tabs.is_empty()).then_some(Self::Pane {
                    active_ix: active_ix.min(tabs.len() - 1),
                    tabs,
                })
            }
            PaneRef::Split { axis, children, .. } => {
                let sizes = area.split_sizes(node.id(), cx).unwrap_or_default();
                let children = children
                    .iter()
                    .enumerate()
                    .filter_map(|(ix, child)| {
                        let share = sizes.get(ix).map_or(1.0, |size| size.as_f32().max(1.0));
                        Self::of_node(child, area, cx).map(|layout| (layout, share))
                    })
                    .collect::<Vec<_>>();
                match children.len() {
                    0 => None,
                    1 => children.into_iter().next().map(|(layout, _)| layout),
                    _ => Some(Self::Split {
                        axis,
                        children: normalize_shares(children),
                    }),
                }
            }
        }
    }

    /// The layout the dock installs. Shares become pixel sizes over a nominal
    /// total; the dock rescales them to the container.
    pub(crate) fn into_dock_layout(self, cx: &App) -> DockLayout {
        match self {
            Self::Pane { tabs, active_ix } => tabs
                .into_iter()
                .fold(DockLayout::tabs(), |layout, tab| {
                    layout.panel_view(Arc::new(tab) as Arc<dyn PanelView>, cx)
                })
                .active_index(active_ix),
            Self::Split { axis, children } => {
                let split = match axis {
                    Axis::Horizontal => DockLayout::h_split(),
                    Axis::Vertical => DockLayout::v_split(),
                };
                children.into_iter().fold(split, |split, (child, share)| {
                    split.child(
                        child.into_dock_layout(cx),
                        Some(px(share * RESTORED_SPLIT_TOTAL)),
                    )
                })
            }
        }
    }

    /// The tabs of every pane, in layout order.
    pub(crate) fn tabs(&self) -> Vec<Entity<TabItem>> {
        match self {
            Self::Pane { tabs, .. } => tabs.clone(),
            Self::Split { children, .. } => children
                .iter()
                .flat_map(|(child, _)| child.tabs())
                .collect(),
        }
    }

    fn contains(&self, tab_id: EntityId) -> bool {
        match self {
            Self::Pane { tabs, .. } => tabs.iter().any(|tab| tab.entity_id() == tab_id),
            Self::Split { children, .. } => {
                children.iter().any(|(child, _)| child.contains(tab_id))
            }
        }
    }

    /// Exchange the panes holding `first` and `second`.
    pub(crate) fn swap_panes(&mut self, first: EntityId, second: EntityId) -> bool {
        let (Some(a), Some(b)) = (
            self.find_pane(first).cloned(),
            self.find_pane(second).cloned(),
        ) else {
            return false;
        };
        if a.contains(second) {
            return false;
        }
        self.replace_panes(&|pane| {
            if pane.contains(first) {
                Some(b.clone())
            } else if pane.contains(second) {
                Some(a.clone())
            } else {
                None
            }
        });
        true
    }

    fn find_pane(&self, tab_id: EntityId) -> Option<&PaneLayout> {
        match self {
            Self::Pane { .. } => self.contains(tab_id).then_some(self),
            Self::Split { children, .. } => children
                .iter()
                .find_map(|(child, _)| child.find_pane(tab_id)),
        }
    }

    /// Replace each pane `replacement` answers for; every original pane is
    /// asked once.
    fn replace_panes(&mut self, replacement: &impl Fn(&PaneLayout) -> Option<PaneLayout>) {
        match self {
            Self::Pane { .. } => {
                if let Some(pane) = replacement(self) {
                    *self = pane;
                }
            }
            Self::Split { children, .. } => {
                for (child, _) in children {
                    child.replace_panes(replacement);
                }
            }
        }
    }

    /// Take the pane holding `tab_id` out of the layout and put it along the
    /// whole edge `direction` points at. Returns whether anything moved.
    pub(crate) fn move_pane_to_border(
        &mut self,
        tab_id: EntityId,
        direction: SplitDirection,
    ) -> bool {
        if matches!(self, Self::Pane { .. }) {
            return false;
        }
        if self
            .pane_at_border(direction)
            .is_some_and(|pane| pane.contains(tab_id))
        {
            return false;
        }
        let Some(pane) = self.remove_pane(tab_id) else {
            return false;
        };
        let rest = std::mem::replace(
            self,
            Self::Pane {
                tabs: Vec::new(),
                active_ix: 0,
            },
        );
        let share = 1.0 / (rest.child_count(direction.axis()) + 1) as f32;
        *self = match rest {
            Self::Split { axis, mut children } if axis == direction.axis() => {
                let rest_total = 1.0 - share;
                for (_, child_share) in &mut children {
                    *child_share *= rest_total;
                }
                if direction.increasing() {
                    children.push((pane, share));
                } else {
                    children.insert(0, (pane, share));
                }
                Self::Split { axis, children }
            }
            rest => {
                let children = if direction.increasing() {
                    vec![(rest, 0.5), (pane, 0.5)]
                } else {
                    vec![(pane, 0.5), (rest, 0.5)]
                };
                Self::Split {
                    axis: direction.axis(),
                    children,
                }
            }
        };
        true
    }

    fn child_count(&self, axis: Axis) -> usize {
        match self {
            Self::Split {
                axis: split_axis,
                children,
            } if *split_axis == axis => children.len(),
            _ => 1,
        }
    }

    fn pane_at_border(&self, direction: SplitDirection) -> Option<&PaneLayout> {
        match self {
            Self::Pane { .. } => Some(self),
            Self::Split { axis, children } => {
                if *axis != direction.axis() {
                    return None;
                }
                let child = if direction.increasing() {
                    children.last()
                } else {
                    children.first()
                };
                child.and_then(|(child, _)| child.pane_at_border(direction))
            }
        }
    }

    /// Remove the pane holding `tab_id`, collapsing splits left with one
    /// child.
    fn remove_pane(&mut self, tab_id: EntityId) -> Option<PaneLayout> {
        let Self::Split { children, .. } = self else {
            return None;
        };
        let removed = if let Some(ix) = children
            .iter()
            .position(|(child, _)| matches!(child, Self::Pane { .. } if child.contains(tab_id)))
        {
            Some(children.remove(ix).0)
        } else {
            children
                .iter_mut()
                .find_map(|(child, _)| child.remove_pane(tab_id))
        };
        if removed.is_some() {
            let taken = std::mem::take(children);
            let taken = normalize_shares(taken);
            *self = match taken.len() {
                1 => taken.into_iter().next().map(|(child, _)| child).unwrap(),
                _ => match self {
                    Self::Split { axis, .. } => Self::Split {
                        axis: *axis,
                        children: taken,
                    },
                    Self::Pane { .. } => unreachable!(),
                },
            };
        }
        removed
    }

    /// The session form, with tab ids numbered from `next_tab_id` and the
    /// runtime id of every tab recorded against its session id.
    pub(crate) fn session_tree(
        &self,
        active_tab: Option<EntityId>,
        next_pane_id: &mut u64,
        next_tab_id: &mut u64,
        active_pane_id: &mut u64,
        runtime_tab_ids: &mut HashMap<EntityId, u64>,
        cx: &App,
    ) -> ReaderPaneTree {
        match self {
            Self::Pane { tabs, active_ix } => {
                *next_pane_id = next_pane_id.saturating_add(1);
                let pane_id = *next_pane_id;
                if active_tab.is_some_and(|active| tabs.iter().any(|tab| tab.entity_id() == active))
                {
                    *active_pane_id = pane_id;
                }
                let mut session_tabs = Vec::with_capacity(tabs.len());
                let mut session_active = None;
                for (ix, tab) in tabs.iter().enumerate() {
                    *next_tab_id = next_tab_id.saturating_add(1);
                    let id = *next_tab_id;
                    runtime_tab_ids.insert(tab.entity_id(), id);
                    if ix == *active_ix {
                        session_active = Some(id);
                    }
                    session_tabs.push(ReaderTabSession {
                        id,
                        kind: tab.read(cx).session_kind(cx),
                    });
                }
                ReaderPaneTree::Pane(ReaderPaneSession {
                    id: pane_id,
                    tabs: session_tabs,
                    active_tab: session_active,
                })
            }
            Self::Split { axis, children } => ReaderPaneTree::Split {
                axis: match axis {
                    Axis::Horizontal => ReaderSplitAxis::Horizontal,
                    Axis::Vertical => ReaderSplitAxis::Vertical,
                },
                ratios: children.iter().map(|(_, share)| *share).collect(),
                children: children
                    .iter()
                    .map(|(child, _)| {
                        child.session_tree(
                            active_tab,
                            next_pane_id,
                            next_tab_id,
                            active_pane_id,
                            runtime_tab_ids,
                            cx,
                        )
                    })
                    .collect(),
            },
        }
    }
}

/// Shares that sum to one, falling back to equal shares.
fn normalize_shares(children: Vec<(PaneLayout, f32)>) -> Vec<(PaneLayout, f32)> {
    let total: f32 = children.iter().map(|(_, share)| *share).sum();
    let count = children.len().max(1) as f32;
    children
        .into_iter()
        .map(|(child, share)| {
            let share = if total.is_finite() && total > f32::EPSILON && share > 0.0 {
                share / total
            } else {
                1.0 / count
            };
            (child, share)
        })
        .collect()
}

#[cfg(test)]
mod tests;
