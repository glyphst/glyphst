use std::{collections::HashSet, ops::Range, path::PathBuf, sync::Arc};

use glyphst_core::{RegionImageReference, SelectedPagesReference};

use crate::agent::{AgentContentPart, AgentImage, AgentMessage, AgentRole};

const MAX_CONTEXT_CHARS: usize = 12_000;
const CONTEXT_ENVELOPE_HEADER: &str = "Attached document context from @ mentions:\n<context>\nThe following document context was attached by the user. It is current; do not ask to re-read it.\n";
pub const MAX_PAGE_IMAGE_BYTES: usize = 16 * 1024 * 1024;

pub const DEFAULT_SESSION_TITLE: &str = "New Chat";
pub const TITLE_GENERATION_PROMPT: &str = "Generate a concise 3-7 word title for this conversation, \
omitting punctuation. Go straight to the title, without any preamble or prefix like `Title:`. \
If the conversation is about a specific subject, include it in the title. Be descriptive. \
Do not speak in the first person.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}

#[derive(Clone, Debug)]
pub enum ContentPart {
    Text(String),
    Context(ContextBlock),
    Image(ImageContext),
    UserImage(AgentImage),
}

#[derive(Clone, Debug)]
pub struct ContextBlock {
    pub kind: MentionKind,
    pub page: Option<usize>,
    pub text: String,
}

impl ContextBlock {
    pub fn new(kind: MentionKind, page: Option<usize>, text: String) -> Self {
        Self {
            kind,
            page,
            text: truncate_context(text)
                .unwrap_or_else(|| kind.empty_context_message().to_string()),
        }
    }
}

#[derive(Clone, Debug)]
pub struct ImageContext {
    pub kind: MentionKind,
    pub page: Option<usize>,
    pub region: Option<Arc<RegionImageReference>>,
    pub media_type: String,
    pub bytes: Arc<[u8]>,
}

impl ImageContext {
    pub fn new(
        kind: MentionKind,
        page: Option<usize>,
        media_type: impl Into<String>,
        bytes: impl Into<Arc<[u8]>>,
    ) -> Self {
        Self {
            kind,
            page,
            region: None,
            media_type: media_type.into(),
            bytes: bytes.into(),
        }
    }

    pub fn new_region(
        region: RegionImageReference,
        media_type: impl Into<String>,
        bytes: impl Into<Arc<[u8]>>,
    ) -> Self {
        Self {
            kind: MentionKind::RegionImage,
            page: None,
            region: Some(Arc::new(region)),
            media_type: media_type.into(),
            bytes: bytes.into(),
        }
    }

    fn descriptor(&self) -> String {
        let reference = if self.kind == MentionKind::RegionImage {
            self.region.as_ref().map_or_else(
                || self.kind.token().to_string(),
                |region| {
                    let mut pages = region
                        .regions
                        .iter()
                        .map(|region| region.page.0 + 1)
                        .collect::<Vec<_>>();
                    pages.dedup();
                    let pages = pages
                        .into_iter()
                        .map(|page| page.to_string())
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!("{} pages {pages}", self.kind.token())
                },
            )
        } else if matches!(
            self.kind,
            MentionKind::DocumentImage | MentionKind::SelectedPagesImage
        ) {
            self.page.map_or_else(
                || self.kind.token().to_string(),
                |page| format!("{} page {page}", self.kind.token()),
            )
        } else {
            MentionReference::new(self.kind, self.page).display_text()
        };
        format!(
            "[Attached {reference} image, media type {}]",
            self.media_type
        )
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct MentionOccurrence {
    pub range: Range<usize>,
    pub reference: MentionReference,
    pub region_reference: Option<RegionImageReference>,
    pub selected_pages_reference: Option<SelectedPagesReference>,
}

/// Internal identity for the document that supplied a turn's implicit context
/// and mentions. The path is never exposed to a model; UI/tool layers replace
/// it with an opaque per-run document id and the display label.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocumentContextScope {
    pub path: PathBuf,
    pub label: String,
}

impl MentionOccurrence {
    pub fn new(range: Range<usize>, reference: MentionReference) -> Self {
        Self {
            range,
            reference,
            region_reference: None,
            selected_pages_reference: None,
        }
    }

    pub fn with_region_reference(mut self, reference: RegionImageReference) -> Self {
        self.region_reference = Some(reference);
        self
    }

    pub fn with_selected_pages_reference(mut self, reference: SelectedPagesReference) -> Self {
        self.selected_pages_reference = Some(reference);
        self
    }

    pub fn range(&self) -> &Range<usize> {
        &self.range
    }

    pub fn reference(&self) -> MentionReference {
        self.reference
    }

    pub fn region_reference(&self) -> Option<&RegionImageReference> {
        self.region_reference.as_ref()
    }

    pub fn selected_pages_reference(&self) -> Option<&SelectedPagesReference> {
        self.selected_pages_reference.as_ref()
    }
}

#[derive(Clone, Debug)]
pub struct ChatMessage {
    pub role: Role,
    pub parts: Vec<ContentPart>,
    references: Vec<MentionReference>,
    region_references: Vec<RegionImageReference>,
    mention_occurrences: Vec<MentionOccurrence>,
    request_prompt: Option<String>,
    request_content: Option<String>,
    agent_trace: Vec<AgentMessage>,
    document_scope: Option<DocumentContextScope>,
    /// The `<reader_state>` block sent at the end of the message.
    reader_state: Option<String>,
}

impl ChatMessage {
    pub fn user(
        display_content: String,
        request_prompt: String,
        contexts: Vec<ContextBlock>,
        references: Vec<MentionReference>,
    ) -> Self {
        let mut parts = vec![ContentPart::Text(display_content)];
        parts.extend(contexts.into_iter().map(ContentPart::Context));
        let request_prompt = normalized_request_content(&parts, request_prompt);
        Self {
            role: Role::User,
            parts,
            references,
            region_references: Vec::new(),
            mention_occurrences: Vec::new(),
            request_prompt,
            request_content: None,
            agent_trace: Vec::new(),
            document_scope: None,
            reader_state: None,
        }
    }

    pub fn user_text(content: String) -> Self {
        Self {
            role: Role::User,
            parts: vec![ContentPart::Text(content)],
            references: Vec::new(),
            region_references: Vec::new(),
            mention_occurrences: Vec::new(),
            request_prompt: None,
            request_content: None,
            agent_trace: Vec::new(),
            document_scope: None,
            reader_state: None,
        }
    }

    pub fn assistant(content: String) -> Self {
        Self {
            role: Role::Assistant,
            parts: vec![ContentPart::Text(content)],
            references: Vec::new(),
            region_references: Vec::new(),
            mention_occurrences: Vec::new(),
            request_prompt: None,
            request_content: None,
            agent_trace: Vec::new(),
            document_scope: None,
            reader_state: None,
        }
    }

    pub fn from_persisted(role: Role, content: String, request_content: Option<String>) -> Self {
        let request_content = request_content
            .map(|content| content.trim().to_string())
            .filter(|request| !request.is_empty() && request != &content);
        Self {
            role,
            parts: vec![ContentPart::Text(content)],
            references: Vec::new(),
            region_references: Vec::new(),
            mention_occurrences: Vec::new(),
            request_prompt: None,
            request_content,
            agent_trace: Vec::new(),
            document_scope: None,
            reader_state: None,
        }
    }

    pub fn display_text(&self) -> String {
        let mut text = String::new();
        for part in &self.parts {
            if let ContentPart::Text(part_text) = part {
                text.push_str(part_text);
            }
        }
        text
    }

    pub fn request_text(&self) -> String {
        let has_persisted_projection = self.request_content.is_some();
        let mut text = self
            .request_content
            .clone()
            .or_else(|| self.request_prompt.clone())
            .unwrap_or_default();
        let mut contexts = Vec::new();
        let mut images = Vec::new();
        for part in &self.parts {
            match part {
                ContentPart::Text(part_text)
                    if self.request_prompt.is_none() && !has_persisted_projection =>
                {
                    text.push_str(part_text);
                }
                ContentPart::Text(_) => {}
                ContentPart::Context(context) if !Self::context_is_projected(&text, context) => {
                    contexts.push(context);
                }
                ContentPart::Context(_) => {}
                ContentPart::Image(image) if !text.contains(&image.descriptor()) => {
                    images.push(image);
                }
                ContentPart::Image(_) | ContentPart::UserImage(_) => {}
            }
        }

        if let Some(context) = context_envelope(&contexts) {
            if !text.trim().is_empty() {
                text.push_str("\n\n");
            }
            text.push_str(&context);
        }

        if !images.is_empty() {
            if !text.trim().is_empty() {
                text.push_str("\n\n");
            }
            text.push_str("Attached image context:\n");
            for image in images {
                text.push_str(&image.descriptor());
                text.push('\n');
            }
            text.pop();
        }

        if let Some(state) = &self.reader_state {
            if !text.trim().is_empty() {
                text.push_str("\n\n");
            }
            text.push_str(state);
        }

        text
    }

    /// Ends the request text with what the reader showed (see
    /// [`crate::prompt::ReaderState`]).
    pub fn with_reader_state(mut self, block: String) -> Self {
        self.reader_state = Some(block);
        self
    }

    pub fn editable_contexts(&self) -> Vec<ContextBlock> {
        let live_contexts = self
            .parts
            .iter()
            .filter_map(|part| match part {
                ContentPart::Context(context) => Some(context.clone()),
                ContentPart::Text(_) | ContentPart::Image(_) | ContentPart::UserImage(_) => None,
            })
            .collect::<Vec<_>>();
        let mut contexts = self
            .request_content
            .as_deref()
            .map(parse_context_envelope)
            .unwrap_or_default();
        let mut replaced = HashSet::new();
        for live in live_contexts {
            if let Some((index, persisted)) =
                contexts.iter_mut().enumerate().find(|(index, context)| {
                    !replaced.contains(index)
                        && context.kind == live.kind
                        && context.page == live.page
                })
            {
                *persisted = live;
                replaced.insert(index);
            } else {
                contexts.push(live);
            }
        }
        contexts
    }

    fn context_is_projected(text: &str, context: &ContextBlock) -> bool {
        let tag = context.kind.context_tag();
        let opening = context.page.map_or_else(
            || format!("<{tag}>"),
            |page| format!("<{tag} page=\"{page}\">"),
        );
        text.contains(&opening)
    }

    pub fn references(&self) -> &[MentionReference] {
        &self.references
    }

    pub fn region_references(&self) -> &[RegionImageReference] {
        &self.region_references
    }

    pub fn mention_occurrences(&self) -> &[MentionOccurrence] {
        &self.mention_occurrences
    }

    pub fn has_resolved_page_reference(&self, reference: MentionReference) -> bool {
        self.parts.iter().any(|part| match part {
            ContentPart::Context(context) => {
                matches!(
                    reference.kind,
                    MentionKind::Page | MentionKind::SelectedPagesText
                ) && context.kind == reference.kind
                    && context.page == reference.page
            }
            ContentPart::Image(image) => {
                matches!(
                    reference.kind,
                    MentionKind::PageImage
                        | MentionKind::DocumentImage
                        | MentionKind::SelectedPagesImage
                ) && image.kind == reference.kind
                    && image.page == reference.page
            }
            ContentPart::Text(_) | ContentPart::UserImage(_) => false,
        })
    }

    pub fn image_context_bytes(&self) -> usize {
        self.parts
            .iter()
            .filter_map(|part| match part {
                ContentPart::Image(image) => Some(image.bytes.len()),
                ContentPart::UserImage(image) => Some(image.bytes.len()),
                ContentPart::Text(_) | ContentPart::Context(_) => None,
            })
            .sum()
    }

    pub fn with_references(mut self, references: Vec<MentionReference>) -> Self {
        self.references = references;
        self
    }

    pub fn with_region_references(mut self, references: Vec<RegionImageReference>) -> Self {
        self.region_references = references;
        self
    }

    pub fn with_mention_occurrences(mut self, occurrences: Vec<MentionOccurrence>) -> Self {
        self.mention_occurrences = occurrences;
        self
    }

    pub fn with_document_scope(mut self, scope: Option<DocumentContextScope>) -> Self {
        self.document_scope = scope;
        self
    }

    pub fn document_scope(&self) -> Option<&DocumentContextScope> {
        self.document_scope.as_ref()
    }

    pub fn has_resolved_region_reference(&self, reference: &RegionImageReference) -> bool {
        self.parts.iter().any(|part| match part {
            ContentPart::Image(image) => {
                image.kind == MentionKind::RegionImage && image.region.as_deref() == Some(reference)
            }
            ContentPart::Text(_) | ContentPart::Context(_) | ContentPart::UserImage(_) => false,
        })
    }

    pub fn attach_context(&mut self, context: ContextBlock) {
        self.parts.push(ContentPart::Context(context));
    }

    pub fn attach_image(&mut self, image: ImageContext) {
        self.parts.push(ContentPart::Image(image));
    }

    pub fn attach_user_image(&mut self, image: AgentImage) {
        self.parts.push(ContentPart::UserImage(image));
    }

    pub fn user_images(&self) -> impl Iterator<Item = &AgentImage> {
        self.parts.iter().filter_map(|part| match part {
            ContentPart::UserImage(image) => Some(image),
            _ => None,
        })
    }

    pub fn agent_trace(&self) -> &[AgentMessage] {
        &self.agent_trace
    }

    pub fn set_agent_trace(&mut self, trace: Vec<AgentMessage>) {
        self.agent_trace = trace;
    }

    pub fn into_agent_messages(&self) -> Vec<AgentMessage> {
        if self.role == Role::Assistant && !self.agent_trace.is_empty() {
            return self.agent_trace.clone();
        }
        let mut content = Vec::new();
        let text = self.request_text();
        if !text.is_empty() {
            content.push(AgentContentPart::Text(text));
        }
        for part in &self.parts {
            match part {
                ContentPart::Image(image) => content.push(AgentContentPart::Image(
                    AgentImage::new(image.media_type.clone(), 0, 0, image.bytes.clone()),
                )),
                ContentPart::UserImage(image) => {
                    content.push(AgentContentPart::Image(image.clone()))
                }
                ContentPart::Text(_) | ContentPart::Context(_) => {}
            }
        }
        vec![AgentMessage {
            role: match self.role {
                Role::User => AgentRole::User,
                Role::Assistant => AgentRole::Assistant,
            },
            content,
        }]
    }

    pub fn title_safe(&self) -> Self {
        Self {
            role: self.role,
            parts: vec![ContentPart::Text(self.display_text())],
            references: Vec::new(),
            region_references: Vec::new(),
            mention_occurrences: Vec::new(),
            request_prompt: None,
            request_content: None,
            agent_trace: Vec::new(),
            document_scope: self.document_scope.clone(),
            reader_state: None,
        }
    }

    pub fn is_display_empty(&self) -> bool {
        self.display_text().trim().is_empty() && self.user_images().next().is_none()
    }

    pub fn push_text(&mut self, text: &str) {
        if let Some(existing) = self.parts.iter_mut().find_map(|part| match part {
            ContentPart::Text(existing) => Some(existing),
            ContentPart::Context(_) | ContentPart::Image(_) | ContentPart::UserImage(_) => None,
        }) {
            existing.push_str(text);
        } else {
            self.parts.push(ContentPart::Text(text.to_string()));
        }
        self.request_prompt = None;
        self.request_content = None;
    }
}

fn normalized_request_content(parts: &[ContentPart], request_prompt: String) -> Option<String> {
    let request_prompt = request_prompt.trim().to_string();
    let display_content = parts
        .iter()
        .filter_map(|part| match part {
            ContentPart::Text(text) => Some(text.as_str()),
            ContentPart::Context(_) | ContentPart::Image(_) | ContentPart::UserImage(_) => None,
        })
        .collect::<String>();
    (request_prompt != display_content).then_some(request_prompt)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MentionKind {
    Page,
    PageImage,
    SelectedPagesText,
    SelectedPagesImage,
    Selection,
    DocumentText,
    DocumentImage,
    RegionImage,
}

impl MentionKind {
    pub const ALL: [Self; 8] = [
        Self::Page,
        Self::PageImage,
        Self::SelectedPagesText,
        Self::SelectedPagesImage,
        Self::Selection,
        Self::DocumentText,
        Self::DocumentImage,
        Self::RegionImage,
    ];

    pub fn token(self) -> &'static str {
        match self {
            Self::Page => "@page",
            Self::PageImage => "@page-image",
            Self::SelectedPagesText => "@selected-pages-text",
            Self::SelectedPagesImage => "@selected-pages-image",
            Self::Selection => "@selection",
            Self::DocumentText => "@document",
            Self::DocumentImage => "@document-image",
            Self::RegionImage => "@region-image",
        }
    }

    pub fn label(self) -> &'static str {
        self.token().strip_prefix('@').unwrap_or(self.token())
    }

    pub fn description(self) -> String {
        match self {
            Self::Page => glyphst_settings::localize::tr("mention-current-page-text-description"),
            Self::PageImage => glyphst_settings::localize::tr("mention-page-image-description"),
            Self::SelectedPagesText => {
                glyphst_settings::localize::tr("mention-selected-pages-text-description")
            }
            Self::SelectedPagesImage => {
                glyphst_settings::localize::tr("mention-selected-pages-image-description")
            }
            Self::Selection => glyphst_settings::localize::tr("mention-selection-description"),
            Self::DocumentText => {
                glyphst_settings::localize::tr("mention-document-text-description")
            }
            Self::DocumentImage => {
                glyphst_settings::localize::tr("mention-document-image-description")
            }
            Self::RegionImage => glyphst_settings::localize::tr("mention-region-image-description"),
        }
    }

    pub fn context_title(self) -> String {
        match self {
            Self::Page => "Page".to_string(),
            Self::PageImage => "Page image".to_string(),
            Self::SelectedPagesText => {
                glyphst_settings::localize::tr("mention-selected-pages-text")
            }
            Self::SelectedPagesImage => {
                glyphst_settings::localize::tr("mention-selected-pages-image")
            }
            Self::Selection => glyphst_settings::localize::tr("mention-selection"),
            Self::DocumentText => glyphst_settings::localize::tr("mention-document-text"),
            Self::DocumentImage => glyphst_settings::localize::tr("mention-document-image"),
            Self::RegionImage => glyphst_settings::localize::tr("mention-region-image"),
        }
    }

    pub fn requires_page_number(self) -> bool {
        matches!(self, Self::Page | Self::PageImage)
    }

    pub fn is_image(self) -> bool {
        matches!(
            self,
            Self::PageImage | Self::SelectedPagesImage | Self::DocumentImage | Self::RegionImage
        )
    }

    pub fn requires_fixed_layout(self) -> bool {
        matches!(
            self,
            Self::Page
                | Self::PageImage
                | Self::SelectedPagesText
                | Self::SelectedPagesImage
                | Self::DocumentImage
                | Self::RegionImage
        )
    }

    pub fn is_page_reference(self) -> bool {
        matches!(
            self,
            Self::Page
                | Self::PageImage
                | Self::SelectedPagesText
                | Self::SelectedPagesImage
                | Self::DocumentImage
        )
    }

    pub fn context_tag(self) -> &'static str {
        match self {
            Self::Page => "page",
            Self::PageImage => "page_image",
            Self::SelectedPagesText => "selected_pages_text",
            Self::SelectedPagesImage => "selected_pages_image",
            Self::Selection => "selection",
            Self::DocumentText => "document_text",
            Self::DocumentImage => "document_image",
            Self::RegionImage => "region_image",
        }
    }

    pub fn empty_context_message(self) -> String {
        match self {
            Self::Page => glyphst_settings::localize::tr("ai-no-current-page-text"),
            Self::PageImage => "No page image is available.".to_string(),
            Self::SelectedPagesText => glyphst_settings::localize::tr("ai-no-selected-pages-text"),
            Self::SelectedPagesImage => {
                glyphst_settings::localize::tr("ai-no-selected-pages-image")
            }
            Self::Selection => glyphst_settings::localize::tr("ai-no-selected-text"),
            Self::DocumentText => glyphst_settings::localize::tr("ai-no-document-text"),
            Self::DocumentImage => glyphst_settings::localize::tr("ai-no-document-image"),
            Self::RegionImage => glyphst_settings::localize::tr("ai-no-region-image"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MentionReference {
    pub kind: MentionKind,
    pub page: Option<usize>,
}

impl MentionReference {
    pub const fn new(kind: MentionKind, page: Option<usize>) -> Self {
        Self { kind, page }
    }

    pub fn display_text(self) -> String {
        match (self.kind.requires_page_number(), self.page) {
            (true, Some(page)) => format!("{}({page})", self.kind.token()),
            _ => self.kind.token().to_string(),
        }
    }
}

pub fn mentioned_kinds(text: &str) -> Vec<MentionKind> {
    let mut seen = HashSet::new();
    let mut mentions = Vec::new();
    for (_, reference) in find_mention_references(text) {
        if seen.insert(reference.kind) {
            mentions.push(reference.kind);
        }
    }
    mentions
}

pub fn mentioned_references(text: &str) -> Vec<MentionReference> {
    let mut seen = HashSet::new();
    let mut mentions = Vec::new();
    for (_, reference) in find_mention_references(text) {
        if seen.insert(reference) {
            mentions.push(reference);
        }
    }
    mentions
}

pub fn find_mention_ranges(text: &str) -> Vec<(Range<usize>, MentionKind)> {
    find_mention_references(text)
        .into_iter()
        .map(|(range, reference)| (range, reference.kind))
        .collect()
}

pub fn find_mention_references(text: &str) -> Vec<(Range<usize>, MentionReference)> {
    let mut ranges = Vec::new();
    for (start, ch) in text.char_indices() {
        if ch != '@' || !is_mention_start_boundary(text, start) {
            continue;
        }

        if let Some((end, reference)) = parse_page_mention(text, start, MentionKind::PageImage)
            .or_else(|| parse_page_mention(text, start, MentionKind::Page))
        {
            ranges.push((start..end, reference));
            continue;
        }

        const LEGACY_DOCUMENT_TOKEN: &str = "@document text";
        let legacy_end = start + LEGACY_DOCUMENT_TOKEN.len();
        if text[start..].starts_with(LEGACY_DOCUMENT_TOKEN)
            && is_mention_boundary(text[legacy_end..].chars().next())
        {
            ranges.push((
                start..legacy_end,
                MentionReference::new(MentionKind::DocumentText, None),
            ));
            continue;
        }

        for kind in [
            MentionKind::RegionImage,
            MentionKind::SelectedPagesImage,
            MentionKind::SelectedPagesText,
            MentionKind::Selection,
            MentionKind::DocumentImage,
            MentionKind::DocumentText,
        ] {
            let token = kind.token();
            let end = start + token.len();
            if text[start..].starts_with(token) && is_mention_boundary(text[end..].chars().next()) {
                ranges.push((start..end, MentionReference::new(kind, None)));
                break;
            }
        }
    }
    ranges
}

fn is_mention_start_boundary(text: &str, start: usize) -> bool {
    start == 0
        || text[..start]
            .chars()
            .next_back()
            .is_some_and(|ch| ch.is_whitespace() || matches!(ch, '(' | '[' | '{' | '"' | '\''))
}

fn parse_page_mention(
    text: &str,
    start: usize,
    kind: MentionKind,
) -> Option<(usize, MentionReference)> {
    let rest = text.get(start..)?;
    let digits = rest.strip_prefix(kind.token())?.strip_prefix('(')?;
    let digit_count = digits
        .bytes()
        .take_while(|byte| byte.is_ascii_digit())
        .count();
    if digit_count == 0 || digits.as_bytes().get(digit_count) != Some(&b')') {
        return None;
    }

    let page = digits[..digit_count].parse::<usize>().ok()?;
    if page == 0 {
        return None;
    }

    let end = start + kind.token().len() + 1 + digit_count + 1;
    is_mention_boundary(text[end..].chars().next())
        .then_some((end, MentionReference::new(kind, Some(page))))
}

fn is_mention_boundary(next: Option<char>) -> bool {
    next.is_none_or(|ch| {
        ch.is_whitespace() || matches!(ch, '.' | ',' | ';' | ':' | '!' | '?' | ')' | ']' | '}')
    })
}

pub fn mention_matches_query(mention: MentionKind, query: &str) -> bool {
    let query = query.trim_start().to_ascii_lowercase();
    if query.is_empty() {
        return true;
    }

    let query_without_trailing = query.trim_end();
    let label = mention.label().to_ascii_lowercase();
    let token = mention.token().trim_start_matches('@').to_ascii_lowercase();

    if query.ends_with(' ') && (query_without_trailing == label || query_without_trailing == token)
    {
        return false;
    }

    label.contains(query_without_trailing) || token.contains(query_without_trailing)
}

fn truncate_context(text: String) -> Option<String> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    Some(text.chars().take(MAX_CONTEXT_CHARS).collect())
}

fn context_envelope(contexts: &[&ContextBlock]) -> Option<String> {
    if contexts.is_empty() {
        return None;
    }

    let mut request = CONTEXT_ENVELOPE_HEADER.to_string();
    for context in contexts {
        let tag = context.kind.context_tag();
        let page = context
            .page
            .map(|page| format!(" page=\"{page}\""))
            .unwrap_or_default();
        request.push('\n');
        request.push('<');
        request.push_str(tag);
        request.push_str(&page);
        request.push_str(">\n");
        request.push_str(&context.kind.context_title());
        request.push_str(":\n");
        request.push_str(&escape_context(&context.text));
        request.push_str("\n</");
        request.push_str(tag);
        request.push_str(">\n");
    }
    request.push_str("</context>");
    Some(request)
}

fn escape_context(text: &str) -> String {
    let mut escaped = text.replace('&', "&amp;");
    for tag in
        std::iter::once("context").chain(MentionKind::ALL.into_iter().map(MentionKind::context_tag))
    {
        escaped = escaped.replace(&format!("</{tag}"), &format!("&lt;/{tag}"));
        escaped = escaped.replace(&format!("<{tag}"), &format!("&lt;{tag}"));
    }
    escaped
}

fn parse_context_envelope(request: &str) -> Vec<ContextBlock> {
    let Some(envelope_start) = request.find(CONTEXT_ENVELOPE_HEADER) else {
        return Vec::new();
    };
    let envelope = &request[envelope_start + CONTEXT_ENVELOPE_HEADER.len()..];
    let Some(envelope_end) = envelope.find("\n</context>") else {
        return Vec::new();
    };
    let envelope = &envelope[..envelope_end];
    let mut contexts = Vec::new();
    let mut cursor = 0;

    while cursor < envelope.len() {
        while envelope[cursor..].starts_with('\n') {
            cursor += 1;
            if cursor == envelope.len() {
                break;
            }
        }
        if cursor == envelope.len() {
            break;
        }

        let Some(opening_end) = envelope[cursor..].find('\n') else {
            break;
        };
        let opening = &envelope[cursor..cursor + opening_end];
        let content_start = cursor + opening_end + 1;
        let Some((kind, page)) = parse_context_opening(opening) else {
            cursor = content_start;
            continue;
        };

        let closing = format!("\n</{}>", kind.context_tag());
        let Some(closing_start) = envelope[content_start..].find(&closing) else {
            cursor = content_start;
            continue;
        };
        let closing_start = content_start + closing_start;
        let closing_end = closing_start + closing.len();
        if !matches!(envelope.as_bytes().get(closing_end), None | Some(b'\n')) {
            cursor = content_start;
            continue;
        }
        if let Some(nested_opening) = find_context_opening(&envelope[content_start..closing_start])
        {
            cursor = content_start + nested_opening;
            continue;
        }

        let content = &envelope[content_start..closing_start];
        let Some((title, text)) = content.split_once('\n') else {
            cursor = closing_end;
            continue;
        };
        if title
            .strip_suffix(':')
            .is_none_or(|title| title.trim().is_empty())
        {
            cursor = closing_end;
            continue;
        }

        contexts.push(ContextBlock {
            kind,
            page,
            text: unescape_context(text),
        });
        cursor = closing_end;
    }

    contexts
}

fn find_context_opening(text: &str) -> Option<usize> {
    text.match_indices('<').find_map(|(start, _)| {
        if start > 0 && text.as_bytes()[start - 1] != b'\n' {
            return None;
        }
        let end = text[start..]
            .find('\n')
            .map_or(text.len(), |end| start + end);
        parse_context_opening(&text[start..end]).map(|_| start)
    })
}

fn parse_context_opening(opening: &str) -> Option<(MentionKind, Option<usize>)> {
    let opening = opening.strip_prefix('<')?.strip_suffix('>')?;
    let (tag, attributes) = opening
        .split_once(' ')
        .map_or((opening, None), |(tag, attributes)| (tag, Some(attributes)));
    let kind = MentionKind::ALL
        .into_iter()
        .find(|kind| kind.context_tag() == tag)?;
    let Some(attributes) = attributes else {
        return Some((kind, None));
    };

    let page = attributes
        .strip_prefix("page=\"")?
        .strip_suffix('"')?
        .parse::<usize>()
        .ok()?;
    (page > 0).then_some((kind, Some(page)))
}

fn unescape_context(text: &str) -> String {
    let mut unescaped = text.to_string();
    for tag in
        std::iter::once("context").chain(MentionKind::ALL.into_iter().map(MentionKind::context_tag))
    {
        unescaped = unescaped.replace(&format!("&lt;/{tag}"), &format!("</{tag}"));
        unescaped = unescaped.replace(&format!("&lt;{tag}"), &format!("<{tag}"));
    }
    unescaped.replace("&amp;", "&")
}

pub fn session_title_for_prompt(prompt: &str) -> String {
    let title = prompt
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or(DEFAULT_SESSION_TITLE)
        .trim()
        .chars()
        .take(48)
        .collect::<String>();
    if title.is_empty() {
        DEFAULT_SESSION_TITLE.to_string()
    } else {
        title
    }
}

pub fn title_from_response(response: &str) -> Option<String> {
    let title = response
        .lines()
        .find(|line| !line.trim().is_empty())?
        .trim()
        .trim_matches(['"', '\'', '`'])
        .trim()
        .trim_end_matches(['.', '!', '?', ':'])
        .chars()
        .take(80)
        .collect::<String>();
    (!title.is_empty()).then_some(title)
}

#[cfg(test)]
mod tests;
