use anyhow::{Context as _, Result, anyhow};
use serde_json::Value;

use crate::chatgpt_subscription::acquire_request_access;
use crate::settings::{AiApiStyle, ResolvedAiConfig};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AiModelInfo {
    pub id: String,
    pub label: String,
    pub owned_by: Option<String>,
}

pub fn test_ai_connection(config: ResolvedAiConfig) -> Result<()> {
    if config.api_style == AiApiStyle::OpenAiResponses {
        acquire_request_access()?;
        return Ok(());
    }
    if config.base_url.trim().is_empty() {
        return Err(anyhow!("missing {} API Host", config.provider_name));
    }
    if !config.provider.is_custom() && config.api_key.is_empty() {
        return Err(anyhow!("missing {} API key", config.provider_name));
    }
    list_ai_models(config).map(|_| ())
}

pub fn list_ai_models(config: ResolvedAiConfig) -> Result<Vec<AiModelInfo>> {
    if config.base_url.trim().is_empty() {
        return Err(anyhow!("missing {} API Host", config.provider_name));
    }

    let client = reqwest::blocking::Client::builder()
        .build()
        .context("failed to create HTTP client")?;

    match config.api_style {
        AiApiStyle::OpenAi => list_openai_compatible_models(&client, config),
        AiApiStyle::OpenAiResponses => Ok(config
            .provider
            .default_models()
            .into_iter()
            .map(|model| AiModelInfo {
                id: model.id,
                label: model.name,
                owned_by: Some(model.group),
            })
            .collect()),
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

    let value = send_json(request, &config.provider_name)?;
    parse_models_response(value)
}

fn list_anthropic_models(
    client: &reqwest::blocking::Client,
    config: ResolvedAiConfig,
) -> Result<Vec<AiModelInfo>> {
    let mut request = client
        .get(anthropic_models_url(&config.base_url))
        .header("anthropic-version", "2023-06-01");
    if !config.api_key.is_empty() {
        request = request.header("x-api-key", config.api_key);
    }

    let value = send_json(request, &config.provider_name)?;
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

    Ok(models)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{AiProvider, AiThinkingLevel};
    use serde_json::json;
    use std::{collections::BTreeMap, thread::JoinHandle, time::Duration};

    struct CapturedModelRequest {
        path: String,
        headers: BTreeMap<String, String>,
    }

    fn start_model_server() -> (String, JoinHandle<CapturedModelRequest>) {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let address = server.server_addr().to_ip().unwrap();
        let base_url = format!("http://{address}");
        let task = std::thread::spawn(move || {
            let request = server
                .recv_timeout(Duration::from_secs(5))
                .unwrap()
                .expect("model list request did not arrive");
            let path = request.url().to_string();
            let headers = request
                .headers()
                .iter()
                .map(|header| {
                    (
                        header.field.as_str().to_ascii_lowercase().to_string(),
                        header.value.as_str().to_string(),
                    )
                })
                .collect();
            request
                .respond(tiny_http::Response::from_string(
                    r#"{"object":"list","data":[{"id":"remote-model","name":"Remote Model"}]}"#,
                ))
                .unwrap();
            CapturedModelRequest { path, headers }
        });
        (base_url, task)
    }

    fn model_config(
        provider: AiProvider,
        api_style: AiApiStyle,
        base_url: String,
        api_key: impl Into<String>,
    ) -> ResolvedAiConfig {
        ResolvedAiConfig {
            provider,
            provider_name: if provider.is_custom() {
                "Custom Provider".to_string()
            } else {
                provider.label().to_string()
            },
            api_style,
            enabled: false,
            model: String::new(),
            api_key: api_key.into(),
            base_url,
            base_url_overridden: provider.is_custom(),
            thinking_level: AiThinkingLevel::Off,
        }
    }

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
                {
                    "id": "claude-sonnet-4-5",
                    "display_name": "Claude Sonnet 4.5",
                    "name": "Ignored name"
                },
                {"id": "deepseek-chat", "name": "DeepSeek Chat"}
            ]
        }))
        .unwrap();

        assert_eq!(models.len(), 3);
        assert_eq!(models[0].id, "gpt-4o");
        assert_eq!(models[0].label, "gpt-4o");
        assert_eq!(models[0].owned_by.as_deref(), Some("openai"));
        assert_eq!(models[1].label, "Claude Sonnet 4.5");
        assert_eq!(models[2].label, "DeepSeek Chat");
    }

    #[test]
    fn model_catalog_fetch_does_not_require_enabled_model_or_api_key() {
        let (base_url, task) = start_model_server();
        let config = model_config(AiProvider::Custom(1), AiApiStyle::OpenAi, base_url, "");

        let models = list_ai_models(config).unwrap();
        let request = task.join().unwrap();

        assert_eq!(models[0].id, "remote-model");
        assert_eq!(request.path, "/v1/models");
        assert!(!request.headers.contains_key("authorization"));
    }

    #[test]
    fn fixed_provider_model_catalog_can_be_fetched_anonymously() {
        let (base_url, task) = start_model_server();
        let config = model_config(AiProvider::OpenRouter, AiApiStyle::OpenAi, base_url, "");

        list_ai_models(config).unwrap();
        let request = task.join().unwrap();

        assert_eq!(request.path, "/v1/models");
        assert!(!request.headers.contains_key("authorization"));
    }

    #[test]
    fn model_catalog_fetch_attaches_configured_authentication() {
        let (base_url, task) = start_model_server();
        let config = model_config(
            AiProvider::Custom(2),
            AiApiStyle::Anthropic,
            base_url,
            "secret",
        );

        list_ai_models(config).unwrap();
        let request = task.join().unwrap();

        assert_eq!(request.path, "/v1/models");
        assert_eq!(
            request.headers.get("x-api-key").map(String::as_str),
            Some("secret")
        );
        assert_eq!(
            request.headers.get("anthropic-version").map(String::as_str),
            Some("2023-06-01")
        );
    }

    #[test]
    fn connection_test_requires_fixed_provider_credentials_but_not_a_model() {
        let fixed = model_config(
            AiProvider::OpenRouter,
            AiApiStyle::OpenAi,
            "http://127.0.0.1:9".to_string(),
            "",
        );
        assert_eq!(
            test_ai_connection(fixed).unwrap_err().to_string(),
            "missing OpenRouter API key"
        );

        let (base_url, task) = start_model_server();
        let fixed = model_config(
            AiProvider::OpenRouter,
            AiApiStyle::OpenAi,
            base_url,
            "secret",
        );
        test_ai_connection(fixed).unwrap();
        assert_eq!(
            task.join()
                .unwrap()
                .headers
                .get("authorization")
                .map(String::as_str),
            Some("Bearer secret")
        );

        let (base_url, task) = start_model_server();
        let custom = model_config(AiProvider::Custom(3), AiApiStyle::OpenAi, base_url, "");
        test_ai_connection(custom).unwrap();
        task.join().unwrap();
    }
}
