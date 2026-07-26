use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{
    Block, BlockContent, BlockKind, BlockRole, BoundingBox, DocumentV1, Line, Page,
    PolicyDecisionSource, ProtectedRegion, ProtectedRegionKind, Segment, SegmentKind,
    TranslationPolicy,
};

pub const POLICY_REVIEW_CONFIDENCE_THRESHOLD: f32 = 0.80;
const UNMAPPED_GLYPH_TAG: &str = "unmapped_glyph_preserved";
const UNMAPPED_REGION_MARGIN_EM: f32 = 0.5;
const MAX_UNMAPPED_REGION_MARGIN_PT: f32 = 24.0;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PolicyReviewRequest {
    pub page_index: usize,
    pub page_width: f32,
    pub page_height: f32,
    pub blocks: Vec<PolicyReviewBlock>,
    pub protected_regions: Vec<ProtectedRegion>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PolicyReviewBlock {
    pub id: String,
    pub order: usize,
    pub kind: BlockKind,
    pub role: BlockRole,
    pub bbox: BoundingBox,
    pub text: String,
    pub translate: bool,
    pub reason: String,
    pub confidence: f32,
    pub locked: bool,
    #[serde(default)]
    pub protected_region_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PolicyReviewDecision {
    pub block_id: String,
    pub translate: bool,
    pub confidence: f32,
    pub reason: String,
}

pub trait TranslationPolicyReviewer: Sync {
    fn review(
        &self,
        request: &PolicyReviewRequest,
    ) -> Result<Vec<PolicyReviewDecision>, PolicyReviewError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyReviewError {
    pub message: String,
}

impl PolicyReviewError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for PolicyReviewError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.message.fmt(formatter)
    }
}

impl std::error::Error for PolicyReviewError {}

pub fn enforce_protected_regions(document: &mut DocumentV1) {
    for page in &mut document.pages {
        enforce_page_protected_regions(page);
    }
}

pub(crate) fn enforce_unmapped_glyph_policy(document: &mut DocumentV1) {
    for page in &mut document.pages {
        enforce_page_unmapped_glyph_policy(page);
    }
}

pub(crate) fn is_unmapped_glyph_character(character: char) -> bool {
    character == '\u{fffd}'
        || matches!(
            character,
            '\u{e000}'..='\u{f8ff}'
                | '\u{f0000}'..='\u{ffffd}'
                | '\u{100000}'..='\u{10fffd}'
                | '\u{fe00}'..='\u{fe0f}'
                | '\u{e0100}'..='\u{e01ef}'
        )
}

pub fn review_translation_policies(
    document: &mut DocumentV1,
    reviewer: &dyn TranslationPolicyReviewer,
) -> Vec<PolicyReviewError> {
    let mut errors = Vec::new();
    for page in &mut document.pages {
        let candidate_ids = page
            .blocks
            .iter()
            .filter(|block| policy_needs_review(&block.policy))
            .map(|block| block.id.clone())
            .collect::<Vec<_>>();
        if candidate_ids.is_empty() {
            continue;
        }
        let request = policy_review_request(page);
        let decisions = match reviewer.review(&request) {
            Ok(decisions) => decisions,
            Err(error) => {
                for block in &mut page.blocks {
                    if candidate_ids.contains(&block.id) {
                        block.policy = TranslationPolicy::candidate(
                            false,
                            format!("policy_review_failed:{}", block.policy.reason),
                            block.policy.source,
                            block.policy.confidence,
                        );
                    }
                }
                errors.push(error);
                continue;
            }
        };
        let by_id = decisions
            .into_iter()
            .map(|decision| (decision.block_id.clone(), decision))
            .collect::<BTreeMap<_, _>>();
        for block in &mut page.blocks {
            if !candidate_ids.contains(&block.id) || block.policy.locked {
                continue;
            }
            let Some(decision) = by_id.get(&block.id) else {
                block.policy.translate = false;
                block.policy.reason = format!("policy_review_omitted:{}", block.policy.reason);
                continue;
            };
            if !decision.confidence.is_finite()
                || decision.confidence < POLICY_REVIEW_CONFIDENCE_THRESHOLD
            {
                block.policy.translate = false;
                block.policy.reason = format!("policy_review_low_confidence:{}", decision.reason);
                continue;
            }
            block.policy = TranslationPolicy::candidate(
                decision.translate,
                format!("ai_review:{}", decision.reason),
                PolicyDecisionSource::AiReview,
                decision.confidence,
            );
        }
    }
    errors
}

pub(crate) fn derive_protected_regions_from_blocks(page: &mut Page) {
    let mut regions = page.protected_regions.clone();
    for block in &page.blocks {
        let Some(kind) = protected_kind_for_block(block) else {
            continue;
        };
        let raw_label = block.source.raw_label.trim().to_ascii_lowercase();
        let kind = if raw_label == "chart" {
            ProtectedRegionKind::Chart
        } else {
            kind
        };
        regions.push(ProtectedRegion {
            id: String::new(),
            page_index: page.page_index,
            kind,
            bbox: block.bbox,
            source: block.source.provider.clone(),
            confidence: if block.source.provider.contains("paddle") {
                0.99
            } else {
                0.96
            },
            reason: format!("provider_non_text:{}", kind.as_str()),
        });
    }
    page.protected_regions = normalized_regions(page.page_index, regions);
}

pub(crate) fn enforce_page_protected_regions(page: &mut Page) {
    enforce_page_unmapped_glyph_policy(page);
    page.protected_regions =
        normalized_regions(page.page_index, std::mem::take(&mut page.protected_regions));
    if page.protected_regions.is_empty() {
        return;
    }
    let mut enforced = Vec::with_capacity(page.blocks.len());
    for block in std::mem::take(&mut page.blocks) {
        enforced.extend(split_block_by_protection(block, &page.protected_regions));
    }
    page.blocks = enforced;
    reorder_page_blocks(page);
}

fn enforce_page_unmapped_glyph_policy(page: &mut Page) {
    for block in &mut page.blocks {
        if !block.policy.translate || !block_has_unmappable_source(block) {
            continue;
        }
        block.policy = TranslationPolicy::locked_preserve(
            "unmapped_glyph_source",
            PolicyDecisionSource::DeterministicRule,
            1.0,
        );
        if !block.tags.iter().any(|tag| tag == UNMAPPED_GLYPH_TAG) {
            block.tags.push(UNMAPPED_GLYPH_TAG.to_string());
        }
    }
}

fn block_has_unmappable_source(block: &Block) -> bool {
    if block.content.text.contains('\u{fffd}') {
        return true;
    }

    let mut characters = block
        .content
        .text
        .chars()
        .filter(|character| !character.is_whitespace());
    let first = characters.next();
    if first.is_some_and(is_unmapped_glyph_character) && characters.all(is_unmapped_glyph_character)
    {
        return true;
    }

    block
        .content
        .segments
        .iter()
        .chain(
            block
                .content
                .lines
                .iter()
                .flat_map(|line| line.segments.iter()),
        )
        .any(|segment| segment.kind == SegmentKind::Literal && !segment.text.trim().is_empty())
}

fn policy_needs_review(policy: &TranslationPolicy) -> bool {
    !policy.locked
        && (!policy.confidence.is_finite()
            || policy.confidence < POLICY_REVIEW_CONFIDENCE_THRESHOLD)
}

fn policy_review_request(page: &Page) -> PolicyReviewRequest {
    PolicyReviewRequest {
        page_index: page.page_index,
        page_width: page.geometry.width,
        page_height: page.geometry.height,
        blocks: page
            .blocks
            .iter()
            .map(|block| PolicyReviewBlock {
                id: block.id.clone(),
                order: block.reading_order,
                kind: block.kind,
                role: block.role,
                bbox: normalized_bbox(block.bbox, page.geometry.width, page.geometry.height),
                text: block.content.text.clone(),
                translate: block.policy.translate,
                reason: block.policy.reason.clone(),
                confidence: block.policy.confidence,
                locked: block.policy.locked,
                protected_region_ids: page
                    .protected_regions
                    .iter()
                    .filter(|region| bbox_center_inside(block.bbox, region.bbox))
                    .map(|region| region.id.clone())
                    .collect(),
            })
            .collect(),
        protected_regions: page
            .protected_regions
            .iter()
            .cloned()
            .map(|mut region| {
                region.bbox =
                    normalized_bbox(region.bbox, page.geometry.width, page.geometry.height);
                region
            })
            .collect(),
    }
}

fn normalized_bbox(bbox: BoundingBox, width: f32, height: f32) -> BoundingBox {
    BoundingBox::new(
        bbox.x0 / width.max(1.0),
        bbox.y0 / height.max(1.0),
        bbox.x1 / width.max(1.0),
        bbox.y1 / height.max(1.0),
    )
}

fn normalized_regions(page_index: usize, regions: Vec<ProtectedRegion>) -> Vec<ProtectedRegion> {
    let mut normalized = Vec::<ProtectedRegion>::new();
    for mut region in regions {
        if !region.bbox.is_valid() || region.bbox.area() < 1.0 {
            continue;
        }
        region.page_index = page_index;
        region.confidence = if region.confidence.is_finite() {
            region.confidence.clamp(0.0, 1.0)
        } else {
            0.0
        };
        if normalized.iter().any(|existing| {
            existing.kind == region.kind && overlap_similarity(existing.bbox, region.bbox) >= 0.92
        }) {
            continue;
        }
        region.id = format!("p{:03}-r{:04}", page_index + 1, normalized.len());
        normalized.push(region);
    }
    normalized.sort_by(|left, right| {
        left.bbox
            .y0
            .total_cmp(&right.bbox.y0)
            .then(left.bbox.x0.total_cmp(&right.bbox.x0))
    });
    for (index, region) in normalized.iter_mut().enumerate() {
        region.id = format!("p{:03}-r{index:04}", page_index + 1);
    }
    normalized
}

fn protected_kind_for_block(block: &Block) -> Option<ProtectedRegionKind> {
    match (block.kind, block.role) {
        (BlockKind::Table, _) | (_, BlockRole::Table) => Some(ProtectedRegionKind::Table),
        (BlockKind::Image, _) | (_, BlockRole::Image) => Some(ProtectedRegionKind::Image),
        (BlockKind::Formula, _) | (_, BlockRole::Formula) | (_, BlockRole::FormulaNumber) => {
            Some(ProtectedRegionKind::Formula)
        }
        (BlockKind::Code, _) | (_, BlockRole::Code) => Some(ProtectedRegionKind::Code),
        _ => None,
    }
}

fn split_block_by_protection(block: Block, regions: &[ProtectedRegion]) -> Vec<Block> {
    if matches!(
        block.role,
        BlockRole::FigureCaption | BlockRole::TableCaption
    ) {
        return vec![block];
    }
    if !matches!(block.kind, BlockKind::Text | BlockKind::Code) {
        let region = region_for_bbox(block.bbox, regions);
        return vec![protect_block(block, region)];
    }
    let region_margin = unmapped_protection_margin(&block);
    if block.content.lines.is_empty() {
        return match region_for_bbox_with_margin(block.bbox, regions, region_margin) {
            Some(region) => vec![protect_block(block, Some(region))],
            None => vec![block],
        };
    }

    let assignments = block
        .content
        .lines
        .iter()
        .map(|line| region_for_bbox_with_margin(line.bbox, regions, region_margin))
        .collect::<Vec<_>>();
    if assignments.iter().all(Option::is_none) {
        return vec![block];
    }
    if assignments.iter().all(|assignment| {
        assignment.map(|region| region.id.as_str())
            == assignments[0].map(|region| region.id.as_str())
    }) {
        return vec![protect_block(block, assignments[0])];
    }

    let original_id = block.id.clone();
    let mut parts = Vec::new();
    let mut start = 0usize;
    while start < assignments.len() {
        let region_id = assignments[start].map(|region| region.id.as_str());
        let mut end = start + 1;
        while end < assignments.len()
            && assignments[end].map(|region| region.id.as_str()) == region_id
        {
            end += 1;
        }
        let mut part = block.clone();
        part.id = format!("{original_id}-s{:02}", parts.len());
        part.continuation = None;
        set_block_lines(&mut part, &block.content.lines[start..end]);
        part.tags.push("protected_boundary_split".to_string());
        if let Some(region) = assignments[start] {
            part = protect_block(part, Some(region));
        }
        parts.push(part);
        start = end;
    }
    parts
}

fn protect_block(mut block: Block, region: Option<&ProtectedRegion>) -> Block {
    let (reason, source, confidence, role, region_id, region_kind) = match region {
        Some(region) => (
            format!(
                "inside_protected_region:{}:{}",
                region.kind.as_str(),
                region.id
            ),
            PolicyDecisionSource::ProtectedRegion,
            region.confidence,
            region.kind.block_role(),
            Some(region.id.clone()),
            Some(region.kind.as_str().to_string()),
        ),
        None => (
            format!("provider_non_text:{}", block.role.as_str()),
            PolicyDecisionSource::DeterministicRule,
            1.0,
            block.role,
            None,
            None,
        ),
    };
    block.policy = TranslationPolicy::locked_preserve(reason, source, confidence);
    block.role = role;
    block.tags.push("protected".to_string());
    if let Some(region_id) = region_id {
        block
            .metadata
            .insert("protected_region_id".to_string(), json!(region_id));
    }
    if let Some(region_kind) = region_kind {
        block
            .metadata
            .insert("protected_region_kind".to_string(), json!(region_kind));
    }
    block
}

fn set_block_lines(block: &mut Block, lines: &[Line]) {
    let lines = lines.to_vec();
    let segments = lines
        .iter()
        .flat_map(|line| line.segments.iter().cloned())
        .collect::<Vec<Segment>>();
    let bbox = lines
        .iter()
        .map(|line| line.bbox)
        .reduce(BoundingBox::union)
        .unwrap_or(block.bbox);
    let text = lines
        .iter()
        .map(|line| line.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    block.bbox = bbox;
    block.content = BlockContent {
        text,
        lines,
        segments,
        markdown: None,
        html: None,
    };
}

fn region_for_bbox(bbox: BoundingBox, regions: &[ProtectedRegion]) -> Option<&ProtectedRegion> {
    region_for_bbox_with_margin(bbox, regions, 0.0)
}

fn region_for_bbox_with_margin(
    bbox: BoundingBox,
    regions: &[ProtectedRegion],
    margin: f32,
) -> Option<&ProtectedRegion> {
    regions
        .iter()
        .filter_map(|region| {
            let candidate_bbox = BoundingBox::new(
                region.bbox.x0 - margin,
                region.bbox.y0 - margin,
                region.bbox.x1 + margin,
                region.bbox.y1 + margin,
            );
            let intersection = bbox.intersection(candidate_bbox)?;
            let ratio = intersection.area() / bbox.area().max(1.0);
            (bbox_center_inside(bbox, candidate_bbox) || ratio >= 0.45).then_some((region, ratio))
        })
        .max_by(|left, right| left.1.total_cmp(&right.1))
        .map(|(region, _)| region)
}

fn unmapped_protection_margin(block: &Block) -> f32 {
    if !block_has_unmappable_source(block) {
        return 0.0;
    }
    let font_size = block
        .content
        .segments
        .iter()
        .filter_map(|segment| segment.style.font_size)
        .filter(|size| size.is_finite() && *size > 0.0)
        .max_by(f32::total_cmp)
        .unwrap_or_else(|| block.bbox.height());
    (font_size * UNMAPPED_REGION_MARGIN_EM).clamp(1.0, MAX_UNMAPPED_REGION_MARGIN_PT)
}

fn bbox_center_inside(inner: BoundingBox, outer: BoundingBox) -> bool {
    let x = (inner.x0 + inner.x1) * 0.5;
    let y = (inner.y0 + inner.y1) * 0.5;
    x >= outer.x0 && x <= outer.x1 && y >= outer.y0 && y <= outer.y1
}

fn overlap_similarity(left: BoundingBox, right: BoundingBox) -> f32 {
    left.intersection(right).map_or(0.0, |intersection| {
        intersection.area() / left.area().min(right.area()).max(1.0)
    })
}

fn reorder_page_blocks(page: &mut Page) {
    page.blocks.sort_by(|left, right| {
        left.bbox
            .y0
            .total_cmp(&right.bbox.y0)
            .then(left.bbox.x0.total_cmp(&right.bbox.x0))
    });
    for (order, block) in page.blocks.iter_mut().enumerate() {
        block.order = order;
        block.reading_order = order;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DocumentMetadata, Line, PageGeometry, SegmentKind, SourceProvenance, TextStyle};

    struct FixtureReviewer;

    impl TranslationPolicyReviewer for FixtureReviewer {
        fn review(
            &self,
            request: &PolicyReviewRequest,
        ) -> Result<Vec<PolicyReviewDecision>, PolicyReviewError> {
            assert_eq!(request.blocks.len(), 4);
            assert!(request.blocks.iter().any(|block| block.locked));
            Ok(vec![
                PolicyReviewDecision {
                    block_id: "accepted".to_string(),
                    translate: true,
                    confidence: 0.91,
                    reason: "narrative prose".to_string(),
                },
                PolicyReviewDecision {
                    block_id: "low-confidence".to_string(),
                    translate: true,
                    confidence: 0.79,
                    reason: "possibly prose".to_string(),
                },
                PolicyReviewDecision {
                    block_id: "locked".to_string(),
                    translate: true,
                    confidence: 0.99,
                    reason: "must be ignored".to_string(),
                },
            ])
        }
    }

    fn text_block(id: &str, lines: &[(&str, BoundingBox)]) -> Block {
        let lines = lines
            .iter()
            .map(|(text, bbox)| Line {
                bbox: *bbox,
                text: (*text).to_string(),
                segments: vec![Segment {
                    kind: SegmentKind::Text,
                    text: (*text).to_string(),
                    bbox: *bbox,
                    style: TextStyle::default(),
                }],
                baseline_y: Some(bbox.y1),
                line_height: Some(bbox.height()),
            })
            .collect::<Vec<_>>();
        let bbox = lines
            .iter()
            .map(|line| line.bbox)
            .reduce(BoundingBox::union)
            .unwrap();
        Block {
            id: id.to_string(),
            page_index: 0,
            order: 0,
            reading_order: 0,
            kind: BlockKind::Text,
            role: BlockRole::Body,
            bbox,
            content: BlockContent {
                text: lines
                    .iter()
                    .map(|line| line.text.as_str())
                    .collect::<Vec<_>>()
                    .join("\n"),
                segments: lines
                    .iter()
                    .flat_map(|line| line.segments.iter().cloned())
                    .collect(),
                lines,
                markdown: None,
                html: None,
            },
            policy: TranslationPolicy::candidate(
                true,
                "native_body_candidate",
                PolicyDecisionSource::NativeHeuristic,
                0.90,
            ),
            continuation: None,
            structure: None,
            toc_entry: None,
            source: SourceProvenance {
                provider: "fixture".to_string(),
                raw_label: "text".to_string(),
                raw_bbox: bbox,
                raw_path: None,
            },
            tags: Vec::new(),
            metadata: BTreeMap::new(),
        }
    }

    #[test]
    fn unmapped_glyph_candidates_fail_closed_without_protected_regions() {
        let mut literal = text_block(
            "literal",
            &[("glyph", BoundingBox::new(10.0, 30.0, 40.0, 40.0))],
        );
        literal.content.segments[0].kind = SegmentKind::Literal;
        literal.content.lines[0].segments[0].kind = SegmentKind::Literal;
        let mut mixed_literal = text_block(
            "mixed-literal",
            &[("prose", BoundingBox::new(10.0, 50.0, 50.0, 60.0))],
        );
        mixed_literal.content.text.push('\u{e000}');
        mixed_literal.content.segments.push(Segment {
            kind: SegmentKind::Literal,
            text: "\u{e000}".to_string(),
            bbox: BoundingBox::new(50.0, 50.0, 55.0, 60.0),
            style: TextStyle::default(),
        });
        let mut document = DocumentV1::new(
            DocumentMetadata {
                document_id: "unmapped".to_string(),
                source_path: "fixture.pdf".into(),
                provider: "fixture".to_string(),
                provider_version: "1".to_string(),
                extra: BTreeMap::new(),
            },
            vec![Page {
                page_index: 0,
                geometry: PageGeometry::new(200.0, 200.0),
                blocks: vec![
                    text_block(
                        "replacement",
                        &[("\u{fffd}", BoundingBox::new(10.0, 10.0, 20.0, 20.0))],
                    ),
                    literal,
                    mixed_literal,
                    text_block(
                        "equation-number",
                        &[("(3)", BoundingBox::new(170.0, 50.0, 190.0, 60.0))],
                    ),
                ],
                protected_regions: Vec::new(),
                metadata: BTreeMap::new(),
            }],
        );

        enforce_protected_regions(&mut document);

        for block in &document.pages[0].blocks[..3] {
            assert!(!block.policy.translate);
            assert!(block.policy.locked);
            assert_eq!(block.policy.reason, "unmapped_glyph_source");
            assert!(block.tags.iter().any(|tag| tag == UNMAPPED_GLYPH_TAG));
        }
        assert!(document.pages[0].blocks[3].policy.translate);
        assert!(!document.pages[0].blocks[3].policy.locked);
    }

    #[test]
    fn formula_numbers_derive_formula_protection() {
        let bbox = BoundingBox::new(170.0, 50.0, 190.0, 62.0);
        let mut formula_number = text_block("equation-number", &[("(4)", bbox)]);
        formula_number.role = BlockRole::FormulaNumber;
        formula_number.source.provider = "paddle".to_string();
        formula_number.source.raw_label = "formula_number".to_string();
        let mut page = Page {
            page_index: 0,
            geometry: PageGeometry::new(200.0, 200.0),
            blocks: vec![formula_number],
            protected_regions: Vec::new(),
            metadata: BTreeMap::new(),
        };

        derive_protected_regions_from_blocks(&mut page);
        enforce_page_protected_regions(&mut page);

        assert_eq!(page.protected_regions.len(), 1);
        assert_eq!(page.protected_regions[0].kind, ProtectedRegionKind::Formula);
        assert_eq!(page.protected_regions[0].bbox, bbox);
        assert!(!page.blocks[0].policy.translate);
        assert!(page.blocks[0].policy.locked);
        assert_eq!(
            page.blocks[0].policy.source,
            PolicyDecisionSource::ProtectedRegion
        );
    }

    #[test]
    fn raw_tall_glyph_geometry_joins_an_overlapping_formula_region() {
        let glyph_bbox = BoundingBox::new(301.0, 564.5, 305.0, 584.1);
        let mut document = DocumentV1::new(
            DocumentMetadata {
                document_id: "formula-glyph".to_string(),
                source_path: "fixture.pdf".into(),
                provider: "fixture".to_string(),
                provider_version: "1".to_string(),
                extra: BTreeMap::new(),
            },
            vec![Page {
                page_index: 0,
                geometry: PageGeometry::new(595.0, 842.0),
                blocks: vec![text_block("delimiter", &[("\u{fffd}", glyph_bbox)])],
                protected_regions: vec![ProtectedRegion {
                    id: "formula".to_string(),
                    page_index: 0,
                    kind: ProtectedRegionKind::Formula,
                    bbox: BoundingBox::new(214.0, 564.4, 379.0, 612.0),
                    source: "paddle".to_string(),
                    confidence: 0.99,
                    reason: "provider_non_text:formula".to_string(),
                }],
                metadata: BTreeMap::new(),
            }],
        );

        enforce_protected_regions(&mut document);

        let block = &document.pages[0].blocks[0];
        assert_eq!(block.role, BlockRole::Formula);
        assert!(!block.policy.translate);
        assert!(block.policy.locked);
        assert_eq!(block.policy.source, PolicyDecisionSource::ProtectedRegion);
    }

    #[test]
    fn collapsed_unmapped_glyph_geometry_uses_typographic_protection_margin() {
        let glyph_bbox = BoundingBox::new(301.0, 556.2, 305.0, 567.5);
        let mut glyph = text_block("delimiter", &[("\u{fffd}", glyph_bbox)]);
        glyph.content.segments[0].style.font_size = Some(10.9);
        glyph.content.lines[0].segments[0].style.font_size = Some(10.9);
        let mut document = DocumentV1::new(
            DocumentMetadata {
                document_id: "formula-glyph".to_string(),
                source_path: "fixture.pdf".into(),
                provider: "fixture".to_string(),
                provider_version: "1".to_string(),
                extra: BTreeMap::new(),
            },
            vec![Page {
                page_index: 0,
                geometry: PageGeometry::new(595.0, 842.0),
                blocks: vec![glyph],
                protected_regions: vec![ProtectedRegion {
                    id: "formula".to_string(),
                    page_index: 0,
                    kind: ProtectedRegionKind::Formula,
                    bbox: BoundingBox::new(214.0, 564.4, 379.0, 612.0),
                    source: "paddle".to_string(),
                    confidence: 0.99,
                    reason: "provider_non_text:formula".to_string(),
                }],
                metadata: BTreeMap::new(),
            }],
        );

        enforce_protected_regions(&mut document);

        let block = &document.pages[0].blocks[0];
        assert_eq!(block.role, BlockRole::Formula);
        assert!(!block.policy.translate);
        assert!(block.policy.locked);
        assert_eq!(block.policy.source, PolicyDecisionSource::ProtectedRegion);
        assert_eq!(
            block.metadata.get("protected_region_kind"),
            Some(&json!("formula"))
        );
    }

    #[test]
    fn protected_region_locks_contained_text_and_splits_boundary_blocks() {
        let mut document = DocumentV1::new(
            DocumentMetadata {
                document_id: "fixture".to_string(),
                source_path: "fixture.pdf".into(),
                provider: "fixture".to_string(),
                provider_version: "1".to_string(),
                extra: BTreeMap::new(),
            },
            vec![Page {
                page_index: 0,
                geometry: PageGeometry::new(200.0, 200.0),
                blocks: vec![text_block(
                    "mixed",
                    &[
                        (
                            "Translate this prose.",
                            BoundingBox::new(10.0, 10.0, 190.0, 20.0),
                        ),
                        ("chart label", BoundingBox::new(80.0, 80.0, 120.0, 90.0)),
                    ],
                )],
                protected_regions: vec![ProtectedRegion {
                    id: "chart".to_string(),
                    page_index: 0,
                    kind: ProtectedRegionKind::Chart,
                    bbox: BoundingBox::new(50.0, 50.0, 150.0, 150.0),
                    source: "paddle".to_string(),
                    confidence: 0.99,
                    reason: "provider_non_text:chart".to_string(),
                }],
                metadata: BTreeMap::new(),
            }],
        );

        enforce_protected_regions(&mut document);

        assert_eq!(document.pages[0].blocks.len(), 2);
        assert!(document.pages[0].blocks[0].policy.translate);
        assert!(!document.pages[0].blocks[1].policy.translate);
        assert!(document.pages[0].blocks[1].policy.locked);
        assert_eq!(document.pages[0].blocks[1].role, BlockRole::Image);
    }

    #[test]
    fn ai_review_only_changes_confident_unlocked_candidates() {
        let mut blocks = ["accepted", "low-confidence", "omitted", "locked"]
            .into_iter()
            .enumerate()
            .map(|(index, id)| {
                text_block(
                    id,
                    &[(
                        "Ambiguous text",
                        BoundingBox::new(
                            10.0,
                            10.0 + index as f32 * 20.0,
                            190.0,
                            20.0 + index as f32 * 20.0,
                        ),
                    )],
                )
            })
            .collect::<Vec<_>>();
        blocks[0].policy = TranslationPolicy::unresolved("fusion_conflict");
        blocks[1].policy = TranslationPolicy::unresolved("fusion_conflict");
        blocks[2].policy = TranslationPolicy::unresolved("fusion_conflict");
        blocks[3].policy = TranslationPolicy::locked_preserve(
            "inside_protected_region:chart",
            PolicyDecisionSource::ProtectedRegion,
            0.99,
        );
        let mut document = DocumentV1::new(
            DocumentMetadata {
                document_id: "fixture".to_string(),
                source_path: "fixture.pdf".into(),
                provider: "fixture".to_string(),
                provider_version: "1".to_string(),
                extra: BTreeMap::new(),
            },
            vec![Page {
                page_index: 0,
                geometry: PageGeometry::new(200.0, 200.0),
                blocks,
                protected_regions: Vec::new(),
                metadata: BTreeMap::new(),
            }],
        );

        let errors = review_translation_policies(&mut document, &FixtureReviewer);

        assert!(errors.is_empty());
        let blocks = &document.pages[0].blocks;
        assert!(blocks[0].policy.translate);
        assert_eq!(blocks[0].policy.source, PolicyDecisionSource::AiReview);
        assert!(!blocks[1].policy.translate);
        assert!(
            blocks[1]
                .policy
                .reason
                .starts_with("policy_review_low_confidence:")
        );
        assert!(!blocks[2].policy.translate);
        assert!(
            blocks[2]
                .policy
                .reason
                .starts_with("policy_review_omitted:")
        );
        assert!(!blocks[3].policy.translate);
        assert!(blocks[3].policy.locked);
        assert_eq!(
            blocks[3].policy.source,
            PolicyDecisionSource::ProtectedRegion
        );
    }
}
