use std::{collections::HashSet, path::PathBuf};

use glyphst_pdf::test_pdf::text_pages_pdf;

use super::*;
use crate::{
    chunk::{Chunk, PageChunks},
    index::Step,
    indexer::{STEP_PAGES, run_steps},
};

fn folder(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("glyphst-search-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn file(item_id: u64, path: PathBuf, sha: u8) -> LibraryFile {
    LibraryFile {
        item_id,
        attachment_id: item_id * 10,
        size: std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0),
        path,
        sha256: [sha; 32],
        priority: 0,
    }
}

fn item(item_id: u64, title: &str) -> ItemText {
    ItemText {
        item_id,
        title: title.to_string(),
        ..ItemText::default()
    }
}

fn search(store: &Store, text: &str) -> SearchResults {
    store
        .search(&SearchRequest {
            text: text.to_string(),
            ..SearchRequest::default()
        })
        .unwrap()
}

/// Two papers: the first says "attention" on its second page.
fn library(dir: &std::path::Path) -> (PathBuf, PathBuf) {
    let attention = dir.join("attention.pdf");
    std::fs::write(
        &attention,
        text_pages_pdf(&[
            &["Introduction to sequence models"],
            &["Scaled dot product attention", "with multiple heads"],
        ]),
    )
    .unwrap();
    let bert = dir.join("bert.pdf");
    std::fs::write(
        &bert,
        text_pages_pdf(&[&["Masked language models", "pre-trained on books"]]),
    )
    .unwrap();
    (attention, bert)
}

#[test]
fn documents_are_read_into_pages_and_found_by_their_words() {
    let dir = folder("read");
    let (attention, bert) = library(&dir);
    let mut store = Store::open_in_memory().unwrap();
    store
        .reconcile(
            &Snapshot {
                files: vec![file(1, attention, 1), file(2, bert, 2)],
                items: vec![item(1, "Attention Is All You Need"), item(2, "BERT")],
            },
            0,
        )
        .unwrap();
    let store = run_steps(store, 10);
    let status = store.status().unwrap();
    assert_eq!((status.docs, status.done), (2, 2), "{status:?}");
    assert_eq!(status.pages_done, 3);

    let results = search(&store, "dot product attention");
    assert_eq!(results.items.len(), 1);
    let hit = &results.items[0];
    assert_eq!(hit.item_id, 1);
    assert!(hit.in_details, "the title says attention too");
    assert_eq!(hit.pages[0].page, 1);
    assert_eq!(hit.pages[0].coverage, 1.0);
    let highlighted = hit.pages[0]
        .highlights
        .iter()
        .map(|range| {
            hit.pages[0]
                .snippet
                .chars()
                .skip(range.start)
                .take(range.len())
                .collect::<String>()
        })
        .collect::<Vec<_>>();
    assert!(
        highlighted.contains(&"attention".to_string()),
        "{highlighted:?}"
    );
    assert!(!hit.pages[0].quote.is_empty());

    // Stemmed: "model" finds "models"; typing matches a prefix.
    assert_eq!(search(&store, "masked model").items[0].item_id, 2);
    let typing = store
        .search(&SearchRequest {
            text: "pre-trai".to_string(),
            typing: true,
            ..SearchRequest::default()
        })
        .unwrap();
    assert_eq!(typing.items[0].item_id, 2);
    // Only what the tab's filter allows.
    let filtered = store
        .search(&SearchRequest {
            text: "models".to_string(),
            items: Some(HashSet::from([1])),
            ..SearchRequest::default()
        })
        .unwrap();
    assert!(filtered.items.iter().all(|hit| hit.item_id == 1));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn moves_merges_trash_and_new_contents_follow_the_library() {
    let dir = folder("follow");
    let (attention, bert) = library(&dir);
    let mut store = Store::open_in_memory().unwrap();
    let items = vec![item(1, "Attention"), item(2, "BERT")];
    store
        .reconcile(
            &Snapshot {
                files: vec![file(1, attention.clone(), 1), file(2, bert.clone(), 2)],
                items: items.clone(),
            },
            0,
        )
        .unwrap();
    let mut store = run_steps(store, 10);
    let chunks = store.status().unwrap().chunks;

    // Moved: the same content at another path is not read again.
    let moved = dir.join("moved.pdf");
    std::fs::rename(&attention, &moved).unwrap();
    store
        .reconcile(
            &Snapshot {
                files: vec![file(1, moved.clone(), 1), file(2, bert.clone(), 2)],
                items: items.clone(),
            },
            0,
        )
        .unwrap();
    assert_eq!(store.next_doc(0).unwrap(), None, "nothing to read again");
    assert_eq!(store.status().unwrap().chunks, chunks);
    assert_eq!(store.doc_path(&[1; 32]).unwrap(), Some(moved.clone()));

    // Merged: item 2's file now belongs to item 1.
    store
        .reconcile(
            &Snapshot {
                files: vec![
                    file(1, moved.clone(), 1),
                    LibraryFile {
                        attachment_id: 11,
                        ..file(1, bert.clone(), 2)
                    },
                ],
                items: vec![item(1, "Attention")],
            },
            0,
        )
        .unwrap();
    assert_eq!(search(&store, "masked").items[0].item_id, 1);

    // Trashed: kept, but not found.
    store
        .reconcile(
            &Snapshot {
                files: vec![file(1, moved.clone(), 1)],
                items: vec![ItemText {
                    trashed: true,
                    ..item(1, "Attention")
                }],
            },
            0,
        )
        .unwrap();
    assert!(search(&store, "attention").items.is_empty());

    // New content: the old text goes, the new is read.
    std::fs::write(&moved, text_pages_pdf(&[&["Linear attention revisited"]])).unwrap();
    store
        .reconcile(
            &Snapshot {
                files: vec![file(1, moved, 3)],
                items: vec![item(1, "Attention")],
            },
            0,
        )
        .unwrap();
    let store = run_steps(store, 10);
    assert!(search(&store, "dot product").items.is_empty());
    assert_eq!(search(&store, "revisited").items[0].pages[0].page, 0);
    assert_eq!(
        store.status().unwrap().docs,
        1,
        "unused documents are dropped"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn reading_stopped_midway_resumes_where_it_was() {
    let dir = folder("resume");
    let book = dir.join("book.pdf");
    let pages = (0..45)
        .map(|page| format!("Chapter text on page {page} about topic{page}"))
        .collect::<Vec<_>>();
    let page_lines = pages
        .iter()
        .map(|page| vec![page.as_str()])
        .collect::<Vec<_>>();
    let page_refs = page_lines.iter().map(Vec::as_slice).collect::<Vec<_>>();
    std::fs::write(&book, text_pages_pdf(&page_refs)).unwrap();
    let path = dir.join("search.sqlite");
    {
        let mut store = Store::open(&path).unwrap();
        store
            .reconcile(
                &Snapshot {
                    files: vec![file(1, book.clone(), 9)],
                    items: vec![item(1, "A Book")],
                },
                0,
            )
            .unwrap();
        // One step, then the app quits.
        let store = run_steps(store, 1);
        assert_eq!(store.status().unwrap().pages_done, STEP_PAGES);
        assert_eq!(search(&store, "topic40").items.len(), 0);
        assert_eq!(
            search(&store, "topic3").items.len(),
            1,
            "searchable already"
        );
    }
    let store = run_steps(Store::open(&path).unwrap(), 5);
    let status = store.status().unwrap();
    assert_eq!((status.done, status.pages_done), (1, 45));
    assert_eq!(status.chunks, 45, "one chunk a page, none twice");
    let hit = &search(&store, "topic40").items[0].pages[0];
    assert_eq!(hit.page, 40);
    // The anchor's offset counts every glyph before it.
    let before = pages[..40]
        .iter()
        .map(|page| page.chars().count())
        .sum::<usize>();
    assert_eq!(hit.doc_offset, before);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn cjk_text_is_found_by_trigrams_and_short_runs() {
    let mut store = Store::open_in_memory().unwrap();
    let dir = folder("cjk");
    let paper = dir.join("paper.pdf");
    std::fs::write(&paper, text_pages_pdf(&[&["placeholder"]])).unwrap();
    store
        .reconcile(
            &Snapshot {
                files: vec![file(1, paper, 5)],
                items: vec![item(1, "注意力")],
            },
            0,
        )
        .unwrap();
    let doc_id = store.next_doc(0).unwrap().unwrap().doc_id;
    let chunk = |text: &str| Chunk {
        page: 0,
        glyph_start: 0,
        glyph_end: text.chars().count(),
        doc_offset: 0,
        quote: text.chars().take(32).collect(),
        prefix: String::new(),
        suffix: String::new(),
        rects: Vec::new(),
        section: String::new(),
        text: text.to_string(),
    };
    store
        .save_step(
            &Step {
                doc_id,
                format: "pdf",
                page_count: 1,
                pages: &[(
                    0,
                    PageChunks {
                        chunks: vec![chunk("本文提出了一种基于注意力机制的序列模型。")],
                        chars: 20,
                        image_only: false,
                    },
                )],
                pages_done: 1,
                glyphs_done: 20,
            },
            0,
        )
        .unwrap();
    assert_eq!(search(&store, "注意力机制").items.len(), 1, "trigrams");
    assert_eq!(search(&store, "模型").items.len(), 1, "two characters");
    assert!(search(&store, "图像").items.is_empty());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn an_epub_is_read_in_its_default_layout_with_sections() {
    let epub = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../pdf/tests/fixtures/sample.epub");
    let mut store = Store::open_in_memory().unwrap();
    store
        .reconcile(
            &Snapshot {
                files: vec![file(1, epub, 7)],
                items: vec![item(1, "Glyphst EPUB Sample")],
            },
            0,
        )
        .unwrap();
    let store = run_steps(store, 5);
    assert_eq!(store.status().unwrap().done, 1);
    let results = search(&store, "footnote target");
    let hit = &results.items[0].pages[0];
    assert_eq!(hit.section, "Chapter One");
    assert!(hit.coverage >= 1.0);
    assert!(!hit.quote.is_empty(), "an anchor to find it in any layout");
}

#[test]
fn reading_speed_is_learned_and_gives_the_time_left() {
    let store = Store::open_in_memory().unwrap();
    assert_eq!(
        store.status().unwrap().seconds_left(),
        None,
        "nothing measured yet"
    );
    store.record_estimate("page", 100.0, 30).unwrap();
    store.record_estimate("page", 200.0, 30).unwrap();
    let mean = store.status().unwrap().ms_per_page.unwrap();
    assert!((mean - 150.0).abs() < 1.0, "{mean}");
    let status = IndexStatus {
        docs: 3,
        done: 1,
        pages: 100,
        pages_done: 40,
        unopened: 1,
        ms_per_page: Some(100.0),
        ..IndexStatus::default()
    };
    // 60 pages left, and the unopened document about 50 more.
    assert_eq!(status.seconds_left(), Some(11));
}

#[test]
fn a_chunk_is_found_with_its_neighbours_and_its_items() {
    let dir = folder("chunks");
    let (attention, _) = library(&dir);
    let mut store = Store::open_in_memory().unwrap();
    store
        .reconcile(
            &Snapshot {
                files: vec![file(1, attention, 1)],
                items: vec![ItemText {
                    creators: "Ashish Vaswani, Noam Shazeer".to_string(),
                    ..item(1, "Attention Is All You Need")
                }],
            },
            0,
        )
        .unwrap();
    let store = run_steps(store, 5);
    let hit = search(&store, "dot product").items[0].pages[0].clone();
    let chunk = store.chunk(hit.chunk_id).unwrap().unwrap();
    assert_eq!((chunk.page, chunk.items.clone()), (1, vec![1]));
    assert!(chunk.text.contains("Scaled dot product attention"));
    let around = store.chunk_with_neighbours(hit.chunk_id, 1).unwrap();
    assert_eq!(around.len(), 2, "the page before is its neighbour");
    assert_eq!(
        store.item_titles(&[1]).unwrap()[&1],
        "Attention Is All You Need (Ashish Vaswani)"
    );
    let _ = std::fs::remove_dir_all(dir);
}
