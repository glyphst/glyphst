use std::{
    collections::BTreeMap,
    io::Read as _,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use anyhow::{Context as _, Result, anyhow};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use serde_json::{Value, json};

use crate::{
    chatgpt_subscription::{
        acquire_request_access, invalidate_credentials, response_error, responses_url,
    },
    prompt::system_prompt,
    settings::{AiApiStyle, AiThinkingLevel, AnthropicThinking, ModelLimits, ResolvedAiConfig},
    tools::{ToolContext, ToolEffect, ToolRegistry},
};

pub const DEFAULT_MAX_TOOL_ROUNDS: usize = 40;
/// Reading tools a turn runs at once.
const MAX_PARALLEL_TOOLS: usize = 4;
/// Output tokens asked of a model when its limit is higher; answers stream,
/// so a large limit costs nothing until it is used.
const MAX_REQUESTED_OUTPUT: u64 = 64_000;
pub const DEFAULT_MAX_TOOL_RESULT_CHARS: usize = 12_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentRole {
    User,
    Assistant,
    Tool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentImage {
    pub name: Option<String>,
    pub media_type: String,
    pub width: u32,
    pub height: u32,
    pub bytes: Arc<[u8]>,
}

impl AgentImage {
    pub fn new(
        media_type: impl Into<String>,
        width: u32,
        height: u32,
        bytes: impl Into<Arc<[u8]>>,
    ) -> Self {
        Self {
            name: None,
            media_type: media_type.into(),
            width,
            height,
            bytes: bytes.into(),
        }
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    pub fn with_optional_name(mut self, name: Option<String>) -> Self {
        self.name = name;
        self
    }

    fn data_url(&self) -> String {
        format!(
            "data:{};base64,{}",
            self.media_type,
            BASE64_STANDARD.encode(self.bytes.as_ref())
        )
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct AgentToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

impl AgentToolCall {
    pub fn arguments_json(&self) -> Result<Value> {
        serde_json::from_str(&self.arguments)
            .with_context(|| format!("{} emitted invalid tool arguments", self.name))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentToolResultPart {
    Text(String),
    Image(AgentImage),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentToolResult {
    pub call_id: String,
    pub tool_name: String,
    pub content: Vec<AgentToolResultPart>,
    pub is_error: bool,
}

impl AgentToolResult {
    pub fn text(
        call_id: impl Into<String>,
        tool_name: impl Into<String>,
        text: impl Into<String>,
    ) -> Self {
        Self {
            call_id: call_id.into(),
            tool_name: tool_name.into(),
            content: vec![AgentToolResultPart::Text(text.into())],
            is_error: false,
        }
    }

    pub fn error(
        call_id: impl Into<String>,
        tool_name: impl Into<String>,
        error: impl Into<String>,
    ) -> Self {
        Self {
            call_id: call_id.into(),
            tool_name: tool_name.into(),
            content: vec![AgentToolResultPart::Text(error.into())],
            is_error: true,
        }
    }

    pub fn bounded(mut self, max_chars: usize) -> Self {
        let mut remaining = max_chars;
        for part in &mut self.content {
            let AgentToolResultPart::Text(text) = part else {
                continue;
            };
            if text.chars().count() <= remaining {
                remaining -= text.chars().count();
                continue;
            }
            let mut truncated = text.chars().take(remaining).collect::<String>();
            truncated.push_str("\n[tool result truncated]");
            *text = truncated;
            remaining = 0;
        }
        self
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum AgentContentPart {
    Text(String),
    Image(AgentImage),
    ToolCall(AgentToolCall),
    ToolResult(AgentToolResult),
    /// A readable summary of the model's reasoning. It is shown, and never
    /// sent back to the provider.
    Reasoning(String),
    /// Provider-owned state required to continue a reasoning/tool turn. It is
    /// replayed only to the provider style that produced it and is never
    /// exposed as assistant text.
    OpaqueReasoning {
        api_style: AiApiStyle,
        value: Value,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct AgentMessage {
    pub role: AgentRole,
    pub content: Vec<AgentContentPart>,
}

impl AgentMessage {
    pub fn user_text(text: impl Into<String>) -> Self {
        Self {
            role: AgentRole::User,
            content: vec![AgentContentPart::Text(text.into())],
        }
    }

    pub fn assistant_text(text: impl Into<String>) -> Self {
        Self {
            role: AgentRole::Assistant,
            content: vec![AgentContentPart::Text(text.into())],
        }
    }

    pub fn tool_result(result: AgentToolResult) -> Self {
        Self {
            role: AgentRole::Tool,
            content: vec![AgentContentPart::ToolResult(result)],
        }
    }

    pub fn text(&self) -> String {
        self.content
            .iter()
            .filter_map(|part| match part {
                AgentContentPart::Text(text) => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("")
    }

    pub fn tool_calls(&self) -> impl Iterator<Item = &AgentToolCall> {
        self.content.iter().filter_map(|part| match part {
            AgentContentPart::ToolCall(call) => Some(call),
            _ => None,
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct AgentToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    pub strict: bool,
}

impl AgentToolDefinition {
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        input_schema: Value,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            input_schema,
            strict: true,
        }
    }
}

#[derive(Clone, Default)]
pub struct AgentCancellationToken(Arc<AtomicBool>);

impl AgentCancellationToken {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    pub fn is_canceled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }

    pub(crate) fn check(&self) -> Result<()> {
        if self.is_canceled() {
            Err(anyhow!("agent run canceled"))
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AgentUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_input_tokens: u64,
}

impl AgentUsage {
    fn add_assign(&mut self, other: Self) {
        self.input_tokens = self.input_tokens.saturating_add(other.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(other.output_tokens);
        self.cached_input_tokens = self
            .cached_input_tokens
            .saturating_add(other.cached_input_tokens);
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AgentStopReason {
    #[default]
    Stop,
    ToolUse,
    Length,
    /// The model or its safety classifiers declined.
    Refusal,
    Canceled,
    Error,
}

#[derive(Clone, Debug, PartialEq)]
pub enum AgentEvent {
    TurnStarted {
        round: usize,
    },
    TextDelta(String),
    ReasoningDelta(String),
    /// A request failed before any output and is tried again.
    Retrying {
        attempt: usize,
        delay: Duration,
        error: String,
    },
    ToolCallStarted {
        id: String,
        name: String,
    },
    ToolCallArgumentsDelta {
        id: String,
        delta: String,
    },
    ToolCallCompleted(AgentToolCall),
    ToolExecutionStarted {
        id: String,
        name: String,
    },
    ToolExecutionCompleted(AgentToolResult),
    TurnCompleted {
        round: usize,
        stop_reason: AgentStopReason,
        usage: AgentUsage,
    },
    LimitReached,
    Done,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AgentLimits {
    pub max_tool_rounds: usize,
    pub max_tool_result_chars: usize,
    pub max_retries_before_output: usize,
}

impl Default for AgentLimits {
    fn default() -> Self {
        Self {
            max_tool_rounds: DEFAULT_MAX_TOOL_ROUNDS,
            max_tool_result_chars: DEFAULT_MAX_TOOL_RESULT_CHARS,
            max_retries_before_output: 3,
        }
    }
}

#[derive(Clone, Debug)]
pub struct AgentRunResult {
    pub appended_messages: Vec<AgentMessage>,
    pub usage: AgentUsage,
    pub stop_reason: AgentStopReason,
    pub limit_reached: bool,
}

/// One run of the agent: the conversation so far and what it may use.
pub struct AgentRun {
    pub settings: ResolvedAiConfig,
    pub messages: Vec<AgentMessage>,
    pub tools: Option<Arc<ToolRegistry>>,
    /// Cancellation, and who the run's tool calls belong to.
    pub context: ToolContext,
    pub limits: AgentLimits,
    /// Identifies the conversation to the provider's prompt cache.
    pub cache_key: Option<String>,
}

pub fn run_agent(
    run: AgentRun,
    send_event: &mut impl FnMut(AgentEvent) -> Result<()>,
) -> Result<AgentRunResult> {
    let AgentRun {
        settings,
        mut messages,
        tools,
        context,
        limits,
        cache_key,
    } = run;
    if !settings.can_request() {
        return Err(anyhow!(settings.missing_credentials_message()));
    }
    let cancellation = context.cancel.clone();
    let tools = tools.unwrap_or_default();
    let definitions = tools.definitions();
    let native_thinking = match settings.api_style {
        AiApiStyle::OpenAiResponses => true,
        style => settings
            .thinking_level
            .supports_native_for_style(style, &settings.model),
    };
    let system = system_prompt(&definitions, settings.thinking_level, native_thinking);
    let client = reqwest::blocking::Client::builder()
        .build()
        .context("failed to create HTTP client")?;
    let request = ProviderRequest {
        client: &client,
        settings: &settings,
        system: &system,
        tools: &definitions,
        cache_key: cache_key.as_deref(),
    };
    let initial_len = messages.len();
    let mut total_usage = AgentUsage::default();
    let mut tool_rounds = 0usize;

    loop {
        cancellation.check()?;
        let round = tool_rounds + 1;
        send_event(AgentEvent::TurnStarted { round })?;

        let mut retry = 0usize;
        let mut stripped_reasoning = false;
        let turn = loop {
            let mut emitted_output = false;
            let result = stream_provider_turn(&request, &messages, &cancellation, &mut |event| {
                emitted_output |= matches!(
                    event,
                    AgentEvent::TextDelta(_)
                        | AgentEvent::ReasoningDelta(_)
                        | AgentEvent::ToolCallStarted { .. }
                        | AgentEvent::ToolCallArgumentsDelta { .. }
                        | AgentEvent::ToolCallCompleted(_)
                );
                send_event(event)
            });
            match result {
                Ok(turn) => break turn,
                // Thinking blocks are bound to the conversation that produced
                // them; when the provider no longer accepts them, the turn
                // goes on without them once.
                Err(error)
                    if !emitted_output && !stripped_reasoning && invalid_reasoning(&error) =>
                {
                    stripped_reasoning = true;
                    strip_reasoning(&mut messages, AiApiStyle::Anthropic);
                }
                Err(error)
                    if !emitted_output
                        && retry < limits.max_retries_before_output
                        && retryable_agent_error(&error)
                        && !cancellation.is_canceled() =>
                {
                    retry += 1;
                    let delay = retry_delay(&error, retry);
                    send_event(AgentEvent::Retrying {
                        attempt: retry,
                        delay,
                        error: format!("{error:#}"),
                    })?;
                    sleep_unless_canceled(delay, &cancellation)?;
                }
                Err(error) => return Err(error),
            }
        };

        total_usage.add_assign(turn.usage);
        send_event(AgentEvent::TurnCompleted {
            round,
            stop_reason: turn.stop_reason,
            usage: turn.usage,
        })?;
        let tool_calls = turn.message.tool_calls().cloned().collect::<Vec<_>>();
        messages.push(turn.message);

        if tool_calls.is_empty() || turn.stop_reason == AgentStopReason::Refusal {
            send_event(AgentEvent::Done)?;
            return Ok(AgentRunResult {
                appended_messages: messages.split_off(initial_len),
                usage: total_usage,
                stop_reason: turn.stop_reason,
                limit_reached: false,
            });
        }

        tool_rounds += 1;
        let results = execute_tool_calls(
            &tools,
            &tool_calls,
            &context,
            limits.max_tool_result_chars,
            send_event,
        )?;
        messages.extend(results.into_iter().map(AgentMessage::tool_result));

        if tool_rounds >= limits.max_tool_rounds {
            send_event(AgentEvent::LimitReached)?;
            return Ok(AgentRunResult {
                appended_messages: messages.split_off(initial_len),
                usage: total_usage,
                stop_reason: AgentStopReason::ToolUse,
                limit_reached: true,
            });
        }
    }
}

/// Runs a turn's tool calls: consecutive reads together (a few at a time),
/// everything else alone and in order.
fn execute_tool_calls(
    tools: &ToolRegistry,
    calls: &[AgentToolCall],
    context: &ToolContext,
    max_chars: usize,
    send_event: &mut impl FnMut(AgentEvent) -> Result<()>,
) -> Result<Vec<AgentToolResult>> {
    let mut results = Vec::with_capacity(calls.len());
    let mut rest = calls;
    while !rest.is_empty() {
        context.cancel.check()?;
        let reads = rest
            .iter()
            .take(MAX_PARALLEL_TOOLS)
            .take_while(|call| tools.effect(&call.name) == ToolEffect::Read)
            .count();
        let (batch, next) = rest.split_at(reads.max(1));
        rest = next;
        for call in batch {
            send_event(AgentEvent::ToolExecutionStarted {
                id: call.id.clone(),
                name: call.name.clone(),
            })?;
        }
        let batch_results = if let [call] = batch {
            vec![tools.execute(call, context)]
        } else {
            thread::scope(|scope| {
                let handles = batch
                    .iter()
                    .map(|call| scope.spawn(|| tools.execute(call, context)))
                    .collect::<Vec<_>>();
                handles
                    .into_iter()
                    .zip(batch)
                    .map(|(handle, call)| {
                        handle.join().unwrap_or_else(|_| {
                            AgentToolResult::error(&call.id, &call.name, "the tool failed")
                        })
                    })
                    .collect::<Vec<_>>()
            })
        };
        for result in batch_results {
            let result = result.bounded(max_chars);
            send_event(AgentEvent::ToolExecutionCompleted(result.clone()))?;
            results.push(result);
        }
    }
    Ok(results)
}

/// A provider's answer with an error status.
#[derive(Debug)]
struct ProviderStatusError {
    status: reqwest::StatusCode,
    retry_after: Option<Duration>,
    body: String,
}

impl std::fmt::Display for ProviderStatusError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "AI request failed with status {}: {}",
            self.status, self.body
        )
    }
}

impl std::error::Error for ProviderStatusError {}

fn retryable_agent_error(error: &anyhow::Error) -> bool {
    if let Some(error) = error.downcast_ref::<ProviderStatusError>() {
        let status = error.status.as_u16();
        return matches!(status, 408 | 409 | 429 | 529) || (500..600).contains(&status);
    }
    let error = format!("{error:#}").to_ascii_lowercase();
    [
        "timed out",
        "timeout",
        "connection reset",
        "connection closed",
        "temporarily unavailable",
        "failed to send",
        "failed to read ai stream",
    ]
    .iter()
    .any(|needle| error.contains(needle))
}

/// The provider's `Retry-After` (at most 30 seconds), or exponential backoff.
fn retry_delay(error: &anyhow::Error, attempt: usize) -> Duration {
    error
        .downcast_ref::<ProviderStatusError>()
        .and_then(|error| error.retry_after)
        .map(|delay| delay.min(Duration::from_secs(30)))
        .unwrap_or_else(|| Duration::from_millis(500 * (1_u64 << (attempt - 1).min(6))))
}

fn sleep_unless_canceled(delay: Duration, cancellation: &AgentCancellationToken) -> Result<()> {
    let step = Duration::from_millis(50);
    let mut waited = Duration::ZERO;
    while waited < delay {
        cancellation.check()?;
        thread::sleep(step.min(delay - waited));
        waited += step;
    }
    cancellation.check()
}

/// Whether the provider rejected replayed thinking blocks.
fn invalid_reasoning(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<ProviderStatusError>()
        .is_some_and(|error| {
            error.status == reqwest::StatusCode::BAD_REQUEST
                && error.body.contains("signature")
                && error.body.contains("thinking")
        })
}

fn strip_reasoning(messages: &mut [AgentMessage], style: AiApiStyle) {
    for message in messages {
        message.content.retain(|part| {
            !matches!(part, AgentContentPart::OpaqueReasoning { api_style, .. } if *api_style == style)
        });
    }
}

struct ProviderTurn {
    message: AgentMessage,
    usage: AgentUsage,
    stop_reason: AgentStopReason,
}

#[derive(Default)]
struct PartialToolCall {
    id: String,
    name: String,
    arguments: String,
    started: bool,
}

#[derive(Default)]
struct ProviderTurnBuilder {
    text: String,
    reasoning: String,
    tool_calls: BTreeMap<usize, PartialToolCall>,
    opaque_reasoning: Vec<Value>,
    usage: AgentUsage,
    stop_reason: AgentStopReason,
}

impl ProviderTurnBuilder {
    fn finish(self, api_style: AiApiStyle) -> ProviderTurn {
        let mut content = Vec::new();
        if !self.reasoning.is_empty() {
            content.push(AgentContentPart::Reasoning(self.reasoning));
        }
        content.extend(
            self.opaque_reasoning
                .into_iter()
                .map(|value| AgentContentPart::OpaqueReasoning { api_style, value }),
        );
        if !self.text.is_empty() {
            content.push(AgentContentPart::Text(self.text));
        }
        content.extend(self.tool_calls.into_values().map(|call| {
            AgentContentPart::ToolCall(AgentToolCall {
                id: call.id,
                name: call.name,
                arguments: if call.arguments.is_empty() {
                    "{}".to_string()
                } else {
                    call.arguments
                },
            })
        }));
        ProviderTurn {
            message: AgentMessage {
                role: AgentRole::Assistant,
                content,
            },
            usage: self.usage,
            stop_reason: self.stop_reason,
        }
    }
}

/// What every turn of a run sends.
struct ProviderRequest<'a> {
    client: &'a reqwest::blocking::Client,
    settings: &'a ResolvedAiConfig,
    system: &'a str,
    tools: &'a [AgentToolDefinition],
    cache_key: Option<&'a str>,
}

fn stream_provider_turn(
    request: &ProviderRequest<'_>,
    messages: &[AgentMessage],
    cancellation: &AgentCancellationToken,
    send_event: &mut impl FnMut(AgentEvent) -> Result<()>,
) -> Result<ProviderTurn> {
    let ProviderRequest {
        client,
        settings,
        system,
        tools,
        cache_key,
    } = *request;
    let body = match settings.api_style {
        AiApiStyle::OpenAi => openai_chat_body(settings, system, messages, tools),
        AiApiStyle::OpenAiResponses => responses_body(settings, system, messages, tools, cache_key),
        AiApiStyle::Anthropic => anthropic_body(settings, system, messages, tools),
    };

    let mut request = match settings.api_style {
        AiApiStyle::OpenAi => client.post(openai_chat_url(&settings.base_url)).json(&body),
        AiApiStyle::OpenAiResponses => {
            let access = acquire_request_access()?;
            access.authorize(client.post(responses_url(&settings.base_url)).json(&body))
        }
        AiApiStyle::Anthropic => {
            let mut request = client
                .post(anthropic_messages_url(&settings.base_url))
                .header("anthropic-version", "2023-06-01")
                .json(&body);
            if anthropic_fallbacks(settings) {
                request = request.header("anthropic-beta", ANTHROPIC_FALLBACK_BETA);
            }
            request
        }
    };
    match settings.api_style {
        AiApiStyle::OpenAi if !settings.api_key.is_empty() => {
            request = request.bearer_auth(&settings.api_key);
        }
        AiApiStyle::Anthropic if !settings.api_key.is_empty() => {
            request = request.header("x-api-key", &settings.api_key);
        }
        _ => {}
    }

    cancellation.check()?;
    let response = request
        .send()
        .with_context(|| format!("failed to send {} request", settings.provider_name))?;
    if settings.api_style == AiApiStyle::OpenAiResponses
        && matches!(
            response.status(),
            reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN
        )
    {
        invalidate_credentials("Your ChatGPT session is invalid. Sign in again.");
    }
    read_provider_sse(response, settings.api_style, cancellation, send_event)
}

fn openai_chat_body(
    settings: &ResolvedAiConfig,
    system: &str,
    messages: &[AgentMessage],
    tools: &[AgentToolDefinition],
) -> Value {
    let mut request_messages = vec![json!({"role": "system", "content": system})];
    let mut pending_tool_images = Vec::new();
    for message in messages {
        if message.role != AgentRole::Tool {
            flush_openai_tool_images(&mut request_messages, &mut pending_tool_images);
        }
        append_openai_chat_message(&mut request_messages, message, &mut pending_tool_images);
    }
    flush_openai_tool_images(&mut request_messages, &mut pending_tool_images);
    let mut body = json!({
        "model": settings.model,
        "messages": request_messages,
        "stream": true,
        "stream_options": {"include_usage": true},
    });
    if !tools.is_empty() {
        body["tools"] = Value::Array(
            tools
                .iter()
                .map(|tool| {
                    let mut function = json!({
                        "name": tool.name,
                        "description": tool.description,
                        "parameters": tool.input_schema,
                    });
                    if tool.strict {
                        function["strict"] = json!(true);
                    }
                    json!({
                        "type": "function",
                        "function": function,
                    })
                })
                .collect(),
        );
        body["tool_choice"] = json!("auto");
    }
    apply_openai_thinking(&mut body, &settings.model, settings.thinking_level);
    body
}

fn append_openai_chat_message(
    out: &mut Vec<Value>,
    message: &AgentMessage,
    pending_tool_images: &mut Vec<Value>,
) {
    match message.role {
        AgentRole::User => out.push(json!({
            "role": "user",
            "content": openai_content(&message.content),
        })),
        AgentRole::Assistant => {
            let text = message
                .content
                .iter()
                .filter_map(|part| match part {
                    AgentContentPart::Text(text) => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("");
            let calls = message
                .tool_calls()
                .map(|call| {
                    json!({
                        "id": call.id,
                        "type": "function",
                        "function": {"name": call.name, "arguments": call.arguments},
                    })
                })
                .collect::<Vec<_>>();
            let mut value = json!({"role": "assistant", "content": text});
            if !calls.is_empty() {
                value["tool_calls"] = Value::Array(calls);
            }
            out.push(value);
        }
        AgentRole::Tool => {
            for result in message.content.iter().filter_map(|part| match part {
                AgentContentPart::ToolResult(result) => Some(result),
                _ => None,
            }) {
                let text = tool_result_text(result);
                out.push(json!({
                    "role": "tool",
                    "tool_call_id": result.call_id,
                    "content": if text.is_empty() { "(see attached image)" } else { &text },
                }));
                let images = result
                    .content
                    .iter()
                    .filter_map(|part| match part {
                        AgentToolResultPart::Image(image) => Some(image),
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                if !images.is_empty() {
                    pending_tool_images.push(json!({
                        "type": "text",
                        "text": format!(
                            "Image output from tool {} (call {}):",
                            result.tool_name, result.call_id
                        ),
                    }));
                    pending_tool_images.extend(images.into_iter().map(|image| {
                        json!({
                            "type": "image_url",
                            "image_url": {"url": image.data_url()},
                        })
                    }));
                }
            }
        }
    }
}

fn flush_openai_tool_images(out: &mut Vec<Value>, pending_tool_images: &mut Vec<Value>) {
    if pending_tool_images.is_empty() {
        return;
    }
    out.push(json!({
        "role": "user",
        "content": std::mem::take(pending_tool_images),
    }));
}

fn openai_content(parts: &[AgentContentPart]) -> Value {
    let mut content = Vec::new();
    for part in parts {
        match part {
            AgentContentPart::Text(text) if !text.is_empty() => {
                content.push(json!({"type": "text", "text": text}));
            }
            AgentContentPart::Image(image) => content.push(json!({
                "type": "image_url",
                "image_url": {"url": image.data_url()},
            })),
            _ => {}
        }
    }
    if let [single] = content.as_slice()
        && single["type"] == "text"
    {
        return single["text"].clone();
    }
    Value::Array(content)
}

fn responses_body(
    settings: &ResolvedAiConfig,
    system: &str,
    messages: &[AgentMessage],
    tools: &[AgentToolDefinition],
    cache_key: Option<&str>,
) -> Value {
    let mut input = Vec::new();
    for message in messages {
        append_responses_message(&mut input, message);
    }
    let mut body = json!({
        "model": settings.model,
        "instructions": system,
        "input": input,
        "stream": true,
        "store": false,
    });
    let replays_reasoning = messages.iter().any(|message| {
        message.content.iter().any(|part| {
            matches!(
                part,
                AgentContentPart::OpaqueReasoning {
                    api_style: AiApiStyle::OpenAiResponses,
                    ..
                }
            )
        })
    });
    if settings.thinking_level != AiThinkingLevel::Off || replays_reasoning {
        body["include"] = json!(["reasoning.encrypted_content"]);
    }
    if !tools.is_empty() {
        body["tools"] = Value::Array(
            tools
                .iter()
                .map(|tool| {
                    let mut definition = json!({
                        "type": "function",
                        "name": tool.name,
                        "description": tool.description,
                        "parameters": tool.input_schema,
                    });
                    if tool.strict {
                        definition["strict"] = json!(true);
                    }
                    definition
                })
                .collect(),
        );
        body["tool_choice"] = json!("auto");
    }
    apply_responses_reasoning(&mut body, settings.thinking_level);
    if let Some(cache_key) = cache_key {
        body["prompt_cache_key"] = json!(cache_key);
    }
    body
}

fn append_responses_message(out: &mut Vec<Value>, message: &AgentMessage) {
    match message.role {
        AgentRole::User | AgentRole::Assistant => {
            let role = if message.role == AgentRole::User {
                "user"
            } else {
                "assistant"
            };
            if message.role == AgentRole::Assistant {
                out.extend(message.content.iter().filter_map(|part| match part {
                    AgentContentPart::OpaqueReasoning { api_style, value }
                        if *api_style == AiApiStyle::OpenAiResponses =>
                    {
                        Some(value.clone())
                    }
                    _ => None,
                }));
            }
            let mut content = Vec::new();
            for part in &message.content {
                match part {
                    AgentContentPart::Text(text)
                        if !text.is_empty() && message.role == AgentRole::User =>
                    {
                        content.push(json!({"type": "input_text", "text": text}));
                    }
                    AgentContentPart::Text(text) if !text.is_empty() => content.push(json!({
                        "type": "output_text",
                        "text": text,
                        "annotations": [],
                    })),
                    AgentContentPart::Image(image) if message.role == AgentRole::User => {
                        content.push(json!({
                            "type": "input_image",
                            "detail": "auto",
                            "image_url": image.data_url(),
                        }));
                    }
                    AgentContentPart::ToolCall(call) => {
                        flush_responses_content(out, role, &mut content);
                        out.push(json!({
                            "type": "function_call",
                            "call_id": call.id,
                            "name": call.name,
                            "arguments": call.arguments,
                        }));
                    }
                    _ => {}
                }
            }
            flush_responses_content(out, role, &mut content);
        }
        AgentRole::Tool => {
            for result in message.content.iter().filter_map(|part| match part {
                AgentContentPart::ToolResult(result) => Some(result),
                _ => None,
            }) {
                let mut output = Vec::new();
                for part in &result.content {
                    match part {
                        AgentToolResultPart::Text(text) => {
                            output.push(json!({"type": "input_text", "text": text}));
                        }
                        AgentToolResultPart::Image(image) => output.push(json!({
                            "type": "input_image",
                            "detail": "auto",
                            "image_url": image.data_url(),
                        })),
                    }
                }
                out.push(json!({
                    "type": "function_call_output",
                    "call_id": result.call_id,
                    "output": output,
                }));
            }
        }
    }
}

fn flush_responses_content(out: &mut Vec<Value>, role: &str, content: &mut Vec<Value>) {
    if content.is_empty() {
        return;
    }
    out.push(json!({
        "type": "message",
        "role": role,
        "content": std::mem::take(content),
    }));
}

const ANTHROPIC_FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";
const CACHE_CONTROL: &str = "cache_control";

/// Whether to ask Anthropic to retry a declined request on another model:
/// only on the Claude API itself, for the models whose safety classifiers
/// can decline.
fn anthropic_fallbacks(settings: &ResolvedAiConfig) -> bool {
    settings
        .base_url
        .trim_end_matches('/')
        .ends_with("api.anthropic.com")
        && [
            "claude-fable-5-1",
            "claude-opus-5-5",
            "claude-opus-5",
            "claude-sonnet-5-5",
        ]
        .contains(&settings.model.as_str())
}

fn anthropic_body(
    settings: &ResolvedAiConfig,
    system: &str,
    messages: &[AgentMessage],
    tools: &[AgentToolDefinition],
) -> Value {
    let mut request_messages = Vec::new();
    for message in messages {
        append_anthropic_message(&mut request_messages, message);
    }
    // Cache the stable prefix (tools, then the system prompt) and the
    // conversation up to the newest message: three of four breakpoints.
    if let Some(block) = request_messages
        .last_mut()
        .and_then(|message| message["content"].as_array_mut())
        .and_then(|content| content.last_mut())
    {
        block[CACHE_CONTROL] = json!({"type": "ephemeral"});
    }
    let max_tokens = ModelLimits::for_model(&settings.model)
        .max_output
        .min(MAX_REQUESTED_OUTPUT);
    let mut body = json!({
        "model": settings.model,
        "system": [{"type": "text", "text": system, CACHE_CONTROL: {"type": "ephemeral"}}],
        "messages": request_messages,
        "stream": true,
        "max_tokens": max_tokens,
    });
    if !tools.is_empty() {
        let mut definitions = tools
            .iter()
            .map(|tool| {
                json!({
                    "name": tool.name,
                    "description": tool.description,
                    "input_schema": tool.input_schema,
                })
            })
            .collect::<Vec<_>>();
        if let Some(last) = definitions.last_mut() {
            last[CACHE_CONTROL] = json!({"type": "ephemeral"});
        }
        body["tools"] = Value::Array(definitions);
        body["tool_choice"] = json!({"type": "auto"});
    }
    apply_anthropic_thinking(&mut body, &settings.model, settings.thinking_level);
    if anthropic_fallbacks(settings) {
        body["fallbacks"] = json!("default");
    }
    body
}

fn append_anthropic_message(out: &mut Vec<Value>, message: &AgentMessage) {
    let (role, mut content) = match message.role {
        AgentRole::User => ("user", Vec::new()),
        AgentRole::Assistant => ("assistant", Vec::new()),
        AgentRole::Tool => ("user", Vec::new()),
    };
    let ordered_parts = if message.role == AgentRole::Assistant {
        message
            .content
            .iter()
            .filter(|part| {
                matches!(
                    part,
                    AgentContentPart::OpaqueReasoning {
                        api_style: AiApiStyle::Anthropic,
                        ..
                    }
                )
            })
            .chain(message.content.iter().filter(|part| {
                !matches!(
                    part,
                    AgentContentPart::OpaqueReasoning {
                        api_style: AiApiStyle::Anthropic,
                        ..
                    }
                )
            }))
            .collect::<Vec<_>>()
    } else {
        message.content.iter().collect::<Vec<_>>()
    };
    for part in ordered_parts {
        match part {
            AgentContentPart::Text(text) if !text.is_empty() => {
                content.push(json!({"type": "text", "text": text}));
            }
            AgentContentPart::Image(image) if message.role == AgentRole::User => {
                content.push(json!({
                    "type": "image",
                    "source": {
                        "type": "base64",
                        "media_type": image.media_type,
                        "data": BASE64_STANDARD.encode(image.bytes.as_ref()),
                    }
                }));
            }
            AgentContentPart::ToolCall(call) => content.push(json!({
                "type": "tool_use",
                "id": normalize_anthropic_id(&call.id),
                "name": call.name,
                "input": serde_json::from_str::<Value>(&call.arguments).unwrap_or_else(|_| json!({})),
            })),
            AgentContentPart::ToolResult(result) => {
                let result_content = result
                    .content
                    .iter()
                    .map(|part| match part {
                        AgentToolResultPart::Text(text) => json!({"type": "text", "text": text}),
                        AgentToolResultPart::Image(image) => json!({
                            "type": "image",
                            "source": {
                                "type": "base64",
                                "media_type": image.media_type,
                                "data": BASE64_STANDARD.encode(image.bytes.as_ref()),
                            }
                        }),
                    })
                    .collect::<Vec<_>>();
                content.push(json!({
                    "type": "tool_result",
                    "tool_use_id": normalize_anthropic_id(&result.call_id),
                    "content": result_content,
                    "is_error": result.is_error,
                }));
            }
            AgentContentPart::OpaqueReasoning { api_style, value }
                if *api_style == AiApiStyle::Anthropic =>
            {
                content.push(value.clone());
            }
            _ => {}
        }
    }
    if content.is_empty() {
        return;
    }
    if let Some(previous) = out.last_mut()
        && previous["role"] == role
        && let Some(previous_content) = previous["content"].as_array_mut()
    {
        previous_content.extend(content);
        return;
    }
    out.push(json!({"role": role, "content": content}));
}

fn tool_result_text(result: &AgentToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|part| match part {
            AgentToolResultPart::Text(text) => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn read_provider_sse(
    mut response: reqwest::blocking::Response,
    api_style: AiApiStyle,
    cancellation: &AgentCancellationToken,
    send_event: &mut impl FnMut(AgentEvent) -> Result<()>,
) -> Result<ProviderTurn> {
    if !response.status().is_success() {
        let status = response.status();
        let retry_after = response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.trim().parse::<f64>().ok())
            .filter(|seconds| seconds.is_finite() && *seconds >= 0.0)
            .map(Duration::from_secs_f64);
        let mut body = String::new();
        if let Err(error) = response.read_to_string(&mut body) {
            body = format!("failed to read error body: {error:#}");
        }
        return Err(ProviderStatusError {
            status,
            retry_after,
            body,
        }
        .into());
    }

    let mut builder = ProviderTurnBuilder::default();
    let mut pending = Vec::new();
    let mut event_data = Vec::<String>::new();
    let mut buffer = [0_u8; 8192];
    loop {
        cancellation.check()?;
        let read = response
            .read(&mut buffer)
            .context("failed to read AI stream")?;
        if read == 0 {
            break;
        }
        pending.extend_from_slice(&buffer[..read]);
        while let Some(newline_ix) = pending.iter().position(|byte| *byte == b'\n') {
            let line = pending.drain(..=newline_ix).collect::<Vec<_>>();
            let line = std::str::from_utf8(&line)
                .context("AI stream emitted invalid UTF-8")?
                .trim_end_matches(['\r', '\n']);
            if line.is_empty() {
                if process_sse_event(api_style, &event_data.join("\n"), &mut builder, send_event)? {
                    return Ok(builder.finish(api_style));
                }
                event_data.clear();
            } else if let Some(data) = line.strip_prefix("data:") {
                event_data.push(data.trim_start().to_string());
            }
        }
    }
    if !pending.is_empty() {
        let line = std::str::from_utf8(&pending)
            .context("AI stream emitted invalid UTF-8")?
            .trim();
        if let Some(data) = line.strip_prefix("data:") {
            event_data.push(data.trim_start().to_string());
        }
    }
    if !event_data.is_empty() {
        process_sse_event(api_style, &event_data.join("\n"), &mut builder, send_event)?;
    }
    Ok(builder.finish(api_style))
}

fn process_sse_event(
    api_style: AiApiStyle,
    data: &str,
    builder: &mut ProviderTurnBuilder,
    send_event: &mut impl FnMut(AgentEvent) -> Result<()>,
) -> Result<bool> {
    if data.is_empty() {
        return Ok(false);
    }
    if data == "[DONE]" {
        return Ok(true);
    }
    let value = serde_json::from_str::<Value>(data).context("failed to parse AI stream event")?;
    match api_style {
        AiApiStyle::OpenAi => process_openai_event(&value, builder, send_event),
        AiApiStyle::OpenAiResponses => process_responses_event(&value, builder, send_event),
        AiApiStyle::Anthropic => process_anthropic_event(&value, builder, send_event),
    }
}

fn process_openai_event(
    value: &Value,
    builder: &mut ProviderTurnBuilder,
    send_event: &mut impl FnMut(AgentEvent) -> Result<()>,
) -> Result<bool> {
    if let Some(usage) = value.get("usage") {
        builder.usage = AgentUsage {
            input_tokens: usage["prompt_tokens"].as_u64().unwrap_or(0),
            output_tokens: usage["completion_tokens"].as_u64().unwrap_or(0),
            cached_input_tokens: usage["prompt_tokens_details"]["cached_tokens"]
                .as_u64()
                .unwrap_or(0),
        };
    }
    let choice = &value["choices"][0];
    // DeepSeek and OpenRouter stream readable reasoning beside the answer.
    if let Some(reasoning) = choice["delta"]["reasoning_content"]
        .as_str()
        .or_else(|| choice["delta"]["reasoning"].as_str())
        && !reasoning.is_empty()
    {
        builder.reasoning.push_str(reasoning);
        send_event(AgentEvent::ReasoningDelta(reasoning.to_string()))?;
    }
    if let Some(delta) = choice["delta"]["content"].as_str()
        && !delta.is_empty()
    {
        builder.text.push_str(delta);
        send_event(AgentEvent::TextDelta(delta.to_string()))?;
    }
    if let Some(calls) = choice["delta"]["tool_calls"].as_array() {
        for call in calls {
            let index = call["index"].as_u64().unwrap_or(0) as usize;
            let partial = builder.tool_calls.entry(index).or_default();
            if let Some(id) = call["id"].as_str() {
                partial.id.push_str(id);
            }
            if let Some(name) = call["function"]["name"].as_str() {
                partial.name.push_str(name);
            }
            if !partial.started && !partial.name.is_empty() {
                partial.started = true;
                send_event(AgentEvent::ToolCallStarted {
                    id: partial.id.clone(),
                    name: partial.name.clone(),
                })?;
            }
            if let Some(arguments) = call["function"]["arguments"].as_str()
                && !arguments.is_empty()
            {
                partial.arguments.push_str(arguments);
                send_event(AgentEvent::ToolCallArgumentsDelta {
                    id: partial.id.clone(),
                    delta: arguments.to_string(),
                })?;
            }
        }
    }
    if let Some(reason) = choice["finish_reason"].as_str() {
        builder.stop_reason = match reason {
            "tool_calls" | "function_call" => AgentStopReason::ToolUse,
            "length" => AgentStopReason::Length,
            _ => AgentStopReason::Stop,
        };
        emit_completed_calls(builder, send_event)?;
    }
    Ok(false)
}

fn process_responses_event(
    value: &Value,
    builder: &mut ProviderTurnBuilder,
    send_event: &mut impl FnMut(AgentEvent) -> Result<()>,
) -> Result<bool> {
    match value["type"].as_str() {
        Some("response.output_text.delta") => {
            if let Some(delta) = value["delta"].as_str()
                && !delta.is_empty()
            {
                builder.text.push_str(delta);
                send_event(AgentEvent::TextDelta(delta.to_string()))?;
            }
        }
        Some("response.reasoning_summary_text.delta") => {
            if let Some(delta) = value["delta"].as_str()
                && !delta.is_empty()
            {
                builder.reasoning.push_str(delta);
                send_event(AgentEvent::ReasoningDelta(delta.to_string()))?;
            }
        }
        Some("response.reasoning_summary_part.added") if !builder.reasoning.is_empty() => {
            builder.reasoning.push_str("\n\n");
            send_event(AgentEvent::ReasoningDelta("\n\n".to_string()))?;
        }
        Some("response.output_item.added") => {
            let item = &value["item"];
            let index = value["output_index"].as_u64().unwrap_or(0) as usize;
            if item["type"] == "function_call" {
                let partial = builder.tool_calls.entry(index).or_default();
                partial.id = item["call_id"]
                    .as_str()
                    .or_else(|| item["id"].as_str())
                    .unwrap_or_default()
                    .to_string();
                partial.name = item["name"].as_str().unwrap_or_default().to_string();
                partial.arguments = item["arguments"].as_str().unwrap_or_default().to_string();
                partial.started = true;
                send_event(AgentEvent::ToolCallStarted {
                    id: partial.id.clone(),
                    name: partial.name.clone(),
                })?;
            }
        }
        Some("response.function_call_arguments.delta") => {
            let index = value["output_index"].as_u64().unwrap_or(0) as usize;
            let delta = value["delta"].as_str().unwrap_or_default();
            let partial = builder.tool_calls.entry(index).or_default();
            partial.arguments.push_str(delta);
            if !delta.is_empty() {
                send_event(AgentEvent::ToolCallArgumentsDelta {
                    id: partial.id.clone(),
                    delta: delta.to_string(),
                })?;
            }
        }
        Some("response.output_item.done") => {
            let item = &value["item"];
            let index = value["output_index"].as_u64().unwrap_or(0) as usize;
            if item["type"] == "function_call" {
                let partial = builder.tool_calls.entry(index).or_default();
                partial.id = item["call_id"]
                    .as_str()
                    .or_else(|| item["id"].as_str())
                    .unwrap_or(&partial.id)
                    .to_string();
                partial.name = item["name"].as_str().unwrap_or(&partial.name).to_string();
                if let Some(arguments) = item["arguments"].as_str() {
                    partial.arguments = arguments.to_string();
                }
                send_event(AgentEvent::ToolCallCompleted(AgentToolCall {
                    id: partial.id.clone(),
                    name: partial.name.clone(),
                    arguments: partial.arguments.clone(),
                }))?;
            } else if item["type"] == "reasoning" {
                builder.opaque_reasoning.push(item.clone());
            }
        }
        Some("response.completed") => {
            let response = &value["response"];
            let usage = &response["usage"];
            builder.usage = AgentUsage {
                input_tokens: usage["input_tokens"].as_u64().unwrap_or(0),
                output_tokens: usage["output_tokens"].as_u64().unwrap_or(0),
                cached_input_tokens: usage["input_tokens_details"]["cached_tokens"]
                    .as_u64()
                    .unwrap_or(0),
            };
            builder.stop_reason = if builder.tool_calls.is_empty() {
                AgentStopReason::Stop
            } else {
                AgentStopReason::ToolUse
            };
            return Ok(true);
        }
        Some("response.incomplete") => {
            builder.stop_reason = AgentStopReason::Length;
            return Ok(true);
        }
        Some("response.failed") | Some("error") => {
            return Err(anyhow!(
                "{}",
                response_error(value)
                    .unwrap_or_else(|| "ChatGPT Subscription request failed".to_string())
            ));
        }
        _ => {}
    }
    Ok(false)
}

fn process_anthropic_event(
    value: &Value,
    builder: &mut ProviderTurnBuilder,
    send_event: &mut impl FnMut(AgentEvent) -> Result<()>,
) -> Result<bool> {
    match value["type"].as_str() {
        Some("message_start") => {
            builder.usage.input_tokens = value["message"]["usage"]["input_tokens"]
                .as_u64()
                .unwrap_or(0);
            builder.usage.cached_input_tokens =
                value["message"]["usage"]["cache_read_input_tokens"]
                    .as_u64()
                    .unwrap_or(0);
        }
        Some("content_block_start") => {
            let index = value["index"].as_u64().unwrap_or(0) as usize;
            let block = &value["content_block"];
            if block["type"] == "fallback" {
                // Another model took over: what the declined model began
                // besides text is not sent back.
                builder.tool_calls.clear();
                builder.opaque_reasoning.clear();
            } else if block["type"] == "tool_use" {
                let partial = builder.tool_calls.entry(index).or_default();
                partial.id = block["id"].as_str().unwrap_or_default().to_string();
                partial.name = block["name"].as_str().unwrap_or_default().to_string();
                partial.started = true;
                send_event(AgentEvent::ToolCallStarted {
                    id: partial.id.clone(),
                    name: partial.name.clone(),
                })?;
            } else if matches!(
                block["type"].as_str(),
                Some("thinking" | "redacted_thinking")
            ) {
                builder.opaque_reasoning.push(block.clone());
            }
        }
        Some("content_block_delta") => {
            let index = value["index"].as_u64().unwrap_or(0) as usize;
            let delta = &value["delta"];
            match delta["type"].as_str() {
                Some("text_delta") => {
                    if let Some(text) = delta["text"].as_str()
                        && !text.is_empty()
                    {
                        builder.text.push_str(text);
                        send_event(AgentEvent::TextDelta(text.to_string()))?;
                    }
                }
                Some("input_json_delta") => {
                    let arguments = delta["partial_json"].as_str().unwrap_or_default();
                    let partial = builder.tool_calls.entry(index).or_default();
                    partial.arguments.push_str(arguments);
                    if !arguments.is_empty() {
                        send_event(AgentEvent::ToolCallArgumentsDelta {
                            id: partial.id.clone(),
                            delta: arguments.to_string(),
                        })?;
                    }
                }
                Some("thinking_delta") | Some("signature_delta") => {
                    if let Some(thinking) = delta["thinking"].as_str()
                        && !thinking.is_empty()
                    {
                        builder.reasoning.push_str(thinking);
                        send_event(AgentEvent::ReasoningDelta(thinking.to_string()))?;
                    }
                    if let Some(block) = builder.opaque_reasoning.last_mut()
                        && let Some(object) = block.as_object_mut()
                    {
                        let key = if delta["type"] == "thinking_delta" {
                            "thinking"
                        } else {
                            "signature"
                        };
                        let fragment = delta[key].as_str().unwrap_or_default();
                        let current = object
                            .entry(key)
                            .or_insert_with(|| Value::String(String::new()));
                        if let Some(current) = current.as_str() {
                            *object.get_mut(key).expect("inserted reasoning field") =
                                Value::String(format!("{current}{fragment}"));
                        }
                    }
                }
                _ => {}
            }
        }
        Some("content_block_stop") => {
            let index = value["index"].as_u64().unwrap_or(0) as usize;
            if let Some(partial) = builder.tool_calls.get(&index) {
                send_event(AgentEvent::ToolCallCompleted(AgentToolCall {
                    id: partial.id.clone(),
                    name: partial.name.clone(),
                    arguments: partial.arguments.clone(),
                }))?;
            }
        }
        Some("message_delta") => {
            builder.usage.output_tokens = value["usage"]["output_tokens"].as_u64().unwrap_or(0);
            builder.stop_reason = match value["delta"]["stop_reason"].as_str() {
                Some("tool_use") => AgentStopReason::ToolUse,
                Some("max_tokens") => AgentStopReason::Length,
                Some("refusal") => AgentStopReason::Refusal,
                _ => AgentStopReason::Stop,
            };
        }
        Some("message_stop") => return Ok(true),
        Some("error") => {
            return Err(anyhow!(
                "{}",
                value["error"]["message"]
                    .as_str()
                    .unwrap_or("Anthropic request failed")
            ));
        }
        _ => {}
    }
    Ok(false)
}

fn emit_completed_calls(
    builder: &ProviderTurnBuilder,
    send_event: &mut impl FnMut(AgentEvent) -> Result<()>,
) -> Result<()> {
    for call in builder.tool_calls.values() {
        send_event(AgentEvent::ToolCallCompleted(AgentToolCall {
            id: call.id.clone(),
            name: call.name.clone(),
            arguments: call.arguments.clone(),
        }))?;
    }
    Ok(())
}

fn openai_chat_url(base_url: &str) -> String {
    let base_url = base_url.trim_end_matches('/');
    if base_url.ends_with("/v1/chat/completions") {
        base_url.to_string()
    } else if openai_base_url_has_version(base_url) {
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

fn openai_base_url_has_version(base_url: &str) -> bool {
    base_url.rsplit('/').next().is_some_and(|segment| {
        segment.len() > 1
            && segment.starts_with('v')
            && segment[1..].chars().all(|ch| ch.is_ascii_digit())
    })
}

fn normalize_anthropic_id(id: &str) -> String {
    id.chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '_' | '-') {
                character
            } else {
                '_'
            }
        })
        .take(64)
        .collect()
}

fn apply_responses_reasoning(body: &mut Value, level: AiThinkingLevel) {
    let effort = match level {
        AiThinkingLevel::Off => return,
        AiThinkingLevel::Minimal => "minimal",
        AiThinkingLevel::Low => "low",
        AiThinkingLevel::Medium => "medium",
        AiThinkingLevel::High => "high",
        AiThinkingLevel::XHigh => "xhigh",
        AiThinkingLevel::Max => "max",
    };
    body["reasoning"] = json!({"effort": effort, "summary": "auto"});
}

fn apply_openai_thinking(body: &mut Value, model: &str, level: AiThinkingLevel) {
    if !level.supports_native_for_style(AiApiStyle::OpenAi, model) {
        return;
    }
    let level = level.clamp_for_style(AiApiStyle::OpenAi, model);
    body["reasoning_effort"] = json!(match level {
        AiThinkingLevel::Off => return,
        AiThinkingLevel::Minimal => "minimal",
        AiThinkingLevel::Low => "low",
        AiThinkingLevel::Medium => "medium",
        AiThinkingLevel::High => "high",
        AiThinkingLevel::XHigh | AiThinkingLevel::Max => "xhigh",
    });
}

fn apply_anthropic_thinking(body: &mut Value, model: &str, level: AiThinkingLevel) {
    let level = level.clamp_for_style(AiApiStyle::Anthropic, model);
    match AnthropicThinking::for_model(model) {
        // Current models think adaptively and take an effort instead of a
        // budget; "off" asks for the least effort, since several of them
        // cannot turn thinking off.
        Some(AnthropicThinking::Adaptive) => {
            if level != AiThinkingLevel::Off {
                body["thinking"] = json!({"type": "adaptive", "display": "summarized"});
            }
            body["output_config"] = json!({
                "effort": match level {
                    AiThinkingLevel::Off | AiThinkingLevel::Minimal | AiThinkingLevel::Low => "low",
                    AiThinkingLevel::Medium => "medium",
                    AiThinkingLevel::High => "high",
                    AiThinkingLevel::XHigh => "xhigh",
                    AiThinkingLevel::Max => "max",
                },
            });
        }
        Some(AnthropicThinking::Budget) if level != AiThinkingLevel::Off => {
            body["thinking"] = json!({
                "type": "enabled",
                "budget_tokens": match level {
                    AiThinkingLevel::Off | AiThinkingLevel::Minimal | AiThinkingLevel::Low => 1024,
                    AiThinkingLevel::Medium => 2048,
                    AiThinkingLevel::High => 4096,
                    AiThinkingLevel::XHigh | AiThinkingLevel::Max => 8192,
                },
            });
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{AiProvider, AiThinkingLevel};

    fn settings(api_style: AiApiStyle) -> ResolvedAiConfig {
        ResolvedAiConfig {
            provider: match api_style {
                AiApiStyle::Anthropic => AiProvider::Anthropic,
                AiApiStyle::OpenAiResponses => AiProvider::ChatGptSubscription,
                AiApiStyle::OpenAi => AiProvider::OpenAi,
            },
            provider_name: "Test".to_string(),
            api_style,
            enabled: true,
            model: "test-model".to_string(),
            api_key: "key".to_string(),
            base_url: "http://127.0.0.1".to_string(),
            base_url_overridden: false,
            thinking_level: AiThinkingLevel::Off,
        }
    }

    fn tool() -> AgentToolDefinition {
        AgentToolDefinition::new(
            "search_document",
            "Search the document",
            json!({
                "type": "object",
                "properties": {"query": {"type": "string"}},
                "required": ["query"],
                "additionalProperties": false,
            }),
        )
    }

    #[test]
    fn openai_chat_keeps_parallel_tool_results_before_their_image_message() {
        let result = |id: &str, byte| {
            AgentMessage::tool_result(AgentToolResult {
                call_id: id.to_string(),
                tool_name: "render_document_pages".to_string(),
                content: vec![
                    AgentToolResultPart::Text(format!("Page {byte}")),
                    AgentToolResultPart::Image(AgentImage::new(
                        "image/png",
                        1,
                        1,
                        Arc::<[u8]>::from([byte]),
                    )),
                ],
                is_error: false,
            })
        };
        let messages = vec![result("call-1", 1), result("call-2", 2)];
        let body = openai_chat_body(&settings(AiApiStyle::OpenAi), "system", &messages, &[]);
        let messages = body["messages"].as_array().unwrap();

        assert_eq!(messages[1]["role"], "tool");
        assert_eq!(messages[2]["role"], "tool");
        assert_eq!(messages[3]["role"], "user");
        assert_eq!(messages[3]["content"][1]["type"], "image_url");
        assert_eq!(messages[3]["content"][3]["type"], "image_url");
    }

    #[test]
    fn configures_native_thinking_for_supported_models() {
        let mut openai_body = json!({});
        apply_openai_thinking(&mut openai_body, "gpt-5", AiThinkingLevel::XHigh);
        assert_eq!(openai_body["reasoning_effort"], "xhigh");

        let mut anthropic_body = json!({});
        apply_anthropic_thinking(
            &mut anthropic_body,
            "claude-sonnet-4-5-latest",
            AiThinkingLevel::Max,
        );
        assert_eq!(anthropic_body["thinking"]["budget_tokens"], 8192);

        let mut adaptive_body = json!({});
        apply_anthropic_thinking(
            &mut adaptive_body,
            "claude-opus-5-5",
            AiThinkingLevel::XHigh,
        );
        assert_eq!(
            adaptive_body["thinking"],
            json!({"type": "adaptive", "display": "summarized"})
        );
        assert_eq!(adaptive_body["output_config"]["effort"], "xhigh");

        let mut off_body = json!({});
        apply_anthropic_thinking(&mut off_body, "claude-sonnet-5-5", AiThinkingLevel::Off);
        assert!(off_body.get("thinking").is_none());
        assert_eq!(off_body["output_config"]["effort"], "low");

        let mut unsupported_body = json!({});
        apply_openai_thinking(
            &mut unsupported_body,
            "openai/gpt-4o",
            AiThinkingLevel::High,
        );
        assert!(unsupported_body.get("reasoning_effort").is_none());

        let mut responses_body = json!({});
        apply_responses_reasoning(&mut responses_body, AiThinkingLevel::Max);
        assert_eq!(responses_body["reasoning"]["effort"], "max");
        assert_eq!(responses_body["reasoning"]["summary"], "auto");
    }

    #[test]
    fn responses_payload_uses_function_call_output_with_images() {
        let result = AgentToolResult {
            call_id: "call-1".to_string(),
            tool_name: "render_document_pages".to_string(),
            content: vec![AgentToolResultPart::Image(AgentImage::new(
                "image/jpeg",
                10,
                20,
                Arc::<[u8]>::from([4, 5, 6]),
            ))],
            is_error: false,
        };
        let mut settings = settings(AiApiStyle::OpenAiResponses);
        settings.thinking_level = AiThinkingLevel::Medium;
        let body = responses_body(
            &settings,
            "system",
            &[AgentMessage::tool_result(result)],
            &[tool()],
            None,
        );

        assert_eq!(body["input"][0]["type"], "function_call_output");
        assert_eq!(body["input"][0]["output"][0]["type"], "input_image");
        assert_eq!(body["tools"][0]["name"], "search_document");
        assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
    }

    #[test]
    fn responses_user_text_does_not_include_output_only_annotations() {
        let body = responses_body(
            &settings(AiApiStyle::OpenAiResponses),
            "system",
            &[AgentMessage::user_text("hello")],
            &[],
            None,
        );

        assert_eq!(body["input"][0]["content"][0]["type"], "input_text");
        assert!(body["input"][0]["content"][0].get("annotations").is_none());
    }

    #[test]
    fn responses_replays_reasoning_before_assistant_content() {
        let message = AgentMessage {
            role: AgentRole::Assistant,
            content: vec![
                AgentContentPart::Text("working".to_string()),
                AgentContentPart::OpaqueReasoning {
                    api_style: AiApiStyle::OpenAiResponses,
                    value: json!({
                        "type": "reasoning",
                        "id": "rs_1",
                        "encrypted_content": "opaque",
                        "summary": [],
                    }),
                },
            ],
        };
        let body = responses_body(
            &settings(AiApiStyle::OpenAiResponses),
            "system",
            &[message],
            &[],
            None,
        );

        assert_eq!(body["input"][0]["type"], "reasoning");
        assert_eq!(body["input"][1]["type"], "message");
        assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
    }

    #[test]
    fn anthropic_payload_nests_image_tool_results() {
        let result = AgentToolResult {
            call_id: "call/1".to_string(),
            tool_name: "render_document_pages".to_string(),
            content: vec![AgentToolResultPart::Image(AgentImage::new(
                "image/png",
                1,
                1,
                Arc::<[u8]>::from([7]),
            ))],
            is_error: false,
        };
        let body = anthropic_body(
            &settings(AiApiStyle::Anthropic),
            "system",
            &[AgentMessage::tool_result(result)],
            &[tool()],
        );

        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["messages"][0]["content"][0]["type"], "tool_result");
        assert_eq!(
            body["messages"][0]["content"][0]["content"][0]["type"],
            "image"
        );
    }

    #[test]
    fn anthropic_replays_thinking_before_assistant_text() {
        let message = AgentMessage {
            role: AgentRole::Assistant,
            content: vec![
                AgentContentPart::Text("answer".to_string()),
                AgentContentPart::OpaqueReasoning {
                    api_style: AiApiStyle::Anthropic,
                    value: json!({
                        "type": "thinking",
                        "thinking": "opaque thought",
                        "signature": "signature",
                    }),
                },
            ],
        };
        let body = anthropic_body(&settings(AiApiStyle::Anthropic), "system", &[message], &[]);

        assert_eq!(body["messages"][0]["content"][0]["type"], "thinking");
        assert_eq!(body["messages"][0]["content"][1]["type"], "text");
    }

    #[test]
    fn fragmented_openai_tool_calls_are_assembled() {
        let mut builder = ProviderTurnBuilder::default();
        let mut events = Vec::new();
        let chunks = [
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"search_document","arguments":"{\"que"}}]}}]}),
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"ry\":\"rust\"}"}}]},"finish_reason":"tool_calls"}]}),
        ];
        for chunk in chunks {
            process_openai_event(&chunk, &mut builder, &mut |event| {
                events.push(event);
                Ok(())
            })
            .unwrap();
        }
        let turn = builder.finish(AiApiStyle::OpenAi);
        let call = turn.message.tool_calls().next().unwrap();
        assert_eq!(call.id, "call_1");
        assert_eq!(call.name, "search_document");
        assert_eq!(call.arguments_json().unwrap()["query"], "rust");
        assert!(
            events
                .iter()
                .any(|event| matches!(event, AgentEvent::ToolCallCompleted(_)))
        );
    }

    #[test]
    fn tool_result_text_is_bounded_without_removing_images() {
        let image = AgentImage::new("image/png", 1, 1, Arc::<[u8]>::from([1]));
        let result = AgentToolResult {
            call_id: "call".to_string(),
            tool_name: "read".to_string(),
            content: vec![
                AgentToolResultPart::Text("abcdefgh".to_string()),
                AgentToolResultPart::Image(image.clone()),
            ],
            is_error: false,
        }
        .bounded(4);
        assert!(matches!(&result.content[1], AgentToolResultPart::Image(value) if value == &image));
        assert!(
            matches!(&result.content[0], AgentToolResultPart::Text(value) if value.starts_with("abcd"))
        );
    }

    #[test]
    fn anthropic_caches_the_prefix_and_thinks_adaptively() {
        let mut settings = settings(AiApiStyle::Anthropic);
        settings.model = "claude-opus-5-5".to_string();
        settings.base_url = "https://api.anthropic.com".to_string();
        settings.thinking_level = AiThinkingLevel::High;
        let body = anthropic_body(
            &settings,
            "system",
            &[AgentMessage::user_text("question")],
            &[tool(), tool()],
        );

        assert_eq!(body["system"][0]["cache_control"]["type"], "ephemeral");
        assert!(body["tools"][0].get("cache_control").is_none());
        assert_eq!(body["tools"][1]["cache_control"]["type"], "ephemeral");
        assert_eq!(
            body["messages"][0]["content"][0]["cache_control"]["type"],
            "ephemeral"
        );
        assert_eq!(body["thinking"]["type"], "adaptive");
        assert_eq!(body["output_config"]["effort"], "high");
        assert_eq!(body["max_tokens"], 64_000);
        assert_eq!(body["fallbacks"], "default");
        assert!(anthropic_fallbacks(&settings));

        settings.base_url = "https://proxy.example".to_string();
        assert!(
            anthropic_body(&settings, "system", &[], &[])
                .get("fallbacks")
                .is_none()
        );
    }

    #[test]
    fn responses_pass_the_cache_key() {
        let body = responses_body(
            &settings(AiApiStyle::OpenAiResponses),
            "system",
            &[],
            &[],
            Some("thread-7"),
        );
        assert_eq!(body["prompt_cache_key"], "thread-7");
    }

    #[test]
    fn reasoning_streams_from_every_provider() {
        let mut events = Vec::new();
        let mut send = |event| {
            events.push(event);
            Ok(())
        };
        let mut anthropic = ProviderTurnBuilder::default();
        for value in [
            json!({"type": "content_block_start", "index": 0, "content_block": {"type": "thinking", "thinking": ""}}),
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "thinking_delta", "thinking": "Look at page 3."}}),
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "signature_delta", "signature": "sig"}}),
            json!({"type": "message_delta", "delta": {"stop_reason": "refusal"}, "usage": {"output_tokens": 1}}),
        ] {
            process_anthropic_event(&value, &mut anthropic, &mut send).unwrap();
        }
        let turn = anthropic.finish(AiApiStyle::Anthropic);
        assert_eq!(turn.stop_reason, AgentStopReason::Refusal);
        assert_eq!(
            turn.message.content[0],
            AgentContentPart::Reasoning("Look at page 3.".to_string())
        );
        assert!(matches!(
            &turn.message.content[1],
            AgentContentPart::OpaqueReasoning { value, .. }
                if value["thinking"] == "Look at page 3." && value["signature"] == "sig"
        ));

        let mut responses = ProviderTurnBuilder::default();
        process_responses_event(
            &json!({"type": "response.reasoning_summary_text.delta", "delta": "Summary"}),
            &mut responses,
            &mut send,
        )
        .unwrap();
        let mut openai = ProviderTurnBuilder::default();
        process_openai_event(
            &json!({"choices": [{"delta": {"reasoning_content": "Hmm"}}]}),
            &mut openai,
            &mut send,
        )
        .unwrap();
        assert_eq!(responses.reasoning, "Summary");
        assert_eq!(openai.reasoning, "Hmm");
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, AgentEvent::ReasoningDelta(_)))
                .count(),
            3
        );
    }

    #[test]
    fn a_fallback_drops_what_the_declined_model_began() {
        let mut builder = ProviderTurnBuilder::default();
        let mut send = |_| Ok(());
        for value in [
            json!({"type": "content_block_start", "index": 0, "content_block": {"type": "tool_use", "id": "t1", "name": "search_document"}}),
            json!({"type": "content_block_start", "index": 1, "content_block": {"type": "fallback"}}),
            json!({"type": "content_block_delta", "index": 2, "delta": {"type": "text_delta", "text": "Answer"}}),
        ] {
            process_anthropic_event(&value, &mut builder, &mut send).unwrap();
        }
        let turn = builder.finish(AiApiStyle::Anthropic);
        assert_eq!(turn.message.tool_calls().count(), 0);
        assert_eq!(turn.message.text(), "Answer");
    }

    #[test]
    fn retries_follow_retry_after_and_reasoning_errors_are_recognized() {
        let error = anyhow::Error::from(ProviderStatusError {
            status: reqwest::StatusCode::TOO_MANY_REQUESTS,
            retry_after: Some(Duration::from_secs(2)),
            body: String::new(),
        });
        assert!(retryable_agent_error(&error));
        assert_eq!(retry_delay(&error, 1), Duration::from_secs(2));
        let bad = anyhow::Error::from(ProviderStatusError {
            status: reqwest::StatusCode::BAD_REQUEST,
            retry_after: None,
            body: "messages.5.content.0: Invalid `signature` in `thinking` block.".to_string(),
        });
        assert!(!retryable_agent_error(&bad));
        assert!(invalid_reasoning(&bad));
        assert_eq!(retry_delay(&bad, 2), Duration::from_millis(1000));

        let mut messages = vec![AgentMessage {
            role: AgentRole::Assistant,
            content: vec![
                AgentContentPart::OpaqueReasoning {
                    api_style: AiApiStyle::Anthropic,
                    value: json!({}),
                },
                AgentContentPart::Text("kept".to_string()),
            ],
        }];
        strip_reasoning(&mut messages, AiApiStyle::Anthropic);
        assert_eq!(
            messages[0].content,
            vec![AgentContentPart::Text("kept".to_string())]
        );
    }

    #[test]
    fn reads_run_together_and_changes_run_alone_in_order() {
        use std::{sync::Mutex, time::Instant};

        use crate::tools::Tool;

        struct Timed {
            name: &'static str,
            effect: ToolEffect,
            log: Arc<Mutex<Vec<(String, Instant, Instant)>>>,
        }

        impl Tool for Timed {
            fn definition(&self) -> AgentToolDefinition {
                AgentToolDefinition::new(self.name, "Timed.", json!({"type": "object"}))
            }

            fn effect(&self) -> ToolEffect {
                self.effect
            }

            fn call(&self, call: &AgentToolCall, _cx: &ToolContext) -> AgentToolResult {
                let start = Instant::now();
                thread::sleep(Duration::from_millis(60));
                self.log
                    .lock()
                    .unwrap()
                    .push((call.id.clone(), start, Instant::now()));
                AgentToolResult::text(&call.id, &call.name, call.id.clone())
            }
        }

        let log = Arc::new(Mutex::new(Vec::new()));
        let registry = ToolRegistry::new()
            .with(Timed {
                name: "read",
                effect: ToolEffect::Read,
                log: log.clone(),
            })
            .with(Timed {
                name: "change",
                effect: ToolEffect::Mutate,
                log: log.clone(),
            });
        let call = |id: &str, name: &str| AgentToolCall {
            id: id.to_string(),
            name: name.to_string(),
            arguments: "{}".to_string(),
        };
        let calls = [
            call("a", "read"),
            call("b", "read"),
            call("c", "change"),
            call("d", "read"),
        ];
        let results = execute_tool_calls(
            &registry,
            &calls,
            &ToolContext::default(),
            1000,
            &mut |_| Ok(()),
        )
        .unwrap();

        let ids = results
            .iter()
            .map(|result| result.call_id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(ids, ["a", "b", "c", "d"]);
        let log = log.lock().unwrap();
        let span = |id: &str| {
            let (_, start, end) = log.iter().find(|(call, ..)| call == id).unwrap();
            (*start, *end)
        };
        let (a, b, c, d) = (span("a"), span("b"), span("c"), span("d"));
        assert!(a.0 < b.1 && b.0 < a.1, "the two reads overlap");
        assert!(c.0 >= a.1.max(b.1), "the change waits for the reads");
        assert!(d.0 >= c.1, "a read after the change waits for it");
    }
}
