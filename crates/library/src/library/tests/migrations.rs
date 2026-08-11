use super::*;
use redb::ReadableTableMetadata as _;

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
        let mut store = LibraryStore::open(database_path.clone()).unwrap();
        store.record_history(&raw_path, position(1));
        store.record_history(&replacement_path, position(2));
        store.save().unwrap();
    }

    {
        let store = LibraryStore::open(database_path.clone()).unwrap();
        assert_eq!(store.history_entries().len(), 2);
        assert_eq!(store.history_position(&raw_path), Some(position(1)));
        assert_eq!(store.history_position(&replacement_path), Some(position(2)));
    }

    let _ = fs::remove_file(database_path);
}

#[test]
fn migrates_legacy_storage_without_replacing_legacy_rows() {
    let database_path = temp_library_path("legacy-migration");
    let document_path = PathBuf::from("/tmp/legacy.pdf");
    let rect = HighlightRect {
        page_idx: 0,
        x0: 1.0,
        y0: 2.0,
        x1: 3.0,
        y1: 4.0,
    };
    let history = HistoryEntry {
        path: document_path.clone(),
        opened: 1,
        position: position(1),
    };
    let bookmark = Bookmark {
        id: 1,
        path: document_path.clone(),
        label: "legacy bookmark".to_string(),
        position: position(2),
        created: 2,
    };
    let highlight = Highlight {
        id: 1,
        path: document_path.clone(),
        color: HighlightColor::Yellow,
        rects: vec![rect],
        semantic_range: None,
        text: "legacy highlight".to_string(),
        note: String::new(),
        created: 3,
    };
    let note = Note {
        id: 1,
        path: document_path.clone(),
        color: HighlightColor::Blue,
        rects: vec![rect],
        semantic_range: None,
        text: "legacy note".to_string(),
        body: "legacy body".to_string(),
        created: 4,
    };
    let session = AiSession {
        id: 1,
        path: document_path.clone(),
        title: "legacy session".to_string(),
        page: 1,
        messages: vec![AiSessionMessage {
            role: AiSessionRole::User,
            content: "legacy message".to_string(),
            request_content: None,
            references: Vec::new(),
            occurrences: Vec::new(),
            images: Vec::new(),
            agent_parts: Vec::new(),
        }],
        queued_messages: Vec::new(),
        created: 5,
        updated: 6,
    };
    let widths = SidebarWidths {
        thumbnails: 1.0,
        toc: 2.0,
        notes: 3.0,
        ai: 4.0,
    };

    {
        let database = Database::create(&database_path).unwrap();
        ensure_tables(&database).unwrap();
        let write_txn = database.begin_write().unwrap();
        {
            let mut table = write_txn.open_table(HISTORY_TABLE).unwrap();
            table
                .insert(
                    document_path.to_str().unwrap(),
                    encode(&LegacyHistoryEntry {
                        path: history.path.clone(),
                        opened: history.opened,
                        position: StoredDocumentPositionV1::from(&history.position),
                    })
                    .unwrap()
                    .as_slice(),
                )
                .unwrap();
        }
        {
            let mut table = write_txn.open_table(BOOKMARKS_TABLE).unwrap();
            table
                .insert(
                    bookmark.id,
                    encode(&LegacyBookmark {
                        id: bookmark.id,
                        path: bookmark.path.clone(),
                        label: bookmark.label.clone(),
                        position: StoredDocumentPositionV1::from(&bookmark.position),
                        created: bookmark.created,
                    })
                    .unwrap()
                    .as_slice(),
                )
                .unwrap();
        }
        {
            let mut table = write_txn.open_table(HIGHLIGHTS_TABLE).unwrap();
            table
                .insert(
                    highlight.id,
                    encode(&LegacyHighlight {
                        id: highlight.id,
                        path: highlight.path.clone(),
                        color: highlight.color,
                        rects: highlight.rects.clone(),
                        text: highlight.text.clone(),
                        note: highlight.note.clone(),
                        created: highlight.created,
                    })
                    .unwrap()
                    .as_slice(),
                )
                .unwrap();
        }
        {
            let mut table = write_txn.open_table(NOTES_TABLE).unwrap();
            table
                .insert(
                    note.id,
                    encode(&LegacyNote {
                        id: note.id,
                        path: note.path.clone(),
                        color: note.color,
                        rects: note.rects.clone(),
                        text: note.text.clone(),
                        body: note.body.clone(),
                        created: note.created,
                    })
                    .unwrap()
                    .as_slice(),
                )
                .unwrap();
        }
        {
            let mut table = write_txn.open_table(AI_SESSIONS_TABLE).unwrap();
            table
                .insert(
                    session.id,
                    encode(&LegacyAiSession {
                        id: session.id,
                        path: session.path.clone(),
                        title: session.title.clone(),
                        page: session.page,
                        messages: session
                            .messages
                            .iter()
                            .map(|message| StoredAiSessionMessageV1 {
                                role: message.role,
                                content: message.content.clone(),
                                request_content: message.request_content.clone(),
                            })
                            .collect(),
                        created: session.created,
                        updated: session.updated,
                    })
                    .unwrap()
                    .as_slice(),
                )
                .unwrap();
        }
        {
            let mut table = write_txn.open_table(UI_STATE_TABLE).unwrap();
            table
                .insert(SIDEBAR_WIDTHS_KEY, encode(&widths).unwrap().as_slice())
                .unwrap();
        }
        {
            let stale_catalog_item = LibraryItem {
                id: DocumentId(1),
                kind: LibraryItemKind::Paper,
                title: "Legacy linked item".to_string(),
                creators: Vec::new(),
                year: None,
                publication: String::new(),
                publisher: String::new(),
                abstract_text: String::new(),
                subject: String::new(),
                language: String::new(),
                identifiers: LibraryIdentifiers::default(),
                embedded_keywords: Vec::new(),
                favorite: false,
                status: ReadingStatus::ToRead,
                work_note: String::new(),
                created: 1,
                updated: 1,
                trashed_at: None,
            };
            let mut table = write_txn.open_table(LIBRARY_ITEMS_V1_TABLE).unwrap();
            table
                .insert(
                    stale_catalog_item.id.0,
                    encode(&StoredLibraryItemV1::new(&stale_catalog_item))
                        .unwrap()
                        .as_slice(),
                )
                .unwrap();
        }
        write_txn.commit().unwrap();
    }

    {
        let store = LibraryStore::open(database_path.clone()).unwrap();
        assert_eq!(store.history_entries(), std::slice::from_ref(&history));
        assert_eq!(store.bookmarks(), std::slice::from_ref(&bookmark));
        assert_eq!(
            store.highlights_for(&document_path),
            vec![highlight.clone()]
        );
        assert_eq!(store.notes_for(&document_path), vec![note.clone()]);
        assert_eq!(store.ai_sessions_for(&document_path), vec![session.clone()]);
        assert_eq!(store.sidebar_widths(), Some(widths));
        assert!(store.library_items().is_empty());
    }

    {
        let database = Database::open(&database_path).unwrap();
        let write_txn = database.begin_write().unwrap();
        {
            let mut table = write_txn.open_table(HISTORY_TABLE).unwrap();
            let replacement_history = LegacyHistoryEntry {
                path: history.path.clone(),
                opened: history.opened,
                position: StoredDocumentPositionV1::from(&position(99)),
            };
            table
                .insert(
                    document_path.to_str().unwrap(),
                    encode(&replacement_history).unwrap().as_slice(),
                )
                .unwrap();
        }
        write_txn.commit().unwrap();
    }

    {
        let store = LibraryStore::open(database_path.clone()).unwrap();
        assert_eq!(store.history_entries(), &[history]);
    }

    {
        let database = Database::open(&database_path).unwrap();
        let read_txn = database.begin_read().unwrap();
        let legacy_history = read_txn
            .open_table(HISTORY_TABLE)
            .unwrap()
            .get(document_path.to_str().unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(
            decode::<LegacyHistoryEntry>(legacy_history.value())
                .unwrap()
                .position,
            StoredDocumentPositionV1::from(&position(99))
        );
        assert!(
            read_txn
                .open_table(STORAGE_MIGRATIONS_TABLE)
                .unwrap()
                .get(STORAGE_V1_MIGRATION_KEY)
                .unwrap()
                .is_some()
        );
        assert_eq!(
            read_txn
                .open_table(LIBRARY_ITEMS_V1_TABLE)
                .unwrap()
                .len()
                .unwrap(),
            0
        );
        assert!(
            read_txn
                .open_table(STORAGE_MIGRATIONS_TABLE)
                .unwrap()
                .get(MANAGED_LIBRARY_MIGRATION_KEY)
                .unwrap()
                .is_some()
        );
    }

    let _ = fs::remove_file(database_path);
}
