use super::*;

fn push_display_fragment(
    text: &mut String,
    display_to_source: &mut Vec<usize>,
    fragment: &str,
    source_offset: usize,
) {
    text.push_str(fragment);
    display_to_source.extend(std::iter::repeat_n(source_offset, fragment.len()));
}

fn push_source_fragment(
    text: &mut String,
    display_to_source: &mut Vec<usize>,
    source_to_display: &mut [usize],
    source: &str,
    range: Range<usize>,
) {
    let display_start = text.len();
    let fragment = &source[range.clone()];
    text.push_str(fragment);
    for (relative_offset, ch) in fragment.char_indices() {
        let source_offset = range.start + relative_offset;
        let display_offset = display_start + relative_offset;
        source_to_display[source_offset] = display_offset;
        for byte_ix in 1..=ch.len_utf8() {
            display_to_source.push(if byte_ix == ch.len_utf8() {
                source_offset + ch.len_utf8()
            } else {
                source_offset
            });
        }
    }
    source_to_display[range.end] = text.len();
}

pub(super) fn mention_range_for_empty_page_number_backspace(
    active: &ActivePageNumberEdit,
    cursor: usize,
    mentions: &[ComposerMention],
) -> Option<Range<usize>> {
    if !active.digits.is_empty() || cursor != active.digits.start {
        return None;
    }
    mentions
        .iter()
        .find(|mention| mention.range.start == active.mention_start)
        .map(|mention| mention.range.clone())
}

pub(super) fn composer_display(source: &str, mentions: &[ComposerMention]) -> ComposerDisplay {
    let mut text = String::with_capacity(source.len() + mentions.len() * 8);
    let mut source_to_display = vec![0; source.len() + 1];
    let mut display_to_source = vec![0];
    let mut display_mentions = Vec::with_capacity(mentions.len());
    let mut source_cursor = 0;

    for mention in mentions {
        if mention.range.start < source_cursor || mention.range.end > source.len() {
            continue;
        }
        push_source_fragment(
            &mut text,
            &mut display_to_source,
            &mut source_to_display,
            source,
            source_cursor..mention.range.start,
        );
        let display_start = text.len();
        source_to_display[mention.range.clone()].fill(display_start);
        push_display_fragment(
            &mut text,
            &mut display_to_source,
            "\u{00a0}\u{00a0}",
            mention.range.start,
        );
        let icon_start = text.len();
        push_display_fragment(
            &mut text,
            &mut display_to_source,
            "\u{00a0}\u{00a0}",
            mention.range.start,
        );
        let icon_range = icon_start..text.len();
        push_display_fragment(
            &mut text,
            &mut display_to_source,
            "\u{00a0}",
            mention.range.start,
        );
        push_display_fragment(
            &mut text,
            &mut display_to_source,
            mention.reference.kind.label(),
            mention.range.start,
        );
        push_display_fragment(
            &mut text,
            &mut display_to_source,
            "\u{00a0}",
            mention.range.start,
        );

        let number_range = AiPanel::page_digits_range(mention).map(|digits| {
            let number_start = text.len();
            push_display_fragment(&mut text, &mut display_to_source, "\u{00a0}", digits.start);
            source_to_display[digits.start] = text.len();
            push_source_fragment(
                &mut text,
                &mut display_to_source,
                &mut source_to_display,
                source,
                digits.clone(),
            );
            push_display_fragment(&mut text, &mut display_to_source, "\u{00a0}", digits.end);
            number_start..text.len()
        });
        push_display_fragment(
            &mut text,
            &mut display_to_source,
            "\u{00a0}\u{00a0}",
            mention.range.end,
        );
        source_to_display[mention.range.end] = text.len();
        let display_end = text.len();
        let end_mapping_start = number_range
            .as_ref()
            .map_or(display_start + (display_end - display_start) / 2, |range| {
                range.end
            });
        display_to_source[end_mapping_start..=display_end].fill(mention.range.end);
        display_mentions.push(DisplayMention {
            kind: mention.reference.kind,
            source_range: mention.range.clone(),
            range: display_start..display_end,
            icon_range,
            number_range,
        });
        push_display_fragment(
            &mut text,
            &mut display_to_source,
            "\u{00a0}",
            mention.range.end,
        );
        source_cursor = mention.range.end;
    }
    push_source_fragment(
        &mut text,
        &mut display_to_source,
        &mut source_to_display,
        source,
        source_cursor..source.len(),
    );
    source_to_display[source.len()] = text.len();
    if let Some(last) = display_to_source.last_mut() {
        *last = source.len();
    }

    ComposerDisplay {
        text,
        source_to_display,
        display_to_source,
        mentions: display_mentions,
    }
}

pub(super) fn input_text_runs(
    text: &str,
    base_run: TextRun,
    theme: Theme,
    composer_mentions: &[DisplayMention],
    marked_range: Option<&Range<usize>>,
) -> Vec<TextRun> {
    let mut boundaries = vec![0, text.len()];
    for mention in composer_mentions {
        boundaries.push(mention.range.start);
        boundaries.push(mention.range.end);
        if let Some(number_range) = &mention.number_range {
            boundaries.push(number_range.start);
            boundaries.push(number_range.end);
        }
    }
    if let Some(range) = marked_range {
        boundaries.push(range.start);
        boundaries.push(range.end);
    }
    boundaries.sort_unstable();
    boundaries.dedup();

    boundaries
        .windows(2)
        .filter_map(|window| {
            let start = window[0];
            let end = window[1];
            if start >= end {
                return None;
            }

            let mention = composer_mentions
                .iter()
                .any(|mention| start >= mention.range.start && end <= mention.range.end);
            let number = composer_mentions.iter().any(|mention| {
                mention
                    .number_range
                    .as_ref()
                    .is_some_and(|range| start >= range.start && end <= range.end)
            });
            let marked = marked_range
                .as_ref()
                .is_some_and(|range| start >= range.start && end <= range.end);
            let mut run = TextRun {
                len: end - start,
                ..base_run.clone()
            };
            if mention {
                run.color = rgb(theme.accent).into();
            }
            if number {
                run.color = rgb(theme.text).into();
            }
            if marked {
                run.underline = Some(gpui::UnderlineStyle {
                    color: Some(run.color),
                    thickness: px(1.0),
                    wavy: false,
                });
            }
            Some(run)
        })
        .collect()
}

pub(super) fn line_starts(text: &str) -> impl Iterator<Item = usize> + '_ {
    std::iter::once(0).chain(
        text.char_indices()
            .filter_map(|(ix, ch)| (ch == '\n').then_some(ix + ch.len_utf8())),
    )
}

pub(super) fn source_index_for_mouse_position(
    content_len: usize,
    bounds: Bounds<Pixels>,
    lines: &[LaidOutLine],
    display: Option<&ComposerDisplay>,
    position: GpuiPoint<Pixels>,
    line_height: Pixels,
) -> usize {
    let local = match bounds.localize(&position) {
        Some(local) => local,
        None if position.y < bounds.top() => return 0,
        None => return content_len,
    };

    let mut line_top = px(0.0);
    for laid_out in lines {
        let line_height_px = laid_out.line.size(line_height).height;
        if local.y <= line_top + line_height_px {
            let line_point = point(local.x, local.y - line_top);
            let local_ix = laid_out
                .line
                .closest_index_for_position(line_point, line_height)
                .unwrap_or_else(|ix| ix);
            let display_offset = laid_out.start + local_ix;
            return display
                .map(|display| display.source_offset(display_offset))
                .unwrap_or(display_offset)
                .min(content_len);
        }
        line_top += line_height_px;
    }

    content_len
}

pub(super) fn mention_aware_word_left(
    content: &str,
    offset: usize,
    mentions: &[ComposerMention],
) -> usize {
    let target = previous_word_start(content, offset);
    mentions
        .iter()
        .find(|mention| mention.range.start < target && target < mention.range.end)
        .map_or(target, |mention| mention.range.start)
}

pub(super) fn mention_aware_word_right(
    content: &str,
    offset: usize,
    mentions: &[ComposerMention],
) -> usize {
    let target = next_word_end(content, offset);
    mentions
        .iter()
        .find(|mention| mention.range.start < target && target < mention.range.end)
        .map_or(target, |mention| mention.range.end)
}

pub(super) fn expand_range_to_mentions(
    mut range: Range<usize>,
    mentions: &[ComposerMention],
) -> Range<usize> {
    if range.is_empty() {
        return range;
    }
    for mention in mentions {
        if range.start < mention.range.end && range.end > mention.range.start {
            range.start = range.start.min(mention.range.start);
            range.end = range.end.max(mention.range.end);
        }
    }
    range
}

pub(super) fn selection_range_for_click(
    content: &str,
    mentions: &[ComposerMention],
    source_offset: usize,
    click_count: usize,
) -> Range<usize> {
    let offset = mention_aware_source_offset(content, mentions, source_offset);
    if click_count >= 3 {
        let start = content[..offset]
            .rfind('\n')
            .map_or(0, |ix| ix + '\n'.len_utf8());
        let end = content[offset..]
            .find('\n')
            .map_or(content.len(), |ix| offset + ix + '\n'.len_utf8());
        return start..end;
    }
    if click_count == 2 {
        if let Some(mention) = mentions
            .iter()
            .find(|mention| mention.range.start <= offset && offset <= mention.range.end)
        {
            return mention.range.clone();
        }
        return word_range_at_offset(content, offset);
    }
    offset..offset
}

fn mention_aware_source_offset(
    content: &str,
    mentions: &[ComposerMention],
    source_offset: usize,
) -> usize {
    let mut offset = source_offset.min(content.len());
    while offset > 0 && !content.is_char_boundary(offset) {
        offset -= 1;
    }
    if let Some(mention) = mentions
        .iter()
        .find(|mention| mention.range.start < offset && offset < mention.range.end)
    {
        if offset - mention.range.start < mention.range.end - offset {
            mention.range.start
        } else {
            mention.range.end
        }
    } else {
        offset
    }
}

fn word_range_at_offset(content: &str, offset: usize) -> Range<usize> {
    if content.is_empty() {
        return 0..0;
    }
    let mut target = offset.min(content.len());
    if target == content.len() {
        target = content.char_indices().next_back().map_or(0, |(ix, _)| ix);
    }
    let Some(target_char) = content[target..].chars().next() else {
        return offset..offset;
    };
    let target_class = word_character_class(target_char);
    let mut start = target;
    while let Some((previous, ch)) = content[..start].char_indices().next_back() {
        if word_character_class(ch) != target_class {
            break;
        }
        start = previous;
    }
    let mut end = target + target_char.len_utf8();
    while let Some(ch) = content[end..].chars().next() {
        if word_character_class(ch) != target_class {
            break;
        }
        end += ch.len_utf8();
    }
    start..end
}

fn word_character_class(ch: char) -> u8 {
    if ch.is_alphanumeric() || ch == '_' {
        0
    } else if ch.is_whitespace() {
        1
    } else {
        2
    }
}

pub(super) fn message_scroll_distance_from_bottom(offset: f32, max_offset: f32) -> f32 {
    (max_offset.max(0.0) + offset.min(0.0)).max(0.0)
}

pub(super) fn cursor_position_for_lines(
    lines: &[LaidOutLine],
    cursor: usize,
    line_height: Pixels,
) -> Option<GpuiPoint<Pixels>> {
    let mut line_top = px(0.0);
    for laid_out in lines {
        let line_end = laid_out.start + laid_out.line.len();
        if cursor <= line_end {
            let local_cursor = cursor.saturating_sub(laid_out.start);
            return laid_out
                .line
                .position_for_index(local_cursor, line_height)
                .map(|position| point(position.x, line_top + position.y));
        }
        line_top += laid_out.line.size(line_height).height;
    }

    lines.last().and_then(|line| {
        line.line
            .position_for_index(line.line.len(), line_height)
            .map(|position| point(position.x, line_top + position.y))
    })
}

pub(super) fn cursor_quad(
    lines: &[LaidOutLine],
    cursor: usize,
    bounds: Bounds<Pixels>,
    line_height: Pixels,
    color: gpui::Hsla,
) -> Option<PaintQuad> {
    let mut line_top = px(0.0);
    for laid_out in lines {
        let line_end = laid_out.start + laid_out.line.len();
        if cursor <= line_end {
            let local_cursor = cursor.saturating_sub(laid_out.start);
            let position = laid_out
                .line
                .position_for_index(local_cursor, line_height)?;
            return Some(fill(
                Bounds::new(
                    point(
                        bounds.left() + position.x,
                        bounds.top() + line_top + position.y,
                    ),
                    size(px(2.0), line_height),
                ),
                color,
            ));
        }
        line_top += laid_out.line.size(line_height).height;
    }

    None
}

pub(super) fn selection_quads(
    lines: &[LaidOutLine],
    selected_range: &Range<usize>,
    bounds: Bounds<Pixels>,
    line_height: Pixels,
    color: gpui::Hsla,
) -> Vec<PaintQuad> {
    let mut quads = Vec::new();
    let mut line_top = px(0.0);
    for laid_out in lines {
        let line_start = laid_out.start;
        let line_end = laid_out.start + laid_out.line.len();
        let start = selected_range.start.max(line_start);
        let end = selected_range.end.min(line_end);
        if start < end || (start == end && selected_range.contains(&start)) {
            let local_start = start.saturating_sub(line_start);
            let local_end = end.saturating_sub(line_start);
            let Some(start_pos) = laid_out.line.position_for_index(local_start, line_height) else {
                line_top += laid_out.line.size(line_height).height;
                continue;
            };
            let Some(end_pos) = laid_out.line.position_for_index(local_end, line_height) else {
                line_top += laid_out.line.size(line_height).height;
                continue;
            };

            let start_row = (start_pos.y / line_height) as usize;
            let end_row = (end_pos.y / line_height) as usize;
            for row in start_row..=end_row {
                let row_y = bounds.top() + line_top + line_height * row as f32;
                let left = if row == start_row {
                    bounds.left() + start_pos.x
                } else {
                    bounds.left()
                };
                let right = if row == end_row {
                    bounds.left() + end_pos.x
                } else {
                    bounds.left() + laid_out.line.width()
                };
                if right > left {
                    quads.push(fill(
                        Bounds::new(point(left, row_y), size(right - left, line_height)),
                        color,
                    ));
                }
            }
        }
        line_top += laid_out.line.size(line_height).height;
    }
    quads
}

pub(super) fn decorated_range_quads(
    lines: &[LaidOutLine],
    range: &Range<usize>,
    bounds: Bounds<Pixels>,
    line_height: Pixels,
    style: DecorationStyle,
) -> Vec<PaintQuad> {
    let fragments = selection_quads(lines, range, bounds, line_height, style.background);
    fragments
        .into_iter()
        .map(|fragment| {
            let fragment_bounds = fragment.bounds;
            quad(
                fragment_bounds,
                Corners::all(style.radius).clamp_radii_for_quad_size(fragment_bounds.size),
                style.background,
                Edges {
                    top: style.border_width,
                    right: style.border_width,
                    bottom: style.border_width,
                    left: style.border_width,
                },
                style.border,
                BorderStyle::Solid,
            )
        })
        .collect()
}

pub(super) fn active_mention_range(
    text: &str,
    cursor: usize,
    mentions: &[ComposerMention],
) -> Option<Range<usize>> {
    let cursor = cursor.min(text.len());
    let line_start = text[..cursor].rfind('\n').map_or(0, |ix| ix + 1);
    let line = &text[line_start..cursor];
    let at = line.rfind('@')?;
    let start = line_start + at;
    let valid_boundary = start == 0
        || mentions.iter().any(|mention| mention.range.end == start)
        || text[..start]
            .chars()
            .next_back()
            .is_some_and(|ch| ch.is_whitespace() || matches!(ch, '(' | '[' | '{' | '"' | '\''));
    if !valid_boundary || text[start..cursor].contains('\n') {
        return None;
    }
    if text[start..cursor].chars().any(char::is_whitespace) {
        return None;
    }
    Some(start..cursor)
}

pub(super) fn mention_matches(query: &str) -> Vec<MentionMatch> {
    let mut matches = MentionKind::ALL
        .into_iter()
        .enumerate()
        .filter_map(|(canonical_ix, kind)| {
            let label = kind.label();
            fuzzy_match(label, query).map(|(highlights, score)| MentionMatch {
                kind,
                highlights,
                score,
                canonical_ix,
            })
        })
        .collect::<Vec<_>>();
    matches.sort_by_key(|candidate| (candidate.score, candidate.canonical_ix));
    matches
}

pub(super) fn mention_is_available(
    mention: MentionKind,
    page_count: usize,
    has_selection: bool,
    has_region: bool,
) -> bool {
    match mention {
        MentionKind::Selection => has_selection,
        MentionKind::RegionImage => has_region,
        MentionKind::Page | MentionKind::PageImage | MentionKind::DocumentImage => page_count > 0,
        MentionKind::DocumentText => true,
    }
}

#[cfg(test)]
pub(super) fn parse_composer_mentions(text: &str) -> Vec<ComposerMention> {
    find_mention_references(text)
        .into_iter()
        .map(|(range, reference)| ComposerMention {
            reference,
            range,
            captured: None,
        })
        .collect()
}

fn composer_mention_from_occurrence(
    range: Range<usize>,
    reference: MentionReference,
    region_reference: Option<RegionImageReference>,
    region_references: &mut Vec<RegionImageReference>,
    contexts: &mut Vec<ContextBlock>,
) -> Option<ComposerMention> {
    if reference.kind == MentionKind::RegionImage {
        let region_reference = region_reference
            .or_else(|| (!region_references.is_empty()).then(|| region_references.remove(0)))?;
        return Some(ComposerMention {
            reference,
            range,
            captured: Some(CapturedMention::Region(region_reference)),
        });
    }

    let captured = match reference.kind {
        MentionKind::Selection => contexts
            .iter()
            .position(|context| context.kind == reference.kind && context.page == reference.page)
            .map(|context_index| CapturedMention::Text(contexts.remove(context_index).text)),
        MentionKind::DocumentText => contexts
            .iter()
            .find(|context| context.kind == reference.kind && context.page == reference.page)
            .map(|context| CapturedMention::Text(context.text.clone())),
        MentionKind::Page
        | MentionKind::PageImage
        | MentionKind::DocumentImage
        | MentionKind::RegionImage => None,
    };
    Some(ComposerMention {
        reference,
        range,
        captured,
    })
}

pub(super) fn display_mentions_for_message(message: &ChatMessage) -> Vec<ComposerMention> {
    let content = message.display_text();
    let mut mentions = message
        .mention_occurrences()
        .iter()
        .filter(|occurrence| occurrence_matches_content(&content, occurrence))
        .map(|occurrence| ComposerMention {
            reference: occurrence.reference,
            range: occurrence.range.clone(),
            captured: occurrence
                .region_reference
                .clone()
                .map(CapturedMention::Region),
        })
        .collect::<Vec<_>>();
    mentions.sort_by_key(|mention| mention.range.start);
    mentions
}

pub(super) fn historical_preview_mention(
    message: &ChatMessage,
    hovered: (MentionHitbox, ComposerMention),
) -> (MentionHitbox, ComposerMention) {
    let captured = composer_mentions_for_message(message)
        .ok()
        .and_then(|mentions| {
            mentions.into_iter().find(|mention| {
                mention.reference == hovered.1.reference && mention.range == hovered.1.range
            })
        });
    (hovered.0, captured.unwrap_or(hovered.1))
}

fn occurrence_matches_content(content: &str, occurrence: &MentionOccurrence) -> bool {
    if occurrence.range.start >= occurrence.range.end
        || occurrence.range.end > content.len()
        || !content.is_char_boundary(occurrence.range.start)
        || !content.is_char_boundary(occurrence.range.end)
    {
        return false;
    }
    let token = &content[occurrence.range.clone()];
    find_mention_references(token).as_slice() == [(0..token.len(), occurrence.reference)].as_slice()
}

pub(super) fn composer_mentions_for_message(
    message: &ChatMessage,
) -> Result<Vec<ComposerMention>, ()> {
    let mut references = message.references().to_vec();
    let mut region_references = message.region_references().to_vec();
    let mut contexts = message.editable_contexts();
    let content = message.display_text();
    let occurrences = message.mention_occurrences();

    if !occurrences.is_empty() {
        let mut mentions = Vec::with_capacity(occurrences.len());
        for occurrence in occurrences {
            if !occurrence_matches_content(&content, occurrence) {
                return Err(());
            }
            let Some(mention) = composer_mention_from_occurrence(
                occurrence.range.clone(),
                occurrence.reference,
                occurrence.region_reference.clone(),
                &mut region_references,
                &mut contexts,
            ) else {
                return Err(());
            };
            mentions.push(mention);
        }
        return Ok(mentions);
    }

    let parsed = find_mention_references(&content);
    let mut unique_references = references.clone();
    unique_references.extend(
        parsed
            .iter()
            .filter(|candidate| candidate.1.kind != MentionKind::RegionImage)
            .map(|candidate| candidate.1),
    );
    unique_references.sort_by_key(|reference| (reference.kind as u8, reference.page));
    unique_references.dedup();
    for reference in unique_references {
        let expected = references
            .iter()
            .filter(|candidate| **candidate == reference)
            .count();
        let actual = parsed
            .iter()
            .filter(|candidate| candidate.1 == reference)
            .count();
        if actual != expected {
            return Err(());
        }
    }
    if parsed
        .iter()
        .filter(|candidate| candidate.1.kind == MentionKind::RegionImage)
        .count()
        != region_references.len()
    {
        return Err(());
    }

    let mut mentions = Vec::new();
    for (range, reference) in parsed {
        if reference.kind == MentionKind::RegionImage {
            let Some(mention) = composer_mention_from_occurrence(
                range,
                reference,
                None,
                &mut region_references,
                &mut contexts,
            ) else {
                return Err(());
            };
            mentions.push(mention);
            continue;
        }
        let Some(reference_index) = references
            .iter()
            .position(|candidate| *candidate == reference)
        else {
            continue;
        };
        references.remove(reference_index);
        let Some(mention) = composer_mention_from_occurrence(
            range,
            reference,
            None,
            &mut region_references,
            &mut contexts,
        ) else {
            return Err(());
        };
        mentions.push(mention);
    }
    Ok(mentions)
}

#[cfg(test)]
pub(super) fn merge_composer_mentions(
    mut parsed: Vec<ComposerMention>,
    existing: &[ComposerMention],
    selected_text: Option<String>,
) -> Vec<ComposerMention> {
    for mention in &mut parsed {
        if let Some(existing) = existing.iter().find(|existing| {
            existing.reference == mention.reference && existing.range == mention.range
        }) {
            mention.captured.clone_from(&existing.captured);
        } else if mention.reference.kind == MentionKind::Selection {
            mention.captured = selected_text.clone().map(CapturedMention::Text);
        }
    }
    parsed
}

pub(super) fn trimmed_composer_content(
    text: &str,
    mentions: &[ComposerMention],
) -> (String, Vec<ComposerMention>) {
    let trimmed_start = text.len() - text.trim_start().len();
    let trimmed_end = trimmed_start + text.trim().len();
    let content = text[trimmed_start..trimmed_end].to_string();
    let mentions = mentions
        .iter()
        .filter(|mention| mention.range.start >= trimmed_start && mention.range.end <= trimmed_end)
        .map(|mention| ComposerMention {
            reference: mention.reference,
            range: mention.range.start - trimmed_start..mention.range.end - trimmed_start,
            captured: mention.captured.clone(),
        })
        .collect();
    (content, mentions)
}

pub(super) fn references_for_composer_mentions(
    mentions: &[ComposerMention],
) -> Vec<MentionReference> {
    let mut seen = HashSet::new();
    mentions
        .iter()
        .filter_map(|mention| {
            if mention.reference.kind == MentionKind::RegionImage {
                return None;
            }
            (mention.reference.kind == MentionKind::Selection || seen.insert(mention.reference))
                .then_some(mention.reference)
        })
        .collect()
}

pub(super) fn region_references_for_composer_mentions(
    mentions: &[ComposerMention],
) -> Vec<RegionImageReference> {
    mentions
        .iter()
        .filter_map(|mention| mention.captured_region().cloned())
        .collect()
}

pub(super) fn context_blocks_for_composer_mentions(
    mentions: &[ComposerMention],
    context: Option<&AiContext>,
) -> Vec<ContextBlock> {
    let mut seen = HashSet::new();
    mentions
        .iter()
        .filter_map(|mention| {
            if mention.reference.kind == MentionKind::Selection {
                return Some(mention.captured_text().map_or_else(
                    || resolve_mention_context(MentionKind::Selection, context),
                    |text| ContextBlock::new(MentionKind::Selection, None, text.to_string()),
                ));
            }
            if mention.reference.kind == MentionKind::DocumentText && seen.insert(mention.reference)
            {
                return Some(mention.captured_text().map_or_else(
                    || resolve_mention_context(MentionKind::DocumentText, context),
                    |text| ContextBlock::new(MentionKind::DocumentText, None, text.to_string()),
                ));
            }
            None
        })
        .collect()
}

fn resolve_mention_context(mention: MentionKind, context: Option<&AiContext>) -> ContextBlock {
    let Some(context) = context else {
        return missing_context_block(mention);
    };

    match mention {
        MentionKind::Selection => ContextBlock::new(
            mention,
            None,
            context
                .selected_text
                .clone()
                .unwrap_or_else(|| tr("ai-no-selected-text")),
        ),
        MentionKind::DocumentText => ContextBlock::new(
            mention,
            None,
            context
                .document_text
                .clone()
                .unwrap_or_else(|| tr("ai-no-document-text")),
        ),
        MentionKind::Page
        | MentionKind::PageImage
        | MentionKind::DocumentImage
        | MentionKind::RegionImage => missing_context_block(mention),
    }
}

fn missing_context_block(mention: MentionKind) -> ContextBlock {
    ContextBlock::new(mention, None, mention.empty_context_message())
}

pub(super) fn mention_preview_text(
    mention: &ComposerMention,
    context: Option<&AiContext>,
) -> Option<String> {
    match mention.reference.kind {
        MentionKind::Selection => Some(mention.captured_text().map_or_else(
            || resolve_mention_context(MentionKind::Selection, context).text,
            |text| ContextBlock::new(MentionKind::Selection, None, text.to_string()).text,
        )),
        MentionKind::DocumentText => Some(mention.captured_text().map_or_else(
            || resolve_mention_context(MentionKind::DocumentText, context).text,
            str::to_string,
        )),
        MentionKind::Page
        | MentionKind::PageImage
        | MentionKind::DocumentImage
        | MentionKind::RegionImage => None,
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct LimitedMentionPreviewText {
    pub(super) text: String,
    pub(super) truncated: bool,
}

pub(super) fn limit_mention_preview_text(
    kind: MentionKind,
    text: String,
) -> LimitedMentionPreviewText {
    if kind != MentionKind::DocumentText {
        return LimitedMentionPreviewText {
            text,
            truncated: false,
        };
    }

    let truncated = text.chars().nth(MENTION_PREVIEW_MAX_CHARS).is_some();
    LimitedMentionPreviewText {
        text: if truncated {
            text.chars().take(MENTION_PREVIEW_MAX_CHARS).collect()
        } else {
            text
        },
        truncated,
    }
}

pub(super) fn preview_content_from_resolved(
    reference: MentionReference,
    resolved: &[ResolvedPageContext],
) -> Result<MentionPreviewPayload, String> {
    for context in resolved {
        match context {
            ResolvedPageContext::Text(context)
                if context.kind == reference.kind && context.page == reference.page =>
            {
                return Ok(MentionPreviewPayload::Text(context.text.clone()));
            }
            ResolvedPageContext::Image(image)
                if image.kind == reference.kind && image.page == reference.page =>
            {
                return preview_payload_from_image(image);
            }
            ResolvedPageContext::Text(_) | ResolvedPageContext::Image(_) => {}
        }
    }

    Err(format!(
        "No preview is available for {}.",
        reference.display_text()
    ))
}

pub(super) fn preview_content_from_resolved_region(
    reference: &RegionImageReference,
    resolved: &[ResolvedRegionContext],
) -> Result<MentionPreviewPayload, String> {
    let image = resolved
        .iter()
        .find(|context| &context.reference == reference)
        .map(|context| &context.image)
        .ok_or_else(|| "No preview is available for @region-image.".to_string())?;
    preview_payload_from_image(image)
}

fn preview_payload_from_image(
    image: &glyphst_ai::ai_chat::ImageContext,
) -> Result<MentionPreviewPayload, String> {
    let format = ImageFormat::from_mime_type(&image.media_type)
        .ok_or_else(|| format!("Unsupported preview image type: {}", image.media_type))?;
    let (width, height) = preview_image_dimensions(format, &image.bytes)?;
    Ok(MentionPreviewPayload::Image(MentionPreviewImage {
        image: Arc::new(Image::from_bytes(format, image.bytes.to_vec())),
        width,
        height,
    }))
}

fn preview_image_dimensions(format: ImageFormat, bytes: &[u8]) -> Result<(u32, u32), String> {
    let format = match format {
        ImageFormat::Png => image::ImageFormat::Png,
        ImageFormat::Jpeg => image::ImageFormat::Jpeg,
        ImageFormat::Webp => image::ImageFormat::WebP,
        ImageFormat::Gif => image::ImageFormat::Gif,
        ImageFormat::Svg => return Err("SVG preview dimensions are unavailable".to_string()),
        ImageFormat::Bmp => image::ImageFormat::Bmp,
        ImageFormat::Tiff => image::ImageFormat::Tiff,
        ImageFormat::Ico => image::ImageFormat::Ico,
        ImageFormat::Pnm => image::ImageFormat::Pnm,
    };
    image::ImageReader::with_format(Cursor::new(bytes), format)
        .into_dimensions()
        .map_err(|error| format!("Failed to read preview image dimensions: {error}"))
}

pub(super) fn mention_preview_matches(
    preview: &MentionPreview,
    source: MentionPreviewSource,
    generation: u64,
    reference: MentionReference,
    source_range: &Range<usize>,
) -> bool {
    preview.source == source
        && preview.generation == generation
        && preview.reference == reference
        && preview.source_range == *source_range
}

pub(super) fn mention_preview_region_contains(
    position: GpuiPoint<Pixels>,
    trigger_bounds: Option<Bounds<Pixels>>,
    preview_bounds: Option<Bounds<Pixels>>,
) -> bool {
    preview_bounds.is_some_and(|bounds| bounds.contains(&position))
        || trigger_bounds.is_some_and(|bounds| bounds.contains(&position))
}

pub(super) fn mention_insertion_text(
    source: &str,
    range: &Range<usize>,
    token: &str,
) -> (String, Range<usize>) {
    let needs_leading_space = range.start > 0
        && source[..range.start]
            .chars()
            .last()
            .is_some_and(|ch| !ch.is_whitespace());
    let mut text = String::new();
    if needs_leading_space {
        text.push(' ');
    }
    let mention_start = text.len();
    text.push_str(token);
    let mention_end = text.len();
    (text, mention_start..mention_end)
}

pub(super) fn adjust_composer_mentions(
    mentions: &mut Vec<ComposerMention>,
    edited_range: &Range<usize>,
    inserted_len: usize,
) {
    let delta = inserted_len as isize - edited_range.len() as isize;
    mentions.retain_mut(|mention| {
        if edited_range.end <= mention.range.start {
            mention.range.start = (mention.range.start as isize + delta) as usize;
            mention.range.end = (mention.range.end as isize + delta) as usize;
            true
        } else {
            edited_range.start >= mention.range.end
        }
    });
}

pub(super) fn adjust_mentions_for_page_number_edit(
    mentions: &mut [ComposerMention],
    mention_start: usize,
    new_digits: Range<usize>,
    delta: isize,
    validated_page: usize,
) {
    for mention in mentions {
        if mention.range.start == mention_start {
            mention.range.end = (mention.range.end as isize + delta) as usize;
            if validated_page > 0 {
                mention.reference.page = Some(validated_page);
            }
        } else if mention.range.start > mention_start {
            mention.range.start = (mention.range.start as isize + delta) as usize;
            mention.range.end = (mention.range.end as isize + delta) as usize;
        }
    }
    debug_assert!(new_digits.start <= new_digits.end);
}

pub(super) fn validate_page_number(digits: &str, page_count: usize) -> usize {
    digits
        .parse::<usize>()
        .unwrap_or(1)
        .clamp(1, page_count.max(1))
}

pub(super) fn request_prompt_without_mentions(text: &str, mentions: &[ComposerMention]) -> String {
    let mut result = String::with_capacity(text.len());
    let mut cursor = 0;
    for mention in mentions {
        if mention.range.start < cursor || mention.range.end > text.len() {
            continue;
        }
        result.push_str(&text[cursor..mention.range.start]);
        cursor = mention.range.end;
        let result_ends_with_space = result
            .chars()
            .next_back()
            .is_some_and(|ch| ch == ' ' || ch == '\t');
        if result_ends_with_space
            && text[cursor..]
                .chars()
                .next()
                .is_some_and(|ch| ch == ' ' || ch == '\t')
        {
            while text[cursor..]
                .chars()
                .next()
                .is_some_and(|ch| ch == ' ' || ch == '\t')
            {
                cursor += text[cursor..].chars().next().unwrap().len_utf8();
            }
        } else if result_ends_with_space
            && text[cursor..].chars().next().is_none_or(|ch| {
                ch == '\n' || matches!(ch, '.' | ',' | ';' | ':' | '!' | '?' | ')' | ']' | '}')
            })
        {
            while result
                .chars()
                .next_back()
                .is_some_and(|ch| ch == ' ' || ch == '\t')
            {
                result.pop();
            }
        }
    }
    result.push_str(&text[cursor..]);
    result.trim().to_string()
}

pub(super) fn validate_page_image_preflight<'a>(
    messages: impl IntoIterator<Item = &'a ChatMessage>,
    page_count: usize,
    page_image_limit: usize,
) -> anyhow::Result<()> {
    let messages = messages.into_iter().collect::<Vec<_>>();
    let mut image_count = 0usize;
    for reference in messages.iter().copied().flat_map(ChatMessage::references) {
        match reference.kind {
            MentionKind::PageImage => image_count = image_count.saturating_add(1),
            MentionKind::DocumentImage => {
                if page_count == 0 {
                    return Err(anyhow!("document images are unavailable for this document"));
                }
                image_count = image_count.saturating_add(page_count);
            }
            MentionKind::Page
            | MentionKind::Selection
            | MentionKind::DocumentText
            | MentionKind::RegionImage => {}
        }
    }
    image_count = image_count.saturating_add(
        messages
            .iter()
            .flat_map(|message| message.region_references())
            .map(|reference| reference.regions.len())
            .sum::<usize>(),
    );
    if image_count > page_image_limit {
        return Err(anyhow!(
            "too many page images attached: {image_count} requested, maximum is {page_image_limit}"
        ));
    }
    Ok(())
}

pub(super) fn validate_composer_page_image_preflight(
    mode: AiPanelMode,
    history: &[ChatMessage],
    pending: &ChatMessage,
    page_count: usize,
    page_image_limit: usize,
) -> anyhow::Result<()> {
    let history = if mode == AiPanelMode::QuickAsk {
        &[][..]
    } else {
        history
    };
    validate_page_image_preflight(
        history.iter().chain(std::iter::once(pending)),
        page_count,
        page_image_limit,
    )
}

pub(super) fn message_has_resolved_reference(
    message: &ChatMessage,
    reference: MentionReference,
    page_count: usize,
) -> bool {
    if reference.kind != MentionKind::DocumentImage {
        return message.has_resolved_page_reference(reference);
    }
    page_count > 0
        && (1..=page_count).all(|page| {
            message.has_resolved_page_reference(MentionReference::new(
                MentionKind::DocumentImage,
                Some(page),
            ))
        })
}

fn message_needs_resolved_reference(
    message: &ChatMessage,
    resolved_reference: MentionReference,
) -> bool {
    let requested = if resolved_reference.kind == MentionKind::DocumentImage {
        MentionReference::new(MentionKind::DocumentImage, None)
    } else {
        resolved_reference
    };
    message.references().contains(&requested)
        && !message.has_resolved_page_reference(resolved_reference)
}

pub(super) fn unresolved_page_references(
    messages: &[ChatMessage],
    page_count: usize,
) -> Vec<MentionReference> {
    let mut seen = HashSet::new();
    messages
        .iter()
        .flat_map(|message| {
            message.references().iter().copied().filter(|reference| {
                reference.kind.is_page_reference()
                    && !message_has_resolved_reference(message, *reference, page_count)
            })
        })
        .filter(|reference| seen.insert(*reference))
        .collect()
}

pub(super) fn unresolved_region_references(messages: &[ChatMessage]) -> Vec<RegionImageReference> {
    let mut seen = Vec::new();
    for message in messages {
        for reference in message.region_references() {
            if message_needs_resolved_region_reference(message, reference)
                && !seen.contains(reference)
            {
                seen.push(reference.clone());
            }
        }
    }
    seen
}

fn message_needs_resolved_region_reference(
    message: &ChatMessage,
    reference: &RegionImageReference,
) -> bool {
    message.region_references().contains(reference)
        && !message.has_resolved_region_reference(reference)
}

pub(super) fn validate_resolved_image_contexts(
    messages: &[ChatMessage],
    resolved_pages: &[ResolvedPageContext],
    resolved_regions: &[ResolvedRegionContext],
) -> anyhow::Result<()> {
    let image_bytes = messages
        .iter()
        .map(ChatMessage::image_context_bytes)
        .sum::<usize>()
        + resolved_pages
            .iter()
            .map(|context| match context {
                ResolvedPageContext::Image(image) => {
                    let reference = MentionReference::new(image.kind, image.page);
                    let occurrence_count = messages
                        .iter()
                        .filter(|message| message_needs_resolved_reference(message, reference))
                        .count();
                    image.bytes.len() * occurrence_count
                }
                ResolvedPageContext::Text(_) => 0,
            })
            .sum::<usize>()
        + resolved_regions
            .iter()
            .map(|context| {
                let occurrence_count = messages
                    .iter()
                    .filter(|message| {
                        message_needs_resolved_region_reference(message, &context.reference)
                    })
                    .count();
                context.image.bytes.len() * occurrence_count
            })
            .sum::<usize>();
    if image_bytes > MAX_PAGE_IMAGE_BYTES {
        return Err(anyhow!(
            "attached page images are too large: {} MiB exceeds the {} MiB limit",
            image_bytes.div_ceil(1024 * 1024),
            MAX_PAGE_IMAGE_BYTES / (1024 * 1024)
        ));
    }
    Ok(())
}

pub(super) fn hydrate_page_contexts(
    messages: &mut [ChatMessage],
    resolved: &[ResolvedPageContext],
) {
    for message in messages {
        let references = message.references().iter().copied().collect::<HashSet<_>>();
        for resolved in resolved {
            match resolved {
                ResolvedPageContext::Text(context) => {
                    let reference = MentionReference::new(context.kind, context.page);
                    if references.contains(&reference)
                        && message_needs_resolved_reference(message, reference)
                    {
                        message.attach_context(context.clone());
                    }
                }
                ResolvedPageContext::Image(image) => {
                    let reference = MentionReference::new(image.kind, image.page);
                    if (references.contains(&reference)
                        || (reference.kind == MentionKind::DocumentImage
                            && references.contains(&MentionReference::new(
                                MentionKind::DocumentImage,
                                None,
                            ))))
                        && message_needs_resolved_reference(message, reference)
                    {
                        message.attach_image(image.clone());
                    }
                }
            }
        }
    }
}

pub(super) fn hydrate_region_contexts(
    messages: &mut [ChatMessage],
    resolved: &[ResolvedRegionContext],
) {
    for message in messages {
        for context in resolved {
            if message_needs_resolved_region_reference(message, &context.reference) {
                message.attach_image(context.image.clone());
            }
        }
    }
}

pub(super) fn persisted_session_messages(
    messages: &[ChatMessage],
    excluded_index: Option<usize>,
) -> Vec<AiSessionMessage> {
    messages
        .iter()
        .enumerate()
        .filter(|(index, _)| Some(*index) != excluded_index)
        .map(|(_, message)| {
            let content = message.display_text();
            let request_content = message.request_text();
            AiSessionMessage {
                role: match message.role {
                    Role::User => AiSessionRole::User,
                    Role::Assistant => AiSessionRole::Assistant,
                },
                content: content.clone(),
                request_content: (request_content != content).then_some(request_content),
                references: message
                    .references()
                    .iter()
                    .copied()
                    .map(ai_reference_to_library)
                    .chain(
                        message
                            .region_references()
                            .iter()
                            .cloned()
                            .map(|reference| AiMentionReference::RegionImage { reference }),
                    )
                    .collect(),
                occurrences: message
                    .mention_occurrences()
                    .iter()
                    .filter_map(ai_occurrence_to_library)
                    .collect(),
            }
        })
        .collect()
}

pub(super) fn ai_reference_to_library(reference: MentionReference) -> AiMentionReference {
    match reference.kind {
        MentionKind::Selection => AiMentionReference::Selection,
        MentionKind::DocumentText => AiMentionReference::DocumentText,
        MentionKind::Page => AiMentionReference::Page {
            page: reference.page.unwrap_or(1),
        },
        MentionKind::PageImage => AiMentionReference::PageImage {
            page: reference.page.unwrap_or(1),
        },
        MentionKind::DocumentImage => AiMentionReference::DocumentImage,
        MentionKind::RegionImage => {
            unreachable!("region references are persisted with their geometry")
        }
    }
}

pub(super) fn library_reference_to_ai(reference: &AiMentionReference) -> Option<MentionReference> {
    match reference {
        AiMentionReference::Selection => Some(MentionReference::new(MentionKind::Selection, None)),
        AiMentionReference::DocumentText => {
            Some(MentionReference::new(MentionKind::DocumentText, None))
        }
        AiMentionReference::Page { page } => {
            Some(MentionReference::new(MentionKind::Page, Some(*page)))
        }
        AiMentionReference::PageImage { page } => {
            Some(MentionReference::new(MentionKind::PageImage, Some(*page)))
        }
        AiMentionReference::DocumentImage => {
            Some(MentionReference::new(MentionKind::DocumentImage, None))
        }
        AiMentionReference::RegionImage { .. } => None,
    }
}

pub(super) fn library_reference_to_region(
    reference: &AiMentionReference,
) -> Option<RegionImageReference> {
    match reference {
        AiMentionReference::RegionImage { reference } => Some(reference.clone()),
        AiMentionReference::Selection
        | AiMentionReference::DocumentText
        | AiMentionReference::Page { .. }
        | AiMentionReference::PageImage { .. }
        | AiMentionReference::DocumentImage => None,
    }
}

fn ai_occurrence_to_library(occurrence: &MentionOccurrence) -> Option<AiMentionOccurrence> {
    let reference = if occurrence.reference.kind == MentionKind::RegionImage {
        AiMentionReference::RegionImage {
            reference: occurrence.region_reference.clone()?,
        }
    } else {
        ai_reference_to_library(occurrence.reference)
    };
    Some(AiMentionOccurrence {
        start: occurrence.range.start,
        end: occurrence.range.end,
        reference,
    })
}

pub(super) fn library_occurrence_to_ai(
    occurrence: &AiMentionOccurrence,
) -> Option<MentionOccurrence> {
    let (reference, region_reference) = match &occurrence.reference {
        AiMentionReference::RegionImage { reference } => (
            MentionReference::new(MentionKind::RegionImage, None),
            Some(reference.clone()),
        ),
        reference => (library_reference_to_ai(reference)?, None),
    };
    Some(MentionOccurrence {
        range: occurrence.start..occurrence.end,
        reference,
        region_reference,
    })
}

pub(super) fn with_alpha(rgb: u32, alpha: u8) -> u32 {
    (rgb << 8) | u32::from(alpha)
}
