use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use mupdf::{Document, TextChar, TextCharFlags, TextPageFlags, text_page::TextBlockType};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::policy::enforce_page_protected_regions;
use crate::{
    BaselinePosition, Block, BlockContent, BlockKind, BlockRole, BoundingBox, CancellationToken,
    DocumentMetadata, DocumentV1, FontFamilyClass, Line, Page, PageGeometry, PolicyDecisionSource,
    ProtectedRegion, ProtectedRegionKind, Segment, SegmentKind, SourceProvenance, TextStyle,
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
                let baseline_y = median(baselines);
                let line_font_size = median(line_font_sizes);
                annotate_baseline_positions(&mut segments);
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
        let extracted = extracted
            .into_iter()
            .flat_map(split_native_paragraphs)
            .collect::<Vec<_>>();

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
        for (order, mut extracted) in extracted.into_iter().enumerate() {
            for line in &mut extracted.lines {
                annotate_baseline_positions(&mut line.segments);
            }
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
        reclassify_standalone_prompt_blocks(&mut page);
        reclassify_dialogue_blocks(&mut page);
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

fn split_native_paragraphs(block: NativeBlock) -> Vec<NativeBlock> {
    if block.lines.len() < 6
        || block.monospaced_ratio >= 0.25
        || native_text_looks_like_dialogue(&block.text)
        || native_text_looks_like_prompt(&block.text)
        || block
            .text
            .chars()
            .filter(|character| character.is_alphabetic())
            .count()
            < 100
    {
        return vec![block];
    }

    let dominant_left = median(
        block
            .lines
            .iter()
            .map(|line| line.bbox.x0)
            .filter(|value| value.is_finite())
            .collect(),
    )
    .unwrap_or(block.bbox.x0);
    let median_pitch = median(
        block
            .lines
            .windows(2)
            .filter_map(|pair| Some(pair[1].baseline_y? - pair[0].baseline_y?))
            .filter(|pitch| pitch.is_finite() && *pitch > 0.0)
            .collect(),
    );
    let indent_threshold = (block.average_font_size * 0.65).max(6.0);
    let mut boundaries = vec![0usize];
    for index in 1..block.lines.len().saturating_sub(1) {
        let previous = &block.lines[index - 1];
        let current = &block.lines[index];
        let indent = current.bbox.x0 - dominant_left;
        let starts_paragraph = current
            .text
            .trim_start()
            .chars()
            .find(|character| character.is_alphabetic())
            .is_some_and(|character| !character.is_lowercase());
        let previous_ends_sentence = previous
            .text
            .trim_end()
            .trim_end_matches(['"', '\'', '”', '’', ')', ']'])
            .ends_with(['.', '?', '!', '。', '？', '！']);
        let short_previous = previous.bbox.width() <= block.bbox.width() * 0.92;
        let wide_vertical_gap = median_pitch
            .zip(previous.baseline_y.zip(current.baseline_y))
            .is_some_and(|(pitch, (previous_y, current_y))| current_y - previous_y >= pitch * 1.12);
        let enough_lines_before = index - boundaries.last().copied().unwrap_or_default() >= 2;
        if indent >= indent_threshold
            && starts_paragraph
            && previous_ends_sentence
            && (short_previous || wide_vertical_gap)
            && enough_lines_before
        {
            boundaries.push(index);
        }
    }
    if boundaries.len() == 1 {
        return vec![block];
    }
    boundaries.push(block.lines.len());

    let NativeBlock {
        lines,
        bold_ratio,
        italic_ratio,
        monospaced_ratio,
        ..
    } = block;
    boundaries
        .windows(2)
        .filter_map(|range| {
            let paragraph_lines = lines[range[0]..range[1]].to_vec();
            native_block_from_lines(paragraph_lines, bold_ratio, italic_ratio, monospaced_ratio)
        })
        .collect()
}

fn native_block_from_lines(
    lines: Vec<Line>,
    fallback_bold_ratio: f32,
    fallback_italic_ratio: f32,
    fallback_monospaced_ratio: f32,
) -> Option<NativeBlock> {
    let bbox = lines
        .iter()
        .map(|line| line.bbox)
        .reduce(BoundingBox::union)?;
    let text = lines
        .iter()
        .map(|line| line.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    if text.trim().is_empty() {
        return None;
    }

    let mut styled_characters = 0usize;
    let mut bold_characters = 0usize;
    let mut italic_characters = 0usize;
    let mut monospaced_characters = 0usize;
    let mut weighted_font_size = 0.0f32;
    let mut font_size_weight = 0usize;
    for segment in lines.iter().flat_map(|line| &line.segments) {
        let characters = segment
            .text
            .chars()
            .filter(|character| !character.is_whitespace())
            .count();
        if characters == 0 {
            continue;
        }
        styled_characters += characters;
        bold_characters += usize::from(segment.style.bold) * characters;
        italic_characters += usize::from(segment.style.italic) * characters;
        monospaced_characters +=
            usize::from(segment.style.font_class == Some(FontFamilyClass::Monospace)) * characters;
        if let Some(font_size) = segment
            .style
            .font_size
            .filter(|font_size| font_size.is_finite() && *font_size > 0.0)
        {
            weighted_font_size += font_size * characters as f32;
            font_size_weight += characters;
        }
    }
    let ratio = |count: usize, fallback: f32| {
        if styled_characters == 0 {
            fallback
        } else {
            count as f32 / styled_characters as f32
        }
    };
    Some(NativeBlock {
        bbox,
        text,
        lines,
        average_font_size: if font_size_weight == 0 {
            0.0
        } else {
            weighted_font_size / font_size_weight as f32
        },
        bold_ratio: ratio(bold_characters, fallback_bold_ratio),
        italic_ratio: ratio(italic_characters, fallback_italic_ratio),
        monospaced_ratio: ratio(monospaced_characters, fallback_monospaced_ratio),
    })
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
        reclassify_prompt_code_regions(page);
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
        reclassify_standalone_prompt_blocks(page);
        reclassify_dialogue_blocks(page);
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

fn reclassify_prompt_code_regions(page: &mut Page) {
    let prompt_boxes = page
        .protected_regions
        .iter()
        .filter(|region| {
            region.kind == ProtectedRegionKind::Code
                && region.source.to_ascii_lowercase().contains("paddle")
        })
        .filter(|region| {
            page.blocks.iter().any(|block| {
                block.kind == BlockKind::Text
                    && prompt_region_overlap(block.bbox, region.bbox) >= 0.55
                    && native_block_looks_like_prompt(block)
            })
        })
        .map(|region| region.bbox)
        .collect::<Vec<_>>();
    if prompt_boxes.is_empty() {
        return;
    }

    page.protected_regions.retain(|region| {
        region.kind != ProtectedRegionKind::Code
            || !region.source.to_ascii_lowercase().contains("paddle")
            || !prompt_boxes.contains(&region.bbox)
    });
    for block in &mut page.blocks {
        if block.kind != BlockKind::Text
            || !native_block_looks_like_prompt(block)
            || !prompt_boxes
                .iter()
                .any(|bbox| prompt_region_overlap(block.bbox, *bbox) >= 0.55)
        {
            continue;
        }
        block.role = BlockRole::Prompt;
        block.policy = policy_for_role(BlockRole::Prompt);
        block.policy.source = PolicyDecisionSource::OcrSemantic;
        block
            .tags
            .push("ocr_code_reclassified_as_prompt".to_string());
        block.metadata.insert(
            "ocr_code_reclassified_as_prompt".to_string(),
            serde_json::Value::Bool(true),
        );
    }
}

fn prompt_region_overlap(block: BoundingBox, region: BoundingBox) -> f32 {
    block.intersection(region).map_or(0.0, |intersection| {
        intersection.area() / block.area().min(region.area()).max(1.0)
    })
}

fn native_block_looks_like_prompt(block: &Block) -> bool {
    let monospaced_ratio = block
        .metadata
        .get("monospaced_ratio")
        .and_then(|value| value.as_f64())
        .unwrap_or_default();
    if monospaced_ratio >= 0.35 {
        return false;
    }
    let text = block.content.text.trim();
    let alphabetic_words = text
        .split(|character: char| !character.is_alphabetic())
        .filter(|word| word.chars().count() >= 2)
        .count();
    if alphabetic_words < 8 || looks_like_executable_code(text) {
        return false;
    }
    let lowercase = text.to_lowercase();
    text.contains('?')
        || text.lines().any(|line| {
            let line = line.trim();
            (line.starts_with('[') && line.contains(']'))
                || line.split_once([':', '：']).is_some_and(|(label, value)| {
                    !label.trim().is_empty()
                        && label.chars().count() <= 48
                        && !value.trim().is_empty()
                })
        })
        || [
            "today is ",
            "plan today",
            "broad strokes",
            "should ",
            "observation",
            "summary description",
        ]
        .iter()
        .any(|cue| lowercase.contains(cue))
}

fn looks_like_executable_code(text: &str) -> bool {
    if ["{", "}", "=>", "::", ":=", "</", "/>"]
        .iter()
        .any(|cue| text.contains(cue))
    {
        return true;
    }
    text.lines().any(|line| {
        let line = line.trim_start().to_ascii_lowercase();
        [
            "def ",
            "class ",
            "fn ",
            "let ",
            "const ",
            "var ",
            "import ",
            "from ",
            "use ",
            "pub ",
            "function ",
        ]
        .iter()
        .any(|prefix| line.starts_with(prefix))
    })
}

fn reclassify_dialogue_blocks(page: &mut Page) {
    for block in &mut page.blocks {
        if block.kind != BlockKind::Text
            || block.policy.locked
            || !block.policy.translate
            || !native_block_looks_like_dialogue(block)
        {
            continue;
        }
        block.role = BlockRole::Dialogue;
        block.policy = policy_for_role(BlockRole::Dialogue);
        if !block.tags.iter().any(|tag| tag == "native_dialogue") {
            block.tags.push("native_dialogue".to_string());
        }
        block.metadata.insert(
            "semantic_line_structure".to_string(),
            json!("speaker_turns"),
        );
    }
}

fn reclassify_standalone_prompt_blocks(page: &mut Page) {
    for block in &mut page.blocks {
        if block.kind != BlockKind::Text
            || block.policy.locked
            || !block.policy.translate
            || native_block_looks_like_dialogue(block)
            || !native_block_looks_like_standalone_prompt(block)
        {
            continue;
        }
        block.role = BlockRole::Prompt;
        block.policy = policy_for_role(BlockRole::Prompt);
        if !block.tags.iter().any(|tag| tag == "native_prompt") {
            block.tags.push("native_prompt".to_string());
        }
        block.metadata.insert(
            "semantic_line_structure".to_string(),
            json!("prompt_fields"),
        );
    }
}

fn native_block_looks_like_standalone_prompt(block: &Block) -> bool {
    native_text_looks_like_prompt(&block.content.text)
}

fn native_text_looks_like_prompt(text: &str) -> bool {
    let lines = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    if lines
        .first()
        .is_some_and(|line| native_bracketed_prompt_heading(line))
    {
        return true;
    }
    lines
        .iter()
        .take(4)
        .filter(|line| native_prompt_field_label(line))
        .count()
        >= 2
}

fn native_bracketed_prompt_heading(line: &str) -> bool {
    line.starts_with('[')
        && line.ends_with(']')
        && line
            .chars()
            .filter(|character| character.is_alphabetic())
            .count()
            >= 4
        && line.chars().count() <= 80
}

fn native_prompt_field_label(line: &str) -> bool {
    let Some((label, _)) = line.split_once([':', '：']) else {
        return false;
    };
    let words = label.split_whitespace().collect::<Vec<_>>();
    !label.is_empty()
        && label.chars().count() <= 40
        && (1..=5).contains(&words.len())
        && words.iter().all(|word| {
            word.chars()
                .all(|character| character.is_alphabetic() || matches!(character, '-' | '\'' | '’'))
        })
}

fn native_block_looks_like_dialogue(block: &Block) -> bool {
    native_text_looks_like_dialogue(&block.content.text)
}

fn native_text_looks_like_dialogue(text: &str) -> bool {
    let labels = text
        .lines()
        .filter_map(native_speaker_label)
        .collect::<Vec<_>>();
    labels.len() >= 3
        && labels
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len()
            >= 2
}

fn native_speaker_label(line: &str) -> Option<String> {
    let (speaker, text) = line.split_once([':', '：'])?;
    let speaker = speaker.trim();
    let words = speaker.split_whitespace().collect::<Vec<_>>();
    (!text.trim().is_empty()
        && speaker.chars().count() <= 24
        && (1..=3).contains(&words.len())
        && words.iter().all(|word| {
            word.chars()
                .all(|character| character.is_alphabetic() || matches!(character, '-' | '\'' | '’'))
        }))
    .then(|| speaker.to_lowercase())
}

fn ocr_role_is_textual(role: BlockRole) -> bool {
    matches!(
        role,
        BlockRole::Title
            | BlockRole::Heading
            | BlockRole::Body
            | BlockRole::Abstract
            | BlockRole::Prompt
            | BlockRole::Dialogue
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
    fn distinguishes_superscripts_from_baseline_small_caps() {
        let segment = |text: &str, bbox: BoundingBox, font_size: f32| Segment {
            kind: SegmentKind::Text,
            text: text.to_string(),
            bbox,
            style: TextStyle {
                font_size: Some(font_size),
                ..TextStyle::default()
            },
        };
        let mut formula = vec![
            segment("n", BoundingBox::new(0.0, 10.0, 6.0, 21.0), 11.0),
            segment("3", BoundingBox::new(6.0, 8.0, 10.0, 16.0), 8.0),
            segment("+n", BoundingBox::new(10.0, 10.0, 22.0, 21.0), 11.0),
        ];
        annotate_baseline_positions(&mut formula);
        assert_eq!(
            formula[1].style.baseline_position,
            BaselinePosition::Superscript
        );

        let mut small_caps = vec![
            segment("W", BoundingBox::new(0.0, 10.0, 7.0, 21.0), 11.0),
            segment("ORD", BoundingBox::new(7.0, 12.0, 20.0, 20.5), 8.5),
            segment("T", BoundingBox::new(20.0, 10.0, 27.0, 21.0), 11.0),
        ];
        annotate_baseline_positions(&mut small_caps);
        assert_eq!(
            small_caps[1].style.baseline_position,
            BaselinePosition::Normal
        );
    }

    #[test]
    fn splits_indented_prose_paragraphs_without_dropping_lines() {
        let style = TextStyle {
            font_size: Some(10.0),
            font_class: Some(FontFamilyClass::Serif),
            ..TextStyle::default()
        };
        let line = |index: usize, x0: f32, x1: f32, text: &str| {
            let y0 = 10.0 + index as f32 * 12.0;
            let bbox = BoundingBox::new(x0, y0, x1, y0 + 10.0);
            Line {
                bbox,
                text: text.to_string(),
                segments: vec![Segment {
                    kind: SegmentKind::Text,
                    text: text.to_string(),
                    bbox,
                    style: style.clone(),
                }],
                baseline_y: Some(y0 + 8.0),
                line_height: Some(10.0),
            }
        };
        let lines = vec![
            line(
                0,
                10.0,
                100.0,
                "This is a sufficiently long opening paragraph",
            ),
            line(
                1,
                10.0,
                100.0,
                "whose visual lines continue at the left edge",
            ),
            line(2, 10.0, 82.0, "and finish with a complete sentence."),
            line(3, 18.0, 100.0, "The second paragraph begins with an indent"),
            line(
                4,
                10.0,
                100.0,
                "and returns to the dominant left edge before",
            ),
            line(5, 10.0, 88.0, "ending without losing any source text."),
        ];
        let block = native_block_from_lines(lines, 0.0, 0.0, 0.0).unwrap();

        let paragraphs = split_native_paragraphs(block);

        assert_eq!(paragraphs.len(), 2);
        assert!(paragraphs[0].text.ends_with("complete sentence."));
        assert!(paragraphs[1].text.starts_with("The second paragraph"));
        assert_eq!(
            paragraphs
                .iter()
                .flat_map(|paragraph| paragraph.lines.iter())
                .count(),
            6
        );
        assert!((paragraphs[1].lines[0].bbox.x0 - paragraphs[1].bbox.x0) >= 7.5);
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

    #[test]
    fn ocr_code_region_is_demoted_for_non_monospace_prompt_prose() {
        let bbox = BoundingBox::new(10.0, 20.0, 90.0, 80.0);
        let mut native_block = fusion_block(
            "native-prompt",
            bbox,
            BlockRole::Body,
            "[Agent’s Summary Description]\nObservation: John saw Eddy taking a short walk.\nShould John react to the observation?",
            TextStyle::default(),
        );
        native_block
            .metadata
            .insert("monospaced_ratio".to_string(), json!(0.0));
        let native = fusion_document("native", vec![native_block]);
        let mut ocr = fusion_document("paddleocr-vl", Vec::new());
        ocr.pages[0].protected_regions.push(ProtectedRegion {
            id: "ocr-code".to_string(),
            page_index: 0,
            kind: ProtectedRegionKind::Code,
            bbox,
            source: "paddleocr-vl".to_string(),
            confidence: 0.99,
            reason: "provider_non_text:code".to_string(),
        });

        let fused = fuse_native_with_ocr(native, &ocr);
        let prompt = &fused.pages[0].blocks[0];

        assert_eq!(prompt.role, BlockRole::Prompt);
        assert!(prompt.policy.translate);
        assert!(!prompt.policy.locked);
        assert!(
            prompt
                .tags
                .contains(&"ocr_code_reclassified_as_prompt".to_string())
        );
        assert!(fused.pages[0].protected_regions.is_empty());
    }

    #[test]
    fn ocr_code_region_remains_locked_for_executable_code() {
        let bbox = BoundingBox::new(10.0, 20.0, 90.0, 80.0);
        let native = fusion_document(
            "native",
            vec![fusion_block(
                "native-code",
                bbox,
                BlockRole::Body,
                "fn main() { println!(\"hello\"); }",
                TextStyle::default(),
            )],
        );
        let mut ocr = fusion_document("paddleocr-vl", Vec::new());
        ocr.pages[0].protected_regions.push(ProtectedRegion {
            id: "ocr-code".to_string(),
            page_index: 0,
            kind: ProtectedRegionKind::Code,
            bbox,
            source: "paddleocr-vl".to_string(),
            confidence: 0.99,
            reason: "provider_non_text:code".to_string(),
        });

        let fused = fuse_native_with_ocr(native, &ocr);
        let code = &fused.pages[0].blocks[0];

        assert_eq!(code.role, BlockRole::Code);
        assert!(!code.policy.translate);
        assert!(code.policy.locked);
        assert_eq!(fused.pages[0].protected_regions.len(), 1);
    }

    #[test]
    fn native_dialogue_is_classified_by_speaker_turns() {
        let bbox = BoundingBox::new(10.0, 20.0, 90.0, 80.0);
        let native = fusion_document(
            "native",
            vec![fusion_block(
                "native-dialogue",
                bbox,
                BlockRole::Body,
                "Interviewer: How are you?\nKlaus Mueller: I am doing well.\nInterviewer: What are you working on?",
                TextStyle::default(),
            )],
        );
        let ocr = fusion_document("ocr", Vec::new());

        let fused = fuse_native_with_ocr(native, &ocr);

        assert_eq!(fused.pages[0].blocks[0].role, BlockRole::Dialogue);
        assert_eq!(
            fused.pages[0].blocks[0]
                .metadata
                .get("semantic_line_structure"),
            Some(&json!("speaker_turns"))
        );
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
