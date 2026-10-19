//! Telling whether a new item is one the library already has, by Zotero's
//! rules: the same DOI, ISBN or arXiv id is the same work and a different
//! one is not; otherwise the same title, a year within one, and a shared
//! creator surname.

use glyphst_library::{DocumentId, LibraryItem};
use unicode_normalization::UnicodeNormalization as _;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DuplicateReason {
    Doi,
    Isbn,
    Arxiv,
    Title,
}

/// The first item in `items` that `candidate` duplicates, and why.
pub fn find_duplicate<'a>(
    candidate: &LibraryItem,
    items: impl IntoIterator<Item = &'a LibraryItem>,
) -> Option<(DocumentId, DuplicateReason)> {
    let title = normalize_title(&candidate.title);
    items
        .into_iter()
        .filter(|item| item.id != candidate.id)
        .find_map(|item| duplicate_reason(candidate, &title, item).map(|reason| (item.id, reason)))
}

fn duplicate_reason(
    candidate: &LibraryItem,
    candidate_title: &str,
    item: &LibraryItem,
) -> Option<DuplicateReason> {
    let left = &candidate.identifiers;
    let right = &item.identifiers;
    for (left, right, reason) in [
        (&left.doi, &right.doi, DuplicateReason::Doi),
        (&left.isbn, &right.isbn, DuplicateReason::Isbn),
        (&left.arxiv, &right.arxiv, DuplicateReason::Arxiv),
    ] {
        if let (Some(left), Some(right)) = (left, right) {
            return left.eq_ignore_ascii_case(right).then_some(reason);
        }
    }
    if candidate_title.is_empty() || candidate_title != normalize_title(&item.title) {
        return None;
    }
    if let (Some(left), Some(right)) = (candidate.year, item.year)
        && (left - right).abs() > 1
    {
        return None;
    }
    if !candidate.creators.is_empty() && !item.creators.is_empty() {
        let surnames = item
            .creators
            .iter()
            .map(|creator| surname(&creator.name))
            .collect::<Vec<_>>();
        if !candidate
            .creators
            .iter()
            .any(|creator| surnames.contains(&surname(&creator.name)))
        {
            return None;
        }
    }
    Some(DuplicateReason::Title)
}

/// A title folded for comparison: accents and case dropped, punctuation
/// gone, a leading article removed.
pub fn normalize_title(title: &str) -> String {
    let folded = title
        .nfkd()
        .filter(|c| !unicode_normalization::char::is_combining_mark(*c))
        .flat_map(char::to_lowercase)
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>();
    let mut words = folded.split_whitespace().collect::<Vec<_>>();
    if words.len() > 1 && matches!(words[0], "a" | "an" | "the") {
        words.remove(0);
    }
    words.join(" ")
}

/// A name's surname, folded: `Vaswani` from `Ashish Vaswani` or
/// `Vaswani, Ashish`.
pub fn surname(name: &str) -> String {
    let name = name.trim();
    let last = match name.split_once(',') {
        Some((last, _)) => last,
        None => name.split_whitespace().last().unwrap_or(name),
    };
    normalize_title(last)
}

#[cfg(test)]
mod tests {
    use glyphst_library::{
        LibraryCreator, LibraryCreatorRole, LibraryIdentifiers, LibraryItemKind, ReadingStatus,
    };

    use super::*;

    fn item(id: u64, title: &str, year: Option<i32>, creators: &[&str]) -> LibraryItem {
        LibraryItem {
            id: DocumentId(id),
            kind: LibraryItemKind::Paper,
            title: title.to_string(),
            creators: creators
                .iter()
                .map(|name| LibraryCreator {
                    name: name.to_string(),
                    role: LibraryCreatorRole::Author,
                })
                .collect(),
            year,
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
    fn the_same_identifier_is_a_duplicate_and_a_different_one_is_not() {
        let mut existing = item(1, "Attention Is All You Need", Some(2017), &["Vaswani"]);
        existing.identifiers.doi = Some("10.5555/3295222.3295349".to_string());
        let mut candidate = item(2, "Something else entirely", None, &[]);
        candidate.identifiers.doi = Some("10.5555/3295222.3295349".to_uppercase());
        assert_eq!(
            find_duplicate(&candidate, [&existing]),
            Some((DocumentId(1), DuplicateReason::Doi))
        );

        let mut same_title = item(3, "Attention Is All You Need", Some(2017), &["Vaswani"]);
        same_title.identifiers.doi = Some("10.1/other".to_string());
        assert_eq!(find_duplicate(&same_title, [&existing]), None);
    }

    #[test]
    fn titles_match_after_folding_within_a_year_and_a_shared_surname() {
        let existing = item(
            1,
            "The Élements of Statistical Learning",
            Some(2009),
            &["Trevor Hastie", "Robert Tibshirani"],
        );
        let candidate = item(
            2,
            "Elements of statistical learning.",
            Some(2008),
            &["Hastie, T."],
        );
        assert_eq!(
            find_duplicate(&candidate, [&existing]),
            Some((DocumentId(1), DuplicateReason::Title))
        );
        let later = item(
            3,
            "Elements of Statistical Learning",
            Some(2016),
            &["Hastie"],
        );
        assert_eq!(find_duplicate(&later, [&existing]), None);
        let other_authors = item(
            4,
            "Elements of Statistical Learning",
            Some(2009),
            &["Smith"],
        );
        assert_eq!(find_duplicate(&other_authors, [&existing]), None);
        let no_authors = item(5, "elements of statistical learning", None, &[]);
        assert_eq!(
            find_duplicate(&no_authors, [&existing]),
            Some((DocumentId(1), DuplicateReason::Title))
        );
    }
}
