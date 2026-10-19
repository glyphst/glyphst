//! arXiv's Atom answers (`/api/query?id_list=`).

use anyhow::{Context as _, Result};
use glyphst_library::{LibraryItemKind, MetadataSource};

use crate::WorkMetadata;

const ATOM: &str = "http://www.w3.org/2005/Atom";
const ARXIV: &str = "http://arxiv.org/schemas/atom";

pub(super) fn parse(body: &str) -> Result<Option<WorkMetadata>> {
    let document = roxmltree::Document::parse(body).context("arxiv answered something else")?;
    let Some(entry) = document
        .root_element()
        .children()
        .find(|node| node.has_tag_name((ATOM, "entry")))
    else {
        return Ok(None);
    };
    let text = |name: (&str, &str)| {
        entry
            .children()
            .find(|node| node.has_tag_name(name))
            .and_then(|node| node.text())
            .map(|text| text.split_whitespace().collect::<Vec<_>>().join(" "))
            .unwrap_or_default()
    };
    let title = text((ATOM, "title"));
    // An unknown id comes back as an entry titled "Error".
    let id = text((ATOM, "id"));
    if title.is_empty() || title == "Error" || !id.contains("arxiv.org/abs/") {
        return Ok(None);
    }
    let arxiv = id
        .rsplit("arxiv.org/abs/")
        .next()
        .map(|id| match id.rfind('v') {
            Some(at) if id[at + 1..].chars().all(|c| c.is_ascii_digit()) && at > 0 => {
                id[..at].to_string()
            }
            _ => id.to_string(),
        });
    let creators = entry
        .children()
        .filter(|node| node.has_tag_name((ATOM, "author")))
        .filter_map(|author| {
            author
                .children()
                .find(|node| node.has_tag_name((ATOM, "name")))
                .and_then(|node| node.text())
                .map(|name| name.trim().to_string())
        })
        .collect();
    let year = text((ATOM, "published"))
        .get(..4)
        .and_then(|year| year.parse().ok());
    let pdf_url = entry
        .children()
        .filter(|node| node.has_tag_name((ATOM, "link")))
        .find(|link| link.attribute("title") == Some("pdf"))
        .and_then(|link| link.attribute("href"))
        .map(|href| href.replace("http://", "https://"));
    let doi = Some(text((ARXIV, "doi")))
        .filter(|doi| !doi.is_empty())
        .map(|doi| doi.to_lowercase());
    let journal = text((ARXIV, "journal_ref"));
    Ok(Some(WorkMetadata {
        source: MetadataSource::Arxiv,
        kind: Some(LibraryItemKind::Paper),
        title,
        creators,
        year,
        publication: if journal.is_empty() {
            "arXiv".to_string()
        } else {
            journal
        },
        publisher: String::new(),
        abstract_text: text((ATOM, "summary")),
        language: String::new(),
        keywords: entry
            .children()
            .filter(|node| node.has_tag_name((ATOM, "category")))
            .filter_map(|node| node.attribute("term").map(str::to_string))
            .collect(),
        doi,
        arxiv,
        isbn: None,
        pdf_url,
    }))
}
