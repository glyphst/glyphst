use super::*;

/// A Zotero data folder with a small library: a paper in two collections
/// (one nested) with tags (one automatic), a stored PDF with a highlight,
/// a child note, a trashed book, a feed item and a standalone note.
fn fixture(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("glyphst-zotero-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("storage/ATTACH01")).unwrap();
    fs::write(
        dir.join("storage/ATTACH01/attention.pdf"),
        b"%PDF-1.4 zotero",
    )
    .unwrap();
    let connection = Connection::open(dir.join("zotero.sqlite")).unwrap();
    connection
        .execute_batch("PRAGMA journal_mode = WAL;")
        .unwrap();
    connection.execute_batch(sample::SCHEMA).unwrap();
    connection
        .execute_batch(
            r##"
            INSERT INTO libraries VALUES (1, 'user'), (2, 'feed');
            INSERT INTO itemTypes VALUES (1, 'preprint'), (2, 'book'), (3, 'attachment'), (4, 'note'), (5, 'annotation'), (6, 'journalArticle');
            INSERT INTO items VALUES
                (1, 1, 1, 'PAPER001'), (2, 3, 1, 'ATTACH01'), (3, 4, 1, 'NOTE0001'),
                (4, 5, 1, 'ANNOT001'), (5, 2, 1, 'BOOK0001'), (6, 6, 2, 'FEED0001'),
                (7, 4, 1, 'NOTE0002');
            INSERT INTO fields VALUES (1, 'title'), (2, 'date'), (3, 'archiveID'), (4, 'DOI'), (5, 'extra'), (6, 'abstractNote');
            INSERT INTO itemDataValues VALUES
                (1, 'Attention Is All You Need'), (2, '2017-06-12 2017-06-12'), (3, 'arXiv:1706.03762'),
                (4, '10.48550/arXiv.1706.03762'), (5, 'Citation Key: vaswani2017'), (6, 'The dominant models…'),
                (7, 'A Trashed Book'), (8, 'A feed entry'), (9, 'attention.pdf');
            INSERT INTO itemData VALUES (1, 1, 1), (1, 2, 2), (1, 3, 3), (1, 4, 4), (1, 5, 5), (1, 6, 6), (5, 1, 7), (6, 1, 8), (2, 1, 9);
            INSERT INTO creators VALUES (1, 'Ashish', 'Vaswani', 0), (2, 'Noam', 'Shazeer', 0);
            INSERT INTO creatorTypes VALUES (1, 'author');
            INSERT INTO itemCreators VALUES (1, 2, 1, 1), (1, 1, 1, 0);
            INSERT INTO tags VALUES (1, 'transformers'), (2, 'Computer Science - Computation and Language');
            INSERT INTO itemTags VALUES (1, 1, 0), (1, 2, 1);
            INSERT INTO collections VALUES (1, 'Machine learning', NULL, 1, 'COLL0001'), (2, 'LLMs', 1, 1, 'COLL0002'), (3, 'Reading', NULL, 1, 'COLL0003');
            INSERT INTO collectionItems VALUES (2, 1, 0), (3, 1, 0);
            INSERT INTO itemAttachments VALUES (2, 1, 0, 'application/pdf', 'storage:attention.pdf');
            INSERT INTO itemNotes VALUES (3, 1, '<p>Compare with <b>RoPE</b>.</p>', 'Compare with RoPE.'), (7, NULL, '<h1>Ideas</h1>', 'Ideas');
            INSERT INTO itemAnnotations VALUES (4, 2, 1, '', 'scaled dot-product', 'key idea', '#ffd400', '4', '00003|000100|00200', '{"pageIndex":3,"rects":[[100,600,300,612]]}', 0);
            INSERT INTO deletedItems VALUES (5, '2026-01-01');
            INSERT INTO relationPredicates VALUES (1, 'dc:relation');
            INSERT INTO itemRelations VALUES (1, 1, 'http://zotero.org/users/local/abc/items/BOOK0001');
            INSERT INTO syncedSettings VALUES ('tagColors', 1, '[{"name":"transformers","color":"#2EA8E5"}]');
            "##,
        )
        .unwrap();
    // Leave a change in the write-ahead log only, as a running Zotero would.
    connection
        .execute(
            "UPDATE itemDataValues SET value = 'Attention Is All You Need' WHERE valueID = 1",
            [],
        )
        .unwrap();
    connection
        .execute("INSERT INTO itemTags VALUES (1, 3, 0)", [])
        .unwrap();
    connection
        .execute("INSERT INTO tags VALUES (3, 'from the log')", [])
        .unwrap();
    std::mem::forget(connection);
    dir
}

#[test]
fn a_zotero_library_reads_with_its_collections_tags_notes_and_annotations() {
    let dir = fixture("read");
    let scratch = dir.join("scratch");
    let library = read_data_dir(&dir, &scratch, None).unwrap();

    let paper = library
        .items
        .iter()
        .find(|item| item.key == "PAPER001")
        .unwrap();
    assert_eq!(paper.item_type, "preprint");
    assert_eq!(paper.field("title"), Some("Attention Is All You Need"));
    assert_eq!(paper.year(), Some(2017));
    assert_eq!(paper.arxiv().as_deref(), Some("1706.03762"));
    assert_eq!(paper.citation_key().as_deref(), Some("vaswani2017"));
    assert_eq!(
        paper.creators.iter().map(Creator::name).collect::<Vec<_>>(),
        vec!["Ashish Vaswani", "Noam Shazeer"],
        "in Zotero's order"
    );
    assert!(paper.tags.contains(&("transformers".to_string(), false)));
    assert!(paper.tags.contains(&(
        "Computer Science - Computation and Language".to_string(),
        true
    )));
    assert!(
        paper.tags.contains(&("from the log".to_string(), false)),
        "changes still in the write-ahead log are read"
    );
    assert_eq!(
        paper
            .collections
            .iter()
            .map(|id| library.collection_path(*id).join("/"))
            .collect::<Vec<_>>(),
        vec!["Machine learning/LLMs", "Reading"]
    );
    assert_eq!(paper.attachments.len(), 1);
    let attachment = &paper.attachments[0];
    assert_eq!(attachment.link_mode, LinkMode::Stored);
    assert_eq!(
        attachment.file.as_deref(),
        Some(dir.join("storage/ATTACH01/attention.pdf").as_path())
    );
    let annotation = &attachment.annotations[0];
    assert_eq!(annotation.kind, AnnotationKind::Highlight);
    assert_eq!(annotation.comment, "key idea");
    assert_eq!(
        annotation.position,
        Some((3, vec![[100.0, 600.0, 300.0, 612.0]]))
    );
    assert_eq!(html_to_markdown(&paper.notes[0]), "Compare with **RoPE**.");
    assert_eq!(paper.related, vec!["BOOK0001".to_string()]);

    let book = library
        .items
        .iter()
        .find(|item| item.key == "BOOK0001")
        .unwrap();
    assert!(book.trashed);
    assert!(
        library.items.iter().all(|item| item.key != "FEED0001"),
        "feed items are not part of the library"
    );
    assert_eq!(library.standalone_notes.len(), 1);
    assert_eq!(
        library.tag_colors,
        vec![("transformers".to_string(), "#2EA8E5".to_string())]
    );
    assert_eq!(library.annotation_count(), 1);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn zotero_rects_flip_to_the_top_left() {
    // A 792pt page: a line near the top in PDF space is near the top here.
    assert_eq!(
        to_top_left([100.0, 600.0, 300.0, 612.0], 792.0, (0.0, 0.0)),
        [100.0, 180.0, 300.0, 192.0]
    );
    assert_eq!(
        to_top_left([110.0, 610.0, 310.0, 622.0], 792.0, (10.0, 10.0)),
        [100.0, 180.0, 300.0, 192.0]
    );
}

#[test]
fn a_moved_data_folder_is_found_in_the_profile() {
    let prefs = r#"user_pref("extensions.zotero.dataDir", "/home/me/Research/Zotero");
user_pref("extensions.zotero.useDataDir", true);"#;
    assert_eq!(
        pref_value(prefs, "extensions.zotero.dataDir").as_deref(),
        Some("/home/me/Research/Zotero")
    );
    assert_eq!(pref_value(prefs, "extensions.zotero.missing"), None);
}

#[test]
fn the_sample_library_reads_back() {
    let dir = std::env::temp_dir().join(format!("glyphst-zotero-sample-{}", std::process::id()));
    sample::write(&dir).unwrap();
    let library = read_data_dir(&dir, &dir.join("scratch"), None).unwrap();
    assert_eq!(library.items.len(), 6);
    assert_eq!(library.collections.len(), 5);
    assert_eq!(library.annotation_count(), 4);
    let missing = library
        .items
        .iter()
        .flat_map(|item| &item.attachments)
        .filter(|attachment| attachment.file.is_none())
        .map(|attachment| attachment.zotero_path.as_str())
        .collect::<Vec<_>>();
    assert_eq!(missing, vec!["storage/8XK2QF7A/rag.pdf"]);
    assert_eq!(library.standalone_notes.len(), 1);
    let _ = fs::remove_dir_all(dir);
}
