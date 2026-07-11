use anyhow::{Context as _, Result, anyhow};
use serde_json::{Value, json};

use crate::settings::{AiApiStyle, ResolvedAiConfig};

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
    let system = translation_system_prompt(source_language, target_language);
    let response = match settings.provider.api_style() {
        AiApiStyle::OpenAi => request_openai(&client, &settings, &system, texts),
        AiApiStyle::Anthropic => request_anthropic(&client, &settings, &system, texts),
    }?;
    parse_translations(&response, texts.len())
}

fn translation_system_prompt(source_language: Option<&str>, target_language: &str) -> String {
    let source = source_language.unwrap_or("the detected source language");
    format!(
        "You are a precise document translator. Translate every input from {source} to \
{target_language}. Preserve equations, citations, reference markers, URLs, names, and paragraph \
meaning. Return only valid JSON in this exact shape: {{\"translations\":[\"...\"]}}. The \
translations array must have the same length and order as the input texts."
    )
}

fn request_openai(
    client: &reqwest::blocking::Client,
    settings: &ResolvedAiConfig,
    system: &str,
    texts: &[String],
) -> Result<String> {
    let url = openai_chat_url(&settings.base_url);
    let mut request = client.post(url).json(&json!({
        "model": settings.model,
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": serde_json::to_string(texts)?}
        ],
        "temperature": 0,
    }));
    if !settings.api_key.is_empty() {
        request = request.bearer_auth(&settings.api_key);
    }

    let value = send_json(request, settings.provider.label())?;
    value
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| {
            anyhow!(
                "{} response did not include text content",
                settings.provider.label()
            )
        })
}

fn request_anthropic(
    client: &reqwest::blocking::Client,
    settings: &ResolvedAiConfig,
    system: &str,
    texts: &[String],
) -> Result<String> {
    let url = anthropic_messages_url(&settings.base_url);
    let response = client
        .post(url)
        .header("x-api-key", &settings.api_key)
        .header("anthropic-version", "2023-06-01")
        .json(&json!({
            "model": settings.model,
            "system": system,
            "messages": [{
                "role": "user",
                "content": serde_json::to_string(texts)?,
            }],
            "max_tokens": 4096,
            "temperature": 0,
        }));

    let value = send_json(response, settings.provider.label())?;
    value
        .pointer("/content/0/text")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| {
            anyhow!(
                "{} response did not include text content",
                settings.provider.label()
            )
        })
}

fn send_json(request: reqwest::blocking::RequestBuilder, provider: &str) -> Result<Value> {
    let response = request
        .send()
        .with_context(|| format!("failed to send {provider} translation request"))?;
    let status = response.status();
    let body = response
        .text()
        .with_context(|| format!("failed to read {provider} translation response"))?;
    if !status.is_success() {
        return Err(anyhow!(
            "{provider} translation request failed with status {status}"
        ));
    }
    serde_json::from_str(&body).context("failed to parse AI translation response")
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
}
