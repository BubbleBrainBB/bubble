//! `bubble`: the command-line agent. `-p` runs one prompt and exits (plain
//! text, one JSON object, or a persistent stream-json session); without it,
//! an interactive session reads prompts line by line.

mod args;
mod changes;
mod update;
mod session;
mod tui;

use std::io::{IsTerminal, Read, Write};

use anyhow::{Result, anyhow};
use bubble_core::agent::Decision;
use bubble_core::prompt::Mode;
use bubble_core::{AgentEvent, ChatMessage, bubble_home};
use bubble_store::Store;
use serde_json::{Value, json};

use args::{Args, Output};
use session::{Host, Session};

const NO_UI: &str = "No interactive UI is available to approve this tool call.";
const PLAN_REFUSAL: &str =
    "Plan mode is active. Do not call destructive tools directly — propose your changes as your answer and let the user decide.";

/// What `-p` decides when a tool needs a yes.
fn automatic(mode: Mode) -> Decision {
    match mode {
        Mode::Bypass => Decision::AllowOnce,
        Mode::Plan => Decision::Refuse(PLAN_REFUSAL),
        Mode::Build => Decision::Refuse(NO_UI),
    }
}

struct Paint {
    on: bool,
}

impl Paint {
    fn wrap(&self, code: &str, text: &str) -> String {
        if self.on { format!("\x1b[{code}m{text}\x1b[0m") } else { text.to_owned() }
    }
    fn cyan(&self, text: &str) -> String {
        self.wrap("36", text)
    }
    fn dim(&self, text: &str) -> String {
        self.wrap("2", text)
    }
    fn red(&self, text: &str) -> String {
        self.wrap("31", text)
    }
}

fn preview(text: &str, max: usize) -> String {
    if text.chars().count() > max { format!("{}...", text.chars().take(max).collect::<String>()) } else { text.to_owned() }
}

/// Totals for one run, as the JSON outputs report them.
#[derive(Default)]
struct Tally {
    text: String,
    turns: u64,
    tool_calls: u64,
    /// The prompt tokens no cache served.
    input: u64,
    cache_read: u64,
    cache_creation: u64,
    output: u64,
    reasoning: u64,
    total: u64,
    reported: bool,
}

impl Tally {
    fn add(&mut self, event: &AgentEvent) {
        match event {
            AgentEvent::Assistant(ChatMessage::Assistant { text, .. }) => {
                self.turns += 1;
                if !text.trim().is_empty() {
                    self.text = text.trim().to_owned();
                }
            }
            AgentEvent::ToolStarted { .. } => self.tool_calls += 1,
            AgentEvent::Usage(usage) => {
                // As the TypeScript CLI reports it: input is the uncached
                // remainder; the three add up to the whole prompt.
                self.reported = true;
                self.input += usage.input_tokens.saturating_sub(usage.cache_read_tokens + usage.cache_creation_tokens);
                self.cache_read += usage.cache_read_tokens;
                self.cache_creation += usage.cache_creation_tokens;
                self.output += usage.output_tokens;
                self.reasoning += usage.reasoning_tokens;
                self.total += usage.input_tokens + usage.output_tokens;
            }
            _ => {}
        }
    }

    fn summary(&self) -> Value {
        json!({
            "num_turns": self.turns,
            "num_tool_calls": self.tool_calls,
            "usage": {
                "input_tokens": self.input,
                "cache_read_input_tokens": self.cache_read,
                "cache_creation_input_tokens": self.cache_creation,
                "output_tokens": self.output,
                "reasoning_tokens": self.reasoning,
                "total_tokens": self.total,
            },
            "usage_reported": self.reported,
        })
    }
}

/// `-p` with plain output: the answer streams to stdout, tool lines between.
struct PlainHost {
    mode: Mode,
    paint: Paint,
    tally: Tally,
}

impl Host for PlainHost {
    async fn event(&mut self, event: AgentEvent) {
        self.tally.add(&event);
        let mut out = std::io::stdout();
        match event {
            AgentEvent::Text(piece) => {
                let _ = out.write_all(piece.as_bytes());
                let _ = out.flush();
            }
            AgentEvent::ToolStarted { call, .. } => println!("{}", self.paint.cyan(&format!("\n[Tool: {}]", call.name))),
            AgentEvent::ToolFinished { output, .. } => {
                let line = format!("[Result: {}]", preview(&output.content, 200));
                println!("{}", if output.is_error { self.paint.red(&line) } else { self.paint.dim(&line) });
            }
            AgentEvent::ApprovalNeeded { respond, .. } => {
                let _ = respond.send(automatic(self.mode));
            }
            AgentEvent::HostCall { respond, .. } => {
                let _ = respond.send(bubble_core::ToolOutput { content: "Not available here.".into(), is_error: true });
            }
            _ => {}
        }
    }
}

/// `-p --output-format json`: nothing on stdout until the one result object.
struct QuietHost {
    mode: Mode,
    tally: Tally,
}

impl Host for QuietHost {
    async fn event(&mut self, event: AgentEvent) {
        self.tally.add(&event);
        match event {
            AgentEvent::ApprovalNeeded { respond, .. } => {
                let _ = respond.send(automatic(self.mode));
            }
            AgentEvent::HostCall { respond, .. } => {
                let _ = respond.send(bubble_core::ToolOutput { content: "Not available here.".into(), is_error: true });
            }
            _ => {}
        }
    }
}

/// An event as stream-json writes it (the TypeScript event names).
fn event_json(event: &AgentEvent) -> Option<Value> {
    Some(match event {
        AgentEvent::Text(content) => json!({ "type": "text_delta", "content": content }),
        AgentEvent::Reasoning(content) => json!({ "type": "reasoning_delta", "content": content }),
        AgentEvent::Assistant(ChatMessage::Assistant { text, tool_calls, .. }) => json!({
            "type": "turn_end",
            "content": text,
            "toolCalls": tool_calls.iter().map(|c| json!({ "id": c.id, "name": c.name, "arguments": c.arguments })).collect::<Vec<_>>(),
        }),
        AgentEvent::ToolStarted { call, description } => {
            json!({ "type": "tool_start", "id": call.id, "name": call.name, "arguments": call.arguments, "description": description })
        }
        AgentEvent::ToolFinished { call_id, output, denied } => json!({
            "type": "tool_end",
            "id": call_id,
            "denied": denied,
            "result": { "content": output.content, "isError": output.is_error },
        }),
        AgentEvent::Usage(usage) => json!({ "type": "usage", "input_tokens": usage.input_tokens, "output_tokens": usage.output_tokens }),
        AgentEvent::Compacted(_) => json!({ "type": "compaction" }),
        _ => return None,
    })
}

fn write_line(value: &Value) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{value}");
    let _ = out.flush();
}

/// stream-json: every event as a line, enveloped.
struct StreamHost {
    mode: Mode,
    tally: Tally,
    envelope: Value,
}

impl Host for StreamHost {
    async fn event(&mut self, event: AgentEvent) {
        self.tally.add(&event);
        if let Some(mut body) = event_json(&event) {
            let mut line = self.envelope.clone();
            line["type"] = json!("stream_event");
            if let Some(object) = body.as_object_mut() {
                line["event"] = Value::Object(object.clone());
            }
            write_line(&line);
        }
        match event {
            AgentEvent::ApprovalNeeded { respond, .. } => {
                let _ = respond.send(automatic(self.mode));
            }
            AgentEvent::HostCall { respond, .. } => {
                let _ = respond.send(bubble_core::ToolOutput { content: "Not available here.".into(), is_error: true });
            }
            _ => {}
        }
    }
}

/// The interactive session: streams like plain output and asks for yeses.
struct InteractiveHost {
    mode: Mode,
    hooks: Option<std::sync::Arc<bubble_core::hooks::Hooks>>,
    cwd: std::path::PathBuf,
    paint: Paint,
    lines: tokio::sync::mpsc::UnboundedReceiver<String>,
    grants: Vec<String>,
    at_line_start: bool,
}

impl Host for InteractiveHost {
    async fn event(&mut self, event: AgentEvent) {
        let mut out = std::io::stdout();
        match event {
            AgentEvent::Text(piece) => {
                let _ = out.write_all(piece.as_bytes());
                let _ = out.flush();
                self.at_line_start = piece.ends_with('\n');
            }
            AgentEvent::ToolStarted { description, .. } => {
                if !self.at_line_start {
                    println!();
                }
                println!("{}", self.paint.cyan(&format!("• {description}")));
                self.at_line_start = true;
            }
            AgentEvent::ToolFinished { output, denied, .. } if output.is_error || denied => {
                println!("{}", self.paint.red(&format!("  {}", preview(output.content.lines().next().unwrap_or_default(), 160))));
            }
            AgentEvent::ApprovalNeeded { permission, respond, .. } => {
                let decision = match self.mode {
                    Mode::Bypass => Decision::AllowOnce,
                    Mode::Plan => Decision::Refuse(PLAN_REFUSAL),
                    Mode::Build => {
                        if let Some(hooks) = &self.hooks {
                            let message = format!("Bubble needs your approval: {}", permission.summary);
                            hooks
                                .run(bubble_core::hooks::Role::Parent, bubble_core::hooks::Event {
                                    name: "Notification",
                                    cwd: &self.cwd,
                                    target: "approval",
                                    payload: json!({ "message": message, "detail": permission.detail }),
                                    full: json!({}),
                                })
                                .await;
                        }
                        println!("{}", self.paint.wrap("33", &format!("? {}: {}", permission.summary, permission.detail)));
                        let choices = if permission.rememberable { "[y]es once / [a]lways this session / [n]o" } else { "[y]es / [n]o" };
                        print!("  {choices} › ");
                        let _ = out.flush();
                        let answer = self.lines.recv().await.unwrap_or_default().trim().to_lowercase();
                        match answer.as_str() {
                            "y" | "yes" => Decision::AllowOnce,
                            "a" | "always" if permission.rememberable => {
                                self.grants.push(permission.grant.clone());
                                Decision::AllowForTask
                            }
                            _ => Decision::Deny,
                        }
                    }
                };
                let _ = respond.send(decision);
            }
            AgentEvent::HostCall { respond, .. } => {
                let _ = respond.send(bubble_core::ToolOutput { content: "Not available here.".into(), is_error: true });
            }
            AgentEvent::Assistant(ChatMessage::Assistant { tool_calls, .. }) if tool_calls.is_empty() => {
                if !self.at_line_start {
                    println!();
                }
                self.at_line_start = true;
            }
            _ => {}
        }
    }
}

fn read_stdin_if_piped() -> Option<String> {
    let mut stdin = std::io::stdin();
    if stdin.is_terminal() {
        return None;
    }
    let mut text = String::new();
    stdin.read_to_string(&mut text).ok()?;
    Some(text).filter(|t| !t.trim().is_empty())
}

/// The text of a stream-json user message.
fn user_text(value: &Value) -> Result<String> {
    match value {
        Value::String(text) if !text.trim().is_empty() => Ok(text.clone()),
        Value::Array(items) if !items.is_empty() && items.iter().all(|i| i["type"] == "text" && i["text"].is_string()) => {
            let text = items.iter().filter_map(|i| i["text"].as_str()).collect::<Vec<_>>().join("\n");
            if text.trim().is_empty() { Err(anyhow!("message.content must be non-empty text or text blocks")) } else { Ok(text) }
        }
        _ => Err(anyhow!("message.content must be non-empty text or text blocks")),
    }
}

async fn stream_json(mut session: Session) -> Result<()> {
    let session_id = session.task.id.clone();
    write_line(&json!({ "type": "system", "subtype": "init", "session_id": session_id, "model": session.model_label(), "protocol_version": 1 }));
    let (lines, mut incoming) = tokio::sync::mpsc::unbounded_channel::<String>();
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        let mut line = String::new();
        while stdin.read_line(&mut line).map(|n| n > 0).unwrap_or(false) {
            if line.len() > 1024 * 1024 {
                let _ = lines.send(String::from("\u{0}too-long"));
            } else if !line.trim().is_empty() {
                let _ = lines.send(line.trim_end_matches(['\r', '\n']).to_owned());
            }
            line.clear();
        }
    });
    let mut turn = 0;
    while let Some(line) = incoming.recv().await {
        let parsed: Result<(Option<String>, String)> = (|| {
            if line == "\u{0}too-long" {
                return Err(anyhow!("Input line exceeds 1 MiB"));
            }
            let message: Value = serde_json::from_str(&line)?;
            let request_id = message["request_id"].as_str().map(str::to_owned);
            if message["type"] != "user" || message["message"]["role"] != "user" {
                return Err(anyhow!("Expected {{type:\"user\",message:{{role:\"user\",content:\"...\"}}}}"));
            }
            if message["session_id"].as_str().is_some_and(|id| id != session_id) {
                return Err(anyhow!("Session ID does not match this process"));
            }
            Ok((request_id, user_text(&message["message"]["content"])?))
        })();
        let (request_id, prompt) = match parsed {
            Ok(parsed) => parsed,
            Err(error) => {
                let request_id = serde_json::from_str::<Value>(&line).ok().and_then(|m| m["request_id"].as_str().map(str::to_owned));
                write_line(&json!({ "type": "result", "is_error": true, "subtype": "invalid_input", "session_id": session_id, "request_id": request_id, "result": error.to_string() }));
                continue;
            }
        };
        turn += 1;
        let envelope = json!({ "session_id": session_id, "request_id": request_id, "turn": turn });
        let mut host = StreamHost { mode: session.mode, tally: Tally::default(), envelope: envelope.clone() };
        let outcome = session.turn(&prompt, &mut host, std::future::pending()).await;
        let mut result = envelope;
        result["type"] = json!("result");
        if let Some(summary) = host.tally.summary().as_object() {
            for (key, value) in summary {
                result[key] = value.clone();
            }
        }
        match outcome {
            Ok(_) => {
                result["is_error"] = json!(false);
                result["result"] = json!(host.tally.text);
                result["text"] = json!(host.tally.text);
                result["stopReason"] = json!("end_turn");
            }
            Err(error) => {
                result["is_error"] = json!(true);
                result["stopReason"] = json!("error");
                result["result"] = json!(format!("{error:#}"));
            }
        }
        write_line(&result);
    }
    session.end().await;
    Ok(())
}

async fn interactive(mut session: Session, first: Option<String>) -> Result<()> {
    let paint = Paint { on: std::io::stdout().is_terminal() };
    let (lines, incoming) = tokio::sync::mpsc::unbounded_channel::<String>();
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        let mut line = String::new();
        while stdin.read_line(&mut line).map(|n| n > 0).unwrap_or(false) {
            if lines.send(line.clone()).is_err() {
                break;
            }
            line.clear();
        }
    });
    let mode = match session.mode {
        Mode::Build => "",
        Mode::Plan => ", plan mode",
        Mode::Bypass => ", bypassing permissions",
    };
    println!(
        "{}",
        paint.dim(&format!(
            "Bubble ({}{mode}) in {} — session \"{}\". /exit to quit, Ctrl-C stops a turn.",
            session.model_label(),
            session.cwd.display(),
            session.task.title
        ))
    );
    if !session.history.is_empty() {
        println!("{}", paint.dim(&format!("Resumed with {} earlier messages.", session.history.len())));
    }
    let mut host = InteractiveHost {
        mode: session.mode,
        hooks: Some(session.hooks()),
        cwd: session.cwd.clone(),
        paint,
        lines: incoming,
        grants: Vec::new(),
        at_line_start: true,
    };
    let mut next = first;
    loop {
        let prompt = match next.take() {
            Some(prompt) => prompt,
            None => {
                print!("\n{} ", host.paint.wrap("1", "›"));
                let _ = std::io::stdout().flush();
                match host.lines.recv().await {
                    Some(line) => line,
                    None => break,
                }
            }
        };
        let prompt = prompt.trim().to_owned();
        if prompt.is_empty() {
            continue;
        }
        if matches!(prompt.as_str(), "/exit" | "/quit") {
            break;
        }
        match session.turn(&prompt, &mut host, ctrl_c()).await {
            Ok(true) => {}
            Ok(false) => println!("{}", host.paint.dim("\nStopped.")),
            Err(error) => println!("{}", host.paint.red(&format!("\nError: {error:#}"))),
        }
        for grant in host.grants.drain(..) {
            session.grant(grant);
        }
    }
    // A session left without a word is not kept.
    let _ = session.store.discard_if_empty(&session.task.id);
    session.end().await;
    Ok(())
}

async fn run(args: Args) -> Result<i32> {
    let store = Store::open(bubble_home().join("native").join("bubble.db"))?;
    // As in the TypeScript CLI, a prompt on the command line runs once.
    let print = args.print || args.prompt.is_some();
    if print && args.stream_input {
        let session = Session::open(&args, None, store).await?;
        stream_json(session).await?;
        return Ok(0);
    }
    if print {
        let prompt = match args.prompt.clone().or_else(read_stdin_if_piped) {
            Some(prompt) => prompt,
            None => {
                eprintln!("Error: No prompt provided.");
                return Ok(1);
            }
        };
        let mut session = Session::open(&args, Some(&prompt), store).await?;
        if !session.history.is_empty() && args.output != Output::Plain {
            eprintln!("Resumed session: {}", session.task.title);
        }
        if args.output == Output::Json {
            let mut host = QuietHost { mode: args.mode, tally: Tally::default() };
            // What git saw change, as the TypeScript CLI reports it.
            let baseline = changes::baseline(&session.cwd);
            let outcome = session.turn(&prompt, &mut host, std::future::pending()).await;
            let mut result = host.tally.summary();
            let changed = baseline.and_then(|b| changes::detect(&session.cwd, &b));
            match outcome {
                Ok(_) => {
                    result["text"] = json!(host.tally.text);
                    result["stopReason"] = json!("end_turn");
                    result["sessionId"] = json!(session.task.id);
                    if let Some(changed) = changed {
                        result["changes"] = changed;
                    }
                    println!("{result}");
                    session.end().await;
                    return Ok(0);
                }
                Err(error) => {
                    let mut failure = json!({ "type": "error", "message": format!("{error:#}"), "stopReason": "error", "sessionId": session.task.id, "text": host.tally.text });
                    for key in ["num_turns", "num_tool_calls", "usage"] {
                        failure[key] = result[key].clone();
                    }
                    if let Some(changed) = changed {
                        failure["changes"] = changed;
                    }
                    println!("{failure}");
                    session.end().await;
                    return Ok(1);
                }
            }
        }
        let mut host = PlainHost { mode: args.mode, paint: Paint { on: std::io::stdout().is_terminal() }, tally: Tally::default() };
        let outcome = session.turn(&prompt, &mut host, std::future::pending()).await;
        println!();
        session.end().await;
        outcome?;
        return Ok(0);
    }
    let session = Session::open(&args, None, store).await?;
    interactive(session, None).await?;
    Ok(0)
}

/// `bubble import-sessions`: the TypeScript CLI's sessions into the store.
fn import_sessions() -> i32 {
    let store = match Store::open(bubble_home().join("native").join("bubble.db")) {
        Ok(store) => store,
        Err(error) => {
            eprintln!("Error: {error:#}");
            return 1;
        }
    };
    let root = bubble_home().join("sessions");
    println!("Importing sessions from {} …", root.display());
    match store.import_ts_sessions(&root) {
        Ok(report) => {
            println!("Imported {} sessions; {} skipped (already imported or empty).", report.imported, report.skipped);
            for failure in &report.failed {
                eprintln!("  could not import {failure}");
            }
            i32::from(!report.failed.is_empty())
        }
        Err(error) => {
            eprintln!("Error: {error:#}");
            1
        }
    }
}

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if argv.first().map(String::as_str) == Some("import-sessions") {
        std::process::exit(import_sessions());
    }
    if matches!(argv.first().map(String::as_str), Some("update" | "upgrade")) {
        let check = argv.iter().any(|a| a == "--check");
        let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("tokio runtime");
        std::process::exit(runtime.block_on(update::run(check)));
    }
    let current = std::env::current_dir().unwrap_or_default();
    let args = match args::parse(&argv, current) {
        Ok(args) => args,
        Err(error) => {
            eprintln!("{error:#}");
            std::process::exit(1);
        }
    };
    if args.help {
        println!("{}", args::HELP);
        return;
    }
    if args.version {
        println!("bubble {} (native)", update::version());
        return;
    }
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("tokio runtime");
    // A person at a terminal with no prompt gets the full-screen session.
    let full_screen = !args.print
        && args.prompt.is_none()
        && std::io::stdin().is_terminal()
        && std::io::stdout().is_terminal()
        && std::env::var("BUBBLE_TUI").map(|v| v != "0").unwrap_or(true);
    if full_screen {
        let code = match tui::run(args, runtime.handle().clone()) {
            Ok(()) => 0,
            Err(error) => {
                eprintln!("Error: {error:#}");
                1
            }
        };
        runtime.shutdown_timeout(std::time::Duration::from_millis(500));
        std::process::exit(code);
    }
    let code = runtime.block_on(async {
        match run(args).await {
            Ok(code) => code,
            Err(error) => {
                eprintln!("Error: {error:#}");
                1
            }
        }
    });
    // Children (MCP servers, subagents) end with the process.
    runtime.shutdown_timeout(std::time::Duration::from_millis(500));
    std::process::exit(code);
}

/// Finishes at the next Ctrl-C.
async fn ctrl_c() {
    if tokio::signal::ctrl_c().await.is_err() {
        std::future::pending::<()>().await;
    }
}
