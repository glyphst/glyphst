use glyphst_translation_ui::DocumentTranslationManager;
use gpui::{AppContext as _, Axis, Entity, EntityId, TestAppContext};
use gpui_component::Root;

use super::{PaneLayout, SplitDirection};
use crate::{
    document_view::RenderCache, library::LibraryStore, library::ReaderPaneTree,
    settings::SettingsStore, tab_item::TabItem,
};

/// `count` new tabs, made in a throwaway window.
fn tabs(count: usize, cx: &mut TestAppContext) -> Vec<Entity<TabItem>> {
    cx.update(|cx| {
        gpui_component::init(cx);
        cx.set_global(SettingsStore::memory());
        cx.set_global(LibraryStore::memory());
        cx.set_global(RenderCache::new());
    });
    let jobs = cx.update(|cx| cx.new(DocumentTranslationManager::new));
    let (_, cx) = cx.add_window_view(|window, cx| Root::new(cx.new(|_| gpui::Empty), window, cx));
    cx.update(|window, cx| {
        (0..count)
            .map(|_| TabItem::new_tab(jobs.clone(), window, cx))
            .collect()
    })
}

fn pane(tab: &Entity<TabItem>) -> PaneLayout {
    PaneLayout::Pane {
        tabs: vec![tab.clone()],
        active_ix: 0,
    }
}

/// The first tab of each pane, in layout order, with its axis path.
fn shape(layout: &PaneLayout) -> String {
    match layout {
        PaneLayout::Pane { tabs, .. } => format!("{}", tabs[0].entity_id().as_u64()),
        PaneLayout::Split { axis, children } => {
            let axis = match axis {
                Axis::Horizontal => "h",
                Axis::Vertical => "v",
            };
            let children = children
                .iter()
                .map(|(child, _)| shape(child))
                .collect::<Vec<_>>()
                .join(",");
            format!("{axis}[{children}]")
        }
    }
}

fn id(tab: &Entity<TabItem>) -> EntityId {
    tab.entity_id()
}

#[gpui::test]
fn swapping_exchanges_two_panes_and_keeps_the_shape(cx: &mut TestAppContext) {
    let tabs = tabs(3, cx);
    let mut layout = PaneLayout::Split {
        axis: Axis::Horizontal,
        children: vec![
            (pane(&tabs[0]), 0.5),
            (
                PaneLayout::Split {
                    axis: Axis::Vertical,
                    children: vec![(pane(&tabs[1]), 0.5), (pane(&tabs[2]), 0.5)],
                },
                0.5,
            ),
        ],
    };
    let [a, b, c] = [id(&tabs[0]), id(&tabs[1]), id(&tabs[2])].map(|id| id.as_u64());

    assert!(layout.swap_panes(id(&tabs[0]), id(&tabs[2])));
    assert_eq!(shape(&layout), format!("h[{c},v[{b},{a}]]"));
    assert!(!layout.swap_panes(id(&tabs[0]), id(&tabs[0])));
}

#[gpui::test]
fn moving_to_a_border_spans_the_whole_edge(cx: &mut TestAppContext) {
    let tabs = tabs(3, cx);
    let mut layout = PaneLayout::Split {
        axis: Axis::Horizontal,
        children: vec![
            (pane(&tabs[0]), 0.5),
            (
                PaneLayout::Split {
                    axis: Axis::Vertical,
                    children: vec![(pane(&tabs[1]), 0.5), (pane(&tabs[2]), 0.5)],
                },
                0.5,
            ),
        ],
    };
    let [a, b, c] = [id(&tabs[0]), id(&tabs[1]), id(&tabs[2])].map(|id| id.as_u64());

    assert!(layout.move_pane_to_border(id(&tabs[2]), SplitDirection::Down));
    assert_eq!(shape(&layout), format!("v[h[{a},{b}],{c}]"));

    // Already along that edge: nothing moves.
    assert!(!layout.move_pane_to_border(id(&tabs[2]), SplitDirection::Down));

    assert!(layout.move_pane_to_border(id(&tabs[0]), SplitDirection::Up));
    assert_eq!(shape(&layout), format!("v[{a},{b},{c}]"));
    let PaneLayout::Split { children, .. } = &layout else {
        panic!("still a split");
    };
    let total: f32 = children.iter().map(|(_, share)| share).sum();
    assert!((total - 1.0).abs() < 0.001, "shares stay normalized");
}

#[gpui::test]
fn a_single_pane_has_no_border_to_move_to(cx: &mut TestAppContext) {
    let tabs = tabs(1, cx);
    let mut layout = pane(&tabs[0]);
    assert!(!layout.move_pane_to_border(id(&tabs[0]), SplitDirection::Left));
}

#[gpui::test]
fn the_session_tree_numbers_tabs_and_records_shares(cx: &mut TestAppContext) {
    let tabs = tabs(3, cx);
    let layout = PaneLayout::Split {
        axis: Axis::Vertical,
        children: vec![
            (
                PaneLayout::Pane {
                    tabs: vec![tabs[0].clone(), tabs[1].clone()],
                    active_ix: 1,
                },
                0.25,
            ),
            (pane(&tabs[2]), 0.75),
        ],
    };
    let (tree, active_pane, runtime_ids) = cx.read(|cx| {
        let mut next_pane = 0;
        let mut next_tab = 0;
        let mut active_pane = 0;
        let mut runtime_ids = Default::default();
        let tree = layout.session_tree(
            Some(id(&tabs[2])),
            &mut next_pane,
            &mut next_tab,
            &mut active_pane,
            &mut runtime_ids,
            cx,
        );
        (tree, active_pane, runtime_ids)
    });

    let ReaderPaneTree::Split {
        ratios, children, ..
    } = tree
    else {
        panic!("a split session");
    };
    assert_eq!(ratios, vec![0.25, 0.75]);
    let ReaderPaneTree::Pane(first) = &children[0] else {
        panic!("a pane");
    };
    assert_eq!(first.tabs.len(), 2);
    assert_eq!(first.active_tab, Some(first.tabs[1].id));
    assert_eq!(active_pane, 2);
    assert_eq!(runtime_ids.len(), 3);
    assert_eq!(runtime_ids[&id(&tabs[2])], 3);
}
