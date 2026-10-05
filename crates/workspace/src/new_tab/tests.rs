use std::{
    cell::RefCell,
    collections::BTreeSet,
    fs::{self, OpenOptions},
    io::Write as _,
    path::PathBuf,
    rc::Rc,
    time::{SystemTime, UNIX_EPOCH},
};

use gpui::{AppContext as _, Focusable as _, TestAppContext};
use gpui_component::Root;
use image::Rgba;

use super::{
    BrowserEmptyAction, CursorMove, NewTab, NewTabEntry, NewTabEvent, NewTabItemKey,
    NewTabLayoutClass, ThumbnailCacheKey, catalog::scan_library_import_paths,
    rendering::format_file_size, thumbnails::persist_cover_thumbnail,
};
use crate::{
    library::{
        DocumentPosition, HistoryEntry, LibraryAddOutcome, LibraryItemMetadata, LibraryNodeId,
        LibrarySavedViewId, LibrarySort, LibrarySortDirection, LibrarySortField, LibraryStore,
        LibraryTagId, LibraryViewMode, QuickOpenSource, ReaderNewTabDestination,
        ReaderNewTabSession,
    },
    settings::{NewTabViewMode, SettingsStore},
};

fn temporary_directory(label: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "glyphst-{label}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

fn test_entry(path: &str) -> NewTabEntry {
    NewTabEntry {
        item_id: None,
        path: PathBuf::from(path),
        file_name: path.into(),
        directory: "/tmp".into(),
        last_read: "Last read".into(),
        progress: "50%".into(),
        format_size: "PDF · 1 KiB".into(),
        last_opened: "2026-07-28 12:00".into(),
        format: Some(glyphst_core::DocumentFormat::Pdf),
        favorite: false,
        status: None,
        missing: false,
        title_sort: path.to_lowercase().into(),
        creator_sort: "".into(),
        year: None,
        date_added: None,
        last_opened_timestamp: None,
        search_text: String::new().into(),
    }
}

fn history_entry(path: PathBuf, opened: u64) -> HistoryEntry {
    HistoryEntry {
        path,
        opened,
        position: DocumentPosition::fixed(0.0, 0.0, 1.0, 1),
    }
}

#[test]
fn responsive_layout_uses_orientation_and_hysteresis() {
    assert_eq!(
        NewTabLayoutClass::for_size(480.0, 700.0, NewTabLayoutClass::Compact),
        NewTabLayoutClass::Narrow
    );
    assert_eq!(
        NewTabLayoutClass::for_size(530.0, 700.0, NewTabLayoutClass::Narrow),
        NewTabLayoutClass::Narrow
    );
    assert_eq!(
        NewTabLayoutClass::for_size(600.0, 700.0, NewTabLayoutClass::Narrow),
        NewTabLayoutClass::Compact
    );
    assert_eq!(
        NewTabLayoutClass::for_size(900.0, 700.0, NewTabLayoutClass::Compact),
        NewTabLayoutClass::Expanded
    );
    assert_eq!(
        NewTabLayoutClass::for_size(900.0, 1_200.0, NewTabLayoutClass::Compact),
        NewTabLayoutClass::Compact
    );
    assert_eq!(
        NewTabLayoutClass::for_size(1_120.0, 1_400.0, NewTabLayoutClass::Compact),
        NewTabLayoutClass::Expanded
    );
    assert_eq!(
        NewTabLayoutClass::for_size(770.0, 700.0, NewTabLayoutClass::Expanded),
        NewTabLayoutClass::Expanded
    );
}

#[gpui::test]
fn new_tab_keeps_its_own_navigation_view_and_sort_state(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_component::init(cx);
        cx.set_global(SettingsStore::memory());
        cx.set_global(LibraryStore::memory());
    });
    let expected = ReaderNewTabSession {
        source: crate::library::QuickOpenSource::Library,
        history: crate::library::BrowserPresentation::history_default(),
        library: crate::library::BrowserPresentation {
            view_mode: LibraryViewMode::Grid,
            sort: LibrarySort {
                field: LibrarySortField::Title,
                direction: LibrarySortDirection::Ascending,
            },
        },
    };
    let restored = expected.clone();
    let (root, cx) = cx.add_window_view(move |window, cx| {
        let view = cx.new(|cx| NewTab::new_with_session(cx.focus_handle(), restored, window, cx));
        Root::new(view, window, cx)
    });
    let view = root.read_with(cx, |root, _| {
        root.view().clone().downcast::<NewTab>().unwrap()
    });
    assert_eq!(view.read_with(cx, |view, _| view.session_state()), expected);
}

#[gpui::test]
fn quick_open_focuses_the_first_visible_item_and_navigates_immediately(cx: &mut TestAppContext) {
    let directory = temporary_directory("quick-open-initial-focus");
    fs::create_dir(&directory).unwrap();
    let paths = ["a.pdf", "b.pdf", "c.pdf"]
        .into_iter()
        .map(|name| directory.join(name))
        .collect::<Vec<_>>();
    for path in &paths {
        fs::write(path, b"document").unwrap();
    }

    cx.update(|cx| {
        gpui_component::init(cx);
        crate::keybindings::capture_framework_bindings(cx);
        crate::keybindings::apply(cx, glyphst_settings::KeymapPreset::Modern, &[]);
        cx.set_global(SettingsStore::memory());
        let mut library = LibraryStore::memory();
        library
            .replace_history_entries(
                paths
                    .iter()
                    .enumerate()
                    .map(|(index, path)| history_entry(path.clone(), (10 - index) as u64))
                    .collect(),
            )
            .unwrap();
        cx.set_global(library);
    });

    let (root, cx) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| NewTab::new(cx.focus_handle(), window, cx));
        Root::new(view, window, cx)
    });
    let view = root.read_with(cx, |root, _| {
        root.view().clone().downcast::<NewTab>().unwrap()
    });
    let events = Rc::new(RefCell::new(Vec::new()));
    let recorded_events = events.clone();
    let _subscription = cx.update(|_, cx| {
        cx.subscribe(&view, move |_, event: &NewTabEvent, _| {
            recorded_events.borrow_mut().push(event.clone());
        })
    });
    cx.run_until_parked();
    view.update_in(cx, |this, window, cx| {
        this.focus_handle.focus(window, cx);
    });
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });

    view.update_in(cx, |this, window, cx| {
        assert_eq!(this.focused_item, NewTabItemKey::Document(paths[0].clone()));
        assert!(this.selection.is_empty());
        assert!(this.item_state(0).focused);
        this.activate_focused(cx);

        for mode in [
            NewTabViewMode::Grid,
            NewTabViewMode::Grouped,
            NewTabViewMode::Table,
        ] {
            this.set_view_mode(mode, cx);
            this.set_focused_index(0);
            this.move_linear_cursor(true, cx);
            assert_eq!(this.focused_item, NewTabItemKey::Document(paths[1].clone()));
            this.move_linear_cursor(false, cx);
            assert_eq!(this.focused_item, NewTabItemKey::Document(paths[0].clone()));
        }

        this.search_input.update(cx, |input, cx| {
            // Search includes parent paths, whose temporary directory name
            // can also contain the single letter "b".
            input.set_value("b.pdf", window, cx);
        });
        this.update_filter(cx);
        assert_eq!(this.focused_item, NewTabItemKey::Document(paths[1].clone()));
        this.search_input.update(cx, |input, cx| {
            input.set_value("", window, cx);
        });
        this.update_filter(cx);
        assert_eq!(this.focused_item, NewTabItemKey::Document(paths[1].clone()));
        this.set_destination(ReaderNewTabDestination::Library, window, cx);
        assert_eq!(this.focused_item, NewTabItemKey::OpenFile);
        this.set_destination(ReaderNewTabDestination::History, window, cx);
        assert_eq!(this.focused_item, NewTabItemKey::Document(paths[0].clone()));
    });
    assert_eq!(
        events.borrow().as_slice(),
        &[NewTabEvent::OpenDocument(paths[0].clone())]
    );

    cx.simulate_keystrokes("ctrl-n");
    assert_eq!(
        view.read_with(cx, |this, _| this.focused_item.clone()),
        NewTabItemKey::Document(paths[1].clone())
    );
    cx.simulate_keystrokes("ctrl-p");
    assert_eq!(
        view.read_with(cx, |this, _| this.focused_item.clone()),
        NewTabItemKey::Document(paths[0].clone())
    );

    view.update_in(cx, |this, _, cx| {
        this.handle_document_pointer(paths[1].clone(), gpui::Modifiers::default(), 1, cx);
    });
    assert_eq!(
        events.borrow().as_slice(),
        &[
            NewTabEvent::OpenDocument(paths[0].clone()),
            NewTabEvent::OpenDocument(paths[1].clone()),
        ]
    );

    fs::remove_dir_all(directory).unwrap();
}

#[gpui::test]
fn home_single_click_keeps_selection_and_double_click_opening(cx: &mut TestAppContext) {
    let directory = temporary_directory("home-click-behavior");
    fs::create_dir(&directory).unwrap();
    let path = directory.join("paper.pdf");
    fs::write(&path, b"document").unwrap();
    cx.update(|cx| {
        gpui_component::init(cx);
        cx.set_global(SettingsStore::memory());
        let mut library = LibraryStore::memory();
        library
            .replace_history_entries(vec![history_entry(path.clone(), 1)])
            .unwrap();
        cx.set_global(library);
    });
    let (root, cx) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| NewTab::new_home(cx.focus_handle(), window, cx));
        Root::new(view, window, cx)
    });
    let view = root.read_with(cx, |root, _| {
        root.view().clone().downcast::<NewTab>().unwrap()
    });
    let events = Rc::new(RefCell::new(Vec::new()));
    let recorded_events = events.clone();
    let _subscription = cx.update(|_, cx| {
        cx.subscribe(&view, move |_, event: &NewTabEvent, _| {
            recorded_events.borrow_mut().push(event.clone());
        })
    });
    cx.run_until_parked();

    view.update_in(cx, |this, window, cx| {
        this.set_destination(ReaderNewTabDestination::History, window, cx);
        this.handle_document_pointer(path.clone(), gpui::Modifiers::default(), 1, cx);
        assert_eq!(this.focused_item, NewTabItemKey::Document(path.clone()));
        assert_eq!(this.selection, BTreeSet::from([path.clone()]));
        assert!(this.inspector_open);
    });
    assert!(events.borrow().is_empty());

    view.update_in(cx, |this, _, cx| {
        this.handle_document_pointer(path.clone(), gpui::Modifiers::default(), 2, cx);
    });
    assert_eq!(
        events.borrow().as_slice(),
        &[NewTabEvent::OpenDocument(path.clone())]
    );

    fs::remove_dir_all(directory).unwrap();
}

#[gpui::test]
fn quick_open_library_tree_focuses_first_row_and_opens_items_on_one_click(cx: &mut TestAppContext) {
    let directory = temporary_directory("quick-open-library-tree");
    fs::create_dir(&directory).unwrap();
    let path = directory.join("paper.pdf");
    fs::write(&path, b"document").unwrap();

    let (collection_id, item_id) = cx.update(|cx| {
        gpui_component::init(cx);
        let mut settings_store = SettingsStore::memory();
        let mut settings = settings_store.settings.clone();
        settings.new_tab.view_mode = NewTabViewMode::Table;
        settings_store.set_settings_in_memory(settings);
        cx.set_global(settings_store);
        let mut library = LibraryStore::memory();
        let collection_id = library
            .create_library_collection(None, "Reading List")
            .unwrap();
        let outcome = library
            .add_linked_item(&path, LibraryItemMetadata::for_path(&path))
            .unwrap();
        let item_id = match outcome {
            LibraryAddOutcome::Added(id)
            | LibraryAddOutcome::Existing(id)
            | LibraryAddOutcome::ExistingInTrash(id) => id,
        };
        cx.set_global(library);
        (collection_id, item_id)
    });
    let session = ReaderNewTabSession {
        source: QuickOpenSource::Library,
        ..ReaderNewTabSession::default()
    };
    let (root, cx) = cx.add_window_view(move |window, cx| {
        let view = cx.new(|cx| NewTab::new_with_session(cx.focus_handle(), session, window, cx));
        Root::new(view, window, cx)
    });
    let view = root.read_with(cx, |root, _| {
        root.view().clone().downcast::<NewTab>().unwrap()
    });
    let events = Rc::new(RefCell::new(Vec::new()));
    let recorded_events = events.clone();
    let _subscription = cx.update(|_, cx| {
        cx.subscribe(&view, move |_, event: &NewTabEvent, _| {
            recorded_events.borrow_mut().push(event.clone());
        })
    });
    cx.run_until_parked();

    view.update_in(cx, |this, _, cx| {
        assert!(this.is_library_tree());
        assert_eq!(
            this.tree.active,
            Some(LibraryNodeId::Collection(collection_id))
        );
        assert_eq!(this.focused_item, NewTabItemKey::Collection(collection_id));
        this.move_linear_cursor(true, cx);
        assert_eq!(this.tree.active, Some(LibraryNodeId::Item(item_id)));
        assert_eq!(this.focused_item, NewTabItemKey::Document(path.clone()));
        this.move_linear_cursor(false, cx);
        assert_eq!(
            this.tree.active,
            Some(LibraryNodeId::Collection(collection_id))
        );
        this.handle_tree_pointer(
            LibraryNodeId::Item(item_id),
            gpui::Modifiers::default(),
            1,
            cx,
        );
    });
    assert_eq!(
        events.borrow().as_slice(),
        &[NewTabEvent::OpenDocument(path.clone())]
    );

    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn selection_ranges_follow_the_visible_document_order() {
    let entries = vec![
        test_entry("/tmp/a.pdf"),
        test_entry("/tmp/b.pdf"),
        test_entry("/tmp/c.pdf"),
    ];
    let filtered = vec![0, 1, 2];

    assert_eq!(
        NewTab::selection_range(
            &entries,
            &filtered,
            PathBuf::from("/tmp/b.pdf").as_path(),
            0,
        ),
        BTreeSet::from([PathBuf::from("/tmp/a.pdf"), PathBuf::from("/tmp/b.pdf"),])
    );
    assert_eq!(
        NewTab::selection_range(
            &entries,
            &filtered,
            PathBuf::from("/tmp/a.pdf").as_path(),
            2,
        ),
        BTreeSet::from([
            PathBuf::from("/tmp/a.pdf"),
            PathBuf::from("/tmp/b.pdf"),
            PathBuf::from("/tmp/c.pdf"),
        ])
    );
    assert!(
        NewTab::selection_range(&entries, &[], PathBuf::from("/tmp/a.pdf").as_path(), 0,)
            .is_empty()
    );
}

#[test]
fn file_sizes_use_binary_units() {
    assert_eq!(format_file_size(42), "42 B");
    assert_eq!(format_file_size(1536), "1.50 KiB");
    assert_eq!(format_file_size(2 * 1024 * 1024), "2.00 MiB");
}

#[test]
fn grouped_labels_cover_timeline_and_alphabetic_fallbacks() {
    let now = u64::try_from(chrono::Utc::now().timestamp()).unwrap();
    assert_eq!(NewTab::grouped_time_label(Some(now), "Unknown"), "Today");
    assert_eq!(
        NewTab::grouped_time_label(None, "Never Opened"),
        "Never Opened"
    );
    assert_eq!(NewTab::grouped_alpha_label("Ada Lovelace", "Unknown"), "A");
    assert_eq!(NewTab::grouped_alpha_label("123 notes", "Unknown"), "#");
    assert_eq!(NewTab::grouped_alpha_label("", "Unknown"), "Unknown");
}

#[test]
fn search_copy_is_specific_to_each_browser_destination() {
    assert_eq!(
        NewTab::search_placeholder(&ReaderNewTabDestination::Home),
        "Search documents…"
    );
    assert_eq!(
        NewTab::search_placeholder(&ReaderNewTabDestination::History),
        "Search recent documents…"
    );
    assert_eq!(
        NewTab::search_placeholder(&ReaderNewTabDestination::Library),
        "Search your library…"
    );
    assert_eq!(
        NewTab::search_placeholder(&ReaderNewTabDestination::Favorites),
        "Search favorites…"
    );
    assert_eq!(
        NewTab::search_placeholder(&ReaderNewTabDestination::Trash),
        "Search trash…"
    );
    assert_eq!(
        NewTab::search_placeholder(&ReaderNewTabDestination::Tags),
        "Search tags…"
    );
    assert_eq!(
        NewTab::search_placeholder(&ReaderNewTabDestination::Tag(LibraryTagId(1))),
        "Search tagged items…"
    );
    assert_eq!(
        NewTab::search_placeholder(&ReaderNewTabDestination::SavedView(LibrarySavedViewId(1))),
        "Search this view…"
    );
}

#[gpui::test]
fn empty_home_library_has_a_full_import_state_in_every_view(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_component::init(cx);
        cx.set_global(SettingsStore::memory());
        cx.set_global(LibraryStore::memory());
    });

    let (root, cx) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| NewTab::new_home(cx.focus_handle(), window, cx));
        Root::new(view, window, cx)
    });
    let view = root.read_with(cx, |root, _| {
        root.view().clone().downcast::<NewTab>().unwrap()
    });
    assert_eq!(
        view.read_with(cx, |this, _| this.empty_state(false).title),
        "Your library is empty"
    );

    for mode in [
        NewTabViewMode::Grid,
        NewTabViewMode::Grouped,
        NewTabViewMode::Table,
    ] {
        view.update_in(cx, |this, _, cx| this.set_view_mode(mode, cx));
        cx.update(|window, cx| {
            _ = window.draw(cx);
            _ = window.draw(cx);
        });
        let empty = cx
            .debug_bounds("new-tab-empty-state")
            .expect("empty Library state");
        assert!(empty.size.height >= gpui::px(180.0));
        assert!(cx.debug_bounds("library-empty-add-files").is_some());
        assert!(cx.debug_bounds("new-tab-empty-open-file").is_none());
    }
}

#[gpui::test]
fn empty_library_actions_respect_host_and_current_folder(cx: &mut TestAppContext) {
    let collection_id = cx.update(|cx| {
        gpui_component::init(cx);
        cx.set_global(SettingsStore::memory());
        let mut library = LibraryStore::memory();
        let collection_id = library.create_library_collection(None, "Papers").unwrap();
        cx.set_global(library);
        collection_id
    });

    let (home_root, home_cx) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| NewTab::new_home(cx.focus_handle(), window, cx));
        Root::new(view, window, cx)
    });
    let home = home_root.read_with(home_cx, |root, _| {
        root.view().clone().downcast::<NewTab>().unwrap()
    });
    home.update_in(home_cx, |this, _, cx| {
        this.set_view_mode(NewTabViewMode::Grid, cx);
        this.enter_collection(collection_id, cx);
        assert!(matches!(
            this.empty_state(false).action,
            BrowserEmptyAction::AddFiles(Some(id)) if id == collection_id
        ));
    });

    let session = ReaderNewTabSession {
        source: crate::library::QuickOpenSource::Library,
        ..ReaderNewTabSession::default()
    };
    let (quick_root, quick_cx) = cx.add_window_view(move |window, cx| {
        let view = cx.new(|cx| NewTab::new_with_session(cx.focus_handle(), session, window, cx));
        Root::new(view, window, cx)
    });
    let quick = quick_root.read_with(quick_cx, |root, _| {
        root.view().clone().downcast::<NewTab>().unwrap()
    });
    quick_cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    assert!(matches!(
        quick.read_with(quick_cx, |this, _| this.empty_state(false).action),
        BrowserEmptyAction::OpenFile
    ));
    assert!(quick_cx.debug_bounds("new-tab-empty-open-file").is_some());
    assert!(quick_cx.debug_bounds("library-empty-add-files").is_none());
}

#[gpui::test]
fn tag_search_filters_names_and_uses_its_own_empty_state(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_component::init(cx);
        cx.set_global(SettingsStore::memory());
        let mut library = LibraryStore::memory();
        library.ensure_library_tag("Methods").unwrap();
        library.ensure_library_tag("Theory").unwrap();
        cx.set_global(library);
    });

    let (root, cx) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| NewTab::new_home(cx.focus_handle(), window, cx));
        Root::new(view, window, cx)
    });
    let view = root.read_with(cx, |root, _| {
        root.view().clone().downcast::<NewTab>().unwrap()
    });
    view.update_in(cx, |this, window, cx| {
        this.set_destination(ReaderNewTabDestination::Tags, window, cx);
        this.search_input.update(cx, |input, cx| {
            input.set_value("method", window, cx);
        });
        assert_eq!(this.tag_result_counts(cx), (1, 2));
        this.search_input.update(cx, |input, cx| {
            input.set_value("missing", window, cx);
        });
    });
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    assert!(cx.debug_bounds("new-tab-empty-state").is_some());
    assert!(cx.debug_bounds("library-empty-add-files").is_none());
}

#[gpui::test]
fn open_file_and_search_stay_in_the_header_across_view_modes(cx: &mut TestAppContext) {
    let directory = temporary_directory("new-tab-view-modes");
    fs::create_dir(&directory).unwrap();
    cx.update(|cx| {
        gpui_component::init(cx);
        let mut settings = SettingsStore::memory();
        settings.config_path = directory.join("settings.kdl");
        cx.set_global(settings);
        cx.set_global(LibraryStore::memory());
    });

    let (_root, cx) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| NewTab::new(cx.focus_handle(), window, cx));
        Root::new(view, window, cx)
    });
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });

    assert!(cx.debug_bounds("new-tab-open-file").is_some());
    assert!(cx.debug_bounds("new-tab-search").is_some());
    assert!(cx.debug_bounds("new-tab-view-grid").is_some());
    assert!(cx.debug_bounds("new-tab-view-grouped").is_some());
    assert!(cx.debug_bounds("new-tab-view-tree").is_some());
    let grid = cx.debug_bounds("new-tab-view-grid").unwrap();
    let tree = cx.debug_bounds("new-tab-view-tree").unwrap();
    let grouped = cx.debug_bounds("new-tab-view-grouped").unwrap();
    assert!(grid.right() <= tree.left());
    assert!(tree.right() <= grouped.left());

    cx.simulate_click(grouped.center(), gpui::Modifiers::default());
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    assert!(cx.debug_bounds("new-tab-open-file").is_some());

    let tree = cx.debug_bounds("new-tab-view-tree").unwrap().center();
    cx.simulate_click(tree, gpui::Modifiers::default());
    cx.update(|window, cx| {
        _ = window.draw(cx);
        assert_eq!(
            cx.global::<SettingsStore>().settings.new_tab.view_mode,
            NewTabViewMode::Table
        );
    });
    assert!(cx.debug_bounds("new-tab-open-file").is_some());

    fs::remove_dir_all(directory).unwrap();
}

#[gpui::test]
fn quick_open_hides_management_while_home_exposes_it(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_component::init(cx);
        cx.set_global(SettingsStore::memory());
        cx.set_global(LibraryStore::memory());
    });

    let (_quick_root, quick_cx) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| NewTab::new(cx.focus_handle(), window, cx));
        Root::new(view, window, cx)
    });
    quick_cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    assert!(quick_cx.debug_bounds("new-tab-source-selector").is_some());
    assert!(quick_cx.debug_bounds("home-navigation-rail").is_none());
    assert!(quick_cx.debug_bounds("new-tab-add-files").is_none());
    assert!(quick_cx.debug_bounds("home-library-filter-menu").is_none());

    let (_home_root, home_cx) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| NewTab::new_home(cx.focus_handle(), window, cx));
        Root::new(view, window, cx)
    });
    home_cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    assert!(home_cx.debug_bounds("home-navigation-rail").is_some());
    assert!(home_cx.debug_bounds("new-tab-add-files").is_some());
    assert!(home_cx.debug_bounds("home-library-new-folder").is_some());
    assert!(home_cx.debug_bounds("home-library-filter-menu").is_none());
    assert!(home_cx.debug_bounds("home-library-kind-filter").is_none());
    assert!(home_cx.debug_bounds("home-library-status-filter").is_none());
    assert!(home_cx.debug_bounds("new-tab-sort-field").is_none());
    assert!(home_cx.debug_bounds("new-tab-sort-direction").is_none());
    assert!(home_cx.debug_bounds("new-tab-source-selector").is_none());
}

#[gpui::test]
fn quick_source_menu_paints_and_receives_clicks_above_search(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_component::init(cx);
        cx.set_global(SettingsStore::memory());
        cx.set_global(LibraryStore::memory());
    });

    let (root, cx) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| NewTab::new(cx.focus_handle(), window, cx));
        Root::new(view, window, cx)
    });
    let view = root.read_with(cx, |root, _| {
        root.view().clone().downcast::<NewTab>().unwrap()
    });
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });

    let selector = cx.debug_bounds("new-tab-source-selector").unwrap();
    cx.simulate_click(selector.center(), gpui::Modifiers::default());
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });

    assert!(cx.debug_bounds("new-tab-source-menu").is_some());
    let history = cx.debug_bounds("new-tab-nav-History").unwrap();
    let search = cx.debug_bounds("new-tab-search").unwrap();
    assert!(history.intersects(&search));
    cx.simulate_click(
        history.intersect(&search).center(),
        gpui::Modifiers::default(),
    );
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });

    assert!(!view.read_with(cx, |this, _| this.nav_menu_open));
    assert!(!cx.update(|window, cx| {
        view.read(cx)
            .search_input
            .focus_handle(cx)
            .is_focused(window)
    }));
}

#[gpui::test]
fn home_grid_cards_stay_inside_the_browser_with_an_inspector(cx: &mut TestAppContext) {
    let directory = temporary_directory("home-grid-browser-width");
    fs::create_dir(&directory).unwrap();
    let paths = (0..4)
        .map(|ix| directory.join(format!("paper-{ix}.pdf")))
        .collect::<Vec<_>>();
    for path in &paths {
        fs::write(path, b"document").unwrap();
    }

    cx.update(|cx| {
        gpui_component::init(cx);
        cx.set_global(SettingsStore::memory());
        let mut library = LibraryStore::memory();
        library
            .replace_history_entries(
                paths
                    .iter()
                    .enumerate()
                    .map(|(ix, path)| history_entry(path.clone(), ix as u64 + 1))
                    .collect(),
            )
            .unwrap();
        cx.set_global(library);
    });
    let (root, cx) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| NewTab::new_home(cx.focus_handle(), window, cx));
        Root::new(view, window, cx)
    });
    let view = root.read_with(cx, |root, _| {
        root.view().clone().downcast::<NewTab>().unwrap()
    });
    cx.simulate_resize(gpui::size(gpui::px(1_200.0), gpui::px(800.0)));
    view.update_in(cx, |this, window, cx| {
        this.set_destination(ReaderNewTabDestination::History, window, cx);
        this.set_view_mode(NewTabViewMode::Grid, cx);
    });
    for _ in 0..3 {
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
    }

    let browser = cx
        .debug_bounds("new-tab-browser-viewport")
        .expect("browser viewport");
    let (window_width, browser_width, cols) = view.read_with(cx, |this, _| {
        (this.viewport_width, this.browser_viewport_width, this.cols)
    });
    assert!(browser_width < window_width);
    assert_eq!(cols, NewTab::cols_for_width(browser_width));
    assert_eq!(cols, 4);
    for selector in [
        "new-tab-card-0",
        "new-tab-card-1",
        "new-tab-card-2",
        "new-tab-card-3",
    ] {
        let card = cx.debug_bounds(selector).expect("visible grid card");
        assert!(card.left() >= browser.left());
        assert!(card.right() <= browser.right());
    }

    let first_card = cx.debug_bounds("new-tab-card-0").unwrap();
    cx.simulate_click(first_card.center(), gpui::Modifiers::default());
    for _ in 0..3 {
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
    }
    assert!(cx.debug_bounds("new-tab-item-inspector").is_some());
    let narrowed_browser = cx
        .debug_bounds("new-tab-browser-viewport")
        .expect("narrowed browser viewport");
    let narrowed_cols = view.read_with(cx, |this, _| this.cols);
    assert!(narrowed_browser.size.width < browser.size.width);
    assert_eq!(narrowed_cols, 3);
    for selector in ["new-tab-card-0", "new-tab-card-1", "new-tab-card-2"] {
        let card = cx
            .debug_bounds(selector)
            .expect("visible narrowed grid card");
        assert!(card.left() >= narrowed_browser.left());
        assert!(card.right() <= narrowed_browser.right());
    }

    fs::remove_dir_all(directory).unwrap();
}

#[gpui::test]
fn folder_editor_input_receives_vim_navigation_keys(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_component::init(cx);
        crate::keybindings::capture_framework_bindings(cx);
        crate::keybindings::apply(cx, glyphst_settings::KeymapPreset::Vim, &[]);
        cx.set_global(SettingsStore::memory());
        cx.set_global(LibraryStore::memory());
    });

    let (root, cx) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| NewTab::new_home(cx.focus_handle(), window, cx));
        Root::new(view, window, cx)
    });
    let view = root.read_with(cx, |root, _| {
        root.view().clone().downcast::<NewTab>().unwrap()
    });
    view.update_in(cx, |this, window, cx| {
        this.open_new_folder_editor(None, window, cx);
    });

    cx.run_until_parked();
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    assert!(cx.debug_bounds("library-folder-editor-overlay").is_some());

    cx.simulate_keystrokes("k");
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    assert_eq!(
        view.read_with(cx, |this, cx| this
            .editors
            .folder_name_input
            .read(cx)
            .value()
            .to_string()),
        "k"
    );

    cx.simulate_keystrokes("escape");
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    assert!(cx.debug_bounds("library-folder-editor-overlay").is_none());
    assert!(cx.update(|_, cx| cx.global::<LibraryStore>().library_collections().is_empty()));
    assert!(cx.update(|window, cx| view.read(cx).focus_handle.is_focused(window)));

    view.update_in(cx, |this, window, cx| {
        this.open_saved_view_editor(window, cx);
    });
    cx.run_until_parked();
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    assert!(cx.debug_bounds("new-tab-saved-view-overlay").is_some());
    cx.simulate_keystrokes("escape");
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    assert!(cx.debug_bounds("new-tab-saved-view-overlay").is_none());
}

#[gpui::test]
fn library_folders_browse_as_cards_and_expand_as_a_tree(cx: &mut TestAppContext) {
    let (papers_id, theory_id) = cx.update(|cx| {
        gpui_component::init(cx);
        cx.set_global(SettingsStore::memory());
        let mut library = LibraryStore::memory();
        let papers_id = library.create_library_collection(None, "Papers").unwrap();
        let theory_id = library
            .create_library_collection(Some(papers_id), "Type Theory")
            .unwrap();
        cx.set_global(library);
        (papers_id, theory_id)
    });

    let (root, cx) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| NewTab::new_home(cx.focus_handle(), window, cx));
        Root::new(view, window, cx)
    });
    let view = root.read_with(cx, |root, _| {
        root.view().clone().downcast::<NewTab>().unwrap()
    });
    view.update_in(cx, |this, _, cx| {
        this.set_view_mode(NewTabViewMode::Grid, cx);
    });
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });

    assert!(cx.debug_bounds("library-folder-grid").is_some());
    assert!(cx.debug_bounds("library-folder-item").is_some());
    assert!(cx.debug_bounds("library-folder-breadcrumb").is_some());

    let papers = cx.debug_bounds("library-folder-item").unwrap().center();
    cx.simulate_mouse_down(papers, gpui::MouseButton::Right, gpui::Modifiers::default());
    cx.run_until_parked();
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    assert_eq!(
        view.read_with(cx, |this, _| this.tree.current_collection),
        None
    );
    cx.simulate_keystrokes("down enter");
    cx.run_until_parked();
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    assert!(cx.debug_bounds("library-folder-editor-overlay").is_some());
    view.update_in(cx, |this, _, cx| {
        this.editors.folder = None;
        cx.notify();
    });
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });

    cx.simulate_click(papers, gpui::Modifiers::default());
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    assert_eq!(
        view.read_with(cx, |this, _| this.tree.current_collection),
        Some(papers_id)
    );
    assert!(cx.debug_bounds("library-folder-item").is_some());

    view.update_in(cx, |this, window, cx| {
        this.search_input.update(cx, |input, cx| {
            input.set_value("type", window, cx);
        });
        this.update_filter(cx);
    });
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    assert!(cx.debug_bounds("library-folder-item").is_none());
    assert_eq!(
        view.read_with(cx, |this, _| this.tree.current_collection),
        Some(papers_id)
    );

    view.update_in(cx, |this, window, cx| {
        this.search_input.update(cx, |input, cx| {
            input.set_value("", window, cx);
        });
        this.update_filter(cx);
        this.set_view_mode(NewTabViewMode::Table, cx);
        this.toggle_collection_expanded(papers_id, cx);
    });
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });

    let papers_row_selector: &'static str =
        Box::leak(format!("library-folder-tree-row-{}", papers_id.0).into_boxed_str());
    let theory_row_selector: &'static str =
        Box::leak(format!("library-folder-tree-row-{}", theory_id.0).into_boxed_str());
    assert!(cx.debug_bounds(papers_row_selector).is_some());
    let tree_viewport = cx
        .debug_bounds("new-tab-tree-viewport")
        .expect("library tree viewport");
    let papers_row = cx
        .debug_bounds(papers_row_selector)
        .expect("top-level library folder row");
    let theory_row = cx
        .debug_bounds(theory_row_selector)
        .expect("nested library folder row");
    let root_area = cx
        .debug_bounds("library-tree-root-area")
        .expect("library tree root area");
    assert!(tree_viewport.size.height > gpui::px(0.0));
    assert!(papers_row.top() >= tree_viewport.top());
    assert!(theory_row.bottom() <= tree_viewport.bottom());
    assert!(root_area.size.height > gpui::px(0.0));
    assert!(root_area.top() >= theory_row.bottom());
    assert!(root_area.bottom() <= tree_viewport.bottom());
    assert!(view.read_with(cx, |this, _| {
        this.tree.rows.iter().any(|row| {
            matches!(
                row,
                super::LibraryTreeRow::Collection { id, depth: 0 } if *id == papers_id
            )
        }) && this.tree.rows.iter().any(|row| {
            matches!(
                row,
                super::LibraryTreeRow::Collection { id, depth: 1 } if *id == theory_id
            )
        })
    }));

    cx.simulate_mouse_down(
        theory_row.center(),
        gpui::MouseButton::Left,
        gpui::Modifiers::default(),
    );
    cx.simulate_mouse_move(
        gpui::point(theory_row.center().x + gpui::px(8.0), theory_row.center().y),
        Some(gpui::MouseButton::Left),
        gpui::Modifiers::default(),
    );
    cx.update(|window, cx| {
        _ = window.draw(cx);
        _ = window.draw(cx);
    });
    assert!(view.read_with(cx, |this, _| {
        this.tree
            .dragging_nodes
            .contains(&crate::library::LibraryNodeId::Collection(theory_id))
    }));
    cx.simulate_mouse_move(
        root_area.center(),
        Some(gpui::MouseButton::Left),
        gpui::Modifiers::default(),
    );
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    cx.simulate_mouse_up(
        root_area.center(),
        gpui::MouseButton::Left,
        gpui::Modifiers::default(),
    );
    cx.update(|window, cx| {
        _ = window.draw(cx);
        _ = window.draw(cx);
    });
    assert_eq!(
        cx.update(|_, cx| cx
            .global::<LibraryStore>()
            .library_collection(theory_id)
            .and_then(|collection| collection.parent_id)),
        None
    );

    cx.simulate_mouse_down(
        root_area.center(),
        gpui::MouseButton::Right,
        gpui::Modifiers::default(),
    );
    cx.run_until_parked();
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    cx.simulate_keystrokes("down enter");
    cx.run_until_parked();
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    assert!(cx.debug_bounds("library-folder-editor-overlay").is_some());
}

#[test]
fn recursive_import_scan_preserves_directories_and_skips_unsupported_files() {
    let directory = temporary_directory("library-import-scan");
    let dropped = directory.join("Reading List");
    let nested = dropped.join("Papers");
    let empty = dropped.join("Empty");
    fs::create_dir_all(&nested).unwrap();
    fs::create_dir_all(&empty).unwrap();
    fs::write(dropped.join("book.epub"), b"book").unwrap();
    fs::write(nested.join("paper.pdf"), b"paper").unwrap();
    fs::write(nested.join("notes.txt"), b"notes").unwrap();

    let scan = scan_library_import_paths(vec![dropped]);
    assert_eq!(scan.candidates.len(), 2);
    assert_eq!(scan.skipped, 1);
    assert_eq!(scan.failed, 0);
    assert!(scan.folders.contains(&vec!["Reading List".to_string()]));
    assert!(
        scan.folders
            .contains(&vec!["Reading List".to_string(), "Empty".to_string(),])
    );
    assert!(
        scan.folders
            .contains(&vec!["Reading List".to_string(), "Papers".to_string(),])
    );
    assert!(scan.candidates.iter().any(|candidate| {
        candidate.path.ends_with("paper.pdf") && candidate.folder_path == ["Reading List", "Papers"]
    }));

    fs::remove_dir_all(directory).unwrap();
}

#[gpui::test]
fn history_supports_grouped_spans_and_headerless_tree_rows(cx: &mut TestAppContext) {
    let directory = temporary_directory("new-tab-rounded-rows");
    fs::create_dir(&directory).unwrap();
    let path = directory.join(
            "a-very-long-academic-paper-title-that-remains-visible-until-the-span-reaches-its-parent.pdf",
        );
    fs::write(&path, b"document").unwrap();

    cx.update(|cx| {
        gpui_component::init(cx);
        let mut settings_store = SettingsStore::memory();
        settings_store.config_path = directory.join("settings.kdl");
        let mut settings = settings_store.settings.clone();
        settings.new_tab.view_mode = NewTabViewMode::Grouped;
        settings_store.set_settings_in_memory(settings);
        cx.set_global(settings_store);

        let mut library = LibraryStore::memory();
        library
            .replace_history_entries(vec![history_entry(path.clone(), 1)])
            .unwrap();
        cx.set_global(library);
    });

    let (root, cx) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| NewTab::new(cx.focus_handle(), window, cx));
        Root::new(view, window, cx)
    });
    let view = root.read_with(cx, |root, _| {
        root.view().clone().downcast::<NewTab>().unwrap()
    });

    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    assert!(cx.debug_bounds("new-tab-grouped-list").is_some());
    let grouped_item = cx
        .debug_bounds("new-tab-grouped-item")
        .expect("grouped item");
    assert!(grouped_item.size.width > gpui::px(320.0));

    view.update_in(cx, |this, _, cx| {
        this.set_view_mode(NewTabViewMode::Table, cx);
    });
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
    let row = cx
        .debug_bounds("new-tab-tree-document-row")
        .expect("tree document row");
    let viewport = cx
        .debug_bounds("new-tab-tree-viewport")
        .expect("history tree viewport");
    let name = cx
        .debug_bounds("new-tab-tree-document-name")
        .expect("tree document name");
    assert!(cx.debug_bounds("new-tab-details-name-header").is_none());
    assert!(viewport.size.height > gpui::px(0.0));
    assert!(row.top() >= viewport.top());
    assert!(row.bottom() <= viewport.bottom());
    assert!(name.left() > row.left());
    assert!(name.right() <= row.right());

    fs::remove_dir_all(directory).unwrap();
}

#[gpui::test]
fn keyboard_selection_supports_ranges_and_select_all(cx: &mut TestAppContext) {
    let directory = temporary_directory("new-tab-selection");
    fs::create_dir(&directory).unwrap();
    let paths = ["a.pdf", "b.pdf", "c.pdf"]
        .into_iter()
        .map(|name| directory.join(name))
        .collect::<Vec<_>>();
    for path in &paths {
        fs::write(path, b"document").unwrap();
    }

    cx.update(|cx| {
        gpui_component::init(cx);
        let mut settings_store = SettingsStore::memory();
        let mut settings = settings_store.settings.clone();
        settings.new_tab.view_mode = NewTabViewMode::Grouped;
        settings_store.set_settings_in_memory(settings);
        cx.set_global(settings_store);

        let mut library = LibraryStore::memory();
        library
            .replace_history_entries(
                paths
                    .iter()
                    .enumerate()
                    .map(|(ix, path)| history_entry(path.clone(), (10 - ix) as u64))
                    .collect(),
            )
            .unwrap();
        cx.set_global(library);
    });

    let (root, cx) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| NewTab::new(cx.focus_handle(), window, cx));
        Root::new(view, window, cx)
    });
    let view = root.read_with(cx, |root, _| {
        root.view().clone().downcast::<NewTab>().unwrap()
    });

    view.update_in(cx, |this, _, cx| {
        assert_eq!(
            this.item_key(0),
            Some(NewTabItemKey::Document(paths[0].clone()))
        );

        this.handle_document_pointer(paths[0].clone(), gpui::Modifiers::secondary_key(), 1, cx);
        assert_eq!(this.selection, BTreeSet::from([paths[0].clone()]));
        this.handle_document_pointer(paths[2].clone(), gpui::Modifiers::secondary_key(), 1, cx);
        assert_eq!(
            this.selection,
            BTreeSet::from([paths[0].clone(), paths[2].clone()])
        );
        this.handle_document_pointer(
            paths[1].clone(),
            gpui::Modifiers {
                shift: true,
                ..Default::default()
            },
            1,
            cx,
        );
        assert_eq!(
            this.selection,
            BTreeSet::from([paths[1].clone(), paths[2].clone()])
        );
        this.handle_document_pointer(
            paths[0].clone(),
            gpui::Modifiers {
                shift: true,
                ..gpui::Modifiers::secondary_key()
            },
            1,
            cx,
        );
        assert_eq!(this.selection.len(), 3);
        this.handle_document_pointer(paths[1].clone(), gpui::Modifiers::default(), 1, cx);
        assert_eq!(this.selection, BTreeSet::from([paths[1].clone()]));
        assert!(this.selecting);
        this.cancel_select(cx);
        this.focused_item = NewTabItemKey::Document(paths[0].clone());
        this.toggle_focused(cx);
        assert_eq!(this.selection.len(), 1);

        this.move_cursor(CursorMove::Down, true, cx);
        assert_eq!(
            this.selection,
            BTreeSet::from([paths[0].clone(), paths[1].clone()])
        );

        this.select_all(cx);
        assert_eq!(this.selection.len(), 3);
        this.cancel_select(cx);
        assert!(this.selection.is_empty());
        assert!(!this.selecting);
        assert_eq!(cx.global::<LibraryStore>().history_entries().len(), 3);
    });

    for path in &paths {
        assert!(path.is_file(), "removing history must not delete {path:?}");
    }
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn thumbnail_fingerprint_changes_when_the_document_changes() {
    let directory = temporary_directory("thumbnail-fingerprint");
    fs::create_dir(&directory).unwrap();
    let document = directory.join("document.pdf");
    fs::write(&document, b"first").unwrap();
    let first = ThumbnailCacheKey::for_path(&document).unwrap();

    OpenOptions::new()
        .append(true)
        .open(&document)
        .unwrap()
        .write_all(b"-changed")
        .unwrap();
    let changed = ThumbnailCacheKey::for_path(&document).unwrap();

    assert_ne!(first, changed);
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn persistent_thumbnail_round_trips_png_pixels() {
    let directory = temporary_directory("thumbnail-persistence");
    fs::create_dir(&directory).unwrap();
    let path = directory.join("thumbnail.png");
    let image = image::RgbaImage::from_pixel(3, 2, Rgba([10, 20, 30, 255]));

    persist_cover_thumbnail(&path, &image).unwrap();
    let restored = image::open(&path).unwrap().into_rgba8();

    assert_eq!(restored, image);
    fs::remove_dir_all(directory).unwrap();
}
