//! A command-line session: a task in Bubble's database (origin `cli`), so
//! it shows in the app and feeds memory like any task. Each process is one
//! run of it; `--resume` and `--session` continue a task's conversation.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, anyhow};
use bubble_core::delegate::{self, Children};
use bubble_core::hooks::{self, Hooks};
use bubble_core::prompt::{self, Mode};
use bubble_core::{AgentEvent, ChatMessage, Config, Level, ResolvedModel, Toolbox, Turn, bubble_home, http_client, run_turn};
use bubble_store::{MessageRole, NewRun, NewTask, Origin, RunRole, RunStatus, Store, Task};

use crate::args::Args;

/// What a front end does with a turn's events.
pub trait Host {
    /// Called for every event, approvals included (answer on their sender).
    fn event(&mut self, event: AgentEvent) -> impl std::future::Future<Output = ()>;
}

/// Which conversation a session continues.
pub enum Start {
    /// As the flags say: `--session`, `--resume`, or a new one (named by
    /// its first prompt).
    Flags,
    /// This task.
    Task(Task),
    /// A new one, whatever the flags say.
    Fresh,
}

/// The title of a session no prompt has named yet.
const UNTITLED: &str = "CLI session";

pub struct Session {
    pub store: Store,
    pub task: Task,
    pub run_id: String,
    pub history: Vec<ChatMessage>,
    pub model: ResolvedModel,
    pub thinking: Level,
    pub mode: Mode,
    pub cwd: PathBuf,
    pub config: Config,
    /// The model as the person named it (`provider:model`).
    pub reference: String,
    client: reqwest::Client,
    toolbox: Arc<Toolbox>,
    hooks: Arc<Hooks>,
    pub children: Arc<Children>,
    grants: Arc<Mutex<HashSet<String>>>,
    unlocked: Arc<Mutex<HashSet<String>>>,
    system: String,
}

const MAX_TOKENS: u32 = 32_000;

fn title_from(prompt: &str) -> String {
    let line = prompt.lines().find(|l| !l.trim().is_empty()).unwrap_or("CLI session").trim();
    if line.chars().count() > 60 { format!("{}…", line.chars().take(59).collect::<String>()) } else { line.to_owned() }
}

impl Session {
    /// Opens (or resumes) the session for `args`; `first_prompt` names a new one.
    pub async fn open(args: &Args, first_prompt: Option<&str>, store: Store) -> Result<Session> {
        Session::open_at(args, first_prompt, store, Start::Flags).await
    }

    /// Opens the session `start` says, with `args`' model and settings.
    pub async fn open_at(args: &Args, first_prompt: Option<&str>, store: Store, start: Start) -> Result<Session> {
        let cwd = args.cwd.canonicalize().with_context(|| format!("--cwd {} is not a directory", args.cwd.display()))?;
        if !cwd.is_dir() {
            return Err(anyhow!("--cwd {} is not a directory.", cwd.display()));
        }
        let config = Config::load()?;
        let reference = args
            .model
            .clone()
            .or_else(|| config.default_model_reference())
            .ok_or_else(|| anyhow!("No model is set: pass -m provider:model or set defaultModel in ~/.bubble/config.json"))?;
        let client = http_client()?;
        let model = config.resolve(&reference)?;
        if let Some(subscription) = model.subscription {
            // The service's model list says which request shape it takes.
            if let Err(error) = bubble_core::discover::discover(&client, subscription).await {
                log::warn!("model discovery: {error:#}");
            }
        }
        let thinking = args
            .thinking
            .or_else(|| config.default_thinking_level.as_deref().and_then(Level::parse))
            .unwrap_or(Level::High);
        let folder = cwd.display().to_string();
        let existing = match start {
            Start::Task(task) => Some(task),
            Start::Fresh => None,
            Start::Flags => match (&args.session, args.resume) {
                (Some(name), _) => store.cli_task_named(&folder, name)?,
                (None, true) => store.latest_cli_task(&folder)?,
                _ => None,
            },
        };
        let task = match existing {
            Some(task) => task,
            None => {
                let title = args.session.clone().unwrap_or_else(|| title_from(first_prompt.unwrap_or(UNTITLED)));
                store.create_task(NewTask { title: &title, project_cwd: Some(&folder), origin: Origin::Cli })?
            }
        };
        let mut messages = Vec::new();
        for run in store.runs_for_task(&task.id)? {
            messages.extend(store.messages(&run.id)?);
        }
        let history = bubble_store::history(&messages);
        let run = store.create_run(NewRun {
            task_id: Some(&task.id),
            role: RunRole::Task,
            model: Some(&reference),
            cwd: Some(&folder),
            status: RunStatus::Idle,
        })?;
        let hub = bubble_core::mcp::Hub::new();
        let toolbox = Arc::new(Toolbox::load(&hub, &client, &config, Some(&cwd)).await);
        let hooks = Arc::new(Hooks::load(&bubble_home(), Some(&cwd)));
        let children = Children::new();
        children.set_env(delegate::Env {
            client: client.clone(),
            config: config.clone(),
            model: model.clone(),
            thinking,
            cwd: cwd.clone(),
            toolbox: Some(toolbox.clone()),
            hooks: Some(hooks.clone()),
        });
        let grants: Arc<Mutex<HashSet<String>>> = Default::default();
        if args.mode == Mode::Build {
            // Build mode: file changes go ahead.
            grants.lock().map_err(|_| anyhow!("poisoned"))?.insert("files".into());
        }
        let system = compose(&model, &cwd, args.mode);
        Ok(Session {
            store,
            task,
            run_id: run.id,
            history,
            model,
            thinking,
            mode: args.mode,
            cwd,
            config,
            reference,
            client,
            toolbox,
            hooks,
            children,
            grants,
            unlocked: Default::default(),
            system,
        })
    }

    /// Switches to model `reference` (`provider:model`) from the next turn.
    pub async fn set_model(&mut self, reference: &str) -> Result<()> {
        self.config = Config::load().unwrap_or_else(|_| self.config.clone());
        let model = self.config.resolve(reference)?;
        if let Some(subscription) = model.subscription
            && let Err(error) = bubble_core::discover::discover(&self.client, subscription).await
        {
            log::warn!("model discovery: {error:#}");
        }
        self.model = model;
        self.reference = reference.to_owned();
        self.system = compose(&self.model, &self.cwd, self.mode);
        self.children.set_env(delegate::Env {
            client: self.client.clone(),
            config: self.config.clone(),
            model: self.model.clone(),
            thinking: self.thinking,
            cwd: self.cwd.clone(),
            toolbox: Some(self.toolbox.clone()),
            hooks: Some(self.hooks.clone()),
        });
        Ok(())
    }

    /// Plan, build or bypass from the next tool call.
    pub fn set_mode(&mut self, mode: Mode) {
        self.mode = mode;
        if let Ok(mut grants) = self.grants.lock() {
            if mode == Mode::Build {
                grants.insert("files".into());
            } else {
                grants.remove("files");
            }
        }
        self.system = compose(&self.model, &self.cwd, mode);
    }

    pub fn toolbox(&self) -> &Toolbox {
        &self.toolbox
    }

    /// Summarizes the older part of the conversation now (`/compact`); how
    /// many messages it replaced, `None` when there is too little to.
    pub async fn compact(&mut self, focus: Option<&str>) -> Result<Option<usize>> {
        let Some(next) = bubble_core::compact::compact_with(&self.client, &self.model, &self.history, focus).await? else {
            return Ok(None);
        };
        let replaced = self.history.len().saturating_sub(next.len());
        self.keep(&AgentEvent::Compacted(next))?;
        Ok(Some(replaced))
    }

    /// What fills the model's context: an estimate by part (`/context`).
    pub fn context(&self) -> String {
        use bubble_core::compact::{estimate_messages, estimate_text, estimate_tools};
        let unlocked = self.unlocked.lock().map(|u| u.clone()).unwrap_or_default();
        let mut tools = bubble_core::tools::specs();
        tools.extend(self.toolbox.specs(&unlocked));
        let system = estimate_text(&self.system);
        let tools = estimate_tools(&tools);
        let messages = estimate_messages(&self.history);
        let total = system + tools + messages;
        let window = bubble_core::model::info(&self.model.provider.id, &self.model.model).context_window;
        let share = |n: u64| match window {
            Some(w) if w > 0 => format!(" ({:.1}%)", n as f64 * 100.0 / w as f64),
            _ => String::new(),
        };
        let mut lines = vec![
            format!(
                "Context for {}: about {} tokens{}",
                self.model_label(),
                thousands(total),
                window.map(|w| format!(" of {}", thousands(w))).unwrap_or_default()
            ),
            format!("  System prompt   {:>9}{}", thousands(system), share(system)),
            format!("  Tools           {:>9}{}", thousands(tools), share(tools)),
            format!("  Messages ({:>3}) {:>9}{}", self.history.len(), thousands(messages), share(messages)),
        ];
        if let Some(w) = window
            && bubble_core::compact::should_compact(Some(w), total)
        {
            lines.push("Earlier messages will be summarized before the next model call.".into());
        }
        lines.join("\n")
    }

    pub fn client(&self) -> &reqwest::Client {
        &self.client
    }

    pub fn model_label(&self) -> String {
        format!("{}:{}", self.model.provider.id, self.model.model)
    }

    /// Remembers "allow for this session" answers.
    pub fn grant(&self, grant: String) {
        if let Ok(mut grants) = self.grants.lock() {
            grants.insert(grant);
        }
    }

    /// One turn: the prompt, the agent's work, everything stored. `stop`
    /// finishing stops it and keeps what came so far; whether it ran to
    /// its end.
    pub async fn turn<H: Host>(&mut self, prompt: &str, host: &mut H, stop: impl Future<Output = ()>) -> Result<bool> {
        if self.history.is_empty() && self.task.title == UNTITLED {
            let title = title_from(prompt);
            self.store.rename_task(&self.task.id, &title)?;
            self.task.title = title;
        }
        let message = ChatMessage::user(prompt.to_owned());
        self.store.append_message_with(
            &self.run_id,
            MessageRole::User,
            prompt,
            serde_json::to_string(&message).ok().as_deref(),
        )?;
        self.history.push(message);
        self.store.set_run_status(&self.run_id, RunStatus::Running, None)?;
        let turn = Turn {
            cwd: Some(self.cwd.clone()),
            grants: self.grants.clone(),
            max_tokens: MAX_TOKENS,
            thinking: self.thinking,
            toolbox: Some(self.toolbox.clone()),
            unlocked: self.unlocked.clone(),
            hooks: Some(self.hooks.clone()),
            role: hooks::Role::Parent,
            children: Some(self.children.clone()),
            ..Turn::new(self.model.clone(), Some(self.system.clone()), self.history.clone())
        };
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let client = self.client.clone();
        let job = tokio::spawn(async move { run_turn(&client, turn, sender).await });
        let mut stopped = false;
        let mut stop = std::pin::pin!(stop);
        loop {
            let event = tokio::select! {
                event = receiver.recv() => event,
                () = &mut stop, if !stopped => {
                    job.abort();
                    self.children.cancel_all();
                    stopped = true;
                    None
                }
            };
            let Some(event) = event else { break };
            self.keep(&event)?;
            host.event(event).await;
        }
        let result = if stopped {
            Ok(())
        } else {
            match job.await {
                Ok(result) => result,
                Err(error) => Err(anyhow!("the turn stopped: {error}")),
            }
        };
        match &result {
            Ok(()) => self.store.set_run_status(&self.run_id, RunStatus::Idle, None)?,
            Err(error) => self.store.set_run_status(&self.run_id, RunStatus::Failed, Some(&format!("{error:#}")))?,
        }
        result.map(|()| !stopped)
    }

    /// Stores what the model view needs, as the app does.
    fn keep(&mut self, event: &AgentEvent) -> Result<()> {
        match event {
            AgentEvent::Assistant(message) => {
                let text = match message {
                    ChatMessage::Assistant { text, .. } => text.clone(),
                    _ => String::new(),
                };
                self.store.append_message_with(
                    &self.run_id,
                    MessageRole::Assistant,
                    &text,
                    serde_json::to_string(message).ok().as_deref(),
                )?;
                self.history.push(message.clone());
            }
            AgentEvent::ToolResults(message) => {
                self.store.append_message_with(&self.run_id, MessageRole::Tool, "", serde_json::to_string(message).ok().as_deref())?;
                self.history.push(message.clone());
            }
            AgentEvent::Compacted(history) => {
                self.store.append_message_with(
                    &self.run_id,
                    MessageRole::Checkpoint,
                    "Earlier messages were summarized to stay within the model's context",
                    serde_json::to_string(history).ok().as_deref(),
                )?;
                self.history = history.clone();
            }
            _ => {}
        }
        Ok(())
    }
}

fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// The system prompt for `model` working in `cwd` in `mode`.
fn compose(model: &ResolvedModel, cwd: &std::path::Path, mode: Mode) -> String {
    let tool_names = [
        "read", "ls", "bash", "edit", "write", "grep", "web_search", "web_fetch", "spawn_agent", "wait_agent", "send_input",
        "skill_search", "skill",
    ];
    let date = bubble_core::time::now_utc().date;
    prompt::compose(model, cwd, &tool_names, true, mode, &date)
}

#[cfg(test)]
mod tests {
    #[test]
    fn titles_come_from_the_first_line() {
        assert_eq!(super::title_from("\n  Fix the build\nmore"), "Fix the build");
        assert_eq!(super::title_from(&"x".repeat(80)).chars().count(), 60);
    }
}
