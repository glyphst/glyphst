use std::{fs, path::PathBuf, time::Duration};

use gpui::{AppContext as _, EntityId, TestAppContext};
use gpui_component::Root;

use super::{
    DraggedTab, LoadingDocumentView, Pane, Tab, loading_grid_opacity,
    parse_debug_file_open_min_duration, remaining_file_open_min_duration,
};
use crate::{
    document_view::RenderCache, library::LibraryStore, settings::SettingsStore,
    synchronized_tabs::SynchronizedTabRegistry,
};
use glyphst_translation_ui::DocumentTranslationJobManager;

fn temporary_pdf_path(label: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "glyphst-{label}-{}-{}.pdf",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

fn write_minimal_pdf(path: &std::path::Path) {
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>",
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << >> /Contents 4 0 R >>",
        "<< /Length 0 >>\nstream\n\nendstream",
    ];
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

#[test]
fn loading_grid_opacity_spans_the_expected_wave() {
    assert!((loading_grid_opacity(0.0, 0.0) - 0.2).abs() < f32::EPSILON);
    assert!((loading_grid_opacity(0.5, 0.0) - 1.0).abs() < f32::EPSILON);

    for phase in super::LOADING_GRID_PHASES {
        for step in 0..=100 {
            let opacity = loading_grid_opacity(step as f32 / 100.0, phase);
            assert!((0.2..=1.0).contains(&opacity));
        }
    }
}

#[gpui::test]
fn rendered_non_current_group_tab_has_link_marker_and_pause_indicator(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_component::init(cx);
        cx.set_global(SettingsStore::memory());
        cx.set_global(LibraryStore::memory());
        cx.set_global(RenderCache::new());
    });
    let jobs = cx.update(|cx| cx.new(DocumentTranslationJobManager::new));

    let (root, cx) = cx.add_window_view(|window, cx| {
        let pane = cx.new(|cx| Pane::empty(cx.focus_handle(), jobs.clone()));
        Root::new(pane, window, cx)
    });
    let pane = root.read_with(cx, |root, _| {
        root.view().clone().downcast::<Pane>().unwrap()
    });
    pane.update_in(cx, |pane, window, cx| {
        pane.set_force_show_tab_bar(true, cx);
        pane.open_new_tab(window, cx);
    });
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });

    let tab_id = pane.read_with(cx, |pane, _| {
        let tab_id = Pane::tab_entity_id(&pane.tabs[0]);
        let measured = pane.tab_bounds.borrow().get(&tab_id).copied().unwrap();
        let body = measured.body.unwrap();
        let suffix = measured.suffix.unwrap();
        let visual = pane.tab_bounds(tab_id).unwrap();

        assert!(
            suffix.right() > body.right(),
            "the suffix must expose the measurement regression: body={body:?}, suffix={suffix:?}"
        );
        assert_eq!(visual, body.union(&suffix));
        assert!(visual.contains(&suffix.center()));
        assert!(visual.center().x > body.center().x);
        tab_id
    });

    let mut registry = SynchronizedTabRegistry::default();
    registry
        .create_group(
            tab_id,
            pane.entity_id(),
            EntityId::from(1001),
            EntityId::from(1002),
        )
        .unwrap();
    registry
        .create_group(
            EntityId::from(2001),
            EntityId::from(2002),
            EntityId::from(2003),
            EntityId::from(2004),
        )
        .unwrap();
    let summary = registry.summary_for_tab(tab_id).unwrap();
    assert!(!summary.is_current);
    pane.update_in(cx, |pane, _, cx| {
        pane.synchronized_presentation
            .group_by_tab
            .insert(tab_id, summary);
        cx.notify();
    });
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    assert!(cx.debug_bounds("synchronized-tab-group-link").is_some());

    pane.update_in(cx, |pane, _, cx| {
        pane.synchronized_presentation.paused_tabs.insert(tab_id);
        cx.notify();
    });
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    assert!(cx.debug_bounds("synchronized-tab-paused").is_some());
}

#[test]
fn debug_file_open_min_duration_is_parsed_as_positive_milliseconds() {
    assert_eq!(
        parse_debug_file_open_min_duration("3000"),
        Some(Duration::from_secs(3))
    );
    assert_eq!(
        parse_debug_file_open_min_duration(" 42 "),
        Some(Duration::from_millis(42))
    );
    assert_eq!(parse_debug_file_open_min_duration("0"), None);
    assert_eq!(parse_debug_file_open_min_duration("-1"), None);
    assert_eq!(parse_debug_file_open_min_duration("invalid"), None);
}

#[test]
fn file_open_min_duration_only_waits_for_the_remaining_time() {
    let minimum = Some(Duration::from_secs(3));

    assert_eq!(
        remaining_file_open_min_duration(minimum, Duration::from_secs(1)),
        Some(Duration::from_secs(2))
    );
    assert_eq!(
        remaining_file_open_min_duration(minimum, Duration::from_secs(3)),
        None
    );
    assert_eq!(
        remaining_file_open_min_duration(minimum, Duration::from_secs(4)),
        None
    );
    assert_eq!(remaining_file_open_min_duration(None, Duration::ZERO), None);
}

#[gpui::test]
fn loading_document_view_switches_from_animated_grid_to_error(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_component::init(cx);
        cx.set_global(SettingsStore::memory());
    });

    let (root, cx) = cx.add_window_view(|window, cx| {
        let loading = cx.new(|cx| LoadingDocumentView {
            path: PathBuf::from("/tmp/example.pdf"),
            focus_handle: cx.focus_handle(),
            job_ids: Vec::new(),
            error: None,
            _task: None,
        });
        Root::new(loading, window, cx)
    });
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });

    assert!(cx.debug_bounds("document-open-loading").is_some());
    assert!(cx.debug_bounds("document-open-loading-grid").is_some());
    assert!(cx.debug_bounds("document-open-loading-error").is_none());

    let loading = root.read_with(cx, |root, _| {
        root.view()
            .clone()
            .downcast::<LoadingDocumentView>()
            .unwrap()
    });
    loading.update(cx, |loading, cx| {
        loading.error = Some("failed".into());
        cx.notify();
    });
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });

    assert!(cx.debug_bounds("document-open-loading").is_none());
    assert!(cx.debug_bounds("document-open-loading-grid").is_none());
    assert!(cx.debug_bounds("document-open-loading-error").is_some());
}

#[gpui::test]
fn document_open_stages_a_loading_tab_without_blocking_tab_switches(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_component::init(cx);
        cx.set_global(SettingsStore::memory());
        cx.set_global(LibraryStore::memory());
        cx.set_global(RenderCache::new());
    });
    let jobs = cx.update(|cx| cx.new(DocumentTranslationJobManager::new));
    let path = temporary_pdf_path("loading-tab");
    fs::create_dir(&path).unwrap();

    let (root, cx) = cx.add_window_view(|window, cx| {
        let pane = cx.new(|cx| Pane::empty(cx.focus_handle(), jobs.clone()));
        Root::new(pane, window, cx)
    });
    let pane = root.read_with(cx, |root, _| {
        root.view().clone().downcast::<Pane>().unwrap()
    });
    pane.update_in(cx, |pane, window, cx| {
        pane.focus_or_open_document(path.clone(), None, true, window, cx)
            .unwrap();
        assert!(matches!(pane.tabs.as_slice(), [Tab::LoadingDocument(_)]));

        pane.open_new_tab(window, cx);
        assert!(matches!(
            pane.tabs.get(pane.active_tab),
            Some(Tab::NewTab(_))
        ));
        assert!(
            pane.tabs
                .iter()
                .any(|tab| matches!(tab, Tab::LoadingDocument(_)))
        );
    });

    cx.run_until_parked();
    pane.read_with(cx, |pane, cx| {
        assert!(matches!(
            pane.tabs.get(pane.active_tab),
            Some(Tab::NewTab(_))
        ));
        let loading = pane
            .tabs
            .iter()
            .find_map(|tab| match tab {
                Tab::LoadingDocument(loading) => Some(loading),
                _ => None,
            })
            .unwrap();
        assert!(loading.read(cx).error.is_some());
    });

    fs::remove_dir(path).unwrap();
}

#[gpui::test]
fn successful_document_preparation_replaces_the_loading_tab(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_component::init(cx);
        cx.set_global(SettingsStore::memory());
        cx.set_global(LibraryStore::memory());
        cx.set_global(RenderCache::new());
    });
    let jobs = cx.update(|cx| cx.new(DocumentTranslationJobManager::new));
    let path = temporary_pdf_path("loading-success");
    write_minimal_pdf(&path);

    let (root, cx) = cx.add_window_view(|window, cx| {
        let pane = cx.new(|cx| Pane::empty(cx.focus_handle(), jobs.clone()));
        Root::new(pane, window, cx)
    });
    let pane = root.read_with(cx, |root, _| {
        root.view().clone().downcast::<Pane>().unwrap()
    });
    pane.update_in(cx, |pane, window, cx| {
        pane.focus_or_open_document(path.clone(), None, true, window, cx)
            .unwrap();
        assert!(matches!(pane.tabs.as_slice(), [Tab::LoadingDocument(_)]));
        pane.associate_open_job(0, 42, cx);
        let Tab::LoadingDocument(loading) = &pane.tabs[0] else {
            unreachable!();
        };
        assert_eq!(loading.read(cx).job_ids, vec![42]);
    });

    cx.run_until_parked();
    pane.read_with(cx, |pane, _| {
        assert!(matches!(pane.tabs.as_slice(), [Tab::Document(_)]));
    });

    fs::remove_file(path).unwrap();
}

#[gpui::test]
fn no_op_tab_drag_activates_the_dragged_tab(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_component::init(cx);
        cx.set_global(SettingsStore::memory());
        cx.set_global(LibraryStore::memory());
        cx.set_global(RenderCache::new());
    });
    let jobs = cx.update(|cx| cx.new(DocumentTranslationJobManager::new));

    let (root, cx) = cx.add_window_view(|window, cx| {
        let pane = cx.new(|cx| Pane::empty(cx.focus_handle(), jobs.clone()));
        Root::new(pane, window, cx)
    });
    let pane = root.read_with(cx, |root, _| {
        root.view().clone().downcast::<Pane>().unwrap()
    });
    let source_pane = pane.clone();

    pane.update_in(cx, |pane, window, cx| {
        pane.open_new_tab(window, cx);
        pane.open_new_tab(window, cx);
        assert_eq!(pane.active_tab_ix(), 1);

        let dragged_tab = DraggedTab {
            source_window: window.window_handle().downcast::<Root>().unwrap(),
            source_pane: source_pane.clone(),
            ix: 0,
            tab_id: Pane::tab_entity_id(&pane.tabs[0]),
            label: "First tab".into(),
        };

        pane.move_dragged_tab(&dragged_tab, 0, window, cx);
        assert_eq!(pane.active_tab_ix(), 0);

        pane.activate_tab(1, window, cx);
        pane.activate_dragged_tab(&dragged_tab, window, cx);
        assert_eq!(pane.active_tab_ix(), 0);
    });
}
