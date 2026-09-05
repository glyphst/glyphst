//! Synchronized-tab connections, viewport propagation, and their presentation.

use super::*;

impl Workspace {
    pub(super) fn synchronized_connection_color_slot(
        &self,
        group: SynchronizedConnectionGroup,
    ) -> usize {
        match group {
            SynchronizedConnectionGroup::New(reserved) => reserved.color_slot,
            SynchronizedConnectionGroup::Existing(group_id) => self
                .synchronized_tabs
                .group_summary(group_id)
                .map(|group| group.color_slot)
                .unwrap_or_default(),
        }
    }

    pub(super) fn synchronized_connection_display_number(
        &self,
        group: SynchronizedConnectionGroup,
    ) -> u64 {
        match group {
            SynchronizedConnectionGroup::New(reserved) => reserved.display_number,
            SynchronizedConnectionGroup::Existing(group_id) => self
                .synchronized_tabs
                .group_summary(group_id)
                .map(|group| group.display_number)
                .unwrap_or_default(),
        }
    }

    pub(super) fn synchronized_connection_tab_state(
        &self,
        connection: &SynchronizedConnection,
        pane_id: EntityId,
        tab_id: EntityId,
        is_document: bool,
    ) -> SynchronizedConnectionTabState {
        if tab_id == connection.source_tab {
            return SynchronizedConnectionTabState::Source;
        }

        if let SynchronizedConnectionGroup::Existing(group_id) = connection.group
            && self.synchronized_tabs.contains(group_id, tab_id)
        {
            return SynchronizedConnectionTabState::Member;
        }

        if !is_document {
            return SynchronizedConnectionTabState::Invalid(
                SynchronizedConnectionInvalidReason::NonDocument,
            );
        }

        if let Some(group_id) = self.synchronized_tabs.group_for_tab(tab_id) {
            let group_number = self
                .synchronized_tabs
                .group_summary(group_id)
                .map_or(0, |group| group.display_number);
            return SynchronizedConnectionTabState::Invalid(
                SynchronizedConnectionInvalidReason::AlreadyInGroup { group_number },
            );
        }

        let group_number = self.synchronized_connection_display_number(connection.group);
        let pane_is_represented = match connection.group {
            SynchronizedConnectionGroup::New(_) => pane_id == connection.source_pane.entity_id(),
            SynchronizedConnectionGroup::Existing(group_id) => !self
                .synchronized_tabs
                .can_add_member(group_id, tab_id, pane_id),
        };
        if pane_is_represented {
            SynchronizedConnectionTabState::Invalid(
                SynchronizedConnectionInvalidReason::PaneAlreadyRepresented { group_number },
            )
        } else {
            SynchronizedConnectionTabState::Eligible
        }
    }

    pub(super) fn synchronized_connection_tab_bounds(
        &self,
        tab_id: EntityId,
        cx: &Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        for pane in self.pane_group.panes() {
            if !pane.read(cx).has_tab_id(tab_id) {
                continue;
            }
            let tab_bounds = pane.read(cx).tab_bounds(tab_id)?;
            let center = tab_bounds.center();
            if self
                .pane_group
                .bounding_box_for_pane(&pane)
                .is_some_and(|pane_bounds| !pane_bounds.contains(&center))
            {
                return None;
            }
            return Some(tab_bounds);
        }
        None
    }

    pub(super) fn synchronized_connection_node(
        &self,
        tab_id: EntityId,
        cx: &Context<Self>,
    ) -> Option<SynchronizedConnectionNode> {
        let tab_bounds = self.synchronized_connection_tab_bounds(tab_id, cx)?;
        Some(SynchronizedConnectionNode {
            tab_id,
            center: tab_bounds.center(),
        })
    }

    pub(super) fn synchronized_group_connection_nodes(
        &self,
        group_id: SynchronizedGroupId,
        cx: &Context<Self>,
    ) -> Vec<SynchronizedConnectionNode> {
        self.synchronized_tabs
            .member_tabs(group_id)
            .into_iter()
            .filter_map(|tab_id| self.synchronized_connection_node(tab_id, cx))
            .collect()
    }

    pub(super) fn initial_synchronized_connection_edges(
        &self,
        group_id: SynchronizedGroupId,
        added_at: Instant,
        cx: &Context<Self>,
    ) -> Vec<SynchronizedConnectionEdge> {
        synchronized_connection_tree(&self.synchronized_group_connection_nodes(group_id, cx))
            .into_iter()
            .map(|(from, to)| SynchronizedConnectionEdge { from, to, added_at })
            .collect()
    }

    pub(super) fn closest_synchronized_connection_edge(
        &self,
        group_id: SynchronizedGroupId,
        target_tab: EntityId,
        added_at: Instant,
        cx: &Context<Self>,
    ) -> Option<SynchronizedConnectionEdge> {
        let target = self.synchronized_connection_node(target_tab, cx)?;
        let candidates = self
            .synchronized_group_connection_nodes(group_id, cx)
            .into_iter()
            .filter(|candidate| candidate.tab_id != target_tab)
            .collect::<Vec<_>>();
        let from = closest_synchronized_tab(target, &candidates)?;
        Some(SynchronizedConnectionEdge {
            from,
            to: target_tab,
            added_at,
        })
    }

    pub(super) fn repair_synchronized_connection_edges(&mut self, cx: &Context<Self>) {
        let Some(connection) = self.synchronized_connection.as_ref() else {
            return;
        };
        let SynchronizedConnectionGroup::Existing(group_id) = connection.group else {
            return;
        };
        let members = self.synchronized_tabs.member_tabs(group_id);
        let needs_repair = connection
            .edges
            .iter()
            .any(|edge| !members.contains(&edge.from) || !members.contains(&edge.to));
        if !needs_repair {
            return;
        }

        let now = Instant::now();
        let repaired = self.initial_synchronized_connection_edges(group_id, now, cx);
        if let Some(connection) = self.synchronized_connection.as_mut() {
            connection.edges = repaired;
            connection.animation_started_at = now;
        }
    }

    pub(super) fn refresh_synchronized_tab_presentation(&mut self, cx: &mut Context<Self>) {
        let invalid_connection = self
            .synchronized_connection
            .as_ref()
            .is_some_and(|connection| {
                !connection
                    .source_pane
                    .read(cx)
                    .has_tab_id(connection.source_tab)
                    || matches!(
                        connection.group,
                        SynchronizedConnectionGroup::Existing(group_id)
                            if self.synchronized_tabs.group_summary(group_id).is_none()
                    )
            });
        if invalid_connection {
            self.synchronized_connection = None;
        }
        self.repair_synchronized_connection_edges(cx);
        let pause_locked_tabs = self
            .translations
            .live_pairs
            .values()
            .flat_map(|pair| [pair.source_id, pair.translated_id])
            .collect::<HashSet<_>>();
        for pane in self.pane_group.panes() {
            let pane_id = pane.entity_id();
            let tab_ids = pane.read(cx).tab_ids();
            let mut presentation = PaneSynchronizedPresentation::default();
            if let Some(connection) = &self.synchronized_connection {
                presentation.connection_color_slot =
                    Some(self.synchronized_connection_color_slot(connection.group));
                presentation.recently_added = connection.recently_added;
            }
            for tab_id in tab_ids {
                if let Some(group) = self.synchronized_tabs.summary_for_tab(tab_id) {
                    presentation.group_by_tab.insert(tab_id, group);
                }
                if self.synchronized_tabs.is_paused(tab_id) {
                    presentation.paused_tabs.insert(tab_id);
                }
                if pause_locked_tabs.contains(&tab_id) {
                    presentation.pause_locked_tabs.insert(tab_id);
                }
                if let Some(connection) = &self.synchronized_connection {
                    let state = self.synchronized_connection_tab_state(
                        connection,
                        pane_id,
                        tab_id,
                        pane.read(cx).tab_is_document(tab_id),
                    );
                    presentation.connection_tabs.insert(tab_id, state);
                }
            }
            pane.update(cx, |pane, cx| {
                pane.set_synchronized_presentation(presentation, cx);
            });
        }
        cx.notify();
    }

    pub(super) fn start_new_synchronized_group(
        &mut self,
        source_pane: Entity<Pane>,
        source_tab: gpui::EntityId,
        cx: &mut Context<Self>,
    ) {
        if !source_pane.read(cx).tab_is_document(source_tab)
            || self.synchronized_tabs.group_for_tab(source_tab).is_some()
        {
            return;
        }
        let reserved = self.synchronized_tabs.reserve_group();
        let cursor = source_pane
            .read(cx)
            .tab_bounds(source_tab)
            .map(|bounds| bounds.center())
            .unwrap_or_else(|| point(px(0.0), px(0.0)));
        let now = Instant::now();
        self.fading_synchronized_connection = None;
        self.synchronized_connection = Some(SynchronizedConnection {
            source_tab,
            source_pane,
            group: SynchronizedConnectionGroup::New(reserved),
            cursor,
            animation_started_at: now,
            edges: Vec::new(),
            recently_added: None,
            addition_sequence: 0,
        });
        self.refresh_synchronized_tab_presentation(cx);
    }

    pub(super) fn start_connecting_synchronized_group(
        &mut self,
        source_pane: Entity<Pane>,
        source_tab: gpui::EntityId,
        group_id: SynchronizedGroupId,
        cx: &mut Context<Self>,
    ) {
        if self.synchronized_tabs.group_for_tab(source_tab) != Some(group_id) {
            return;
        }
        self.synchronized_tabs.select_for_tab(source_tab);
        let cursor = source_pane
            .read(cx)
            .tab_bounds(source_tab)
            .map(|bounds| bounds.center())
            .unwrap_or_else(|| point(px(0.0), px(0.0)));
        let now = Instant::now();
        let edges = self.initial_synchronized_connection_edges(group_id, now, cx);
        self.fading_synchronized_connection = None;
        self.synchronized_connection = Some(SynchronizedConnection {
            source_tab,
            source_pane,
            group: SynchronizedConnectionGroup::Existing(group_id),
            cursor,
            animation_started_at: now,
            edges,
            recently_added: None,
            addition_sequence: 0,
        });
        self.refresh_synchronized_tab_presentation(cx);
    }

    pub(super) fn finish_synchronized_connection(&mut self, cx: &mut Context<Self>) {
        let Some(connection) = self.synchronized_connection.take() else {
            return;
        };
        if !cx.reduce_motion() && !connection.edges.is_empty() {
            self.fading_synchronized_connection = Some(FadingSynchronizedConnection {
                color_slot: self.synchronized_connection_color_slot(connection.group),
                edges: connection.edges,
                animation_started_at: connection.animation_started_at,
                started_at: Instant::now(),
            });
        } else {
            self.fading_synchronized_connection = None;
        }
        self.refresh_synchronized_tab_presentation(cx);
    }

    pub(super) fn handle_synchronized_tab_click(
        &mut self,
        pane: &Entity<Pane>,
        tab_id: gpui::EntityId,
        was_active: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(connection) = self.synchronized_connection.as_ref() {
            let source_tab = connection.source_tab;
            let source_pane = connection.source_pane.clone();
            let connection_group = connection.group;
            let target_pane_id = pane.entity_id();
            if tab_id == source_tab || !pane.read(cx).tab_is_document(tab_id) {
                return;
            }
            let now = Instant::now();
            let (added_group, first_edge) = match connection_group {
                SynchronizedConnectionGroup::New(reserved) => {
                    if target_pane_id == source_pane.entity_id()
                        || !self.synchronized_tabs.commit_reserved_group(
                            reserved,
                            source_tab,
                            source_pane.entity_id(),
                            tab_id,
                            target_pane_id,
                        )
                    {
                        return;
                    }
                    (
                        Some(reserved.id),
                        Some(SynchronizedConnectionEdge {
                            from: source_tab,
                            to: tab_id,
                            added_at: now,
                        }),
                    )
                }
                SynchronizedConnectionGroup::Existing(group_id) => (
                    self.synchronized_tabs
                        .add_member(group_id, tab_id, target_pane_id)
                        .then_some(group_id),
                    None,
                ),
            };
            if let Some(group_id) = added_group {
                let edge = first_edge.or_else(|| {
                    self.closest_synchronized_connection_edge(group_id, tab_id, now, cx)
                });
                if let Some(connection) = self.synchronized_connection.as_mut() {
                    connection.group = SynchronizedConnectionGroup::Existing(group_id);
                    if let Some(edge) = edge
                        && !connection.edges.iter().any(|candidate| {
                            (candidate.from == edge.from && candidate.to == edge.to)
                                || (candidate.from == edge.to && candidate.to == edge.from)
                        })
                    {
                        connection.edges.push(edge);
                    }
                    connection.addition_sequence += 1;
                    connection.recently_added = Some(RecentlyAddedSynchronizedTab {
                        tab_id,
                        group_id,
                        sequence: connection.addition_sequence,
                    });
                }
                self.refresh_synchronized_tab_presentation(cx);
            }
            return;
        }

        let Some(group_id) = self.synchronized_tabs.select_for_tab(tab_id) else {
            return;
        };
        if was_active {
            self.activate_synchronized_group(group_id, pane, window, cx);
        }
        self.refresh_synchronized_tab_presentation(cx);
    }

    pub(super) fn activate_synchronized_group(
        &mut self,
        group_id: SynchronizedGroupId,
        initiating_pane: &Entity<Pane>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let members = self.synchronized_tabs.member_tabs(group_id);
        for pane in self.pane_group.panes() {
            let member = members
                .iter()
                .find(|tab_id| pane.read(cx).has_tab_id(**tab_id))
                .copied();
            if let Some(tab_id) = member {
                pane.update(cx, |pane, cx| {
                    pane.activate_tab_by_id(tab_id, false, window, cx);
                });
            }
        }
        self.active_pane = initiating_pane.clone();
        initiating_pane.update(cx, |pane, cx| pane.focus(window, cx));
        cx.notify();
    }

    pub(super) fn live_translation_pair_for_tab(
        &self,
        tab_id: EntityId,
    ) -> Option<(DocumentTranslationId, EntityId, EntityId)> {
        self.translations
            .live_pairs
            .iter()
            .find_map(|(session_id, pair)| {
                (pair.source_id == tab_id || pair.translated_id == tab_id).then_some((
                    *session_id,
                    pair.source_id,
                    pair.translated_id,
                ))
            })
    }

    pub(super) fn live_translation_counterpart(&self, tab_id: EntityId) -> Option<EntityId> {
        self.live_translation_pair_for_tab(tab_id)
            .map(|(_, source_id, translated_id)| {
                if source_id == tab_id {
                    translated_id
                } else {
                    source_id
                }
            })
    }

    pub(super) fn is_live_translation_pair_tab(&self, tab_id: EntityId) -> bool {
        self.live_translation_counterpart(tab_id).is_some()
    }

    pub(super) fn tab_is_displayed(&self, tab_id: EntityId, cx: &App) -> bool {
        self.pane_group
            .panes()
            .into_iter()
            .any(|pane| pane.read(cx).active_tab_id() == Some(tab_id))
    }

    pub(super) fn document_view_for_tab(
        &self,
        tab_id: EntityId,
        cx: &App,
    ) -> Option<Entity<DocumentView>> {
        self.pane_group
            .panes()
            .into_iter()
            .find_map(|pane| pane.read(cx).document_view_for_id(tab_id))
    }

    pub(super) fn synchronize_live_translation_viewport_anchor(
        &self,
        session_id: DocumentTranslationId,
        changed_tab: EntityId,
        anchor: TranslationViewportAnchor,
        cx: &mut Context<Self>,
    ) {
        let Some((pair_session_id, source_id, translated_id)) =
            self.live_translation_pair_for_tab(changed_tab)
        else {
            return;
        };
        if pair_session_id != session_id {
            return;
        }
        let counterpart = if changed_tab == source_id {
            translated_id
        } else {
            source_id
        };
        let Some(changed_view) = self.document_view_for_tab(changed_tab, cx) else {
            return;
        };
        let Some(counterpart_view) = self.document_view_for_tab(counterpart, cx) else {
            return;
        };
        counterpart_view.update(cx, |view, cx| {
            view.apply_live_translation_viewport_anchor(session_id, anchor, cx);
        });
        let Some(applied) = counterpart_view
            .read(cx)
            .live_translation_viewport_anchor(anchor.mode)
        else {
            return;
        };
        let applied_exactly = applied.page_index == anchor.page_index
            && (applied.pdf_x - anchor.pdf_x).abs() < 0.01
            && (applied.pdf_y - anchor.pdf_y).abs() < 0.01
            && (applied.viewport_y_fraction - anchor.viewport_y_fraction).abs() < 0.001;
        if !applied_exactly {
            // Near a document edge, the counterpart may be unable to center
            // the requested point at its independent zoom. Move the initiating
            // view to the closest point both viewports can represent.
            changed_view.update(cx, |view, cx| {
                view.apply_live_translation_viewport_anchor(session_id, applied, cx);
            });
        }
    }

    pub(super) fn align_live_translation_counterpart(
        &self,
        changed_tab: EntityId,
        mode: crate::document_view::SynchronizedScrollMode,
        cx: &mut Context<Self>,
    ) {
        let Some((session_id, _, _)) = self.live_translation_pair_for_tab(changed_tab) else {
            return;
        };
        let Some(changed_view) = self.document_view_for_tab(changed_tab, cx) else {
            return;
        };
        let Some(anchor) = changed_view.read(cx).live_translation_viewport_anchor(mode) else {
            return;
        };
        self.synchronize_live_translation_viewport_anchor(session_id, changed_tab, anchor, cx);
    }

    pub(super) fn propagate_synchronized_scroll(
        &mut self,
        source_tab: gpui::EntityId,
        delta: crate::document_view::SynchronizedScrollDelta,
        cx: &mut Context<Self>,
    ) {
        let Some(group_id) = self.synchronized_tabs.group_for_tab(source_tab) else {
            return;
        };
        if self.synchronized_tabs.is_paused(source_tab) {
            return;
        }
        if !self.tab_is_displayed(source_tab, cx) {
            return;
        }

        let translation_counterpart = self.live_translation_counterpart(source_tab);
        if translation_counterpart.is_some() {
            self.align_live_translation_counterpart(source_tab, delta.mode, cx);
        }
        let mut aligned_translation_tabs = HashSet::new();
        for member_tab in self.synchronized_tabs.member_tabs(group_id) {
            if member_tab == source_tab
                || Some(member_tab) == translation_counterpart
                || aligned_translation_tabs.contains(&member_tab)
                || self.synchronized_tabs.is_paused(member_tab)
            {
                continue;
            }
            let translation_pair = self.live_translation_pair_for_tab(member_tab);
            if let Some((_, pair_source, pair_translated)) = translation_pair {
                let preferred = if self.tab_is_displayed(pair_source, cx) {
                    pair_source
                } else {
                    pair_translated
                };
                if member_tab != preferred {
                    continue;
                }
            }
            let Some(pane) = self
                .pane_group
                .panes()
                .into_iter()
                .find(|pane| pane.read(cx).has_tab_id(member_tab))
            else {
                continue;
            };
            if pane.read(cx).active_tab_id() != Some(member_tab) {
                continue;
            }
            if let Some(document_view) = pane.read(cx).document_view_for_id(member_tab) {
                document_view.update(cx, |document_view, cx| {
                    document_view.apply_synchronized_scroll_delta(delta, cx);
                });
                if let Some((_, pair_source, pair_translated)) = translation_pair {
                    aligned_translation_tabs.insert(pair_source);
                    aligned_translation_tabs.insert(pair_translated);
                    self.align_live_translation_counterpart(member_tab, delta.mode, cx);
                }
            }
        }
    }

    pub(super) fn synchronized_connection_anchor(
        tab_bounds: Bounds<Pixels>,
        toward: Point<Pixels>,
    ) -> SynchronizedConnectionAnchor {
        let center = tab_bounds.center();
        let dx = toward.x.as_f32() - center.x.as_f32();
        let dy = toward.y.as_f32() - center.y.as_f32();
        if dx.abs() >= dy.abs() {
            let (x, edge) = if dx >= 0.0 {
                (tab_bounds.right(), SynchronizedConnectionAnchorEdge::Right)
            } else {
                (tab_bounds.left(), SynchronizedConnectionAnchorEdge::Left)
            };
            SynchronizedConnectionAnchor {
                position: point(x, center.y),
                edge,
            }
        } else {
            let (y, edge) = if dy >= 0.0 {
                (
                    tab_bounds.bottom(),
                    SynchronizedConnectionAnchorEdge::Bottom,
                )
            } else {
                (tab_bounds.top(), SynchronizedConnectionAnchorEdge::Top)
            };
            SynchronizedConnectionAnchor {
                position: point(center.x, y),
                edge,
            }
        }
    }

    pub(super) fn synchronized_connection_control(
        anchor: SynchronizedConnectionAnchor,
        distance: f32,
    ) -> Point<Pixels> {
        match anchor.edge {
            SynchronizedConnectionAnchorEdge::Top => {
                point(anchor.position.x, anchor.position.y - px(distance))
            }
            SynchronizedConnectionAnchorEdge::Right => {
                point(anchor.position.x + px(distance), anchor.position.y)
            }
            SynchronizedConnectionAnchorEdge::Bottom => {
                point(anchor.position.x, anchor.position.y + px(distance))
            }
            SynchronizedConnectionAnchorEdge::Left => {
                point(anchor.position.x - px(distance), anchor.position.y)
            }
        }
    }

    pub(super) fn synchronized_connection_handles(
        curves: &[SynchronizedConnectionCurve],
    ) -> Vec<SynchronizedConnectionHandle> {
        let mut handles: Vec<SynchronizedConnectionHandle> = Vec::with_capacity(curves.len() * 2);
        for curve in curves {
            for center in [curve.start.position, curve.end.position] {
                if let Some(existing) = handles
                    .iter_mut()
                    .find(|handle| Self::point_distance(handle.center, center) < 0.5)
                {
                    if curve.added_at < existing.added_at {
                        existing.added_at = curve.added_at;
                    }
                } else {
                    handles.push(SynchronizedConnectionHandle {
                        center,
                        added_at: curve.added_at,
                    });
                }
            }
        }
        handles
    }

    pub(super) fn synchronized_connection_curve(
        from: Bounds<Pixels>,
        to: Bounds<Pixels>,
        added_at: Instant,
    ) -> SynchronizedConnectionCurve {
        let start = Self::synchronized_connection_anchor(from, to.center());
        let end = Self::synchronized_connection_anchor(to, from.center());
        let dx = end.position.x.as_f32() - start.position.x.as_f32();
        let dy = end.position.y.as_f32() - start.position.y.as_f32();
        let bend = if matches!(
            start.edge,
            SynchronizedConnectionAnchorEdge::Left | SynchronizedConnectionAnchorEdge::Right
        ) {
            (dx.abs() * 0.35).clamp(18.0, 110.0)
        } else {
            (dy.abs() * 0.35).clamp(18.0, 110.0)
        };
        let control_a = Self::synchronized_connection_control(start, bend);
        let control_b = Self::synchronized_connection_control(end, bend);
        SynchronizedConnectionCurve {
            start,
            control_a,
            control_b,
            end,
            added_at,
        }
    }

    pub(super) fn cubic_point(curve: SynchronizedConnectionCurve, t: f32) -> Point<Pixels> {
        let inverse = 1.0 - t;
        let a = inverse * inverse * inverse;
        let b = 3.0 * inverse * inverse * t;
        let c = 3.0 * inverse * t * t;
        let d = t * t * t;
        point(
            px(curve.start.position.x.as_f32() * a
                + curve.control_a.x.as_f32() * b
                + curve.control_b.x.as_f32() * c
                + curve.end.position.x.as_f32() * d),
            px(curve.start.position.y.as_f32() * a
                + curve.control_a.y.as_f32() * b
                + curve.control_b.y.as_f32() * c
                + curve.end.position.y.as_f32() * d),
        )
    }

    pub(super) fn synchronized_connection_dot_parameter(index: usize, dot_count: usize) -> f32 {
        debug_assert!(index < dot_count);
        (index + 1) as f32 / (dot_count + 1) as f32
    }

    pub(super) fn point_distance(left: Point<Pixels>, right: Point<Pixels>) -> f32 {
        let dx = left.x.as_f32() - right.x.as_f32();
        let dy = left.y.as_f32() - right.y.as_f32();
        (dx * dx + dy * dy).sqrt()
    }

    pub(super) fn synchronized_connection_curves(
        &self,
        edges: &[SynchronizedConnectionEdge],
        viewport: Bounds<Pixels>,
        cx: &Context<Self>,
    ) -> Vec<SynchronizedConnectionCurve> {
        edges
            .iter()
            .filter_map(|edge| {
                let from = self.synchronized_connection_tab_bounds(edge.from, cx)?;
                let to = self.synchronized_connection_tab_bounds(edge.to, cx)?;
                if !viewport.contains(&from.center()) || !viewport.contains(&to.center()) {
                    return None;
                }
                Some(Self::synchronized_connection_curve(from, to, edge.added_at))
            })
            .collect()
    }

    pub(super) fn hovered_synchronized_connection_tab(
        &self,
        cx: &App,
    ) -> Option<(SynchronizedConnectionTabState, Bounds<Pixels>)> {
        let connection = self.synchronized_connection.as_ref()?;
        for pane in self.pane_group.panes() {
            if self
                .pane_group
                .bounding_box_for_pane(&pane)
                .is_some_and(|pane_bounds| !pane_bounds.contains(&connection.cursor))
            {
                continue;
            }
            for tab_id in pane.read(cx).tab_ids() {
                let Some(tab_bounds) = pane.read(cx).tab_bounds(tab_id) else {
                    continue;
                };
                if !tab_bounds.contains(&connection.cursor) {
                    continue;
                }
                return Some((
                    self.synchronized_connection_tab_state(
                        connection,
                        pane.entity_id(),
                        tab_id,
                        pane.read(cx).tab_is_document(tab_id),
                    ),
                    tab_bounds,
                ));
            }
        }
        None
    }

    pub(super) fn synchronized_connection_hint_origin(
        cursor: Point<Pixels>,
        hovered_bounds: Option<Bounds<Pixels>>,
        viewport: gpui::Size<Pixels>,
    ) -> Point<Pixels> {
        const HINT_WIDTH: f32 = 310.0;
        const HINT_HEIGHT: f32 = 58.0;
        const MARGIN: f32 = 8.0;
        const OFFSET: f32 = 12.0;

        let mut x = cursor.x.as_f32() + OFFSET;
        let mut y = hovered_bounds.map_or(cursor.y.as_f32() + OFFSET, |bounds| {
            bounds.bottom().as_f32() + MARGIN
        });
        if x + HINT_WIDTH + MARGIN > viewport.width.as_f32() {
            x = cursor.x.as_f32() - HINT_WIDTH - OFFSET;
        }
        if y + HINT_HEIGHT + MARGIN > viewport.height.as_f32() {
            y = hovered_bounds.map_or(cursor.y.as_f32() - HINT_HEIGHT - OFFSET, |bounds| {
                bounds.top().as_f32() - HINT_HEIGHT - MARGIN
            });
        }
        let max_x = (viewport.width.as_f32() - HINT_WIDTH - MARGIN).max(MARGIN);
        let max_y = (viewport.height.as_f32() - HINT_HEIGHT - MARGIN).max(MARGIN);
        point(px(x.clamp(MARGIN, max_x)), px(y.clamp(MARGIN, max_y)))
    }

    pub(super) fn render_synchronized_connection(
        &mut self,
        theme: ThemeColor,
        window: &mut Window,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        let now = Instant::now();
        let reduce_motion = cx.reduce_motion();
        if reduce_motion
            || self
                .fading_synchronized_connection
                .as_ref()
                .is_some_and(|fade| {
                    now.saturating_duration_since(fade.started_at) >= SYNCHRONIZED_NETWORK_FADE_OUT
                })
        {
            self.fading_synchronized_connection = None;
        }

        let hovered = self.hovered_synchronized_connection_tab(cx);
        let active = self.synchronized_connection.as_ref().map(|connection| {
            (
                connection.edges.clone(),
                self.synchronized_connection_color_slot(connection.group),
                connection.animation_started_at,
                connection.cursor,
                connection.group,
            )
        });
        let (edges, color_slot, animation_started_at, overall_opacity, hint_data, fading) =
            if let Some((edges, color_slot, animation_started_at, cursor, group)) = active {
                (
                    edges,
                    color_slot,
                    animation_started_at,
                    1.0,
                    Some((cursor, group)),
                    false,
                )
            } else if let Some(fade) = &self.fading_synchronized_connection {
                let progress = now.saturating_duration_since(fade.started_at).as_secs_f32()
                    / SYNCHRONIZED_NETWORK_FADE_OUT.as_secs_f32();
                (
                    fade.edges.clone(),
                    fade.color_slot,
                    fade.animation_started_at,
                    1.0 - progress.clamp(0.0, 1.0),
                    None,
                    true,
                )
            } else {
                return None;
            };

        let viewport = bounds(point(px(0.0), px(0.0)), window.viewport_size());
        let curves = self.synchronized_connection_curves(&edges, viewport, cx);
        let synchronized_theme = cx.global::<SettingsStore>().resolved_theme;
        let color = crate::synchronized_group_color(synchronized_theme, color_slot);
        let handle_outline = theme.background;
        let network = (!curves.is_empty()).then(|| {
            canvas(
                move |_, _, _| curves,
                move |_, curves, window, _| {
                    let handles = Self::synchronized_connection_handles(&curves);
                    let period = SYNCHRONIZED_DOT_PERIOD.as_secs_f32();
                    let phase = now
                        .saturating_duration_since(animation_started_at)
                        .as_secs_f32()
                        / period
                        % 1.0;
                    for curve in curves.iter() {
                        let approximate_length =
                            Self::point_distance(curve.start.position, curve.control_a)
                                + Self::point_distance(curve.control_a, curve.control_b)
                                + Self::point_distance(curve.control_b, curve.end.position);
                        let dot_count = ((approximate_length / 9.0).ceil() as usize).clamp(2, 120);
                        let edge_opacity = if reduce_motion {
                            1.0
                        } else {
                            (now.saturating_duration_since(curve.added_at).as_secs_f32()
                                / SYNCHRONIZED_EDGE_FADE_IN.as_secs_f32())
                            .clamp(0.0, 1.0)
                        };
                        for index in 0..dot_count {
                            let t = Self::synchronized_connection_dot_parameter(index, dot_count);
                            let distance_from_end = t.min(1.0 - t) * 2.0;
                            let pulse = if reduce_motion {
                                0.0
                            } else {
                                (1.0 - (distance_from_end - phase).abs() / 0.22)
                                    .clamp(0.0, 1.0)
                                    .powi(2)
                            };
                            let diameter = if reduce_motion {
                                3.0
                            } else {
                                2.7 + pulse * 1.6
                            };
                            let opacity = if reduce_motion {
                                0.58
                            } else {
                                0.3 + pulse * 0.65
                            };
                            let center = Self::cubic_point(*curve, t);
                            let radius = diameter / 2.0;
                            window.paint_quad(
                                fill(
                                    bounds(
                                        point(center.x - px(radius), center.y - px(radius)),
                                        size(px(diameter), px(diameter)),
                                    ),
                                    color.opacity(opacity * edge_opacity * overall_opacity),
                                )
                                .corner_radii(px(radius)),
                            );
                        }
                    }

                    for handle in handles {
                        let handle_opacity = if reduce_motion {
                            1.0
                        } else {
                            (now.saturating_duration_since(handle.added_at).as_secs_f32()
                                / SYNCHRONIZED_EDGE_FADE_IN.as_secs_f32())
                            .clamp(0.0, 1.0)
                        } * overall_opacity;
                        let outline_radius = SYNCHRONIZED_HANDLE_OUTLINE_DIAMETER / 2.0;
                        window.paint_quad(
                            fill(
                                bounds(
                                    point(
                                        handle.center.x - px(outline_radius),
                                        handle.center.y - px(outline_radius),
                                    ),
                                    size(
                                        px(SYNCHRONIZED_HANDLE_OUTLINE_DIAMETER),
                                        px(SYNCHRONIZED_HANDLE_OUTLINE_DIAMETER),
                                    ),
                                ),
                                handle_outline.opacity(0.96 * handle_opacity),
                            )
                            .corner_radii(px(outline_radius)),
                        );

                        let handle_radius = SYNCHRONIZED_HANDLE_DIAMETER / 2.0;
                        window.paint_quad(
                            fill(
                                bounds(
                                    point(
                                        handle.center.x - px(handle_radius),
                                        handle.center.y - px(handle_radius),
                                    ),
                                    size(
                                        px(SYNCHRONIZED_HANDLE_DIAMETER),
                                        px(SYNCHRONIZED_HANDLE_DIAMETER),
                                    ),
                                ),
                                color.opacity(handle_opacity),
                            )
                            .corner_radii(px(handle_radius)),
                        );
                    }
                },
            )
            .absolute()
            .top_0()
            .right_0()
            .bottom_0()
            .left_0()
        });

        if !reduce_motion && (!edges.is_empty() || fading) {
            window.request_animation_frame();
        }

        let hint = hint_data.map(|(cursor, group)| {
            let group_number = self.synchronized_connection_display_number(group);
            let (primary, secondary) = match hovered.map(|(state, _)| state) {
                Some(SynchronizedConnectionTabState::Source) => (
                    crate::fl!("synchronized-tabs-connection-source"),
                    crate::fl!("synchronized-tabs-connection-finish"),
                ),
                Some(SynchronizedConnectionTabState::Member) => (
                    crate::fl!(
                        "synchronized-tabs-connection-already-member",
                        number = group_number as i64
                    ),
                    crate::fl!("synchronized-tabs-connection-finish"),
                ),
                Some(SynchronizedConnectionTabState::Invalid(
                    SynchronizedConnectionInvalidReason::NonDocument,
                )) => (
                    crate::fl!("synchronized-tabs-connection-documents-only"),
                    crate::fl!("synchronized-tabs-connection-finish"),
                ),
                Some(SynchronizedConnectionTabState::Invalid(
                    SynchronizedConnectionInvalidReason::AlreadyInGroup { group_number },
                )) => (
                    crate::fl!(
                        "synchronized-tabs-connection-already-member",
                        number = group_number as i64
                    ),
                    crate::fl!("synchronized-tabs-connection-finish"),
                ),
                Some(SynchronizedConnectionTabState::Invalid(
                    SynchronizedConnectionInvalidReason::PaneAlreadyRepresented { group_number },
                )) => (
                    crate::fl!(
                        "synchronized-tabs-connection-pane-represented",
                        number = group_number as i64
                    ),
                    crate::fl!("synchronized-tabs-connection-finish"),
                ),
                Some(SynchronizedConnectionTabState::Eligible) | None => match group {
                    SynchronizedConnectionGroup::New(_) => (
                        crate::fl!("synchronized-tabs-connection-select"),
                        crate::fl!("synchronized-tabs-connection-finish"),
                    ),
                    SynchronizedConnectionGroup::Existing(group_id) => {
                        let count = self
                            .synchronized_tabs
                            .group_summary(group_id)
                            .map_or(0, |group| group.member_count);
                        (
                            crate::fl!(
                                "synchronized-tabs-group-summary",
                                number = group_number as i64,
                                count = count as i64
                            ),
                            crate::fl!("synchronized-tabs-connection-continue"),
                        )
                    }
                },
            };
            let origin = Self::synchronized_connection_hint_origin(
                cursor,
                hovered.map(|(_, bounds)| bounds),
                window.viewport_size(),
            );
            div()
                .absolute()
                .left(origin.x)
                .top(origin.y)
                .w(px(310.0))
                .px_3()
                .py_2()
                .rounded_md()
                .border_1()
                .border_color(theme.border)
                .bg(theme.popover)
                .text_color(theme.popover_foreground)
                .shadow_lg()
                .debug_selector(|| "synchronized-connection-hint".to_string())
                .child(
                    div().flex().flex_col().gap_1().child(primary).child(
                        div()
                            .text_size(px(11.0))
                            .text_color(theme.muted_foreground)
                            .child(secondary),
                    ),
                )
        });

        Some(
            div()
                .absolute()
                .top_0()
                .right_0()
                .bottom_0()
                .left_0()
                .debug_selector(|| "synchronized-connection-overlay".to_string())
                .children(network)
                .children(hint)
                .into_any_element(),
        )
    }
}
