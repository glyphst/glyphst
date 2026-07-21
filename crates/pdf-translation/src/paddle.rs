use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use reqwest::StatusCode;
use reqwest::blocking::{Client, Response, multipart};
use reqwest::header::AUTHORIZATION;
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Value, json};

use crate::{
    Block, BlockContent, BlockKind, BlockRole, BoundingBox, CancellationToken, ContinuationHint,
    ContinuationScope, DocumentMetadata, DocumentV1, Line, Page, PageGeometry, Segment,
    SegmentKind, SourceProvenance, TextStyle, TranslationPolicy,
};

static PROBE_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone)]
pub struct PaddleClientConfig {
    pub base_url: String,
    pub request_timeout: Duration,
    pub download_timeout: Duration,
    pub poll_interval: Duration,
    pub poll_timeout: Duration,
    pub retry_attempts: usize,
    pub retry_base_delay: Duration,
}

impl Default for PaddleClientConfig {
    fn default() -> Self {
        Self {
            base_url: "https://paddleocr.aistudio-app.com".to_string(),
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
pub struct PaddleClient {
    config: PaddleClientConfig,
    token: String,
    http: Client,
}

impl fmt::Debug for PaddleClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PaddleClient")
            .field("base_url", &self.config.base_url)
            .field("token", &"[REDACTED]")
            .finish_non_exhaustive()
    }
}

impl PaddleClient {
    pub fn new(config: PaddleClientConfig, token: impl Into<String>) -> Result<Self> {
        let mut config = config;
        config.base_url = config.base_url.trim().trim_end_matches('/').to_string();
        if config.base_url.is_empty() {
            config.base_url = PaddleClientConfig::default().base_url;
        }
        let token = token.into();
        if token.trim().is_empty() {
            bail!("PaddleOCR token is not configured");
        }
        let http = Client::builder()
            .connect_timeout(config.request_timeout)
            .timeout(config.request_timeout)
            .build()
            .context("failed to create PaddleOCR HTTP client")?;
        Ok(Self {
            config,
            token,
            http,
        })
    }

    pub fn probe(
        &self,
        cancellation: &CancellationToken,
    ) -> Result<PaddleProbeResult, PaddleError> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let sequence = PROBE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let job_id = format!("glyphst-token-probe-{now:x}-{sequence:x}");
        let url = format!("{}/api/v2/ocr/jobs/{job_id}", self.config.base_url);
        let response = self.send_with_retry("probe", cancellation, || {
            self.http
                .get(&url)
                .header(AUTHORIZATION, self.auth_header())
                .send()
        })?;
        let status = response.status();
        let body = response
            .bytes()
            .map_err(|error| PaddleError::request("probe", &error))?;
        let envelope = serde_json::from_slice::<PaddlePollEnvelope>(&body).ok();
        let trace_id = envelope.as_ref().and_then(|value| non_empty(&value.log_id));

        if status == StatusCode::NOT_FOUND {
            return Ok(PaddleProbeResult { trace_id });
        }
        if !status.is_success() {
            return Err(PaddleError::http(
                "probe",
                status,
                &String::from_utf8_lossy(&body),
                trace_id,
            ));
        }
        match envelope {
            Some(envelope) if matches!(envelope.error_code, 0 | 404 | 11001) => {
                Ok(PaddleProbeResult {
                    trace_id: non_empty(&envelope.log_id),
                })
            }
            Some(envelope) => Err(PaddleError::provider(
                "probe",
                envelope.error_code,
                &envelope.error_msg,
                non_empty(&envelope.log_id),
            )),
            None => Err(PaddleError::invalid_response(
                "probe",
                "PaddleOCR returned an invalid probe response",
                trace_id,
            )),
        }
    }

    pub fn run_document(
        &self,
        file_path: &Path,
        model: &str,
        cancellation: &CancellationToken,
        mut on_progress: impl FnMut(PaddleJobProgress),
    ) -> Result<PaddleOcrResult, PaddleError> {
        cancellation
            .check()
            .map_err(|_| PaddleError::cancelled("submit"))?;
        let trace = self.submit_local_file(file_path, model, cancellation)?;
        let job_id = trace.data;
        let started = Instant::now();
        let mut latest_trace_id = trace.trace_id;

        loop {
            cancellation
                .check()
                .map_err(|_| PaddleError::cancelled("poll"))?;
            if started.elapsed() >= self.config.poll_timeout {
                return Err(PaddleError::new(
                    "poll",
                    PaddleErrorCategory::PollTimeout,
                    format!("timed out waiting for PaddleOCR job {job_id}"),
                    latest_trace_id,
                    None,
                    None,
                ));
            }
            let trace = self.query_job(&job_id, cancellation)?;
            latest_trace_id = trace.trace_id.or(latest_trace_id);
            let state = trace.data.state.trim().to_ascii_lowercase();
            let progress = trace.data.extract_progress.as_ref();
            on_progress(PaddleJobProgress {
                state: state.clone(),
                extracted_pages: progress.and_then(|value| value.extracted_pages),
                total_pages: progress.and_then(|value| value.total_pages),
            });
            match state.as_str() {
                "pending" | "running" => {
                    sleep_cancellable(self.config.poll_interval, cancellation)
                        .map_err(|_| PaddleError::cancelled("poll"))?;
                }
                "done" => {
                    let json_url = trace
                        .data
                        .result_url
                        .as_ref()
                        .map(|value| value.json_url.trim())
                        .filter(|value| !value.is_empty())
                        .ok_or_else(|| {
                            PaddleError::invalid_response(
                                "poll",
                                "PaddleOCR completed without resultUrl.jsonUrl",
                                latest_trace_id.clone(),
                            )
                        })?;
                    let payload = self.download_jsonl_result(json_url, cancellation)?;
                    return Ok(PaddleOcrResult {
                        job_id,
                        trace_id: latest_trace_id,
                        payload,
                    });
                }
                "failed" => {
                    return Err(PaddleError::new(
                        "poll",
                        PaddleErrorCategory::ProviderFailed,
                        "PaddleOCR job failed".to_string(),
                        latest_trace_id,
                        None,
                        non_empty(&trace.data.error_msg),
                    ));
                }
                _ => {
                    return Err(PaddleError::invalid_response(
                        "poll",
                        format!("PaddleOCR returned unknown job state {state:?}"),
                        latest_trace_id,
                    ));
                }
            }
        }
    }

    fn submit_local_file(
        &self,
        file_path: &Path,
        model: &str,
        cancellation: &CancellationToken,
    ) -> Result<PaddleTrace<String>, PaddleError> {
        let file_name = file_path
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| {
                PaddleError::invalid_response("submit", "invalid upload filename", None)
            })?
            .to_string();
        let file_bytes = std::fs::read(file_path).map_err(|error| {
            PaddleError::new(
                "submit",
                PaddleErrorCategory::InvalidRequest,
                format!("failed to read source PDF: {error}"),
                None,
                None,
                None,
            )
        })?;
        let url = format!("{}/api/v2/ocr/jobs", self.config.base_url);
        let model = normalize_model_name(model);
        let optional_payload = json!({
            "useDocOrientationClassify": false,
            "useDocUnwarping": false,
            "useChartRecognition": false
        })
        .to_string();
        let response = self.send_with_retry("submit", cancellation, || {
            let part = multipart::Part::bytes(file_bytes.clone()).file_name(file_name.clone());
            let form = multipart::Form::new()
                .text("model", model.clone())
                .text("optionalPayload", optional_payload.clone())
                .part("file", part);
            self.http
                .post(&url)
                .header(AUTHORIZATION, self.auth_header())
                .multipart(form)
                .send()
        })?;
        let envelope: PaddleSubmitEnvelope = parse_json_response("submit", response)?;
        if envelope.error_code != 0 {
            return Err(PaddleError::provider(
                "submit",
                envelope.error_code,
                &envelope.error_msg,
                non_empty(&envelope.log_id),
            ));
        }
        let trace_id = non_empty(&envelope.log_id);
        let job_id = envelope
            .data
            .map(|value| value.job_id)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                PaddleError::invalid_response(
                    "submit",
                    "PaddleOCR submit response did not include a job ID",
                    trace_id.clone(),
                )
            })?;
        Ok(PaddleTrace {
            data: job_id,
            trace_id,
        })
    }

    fn query_job(
        &self,
        job_id: &str,
        cancellation: &CancellationToken,
    ) -> Result<PaddleTrace<PaddlePollData>, PaddleError> {
        let url = format!("{}/api/v2/ocr/jobs/{job_id}", self.config.base_url);
        let response = self.send_with_retry("poll", cancellation, || {
            self.http
                .get(&url)
                .header(AUTHORIZATION, self.auth_header())
                .send()
        })?;
        let envelope: PaddlePollEnvelope = parse_json_response("poll", response)?;
        if envelope.error_code != 0 {
            return Err(PaddleError::provider(
                "poll",
                envelope.error_code,
                &envelope.error_msg,
                non_empty(&envelope.log_id),
            ));
        }
        Ok(PaddleTrace {
            data: envelope.data.unwrap_or_default(),
            trace_id: non_empty(&envelope.log_id),
        })
    }

    fn download_jsonl_result(
        &self,
        jsonl_url: &str,
        cancellation: &CancellationToken,
    ) -> Result<Value, PaddleError> {
        let response = self.send_with_retry("download", cancellation, || {
            self.http
                .get(jsonl_url)
                .timeout(self.config.download_timeout)
                .send()
        })?;
        let status = response.status();
        let text = response
            .text()
            .map_err(|error| PaddleError::request("download", &error))?;
        if !status.is_success() {
            return Err(PaddleError::http("download", status, &text, None));
        }
        combine_jsonl_payload(&text)
    }

    fn send_with_retry(
        &self,
        stage: &'static str,
        cancellation: &CancellationToken,
        mut action: impl FnMut() -> reqwest::Result<Response>,
    ) -> Result<Response, PaddleError> {
        let attempts = self.config.retry_attempts.max(1);
        let mut last_error = None;
        for attempt in 1..=attempts {
            cancellation
                .check()
                .map_err(|_| PaddleError::cancelled(stage))?;
            match action() {
                Ok(response)
                    if (response.status() == StatusCode::TOO_MANY_REQUESTS
                        || response.status().is_server_error())
                        && attempt < attempts =>
                {
                    sleep_cancellable(self.config.retry_base_delay * attempt as u32, cancellation)
                        .map_err(|_| PaddleError::cancelled(stage))?;
                }
                Ok(response) => return Ok(response),
                Err(error) => {
                    let retryable = error.is_connect() || error.is_timeout() || error.is_body();
                    last_error = Some(error);
                    if !retryable || attempt >= attempts {
                        break;
                    }
                    sleep_cancellable(self.config.retry_base_delay * attempt as u32, cancellation)
                        .map_err(|_| PaddleError::cancelled(stage))?;
                }
            }
        }
        let error = last_error.ok_or_else(|| {
            PaddleError::new(
                stage,
                PaddleErrorCategory::ServiceUnavailable,
                "PaddleOCR request failed after retries".to_string(),
                None,
                None,
                None,
            )
        })?;
        Err(PaddleError::request(stage, &error))
    }

    fn auth_header(&self) -> String {
        format!("bearer {}", self.token.trim())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaddleProbeResult {
    pub trace_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaddleJobProgress {
    pub state: String,
    pub extracted_pages: Option<i64>,
    pub total_pages: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct PaddleOcrResult {
    pub job_id: String,
    pub trace_id: Option<String>,
    pub payload: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaddleErrorCategory {
    Cancelled,
    Unauthorized,
    PermissionDenied,
    InvalidRequest,
    QueueFull,
    ServiceUnavailable,
    TaskNotFound,
    PollTimeout,
    ProviderFailed,
    InvalidResponse,
    ResultDownloadFailed,
}

#[derive(Debug, Clone)]
pub struct PaddleError {
    pub stage: &'static str,
    pub category: PaddleErrorCategory,
    pub detail: String,
    pub trace_id: Option<String>,
    pub provider_code: Option<String>,
    pub provider_message: Option<String>,
}

impl PaddleError {
    fn new(
        stage: &'static str,
        category: PaddleErrorCategory,
        detail: String,
        trace_id: Option<String>,
        provider_code: Option<String>,
        provider_message: Option<String>,
    ) -> Self {
        Self {
            stage,
            category,
            detail,
            trace_id,
            provider_code,
            provider_message,
        }
    }

    fn request(stage: &'static str, error: &reqwest::Error) -> Self {
        Self::new(
            stage,
            PaddleErrorCategory::ServiceUnavailable,
            error.to_string(),
            None,
            None,
            None,
        )
    }

    fn http(stage: &'static str, status: StatusCode, body: &str, trace_id: Option<String>) -> Self {
        let parsed = serde_json::from_str::<PaddleErrorEnvelope>(body).ok();
        let provider_code = parsed
            .as_ref()
            .and_then(|value| value.error_code)
            .map(|value| value.to_string());
        let provider_message = parsed
            .as_ref()
            .and_then(|value| non_empty(&value.error_msg))
            .or_else(|| sanitized_excerpt(body));
        let trace_id =
            trace_id.or_else(|| parsed.as_ref().and_then(|value| non_empty(&value.log_id)));
        let category = match status {
            StatusCode::UNAUTHORIZED => PaddleErrorCategory::Unauthorized,
            StatusCode::FORBIDDEN => PaddleErrorCategory::PermissionDenied,
            StatusCode::BAD_REQUEST
            | StatusCode::METHOD_NOT_ALLOWED
            | StatusCode::UNPROCESSABLE_ENTITY => PaddleErrorCategory::InvalidRequest,
            StatusCode::TOO_MANY_REQUESTS => PaddleErrorCategory::QueueFull,
            StatusCode::NOT_FOUND => PaddleErrorCategory::TaskNotFound,
            _ if status.is_server_error() => PaddleErrorCategory::ServiceUnavailable,
            _ => PaddleErrorCategory::ProviderFailed,
        };
        Self::new(
            stage,
            category,
            format!("PaddleOCR request returned HTTP {}", status.as_u16()),
            trace_id,
            provider_code,
            provider_message,
        )
    }

    fn provider(stage: &'static str, code: i64, message: &str, trace_id: Option<String>) -> Self {
        let category = match code {
            401 | 403 => PaddleErrorCategory::Unauthorized,
            404 | 11001 => PaddleErrorCategory::TaskNotFound,
            10010 | 429 => PaddleErrorCategory::QueueFull,
            value if value >= 500 => PaddleErrorCategory::ServiceUnavailable,
            _ => PaddleErrorCategory::ProviderFailed,
        };
        Self::new(
            stage,
            category,
            format!("PaddleOCR returned provider code {code}"),
            trace_id,
            Some(code.to_string()),
            non_empty(message),
        )
    }

    fn invalid_response(
        stage: &'static str,
        detail: impl Into<String>,
        trace_id: Option<String>,
    ) -> Self {
        Self::new(
            stage,
            PaddleErrorCategory::InvalidResponse,
            detail.into(),
            trace_id,
            None,
            None,
        )
    }

    fn cancelled(stage: &'static str) -> Self {
        Self::new(
            stage,
            PaddleErrorCategory::Cancelled,
            "PDF translation was cancelled".to_string(),
            None,
            None,
            None,
        )
    }
}

impl fmt::Display for PaddleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = self
            .provider_message
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or(&self.detail);
        if let Some(trace_id) = self.trace_id.as_deref() {
            write!(f, "{message} (trace ID: {trace_id})")
        } else {
            f.write_str(message)
        }
    }
}

impl std::error::Error for PaddleError {}

pub fn normalize_paddle_payload(
    payload: &Value,
    source_path: impl Into<PathBuf>,
    document_id: impl Into<String>,
    provider_version: impl Into<String>,
    target_pages: &[PageGeometry],
) -> Result<DocumentV1> {
    let source_path = source_path.into();
    let layout_results = payload
        .get("layoutParsingResults")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("PaddleOCR payload is missing layoutParsingResults"))?;
    let page_metadata = payload
        .pointer("/dataInfo/pages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut pages = Vec::with_capacity(layout_results.len());

    for (page_index, page_payload) in layout_results.iter().enumerate() {
        let pruned = page_payload.get("prunedResult").unwrap_or(page_payload);
        let raw_blocks = pruned
            .get("parsing_res_list")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let page_meta = page_metadata.get(page_index).unwrap_or(&Value::Null);
        let provider_width = number(page_meta.get("width"))
            .or_else(|| number(pruned.get("width")))
            .unwrap_or(0.0);
        let provider_height = number(page_meta.get("height"))
            .or_else(|| number(pruned.get("height")))
            .unwrap_or(0.0);
        let geometry = target_pages.get(page_index).copied().unwrap_or_else(|| {
            PageGeometry::new(provider_width.max(1.0), provider_height.max(1.0))
        });
        let scale_x = if provider_width > 0.0 {
            geometry.width / provider_width
        } else {
            1.0
        };
        let scale_y = if provider_height > 0.0 {
            geometry.height / provider_height
        } else {
            1.0
        };
        let body_flow_start = body_flow_start_order(&raw_blocks);
        let mut previous_anchor = BlockRole::Unknown;
        let mut blocks = Vec::with_capacity(raw_blocks.len());
        for (order, raw_block) in raw_blocks.iter().enumerate() {
            let raw_label = string(raw_block.get("block_label"));
            let text = string(raw_block.get("block_content")).trim().to_string();
            let (kind, role, mut policy, tags) =
                classify_paddle_block(&raw_label, &text, order, body_flow_start, previous_anchor);
            if matches!(
                kind,
                BlockKind::Text | BlockKind::Image | BlockKind::Table | BlockKind::Formula
            ) {
                previous_anchor = role;
            }
            let raw_bbox = parse_bbox(raw_block.get("block_bbox")).unwrap_or_default();
            let page_bounds = BoundingBox::new(0.0, 0.0, geometry.width, geometry.height);
            let scaled_bbox = raw_bbox.scale(scale_x, scale_y).clamp_to(page_bounds);
            let bbox = tighten_text_bbox(scaled_bbox, &text, kind, role);
            if bbox.area() < 1.0 && policy.translate {
                policy = TranslationPolicy::preserve("provider_text_missing_bbox");
            }
            let lines = build_lines(bbox, &text, &raw_label, kind, role);
            let segments = lines
                .iter()
                .flat_map(|line| line.segments.iter().cloned())
                .collect();
            let mut metadata = BTreeMap::new();
            insert_raw_metadata(&mut metadata, raw_block);
            metadata.insert("provider_width".to_string(), json!(provider_width));
            metadata.insert("provider_height".to_string(), json!(provider_height));
            let content = BlockContent {
                text: text.clone(),
                lines,
                segments,
                markdown: None,
                html: (kind == BlockKind::Table && text.contains('<')).then_some(text.clone()),
            };
            blocks.push(Block {
                id: format!("p{:03}-b{order:04}", page_index + 1),
                page_index,
                order,
                reading_order: order,
                kind,
                role,
                bbox,
                content,
                policy,
                continuation: None,
                source: SourceProvenance {
                    provider: "paddle".to_string(),
                    raw_label,
                    raw_bbox: raw_bbox.scale(scale_x, scale_y),
                    raw_path: None,
                },
                tags,
                metadata,
            });
        }
        let mut metadata = BTreeMap::new();
        metadata.insert("provider_width".to_string(), json!(provider_width));
        metadata.insert("provider_height".to_string(), json!(provider_height));
        if let Some(markdown) = page_payload
            .pointer("/markdown/text")
            .and_then(Value::as_str)
        {
            metadata.insert("provider_markdown".to_string(), json!(markdown));
        }
        pages.push(Page {
            page_index,
            geometry,
            blocks,
            metadata,
        });
    }
    assign_provider_continuations(&mut pages);

    let document = DocumentV1::new(
        DocumentMetadata {
            document_id: document_id.into(),
            source_path,
            provider: "paddle".to_string(),
            provider_version: provider_version.into(),
            extra: BTreeMap::new(),
        },
        pages,
    );
    document.validate()?;
    Ok(document)
}

pub fn normalize_model_name(model: &str) -> String {
    match model.trim().to_ascii_lowercase().as_str() {
        "" | "paddle-ocr-vl" | "paddleocr-vl" | "paddle-ocr-vl-1.6" | "paddleocr-vl-1.6" => {
            "PaddleOCR-VL-1.6".to_string()
        }
        "paddle-ocr-vl-1.5" | "paddleocr-vl-1.5" => "PaddleOCR-VL-1.5".to_string(),
        _ => model.trim().to_string(),
    }
}

fn classify_paddle_block(
    raw_label: &str,
    text: &str,
    order: usize,
    body_flow_start: Option<usize>,
    previous_anchor: BlockRole,
) -> (BlockKind, BlockRole, TranslationPolicy, Vec<String>) {
    let label = raw_label.trim().to_ascii_lowercase();
    if label == "text"
        && body_flow_start.is_some_and(|start| order < start)
        && !text.trim().is_empty()
    {
        return preserved_text(
            BlockRole::Metadata,
            "provider_front_matter_text",
            &["metadata"],
        );
    }
    if label == "text" && looks_like_metadata(text) {
        return preserved_text(BlockRole::Metadata, "provider_metadata_text", &["metadata"]);
    }
    match label.as_str() {
        "doc_title" => translated_text(BlockRole::Title, "provider_title_candidate", &["title"]),
        "abstract" => translated_text(
            BlockRole::Abstract,
            "provider_body_whitelist:abstract",
            &["abstract"],
        ),
        "text" => translated_text(BlockRole::Body, "provider_body_whitelist:body", &[]),
        "paragraph_title" if looks_like_ancillary_heading(text) => preserved_text(
            BlockRole::Metadata,
            "provider_ancillary_tail_heading",
            &["metadata"],
        ),
        "paragraph_title" => translated_text(
            BlockRole::Heading,
            "provider_heading_candidate",
            &["heading"],
        ),
        "content" => translated_text(
            BlockRole::TableOfContents,
            "provider_toc_whitelist:content",
            &["table_of_contents", "toc"],
        ),
        "figure_title" => translated_text(
            BlockRole::FigureCaption,
            "provider_caption_whitelist:figure_caption",
            &["caption", "figure_caption"],
        ),
        "vision_footnote" => {
            let role = match previous_anchor {
                BlockRole::Table => BlockRole::TableCaption,
                BlockRole::Image => BlockRole::FigureCaption,
                _ => BlockRole::Footnote,
            };
            translated_text(
                role,
                "provider_footnote_whitelist:vision_footnote",
                &["footnote"],
            )
        }
        "reference_content" => preserved_text(
            BlockRole::Reference,
            "provider_reference_entry",
            &["reference_entry", "reference_zone"],
        ),
        "formula_number" => preserved_text(
            BlockRole::FormulaNumber,
            "provider_formula_number",
            &["formula_number"],
        ),
        "header" => preserved_text(BlockRole::Header, "provider_header", &[]),
        "footer" => preserved_text(BlockRole::Footer, "provider_footer", &[]),
        "footnote" => preserved_text(
            BlockRole::Footnote,
            "provider_non_body:footnote",
            &["footnote"],
        ),
        "aside_text" => preserved_text(BlockRole::Metadata, "provider_metadata", &["metadata"]),
        "number" => preserved_text(BlockRole::PageNumber, "provider_page_number", &[]),
        "table" => (
            BlockKind::Table,
            BlockRole::Table,
            TranslationPolicy::preserve("provider_non_text:table"),
            vec!["table".to_string()],
        ),
        "image" | "chart" | "header_image" | "footer_image" => (
            BlockKind::Image,
            BlockRole::Image,
            TranslationPolicy::preserve("provider_non_text:image"),
            vec!["image".to_string()],
        ),
        "algorithm" => (
            BlockKind::Code,
            BlockRole::Code,
            TranslationPolicy::preserve("provider_non_text:code"),
            vec!["code".to_string()],
        ),
        "display_formula" | "formula" => (
            BlockKind::Formula,
            BlockRole::Formula,
            TranslationPolicy::preserve("provider_non_text:formula"),
            vec!["formula".to_string()],
        ),
        _ => (
            BlockKind::Unknown,
            BlockRole::Unknown,
            TranslationPolicy::preserve("provider_unknown"),
            vec!["unknown".to_string()],
        ),
    }
}

fn translated_text(
    role: BlockRole,
    reason: &str,
    tags: &[&str],
) -> (BlockKind, BlockRole, TranslationPolicy, Vec<String>) {
    (
        BlockKind::Text,
        role,
        TranslationPolicy::translate(reason),
        tags.iter().map(|value| (*value).to_string()).collect(),
    )
}

fn preserved_text(
    role: BlockRole,
    reason: &str,
    tags: &[&str],
) -> (BlockKind, BlockRole, TranslationPolicy, Vec<String>) {
    (
        BlockKind::Text,
        role,
        TranslationPolicy::preserve(reason),
        tags.iter().map(|value| (*value).to_string()).collect(),
    )
}

fn body_flow_start_order(blocks: &[Value]) -> Option<usize> {
    let mut front_matter_end = None;
    for (order, block) in blocks.iter().enumerate() {
        let label = string(block.get("block_label")).to_ascii_lowercase();
        let text = compact(&string(block.get("block_content"))).to_ascii_lowercase();
        if matches!(label.as_str(), "doc_title" | "abstract")
            || (label == "paragraph_title" && text == "abstract")
        {
            front_matter_end = Some(order);
        }
    }
    let front_matter_end = front_matter_end?;
    blocks
        .iter()
        .enumerate()
        .skip(front_matter_end + 1)
        .find_map(|(order, block)| {
            let label = string(block.get("block_label")).to_ascii_lowercase();
            let text = compact(&string(block.get("block_content")));
            ((label == "paragraph_title"
                && !text.eq_ignore_ascii_case("abstract")
                && !looks_like_ancillary_heading(&text)
                && !text.is_empty())
                || (matches!(label.as_str(), "text" | "abstract")
                    && !text.is_empty()
                    && !looks_like_metadata(&text)))
            .then_some(order)
        })
}

fn looks_like_metadata(text: &str) -> bool {
    let text = compact(text).to_ascii_lowercase();
    [
        "keyword",
        "doi:",
        "cite this article as:",
        "submit your manuscript here",
        "open access",
        "copyright",
        "authors declare",
        "competing interests",
        "funded by",
        "received:",
        "accepted:",
        "published:",
        "e-mail:",
        "orcid",
    ]
    .iter()
    .any(|cue| text.starts_with(cue) || text.contains(cue))
}

fn looks_like_ancillary_heading(text: &str) -> bool {
    matches!(
        compact(text).to_ascii_lowercase().as_str(),
        "competing interests" | "authors' contributions" | "acknowledgments" | "references"
    )
}

fn build_lines(
    bbox: BoundingBox,
    text: &str,
    raw_label: &str,
    kind: BlockKind,
    role: BlockRole,
) -> Vec<Line> {
    let explicit: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    let chunks = if explicit.len() >= 2 {
        explicit.into_iter().map(str::to_string).collect()
    } else if kind == BlockKind::Text && matches!(role, BlockRole::Body | BlockRole::Heading) {
        pseudo_line_chunks(bbox, text)
    } else if text.trim().is_empty() {
        Vec::new()
    } else {
        vec![text.trim().to_string()]
    };
    if chunks.is_empty() {
        return Vec::new();
    }
    let line_height = bbox.height() / chunks.len() as f32;
    let chunk_count = chunks.len();
    chunks
        .into_iter()
        .enumerate()
        .map(|(index, text)| {
            let line_bbox = BoundingBox::new(
                bbox.x0,
                bbox.y0 + line_height * index as f32,
                bbox.x1,
                if index + 1 == chunk_count {
                    bbox.y1
                } else {
                    bbox.y0 + line_height * (index + 1) as f32
                },
            );
            Line {
                bbox: line_bbox,
                segments: split_inline_segments(&text, raw_label, line_bbox),
                text,
            }
        })
        .collect()
}

fn pseudo_line_chunks(bbox: BoundingBox, text: &str) -> Vec<String> {
    let compact_length = text.chars().filter(|value| !value.is_whitespace()).count();
    if compact_length < 72 || bbox.height() < 24.2 || bbox.width() <= 0.0 {
        return vec![text.trim().to_string()];
    }
    let chars_per_line = (bbox.width() / 5.2).floor().max(12.0) as usize;
    let predicted = compact_length.div_ceil(chars_per_line).max(2);
    let max_by_height = (bbox.height() / 11.0).floor().max(1.0) as usize;
    let line_count = predicted.min(max_by_height).max(1);
    if line_count <= 1 {
        return vec![text.trim().to_string()];
    }
    let target = compact_length.div_ceil(line_count).max(10);
    let mut chunks = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        if !current.is_empty() && current.chars().count() >= target && chunks.len() + 1 < line_count
        {
            chunks.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

fn tighten_text_bbox(
    bbox: BoundingBox,
    text: &str,
    kind: BlockKind,
    role: BlockRole,
) -> BoundingBox {
    if kind != BlockKind::Text || !matches!(role, BlockRole::Body | BlockRole::Heading) {
        return bbox;
    }
    let chunks = pseudo_line_chunks(bbox, text);
    if chunks.len() <= 1 {
        return bbox;
    }
    let target_height = (chunks.len() as f32 * 12.0 * 1.08)
        .max(24.0)
        .min(bbox.height());
    BoundingBox::new(bbox.x0, bbox.y0, bbox.x1, bbox.y0 + target_height)
}

fn split_inline_segments(text: &str, raw_label: &str, bbox: BoundingBox) -> Vec<Segment> {
    if matches!(raw_label, "display_formula" | "formula") {
        return vec![Segment {
            kind: SegmentKind::Formula,
            text: text.to_string(),
            bbox,
            style: TextStyle::default(),
        }];
    }
    let mut segments = Vec::new();
    let mut cursor = 0usize;
    while cursor < text.len() {
        let rest = &text[cursor..];
        let Some(relative_start) = rest.find('$') else {
            push_text_segment(&mut segments, rest, bbox);
            break;
        };
        push_text_segment(&mut segments, &rest[..relative_start], bbox);
        let formula_start = cursor + relative_start;
        let after_start = formula_start + 1;
        let Some(relative_end) = text[after_start..].find('$') else {
            push_text_segment(&mut segments, &text[formula_start..], bbox);
            break;
        };
        let formula_end = after_start + relative_end + 1;
        segments.push(Segment {
            kind: SegmentKind::Formula,
            text: text[formula_start..formula_end].to_string(),
            bbox,
            style: TextStyle::default(),
        });
        cursor = formula_end;
    }
    if segments.is_empty() && !text.trim().is_empty() {
        push_text_segment(&mut segments, text, bbox);
    }
    segments
}

fn push_text_segment(segments: &mut Vec<Segment>, text: &str, bbox: BoundingBox) {
    if text.is_empty() {
        return;
    }
    segments.push(Segment {
        kind: SegmentKind::Text,
        text: text.to_string(),
        bbox,
        style: TextStyle::default(),
    });
}

fn assign_provider_continuations(pages: &mut [Page]) {
    let mut groups: HashMap<String, Vec<(usize, usize, Option<usize>)>> = HashMap::new();
    for page in pages.iter() {
        for (block_index, block) in page.blocks.iter().enumerate() {
            let global = metadata_token(block.metadata.get("raw_global_group_id"));
            let local = metadata_token(block.metadata.get("raw_group_id"));
            let group_id = if let Some(global) = global {
                format!("provider-paddle-global-{global}")
            } else if let Some(local) = local {
                format!(
                    "provider-paddle-page-{:03}-group-{local}",
                    page.page_index + 1
                )
            } else {
                continue;
            };
            let raw_order = block
                .metadata
                .get("raw_block_order")
                .and_then(Value::as_i64)
                .and_then(|value| usize::try_from(value).ok());
            groups
                .entry(group_id)
                .or_default()
                .push((page.page_index, block_index, raw_order));
        }
    }

    for (group_id, mut members) in groups {
        if members.len() <= 1 || members.iter().any(|member| member.2.is_none()) {
            continue;
        }
        members.sort_by_key(|member| (member.0, member.2.unwrap_or_default(), member.1));
        if continuation_crosses_columns(pages, &members) {
            continue;
        }
        let scope = if members
            .iter()
            .map(|member| member.0)
            .any(|page_index| page_index != members[0].0)
        {
            ContinuationScope::CrossPage
        } else {
            ContinuationScope::IntraPage
        };
        for (reading_order, (page_index, block_index, _)) in members.iter().enumerate() {
            pages[*page_index].blocks[*block_index].continuation = Some(ContinuationHint {
                group_id: group_id.clone(),
                scope,
                reading_order: Some(reading_order),
                confidence: 0.98,
            });
        }
    }
}

fn continuation_crosses_columns(pages: &[Page], members: &[(usize, usize, Option<usize>)]) -> bool {
    members.windows(2).any(|pair| {
        let (left_page, left_block, _) = pair[0];
        let (right_page, right_block, _) = pair[1];
        if left_page != right_page {
            return false;
        }
        let page = &pages[left_page];
        let left = page.blocks[left_block].bbox;
        let right = page.blocks[right_block].bbox;
        let center_distance = ((left.x0 + left.x1) / 2.0 - (right.x0 + right.x1) / 2.0).abs();
        center_distance > page.geometry.width * 0.32 && right.y0 < left.y1
    })
}

fn insert_raw_metadata(metadata: &mut BTreeMap<String, Value>, raw_block: &Value) {
    for (source, target) in [
        ("block_order", "raw_block_order"),
        ("group_id", "raw_group_id"),
        ("global_group_id", "raw_global_group_id"),
    ] {
        if let Some(value) = raw_block.get(source).filter(|value| !value.is_null()) {
            metadata.insert(target.to_string(), value.clone());
        }
    }
}

fn metadata_token(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(value) => non_empty(value),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

fn parse_bbox(value: Option<&Value>) -> Option<BoundingBox> {
    BoundingBox::from_slice(value?.as_array()?)
}

fn compact(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn string(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(value)) => value.clone(),
        Some(Value::Number(value)) => value.to_string(),
        _ => String::new(),
    }
}

fn number(value: Option<&Value>) -> Option<f32> {
    value?.as_f64().map(|value| value as f32)
}

fn non_empty(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn sanitized_excerpt(value: &str) -> Option<String> {
    let value = compact(value);
    (!value.is_empty()).then(|| value.chars().take(180).collect())
}

fn sleep_cancellable(
    duration: Duration,
    cancellation: &CancellationToken,
) -> Result<(), crate::TranslationCancelled> {
    let started = Instant::now();
    while started.elapsed() < duration {
        cancellation.check()?;
        std::thread::sleep(Duration::from_millis(100).min(duration - started.elapsed()));
    }
    Ok(())
}

fn parse_json_response<T: DeserializeOwned>(
    stage: &'static str,
    response: Response,
) -> Result<T, PaddleError> {
    let status = response.status();
    let body = response
        .bytes()
        .map_err(|error| PaddleError::request(stage, &error))?;
    if !status.is_success() {
        return Err(PaddleError::http(
            stage,
            status,
            &String::from_utf8_lossy(&body),
            None,
        ));
    }
    serde_json::from_slice(&body)
        .map_err(|_| PaddleError::invalid_response(stage, "PaddleOCR returned invalid JSON", None))
}

fn combine_jsonl_payload(text: &str) -> Result<Value, PaddleError> {
    let mut layout_results = Vec::new();
    let mut data_info = Value::Object(Default::default());
    let mut preprocessed_images = Vec::new();
    let mut line_count = 0usize;
    for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
        line_count += 1;
        let parsed: PaddleJsonlLine = serde_json::from_str(line).map_err(|_| {
            PaddleError::new(
                "download",
                PaddleErrorCategory::ResultDownloadFailed,
                format!("failed to parse PaddleOCR JSONL line {line_count}"),
                None,
                None,
                None,
            )
        })?;
        let Some(result) = parsed.result else {
            continue;
        };
        if let Some(items) = result.get("layoutParsingResults").and_then(Value::as_array) {
            layout_results.extend(items.iter().cloned());
        }
        if let Some(items) = result.get("preprocessedImages").and_then(Value::as_array) {
            preprocessed_images.extend(items.iter().cloned());
        }
        if data_info.as_object().is_some_and(|map| map.is_empty()) {
            data_info = result.get("dataInfo").cloned().unwrap_or_else(|| json!({}));
        }
    }
    Ok(json!({
        "layoutParsingResults": layout_results,
        "preprocessedImages": preprocessed_images,
        "dataInfo": data_info,
        "_meta": {
            "source": "paddle_jsonl",
            "lineCount": line_count
        }
    }))
}

#[derive(Debug, Deserialize)]
struct PaddleSubmitEnvelope {
    #[serde(default, rename = "logId", alias = "traceId")]
    log_id: String,
    #[serde(default, rename = "errorCode", alias = "code")]
    error_code: i64,
    #[serde(default, rename = "errorMsg", alias = "msg")]
    error_msg: String,
    data: Option<PaddleSubmitData>,
}

#[derive(Debug, Deserialize)]
struct PaddleSubmitData {
    #[serde(default, rename = "jobId")]
    job_id: String,
}

#[derive(Debug, Deserialize)]
struct PaddlePollEnvelope {
    #[serde(default, rename = "logId", alias = "traceId")]
    log_id: String,
    #[serde(default, rename = "errorCode", alias = "code")]
    error_code: i64,
    #[serde(default, rename = "errorMsg", alias = "msg")]
    error_msg: String,
    data: Option<PaddlePollData>,
}

#[derive(Debug, Default, Deserialize)]
struct PaddlePollData {
    #[serde(default)]
    state: String,
    #[serde(default, rename = "errorMsg")]
    error_msg: String,
    #[serde(rename = "extractProgress")]
    extract_progress: Option<PaddleExtractProgress>,
    #[serde(rename = "resultUrl")]
    result_url: Option<PaddleResultUrl>,
}

#[derive(Debug, Deserialize)]
struct PaddleExtractProgress {
    #[serde(rename = "totalPages")]
    total_pages: Option<i64>,
    #[serde(rename = "extractedPages")]
    extracted_pages: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct PaddleResultUrl {
    #[serde(default, rename = "jsonUrl")]
    json_url: String,
}

#[derive(Debug, Deserialize)]
struct PaddleJsonlLine {
    result: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct PaddleErrorEnvelope {
    #[serde(default, rename = "logId", alias = "traceId")]
    log_id: String,
    #[serde(default, rename = "errorCode", alias = "code")]
    error_code: Option<i64>,
    #[serde(default, rename = "errorMsg", alias = "msg", alias = "message")]
    error_msg: String,
}

#[derive(Debug)]
struct PaddleTrace<T> {
    data: T,
    trace_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combines_jsonl_pages() {
        let payload = r#"
{"result":{"layoutParsingResults":[{"page":1}],"dataInfo":{"pages":[{"width":100}]}}}
{"result":{"layoutParsingResults":[{"page":2}]}}
"#;

        let merged = combine_jsonl_payload(payload).unwrap();

        assert_eq!(merged["layoutParsingResults"].as_array().unwrap().len(), 2);
        assert_eq!(merged["dataInfo"]["pages"][0]["width"], 100);
        assert_eq!(merged["_meta"]["lineCount"], 2);
    }

    #[test]
    fn normalizes_geometry_and_translation_policy() {
        let payload = json!({
            "dataInfo": {"pages": [{"width": 1000, "height": 2000}]},
            "layoutParsingResults": [{
                "prunedResult": {
                    "parsing_res_list": [
                        {
                            "block_label": "text",
                            "block_content": "A body paragraph with enough text to translate.",
                            "block_bbox": [100, 200, 900, 400]
                        },
                        {
                            "block_label": "display_formula",
                            "block_content": "E = mc^2",
                            "block_bbox": [200, 500, 800, 600]
                        }
                    ]
                }
            }]
        });

        let document = normalize_paddle_payload(
            &payload,
            "source.pdf",
            "test",
            "PaddleOCR-VL-1.6",
            &[PageGeometry::new(500.0, 1000.0)],
        )
        .unwrap();

        assert_eq!(
            document.pages[0].blocks[0].bbox,
            BoundingBox::new(50.0, 100.0, 450.0, 200.0)
        );
        assert!(document.pages[0].blocks[0].policy.translate);
        assert!(!document.pages[0].blocks[1].policy.translate);
    }

    #[test]
    fn probe_accepts_provider_not_found_codes() {
        let accepted = |code| matches!(code, 0 | 404 | 11001);
        assert!(accepted(404));
        assert!(accepted(11001));
        assert!(!accepted(401));
    }

    #[test]
    fn client_debug_redacts_token() {
        let client = PaddleClient::new(PaddleClientConfig::default(), "secret-token").unwrap();
        let debug = format!("{client:?}");

        assert!(!debug.contains("secret-token"));
        assert!(debug.contains("[REDACTED]"));
    }

    #[test]
    fn normalizes_full_provider_fixture_when_requested() {
        let Some(path) = std::env::var_os("GLYPHST_PADDLE_FIXTURE").map(PathBuf::from) else {
            return;
        };
        let payload: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        let page_count = payload["layoutParsingResults"].as_array().unwrap().len();
        let target_pages = vec![PageGeometry::new(595.0, 842.0); page_count];

        let document = normalize_paddle_payload(
            &payload,
            "source.pdf",
            "fixture",
            "PaddleOCR-VL-1.6",
            &target_pages,
        )
        .unwrap();

        assert_eq!(document.pages.len(), page_count);
        assert!(
            document
                .pages
                .iter()
                .flat_map(|page| &page.blocks)
                .any(|block| block.policy.translate)
        );
        document.validate().unwrap();
    }
}
