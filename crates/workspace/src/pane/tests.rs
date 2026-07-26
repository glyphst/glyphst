use std::fs;

use gpui::{AppContext as _, TestAppContext};
use gpui_component::Root;

use super::{Pane, Tab};
use crate::{library::LibraryStore, pdf_view::RenderCache, settings::SettingsStore};
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
