use super::*;

pub(super) fn message_scroll_distance_from_bottom(offset: f32, max_offset: f32) -> f32 {
    (max_offset.max(0.0) + offset.min(0.0)).max(0.0)
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
