//! MCP (Model Context Protocol) servers: their configuration in
//! `settings.json`, a JSON-RPC client over stdio or HTTP, and a hub that keeps
//! connections alive across turns (src/mcp/ in the TypeScript core).
//!
//! Servers are configured as in Claude Code:
//!
//! ```json
//! "mcpServers": {
//!   "github": { "command": "npx", "args": ["-y", "…"], "env": { … } },
//!   "exa":    { "type": "http", "url": "https://mcp.exa.ai/mcp", "headers": { … } }
//! }
//! ```
//!
//! in `~/.bubble/settings.json`, then a project's `.bubble/settings.json` and
//! `.bubble/settings.local.json` (later wins). Their tools are offered as
//! `mcp__<server>__<tool>`.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use serde::Serialize;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::oneshot;

use crate::provider::ToolSpec;
use crate::tools::ToolOutput;

const PROTOCOL_VERSION: &str = "2025-06-18";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// A server that failed is tried again after this long.
const RETRY_AFTER: Duration = Duration::from_secs(300);
const STDERR_KEEP: usize = 4096;

// --- Configuration -------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub enum ServerConfig {
    Stdio { command: String, args: Vec<String>, env: BTreeMap<String, String>, cwd: Option<String> },
    /// Streamable HTTP; `sse` servers too, as the spec lets either answer.
    Http { url: String, headers: BTreeMap<String, String> },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum Scope {
    User,
    Project,
    Local,
}

impl Scope {
    pub fn label(self) -> &'static str {
        match self {
            Scope::User => "user",
            Scope::Project => "project",
            Scope::Local => "local",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScopedServer {
    pub name: String,
    pub scope: Scope,
    pub config: ServerConfig,
}

/// The servers configured for a project (or only the person's own, with no
/// project), and what was wrong with the files.
pub fn load_config(home: &Path, project: Option<&Path>) -> (Vec<ScopedServer>, Vec<String>) {
    let mut files = vec![(Scope::User, home.join("settings.json"))];
    if let Some(project) = project {
        files.push((Scope::Project, project.join(".bubble").join("settings.json")));
        files.push((Scope::Local, project.join(".bubble").join("settings.local.json")));
    }
    let mut diagnostics = Vec::new();
    let mut merged: Vec<ScopedServer> = Vec::new();
    for (scope, path) in files {
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        let raw: Value = match serde_json::from_str(&text) {
            Ok(raw) => raw,
            Err(error) => {
                diagnostics.push(format!("{}: failed to parse: {error}", path.display()));
                continue;
            }
        };
        let Some(servers) = raw["mcpServers"].as_object() else { continue };
        for (name, value) in servers {
            let config = match validate(value) {
                Ok(config) => expand(config, shell_env(), &mut |missing| {
                    diagnostics.push(format!("mcpServers.{name}: env var \"{missing}\" is not set; expanded to empty string."))
                }),
                Err(message) => {
                    diagnostics.push(format!("{}: mcpServers.{name}: {message}", path.display()));
                    continue;
                }
            };
            merged.retain(|s| &s.name != name);
            merged.push(ScopedServer { name: name.clone(), scope, config });
        }
    }
    (merged, diagnostics)
}

fn strings(value: &Value) -> BTreeMap<String, String> {
    value
        .as_object()
        .map(|o| o.iter().filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_owned()))).collect())
        .unwrap_or_default()
}

fn validate(value: &Value) -> std::result::Result<ServerConfig, String> {
    if !value.is_object() {
        return Err("expected an object".into());
    }
    let kind = value["type"].as_str().or(if value.get("command").is_some() { Some("stdio") } else { None });
    match kind {
        Some("stdio") => {
            let command = value["command"].as_str().unwrap_or_default().trim();
            if command.is_empty() {
                return Err("stdio server requires a non-empty \"command\"".into());
            }
            Ok(ServerConfig::Stdio {
                command: command.to_owned(),
                args: value["args"]
                    .as_array()
                    .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect())
                    .unwrap_or_default(),
                env: strings(&value["env"]),
                cwd: value["cwd"].as_str().map(str::to_owned),
            })
        }
        Some(kind @ ("http" | "sse")) => {
            let url = value["url"].as_str().unwrap_or_default().trim();
            if url.is_empty() {
                return Err(format!("{kind} server requires a non-empty \"url\""));
            }
            Ok(ServerConfig::Http { url: url.to_owned(), headers: strings(&value["headers"]) })
        }
        other => Err(format!(
            "unsupported transport type \"{}\" (expected \"stdio\", \"http\", or \"sse\")",
            other.unwrap_or("undefined")
        )),
    }
}

fn expand_text(input: &str, env: &HashMap<String, String>, missing: &mut dyn FnMut(&str)) -> String {
    static VAR: OnceLock<regex::Regex> = OnceLock::new();
    let var = VAR.get_or_init(|| regex::Regex::new(r"\$\{([A-Za-z_][A-Za-z0-9_]*)\}").unwrap());
    var.replace_all(input, |caps: &regex::Captures| match env.get(&caps[1]) {
        Some(value) => value.clone(),
        None => {
            missing(&caps[1]);
            String::new()
        }
    })
    .into_owned()
}

fn expand(config: ServerConfig, env: &HashMap<String, String>, missing: &mut dyn FnMut(&str)) -> ServerConfig {
    match config {
        ServerConfig::Stdio { command, args, env: vars, cwd } => ServerConfig::Stdio {
            command: expand_text(&command, env, missing),
            args: args.iter().map(|a| expand_text(a, env, missing)).collect(),
            env: vars.into_iter().map(|(k, v)| (k, expand_text(&v, env, missing))).collect(),
            cwd: cwd.map(|c| expand_text(&c, env, missing)),
        },
        ServerConfig::Http { url, headers } => ServerConfig::Http {
            url: expand_text(&url, env, missing),
            headers: headers.into_iter().map(|(k, v)| (k, expand_text(&v, env, missing))).collect(),
        },
    }
}

/// The environment of the person's login shell. An app opened from the
/// Finder starts with a bare environment, so `npx` and `uvx` and the
/// variables settings refer to come from here.
pub fn shell_env() -> &'static HashMap<String, String> {
    static ENV: OnceLock<HashMap<String, String>> = OnceLock::new();
    ENV.get_or_init(|| {
        let mut env: HashMap<String, String> = std::env::vars().collect();
        if cfg!(test) || std::env::var_os("BUBBLE_NO_SHELL_ENV").is_some() {
            return env;
        }
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
        const MARK: &str = "__BUBBLE_ENV__";
        let mut command = std::process::Command::new(&shell);
        command.args(["-ilc", &format!("printf '{MARK}'; env -0")]).stdin(Stdio::null()).stderr(Stdio::null());
        // An interactive shell takes the terminal's foreground otherwise.
        crate::detach(&mut command);
        let output = command.output();
        if let Ok(output) = output {
            let text = String::from_utf8_lossy(&output.stdout);
            if let Some((_, vars)) = text.split_once(MARK) {
                for entry in vars.split('\0') {
                    if let Some((key, value)) = entry.split_once('=') {
                        if !key.is_empty() && !key.contains('\n') {
                            env.insert(key.to_owned(), value.to_owned());
                        }
                    }
                }
            }
        }
        env
    })
}

// --- Names ---------------------------------------------------------------------

pub fn normalize(name: &str) -> String {
    name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' }).collect()
}

pub fn tool_name(server: &str, tool: &str) -> String {
    format!("mcp__{}__{}", normalize(server), normalize(tool))
}

/// `mcp__<server>__<tool>` back into its parts.
pub fn parse_tool_name(name: &str) -> Option<(&str, &str)> {
    let rest = name.strip_prefix("mcp__")?;
    let (server, tool) = rest.split_once("__")?;
    (!server.is_empty() && !tool.is_empty()).then_some((server, tool))
}

// --- Client --------------------------------------------------------------------

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<std::result::Result<Value, String>>>>>;

enum Link {
    Stdio { stdin: Arc<tokio::sync::Mutex<ChildStdin>>, _child: Child, stderr: Arc<Mutex<String>> },
    Http { client: reqwest::Client, url: String, headers: BTreeMap<String, String>, session: Mutex<Option<String>> },
}

/// A connection to one server.
pub struct Client {
    link: Link,
    next_id: AtomicU64,
    pending: Pending,
    closed: Arc<AtomicBool>,
    pub instructions: Option<String>,
    /// The server offers prompts.
    has_prompts: bool,
}

/// Removes a request's waiter however the request ends.
struct Waiting<'a> {
    pending: &'a Pending,
    id: u64,
}

impl Drop for Waiting<'_> {
    fn drop(&mut self) {
        if let Ok(mut pending) = self.pending.lock() {
            pending.remove(&self.id);
        }
    }
}

/// Hands a response to its waiter; returns what to answer a request from
/// the server with.
fn deliver(pending: &Pending, message: Value) -> Option<Value> {
    let id = message.get("id").cloned();
    if let Some(id) = &id {
        if message.get("result").is_some() || message.get("error").is_some() {
            let key = id.as_u64().or_else(|| id.as_str().and_then(|s| s.parse().ok()))?;
            let waiter = pending.lock().ok()?.remove(&key)?;
            let outcome = match message.get("error").filter(|e| !e.is_null()) {
                Some(error) => Err(format!(
                    "{} (code {})",
                    error["message"].as_str().unwrap_or("error"),
                    error["code"].as_i64().unwrap_or_default()
                )),
                None => Ok(message["result"].clone()),
            };
            let _ = waiter.send(outcome);
            return None;
        }
    }
    // Ping is a core request; long-lived servers close sessions that ignore it.
    let method = message["method"].as_str()?;
    let id = id?;
    Some(if method == "ping" {
        json!({ "jsonrpc": "2.0", "id": id, "result": {} })
    } else {
        json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": format!("Unsupported client method: {method}") } })
    })
}

fn fail_all(pending: &Pending, why: &str) {
    if let Ok(mut pending) = pending.lock() {
        for (_, waiter) in pending.drain() {
            let _ = waiter.send(Err(why.to_owned()));
        }
    }
}

fn tail(text: &str) -> String {
    let text = text.trim();
    if text.is_empty() { String::new() } else { format!("\nstderr:\n{text}") }
}

impl Client {
    pub async fn connect(config: &ServerConfig, http: &reqwest::Client) -> Result<Client> {
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let closed = Arc::new(AtomicBool::new(false));
        let link = match config {
            ServerConfig::Stdio { command, args, env, cwd } => {
                let mut cmd = Command::new(command);
                cmd.args(args)
                    .env_clear()
                    .envs(shell_env())
                    .envs(env)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .kill_on_drop(true);
                if let Some(cwd) = cwd {
                    cmd.current_dir(cwd);
                }
                crate::detach_async(&mut cmd);
                let mut child = cmd.spawn().with_context(|| format!("could not start {command}"))?;
                let stdin = Arc::new(tokio::sync::Mutex::new(child.stdin.take().context("no stdin")?));
                let stdout = child.stdout.take().context("no stdout")?;
                let mut err_pipe = child.stderr.take().context("no stderr")?;
                let stderr = Arc::new(Mutex::new(String::new()));
                {
                    let stderr = stderr.clone();
                    tokio::spawn(async move {
                        let mut buffer = [0u8; 2048];
                        while let Ok(n) = err_pipe.read(&mut buffer).await {
                            if n == 0 {
                                break;
                            }
                            if let Ok(mut text) = stderr.lock() {
                                text.push_str(&String::from_utf8_lossy(&buffer[..n]));
                                if text.len() > STDERR_KEEP * 2 {
                                    let cut = text.len() - STDERR_KEEP;
                                    let cut = (cut..text.len()).find(|i| text.is_char_boundary(*i)).unwrap_or(0);
                                    *text = text[cut..].to_owned();
                                }
                            }
                        }
                    });
                }
                {
                    let pending = pending.clone();
                    let closed = closed.clone();
                    let stdin = stdin.clone();
                    let stderr = stderr.clone();
                    tokio::spawn(async move {
                        let mut lines = BufReader::new(stdout).lines();
                        while let Ok(Some(line)) = lines.next_line().await {
                            let line = line.trim();
                            if line.is_empty() {
                                continue;
                            }
                            // Servers may log to stdout; only JSON counts.
                            let Ok(message) = serde_json::from_str::<Value>(line) else { continue };
                            if let Some(answer) = deliver(&pending, message) {
                                let mut stdin = stdin.lock().await;
                                let _ = stdin.write_all(format!("{answer}\n").as_bytes()).await;
                                let _ = stdin.flush().await;
                            }
                        }
                        closed.store(true, Ordering::SeqCst);
                        let stderr = stderr.lock().map(|s| s.clone()).unwrap_or_default();
                        fail_all(&pending, &format!("the server stopped{}", tail(&stderr)));
                    });
                }
                Link::Stdio { stdin, _child: child, stderr }
            }
            ServerConfig::Http { url, headers } => {
                Link::Http { client: http.clone(), url: url.clone(), headers: headers.clone(), session: Mutex::new(None) }
            }
        };
        let mut client = Client { link, next_id: AtomicU64::new(1), pending, closed, instructions: None, has_prompts: false };
        let result = client
            .request(
                "initialize",
                json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": { "name": "bubble", "version": env!("CARGO_PKG_VERSION") },
                }),
                REQUEST_TIMEOUT,
            )
            .await?;
        client.instructions = result["instructions"].as_str().map(str::to_owned);
        client.has_prompts = result["capabilities"]["prompts"].is_object();
        client.send(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })).await?;
        Ok(client)
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    fn stderr(&self) -> String {
        match &self.link {
            Link::Stdio { stderr, .. } => stderr.lock().map(|s| s.clone()).unwrap_or_default(),
            Link::Http { .. } => String::new(),
        }
    }

    async fn send(&self, message: Value) -> Result<()> {
        match &self.link {
            Link::Stdio { stdin, .. } => {
                if self.is_closed() {
                    bail!("the server stopped{}", tail(&self.stderr()));
                }
                let mut stdin = stdin.lock().await;
                stdin
                    .write_all(format!("{message}\n").as_bytes())
                    .await
                    .map_err(|e| anyhow!("the server stopped reading its input ({e}){}", tail(&self.stderr())))?;
                stdin.flush().await?;
                Ok(())
            }
            Link::Http { client, url, headers, session } => {
                let mut request = client
                    .post(url)
                    .header("content-type", "application/json")
                    .header("accept", "application/json, text/event-stream");
                for (name, value) in headers {
                    request = request.header(name, value);
                }
                // A sign-in kept for the server, unless the configuration
                // carries its own credentials.
                if !headers.keys().any(|k| k.eq_ignore_ascii_case("authorization"))
                    && let Some(token) = crate::mcp_oauth::bearer(client, url).await
                {
                    request = request.bearer_auth(token);
                }
                if let Some(id) = session.lock().ok().and_then(|s| s.clone()) {
                    request = request.header("mcp-session-id", id);
                }
                let response = request.body(message.to_string()).send().await?;
                if let Some(id) = response.headers().get("mcp-session-id").and_then(|v| v.to_str().ok()) {
                    if let Ok(mut session) = session.lock() {
                        *session = Some(id.to_owned());
                    }
                }
                let status = response.status();
                if status.as_u16() == 202 || status.as_u16() == 204 {
                    return Ok(());
                }
                if status.as_u16() == 401 {
                    let challenge = response.headers().get("www-authenticate").and_then(|v| v.to_str().ok());
                    crate::mcp_oauth::challenged(url, challenge);
                    bail!("{}: the server answered 401", crate::mcp_oauth::NEEDS_SIGN_IN);
                }
                let event_stream = response
                    .headers()
                    .get("content-type")
                    .and_then(|v| v.to_str().ok())
                    .is_some_and(|t| t.contains("text/event-stream"));
                let body = response.text().await.unwrap_or_default();
                if !status.is_success() {
                    let body: String = body.chars().take(500).collect();
                    bail!("HTTP {} from MCP server{}", status.as_u16(), if body.is_empty() { String::new() } else { format!(": {body}") });
                }
                for message in http_messages(&body, event_stream) {
                    if let Some(answer) = deliver(&self.pending, message) {
                        // Answered best-effort; an HTTP server rarely asks.
                        let _ = Box::pin(self.send(answer)).await;
                    }
                }
                Ok(())
            }
        }
    }

    async fn request(&self, method: &str, params: Value, timeout: Duration) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (sender, receiver) = oneshot::channel();
        self.pending.lock().map_err(|_| anyhow!("MCP client poisoned"))?.insert(id, sender);
        let _waiting = Waiting { pending: &self.pending, id };
        self.send(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })).await?;
        match tokio::time::timeout(timeout, receiver).await {
            Err(_) => bail!("MCP request \"{method}\" timed out after {}ms", timeout.as_millis()),
            Ok(Err(_)) => bail!("the server stopped{}", tail(&self.stderr())),
            Ok(Ok(Err(message))) => bail!("{message}"),
            Ok(Ok(Ok(result))) => Ok(result),
        }
    }

    pub async fn list_tools(&self) -> Result<Vec<McpTool>> {
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let params = match &cursor {
                Some(cursor) => json!({ "cursor": cursor }),
                None => json!({}),
            };
            let result = self.request("tools/list", params, REQUEST_TIMEOUT).await?;
            for tool in result["tools"].as_array().into_iter().flatten() {
                let Some(name) = tool["name"].as_str() else { continue };
                tools.push(McpTool {
                    name: name.to_owned(),
                    description: tool["description"].as_str().map(str::to_owned),
                    schema: schema_of(&tool["inputSchema"]),
                });
            }
            cursor = result["nextCursor"].as_str().filter(|c| !c.is_empty()).map(str::to_owned);
            if cursor.is_none() {
                return Ok(tools);
            }
        }
    }

    /// The prompts the server offers (none when it offers no prompts).
    pub async fn list_prompts(&self) -> Result<Vec<McpPrompt>> {
        let mut prompts = Vec::new();
        if !self.has_prompts {
            return Ok(prompts);
        }
        let mut cursor: Option<String> = None;
        loop {
            let params = match &cursor {
                Some(cursor) => json!({ "cursor": cursor }),
                None => json!({}),
            };
            let result = self.request("prompts/list", params, REQUEST_TIMEOUT).await?;
            for prompt in result["prompts"].as_array().into_iter().flatten() {
                let Some(name) = prompt["name"].as_str() else { continue };
                prompts.push(McpPrompt {
                    name: name.to_owned(),
                    description: prompt["description"].as_str().map(str::to_owned),
                    arguments: prompt["arguments"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|a| Some((a["name"].as_str()?.to_owned(), a["required"].as_bool().unwrap_or(false))))
                        .collect(),
                });
            }
            cursor = result["nextCursor"].as_str().filter(|c| !c.is_empty()).map(str::to_owned);
            if cursor.is_none() {
                return Ok(prompts);
            }
        }
    }

    /// A prompt's messages, as one text for the agent (roles kept only
    /// when they mix, as in the TypeScript runtime).
    pub async fn get_prompt(&self, name: &str, arguments: Value) -> Result<String> {
        let result = self.request("prompts/get", json!({ "name": name, "arguments": arguments }), REQUEST_TIMEOUT).await?;
        let messages = result["messages"].as_array().cloned().unwrap_or_default();
        let all_user = messages.iter().all(|m| m["role"] == "user");
        let mut parts = Vec::new();
        for message in &messages {
            let blocks = match &message["content"] {
                Value::Array(blocks) => blocks.clone(),
                block => vec![block.clone()],
            };
            let text = blocks
                .iter()
                .map(|b| match b["type"].as_str() {
                    Some("text") => b["text"].as_str().unwrap_or_default().to_owned(),
                    Some("resource") => b["resource"]["text"]
                        .as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| format!("[resource: {}]", b["resource"]["mimeType"].as_str().unwrap_or("unknown"))),
                    Some("image") => format!("[image: {}]", b["mimeType"].as_str().unwrap_or("unknown")),
                    Some(other) => format!("[{other}]"),
                    None => String::new(),
                })
                .filter(|t| !t.is_empty())
                .collect::<Vec<_>>()
                .join("\n");
            if text.is_empty() {
                continue;
            }
            let role = message["role"].as_str().unwrap_or("user");
            parts.push(if all_user { text } else { format!("[{role}]\n{text}") });
        }
        Ok(parts.join("\n\n"))
    }

    pub async fn call_tool(&self, name: &str, arguments: Value) -> Result<ToolOutput> {
        let result = self.request("tools/call", json!({ "name": name, "arguments": arguments }), REQUEST_TIMEOUT).await?;
        let text = format_content(&result["content"]);
        Ok(ToolOutput {
            content: if text.is_empty() { "(no content)".into() } else { text },
            is_error: result["isError"].as_bool() == Some(true),
        })
    }
}

/// The JSON-RPC messages in an HTTP response body: JSON (one or an array)
/// or server-sent events.
fn http_messages(body: &str, event_stream: bool) -> Vec<Value> {
    let from_events = |body: &str| {
        body.lines()
            .filter_map(|line| line.strip_prefix("data:"))
            .filter_map(|data| serde_json::from_str::<Value>(data.trim()).ok())
            .collect::<Vec<_>>()
    };
    if event_stream {
        return from_events(body);
    }
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    match serde_json::from_str::<Value>(trimmed) {
        Ok(Value::Array(items)) => items,
        Ok(item) => vec![item],
        // Some servers send events without saying so.
        Err(_) => from_events(body),
    }
}

/// Some servers omit the schema or send something odd.
fn schema_of(input: &Value) -> Value {
    if input["type"] == "object" && input["properties"].is_object() {
        input.clone()
    } else {
        json!({ "type": "object", "properties": {} })
    }
}

fn format_content(content: &Value) -> String {
    content
        .as_array()
        .into_iter()
        .flatten()
        .map(|item| match item["type"].as_str().unwrap_or_default() {
            "text" if item["text"].is_string() => item["text"].as_str().unwrap_or_default().to_owned(),
            "image" if item["mimeType"].is_string() => format!(
                "[image: {}, {} bytes base64]",
                item["mimeType"].as_str().unwrap_or_default(),
                item["data"].as_str().map(str::len).unwrap_or_default()
            ),
            "resource" => format!("[resource: {}]", item["mimeType"].as_str().unwrap_or("unknown")),
            other => format!("[{other}]"),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

// --- Servers -------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub struct McpTool {
    pub name: String,
    pub description: Option<String>,
    pub schema: Value,
}

/// A prompt a server offers: its name, what it is for, and its arguments
/// (name, required).
#[derive(Clone, Debug, PartialEq)]
pub struct McpPrompt {
    pub name: String,
    pub description: Option<String>,
    pub arguments: Vec<(String, bool)>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    NotStarted,
    Connecting,
    Connected { tools: Vec<McpTool>, prompts: Vec<McpPrompt> },
    Failed(String),
}

struct Connection {
    client: Option<Arc<Client>>,
    failed_at: Option<Instant>,
}

/// One configured server and its connection, shared by every turn.
pub struct Server {
    pub name: String,
    pub scope: Scope,
    pub config: ServerConfig,
    status: Mutex<Status>,
    connection: tokio::sync::Mutex<Connection>,
}

impl Server {
    fn new(scoped: ScopedServer) -> Self {
        Server {
            name: scoped.name,
            scope: scoped.scope,
            config: scoped.config,
            status: Mutex::new(Status::NotStarted),
            connection: tokio::sync::Mutex::new(Connection { client: None, failed_at: None }),
        }
    }

    pub fn status(&self) -> Status {
        self.status.lock().map(|s| s.clone()).unwrap_or(Status::NotStarted)
    }

    fn set_status(&self, status: Status) {
        if let Ok(mut current) = self.status.lock() {
            *current = status;
        }
    }

    /// The tools on offer, once connected.
    pub fn tools(&self) -> Vec<McpTool> {
        match self.status() {
            Status::Connected { tools, .. } => tools,
            _ => Vec::new(),
        }
    }

    /// Connects unless connected, or failed a short while ago.
    pub async fn ensure(&self, http: &reqwest::Client) {
        let mut connection = self.connection.lock().await;
        if connection.client.as_ref().is_some_and(|c| !c.is_closed()) {
            return;
        }
        if connection.failed_at.is_some_and(|at| at.elapsed() < RETRY_AFTER) && connection.client.is_none() {
            return;
        }
        connection.client = None;
        self.set_status(Status::Connecting);
        let attempt = async {
            let client = Client::connect(&self.config, http).await?;
            let tools = client.list_tools().await?;
            // A server whose prompts fail still serves its tools.
            let prompts = client.list_prompts().await.unwrap_or_default();
            Ok::<_, anyhow::Error>((client, tools, prompts))
        };
        match tokio::time::timeout(REQUEST_TIMEOUT * 2, attempt).await {
            Ok(Ok((client, tools, prompts))) => {
                connection.client = Some(Arc::new(client));
                connection.failed_at = None;
                self.set_status(Status::Connected { tools, prompts });
            }
            Ok(Err(error)) => {
                connection.failed_at = Some(Instant::now());
                let message = format!("{error:#}");
                self.set_status(Status::Failed(if message.contains(crate::mcp_oauth::NEEDS_SIGN_IN) {
                    format!("{}: /mcp login {}", crate::mcp_oauth::NEEDS_SIGN_IN, self.name)
                } else {
                    message
                }));
            }
            Err(_) => {
                connection.failed_at = Some(Instant::now());
                self.set_status(Status::Failed("did not answer within a minute".into()));
            }
        }
    }

    /// The address of an HTTP server.
    pub fn url(&self) -> Option<&str> {
        match &self.config {
            ServerConfig::Http { url, .. } => Some(url),
            _ => None,
        }
    }

    /// Starts signing in to this (HTTP) server in the browser.
    pub async fn login(&self, http: &reqwest::Client) -> Result<crate::mcp_oauth::Pending> {
        let url = self.url().ok_or_else(|| anyhow!("{} is a local (stdio) server; it has no sign-in", self.name))?;
        crate::mcp_oauth::begin(http, url).await
    }

    /// Forgets the sign-in of this server.
    pub fn logout(&self) -> Result<bool> {
        match self.url() {
            Some(url) => crate::mcp_oauth::forget(url),
            None => Ok(false),
        }
    }

    /// Forgets a failure so the next use connects again.
    pub async fn reset(&self) {
        let mut connection = self.connection.lock().await;
        connection.client = None;
        connection.failed_at = None;
        self.set_status(Status::NotStarted);
    }

    pub async fn call(&self, tool: &str, arguments: Value) -> ToolOutput {
        let client = self.connection.lock().await.client.clone();
        let Some(client) = client.filter(|c| !c.is_closed()) else {
            return ToolOutput { content: format!("Error: MCP server \"{}\" is not connected.", self.name), is_error: true };
        };
        match client.call_tool(tool, arguments).await {
            Ok(output) => output,
            Err(error) => {
                ToolOutput { content: format!("Error calling {}.{tool}: {error:#}", self.name), is_error: true }
            }
        }
    }

    /// The prompts on offer, once connected.
    pub fn prompts(&self) -> Vec<McpPrompt> {
        match self.status() {
            Status::Connected { prompts, .. } => prompts,
            _ => Vec::new(),
        }
    }

    /// Prompt `name` with `arguments` (by the prompt's argument names).
    pub async fn prompt(&self, name: &str, arguments: Value) -> Result<String> {
        let client = self.connection.lock().await.client.clone();
        let Some(client) = client.filter(|c| !c.is_closed()) else {
            bail!("MCP server \"{}\" is not connected", self.name);
        };
        client.get_prompt(name, arguments).await
    }

    /// The tool specs, named `mcp__<server>__<tool>`.
    pub fn specs(&self) -> Vec<(ToolSpec, String)> {
        self.tools()
            .into_iter()
            .map(|tool| {
                let description = format!(
                    "[MCP:{}] {}",
                    self.name,
                    tool.description.as_deref().map(str::trim).filter(|d| !d.is_empty()).unwrap_or(&tool.name)
                );
                (ToolSpec { name: tool_name(&self.name, &tool.name), description, parameters: tool.schema }, tool.name)
            })
            .collect()
    }
}

/// Every server the app has met, keyed by name and configuration, so two
/// projects with the same server share one connection.
#[derive(Default)]
pub struct Hub {
    servers: Mutex<Vec<Arc<Server>>>,
}

impl Hub {
    pub fn new() -> Self {
        Self::default()
    }

    /// The servers for a configuration, connected as far as they will.
    pub async fn connect(&self, configured: Vec<ScopedServer>, http: &reqwest::Client) -> Vec<Arc<Server>> {
        let servers = self.servers_for(configured);
        futures_util::future::join_all(servers.iter().map(|s| s.ensure(http))).await;
        servers
    }

    /// The servers for a configuration, without connecting.
    pub fn servers_for(&self, configured: Vec<ScopedServer>) -> Vec<Arc<Server>> {
        let Ok(mut known) = self.servers.lock() else { return Vec::new() };
        configured
            .into_iter()
            .map(|scoped| {
                if let Some(existing) = known.iter().find(|s| s.name == scoped.name && s.config == scoped.config) {
                    return existing.clone();
                }
                let server = Arc::new(Server::new(scoped));
                known.push(server.clone());
                server
            })
            .collect()
    }

    pub fn servers(&self) -> Vec<Arc<Server>> {
        self.servers.lock().map(|s| s.clone()).unwrap_or_default()
    }
}

/// Where a project's MCP settings live, for showing a person.
pub fn settings_paths(home: &Path, project: Option<&Path>) -> Vec<PathBuf> {
    let mut paths = vec![home.join("settings.json")];
    if let Some(project) = project {
        paths.push(project.join(".bubble").join("settings.json"));
        paths.push(project.join(".bubble").join("settings.local.json"));
    }
    paths
}

#[cfg(test)]
mod tests {
    use super::*;

    static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

    fn temp() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("bubble-mcp-{}-{}", std::process::id(), NEXT_DIR.fetch_add(1, Ordering::SeqCst)));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn names_round_trip() {
        assert_eq!(tool_name("my.server", "do thing"), "mcp__my_server__do_thing");
        assert_eq!(parse_tool_name("mcp__arxiv__search_papers"), Some(("arxiv", "search_papers")));
        assert_eq!(parse_tool_name("bash"), None);
    }

    #[test]
    fn configs_merge_validate_and_expand() {
        let home = temp();
        let project = temp();
        std::fs::write(
            home.join("settings.json"),
            r#"{"mcpServers":{"a":{"command":"run","args":["${HOME}"]},"b":{"type":"http","url":"https://x"},"bad":{"type":"ws"}}}"#,
        )
        .unwrap();
        std::fs::create_dir_all(project.join(".bubble")).unwrap();
        std::fs::write(project.join(".bubble/settings.json"), r#"{"mcpServers":{"b":{"type":"sse","url":"https://y/${NOPE_NOT_SET}"}}}"#)
            .unwrap();
        let (servers, diagnostics) = load_config(&home, Some(&project));
        assert_eq!(servers.len(), 2);
        assert_eq!(servers[0].name, "a");
        let ServerConfig::Stdio { args, .. } = &servers[0].config else { panic!() };
        assert_eq!(args[0], std::env::var("HOME").unwrap());
        assert_eq!(servers[1].scope, Scope::Project);
        assert_eq!(servers[1].config, ServerConfig::Http { url: "https://y/".into(), headers: BTreeMap::new() });
        assert!(diagnostics.iter().any(|d| d.contains("unsupported transport")));
        assert!(diagnostics.iter().any(|d| d.contains("NOPE_NOT_SET")));
        // Without a project, only the person's own servers.
        assert_eq!(load_config(&home, None).0.len(), 2);
    }

    #[test]
    fn http_bodies_may_be_json_or_events() {
        assert_eq!(http_messages(r#"{"id":1}"#, false), vec![json!({"id":1})]);
        assert_eq!(http_messages("event: message\ndata: {\"id\":2}\n\n", true), vec![json!({"id":2})]);
        assert_eq!(http_messages("data: {\"id\":3}\n", false), vec![json!({"id":3})]);
    }

    #[test]
    fn content_is_flattened_to_text() {
        let content = json!([{ "type": "text", "text": "hi" }, { "type": "image", "mimeType": "image/png", "data": "abcd" }]);
        assert_eq!(format_content(&content), "hi\n[image: image/png, 4 bytes base64]");
    }

    /// A tiny stdio server in Python: initialize, a ping to us, one tool.
    #[tokio::test]
    async fn stdio_servers_list_and_call_tools() {
        let dir = temp();
        let script = dir.join("server.py");
        std::fs::write(
            &script,
            r#"
import json, sys
def send(m):
    sys.stdout.write(json.dumps(m) + "\n"); sys.stdout.flush()
print("starting up (not json)", flush=True)
for line in sys.stdin:
    m = json.loads(line)
    if m.get("method") == "initialize":
        send({"jsonrpc": "2.0", "method": "ping", "id": "p1"})
        send({"jsonrpc": "2.0", "id": m["id"], "result": {"serverInfo": {"name": "t", "version": "1"}, "capabilities": {"tools": {}, "prompts": {}}}})
    elif m.get("method") == "prompts/list":
        send({"jsonrpc": "2.0", "id": m["id"], "result": {"prompts": [{"name": "review", "description": "Review a file", "arguments": [{"name": "path", "required": True}, {"name": "focus"}]}]}})
    elif m.get("method") == "prompts/get":
        a = m["params"]["arguments"]
        send({"jsonrpc": "2.0", "id": m["id"], "result": {"messages": [{"role": "user", "content": {"type": "text", "text": "Review " + a["path"] + " for " + a.get("focus", "anything")}}]}})
    elif m.get("method") == "tools/list":
        if m["params"].get("cursor"):
            send({"jsonrpc": "2.0", "id": m["id"], "result": {"tools": [{"name": "echo", "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}}}]}})
        else:
            send({"jsonrpc": "2.0", "id": m["id"], "result": {"tools": [{"name": "noschema"}], "nextCursor": "2"}})
    elif m.get("method") == "tools/call":
        send({"jsonrpc": "2.0", "id": m["id"], "result": {"content": [{"type": "text", "text": "echo: " + m["params"]["arguments"]["text"]}]}})
    elif m.get("id") == "p1":
        sys.stderr.write("got pong\n")
"#,
        )
        .unwrap();
        let hub = Hub::new();
        let configured = vec![ScopedServer {
            name: "t".into(),
            scope: Scope::User,
            config: ServerConfig::Stdio {
                command: "python3".into(),
                args: vec![script.display().to_string()],
                env: BTreeMap::new(),
                cwd: None,
            },
        }];
        let http = reqwest::Client::new();
        let servers = hub.connect(configured.clone(), &http).await;
        let specs = servers[0].specs();
        assert_eq!(specs.iter().map(|(s, _)| s.name.as_str()).collect::<Vec<_>>(), ["mcp__t__noschema", "mcp__t__echo"]);
        assert_eq!(specs[0].0.parameters, json!({ "type": "object", "properties": {} }));
        let output = servers[0].call("echo", json!({ "text": "hi" })).await;
        assert_eq!(output, ToolOutput { content: "echo: hi".into(), is_error: false });
        let prompts = servers[0].prompts();
        assert_eq!(prompts[0].name, "review");
        assert_eq!(prompts[0].arguments, [("path".to_owned(), true), ("focus".to_owned(), false)]);
        let text = servers[0].prompt("review", json!({ "path": "src/a.rs", "focus": "error handling" })).await.unwrap();
        assert_eq!(text, "Review src/a.rs for error handling");
        // The same configuration again shares the connection.
        let again = hub.servers_for(configured);
        assert!(Arc::ptr_eq(&again[0], &servers[0]));
    }

    #[tokio::test]
    async fn servers_that_cannot_start_fail_without_blocking() {
        let hub = Hub::new();
        let servers = hub
            .connect(
                vec![ScopedServer {
                    name: "gone".into(),
                    scope: Scope::User,
                    config: ServerConfig::Stdio {
                        command: "/nonexistent/bubble-mcp".into(),
                        args: vec![],
                        env: BTreeMap::new(),
                        cwd: None,
                    },
                }],
                &reqwest::Client::new(),
            )
            .await;
        assert!(matches!(servers[0].status(), Status::Failed(message) if message.contains("could not start")));
        let output = servers[0].call("x", json!({})).await;
        assert!(output.is_error);
    }
}
