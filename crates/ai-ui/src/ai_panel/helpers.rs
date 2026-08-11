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
    if mentions
        .iter()
        .any(|mention| mention.range.start <= start && start < mention.range.end)
    {
        return None;
    }
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
    has_selected_pages: bool,
) -> bool {
    match mention {
        MentionKind::Selection => has_selection,
        MentionKind::RegionImage => has_region,
        MentionKind::SelectedPagesText | MentionKind::SelectedPagesImage => {
            page_count > 0 && has_selected_pages
        }
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
    selected_pages_reference: Option<SelectedPagesReference>,
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
    if matches!(
        reference.kind,
        MentionKind::SelectedPagesText | MentionKind::SelectedPagesImage
    ) {
        return Some(ComposerMention {
            reference,
            range,
            captured: Some(CapturedMention::Pages(selected_pages_reference?)),
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
        | MentionKind::SelectedPagesText
        | MentionKind::SelectedPagesImage
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
                occurrence.selected_pages_reference.clone(),
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
            if matches!(
                mention.reference.kind,
                MentionKind::RegionImage
                    | MentionKind::SelectedPagesText
                    | MentionKind::SelectedPagesImage
            ) {
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
        | MentionKind::SelectedPagesText
        | MentionKind::SelectedPagesImage
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
        | MentionKind::SelectedPagesText
        | MentionKind::SelectedPagesImage
        | MentionKind::DocumentImage
        | MentionKind::RegionImage => None,
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct LimitedMentionPreviewText {
    pub(super) text: String,
    pub(super) truncated: bool,
}

pub(super) fn limit_mention_preview_text(text: String) -> LimitedMentionPreviewText {
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

pub(super) fn preview_content_from_selected_pages(
    kind: MentionKind,
    pages: &SelectedPagesReference,
    resolved: &[ResolvedPageContext],
) -> Result<MentionPreviewPayload, String> {
    let mut sections = Vec::with_capacity(pages.pages.len());
    for page in &pages.pages {
        let page_number = page.0 + 1;
        let text = resolved.iter().find_map(|context| match context {
            ResolvedPageContext::Text(context)
                if context.kind == kind && context.page == Some(page_number) =>
            {
                Some(context.text.as_str())
            }
            ResolvedPageContext::Text(_) | ResolvedPageContext::Image(_) => None,
        });
        let Some(text) = text else {
            return Err(format!(
                "No preview is available for {} page {page_number}.",
                kind.token()
            ));
        };
        sections.push(format!("## Page {page_number}\n\n{text}"));
    }
    Ok(MentionPreviewPayload::Text(sections.join("\n\n")))
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

pub(super) fn agent_image_preview(image: &AgentImage) -> Option<Arc<Image>> {
    let format = match image.media_type.as_str() {
        "image/png" => ImageFormat::Png,
        "image/jpeg" | "image/jpg" => ImageFormat::Jpeg,
        "image/gif" => ImageFormat::Gif,
        "image/webp" => ImageFormat::Webp,
        _ => return None,
    };
    Some(Arc::new(Image::from_bytes(format, image.bytes.to_vec())))
}

pub(super) fn mention_preview_matches(
    preview: &MentionPreview,
    generation: u64,
    reference: MentionReference,
    source_range: &Range<usize>,
) -> bool {
    preview.generation == generation
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

pub(super) fn composer_page_digits_range(mention: &ComposerMention) -> Option<Range<usize>> {
    if !mention.reference.kind.requires_page_number() {
        return None;
    }
    let start = mention
        .range
        .start
        .checked_add(mention.reference.kind.token().len() + 1)?;
    let end = mention.range.end.checked_sub(1)?;
    (start <= end).then_some(start..end)
}

/// Rebase explicit composer mentions after an input edit while preserving a
/// page mention whose editable numeric subrange is being changed.
///
/// Ordinary text that happens to parse as a mention is intentionally not
/// promoted here: only a previously explicit page mention can be restored.
pub(super) fn adjust_composer_mentions_for_input_change(
    mentions: &mut Vec<ComposerMention>,
    edited_range: &Range<usize>,
    inserted_len: usize,
    new_content: &str,
) {
    let delta = inserted_len as isize - edited_range.len() as isize;
    let page_mentions = mentions
        .iter()
        .filter_map(|mention| {
            let digits = composer_page_digits_range(mention)?;
            (digits.start <= edited_range.start && edited_range.end <= digits.end)
                .then(|| (mention.clone(), digits))
        })
        .collect::<Vec<_>>();

    adjust_composer_mentions(mentions, edited_range, inserted_len);

    for (mut mention, old_digits) in page_mentions {
        let Some(new_end) = mention.range.end.checked_add_signed(delta) else {
            continue;
        };
        let new_range = mention.range.start..new_end;
        let new_digits = old_digits.start..new_end.saturating_sub(1);
        let prefix = format!("{}(", mention.reference.kind.token());
        if new_range.end > new_content.len()
            || new_digits.start > new_digits.end
            || new_content.get(new_range.start..new_digits.start) != Some(prefix.as_str())
            || new_content.get(new_digits.end..new_range.end) != Some(")")
        {
            continue;
        }

        let Some(digits) = new_content.get(new_digits.clone()) else {
            continue;
        };
        if !digits.bytes().all(|byte| byte.is_ascii_digit()) {
            continue;
        }
        if let Ok(page) = digits.parse::<usize>() {
            mention.reference = MentionReference::new(mention.reference.kind, Some(page));
        }
        mention.range = new_range;
        mentions.push(mention);
    }

    mentions.sort_by_key(|mention| mention.range.start);
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
            | MentionKind::SelectedPagesText
            | MentionKind::SelectedPagesImage
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
    image_count = image_count.saturating_add(
        messages
            .iter()
            .flat_map(|message| message.mention_occurrences())
            .filter(|occurrence| occurrence.reference.kind == MentionKind::SelectedPagesImage)
            .filter_map(MentionOccurrence::selected_pages_reference)
            .map(|reference| reference.pages.len())
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

pub(super) fn main_model_image_support(cx: &App) -> AiCapabilitySupport {
    let ai = &cx.global::<SettingsStore>().settings.ai;
    let resolved = ai.resolved_main();
    ai.provider_config(resolved.provider)
        .model(&resolved.model)
        .map(|model| model.image_input_support(resolved.provider))
        .unwrap_or_default()
}

pub(super) fn validate_model_image_support<'a>(
    messages: impl IntoIterator<Item = &'a ChatMessage>,
    support: AiCapabilitySupport,
) -> Result<()> {
    if support != AiCapabilitySupport::Unsupported {
        return Ok(());
    }
    let has_images = messages.into_iter().any(|message| {
        message.user_images().next().is_some()
            || message
                .references()
                .iter()
                .any(|reference| reference.kind.is_image())
            || !message.region_references().is_empty()
    });
    if has_images {
        bail!("{}", tr("ai-model-no-image-support"));
    }
    Ok(())
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
    if matches!(
        resolved_reference.kind,
        MentionKind::SelectedPagesText | MentionKind::SelectedPagesImage
    ) {
        let Some(page) = resolved_reference.page.filter(|page| *page > 0) else {
            return false;
        };
        return message.mention_occurrences().iter().any(|occurrence| {
            occurrence.reference.kind == resolved_reference.kind
                && occurrence
                    .selected_pages_reference()
                    .is_some_and(|reference| {
                        reference
                            .pages
                            .iter()
                            .any(|candidate| candidate.0 + 1 == page)
                    })
        }) && !message.has_resolved_page_reference(resolved_reference);
    }
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
    let mut unresolved = Vec::new();
    for message in messages {
        for reference in message.references().iter().copied().filter(|reference| {
            reference.kind.is_page_reference()
                && !message_has_resolved_reference(message, *reference, page_count)
        }) {
            if seen.insert(reference) {
                unresolved.push(reference);
            }
        }
        for occurrence in message.mention_occurrences() {
            if !matches!(
                occurrence.reference.kind,
                MentionKind::SelectedPagesText | MentionKind::SelectedPagesImage
            ) {
                continue;
            }
            let Some(reference) = occurrence.selected_pages_reference() else {
                continue;
            };
            for page in &reference.pages {
                let reference = MentionReference::new(occurrence.reference.kind, Some(page.0 + 1));
                if !message.has_resolved_page_reference(reference) && seen.insert(reference) {
                    unresolved.push(reference);
                }
            }
        }
    }
    unresolved
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
        for resolved in resolved {
            match resolved {
                ResolvedPageContext::Text(context) => {
                    let reference = MentionReference::new(context.kind, context.page);
                    if message_needs_resolved_reference(message, reference) {
                        message.attach_context(context.clone());
                    }
                }
                ResolvedPageContext::Image(image) => {
                    let reference = MentionReference::new(image.kind, image.page);
                    if message_needs_resolved_reference(message, reference) {
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

#[cfg(test)]
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
                images: Vec::new(),
                agent_parts: Vec::new(),
            }
        })
        .collect()
}

pub(super) fn persisted_session_messages_with_assets(
    messages: &[ChatMessage],
    excluded_index: Option<usize>,
    library: &mut LibraryStore,
) -> Result<Vec<AiSessionMessage>> {
    messages
        .iter()
        .enumerate()
        .filter(|(index, _)| Some(*index) != excluded_index)
        .map(|(_, message)| {
            let content = message.display_text();
            let request_content = message.request_text();
            let images = message
                .user_images()
                .map(|image| install_agent_image(image, library))
                .collect::<Result<Vec<_>>>()?;
            let result_ids = message
                .agent_trace()
                .iter()
                .flat_map(|message| &message.content)
                .filter_map(|part| match part {
                    AgentContentPart::ToolResult(result) => {
                        Some((result.call_id.as_str(), result.is_error))
                    }
                    _ => None,
                })
                .collect::<HashMap<_, _>>();
            let mut agent_parts = Vec::new();
            for trace_message in message.agent_trace() {
                for part in &trace_message.content {
                    match part {
                        AgentContentPart::Text(text) => {
                            agent_parts.push(AiSessionAgentPart::AssistantText(text.clone()));
                        }
                        AgentContentPart::ToolCall(call) => {
                            let status = match result_ids.get(call.id.as_str()) {
                                Some(true) => AiSessionToolStatus::Error,
                                Some(false) => AiSessionToolStatus::Completed,
                                None => AiSessionToolStatus::Pending,
                            };
                            agent_parts.push(AiSessionAgentPart::ToolCall {
                                id: call.id.clone(),
                                name: call.name.clone(),
                                arguments: call.arguments.clone(),
                                status,
                            });
                        }
                        AgentContentPart::ToolResult(result) => {
                            let result_content = result
                                .content
                                .iter()
                                .map(|part| match part {
                                    AgentToolResultPart::Text(text) => {
                                        Ok(AiSessionToolResultPart::Text(text.clone()))
                                    }
                                    AgentToolResultPart::Image(image) => {
                                        install_agent_image(image, library)
                                            .map(AiSessionToolResultPart::Image)
                                    }
                                })
                                .collect::<Result<Vec<_>>>()?;
                            agent_parts.push(AiSessionAgentPart::ToolResult {
                                call_id: result.call_id.clone(),
                                tool_name: result.tool_name.clone(),
                                content: result_content,
                                is_error: result.is_error,
                            });
                        }
                        AgentContentPart::OpaqueReasoning { api_style, value } => {
                            agent_parts.push(AiSessionAgentPart::OpaqueReasoning {
                                api_style: api_style.as_str().to_string(),
                                value: serde_json::to_string(value)
                                    .context("failed to encode agent continuation state")?,
                            });
                        }
                        AgentContentPart::Image(_) => {}
                    }
                }
            }
            Ok(AiSessionMessage {
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
                images,
                agent_parts,
            })
        })
        .collect()
}

fn install_agent_image(image: &AgentImage, library: &mut LibraryStore) -> Result<AiSessionImage> {
    library.install_ai_image(
        image.name.as_deref(),
        &image.media_type,
        image.width,
        image.height,
        &image.bytes,
    )
}

pub(super) fn chat_message_from_session(
    message: AiSessionMessage,
    library: &LibraryStore,
) -> ChatMessage {
    let role = match message.role {
        AiSessionRole::User => Role::User,
        AiSessionRole::Assistant => Role::Assistant,
    };
    let references = message
        .references
        .iter()
        .filter_map(library_reference_to_ai)
        .collect();
    let region_references = message
        .references
        .iter()
        .filter_map(library_reference_to_region)
        .collect();
    let occurrences = message
        .occurrences
        .iter()
        .filter_map(library_occurrence_to_ai)
        .collect();
    let mut chat = ChatMessage::from_persisted(role, message.content, message.request_content)
        .with_references(references)
        .with_region_references(region_references)
        .with_mention_occurrences(occurrences);
    for image in &message.images {
        match library.load_ai_image(image) {
            Ok(bytes) => chat.attach_user_image(
                AgentImage::new(image.media_type.clone(), image.width, image.height, bytes)
                    .with_optional_name(image.name.clone()),
            ),
            Err(error) => eprintln!("failed to load AI image {}: {error:#}", image.asset_id),
        }
    }

    let mut trace = Vec::new();
    for part in message.agent_parts {
        match part {
            AiSessionAgentPart::AssistantText(text) => {
                append_assistant_trace_part(&mut trace, AgentContentPart::Text(text))
            }
            AiSessionAgentPart::ToolCall {
                id,
                name,
                arguments,
                ..
            } => append_assistant_trace_part(
                &mut trace,
                AgentContentPart::ToolCall(AgentToolCall {
                    id,
                    name,
                    arguments,
                }),
            ),
            AiSessionAgentPart::ToolResult {
                call_id,
                tool_name,
                content,
                is_error,
            } => {
                let content = content
                    .into_iter()
                    .filter_map(|part| match part {
                        AiSessionToolResultPart::Text(text) => {
                            Some(AgentToolResultPart::Text(text))
                        }
                        AiSessionToolResultPart::Image(image) => {
                            match library.load_ai_image(&image) {
                                Ok(bytes) => Some(AgentToolResultPart::Image(
                                    AgentImage::new(
                                        image.media_type,
                                        image.width,
                                        image.height,
                                        bytes,
                                    )
                                    .with_optional_name(image.name),
                                )),
                                Err(error) => {
                                    eprintln!(
                                        "failed to load AI tool image {}: {error:#}",
                                        image.asset_id
                                    );
                                    None
                                }
                            }
                        }
                    })
                    .collect();
                trace.push(AgentMessage::tool_result(AgentToolResult {
                    call_id,
                    tool_name,
                    content,
                    is_error,
                }));
            }
            AiSessionAgentPart::OpaqueReasoning { api_style, value } => {
                let api_style = match api_style.as_str() {
                    "openai-compatible" => Some(AiApiStyle::OpenAi),
                    "openai-responses" => Some(AiApiStyle::OpenAiResponses),
                    "anthropic-compatible" => Some(AiApiStyle::Anthropic),
                    _ => None,
                };
                if let (Some(api_style), Ok(value)) = (api_style, serde_json::from_str(&value)) {
                    append_assistant_trace_part(
                        &mut trace,
                        AgentContentPart::OpaqueReasoning { api_style, value },
                    );
                }
            }
        }
    }
    chat.set_agent_trace(trace);
    chat
}

fn append_assistant_trace_part(trace: &mut Vec<AgentMessage>, part: AgentContentPart) {
    if let Some(message) = trace.last_mut()
        && message.role == AgentRole::Assistant
    {
        message.content.push(part);
    } else {
        trace.push(AgentMessage {
            role: AgentRole::Assistant,
            content: vec![part],
        });
    }
}

pub(super) fn tool_cards_from_trace(trace: &[AgentMessage]) -> Vec<AgentToolCard> {
    let mut cards = Vec::<AgentToolCard>::new();
    for message in trace {
        for part in &message.content {
            match part {
                AgentContentPart::ToolCall(call) => cards.push(AgentToolCard {
                    id: call.id.clone(),
                    name: call.name.clone(),
                    arguments: call.arguments.clone(),
                    result: Vec::new(),
                    status: AgentToolCardStatus::Requested,
                }),
                AgentContentPart::ToolResult(result) => {
                    if let Some(card) = cards
                        .iter_mut()
                        .rev()
                        .find(|card| card.id == result.call_id)
                    {
                        card.result = result.content.clone();
                        card.status = if result.is_error {
                            AgentToolCardStatus::Error
                        } else {
                            AgentToolCardStatus::Completed
                        };
                    }
                }
                AgentContentPart::Text(_)
                | AgentContentPart::Image(_)
                | AgentContentPart::OpaqueReasoning { .. } => {}
            }
        }
    }
    cards
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
        MentionKind::RegionImage
        | MentionKind::SelectedPagesText
        | MentionKind::SelectedPagesImage => {
            unreachable!("snapshot mentions are persisted with their occurrence payload")
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
        AiMentionReference::SelectedPagesText { .. } => {
            Some(MentionReference::new(MentionKind::SelectedPagesText, None))
        }
        AiMentionReference::SelectedPagesImage { .. } => {
            Some(MentionReference::new(MentionKind::SelectedPagesImage, None))
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
        | AiMentionReference::DocumentImage
        | AiMentionReference::SelectedPagesText { .. }
        | AiMentionReference::SelectedPagesImage { .. } => None,
    }
}

fn ai_occurrence_to_library(occurrence: &MentionOccurrence) -> Option<AiMentionOccurrence> {
    let reference = match occurrence.reference.kind {
        MentionKind::RegionImage => AiMentionReference::RegionImage {
            reference: occurrence.region_reference.clone()?,
        },
        MentionKind::SelectedPagesText | MentionKind::SelectedPagesImage => {
            let pages = occurrence
                .selected_pages_reference
                .as_ref()?
                .pages
                .iter()
                .map(|page| page.0)
                .collect();
            if occurrence.reference.kind == MentionKind::SelectedPagesText {
                AiMentionReference::SelectedPagesText { pages }
            } else {
                AiMentionReference::SelectedPagesImage { pages }
            }
        }
        _ => ai_reference_to_library(occurrence.reference),
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
    let (reference, region_reference, selected_pages_reference) = match &occurrence.reference {
        AiMentionReference::RegionImage { reference } => (
            MentionReference::new(MentionKind::RegionImage, None),
            Some(reference.clone()),
            None,
        ),
        AiMentionReference::SelectedPagesText { pages } => (
            MentionReference::new(MentionKind::SelectedPagesText, None),
            None,
            Some(SelectedPagesReference::new(
                pages.iter().copied().map(glyphst_core::PageIndex),
            )),
        ),
        AiMentionReference::SelectedPagesImage { pages } => (
            MentionReference::new(MentionKind::SelectedPagesImage, None),
            None,
            Some(SelectedPagesReference::new(
                pages.iter().copied().map(glyphst_core::PageIndex),
            )),
        ),
        reference => (library_reference_to_ai(reference)?, None, None),
    };
    if selected_pages_reference
        .as_ref()
        .is_some_and(SelectedPagesReference::is_empty)
    {
        return None;
    }
    Some(MentionOccurrence {
        range: occurrence.start..occurrence.end,
        reference,
        region_reference,
        selected_pages_reference,
    })
}
