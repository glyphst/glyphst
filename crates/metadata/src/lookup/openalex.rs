//! OpenAlex's answers (`/works/doi:{doi}` and `/works?search=`).

use anyhow::{Context as _, Result};
use glyphst_library::{LibraryItemKind, MetadataSource};
use serde_json::Value;

use crate::WorkMetadata;

pub(super) fn parse_work(body: &str) -> Result<Option<WorkMetadata>> {
    let value: Value = serde_json::from_str(body).context("openalex answered something else")?;
    Ok(work(&value))
}

pub(super) fn parse_search(body: &str) -> Result<Vec<WorkMetadata>> {
    let value: Value = serde_json::from_str(body).context("openalex answered something else")?;
    Ok(value["results"]
        .as_array()
        .map(|results| results.iter().filter_map(work).collect())
        .unwrap_or_default())
}

fn work(value: &Value) -> Option<WorkMetadata> {
    let title = value["title"]
        .as_str()
        .or_else(|| value["display_name"].as_str())
        .unwrap_or_default()
        .trim()
        .to_string();
    if title.is_empty() {
        return None;
    }
    let creators = value["authorships"]
        .as_array()
        .map(|authorships| {
            authorships
                .iter()
                .filter_map(|authorship| authorship["author"]["display_name"].as_str())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let kind = match value["type"].as_str().unwrap_or_default() {
        "book" => Some(LibraryItemKind::Book),
        "" => None,
        "article" | "preprint" | "book-chapter" | "dissertation" | "report" | "review" => {
            Some(LibraryItemKind::Paper)
        }
        _ => Some(LibraryItemKind::Other),
    };
    let doi = value["doi"]
        .as_str()
        .map(|doi| doi.trim_start_matches("https://doi.org/").to_lowercase());
    let arxiv = doi
        .as_deref()
        .and_then(|doi| doi.strip_prefix("10.48550/arxiv."))
        .map(str::to_string);
    Some(WorkMetadata {
        source: MetadataSource::OpenAlex,
        kind,
        title,
        creators,
        year: value["publication_year"]
            .as_i64()
            .and_then(|year| i32::try_from(year).ok()),
        publication: value["primary_location"]["source"]["display_name"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        publisher: value["primary_location"]["source"]["host_organization_name"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        abstract_text: rebuild_abstract(&value["abstract_inverted_index"]),
        language: value["language"].as_str().unwrap_or_default().to_string(),
        keywords: Vec::new(),
        doi,
        arxiv,
        isbn: None,
        pdf_url: value["best_oa_location"]["pdf_url"]
            .as_str()
            .map(str::to_string),
    })
}

/// OpenAlex gives an abstract as each word's positions.
fn rebuild_abstract(index: &Value) -> String {
    let Some(index) = index.as_object() else {
        return String::new();
    };
    let mut words = Vec::new();
    for (word, positions) in index {
        for position in positions.as_array().into_iter().flatten() {
            if let Some(position) = position.as_u64() {
                words.push((position, word.as_str()));
            }
        }
    }
    words.sort_by_key(|(position, _)| *position);
    words
        .into_iter()
        .map(|(_, word)| word)
        .collect::<Vec<_>>()
        .join(" ")
}
