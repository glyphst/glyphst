//! `@` mentions in the assistant's composer: what the user can attach (pages
//! as text or images, the selection, documents, outline sections,
//! annotations, library books), the menu that offers them, and the context
//! they become.
//!
//! What can change in the reader (the selection, an annotation, a book's
//! details) is copied when it is mentioned; pages are read when the message
//! is sent, on the assistant's worker thread.

use glyphst_ai::agent::{AgentCancellationToken, AgentImage};

use crate::{ai_tools::OpenDocument, document::OutlineEntry};

/// Pages of text a mention reads at most.
const MAX_TEXT_PAGES: usize = 30;
const MAX_TEXT_CHARS: usize = 40_000;
const MAX_DOCUMENT_CHARS: usize = 60_000;
/// Page images a mention attaches at most.
pub const MAX_IMAGE_PAGES: usize = 4;
/// Entries the menu shows at most.
const MAX_CANDIDATES: usize = 40;

/// Something the user attached to a message.
#[derive(Clone, Debug)]
pub enum Mention {
    /// 0-based pages `first..=last` as text.
    Pages {
        document: OpenDocument,
        first: usize,
        last: usize,
    },
    /// 0-based pages as images.
    PageImages {
        document: OpenDocument,
        pages: Vec<usize>,
    },
    Document(OpenDocument),
    /// An outline entry's pages as text.
    Section {
        document: OpenDocument,
        title: String,
        first: usize,
        last: usize,
    },
    /// Text copied when mentioned: the selection, an annotation, a book.
    Text {
        kind: &'static str,
        label: String,
        text: String,
    },
}

impl Mention {
    /// The chip's text.
    pub fn label(&self) -> String {
        match self {
            Self::Pages {
                document,
                first,
                last,
            } => format!("{} {}", short(&document.title), pages_label(*first, *last)),
            Self::PageImages { document, pages } => match pages.as_slice() {
                [page] => format!("{} p. {} image", short(&document.title), page + 1),
                _ => format!("{} {} page images", short(&document.title), pages.len()),
            },
            Self::Document(document) => short(&document.title),
            Self::Section { title, .. } => short(title),
            Self::Text { label, .. } => label.clone(),
        }
    }

    pub fn icon(&self) -> &'static str {
        match self {
            Self::Pages { .. } | Self::Section { .. } => "file-text",
            Self::PageImages { .. } => "image",
            Self::Document(_) => "book-open",
            Self::Text { kind, .. } => match *kind {
                "selection" => "type",
                "annotation" => "highlighter",
                "book" => "library",
                _ => "file-text",
            },
        }
    }

    /// The context block and images the mention stands for.
    pub fn resolve(&self, cancel: &AgentCancellationToken) -> (String, Vec<AgentImage>) {
        let block = |kind: &str, attributes: String, body: &str| {
            format!(
                "<mention kind=\"{kind}\"{attributes}>\n{}\n</mention>",
                escape(body)
            )
        };
        let document_attributes = |document: &OpenDocument| {
            format!(
                " document=\"{}\" title=\"{}\"",
                document.id,
                escape(&document.title)
            )
        };
        let failed = |error: anyhow::Error| format!("[could not read it: {error:#}]");
        match self {
            Self::Pages {
                document,
                first,
                last,
            }
            | Self::Section {
                document,
                first,
                last,
                ..
            } => {
                let pages = (*first..=*last).take(MAX_TEXT_PAGES).collect::<Vec<_>>();
                let text = document
                    .page_texts(&pages, MAX_TEXT_CHARS, cancel)
                    .unwrap_or_else(failed);
                let mut attributes = document_attributes(document);
                attributes.push_str(&format!(" pages=\"{}-{}\"", first + 1, last + 1));
                let kind = match self {
                    Self::Section { title, .. } => {
                        attributes.push_str(&format!(" section=\"{}\"", escape(title)));
                        "section"
                    }
                    _ => "pages",
                };
                (block(kind, attributes, &text), Vec::new())
            }
            Self::Document(document) => {
                let pages = (0..document.page_count).collect::<Vec<_>>();
                let text = document
                    .page_texts(&pages, MAX_DOCUMENT_CHARS, cancel)
                    .unwrap_or_else(failed);
                (
                    block("document", document_attributes(document), &text),
                    Vec::new(),
                )
            }
            Self::PageImages { document, pages } => {
                let pages = &pages[..pages.len().min(MAX_IMAGE_PAGES)];
                let numbers = pages
                    .iter()
                    .map(|page| (page + 1).to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                let attributes = document_attributes(document) + &format!(" pages=\"{numbers}\"");
                match document.page_images(pages, cancel) {
                    Ok(images) => (
                        block("page_images", attributes, "The page images are attached."),
                        images,
                    ),
                    Err(error) => (block("page_images", attributes, &failed(error)), Vec::new()),
                }
            }
            Self::Text { kind, label, text } => (
                block(kind, format!(" label=\"{}\"", escape(label)), text),
                Vec::new(),
            ),
        }
    }
}

/// The `<context>` block of a message's mentions.
pub fn context(blocks: &[String]) -> Option<String> {
    (!blocks.is_empty()).then(|| {
        format!(
            "<context>\nThe user attached this with @ mentions; it is current.\n{}\n</context>",
            blocks.join("\n")
        )
    })
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn short(title: &str) -> String {
    let short = title.chars().take(28).collect::<String>();
    if title.chars().count() > 28 {
        format!("{short}…")
    } else {
        short
    }
}

fn pages_label(first: usize, last: usize) -> String {
    if first == last {
        format!("p. {}", first + 1)
    } else {
        format!("pp. {}-{}", first + 1, last + 1)
    }
}

/// An entry of the `@` menu.
#[derive(Clone, Debug)]
pub struct Candidate {
    pub title: String,
    pub detail: String,
    pub mention: Mention,
}

/// What the menu can offer, gathered by the app when the menu is open.
#[derive(Default)]
pub struct Sources {
    /// The open documents, the active one first.
    pub documents: Vec<OpenDocument>,
    /// The active document's outline.
    pub outline: Vec<OutlineEntry>,
    /// The selection in the active document: its first page and text.
    pub selection: Option<(usize, String)>,
    /// The active document's annotations: a label and the text to attach.
    pub annotations: Vec<(String, String)>,
    /// Library books: a label and their details.
    pub books: Vec<(String, String)>,
}

/// The menu's entries for `query` (the text after `@`), best first.
pub fn candidates(sources: &Sources, query: &str) -> Vec<Candidate> {
    let query = query.trim().to_lowercase();
    let mut out = Vec::new();
    let Some(active) = sources.documents.first() else {
        return books(sources, &query);
    };
    if let Some((first, last)) = page_query(&query) {
        let last = last.unwrap_or(first).max(first);
        if first >= 1 && last <= active.page_count {
            let (first, last) = (first - 1, last - 1);
            out.push(Candidate {
                title: format!("Page text: {}", pages_label(first, last)),
                detail: active.title.clone(),
                mention: Mention::Pages {
                    document: active.clone(),
                    first,
                    last,
                },
            });
            if active.renders() {
                out.push(Candidate {
                    title: format!("Page images: {}", pages_label(first, last)),
                    detail: format!("At most {MAX_IMAGE_PAGES} pages"),
                    mention: Mention::PageImages {
                        document: active.clone(),
                        pages: (first..=last).take(MAX_IMAGE_PAGES).collect(),
                    },
                });
            }
        }
        return out;
    }
    let current = active.current_page;
    let mut fixed = vec![Candidate {
        title: "This page".into(),
        detail: format!("Page {} as text", current + 1),
        mention: Mention::Pages {
            document: active.clone(),
            first: current,
            last: current,
        },
    }];
    if active.renders() {
        fixed.push(Candidate {
            title: "This page as an image".into(),
            detail: format!("Page {}", current + 1),
            mention: Mention::PageImages {
                document: active.clone(),
                pages: vec![current],
            },
        });
    }
    if let Some((page, text)) = &sources.selection {
        fixed.push(Candidate {
            title: "Selection".into(),
            detail: short(text),
            mention: Mention::Text {
                kind: "selection",
                label: format!("Selection, p. {}", page + 1),
                text: text.clone(),
            },
        });
    }
    fixed.push(Candidate {
        title: "Whole document".into(),
        detail: active.title.clone(),
        mention: Mention::Document(active.clone()),
    });
    for document in &sources.documents[1..] {
        fixed.push(Candidate {
            title: format!("Document: {}", document.title),
            detail: format!("{} pages, open", document.page_count),
            mention: Mention::Document(document.clone()),
        });
    }
    out.extend(fixed.into_iter().filter(|c| matches(&query, &c.title)));
    out.extend(sections(sources, active, &query));
    out.extend(
        sources
            .annotations
            .iter()
            .filter(|(label, _)| matches(&query, label))
            .map(|(label, text)| Candidate {
                title: label.clone(),
                detail: "Annotation".into(),
                mention: Mention::Text {
                    kind: "annotation",
                    label: short(label),
                    text: text.clone(),
                },
            }),
    );
    if !query.is_empty() {
        out.extend(books(sources, &query));
    }
    out.truncate(MAX_CANDIDATES);
    out
}

fn sections(sources: &Sources, active: &OpenDocument, query: &str) -> Vec<Candidate> {
    let outline = &sources.outline;
    outline
        .iter()
        .enumerate()
        .filter(|(_, entry)| entry.page.is_some() && matches(query, &entry.title))
        .map(|(index, entry)| {
            let first = entry.page.unwrap_or(0);
            // The section ends before the next entry at its level or above.
            let next = outline[index + 1..]
                .iter()
                .find(|other| other.depth <= entry.depth && other.page.is_some())
                .and_then(|other| other.page);
            let last = next
                .map_or(active.page_count.saturating_sub(1), |page| {
                    page.saturating_sub(1).max(first)
                })
                .min(first + MAX_TEXT_PAGES - 1);
            Candidate {
                title: format!("Section: {}", entry.title),
                detail: pages_label(first, last),
                mention: Mention::Section {
                    document: active.clone(),
                    title: entry.title.clone(),
                    first,
                    last,
                },
            }
        })
        .collect()
}

fn books(sources: &Sources, query: &str) -> Vec<Candidate> {
    sources
        .books
        .iter()
        .filter(|(label, _)| matches(query, label))
        .take(10)
        .map(|(label, text)| Candidate {
            title: format!("Book: {label}"),
            detail: "Library".into(),
            mention: Mention::Text {
                kind: "book",
                label: short(label),
                text: text.clone(),
            },
        })
        .collect()
}

/// Whether every character of `query` appears in `label` in order.
fn matches(query: &str, label: &str) -> bool {
    let label = label.to_lowercase();
    let mut chars = label.chars();
    query
        .chars()
        .filter(|c| !c.is_whitespace())
        .all(|c| chars.any(|l| l == c))
}

/// A page query: `12`, `p12`, `page12`, `page 12`, `pages 3-5`.
fn page_query(query: &str) -> Option<(usize, Option<usize>)> {
    let rest = query
        .trim_start_matches("pages")
        .trim_start_matches("page")
        .trim_start_matches("pp")
        .trim_start_matches('p')
        .trim_start_matches(['.', ' ']);
    let (first, last) = match rest.split_once('-') {
        Some((first, last)) => (first, Some(last)),
        None => (rest, None),
    };
    let first = first.trim().parse().ok()?;
    let last = match last.map(str::trim) {
        Some("") | None => None,
        Some(last) => Some(last.parse().ok()?),
    };
    Some((first, last))
}

/// The `@query` being typed before `cursor` (a byte offset), as the byte
/// range from the `@`.
pub fn active_query(text: &str, cursor: usize) -> Option<std::ops::Range<usize>> {
    let cursor = cursor.min(text.len());
    let before = text.get(..cursor)?;
    let start = before.rfind('@')?;
    let typed = &before[start + 1..];
    let boundary = before[..start]
        .chars()
        .next_back()
        .is_none_or(|ch| ch.is_whitespace() || matches!(ch, '(' | '[' | '"' | '\''));
    // A page range may hold one space ("page 12"); anything longer ends it.
    let words = typed.split_whitespace().count();
    let spaced = typed.contains(char::is_whitespace);
    (boundary && !typed.contains('\n') && (!spaced || (words <= 2 && !typed.ends_with("  "))))
        .then_some(start..cursor)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use glyphst_core::DocumentLayout;

    use super::*;

    fn document(id: &str, title: &str) -> OpenDocument {
        OpenDocument {
            id: id.into(),
            title: title.into(),
            path: PathBuf::from("/nonexistent.pdf"),
            layout: DocumentLayout::Fixed,
            page_count: 20,
            current_page: 4,
        }
    }

    fn outline(title: &str, page: usize, depth: usize) -> OutlineEntry {
        OutlineEntry {
            title: title.into(),
            page: Some(page),
            y: None,
            depth,
            parent: None,
        }
    }

    #[test]
    fn the_menu_offers_pages_sections_and_documents() {
        let sources = Sources {
            documents: vec![document("doc_1", "Paper"), document("doc_2", "Notes")],
            outline: vec![
                outline("Introduction", 0, 0),
                outline("Methods", 3, 0),
                outline("Setup", 4, 1),
                outline("Results", 9, 0),
            ],
            selection: Some((4, "a selected sentence".into())),
            annotations: vec![("Highlight: key claim".into(), "key claim".into())],
            books: vec![("Old book by Ada".into(), "details".into())],
        };
        let titles = |query: &str| {
            candidates(&sources, query)
                .into_iter()
                .map(|candidate| candidate.title)
                .collect::<Vec<_>>()
        };
        let all = titles("");
        assert_eq!(
            &all[..5],
            [
                "This page",
                "This page as an image",
                "Selection",
                "Whole document",
                "Document: Notes"
            ]
        );
        assert!(all.contains(&"Section: Methods".to_string()));
        assert!(all.contains(&"Highlight: key claim".to_string()));
        assert!(
            !all.iter().any(|title| title.starts_with("Book:")),
            "books need a query"
        );
        assert_eq!(titles("book"), ["Book: Old book by Ada"]);
        assert_eq!(titles("page12"), ["Page text: p. 12", "Page images: p. 12"]);
        assert_eq!(
            titles("pages 3-5"),
            ["Page text: pp. 3-5", "Page images: pp. 3-5"]
        );
        assert!(titles("page 99").is_empty());

        let methods = candidates(&sources, "methods")
            .into_iter()
            .find(|candidate| candidate.title == "Section: Methods")
            .unwrap();
        match methods.mention {
            Mention::Section { first, last, .. } => assert_eq!((first, last), (3, 8)),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn typed_queries_are_found_before_the_cursor() {
        assert_eq!(active_query("ask @pag", 8), Some(4..8));
        assert_eq!(active_query("@page 12", 8), Some(0..8));
        assert_eq!(active_query("mail@example", 12), None, "not after a word");
        assert_eq!(active_query("@page 12 and more", 17), None);
        assert_eq!(active_query("no mention", 10), None);
    }

    #[test]
    fn mentions_escape_their_text() {
        let mention = Mention::Text {
            kind: "selection",
            label: "Selection".into(),
            text: "a < b & </mention>".into(),
        };
        let (block, images) = mention.resolve(&AgentCancellationToken::default());
        assert!(images.is_empty());
        assert!(block.contains("a &lt; b &amp; &lt;/mention&gt;"));
        assert!(context(&[block]).unwrap().starts_with("<context>"));
        assert_eq!(context(&[]), None);
    }
}
