//! `glyphst-reader --eval-search [SET]`: measures the library's search on
//! this computer's index, before any window opens. The set is a labelled
//! one (`{"queries": [{"q", "expect": ["file.pdf@3-4"]}]}`), else the
//! searches and passages opened from them (`opened.jsonl`, kept while
//! Settings › Library › "learn from what I open" is on). Meaning search and
//! the reranker join when downloaded (the downloaded engine, not one named
//! in Settings).

use std::path::PathBuf;

use anyhow::{Result, bail};
use glyphst_embed::{
    llama::{LlamaEmbedder, LlamaReranker},
    server::{ServerConfig, vulkan_dirs},
};
use glyphst_search::{
    Store,
    embed::{Embedder, Reranker},
    eval::{EvalSet, compare},
};

pub fn run(set: Option<PathBuf>) -> Result<()> {
    let index = crate::library_search::SearchService::default_path();
    if !index.is_file() {
        bail!(
            "no search index at {}: open the Reader once",
            index.display()
        );
    }
    let set_path = set.unwrap_or_else(|| index.with_file_name("opened.jsonl"));
    if !set_path.is_file() {
        bail!(
            "no set at {}: name one, or open passages from the library's search first",
            set_path.display()
        );
    }
    let set = EvalSet::load(&set_path)?;
    if set.queries.is_empty() {
        bail!("{} has no queries", set_path.display());
    }
    let store = Store::open_reader(&index)?;
    let status = store.status()?;
    println!(
        "{} queries from {} · index: {} / {} documents read, {} passages, {} with meaning",
        set.queries.len(),
        set_path.display(),
        status.done,
        status.docs,
        status.chunks,
        store.vector_count(glyphst_embed::manifest::MODEL_KEY)?
    );
    let config = |(exe, model)| ServerConfig {
        exe,
        model,
        library_dirs: vulkan_dirs(),
        pid_file: None,
        embedding: Default::default(),
    };
    let embedder = match crate::meaning::setup(None, "embeddinggemma-2").paths() {
        Some(paths) => Some(LlamaEmbedder::start(&config(paths))?),
        None => {
            println!("search by meaning is not downloaded: keywords only");
            None
        }
    };
    let reranker = match crate::meaning::rerank_setup(None, "bge-reranker-v2-m3").paths() {
        Some(paths) => Some(LlamaReranker::start(&config(paths))?),
        None => {
            println!("the reranker is not downloaded");
            None
        }
    };
    let report = compare(
        &store,
        &set,
        embedder.as_ref().map(|embedder| embedder as &dyn Embedder),
        reranker.as_ref().map(|reranker| reranker as &dyn Reranker),
    )?;
    for row in &report.rows {
        println!("{:10} {:8} {}", row.run, row.tag, row.scores);
    }
    if let Some(time) = report.rerank_time {
        println!(
            "reranking: {:.0} ms a query (search and rereading)",
            time.as_secs_f64() * 1000.0
        );
    }
    let worse = report.worse_than_keywords();
    if !worse.is_empty() {
        println!("ranked worse than by keywords:");
        for line in worse {
            println!("  {line}");
        }
    }
    Ok(())
}
