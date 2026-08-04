use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use reqwest::StatusCode;
use reqwest::blocking::{Client, Response};
use serde_json::{Value, json};

use crate::paddle::normalize_paddle_payload;
use crate::{
    BlockKind, CancellationToken, DocumentV1, PageGeometry, PolicyDecisionSource, TextFlow,
};

#[derive(Debug, Clone)]
pub struct UnlimitedClientConfig {
    pub base_url: String,
    pub request_timeout: Duration,
    pub download_timeout: Duration,
    pub poll_interval: Duration,
    pub poll_timeout: Duration,
    pub retry_attempts: usize,
    pub retry_base_delay: Duration,
}

impl Default for UnlimitedClientConfig {
    fn default() -> Self {
        Self {
            base_url: "https://aip.baidubce.com".to_string(),
            request_timeout: Duration::from_secs(180),
            download_timeout: Duration::from_secs(180),
            poll_interval: Duration::from_secs(5),
            poll_timeout: Duration::from_secs(60 * 60),
            retry_attempts: 3,
            retry_base_delay: Duration::from_secs(1),
        }
    }
}

#[derive(Clone)]
pub struct UnlimitedClient {
    config: UnlimitedClientConfig,
    api_key: String,
    secret_key: String,
    http: Client,
}

impl fmt::Debug for UnlimitedClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UnlimitedClient")
            .field("base_url", &self.config.base_url)
            .field("api_key", &"[REDACTED]")
            .field("secret_key", &"[REDACTED]")
            .finish_non_exhaustive()
    }
}

impl UnlimitedClient {
    pub fn new(
        mut config: UnlimitedClientConfig,
        api_key: impl Into<String>,
        secret_key: impl Into<String>,
    ) -> Result<Self> {
        config.base_url = config.base_url.trim().trim_end_matches('/').to_string();
        if config.base_url.is_empty() {
            config.base_url = UnlimitedClientConfig::default().base_url;
        }
        let api_key = api_key.into().trim().to_string();
        let secret_key = secret_key.into().trim().to_string();
        if api_key.is_empty() || secret_key.is_empty() {
            bail!("Unlimited-OCR API key and secret key are not configured");
        }
        let timeout = config.request_timeout.max(config.download_timeout);
        let http = Client::builder()
            .connect_timeout(config.request_timeout)
            .timeout(timeout)
            .build()
            .context("failed to create Unlimited-OCR HTTP client")?;
        Ok(Self {
            config,
            api_key,
            secret_key,
            http,
        })
    }

    pub fn probe(
        &self,
        cancellation: &CancellationToken,
    ) -> Result<UnlimitedProbeResult, UnlimitedError> {
        self.access_token(cancellation)
            .map(|token| UnlimitedProbeResult {
                expires_in_seconds: token.expires_in_seconds,
            })
            .map_err(|error| self.redact_error(error))
    }

    pub fn run_document(
        &self,
        file_path: &Path,
        cancellation: &CancellationToken,
        on_progress: impl FnMut(UnlimitedJobProgress),
    ) -> Result<UnlimitedOcrResult, UnlimitedError> {
        cancellation
            .check()
            .map_err(|_| UnlimitedError::cancelled("submit"))?;
        let file_name = file_path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or_default();
        let bytes = fs::read(file_path).map_err(|error| {
            UnlimitedError::new(
                "submit",
                UnlimitedErrorCategory::InvalidRequest,
                format!("could not read Unlimited-OCR source: {error}"),
                None,
                None,
                None,
            )
        })?;
        self.run_document_bytes(file_name, &bytes, cancellation, on_progress)
    }

    pub fn run_document_bytes(
        &self,
        file_name: &str,
        bytes: &[u8],
        cancellation: &CancellationToken,
        on_progress: impl FnMut(UnlimitedJobProgress),
    ) -> Result<UnlimitedOcrResult, UnlimitedError> {
        self.run_document_inner(file_name, bytes, cancellation, on_progress)
            .map_err(|error| self.redact_error(error))
    }

    fn run_document_inner(
        &self,
        file_name: &str,
        bytes: &[u8],
        cancellation: &CancellationToken,
        mut on_progress: impl FnMut(UnlimitedJobProgress),
    ) -> Result<UnlimitedOcrResult, UnlimitedError> {
        cancellation
            .check()
            .map_err(|_| UnlimitedError::cancelled("authenticate"))?;
        let token = self.access_token(cancellation)?;
        let submitted = self.submit_file_bytes(file_name, bytes, &token.value, cancellation)?;
        let task_id = submitted.data;
        let mut latest_log_id = submitted.log_id;
        let started = Instant::now();

        loop {
            cancellation
                .check()
                .map_err(|_| UnlimitedError::cancelled("poll"))?;
            if started.elapsed() >= self.config.poll_timeout {
                return Err(UnlimitedError::new(
                    "poll",
                    UnlimitedErrorCategory::PollTimeout,
                    format!("timed out waiting for Unlimited-OCR task {task_id}"),
                    latest_log_id,
                    None,
                    None,
                ));
            }
            let query = self.query_task(&task_id, &token.value, cancellation)?;
            latest_log_id = query.log_id.or(latest_log_id);
            let state = query.data.status.trim().to_ascii_lowercase();
            on_progress(UnlimitedJobProgress {
                state: state.clone(),
            });
            match state.as_str() {
                "pending" | "running" => {
                    sleep_cancellable(self.config.poll_interval, cancellation)
                        .map_err(|_| UnlimitedError::cancelled("poll"))?;
                }
                "success" => {
                    let result_url = query
                        .data
                        .parse_result_url
                        .as_deref()
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .ok_or_else(|| {
                            UnlimitedError::invalid_response(
                                "poll",
                                "Unlimited-OCR completed without parse_result_url; Markdown-only results cannot provide layout geometry",
                                latest_log_id.clone(),
                            )
                        })?;
                    let payload =
                        self.download_json_result(result_url, cancellation, latest_log_id.clone())?;
                    return Ok(UnlimitedOcrResult {
                        task_id,
                        log_id: latest_log_id,
                        payload,
                    });
                }
                "failed" => {
                    return Err(UnlimitedError::new(
                        "poll",
                        UnlimitedErrorCategory::ProviderFailed,
                        "Unlimited-OCR task failed".to_string(),
                        latest_log_id,
                        None,
                        query.data.task_error,
                    ));
                }
                _ => {
                    return Err(UnlimitedError::invalid_response(
                        "poll",
                        format!("Unlimited-OCR returned unknown task state {state:?}"),
                        latest_log_id,
                    ));
                }
            }
        }
    }

    fn redact_error(&self, mut error: UnlimitedError) -> UnlimitedError {
        error.message = redact_credentials(&error.message, &self.api_key, &self.secret_key);
        error.provider_message = error
            .provider_message
            .map(|message| redact_credentials(&message, &self.api_key, &self.secret_key));
        error
    }

    fn access_token(
        &self,
        cancellation: &CancellationToken,
    ) -> Result<UnlimitedAccessToken, UnlimitedError> {
        let url = format!("{}/oauth/2.0/token", self.config.base_url);
        let response = self.send_with_retry("authenticate", cancellation, || {
            self.http
                .get(&url)
                .query(&[
                    ("grant_type", "client_credentials"),
                    ("client_id", self.api_key.as_str()),
                    ("client_secret", self.secret_key.as_str()),
                ])
                .send()
        })?;
        let status = response.status();
        let payload = response.json::<Value>().map_err(|_| {
            UnlimitedError::invalid_response(
                "authenticate",
                "Baidu OAuth returned invalid JSON",
                None,
            )
        })?;
        if !status.is_success() {
            return Err(UnlimitedError::http(
                "authenticate",
                status,
                provider_message(&payload),
                None,
            ));
        }
        if let Some(error) = payload.get("error").and_then(Value::as_str) {
            return Err(UnlimitedError::new(
                "authenticate",
                UnlimitedErrorCategory::Unauthorized,
                "Baidu rejected the Unlimited-OCR credentials".to_string(),
                None,
                None,
                non_empty(
                    payload
                        .get("error_description")
                        .and_then(Value::as_str)
                        .unwrap_or(error),
                ),
            ));
        }
        let value = payload
            .get("access_token")
            .and_then(Value::as_str)
            .and_then(non_empty)
            .ok_or_else(|| {
                UnlimitedError::invalid_response(
                    "authenticate",
                    "Baidu OAuth response did not include an access token",
                    None,
                )
            })?;
        Ok(UnlimitedAccessToken {
            value,
            expires_in_seconds: payload.get("expires_in").and_then(Value::as_u64),
        })
    }

    fn submit_file_bytes(
        &self,
        file_name: &str,
        bytes: &[u8],
        access_token: &str,
        cancellation: &CancellationToken,
    ) -> Result<UnlimitedTrace<String>, UnlimitedError> {
        cancellation
            .check()
            .map_err(|_| UnlimitedError::cancelled("submit"))?;
        let file_name = non_empty(file_name).ok_or_else(|| {
            UnlimitedError::new(
                "submit",
                UnlimitedErrorCategory::InvalidRequest,
                "Unlimited-OCR source has no valid filename".to_string(),
                None,
                None,
                None,
            )
        })?;
        if bytes.is_empty() {
            return Err(UnlimitedError::new(
                "submit",
                UnlimitedErrorCategory::InvalidRequest,
                "Unlimited-OCR source PDF is empty".to_string(),
                None,
                None,
                None,
            ));
        }
        let file_data = BASE64_STANDARD.encode(bytes);
        let url = format!(
            "{}/rest/2.0/brain/online/v2/unlimited-ocr-parser/task",
            self.config.base_url
        );
        let response = self.send_with_retry("submit", cancellation, || {
            self.http
                .post(&url)
                .query(&[("access_token", access_token)])
                .form(&[
                    ("file_data", file_data.as_str()),
                    ("file_name", file_name.as_str()),
                ])
                .send()
        })?;
        let payload = parse_provider_response("submit", response)?;
        let log_id = provider_log_id(&payload);
        ensure_provider_success("submit", &payload, log_id.clone())?;
        let task_id = payload
            .pointer("/result/task_id")
            .and_then(Value::as_str)
            .and_then(non_empty)
            .ok_or_else(|| {
                UnlimitedError::invalid_response(
                    "submit",
                    "Unlimited-OCR submit response did not include a task ID",
                    log_id.clone(),
                )
            })?;
        Ok(UnlimitedTrace {
            data: task_id,
            log_id,
        })
    }

    fn query_task(
        &self,
        task_id: &str,
        access_token: &str,
        cancellation: &CancellationToken,
    ) -> Result<UnlimitedTrace<UnlimitedPollData>, UnlimitedError> {
        let url = format!(
            "{}/rest/2.0/brain/online/v2/unlimited-ocr-parser/task/query",
            self.config.base_url
        );
        let response = self.send_with_retry("poll", cancellation, || {
            self.http
                .post(&url)
                .query(&[("access_token", access_token)])
                .form(&[("task_id", task_id)])
                .send()
        })?;
        let payload = parse_provider_response("poll", response)?;
        let log_id = provider_log_id(&payload);
        ensure_provider_success("poll", &payload, log_id.clone())?;
        let result = payload.get("result").ok_or_else(|| {
            UnlimitedError::invalid_response(
                "poll",
                "Unlimited-OCR query response did not include a result",
                log_id.clone(),
            )
        })?;
        let status = result
            .get("status")
            .and_then(Value::as_str)
            .and_then(non_empty)
            .ok_or_else(|| {
                UnlimitedError::invalid_response(
                    "poll",
                    "Unlimited-OCR query response did not include task status",
                    log_id.clone(),
                )
            })?;
        Ok(UnlimitedTrace {
            data: UnlimitedPollData {
                status,
                task_error: result
                    .get("task_error")
                    .and_then(Value::as_str)
                    .and_then(non_empty),
                parse_result_url: result
                    .get("parse_result_url")
                    .and_then(Value::as_str)
                    .and_then(non_empty),
            },
            log_id,
        })
    }

    fn download_json_result(
        &self,
        result_url: &str,
        cancellation: &CancellationToken,
        log_id: Option<String>,
    ) -> Result<Value, UnlimitedError> {
        let response = self.send_with_retry("download", cancellation, || {
            self.http.get(result_url).send()
        })?;
        let status = response.status();
        let bytes = response.bytes().map_err(|_| {
            UnlimitedError::new(
                "download",
                UnlimitedErrorCategory::ResultDownloadFailed,
                "could not read the Unlimited-OCR JSON result".to_string(),
                log_id.clone(),
                None,
                None,
            )
        })?;
        if !status.is_success() {
            return Err(UnlimitedError::http(
                "download",
                status,
                "Unlimited-OCR result download failed",
                log_id,
            ));
        }
        serde_json::from_slice(&bytes).map_err(|_| {
            UnlimitedError::new(
                "download",
                UnlimitedErrorCategory::ResultDownloadFailed,
                "Unlimited-OCR result was not valid JSON".to_string(),
                log_id,
                None,
                None,
            )
        })
    }

    fn send_with_retry(
        &self,
        stage: &'static str,
        cancellation: &CancellationToken,
        mut request: impl FnMut() -> reqwest::Result<Response>,
    ) -> Result<Response, UnlimitedError> {
        let attempts = self.config.retry_attempts.max(1);
        for attempt in 0..attempts {
            cancellation
                .check()
                .map_err(|_| UnlimitedError::cancelled(stage))?;
            match request() {
                Ok(response)
                    if (response.status() == StatusCode::TOO_MANY_REQUESTS
                        || response.status().is_server_error())
                        && attempt + 1 < attempts =>
                {
                    sleep_cancellable(
                        retry_delay(self.config.retry_base_delay, attempt),
                        cancellation,
                    )
                    .map_err(|_| UnlimitedError::cancelled(stage))?;
                }
                Ok(response) => return Ok(response),
                Err(error)
                    if (error.is_timeout() || error.is_connect()) && attempt + 1 < attempts =>
                {
                    sleep_cancellable(
                        retry_delay(self.config.retry_base_delay, attempt),
                        cancellation,
                    )
                    .map_err(|_| UnlimitedError::cancelled(stage))?;
                }
                Err(error) => return Err(UnlimitedError::request(stage, &error)),
            }
        }
        Err(UnlimitedError::new(
            stage,
            UnlimitedErrorCategory::ServiceUnavailable,
            format!("Unlimited-OCR request failed after {attempts} attempts"),
            None,
            None,
            None,
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnlimitedProbeResult {
    pub expires_in_seconds: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnlimitedJobProgress {
    pub state: String,
}

#[derive(Debug, Clone)]
pub struct UnlimitedOcrResult {
    pub task_id: String,
    pub log_id: Option<String>,
    pub payload: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnlimitedErrorCategory {
    Unauthorized,
    PermissionDenied,
    InvalidRequest,
    QueueFull,
    TaskNotFound,
    ProviderFailed,
    ServiceUnavailable,
    PollTimeout,
    ResultDownloadFailed,
    InvalidResponse,
    Cancelled,
}

#[derive(Debug, Clone)]
pub struct UnlimitedError {
    pub stage: &'static str,
    pub category: UnlimitedErrorCategory,
    pub message: String,
    pub log_id: Option<String>,
    pub provider_code: Option<i64>,
    pub provider_message: Option<String>,
}

impl UnlimitedError {
    fn new(
        stage: &'static str,
        category: UnlimitedErrorCategory,
        message: String,
        log_id: Option<String>,
        provider_code: Option<i64>,
        provider_message: Option<String>,
    ) -> Self {
        Self {
            stage,
            category,
            message,
            log_id,
            provider_code,
            provider_message,
        }
    }

    fn request(stage: &'static str, error: &reqwest::Error) -> Self {
        let category = if error.is_timeout() || error.is_connect() {
            UnlimitedErrorCategory::ServiceUnavailable
        } else {
            UnlimitedErrorCategory::ProviderFailed
        };
        Self::new(
            stage,
            category,
            format!("Unlimited-OCR request failed during {stage}"),
            None,
            None,
            None,
        )
    }

    fn http(
        stage: &'static str,
        status: StatusCode,
        provider_message: impl AsRef<str>,
        log_id: Option<String>,
    ) -> Self {
        let category = match status {
            StatusCode::UNAUTHORIZED => UnlimitedErrorCategory::Unauthorized,
            StatusCode::FORBIDDEN => UnlimitedErrorCategory::PermissionDenied,
            StatusCode::REQUEST_TIMEOUT | StatusCode::TOO_MANY_REQUESTS => {
                UnlimitedErrorCategory::QueueFull
            }
            _ if status.is_server_error() => UnlimitedErrorCategory::ServiceUnavailable,
            _ => UnlimitedErrorCategory::ProviderFailed,
        };
        Self::new(
            stage,
            category,
            format!("Unlimited-OCR request returned HTTP {}", status.as_u16()),
            log_id,
            Some(i64::from(status.as_u16())),
            sanitized_excerpt(provider_message.as_ref()),
        )
    }

    fn provider(stage: &'static str, code: i64, message: &str, log_id: Option<String>) -> Self {
        let normalized = message.to_ascii_lowercase();
        let category = match code {
            110 | 111 | 401 | 403 => UnlimitedErrorCategory::Unauthorized,
            282007 | 404 => UnlimitedErrorCategory::TaskNotFound,
            18 | 19 | 429 => UnlimitedErrorCategory::QueueFull,
            _ if normalized.contains("quota") || normalized.contains("额度") => {
                UnlimitedErrorCategory::PermissionDenied
            }
            value if value >= 500 => UnlimitedErrorCategory::ServiceUnavailable,
            _ => UnlimitedErrorCategory::ProviderFailed,
        };
        Self::new(
            stage,
            category,
            format!("Unlimited-OCR returned provider code {code}"),
            log_id,
            Some(code),
            sanitized_excerpt(message),
        )
    }

    fn invalid_response(
        stage: &'static str,
        message: impl Into<String>,
        log_id: Option<String>,
    ) -> Self {
        Self::new(
            stage,
            UnlimitedErrorCategory::InvalidResponse,
            message.into(),
            log_id,
            None,
            None,
        )
    }

    fn cancelled(stage: &'static str) -> Self {
        Self::new(
            stage,
            UnlimitedErrorCategory::Cancelled,
            "Unlimited-OCR request was cancelled".to_string(),
            None,
            None,
            None,
        )
    }
}

impl fmt::Display for UnlimitedError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.message)?;
        if let Some(message) = self.provider_message.as_deref() {
            write!(formatter, ": {message}")?;
        }
        if let Some(log_id) = self.log_id.as_deref() {
            write!(formatter, " (log ID {log_id})")?;
        }
        Ok(())
    }
}

impl std::error::Error for UnlimitedError {}

pub fn normalize_unlimited_payload(
    payload: &Value,
    source_path: impl Into<PathBuf>,
    document_id: impl Into<String>,
    target_pages: &[PageGeometry],
) -> Result<DocumentV1> {
    let canonical = canonical_unlimited_payload(payload)?;
    let mut document = normalize_paddle_payload(
        &canonical,
        source_path,
        document_id,
        "PaddleOCR-VL-1.6",
        target_pages,
    )?;
    document.metadata.provider = "unlimited".to_string();
    document.metadata.provider_version = "Unlimited-OCR".to_string();
    document
        .metadata
        .extra
        .insert("structure_confidence".to_string(), json!(0.99));

    let raw_pages = payload
        .get("pages")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("Unlimited-OCR payload is missing pages[].layouts"))?;
    for (page, raw_page) in document.pages.iter_mut().zip(raw_pages) {
        page.metadata
            .insert("ocr_provider".to_string(), json!("unlimited"));
        let raw_layouts = raw_page
            .get("layouts")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let null_layout = Value::Null;
        for block in &mut page.blocks {
            let raw_order = block
                .metadata
                .get("raw_block_order")
                .and_then(Value::as_u64)
                .and_then(|value| usize::try_from(value).ok())
                .unwrap_or(block.order);
            let raw_layout = raw_layouts.get(raw_order).unwrap_or(&null_layout);
            let raw_type = string(raw_layout.get("type"));
            let raw_sub_type = string(raw_layout.get("sub_type"));
            block.source.provider = "unlimited".to_string();
            block.source.raw_label = if raw_sub_type.is_empty() {
                raw_type.clone()
            } else {
                format!("{raw_type}:{raw_sub_type}")
            };
            block
                .metadata
                .insert("raw_unlimited_type".to_string(), json!(raw_type));
            block
                .metadata
                .insert("raw_unlimited_sub_type".to_string(), json!(raw_sub_type));
            if let Some(parent) = raw_layout.get("parent").filter(|value| !value.is_null()) {
                block
                    .metadata
                    .insert("raw_unlimited_parent".to_string(), parent.clone());
            }
            if let Some(children) = raw_layout.get("children").filter(|value| !value.is_null()) {
                block
                    .metadata
                    .insert("raw_unlimited_children".to_string(), children.clone());
            }
            if let Some(structure) = &mut block.structure {
                structure.provider = "unlimited".to_string();
                structure.confidence = 0.99;
                if block.kind == BlockKind::Code {
                    structure.flow = TextFlow::PreserveLines;
                }
            }
            if block.policy.source == PolicyDecisionSource::OcrSemantic {
                block.policy.confidence = 0.99;
            }
            block.tags.push("ocr_provider:unlimited".to_string());
        }
        for region in &mut page.protected_regions {
            region.source = "unlimited".to_string();
            region.confidence = 0.99;
        }
    }
    document.validate()?;
    Ok(document)
}

fn canonical_unlimited_payload(payload: &Value) -> Result<Value> {
    let pages = payload
        .get("pages")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("Unlimited-OCR payload is missing pages[].layouts"))?;
    let pages = pages
        .iter()
        .map(|page| {
            let mut page = page.clone();
            let layouts = page
                .get_mut("layouts")
                .and_then(Value::as_array_mut)
                .ok_or_else(|| anyhow!("Unlimited-OCR page is missing layouts"))?;
            for layout in layouts {
                let raw_type = string(layout.get("type"));
                let sub_type = string(layout.get("sub_type"));
                let mapped = unlimited_block_label(&raw_type, &sub_type);
                if let Some(object) = layout.as_object_mut() {
                    object.insert("type".to_string(), Value::String(mapped.to_string()));
                }
            }
            Ok(page)
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(json!({ "pages": pages }))
}

fn unlimited_block_label(raw_type: &str, sub_type: &str) -> &'static str {
    match raw_type.trim().to_ascii_lowercase().as_str() {
        "head_tail" => match sub_type.trim().to_ascii_lowercase().as_str() {
            "footer" => "footer",
            _ => "header",
        },
        "header" => "header",
        "footer" => "footer",
        "title" | "paragraph_title" => "paragraph_title",
        "doc_title" => "doc_title",
        "abstract" => "abstract",
        "content" => "content",
        "figure_title" => "figure_title",
        "vision_footnote" => "vision_footnote",
        "reference_content" => "reference_content",
        "formula_number" => "formula_number",
        "aside_text" => "aside_text",
        "text" => "text",
        "table" => "table",
        "image" => "image",
        "chart" => "chart",
        "formula" | "display_formula" => "display_formula",
        "algorithm" | "code" => "algorithm",
        "number" | "page_number" => "number",
        "footnote" => "footnote",
        _ => "__unlimited_unknown",
    }
}

#[derive(Debug)]
struct UnlimitedAccessToken {
    value: String,
    expires_in_seconds: Option<u64>,
}

#[derive(Debug)]
struct UnlimitedPollData {
    status: String,
    task_error: Option<String>,
    parse_result_url: Option<String>,
}

#[derive(Debug)]
struct UnlimitedTrace<T> {
    data: T,
    log_id: Option<String>,
}

fn parse_provider_response(
    stage: &'static str,
    response: Response,
) -> Result<Value, UnlimitedError> {
    let status = response.status();
    let bytes = response.bytes().map_err(|_| {
        UnlimitedError::invalid_response(
            stage,
            "Unlimited-OCR response body could not be read",
            None,
        )
    })?;
    let payload = serde_json::from_slice::<Value>(&bytes).map_err(|_| {
        UnlimitedError::invalid_response(stage, "Unlimited-OCR returned invalid JSON", None)
    })?;
    if !status.is_success() {
        return Err(UnlimitedError::http(
            stage,
            status,
            provider_message(&payload),
            provider_log_id(&payload),
        ));
    }
    Ok(payload)
}

fn ensure_provider_success(
    stage: &'static str,
    payload: &Value,
    log_id: Option<String>,
) -> Result<(), UnlimitedError> {
    let code = provider_code(payload).unwrap_or_default();
    if code == 0 {
        return Ok(());
    }
    Err(UnlimitedError::provider(
        stage,
        code,
        provider_message(payload),
        log_id,
    ))
}

fn provider_code(payload: &Value) -> Option<i64> {
    payload.get("error_code").and_then(|value| {
        value
            .as_i64()
            .or_else(|| value.as_str()?.trim().parse::<i64>().ok())
    })
}

fn provider_log_id(payload: &Value) -> Option<String> {
    match payload.get("log_id") {
        Some(Value::String(value)) => non_empty(value),
        Some(Value::Number(value)) => Some(value.to_string()),
        _ => None,
    }
}

fn provider_message(payload: &Value) -> &str {
    payload
        .get("error_msg")
        .and_then(Value::as_str)
        .unwrap_or("Unlimited-OCR provider error")
}

fn retry_delay(base: Duration, attempt: usize) -> Duration {
    base.saturating_mul(1_u32.checked_shl(attempt.min(8) as u32).unwrap_or(u32::MAX))
}

fn sleep_cancellable(
    duration: Duration,
    cancellation: &CancellationToken,
) -> std::result::Result<(), ()> {
    let started = Instant::now();
    while started.elapsed() < duration {
        cancellation.check().map_err(|_| ())?;
        std::thread::sleep(
            duration
                .saturating_sub(started.elapsed())
                .min(Duration::from_millis(50)),
        );
    }
    Ok(())
}

fn string(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(value)) => value.clone(),
        Some(Value::Number(value)) => value.to_string(),
        _ => String::new(),
    }
}

fn non_empty(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn sanitized_excerpt(value: &str) -> Option<String> {
    let value = value.split_whitespace().collect::<Vec<_>>().join(" ");
    (!value.is_empty()).then(|| value.chars().take(180).collect())
}

fn redact_credentials(value: &str, api_key: &str, secret_key: &str) -> String {
    let mut redacted = value.to_string();
    let mut credentials = [api_key, secret_key];
    credentials.sort_by_key(|credential| std::cmp::Reverse(credential.len()));
    for credential in credentials {
        if !credential.is_empty() {
            redacted = redacted.replace(credential, "[REDACTED]");
        }
    }
    redacted
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::thread;

    use super::*;
    use crate::{BlockRole, BoundingBox};

    #[derive(Debug)]
    struct ScriptedResponse {
        status: u16,
        body: String,
    }

    fn serve_scripted_requests(
        listener: TcpListener,
        responses: Vec<ScriptedResponse>,
    ) -> Vec<String> {
        responses
            .into_iter()
            .map(|response| {
                let (mut stream, _) = listener.accept().unwrap();
                let request = read_http_request(&mut stream);
                let reason = match response.status {
                    200 => "OK",
                    401 => "Unauthorized",
                    429 => "Too Many Requests",
                    503 => "Service Unavailable",
                    _ => "Test Response",
                };
                write!(
                    stream,
                    "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    response.status,
                    reason,
                    response.body.len(),
                    response.body
                )
                .unwrap();
                stream.flush().unwrap();
                request
            })
            .collect()
    }

    fn read_http_request(stream: &mut TcpStream) -> String {
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut bytes = Vec::new();
        let mut buffer = [0_u8; 4096];
        loop {
            let read = stream.read(&mut buffer).unwrap();
            if read == 0 {
                break;
            }
            bytes.extend_from_slice(&buffer[..read]);
            let Some(header_end) = bytes
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
                .map(|index| index + 4)
            else {
                continue;
            };
            let headers = String::from_utf8_lossy(&bytes[..header_end]);
            let content_length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })
                .unwrap_or_default();
            if bytes.len() >= header_end + content_length {
                break;
            }
        }
        String::from_utf8_lossy(&bytes).into_owned()
    }

    #[test]
    fn normalizes_unlimited_geometry_and_semantic_regions() {
        let payload = json!({
            "pages": [{
                "meta": {"page_width": 612, "page_height": 792},
                "layouts": [
                    {
                        "layout_id": "header",
                        "type": "head_tail",
                        "sub_type": "header",
                        "text": "Document header",
                        "position": [20, 20, 180, 10],
                        "parent": "root",
                        "children": []
                    },
                    {
                        "layout_id": "field-1",
                        "type": "text",
                        "sub_type": "",
                        "text": "Status: a wrapped logical field",
                        "position": [80, 180, 190, 22],
                        "parent": "root",
                        "children": []
                    },
                    {
                        "layout_id": "field-2",
                        "type": "text",
                        "sub_type": "",
                        "text": "Observation: another logical field",
                        "position": [80, 205, 190, 22],
                        "parent": "root",
                        "children": []
                    },
                    {
                        "layout_id": "chart-1",
                        "type": "chart",
                        "sub_type": "",
                        "text": "",
                        "position": [20, 240, 200, 100],
                        "parent": "root",
                        "children": []
                    }
                ]
            }]
        });

        let document = normalize_unlimited_payload(
            &payload,
            "source.pdf",
            "unlimited-test",
            &[PageGeometry::new(306.0, 396.0)],
        )
        .unwrap();
        let blocks = &document.pages[0].blocks;

        assert_eq!(document.metadata.provider, "unlimited");
        assert_eq!(blocks[0].role, BlockRole::Header);
        assert!(!blocks[0].policy.translate);
        assert_eq!(blocks[1].bbox, BoundingBox::new(40.0, 90.0, 135.0, 101.0));
        assert!(blocks[1].policy.translate);
        assert_eq!(blocks[1].source.provider, "unlimited");
        assert_eq!(blocks[1].structure.as_ref().unwrap().confidence, 0.99);
        assert_eq!(blocks[1].content.lines.len(), 1);
        assert_eq!(blocks[2].content.lines.len(), 1);
        assert!(!blocks[3].policy.translate);
        assert_eq!(blocks[3].source.raw_label, "chart");
        assert_eq!(
            document.pages[0].protected_regions[0].kind,
            crate::ProtectedRegionKind::Chart
        );
        assert_eq!(document.pages[0].protected_regions[0].source, "unlimited");
        assert_eq!(
            unlimited_block_label("paragraph_title", ""),
            "paragraph_title"
        );
        assert_eq!(unlimited_block_label("header", ""), "header");
    }

    #[test]
    fn client_debug_redacts_both_credentials() {
        let client = UnlimitedClient::new(
            UnlimitedClientConfig::default(),
            "secret-api-key",
            "secret-secret-key",
        )
        .unwrap();
        let debug = format!("{client:?}");

        assert!(!debug.contains("secret-api-key"));
        assert!(!debug.contains("secret-secret-key"));
        assert_eq!(debug.matches("[REDACTED]").count(), 2);
    }

    #[test]
    fn client_runs_baidu_protocol_and_retries_transient_failures() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let result_url = format!("{base_url}/result.json");
        let responses = vec![
            ScriptedResponse {
                status: 503,
                body: json!({"error_msg": "temporarily busy"}).to_string(),
            },
            ScriptedResponse {
                status: 200,
                body: json!({
                    "access_token": "test-access-token",
                    "expires_in": 2_592_000
                })
                .to_string(),
            },
            ScriptedResponse {
                status: 200,
                body: json!({
                    "log_id": "submit-log",
                    "result": {"task_id": "task-7"}
                })
                .to_string(),
            },
            ScriptedResponse {
                status: 200,
                body: json!({
                    "log_id": "poll-running-log",
                    "result": {"status": "running"}
                })
                .to_string(),
            },
            ScriptedResponse {
                status: 200,
                body: json!({
                    "log_id": "poll-success-log",
                    "result": {
                        "status": "success",
                        "parse_result_url": result_url
                    }
                })
                .to_string(),
            },
            ScriptedResponse {
                status: 200,
                body: json!({
                    "pages": [{
                        "meta": {"page_width": 100, "page_height": 100},
                        "layouts": []
                    }]
                })
                .to_string(),
            },
        ];
        let server = thread::spawn(move || serve_scripted_requests(listener, responses));
        let client = UnlimitedClient::new(
            UnlimitedClientConfig {
                base_url,
                request_timeout: Duration::from_secs(3),
                download_timeout: Duration::from_secs(3),
                poll_interval: Duration::from_millis(1),
                poll_timeout: Duration::from_secs(3),
                retry_attempts: 2,
                retry_base_delay: Duration::from_millis(1),
            },
            "api-key",
            "secret-key",
        )
        .unwrap();
        let mut states = Vec::new();

        let result = client
            .run_document(
                &Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"),
                &CancellationToken::new(),
                |progress| states.push(progress.state),
            )
            .unwrap();
        let requests = server.join().unwrap();

        assert_eq!(result.task_id, "task-7");
        assert_eq!(result.log_id.as_deref(), Some("poll-success-log"));
        assert!(result.payload.get("pages").is_some());
        assert_eq!(states, ["running", "success"]);
        assert_eq!(requests.len(), 6);
        assert!(
            requests[0].starts_with("GET /oauth/2.0/token?"),
            "{}",
            requests[0].lines().next().unwrap_or_default()
        );
        assert!(requests[1].contains("client_id=api-key"));
        assert!(requests[1].contains("client_secret=secret-key"));
        assert!(requests[2].contains("/unlimited-ocr-parser/task?access_token=test-access-token"));
        assert!(requests[2].contains("file_name=Cargo.toml"));
        assert!(requests[3].contains("/unlimited-ocr-parser/task/query?access_token="));
        assert!(requests[3].contains("task_id=task-7"));
        assert!(requests[5].starts_with("GET /result.json "));
    }

    #[test]
    fn public_errors_are_cancelled_early_and_never_expose_credentials() {
        let client = UnlimitedClient::new(
            UnlimitedClientConfig::default(),
            "api-key-that-must-not-leak",
            "secret-key-that-must-not-leak",
        )
        .unwrap();
        let cancellation = CancellationToken::new();
        cancellation.cancel();

        let cancelled = client
            .run_document(Path::new("unused.pdf"), &cancellation, |_| {})
            .unwrap_err();
        assert_eq!(cancelled.category, UnlimitedErrorCategory::Cancelled);

        let redacted = client.redact_error(UnlimitedError::new(
            "authenticate",
            UnlimitedErrorCategory::Unauthorized,
            "rejected api-key-that-must-not-leak".to_string(),
            None,
            Some(401),
            Some("bad secret-key-that-must-not-leak".to_string()),
        ));
        let display = redacted.to_string();
        assert!(!display.contains("api-key-that-must-not-leak"));
        assert!(!display.contains("secret-key-that-must-not-leak"));
        assert_eq!(display.matches("[REDACTED]").count(), 2);
    }

    #[test]
    fn provider_errors_have_actionable_categories() {
        assert_eq!(
            UnlimitedError::provider("poll", 282007, "task missing", None).category,
            UnlimitedErrorCategory::TaskNotFound
        );
        assert_eq!(
            UnlimitedError::provider("submit", 18, "queue full", None).category,
            UnlimitedErrorCategory::QueueFull
        );
        assert_eq!(
            UnlimitedError::provider("submit", 6, "quota exhausted", None).category,
            UnlimitedErrorCategory::PermissionDenied
        );
    }

    #[test]
    fn normalizes_full_provider_fixture_when_requested() {
        let Some(path) = std::env::var_os("GLYPHST_UNLIMITED_FIXTURE").map(PathBuf::from) else {
            return;
        };
        let payload: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        let pages = payload["pages"].as_array().unwrap();
        let target_pages = pages
            .iter()
            .map(|page| {
                PageGeometry::new(
                    page.pointer("/meta/page_width")
                        .and_then(Value::as_f64)
                        .unwrap_or(612.0) as f32,
                    page.pointer("/meta/page_height")
                        .and_then(Value::as_f64)
                        .unwrap_or(792.0) as f32,
                )
            })
            .collect::<Vec<_>>();
        let document =
            normalize_unlimited_payload(&payload, "source.pdf", "unlimited-fixture", &target_pages)
                .unwrap();

        assert_eq!(document.pages.len(), pages.len());
        assert!(
            document
                .pages
                .iter()
                .flat_map(|page| &page.blocks)
                .any(|block| block.policy.translate)
        );
        document.validate().unwrap();

        let Some(source_path) =
            std::env::var_os("GLYPHST_UNLIMITED_FIXTURE_SOURCE").map(PathBuf::from)
        else {
            return;
        };
        let cancellation = CancellationToken::new();
        let native =
            crate::extract_native_document(&source_path, "unlimited-native", &cancellation)
                .unwrap();
        let fusion = crate::fuse_native_with_ocr_report(native, &document);
        eprintln!(
            "Unlimited fixture fusion report: {}",
            serde_json::to_string_pretty(&fusion.report).unwrap()
        );
        fusion.document.validate().unwrap();
        assert!(
            fusion
                .report
                .pages
                .iter()
                .any(|page| page.accepted_splits > 0),
            "fine Unlimited regions should split at least one coarse native block"
        );
    }
}
