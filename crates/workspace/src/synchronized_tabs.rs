use std::collections::{HashMap, HashSet};

use glyphst_library::ReaderSynchronizedGroup;
use gpui::{EntityId, Pixels, Point};

use crate::theme::SYNCHRONIZED_GROUP_COLOR_COUNT;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct SynchronizedConnectionNode {
    pub(crate) tab_id: EntityId,
    pub(crate) center: Point<Pixels>,
}

fn squared_distance(left: Point<Pixels>, right: Point<Pixels>) -> f32 {
    let x = left.x.as_f32() - right.x.as_f32();
    let y = left.y.as_f32() - right.y.as_f32();
    x * x + y * y
}

fn connection_candidate_is_better(
    candidate: (f32, EntityId, EntityId),
    current: Option<(f32, EntityId, EntityId)>,
) -> bool {
    let Some(current) = current else {
        return true;
    };
    candidate
        .0
        .total_cmp(&current.0)
        .then_with(|| candidate.1.as_u64().cmp(&current.1.as_u64()))
        .then_with(|| candidate.2.as_u64().cmp(&current.2.as_u64()))
        .is_lt()
}

/// Builds a deterministic minimum spanning tree for the currently measurable tab headers.
pub(crate) fn synchronized_connection_tree(
    nodes: &[SynchronizedConnectionNode],
) -> Vec<(EntityId, EntityId)> {
    if nodes.len() < 2 {
        return Vec::new();
    }

    let mut nodes = nodes.to_vec();
    nodes.sort_by_key(|node| node.tab_id.as_u64());
    let mut connected = vec![false; nodes.len()];
    connected[0] = true;
    let mut edges = Vec::with_capacity(nodes.len() - 1);

    while edges.len() + 1 < nodes.len() {
        let mut best: Option<(f32, EntityId, EntityId, usize)> = None;
        for (_, from) in nodes.iter().enumerate().filter(|(ix, _)| connected[*ix]) {
            for (to_ix, to) in nodes.iter().enumerate().filter(|(ix, _)| !connected[*ix]) {
                let candidate = (
                    squared_distance(from.center, to.center),
                    from.tab_id,
                    to.tab_id,
                );
                if connection_candidate_is_better(
                    candidate,
                    best.map(|best| (best.0, best.1, best.2)),
                ) {
                    best = Some((candidate.0, candidate.1, candidate.2, to_ix));
                }
            }
        }

        let Some((_, from, to, to_ix)) = best else {
            break;
        };
        connected[to_ix] = true;
        edges.push((from, to));
    }

    edges
}

/// Chooses the closest existing node, using entity ids to make equal-distance ties stable.
pub(crate) fn closest_synchronized_tab(
    target: SynchronizedConnectionNode,
    candidates: &[SynchronizedConnectionNode],
) -> Option<EntityId> {
    let mut best: Option<(f32, EntityId, EntityId)> = None;
    for candidate in candidates {
        let next = (
            squared_distance(target.center, candidate.center),
            candidate.tab_id,
            target.tab_id,
        );
        if connection_candidate_is_better(next, best) {
            best = Some(next);
        }
    }
    best.map(|(_, tab_id, _)| tab_id)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct SynchronizedGroupId(u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ReservedSynchronizedGroup {
    pub(crate) id: SynchronizedGroupId,
    pub(crate) display_number: u64,
    pub(crate) color_slot: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SynchronizedGroupSummary {
    pub(crate) id: SynchronizedGroupId,
    pub(crate) display_number: u64,
    pub(crate) color_slot: usize,
    pub(crate) member_count: usize,
    pub(crate) is_current: bool,
}

#[derive(Clone, Debug)]
struct SynchronizedGroup {
    id: SynchronizedGroupId,
    display_number: u64,
    color_slot: usize,
    members: HashMap<EntityId, EntityId>,
}

#[derive(Default)]
pub(crate) struct SynchronizedTabRegistry {
    groups: HashMap<SynchronizedGroupId, SynchronizedGroup>,
    memberships: HashMap<EntityId, SynchronizedGroupId>,
    paused_tabs: HashSet<EntityId>,
    current_group: Option<SynchronizedGroupId>,
    next_group_id: u64,
}

impl SynchronizedTabRegistry {
    pub(crate) fn reader_session_groups(
        &self,
        runtime_tab_ids: &HashMap<EntityId, u64>,
    ) -> Vec<ReaderSynchronizedGroup> {
        let mut groups = self.groups.values().collect::<Vec<_>>();
        groups.sort_by_key(|group| group.display_number);
        groups
            .into_iter()
            .filter_map(|group| {
                let mut tabs = group
                    .members
                    .keys()
                    .filter_map(|tab_id| runtime_tab_ids.get(tab_id).copied())
                    .collect::<Vec<_>>();
                tabs.sort_unstable();
                if tabs.len() < 2 {
                    return None;
                }
                let mut paused_tabs = group
                    .members
                    .keys()
                    .filter(|tab_id| self.paused_tabs.contains(tab_id))
                    .filter_map(|tab_id| runtime_tab_ids.get(tab_id).copied())
                    .collect::<Vec<_>>();
                paused_tabs.sort_unstable();
                Some(ReaderSynchronizedGroup { tabs, paused_tabs })
            })
            .collect()
    }

    pub(crate) fn reserve_group(&mut self) -> ReservedSynchronizedGroup {
        self.next_group_id += 1;
        let id = SynchronizedGroupId(self.next_group_id);
        let used_numbers = self
            .groups
            .values()
            .map(|group| group.display_number)
            .collect::<HashSet<_>>();
        let display_number = (1_u64..)
            .find(|number| !used_numbers.contains(number))
            .expect("synchronized group display numbers are exhausted");
        let used_slots = self
            .groups
            .values()
            .map(|group| group.color_slot)
            .collect::<HashSet<_>>();
        let color_slot = (0..SYNCHRONIZED_GROUP_COLOR_COUNT)
            .find(|slot| !used_slots.contains(slot))
            .unwrap_or(
                (display_number.saturating_sub(1) as usize) % SYNCHRONIZED_GROUP_COLOR_COUNT,
            );
        ReservedSynchronizedGroup {
            id,
            display_number,
            color_slot,
        }
    }

    pub(crate) fn create_group(
        &mut self,
        source_tab: EntityId,
        source_pane: EntityId,
        target_tab: EntityId,
        target_pane: EntityId,
    ) -> Option<SynchronizedGroupId> {
        if source_tab == target_tab
            || source_pane == target_pane
            || self.memberships.contains_key(&source_tab)
            || self.memberships.contains_key(&target_tab)
        {
            return None;
        }
        let reserved = self.reserve_group();
        self.commit_reserved_group(reserved, source_tab, source_pane, target_tab, target_pane)
            .then_some(reserved.id)
    }

    pub(crate) fn connect_tab_to_source_group(
        &mut self,
        source_tab: EntityId,
        source_pane: EntityId,
        target_tab: EntityId,
        target_pane: EntityId,
    ) -> Option<SynchronizedGroupId> {
        if let Some(group_id) = self.group_for_tab(source_tab) {
            self.add_member(group_id, target_tab, target_pane)
                .then_some(group_id)
        } else {
            self.create_group(source_tab, source_pane, target_tab, target_pane)
        }
    }

    pub(crate) fn commit_reserved_group(
        &mut self,
        reserved: ReservedSynchronizedGroup,
        source_tab: EntityId,
        source_pane: EntityId,
        target_tab: EntityId,
        target_pane: EntityId,
    ) -> bool {
        if source_tab == target_tab
            || source_pane == target_pane
            || self.groups.contains_key(&reserved.id)
            || self.memberships.contains_key(&source_tab)
            || self.memberships.contains_key(&target_tab)
        {
            return false;
        }
        let mut members = HashMap::new();
        members.insert(source_tab, source_pane);
        members.insert(target_tab, target_pane);
        self.groups.insert(
            reserved.id,
            SynchronizedGroup {
                id: reserved.id,
                display_number: reserved.display_number,
                color_slot: reserved.color_slot,
                members,
            },
        );
        self.memberships.insert(source_tab, reserved.id);
        self.memberships.insert(target_tab, reserved.id);
        self.current_group = Some(reserved.id);
        true
    }

    pub(crate) fn add_member(
        &mut self,
        group_id: SynchronizedGroupId,
        tab_id: EntityId,
        pane_id: EntityId,
    ) -> bool {
        if self.memberships.contains_key(&tab_id) {
            return false;
        }
        let Some(group) = self.groups.get_mut(&group_id) else {
            return false;
        };
        if group
            .members
            .values()
            .any(|member_pane| *member_pane == pane_id)
        {
            return false;
        }
        group.members.insert(tab_id, pane_id);
        self.memberships.insert(tab_id, group_id);
        self.current_group = Some(group_id);
        true
    }

    pub(crate) fn can_add_member(
        &self,
        group_id: SynchronizedGroupId,
        tab_id: EntityId,
        pane_id: EntityId,
    ) -> bool {
        !self.memberships.contains_key(&tab_id)
            && self.groups.get(&group_id).is_some_and(|group| {
                !group
                    .members
                    .values()
                    .any(|member_pane| *member_pane == pane_id)
            })
    }

    pub(crate) fn can_move_tab(&self, tab_id: EntityId, destination_pane: EntityId) -> bool {
        self.memberships.get(&tab_id).is_none_or(|group_id| {
            self.groups.get(group_id).is_none_or(|group| {
                !group.members.iter().any(|(member_tab, member_pane)| {
                    *member_tab != tab_id && *member_pane == destination_pane
                })
            })
        })
    }

    pub(crate) fn move_tab(&mut self, tab_id: EntityId, destination_pane: EntityId) -> bool {
        if !self.can_move_tab(tab_id, destination_pane) {
            return false;
        }
        if let Some(group_id) = self.memberships.get(&tab_id)
            && let Some(group) = self.groups.get_mut(group_id)
            && let Some(pane_id) = group.members.get_mut(&tab_id)
        {
            *pane_id = destination_pane;
        }
        true
    }

    pub(crate) fn remove_tab(&mut self, tab_id: EntityId) {
        let membership = self.memberships.remove(&tab_id);
        self.paused_tabs.remove(&tab_id);
        if let Some(group_id) = membership {
            if let Some(group) = self.groups.get_mut(&group_id) {
                group.members.remove(&tab_id);
            }
            self.dissolve_if_too_small(group_id);
        }
        self.repair_current_group();
    }

    pub(crate) fn leave_group(&mut self, tab_id: EntityId) {
        let Some(group_id) = self.memberships.remove(&tab_id) else {
            return;
        };
        if let Some(group) = self.groups.get_mut(&group_id) {
            group.members.remove(&tab_id);
        }
        self.paused_tabs.remove(&tab_id);
        self.dissolve_if_too_small(group_id);
        self.repair_current_group();
    }

    pub(crate) fn select_for_tab(&mut self, tab_id: EntityId) -> Option<SynchronizedGroupId> {
        let group_id = self.group_for_tab(tab_id)?;
        self.current_group = Some(group_id);
        Some(group_id)
    }

    pub(crate) fn group_for_tab(&self, tab_id: EntityId) -> Option<SynchronizedGroupId> {
        self.memberships
            .get(&tab_id)
            .copied()
            .filter(|group_id| self.groups.contains_key(group_id))
    }

    #[cfg(test)]
    pub(crate) fn current_group(&self) -> Option<SynchronizedGroupId> {
        self.current_group
    }

    pub(crate) fn is_paused(&self, tab_id: EntityId) -> bool {
        self.paused_tabs.contains(&tab_id)
    }

    pub(crate) fn set_paused(&mut self, tab_id: EntityId, paused: bool) -> bool {
        if paused {
            if !self.memberships.contains_key(&tab_id) {
                return false;
            }
            self.paused_tabs.insert(tab_id)
        } else {
            self.paused_tabs.remove(&tab_id)
        }
    }

    pub(crate) fn contains(&self, group_id: SynchronizedGroupId, tab_id: EntityId) -> bool {
        self.groups
            .get(&group_id)
            .is_some_and(|group| group.members.contains_key(&tab_id))
    }

    pub(crate) fn member_tabs(&self, group_id: SynchronizedGroupId) -> Vec<EntityId> {
        self.groups
            .get(&group_id)
            .map(|group| group.members.keys().copied().collect())
            .unwrap_or_default()
    }

    pub(crate) fn group_summary(
        &self,
        group_id: SynchronizedGroupId,
    ) -> Option<SynchronizedGroupSummary> {
        let group = self.groups.get(&group_id)?;
        Some(SynchronizedGroupSummary {
            id: group.id,
            display_number: group.display_number,
            color_slot: group.color_slot,
            member_count: group.members.len(),
            is_current: self.current_group == Some(group.id),
        })
    }

    pub(crate) fn summary_for_tab(&self, tab_id: EntityId) -> Option<SynchronizedGroupSummary> {
        self.group_summary(self.group_for_tab(tab_id)?)
    }

    fn dissolve_if_too_small(&mut self, group_id: SynchronizedGroupId) {
        let should_dissolve = self
            .groups
            .get(&group_id)
            .is_some_and(|group| group.members.len() < 2);
        if !should_dissolve {
            return;
        }
        let Some(group) = self.groups.remove(&group_id) else {
            return;
        };
        for tab_id in group.members.keys() {
            if self.memberships.get(tab_id) == Some(&group_id) {
                self.memberships.remove(tab_id);
                self.paused_tabs.remove(tab_id);
            }
        }
    }

    fn repair_current_group(&mut self) {
        if self
            .current_group
            .is_some_and(|group_id| !self.groups.contains_key(&group_id))
        {
            self.current_group = self
                .groups
                .values()
                .min_by_key(|group| group.display_number)
                .map(|group| group.id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(tab: u64, x: f32, y: f32) -> SynchronizedConnectionNode {
        SynchronizedConnectionNode {
            tab_id: id(tab),
            center: Point::new(gpui::px(x), gpui::px(y)),
        }
    }

    fn id(value: u64) -> EntityId {
        EntityId::from(value)
    }

    #[test]
    fn rejects_overlapping_groups_without_changing_existing_memberships() {
        let mut registry = SynchronizedTabRegistry::default();
        let first = registry.create_group(id(1), id(11), id(2), id(12)).unwrap();
        let second = registry.create_group(id(3), id(13), id(4), id(14)).unwrap();

        assert!(
            registry
                .create_group(id(1), id(11), id(5), id(15))
                .is_none()
        );
        assert!(
            registry
                .create_group(id(5), id(15), id(2), id(12))
                .is_none()
        );
        assert!(!registry.can_add_member(first, id(3), id(16)));
        assert!(!registry.add_member(first, id(3), id(16)));
        let reserved = registry.reserve_group();
        assert!(!registry.commit_reserved_group(reserved, id(5), id(15), id(2), id(12)));

        assert_eq!(registry.group_for_tab(id(1)), Some(first));
        assert_eq!(registry.group_for_tab(id(2)), Some(first));
        assert_eq!(registry.group_for_tab(id(3)), Some(second));
        assert_eq!(registry.group_for_tab(id(4)), Some(second));
        assert_eq!(registry.group_for_tab(id(5)), None);
        assert!(registry.group_summary(reserved.id).is_none());
        assert_eq!(registry.group_summary(first).unwrap().member_count, 2);
        assert_eq!(registry.group_summary(second).unwrap().member_count, 2);

        assert_eq!(registry.select_for_tab(id(1)), Some(first));
        assert_eq!(registry.current_group(), Some(first));
        assert_eq!(registry.summary_for_tab(id(1)).unwrap().id, first);
    }

    #[test]
    fn connects_a_target_to_the_sources_existing_group_or_creates_one() {
        let mut registry = SynchronizedTabRegistry::default();
        let first = registry
            .connect_tab_to_source_group(id(1), id(11), id(2), id(12))
            .unwrap();

        assert_eq!(
            registry.connect_tab_to_source_group(id(1), id(11), id(3), id(13)),
            Some(first)
        );
        assert_eq!(registry.group_summary(first).unwrap().member_count, 3);
        assert_eq!(registry.group_for_tab(id(3)), Some(first));

        let second = registry
            .connect_tab_to_source_group(id(4), id(14), id(5), id(15))
            .unwrap();
        assert_ne!(second, first);
        assert_eq!(
            registry.connect_tab_to_source_group(id(1), id(11), id(4), id(14)),
            None
        );
        assert_eq!(registry.group_for_tab(id(4)), Some(second));
    }

    #[test]
    fn display_numbers_reuse_gaps_without_reusing_internal_ids() {
        let mut registry = SynchronizedTabRegistry::default();
        let first = registry.create_group(id(1), id(11), id(2), id(12)).unwrap();
        let second = registry.create_group(id(3), id(13), id(4), id(14)).unwrap();
        let third = registry.create_group(id(5), id(15), id(6), id(16)).unwrap();

        assert_eq!(registry.group_summary(first).unwrap().display_number, 1);
        assert_eq!(registry.group_summary(second).unwrap().display_number, 2);
        assert_eq!(registry.group_summary(third).unwrap().display_number, 3);

        registry.leave_group(id(3));
        assert_eq!(registry.group_summary(third).unwrap().display_number, 3);
        registry.leave_group(id(5));
        assert_eq!(registry.current_group(), Some(first));
        let fourth = registry.create_group(id(7), id(17), id(8), id(18)).unwrap();
        let fifth = registry
            .create_group(id(9), id(19), id(10), id(20))
            .unwrap();

        assert_eq!(registry.group_summary(first).unwrap().display_number, 1);
        assert_eq!(registry.group_summary(fourth).unwrap().display_number, 2);
        assert_eq!(registry.group_summary(fifth).unwrap().display_number, 3);
        assert_eq!(fourth.0, 4);
        assert_eq!(fifth.0, 5);
    }

    #[test]
    fn color_slots_follow_creation_order_and_reuse_gaps() {
        let mut registry = SynchronizedTabRegistry::default();
        let groups = (0..SYNCHRONIZED_GROUP_COLOR_COUNT)
            .map(|index| {
                let tab = index as u64 * 2 + 1;
                let pane = index as u64 * 2 + 101;
                registry
                    .create_group(id(tab), id(pane), id(tab + 1), id(pane + 1))
                    .unwrap()
            })
            .collect::<Vec<_>>();

        assert_eq!(
            groups
                .iter()
                .map(|group| registry.group_summary(*group).unwrap().color_slot)
                .collect::<Vec<_>>(),
            (0..SYNCHRONIZED_GROUP_COLOR_COUNT).collect::<Vec<_>>()
        );

        registry.leave_group(id(1));
        let replacement = registry
            .create_group(id(1001), id(1101), id(1002), id(1102))
            .unwrap();
        assert_eq!(registry.group_summary(replacement).unwrap().color_slot, 0);
    }

    #[test]
    fn enforces_one_member_of_a_group_per_pane() {
        let mut registry = SynchronizedTabRegistry::default();
        let group = registry.create_group(id(1), id(11), id(2), id(12)).unwrap();

        assert!(!registry.can_add_member(group, id(3), id(12)));
        assert!(!registry.add_member(group, id(3), id(12)));
        assert!(registry.add_member(group, id(3), id(13)));
    }

    #[test]
    fn removing_a_tab_dissolves_groups_smaller_than_two() {
        let mut registry = SynchronizedTabRegistry::default();
        let group = registry.create_group(id(1), id(11), id(2), id(12)).unwrap();
        registry.remove_tab(id(2));

        assert!(registry.group_summary(group).is_none());
        assert!(registry.summary_for_tab(id(1)).is_none());
        assert_eq!(registry.current_group(), None);
    }

    #[test]
    fn paused_state_clears_when_membership_is_removed_or_dissolved() {
        let mut registry = SynchronizedTabRegistry::default();
        registry.create_group(id(1), id(11), id(2), id(12)).unwrap();

        assert!(!registry.set_paused(id(99), true));
        assert!(registry.set_paused(id(1), true));
        assert!(registry.is_paused(id(1)));

        registry.leave_group(id(2));
        assert!(!registry.is_paused(id(1)));
        assert!(!registry.set_paused(id(1), true));

        registry.create_group(id(1), id(11), id(3), id(13)).unwrap();
        assert!(registry.set_paused(id(1), true));
        registry.remove_tab(id(1));
        assert!(!registry.is_paused(id(1)));
    }

    #[test]
    fn rejects_moves_into_a_pane_already_represented_by_the_tabs_group() {
        let mut registry = SynchronizedTabRegistry::default();
        let first = registry.create_group(id(1), id(11), id(2), id(12)).unwrap();
        let second = registry.create_group(id(3), id(13), id(4), id(14)).unwrap();

        assert!(!registry.can_move_tab(id(1), id(12)));
        assert!(registry.can_move_tab(id(1), id(13)));
        assert!(registry.move_tab(id(1), id(13)));
        assert!(!registry.can_add_member(first, id(5), id(13)));
        assert!(registry.can_add_member(second, id(5), id(15)));
    }

    #[test]
    fn closest_tree_is_deterministic_and_uses_short_edges() {
        let nodes = [
            node(4, 100.0, 0.0),
            node(2, 10.0, 0.0),
            node(1, 0.0, 0.0),
            node(3, 20.0, 0.0),
        ];

        assert_eq!(
            synchronized_connection_tree(&nodes),
            vec![(id(1), id(2)), (id(2), id(3)), (id(3), id(4))]
        );
    }

    #[test]
    fn closest_tab_breaks_equal_distance_ties_by_entity_id() {
        let target = node(10, 10.0, 0.0);
        let candidates = [node(3, 20.0, 0.0), node(2, 0.0, 0.0)];

        assert_eq!(closest_synchronized_tab(target, &candidates), Some(id(2)));
    }

    #[test]
    fn adding_a_node_keeps_existing_tree_edges_stable() {
        let existing = [node(1, 0.0, 0.0), node(2, 40.0, 0.0)];
        let mut edges = synchronized_connection_tree(&existing);
        let original_edges = edges.clone();
        let target = node(3, 45.0, 20.0);

        let nearest = closest_synchronized_tab(target, &existing).unwrap();
        edges.push((nearest, target.tab_id));

        assert_eq!(original_edges, vec![(id(1), id(2))]);
        assert_eq!(edges, vec![(id(1), id(2)), (id(2), id(3))]);
    }
}
