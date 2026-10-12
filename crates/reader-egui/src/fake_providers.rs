//! Local stand-ins for providers, for scripted runs and tests: no network
//! and no keys. The translation server speaks the Google Translation API
//! (as `translation/src/tests.rs` fakes it) and returns each text in «».

use std::thread;

use anyhow::{Result, anyhow};
use glyphst_settings::{OcrMode, Settings, TranslationProvider, TranslationTargetLanguage};
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
pub fn translation_settings(port: u16) -> Settings {
    let mut settings = Settings::default();
    settings.ocr.mode = OcrMode::Off;
    settings.translation.active_provider = TranslationProvider::Google;
    settings.translation.target_language = TranslationTargetLanguage::Explicit("fr".into());
    let google = settings
        .translation
        .provider_config_mut(TranslationProvider::Google);
    google.api_key = "fake".into();
    google.base_url = Some(format!("http://localhost:{port}"));
    settings
}
