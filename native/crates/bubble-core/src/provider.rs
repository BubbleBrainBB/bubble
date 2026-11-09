//! Streams a reply from a model: OpenAI Chat Completions and Anthropic
//! Messages, the two protocols nearly every configured provider speaks,
//! with tool calls.

use std::collections::BTreeMap;

use anyhow::{Context, Result, anyhow, bail};
use futures_util::StreamExt;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::auth::{AuthFile, Token};
use crate::config::{Dialect, Protocol, ResolvedModel};
use crate::model::{self, Echo, Level};
use crate::sse::{SseEvent, SseParser};

const USER_AGENT: &str = concat!("bubble-native/", env!("CARGO_PKG_VERSION"));
const ANTHROPIC_VERSION: &str = "2023-06-01";
/// The Codex client version ChatGPT's backend sees (`CODEX_CLIENT_VERSION`).
const CODEX_CLIENT_VERSION: &str = "0.159.0";
const MAX_RETRIES: u32 = 4;
const RETRYABLE: &[u16] = &[408, 429, 500, 502, 503, 504, 529];

/// One id for this process's requests (Codex `session_id`, cache seeds).
fn session_id() -> &'static str {
    static ID: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    ID.get_or_init(|| {
        let seed = format!("{:?}-{}", std::time::SystemTime::now(), std::process::id());
        let hex = crate::responses::cache_key("session", &seed);
        format!("{}-{}-{}-{}-{}", &hex[0..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..32])
    })
}

/// How long to wait before retry `attempt` (from 1): equal-jitter backoff,
/// or what the provider asked for, up to a minute.
fn backoff(attempt: u32, retry_after: Option<std::time::Duration>) -> std::time::Duration {
    if let Some(wait) = retry_after {
        return wait.min(std::time::Duration::from_secs(60));
    }
    let cap = (1000u64 << (attempt - 1).min(5)).min(32_000);
    let jitter = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64 % (cap / 2 + 1))
        .unwrap_or(0);
    std::time::Duration::from_millis(cap / 2 + jitter)
}

/// A call the model asked for: `arguments` is its JSON text, as streamed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

impl ToolCall {
    /// The arguments as JSON; an empty or broken text is an empty object.
    pub fn arguments_json(&self) -> Value {
        serde_json::from_str(&self.arguments).unwrap_or_else(|_| json!({}))
    }
}

/// What a tool returned to the model.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolResult {
    pub call_id: String,
    pub content: String,
    #[serde(default)]
    pub is_error: bool,
}

/// One message of a conversation as the model sees it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum ChatMessage {
    User {
        text: String,
    },
    Assistant {
        #[serde(default)]
        text: String,
        /// The model's reasoning, kept because some providers want it sent
        /// back with tool calls.
        #[serde(default)]
        reasoning: String,
        #[serde(default)]
        tool_calls: Vec<ToolCall>,
    },
    Tool {
        results: Vec<ToolResult>,
    },
}

impl ChatMessage {
    pub fn user(text: impl Into<String>) -> Self {
        ChatMessage::User { text: text.into() }
    }

    pub fn assistant(text: impl Into<String>) -> Self {
        ChatMessage::Assistant { text: text.into(), reasoning: String::new(), tool_calls: Vec::new() }
    }
}

/// A tool offered to the model: its name, what it does, and a JSON schema
/// of its arguments.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

#[derive(Clone, Debug)]
pub struct ChatRequest {
    pub system: Option<String>,
    pub messages: Vec<ChatMessage>,
    pub tools: Vec<ToolSpec>,
    pub max_tokens: u32,
    /// How hard the model should think; clamped to what it offers.
    pub thinking: Level,
}

/// What arrives while a reply streams.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StreamEvent {
    Text(String),
    Reasoning(String),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    /// The whole prompt, cached parts included.
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// Of `input_tokens`: read from the provider's prompt cache.
    pub cache_read_tokens: u64,
    /// Of `input_tokens`: written to the cache (Anthropic).
    pub cache_creation_tokens: u64,
    /// Of `output_tokens`: reasoning, where the provider says.
    pub reasoning_tokens: u64,
}

impl Usage {
    /// An OpenAI Chat Completions `usage` (DeepSeek's cache fields too).
    pub fn from_chat(usage: &Value) -> Usage {
        let input = usage["prompt_tokens"].as_u64().unwrap_or(0);
        let cached = usage["prompt_cache_hit_tokens"].as_u64().or_else(|| usage["prompt_tokens_details"]["cached_tokens"].as_u64());
        Usage {
            input_tokens: input,
            output_tokens: usage["completion_tokens"].as_u64().unwrap_or(0),
            cache_read_tokens: cached.unwrap_or(0).min(input),
            cache_creation_tokens: 0,
            reasoning_tokens: usage["completion_tokens_details"]["reasoning_tokens"].as_u64().unwrap_or(0),
        }
    }

    /// A Responses API `usage`.
    pub fn from_responses(usage: &Value) -> Usage {
        let input = usage["input_tokens"].as_u64().unwrap_or(0);
        Usage {
            input_tokens: input,
            output_tokens: usage["output_tokens"].as_u64().unwrap_or(0),
            cache_read_tokens: usage["input_tokens_details"]["cached_tokens"].as_u64().unwrap_or(0).min(input),
            cache_creation_tokens: 0,
            reasoning_tokens: usage["output_tokens_details"]["reasoning_tokens"].as_u64().unwrap_or(0),
        }
    }

    /// An Anthropic `usage`, merged into what came before (a
    /// `message_delta` may repeat some fields and not others). Anthropic's
    /// `input_tokens` leave the cached parts out.
    pub fn merge_anthropic(&mut self, usage: &Value) {
        let uncached_before = self.input_tokens.saturating_sub(self.cache_read_tokens + self.cache_creation_tokens);
        let uncached = usage["input_tokens"].as_u64().unwrap_or(uncached_before);
        let read = usage["cache_read_input_tokens"].as_u64().unwrap_or(self.cache_read_tokens);
        let created = usage["cache_creation_input_tokens"].as_u64().unwrap_or(self.cache_creation_tokens);
        self.input_tokens = uncached + read + created;
        self.cache_read_tokens = read;
        self.cache_creation_tokens = created;
        if let Some(output) = usage["output_tokens"].as_u64() {
            self.output_tokens = output;
        }
    }
}

/// The whole reply, once the stream ends.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChatOutcome {
    pub text: String,
    pub reasoning: String,
    pub tool_calls: Vec<ToolCall>,
    pub stop_reason: Option<String>,
    pub usage: Option<Usage>,
}

impl ChatOutcome {
    /// The reply as a message for the history.
    pub fn to_message(&self) -> ChatMessage {
        ChatMessage::Assistant {
            text: self.text.clone(),
            reasoning: self.reasoning.clone(),
            tool_calls: self.tool_calls.clone(),
        }
    }
}

/// An HTTP client for model calls. It uses the system's proxy settings.
pub fn http_client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .connect_timeout(std::time::Duration::from_secs(20))
        .build()?)
}

/// Streams one reply, calling `on_event` for each piece as it arrives.
/// Failures before anything arrived are retried; a subscription token the
/// provider refuses is refreshed once.
pub async fn stream_chat(
    client: &reqwest::Client,
    model: &ResolvedModel,
    request: &ChatRequest,
    mut on_event: impl FnMut(StreamEvent),
) -> Result<ChatOutcome> {
    let (url, body) = match model.protocol {
        Protocol::OpenAiChat => (format!("{}/chat/completions", model.base_url), openai_body(model, request)),
        Protocol::AnthropicMessages => (anthropic_url(&model.base_url), anthropic_body(model, request)),
        Protocol::Responses(dialect) => {
            (crate::responses::url(dialect, &model.base_url), crate::responses::body(dialect, model, request, session_id()))
        }
    };
    let mut attempt = 0u32;
    let mut force_refresh = false;
    let response = loop {
        let token = match model.subscription {
            Some(subscription) => Some(AuthFile::default().token(client, subscription, force_refresh).await?),
            None => None,
        };
        let sent = client.post(&url).headers(headers(model, token.as_ref())?).json(&body).send().await;
        let response = match sent {
            Ok(response) => response,
            Err(error) if attempt < MAX_RETRIES && (error.is_connect() || error.is_timeout() || error.is_request()) => {
                attempt += 1;
                tokio::time::sleep(backoff(attempt, None)).await;
                continue;
            }
            Err(error) => return Err(error).with_context(|| format!("calling {}", model.provider.display_name())),
        };
        let status = response.status();
        if status.is_success() {
            break response;
        }
        let retry_after = response
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<u64>().ok())
            .map(std::time::Duration::from_secs);
        let text = response.text().await.unwrap_or_default();
        if status.as_u16() == 401 && model.subscription.is_some() && !force_refresh {
            force_refresh = true;
            continue;
        }
        if RETRYABLE.contains(&status.as_u16()) && attempt < MAX_RETRIES {
            attempt += 1;
            tokio::time::sleep(backoff(attempt, retry_after)).await;
            continue;
        }
        bail!("{} answered {status}: {}", model.provider.display_name(), error_text(&text));
    };

    let mut state = StreamState::default();
    let mut parser = SseParser::new();
    let mut stream = response.bytes_stream();
    let mut ended = false;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("reading the reply")?;
        for event in parser.push(&chunk) {
            ended |= state.handle(model.protocol, &event, &mut on_event)?;
        }
        if ended {
            break;
        }
    }
    if !ended {
        if let Some(event) = parser.finish() {
            state.handle(model.protocol, &event, &mut on_event)?;
        }
    }
    Ok(state.finish())
}

/// A reply being assembled from its events.
#[derive(Default)]
struct StreamState {
    outcome: ChatOutcome,
    /// Tool calls by the stream's index for them.
    calls: BTreeMap<u64, ToolCall>,
    /// Reasoning summary parts seen (Responses APIs).
    reasoning_parts: usize,
}

impl StreamState {
    fn handle(&mut self, protocol: Protocol, event: &SseEvent, on_event: &mut impl FnMut(StreamEvent)) -> Result<bool> {
        match protocol {
            Protocol::OpenAiChat => self.handle_openai(event, on_event),
            Protocol::AnthropicMessages => self.handle_anthropic(event, on_event),
            Protocol::Responses(_) => self.handle_responses(event, on_event),
        }
    }

    fn handle_responses(&mut self, event: &SseEvent, on_event: &mut impl FnMut(StreamEvent)) -> Result<bool> {
        let data = event.data.trim();
        if data == "[DONE]" {
            return Ok(true);
        }
        let value: Value = serde_json::from_str(data).with_context(|| format!("an unreadable event: {data}"))?;
        let kind = value["type"].as_str().unwrap_or(event.event.as_str()).to_owned();
        let index = value["output_index"].as_u64().unwrap_or(0);
        match kind.as_str() {
            "error" => bail!("the provider reported an error: {}", value["message"].as_str().map(str::to_owned).unwrap_or_else(|| value.to_string())),
            "response.failed" => bail!(
                "the provider reported an error: {}",
                value["response"]["error"]["message"].as_str().unwrap_or("the response failed")
            ),
            "response.output_item.added" | "response.output_item.done" if value["item"]["type"] == "function_call" => {
                let item = &value["item"];
                let call = self.calls.entry(index).or_insert_with(|| ToolCall {
                    id: String::new(),
                    name: String::new(),
                    arguments: String::new(),
                });
                if let Some(id) = item["call_id"].as_str().filter(|s| !s.is_empty()) {
                    call.id = id.to_owned();
                }
                if let Some(name) = item["name"].as_str().filter(|s| !s.is_empty()) {
                    call.name = name.to_owned();
                }
                if let Some(arguments) = item["arguments"].as_str() {
                    if arguments.len() > call.arguments.len() {
                        call.arguments = arguments.to_owned();
                    }
                }
            }
            "response.output_text.delta" | "response.refusal.delta" => self.text(value["delta"].as_str().unwrap_or_default(), on_event),
            "response.reasoning_summary_part.added" => {
                if self.reasoning_parts > 0 {
                    self.reasoning("\n\n", on_event);
                }
                self.reasoning_parts += 1;
            }
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                self.reasoning(value["delta"].as_str().unwrap_or_default(), on_event)
            }
            "response.function_call_arguments.delta" => {
                if let Some(call) = self.calls.get_mut(&index) {
                    call.arguments.push_str(value["delta"].as_str().unwrap_or_default());
                }
            }
            "response.function_call_arguments.done" => {
                if let (Some(call), Some(arguments)) = (self.calls.get_mut(&index), value["arguments"].as_str()) {
                    if arguments.len() > call.arguments.len() {
                        call.arguments = arguments.to_owned();
                    }
                }
            }
            "response.completed" | "response.done" | "response.incomplete" => {
                let usage = &value["response"]["usage"];
                if usage.is_object() {
                    self.outcome.usage = Some(Usage::from_responses(usage));
                }
                self.outcome.stop_reason = Some(if kind == "response.incomplete" { "incomplete".into() } else { "stop".into() });
                return Ok(true);
            }
            _ => {}
        }
        Ok(false)
    }

    fn finish(mut self) -> ChatOutcome {
        self.outcome.tool_calls = self
            .calls
            .into_values()
            .filter(|call| !call.name.is_empty())
            .enumerate()
            .map(|(index, mut call)| {
                if call.id.is_empty() {
                    call.id = format!("call_{index}");
                }
                call
            })
            .collect();
        self.outcome
    }

    fn text(&mut self, piece: &str, on_event: &mut impl FnMut(StreamEvent)) {
        if !piece.is_empty() {
            self.outcome.text.push_str(piece);
            on_event(StreamEvent::Text(piece.to_owned()));
        }
    }

    fn reasoning(&mut self, piece: &str, on_event: &mut impl FnMut(StreamEvent)) {
        if !piece.is_empty() {
            self.outcome.reasoning.push_str(piece);
            on_event(StreamEvent::Reasoning(piece.to_owned()));
        }
    }

    fn handle_openai(&mut self, event: &SseEvent, on_event: &mut impl FnMut(StreamEvent)) -> Result<bool> {
        let data = event.data.trim();
        if data == "[DONE]" {
            return Ok(true);
        }
        let value: Value = serde_json::from_str(data).with_context(|| format!("an unreadable chunk: {data}"))?;
        if let Some(error) = value.get("error") {
            bail!("the provider reported an error: {}", message_of(error));
        }
        if let Some(choice) = value.get("choices").and_then(|c| c.get(0)) {
            let delta = &choice["delta"];
            for key in ["reasoning_content", "reasoning", "thinking"] {
                if let Some(piece) = delta.get(key).and_then(Value::as_str) {
                    self.reasoning(piece, on_event);
                }
            }
            // MiniMax with reasoning_split.
            if let Some(details) = delta.get("reasoning_details").and_then(Value::as_array) {
                for detail in details {
                    if let Some(piece) = detail.get("text").and_then(Value::as_str) {
                        self.reasoning(piece, on_event);
                    }
                }
            }
            if let Some(piece) = delta.get("content").and_then(Value::as_str) {
                self.text(piece, on_event);
            }
            if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
                for (position, call) in calls.iter().enumerate() {
                    let index = call["index"].as_u64().unwrap_or(position as u64);
                    let entry = self.calls.entry(index).or_insert_with(|| ToolCall {
                        id: String::new(),
                        name: String::new(),
                        arguments: String::new(),
                    });
                    if let Some(id) = call["id"].as_str().filter(|id| !id.is_empty()) {
                        entry.id = id.to_owned();
                    }
                    if let Some(name) = call["function"]["name"].as_str().filter(|n| !n.is_empty()) {
                        entry.name = name.to_owned();
                    }
                    if let Some(arguments) = call["function"]["arguments"].as_str() {
                        entry.arguments.push_str(arguments);
                    }
                }
            }
            if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
                self.outcome.stop_reason = Some(reason.to_owned());
            }
        }
        if let Some(usage) = value.get("usage").filter(|u| u.is_object()) {
            self.outcome.usage = Some(Usage::from_chat(usage));
        }
        Ok(false)
    }

    fn handle_anthropic(&mut self, event: &SseEvent, on_event: &mut impl FnMut(StreamEvent)) -> Result<bool> {
        let value: Value = serde_json::from_str(event.data.trim())
            .with_context(|| format!("an unreadable event: {}", event.data))?;
        let kind = value["type"].as_str().unwrap_or(event.event.as_str());
        match kind {
            "message_start" => {
                let usage = &value["message"]["usage"];
                self.outcome.usage.get_or_insert_with(Usage::default).merge_anthropic(usage);
            }
            "content_block_start" => {
                let block = &value["content_block"];
                if block["type"] == "tool_use" {
                    let index = value["index"].as_u64().unwrap_or(self.calls.len() as u64);
                    self.calls.insert(
                        index,
                        ToolCall {
                            id: block["id"].as_str().unwrap_or_default().to_owned(),
                            name: block["name"].as_str().unwrap_or_default().to_owned(),
                            arguments: String::new(),
                        },
                    );
                }
            }
            "content_block_delta" => {
                let delta = &value["delta"];
                match delta["type"].as_str() {
                    Some("text_delta") => self.text(delta["text"].as_str().unwrap_or_default(), on_event),
                    Some("thinking_delta") => self.reasoning(delta["thinking"].as_str().unwrap_or_default(), on_event),
                    Some("input_json_delta") => {
                        let index = value["index"].as_u64().unwrap_or(0);
                        if let Some(call) = self.calls.get_mut(&index) {
                            call.arguments.push_str(delta["partial_json"].as_str().unwrap_or_default());
                        }
                    }
                    _ => {}
                }
            }
            "message_delta" => {
                if let Some(reason) = value["delta"]["stop_reason"].as_str() {
                    self.outcome.stop_reason = Some(reason.to_owned());
                }
                if value["usage"].is_object() {
                    self.outcome.usage.get_or_insert_with(Usage::default).merge_anthropic(&value["usage"]);
                }
            }
            "message_stop" => return Ok(true),
            "error" => bail!("the provider reported an error: {}", message_of(&value["error"])),
            _ => {}
        }
        Ok(false)
    }
}

fn headers(model: &ResolvedModel, token: Option<&Token>) -> Result<HeaderMap> {
    let mut headers = HeaderMap::new();
    let secret = token.map(|t| t.access_token.as_str()).unwrap_or(&model.api_key);
    match model.protocol {
        Protocol::AnthropicMessages => {
            headers.insert("x-api-key", HeaderValue::from_str(secret)?);
            headers.insert("anthropic-version", HeaderValue::from_static(ANTHROPIC_VERSION));
        }
        _ => {
            headers.insert("authorization", HeaderValue::from_str(&format!("Bearer {secret}"))?);
        }
    }
    if model.protocol == Protocol::Responses(Dialect::Codex) {
        let account = token
            .and_then(|t| t.account_id.clone())
            .ok_or_else(|| anyhow!("the ChatGPT sign-in has no account id; sign in again"))?;
        headers.insert("chatgpt-account-id", HeaderValue::from_str(&account)?);
        headers.insert("originator", HeaderValue::from_static("codex_cli_rs"));
        headers.insert("user-agent", HeaderValue::from_str(&format!("codex_cli_rs/{CODEX_CLIENT_VERSION} (bubble)"))?);
        headers.insert("session_id", HeaderValue::from_str(session_id())?);
        headers.insert("openai-beta", HeaderValue::from_static("responses=experimental"));
        if model::codex_lite(&model.model) {
            headers.insert("x-openai-internal-codex-responses-lite", HeaderValue::from_static("true"));
            for name in ["thread_id", "x-codex-window-id", "x-codex-installation-id"] {
                headers.insert(HeaderName::from_static(name), HeaderValue::from_str(session_id())?);
            }
        }
    }
    headers.insert("accept", HeaderValue::from_static("text/event-stream"));
    for (name, value) in &model.provider.headers {
        headers.insert(HeaderName::from_bytes(name.as_bytes())?, HeaderValue::from_str(value)?);
    }
    Ok(headers)
}

/// Providers whose streams carry token usage only when asked for it
/// (mirrors `shouldRequestStreamUsage` in the TypeScript core).
fn wants_stream_usage(model: &ResolvedModel) -> bool {
    const IDS: &[&str] = &[
        "openai",
        "opencode-zen",
        "deepseek",
        "moonshot-cn",
        "moonshot-intl",
        "kimi-for-coding",
        "zhipuai",
        "zhipuai-coding-plan",
        "zai",
        "zai-coding-plan",
        "bailian-token-plan",
        "stepfun-api",
    ];
    let id = model.provider.id.to_ascii_lowercase();
    IDS.contains(&id.as_str())
        || model.base_url.contains("api.minimax.io/v1")
        || model.base_url.contains("api.minimaxi.com/v1")
}

fn openai_body(model: &ResolvedModel, request: &ChatRequest) -> Value {
    let reasoning = model::openai_reasoning(&model.provider.id, &model.model, request.thinking);
    let reasoning_echo = reasoning.echo;
    let mut messages = Vec::new();
    if let Some(system) = request.system.as_deref().filter(|s| !s.trim().is_empty()) {
        messages.push(json!({ "role": "system", "content": system }));
    }
    for message in &request.messages {
        match message {
            ChatMessage::User { text } => messages.push(json!({ "role": "user", "content": text })),
            ChatMessage::Assistant { text, reasoning, tool_calls } => {
                let mut out = json!({
                    "role": "assistant",
                    "content": if text.is_empty() { Value::Null } else { json!(text) },
                });
                if !tool_calls.is_empty() {
                    out["tool_calls"] = tool_calls
                        .iter()
                        .map(|call| {
                            json!({
                                "id": call.id,
                                "type": "function",
                                "function": {
                                    "name": call.name,
                                    "arguments": if call.arguments.trim().is_empty() { "{}" } else { call.arguments.as_str() },
                                },
                            })
                        })
                        .collect();
                }
                match reasoning_echo {
                    Echo::Always => out["reasoning_content"] = json!(reasoning),
                    Echo::ToolCalls if !tool_calls.is_empty() && !reasoning.is_empty() => {
                        out["reasoning_content"] = json!(reasoning)
                    }
                    Echo::Minimax if !reasoning.is_empty() => {
                        out["reasoning_details"] = json!([{
                            "type": "reasoning.text",
                            "id": "reasoning-text-1",
                            "format": "MiniMax-response-v1",
                            "index": 0,
                            "text": reasoning,
                        }])
                    }
                    _ => {}
                }
                messages.push(out);
            }
            ChatMessage::Tool { results } => {
                for result in results {
                    messages.push(json!({
                        "role": "tool",
                        "tool_call_id": result.call_id,
                        "content": result.content,
                    }));
                }
            }
        }
    }
    let mut body = json!({
        "model": model.model,
        "messages": messages,
        "stream": true,
        "max_tokens": request.max_tokens,
    });
    if !request.tools.is_empty() {
        body["tools"] = request
            .tools
            .iter()
            .map(|tool| {
                json!({
                    "type": "function",
                    "function": { "name": tool.name, "description": tool.description, "parameters": tool.parameters },
                })
            })
            .collect();
    }
    if wants_stream_usage(model) {
        body["stream_options"] = json!({ "include_usage": true });
    }
    if !reasoning.omit_temperature {
        body["temperature"] = json!(0.2);
    }
    for (key, value) in reasoning.fields {
        body[key] = value;
    }
    if !request.tools.is_empty() && model::serial_tool_calls(&model.provider.id, &model.model) {
        body["parallel_tool_calls"] = json!(false);
    }
    body
}

fn anthropic_url(base_url: &str) -> String {
    if base_url.ends_with("/v1") {
        format!("{base_url}/messages")
    } else {
        format!("{base_url}/v1/messages")
    }
}

fn anthropic_body(model: &ResolvedModel, request: &ChatRequest) -> Value {
    let messages: Vec<Value> = request
        .messages
        .iter()
        .map(|message| match message {
            ChatMessage::User { text } => json!({ "role": "user", "content": text }),
            ChatMessage::Assistant { text, tool_calls, .. } => {
                let mut blocks = Vec::new();
                if !text.is_empty() {
                    blocks.push(json!({ "type": "text", "text": text }));
                }
                for call in tool_calls {
                    blocks.push(json!({
                        "type": "tool_use",
                        "id": call.id,
                        "name": call.name,
                        "input": call.arguments_json(),
                    }));
                }
                if blocks.is_empty() {
                    blocks.push(json!({ "type": "text", "text": "…" }));
                }
                json!({ "role": "assistant", "content": blocks })
            }
            ChatMessage::Tool { results } => json!({
                "role": "user",
                "content": results
                    .iter()
                    .map(|result| json!({
                        "type": "tool_result",
                        "tool_use_id": result.call_id,
                        "content": result.content,
                        "is_error": result.is_error,
                    }))
                    .collect::<Vec<_>>(),
            }),
        })
        .collect();
    let reasoning = model::anthropic_reasoning(&model.provider.id, &model.model, &model.base_url, request.thinking);
    let mut body = json!({
        "model": model.model,
        "max_tokens": reasoning.max_tokens.max(request.max_tokens.min(8192)),
        "messages": messages,
        "stream": true,
    });
    for (key, value) in reasoning.fields {
        body[key] = value;
    }
    if let Some(system) = request.system.as_deref().filter(|s| !s.trim().is_empty()) {
        body["system"] = json!(system);
    }
    if !request.tools.is_empty() {
        body["tools"] = request
            .tools
            .iter()
            .map(|tool| json!({ "name": tool.name, "description": tool.description, "input_schema": tool.parameters }))
            .collect();
    }
    body
}

fn message_of(error: &Value) -> String {
    error
        .get("message")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| error.to_string())
}

/// The useful part of an error body, kept short.
fn error_text(body: &str) -> String {
    let message = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|value| {
            value
                .get("error")
                .map(message_of)
                .or_else(|| value.get("message").and_then(Value::as_str).map(str::to_owned))
        })
        .unwrap_or_else(|| body.trim().to_owned());
    let mut short: String = message.chars().take(400).collect();
    if short.len() < message.len() {
        short.push('…');
    }
    if short.is_empty() { "(no details)".to_owned() } else { short }
}

/// Fails with a readable message when nothing came back.
pub fn require_reply(outcome: &ChatOutcome) -> Result<()> {
    if outcome.text.trim().is_empty() && outcome.reasoning.trim().is_empty() && outcome.tool_calls.is_empty() {
        return Err(anyhow!("the model returned an empty reply"));
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::config::Config;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// Serves the given bodies, one per request, as event streams; returns
    /// the base URL and a handle that yields the requests received.
    pub(crate) async fn serve(status: &'static str, bodies: Vec<String>) -> (String, tokio::task::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            let mut requests = Vec::new();
            for body in bodies {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                let mut buffer = [0u8; 8192];
                loop {
                    let n = socket.read(&mut buffer).await.unwrap();
                    request.extend_from_slice(&buffer[..n]);
                    let text = String::from_utf8_lossy(&request);
                    if let Some(head_end) = text.find("\r\n\r\n") {
                        let length = text[..head_end]
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse::<usize>().unwrap())
                            })
                            .unwrap_or(0);
                        if request.len() >= head_end + 4 + length {
                            break;
                        }
                    }
                    if n == 0 {
                        break;
                    }
                }
                let response = format!(
                    "HTTP/1.1 {status}\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n{body}"
                );
                socket.write_all(response.as_bytes()).await.unwrap();
                socket.shutdown().await.ok();
                requests.push(String::from_utf8_lossy(&request).into_owned());
            }
            requests
        });
        (format!("http://{address}"), handle)
    }

    pub(crate) fn model(base_url: &str, protocol: &str, id: &str) -> ResolvedModel {
        let config = Config::parse(&format!(
            r#"{{"providers":[{{"id":"{id}","baseURL":"{base_url}","apiKey":"secret","protocol":"{protocol}","headers":{{"User-Agent":"KimiCLI/1.0"}}}}]}}"#
        ))
        .unwrap();
        config.resolve(&format!("{id}:test-model")).unwrap()
    }

    pub(crate) fn client() -> reqwest::Client {
        reqwest::Client::builder().no_proxy().build().unwrap()
    }

    fn request() -> ChatRequest {
        ChatRequest {
            system: Some("Be brief.".into()),
            messages: vec![ChatMessage::user("hi")],
            tools: Vec::new(),
            max_tokens: 256,
            thinking: Level::Off,
        }
    }

    fn body_of(request: &str) -> Value {
        let start = request.find("\r\n\r\n").unwrap() + 4;
        serde_json::from_str(&request[start..]).unwrap()
    }

    #[tokio::test]
    async fn openai_streams_text_reasoning_and_usage() {
        let (url, server) = serve(
            "200 OK",
            vec![
                "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"think\"}}]}\n\n\
                 data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\n\n\
                 data: {\"choices\":[{\"delta\":{\"content\":\"lo\"},\"finish_reason\":\"stop\"}]}\n\n\
                 data: {\"choices\":[],\"usage\":{\"prompt_tokens\":12,\"completion_tokens\":3,\"prompt_tokens_details\":{\"cached_tokens\":8}}}\n\n\
                 data: [DONE]\n\n"
                    .to_owned(),
            ],
        )
        .await;
        let model = model(&url, "openai", "kimi-for-coding");
        let mut seen = Vec::new();
        let outcome = stream_chat(&client(), &model, &request(), |e| seen.push(e)).await.unwrap();
        assert_eq!(outcome.text, "Hello");
        assert_eq!(outcome.reasoning, "think");
        assert_eq!(outcome.stop_reason.as_deref(), Some("stop"));
        assert_eq!(outcome.usage, Some(Usage { input_tokens: 12, output_tokens: 3, cache_read_tokens: 8, ..Usage::default() }));
        assert_eq!(seen.first(), Some(&StreamEvent::Reasoning("think".into())));
        let sent = &server.await.unwrap()[0];
        assert!(sent.starts_with("POST /chat/completions"));
        assert!(sent.to_ascii_lowercase().contains("authorization: bearer secret"));
        assert!(sent.contains("KimiCLI/1.0"));
        let body = body_of(sent);
        assert_eq!(body["stream_options"]["include_usage"], true);
        assert_eq!(body["messages"][0]["role"], "system");
        assert!(body.get("tools").is_none());
    }

    #[tokio::test]
    async fn openai_tool_calls_stream_in_pieces_and_go_back_with_reasoning() {
        let (url, server) = serve(
            "200 OK",
            vec![
                "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"look first\"}}]}\n\n\
                 data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_a\",\"function\":{\"name\":\"read\",\"arguments\":\"{\\\"pa\"}}]}}]}\n\n\
                 data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"th\\\":\\\"a.txt\\\"}\"}}]}}]}\n\n\
                 data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n\
                 data: [DONE]\n\n"
                    .to_owned(),
                "data: {\"choices\":[{\"delta\":{\"content\":\"done\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n".to_owned(),
            ],
        )
        .await;
        let model = model(&url, "openai", "kimi-for-coding");
        let mut request = request();
        request.tools = vec![ToolSpec { name: "read".into(), description: "Reads".into(), parameters: json!({"type":"object"}) }];
        let first = stream_chat(&client(), &model, &request, |_| {}).await.unwrap();
        assert_eq!(first.tool_calls, [ToolCall { id: "call_a".into(), name: "read".into(), arguments: "{\"path\":\"a.txt\"}".into() }]);
        assert_eq!(first.tool_calls[0].arguments_json()["path"], "a.txt");

        request.messages.push(first.to_message());
        request.messages.push(ChatMessage::Tool {
            results: vec![ToolResult { call_id: "call_a".into(), content: "hello".into(), is_error: false }],
        });
        let second = stream_chat(&client(), &model, &request, |_| {}).await.unwrap();
        assert_eq!(second.text, "done");

        let requests = server.await.unwrap();
        let first_body = body_of(&requests[0]);
        assert_eq!(first_body["tools"][0]["function"]["name"], "read");
        let second_body = body_of(&requests[1]);
        let assistant = &second_body["messages"][2];
        assert_eq!(assistant["tool_calls"][0]["id"], "call_a");
        assert_eq!(assistant["reasoning_content"], "look first");
        assert_eq!(second_body["messages"][3]["role"], "tool");
        assert_eq!(second_body["messages"][3]["tool_call_id"], "call_a");
    }

    #[tokio::test]
    async fn anthropic_streams_tool_use_and_sends_results_back() {
        let (url, server) = serve(
            "200 OK",
            vec![
                "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":20,\"output_tokens\":1}}}\n\n\
                 event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Let me look.\"}}\n\n\
                 event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"ls\",\"input\":{}}}\n\n\
                 event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\":\"}}\n\n\
                 event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"\\\"src\\\"}\"}}\n\n\
                 event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":5}}\n\n\
                 event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"
                    .to_owned(),
                "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Two files.\"}}\n\n\
                 event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"
                    .to_owned(),
            ],
        )
        .await;
        let model = model(&url, "anthropic-messages", "minimax-anthropic");
        let mut request = request();
        request.tools = vec![ToolSpec { name: "ls".into(), description: "Lists".into(), parameters: json!({"type":"object"}) }];
        let first = stream_chat(&client(), &model, &request, |_| {}).await.unwrap();
        assert_eq!(first.text, "Let me look.");
        assert_eq!(first.stop_reason.as_deref(), Some("tool_use"));
        assert_eq!(first.tool_calls[0].name, "ls");
        assert_eq!(first.tool_calls[0].arguments_json()["path"], "src");

        request.messages.push(first.to_message());
        request.messages.push(ChatMessage::Tool {
            results: vec![ToolResult { call_id: "toolu_1".into(), content: "a.rs\nb.rs".into(), is_error: false }],
        });
        let second = stream_chat(&client(), &model, &request, |_| {}).await.unwrap();
        assert_eq!(second.text, "Two files.");

        let requests = server.await.unwrap();
        assert!(requests[0].starts_with("POST /v1/messages"));
        assert!(requests[0].contains("x-api-key: secret"));
        let first_body = body_of(&requests[0]);
        assert_eq!(first_body["tools"][0]["input_schema"]["type"], "object");
        assert_eq!(first_body["system"], "Be brief.");
        let second_body = body_of(&requests[1]);
        assert_eq!(second_body["messages"][1]["content"][1]["type"], "tool_use");
        assert_eq!(second_body["messages"][1]["content"][1]["input"]["path"], "src");
        assert_eq!(second_body["messages"][2]["content"][0]["type"], "tool_result");
        assert_eq!(second_body["messages"][2]["content"][0]["tool_use_id"], "toolu_1");
    }

    #[tokio::test]
    async fn http_errors_carry_the_providers_message() {
        let (url, _server) =
            serve("401 Unauthorized", vec!["{\"error\":{\"message\":\"Invalid API key\"}}".to_owned()]).await;
        let model = model(&url, "openai", "deepseek");
        let error = stream_chat(&client(), &model, &request(), |_| {}).await.unwrap_err();
        let text = error.to_string();
        assert!(text.contains("401"), "{text}");
        assert!(text.contains("Invalid API key"), "{text}");
    }

    #[tokio::test]
    async fn errors_inside_the_stream_fail_the_call() {
        let (url, _server) = serve(
            "200 OK",
            vec!["event: error\ndata: {\"type\":\"error\",\"error\":{\"message\":\"Overloaded\"}}\n\n".to_owned()],
        )
        .await;
        let model = model(&url, "anthropic-messages", "anthropic");
        let error = stream_chat(&client(), &model, &request(), |_| {}).await.unwrap_err();
        assert!(error.to_string().contains("Overloaded"));
    }

    #[test]
    fn deepseek_gets_reasoning_back_on_every_assistant_message() {
        let mut model = model("http://x.example", "openai", "deepseek");
        model.model = "deepseek-v4-pro".into();
        let request = ChatRequest {
            system: None,
            messages: vec![
                ChatMessage::user("hi"),
                ChatMessage::Assistant { text: "hello".into(), reasoning: String::new(), tool_calls: Vec::new() },
            ],
            tools: Vec::new(),
            max_tokens: 10,
            thinking: Level::High,
        };
        let body = openai_body(&model, &request);
        assert_eq!(body["messages"][1]["reasoning_content"], "");
        assert_eq!(body["reasoning_effort"], "high");
        assert_eq!(body["thinking"]["type"], "enabled");
    }

    #[tokio::test]
    async fn responses_streams_text_reasoning_and_tool_calls() {
        let (url, server) = serve(
            "200 OK",
            vec![
                "data: {\"type\":\"response.reasoning_summary_part.added\"}\n\n\
                 data: {\"type\":\"response.reasoning_summary_text.delta\",\"delta\":\"plan\"}\n\n\
                 data: {\"type\":\"response.output_text.delta\",\"delta\":\"Looking.\"}\n\n\
                 data: {\"type\":\"response.output_item.added\",\"output_index\":1,\"item\":{\"type\":\"function_call\",\"id\":\"fc_1\",\"call_id\":\"call_9\",\"name\":\"ls\",\"arguments\":\"\"}}\n\n\
                 data: {\"type\":\"response.function_call_arguments.delta\",\"output_index\":1,\"delta\":\"{\\\"path\\\":\"}\n\n\
                 data: {\"type\":\"response.function_call_arguments.done\",\"output_index\":1,\"arguments\":\"{\\\"path\\\":\\\"src\\\"}\"}\n\n\
                 data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":30,\"output_tokens\":9}}}\n\n"
                    .to_owned(),
            ],
        )
        .await;
        let mut model = model(&url, "openai-responses", "opencode-zen");
        model.protocol = Protocol::Responses(Dialect::OpenAi);
        let outcome = stream_chat(&client(), &model, &request(), |_| {}).await.unwrap();
        assert_eq!(outcome.text, "Looking.");
        assert_eq!(outcome.reasoning, "plan");
        assert_eq!(outcome.tool_calls, [ToolCall { id: "call_9".into(), name: "ls".into(), arguments: "{\"path\":\"src\"}".into() }]);
        assert_eq!(outcome.usage, Some(Usage { input_tokens: 30, output_tokens: 9, ..Usage::default() }));
        let sent = &server.await.unwrap()[0];
        assert!(sent.starts_with("POST /responses"), "{sent}");
        assert_eq!(body_of(sent)["instructions"], "Be brief.");
    }

    #[tokio::test]
    async fn busy_providers_are_tried_again() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            for answer in [
                "HTTP/1.1 503 Service Unavailable\r\nretry-after: 0\r\ncontent-length: 4\r\nconnection: close\r\n\r\nbusy",
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\ndata: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\ndata: [DONE]\n\n",
            ] {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut buffer = [0u8; 8192];
                let _ = socket.read(&mut buffer).await;
                socket.write_all(answer.as_bytes()).await.unwrap();
                socket.shutdown().await.ok();
            }
        });
        let model = model(&format!("http://{address}"), "openai", "deepseek");
        let outcome = stream_chat(&client(), &model, &request(), |_| {}).await.unwrap();
        assert_eq!(outcome.text, "ok");
    }

    #[test]
    fn backoff_grows_and_honors_retry_after() {
        assert!(backoff(1, None) >= std::time::Duration::from_millis(500));
        assert!(backoff(3, None) >= std::time::Duration::from_millis(2000));
        assert_eq!(backoff(2, Some(std::time::Duration::from_secs(90))), std::time::Duration::from_secs(60));
    }

    #[test]
    fn anthropic_urls_accept_a_trailing_v1() {
        assert_eq!(anthropic_url("https://api.anthropic.com"), "https://api.anthropic.com/v1/messages");
        assert_eq!(anthropic_url("https://x.example/anthropic/v1"), "https://x.example/anthropic/v1/messages");
    }

    #[test]
    fn messages_round_trip_as_json() {
        let message = ChatMessage::Assistant {
            text: "hi".into(),
            reasoning: "r".into(),
            tool_calls: vec![ToolCall { id: "1".into(), name: "bash".into(), arguments: "{}".into() }],
        };
        let text = serde_json::to_string(&message).unwrap();
        assert!(text.contains("\"role\":\"assistant\""));
        assert_eq!(serde_json::from_str::<ChatMessage>(&text).unwrap(), message);
    }
}
