use super::storage_helpers::*;
use super::*;

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
        let mut store = LibraryStore::open(database_path.clone()).unwrap();
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

    let store = LibraryStore::open(database_path.clone()).unwrap();
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
        let mut store = LibraryStore::open(database_path.clone()).unwrap();
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
        let mut store = LibraryStore::open(database_path.clone()).unwrap();
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

    let reopened = LibraryStore::open(database_path.clone()).unwrap();
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
        let mut store = LibraryStore::open(path.clone()).unwrap();
        store.record_history(document_path, position(4));
        store.save().unwrap();
        bookmark_id = store
            .add_bookmark(document_path, "Chapter 1", position(5))
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
                path: document_path.to_path_buf(),
                title: "Explain section".to_string(),
                page: 4,
                messages: vec![
                    AiSessionMessage {
                        role: AiSessionRole::User,
                        content: "Explain @selection".to_string(),
                        request_content: Some("Explain @selection\n\nAttached context".to_string()),
                        references: Vec::new(),
                        occurrences: Vec::new(),
                    },
                    AiSessionMessage {
                        role: AiSessionRole::Assistant,
                        content: "Explanation".to_string(),
                        request_content: None,
                        references: Vec::new(),
                        occurrences: Vec::new(),
                    },
                ],
                created: 100,
                updated: 101,
            })
            .unwrap();
    }

    {
        let mut store = LibraryStore::open(path.clone()).unwrap();
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
        let ai_sessions = store.ai_sessions_for(document_path);
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
        let store = LibraryStore::open(path.clone()).unwrap();
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
    let mut store = LibraryStore::open(database_path.clone()).unwrap();

    let bookmark_id = store
        .add_bookmark(document_path, "Chapter", position(1))
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
    let mut store = LibraryStore::open(database_path.clone()).unwrap();
    let bookmark_id = store
        .add_bookmark(document_path, "Bookmark", position(1))
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
    let mut store = LibraryStore::open(database_path.clone()).unwrap();
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
    let mut store = LibraryStore::open(database_path.clone()).unwrap();

    let bookmark_id = store
        .add_bookmark(document_b, "Bookmark", position(1))
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
fn per_document_history_drops_oldest_edits() {
    let database_path = temp_library_path("document-edit-capacity");
    let document_path = Path::new("/tmp/document-edit-capacity.pdf");
    let mut store = LibraryStore::open(database_path.clone()).unwrap();

    for index in 0..MAX_DOCUMENT_EDIT_HISTORY_ENTRIES + 1 {
        store
            .add_bookmark(document_path, format!("Bookmark {index}"), position(index))
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
    let mut store = LibraryStore::open(database_path.clone()).unwrap();
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
        startup_error: Some("database startup failed".to_string()),
        document_edit_histories: HashMap::new(),
        next_document_edit_sequence: 0,
        bookmarks_revision: 0,
        history_entry_revisions: Arc::new(Mutex::new(HashMap::new())),
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
    let mut store = LibraryStore::open(database_path.clone()).unwrap();
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
        let mut store = LibraryStore::open(database_path.clone()).unwrap();
        for index in 0..=MAX_DOCUMENT_EDIT_HISTORIES {
            let path = PathBuf::from(format!("/tmp/bounded-document-{index}.pdf"));
            store
                .add_bookmark(&path, format!("Bookmark {index}"), position(index))
                .unwrap();
        }
        assert!(!store.can_undo_document(&oldest_path));
        assert!(store.can_undo_document(Path::new(&format!(
            "/tmp/bounded-document-{}.pdf",
            MAX_DOCUMENT_EDIT_HISTORIES
        ))));
    }

    let store = LibraryStore::open(database_path.clone()).unwrap();
    assert!(!store.can_undo_document(Path::new("/tmp/bounded-document-32.pdf")));
    assert_eq!(store.bookmarks().len(), MAX_DOCUMENT_EDIT_HISTORIES + 1);

    let _ = fs::remove_file(database_path);
}

#[test]
fn loads_v1_ai_sessions_and_migrates_them_without_references() {
    let database_path = temp_library_path("ai-session-v1-compatibility");
    let document_path = PathBuf::from("/tmp/v1-session.pdf");
    let stored_session = StoredAiSessionV1 {
        version: STORAGE_SCHEMA_VERSION,
        id: 7,
        path: StoredPathV1::from_path(&document_path),
        title: "V1 session".to_string(),
        page: 3,
        messages: vec![StoredAiSessionMessageV1 {
            role: AiSessionRole::User,
            content: "Explain @current page text".to_string(),
            request_content: Some("Explain the attached page text".to_string()),
        }],
        created: 10,
        updated: 11,
    };

    {
        let database = Database::create(&database_path).unwrap();
        ensure_tables(&database).unwrap();
        let write_txn = database.begin_write().unwrap();
        {
            let mut table = write_txn.open_table(AI_SESSIONS_V1_TABLE).unwrap();
            table
                .insert(
                    stored_session.id,
                    encode(&stored_session).unwrap().as_slice(),
                )
                .unwrap();
        }
        {
            let mut migrations = write_txn.open_table(STORAGE_MIGRATIONS_TABLE).unwrap();
            migrations
                .insert(
                    STORAGE_V1_MIGRATION_KEY,
                    [STORAGE_SCHEMA_VERSION].as_slice(),
                )
                .unwrap();
            migrations
                .insert(STORAGE_V2_MIGRATION_KEY, [STORAGE_V2_VERSION].as_slice())
                .unwrap();
        }
        write_txn.commit().unwrap();
    }

    {
        let store = LibraryStore::open(database_path.clone()).unwrap();
        let sessions = store.ai_sessions_for(&document_path);
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, stored_session.id);
        assert_eq!(sessions[0].messages.len(), 1);
        assert!(sessions[0].messages[0].references.is_empty());
        assert!(sessions[0].messages[0].occurrences.is_empty());
    }

    {
        let database = Database::open(&database_path).unwrap();
        let read_txn = database.begin_read().unwrap();
        assert!(
            read_txn
                .open_table(STORAGE_MIGRATIONS_TABLE)
                .unwrap()
                .get(AI_SESSIONS_V2_MIGRATION_KEY)
                .unwrap()
                .is_some()
        );
        assert!(
            read_txn
                .open_table(STORAGE_MIGRATIONS_TABLE)
                .unwrap()
                .get(AI_SESSIONS_V3_MIGRATION_KEY)
                .unwrap()
                .is_some()
        );
        assert!(
            read_txn
                .open_table(STORAGE_MIGRATIONS_TABLE)
                .unwrap()
                .get(AI_SESSIONS_V4_MIGRATION_KEY)
                .unwrap()
                .is_some()
        );
        let v1_value = read_txn
            .open_table(AI_SESSIONS_V1_TABLE)
            .unwrap()
            .get(stored_session.id)
            .unwrap()
            .unwrap();
        assert_eq!(
            decode::<StoredAiSessionV1>(v1_value.value())
                .unwrap()
                .messages
                .len(),
            1
        );
        let v2_value = read_txn
            .open_table(AI_SESSIONS_V2_TABLE)
            .unwrap()
            .get(stored_session.id)
            .unwrap()
            .unwrap();
        assert!(
            decode::<StoredAiSessionV2>(v2_value.value())
                .unwrap()
                .messages[0]
                .references
                .is_empty()
        );
        let v3_value = read_txn
            .open_table(AI_SESSIONS_V3_TABLE)
            .unwrap()
            .get(stored_session.id)
            .unwrap()
            .unwrap();
        assert!(
            decode::<StoredAiSessionV3>(v3_value.value())
                .unwrap()
                .messages[0]
                .occurrences
                .is_empty()
        );
        let v4_value = read_txn
            .open_table(AI_SESSIONS_V4_TABLE)
            .unwrap()
            .get(stored_session.id)
            .unwrap()
            .unwrap();
        assert!(
            decode::<StoredAiSessionV4>(v4_value.value())
                .unwrap()
                .messages[0]
                .occurrences
                .is_empty()
        );
    }

    {
        let store = LibraryStore::open(database_path.clone()).unwrap();
        assert_eq!(store.ai_sessions_for(&document_path).len(), 1);
    }

    let _ = fs::remove_file(database_path);
}

#[test]
fn loads_genuinely_v2_encoded_ai_session_after_domain_occurrences_are_added() {
    #[derive(Serialize)]
    struct HistoricalAiSessionMessageV2 {
        role: AiSessionRole,
        content: String,
        request_content: Option<String>,
        references: Vec<AiMentionReference>,
    }

    #[derive(Serialize)]
    struct HistoricalAiSessionV2 {
        version: u8,
        id: u64,
        path: StoredPathV1,
        title: String,
        page: usize,
        messages: Vec<HistoricalAiSessionMessageV2>,
        created: u64,
        updated: u64,
    }

    let database_path = temp_library_path("ai-session-v2-compatibility");
    let document_path = PathBuf::from("/tmp/genuine-v2-session.pdf");
    let stored_session = HistoricalAiSessionV2 {
        version: STORAGE_V2_VERSION,
        id: 8,
        path: StoredPathV1::from_path(&document_path),
        title: "V2 session".to_string(),
        page: 4,
        messages: vec![HistoricalAiSessionMessageV2 {
            role: AiSessionRole::User,
            content: "Explain @selection twice".to_string(),
            request_content: Some("Explain the attached selections".to_string()),
            references: vec![AiMentionReference::Selection, AiMentionReference::Selection],
        }],
        created: 12,
        updated: 13,
    };

    {
        let database = Database::create(&database_path).unwrap();
        ensure_tables(&database).unwrap();
        let write_txn = database.begin_write().unwrap();
        {
            let mut table = write_txn.open_table(AI_SESSIONS_V2_TABLE).unwrap();
            table
                .insert(
                    stored_session.id,
                    encode(&stored_session).unwrap().as_slice(),
                )
                .unwrap();
        }
        {
            let mut migrations = write_txn.open_table(STORAGE_MIGRATIONS_TABLE).unwrap();
            migrations
                .insert(
                    STORAGE_V1_MIGRATION_KEY,
                    [STORAGE_SCHEMA_VERSION].as_slice(),
                )
                .unwrap();
            migrations
                .insert(STORAGE_V2_MIGRATION_KEY, [STORAGE_V2_VERSION].as_slice())
                .unwrap();
            migrations
                .insert(
                    AI_SESSIONS_V2_MIGRATION_KEY,
                    [STORAGE_V2_VERSION].as_slice(),
                )
                .unwrap();
        }
        write_txn.commit().unwrap();
    }

    {
        let store = LibraryStore::open(database_path.clone()).unwrap();
        let sessions = store.ai_sessions_for(&document_path);
        assert_eq!(sessions.len(), 1);
        assert_eq!(
            sessions[0].messages[0].references,
            vec![AiMentionReference::Selection, AiMentionReference::Selection]
        );
        assert!(sessions[0].messages[0].occurrences.is_empty());
    }

    {
        let database = Database::open(&database_path).unwrap();
        let read_txn = database.begin_read().unwrap();
        assert!(
            read_txn
                .open_table(STORAGE_MIGRATIONS_TABLE)
                .unwrap()
                .get(AI_SESSIONS_V3_MIGRATION_KEY)
                .unwrap()
                .is_some()
        );
        let v3_value = read_txn
            .open_table(AI_SESSIONS_V3_TABLE)
            .unwrap()
            .get(stored_session.id)
            .unwrap()
            .unwrap();
        let migrated = decode::<StoredAiSessionV3>(v3_value.value()).unwrap();
        assert_eq!(migrated.messages[0].references.len(), 2);
        assert!(migrated.messages[0].occurrences.is_empty());
    }

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
        let mut store = LibraryStore::open(database_path.clone()).unwrap();
        let session_id = store.next_ai_session_id();
        store
            .save_ai_session(AiSession {
                id: session_id,
                path: document_path.clone(),
                title: "Referenced context".to_string(),
                page: 5,
                messages: vec![AiSessionMessage {
                    role: AiSessionRole::User,
                    content: "Compare the references".to_string(),
                    request_content: Some("Compare the attached context".to_string()),
                    references: references.clone(),
                    occurrences: Vec::new(),
                }],
                created: 20,
                updated: 21,
            })
            .unwrap();
    }

    {
        let store = LibraryStore::open(database_path.clone()).unwrap();
        let sessions = store.ai_sessions_for(&document_path);
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].messages[0].references, references);
    }

    let _ = fs::remove_file(database_path);
}

#[test]
fn round_trips_v3_occurrences_with_duplicate_references() {
    let database_path = temp_library_path("ai-session-v3-duplicate-occurrences");
    let document_path = PathBuf::from("/tmp/v3-duplicate-occurrences.pdf");
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
        let mut store = LibraryStore::open(database_path.clone()).unwrap();
        let session_id = store.next_ai_session_id();
        store
            .save_ai_session(AiSession {
                id: session_id,
                path: document_path.clone(),
                title: "Duplicate mentions".to_string(),
                page: 1,
                messages: vec![AiSessionMessage {
                    role: AiSessionRole::User,
                    content: "@selection and @selection".to_string(),
                    request_content: Some("Compare the attached selections".to_string()),
                    references: vec![reference.clone(), reference],
                    occurrences: occurrences.clone(),
                }],
                created: 30,
                updated: 31,
            })
            .unwrap();
    }

    {
        let store = LibraryStore::open(database_path.clone()).unwrap();
        let sessions = store.ai_sessions_for(&document_path);
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].messages[0].occurrences, occurrences);
        assert_eq!(sessions[0].messages[0].references.len(), 2);
    }

    let _ = fs::remove_file(database_path);
}

#[test]
fn round_trips_v4_selected_page_snapshots() {
    let database_path = temp_library_path("ai-session-v4-selected-pages");
    let document_path = PathBuf::from("/tmp/v4-selected-pages.pdf");
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
        let mut store = LibraryStore::open(database_path.clone()).unwrap();
        let session_id = store.next_ai_session_id();
        store
            .save_ai_session(AiSession {
                id: session_id,
                path: document_path.clone(),
                title: "Selected pages".to_string(),
                page: 1,
                messages: vec![AiSessionMessage {
                    role: AiSessionRole::User,
                    content: "@selected-pages-text and @selected-pages-image".to_string(),
                    request_content: Some("Compare the attached pages".to_string()),
                    references: Vec::new(),
                    occurrences: occurrences.clone(),
                }],
                created: 35,
                updated: 36,
            })
            .unwrap();
        session_id
    };

    {
        let store = LibraryStore::open(database_path.clone()).unwrap();
        let sessions = store.ai_sessions_for(&document_path);
        assert_eq!(sessions[0].messages[0].occurrences, occurrences);
    }

    {
        let database = Database::open(&database_path).unwrap();
        let read_txn = database.begin_read().unwrap();
        let stored = read_txn
            .open_table(AI_SESSIONS_V4_TABLE)
            .unwrap()
            .get(session_id)
            .unwrap()
            .unwrap();
        let stored = decode::<StoredAiSessionV4>(stored.value()).unwrap();
        assert_eq!(stored.version, AI_SESSIONS_V4_VERSION);
        assert_eq!(stored.messages[0].occurrences, occurrences);
    }

    let _ = fs::remove_file(database_path);
}

#[test]
fn round_trips_v3_region_occurrence_geometry() {
    let database_path = temp_library_path("ai-session-v3-region-geometry");
    let document_path = PathBuf::from("/tmp/v3-region-geometry.pdf");
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
        let mut store = LibraryStore::open(database_path.clone()).unwrap();
        let session_id = store.next_ai_session_id();
        store
            .save_ai_session(AiSession {
                id: session_id,
                path: document_path.clone(),
                title: "Region geometry".to_string(),
                page: 3,
                messages: vec![AiSessionMessage {
                    role: AiSessionRole::User,
                    content: "Describe @region-image".to_string(),
                    request_content: Some("Describe the attached image region".to_string()),
                    references: vec![AiMentionReference::RegionImage {
                        reference: region_reference.clone(),
                    }],
                    occurrences: vec![occurrence.clone()],
                }],
                created: 40,
                updated: 41,
            })
            .unwrap();
    }

    {
        let store = LibraryStore::open(database_path.clone()).unwrap();
        let sessions = store.ai_sessions_for(&document_path);
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
fn persists_sidebar_widths() {
    let path = temp_library_path("sidebar-widths");
    let widths = SidebarWidths {
        thumbnails: 222.0,
        toc: 333.0,
        notes: 444.0,
        ai: 555.0,
    };

    {
        let mut store = LibraryStore::open(path.clone()).unwrap();
        store.set_sidebar_widths(widths).unwrap();
    }

    {
        let store = LibraryStore::open(path.clone()).unwrap();
        assert_eq!(store.sidebar_widths(), Some(widths));
    }

    let _ = fs::remove_file(path);
}

#[test]
fn persists_document_browser_size() {
    let path = temp_library_path("document-browser-size");
    let size = DocumentBrowserSize {
        width: 864.0,
        height: 576.0,
    };

    {
        let mut store = LibraryStore::open(path.clone()).unwrap();
        store.set_document_browser_size(size).unwrap();
    }

    {
        let store = LibraryStore::open(path.clone()).unwrap();
        assert_eq!(store.document_browser_size(), Some(size));
    }

    let _ = fs::remove_file(path);
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
                            kind: ReaderTabKind::NewTab,
                        }],
                        active_tab: Some(3),
                    }),
                ],
            },
            active_pane: 1,
            left_sidebar: Some(ReaderSidebar::Notes),
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
        let mut store = LibraryStore::open(database_path.clone()).unwrap();
        store.set_reader_session(session.clone()).unwrap();
    }

    let store = LibraryStore::open(database_path.clone()).unwrap();
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
        ensure_tables(&database).unwrap();
        let write_txn = database.begin_write().unwrap();
        {
            let mut table = write_txn.open_table(UI_STATE_V1_TABLE).unwrap();
            table
                .insert(READER_SESSION_KEY, encode(&stored).unwrap().as_slice())
                .unwrap();
        }
        {
            let mut migrations = write_txn.open_table(STORAGE_MIGRATIONS_TABLE).unwrap();
            migrations
                .insert(
                    STORAGE_V1_MIGRATION_KEY,
                    [STORAGE_SCHEMA_VERSION].as_slice(),
                )
                .unwrap();
            migrations
                .insert(STORAGE_V2_MIGRATION_KEY, [STORAGE_V2_VERSION].as_slice())
                .unwrap();
        }
        write_txn.commit().unwrap();
    }

    let store = LibraryStore::open(database_path.clone()).unwrap();
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
        let mut store = LibraryStore::open(database_path.clone()).unwrap();
        store
            .set_document_target_language(document_path, " Zh-Hans ")
            .unwrap();
        assert_eq!(
            store.document_target_language(document_path).as_deref(),
            Some("zh-hans")
        );
    }

    {
        let store = LibraryStore::open(database_path.clone()).unwrap();
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
        startup_error: Some("database startup failed".to_string()),
        document_edit_histories: HashMap::new(),
        next_document_edit_sequence: 0,
        bookmarks_revision: 0,
        history_entry_revisions: Arc::new(Mutex::new(HashMap::new())),
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
        startup_error: Some("database startup failed".to_string()),
        document_edit_histories: HashMap::new(),
        next_document_edit_sequence: 0,
        bookmarks_revision: 0,
        history_entry_revisions: Arc::new(Mutex::new(HashMap::new())),
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
        let mut store = LibraryStore::open(database_path.clone()).unwrap();
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
        let store = LibraryStore::open(database_path.clone()).unwrap();
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
        let mut store = LibraryStore::open(database_path.clone()).unwrap();
        store
            .set_document_rotation(document_path, DocumentRotation::Degrees90)
            .unwrap();
        store
            .set_epub_reader_preferences(document_path, preferences)
            .unwrap();
        assert_eq!(
            store.document_rotation(document_path),
            DocumentRotation::Degrees90
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

        store.clear_epub_reader_preferences(document_path).unwrap();
        assert_eq!(
            store.document_target_language(document_path).as_deref(),
            Some("ja")
        );
        assert_eq!(
            store.document_rotation(document_path),
            DocumentRotation::Degrees90
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
        let store = LibraryStore::open(database_path.clone()).unwrap();
        assert_eq!(
            store.document_target_language(document_path).as_deref(),
            Some("ja")
        );
        assert_eq!(
            store.document_rotation(document_path),
            DocumentRotation::Degrees90
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
        let mut store = LibraryStore::open(database_path.clone()).unwrap();
        store
            .set_document_rotation(document_path, DocumentRotation::Degrees270)
            .unwrap();
        assert_eq!(
            store.document_rotation(document_path),
            DocumentRotation::Degrees270
        );
    }

    {
        let store = LibraryStore::open(database_path.clone()).unwrap();
        assert_eq!(
            store.document_rotation(document_path),
            DocumentRotation::Degrees270
        );
    }

    let _ = fs::remove_file(database_path);
}

#[test]
fn migrates_v2_document_preferences_with_default_rotation() {
    let database_path = temp_library_path("document-preferences-v2-migration");
    let document_path = PathBuf::from("/tmp/v2-preferences.epub");
    let epub = EpubReaderPreferences {
        font_size: 19.0,
        line_height: 1.7,
        horizontal_margin: 40.0,
        max_content_width: 680.0,
        theme: glyphst_core::EpubTheme::Sepia,
        use_publisher_styles: false,
    };
    let stored_preferences = StoredDocumentPreferencesV2 {
        version: STORAGE_V2_VERSION,
        path: StoredPathV1::from_path(&document_path),
        target_language: "ja".to_string(),
        epub: Some(epub),
    };

    {
        let database = Database::create(&database_path).unwrap();
        ensure_tables(&database).unwrap();
        let write_txn = database.begin_write().unwrap();
        {
            let mut table = write_txn.open_table(DOCUMENT_PREFERENCES_V2_TABLE).unwrap();
            let key = history_key(&document_path);
            table
                .insert(
                    key.as_slice(),
                    encode(&stored_preferences).unwrap().as_slice(),
                )
                .unwrap();
        }
        {
            let mut migrations = write_txn.open_table(STORAGE_MIGRATIONS_TABLE).unwrap();
            migrations
                .insert(
                    STORAGE_V1_MIGRATION_KEY,
                    [STORAGE_SCHEMA_VERSION].as_slice(),
                )
                .unwrap();
            migrations
                .insert(STORAGE_V2_MIGRATION_KEY, [STORAGE_V2_VERSION].as_slice())
                .unwrap();
        }
        write_txn.commit().unwrap();
    }

    {
        let store = LibraryStore::open(database_path.clone()).unwrap();
        assert_eq!(
            store.document_target_language(&document_path).as_deref(),
            Some("ja")
        );
        assert_eq!(store.epub_reader_preferences(&document_path), Some(epub));
        assert_eq!(
            store.document_rotation(&document_path),
            DocumentRotation::default()
        );
    }

    {
        let database = Database::open(&database_path).unwrap();
        let read_txn = database.begin_read().unwrap();
        assert!(
            read_txn
                .open_table(STORAGE_MIGRATIONS_TABLE)
                .unwrap()
                .get(DOCUMENT_PREFERENCES_V3_MIGRATION_KEY)
                .unwrap()
                .is_some()
        );
        let key = history_key(&document_path);
        let value = read_txn
            .open_table(DOCUMENT_PREFERENCES_V3_TABLE)
            .unwrap()
            .get(key.as_slice())
            .unwrap()
            .unwrap();
        let migrated = decode::<StoredDocumentPreferencesV3>(value.value()).unwrap();
        assert_eq!(migrated.target_language, "ja");
        assert_eq!(migrated.epub, Some(epub));
        assert_eq!(migrated.rotation, DocumentRotation::default());
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
        let mut store = LibraryStore::open(database_path.clone()).unwrap();
        store
            .set_epub_reader_preferences(document_path, preferences)
            .unwrap();
        assert_eq!(store.document_target_language(document_path), None);
    }

    {
        let mut store = LibraryStore::open(database_path.clone()).unwrap();
        assert_eq!(store.document_target_language(document_path), None);
        assert_eq!(
            store.epub_reader_preferences(document_path),
            Some(preferences)
        );
        store.clear_epub_reader_preferences(document_path).unwrap();
        assert_eq!(store.library.document_preferences.len(), 0);
    }

    {
        let store = LibraryStore::open(database_path.clone()).unwrap();
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
        let mut store = LibraryStore::open(database_path.clone()).unwrap();
        store.record_history(&document_path, position.clone());
        store
            .add_bookmark(&document_path, "Chapter", position.clone())
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
        let store = LibraryStore::open(database_path.clone()).unwrap();
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
        decode::<StoredHistoryEntryV1>(&encode(&StoredHistoryEntryV1::from(&entry)).unwrap())
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
    };
    assert_eq!(
        decode::<StoredDocumentPreferencesV1>(
            &encode(&StoredDocumentPreferencesV1::from(&preferences)).unwrap()
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
        let mut store = LibraryStore::open(database_path.clone()).unwrap();
        store.record_history(&document_path, position(1));
        store.save().unwrap();
        store
            .add_bookmark(&document_path, "bookmark", position(2))
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
                path: document_path.clone(),
                title: "session".to_string(),
                page: 1,
                messages: vec![AiSessionMessage {
                    role: AiSessionRole::User,
                    content: "message".to_string(),
                    request_content: None,
                    references: Vec::new(),
                    occurrences: Vec::new(),
                }],
                created: 1,
                updated: 1,
            })
            .unwrap();
    }

    {
        let store = LibraryStore::open(database_path.clone()).unwrap();
        let normalized = normalize_path(&document_path);
        assert_eq!(store.history_entries()[0].path, normalized);
        assert_eq!(store.bookmarks()[0].path, normalized);
        assert_eq!(store.highlights_for(&document_path)[0].path, normalized);
        assert_eq!(store.notes_for(&document_path)[0].path, normalized);
        assert_eq!(store.ai_sessions_for(&document_path)[0].path, normalized);
    }

    let _ = fs::remove_file(database_path);
}

#[cfg(unix)]
mod migrations;
