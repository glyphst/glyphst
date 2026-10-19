//! Open Library's answers (`/api/books?bibkeys=ISBN:`).

use anyhow::{Context as _, Result};
use glyphst_library::{LibraryItemKind, MetadataSource};
use serde_json::Value;

use crate::WorkMetadata;

pub(super) fn parse(body: &str, isbn: &str) -> Result<Option<WorkMetadata>> {
    let value: Value =
        serde_json::from_str(body).context("open library answered something else")?;
    let book = &value[format!("ISBN:{isbn}")];
    let title = book["title"]
        .as_str()
        .unwrap_or_default()
        .trim()
        .to_string();
    if title.is_empty() {
        return Ok(None);
    }
    let title = match book["subtitle"].as_str() {
        Some(subtitle) if !subtitle.trim().is_empty() => format!("{title}: {}", subtitle.trim()),
        _ => title,
    };
    let names = |key: &str| -> Vec<String> {
        book[key]
            .as_array()
            .map(|entries| {
                entries
                    .iter()
                    .filter_map(|entry| entry["name"].as_str())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    };
    let year = book["publish_date"].as_str().and_then(|date| {
        date.split(|c: char| !c.is_ascii_digit())
            .find(|part| part.len() == 4)
            .and_then(|year| year.parse().ok())
    });
    Ok(Some(WorkMetadata {
        source: MetadataSource::OpenLibrary,
        kind: Some(LibraryItemKind::Book),
        title,
        creators: names("authors"),
        year,
        publication: String::new(),
        publisher: names("publishers").into_iter().next().unwrap_or_default(),
        abstract_text: String::new(),
        language: String::new(),
        keywords: names("subjects").into_iter().take(8).collect(),
        doi: None,
        arxiv: None,
        isbn: Some(isbn.to_string()),
        pdf_url: None,
    }))
}
