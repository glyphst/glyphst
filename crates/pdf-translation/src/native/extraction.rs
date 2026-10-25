use super::fusion::{
    native_text_looks_like_dialogue, native_text_looks_like_prompt, reclassify_dialogue_blocks,
    reclassify_standalone_prompt_blocks,
};
use super::text_quality::text_looks_broken;
use super::*;
use crate::mupdf_path;
use crate::toc::annotate_table_of_contents_page;

pub fn inspect_pdf(path: &Path, cancellation: &CancellationToken) -> Result<PdfInspection> {
    let document = mupdf_path(path)
        .and_then(Document::open)
        .with_context(|| format!("failed to open PDF {}", path.display()))?;
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
        let mut text = String::new();
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
                                text.push(value);
                                if !value.is_whitespace() {
                                    character_count += 1;
                                }
                                if value == '\u{fffd}' {
                                    replacement_character_count += 1;
                                }
                            }
                        }
                        text.push('\n');
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
            || (image_area_ratio > 0.72 && character_count < 350)
            || text_looks_broken(&text);
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
    let source = mupdf_path(path)
        .and_then(Document::open)
        .with_context(|| format!("failed to open PDF {}", path.display()))?;
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
        let mut native_images = Vec::new();
        for (source_block_index, block) in text_page.blocks().enumerate() {
            if block.r#type() == TextBlockType::Image {
                let bbox = bounding_box(block.bounds());
                let page_area = (geometry.width * geometry.height).max(1.0);
                if bbox.area() >= 1.0 && bbox.area() / page_area < 0.80 {
                    native_images.push(NativeImageCandidate {
                        source_block_index,
                        bbox,
                    });
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
        let mut blocks = Vec::with_capacity(extracted.len());
        for (order, mut extracted) in extracted.into_iter().enumerate() {
            for line in &mut extracted.lines {
                annotate_baseline_positions(&mut line.segments);
            }
            let role = classify_native_role(page_index, &geometry, &extracted, body_font_size);
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
                toc_entry: None,
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
        let inline_objects = detect_and_merge_inline_images(
            page_index,
            &mut blocks,
            &mut protected_regions,
            &native_images,
        );
        let mut page = Page {
            page_index,
            geometry,
            blocks,
            inline_objects,
            protected_regions,
            metadata: BTreeMap::new(),
        };
        annotate_table_of_contents_page(&mut page);
        enforce_page_protected_regions(&mut page);
        reclassify_standalone_prompt_blocks(&mut page);
        reclassify_dialogue_blocks(&mut page);
        pages.push(page);
    }

    let mut document = DocumentV1::new(
        DocumentMetadata {
            document_id: document_id.into(),
            source_path: path.to_path_buf(),
            provider: "mupdf".to_string(),
            provider_version: env!("CARGO_PKG_VERSION").to_string(),
            extra: BTreeMap::new(),
        },
        pages,
    );
    crate::annotate_reference_sections(&mut document);
    document.validate()?;
    Ok(document)
}

#[derive(Debug, Clone, Copy)]
struct NativeImageCandidate {
    source_block_index: usize,
    bbox: BoundingBox,
}

#[derive(Debug, Clone)]
struct InlineImageCluster {
    bbox: BoundingBox,
    baseline_y: f32,
    preceding_block: usize,
    following_block: usize,
    parts: Vec<NativeImageCandidate>,
}

fn detect_and_merge_inline_images(
    page_index: usize,
    blocks: &mut Vec<Block>,
    protected_regions: &mut Vec<ProtectedRegion>,
    images: &[NativeImageCandidate],
) -> Vec<InlineObject> {
    let lines = blocks
        .iter()
        .enumerate()
        .flat_map(|(block_index, block)| {
            block.content.lines.iter().map(move |line| {
                (
                    block_index,
                    line,
                    line.baseline_y.unwrap_or(line.bbox.y1),
                    line.line_height
                        .unwrap_or_else(|| line.bbox.height())
                        .max(1.0),
                )
            })
        })
        .collect::<Vec<_>>();

    let mut anchored_parts = images
        .iter()
        .filter_map(|image| {
            let (_, _, baseline_y, line_height) = lines
                .iter()
                .filter(|(_, line, baseline_y, line_height)| {
                    image.bbox.height() >= *line_height * 0.45
                        && image.bbox.height() <= *line_height * 1.60
                        && image.bbox.width() <= *line_height * 2.0
                        && (image.bbox.y1 - *baseline_y).abs() <= (*line_height * 0.55).max(2.0)
                        && image.bbox.y1 >= line.bbox.y0 - *line_height * 0.25
                        && image.bbox.y0 <= line.bbox.y1 + *line_height * 0.25
                })
                .min_by(|left, right| {
                    (image.bbox.y1 - left.2)
                        .abs()
                        .total_cmp(&(image.bbox.y1 - right.2).abs())
                })?;
            Some((*image, *baseline_y, *line_height))
        })
        .collect::<Vec<_>>();
    anchored_parts.sort_by(|left, right| {
        left.1
            .total_cmp(&right.1)
            .then(left.0.bbox.x0.total_cmp(&right.0.bbox.x0))
    });

    let mut grouped_parts = Vec::<(BoundingBox, f32, f32, Vec<NativeImageCandidate>)>::new();
    for (image, baseline_y, line_height) in anchored_parts {
        let joins_previous =
            grouped_parts
                .last()
                .is_some_and(|(bbox, previous_baseline, previous_height, _)| {
                    (baseline_y - *previous_baseline).abs()
                        <= line_height.max(*previous_height) * 0.35
                        && image.bbox.x0 - bbox.x1 <= line_height.max(*previous_height) * 0.80
                        && image.bbox.x0 >= bbox.x0
                });
        if joins_previous {
            let (bbox, previous_baseline, previous_height, parts) =
                grouped_parts.last_mut().expect("inline image group exists");
            *bbox = bbox.union(image.bbox);
            *previous_baseline = (*previous_baseline + baseline_y) * 0.5;
            *previous_height = previous_height.max(line_height);
            parts.push(image);
        } else {
            grouped_parts.push((image.bbox, baseline_y, line_height, vec![image]));
        }
    }

    let mut clusters = grouped_parts
        .into_iter()
        .filter_map(|(bbox, baseline_y, line_height, parts)| {
            let preceding_block = lines
                .iter()
                .filter(|(_, line, line_baseline, line_height_candidate)| {
                    (*line_baseline - baseline_y).abs()
                        <= line_height.max(*line_height_candidate) * 0.45
                        && line.bbox.x1 <= bbox.x0 + line_height * 0.20
                        && line.bbox.x1 >= bbox.x0 - line_height * 1.20
                })
                .max_by(|left, right| left.1.bbox.x1.total_cmp(&right.1.bbox.x1))?
                .0;
            let following_block = lines
                .iter()
                .filter(
                    |(block_index, line, line_baseline, line_height_candidate)| {
                        *block_index != preceding_block
                            && (*line_baseline - baseline_y).abs()
                                <= line_height.max(*line_height_candidate) * 0.45
                            && line.bbox.x0 >= bbox.x0
                            && line.bbox.x0 <= bbox.x1 + line_height
                    },
                )
                .min_by(|left, right| left.1.bbox.x0.total_cmp(&right.1.bbox.x0))?
                .0;
            Some(InlineImageCluster {
                bbox,
                baseline_y,
                preceding_block,
                following_block,
                parts,
            })
        })
        .collect::<Vec<_>>();
    clusters.sort_by(|left, right| {
        left.baseline_y
            .total_cmp(&right.baseline_y)
            .then(left.bbox.x0.total_cmp(&right.bbox.x0))
    });
    if clusters.is_empty() {
        return Vec::new();
    }

    let mut parents = (0..blocks.len()).collect::<Vec<_>>();
    for cluster in &clusters {
        union_indices(
            &mut parents,
            cluster.preceding_block,
            cluster.following_block,
        );
    }
    for index in 0..parents.len() {
        parents[index] = find_root(&mut parents, index);
    }

    let mut inline_objects = clusters
        .iter()
        .enumerate()
        .map(|(index, cluster)| InlineObject {
            id: format!("p{:03}-o{index:04}", page_index + 1),
            page_index,
            bbox: cluster.bbox,
            baseline_y: cluster.baseline_y,
            parts: cluster
                .parts
                .iter()
                .map(|part| InlineObjectPart {
                    source_block_index: part.source_block_index,
                    bbox: part.bbox,
                })
                .collect(),
        })
        .collect::<Vec<_>>();

    let mut merged_by_root = std::collections::BTreeMap::<usize, Block>::new();
    for (root, _) in parents
        .iter()
        .copied()
        .enumerate()
        .filter(|(index, root)| index == root)
    {
        let member_indices = parents
            .iter()
            .enumerate()
            .filter_map(|(index, candidate_root)| (*candidate_root == root).then_some(index))
            .collect::<Vec<_>>();
        if member_indices.len() < 2 {
            continue;
        }
        let object_indices = clusters
            .iter()
            .enumerate()
            .filter_map(|(index, cluster)| {
                (parents[cluster.preceding_block] == root).then_some(index)
            })
            .collect::<Vec<_>>();
        if let Some(merged) =
            merge_inline_block_group(blocks, &member_indices, &object_indices, &inline_objects)
        {
            merged_by_root.insert(root, merged);
        }
    }

    if merged_by_root.is_empty() {
        return Vec::new();
    }
    let original_blocks = std::mem::take(blocks);
    for (index, block) in original_blocks.into_iter().enumerate() {
        let root = parents[index];
        if let Some(merged) = merged_by_root.get(&root) {
            if index == root {
                blocks.push(merged.clone());
            }
        } else {
            blocks.push(block);
        }
    }

    let moved_regions = inline_objects
        .iter()
        .flat_map(|object| object.parts.iter().map(|part| part.bbox))
        .collect::<Vec<_>>();
    protected_regions.retain(|region| {
        !moved_regions
            .iter()
            .any(|bbox| bbox_similarity(region.bbox, *bbox) >= 0.98)
    });
    inline_objects.retain(|object| {
        let root = clusters
            .iter()
            .find(|cluster| cluster.bbox == object.bbox)
            .map(|cluster| parents[cluster.preceding_block]);
        root.is_some_and(|root| merged_by_root.contains_key(&root))
    });
    inline_objects
}

fn merge_inline_block_group(
    blocks: &[Block],
    member_indices: &[usize],
    object_indices: &[usize],
    objects: &[InlineObject],
) -> Option<Block> {
    let primary_index = *member_indices
        .iter()
        .min_by_key(|index| blocks[**index].order)?;
    let mut merged = blocks[primary_index].clone();
    let mut lines = member_indices
        .iter()
        .flat_map(|index| blocks[*index].content.lines.iter().cloned())
        .collect::<Vec<_>>();
    for object_index in object_indices {
        let object = &objects[*object_index];
        lines.push(Line {
            bbox: object.bbox,
            text: crate::inline_object_marker(&object.id),
            segments: vec![Segment {
                kind: SegmentKind::InlineObject,
                text: crate::inline_object_marker(&object.id),
                bbox: object.bbox,
                style: TextStyle::default(),
            }],
            baseline_y: Some(object.baseline_y),
            line_height: Some(object.bbox.height()),
        });
    }
    lines.sort_by(|left, right| {
        left.baseline_y
            .unwrap_or(left.bbox.y1)
            .total_cmp(&right.baseline_y.unwrap_or(right.bbox.y1))
            .then(left.bbox.x0.total_cmp(&right.bbox.x0))
    });
    let mut merged_lines = Vec::<Line>::new();
    for mut line in lines {
        let joins_previous = merged_lines.last().is_some_and(|previous| {
            let left_baseline = previous.baseline_y.unwrap_or(previous.bbox.y1);
            let right_baseline = line.baseline_y.unwrap_or(line.bbox.y1);
            let height = previous
                .line_height
                .unwrap_or_else(|| previous.bbox.height())
                .max(line.line_height.unwrap_or_else(|| line.bbox.height()));
            (left_baseline - right_baseline).abs() <= height * 0.35
        });
        if !joins_previous {
            merged_lines.push(line);
            continue;
        }
        let previous = merged_lines.last_mut().expect("merged line exists");
        restore_inline_fragment_space(previous, &mut line);
        previous.bbox = previous.bbox.union(line.bbox);
        previous.segments.extend(line.segments);
    }
    for line in &mut merged_lines {
        line.segments
            .sort_by(|left, right| left.bbox.x0.total_cmp(&right.bbox.x0));
        restore_segment_spaces(&mut line.segments);
        line.text = line
            .segments
            .iter()
            .map(|segment| segment.text.as_str())
            .collect();
        line.bbox = line
            .segments
            .iter()
            .map(|segment| segment.bbox)
            .reduce(BoundingBox::union)
            .unwrap_or(line.bbox);
    }
    merged.content.lines = merged_lines;
    merged.content.segments = merged
        .content
        .lines
        .iter()
        .flat_map(|line| line.segments.iter().cloned())
        .collect();
    merged.content.text = merged
        .content
        .lines
        .iter()
        .map(|line| line.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    merged.bbox = merged
        .content
        .lines
        .iter()
        .map(|line| line.bbox)
        .reduce(BoundingBox::union)?;
    merged.source.raw_bbox = merged.bbox;
    merged.metadata.insert(
        "merged_block_ids".to_string(),
        json!(
            member_indices
                .iter()
                .map(|index| blocks[*index].id.as_str())
                .collect::<Vec<_>>()
        ),
    );
    merged.metadata.insert(
        "inline_object_ids".to_string(),
        json!(
            object_indices
                .iter()
                .map(|index| objects[*index].id.as_str())
                .collect::<Vec<_>>()
        ),
    );
    if !merged.tags.iter().any(|tag| tag == "inline_objects") {
        merged.tags.push("inline_objects".to_string());
    }
    Some(merged)
}

fn find_root(parents: &mut [usize], index: usize) -> usize {
    if parents[index] != index {
        parents[index] = find_root(parents, parents[index]);
    }
    parents[index]
}

fn union_indices(parents: &mut [usize], left: usize, right: usize) {
    let left_root = find_root(parents, left);
    let right_root = find_root(parents, right);
    let root = left_root.min(right_root);
    parents[left_root] = root;
    parents[right_root] = root;
}

fn bbox_similarity(left: BoundingBox, right: BoundingBox) -> f32 {
    left.intersection(right).map_or(0.0, |intersection| {
        intersection.area() / left.area().min(right.area()).max(1.0)
    })
}

fn restore_inline_fragment_space(previous: &Line, next: &mut Line) {
    let Some(previous_segment) = previous.segments.last() else {
        return;
    };
    let Some(next_segment) = next.segments.first_mut() else {
        return;
    };
    let left = previous_segment
        .text
        .chars()
        .rev()
        .find(|character| !character.is_whitespace());
    let right = next_segment
        .text
        .chars()
        .find(|character| !character.is_whitespace());
    if left.is_some_and(|character| matches!(character, '.' | '!' | '?' | ':' | ';'))
        && right.is_some_and(char::is_alphanumeric)
        && !previous_segment.text.ends_with(char::is_whitespace)
        && !next_segment.text.starts_with(char::is_whitespace)
    {
        next_segment.text.insert(0, ' ');
    }
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
