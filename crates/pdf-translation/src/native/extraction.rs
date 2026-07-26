use super::fusion::{
    native_text_looks_like_dialogue, native_text_looks_like_prompt, reclassify_dialogue_blocks,
    reclassify_standalone_prompt_blocks,
};
use super::*;

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
                            kind: if flags.intersects(
                                TextCharFlags::UNICODE_IS_CID | TextCharFlags::UNICODE_IS_GID,
                            ) {
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
                    (Some(baseline), Some(font_size)) => {
                        baseline_normalized_line_bbox(horizontal_bbox, baseline, font_size)
                    }
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
                structure: None,
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

pub(super) fn split_native_paragraphs(block: NativeBlock) -> Vec<NativeBlock> {
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

pub(super) fn native_block_from_lines(
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

pub(super) fn merge_inline_native_lines(lines: Vec<Line>) -> Vec<Line> {
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

pub(super) fn restore_segment_spaces(segments: &mut [Segment]) {
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
