use super::*;

pub fn protect_text(text: &str) -> ProtectedText {
    protect_text_with_identifier_policy(text, true)
}

pub(super) fn protect_text_for_role(text: &str, role: BlockRole) -> ProtectedText {
    protect_text_with_policies(
        text,
        !matches!(
            role,
            BlockRole::Title | BlockRole::Heading | BlockRole::TableOfContents
        ),
        preserves_role_line_breaks(role),
    )
}

fn protect_text_for_block(text: &str, block: &Block, role: BlockRole) -> ProtectedText {
    protect_text_with_style_boundaries(
        text,
        !matches!(
            role,
            BlockRole::Title | BlockRole::Heading | BlockRole::TableOfContents
        ),
        block_preserves_line_breaks(block, role),
        &[],
        block
            .structure
            .as_ref()
            .and_then(|structure| structure.marker.as_deref()),
    )
}

#[derive(Debug, Clone, Copy)]
struct StyleBoundary {
    offset: usize,
    style_id: usize,
    start: bool,
}

pub(super) fn protect_block_text(
    block: &Block,
    role: BlockRole,
) -> (String, ProtectedText, Vec<StyleDefinition>) {
    if let Some(entry) = &block.toc_entry {
        let source = entry.title.trim().to_string();
        return (
            source.clone(),
            protect_text_for_role(&source, BlockRole::TableOfContents),
            Vec::new(),
        );
    }
    let source = block.content.text.trim();
    let Some(dominant) = dominant_text_style(block) else {
        let source = normalize_block_hard_wraps(block, role);
        return (
            source.clone(),
            protect_text_for_block(&source, block, role),
            Vec::new(),
        );
    };
    let mut definitions = Vec::<StyleDefinition>::new();
    let mut boundaries = Vec::<StyleBoundary>::new();
    let mut search_from = 0usize;
    for line in &block.content.lines {
        let Some(relative_line_start) = source[search_from..].find(line.text.trim()) else {
            continue;
        };
        let line_start = search_from + relative_line_start;
        let mut segment_offset = line_start;
        for segment in &line.segments {
            let segment_text = segment.text.as_str();
            let Some(relative_segment_start) = source[segment_offset..].find(segment_text) else {
                continue;
            };
            let start = segment_offset + relative_segment_start;
            let end = start + segment_text.len();
            segment_offset = end;
            let styled_start = start + segment_text.len() - segment_text.trim_start().len();
            let styled_end = end - (segment_text.len() - segment_text.trim_end().len());
            if styled_start >= styled_end || !meaningful_style_difference(&dominant, &segment.style)
            {
                continue;
            }
            let style_id = definitions
                .iter()
                .find(|definition| definition.style == segment.style)
                .map(|definition| definition.id)
                .unwrap_or_else(|| {
                    let id = definitions.len();
                    definitions.push(StyleDefinition {
                        id,
                        style: segment.style.clone(),
                    });
                    id
                });
            boundaries.push(StyleBoundary {
                offset: styled_start,
                style_id,
                start: true,
            });
            boundaries.push(StyleBoundary {
                offset: styled_end,
                style_id,
                start: false,
            });
        }
        search_from = line_start + line.text.trim().len();
    }
    boundaries.sort_by(|left, right| {
        left.offset
            .cmp(&right.offset)
            .then(left.start.cmp(&right.start))
    });
    let normalized = normalize_styled_source(
        source,
        &boundaries,
        role,
        &block.content.lines,
        block.structure.as_ref().map(|structure| structure.flow),
    );
    let protected = if normalized.boundaries.is_empty() {
        protect_text_for_block(&normalized.text, block, role)
    } else {
        protect_text_with_style_boundaries(
            &normalized.text,
            !matches!(
                role,
                BlockRole::Title | BlockRole::Heading | BlockRole::TableOfContents
            ),
            block_preserves_line_breaks(block, role),
            &normalized.boundaries,
            block
                .structure
                .as_ref()
                .and_then(|structure| structure.marker.as_deref()),
        )
    };
    (normalized.text, protected, definitions)
}

#[derive(Debug, Clone, Copy)]
pub(super) struct StyledSourceCharacter {
    character: char,
    style_id: Option<usize>,
}

pub(super) struct NormalizedStyledSource {
    pub(super) text: String,
    boundaries: Vec<StyleBoundary>,
}

fn normalize_styled_source(
    source: &str,
    boundaries: &[StyleBoundary],
    role: BlockRole,
    source_lines: &[Line],
    text_flow: Option<TextFlow>,
) -> NormalizedStyledSource {
    let annotated = annotate_source_styles(source, boundaries);
    let mut lines = vec![Vec::<StyledSourceCharacter>::new()];
    for character in annotated {
        match character.character {
            '\r' => {}
            '\n' => lines.push(Vec::new()),
            _ => lines
                .last_mut()
                .expect("at least one styled line exists")
                .push(character),
        }
    }

    let normalized = match text_flow {
        Some(TextFlow::PreserveLines) => normalize_styled_verbatim_lines(lines),
        Some(TextFlow::Reflow) if role == BlockRole::Prompt => {
            normalize_styled_prompt_lines(lines, source_lines)
        }
        Some(TextFlow::Reflow) => normalize_styled_visual_lines(lines),
        None => match role {
            BlockRole::Dialogue => normalize_styled_dialogue_lines(lines),
            BlockRole::Prompt => normalize_styled_prompt_lines(lines, source_lines),
            _ if preserves_semantic_line_breaks(role, source) => {
                normalize_styled_verbatim_lines(lines)
            }
            _ => normalize_styled_visual_lines(lines),
        },
    };
    styled_characters_to_source(normalized)
}

fn annotate_source_styles(
    source: &str,
    boundaries: &[StyleBoundary],
) -> Vec<StyledSourceCharacter> {
    let mut characters = Vec::with_capacity(source.chars().count());
    let mut boundary_index = 0usize;
    let mut active_style = None;
    for (offset, character) in source.char_indices() {
        while let Some(boundary) = boundaries.get(boundary_index)
            && boundary.offset <= offset
        {
            if boundary.start {
                active_style = Some(boundary.style_id);
            } else if active_style == Some(boundary.style_id) {
                active_style = None;
            }
            boundary_index += 1;
        }
        characters.push(StyledSourceCharacter {
            character,
            style_id: active_style,
        });
    }
    characters
}

fn normalize_styled_verbatim_lines(
    mut lines: Vec<Vec<StyledSourceCharacter>>,
) -> Vec<StyledSourceCharacter> {
    for line in &mut lines {
        trim_styled_line_end(line);
    }
    while lines.first().is_some_and(|line| line.is_empty()) {
        lines.remove(0);
    }
    while lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }
    let mut output = Vec::new();
    for (index, line) in lines.into_iter().enumerate() {
        if index > 0 {
            output.push(unstyled_character('\n'));
        }
        output.extend(line);
    }
    trim_styled_output(&mut output);
    output
}

fn normalize_styled_visual_lines(
    mut lines: Vec<Vec<StyledSourceCharacter>>,
) -> Vec<StyledSourceCharacter> {
    for line in &mut lines {
        trim_styled_line(line);
    }
    let mut output = Vec::new();
    let mut paragraph_has_text = false;
    let mut previous_was_empty = true;
    for line in lines {
        if line.is_empty() {
            if paragraph_has_text {
                previous_was_empty = true;
            }
            continue;
        }
        if previous_was_empty && paragraph_has_text {
            output.push(unstyled_character('\n'));
            output.push(unstyled_character('\n'));
        } else if paragraph_has_text {
            let dehyphenate = output
                .last()
                .is_some_and(|character| character.character == '-')
                && line
                    .first()
                    .is_some_and(|character| character.character.is_lowercase());
            if dehyphenate {
                output.pop();
            } else if !output
                .last()
                .is_some_and(|character| character.character.is_whitespace())
            {
                output.push(unstyled_character(' '));
            }
        }
        output.extend(line);
        paragraph_has_text = true;
        previous_was_empty = false;
    }
    trim_styled_output(&mut output);
    output
}

fn normalize_styled_dialogue_lines(
    mut lines: Vec<Vec<StyledSourceCharacter>>,
) -> Vec<StyledSourceCharacter> {
    for line in &mut lines {
        trim_styled_line(line);
    }
    let mut output = Vec::new();
    for line in lines.into_iter().filter(|line| !line.is_empty()) {
        let line_text = styled_line_text(&line);
        let starts_turn = looks_like_speaker_turn(&line_text);
        if output.is_empty() {
            output.extend(line);
        } else if starts_turn {
            output.push(unstyled_character('\n'));
            output.extend(line);
        } else {
            append_styled_visual_line(&mut output, line);
        }
    }
    trim_styled_output(&mut output);
    output
}

pub(super) fn normalize_styled_prompt_lines(
    mut lines: Vec<Vec<StyledSourceCharacter>>,
    source_lines: &[Line],
) -> Vec<StyledSourceCharacter> {
    for line in &mut lines {
        trim_styled_line(line);
    }
    let block_width = source_lines
        .iter()
        .map(|line| line.bbox)
        .reduce(BoundingBox::union)
        .map_or(0.0, BoundingBox::width);
    let mut output = Vec::new();
    let mut previous_text = String::new();
    let mut previous_source_line = None::<&Line>;
    for (line_index, line) in lines.into_iter().enumerate() {
        if line.is_empty() {
            continue;
        }
        let line_text = styled_line_text(&line);
        let source_line = source_lines.get(line_index);
        let strong_boundary = looks_like_prompt_boundary(&line_text)
            || looks_like_bracketed_prompt_heading(&previous_text)
            || prompt_sentence_boundary(&previous_text, &line_text);
        let visual_continuation =
            source_line
                .zip(previous_source_line)
                .is_some_and(|(current, previous)| {
                    prompt_line_is_visual_continuation(previous, current, block_width, &line_text)
                });
        let structural_break = !output.is_empty() && strong_boundary && !visual_continuation;
        if structural_break {
            output.push(unstyled_character('\n'));
            output.extend(line);
        } else if output.is_empty() {
            output.extend(line);
        } else {
            append_styled_visual_line(&mut output, line);
        }
        previous_text = line_text;
        previous_source_line = source_line;
    }
    trim_styled_output(&mut output);
    output
}

fn prompt_line_is_visual_continuation(
    previous: &Line,
    current: &Line,
    block_width: f32,
    current_text: &str,
) -> bool {
    if looks_like_bracketed_prompt_heading(current_text)
        || looks_like_numbered_prompt_item(current_text)
        || looks_like_speaker_turn(current_text)
    {
        return false;
    }
    let left_tolerance = previous
        .bbox
        .height()
        .max(current.bbox.height())
        .mul_add(0.35, 1.0);
    let aligned = (previous.bbox.x0 - current.bbox.x0).abs() <= left_tolerance;
    let previous_fills_line = block_width > 0.0 && previous.bbox.width() >= block_width * 0.82;
    let starts_lowercase = current_text
        .chars()
        .find(|character| character.is_alphabetic())
        .is_some_and(|character| character.is_lowercase());
    aligned && (previous_fills_line || starts_lowercase)
}

fn append_styled_visual_line(
    output: &mut Vec<StyledSourceCharacter>,
    line: Vec<StyledSourceCharacter>,
) {
    let dehyphenate = output
        .last()
        .is_some_and(|character| character.character == '-')
        && line
            .first()
            .is_some_and(|character| character.character.is_lowercase());
    if dehyphenate {
        output.pop();
    } else if !output
        .last()
        .is_some_and(|character| character.character.is_whitespace())
    {
        output.push(unstyled_character(' '));
    }
    output.extend(line);
}

fn styled_line_text(line: &[StyledSourceCharacter]) -> String {
    line.iter().map(|character| character.character).collect()
}

fn trim_styled_line(line: &mut Vec<StyledSourceCharacter>) {
    while line
        .first()
        .is_some_and(|character| character.character.is_whitespace())
    {
        line.remove(0);
    }
    trim_styled_line_end(line);
}

fn trim_styled_line_end(line: &mut Vec<StyledSourceCharacter>) {
    while line
        .last()
        .is_some_and(|character| character.character.is_whitespace())
    {
        line.pop();
    }
}

fn trim_styled_output(output: &mut Vec<StyledSourceCharacter>) {
    while output
        .first()
        .is_some_and(|character| character.character.is_whitespace())
    {
        output.remove(0);
    }
    while output
        .last()
        .is_some_and(|character| character.character.is_whitespace())
    {
        output.pop();
    }
}

pub(super) fn unstyled_character(character: char) -> StyledSourceCharacter {
    StyledSourceCharacter {
        character,
        style_id: None,
    }
}

pub(super) fn styled_characters_to_source(
    characters: Vec<StyledSourceCharacter>,
) -> NormalizedStyledSource {
    let mut text = String::new();
    let mut boundaries = Vec::new();
    let mut active_style = None;
    for character in characters {
        if character.style_id != active_style {
            if let Some(style_id) = active_style {
                boundaries.push(StyleBoundary {
                    offset: text.len(),
                    style_id,
                    start: false,
                });
            }
            if let Some(style_id) = character.style_id {
                boundaries.push(StyleBoundary {
                    offset: text.len(),
                    style_id,
                    start: true,
                });
            }
            active_style = character.style_id;
        }
        text.push(character.character);
    }
    if let Some(style_id) = active_style {
        boundaries.push(StyleBoundary {
            offset: text.len(),
            style_id,
            start: false,
        });
    }
    NormalizedStyledSource { text, boundaries }
}

fn dominant_text_style(block: &Block) -> Option<TextStyle> {
    let mut candidates = Vec::<(TextStyle, usize)>::new();
    for segment in block
        .content
        .lines
        .iter()
        .flat_map(|line| line.segments.iter())
    {
        if segment.kind == SegmentKind::InlineObject {
            continue;
        }
        let weight = segment
            .text
            .chars()
            .filter(|character| !character.is_whitespace())
            .count();
        if weight == 0 {
            continue;
        }
        if let Some((_, total)) = candidates
            .iter_mut()
            .find(|(style, _)| !meaningful_style_difference(style, &segment.style))
        {
            *total += weight;
        } else {
            candidates.push((segment.style.clone(), weight));
        }
    }
    candidates
        .into_iter()
        .max_by_key(|(_, weight)| *weight)
        .map(|(style, _)| style)
}

fn meaningful_style_difference(left: &TextStyle, right: &TextStyle) -> bool {
    left.font_class != right.font_class
        || left.baseline_position != right.baseline_position
        || left.bold != right.bold
        || left.italic != right.italic
        || left.underline != right.underline
        || left.strikeout != right.strikeout
        || left.color_argb != right.color_argb
        || match (left.font_size, right.font_size) {
            (Some(left), Some(right)) => (left - right).abs() > 0.5,
            (None, None) => false,
            _ => true,
        }
}

fn protect_text_with_identifier_policy(text: &str, protect_identifiers: bool) -> ProtectedText {
    protect_text_with_policies(text, protect_identifiers, false)
}

fn protect_text_with_policies(
    text: &str,
    protect_identifiers: bool,
    protect_line_breaks: bool,
) -> ProtectedText {
    protect_text_with_style_boundaries(text, protect_identifiers, protect_line_breaks, &[], None)
}

fn protect_text_with_style_boundaries(
    text: &str,
    protect_identifiers: bool,
    protect_line_breaks: bool,
    boundaries: &[StyleBoundary],
    structural_marker: Option<&str>,
) -> ProtectedText {
    let mut protected = String::with_capacity(text.len());
    let mut tokens = Vec::new();
    let mut cursor = 0;
    let mut boundary_index = 0;
    while cursor <= text.len() {
        while let Some(boundary) = boundaries.get(boundary_index)
            && boundary.offset <= cursor
        {
            let kind = if boundary.start {
                ProtectedTokenKind::StyleStart
            } else {
                ProtectedTokenKind::StyleEnd
            };
            let original = style_marker(boundary.style_id, boundary.start);
            let placeholder = placeholder(kind, tokens.len(), &original);
            protected.push_str(&placeholder);
            tokens.push(ProtectedToken {
                placeholder,
                original,
                kind,
                styled_restoration: None,
            });
            boundary_index += 1;
        }
        if cursor == text.len() {
            break;
        }
        let rest = &text[cursor..];
        if cursor == 0
            && let Some(marker) = structural_marker.filter(|marker| !marker.is_empty())
            && rest.starts_with(marker)
        {
            let kind = ProtectedTokenKind::StructuralMarker;
            let placeholder = placeholder(kind, tokens.len(), marker);
            let end = cursor + marker.len();
            let mut restoration = String::with_capacity(marker.len());
            let mut restoration_cursor = cursor;
            while let Some(boundary) = boundaries.get(boundary_index)
                && boundary.offset < end
            {
                restoration.push_str(&text[restoration_cursor..boundary.offset]);
                restoration.push_str(&style_marker(boundary.style_id, boundary.start));
                restoration_cursor = boundary.offset;
                boundary_index += 1;
            }
            restoration.push_str(&text[restoration_cursor..end]);
            let styled_restoration = (restoration != marker).then_some(restoration);
            protected.push_str(&placeholder);
            tokens.push(ProtectedToken {
                placeholder,
                original: marker.to_string(),
                kind,
                styled_restoration,
            });
            cursor = end;
            continue;
        }
        if protect_line_breaks && rest.starts_with('\n') {
            let original = "\n";
            let kind = ProtectedTokenKind::LineBreak;
            let placeholder = placeholder(kind, tokens.len(), original);
            protected.push_str(&placeholder);
            tokens.push(ProtectedToken {
                placeholder,
                original: original.to_string(),
                kind,
                styled_restoration: None,
            });
            cursor += original.len();
            continue;
        }
        if let Some((kind, length)) = protected_prefix(rest, protect_identifiers) {
            let original = &rest[..length];
            let placeholder = placeholder(kind, tokens.len(), original);
            let end = cursor + length;
            let mut restoration = String::with_capacity(length);
            let mut restoration_cursor = cursor;
            while let Some(boundary) = boundaries.get(boundary_index)
                && boundary.offset < end
            {
                restoration.push_str(&text[restoration_cursor..boundary.offset]);
                restoration.push_str(&style_marker(boundary.style_id, boundary.start));
                restoration_cursor = boundary.offset;
                boundary_index += 1;
            }
            restoration.push_str(&text[restoration_cursor..end]);
            let styled_restoration = (restoration != original).then_some(restoration);
            protected.push_str(&placeholder);
            tokens.push(ProtectedToken {
                placeholder,
                original: original.to_string(),
                kind,
                styled_restoration,
            });
            cursor += length;
            continue;
        }
        let character = rest
            .chars()
            .next()
            .expect("cursor is on a character boundary");
        protected.push(character);
        cursor += character.len_utf8();
    }
    ProtectedText {
        text: protected,
        tokens,
    }
}

pub(super) fn style_marker(style_id: usize, start: bool) -> String {
    format!(
        "\u{e000}{}{:04}\u{e001}",
        if start { 'S' } else { 'E' },
        style_id
    )
}

pub(super) fn strip_style_placeholders(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut cursor = 0;
    while let Some(relative_start) = value[cursor..].find("GLYPHST_STYLE_") {
        let core_start = cursor + relative_start;
        let Some(core_length) = style_placeholder_core_length(&value[core_start..]) else {
            let prefix_end = core_start + "GLYPHST_STYLE_".len();
            output.push_str(&value[cursor..prefix_end]);
            cursor = prefix_end;
            continue;
        };
        let marker_start = if value[..core_start].ends_with("[[") {
            core_start - 2
        } else if value[..core_start].ends_with('[') {
            core_start - 1
        } else {
            core_start
        };
        let mut marker_end = core_start + core_length;
        for _ in 0..2 {
            if value[marker_end..].starts_with(']') {
                marker_end += 1;
            }
        }
        output.push_str(&value[cursor..marker_start]);
        cursor = marker_end;
    }
    output.push_str(&value[cursor..]);
    output
}

fn style_placeholder_core_length(value: &str) -> Option<usize> {
    let suffix = value.strip_prefix("GLYPHST_STYLE_")?;
    let kind = if suffix.starts_with("START_") {
        "START_"
    } else if suffix.starts_with("END_") {
        "END_"
    } else {
        return None;
    };
    let fields = &suffix[kind.len()..];
    let (style_id, checksum) = fields.split_once('_')?;
    let checksum = checksum.get(..8)?;
    if style_id.len() != 4
        || !style_id.bytes().all(|byte| byte.is_ascii_digit())
        || !checksum.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return None;
    }
    let core_length = "GLYPHST_STYLE_".len() + kind.len() + style_id.len() + 1 + checksum.len();
    value
        .get(core_length..)
        .and_then(|tail| tail.chars().next())
        .filter(|character| character.is_ascii_alphanumeric() || *character == '_')
        .is_none()
        .then_some(core_length)
}

pub(super) fn parse_styled_translation(
    value: &str,
    definitions: &[StyleDefinition],
) -> (String, Vec<StyledRun>, bool) {
    let mut runs = Vec::<StyledRun>::new();
    let mut active_style = None::<usize>;
    let mut cursor = 0usize;
    let mut invalid = false;
    while let Some(relative_start) = value[cursor..].find('\u{e000}') {
        let start = cursor + relative_start;
        append_styled_run(
            &mut runs,
            &value[cursor..start],
            active_style.and_then(|id| {
                definitions
                    .iter()
                    .find(|definition| definition.id == id)
                    .map(|definition| definition.style.clone())
            }),
        );
        let marker = &value[start + '\u{e000}'.len_utf8()..];
        let Some(relative_end) = marker.find('\u{e001}') else {
            invalid = true;
            break;
        };
        let marker_text = &marker[..relative_end];
        let marker_end = start + '\u{e000}'.len_utf8() + relative_end + '\u{e001}'.len_utf8();
        let parsed = marker_text
            .get(1..)
            .and_then(|value| value.parse::<usize>().ok());
        match (marker_text.chars().next(), parsed) {
            (Some('S'), Some(style_id))
                if active_style.is_none()
                    && definitions
                        .iter()
                        .any(|definition| definition.id == style_id) =>
            {
                active_style = Some(style_id);
            }
            (Some('E'), Some(style_id)) if active_style == Some(style_id) => {
                active_style = None;
            }
            _ => invalid = true,
        }
        cursor = marker_end;
    }
    if !invalid {
        append_styled_run(
            &mut runs,
            &value[cursor..],
            active_style.and_then(|id| {
                definitions
                    .iter()
                    .find(|definition| definition.id == id)
                    .map(|definition| definition.style.clone())
            }),
        );
        invalid |= active_style.is_some();
    }
    if invalid {
        let plain = strip_style_markers(value).trim().to_string();
        let runs = (!plain.is_empty())
            .then(|| StyledRun {
                text: plain.clone(),
                style: None,
            })
            .into_iter()
            .collect();
        return (plain, runs, true);
    }
    trim_styled_runs(&mut runs);
    let plain = runs.iter().map(|run| run.text.as_str()).collect::<String>();
    (plain, runs, false)
}

fn append_styled_run(runs: &mut Vec<StyledRun>, text: &str, style: Option<TextStyle>) {
    if text.is_empty() {
        return;
    }
    if let Some(last) = runs.last_mut()
        && last.style == style
    {
        last.text.push_str(text);
    } else {
        runs.push(StyledRun {
            text: text.to_string(),
            style,
        });
    }
}

fn trim_styled_runs(runs: &mut Vec<StyledRun>) {
    while runs.first().is_some_and(|run| run.text.trim().is_empty()) {
        runs.remove(0);
    }
    while runs.last().is_some_and(|run| run.text.trim().is_empty()) {
        runs.pop();
    }
    if let Some(first) = runs.first_mut() {
        first.text = first.text.trim_start().to_string();
    }
    if let Some(last) = runs.last_mut() {
        last.text = last.text.trim_end().to_string();
    }
    runs.retain(|run| !run.text.is_empty());
}

pub(super) fn normalize_cjk_styled_spacing(runs: &mut Vec<StyledRun>) {
    let characters = runs
        .iter()
        .flat_map(|run| {
            run.text
                .chars()
                .map(|character| (character, run.style.clone()))
        })
        .collect::<Vec<_>>();
    if !characters
        .iter()
        .any(|(character, _)| is_cjk_character(*character))
    {
        return;
    }

    let mut normalized = Vec::<(char, Option<TextStyle>)>::with_capacity(characters.len());
    let mut index = 0usize;
    while index < characters.len() {
        let (character, style) = &characters[index];
        if !is_horizontal_whitespace(*character) {
            normalized.push((*character, style.clone()));
            index += 1;
            continue;
        }

        let whitespace_style = style.clone();
        let mut next_index = index + 1;
        while next_index < characters.len() && is_horizontal_whitespace(characters[next_index].0) {
            next_index += 1;
        }
        let previous = normalized.last().map(|(character, _)| *character);
        let next = characters.get(next_index).map(|(character, _)| *character);
        let remove = previous
            .zip(next)
            .is_some_and(|(left, right)| cjk_boundary_needs_no_space(left, right));
        if !remove
            && !normalized
                .last()
                .is_some_and(|(character, _)| is_horizontal_whitespace(*character))
        {
            normalized.push((' ', whitespace_style));
        }
        index = next_index;
    }

    runs.clear();
    for (character, style) in normalized {
        let mut encoded = [0; 4];
        append_styled_run(runs, character.encode_utf8(&mut encoded), style);
    }
    trim_styled_runs(runs);
}

fn is_horizontal_whitespace(character: char) -> bool {
    character.is_whitespace() && !matches!(character, '\n' | '\r')
}

fn cjk_boundary_needs_no_space(left: char, right: char) -> bool {
    (is_cjk_character(left) && (is_cjk_character(right) || right.is_ascii_alphanumeric()))
        || (left.is_ascii_alphanumeric() && is_cjk_character(right))
        || is_cjk_closing_punctuation(right)
        || is_cjk_opening_punctuation(left)
}

pub(super) fn is_cjk_character(character: char) -> bool {
    matches!(
        character as u32,
        0x2e80..=0x2fff
            | 0x3040..=0x30ff
            | 0x3100..=0x312f
            | 0x31a0..=0x31bf
            | 0x3400..=0x4dbf
            | 0x4e00..=0x9fff
            | 0xac00..=0xd7af
            | 0xf900..=0xfaff
            | 0x20000..=0x2fa1f
    )
}

fn is_cjk_closing_punctuation(character: char) -> bool {
    matches!(
        character,
        '，' | '。'
            | '、'
            | '；'
            | '：'
            | '！'
            | '？'
            | '）'
            | '】'
            | '》'
            | '〉'
            | '」'
            | '』'
            | '”'
            | '’'
    )
}

fn is_cjk_opening_punctuation(character: char) -> bool {
    matches!(
        character,
        '（' | '【' | '《' | '〈' | '「' | '『' | '“' | '‘'
    )
}

fn strip_style_markers(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut remaining = value;
    while let Some(start) = remaining.find('\u{e000}') {
        output.push_str(&remaining[..start]);
        let marker = &remaining[start + '\u{e000}'.len_utf8()..];
        let Some(end) = marker.find('\u{e001}') else {
            output.push_str(marker);
            return output;
        };
        remaining = &marker[end + '\u{e001}'.len_utf8()..];
    }
    output.push_str(remaining);
    output
}
