use super::protection::*;
use super::*;

pub(super) fn build_translation_units(document: &DocumentV1) -> Vec<TranslationUnit> {
    let mut units = Vec::<TranslationUnit>::new();
    let mut continuation_units = HashMap::<String, usize>::new();
    for page in &document.pages {
        let mut blocks = page.blocks.iter().collect::<Vec<_>>();
        blocks.sort_by_key(|block| (block.reading_order, block.order));
        for block in blocks {
            if !block.policy.translate || block.content.text.trim().is_empty() {
                continue;
            }
            let continuation_id = block
                .structure
                .as_ref()
                .and_then(|structure| structure.group_id.as_ref())
                .is_none()
                .then(|| {
                    block
                        .continuation
                        .as_ref()
                        .map(|continuation| continuation.group_id.clone())
                })
                .flatten();
            if let Some(group_id) = continuation_id.as_ref()
                && let Some(unit_index) = continuation_units.get(group_id).copied()
            {
                let unit = &mut units[unit_index];
                append_continuation_text(
                    &mut unit.source_text,
                    &normalize_pdf_hard_wraps(&block.content.text, block.role),
                );
                unit.member_block_ids.push(block.id.clone());
                unit.protected = protect_text_for_role(&unit.source_text, unit.role);
                unit.style_definitions.clear();
                continue;
            }
            let id = continuation_id
                .clone()
                .unwrap_or_else(|| format!("unit-{}", block.id));
            let (source_text, protected, style_definitions) = protect_block_text(block, block.role);
            let unit_index = units.len();
            units.push(TranslationUnit {
                id,
                member_block_ids: vec![block.id.clone()],
                role: block.role,
                protected,
                style_definitions,
                source_text,
                context_before: None,
                context_after: None,
            });
            if let Some(group_id) = continuation_id {
                continuation_units.insert(group_id, unit_index);
            }
        }
    }
    for index in 0..units.len() {
        units[index].context_before = index
            .checked_sub(1)
            .and_then(|previous| units.get(previous))
            .map(|unit| context_excerpt(&unit.source_text));
        units[index].context_after = units
            .get(index + 1)
            .map(|unit| context_excerpt(&unit.source_text));
    }
    units
}

pub(super) fn plan_translation_requests(units: &[TranslationUnit]) -> Vec<PlannedRequest> {
    let mut requests = Vec::new();
    let mut batch = Vec::new();
    let mut batch_characters = 0usize;
    let mut batch_placeholders = 0usize;

    let flush_batch = |requests: &mut Vec<PlannedRequest>,
                       batch: &mut Vec<TranslationUnit>,
                       characters: &mut usize,
                       placeholders: &mut usize| {
        if batch.is_empty() {
            return;
        }
        requests.push(PlannedRequest {
            units: std::mem::take(batch),
            complex: false,
        });
        *characters = 0;
        *placeholders = 0;
    };

    for unit in units {
        let characters = unit.protected.text.chars().count();
        let placeholders = unit.protected.tokens.len();
        let batchable = matches!(
            unit.role,
            BlockRole::Body | BlockRole::Abstract | BlockRole::Prompt | BlockRole::Dialogue
        ) && characters <= MAX_BATCHABLE_UNIT_CHARACTERS
            && placeholders <= MAX_BATCHABLE_UNIT_PLACEHOLDERS;
        if !batchable {
            flush_batch(
                &mut requests,
                &mut batch,
                &mut batch_characters,
                &mut batch_placeholders,
            );
            requests.push(PlannedRequest {
                units: vec![unit.clone()],
                complex: true,
            });
            continue;
        }
        if !batch.is_empty()
            && (batch.len() >= MAX_BATCH_UNITS
                || batch_characters + characters > MAX_BATCH_CHARACTERS
                || batch_placeholders + placeholders > MAX_BATCH_PLACEHOLDERS)
        {
            flush_batch(
                &mut requests,
                &mut batch,
                &mut batch_characters,
                &mut batch_placeholders,
            );
        }
        batch_characters += characters;
        batch_placeholders += placeholders;
        batch.push(unit.clone());
    }
    flush_batch(
        &mut requests,
        &mut batch,
        &mut batch_characters,
        &mut batch_placeholders,
    );
    requests
}

pub(super) fn semantic_unit_hash(unit: &TranslationUnit) -> u64 {
    let mut hasher = DefaultHasher::new();
    unit.id.hash(&mut hasher);
    unit.member_block_ids.hash(&mut hasher);
    role_name(unit.role).hash(&mut hasher);
    unit.source_text.hash(&mut hasher);
    unit.protected.text.hash(&mut hasher);
    if let Ok(tokens) = serde_json::to_string(&unit.protected.tokens) {
        tokens.hash(&mut hasher);
    }
    if let Ok(styles) = serde_json::to_string(&unit.style_definitions) {
        styles.hash(&mut hasher);
    }
    unit.context_before.hash(&mut hasher);
    unit.context_after.hash(&mut hasher);
    hasher.finish()
}

pub(super) fn sleep_cancellable(
    duration: Duration,
    cancellation: &CancellationToken,
) -> Result<(), String> {
    let mut remaining = duration;
    while remaining > Duration::ZERO {
        cancellation.check().map_err(|error| error.to_string())?;
        let interval = remaining.min(Duration::from_millis(100));
        thread::sleep(interval);
        remaining = remaining.saturating_sub(interval);
    }
    Ok(())
}

pub(super) struct ValidatedTranslation {
    pub(super) text: String,
    pub(super) runs: Vec<StyledRun>,
    pub(super) style_fallback: bool,
}

pub(super) fn validate_translation(
    unit: &TranslationUnit,
    translated: &str,
    reject_unchanged: bool,
) -> Result<ValidatedTranslation, String> {
    let (restored, marker_fallback) = unit.protected.restore_translation(translated)?;
    let (_plain, mut runs, parse_fallback) =
        parse_styled_translation(&restored, &unit.style_definitions);
    normalize_cjk_styled_spacing(&mut runs);
    let plain = crate::strip_inline_object_markers(
        &runs.iter().map(|run| run.text.as_str()).collect::<String>(),
    );
    if reject_unchanged
        && should_reject_unchanged(&unit.source_text)
        && normalize_for_comparison(&plain)
            == normalize_for_comparison(&crate::strip_inline_object_markers(&unit.source_text))
    {
        return Err("translation is unchanged from the source".to_string());
    }
    let lowercase = plain.trim().to_ascii_lowercase();
    if lowercase.starts_with("i cannot translate")
        || lowercase.starts_with("i can't translate")
        || lowercase.starts_with("unable to translate")
    {
        return Err("translation provider refused the unit".to_string());
    }
    Ok(ValidatedTranslation {
        text: plain.trim().to_string(),
        runs,
        style_fallback: marker_fallback || parse_fallback,
    })
}

pub(super) fn validate_resumed_translation(
    unit: &TranslationUnit,
    translated: &str,
    reject_unchanged: bool,
) -> bool {
    if translated.trim().is_empty() || translated.contains("GLYPHST_") {
        return false;
    }
    if unit
        .protected
        .tokens
        .iter()
        .filter(|token| {
            !matches!(
                token.kind,
                ProtectedTokenKind::StyleStart
                    | ProtectedTokenKind::StyleEnd
                    | ProtectedTokenKind::InlineObject
            )
        })
        .any(|token| !translated.contains(&token.original))
    {
        return false;
    }
    let source_without_styles = strip_style_placeholders(&unit.protected.text);
    if unit.protected.tokens.iter().any(|token| {
        token.kind == ProtectedTokenKind::StructuralMarker
            && source_without_styles
                .trim_start()
                .starts_with(&token.placeholder)
            && !translated.trim_start().starts_with(&token.original)
    }) {
        return false;
    }
    if reject_unchanged
        && should_reject_unchanged(&unit.source_text)
        && normalize_for_comparison(translated)
            == normalize_for_comparison(&crate::strip_inline_object_markers(&unit.source_text))
    {
        return false;
    }
    let lowercase = translated.trim().to_ascii_lowercase();
    !(lowercase.starts_with("i cannot translate")
        || lowercase.starts_with("i can't translate")
        || lowercase.starts_with("unable to translate"))
}

pub(super) fn validate_resumed_inline_objects(
    unit: &TranslationUnit,
    resumed: &TranslatedUnit,
) -> bool {
    let inline_objects = unit
        .protected
        .tokens
        .iter()
        .filter(|token| token.kind == ProtectedTokenKind::InlineObject)
        .collect::<Vec<_>>();
    if inline_objects.is_empty() {
        return true;
    }

    let styled_text = resumed
        .styled_runs
        .iter()
        .map(|run| run.text.as_str())
        .collect::<String>();
    inline_objects
        .iter()
        .all(|token| styled_text.matches(&token.original).count() == 1)
}

pub(super) fn protected_prefix(
    text: &str,
    protect_identifiers: bool,
) -> Option<(ProtectedTokenKind, usize)> {
    if let Some((length, _)) = crate::document::inline_object_marker_prefix(text) {
        return Some((ProtectedTokenKind::InlineObject, length));
    }
    if let Some(length) =
        delimited_length(text, "$$", "$$").or_else(|| delimited_length(text, "$", "$"))
    {
        return Some((ProtectedTokenKind::Formula, length));
    }
    if let Some(length) =
        delimited_length(text, "\\(", "\\)").or_else(|| delimited_length(text, "\\[", "\\]"))
    {
        return Some((ProtectedTokenKind::Formula, length));
    }
    if text.starts_with("http://") || text.starts_with("https://") {
        let length = text.find(char::is_whitespace).unwrap_or(text.len());
        return Some((
            ProtectedTokenKind::Url,
            trim_trailing_punctuation(&text[..length]),
        ));
    }
    if text.starts_with('[')
        && let Some(end) = text.find(']')
        && end > 1
        && text[1..end].chars().all(|character| {
            character.is_ascii_digit() || matches!(character, ',' | '-' | '–' | ' ')
        })
    {
        return Some((ProtectedTokenKind::Citation, end + 1));
    }
    if text.starts_with('(')
        && let Some(end) = text.find(')')
    {
        let candidate = &text[1..end];
        if candidate
            .chars()
            .any(|character| character.is_ascii_digit())
            && candidate.chars().all(|character| {
                character.is_ascii_alphanumeric()
                    || character.is_whitespace()
                    || matches!(character, ',' | ';' | '-' | '–' | '&' | '.')
            })
        {
            return Some((ProtectedTokenKind::Citation, end + 1));
        }
    }
    if text
        .chars()
        .next()
        .is_some_and(|character| character.is_ascii_digit())
    {
        let length = numeric_literal_length(text);
        if length > 0 {
            return Some((ProtectedTokenKind::NumericLiteral, length));
        }
    }
    if protect_identifiers
        && text
            .chars()
            .next()
            .is_some_and(|character| character.is_ascii_uppercase())
    {
        let length = text
            .char_indices()
            .take_while(|(_, character)| {
                character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.')
            })
            .map(|(index, character)| index + character.len_utf8())
            .last()
            .unwrap_or(0);
        let candidate = text[..length].trim_end_matches(['.', ',', ';', ':']);
        if candidate.len() >= 2
            && (candidate.contains('_')
                || candidate
                    .chars()
                    .filter(|character| character.is_ascii_uppercase())
                    .count()
                    >= 2)
        {
            return Some((
                ProtectedTokenKind::Identifier,
                identifier_prefix_length(candidate),
            ));
        }
    }
    None
}

fn numeric_literal_length(text: &str) -> usize {
    let mut length = 0usize;
    let mut digit_count = 0usize;
    let mut only_digits = true;
    for (index, character) in text.char_indices() {
        if character.is_ascii_digit() {
            length = index + character.len_utf8();
            digit_count += 1;
            continue;
        }
        if matches!(
            character,
            '.' | ',' | '%' | '°' | '/' | ':' | '-' | '+' | '×'
        ) {
            let next = text[index + character.len_utf8()..].chars().next();
            if matches!(character, '-' | '+') && next.is_some_and(|next| next.is_ascii_alphabetic())
            {
                break;
            }
            length = index + character.len_utf8();
            only_digits = false;
            continue;
        }
        break;
    }

    let suffix = &text[length..];
    let suffix_length = suffix
        .char_indices()
        .take_while(|(_, character)| character.is_ascii_alphabetic())
        .map(|(index, character)| index + character.len_utf8())
        .last()
        .unwrap_or(0);
    if only_digits
        && digit_count >= 2
        && suffix_length > 0
        && suffix
            .chars()
            .next()
            .is_some_and(|character| character.is_ascii_lowercase())
    {
        length += suffix_length;
    }
    while length > 0
        && text[..length]
            .chars()
            .last()
            .is_some_and(|character| matches!(character, '.' | ',' | ';' | ':'))
    {
        length -= text[..length]
            .chars()
            .last()
            .map(char::len_utf8)
            .unwrap_or_default();
    }
    length
}

fn identifier_prefix_length(candidate: &str) -> usize {
    let mut prefix_end = 0usize;
    let mut uppercase = 0usize;
    for (index, character) in candidate.char_indices() {
        if character.is_ascii_uppercase()
            || character.is_ascii_digit()
            || matches!(character, '_' | '-')
        {
            prefix_end = index + character.len_utf8();
            uppercase += usize::from(character.is_ascii_uppercase());
        } else {
            break;
        }
    }
    let suffix = &candidate[prefix_end..];
    if uppercase >= 2
        && suffix.len() >= 4
        && suffix
            .chars()
            .all(|character| character.is_ascii_alphabetic())
    {
        prefix_end
    } else {
        candidate.len()
    }
}

fn delimited_length(text: &str, start: &str, end: &str) -> Option<usize> {
    if !text.starts_with(start) {
        return None;
    }
    let search_start = start.len();
    text[search_start..]
        .find(end)
        .map(|relative| search_start + relative + end.len())
        .filter(|length| *length > start.len() + end.len())
}

fn trim_trailing_punctuation(value: &str) -> usize {
    value.trim_end_matches(['.', ',', ';', ':', ')', ']']).len()
}

pub(super) fn placeholder(kind: ProtectedTokenKind, index: usize, original: &str) -> String {
    let kind = match kind {
        ProtectedTokenKind::Formula => "FORMULA",
        ProtectedTokenKind::Citation => "CITATION",
        ProtectedTokenKind::Url => "URL",
        ProtectedTokenKind::NumericLiteral => "NUMBER",
        ProtectedTokenKind::Identifier => "IDENTIFIER",
        ProtectedTokenKind::InlineObject => "INLINE_OBJECT",
        ProtectedTokenKind::StructuralMarker => "STRUCTURAL_MARKER",
        ProtectedTokenKind::LineBreak => "LINE_BREAK",
        ProtectedTokenKind::StyleStart => "STYLE_START",
        ProtectedTokenKind::StyleEnd => "STYLE_END",
    };
    format!(
        "[[GLYPHST_{kind}_{index:04}_{:08X}]]",
        checksum(original.as_bytes())
    )
}

fn checksum(bytes: &[u8]) -> u32 {
    let mut hash = 0x811C_9DC5u32;
    for byte in bytes {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

fn append_continuation_text(target: &mut String, continuation: &str) {
    let continuation = continuation.trim();
    if continuation.is_empty() {
        return;
    }
    let should_join_without_space = target.ends_with('-')
        && continuation
            .chars()
            .next()
            .is_some_and(|character| character.is_ascii_lowercase());
    let starts_with_punctuation = continuation.chars().next().is_some_and(|character| {
        matches!(
            character,
            ',' | '.'
                | ';'
                | ':'
                | '!'
                | '?'
                | ')'
                | ']'
                | '}'
                | '，'
                | '。'
                | '；'
                | '：'
                | '！'
                | '？'
        )
    });
    if should_join_without_space {
        target.pop();
    } else if !starts_with_punctuation && !target.ends_with(char::is_whitespace) {
        target.push(' ');
    }
    target.push_str(continuation);
}

pub(super) fn block_preserves_line_breaks(block: &Block, role: BlockRole) -> bool {
    block
        .structure
        .as_ref()
        .map(|structure| structure.flow == TextFlow::PreserveLines)
        .unwrap_or_else(|| preserves_role_line_breaks(role))
}

pub(super) fn normalize_block_hard_wraps(block: &Block, role: BlockRole) -> String {
    match block.structure.as_ref().map(|structure| structure.flow) {
        Some(TextFlow::PreserveLines) => normalize_verbatim_lines(&block.content.text),
        Some(TextFlow::Reflow) if role == BlockRole::Prompt => {
            normalize_prompt_lines(&block.content.text)
        }
        Some(TextFlow::Reflow) => normalize_visual_hard_wraps(&block.content.text),
        None => normalize_pdf_hard_wraps(&block.content.text, role),
    }
}

fn normalize_pdf_hard_wraps(text: &str, role: BlockRole) -> String {
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    if role == BlockRole::Dialogue {
        return normalize_dialogue_lines(&text);
    }
    if role == BlockRole::Prompt {
        return normalize_prompt_lines(&text);
    }
    if preserves_semantic_line_breaks(role, &text) {
        return normalize_verbatim_lines(&text);
    }
    normalize_visual_hard_wraps(&text)
}

fn normalize_verbatim_lines(text: &str) -> String {
    text.replace("\r\n", "\n")
        .replace('\r', "\n")
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

fn normalize_visual_hard_wraps(text: &str) -> String {
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut paragraphs = Vec::new();
    let mut lines = Vec::new();
    let flush = |paragraphs: &mut Vec<String>, lines: &mut Vec<&str>| {
        if lines.is_empty() {
            return;
        }
        paragraphs.push(join_visual_lines(lines));
        lines.clear();
    };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            flush(&mut paragraphs, &mut lines);
        } else {
            lines.push(line);
        }
    }
    flush(&mut paragraphs, &mut lines);
    paragraphs
        .into_iter()
        .filter(|paragraph| !paragraph.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
        .trim()
        .to_string()
}

fn join_visual_lines(lines: &[&str]) -> String {
    let mut joined = String::new();
    for line in lines {
        if joined.is_empty() {
            joined.push_str(line);
            continue;
        }
        let dehyphenate = joined.ends_with('-')
            && line
                .chars()
                .next()
                .is_some_and(|character| character.is_lowercase());
        if dehyphenate {
            joined.pop();
        } else if !joined.ends_with(char::is_whitespace) {
            joined.push(' ');
        }
        joined.push_str(line);
    }
    joined
}

fn normalize_dialogue_lines(text: &str) -> String {
    let mut turns = Vec::<String>::new();
    for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
        if turns.is_empty() || looks_like_speaker_turn(line) {
            turns.push(line.to_string());
        } else {
            append_continuation_text(
                turns.last_mut().expect("dialogue has a previous turn"),
                line,
            );
        }
    }
    turns.join("\n")
}

fn normalize_prompt_lines(text: &str) -> String {
    let mut parts = Vec::<String>::new();
    let mut previous = String::new();
    for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
        if parts.is_empty()
            || looks_like_prompt_boundary(line)
            || looks_like_bracketed_prompt_heading(&previous)
            || prompt_sentence_boundary(&previous, line)
        {
            parts.push(line.to_string());
        } else {
            append_continuation_text(parts.last_mut().expect("prompt has a previous part"), line);
        }
        previous = line.to_string();
    }
    parts.join("\n")
}

pub(super) fn looks_like_speaker_turn(line: &str) -> bool {
    speaker_label(line).is_some()
}

fn speaker_label(line: &str) -> Option<String> {
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

pub(super) fn looks_like_prompt_boundary(line: &str) -> bool {
    let line = line.trim();
    if line.is_empty() {
        return false;
    }
    if looks_like_bracketed_prompt_heading(line) {
        return true;
    }
    if looks_like_numbered_prompt_item(line) {
        return true;
    }
    line.split_once([':', '：']).is_some_and(|(label, _)| {
        let label = label.trim();
        !label.is_empty()
            && label.chars().count() <= 48
            && label.split_whitespace().count() <= 7
            && !label.contains(['.', '?', '!'])
            && label_starts_like_prompt_field(label)
    })
}

pub(super) fn looks_like_numbered_prompt_item(line: &str) -> bool {
    line.trim()
        .split_once([')', '）', '.', '．'])
        .is_some_and(|(prefix, _)| {
            !prefix.is_empty()
                && prefix.len() <= 3
                && prefix.chars().all(|character| character.is_ascii_digit())
        })
}

fn label_starts_like_prompt_field(label: &str) -> bool {
    let Some(character) = label.chars().find(|character| character.is_alphabetic()) else {
        return false;
    };
    if character.is_uppercase() {
        return true;
    }
    !character.is_lowercase() && is_cjk_character(character)
}

pub(super) fn looks_like_bracketed_prompt_heading(line: &str) -> bool {
    let line = line.trim();
    line.starts_with('[') && line.ends_with(']') && line.find(']').is_some_and(|end| end <= 80)
}

pub(super) fn prompt_sentence_boundary(previous: &str, line: &str) -> bool {
    let previous = previous.trim_end();
    let line = line.trim_start();
    previous.ends_with(['.', '?', '!', '。', '？', '！'])
        && (line.ends_with('?')
            || line.ends_with('？')
            || line.starts_with("Should ")
            || line.starts_with("Is ")
            || line.starts_with("Are ")
            || line.starts_with("Can ")
            || line.starts_with("Could ")
            || line.starts_with("Would ")
            || line.starts_with("Will ")
            || line.starts_with("What ")
            || line.starts_with("How ")
            || line.starts_with("Which "))
}

pub(super) fn normalize_translated_whitespace(
    translated: &str,
    source: &str,
    role: BlockRole,
) -> String {
    let translated = translated.replace("\r\n", "\n").replace('\r', "\n");
    if preserves_semantic_line_breaks(role, source) {
        return translated
            .lines()
            .map(str::trim_end)
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            .to_string();
    }
    let source_paragraphs = source
        .split("\n\n")
        .filter(|paragraph| !paragraph.trim().is_empty())
        .count()
        .max(1);
    let translated_paragraphs = translated
        .split("\n\n")
        .map(|paragraph| paragraph.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|paragraph| !paragraph.is_empty())
        .collect::<Vec<_>>();
    if source_paragraphs == 1 || translated_paragraphs.len() > source_paragraphs + 1 {
        translated_paragraphs.join(" ")
    } else {
        translated_paragraphs.join("\n\n")
    }
}

pub(super) fn preserves_semantic_line_breaks(role: BlockRole, text: &str) -> bool {
    preserves_role_line_breaks(role) || looks_like_dialogue(text)
}

pub(super) fn preserves_role_line_breaks(role: BlockRole) -> bool {
    matches!(
        role,
        BlockRole::Prompt | BlockRole::Dialogue | BlockRole::TableOfContents | BlockRole::Code
    )
}

pub(super) fn looks_like_dialogue(text: &str) -> bool {
    let labels = text.lines().filter_map(speaker_label).collect::<Vec<_>>();
    labels.len() >= 3 && labels.iter().collect::<HashSet<_>>().len() >= 2
}

fn context_excerpt(text: &str) -> String {
    let characters = text.chars().collect::<Vec<_>>();
    if characters.len() <= 280 {
        text.to_string()
    } else {
        characters[..280].iter().collect()
    }
}

fn normalize_for_comparison(text: &str) -> String {
    text.chars()
        .filter(|character| !character.is_whitespace() && !character.is_ascii_punctuation())
        .flat_map(char::to_lowercase)
        .collect()
}

fn should_reject_unchanged(text: &str) -> bool {
    let alphabetic = text
        .chars()
        .filter(|character| character.is_alphabetic())
        .count();
    alphabetic >= 12
}

fn role_name(role: BlockRole) -> &'static str {
    match role {
        BlockRole::Title => "title",
        BlockRole::Heading => "heading",
        BlockRole::Body => "body",
        BlockRole::Abstract => "abstract",
        BlockRole::Prompt => "prompt",
        BlockRole::Dialogue => "dialogue",
        BlockRole::TableOfContents => "table_of_contents",
        BlockRole::FigureCaption => "figure_caption",
        BlockRole::TableCaption => "table_caption",
        BlockRole::Footnote => "footnote",
        BlockRole::Header => "header",
        BlockRole::Footer => "footer",
        BlockRole::PageNumber => "page_number",
        BlockRole::Metadata => "metadata",
        BlockRole::Reference => "reference",
        BlockRole::FormulaNumber => "formula_number",
        BlockRole::Image => "image",
        BlockRole::Table => "table",
        BlockRole::Formula => "formula",
        BlockRole::Code => "code",
        BlockRole::Unknown => "unknown",
    }
}
