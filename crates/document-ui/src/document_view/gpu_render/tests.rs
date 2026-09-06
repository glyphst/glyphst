use super::*;

fn render_key() -> RenderKey {
    RenderKey {
        doc_id: 1,
        page_idx: 0,
        source_revision: 0,
        rotation: DocumentRotation::Degrees0,
        color_transform: None,
        physical_width: 1_000,
        physical_height: 2_000,
    }
}

#[test]
fn vector_refinement_prepares_the_requested_camera_scale_once() {
    assert_eq!(next_vector_lod_scale(8.0, 11.264), Some(11.264));
    assert_eq!(next_vector_lod_scale(8.0, 8_192.0), Some(8_192.0));
    assert_eq!(next_vector_lod_scale(8.0, 19_689.184), Some(19_689.184));
    assert_eq!(next_vector_lod_scale(4_096.0, 8_192.0), Some(8_192.0));
    assert_eq!(next_vector_lod_scale(8.0, 8.0), None);
    assert_eq!(next_vector_lod_scale(8_192.0, 16_384.0), Some(16_384.0));
    assert_eq!(motion_vector_lod_scale(10.0, 2.0, 8.0, 8.0), 40.4);
    assert_eq!(motion_vector_lod_scale(10.0, 8.0, 2.0, 8.0), 10.0);
    assert_eq!(motion_vector_lod_scale(10.0, 2.0, 2.1, 8.0), 20.2);
    assert!(vector_lod_request_covers(20_000.0, 19_689.184));
    assert!(!vector_lod_request_covers(8.0, 19_689.184));
    assert!(vector_lod_request_covers(53.0, 54.0));
    assert!(!vector_lod_request_covers(53.0, 107.0));
}

#[test]
fn every_visible_cache_miss_uses_the_camera_current_path() {
    assert!(use_current_camera_render(true, false));
    assert!(!use_current_camera_render(true, true));
    assert!(!use_current_camera_render(false, false));
}

#[test]
fn unchanged_camera_reuses_one_direct_submission() {
    let identity = DirectRenderIdentity {
        key: render_key(),
        visible_region: GpuPageRegion {
            x: 1,
            y: 2,
            width: 3,
            height: 4,
        },
        device_origin_phase: [0.25, -0.25],
        camera_serial: 7,
        bounds: Bounds::new(point(px(10.0), px(20.0)), size(px(30.0), px(40.0))),
    };
    assert!(identity == identity);
    assert!(
        identity
            != DirectRenderIdentity {
                camera_serial: 8,
                ..identity
            }
    );
}

#[test]
fn offscreen_retained_refinements_do_not_keep_the_visible_pipeline_pending() {
    let presented = HashSet::from([2, 3]);
    assert_eq!(presented_refinement_page_count(&presented, [1, 2, 4]), 1);
    assert_eq!(presented_refinement_page_count(&presented, [1, 4]), 0);
}

#[test]
fn viewport_crop_keeps_device_density_above_the_raster_dimension_limit() {
    let key = RenderKey {
        physical_width: 40_000,
        physical_height: 20_000,
        ..render_key()
    };
    let image = Bounds::new(point(px(0.0), px(0.0)), size(px(20_000.0), px(10_000.0)));
    let viewport = Bounds::new(
        point(px(10_000.0), px(5_000.0)),
        size(px(1_000.0), px(800.0)),
    );
    let region = visible_gpu_region(key, image, viewport).expect("visible virtual crop");
    assert_eq!(
        region,
        GpuPageRegion {
            x: 20_000,
            y: 10_000,
            width: 2_000,
            height: 1_600,
        }
    );
    assert_eq!(
        physical_gpu_viewport_size(key, image, viewport),
        Some([2_000, 1_600])
    );
    assert_eq!(gpu_surface_bounds(key, image, region), viewport);

    let capped = RenderKey {
        physical_width: 8_192,
        physical_height: 4_096,
        ..key
    };
    assert_eq!(
        physical_gpu_viewport_size(capped, image, viewport),
        Some([410, 328]),
        "the former raster-sized page space undersampled the visible viewport"
    );
}

#[test]
fn continuity_prevents_motion_selection_gaps() {
    assert_eq!(
        presented_surface_kind(true, true, true),
        Some(PresentedSurfaceKind::Primary)
    );
    assert_eq!(
        presented_surface_kind(false, true, true),
        Some(PresentedSurfaceKind::Continuity)
    );
    assert_eq!(
        presented_surface_kind(false, false, true),
        Some(PresentedSurfaceKind::PartialPrimary)
    );
    assert_eq!(presented_surface_kind(false, false, false), None);

    let primary = GpuPageRegion {
        x: 200,
        y: 300,
        width: 600,
        height: 800,
    };
    for frame in 0..120_u32 {
        let visible = GpuPageRegion {
            x: frame.saturating_mul(10),
            y: frame.saturating_mul(15),
            width: 400,
            height: 500,
        };
        let primary_covers = gpu_region_contains(primary, visible);
        let primary_overlaps = gpu_regions_overlap(primary, visible);
        assert!(
            presented_surface_kind(primary_covers, true, primary_overlaps).is_some(),
            "frame {frame} lost all presentation coverage"
        );
    }
}

#[test]
fn motion_density_gate_is_bounded_and_source_aware() {
    let requested = render_key();
    let mut ninety_percent = requested;
    ninety_percent.physical_width = 900;
    ninety_percent.physical_height = 1_800;
    assert!(motion_surface_density_sufficient(ninety_percent, requested));

    let mut below_threshold = ninety_percent;
    below_threshold.physical_width -= 1;
    assert!(!motion_surface_density_sufficient(
        below_threshold,
        requested
    ));

    let mut higher_resolution = requested;
    higher_resolution.physical_width = 2_000;
    higher_resolution.physical_height = 4_000;
    assert!(motion_surface_density_sufficient(
        higher_resolution,
        requested
    ));

    let mut different_source = requested;
    different_source.source_revision = 1;
    assert!(!motion_surface_density_sufficient(
        requested,
        different_source
    ));
}

#[test]
fn translation_cache_never_crosses_a_zoom_scale() {
    let cached = render_key();
    assert!(translation_cache_compatible(cached, cached));
    assert!(!translation_cache_compatible(
        cached,
        RenderKey {
            physical_width: cached.physical_width + 1,
            ..cached
        }
    ));
}

#[test]
fn device_origin_phase_matches_gpui_half_ties_and_fractional_scroll() {
    let bounds = Bounds::new(point(px(10.25), px(-3.25)), size(px(100.0), px(200.0)));
    assert_eq!(gpu_device_origin_phase(bounds, 2.0), [0.5, -0.5]);

    let half_tie = Bounds::new(point(px(0.25), px(-0.25)), size(px(1.0), px(1.0)));
    assert_eq!(gpu_device_origin_phase(half_tie, 2.0), [0.5, -0.5]);
    assert_eq!(round_half_toward_zero(0.5), 0.0);
    assert_eq!(round_half_toward_zero(-0.5), 0.0);
}

#[test]
fn zoom_motion_cancels_only_replaceable_or_lower_priority_work() {
    assert!(zoom_motion_cancels_surface_work(
        true,
        SurfaceRenderRole::Primary,
        false,
    ));
    assert!(zoom_motion_cancels_surface_work(
        false,
        SurfaceRenderRole::Continuity,
        false,
    ));
    assert!(zoom_motion_cancels_surface_work(
        false,
        SurfaceRenderRole::Primary,
        true,
    ));
    assert!(!zoom_motion_cancels_surface_work(
        false,
        SurfaceRenderRole::Primary,
        false,
    ));
}

#[test]
fn patch_lod_worker_is_reused_only_for_the_same_declared_scale_slot() {
    assert!(same_patch_lod_request(4.0, 4.0));
    assert!(!same_patch_lod_request(4.0, 8.0));
    assert!(!same_patch_lod_request(8.0, 4.0));
}

#[test]
fn preparation_window_is_directional_and_deterministic() {
    let visible = [4, 5].into_iter().collect::<HashSet<_>>();
    let (forward, nearest_forward) =
        gpu_preparation_targets(&visible, &HashSet::new(), &[], 10, true);
    assert_eq!(nearest_forward, Some(6));
    assert_eq!(
        forward
            .iter()
            .map(|target| (target.page, target.speculative))
            .collect::<Vec<_>>(),
        vec![
            (4, false),
            (5, false),
            (6, true),
            (7, true),
            (8, true),
            (9, true),
            (3, true),
            (2, true)
        ]
    );

    let (backward, nearest_backward) =
        gpu_preparation_targets(&visible, &HashSet::new(), &[], 10, false);
    assert_eq!(nearest_backward, Some(3));
    assert_eq!(
        backward
            .iter()
            .map(|target| (target.page, target.speculative))
            .collect::<Vec<_>>(),
        vec![
            (4, false),
            (5, false),
            (3, true),
            (2, true),
            (1, true),
            (0, true),
            (6, true),
            (7, true)
        ]
    );
}

#[test]
fn target_viewport_precedes_the_directional_scroll_corridor() {
    let visible = [2].into_iter().collect::<HashSet<_>>();
    let target = [6, 7].into_iter().collect::<HashSet<_>>();
    let (targets, nearest) = gpu_preparation_targets(&visible, &target, &[], 12, true);
    assert_eq!(nearest, Some(3));
    assert_eq!(
        targets
            .iter()
            .map(|target| (target.page, target.speculative))
            .collect::<Vec<_>>(),
        vec![
            (2, false),
            (6, false),
            (7, false),
            (3, true),
            (4, true),
            (5, true),
            (8, true),
            (1, true),
        ]
    );
}

#[test]
fn recent_motion_history_precedes_directional_speculation() {
    let visible = HashSet::from([6]);
    let history = [2, 3, 4, 5, 6];
    let (targets, nearest) = gpu_preparation_targets(&visible, &HashSet::new(), &history, 12, true);
    assert_eq!(nearest, Some(7));
    assert_eq!(
        targets
            .iter()
            .map(|target| (target.page, target.speculative))
            .collect::<Vec<_>>(),
        vec![
            (6, false),
            (5, true),
            (4, true),
            (3, true),
            (2, true),
            (7, true),
            (8, true),
            (9, true),
        ]
    );
}

#[test]
fn visible_pages_consume_the_speculative_cache_budget_first() {
    let visible = (0..MAX_RETAINED_GPU_PAGES as u32).collect::<HashSet<_>>();
    let (targets, nearest) = gpu_preparation_targets(&visible, &HashSet::new(), &[], 20, true);
    assert_eq!(targets.len(), visible.len());
    assert!(targets.iter().all(|target| !target.speculative));
    assert_eq!(nearest, None);
}

#[test]
fn scrolling_preserves_one_lookahead_worker_and_zoom_suspends_it() {
    assert_eq!(speculative_preparation_limit(true, true), 0);
    assert_eq!(speculative_preparation_limit(false, true), 0);
    assert_eq!(speculative_preparation_limit(true, false), 1);
    assert_eq!(
        speculative_preparation_limit(false, false),
        MAX_CONCURRENT_GPU_PAGE_PREPARATIONS,
    );

    let mut state = GpuPdfState::default();
    for (page, speculative) in [(4, false), (5, true), (6, true)] {
        state.pages.insert(
            page,
            GpuPageState::Preparing {
                generation: 1,
                cancellation: GpuCancellationToken::new(),
                demand: GpuPreparationDemand {
                    priority: page as usize,
                    speculative,
                    sequence: u64::from(page),
                },
                _task: Task::ready(()),
            },
        );
    }
    state.begin_motion();
    assert!(state.pages.contains_key(&4));
    assert!(state.pages.contains_key(&5));
    assert!(!state.pages.contains_key(&6));
    assert_eq!(state.active_speculative_preparations(), 1);
    assert_eq!(state.preparation_cancelled, 1);

    state.begin_zoom_motion();
    assert!(state.pages.contains_key(&4));
    assert!(!state.pages.contains_key(&5));
    assert_eq!(state.active_speculative_preparations(), 0);
    assert_eq!(state.preparation_cancelled, 2);
}

#[test]
fn demand_changes_update_active_priority_without_restarting_the_page() {
    let mut state = GpuPdfState::default();
    let cancellation = GpuCancellationToken::new();
    state.pages.insert(
        4,
        GpuPageState::Preparing {
            generation: 1,
            cancellation: cancellation.clone(),
            demand: GpuPreparationDemand {
                priority: 4,
                speculative: true,
                sequence: 1,
            },
            _task: Task::ready(()),
        },
    );
    state.preparation_demands.insert(
        4,
        GpuPreparationDemand {
            priority: 4,
            speculative: true,
            sequence: 1,
        },
    );
    state.set_preparation_demands(&[GpuPreparationTarget {
        page: 4,
        priority: 0,
        speculative: false,
    }]);

    assert!(!cancellation.is_cancelled());
    assert!(matches!(
        state.pages.get(&4),
        Some(GpuPageState::Preparing {
            demand: GpuPreparationDemand {
                priority: 0,
                speculative: false,
                ..
            },
            ..
        })
    ));
}

#[test]
fn pruning_cancels_stale_preparation_and_drops_its_demand() {
    let mut state = GpuPdfState::default();
    let cancellation = GpuCancellationToken::new();
    let demand = GpuPreparationDemand {
        priority: 2,
        speculative: true,
        sequence: 1,
    };
    state.pages.insert(
        7,
        GpuPageState::Preparing {
            generation: 1,
            cancellation: cancellation.clone(),
            demand,
            _task: Task::ready(()),
        },
    );
    state.preparation_demands.insert(7, demand);
    state.prune(&HashSet::new(), &HashSet::new());

    assert!(cancellation.is_cancelled());
    assert!(!state.pages.contains_key(&7));
    assert!(!state.preparation_demands.contains_key(&7));
    assert_eq!(state.preparation_cancelled, 1);
}

#[test]
fn visible_preemption_selects_the_least_useful_speculative_worker() {
    let mut state = GpuPdfState::default();
    for (page, priority, sequence) in [(6, 2, 1), (8, 4, 2)] {
        let demand = GpuPreparationDemand {
            priority,
            speculative: true,
            sequence,
        };
        state.preparation_demands.insert(page, demand);
        state.pages.insert(
            page,
            GpuPageState::Preparing {
                generation: sequence,
                cancellation: GpuCancellationToken::new(),
                demand,
                _task: Task::ready(()),
            },
        );
    }

    assert_eq!(state.worst_speculative_preparation(), Some(8));
    state.cancel_active_preparation(8, true);
    assert!(!state.pages.contains_key(&8));
    assert!(state.preparation_demands.contains_key(&8));
    assert_eq!(state.preparation_cancelled, 1);
}

#[test]
fn terminal_pages_are_not_requeued_until_state_reset() {
    let mut state = GpuPdfState::default();
    state.terminal_pages.insert(3);
    state.set_preparation_demands(&[GpuPreparationTarget {
        page: 3,
        priority: 0,
        speculative: false,
    }]);
    assert!(!state.preparation_demands.contains_key(&3));

    state.reset();
    state.demand_page(3, false, 0);
    assert!(state.preparation_demands.contains_key(&3));
}

#[test]
fn viewport_intersection_maps_outward_to_page_pixels() {
    let image = Bounds::new(point(px(100.0), px(50.0)), size(px(500.0), px(1_000.0)));
    let viewport = Bounds::new(point(px(200.0), px(300.0)), size(px(300.0), px(400.0)));
    assert_eq!(
        visible_gpu_region(render_key(), image, viewport),
        Some(GpuPageRegion {
            x: 200,
            y: 500,
            width: 600,
            height: 800,
        })
    );
}

#[test]
fn clipped_surface_maps_back_to_its_page_destination() {
    let image = Bounds::new(point(px(100.0), px(50.0)), size(px(500.0), px(1_000.0)));
    let bounds = gpu_surface_bounds(
        render_key(),
        image,
        GpuPageRegion {
            x: 200,
            y: 500,
            width: 600,
            height: 800,
        },
    );
    assert_eq!(bounds.origin, point(px(200.0), px(300.0)));
    assert_eq!(bounds.size, size(px(300.0), px(400.0)));
}

#[test]
fn adjacent_page_warming_predicts_the_entering_viewport_edge() {
    let image = Bounds::new(point(px(100.0), px(100.0)), size(px(500.0), px(1_000.0)));
    let viewport = Bounds::new(point(px(0.0), px(0.0)), size(px(800.0), px(600.0)));

    let forward = adjacent_gpu_viewport_bounds(image, viewport, true);
    assert_eq!(forward.origin, point(px(0.0), px(100.0)));
    assert_eq!(
        visible_gpu_region(render_key(), image, forward),
        Some(GpuPageRegion {
            x: 0,
            y: 0,
            width: 1_000,
            height: 1_200,
        })
    );

    let backward = adjacent_gpu_viewport_bounds(image, viewport, false);
    assert_eq!(backward.origin, point(px(0.0), px(500.0)));
    assert_eq!(
        visible_gpu_region(render_key(), image, backward),
        Some(GpuPageRegion {
            x: 0,
            y: 800,
            width: 1_000,
            height: 1_200,
        })
    );
}

#[test]
fn adjacent_surface_intent_never_demands_preparation_or_starts_fallback_grace() {
    assert!(should_demand_missing_gpu_page(GpuSurfaceIntent::Visible));
    assert!(!should_demand_missing_gpu_page(
        GpuSurfaceIntent::AdjacentWarm
    ));
    assert!(should_start_gpu_fallback_grace(
        PdfRenderEngine::GlyphstFirst,
        GpuSurfaceIntent::Visible,
    ));
    assert!(!should_start_gpu_fallback_grace(
        PdfRenderEngine::GlyphstFirst,
        GpuSurfaceIntent::AdjacentWarm,
    ));
    assert!(!should_start_gpu_fallback_grace(
        PdfRenderEngine::Glyphst,
        GpuSurfaceIntent::Visible,
    ));
}

#[test]
fn adjacent_surface_warming_waits_for_visible_coverage_and_higher_priority_work() {
    assert!(should_warm_adjacent_surface(
        true, true, true, false, true, true, true,
    ));
    assert!(should_warm_adjacent_surface(
        false, true, false, false, true, true, true,
    ));
    assert!(!should_warm_adjacent_surface(
        false, true, true, false, true, true, true,
    ));
    assert!(!should_warm_adjacent_surface(
        true, false, false, false, true, true, true,
    ));
    assert!(!should_warm_adjacent_surface(
        true, true, false, true, true, true, true,
    ));
    assert!(!should_warm_adjacent_surface(
        true, true, false, false, false, true, true,
    ));
    assert!(!should_warm_adjacent_surface(
        true, true, false, false, true, false, true,
    ));
    assert!(!should_warm_adjacent_surface(
        true, true, false, false, true, true, false,
    ));
}

#[test]
fn adjacent_warming_uses_exact_density_settled_quality() {
    let key = render_key();
    let visible_region = GpuPageRegion {
        x: 0,
        y: 0,
        width: 1_000,
        height: 1_000,
    };
    assert!(
        !gpu_paint_request(
            key,
            visible_region,
            false,
            GpuPaintTargetPlan {
                pan_capacity: None,
                pan_axes: [true; 2],
                full_page_pan_axes: [false; 2],
                camera_capacity: None,
            },
            0,
            [0.0; 2],
        )
        .settled
    );
    assert!(
        gpu_paint_request(
            key,
            visible_region,
            true,
            GpuPaintTargetPlan {
                pan_capacity: None,
                pan_axes: [true; 2],
                full_page_pan_axes: [false; 2],
                camera_capacity: None,
            },
            0,
            [0.0; 2],
        )
        .settled
    );
}

#[test]
fn stable_target_capacity_is_invariant_under_reader_zoom() {
    let key = render_key();
    let image = Bounds::new(point(px(0.0), px(0.0)), size(px(500.0), px(1_000.0)));
    let viewport = Bounds::new(point(px(0.0), px(0.0)), size(px(800.0), px(600.0)));
    let capacity = stable_gpu_target_capacity(key, image, viewport, 8_192, [true; 2]).unwrap();
    assert_eq!(capacity, [2_816, 2_048]);
    assert_eq!(
        stable_gpu_target_capacity(key, image, viewport, 8_192, [false, true]),
        Some([1_792, 2_048]),
    );
    let camera_capacity = stable_gpu_camera_target_capacity(key, image, viewport, 8_192).unwrap();
    assert_eq!(camera_capacity, [1_664, 1_280]);

    let zoomed_key = RenderKey {
        physical_width: 2_000,
        physical_height: 4_000,
        ..key
    };
    let zoomed_image = Bounds::new(point(px(0.0), px(0.0)), size(px(1_000.0), px(2_000.0)));
    assert_eq!(
        stable_gpu_target_capacity(zoomed_key, zoomed_image, viewport, 8_192, [true; 2],),
        Some(capacity)
    );
    assert_eq!(
        stable_gpu_camera_target_capacity(zoomed_key, zoomed_image, viewport, 8_192),
        Some(camera_capacity)
    );
}

#[test]
fn expensive_pan_cache_promotes_only_full_page_axes_that_fit_the_ring_budget() {
    let mut preparation = GpuPreparationStats {
        motion_stroke_lod_segments: GPU_FULL_PAGE_CACHE_MIN_MOTION_SEGMENTS,
        ..GpuPreparationStats::default()
    };
    assert_eq!(
        bounded_full_page_pan_cache_axes(
            [4_000, 3_000],
            [1_792, 2_048],
            [1_536, 1_024],
            [false, true],
            preparation,
            8_192,
        ),
        [false, true],
    );
    assert_eq!(
        bounded_full_page_pan_cache_axes(
            [8_000, 8_000],
            [1_792, 2_048],
            [1_536, 1_024],
            [true; 2],
            preparation,
            8_192,
        ),
        [true, false],
        "the second full axis would exceed the bounded seven-target ring",
    );

    preparation.motion_stroke_lod_segments =
        GPU_FULL_PAGE_CACHE_MIN_MOTION_SEGMENTS.saturating_sub(1);
    assert_eq!(
        bounded_full_page_pan_cache_axes(
            [4_000, 3_000],
            [1_792, 2_048],
            [1_536, 1_024],
            [false, true],
            preparation,
            8_192,
        ),
        [false; 2],
        "ordinary pages retain the smaller directional cache",
    );
}

#[test]
fn pan_cache_overscan_tracks_the_active_translation_axes() {
    let visible = GpuPageRegion {
        x: 100,
        y: 200,
        width: 800,
        height: 600,
    };
    assert_eq!(directional_pan_cache_axes(None, visible), [false, true]);
    assert_eq!(
        directional_pan_cache_axes(Some((visible, [true, false])), visible),
        [true, false],
    );
    assert_eq!(
        directional_pan_cache_axes(
            Some((visible, [false, true])),
            GpuPageRegion { x: 101, ..visible },
        ),
        [false, true],
    );
    assert_eq!(
        directional_pan_cache_axes(
            Some((visible, [false, true])),
            GpuPageRegion { y: 220, ..visible },
        ),
        [false, true],
    );
    assert_eq!(
        directional_pan_cache_axes(
            Some((visible, [false, true])),
            GpuPageRegion { x: 120, ..visible },
        ),
        [true, false],
    );
    assert_eq!(
        directional_pan_cache_axes(
            Some((visible, [false, true])),
            GpuPageRegion {
                x: 120,
                y: 220,
                ..visible
            },
        ),
        [true, true],
    );
    assert_eq!(
        directional_pan_cache_axes(
            Some((visible, [false, true])),
            GpuPageRegion {
                x: 101,
                y: 220,
                ..visible
            },
        ),
        [false, true],
    );
}

#[test]
fn speculative_surface_budget_and_cache_pressure_never_poison_visible_routing() {
    assert!(speculative_surface_fits(100, 25, 125));
    assert!(!speculative_surface_fits(100, 26, 125));
    assert!(!speculative_surface_fits(u64::MAX, 1, u64::MAX));

    assert_eq!(
        surface_cache_pressure_action(true, false),
        SurfaceCachePressureAction::PreserveDiagnostics,
    );
    assert_eq!(
        surface_cache_pressure_action(true, true),
        SurfaceCachePressureAction::ClearDiagnostics,
    );
    assert_eq!(
        surface_cache_pressure_action(false, true),
        SurfaceCachePressureAction::ClearDiagnostics,
    );
    assert_eq!(
        surface_cache_pressure_action(false, false),
        SurfaceCachePressureAction::RouteToFallback,
    );
}

#[test]
fn prewarmed_hit_requires_compatible_scale_content_and_coverage() {
    let key = render_key();
    let request = gpu_paint_request(
        key,
        GpuPageRegion {
            x: 100,
            y: 400,
            width: 800,
            height: 800,
        },
        false,
        GpuPaintTargetPlan {
            pan_capacity: None,
            pan_axes: [true; 2],
            full_page_pan_axes: [false; 2],
            camera_capacity: None,
        },
        0,
        [0.0; 2],
    );
    let surface = request.surface_region().expect("warm surface region");
    assert!(prewarmed_surface_covers(
        true,
        key,
        surface,
        key,
        request.visible_region,
        true,
    ));
    assert!(!prewarmed_surface_covers(
        false,
        key,
        surface,
        key,
        request.visible_region,
        true,
    ));
    assert!(!prewarmed_surface_covers(
        true,
        key,
        surface,
        key,
        request.visible_region,
        false,
    ));

    let compatible_motion_key = RenderKey {
        physical_width: 1_100,
        physical_height: 2_200,
        ..key
    };
    assert!(prewarmed_surface_covers(
        true,
        key,
        surface,
        compatible_motion_key,
        request.visible_region,
        true,
    ));

    let mut scaled_key = key;
    scaled_key.physical_width = 2_000;
    assert!(!prewarmed_surface_covers(
        true,
        key,
        surface,
        scaled_key,
        request.visible_region,
        true,
    ));
    assert!(!prewarmed_surface_covers(
        true,
        key,
        GpuPageRegion {
            height: 128,
            ..surface
        },
        key,
        request.visible_region,
        true,
    ));
}

#[test]
fn visible_surface_request_supersedes_matching_speculative_work() {
    assert!(!can_reuse_active_surface_request(true, true, false));
    assert!(can_reuse_active_surface_request(true, true, true));
    assert!(can_reuse_active_surface_request(true, false, false));
    assert!(can_reuse_active_surface_request(true, false, true));
    assert!(!can_reuse_active_surface_request(false, false, false));
}

#[test]
fn sequential_surface_warming_keeps_exactly_one_page_of_lookahead() {
    let page_count = 8_u32;
    let mut prewarmed = None;
    let mut hits = 0_u32;
    let mut maximum_resident = 0_usize;

    for visible in 0..page_count {
        if prewarmed == Some(visible) {
            hits += 1;
            prewarmed = None;
        }
        if visible + 1 < page_count
            && should_warm_adjacent_surface(true, true, true, false, true, true, true)
        {
            assert!(prewarmed.is_none());
            prewarmed = Some(visible + 1);
        }
        maximum_resident = maximum_resident.max(usize::from(prewarmed.is_some()));
    }

    assert_eq!(hits, page_count - 1);
    assert_eq!(maximum_resident, 1);
}

#[test]
fn cold_visible_pages_are_not_starved_during_motion() {
    assert!(should_start_surface_seed(false, false, false));
    assert!(!should_start_surface_seed(false, false, true));
    assert!(should_start_surface_seed(false, true, true));
    assert!(should_start_surface_seed(true, false, true));

    assert!(!should_defer_full_surface_render(false, false, false));
    assert!(should_defer_full_surface_render(false, false, true));
    assert!(!should_defer_full_surface_render(false, true, true));
    assert!(!should_defer_full_surface_render(true, false, true));
}

#[test]
fn surface_containment_is_edge_inclusive_and_overflow_safe() {
    let surface = GpuPageRegion {
        x: 100,
        y: 200,
        width: 400,
        height: 600,
    };
    assert!(gpu_region_contains(surface, surface));
    assert!(gpu_region_contains(
        surface,
        GpuPageRegion {
            x: 150,
            y: 250,
            width: 300,
            height: 500,
        }
    ));
    assert!(!gpu_region_contains(
        surface,
        GpuPageRegion { x: 99, ..surface }
    ));
    assert!(!gpu_region_contains(
        surface,
        GpuPageRegion {
            x: 450,
            width: 51,
            ..surface
        }
    ));

    let overflow_edge = GpuPageRegion {
        x: u32::MAX - 50,
        y: u32::MAX - 50,
        width: 100,
        height: 100,
    };
    assert!(gpu_region_contains(
        overflow_edge,
        GpuPageRegion {
            x: u32::MAX - 40,
            y: u32::MAX - 40,
            width: 20,
            height: 20,
        }
    ));
}

#[test]
fn surface_refresh_guard_uses_half_of_each_available_margin() {
    let surface = GpuPageRegion {
        x: 100,
        y: 200,
        width: 1_000,
        height: 800,
    };
    let visible = GpuPageRegion {
        x: 300,
        y: 300,
        width: 400,
        height: 400,
    };
    assert_eq!(
        surface_refresh_guard(surface, visible),
        GpuPageRegion {
            x: 200,
            y: 250,
            width: 700,
            height: 600,
        }
    );

    let page_edge_surface = GpuPageRegion {
        x: 0,
        y: 0,
        width: 800,
        height: 900,
    };
    let page_edge_visible = GpuPageRegion {
        x: 0,
        y: 100,
        width: 500,
        height: 500,
    };
    assert_eq!(
        surface_refresh_guard(page_edge_surface, page_edge_visible),
        GpuPageRegion {
            x: 0,
            y: 50,
            width: 650,
            height: 700,
        },
        "page edges have no artificial margin to inset"
    );
    assert_eq!(
        surface_refresh_guard(visible, visible),
        visible,
        "a budget-tight visible-only surface has a zero-width gutter"
    );

    let overflow_surface = GpuPageRegion {
        x: u32::MAX - 100,
        y: u32::MAX - 100,
        width: 100,
        height: 100,
    };
    let overflow_visible = GpuPageRegion {
        x: u32::MAX - 80,
        y: u32::MAX - 60,
        width: 40,
        height: 20,
    };
    assert_eq!(
        surface_refresh_guard(overflow_surface, overflow_visible),
        GpuPageRegion {
            x: u32::MAX - 90,
            y: u32::MAX - 80,
            width: 70,
            height: 60,
        }
    );
}

#[test]
fn overscan_refresh_trajectory_stays_covered_with_few_submissions() {
    let mut request = GpuPaintRequest {
        page_size: [1_000, 6_000],
        visible_region: GpuPageRegion {
            x: 0,
            y: 500,
            width: 1_000,
            height: 800,
        },
        target_capacity: None,
        pan_cache_axes: [false, true],
        full_page_pan_cache_axes: [false; 2],
        camera_target_capacity: None,
        camera_serial: 1,
        device_origin_phase: [0.0; 2],
        rotation: DocumentRotation::Degrees0,
        background: [1.0, 1.0, 1.0, 1.0],
        settled: false,
        generation: 1,
    };
    let mut surface = request.surface_region().expect("initial surface region");
    let mut guard = surface_refresh_guard(surface, request.visible_region);
    let mut submissions = 1_u32;
    let mut uncovered_frames = 0_u32;

    for frame in 1..=120_u32 {
        request.visible_region.y = 500 + frame * 8;
        if !gpu_region_contains(surface, request.visible_region) {
            uncovered_frames += 1;
        }
        if !gpu_region_contains(guard, request.visible_region) {
            let next_surface = request.surface_region().expect("shifted surface region");
            if next_surface != surface {
                surface = next_surface;
                guard = surface_refresh_guard(surface, request.visible_region);
                submissions += 1;
            }
        }
    }

    assert_eq!(uncovered_frames, 0);
    assert!(
        submissions * 4 < 120,
        "{submissions} submissions should remain below 25% of paint frames"
    );
}

#[test]
fn surface_request_coalescing_tracks_quality_and_visible_coverage() {
    let request = GpuPaintRequest {
        page_size: [1_000, 2_000],
        visible_region: GpuPageRegion {
            x: 10,
            y: 20,
            width: 800,
            height: 1_200,
        },
        target_capacity: None,
        pan_cache_axes: [true; 2],
        full_page_pan_cache_axes: [false; 2],
        camera_target_capacity: None,
        camera_serial: 1,
        device_origin_phase: [0.0; 2],
        rotation: DocumentRotation::Degrees0,
        background: [1.0, 1.0, 1.0, 1.0],
        settled: false,
        generation: 1,
    };
    let region = request.surface_region().expect("surface region");
    let vertical_full = GpuPaintRequest {
        pan_cache_axes: [false, true],
        full_page_pan_cache_axes: [false, true],
        ..request
    };
    assert!(full_page_pan_cache_covers_axes(
        vertical_full,
        [false, true],
    ));
    assert!(!full_page_pan_cache_covers_axes(
        vertical_full,
        [true, false],
    ));
    let shifted_request = GpuPaintRequest {
        visible_region: GpuPageRegion {
            y: 21,
            ..request.visible_region
        },
        generation: 2,
        ..request
    };
    assert!(same_gpu_surface_content(
        request,
        GpuPaintRequest {
            generation: 2,
            camera_serial: 2,
            ..request
        }
    ));
    assert!(same_gpu_surface_content(request, shifted_request));
    assert!(same_gpu_surface_work(request, shifted_request));

    let key = render_key();
    let identity = SurfaceRequestIdentity::new(key, request, region);
    let shifted_identity = SurfaceRequestIdentity::new(key, shifted_request, region);
    assert!(surface_request_covers(identity, shifted_identity));
    assert!(rendered_surface_covers_request(identity, shifted_identity));
    assert!(can_coalesce_pending_surface_request(
        identity,
        false,
        shifted_identity,
        false,
    ));
    assert!(!can_coalesce_pending_surface_request(
        identity, true, identity, false,
    ));
    assert!(surface_request_covers(
        identity,
        SurfaceRequestIdentity::new(
            key,
            shifted_request,
            GpuPageRegion {
                y: region.y + 128,
                ..region
            },
        ),
    ));
    assert!(!rendered_surface_covers_request(
        identity,
        SurfaceRequestIdentity::new(
            key,
            shifted_request,
            GpuPageRegion {
                y: region.y + 128,
                ..region
            },
        ),
    ));
    assert!(!surface_request_covers(
        identity,
        SurfaceRequestIdentity::new(
            key,
            GpuPaintRequest {
                settled: true,
                ..request
            },
            region,
        ),
    ));
    assert!(!same_gpu_surface_content(
        request,
        GpuPaintRequest {
            device_origin_phase: [0.25, 0.0],
            ..request
        }
    ));
    assert!(!same_gpu_surface_content(
        request,
        GpuPaintRequest {
            background: [0.0, 0.0, 0.0, 1.0],
            ..request
        }
    ));
    assert!(!same_gpu_surface_content(
        request,
        GpuPaintRequest {
            rotation: DocumentRotation::Degrees90,
            ..request
        }
    ));

    let mut scaled_key = key;
    scaled_key.physical_width = 2_000;
    assert!(!surface_request_covers(
        identity,
        SurfaceRequestIdentity::new(
            scaled_key,
            GpuPaintRequest {
                page_size: [2_000, 2_000],
                ..request
            },
            region,
        ),
    ));

    let outside_request = GpuPaintRequest {
        visible_region: GpuPageRegion {
            x: region.right(),
            y: region.y,
            width: 1,
            height: 1,
        },
        ..request
    };
    assert!(!surface_request_covers(
        identity,
        SurfaceRequestIdentity::new(key, outside_request, region),
    ));
}

#[test]
fn material_surface_work_receives_the_next_nonzero_generation() {
    let mut request = GpuPaintRequest {
        page_size: [1_000, 2_000],
        visible_region: GpuPageRegion {
            x: 0,
            y: 0,
            width: 1_000,
            height: 1_000,
        },
        target_capacity: None,
        pan_cache_axes: [true; 2],
        full_page_pan_cache_axes: [false; 2],
        camera_target_capacity: None,
        camera_serial: 0,
        device_origin_phase: [0.0; 2],
        rotation: DocumentRotation::Degrees0,
        background: [1.0, 1.0, 1.0, 1.0],
        settled: false,
        generation: 0,
    };
    let mut counter = 0;

    assign_surface_render_generation(&mut counter, &mut request);
    assert_eq!(counter, 1);
    assert_eq!(request.generation, 1);

    counter = u64::MAX;
    assign_surface_render_generation(&mut counter, &mut request);
    assert_eq!(counter, 1);
    assert_eq!(request.generation, 1);
}

#[test]
fn primary_surface_work_precedes_continuity_then_stays_fifo() {
    let mut priorities = [
        pending_surface_priority(8, SurfaceRenderRole::Continuity, true, 1),
        pending_surface_priority(4, SurfaceRenderRole::Continuity, false, 3),
        pending_surface_priority(3, SurfaceRenderRole::Primary, false, 2),
        pending_surface_priority(2, SurfaceRenderRole::Primary, true, 0),
    ];
    priorities.sort_unstable();

    assert_eq!(
        priorities,
        [
            pending_surface_priority(3, SurfaceRenderRole::Primary, false, 2),
            pending_surface_priority(4, SurfaceRenderRole::Continuity, false, 3),
            pending_surface_priority(2, SurfaceRenderRole::Primary, true, 0),
            pending_surface_priority(8, SurfaceRenderRole::Continuity, true, 1),
        ]
    );
}

#[test]
fn surface_latency_samples_are_bounded_and_report_nearest_rank_p95() {
    let mut samples = VecDeque::new();
    for millis in 0..=MAX_SURFACE_LATENCY_SAMPLES {
        push_surface_latency(&mut samples, Duration::from_millis(millis as u64));
    }

    assert_eq!(samples.len(), MAX_SURFACE_LATENCY_SAMPLES);
    assert_eq!(samples.front(), Some(&Duration::from_millis(1)));
    assert_eq!(samples.back(), Some(&Duration::from_millis(120)));
    assert_eq!(surface_latency_p95(&samples), Duration::from_millis(114));
    assert_eq!(surface_latency_p95(&VecDeque::new()), Duration::ZERO);
}

#[test]
fn backend_modes_keep_strict_pages_native_and_delay_compatibility_fallback() {
    assert!(mupdf_placeholder_allowed(
        PdfRenderEngine::MuPdf,
        false,
        false
    ));
    assert!(!mupdf_placeholder_allowed(
        PdfRenderEngine::Glyphst,
        true,
        true
    ));
    assert!(!mupdf_placeholder_allowed(
        PdfRenderEngine::GlyphstFirst,
        false,
        false
    ));
    assert!(mupdf_placeholder_allowed(
        PdfRenderEngine::GlyphstFirst,
        false,
        true
    ));
    assert!(mupdf_placeholder_allowed(
        PdfRenderEngine::GlyphstFirst,
        true,
        false
    ));
}
