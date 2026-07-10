use anyhow::{Context as _, Result, anyhow};
use serde_json::Value;

use crate::settings::{AiApiStyle, ResolvedAiConfig};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AiModelInfo {
    pub id: String,
    pub label: String,
    pub owned_by: Option<String>,
}

pub fn test_ai_connection(config: ResolvedAiConfig) -> Result<()> {
    list_ai_models(config).map(|_| ())
}

pub fn list_ai_models(config: ResolvedAiConfig) -> Result<Vec<AiModelInfo>> {
    if !config.can_request() {
        return Err(anyhow!(config.missing_credentials_message()));
    }

    let client = reqwest::blocking::Client::builder()
        .build()
        .context("failed to create HTTP client")?;

    match config.provider.api_style() {
        AiApiStyle::OpenAi => list_openai_compatible_models(&client, config),
        AiApiStyle::Anthropic => list_anthropic_models(&client, config),
    }
}

fn list_openai_compatible_models(
    client: &reqwest::blocking::Client,
    config: ResolvedAiConfig,
) -> Result<Vec<AiModelInfo>> {
    let mut request = client.get(openai_models_url(&config.base_url));
    if !config.api_key.is_empty() {
        request = request.bearer_auth(config.api_key);
    }

    let value = send_json(request, config.provider.label())?;
    parse_models_response(value)
}

fn list_anthropic_models(
    client: &reqwest::blocking::Client,
    config: ResolvedAiConfig,
) -> Result<Vec<AiModelInfo>> {
    let request = client
        .get(anthropic_models_url(&config.base_url))
        .header("x-api-key", config.api_key)
        .header("anthropic-version", "2023-06-01");

    let value = send_json(request, config.provider.label())?;
    parse_models_response(value)
}

fn send_json(request: reqwest::blocking::RequestBuilder, provider: &str) -> Result<Value> {
    let response = request
        .send()
        .with_context(|| format!("failed to send {provider} model list request"))?;
    let status = response.status();
    let body = response
        .text()
        .with_context(|| format!("failed to read {provider} model list response"))?;

    if !status.is_success() {
        return Err(anyhow!(
            "{provider} model list request failed with status {status}: {body}"
        ));
    }

    serde_json::from_str(&body).context("failed to parse model list response")
}

fn openai_models_url(base_url: &str) -> String {
    let base_url = base_url.trim_end_matches('/');
    if base_url.ends_with("/models") {
        base_url.to_string()
    } else if openai_base_url_has_version(base_url) {
        format!("{base_url}/models")
    } else {
        format!("{base_url}/v1/models")
    }
}

fn openai_base_url_has_version(base_url: &str) -> bool {
    base_url.rsplit('/').next().is_some_and(|segment| {
        segment.len() > 1
            && segment.starts_with('v')
            && segment[1..].chars().all(|ch| ch.is_ascii_digit())
    })
}

fn anthropic_models_url(base_url: &str) -> String {
    let base_url = base_url.trim_end_matches('/');
    if base_url.ends_with("/v1/models") {
        base_url.to_string()
    } else if base_url.ends_with("/v1") {
        format!("{base_url}/models")
    } else {
        format!("{base_url}/v1/models")
    }
}

fn parse_models_response(value: Value) -> Result<Vec<AiModelInfo>> {
    let Some(items) = value.get("data").and_then(Value::as_array) else {
        return Err(anyhow!("model list response is missing a data array"));
    };

    let mut models = Vec::new();
    for item in items {
        let Some(id) = item.get("id").and_then(Value::as_str) else {
            continue;
        };
        let id = id.trim();
        if id.is_empty() || models.iter().any(|model: &AiModelInfo| model.id == id) {
            continue;
        }

        let label = item
            .get("display_name")
            .or_else(|| item.get("name"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|label| !label.is_empty())
            .unwrap_or(id);
        let owned_by = item
            .get("owned_by")
            .or_else(|| item.get("ownedBy"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);

        models.push(AiModelInfo {
            id: id.to_string(),
            label: label.to_string(),
            owned_by,
        });
    }

    models.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(models)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn builds_model_list_urls() {
        assert_eq!(
            openai_models_url("https://api.openai.com"),
            "https://api.openai.com/v1/models"
        );
        assert_eq!(
            openai_models_url("https://api.openai.com/v1"),
            "https://api.openai.com/v1/models"
        );
        assert_eq!(
            openai_models_url("https://ark.cn-beijing.volces.com/api/v3"),
            "https://ark.cn-beijing.volces.com/api/v3/models"
        );
        assert_eq!(
            anthropic_models_url("https://api.anthropic.com"),
            "https://api.anthropic.com/v1/models"
        );
    }

    #[test]
    fn parses_and_deduplicates_models() {
        let models = parse_models_response(json!({
            "data": [
                {"id": "gpt-4o", "owned_by": "openai"},
                {"id": "gpt-4o", "owned_by": "openai"},
                {"id": "claude-sonnet-4-5", "display_name": "Claude Sonnet 4.5"}
            ]
        }))
        .unwrap();

        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id, "claude-sonnet-4-5");
        assert_eq!(models[0].label, "Claude Sonnet 4.5");
        assert_eq!(models[1].owned_by.as_deref(), Some("openai"));
    }
}
