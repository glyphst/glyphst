use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{Block, BlockKind, BlockRole, DocumentV1, PolicyDecisionSource, TranslationPolicy};

const STANDALONE_MIN_MARKERS: usize = 5;
const STANDALONE_MIN_DOMINANCE: f32 = 0.60;
const BORDERLINE_MIN_MARKERS: usize = 3;
const BORDERLINE_MIN_DOMINANCE: f32 = 0.35;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReferenceClassification {
    pub reference_block_ids: Vec<String>,
    pub borderline_page_indices: Vec<usize>,
}

/// Applies bibliography policy with document-wide context.
///
/// The pass is intentionally idempotent so callers can run it after native
/// extraction and again after OCR/native fusion.
pub fn annotate_reference_sections(document: &mut DocumentV1) -> ReferenceClassification {
    let mut classification = ReferenceClassification::default();
    let standalone = document
        .pages
        .iter()
        .map(classify_standalone_page)
        .collect::<Vec<_>>();
    let mut in_references = false;

    for (page_index, page) in document.pages.iter_mut().enumerate() {
        let page_classification = standalone[page_index];
        let page_contains_reference_heading = page
            .blocks
            .iter()
            .any(|block| is_reference_heading(&normalized_text(&block.content.text)));
        if page_classification == StandalonePageClassification::Borderline {
            classification.borderline_page_indices.push(page.page_index);
            page.metadata
                .insert("reference_review_candidate".to_string(), json!(true));
            for block in &mut page.blocks {
                if is_main_flow_text(block) && !block.policy.locked {
                    block
                        .metadata
                        .insert("reference_review_candidate".to_string(), json!(true));
                    block.policy.confidence = block.policy.confidence.min(0.79);
                    if !block
                        .tags
                        .iter()
                        .any(|tag| tag == "reference_review_candidate")
                    {
                        block.tags.push("reference_review_candidate".to_string());
                    }
                }
            }
        }

        let standalone_reference = page_classification == StandalonePageClassification::Reference;
        for block in &mut page.blocks {
            let text = normalized_text(&block.content.text);
            let reference_heading = is_reference_heading(&text);

            if in_references && is_reference_exit_heading(block, &text) {
                in_references = false;
            }

            if reference_heading {
                in_references = true;
                mark_reference(
                    block,
                    "deterministic_reference_heading",
                    &mut classification,
                );
                continue;
            }

            if (in_references || standalone_reference || page_contains_reference_heading)
                && is_probable_page_furniture(block, page.geometry.height)
            {
                mark_reference_page_furniture(block, page.geometry.height);
                continue;
            }

            if (in_references || standalone_reference) && is_main_flow_text(block) {
                let reason = if standalone_reference && !in_references {
                    "deterministic_standalone_reference_page"
                } else {
                    "deterministic_reference_section"
                };
                mark_reference(block, reason, &mut classification);
            }
        }
    }

    classification
}

fn mark_reference_page_furniture(block: &mut Block, page_height: f32) {
    if !matches!(
        block.role,
        BlockRole::Header | BlockRole::Footer | BlockRole::PageNumber
    ) {
        block.role = if block.bbox.y0 < page_height * 0.5 {
            BlockRole::Header
        } else {
            BlockRole::Footer
        };
    }
    block.policy = TranslationPolicy::locked_preserve(
        "deterministic_reference_page_furniture",
        PolicyDecisionSource::DeterministicRule,
        1.0,
    );
    if !block.tags.iter().any(|tag| tag == "page_furniture") {
        block.tags.push("page_furniture".to_string());
    }
}

fn is_probable_page_furniture(block: &Block, page_height: f32) -> bool {
    let characters = block
        .content
        .text
        .chars()
        .filter(|character| !character.is_whitespace())
        .count();
    if characters == 0 || characters > 180 {
        return false;
    }
    let short_line = block.content.lines.len() <= 2 && block.bbox.height() <= page_height * 0.035;
    short_line && (block.bbox.y0 < page_height * 0.11 || block.bbox.y1 > page_height * 0.94)
}

fn mark_reference(
    block: &mut Block,
    reason: &'static str,
    classification: &mut ReferenceClassification,
) {
    block.role = BlockRole::Reference;
    block.policy =
        TranslationPolicy::locked_preserve(reason, PolicyDecisionSource::DeterministicRule, 1.0);
    if !block.tags.iter().any(|tag| tag == "reference") {
        block.tags.push("reference".to_string());
    }
    if !classification
        .reference_block_ids
        .iter()
        .any(|id| id == &block.id)
    {
        classification.reference_block_ids.push(block.id.clone());
    }
}

fn is_main_flow_text(block: &Block) -> bool {
    block.kind == BlockKind::Text
        && !matches!(
            block.role,
            BlockRole::Header
                | BlockRole::Footer
                | BlockRole::PageNumber
                | BlockRole::Metadata
                | BlockRole::Image
                | BlockRole::Table
                | BlockRole::Formula
                | BlockRole::FormulaNumber
                | BlockRole::Code
        )
        && !block.tags.iter().any(|tag| tag == "protected")
}

fn normalized_text(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn is_reference_heading(text: &str) -> bool {
    matches!(
        text.trim_matches(|character: char| !character.is_alphanumeric())
            .to_ascii_lowercase()
            .as_str(),
        "references" | "bibliography"
    )
}

fn is_reference_exit_heading(block: &Block, text: &str) -> bool {
    if block.role != BlockRole::Heading || is_reference_heading(text) {
        return false;
    }
    let compact = text.trim();
    if compact.is_empty() || compact.chars().count() > 180 {
        return false;
    }
    let upper = compact.to_ascii_uppercase();
    if upper.starts_with("APPENDIX")
        || matches!(
            upper.as_str(),
            "ACKNOWLEDGMENTS" | "ACKNOWLEDGEMENTS" | "AUTHOR BIOGRAPHIES"
        )
    {
        return true;
    }
    let mut words = compact.split_whitespace();
    let first = words.next().unwrap_or_default();
    let has_title = words.next().is_some();
    has_title
        && (is_top_level_marker(first) || first.strip_suffix('.').is_some_and(is_top_level_marker))
}

fn is_top_level_marker(marker: &str) -> bool {
    (marker.len() == 1
        && marker
            .chars()
            .all(|character| character.is_ascii_uppercase()))
        || (!marker.is_empty()
            && marker.len() <= 3
            && marker.chars().all(|character| character.is_ascii_digit()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StandalonePageClassification {
    No,
    Borderline,
    Reference,
}

fn classify_standalone_page(page: &crate::Page) -> StandalonePageClassification {
    if page
        .blocks
        .iter()
        .any(|block| is_reference_heading(&normalized_text(&block.content.text)))
    {
        return StandalonePageClassification::No;
    }
    if page
        .blocks
        .iter()
        .any(|block| is_reference_exit_heading(block, &normalized_text(&block.content.text)))
    {
        return StandalonePageClassification::No;
    }

    let candidates = page
        .blocks
        .iter()
        .filter(|block| is_main_flow_text(block))
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return StandalonePageClassification::No;
    }

    let mut markers = Vec::new();
    let mut total_characters = 0usize;
    let mut reference_characters = 0usize;
    let mut families = BibliographyFamilies::default();
    for block in candidates {
        let evidence = reference_evidence(&block.content.text);
        let characters = block
            .content
            .text
            .chars()
            .filter(|character| !character.is_whitespace())
            .count();
        total_characters += characters;
        markers.extend(evidence.markers.iter().copied());
        families.merge(evidence.families);
        if evidence.markers.len() >= 2 && evidence.families.count() >= 2 {
            reference_characters += characters;
        }
    }

    let dominance = reference_characters as f32 / total_characters.max(1) as f32;
    let monotonic = mostly_monotonic(&markers);
    if markers.len() >= STANDALONE_MIN_MARKERS
        && dominance >= STANDALONE_MIN_DOMINANCE
        && families.count() >= 2
        && monotonic
    {
        StandalonePageClassification::Reference
    } else if markers.len() >= BORDERLINE_MIN_MARKERS
        && dominance >= BORDERLINE_MIN_DOMINANCE
        && families.count() >= 2
    {
        StandalonePageClassification::Borderline
    } else {
        StandalonePageClassification::No
    }
}

#[derive(Debug, Clone, Default)]
struct ReferenceEvidence {
    markers: Vec<u32>,
    families: BibliographyFamilies,
}

#[derive(Debug, Clone, Copy, Default)]
struct BibliographyFamilies {
    year: bool,
    doi: bool,
    arxiv: bool,
    url: bool,
    venue: bool,
}

impl BibliographyFamilies {
    fn count(self) -> usize {
        [self.year, self.doi, self.arxiv, self.url, self.venue]
            .into_iter()
            .filter(|present| *present)
            .count()
    }

    fn merge(&mut self, other: Self) {
        self.year |= other.year;
        self.doi |= other.doi;
        self.arxiv |= other.arxiv;
        self.url |= other.url;
        self.venue |= other.venue;
    }
}

fn reference_evidence(text: &str) -> ReferenceEvidence {
    let lower = text.to_ascii_lowercase();
    ReferenceEvidence {
        markers: bracketed_numbers(text),
        families: BibliographyFamilies {
            year: contains_publication_year(text),
            doi: lower.contains("doi:") || lower.contains("doi.org/"),
            arxiv: lower.contains("arxiv:"),
            url: lower.contains("http://") || lower.contains("https://"),
            venue: [
                "proceedings of",
                "conference on",
                "journal of",
                "transactions on",
                "commun. acm",
                "association for computational linguistics",
            ]
            .iter()
            .any(|needle| lower.contains(needle)),
        },
    }
}

fn bracketed_numbers(text: &str) -> Vec<u32> {
    let bytes = text.as_bytes();
    let mut values = Vec::new();
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] != b'[' {
            index += 1;
            continue;
        }
        let start = index + 1;
        let mut end = start;
        while end < bytes.len() && bytes[end].is_ascii_digit() && end - start < 6 {
            end += 1;
        }
        if end > start
            && end < bytes.len()
            && bytes[end] == b']'
            && text[text[..index].rfind('\n').map_or(0, |newline| newline + 1)..index]
                .chars()
                .all(char::is_whitespace)
            && let Ok(value) = text[start..end].parse()
        {
            values.push(value);
            index = end + 1;
            continue;
        }
        index += 1;
    }
    values
}

fn contains_publication_year(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.windows(4).any(|window| {
        window.iter().all(u8::is_ascii_digit)
            && matches!(window[0], b'1' | b'2')
            && match window[0] {
                b'1' => window[1] == b'9',
                b'2' => window[1] <= b'1',
                _ => false,
            }
    })
}

fn mostly_monotonic(markers: &[u32]) -> bool {
    if markers.len() < 2 {
        return false;
    }
    let monotonic_pairs = markers
        .windows(2)
        .filter(|pair| pair[1] > pair[0] && pair[1] - pair[0] <= 5)
        .count();
    monotonic_pairs * 10 >= (markers.len() - 1) * 7
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::{
        BlockContent, BoundingBox, DocumentMetadata, Page, PageGeometry, SourceProvenance,
    };

    fn block(id: &str, page_index: usize, role: BlockRole, text: &str) -> Block {
        Block {
            id: id.to_string(),
            page_index,
            order: 0,
            reading_order: 0,
            kind: BlockKind::Text,
            role,
            bbox: BoundingBox::new(10.0, 20.0, 190.0, 180.0),
            content: BlockContent {
                text: text.to_string(),
                ..BlockContent::default()
            },
            policy: TranslationPolicy::candidate(
                true,
                "fixture",
                PolicyDecisionSource::NativeHeuristic,
                0.9,
            ),
            continuation: None,
            structure: None,
            toc_entry: None,
            source: SourceProvenance {
                provider: "fixture".to_string(),
                raw_label: "text".to_string(),
                raw_bbox: BoundingBox::new(10.0, 20.0, 190.0, 180.0),
                raw_path: None,
            },
            tags: Vec::new(),
            metadata: BTreeMap::new(),
        }
    }

    fn page(page_index: usize, blocks: Vec<Block>) -> Page {
        Page {
            page_index,
            geometry: PageGeometry::new(200.0, 200.0),
            blocks,
            inline_objects: Vec::new(),
            protected_regions: Vec::new(),
            metadata: BTreeMap::new(),
        }
    }

    fn document(pages: Vec<Page>) -> DocumentV1 {
        DocumentV1::new(
            DocumentMetadata {
                document_id: "fixture".to_string(),
                source_path: "fixture.pdf".into(),
                provider: "fixture".to_string(),
                provider_version: "1".to_string(),
                extra: BTreeMap::new(),
            },
            pages,
        )
    }

    #[test]
    fn reference_section_crosses_pages_and_stops_at_appendix() {
        let mut header = block(
            "header",
            0,
            BlockRole::Body,
            "Paper title, Author One, Author Two",
        );
        header.bbox = BoundingBox::new(10.0, 2.0, 190.0, 7.0);
        let mut document = document(vec![
            page(
                0,
                vec![
                    header,
                    block("heading", 0, BlockRole::Heading, "REFERENCES"),
                    block("reference-one", 0, BlockRole::Body, "[1] A. Author. 2020."),
                ],
            ),
            page(
                1,
                vec![block(
                    "reference-two",
                    1,
                    BlockRole::Body,
                    "[2] B. Author. 2021.",
                )],
            ),
            page(
                2,
                vec![
                    block(
                        "reference-three",
                        2,
                        BlockRole::Body,
                        "[3] C. Author. 2022.",
                    ),
                    block(
                        "appendix",
                        2,
                        BlockRole::Heading,
                        "A ARCHITECTURE OPTIMIZATIONS",
                    ),
                    block(
                        "appendix-body",
                        2,
                        BlockRole::Body,
                        "Translate this appendix.",
                    ),
                ],
            ),
        ]);

        annotate_reference_sections(&mut document);

        assert_eq!(document.pages[0].blocks[0].role, BlockRole::Header);
        assert!(!document.pages[0].blocks[0].policy.translate);
        for id in [
            "heading",
            "reference-one",
            "reference-two",
            "reference-three",
        ] {
            let block = document
                .pages
                .iter()
                .flat_map(|page| &page.blocks)
                .find(|block| block.id == id)
                .unwrap();
            assert_eq!(block.role, BlockRole::Reference);
            assert!(block.policy.locked);
            assert!(!block.policy.translate);
        }
        assert!(document.pages[2].blocks[1].policy.translate);
        assert!(document.pages[2].blocks[2].policy.translate);
    }

    #[test]
    fn standalone_numbered_bibliography_is_preserved() {
        let references = (21..=28)
            .map(|number| {
                format!(
                    "[{number}] A. Author. 2023. Paper title. In Proceedings of Example. https://doi.org/10.1/{number}"
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        let mut header = block("header", 0, BlockRole::Header, "Paper title, April 2023");
        header.bbox = BoundingBox::new(10.0, 2.0, 190.0, 7.0);
        let mut document = document(vec![page(
            0,
            vec![
                header,
                block("references", 0, BlockRole::Dialogue, &references),
            ],
        )]);

        annotate_reference_sections(&mut document);

        assert_eq!(document.pages[0].blocks[0].role, BlockRole::Header);
        assert!(!document.pages[0].blocks[0].policy.translate);
        assert_eq!(document.pages[0].blocks[1].role, BlockRole::Reference);
        assert!(!document.pages[0].blocks[1].policy.translate);
    }

    #[test]
    fn numbered_prose_is_not_a_standalone_bibliography() {
        let text = "[1] Install the program.\n[2] Open it.\n[3] Choose a file.\n[4] Click run.\n[5] Review the result.";
        let mut document = document(vec![page(
            0,
            vec![block("instructions", 0, BlockRole::Body, text)],
        )]);

        annotate_reference_sections(&mut document);

        assert_eq!(document.pages[0].blocks[0].role, BlockRole::Body);
        assert!(document.pages[0].blocks[0].policy.translate);
    }

    #[test]
    fn inline_citations_do_not_make_a_standalone_bibliography() {
        let text = "Prior work [1] introduced the system, [2] evaluated it, and [3] extended it. A 2023 conference paper [4] and the proceedings of another venue [5] are discussed at https://example.test.";
        let mut document = document(vec![page(
            0,
            vec![block("related-work", 0, BlockRole::Body, text)],
        )]);

        annotate_reference_sections(&mut document);

        assert_eq!(document.pages[0].blocks[0].role, BlockRole::Body);
        assert!(document.pages[0].blocks[0].policy.translate);
    }
}
