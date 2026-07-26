use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::Path;

use anyhow::{Context, Result};
use mupdf::{Document, TextChar, TextCharFlags, TextPageFlags, text_page::TextBlockType};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::policy::enforce_page_protected_regions;
use crate::{
    BaselinePosition, Block, BlockContent, BlockKind, BlockRole, BlockStructure, BoundingBox,
    CancellationToken, DocumentMetadata, DocumentV1, FontFamilyClass, Line, Page, PageGeometry,
    PolicyDecisionSource, ProtectedRegion, ProtectedRegionKind, Segment, SegmentKind,
    SourceProvenance, StructureKind, TextFlow, TextStyle, TranslationPolicy,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PdfInspection {
    pub pages: Vec<PdfPageProfile>,
    pub recommends_ocr: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PdfPageProfile {
    pub page_index: usize,
    pub width: f32,
    pub height: f32,
    pub character_count: usize,
    pub replacement_character_count: usize,
    pub text_area_ratio: f32,
    pub image_area_ratio: f32,
    pub needs_ocr: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OcrFusionResult {
    pub document: DocumentV1,
    pub report: OcrFusionReport,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct OcrFusionReport {
    pub pages: Vec<OcrFusionPageReport>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct OcrFusionPageReport {
    pub page_index: usize,
    pub one_to_one_matches: usize,
    pub accepted_merges: usize,
    pub accepted_splits: usize,
    pub accepted_many_to_many: usize,
    pub ocr_only_blocks: usize,
    pub native_fallbacks: usize,
    pub decisions: Vec<OcrFusionDecision>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OcrFusionDecision {
    pub relation: OcrFusionRelation,
    pub native_block_ids: Vec<String>,
    pub ocr_block_ids: Vec<String>,
    pub score: f32,
    pub native_coverage: f32,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OcrFusionRelation {
    OneToOne,
    Merge,
    Split,
    ManyToMany,
    OcrOnly,
    NativeFallback,
}

mod extraction;
mod fusion;

pub use extraction::{extract_native_document, inspect_pdf};
pub use fusion::{fuse_native_with_ocr, fuse_native_with_ocr_report, reconcile_ocr_geometry};

#[derive(Debug)]
struct NativeBlock {
    bbox: BoundingBox,
    text: String,
    lines: Vec<Line>,
    average_font_size: f32,
    bold_ratio: f32,
    italic_ratio: f32,
    monospaced_ratio: f32,
}

fn classify_native_role(
    page_index: usize,
    page: &PageGeometry,
    block: &NativeBlock,
    body_font_size: f32,
    in_references: bool,
) -> BlockRole {
    let compact = block.text.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().all(|character| character.is_ascii_digit()) && compact.chars().count() <= 4 {
        return BlockRole::PageNumber;
    }
    if block.lines.len() == 1
        && compact.chars().count() >= 5
        && (block.bbox.height() > block.bbox.width() * 1.2
            || block.bbox.width() < block.average_font_size * 1.5)
    {
        return BlockRole::Metadata;
    }
    if block.bbox.y0 < page.height * 0.10
        && block.average_font_size < body_font_size * 0.92
        && compact.chars().count() < 160
    {
        return BlockRole::Header;
    }
    if block.bbox.y1 > page.height * 0.88
        && block.average_font_size < body_font_size * 0.9
        && compact.chars().count() >= 20
    {
        return BlockRole::Footnote;
    }
    if block.bbox.y1 > page.height * 0.96 && compact.chars().count() < 80 {
        return BlockRole::Footer;
    }
    let lower = compact.to_ascii_lowercase();
    if lower.starts_with("figure ") || lower.starts_with("fig. ") {
        return BlockRole::FigureCaption;
    }
    if lower.starts_with("table ") {
        return BlockRole::TableCaption;
    }
    if block.average_font_size < body_font_size * 0.70
        && block.bbox.height() < block.average_font_size * 1.35
        && compact.chars().count() <= 220
    {
        return BlockRole::Table;
    }
    if block.monospaced_ratio >= 0.62 {
        return BlockRole::Code;
    }
    if in_references {
        return BlockRole::Reference;
    }
    if page_index == 0
        && block.bbox.y0 < page.height * 0.42
        && block.average_font_size >= 14.0
        && block.average_font_size >= body_font_size * 1.55
    {
        return BlockRole::Title;
    }
    if page_index == 0
        && block.bbox.y0 < page.height * 0.32
        && compact.chars().count() <= 240
        && (compact.contains('@')
            || lower.contains("university")
            || lower.contains("institute")
            || lower.contains("research"))
    {
        return BlockRole::Metadata;
    }
    if compact.chars().count() <= 180
        && (block.average_font_size >= body_font_size * 1.22
            || block.bold_ratio >= 0.65
            || (looks_like_numbered_heading(&compact) && block.italic_ratio >= 0.15))
    {
        return BlockRole::Heading;
    }
    BlockRole::Body
}

pub(crate) fn policy_for_role(role: BlockRole) -> TranslationPolicy {
    match role {
        BlockRole::Title => TranslationPolicy::candidate(
            true,
            "native_title_candidate",
            PolicyDecisionSource::NativeHeuristic,
            0.90,
        ),
        BlockRole::Heading => TranslationPolicy::candidate(
            true,
            "native_heading_candidate",
            PolicyDecisionSource::NativeHeuristic,
            0.88,
        ),
        BlockRole::Body => TranslationPolicy::candidate(
            true,
            "native_body_candidate",
            PolicyDecisionSource::NativeHeuristic,
            0.90,
        ),
        BlockRole::Abstract => TranslationPolicy::candidate(
            true,
            "native_abstract_candidate",
            PolicyDecisionSource::NativeHeuristic,
            0.92,
        ),
        BlockRole::Prompt => TranslationPolicy::candidate(
            true,
            "native_prompt_candidate",
            PolicyDecisionSource::NativeHeuristic,
            0.94,
        ),
        BlockRole::Dialogue => TranslationPolicy::candidate(
            true,
            "native_dialogue_candidate",
            PolicyDecisionSource::NativeHeuristic,
            0.94,
        ),
        BlockRole::TableOfContents => TranslationPolicy::candidate(
            true,
            "native_toc_candidate",
            PolicyDecisionSource::NativeHeuristic,
            0.86,
        ),
        BlockRole::FigureCaption | BlockRole::TableCaption => TranslationPolicy::candidate(
            true,
            "native_caption_candidate",
            PolicyDecisionSource::NativeHeuristic,
            0.86,
        ),
        BlockRole::Footnote => TranslationPolicy::candidate(
            true,
            "native_footnote_candidate",
            PolicyDecisionSource::NativeHeuristic,
            0.82,
        ),
        BlockRole::Unknown => TranslationPolicy::unresolved("native_unknown"),
        _ => TranslationPolicy::locked_preserve(
            "native_non_body",
            PolicyDecisionSource::DeterministicRule,
            0.95,
        ),
    }
}

fn character_text_style(character: &TextChar<'_>) -> TextStyle {
    let size = character.size();
    let flags = character.flags();
    let mut style = TextStyle {
        font_size: (size.is_finite() && size > 0.0).then_some(size),
        bold: flags.contains(TextCharFlags::BOLD),
        underline: flags.contains(TextCharFlags::UNDERLINE),
        strikeout: flags.contains(TextCharFlags::STRIKEOUT),
        color_argb: Some(character.argb()),
        ..TextStyle::default()
    };
    if let Some(font) = character.font() {
        let family = normalize_font_family(font.name());
        let lower = family.to_ascii_lowercase();
        style.font_class = Some(if font.is_monospaced() {
            FontFamilyClass::Monospace
        } else if lower.contains("math")
            || lower.contains("symbol")
            || lower.starts_with("cmmi")
            || lower.starts_with("cmr")
            || lower.starts_with("cmsy")
            || lower.starts_with("cmex")
            || lower.starts_with("msam")
            || lower.starts_with("msbm")
        {
            FontFamilyClass::Math
        } else if lower.contains("sans")
            || lower.contains("sanl")
            || lower.contains("helvetica")
            || lower.contains("arial")
            || lower.contains("grotesk")
        {
            FontFamilyClass::SansSerif
        } else if font.is_serif() {
            FontFamilyClass::Serif
        } else {
            FontFamilyClass::SansSerif
        });
        style.font_family = Some(family);
        style.bold |= font.is_bold();
        style.italic = font.is_italic();
    }
    style
}

fn normalize_font_family(name: &str) -> String {
    let mut parts = name.splitn(2, '+');
    let prefix = parts.next().unwrap_or_default();
    let suffix = parts.next();
    if prefix.len() == 6
        && prefix
            .chars()
            .all(|character| character.is_ascii_uppercase())
        && let Some(suffix) = suffix
    {
        return suffix.to_string();
    }
    name.to_string()
}

fn styles_are_equivalent(left: &TextStyle, right: &TextStyle) -> bool {
    left.font_family == right.font_family
        && left.font_class == right.font_class
        && match (left.font_size, right.font_size) {
            (Some(left), Some(right)) => (left - right).abs() <= 0.15,
            (None, None) => true,
            _ => false,
        }
        && left.bold == right.bold
        && left.italic == right.italic
        && left.underline == right.underline
        && left.strikeout == right.strikeout
        && left.color_argb == right.color_argb
        && left.baseline_position == right.baseline_position
}

fn annotate_baseline_positions(segments: &mut [Segment]) {
    let Some(dominant_font_size) = weighted_median(
        segments
            .iter()
            .filter_map(|segment| {
                let font_size = segment
                    .style
                    .font_size
                    .filter(|size| size.is_finite() && *size > 0.0)?;
                let weight = segment
                    .text
                    .chars()
                    .filter(|character| !character.is_whitespace())
                    .count();
                (weight > 0).then_some((font_size, weight))
            })
            .collect(),
    ) else {
        return;
    };
    let normal_centers = segments
        .iter()
        .filter(|segment| {
            segment
                .style
                .font_size
                .is_some_and(|size| size >= dominant_font_size * 0.90)
        })
        .map(|segment| (segment.bbox.y0 + segment.bbox.y1) * 0.5)
        .filter(|center| center.is_finite())
        .collect::<Vec<_>>();
    let Some(normal_center) = median(normal_centers) else {
        return;
    };
    let shift_threshold = (dominant_font_size * 0.12).max(0.8);
    for segment in segments {
        let Some(size) = segment.style.font_size else {
            continue;
        };
        if !size.is_finite() || size > dominant_font_size * 0.90 {
            continue;
        }
        let center_shift = (segment.bbox.y0 + segment.bbox.y1) * 0.5 - normal_center;
        if center_shift <= -shift_threshold {
            segment.style.baseline_position = BaselinePosition::Superscript;
        } else if center_shift >= shift_threshold {
            segment.style.baseline_position = BaselinePosition::Subscript;
        }
    }
}

fn quad_bbox(quad: mupdf::Quad) -> BoundingBox {
    let xs = [quad.ul.x, quad.ur.x, quad.ll.x, quad.lr.x];
    let ys = [quad.ul.y, quad.ur.y, quad.ll.y, quad.lr.y];
    BoundingBox::new(
        xs.into_iter().fold(f32::INFINITY, f32::min),
        ys.into_iter().fold(f32::INFINITY, f32::min),
        xs.into_iter().fold(f32::NEG_INFINITY, f32::max),
        ys.into_iter().fold(f32::NEG_INFINITY, f32::max),
    )
}

fn baseline_normalized_line_bbox(
    horizontal_bbox: BoundingBox,
    baseline: f32,
    font_size: f32,
) -> BoundingBox {
    BoundingBox::new(
        horizontal_bbox.x0,
        baseline - font_size * 0.82,
        horizontal_bbox.x1,
        baseline + font_size * 0.22,
    )
}

fn trim_segments(segments: &mut Vec<Segment>) {
    while segments
        .first()
        .is_some_and(|segment| segment.text.trim().is_empty())
    {
        segments.remove(0);
    }
    while segments
        .last()
        .is_some_and(|segment| segment.text.trim().is_empty())
    {
        segments.pop();
    }
    if let Some(first) = segments.first_mut() {
        first.text = first.text.trim_start().to_string();
    }
    if let Some(last) = segments.last_mut() {
        last.text = last.text.trim_end().to_string();
    }
    segments.retain(|segment| !segment.text.is_empty());
}

fn looks_like_numbered_heading(text: &str) -> bool {
    let token = text.split_whitespace().next().unwrap_or_default();
    let trimmed = token.trim_end_matches('.');
    !trimmed.is_empty()
        && trimmed
            .chars()
            .all(|character| character.is_ascii_digit() || character == '.')
        && trimmed.chars().any(|character| character.is_ascii_digit())
}

fn bounding_box(rect: mupdf::Rect) -> BoundingBox {
    BoundingBox::new(rect.x0, rect.y0, rect.x1, rect.y1)
}

fn rect_area(x0: f32, y0: f32, x1: f32, y1: f32) -> f32 {
    ((x1 - x0).max(0.0)) * ((y1 - y0).max(0.0))
}

fn median(mut values: Vec<f32>) -> Option<f32> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
    let middle = values.len() / 2;
    if values.len().is_multiple_of(2) {
        Some((values[middle - 1] + values[middle]) / 2.0)
    } else {
        Some(values[middle])
    }
}

fn weighted_median(mut values: Vec<(f32, usize)>) -> Option<f32> {
    values.retain(|(value, weight)| value.is_finite() && *weight > 0);
    values.sort_by(|left, right| left.0.total_cmp(&right.0));
    let total = values.iter().map(|(_, weight)| *weight).sum::<usize>();
    let midpoint = total.div_ceil(2);
    let mut cumulative = 0usize;
    for (value, weight) in values {
        cumulative += weight;
        if cumulative >= midpoint {
            return Some(value);
        }
    }
    None
}

#[cfg(test)]
mod tests;
