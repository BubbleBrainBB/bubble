//! The app's state: the store, the configuration, where the window is, and
//! the conversations (the Manager's on Home, each task's on its page).
//!
//! Conversations and the agent turns they run belong to the app's root,
//! not to a screen: leaving a task's page does not stop its agent. Task
//! turns beyond the "running at once" limit wait in a queue.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use anyhow::anyhow;
use bubble_core::delegate::{self, Children};
use bubble_core::hooks::Hooks;
use bubble_core::mcp::{self, Hub};
use bubble_core::skills::Skills;
use bubble_core::{
    AgentEvent, ChatMessage, Config, Decision, Level as ThinkingLevel, Permission, Toolbox, Turn, bubble_home,
    http_client, run_turn,
};
use bubble_github::Gh;
use bubble_store::{
    Lifecycle, Message, MessageRole, NewRun, NewTask, Origin, Run, RunRole, RunStatus, Store, TaskState, TaskSummary,
};
use nsh_core::prelude::*;
use tokio::sync::oneshot;

use crate::github::{self, Level, Repo};
use crate::manager;

const MAX_TOKENS: u32 = 16_384;
const MANAGER_KEY: &str = "manager";

/// Settings keys in the store.
pub const MANAGER_MODEL: &str = "manager_model";
pub const TASK_MODEL: &str = "task_model";
pub const MAX_RUNS: &str = "max_concurrent_runs";
pub const DEFAULT_MAX_RUNS: u32 = 4;
pub const THINKING: &str = "thinking_level";
/// Set while memory has extractions phase 2 has not consolidated.
const MEMORY_PENDING: &str = "memory_phase2_pending";

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
    Inbox,
    Tasks,
    Task(String),
    Settings,
}

/// Where a tool call stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolStatus {
    Running,
    Done,
    Failed,
    Denied,
}

/// What a conversation line is.
#[derive(Clone, Debug, PartialEq)]
pub enum LineKind {
    Message(MessageRole),
    /// A tool the agent called: which, how it went, what it gave back.
    Tool { name: String, status: ToolStatus, output: String },
    /// The agent waits for a yes: `decided` once the person answered.
    Approval { summary: String, detail: String, rememberable: bool, decided: Option<Decision> },
    /// Where a task's next run begins.
    RunStart(usize),
    /// A run that stopped with an error.
    Failure,
    /// A quiet note: the person stopped the agent.
    Notice,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub id: String,
    pub kind: LineKind,
    pub text: String,
    /// A reply still streaming.
    pub pending: bool,
    /// The model's reasoning before the reply.
    pub reasoning: String,
    /// When it was written (ms since the epoch).
    pub at: i64,
}

impl Line {
    fn new(id: impl Into<String>, kind: LineKind, text: impl Into<String>) -> Self {
        Line { id: id.into(), kind, text: text.into(), pending: false, reasoning: String::new(), at: bubble_store::now_ms() }
    }
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
    config: Stored<Config>,
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
    /// Who GitHub knows the person as, or why it cannot be reached.
    pub github_status: Signal<Option<String>>,
    /// The subscriptions signed in to, and what their services offer.
    /// ChatGPT and Grok: how each sign-in stands.
    pub accounts: Signal<Vec<Account>>,
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
    /// The Tasks page's new-task form is open.
    pub composing_task: Signal<bool>,
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
            hub: stored(Arc::new(Hub::new())),
            mcp_servers: signal(Vec::new()),
            skills: signal(SkillsSummary::default()),
            memory_status: signal(String::new()),
            hook_rules: signal(Vec::new()),
            problem: signal((!problems.is_empty()).then(|| problems.join("\n"))),
            route: signal(Route::Home),
            tasks: signal(Vec::new()),
            composing_task: signal(false),
            conversations: stored(HashMap::new()),
            queue: stored(VecDeque::new()),
            gh: stored(Gh::locate()),
            repo_cache: stored(HashMap::new()),
            repo_levels: signal(0),
            github_status: signal(None),
            root: Owner::current(),
            store: stored(store),
            config: stored(config),
        };
        state.refresh_tasks();
        state.resume_queue();
        github::start(state);
        state.discover_models();
        state.start_tools();
        state.start_memory();
        state
    }

    /// Phase 1 for conversations that changed since they were last read,
    /// then phase 2 when there is something new; in the background.
    fn start_memory(&self) {
        use bubble_core::memory;
        let app = *self;
        let hooks = Hooks::load(&bubble_home(), None);
        self.hook_rules.set(
            hooks
                .rules
                .iter()
                .map(|r| format!("{}: {}{}", r.events.join(", "), r.command, r.matcher.as_ref().map(|m| format!(" (matching {m})")).unwrap_or_default()))
                .collect(),
        );
        let root = memory::root();
        let describe = move |extra: &str| {
            let updated = std::fs::metadata(root.join("memory_summary.md"))
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| bubble_core::time::from_millis(d.as_millis()).date);
            match updated {
                Some(date) => format!("Summary updated {date}{extra}"),
                None => format!("No memory yet{extra}"),
            }
        };
        if memory::auto_disabled() {
            self.memory_status.set(format!("{}; updating is off (BUBBLE_MEMORY_AUTO)", describe("")));
            return;
        }
        let Some(model) = self.model_for(RunRole::Task).and_then(|r| self.config.with(|c| c.resolve(&r)).ok()) else {
            self.memory_status.set(describe("; no model to update it with"));
            return;
        };
        // Conversations worth reading: newest first, changed since last time.
        let mut candidates: Vec<(String, String, Vec<ChatMessage>, usize, String)> = Vec::new();
        let tasks = self.with_store(|s| s.list_tasks()).unwrap_or_default();
        for task in tasks.iter().take(24) {
            let source = format!("task:{}", task.task.id);
            let messages: Vec<Message> = self
                .with_store(|s| Ok(s.runs_for_task(&task.task.id)?.iter().flat_map(|r| s.messages(&r.id).unwrap_or_default()).collect()))
                .unwrap_or_default();
            let meaningful = messages.iter().filter(|m| matches!(m.role, MessageRole::User | MessageRole::Assistant | MessageRole::Tool)).count();
            if meaningful < memory::MIN_ENTRIES {
                continue;
            }
            let newest = messages.iter().map(|m| m.created_at).max().unwrap_or_default();
            let updated = bubble_core::time::from_millis(newest.max(0) as u128).iso;
            let known = self.with_store(|s| s.stage1(&source)).flatten();
            if known.is_some_and(|k| k.entry_count == messages.len() as i64 && k.source_updated_at == updated) {
                continue;
            }
            let cwd = task.task.project_cwd.clone().unwrap_or_default();
            candidates.push((source, cwd, history_of(&messages), messages.len(), updated));
        }
        let summary_missing = !memory::root().join("memory_summary.md").exists();
        let has_rollouts = self.with_store(|s| s.all_stage1()).is_some_and(|r| !r.is_empty());
        let pending = self.with_store(|s| s.setting(MEMORY_PENDING)).flatten().is_some();
        if candidates.is_empty() && !pending && !(summary_missing && has_rollouts) {
            self.memory_status.set(describe(""));
            return;
        }
        self.memory_status.set(describe(&format!("; reading {} conversation(s)…", candidates.len())));
        spawn(async move {
            let jobs = candidates.clone();
            let model_for_phase1 = model.clone();
            let extracted = background(async move {
                let client = http_client()?;
                let mut found = Vec::new();
                for (source, cwd, history, count, updated) in jobs {
                    let transcript = memory::transcript(&history);
                    match memory::extract(&client, &model_for_phase1, &cwd, &source, &transcript).await {
                        Ok(extraction) => found.push((source, cwd, count, updated, extraction)),
                        Err(error) => log::warn!("memory phase 1 for {source}: {error:#}"),
                    }
                }
                Ok::<_, anyhow::Error>(found)
            })
            .await;
            let found = match extracted {
                Ok(Ok(found)) => found,
                _ => Vec::new(),
            };
            let mut new = 0;
            for (source, cwd, count, updated, extraction) in found {
                let Some(extraction) = extraction else { continue };
                new += 1;
                app.with_store(|s| {
                    s.upsert_stage1(&bubble_store::Stage1 {
                        source: source.clone(),
                        cwd: cwd.clone(),
                        entry_count: count as i64,
                        source_updated_at: updated.clone(),
                        generated_at: bubble_store::now_ms(),
                        raw_memory: extraction.raw_memory.clone(),
                        rollout_summary: extraction.rollout_summary.clone(),
                        slug: extraction.slug.clone(),
                        selected: false,
                    })
                });
            }
            if new > 0 {
                app.with_store(|s| s.set_setting(MEMORY_PENDING, Some("1")));
            } else if !pending && !summary_missing {
                app.memory_status.set(describe(""));
                return;
            }
            // Phase 2 over this app's extractions and the TypeScript ones.
            let mut all = app.with_store(|s| s.all_stage1()).unwrap_or_default();
            let native: HashSet<String> = all.iter().map(|s| s.source.clone()).collect();
            all.extend(bubble_store::legacy_stage1(&memory::root().join("state.sqlite")).into_iter().filter(|s| !native.contains(&s.source)));
            all.sort_by(|a, b| b.source_updated_at.cmp(&a.source_updated_at));
            let selected: Vec<memory::Rollout> = all
                .iter()
                .take(memory::SELECTION_LIMIT)
                .map(|s| memory::Rollout {
                    source: s.source.clone(),
                    cwd: s.cwd.clone(),
                    updated_at: s.source_updated_at.clone(),
                    raw_memory: s.raw_memory.clone(),
                    rollout_summary: s.rollout_summary.clone(),
                    slug: s.slug.clone(),
                    selected: s.selected,
                })
                .collect();
            let removed = all.iter().skip(memory::SELECTION_LIMIT).filter(|s| s.selected).count();
            app.memory_status.set(describe("; consolidating…"));
            let sources: Vec<String> = selected.iter().map(|r| r.source.clone()).collect();
            let done = background(async move {
                let client = http_client()?;
                memory::consolidate(&client, &model, &memory::root(), &selected, removed).await
            })
            .await;
            match done {
                Ok(Ok(())) => {
                    app.with_store(|s| s.mark_stage1_selected(&sources));
                    app.with_store(|s| s.set_setting(MEMORY_PENDING, None));
                    app.memory_status.set(describe(""));
                }
                Ok(Err(error)) => app.memory_status.set(describe(&format!("; last update failed: {error:#}"))),
                Err(error) => app.memory_status.set(describe(&format!("; last update failed: {error}"))),
            }
        });
    }

    /// Finds the person's skills and connects their own MCP servers, so
    /// Settings shows what works before a task needs it.
    fn start_tools(&self) {
        let app = *self;
        let home = bubble_home();
        let (paths, disabled) = self.config.with(|c| (c.skill_paths.clone(), c.skills.disabled.clone()));
        let roots = Skills::roots(&home, None, &paths);
        let skills = Skills::discover(&roots, &disabled);
        let user_home = dirs_home();
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
                    mcp::Status::Connected { tools } => {
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

    pub fn refresh_tasks(&self) {
        if let Some(tasks) = self.with_store(|s| s.list_tasks()) {
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
        let level = self.thinking.get();
        match reference.split_once(':') {
            Some((provider, model)) => {
                let effective = bubble_core::model::effective_level(provider, model, level);
                if *bubble_core::model::info(provider, model).levels == [ThinkingLevel::Off] {
                    reference
                } else {
                    format!("{reference}  {}", effective.as_str())
                }
            }
            None => reference,
        }
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
            grants: stored(Arc::new(Mutex::new(HashSet::new()))),
            unlocked: stored(Arc::new(Mutex::new(HashSet::new()))),
            children: stored(children),
            subagents: signal(Vec::new()),
            approvals: stored(HashMap::new()),
            pending: stored(String::new()),
            abort: stored(None),
            on_change: stored(None),
        });
        self.conversations.with(|map| map.insert(key.to_owned(), conversation));
        // The children's progress, shown under the conversation.
        let children = conversation.children.get();
        // Children kept from before show at once.
        conversation.subagents.set(children.snapshots());
        let mut changes = children.subscribe();
        self.root.run(|| {
            spawn(async move {
                while changes.changed().await.is_ok() {
                    conversation.subagents.set(children.snapshots());
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
    /// What "allow for this task" granted.
    grants: Stored<Arc<Mutex<HashSet<String>>>>,
    /// The deferred (MCP) tools the agent has loaded.
    unlocked: Stored<Arc<Mutex<HashSet<String>>>>,
    /// Subagents the task's agent started; they outlive its turns.
    children: Stored<Arc<Children>>,
    pub subagents: Signal<Vec<delegate::Snapshot>>,
    /// Questions the agent waits on, by their line's id.
    approvals: Stored<HashMap<String, oneshot::Sender<Decision>>>,
    /// The line the next reply streams into.
    pending: Stored<String>,
    /// Cancels the running turn.
    abort: Stored<Option<tokio::task::AbortHandle>>,
    /// Called after the lines change, to keep the newest in view.
    on_change: Stored<Option<Rc<dyn Fn()>>>,
}

impl Conversation {
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

    fn push(&self, line: Line) {
        self.lines.update(|lines| lines.push(line));
    }

    fn push_pending(&self, id: String) {
        self.pending.replace(id.clone());
        self.push(Line { pending: true, ..Line::new(id, LineKind::Message(MessageRole::Assistant), "") });
    }

    /// Shows a question after the tool line of `call_id`, marks the run as
    /// waiting on the person, and returns their answer.
    pub fn ask(&self, app: AppState, run_id: &str, call_id: &str, permission: Permission) -> oneshot::Receiver<Decision> {
        let (respond, answer) = oneshot::channel();
        self.ask_with(app, run_id, call_id, permission, respond);
        answer
    }

    fn ask_with(&self, app: AppState, run_id: &str, call_id: &str, permission: Permission, respond: oneshot::Sender<Decision>) {
        let id = format!("approval-{call_id}");
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
        let tool_id = format!("tool-{call_id}");
        self.lines.update(|lines| {
            let at = lines.iter().position(|l| l.id == tool_id).map(|i| i + 1).unwrap_or(lines.len());
            lines.insert(at, line);
        });
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
        if self.approvals.with(|map| map.is_empty()) {
            if let Some(run) = self.current_run(app) {
                app.with_store(|s| s.set_run_status(&run.id, RunStatus::Running, None));
                app.refresh_tasks();
            }
        }
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
            self.push(Line::new(format!("stopped-{}", bubble_store::now_ms()), LineKind::Notice, "Taken out of the queue"));
            app.refresh_tasks();
            self.changed();
            return;
        }
        if let Some(handle) = self.abort.replace(None) {
            handle.abort();
        }
        self.children.get().cancel_all();
    }

    /// Sends what the person wrote: runs the turn now, or queues it.
    pub fn send(&self, app: AppState, text: String) {
        if let Err(message) = self.post(app, text, MessageRole::User) {
            self.fail_now(&message);
        }
    }

    /// Records a message for the agent (the person's, or a brief from the
    /// Manager) and starts the turn, or queues it when the limit is
    /// reached. `Ok(true)`: started; `Ok(false)`: queued.
    pub fn post(&self, app: AppState, text: String, role: MessageRole) -> Result<bool, String> {
        let text = text.trim().to_owned();
        if text.is_empty() {
            return Err("The message is empty.".into());
        }
        if self.busy.get_untracked() {
            return Err("The agent is still on its previous turn; send this when it finishes.".into());
        }
        let task_id = self.task_id.get();
        let run_role = if task_id.is_some() { RunRole::Task } else { RunRole::Manager };
        let model_reference = app.model_for(run_role).ok_or_else(|| {
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
                    let reusable = runs.last().filter(|r| r.status != RunStatus::Failed).cloned();
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
        let content = serde_json::to_string(&ChatMessage::user(text.clone())).ok();
        let saved = app
            .with_store(|s| s.append_message_with(&run.id, role, &text, content.as_deref()))
            .ok_or_else(|| "The message could not be saved.".to_owned())?;

        if let Some(index) = new_run_index.filter(|&i| i > 1) {
            self.push(Line::new(format!("{}-start", run.id), LineKind::RunStart(index), ""));
        }
        self.push(Line::new(saved.id.clone(), LineKind::Message(role), text));
        self.changed();
        let pending = format!("pending-{}", saved.id);
        if start_now {
            self.push_pending(pending);
            self.start_turn(app);
            Ok(true)
        } else {
            self.wait_in_queue(app, &pending);
            Ok(false)
        }
    }

    /// Marks the conversation as waiting for a slot.
    fn wait_in_queue(&self, app: AppState, pending: &str) {
        let Some(task_id) = self.task_id.get() else { return };
        if let Some(run) = self.current_run(app) {
            app.with_store(|s| s.set_run_status(&run.id, RunStatus::Queued, None));
        }
        if !self.lines.with_untracked(|lines| lines.iter().any(|l| l.pending)) {
            self.push_pending(pending.to_owned());
        }
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

    /// Runs the agent's turn on the run's history (the newest message is
    /// already stored).
    fn start_turn(&self, app: AppState) {
        let task_id = self.task_id.get();
        let Some(run) = self.current_run(app) else { return };
        let role = if task_id.is_some() { RunRole::Task } else { RunRole::Manager };
        let model = app
            .model_for(role)
            .ok_or_else(|| anyhow!("no model is set"))
            .and_then(|reference| app.config.with(|c| c.resolve(&reference)));
        let model = match model {
            Ok(model) => model,
            Err(error) => {
                self.queued.set(false);
                self.busy.set(false);
                let pending = self.pending.get();
                self.remove_line(&pending);
                app.with_store(|s| s.set_run_status(&run.id, RunStatus::Failed, Some(&format!("{error:#}"))));
                self.fail_now(&format!("{error:#}"));
                app.refresh_tasks();
                return;
            }
        };
        let (system, cwd, host_tools) = match &task_id {
            None => (with_instructions(manager::PROMPT.to_owned(), None), None, manager::specs()),
            Some(id) => {
                let task = app.with_store(|s| s.get_task(id)).flatten();
                let title = task.as_ref().map(|t| t.title.clone()).unwrap_or_default();
                let project = task.and_then(|t| t.project_cwd);
                let cwd = project.as_deref().map(PathBuf::from).filter(|p| p.is_dir());
                let tools = if cwd.is_some() { github::task_tools(app, project.as_deref()) } else { Vec::new() };
                (with_instructions(task_prompt(&title, project.as_deref(), cwd.is_some()), cwd.as_deref()), cwd, tools)
            }
        };
        let history = app.with_store(|s| Ok(history_of(&s.messages(&run.id)?))).unwrap_or_default();
        app.with_store(|s| s.set_run_status(&run.id, RunStatus::Running, None));
        self.queued.set(false);
        self.busy.set(true);
        if !self.lines.with_untracked(|lines| lines.iter().any(|l| l.pending)) {
            self.push_pending(format!("pending-{}-{}", run.id, bubble_store::now_ms()));
        }
        app.refresh_tasks();
        self.changed();

        let turn = Turn {
            model,
            system: Some(system),
            history,
            cwd,
            grants: self.grants.get(),
            host_tools,
            max_tokens: MAX_TOKENS,
            thinking: app.thinking.get_untracked(),
            toolbox: None,
            unlocked: self.unlocked.get(),
            hooks: None,
            role: bubble_core::hooks::Role::Parent,
            allowed: None,
            ask: true,
            children: task_id.is_some().then(|| self.children.get()),
        };
        // Tasks with a project get skills, web and MCP tools.
        let tools_for = turn.cwd.clone();
        let hub = app.hub.get();
        let config = app.config.with(|c| c.clone());
        let conversation = *self;
        let run_id = run.id.clone();
        // The turn belongs to the app, not to the screen that started it.
        app.root.run(move || {
            spawn(async move {
                let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel::<AgentEvent>();
                let job = background(async move {
                    let client = http_client()?;
                    let mut turn = turn;
                    turn.hooks = Some(Arc::new(Hooks::load(&bubble_home(), tools_for.as_deref())));
                    if let Some(project) = tools_for {
                        turn.toolbox = Some(Arc::new(Toolbox::load(&hub, &client, &config, Some(&project)).await));
                        if let Some(children) = &turn.children {
                            children.set_env(delegate::Env {
                                client: client.clone(),
                                config: config.clone(),
                                model: turn.model.clone(),
                                thinking: turn.thinking,
                                cwd: project.clone(),
                                toolbox: turn.toolbox.clone(),
                                hooks: turn.hooks.clone(),
                            });
                        }
                    }
                    run_turn(&client, turn, sender).await
                });
                conversation.abort.replace(Some(job.abort_handle()));
                let mut step = 0usize;
                while let Some(event) = receiver.recv().await {
                    conversation.apply(app, &run_id, event, &mut step);
                }
                let joined = job.await;
                conversation.abort.replace(None);
                app.refresh_servers();
                let stopped = matches!(&joined, Err(error) if error.is_cancelled());
                let result = match joined {
                    Ok(result) => result,
                    Err(error) => Err(anyhow!("the turn stopped: {error}")),
                };
                let pending = conversation.pending.get();
                // What streamed before a stop is kept.
                let partial = conversation
                    .lines
                    .with_untracked(|lines| lines.iter().find(|l| l.id == pending).map(|l| l.text.clone()))
                    .unwrap_or_default();
                if stopped && !partial.trim().is_empty() {
                    let message = ChatMessage::assistant(partial.clone());
                    let saved = app.with_store(|s| {
                        s.append_message_with(&run_id, MessageRole::Assistant, &partial, serde_json::to_string(&message).ok().as_deref())
                    });
                    if let Some(saved) = saved {
                        conversation.update_line(&pending, |line| {
                            line.id = saved.id;
                            line.pending = false;
                        });
                    }
                } else {
                    conversation.remove_line(&pending);
                }
                match result {
                    _ if stopped => {
                        app.with_store(|s| s.set_run_status(&run_id, RunStatus::Idle, None));
                        conversation.lines.update(|lines| {
                            for line in lines.iter_mut() {
                                if let LineKind::Tool { status: status @ ToolStatus::Running, output, .. } = &mut line.kind {
                                    *status = ToolStatus::Failed;
                                    *output = "Stopped before it finished.".into();
                                }
                            }
                        });
                        conversation.push(Line::new(format!("stopped-{}", bubble_store::now_ms()), LineKind::Notice, "Stopped"));
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
                conversation.approvals.with(|map| map.clear());
                conversation.busy.set(false);
                app.refresh_tasks();
                conversation.changed();
                app.pump_queue();
            });
        });
    }

    /// Shows and keeps what the turn reports.
    fn apply(&self, app: AppState, run_id: &str, event: AgentEvent, step: &mut usize) {
        match event {
            AgentEvent::Text(piece) => {
                let pending = self.pending.get();
                self.update_line(&pending, |line| line.text.push_str(&piece));
                self.changed();
            }
            AgentEvent::Reasoning(piece) => {
                let pending = self.pending.get();
                self.update_line(&pending, |line| line.reasoning.push_str(&piece));
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
                let note = Line::new(format!("checkpoint-{}", bubble_store::now_ms()), LineKind::Notice, CHECKPOINT_NOTE);
                self.lines.update(|lines| {
                    let at = lines.iter().position(|l| l.id == pending).unwrap_or(lines.len());
                    lines.insert(at, note);
                });
                self.changed();
            }
            AgentEvent::Assistant(message) => {
                let ChatMessage::Assistant { text, tool_calls, reasoning } = &message else { return };
                let saved = app.with_store(|s| {
                    s.append_message_with(run_id, MessageRole::Assistant, text, serde_json::to_string(&message).ok().as_deref())
                });
                let finished = self.pending.get();
                if text.trim().is_empty() && reasoning.trim().is_empty() {
                    self.remove_line(&finished);
                } else {
                    let id = saved.map(|m| m.id).unwrap_or_else(|| finished.clone());
                    let text = text.clone();
                    let reasoning = reasoning.clone();
                    self.update_line(&finished, move |line| {
                        line.id = id;
                        line.pending = false;
                        line.text = text;
                        line.reasoning = reasoning;
                    });
                }
                for call in tool_calls {
                    self.push(Line::new(
                        format!("tool-{}", call.id),
                        LineKind::Tool { name: call.name.clone(), status: ToolStatus::Running, output: String::new() },
                        bubble_core::tools::describe(call),
                    ));
                }
                if !tool_calls.is_empty() {
                    *step += 1;
                    self.push_pending(format!("{finished}-{step}"));
                }
                self.changed();
            }
            AgentEvent::ToolStarted { .. } => {}
            AgentEvent::HostCall { call, respond } => {
                if github::is_task_tool(&call.name) {
                    github::handle_call(app, *self, run_id, self.task_id.get(), call, respond);
                } else {
                    let _ = respond.send(manager::execute(app, &call));
                }
            }
            AgentEvent::ApprovalNeeded { call, permission, respond } => {
                self.ask_with(app, run_id, &call.id, permission, respond);
            }
            AgentEvent::ToolFinished { call_id, output, denied } => {
                let status = if denied {
                    ToolStatus::Denied
                } else if output.is_error {
                    ToolStatus::Failed
                } else {
                    ToolStatus::Done
                };
                self.update_line(&format!("tool-{call_id}"), move |line| {
                    if let LineKind::Tool { name, .. } = &line.kind {
                        line.kind = LineKind::Tool { name: name.clone(), status, output: output.content };
                    }
                });
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

fn task_prompt(title: &str, project: Option<&str>, has_tools: bool) -> String {
    let place = match (project, has_tools) {
        (Some(path), true) => format!(
            "You work in the project folder {path}. Use the tools to read, search, change files and run commands there; \
             relative paths start in that folder. Changes and commands wait for the person's yes."
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
fn lines_of(store: &Store, runs: &[Run]) -> anyhow::Result<Vec<Line>> {
    let mut lines: Vec<Line> = Vec::new();
    for (index, run) in runs.iter().enumerate() {
        if index > 0 {
            lines.push(Line { at: run.created_at, ..Line::new(format!("{}-start", run.id), LineKind::RunStart(index + 1), "") });
        }
        for message in store.messages(&run.id)? {
            if message.role == MessageRole::Checkpoint {
                lines.push(Line { at: message.created_at, ..Line::new(message.id.clone(), LineKind::Notice, CHECKPOINT_NOTE) });
                continue;
            }
            let parsed = message.content.as_deref().and_then(|c| serde_json::from_str::<ChatMessage>(c).ok());
            match parsed {
                Some(ChatMessage::Assistant { text, tool_calls, reasoning }) => {
                    if !text.trim().is_empty() || !reasoning.trim().is_empty() {
                        lines.push(Line {
                            at: message.created_at,
                            reasoning,
                            ..Line::new(message.id.clone(), LineKind::Message(MessageRole::Assistant), text)
                        });
                    }
                    for call in &tool_calls {
                        lines.push(Line::new(
                            format!("tool-{}", call.id),
                            LineKind::Tool {
                                name: call.name.clone(),
                                status: ToolStatus::Failed,
                                output: "Interrupted before it ran.".into(),
                            },
                            bubble_core::tools::describe(call),
                        ));
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
                _ => {
                    if message.role != MessageRole::Tool && !message.text.trim().is_empty() {
                        lines.push(Line {
                            at: message.created_at,
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
    Ok(lines)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bubble_core::ToolCall;

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
    fn calls_without_results_are_closed() {
        let call = ToolCall { id: "c1".into(), name: "bash".into(), arguments: "{}".into() };
        let messages = vec![
            message(MessageRole::User, Some(ChatMessage::user("go"))),
            message(
                MessageRole::Assistant,
                Some(ChatMessage::Assistant { text: String::new(), reasoning: String::new(), tool_calls: vec![call] }),
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
