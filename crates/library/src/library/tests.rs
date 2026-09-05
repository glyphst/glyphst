use super::storage_helpers::*;
use super::*;

#[test]
fn memory_store_replaces_history_without_persistence() {
    let mut store = LibraryStore::memory();
    let entry = HistoryEntry {
        path: PathBuf::from("/tmp/in-memory.pdf"),
        opened: 1,
        position: position(1),
    };

    store.replace_history_entries(vec![entry.clone()]).unwrap();

    assert_eq!(store.history_entries(), &[entry]);
}

fn position(page: usize) -> DocumentPosition {
    DocumentPosition::fixed(page as f32, page as f32 * 10.0, 1.25, page)
}

fn temp_library_path(name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!(
        "glyphst-{name}-{}-{nanos}.redb",
        std::process::id()
    ))
}

#[test]
fn current_storage_uses_versioned_json_tables() {
    let database_path = temp_library_path("current-json-storage");
    let managed_root = database_path.with_extension("library-v2");
    let document_path = PathBuf::from("/tmp/current-json.pdf");
    {
        let mut store =
            LibraryStore::open_current_with_root(database_path.clone(), managed_root.clone())
                .unwrap();
        store
            .replace_history_entries(vec![HistoryEntry {
                path: document_path.clone(),
                opened: 42,
                position: position(7),
            }])
            .unwrap();
    }

    {
        let database = Database::open(&database_path).unwrap();
        let read_txn = database.begin_read().unwrap();
        {
            let metadata = read_txn.open_table(STORAGE_METADATA_TABLE).unwrap();
            assert_eq!(
                metadata
                    .get(STORAGE_SCHEMA_VERSION_KEY)
                    .unwrap()
                    .unwrap()
                    .value(),
                CURRENT_STORAGE_SCHEMA_VERSION
            );
        }
        {
            let history = read_txn.open_table(HISTORY_V2_TABLE).unwrap();
            let encoded = history
                .get(history_key(&document_path).as_slice())
                .unwrap()
                .unwrap();
            assert_eq!(encoded.value().first(), Some(&b'{'));
        }
    }

    let reopened =
        LibraryStore::open_current_with_root(database_path.clone(), managed_root.clone()).unwrap();
    assert_eq!(reopened.history_position(&document_path), Some(position(7)));
    drop(reopened);

    fs::remove_file(database_path).unwrap();
    fs::remove_dir_all(managed_root).unwrap();
}

#[test]
fn current_storage_does_not_touch_a_legacy_database() {
    let directory = temp_library_path("legacy-store-remains").with_extension("dir");
    fs::create_dir_all(&directory).unwrap();
    let legacy = directory.join("library.redb");
    let legacy_bytes = b"legacy-library-sentinel";
    fs::write(&legacy, legacy_bytes).unwrap();
    let current_root = directory.join("library-v2");
    let current_database = current_root.join("library.redb");

    let store = LibraryStore::open_current_with_root(current_database, current_root).unwrap();
    assert!(store.history_entries().is_empty());
    assert_eq!(fs::read(&legacy).unwrap(), legacy_bytes);
    drop(store);

    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn current_storage_rejects_unsupported_schema_without_replacing_it() {
    let path = temp_library_path("unsupported-schema");
    let unsupported = CURRENT_STORAGE_SCHEMA_VERSION + 1;
    {
        let database = Database::create(&path).unwrap();
        let write = database.begin_write().unwrap();
        write
            .open_table(STORAGE_METADATA_TABLE)
            .unwrap()
            .insert(STORAGE_SCHEMA_VERSION_KEY, unsupported)
            .unwrap();
        write.commit().unwrap();
    }

    let error = LibraryStore::open_for_test(path.clone())
        .err()
        .expect("unsupported schema must fail to open");
    assert!(
        error
            .to_string()
            .contains("unsupported library schema version")
    );
    assert!(!path.with_extension("managed-library").exists());
    {
        let database = Database::open(&path).unwrap();
        let read = database.begin_read().unwrap();
        assert_eq!(
            read.open_table(STORAGE_METADATA_TABLE)
                .unwrap()
                .get(STORAGE_SCHEMA_VERSION_KEY)
                .unwrap()
                .unwrap()
                .value(),
            unsupported
        );
    }
    fs::remove_file(path).unwrap();
}

#[test]
fn history_is_deduplicated_and_sorted() {
    let mut library = Library::default();
    library.record_history_at(Path::new("/tmp/a.pdf"), position(1), 10);
    library.record_history_at(Path::new("/tmp/b.pdf"), position(2), 11);
    library.record_history_at(Path::new("/tmp/a.pdf"), position(3), 12);

    assert_eq!(library.history.len(), 2);
    assert_eq!(
        library.history[0].path,
        normalize_path(Path::new("/tmp/a.pdf"))
    );
    assert_eq!(library.history[0].position.page, 3);
    assert_eq!(
        library.history[1].path,
        normalize_path(Path::new("/tmp/b.pdf"))
    );
}

#[test]
fn caps_history_entries() {
    let mut library = Library::default();
    for ix in 0..MAX_HISTORY_ENTRIES + 5 {
        library.record_history_at(
            Path::new(&format!("/tmp/{ix}.pdf")),
            position(ix + 1),
            ix as u64,
        );
    }

    assert_eq!(library.history.len(), MAX_HISTORY_ENTRIES);
    assert_eq!(library.history[0].opened, (MAX_HISTORY_ENTRIES + 4) as u64);
}

#[test]
fn stale_background_history_writes_do_not_overwrite_newer_positions() {
    let database_path = temp_library_path("ordered-history-persistence");
    let document_path = Path::new("/tmp/ordered-history.pdf");

    {
        let mut store = LibraryStore::open_for_test(database_path.clone()).unwrap();
        store.record_history(document_path, position(1));
        let stale = store
            .history_entry_persistence_snapshot(document_path)
            .unwrap();
        store.record_history(document_path, position(2));
        let current = store
            .history_entry_persistence_snapshot(document_path)
            .unwrap();

        current.persist().unwrap();
        stale.persist().unwrap();
    }

    let store = LibraryStore::open_for_test(database_path.clone()).unwrap();
    assert_eq!(store.history_position(document_path).unwrap().page, 2);
    let _ = fs::remove_file(database_path);
}

#[test]
fn removes_missing_history_entries() {
    let database_path = temp_library_path("missing-history");
    let existing_path = temp_library_path("existing-history").with_extension("pdf");
    let missing_path = temp_library_path("missing-history-pdf").with_extension("pdf");
    fs::write(&existing_path, b"%PDF-1.4\n").unwrap();

    {
        let mut store = LibraryStore::open_for_test(database_path.clone()).unwrap();
        store.record_history(&existing_path, position(1));
        store.record_history(&missing_path, position(2));

        assert!(store.remove_missing_history_entries().unwrap());
        assert_eq!(store.history_entries().len(), 1);
        assert_eq!(
            store.history_entries()[0].path,
            normalize_path(&existing_path)
        );
        assert!(!store.remove_missing_history_entries().unwrap());
    }

    let _ = fs::remove_file(database_path);
    let _ = fs::remove_file(existing_path);
}

#[test]
fn removing_history_entries_never_deletes_document_files() {
    let database_path = temp_library_path("remove-history-only");
    let document_path = temp_library_path("recent-document").with_extension("pdf");
    fs::write(&document_path, b"%PDF-1.4\n").unwrap();

    {
        let mut store = LibraryStore::open_for_test(database_path.clone()).unwrap();
        store.record_history(&document_path, position(1));
        assert_eq!(
            store
                .remove_history_entries(std::slice::from_ref(&document_path))
                .unwrap(),
            1
        );
        assert!(store.history_entries().is_empty());
        assert!(document_path.is_file());
    }

    let reopened = LibraryStore::open_for_test(database_path.clone()).unwrap();
    assert!(reopened.history_entries().is_empty());
    assert!(document_path.is_file());

    let _ = fs::remove_file(database_path);
    let _ = fs::remove_file(document_path);
}

#[test]
fn persists_history_bookmarks_and_highlights() {
    let path = temp_library_path("round-trip");
    let document_path = Path::new("/tmp/a.pdf");
    let highlight_rect = HighlightRect {
        page_idx: 0,
        x0: 10.0,
        y0: 20.0,
        x1: 30.0,
        y1: 40.0,
    };
    let highlight_id;
    let bookmark_id;
    let note_id;
    let ai_session_id;

    {
        let mut store = LibraryStore::open_for_test(path.clone()).unwrap();
        store.record_history(document_path, position(4));
        store.save().unwrap();
        bookmark_id = store
            .add_bookmark(document_path, Some("Chapter 1".to_string()), position(5))
            .unwrap();
        let highlight = store
            .add_highlight(
                document_path,
                HighlightColor::Yellow,
                vec![highlight_rect],
                "selected text",
                "",
            )
            .unwrap();
        highlight_id = highlight.id;
        assert!(
            store
                .set_highlight_color(highlight_id, HighlightColor::Blue)
                .unwrap()
        );
        let note = store
            .add_note(
                document_path,
                HighlightColor::Yellow,
                vec![highlight_rect],
                "selected text",
                "initial note",
            )
            .unwrap();
        note_id = note.id;
        assert!(
            store
                .update_note(note_id, "updated note", HighlightColor::Green)
                .unwrap()
        );
        ai_session_id = store.next_ai_session_id();
        store
            .save_ai_session(AiSession {
                id: ai_session_id,
                title: "Explain section".to_string(),
                messages: vec![
                    AiSessionMessage {
                        role: AiSessionRole::User,
                        content: "Explain @selection".to_string(),
                        request_content: Some("Explain @selection\n\nAttached context".to_string()),
                        references: Vec::new(),
                        occurrences: Vec::new(),
                        images: Vec::new(),
                        document_path: None,
                        document_label: None,
                        agent_parts: Vec::new(),
                    },
                    AiSessionMessage {
                        role: AiSessionRole::Assistant,
                        content: "Explanation".to_string(),
                        request_content: None,
                        references: Vec::new(),
                        occurrences: Vec::new(),
                        images: Vec::new(),
                        document_path: None,
                        document_label: None,
                        agent_parts: Vec::new(),
                    },
                ],
                queued_messages: Vec::new(),
                created: 100,
                updated: 101,
            })
            .unwrap();
    }

    {
        let mut store = LibraryStore::open_for_test(path.clone()).unwrap();
        assert_eq!(store.history_position(document_path).unwrap().page, 4);
        assert_eq!(store.bookmarks()[0].id, bookmark_id);
        let highlights = store.highlights_for(document_path);
        assert_eq!(highlights.len(), 1);
        assert_eq!(highlights[0].id, highlight_id);
        assert_eq!(highlights[0].color, HighlightColor::Blue);
        assert_eq!(highlights[0].rects, vec![highlight_rect]);
        let notes = store.notes_for(document_path);
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].id, note_id);
        assert_eq!(notes[0].color, HighlightColor::Green);
        assert_eq!(notes[0].rects, vec![highlight_rect]);
        assert_eq!(notes[0].body, "updated note");
        let ai_sessions = store.ai_sessions();
        assert_eq!(ai_sessions.len(), 1);
        assert_eq!(ai_sessions[0].id, ai_session_id);
        assert_eq!(ai_sessions[0].title, "Explain section");
        assert_eq!(ai_sessions[0].messages.len(), 2);
        assert_eq!(
            ai_sessions[0].messages[0].request_content.as_deref(),
            Some("Explain @selection\n\nAttached context")
        );

        assert!(store.remove_highlight(highlight_id).unwrap());
        assert!(store.remove_note(note_id).unwrap());
        assert!(store.remove_bookmark(bookmark_id).unwrap());
    }

    {
        let store = LibraryStore::open_for_test(path.clone()).unwrap();
        assert!(store.bookmarks().is_empty());
        assert!(store.highlights_for(document_path).is_empty());
        assert!(store.notes_for(document_path).is_empty());
    }

    let _ = fs::remove_file(path);
}

#[test]
fn undo_and_redo_cover_persistent_document_edits() {
    let database_path = temp_library_path("document-undo-redo");
    let document_path = Path::new("/tmp/document-undo-redo.pdf");
    let rect = HighlightRect {
        page_idx: 0,
        x0: 1.0,
        y0: 2.0,
        x1: 3.0,
        y1: 4.0,
    };
    let mut store = LibraryStore::open_for_test(database_path.clone()).unwrap();

    let bookmark_id = store
        .add_bookmark(document_path, Some("Chapter".to_string()), position(1))
        .unwrap();
    let highlight = store
        .add_highlight(
            document_path,
            HighlightColor::Yellow,
            vec![rect],
            "highlight",
            "",
        )
        .unwrap();
    assert!(
        store
            .set_highlight_color(highlight.id, HighlightColor::Blue)
            .unwrap()
    );
    let note = store
        .add_note(
            document_path,
            HighlightColor::Yellow,
            vec![rect],
            "note",
            "before",
        )
        .unwrap();
    assert!(
        store
            .update_note(note.id, "after", HighlightColor::Green)
            .unwrap()
    );
    assert!(
        store
            .set_document_rotation(document_path, DocumentRotation::Degrees90)
            .unwrap()
    );

    assert!(store.undo_document(document_path).unwrap());
    assert_eq!(
        store.document_rotation(document_path),
        DocumentRotation::Degrees0
    );
    assert!(store.undo_document(document_path).unwrap());
    assert_eq!(store.notes_for(document_path)[0].body, "before");
    assert!(store.undo_document(document_path).unwrap());
    assert!(store.notes_for(document_path).is_empty());
    assert!(store.undo_document(document_path).unwrap());
    assert_eq!(
        store.highlights_for(document_path)[0].color,
        HighlightColor::Yellow
    );
    assert!(store.undo_document(document_path).unwrap());
    assert!(store.highlights_for(document_path).is_empty());
    assert!(store.undo_document(document_path).unwrap());
    assert!(store.bookmarks().is_empty());
    assert!(!store.can_undo_document(document_path));
    assert!(store.can_redo_document(document_path));

    for _ in 0..6 {
        assert!(store.redo_document(document_path).unwrap());
    }
    assert!(!store.can_redo_document(document_path));
    assert_eq!(store.bookmarks()[0].id, bookmark_id);
    assert_eq!(
        store.highlights_for(document_path)[0],
        Highlight {
            color: HighlightColor::Blue,
            ..highlight.clone()
        }
    );
    assert_eq!(
        store.notes_for(document_path)[0],
        Note {
            body: "after".to_string(),
            color: HighlightColor::Green,
            ..note
        }
    );
    assert_eq!(
        store.document_rotation(document_path),
        DocumentRotation::Degrees90
    );

    let _ = fs::remove_file(database_path);
}

#[test]
fn undo_restores_deleted_objects_with_stable_ids() {
    let database_path = temp_library_path("document-delete-undo");
    let document_path = Path::new("/tmp/document-delete-undo.pdf");
    let rect = HighlightRect {
        page_idx: 0,
        x0: 1.0,
        y0: 2.0,
        x1: 3.0,
        y1: 4.0,
    };
    let mut store = LibraryStore::open_for_test(database_path.clone()).unwrap();
    let bookmark_id = store
        .add_bookmark(document_path, Some("Bookmark".to_string()), position(1))
        .unwrap();
    let highlight = store
        .add_highlight(
            document_path,
            HighlightColor::Yellow,
            vec![rect],
            "highlight",
            "",
        )
        .unwrap();
    let note = store
        .add_note(
            document_path,
            HighlightColor::Blue,
            vec![rect],
            "note",
            "body",
        )
        .unwrap();

    assert!(store.remove_bookmark(bookmark_id).unwrap());
    assert!(store.undo_document(document_path).unwrap());
    assert_eq!(store.bookmarks()[0].id, bookmark_id);
    assert!(store.redo_document(document_path).unwrap());
    assert!(store.bookmarks().is_empty());

    assert!(store.remove_highlight(highlight.id).unwrap());
    assert!(store.undo_document(document_path).unwrap());
    assert_eq!(store.highlights_for(document_path)[0], highlight);
    assert!(store.redo_document(document_path).unwrap());
    assert!(store.highlights_for(document_path).is_empty());

    assert!(store.remove_note(note.id).unwrap());
    assert!(store.undo_document(document_path).unwrap());
    assert_eq!(store.notes_for(document_path)[0], note);
    assert!(store.redo_document(document_path).unwrap());
    assert!(store.notes_for(document_path).is_empty());

    let next_highlight = store
        .add_highlight(document_path, HighlightColor::Green, vec![rect], "next", "")
        .unwrap();
    assert!(next_highlight.id > highlight.id);
    assert!(!store.can_redo_document(document_path));

    let _ = fs::remove_file(database_path);
}

#[test]
fn document_histories_are_independent_and_no_ops_preserve_redo() {
    let database_path = temp_library_path("independent-document-undo");
    let document_a = Path::new("/tmp/undo-a.pdf");
    let document_b = Path::new("/tmp/undo-b.pdf");
    let rect = HighlightRect {
        page_idx: 0,
        x0: 1.0,
        y0: 2.0,
        x1: 3.0,
        y1: 4.0,
    };
    let mut store = LibraryStore::open_for_test(database_path.clone()).unwrap();
    let highlight_a = store
        .add_highlight(document_a, HighlightColor::Yellow, vec![rect], "a", "")
        .unwrap();
    store
        .add_highlight(document_b, HighlightColor::Blue, vec![rect], "b", "")
        .unwrap();
    assert!(
        store
            .set_highlight_color(highlight_a.id, HighlightColor::Green)
            .unwrap()
    );
    assert!(store.undo_document(document_a).unwrap());
    assert_eq!(
        store.highlights_for(document_a)[0].color,
        HighlightColor::Yellow
    );
    assert_eq!(store.highlights_for(document_b).len(), 1);
    assert!(
        !store
            .set_highlight_color(highlight_a.id, HighlightColor::Yellow)
            .unwrap()
    );
    assert!(store.can_redo_document(document_a));
    assert!(store.redo_document(document_a).unwrap());
    assert_eq!(
        store.highlights_for(document_a)[0].color,
        HighlightColor::Green
    );
    assert!(store.undo_document(document_a).unwrap());
    store
        .add_note(
            document_a,
            HighlightColor::Yellow,
            vec![rect],
            "note",
            "body",
        )
        .unwrap();
    assert!(!store.can_redo_document(document_a));
    assert!(store.can_undo_document(document_b));

    let _ = fs::remove_file(database_path);
}

#[test]
fn revisions_distinguish_document_content_from_global_bookmarks() {
    let database_path = temp_library_path("document-edit-revisions");
    let document_a = Path::new("/tmp/revision-a.pdf");
    let document_b = Path::new("/tmp/revision-b.pdf");
    let rect = HighlightRect {
        page_idx: 0,
        x0: 1.0,
        y0: 2.0,
        x1: 3.0,
        y1: 4.0,
    };
    let mut store = LibraryStore::open_for_test(database_path.clone()).unwrap();

    let bookmark_id = store
        .add_bookmark(document_b, Some("Bookmark".to_string()), position(1))
        .unwrap();
    assert_eq!(
        store.document_edit_revision(document_a),
        DocumentEditRevision::default()
    );
    assert_eq!(
        store.document_edit_revision(document_b),
        DocumentEditRevision::default()
    );
    assert_eq!(store.bookmarks_revision(), 1);
    assert!(store.remove_bookmark(bookmark_id).unwrap());
    assert_eq!(store.bookmarks_revision(), 2);
    assert!(store.undo_document(document_b).unwrap());
    assert_eq!(store.bookmarks_revision(), 3);

    store
        .add_highlight(
            document_a,
            HighlightColor::Yellow,
            vec![rect],
            "highlight",
            "",
        )
        .unwrap();
    assert_eq!(
        store.document_edit_revision(document_a),
        DocumentEditRevision {
            annotations: 1,
            rotation: 0,
        }
    );
    assert_eq!(
        store.document_edit_revision(document_b),
        DocumentEditRevision::default()
    );
    assert_eq!(store.bookmarks_revision(), 3);
    assert!(
        store
            .set_document_rotation(document_a, DocumentRotation::Degrees90)
            .unwrap()
    );
    assert_eq!(
        store.document_edit_revision(document_a),
        DocumentEditRevision {
            annotations: 1,
            rotation: 1,
        }
    );

    let _ = fs::remove_file(database_path);
}

#[test]
fn bookmark_titles_are_automatic_customizable_and_undoable() {
    let database_path = temp_library_path("bookmark-titles");
    let document_path = Path::new("/tmp/bookmark-titles.pdf");
    let mut store = LibraryStore::open_for_test(database_path.clone()).unwrap();

    let bookmark_id = store
        .add_bookmark(document_path, None, position(3))
        .unwrap();
    assert_eq!(store.bookmarks_for(document_path).len(), 1);
    assert_eq!(store.bookmarks_for(document_path)[0].title, None);

    assert!(
        store
            .update_bookmark_title(bookmark_id, Some("  Useful passage  ".to_string()))
            .unwrap()
    );
    assert_eq!(
        store.bookmarks_for(document_path)[0].title.as_deref(),
        Some("Useful passage")
    );
    assert!(store.undo_document(document_path).unwrap());
    assert_eq!(store.bookmarks_for(document_path)[0].title, None);
    assert!(store.redo_document(document_path).unwrap());
    assert_eq!(
        store.bookmarks_for(document_path)[0].title.as_deref(),
        Some("Useful passage")
    );
    assert!(
        store
            .update_bookmark_title(bookmark_id, Some("  ".to_string()))
            .unwrap()
    );
    assert_eq!(store.bookmarks_for(document_path)[0].title, None);

    drop(store);
    let store = LibraryStore::open_for_test(database_path.clone()).unwrap();
    assert_eq!(store.bookmarks_for(document_path)[0].title, None);

    let _ = fs::remove_file(database_path);
}

#[test]
fn per_document_history_drops_oldest_edits() {
    let database_path = temp_library_path("document-edit-capacity");
    let document_path = Path::new("/tmp/document-edit-capacity.pdf");
    let mut store = LibraryStore::open_for_test(database_path.clone()).unwrap();

    for index in 0..MAX_DOCUMENT_EDIT_HISTORY_ENTRIES + 1 {
        store
            .add_bookmark(
                document_path,
                Some(format!("Bookmark {index}")),
                position(index),
            )
            .unwrap();
    }
    for _ in 0..MAX_DOCUMENT_EDIT_HISTORY_ENTRIES {
        assert!(store.undo_document(document_path).unwrap());
    }
    assert!(!store.undo_document(document_path).unwrap());
    assert_eq!(store.bookmarks().len(), 1);

    let _ = fs::remove_file(database_path);
}

#[test]
fn rotation_undo_preserves_other_document_preferences() {
    let database_path = temp_library_path("rotation-undo-preferences");
    let document_path = Path::new("/tmp/rotation-undo-preferences.epub");
    let preferences = EpubReaderPreferences {
        font_size: 19.0,
        line_height: 1.6,
        horizontal_margin: 32.0,
        max_content_width: 720.0,
        theme: glyphst_core::EpubTheme::Sepia,
        use_publisher_styles: false,
    };
    let mut store = LibraryStore::open_for_test(database_path.clone()).unwrap();
    store
        .set_document_target_language(document_path, "ja")
        .unwrap();
    store
        .set_epub_reader_preferences(document_path, preferences)
        .unwrap();
    assert!(
        store
            .set_document_rotation(document_path, DocumentRotation::Degrees180)
            .unwrap()
    );

    assert!(store.undo_document(document_path).unwrap());
    assert_eq!(
        store.document_rotation(document_path),
        DocumentRotation::Degrees0
    );
    assert_eq!(
        store.document_target_language(document_path).as_deref(),
        Some("ja")
    );
    assert_eq!(
        store.epub_reader_preferences(document_path),
        Some(preferences)
    );

    let _ = fs::remove_file(database_path);
}

#[test]
fn failed_edits_do_not_move_and_conflicts_clear_history() {
    let document_path = Path::new("/tmp/failed-document-undo.pdf");
    let rect = HighlightRect {
        page_idx: 0,
        x0: 1.0,
        y0: 2.0,
        x1: 3.0,
        y1: 4.0,
    };
    let mut unavailable = LibraryStore {
        library: Library::default(),
        database: None,
        managed_library_root: None,
        ai_image_memory: HashMap::new(),
        startup_error: Some("database startup failed".to_string()),
        document_edit_histories: HashMap::new(),
        next_document_edit_sequence: 0,
        bookmarks_revision: 0,
        history_entry_revisions: Arc::new(Mutex::new(HashMap::new())),
        catalog_revision: 0,
    };
    assert!(
        unavailable
            .add_highlight(
                document_path,
                HighlightColor::Yellow,
                vec![rect],
                "highlight",
                "",
            )
            .is_err()
    );
    assert!(unavailable.highlights_for(document_path).is_empty());
    assert!(!unavailable.can_undo_document(document_path));

    let database_path = temp_library_path("conflicting-document-undo");
    let mut store = LibraryStore::open_for_test(database_path.clone()).unwrap();
    let note = store
        .add_note(
            document_path,
            HighlightColor::Yellow,
            vec![rect],
            "note",
            "body",
        )
        .unwrap();
    assert!(
        store
            .update_note(note.id, "   ", HighlightColor::Blue)
            .is_err()
    );
    assert_eq!(store.notes_for(document_path)[0], note);
    assert!(store.undo_document(document_path).unwrap());
    assert!(store.notes_for(document_path).is_empty());

    let highlight = store
        .add_highlight(
            document_path,
            HighlightColor::Yellow,
            vec![rect],
            "highlight",
            "",
        )
        .unwrap();
    store
        .library
        .highlights
        .iter_mut()
        .find(|stored| stored.id == highlight.id)
        .unwrap()
        .color = HighlightColor::Blue;
    assert!(store.undo_document(document_path).is_err());
    assert!(!store.can_undo_document(document_path));
    assert!(!store.can_redo_document(document_path));

    let _ = fs::remove_file(database_path);
}

#[test]
fn edit_history_is_bounded_and_not_persisted() {
    let database_path = temp_library_path("bounded-document-undo");
    let oldest_path = PathBuf::from("/tmp/bounded-document-0.pdf");
    {
        let mut store = LibraryStore::open_for_test(database_path.clone()).unwrap();
        for index in 0..=MAX_DOCUMENT_EDIT_HISTORIES {
            let path = PathBuf::from(format!("/tmp/bounded-document-{index}.pdf"));
            store
                .add_bookmark(&path, Some(format!("Bookmark {index}")), position(index))
                .unwrap();
        }
        assert!(!store.can_undo_document(&oldest_path));
        assert!(store.can_undo_document(Path::new(&format!(
            "/tmp/bounded-document-{}.pdf",
            MAX_DOCUMENT_EDIT_HISTORIES
        ))));
    }

    let store = LibraryStore::open_for_test(database_path.clone()).unwrap();
    assert!(!store.can_undo_document(Path::new("/tmp/bounded-document-32.pdf")));
    assert_eq!(store.bookmarks().len(), MAX_DOCUMENT_EDIT_HISTORIES + 1);

    let _ = fs::remove_file(database_path);
}

#[test]
fn round_trips_v3_ai_session_mention_references_without_image_bytes() {
    let database_path = temp_library_path("ai-session-v3-references");
    let document_path = PathBuf::from("/tmp/v3-session.pdf");
    let references = vec![
        AiMentionReference::Selection,
        AiMentionReference::DocumentText,
        AiMentionReference::Page { page: 4 },
        AiMentionReference::PageImage { page: 5 },
        AiMentionReference::DocumentImage,
        AiMentionReference::RegionImage {
            reference: RegionImageReference::new([glyphst_core::PageRegion {
                page: glyphst_core::PageIndex(5),
                x0: 10.0,
                y0: 20.0,
                x1: 30.0,
                y1: 40.0,
            }]),
        },
    ];

    {
        let mut store = LibraryStore::open_for_test(database_path.clone()).unwrap();
        let session_id = store.next_ai_session_id();
        store
            .save_ai_session(AiSession {
                id: session_id,
                title: "Referenced context".to_string(),
                messages: vec![AiSessionMessage {
                    role: AiSessionRole::User,
                    content: "Compare the references".to_string(),
                    request_content: Some("Compare the attached context".to_string()),
                    references: references.clone(),
                    occurrences: Vec::new(),
                    images: Vec::new(),
                    document_path: Some(document_path.clone()),
                    document_label: Some("v3-session.pdf".to_string()),
                    agent_parts: Vec::new(),
                }],
                queued_messages: Vec::new(),
                created: 20,
                updated: 21,
            })
            .unwrap();
    }

    {
        let store = LibraryStore::open_for_test(database_path.clone()).unwrap();
        let sessions = store.ai_sessions();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].messages[0].references, references);
        assert_eq!(
            sessions[0].messages[0].document_path.as_ref(),
            Some(&document_path)
        );
        assert_eq!(
            sessions[0].messages[0].document_label.as_deref(),
            Some("v3-session.pdf")
        );
    }

    let _ = fs::remove_file(database_path);
}

#[test]
fn round_trips_v3_occurrences_with_duplicate_references() {
    let database_path = temp_library_path("ai-session-v3-duplicate-occurrences");
    let reference = AiMentionReference::Selection;
    let occurrences = vec![
        AiMentionOccurrence {
            start: 0,
            end: 10,
            reference: reference.clone(),
        },
        AiMentionOccurrence {
            start: 15,
            end: 25,
            reference: reference.clone(),
        },
    ];

    {
        let mut store = LibraryStore::open_for_test(database_path.clone()).unwrap();
        let session_id = store.next_ai_session_id();
        store
            .save_ai_session(AiSession {
                id: session_id,
                title: "Duplicate mentions".to_string(),
                messages: vec![AiSessionMessage {
                    role: AiSessionRole::User,
                    content: "@selection and @selection".to_string(),
                    request_content: Some("Compare the attached selections".to_string()),
                    references: vec![reference.clone(), reference],
                    occurrences: occurrences.clone(),
                    images: Vec::new(),
                    document_path: None,
                    document_label: None,
                    agent_parts: Vec::new(),
                }],
                queued_messages: Vec::new(),
                created: 30,
                updated: 31,
            })
            .unwrap();
    }

    {
        let store = LibraryStore::open_for_test(database_path.clone()).unwrap();
        let sessions = store.ai_sessions();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].messages[0].occurrences, occurrences);
        assert_eq!(sessions[0].messages[0].references.len(), 2);
    }

    let _ = fs::remove_file(database_path);
}

#[test]
fn round_trips_v6_selected_page_snapshots() {
    let database_path = temp_library_path("ai-session-v6-selected-pages");
    let occurrences = vec![
        AiMentionOccurrence {
            start: 0,
            end: 20,
            reference: AiMentionReference::SelectedPagesText {
                pages: vec![0, 2, 5],
            },
        },
        AiMentionOccurrence {
            start: 25,
            end: 46,
            reference: AiMentionReference::SelectedPagesImage { pages: vec![1, 4] },
        },
    ];

    let session_id = {
        let mut store = LibraryStore::open_for_test(database_path.clone()).unwrap();
        let session_id = store.next_ai_session_id();
        store
            .save_ai_session(AiSession {
                id: session_id,
                title: "Selected pages".to_string(),
                messages: vec![AiSessionMessage {
                    role: AiSessionRole::User,
                    content: "@selected-pages-text and @selected-pages-image".to_string(),
                    request_content: Some("Compare the attached pages".to_string()),
                    references: Vec::new(),
                    occurrences: occurrences.clone(),
                    images: Vec::new(),
                    document_path: None,
                    document_label: None,
                    agent_parts: Vec::new(),
                }],
                queued_messages: Vec::new(),
                created: 35,
                updated: 36,
            })
            .unwrap();
        session_id
    };

    {
        let store = LibraryStore::open_for_test(database_path.clone()).unwrap();
        let sessions = store.ai_sessions();
        assert_eq!(sessions[0].messages[0].occurrences, occurrences);
    }

    {
        let database = Database::open(&database_path).unwrap();
        let read_txn = database.begin_read().unwrap();
        let stored = read_txn
            .open_table(AI_SESSIONS_V6_TABLE)
            .unwrap()
            .get(session_id)
            .unwrap()
            .unwrap();
        let stored = decode::<StoredAiSessionV6>(stored.value()).unwrap();
        assert_eq!(stored.version, AI_SESSIONS_V6_VERSION);
        assert_eq!(stored.messages[0].occurrences, occurrences);
    }

    let _ = fs::remove_file(database_path);
}

#[test]
fn v6_round_trips_managed_images_agent_trace_and_queue() {
    let database_path = temp_library_path("ai-session-v6-images");
    let managed_root = database_path.with_extension("managed-library");
    let bytes = b"test-png-payload".to_vec();
    let session_id;
    let image;

    {
        let mut store = LibraryStore::open_for_test(database_path.clone()).unwrap();
        image = store
            .install_ai_image(Some("figure.png"), "image/png", 12, 8, &bytes)
            .unwrap();
        session_id = store.next_ai_session_id();
        store
            .save_ai_session(AiSession {
                id: session_id,
                title: "Image trace".to_string(),
                messages: vec![
                    AiSessionMessage {
                        role: AiSessionRole::User,
                        content: "What is shown?".to_string(),
                        request_content: None,
                        references: Vec::new(),
                        occurrences: Vec::new(),
                        images: vec![image.clone()],
                        document_path: None,
                        document_label: None,
                        agent_parts: Vec::new(),
                    },
                    AiSessionMessage {
                        role: AiSessionRole::Assistant,
                        content: "I inspected the page.".to_string(),
                        request_content: None,
                        references: Vec::new(),
                        occurrences: Vec::new(),
                        images: Vec::new(),
                        document_path: None,
                        document_label: None,
                        agent_parts: vec![
                            AiSessionAgentPart::ToolCall {
                                id: "call-1".to_string(),
                                name: "render_document_pages".to_string(),
                                arguments: "{\"pages\":[2]}".to_string(),
                                status: AiSessionToolStatus::Completed,
                            },
                            AiSessionAgentPart::ToolResult {
                                call_id: "call-1".to_string(),
                                tool_name: "render_document_pages".to_string(),
                                content: vec![AiSessionToolResultPart::Image(image.clone())],
                                is_error: false,
                            },
                            AiSessionAgentPart::AssistantText("I inspected the page.".to_string()),
                        ],
                    },
                ],
                queued_messages: vec![AiSessionMessage {
                    role: AiSessionRole::User,
                    content: "Then compare it.".to_string(),
                    request_content: None,
                    references: Vec::new(),
                    occurrences: Vec::new(),
                    images: vec![image.clone()],
                    document_path: None,
                    document_label: None,
                    agent_parts: Vec::new(),
                }],
                created: 50,
                updated: 51,
            })
            .unwrap();
    }

    let asset_path = managed_root
        .join("ai-images")
        .join(format!("{}.png", image.asset_id));
    assert!(asset_path.is_file());
    {
        let store = LibraryStore::open_for_test(database_path.clone()).unwrap();
        let sessions = store.ai_sessions();
        let session = &sessions[0];
        assert_eq!(session.queued_messages.len(), 1);
        assert_eq!(session.messages[0].images, vec![image.clone()]);
        assert_eq!(store.load_ai_image(&image).unwrap(), bytes);
        assert!(matches!(
            session.messages[1].agent_parts[1],
            AiSessionAgentPart::ToolResult { .. }
        ));
    }

    {
        let mut store = LibraryStore::open_for_test(database_path.clone()).unwrap();
        store
            .save_ai_session(AiSession {
                id: session_id,
                title: "Image trace".to_string(),
                messages: vec![AiSessionMessage {
                    role: AiSessionRole::User,
                    content: "No image now".to_string(),
                    request_content: None,
                    references: Vec::new(),
                    occurrences: Vec::new(),
                    images: Vec::new(),
                    document_path: None,
                    document_label: None,
                    agent_parts: Vec::new(),
                }],
                queued_messages: Vec::new(),
                created: 50,
                updated: 52,
            })
            .unwrap();
    }
    assert!(!asset_path.exists());

    let _ = fs::remove_file(database_path);
    let _ = fs::remove_dir_all(managed_root);
}

#[test]
fn round_trips_v3_region_occurrence_geometry() {
    let database_path = temp_library_path("ai-session-v3-region-geometry");
    let region_reference = RegionImageReference::new([
        glyphst_core::PageRegion {
            page: glyphst_core::PageIndex(2),
            x0: 10.25,
            y0: 20.5,
            x1: 30.75,
            y1: 40.125,
        },
        glyphst_core::PageRegion {
            page: glyphst_core::PageIndex(3),
            x0: 1.5,
            y0: 2.25,
            x1: 99.5,
            y1: 120.875,
        },
    ]);
    let occurrence = AiMentionOccurrence {
        start: 9,
        end: 22,
        reference: AiMentionReference::RegionImage {
            reference: region_reference.clone(),
        },
    };

    {
        let mut store = LibraryStore::open_for_test(database_path.clone()).unwrap();
        let session_id = store.next_ai_session_id();
        store
            .save_ai_session(AiSession {
                id: session_id,
                title: "Region geometry".to_string(),
                messages: vec![AiSessionMessage {
                    role: AiSessionRole::User,
                    content: "Describe @region-image".to_string(),
                    request_content: Some("Describe the attached image region".to_string()),
                    references: vec![AiMentionReference::RegionImage {
                        reference: region_reference.clone(),
                    }],
                    occurrences: vec![occurrence.clone()],
                    images: Vec::new(),
                    document_path: None,
                    document_label: None,
                    agent_parts: Vec::new(),
                }],
                queued_messages: Vec::new(),
                created: 40,
                updated: 41,
            })
            .unwrap();
    }

    {
        let store = LibraryStore::open_for_test(database_path.clone()).unwrap();
        let sessions = store.ai_sessions();
        assert_eq!(
            sessions[0].messages[0].occurrences,
            vec![occurrence.clone()]
        );
        let AiMentionReference::RegionImage { reference } =
            &sessions[0].messages[0].occurrences[0].reference
        else {
            panic!("expected a region image occurrence");
        };
        assert_eq!(reference, &region_reference);
    }

    let _ = fs::remove_file(database_path);
}

#[test]
fn ai_mention_reference_keeps_historical_variant_encoding_compatible() {
    #[derive(Serialize)]
    enum HistoricalAiMentionReference {
        Selection,
        DocumentText,
        Page { page: usize },
        PageImage { page: usize },
    }

    let historical = [
        (
            HistoricalAiMentionReference::Selection,
            AiMentionReference::Selection,
        ),
        (
            HistoricalAiMentionReference::DocumentText,
            AiMentionReference::DocumentText,
        ),
        (
            HistoricalAiMentionReference::Page { page: 7 },
            AiMentionReference::Page { page: 7 },
        ),
        (
            HistoricalAiMentionReference::PageImage { page: 9 },
            AiMentionReference::PageImage { page: 9 },
        ),
    ];

    for (historical, expected) in historical {
        let bytes = encode(&historical).unwrap();
        assert_eq!(decode::<AiMentionReference>(&bytes).unwrap(), expected);
    }
}

#[test]
fn reader_sidebar_keeps_historical_variant_encoding_compatible() {
    #[derive(Serialize)]
    enum HistoricalReaderSidebar {
        Thumbnails,
        TableOfContents,
        Notes,
    }

    let historical = [
        (
            HistoricalReaderSidebar::Thumbnails,
            ReaderSidebar::Thumbnails,
        ),
        (
            HistoricalReaderSidebar::TableOfContents,
            ReaderSidebar::TableOfContents,
        ),
        (HistoricalReaderSidebar::Notes, ReaderSidebar::Notes),
    ];

    for (historical, expected) in historical {
        let bytes = encode(&historical).unwrap();
        assert_eq!(decode::<ReaderSidebar>(&bytes).unwrap(), expected);
    }
}

#[test]
fn persists_sidebar_widths() {
    let path = temp_library_path("sidebar-widths");
    let widths = SidebarWidths {
        thumbnails: 222.0,
        toc: 333.0,
        bookmarks: 388.0,
        notes: 444.0,
        ai: 555.0,
    };

    {
        let mut store = LibraryStore::open_for_test(path.clone()).unwrap();
        store.set_sidebar_widths(widths).unwrap();
    }

    {
        let store = LibraryStore::open_for_test(path.clone()).unwrap();
        assert_eq!(store.sidebar_widths(), Some(widths));
    }

    let _ = fs::remove_file(path);
}

#[test]
fn legacy_sidebar_widths_receive_a_bookmarks_default() {
    let stored = StoredSidebarWidthsV1 {
        version: STORAGE_SCHEMA_VERSION,
        widths: LegacySidebarWidthsV1 {
            thumbnails: 210.0,
            toc: 275.0,
            notes: 320.0,
            ai: 380.0,
        },
    };
    let widths = decode::<StoredSidebarWidthsV1>(&encode(&stored).unwrap())
        .unwrap()
        .into_domain()
        .unwrap();

    assert_eq!(widths.thumbnails, 210.0);
    assert_eq!(widths.toc, 275.0);
    assert_eq!(widths.bookmarks, DEFAULT_BOOKMARKS_SIDEBAR_WIDTH);
    assert_eq!(widths.notes, 320.0);
    assert_eq!(widths.ai, 380.0);
}

#[test]
fn persists_document_browser_size() {
    let path = temp_library_path("document-browser-size");
    let size = DocumentBrowserSize {
        width: 864.0,
        height: 576.0,
    };

    {
        let mut store = LibraryStore::open_for_test(path.clone()).unwrap();
        store.set_document_browser_size(size).unwrap();
    }

    {
        let store = LibraryStore::open_for_test(path.clone()).unwrap();
        assert_eq!(store.document_browser_size(), Some(size));
    }

    let _ = fs::remove_file(path);
}

#[test]
fn persists_application_wide_home_preferences() {
    let path = temp_library_path("home-preferences");
    let preferences = HomePreferences {
        destination: HomeDestination::History,
        library: BrowserPresentation {
            view_mode: LibraryViewMode::Grid,
            sort: LibrarySort {
                field: LibrarySortField::Creator,
                direction: LibrarySortDirection::Ascending,
            },
        },
        history: BrowserPresentation::history_default(),
        trash: BrowserPresentation {
            view_mode: LibraryViewMode::Grouped,
            sort: LibrarySort {
                field: LibrarySortField::Year,
                direction: LibrarySortDirection::Descending,
            },
        },
    };

    {
        let mut store = LibraryStore::open_for_test(path.clone()).unwrap();
        store.set_home_preferences(preferences.clone()).unwrap();
    }

    let store = LibraryStore::open_for_test(path.clone()).unwrap();
    assert_eq!(store.home_preferences(), &preferences);

    let _ = fs::remove_file(path);
}

#[test]
fn legacy_new_tab_destinations_migrate_to_quick_open_sources() {
    let presentation = BrowserPresentation {
        view_mode: LibraryViewMode::Grid,
        sort: LibrarySort {
            field: LibrarySortField::Title,
            direction: LibrarySortDirection::Ascending,
        },
    };
    for destination in [
        ReaderNewTabDestination::Home,
        ReaderNewTabDestination::History,
    ] {
        let migrated = StoredReaderNewTabSessionV3 {
            destination,
            view_mode: presentation.view_mode,
            sort: presentation.sort,
        }
        .into_domain();
        assert_eq!(migrated.source, QuickOpenSource::History);
        assert_eq!(migrated.history, presentation);
    }
    for destination in [
        ReaderNewTabDestination::Library,
        ReaderNewTabDestination::Favorites,
        ReaderNewTabDestination::Tags,
        ReaderNewTabDestination::Trash,
        ReaderNewTabDestination::Tag(LibraryTagId(9)),
        ReaderNewTabDestination::SavedView(LibrarySavedViewId(7)),
    ] {
        let migrated = StoredReaderNewTabSessionV3 {
            destination,
            view_mode: presentation.view_mode,
            sort: presentation.sort,
        }
        .into_domain();
        assert_eq!(migrated.source, QuickOpenSource::Library);
        assert_eq!(migrated.library, presentation);
    }
}

#[test]
fn persists_reader_session_with_duplicate_document_tabs() {
    let database_path = temp_library_path("reader-session");
    let document_path = PathBuf::from("/tmp/reader-session.pdf");
    let session = ReaderSessionSnapshot {
        windows: vec![ReaderWindowSession {
            bounds: ReaderWindowBounds {
                x: -20.0,
                y: 40.0,
                width: 1200.0,
                height: 800.0,
                state: ReaderWindowState::Maximized,
            },
            pane_tree: ReaderPaneTree::Split {
                axis: ReaderSplitAxis::Horizontal,
                ratios: vec![0.35, 0.65],
                children: vec![
                    ReaderPaneTree::Pane(ReaderPaneSession {
                        id: 1,
                        tabs: vec![
                            ReaderTabSession {
                                id: 1,
                                kind: ReaderTabKind::Document {
                                    path: document_path.clone(),
                                    position: position(2),
                                },
                            },
                            ReaderTabSession {
                                id: 2,
                                kind: ReaderTabKind::Document {
                                    path: document_path.clone(),
                                    position: position(7),
                                },
                            },
                        ],
                        active_tab: Some(2),
                    }),
                    ReaderPaneTree::Pane(ReaderPaneSession {
                        id: 2,
                        tabs: vec![ReaderTabSession {
                            id: 3,
                            kind: ReaderTabKind::NewTab(ReaderNewTabSession {
                                source: QuickOpenSource::Library,
                                history: BrowserPresentation::history_default(),
                                library: BrowserPresentation {
                                    view_mode: LibraryViewMode::Grid,
                                    sort: LibrarySort {
                                        field: LibrarySortField::Title,
                                        direction: LibrarySortDirection::Ascending,
                                    },
                                },
                            }),
                        }],
                        active_tab: Some(3),
                    }),
                ],
            },
            active_pane: 1,
            dock_document_tab: Some(2),
            left_sidebar: Some(ReaderSidebar::Bookmarks),
            ai_panel_open: true,
            transparency_enabled: false,
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
                cache_key: Some("0123456789abcdef".to_string()),
                original_unit_ids: vec!["unit-2".to_string(), "unit-7".to_string()],
            }],
        }],
    };

    {
        let mut store = LibraryStore::open_for_test(database_path.clone()).unwrap();
        store.set_reader_session(session.clone()).unwrap();
    }

    let store = LibraryStore::open_for_test(database_path.clone()).unwrap();
    assert_eq!(store.reader_session(), Some(&session));

    let _ = fs::remove_file(database_path);
}

#[test]
fn loads_v1_reader_sessions_without_translation_metadata() {
    let database_path = temp_library_path("reader-session-v1-compatibility");
    let stored = StoredReaderSessionV1 {
        version: STORAGE_SCHEMA_VERSION,
        windows: vec![StoredReaderWindowSessionV1 {
            bounds: ReaderWindowBounds {
                x: 10.0,
                y: 20.0,
                width: 900.0,
                height: 700.0,
                state: ReaderWindowState::Windowed,
            },
            pane_tree: StoredReaderPaneTreeV1::Pane {
                id: 1,
                tabs: vec![StoredReaderTabV1 {
                    id: 1,
                    kind: StoredReaderTabKindV1::NewTab,
                }],
                active_tab: Some(1),
            },
            active_pane: 1,
            left_sidebar: None,
            ai_panel_open: false,
            transparency_enabled: false,
            synchronized_groups: Vec::new(),
        }],
    };

    {
        let database = Database::create(&database_path).unwrap();
        ensure_current_tables(&database).unwrap();
        let write_txn = database.begin_write().unwrap();
        {
            let mut table = write_txn.open_table(UI_STATE_V1_TABLE).unwrap();
            table
                .insert(READER_SESSION_KEY, encode(&stored).unwrap().as_slice())
                .unwrap();
        }
        write_txn.commit().unwrap();
    }

    let store = LibraryStore::open_for_test(database_path.clone()).unwrap();
    let session = store.reader_session().unwrap();
    assert_eq!(session.windows.len(), 1);
    assert!(session.windows[0].translation_sessions.is_empty());

    let _ = fs::remove_file(database_path);
}

#[test]
fn persists_document_target_language() {
    let database_path = temp_library_path("document-language");
    let document_path = Path::new("/tmp/translated.pdf");

    {
        let mut store = LibraryStore::open_for_test(database_path.clone()).unwrap();
        store
            .set_document_target_language(document_path, " Zh-Hans ")
            .unwrap();
        assert_eq!(
            store.document_target_language(document_path).as_deref(),
            Some("zh-hans")
        );
    }

    {
        let store = LibraryStore::open_for_test(database_path.clone()).unwrap();
        assert_eq!(
            store.document_target_language(document_path).as_deref(),
            Some("zh-hans")
        );
    }

    let _ = fs::remove_file(database_path);
}

#[test]
fn history_save_fails_when_database_is_unavailable() {
    let document_path = Path::new("/tmp/unavailable-history.epub");
    let mut store = LibraryStore {
        library: Library::default(),
        database: None,
        managed_library_root: None,
        ai_image_memory: HashMap::new(),
        startup_error: Some("database startup failed".to_string()),
        document_edit_histories: HashMap::new(),
        next_document_edit_sequence: 0,
        bookmarks_revision: 0,
        history_entry_revisions: Arc::new(Mutex::new(HashMap::new())),
        catalog_revision: 0,
    };
    store.record_history(document_path, position(1));

    let error = store.save().unwrap_err();

    assert_eq!(
        error.to_string(),
        "document library database is unavailable"
    );
    assert_eq!(store.history_position(document_path), Some(position(1)));
}

#[test]
fn document_preference_writes_fail_when_database_is_unavailable() {
    let document_path = Path::new("/tmp/unavailable-preferences.epub");
    let preferences = EpubReaderPreferences {
        font_size: 19.0,
        line_height: 1.7,
        horizontal_margin: 40.0,
        max_content_width: 680.0,
        theme: glyphst_core::EpubTheme::Sepia,
        use_publisher_styles: false,
    };
    let mut store = LibraryStore {
        library: Library::default(),
        database: None,
        managed_library_root: None,
        ai_image_memory: HashMap::new(),
        startup_error: Some("database startup failed".to_string()),
        document_edit_histories: HashMap::new(),
        next_document_edit_sequence: 0,
        bookmarks_revision: 0,
        history_entry_revisions: Arc::new(Mutex::new(HashMap::new())),
        catalog_revision: 0,
    };

    let language_error = store
        .set_document_target_language(document_path, "ja")
        .unwrap_err();
    assert_eq!(
        language_error.to_string(),
        "document library database is unavailable"
    );
    assert_eq!(store.document_target_language(document_path), None);

    let epub_error = store
        .set_epub_reader_preferences(document_path, preferences)
        .unwrap_err();
    assert_eq!(
        epub_error.to_string(),
        "document library database is unavailable"
    );
    assert_eq!(store.epub_reader_preferences(document_path), None);

    let rotation_error = store
        .set_document_rotation(document_path, DocumentRotation::Degrees90)
        .unwrap_err();
    assert_eq!(
        rotation_error.to_string(),
        "document library database is unavailable"
    );
    assert_eq!(
        store.document_rotation(document_path),
        DocumentRotation::default()
    );

    store
        .library
        .document_preferences
        .push(DocumentPreferences {
            path: normalize_path(document_path),
            target_language: "ja".to_string(),
            epub: Some(preferences),
            rotation: DocumentRotation::default(),
            page_layout: DocumentPageLayout::default(),
        });
    let clear_error = store
        .clear_epub_reader_preferences(document_path)
        .unwrap_err();
    assert_eq!(
        clear_error.to_string(),
        "document library database is unavailable"
    );
    assert_eq!(
        store.document_target_language(document_path).as_deref(),
        Some("ja")
    );
    assert_eq!(
        store.epub_reader_preferences(document_path),
        Some(preferences)
    );
}

#[test]
fn clears_epub_reader_preferences_and_preserves_target_language() {
    let database_path = temp_library_path("clear-epub-preferences");
    let document_path = Path::new("/tmp/book.epub");
    let preferences = EpubReaderPreferences {
        font_size: 19.0,
        line_height: 1.7,
        horizontal_margin: 40.0,
        max_content_width: 680.0,
        theme: glyphst_core::EpubTheme::Sepia,
        use_publisher_styles: false,
    };

    {
        let mut store = LibraryStore::open_for_test(database_path.clone()).unwrap();
        store
            .set_document_target_language(document_path, "ja")
            .unwrap();
        store
            .set_epub_reader_preferences(document_path, preferences)
            .unwrap();
        store.clear_epub_reader_preferences(document_path).unwrap();

        assert_eq!(store.epub_reader_preferences(document_path), None);
        assert_eq!(
            store.document_target_language(document_path).as_deref(),
            Some("ja")
        );
    }

    {
        let store = LibraryStore::open_for_test(database_path.clone()).unwrap();
        assert_eq!(store.epub_reader_preferences(document_path), None);
        assert_eq!(
            store.document_target_language(document_path).as_deref(),
            Some("ja")
        );
    }

    let _ = fs::remove_file(database_path);
}

#[test]
fn preference_setters_preserve_other_document_fields() {
    let database_path = temp_library_path("preference-field-preservation");
    let document_path = Path::new("/tmp/preference-fields.epub");
    let rotation_only_path = Path::new("/tmp/rotation-only.epub");
    let preferences = EpubReaderPreferences {
        font_size: 19.0,
        line_height: 1.7,
        horizontal_margin: 40.0,
        max_content_width: 680.0,
        theme: glyphst_core::EpubTheme::Sepia,
        use_publisher_styles: false,
    };

    {
        let mut store = LibraryStore::open_for_test(database_path.clone()).unwrap();
        store
            .set_document_rotation(document_path, DocumentRotation::Degrees90)
            .unwrap();
        store
            .set_document_page_layout(document_path, DocumentPageLayout::TwoColumn)
            .unwrap();
        store
            .set_epub_reader_preferences(document_path, preferences)
            .unwrap();
        assert_eq!(
            store.document_rotation(document_path),
            DocumentRotation::Degrees90
        );
        assert_eq!(
            store.document_page_layout(document_path),
            DocumentPageLayout::TwoColumn
        );

        store
            .set_document_target_language(document_path, "JA")
            .unwrap();
        assert_eq!(
            store.epub_reader_preferences(document_path),
            Some(preferences)
        );
        assert_eq!(
            store.document_rotation(document_path),
            DocumentRotation::Degrees90
        );
        assert_eq!(
            store.document_page_layout(document_path),
            DocumentPageLayout::TwoColumn
        );

        store.clear_epub_reader_preferences(document_path).unwrap();
        assert_eq!(
            store.document_target_language(document_path).as_deref(),
            Some("ja")
        );
        assert_eq!(
            store.document_rotation(document_path),
            DocumentRotation::Degrees90
        );
        assert_eq!(
            store.document_page_layout(document_path),
            DocumentPageLayout::TwoColumn
        );

        store
            .set_document_rotation(rotation_only_path, DocumentRotation::Degrees180)
            .unwrap();
        store
            .set_epub_reader_preferences(rotation_only_path, preferences)
            .unwrap();
        store
            .clear_epub_reader_preferences(rotation_only_path)
            .unwrap();
        assert_eq!(
            store.document_rotation(rotation_only_path),
            DocumentRotation::Degrees180
        );
        assert_eq!(store.epub_reader_preferences(rotation_only_path), None);
    }

    {
        let store = LibraryStore::open_for_test(database_path.clone()).unwrap();
        assert_eq!(
            store.document_target_language(document_path).as_deref(),
            Some("ja")
        );
        assert_eq!(
            store.document_rotation(document_path),
            DocumentRotation::Degrees90
        );
        assert_eq!(
            store.document_page_layout(document_path),
            DocumentPageLayout::TwoColumn
        );
        assert_eq!(
            store.document_rotation(rotation_only_path),
            DocumentRotation::Degrees180
        );
    }

    let _ = fs::remove_file(database_path);
}

#[test]
fn persists_document_rotation_round_trip() {
    let database_path = temp_library_path("document-rotation-round-trip");
    let document_path = Path::new("/tmp/rotated.pdf");

    {
        let mut store = LibraryStore::open_for_test(database_path.clone()).unwrap();
        store
            .set_document_rotation(document_path, DocumentRotation::Degrees270)
            .unwrap();
        assert_eq!(
            store.document_rotation(document_path),
            DocumentRotation::Degrees270
        );
    }

    {
        let store = LibraryStore::open_for_test(database_path.clone()).unwrap();
        assert_eq!(
            store.document_rotation(document_path),
            DocumentRotation::Degrees270
        );
    }

    let _ = fs::remove_file(database_path);
}

#[test]
fn persists_document_page_layout_round_trip() {
    let database_path = temp_library_path("document-page-layout-round-trip");
    let document_path = Path::new("/tmp/two-column.pdf");

    {
        let mut store = LibraryStore::open_for_test(database_path.clone()).unwrap();
        assert_eq!(
            store.document_page_layout(document_path),
            DocumentPageLayout::SingleColumn
        );
        assert!(
            store
                .set_document_page_layout(document_path, DocumentPageLayout::TwoColumn)
                .unwrap()
        );
    }

    {
        let store = LibraryStore::open_for_test(database_path.clone()).unwrap();
        assert_eq!(
            store.document_page_layout(document_path),
            DocumentPageLayout::TwoColumn
        );
    }

    let _ = fs::remove_file(database_path);
}

#[test]
fn epub_preferences_do_not_create_a_target_language_override() {
    let database_path = temp_library_path("epub-preferences-without-language");
    let document_path = Path::new("/tmp/book-with-global-language.epub");
    let preferences = EpubReaderPreferences {
        font_size: 19.0,
        line_height: 1.7,
        horizontal_margin: 40.0,
        max_content_width: 680.0,
        theme: glyphst_core::EpubTheme::Sepia,
        use_publisher_styles: false,
    };

    {
        let mut store = LibraryStore::open_for_test(database_path.clone()).unwrap();
        store
            .set_epub_reader_preferences(document_path, preferences)
            .unwrap();
        assert_eq!(store.document_target_language(document_path), None);
    }

    {
        let mut store = LibraryStore::open_for_test(database_path.clone()).unwrap();
        assert_eq!(store.document_target_language(document_path), None);
        assert_eq!(
            store.epub_reader_preferences(document_path),
            Some(preferences)
        );
        store.clear_epub_reader_preferences(document_path).unwrap();
        assert_eq!(store.library.document_preferences.len(), 0);
    }

    {
        let store = LibraryStore::open_for_test(database_path.clone()).unwrap();
        assert_eq!(store.document_target_language(document_path), None);
        assert_eq!(store.epub_reader_preferences(document_path), None);
    }

    let _ = fs::remove_file(database_path);
}

#[test]
fn persists_epub_positions_annotations_and_preferences() {
    let database_path = temp_library_path("epub-round-trip");
    let document_path = PathBuf::from("/tmp/book.epub");
    let start = SemanticTextAnchor::new(12, "chapter text", "before ", " after");
    let end = SemanticTextAnchor::new(24, "selection end", "middle ", " suffix");
    let position = DocumentPosition::reflow(start.clone(), 0.25, 0.4, 1.5, 3);
    let range = SemanticTextRange {
        start: start.clone(),
        end: end.clone(),
    };
    let preferences = EpubReaderPreferences {
        font_size: 19.0,
        line_height: 1.7,
        horizontal_margin: 40.0,
        max_content_width: 680.0,
        theme: glyphst_core::EpubTheme::Sepia,
        use_publisher_styles: false,
    };

    {
        let mut store = LibraryStore::open_for_test(database_path.clone()).unwrap();
        store.record_history(&document_path, position.clone());
        store
            .add_bookmark(
                &document_path,
                Some("Chapter".to_string()),
                position.clone(),
            )
            .unwrap();
        store
            .add_reflow_highlight(
                &document_path,
                HighlightColor::Yellow,
                range.clone(),
                Vec::new(),
                "selected text",
                "",
            )
            .unwrap();
        store
            .add_reflow_note(
                &document_path,
                HighlightColor::Blue,
                range.clone(),
                Vec::new(),
                "selected text",
                "remember this",
            )
            .unwrap();
        store
            .set_epub_reader_preferences(&document_path, preferences)
            .unwrap();
        store.save().unwrap();
    }

    {
        let store = LibraryStore::open_for_test(database_path.clone()).unwrap();
        assert_eq!(
            store.history_position(&document_path),
            Some(position.clone())
        );
        assert_eq!(store.bookmarks()[0].position, position);
        assert_eq!(
            store.highlights_for(&document_path)[0].semantic_range,
            Some(range.clone())
        );
        assert_eq!(
            store.notes_for(&document_path)[0].semantic_range,
            Some(range)
        );
        assert_eq!(
            store.epub_reader_preferences(&document_path),
            Some(preferences)
        );
    }

    let _ = fs::remove_file(database_path);
}

#[cfg(unix)]
#[test]
fn serializes_non_utf8_paths_without_loss() {
    use std::os::unix::ffi::OsStringExt as _;

    let path = PathBuf::from(std::ffi::OsString::from_vec(vec![
        b'/', b't', b'm', b'p', b'/', 0xff, b'.', b'p', b'd', b'f',
    ]));
    let entry = HistoryEntry {
        path,
        opened: 1,
        position: position(0),
    };

    assert_eq!(
        decode::<StoredHistoryEntryV2>(&encode(&StoredHistoryEntryV2::from(&entry)).unwrap())
            .unwrap()
            .into_domain()
            .unwrap(),
        entry
    );

    let preferences = DocumentPreferences {
        path: entry.path.clone(),
        target_language: "ja".to_string(),
        epub: None,
        rotation: DocumentRotation::default(),
        page_layout: DocumentPageLayout::default(),
    };
    assert_eq!(
        decode::<StoredDocumentPreferencesV4>(
            &encode(&StoredDocumentPreferencesV4::from(&preferences)).unwrap()
        )
        .unwrap()
        .into_domain()
        .unwrap(),
        preferences
    );
}

#[cfg(unix)]
#[test]
fn persists_non_utf8_paths_for_all_path_records() {
    use std::os::unix::ffi::OsStringExt as _;

    let database_path = temp_library_path("non-utf8-paths");
    let document_path = PathBuf::from(std::ffi::OsString::from_vec(vec![
        b'/', b't', b'm', b'p', b'/', 0xff, b'.', b'p', b'd', b'f',
    ]));
    let rect = HighlightRect {
        page_idx: 1,
        x0: 1.0,
        y0: 2.0,
        x1: 3.0,
        y1: 4.0,
    };

    {
        let mut store = LibraryStore::open_for_test(database_path.clone()).unwrap();
        store.record_history(&document_path, position(1));
        store.save().unwrap();
        store
            .add_bookmark(&document_path, Some("bookmark".to_string()), position(2))
            .unwrap();
        store
            .add_highlight(
                &document_path,
                HighlightColor::Yellow,
                vec![rect],
                "highlight",
                "",
            )
            .unwrap();
        store
            .add_note(
                &document_path,
                HighlightColor::Blue,
                vec![rect],
                "note",
                "body",
            )
            .unwrap();
        let session_id = store.next_ai_session_id();
        store
            .save_ai_session(AiSession {
                id: session_id,
                title: "session".to_string(),
                messages: vec![AiSessionMessage {
                    role: AiSessionRole::User,
                    content: "message".to_string(),
                    request_content: None,
                    references: Vec::new(),
                    occurrences: Vec::new(),
                    images: Vec::new(),
                    document_path: Some(document_path.clone()),
                    document_label: Some("normalized.pdf".to_string()),
                    agent_parts: Vec::new(),
                }],
                queued_messages: Vec::new(),
                created: 1,
                updated: 1,
            })
            .unwrap();
    }

    {
        let store = LibraryStore::open_for_test(database_path.clone()).unwrap();
        let normalized = normalize_path(&document_path);
        assert_eq!(store.history_entries()[0].path, normalized);
        assert_eq!(store.bookmarks()[0].path, normalized);
        assert_eq!(store.highlights_for(&document_path)[0].path, normalized);
        assert_eq!(store.notes_for(&document_path)[0].path, normalized);
        assert_eq!(
            store.ai_sessions()[0].messages[0].document_path.as_ref(),
            Some(&normalized)
        );
    }

    let _ = fs::remove_file(database_path);
}

#[test]
fn catalog_persists_items_tags_saved_views_and_trash() {
    let database_path = temp_library_path("catalog-roundtrip");
    let document_path = database_path.with_extension("pdf");
    fs::write(&document_path, b"%PDF-1.4\n").unwrap();

    let item_id = {
        let mut store = LibraryStore::open_for_test(database_path.clone()).unwrap();
        let mut metadata = LibraryItemMetadata::for_path(&document_path);
        metadata.title = "A Research Paper".to_string();
        metadata.creators.push(LibraryCreator {
            name: "Ada Lovelace".to_string(),
            role: LibraryCreatorRole::Author,
        });
        metadata.year = Some(1843);
        let LibraryAddOutcome::Added(item_id) =
            store.add_linked_item(&document_path, metadata).unwrap()
        else {
            panic!("expected a new library item");
        };
        store
            .add_tag_to_items(&[item_id], "  Machine   Learning ")
            .unwrap();
        store.set_library_item_favorite(item_id, true).unwrap();
        let tag_id = store.library_tags()[0].id;
        store
            .save_library_view(
                "Favorite ML",
                LibraryFilter {
                    tag_ids: vec![tag_id],
                    favorite: Some(true),
                    ..LibraryFilter::default()
                },
                LibrarySort::default(),
                LibraryViewMode::Table,
            )
            .unwrap();
        item_id
    };

    {
        let mut store = LibraryStore::open_for_test(database_path.clone()).unwrap();
        assert_eq!(store.library_items().len(), 1);
        assert_eq!(store.library_items()[0].title, "A Research Paper");
        assert_eq!(store.library_tags()[0].name, "Machine Learning");
        assert_eq!(store.library_saved_views().len(), 1);
        assert_eq!(
            store.query_library_items(
                &store.library_saved_views()[0].filter,
                store.library_saved_views()[0].sort,
            ),
            vec![item_id]
        );
        assert_eq!(store.trash_library_items(&[item_id]).unwrap(), 1);
        assert!(
            store
                .query_library_items(&LibraryFilter::default(), LibrarySort::default())
                .is_empty()
        );
        assert_eq!(
            store.query_library_items(
                &LibraryFilter {
                    trash: LibraryTrashFilter::Trashed,
                    ..LibraryFilter::default()
                },
                LibrarySort::default(),
            ),
            vec![item_id]
        );
        assert!(document_path.exists());
    }

    let _ = fs::remove_file(document_path);
    let _ = fs::remove_file(database_path);
}

#[test]
fn catalog_persists_nested_folders_and_single_parent_locations() {
    let database_path = temp_library_path("catalog-folders");
    let first_path = database_path.with_extension("first.pdf");
    let second_path = database_path.with_extension("second.pdf");
    fs::write(&first_path, b"%PDF-1.4\nfirst").unwrap();
    fs::write(&second_path, b"%PDF-1.4\nsecond").unwrap();

    let (first_id, second_id, papers_id, ml_id, books_id) = {
        let mut store = LibraryStore::open_for_test(database_path.clone()).unwrap();
        let LibraryAddOutcome::Added(first_id) = store
            .add_linked_item(&first_path, LibraryItemMetadata::for_path(&first_path))
            .unwrap()
        else {
            panic!("expected first item to be added");
        };
        let LibraryAddOutcome::Added(second_id) = store
            .add_linked_item(&second_path, LibraryItemMetadata::for_path(&second_path))
            .unwrap()
        else {
            panic!("expected second item to be added");
        };
        let papers_id = store.create_library_collection(None, " Papers ").unwrap();
        let ml_id = store
            .create_library_collection(Some(papers_id), "Machine Learning")
            .unwrap();
        let books_id = store.create_library_collection(None, "Books").unwrap();
        assert!(store.create_library_collection(None, "papers").is_err());
        assert!(
            store
                .move_library_collection(papers_id, Some(ml_id))
                .is_err()
        );
        assert_eq!(
            store.add_items_to_collection(&[first_id], ml_id).unwrap(),
            1
        );
        assert_eq!(
            store
                .add_items_to_collection(&[first_id], books_id)
                .unwrap(),
            1
        );
        assert!(store.direct_collection_items(Some(ml_id)).is_empty());
        assert_eq!(
            store.direct_collection_items(Some(books_id)),
            vec![first_id]
        );
        assert_eq!(store.direct_collection_items(None), vec![second_id]);
        assert_eq!(
            store.collection_path_label(ml_id),
            "Papers/Machine Learning"
        );
        (first_id, second_id, papers_id, ml_id, books_id)
    };

    {
        let mut store = LibraryStore::open_for_test(database_path.clone()).unwrap();
        assert_eq!(store.library_collections().len(), 3);
        assert_eq!(
            store
                .collections_for_item(first_id)
                .into_iter()
                .map(|collection| collection.id)
                .collect::<Vec<_>>(),
            vec![books_id]
        );
        assert_eq!(store.delete_library_collection(papers_id).unwrap(), 2);
        assert!(store.library_collection(papers_id).is_none());
        assert!(store.library_collection(ml_id).is_none());
        assert_eq!(
            store
                .collections_for_item(first_id)
                .into_iter()
                .map(|collection| collection.id)
                .collect::<Vec<_>>(),
            vec![books_id]
        );
        assert_eq!(store.direct_collection_items(None), vec![second_id]);
        assert!(first_path.exists());
        assert!(second_path.exists());
    }

    let _ = fs::remove_file(first_path);
    let _ = fs::remove_file(second_path);
    let _ = fs::remove_file(&database_path);
    let _ = fs::remove_dir_all(database_path.with_extension("managed-library"));
}

#[test]
fn relinking_catalog_item_moves_path_backed_reader_data_atomically() {
    let database_path = temp_library_path("catalog-relink");
    let old_path = database_path.with_extension("old.pdf");
    let new_path = database_path.with_extension("new.pdf");
    fs::write(&old_path, b"%PDF-1.4\nold").unwrap();
    fs::write(&new_path, b"%PDF-1.4\nnew").unwrap();

    {
        let mut store = LibraryStore::open_for_test(database_path.clone()).unwrap();
        let LibraryAddOutcome::Added(item_id) = store
            .add_linked_item(&old_path, LibraryItemMetadata::for_path(&old_path))
            .unwrap()
        else {
            panic!("expected a new library item");
        };
        let managed_old_path = store.primary_attachment(item_id).unwrap().path.clone();
        store.record_history(&managed_old_path, position(3));
        store
            .add_bookmark(&managed_old_path, Some("bookmark".to_string()), position(4))
            .unwrap();
        store.relink_library_attachment(item_id, &new_path).unwrap();

        let managed_path = store.primary_attachment(item_id).unwrap().path.clone();
        assert_ne!(managed_path, normalize_path(&new_path));
        assert_eq!(store.history_entries()[0].path, managed_path);
        assert_eq!(store.bookmarks()[0].path, managed_path);
        assert!(old_path.exists());
        assert!(new_path.exists());
    }

    {
        let store = LibraryStore::open_for_test(database_path.clone()).unwrap();
        assert_eq!(store.library_items().len(), 1);
        let managed_path = store.library.library_attachments[0].path.clone();
        assert_ne!(managed_path, normalize_path(&new_path));
        assert_eq!(store.history_entries()[0].path, managed_path);
        assert_eq!(store.bookmarks()[0].path, managed_path);
    }

    let _ = fs::remove_file(old_path);
    let _ = fs::remove_file(new_path);
    let _ = fs::remove_file(&database_path);
    let _ = fs::remove_dir_all(database_path.with_extension("managed-library"));
}

#[test]
fn catalog_queries_compose_facets_and_tag_renames_merge_saved_views() {
    let first_path = temp_library_path("catalog-query-first").with_extension("pdf");
    let second_path = temp_library_path("catalog-query-second").with_extension("epub");
    let third_path = temp_library_path("catalog-query-third").with_extension("pdf");
    for path in [&first_path, &second_path, &third_path] {
        fs::write(path, b"document").unwrap();
    }
    let mut store = LibraryStore::memory();

    let mut first = LibraryItemMetadata::for_path(&first_path);
    first.title = "Graph Neural Networks".to_string();
    first.year = Some(2024);
    first.creators = vec![LibraryCreator {
        name: "Ada Researcher".to_string(),
        role: LibraryCreatorRole::Author,
    }];
    let LibraryAddOutcome::Added(first_id) = store.add_linked_item(&first_path, first).unwrap()
    else {
        panic!("expected first item");
    };

    let mut second = LibraryItemMetadata::for_path(&second_path);
    second.title = "Foundations of Graphs".to_string();
    second.year = Some(2020);
    let LibraryAddOutcome::Added(second_id) = store.add_linked_item(&second_path, second).unwrap()
    else {
        panic!("expected second item");
    };

    let mut third = LibraryItemMetadata::for_path(&third_path);
    third.title = "Unrelated Paper".to_string();
    third.year = Some(2024);
    let LibraryAddOutcome::Added(third_id) = store.add_linked_item(&third_path, third).unwrap()
    else {
        panic!("expected third item");
    };

    let theory = store
        .add_tag_to_items(&[first_id, second_id], "Theory")
        .unwrap();
    let reviewed = store
        .add_tag_to_items(&[first_id, third_id], "Reviewed")
        .unwrap();
    store.set_library_item_favorite(first_id, true).unwrap();
    store
        .set_library_item_status(first_id, ReadingStatus::Reading)
        .unwrap();

    assert_eq!(
        store.query_library_items(
            &LibraryFilter {
                tag_ids: vec![theory, reviewed],
                kinds: vec![LibraryItemKind::Paper],
                statuses: vec![ReadingStatus::Reading],
                favorite: Some(true),
                year_min: Some(2024),
                text: "ada".to_string(),
                ..LibraryFilter::default()
            },
            LibrarySort::default(),
        ),
        vec![first_id]
    );
    assert_eq!(
        store.query_library_items(
            &LibraryFilter {
                text: "theory".to_string(),
                ..LibraryFilter::default()
            },
            LibrarySort {
                field: LibrarySortField::Title,
                direction: LibrarySortDirection::Ascending,
            },
        ),
        vec![second_id, first_id]
    );

    store
        .save_library_view(
            "Theory",
            LibraryFilter {
                tag_ids: vec![theory],
                ..LibraryFilter::default()
            },
            LibrarySort::default(),
            LibraryViewMode::Table,
        )
        .unwrap();
    assert_eq!(
        store.rename_library_tag(theory, "reviewed").unwrap(),
        reviewed
    );
    assert_eq!(store.library_tags().len(), 1);
    assert_eq!(store.tags_for_item(first_id).len(), 1);
    assert_eq!(
        store.library_saved_views()[0].filter.tag_ids,
        vec![reviewed]
    );

    for path in [first_path, second_path, third_path] {
        let _ = fs::remove_file(path);
    }
}

#[test]
fn catalog_trash_is_reversible_and_permanent_delete_preserves_source_and_history() {
    let document_path = temp_library_path("catalog-trash").with_extension("pdf");
    fs::write(&document_path, b"document").unwrap();
    let mut store = LibraryStore::memory();
    let LibraryAddOutcome::Added(item_id) = store
        .add_linked_item(
            &document_path,
            LibraryItemMetadata::for_path(&document_path),
        )
        .unwrap()
    else {
        panic!("expected a new item");
    };
    store.record_history(&document_path, position(2));

    assert_eq!(
        store.permanently_delete_library_items(&[item_id]).unwrap(),
        0
    );
    assert_eq!(store.trash_library_items(&[item_id]).unwrap(), 1);
    assert!(matches!(
        store
            .add_linked_item(
                &document_path,
                LibraryItemMetadata::for_path(&document_path),
            )
            .unwrap(),
        LibraryAddOutcome::ExistingInTrash(id) if id == item_id
    ));
    assert_eq!(store.restore_library_items(&[item_id]).unwrap(), 1);
    assert_eq!(store.trash_library_items(&[item_id]).unwrap(), 1);
    assert_eq!(
        store.permanently_delete_library_items(&[item_id]).unwrap(),
        1
    );

    assert!(store.library_item(item_id).is_none());
    assert_eq!(store.history_entries().len(), 1);
    assert!(document_path.exists());

    let _ = fs::remove_file(document_path);
}

#[test]
fn managed_import_copies_content_deduplicates_and_deletes_only_owned_files() {
    let database_path = temp_library_path("managed-import");
    let managed_root = database_path.with_extension("managed-library");
    let first_source = database_path.with_extension("first.pdf");
    let duplicate_source = database_path.with_extension("duplicate.pdf");
    let content = b"%PDF-1.4\nmanaged library test";
    fs::write(&first_source, content).unwrap();
    fs::write(&duplicate_source, content).unwrap();

    let mut store = LibraryStore::open_for_test(database_path.clone()).unwrap();
    let LibraryAddOutcome::Added(item_id) = store
        .add_linked_item(&first_source, LibraryItemMetadata::for_path(&first_source))
        .unwrap()
    else {
        panic!("expected a new managed item");
    };
    let managed_path = store.primary_attachment(item_id).unwrap().path.clone();
    assert!(managed_path.starts_with(managed_root.join("files")));
    assert_ne!(managed_path, normalize_path(&first_source));
    assert_eq!(fs::read(&managed_path).unwrap(), content);

    fs::remove_file(&first_source).unwrap();
    assert_eq!(fs::read(&managed_path).unwrap(), content);
    assert!(matches!(
        store
            .add_linked_item(
                &duplicate_source,
                LibraryItemMetadata::for_path(&duplicate_source),
            )
            .unwrap(),
        LibraryAddOutcome::Existing(id) if id == item_id
    ));
    assert_eq!(store.library_items().len(), 1);

    store.trash_library_items(&[item_id]).unwrap();
    assert!(matches!(
        store
            .add_linked_item(
                &duplicate_source,
                LibraryItemMetadata::for_path(&duplicate_source),
            )
            .unwrap(),
        LibraryAddOutcome::ExistingInTrash(id) if id == item_id
    ));
    assert_eq!(
        store.permanently_delete_library_items(&[item_id]).unwrap(),
        1
    );
    assert!(!managed_path.exists());
    assert!(duplicate_source.exists());

    let _ = fs::remove_file(duplicate_source);
    let _ = fs::remove_file(database_path);
    let _ = fs::remove_dir_all(managed_root);
}

#[test]
fn mixed_tree_moves_are_disjoint_and_can_move_items_back_to_root() {
    let first_path = temp_library_path("mixed-tree-first").with_extension("pdf");
    let second_path = temp_library_path("mixed-tree-second").with_extension("pdf");
    fs::write(&first_path, b"first").unwrap();
    fs::write(&second_path, b"second").unwrap();
    let mut store = LibraryStore::memory();
    let LibraryAddOutcome::Added(first_id) = store
        .add_linked_item(&first_path, LibraryItemMetadata::for_path(&first_path))
        .unwrap()
    else {
        panic!("expected first item");
    };
    let LibraryAddOutcome::Added(second_id) = store
        .add_linked_item(&second_path, LibraryItemMetadata::for_path(&second_path))
        .unwrap()
    else {
        panic!("expected second item");
    };
    let papers = store.create_library_collection(None, "Papers").unwrap();
    let inner = store
        .create_library_collection(Some(papers), "Inner")
        .unwrap();
    let archive = store.create_library_collection(None, "Archive").unwrap();
    store.add_items_to_collection(&[first_id], inner).unwrap();

    assert_eq!(
        store.disjoint_library_nodes(&[
            LibraryNodeId::Collection(papers),
            LibraryNodeId::Item(first_id),
            LibraryNodeId::Item(second_id),
        ]),
        vec![
            LibraryNodeId::Collection(papers),
            LibraryNodeId::Item(second_id),
        ]
    );
    assert_eq!(
        store
            .move_library_nodes(
                &[
                    LibraryNodeId::Collection(papers),
                    LibraryNodeId::Item(first_id),
                    LibraryNodeId::Item(second_id),
                ],
                Some(archive),
            )
            .unwrap(),
        2
    );
    assert_eq!(
        store.library_collection(papers).unwrap().parent_id,
        Some(archive)
    );
    assert_eq!(store.item_parent(first_id), Some(inner));
    assert_eq!(store.item_parent(second_id), Some(archive));

    assert_eq!(
        store
            .move_library_nodes(&[LibraryNodeId::Item(first_id)], None)
            .unwrap(),
        1
    );
    assert_eq!(store.item_parent(first_id), None);
    assert!(
        store
            .validate_library_move(&[LibraryNodeId::Collection(papers)], Some(inner))
            .is_err()
    );

    let _ = fs::remove_file(first_path);
    let _ = fs::remove_file(second_path);
}

#[test]
fn deleting_a_folder_trashes_its_items_and_restore_recreates_the_path() {
    let document_path = temp_library_path("folder-trash-restore").with_extension("pdf");
    fs::write(&document_path, b"document").unwrap();
    let mut store = LibraryStore::memory();
    let LibraryAddOutcome::Added(item_id) = store
        .add_linked_item(
            &document_path,
            LibraryItemMetadata::for_path(&document_path),
        )
        .unwrap()
    else {
        panic!("expected item");
    };
    let papers = store.create_library_collection(None, "Papers").unwrap();
    let topic = store
        .create_library_collection(Some(papers), "Topic")
        .unwrap();
    store.add_items_to_collection(&[item_id], topic).unwrap();

    assert_eq!(store.delete_library_collection(papers).unwrap(), 2);
    assert!(store.library_item(item_id).unwrap().trashed_at.is_some());
    assert_eq!(store.item_parent(item_id), None);
    assert!(store.library_collections().is_empty());

    assert_eq!(store.restore_library_items(&[item_id]).unwrap(), 1);
    assert!(store.library_item(item_id).unwrap().trashed_at.is_none());
    let restored_parent = store.item_parent(item_id).unwrap();
    assert_eq!(store.collection_path_label(restored_parent), "Papers/Topic");

    let _ = fs::remove_file(document_path);
}

#[cfg(unix)]
#[test]
fn history_path_keys_do_not_collide_for_non_utf8_paths() {
    use std::os::unix::ffi::OsStringExt as _;

    let database_path = temp_library_path("history-key-collision");
    let raw_path = PathBuf::from(std::ffi::OsString::from_vec(vec![
        b'/', b't', b'm', b'p', b'/', 0xff, b'.', b'p', b'd', b'f',
    ]));
    let replacement_path = PathBuf::from("/tmp/\u{fffd}.pdf");
    assert_ne!(history_key(&raw_path), history_key(&replacement_path));

    {
        let mut store = LibraryStore::open_for_test(database_path.clone()).unwrap();
        store.record_history(&raw_path, position(1));
        store.record_history(&replacement_path, position(2));
        store.save().unwrap();
    }

    {
        let store = LibraryStore::open_for_test(database_path.clone()).unwrap();
        assert_eq!(store.history_entries().len(), 2);
        assert_eq!(store.history_position(&raw_path), Some(position(1)));
        assert_eq!(store.history_position(&replacement_path), Some(position(2)));
    }

    let _ = fs::remove_file(database_path);
}
