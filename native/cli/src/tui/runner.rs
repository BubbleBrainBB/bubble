//! The agent behind the screen: one task on the runtime that owns the
//! session and takes the screen's commands in order; what happens comes
//! back as [`Update`]s.

use std::sync::Arc;

use bubble_core::prompt::Mode;
use bubble_core::auth::{AuthFile, Subscription};
use bubble_core::{AgentEvent, ChatMessage, Decision, Level, ToolOutput, bubble_home, login};
use bubble_store::{Store, Task};
use tokio::sync::Notify;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::args::Args;
use crate::session::{Host, Session, Start};

pub enum Command {
    Prompt(String),
    Model(String),
    Thinking(Level),
    Mode(Mode),
    /// A new conversation in this folder.
    Fresh,
    /// Continue this one.
    Resume(Task),
    /// List this folder's sessions.
    Sessions,
    /// Sign in to a subscription in the browser (or take the Grok CLI's).
    Login(Subscription),
    Logout(Subscription),
    /// Summarize the older conversation now, keeping what the text names.
    Compact(Option<String>),
    /// What fills the context.
    Context,
    /// The MCP servers, or reconnect the one named.
    Mcp(Option<String>),
    /// Take the clipboard's image for the next prompt.
    PasteImage,
    /// Sign in to an MCP server in the browser.
    McpLogin(String),
    /// Forget an MCP server's sign-in.
    McpLogout(String),
    /// Send an MCP server's prompt, with its arguments as typed.
    McpPrompt { server: String, name: String, args: String },
}

/// An MCP prompt a person can run as `/name args`.
#[derive(Clone, Debug, PartialEq)]
pub struct PromptInfo {
    pub server: String,
    pub name: String,
    pub description: String,
    /// Argument names, and whether each is required.
    pub arguments: Vec<(String, bool)>,
}

/// Arguments as typed: words, or "quoted phrases".
pub fn split_args(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
            continue;
        }
        let mut word = String::new();
        if c == '"' || c == '\'' {
            chars.next();
            for next in chars.by_ref() {
                if next == c {
                    break;
                }
                word.push(next);
            }
        } else {
            while let Some(&next) = chars.peek() {
                if next.is_whitespace() {
                    break;
                }
                word.push(next);
                chars.next();
            }
        }
        out.push(word);
    }
    out
}

/// What the screen shows about the session.
#[derive(Clone, Debug, PartialEq)]
pub struct Info {
    pub model: String,
    pub models: Vec<String>,
    pub thinking: Level,
    pub levels: Vec<Level>,
    pub mode: Mode,
    pub title: String,
    pub cwd: String,
    pub context_window: Option<u64>,
}

pub enum Update {
    /// The session opened (again): its conversation so far.
    Opened(Info, Vec<ChatMessage>),
    Info(Info),
    Agent(AgentEvent),
    /// The turn ended: stopped by the person, or with an error.
    Ended { stopped: bool, error: Option<String> },
    Sessions(Vec<Task>),
    /// A clipboard image waits under this label for the prompt.
    Pasted(String),
    /// The project's files (for `@` mentions), the prompts typed here, and
    /// the skills (name, description).
    Helpers { files: Vec<String>, history: Vec<String>, skills: Vec<(String, String)>, prompts: Vec<PromptInfo> },
    Notice(String),
    Error(String),
}

fn info(session: &Session) -> Info {
    let model = bubble_core::model::info(&session.model.provider.id, &session.model.model);
    Info {
        model: session.reference.clone(),
        models: session.config.selectable_models(),
        thinking: session.thinking,
        levels: model.levels.to_vec(),
        mode: session.mode,
        title: session.task.title.clone(),
        cwd: session.cwd.display().to_string(),
        context_window: model.context_window,
    }
}

/// Sends the turn's events to the screen; decides what needs no person.
struct Forward {
    updates: UnboundedSender<Update>,
    mode: Mode,
    hooks: Arc<bubble_core::hooks::Hooks>,
    cwd: std::path::PathBuf,
}

impl Host for Forward {
    async fn event(&mut self, event: AgentEvent) {
        let event = match event {
            AgentEvent::ApprovalNeeded { respond, .. } if self.mode != Mode::Build => {
                let _ = respond.send(crate::automatic(self.mode));
                return;
            }
            AgentEvent::ApprovalNeeded { ref permission, .. } => {
                // Someone may be away from the terminal: a hook can tell them.
                let message = format!("Bubble needs your approval: {}", permission.summary);
                self.hooks
                    .run(bubble_core::hooks::Role::Parent, bubble_core::hooks::Event {
                        name: "Notification",
                        cwd: &self.cwd,
                        target: "approval",
                        payload: serde_json::json!({ "message": message, "detail": permission.detail }),
                        full: serde_json::json!({}),
                    })
                    .await;
                event
            }
            AgentEvent::HostCall { respond, .. } => {
                let _ = respond.send(ToolOutput { content: "Not available here.".into(), is_error: true });
                return;
            }
            event => event,
        };
        let _ = self.updates.send(Update::Agent(event));
    }
}

fn open_store() -> anyhow::Result<Store> {
    Store::open(bubble_home().join("native").join("bubble.db"))
}

/// Runs the session until the screen goes away.
pub async fn run(mut args: Args, mut commands: UnboundedReceiver<Command>, updates: UnboundedSender<Update>, stop: Arc<Notify>) {
    let opened = match open_store() {
        Ok(store) => Session::open(&args, None, store).await,
        Err(error) => Err(error),
    };
    let mut session = match opened {
        Ok(session) => session,
        Err(error) => {
            let _ = updates.send(Update::Error(format!("{error:#}")));
            return;
        }
    };
    let _ = updates.send(Update::Opened(info(&session), session.history.clone()));
    send_helpers(&session, &updates, true).await;
    // The TypeScript CLI's sessions come in once, in the background.
    if session.store.setting(bubble_store::import::DONE).ok().flatten().is_none() {
        let updates = updates.clone();
        tokio::task::spawn_blocking(move || {
            let report = open_store().and_then(|store| store.import_ts_sessions(&bubble_home().join("sessions")));
            if let Ok(report) = report
                && report.imported > 0
            {
                let _ = updates.send(Update::Notice(format!(
                    "Brought in {} sessions from the TypeScript Bubble; /session lists this folder's.",
                    report.imported
                )));
            }
        });
    }
    // Clipboard images by label, and how many images this session numbered.
    let mut pasted: std::collections::HashMap<String, bubble_core::Image> = std::collections::HashMap::new();
    let mut numbered = 0usize;
    while let Some(command) = commands.recv().await {
        match command {
            Command::Prompt(prompt) => {
                let folder = session.cwd.display().to_string();
                super::files::remember(&prompt, &folder);
                let prompt = super::files::expand(&prompt, &session.cwd);
                let (prompt, images, problems) = super::images::attach(&prompt, &pasted, numbered);
                numbered += images.len().saturating_sub(pasted.keys().filter(|l| prompt.contains(l.as_str())).count());
                for problem in problems {
                    let _ = updates.send(Update::Error(format!("Image not attached: {problem}")));
                }
                let mut host =
                    Forward { updates: updates.clone(), mode: session.mode, hooks: session.hooks(), cwd: session.cwd.clone() };
                let result = session.turn_with(&prompt, images, &mut host, stop.notified()).await;
                let _ = updates.send(match result {
                    Ok(done) => Update::Ended { stopped: !done, error: None },
                    Err(error) => Update::Ended { stopped: false, error: Some(format!("{error:#}")) },
                });
                let _ = updates.send(Update::Info(info(&session)));
                // The agent may have made files.
                send_helpers(&session, &updates, false).await;
            }
            Command::Model(reference) => match session.set_model(&reference).await {
                Ok(()) => {
                    let _ = updates.send(Update::Info(info(&session)));
                    let _ = updates.send(Update::Notice(format!("Model: {reference}")));
                }
                Err(error) => {
                    let _ = updates.send(Update::Error(format!("{error:#}")));
                }
            },
            Command::Thinking(level) => {
                session.thinking = level;
                let _ = updates.send(Update::Info(info(&session)));
                let _ = updates.send(Update::Notice(format!("Reasoning effort: {}", level.as_str())));
            }
            Command::Mode(mode) => {
                session.set_mode(mode);
                let _ = updates.send(Update::Info(info(&session)));
            }
            Command::Login(subscription) => {
                let outcome = sign_in(&session, subscription, &updates, &stop).await;
                let _ = updates.send(match outcome {
                    Ok(Some(text)) => Update::Notice(text),
                    Ok(None) => Update::Notice("Sign-in canceled.".into()),
                    Err(error) => Update::Error(format!("{error:#}")),
                });
                let _ = updates.send(Update::Ended { stopped: false, error: None });
                let _ = updates.send(Update::Info(info(&session)));
            }
            Command::Logout(subscription) => {
                let label = subscription.label();
                let text = match AuthFile::default().remove(subscription) {
                    Ok(true) if session.model.subscription == Some(subscription) => {
                        format!("Signed out of {label} on this device. The current model needs that sign-in: /model picks another.")
                    }
                    Ok(true) => format!("Signed out of {label} on this device."),
                    Ok(false) => format!("No {label} sign-in is stored on this device."),
                    Err(error) => format!("Signing out failed: {error:#}"),
                };
                let _ = updates.send(Update::Notice(text));
                let _ = updates.send(Update::Info(info(&session)));
            }
            Command::Compact(focus) => {
                let text = match session.compact(focus.as_deref()).await {
                    Ok(Some(replaced)) => Update::Notice(format!(
                        "Summarized {replaced} earlier messages to free up context.\n{}",
                        session.context().lines().next().unwrap_or_default()
                    )),
                    Ok(None) => Update::Notice("The conversation is too short to summarize yet.".into()),
                    Err(error) => Update::Error(format!("Compacting failed: {error:#}")),
                };
                let _ = updates.send(text);
                let _ = updates.send(Update::Ended { stopped: false, error: None });
            }
            Command::McpLogin(name) => {
                let found = session.toolbox().servers.iter().find(|s| s.name == name).cloned();
                let outcome = match found {
                    None => Err(format!("No MCP server named {name}.")),
                    Some(server) => match server.login(session.client()).await {
                        Err(error) => Err(format!("{error:#}")),
                        Ok(pending) => {
                            let opened = login::open_browser(&pending.url);
                            let _ = updates.send(Update::Notice(format!(
                                "{} sign in to {name} there. If it did not open, use this address:\n{}\nEsc cancels.",
                                if opened { "Your browser is opening:" } else { "Open your browser and" },
                                pending.url
                            )));
                            let stopped = stop.notified();
                            let finished = tokio::select! {
                                result = pending.finish(session.client()) => Some(result),
                                () = stopped => None,
                            };
                            match finished {
                                None => Ok("Sign-in canceled.".to_owned()),
                                Some(Err(error)) => Err(format!("{error:#}")),
                                Some(Ok(())) => {
                                    server.reset().await;
                                    server.ensure(session.client()).await;
                                    Ok(format!("Signed in to {name}.\n{}", mcp_status(&session.toolbox().servers)))
                                }
                            }
                        }
                    },
                };
                let _ = updates.send(match outcome {
                    Ok(text) => Update::Notice(text),
                    Err(error) => Update::Error(error),
                });
                let _ = updates.send(Update::Ended { stopped: false, error: None });
                send_helpers(&session, &updates, false).await;
            }
            Command::McpLogout(name) => {
                let found = session.toolbox().servers.iter().find(|s| s.name == name).cloned();
                let _ = updates.send(match found.map(|s| s.logout()) {
                    None => Update::Error(format!("No MCP server named {name}.")),
                    Some(Ok(true)) => Update::Notice(format!("Signed out of {name} on this device.")),
                    Some(Ok(false)) => Update::Notice(format!("No sign-in is stored for {name}.")),
                    Some(Err(error)) => Update::Error(format!("{error:#}")),
                });
            }
            Command::McpPrompt { server, name, args } => {
                let found = session.toolbox().servers.iter().find(|s| s.name == server).cloned();
                let prompt = found.as_ref().and_then(|s| s.prompts().into_iter().find(|p| bubble_core::mcp::normalize(&p.name) == name));
                let text = match (found, prompt) {
                    (Some(server), Some(prompt)) => {
                        let values = split_args(&args);
                        let missing: Vec<&str> = prompt
                            .arguments
                            .iter()
                            .enumerate()
                            .filter(|(i, (_, required))| *required && values.get(*i).is_none())
                            .map(|(_, (n, _))| n.as_str())
                            .collect();
                        if missing.is_empty() {
                            let mut map = serde_json::Map::new();
                            for (i, (arg, _)) in prompt.arguments.iter().enumerate() {
                                // The last argument takes whatever is left.
                                let value = if i + 1 == prompt.arguments.len() && values.len() > i {
                                    values[i..].join(" ")
                                } else {
                                    match values.get(i) {
                                        Some(v) => v.clone(),
                                        None => continue,
                                    }
                                };
                                map.insert(arg.clone(), serde_json::Value::String(value));
                            }
                            server.prompt(&prompt.name, serde_json::Value::Object(map)).await.map_err(|e| format!("{e:#}"))
                        } else {
                            Err(format!("/{name} needs {}", missing.iter().map(|m| format!("<{m}>")).collect::<Vec<_>>().join(" ")))
                        }
                    }
                    _ => Err(format!("MCP prompt {name} of {server} is not available")),
                };
                match text {
                    Ok(text) if !text.trim().is_empty() => {
                        let mut host =
                            Forward { updates: updates.clone(), mode: session.mode, hooks: session.hooks(), cwd: session.cwd.clone() };
                        let result = session.turn(&text, &mut host, stop.notified()).await;
                        let _ = updates.send(match result {
                            Ok(done) => Update::Ended { stopped: !done, error: None },
                            Err(error) => Update::Ended { stopped: false, error: Some(format!("{error:#}")) },
                        });
                        let _ = updates.send(Update::Info(info(&session)));
                    }
                    Ok(_) => {
                        let _ = updates.send(Update::Ended { stopped: false, error: Some(format!("/{name} returned no text")) });
                    }
                    Err(error) => {
                        let _ = updates.send(Update::Ended { stopped: false, error: Some(error) });
                    }
                }
            }
            Command::PasteImage => {
                let found = tokio::task::spawn_blocking(super::images::clipboard).await.unwrap_or(Ok(None));
                let _ = updates.send(match found {
                    Ok(Some(image)) => {
                        numbered += 1;
                        let label = super::images::label(numbered, &image);
                        pasted.insert(label.clone(), image);
                        Update::Pasted(label)
                    }
                    Ok(None) => Update::Notice("No image on the clipboard.".into()),
                    Err(problem) => Update::Error(format!("Image not attached: {problem}")),
                });
            }
            Command::Context => {
                let _ = updates.send(Update::Notice(session.context()));
            }
            Command::Mcp(reconnect) => {
                let servers = session.toolbox().servers.clone();
                if let Some(name) = reconnect {
                    let Some(server) = servers.iter().find(|s| s.name == name) else {
                        let _ = updates.send(Update::Error(format!("No MCP server named {name}.")));
                        continue;
                    };
                    server.reset().await;
                    server.ensure(session.client()).await;
                }
                let _ = updates.send(Update::Notice(mcp_status(&servers)));
            }
            Command::Sessions => {
                let folder = session.cwd.display().to_string();
                match session.store.cli_tasks(&folder, 30) {
                    Ok(tasks) => {
                        let _ = updates.send(Update::Sessions(tasks));
                    }
                    Err(error) => {
                        let _ = updates.send(Update::Error(format!("{error:#}")));
                    }
                }
            }
            Command::Fresh | Command::Resume(_) => {
                // The next session keeps this one's model and settings.
                args.model = Some(session.reference.clone());
                args.thinking = Some(session.thinking);
                args.mode = session.mode;
                args.session = None;
                let start = match command {
                    Command::Resume(task) => Start::Task(task),
                    _ => Start::Fresh,
                };
                let _ = session.store.discard_if_empty(&session.task.id);
                let opened = match open_store() {
                    Ok(store) => Session::open_at(&args, None, store, start).await,
                    Err(error) => Err(error),
                };
                match opened {
                    Ok(next) => {
                        session = next;
                        let _ = updates.send(Update::Opened(info(&session), session.history.clone()));
                    }
                    Err(error) => {
                        let _ = updates.send(Update::Error(format!("{error:#}")));
                    }
                }
            }
        }
    }
    session.end().await;
    // A session left without a word is not kept.
    let _ = session.store.discard_if_empty(&session.task.id);
}

/// The MCP servers and how each stands.
fn mcp_status(servers: &[Arc<bubble_core::mcp::Server>]) -> String {
    use bubble_core::mcp::Status;
    if servers.is_empty() {
        return "No MCP servers are configured (mcpServers in ~/.bubble/settings.json or the project's .bubble/settings.json).".into();
    }
    let mut lines = vec!["MCP servers:".to_owned()];
    for server in servers {
        let status = match server.status() {
            Status::NotStarted => "not started".to_owned(),
            Status::Connecting => "connecting…".to_owned(),
            Status::Connected { tools, prompts } if prompts.is_empty() => format!("{} tools", tools.len()),
            Status::Connected { tools, prompts } => format!("{} tools, {} prompts", tools.len(), prompts.len()),
            Status::Failed(error) => format!("failed: {error}"),
        };
        lines.push(format!("  {} ({}) — {status}", server.name, server.scope.label()));
    }
    lines.push("/mcp reconnect <name> tries one again; /mcp login <name> signs in to one that needs it.".into());
    lines.join("\n")
}

/// The project's files, and (`with_history`) the prompts typed in it.
async fn send_helpers(session: &Session, updates: &UnboundedSender<Update>, with_history: bool) {
    let cwd = session.cwd.clone();
    let found = tokio::task::spawn_blocking(move || {
        let history = if with_history { super::files::history(&cwd.display().to_string()) } else { Vec::new() };
        (super::files::list(&cwd), history)
    })
    .await;
    let skills =
        session.toolbox().skills.visible().map(|s| (s.name.clone(), s.description.clone())).collect::<Vec<_>>();
    let prompts = session
        .toolbox()
        .servers
        .iter()
        .flat_map(|server| {
            server.prompts().into_iter().map(|p| PromptInfo {
                server: server.name.clone(),
                name: bubble_core::mcp::normalize(&p.name),
                description: p.description.unwrap_or_default(),
                arguments: p.arguments,
            })
        })
        .collect();
    if let Ok((files, history)) = found {
        let _ = updates.send(Update::Helpers { files, history, skills, prompts });
    }
}

/// Signs in to `subscription`: the Grok CLI's sign-in when there is one,
/// else the browser. `None` when the person canceled.
async fn sign_in(
    session: &Session,
    subscription: Subscription,
    updates: &UnboundedSender<Update>,
    stop: &Notify,
) -> anyhow::Result<Option<String>> {
    let auth = AuthFile::default();
    let label = subscription.label();
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from).unwrap_or_default();
    let imported = subscription == Subscription::Grok
        && !auth.signed_in(subscription)
        && login::import_grok_cli(&auth, &bubble_home(), &home)?;
    if !imported {
        let pending = login::begin(subscription).await?;
        let opened = login::open_browser(&pending.url);
        let _ = updates.send(Update::Notice(format!(
            "{} sign in to {label} there. If it did not open, use this address:\n{}\nEsc cancels.",
            if opened { "Your browser is opening:" } else { "Open your browser and" },
            pending.url
        )));
        let stopped = stop.notified();
        tokio::select! {
            result = pending.finish(session.client(), &auth) => { result?; }
            () = stopped => return Ok(None),
        }
    }
    // Its models join the picker.
    if let Err(error) = bubble_core::discover::discover(session.client(), subscription).await {
        log::warn!("model discovery: {error:#}");
    }
    let how = if imported { "Imported the Grok CLI's sign-in" } else { "Signed in" };
    Ok(Some(format!("{how} to {label}. /model lists its models.")))
}

/// What a decision on the screen answers.
pub fn decision(choice: char) -> Decision {
    match choice {
        'y' => Decision::AllowOnce,
        'a' => Decision::AllowForTask,
        _ => Decision::Deny,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn arguments_split_on_spaces_outside_quotes() {
        assert_eq!(super::split_args(r#"src/a.rs "error handling" x"#), ["src/a.rs", "error handling", "x"]);
        assert!(super::split_args("   ").is_empty());
    }
}
