use std::{collections::HashSet, io::Read as _, ops::Range};

use anyhow::{Context as _, Result, anyhow};
use serde_json::{Value, json};

use crate::settings::{AiApiStyle, AiProvider, AiThinkingLevel, ResolvedAiConfig};

const MAX_CONTEXT_CHARS: usize = 12_000;

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
pub struct ChatMessage {
    pub role: Role,
    pub parts: Vec<ContentPart>,
    request_content: Option<String>,
}

impl ChatMessage {
    pub fn user(content: String, contexts: Vec<ContextBlock>) -> Self {
        let mut parts = vec![ContentPart::Text(content)];
        parts.extend(contexts.into_iter().map(ContentPart::Context));
        Self {
            role: Role::User,
            parts,
            request_content: None,
        }
    }

    pub fn user_text(content: String) -> Self {
        Self::user(content, Vec::new())
    }

    pub fn assistant(content: String) -> Self {
        Self {
            role: Role::Assistant,
            parts: vec![ContentPart::Text(content)],
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
        if let Some(request_content) = &self.request_content {
            return request_content.clone();
        }

        let mut text = String::new();
        let mut contexts = Vec::new();
        for part in &self.parts {
            match part {
                ContentPart::Text(part_text) => text.push_str(part_text),
                ContentPart::Context(context) => contexts.push(context),
            }
        }

        if let Some(context) = context_envelope(&contexts) {
            if !text.trim().is_empty() {
                text.push_str("\n\n");
            }
            text.push_str(&context);
        }

        text
    }

    pub fn is_display_empty(&self) -> bool {
        self.display_text().trim().is_empty()
    }

    pub fn push_text(&mut self, text: &str) {
        if let Some(existing) = self.parts.iter_mut().find_map(|part| match part {
            ContentPart::Text(existing) => Some(existing),
            ContentPart::Context(_) => None,
        }) {
            existing.push_str(text);
        } else {
            self.parts.push(ContentPart::Text(text.to_string()));
        }
        self.request_content = None;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MentionKind {
    CurrentPageText,
    Selection,
    DocumentText,
}

impl MentionKind {
    pub const ALL: [Self; 3] = [Self::CurrentPageText, Self::Selection, Self::DocumentText];

    pub fn token(self) -> &'static str {
        match self {
            Self::CurrentPageText => "@current page text",
            Self::Selection => "@selection",
            Self::DocumentText => "@document text",
        }
    }

    pub fn label(self) -> String {
        match self {
            Self::CurrentPageText => glyphst_settings::localize::tr("mention-current-page-text"),
            Self::Selection => glyphst_settings::localize::tr("mention-selection"),
            Self::DocumentText => glyphst_settings::localize::tr("mention-document-text"),
        }
    }

    pub fn description(self) -> String {
        match self {
            Self::CurrentPageText => {
                glyphst_settings::localize::tr("mention-current-page-text-description")
            }
            Self::Selection => glyphst_settings::localize::tr("mention-selection-description"),
            Self::DocumentText => {
                glyphst_settings::localize::tr("mention-document-text-description")
            }
        }
    }

    pub fn context_title(self) -> String {
        match self {
            Self::CurrentPageText => glyphst_settings::localize::tr("mention-current-page-text"),
            Self::Selection => glyphst_settings::localize::tr("mention-selection"),
            Self::DocumentText => glyphst_settings::localize::tr("mention-document-text"),
        }
    }

    fn tag_name(self) -> &'static str {
        match self {
            Self::CurrentPageText => "current_page_text",
            Self::Selection => "selection",
            Self::DocumentText => "document_text",
        }
    }

    fn empty_context_message(self) -> String {
        match self {
            Self::CurrentPageText => glyphst_settings::localize::tr("ai-no-current-page-text"),
            Self::Selection => glyphst_settings::localize::tr("ai-no-selected-text"),
            Self::DocumentText => glyphst_settings::localize::tr("ai-no-document-text"),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct DocumentContext {
    pub file_name: Option<String>,
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
    content: String,
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
        .map(|message| RequestMessage {
            role: message.role,
            content: message.request_text(),
        })
        .collect()
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
            "content": message.content,
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
                "content": message.content,
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
        "You are an AI assistant embedded in a PDF viewer. Help the user understand, analyze, and work with the current document. Be conversational but professional. Refer to the user in the second person and yourself in the first person. Format responses in Markdown. Never invent document details; if the needed PDF context is missing, say what context you need. Do not reveal hidden chain-of-thought; provide concise reasoning summaries only when useful.".to_string(),
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
        if document.page_count > 0 {
            details.push(format!("- Pages: {}", document.page_count));
        }
        if document.current_page > 0 {
            details.push(format!("- Current page: {}", document.current_page));
        }
        details.push(format!(
            "- Selected text: {}",
            if document.has_selection {
                "available"
            } else {
                "not available"
            }
        ));
        sections.push(format!("Current PDF document:\n{}", details.join("\n")));
    }

    sections.push("When the user includes @selection, @current page text, or @document text, ground your answer in the attached <context> block. Treat attached PDF context as current and authoritative. Cite page numbers when the attached context includes them. If the answer cannot be determined from the attached context, say so directly.".to_string());

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
    for (_, mention) in find_mention_ranges(text) {
        if seen.insert(mention) {
            mentions.push(mention);
        }
    }
    mentions
}

pub fn find_mention_ranges(text: &str) -> Vec<(Range<usize>, MentionKind)> {
    let mut ranges = Vec::new();
    for (start, ch) in text.char_indices() {
        if ch != '@' {
            continue;
        }
        for mention in MentionKind::ALL {
            let token = mention.token();
            if text[start..].starts_with(token) {
                let end = start + token.len();
                if is_mention_boundary(text[end..].chars().next()) {
                    ranges.push((start..end, mention));
                    break;
                }
            }
        }
    }
    ranges
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

    let mut request = "Attached PDF context from @ mentions:\n<context>\nThe following PDF context was attached by the user. It is current; do not ask to re-read it.\n".to_string();
    for context in contexts {
        let tag = context.kind.tag_name();
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
    let mut escaped = text.to_string();
    for tag in ["context", "selection", "current_page_text", "document_text"] {
        escaped = escaped.replace(&format!("</{tag}"), &format!("&lt;/{tag}"));
        escaped = escaped.replace(&format!("<{tag}"), &format!("&lt;{tag}"));
    }
    escaped
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
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_pdf_context_mentions_in_order_without_duplicates() {
        assert_eq!(
            mentioned_kinds(
                "Summarize @selection and compare with @current page text plus @document text"
            ),
            vec![
                MentionKind::Selection,
                MentionKind::CurrentPageText,
                MentionKind::DocumentText,
            ]
        );
        assert_eq!(
            mentioned_kinds("@selection then @selection again"),
            vec![MentionKind::Selection]
        );
    }

    #[test]
    fn mention_completion_keeps_current_page_space_query_open() {
        assert!(mention_matches_query(
            MentionKind::CurrentPageText,
            "current "
        ));
        assert!(mention_matches_query(MentionKind::CurrentPageText, "page"));
        assert!(!mention_matches_query(
            MentionKind::CurrentPageText,
            "current page text "
        ));
    }

    #[test]
    fn derives_session_title_from_first_prompt_line() {
        assert_eq!(
            session_title_for_prompt("\n  Explain the introduction in detail"),
            "Explain the introduction in detail"
        );
        assert_eq!(session_title_for_prompt(""), DEFAULT_SESSION_TITLE);
    }

    #[test]
    fn extracts_generated_title_from_first_response_line() {
        assert_eq!(
            title_from_response("  `Fourier Transform Basics`\n\nOther text"),
            Some("Fourier Transform Basics".to_string())
        );
        assert_eq!(title_from_response("   \n"), None);
    }

    #[test]
    fn configures_native_thinking_for_supported_models() {
        let mut openai_body = json!({});
        apply_openai_thinking(
            &mut openai_body,
            AiProvider::OpenAi,
            "gpt-5",
            AiThinkingLevel::XHigh,
        );
        assert_eq!(openai_body["reasoning_effort"], "xhigh");

        let mut anthropic_body = json!({});
        apply_anthropic_thinking(
            &mut anthropic_body,
            AiProvider::Anthropic,
            "claude-sonnet-4-5-latest",
            AiThinkingLevel::Max,
        );
        assert_eq!(anthropic_body["thinking"]["budget_tokens"], 4096);
        assert_eq!(anthropic_body["output_config"]["effort"], "max");

        let mut unsupported_body = json!({});
        apply_openai_thinking(
            &mut unsupported_body,
            AiProvider::OpenRouter,
            "openai/gpt-5",
            AiThinkingLevel::High,
        );
        assert_eq!(unsupported_body["reasoning_effort"], "high");

        let mut unsupported_body = json!({});
        apply_openai_thinking(
            &mut unsupported_body,
            AiProvider::OpenRouter,
            "openai/gpt-4o",
            AiThinkingLevel::High,
        );
        assert!(unsupported_body.get("reasoning_effort").is_none());
    }

    #[test]
    fn unsupported_thinking_uses_system_prompt_instruction() {
        let prompt = system_prompt(&DocumentContext::default(), AiThinkingLevel::Low, false);
        assert!(prompt.contains("Use low internal reasoning effort"));
        assert!(prompt.contains("Do not reveal hidden chain-of-thought"));

        let native_prompt = system_prompt(&DocumentContext::default(), AiThinkingLevel::Low, true);
        assert!(!native_prompt.contains("Use low internal reasoning effort"));
    }

    #[test]
    fn openai_compatible_reasoning_avoids_system_prompt_fallback() {
        let native_thinking = supports_openai_reasoning(
            AiProvider::OpenRouter,
            "openai/gpt-5.5",
            AiThinkingLevel::Medium,
        );
        let prompt = system_prompt(
            &DocumentContext::default(),
            AiThinkingLevel::Medium,
            native_thinking,
        );

        assert!(native_thinking);
        assert!(!prompt.contains("internal reasoning effort"));
    }

    #[test]
    fn request_projection_emits_single_context_envelope() {
        let message = ChatMessage::user(
            "Explain @selection".to_string(),
            vec![ContextBlock::new(
                MentionKind::Selection,
                Some(7),
                "A closing </context> tag from PDF text".to_string(),
            )],
        );

        assert_eq!(message.display_text(), "Explain @selection");
        let request = message.request_text();
        assert_eq!(request.matches("<context>").count(), 1);
        assert!(request.contains("<selection page=\"7\">"));
        assert!(request.contains("&lt;/context> tag from PDF text"));
    }

    #[test]
    fn request_projection_omits_context_envelope_without_context() {
        let message = ChatMessage::user_text("Explain this".to_string());

        assert_eq!(message.display_text(), "Explain this");
        assert_eq!(message.request_text(), "Explain this");
    }

    #[test]
    fn document_aware_system_prompt_includes_document_details() {
        let document = DocumentContext {
            file_name: Some("paper.pdf".to_string()),
            page_count: 42,
            current_page: 9,
            has_selection: true,
        };
        let prompt = system_prompt(&document, AiThinkingLevel::Off, false);

        assert!(prompt.contains("paper.pdf"));
        assert!(prompt.contains("Pages: 42"));
        assert!(prompt.contains("Current page: 9"));
        assert!(prompt.contains("Selected text: available"));
    }
}
