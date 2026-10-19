//! Measuring search on a labelled set, as DigUp does: each query names
//! the file (and pages) that should come back, and the runner reports how
//! often it is first (R@1), in the first five (R@5), and the mean
//! reciprocal rank. Defaults (z-scores or RRF) are chosen on these numbers.

use std::path::Path;

use anyhow::{Context as _, Result};
use serde::Deserialize;

use crate::search::SearchResults;

/// `{"queries": [{"q": "…", "expect": ["paper.pdf@3-4"]}]}`.
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
