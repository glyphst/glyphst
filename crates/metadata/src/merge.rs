//! Putting what was found about a work into a library item: a lookup's
//! answer over what the file says about itself over its file name, and
//! never over a field the user edited.

use glyphst_library::{
    LibraryCreator, LibraryCreatorRole, LibraryItem, LibraryItemKind, MetadataField, MetadataSource,
};

/// What a lookup service knows about a work.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WorkMetadata {
    pub source: MetadataSource,
    pub kind: Option<LibraryItemKind>,
    pub title: String,
    /// Full names, first name first.
    pub creators: Vec<String>,
    pub year: Option<i32>,
    /// The journal, conference or repository.
    pub publication: String,
    pub publisher: String,
    pub abstract_text: String,
    pub language: String,
    pub keywords: Vec<String>,
    pub doi: Option<String>,
    pub arxiv: Option<String>,
    pub isbn: Option<String>,
    /// A PDF anyone may download (arXiv, open access).
    pub pdf_url: Option<String>,
}

impl WorkMetadata {
    pub fn is_empty(&self) -> bool {
        self.title.trim().is_empty()
    }

    /// The identifier the work was found by, for the metadata record.
    pub fn identifier_label(&self) -> Option<String> {
        self.doi
            .as_ref()
            .map(|doi| format!("doi:{doi}"))
            .or_else(|| self.arxiv.as_ref().map(|id| format!("arxiv:{id}")))
            .or_else(|| self.isbn.as_ref().map(|isbn| format!("isbn:{isbn}")))
    }
}

/// Copies the work's fields into `item`, except locked ones and the ones
/// the work leaves empty. Returns the fields that changed.
pub fn merge_work(
    item: &mut LibraryItem,
    work: &WorkMetadata,
    locked: &[MetadataField],
) -> Vec<MetadataField> {
    let mut proposed = item.clone();
    if let Some(kind) = work.kind {
        proposed.kind = kind;
    }
    if !work.title.trim().is_empty() {
        proposed.title = clean(&work.title);
    }
    if !work.creators.is_empty() {
        proposed.creators = work
            .creators
            .iter()
            .map(|name| clean(name))
            .filter(|name| !name.is_empty())
            .map(|name| LibraryCreator {
                name,
                role: LibraryCreatorRole::Author,
            })
            .collect();
    }
    if work.year.is_some() {
        proposed.year = work.year;
    }
    for (value, target) in [
        (&work.publication, &mut proposed.publication),
        (&work.publisher, &mut proposed.publisher),
        (&work.language, &mut proposed.language),
    ] {
        if !value.trim().is_empty() {
            *target = clean(value);
        }
    }
    if !work.abstract_text.trim().is_empty() {
        proposed.abstract_text = work.abstract_text.trim().to_string();
    }
    if !work.keywords.is_empty() {
        proposed.embedded_keywords = work.keywords.clone();
    }
    for (value, target) in [
        (&work.doi, &mut proposed.identifiers.doi),
        (&work.arxiv, &mut proposed.identifiers.arxiv),
        (&work.isbn, &mut proposed.identifiers.isbn),
    ] {
        if value.is_some() {
            *target = value.clone();
        }
    }
    let mut changed = Vec::new();
    for field in MetadataField::ALL {
        if !locked.contains(&field) && field.differs(item, &proposed) {
            field.copy(&proposed, item);
            changed.push(field);
        }
    }
    changed
}

/// What a file's info entries say, as a work, with the junk titles and
/// authors that writing tools leave there dropped.
pub fn embedded_work(info: &[(&str, String)], file_stem: &str) -> WorkMetadata {
    let value = |key: &str| {
        info.iter()
            .find(|(candidate, _)| *candidate == key)
            .map(|(_, value)| value.trim())
            .unwrap_or_default()
    };
    let title = clean_embedded_title(value("title"), file_stem);
    let creators = split_authors(value("author"));
    let keywords = value("keywords")
        .split([';', ','])
        .map(clean)
        .filter(|keyword| !keyword.is_empty() && keyword.len() < 60)
        .collect();
    WorkMetadata {
        source: MetadataSource::Embedded,
        title,
        creators,
        keywords,
        ..WorkMetadata::default()
    }
}

fn clean_embedded_title(title: &str, file_stem: &str) -> String {
    let mut title = clean(title);
    for prefix in [
        "Microsoft Word - ",
        "Microsoft PowerPoint - ",
        "untitled - ",
    ] {
        if let Some(rest) = title.strip_prefix(prefix) {
            title = rest.to_string();
        }
    }
    for suffix in [".docx", ".doc", ".pptx", ".tex", ".dvi", ".pdf", ".indd"] {
        if title.to_lowercase().ends_with(suffix) {
            title.truncate(title.len() - suffix.len());
        }
    }
    let lower = title.to_lowercase();
    let junk = title.chars().filter(|c| c.is_alphabetic()).count() < 3
        || matches!(
            lower.as_str(),
            "untitled" | "title" | "document" | "slide 1" | "pdf"
        )
        || lower == file_stem.to_lowercase()
        || lower.starts_with("doi:")
        || lower.ends_with(".pdf");
    if junk { String::new() } else { title }
}

fn split_authors(authors: &str) -> Vec<String> {
    let authors = authors.trim();
    if authors.is_empty() || authors.chars().filter(|c| c.is_alphabetic()).count() < 2 {
        return Vec::new();
    }
    let separator = if authors.contains(';') { ";" } else { "," };
    authors
        .split(separator)
        .flat_map(|part| part.split(" and "))
        .flat_map(|part| part.split(" & "))
        .map(clean)
        .filter(|name| !name.is_empty() && !name.eq_ignore_ascii_case("unknown"))
        .collect()
}

fn clean(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use glyphst_library::{DocumentId, LibraryIdentifiers, ReadingStatus};

    use super::*;

    fn untitled() -> LibraryItem {
        LibraryItem {
            id: DocumentId(1),
            kind: LibraryItemKind::Paper,
            title: "1706.03762v7".to_string(),
            creators: Vec::new(),
            year: None,
            publication: String::new(),
            publisher: String::new(),
            abstract_text: String::new(),
            subject: String::new(),
            language: String::new(),
            identifiers: LibraryIdentifiers::default(),
            embedded_keywords: Vec::new(),
            favorite: false,
            status: ReadingStatus::ToRead,
            work_note: String::new(),
            created: 0,
            updated: 0,
            trashed_at: None,
        }
    }

    #[test]
    fn a_lookup_fills_the_item_but_not_its_locked_fields() {
        let mut item = untitled();
        item.year = Some(2018);
        let work = WorkMetadata {
            source: MetadataSource::Arxiv,
            kind: Some(LibraryItemKind::Paper),
            title: "Attention Is  All You Need".to_string(),
            creators: vec!["Ashish Vaswani".to_string(), "Noam Shazeer".to_string()],
            year: Some(2017),
            publication: "arXiv".to_string(),
            arxiv: Some("1706.03762".to_string()),
            ..WorkMetadata::default()
        };
        let changed = merge_work(&mut item, &work, &[MetadataField::Year]);
        assert_eq!(item.title, "Attention Is All You Need");
        assert_eq!(item.creators.len(), 2);
        assert_eq!(item.year, Some(2018), "the user's year stays");
        assert_eq!(item.identifiers.arxiv.as_deref(), Some("1706.03762"));
        assert_eq!(
            changed,
            vec![
                MetadataField::Title,
                MetadataField::Creators,
                MetadataField::Publication,
                MetadataField::Identifiers
            ]
        );
    }

    #[test]
    fn writing_tools_junk_is_dropped_from_embedded_info() {
        let work = embedded_work(
            &[
                ("title", "Microsoft Word - thesis-final.docx".to_string()),
                ("author", "Jane Doe; John Smith".to_string()),
                ("keywords", "transformers, attention".to_string()),
            ],
            "thesis",
        );
        assert_eq!(work.title, "thesis-final");
        assert_eq!(work.creators, vec!["Jane Doe", "John Smith"]);
        assert_eq!(work.keywords, vec!["transformers", "attention"]);

        assert!(
            embedded_work(&[("title", "untitled".to_string())], "x")
                .title
                .is_empty()
        );
        assert!(
            embedded_work(&[("title", "paper.pdf".to_string())], "paper")
                .title
                .is_empty()
        );
        assert_eq!(
            embedded_work(&[("author", "A. Vaswani and N. Shazeer".to_string())], "x").creators,
            vec!["A. Vaswani", "N. Shazeer"]
        );
    }
}
