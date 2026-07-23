use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use mupdf::{Document, TextChar, TextCharFlags, TextPageFlags, text_page::TextBlockType};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::policy::enforce_page_protected_regions;
use crate::{
    Block, BlockContent, BlockKind, BlockRole, BoundingBox, CancellationToken, DocumentMetadata,
    DocumentV1, FontFamilyClass, Line, Page, PageGeometry, PolicyDecisionSource, ProtectedRegion,
    ProtectedRegionKind, Segment, SegmentKind, SourceProvenance, TextStyle, TranslationPolicy,
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
                | TextPageFlags::ACCURATE_ASCENDERS
                | TextPageFlags::ACCURATE_SIDE_BEARINGS
                | TextPageFlags::PRESERVE_SPANS
                | TextPageFlags::PRESERVE_IMAGES
                | TextPageFlags::COLLECT_STYLES,
        )?;
        let mut extracted = Vec::new();
        let mut protected_regions = Vec::new();
        for block in text_page.blocks() {
            if block.r#type() == TextBlockType::Image {
                let bbox = bounding_box(block.bounds());
                let page_area = (geometry.width * geometry.height).max(1.0);
                if bbox.area() >= 1.0 && bbox.area() / page_area < 0.80 {
                    protected_regions.push(ProtectedRegion {
                        id: String::new(),
                        page_index,
                        kind: ProtectedRegionKind::Image,
                        bbox,
                        source: "mupdf_image".to_string(),
                        confidence: 0.98,
                        reason: "native_image_block".to_string(),
                    });
                }
                continue;
            }
            if block.r#type() != TextBlockType::Text {
                continue;
            }
            let mut lines = Vec::new();
            let mut font_sizes = Vec::new();
            let mut bold_chars = 0usize;
            let mut italic_chars = 0usize;
            let mut monospaced_chars = 0usize;
            let mut styled_chars = 0usize;
            for line in block.lines() {
                let mut segments = Vec::<Segment>::new();
                let mut baselines = Vec::new();
                let mut line_font_sizes = Vec::new();
                for character in line.chars() {
                    let Some(value) = character.char() else {
                        continue;
                    };
                    let size = character.size();
                    if !value.is_whitespace() && size.is_finite() && size > 0.0 {
                        font_sizes.push(size);
                        line_font_sizes.push(size);
                    }
                    let flags = character.flags();
                    let style = character_text_style(&character);
                    if !value.is_whitespace() {
                        styled_chars += 1;
                        if style.bold {
                            bold_chars += 1;
                        }
                        if style.italic {
                            italic_chars += 1;
                        }
                        if style.font_class == Some(FontFamilyClass::Monospace) {
                            monospaced_chars += 1;
                        }
                    }
                    let char_bbox = quad_bbox(character.quad());
                    let origin = character.origin();
                    if origin.y.is_finite() {
                        baselines.push(origin.y);
                    }
                    if let Some(segment) = segments.last_mut()
                        && styles_are_equivalent(&segment.style, &style)
                    {
                        segment.text.push(value);
                        segment.bbox = segment.bbox.union(char_bbox);
                    } else {
                        segments.push(Segment {
                            kind: if flags.contains(TextCharFlags::UNICODE_IS_GID) {
                                SegmentKind::Literal
                            } else {
                                SegmentKind::Text
                            },
                            text: value.to_string(),
                            bbox: char_bbox,
                            style,
                        });
                    }
                }
                trim_segments(&mut segments);
                restore_segment_spaces(&mut segments);
                let text = segments
                    .iter()
                    .map(|segment| segment.text.as_str())
                    .collect::<String>();
                if text.is_empty() {
                    continue;
                }
                let horizontal_bbox = segments
                    .iter()
                    .map(|segment| segment.bbox)
                    .reduce(BoundingBox::union)
                    .unwrap_or_else(|| bounding_box(line.bounds()));
                let baseline_y = median(baselines);
                let line_font_size = median(line_font_sizes);
                let line_bbox = match (baseline_y, line_font_size) {
                    (Some(baseline), Some(font_size)) => BoundingBox::new(
                        horizontal_bbox.x0,
                        baseline - font_size * 0.82,
                        horizontal_bbox.x1,
                        baseline + font_size * 0.22,
                    ),
                    _ => bounding_box(line.bounds()),
                };
                for segment in &mut segments {
                    let x0 = segment.bbox.x0.clamp(line_bbox.x0, line_bbox.x1);
                    let x1 = segment.bbox.x1.clamp(line_bbox.x0, line_bbox.x1);
                    segment.bbox = if x1 > x0 {
                        BoundingBox::new(x0, line_bbox.y0, x1, line_bbox.y1)
                    } else {
                        line_bbox
                    };
                }
                lines.push(Line {
                    bbox: line_bbox,
                    text,
                    segments,
                    baseline_y,
                    line_height: Some(line_bbox.height()),
                });
            }
            let lines = merge_inline_native_lines(lines);
            let block_bbox = lines
                .iter()
                .map(|line| line.bbox)
                .reduce(BoundingBox::union)
                .unwrap_or_else(|| bounding_box(block.bounds()));
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
                italic_ratio: italic_chars as f32 / styled_chars.max(1) as f32,
                monospaced_ratio: monospaced_chars as f32 / styled_chars.max(1) as f32,
            });
        }

        let body_font_size = weighted_median(
            extracted
                .iter()
                .filter(|block| block.average_font_size > 0.0)
                .map(|block| {
                    (
                        block.average_font_size,
                        block
                            .text
                            .chars()
                            .filter(|character| !character.is_whitespace())
                            .count()
                            .max(1),
                    )
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
            metadata.insert("italic_ratio".to_string(), json!(extracted.italic_ratio));
            metadata.insert(
                "monospaced_ratio".to_string(),
                json!(extracted.monospaced_ratio),
            );
            if let Some(first_line) = extracted.lines.first() {
                metadata.insert(
                    "first_line_indent_pt".to_string(),
                    json!((first_line.bbox.x0 - extracted.bbox.x0).max(0.0)),
                );
            }
            let line_pitches = extracted
                .lines
                .windows(2)
                .filter_map(|pair| Some(pair[1].baseline_y? - pair[0].baseline_y?))
                .filter(|pitch| pitch.is_finite() && *pitch > 0.0)
                .collect::<Vec<_>>();
            if let Some(line_pitch) = median(line_pitches) {
                metadata.insert("line_pitch_pt".to_string(), json!(line_pitch));
            }
            let justified = extracted.lines.len() > 2
                && extracted
                    .lines
                    .iter()
                    .take(extracted.lines.len() - 1)
                    .filter(|line| line.bbox.width() >= extracted.bbox.width() * 0.88)
                    .count()
                    * 4
                    >= (extracted.lines.len() - 1) * 3;
            metadata.insert("justify".to_string(), json!(justified));
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
        let mut page = Page {
            page_index,
            geometry,
            blocks,
            protected_regions,
            metadata: BTreeMap::new(),
        };
        enforce_page_protected_regions(&mut page);
        pages.push(page);
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

fn merge_inline_native_lines(lines: Vec<Line>) -> Vec<Line> {
    let mut merged = Vec::<Line>::with_capacity(lines.len());
    for mut line in lines {
        let merge_with_previous = merged.last().is_some_and(|previous| {
            let y_tolerance = (previous.bbox.height().max(line.bbox.height()) * 0.18).max(1.2);
            let vertically_inline = line.bbox.y0 <= previous.bbox.y1 + y_tolerance
                && line.bbox.y1 >= previous.bbox.y0 - y_tolerance;
            let endpoint_tolerance = previous.bbox.height().clamp(4.0, 12.0);
            let follows_endpoint = line.bbox.x0 >= previous.bbox.x1 - endpoint_tolerance
                && line.bbox.x0 <= previous.bbox.x1 + endpoint_tolerance;
            vertically_inline && follows_endpoint
        });
        if !merge_with_previous {
            merged.push(line);
            continue;
        }
        let previous = merged.last_mut().expect("previous line was checked");
        if inline_fragments_need_space(previous, &line)
            && !previous.text.ends_with(char::is_whitespace)
            && !line.text.starts_with(char::is_whitespace)
        {
            previous.text.push(' ');
            if let Some(first) = line.segments.first_mut() {
                first.text.insert(0, ' ');
            }
        }
        previous.text.push_str(&line.text);
        previous.bbox = previous.bbox.union(line.bbox);
        if line.line_height.unwrap_or_default() > previous.line_height.unwrap_or_default() {
            previous.baseline_y = line.baseline_y;
            previous.line_height = line.line_height;
        }
        previous.segments.append(&mut line.segments);
    }
    merged
}

fn inline_fragments_need_space(previous: &Line, next: &Line) -> bool {
    let Some(left) = previous
        .text
        .chars()
        .rev()
        .find(|character| !character.is_whitespace())
    else {
        return false;
    };
    let Some(right) = next
        .text
        .chars()
        .find(|character| !character.is_whitespace())
    else {
        return false;
    };
    let previous_math = previous
        .segments
        .last()
        .is_some_and(|segment| segment.style.font_class == Some(FontFamilyClass::Math));
    let next_math = next
        .segments
        .first()
        .is_some_and(|segment| segment.style.font_class == Some(FontFamilyClass::Math));
    if previous_math && next_math {
        return false;
    }
    if previous_math != next_math && matches!(left, ')' | ']') && right.is_alphabetic() {
        return true;
    }
    if !left.is_alphanumeric() || !right.is_alphanumeric() {
        return false;
    }
    if next.bbox.x0 - previous.bbox.x1 > 1.5 {
        return true;
    }
    if previous_math != next_math {
        return true;
    }
    if left.is_ascii_digit() && right.is_ascii_lowercase() {
        return true;
    }
    let trailing_uppercase = previous
        .text
        .chars()
        .rev()
        .take_while(|character| character.is_ascii_uppercase())
        .count();
    if trailing_uppercase >= 2 && right.is_ascii_lowercase() {
        return true;
    }
    let trailing_letters = previous
        .text
        .chars()
        .rev()
        .take_while(|character| character.is_ascii_alphabetic())
        .count();
    trailing_letters >= 2
        && (right.is_ascii_digit() || (left.is_ascii_lowercase() && right.is_ascii_uppercase()))
}

fn restore_segment_spaces(segments: &mut [Segment]) {
    for index in 1..segments.len() {
        let (previous, next) = segments.split_at_mut(index);
        let previous = &previous[index - 1];
        let next = &mut next[0];
        if segment_pair_needs_space(previous, next)
            && !previous.text.ends_with(char::is_whitespace)
            && !next.text.starts_with(char::is_whitespace)
        {
            next.text.insert(0, ' ');
        }
    }
}

fn segment_pair_needs_space(previous: &Segment, next: &Segment) -> bool {
    let Some(left) = previous
        .text
        .chars()
        .rev()
        .find(|character| !character.is_whitespace())
    else {
        return false;
    };
    let Some(right) = next
        .text
        .chars()
        .find(|character| !character.is_whitespace())
    else {
        return false;
    };
    let previous_math = previous.style.font_class == Some(FontFamilyClass::Math);
    let next_math = next.style.font_class == Some(FontFamilyClass::Math);
    if previous_math && next_math {
        return false;
    }
    if previous_math != next_math && matches!(left, ')' | ']') && right.is_alphabetic() {
        return true;
    }
    if !left.is_alphanumeric() || !right.is_alphanumeric() {
        return false;
    }
    if next.bbox.x0 - previous.bbox.x1 > 1.5 {
        return true;
    }
    if previous_math != next_math {
        return true;
    }
    if left.is_ascii_digit() && right.is_ascii_lowercase() {
        return true;
    }
    let trailing_uppercase = previous
        .text
        .chars()
        .rev()
        .take_while(|character| character.is_ascii_uppercase())
        .count();
    if trailing_uppercase >= 2 && right.is_ascii_lowercase() {
        return true;
    }
    let trailing_letters = previous
        .text
        .chars()
        .rev()
        .take_while(|character| character.is_ascii_alphabetic())
        .count();
    trailing_letters >= 2
        && (right.is_ascii_digit() || (left.is_ascii_lowercase() && right.is_ascii_uppercase()))
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

pub fn fuse_native_with_ocr(mut native: DocumentV1, ocr: &DocumentV1) -> DocumentV1 {
    for page in &mut native.pages {
        let Some(ocr_page) = ocr
            .pages
            .iter()
            .find(|candidate| candidate.page_index == page.page_index)
        else {
            continue;
        };
        page.protected_regions
            .extend(ocr_page.protected_regions.iter().cloned());
        enforce_page_protected_regions(page);
        let original_native_count = page.blocks.len();
        let mut matched_ocr = vec![false; ocr_page.blocks.len()];

        for native_block in &mut page.blocks {
            if native_block.policy.locked
                || !matches!(native_block.kind, BlockKind::Text | BlockKind::Code)
            {
                continue;
            }
            let best = ocr_page
                .blocks
                .iter()
                .enumerate()
                .filter(|(_, ocr_block)| {
                    matches!(ocr_block.kind, BlockKind::Text | BlockKind::Code)
                        && !ocr_block.policy.locked
                })
                .filter_map(|(index, ocr_block)| {
                    let containment = intersection_ratio(native_block.bbox, ocr_block.bbox);
                    let similarity = normalized_text_similarity(
                        &native_block.content.text,
                        &ocr_block.content.text,
                    );
                    (containment >= 0.20 || similarity >= 0.82)
                        .then_some((index, containment * 0.75 + similarity * 0.25))
                })
                .max_by(|left, right| left.1.total_cmp(&right.1));
            let Some((ocr_index, score)) = best else {
                continue;
            };
            let ocr_block = &ocr_page.blocks[ocr_index];
            matched_ocr[ocr_index] = true;
            if ocr_role_is_textual(ocr_block.role) && ocr_block.role != BlockRole::Unknown {
                native_block.role = ocr_block.role;
                native_block.policy = policy_for_role(ocr_block.role);
                native_block.policy.source = PolicyDecisionSource::OcrSemantic;
                native_block.policy.confidence = 0.96;
            }
            if native_block.continuation.is_none() {
                native_block
                    .continuation
                    .clone_from(&ocr_block.continuation);
            }
            native_block
                .tags
                .push(format!("ocr_role:{}", ocr_block.role.as_str()));
            native_block
                .metadata
                .insert("ocr_match_score".to_string(), json!(score));
            native_block.metadata.insert(
                "ocr_text_was_empty".to_string(),
                json!(ocr_block.content.text.trim().is_empty()),
            );
        }

        for (ocr_index, ocr_block) in ocr_page.blocks.iter().enumerate() {
            let native_coverage = page
                .blocks
                .iter()
                .take(original_native_count)
                .filter(|native_block| {
                    matches!(native_block.kind, BlockKind::Text | BlockKind::Code)
                })
                .map(|native_block| {
                    native_block
                        .bbox
                        .intersection(ocr_block.bbox)
                        .map_or(0.0, |intersection| {
                            intersection.area() / ocr_block.bbox.area().max(1.0)
                        })
                })
                .sum::<f32>()
                .clamp(0.0, 1.0);
            let should_add = match ocr_block.kind {
                BlockKind::Text | BlockKind::Code => {
                    !ocr_block.policy.locked
                        && !matched_ocr[ocr_index]
                        && native_coverage < 0.20
                        && !ocr_block.content.text.trim().is_empty()
                }
                BlockKind::Image | BlockKind::Table | BlockKind::Formula => native_coverage < 0.80,
                BlockKind::Unknown => false,
            };
            if !should_add {
                continue;
            }
            let mut inserted = ocr_block.clone();
            inserted.id = format!("ocr-{}", inserted.id);
            inserted.order = page.blocks.len();
            inserted.reading_order = inserted.order;
            inserted.source.provider = format!("{}+native-fusion", inserted.source.provider);
            inserted.tags.push("ocr_only".to_string());
            page.blocks.push(inserted);
        }
        enforce_page_protected_regions(page);
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
        page.metadata
            .insert("ocr_fused".to_string(), serde_json::Value::Bool(true));
    }
    native.metadata.provider = format!("{}+{}", native.metadata.provider, ocr.metadata.provider);
    native.metadata.provider_version = format!(
        "{}+{}",
        native.metadata.provider_version, ocr.metadata.provider_version
    );
    native
}

fn ocr_role_is_textual(role: BlockRole) -> bool {
    matches!(
        role,
        BlockRole::Title
            | BlockRole::Heading
            | BlockRole::Body
            | BlockRole::Abstract
            | BlockRole::TableOfContents
            | BlockRole::FigureCaption
            | BlockRole::TableCaption
            | BlockRole::Footnote
            | BlockRole::Header
            | BlockRole::Footer
            | BlockRole::PageNumber
            | BlockRole::Metadata
            | BlockRole::Reference
            | BlockRole::Code
    )
}

fn intersection_ratio(left: BoundingBox, right: BoundingBox) -> f32 {
    left.intersection(right).map_or(0.0, |intersection| {
        intersection.area() / left.area().max(1.0)
    })
}

fn normalized_text_similarity(left: &str, right: &str) -> f32 {
    let left = normalize_match_text(left);
    let right = normalize_match_text(right);
    if left.is_empty() || right.is_empty() {
        return 0.0;
    }
    let left_chars = left.chars().collect::<Vec<_>>();
    let right_chars = right.chars().collect::<Vec<_>>();
    let prefix = left_chars
        .iter()
        .zip(&right_chars)
        .take_while(|(left, right)| left == right)
        .count();
    let length_ratio = left_chars.len().min(right_chars.len()) as f32
        / left_chars.len().max(right_chars.len()) as f32;
    (prefix as f32 / left_chars.len().min(right_chars.len()).max(1) as f32) * 0.45
        + length_ratio * 0.55
}

fn normalize_match_text(text: &str) -> String {
    text.chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
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

fn policy_for_role(role: BlockRole) -> TranslationPolicy {
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
mod tests {
    use std::collections::BTreeMap;

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

    #[test]
    fn merges_small_cap_fragments_back_into_their_visual_line() {
        let style = TextStyle {
            font_size: Some(10.0),
            ..TextStyle::default()
        };
        let line = |bbox: BoundingBox, text: &str| Line {
            bbox,
            text: text.to_string(),
            segments: vec![Segment {
                kind: SegmentKind::Text,
                text: text.to_string(),
                bbox,
                style: style.clone(),
            }],
            baseline_y: Some(bbox.y1 - 2.0),
            line_height: Some(bbox.height()),
        };
        let lines = vec![
            line(
                BoundingBox::new(10.0, 10.0, 90.0, 20.0),
                "effectiveness of W",
            ),
            line(BoundingBox::new(89.0, 12.0, 105.0, 18.0), "ORD"),
            line(BoundingBox::new(104.0, 12.0, 120.0, 18.0), "TOUR"),
        ];

        let merged = merge_inline_native_lines(lines);

        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].text, "effectiveness of WORDTOUR");
        assert_eq!(merged[0].segments.len(), 3);
    }

    #[test]
    fn inline_merge_restores_spaces_at_semantic_fragment_boundaries() {
        let style = TextStyle {
            font_size: Some(10.0),
            ..TextStyle::default()
        };
        let line = |bbox: BoundingBox, text: &str| Line {
            bbox,
            text: text.to_string(),
            segments: vec![Segment {
                kind: SegmentKind::Text,
                text: text.to_string(),
                bbox,
                style: style.clone(),
            }],
            baseline_y: Some(bbox.y1 - 2.0),
            line_height: Some(bbox.height()),
        };
        let lines = vec![
            line(BoundingBox::new(10.0, 10.0, 45.0, 20.0), "Figure"),
            line(BoundingBox::new(44.5, 10.0, 50.0, 20.0), "2"),
            line(BoundingBox::new(49.5, 10.0, 75.0, 20.0), "shows"),
            line(BoundingBox::new(74.5, 10.0, 85.0, 20.0), "W"),
            line(BoundingBox::new(84.5, 10.0, 103.0, 20.0), "ORD"),
            line(BoundingBox::new(102.5, 10.0, 110.0, 20.0), "T"),
            line(BoundingBox::new(109.5, 10.0, 130.0, 20.0), "OUR"),
            line(BoundingBox::new(129.5, 10.0, 155.0, 20.0), "aligns"),
        ];

        let merged = merge_inline_native_lines(lines);

        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].text, "Figure 2 shows WORDTOUR aligns");
        let numeric_identifier = line(BoundingBox::new(10.0, 30.0, 50.0, 40.0), "20news");
        assert_eq!(
            merge_inline_native_lines(vec![numeric_identifier])[0].text,
            "20news"
        );
    }

    #[test]
    fn segment_spacing_separates_prose_from_inline_math() {
        let prose = TextStyle {
            font_class: Some(FontFamilyClass::Serif),
            ..TextStyle::default()
        };
        let math = TextStyle {
            font_class: Some(FontFamilyClass::Math),
            ..TextStyle::default()
        };
        let segment = |text: &str, x0: f32, x1: f32, style: TextStyle| Segment {
            kind: SegmentKind::Text,
            text: text.to_string(),
            bbox: BoundingBox::new(x0, 10.0, x1, 20.0),
            style,
        };
        let mut segments = vec![
            segment("ordering", 10.0, 45.0, prose.clone()),
            segment("σ", 45.0, 52.0, math.clone()),
            segment("and", 52.0, 68.0, prose.clone()),
            segment("τ", 68.0, 75.0, math.clone()),
        ];

        restore_segment_spaces(&mut segments);

        assert_eq!(
            segments
                .iter()
                .map(|segment| segment.text.as_str())
                .collect::<String>(),
            "ordering σ and τ"
        );

        let mut formula = vec![
            segment("n", 10.0, 16.0, math.clone()),
            segment("3", 17.8, 21.0, math.clone()),
            segment("+", 21.0, 25.0, math.clone()),
            segment("n", 25.0, 31.0, math.clone()),
            segment("2", 32.8, 36.0, math.clone()),
            segment("d", 36.0, 42.0, math),
        ];
        restore_segment_spaces(&mut formula);
        assert_eq!(
            formula
                .iter()
                .map(|segment| segment.text.as_str())
                .collect::<String>(),
            "n3+n2d"
        );
    }

    #[test]
    fn ocr_fusion_keeps_native_text_and_style_while_adding_missing_regions() {
        let native_style = TextStyle {
            font_family: Some("Source Serif".to_string()),
            font_class: Some(FontFamilyClass::Serif),
            font_size: Some(10.0),
            bold: true,
            ..TextStyle::default()
        };
        let native = fusion_document(
            "native",
            vec![fusion_block(
                "native-body",
                BoundingBox::new(10.0, 30.0, 90.0, 60.0),
                BlockRole::Body,
                "Native text",
                native_style.clone(),
            )],
        );
        let ocr = fusion_document(
            "ocr",
            vec![
                fusion_block(
                    "ocr-body",
                    BoundingBox::new(10.0, 30.0, 90.0, 60.0),
                    BlockRole::Abstract,
                    "",
                    TextStyle::default(),
                ),
                fusion_block(
                    "ocr-title",
                    BoundingBox::new(10.0, 5.0, 90.0, 20.0),
                    BlockRole::Title,
                    "Missing title",
                    TextStyle::default(),
                ),
            ],
        );

        let fused = fuse_native_with_ocr(native, &ocr);
        let native_body = fused.pages[0]
            .blocks
            .iter()
            .find(|block| block.id == "native-body")
            .unwrap();
        let added_title = fused.pages[0]
            .blocks
            .iter()
            .find(|block| block.id == "ocr-ocr-title")
            .unwrap();

        assert_eq!(native_body.content.text, "Native text");
        assert_eq!(native_body.content.segments[0].style, native_style);
        assert_eq!(native_body.role, BlockRole::Abstract);
        assert_eq!(added_title.role, BlockRole::Title);
        assert_eq!(added_title.content.text, "Missing title");
        assert!(added_title.tags.contains(&"ocr_only".to_string()));
    }

    fn fusion_document(provider: &str, blocks: Vec<Block>) -> DocumentV1 {
        DocumentV1::new(
            DocumentMetadata {
                document_id: provider.to_string(),
                source_path: "fixture.pdf".into(),
                provider: provider.to_string(),
                provider_version: "1".to_string(),
                extra: BTreeMap::new(),
            },
            vec![Page {
                page_index: 0,
                geometry: PageGeometry::new(100.0, 100.0),
                blocks,
                protected_regions: Vec::new(),
                metadata: BTreeMap::new(),
            }],
        )
    }

    fn fusion_block(
        id: &str,
        bbox: BoundingBox,
        role: BlockRole,
        text: &str,
        style: TextStyle,
    ) -> Block {
        let segment = Segment {
            kind: SegmentKind::Text,
            text: text.to_string(),
            bbox,
            style,
        };
        Block {
            id: id.to_string(),
            page_index: 0,
            order: 0,
            reading_order: 0,
            kind: BlockKind::Text,
            role,
            bbox,
            content: BlockContent {
                text: text.to_string(),
                lines: vec![Line {
                    bbox,
                    text: text.to_string(),
                    segments: vec![segment.clone()],
                    baseline_y: Some(bbox.y1 - 2.0),
                    line_height: Some(bbox.height()),
                }],
                segments: vec![segment],
                markdown: None,
                html: None,
            },
            policy: policy_for_role(role),
            continuation: None,
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
}
