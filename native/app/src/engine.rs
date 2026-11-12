//! Turns run on Bubble's agent: the TypeScript core, in the host process
//! (`bubble host --stdio`). This module starts that process once for the
//! app and reports a turn in the words the conversation already keeps.

use std::sync::Arc;

use anyhow::{Result, anyhow};
use bubble_core::{AgentEvent, ChatMessage, Decision, Permission, ToolCall, ToolOutput, ToolResult, ToolSpec};
use bubble_host::{Approval, ApprovalRequest, Event, Host, HostTool, Launch, Picture, SessionMessage, Terminal, TurnRequest};
use tokio::sync::{Notify, mpsc, oneshot};

/// The app's one host process, started when first needed and again if it
/// died.
#[derive(Default)]
pub struct Hosts {
    running: tokio::sync::Mutex<Option<Host>>,
}

impl Hosts {
    pub async fn get(&self) -> Result<Host> {
        let mut running = self.running.lock().await;
        if let Some(host) = running.as_ref().filter(|host| host.is_alive()) {
            return Ok(host.clone());
        }
        // The login shell's environment: an app opened from the Finder has
        // no PATH to find Node, or the tools the agent's commands use.
        let mut env = tokio::task::spawn_blocking(|| bubble_core::mcp::shell_env().clone()).await?;
        // The agent keeps its data where the app does, whatever the login
        // shell says: a development build's sessions stay with its bots.
        env.insert("BUBBLE_HOME".to_owned(), bubble_core::bubble_home().to_string_lossy().into_owned());
        let host = Host::start(Launch::find(&env)?).await?;
        *running = Some(host.clone());
        Ok(host)
    }

}

/// The session of a run: its conversation as the agent keeps it.
pub fn session_of(run_id: &str) -> String {
    format!("desk-{run_id}")
}

/// One turn handed to the agent.
pub struct HostTurn {
    pub session_id: String,
    /// The folder the session lives in: one place for a conversation's
    /// whole life, whatever it works on.
    pub home: String,
    /// The folder this turn's tools work in.
    pub cwd: String,
    pub text: String,
    pub pictures: Vec<Picture>,
    /// "provider:model".
    pub model: String,
    pub thinking: String,
    /// What the agent's own system prompt calls it: the bot's name.
    pub agent_name: Option<String>,
    /// After the agent's own system prompt.
    pub append_system_prompt: Option<String>,
    /// Instead of the agent's own system prompt.
    pub system_prompt: Option<String>,
    /// Only these built-in tools; `None` offers all.
    pub allowed_tools: Option<Vec<String>>,
    /// Tools the app answers itself.
    pub host_tools: Vec<ToolSpec>,
    /// What was said before the agent kept this conversation (a run from
    /// the earlier engine); given to the model once, when its session is new.
    pub recap: Option<String>,
}

/// What a turn reports beyond the agent's own events.
pub enum TurnEvent {
    Agent(AgentEvent),
    /// The context was summarized to stay within the model's window.
    Compacted,
    /// The model's reply broke off and is asked for again: what it had
    /// written of it so far is superseded.
    Retried,
    /// The agent asks the person something. The answer is the labels
    /// chosen for each ask, or `None` when the person will not answer.
    Question {
        /// The `question` call it belongs to, when the agent said.
        call_id: Option<String>,
        asks: Vec<crate::transcript::Ask>,
        respond: oneshot::Sender<Option<Vec<Vec<String>>>>,
    },
    /// A subagent moved on; `tool` is the call it reports through
    /// (spawn_agent, wait_agent, run_workflow).
    Subagent { tool: String, update: bubble_host::SubagentUpdate },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ending {
    Completed,
    Stopped,
}

/// One model request of the turn, as it accumulates.
#[derive(Default)]
struct Step {
    text: String,
    reasoning: String,
    calls: Vec<ToolCall>,
    /// The reply (with its calls) was handed on.
    announced: bool,
    results: Vec<ToolResult>,
}

impl Step {
    fn announce(&mut self, events: &mpsc::UnboundedSender<TurnEvent>) {
        if self.announced {
            return;
        }
        self.announced = true;
        if self.text.trim().is_empty() && self.reasoning.is_empty() && self.calls.is_empty() {
            return;
        }
        let _ = events.send(TurnEvent::Agent(AgentEvent::Assistant(ChatMessage::Assistant {
            text: std::mem::take(&mut self.text),
            reasoning: std::mem::take(&mut self.reasoning),
            tool_calls: self.calls.clone(),
            state: serde_json::Value::Null,
        })));
    }

    fn call(&self, id: Option<&str>) -> Option<ToolCall> {
        let id = id?;
        self.calls.iter().find(|call| call.id == id).cloned()
    }
}

/// Runs `turn` to its end. `stop` ends it early (the reply so far stays).
pub async fn run(host: Host, turn: HostTurn, events: mpsc::UnboundedSender<TurnEvent>, stop: Arc<Notify>) -> Result<Ending> {
    let session = turn.session_id.clone();
    host.open_session(&session, &turn.home).await?;
    let mut text = turn.text;
    if let Some(recap) = turn.recap.filter(|r| !r.trim().is_empty()) {
        if host.history(&session).await.map(|h| h.is_empty()).unwrap_or(false) {
            text = format!(
                "<earlier-conversation>\nThis task was started before; what was said so far, oldest first:\n\n{recap}\n</earlier-conversation>\n\n{text}"
            );
        }
    }
    let mut messages = host.subscribe(&session);
    host.start_turn(TurnRequest {
        session_id: session.clone(),
        cwd: Some(turn.cwd),
        text,
        pictures: turn.pictures,
        model: Some(turn.model),
        thinking: Some(turn.thinking),
        // The app decides each question itself (a task's full access is
        // read at every call), so the agent always asks.
        full_access: false,
        agent_name: turn.agent_name,
        append_system_prompt: turn.append_system_prompt,
        system_prompt: turn.system_prompt,
        allowed_tools: turn.allowed_tools,
        host_tools: turn
            .host_tools
            .into_iter()
            // Read-only to the agent: the app asks the person where a call needs it.
            .map(|spec| HostTool { name: spec.name, description: spec.description, parameters: spec.parameters, read_only: true })
            .collect(),
    })
    .await?;

    let mut step = Step::default();
    let mut stopping = false;
    let ending = loop {
        let message = tokio::select! {
            message = messages.recv() => message,
            _ = stop.notified(), if !stopping => {
                stopping = true;
                let _ = host.stop(&session).await;
                continue;
            }
        };
        let Some(message) = message else { break Err(anyhow!("the Bubble agent process stopped")) };
        match message {
            SessionMessage::Event { event, terminal, .. } => {
                report(&mut step, event, &events);
                match terminal {
                    None => {}
                    Some(Terminal::Completed) => break Ok(Ending::Completed),
                    Some(Terminal::Cancelled { .. }) => break Ok(Ending::Stopped),
                    Some(Terminal::Failed { .. }) if stopping => break Ok(Ending::Stopped),
                    Some(Terminal::Failed { message }) => break Err(anyhow!(message)),
                }
            }
            SessionMessage::Approval { id, tool_call_id, request } => {
                let call = step.call(tool_call_id.as_deref()).unwrap_or_else(|| ToolCall {
                    id: tool_call_id.unwrap_or_else(|| id.clone()),
                    name: tool_of(&request).to_owned(),
                    arguments: "{}".into(),
                });
                let (respond, answer) = oneshot::channel();
                let _ = events.send(TurnEvent::Agent(AgentEvent::ApprovalNeeded { call, permission: permission_of(&request), respond }));
                let host = host.clone();
                tokio::spawn(async move {
                    let approval = match answer.await {
                        Ok(Decision::AllowOnce) => Approval::Once,
                        Ok(Decision::AllowForTask) => Approval::ForSession,
                        Ok(Decision::Refuse(why)) => Approval::Deny(Some(why.to_owned())),
                        Ok(Decision::Deny) | Err(_) => Approval::Deny(None),
                    };
                    host.approve(&id, &approval);
                });
            }
            SessionMessage::ToolCall { id, tool_call_id, name, args } => {
                let call = step.call(tool_call_id.as_deref()).unwrap_or_else(|| ToolCall {
                    id: tool_call_id.unwrap_or_else(|| id.clone()),
                    name,
                    arguments: args.to_string(),
                });
                let (respond, answer) = oneshot::channel::<ToolOutput>();
                let _ = events.send(TurnEvent::Agent(AgentEvent::HostCall { call, respond }));
                let host = host.clone();
                tokio::spawn(async move {
                    match answer.await {
                        Ok(output) => host.tool_result(&id, &output.content, output.is_error),
                        Err(_) => host.tool_result(&id, "The app did not run this tool.", true),
                    }
                });
            }
            SessionMessage::Question { id, request } => {
                let asks = crate::transcript::Ask::from_request(&request);
                let call_id = request.get("tool").and_then(|t| t.get("callID")).and_then(|c| c.as_str()).map(str::to_owned);
                let (respond, answer) = oneshot::channel();
                let _ = events.send(TurnEvent::Question { call_id, asks, respond });
                let host = host.clone();
                tokio::spawn(async move {
                    // No answer (skipped, or nobody there) declines the question.
                    let answers = answer.await.ok().flatten();
                    host.answer(&id, serde_json::json!({ "answers": answers }));
                });
            }
            // The app has no plan mode: a plan is turned down, and the
            // agent says what it would do in its reply instead.
            SessionMessage::Plan { id, .. } => host.answer(&id, serde_json::json!({ "approved": false })),
            // The person added this folder as a project: its own settings
            // (MCP servers, allow rules) are theirs.
            SessionMessage::Trust { id, .. } => host.answer(&id, serde_json::json!({ "trusted": true })),
            SessionMessage::Started(_) | SessionMessage::Withdrawn { .. } => {}
            SessionMessage::Lost(why) => break Err(anyhow!(why)),
        }
    };
    host.unsubscribe(&session);
    ending
}

fn report(step: &mut Step, event: Event, events: &mpsc::UnboundedSender<TurnEvent>) {
    let agent = |event: AgentEvent| {
        let _ = events.send(TurnEvent::Agent(event));
    };
    match event {
        Event::TurnStart => {
            *step = Step::default();
            agent(AgentEvent::ModelStarted);
        }
        Event::TextDelta { content } => {
            step.text.push_str(&content);
            agent(AgentEvent::Text(content));
        }
        Event::ReasoningDelta { content } => {
            step.reasoning.push_str(&content);
            agent(AgentEvent::Reasoning(content));
        }
        Event::ToolCallEnd { id, name, arguments } => {
            if !step.calls.iter().any(|call| call.id == id) {
                step.calls.push(ToolCall { id, name, arguments });
            }
        }
        Event::ToolStart { id, name, args } => {
            if !step.calls.iter().any(|call| call.id == id) {
                step.calls.push(ToolCall { id: id.clone(), name, arguments: args.to_string() });
            }
            step.announce(events);
            if let Some(call) = step.call(Some(&id)) {
                let description = bubble_core::tools::describe(&call);
                agent(AgentEvent::ToolStarted { call, description });
            }
        }
        Event::ToolUpdate { name, update, .. } => {
            let _ = events.send(TurnEvent::Subagent { tool: name, update });
        }
        Event::ToolEnd { id, result, .. } => {
            let denied = matches!(result.status.as_deref(), Some("blocked"));
            step.results.push(ToolResult { call_id: id.clone(), content: result.content.clone(), is_error: result.is_error });
            agent(AgentEvent::ToolFinished {
                call_id: id,
                output: ToolOutput { content: result.content, is_error: result.is_error },
                denied,
            });
        }
        Event::TurnEnd { .. } => {
            step.announce(events);
            if !step.results.is_empty() {
                agent(AgentEvent::ToolResults(ChatMessage::Tool { results: std::mem::take(&mut step.results) }));
            }
        }
        Event::ProviderRetry { .. } => {
            *step = Step::default();
            let _ = events.send(TurnEvent::Retried);
        }
        Event::ContextCompaction { status, .. } if status == "completed" => {
            let _ = events.send(TurnEvent::Compacted);
        }
        _ => {}
    }
}

fn tool_of(request: &ApprovalRequest) -> &str {
    match request {
        ApprovalRequest::Bash { .. } => "bash",
        ApprovalRequest::Edit { .. } | ApprovalRequest::Patch { .. } => "edit",
        ApprovalRequest::Write { .. } => "write",
        ApprovalRequest::Lsp { .. } => "lsp",
        ApprovalRequest::AgentProfile { .. } => "spawn_agent",
        ApprovalRequest::ExternalTool { title, .. } => title,
    }
}

/// What the person reads to decide.
fn permission_of(request: &ApprovalRequest) -> Permission {
    let rememberable = request.rememberable();
    let (summary, detail) = match request {
        ApprovalRequest::Bash { command, background, .. } => {
            let summary = if *background { "Run a command in the background" } else { "Run a command" };
            (summary.to_owned(), command.clone())
        }
        ApprovalRequest::Edit { path, outside_workspace, protected_path, .. } => {
            (file_summary("Edit a file", *outside_workspace, *protected_path), path.clone())
        }
        ApprovalRequest::Write { path, outside_workspace, protected_path, .. } => {
            (file_summary("Write a file", *outside_workspace, *protected_path), path.clone())
        }
        ApprovalRequest::Patch { path, .. } => ("Change files".to_owned(), path.clone()),
        ApprovalRequest::Lsp { path, operation } => (format!("Use the language server: {operation}"), path.clone()),
        ApprovalRequest::AgentProfile { name, path, prompt_preview } => {
            (format!("Use this project's agent profile \"{name}\""), format!("{path}\n\n{prompt_preview}"))
        }
        ApprovalRequest::ExternalTool { title, raw_input, .. } => {
            let summary = match bubble_core::mcp::parse_tool_name(title) {
                Some((server, tool)) => format!("Use {tool} from the {server} MCP server"),
                None => format!("Use {title}"),
            };
            let detail = match raw_input {
                serde_json::Value::Null => String::new(),
                input => serde_json::to_string_pretty(input).unwrap_or_default(),
            };
            (summary, detail)
        }
    };
    Permission { grant: String::new(), summary, detail, rememberable }
}

fn file_summary(base: &str, outside: bool, protected: bool) -> String {
    if protected {
        format!("{base} that can change what the agent is allowed to do")
    } else if outside {
        format!("{base} outside the project")
    } else {
        base.to_owned()
    }
}

/// What the stored messages of a run said, for a model that never saw
/// them: the person's and the agent's words, newest kept when it is long.
pub fn recap_of(messages: &[ChatMessage]) -> Option<String> {
    const LIMIT: usize = 24_000;
    let mut parts: Vec<String> = Vec::new();
    for message in messages {
        match message {
            ChatMessage::User { text, .. } if !text.trim().is_empty() => parts.push(format!("Person: {}", text.trim())),
            ChatMessage::Assistant { text, .. } if !text.trim().is_empty() => parts.push(format!("Agent: {}", text.trim())),
            _ => {}
        }
    }
    if parts.is_empty() {
        return None;
    }
    let mut kept: Vec<String> = Vec::new();
    let mut size = 0usize;
    for part in parts.into_iter().rev() {
        size += part.chars().count();
        if size > LIMIT && !kept.is_empty() {
            kept.push("(earlier messages left out)".to_owned());
            break;
        }
        kept.push(part);
    }
    kept.reverse();
    Some(kept.join("\n\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect(events: Vec<Event>) -> Vec<String> {
        let (sender, mut receiver) = mpsc::unbounded_channel();
        let mut step = Step::default();
        for event in events {
            report(&mut step, event, &sender);
        }
        drop(sender);
        let mut seen = Vec::new();
        while let Ok(event) = receiver.try_recv() {
            seen.push(match event {
                TurnEvent::Agent(AgentEvent::ModelStarted) => "model".to_owned(),
                TurnEvent::Agent(AgentEvent::Text(text)) => format!("text:{text}"),
                TurnEvent::Agent(AgentEvent::Reasoning(text)) => format!("reasoning:{text}"),
                TurnEvent::Agent(AgentEvent::Assistant(ChatMessage::Assistant { text, tool_calls, .. })) => {
                    format!("assistant:{text}:{}", tool_calls.iter().map(|c| c.name.as_str()).collect::<Vec<_>>().join(","))
                }
                TurnEvent::Agent(AgentEvent::ToolStarted { call, description }) => format!("started:{}:{description}", call.id),
                TurnEvent::Agent(AgentEvent::ToolFinished { call_id, denied, output }) => {
                    format!("finished:{call_id}:{}:{}", if denied { "denied" } else { "ran" }, output.content)
                }
                TurnEvent::Agent(AgentEvent::ToolResults(ChatMessage::Tool { results })) => format!("results:{}", results.len()),
                TurnEvent::Compacted => "compacted".to_owned(),
                TurnEvent::Retried => "retried".to_owned(),
                TurnEvent::Question { .. } => "question".to_owned(),
                _ => "other".to_owned(),
            });
        }
        seen
    }

    fn event(json: serde_json::Value) -> Event {
        serde_json::from_value(json).unwrap()
    }

    #[test]
    fn a_step_with_tools_keeps_the_reply_before_the_tools_and_the_results_after() {
        let seen = collect(vec![
            event(serde_json::json!({ "type": "turn_start" })),
            event(serde_json::json!({ "type": "text_delta", "content": "Looking." })),
            event(serde_json::json!({ "type": "tool_call_end", "id": "a", "name": "read", "arguments": "{\"path\":\"x.rs\"}" })),
            event(serde_json::json!({ "type": "tool_call_end", "id": "b", "name": "bash", "arguments": "{\"command\":\"ls\"}" })),
            event(serde_json::json!({ "type": "tool_start", "id": "a", "name": "read", "args": { "path": "x.rs" } })),
            event(serde_json::json!({ "type": "tool_end", "id": "a", "name": "read", "result": { "content": "fn main() {}" } })),
            event(serde_json::json!({ "type": "tool_start", "id": "b", "name": "bash", "args": { "command": "ls" } })),
            event(serde_json::json!({ "type": "tool_end", "id": "b", "name": "bash", "result": { "content": "no", "isError": true, "status": "blocked" } })),
            event(serde_json::json!({ "type": "turn_end", "willContinue": true })),
            event(serde_json::json!({ "type": "turn_start" })),
            event(serde_json::json!({ "type": "text_delta", "content": "Done." })),
            event(serde_json::json!({ "type": "turn_end", "willContinue": false })),
            event(serde_json::json!({ "type": "agent_end" })),
        ]);
        assert_eq!(
            seen,
            [
                "model",
                "text:Looking.",
                "assistant:Looking.:read,bash",
                "started:a:Read x.rs",
                "finished:a:ran:fn main() {}",
                "started:b:Ran ls",
                "finished:b:denied:no",
                "results:2",
                "model",
                "text:Done.",
                "assistant:Done.:",
            ]
        );
    }

    #[test]
    fn a_reply_that_broke_off_is_superseded_by_the_one_asked_for_again() {
        let seen = collect(vec![
            event(serde_json::json!({ "type": "turn_start" })),
            event(serde_json::json!({ "type": "text_delta", "content": "cut off" })),
            event(serde_json::json!({ "type": "provider_retry", "attempt": 1, "maxAttempts": 10, "reason": "terminated" })),
            event(serde_json::json!({ "type": "turn_start" })),
            event(serde_json::json!({ "type": "text_delta", "content": "The whole answer." })),
            event(serde_json::json!({ "type": "turn_end", "willContinue": false })),
        ]);
        assert_eq!(seen, ["model", "text:cut off", "retried", "model", "text:The whole answer.", "assistant:The whole answer.:"]);
    }

    #[test]
    fn events_this_app_has_no_use_for_are_passed_over() {
        let seen = collect(vec![
            event(serde_json::json!({ "type": "context_usage", "usedTokens": 10, "estimated": true })),
            event(serde_json::json!({ "type": "hook_start", "eventName": "PreToolUse", "hookId": "h", "source": "user" })),
            event(serde_json::json!({ "type": "context_compaction", "status": "started", "preTokens": 9 })),
            event(serde_json::json!({ "type": "context_compaction", "status": "completed", "preTokens": 9, "postTokens": 3 })),
            event(serde_json::json!({ "type": "something_new", "field": 1 })),
        ]);
        assert_eq!(seen, ["compacted"]);
    }

    #[test]
    fn a_question_names_what_it_is_for() {
        let bash: ApprovalRequest =
            serde_json::from_value(serde_json::json!({ "type": "bash", "command": "git status", "cwd": "/p", "sessionGrant": "git status" })).unwrap();
        let permission = permission_of(&bash);
        assert_eq!((permission.summary.as_str(), permission.detail.as_str(), permission.rememberable), ("Run a command", "git status", true));

        let mcp: ApprovalRequest = serde_json::from_value(serde_json::json!({
            "type": "external_tool", "toolCallId": "c", "title": "mcp__echo__echo", "kind": "mcp", "rawInput": { "text": "hi" }
        }))
        .unwrap();
        let permission = permission_of(&mcp);
        assert_eq!(permission.summary, "Use echo from the echo MCP server");
        assert!(permission.detail.contains("\"text\": \"hi\""));
        assert!(!permission.rememberable);

        let write: ApprovalRequest =
            serde_json::from_value(serde_json::json!({ "type": "write", "path": "/etc/hosts", "content": "x", "fileExists": true, "outsideWorkspace": true }))
                .unwrap();
        assert_eq!(permission_of(&write).summary, "Write a file outside the project");
    }

    #[test]
    fn a_recap_keeps_the_newest_words() {
        let messages = vec![
            ChatMessage::user("Add a changelog"),
            ChatMessage::Assistant { text: String::new(), reasoning: String::new(), tool_calls: vec![], state: serde_json::Value::Null },
            ChatMessage::assistant("Added CHANGELOG.md"),
        ];
        assert_eq!(recap_of(&messages).unwrap(), "Person: Add a changelog\n\nAgent: Added CHANGELOG.md");
        assert_eq!(recap_of(&[]), None);
        let long = vec![ChatMessage::user("a".repeat(30_000)), ChatMessage::assistant("the end")];
        let recap = recap_of(&long).unwrap();
        assert!(recap.starts_with("(earlier messages left out)") && recap.ends_with("Agent: the end"));
    }
}
