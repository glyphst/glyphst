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

/// Words of one meaning share a dimension: a model that knows synonyms.
struct Synonyms;

impl Synonyms {
    const GROUPS: &[&[&str]] = &[
        &["cheap", "inexpensive", "efficient", "cheaper"],
        &["long", "lengthy", "longer"],
        &["sequence", "sequences", "series"],
        &["picture", "image", "images", "vision"],
        &["cooking", "recipe", "kitchen"],
        &["music", "melody", "song"],
    ];

    fn vector(text: &str) -> Vec<f32> {
        let mut vector = vec![0.01_f32; Self::GROUPS.len() + 1];
        for word in text.to_lowercase().split(|c: char| !c.is_alphanumeric()) {
            if let Some(group) = Self::GROUPS.iter().position(|group| group.contains(&word)) {
                vector[group] += 1.0;
            }
        }
        crate::embed::truncate(&vector, vector.len()).unwrap()
    }
}

impl crate::embed::Embedder for Synonyms {
    fn model_key(&self) -> &str {
        "synonyms/7/1"
    }

    fn embed_documents(&self, passages: &[(&str, &str)]) -> anyhow::Result<Vec<Vec<f32>>> {
        Ok(passages
            .iter()
            .map(|(_, text)| Self::vector(text))
            .collect())
    }

    fn embed_query(&self, query: &str) -> anyhow::Result<Vec<f32>> {
        Ok(Self::vector(query))
    }
}

#[test]
fn meaning_finds_passages_without_the_words_and_mixes_with_them() {
    use crate::{embed::Embedder as _, search::Meaning, vectors::VectorIndex};
    let dir = folder("meaning");
    let texts = [
        "Efficient models for long sequences",
        "A recipe book for the kitchen",
        "Songs and melody in music",
        "Image classification with vision models",
        "Kitchen tools and cooking",
        "Music theory for beginners",
    ];
    let mut files = Vec::new();
    let mut items = Vec::new();
    for (index, text) in texts.iter().enumerate() {
        let path = dir.join(format!("{index}.pdf"));
        std::fs::write(&path, text_pages_pdf(&[&[text]])).unwrap();
        files.push(file(index as u64 + 1, path, index as u8 + 1));
        items.push(item(index as u64 + 1, "untitled"));
    }
    let mut store = Store::open_in_memory().unwrap();
    store.reconcile(&Snapshot { files, items }, 0).unwrap();
    let store = run_steps(store, 20);
    let store = crate::indexer::run_embed(store, std::sync::Arc::new(Synonyms), 5);
    assert_eq!(store.vector_count("synonyms/7/1").unwrap(), texts.len());

    // The words alone find nothing.
    let request = SearchRequest {
        text: "inexpensive lengthy series".to_string(),
        ..SearchRequest::default()
    };
    assert!(
        store
            .search(&request)
            .unwrap()
            .items
            .iter()
            .all(|item| item.item_id != 1)
    );
    // Meaning finds the passage that says it in other words.
    let index = VectorIndex::load(&store, "synonyms/7/1").unwrap();
    let query = Synonyms.embed_query(&request.text).unwrap();
    let results = store
        .search_with(
            &request,
            Some(Meaning {
                index: &index,
                query: &query,
            }),
        )
        .unwrap();
    let top = &results.items[0];
    assert_eq!(top.item_id, 1, "{results:?}");
    assert!(top.pages[0].meaning.unwrap() >= 2.0);
    assert_eq!(results.weaker, results.items.len() - 1);

    // Another model's vectors are dropped and made again.
    let mut store = store;
    assert_eq!(store.drop_other_vectors("other/1").unwrap(), texts.len());
    let _ = std::fs::remove_dir_all(dir);
}

/// The labelled set (`tests/fixtures/eval.json`) indexed: its documents
/// as PDFs, one item each.
fn eval_corpus(name: &str) -> (Store, crate::eval::EvalSet, PathBuf) {
    #[derive(serde::Deserialize)]
    struct Fixture {
        documents: std::collections::BTreeMap<String, Vec<String>>,
    }
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/eval.json");
    let fixture: Fixture = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let set = crate::eval::EvalSet::load(&path).unwrap();
    let dir = folder(name);
    let mut snapshot = Snapshot::default();
    for (index, (file_name, pages)) in fixture.documents.iter().enumerate() {
        let path = dir.join(file_name);
        let pages = pages
            .iter()
            .map(|page| vec![page.as_str()])
            .collect::<Vec<_>>();
        let refs = pages.iter().map(Vec::as_slice).collect::<Vec<_>>();
        std::fs::write(&path, text_pages_pdf(&refs)).unwrap();
        let id = index as u64 + 1;
        snapshot.files.push(file(id, path, index as u8 + 1));
        snapshot
            .items
            .push(item(id, file_name.trim_end_matches(".pdf")));
    }
    let mut store = Store::open_in_memory().unwrap();
    store.reconcile(&snapshot, 0).unwrap();
    (run_steps(store, 50), set, dir)
}

#[test]
fn the_labelled_set_finds_exact_words_by_keyword() {
    let (store, set, dir) = eval_corpus("eval-words");
    let ranks = set
        .queries
        .iter()
        .filter(|query| query.tag == "words")
        .map(|query| {
            let results = store
                .search(&SearchRequest {
                    text: query.q.clone(),
                    ..SearchRequest::default()
                })
                .unwrap();
            crate::eval::rank(&results, query)
        })
        .collect::<Vec<_>>();
    let scores = crate::eval::Scores::of(&ranks);
    assert!(scores.at1 >= 0.85, "keyword floor: {scores}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn the_reranker_reorders_the_strong_items_by_their_best_passage() {
    use crate::embed::Reranker;

    /// Prefers whatever mentions "recurrent".
    struct Prefers;
    impl Reranker for Prefers {
        fn rerank(&self, _query: &str, passages: &[&str]) -> anyhow::Result<Vec<f32>> {
            Ok(passages
                .iter()
                .map(|text| if text.contains("recurrent") { 5.0 } else { 1.0 })
                .collect())
        }
    }
    let dir = folder("rerank");
    let texts = [
        "sequence models sequence models sequence models with attention",
        "sequence models built from recurrent networks",
        "a recipe for bread",
    ];
    let mut snapshot = Snapshot::default();
    for (index, text) in texts.iter().enumerate() {
        let path = dir.join(format!("{index}.pdf"));
        std::fs::write(&path, text_pages_pdf(&[&[text]])).unwrap();
        let id = index as u64 + 1;
        snapshot.files.push(file(id, path, index as u8 + 1));
        snapshot.items.push(item(id, &format!("paper {id}")));
    }
    let mut store = Store::open_in_memory().unwrap();
    store.reconcile(&snapshot, 0).unwrap();
    let store = run_steps(store, 20);
    let request = SearchRequest {
        text: "sequence models".to_string(),
        ..SearchRequest::default()
    };
    let plain = store.search(&request).unwrap();
    assert_eq!(plain.items[0].item_id, 1, "{plain:#?}");
    assert!(!plain.reranked);
    let reranked = store.search_reranked(&request, None, &Prefers).unwrap();
    assert!(reranked.reranked);
    assert_eq!(reranked.items[0].item_id, 2, "{reranked:#?}");
    // Logits (5 and 1) read as probabilities: 0.99 and 0.73, both strong.
    let first = reranked.items[0].pages[0].rerank.unwrap();
    assert!((first - 1.0 / (1.0 + (-5.0f32).exp())).abs() < 1e-6);
    assert_eq!(reranked.items.len(), plain.items.len());
    assert_eq!(reranked.weaker, 0);

    // A weak match by its words is read too, and rises when it answers.
    struct Bread;
    impl Reranker for Bread {
        fn rerank(&self, _query: &str, passages: &[&str]) -> anyhow::Result<Vec<f32>> {
            Ok(passages
                .iter()
                .map(|text| if text.contains("bread") { 0.9 } else { 0.1 })
                .collect())
        }
    }
    let request = SearchRequest {
        text: "sequence models bread".to_string(),
        ..SearchRequest::default()
    };
    let plain = store.search(&request).unwrap();
    let bread = plain.items.iter().position(|item| item.item_id == 3);
    assert!(bread.is_some_and(|at| at > 0), "{plain:#?}");
    let reranked = store.search_reranked(&request, None, &Bread).unwrap();
    assert_eq!(reranked.items[0].item_id, 3, "{reranked:#?}");
    assert_eq!(
        reranked.weaker,
        reranked.items.len() - 1,
        "0.1 is under half of 0.9"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn compare_scores_every_run_the_models_allow() {
    use crate::embed::{Embedder as _, FakeEmbedder, FakeReranker};
    let (mut store, set, dir) = eval_corpus("eval-compare");
    let crate::eval::Report {
        rows,
        rerank_time: time,
        ..
    } = crate::eval::compare(&store, &set, None, None).unwrap();
    assert!(rows.iter().all(|row| row.run == "keyword"), "{rows:#?}");
    assert!(time.is_none());
    let words = rows.iter().find(|row| row.tag == "words").unwrap().scores;
    assert!(words.at1 >= 0.85, "{words}");

    // Vectors from the fake model: meaning and the reranker join.
    let pending = store
        .next_to_embed(FakeEmbedder.model_key(), 1_000)
        .unwrap();
    let vectors = FakeEmbedder
        .embed_documents(
            &pending
                .iter()
                .map(|p| (p.title.as_str(), p.text.as_str()))
                .collect::<Vec<_>>(),
        )
        .unwrap();
    let rows_to_save = pending
        .into_iter()
        .zip(vectors)
        .map(|(p, v)| (p.chunk_id, p.title, v))
        .collect::<Vec<_>>();
    store
        .save_vectors(FakeEmbedder.model_key(), &rows_to_save)
        .unwrap();
    let report =
        crate::eval::compare(&store, &set, Some(&FakeEmbedder), Some(&FakeReranker)).unwrap();
    let (rows, time) = (report.rows, report.rerank_time);
    let runs = rows
        .iter()
        .filter(|row| row.tag == "all")
        .map(|row| row.run)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        runs,
        [
            "keyword",
            "kw+rerank",
            "rrf",
            "rrf+rerank",
            "z+rerank",
            "z-score"
        ]
        .into()
    );
    assert!(time.is_some());
    let _ = std::fs::remove_dir_all(dir);
}
