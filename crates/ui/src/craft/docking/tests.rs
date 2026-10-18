use super::*;
use crate::craft::layout::{SplitAxis, SplitSize};
use egui::{Rect, pos2, vec2};

fn tabs(panels: &[u32], active: usize) -> Node<u32> {
    Node::Tabs {
        panels: panels.to_vec(),
        active,
    }
}

fn sample() -> Layout<u32> {
    Layout {
        root: Some(Node::Split {
            axis: SplitAxis::Horizontal,
            size: SplitSize::FixedSecond(240.0),
            first: Box::new(tabs(&[1], 0)),
            second: Box::new(tabs(&[2, 3, 4], 1)),
        }),
        floating: vec![Floating {
            panels: vec![5, 6],
            active: 1,
            rect: [80.0, 70.0, 300.0, 220.0],
        }],
    }
}

fn move_to(panel: u32, anchor: u32, zone: Zone) -> Action<u32> {
    Action::Move {
        panel,
        anchor,
        placement: Placement::Split(zone),
    }
}

#[test]
fn floating_brush_panel_joins_main_tabs_without_losing_other_panels() {
    let mut layout = sample();
    layout
        .apply(Action::Move {
            panel: 5,
            anchor: 3,
            placement: Placement::Tab { before: Some(4) },
        })
        .unwrap();
    let Node::Split { size, second, .. } = layout.root.as_ref().unwrap() else {
        panic!()
    };
    assert_eq!(*size, SplitSize::FixedSecond(240.0));
    assert_eq!(**second, tabs(&[2, 3, 5, 4], 2));
    assert_eq!(layout.floating[0].panels, vec![6]);
    assert_eq!(layout.floating[0].active, 0);
    layout.validate().unwrap();
    assert_eq!(layout.panels().len(), 6);
}

#[test]
fn detach_redock_merge_and_collapse_empty_source_containers() {
    let mut layout = sample();
    layout
        .apply(Action::Float {
            panel: 1,
            rect: [30.0, 40.0, 250.0, 200.0],
        })
        .unwrap();
    assert_eq!(layout.root, Some(tabs(&[2, 3, 4], 1)));
    layout.apply(move_to(1, 2, Zone::Left)).unwrap();
    assert_eq!(layout.floating.len(), 1);
    layout.apply(move_to(5, 6, Zone::Center)).unwrap();
    assert_eq!(layout.floating[0].panels, vec![6, 5]);
    assert_eq!(layout.floating[0].active, 1);
    layout.apply(move_to(6, 3, Zone::Center)).unwrap();
    layout.apply(move_to(5, 3, Zone::Center)).unwrap();
    assert!(layout.floating.is_empty());
    assert_eq!(layout.panels().len(), 6);
}

#[test]
fn invalid_moves_are_atomic_including_after_a_source_would_have_been_removed() {
    let layout = sample();
    for action in [
        move_to(5, 99, Zone::Center),
        move_to(99, 2, Zone::Center),
        move_to(5, 6, Zone::Left),
        move_to(5, 5, Zone::Center),
        Action::Move {
            panel: 5,
            anchor: 2,
            placement: Placement::Tab { before: Some(6) },
        },
        Action::Float {
            panel: 3,
            rect: [f32::NAN, 0.0, 100.0, 100.0],
        },
        Action::Float {
            panel: 3,
            rect: [0.0, 0.0, 0.0, 100.0],
        },
        Action::ResizeSplit {
            path: vec![false],
            size: SplitSize::Ratio(0.3),
        },
    ] {
        let mut changed = layout.clone();
        assert!(changed.apply(action).is_err());
        assert_eq!(changed, layout);
    }
}

#[test]
fn removing_an_earlier_tab_preserves_selected_identity() {
    let mut layout = Layout {
        root: Some(tabs(&[1, 2, 3, 4], 2)),
        floating: Vec::new(),
    };
    layout.apply(Action::Close { panel: 1 }).unwrap();
    assert_eq!(layout.root, Some(tabs(&[2, 3, 4], 1)));
    layout.apply(Action::Close { panel: 3 }).unwrap();
    assert_eq!(layout.root, Some(tabs(&[2, 4], 1)));
}

#[test]
fn reordering_tabs_uses_destination_identity_after_removal() {
    let mut layout = Layout {
        root: Some(tabs(&[1, 2, 3], 0)),
        floating: Vec::new(),
    };
    layout
        .apply(Action::Move {
            panel: 1,
            anchor: 2,
            placement: Placement::Tab { before: None },
        })
        .unwrap();
    assert_eq!(layout.root, Some(tabs(&[2, 3, 1], 2)));
    layout
        .apply(Action::Move {
            panel: 1,
            anchor: 2,
            placement: Placement::Tab { before: Some(2) },
        })
        .unwrap();
    assert_eq!(layout.root, Some(tabs(&[1, 2, 3], 0)));
    let before = layout.clone();
    layout
        .apply(Action::Move {
            panel: 1,
            anchor: 2,
            placement: Placement::Tab { before: Some(1) },
        })
        .unwrap();
    assert_eq!(layout, before);
}

#[test]
fn accordion_open_state_and_custom_height_survive_reorder() {
    let mut layout = Layout {
        root: Some(Node::Stack {
            entries: vec![
                StackEntry {
                    panel: 1,
                    open: false,
                    height: Some(123.0),
                },
                StackEntry {
                    panel: 2,
                    open: true,
                    height: None,
                },
            ],
        }),
        floating: Vec::new(),
    };
    layout
        .apply(Action::Move {
            panel: 1,
            anchor: 2,
            placement: Placement::Tab { before: None },
        })
        .unwrap();
    let Node::Stack { entries } = layout.root.as_ref().unwrap() else {
        panic!()
    };
    assert_eq!(
        entries[1],
        StackEntry {
            panel: 1,
            open: false,
            height: Some(123.0)
        }
    );
    let location = layout.location(&1).unwrap();
    layout.apply(Action::Close { panel: 1 }).unwrap();
    layout.restore(1, &location).unwrap();
    let Node::Stack { entries } = layout.root.as_ref().unwrap() else {
        panic!()
    };
    assert_eq!(
        entries[1],
        StackEntry {
            panel: 1,
            open: false,
            height: Some(123.0)
        }
    );
}

#[test]
fn protected_canvas_remains_present_and_cannot_be_grouped_with_panels() {
    let permissions = |panel: &u32| {
        if *panel == 1 {
            Permissions::PROTECTED
        } else {
            Permissions::default()
        }
    };
    let layout = sample();
    for action in [
        Action::Close { panel: 1 },
        Action::Float {
            panel: 1,
            rect: [10.0, 10.0, 200.0, 200.0],
        },
        move_to(1, 3, Zone::Center),
        move_to(3, 1, Zone::Center),
        Action::OpenAt {
            panel: 1,
            anchor: 3,
            placement: Placement::Tab { before: None },
        },
    ] {
        let mut next = layout.clone();
        assert_eq!(
            next.apply_with_permissions(action, permissions),
            Err(DockError::NotAllowed)
        );
        assert_eq!(next, layout);
    }
    let mut next = layout;
    next.apply_with_permissions(
        Action::OpenAt {
            panel: 7,
            anchor: 1,
            placement: Placement::Split(Zone::Left),
        },
        permissions,
    )
    .unwrap();
    assert!(next.contains(&1) && next.contains(&7));
}

#[test]
fn close_and_restore_preserve_fixed_side_and_floating_geometry() {
    let mut layout = Layout {
        root: Some(Node::Split {
            axis: SplitAxis::Horizontal,
            size: SplitSize::FixedFirst(180.0),
            first: Box::new(tabs(&[1], 0)),
            second: Box::new(tabs(&[2], 0)),
        }),
        floating: vec![Floating {
            panels: vec![3],
            active: 0,
            rect: [41.0, 52.0, 233.0, 244.0],
        }],
    };
    let saved = layout.clone();
    let first = layout.location(&1).unwrap();
    let float = layout.location(&3).unwrap();
    layout.apply(Action::Close { panel: 1 }).unwrap();
    layout.apply(Action::Close { panel: 3 }).unwrap();
    layout.restore(1, &first).unwrap();
    layout.restore(3, &float).unwrap();
    assert_eq!(layout, saved);
    let invalid = Location {
        anchor: Some(99),
        ..first
    };
    layout.apply(Action::Close { panel: 1 }).unwrap();
    let before = layout.clone();
    assert_eq!(layout.restore(1, &invalid), Err(DockError::MissingTarget));
    assert_eq!(layout, before);
}

#[test]
fn closing_and_reopening_an_entire_accordion_restores_its_kind_and_heights() {
    let mut layout = Layout {
        root: Some(Node::Split {
            axis: SplitAxis::Horizontal,
            size: SplitSize::FixedSecond(220.0),
            first: Box::new(tabs(&[1], 0)),
            second: Box::new(Node::Stack {
                entries: vec![
                    StackEntry {
                        panel: 2,
                        open: false,
                        height: Some(90.0),
                    },
                    StackEntry {
                        panel: 3,
                        open: true,
                        height: Some(180.0),
                    },
                ],
            }),
        }),
        floating: Vec::new(),
    };
    let before = layout.clone();
    let second = layout.location(&2).unwrap();
    layout.apply(Action::Close { panel: 2 }).unwrap();
    let third = layout.location(&3).unwrap();
    layout.apply(Action::Close { panel: 3 }).unwrap();
    layout.restore(3, &third).unwrap();
    layout.restore(2, &second).unwrap();
    assert_eq!(layout, before);
}

#[test]
fn invalid_saved_layouts_reject_duplicates_selection_depth_and_nonfinite_geometry() {
    let mut duplicate = sample();
    duplicate.floating[0].panels.push(1);
    assert_eq!(duplicate.validate(), Err(DockError::DuplicatePanel));
    let mut invalid = sample();
    invalid.floating[0].active = usize::MAX;
    assert_eq!(invalid.validate(), Err(DockError::InvalidSelection));
    invalid.floating[0].active = 0;
    invalid.floating[0].rect[3] = f32::INFINITY;
    assert_eq!(invalid.validate(), Err(DockError::InvalidGeometry));
    let mut deep = tabs(&[0], 0);
    for panel in 1..=MAX_DEPTH as u32 {
        deep = Node::Split {
            axis: SplitAxis::Horizontal,
            size: SplitSize::Ratio(0.5),
            first: Box::new(deep),
            second: Box::new(tabs(&[panel], 0)),
        };
    }
    let mut deep = Layout {
        root: Some(deep),
        floating: Vec::new(),
    };
    assert_eq!(deep.validate(), Err(DockError::LimitExceeded));
    assert_eq!(
        deep.apply(Action::Activate { panel: 0 }),
        Err(DockError::LimitExceeded)
    );
    let oversized = Layout {
        root: Some(Node::Tabs {
            panels: (0..=MAX_PANELS as u32).collect(),
            active: 0,
        }),
        floating: Vec::new(),
    };
    assert_eq!(oversized.validate(), Err(DockError::LimitExceeded));
}

fn target(anchor: u32, members: &[u32], rect: Rect) -> DropTarget<u32> {
    let strip = Rect::from_min_size(rect.min, vec2(rect.width(), 24.0));
    DropTarget {
        anchor,
        rect,
        tab_strip: Some(strip),
        tabs: members
            .iter()
            .enumerate()
            .map(|(i, panel)| {
                (
                    *panel,
                    Rect::from_min_size(rect.min + vec2(i as f32 * 60.0, 0.0), vec2(60.0, 24.0)),
                )
            })
            .collect(),
        allow_splits: true,
        accepts_tabs: true,
    }
}

#[test]
fn pointer_targets_distinguish_reorder_center_split_and_occlusion() {
    let rect = Rect::from_min_size(pos2(10.0, 20.0), vec2(300.0, 200.0));
    let group = target(2, &[2, 3], rect);
    let placement = resolve_drop(&5, pos2(90.0, 30.0), std::slice::from_ref(&group)).unwrap();
    assert_eq!(placement.placement, Placement::Tab { before: Some(3) });
    assert!(placement.insertion.unwrap().is_finite());
    assert_eq!(
        resolve_drop(&5, rect.center(), std::slice::from_ref(&group))
            .unwrap()
            .placement,
        Placement::Split(Zone::Center)
    );
    assert_eq!(
        resolve_drop(&5, pos2(11.0, 90.0), std::slice::from_ref(&group))
            .unwrap()
            .placement,
        Placement::Split(Zone::Left)
    );
    assert!(resolve_drop(&3, rect.center(), std::slice::from_ref(&group)).is_none());
    assert!(resolve_drop(&3, pos2(11.0, 90.0), std::slice::from_ref(&group)).is_some());
    let mut overlay = target(5, &[5], rect);
    overlay.accepts_tabs = false;
    overlay.allow_splits = false;
    assert!(resolve_drop(&3, rect.center(), &[group, overlay]).is_none());
    assert!(drop_zone(Rect::NOTHING, 24.0, pos2(0.0, 0.0)).is_none());
    assert!(drop_zone(rect, 24.0, pos2(f32::NAN, 0.0)).is_none());
}

#[test]
fn long_action_sequence_never_loses_or_duplicates_a_panel() {
    let mut layout = sample();
    let mut seed = 72u64;
    for step in 0..800 {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        let panel = (seed % 6 + 1) as u32;
        let anchor = ((seed >> 16) % 6 + 1) as u32;
        let action = if step % 7 == 0 {
            Action::Float {
                panel,
                rect: [10.0, 20.0, 200.0, 220.0],
            }
        } else {
            move_to(
                panel,
                anchor,
                match seed % 5 {
                    0 => Zone::Left,
                    1 => Zone::Right,
                    2 => Zone::Top,
                    3 => Zone::Bottom,
                    _ => Zone::Center,
                },
            )
        };
        let before = layout.clone();
        if layout.apply(action).is_err() {
            assert_eq!(layout, before);
        }
        layout.validate().unwrap();
        let mut panels: Vec<u32> = layout.panels().into_iter().copied().collect();
        panels.sort_unstable();
        assert_eq!(panels, vec![1, 2, 3, 4, 5, 6]);
    }
}

#[test]
fn saved_layout_and_hidden_location_round_trip() {
    let layout = sample();
    let value = serde_json::to_string(&layout).unwrap();
    let restored: Layout<u32> = serde_json::from_str(&value).unwrap();
    restored.validate().unwrap();
    assert_eq!(restored, layout);
    let location = layout.location(&3).unwrap();
    let encoded = serde_json::to_string(&location).unwrap();
    assert_eq!(
        serde_json::from_str::<Location<u32>>(&encoded).unwrap(),
        location
    );
}

#[test]
fn old_hidden_locations_accept_panel_ids_without_a_default_identity() {
    #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
    enum Panel {
        Layers,
        Brushes,
    }
    let location: Location<Panel> = serde_json::from_value(serde_json::json!({
        "floating": null, "anchor": "Layers", "placement": {"Tab": {"before": null}},
        "size": null, "stack": null
    }))
    .unwrap();
    assert!(location.sibling.is_empty());
    assert_eq!(location.anchor, Some(Panel::Layers));
}
