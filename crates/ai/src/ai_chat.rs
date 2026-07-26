use std::{collections::HashSet, io::Read as _, ops::Range, sync::Arc};

use anyhow::{Context as _, Result, anyhow};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use glyphst_core::RegionImageReference;
use serde_json::{Value, json};

use crate::settings::{AiApiStyle, AiProvider, AiThinkingLevel, ResolvedAiConfig};

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

impl Role {
    fn api_role(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
        }
    }
}

#[derive(Clone, Debug)]
pub enum ContentPart {
    Text(String),
    Context(ContextBlock),
    Image(ImageContext),
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
        } else if self.kind == MentionKind::DocumentImage {
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
}

impl MentionOccurrence {
    pub fn new(range: Range<usize>, reference: MentionReference) -> Self {
        Self {
            range,
            reference,
            region_reference: None,
        }
    }

    pub fn with_region_reference(mut self, reference: RegionImageReference) -> Self {
        self.region_reference = Some(reference);
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
                ContentPart::Image(_) => {}
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

        text
    }

    pub fn editable_contexts(&self) -> Vec<ContextBlock> {
        let live_contexts = self
            .parts
            .iter()
            .filter_map(|part| match part {
                ContentPart::Context(context) => Some(context.clone()),
                ContentPart::Text(_) | ContentPart::Image(_) => None,
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
                reference.kind == MentionKind::Page
                    && context.kind == reference.kind
                    && context.page == reference.page
            }
            ContentPart::Image(image) => {
                matches!(
                    reference.kind,
                    MentionKind::PageImage | MentionKind::DocumentImage
                ) && image.kind == reference.kind
                    && image.page == reference.page
            }
            ContentPart::Text(_) => false,
        })
    }

    pub fn image_context_bytes(&self) -> usize {
        self.parts
            .iter()
            .filter_map(|part| match part {
                ContentPart::Image(image) => Some(image.bytes.len()),
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

    pub fn has_resolved_region_reference(&self, reference: &RegionImageReference) -> bool {
        self.parts.iter().any(|part| match part {
            ContentPart::Image(image) => {
                image.kind == MentionKind::RegionImage && image.region.as_deref() == Some(reference)
            }
            ContentPart::Text(_) | ContentPart::Context(_) => false,
        })
    }

    pub fn attach_context(&mut self, context: ContextBlock) {
        self.parts.push(ContentPart::Context(context));
    }

    pub fn attach_image(&mut self, image: ImageContext) {
        self.parts.push(ContentPart::Image(image));
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
        }
    }

    pub fn is_display_empty(&self) -> bool {
        self.display_text().trim().is_empty()
    }

    pub fn push_text(&mut self, text: &str) {
        if let Some(existing) = self.parts.iter_mut().find_map(|part| match part {
            ContentPart::Text(existing) => Some(existing),
            ContentPart::Context(_) | ContentPart::Image(_) => None,
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
            ContentPart::Context(_) | ContentPart::Image(_) => None,
        })
        .collect::<String>();
    (request_prompt != display_content).then_some(request_prompt)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MentionKind {
    Page,
    PageImage,
    Selection,
    DocumentText,
    DocumentImage,
    RegionImage,
}

impl MentionKind {
    pub const ALL: [Self; 6] = [
        Self::Page,
        Self::PageImage,
        Self::Selection,
        Self::DocumentText,
        Self::DocumentImage,
        Self::RegionImage,
    ];

    pub fn token(self) -> &'static str {
        match self {
            Self::Page => "@page",
            Self::PageImage => "@page-image",
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
            Self::PageImage | Self::DocumentImage | Self::RegionImage
        )
    }

    pub fn requires_fixed_layout(self) -> bool {
        matches!(
            self,
            Self::Page | Self::PageImage | Self::DocumentImage | Self::RegionImage
        )
    }

    pub fn is_page_reference(self) -> bool {
        matches!(self, Self::Page | Self::PageImage | Self::DocumentImage)
    }

    pub fn context_tag(self) -> &'static str {
        match self {
            Self::Page => "page",
            Self::PageImage => "page_image",
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

#[derive(Clone, Debug, Default)]
pub struct DocumentContext {
    pub file_name: Option<String>,
    pub format: Option<String>,
    pub location_label: Option<String>,
    pub page_count: usize,
    pub current_page: usize,
    pub has_selection: bool,
}

impl DocumentContext {
    fn has_document(&self) -> bool {
        self.file_name.is_some() || self.page_count > 0
    }
}

pub enum AiStreamEvent {
    Delta(String),
    Error(String),
    Done,
}

struct RequestMessage {
    role: Role,
    parts: Vec<RequestPart>,
}

enum RequestPart {
    Text(String),
    Image(ImageContext),
}

pub fn stream_completion(
    settings: ResolvedAiConfig,
    messages: Vec<ChatMessage>,
    document: DocumentContext,
    send_event: &mut impl FnMut(AiStreamEvent) -> Result<()>,
) -> Result<()> {
    if !settings.can_request() {
        return Err(anyhow!(settings.missing_credentials_message()));
    }

    let client = reqwest::blocking::Client::builder()
        .build()
        .context("failed to create HTTP client")?;
    let native_thinking = match settings.provider.api_style() {
        AiApiStyle::OpenAi => {
            supports_openai_reasoning(settings.provider, &settings.model, settings.thinking_level)
        }
        AiApiStyle::Anthropic => {
            supports_anthropic_thinking(settings.provider, &settings.model, settings.thinking_level)
        }
    };
    let system = system_prompt(&document, settings.thinking_level, native_thinking);
    let request_messages = build_request_messages(messages);

    match settings.provider.api_style() {
        AiApiStyle::OpenAi => {
            stream_openai(&client, settings, system, request_messages, send_event)
        }
        AiApiStyle::Anthropic => {
            stream_anthropic(&client, settings, system, request_messages, send_event)
        }
    }
}

fn build_request_messages(messages: Vec<ChatMessage>) -> Vec<RequestMessage> {
    messages
        .into_iter()
        .map(|message| {
            let mut parts = vec![RequestPart::Text(message.request_text())];
            parts.extend(message.parts.into_iter().filter_map(|part| match part {
                ContentPart::Image(image) => Some(RequestPart::Image(image)),
                ContentPart::Text(_) | ContentPart::Context(_) => None,
            }));
            RequestMessage {
                role: message.role,
                parts,
            }
        })
        .collect()
}

fn openai_message_content(parts: &[RequestPart]) -> Value {
    if let [RequestPart::Text(text)] = parts {
        return json!(text);
    }

    Value::Array(
        parts
            .iter()
            .filter_map(|part| match part {
                RequestPart::Text(text) if text.is_empty() => None,
                RequestPart::Text(text) => Some(json!({
                    "type": "text",
                    "text": text,
                })),
                RequestPart::Image(image) => Some(json!({
                    "type": "image_url",
                    "image_url": {
                        "url": format!(
                            "data:{};base64,{}",
                            image.media_type,
                            BASE64_STANDARD.encode(image.bytes.as_ref())
                        ),
                    },
                })),
            })
            .collect(),
    )
}

fn anthropic_message_content(parts: &[RequestPart]) -> Value {
    if let [RequestPart::Text(text)] = parts {
        return json!(text);
    }

    Value::Array(
        parts
            .iter()
            .filter_map(|part| match part {
                RequestPart::Text(text) if text.is_empty() => None,
                RequestPart::Text(text) => Some(json!({
                    "type": "text",
                    "text": text,
                })),
                RequestPart::Image(image) => Some(json!({
                    "type": "image",
                    "source": {
                        "type": "base64",
                        "media_type": image.media_type.as_str(),
                        "data": BASE64_STANDARD.encode(image.bytes.as_ref()),
                    },
                })),
            })
            .collect(),
    )
}

fn stream_openai(
    client: &reqwest::blocking::Client,
    settings: ResolvedAiConfig,
    system: String,
    messages: Vec<RequestMessage>,
    send_event: &mut impl FnMut(AiStreamEvent) -> Result<()>,
) -> Result<()> {
    let url = openai_chat_url(&settings.base_url);
    let mut request_messages = Vec::new();
    request_messages.push(json!({
        "role": "system",
        "content": system,
    }));
    for message in messages {
        request_messages.push(json!({
            "role": message.role.api_role(),
            "content": openai_message_content(&message.parts),
        }));
    }

    let mut body = json!({
        "model": settings.model.clone(),
        "messages": request_messages,
        "stream": true,
    });
    apply_openai_thinking(
        &mut body,
        settings.provider,
        &settings.model,
        settings.thinking_level,
    );

    let mut request = client.post(url).json(&body);
    if !settings.api_key.is_empty() {
        request = request.bearer_auth(settings.api_key);
    }

    let response = request
        .send()
        .with_context(|| format!("failed to send {} request", settings.provider.label()))?;
    read_sse_response(response, AiApiStyle::OpenAi, send_event)
}

fn stream_anthropic(
    client: &reqwest::blocking::Client,
    settings: ResolvedAiConfig,
    system: String,
    messages: Vec<RequestMessage>,
    send_event: &mut impl FnMut(AiStreamEvent) -> Result<()>,
) -> Result<()> {
    let base_url = settings.base_url.trim_end_matches('/');
    let url = if base_url.ends_with("/v1/messages") {
        base_url.to_string()
    } else if base_url.ends_with("/v1") {
        format!("{base_url}/messages")
    } else {
        format!("{base_url}/v1/messages")
    };

    let request_messages = messages
        .into_iter()
        .map(|message| {
            json!({
                "role": message.role.api_role(),
                "content": anthropic_message_content(&message.parts),
            })
        })
        .collect::<Vec<_>>();

    let mut body = json!({
        "model": settings.model.clone(),
        "messages": request_messages,
        "max_tokens": 4096,
        "stream": true,
        "system": system,
    });
    apply_anthropic_thinking(
        &mut body,
        settings.provider,
        &settings.model,
        settings.thinking_level,
    );

    let response = client
        .post(url)
        .header("x-api-key", settings.api_key)
        .header("anthropic-version", "2023-06-01")
        .json(&body)
        .send()
        .context("failed to send Anthropic request")?;
    read_sse_response(response, AiApiStyle::Anthropic, send_event)
}

fn read_sse_response(
    mut response: reqwest::blocking::Response,
    api_style: AiApiStyle,
    send_event: &mut impl FnMut(AiStreamEvent) -> Result<()>,
) -> Result<()> {
    if !response.status().is_success() {
        let status = response.status();
        let mut body = String::new();
        if let Err(error) = response.read_to_string(&mut body) {
            body = format!("failed to read error body: {error:#}");
        }
        return Err(anyhow!("AI request failed with status {status}: {body}"));
    }

    let mut pending = Vec::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let read = response
            .read(&mut buffer)
            .context("failed to read AI stream")?;
        if read == 0 {
            break;
        }
        pending.extend_from_slice(&buffer[..read]);

        while let Some(newline_ix) = pending.iter().position(|byte| *byte == b'\n') {
            let line = pending.drain(..=newline_ix).collect::<Vec<_>>();
            let line = std::str::from_utf8(&line)
                .context("AI stream emitted invalid UTF-8")?
                .trim();
            if process_sse_line(api_style, line, send_event)? {
                return Ok(());
            }
        }
    }

    if !pending.is_empty() {
        let line = std::str::from_utf8(&pending)
            .context("AI stream emitted invalid UTF-8")?
            .trim();
        process_sse_line(api_style, line, send_event)?;
    }

    Ok(())
}

fn process_sse_line(
    api_style: AiApiStyle,
    line: &str,
    send_event: &mut impl FnMut(AiStreamEvent) -> Result<()>,
) -> Result<bool> {
    let Some(data) = line.strip_prefix("data:") else {
        return Ok(false);
    };
    let data = data.trim();
    if data == "[DONE]" {
        return Ok(true);
    }

    let value = serde_json::from_str::<Value>(data).context("failed to parse AI stream event")?;
    match api_style {
        AiApiStyle::OpenAi => {
            if let Some(delta) = value["choices"][0]["delta"]["content"].as_str()
                && !delta.is_empty()
            {
                send_event(AiStreamEvent::Delta(delta.to_string()))?;
            }
            Ok(false)
        }
        AiApiStyle::Anthropic => {
            match value["type"].as_str() {
                Some("content_block_delta") => {
                    if let Some(delta) = value["delta"]["text"].as_str()
                        && !delta.is_empty()
                    {
                        send_event(AiStreamEvent::Delta(delta.to_string()))?;
                    }
                }
                Some("message_stop") => return Ok(true),
                _ => {}
            }
            Ok(false)
        }
    }
}

fn openai_chat_url(base_url: &str) -> String {
    let base_url = base_url.trim_end_matches('/');
    if base_url.ends_with("/v1/chat/completions") {
        base_url.to_string()
    } else if openai_base_url_has_version(base_url) {
        format!("{base_url}/chat/completions")
    } else {
        format!("{base_url}/v1/chat/completions")
    }
}

fn openai_base_url_has_version(base_url: &str) -> bool {
    base_url.rsplit('/').next().is_some_and(|segment| {
        segment.len() > 1
            && segment.starts_with('v')
            && segment[1..].chars().all(|ch| ch.is_ascii_digit())
    })
}

pub fn system_prompt(
    document: &DocumentContext,
    thinking_level: AiThinkingLevel,
    native_thinking: bool,
) -> String {
    let mut sections = vec![
        "You are an AI assistant embedded in a document reader. Help the user understand, analyze, and work with the current document. Be conversational but professional. Refer to the user in the second person and yourself in the first person. Format responses in Markdown. Never invent document details; if needed context is missing, say what context you need. Do not reveal hidden chain-of-thought; provide concise reasoning summaries only when useful.".to_string(),
    ];

    if document.has_document() {
        let mut details = Vec::new();
        if let Some(file_name) = document
            .file_name
            .as_deref()
            .filter(|name| !name.is_empty())
        {
            details.push(format!("- File: {file_name}"));
        }
        if let Some(format) = document
            .format
            .as_deref()
            .filter(|format| !format.is_empty())
        {
            details.push(format!("- Format: {format}"));
        }
        if document.page_count > 0 {
            details.push(format!("- Pages: {}", document.page_count));
        }
        if document.current_page > 0 {
            details.push(format!("- Current page: {}", document.current_page));
        }
        if let Some(location) = document
            .location_label
            .as_deref()
            .filter(|location| !location.is_empty())
        {
            details.push(format!("- Location: {location}"));
        }
        details.push(format!(
            "- Selected text: {}",
            if document.has_selection {
                "available"
            } else {
                "not available"
            }
        ));
        sections.push(format!("Current document:\n{}", details.join("\n")));
    }

    sections.push("When the user attaches @page(<page number>), @page-image(<page number>), @selection, @document, @document-image, or @region-image, ground your answer in the attached context. Page numbers are 1-based. Treat attached document text and images as current and authoritative. Cite page numbers when the attached context includes them. If the answer cannot be determined from the attached context, say so directly.".to_string());

    if thinking_level != AiThinkingLevel::Off && !native_thinking {
        sections.push(format!(
            "Use {} internal reasoning effort before answering.",
            thinking_level.as_str()
        ));
    }

    sections.join("\n\n")
}

pub fn thinking_level_description(level: AiThinkingLevel) -> String {
    match level {
        AiThinkingLevel::Off => glyphst_settings::localize::tr("thinking-level-off-description"),
        AiThinkingLevel::Minimal => {
            glyphst_settings::localize::tr("thinking-level-minimal-description")
        }
        AiThinkingLevel::Low => glyphst_settings::localize::tr("thinking-level-low-description"),
        AiThinkingLevel::Medium => {
            glyphst_settings::localize::tr("thinking-level-medium-description")
        }
        AiThinkingLevel::High => glyphst_settings::localize::tr("thinking-level-high-description"),
        AiThinkingLevel::XHigh => {
            glyphst_settings::localize::tr("thinking-level-x-high-description")
        }
        AiThinkingLevel::Max => glyphst_settings::localize::tr("thinking-level-max-description"),
    }
}

fn supports_openai_reasoning(provider: AiProvider, model: &str, level: AiThinkingLevel) -> bool {
    provider.api_style() == AiApiStyle::OpenAi && level.supports_native_for(provider, model)
}

fn supports_anthropic_thinking(provider: AiProvider, model: &str, level: AiThinkingLevel) -> bool {
    provider.api_style() == AiApiStyle::Anthropic && level.supports_native_for(provider, model)
}

fn apply_openai_thinking(
    body: &mut Value,
    provider: AiProvider,
    model: &str,
    level: AiThinkingLevel,
) {
    if !supports_openai_reasoning(provider, model, level) {
        return;
    }

    let level = level.clamp_for(provider, model);
    body["reasoning_effort"] = json!(match level {
        AiThinkingLevel::Off => return,
        AiThinkingLevel::Minimal => "minimal",
        AiThinkingLevel::Low => "low",
        AiThinkingLevel::Medium => "medium",
        AiThinkingLevel::High => "high",
        AiThinkingLevel::XHigh | AiThinkingLevel::Max => "xhigh",
    });
}

fn apply_anthropic_thinking(
    body: &mut Value,
    provider: AiProvider,
    model: &str,
    level: AiThinkingLevel,
) {
    if !supports_anthropic_thinking(provider, model, level) {
        return;
    }

    let level = level.clamp_for(provider, model);
    body["thinking"] = json!({
        "type": "enabled",
        "budget_tokens": match level {
            AiThinkingLevel::Off => return,
            AiThinkingLevel::Minimal => 1024,
            AiThinkingLevel::Low => 1024,
            AiThinkingLevel::Medium => 2048,
            AiThinkingLevel::High => 3072,
            AiThinkingLevel::XHigh | AiThinkingLevel::Max => 4096,
        },
    });
    body["output_config"] = json!({
        "effort": match level {
            AiThinkingLevel::Off => return,
            AiThinkingLevel::Minimal | AiThinkingLevel::Low => "low",
            AiThinkingLevel::Medium => "medium",
            AiThinkingLevel::High => "high",
            AiThinkingLevel::XHigh | AiThinkingLevel::Max => "max",
        },
    });
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
