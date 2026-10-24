//! Local stand-ins for providers, for scripted runs and tests: no network
//! and no keys. The translation server speaks the Google Translation API
//! (as `translation/src/tests.rs` fakes it) and returns each text in «».
//! With `GLYPHST_FAKE_TRANSLATIONS` naming a JSON object of texts and their
//! Chinese translations (the website's shots), it translates into Chinese
//! from that file instead; a text it lacks stays as it is and is appended
//! to the file's `.missing` neighbour as a JSON line, so the file can be
//! completed.
//! The AI server speaks OpenAI's streaming chat completions: it first asks
//! for a `search_document` call, then answers in Markdown. Asked for a
//! concept page, it searches the library and writes one that cites the
//! first passage found, plus a made-up passage and a note that does not
//! exist (which the page writer must drop). It lists [`FAKE_MODELS`] as
//! its models.

use std::{collections::HashMap, io::Write as _, path::PathBuf, thread};

use anyhow::{Result, anyhow};
use glyphst_settings::{
    AiApiStyle, AiModel, AiProvider, AiProviderConfig, ModelRef, OcrMode, Settings,
    TranslationProvider, TranslationTargetLanguage,
};
use serde_json::{Value, json};

/// The fake translation of `text` into `target`: Chinese, Japanese and
/// Korean ones carry Han, kana or Hangul, so they need CJK faces.
pub fn fake_translation(text: &str, target: &str) -> String {
    let target = target.to_ascii_lowercase();
    if target.starts_with("zh") {
        format!("「{text}」译文")
    } else if target.starts_with("ja") {
        format!("「{text}」の翻訳")
    } else if target.starts_with("ko") {
        format!("「{text}」 번역문")
    } else {
        format!("«{text}»")
    }
}

/// The recorded translations `GLYPHST_FAKE_TRANSLATIONS` names, if any.
fn recorded_translations() -> Option<(PathBuf, HashMap<String, String>)> {
    let path = PathBuf::from(std::env::var_os("GLYPHST_FAKE_TRANSLATIONS")?);
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let recorded = serde_json::from_str(&text).unwrap_or_default();
    Some((path, recorded))
}

/// Starts the fake translation server; returns its port. It runs until the
/// process exits.
pub fn start_translation_server() -> Result<u16> {
    let recorded = recorded_translations();
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
                    .map(|text| {
                        let target = query["target"].as_str().unwrap_or_default();
                        let translation = match &recorded {
                            Some((path, recorded)) => {
                                recorded.get(text).cloned().unwrap_or_else(|| {
                                    let missing = path.with_extension("missing");
                                    if let Ok(mut file) = std::fs::OpenOptions::new()
                                        .create(true)
                                        .append(true)
                                        .open(missing)
                                    {
                                        // One write per line, so Readers sharing the
                                        // file never interleave.
                                        let _ =
                                            file.write_all(format!("{}\n", json!(text)).as_bytes());
                                    }
                                    text.to_string()
                                })
                            }
                            None => fake_translation(text, target),
                        };
                        json!({ "translatedText": translation })
                    })
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
    use_fake_translation(&mut settings, port, None);
    settings
}

/// Points translation at the fake server on `port`, into `target` (French
/// unless named).
pub fn use_fake_translation(settings: &mut Settings, port: u16, target: Option<&str>) {
    settings.ocr.mode = OcrMode::Off;
    settings.translation.active_provider = TranslationProvider::Google;
    // The recorded translations are Chinese; otherwise French.
    let fallback = if std::env::var_os("GLYPHST_FAKE_TRANSLATIONS").is_some() {
        "zh-hans"
    } else {
        "fr"
    };
    settings.translation.target_language =
        TranslationTargetLanguage::Explicit(target.unwrap_or(fallback).into());
    let google = settings
        .translation
        .provider_config_mut(TranslationProvider::Google);
    google.api_key = "fake".into();
    google.base_url = Some(format!("http://localhost:{port}"));
}

/// The search the fake assistant asks for.
pub const FAKE_SEARCH: &str = "the";
/// The fake model's title for any chat.
pub const FAKE_TITLE: &str = "Where the story ends";
/// The fake model's summary of any conversation.
pub const FAKE_SUMMARY: &str = "We discussed where the story ends.";
/// The fake library model's answer to any item.
pub const FAKE_ENRICHMENT: &str = r#"{"tldr":"A fake summary of the item.","key_points":["first point","second point","third point"],"tags":["fake topic"],"collection":{"new":"Fake / Collection"},"collection_confidence":0.9,"kind":"paper","language":"en","metadata":null,"confidence":0.8}"#;
/// The fake assistant's answer.
pub const FAKE_ANSWER: &str = "## In short\n\nThe document **mentions** this in two places:\n\n\
1. on the first page\n2. near the end\n\nSee `search_document` above.";

/// The library search the fake model runs for a concept page.
pub const FAKE_PAGE_SEARCH: &str = "attention";

/// The fake model's page: the first passage the search found, cited twice,
/// a made-up passage, the first source's note and a note that does not
/// exist.
fn fake_page(conversation: &str, last: &str) -> String {
    let found = last
        .find("glyphst://open?id=")
        .map(|start| {
            let rest = &last[start..];
            let end = rest
                .find(|c: char| !(c.is_ascii_alphanumeric() || "/:?=&_".contains(c)))
                .unwrap_or(rest.len());
            rest[..end].to_string()
        })
        .unwrap_or_default();
    let source = conversation
        .split("[[")
        .nth(1)
        .and_then(|rest| rest.split("]]").next())
        .unwrap_or("nobody");
    format!(
        "Attention lets a model weigh every token [p 1]({found}), as [[{source}]] shows; \
         it was also invented on the moon [p 9](glyphst://open?id=1&page=9&chunk=999999).\n\n\
         ## In practice\n\nSee [[no-such-note]] and again [the passage]({found})."
    )
}

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
                if request.url().ends_with("/models") {
                    let data: Vec<Value> = FAKE_MODELS
                        .iter()
                        .map(|(id, name)| json!({"id": id, "name": name}))
                        .collect();
                    let response =
                        tiny_http::Response::from_string(json!({ "data": data }).to_string())
                            .with_header(
                                "Content-Type: application/json"
                                    .parse::<tiny_http::Header>()
                                    .expect("valid header"),
                            );
                    let _ = request.respond(response);
                    continue;
                }
                let mut body = String::new();
                let _ = request.as_reader().read_to_string(&mut body);
                let query: Value = serde_json::from_str(&body).unwrap_or_default();
                let answered = query["messages"]
                    .as_array()
                    .is_some_and(|messages| messages.iter().any(|m| m["role"] == "tool"));
                let last = query["messages"]
                    .as_array()
                    .and_then(|messages| messages.last())
                    .map(|message| message["content"].to_string())
                    .unwrap_or_default();
                let all = query["messages"].to_string();
                let page = all
                    .contains("<concept_page>")
                    .then(|| fake_page(&all, &last));
                // Titles and summaries are answered at once.
                let reply = if page.is_some() && answered {
                    page.as_deref()
                } else if last.contains("Generate a concise") {
                    Some(FAKE_TITLE)
                } else if last.contains("Summarize this conversation") {
                    Some(FAKE_SUMMARY)
                } else if last.contains("<existing_tags>") {
                    Some(FAKE_ENRICHMENT)
                } else {
                    None
                };
                let events: Vec<Value> = if let Some(reply) = reply {
                    vec![
                        json!({"choices": [{"delta": {"content": reply}}]}),
                        json!({"choices": [{"delta": {}, "finish_reason": "stop"}]}),
                    ]
                } else if answered {
                    let mut events: Vec<Value> = FAKE_ANSWER
                        .split_inclusive(' ')
                        .map(|chunk| json!({"choices": [{"delta": {"content": chunk}}]}))
                        .collect();
                    events.push(json!({"choices": [{"delta": {}, "finish_reason": "stop"}]}));
                    events
                } else {
                    let (tool, arguments) = if page.is_some() {
                        ("search_library", json!({"query": FAKE_PAGE_SEARCH}))
                    } else {
                        (
                            "search_document",
                            json!({"query": FAKE_SEARCH, "max_results": 3}),
                        )
                    };
                    vec![
                        json!({"choices": [{"delta": {"tool_calls": [{
                            "index": 0,
                            "id": "call_1",
                            "function": {
                                "name": tool,
                                "arguments": arguments.to_string()
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
    settings.ai.library_model = Some(ModelRef::new(provider, FAKE_MODEL));
}

const FAKE_MODEL: &str = "fake-model";
/// The models the fake AI server lists: id and name.
pub const FAKE_MODELS: [(&str, &str); 2] = [
    (FAKE_MODEL, "Fake model"),
    ("fake-large", "Fake large model"),
];
