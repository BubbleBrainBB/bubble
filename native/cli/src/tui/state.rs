//! What the screen shows, as signals: the conversation's lines, the turn
//! in progress, the question waiting for a yes, the open picker.

use std::rc::Rc;
use std::sync::Arc;

use bubble_core::auth::{AuthFile, Subscription};
use bubble_core::prompt::Mode;
use bubble_core::{AgentEvent, ChatMessage, Decision, Level};
use bubble_store::Task;
use nsh_core::prelude::*;
use tokio::sync::Notify;
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::oneshot;

use super::runner::{Command, Info, Update};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolState {
    Running,
    Done,
    Failed,
    Denied,
    Stopped,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    User,
    Assistant,
    Reasoning,
    Tool { state: ToolState, output: String },
    Notice,
    Error,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub id: u64,
    pub kind: Kind,
    pub text: String,
    /// Still streaming.
    pub live: bool,
    /// The tool call it shows.
    pub call: Option<String>,
    /// An edit's diff.
    pub diff: Vec<super::diff::DiffLine>,
}

/// A tool waiting for a yes.
#[derive(Clone, Debug, PartialEq)]
pub struct Ask {
    pub summary: String,
    pub detail: String,
    pub rememberable: bool,
}

/// What a picker picks.
#[derive(Clone, Debug, PartialEq)]
pub enum Pick {
    Model(String),
    Thinking(Level),
    Session(Task),
    Login(Subscription),
    Logout(Subscription),
    Skill(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub label: String,
    pub detail: String,
    pub pick: Pick,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Picker {
    pub title: String,
    pub items: Vec<Item>,
    pub selected: usize,
}

/// The slash commands, with what they do.
pub const COMMANDS: &[(&str, &str)] = &[
    ("/model", "Switch model: /model <provider:model>, or pick one"),
    ("/effort", "Reasoning effort: /effort <level>, or pick one"),
    ("/plan", "Toggle plan mode (Tab also switches Build/Plan)"),
    ("/session", "Browse this folder's sessions and resume one"),
    ("/compact", "Summarize the older conversation now; text after it says what to keep"),
    ("/context", "Show what fills the model's context"),
    ("/mcp", "MCP servers: /mcp, /mcp reconnect|login|logout <name>"),
    ("/skills", "Pick a skill to use"),
    ("/login", "Sign in to a subscription: /login openai or /login grok"),
    ("/logout", "Sign out of a subscription on this device"),
    ("/new", "Start a new session"),
    ("/clear", "Start a new session (alias of /new)"),
    ("/theme", "Colors for a light or dark terminal: /theme auto, light or dark"),
    ("/help", "Show commands and keys"),
    ("/quit", "Exit Bubble"),
];

pub const HELP: &str = "Keys: Enter sends · Shift+Enter, Alt+Enter or Ctrl+J adds a line · Up/Down recall earlier prompts · \
@ completes a file (its contents go with the prompt) · Ctrl+V pastes an image (or paste an image's path) · Esc stops the agent · Ctrl+C stops, clears, then exits · \
Tab completes or switches Build/Plan · Ctrl+O shows full tool output · PgUp/PgDn scroll · y/a/n answer a question";

#[derive(Clone, Copy)]
pub struct Tui {
    pub lines: Signal<Vec<Line>>,
    pub info: Signal<Option<Info>>,
    /// Turns sent and not yet ended.
    pub turns: Signal<usize>,
    pub ask: Signal<Option<Ask>>,
    pub picker: Signal<Option<Picker>>,
    pub draft: Signal<String>,
    /// Tool output and reasoning shown whole.
    pub expanded: Signal<bool>,
    /// Input tokens of the newest model call: the context in use.
    pub context: Signal<u64>,
    /// When the running turn started (ms).
    pub started: Signal<i64>,
    pub fatal: Signal<Option<String>>,
    /// What the work under way is called in the status line.
    pub activity: Signal<String>,
    /// The project's files, for `@` mentions.
    pub files: Signal<Vec<String>>,
    /// The prompts typed in this folder, oldest first.
    pub history: Signal<Vec<String>>,
    /// The entry of `history` the composer shows, while it shows one.
    pub recall: Signal<Option<usize>>,
    /// The skills a person can name (`/<skill> task`), with descriptions.
    pub skills: Signal<Vec<(String, String)>>,
    /// The MCP prompts a person can run (`/<prompt> args`).
    pub prompts: Signal<Vec<super::runner::PromptInfo>>,
    next_id: Stored<u64>,
    /// The reply streaming in, and its reasoning.
    reply: Stored<Option<u64>>,
    thought: Stored<Option<u64>>,
    respond: Stored<Option<oneshot::Sender<Decision>>>,
    commands: Stored<Option<UnboundedSender<Command>>>,
    stop: Stored<Arc<Notify>>,
    /// Called when the lines change, to keep the newest in view.
    on_change: Stored<Option<Rc<dyn Fn()>>>,
}

impl Tui {
    pub fn new(stop: Arc<Notify>) -> Tui {
        Tui {
            lines: signal(Vec::new()),
            info: signal(None),
            turns: signal(0),
            ask: signal(None),
            picker: signal(None),
            draft: signal(String::new()),
            expanded: signal(false),
            context: signal(0),
            started: signal(0),
            fatal: signal(None),
            activity: signal("Working".into()),
            files: signal(Vec::new()),
            history: signal(Vec::new()),
            recall: signal(None),
            skills: signal(Vec::new()),
            prompts: signal(Vec::new()),
            next_id: stored(0),
            reply: stored(None),
            thought: stored(None),
            respond: stored(None),
            commands: stored(None),
            stop: stored(stop),
            on_change: stored(None),
        }
    }

    pub fn connect(&self, commands: UnboundedSender<Command>) {
        self.commands.replace(Some(commands));
    }

    pub fn on_change(&self, f: impl Fn() + 'static) {
        self.on_change.replace(Some(Rc::new(f)));
    }

    fn changed(&self) {
        if let Some(f) = self.on_change.get() {
            f();
        }
    }

    pub fn busy(&self) -> bool {
        self.turns.get() > 0
    }

    pub fn send(&self, command: Command) {
        if let Some(commands) = self.commands.get() {
            let _ = commands.send(command);
        }
    }

    fn id(&self) -> u64 {
        let id = self.next_id.get() + 1;
        self.next_id.replace(id);
        id
    }

    pub fn push(&self, kind: Kind, text: impl Into<String>) -> u64 {
        let id = self.id();
        let line = Line { id, kind, text: text.into(), live: false, call: None, diff: Vec::new() };
        self.lines.update(|lines| lines.push(line));
        self.changed();
        id
    }

    fn update_line(&self, id: u64, f: impl FnOnce(&mut Line)) {
        self.lines.update(|lines| {
            if let Some(line) = lines.iter_mut().find(|l| l.id == id) {
                f(line);
            }
        });
    }

    fn remove_line(&self, id: u64) {
        self.lines.update(|lines| lines.retain(|l| l.id != id));
    }

    /// Sends what the person wrote: a command, or a prompt for the agent.
    pub fn submit(&self, text: String) {
        let text = text.trim().to_owned();
        if text.is_empty() {
            return;
        }
        if text.starts_with('/') {
            self.command(&text);
            return;
        }
        self.prompt(text.clone(), text);
    }

    /// Sends `sent` to the agent, showing `shown`.
    fn prompt(&self, shown: String, sent: String) {
        let text = shown;
        self.history.update(|h| {
            if h.last() != Some(&text) {
                h.push(text.clone());
            }
        });
        self.recall.set(None);
        self.push(Kind::User, text.clone());
        self.activity.set("Working".into());
        if self.turns.get_untracked() == 0 {
            self.started.set(bubble_store::now_ms());
        }
        self.turns.update(|t| *t += 1);
        self.send(Command::Prompt(sent));
    }

    /// Runs `command` as the agent's work: Esc stops it.
    fn work(&self, activity: &str, command: Command) {
        if self.busy() {
            self.push(Kind::Error, "Stop the agent first (Esc).");
            return;
        }
        self.activity.set(activity.into());
        self.started.set(bubble_store::now_ms());
        self.turns.update(|t| *t += 1);
        self.send(command);
    }

    fn command(&self, text: &str) {
        let (name, rest) = text.split_once(' ').map(|(n, r)| (n, r.trim())).unwrap_or((text, ""));
        let info = self.info.get_untracked();
        match name {
            "/quit" | "/exit" => bubble_term::exit(),
            "/help" | "/?" => {
                let commands: Vec<String> = COMMANDS.iter().map(|(c, d)| format!("{c:<9} {d}")).collect();
                self.push(Kind::Notice, format!("{}\n\n{HELP}", commands.join("\n")));
            }
            "/new" | "/clear" => {
                if self.busy() {
                    self.push(Kind::Error, "Stop the agent first (Esc).");
                    return;
                }
                self.send(Command::Fresh);
            }
            "/plan" => self.toggle_plan(),
            "/model" if !rest.is_empty() => self.send(Command::Model(rest.to_owned())),
            "/model" => {
                let Some(info) = info else { return };
                let selected = info.models.iter().position(|m| *m == info.model).unwrap_or(0);
                let items = info
                    .models
                    .iter()
                    .map(|m| Item {
                        label: m.clone(),
                        detail: if *m == info.model { "current".into() } else { String::new() },
                        pick: Pick::Model(m.clone()),
                    })
                    .collect();
                self.open(Picker { title: "Switch model".into(), items, selected });
            }
            "/effort" | "/reasoning" if !rest.is_empty() => match Level::parse(rest) {
                Some(level) => self.send(Command::Thinking(level)),
                None => {
                    self.push(Kind::Error, format!("Unknown effort {rest}: off, minimal, low, medium, high, xhigh, max or ultra"));
                }
            },
            "/effort" | "/reasoning" => {
                let Some(info) = info else { return };
                let selected = info.levels.iter().position(|l| *l == info.thinking).unwrap_or(0);
                let items = info
                    .levels
                    .iter()
                    .map(|l| Item {
                        label: l.as_str().to_owned(),
                        detail: if *l == info.thinking { "current".into() } else { String::new() },
                        pick: Pick::Thinking(*l),
                    })
                    .collect();
                self.open(Picker { title: "Reasoning effort".into(), items, selected });
            }
            "/login" | "/logout" => {
                let login = name == "/login";
                let subscription = match rest.to_ascii_lowercase().as_str() {
                    "" => None,
                    "openai" | "chatgpt" | "codex" => Some(Subscription::ChatGpt),
                    "grok" | "xai" => Some(Subscription::Grok),
                    other => {
                        self.push(Kind::Error, format!("Unknown subscription {other}: openai or grok"));
                        return;
                    }
                };
                match (subscription, login) {
                    (Some(s), true) => self.login(s),
                    (Some(s), false) => self.send(Command::Logout(s)),
                    (None, _) => {
                        let auth = AuthFile::default();
                        let items = [Subscription::ChatGpt, Subscription::Grok]
                            .into_iter()
                            .map(|s| Item {
                                label: format!("{} ({})", s.label(), if s == Subscription::ChatGpt { "openai" } else { "grok" }),
                                detail: if auth.signed_in(s) { "signed in".into() } else { String::new() },
                                pick: if login { Pick::Login(s) } else { Pick::Logout(s) },
                            })
                            .collect();
                        let title = if login { "Sign in to" } else { "Sign out of" };
                        self.open(Picker { title: title.into(), items, selected: 0 });
                    }
                }
            }
            "/session" | "/sessions" | "/resume" => {
                if self.busy() {
                    self.push(Kind::Error, "Stop the agent first (Esc).");
                    return;
                }
                self.send(Command::Sessions);
            }
            "/compact" => self.work("Summarizing", Command::Compact((!rest.is_empty()).then(|| rest.to_owned()))),
            "/context" => self.send(Command::Context),
            "/theme" => {
                let appearance = match rest {
                    "light" => Appearance::Light,
                    "dark" => Appearance::Dark,
                    "auto" | "" => Appearance::System,
                    other => {
                        self.push(Kind::Error, format!("Unknown theme {other}: auto, light or dark"));
                        return;
                    }
                };
                set_color_scheme(appearance);
                let mode = if rest.is_empty() { "auto" } else { rest };
                match super::save_theme(mode) {
                    Ok(()) => self.push(Kind::Notice, format!("Theme: {mode} (kept in config.json)")),
                    Err(error) => self.push(Kind::Error, format!("Theme {mode} is on, but config.json could not be updated: {error}")),
                };
            }
            "/mcp" => match rest.split_once(' ').map(|(a, b)| (a, b.trim())).unwrap_or((rest, "")) {
                ("", _) | ("list", _) => self.send(Command::Mcp(None)),
                ("reconnect", server) if !server.is_empty() => self.send(Command::Mcp(Some(server.to_owned()))),
                ("login", server) if !server.is_empty() => self.work("Waiting for the sign-in", Command::McpLogin(server.to_owned())),
                ("logout", server) if !server.is_empty() => self.send(Command::McpLogout(server.to_owned())),
                _ => {
                    self.push(Kind::Error, "Usage: /mcp [list | reconnect <name> | login <name> | logout <name>]");
                }
            },
            "/skills" => {
                let items: Vec<Item> = self
                    .skills
                    .get_untracked()
                    .into_iter()
                    .map(|(name, description)| Item { label: name.clone(), detail: description, pick: Pick::Skill(name) })
                    .collect();
                if items.is_empty() {
                    self.push(Kind::Notice, "No skills found (~/.bubble/skills, ~/.agents/skills, ~/.claude/skills, .bubble/skills).");
                } else {
                    self.open(Picker { title: "Use a skill: pick one, then say the task".into(), items, selected: 0 });
                }
            }
            _ => {
                // `/<prompt> args`: an MCP server's prompt.
                let bare = name.trim_start_matches('/');
                if let Some(prompt) = self.prompts.with_untracked(|p| p.iter().find(|p| p.name == bare).cloned()) {
                    self.push(Kind::User, text.to_owned());
                    self.activity.set("Working".into());
                    if self.turns.get_untracked() == 0 {
                        self.started.set(bubble_store::now_ms());
                    }
                    self.turns.update(|t| *t += 1);
                    self.send(Command::McpPrompt { server: prompt.server, name: prompt.name, args: rest.to_owned() });
                    return;
                }
                // `/<skill> task`: the agent loads that skill for the task.
                let skill = name.trim_start_matches('/');
                if !rest.is_empty() && self.skills.with_untracked(|s| s.iter().any(|(n, _)| n == skill)) {
                    let sent = format!(
                        "Use the skill tool to load the \"{skill}\" skill before responding.\nThe task clearly matches that skill.\n\
                         Do not simply restate the skill contents; use the loaded instructions to complete the task.\n\nUser request:\n{rest}"
                    );
                    self.prompt(text.to_owned(), sent);
                    return;
                }
                self.push(Kind::Error, format!("Unknown command {name}. /help lists them."));
            }
        }
    }

    /// Signs in; the wait counts as work Esc can stop.
    fn login(&self, subscription: Subscription) {
        self.work("Waiting for the sign-in", Command::Login(subscription));
    }

    pub fn toggle_plan(&self) {
        let Some(info) = self.info.get_untracked() else { return };
        let mode = if info.mode == Mode::Plan { Mode::Build } else { Mode::Plan };
        self.send(Command::Mode(mode));
    }

    pub fn open(&self, picker: Picker) {
        if !picker.items.is_empty() {
            self.picker.set(Some(picker));
        }
    }

    pub fn move_selection(&self, by: i32) {
        self.picker.update(|picker| {
            if let Some(picker) = picker {
                let count = picker.items.len() as i32;
                picker.selected = ((picker.selected as i32 + by).rem_euclid(count.max(1))) as usize;
            }
        });
    }

    /// The picker's selected item, chosen.
    pub fn choose(&self) {
        let Some(picker) = self.picker.get_untracked() else { return };
        self.picker.set(None);
        let Some(item) = picker.items.get(picker.selected) else { return };
        match &item.pick {
            Pick::Model(model) => self.send(Command::Model(model.clone())),
            Pick::Thinking(level) => self.send(Command::Thinking(*level)),
            Pick::Session(task) => self.send(Command::Resume(task.clone())),
            Pick::Login(subscription) => self.login(*subscription),
            Pick::Logout(subscription) => self.send(Command::Logout(*subscription)),
            Pick::Skill(name) => self.draft.set(format!("/{name} ")),
        }
    }

    /// The answer to the question waiting: `y`, `a` or `n`.
    pub fn answer(&self, choice: char) {
        let Some(ask) = self.ask.get_untracked() else { return };
        if choice == 'a' && !ask.rememberable {
            return;
        }
        self.ask.set(None);
        if let Some(respond) = self.respond.replace(None) {
            let _ = respond.send(super::runner::decision(choice));
        }
    }

    pub fn stop(&self) {
        if self.busy() {
            // A question waiting is answered no.
            self.answer('n');
            self.stop.with(|stop| stop.notify_waiters());
        }
    }

    /// The conversation so far, as lines.
    fn show_history(&self, history: &[ChatMessage]) {
        let mut lines = Vec::new();
        for message in history {
            match message {
                ChatMessage::User { text, .. } => {
                    lines.push(Line { id: self.id(), kind: Kind::User, text: text.clone(), live: false, call: None, diff: Vec::new() })
                }
                ChatMessage::Assistant { text, reasoning, tool_calls, .. } => {
                    if !reasoning.trim().is_empty() {
                        lines.push(Line { id: self.id(), kind: Kind::Reasoning, text: reasoning.clone(), live: false, call: None, diff: Vec::new() });
                    }
                    if !text.trim().is_empty() {
                        lines.push(Line { id: self.id(), kind: Kind::Assistant, text: text.clone(), live: false, call: None, diff: Vec::new() });
                    }
                    for call in tool_calls {
                        lines.push(Line {
                            id: self.id(),
                            kind: Kind::Tool { state: ToolState::Done, output: String::new() },
                            text: bubble_core::tools::describe(call),
                            live: false,
                            call: Some(call.id.clone()),
                            diff: super::diff::of_call(call),
                        });
                    }
                }
                ChatMessage::Tool { results } => {
                    for result in results {
                        if let Some(line) = lines.iter_mut().find(|l| l.call.as_deref() == Some(&result.call_id)) {
                            line.kind = Kind::Tool {
                                state: if result.is_error { ToolState::Failed } else { ToolState::Done },
                                output: result.content.clone(),
                            };
                        }
                    }
                }
            }
        }
        self.lines.set(lines);
    }

    /// Shows what the runner reports.
    pub fn apply(&self, update: Update) {
        match update {
            Update::Opened(info, history) => {
                let resumed = !history.is_empty();
                let title = info.title.clone();
                self.info.set(Some(info));
                self.show_history(&history);
                self.context.set(0);
                if resumed {
                    self.push(Kind::Notice, format!("Resumed \"{title}\""));
                }
            }
            Update::Info(info) => self.info.set(Some(info)),
            Update::Notice(text) => {
                self.push(Kind::Notice, text);
            }
            Update::Error(text) => {
                if self.info.get_untracked().is_none() {
                    self.fatal.set(Some(text.clone()));
                }
                self.push(Kind::Error, text);
            }
            Update::Sessions(tasks) => {
                let current = self.info.get_untracked().map(|i| i.title).unwrap_or_default();
                let items = tasks
                    .into_iter()
                    .map(|task| Item {
                        label: task.title.clone(),
                        detail: super::view::age(task.updated_at),
                        pick: Pick::Session(task),
                    })
                    .collect::<Vec<_>>();
                if items.is_empty() {
                    self.push(Kind::Notice, "No sessions in this folder yet.");
                    return;
                }
                let selected = items.iter().position(|i| i.label == current).unwrap_or(0);
                self.open(Picker { title: "Resume a session".into(), items, selected });
            }
            Update::Pasted(label) => {
                self.draft.update(|draft| {
                    if !draft.is_empty() && !draft.ends_with(char::is_whitespace) {
                        draft.push(' ');
                    }
                    draft.push_str(&label);
                    draft.push(' ');
                });
            }
            Update::Helpers { files, history, skills, prompts } => {
                self.files.set(files);
                self.skills.set(skills);
                self.prompts.set(prompts);
                if !history.is_empty() {
                    self.history.set(history);
                }
            }
            Update::Ended { stopped, error } => {
                self.finish_reply();
                if stopped {
                    self.lines.update(|lines| {
                        for line in lines.iter_mut() {
                            if let Kind::Tool { state: state @ ToolState::Running, .. } = &mut line.kind {
                                *state = ToolState::Stopped;
                            }
                        }
                    });
                    self.push(Kind::Notice, "Stopped");
                }
                if let Some(error) = error {
                    self.push(Kind::Error, error);
                }
                self.ask.set(None);
                self.respond.replace(None);
                self.turns.update(|t| *t = t.saturating_sub(1));
                if self.turns.get_untracked() > 0 {
                    self.started.set(bubble_store::now_ms());
                }
                self.changed();
            }
            Update::Agent(event) => self.agent(event),
        }
    }

    fn finish_reply(&self) {
        for id in [self.thought.replace(None), self.reply.replace(None)].into_iter().flatten() {
            let empty = self.lines.with_untracked(|lines| lines.iter().any(|l| l.id == id && l.text.trim().is_empty()));
            if empty {
                self.remove_line(id);
            } else {
                self.update_line(id, |line| line.live = false);
            }
        }
    }

    fn agent(&self, event: AgentEvent) {
        match event {
            AgentEvent::Reasoning(piece) => {
                let id = match self.thought.get() {
                    Some(id) => id,
                    None => {
                        let id = self.push(Kind::Reasoning, "");
                        self.update_line(id, |l| l.live = true);
                        self.thought.replace(Some(id));
                        id
                    }
                };
                self.update_line(id, |line| line.text.push_str(&piece));
                self.changed();
            }
            AgentEvent::Text(piece) => {
                if let Some(id) = self.thought.get() {
                    self.update_line(id, |l| l.live = false);
                }
                let id = match self.reply.get() {
                    Some(id) => id,
                    None => {
                        let id = self.push(Kind::Assistant, "");
                        self.update_line(id, |l| l.live = true);
                        self.reply.replace(Some(id));
                        id
                    }
                };
                self.update_line(id, |line| line.text.push_str(&piece));
                self.changed();
            }
            AgentEvent::Assistant(ChatMessage::Assistant { text, .. }) => {
                if let Some(id) = self.reply.get() {
                    self.update_line(id, move |line| line.text = text);
                }
                self.finish_reply();
                self.changed();
            }
            AgentEvent::ToolStarted { call, description } => {
                self.finish_reply();
                let id = self.push(Kind::Tool { state: ToolState::Running, output: String::new() }, description);
                let diff = super::diff::of_call(&call);
                self.update_line(id, |line| {
                    line.call = Some(call.id.clone());
                    line.diff = diff;
                });
            }
            AgentEvent::ApprovalNeeded { permission, respond, .. } => {
                self.respond.replace(Some(respond));
                self.ask.set(Some(Ask {
                    summary: permission.summary,
                    detail: permission.detail,
                    rememberable: permission.rememberable,
                }));
                self.changed();
            }
            AgentEvent::ToolFinished { call_id, output, denied } => {
                self.lines.update(|lines| {
                    if let Some(line) = lines.iter_mut().find(|l| l.call.as_deref() == Some(&call_id)) {
                        let state = if denied {
                            ToolState::Denied
                        } else if output.is_error {
                            ToolState::Failed
                        } else {
                            ToolState::Done
                        };
                        line.kind = Kind::Tool { state, output: output.content };
                    }
                });
                self.changed();
            }
            AgentEvent::Compacted(_) => {
                self.push(Kind::Notice, "Earlier messages were summarized to stay within the model's context");
            }
            AgentEvent::Usage(usage) => self.context.set(usage.input_tokens + usage.output_tokens),
            _ => {}
        }
    }
}
