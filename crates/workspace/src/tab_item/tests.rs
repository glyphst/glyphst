use std::{fs, path::PathBuf, time::Duration};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use glyphst_translation_ui::DocumentTranslationManager;
use gpui::{AppContext as _, Entity, TestAppContext, VisualTestContext};
use gpui_component::Root;

use super::{
    LoadingDocumentStatus, LoadingDocumentView, TabItem, loading_grid_opacity,
    parse_debug_file_open_min_duration, remaining_file_open_min_duration,
};
use crate::{
    document_view::RenderCache,
    library::{DocumentPosition, LibraryStore},
    pdf_password_store::PdfPasswordStore,
    settings::SettingsStore,
};

pub(crate) fn temporary_pdf_path(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "glyphst-{label}-{}-{}.pdf",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

pub(crate) fn write_minimal_pdf(path: &std::path::Path) {
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

fn write_encrypted_pdf(path: &std::path::Path) {
    let encoded = include_str!("../../../pdf/tests/fixtures/dummy-encrypted.pdf.b64")
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>();
    fs::write(path, STANDARD.decode(encoded).unwrap()).unwrap();
}

fn init(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_component::init(cx);
        cx.set_global(SettingsStore::memory());
        cx.set_global(LibraryStore::memory());
        cx.set_global(RenderCache::new());
        cx.set_global(PdfPasswordStore::memory());
    });
}

/// A window showing a tab that opens `path`.
fn open_tab<'a>(
    path: &std::path::Path,
    cx: &'a mut TestAppContext,
) -> (Entity<TabItem>, &'a mut VisualTestContext) {
    let jobs = cx.update(|cx| cx.new(DocumentTranslationManager::new));
    let path = path.to_path_buf();
    let (root, cx) = cx.add_window_view(move |window, cx| {
        let item =
            TabItem::open_document(&path, None, None, None, jobs.clone(), window, cx).unwrap();
        Root::new(item, window, cx)
    });
    let item = root.read_with(cx, |root, _| {
        root.view().clone().downcast::<TabItem>().unwrap()
    });
    (item, cx)
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
        let password_input =
            cx.new(|cx| gpui_component::input::InputState::new(window, cx).masked(true));
        let loading = cx.new(|_| LoadingDocumentView {
            path: PathBuf::from("/tmp/example.pdf"),
            initial_position: DocumentPosition::fixed(0.0, 0.0, 1.0, 1),
            restore_tab_id: None,
            job_ids: Vec::new(),
            password_input,
            remember_password: true,
            status: LoadingDocumentStatus::Loading,
            pending_open: None,
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
        loading.status = LoadingDocumentStatus::PasswordRequired {
            error: Some("incorrect".into()),
            keychain_warning: Some("keychain unavailable".into()),
        };
        cx.notify();
    });
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    assert!(cx.debug_bounds("document-password-prompt").is_some());
    assert!(cx.debug_bounds("document-password-error").is_some());
    assert!(
        cx.debug_bounds("document-password-keychain-warning")
            .is_some()
    );

    loading.update(cx, |loading, cx| {
        loading.status = LoadingDocumentStatus::Error("failed".into());
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
fn an_unreadable_document_leaves_the_tab_showing_its_error(cx: &mut TestAppContext) {
    init(cx);
    let path = temporary_pdf_path("loading-tab");
    fs::create_dir(&path).unwrap();
    let (item, cx) = open_tab(&path, cx);
    item.read_with(cx, |item, _| assert!(item.is_loading()));

    cx.run_until_parked();
    item.read_with(cx, |item, cx| {
        let loading = item.loading_view().expect("the tab is still a loading tab");
        assert!(matches!(
            loading.read(cx).status,
            LoadingDocumentStatus::Error(_)
        ));
    });

    fs::remove_dir(path).unwrap();
}

#[gpui::test]
fn successful_preparation_turns_the_tab_into_a_document_in_place(cx: &mut TestAppContext) {
    init(cx);
    let path = temporary_pdf_path("loading-success");
    write_minimal_pdf(&path);
    let jobs = cx.update(|cx| cx.new(DocumentTranslationManager::new));
    let (_, cx) = cx.add_window_view(|window, cx| Root::new(cx.new(|_| gpui::Empty), window, cx));
    let item = cx.update(|window, cx| {
        let item = TabItem::open_document(&path, None, None, None, jobs, window, cx).unwrap();
        item.update(cx, |item, cx| {
            item.associate_open_job(42, cx);
            let loading = item.loading_view().unwrap();
            assert_eq!(loading.read(cx).job_ids, vec![42]);
        });
        item
    });
    let tab_id = item.entity_id();

    cx.run_until_parked();
    item.read_with(cx, |item, _| {
        assert!(item.is_document());
    });
    assert_eq!(item.entity_id(), tab_id, "the tab keeps its identity");

    fs::remove_file(path).unwrap();
}

#[gpui::test]
fn encrypted_document_retries_inline_and_reuses_a_saved_password(cx: &mut TestAppContext) {
    init(cx);
    let path = temporary_pdf_path("encrypted-open");
    write_encrypted_pdf(&path);
    let (item, cx) = open_tab(&path, cx);
    cx.run_until_parked();

    let loading = item.read_with(cx, |item, cx| {
        let loading = item
            .loading_view()
            .expect("encrypted document should wait for a password");
        assert!(matches!(
            loading.read(cx).status,
            LoadingDocumentStatus::PasswordRequired { error: None, .. }
        ));
        loading
    });
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    assert!(cx.debug_bounds("document-password-prompt").is_some());

    item.update_in(cx, |item, window, cx| {
        assert!(item.submit_document_password_for_test("wrong-password", true, window, cx));
    });
    cx.run_until_parked();
    loading.read_with(cx, |loading, _| {
        assert!(matches!(
            loading.status,
            LoadingDocumentStatus::PasswordRequired { error: Some(_), .. }
        ));
    });

    item.update_in(cx, |item, window, cx| {
        assert!(item.submit_document_password_for_test("123456", true, window, cx));
    });
    cx.run_until_parked();
    item.read_with(cx, |item, cx| {
        let document = item
            .document_view()
            .expect("correct password should open the document");
        assert!(document.read(cx).is_password_protected());
    });
    assert!(cx.read(|cx| cx.global::<PdfPasswordStore>().is_known_saved(&path)));

    // The saved password opens the document again without a prompt.
    let jobs = cx.update(|_, cx| cx.new(DocumentTranslationManager::new));
    let reopened = cx.update(|window, cx| {
        TabItem::open_document(&path, None, None, None, jobs.clone(), window, cx).unwrap()
    });
    cx.run_until_parked();
    reopened.read_with(cx, |item, _| assert!(item.is_document()));

    item.update_in(cx, |item, window, cx| {
        item.forget_saved_document_password(path.clone(), window, cx);
    });
    cx.run_until_parked();
    assert!(!cx.read(|cx| cx.global::<PdfPasswordStore>().is_known_saved(&path)));

    let prompted = cx.update(|window, cx| {
        TabItem::open_document(&path, None, None, None, jobs, window, cx).unwrap()
    });
    cx.run_until_parked();
    prompted.read_with(cx, |item, cx| {
        let loading = item
            .loading_view()
            .expect("a forgotten password should require another prompt");
        assert!(matches!(
            loading.read(cx).status,
            LoadingDocumentStatus::PasswordRequired { .. }
        ));
    });

    fs::remove_file(path).unwrap();
}
