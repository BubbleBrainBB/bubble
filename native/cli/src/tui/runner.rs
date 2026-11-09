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
    /// The project's files (for `@` mentions), the prompts typed here, and
    /// the skills (name, description).
    Helpers { files: Vec<String>, history: Vec<String>, skills: Vec<(String, String)> },
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
}

impl Host for Forward {
    async fn event(&mut self, event: AgentEvent) {
        let event = match event {
            AgentEvent::ApprovalNeeded { respond, .. } if self.mode != Mode::Build => {
                let _ = respond.send(crate::automatic(self.mode));
                return;
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
    while let Some(command) = commands.recv().await {
        match command {
            Command::Prompt(prompt) => {
                let folder = session.cwd.display().to_string();
                super::files::remember(&prompt, &folder);
                let prompt = super::files::expand(&prompt, &session.cwd);
                let mut host = Forward { updates: updates.clone(), mode: session.mode };
                let result = session.turn(&prompt, &mut host, stop.notified()).await;
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
            Status::Connected { tools } => format!("{} tools", tools.len()),
            Status::Failed(error) => format!("failed: {error}"),
        };
        lines.push(format!("  {} ({}) — {status}", server.name, server.scope.label()));
    }
    lines.push("/mcp reconnect <name> tries one again.".into());
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
    if let Ok((files, history)) = found {
        let _ = updates.send(Update::Helpers { files, history, skills });
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
