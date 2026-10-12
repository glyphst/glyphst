//! Local stand-ins for providers, for scripted runs and tests: no network
//! and no keys. The translation server speaks the Google Translation API
//! (as `translation/src/tests.rs` fakes it) and returns each text in «».
//! The AI server speaks OpenAI's streaming chat completions: it first asks
//! for a `search_document` call, then answers in Markdown.

use std::thread;

use anyhow::{Result, anyhow};
use glyphst_settings::{
    AiApiStyle, AiModel, AiProvider, AiProviderConfig, ModelRef, OcrMode, Settings,
    TranslationProvider, TranslationTargetLanguage,
};
use serde_json::{Value, json};

/// The fake translation of `text`.
pub fn fake_translation(text: &str) -> String {
    format!("«{text}»")
}

/// Starts the fake translation server; returns its port. It runs until the
/// process exits.
pub fn start_translation_server() -> Result<u16> {
    let server = tiny_http::Server::http("127.0.0.1:0").map_err(|error| anyhow!("{error}"))?;
    let port = server
        .server_addr()
        .to_ip()
        .map(|address| address.port())
        .ok_or_else(|| anyhow!("the fake translation server has no port"))?;
    thread::Builder::new()
        .name("fake-translation".into())
        .spawn(move || {
            for mut request in server.incoming_requests() {
                let mut body = String::new();
                let _ = request.as_reader().read_to_string(&mut body);
                let query: Value = serde_json::from_str(&body).unwrap_or_default();
                let texts: Vec<&str> = match &query["q"] {
                    Value::Array(texts) => texts.iter().filter_map(Value::as_str).collect(),
                    Value::String(text) => vec![text.as_str()],
                    _ => Vec::new(),
                };
                let translations: Vec<Value> = texts
                    .into_iter()
                    .map(|text| json!({ "translatedText": fake_translation(text) }))
                    .collect();
                let response = tiny_http::Response::from_string(
                    json!({ "data": { "translations": translations } }).to_string(),
                )
                .with_header(
                    "Content-Type: application/json"
                        .parse::<tiny_http::Header>()
                        .expect("valid header"),
                );
                let _ = request.respond(response);
            }
        })?;
    Ok(port)
}

/// Settings translating into French through the fake server on `port`,
/// reading pages' own text (no OCR provider).
#[cfg(test)]
pub fn translation_settings(port: u16) -> Settings {
    let mut settings = Settings::default();
    use_fake_translation(&mut settings, port);
    settings
}

/// Points translation at the fake server on `port`.
pub fn use_fake_translation(settings: &mut Settings, port: u16) {
    settings.ocr.mode = OcrMode::Off;
    settings.translation.active_provider = TranslationProvider::Google;
    settings.translation.target_language = TranslationTargetLanguage::Explicit("fr".into());
    let google = settings
        .translation
        .provider_config_mut(TranslationProvider::Google);
    google.api_key = "fake".into();
    google.base_url = Some(format!("http://localhost:{port}"));
}

/// The search the fake assistant asks for.
pub const FAKE_SEARCH: &str = "the";
/// The fake assistant's answer.
pub const FAKE_ANSWER: &str = "## In short\n\nThe document **mentions** this in two places:\n\n\
1. on the first page\n2. near the end\n\nSee `search_document` above.";

/// Starts the fake AI server; returns its port.
pub fn start_ai_server() -> Result<u16> {
    let server = tiny_http::Server::http("127.0.0.1:0").map_err(|error| anyhow!("{error}"))?;
    let port = server
        .server_addr()
        .to_ip()
        .map(|address| address.port())
        .ok_or_else(|| anyhow!("the fake AI server has no port"))?;
    thread::Builder::new()
        .name("fake-ai".into())
        .spawn(move || {
            for mut request in server.incoming_requests() {
                let mut body = String::new();
                let _ = request.as_reader().read_to_string(&mut body);
                let query: Value = serde_json::from_str(&body).unwrap_or_default();
                let answered = query["messages"]
                    .as_array()
                    .is_some_and(|messages| messages.iter().any(|m| m["role"] == "tool"));
                let events: Vec<Value> = if answered {
                    let mut events: Vec<Value> = FAKE_ANSWER
                        .split_inclusive(' ')
                        .map(|chunk| json!({"choices": [{"delta": {"content": chunk}}]}))
                        .collect();
                    events.push(json!({"choices": [{"delta": {}, "finish_reason": "stop"}]}));
                    events
                } else {
                    vec![
                        json!({"choices": [{"delta": {"tool_calls": [{
                            "index": 0,
                            "id": "call_1",
                            "function": {
                                "name": "search_document",
                                "arguments": json!({"query": FAKE_SEARCH, "max_results": 3}).to_string()
                            }
                        }]}}]}),
                        json!({"choices": [{"delta": {}, "finish_reason": "tool_calls"}]}),
                    ]
                };
                let mut stream: String = events
                    .into_iter()
                    .map(|event| format!("data: {event}\n\n"))
                    .collect();
                stream.push_str("data: [DONE]\n\n");
                let response = tiny_http::Response::from_string(stream).with_header(
                    "Content-Type: text/event-stream"
                        .parse::<tiny_http::Header>()
                        .expect("valid header"),
                );
                let _ = request.respond(response);
            }
        })?;
    Ok(port)
}

/// Points the assistant's model at the fake server on `port`: a custom
/// OpenAI-style provider, since only custom providers take a base URL.
pub fn use_fake_ai(settings: &mut Settings, port: u16) {
    let provider = AiProvider::Custom(1);
    settings.ai.providers.insert(
        provider,
        AiProviderConfig {
            name: "Fake".into(),
            api_style: AiApiStyle::OpenAi,
            enabled: true,
            api_key: "fake".into(),
            base_url: Some(format!("http://localhost:{port}")),
            models: vec![AiModel::new(FAKE_MODEL, "Fake model", "Fake")],
        },
    );
    settings.ai.provider_order.insert(0, provider);
    settings.ai.main_model = Some(ModelRef::new(provider, FAKE_MODEL));
}

const FAKE_MODEL: &str = "fake-model";
