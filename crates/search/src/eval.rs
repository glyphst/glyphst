//! Measuring search on a labelled set, as DigUp does: each query names
//! the file (and pages) that should come back, and the runner reports how
//! often it is first (R@1), in the first five (R@5), and the mean
//! reciprocal rank. Defaults (z-scores or RRF) are chosen on these numbers.

use std::path::Path;

use anyhow::{Context as _, Result};
use serde::Deserialize;

use crate::{
    Fusion, SearchRequest, Store,
    embed::{Embedder, Reranker},
    search::{Meaning, SearchResults},
    vectors::VectorIndex,
};

/// `{"queries": [{"q": "…", "expect": ["paper.pdf@3-4"]}]}`, or one
/// query a line (the Reader's `opened.jsonl`).
#[derive(Clone, Debug, Deserialize)]
pub struct EvalSet {
    pub queries: Vec<EvalQuery>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct EvalQuery {
    pub q: String,
    /// Files, each with an optional 1-based page range: `name.pdf@3-5`.
    pub expect: Vec<String>,
    #[serde(default)]
    pub tag: String,
}

impl EvalSet {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        if path
            .extension()
            .is_some_and(|extension| extension == "jsonl")
        {
            let queries = text
                .lines()
                .filter(|line| !line.trim().is_empty())
                .map(serde_json::from_str)
                .collect::<Result<Vec<EvalQuery>, _>>()?;
            return Ok(Self { queries });
        }
        Ok(serde_json::from_str(&text)?)
    }
}

/// Whether a hit (a file name, a 0-based page) is what `expect` names.
pub fn matches(expect: &str, file: &str, page: usize) -> bool {
    let (name, pages) = expect.split_once('@').unwrap_or((expect, ""));
    if name != file {
        return false;
    }
    if pages.is_empty() {
        return true;
    }
    let page = page + 1;
    let (first, last) = pages.split_once('-').unwrap_or((pages, pages));
    let (Ok(first), Ok(last)) = (first.parse::<usize>(), last.parse::<usize>()) else {
        return true;
    };
    (first..=last).contains(&page)
}

/// The 1-based rank of the first right answer among the results' items
/// (any of the item's shown pages counts), if any.
pub fn rank(results: &SearchResults, query: &EvalQuery) -> Option<usize> {
    results
        .items
        .iter()
        .position(|item| {
            item.pages.iter().any(|hit| {
                let file = hit
                    .path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default();
                query
                    .expect
                    .iter()
                    .any(|expect| matches(expect, &file, hit.page))
            })
        })
        .map(|index| index + 1)
}

/// R@1, R@5 and MRR over ranks.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Scores {
    pub at1: f32,
    pub at5: f32,
    pub mrr: f32,
    pub queries: usize,
}

impl Scores {
    pub fn of(ranks: &[Option<usize>]) -> Self {
        let count = ranks.len().max(1) as f32;
        Self {
            at1: ranks.iter().filter(|rank| **rank == Some(1)).count() as f32 / count,
            at5: ranks
                .iter()
                .filter(|rank| rank.is_some_and(|rank| rank <= 5))
                .count() as f32
                / count,
            mrr: ranks
                .iter()
                .map(|rank| rank.map_or(0.0, |rank| 1.0 / rank as f32))
                .sum::<f32>()
                / count,
            queries: ranks.len(),
        }
    }
}

impl std::fmt::Display for Scores {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "R@1 {:.2} · R@5 {:.2} · MRR {:.2} ({} queries)",
            self.at1, self.at5, self.mrr, self.queries
        )
    }
}

/// One way of searching, scored on a group of the set's queries.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    /// `keyword`, `z-score`, `rrf`, and with the reranker `kw+rerank`,
    /// `z+rerank`, `rrf+rerank`.
    pub run: &'static str,
    /// A tag of the set's, or `all`.
    pub tag: String,
    pub scores: Scores,
}

/// A query's rank (1-based, or not found) in each run.
pub type RunRanks = Vec<(&'static str, Option<usize>)>;

/// What `compare` found.
#[derive(Clone, Debug, Default)]
pub struct Report {
    pub rows: Vec<Row>,
    /// Each query with its rank in each run, in the set's order.
    pub queries: Vec<(String, RunRanks)>,
    /// The reranker's time a query (with meaning), when it ran.
    pub rerank_time: Option<std::time::Duration>,
}

impl Report {
    /// Queries some run ranks worse than keywords alone: `q · keyword 1 ·
    /// z-score 4`.
    pub fn worse_than_keywords(&self) -> Vec<String> {
        let shown = |rank: Option<usize>| rank.map_or("-".to_string(), |rank| rank.to_string());
        let worse = |rank: Option<usize>, keyword: Option<usize>| match (rank, keyword) {
            (None, Some(_)) => true,
            (Some(rank), Some(keyword)) => rank > keyword,
            _ => false,
        };
        self.queries
            .iter()
            .filter_map(|(q, ranks)| {
                let keyword = ranks.iter().find(|(run, _)| *run == "keyword")?.1;
                ranks
                    .iter()
                    .any(|(_, rank)| worse(*rank, keyword))
                    .then(|| {
                        let runs = ranks
                            .iter()
                            .map(|(run, rank)| format!("{run} {}", shown(*rank)))
                            .collect::<Vec<_>>();
                        format!("{q} · {}", runs.join(" · "))
                    })
            })
            .collect()
    }
}

/// Every way of searching that the models given allow, on every query of
/// `set`, scored by tag and over all.
pub fn compare(
    store: &Store,
    set: &EvalSet,
    embedder: Option<&dyn Embedder>,
    reranker: Option<&dyn Reranker>,
) -> Result<Report> {
    let vectors = embedder
        .map(|embedder| VectorIndex::load(store, embedder.model_key()))
        .transpose()?;
    let mut ranks: std::collections::BTreeMap<(&'static str, String), Vec<Option<usize>>> =
        Default::default();
    let mut rerank_time = std::time::Duration::ZERO;
    let mut per_query = Vec::new();
    for query in &set.queries {
        let request = SearchRequest {
            text: query.q.clone(),
            ..SearchRequest::default()
        };
        let rrf_request = SearchRequest {
            fusion: Fusion::Rrf,
            ..request.clone()
        };
        let vector = embedder
            .map(|embedder| embedder.embed_query(&query.q))
            .transpose()?;
        let meaning = || match (&vectors, &vector) {
            (Some(index), Some(vector)) => Some(Meaning {
                index,
                query: vector,
            }),
            _ => None,
        };
        let mut runs = vec![("keyword", store.search(&request)?)];
        if meaning().is_some() {
            runs.push(("z-score", store.search_with(&request, meaning())?));
            runs.push(("rrf", store.search_with(&rrf_request, meaning())?));
        }
        if let Some(reranker) = reranker {
            runs.push((
                "kw+rerank",
                store.search_reranked(&request, None, reranker)?,
            ));
            if meaning().is_some() {
                let started = std::time::Instant::now();
                runs.push((
                    "z+rerank",
                    store.search_reranked(&request, meaning(), reranker)?,
                ));
                rerank_time += started.elapsed();
                runs.push((
                    "rrf+rerank",
                    store.search_reranked(&rrf_request, meaning(), reranker)?,
                ));
            }
        }
        let mut found_by_run = Vec::new();
        for (run, results) in &runs {
            let found = rank(results, query);
            found_by_run.push((*run, found));
            if !query.tag.is_empty() && query.tag != "all" {
                ranks
                    .entry((run, query.tag.clone()))
                    .or_default()
                    .push(found);
            }
            ranks
                .entry((run, "all".to_string()))
                .or_default()
                .push(found);
        }
        per_query.push((query.q.clone(), found_by_run));
    }
    let rows = ranks
        .into_iter()
        .map(|((run, tag), ranks)| Row {
            run,
            tag,
            scores: Scores::of(&ranks),
        })
        .collect();
    let rerank_time = (reranker.is_some() && embedder.is_some() && !set.queries.is_empty())
        .then(|| rerank_time / set.queries.len() as u32);
    Ok(Report {
        rows,
        queries: per_query,
        rerank_time,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expectations_name_files_and_page_ranges() {
        assert!(matches("a.pdf", "a.pdf", 9));
        assert!(matches("a.pdf@3-4", "a.pdf", 2));
        assert!(!matches("a.pdf@3-4", "a.pdf", 4));
        assert!(!matches("a.pdf@3", "b.pdf", 2));
        let scores = Scores::of(&[Some(1), Some(3), None, Some(1)]);
        assert_eq!((scores.at1, scores.at5), (0.5, 0.75));
        assert!((scores.mrr - (1.0 + 1.0 / 3.0 + 1.0) / 4.0).abs() < 1e-6);
    }
}
