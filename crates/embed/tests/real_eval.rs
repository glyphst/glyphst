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
    Indexer, ItemText, LibraryFile, Snapshot, Store,
    embed::{Embedder, Reranker},
    eval::{EvalSet, compare},
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
            embedding: Default::default(),
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
            embedding: Default::default(),
        })
        .unwrap()
    });
    let store = Store::open_reader(&index_path).unwrap();
    let report = compare(
        &store,
        &set,
        Some(embedder.as_ref()),
        reranker.as_ref().map(|reranker| reranker as &dyn Reranker),
    )
    .unwrap();
    for row in &report.rows {
        eprintln!("{:10} {:8} {}", row.run, row.tag, row.scores);
    }
    if let Some(time) = report.rerank_time {
        eprintln!(
            "rerank: {:.0} ms a query (search and rereading)",
            time.as_secs_f64() * 1000.0
        );
    }
    let _ = std::fs::remove_dir_all(dir);
}
