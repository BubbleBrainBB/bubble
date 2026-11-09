//! One turn of an agent: ask the model, run the tools it calls (asking the
//! person first where a tool needs it), give it the results, and repeat
//! until it answers without calling a tool. Hooks run around the turn, the
//! model calls and the tools.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Result, bail};
use serde_json::json;
use tokio::sync::{mpsc, oneshot};

use crate::compact;
use crate::config::ResolvedModel;
use crate::delegate::Children;
use crate::hooks::{self, Hooks, Outcome};
use crate::model::{self, Level};
use crate::provider::{ChatMessage, ChatRequest, StreamEvent, ToolCall, ToolResult, ToolSpec, require_reply, stream_chat};
use crate::toolbox::Toolbox;
use crate::tools::{self, Permission, ToolOutput};

/// How many model calls one turn may make.
const MAX_STEPS: usize = 50;

/// A person's answer to "may I?".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    AllowOnce,
    /// Remember the yes for the rest of the task: this exact command, or
    /// file changes in the project.
    AllowForTask,
    Deny,
    /// Turned down by the host, with what the model should read (no one to
    /// ask, plan mode).
    Refuse(&'static str),
}

/// What a turn reports as it goes.
#[derive(Debug)]
pub enum AgentEvent {
    Text(String),
    Reasoning(String),
    /// A model reply is complete; keep it.
    Assistant(ChatMessage),
    ToolStarted { call: ToolCall, description: String },
    /// The turn waits on `respond`.
    ApprovalNeeded { call: ToolCall, permission: Permission, respond: oneshot::Sender<Decision> },
    ToolFinished { call_id: String, output: ToolOutput, denied: bool },
    /// A step's tool results are complete; keep them.
    ToolResults(ChatMessage),
    /// A host tool was called: the app runs it and answers on `respond`.
    HostCall { call: ToolCall, respond: oneshot::Sender<ToolOutput> },
    /// The history was compacted to fit the window; keep it as a checkpoint.
    Compacted(Vec<ChatMessage>),
    /// What one model call used, when the provider said.
    Usage(crate::provider::Usage),
}

pub struct Turn {
    pub model: ResolvedModel,
    pub system: Option<String>,
    /// The conversation so far, ending with the person's new message.
    pub history: Vec<ChatMessage>,
    /// The project folder the tools work in; `None` offers no tools.
    pub cwd: Option<PathBuf>,
    /// What "allow for this task" has granted so far; shared across turns.
    pub grants: Arc<Mutex<HashSet<String>>>,
    /// Tools the app itself answers (the Manager's), on top of the
    /// project tools.
    pub host_tools: Vec<ToolSpec>,
    pub max_tokens: u32,
    pub thinking: Level,
    /// Skills, web, memory and MCP tools, if the host offers them.
    pub toolbox: Option<Arc<Toolbox>>,
    /// The deferred tools loaded so far; shared across turns.
    pub unlocked: Arc<Mutex<HashSet<String>>>,
    pub hooks: Option<Arc<Hooks>>,
    pub role: hooks::Role,
    /// Only these tools (a subagent's profile); `None` offers all.
    pub allowed: Option<HashSet<String>>,
    /// Whether a tool that needs a yes may ask; a subagent's cannot.
    pub ask: bool,
    /// Subagents and workflows, if the host offers them.
    pub children: Option<Arc<Children>>,
}

impl Turn {
    /// A turn with no tools, hooks or children: set the fields you need.
    pub fn new(model: ResolvedModel, system: Option<String>, history: Vec<ChatMessage>) -> Turn {
        Turn {
            model,
            system,
            history,
            cwd: None,
            grants: Default::default(),
            host_tools: Vec::new(),
            max_tokens: 16_384,
            thinking: Level::High,
            toolbox: None,
            unlocked: Default::default(),
            hooks: None,
            role: hooks::Role::Parent,
            allowed: None,
            ask: true,
            children: None,
        }
    }
}

fn reminder(event: &str, context: &[String]) -> String {
    context.iter().map(|c| format!("\n\n<system-reminder>\n[Hook {event}] {c}\n</system-reminder>")).collect()
}

fn preview(text: &str, max: usize) -> String {
    if text.chars().count() <= max { text.to_owned() } else { format!("{}...", text.chars().take(max).collect::<String>()) }
}

/// Runs a turn to its end, reporting through `events`.
pub async fn run_turn(client: &reqwest::Client, turn: Turn, events: mpsc::UnboundedSender<AgentEvent>) -> Result<()> {
    let hooks = turn.hooks.clone();
    let role = turn.role;
    let cwd = turn.cwd.clone().unwrap_or_else(|| dirs::home_dir().unwrap_or_default());
    let result = turn_steps(client, turn, &events).await;
    if let Err(error) = &result {
        fire(&hooks, role, &cwd, "StopFailure", "run_error", json!({ "error": format!("{error:#}") }), json!({})).await;
    }
    result
}

async fn fire(
    hooks: &Option<Arc<Hooks>>,
    role: hooks::Role,
    cwd: &Path,
    name: &str,
    target: &str,
    payload: serde_json::Value,
    full: serde_json::Value,
) -> Outcome {
    match hooks {
        Some(hooks) if !hooks.is_empty() => hooks.run(role, hooks::Event { name, cwd, target, payload, full }).await,
        _ => Outcome::default(),
    }
}

async fn turn_steps(client: &reqwest::Client, turn: Turn, events: &mpsc::UnboundedSender<AgentEvent>) -> Result<()> {
    let Turn {
        model,
        system,
        mut history,
        cwd,
        grants,
        host_tools,
        max_tokens,
        thinking,
        toolbox,
        unlocked,
        hooks,
        role,
        allowed,
        ask,
        children,
    } = turn;
    let system = match (system, toolbox.as_ref().and_then(|b| b.prompt())) {
        (Some(system), Some(extra)) => Some(format!("{system}\n\n{extra}")),
        (system, extra) => system.or(extra),
    };
    let hook_cwd = cwd.clone().unwrap_or_else(|| dirs::home_dir().unwrap_or_default());
    // Children only for an agent that may delegate (not a child itself).
    let children = children.filter(|_| role == hooks::Role::Parent && cwd.is_some());

    // The person's message passes the prompt hooks first.
    if let Some(ChatMessage::User { text }) = history.last_mut() {
        let outcome = fire(
            &hooks,
            role,
            &hook_cwd,
            "UserPromptSubmit",
            &preview(text, 2000),
            json!({ "promptPreview": preview(text, 240), "promptLength": text.chars().count() }),
            json!({ "prompt": text.clone() }),
        )
        .await;
        if let Some(reason) = outcome.denied {
            let _ = events.send(AgentEvent::Text(reason.clone()));
            let _ = events.send(AgentEvent::Assistant(ChatMessage::assistant(reason)));
            return Ok(());
        }
        text.push_str(&reminder("UserPromptSubmit", &outcome.context));
    }

    let window = model::info(&model.provider.id, &model.model).context_window;
    // The provider's last prompt count and how many messages it covered.
    let mut anchor: Option<(u64, usize)> = None;
    let is_host = |name: &str| host_tools.iter().any(|tool| tool.name == name);
    let in_box = |name: &str| toolbox.as_ref().is_some_and(|b| b.owns(name));
    let delegates = |name: &str| children.is_some() && (Children::owns(name) || name == crate::workflow::TOOL);
    let offered = |name: &str| allowed.as_ref().is_none_or(|a| a.contains(name));
    for _ in 0..MAX_STEPS {
        // Loading a deferred tool adds it for the next call.
        let mut tools = if cwd.is_some() { tools::specs() } else { Vec::new() };
        if let Some(toolbox) = &toolbox {
            tools.extend(toolbox.specs(&unlocked.lock().map(|u| u.clone()).unwrap_or_default()));
        }
        if children.is_some() {
            tools.extend(Children::specs(cwd.as_deref()));
            tools.push(crate::workflow::spec());
        }
        tools.extend(host_tools.iter().cloned());
        tools.retain(|t| offered(&t.name));
        if compact::should_compact(window, compact::estimate_request(anchor, &history, &tools)) {
            let outcome = fire(&hooks, role, &hook_cwd, "PreCompact", "context", json!({ "messages": history.len() }), json!({})).await;
            if outcome.denied.is_none() {
                match compact::compact(client, &model, &history).await {
                    Ok(Some(compacted)) => {
                        history = compacted;
                        anchor = None;
                        let _ = events.send(AgentEvent::Compacted(history.clone()));
                        fire(&hooks, role, &hook_cwd, "PostCompact", "context", json!({ "messages": history.len() }), json!({})).await;
                    }
                    Ok(None) => {}
                    Err(error) => log::warn!("compaction failed: {error:#}"),
                }
            }
        }
        fire(
            &hooks,
            role,
            &hook_cwd,
            "PreModelCall",
            &model.model,
            json!({ "providerId": model.provider.id, "model": model.model, "messages": history.len(), "tools": tools.len() }),
            json!({}),
        )
        .await;
        let request =
            ChatRequest { system: system.clone(), messages: history.clone(), tools: tools.clone(), max_tokens, thinking };
        let stream_events = events.clone();
        let outcome = stream_chat(client, &model, &request, move |event| {
            let _ = stream_events.send(match event {
                StreamEvent::Text(text) => AgentEvent::Text(text),
                StreamEvent::Reasoning(text) => AgentEvent::Reasoning(text),
            });
        })
        .await?;
        require_reply(&outcome)?;
        if let Some(usage) = &outcome.usage {
            if usage.input_tokens > 0 {
                anchor = Some((usage.input_tokens, history.len()));
            }
            let _ = events.send(AgentEvent::Usage(usage.clone()));
        }
        let reply = outcome.to_message();
        history.push(reply.clone());
        let _ = events.send(AgentEvent::Assistant(reply));
        if outcome.tool_calls.is_empty() {
            fire(
                &hooks,
                role,
                &hook_cwd,
                "Stop",
                "turn",
                json!({ "providerId": model.provider.id, "model": model.model, "assistantChars": outcome.text.chars().count(), "toolCalls": 0 }),
                json!({}),
            )
            .await;
            return Ok(());
        }

        let mut results = Vec::new();
        for call in outcome.tool_calls {
            let _ = events.send(AgentEvent::ToolStarted { description: tools::describe(&call), call: call.clone() });
            let finish = |output: ToolOutput, denied: bool, results: &mut Vec<ToolResult>| {
                let _ = events.send(AgentEvent::ToolFinished { call_id: call.id.clone(), output: output.clone(), denied });
                results.push(ToolResult { call_id: call.id.clone(), content: output.content, is_error: output.is_error });
            };
            if !offered(&call.name) {
                let output = ToolOutput { content: format!("There is no tool called {} for this agent.", call.name), is_error: true };
                finish(output, false, &mut results);
                continue;
            }
            let pre = fire(
                &hooks,
                role,
                &hook_cwd,
                "PreToolUse",
                &call.name,
                json!({ "id": call.id, "name": call.name, "argsPreview": preview(&call.arguments, 1000) }),
                json!({ "toolArgs": call.arguments_json(), "toolArguments": call.arguments }),
            )
            .await;
            if let Some(reason) = pre.denied {
                finish(ToolOutput { content: reason, is_error: true }, true, &mut results);
                continue;
            }
            let mut output = if is_host(&call.name) {
                let (respond, answer) = oneshot::channel();
                let _ = events.send(AgentEvent::HostCall { call: call.clone(), respond });
                answer.await.unwrap_or_else(|_| ToolOutput { content: "The app did not answer.".into(), is_error: true })
            } else if delegates(&call.name) {
                let children = children.as_ref().expect("checked");
                if call.name == crate::workflow::TOOL {
                    crate::workflow::execute(children, &call).await
                } else {
                    children.execute(&call, &history).await
                }
            } else {
                let boxed = in_box(&call.name);
                let project = cwd.as_deref();
                if !boxed && project.is_none() {
                    finish(ToolOutput { content: format!("There is no tool called {}.", call.name), is_error: true }, false, &mut results);
                    continue;
                }
                let permission = match (&toolbox, project) {
                    (Some(toolbox), _) if boxed => toolbox.permission(&call, &unlocked),
                    (_, Some(project)) => tools::permission(&call, project),
                    _ => None,
                };
                let mut denied: Option<&'static str> = None;
                if let Some(permission) = permission {
                    let rememberable = permission.rememberable;
                    let granted = rememberable && grants.lock().map(|g| g.contains(&permission.grant)).unwrap_or(false);
                    if !granted {
                        if !ask {
                            finish(
                                ToolOutput {
                                    content: format!(
                                        "Blocked: tool \"{}\" requires interactive approval, which is disabled for subagents.",
                                        call.name
                                    ),
                                    is_error: true,
                                },
                                true,
                                &mut results,
                            );
                            continue;
                        }
                        let request = fire(
                            &hooks,
                            role,
                            &hook_cwd,
                            "PermissionRequest",
                            &call.name,
                            json!({ "request": { "type": call.name, "summary": permission.summary, "detail": permission.detail } }),
                            json!({}),
                        )
                        .await;
                        let decision = match request.denied {
                            Some(reason) => {
                                finish(ToolOutput { content: format!("Blocked by a hook: {reason}"), is_error: true }, true, &mut results);
                                continue;
                            }
                            None => {
                                let (respond, answer) = oneshot::channel();
                                let grant = permission.grant.clone();
                                let summary = permission.summary.clone();
                                let _ = events.send(AgentEvent::ApprovalNeeded { call: call.clone(), permission, respond });
                                let decision = answer.await.unwrap_or(Decision::Deny);
                                if decision == Decision::AllowForTask && rememberable {
                                    if let Ok(mut grants) = grants.lock() {
                                        grants.insert(grant);
                                    }
                                }
                                fire(
                                    &hooks,
                                    role,
                                    &hook_cwd,
                                    "PermissionResult",
                                    &call.name,
                                    json!({ "request": { "type": call.name, "summary": summary }, "decision": if matches!(decision, Decision::Deny | Decision::Refuse(_)) { "deny" } else { "approve" } }),
                                    json!({}),
                                )
                                .await;
                                decision
                            }
                        };
                        denied = match decision {
                            Decision::Deny => Some("The person declined this. Do not retry it as is; ask them or take another way."),
                            Decision::Refuse(reason) => Some(reason),
                            _ => None,
                        };
                    }
                }
                if let Some(reason) = denied {
                    finish(ToolOutput { content: reason.into(), is_error: true }, true, &mut results);
                    continue;
                }
                match (&toolbox, project) {
                    (Some(toolbox), _) if boxed => toolbox.execute(&call, &unlocked).await,
                    (_, Some(project)) => tools::execute(&call, project).await,
                    _ => ToolOutput { content: format!("There is no tool called {}.", call.name), is_error: true },
                }
            };
            let event = if output.is_error { "PostToolUseFailure" } else { "PostToolUse" };
            let post = fire(
                &hooks,
                role,
                &hook_cwd,
                event,
                &call.name,
                json!({
                    "id": call.id,
                    "name": call.name,
                    "argsPreview": preview(&call.arguments, 1000),
                    "resultPreview": preview(&output.content, 1000),
                    "isError": output.is_error,
                }),
                json!({ "toolArgs": call.arguments_json(), "toolArguments": call.arguments, "toolResult": output.content }),
            )
            .await;
            output.content.push_str(&reminder(event, &post.context));
            finish(output, false, &mut results);
        }
        let message = ChatMessage::Tool { results };
        history.push(message.clone());
        let _ = events.send(AgentEvent::ToolResults(message));
    }
    bail!("stopped after {MAX_STEPS} model calls in one turn")
}

#[cfg(test)]
mod tests {
    use super::*;

    static NEXT_DIR: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    use crate::provider::tests::{client, model, serve};

    fn openai_tool_call(id: &str, name: &str, arguments: &str) -> String {
        let arguments = serde_json::to_string(arguments).unwrap();
        format!(
            "data: {{\"choices\":[{{\"delta\":{{\"tool_calls\":[{{\"index\":0,\"id\":\"{id}\",\"function\":{{\"name\":\"{name}\",\"arguments\":{arguments}}}}}]}},\"finish_reason\":\"tool_calls\"}}]}}\n\ndata: [DONE]\n\n"
        )
    }

    fn openai_text(text: &str) -> String {
        format!("data: {{\"choices\":[{{\"delta\":{{\"content\":\"{text}\"}},\"finish_reason\":\"stop\"}}]}}\n\ndata: [DONE]\n\n")
    }

    fn project() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("bubble-agent-{}-{}", std::process::id(), NEXT_DIR.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("notes.txt"), "draft").unwrap();
        dir
    }

    async fn run(bodies: Vec<String>, answer: Decision, grants: Arc<Mutex<HashSet<String>>>) -> (Result<()>, Vec<AgentEvent>, PathBuf) {
        let (url, _server) = serve("200 OK", bodies).await;
        let dir = project();
        let turn = Turn {
            model: model(&url, "openai", "deepseek"),
            system: None,
            history: vec![ChatMessage::user("fix the notes")],
            cwd: Some(dir.clone()),
            grants,
            host_tools: Vec::new(),
            max_tokens: 256,
            thinking: Level::Off,
            toolbox: None,
            unlocked: Default::default(),
            hooks: None,
            role: hooks::Role::Parent,
            allowed: None,
            ask: true,
            children: None,
        };
        let (sender, mut receiver) = mpsc::unbounded_channel();
        let agent = tokio::spawn(async move { run_turn(&client(), turn, sender).await });
        let mut events = Vec::new();
        while let Some(event) = receiver.recv().await {
            if let AgentEvent::ApprovalNeeded { respond, call, permission } = event {
                let _ = respond.send(answer);
                events.push(AgentEvent::ToolStarted { call, description: permission.summary });
                continue;
            }
            events.push(event);
        }
        (agent.await.unwrap(), events, dir)
    }

    #[tokio::test]
    async fn reads_run_without_asking_and_the_turn_ends_on_text() {
        let grants = Arc::new(Mutex::new(HashSet::new()));
        let (result, events, _) = run(
            vec![openai_tool_call("c1", "read", "{\"path\":\"notes.txt\"}"), openai_text("It says draft.")],
            Decision::Deny,
            grants,
        )
        .await;
        result.unwrap();
        let finished: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                AgentEvent::ToolFinished { output, denied, .. } => Some((output.content.clone(), *denied)),
                _ => None,
            })
            .collect();
        assert_eq!(finished.len(), 1);
        assert!(finished[0].0.contains("draft") && !finished[0].1);
        assert!(matches!(events.last(), Some(AgentEvent::Assistant(ChatMessage::Assistant { text, .. })) if text == "It says draft."));
    }

    #[tokio::test]
    async fn a_denied_edit_leaves_the_file_alone() {
        let grants = Arc::new(Mutex::new(HashSet::new()));
        let (result, events, dir) = run(
            vec![
                openai_tool_call("c1", "write", "{\"path\":\"notes.txt\",\"content\":\"final\"}"),
                openai_text("Understood."),
            ],
            Decision::Deny,
            grants,
        )
        .await;
        result.unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("notes.txt")).unwrap(), "draft");
        assert!(events.iter().any(|e| matches!(e, AgentEvent::ToolFinished { denied: true, .. })));
    }

    /// A PreToolUse hook blocks a command before anyone is asked, and a
    /// prompt hook's context reaches the model.
    #[tokio::test]
    async fn hooks_deny_tools_and_add_context() {
        let dir = project();
        let deny = dir.join("deny.sh");
        std::fs::write(&deny, "#!/bin/sh\necho 'no shell here' >&2\nexit 2\n").unwrap();
        let context = dir.join("context.sh");
        std::fs::write(&context, "#!/bin/sh\necho '{\"modelContext\":\"Use tabs.\",\"exposeToModel\":true}'\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        for script in [&deny, &context] {
            std::fs::set_permissions(script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let home = dir.join("home");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(
            home.join("settings.json"),
            serde_json::json!({ "hooks": [
                { "event": "PreToolUse", "matcher": "^bash$", "command": deny },
                { "event": "UserPromptSubmit", "command": context },
            ]})
            .to_string(),
        )
        .unwrap();
        let (url, server) =
            serve("200 OK", vec![openai_tool_call("c1", "bash", "{\"command\":\"rm -rf x\"}"), openai_text("Blocked.")]).await;
        let turn = Turn {
            cwd: Some(dir.clone()),
            hooks: Some(Arc::new(Hooks::load(&home, None))),
            ..Turn::new(model(&url, "openai", "deepseek"), None, vec![ChatMessage::user("clean up")])
        };
        let (sender, mut receiver) = mpsc::unbounded_channel();
        let agent = tokio::spawn(async move { run_turn(&client(), turn, sender).await });
        let mut asked = false;
        let mut outputs = Vec::new();
        while let Some(event) = receiver.recv().await {
            match event {
                AgentEvent::ApprovalNeeded { .. } => asked = true,
                AgentEvent::ToolFinished { output, denied, .. } => outputs.push((output.content, denied)),
                _ => {}
            }
        }
        agent.await.unwrap().unwrap();
        assert!(!asked, "a hook's deny comes before asking");
        assert_eq!(outputs, [("no shell here".to_owned(), true)]);
        let requests = server.await.unwrap();
        assert!(requests[0].contains("clean up\\n\\n<system-reminder>\\n[Hook UserPromptSubmit] Use tabs."));
    }

    /// An MCP tool is named in the prompt, loaded with tool_search, then
    /// called after the person's yes.
    #[tokio::test]
    async fn deferred_mcp_tools_load_then_run_after_asking() {
        let dir = project();
        let script = dir.join("server.py");
        std::fs::write(
            &script,
            r#"
import json, sys
for line in sys.stdin:
    m = json.loads(line)
    reply = None
    if m.get("method") == "initialize":
        reply = {"capabilities": {"tools": {}}}
    elif m.get("method") == "tools/list":
        reply = {"tools": [{"name": "echo", "description": "Echo text", "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}}}]}
    elif m.get("method") == "tools/call":
        reply = {"content": [{"type": "text", "text": "echo: " + m["params"]["arguments"]["text"]}]}
    if reply is not None:
        sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": m["id"], "result": reply}) + "\n"); sys.stdout.flush()
"#,
        )
        .unwrap();
        let hub = crate::mcp::Hub::new();
        let servers = hub
            .connect(
                vec![crate::mcp::ScopedServer {
                    name: "t".into(),
                    scope: crate::mcp::Scope::User,
                    config: crate::mcp::ServerConfig::Stdio {
                        command: "python3".into(),
                        args: vec![script.display().to_string()],
                        env: Default::default(),
                        cwd: None,
                    },
                }],
                &client(),
            )
            .await;
        let mut toolbox = Toolbox::empty(&client());
        toolbox.servers = servers;
        let (url, server) = serve(
            "200 OK",
            vec![
                openai_tool_call("c1", "mcp__t__echo", "{\"text\":\"early\"}"),
                openai_tool_call("c2", "tool_search", "{\"query\":\"select:mcp__t__echo\"}"),
                openai_tool_call("c3", "mcp__t__echo", "{\"text\":\"hi\"}"),
                openai_text("It echoed."),
            ],
        )
        .await;
        let unlocked: Arc<Mutex<HashSet<String>>> = Default::default();
        let turn = Turn {
            model: model(&url, "openai", "deepseek"),
            system: Some("Be brief.".into()),
            history: vec![ChatMessage::user("echo hi")],
            cwd: Some(dir),
            grants: Default::default(),
            host_tools: Vec::new(),
            max_tokens: 256,
            thinking: Level::Off,
            toolbox: Some(Arc::new(toolbox)),
            unlocked: unlocked.clone(),
            hooks: None,
            role: hooks::Role::Parent,
            allowed: None,
            ask: true,
            children: None,
        };
        let (sender, mut receiver) = mpsc::unbounded_channel();
        let agent = tokio::spawn(async move { run_turn(&client(), turn, sender).await });
        let mut asked = Vec::new();
        let mut outputs = Vec::new();
        while let Some(event) = receiver.recv().await {
            match event {
                AgentEvent::ApprovalNeeded { respond, permission, .. } => {
                    asked.push(permission.summary);
                    let _ = respond.send(Decision::AllowOnce);
                }
                AgentEvent::ToolFinished { output, .. } => outputs.push(output),
                _ => {}
            }
        }
        agent.await.unwrap().unwrap();
        assert_eq!(asked, ["Use echo from the t MCP server"], "only the loaded call asks");
        assert!(outputs[0].is_error && outputs[0].content.contains("deferred tool"));
        assert!(outputs[1].content.starts_with("<functions>\n<function>{"));
        assert_eq!(outputs[2], ToolOutput { content: "echo: hi".into(), is_error: false });
        assert!(unlocked.lock().unwrap().contains("mcp__t__echo"));
        let requests = server.await.unwrap();
        assert!(requests[0].contains("Be brief.\\n\\n## Deferred tools"));
        assert!(requests[0].contains("\"name\":\"tool_search\""));
        assert!(!requests[0].contains("\"name\":\"mcp__t__echo\""), "not offered before loading");
        assert!(requests[2].contains("\"name\":\"mcp__t__echo\""), "offered once loaded");
        assert!(requests[3].contains("echo: hi"));
    }

    #[tokio::test]
    async fn host_tools_are_answered_by_the_app_without_a_project() {
        let (url, server) = serve(
            "200 OK",
            vec![openai_tool_call("c1", "list_tasks", "{}"), openai_text("Nothing running.")],
        )
        .await;
        let turn = Turn {
            model: model(&url, "openai", "kimi-for-coding"),
            system: None,
            history: vec![ChatMessage::user("what is running?")],
            cwd: None,
            grants: Arc::new(Mutex::new(HashSet::new())),
            host_tools: vec![crate::provider::ToolSpec {
                name: "list_tasks".into(),
                description: "Lists tasks".into(),
                parameters: serde_json::json!({"type": "object"}),
            }],
            max_tokens: 256,
            thinking: Level::Off,
            toolbox: None,
            unlocked: Default::default(),
            hooks: None,
            role: hooks::Role::Parent,
            allowed: None,
            ask: true,
            children: None,
        };
        let (sender, mut receiver) = mpsc::unbounded_channel();
        let agent = tokio::spawn(async move { run_turn(&client(), turn, sender).await });
        let mut answered = false;
        while let Some(event) = receiver.recv().await {
            if let AgentEvent::HostCall { call, respond } = event {
                assert_eq!(call.name, "list_tasks");
                let _ = respond.send(ToolOutput { content: "No tasks.".into(), is_error: false });
                answered = true;
            }
        }
        agent.await.unwrap().unwrap();
        assert!(answered);
        let requests = server.await.unwrap();
        assert!(requests[0].contains("\"list_tasks\""));
        assert!(!requests[0].contains("\"bash\""), "no project tools without a project");
        assert!(requests[1].contains("No tasks."));
    }

    #[tokio::test]
    async fn a_history_near_the_window_is_summarized_before_the_call() {
        let (url, server) = serve(
            "200 OK",
            vec![openai_text("Read a.rs; it is huge and unchanged."), openai_text("Continuing.")],
        )
        .await;
        let call = |id: &str| ChatMessage::Assistant {
            text: String::new(),
            reasoning: String::new(),
            tool_calls: vec![ToolCall { id: id.into(), name: "read".into(), arguments: "{\"path\":\"a.rs\"}".into() }],
        };
        let result = |id: &str, content: String| ChatMessage::Tool {
            results: vec![ToolResult { call_id: id.into(), content, is_error: false }],
        };
        let history = vec![
            ChatMessage::user("Study a.rs"),
            call("c1"),
            result("c1", "a".repeat(140_000)),
            call("c2"),
            result("c2", "short".into()),
            call("c3"),
            result("c3", "short".into()),
        ];
        let turn = Turn {
            model: model(&url, "openai", "somewhere"),
            system: None,
            history,
            cwd: None,
            grants: Arc::new(Mutex::new(HashSet::new())),
            host_tools: Vec::new(),
            max_tokens: 256,
            thinking: Level::Off,
            toolbox: None,
            unlocked: Default::default(),
            hooks: None,
            role: hooks::Role::Parent,
            allowed: None,
            ask: true,
            children: None,
        };
        let (sender, mut receiver) = mpsc::unbounded_channel();
        let agent = tokio::spawn(async move { run_turn(&client(), turn, sender).await });
        let mut compacted = None;
        while let Some(event) = receiver.recv().await {
            if let AgentEvent::Compacted(history) = event {
                compacted = Some(history);
            }
        }
        agent.await.unwrap().unwrap();
        let compacted = compacted.expect("the history was compacted");
        assert!(matches!(&compacted[1], ChatMessage::User { text } if text.contains("Read a.rs; it is huge")));
        let requests = server.await.unwrap();
        assert!(requests[0].contains("CONTEXT CHECKPOINT COMPACTION"));
        assert!(requests[1].contains("compaction-summary"));
        assert!(requests[1].len() < 20_000, "the big tool output is gone from the real call");
    }

    #[tokio::test]
    async fn allowing_for_the_task_remembers_file_changes() {
        let grants = Arc::new(Mutex::new(HashSet::new()));
        let (result, _, dir) = run(
            vec![openai_tool_call("c1", "write", "{\"path\":\"notes.txt\",\"content\":\"final\"}"), openai_text("Done.")],
            Decision::AllowForTask,
            grants.clone(),
        )
        .await;
        result.unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("notes.txt")).unwrap(), "final");
        assert!(grants.lock().unwrap().contains("files"));
    }
}
