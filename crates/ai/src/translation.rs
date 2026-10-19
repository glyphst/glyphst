use std::fmt;
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::chatgpt_subscription::{
    acquire_request_access, invalidate_credentials, response_error, response_text, responses_url,
};
use crate::settings::{AiApiStyle, AiProvider, AiThinkingLevel, ResolvedAiConfig};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SemanticTranslationInput {
    pub id: String,
    pub role: String,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_before: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_after: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SemanticTranslationOutput {
    pub id: String,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TextPolicyReviewDecision {
    pub block_id: String,
    pub translate: bool,
    pub confidence: f32,
    pub reason: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AiTranslationErrorCategory {
    Authentication,
    RateLimited,
    ServiceUnavailable,
    Timeout,
    InvalidRequest,
    InvalidResponse,
    Network,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AiTranslationError {
    pub category: AiTranslationErrorCategory,
    pub message: String,
    pub status: Option<u16>,
    pub retryable: bool,
    pub retry_after: Option<Duration>,
}

impl fmt::Display for AiTranslationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.message.fmt(formatter)
    }
}

impl std::error::Error for AiTranslationError {}

pub fn translate_batch(
    settings: ResolvedAiConfig,
    texts: &[String],
    source_language: Option<&str>,
    target_language: &str,
) -> Result<Vec<String>> {
    if !settings.can_request() {
        return Err(anyhow!(settings.missing_credentials_message()));
    }
    if texts.is_empty() {
        return Ok(Vec::new());
    }

    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .context("failed to create AI translation client")?;
    translate_batch_with_client(&client, settings, texts, source_language, target_language)
        .map_err(Into::into)
}

pub fn translate_batch_with_client(
    client: &reqwest::blocking::Client,
    settings: ResolvedAiConfig,
    texts: &[String],
    source_language: Option<&str>,
    target_language: &str,
) -> std::result::Result<Vec<String>, AiTranslationError> {
    if !settings.can_request() {
        return Err(configuration_error(settings.missing_credentials_message()));
    }
    if texts.is_empty() {
        return Ok(Vec::new());
    }

    let system = translation_system_prompt(source_language, target_language);
    let response = match settings.api_style {
        AiApiStyle::OpenAi => request_openai(client, &settings, &system, texts),
        AiApiStyle::OpenAiResponses => request_chatgpt(client, &settings, &system, texts),
        AiApiStyle::Anthropic => request_anthropic(client, &settings, &system, texts),
    }?;
    parse_translations(&response, texts.len()).map_err(invalid_response)
}

pub fn translate_semantic_batch(
    settings: ResolvedAiConfig,
    units: &[SemanticTranslationInput],
    source_language: Option<&str>,
    target_language: &str,
) -> Result<Vec<SemanticTranslationOutput>> {
    if !settings.can_request() {
        return Err(anyhow!(settings.missing_credentials_message()));
    }
    if units.is_empty() {
        return Ok(Vec::new());
    }

    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(90))
        .build()
        .context("failed to create AI translation client")?;
    translate_semantic_batch_with_client(&client, settings, units, source_language, target_language)
        .map_err(Into::into)
}

pub fn review_pdf_translation_policy(
    settings: ResolvedAiConfig,
    page_map: Value,
) -> Result<Vec<TextPolicyReviewDecision>> {
    if !settings.can_request() {
        return Err(anyhow!(settings.missing_credentials_message()));
    }
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .context("failed to create AI policy review client")?;
    review_pdf_translation_policy_with_client(&client, settings, page_map).map_err(Into::into)
}

pub fn review_pdf_translation_policy_with_client(
    client: &reqwest::blocking::Client,
    settings: ResolvedAiConfig,
    page_map: Value,
) -> std::result::Result<Vec<TextPolicyReviewDecision>, AiTranslationError> {
    if !settings.can_request() {
        return Err(configuration_error(settings.missing_credentials_message()));
    }
    let system = "You review a structured, text-only PDF page map to decide whether ambiguous \
text blocks should be translated. You do not receive or need an image. Bounding boxes are \
normalized to the page. Never change a block whose locked field is true. Translate narrative \
prose, headings, and separate figure/table captions. Preserve author and publication metadata, \
reference and bibliography entries, page furniture, formulas, code and literal examples, table \
cells, chart labels, axis labels, legends, and text associated with protected regions. The optional \
context_before and context_after headings describe neighboring document pages. Review only unlocked \
low-confidence blocks. Return exactly one decision for every such block and no other blocks. \
When uncertain, choose translate=false. Return only valid JSON in this exact shape: \
{\"decisions\":[{\"block_id\":\"id\",\"translate\":false,\"confidence\":0.0,\"reason\":\"short \
reason\"}]}. Omit locked and already-confident blocks.";
    let response = match settings.api_style {
        AiApiStyle::OpenAi => request_openai_payload(client, &settings, system, page_map),
        AiApiStyle::OpenAiResponses => request_chatgpt_payload(client, &settings, system, page_map),
        AiApiStyle::Anthropic => request_anthropic_payload(client, &settings, system, page_map),
    }?;
    parse_policy_review_decisions(&response).map_err(invalid_response)
}

pub fn translate_semantic_batch_with_client(
    client: &reqwest::blocking::Client,
    settings: ResolvedAiConfig,
    units: &[SemanticTranslationInput],
    source_language: Option<&str>,
    target_language: &str,
) -> std::result::Result<Vec<SemanticTranslationOutput>, AiTranslationError> {
    if !settings.can_request() {
        return Err(configuration_error(settings.missing_credentials_message()));
    }
    if units.is_empty() {
        return Ok(Vec::new());
    }

    let source = source_language.unwrap_or("the detected source language");
    let system = format!(
        "You are a precise document translator. Translate every unit from {source} to \
{target_language}. Use each unit's role and neighboring context to resolve terminology and \
continuations, but translate only its text field. Preserve every token matching \
[[GLYPHST_TYPE_INDEX_CHECKSUM]] exactly, including brackets, spelling, order, and count. Preserve \
adjacency too: when two placeholders touch in the input, return them touching with no inserted \
text or whitespace. Translate every natural-language word, including words immediately beside a \
placeholder. For prompt and dialogue roles, translate natural-language bracketed headings, field \
labels, and speaker labels too. Their line boundaries are semantic and are represented by protected \
line-break placeholders, so preserve them exactly. Preserve \
equations, citations, URLs, names, reference markers, identifiers, and paragraph meaning. PDF \
line wrapping has already been normalized: do not introduce line breaks merely for visual \
wrapping, do not put each sentence on a separate line or paragraph, and preserve blank-line \
paragraph boundaries only when they exist in the input text. Return only valid JSON in this exact \
shape: \
{{\"translations\":[{{\"id\":\"unit-id\",\"text\":\"translated text\"}}]}}. Return every input ID \
exactly once and do not add IDs."
    );
    let payload = serde_json::to_value(units)
        .map_err(|error| configuration_error(format!("failed to encode request: {error}")))?;
    let response = match settings.api_style {
        AiApiStyle::OpenAi => request_openai_payload(client, &settings, &system, payload),
        AiApiStyle::OpenAiResponses => request_chatgpt_payload(client, &settings, &system, payload),
        AiApiStyle::Anthropic => request_anthropic_payload(client, &settings, &system, payload),
    }?;
    parse_semantic_translations(&response, units).map_err(invalid_response)
}

pub fn translate_semantic_single_with_client(
    client: &reqwest::blocking::Client,
    settings: ResolvedAiConfig,
    unit: &SemanticTranslationInput,
    source_language: Option<&str>,
    target_language: &str,
) -> std::result::Result<SemanticTranslationOutput, AiTranslationError> {
    if !settings.can_request() {
        return Err(configuration_error(settings.missing_credentials_message()));
    }
    let text = if unit.text.contains("[[GLYPHST_") {
        translate_around_protected_tokens(
            client,
            &settings,
            &unit.text,
            source_language,
            target_language,
        )?
    } else {
        let input = vec![unit.text.clone()];
        let mut outputs = translate_batch_with_client(
            client,
            settings,
            &input,
            source_language,
            target_language,
        )?;
        if outputs.len() != 1 {
            return Err(invalid_response(anyhow!(
                "AI translation recovery omitted the semantic unit"
            )));
        }
        outputs.remove(0)
    };
    Ok(SemanticTranslationOutput {
        id: unit.id.clone(),
        text,
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum RecoverySegment {
    Natural(String),
    Protected(String),
}

fn translate_around_protected_tokens(
    client: &reqwest::blocking::Client,
    settings: &ResolvedAiConfig,
    text: &str,
    source_language: Option<&str>,
    target_language: &str,
) -> std::result::Result<String, AiTranslationError> {
    let mut segments = split_recovery_segments(text)?;
    let translatable = segments
        .iter()
        .enumerate()
        .filter_map(|(index, segment)| match segment {
            RecoverySegment::Natural(text) if text.chars().any(char::is_alphabetic) => {
                Some((index, text.clone()))
            }
            RecoverySegment::Natural(_) | RecoverySegment::Protected(_) => None,
        })
        .collect::<Vec<_>>();

    let mut translated = Vec::with_capacity(translatable.len());
    let mut cursor = 0;
    while cursor < translatable.len() {
        let mut end = cursor;
        let mut characters = 0usize;
        while end < translatable.len()
            && end - cursor < 24
            && (end == cursor || characters + translatable[end].1.chars().count() <= 4_000)
        {
            characters += translatable[end].1.chars().count();
            end += 1;
        }
        let texts = translatable[cursor..end]
            .iter()
            .map(|(_, text)| text.clone())
            .collect::<Vec<_>>();
        translated.extend(translate_recovery_fragments(
            client,
            settings,
            &texts,
            source_language,
            target_language,
        )?);
        cursor = end;
    }

    for ((segment_index, source), translated) in translatable.into_iter().zip(translated) {
        segments[segment_index] =
            RecoverySegment::Natural(restore_edge_whitespace(&source, translated.trim()));
    }
    Ok(segments
        .into_iter()
        .map(|segment| match segment {
            RecoverySegment::Natural(text) | RecoverySegment::Protected(text) => text,
        })
        .collect())
}

fn translate_recovery_fragments(
    client: &reqwest::blocking::Client,
    settings: &ResolvedAiConfig,
    texts: &[String],
    source_language: Option<&str>,
    target_language: &str,
) -> std::result::Result<Vec<String>, AiTranslationError> {
    match translate_batch_with_client(
        client,
        settings.clone(),
        texts,
        source_language,
        target_language,
    ) {
        Ok(translated) => Ok(translated),
        Err(error)
            if error.category == AiTranslationErrorCategory::InvalidResponse && texts.len() > 1 =>
        {
            let middle = texts.len() / 2;
            let mut translated = translate_recovery_fragments(
                client,
                settings,
                &texts[..middle],
                source_language,
                target_language,
            )?;
            translated.extend(translate_recovery_fragments(
                client,
                settings,
                &texts[middle..],
                source_language,
                target_language,
            )?);
            Ok(translated)
        }
        Err(error) => Err(error),
    }
}

fn split_recovery_segments(
    text: &str,
) -> std::result::Result<Vec<RecoverySegment>, AiTranslationError> {
    let mut segments = Vec::new();
    let mut remaining = text;
    while let Some(start) = remaining.find("[[GLYPHST_") {
        if start > 0 {
            segments.push(RecoverySegment::Natural(remaining[..start].to_string()));
        }
        remaining = &remaining[start..];
        let Some(end) = remaining.find("]]") else {
            return Err(configuration_error(
                "semantic translation recovery received an incomplete protected token".to_string(),
            ));
        };
        let end = end + 2;
        segments.push(RecoverySegment::Protected(remaining[..end].to_string()));
        remaining = &remaining[end..];
    }
    if !remaining.is_empty() {
        segments.push(RecoverySegment::Natural(remaining.to_string()));
    }
    Ok(segments)
}

fn restore_edge_whitespace(source: &str, translated: &str) -> String {
    let leading = source.len() - source.trim_start_matches(char::is_whitespace).len();
    let trailing_start = source
        .trim_end_matches(char::is_whitespace)
        .len()
        .max(leading);
    format!(
        "{}{}{}",
        &source[..leading],
        translated,
        &source[trailing_start..]
    )
}

fn translation_system_prompt(source_language: Option<&str>, target_language: &str) -> String {
    let source = source_language.unwrap_or("the detected source language");
    format!(
        "You are a precise document translator. Translate every input from {source} to \
{target_language}. Preserve equations, citations, reference markers, URLs, names, and paragraph \
meaning. Do not add visual line wrapping or turn individual sentences into separate paragraphs; \
preserve blank-line paragraph boundaries only when present in the input. Return only valid JSON \
in this exact shape: {{\"translations\":[\"...\"]}}. The translations array must have the same \
length and order as the input texts."
    )
}

fn request_openai(
    client: &reqwest::blocking::Client,
    settings: &ResolvedAiConfig,
    system: &str,
    texts: &[String],
) -> std::result::Result<String, AiTranslationError> {
    let payload = serde_json::to_value(texts)
        .map_err(|error| configuration_error(format!("failed to encode request: {error}")))?;
    request_openai_payload(client, settings, system, payload)
}

fn request_openai_payload(
    client: &reqwest::blocking::Client,
    settings: &ResolvedAiConfig,
    system: &str,
    payload: Value,
) -> std::result::Result<String, AiTranslationError> {
    match request_openai_payload_inner(client, settings, system, &payload, true) {
        Err(error)
            if error.category == AiTranslationErrorCategory::InvalidRequest
                && matches!(error.status, Some(400 | 404 | 422)) =>
        {
            request_openai_payload_inner(client, settings, system, &payload, false)
        }
        result => result,
    }
}

fn request_openai_payload_inner(
    client: &reqwest::blocking::Client,
    settings: &ResolvedAiConfig,
    system: &str,
    payload: &Value,
    structured_output: bool,
) -> std::result::Result<String, AiTranslationError> {
    let url = openai_chat_url(&settings.base_url);
    let mut body = json!({
        "model": settings.model,
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": serde_json::to_string(payload).map_err(|error|
                configuration_error(format!("failed to encode request: {error}"))) ?}
        ],
        "temperature": 0,
    });
    if structured_output {
        body["response_format"] = json!({"type": "json_object"});
    }
    apply_translation_thinking_mode(&mut body, settings);
    apply_translation_output_budget(&mut body, settings, payload);
    let mut request = client.post(url).json(&body);
    if !settings.api_key.is_empty() {
        request = request.bearer_auth(&settings.api_key);
    }

    let value = send_json(request, &settings.provider_name)?;
    if let Some(reason) = value
        .pointer("/choices/0/finish_reason")
        .and_then(Value::as_str)
        .filter(|reason| *reason != "stop")
    {
        return Err(invalid_response(anyhow!(
            "{} stopped translation output with finish_reason={reason}",
            settings.provider_name
        )));
    }
    value
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| invalid_response_message(&settings.provider_name))
}

fn request_chatgpt(
    client: &reqwest::blocking::Client,
    settings: &ResolvedAiConfig,
    system: &str,
    texts: &[String],
) -> std::result::Result<String, AiTranslationError> {
    let payload = serde_json::to_value(texts)
        .map_err(|error| configuration_error(format!("failed to encode request: {error}")))?;
    request_chatgpt_payload(client, settings, system, payload)
}

fn request_chatgpt_payload(
    client: &reqwest::blocking::Client,
    settings: &ResolvedAiConfig,
    system: &str,
    payload: Value,
) -> std::result::Result<String, AiTranslationError> {
    let content = serde_json::to_string(&payload)
        .map_err(|error| configuration_error(format!("failed to encode request: {error}")))?;
    let body = chatgpt_translation_body(settings, system, content);
    let access =
        acquire_request_access().map_err(|error| authentication_error(format!("{error:#}")))?;
    let request = access.authorize(client.post(responses_url(&settings.base_url)).json(&body));
    let text = send_chatgpt_sse(request, &settings.provider_name).inspect_err(|error| {
        if matches!(error.status, Some(401 | 403)) {
            invalidate_credentials("Your ChatGPT session is invalid. Sign in again.");
        }
    })?;
    if text.trim().is_empty() {
        Err(invalid_response_message(&settings.provider_name))
    } else {
        Ok(text)
    }
}

fn chatgpt_translation_body(settings: &ResolvedAiConfig, system: &str, content: String) -> Value {
    json!({
        "model": settings.model,
        "instructions": system,
        "input": [{
            "role": "user",
            "content": [{
                "type": "input_text",
                "text": content,
            }],
        }],
        "stream": true,
        "store": false,
    })
}

fn send_chatgpt_sse(
    request: reqwest::blocking::RequestBuilder,
    provider: &str,
) -> std::result::Result<String, AiTranslationError> {
    let response = request
        .send()
        .map_err(|error| request_error(provider, error))?;
    let status = response.status();
    let retry_after = response
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(retry_after_duration);
    let body = response.text().map_err(|_| AiTranslationError {
        category: AiTranslationErrorCategory::Network,
        message: format!("failed to read {provider} translation response"),
        status: Some(status.as_u16()),
        retryable: true,
        retry_after: None,
    })?;
    if !status.is_success() {
        return Err(http_status_error(provider, status, retry_after, &body));
    }
    parse_chatgpt_stream_text(&body).map_err(invalid_response)
}

fn parse_chatgpt_stream_text(body: &str) -> Result<String> {
    if let Ok(value) = serde_json::from_str::<Value>(body.trim()) {
        return response_text(&value).ok_or_else(|| {
            anyhow!(response_error(&value).unwrap_or_else(|| {
                "ChatGPT Subscription response did not include text content".to_string()
            }))
        });
    }

    let mut text = String::new();
    let mut completed_text = None;
    for line in body.lines() {
        let Some(data) = line.trim().strip_prefix("data:") else {
            continue;
        };
        let data = data.trim();
        if data.is_empty() || data == "[DONE]" {
            continue;
        }
        let value = serde_json::from_str::<Value>(data)
            .context("failed to parse ChatGPT Subscription stream event")?;
        match value.get("type").and_then(Value::as_str) {
            Some("response.output_text.delta") => {
                if let Some(delta) = value.get("delta").and_then(Value::as_str) {
                    text.push_str(delta);
                }
            }
            Some("response.output_text.done") => {
                if let Some(done) = value.get("text").and_then(Value::as_str) {
                    completed_text = Some(done.to_string());
                }
            }
            Some("response.completed") => {
                if text.is_empty()
                    && let Some(response) = value.get("response")
                {
                    completed_text = response_text(response);
                }
            }
            Some("response.failed") | Some("response.incomplete") | Some("error") => {
                return Err(anyhow!(response_error(&value).unwrap_or_else(|| {
                    "ChatGPT Subscription request failed".to_string()
                })));
            }
            _ => {}
        }
    }
    if text.is_empty()
        && let Some(completed_text) = completed_text
    {
        text = completed_text;
    }
    (!text.trim().is_empty())
        .then_some(text)
        .ok_or_else(|| anyhow!("ChatGPT Subscription response did not include text content"))
}

fn apply_translation_thinking_mode(body: &mut Value, settings: &ResolvedAiConfig) {
    if settings.provider == AiProvider::DeepSeek && settings.thinking_level == AiThinkingLevel::Off
    {
        body["thinking"] = json!({"type": "disabled"});
    }
}

fn apply_translation_output_budget(body: &mut Value, settings: &ResolvedAiConfig, payload: &Value) {
    if settings.provider != AiProvider::DeepSeek {
        return;
    }
    let characters = payload.to_string().chars().count() as u64;
    let provider_limit = if settings.model.starts_with("deepseek-v4-") {
        64_000
    } else {
        8_192
    };
    body["max_tokens"] = json!(characters.saturating_mul(2).clamp(4_096, provider_limit));
}

fn request_anthropic(
    client: &reqwest::blocking::Client,
    settings: &ResolvedAiConfig,
    system: &str,
    texts: &[String],
) -> std::result::Result<String, AiTranslationError> {
    let payload = serde_json::to_value(texts)
        .map_err(|error| configuration_error(format!("failed to encode request: {error}")))?;
    request_anthropic_payload(client, settings, system, payload)
}

fn request_anthropic_payload(
    client: &reqwest::blocking::Client,
    settings: &ResolvedAiConfig,
    system: &str,
    payload: Value,
) -> std::result::Result<String, AiTranslationError> {
    let url = anthropic_messages_url(&settings.base_url);
    let max_tokens = anthropic_max_tokens(&payload);
    let mut response = client
        .post(url)
        .header("anthropic-version", "2023-06-01")
        .json(&json!({
            "model": settings.model,
            "system": system,
            "messages": [{
                "role": "user",
                "content": serde_json::to_string(&payload).map_err(|error|
                    configuration_error(format!("failed to encode request: {error}"))) ?,
            }],
            "max_tokens": max_tokens,
            "temperature": 0,
        }));
    if !settings.api_key.is_empty() {
        response = response.header("x-api-key", &settings.api_key);
    }

    let value = send_json(response, &settings.provider_name)?;
    value
        .pointer("/content/0/text")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| invalid_response_message(&settings.provider_name))
}

fn parse_semantic_translations(
    content: &str,
    expected: &[SemanticTranslationInput],
) -> Result<Vec<SemanticTranslationOutput>> {
    let value = parse_json_object(content)?;
    let expected_ids = expected
        .iter()
        .map(|unit| unit.id.as_str())
        .collect::<std::collections::HashSet<_>>();
    let mut translations = std::collections::HashMap::new();
    let mut duplicate_ids = std::collections::HashSet::new();
    for output in value
        .get("translations")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("AI translation response is missing translations"))?
        .iter()
        .filter_map(|value| {
            serde_json::from_value::<SemanticTranslationOutput>(value.clone())
                .ok()
                .filter(|output| expected_ids.contains(output.id.as_str()))
        })
    {
        let id = output.id.clone();
        if translations.insert(id.clone(), output).is_some() {
            duplicate_ids.insert(id);
        }
    }
    for id in duplicate_ids {
        translations.remove(&id);
    }
    Ok(translations.into_values().collect())
}

fn parse_policy_review_decisions(content: &str) -> Result<Vec<TextPolicyReviewDecision>> {
    let value = parse_json_object(content)?;
    let decisions = value
        .get("decisions")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("AI policy review response is missing decisions"))?;
    let mut by_id = std::collections::HashMap::new();
    let mut duplicates = std::collections::HashSet::new();
    for decision in decisions {
        let Ok(mut decision) = serde_json::from_value::<TextPolicyReviewDecision>(decision.clone())
        else {
            continue;
        };
        decision.block_id = decision.block_id.trim().to_string();
        decision.reason = decision.reason.trim().chars().take(240).collect();
        if decision.block_id.is_empty()
            || !decision.confidence.is_finite()
            || !(0.0..=1.0).contains(&decision.confidence)
        {
            continue;
        }
        let id = decision.block_id.clone();
        if by_id.insert(id.clone(), decision).is_some() {
            duplicates.insert(id);
        }
    }
    for id in duplicates {
        by_id.remove(&id);
    }
    Ok(by_id.into_values().collect())
}

fn send_json(
    request: reqwest::blocking::RequestBuilder,
    provider: &str,
) -> std::result::Result<Value, AiTranslationError> {
    let response = request
        .send()
        .map_err(|error| request_error(provider, error))?;
    let status = response.status();
    let retry_after = response
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(retry_after_duration);
    let body = response.text().map_err(|_| AiTranslationError {
        category: AiTranslationErrorCategory::Network,
        message: format!("failed to read {provider} translation response"),
        status: Some(status.as_u16()),
        retryable: true,
        retry_after: None,
    })?;
    if !status.is_success() {
        return Err(http_status_error(provider, status, retry_after, &body));
    }
    serde_json::from_str(&body).map_err(|_| AiTranslationError {
        category: AiTranslationErrorCategory::InvalidResponse,
        message: format!("{provider} returned an invalid JSON response"),
        status: Some(status.as_u16()),
        retryable: true,
        retry_after: None,
    })
}

fn http_status_error(
    provider: &str,
    status: StatusCode,
    retry_after: Option<Duration>,
    body: &str,
) -> AiTranslationError {
    let category = match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
            AiTranslationErrorCategory::Authentication
        }
        StatusCode::TOO_MANY_REQUESTS => AiTranslationErrorCategory::RateLimited,
        StatusCode::REQUEST_TIMEOUT => AiTranslationErrorCategory::Timeout,
        status if status.is_server_error() => AiTranslationErrorCategory::ServiceUnavailable,
        _ => AiTranslationErrorCategory::InvalidRequest,
    };
    let detail = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|value| response_error(&value))
        .map(|message| {
            message
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .chars()
                .take(300)
                .collect::<String>()
        })
        .filter(|message| !message.is_empty());
    AiTranslationError {
        category,
        message: match detail {
            Some(detail) => {
                format!("{provider} translation request failed with status {status}: {detail}")
            }
            None => format!("{provider} translation request failed with status {status}"),
        },
        status: Some(status.as_u16()),
        retryable: matches!(
            status,
            StatusCode::REQUEST_TIMEOUT | StatusCode::TOO_MANY_REQUESTS
        ) || status.is_server_error(),
        retry_after,
    }
}

pub use glyphst_core::retry_after_duration;

fn request_error(provider: &str, error: reqwest::Error) -> AiTranslationError {
    let timeout = error.is_timeout();
    AiTranslationError {
        category: if timeout {
            AiTranslationErrorCategory::Timeout
        } else {
            AiTranslationErrorCategory::Network
        },
        message: format!("{provider} translation request could not be completed"),
        status: error.status().map(|status| status.as_u16()),
        retryable: timeout || error.is_connect(),
        retry_after: None,
    }
}

fn configuration_error(message: String) -> AiTranslationError {
    AiTranslationError {
        category: AiTranslationErrorCategory::InvalidRequest,
        message,
        status: None,
        retryable: false,
        retry_after: None,
    }
}

fn authentication_error(message: String) -> AiTranslationError {
    AiTranslationError {
        category: AiTranslationErrorCategory::Authentication,
        message,
        status: None,
        retryable: false,
        retry_after: None,
    }
}

fn invalid_response(error: anyhow::Error) -> AiTranslationError {
    AiTranslationError {
        category: AiTranslationErrorCategory::InvalidResponse,
        message: error.to_string(),
        status: None,
        retryable: true,
        retry_after: None,
    }
}

fn invalid_response_message(provider: &str) -> AiTranslationError {
    AiTranslationError {
        category: AiTranslationErrorCategory::InvalidResponse,
        message: format!("{provider} response did not include text content"),
        status: None,
        retryable: true,
        retry_after: None,
    }
}

fn anthropic_max_tokens(payload: &Value) -> u32 {
    let characters = payload.to_string().chars().count() as u32;
    (characters.saturating_mul(2).saturating_add(1024)).clamp(4096, 8192)
}

fn openai_chat_url(base_url: &str) -> String {
    let base_url = base_url.trim_end_matches('/');
    if base_url.ends_with("/chat/completions") {
        base_url.to_string()
    } else if base_url_has_version(base_url) {
        format!("{base_url}/chat/completions")
    } else {
        format!("{base_url}/v1/chat/completions")
    }
}

fn anthropic_messages_url(base_url: &str) -> String {
    let base_url = base_url.trim_end_matches('/');
    if base_url.ends_with("/v1/messages") {
        base_url.to_string()
    } else if base_url.ends_with("/v1") {
        format!("{base_url}/messages")
    } else {
        format!("{base_url}/v1/messages")
    }
}

fn base_url_has_version(base_url: &str) -> bool {
    base_url.rsplit('/').next().is_some_and(|segment| {
        segment.len() > 1
            && segment.starts_with('v')
            && segment[1..].chars().all(|ch| ch.is_ascii_digit())
    })
}

fn parse_translations(content: &str, expected_count: usize) -> Result<Vec<String>> {
    let value = parse_json_object(content)?;
    let translations = value
        .get("translations")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("AI translation response is missing translations"))?
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| anyhow!("AI translation response contains a non-text item"))
        })
        .collect::<Result<Vec<_>>>()?;
    if translations.len() != expected_count {
        return Err(anyhow!(
            "AI translation response returned {} items, expected {expected_count}",
            translations.len()
        ));
    }
    Ok(translations)
}

fn parse_json_object(content: &str) -> Result<Value> {
    crate::json::parse_json_object(content, "AI translation response")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_and_fenced_json() {
        assert_eq!(
            parse_translations(r#"{"translations":["one","two"]}"#, 2).unwrap(),
            ["one", "two"]
        );
        assert_eq!(
            parse_translations("```json\n{\"translations\":[\"one\"]}\n```", 1).unwrap(),
            ["one"]
        );
    }

    #[test]
    fn rejects_wrong_item_counts() {
        assert!(parse_translations(r#"{"translations":["one"]}"#, 2).is_err());
    }

    #[test]
    fn semantic_responses_must_preserve_ids() {
        let expected = vec![SemanticTranslationInput {
            id: "unit-1".to_string(),
            role: "body".to_string(),
            text: "source".to_string(),
            context_before: None,
            context_after: None,
        }];

        assert!(
            parse_semantic_translations(
                r#"{"translations":[{"id":"unit-1","text":"target"}]}"#,
                &expected
            )
            .is_ok()
        );
        assert!(
            parse_semantic_translations(
                r#"{"translations":[{"id":"wrong","text":"target"}]}"#,
                &expected
            )
            .unwrap()
            .is_empty()
        );
    }

    #[test]
    fn semantic_responses_retain_valid_partial_items() {
        let expected = vec![
            SemanticTranslationInput {
                id: "unit-1".to_string(),
                role: "body".to_string(),
                text: "first".to_string(),
                context_before: None,
                context_after: None,
            },
            SemanticTranslationInput {
                id: "unit-2".to_string(),
                role: "body".to_string(),
                text: "second".to_string(),
                context_before: None,
                context_after: None,
            },
        ];

        let parsed = parse_semantic_translations(
            r#"{"translations":[{"id":"unit-2","text":"target"},{"id":"unit-1"},{"bad":true}]}"#,
            &expected,
        )
        .unwrap();

        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].id, "unit-2");
    }

    #[test]
    fn semantic_responses_reject_duplicate_ids() {
        let expected = vec![
            SemanticTranslationInput {
                id: "unit-1".to_string(),
                role: "body".to_string(),
                text: "first".to_string(),
                context_before: None,
                context_after: None,
            },
            SemanticTranslationInput {
                id: "unit-2".to_string(),
                role: "body".to_string(),
                text: "second".to_string(),
                context_before: None,
                context_after: None,
            },
        ];

        let parsed = parse_semantic_translations(
            r#"{"translations":[{"id":"unit-1","text":"first target"},{"id":"unit-1","text":"duplicate target"},{"id":"unit-2","text":"second target"}]}"#,
            &expected,
        )
        .unwrap();

        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].id, "unit-2");
    }

    #[test]
    fn policy_review_parser_is_fail_closed_for_invalid_and_duplicate_items() {
        let parsed = parse_policy_review_decisions(
            r#"{"decisions":[
                {"block_id":"body","translate":true,"confidence":0.91,"reason":"narrative prose"},
                {"block_id":"duplicate","translate":true,"confidence":0.95,"reason":"first"},
                {"block_id":"duplicate","translate":false,"confidence":0.99,"reason":"second"},
                {"block_id":"invalid","translate":true,"confidence":1.5,"reason":"bad confidence"}
            ]}"#,
        )
        .unwrap();

        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].block_id, "body");
    }

    #[test]
    fn retry_after_supports_seconds_and_http_dates() {
        assert_eq!(retry_after_duration("20"), Some(Duration::from_secs(20)));
        assert_eq!(
            retry_after_duration("Sun, 06 Nov 1994 08:49:37 GMT"),
            Some(Duration::ZERO)
        );
        assert_eq!(
            retry_after_duration("999999999"),
            Some(Duration::from_secs(15 * 60))
        );
    }

    #[test]
    fn anthropic_output_limit_scales_with_payload() {
        assert_eq!(anthropic_max_tokens(&json!({"text": "short"})), 4096);
        assert!(anthropic_max_tokens(&json!({"text": "x".repeat(4000)})) > 4096);
        assert_eq!(
            anthropic_max_tokens(&json!({"text": "x".repeat(20_000)})),
            8192
        );
    }

    #[test]
    fn deepseek_translation_explicitly_disables_thinking() {
        let settings = ResolvedAiConfig {
            provider: AiProvider::DeepSeek,
            provider_name: "DeepSeek".to_string(),
            api_style: AiApiStyle::OpenAi,
            enabled: true,
            model: "deepseek-v4-flash".to_string(),
            api_key: String::new(),
            base_url: "https://api.deepseek.com".to_string(),
            base_url_overridden: false,
            thinking_level: AiThinkingLevel::Off,
        };
        let mut body = json!({"model": settings.model});

        apply_translation_thinking_mode(&mut body, &settings);

        assert_eq!(body["thinking"], json!({"type": "disabled"}));
    }

    #[test]
    fn deepseek_translation_scales_the_output_budget() {
        let settings = ResolvedAiConfig {
            provider: AiProvider::DeepSeek,
            provider_name: "DeepSeek".to_string(),
            api_style: AiApiStyle::OpenAi,
            enabled: true,
            model: "deepseek-v4-flash".to_string(),
            api_key: String::new(),
            base_url: "https://api.deepseek.com".to_string(),
            base_url_overridden: false,
            thinking_level: AiThinkingLevel::Off,
        };
        let mut body = json!({"model": settings.model});

        apply_translation_output_budget(&mut body, &settings, &json!({"text": "x".repeat(6_000)}));

        assert!(body["max_tokens"].as_u64().unwrap() >= 12_000);
    }

    #[test]
    fn chatgpt_subscription_translation_uses_streaming_responses() {
        let settings = ResolvedAiConfig {
            provider: AiProvider::ChatGptSubscription,
            provider_name: "ChatGPT Subscription".to_string(),
            api_style: AiApiStyle::OpenAiResponses,
            enabled: true,
            model: "gpt-5.6-luna".to_string(),
            api_key: String::new(),
            base_url: "https://chatgpt.com/backend-api/codex".to_string(),
            base_url_overridden: false,
            thinking_level: AiThinkingLevel::Off,
        };

        let body = chatgpt_translation_body(&settings, "Translate to JSON.", "[]".to_string());

        assert_eq!(body["stream"], json!(true));
        assert_eq!(body["store"], json!(false));
        assert_eq!(body["input"][0]["content"][0]["type"], "input_text");
    }

    #[test]
    fn chatgpt_subscription_translation_collects_sse_text() {
        let body = concat!(
            "event: response.output_text.delta\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"{\\\"translations\\\":[\"}\n\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"\\\"你好\\\"]}\"}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{}}\n\n",
            "data: [DONE]\n"
        );

        assert_eq!(
            parse_chatgpt_stream_text(body).unwrap(),
            r#"{"translations":["你好"]}"#
        );
    }

    #[test]
    fn protected_token_recovery_segmentation_round_trips_all_tokens() {
        let source = (0..150)
            .map(|index| {
                format!("Entry {index} [[GLYPHST_IDENTIFIER_{index:04}_AAAAAAAA]] title. ")
            })
            .collect::<String>();
        let segments = split_recovery_segments(&source).unwrap();
        let restored = segments
            .into_iter()
            .map(|segment| match segment {
                RecoverySegment::Natural(text) | RecoverySegment::Protected(text) => text,
            })
            .collect::<String>();

        assert_eq!(restored, source);
        assert_eq!(restored.matches("[[GLYPHST_").count(), 150);
    }
}
