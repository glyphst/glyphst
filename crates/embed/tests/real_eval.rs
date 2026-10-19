//! The labelled set with the real model, by hand: keyword search, then
//! meaning mixed by z-scores and by reciprocal ranks, and each reranked
//! when GLYPHST_RERANK_MODEL names a reranker. Set GLYPHST_EMBED_SERVER,
//! GLYPHST_EMBED_MODEL (and GLYPHST_EMBED_LIBS on NixOS), then
//! `cargo test -p glyphst-embed --test real_eval -- --ignored --nocapture`.

use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use glyphst_embed::{
    llama::{LlamaEmbedder, LlamaReranker},
    server::ServerConfig,
};
use glyphst_search::{
    Fusion, Indexer, ItemText, LibraryFile, SearchRequest, Snapshot, Store,
    embed::Embedder,
    eval::{EvalSet, Scores, rank},
    search::Meaning,
    vectors::VectorIndex,
};

#[derive(serde::Deserialize)]
struct Fixture {
    documents: BTreeMap<String, Vec<String>>,
}

#[test]
#[ignore = "needs the real engine and model"]
fn the_labelled_set_with_the_real_model() {
    let var = |name: &str| std::env::var_os(name).map(PathBuf::from);
    let (Some(exe), Some(model)) = (var("GLYPHST_EMBED_SERVER"), var("GLYPHST_EMBED_MODEL")) else {
        return;
    };
    let fixture_path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../search/tests/fixtures/eval.json");
    let fixture: Fixture =
        serde_json::from_str(&std::fs::read_to_string(&fixture_path).unwrap()).unwrap();
    let set = EvalSet::load(&fixture_path).unwrap();
    let dir = std::env::temp_dir().join(format!("glyphst-real-eval-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut snapshot = Snapshot::default();
    for (index, (name, pages)) in fixture.documents.iter().enumerate() {
        let path = dir.join(name);
        let pages = pages
            .iter()
            .map(|page| vec![page.as_str()])
            .collect::<Vec<_>>();
        let refs = pages.iter().map(Vec::as_slice).collect::<Vec<_>>();
        std::fs::write(&path, glyphst_pdf::test_pdf::text_pages_pdf(&refs)).unwrap();
        let id = index as u64 + 1;
        snapshot.files.push(LibraryFile {
            item_id: id,
            attachment_id: id,
            size: std::fs::metadata(&path).unwrap().len(),
            path,
            sha256: [index as u8 + 1; 32],
            priority: 0,
        });
        snapshot.items.push(ItemText {
            item_id: id,
            title: name.trim_end_matches(".pdf").to_string(),
            ..ItemText::default()
        });
    }
    let index_path = dir.join("search.sqlite");
    let embedder: Arc<LlamaEmbedder> = Arc::new(
        LlamaEmbedder::start(&ServerConfig {
            exe,
            model,
            library_dirs: var("GLYPHST_EMBED_LIBS").into_iter().collect(),
            pid_file: None,
        })
        .unwrap(),
    );
    {
        let indexer = Indexer::start(index_path.clone(), || {}).unwrap();
        indexer.reconcile(snapshot);
        indexer.set_embedder(Some(embedder.clone() as Arc<dyn Embedder>));
        let started = Instant::now();
        loop {
            let status = indexer.status();
            if status.docs > 0 && status.done == status.docs && status.embedded >= status.chunks {
                break;
            }
            assert!(started.elapsed().as_secs() < 300, "{status:?}");
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    let reranker = var("GLYPHST_RERANK_MODEL").map(|model| {
        LlamaReranker::start(&ServerConfig {
            exe: var("GLYPHST_EMBED_SERVER").unwrap(),
            model,
            library_dirs: var("GLYPHST_EMBED_LIBS").into_iter().collect(),
            pid_file: None,
        })
        .unwrap()
    });
    let mut rerank_time = Duration::ZERO;
    let store = Store::open_reader(&index_path).unwrap();
    let vectors = VectorIndex::load(&store, embedder.model_key()).unwrap();
    let mut table: BTreeMap<(&str, &str), Vec<Option<usize>>> = BTreeMap::new();
    for query in &set.queries {
        let request = SearchRequest {
            text: query.q.clone(),
            ..SearchRequest::default()
        };
        let vector = embedder.embed_query(&query.q).unwrap();
        let meaning = || Meaning {
            index: &vectors,
            query: &vector,
        };
        let keyword = store.search(&request).unwrap();
        let z = store.search_with(&request, Some(meaning())).unwrap();
        let rrf = store
            .search_with(
                &SearchRequest {
                    fusion: Fusion::Rrf,
                    ..request.clone()
                },
                Some(meaning()),
            )
            .unwrap();
        let mut runs = vec![("keyword", keyword), ("z-score", z), ("rrf", rrf)];
        if let Some(reranker) = &reranker {
            let started = Instant::now();
            let reranked = store
                .search_reranked(&request, Some(meaning()), reranker)
                .unwrap();
            rerank_time += started.elapsed();
            let rrf_reranked = store
                .search_reranked(
                    &SearchRequest {
                        fusion: Fusion::Rrf,
                        ..request.clone()
                    },
                    Some(meaning()),
                    reranker,
                )
                .unwrap();
            // The reranker alone ordering what keywords found.
            runs.push((
                "kw+rerank",
                store.search_reranked(&request, None, reranker).unwrap(),
            ));
            runs.push(("z+rerank", reranked));
            runs.push(("rrf+rerank", rrf_reranked));
        }
        for (name, results) in &runs {
            let found = rank(results, query);
            table
                .entry((name, query.tag.as_str()))
                .or_default()
                .push(found);
            table.entry((name, "all")).or_default().push(found);
        }
    }
    for ((fusion, tag), ranks) in &table {
        eprintln!("{fusion:10} {tag:8} {}", Scores::of(ranks));
    }
    if let Some(reranker) = &reranker {
        use glyphst_search::embed::Reranker as _;
        let scores = reranker
            .rerank(
                "stopping a big network from memorising its training data",
                &[
                    "It is a simple way to reduce overfitting in large neural networks.",
                    "Dropout randomly drops units during training to prevent co-adaptation.",
                    "Sample quality on CIFAR10 reaches an FID score of 3.17.",
                    "A collection of recipes for baking bread at home",
                ],
            )
            .unwrap();
        eprintln!("scores (overfitting, dropout, FID, bread): {scores:?}");
        eprintln!(
            "rerank: {:.0} ms a query (search and rereading)",
            rerank_time.as_secs_f64() * 1000.0 / set.queries.len() as f64
        );
    }
    let _ = std::fs::remove_dir_all(dir);
}
