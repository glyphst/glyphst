//! Panes and tabs on the GPUI Kit dock, driven through the workspace.

use gpui::{AppContext as _, Entity, EntityId, TestAppContext, VisualTestContext, px, size};
use gpui_component::{
    Placement, Root,
    dock::{DockPlacement, InsertTarget},
};

use super::{SplitDirection, Workspace, WorkspaceCallbacks, WorkspaceSurfaceMode};
use crate::{
    document_view::RenderCache,
    library::LibraryStore,
    pane::{PaneId, panel_id},
    settings::SettingsStore,
    synchronized_tabs::SynchronizedTabRegistry,
};

fn unavailable_workspace_window(
    _: super::WorkspaceWindowRequest,
    _: &mut gpui::App,
) -> anyhow::Result<super::WorkspaceWindowHandle> {
    anyhow::bail!("not available in tests")
}

/// A workspace whose single pane holds `tab_count` new tabs.
fn workspace_with_tabs(
    tab_count: usize,
    cx: &mut TestAppContext,
) -> (Entity<Workspace>, &mut VisualTestContext) {
    cx.update(|cx| {
        gpui_component::init(cx);
        cx.set_reduce_motion(true);
        cx.set_global(SettingsStore::memory());
        cx.set_global(LibraryStore::memory());
        cx.set_global(RenderCache::new());
    });
    let callbacks = WorkspaceCallbacks {
        open_workspace_window: unavailable_workspace_window,
        open_settings_window: |_| {},
    };
    let (root, cx) = cx.add_window_view(move |window, cx| {
        let workspace = cx.new(|cx| Workspace::new_empty(callbacks, window, cx));
        Root::new(workspace, window, cx)
    });
    let workspace = root.read_with(cx, |root, _| {
        root.view().clone().downcast::<Workspace>().unwrap()
    });
    workspace.update_in(cx, |workspace, window, cx| {
        for _ in 0..tab_count {
            let pane = workspace.active_pane;
            workspace.open_new_tab_in_pane(pane, window, cx);
        }
    });
    draw(cx);
    (workspace, cx)
}

fn draw(cx: &mut VisualTestContext) {
    cx.run_until_parked();
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
}

fn tab_ids(
    workspace: &Entity<Workspace>,
    pane: PaneId,
    cx: &mut VisualTestContext,
) -> Vec<EntityId> {
    workspace.read_with(cx, |workspace, cx| workspace.pane(pane, cx).tab_ids())
}

fn drop_tab(
    workspace: &Entity<Workspace>,
    tab: EntityId,
    target: InsertTarget,
    cx: &mut VisualTestContext,
) {
    workspace.update_in(cx, |workspace, window, cx| {
        let area = workspace.dock_area.entity_id();
        workspace.handle_panel_drop(panel_id(tab), Some(area), target, window, cx);
    });
    draw(cx);
}

#[gpui::test]
fn tab_bar_shows_synchronized_group_markers(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace_with_tabs(2, cx);
    let (pane, tab_id) = workspace.read_with(cx, |workspace, cx| {
        let pane = workspace.active_pane(cx);
        (pane.id(), pane.tab_ids()[0])
    });

    workspace.read_with(cx, |workspace, _| {
        let geometry = workspace.pane_geometry.borrow();
        let measured = geometry.tabs.get(&tab_id).copied().unwrap();
        let body = measured.body.unwrap();
        let suffix = measured.suffix.unwrap();
        let visual = workspace.tab_bounds(tab_id).unwrap();
        assert!(
            suffix.right() > body.right(),
            "the suffix is measured separately: body={body:?}, suffix={suffix:?}"
        );
        assert_eq!(visual, body.union(&suffix));
    });

    let mut registry = SynchronizedTabRegistry::default();
    registry
        .create_group(tab_id, pane, EntityId::from(1001), PaneId::from_u64(1002))
        .unwrap();
    registry
        .create_group(
            EntityId::from(2001),
            PaneId::from_u64(2002),
            EntityId::from(2003),
            PaneId::from_u64(2004),
        )
        .unwrap();
    let summary = registry.summary_for_tab(tab_id).unwrap();
    assert!(!summary.is_current);
    workspace.update(cx, |workspace, cx| {
        workspace
            .synchronized_presentation
            .group_by_tab
            .insert(tab_id, summary);
        cx.notify();
    });
    draw(cx);
    assert!(cx.debug_bounds("synchronized-tab-group-link").is_some());

    workspace.update(cx, |workspace, cx| {
        workspace
            .synchronized_presentation
            .paused_tabs
            .insert(tab_id);
        cx.notify();
    });
    draw(cx);
    assert!(cx.debug_bounds("synchronized-tab-paused").is_some());
}

#[gpui::test]
fn a_single_tab_hides_the_tab_bar_until_a_second_one_opens(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace_with_tabs(1, cx);
    assert!(cx.debug_bounds("pane-tab-0").is_none());

    workspace.update_in(cx, |workspace, window, cx| {
        let pane = workspace.active_pane;
        workspace.open_new_tab_in_pane(pane, window, cx);
    });
    draw(cx);
    assert!(cx.debug_bounds("pane-tab-0").is_some());
    assert!(cx.debug_bounds("pane-tab-1").is_some());
}

#[gpui::test]
fn dropping_a_tab_on_its_own_slot_shows_it(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace_with_tabs(2, cx);
    let pane = workspace.read_with(cx, |workspace, _| workspace.active_pane);
    let tabs = tab_ids(&workspace, pane, cx);
    workspace.read_with(cx, |workspace, cx| {
        assert_eq!(workspace.pane(pane, cx).active_tab_ix(), 1);
    });

    drop_tab(
        &workspace,
        tabs[0],
        InsertTarget::Tabs {
            node: pane.node(),
            ix: Some(0),
            activate: true,
        },
        cx,
    );

    workspace.read_with(cx, |workspace, cx| {
        let snapshot = workspace.pane(pane, cx);
        assert_eq!(snapshot.tab_ids(), tabs);
        assert_eq!(snapshot.active_tab_ix(), 0);
    });
}

#[gpui::test]
fn tabs_reorder_within_a_pane(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace_with_tabs(3, cx);
    let pane = workspace.read_with(cx, |workspace, _| workspace.active_pane);
    let tabs = tab_ids(&workspace, pane, cx);

    drop_tab(
        &workspace,
        tabs[2],
        InsertTarget::Tabs {
            node: pane.node(),
            ix: Some(0),
            activate: true,
        },
        cx,
    );

    assert_eq!(
        tab_ids(&workspace, pane, cx),
        vec![tabs[2], tabs[0], tabs[1]]
    );
}

#[gpui::test]
fn dropping_a_tab_on_a_pane_edge_splits_it_off(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace_with_tabs(2, cx);
    let pane = workspace.read_with(cx, |workspace, _| workspace.active_pane);
    let tabs = tab_ids(&workspace, pane, cx);

    drop_tab(
        &workspace,
        tabs[1],
        InsertTarget::Split {
            node: pane.node(),
            placement: Placement::Right,
            size: None,
        },
        cx,
    );

    workspace.read_with(cx, |workspace, cx| {
        let panes = workspace.panes(cx);
        assert_eq!(panes.len(), 2);
        assert_eq!(panes[0].tab_ids(), vec![tabs[0]]);
        assert_eq!(panes[1].tab_ids(), vec![tabs[1]]);
        assert_eq!(workspace.active_pane, panes[1].id());
    });
    assert!(cx.debug_bounds("active-pane-surface").is_some());
}

#[gpui::test]
fn a_drop_that_would_put_two_group_members_in_one_pane_is_refused(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace_with_tabs(3, cx);
    let left = workspace.read_with(cx, |workspace, _| workspace.active_pane);
    let tabs = tab_ids(&workspace, left, cx);
    drop_tab(
        &workspace,
        tabs[2],
        InsertTarget::Split {
            node: left.node(),
            placement: Placement::Right,
            size: None,
        },
        cx,
    );
    let right = workspace.read_with(cx, |workspace, cx| workspace.panes(cx)[1].id());
    workspace.update(cx, |workspace, _| {
        workspace
            .synchronized_tabs
            .create_group(tabs[0], left, tabs[2], right)
            .unwrap();
    });

    drop_tab(
        &workspace,
        tabs[2],
        InsertTarget::Tabs {
            node: left.node(),
            ix: None,
            activate: true,
        },
        cx,
    );
    assert_eq!(tab_ids(&workspace, right, cx), vec![tabs[2]]);

    drop_tab(
        &workspace,
        tabs[1],
        InsertTarget::Tabs {
            node: right.node(),
            ix: None,
            activate: true,
        },
        cx,
    );
    assert_eq!(tab_ids(&workspace, right, cx), vec![tabs[2], tabs[1]]);
    assert_eq!(tab_ids(&workspace, left, cx), vec![tabs[0]]);
}

#[gpui::test]
fn closing_the_last_tab_of_a_pane_closes_the_pane(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace_with_tabs(1, cx);
    workspace.update_in(cx, |workspace, window, cx| {
        workspace.split_active_pane(SplitDirection::Right, window, cx);
    });
    draw(cx);
    let (left, right) = workspace.read_with(cx, |workspace, cx| {
        let panes = workspace.panes(cx);
        (panes[0].id(), panes[1].id())
    });
    assert_eq!(
        workspace.read_with(cx, |workspace, _| workspace.active_pane),
        right
    );

    workspace.update_in(cx, |workspace, window, cx| {
        workspace.close_tab_in_pane(right, None, window, cx);
    });
    draw(cx);
    workspace.read_with(cx, |workspace, cx| {
        assert_eq!(workspace.pane_count(cx), 1);
        assert_eq!(workspace.active_pane, left);
    });

    workspace.update_in(cx, |workspace, window, cx| {
        workspace.close_tab_in_pane(left, None, window, cx);
    });
    draw(cx);
    workspace.read_with(cx, |workspace, cx| {
        assert_eq!(workspace.pane_count(cx), 1, "the last pane stays");
        assert!(workspace.active_pane(cx).active_tab_is_new_tab(cx));
        assert_eq!(workspace.surface_mode, WorkspaceSurfaceMode::Home);
    });
}

#[gpui::test]
fn panes_swap_and_move_to_a_border(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace_with_tabs(1, cx);
    cx.simulate_resize(size(px(1200.0), px(800.0)));
    workspace.update_in(cx, |workspace, window, cx| {
        workspace.split_active_pane(SplitDirection::Right, window, cx);
        workspace.split_active_pane(SplitDirection::Down, window, cx);
    });
    draw(cx);
    // [left | [top right / bottom right]], bottom right active.
    let first_tabs = workspace.read_with(cx, |workspace, cx| {
        workspace
            .panes(cx)
            .iter()
            .map(|pane| pane.active_tab_id().unwrap())
            .collect::<Vec<_>>()
    });
    assert_eq!(first_tabs.len(), 3);

    workspace.update_in(cx, |workspace, window, cx| {
        workspace.swap_pane_in_direction(SplitDirection::Up, window, cx);
    });
    draw(cx);
    let swapped = workspace.read_with(cx, |workspace, cx| {
        workspace
            .panes(cx)
            .iter()
            .map(|pane| pane.active_tab_id().unwrap())
            .collect::<Vec<_>>()
    });
    assert_eq!(swapped, vec![first_tabs[0], first_tabs[2], first_tabs[1]]);
    workspace.read_with(cx, |workspace, cx| {
        assert_eq!(
            workspace.active_pane(cx).active_tab_id(),
            Some(first_tabs[2]),
            "the moved pane stays active"
        );
    });

    workspace.update_in(cx, |workspace, window, cx| {
        workspace.move_pane_to_border(SplitDirection::Left, window, cx);
    });
    draw(cx);
    let moved = workspace.read_with(cx, |workspace, cx| {
        workspace
            .panes(cx)
            .iter()
            .map(|pane| pane.active_tab_id().unwrap())
            .collect::<Vec<_>>()
    });
    assert_eq!(moved, vec![first_tabs[2], first_tabs[0], first_tabs[1]]);
}

#[gpui::test]
fn keyboard_resize_moves_the_divider_by_a_step(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace_with_tabs(1, cx);
    cx.simulate_resize(size(px(1200.0), px(800.0)));
    workspace.update_in(cx, |workspace, window, cx| {
        workspace.split_active_pane(SplitDirection::Right, window, cx);
    });
    draw(cx);
    let root = workspace.read_with(cx, |workspace, cx| {
        workspace
            .dock_area
            .read(cx)
            .layout(DockPlacement::Center)
            .unwrap()
            .root()
            .id()
    });
    let before = workspace.read_with(cx, |workspace, cx| {
        workspace.dock_area.read(cx).split_sizes(root, cx).unwrap()
    });

    workspace.update_in(cx, |workspace, window, cx| {
        workspace.resize_active_pane(SplitDirection::Left, window, cx);
    });
    draw(cx);
    let after = workspace.read_with(cx, |workspace, cx| {
        workspace.dock_area.read(cx).split_sizes(root, cx).unwrap()
    });
    assert!(
        (after[1].as_f32() - (before[1].as_f32() - crate::pane::RESIZE_STEP)).abs() < 1.0,
        "before={before:?}, after={after:?}"
    );

    workspace.update_in(cx, |workspace, window, cx| {
        workspace.reset_pane_sizes(window, cx);
    });
    draw(cx);
    let reset = workspace.read_with(cx, |workspace, cx| {
        workspace.dock_area.read(cx).split_sizes(root, cx).unwrap()
    });
    assert!((reset[0].as_f32() - reset[1].as_f32()).abs() < 1.0);
}
