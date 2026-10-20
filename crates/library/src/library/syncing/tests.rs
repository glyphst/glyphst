use std::collections::BTreeMap;

use glyphst_sync::{DELETED, RecordKey};
use serde_json::{Value, json};

use super::*;

fn folder(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("glyphst-syncing-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn file(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(format!("{name}.pdf"));
    fs::write(&path, format!("%PDF-1.4 {name}")).unwrap();
    path
}

fn link(store: &mut LibraryStore, path: &Path) -> DocumentId {
    match store
        .link_item(path, LibraryItemMetadata::for_path(path), None)
        .unwrap()
    {
        LibraryImportOutcome::Added(id) => id,
        other => panic!("{other:?}"),
    }
}

fn tag_id(store: &LibraryStore, name: &str) -> LibraryTagId {
    store
        .library_tags()
        .iter()
        .find(|tag| tag.normalized_name == name)
        .unwrap()
        .id
}

fn rect() -> HighlightRect {
    HighlightRect {
        page_idx: 0,
        x0: 0.0,
        y0: 0.0,
        x1: 1.0,
        y1: 1.0,
    }
}

/// The records with every uid replaced by what it names and the times
/// zeroed: equal for two libraries with the same content.
fn named(store: &mut LibraryStore) -> BTreeMap<String, String> {
    let snapshot = store.sync_snapshot().unwrap();
    let records = canonical(&snapshot);
    let ids = &snapshot.ids;
    let mut names = Vec::new();
    for item in &snapshot.items {
        names.push((
            ids.uid(SyncKind::Item, item.id.0).unwrap(),
            format!("item:{}", item.title),
        ));
    }
    for attachment in &snapshot.attachments {
        names.push((
            ids.uid(SyncKind::Attachment, attachment.id.0).unwrap(),
            format!("file:{}", attachment.path.display()),
        ));
    }
    for collection in &snapshot.collections {
        names.push((
            ids.uid(SyncKind::Collection, collection.id.0).unwrap(),
            format!("collection:{}", collection.name),
        ));
    }
    for view in &snapshot.views {
        names.push((
            ids.uid(SyncKind::SavedView, view.id.0).unwrap(),
            format!("view:{}", view.name),
        ));
    }
    for highlight in &snapshot.highlights {
        names.push((
            ids.uid(SyncKind::Highlight, highlight.id).unwrap(),
            format!("highlight:{}", highlight.text),
        ));
    }
    for bookmark in &snapshot.bookmarks {
        names.push((
            ids.uid(SyncKind::Bookmark, bookmark.id).unwrap(),
            format!("bookmark:{:?}", bookmark.title),
        ));
    }
    for (id, page) in &snapshot.pages {
        names.push((
            ids.uid(SyncKind::Page, *id).unwrap(),
            format!("page:{}", page.title),
        ));
    }
    let rename = |text: &str| {
        let mut text = text.to_string();
        for (uid, name) in &names {
            text = text.replace(&uid.to_string(), name);
        }
        text
    };
    records
        .into_iter()
        .map(|(key, mut fields)| {
            for field in ["created", "updated", "at", "written_at", "fetched_at"] {
                if fields.contains_key(field) {
                    fields.insert(field.to_string(), json!(0));
                }
            }
            let mut name = rename(&key.key);
            if key.kind == kinds::PAIR || key.kind == kinds::SUGGESTION {
                // In uid order, which differs between the two.
                let mut sides = name.split('|').map(str::to_string).collect::<Vec<_>>();
                sides.sort();
                name = sides.join("|");
            }
            if let Some(items) = name.strip_prefix("items:") {
                let mut items = items.split(',').map(str::to_string).collect::<Vec<_>>();
                items.sort();
                name = format!("items:{}", items.join(","));
            }
            (
                format!("{}/{name}", key.kind),
                rename(&serde_json::to_string(&fields).unwrap()),
            )
        })
        .collect()
}

/// The same library built in another order, so every local number
/// differs: `extra` adds and removes things first.
fn build(store: &mut LibraryStore, dir: &Path, extra: bool) {
    if extra {
        let scratch = link(store, &file(dir, "scratch"));
        store
            .set_item_tag_names(scratch, &["vision".to_string()])
            .unwrap();
        let folder = store.create_library_collection(None, "Scratch").unwrap();
        store.delete_library_collection(folder).unwrap();
        store.trash_library_items(&[scratch]).unwrap();
        store.permanently_delete_library_items(&[scratch]).unwrap();
    }
    let (first, second) = if extra {
        ("beta", "alpha")
    } else {
        ("alpha", "beta")
    };
    let first = link(store, &dir.join(format!("{first}.pdf")));
    let second = link(store, &dir.join(format!("{second}.pdf")));
    let (alpha, beta) = if extra {
        (second, first)
    } else {
        (first, second)
    };

    store
        .set_item_tag_names(alpha, &["ml".to_string(), "vision".to_string()])
        .unwrap();
    store.set_item_tag_names(beta, &["ml".to_string()]).unwrap();
    let ml = tag_id(store, "ml");
    store.set_tag_color(ml, Some(HighlightColor::Blue)).unwrap();
    let papers = store.create_library_collection(None, "Papers").unwrap();
    let nested = store
        .create_library_collection(Some(papers), "Nested")
        .unwrap();
    store
        .add_items_to_collection(&[alpha, beta], nested)
        .unwrap();
    assert_eq!(store.suggest_related(&[(beta, alpha, 0.75)]).unwrap(), 1);
    store.set_related_items(alpha, &[beta]).unwrap();
    store
        .save_library_view(
            "ML papers",
            LibraryFilter {
                tag_ids: vec![ml],
                collection_id: Some(papers),
                ..LibraryFilter::default()
            },
            LibrarySort::default(),
            LibraryViewMode::default(),
        )
        .unwrap();
    store
        .add_highlight(
            &dir.join("alpha.pdf"),
            HighlightColor::Yellow,
            vec![rect()],
            "attention",
            "a note",
        )
        .unwrap();
    store
        .add_bookmark(
            &dir.join("beta.pdf"),
            Some("results".to_string()),
            DocumentPosition::fixed(0.0, 10.0, 1.0, 4),
        )
        .unwrap();
    let page = store.next_page_id();
    store
        .set_page(
            page,
            Some(PageRecord {
                id: page,
                title: "Attention".to_string(),
                relative_path: "concepts/attention.md".to_string(),
                topic: PageTopic::Items(vec![alpha, beta]),
                sources: vec![alpha, beta],
                frontmatter_sha256: [0; 32],
                blocks: Vec::new(),
                modified: 1,
                written_at: 1,
                model: "fake".to_string(),
                tokens_in: 10,
                tokens_out: 5,
                previous: Some("the undo text stays here".to_string()),
                unreviewed: true,
            }),
        )
        .unwrap();
    store.dismiss_topic(&format!("tag:{}", ml.0)).unwrap();
    store
        .dismiss_topic(&format!("items:{},{}", alpha.0, beta.0))
        .unwrap();
}

#[test]
fn two_libraries_with_the_same_content_give_the_same_records() {
    let dir = folder("same");
    file(&dir, "alpha");
    file(&dir, "beta");
    let mut one = LibraryStore::open_for_test(dir.join("one.redb")).unwrap();
    let mut two = LibraryStore::open_for_test(dir.join("two.redb")).unwrap();
    build(&mut one, &dir, false);
    build(&mut two, &dir, true);

    let (one, two) = (named(&mut one), named(&mut two));
    for key in one.keys().chain(two.keys()) {
        assert_eq!(one.get(key), two.get(key), "{key}");
    }
    // The records say what they are, with no local numbers.
    let item = &one["item/item:alpha"];
    assert!(item.contains(r#""tags/ml":true"#) && item.contains(r#""tags/vision":true"#));
    assert!(item.contains(r#""collections/collection:Nested":true"#));
    assert!(one["collection/collection:Nested"].contains(r#""parent":"collection:Papers""#));
    assert!(one["view/view:ML papers"].contains(r#""tags":["ml"]"#));
    assert!(one["view/view:ML papers"].contains(r#""collection":"collection:Papers""#));
    assert!(one.contains_key("pair/item:alpha|item:beta"));
    assert!(one.contains_key("suggestion/item:alpha|item:beta"));
    assert!(one["tag/ml"].contains(r#""color":"Blue""#));
    assert!(one["page/page:Attention"].contains(r#""items":["item:alpha","item:beta"]"#));
    assert!(!one["page/page:Attention"].contains("undo text"));
    assert!(one.contains_key("topic/tag:ml"));
    assert!(one.keys().any(|key| key.starts_with("topic/items:")));
    assert!(one["highlight/highlight:attention"].contains(r#""item":"item:alpha""#));
    assert!(!one.keys().any(|key| key.contains("scratch")));
    fs::remove_dir_all(dir).unwrap();
}

fn changed(store: &mut LibraryStore) -> Vec<(RecordKey, glyphst_sync::Fields)> {
    let snapshot = store.sync_snapshot().unwrap();
    store
        .sync_reader()
        .unwrap()
        .diff(&snapshot)
        .unwrap()
        .changes
}

#[test]
fn an_export_holds_what_changed_since_the_last() {
    let dir = folder("diff");
    let mut store = LibraryStore::open_for_test(dir.join("library.redb")).unwrap();
    let id = link(&mut store, &file(&dir, "paper"));
    let uid = store
        .sync_ids()
        .unwrap()
        .uid(SyncKind::Item, id.0)
        .unwrap()
        .to_string();
    store.set_item_tag_names(id, &["ml".to_string()]).unwrap();
    let seqs = store.export_now().unwrap();
    assert_eq!(seqs, vec![1]);
    assert_eq!(store.sync_outbox().unwrap().len(), 1);
    assert!(changed(&mut store).is_empty(), "nothing changed since");

    // A field edited, a tag taken off and put back on, another taken off.
    store.set_library_item_favorite(id, true).unwrap();
    store.set_item_tag_names(id, &["nlp".to_string()]).unwrap();
    let changes = changed(&mut store);
    let item = changes
        .iter()
        .find(|(key, _)| key.kind == kinds::ITEM && key.key == uid)
        .map(|(_, fields)| fields)
        .unwrap();
    assert_eq!(item["favorite"], json!(true));
    assert_eq!(item["tags/ml"], Value::Null);
    assert_eq!(item["tags/nlp"], json!(true));
    assert!(!item.contains_key("title"));
    assert_eq!(store.export_now().unwrap(), vec![2]);

    store
        .set_item_tag_names(id, &["ml".to_string(), "nlp".to_string()])
        .unwrap();
    let changes = changed(&mut store);
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].1["tags/ml"], json!(true));
    store.export_now().unwrap();

    // Removed for good: a tombstone for it and its file.
    store.trash_library_items(&[id]).unwrap();
    store.permanently_delete_library_items(&[id]).unwrap();
    let changes = changed(&mut store);
    for kind in [kinds::ITEM, kinds::ATTACHMENT] {
        let (_, fields) = changes.iter().find(|(key, _)| key.kind == kind).unwrap();
        assert_eq!(fields.get(DELETED), Some(&json!(true)), "{kind}");
    }
    store.export_now().unwrap();
    assert!(changed(&mut store).is_empty(), "a tombstone is sent once");
    assert_eq!(
        store
            .sync_outbox()
            .unwrap()
            .iter()
            .map(|(seq, _)| *seq)
            .collect::<Vec<_>>(),
        vec![1, 2, 3, 4]
    );
    store.sync_segment_written(1, "digest".to_string()).unwrap();
    assert_eq!(store.sync_outbox().unwrap().len(), 3);
    assert_eq!(
        store.sync_written().get(&1).map(String::as_str),
        Some("digest")
    );
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn annotations_on_files_outside_the_library_stay_here() {
    let dir = folder("outside");
    let mut store = LibraryStore::open_for_test(dir.join("library.redb")).unwrap();
    let inside = file(&dir, "inside");
    link(&mut store, &inside);
    let outside = file(&dir, "outside");
    for path in [&inside, &outside] {
        store
            .add_highlight(path, HighlightColor::Green, vec![rect()], "words", "")
            .unwrap();
        store
            .add_bookmark(path, None, DocumentPosition::fixed(0.0, 0.0, 1.0, 1))
            .unwrap();
    }
    let snapshot = store.sync_snapshot().unwrap();
    let records = canonical(&snapshot);
    let count = |kind| records.keys().filter(|key| key.kind == kind).count();
    assert_eq!(count(kinds::HIGHLIGHT), 1);
    assert_eq!(count(kinds::BOOKMARK), 1);
    let sha = records
        .iter()
        .find(|(key, _)| key.kind == kinds::ATTACHMENT)
        .map(|(_, fields)| fields["sha256"].clone())
        .unwrap();
    let highlight = records
        .iter()
        .find(|(key, _)| key.kind == kinds::HIGHLIGHT)
        .map(|(_, fields)| fields)
        .unwrap();
    assert_eq!(highlight["file"], sha);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_stale_export_is_dropped() {
    let dir = folder("stale");
    let mut store = LibraryStore::open_for_test(dir.join("library.redb")).unwrap();
    link(&mut store, &file(&dir, "paper"));
    let snapshot = store.sync_snapshot().unwrap();
    let export = store.sync_reader().unwrap().diff(&snapshot).unwrap();
    assert!(!export.is_empty());
    // Others' changes were applied meanwhile.
    store.sync.generation += 1;
    assert!(store.commit_export(export).unwrap().is_empty());
    assert!(store.sync_outbox().unwrap().is_empty());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn ai_results_name_tags_collections_and_runs_without_local_numbers() {
    let dir = folder("ai");
    let mut store = LibraryStore::open_for_test(dir.join("library.redb")).unwrap();
    let id = link(&mut store, &file(&dir, "paper"));
    let other = link(&mut store, &file(&dir, "other"));
    let run = store.next_enrichment_run();
    store
        .apply_enrichment(
            id,
            EnrichmentApply {
                run,
                tags: vec!["ml".to_string()],
                create_tags: true,
                collections: Vec::new(),
                suggested_collection: Some(vec!["Reading".to_string(), "Later".to_string()]),
                metadata: None,
                enrichment: Some(ItemEnrichment {
                    prompt_version: 1,
                    model: "fake".to_string(),
                    input_sha256: [7; 32],
                    status: EnrichmentStatus::Done,
                    attempts: 1,
                    finished_at: 5,
                    input_tokens: 10,
                    output_tokens: 3,
                    tldr: "short".to_string(),
                    key_points: Vec::new(),
                    suggested_tags: vec!["ml".to_string()],
                    suggested_collection: None,
                    kind: None,
                    language: None,
                    confidence: None,
                }),
            },
        )
        .unwrap();
    store
        .set_metadata_record(
            id,
            MetadataRecord {
                possible_duplicate_of: Some(other),
                external_ids: vec!["zotero:1/ABCD".to_string()],
                ..MetadataRecord::default()
            },
        )
        .unwrap();

    let snapshot = store.sync_snapshot().unwrap();
    let records = canonical(&snapshot);
    let uid = snapshot.ids.uid(SyncKind::Item, id.0).unwrap();
    let other_uid = snapshot.ids.uid(SyncKind::Item, other.0).unwrap();
    let tag = &records[&RecordKey::new(kinds::ASSIGNMENT, format!("{uid}|tag:ml"))];
    assert_eq!(
        tag["origin"],
        json!({ "enrichment": format!("{}:{run}", store.sync_device()) })
    );
    assert_eq!(tag["state"], json!("Applied"));
    assert!(records.contains_key(&RecordKey::new(
        kinds::ASSIGNMENT,
        format!("{uid}|new:[\"Reading\",\"Later\"]")
    )));
    assert_eq!(
        records[&RecordKey::new(kinds::ENRICHMENT, uid.to_string())]["value"]["tldr"],
        json!("short")
    );
    let metadata = &records[&RecordKey::new(kinds::METADATA, uid.to_string())];
    assert_eq!(metadata["possible_duplicate_of"], json!(other_uid));
    assert_eq!(metadata["external_ids"], json!(["zotero:1/ABCD"]));
    fs::remove_dir_all(dir).unwrap();
}
