//! Crossref's REST answers (`/works/{doi}` and `/works?query=`).

use anyhow::{Context as _, Result};
use glyphst_library::{LibraryItemKind, MetadataSource};
use serde_json::Value;

use super::strip_markup;
use crate::WorkMetadata;

pub(super) fn parse_work(body: &str) -> Result<Option<WorkMetadata>> {
    let value: Value = serde_json::from_str(body).context("crossref answered something else")?;
    Ok(work(&value["message"]))
}

pub(super) fn parse_search(body: &str) -> Result<Vec<WorkMetadata>> {
    let value: Value = serde_json::from_str(body).context("crossref answered something else")?;
    Ok(value["message"]["items"]
        .as_array()
        .map(|items| items.iter().filter_map(work).collect())
        .unwrap_or_default())
}

fn first_string(value: &Value) -> String {
    match value {
        Value::Array(values) => values
            .iter()
            .find_map(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        Value::String(value) => value.clone(),
        _ => String::new(),
    }
}

fn work(message: &Value) -> Option<WorkMetadata> {
    let title = first_string(&message["title"]);
    if title.trim().is_empty() {
        return None;
    }
    let creators = message["author"]
        .as_array()
        .map(|authors| {
            authors
                .iter()
                .filter_map(|author| {
                    let given = author["given"].as_str().unwrap_or_default();
                    let family = author["family"].as_str().unwrap_or_default();
                    let name = author["name"].as_str().unwrap_or_default();
                    let full = format!("{given} {family}").trim().to_string();
                    let full = if full.is_empty() {
                        name.to_string()
                    } else {
                        full
                    };
                    (!full.is_empty()).then_some(full)
                })
                .collect()
        })
        .unwrap_or_default();
    let year = ["published-print", "published-online", "issued", "created"]
        .iter()
        .find_map(|key| message[*key]["date-parts"][0][0].as_i64())
        .and_then(|year| i32::try_from(year).ok());
    let kind = match message["type"].as_str().unwrap_or_default() {
        "book" | "monograph" | "edited-book" | "reference-book" => Some(LibraryItemKind::Book),
        "" => None,
        "journal-article"
        | "proceedings-article"
        | "posted-content"
        | "report"
        | "dissertation"
        | "book-chapter" => Some(LibraryItemKind::Paper),
        _ => Some(LibraryItemKind::Other),
    };
    let isbn = message["ISBN"].as_array().and_then(|isbns| {
        isbns
            .iter()
            .filter_map(Value::as_str)
            .find_map(crate::identifiers::normalize_isbn)
    });
    Some(WorkMetadata {
        source: MetadataSource::Crossref,
        kind,
        title,
        creators,
        year,
        publication: first_string(&message["container-title"]),
        publisher: message["publisher"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        abstract_text: message["abstract"]
            .as_str()
            .map(strip_markup)
            .unwrap_or_default(),
        language: message["language"].as_str().unwrap_or_default().to_string(),
        keywords: message["subject"]
            .as_array()
            .map(|subjects| {
                subjects
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default(),
        doi: message["DOI"].as_str().map(str::to_lowercase),
        arxiv: None,
        isbn,
        pdf_url: None,
    })
}
