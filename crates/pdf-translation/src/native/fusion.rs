use super::extraction::{merge_inline_native_lines, native_block_from_lines};
use super::*;
use crate::toc::{
    annotate_table_of_contents_page, parse_table_of_contents_entry, semantic_member_bbox,
};

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

pub fn fuse_native_with_ocr(native: DocumentV1, ocr: &DocumentV1) -> DocumentV1 {
    fuse_native_with_ocr_report(native, ocr).document
}

pub fn fuse_native_with_ocr_report(mut native: DocumentV1, ocr: &DocumentV1) -> OcrFusionResult {
    let mut report = OcrFusionReport::default();
    for page in &mut native.pages {
        annotate_table_of_contents_page(page);
        let Some(source_ocr_page) = ocr
            .pages
            .iter()
            .find(|candidate| candidate.page_index == page.page_index)
        else {
            continue;
        };
        let expanded_ocr_page = expand_ocr_semantic_regions(source_ocr_page);
        let ocr_page = &expanded_ocr_page;
        let mut page_report = OcrFusionPageReport {
            page_index: page.page_index,
            ..OcrFusionPageReport::default()
        };
        page.protected_regions
            .extend(ocr_page.protected_regions.iter().cloned());
        reclassify_prompt_code_regions(page);
        enforce_page_protected_regions(page);

        let original_blocks = page.blocks.clone();
        let atoms = native_line_atoms(&original_blocks);
        let eligible_ocr = ocr_page
            .blocks
            .iter()
            .enumerate()
            .filter(|(_, block)| {
                !block.policy.locked
                    && matches!(block.kind, BlockKind::Text | BlockKind::Code)
                    && (!block.content.text.trim().is_empty() || block.role != BlockRole::Unknown)
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        let mut assignments = assign_native_lines_to_ocr(
            &atoms,
            &original_blocks,
            ocr_page,
            &eligible_ocr,
            page.geometry.width,
        );
        let (native_to_ocr, ocr_to_atoms, ocr_to_native) =
            index_fusion_relations(&atoms, &assignments);
        let components = topology_components(&native_to_ocr, &ocr_to_native, &ocr_to_atoms);
        let mut consumed_native = HashSet::<usize>::new();
        let mut matched_ocr = HashSet::<usize>::new();
        let mut fused_blocks = Vec::<Block>::new();

        for (component_index, (native_indices, ocr_indices)) in components.into_iter().enumerate() {
            attach_component_native_lines(
                &native_indices,
                &ocr_indices,
                &atoms,
                &original_blocks,
                ocr_page,
                page.geometry.width,
                &mut assignments,
            );
            let coverages = native_indices
                .iter()
                .map(|native_index| {
                    native_assignment_coverage(*native_index, &ocr_indices, &atoms, &assignments)
                })
                .collect::<Vec<_>>();
            let coverage = if coverages.is_empty() {
                0.0
            } else {
                coverages.iter().sum::<f32>() / coverages.len() as f32
            };
            let high_confidence_structure = ocr_indices.iter().all(|ocr_index| {
                ocr_page.blocks[*ocr_index]
                    .structure
                    .as_ref()
                    .is_some_and(|structure| structure.confidence >= 0.95)
            });
            let score =
                component_assignment_score(&native_indices, &ocr_indices, &atoms, &assignments);
            let relation = fusion_relation(native_indices.len(), ocr_indices.len());
            let native_ids = sorted_native_ids(&native_indices, &original_blocks);
            let ocr_ids = sorted_ocr_ids(&ocr_indices, ocr_page);
            if coverage < 0.999 || !high_confidence_structure {
                page_report.native_fallbacks += native_indices.len();
                page_report.decisions.push(OcrFusionDecision {
                    relation: OcrFusionRelation::NativeFallback,
                    native_block_ids: native_ids,
                    ocr_block_ids: ocr_ids,
                    score,
                    native_coverage: coverage,
                    reason: if high_confidence_structure {
                        "native_line_coverage_below_threshold".to_string()
                    } else {
                        "ocr_structure_confidence_below_threshold".to_string()
                    },
                });
                continue;
            }

            let component_id = format!(
                "p{:03}-ocr-structure-{component_index:04}",
                page.page_index + 1
            );
            let group_bbox = ocr_indices
                .iter()
                .map(|index| ocr_structure_bbox(&ocr_page.blocks[*index]))
                .reduce(BoundingBox::union);
            let mut ordered_ocr = ocr_indices.iter().copied().collect::<Vec<_>>();
            ordered_ocr.sort_by_key(|index| {
                ocr_page.blocks[*index]
                    .metadata
                    .get("raw_block_order")
                    .and_then(|value| value.as_u64())
                    .unwrap_or(ocr_page.blocks[*index].reading_order as u64)
            });
            for ocr_index in ordered_ocr {
                let assigned_atoms = assignments
                    .iter()
                    .enumerate()
                    .filter_map(|(atom_index, assignment)| {
                        assignment
                            .is_some_and(|assignment| assignment.ocr_index == ocr_index)
                            .then_some(atom_index)
                    })
                    .filter(|atom_index| native_indices.contains(&atoms[*atom_index].block_index))
                    .collect::<Vec<_>>();
                if assigned_atoms.is_empty() {
                    continue;
                }
                fused_blocks.push(build_topology_fused_block(
                    page.page_index,
                    ocr_index,
                    &component_id,
                    group_bbox,
                    &assigned_atoms,
                    &atoms,
                    &original_blocks,
                    &ocr_page.blocks[ocr_index],
                    score,
                ));
                matched_ocr.insert(ocr_index);
            }
            consumed_native.extend(native_indices.iter().copied());
            match relation {
                OcrFusionRelation::Merge => page_report.accepted_merges += 1,
                OcrFusionRelation::Split => page_report.accepted_splits += 1,
                OcrFusionRelation::ManyToMany => page_report.accepted_many_to_many += 1,
                _ => {}
            }
            page_report.decisions.push(OcrFusionDecision {
                relation,
                native_block_ids: native_ids,
                ocr_block_ids: ocr_ids,
                score,
                native_coverage: coverage,
                reason: format!(
                    "validated_ocr_structure:{}",
                    ocr_provider_name(&ocr_page.blocks, &ocr_indices)
                ),
            });
        }

        let mut output_blocks = original_blocks;
        for (native_index, ocr_indices) in &native_to_ocr {
            if consumed_native.contains(native_index) || ocr_indices.len() != 1 {
                continue;
            }
            let ocr_index = *ocr_indices.iter().next().expect("one OCR relation exists");
            if ocr_to_native.get(&ocr_index).map_or(0, HashSet::len) != 1 {
                continue;
            }
            let scores = ocr_to_atoms
                .get(&ocr_index)
                .into_iter()
                .flatten()
                .filter_map(|atom_index| assignments[*atom_index].map(|value| value.score))
                .collect::<Vec<_>>();
            let score = if scores.is_empty() {
                0.0
            } else {
                scores.iter().sum::<f32>() / scores.len() as f32
            };
            apply_ocr_semantics(
                &mut output_blocks[*native_index],
                &ocr_page.blocks[ocr_index],
                score,
            );
            matched_ocr.insert(ocr_index);
            page_report.one_to_one_matches += 1;
            page_report.decisions.push(OcrFusionDecision {
                relation: OcrFusionRelation::OneToOne,
                native_block_ids: vec![output_blocks[*native_index].id.clone()],
                ocr_block_ids: vec![ocr_page.blocks[ocr_index].id.clone()],
                score,
                native_coverage: native_assignment_coverage(
                    *native_index,
                    &HashSet::from([ocr_index]),
                    &atoms,
                    &assignments,
                ),
                reason: "validated_one_to_one_match".to_string(),
            });
        }

        output_blocks = output_blocks
            .into_iter()
            .enumerate()
            .filter_map(|(index, block)| (!consumed_native.contains(&index)).then_some(block))
            .collect();
        output_blocks.extend(fused_blocks);

        for (ocr_index, ocr_block) in ocr_page.blocks.iter().enumerate() {
            let native_coverage = output_blocks
                .iter()
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
                        && !matched_ocr.contains(&ocr_index)
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
            inserted.source.provider = format!("{}+native-fusion", inserted.source.provider);
            inserted.tags.push("ocr_only".to_string());
            page_report.ocr_only_blocks += 1;
            page_report.decisions.push(OcrFusionDecision {
                relation: OcrFusionRelation::OcrOnly,
                native_block_ids: Vec::new(),
                ocr_block_ids: vec![ocr_block.id.clone()],
                score: 1.0,
                native_coverage,
                reason: "ocr_text_missing_from_native_extraction".to_string(),
            });
            output_blocks.push(inserted);
        }
        page.blocks = output_blocks;
        annotate_table_of_contents_page(page);
        enforce_page_protected_regions(page);
        reclassify_standalone_prompt_blocks(page);
        reclassify_dialogue_blocks(page);
        classify_ocr_structure_groups(page);
        page.blocks.sort_by(|left, right| {
            left.bbox
                .y0
                .total_cmp(&right.bbox.y0)
                .then(left.bbox.x0.total_cmp(&right.bbox.x0))
        });
        for (order, block) in page.blocks.iter_mut().enumerate() {
            block.order = order;
        }
        page.metadata
            .insert("ocr_fused".to_string(), serde_json::Value::Bool(true));
        page.metadata.insert(
            "ocr_structure_merge_count".to_string(),
            json!(page_report.accepted_merges),
        );
        page.metadata.insert(
            "ocr_structure_split_count".to_string(),
            json!(page_report.accepted_splits),
        );
        page.metadata.insert(
            "ocr_structure_fallback_count".to_string(),
            json!(page_report.native_fallbacks),
        );
        report.pages.push(page_report);
    }
    native.metadata.provider = format!("{}+{}", native.metadata.provider, ocr.metadata.provider);
    native.metadata.provider_version = format!(
        "{}+{}",
        native.metadata.provider_version, ocr.metadata.provider_version
    );
    OcrFusionResult {
        document: native,
        report,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OcrLineTopology {
    Prompt,
    Dialogue,
    OrderedList,
    BulletList,
    TableOfContents,
}

fn expand_ocr_semantic_regions(page: &Page) -> Page {
    let mut annotated = page.clone();
    annotate_table_of_contents_page(&mut annotated);
    let mut expanded = annotated.clone();
    let mut blocks = Vec::new();
    for block in &annotated.blocks {
        let Some((topology, members)) = ocr_semantic_line_members(block) else {
            blocks.push(block.clone());
            continue;
        };
        let group_id = format!("{}-semantic-lines", block.id);
        let group_bbox = ocr_structure_bbox(block);
        let member_count = members.len();
        let confidence = block
            .structure
            .as_ref()
            .map_or(block.policy.confidence, |structure| structure.confidence);
        for (member_index, member) in members.into_iter().enumerate() {
            let mut expanded_block = block.clone();
            expanded_block.id = format!("{}-s{member_index:02}", block.id);
            expanded_block.continuation = None;
            expanded_block.role = match topology {
                OcrLineTopology::Prompt => BlockRole::Prompt,
                OcrLineTopology::Dialogue => BlockRole::Dialogue,
                OcrLineTopology::OrderedList | OcrLineTopology::BulletList => BlockRole::Body,
                OcrLineTopology::TableOfContents => BlockRole::TableOfContents,
            };
            expanded_block.policy = policy_for_role(expanded_block.role);
            expanded_block.policy.source = PolicyDecisionSource::OcrSemantic;
            expanded_block.policy.confidence = confidence;
            let toc_entry = matches!(topology, OcrLineTopology::TableOfContents)
                .then(|| parse_table_of_contents_entry(&member))
                .flatten();
            let marker = toc_entry
                .as_ref()
                .and_then(|entry| entry.marker.clone())
                .or_else(|| structural_marker(&member));
            let kind = match topology {
                OcrLineTopology::Prompt
                    if member_index == 0 && native_bracketed_prompt_heading(&member) =>
                {
                    StructureKind::PromptHeading
                }
                OcrLineTopology::Prompt if member.contains(['?', '？']) => {
                    StructureKind::PromptQuestion
                }
                OcrLineTopology::Dialogue => StructureKind::DialogueTurn,
                OcrLineTopology::OrderedList if marker.is_some() => StructureKind::OrderedListItem,
                OcrLineTopology::BulletList if marker.is_some() => StructureKind::BulletListItem,
                OcrLineTopology::TableOfContents => StructureKind::TableOfContentsEntry,
                _ => StructureKind::Region,
            };
            let provider = expanded_block
                .structure
                .as_ref()
                .map(|structure| structure.provider.clone())
                .unwrap_or_else(|| expanded_block.source.provider.clone());
            expanded_block.structure = Some(BlockStructure {
                kind,
                flow: TextFlow::Reflow,
                group_id: Some(group_id.clone()),
                group_bbox: Some(group_bbox),
                marker,
                provider,
                confidence,
            });
            let member_bbox = semantic_member_bbox(group_bbox, member_index, member_count);
            expanded_block.bbox = member_bbox;
            expanded_block.source.raw_bbox = member_bbox;
            expanded_block.toc_entry = toc_entry;
            let style = block
                .content
                .segments
                .first()
                .map(|segment| segment.style.clone())
                .unwrap_or_default();
            let segment = Segment {
                kind: SegmentKind::Text,
                text: member.clone(),
                bbox: member_bbox,
                style,
            };
            expanded_block.content = BlockContent {
                text: member.clone(),
                lines: vec![Line {
                    bbox: member_bbox,
                    text: member,
                    segments: vec![segment.clone()],
                    baseline_y: None,
                    line_height: Some(member_bbox.height()),
                }],
                segments: vec![segment],
                markdown: None,
                html: None,
            };
            expanded_block.tags.push("ocr_semantic_lines".to_string());
            expanded_block.metadata.insert(
                "ocr_semantic_parent_id".to_string(),
                json!(block.id.clone()),
            );
            expanded_block
                .metadata
                .insert("ocr_semantic_member_index".to_string(), json!(member_index));
            blocks.push(expanded_block);
        }
    }
    for (order, block) in blocks.iter_mut().enumerate() {
        block.order = order;
        block.reading_order = order;
    }
    expanded.blocks = blocks;
    expanded
}

fn ocr_semantic_line_members(block: &Block) -> Option<(OcrLineTopology, Vec<String>)> {
    if block.kind != BlockKind::Text
        || block.policy.locked
        || block
            .structure
            .as_ref()
            .is_none_or(|structure| structure.confidence < 0.95)
    {
        return None;
    }
    let raw_lines = block
        .content
        .text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    if raw_lines.len() < 2 {
        return None;
    }
    let ordered_count = raw_lines
        .iter()
        .filter(|line| ordered_list_marker(line).is_some())
        .count();
    let bullet_count = raw_lines
        .iter()
        .filter(|line| bullet_list_marker(line).is_some())
        .count();
    let toc_count = raw_lines
        .iter()
        .filter(|line| parse_table_of_contents_entry(line).is_some())
        .count();
    let topology = if block.role == BlockRole::TableOfContents
        && toc_count >= 3
        && toc_count * 4 >= raw_lines.len() * 3
    {
        OcrLineTopology::TableOfContents
    } else if native_text_looks_like_prompt(&block.content.text) {
        OcrLineTopology::Prompt
    } else if native_text_looks_like_dialogue(&block.content.text) {
        OcrLineTopology::Dialogue
    } else if ordered_count >= 2 {
        OcrLineTopology::OrderedList
    } else if bullet_count >= 2 {
        OcrLineTopology::BulletList
    } else {
        return None;
    };
    let mut members = Vec::<String>::new();
    for line in raw_lines {
        let starts_member = members
            .last()
            .is_none_or(|previous| semantic_ocr_line_boundary(previous, line, topology));
        if starts_member {
            members.push(line.to_string());
        } else if let Some(previous) = members.last_mut() {
            previous.push(' ');
            previous.push_str(line);
        }
    }
    (members.len() >= 2).then_some((topology, members))
}

fn semantic_ocr_line_boundary(previous: &str, current: &str, topology: OcrLineTopology) -> bool {
    if matches!(topology, OcrLineTopology::TableOfContents) {
        return true;
    }
    if matches!(topology, OcrLineTopology::OrderedList) {
        return ordered_list_marker(current).is_some();
    }
    if matches!(topology, OcrLineTopology::BulletList) {
        return bullet_list_marker(current).is_some();
    }
    if matches!(topology, OcrLineTopology::Dialogue) && native_speaker_label(current).is_some() {
        return true;
    }
    if native_bracketed_prompt_heading(current)
        || native_prompt_field_label(current)
        || structural_marker(current).is_some()
    {
        return true;
    }
    previous
        .trim_end()
        .ends_with(['.', '!', '?', '。', '！', '？', ']', '］', ')', '）'])
}

#[derive(Debug, Clone)]
struct NativeLineAtom {
    block_index: usize,
    line: Line,
    weight: usize,
}

#[derive(Debug, Clone, Copy)]
struct LineOcrAssignment {
    ocr_index: usize,
    score: f32,
}

type FusionRelationIndex = (
    HashMap<usize, HashSet<usize>>,
    HashMap<usize, Vec<usize>>,
    HashMap<usize, HashSet<usize>>,
);

fn native_line_atoms(blocks: &[Block]) -> Vec<NativeLineAtom> {
    let mut atoms = Vec::new();
    for (block_index, block) in blocks.iter().enumerate() {
        if block.policy.locked
            || !matches!(block.kind, BlockKind::Text | BlockKind::Code)
            || block.content.text.trim().is_empty()
        {
            continue;
        }
        if block.content.lines.is_empty() {
            atoms.push(NativeLineAtom {
                block_index,
                line: Line {
                    bbox: block.bbox,
                    text: block.content.text.clone(),
                    segments: block.content.segments.clone(),
                    baseline_y: None,
                    line_height: Some(block.bbox.height()),
                },
                weight: significant_text_length(&block.content.text),
            });
            continue;
        }
        atoms.extend(
            block
                .content
                .lines
                .iter()
                .cloned()
                .map(|line| NativeLineAtom {
                    block_index,
                    weight: significant_text_length(&line.text),
                    line,
                }),
        );
    }
    atoms
}

fn assign_native_lines_to_ocr(
    atoms: &[NativeLineAtom],
    native_blocks: &[Block],
    ocr_page: &Page,
    eligible_ocr: &[usize],
    page_width: f32,
) -> Vec<Option<LineOcrAssignment>> {
    atoms
        .iter()
        .map(|atom| {
            let native_block = &native_blocks[atom.block_index];
            let mut candidates = eligible_ocr
                .iter()
                .filter_map(|ocr_index| {
                    let ocr_block = &ocr_page.blocks[*ocr_index];
                    line_ocr_match_score(atom, native_block, ocr_block, page_width)
                        .map(|(score, geometry)| (*ocr_index, score, geometry))
                })
                .collect::<Vec<_>>();
            candidates.sort_by(|left, right| right.1.total_cmp(&left.1));
            let (ocr_index, score, geometry) = *candidates.first()?;
            let margin = candidates
                .get(1)
                .map_or(f32::INFINITY, |candidate| score - candidate.1);
            let structured_toc_match =
                table_of_contents_match_score(native_block, &ocr_page.blocks[ocr_index]).is_some();
            (score >= 0.70 && (structured_toc_match || margin >= 0.08 || geometry >= 0.92))
                .then_some(LineOcrAssignment { ocr_index, score })
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn attach_component_native_lines(
    native_indices: &HashSet<usize>,
    ocr_indices: &HashSet<usize>,
    atoms: &[NativeLineAtom],
    native_blocks: &[Block],
    ocr_page: &Page,
    page_width: f32,
    assignments: &mut [Option<LineOcrAssignment>],
) {
    for (atom_index, atom) in atoms.iter().enumerate() {
        if !native_indices.contains(&atom.block_index) || assignments[atom_index].is_some() {
            continue;
        }
        let native_block = &native_blocks[atom.block_index];
        let best = ocr_indices
            .iter()
            .filter_map(|ocr_index| {
                let ocr_block = &ocr_page.blocks[*ocr_index];
                let (score, geometry) =
                    line_ocr_match_score(atom, native_block, ocr_block, page_width)?;
                (geometry >= 0.82).then_some((*ocr_index, score, geometry))
            })
            .max_by(|left, right| left.2.total_cmp(&right.2).then(left.1.total_cmp(&right.1)));
        if let Some((ocr_index, score, _)) = best {
            assignments[atom_index] = Some(LineOcrAssignment { ocr_index, score });
        }
    }
}

fn line_ocr_match_score(
    atom: &NativeLineAtom,
    native_block: &Block,
    ocr_block: &Block,
    page_width: f32,
) -> Option<(f32, f32)> {
    let ocr_bbox = ocr_structure_bbox(ocr_block);
    let vertical_overlap = axis_overlap(
        atom.line.bbox.y0,
        atom.line.bbox.y1,
        ocr_bbox.y0,
        ocr_bbox.y1,
    ) / atom.line.bbox.height().max(1.0);
    let horizontal_overlap = axis_overlap(
        atom.line.bbox.x0,
        atom.line.bbox.x1,
        ocr_bbox.x0,
        ocr_bbox.x1,
    ) / atom.line.bbox.width().max(1.0);
    let center_inside = bbox_contains_with_padding(ocr_bbox, atom.line.bbox, 2.5);
    let text_similarity = partial_text_similarity(&atom.line.text, &ocr_block.content.text);
    if native_block.toc_entry.is_some() && ocr_block.toc_entry.is_some() {
        let toc_score = table_of_contents_match_score(native_block, ocr_block)?;
        let geometry = (vertical_overlap.clamp(0.0, 1.0) * 0.65
            + horizontal_overlap.clamp(0.0, 1.0) * 0.25
            + if center_inside { 0.10 } else { 0.0 })
        .clamp(0.0, 1.0);
        return Some((toc_score, geometry.max(0.50)));
    }
    if (vertical_overlap < 0.20 || horizontal_overlap < 0.05)
        && !(text_similarity >= 0.90 && native_block.bbox.intersection(ocr_bbox).is_some())
    {
        return None;
    }
    // A native text block can span the page even when OCR correctly divides it
    // into narrower semantic regions, while the last visual line of one region
    // can itself be much shorter than the region. Accept either the physical
    // line or its enclosing native block as column-compatible. A genuinely
    // cross-column OCR region remains incompatible with both.
    if !column_compatible(atom.line.bbox, ocr_bbox, page_width, ocr_block.role)
        && !column_compatible(native_block.bbox, ocr_bbox, page_width, ocr_block.role)
    {
        return None;
    }
    let geometry = (vertical_overlap.clamp(0.0, 1.0) * 0.65
        + horizontal_overlap.clamp(0.0, 1.0) * 0.25
        + if center_inside { 0.10 } else { 0.0 })
    .clamp(0.0, 1.0);
    let score = geometry * 0.60 + text_similarity * 0.30 + 0.10;
    Some((score, geometry))
}

fn table_of_contents_match_score(native_block: &Block, ocr_block: &Block) -> Option<f32> {
    let native = native_block.toc_entry.as_ref()?;
    let ocr = ocr_block.toc_entry.as_ref()?;
    if native.marker.as_deref().map(normalize_match_text)
        != ocr.marker.as_deref().map(normalize_match_text)
        || normalize_match_text(&native.page_label) != normalize_match_text(&ocr.page_label)
    {
        return None;
    }
    let title_similarity = normalized_text_similarity(&native.title, &ocr.title);
    (title_similarity >= 0.72).then_some(0.84 + title_similarity * 0.15)
}

fn ocr_structure_bbox(block: &Block) -> BoundingBox {
    let raw_bbox = block.source.raw_bbox;
    if block
        .structure
        .as_ref()
        .is_some_and(|structure| structure.confidence >= 0.95)
        && raw_bbox.is_valid()
        && raw_bbox.area() >= 1.0
    {
        raw_bbox
    } else {
        block.bbox
    }
}

fn axis_overlap(left_start: f32, left_end: f32, right_start: f32, right_end: f32) -> f32 {
    (left_end.min(right_end) - left_start.max(right_start)).max(0.0)
}

fn bbox_contains_with_padding(container: BoundingBox, line: BoundingBox, padding: f32) -> bool {
    let x = (line.x0 + line.x1) * 0.5;
    let y = (line.y0 + line.y1) * 0.5;
    x >= container.x0 - padding
        && x <= container.x1 + padding
        && y >= container.y0 - padding
        && y <= container.y1 + padding
}

fn column_compatible(
    native: BoundingBox,
    ocr: BoundingBox,
    page_width: f32,
    ocr_role: BlockRole,
) -> bool {
    if page_width <= 0.0 {
        return true;
    }
    let native_full = native.width() >= page_width * 0.62;
    let ocr_full = ocr.width() >= page_width * 0.62;
    if native_full == ocr_full {
        return true;
    }
    if ocr_full
        && matches!(
            ocr_role,
            BlockRole::Title | BlockRole::FigureCaption | BlockRole::TableCaption
        )
    {
        return true;
    }
    let native_side = ((native.x0 + native.x1) * 0.5) <= page_width * 0.5;
    let ocr_side = ((ocr.x0 + ocr.x1) * 0.5) <= page_width * 0.5;
    native_side == ocr_side
}

fn partial_text_similarity(left: &str, right: &str) -> f32 {
    let left_normalized = normalize_match_text(left);
    let right_normalized = normalize_match_text(right);
    if left_normalized.is_empty() || right_normalized.is_empty() {
        let left_marker = structural_marker(left);
        let right_marker = structural_marker(right);
        return if left_marker.is_some() && left_marker == right_marker {
            1.0
        } else {
            0.0
        };
    }
    if left_normalized.contains(&right_normalized) || right_normalized.contains(&left_normalized) {
        return 1.0;
    }
    normalized_text_similarity(&left_normalized, &right_normalized)
}

fn significant_text_length(text: &str) -> usize {
    text.chars()
        .filter(|character| !character.is_whitespace())
        .count()
        .max(1)
}

fn index_fusion_relations(
    atoms: &[NativeLineAtom],
    assignments: &[Option<LineOcrAssignment>],
) -> FusionRelationIndex {
    let mut native_to_ocr = HashMap::<usize, HashSet<usize>>::new();
    let mut ocr_to_atoms = HashMap::<usize, Vec<usize>>::new();
    let mut ocr_to_native = HashMap::<usize, HashSet<usize>>::new();
    for (atom_index, assignment) in assignments.iter().enumerate() {
        let Some(assignment) = assignment else {
            continue;
        };
        let native_index = atoms[atom_index].block_index;
        native_to_ocr
            .entry(native_index)
            .or_default()
            .insert(assignment.ocr_index);
        ocr_to_atoms
            .entry(assignment.ocr_index)
            .or_default()
            .push(atom_index);
        ocr_to_native
            .entry(assignment.ocr_index)
            .or_default()
            .insert(native_index);
    }
    (native_to_ocr, ocr_to_atoms, ocr_to_native)
}

fn topology_components(
    native_to_ocr: &HashMap<usize, HashSet<usize>>,
    ocr_to_native: &HashMap<usize, HashSet<usize>>,
    ocr_to_atoms: &HashMap<usize, Vec<usize>>,
) -> Vec<(HashSet<usize>, HashSet<usize>)> {
    let mut seeds = Vec::<(bool, usize)>::new();
    seeds.extend(
        native_to_ocr
            .iter()
            .filter(|(_, ocr)| ocr.len() > 1)
            .map(|(native, _)| (true, *native)),
    );
    seeds.extend(
        ocr_to_native
            .iter()
            .filter(|(_, native)| native.len() > 1)
            .map(|(ocr, _)| (false, *ocr)),
    );
    let mut visited_native = HashSet::new();
    let mut visited_ocr = HashSet::new();
    let mut components = Vec::new();
    for seed in seeds {
        if (seed.0 && visited_native.contains(&seed.1))
            || (!seed.0 && visited_ocr.contains(&seed.1))
        {
            continue;
        }
        let mut native_component = HashSet::new();
        let mut ocr_component = HashSet::new();
        let mut queue = VecDeque::from([seed]);
        while let Some((is_native, index)) = queue.pop_front() {
            if is_native {
                if !visited_native.insert(index) {
                    continue;
                }
                native_component.insert(index);
                for ocr_index in native_to_ocr.get(&index).into_iter().flatten() {
                    queue.push_back((false, *ocr_index));
                }
            } else {
                if !visited_ocr.insert(index) {
                    continue;
                }
                ocr_component.insert(index);
                for native_index in ocr_to_native.get(&index).into_iter().flatten() {
                    queue.push_back((true, *native_index));
                }
            }
        }
        if !native_component.is_empty()
            && !ocr_component.is_empty()
            && ocr_component
                .iter()
                .any(|index| ocr_to_atoms.contains_key(index))
        {
            components.push((native_component, ocr_component));
        }
    }
    components
}

fn native_assignment_coverage(
    native_index: usize,
    ocr_indices: &HashSet<usize>,
    atoms: &[NativeLineAtom],
    assignments: &[Option<LineOcrAssignment>],
) -> f32 {
    let mut total = 0usize;
    let mut assigned = 0usize;
    for (atom_index, atom) in atoms.iter().enumerate() {
        if atom.block_index != native_index {
            continue;
        }
        total += atom.weight;
        if assignments[atom_index]
            .is_some_and(|assignment| ocr_indices.contains(&assignment.ocr_index))
        {
            assigned += atom.weight;
        }
    }
    assigned as f32 / total.max(1) as f32
}

fn component_assignment_score(
    native_indices: &HashSet<usize>,
    ocr_indices: &HashSet<usize>,
    atoms: &[NativeLineAtom],
    assignments: &[Option<LineOcrAssignment>],
) -> f32 {
    let scores = atoms
        .iter()
        .enumerate()
        .filter(|(_, atom)| native_indices.contains(&atom.block_index))
        .filter_map(|(index, _)| assignments[index])
        .filter(|assignment| ocr_indices.contains(&assignment.ocr_index))
        .map(|assignment| assignment.score)
        .collect::<Vec<_>>();
    if scores.is_empty() {
        0.0
    } else {
        scores.iter().sum::<f32>() / scores.len() as f32
    }
}

fn fusion_relation(native_count: usize, ocr_count: usize) -> OcrFusionRelation {
    match (native_count, ocr_count) {
        (1, count) if count > 1 => OcrFusionRelation::Split,
        (count, 1) if count > 1 => OcrFusionRelation::Merge,
        _ => OcrFusionRelation::ManyToMany,
    }
}

fn sorted_native_ids(indices: &HashSet<usize>, blocks: &[Block]) -> Vec<String> {
    let mut indices = indices.iter().copied().collect::<Vec<_>>();
    indices.sort_unstable();
    indices
        .into_iter()
        .map(|index| blocks[index].id.clone())
        .collect()
}

fn sorted_ocr_ids(indices: &HashSet<usize>, page: &Page) -> Vec<String> {
    let mut indices = indices.iter().copied().collect::<Vec<_>>();
    indices.sort_unstable();
    indices
        .into_iter()
        .map(|index| page.blocks[index].id.clone())
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn build_topology_fused_block(
    page_index: usize,
    ocr_index: usize,
    component_id: &str,
    group_bbox: Option<BoundingBox>,
    atom_indices: &[usize],
    atoms: &[NativeLineAtom],
    native_blocks: &[Block],
    ocr_block: &Block,
    score: f32,
) -> Block {
    let mut lines = atom_indices
        .iter()
        .map(|index| atoms[*index].line.clone())
        .collect::<Vec<_>>();
    lines.sort_by(|left, right| {
        left.bbox
            .y0
            .total_cmp(&right.bbox.y0)
            .then(left.bbox.x0.total_cmp(&right.bbox.x0))
    });
    let mut lines = merge_inline_native_lines(lines);
    promote_detached_structural_marker(&mut lines, &ocr_block.content.text);
    let first_native_index = atom_indices
        .iter()
        .map(|index| atoms[*index].block_index)
        .min()
        .expect("topology fusion has native atoms");
    let first_native = &native_blocks[first_native_index];
    let fallback_ratio = |name: &str| {
        first_native
            .metadata
            .get(name)
            .and_then(|value| value.as_f64())
            .unwrap_or_default() as f32
    };
    let rebuilt = native_block_from_lines(
        lines,
        fallback_ratio("bold_ratio"),
        fallback_ratio("italic_ratio"),
        fallback_ratio("monospaced_ratio"),
    )
    .expect("topology fusion lines contain text");
    let mut metadata = first_native.metadata.clone();
    metadata.insert(
        "average_font_size".to_string(),
        json!(rebuilt.average_font_size),
    );
    metadata.insert("bold_ratio".to_string(), json!(rebuilt.bold_ratio));
    metadata.insert("italic_ratio".to_string(), json!(rebuilt.italic_ratio));
    metadata.insert(
        "monospaced_ratio".to_string(),
        json!(rebuilt.monospaced_ratio),
    );
    metadata.insert("ocr_match_score".to_string(), json!(score));
    metadata.insert("ocr_topology_component".to_string(), json!(component_id));
    let mut native_ids = atom_indices
        .iter()
        .map(|index| native_blocks[atoms[*index].block_index].id.clone())
        .collect::<Vec<_>>();
    native_ids.sort();
    native_ids.dedup();
    metadata.insert("native_source_block_ids".to_string(), json!(native_ids));

    let mut structure = ocr_block.structure.clone().unwrap_or(BlockStructure {
        kind: StructureKind::Region,
        flow: TextFlow::Reflow,
        group_id: None,
        group_bbox: None,
        marker: None,
        provider: ocr_block.source.provider.clone(),
        confidence: ocr_block.policy.confidence.max(0.95),
    });
    structure.group_id = Some(component_id.to_string());
    structure.group_bbox = group_bbox;
    let role = resolved_ocr_role(
        ocr_block.role,
        atom_indices
            .iter()
            .map(|index| native_blocks[atoms[*index].block_index].role),
    );
    let mut policy = policy_for_role(role);
    policy.source = PolicyDecisionSource::OcrSemantic;
    policy.confidence = structure.confidence;
    let segments = rebuilt
        .lines
        .iter()
        .flat_map(|line| line.segments.iter().cloned())
        .collect();
    let mut tags = first_native.tags.clone();
    tags.push("ocr_topology_fused".to_string());
    tags.push(format!("ocr_role:{}", role.as_str()));
    Block {
        id: format!("p{:03}-f{ocr_index:04}", page_index + 1),
        page_index,
        order: ocr_block.order,
        reading_order: ocr_block.reading_order,
        kind: BlockKind::Text,
        role,
        bbox: rebuilt.bbox,
        content: BlockContent {
            text: rebuilt.text,
            lines: rebuilt.lines,
            segments,
            markdown: None,
            html: None,
        },
        policy,
        continuation: ocr_block.continuation.clone(),
        structure: Some(structure),
        toc_entry: first_native
            .toc_entry
            .clone()
            .or_else(|| ocr_block.toc_entry.clone()),
        source: SourceProvenance {
            provider: format!("mupdf+{}-structure", ocr_block.source.provider),
            raw_label: ocr_block.source.raw_label.clone(),
            raw_bbox: ocr_block.bbox,
            raw_path: ocr_block.source.raw_path.clone(),
        },
        tags,
        metadata,
    }
}

fn promote_detached_structural_marker(lines: &mut Vec<Line>, ocr_text: &str) {
    if structural_marker(ocr_text).is_none() {
        return;
    }
    let Some(marker_index) = lines.iter().position(|line| {
        structural_marker(&line.text).is_some_and(|marker| line.text.trim() == marker)
    }) else {
        return;
    };
    if marker_index > 0 {
        let marker = lines.remove(marker_index);
        lines.insert(0, marker);
    }
}

fn ocr_provider_name(blocks: &[Block], indices: &HashSet<usize>) -> String {
    let mut providers = indices
        .iter()
        .filter_map(|index| blocks.get(*index))
        .map(|block| block.source.provider.trim())
        .filter(|provider| !provider.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    providers.sort();
    providers.dedup();
    if providers.is_empty() {
        "unknown".to_string()
    } else {
        providers.join("+")
    }
}

fn resolved_ocr_role(
    ocr_role: BlockRole,
    native_roles: impl IntoIterator<Item = BlockRole>,
) -> BlockRole {
    if !matches!(ocr_role, BlockRole::Unknown | BlockRole::Body) {
        return ocr_role;
    }
    native_roles
        .into_iter()
        .filter(|role| !matches!(role, BlockRole::Unknown | BlockRole::Body))
        .max_by_key(|role| native_role_specificity(*role))
        .unwrap_or(ocr_role)
}

fn native_role_specificity(role: BlockRole) -> u8 {
    match role {
        BlockRole::Title => 6,
        BlockRole::Heading => 5,
        BlockRole::Prompt
        | BlockRole::Dialogue
        | BlockRole::Code
        | BlockRole::TableOfContents
        | BlockRole::FigureCaption
        | BlockRole::TableCaption => 4,
        BlockRole::Abstract | BlockRole::Footnote => 3,
        BlockRole::Metadata
        | BlockRole::Reference
        | BlockRole::Header
        | BlockRole::Footer
        | BlockRole::PageNumber
        | BlockRole::FormulaNumber => 2,
        BlockRole::Body
        | BlockRole::Image
        | BlockRole::Table
        | BlockRole::Formula
        | BlockRole::Unknown => 1,
    }
}

fn apply_ocr_semantics(native_block: &mut Block, ocr_block: &Block, score: f32) {
    let role = resolved_ocr_role(ocr_block.role, std::iter::once(native_block.role));
    if ocr_role_is_textual(role) && role != BlockRole::Unknown {
        native_block.role = role;
        native_block.policy = policy_for_role(role);
        native_block.policy.source = PolicyDecisionSource::OcrSemantic;
        native_block.policy.confidence = ocr_block
            .structure
            .as_ref()
            .map_or(0.96, |structure| structure.confidence);
    }
    if native_block.continuation.is_none() {
        native_block
            .continuation
            .clone_from(&ocr_block.continuation);
    }
    native_block.structure.clone_from(&ocr_block.structure);
    if native_block.toc_entry.is_none() {
        native_block.toc_entry.clone_from(&ocr_block.toc_entry);
    }
    native_block.reading_order = ocr_block.reading_order;
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

fn classify_ocr_structure_groups(page: &mut Page) {
    let mut groups = HashMap::<String, Vec<usize>>::new();
    for (index, block) in page.blocks.iter().enumerate() {
        let Some(group_id) = block
            .structure
            .as_ref()
            .and_then(|structure| structure.group_id.clone())
        else {
            continue;
        };
        groups.entry(group_id).or_default().push(index);
    }
    for (group_id, mut members) in groups {
        members.sort_by(|left, right| {
            page.blocks[*left]
                .bbox
                .y0
                .total_cmp(&page.blocks[*right].bbox.y0)
                .then(
                    page.blocks[*left]
                        .bbox
                        .x0
                        .total_cmp(&page.blocks[*right].bbox.x0),
                )
        });
        let group_bbox = members
            .iter()
            .map(|index| {
                page.blocks[*index]
                    .structure
                    .as_ref()
                    .and_then(|structure| structure.group_bbox)
                    .unwrap_or(page.blocks[*index].bbox)
            })
            .reduce(BoundingBox::union);
        let ordered_count = members
            .iter()
            .filter(|index| ordered_list_marker(&page.blocks[**index].content.text).is_some())
            .count();
        let bullet_count = members
            .iter()
            .filter(|index| bullet_list_marker(&page.blocks[**index].content.text).is_some())
            .count();
        let semantic_prompt_count = members
            .iter()
            .filter(|index| page.blocks[**index].role == BlockRole::Prompt)
            .count();
        let toc_group = members.iter().any(|index| {
            page.blocks[*index].role == BlockRole::TableOfContents
                || page.blocks[*index].toc_entry.is_some()
                || page.blocks[*index]
                    .structure
                    .as_ref()
                    .is_some_and(|structure| structure.kind == StructureKind::TableOfContentsEntry)
        });
        let bracketed_prompt_heading = members.first().is_some_and(|index| {
            page.blocks[*index]
                .content
                .text
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty())
                .is_some_and(native_bracketed_prompt_heading)
        });
        let prompt_group = !toc_group
            && (ordered_count >= 2
                || semantic_prompt_count >= 2
                || (members.len() >= 2 && bracketed_prompt_heading));
        let bullet_group = !toc_group && bullet_count >= 2;
        for (position, index) in members.iter().enumerate() {
            let block = &mut page.blocks[*index];
            let ordered_marker = ordered_list_marker(&block.content.text);
            let bullet_marker = bullet_list_marker(&block.content.text);
            let toc_marker = block
                .toc_entry
                .as_ref()
                .and_then(|entry| entry.marker.clone());
            let structure = block.structure.get_or_insert_with(|| BlockStructure {
                kind: StructureKind::Region,
                flow: TextFlow::Reflow,
                group_id: Some(group_id.clone()),
                group_bbox,
                marker: None,
                provider: block.source.provider.clone(),
                confidence: block.policy.confidence.max(0.95),
            });
            structure.group_id = Some(group_id.clone());
            structure.group_bbox = group_bbox;
            structure.flow = TextFlow::Reflow;
            if toc_group {
                block.role = BlockRole::TableOfContents;
                block.policy = policy_for_role(BlockRole::TableOfContents);
                block.policy.source = PolicyDecisionSource::OcrSemantic;
                structure.kind = StructureKind::TableOfContentsEntry;
                structure.marker = toc_marker;
            } else if prompt_group {
                block.role = BlockRole::Prompt;
                block.policy = policy_for_role(BlockRole::Prompt);
                structure.kind = if ordered_marker.is_some() {
                    StructureKind::OrderedListItem
                } else if position == 0 {
                    StructureKind::PromptHeading
                } else if block.content.text.contains(['?', '？']) {
                    StructureKind::PromptQuestion
                } else {
                    StructureKind::Region
                };
                structure.marker = ordered_marker;
            } else if bullet_group {
                block.role = BlockRole::Body;
                block.policy = policy_for_role(BlockRole::Body);
                structure.kind = StructureKind::BulletListItem;
                structure.marker = bullet_marker;
            } else if matches!(
                block.role,
                BlockRole::FigureCaption | BlockRole::TableCaption
            ) {
                structure.kind = StructureKind::Caption;
                structure.flow = TextFlow::Reflow;
            }
        }
    }
}

pub(super) fn structural_marker(text: &str) -> Option<String> {
    ordered_list_marker(text).or_else(|| bullet_list_marker(text))
}

fn ordered_list_marker(text: &str) -> Option<String> {
    let trimmed = text.trim_start();
    let mut digits = String::new();
    let mut chars = trimmed.chars();
    for character in chars.by_ref() {
        if character.is_ascii_digit() && digits.len() < 4 {
            digits.push(character);
            continue;
        }
        if digits.is_empty() || !matches!(character, '.' | ')' | '）' | '．' | '、') {
            return None;
        }
        return Some(format!("{digits}{character}"));
    }
    None
}

fn bullet_list_marker(text: &str) -> Option<String> {
    text.trim_start()
        .chars()
        .next()
        .filter(|character| matches!(character, '•' | '‣' | '◦' | '*' | '-' | '–'))
        .map(|character| character.to_string())
}

fn reclassify_prompt_code_regions(page: &mut Page) {
    let prompt_boxes = page
        .protected_regions
        .iter()
        .filter(|region| {
            region.kind == ProtectedRegionKind::Code && is_ocr_region_source(&region.source)
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
            || !is_ocr_region_source(&region.source)
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

fn is_ocr_region_source(source: &str) -> bool {
    let source = source.to_ascii_lowercase();
    source.contains("ocr") || source.contains("paddle") || source.contains("unlimited")
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

pub(super) fn reclassify_dialogue_blocks(page: &mut Page) {
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

pub(super) fn reclassify_standalone_prompt_blocks(page: &mut Page) {
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

pub(super) fn native_text_looks_like_prompt(text: &str) -> bool {
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

pub(super) fn native_text_looks_like_dialogue(text: &str) -> bool {
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

pub(super) fn reconciled_bbox(
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
