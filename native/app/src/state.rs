//! The app's state: the store, the configuration, where the window is, and
//! the conversations (the Manager's on Home, each task's on its page).
//!
//! Conversations and the agent turns they run belong to the app's root,
//! not to a screen: leaving a task's page does not stop its agent. Task
//! turns beyond the "running at once" limit wait in a queue.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use anyhow::anyhow;
use bubble_core::delegate::{self, Children};
use bubble_core::hooks::Hooks;
use bubble_core::{
    AgentEvent, ChatMessage, Config, Decision, Level as ThinkingLevel, Permission, bubble_home,
};
use bubble_github::Gh;
use bubble_store::{
    Lifecycle, Message, MessageRole, NewRun, NewTask, Origin, Run, RunRole, RunStatus, Store, TaskState, TaskSummary,
};
use nsh_core::prelude::*;
use tokio::sync::oneshot;

use crate::engine::{self, Ending, HostTurn, Hosts, TurnEvent};
use crate::github::{self, Level, Repo};
use crate::manager::{self, Incoming, Sender};

const MANAGER_KEY: &str = "manager";

/// Settings keys in the store.
pub const MANAGER_MODEL: &str = "manager_model";
pub const TASK_MODEL: &str = "task_model";
pub const MAX_RUNS: &str = "max_concurrent_runs";
pub const DEFAULT_MAX_RUNS: u32 = 4;
pub const THINKING: &str = "thinking_level";
/// "system", "light" or "dark".
pub const APPEARANCE: &str = "appearance";
/// The bot whose conversation was open when the app closed.
const OPEN_BOT: &str = "open_bot";
/// When each bot's conversation was last looked at, as JSON.
const BOTS_SEEN: &str = "bots_seen";

/// Which bot a message in a conversation came from, by the message's id.
fn from_key(message_id: &str) -> String {
    format!("from:{message_id}")
}

/// The shape and color a person picked for a bot, as "shape:color".
fn bot_face_key(bot_id: &str) -> String {
    format!("bot_face:{bot_id}")
}

/// A bot's role, in a word or two: what it is on the team ("Developer").
fn bot_role_key(bot_id: &str) -> String {
    format!("bot_role:{bot_id}")
}

/// The project folders the bots have worked in, newest first, as JSON.
const PROJECTS: &str = "projects";
/// How many bots of one role there may be before new work waits its turn.
pub const MAX_ROLE_BOTS: &str = "max_role_bots";
pub const DEFAULT_MAX_ROLE_BOTS: u32 = 3;

fn bot_instruction_key(bot_id: &str) -> String {
    format!("bot_instruction:{bot_id}")
}
/// A task's own model and effort, over the Settings ones.
fn task_model_key(task_id: &str) -> String {
    format!("task_model:{task_id}")
}
fn task_thinking_key(task_id: &str) -> String {
    format!("task_thinking:{task_id}")
}
/// "full" when the task's runs the person starts need no approvals.
fn task_access_key(task_id: &str) -> String {
    format!("task_access:{task_id}")
}
/// Why a run began, when the person chose it (a rerun).
/// The subagents a task's agent started, as the agent reported them.
fn subagents_key(task_id: &str) -> String {
    format!("subagents:{task_id}")
}

fn run_cause_key(run_id: &str) -> String {
    format!("run_cause:{run_id}")
}
/// An MCP server as Settings shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct ServerRow {
    pub name: String,
    pub scope: &'static str,
    pub status: String,
    pub failed: bool,
}

/// What the agent said it can run.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Catalog {
    /// The agent answered; before that nothing is known to be missing.
    loaded: bool,
    providers: HashSet<String>,
    /// "provider:model" to the levels it can think at.
    levels: HashMap<String, Vec<ThinkingLevel>>,
}

fn server_row(server: &bubble_host::McpServer) -> ServerRow {
    let (status, failed) = match &server.state {
        bubble_host::McpState::Connected { tools, .. } => (format!("{tools} tool{}", if *tools == 1 { "" } else { "s" }), false),
        bubble_host::McpState::Failed(error) => (error.lines().next().unwrap_or_default().to_owned(), true),
        bubble_host::McpState::Disabled => ("Turned off".to_owned(), false),
    };
    let scope = match server.scope.as_str() {
        "project" => "project",
        "local" => "local",
        _ => "user",
    };
    ServerRow { name: server.name.clone(), scope, status, failed }
}

/// What Settings says about skills.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SkillsSummary {
    pub count: usize,
    pub folders: Vec<String>,
    pub problems: usize,
    /// What was wrong with the first one that could not be used.
    pub first_problem: Option<String>,
}

/// Where the window is.
#[derive(Clone, Debug, PartialEq)]
pub enum Route {
    Home,
    Tasks,
    /// A bot's conversation.
    Task(String),
    /// The form that makes a bot.
    NewBot,
    PullRequests,
    Skills,
    Search,
    Settings,
}

pub use crate::transcript::{Line, LineKind, ToolStatus, TurnStatus};
use crate::transcript;

/// A skill as the Skills page lists it.
#[derive(Clone, Debug, PartialEq)]
pub struct SkillRow {
    pub name: String,
    pub description: String,
    pub source: &'static str,
    pub folder: String,
    pub manual_only: bool,
}

/// A subscription sign-in as Settings shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct Account {
    pub subscription: bubble_core::auth::Subscription,
    pub signed_in: bool,
    pub status: String,
}

/// Everything the screens share; `Copy`, handed out through context.
#[derive(Clone, Copy)]
pub struct AppState {
    store: Stored<Option<Store>>,
    pub(crate) config: Stored<Config>,
    /// The app's root, which conversations and running turns belong to.
    root: Owner,
    conversations: Stored<HashMap<String, Conversation>>,
    /// Tasks waiting for a free slot, first come first served.
    queue: Stored<VecDeque<String>>,
    gh: Stored<Option<Gh>>,
    /// The GitHub repository of each project folder, once looked up.
    repo_cache: Stored<HashMap<String, Option<String>>>,
    /// Bumped when a repository's level changes.
    repo_levels: Signal<u32>,
    /// Bumped when a task's own model or effort, or its links, change.
    pub task_revision: Signal<u32>,
    /// Who GitHub knows the person as, or why it cannot be reached.
    pub github_status: Signal<Option<String>>,
    /// The subscriptions signed in to, and what their services offer.
    /// ChatGPT and Grok: how each sign-in stands.
    pub accounts: Signal<Vec<Account>>,
    /// Bubble's agent: the host process every turn runs on.
    pub(crate) hosts: Stored<Arc<Hosts>>,
    /// The models the agent can run, once it has said.
    pub catalog: Signal<Arc<Catalog>>,
    pub mcp_servers: Signal<Vec<ServerRow>>,
    pub skills: Signal<SkillsSummary>,
    /// Where persistent memory stands.
    pub memory_status: Signal<String>,
    /// The person's hooks (settings.json), as Settings shows them.
    pub hook_rules: Signal<Vec<String>>,
    pub route: Signal<Route>,
    pub tasks: Signal<Vec<TaskSummary>>,
    /// The newest thing said in each bot's conversation.
    pub previews: Signal<HashMap<String, bubble_store::Preview>>,
    /// Work brought to a bot that waits for the person's yes.
    pub proposed: Signal<Vec<bubble_store::Ticket>>,
    /// Bumped when a ticket is made or moves on.
    pub tickets_revision: Signal<u32>,
    /// Bumped when a bot's or the team's notes change.
    pub memory_revision: Signal<u32>,
    /// The open bot's details show beside its conversation.
    pub details_open: Signal<bool>,
    /// When the person last looked at each bot's conversation.
    seen: Signal<HashMap<String, i64>>,
    /// GitHub events since the person last wrote to the Manager (Home's
    /// "while you were away").
    pub away: Signal<Vec<bubble_store::Event>>,
    /// The task page's "link an issue or PR" row is open.
    pub linking: Signal<bool>,
    /// The task page's Changes panel is open.
    pub changes_open: Signal<bool>,
    /// Light, dark, or as the system is.
    pub appearance: Signal<nsh_core::platform::Appearance>,
    /// Prompts that start tasks on a schedule.
    pub automations: Signal<Vec<crate::automations::Automation>>,
    /// The person's skills, for the Skills page.
    pub skill_list: Signal<Vec<SkillRow>>,
    pub manager_model: Signal<Option<String>>,
    pub task_model: Signal<Option<String>>,
    pub max_runs: Signal<u32>,
    /// How hard models think (config.json `defaultThinkingLevel`).
    pub thinking: Signal<ThinkingLevel>,
    /// The models a person can pick: the default and the recent ones that
    /// resolve against config.json.
    pub models: Signal<Vec<String>>,
    /// Why the app cannot work (no database, an unreadable config).
    pub problem: Signal<Option<String>>,
}

impl AppState {
    /// Made by the root component, whose owner it keeps.
    pub fn load() -> Self {
        let mut problems = Vec::new();
        seed_development_home();
        let config = Config::load().unwrap_or_else(|error| {
            problems.push(format!("config.json: {error:#}"));
            Config::default()
        });
        let path = bubble_home().join("native").join("bubble.db");
        let store = match Store::open(&path) {
            Ok(store) => {
                if let Err(error) = store.recover_interrupted_runs() {
                    log::warn!("could not mark interrupted runs: {error:#}");
                }
                // A bot that was working when the app closed never answered.
                if let Err(error) = store.close_unanswered_tickets() {
                    log::warn!("could not close unanswered tickets: {error:#}");
                }
                Some(store)
            }
            Err(error) => {
                problems.push(format!("{}: {error:#}", path.display()));
                None
            }
        };
        if let Some(store) = &store {
            seed_development_preferences(store, &config);
        }
        let setting = |key: &str| store.as_ref().and_then(|s| s.setting(key).ok().flatten());
        let max_runs = setting(MAX_RUNS).and_then(|v| v.parse().ok()).unwrap_or(DEFAULT_MAX_RUNS);
        let state = AppState {
            models: signal(config.selectable_models()),
            manager_model: signal(setting(MANAGER_MODEL)),
            task_model: signal(setting(TASK_MODEL)),
            max_runs: signal(max_runs),
            thinking: signal(
                setting(THINKING)
                    .or_else(|| config.default_thinking_level.clone())
                    .as_deref()
                    .and_then(ThinkingLevel::parse)
                    .unwrap_or(ThinkingLevel::High),
            ),
            accounts: signal(Vec::new()),
            hosts: stored(Arc::new(Hosts::default())),
            catalog: signal(Arc::new(Catalog::default())),
            mcp_servers: signal(Vec::new()),
            skills: signal(SkillsSummary::default()),
            memory_status: signal(String::new()),
            hook_rules: signal(Vec::new()),
            problem: signal((!problems.is_empty()).then(|| problems.join("\n"))),
            // The conversation that was open comes back, as a messenger's does.
            route: signal(match setting(OPEN_BOT) {
                Some(id) if store.as_ref().is_some_and(|s| s.get_task(&id).ok().flatten().is_some_and(|t| t.lifecycle == Lifecycle::Open)) => Route::Task(id),
                _ => Route::Home,
            }),
            tasks: signal(Vec::new()),
            previews: signal(HashMap::new()),
            proposed: signal(Vec::new()),
            tickets_revision: signal(0),
            memory_revision: signal(0),
            details_open: signal(false),
            seen: signal(setting(BOTS_SEEN).and_then(|json| serde_json::from_str(&json).ok()).unwrap_or_default()),
            away: signal(Vec::new()),
            linking: signal(false),
            changes_open: signal(false),
            automations: signal(crate::automations::load(setting(crate::automations::KEY).as_deref())),
            skill_list: signal(Vec::new()),
            appearance: signal(appearance_of(setting(APPEARANCE).as_deref())),
            conversations: stored(HashMap::new()),
            queue: stored(VecDeque::new()),
            gh: stored(Gh::locate()),
            repo_cache: stored(HashMap::new()),
            repo_levels: signal(0),
            task_revision: signal(0),
            github_status: signal(None),
            root: Owner::current(),
            store: stored(store),
            config: stored(config),
        };
        state.refresh_tasks();
        state.refresh_tickets();
        state.refresh_away();
        // Which conversation is open is kept for the next start.
        effect(move || {
            let open = match state.route.get() {
                Route::Task(id) => Some(id),
                Route::Home => None,
                // A page is not a place to come back to.
                _ => return,
            };
            untracked(|| state.with_store(|s| s.set_setting(OPEN_BOT, open.as_deref())));
        });
        state.resume_queue();
        github::start(state);
        crate::automations::start(state);
        state.refresh_catalog();
        state.show_hooks_and_memory();
        state.import_sessions();
        state
    }

    /// The TypeScript CLI's sessions, brought in once in the background.
    fn import_sessions(&self) {
        let done = self.with_store(|s| s.setting(bubble_store::import::DONE)).flatten().is_some();
        if done {
            return;
        }
        let app = *self;
        spawn(async move {
            let imported = background(async move {
                tokio::task::spawn_blocking(|| {
                    let store = bubble_store::Store::open(bubble_home().join("native").join("bubble.db"))?;
                    store.import_ts_sessions(&bubble_home().join("sessions"))
                })
                .await
                .map_err(|e| anyhow!("{e}"))?
            })
            .await;
            match imported {
                Ok(Ok(report)) if report.imported > 0 => app.refresh_tasks(),
                Ok(Err(error)) => log::warn!("importing sessions: {error:#}"),
                _ => {}
            }
        });
    }

    /// Finds the person's skills and connects their own MCP servers, so
    /// Settings shows what works before a task needs it.
    /// What Settings says about hooks and memory. Both are the agent's own
    /// work (it runs the hooks and keeps the memory, from the conversations
    /// in its session files); the app only shows where they stand.
    fn show_hooks_and_memory(&self) {
        let hooks = Hooks::load(&bubble_home(), None);
        self.hook_rules.set(
            hooks
                .rules
                .iter()
                .map(|r| format!("{}: {}{}", r.events.join(", "), r.command, r.matcher.as_ref().map(|m| format!(" (matching {m})")).unwrap_or_default()))
                .collect(),
        );
        let updated = std::fs::metadata(bubble_core::memory::root().join("memory_summary.md"))
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| bubble_core::time::from_millis(d.as_millis()).date);
        self.memory_status.set(match updated {
            Some(date) => format!("Summary updated {date}"),
            None => "No memory yet".to_owned(),
        });
    }

    /// Asks the agent what it offers (models and accounts, skills, MCP
    /// servers) and shows it. The first call also starts the agent.
    pub fn refresh_catalog(&self) {
        self.show_accounts_from_disk();
        self.load_models(false);
        self.load_skills();
        self.refresh_servers();
    }

    /// The agent could not be reached; said where the app's problems show.
    fn agent_problem(&self, error: &str) {
        let message = format!("Bubble's agent could not start: {error}");
        log::warn!("{message}");
        self.problem.update(|problem| match problem {
            Some(existing) if existing.contains(&message) => {}
            Some(existing) => existing.push_str(&format!("\n{message}")),
            None => *problem = Some(message),
        });
    }

    /// What auth.json says, until the agent answers.
    fn show_accounts_from_disk(&self) {
        use bubble_core::auth::{AuthFile, Subscription};
        let auth = AuthFile::default();
        self.accounts.set(
            [Subscription::ChatGpt, Subscription::Grok]
                .into_iter()
                .map(|subscription| {
                    let signed_in = auth.signed_in(subscription);
                    Account {
                        subscription,
                        signed_in,
                        status: if signed_in { "Signed in; looking up its models…" } else { "Not signed in" }.to_owned(),
                    }
                })
                .collect(),
        );
    }

    /// The models the agent can run and the subscriptions signed in to.
    fn load_models(&self, refresh: bool) {
        let app = *self;
        let hosts = self.hosts.get();
        spawn(async move {
            let found = background(async move {
                let host = hosts.get().await?;
                Ok::<_, anyhow::Error>((host.models(refresh).await?, host.accounts().await?))
            })
            .await;
            match found {
                Ok(Ok((models, accounts))) => app.show_models(models, accounts),
                Ok(Err(error)) => app.agent_problem(&format!("{error:#}")),
                Err(error) => app.agent_problem(&error.to_string()),
            }
        });
    }

    fn show_models(&self, models: bubble_host::Models, accounts: Vec<bubble_host::Account>) {
        use bubble_core::auth::Subscription;
        let mut catalog = Catalog { loaded: true, ..Catalog::default() };
        let mut subscription_models: Vec<String> = Vec::new();
        for provider in &models.providers {
            catalog.providers.insert(provider.id.clone());
            for model in &provider.models {
                let levels: Vec<ThinkingLevel> = model.thinking_levels.iter().filter_map(|l| ThinkingLevel::parse(l)).collect();
                catalog.levels.insert(model.reference.clone(), if levels.is_empty() { vec![ThinkingLevel::Off] } else { levels });
                if provider.oauth {
                    subscription_models.push(model.reference.clone());
                }
            }
        }
        // The default, the recent ones the agent still knows, then what the
        // subscriptions offer.
        let (default, recent) = self.config.with(|c| (c.default_model_reference(), c.recent_models.clone()));
        let default = Some(models.default_model.clone()).filter(|m| m.contains(':')).or(default);
        let mut offered: Vec<String> = Vec::new();
        for reference in default.into_iter().chain(recent).chain(subscription_models) {
            let known = catalog.levels.contains_key(&reference)
                || reference.split_once(':').is_some_and(|(provider, _)| catalog.providers.contains(provider));
            if known && !offered.contains(&reference) {
                offered.push(reference);
            }
        }
        self.accounts.set(
            accounts
                .into_iter()
                .filter_map(|account| {
                    let subscription = Subscription::from_key(&account.provider_id)?;
                    let count = models.providers.iter().find(|p| p.id == account.provider_id).map_or(0, |p| p.models.len());
                    Some(Account {
                        subscription,
                        signed_in: account.signed_in,
                        status: if account.signed_in { format!("Signed in; {count} models") } else { "Not signed in".to_owned() },
                    })
                })
                .collect(),
        );
        self.models.set(offered);
        self.catalog.set(Arc::new(catalog));
    }

    fn set_account_status(&self, subscription: bubble_core::auth::Subscription, status: String) {
        self.accounts.update(|accounts| {
            if let Some(account) = accounts.iter_mut().find(|a| a.subscription == subscription) {
                account.status = status;
            }
        });
    }

    /// Signs in to `subscription` in the browser (the Grok CLI's sign-in is
    /// taken when there is one). The agent does it, and keeps the tokens.
    pub fn sign_in(&self, subscription: bubble_core::auth::Subscription) {
        let app = *self;
        let hosts = self.hosts.get();
        app.set_account_status(subscription, "Finish signing in in your browser…".into());
        spawn(async move {
            let done = background(async move {
                let host = hosts.get().await?;
                let (progress, _unread) = tokio::sync::mpsc::unbounded_channel();
                host.sign_in(subscription.key(), progress).await
            })
            .await;
            match done {
                Ok(Ok(())) => app.load_models(true),
                Ok(Err(error)) => app.set_account_status(subscription, format!("{error:#}")),
                Err(error) => app.set_account_status(subscription, format!("{error}")),
            }
        });
    }

    pub fn sign_out(&self, subscription: bubble_core::auth::Subscription) {
        let app = *self;
        let hosts = self.hosts.get();
        spawn(async move {
            let done = background(async move { hosts.get().await?.sign_out(subscription.key()).await }).await;
            match done {
                Ok(Ok(())) => app.load_models(true),
                Ok(Err(error)) => app.set_account_status(subscription, format!("{error:#}")),
                Err(error) => app.set_account_status(subscription, format!("{error}")),
            }
        });
    }

    /// The person's skills, as the agent finds them.
    fn load_skills(&self) {
        let app = *self;
        let hosts = self.hosts.get();
        spawn(async move {
            let found = background(async move { hosts.get().await?.skills(None).await }).await;
            let Ok(Ok(found)) = found else { return };
            let user_home = dirs_home();
            let short = |folder: &str| match std::path::Path::new(folder).strip_prefix(&user_home) {
                Ok(rest) => format!("~/{}", rest.display()),
                Err(_) => folder.to_owned(),
            };
            let mut folders: Vec<String> = Vec::new();
            for skill in &found.skills {
                let folder = short(&skill.folder);
                if !folders.contains(&folder) {
                    folders.push(folder);
                }
            }
            app.skills.set(SkillsSummary {
                count: found.skills.iter().filter(|s| s.enabled && !s.manual_only).count(),
                folders,
                problems: found.problems.len(),
                first_problem: found.problems.first().map(|d| match d.split_once("SKILL.md: ") {
                    Some((path, why)) => format!("{why} in {}", path.trim_end_matches('/').rsplit('/').next().unwrap_or_default()),
                    None => d.clone(),
                }),
            });
            let mut rows: Vec<SkillRow> = found
                .skills
                .into_iter()
                .filter(|skill| skill.enabled)
                .map(|skill| SkillRow {
                    source: match skill.source.as_str() {
                        "project" => "project",
                        "configured" => "configured",
                        _ => "user",
                    },
                    name: skill.name,
                    description: skill.description,
                    folder: skill.folder,
                    manual_only: skill.manual_only,
                })
                .collect();
            rows.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
            app.skill_list.set(rows);
        });
    }

    /// Shows the person's MCP servers as the agent has them.
    pub fn refresh_servers(&self) {
        let app = *self;
        let hosts = self.hosts.get();
        spawn(async move {
            let found = background(async move { hosts.get().await?.mcp_servers(None).await }).await;
            if let Ok(Ok(servers)) = found {
                app.mcp_servers.set(servers.iter().map(server_row).collect());
            }
        });
    }

    /// Tries a failed server again.
    pub fn retry_server(&self, name: &str) {
        let app = *self;
        let hosts = self.hosts.get();
        let server = name.to_owned();
        spawn(async move {
            let _ = background(async move { hosts.get().await?.reconnect_mcp_server(None, &server).await }).await;
            app.refresh_servers();
        });
        self.mcp_servers.update(|rows| {
            for row in rows.iter_mut().filter(|r| r.name == name) {
                row.status = "Connecting…".into();
                row.failed = false;
            }
        });
    }

    pub fn set_thinking(&self, level: ThinkingLevel) {
        self.with_store(|s| s.set_setting(THINKING, Some(level.as_str())));
        self.thinking.set(level);
    }

    pub fn set_appearance(&self, appearance: nsh_core::platform::Appearance) {
        use nsh_core::platform::Appearance;
        let value = match appearance {
            Appearance::Light => "light",
            Appearance::Dark => "dark",
            _ => "system",
        };
        self.with_store(|s| s.set_setting(APPEARANCE, Some(value)));
        self.appearance.set(appearance);
        set_color_scheme(appearance);
    }

    pub fn gh(&self) -> Option<Gh> {
        self.gh.get()
    }

    /// The GitHub repository a project folder belongs to.
    pub fn repo_of(&self, project: &str) -> Option<String> {
        if let Some(known) = self.repo_cache.with(|cache| cache.get(project).cloned()) {
            return known;
        }
        let repo = bubble_github::repo_of_folder(std::path::Path::new(project));
        self.repo_cache.with(|cache| cache.insert(project.to_owned(), repo.clone()));
        repo
    }

    pub fn repo_level(&self, repo: &str) -> Option<Level> {
        self.with_store(|s| s.setting(&github::level_key(repo))).flatten().and_then(|v| Level::parse(&v))
    }

    pub fn set_repo_level(&self, repo: &str, level: Option<Level>) {
        self.with_store(|s| s.set_setting(&github::level_key(repo), level.map(Level::as_str)));
        self.repo_levels.update(|v| *v += 1);
    }

    /// The projects that are GitHub repositories, with their levels.
    pub fn github_repos(&self) -> Vec<Repo> {
        self.repo_levels.get();
        let mut repos: Vec<Repo> = Vec::new();
        for project in self.projects_untracked() {
            if let Some(name) = self.repo_of(&project) {
                if !repos.iter().any(|r| r.name == name) {
                    let level = self.repo_level(&name);
                    repos.push(Repo { name, project, level });
                }
            }
        }
        repos
    }

    /// Runs `f` with the store; `None` when it could not be opened.
    pub fn with_store<R>(&self, f: impl FnOnce(&Store) -> anyhow::Result<R>) -> Option<R> {
        self.store.with(|store| {
            let store = store.as_ref()?;
            match f(store) {
                Ok(value) => Some(value),
                Err(error) => {
                    log::error!("store: {error:#}");
                    self.problem.set(Some(format!("{error:#}")));
                    None
                }
            }
        })
    }

    /// GitHub's events since the person last wrote to the Manager (the
    /// last day, before they ever have).
    pub fn refresh_away(&self) {
        let events = self.with_store(|s| {
            let run = s.manager_run(None)?;
            let last = s.messages(&run.id)?.iter().filter(|m| m.role == MessageRole::User).map(|m| m.created_at).max();
            let since = last.unwrap_or_else(|| bubble_store::now_ms() - 24 * 3600 * 1000);
            Ok(s.events_since(since)?.into_iter().filter(|e| e.source == "github").collect::<Vec<_>>())
        });
        self.away.set(events.unwrap_or_default());
    }

    pub fn refresh_tasks(&self) {
        if let Some(mut tasks) = self.with_store(|s| s.list_tasks()) {
            // Sessions imported from the TypeScript CLI show only in folders
            // that are projects here; the rest stay in `bubble`'s /session.
            let imported = self.with_store(|s| s.imported_task_ids()).unwrap_or_default();
            if !imported.is_empty() {
                let projects: HashSet<Option<String>> =
                    tasks.iter().filter(|t| !imported.contains(&t.task.id)).map(|t| t.task.project_cwd.clone()).collect();
                tasks.retain(|t| !imported.contains(&t.task.id) || projects.contains(&t.task.project_cwd));
            }
            self.tasks.set(tasks);
        }
        if let Some(previews) = self.with_store(|s| s.previews()) {
            self.previews.set(previews);
        }
        // What arrives in the conversation on screen is seen as it arrives.
        if let Route::Task(open) = self.route.get_untracked() {
            self.mark_seen(&open);
        }
    }

    /// Reads again which tickets wait for the person.
    pub fn refresh_tickets(&self) {
        if let Some(proposed) = self.with_store(|s| s.proposed_tickets()) {
            self.proposed.set(proposed);
        }
        self.tickets_revision.update(|revision| *revision += 1);
    }

    /// [`Self::bots`], without following changes.
    pub fn bots_untracked(&self) -> Vec<TaskSummary> {
        untracked(|| self.bots())
    }

    /// The bots, the one that spoke or was spoken to last first: the
    /// conversations the person keeps (the command line's sessions and
    /// what was closed are found by searching).
    pub fn bots(&self) -> Vec<TaskSummary> {
        let previews = self.previews.get();
        let mut bots: Vec<TaskSummary> = self
            .tasks
            .get()
            .into_iter()
            .filter(|t| t.task.origin != Origin::Cli && !matches!(t.task.lifecycle, Lifecycle::Done | Lifecycle::Canceled))
            .collect();
        let latest = |t: &TaskSummary| previews.get(&t.task.id).map_or(t.task.updated_at, |p| p.at.max(t.task.created_at));
        bots.sort_by(|a, b| latest(b).cmp(&latest(a)));
        bots
    }

    /// Whether a bot said something the person has not looked at.
    pub fn unread(&self, bot_id: &str) -> bool {
        let Some(preview) = self.previews.with(|p| p.get(bot_id).cloned()) else { return false };
        matches!(preview.role, MessageRole::Assistant | MessageRole::External)
            && preview.at > self.seen.with(|seen| seen.get(bot_id).copied().unwrap_or(0))
    }

    /// The person is looking at a bot's conversation.
    pub fn mark_seen(&self, bot_id: &str) {
        let now = bubble_store::now_ms();
        let newest = self.previews.with_untracked(|p| p.get(bot_id).map(|p| p.at));
        let known = self.seen.with_untracked(|seen| seen.get(bot_id).copied());
        // Nothing new since the last look: nothing to write.
        if known.is_some_and(|at| newest.is_none_or(|n| at >= n)) {
            return;
        }
        self.seen.update(|seen| {
            seen.insert(bot_id.to_owned(), now);
        });
        let json = self.seen.with_untracked(|seen| serde_json::to_string(seen).ok());
        self.with_store(|s| s.set_setting(BOTS_SEEN, json.as_deref()));
    }

    /// Makes a bot: a name no other bot has, its role in a word or two, and
    /// what it is for (its standing instructions). A bot is a role, not a
    /// folder: where it works comes with the work.
    pub fn create_bot(&self, name: &str, role: &str, instruction: &str, origin: Origin) -> Result<String, String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("A bot needs a name.".into());
        }
        if self.name_taken(name, None) {
            return Err(format!("A bot called {name} exists. Choose another name."));
        }
        let task = self
            .with_store(|s| s.create_task(NewTask { title: name, project_cwd: None, origin }))
            .ok_or_else(|| "The bot could not be saved.".to_owned())?;
        self.set_bot_role(&task.id, role);
        self.set_bot_instruction(&task.id, instruction);
        self.refresh_tasks();
        Ok(task.id)
    }

    /// Whether a bot at work (not `except`) already goes by `name`.
    pub fn name_taken(&self, name: &str, except: Option<&str>) -> bool {
        let name = name.trim().to_lowercase();
        self.bots_untracked().iter().any(|bot| Some(bot.task.id.as_str()) != except && bot.task.title.trim().to_lowercase() == name)
    }

    /// A copy of a bot: the same role and instructions under the next free
    /// name ("Developer 2"), with a conversation and notes of its own.
    pub fn duplicate_bot(&self, bot_id: &str) -> Option<String> {
        let original = self.with_store(|s| s.get_task(bot_id)).flatten()?;
        // "Developer 2" copies to "Developer 3", not "Developer 2 2".
        let base = match original.title.rsplit_once(' ') {
            Some((base, number)) if number.parse::<u32>().is_ok() => base.to_owned(),
            _ => original.title.clone(),
        };
        let name = (2..).map(|n| format!("{base} {n}")).find(|name| !self.name_taken(name, None))?;
        let id = self.create_bot(&name, &self.bot_role(bot_id), &self.bot_instruction(bot_id), original.origin).ok()?;
        for key in [task_model_key, task_thinking_key, task_access_key] {
            if let Some(value) = self.with_store(|s| s.setting(&key(bot_id))).flatten() {
                self.with_store(|s| s.set_setting(&key(&id), Some(&value)));
            }
        }
        Some(id)
    }

    /// A bot's role, in a word or two.
    pub fn bot_role(&self, bot_id: &str) -> String {
        self.with_store(|s| s.setting(&bot_role_key(bot_id))).flatten().unwrap_or_default()
    }

    pub fn set_bot_role(&self, bot_id: &str, role: &str) {
        let role: String = role.trim().chars().take(24).collect();
        self.with_store(|s| s.set_setting(&bot_role_key(bot_id), (!role.is_empty()).then_some(role.as_str())));
        self.task_revision.update(|v| *v += 1);
    }

    /// How many bots of one role there may be.
    pub fn max_role_bots(&self) -> u32 {
        self.with_store(|s| s.setting(MAX_ROLE_BOTS)).flatten().and_then(|v| v.parse().ok()).unwrap_or(DEFAULT_MAX_ROLE_BOTS).clamp(1, 12)
    }

    pub fn set_max_role_bots(&self, value: u32) {
        self.with_store(|s| s.set_setting(MAX_ROLE_BOTS, Some(&value.clamp(1, 12).to_string())));
        self.task_revision.update(|v| *v += 1);
    }

    /// The folder a bot works in now, if it is there.
    pub fn bot_project(&self, bot_id: &str) -> Option<String> {
        self.with_store(|s| s.get_task(bot_id)).flatten().and_then(|t| t.project_cwd)
    }

    /// The project folders known beyond the bots' current ones: every one
    /// a bot was sent to.
    fn remembered_projects(&self) -> Vec<String> {
        self.with_store(|s| s.setting(PROJECTS)).flatten().and_then(|json| serde_json::from_str(&json).ok()).unwrap_or_default()
    }

    /// Keeps `folder` among the known projects, newest first.
    pub fn remember_project(&self, folder: &str) {
        let mut known = self.remembered_projects();
        if known.first().map(String::as_str) == Some(folder) {
            return;
        }
        known.retain(|p| p != folder);
        known.insert(0, folder.to_owned());
        known.truncate(40);
        let json = serde_json::to_string(&known).ok();
        self.with_store(|s| s.set_setting(PROJECTS, json.as_deref()));
        // The repositories to watch are those of the known projects.
        self.repo_levels.update(|v| *v += 1);
    }

    /// Who sent the message `message_id` shows, when it came from a bot:
    /// the main bot unless another is recorded.
    pub fn sender_label(&self, message_id: &str) -> String {
        self.with_store(|s| s.setting(&from_key(message_id))).flatten().unwrap_or_else(|| manager::MAIN_BOT.to_owned())
    }

    /// The shape and color of a bot's face.
    pub fn face_of(&self, bot_id: &str) -> (usize, usize) {
        self.task_revision.get();
        self.with_store(|s| s.setting(&bot_face_key(bot_id)))
            .flatten()
            .and_then(|saved| {
                let (shape, color) = saved.split_once(':')?;
                Some((shape.parse().ok()?, color.parse().ok()?))
            })
            .unwrap_or_else(|| crate::avatar::default_face(bot_id))
    }

    /// Gives a bot the next shape, or the next color.
    pub fn cycle_face(&self, bot_id: &str, shape: bool) {
        let (now_shape, now_color) = untracked(|| self.face_of(bot_id));
        let next = if shape {
            ((now_shape + 1) % crate::avatar::SHAPE_COUNT, now_color)
        } else {
            (now_shape, (now_color + 1) % crate::avatar::COLOR_COUNT)
        };
        self.with_store(|s| s.set_setting(&bot_face_key(bot_id), Some(&format!("{}:{}", next.0, next.1))));
        self.task_revision.update(|v| *v += 1);
    }

    /// Renames a bot, unless another bot has the name.
    pub fn rename_bot(&self, bot_id: &str, name: &str) -> Result<(), String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("A bot needs a name.".into());
        }
        if self.name_taken(name, Some(bot_id)) {
            return Err(format!("A bot called {name} exists. Choose another name."));
        }
        self.with_store(|s| s.rename_task(bot_id, name));
        self.refresh_tasks();
        self.task_revision.update(|v| *v += 1);
        Ok(())
    }

    /// Sends a bot to work in another folder, or in none. Its conversation
    /// goes on: the folder belongs to the work, a turn at a time.
    pub fn set_bot_project(&self, bot_id: &str, folder: Option<&str>) {
        let folder = folder.map(|f| f.trim_end_matches('/')).filter(|f| !f.is_empty());
        if let Some(folder) = folder {
            self.remember_project(folder);
        }
        if self.bot_project(bot_id).as_deref() == folder {
            return;
        }
        self.with_store(|s| s.set_task_project(bot_id, folder));
        self.refresh_tasks();
        self.task_revision.update(|v| *v += 1);
    }

    /// What a bot was told it is for.
    pub fn bot_instruction(&self, bot_id: &str) -> String {
        self.with_store(|s| s.setting(&bot_instruction_key(bot_id))).flatten().unwrap_or_default()
    }

    pub fn set_bot_instruction(&self, bot_id: &str, instruction: &str) {
        let instruction = instruction.trim();
        self.with_store(|s| s.set_setting(&bot_instruction_key(bot_id), (!instruction.is_empty()).then_some(instruction)));
    }

    pub fn default_model(&self) -> Option<String> {
        self.config.with(|c| c.default_model_reference())
    }

    pub fn model_for(&self, role: RunRole) -> Option<String> {
        let chosen = match role {
            RunRole::Manager | RunRole::Triage => self.manager_model.get_untracked(),
            RunRole::Task => self.task_model.get_untracked(),
        };
        chosen.or_else(|| self.default_model())
    }

    /// The model a role runs on and the level it thinks at, for the composer.
    pub fn model_label(&self, role: RunRole) -> String {
        let chosen = match role {
            RunRole::Manager | RunRole::Triage => self.manager_model.get(),
            RunRole::Task => self.task_model.get(),
        };
        let Some(reference) = chosen.or_else(|| self.default_model()) else { return "No model".into() };
        self.label_of(&reference, self.thinking.get())
    }

    /// The model a task runs on: its own, else the Settings one.
    pub fn task_model_of(&self, task_id: &str) -> Option<String> {
        self.task_revision.get();
        self.with_store(|s| s.setting(&task_model_key(task_id))).flatten().or_else(|| self.model_for(RunRole::Task))
    }

    /// The task's own model, when it has one.
    pub fn task_model_override(&self, task_id: &str) -> Option<String> {
        self.task_revision.get();
        self.with_store(|s| s.setting(&task_model_key(task_id))).flatten()
    }

    /// How hard a task's model thinks: its own level, else Settings'.
    pub fn task_thinking_of(&self, task_id: &str) -> ThinkingLevel {
        self.task_revision.get();
        self.with_store(|s| s.setting(&task_thinking_key(task_id)))
            .flatten()
            .and_then(|v| ThinkingLevel::parse(&v))
            .unwrap_or_else(|| self.thinking.get())
    }

    pub fn set_task_model(&self, task_id: &str, model: Option<String>) {
        self.with_store(|s| s.set_setting(&task_model_key(task_id), model.as_deref()));
        self.task_revision.update(|v| *v += 1);
    }

    pub fn set_task_thinking(&self, task_id: &str, level: Option<ThinkingLevel>) {
        self.with_store(|s| s.set_setting(&task_thinking_key(task_id), level.map(|l| l.as_str())));
        self.task_revision.update(|v| *v += 1);
    }

    /// Whether the task's runs the person starts go ahead without asking.
    pub fn task_full_access(&self, task_id: &str) -> bool {
        self.task_revision.get();
        self.with_store(|s| s.setting(&task_access_key(task_id))).flatten().as_deref() == Some("full")
    }

    pub fn set_task_full_access(&self, task_id: &str, full: bool) {
        self.with_store(|s| s.set_setting(&task_access_key(task_id), full.then_some("full")));
        self.task_revision.update(|v| *v += 1);
        if full {
            if let Some(conversation) = self.conversations.with(|map| map.get(task_id).copied()) {
                conversation.allow_waiting(*self);
            }
        }
    }

    /// The levels a model can think at.
    pub fn levels_of(&self, reference: &str) -> Vec<ThinkingLevel> {
        if let Some(levels) = self.catalog.with(|c| c.levels.get(reference).cloned()) {
            return levels;
        }
        // Until the agent has answered, or for a model it did not list.
        match reference.split_once(':') {
            Some((provider, model)) => bubble_core::model::info(provider, model).levels.to_vec(),
            None => vec![ThinkingLevel::Off],
        }
    }

    /// The level a model really thinks at when `requested` is asked for:
    /// the nearest one it has at or below it, else its lowest.
    pub fn effective_level(&self, reference: &str, requested: ThinkingLevel) -> ThinkingLevel {
        let levels = self.levels_of(reference);
        levels.iter().copied().filter(|level| *level <= requested).max().or_else(|| levels.first().copied()).unwrap_or(ThinkingLevel::Off)
    }

    /// A model and the level it thinks at, as the composer shows them.
    fn label_of(&self, reference: &str, level: ThinkingLevel) -> String {
        if self.levels_of(reference) == [ThinkingLevel::Off] {
            reference.to_owned()
        } else {
            format!("{reference}  {}", self.effective_level(reference, level).as_str())
        }
    }

    /// Why the agent cannot run `reference`, when that is already known.
    fn check_model(&self, reference: &str) -> Result<(), String> {
        let Some((provider, _)) = reference.split_once(':') else {
            return Err(format!("model {reference:?} does not name its provider (provider:model)"));
        };
        self.catalog.with(|catalog| {
            if !catalog.loaded || catalog.providers.contains(provider) {
                Ok(())
            } else {
                Err(format!("Provider {provider:?} is not set up. Add it to ~/.bubble/config.json, or sign in to it in Settings."))
            }
        })
    }

    /// Stops a task's agent and its subagents and marks it canceled.
    pub fn cancel_task(&self, task_id: &str) {
        let conversation = self.task_conversation(task_id);
        conversation.stop(*self);
        // A queued one is out of the queue now; a running one stops.
        if conversation.busy.get_untracked() {
            conversation.stop(*self);
        }
        self.set_lifecycle(task_id, Lifecycle::Canceled);
    }

    pub fn set_model_setting(&self, role: RunRole, model: Option<String>) {
        let (key, signal) = match role {
            RunRole::Task => (TASK_MODEL, self.task_model),
            RunRole::Manager | RunRole::Triage => (MANAGER_MODEL, self.manager_model),
        };
        self.with_store(|s| s.set_setting(key, model.as_deref()));
        signal.set(model);
    }

    pub fn set_max_runs(&self, value: u32) {
        let value = value.clamp(1, 12);
        self.with_store(|s| s.set_setting(MAX_RUNS, Some(&value.to_string())));
        self.max_runs.set(value);
        self.pump_queue();
    }

    pub fn create_task(&self, title: &str, project: Option<&str>) -> Option<String> {
        let task = self.with_store(|s| s.create_task(NewTask { title, project_cwd: project, origin: Origin::User }))?;
        self.refresh_tasks();
        Some(task.id)
    }

    pub fn set_lifecycle(&self, task_id: &str, lifecycle: Lifecycle) {
        self.with_store(|s| s.set_task_lifecycle(task_id, lifecycle, None));
        self.refresh_tasks();
    }

    /// The projects tasks belong to, most recently active first.
    pub fn projects(&self) -> Vec<String> {
        self.task_revision.get();
        projects_of(self.tasks.get(), self.remembered_projects())
    }

    /// `projects`, without subscribing to the task list.
    pub fn projects_untracked(&self) -> Vec<String> {
        projects_of(self.tasks.get_untracked(), self.remembered_projects())
    }

    /// The Manager's conversation, made once and kept by the app's root.
    pub fn manager_conversation(&self) -> Conversation {
        self.conversation(MANAGER_KEY, None)
    }

    /// A task's conversation, made once and kept by the app's root.
    pub fn task_conversation(&self, task_id: &str) -> Conversation {
        self.conversation(task_id, Some(task_id.to_owned()))
    }

    fn conversation(&self, key: &str, task_id: Option<String>) -> Conversation {
        if let Some(existing) = self.conversations.with(|map| map.get(key).copied()) {
            return existing;
        }
        let lines = self
            .with_store(|s| {
                let runs = match &task_id {
                    None => vec![s.manager_run(None)?],
                    Some(id) => s.runs_for_task(id)?,
                };
                lines_of(s, &runs)
            })
            .unwrap_or_default();
        // A task's children outlive the app.
        let children = match &task_id {
            Some(id) => Children::persistent(bubble_home().join("native").join("subagents").join(id)),
            None => Children::new(),
        };
        let conversation = self.root.run(|| Conversation {
            lines: signal(lines),
            busy: signal(false),
            queued: signal(false),
            task_id: stored(task_id),
            children: stored(children),
            subagents: signal(Vec::new()),
            approvals: stored(HashMap::new()),
            questions: stored(HashMap::new()),
            pending: stored(String::new()),
            active_turn: stored(String::new()),
            active_run: stored(String::new()),
            checkpoint_at: stored(0),
            opened: signal(HashSet::new()),
            following: signal(true),
            abort: stored(None),
            on_change: stored(None),
            owes: stored(None),
            awaiting: stored(HashSet::new()),
            inbox: stored(VecDeque::new()),
        });
        self.conversations.with(|map| map.insert(key.to_owned(), conversation));
        // Subagents from before show at once: those the agent reported in an
        // earlier run of the app, and those the earlier engine kept on disk.
        let mut snapshots = conversation.children.get().snapshots();
        let stored_ones = conversation
            .task_id
            .get()
            .and_then(|id| self.with_store(|s| s.setting(&subagents_key(&id))).flatten())
            .and_then(|json| serde_json::from_str::<Vec<delegate::Snapshot>>(&json).ok())
            .unwrap_or_default();
        for mut child in stored_ones {
            // The app closed while it ran; the agent it belonged to is gone.
            if !child.status.is_final() {
                child.status = delegate::Status::Failed;
                child.error = Some("Interrupted when Bubble closed".to_owned());
            }
            if !snapshots.iter().any(|c| c.id == child.id) {
                snapshots.push(child);
            }
        }
        conversation.lines.update(|lines| transcript::restore_child_anchors(lines, &snapshots));
        conversation.subagents.set(snapshots);
        // The earlier engine's children still change when the person applies
        // or discards a worktree one of them kept.
        let children = conversation.children.get();
        let mut changes = children.subscribe();
        self.root.run(|| {
            spawn(async move {
                while changes.changed().await.is_ok() {
                    let kept = children.snapshots();
                    conversation.subagents.update(|all| {
                        for child in kept {
                            match all.iter_mut().find(|c| c.id == child.id) {
                                Some(existing) => *existing = child,
                                None => all.push(child),
                            }
                        }
                    });
                }
            });
        });
        conversation
    }

    /// Task turns running now (waiting on approval counts; queued does not).
    fn running_task_turns(&self) -> usize {
        self.conversations.with(|map| {
            map.iter()
                .filter(|(key, c)| key.as_str() != MANAGER_KEY && c.busy.get_untracked() && !c.queued.get_untracked())
                .count()
        })
    }

    fn has_free_slot(&self) -> bool {
        self.running_task_turns() < self.max_runs.get_untracked() as usize
    }

    /// Starts queued tasks while there is room.
    fn pump_queue(&self) {
        while self.has_free_slot() {
            let Some(task_id) = self.queue.with(|q| q.pop_front()) else { break };
            self.task_conversation(&task_id).start_turn(*self);
        }
    }

    /// Queued tasks from before a restart wait again.
    fn resume_queue(&self) {
        let queued: Vec<String> = self
            .tasks
            .get_untracked()
            .into_iter()
            .filter(|t| t.state == TaskState::Queued)
            .map(|t| t.task.id)
            .collect();
        for task_id in queued {
            let conversation = self.task_conversation(&task_id);
            conversation.wait_in_queue(*self, &format!("resumed-{task_id}"));
        }
        self.pump_queue();
    }
}

fn appearance_of(value: Option<&str>) -> nsh_core::platform::Appearance {
    use nsh_core::platform::Appearance;
    match value {
        Some("light") => Appearance::Light,
        Some("dark") => Appearance::Dark,
        _ => Appearance::System,
    }
}

/// A development home that has no configuration yet starts with the
/// person's own: their providers and sign-ins, so models need no second
/// setup. One that has its own keeps it.
fn seed_development_home() {
    if !bubble_core::config::development_data() {
        return;
    }
    bring_configuration(&bubble_core::config::production_home(), &bubble_home());
}

/// Copies the providers and sign-ins of the home `from` to the home `to`,
/// unless `to` is set up already.
fn bring_configuration(from: &std::path::Path, to: &std::path::Path) {
    if to.join("config.json").exists() {
        return;
    }
    std::fs::create_dir_all(to).ok();
    for file in ["config.json", "auth.json"] {
        if from.join(file).is_file() {
            if let Err(error) = std::fs::copy(from.join(file), to.join(file)) {
                log::warn!("{file} could not be brought to the development data: {error}");
            }
        }
    }
}

#[cfg(test)]
mod development_tests {
    #[test]
    fn a_home_that_is_not_set_up_takes_the_configuration_and_one_that_is_keeps_its_own() {
        let root = std::env::temp_dir().join(format!("bubble-bring-{}", std::process::id()));
        let (from, fresh, set_up) = (root.join("from"), root.join("fresh"), root.join("set-up"));
        std::fs::create_dir_all(&from).unwrap();
        std::fs::create_dir_all(&set_up).unwrap();
        std::fs::write(from.join("config.json"), "theirs").unwrap();
        std::fs::write(from.join("auth.json"), "signed in").unwrap();
        std::fs::write(set_up.join("config.json"), "its own").unwrap();
        super::bring_configuration(&from, &fresh);
        super::bring_configuration(&from, &set_up);
        assert_eq!(std::fs::read_to_string(fresh.join("config.json")).unwrap(), "theirs");
        assert_eq!(std::fs::read_to_string(fresh.join("auth.json")).unwrap(), "signed in");
        assert_eq!(std::fs::read_to_string(set_up.join("config.json")).unwrap(), "its own");
        assert!(!set_up.join("auth.json").exists());
        std::fs::remove_dir_all(root).ok();
    }
}

/// A development database that is new takes over how the person set the
/// app up (its models, how hard they think, its look, its limits), once.
/// Bots, conversations and what GitHub brought start empty: that is what
/// is kept apart.
fn seed_development_preferences(store: &Store, config: &Config) {
    const SEEDED: &str = "development_seeded";
    if !bubble_core::config::development_data() || store.setting(SEEDED).ok().flatten().is_some() {
        return;
    }
    let theirs = bubble_core::config::production_home();
    let mut kept = Store::peek_settings(theirs.join("native").join("bubble.db"), &[MANAGER_MODEL, TASK_MODEL, THINKING, APPEARANCE, MAX_RUNS, MAX_ROLE_BOTS]);
    // Where they chose no model there, the app runs on their configuration's
    // default; here that default may be another, so theirs is written
    // down, when this configuration has its provider too.
    let model = Config::load_from(&theirs.join("config.json")).ok().and_then(|c| c.default_model_reference());
    let offered = |reference: &str| reference.split_once(':').is_some_and(|(provider, _)| config.providers.iter().any(|p| p.id == provider));
    if let Some(model) = model.filter(|m| offered(m) && config.default_model_reference().as_deref() != Some(m.as_str())) {
        for key in [MANAGER_MODEL, TASK_MODEL] {
            kept.entry(key.to_owned()).or_insert_with(|| model.clone());
        }
    }
    for (key, value) in &kept {
        let _ = store.set_setting(key, Some(value));
    }
    let _ = store.set_setting(SEEDED, Some("1"));
}

/// The known project folders: the ones bots were sent to, newest first,
/// then the ones conversations are in.
fn projects_of(mut tasks: Vec<TaskSummary>, remembered: Vec<String>) -> Vec<String> {
    tasks.sort_by(|a, b| b.task.updated_at.cmp(&a.task.updated_at));
    let mut projects: Vec<String> = remembered;
    for task in tasks {
        if let Some(cwd) = task.task.project_cwd {
            if !projects.contains(&cwd) {
                projects.push(cwd);
            }
        }
    }
    projects
}

/// Work a bot does for someone other than the person writing to it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Owed {
    /// Who hears how it went: a bot does, an event or a routine does not.
    to: Option<Sender>,
    /// The record of the work.
    ticket: Option<String>,
}

/// A conversation: the Manager's, or a task's runs.
#[derive(Clone, Copy)]
pub struct Conversation {
    pub lines: Signal<Vec<Line>>,
    /// A turn is running or queued; new messages wait.
    pub busy: Signal<bool>,
    /// Waiting for a free slot.
    pub queued: Signal<bool>,
    pub task_id: Stored<Option<String>>,
    /// Subagents the task's agent started; they outlive its turns.
    children: Stored<Arc<Children>>,
    pub subagents: Signal<Vec<delegate::Snapshot>>,
    /// Disclosure choices belong to the conversation, including across page changes.
    pub opened: Signal<HashSet<String>>,
    pub following: Signal<bool>,
    /// Questions the agent waits on, by their line's id.
    approvals: Stored<HashMap<String, oneshot::Sender<Decision>>>,
    /// What the agent asked the person and waits to hear, by line id.
    questions: Stored<HashMap<String, oneshot::Sender<Option<Vec<Vec<String>>>>>>,
    /// The line the next reply streams into.
    pending: Stored<String>,
    active_turn: Stored<String>,
    active_run: Stored<String>,
    checkpoint_at: Stored<i64>,
    /// Stops the running turn.
    abort: Stored<Option<Arc<tokio::sync::Notify>>>,
    /// Called after the lines change, to keep the newest in view.
    on_change: Stored<Option<Rc<dyn Fn()>>>,
    /// The work in progress that someone other than the person asked for.
    owes: Stored<Option<Owed>>,
    /// The bots this one asked and has not heard back from.
    awaiting: Stored<HashSet<String>>,
    /// What other bots sent while a turn ran, in order.
    inbox: Stored<VecDeque<Incoming>>,
}

impl Conversation {
    /// The subagents the task's agent started.
    pub fn children(&self) -> Arc<Children> {
        self.children.get()
    }

    pub fn on_change(&self, f: impl Fn() + 'static) {
        self.on_change.replace(Some(Rc::new(f)));
    }

    fn changed(&self) {
        if let Some(f) = self.on_change.get() {
            f();
        }
    }

    fn update_line(&self, id: &str, f: impl FnOnce(&mut Line)) {
        self.lines.update(|lines| {
            if let Some(line) = lines.iter_mut().find(|l| l.id == id) {
                f(line);
            }
        });
    }

    fn remove_line(&self, id: &str) {
        self.lines.update(|lines| lines.retain(|l| l.id != id));
    }

    fn push(&self, mut line: Line) {
        let turn = self.active_turn.get();
        if !turn.is_empty() && line.turn_id.is_none() && !matches!(line.kind, LineKind::Message(MessageRole::User | MessageRole::System | MessageRole::External) | LineKind::RunStart(_)) {
            line.turn_id = Some(turn);
        }
        self.lines.update(|lines| lines.push(line));
    }

    fn begin_model(&self, step: &mut usize) -> String {
        *step += 1;
        let turn = self.active_turn.get();
        self.lines.update(|lines| { transcript::begin_model(lines, &turn, *step); });
        let id = format!("{turn}:model-{}", *step);
        self.pending.replace(id.clone());
        id
    }

    fn transcript_checkpoint(&self) -> Option<(String, String)> {
        let turn = self.active_turn.get();
        if turn.is_empty() { return None; }
        let items = self.lines.with_untracked(|lines| lines.iter()
            .filter(|l| l.turn_id.as_deref() == Some(&turn)).cloned().collect::<Vec<_>>());
        serde_json::to_string(&items).ok().map(|content| (turn.strip_prefix("turn-").unwrap_or(&turn).to_owned(), content))
    }

    fn checkpoint(&self, app: AppState, force: bool) {
        let run = self.active_run.get();
        if run.is_empty() { return; }
        let now = bubble_store::now_ms();
        if !force && now - self.checkpoint_at.get() < 500 { return; }
        if let Some((anchor, content)) = self.transcript_checkpoint() {
            if app.with_store(|s| s.save_turn_transcript(&run, &anchor, &content)).is_some() {
                self.checkpoint_at.replace(now);
            }
        }
    }

    fn turn_status(&self, status: TurnStatus) {
        let turn = self.active_turn.get();
        self.lines.update(|lines| transcript::set_turn_status(lines, &turn, status));
    }

    /// Shows a question after the tool line of `call_id`, marks the run as
    /// waiting on the person, and returns their answer.
    pub fn ask(&self, app: AppState, run_id: &str, call_id: &str, permission: Permission) -> oneshot::Receiver<Decision> {
        let (respond, answer) = oneshot::channel();
        self.ask_with(app, run_id, call_id, permission, respond);
        answer
    }

    fn ask_with(&self, app: AppState, run_id: &str, call_id: &str, permission: Permission, respond: oneshot::Sender<Decision>) {
        let turn = self.active_turn.get();
        let tool_id = self.lines.with_untracked(|lines| lines.iter().rev().find(|l| l.turn_id.as_deref() == Some(&turn) && l.call_id.as_deref() == Some(call_id)).map(|l| l.id.clone()))
            .unwrap_or_else(|| format!("{turn}:tool-{call_id}"));
        let id = format!("{tool_id}:approval");
        self.approvals.with(|map| map.insert(id.clone(), respond));
        let line = Line::new(
            id,
            LineKind::Approval {
                summary: permission.summary,
                detail: permission.detail,
                rememberable: permission.rememberable,
                decided: None,
            },
            "",
        );
        let line = line.in_turn(&turn);
        self.lines.update(|lines| {
            if let Some(tool) = transcript::tool_mut(lines, &turn, call_id) {
                if let LineKind::Tool { status, .. } = &mut tool.kind { *status = ToolStatus::NeedsInput; }
            }
            let at = lines.iter().position(|l| l.id == tool_id).map(|i| i + 1).unwrap_or(lines.len());
            lines.insert(at, line);
        });
        self.turn_status(TurnStatus::NeedsInput);
        self.checkpoint(app, true);
        app.with_store(|s| s.set_run_status(run_id, RunStatus::NeedsInput, None));
        app.refresh_tasks();
        self.changed();
    }

    /// Shows what the agent asks the person, after the call that asked
    /// it, and marks the run as waiting on them.
    fn ask_person(
        &self,
        app: AppState,
        run_id: &str,
        call_id: Option<&str>,
        asks: Vec<transcript::Ask>,
        respond: oneshot::Sender<Option<Vec<Vec<String>>>>,
    ) {
        if asks.is_empty() {
            let _ = respond.send(None);
            return;
        }
        let turn = self.active_turn.get();
        let number = self.lines.with_untracked(|lines| lines.iter().filter(|l| matches!(l.kind, LineKind::Question { .. })).count() + 1);
        let id = format!("{turn}:question-{number}");
        self.questions.with(|map| map.insert(id.clone(), respond));
        let line = Line::new(id, LineKind::Question { asks, answers: None }, "").in_turn(&turn);
        self.lines.update(|lines| {
            let after = call_id.and_then(|call| lines.iter().rposition(|l| l.turn_id.as_deref() == Some(&turn) && l.call_id.as_deref() == Some(call)));
            match after {
                Some(index) => lines.insert(index + 1, line),
                None => lines.push(line),
            }
        });
        self.turn_status(TurnStatus::NeedsInput);
        self.checkpoint(app, true);
        app.with_store(|s| s.set_run_status(run_id, RunStatus::NeedsInput, None));
        app.refresh_tasks();
        self.changed();
    }

    /// Gives the agent the person's answer to the question on line `id`:
    /// the labels chosen for each ask, or `None` to skip it.
    pub fn answer_question(&self, app: AppState, id: &str, answers: Option<Vec<Vec<String>>>) {
        let Some(respond) = self.questions.with(|map| map.remove(id)) else { return };
        let _ = respond.send(answers.clone());
        self.update_line(id, |line| {
            if let LineKind::Question { answers: kept, .. } = &mut line.kind {
                *kept = Some(answers.unwrap_or_default());
            }
        });
        if self.approvals.with(|map| map.is_empty()) && self.questions.with(|map| map.is_empty()) {
            self.turn_status(TurnStatus::Working);
            if let Some(run) = self.current_run(app) {
                app.with_store(|s| s.set_run_status(&run.id, RunStatus::Running, None));
                app.refresh_tasks();
            }
        }
        self.checkpoint(app, true);
        self.changed();
    }

    /// Answers the question on line `id`.
    pub fn decide(&self, app: AppState, id: &str, decision: Decision) {
        let Some(respond) = self.approvals.with(|map| map.remove(id)) else { return };
        let _ = respond.send(decision);
        self.update_line(id, |line| {
            if let LineKind::Approval { decided, .. } = &mut line.kind {
                *decided = Some(decision);
            }
        });
        if let Some(tool_id) = id.strip_suffix(":approval") {
            self.update_line(tool_id, |line| {
                if let LineKind::Tool { status, .. } = &mut line.kind { *status = ToolStatus::Running; }
            });
        }
        if self.approvals.with(|map| map.is_empty()) && self.questions.with(|map| map.is_empty()) {
            self.turn_status(TurnStatus::Working);
            if let Some(run) = self.current_run(app) {
                app.with_store(|s| s.set_run_status(&run.id, RunStatus::Running, None));
                app.refresh_tasks();
            }
        }
        self.checkpoint(app, true);
        self.changed();
    }

    fn current_run(&self, app: AppState) -> Option<Run> {
        match self.task_id.get() {
            None => app.with_store(|s| s.manager_run(None)),
            Some(id) => app.with_store(|s| Ok(s.runs_for_task(&id)?.last().cloned())).flatten(),
        }
    }

    /// Stops the running turn, or takes a queued one out of the queue.
    pub fn stop(&self, app: AppState) {
        if !self.busy.get_untracked() {
            return;
        }
        if self.queued.get_untracked() {
            if let Some(task_id) = self.task_id.get() {
                app.queue.with(|q| q.retain(|id| id != &task_id));
            }
            if let Some(run) = self.current_run(app) {
                app.with_store(|s| s.set_run_status(&run.id, RunStatus::Idle, None));
            }
            let pending = self.pending.get();
            self.remove_line(&pending);
            self.queued.set(false);
            self.busy.set(false);
            let turn = self.active_turn.get();
            self.lines.update(|lines| transcript::finish(lines, &turn, TurnStatus::Stopped, bubble_store::now_ms()));
            self.push(Line::new(format!("stopped-{}", bubble_store::now_ms()), LineKind::Notice, "Taken out of the queue"));
            self.checkpoint(app, true);
            app.refresh_tasks();
            self.changed();
            return;
        }
        // The agent ends the turn and says so; what it wrote so far stays.
        if let Some(stop) = self.abort.get() {
            stop.notify_one();
        }
        self.children.get().cancel_all();
    }

    /// Sends what the person wrote: runs the turn now, or queues it.
    pub fn send(&self, app: AppState, text: String) {
        self.send_with(app, text, None, Vec::new());
    }

    /// Sends what the person wrote, with the files it mentions (`expanded`:
    /// the text the model reads) and pictures.
    pub fn send_with(&self, app: AppState, text: String, expanded: Option<String>, images: Vec<bubble_core::Image>) {
        let content = (expanded.is_some() || !images.is_empty())
            .then(|| ChatMessage::user_with_images(expanded.unwrap_or_else(|| text.trim().to_owned()), images));
        if let Err(message) = self.post_to(app, text, MessageRole::User, None, content, None) {
            self.fail_now(&message);
        }
    }

    /// Records a message for the agent (the person's, or a brief from the
    /// Manager) and starts the turn, or queues it when the limit is
    /// reached. `Ok(true)`: started; `Ok(false)`: queued.
    pub fn post(&self, app: AppState, text: String, role: MessageRole) -> Result<bool, String> {
        self.post_to(app, text, role, None, None, None)
    }

    /// Whether the turn in progress waits on the person: an approval or a
    /// question nobody answered yet.
    pub fn waits_on_person(&self) -> bool {
        self.lines.with(|lines| {
            lines.iter().any(|l| matches!(l.kind, LineKind::Approval { decided: None, .. } | LineKind::Question { answers: None, .. }))
        })
    }

    /// This bot asked `bot_id` for something and waits for its answer.
    pub fn awaits(&self, bot_id: &str) {
        self.awaiting.with(|waiting| {
            waiting.insert(bot_id.to_owned());
        });
    }

    /// How much stands between this bot and new work: its turn in
    /// progress, and what waits behind it.
    pub fn waiting(&self) -> usize {
        usize::from(self.busy.get_untracked()) + self.inbox.with(|inbox| inbox.len())
    }

    /// Takes a message from another bot: starts on it now, or keeps it
    /// until the running turn ends. `Ok(true)`: a turn started.
    pub fn receive(&self, app: AppState, incoming: Incoming) -> Result<bool, String> {
        if self.busy.get_untracked() {
            self.inbox.with(|inbox| inbox.push_back(incoming));
            return Ok(false);
        }
        self.take(app, incoming)
    }

    /// What waits that wants doing first: the most pressing, and among
    /// equals the one that came first.
    fn next_waiting(&self) -> Option<Incoming> {
        self.inbox.with(|inbox| {
            let first = inbox.iter().enumerate().min_by_key(|(index, incoming)| (incoming.priority, *index)).map(|(index, _)| index)?;
            inbox.remove(first)
        })
    }

    fn take(&self, app: AppState, incoming: Incoming) -> Result<bool, String> {
        let before = self.owes.get();
        match (&incoming.report, &incoming.from) {
            // A request: it is this bot's work now, and a bot that sent
            // it is owed the answer.
            (None, from) => {
                self.owes.replace(Some(Owed { to: from.hears_back().then(|| from.clone()), ticket: incoming.ticket.clone() }));
            }
            // The answer to something this bot asked.
            (Some(_), Sender::Bot(id)) => {
                self.awaiting.with(|waiting| {
                    waiting.remove(id);
                });
            }
            (Some(_), _) => {}
        }
        // The work says where it is: the bot goes there for it.
        if let (Some(id), Some(project), None) = (self.task_id.get(), &incoming.project, &incoming.report) {
            app.set_bot_project(&id, Some(project));
        }
        let label = incoming.from.name(app);
        // What GitHub brings shows as its own kind of message.
        let role = if incoming.from == Sender::Github { MessageRole::External } else { MessageRole::System };
        // The model is told who it is from; the person sees that as a label.
        let content = ChatMessage::user(manager::framed(app, &incoming));
        let shown = incoming.shown.clone().unwrap_or(incoming.text);
        let started = self.post_to(app, shown, role, None, Some(content), Some(label));
        if started.is_err() {
            self.owes.replace(before);
        }
        started
    }

    /// What the person said in the turn that just ended and what the bot
    /// answered, when it was the person who started the turn.
    fn last_exchange_with_person(&self) -> Option<(String, String)> {
        let turn = self.active_turn.get();
        self.lines.with_untracked(|lines| {
            let opener_id = turn.strip_prefix("turn-")?;
            let opener = lines.iter().find(|l| l.id == opener_id)?;
            if !matches!(opener.kind, LineKind::Message(MessageRole::User)) {
                return None;
            }
            let answered = lines
                .iter()
                .rev()
                .find(|l| l.turn_id.as_deref() == Some(&turn) && matches!(l.kind, LineKind::Message(MessageRole::Assistant)) && !l.text.trim().is_empty())?;
            Some((opener.text.clone(), answered.text.clone()))
        })
    }

    /// A turn is over: whoever asked for the work hears how it went, and
    /// the next message that waited is taken up.
    fn after_turn(&self, app: AppState, ending: TurnStatus, failure: Option<String>) {
        if self.awaiting.with(|waiting| waiting.is_empty()) {
            if let Some(owed) = self.owes.replace(None) {
                let turn = self.active_turn.get();
                let said = self
                    .lines
                    .with_untracked(|lines| {
                        lines
                            .iter()
                            .rev()
                            .find(|l| {
                                l.turn_id.as_deref() == Some(&turn)
                                    && matches!(l.kind, LineKind::Message(MessageRole::Assistant))
                                    && !l.text.trim().is_empty()
                            })
                            .map(|l| l.text.clone())
                    })
                    .unwrap_or_default();
                let (outcome, text) = match (ending, failure) {
                    (TurnStatus::Failed, Some(error)) => ("failed", format!("{error}\n\n{said}").trim().to_owned()),
                    (TurnStatus::Failed, None) => ("failed", said),
                    (TurnStatus::Stopped, _) if said.is_empty() => ("stopped by the person", "It was stopped before it answered.".to_owned()),
                    (TurnStatus::Stopped, _) => ("stopped by the person", said),
                    _ if said.is_empty() => ("finished", "It finished without a reply.".to_owned()),
                    _ => ("finished", said),
                };
                // The whole answer is in this bot's own conversation.
                let text: String = if text.chars().count() > 6000 {
                    format!("{}\n(cut short; the rest is in the bot's conversation)", text.chars().take(6000).collect::<String>())
                } else {
                    text
                };
                if let Some(ticket) = &owed.ticket {
                    let status = match ending {
                        TurnStatus::Failed => bubble_store::TicketStatus::Failed,
                        TurnStatus::Stopped => bubble_store::TicketStatus::Stopped,
                        _ => bubble_store::TicketStatus::Finished,
                    };
                    app.with_store(|s| s.set_ticket_status(ticket, status, Some(&text)));
                    app.refresh_tickets();
                }
                let me = match self.task_id.get() {
                    Some(id) => Sender::Bot(id),
                    None => Sender::Main,
                };
                if let Some(asker) = &owed.to {
                    let report = Incoming::report(me, text, outcome);
                    if let Err(error) = manager::deliver(app, asker, report) {
                        log::warn!("a report could not be delivered: {error}");
                    }
                }
            }
        }
        if !self.busy.get_untracked() {
            if let Some(next) = self.next_waiting() {
                if let Err(error) = self.take(app, next) {
                    log::warn!("a waiting message could not be taken up: {error}");
                }
            }
        }
    }

    /// `post`; with `fresh`, in a new run begun for that reason; `content`:
    /// what the model reads, when it is more than the text.
    fn post_to(
        &self,
        app: AppState,
        text: String,
        role: MessageRole,
        fresh: Option<String>,
        content: Option<ChatMessage>,
        from: Option<String>,
    ) -> Result<bool, String> {
        let text = text.trim().to_owned();
        let images = match &content {
            Some(ChatMessage::User { images, .. }) => images.len(),
            _ => 0,
        };
        if text.is_empty() && images == 0 {
            return Err("The message is empty.".into());
        }
        if self.busy.get_untracked() {
            return Err("The agent is still on its previous turn; send this when it finishes.".into());
        }
        let task_id = self.task_id.get();
        let model_reference = match &task_id {
            Some(id) => app.task_model_of(id),
            None => app.model_for(RunRole::Manager),
        };
        let model_reference = model_reference.ok_or_else(|| {
            "No model is set. Choose one in Settings, or set a default model in ~/.bubble/config.json.".to_owned()
        })?;
        app.check_model(&model_reference)?;
        let start_now = task_id.is_none() || app.has_free_slot();

        // The run this message goes to: the Manager's, or the task's latest
        // one unless it failed (then a new run starts).
        let prepared = app.with_store(|s| -> anyhow::Result<(Run, Option<usize>)> {
            match &task_id {
                None => Ok((s.manager_run(Some(&model_reference))?, None)),
                Some(task_id) => {
                    let task = s.get_task(task_id)?.ok_or_else(|| anyhow!("the bot is gone"))?;
                    let runs = s.runs_for_task(task_id)?;
                    // A bot's conversation is one: a failed turn does not start another.
                    let reusable = runs.last().filter(|_| fresh.is_none()).cloned();
                    let (run, index) = match reusable {
                        Some(run) => (run, None),
                        None => {
                            let run = s.create_run(NewRun {
                                task_id: Some(task_id),
                                role: RunRole::Task,
                                model: Some(&model_reference),
                                // Its session lives in the app's own
                                // folder: the bot goes from project to
                                // project, its conversation stays one.
                                cwd: None,
                                status: RunStatus::Queued,
                            })?;
                            if let Some(cause) = &fresh {
                                s.set_setting(&run_cause_key(&run.id), Some(cause))?;
                            }
                            (run, Some(runs.len() + 1))
                        }
                    };
                    if task.lifecycle != Lifecycle::Open {
                        s.set_task_lifecycle(task_id, Lifecycle::Open, None)?;
                    }
                    Ok((run, index))
                }
            }
        });
        let (run, new_run_index) = prepared.ok_or_else(|| "The message could not be saved.".to_owned())?;
        let content = serde_json::to_string(&content.unwrap_or_else(|| ChatMessage::user(text.clone()))).ok();
        let saved = app
            .with_store(|s| s.append_message_with(&run.id, role, &text, content.as_deref()))
            .ok_or_else(|| "The message could not be saved.".to_owned())?;
        if let Some(label) = &from {
            app.with_store(|s| s.set_setting(&from_key(&saved.id), Some(label)));
        }

        if let Some(index) = new_run_index.filter(|&i| i > 1) {
            let cause = fresh.clone().unwrap_or_else(|| cause_of(role).to_owned());
            let label = run_label(&cause, run.model.as_deref());
            self.push(Line::new(format!("{}-start", run.id), LineKind::RunStart(index), label));
        }
        self.push(Line { images, at: saved.created_at, ..Line::new(saved.id.clone(), LineKind::Message(role), text) });
        let turn = format!("turn-{}", saved.id);
        self.active_turn.replace(turn.clone());
        self.active_run.replace(run.id.clone());
        self.pending.replace(String::new());
        self.lines.update(|lines| transcript::begin_turn(lines, &turn, saved.created_at, if start_now { TurnStatus::Working } else { TurnStatus::Queued }));
        self.checkpoint(app, true);
        self.changed();
        if task_id.is_none() {
            // The person is back: what happened is no longer "while away".
            app.refresh_away();
        }
        let pending = format!("pending-{}", saved.id);
        if start_now {
            self.start_turn(app);
            Ok(true)
        } else {
            self.wait_in_queue(app, &pending);
            Ok(false)
        }
    }

    /// Marks the conversation as waiting for a slot.
    fn wait_in_queue(&self, app: AppState, _pending: &str) {
        let Some(task_id) = self.task_id.get() else { return };
        if let Some(run) = self.current_run(app) {
            app.with_store(|s| s.set_run_status(&run.id, RunStatus::Queued, None));
        }
        self.restore_turn_anchor(app);
        self.turn_status(TurnStatus::Queued);
        self.checkpoint(app, true);
        self.busy.set(true);
        self.queued.set(true);
        app.queue.with(|q| {
            if !q.contains(&task_id) {
                q.push_back(task_id)
            }
        });
        app.refresh_tasks();
        self.changed();
    }

    fn restore_turn_anchor(&self, app: AppState) {
        if !self.active_turn.get().is_empty() { return; }
        let Some(run) = self.current_run(app) else { return };
        let opener = self.lines.with_untracked(|lines| lines.iter().rev().find(|l| matches!(l.kind,
            LineKind::Message(MessageRole::User | MessageRole::System | MessageRole::External))).cloned());
        let Some(opener) = opener else { return };
        let turn = format!("turn-{}", opener.id);
        self.active_turn.replace(turn.clone());
        self.active_run.replace(run.id);
        if !self.lines.with_untracked(|lines| lines.iter().any(|l| l.id == turn)) {
            self.lines.update(|lines| transcript::begin_turn(lines, &turn, opener.at, TurnStatus::Queued));
        }
    }

    /// Runs the agent's turn on the run's history (the newest message is
    /// already stored).
    fn start_turn(&self, app: AppState) {
        let task_id = self.task_id.get();
        let Some(run) = self.current_run(app) else { return };
        let reference = match &task_id {
            Some(id) => app.task_model_of(id),
            None => app.model_for(RunRole::Manager),
        };
        let thinking = match &task_id {
            Some(id) => app.task_thinking_of(id),
            None => app.thinking.get_untracked(),
        };
        let model = reference
            .clone()
            .ok_or_else(|| anyhow!("no model is set"))
            .and_then(|reference| app.check_model(&reference).map(|_| reference).map_err(|error| anyhow!(error)));
        let model = match model {
            Ok(model) => model,
            Err(error) => {
                self.queued.set(false);
                self.busy.set(false);
                let pending = self.pending.get();
                self.remove_line(&pending);
                app.with_store(|s| s.set_run_status(&run.id, RunStatus::Failed, Some(&format!("{error:#}"))));
                self.restore_turn_anchor(app);
                let turn = self.active_turn.get();
                self.lines.update(|lines| transcript::finish(lines, &turn, TurnStatus::Failed, bubble_store::now_ms()));
                self.fail_now(&format!("{error:#}"));
                self.checkpoint(app, true);
                app.refresh_tasks();
                self.after_turn(app, TurnStatus::Failed, Some(format!("{error:#}")));
                return;
            }
        };
        // The Manager has a role of its own and only the app's tools; a task
        // is Bubble's coding agent in its project, told what the task is.
        let (system_prompt, append_system_prompt, cwd, host_tools, agent_name) = match &task_id {
            None => {
                // The main bot reads the team's notes and keeps them.
                let notes = crate::memory::prompt(&Sender::Main, None);
                let prompt = if notes.is_empty() { manager::PROMPT.to_owned() } else { format!("{}\n\n{notes}", manager::PROMPT) };
                let mut tools = manager::specs();
                tools.extend(crate::memory::specs());
                (Some(with_instructions(prompt, None)), None, None, tools, None)
            }
            Some(id) => {
                let task = app.with_store(|s| s.get_task(id)).flatten();
                let title = task.as_ref().map(|t| t.title.clone()).unwrap_or_default();
                // Where the work at hand is: the folder its ticket named,
                // or the one the person chose.
                let project = task.and_then(|t| t.project_cwd);
                // A second bot in one repository works in a checkout of
                // its own, so neither changes files under the other.
                let place = project.as_deref().map(PathBuf::from).filter(|p| p.is_dir()).map(|folder| {
                    let at_work: Vec<String> = app
                        .bots_untracked()
                        .into_iter()
                        .filter(|bot| matches!(bot.state, TaskState::Running | TaskState::NeedsYou))
                        .map(|bot| bot.task.id)
                        .collect();
                    crate::worktree::place_for(app, id, &title, &folder, &at_work)
                });
                let apart = place.as_ref().and_then(|place| place.note.clone());
                let cwd = place.map(|place| place.folder);
                let mut tools = if cwd.is_some() { github::task_tools(app, project.as_deref()) } else { Vec::new() };
                tools.extend(manager::bot_specs());
                tools.extend(crate::automations::specs());
                tools.extend(crate::memory::specs());
                let full = app.task_full_access(id) && self.driven_by_person(app, &run.id);
                let instruction = app.bot_instruction(id);
                // The agent goes by the bot's name, not the main bot's.
                let name = Some(title.clone()).filter(|t| !t.trim().is_empty());
                let here = cwd.as_ref().map(|folder| folder.to_string_lossy().into_owned()).or_else(|| project.clone());
                let mut prompt = task_prompt(&title, &app.bot_role(id), &instruction, here.as_deref(), cwd.is_some(), full);
                if let Some(apart) = apart {
                    prompt.push_str("\n\n");
                    prompt.push_str(&apart);
                }
                prompt.push_str("\n\n");
                prompt.push_str(crate::memory::GUIDANCE);
                let notes = crate::memory::prompt(&Sender::Bot(id.clone()), project.as_deref().filter(|_| cwd.is_some()));
                if !notes.is_empty() {
                    prompt.push_str("\n\n");
                    prompt.push_str(&notes);
                }
                (None, Some(prompt), cwd, tools, name)
            }
        };
        // The newest message is what the agent answers; it keeps the rest
        // itself, in the run's session.
        let stored = app.with_store(|s| s.messages(&run.id)).unwrap_or_default();
        let (text, pictures, recap) = prompt_of(&stored);
        app.with_store(|s| s.set_run_status(&run.id, RunStatus::Running, None));
        self.queued.set(false);
        self.busy.set(true);
        self.restore_turn_anchor(app);
        self.turn_status(TurnStatus::Working);
        self.checkpoint(app, true);
        app.refresh_tasks();
        self.changed();

        let turn = HostTurn {
            session_id: engine::session_of(&run.id),
            // Without a project folder there are no file or command tools,
            // and the session lives in the app's own folder. The main bot
            // only coordinates, and may ask the person; a bot that only
            // talks can still look things up.
            allowed_tools: match (&task_id, &cwd) {
                (None, _) => Some(vec!["question".to_owned()]),
                (Some(_), None) => Some(TALKING_TOOLS.iter().map(|tool| (*tool).to_owned()).collect()),
                (Some(_), Some(_)) => None,
            },
            // A conversation's session stays in one place for its whole
            // life (the folder an earlier run was made in, else the app's
            // own); each turn works where its work is.
            home: run.cwd.as_deref().map(PathBuf::from).filter(|p| p.is_dir()).unwrap_or_else(scratch_folder).to_string_lossy().into_owned(),
            cwd: cwd.unwrap_or_else(scratch_folder).to_string_lossy().into_owned(),
            text,
            pictures,
            model,
            thinking: thinking.as_str().to_owned(),
            agent_name,
            append_system_prompt,
            system_prompt,
            host_tools,
            recap,
        };
        let hosts = app.hosts.get();
        let stop = Arc::new(tokio::sync::Notify::new());
        self.abort.replace(Some(stop.clone()));
        let conversation = *self;
        let run_id = run.id.clone();
        // The turn belongs to the app, not to the screen that started it.
        app.root.run(move || {
            spawn(async move {
                let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel::<TurnEvent>();
                let job = background(async move {
                    let host = hosts.get().await?;
                    engine::run(host, turn, sender, stop).await
                });
                let mut step = 0usize;
                while let Some(event) = receiver.recv().await {
                    match event {
                        TurnEvent::Agent(event) => conversation.apply(app, &run_id, event, &mut step),
                        TurnEvent::Compacted => conversation.note_compaction(),
                        TurnEvent::Retried => conversation.drop_pending_reply(),
                        TurnEvent::Question { call_id, asks, respond } => {
                            conversation.ask_person(app, &run_id, call_id.as_deref(), asks, respond)
                        }
                        TurnEvent::Subagent { tool, update } => conversation.subagent_update(app, &tool, update),
                    }
                }
                let joined = job.await;
                conversation.abort.replace(None);
                app.refresh_servers();
                let (stopped, result) = match joined {
                    Ok(Ok(Ending::Stopped)) => (true, Ok(())),
                    Ok(Ok(Ending::Completed)) => (false, Ok(())),
                    Ok(Err(error)) => (false, Err(error)),
                    Err(error) => (false, Err(anyhow!("the turn stopped: {error}"))),
                };
                let pending = conversation.pending.get();
                let partial_message = if stopped || result.is_err() {
                    let (partial, reasoning) = conversation.lines.with_untracked(|lines| (
                        lines.iter().find(|l| l.id == pending).map(|l| l.text.clone()).unwrap_or_default(),
                        lines.iter().find(|l| l.id == format!("{pending}:reasoning")).map(|l| l.reasoning.clone()).unwrap_or_default(),
                    ));
                    (!partial.is_empty() || !reasoning.is_empty()).then(|| ChatMessage::Assistant {
                        text: partial, reasoning, tool_calls: Vec::new(), state: serde_json::Value::Null,
                    })
                } else { None };
                let ending = if stopped { TurnStatus::Stopped } else if result.is_err() { TurnStatus::Failed } else { TurnStatus::Completed };
                let failure = result.as_ref().err().map(|error| format!("{error:#}"));
                let turn = conversation.active_turn.get();
                conversation.lines.update(|lines| transcript::finish(lines, &turn, ending, bubble_store::now_ms()));
                match result {
                    _ if stopped => {
                        app.with_store(|s| s.set_run_status(&run_id, RunStatus::Idle, None));
                    }
                    Ok(()) => {
                        app.with_store(|s| s.set_run_status(&run_id, RunStatus::Idle, None));
                    }
                    Err(error) => {
                        let message = format!("{error:#}");
                        app.with_store(|s| s.set_run_status(&run_id, RunStatus::Failed, Some(&message)));
                        conversation.push(Line::new(
                            format!("{run_id}-error-{}", bubble_store::now_ms()),
                            LineKind::Failure,
                            message,
                        ));
                    }
                }
                if let Some(message @ ChatMessage::Assistant { .. }) = partial_message {
                    let ChatMessage::Assistant { text, .. } = &message else { unreachable!() };
                    let checkpoint = conversation.transcript_checkpoint();
                    app.with_store(|s| s.append_message_and_transcript(&run_id, MessageRole::Assistant, text,
                        serde_json::to_string(&message).ok().as_deref(), checkpoint.as_ref().map(|(a, c)| (a.as_str(), c.as_str()))));
                } else {
                    conversation.checkpoint(app, true);
                }
                conversation.pending.replace(String::new());
                conversation.approvals.with(|map| map.clear());
                conversation.questions.with(|map| map.clear());
                conversation.busy.set(false);
                app.refresh_tasks();
                conversation.changed();
                // What the person said that should last is kept in the bot's notes.
                if ending == TurnStatus::Completed {
                    if let Some(bot) = conversation.task_id.get() {
                        if let Some((said, answered)) = conversation.last_exchange_with_person() {
                            let model = app.task_model_of(&bot);
                            crate::memory::review(app, bot, said, answered, model);
                        }
                    }
                }
                conversation.after_turn(app, ending, failure);
                app.pump_queue();
            });
        });
    }

    /// A subagent moved on: kept for the panel under the call that
    /// started it, and across restarts of the app.
    fn subagent_update(&self, app: AppState, tool: &str, update: bubble_host::SubagentUpdate) {
        use delegate::{Snapshot, Status};
        let status = match update.status.as_str() {
            "queued" => Status::Queued,
            "completed" => Status::Completed,
            "failed" => Status::Failed,
            "blocked" => Status::Blocked,
            "cancelled" => Status::Cancelled,
            _ => Status::Running,
        };
        let mut settled = false;
        let mut known = true;
        self.subagents.update(|children| {
            let index = match children.iter().position(|c| c.id == update.sub_agent_id) {
                Some(index) => index,
                None => {
                    known = false;
                    // What it was asked is the title of the call that started it.
                    let asked = self.lines.with_untracked(|lines| {
                        lines
                            .iter()
                            .rev()
                            .find(|l| l.call_id.as_deref() == Some(update.parent_tool_call_id.as_str()))
                            .map(|l| l.text.strip_prefix("Started a subagent: ").unwrap_or(&l.text).to_owned())
                    });
                    let workflow = tool == "run_workflow";
                    let route = update.route.as_ref();
                    let part = |key: &str| route.and_then(|r| r.get(key)).and_then(|v| v.as_str()).unwrap_or_default().to_owned();
                    children.push(Snapshot {
                        id: update.sub_agent_id.clone(),
                        parent_call_id: Some(update.parent_tool_call_id.clone()),
                        nickname: update.nickname.clone().unwrap_or_else(|| update.agent_name.clone()),
                        profile: update.agent_name.clone(),
                        task: if workflow { String::new() } else { asked.clone().unwrap_or_default() },
                        description: if workflow { None } else { asked },
                        status,
                        model: match (part("providerId"), part("model")) {
                            (provider, model) if !provider.is_empty() && !model.is_empty() => format!("{provider}:{model}"),
                            (_, model) => model,
                        },
                        summary: String::new(),
                        notes: Vec::new(),
                        error: None,
                        workflow,
                        pending_inputs: 0,
                        category: update.category.clone(),
                        tokens: 0,
                        worktree: None,
                    });
                    children.len() - 1
                }
            };
            let child = &mut children[index];
            settled = child.status != status;
            child.status = status;
            match update.child_event.as_deref() {
                Some(bubble_host::Event::ToolStart { id, name, args }) => {
                    let call = bubble_core::ToolCall { id: id.clone(), name: name.clone(), arguments: args.to_string() };
                    child.notes.push(bubble_core::tools::describe(&call));
                    if child.notes.len() > 20 {
                        child.notes.remove(0);
                    }
                }
                Some(bubble_host::Event::TurnEnd { usage: Some(usage), .. }) => {
                    child.tokens += usage.prompt_tokens + usage.completion_tokens;
                }
                // A new reply replaces what the last one said.
                Some(bubble_host::Event::TurnStart) => child.summary.clear(),
                _ => {}
            }
            if let Some(delta) = &update.summary_delta {
                child.summary.push_str(delta);
            }
            match (status, &update.message) {
                (Status::Completed, Some(message)) if !message.trim().is_empty() => child.summary = message.clone(),
                (Status::Failed | Status::Blocked | Status::Cancelled, Some(message)) => child.error = Some(message.clone()),
                _ => {}
            }
        });
        if !known {
            let children = self.subagents.get_untracked();
            self.lines.update(|lines| transcript::anchor_children(lines, &children));
        }
        if !known || settled {
            if let Some(task_id) = self.task_id.get() {
                let kept = self.subagents.with_untracked(|children| serde_json::to_string(children).ok());
                app.with_store(|s| s.set_setting(&subagents_key(&task_id), kept.as_deref()));
            }
            self.checkpoint(app, true);
            self.changed();
        }
    }

    /// The model's reply broke off and the agent asks for it again: what
    /// it had written so far is not part of the answer, and goes.
    fn drop_pending_reply(&self) {
        let pending = self.pending.get();
        if pending.is_empty() {
            return;
        }
        let thought = format!("{pending}:reasoning");
        self.lines.update(|lines| lines.retain(|l| l.id != pending && l.id != thought));
        self.pending.replace(String::new());
        self.changed();
    }

    /// The agent summarized the conversation so far to stay within the
    /// model's context; shown before the reply in progress.
    fn note_compaction(&self) {
        let pending = self.pending.get();
        let note = Line::new(format!("checkpoint-{}", bubble_store::now_ms()), LineKind::Notice, CHECKPOINT_NOTE).in_turn(&self.active_turn.get());
        self.lines.update(|lines| {
            let at = lines.iter().position(|l| l.id == format!("{pending}:reasoning")).unwrap_or(lines.len());
            lines.insert(at, note);
        });
        self.changed();
    }

    /// Shows and keeps what the turn reports.
    fn apply(&self, app: AppState, run_id: &str, event: AgentEvent, step: &mut usize) {
        match event {
            AgentEvent::ModelStarted => {
                self.begin_model(step);
                self.checkpoint(app, true);
                self.changed();
            }
            AgentEvent::Text(piece) => {
                let pending = self.pending.get();
                let turn = self.active_turn.get();
                self.lines.update(|lines| transcript::text(lines, &turn, &pending, &piece));
                self.checkpoint(app, false);
                self.changed();
            }
            AgentEvent::Reasoning(piece) => {
                let pending = self.pending.get();
                self.lines.update(|lines| transcript::reasoning(lines, &pending, &piece));
                self.checkpoint(app, false);
                self.changed();
            }
            AgentEvent::Compacted(history) => {
                app.with_store(|s| {
                    s.append_message_with(
                        run_id,
                        MessageRole::Checkpoint,
                        CHECKPOINT_NOTE,
                        serde_json::to_string(&history).ok().as_deref(),
                    )
                });
                // Shown before the reply in progress.
                let pending = self.pending.get();
                let note = Line::new(format!("checkpoint-{}", bubble_store::now_ms()), LineKind::Notice, CHECKPOINT_NOTE).in_turn(&self.active_turn.get());
                self.lines.update(|lines| {
                    let at = lines.iter().position(|l| l.id == format!("{pending}:reasoning")).unwrap_or(lines.len());
                    lines.insert(at, note);
                });
                self.changed();
            }
            AgentEvent::Assistant(message) => {
                let ChatMessage::Assistant { text, .. } = &message else { return };
                let pending = if self.pending.get().is_empty() { self.begin_model(step) } else { self.pending.get() };
                let turn = self.active_turn.get();
                self.lines.update(|lines| {
                    transcript::assistant(lines, &turn, &pending, &message);
                });
                let checkpoint = self.transcript_checkpoint();
                app.with_store(|s| s.append_message_and_transcript(run_id, MessageRole::Assistant, text,
                    serde_json::to_string(&message).ok().as_deref(), checkpoint.as_ref().map(|(a, c)| (a.as_str(), c.as_str()))));
                self.pending.replace(String::new());
                self.changed();
            }
            AgentEvent::ToolStarted { call, .. } => {
                let turn = self.active_turn.get();
                self.lines.update(|lines| {
                    if let Some(line) = transcript::tool_mut(lines, &turn, &call.id) {
                        if let LineKind::Tool { status, .. } = &mut line.kind { *status = ToolStatus::Running; }
                    }
                });
                self.checkpoint(app, true);
                self.changed();
            }
            AgentEvent::HostCall { call, respond } => {
                if github::is_task_tool(&call.name) {
                    github::handle_call(app, *self, run_id, self.task_id.get(), call, respond);
                } else {
                    let caller = match self.task_id.get() {
                        Some(id) => Sender::Bot(id),
                        None => Sender::Main,
                    };
                    let output = match &caller {
                        _ if crate::memory::is_memory_tool(&call.name) => crate::memory::execute(app, &caller, &call),
                        Sender::Bot(id) if crate::automations::is_routine_tool(&call.name) => crate::automations::execute(app, id, &call),
                        _ => manager::execute(app, &caller, &call),
                    };
                    let _ = respond.send(output);
                }
            }
            AgentEvent::ApprovalNeeded { call, permission, respond } => {
                // Full access, read at each call so switching it applies to
                // the next one; only while the person drives the run.
                let full = self
                    .task_id
                    .get()
                    .is_some_and(|id| app.task_full_access(&id) && self.driven_by_person(app, run_id));
                if full {
                    let _ = respond.send(Decision::AllowOnce);
                } else {
                    self.ask_with(app, run_id, &call.id, permission, respond);
                }
            }
            AgentEvent::ToolOutputDelta { call_id, chunk } => {
                let turn = self.active_turn.get();
                self.lines.update(|lines| transcript::live_output(lines, &turn, &call_id, &chunk));
                self.changed();
            }
            AgentEvent::ToolFinished { call_id, output, denied } => {
                let status = if denied {
                    ToolStatus::Denied
                } else if output.is_error {
                    ToolStatus::Failed
                } else {
                    ToolStatus::Done
                };
                let turn = self.active_turn.get();
                self.lines.update(|lines| {
                    if let Some(line) = transcript::tool_mut(lines, &turn, &call_id) {
                        if let LineKind::Tool { name, .. } = &line.kind {
                            line.kind = LineKind::Tool { name: name.clone(), status, output: output.content };
                        }
                        line.live.clear();
                    }
                });
                self.checkpoint(app, true);
                self.changed();
            }
            AgentEvent::ToolResults(message) => {
                app.with_store(|s| {
                    s.append_message_with(run_id, MessageRole::Tool, "", serde_json::to_string(&message).ok().as_deref())
                });
            }
            AgentEvent::Usage(_) => {}
        }
    }

    /// Whether the person (not GitHub or the Manager) sent the run's latest
    /// message, the one the agent is working on now.
    fn driven_by_person(&self, app: AppState, run_id: &str) -> bool {
        app.with_store(|s| s.messages(run_id)).is_some_and(|messages| person_spoke_last(&messages))
    }

    /// Full access was turned on: the questions waiting in a run the
    /// person is driving are allowed.
    fn allow_waiting(&self, app: AppState) {
        let Some(run) = self.current_run(app) else { return };
        if !self.driven_by_person(app, &run.id) {
            return;
        }
        let waiting: Vec<String> = self.approvals.with(|map| map.keys().cloned().collect());
        for id in waiting {
            self.decide(app, &id, Decision::AllowOnce);
        }
    }

    fn fail_now(&self, message: &str) {
        self.push(Line::new(format!("notice-{}", bubble_store::now_ms()), LineKind::Failure, message));
        self.changed();
    }
}

const CHECKPOINT_NOTE: &str = "Earlier messages were summarized to stay within the model's context";

/// A system prompt with the person's AGENTS.md instructions after it.
fn with_instructions(prompt: String, cwd: Option<&std::path::Path>) -> String {
    match bubble_core::instructions::instructions(cwd) {
        Some(instructions) => format!("{prompt}\n\n{instructions}"),
        None => prompt,
    }
}

/// What a bot without a project folder may use of the agent's own tools:
/// asking, looking things up, its memory and skills. Nothing that reads or
/// changes files or runs commands.
const TALKING_TOOLS: [&str; 7] = ["question", "web_search", "web_fetch", "memory", "skill", "skill_search", "tool_search"];

/// Where a conversation without a project folder keeps its session.
pub(crate) fn scratch_folder() -> PathBuf {
    let folder = bubble_home().join("native").join("manager");
    std::fs::create_dir_all(&folder).ok();
    folder
}

/// What the agent is handed for a turn: the newest message that sets it
/// going (its text as the model should read it, and its pictures), and a
/// recap of what was said before, for a run the agent has no session of.
fn prompt_of(messages: &[Message]) -> (String, Vec<bubble_host::Picture>, Option<String>) {
    let opener = messages.iter().rposition(|m| matches!(m.role, MessageRole::User | MessageRole::System | MessageRole::External));
    let Some(index) = opener else { return (String::new(), Vec::new(), None) };
    let message = &messages[index];
    let (text, pictures) = match message.content.as_deref().and_then(|c| serde_json::from_str::<ChatMessage>(c).ok()) {
        Some(ChatMessage::User { text, images }) => (
            text,
            images.into_iter().map(|image| bubble_host::Picture { media_type: image.media_type, data: image.data }).collect(),
        ),
        _ => (message.text.clone(), Vec::new()),
    };
    (text, pictures, engine::recap_of(&history_of(&messages[..index])))
}

fn task_prompt(name: &str, role: &str, instruction: &str, project: Option<&str>, has_tools: bool, full: bool) -> String {
    let place = match (project, has_tools) {
        (Some(path), true) => format!(
            "Right now you work in the project folder {path}: the work at hand is there. Use the tools to read, search, change \
             files and run commands there; relative paths start in that folder. You are not tied to it: other work may come \
             with another folder, and what you did in earlier projects stays in this conversation. {}",
            if full {
                "The person gave this bot full access: changes and commands go ahead without asking. \
                 Say what a risky step will do before you take it, prefer reversible steps, and never push, \
                 delete or expose credentials just because nothing asks."
            } else {
                "Commands, and changes outside the project, wait for the person's yes."
            }
        ),
        (Some(path), false) => format!("The project folder {path} cannot be found, so you have no tools; say what you would do."),
        (None, _) => "The work at hand names no project folder, so you have no file or command tools now; when work needs \
                      files, say which folder it is in and what you need."
            .to_owned(),
    };
    let purpose = match instruction.trim() {
        "" => String::new(),
        instruction => format!(" What you are for:\n{instruction}\n"),
    };
    let role = match role.trim() {
        "" => String::new(),
        role => format!(" Your role on the team: {role}."),
    };
    format!(
        "You are \"{name}\", one of the person's Bubble bots: a long-lived member of their team with one continuing \
         conversation.{role}{purpose} {place} \
         Messages may come from the person, from the main bot, which coordinates the bots, or from GitHub (reviews, comments, checks). \
         Work until what was asked is done or you need the person, then say briefly what you did and what is left. \
         Answer in the language the person writes in.\n\n{}",
        manager::BOT_TEAMWORK
    )
}

/// The model's view of stored messages. A tool call whose results never
/// came (the app quit mid-turn) gets an "interrupted" result, so the history
/// stays valid for the provider.
fn history_of(messages: &[Message]) -> Vec<ChatMessage> {
    bubble_store::history(messages)
}

/// The lines of `runs`, oldest first: messages, the tools each reply
/// called, where each later run begins and why a failed one stopped.
/// Who began a run, by its first message.
/// Whether the latest message that set the agent going (not its own
/// replies or tool results) came from the person.
fn person_spoke_last(messages: &[Message]) -> bool {
    messages
        .iter()
        .rev()
        .find(|m| matches!(m.role, MessageRole::User | MessageRole::System | MessageRole::External))
        .is_some_and(|m| m.role == MessageRole::User)
}

fn cause_of(role: MessageRole) -> &'static str {
    match role {
        MessageRole::External => "started by GitHub",
        MessageRole::System => "started by the Manager",
        _ => "started by you",
    }
}

/// What a run's divider says after "Run N".
fn run_label(cause: &str, model: Option<&str>) -> String {
    match model {
        Some(model) if !cause.contains(model) => format!("{cause}, on {model}"),
        _ => cause.to_owned(),
    }
}

/// Renames earlier lines that hold these ids, so a reused tool-call id
/// starts fresh lines.
fn retire(lines: &mut [Line], tool: &str, approval: &str) {
    let count = lines.len();
    for (index, line) in lines.iter_mut().enumerate() {
        if line.id == tool || line.id == approval {
            line.id = format!("{}~{index}of{count}", line.id);
        }
    }
}

fn lines_of(store: &Store, runs: &[Run]) -> anyhow::Result<Vec<Line>> {
    let mut lines: Vec<Line> = Vec::new();
    for (index, run) in runs.iter().enumerate() {
        if index > 0 {
            let cause = match store.setting(&run_cause_key(&run.id))? {
                Some(cause) => cause,
                None => {
                    let first = store.messages(&run.id)?.into_iter().map(|m| m.role).next();
                    cause_of(first.unwrap_or(MessageRole::User)).to_owned()
                }
            };
            let label = run_label(&cause, run.model.as_deref());
            lines.push(Line { at: run.created_at, ..Line::new(format!("{}-start", run.id), LineKind::RunStart(index + 1), label) });
        }
        for message in store.messages(&run.id)? {
            if message.role == MessageRole::Checkpoint {
                lines.push(Line { at: message.created_at, ..Line::new(message.id.clone(), LineKind::Notice, CHECKPOINT_NOTE) });
                continue;
            }
            let parsed = message.content.as_deref().and_then(|c| serde_json::from_str::<ChatMessage>(c).ok());
            match parsed {
                Some(ChatMessage::Assistant { text, tool_calls, reasoning, .. }) => {
                    if !text.trim().is_empty() || !reasoning.trim().is_empty() {
                        lines.push(Line {
                            at: message.created_at,
                            reasoning,
                            ..Line::new(message.id.clone(), LineKind::Message(MessageRole::Assistant), text)
                        });
                    }
                    for call in &tool_calls {
                        retire(&mut lines, &format!("tool-{}", call.id), &format!("approval-{}", call.id));
                        lines.push(Line {
                            at: message.created_at,
                            ..Line::new(
                                format!("tool-{}", call.id),
                                LineKind::Tool {
                                    name: call.name.clone(),
                                    status: ToolStatus::Failed,
                                    output: "Interrupted before it ran.".into(),
                                },
                                bubble_core::tools::describe(call),
                            )
                        });
                    }
                }
                Some(ChatMessage::Tool { results }) => {
                    for result in results {
                        let status = if result.content.starts_with("The person declined") {
                            ToolStatus::Denied
                        } else if result.is_error {
                            ToolStatus::Failed
                        } else {
                            ToolStatus::Done
                        };
                        if let Some(line) = lines.iter_mut().find(|l| l.id == format!("tool-{}", result.call_id)) {
                            if let LineKind::Tool { name, .. } = &line.kind {
                                line.kind = LineKind::Tool { name: name.clone(), status, output: result.content };
                            }
                        }
                    }
                }
                other => {
                    let images = match &other {
                        Some(ChatMessage::User { images, .. }) => images.len(),
                        _ => 0,
                    };
                    if message.role != MessageRole::Tool && (!message.text.trim().is_empty() || images > 0) {
                        lines.push(Line {
                            at: message.created_at,
                            images,
                            ..Line::new(message.id.clone(), LineKind::Message(message.role), message.text.clone())
                        });
                    }
                }
            }
        }
        if run.status == RunStatus::Failed {
            let reason = run.last_error.clone().unwrap_or_else(|| "The run stopped.".to_owned());
            lines.push(Line { at: run.updated_at, ..Line::new(format!("{}-error", run.id), LineKind::Failure, reason) });
        }
    }
    for run in runs { transcript::overlay(&mut lines, store.turn_transcripts(&run.id)?); }
    Ok(lines)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bubble_core::ToolCall;

    #[test]
    fn completed_stopped_and_failed_transcripts_restore_the_same_order_state_and_duration() {
        use crate::transcript::Phase;
        let store = Store::open_in_memory().unwrap();
        let run = store.manager_run(None).unwrap();
        for status in [TurnStatus::Completed, TurnStatus::Stopped, TurnStatus::Failed] {
            let prompt = store.append_message(&run.id, MessageRole::User, "Inspect this").unwrap();
            let turn = format!("turn-{}", prompt.id);
            let mut lines = vec![Line { at: prompt.created_at, ..Line::new(&prompt.id, LineKind::Message(MessageRole::User), &prompt.text) }];
            transcript::begin_turn(&mut lines, &turn, prompt.created_at, TurnStatus::Working);
            let id = transcript::begin_model(&mut lines, &turn, 1);
            let commentary = ChatMessage::Assistant { text: "I will inspect it.".into(), reasoning: "**Inspecting**".into(),
                tool_calls: vec![ToolCall { id: "reused".into(), name: "read".into(), arguments: "{\"path\":\"a\"}".into() }],
                state: serde_json::Value::Null };
            transcript::assistant(&mut lines, &turn, &id, &commentary);
            store.append_message_with(&run.id, MessageRole::Assistant, "I will inspect it.", Some(&serde_json::to_string(&commentary).unwrap())).unwrap();
            if let LineKind::Tool { status, .. } = &mut transcript::tool_mut(&mut lines, &turn, "reused").unwrap().kind { *status = ToolStatus::Done; }
            let id = transcript::begin_model(&mut lines, &turn, 2);
            transcript::reasoning(&mut lines, &id, "**Summarizing**");
            transcript::text(&mut lines, &turn, &id, "Here is the result");
            if status == TurnStatus::Completed { lines.iter_mut().find(|l| l.id == id).unwrap().phase = Phase::Final; }
            transcript::finish(&mut lines, &turn, status, prompt.created_at + 17_500);
            let saved = serde_json::to_string(&lines[1..]).unwrap();
            store.save_turn_transcript(&run.id, &prompt.id, &saved).unwrap();
            let restored = lines_of(&store, std::slice::from_ref(&run)).unwrap();
            let start = restored.iter().position(|l| l.id == prompt.id).unwrap();
            assert_eq!(&restored[start..], &lines, "{status:?}");
            assert_eq!(crate::trace::layout(&restored[start..], false), crate::trace::layout(&lines, false));
        }
        // Later CLI history without presentation metadata remains intact.
        let prompt = store.append_message(&run.id, MessageRole::User, "From CLI").unwrap();
        store.append_message(&run.id, MessageRole::Assistant, "CLI reply").unwrap();
        let restored = lines_of(&store, &[run]).unwrap();
        assert!(restored.iter().any(|l| l.id == prompt.id));
        assert_eq!(restored.last().unwrap().text, "CLI reply");
    }

    fn message(role: MessageRole, content: Option<ChatMessage>) -> Message {
        Message {
            id: "m".into(),
            run_id: "r".into(),
            seq: 1,
            role,
            text: String::new(),
            content: content.map(|c| serde_json::to_string(&c).unwrap()),
            created_at: 0,
        }
    }

    #[test]
    fn full_access_follows_who_spoke_last() {
        use MessageRole::*;
        let run = |roles: &[MessageRole]| roles.iter().map(|r| message(*r, None)).collect::<Vec<_>>();
        // The Manager started the run; the person's reply puts them in charge.
        assert!(!person_spoke_last(&run(&[System, Assistant, Tool])));
        assert!(person_spoke_last(&run(&[System, Assistant, User, Assistant, Tool])));
        // GitHub speaking after the person hands the run back to asking.
        assert!(!person_spoke_last(&run(&[User, Assistant, External, Assistant])));
        assert!(!person_spoke_last(&[]));
    }

    #[test]
    fn calls_without_results_are_closed() {
        let call = ToolCall { id: "c1".into(), name: "bash".into(), arguments: "{}".into() };
        let messages = vec![
            message(MessageRole::User, Some(ChatMessage::user("go"))),
            message(
                MessageRole::Assistant,
                Some(ChatMessage::Assistant { text: String::new(), reasoning: String::new(), tool_calls: vec![call], state: serde_json::Value::Null }),
            ),
        ];
        let history = history_of(&messages);
        assert_eq!(history.len(), 3);
        assert!(matches!(&history[2], ChatMessage::Tool { results } if results[0].call_id == "c1" && results[0].is_error));
    }

    #[test]
    fn old_text_only_messages_still_count() {
        let mut user = message(MessageRole::User, None);
        user.text = "hello".into();
        let mut reply = message(MessageRole::Assistant, None);
        reply.text = "hi".into();
        assert_eq!(history_of(&[user, reply]), [ChatMessage::user("hello"), ChatMessage::assistant("hi")]);
    }

    #[test]
    fn a_checkpoint_replaces_what_came_before_it() {
        let old = message(MessageRole::User, Some(ChatMessage::user("a very long start")));
        let mut checkpoint = message(MessageRole::Checkpoint, None);
        checkpoint.content = Some(serde_json::to_string(&vec![ChatMessage::user("summary")]).unwrap());
        let after = message(MessageRole::User, Some(ChatMessage::user("go on")));
        assert_eq!(history_of(&[old, checkpoint, after]), [ChatMessage::user("summary"), ChatMessage::user("go on")]);
    }

    #[test]
    fn a_brief_from_the_manager_reaches_the_model_as_a_user_message() {
        let brief = message(MessageRole::System, Some(ChatMessage::user("Write the notes")));
        assert_eq!(history_of(&[brief]), [ChatMessage::user("Write the notes")]);
    }
}

fn dirs_home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default()
}
