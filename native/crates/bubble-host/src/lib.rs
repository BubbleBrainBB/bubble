//! Bubble's agent as a child process.
//!
//! The agent core is the TypeScript one. `bubble host --stdio` serves it
//! over line-delimited JSON-RPC; this crate starts that process and talks
//! to it. Requests go both ways: the app starts sessions and turns, the
//! host asks the app for what only a person or the app can give.
//!
//! The client runs on a runtime of its own, so its futures can be awaited
//! from any executor (the UI's included).

mod events;
mod launch;

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot};

pub use events::{Approval, ApprovalRequest, Event, SessionMessage, SubagentUpdate, Terminal, ToolResult, TurnStarted, Usage};
pub use launch::Launch;

/// The protocol version this client speaks.
pub const PROTOCOL_VERSION: u64 = 1;

/// The threads the client's own tasks run on.
fn runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("bubble-host")
            .enable_all()
            .build()
            .expect("the host client's runtime starts")
    })
}

/// A tool the app runs itself, offered to the model for one turn.
#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostTool {
    pub name: String,
    pub description: String,
    /// JSON Schema of the arguments.
    pub parameters: Value,
    /// The call changes nothing, or the app asks the person itself.
    pub read_only: bool,
}

/// A picture sent with a message.
#[derive(Clone, Debug)]
pub struct Picture {
    pub media_type: String,
    /// Base64.
    pub data: String,
}

/// One turn: what the person (or whoever drives the session) says, and how
/// the agent should take it.
#[derive(Clone, Debug, Default)]
pub struct TurnRequest {
    pub session_id: String,
    pub text: String,
    pub pictures: Vec<Picture>,
    /// "provider:model"; the configured default when `None`.
    pub model: Option<String>,
    /// "off", "low", "high", ...; the model's default when `None`.
    pub thinking: Option<String>,
    /// Every tool call goes ahead without asking.
    pub full_access: bool,
    /// Added after the agent's own system prompt.
    pub append_system_prompt: Option<String>,
    /// Instead of the agent's own system prompt.
    pub system_prompt: Option<String>,
    /// Only these built-in tools; `None` offers all.
    pub allowed_tools: Option<Vec<String>>,
    pub host_tools: Vec<HostTool>,
}

/// A model a turn can run on.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Model {
    /// "provider:model".
    pub reference: String,
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub context_window: Option<u64>,
    #[serde(default)]
    pub thinking_levels: Vec<String>,
    #[serde(default)]
    pub default_thinking_level: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Provider {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub oauth: bool,
    #[serde(default)]
    pub models: Vec<Model>,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Models {
    #[serde(default)]
    pub default_model: String,
    #[serde(default)]
    pub default_thinking_level: Option<String>,
    #[serde(default)]
    pub providers: Vec<Provider>,
}

/// A subscription the person can sign in to.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    pub provider_id: String,
    pub name: String,
    pub signed_in: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Skill {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// "user", "project" or "configured".
    #[serde(default)]
    pub source: String,
    /// The skills folder it was found in.
    #[serde(default)]
    pub folder: String,
    /// Loaded only when the person names it.
    #[serde(default)]
    pub manual_only: bool,
    #[serde(default)]
    pub enabled: bool,
}

/// The skills found, and what was wrong with those that could not be read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Skills {
    pub skills: Vec<Skill>,
    pub problems: Vec<String>,
}

/// An MCP server of a folder and how its connection stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpServer {
    pub name: String,
    /// "user", "project" or "local".
    pub scope: String,
    pub state: McpState,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum McpState {
    Connected { tools: usize, prompts: usize },
    Failed(String),
    Disabled,
}

/// What the host said about itself when it started.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostInfo {
    pub protocol_version: u64,
    pub bubble_version: String,
    pub home: String,
}

type Reply = oneshot::Sender<Result<Value, String>>;

struct Inner {
    outgoing: mpsc::UnboundedSender<String>,
    waiting: Mutex<HashMap<u64, Reply>>,
    subscribers: Mutex<HashMap<String, mpsc::UnboundedSender<SessionMessage>>>,
    /// The session each of the host's open requests belongs to.
    asked: Mutex<HashMap<String, String>>,
    progress: Mutex<Option<mpsc::UnboundedSender<String>>>,
    next_id: AtomicU64,
    alive: AtomicBool,
    info: OnceLock<HostInfo>,
    /// Kills the process when the last handle goes.
    _kill: mpsc::UnboundedSender<()>,
}

/// A running host process. Clones share it; it ends with the last one.
#[derive(Clone)]
pub struct Host(Arc<Inner>);

impl Host {
    /// Starts the host and waits until it answers.
    pub async fn start(launch: Launch) -> Result<Host> {
        let (ready, started) = oneshot::channel();
        runtime().spawn(async move {
            let _ = ready.send(Host::start_on_runtime(launch).await);
        });
        started.await.map_err(|_| anyhow!("the host could not be started"))?
    }

    async fn start_on_runtime(launch: Launch) -> Result<Host> {
        let mut command = tokio::process::Command::new(&launch.program);
        command
            .args(&launch.args)
            .env_clear()
            .envs(&launch.env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        detach(&mut command);
        let mut child = command.spawn().with_context(|| format!("could not start {}", launch.program.display()))?;
        let mut stdin = child.stdin.take().context("the host has no stdin")?;
        let stdout = child.stdout.take().context("the host has no stdout")?;
        let stderr = child.stderr.take().context("the host has no stderr")?;

        let (outgoing, mut lines) = mpsc::unbounded_channel::<String>();
        let (kill, mut killed) = mpsc::unbounded_channel::<()>();
        let inner = Arc::new(Inner {
            outgoing,
            waiting: Mutex::default(),
            subscribers: Mutex::default(),
            asked: Mutex::default(),
            progress: Mutex::default(),
            next_id: AtomicU64::new(0),
            alive: AtomicBool::new(true),
            info: OnceLock::new(),
            _kill: kill,
        });

        // Writer: one line per message.
        tokio::spawn(async move {
            while let Some(line) = lines.recv().await {
                if stdin.write_all(line.as_bytes()).await.is_err() || stdin.write_all(b"\n").await.is_err() {
                    break;
                }
                let _ = stdin.flush().await;
            }
        });

        // What the host prints besides the protocol: kept for the error
        // message if it dies.
        let last_words = Arc::new(Mutex::new(String::new()));
        let words = last_words.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                let mut kept = words.lock().unwrap();
                kept.push_str(&line);
                kept.push('\n');
                if kept.len() > 4000 {
                    let cut = kept.len() - 2000;
                    let cut = (cut..kept.len()).find(|&i| kept.is_char_boundary(i)).unwrap_or(kept.len());
                    kept.drain(..cut);
                }
            }
        });

        // Reader, and the end of the process.
        let weak = Arc::downgrade(&inner);
        tokio::spawn(async move {
            let mut reader = BufReader::new(stdout).lines();
            loop {
                tokio::select! {
                    line = reader.next_line() => match line {
                        Ok(Some(line)) => {
                            let Some(inner) = weak.upgrade() else { break };
                            inner.receive(&line);
                        }
                        _ => break,
                    },
                    // Every handle is gone.
                    _ = killed.recv() => break,
                }
            }
            let _ = child.start_kill();
            let _ = child.wait().await;
            if let Some(inner) = weak.upgrade() {
                let words = last_words.lock().unwrap().trim().to_owned();
                inner.lost(if words.is_empty() { "The Bubble agent process stopped.".to_owned() } else { words });
            }
        });

        let host = Host(inner);
        let info = tokio::time::timeout(std::time::Duration::from_secs(30), host.call("initialize", json!({})))
            .await
            .map_err(|_| anyhow!("the Bubble agent process did not answer"))?
            .context("the Bubble agent process did not start (is this Bubble new enough for the desktop app?)")?;
        let info: HostInfo = serde_json::from_value(info).context("the host's greeting was not understood")?;
        if info.protocol_version != PROTOCOL_VERSION {
            bail!("the Bubble agent speaks protocol {}, this app speaks {PROTOCOL_VERSION}", info.protocol_version);
        }
        let _ = host.0.info.set(info);
        Ok(host)
    }

    pub fn info(&self) -> &HostInfo {
        self.0.info.get().expect("set before the host is handed out")
    }

    /// Whether the process still runs.
    pub fn is_alive(&self) -> bool {
        self.0.alive.load(Ordering::SeqCst)
    }

    /// Sends a request and waits for its result.
    pub async fn call(&self, method: &str, params: Value) -> Result<Value> {
        if !self.is_alive() {
            bail!("the Bubble agent process is not running");
        }
        let id = self.0.next_id.fetch_add(1, Ordering::SeqCst) + 1;
        let (reply, answer) = oneshot::channel();
        self.0.waiting.lock().unwrap().insert(id, reply);
        let line = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }).to_string();
        if self.0.outgoing.send(line).is_err() {
            self.0.waiting.lock().unwrap().remove(&id);
            bail!("the Bubble agent process is not running");
        }
        match answer.await {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(message)) => Err(anyhow!(message)),
            Err(_) => Err(anyhow!("the Bubble agent process stopped")),
        }
    }

    /// Everything the host sends about `session_id` from now on. A second
    /// subscription to the same session replaces the first.
    pub fn subscribe(&self, session_id: &str) -> mpsc::UnboundedReceiver<SessionMessage> {
        let (sender, receiver) = mpsc::unbounded_channel();
        if !self.is_alive() {
            let _ = sender.send(SessionMessage::Lost("The Bubble agent process is not running.".into()));
        }
        self.0.subscribers.lock().unwrap().insert(session_id.to_owned(), sender);
        receiver
    }

    pub fn unsubscribe(&self, session_id: &str) {
        self.0.subscribers.lock().unwrap().remove(session_id);
    }

    // --- Sessions and turns ---------------------------------------------------

    /// Makes sure session `id` exists in `cwd`: a new one, or the one a
    /// previous run of the app left.
    pub async fn open_session(&self, id: &str, cwd: &str) -> Result<()> {
        self.call("session/create", json!({ "id": id, "cwd": cwd })).await.map(|_| ())
    }

    pub async fn delete_session(&self, id: &str) -> Result<()> {
        self.call("session/delete", json!({ "sessionId": id })).await.map(|_| ())
    }

    /// The session's messages as the model has them.
    pub async fn history(&self, id: &str) -> Result<Vec<Value>> {
        let result = self.call("session/history", json!({ "sessionId": id })).await?;
        Ok(result.get("messages").and_then(Value::as_array).cloned().unwrap_or_default())
    }

    /// Starts a turn; its events come to the session's subscriber. Returns
    /// once the host accepted it (a session with a turn running queues it).
    pub async fn start_turn(&self, turn: TurnRequest) -> Result<()> {
        let prompt = if turn.pictures.is_empty() {
            json!(turn.text)
        } else {
            let mut parts = vec![json!({ "type": "text", "text": turn.text })];
            parts.extend(turn.pictures.iter().map(|p| {
                json!({ "type": "image_url", "image_url": { "url": format!("data:{};base64,{}", p.media_type, p.data) } })
            }));
            Value::Array(parts)
        };
        let mut params = json!({
            "sessionId": turn.session_id,
            "prompt": prompt,
            "mode": if turn.full_access { "bypassPermissions" } else { "default" },
            "hostTools": turn.host_tools,
        });
        let fields = params.as_object_mut().expect("an object");
        if let Some(model) = turn.model {
            fields.insert("model".into(), json!(model));
        }
        if let Some(thinking) = turn.thinking {
            fields.insert("thinkingLevel".into(), json!(thinking));
        }
        if let Some(prompt) = turn.append_system_prompt {
            fields.insert("appendSystemPrompt".into(), json!(prompt));
        }
        if let Some(prompt) = turn.system_prompt {
            fields.insert("systemPrompt".into(), json!(prompt));
        }
        if let Some(tools) = turn.allowed_tools {
            fields.insert("allowedTools".into(), json!(tools));
        }
        self.call("turn/start", params).await.map(|_| ())
    }

    /// Stops the session's running turn (and what waits behind it).
    pub async fn stop(&self, session_id: &str) -> Result<()> {
        self.call("turn/stop", json!({ "sessionId": session_id, "cancelQueued": true })).await.map(|_| ())
    }

    /// Adds `text` to the running turn; `Ok(true)` when the host took it.
    pub async fn steer(&self, session_id: &str, text: &str) -> Result<bool> {
        let result = self.call("turn/steer", json!({ "sessionId": session_id, "content": text })).await?;
        Ok(result.get("accepted").and_then(Value::as_bool).unwrap_or(false))
    }

    // --- Answers to the host's requests -------------------------------------

    pub fn approve(&self, id: &str, approval: &Approval) {
        self.answer(id, approval.to_json());
    }

    /// The result of a tool the app ran.
    pub fn tool_result(&self, id: &str, content: &str, is_error: bool) {
        self.answer(id, json!({ "content": content, "isError": is_error }));
    }

    /// Answers the host's request `id` with `result`.
    pub fn answer(&self, id: &str, result: Value) {
        if self.0.asked.lock().unwrap().remove(id).is_none() {
            return;
        }
        let _ = self.0.outgoing.send(json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string());
    }

    // --- Lists ----------------------------------------------------------------

    pub async fn models(&self, refresh: bool) -> Result<Models> {
        Ok(serde_json::from_value(self.call("models/list", json!({ "refresh": refresh })).await?)?)
    }

    pub async fn skills(&self, cwd: Option<&str>) -> Result<Skills> {
        let result = self.call("skills/list", json!({ "cwd": cwd })).await?;
        let problems = result.get("problems").and_then(Value::as_array).cloned().unwrap_or_default();
        Ok(Skills {
            skills: serde_json::from_value(result.get("skills").cloned().unwrap_or_default()).unwrap_or_default(),
            problems: problems
                .iter()
                .map(|problem| {
                    let message = problem.get("message").and_then(Value::as_str).unwrap_or_default();
                    match problem.get("file").and_then(Value::as_str) {
                        Some(file) => format!("{file}: {message}"),
                        None => message.to_owned(),
                    }
                })
                .collect(),
        })
    }

    pub async fn mcp_servers(&self, cwd: Option<&str>) -> Result<Vec<McpServer>> {
        let result = self.call("mcp/status", json!({ "cwd": cwd })).await?;
        Ok(result.get("servers").and_then(Value::as_array).map(|s| s.iter().filter_map(mcp_server).collect()).unwrap_or_default())
    }

    pub async fn reconnect_mcp_server(&self, cwd: Option<&str>, name: &str) -> Result<Option<McpServer>> {
        let result = self.call("mcp/reconnect", json!({ "cwd": cwd, "name": name })).await?;
        Ok(result.get("server").and_then(mcp_server))
    }

    pub async fn accounts(&self) -> Result<Vec<Account>> {
        let result = self.call("auth/list", json!({})).await?;
        Ok(serde_json::from_value(result.get("accounts").cloned().unwrap_or_default())?)
    }

    /// Signs in to a subscription in the browser; `progress` gets what to
    /// tell the person meanwhile.
    pub async fn sign_in(&self, provider_id: &str, progress: mpsc::UnboundedSender<String>) -> Result<()> {
        *self.0.progress.lock().unwrap() = Some(progress);
        let result = self.call("auth/login", json!({ "providerId": provider_id })).await;
        *self.0.progress.lock().unwrap() = None;
        result.map(|_| ())
    }

    pub async fn sign_out(&self, provider_id: &str) -> Result<()> {
        self.call("auth/logout", json!({ "providerId": provider_id })).await.map(|_| ())
    }

    /// A short answer from the model, outside any session.
    pub async fn complete(&self, prompt: &str, model: Option<&str>) -> Result<String> {
        let result = self.call("text/complete", json!({ "prompt": prompt, "model": model })).await?;
        Ok(result.get("text").and_then(Value::as_str).unwrap_or_default().to_owned())
    }

    /// Has the host read the configuration files again.
    pub async fn reload_config(&self) -> Result<()> {
        self.call("config/reload", json!({})).await.map(|_| ())
    }

    /// Asks the host to end its turns and servers and exit.
    pub async fn shutdown(&self) {
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), self.call("shutdown", json!({}))).await;
    }
}

fn mcp_server(value: &Value) -> Option<McpServer> {
    let status = value.get("status")?;
    let state = match status.get("kind")?.as_str()? {
        "connected" => McpState::Connected {
            tools: status.get("tools").and_then(Value::as_array).map_or(0, Vec::len),
            prompts: status.get("prompts").and_then(Value::as_array).map_or(0, Vec::len),
        },
        "failed" => McpState::Failed(status.get("error").and_then(Value::as_str).unwrap_or_default().to_owned()),
        _ => McpState::Disabled,
    };
    Some(McpServer {
        name: value.get("name")?.as_str()?.to_owned(),
        scope: value.get("scope").and_then(Value::as_str).unwrap_or("user").to_owned(),
        state,
    })
}

impl Inner {
    fn receive(&self, line: &str) {
        let Ok(message) = serde_json::from_str::<Value>(line) else { return };
        let Some(method) = message.get("method").and_then(Value::as_str) else {
            // An answer to one of our calls.
            let Some(id) = message.get("id").and_then(Value::as_u64) else { return };
            let Some(reply) = self.waiting.lock().unwrap().remove(&id) else { return };
            let _ = reply.send(match message.get("error") {
                Some(error) => Err(error.get("message").and_then(Value::as_str).unwrap_or("the host reported an error").to_owned()),
                None => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
            });
            return;
        };
        let params = message.get("params").cloned().unwrap_or(Value::Null);
        let session = params.get("sessionId").and_then(Value::as_str).unwrap_or_default().to_owned();
        match message.get("id") {
            // A request of the host's: someone has to answer it.
            Some(id) if !id.is_null() => {
                let id = id.as_str().map(str::to_owned).unwrap_or_else(|| id.to_string());
                let text = |key: &str| params.get(key).and_then(Value::as_str).unwrap_or_default().to_owned();
                let call = params.get("toolCallId").and_then(Value::as_str).map(str::to_owned);
                let parsed = match method {
                    "approval/request" => serde_json::from_value(params.get("request").cloned().unwrap_or_default())
                        .ok()
                        .map(|request| SessionMessage::Approval { id: id.clone(), tool_call_id: call, request }),
                    "question/request" => Some(SessionMessage::Question {
                        id: id.clone(),
                        request: params.get("request").cloned().unwrap_or_default(),
                    }),
                    "plan/request" => Some(SessionMessage::Plan { id: id.clone(), plan: text("plan") }),
                    "trust/request" => Some(SessionMessage::Trust {
                        id: id.clone(),
                        cwd: text("cwd"),
                        pending: params.get("pending").cloned().unwrap_or_default(),
                    }),
                    "tool/call" => Some(SessionMessage::ToolCall {
                        id: id.clone(),
                        tool_call_id: call,
                        name: text("name"),
                        args: params.get("args").cloned().unwrap_or_default(),
                    }),
                    _ => None,
                };
                let delivered = parsed.is_some_and(|message| {
                    self.asked.lock().unwrap().insert(id.clone(), session.clone());
                    self.deliver(&session, message)
                });
                if !delivered {
                    // Nobody can answer: the host falls back to its refusal.
                    self.asked.lock().unwrap().remove(&id);
                    let error = json!({ "code": -32000, "message": "nobody is there to answer" });
                    let _ = self.outgoing.send(json!({ "jsonrpc": "2.0", "id": id, "error": error }).to_string());
                }
            }
            _ => match method {
                "session/event" => {
                    let Ok(event) = serde_json::from_value::<Event>(params.get("event").cloned().unwrap_or_default()) else { return };
                    let terminal = params.get("terminal").and_then(|t| serde_json::from_value(t.clone()).ok());
                    let turn_id = params.get("turnId").and_then(Value::as_str).unwrap_or_default().to_owned();
                    self.deliver(&session, SessionMessage::Event { turn_id, event, terminal });
                }
                "turn/started" => {
                    if let Ok(started) = serde_json::from_value(params) {
                        self.deliver(&session, SessionMessage::Started(started));
                    }
                }
                "request/cancelled" => {
                    let Some(id) = params.get("id").and_then(Value::as_str) else { return };
                    if let Some(session) = self.asked.lock().unwrap().remove(id) {
                        self.deliver(&session, SessionMessage::Withdrawn { id: id.to_owned() });
                    }
                }
                "auth/progress" => {
                    let message = params.get("message").and_then(Value::as_str).unwrap_or_default().to_owned();
                    if let Some(progress) = self.progress.lock().unwrap().as_ref() {
                        let _ = progress.send(message);
                    }
                }
                _ => {}
            },
        }
    }

    fn deliver(&self, session: &str, message: SessionMessage) -> bool {
        let subscribers = self.subscribers.lock().unwrap();
        subscribers.get(session).is_some_and(|subscriber| subscriber.send(message).is_ok())
    }

    /// The process ended: everyone waiting learns it.
    fn lost(&self, why: String) {
        self.alive.store(false, Ordering::SeqCst);
        for (_, reply) in self.waiting.lock().unwrap().drain() {
            let _ = reply.send(Err(why.clone()));
        }
        self.asked.lock().unwrap().clear();
        for (_, subscriber) in self.subscribers.lock().unwrap().drain() {
            let _ = subscriber.send(SessionMessage::Lost(why.clone()));
        }
    }
}

/// Starts the host in a session of its own, with no controlling terminal:
/// the commands the agent runs must not take the terminal or read what the
/// person types.
fn detach(command: &mut tokio::process::Command) {
    #[cfg(unix)]
    {
        // SAFETY: setsid is async-signal-safe; nothing else runs in the child.
        unsafe {
            command.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }
    #[cfg(not(unix))]
    let _ = command;
}
