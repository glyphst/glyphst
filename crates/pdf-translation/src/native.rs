use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use mupdf::{Document, TextCharFlags, TextPageFlags, text_page::TextBlockType};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{
    Block, BlockContent, BlockKind, BlockRole, BoundingBox, CancellationToken, DocumentMetadata,
    DocumentV1, Line, Page, PageGeometry, Segment, SegmentKind, SourceProvenance, TextStyle,
    TranslationPolicy,
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

pub fn inspect_pdf(path: &Path, cancellation: &CancellationToken) -> Result<PdfInspection> {
    let document =
        Document::open(path).with_context(|| format!("failed to open PDF {}", path.display()))?;
    let page_count = usize::try_from(document.page_count()?).unwrap_or_default();
    let mut pages = Vec::with_capacity(page_count);

    for page_index in 0..page_count {
        cancellation.check()?;
        let page = document.load_page(page_index as i32)?;
        let bounds = page.bounds()?;
        let page_area = ((bounds.x1 - bounds.x0) * (bounds.y1 - bounds.y0)).max(1.0);
        let text_page = page.to_text_page(
            TextPageFlags::PRESERVE_WHITESPACE
                | TextPageFlags::PRESERVE_IMAGES
                | TextPageFlags::ACCURATE_BBOXES
                | TextPageFlags::PARAGRAPH_BREAK
                | TextPageFlags::TABLE_HUNT,
        )?;
        let mut character_count = 0;
        let mut replacement_character_count = 0;
        let mut text_area = 0.0;
        let mut image_area = 0.0;
        for block in text_page.blocks() {
            match block.r#type() {
                TextBlockType::Text => {
                    let block_bounds = block.bounds();
                    text_area += rect_area(
                        block_bounds.x0,
                        block_bounds.y0,
                        block_bounds.x1,
                        block_bounds.y1,
                    );
                    for line in block.lines() {
                        for character in line.chars() {
                            if let Some(value) = character.char() {
                                if !value.is_whitespace() {
                                    character_count += 1;
                                }
                                if value == '\u{fffd}' {
                                    replacement_character_count += 1;
                                }
                            }
                        }
                    }
                }
                TextBlockType::Image => {
                    let block_bounds = block.bounds();
                    image_area += rect_area(
                        block_bounds.x0,
                        block_bounds.y0,
                        block_bounds.x1,
                        block_bounds.y1,
                    );
                }
                _ => {}
            }
        }
        let text_area_ratio = (text_area / page_area).clamp(0.0, 1.0);
        let image_area_ratio = (image_area / page_area).clamp(0.0, 1.0);
        let replacement_ratio = replacement_character_count as f32 / character_count.max(1) as f32;
        let needs_ocr = character_count < 80
            || text_area_ratio < 0.012
            || replacement_ratio > 0.02
            || (image_area_ratio > 0.72 && character_count < 350);
        pages.push(PdfPageProfile {
            page_index,
            width: bounds.x1 - bounds.x0,
            height: bounds.y1 - bounds.y0,
            character_count,
            replacement_character_count,
            text_area_ratio,
            image_area_ratio,
            needs_ocr,
        });
    }

    Ok(PdfInspection {
        recommends_ocr: pages.iter().any(|page| page.needs_ocr),
        pages,
    })
}

pub fn extract_native_document(
    path: &Path,
    document_id: impl Into<String>,
    cancellation: &CancellationToken,
) -> Result<DocumentV1> {
    let source =
        Document::open(path).with_context(|| format!("failed to open PDF {}", path.display()))?;
    let page_count = usize::try_from(source.page_count()?).unwrap_or_default();
    let mut pages = Vec::with_capacity(page_count);

    for page_index in 0..page_count {
        cancellation.check()?;
        let source_page = source.load_page(page_index as i32)?;
        let bounds = source_page.bounds()?;
        let geometry = PageGeometry::new(bounds.x1 - bounds.x0, bounds.y1 - bounds.y0);
        let text_page = source_page.to_text_page(
            TextPageFlags::PRESERVE_WHITESPACE
                | TextPageFlags::ACCURATE_BBOXES
                | TextPageFlags::PARAGRAPH_BREAK
                | TextPageFlags::TABLE_HUNT
                | TextPageFlags::COLLECT_STYLES,
        )?;
        let mut extracted = Vec::new();
        for block in text_page.blocks() {
            if block.r#type() != TextBlockType::Text {
                continue;
            }
            let block_bbox = bounding_box(block.bounds());
            let mut lines = Vec::new();
            let mut font_sizes = Vec::new();
            let mut bold_chars = 0usize;
            let mut styled_chars = 0usize;
            for line in block.lines() {
                let mut text = String::new();
                let mut style = TextStyle::default();
                let mut style_initialized = false;
                for character in line.chars() {
                    let Some(value) = character.char() else {
                        continue;
                    };
                    text.push(value);
                    let size = character.size();
                    if size.is_finite() && size > 0.0 {
                        font_sizes.push(size);
                    }
                    let flags = character.flags();
                    styled_chars += 1;
                    if flags.contains(TextCharFlags::BOLD) {
                        bold_chars += 1;
                    }
                    if !style_initialized {
                        style.font_size = (size.is_finite() && size > 0.0).then_some(size);
                        style.bold = flags.contains(TextCharFlags::BOLD);
                        if let Some(font) = character.font() {
                            style.font_family = Some(font.name().to_string());
                            style.bold |= font.is_bold();
                            style.italic = font.is_italic();
                        }
                        style.color_argb = Some(character.argb());
                        style_initialized = true;
                    }
                }
                let text = text.trim().to_string();
                if text.is_empty() {
                    continue;
                }
                let line_bbox = bounding_box(line.bounds());
                lines.push(Line {
                    bbox: line_bbox,
                    text: text.clone(),
                    segments: vec![Segment {
                        kind: SegmentKind::Text,
                        text,
                        bbox: line_bbox,
                        style,
                    }],
                });
            }
            let text = lines
                .iter()
                .map(|line| line.text.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            if text.trim().is_empty() {
                continue;
            }
            let average_font_size = if font_sizes.is_empty() {
                0.0
            } else {
                font_sizes.iter().sum::<f32>() / font_sizes.len() as f32
            };
            extracted.push(NativeBlock {
                bbox: block_bbox,
                text,
                lines,
                average_font_size,
                bold_ratio: bold_chars as f32 / styled_chars.max(1) as f32,
            });
        }

        let body_font_size = median(
            extracted
                .iter()
                .filter_map(|block| {
                    (block.average_font_size > 0.0).then_some(block.average_font_size)
                })
                .collect(),
        )
        .unwrap_or(10.0);
        let mut in_references = false;
        let mut blocks = Vec::with_capacity(extracted.len());
        for (order, extracted) in extracted.into_iter().enumerate() {
            let compact = extracted
                .text
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            let lowercase = compact.to_lowercase();
            let role = classify_native_role(
                page_index,
                &geometry,
                &extracted,
                body_font_size,
                in_references,
            );
            if role == BlockRole::Heading
                && matches!(lowercase.as_str(), "references" | "bibliography")
            {
                in_references = true;
            }
            let policy = policy_for_role(role);
            let mut metadata = BTreeMap::new();
            metadata.insert(
                "average_font_size".to_string(),
                json!(extracted.average_font_size),
            );
            metadata.insert("bold_ratio".to_string(), json!(extracted.bold_ratio));
            let segments = extracted
                .lines
                .iter()
                .flat_map(|line| line.segments.iter().cloned())
                .collect();
            blocks.push(Block {
                id: format!("p{:03}-b{order:04}", page_index + 1),
                page_index,
                order,
                reading_order: order,
                kind: BlockKind::Text,
                role,
                bbox: extracted.bbox,
                content: BlockContent {
                    text: extracted.text,
                    lines: extracted.lines,
                    segments,
                    markdown: None,
                    html: None,
                },
                policy,
                continuation: None,
                source: SourceProvenance {
                    provider: "mupdf".to_string(),
                    raw_label: "text".to_string(),
                    raw_bbox: extracted.bbox,
                    raw_path: None,
                },
                tags: Vec::new(),
                metadata,
            });
        }
        pages.push(Page {
            page_index,
            geometry,
            blocks,
            metadata: BTreeMap::new(),
        });
    }

    let document = DocumentV1::new(
        DocumentMetadata {
            document_id: document_id.into(),
            source_path: path.to_path_buf(),
            provider: "mupdf".to_string(),
            provider_version: env!("CARGO_PKG_VERSION").to_string(),
            extra: BTreeMap::new(),
        },
        pages,
    );
    document.validate()?;
    Ok(document)
}

pub fn reconcile_ocr_geometry(document: &mut DocumentV1, native: &DocumentV1) {
    for page in &mut document.pages {
        let Some(native_page) = native
            .pages
            .iter()
            .find(|native_page| native_page.page_index == page.page_index)
        else {
            continue;
        };
        for block in &mut page.blocks {
            if !block.policy.translate || !matches!(block.kind, BlockKind::Text) {
                continue;
            }
            if let Some(reconciled) = reconciled_bbox(
                block.bbox,
                native_page
                    .blocks
                    .iter()
                    .filter(|native_block| matches!(native_block.kind, BlockKind::Text))
                    .map(|native_block| native_block.bbox),
            ) {
                block.bbox = reconciled;
            }
        }
    }
}

fn reconciled_bbox(
    provider: BoundingBox,
    native_boxes: impl Iterator<Item = BoundingBox>,
) -> Option<BoundingBox> {
    let native = native_boxes
        .filter(|native| {
            horizontal_containment(provider, *native) >= 0.65
                && vertical_native_coverage(provider, *native) >= 0.45
        })
        .reduce(BoundingBox::union)?;
    geometry_reconciliation_is_safe(provider, native).then(|| provider.union(native))
}

fn horizontal_containment(left: BoundingBox, right: BoundingBox) -> f32 {
    let intersection = (left.x1.min(right.x1) - left.x0.max(right.x0)).max(0.0);
    intersection / left.width().min(right.width()).max(1.0)
}

fn vertical_native_coverage(provider: BoundingBox, native: BoundingBox) -> f32 {
    let intersection = (provider.y1.min(native.y1) - provider.y0.max(native.y0)).max(0.0);
    intersection / native.height().max(1.0)
}

fn geometry_containment(left: BoundingBox, right: BoundingBox) -> f32 {
    let intersection = BoundingBox::new(
        left.x0.max(right.x0),
        left.y0.max(right.y0),
        left.x1.min(right.x1),
        left.y1.min(right.y1),
    )
    .area();
    intersection / left.area().min(right.area()).max(1.0)
}

fn geometry_reconciliation_is_safe(provider: BoundingBox, native: BoundingBox) -> bool {
    geometry_containment(provider, native) >= 0.55
        && (provider.x0 - native.x0).abs() <= 48.0
        && (provider.x1 - native.x1).abs() <= 48.0
        && (provider.y0 - native.y0).abs() <= 48.0
        && (provider.y1 - native.y1).abs() <= 48.0
}

#[derive(Debug)]
struct NativeBlock {
    bbox: BoundingBox,
    text: String,
    lines: Vec<Line>,
    average_font_size: f32,
    bold_ratio: f32,
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
    if block.bbox.y0 < page.height * 0.075 && compact.chars().count() < 160 {
        return BlockRole::Header;
    }
    if block.bbox.y1 > page.height * 0.94 && compact.chars().count() < 160 {
        return BlockRole::Footer;
    }
    if in_references {
        return BlockRole::Reference;
    }
    if page_index == 0
        && block.bbox.y0 < page.height * 0.42
        && block.average_font_size >= body_font_size * 1.55
    {
        return BlockRole::Title;
    }
    if compact.chars().count() <= 180
        && (block.average_font_size >= body_font_size * 1.22 || block.bold_ratio >= 0.65)
    {
        return BlockRole::Heading;
    }
    BlockRole::Body
}

fn policy_for_role(role: BlockRole) -> TranslationPolicy {
    match role {
        BlockRole::Title => TranslationPolicy::translate("native_title_candidate"),
        BlockRole::Heading => TranslationPolicy::translate("native_heading_candidate"),
        BlockRole::Body => TranslationPolicy::translate("native_body_candidate"),
        BlockRole::Abstract => TranslationPolicy::translate("native_abstract_candidate"),
        BlockRole::TableOfContents => TranslationPolicy::translate("native_toc_candidate"),
        BlockRole::FigureCaption | BlockRole::TableCaption => {
            TranslationPolicy::translate("native_caption_candidate")
        }
        _ => TranslationPolicy::preserve("native_non_body"),
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconciles_provider_bbox_across_split_native_blocks() {
        let provider = BoundingBox::new(66.0, 449.0, 293.0, 748.0);
        let native = [
            BoundingBox::new(70.0, 451.0, 289.0, 489.0),
            BoundingBox::new(70.0, 493.0, 291.0, 690.0),
            BoundingBox::new(71.0, 695.0, 291.0, 773.0),
        ];

        assert_eq!(
            reconciled_bbox(provider, native.into_iter()),
            Some(BoundingBox::new(66.0, 449.0, 293.0, 773.0))
        );
    }

    #[test]
    fn rejects_unrelated_native_geometry() {
        let provider = BoundingBox::new(66.0, 449.0, 293.0, 748.0);
        let native = [BoundingBox::new(310.0, 440.0, 525.0, 750.0)];

        assert_eq!(reconciled_bbox(provider, native.into_iter()), None);
    }
}
