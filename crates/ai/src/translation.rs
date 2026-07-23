use std::fmt;
use std::time::{Duration, SystemTime};

use anyhow::{Context as _, Result, anyhow};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::settings::{AiApiStyle, ResolvedAiConfig};

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
    let response = match settings.provider.api_style() {
        AiApiStyle::OpenAi => request_openai(client, &settings, &system, texts),
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
page furniture, formulas, code and literal examples, table cells, chart labels, axis labels, \
legends, and text associated with protected regions. Review only unlocked low-confidence blocks. \
When uncertain, choose translate=false. Return only valid JSON in this exact shape: \
{\"decisions\":[{\"block_id\":\"id\",\"translate\":false,\"confidence\":0.0,\"reason\":\"short \
reason\"}]}. Omit locked and already-confident blocks.";
    let response = match settings.provider.api_style() {
        AiApiStyle::OpenAi => request_openai_payload(client, &settings, system, page_map),
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
placeholder. Preserve \
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
    let response = match settings.provider.api_style() {
        AiApiStyle::OpenAi => request_openai_payload(client, &settings, &system, payload),
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
    let input = vec![unit.text.clone()];
    let mut outputs =
        translate_batch_with_client(client, settings, &input, source_language, target_language)?;
    if outputs.len() != 1 {
        return Err(invalid_response(anyhow!(
            "AI translation recovery omitted the semantic unit"
        )));
    }
    Ok(SemanticTranslationOutput {
        id: unit.id.clone(),
        text: outputs.remove(0),
    })
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
    let mut request = client.post(url).json(&body);
    if !settings.api_key.is_empty() {
        request = request.bearer_auth(&settings.api_key);
    }

    let value = send_json(request, settings.provider.label())?;
    value
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| invalid_response_message(settings.provider.label()))
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
    let response = client
        .post(url)
        .header("x-api-key", &settings.api_key)
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

    let value = send_json(response, settings.provider.label())?;
    value
        .pointer("/content/0/text")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| invalid_response_message(settings.provider.label()))
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
        let category = match status {
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                AiTranslationErrorCategory::Authentication
            }
            StatusCode::TOO_MANY_REQUESTS => AiTranslationErrorCategory::RateLimited,
            StatusCode::REQUEST_TIMEOUT => AiTranslationErrorCategory::Timeout,
            status if status.is_server_error() => AiTranslationErrorCategory::ServiceUnavailable,
            _ => AiTranslationErrorCategory::InvalidRequest,
        };
        return Err(AiTranslationError {
            category,
            message: format!("{provider} translation request failed with status {status}"),
            status: Some(status.as_u16()),
            retryable: matches!(
                status,
                StatusCode::REQUEST_TIMEOUT | StatusCode::TOO_MANY_REQUESTS
            ) || status.is_server_error(),
            retry_after,
        });
    }
    serde_json::from_str(&body).map_err(|_| AiTranslationError {
        category: AiTranslationErrorCategory::InvalidResponse,
        message: format!("{provider} returned an invalid JSON response"),
        status: Some(status.as_u16()),
        retryable: true,
        retry_after: None,
    })
}

pub fn retry_after_duration(value: &str) -> Option<Duration> {
    const MAX_RETRY_AFTER: Duration = Duration::from_secs(15 * 60);
    if let Ok(seconds) = value.trim().parse::<u64>() {
        return Some(Duration::from_secs(seconds).min(MAX_RETRY_AFTER));
    }
    let deadline = httpdate::parse_http_date(value).ok()?;
    Some(
        deadline
            .duration_since(SystemTime::now())
            .unwrap_or_default()
            .min(MAX_RETRY_AFTER),
    )
}

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
    let content = content.trim();
    let value = match serde_json::from_str::<Value>(content) {
        Ok(value) => value,
        Err(_) => {
            let start = content
                .find('{')
                .ok_or_else(|| anyhow!("AI translation response was not JSON"))?;
            let end = content
                .rfind('}')
                .ok_or_else(|| anyhow!("AI translation response was not JSON"))?;
            serde_json::from_str::<Value>(&content[start..=end])
                .context("AI translation response was not JSON")?
        }
    };
    Ok(value)
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
}
