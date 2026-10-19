//! Finding a document's own DOI, arXiv id and ISBN in its first pages,
//! its file name and its info entries.

use std::sync::LazyLock;

use regex::Regex;

/// The identifiers found for one document.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Identifiers {
    /// Lowercase, without a `doi:` or URL prefix.
    pub doi: Option<String>,
    /// Without a version: `1706.03762` or `hep-th/9901001`.
    pub arxiv: Option<String>,
    /// As ISBN-13 digits.
    pub isbn: Option<String>,
}

impl Identifiers {
    pub fn is_empty(&self) -> bool {
        self.doi.is_none() && self.arxiv.is_none() && self.isbn.is_none()
    }
}

static DOI: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)\b(10\.\d{4,9}/[^\s"<>{}|\\^`]+)"#).unwrap());
static ARXIV_NEW: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:\barxiv\s*:\s*|arxiv\.org/(?:abs|pdf)/)(\d{4})\.(\d{4,5})(?:v\d+)?\b")
        .unwrap()
});
static ARXIV_OLD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(?:\barxiv\s*:\s*|arxiv\.org/(?:abs|pdf)/)([a-z][a-z\-]+(?:\.[a-z]{2})?/\d{7})(?:v\d+)?\b",
    )
    .unwrap()
});
static ARXIV_FILE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(\d{4})\.(\d{4,5})(?:v\d+)?$").unwrap());
static ISBN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\bISBN(?:-1[03])?\s*(?:\(\w+\))?\s*:?\s*([0-9][0-9\- ]{8,16}[0-9X])\b")
        .unwrap()
});
static REFERENCES: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?im)^\s*(?:\d+\.?\s+)?(?:references|bibliography)\s*$").unwrap()
});

/// Looks for the document's identifiers.
///
/// `pages` is the text of its first pages: a DOI or arXiv id is taken from
/// the first two, stopping at a references heading so a cited paper's is
/// not mistaken for its own; an ISBN from any of them (a book prints it on
/// its copyright page). `file_stem` catches arXiv's own file names
/// (`1706.03762v7`) and `info` the DOI some publishers put in the subject.
pub fn extract(pages: &[String], file_stem: &str, info: &[&str]) -> Identifiers {
    let head = pages
        .iter()
        .take(2)
        .map(|page| before_references(page))
        .collect::<Vec<_>>()
        .join("\n");
    let info = info.join("\n");
    let mut found = Identifiers {
        arxiv: find_arxiv(&head)
            .or_else(|| find_arxiv(&info))
            .or_else(|| arxiv_from_file_stem(file_stem)),
        doi: find_doi(&head).or_else(|| find_doi(&info)),
        isbn: pages
            .iter()
            .map(|page| before_references(page))
            .find_map(find_isbn)
            .or_else(|| find_isbn(&info)),
    };
    // arXiv's own DOIs name the preprint.
    if found.arxiv.is_none()
        && let Some(doi) = &found.doi
        && let Some(id) = doi.strip_prefix("10.48550/arxiv.")
        && is_valid_new_arxiv(id)
    {
        found.arxiv = Some(id.to_string());
    }
    found
}

fn before_references(text: &str) -> &str {
    match REFERENCES.find(text) {
        Some(heading) => &text[..heading.start()],
        None => text,
    }
}

/// The first DOI in `text`, trimmed of the punctuation around it.
pub fn find_doi(text: &str) -> Option<String> {
    DOI.captures_iter(text)
        .filter_map(|captures| captures.get(1))
        .map(|doi| trim_doi(doi.as_str()))
        .find(|doi| doi.len() > 8)
}

fn trim_doi(raw: &str) -> String {
    let mut doi = raw.to_string();
    while let Some(last) = doi.chars().last() {
        let unbalanced = |open: char, close: char| {
            last == close && doi.matches(open).count() < doi.matches(close).count()
        };
        if matches!(last, '.' | ',' | ';' | ':' | '\'' | '"' | '>')
            || unbalanced('(', ')')
            || unbalanced('[', ']')
        {
            doi.pop();
        } else {
            break;
        }
    }
    doi.to_lowercase()
}

/// The first arXiv id in `text`, without its version.
pub fn find_arxiv(text: &str) -> Option<String> {
    ARXIV_NEW
        .captures_iter(text)
        .find_map(|captures| {
            let id = format!("{}.{}", &captures[1], &captures[2]);
            is_valid_new_arxiv(&id).then_some(id)
        })
        .or_else(|| {
            ARXIV_OLD
                .captures(text)
                .map(|captures| captures[1].to_lowercase())
        })
}

fn arxiv_from_file_stem(stem: &str) -> Option<String> {
    let captures = ARXIV_FILE.captures(stem.trim())?;
    let id = format!("{}.{}", &captures[1], &captures[2]);
    is_valid_new_arxiv(&id).then_some(id)
}

/// A new-style id has a real month, four digits after the dot before
/// 2015 and five from then on.
fn is_valid_new_arxiv(id: &str) -> bool {
    let Some((yymm, number)) = id.split_once('.') else {
        return false;
    };
    let (Ok(yymm_value), true) = (yymm.parse::<u32>(), yymm.len() == 4) else {
        return false;
    };
    let month = yymm_value % 100;
    if !(1..=12).contains(&month) || !number.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    match number.len() {
        4 => yymm_value < 1501,
        5 => yymm_value >= 1501,
        _ => false,
    }
}

/// The first valid ISBN in `text`, as ISBN-13.
pub fn find_isbn(text: &str) -> Option<String> {
    ISBN.captures_iter(text).find_map(|captures| {
        let digits = captures[1]
            .chars()
            .filter(|c| c.is_ascii_digit() || *c == 'X' || *c == 'x')
            .collect::<String>()
            .to_uppercase();
        normalize_isbn(&digits)
    })
}

/// An ISBN-10 or ISBN-13 with a valid check digit, as ISBN-13.
pub fn normalize_isbn(raw: &str) -> Option<String> {
    let digits = raw
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == 'X' || *c == 'x')
        .collect::<String>()
        .to_uppercase();
    match digits.len() {
        10 => {
            let sum = digits
                .chars()
                .enumerate()
                .try_fold(0_u32, |sum, (index, c)| {
                    let value = match c {
                        'X' if index == 9 => 10,
                        c => c.to_digit(10)?,
                    };
                    Some(sum + value * (10 - index as u32))
                })?;
            (sum % 11 == 0).then(|| isbn13_from_isbn10(&digits[..9]))
        }
        13 if digits.starts_with("978") || digits.starts_with("979") => {
            let values = digits
                .chars()
                .map(|c| c.to_digit(10))
                .collect::<Option<Vec<_>>>()?;
            let sum = values
                .iter()
                .enumerate()
                .map(|(index, value)| if index % 2 == 0 { *value } else { value * 3 })
                .sum::<u32>();
            (sum % 10 == 0).then_some(digits)
        }
        _ => None,
    }
}

fn isbn13_from_isbn10(first_nine: &str) -> String {
    let body = format!("978{first_nine}");
    let sum = body
        .chars()
        .filter_map(|c| c.to_digit(10))
        .enumerate()
        .map(|(index, value)| if index % 2 == 0 { value } else { value * 3 })
        .sum::<u32>();
    format!("{body}{}", (10 - sum % 10) % 10)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pages(texts: &[&str]) -> Vec<String> {
        texts.iter().map(|text| text.to_string()).collect()
    }

    #[test]
    fn dois_lose_surrounding_punctuation_and_keep_balanced_brackets() {
        assert_eq!(
            find_doi("See https://doi.org/10.1145/3292500.3330701)."),
            Some("10.1145/3292500.3330701".to_string())
        );
        assert_eq!(
            find_doi("doi:10.1016/S0140-6736(20)30183-5, accessed"),
            Some("10.1016/s0140-6736(20)30183-5".to_string())
        );
        assert_eq!(find_doi("no identifier here"), None);
    }

    #[test]
    fn arxiv_ids_check_their_month_and_digit_count() {
        assert_eq!(
            find_arxiv("arXiv:1706.03762v7 [cs.CL] 6 Dec 2017"),
            Some("1706.03762".to_string())
        );
        assert_eq!(
            find_arxiv("https://arxiv.org/abs/0704.0001"),
            Some("0704.0001".to_string())
        );
        // Month 13, and five digits before 2015.
        assert_eq!(find_arxiv("arXiv:1713.03762"), None);
        assert_eq!(find_arxiv("arXiv:1406.03762"), None);
        assert_eq!(
            find_arxiv("arXiv:hep-th/9901001v2"),
            Some("hep-th/9901001".to_string())
        );
        assert_eq!(
            find_arxiv("arXiv:math.GT/0309136"),
            Some("math.gt/0309136".to_string())
        );
    }

    #[test]
    fn isbns_need_a_valid_check_digit_and_become_isbn_13() {
        assert_eq!(
            find_isbn("ISBN 0-262-51087-1 (paperback)"),
            Some("9780262510875".to_string())
        );
        assert_eq!(
            find_isbn("ISBN-13: 978-1-4493-7332-0"),
            Some("9781449373320".to_string())
        );
        assert_eq!(find_isbn("ISBN 0-262-51087-2"), None);
        assert_eq!(
            normalize_isbn("080442957X"),
            Some("9780804429573".to_string())
        );
        assert_eq!(normalize_isbn("9781449373321"), None);
    }

    #[test]
    fn a_cited_papers_identifier_after_the_references_heading_is_ignored() {
        let found = extract(
            &pages(&[
                "A Paper\nIntroduction\n...\nReferences\n[1] Vaswani, arXiv:1706.03762, doi:10.5555/3295222.3295349",
            ]),
            "a-paper",
            &[],
        );
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn the_file_name_info_and_arxiv_dois_fill_in() {
        let found = extract(&pages(&["no ids in the text"]), "2205.14135v2", &[]);
        assert_eq!(found.arxiv.as_deref(), Some("2205.14135"));

        let found = extract(&pages(&[""]), "paper", &["doi:10.1016/j.cell.2020.01.001"]);
        assert_eq!(found.doi.as_deref(), Some("10.1016/j.cell.2020.01.001"));

        let found = extract(&pages(&["DOI 10.48550/arXiv.2510.19315"]), "paper", &[]);
        assert_eq!(found.arxiv.as_deref(), Some("2510.19315"));
    }

    #[test]
    fn a_books_isbn_is_found_on_its_copyright_page() {
        let found = extract(
            &pages(&[
                "Designing Data-Intensive Applications",
                "Martin Kleppmann",
                "Copyright © 2017\nISBN: 978-1-449-37332-0",
            ]),
            "ddia",
            &[],
        );
        assert_eq!(found.isbn.as_deref(), Some("9781449373320"));
    }
}
