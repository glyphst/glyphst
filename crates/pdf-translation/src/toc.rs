use serde_json::json;

use crate::native::policy_for_role;
use crate::{
    Block, BlockKind, BlockRole, BlockStructure, BoundingBox, Page, PolicyDecisionSource,
    StructureKind, TableOfContentsEntry, TableOfContentsLeader, TextFlow,
};

const TOC_STRUCTURE_CONFIDENCE: f32 = 0.97;

pub(crate) fn annotate_table_of_contents_page(page: &mut Page) {
    let candidates = page
        .blocks
        .iter()
        .enumerate()
        .filter(|(_, block)| {
            block.kind == BlockKind::Text
                && !block.policy.locked
                && !matches!(
                    block.role,
                    BlockRole::Header
                        | BlockRole::Footer
                        | BlockRole::PageNumber
                        | BlockRole::Reference
                )
        })
        .filter_map(|(index, block)| {
            parse_table_of_contents_entry(&block.content.text).map(|entry| {
                let page_label_right = page_label_right(block, &entry);
                (index, entry, page_label_right)
            })
        })
        .collect::<Vec<_>>();
    if candidates.len() < 3 {
        return;
    }

    let explicit_heading = page
        .blocks
        .iter()
        .any(|block| is_table_of_contents_heading(&block.content.text));
    let mut right_edges = candidates
        .iter()
        .map(|(_, _, right)| *right)
        .filter(|right| right.is_finite())
        .collect::<Vec<_>>();
    right_edges.sort_by(f32::total_cmp);
    let Some(median_right) = right_edges.get(right_edges.len() / 2).copied() else {
        return;
    };
    let alignment_tolerance = (page.geometry.width * 0.025).clamp(4.0, 16.0);
    let aligned = candidates
        .iter()
        .filter(|(_, _, right)| (*right - median_right).abs() <= alignment_tolerance)
        .count();
    let leaders = candidates
        .iter()
        .filter(|(_, entry, _)| entry.leader.is_some())
        .count();
    let markers = candidates
        .iter()
        .filter(|(_, entry, _)| entry.marker.is_some())
        .count();
    let enough_aligned_rows = aligned >= 4 && aligned * 4 >= candidates.len() * 3;
    let heading_supported_rows = explicit_heading && aligned >= 3;
    if !heading_supported_rows && !(enough_aligned_rows && (leaders >= 2 || markers >= 3)) {
        return;
    }

    let group_bbox = candidates
        .iter()
        .filter(|(_, _, right)| (*right - median_right).abs() <= alignment_tolerance)
        .map(|(index, _, _)| page.blocks[*index].bbox)
        .reduce(BoundingBox::union);
    let group_id = format!("p{:03}-toc", page.page_index + 1);
    for (index, entry, right) in candidates {
        if (right - median_right).abs() > alignment_tolerance {
            continue;
        }
        let block = &mut page.blocks[index];
        block.role = BlockRole::TableOfContents;
        block.policy = policy_for_role(BlockRole::TableOfContents);
        block.policy.source = if block.source.provider.contains("mupdf") {
            PolicyDecisionSource::NativeHeuristic
        } else {
            PolicyDecisionSource::OcrSemantic
        };
        block.policy.confidence = TOC_STRUCTURE_CONFIDENCE;
        let provider = block
            .structure
            .as_ref()
            .map(|structure| structure.provider.clone())
            .unwrap_or_else(|| block.source.provider.clone());
        block.structure = Some(BlockStructure {
            kind: StructureKind::TableOfContentsEntry,
            flow: TextFlow::Reflow,
            group_id: Some(group_id.clone()),
            group_bbox,
            marker: entry.marker.clone(),
            provider,
            confidence: TOC_STRUCTURE_CONFIDENCE,
        });
        block.toc_entry = Some(entry);
        if !block.tags.iter().any(|tag| tag == "structured_toc_entry") {
            block.tags.push("structured_toc_entry".to_string());
        }
        block
            .metadata
            .insert("layout_paradigm".to_string(), json!("table_of_contents"));
    }
    page.metadata
        .insert("layout_paradigm".to_string(), json!("table_of_contents"));
}

pub(crate) fn parse_table_of_contents_entry(text: &str) -> Option<TableOfContentsEntry> {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let lines = normalized
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    if lines.is_empty() {
        return None;
    }

    let (content, page_label) =
        if lines.len() > 1 && looks_like_page_label(lines.last().copied().unwrap_or_default()) {
            (
                lines[..lines.len() - 1].join(" "),
                lines.last().copied().unwrap_or_default().to_string(),
            )
        } else {
            split_terminal_page_label(&lines.join(" "))?
        };
    let (without_leader, leader) = strip_dot_leader(&content);
    let (marker, title) = split_section_marker(&without_leader);
    let title = title.trim().trim_matches(['.', '·', '…']).trim();
    if title.is_empty()
        || title.chars().count() > 320
        || !title.chars().any(|character| character.is_alphabetic())
    {
        return None;
    }
    Some(TableOfContentsEntry {
        marker,
        title: title.to_string(),
        page_label,
        leader,
    })
}

pub(crate) fn semantic_member_bbox(
    group_bbox: BoundingBox,
    member_index: usize,
    member_count: usize,
) -> BoundingBox {
    let member_count = member_count.max(1);
    let slice_height = group_bbox.height() / member_count as f32;
    let slice_y0 = group_bbox.y0 + slice_height * member_index as f32;
    let slice_y1 = if member_index + 1 == member_count {
        group_bbox.y1
    } else {
        slice_y0 + slice_height
    };
    let padding = (slice_height * 0.08).min(1.5);
    BoundingBox::new(
        group_bbox.x0,
        slice_y0 + padding,
        group_bbox.x1,
        (slice_y1 - padding).max(slice_y0 + padding + 0.5),
    )
}

fn page_label_right(block: &Block, entry: &TableOfContentsEntry) -> f32 {
    block
        .content
        .lines
        .iter()
        .rev()
        .find(|line| line.text.trim() == entry.page_label)
        .map_or(block.bbox.x1, |line| line.bbox.x1)
}

fn split_terminal_page_label(text: &str) -> Option<(String, String)> {
    let trimmed = text.trim();
    let split_at = trimmed.rfind(char::is_whitespace)?;
    let (content, suffix) = trimmed.split_at(split_at);
    let page_label = suffix.trim();
    if content.trim().is_empty() || !looks_like_page_label(page_label) {
        return None;
    }
    Some((content.trim().to_string(), page_label.to_string()))
}

fn looks_like_page_label(value: &str) -> bool {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > 9 {
        return false;
    }
    if value.chars().all(|character| character.is_ascii_digit()) {
        return true;
    }
    let lowercase = value.to_ascii_lowercase();
    if lowercase
        .chars()
        .all(|character| matches!(character, 'i' | 'v' | 'x' | 'l' | 'c' | 'd' | 'm'))
    {
        return true;
    }
    if let Some((left, right)) = value.split_once(['-', '–', '—']) {
        return !left.is_empty()
            && !right.is_empty()
            && left.chars().all(|character| character.is_ascii_digit())
            && right.chars().all(|character| character.is_ascii_digit());
    }
    let prefix = value
        .chars()
        .take_while(|character| character.is_ascii_alphabetic())
        .count();
    (1..=2).contains(&prefix)
        && value.chars().skip(prefix).count() >= 1
        && value
            .chars()
            .skip(prefix)
            .all(|character| character.is_ascii_digit())
}

fn split_section_marker(text: &str) -> (Option<String>, &str) {
    let trimmed = text.trim_start();
    let Some(split_at) = trimmed.find(char::is_whitespace) else {
        return (None, trimmed);
    };
    let token = &trimmed[..split_at];
    let marker = token.trim_end_matches(['.', ')', '）', '．', '、']);
    let valid = !marker.is_empty()
        && marker.len() <= 16
        && marker.split('.').all(|part| {
            !part.is_empty()
                && part.len() <= 4
                && part.chars().all(|character| character.is_ascii_digit())
        });
    if valid {
        (Some(marker.to_string()), trimmed[split_at..].trim_start())
    } else {
        (None, trimmed)
    }
}

fn strip_dot_leader(text: &str) -> (String, Option<TableOfContentsLeader>) {
    let characters = text.chars().collect::<Vec<_>>();
    let mut start = characters.len();
    let mut dots = 0usize;
    let mut whitespace = false;
    while start > 0 {
        let character = characters[start - 1];
        if matches!(character, '.' | '·' | '…') {
            dots += 1;
            start -= 1;
        } else if character.is_whitespace() {
            whitespace = true;
            start -= 1;
        } else {
            break;
        }
    }
    if dots < 3 || start == 0 {
        return (text.trim().to_string(), None);
    }
    let leader_text = &characters[start..];
    let glyph = if leader_text.contains(&'…') {
        "…"
    } else if leader_text.contains(&'·') {
        "·"
    } else {
        "."
    };
    (
        characters[..start]
            .iter()
            .collect::<String>()
            .trim_end()
            .to_string(),
        Some(TableOfContentsLeader {
            glyph: glyph.to_string(),
            gap_em: if whitespace { 0.32 } else { 0.08 },
        }),
    )
}

fn is_table_of_contents_heading(text: &str) -> bool {
    let normalized = text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim_matches([':', '：'])
        .to_lowercase();
    matches!(
        normalized.as_str(),
        "contents"
            | "table of contents"
            | "content"
            | "目录"
            | "目次"
            | "차례"
            | "목차"
            | "inhalt"
            | "table des matières"
    )
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::{
        BlockContent, FontFamilyClass, Line, PageGeometry, Segment, SegmentKind, SourceProvenance,
        TextStyle, TranslationPolicy,
    };

    #[test]
    fn parses_native_and_ocr_toc_rows() {
        let native =
            parse_table_of_contents_entry("2.1\nPipeline-based Framework. . . . . . . . .\n4")
                .unwrap();
        assert_eq!(native.marker.as_deref(), Some("2.1"));
        assert_eq!(native.title, "Pipeline-based Framework");
        assert_eq!(native.page_label, "4");
        assert_eq!(native.leader.unwrap().glyph, ".");

        let ocr = parse_table_of_contents_entry("7 Limitation and Future Work 11").unwrap();
        assert_eq!(ocr.marker.as_deref(), Some("7"));
        assert_eq!(ocr.title, "Limitation and Future Work");
        assert_eq!(ocr.page_label, "11");
        assert!(ocr.leader.is_none());
    }

    #[test]
    fn detects_a_toc_page_without_using_a_page_identifier() {
        let mut page = Page {
            page_index: 4,
            geometry: PageGeometry::new(600.0, 800.0),
            blocks: vec![
                test_block(
                    "heading",
                    "Contents",
                    BoundingBox::new(70.0, 40.0, 150.0, 55.0),
                ),
                test_block(
                    "one",
                    "1 Introduction\n3",
                    BoundingBox::new(70.0, 80.0, 530.0, 92.0),
                ),
                test_block(
                    "two",
                    "2.1\nRelated Work. . . . .\n4",
                    BoundingBox::new(85.0, 100.0, 530.0, 112.0),
                ),
                test_block(
                    "three",
                    "2.2\nMethod. . . . .\n5",
                    BoundingBox::new(85.0, 120.0, 530.0, 132.0),
                ),
                test_block(
                    "four",
                    "3 Conclusion\n8",
                    BoundingBox::new(70.0, 140.0, 530.0, 152.0),
                ),
            ],
            inline_objects: Vec::new(),
            protected_regions: Vec::new(),
            metadata: BTreeMap::new(),
        };

        annotate_table_of_contents_page(&mut page);

        assert_eq!(
            page.blocks
                .iter()
                .filter(|block| block.toc_entry.is_some())
                .count(),
            4
        );
        assert!(page.blocks.iter().skip(1).all(|block| {
            block.role == BlockRole::TableOfContents
                && block
                    .structure
                    .as_ref()
                    .is_some_and(|structure| structure.kind == StructureKind::TableOfContentsEntry)
        }));
    }

    #[test]
    fn does_not_classify_unaligned_numbered_prose_as_a_toc() {
        let mut page = Page {
            page_index: 0,
            geometry: PageGeometry::new(600.0, 800.0),
            blocks: vec![
                test_block(
                    "one",
                    "1 First argument 3",
                    BoundingBox::new(70.0, 80.0, 210.0, 92.0),
                ),
                test_block(
                    "two",
                    "2 Second argument 4",
                    BoundingBox::new(70.0, 110.0, 350.0, 122.0),
                ),
                test_block(
                    "three",
                    "3 Third argument 5",
                    BoundingBox::new(70.0, 140.0, 470.0, 152.0),
                ),
                test_block(
                    "four",
                    "4 Fourth argument 6",
                    BoundingBox::new(70.0, 170.0, 540.0, 182.0),
                ),
            ],
            inline_objects: Vec::new(),
            protected_regions: Vec::new(),
            metadata: BTreeMap::new(),
        };

        annotate_table_of_contents_page(&mut page);

        assert!(page.blocks.iter().all(|block| block.toc_entry.is_none()));
    }

    fn test_block(id: &str, text: &str, bbox: BoundingBox) -> Block {
        let line = Line {
            bbox,
            text: text.to_string(),
            segments: vec![Segment {
                kind: SegmentKind::Text,
                text: text.to_string(),
                bbox,
                style: TextStyle {
                    font_class: Some(FontFamilyClass::Serif),
                    font_size: Some(10.0),
                    ..TextStyle::default()
                },
            }],
            baseline_y: None,
            line_height: Some(bbox.height()),
        };
        Block {
            id: id.to_string(),
            page_index: 0,
            order: 0,
            reading_order: 0,
            kind: BlockKind::Text,
            role: BlockRole::Body,
            bbox,
            content: BlockContent {
                text: text.to_string(),
                lines: vec![line.clone()],
                segments: line.segments,
                markdown: None,
                html: None,
            },
            policy: TranslationPolicy::candidate(
                true,
                "test",
                PolicyDecisionSource::NativeHeuristic,
                1.0,
            ),
            continuation: None,
            structure: None,
            toc_entry: None,
            source: SourceProvenance {
                provider: "mupdf".to_string(),
                raw_label: "text".to_string(),
                raw_bbox: bbox,
                raw_path: None,
            },
            tags: Vec::new(),
            metadata: BTreeMap::new(),
        }
    }
}
