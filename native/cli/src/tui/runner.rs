//! The agent behind the screen: one task on the runtime that owns the
//! session and takes the screen's commands in order; what happens comes
//! back as [`Update`]s.

use std::sync::Arc;

use bubble_core::prompt::Mode;
use bubble_core::{AgentEvent, ChatMessage, Decision, Level, ToolOutput, bubble_home};
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
    while let Some(command) = commands.recv().await {
        match command {
            Command::Prompt(prompt) => {
                let mut host = Forward { updates: updates.clone(), mode: session.mode };
                let result = session.turn(&prompt, &mut host, stop.notified()).await;
                let _ = updates.send(match result {
                    Ok(done) => Update::Ended { stopped: !done, error: None },
                    Err(error) => Update::Ended { stopped: false, error: Some(format!("{error:#}")) },
                });
                let _ = updates.send(Update::Info(info(&session)));
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

/// What a decision on the screen answers.
pub fn decision(choice: char) -> Decision {
    match choice {
        'y' => Decision::AllowOnce,
        'a' => Decision::AllowForTask,
        _ => Decision::Deny,
    }
}
