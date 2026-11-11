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
use bubble_core::mcp::{self, Hub};
use bubble_core::skills::Skills;
use bubble_core::{
    AgentEvent, ChatMessage, Config, Decision, Level as ThinkingLevel, Permission, bubble_home, http_client,
};
use bubble_github::Gh;
use bubble_store::{
    Lifecycle, Message, MessageRole, NewRun, NewTask, Origin, Run, RunRole, RunStatus, Store, TaskState, TaskSummary,
};
use nsh_core::prelude::*;
use tokio::sync::oneshot;

use crate::engine::{self, Ending, HostTurn, Hosts, TurnEvent};
use crate::github::{self, Level, Repo};
use crate::manager;

const MANAGER_KEY: &str = "manager";

/// Settings keys in the store.
pub const MANAGER_MODEL: &str = "manager_model";
pub const TASK_MODEL: &str = "task_model";
pub const MAX_RUNS: &str = "max_concurrent_runs";
pub const DEFAULT_MAX_RUNS: u32 = 4;
pub const THINKING: &str = "thinking_level";
/// "system", "light" or "dark".
pub const APPEARANCE: &str = "appearance";
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
    Task(String),
    PullRequests,
    Automations,
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
    /// MCP connections, shared by every task.
    hub: Stored<Arc<Hub>>,
    pub mcp_servers: Signal<Vec<ServerRow>>,
    pub skills: Signal<SkillsSummary>,
    /// Where persistent memory stands.
    pub memory_status: Signal<String>,
    /// The person's hooks (settings.json), as Settings shows them.
    pub hook_rules: Signal<Vec<String>>,
    pub route: Signal<Route>,
    pub tasks: Signal<Vec<TaskSummary>>,
    /// GitHub events since the person last wrote to the Manager (Home's
    /// "while you were away").
    pub away: Signal<Vec<bubble_store::Event>>,
    /// The Tasks page's new-task form is open.
    /// Text to put in the Manager's composer once it shows (a project the
    /// person picked to start work in).
    pub manager_draft: Signal<Option<String>>,
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
                Some(store)
            }
            Err(error) => {
                problems.push(format!("{}: {error:#}", path.display()));
                None
            }
        };
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
            hub: stored(Arc::new(Hub::new())),
            mcp_servers: signal(Vec::new()),
            skills: signal(SkillsSummary::default()),
            memory_status: signal(String::new()),
            hook_rules: signal(Vec::new()),
            problem: signal((!problems.is_empty()).then(|| problems.join("\n"))),
            route: signal(Route::Home),
            tasks: signal(Vec::new()),
            away: signal(Vec::new()),
            manager_draft: signal(None),
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
        state.refresh_away();
        state.resume_queue();
        github::start(state);
        crate::automations::start(state);
        state.discover_models();
        state.start_tools();
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

    fn start_tools(&self) {
        let app = *self;
        let home = bubble_home();
        let (paths, disabled) = self.config.with(|c| (c.skill_paths.clone(), c.skills.disabled.clone()));
        let roots = Skills::roots(&home, None, &paths);
        let skills = Skills::discover(&roots, &disabled);
        let user_home = dirs_home();
        let mut rows: Vec<SkillRow> = skills
            .all()
            .iter()
            .map(|skill| SkillRow {
                name: skill.name.clone(),
                description: skill.description.clone(),
                source: skill.source.label(),
                folder: skill.root.display().to_string(),
                manual_only: skill.manual_only,
            })
            .collect();
        rows.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        self.skill_list.set(rows);
        self.skills.set(SkillsSummary {
            count: skills.visible().count(),
            folders: roots
                .iter()
                .filter(|(path, _)| path.is_dir())
                .map(|(path, _)| match path.strip_prefix(&user_home) {
                    Ok(rest) => format!("~/{}", rest.display()),
                    Err(_) => path.display().to_string(),
                })
                .collect(),
            problems: skills.diagnostics.len(),
            first_problem: skills.diagnostics.first().map(|d| match d.split_once("SKILL.md: ") {
                Some((path, why)) => format!("{why} in {}", path.trim_end_matches('/').rsplit('/').next().unwrap_or_default()),
                None => d.clone(),
            }),
        });
        let (configured, diagnostics) = mcp::load_config(&home, None);
        for diagnostic in diagnostics {
            log::warn!("mcp: {diagnostic}");
        }
        let hub = self.hub.get();
        hub.servers_for(configured.clone());
        self.refresh_servers();
        spawn(async move {
            let _ = background(async move {
                let client = http_client()?;
                hub.connect(configured, &client).await;
                Ok::<_, anyhow::Error>(())
            })
            .await;
            app.refresh_servers();
        });
    }

    /// Shows the MCP servers as they stand.
    pub fn refresh_servers(&self) {
        let rows = self
            .hub
            .get()
            .servers()
            .iter()
            .map(|server| {
                let (status, failed) = match server.status() {
                    mcp::Status::NotStarted | mcp::Status::Connecting => ("Connecting…".to_owned(), false),
                    mcp::Status::Connected { tools, .. } => {
                        (format!("{} tool{}", tools.len(), if tools.len() == 1 { "" } else { "s" }), false)
                    }
                    mcp::Status::Failed(error) => (error.lines().next().unwrap_or_default().to_owned(), true),
                };
                ServerRow { name: server.name.clone(), scope: server.scope.label(), status, failed }
            })
            .collect();
        self.mcp_servers.set(rows);
    }

    /// Tries a failed server again.
    /// Signs in to an MCP server that needs it, in the browser.
    pub fn sign_in_server(&self, name: &str) {
        let app = *self;
        let servers: Vec<_> = self.hub.get().servers().into_iter().filter(|s| s.name == name).collect();
        spawn(async move {
            let _ = background(async move {
                let client = http_client()?;
                for server in servers {
                    let pending = server.login(&client).await?;
                    bubble_core::login::open_browser(&pending.url);
                    pending.finish(&client).await?;
                    server.reset().await;
                    server.ensure(&client).await;
                }
                Ok::<_, anyhow::Error>(())
            })
            .await;
            app.refresh_servers();
        });
        self.mcp_servers.update(|rows| {
            for row in rows.iter_mut().filter(|r| r.name == name) {
                row.status = "Finish signing in in your browser…".into();
                row.failed = false;
            }
        });
    }

    pub fn retry_server(&self, name: &str) {
        let app = *self;
        let servers: Vec<_> = self.hub.get().servers().into_iter().filter(|s| s.name == name).collect();
        spawn(async move {
            let _ = background(async move {
                let client = http_client()?;
                for server in servers {
                    server.reset().await;
                    server.ensure(&client).await;
                }
                Ok::<_, anyhow::Error>(())
            })
            .await;
            app.refresh_servers();
        });
        self.mcp_servers.update(|rows| {
            for row in rows.iter_mut().filter(|r| r.name == name) {
                row.status = "Connecting…".into();
                row.failed = false;
            }
        });
    }

    /// Asks each signed-in subscription for its models, then offers them.
    fn discover_models(&self) {
        use bubble_core::auth::{AuthFile, Subscription};
        let app = *self;
        let auth = AuthFile::default();
        let all = [Subscription::ChatGpt, Subscription::Grok];
        app.accounts.set(
            all.iter()
                .map(|s| {
                    let signed_in = auth.signed_in(*s);
                    Account {
                        subscription: *s,
                        signed_in,
                        status: if signed_in { "Signed in; looking up its models…" } else { "Not signed in" }.to_owned(),
                    }
                })
                .collect(),
        );
        app.models.set(app.config.with(|c| c.selectable_models()));
        let signed_in: Vec<Subscription> = all.into_iter().filter(|s| auth.signed_in(*s)).collect();
        for subscription in signed_in {
            spawn(async move {
                let found = background(async move {
                    let client = http_client()?;
                    bubble_core::discover::discover(&client, subscription).await
                })
                .await;
                let status = match found {
                    Ok(Ok(models)) => format!("Signed in; {} models", models.len()),
                    Ok(Err(error)) => format!("{error:#}"),
                    Err(error) => format!("{error}"),
                };
                app.set_account_status(subscription, status);
                app.models.set(app.config.with(|c| c.selectable_models()));
            });
        }
    }

    fn set_account_status(&self, subscription: bubble_core::auth::Subscription, status: String) {
        self.accounts.update(|accounts| {
            if let Some(account) = accounts.iter_mut().find(|a| a.subscription == subscription) {
                account.status = status;
            }
        });
    }

    /// Signs in to `subscription` in the browser (the Grok CLI's sign-in is
    /// taken when there is one).
    pub fn sign_in(&self, subscription: bubble_core::auth::Subscription) {
        use bubble_core::auth::{AuthFile, Subscription};
        let app = *self;
        app.set_account_status(subscription, "Finish signing in in your browser…".into());
        spawn(async move {
            let done = background(async move {
                let auth = AuthFile::default();
                let home = std::env::var_os("HOME").map(std::path::PathBuf::from).unwrap_or_default();
                if subscription == Subscription::Grok && bubble_core::login::import_grok_cli(&auth, &bubble_home(), &home)? {
                    return Ok(());
                }
                let pending = bubble_core::login::begin(subscription).await?;
                bubble_core::login::open_browser(&pending.url);
                pending.finish(&http_client()?, &auth).await.map(|_| ())
            })
            .await;
            match done {
                Ok(Ok(())) => app.discover_models(),
                Ok(Err(error)) => app.set_account_status(subscription, format!("{error:#}")),
                Err(error) => app.set_account_status(subscription, format!("{error}")),
            }
        });
    }

    pub fn sign_out(&self, subscription: bubble_core::auth::Subscription) {
        match bubble_core::auth::AuthFile::default().remove(subscription) {
            Ok(_) => self.discover_models(),
            Err(error) => self.set_account_status(subscription, format!("{error:#}")),
        }
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
        label_of(&reference, self.thinking.get())
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
        match reference.split_once(':') {
            Some((provider, model)) => bubble_core::model::info(provider, model).levels.to_vec(),
            None => vec![ThinkingLevel::Off],
        }
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
        projects_of(self.tasks.get())
    }

    /// `projects`, without subscribing to the task list.
    pub fn projects_untracked(&self) -> Vec<String> {
        projects_of(self.tasks.get_untracked())
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
            restored_children: stored(HashSet::new()),
            approvals: stored(HashMap::new()),
            pending: stored(String::new()),
            active_turn: stored(String::new()),
            active_run: stored(String::new()),
            checkpoint_at: stored(0),
            opened: signal(HashSet::new()),
            following: signal(true),
            abort: stored(None),
            on_change: stored(None),
        });
        self.conversations.with(|map| map.insert(key.to_owned(), conversation));
        // The children's progress, shown under the conversation.
        let children = conversation.children.get();
        // Children kept from before show at once.
        let snapshots = children.snapshots();
        conversation.lines.update(|lines| transcript::restore_child_anchors(lines, &snapshots));
        conversation.restored_children.replace(snapshots.iter().map(|c| c.id.clone()).collect());
        conversation.subagents.set(snapshots);
        let mut changes = children.subscribe();
        let app = *self;
        self.root.run(|| {
            spawn(async move {
                while changes.changed().await.is_ok() {
                    let snapshots = children.snapshots();
                    let restored = conversation.restored_children.get();
                    let new = snapshots.iter().filter(|c| !restored.contains(&c.id)).cloned().collect::<Vec<_>>();
                    conversation.lines.update(|lines| transcript::anchor_children(lines, &new));
                    conversation.subagents.set(snapshots);
                    conversation.checkpoint(app, true);
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

/// A model and the level it thinks at, as the composer shows them.
fn label_of(reference: &str, level: ThinkingLevel) -> String {
    match reference.split_once(':') {
        Some((provider, model)) => {
            let effective = bubble_core::model::effective_level(provider, model, level);
            if *bubble_core::model::info(provider, model).levels == [ThinkingLevel::Off] {
                reference.to_owned()
            } else {
                format!("{reference}  {}", effective.as_str())
            }
        }
        None => reference.to_owned(),
    }
}

fn projects_of(mut tasks: Vec<TaskSummary>) -> Vec<String> {
    tasks.sort_by(|a, b| b.task.updated_at.cmp(&a.task.updated_at));
    let mut projects: Vec<String> = Vec::new();
    for task in tasks {
        if let Some(cwd) = task.task.project_cwd {
            if !projects.contains(&cwd) {
                projects.push(cwd);
            }
        }
    }
    projects
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
    restored_children: Stored<HashSet<String>>,
    /// Disclosure choices belong to the conversation, including across page changes.
    pub opened: Signal<HashSet<String>>,
    pub following: Signal<bool>,
    /// Questions the agent waits on, by their line's id.
    approvals: Stored<HashMap<String, oneshot::Sender<Decision>>>,
    /// The line the next reply streams into.
    pending: Stored<String>,
    active_turn: Stored<String>,
    active_run: Stored<String>,
    checkpoint_at: Stored<i64>,
    /// Stops the running turn.
    abort: Stored<Option<Arc<tokio::sync::Notify>>>,
    /// Called after the lines change, to keep the newest in view.
    on_change: Stored<Option<Rc<dyn Fn()>>>,
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
        if self.approvals.with(|map| map.is_empty()) {
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
        if let Err(message) = self.post_to(app, text, MessageRole::User, None, content) {
            self.fail_now(&message);
        }
    }

    /// Records a message for the agent (the person's, or a brief from the
    /// Manager) and starts the turn, or queues it when the limit is
    /// reached. `Ok(true)`: started; `Ok(false)`: queued.
    pub fn post(&self, app: AppState, text: String, role: MessageRole) -> Result<bool, String> {
        self.post_to(app, text, role, None, None)
    }

    /// Runs the task again from its first message, in a new run on
    /// `model` (which becomes the task's own).
    pub fn rerun(&self, app: AppState, model: String) -> Result<bool, String> {
        let Some(task_id) = self.task_id.get() else { return Err("Only a task can be rerun.".into()) };
        if self.busy.get_untracked() {
            return Err("Stop the agent before running the task again.".into());
        }
        let first = app
            .with_store(|s| {
                let runs = s.runs_for_task(&task_id)?;
                let Some(run) = runs.first() else { return Ok(None) };
                Ok(s.messages(&run.id)?
                    .into_iter()
                    .find(|m| matches!(m.role, MessageRole::User | MessageRole::External | MessageRole::System)))
            })
            .flatten()
            .ok_or_else(|| "The task has nothing to run again yet.".to_owned())?;
        app.set_task_model(&task_id, Some(model.clone()));
        let content = first.content.as_deref().and_then(|c| serde_json::from_str::<ChatMessage>(c).ok());
        self.post_to(app, first.text, first.role, Some(format!("rerun on {model}")), content)
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
        app.config.with(|c| c.resolve(&model_reference)).map_err(|e| format!("{e:#}"))?;
        let start_now = task_id.is_none() || app.has_free_slot();

        // The run this message goes to: the Manager's, or the task's latest
        // one unless it failed (then a new run starts).
        let prepared = app.with_store(|s| -> anyhow::Result<(Run, Option<usize>)> {
            match &task_id {
                None => Ok((s.manager_run(Some(&model_reference))?, None)),
                Some(task_id) => {
                    let task = s.get_task(task_id)?.ok_or_else(|| anyhow!("the task is gone"))?;
                    let runs = s.runs_for_task(task_id)?;
                    let reusable =
                        runs.last().filter(|r| r.status != RunStatus::Failed && fresh.is_none()).cloned();
                    let (run, index) = match reusable {
                        Some(run) => (run, None),
                        None => {
                            let run = s.create_run(NewRun {
                                task_id: Some(task_id),
                                role: RunRole::Task,
                                model: Some(&model_reference),
                                cwd: task.project_cwd.as_deref(),
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
            .and_then(|reference| app.config.with(|c| c.resolve(&reference)).map(|_| reference));
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
                return;
            }
        };
        // The Manager has a role of its own and only the app's tools; a task
        // is Bubble's coding agent in its project, told what the task is.
        let (system_prompt, append_system_prompt, cwd, host_tools) = match &task_id {
            None => (Some(with_instructions(manager::PROMPT.to_owned(), None)), None, None, manager::specs()),
            Some(id) => {
                let task = app.with_store(|s| s.get_task(id)).flatten();
                let title = task.as_ref().map(|t| t.title.clone()).unwrap_or_default();
                let project = task.and_then(|t| t.project_cwd);
                let cwd = project.as_deref().map(PathBuf::from).filter(|p| p.is_dir());
                let tools = if cwd.is_some() { github::task_tools(app, project.as_deref()) } else { Vec::new() };
                let full = app.task_full_access(id) && self.driven_by_person(app, &run.id);
                (None, Some(task_prompt(&title, project.as_deref(), cwd.is_some(), full)), cwd, tools)
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
            // and the session lives in the app's own folder.
            allowed_tools: cwd.is_none().then(Vec::new),
            cwd: cwd.unwrap_or_else(scratch_folder).to_string_lossy().into_owned(),
            text,
            pictures,
            model,
            thinking: thinking.as_str().to_owned(),
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
                        TurnEvent::Subagent(_) => {}
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
                conversation.busy.set(false);
                app.refresh_tasks();
                conversation.changed();
                app.pump_queue();
            });
        });
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
                    let restored = self.restored_children.get();
                    let children = self.children.get().snapshots().into_iter().filter(|c| !restored.contains(&c.id)).collect::<Vec<_>>();
                    transcript::anchor_children(lines, &children);
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
                    let _ = respond.send(manager::execute(app, &call));
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
                let snapshots = self.children.get().snapshots();
                let restored = self.restored_children.get();
                let new = snapshots.iter().filter(|c| !restored.contains(&c.id)).cloned().collect::<Vec<_>>();
                self.lines.update(|lines| {
                    transcript::anchor_children(lines, &new);
                    if let Some(line) = transcript::tool_mut(lines, &turn, &call_id) {
                        if let LineKind::Tool { name, .. } = &line.kind {
                            line.kind = LineKind::Tool { name: name.clone(), status, output: output.content };
                        }
                        line.live.clear();
                    }
                });
                self.subagents.set(snapshots);
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

/// Where a conversation without a project folder keeps its session.
fn scratch_folder() -> PathBuf {
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

fn task_prompt(title: &str, project: Option<&str>, has_tools: bool, full: bool) -> String {
    let place = match (project, has_tools) {
        (Some(path), true) => format!(
            "You work in the project folder {path}. Use the tools to read, search, change files and run commands there; \
             relative paths start in that folder. {}",
            if full {
                "The person gave this task full access: changes and commands go ahead without asking. \
                 Say what a risky step will do before you take it, prefer reversible steps, and never push, \
                 delete or expose credentials just because nothing asks."
            } else {
                "Commands, and changes outside the project, wait for the person's yes."
            }
        ),
        (Some(path), false) => format!("The project folder {path} cannot be found, so you have no tools; say what you would do."),
        (None, _) => "The task has no project folder, so you have no tools; say what you would do and what you need.".to_owned(),
    };
    format!(
        "You are a Bubble agent working on one task: \"{title}\". {place} \
         Messages may come from the person, from the Manager, who hands out the work, or from GitHub (reviews, comments, checks). \
         Work until the task is done or you need the person, then say briefly what you did and what is left. \
         Answer in the language the person writes in."
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
