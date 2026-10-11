use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime},
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use glyphst_ai::{
    agent::{AgentCancellationToken, AgentToolCall, AgentToolResultPart},
    ai_chat::DocumentContextScope,
};
use glyphst_ai_ui::AiContextProvider as _;
use glyphst_pdf::DocumentPassword;
use glyphst_translation_ui::DocumentTranslationOpenMode;
use gpui::{
    AppContext as _, EntityId, ExternalPaths, FileDropEvent, Modifiers, MouseButton, MouseUpEvent,
    TestAppContext, bounds, point, px, size,
};
use gpui_component::{Root, WindowExt as _};

use super::{
    PaneEvent, SplitDirection, SynchronizedConnectionAnchor, SynchronizedConnectionAnchorEdge,
    SynchronizedConnectionCurve, SynchronizedConnectionGroup, SynchronizedConnectionInvalidReason,
    SynchronizedConnectionTabState, ToggleRenderingDebug, ToggleZenMode, Workspace,
    WorkspaceAiContextProvider, WorkspaceCallbacks, WorkspaceMenu, WorkspaceSurfaceMode,
    WorkspaceWindowHandle, WorkspaceWindowRequest, live_translation_pair_is_active,
    open_document_options,
};
use crate::{
    document_view::{
        DocumentSidebar, RenderCache, SynchronizedScrollDelta, SynchronizedScrollMode,
        ToggleBookmarkSidebar,
    },
    library::{
        DocumentPosition, LibraryStore, ReaderNewTabSession, ReaderPaneSession, ReaderPaneTree,
        ReaderSidebar, ReaderSplitAxis, ReaderSynchronizedGroup, ReaderTabKind, ReaderTabSession,
        ReaderTranslationSession, ReaderWindowBounds, ReaderWindowSession, ReaderWindowState,
    },
    pdf_password_store::PdfPasswordStore,
    settings::SettingsStore,
};

fn unavailable_workspace_window(
    _: WorkspaceWindowRequest,
    _: &mut gpui::App,
) -> anyhow::Result<WorkspaceWindowHandle> {
    anyhow::bail!("not available in tests")
}

fn ignore_settings_window(_: &mut gpui::App) {}

#[test]
fn live_translation_pair_requires_both_selected_tabs() {
    let source = EntityId::from(1001);
    let translated = EntityId::from(1002);
    assert!(!live_translation_pair_is_active(
        source,
        translated,
        &HashSet::new()
    ));
    assert!(!live_translation_pair_is_active(
        source,
        translated,
        &HashSet::from([translated])
    ));
    assert!(live_translation_pair_is_active(
        source,
        translated,
        &HashSet::from([source, translated])
    ));
}

fn temporary_pdf_path(label: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "glyphst-workspace-{label}-{}-{}.pdf",
        std::process::id(),
        SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

#[gpui::test]
fn bookmark_sidebar_action_targets_the_current_document(cx: &mut TestAppContext) {
    let document_path = temporary_pdf_path("bookmarks");
    write_minimal_pdf(&document_path);
    cx.update(|cx| {
        gpui_component::init(cx);
        crate::keybindings::capture_framework_bindings(cx);
        crate::keybindings::apply(cx, glyphst_settings::KeymapPreset::Modern, &[]);
        cx.set_reduce_motion(true);
        cx.set_global(SettingsStore::memory());
        cx.set_global(LibraryStore::memory());
        cx.set_global(RenderCache::new());
    });
    let callbacks = WorkspaceCallbacks {
        open_workspace_window: unavailable_workspace_window,
        open_settings_window: ignore_settings_window,
    };
    let initial_path = document_path.clone();
    let (root, cx) = cx.add_window_view(move |window, cx| {
        let workspace =
            cx.new(|cx| Workspace::new(Some(initial_path.clone()), None, callbacks, window, cx));
        Root::new(workspace, window, cx)
    });
    let workspace = root.read_with(cx, |root, _| {
        root.view().clone().downcast::<Workspace>().unwrap()
    });
    cx.run_until_parked();

    cx.update(|window, cx| {
        window.dispatch_action(Box::new(ToggleBookmarkSidebar), cx);
        _ = window.draw(cx);
    });
    workspace.read_with(cx, |workspace, _| {
        assert_eq!(
            workspace.docks.left_sidebar,
            Some(DocumentSidebar::Bookmarks)
        );
    });
    assert!(cx.debug_bounds("bookmarks-sidebar-content").is_some());

    fs::remove_file(document_path).unwrap();
}

#[gpui::test]
fn document_footer_reserves_layout_and_exposes_progress_and_bookmark_actions(
    cx: &mut TestAppContext,
) {
    let document_path = temporary_pdf_path("document-footer");
    let settings_path = document_path.with_extension("settings.kdl");
    let library_path = document_path.with_extension("library.redb");
    let managed_library_path = library_path.with_extension("managed-library");
    write_minimal_pdf_with_height(&document_path, 5_000);
    cx.update(|cx| {
        gpui_component::init(cx);
        crate::keybindings::capture_framework_bindings(cx);
        crate::keybindings::apply(cx, glyphst_settings::KeymapPreset::Modern, &[]);
        cx.set_reduce_motion(true);
        let mut settings = SettingsStore::memory();
        settings.config_path = settings_path.clone();
        cx.set_global(settings);
        cx.set_global(LibraryStore::open_for_test(library_path.clone()).unwrap());
        cx.set_global(RenderCache::new());
    });
    let callbacks = WorkspaceCallbacks {
        open_workspace_window: unavailable_workspace_window,
        open_settings_window: ignore_settings_window,
    };
    let initial_path = document_path.clone();
    let (_root, cx) = cx.add_window_view(move |window, cx| {
        let workspace =
            cx.new(|cx| Workspace::new(Some(initial_path.clone()), None, callbacks, window, cx));
        Root::new(workspace, window, cx)
    });
    cx.run_until_parked();
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });

    let root = cx.debug_bounds("document-view-root").unwrap();
    let viewport = cx.debug_bounds("document-viewport").unwrap();
    let footer = cx.debug_bounds("document-footer").unwrap();
    assert_eq!(footer.size.height, px(32.0));
    assert_eq!(viewport.top(), root.top());
    assert_eq!(viewport.bottom(), footer.top());
    assert_eq!(footer.bottom(), root.bottom());
    assert!(cx.debug_bounds("document-toolbar-hover-zone").is_none());

    cx.update(|window, cx| {
        window.dispatch_action(
            Box::new(glyphst_actions::document_viewer::TogglePresentationMode),
            cx,
        );
        _ = window.draw(cx);
    });
    assert!(cx.debug_bounds("document-footer").is_none());
    cx.update(|window, cx| {
        window.dispatch_action(
            Box::new(glyphst_actions::document_viewer::TogglePresentationMode),
            cx,
        );
        _ = window.draw(cx);
    });
    assert!(cx.debug_bounds("document-footer").is_some());

    let progress = cx.debug_bounds("document-footer-progress").unwrap();
    cx.simulate_mouse_down(progress.center(), MouseButton::Right, Modifiers::default());
    cx.run_until_parked();
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    cx.simulate_mouse_up(progress.center(), MouseButton::Right, Modifiers::default());
    cx.simulate_keystrokes("down down enter");
    cx.run_until_parked();
    cx.update(|window, cx| {
        _ = window.draw(cx);
        assert_eq!(
            cx.global::<SettingsStore>()
                .settings
                .document_view
                .progress_display,
            glyphst_settings::DocumentProgressDisplay::Percentage
        );
    });

    let progress = cx.debug_bounds("document-footer-progress").unwrap();
    cx.simulate_click(progress.center(), Modifiers::default());
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    assert!(cx.debug_bounds("picker-input-row").is_some());
    cx.simulate_keystrokes("escape");
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });

    let bookmark = cx.debug_bounds("document-footer-bookmark").unwrap();
    cx.simulate_mouse_down(bookmark.center(), MouseButton::Right, Modifiers::default());
    cx.run_until_parked();
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    assert!(cx.debug_bounds("bookmark-title-editor").is_some());
    cx.update(|_, cx| {
        assert!(
            cx.global::<LibraryStore>()
                .bookmarks_for(&document_path)
                .is_empty()
        );
    });
    cx.simulate_mouse_up(bookmark.center(), MouseButton::Right, Modifiers::default());
    cx.simulate_keystrokes("escape");
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });

    cx.simulate_click(bookmark.center(), Modifiers::default());
    cx.update(|window, cx| {
        _ = window.draw(cx);
        assert_eq!(
            cx.global::<LibraryStore>()
                .bookmarks_for(&document_path)
                .len(),
            1
        );
    });
    cx.simulate_mouse_down(bookmark.center(), MouseButton::Right, Modifiers::default());
    cx.run_until_parked();
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    assert!(cx.debug_bounds("bookmark-title-editor").is_some());

    fs::remove_file(document_path).unwrap();
    fs::remove_file(settings_path).unwrap();
    cx.update(|_, cx| cx.set_global(LibraryStore::memory()));
    fs::remove_file(library_path).unwrap();
    fs::remove_dir_all(managed_library_path).unwrap();
}

fn write_minimal_pdf_with_height(path: &Path, height: u32) {
    write_minimal_pdf_pages(path, height, 1);
}

fn write_minimal_pdf_pages(path: &Path, height: u32, page_count: usize) {
    let kids = (4..4 + page_count)
        .map(|id| format!("{id} 0 R"))
        .collect::<Vec<_>>()
        .join(" ");
    let mut objects = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        format!("<< /Type /Pages /Kids [{kids}] /Count {page_count} >>"),
        "<< /Length 0 >>\nstream\n\nendstream".to_owned(),
    ];
    objects.extend((0..page_count).map(|_| format!(
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 {height}] /Resources << >> /Contents 3 0 R >>"
    )));
    let mut bytes = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::with_capacity(objects.len());
    for (index, object) in objects.iter().enumerate() {
        offsets.push(bytes.len());
        bytes.extend_from_slice(format!("{} 0 obj\n{object}\nendobj\n", index + 1).as_bytes());
    }
    let xref = bytes.len();
    bytes.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    for offset in offsets {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    fs::write(path, bytes).unwrap();
}

fn write_minimal_pdf(path: &Path) {
    write_minimal_pdf_with_height(path, 200);
}

fn write_encrypted_pdf(path: &Path) {
    let encoded = include_str!("../../../pdf/tests/fixtures/dummy-encrypted.pdf.b64")
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>();
    fs::write(path, STANDARD.decode(encoded).unwrap()).unwrap();
}

fn encrypted_translation_reader_session(path: &Path) -> ReaderWindowSession {
    let document_tab = |id, zoom| ReaderTabSession {
        id,
        kind: ReaderTabKind::Document {
            path: path.to_path_buf(),
            position: DocumentPosition::fixed(0.0, 0.0, zoom, 1),
        },
    };
    ReaderWindowSession {
        bounds: ReaderWindowBounds {
            x: 0.0,
            y: 0.0,
            width: 1200.0,
            height: 800.0,
            state: ReaderWindowState::Windowed,
        },
        pane_tree: ReaderPaneTree::Split {
            axis: ReaderSplitAxis::Horizontal,
            ratios: vec![0.5, 0.5],
            children: vec![
                ReaderPaneTree::Pane(ReaderPaneSession {
                    id: 1,
                    tabs: vec![document_tab(1, 1.0)],
                    active_tab: Some(1),
                }),
                ReaderPaneTree::Pane(ReaderPaneSession {
                    id: 2,
                    tabs: vec![document_tab(2, 1.5)],
                    active_tab: Some(2),
                }),
            ],
        },
        active_pane: 1,
        dock_document_tab: Some(1),
        left_sidebar: None,
        ai_panel_open: false,
        transparency_enabled: true,
        synchronized_groups: Vec::new(),
        translation_sessions: vec![ReaderTranslationSession {
            source_tab: 1,
            translated_tab: 2,
            source_language: Some("en".to_string()),
            target_language: "zh-hans".to_string(),
            mode: "auto".to_string(),
            cache_key: None,
            original_unit_ids: vec!["encrypted-unit".to_string()],
        }],
    }
}

#[test]
fn synchronized_connection_anchors_use_facing_edge_midpoints() {
    let tab = bounds(point(px(10.0), px(20.0)), size(px(100.0), px(30.0)));
    let cases = [
        (
            point(px(500.0), px(35.0)),
            point(px(110.0), px(35.0)),
            SynchronizedConnectionAnchorEdge::Right,
        ),
        (
            point(px(-500.0), px(35.0)),
            point(px(10.0), px(35.0)),
            SynchronizedConnectionAnchorEdge::Left,
        ),
        (
            point(px(60.0), px(500.0)),
            point(px(60.0), px(50.0)),
            SynchronizedConnectionAnchorEdge::Bottom,
        ),
        (
            point(px(60.0), px(-500.0)),
            point(px(60.0), px(20.0)),
            SynchronizedConnectionAnchorEdge::Top,
        ),
    ];

    for (toward, expected_position, expected_edge) in cases {
        let anchor = Workspace::synchronized_connection_anchor(tab, toward);
        assert_eq!(anchor.position, expected_position);
        assert_eq!(anchor.edge, expected_edge);
    }
}

#[test]
fn synchronized_connection_curves_leave_handles_perpendicularly() {
    let now = Instant::now();
    let horizontal = Workspace::synchronized_connection_curve(
        bounds(point(px(0.0), px(10.0)), size(px(80.0), px(30.0))),
        bounds(point(px(220.0), px(70.0)), size(px(80.0), px(30.0))),
        now,
    );
    assert_eq!(
        horizontal.start.edge,
        SynchronizedConnectionAnchorEdge::Right
    );
    assert_eq!(horizontal.end.edge, SynchronizedConnectionAnchorEdge::Left);
    assert_eq!(horizontal.control_a.y, horizontal.start.position.y);
    assert!(horizontal.control_a.x > horizontal.start.position.x);
    assert_eq!(horizontal.control_b.y, horizontal.end.position.y);
    assert!(horizontal.control_b.x < horizontal.end.position.x);

    let vertical = Workspace::synchronized_connection_curve(
        bounds(point(px(20.0), px(0.0)), size(px(80.0), px(30.0))),
        bounds(point(px(100.0), px(220.0)), size(px(80.0), px(30.0))),
        now,
    );
    assert_eq!(
        vertical.start.edge,
        SynchronizedConnectionAnchorEdge::Bottom
    );
    assert_eq!(vertical.end.edge, SynchronizedConnectionAnchorEdge::Top);
    assert_eq!(vertical.control_a.x, vertical.start.position.x);
    assert!(vertical.control_a.y > vertical.start.position.y);
    assert_eq!(vertical.control_b.x, vertical.end.position.x);
    assert!(vertical.control_b.y < vertical.end.position.y);
}

#[test]
fn synchronized_connection_dots_stay_between_endpoint_handles() {
    for dot_count in [2, 7, 120] {
        let parameters = (0..dot_count)
            .map(|index| Workspace::synchronized_connection_dot_parameter(index, dot_count))
            .collect::<Vec<_>>();
        assert!(parameters.first().is_some_and(|parameter| *parameter > 0.0));
        assert!(parameters.last().is_some_and(|parameter| *parameter < 1.0));
        assert!(parameters.windows(2).all(|pair| pair[0] < pair[1]));
    }
}

#[test]
fn synchronized_connection_handles_deduplicate_shared_endpoints() {
    let now = Instant::now();
    let earlier = now - Duration::from_millis(40);
    let shared = SynchronizedConnectionAnchor {
        position: point(px(80.0), px(25.0)),
        edge: SynchronizedConnectionAnchorEdge::Right,
    };
    let upper = SynchronizedConnectionAnchor {
        position: point(px(220.0), px(15.0)),
        edge: SynchronizedConnectionAnchorEdge::Left,
    };
    let lower = SynchronizedConnectionAnchor {
        position: point(px(220.0), px(85.0)),
        edge: SynchronizedConnectionAnchorEdge::Left,
    };
    let curves = [
        SynchronizedConnectionCurve {
            start: shared,
            control_a: point(px(120.0), px(25.0)),
            control_b: point(px(180.0), px(15.0)),
            end: upper,
            added_at: now,
        },
        SynchronizedConnectionCurve {
            start: shared,
            control_a: point(px(120.0), px(25.0)),
            control_b: point(px(180.0), px(85.0)),
            end: lower,
            added_at: earlier,
        },
    ];

    let handles = Workspace::synchronized_connection_handles(&curves);
    assert_eq!(handles.len(), 3);
    let shared_handle = handles
        .iter()
        .find(|handle| handle.center == shared.position)
        .unwrap();
    assert_eq!(shared_handle.added_at, earlier);
}

#[gpui::test]
fn documentless_workspace_starts_on_home_with_a_backing_new_tab(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_component::init(cx);
        cx.set_global(SettingsStore::memory());
        cx.set_global(LibraryStore::memory());
        cx.set_global(RenderCache::new());
    });
    let callbacks = WorkspaceCallbacks {
        open_workspace_window: unavailable_workspace_window,
        open_settings_window: ignore_settings_window,
    };

    let (root, cx) = cx.add_window_view(|window, cx| {
        let workspace = cx.new(|cx| Workspace::new(None, None, callbacks, window, cx));
        Root::new(workspace, window, cx)
    });
    let workspace = root.read_with(cx, |root, _| {
        root.view().clone().downcast::<Workspace>().unwrap()
    });

    workspace.read_with(cx, |workspace, cx| {
        assert_eq!(workspace.surface_mode, WorkspaceSurfaceMode::Home);
        assert!(workspace.active_pane.read(cx).active_tab_is_new_tab());
    });
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    assert!(cx.debug_bounds("home-view").is_some());
    assert!(cx.debug_bounds("home-navigation-rail").is_some());
    assert!(cx.debug_bounds("workspace-home").is_some());
    assert!(cx.debug_bounds("workspace-pane-column").is_none());
}

#[gpui::test]
fn rendering_debug_toggles_a_transient_monitor_on_home(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_component::init(cx);
        cx.set_global(SettingsStore::memory());
        cx.set_global(LibraryStore::memory());
        cx.set_global(RenderCache::new());
    });
    let callbacks = WorkspaceCallbacks {
        open_workspace_window: unavailable_workspace_window,
        open_settings_window: ignore_settings_window,
    };
    let (root, cx) = cx.add_window_view(|window, cx| {
        let workspace = cx.new(|cx| Workspace::new(None, None, callbacks, window, cx));
        Root::new(workspace, window, cx)
    });
    let workspace = root.read_with(cx, |root, _| {
        root.view().clone().downcast::<Workspace>().unwrap()
    });

    cx.update(|window, cx| {
        window.dispatch_action(Box::new(ToggleRenderingDebug), cx);
    });
    cx.run_until_parked();
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    workspace.read_with(cx, |workspace, _| {
        assert!(workspace.rendering_debug.is_some());
    });
    workspace.update_in(cx, |workspace, window, cx| {
        workspace.start_render_debug_capture(window, cx);
        assert!(workspace.rendering_debug_capture.is_none());
    });
    cx.update(|window, cx| {
        assert_eq!(window.notifications(cx).len(), 1);
    });

    cx.update(|window, cx| {
        window.dispatch_action(Box::new(ToggleRenderingDebug), cx);
    });
    cx.run_until_parked();
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    workspace.read_with(cx, |workspace, _| {
        assert!(workspace.rendering_debug.is_none());
    });
}

#[gpui::test]
fn rendering_debug_capture_copies_a_versioned_pdf_report(cx: &mut TestAppContext) {
    let document_path = temporary_pdf_path("render-debug-capture");
    write_minimal_pdf_pages(&document_path, 100, 2);
    cx.update(|cx| {
        gpui_component::init(cx);
        cx.set_reduce_motion(true);
        cx.set_global(SettingsStore::memory());
        cx.set_global(LibraryStore::memory());
        cx.set_global(RenderCache::new());
    });
    let callbacks = WorkspaceCallbacks {
        open_workspace_window: unavailable_workspace_window,
        open_settings_window: ignore_settings_window,
    };
    let initial_path = document_path.clone();
    let (root, cx) = cx.add_window_view(move |window, cx| {
        let workspace =
            cx.new(|cx| Workspace::new(Some(initial_path.clone()), None, callbacks, window, cx));
        Root::new(workspace, window, cx)
    });
    let workspace = root.read_with(cx, |root, _| {
        root.view().clone().downcast::<Workspace>().unwrap()
    });
    cx.run_until_parked();

    workspace.update_in(cx, |workspace, window, cx| {
        workspace
            .active_render_debug_document(cx)
            .unwrap()
            .update(cx, |document, cx| {
                for _ in 0..4 {
                    document.zoom_out(cx);
                }
            });
        workspace.toggle_rendering_debug(window, cx);
    });
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    let capture_button = cx
        .debug_bounds("render-debug-capture")
        .expect("rendering capture control");
    cx.simulate_click(capture_button.center(), Modifiers::default());
    workspace.read_with(cx, |workspace, _| {
        assert!(workspace.rendering_debug_capture.is_some());
    });
    for _ in 0..3 {
        cx.update(|window, cx| {
            window.refresh();
            _ = window.draw(cx);
        });
    }
    let capture_button = cx
        .debug_bounds("render-debug-capture")
        .expect("expanded rendering capture control");
    cx.simulate_click(capture_button.center(), Modifiers::default());
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });

    let report_text = cx
        .read_from_clipboard()
        .and_then(|clipboard| clipboard.text())
        .expect("rendering capture JSON on clipboard");
    let report: serde_json::Value = serde_json::from_str(&report_text).unwrap();
    assert_eq!(report["schema_version"], 20);
    assert_eq!(report["kind"], "glyphst-reader-render-capture");
    assert_eq!(
        report["document"]["start"]["source_path"],
        document_path.to_string_lossy().as_ref()
    );
    assert!(report["frames"]["total_frames"].as_u64().unwrap() >= 1);
    assert_eq!(
        report["document"]["end"]["pages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|page| page["page_number"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        vec![1, 2],
        "the capture must include visible neighbors outside the center page range"
    );
    workspace.read_with(cx, |workspace, _| {
        assert!(workspace.rendering_debug_capture.is_none());
    });
    assert!(
        cx.debug_bounds("render-debug-capture").is_some(),
        "the capture control must stop the parent HUD click from collapsing it"
    );

    workspace.update_in(cx, |workspace, window, cx| {
        workspace.start_render_debug_capture(window, cx);
        assert!(workspace.rendering_debug_capture.is_some());
        workspace.toggle_rendering_debug(window, cx);
        assert!(workspace.rendering_debug_capture.is_none());
        assert!(workspace.rendering_debug.is_none());
    });
    assert_eq!(
        cx.read_from_clipboard()
            .and_then(|clipboard| clipboard.text())
            .as_deref(),
        Some(report_text.as_str())
    );

    fs::remove_file(document_path).unwrap();
}

#[gpui::test]
fn zoom_actions_target_the_active_pane_and_show_a_local_transient_popup(cx: &mut TestAppContext) {
    let document_path = temporary_pdf_path("active-pane-zoom");
    write_minimal_pdf(&document_path);
    cx.update(|cx| {
        gpui_component::init(cx);
        crate::keybindings::capture_framework_bindings(cx);
        crate::keybindings::apply(cx, glyphst_settings::KeymapPreset::Modern, &[]);
        cx.set_reduce_motion(true);
        cx.set_global(SettingsStore::memory());
        cx.set_global(LibraryStore::memory());
        cx.set_global(RenderCache::new());
    });
    let callbacks = WorkspaceCallbacks {
        open_workspace_window: unavailable_workspace_window,
        open_settings_window: ignore_settings_window,
    };
    let initial_path = document_path.clone();
    let (root, cx) = cx.add_window_view(move |window, cx| {
        let workspace =
            cx.new(|cx| Workspace::new(Some(initial_path.clone()), None, callbacks, window, cx));
        Root::new(workspace, window, cx)
    });
    let workspace = root.read_with(cx, |root, _| {
        root.view().clone().downcast::<Workspace>().unwrap()
    });

    cx.run_until_parked();
    workspace.update_in(cx, |workspace, window, cx| {
        workspace.split_active_pane(SplitDirection::Right, window, cx);
    });
    cx.run_until_parked();

    let (left_pane, right_pane, left_document, right_document, zoom_step) =
        workspace.update_in(cx, |workspace, window, cx| {
            let panes = workspace.pane_group.panes();
            let left_pane = panes[0].clone();
            let right_pane = panes[1].clone();
            let left_document = left_pane.read(cx).active_document_view().unwrap();
            let right_document = right_pane.read(cx).active_document_view().unwrap();

            // Keep descendant focus on the right to reproduce the stale focus path that
            // previously consumed document actions, while the workspace source of truth is
            // the left pane.
            right_pane.update(cx, |pane, cx| pane.focus(window, cx));
            workspace.active_pane = left_pane.clone();
            cx.notify();

            (
                left_pane,
                right_pane,
                left_document,
                right_document,
                cx.global::<SettingsStore>().settings.behavior.zoom_step,
            )
        });
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });

    let zooms = |cx: &TestAppContext| {
        (
            left_document.read_with(cx, |view, _| view.current_document_position().zoom),
            right_document.read_with(cx, |view, _| view.current_document_position().zoom),
        )
    };
    assert_eq!(zooms(cx), (1.0, 1.0));

    cx.simulate_keystrokes("+");
    let (left_zoom, right_zoom) = zooms(cx);
    assert!((left_zoom - zoom_step).abs() < 0.0001);
    assert_eq!(right_zoom, 1.0);

    cx.simulate_keystrokes("=");
    assert_eq!(zooms(cx), (1.0, 1.0));

    cx.simulate_keystrokes("-");
    let (left_zoom, right_zoom) = zooms(cx);
    assert!((left_zoom - 1.0 / zoom_step).abs() < 0.0001);
    assert_eq!(right_zoom, 1.0);

    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    let popup = cx
        .debug_bounds("document-zoom-popup")
        .expect("zoom popup after a keyboard zoom");
    let left_bounds = workspace.read_with(cx, |workspace, _| {
        workspace
            .pane_group
            .bounding_box_for_pane(&left_pane)
            .expect("left pane bounds")
    });
    let right_bounds = workspace.read_with(cx, |workspace, _| {
        workspace
            .pane_group
            .bounding_box_for_pane(&right_pane)
            .expect("right pane bounds")
    });
    assert!(left_bounds.contains(&popup.center()));
    assert!(!right_bounds.contains(&popup.center()));
    assert!(cx.debug_bounds("zoom-popup-out").is_some());
    assert!(cx.debug_bounds("zoom-popup-reset").is_some());
    assert!(cx.debug_bounds("zoom-popup-in").is_some());
    let fit_width = cx
        .debug_bounds("zoom-popup-fit-width")
        .expect("fit-width control for a fixed-layout document");
    assert!(cx.debug_bounds("zoom-popup-fit-height").is_some());

    cx.simulate_click(fit_width.center(), Modifiers::default());
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    assert_eq!(zooms(cx), (1.0, 1.0));

    let popup = cx.debug_bounds("document-zoom-popup").unwrap();
    cx.simulate_mouse_move(popup.center(), None, Modifiers::default());
    cx.executor()
        .advance_clock(glyphst_ui::TRANSIENT_FEEDBACK_DURATION);
    cx.run_until_parked();
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    assert!(cx.debug_bounds("document-zoom-popup").is_some());

    cx.simulate_mouse_move(right_bounds.center(), None, Modifiers::default());
    cx.executor()
        .advance_clock(glyphst_ui::TRANSIENT_FEEDBACK_DURATION);
    cx.run_until_parked();
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    assert!(cx.debug_bounds("document-zoom-popup").is_none());

    fs::remove_file(document_path).unwrap();
}

#[gpui::test]
fn tab_toggles_quick_ask_without_reentering_or_replacing_it(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_component::init(cx);
        crate::keybindings::capture_framework_bindings(cx);
        crate::keybindings::apply(cx, glyphst_settings::KeymapPreset::Modern, &[]);
        cx.set_global(SettingsStore::memory());
        cx.set_global(LibraryStore::memory());
        cx.set_global(RenderCache::new());
    });
    let callbacks = WorkspaceCallbacks {
        open_workspace_window: unavailable_workspace_window,
        open_settings_window: ignore_settings_window,
    };
    let (root, cx) = cx.add_window_view(|window, cx| {
        let workspace = cx.new(|cx| Workspace::new(None, None, callbacks, window, cx));
        Root::new(workspace, window, cx)
    });
    let workspace = root.read_with(cx, |root, _| {
        root.view().clone().downcast::<Workspace>().unwrap()
    });
    workspace.update_in(cx, |workspace, window, cx| {
        workspace.focus_handle.focus(window, cx);
    });
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });

    cx.simulate_keystrokes("tab");
    let first_quick_ask = workspace.read_with(cx, |workspace, _| {
        assert!(workspace.quick_ask_visible);
        workspace.quick_ask.as_ref().unwrap().entity_id()
    });
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });

    cx.simulate_keystrokes("tab");
    workspace.read_with(cx, |workspace, _| {
        assert!(!workspace.quick_ask_visible);
        assert_eq!(
            workspace.quick_ask.as_ref().unwrap().entity_id(),
            first_quick_ask
        );
    });
    cx.update(|window, cx| {
        assert!(workspace.read(cx).focus_handle.is_focused(window));
        _ = window.draw(cx);
    });

    cx.simulate_keystrokes("tab");
    workspace.read_with(cx, |workspace, _| {
        assert!(workspace.quick_ask_visible);
        assert_eq!(
            workspace.quick_ask.as_ref().unwrap().entity_id(),
            first_quick_ask
        );
    });
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });

    cx.simulate_keystrokes("escape");
    workspace.read_with(cx, |workspace, _| {
        assert!(!workspace.quick_ask_visible);
        assert!(workspace.quick_ask.is_none());
    });
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });

    cx.simulate_keystrokes("tab");
    workspace.read_with(cx, |workspace, _| {
        assert!(workspace.quick_ask_visible);
        assert_ne!(
            workspace.quick_ask.as_ref().unwrap().entity_id(),
            first_quick_ask
        );
    });
}

#[gpui::test]
fn quick_ask_positions_use_the_expected_workspace_bounds(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_component::init(cx);
        crate::keybindings::capture_framework_bindings(cx);
        crate::keybindings::apply(cx, glyphst_settings::KeymapPreset::Modern, &[]);
        cx.set_global(SettingsStore::memory());
        cx.set_global(LibraryStore::memory());
        cx.set_global(RenderCache::new());
    });
    let callbacks = WorkspaceCallbacks {
        open_workspace_window: unavailable_workspace_window,
        open_settings_window: ignore_settings_window,
    };
    let (root, cx) = cx.add_window_view(|window, cx| {
        let workspace = cx.new(|cx| Workspace::new(None, None, callbacks, window, cx));
        Root::new(workspace, window, cx)
    });
    let workspace = root.read_with(cx, |root, _| {
        root.view().clone().downcast::<Workspace>().unwrap()
    });
    cx.simulate_resize(size(px(900.0), px(640.0)));
    workspace.update_in(cx, |workspace, window, cx| {
        workspace.focus_handle.focus(window, cx);
    });
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    cx.simulate_keystrokes("tab");

    for (position, expected_top, expected_bottom) in [
        (glyphst_settings::QuickAskPosition::Top, Some(px(0.0)), None),
        (
            glyphst_settings::QuickAskPosition::Bottom,
            None,
            Some(px(640.0)),
        ),
    ] {
        workspace.update_in(cx, |_, _, cx| {
            cx.global_mut::<SettingsStore>()
                .settings
                .ai
                .quick_ask
                .position = position;
            cx.notify();
        });
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });

        let bounds = cx
            .debug_bounds("workspace-quick-ask-panel")
            .expect("Quick Ask panel bounds");
        assert_eq!(bounds.left(), px(0.0));
        assert_eq!(bounds.size.width, px(900.0));
        if let Some(expected_top) = expected_top {
            assert_eq!(bounds.top(), expected_top);
        }
        if let Some(expected_bottom) = expected_bottom {
            assert_eq!(bounds.bottom(), expected_bottom);
        }
    }

    for (position, expected_left, expected_right) in [
        (
            glyphst_settings::QuickAskPosition::Left,
            Some(px(0.0)),
            None,
        ),
        (
            glyphst_settings::QuickAskPosition::Right,
            None,
            Some(px(900.0)),
        ),
    ] {
        workspace.update_in(cx, |_, _, cx| {
            cx.global_mut::<SettingsStore>()
                .settings
                .ai
                .quick_ask
                .position = position;
            cx.notify();
        });
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });

        let bounds = cx
            .debug_bounds("workspace-quick-ask-panel")
            .expect("Quick Ask panel bounds");
        assert_eq!(bounds.top(), px(0.0));
        assert_eq!(bounds.size, size(px(360.0), px(640.0)));
        if let Some(expected_left) = expected_left {
            assert_eq!(bounds.left(), expected_left);
        }
        if let Some(expected_right) = expected_right {
            assert_eq!(bounds.right(), expected_right);
        }
    }

    workspace.update_in(cx, |_, _, cx| {
        cx.global_mut::<SettingsStore>()
            .settings
            .ai
            .quick_ask
            .position = glyphst_settings::QuickAskPosition::Center;
        cx.notify();
    });
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    let bounds = cx
        .debug_bounds("workspace-quick-ask-panel")
        .expect("Quick Ask panel bounds");
    assert_eq!(bounds.size.width, px(640.0));
    assert_eq!(bounds.center().x, px(450.0));
    assert_eq!(bounds.center().y, px(320.0));
}

#[gpui::test]
fn workspace_ai_snapshot_prefers_document_focus_mru_over_active_pane(cx: &mut TestAppContext) {
    let left_path = temporary_pdf_path("ai-context-left");
    let right_path = temporary_pdf_path("ai-context-right");
    write_minimal_pdf(&left_path);
    write_minimal_pdf(&right_path);
    cx.update(|cx| {
        gpui_component::init(cx);
        cx.set_reduce_motion(true);
        cx.set_global(SettingsStore::memory());
        cx.set_global(LibraryStore::memory());
        cx.set_global(RenderCache::new());
    });
    let callbacks = WorkspaceCallbacks {
        open_workspace_window: unavailable_workspace_window,
        open_settings_window: ignore_settings_window,
    };
    let initial_path = left_path.clone();
    let (root, cx) = cx.add_window_view(move |window, cx| {
        let workspace =
            cx.new(|cx| Workspace::new(Some(initial_path.clone()), None, callbacks, window, cx));
        Root::new(workspace, window, cx)
    });
    let workspace = root.read_with(cx, |root, _| {
        root.view().clone().downcast::<Workspace>().unwrap()
    });
    cx.run_until_parked();
    workspace.update_in(cx, |workspace, window, cx| {
        workspace.split_active_pane(SplitDirection::Right, window, cx);
    });
    cx.run_until_parked();

    let (left_pane, right_pane) = workspace.update_in(cx, |workspace, window, cx| {
        let panes = workspace.pane_group.panes();
        let left_pane = panes[0].clone();
        let right_pane = panes[1].clone();
        workspace.active_pane = right_pane.clone();
        right_pane
            .update(cx, |pane, cx| {
                pane.focus_or_open_document(right_path.clone(), None, false, window, cx)
            })
            .unwrap();
        (left_pane, right_pane)
    });
    cx.run_until_parked();

    workspace.update_in(cx, |workspace, _, cx| {
        workspace.record_document_context(&right_pane, cx);
        workspace.active_pane = left_pane.clone();
        let snapshot = workspace.ai_context_snapshot(cx);
        assert_eq!(
            snapshot
                .current_document
                .as_ref()
                .map(|document| document.read(cx).path().to_path_buf()),
            Some(right_path.clone())
        );
        assert_eq!(
            snapshot
                .open_documents
                .first()
                .map(|document| document.read(cx).path().to_path_buf()),
            Some(right_path.clone())
        );
    });

    let provider = WorkspaceAiContextProvider::new(workspace.downgrade());
    let (scope, documents) = cx.update(|_, cx| {
        let scope = provider.document_scope(cx).expect("current document scope");
        let tools = provider.agent_tools(cx).expect("workspace document tools");
        let result = tools.execute(
            &AgentToolCall {
                id: "list-current".to_string(),
                name: "list_open_documents".to_string(),
                arguments: "{}".to_string(),
            },
            &AgentCancellationToken::default(),
        );
        let documents = result
            .content
            .into_iter()
            .find_map(|part| match part {
                AgentToolResultPart::Text(text) => Some(text),
                AgentToolResultPart::Image(_) => None,
            })
            .expect("document list text");
        (scope, documents)
    });
    assert_eq!(
        scope,
        DocumentContextScope {
            path: right_path.clone(),
            label: right_path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .to_string(),
        }
    );
    let current_line = documents
        .lines()
        .find(|line| line.contains("current context"))
        .expect("one current-context document");
    assert!(current_line.contains(right_path.file_name().unwrap().to_str().unwrap()));
    assert_eq!(
        documents
            .lines()
            .filter(|line| line.contains("current context"))
            .count(),
        1
    );

    workspace.update_in(cx, |workspace, window, cx| {
        workspace.close_tab_in_pane_now(&right_pane, None, window, cx);
    });
    cx.run_until_parked();
    workspace.read_with(cx, |workspace, cx| {
        assert_eq!(
            workspace
                .ai_context_snapshot(cx)
                .current_document
                .map(|document| document.read(cx).path().to_path_buf()),
            Some(left_path.clone())
        );
    });

    fs::remove_file(left_path).unwrap();
    fs::remove_file(right_path).unwrap();
}

#[gpui::test]
fn zen_mode_hides_workspace_chrome_preserves_state_and_restores_with_escape(
    cx: &mut TestAppContext,
) {
    let first_path = temporary_pdf_path("zen-mode-first");
    let second_path = temporary_pdf_path("zen-mode-second");
    write_minimal_pdf(&first_path);
    write_minimal_pdf(&second_path);
    cx.update(|cx| {
        gpui_component::init(cx);
        crate::keybindings::capture_framework_bindings(cx);
        crate::keybindings::apply(cx, glyphst_settings::KeymapPreset::Modern, &[]);
        cx.set_reduce_motion(true);
        cx.set_global(SettingsStore::memory());
        cx.set_global(LibraryStore::memory());
        cx.set_global(RenderCache::new());
    });
    let callbacks = WorkspaceCallbacks {
        open_workspace_window: unavailable_workspace_window,
        open_settings_window: ignore_settings_window,
    };
    let initial_path = first_path.clone();
    let (root, cx) = cx.add_window_view(move |window, cx| {
        let workspace =
            cx.new(|cx| Workspace::new(Some(initial_path.clone()), None, callbacks, window, cx));
        Root::new(workspace, window, cx)
    });
    let workspace = root.read_with(cx, |root, _| {
        root.view().clone().downcast::<Workspace>().unwrap()
    });

    cx.run_until_parked();
    workspace.update_in(cx, |workspace, window, cx| {
        workspace.split_active_pane(SplitDirection::Right, window, cx);
    });
    cx.run_until_parked();
    cx.update(|window, cx| {
        _ = window.draw(cx);
        assert!(
            window
                .available_actions(cx)
                .iter()
                .any(|action| action.partial_eq(&ToggleZenMode))
        );
    });

    let fullscreen_before = workspace.update_in(cx, |workspace, window, cx| {
        workspace.toggle_left_sidebar(DocumentSidebar::Toc, window, cx);
        workspace.toggle_ai_dock(window, cx);
        let fullscreen = window.is_fullscreen();
        workspace.toggle_zen_mode(window, cx);
        assert!(workspace.zen_mode);
        assert_eq!(workspace.pane_group.panes().len(), 2);
        assert_eq!(workspace.docks.left_sidebar, Some(DocumentSidebar::Toc));
        assert!(workspace.docks.ai_panel_open);
        fullscreen
    });
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });

    assert!(cx.debug_bounds("workspace-zen-surface").is_some());
    assert!(cx.debug_bounds("workspace-title-bar-drag-region").is_none());
    assert!(cx.debug_bounds("workspace-pane-column").is_none());
    assert!(cx.debug_bounds("active-pane-surface").is_none());
    assert!(cx.debug_bounds("workspace-left-dock").is_none());
    assert!(cx.debug_bounds("workspace-ai-dock").is_none());
    assert!(cx.debug_bounds("workspace-pane-footbar").is_none());
    assert!(cx.debug_bounds("document-footer").is_some());
    cx.update(|window, _| assert_eq!(window.is_fullscreen(), fullscreen_before));

    cx.simulate_keystrokes("tab");
    workspace.read_with(cx, |workspace, _| {
        assert!(workspace.zen_mode);
        assert!(workspace.quick_ask_visible);
    });
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    assert!(cx.debug_bounds("workspace-quick-ask-panel").is_some());
    assert!(cx.debug_bounds("workspace-zen-surface").is_some());
    assert!(cx.debug_bounds("workspace-pane-column").is_none());

    cx.simulate_keystrokes("escape");
    workspace.read_with(cx, |workspace, _| {
        assert!(workspace.zen_mode);
        assert!(!workspace.quick_ask_visible);
        assert!(workspace.quick_ask.is_none());
    });
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    assert!(cx.debug_bounds("workspace-quick-ask-panel").is_none());
    assert!(cx.debug_bounds("workspace-zen-surface").is_some());

    workspace.update_in(cx, |workspace, window, cx| {
        workspace
            .open_document_in_active_pane(second_path.clone(), None, window, cx)
            .unwrap();
        assert!(workspace.zen_mode);
        assert!(workspace.active_pane.read(cx).active_tab_is_loading());
    });
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    assert!(cx.debug_bounds("workspace-zen-surface").is_some());
    assert!(cx.debug_bounds("document-open-loading").is_some());
    cx.update(|window, cx| {
        assert!(
            window
                .available_actions(cx)
                .iter()
                .any(|action| action.partial_eq(&ToggleZenMode))
        );
    });

    cx.run_until_parked();
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    assert!(cx.debug_bounds("document-open-loading").is_none());

    cx.simulate_keystrokes("escape");
    workspace.read_with(cx, |workspace, _| {
        assert!(!workspace.zen_mode);
        assert_eq!(workspace.pane_group.panes().len(), 2);
        assert_eq!(workspace.docks.left_sidebar, Some(DocumentSidebar::Toc));
        assert!(workspace.docks.ai_panel_open);
    });
    cx.update(|window, cx| {
        assert_eq!(window.is_fullscreen(), fullscreen_before);
        _ = window.draw(cx);
    });

    assert!(cx.debug_bounds("workspace-zen-surface").is_none());
    assert!(cx.debug_bounds("workspace-title-bar-drag-region").is_some());
    assert!(cx.debug_bounds("workspace-pane-column").is_some());
    assert!(cx.debug_bounds("active-pane-surface").is_some());
    assert!(cx.debug_bounds("workspace-left-dock").is_some());
    assert!(cx.debug_bounds("workspace-ai-dock").is_some());
    assert!(cx.debug_bounds("workspace-pane-footbar").is_none());
    assert!(cx.debug_bounds("document-footer").is_some());

    workspace.update_in(cx, |workspace, window, cx| {
        workspace.enter_home(window, cx);
        workspace.toggle_zen_mode(window, cx);
        assert!(!workspace.zen_mode);
    });
    cx.update(|window, cx| {
        _ = window.draw(cx);
        assert!(
            window
                .available_actions(cx)
                .iter()
                .all(|action| !action.partial_eq(&ToggleZenMode))
        );
    });

    fs::remove_file(first_path).unwrap();
    fs::remove_file(second_path).unwrap();
}

#[gpui::test]
fn home_toggle_preserves_the_document_workspace_and_requested_docks(cx: &mut TestAppContext) {
    let document_path = temporary_pdf_path("home-toggle-preserves-workspace");
    write_minimal_pdf(&document_path);
    cx.update(|cx| {
        gpui_component::init(cx);
        cx.set_reduce_motion(true);
        cx.set_global(SettingsStore::memory());
        cx.set_global(LibraryStore::memory());
        cx.set_global(RenderCache::new());
    });
    let callbacks = WorkspaceCallbacks {
        open_workspace_window: unavailable_workspace_window,
        open_settings_window: ignore_settings_window,
    };
    let initial_path = document_path.clone();
    let (root, cx) = cx.add_window_view(move |window, cx| {
        let workspace =
            cx.new(|cx| Workspace::new(Some(initial_path.clone()), None, callbacks, window, cx));
        Root::new(workspace, window, cx)
    });
    let workspace = root.read_with(cx, |root, _| {
        root.view().clone().downcast::<Workspace>().unwrap()
    });
    cx.run_until_parked();
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    let document_rail = cx
        .debug_bounds("workspace-document-tool-rail")
        .expect("document navigation rail");
    let document_id = workspace.update_in(cx, |workspace, window, cx| {
        workspace.toggle_left_sidebar(DocumentSidebar::Toc, window, cx);
        let document_id = workspace
            .active_pane
            .read(cx)
            .active_document_view()
            .unwrap()
            .entity_id();
        workspace.enter_home(window, cx);
        document_id
    });
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    workspace.read_with(cx, |workspace, _| {
        assert_eq!(workspace.surface_mode, WorkspaceSurfaceMode::Home);
        assert_eq!(workspace.docks.left_sidebar, Some(DocumentSidebar::Toc));
    });
    assert!(cx.debug_bounds("home-view").is_some());
    assert!(cx.debug_bounds("toc-sidebar-content").is_none());
    let home_rail = cx
        .debug_bounds("home-navigation-rail")
        .expect("Home navigation rail");
    assert_eq!(home_rail.left(), document_rail.left());
    assert_eq!(home_rail.size.width, document_rail.size.width);
    assert_eq!(home_rail.left(), px(glyphst_ui::style::SURFACE_INSET));

    workspace.update_in(cx, |workspace, window, cx| {
        workspace.leave_home(window, cx);
    });
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    workspace.read_with(cx, |workspace, cx| {
        assert_eq!(workspace.surface_mode, WorkspaceSurfaceMode::Panes);
        assert_eq!(
            workspace.active_pane.read(cx).active_tab_id(),
            Some(document_id)
        );
    });
    assert!(cx.debug_bounds("toc-sidebar-content").is_some());

    fs::remove_file(document_path).unwrap();
}

#[gpui::test]
fn closing_the_last_tab_enters_home_and_keeps_a_quick_open_tab(cx: &mut TestAppContext) {
    let document_path = temporary_pdf_path("last-tab-enters-home");
    write_minimal_pdf(&document_path);
    cx.update(|cx| {
        gpui_component::init(cx);
        cx.set_global(SettingsStore::memory());
        cx.set_global(LibraryStore::memory());
        cx.set_global(RenderCache::new());
    });
    let callbacks = WorkspaceCallbacks {
        open_workspace_window: unavailable_workspace_window,
        open_settings_window: ignore_settings_window,
    };
    let initial_path = document_path.clone();
    let (root, cx) = cx.add_window_view(move |window, cx| {
        let workspace =
            cx.new(|cx| Workspace::new(Some(initial_path.clone()), None, callbacks, window, cx));
        Root::new(workspace, window, cx)
    });
    let workspace = root.read_with(cx, |root, _| {
        root.view().clone().downcast::<Workspace>().unwrap()
    });
    cx.run_until_parked();
    workspace.update_in(cx, |workspace, window, cx| {
        workspace.close_active_tab(window, cx);
    });
    workspace.read_with(cx, |workspace, cx| {
        assert_eq!(workspace.surface_mode, WorkspaceSurfaceMode::Home);
        assert_eq!(workspace.pane_group.panes().len(), 1);
        assert!(workspace.active_pane.read(cx).active_tab_is_new_tab());
    });

    fs::remove_file(document_path).unwrap();
}

#[gpui::test]
fn home_open_targets_the_previous_active_pane_even_when_document_exists_elsewhere(
    cx: &mut TestAppContext,
) {
    let document_path = temporary_pdf_path("home-strict-active-pane");
    write_minimal_pdf(&document_path);
    cx.update(|cx| {
        gpui_component::init(cx);
        cx.set_global(SettingsStore::memory());
        cx.set_global(LibraryStore::memory());
        cx.set_global(RenderCache::new());
    });
    let callbacks = WorkspaceCallbacks {
        open_workspace_window: unavailable_workspace_window,
        open_settings_window: ignore_settings_window,
    };
    let initial_path = document_path.clone();
    let (root, cx) = cx.add_window_view(move |window, cx| {
        let workspace =
            cx.new(|cx| Workspace::new(Some(initial_path.clone()), None, callbacks, window, cx));
        Root::new(workspace, window, cx)
    });
    let workspace = root.read_with(cx, |root, _| {
        root.view().clone().downcast::<Workspace>().unwrap()
    });
    cx.run_until_parked();
    workspace.update_in(cx, |workspace, window, cx| {
        workspace.split_active_pane(SplitDirection::Right, window, cx);
    });
    cx.run_until_parked();

    let (left_pane, right_pane) = workspace.update_in(cx, |workspace, window, cx| {
        let panes = workspace.pane_group.panes();
        let left_pane = panes[0].clone();
        let right_pane = panes[1].clone();
        right_pane.update(cx, |pane, cx| pane.open_new_tab(window, cx));
        workspace.close_tab_in_pane_now(&right_pane, Some(0), window, cx);
        workspace.active_pane = right_pane.clone();
        workspace.enter_home(window, cx);
        (left_pane, right_pane)
    });
    workspace.update_in(cx, |workspace, window, cx| {
        workspace
            .open_document_in_active_pane(document_path.clone(), None, window, cx)
            .unwrap();
    });
    cx.run_until_parked();

    workspace.read_with(cx, |workspace, cx| {
        assert_eq!(workspace.surface_mode, WorkspaceSurfaceMode::Panes);
        assert_eq!(workspace.active_pane, right_pane);
        assert!(
            left_pane
                .read(cx)
                .document_tab_ix(&document_path, cx)
                .is_some()
        );
        assert!(
            right_pane
                .read(cx)
                .document_tab_ix(&document_path, cx)
                .is_some()
        );
    });

    fs::remove_file(document_path).unwrap();
}

#[gpui::test]
fn reader_session_restores_split_tabs_active_pane_and_docks(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_component::init(cx);
        cx.set_global(SettingsStore::memory());
        cx.set_global(LibraryStore::memory());
        cx.set_global(RenderCache::new());
    });
    let callbacks = WorkspaceCallbacks {
        open_workspace_window: unavailable_workspace_window,
        open_settings_window: ignore_settings_window,
    };
    let session = ReaderWindowSession {
        bounds: ReaderWindowBounds {
            x: 0.0,
            y: 0.0,
            width: 1200.0,
            height: 800.0,
            state: ReaderWindowState::Windowed,
        },
        pane_tree: ReaderPaneTree::Split {
            axis: ReaderSplitAxis::Horizontal,
            ratios: vec![0.3, 0.7],
            children: vec![
                ReaderPaneTree::Pane(ReaderPaneSession {
                    id: 1,
                    tabs: vec![ReaderTabSession {
                        id: 1,
                        kind: ReaderTabKind::NewTab(ReaderNewTabSession::default()),
                    }],
                    active_tab: Some(1),
                }),
                ReaderPaneTree::Pane(ReaderPaneSession {
                    id: 2,
                    tabs: vec![ReaderTabSession {
                        id: 2,
                        kind: ReaderTabKind::Keymap,
                    }],
                    active_tab: Some(2),
                }),
            ],
        },
        active_pane: 2,
        dock_document_tab: None,
        left_sidebar: Some(ReaderSidebar::Bookmarks),
        ai_panel_open: true,
        transparency_enabled: false,
        synchronized_groups: Vec::new(),
        translation_sessions: Vec::new(),
    };

    let (root, cx) = cx.add_window_view(move |window, cx| {
        let workspace = cx.new(|cx| Workspace::from_reader_session(session, callbacks, window, cx));
        Root::new(workspace, window, cx)
    });
    let workspace = root.read_with(cx, |root, _| {
        root.view().clone().downcast::<Workspace>().unwrap()
    });

    workspace.read_with(cx, |workspace, cx| {
        assert_eq!(workspace.pane_group.panes().len(), 2);
        assert_eq!(workspace.active_pane, workspace.pane_group.panes()[1]);
        assert_eq!(workspace.active_pane.read(cx).active_tab_ix(), 0);
        assert_eq!(
            workspace.docks.left_sidebar,
            Some(DocumentSidebar::Bookmarks)
        );
        assert!(workspace.docks.ai_panel_open);
        assert!(!workspace.transparency_enabled);
        assert!(!workspace.reader_session_restore_pending());
    });

    workspace.update_in(cx, |workspace, window, cx| {
        let snapshot = workspace.reader_window_session(window, cx);
        assert_eq!(snapshot.active_pane, 2);
        assert_eq!(snapshot.left_sidebar, Some(ReaderSidebar::Bookmarks));
        let ReaderPaneTree::Split {
            axis,
            ratios,
            children,
        } = snapshot.pane_tree
        else {
            panic!("expected restored split tree");
        };
        assert_eq!(axis, ReaderSplitAxis::Horizontal);
        assert_eq!(children.len(), 2);
        assert_eq!(ratios.len(), 2);
        assert!((ratios[0] - 0.3).abs() < 0.001);
    });
}

#[gpui::test]
fn encrypted_reader_session_unlocks_once_and_restores_its_translation(cx: &mut TestAppContext) {
    let document_path = temporary_pdf_path("restore-encrypted-translation");
    write_encrypted_pdf(&document_path);
    cx.update(|cx| {
        gpui_component::init(cx);
        cx.set_global(SettingsStore::memory());
        cx.set_global(LibraryStore::memory());
        cx.set_global(RenderCache::new());
        cx.set_global(PdfPasswordStore::memory());
    });
    let callbacks = WorkspaceCallbacks {
        open_workspace_window: unavailable_workspace_window,
        open_settings_window: ignore_settings_window,
    };
    let session = encrypted_translation_reader_session(&document_path);
    let (root, cx) = cx.add_window_view(move |window, cx| {
        let workspace = cx.new(|cx| Workspace::from_reader_session(session, callbacks, window, cx));
        Root::new(workspace, window, cx)
    });
    let workspace = root.read_with(cx, |root, _| {
        root.view().clone().downcast::<Workspace>().unwrap()
    });
    cx.run_until_parked();

    workspace.read_with(cx, |workspace, cx| {
        assert!(workspace.reader_session_restore_pending());
        assert!(workspace.translations.live_pairs.is_empty());
        assert!(
            workspace
                .pane_group
                .panes()
                .iter()
                .all(|pane| pane.read(cx).active_tab_is_loading())
        );
    });

    let source_pane =
        workspace.read_with(cx, |workspace, _| workspace.pane_group.panes()[0].clone());
    source_pane.update_in(cx, |pane, window, cx| {
        assert!(pane.submit_document_password_for_test(
            &document_path,
            "wrong-password",
            false,
            window,
            cx,
        ));
    });
    cx.run_until_parked();
    workspace.read_with(cx, |workspace, cx| {
        assert!(workspace.reader_session_restore_pending());
        assert!(workspace.translations.live_pairs.is_empty());
        assert!(
            workspace
                .pane_group
                .panes()
                .iter()
                .all(|pane| pane.read(cx).active_tab_is_loading())
        );
    });

    source_pane.update_in(cx, |pane, window, cx| {
        assert!(pane.submit_document_password_for_test(
            &document_path,
            "123456",
            false,
            window,
            cx,
        ));
    });
    cx.run_until_parked();
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    cx.run_until_parked();

    workspace.read_with(cx, |workspace, cx| {
        assert!(!workspace.reader_session_restore_pending());
        assert_eq!(workspace.translations.live_pairs.len(), 1);
        let panes = workspace.pane_group.panes();
        let source = panes[0].read(cx).document_views()[0].clone();
        let translated = panes[1].read(cx).document_views()[0].clone();
        assert!(source.read(cx).is_password_protected());
        assert!(translated.read(cx).is_password_protected());
        let (&session_id, pair) = workspace.translations.live_pairs.iter().next().unwrap();
        assert_eq!(pair.source_id, source.entity_id());
        assert_eq!(pair.translated_id, translated.entity_id());
        assert_eq!(
            translated.read(cx).translation_session_id(),
            Some(session_id)
        );
        assert_eq!(source.read(cx).translation_session_id(), None);
        assert_eq!(
            workspace
                .synchronized_tabs
                .group_for_tab(source.entity_id()),
            workspace
                .synchronized_tabs
                .group_for_tab(translated.entity_id())
        );
        assert!(
            workspace
                .synchronized_tabs
                .group_for_tab(source.entity_id())
                .is_some()
        );
    });
    workspace.update_in(cx, |workspace, window, cx| {
        let snapshot = workspace.reader_window_session(window, cx);
        assert_eq!(snapshot.translation_sessions.len(), 1);
        assert_eq!(
            snapshot.translation_sessions[0].source_language.as_deref(),
            Some("en")
        );
        assert_eq!(snapshot.translation_sessions[0].target_language, "zh-hans");
    });
    assert!(cx.read(|cx| {
        !cx.global::<PdfPasswordStore>()
            .is_known_saved(&document_path)
    }));

    fs::remove_file(document_path).ok();
}

#[gpui::test]
fn encrypted_reader_session_uses_a_saved_password_for_both_translation_tabs(
    cx: &mut TestAppContext,
) {
    let document_path = temporary_pdf_path("restore-encrypted-translation-saved");
    write_encrypted_pdf(&document_path);
    let password_store = PdfPasswordStore::memory();
    cx.update(|cx| {
        gpui_component::init(cx);
        cx.set_global(SettingsStore::memory());
        cx.set_global(LibraryStore::memory());
        cx.set_global(RenderCache::new());
        cx.set_global(password_store.clone());
    });
    let save =
        cx.update(|cx| password_store.write(&document_path, DocumentPassword::new("123456"), cx));
    cx.foreground_executor().block_test(save).unwrap();

    let callbacks = WorkspaceCallbacks {
        open_workspace_window: unavailable_workspace_window,
        open_settings_window: ignore_settings_window,
    };
    let session = encrypted_translation_reader_session(&document_path);
    let (root, cx) = cx.add_window_view(move |window, cx| {
        let workspace = cx.new(|cx| Workspace::from_reader_session(session, callbacks, window, cx));
        Root::new(workspace, window, cx)
    });
    let workspace = root.read_with(cx, |root, _| {
        root.view().clone().downcast::<Workspace>().unwrap()
    });
    cx.run_until_parked();
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    cx.run_until_parked();

    workspace.read_with(cx, |workspace, cx| {
        assert!(!workspace.reader_session_restore_pending());
        assert_eq!(workspace.translations.live_pairs.len(), 1);
        assert!(
            workspace
                .pane_group
                .panes()
                .iter()
                .all(|pane| pane.read(cx).document_views().len() == 1)
        );
    });
    assert!(password_store.is_known_saved(&document_path));

    fs::remove_file(document_path).ok();
}

#[gpui::test]
fn reader_session_restores_duplicate_documents_synchronization_and_translation(
    cx: &mut TestAppContext,
) {
    let document_path = temporary_pdf_path("restore-duplicates");
    write_minimal_pdf_with_height(&document_path, 1_000);
    cx.update(|cx| {
        gpui_component::init(cx);
        cx.set_global(SettingsStore::memory());
        cx.set_global(LibraryStore::memory());
        cx.set_global(RenderCache::new());
    });
    let callbacks = WorkspaceCallbacks {
        open_workspace_window: unavailable_workspace_window,
        open_settings_window: ignore_settings_window,
    };
    let document_tab = |id, zoom| ReaderTabSession {
        id,
        kind: ReaderTabKind::Document {
            path: document_path.clone(),
            position: DocumentPosition::fixed(0.0, 0.0, zoom, 1),
        },
    };
    let session = ReaderWindowSession {
        bounds: ReaderWindowBounds {
            x: 0.0,
            y: 0.0,
            width: 1200.0,
            height: 800.0,
            state: ReaderWindowState::Windowed,
        },
        pane_tree: ReaderPaneTree::Split {
            axis: ReaderSplitAxis::Horizontal,
            ratios: vec![0.5, 0.5],
            children: vec![
                ReaderPaneTree::Pane(ReaderPaneSession {
                    id: 1,
                    tabs: vec![document_tab(1, 1.0), document_tab(2, 1.5)],
                    active_tab: Some(2),
                }),
                ReaderPaneTree::Pane(ReaderPaneSession {
                    id: 2,
                    tabs: vec![document_tab(3, 2.0)],
                    active_tab: Some(3),
                }),
            ],
        },
        active_pane: 1,
        dock_document_tab: Some(2),
        left_sidebar: None,
        ai_panel_open: false,
        transparency_enabled: true,
        synchronized_groups: vec![ReaderSynchronizedGroup {
            tabs: vec![2, 3],
            paused_tabs: vec![3],
        }],
        translation_sessions: vec![ReaderTranslationSession {
            source_tab: 2,
            translated_tab: 3,
            source_language: Some("en".to_string()),
            target_language: "zh-hans".to_string(),
            mode: "auto".to_string(),
            cache_key: None,
            original_unit_ids: vec!["unit-1".to_string()],
        }],
    };

    let (root, cx) = cx.add_window_view(move |window, cx| {
        let workspace = cx.new(|cx| Workspace::from_reader_session(session, callbacks, window, cx));
        Root::new(workspace, window, cx)
    });
    let workspace = root.read_with(cx, |root, _| {
        root.view().clone().downcast::<Workspace>().unwrap()
    });
    cx.run_until_parked();
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    cx.run_until_parked();

    workspace.read_with(cx, |workspace, cx| {
        assert!(!workspace.reader_session_restore_pending());
        let panes = workspace.pane_group.panes();
        let left = panes[0].read(cx);
        let right = panes[1].read(cx);
        let left_views = left.document_views();
        let right_views = right.document_views();
        assert_eq!(left_views.len(), 2);
        assert_eq!(right_views.len(), 1);
        assert_ne!(left_views[0], left_views[1]);
        assert_eq!(left.active_tab_ix(), 1);
        assert_eq!(
            workspace
                .dock_document_target(cx)
                .map(|(_, document)| document.entity_id()),
            Some(left_views[1].entity_id())
        );
        let left_group = workspace
            .synchronized_tabs
            .group_for_tab(left_views[1].entity_id())
            .unwrap();
        assert_eq!(
            workspace
                .synchronized_tabs
                .group_for_tab(right_views[0].entity_id()),
            Some(left_group)
        );
        assert!(
            !workspace
                .synchronized_tabs
                .is_paused(right_views[0].entity_id())
        );
        assert!(right.synchronized_pause_locked(right_views[0].entity_id()));
        assert!(left.synchronized_pause_locked(left_views[1].entity_id()));
        assert_eq!(workspace.translations.live_pairs.len(), 1);
        let (&session_id, pair) = workspace.translations.live_pairs.iter().next().unwrap();
        assert_eq!(pair.source_id, left_views[1].entity_id());
        assert_eq!(pair.translated_id, right_views[0].entity_id());
        assert_eq!(
            right_views[0].read(cx).translation_session_id(),
            Some(session_id)
        );
        assert_eq!(left_views[1].read(cx).translation_session_id(), None);
        let translation = workspace
            .translations
            .manager
            .read(cx)
            .session_snapshot(session_id)
            .unwrap();
        assert!(translation.can_resume());
        assert_eq!(
            translation
                .request
                .source_language
                .as_ref()
                .unwrap()
                .as_str(),
            "en"
        );
        assert_eq!(translation.request.target_language.as_str(), "zh-hans");
        assert!(translation.original_unit_ids.contains("unit-1"));

        let source_anchor = left_views[1]
            .read(cx)
            .live_translation_viewport_anchor(SynchronizedScrollMode::Immediate)
            .unwrap();
        let translated_anchor = right_views[0]
            .read(cx)
            .live_translation_viewport_anchor(SynchronizedScrollMode::Immediate)
            .unwrap();
        assert_eq!(source_anchor.page_index, translated_anchor.page_index);
        assert!((source_anchor.pdf_x - translated_anchor.pdf_x).abs() < 0.01);
        assert!((source_anchor.pdf_y - translated_anchor.pdf_y).abs() < 0.01);
        assert_eq!(left_views[1].read(cx).current_document_position().zoom, 1.5);
        assert_eq!(
            right_views[0].read(cx).current_document_position().zoom,
            2.0
        );
    });

    let (source, translated) = workspace.read_with(cx, |workspace, _| {
        let pair = workspace.translations.live_pairs.values().next().unwrap();
        (
            pair.source.upgrade().unwrap(),
            pair.translated.upgrade().unwrap(),
        )
    });
    let overlay_button_bounds = cx
        .debug_bounds("translation-session-toggle-overlays")
        .unwrap();
    cx.simulate_click(overlay_button_bounds.center(), Modifiers::default());
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    assert!(!translated.read_with(cx, |view, _| view.translation_overlay_enabled()));
    assert!(!source.read_with(cx, |view, _| view.translation_overlay_enabled()));
    let overlay_button_bounds = cx
        .debug_bounds("translation-session-toggle-overlays")
        .unwrap();
    cx.simulate_click(overlay_button_bounds.center(), Modifiers::default());
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    assert!(translated.read_with(cx, |view, _| view.translation_overlay_enabled()));
    assert!(source.read_with(cx, |view, _| view.translation_overlay_enabled()));

    cx.update(|_, cx| {
            let source_anchor = source
                .read(cx)
                .live_translation_viewport_anchor(SynchronizedScrollMode::Immediate)
                .unwrap();
            let translated_anchor = translated
                .read(cx)
                .live_translation_viewport_anchor(SynchronizedScrollMode::Immediate)
                .unwrap();
            assert!(
                (source_anchor.pdf_y - translated_anchor.pdf_y).abs() < 0.01,
                "translation anchors diverged before resize: source={source_anchor:?}, translated={translated_anchor:?}"
            );
        });

    cx.simulate_resize(size(px(900.0), px(640.0)));
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    cx.update(|_, cx| {
            let source_anchor = source
                .read(cx)
                .live_translation_viewport_anchor(SynchronizedScrollMode::Immediate)
                .unwrap();
            let translated_anchor = translated
                .read(cx)
                .live_translation_viewport_anchor(SynchronizedScrollMode::Immediate)
                .unwrap();
            assert!((source_anchor.pdf_x - translated_anchor.pdf_x).abs() < 0.01);
            assert!(
                (source_anchor.pdf_y - translated_anchor.pdf_y).abs() < 0.01,
                "translation anchors diverged after resize: source={source_anchor:?} at {:?}, translated={translated_anchor:?} at {:?}",
                source.read(cx).current_document_position(),
                translated.read(cx).current_document_position(),
            );
        });

    workspace.update_in(cx, |workspace, window, cx| {
        workspace.split_active_pane(SplitDirection::Right, window, cx);
    });
    cx.run_until_parked();
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    workspace.update_in(cx, |workspace, _, cx| {
            let third_pane = workspace.active_pane.clone();
            let third = third_pane.read(cx).active_document_view().unwrap();
            let group_id = workspace
                .synchronized_tabs
                .group_for_tab(source.entity_id())
                .unwrap();
            assert!(workspace.synchronized_tabs.add_member(
                group_id,
                third.entity_id(),
                third_pane.entity_id(),
            ));
            workspace.refresh_synchronized_tab_presentation(cx);
            let delta = SynchronizedScrollDelta {
                x_pixels: 0.0,
                y_pixels: 48.0,
                mode: SynchronizedScrollMode::Immediate,
            };
            third.update(cx, |view, cx| {
                view.apply_synchronized_scroll_delta(delta, cx);
            });
            workspace.propagate_synchronized_scroll(third.entity_id(), delta, cx);
            assert_eq!(
                workspace
                    .synchronized_tabs
                    .group_summary(group_id)
                    .unwrap()
                    .member_count,
                3
            );
            let source_anchor = source
                .read(cx)
                .live_translation_viewport_anchor(SynchronizedScrollMode::Immediate)
                .unwrap();
            let translated_anchor = translated
                .read(cx)
                .live_translation_viewport_anchor(SynchronizedScrollMode::Immediate)
                .unwrap();
            assert!((source_anchor.pdf_x - translated_anchor.pdf_x).abs() < 0.01);
            assert!(
                (source_anchor.pdf_y - translated_anchor.pdf_y).abs() < 0.01,
                "translation anchors diverged after a third synchronized tab moved: source={source_anchor:?}, translated={translated_anchor:?}"
            );
            assert!(
                workspace
                    .synchronized_tabs
                    .set_paused(third.entity_id(), true)
            );
            assert!(workspace.synchronized_tabs.is_paused(third.entity_id()));
            workspace
                .synchronized_tabs
                .set_paused(third.entity_id(), false);
        });

    let (source, translated, translated_pane, translated_id, before) =
        workspace.update_in(cx, |workspace, window, cx| {
            let pair = workspace.translations.live_pairs.values().next().unwrap();
            let source = pair.source.upgrade().unwrap();
            let translated = pair.translated.upgrade().unwrap();
            let translated_id = translated.entity_id();
            let translated_pane = workspace
                .pane_group
                .panes()
                .into_iter()
                .find(|pane| pane.read(cx).has_tab_id(translated_id))
                .unwrap();
            translated_pane.update(cx, |pane, cx| pane.open_new_tab(window, cx));

            let before = translated.read(cx).current_document_position();
            source.update(cx, |view, cx| {
                view.navigate_to_document_position(DocumentPosition::fixed(0.0, 80.0, 4.0, 1), cx);
            });
            (source, translated, translated_pane, translated_id, before)
        });
    cx.run_until_parked();
    let after = translated.read_with(cx, |view, _| view.current_document_position());
    assert!(
        (after.scroll_y_doc - before.scroll_y_doc).abs() > 0.01,
        "hidden translated position did not update: before={before:?}, after={after:?}"
    );
    cx.update(|_, cx| {
        let source_anchor = source
            .read(cx)
            .live_translation_viewport_anchor(SynchronizedScrollMode::Immediate)
            .unwrap();
        let translated_anchor = translated
            .read(cx)
            .live_translation_viewport_anchor(SynchronizedScrollMode::Immediate)
            .unwrap();
        assert!((source_anchor.pdf_x - translated_anchor.pdf_x).abs() < 0.01);
        assert!((source_anchor.pdf_y - translated_anchor.pdf_y).abs() < 0.01);
    });

    translated_pane.update(cx, |_, cx| {
        cx.emit(PaneEvent::SetSynchronizedScrollPaused {
            tab_id: translated_id,
            paused: true,
        });
    });
    cx.run_until_parked();
    workspace.update_in(cx, |workspace, window, cx| {
        assert!(!workspace.synchronized_tabs.is_paused(translated_id));
        let snapshot = workspace.reader_window_session(window, cx);
        assert!(
            snapshot
                .synchronized_groups
                .iter()
                .all(|group| group.paused_tabs.is_empty())
        );
    });

    fs::remove_file(document_path).ok();
}

#[gpui::test]
fn duplicate_tab_creates_an_independent_adjacent_view_without_stale_history(
    cx: &mut TestAppContext,
) {
    let document_path = temporary_pdf_path("duplicate-tab");
    write_minimal_pdf(&document_path);
    cx.update(|cx| {
        gpui_component::init(cx);
        cx.set_global(SettingsStore::memory());
        cx.set_global(LibraryStore::memory());
        cx.set_global(RenderCache::new());
    });
    let callbacks = WorkspaceCallbacks {
        open_workspace_window: unavailable_workspace_window,
        open_settings_window: ignore_settings_window,
    };
    let initial_path = document_path.clone();
    let (root, cx) = cx.add_window_view(move |window, cx| {
        let workspace =
            cx.new(|cx| Workspace::new(Some(initial_path.clone()), None, callbacks, window, cx));
        Root::new(workspace, window, cx)
    });
    let workspace = root.read_with(cx, |root, _| {
        root.view().clone().downcast::<Workspace>().unwrap()
    });
    cx.run_until_parked();

    let source_pane = workspace.read_with(cx, |workspace, _| workspace.active_pane.clone());
    workspace.update_in(cx, |workspace, window, cx| {
        workspace.split_active_pane(SplitDirection::Right, window, cx);
    });
    cx.run_until_parked();
    workspace.update_in(cx, |workspace, window, cx| {
        let target_pane = workspace.active_pane.clone();
        let source = source_pane.read(cx).active_document_view().unwrap();
        let target = target_pane.read(cx).active_document_view().unwrap();
        assert!(
            workspace
                .synchronized_tabs
                .create_group(
                    source.entity_id(),
                    source_pane.entity_id(),
                    target.entity_id(),
                    target_pane.entity_id(),
                )
                .is_some()
        );
        workspace.active_pane = source_pane.clone();
        workspace.refresh_synchronized_tab_presentation(cx);
        workspace.duplicate_active_tab(window, cx);
    });
    let (pane, source, duplicate) = workspace.read_with(cx, |workspace, cx| {
        let pane = workspace.active_pane.clone();
        let views = pane.read(cx).document_views();
        assert_eq!(pane.read(cx).len(), 2);
        assert_eq!(pane.read(cx).active_tab_ix(), 1);
        assert_eq!(views.len(), 2);
        assert_ne!(views[0].entity_id(), views[1].entity_id());
        assert_eq!(views[0].read(cx).path(), views[1].read(cx).path());
        assert_eq!(
            views[0].read(cx).current_document_position(),
            views[1].read(cx).current_document_position()
        );
        assert!(
            workspace
                .synchronized_tabs
                .group_for_tab(views[0].entity_id())
                .is_some()
        );
        assert!(
            workspace
                .synchronized_tabs
                .group_for_tab(views[1].entity_id())
                .is_none()
        );
        (pane, views[0].clone(), views[1].clone())
    });

    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    assert!(cx.debug_bounds("pane-tab-copy-index-0").is_some());
    assert!(cx.debug_bounds("pane-tab-copy-index-1").is_some());

    let source_position = source.read_with(cx, |source, _| source.current_document_position());
    let duplicate_position = DocumentPosition::fixed(
        source_position.scroll_x_doc,
        source_position.scroll_y_doc,
        1.6,
        source_position.page,
    );
    duplicate.update(cx, |duplicate, cx| {
        duplicate.navigate_to_document_position(duplicate_position.clone(), cx);
    });
    assert_eq!(
        source.read_with(cx, |source, _| source.current_document_position()),
        source_position
    );
    assert_eq!(
        duplicate.read_with(cx, |duplicate, _| duplicate.current_document_position()),
        duplicate_position
    );

    workspace.update_in(cx, |workspace, window, cx| {
        workspace
            .focus_or_open_document(document_path.clone(), None, false, window, cx)
            .unwrap();
        assert_eq!(pane.read(cx).len(), 2, "normal Open must still deduplicate");
        assert_eq!(
            pane.read(cx).active_tab_id(),
            Some(duplicate.entity_id()),
            "normal Open should keep the active matching copy"
        );

        workspace.close_tab_in_pane(&pane, Some(0), window, cx);
        assert_eq!(pane.read(cx).len(), 1);
        assert_eq!(pane.read(cx).active_tab_id(), Some(duplicate.entity_id()));
        assert_eq!(
            cx.global::<LibraryStore>()
                .history_position(&document_path)
                .unwrap(),
            duplicate_position,
            "closing a stale copy must not overwrite the newer same-path position"
        );
    });

    fs::remove_file(document_path).unwrap();
}

#[gpui::test]
fn disjoint_synchronized_groups_scroll_independently(cx: &mut TestAppContext) {
    let document_path = temporary_pdf_path("independent-synchronized-groups");
    write_minimal_pdf(&document_path);
    cx.update(|cx| {
        gpui_component::init(cx);
        cx.set_global(SettingsStore::memory());
        cx.set_global(LibraryStore::memory());
        cx.set_global(RenderCache::new());
    });
    let callbacks = WorkspaceCallbacks {
        open_workspace_window: unavailable_workspace_window,
        open_settings_window: ignore_settings_window,
    };
    let initial_path = document_path.clone();
    let (root, cx) = cx.add_window_view(move |window, cx| {
        let workspace =
            cx.new(|cx| Workspace::new(Some(initial_path.clone()), None, callbacks, window, cx));
        Root::new(workspace, window, cx)
    });
    let workspace = root.read_with(cx, |root, _| {
        root.view().clone().downcast::<Workspace>().unwrap()
    });
    cx.run_until_parked();

    let top_left = workspace.read_with(cx, |workspace, _| workspace.active_pane.clone());
    workspace.update_in(cx, |workspace, window, cx| {
        workspace.split_active_pane(SplitDirection::Right, window, cx);
    });
    cx.run_until_parked();
    let top_right = workspace.read_with(cx, |workspace, _| workspace.active_pane.clone());
    workspace.update_in(cx, |workspace, window, cx| {
        workspace.split_active_pane(SplitDirection::Down, window, cx);
    });
    cx.run_until_parked();
    let bottom_right = workspace.read_with(cx, |workspace, _| workspace.active_pane.clone());
    workspace.update_in(cx, |workspace, window, cx| {
        workspace.active_pane = top_left.clone();
        workspace.split_active_pane(SplitDirection::Down, window, cx);
    });
    cx.run_until_parked();
    let bottom_left = workspace.read_with(cx, |workspace, _| workspace.active_pane.clone());
    cx.simulate_resize(size(px(900.0), px(360.0)));
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });

    workspace.update_in(cx, |workspace, window, cx| {
        let top_left_document = top_left.read(cx).active_document_view().unwrap();
        let bottom_left_document = bottom_left.read(cx).active_document_view().unwrap();
        let top_right_document = top_right.read(cx).active_document_view().unwrap();
        let bottom_right_document = bottom_right.read(cx).active_document_view().unwrap();
        let first_group = workspace
            .synchronized_tabs
            .create_group(
                top_left_document.entity_id(),
                top_left.entity_id(),
                bottom_left_document.entity_id(),
                bottom_left.entity_id(),
            )
            .unwrap();
        let second_group = workspace
            .synchronized_tabs
            .create_group(
                top_right_document.entity_id(),
                top_right.entity_id(),
                bottom_right_document.entity_id(),
                bottom_right.entity_id(),
            )
            .unwrap();
        workspace.refresh_synchronized_tab_presentation(cx);
        assert_eq!(
            workspace.synchronized_tabs.current_group(),
            Some(second_group)
        );

        let first_target_before = bottom_left_document.read(cx).current_document_position();
        let second_source_before = top_right_document.read(cx).current_document_position();
        let second_target_before = bottom_right_document.read(cx).current_document_position();
        workspace.propagate_synchronized_scroll(
            top_left_document.entity_id(),
            SynchronizedScrollDelta {
                x_pixels: 0.0,
                y_pixels: 48.0,
                mode: SynchronizedScrollMode::Immediate,
            },
            cx,
        );
        let first_target_after = bottom_left_document.read(cx).current_document_position();
        assert!(first_target_after.scroll_y_doc > first_target_before.scroll_y_doc);
        assert_eq!(
            top_right_document.read(cx).current_document_position(),
            second_source_before
        );
        assert_eq!(
            bottom_right_document.read(cx).current_document_position(),
            second_target_before
        );
        assert_eq!(
            workspace.synchronized_tabs.current_group(),
            Some(second_group)
        );

        workspace.handle_synchronized_tab_click(
            &top_left,
            top_left_document.entity_id(),
            true,
            window,
            cx,
        );
        assert_eq!(
            workspace.synchronized_tabs.current_group(),
            Some(first_group)
        );
        let first_source_before = top_left_document.read(cx).current_document_position();
        let first_target_before = bottom_left_document.read(cx).current_document_position();
        workspace.propagate_synchronized_scroll(
            top_right_document.entity_id(),
            SynchronizedScrollDelta {
                x_pixels: 0.0,
                y_pixels: 48.0,
                mode: SynchronizedScrollMode::Immediate,
            },
            cx,
        );
        assert!(
            bottom_right_document
                .read(cx)
                .current_document_position()
                .scroll_y_doc
                > second_target_before.scroll_y_doc
        );
        assert_eq!(
            top_left_document.read(cx).current_document_position(),
            first_source_before
        );
        assert_eq!(
            bottom_left_document.read(cx).current_document_position(),
            first_target_before
        );
        assert_eq!(
            workspace.synchronized_tabs.current_group(),
            Some(first_group)
        );
    });

    fs::remove_file(document_path).unwrap();
}

#[gpui::test]
fn synchronized_connection_renders_stable_network_and_supports_both_exit_paths(
    cx: &mut TestAppContext,
) {
    let document_path = temporary_pdf_path("synchronized-connection");
    let other_document_path = temporary_pdf_path("synchronized-connection-other");
    write_minimal_pdf(&document_path);
    write_minimal_pdf(&other_document_path);
    cx.update(|cx| {
        gpui_component::init(cx);
        cx.set_reduce_motion(true);
        cx.set_global(SettingsStore::memory());
        cx.set_global(LibraryStore::memory());
        cx.set_global(RenderCache::new());
    });
    let callbacks = WorkspaceCallbacks {
        open_workspace_window: unavailable_workspace_window,
        open_settings_window: ignore_settings_window,
    };
    let initial_path = document_path.clone();
    let (root, cx) = cx.add_window_view(move |window, cx| {
        let workspace =
            cx.new(|cx| Workspace::new(Some(initial_path.clone()), None, callbacks, window, cx));
        Root::new(workspace, window, cx)
    });
    let workspace = root.read_with(cx, |root, _| {
        root.view().clone().downcast::<Workspace>().unwrap()
    });
    cx.run_until_parked();
    workspace.update_in(cx, |workspace, window, cx| {
        workspace.split_active_pane(SplitDirection::Right, window, cx);
    });
    cx.run_until_parked();
    workspace.update_in(cx, |workspace, window, cx| {
        workspace
            .focus_or_open_document(other_document_path.clone(), None, false, window, cx)
            .unwrap();
    });
    cx.run_until_parked();
    workspace.update_in(cx, |workspace, window, cx| {
        workspace.split_active_pane(SplitDirection::Right, window, cx);
    });
    cx.run_until_parked();
    cx.simulate_resize(size(px(1200.0), px(300.0)));
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });

    workspace.update_in(cx, |workspace, window, cx| {
        let panes = workspace.pane_group.panes();
        assert_eq!(panes.len(), 3);
        let source = panes[0].read(cx).active_document_view().unwrap();
        let target = panes[1].read(cx).active_document_view().unwrap();
        let group_id = workspace
            .synchronized_tabs
            .create_group(
                source.entity_id(),
                panes[0].entity_id(),
                target.entity_id(),
                panes[1].entity_id(),
            )
            .unwrap();
        workspace.refresh_synchronized_tab_presentation(cx);
        panes[1].update(cx, |pane, cx| pane.open_new_tab(window, cx));
        workspace.handle_synchronized_tab_click(&panes[0], source.entity_id(), true, window, cx);
        assert_eq!(panes[1].read(cx).active_tab_id(), Some(target.entity_id()));
        let represented_document = panes[1]
            .read(cx)
            .tab_ids()
            .into_iter()
            .find(|tab_id| {
                *tab_id != target.entity_id() && panes[1].read(cx).tab_is_document(*tab_id)
            })
            .unwrap();
        let grouped_candidate = panes[2].read(cx).active_document_view().unwrap();
        workspace
            .synchronized_tabs
            .create_group(
                represented_document,
                panes[1].entity_id(),
                grouped_candidate.entity_id(),
                panes[2].entity_id(),
            )
            .unwrap();
        workspace.start_connecting_synchronized_group(
            panes[0].clone(),
            source.entity_id(),
            group_id,
            cx,
        );
    });
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    assert!(cx.debug_bounds("synchronized-connection-hint").is_some());
    let original_edge = workspace.read_with(cx, |workspace, cx| {
        let connection = workspace.synchronized_connection.as_ref().unwrap();
        assert_eq!(connection.edges.len(), 1);
        let panes = workspace.pane_group.panes();
        let source = panes[0].read(cx).active_document_view().unwrap();
        let target = panes[1].read(cx).active_document_view().unwrap();
        assert_eq!(
            panes[0]
                .read(cx)
                .synchronized_connection_tab_state(source.entity_id()),
            Some(SynchronizedConnectionTabState::Source)
        );
        assert_eq!(
            panes[1]
                .read(cx)
                .synchronized_connection_tab_state(target.entity_id()),
            Some(SynchronizedConnectionTabState::Member)
        );
        let non_document = panes[1]
            .read(cx)
            .tab_ids()
            .into_iter()
            .find(|tab_id| !panes[1].read(cx).tab_is_document(*tab_id))
            .unwrap();
        assert_eq!(
            panes[1]
                .read(cx)
                .synchronized_connection_tab_state(non_document),
            Some(SynchronizedConnectionTabState::Invalid(
                SynchronizedConnectionInvalidReason::NonDocument
            ))
        );
        let represented_document = panes[1]
            .read(cx)
            .tab_ids()
            .into_iter()
            .find(|tab_id| {
                *tab_id != target.entity_id() && panes[1].read(cx).tab_is_document(*tab_id)
            })
            .unwrap();
        let other_group_number = workspace
            .synchronized_tabs
            .summary_for_tab(represented_document)
            .unwrap()
            .display_number;
        assert_eq!(
            panes[1]
                .read(cx)
                .synchronized_connection_tab_state(represented_document),
            Some(SynchronizedConnectionTabState::Invalid(
                SynchronizedConnectionInvalidReason::AlreadyInGroup {
                    group_number: other_group_number
                }
            ))
        );
        let grouped_candidate = panes[2].read(cx).active_document_view().unwrap();
        assert_eq!(
            panes[2]
                .read(cx)
                .synchronized_connection_tab_state(grouped_candidate.entity_id()),
            Some(SynchronizedConnectionTabState::Invalid(
                SynchronizedConnectionInvalidReason::AlreadyInGroup {
                    group_number: other_group_number
                }
            ))
        );
        connection.edges[0]
    });

    workspace.update_in(cx, |workspace, _, cx| {
        let panes = workspace.pane_group.panes();
        let target = panes[1].read(cx).active_document_view().unwrap();
        let represented_document = panes[1]
            .read(cx)
            .tab_ids()
            .into_iter()
            .find(|tab_id| {
                *tab_id != target.entity_id() && panes[1].read(cx).tab_is_document(*tab_id)
            })
            .unwrap();
        workspace
            .synchronized_tabs
            .leave_group(represented_document);
        workspace.refresh_synchronized_tab_presentation(cx);
    });
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    workspace.read_with(cx, |workspace, cx| {
        let panes = workspace.pane_group.panes();
        let connection = workspace.synchronized_connection.as_ref().unwrap();
        let group_number = match connection.group {
            SynchronizedConnectionGroup::Existing(group_id) => {
                workspace
                    .synchronized_tabs
                    .group_summary(group_id)
                    .unwrap()
                    .display_number
            }
            SynchronizedConnectionGroup::New(_) => unreachable!(),
        };
        let target = panes[1].read(cx).active_document_view().unwrap();
        let represented_document = panes[1]
            .read(cx)
            .tab_ids()
            .into_iter()
            .find(|tab_id| {
                *tab_id != target.entity_id() && panes[1].read(cx).tab_is_document(*tab_id)
            })
            .unwrap();
        assert_eq!(
            panes[1]
                .read(cx)
                .synchronized_connection_tab_state(represented_document),
            Some(SynchronizedConnectionTabState::Invalid(
                SynchronizedConnectionInvalidReason::PaneAlreadyRepresented { group_number }
            ))
        );
        let eligible = panes[2].read(cx).active_document_view().unwrap();
        assert_eq!(
            panes[2]
                .read(cx)
                .synchronized_connection_tab_state(eligible.entity_id()),
            Some(SynchronizedConnectionTabState::Eligible)
        );
    });

    let eligible_bounds = workspace.read_with(cx, |workspace, cx| {
        let pane = &workspace.pane_group.panes()[2];
        let eligible = pane.read(cx).active_document_view().unwrap();
        pane.read(cx).tab_bounds(eligible.entity_id()).unwrap()
    });
    cx.simulate_mouse_move(eligible_bounds.center(), None, Modifiers::default());
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    workspace.read_with(cx, |workspace, cx| {
        assert!(matches!(
            workspace.hovered_synchronized_connection_tab(cx),
            Some((SynchronizedConnectionTabState::Eligible, _))
        ));
    });
    cx.simulate_click(eligible_bounds.center(), Modifiers::default());
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    workspace.read_with(cx, |workspace, cx| {
        let connection = workspace.synchronized_connection.as_ref().unwrap();
        assert_eq!(connection.edges.len(), 2);
        assert!(connection.edges.contains(&original_edge));
        let pane = &workspace.pane_group.panes()[2];
        let added = pane.read(cx).active_document_view().unwrap();
        assert_eq!(
            pane.read(cx)
                .synchronized_connection_tab_state(added.entity_id()),
            Some(SynchronizedConnectionTabState::Member)
        );
        let group_id = match connection.group {
            SynchronizedConnectionGroup::Existing(group_id) => group_id,
            SynchronizedConnectionGroup::New(_) => unreachable!(),
        };
        assert_eq!(
            workspace
                .synchronized_tabs
                .group_summary(group_id)
                .unwrap()
                .member_count,
            3
        );
    });

    cx.simulate_keystrokes("escape");
    workspace.read_with(cx, |workspace, _| {
        assert!(workspace.synchronized_connection.is_none());
        assert!(workspace.fading_synchronized_connection.is_none());
    });

    cx.update(|_, cx| cx.set_reduce_motion(false));
    workspace.update_in(cx, |workspace, _, cx| {
        let group_id = workspace.synchronized_tabs.current_group().unwrap();
        let source_pane = workspace.pane_group.panes()[0].clone();
        let source_tab = source_pane
            .read(cx)
            .active_document_view()
            .unwrap()
            .entity_id();
        workspace.start_connecting_synchronized_group(source_pane, source_tab, group_id, cx);
    });
    let position = point(px(30.0), px(120.0));
    cx.simulate_mouse_down(position, MouseButton::Right, Modifiers::default());
    workspace.read_with(cx, |workspace, _| {
        assert!(workspace.synchronized_connection.is_none());
        assert!(workspace.fading_synchronized_connection.is_some());
    });

    workspace.update_in(cx, |workspace, window, cx| {
        let panes = workspace.pane_group.panes();
        let source = panes[0].read(cx).active_document_view().unwrap();
        let target = panes[1].read(cx).active_document_view().unwrap();
        let before = target.read(cx).current_document_position();
        workspace.propagate_synchronized_scroll(
            source.entity_id(),
            SynchronizedScrollDelta {
                x_pixels: 0.0,
                y_pixels: 48.0,
                mode: SynchronizedScrollMode::Immediate,
            },
            cx,
        );
        let after = target.read(cx).current_document_position();
        assert!(after.scroll_y_doc > before.scroll_y_doc);

        assert!(
            workspace
                .synchronized_tabs
                .set_paused(target.entity_id(), true)
        );
        workspace.refresh_synchronized_tab_presentation(cx);
        let paused_target = target.read(cx).current_document_position();
        workspace.propagate_synchronized_scroll(
            source.entity_id(),
            SynchronizedScrollDelta {
                x_pixels: 0.0,
                y_pixels: 48.0,
                mode: SynchronizedScrollMode::Immediate,
            },
            cx,
        );
        assert_eq!(target.read(cx).current_document_position(), paused_target);

        assert!(
            workspace
                .synchronized_tabs
                .set_paused(target.entity_id(), false)
        );
        workspace.refresh_synchronized_tab_presentation(cx);
        assert_eq!(target.read(cx).current_document_position(), paused_target);
        workspace.propagate_synchronized_scroll(
            source.entity_id(),
            SynchronizedScrollDelta {
                x_pixels: 0.0,
                y_pixels: 48.0,
                mode: SynchronizedScrollMode::Immediate,
            },
            cx,
        );
        let resumed_target = target.read(cx).current_document_position();
        assert!(resumed_target.scroll_y_doc > paused_target.scroll_y_doc);

        assert!(
            workspace
                .synchronized_tabs
                .set_paused(source.entity_id(), true)
        );
        workspace.refresh_synchronized_tab_presentation(cx);
        workspace.propagate_synchronized_scroll(
            source.entity_id(),
            SynchronizedScrollDelta {
                x_pixels: 0.0,
                y_pixels: 48.0,
                mode: SynchronizedScrollMode::Immediate,
            },
            cx,
        );
        assert_eq!(target.read(cx).current_document_position(), resumed_target);
        assert!(
            workspace
                .synchronized_tabs
                .set_paused(source.entity_id(), false)
        );
        workspace.refresh_synchronized_tab_presentation(cx);

        panes[1].update(cx, |pane, cx| pane.open_new_tab(window, cx));
        let hidden_before = target.read(cx).current_document_position();
        workspace.propagate_synchronized_scroll(
            source.entity_id(),
            SynchronizedScrollDelta {
                x_pixels: 0.0,
                y_pixels: 48.0,
                mode: SynchronizedScrollMode::Immediate,
            },
            cx,
        );
        assert_eq!(target.read(cx).current_document_position(), hidden_before);
    });

    workspace.update_in(cx, |workspace, window, cx| {
        let panes = workspace.pane_group.panes();
        let source_pane = panes[0].clone();
        let target_pane = panes[1].clone();
        let group_id = workspace.synchronized_tabs.current_group().unwrap();
        let target_tab = workspace
            .synchronized_tabs
            .member_tabs(group_id)
            .into_iter()
            .find(|tab_id| target_pane.read(cx).has_tab_id(*tab_id))
            .unwrap();
        let target_ix = target_pane
            .read(cx)
            .tab_ix_for_entity_id(target_tab)
            .unwrap();
        workspace.close_tab_in_pane(&source_pane, None, window, cx);
        workspace.close_tab_in_pane(&target_pane, Some(target_ix), window, cx);
        assert!(workspace.synchronized_tabs.current_group().is_none());
    });
    fs::remove_file(document_path).unwrap();
    fs::remove_file(other_document_path).unwrap();
}

#[gpui::test]
fn open_shortcut_does_not_stack_the_custom_path_chooser(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_component::init(cx);
        crate::keybindings::capture_framework_bindings(cx);
        crate::keybindings::apply(cx, glyphst_settings::KeymapPreset::Modern, &[]);
        cx.set_global(SettingsStore::memory());
        cx.set_global(LibraryStore::memory());
        cx.set_global(RenderCache::new());
    });
    let callbacks = WorkspaceCallbacks {
        open_workspace_window: unavailable_workspace_window,
        open_settings_window: ignore_settings_window,
    };

    let (root, cx) = cx.add_window_view(|window, cx| {
        let workspace = cx.new(|cx| Workspace::new(None, None, callbacks, window, cx));
        Root::new(workspace, window, cx)
    });
    let workspace = root.read_with(cx, |root, _| {
        root.view().clone().downcast::<Workspace>().unwrap()
    });

    workspace.update_in(cx, |workspace, window, cx| {
        workspace.open_file(window, cx);
    });
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    assert!(cx.debug_bounds("path-chooser").is_some());
    cx.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-o"
    } else {
        "ctrl-o"
    });
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    cx.simulate_keystrokes("escape");
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    assert!(cx.debug_bounds("path-chooser").is_none());
}

#[gpui::test]
fn system_path_chooser_requests_multiple_files_from_native_prompt(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_component::init(cx);
        let mut settings = SettingsStore::memory();
        settings.settings.path_chooser = glyphst_settings::PathChooserBackend::System;
        cx.set_global(settings);
        cx.set_global(LibraryStore::memory());
        cx.set_global(RenderCache::new());
    });
    let callbacks = WorkspaceCallbacks {
        open_workspace_window: unavailable_workspace_window,
        open_settings_window: ignore_settings_window,
    };

    let (root, cx) = cx.add_window_view(|window, cx| {
        let workspace = cx.new(|cx| Workspace::new(None, None, callbacks, window, cx));
        Root::new(workspace, window, cx)
    });
    let workspace = root.read_with(cx, |root, _| {
        root.view().clone().downcast::<Workspace>().unwrap()
    });
    cx.update(|_, _| {
        let options = open_document_options(PathBuf::from("."));
        assert!(options.files);
        assert!(!options.directories);
        assert!(options.multiple);
    });
    workspace.update_in(cx, |workspace, window, cx| {
        workspace.open_file(window, cx);
    });
    assert!(cx.did_prompt_for_paths());
}

#[gpui::test]
fn documentless_tabs_suspend_document_docks_but_keep_global_ai(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_component::init(cx);
        cx.set_global(SettingsStore::memory());
        cx.set_global(LibraryStore::memory());
        cx.set_global(RenderCache::new());
    });
    let callbacks = WorkspaceCallbacks {
        open_workspace_window: unavailable_workspace_window,
        open_settings_window: ignore_settings_window,
    };

    let (root, cx) = cx.add_window_view(|window, cx| {
        let workspace = cx.new(|cx| Workspace::new(None, None, callbacks, window, cx));
        Root::new(workspace, window, cx)
    });
    let workspace = root.read_with(cx, |root, _| {
        root.view().clone().downcast::<Workspace>().unwrap()
    });
    workspace.update_in(cx, |workspace, window, cx| {
        workspace.leave_home(window, cx);
        workspace.toggle_left_sidebar(DocumentSidebar::Toc, window, cx);
        workspace.toggle_ai_dock(window, cx);
        workspace.open_new_tab(window, cx);
    });
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });

    workspace.read_with(cx, |workspace, _| {
        assert_eq!(workspace.docks.left_sidebar, Some(DocumentSidebar::Toc));
        assert!(workspace.docks.ai_panel_open);
    });
    assert!(cx.debug_bounds("workspace-left-dock").is_none());
    assert!(cx.debug_bounds("workspace-ai-dock").is_some());
}

#[gpui::test]
fn global_ai_dock_omits_the_focused_document_label(cx: &mut TestAppContext) {
    let document_path = temporary_pdf_path("ai-dock-without-document-label");
    write_minimal_pdf(&document_path);
    cx.update(|cx| {
        gpui_component::init(cx);
        cx.set_reduce_motion(true);
        cx.set_global(SettingsStore::memory());
        cx.set_global(LibraryStore::memory());
        cx.set_global(RenderCache::new());
    });
    let callbacks = WorkspaceCallbacks {
        open_workspace_window: unavailable_workspace_window,
        open_settings_window: ignore_settings_window,
    };
    let initial_path = document_path.clone();
    let (root, cx) = cx.add_window_view(move |window, cx| {
        let workspace =
            cx.new(|cx| Workspace::new(Some(initial_path.clone()), None, callbacks, window, cx));
        Root::new(workspace, window, cx)
    });
    let workspace = root.read_with(cx, |root, _| {
        root.view().clone().downcast::<Workspace>().unwrap()
    });
    cx.run_until_parked();

    workspace.update_in(cx, |workspace, window, cx| {
        assert!(workspace.ai_context_document(cx).is_some());
        workspace.toggle_ai_dock(window, cx);
    });
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });

    assert!(cx.debug_bounds("workspace-ai-dock").is_some());
    assert!(cx.debug_bounds("workspace-dock-document-context").is_none());

    fs::remove_file(document_path).unwrap();
}

#[gpui::test]
fn dock_widths_stay_fixed_across_window_resizes_and_restore_after_constraint(
    cx: &mut TestAppContext,
) {
    let document_path = temporary_pdf_path("dock-widths");
    write_minimal_pdf(&document_path);
    cx.update(|cx| {
        gpui_component::init(cx);
        cx.set_reduce_motion(true);
        cx.set_global(SettingsStore::memory());
        cx.set_global(LibraryStore::memory());
        cx.set_global(RenderCache::new());
    });
    let callbacks = WorkspaceCallbacks {
        open_workspace_window: unavailable_workspace_window,
        open_settings_window: ignore_settings_window,
    };

    let initial_path = document_path.clone();
    let (root, cx) = cx.add_window_view(move |window, cx| {
        let workspace =
            cx.new(|cx| Workspace::new(Some(initial_path.clone()), None, callbacks, window, cx));
        Root::new(workspace, window, cx)
    });
    let workspace = root.read_with(cx, |root, _| {
        root.view().clone().downcast::<Workspace>().unwrap()
    });
    cx.run_until_parked();
    workspace.update_in(cx, |workspace, window, cx| {
        workspace.docks.preferred_widths.toc = 280.0;
        workspace.docks.preferred_widths.ai = 360.0;
        workspace.toggle_left_sidebar(DocumentSidebar::Toc, window, cx);
        workspace.toggle_ai_dock(window, cx);
    });

    cx.simulate_resize(size(px(800.0), px(700.0)));
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    assert_eq!(
        cx.debug_bounds("workspace-left-dock").unwrap().size.width,
        px(280.0)
    );
    assert_eq!(
        cx.debug_bounds("workspace-ai-dock").unwrap().size.width,
        px(360.0)
    );
    let navigation = cx.debug_bounds("workspace-left-navigation").unwrap();
    let pane_column = cx.debug_bounds("workspace-pane-column").unwrap();
    let pane_area = cx.debug_bounds("workspace-dock-center").unwrap();
    let ai = cx.debug_bounds("workspace-ai-dock").unwrap();
    assert!(cx.debug_bounds("workspace-main-card").is_none());
    assert_eq!(pane_column.left() - navigation.right(), px(4.0));
    assert_eq!(ai.left() - pane_column.right(), px(4.0));
    assert_eq!(pane_area.top(), pane_column.top());
    assert_eq!(pane_area.bottom(), pane_column.bottom());
    assert!(cx.debug_bounds("workspace-pane-footbar").is_none());

    cx.simulate_resize(size(px(1_600.0), px(900.0)));
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    assert_eq!(
        cx.debug_bounds("workspace-left-dock").unwrap().size.width,
        px(280.0)
    );
    assert_eq!(
        cx.debug_bounds("workspace-ai-dock").unwrap().size.width,
        px(360.0)
    );

    cx.simulate_resize(size(px(500.0), px(700.0)));
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    let constrained_left = cx
        .debug_bounds("workspace-left-dock")
        .unwrap()
        .size
        .width
        .as_f32();
    let constrained_right = cx
        .debug_bounds("workspace-ai-dock")
        .unwrap()
        .size
        .width
        .as_f32();
    assert!(constrained_left < 280.0);
    assert!(constrained_right < 360.0);
    // Insets, the 44 px tool rail, and two 4 px surface gutters remain fixed.
    assert!((constrained_left + constrained_right - 432.0).abs() < 0.1);
    workspace.read_with(cx, |workspace, _| {
        assert_eq!(workspace.docks.preferred_widths.toc, 280.0);
        assert_eq!(workspace.docks.preferred_widths.ai, 360.0);
    });

    cx.simulate_resize(size(px(1_600.0), px(900.0)));
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    assert_eq!(
        cx.debug_bounds("workspace-left-dock").unwrap().size.width,
        px(280.0)
    );
    assert_eq!(
        cx.debug_bounds("workspace-ai-dock").unwrap().size.width,
        px(360.0)
    );
    fs::remove_file(document_path).unwrap();
}

#[gpui::test]
fn double_clicking_dock_handles_restores_each_default_width(cx: &mut TestAppContext) {
    let document_path = temporary_pdf_path("dock-double-click");
    write_minimal_pdf(&document_path);
    cx.update(|cx| {
        gpui_component::init(cx);
        cx.set_reduce_motion(true);
        cx.set_global(SettingsStore::memory());
        cx.set_global(LibraryStore::memory());
        cx.set_global(RenderCache::new());
    });
    let callbacks = WorkspaceCallbacks {
        open_workspace_window: unavailable_workspace_window,
        open_settings_window: ignore_settings_window,
    };

    let initial_path = document_path.clone();
    let (root, cx) = cx.add_window_view(move |window, cx| {
        let workspace =
            cx.new(|cx| Workspace::new(Some(initial_path.clone()), None, callbacks, window, cx));
        Root::new(workspace, window, cx)
    });
    let workspace = root.read_with(cx, |root, _| {
        root.view().clone().downcast::<Workspace>().unwrap()
    });
    cx.run_until_parked();
    workspace.update_in(cx, |workspace, window, cx| {
        workspace.docks.preferred_widths.toc = 420.0;
        workspace.docks.preferred_widths.notes = 410.0;
        workspace.docks.preferred_widths.ai = 520.0;
        workspace.toggle_left_sidebar(DocumentSidebar::Toc, window, cx);
        workspace.toggle_ai_dock(window, cx);
    });
    cx.simulate_resize(size(px(1_200.0), px(800.0)));
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });

    let left_handle = cx
        .debug_bounds("workspace-left-dock-resize-handle")
        .unwrap()
        .center();
    cx.simulate_event(MouseUpEvent {
        position: left_handle,
        modifiers: Modifiers::default(),
        button: MouseButton::Left,
        click_count: 2,
    });
    workspace.read_with(cx, |workspace, _| {
        assert_eq!(workspace.docks.preferred_widths.toc, 280.0);
        assert_eq!(workspace.docks.preferred_widths.ai, 520.0);
    });

    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    let right_handle = cx
        .debug_bounds("workspace-ai-dock-resize-handle")
        .unwrap()
        .center();
    cx.simulate_event(MouseUpEvent {
        position: right_handle,
        modifiers: Modifiers::default(),
        button: MouseButton::Left,
        click_count: 2,
    });
    workspace.read_with(cx, |workspace, _| {
        assert_eq!(workspace.docks.preferred_widths.toc, 280.0);
        assert_eq!(workspace.docks.preferred_widths.ai, 360.0);
    });

    workspace.update_in(cx, |workspace, window, cx| {
        workspace.toggle_left_sidebar(DocumentSidebar::Notes, window, cx);
    });
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    assert_eq!(
        cx.debug_bounds("workspace-left-dock").unwrap().size.width,
        px(410.0)
    );

    workspace.update_in(cx, |workspace, window, cx| {
        workspace.toggle_left_sidebar(DocumentSidebar::Toc, window, cx);
    });
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    assert_eq!(
        cx.debug_bounds("workspace-left-dock").unwrap().size.width,
        px(280.0)
    );
    fs::remove_file(document_path).unwrap();
}

#[gpui::test]
fn dragging_a_dock_handle_can_exceed_half_the_window(cx: &mut TestAppContext) {
    let document_path = temporary_pdf_path("dock-drag");
    write_minimal_pdf(&document_path);
    cx.update(|cx| {
        gpui_component::init(cx);
        cx.set_reduce_motion(true);
        cx.set_global(SettingsStore::memory());
        cx.set_global(LibraryStore::memory());
        cx.set_global(RenderCache::new());
    });
    let callbacks = WorkspaceCallbacks {
        open_workspace_window: unavailable_workspace_window,
        open_settings_window: ignore_settings_window,
    };

    let initial_path = document_path.clone();
    let (root, cx) = cx.add_window_view(move |window, cx| {
        let workspace =
            cx.new(|cx| Workspace::new(Some(initial_path.clone()), None, callbacks, window, cx));
        Root::new(workspace, window, cx)
    });
    let workspace = root.read_with(cx, |root, _| {
        root.view().clone().downcast::<Workspace>().unwrap()
    });
    cx.run_until_parked();
    workspace.update_in(cx, |workspace, window, cx| {
        workspace.toggle_left_sidebar(DocumentSidebar::Toc, window, cx);
    });
    cx.simulate_resize(size(px(1_000.0), px(700.0)));
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });

    let handle = cx
        .debug_bounds("workspace-left-dock-resize-handle")
        .unwrap()
        .center();
    cx.simulate_mouse_down(handle, MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_move(
        point(px(600.0), handle.y),
        MouseButton::Left,
        Modifiers::default(),
    );
    cx.simulate_mouse_move(
        point(px(852.0), handle.y),
        MouseButton::Left,
        Modifiers::default(),
    );
    cx.simulate_mouse_up(
        point(px(852.0), handle.y),
        MouseButton::Left,
        Modifiers::default(),
    );

    workspace.read_with(cx, |workspace, _| {
        assert_eq!(workspace.docks.preferred_widths.toc, 800.0);
    });
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    assert_eq!(
        cx.debug_bounds("workspace-left-dock").unwrap().size.width,
        px(800.0)
    );
    fs::remove_file(document_path).unwrap();
}

#[gpui::test]
fn translation_failures_use_component_notification_layer(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_component::init(cx);
        glyphst_ui::activity::init(cx);
        cx.set_global(SettingsStore::memory());
        cx.set_global(LibraryStore::memory());
        cx.set_global(RenderCache::new());
    });
    let callbacks = WorkspaceCallbacks {
        open_workspace_window: unavailable_workspace_window,
        open_settings_window: ignore_settings_window,
    };

    let (root, cx) = cx.add_window_view(|window, cx| {
        let workspace = cx.new(|cx| Workspace::new(None, None, callbacks, window, cx));
        Root::new(workspace, window, cx)
    });
    let workspace = root.read_with(cx, |root, _| {
        root.view().clone().downcast::<Workspace>().unwrap()
    });

    workspace.update_in(cx, |workspace, _, cx| {
        workspace.translations.manager.update(cx, |jobs, cx| {
            jobs.report_preflight_error(
                "/tmp/source.pdf".into(),
                DocumentTranslationOpenMode::Document,
                "Invalid target language",
                cx,
            );
        });
    });
    cx.run_until_parked();

    workspace.update_in(cx, |_, window, cx| {
        assert_eq!(window.notifications(cx).len(), 1);
    });
}

#[gpui::test]
fn dropped_documents_open_in_order_and_report_skipped_items(cx: &mut TestAppContext) {
    let first_path = temporary_pdf_path("external-drop-first");
    let second_path = temporary_pdf_path("external-drop-second");
    let unsupported_path = temporary_pdf_path("external-drop-unsupported").with_extension("txt");
    write_minimal_pdf(&first_path);
    write_minimal_pdf(&second_path);
    fs::write(&unsupported_path, b"not a document").unwrap();

    cx.update(|cx| {
        gpui_component::init(cx);
        glyphst_ui::activity::init(cx);
        cx.set_global(SettingsStore::memory());
        cx.set_global(LibraryStore::memory());
        cx.set_global(RenderCache::new());
    });
    let callbacks = WorkspaceCallbacks {
        open_workspace_window: unavailable_workspace_window,
        open_settings_window: ignore_settings_window,
    };
    let (root, cx) = cx.add_window_view(|window, cx| {
        let workspace = cx.new(|cx| Workspace::new(None, None, callbacks, window, cx));
        Root::new(workspace, window, cx)
    });
    let workspace = root.read_with(cx, |root, _| {
        root.view().clone().downcast::<Workspace>().unwrap()
    });

    workspace.update_in(cx, |workspace, window, cx| {
        workspace.open_external_document_paths(
            &[
                first_path.clone(),
                unsupported_path.clone(),
                second_path.clone(),
            ],
            window,
            cx,
        );
        assert_eq!(window.notifications(cx).len(), 1);
    });
    cx.run_until_parked();

    workspace.read_with(cx, |workspace, cx| {
        assert_eq!(workspace.surface_mode, WorkspaceSurfaceMode::Panes);
        let pane = workspace.active_pane.read(cx);
        assert_eq!(pane.len(), 2);
        assert_eq!(pane.active_tab_ix(), 1);
        let paths = pane
            .document_views()
            .into_iter()
            .map(|view| view.read(cx).path().to_path_buf())
            .collect::<Vec<_>>();
        assert_eq!(paths, vec![first_path.clone(), second_path.clone()]);
    });

    fs::remove_file(first_path).unwrap();
    fs::remove_file(second_path).unwrap();
    fs::remove_file(unsupported_path).unwrap();
}

#[gpui::test]
fn pane_drop_opens_documents_in_the_hovered_inactive_pane(cx: &mut TestAppContext) {
    let first_path = temporary_pdf_path("pane-drop-first");
    let second_path = temporary_pdf_path("pane-drop-second");
    let unsupported_path = temporary_pdf_path("pane-drop-unsupported").with_extension("txt");
    write_minimal_pdf(&first_path);
    write_minimal_pdf(&second_path);
    fs::write(&unsupported_path, b"not a document").unwrap();

    cx.update(|cx| {
        gpui_component::init(cx);
        glyphst_ui::activity::init(cx);
        cx.set_global(SettingsStore::memory());
        cx.set_global(LibraryStore::memory());
        cx.set_global(RenderCache::new());
    });
    let callbacks = WorkspaceCallbacks {
        open_workspace_window: unavailable_workspace_window,
        open_settings_window: ignore_settings_window,
    };
    let (root, cx) = cx.add_window_view(|window, cx| {
        let workspace = cx.new(|cx| Workspace::new(None, None, callbacks, window, cx));
        Root::new(workspace, window, cx)
    });
    let workspace = root.read_with(cx, |root, _| {
        root.view().clone().downcast::<Workspace>().unwrap()
    });
    let hovered_pane = workspace.read_with(cx, |workspace, _| workspace.active_pane.clone());
    workspace.update_in(cx, |workspace, window, cx| {
        workspace.leave_home(window, cx);
        workspace.split_active_pane(SplitDirection::Right, window, cx);
    });
    let previously_active_pane =
        workspace.read_with(cx, |workspace, _| workspace.active_pane.clone());
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    let drop_position = workspace.read_with(cx, |workspace, _| {
        workspace
            .pane_group
            .bounding_box_for_pane(&hovered_pane)
            .expect("hovered pane bounds")
            .center()
    });
    cx.simulate_event(FileDropEvent::Entered {
        position: drop_position,
        paths: ExternalPaths(
            [
                first_path.clone(),
                unsupported_path.clone(),
                second_path.clone(),
            ]
            .into_iter()
            .collect(),
        ),
    });
    cx.simulate_event(FileDropEvent::Submit {
        position: drop_position,
    });
    cx.run_until_parked();

    workspace.update_in(cx, |workspace, window, cx| {
        assert_eq!(workspace.active_pane, hovered_pane);
        assert_eq!(window.notifications(cx).len(), 1);
        let hovered = hovered_pane.read(cx);
        assert_eq!(hovered.len(), 2);
        assert_eq!(hovered.active_tab_ix(), 1);
        let paths = hovered
            .document_views()
            .into_iter()
            .map(|view| view.read(cx).path().to_path_buf())
            .collect::<Vec<_>>();
        assert_eq!(paths, vec![first_path.clone(), second_path.clone()]);
        let previous = previously_active_pane.read(cx);
        assert_eq!(previous.len(), 0);
    });

    fs::remove_file(first_path).unwrap();
    fs::remove_file(second_path).unwrap();
    fs::remove_file(unsupported_path).unwrap();
}

#[gpui::test]
fn unsupported_pane_drop_does_not_activate_the_hovered_pane(cx: &mut TestAppContext) {
    let unsupported_path = temporary_pdf_path("pane-drop-invalid").with_extension("txt");
    fs::write(&unsupported_path, b"not a document").unwrap();

    cx.update(|cx| {
        gpui_component::init(cx);
        glyphst_ui::activity::init(cx);
        cx.set_global(SettingsStore::memory());
        cx.set_global(LibraryStore::memory());
        cx.set_global(RenderCache::new());
    });
    let callbacks = WorkspaceCallbacks {
        open_workspace_window: unavailable_workspace_window,
        open_settings_window: ignore_settings_window,
    };
    let (root, cx) = cx.add_window_view(|window, cx| {
        let workspace = cx.new(|cx| Workspace::new(None, None, callbacks, window, cx));
        Root::new(workspace, window, cx)
    });
    let workspace = root.read_with(cx, |root, _| {
        root.view().clone().downcast::<Workspace>().unwrap()
    });
    let hovered_pane = workspace.read_with(cx, |workspace, _| workspace.active_pane.clone());
    workspace.update_in(cx, |workspace, window, cx| {
        workspace.leave_home(window, cx);
        workspace.split_active_pane(SplitDirection::Right, window, cx);
    });
    let active_pane = workspace.read_with(cx, |workspace, _| workspace.active_pane.clone());
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    let drop_position = workspace.read_with(cx, |workspace, _| {
        workspace
            .pane_group
            .bounding_box_for_pane(&hovered_pane)
            .expect("hovered pane bounds")
            .center()
    });
    cx.simulate_event(FileDropEvent::Entered {
        position: drop_position,
        paths: ExternalPaths([unsupported_path.clone()].into_iter().collect()),
    });
    cx.simulate_event(FileDropEvent::Submit {
        position: drop_position,
    });
    cx.run_until_parked();

    workspace.update_in(cx, |workspace, window, cx| {
        assert_eq!(workspace.active_pane, active_pane);
        assert_eq!(window.notifications(cx).len(), 1);
        assert!(hovered_pane.read(cx).active_tab_is_new_tab());
    });

    fs::remove_file(unsupported_path).unwrap();
}

#[gpui::test]
fn external_drop_overlay_is_scoped_to_the_hovered_pane(cx: &mut TestAppContext) {
    let document_path = temporary_pdf_path("pane-drop-overlay");
    write_minimal_pdf(&document_path);
    cx.update(|cx| {
        gpui_component::init(cx);
        cx.set_global(SettingsStore::memory());
        cx.set_global(LibraryStore::memory());
        cx.set_global(RenderCache::new());
    });
    let callbacks = WorkspaceCallbacks {
        open_workspace_window: unavailable_workspace_window,
        open_settings_window: ignore_settings_window,
    };
    let (root, cx) = cx.add_window_view(|window, cx| {
        let workspace = cx.new(|cx| Workspace::new(None, None, callbacks, window, cx));
        Root::new(workspace, window, cx)
    });
    let workspace = root.read_with(cx, |root, _| {
        root.view().clone().downcast::<Workspace>().unwrap()
    });
    let hovered_pane = workspace.read_with(cx, |workspace, _| workspace.active_pane.clone());
    workspace.update_in(cx, |workspace, window, cx| {
        workspace.leave_home(window, cx);
        workspace.split_active_pane(SplitDirection::Right, window, cx);
    });
    let other_pane = workspace.read_with(cx, |workspace, _| workspace.active_pane.clone());
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    let drop_position = workspace.read_with(cx, |workspace, _| {
        workspace
            .pane_group
            .bounding_box_for_pane(&hovered_pane)
            .expect("hovered pane bounds")
            .center()
    });
    cx.simulate_event(FileDropEvent::Entered {
        position: drop_position,
        paths: ExternalPaths([document_path.clone()].into_iter().collect()),
    });
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });

    let hovered_selector = Box::leak(
        format!(
            "pane-external-document-drop-overlay-{}-accepted",
            hovered_pane.entity_id().as_u64()
        )
        .into_boxed_str(),
    );
    let other_selector = Box::leak(
        format!(
            "pane-external-document-drop-overlay-{}-accepted",
            other_pane.entity_id().as_u64()
        )
        .into_boxed_str(),
    );
    let overlay_bounds = cx
        .debug_bounds(hovered_selector)
        .expect("hovered pane drop overlay");
    let (hovered_bounds, other_bounds) = workspace.read_with(cx, |workspace, _| {
        (
            workspace
                .pane_group
                .bounding_box_for_pane(&hovered_pane)
                .expect("hovered pane bounds"),
            workspace
                .pane_group
                .bounding_box_for_pane(&other_pane)
                .expect("other pane bounds"),
        )
    });
    assert!(overlay_bounds.left() >= hovered_bounds.left());
    assert!(overlay_bounds.top() >= hovered_bounds.top());
    assert!(overlay_bounds.right() <= hovered_bounds.right());
    assert!(overlay_bounds.bottom() <= hovered_bounds.bottom());
    assert!(!overlay_bounds.intersects(&other_bounds));
    assert!(cx.debug_bounds(other_selector).is_none());
    assert_ne!(
        overlay_bounds,
        cx.debug_bounds("workspace-pane-column").unwrap()
    );

    cx.simulate_event(FileDropEvent::Pending {
        position: other_bounds.center(),
    });
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    assert!(cx.debug_bounds(hovered_selector).is_none());
    let other_overlay_bounds = cx
        .debug_bounds(other_selector)
        .expect("newly hovered pane drop overlay");
    assert!(other_overlay_bounds.left() >= other_bounds.left());
    assert!(other_overlay_bounds.top() >= other_bounds.top());
    assert!(other_overlay_bounds.right() <= other_bounds.right());
    assert!(other_overlay_bounds.bottom() <= other_bounds.bottom());
    assert!(!other_overlay_bounds.intersects(&hovered_bounds));

    cx.simulate_event(FileDropEvent::Exited);
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    assert!(cx.debug_bounds(other_selector).is_none());
    fs::remove_file(document_path).unwrap();
}

#[gpui::test]
fn compact_title_bar_menus_switch_without_changing_layout(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_component::init(cx);
        cx.set_global(SettingsStore::memory());
        cx.set_global(LibraryStore::memory());
        cx.set_global(RenderCache::new());
    });
    let callbacks = WorkspaceCallbacks {
        open_workspace_window: unavailable_workspace_window,
        open_settings_window: ignore_settings_window,
    };

    let (root, cx) = cx.add_window_view(|window, cx| {
        let workspace = cx.new(|cx| Workspace::new_empty(callbacks, window, cx));
        Root::new(workspace, window, cx)
    });
    let workspace = root.read_with(cx, |root, _| {
        root.view().clone().downcast::<Workspace>().unwrap()
    });
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });

    assert!(cx.debug_bounds("workspace-menu-glyphst").is_some());
    assert!(cx.debug_bounds("workspace-menu-file").is_some());
    assert!(cx.debug_bounds("workspace-menu-tab").is_some());
    let home = cx.debug_bounds("workspace-home").unwrap();
    let glyphst_menu = cx.debug_bounds("workspace-menu-glyphst").unwrap();
    assert!(home.right() <= glyphst_menu.left());

    let glyphst = cx.debug_bounds("workspace-menu-glyphst").unwrap().center();
    cx.simulate_click(glyphst, Modifiers::default());
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });

    assert!(cx.debug_bounds("workspace-menu-glyphst").is_some());
    assert!(cx.debug_bounds("workspace-menu-file").is_some());
    assert!(cx.debug_bounds("workspace-menu-tab").is_some());
    let glyphst_popup = workspace.read_with(cx, |workspace, _| {
        assert!(workspace.title_bar_menu_expanded);
        assert_eq!(workspace.title_bar_open_menu, Some(WorkspaceMenu::Glyphst));
        workspace.title_bar_popup_menu.as_ref().unwrap().entity_id()
    });

    workspace.update_in(cx, |workspace, window, cx| {
        workspace.hover_title_bar_menu(WorkspaceMenu::File, window, cx);
    });
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    workspace.read_with(cx, |workspace, _| {
        assert_eq!(workspace.title_bar_open_menu, Some(WorkspaceMenu::File));
        assert_ne!(
            workspace.title_bar_popup_menu.as_ref().unwrap().entity_id(),
            glyphst_popup
        );
    });

    let drag_region = cx
        .debug_bounds("workspace-title-bar-drag-region")
        .unwrap()
        .center();
    cx.simulate_click(drag_region, Modifiers::default());
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });

    assert!(cx.debug_bounds("workspace-menu-glyphst").is_some());
    workspace.read_with(cx, |workspace, _| {
        assert!(!workspace.title_bar_menu_expanded);
        assert_eq!(workspace.title_bar_open_menu, None);
    });
}

#[gpui::test]
fn root_hosted_workspace_tabs_render_and_interact(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_component::init(cx);
        cx.set_global(SettingsStore::memory());
        cx.set_global(LibraryStore::memory());
        cx.set_global(RenderCache::new());
    });
    let callbacks = WorkspaceCallbacks {
        open_workspace_window: unavailable_workspace_window,
        open_settings_window: ignore_settings_window,
    };

    let (root, cx) = cx.add_window_view(|window, cx| {
        let workspace = cx.new(|cx| Workspace::new(None, None, callbacks, window, cx));
        Root::new(workspace, window, cx)
    });
    let workspace = root.read_with(cx, |root, _| {
        root.view().clone().downcast::<Workspace>().unwrap()
    });
    workspace.update_in(cx, |workspace, window, cx| {
        workspace.open_new_tab(window, cx);
        workspace.open_new_tab(window, cx);
    });
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });

    assert!(cx.debug_bounds("workspace-main-card").is_none());
    assert!(cx.debug_bounds("workspace-pane-column").is_some());
    assert!(cx.debug_bounds("pane-tab-0").is_some());
    assert!(cx.debug_bounds("pane-tab-1").is_some());
    assert!(cx.debug_bounds("active-pane-surface").is_none());
    let first_tab_bounds = cx.debug_bounds("pane-tab-0").unwrap();
    let second_tab_bounds = cx.debug_bounds("pane-tab-1").unwrap();
    assert_eq!(second_tab_bounds.left() - first_tab_bounds.right(), px(2.0));

    let first_tab = cx.debug_bounds("pane-tab-0").unwrap().center();
    cx.simulate_click(first_tab, Modifiers::default());
    workspace.read_with(cx, |workspace, cx| {
        assert_eq!(workspace.active_pane.read(cx).active_tab_ix(), 0);
    });

    let new_tab = cx.debug_bounds("pane-new-tab").unwrap().center();
    cx.simulate_click(new_tab, Modifiers::default());
    workspace.read_with(cx, |workspace, cx| {
        let pane = workspace.active_pane.read(cx);
        assert_eq!(pane.len(), 3);
        assert_eq!(pane.active_tab_ix(), 2);
    });
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });

    let close_tab = cx.debug_bounds("pane-tab-close-2").unwrap().center();
    cx.simulate_click(close_tab, Modifiers::default());
    workspace.read_with(cx, |workspace, cx| {
        assert_eq!(workspace.active_pane.read(cx).len(), 2);
    });
}

#[gpui::test]
fn split_pane_cards_keep_nested_gutters_and_move_the_active_border(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_component::init(cx);
        cx.set_global(SettingsStore::memory());
        cx.set_global(LibraryStore::memory());
        cx.set_global(RenderCache::new());
    });
    let callbacks = WorkspaceCallbacks {
        open_workspace_window: unavailable_workspace_window,
        open_settings_window: ignore_settings_window,
    };

    let (root, cx) = cx.add_window_view(|window, cx| {
        let workspace = cx.new(|cx| Workspace::new(None, None, callbacks, window, cx));
        Root::new(workspace, window, cx)
    });
    let workspace = root.read_with(cx, |root, _| {
        root.view().clone().downcast::<Workspace>().unwrap()
    });
    let top_left = workspace.read_with(cx, |workspace, _| workspace.active_pane.clone());
    workspace.update_in(cx, |workspace, window, cx| {
        workspace.leave_home(window, cx);
        workspace.split_active_pane(SplitDirection::Right, window, cx);
    });
    let top_right = workspace.read_with(cx, |workspace, _| workspace.active_pane.clone());
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });

    let (initial_left_bounds, initial_right_bounds) = workspace.read_with(cx, |workspace, _| {
        (
            workspace
                .pane_group
                .bounding_box_for_pane(&top_left)
                .expect("left pane bounds"),
            workspace
                .pane_group
                .bounding_box_for_pane(&top_right)
                .expect("right pane bounds"),
        )
    });
    let active_surface = cx
        .debug_bounds("active-pane-surface")
        .expect("right active pane surface");
    assert_eq!(active_surface, initial_right_bounds);
    assert!(
        active_surface.left() - initial_left_bounds.right() >= px(glyphst_ui::style::SURFACE_GAP)
    );

    workspace.update_in(cx, |workspace, window, cx| {
        workspace.split_active_pane(SplitDirection::Down, window, cx);
    });
    let bottom_right = workspace.read_with(cx, |workspace, _| workspace.active_pane.clone());
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });

    let (right_top_bounds, right_bottom_bounds) = workspace.read_with(cx, |workspace, _| {
        (
            workspace
                .pane_group
                .bounding_box_for_pane(&top_right)
                .expect("top-right pane bounds"),
            workspace
                .pane_group
                .bounding_box_for_pane(&bottom_right)
                .expect("bottom-right pane bounds"),
        )
    });
    let active_surface = cx
        .debug_bounds("active-pane-surface")
        .expect("bottom active pane surface");
    assert_eq!(active_surface, right_bottom_bounds);
    assert!(active_surface.top() - right_top_bounds.bottom() >= px(glyphst_ui::style::SURFACE_GAP));

    workspace.update_in(cx, |workspace, window, cx| {
        workspace.active_pane = top_left.clone();
        workspace.split_active_pane(SplitDirection::Down, window, cx);
    });
    let bottom_left = workspace.read_with(cx, |workspace, _| workspace.active_pane.clone());
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });

    let (top_left_bounds, top_right_bounds, bottom_left_bounds, bottom_right_bounds) = workspace
        .read_with(cx, |workspace, _| {
            let bounds = |pane| {
                workspace
                    .pane_group
                    .bounding_box_for_pane(pane)
                    .expect("pane bounds")
            };
            (
                bounds(&top_left),
                bounds(&top_right),
                bounds(&bottom_left),
                bounds(&bottom_right),
            )
        });
    assert_eq!(top_right_bounds.left() - top_left_bounds.right(), px(4.0));
    assert_eq!(
        bottom_right_bounds.left() - bottom_left_bounds.right(),
        px(4.0)
    );
    assert_eq!(bottom_left_bounds.top() - top_left_bounds.bottom(), px(4.0));
    assert_eq!(
        bottom_right_bounds.top() - top_right_bounds.bottom(),
        px(4.0)
    );
    workspace.read_with(cx, |workspace, _| {
        assert_eq!(
            workspace
                .pane_group
                .find_pane_in_direction(&top_left, SplitDirection::Right),
            Some(top_right.clone())
        );
        assert_eq!(
            workspace
                .pane_group
                .find_pane_in_direction(&top_right, SplitDirection::Left),
            Some(top_left.clone())
        );
        assert_eq!(
            workspace
                .pane_group
                .find_pane_in_direction(&top_left, SplitDirection::Down),
            Some(bottom_left.clone())
        );
    });

    let active_surface = cx
        .debug_bounds("active-pane-surface")
        .expect("active pane surface");
    assert_eq!(active_surface, bottom_left_bounds);

    cx.simulate_click(top_right_bounds.center(), Modifiers::default());
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    workspace.read_with(cx, |workspace, _| {
        assert_eq!(workspace.active_pane, top_right);
    });

    let active_surface = cx
        .debug_bounds("active-pane-surface")
        .expect("clicked pane surface");
    assert_eq!(active_surface, top_right_bounds);
}

#[gpui::test]
fn open_sidebar_survives_document_tab_changes(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_component::init(cx);
        cx.set_reduce_motion(true);
        cx.set_global(SettingsStore::memory());
        cx.set_global(LibraryStore::memory());
        cx.set_global(RenderCache::new());
    });
    let callbacks = WorkspaceCallbacks {
        open_workspace_window: unavailable_workspace_window,
        open_settings_window: ignore_settings_window,
    };
    let first_path = temporary_pdf_path("sidebar-first");
    let second_path = temporary_pdf_path("sidebar-second");
    write_minimal_pdf(&first_path);
    write_minimal_pdf(&second_path);

    let (root, cx) = cx.add_window_view(|window, cx| {
        let workspace = cx.new(|cx| Workspace::new(None, None, callbacks, window, cx));
        Root::new(workspace, window, cx)
    });
    let workspace = root.read_with(cx, |root, _| {
        root.view().clone().downcast::<Workspace>().unwrap()
    });
    workspace.update_in(cx, |workspace, window, cx| {
        workspace
            .focus_or_open_document(first_path.clone(), None, true, window, cx)
            .unwrap();
    });
    cx.run_until_parked();
    workspace.update_in(cx, |workspace, window, cx| {
        workspace
            .focus_or_open_document(second_path.clone(), None, false, window, cx)
            .unwrap();
    });
    cx.run_until_parked();

    workspace.update_in(cx, |workspace, window, cx| {
        workspace.toggle_left_sidebar(DocumentSidebar::Toc, window, cx);
    });
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    assert!(cx.debug_bounds("toc-sidebar-content").is_some());

    let first_tab = cx.debug_bounds("pane-tab-0").unwrap().center();
    cx.simulate_click(first_tab, Modifiers::default());
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    workspace.read_with(cx, |workspace, cx| {
        assert_eq!(workspace.docks.left_sidebar, Some(DocumentSidebar::Toc));
        assert_eq!(workspace.active_pane.read(cx).active_tab_ix(), 0);
    });
    assert!(cx.debug_bounds("toc-sidebar-content").is_some());

    let second_tab = cx.debug_bounds("pane-tab-1").unwrap().center();
    cx.simulate_click(second_tab, Modifiers::default());
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    workspace.read_with(cx, |workspace, cx| {
        assert_eq!(workspace.docks.left_sidebar, Some(DocumentSidebar::Toc));
        assert_eq!(workspace.active_pane.read(cx).active_tab_ix(), 1);
    });
    assert!(cx.debug_bounds("toc-sidebar-content").is_some());

    fs::remove_file(first_path).unwrap();
    fs::remove_file(second_path).unwrap();
}

#[gpui::test]
fn single_pane_new_tab_suspends_document_sidebar_but_keeps_global_ai(cx: &mut TestAppContext) {
    let document_path = temporary_pdf_path("new-tab-sticky-docks");
    write_minimal_pdf(&document_path);
    cx.update(|cx| {
        gpui_component::init(cx);
        cx.set_reduce_motion(true);
        cx.set_global(SettingsStore::memory());
        cx.set_global(LibraryStore::memory());
        cx.set_global(RenderCache::new());
    });
    let callbacks = WorkspaceCallbacks {
        open_workspace_window: unavailable_workspace_window,
        open_settings_window: ignore_settings_window,
    };
    let initial_path = document_path.clone();
    let (root, cx) = cx.add_window_view(move |window, cx| {
        let workspace =
            cx.new(|cx| Workspace::new(Some(initial_path.clone()), None, callbacks, window, cx));
        Root::new(workspace, window, cx)
    });
    let workspace = root.read_with(cx, |root, _| {
        root.view().clone().downcast::<Workspace>().unwrap()
    });
    cx.run_until_parked();

    let document_id = workspace.update_in(cx, |workspace, window, cx| {
        let document_id = workspace
            .active_pane
            .read(cx)
            .active_document_view()
            .unwrap()
            .entity_id();
        workspace.toggle_left_sidebar(DocumentSidebar::Toc, window, cx);
        workspace.toggle_ai_dock(window, cx);
        workspace.open_new_tab(window, cx);
        document_id
    });
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });

    workspace.read_with(cx, |workspace, cx| {
        assert!(workspace.active_pane.read(cx).active_tab_is_new_tab());
        assert!(workspace.dock_document_target(cx).is_none());
        assert_eq!(workspace.docks.left_sidebar, Some(DocumentSidebar::Toc));
        assert!(workspace.docks.ai_panel_open);
    });
    assert!(cx.debug_bounds("workspace-left-dock").is_none());
    assert!(cx.debug_bounds("workspace-ai-dock").is_some());
    assert!(cx.debug_bounds("toc-sidebar-content").is_none());

    workspace.update_in(cx, |workspace, window, cx| {
        let pane = workspace.active_pane.clone();
        assert!(pane.update(cx, |pane, cx| {
            pane.activate_tab_by_id(document_id, true, window, cx)
        }));
        workspace.record_document_context(&pane, cx);
    });
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    workspace.read_with(cx, |workspace, cx| {
        assert_eq!(
            workspace.active_pane.read(cx).active_tab_id(),
            Some(document_id)
        );
    });
    assert!(cx.debug_bounds("workspace-left-dock").is_some());
    assert!(cx.debug_bounds("workspace-ai-dock").is_some());
    assert!(cx.debug_bounds("toc-sidebar-content").is_some());

    fs::remove_file(document_path).unwrap();
}

#[gpui::test]
fn closing_new_tab_in_inactive_pane_does_not_activate_it_or_hide_sidebar(cx: &mut TestAppContext) {
    let document_path = temporary_pdf_path("inactive-pane-new-tab-close");
    write_minimal_pdf(&document_path);
    cx.update(|cx| {
        gpui_component::init(cx);
        cx.set_reduce_motion(true);
        cx.set_global(SettingsStore::memory());
        cx.set_global(LibraryStore::memory());
        cx.set_global(RenderCache::new());
    });
    let callbacks = WorkspaceCallbacks {
        open_workspace_window: unavailable_workspace_window,
        open_settings_window: ignore_settings_window,
    };
    let initial_path = document_path.clone();
    let (root, cx) = cx.add_window_view(move |window, cx| {
        let workspace =
            cx.new(|cx| Workspace::new(Some(initial_path.clone()), None, callbacks, window, cx));
        Root::new(workspace, window, cx)
    });
    let workspace = root.read_with(cx, |root, _| {
        root.view().clone().downcast::<Workspace>().unwrap()
    });
    cx.run_until_parked();
    workspace.update_in(cx, |workspace, window, cx| {
        workspace.split_active_pane(SplitDirection::Right, window, cx);
    });
    cx.run_until_parked();

    let (left_pane, right_pane, left_document_id) =
        workspace.update_in(cx, |workspace, window, cx| {
            let panes = workspace.pane_group.panes();
            let left_pane = panes[0].clone();
            let right_pane = panes[1].clone();
            right_pane.update(cx, |pane, cx| pane.open_new_tab(window, cx));
            workspace.active_pane = left_pane.clone();
            left_pane.update(cx, |pane, cx| pane.focus(window, cx));
            workspace.record_document_context(&left_pane, cx);
            workspace.toggle_left_sidebar(DocumentSidebar::Toc, window, cx);
            let left_document_id = left_pane
                .read(cx)
                .active_document_view()
                .unwrap()
                .entity_id();
            (left_pane, right_pane, left_document_id)
        });
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    assert!(right_pane.read_with(cx, |pane, _| pane.active_tab_is_new_tab()));
    assert!(cx.debug_bounds("toc-sidebar-content").is_some());

    let close = cx
        .debug_bounds("pane-tab-close-1")
        .expect("inactive pane New Tab close button")
        .center();
    cx.simulate_mouse_down(close, MouseButton::Left, Modifiers::default());
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    workspace.read_with(cx, |workspace, cx| {
        assert_eq!(workspace.active_pane, left_pane);
        assert_eq!(
            workspace
                .dock_document_target(cx)
                .map(|(_, document)| document.entity_id()),
            Some(left_document_id)
        );
    });
    assert!(cx.debug_bounds("toc-sidebar-content").is_some());

    cx.simulate_mouse_up(close, MouseButton::Left, Modifiers::default());
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    workspace.read_with(cx, |workspace, cx| {
        assert_eq!(workspace.active_pane, left_pane);
        assert_eq!(right_pane.read(cx).len(), 1);
        assert!(!right_pane.read(cx).active_tab_is_new_tab());
        assert_eq!(
            workspace
                .dock_document_target(cx)
                .map(|(_, document)| document.entity_id()),
            Some(left_document_id)
        );
    });
    assert!(cx.debug_bounds("toc-sidebar-content").is_some());

    fs::remove_file(document_path).unwrap();
}

#[gpui::test]
fn removing_dock_owner_pane_does_not_fall_back_to_hidden_document(cx: &mut TestAppContext) {
    let document_path = temporary_pdf_path("removed-owner-hidden-document");
    write_minimal_pdf(&document_path);
    cx.update(|cx| {
        gpui_component::init(cx);
        cx.set_reduce_motion(true);
        cx.set_global(SettingsStore::memory());
        cx.set_global(LibraryStore::memory());
        cx.set_global(RenderCache::new());
    });
    let callbacks = WorkspaceCallbacks {
        open_workspace_window: unavailable_workspace_window,
        open_settings_window: ignore_settings_window,
    };
    let initial_path = document_path.clone();
    let (root, cx) = cx.add_window_view(move |window, cx| {
        let workspace =
            cx.new(|cx| Workspace::new(Some(initial_path.clone()), None, callbacks, window, cx));
        Root::new(workspace, window, cx)
    });
    let workspace = root.read_with(cx, |root, _| {
        root.view().clone().downcast::<Workspace>().unwrap()
    });
    cx.run_until_parked();
    workspace.update_in(cx, |workspace, window, cx| {
        workspace.split_active_pane(SplitDirection::Right, window, cx);
    });
    cx.run_until_parked();

    let (left_pane, right_pane, right_document_id) =
        workspace.update_in(cx, |workspace, window, cx| {
            let panes = workspace.pane_group.panes();
            let left_pane = panes[0].clone();
            let right_pane = panes[1].clone();
            let right_document_id = right_pane
                .read(cx)
                .active_document_view()
                .expect("split document")
                .entity_id();
            right_pane.update(cx, |pane, cx| pane.open_new_tab(window, cx));
            workspace.active_pane = left_pane.clone();
            left_pane.update(cx, |pane, cx| pane.focus(window, cx));
            workspace.record_document_context(&left_pane, cx);
            workspace.toggle_left_sidebar(DocumentSidebar::Toc, window, cx);
            workspace.active_pane = right_pane.clone();
            right_pane.update(cx, |pane, cx| pane.focus(window, cx));
            (left_pane, right_pane, right_document_id)
        });
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    assert!(cx.debug_bounds("toc-sidebar-content").is_some());

    workspace.update_in(cx, |workspace, window, cx| {
        workspace.close_tab_in_pane_now(&left_pane, None, window, cx);
    });
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    workspace.read_with(cx, |workspace, cx| {
        assert_eq!(workspace.pane_group.panes().len(), 1);
        assert_eq!(workspace.active_pane, right_pane);
        assert!(right_pane.read(cx).active_tab_is_new_tab());
        assert!(workspace.dock_document_target(cx).is_none());
        assert_eq!(workspace.docks.left_sidebar, Some(DocumentSidebar::Toc));
    });
    assert!(cx.debug_bounds("workspace-left-dock").is_none());
    assert!(cx.debug_bounds("toc-sidebar-content").is_none());

    workspace.update_in(cx, |workspace, window, cx| {
        assert!(right_pane.update(cx, |pane, cx| {
            pane.activate_tab_by_id(right_document_id, true, window, cx)
        }));
        workspace.record_document_context(&right_pane, cx);
    });
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    workspace.read_with(cx, |workspace, cx| {
        assert_eq!(
            workspace
                .dock_document_target(cx)
                .map(|(_, document)| document.entity_id()),
            Some(right_document_id)
        );
    });
    assert!(cx.debug_bounds("toc-sidebar-content").is_some());

    fs::remove_file(document_path).unwrap();
}
