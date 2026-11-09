//! Subagents: children an agent starts in the background with a
//! self-contained brief, then collects (src/agent/subagent/, profiles.ts and
//! tools/agent-lifecycle.ts in the TypeScript core).
//!
//! Children are read-only: they read, search and look things up, and hand
//! back a summary. They cannot ask the person anything (a tool that needs a
//! yes is blocked) and cannot start children of their own. Profiles come
//! built in (`default`, `explorer`, `worker`, and the subtask ones) or from
//! `~/.bubble/agents/*.md` and the project's `.bubble/agents/*.md`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::{Semaphore, mpsc, watch};

use crate::agent::{AgentEvent, Turn, run_turn};
use crate::config::{Config, ResolvedModel};
use crate::hooks::{self, Hooks};
use crate::instructions;
use crate::model::Level;
use crate::provider::{ChatMessage, ToolCall, ToolSpec};
use crate::toolbox::Toolbox;
use crate::tools::{self, ToolOutput};

/// How many children run at once in one conversation.
pub const MAX_ACTIVE: usize = 8;
const DEFAULT_WAIT: Duration = Duration::from_secs(30);
const MAX_WAIT: Duration = Duration::from_secs(1800);
const HANDOFF_TOKEN_FLOOR: f64 = 60.0;

const READONLY_PRESET: &[&str] = &["read", "ls", "grep", "lsp", "web_search", "web_fetch", "memory", "skill"];

/// Delegation tools a child never gets.
const DENIED: &[&str] =
    &["subagent", "spawn_agent", "wait_agent", "send_input", "close_agent", "list_agents", "run_workflow", "wait_workflow"];

const NICKNAMES: &[&str] = &[
    "Ada", "Alan", "Grace", "Katherine", "Claude", "Edsger", "Barbara", "Donald", "Margaret", "Ken", "Radia", "Leslie",
    "Mary", "Dennis", "Frances", "Niklaus", "Jean", "Linus", "Anita", "Yukihiro", "Brenda", "Guido", "Sophie", "Tim",
    "Hedy", "John", "Evelyn", "Bjarne", "Karen", "Vint", "Adele", "Fernando",
];

const WAIT_GUIDANCE: &str = "Coordinate around dependencies: after delegation, continue useful non-overlapping work that does not require the child's result; wait_agent when that result is needed for your next meaningful step or final synthesis. Choose a wait timeout according to the task and your next decision. It is an upper bound, not a mandatory delay: wait_agent returns early when a target finishes, so a generous timeout does not slow down a short task. A short wait is appropriate when its outcome informs a concrete scheduling decision. Avoid repeated short checks solely to see whether time has passed; use list_agents for a status snapshot when that information is useful. The same decision-value principle applies to list_agents: do not replace short waits with repeated status polling. With multiple children, wait_agent may return as soon as any target finishes; use explicit IDs for the remaining children whose results you still need, rather than repeatedly collecting an already-finished child. A timeout ends only that wait, not the child's work. Reassess available independent work and whether the result still blocks progress before waiting again; do not duplicate the delegated task.";

// --- Profiles ------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub struct Profile {
    pub name: String,
    pub description: String,
    /// `builtin`, `user` or `project`.
    pub source: &'static str,
    /// `readonly`, or a write mode this runtime does not offer yet.
    pub mode: String,
    /// `None` inherits the parent's model.
    pub model: Option<String>,
    pub tools: Vec<String>,
    pub prompt: String,
    pub nicknames: Vec<String>,
}

fn role(name: &str, description: &str, prompt: &[&str], tools: &[&str]) -> Profile {
    Profile {
        name: name.into(),
        description: description.into(),
        source: "builtin",
        mode: "readonly".into(),
        model: None,
        tools: tools.iter().map(|t| t.to_string()).collect(),
        prompt: prompt.join("\n"),
        nicknames: Vec::new(),
    }
}

pub fn builtin_profiles() -> Vec<Profile> {
    let subtask = |kind: &str, tools: &[&str], lines: &[&str]| {
        let mut prompt = vec![format!("Subtask policy: {kind}")];
        prompt.extend(lines.iter().map(|l| l.to_string()));
        Profile {
            name: format!("builtin:{kind}"),
            description: format!("{kind} read-only subagent"),
            source: "builtin",
            mode: "readonly".into(),
            model: None,
            tools: tools.iter().map(|t| t.to_string()).collect(),
            prompt: prompt.join("\n"),
            nicknames: Vec::new(),
        }
    };
    let search_tools = ["read", "ls", "grep", "web_search", "web_fetch", "skill"];
    vec![
        role(
            "default",
            "General read-only subagent",
            &[
                "You are a focused child agent. Complete the assigned task independently using only read-only tools.",
                "Return a concise result with concrete evidence, file paths, and any uncertainty.",
                "Do not ask the user questions. If blocked by policy or missing data, state the blocker plainly.",
            ],
            READONLY_PRESET,
        ),
        role(
            "explorer",
            "Fast codebase exploration subagent",
            &[
                "You are an explorer subagent for codebase reconnaissance.",
                "Answer the specific question by inspecting the repository directly. Prefer precise file paths and line-level evidence.",
                "Keep the answer compact and avoid broad refactors or implementation plans unless asked.",
            ],
            &["read", "ls", "grep", "lsp", "memory", "skill"],
        ),
        role(
            "worker",
            "Implementation-planning worker subagent",
            &[
                "You are a worker subagent. In this Phase 1 runtime you are read-only, so you must not modify files.",
                "Analyze the assigned implementation slice, identify exact files to change, and return a concrete patch plan or findings.",
                "If write-capable worker mode is needed, say so explicitly.",
            ],
            &["read", "ls", "grep", "lsp", "memory", "skill"],
        ),
        subtask(
            "search",
            &search_tools,
            &[
                "- Focus on locating relevant files, symbols, and evidence quickly.",
                "- Use ls for directory contents and grep for content search.",
                "- Return a concise summary of what you found and where.",
            ],
        ),
        subtask(
            "security_investigation",
            &search_tools,
            &[
                "- Investigate only in read-only mode.",
                "- Collect evidence about config load paths, environment reads, persistence paths, masking, and exposure paths.",
                "- Do not loop on broad keyword search; summarize evidence and uncertainty.",
            ],
        ),
        subtask(
            "evidence_correlation",
            &["read", "skill"],
            &[
                "- Correlate evidence already discovered.",
                "- Avoid new broad searches; read only the specific files that matter.",
                "- Produce a reasoning-focused summary that states what the evidence supports.",
            ],
        ),
        subtask(
            "general_readonly",
            &search_tools,
            &["- Stay in read-only mode.", "- Keep the scope tightly bounded and summarize findings concisely."],
        ),
    ]
}

/// Built-ins, then the person's, then the project's (nearest
/// `.bubble/agents` up from `cwd`); a later one of the same name wins.
pub fn profiles(home: &Path, cwd: Option<&Path>) -> Vec<Profile> {
    let mut all = builtin_profiles();
    all.extend(load_dir(&home.join("agents"), "user"));
    if let Some(dir) = cwd.and_then(project_agents_dir) {
        all.extend(load_dir(&dir, "project"));
    }
    let mut merged: Vec<Profile> = Vec::new();
    for profile in all {
        merged.retain(|p| p.name != profile.name);
        merged.push(profile);
    }
    merged
}

pub fn find<'a>(profiles: &'a [Profile], name: &str) -> Option<&'a Profile> {
    profiles.iter().find(|p| p.name == name).or_else(|| profiles.iter().find(|p| p.name == format!("builtin:{name}")))
}

fn project_agents_dir(cwd: &Path) -> Option<PathBuf> {
    cwd.ancestors().map(|dir| dir.join(".bubble").join("agents")).find(|dir| dir.is_dir())
}

fn load_dir(dir: &Path, source: &'static str) -> Vec<Profile> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut files: Vec<PathBuf> =
        entries.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "md") && p.is_file()).collect();
    files.sort();
    files.iter().filter_map(|file| parse_profile(&std::fs::read_to_string(file).ok()?, source, file)).collect()
}

fn profile_scalar(value: &str) -> Value {
    let unquoted = value
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .or_else(|| value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
        .unwrap_or(value);
    if let Some(inner) = unquoted.strip_prefix('[').and_then(|v| v.strip_suffix(']')) {
        return Value::Array(
            inner.split(',').map(|i| profile_scalar(i.trim())).filter(|v| v.as_str() != Some("")).map(|v| json!(v.as_str().map(str::to_owned).unwrap_or_else(|| v.to_string()))).collect(),
        );
    }
    match unquoted {
        "true" => Value::Bool(true),
        "false" => Value::Bool(false),
        n if n.parse::<i64>().is_ok() => json!(n.parse::<i64>().unwrap()),
        text => Value::String(text.to_owned()),
    }
}

/// The profile front matter: scalars, `  - item` lists, and one level of
/// nested keys (for `tools: {preset, include, exclude}`).
fn profile_front_matter(block: &str) -> serde_json::Map<String, Value> {
    let lines: Vec<&str> = block.lines().collect();
    let mut out = serde_json::Map::new();
    let key_value = |line: &str| -> Option<(String, String)> {
        let (key, value) = line.split_once(':')?;
        (!key.is_empty() && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'))
            .then(|| (key.to_owned(), value.trim().to_owned()))
    };
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        i += 1;
        let Some((key, value)) = key_value(line) else { continue };
        if !value.is_empty() {
            out.insert(key, profile_scalar(&value));
            continue;
        }
        let mut items = Vec::new();
        while i < lines.len() && lines[i].starts_with("  - ") {
            let item = lines[i][4..].trim();
            if !item.is_empty() {
                items.push(json!(profile_scalar(item).as_str().map(str::to_owned).unwrap_or_else(|| item.to_owned())));
            }
            i += 1;
        }
        if !items.is_empty() {
            out.insert(key, Value::Array(items));
            continue;
        }
        let mut nested = serde_json::Map::new();
        while i < lines.len() && lines[i].starts_with("  ") {
            if let Some((nested_key, nested_value)) = key_value(lines[i].trim()) {
                i += 1;
                if !nested_value.is_empty() {
                    nested.insert(nested_key, profile_scalar(&nested_value));
                    continue;
                }
                let mut items = Vec::new();
                while i < lines.len() && lines[i].starts_with("    - ") {
                    let item = lines[i][6..].trim();
                    if !item.is_empty() {
                        items.push(json!(item));
                    }
                    i += 1;
                }
                nested.insert(nested_key, if items.is_empty() { json!("") } else { Value::Array(items) });
            } else {
                i += 1;
            }
        }
        out.insert(key, Value::Object(nested));
    }
    out
}

fn strings(value: &Value) -> Vec<String> {
    match value {
        Value::String(s) if !s.trim().is_empty() => vec![s.trim().to_owned()],
        Value::Array(items) => items.iter().filter_map(|i| i.as_str()).map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned).collect(),
        _ => Vec::new(),
    }
}

fn parse_profile(raw: &str, source: &'static str, file: &Path) -> Option<Profile> {
    let normalized = raw.replace("\r\n", "\n");
    let rest = normalized.strip_prefix("---\n")?;
    let end = rest.find("\n---\n")?;
    let front = profile_front_matter(&rest[..end]);
    let body = rest[end + 5..].trim().to_owned();
    let text = |key: &str| front.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned);
    let name = text("name").unwrap_or_else(|| file.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default());
    let description = text("description")?;
    // Tools: a preset name, a list, or {preset, include, exclude}.
    let (preset, include, exclude) = match front.get("tools") {
        Some(Value::String(s)) if matches!(s.as_str(), "none" | "explicit" | "readonly") => (s.clone(), Vec::new(), Vec::new()),
        Some(Value::String(s)) => ("explicit".into(), vec![s.clone()], Vec::new()),
        Some(Value::Array(_)) => ("explicit".into(), strings(&front["tools"]), Vec::new()),
        Some(Value::Object(o)) => (
            o.get("preset").and_then(Value::as_str).filter(|p| matches!(*p, "none" | "explicit" | "readonly")).unwrap_or("readonly").to_owned(),
            o.get("include").map(strings).unwrap_or_default(),
            o.get("exclude").map(strings).unwrap_or_default(),
        ),
        _ => ("readonly".into(), Vec::new(), Vec::new()),
    };
    let canonical = |t: &str| if matches!(t, "memory_search" | "memory_read_summary") { "memory".to_owned() } else { t.to_owned() };
    let mut tools: Vec<String> = if preset == "readonly" { READONLY_PRESET.iter().map(|t| t.to_string()).collect() } else { Vec::new() };
    for tool in include.iter().map(|t| canonical(t)) {
        if !tools.contains(&tool) {
            tools.push(tool);
        }
    }
    tools.retain(|t| !exclude.iter().any(|e| &canonical(e) == t));
    let model = text("model").filter(|m| m != "inherit");
    let mode = text("mode").filter(|m| m == "write_patch" || m == "write_worktree").unwrap_or_else(|| "readonly".into());
    let nicknames = front.get("nicknameCandidates").or(front.get("nicknames")).map(strings).unwrap_or_default();
    Some(Profile { name, description, source, mode, model, tools, prompt: body, nicknames })
}

// --- Children ------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Queued,
    Running,
    Completed,
    Failed,
    Blocked,
    Cancelled,
    Closed,
}

impl Status {
    pub fn label(self) -> &'static str {
        match self {
            Status::Queued => "queued",
            Status::Running => "running",
            Status::Completed => "completed",
            Status::Failed => "failed",
            Status::Blocked => "blocked",
            Status::Cancelled => "cancelled",
            Status::Closed => "closed",
        }
    }

    pub fn is_final(self) -> bool {
        !matches!(self, Status::Queued | Status::Running)
    }
}

/// A child as the parent and the app see it.
#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    pub id: String,
    pub nickname: String,
    pub profile: String,
    pub task: String,
    /// Short title for the activity card.
    pub description: Option<String>,
    pub status: Status,
    pub model: String,
    pub summary: String,
    pub notes: Vec<String>,
    pub error: Option<String>,
    /// Started by a workflow rather than spawn_agent.
    pub workflow: bool,
    pub pending_inputs: usize,
}

/// What a child's turns run with, set by the parent's turn.
#[derive(Clone)]
pub struct Env {
    pub client: reqwest::Client,
    pub config: Config,
    pub model: ResolvedModel,
    pub thinking: Level,
    pub cwd: PathBuf,
    pub toolbox: Option<Arc<Toolbox>>,
    pub hooks: Option<Arc<Hooks>>,
}

struct Record {
    snapshot: Snapshot,
    env: Env,
    model: ResolvedModel,
    thinking: Level,
    system: String,
    tools: HashSet<String>,
    history: Vec<ChatMessage>,
    inputs: Vec<String>,
    abort: Option<tokio::task::AbortHandle>,
    /// Ask for a JSON value matching this schema (workflow members).
    schema: Option<Value>,
}

/// One conversation's children, kept across its turns.
pub struct Children {
    records: Mutex<Vec<Record>>,
    env: Mutex<Option<Env>>,
    slots: Arc<Semaphore>,
    version: watch::Sender<u64>,
    next: Mutex<u64>,
}

impl Default for Children {
    fn default() -> Self {
        Children {
            records: Mutex::new(Vec::new()),
            env: Mutex::new(None),
            slots: Arc::new(Semaphore::new(MAX_ACTIVE)),
            version: watch::channel(0).0,
            next: Mutex::new(0),
        }
    }
}

/// Options for one new child.
#[derive(Clone, Debug, Default)]
pub struct SpawnOptions {
    pub profile: String,
    pub description: Option<String>,
    pub model: Option<String>,
    pub effort: Option<Level>,
    pub nickname: Option<String>,
    pub fork_context: Option<Vec<ChatMessage>>,
    pub schema: Option<Value>,
    pub workflow: bool,
}

impl Children {
    pub fn new() -> Arc<Children> {
        Arc::new(Children::default())
    }

    /// What new children run with; the parent's turn sets it.
    pub fn set_env(&self, env: Env) {
        if let Ok(mut current) = self.env.lock() {
            *current = Some(env);
        }
    }

    fn changed(&self) {
        self.version.send_modify(|v| *v += 1);
    }

    /// Changes to watch, for a view of the children.
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.version.subscribe()
    }

    pub fn snapshots(&self) -> Vec<Snapshot> {
        self.records.lock().map(|r| r.iter().map(|r| r.snapshot.clone()).collect()).unwrap_or_default()
    }

    pub fn snapshot(&self, id: &str) -> Option<Snapshot> {
        self.records.lock().ok()?.iter().find(|r| r.snapshot.id == id).map(|r| r.snapshot.clone())
    }

    fn update(&self, id: &str, f: impl FnOnce(&mut Record)) {
        if let Ok(mut records) = self.records.lock() {
            if let Some(record) = records.iter_mut().find(|r| r.snapshot.id == id) {
                f(record);
            }
        }
        self.changed();
    }

    /// Stops every running child (the parent was stopped).
    pub fn cancel_all(&self) {
        if let Ok(mut records) = self.records.lock() {
            for record in records.iter_mut().filter(|r| !r.snapshot.status.is_final()) {
                if let Some(abort) = record.abort.take() {
                    abort.abort();
                }
                record.snapshot.status = Status::Cancelled;
                record.snapshot.error = Some("Stopped with the parent.".into());
            }
        }
        self.changed();
    }

    /// Starts a child in the background.
    pub fn spawn(self: &Arc<Self>, message: &str, options: SpawnOptions) -> Result<Snapshot, String> {
        let env = self.env.lock().ok().and_then(|e| e.clone()).ok_or("no agent runtime for subagents")?;
        let all = profiles(&crate::config::bubble_home(), Some(&env.cwd));
        let Some(profile) = find(&all, &options.profile).cloned() else {
            let mut names: Vec<String> = all.iter().map(|p| p.name.clone()).collect();
            names.sort();
            return Err(format!("Error: unknown subagent profile \"{}\". Available profiles: {}", options.profile, names.join(", ")));
        };
        if profile.mode != "readonly" {
            return Err(format!(
                "Error: subagent profile \"{}\" uses mode \"{}\", which this runtime does not offer yet. Use a read-only profile.",
                profile.name, profile.mode
            ));
        }
        // Model: the call's, else the profile's, else the parent's.
        let model = match options.model.as_deref().or(profile.model.as_deref()) {
            None => env.model.clone(),
            Some(reference) => {
                let reference =
                    if reference.contains(':') { reference.to_owned() } else { format!("{}:{reference}", env.model.provider.id) };
                env.config.resolve(&reference).map_err(|e| format!("Error: cannot use model {reference}: {e:#}"))?
            }
        };
        let thinking = options.effort.unwrap_or(env.thinking);
        let id = {
            let mut next = self.next.lock().map_err(|_| "poisoned")?;
            *next += 1;
            format!("agent_{}", *next)
        };
        let nickname = options.nickname.clone().unwrap_or_else(|| {
            let active: Vec<String> = self.snapshots().into_iter().filter(|s| !s.status.is_final()).map(|s| s.nickname).collect();
            let pool: Vec<&str> =
                if profile.nicknames.is_empty() { NICKNAMES.to_vec() } else { profile.nicknames.iter().map(String::as_str).collect() };
            let free: Vec<&str> = pool.iter().copied().filter(|n| !active.iter().any(|a| a.eq_ignore_ascii_case(n))).collect();
            let pick = if free.is_empty() { &pool } else { &free };
            let seed = crate::time::now_utc().millis as usize + id.len();
            pick[seed % pick.len()].to_owned()
        });
        let tools: HashSet<String> = profile.tools.iter().filter(|t| !DENIED.contains(&t.as_str())).cloned().collect();
        let system = child_system(&nickname, &profile, &env, &tools);
        let mut history = options.fork_context.clone().unwrap_or_default();
        let mut task = message.to_owned();
        if let Some(schema) = &options.schema {
            task.push_str(&format!(
                "\n\nEnd with your answer as a single JSON value matching this JSON Schema, and nothing else:\n{schema}"
            ));
        }
        history.push(ChatMessage::user(task));
        let snapshot = Snapshot {
            id: id.clone(),
            nickname,
            profile: profile.name.clone(),
            task: message.to_owned(),
            description: options.description.clone(),
            status: Status::Queued,
            model: format!("{}:{}", model.provider.id, model.model),
            summary: String::new(),
            notes: Vec::new(),
            error: None,
            workflow: options.workflow,
            pending_inputs: 0,
        };
        let record = Record {
            snapshot: snapshot.clone(),
            env,
            model,
            thinking,
            system,
            tools,
            history,
            inputs: Vec::new(),
            abort: None,
            schema: options.schema,
        };
        if let Ok(mut records) = self.records.lock() {
            records.push(record);
        }
        self.start(&id);
        Ok(self.snapshot(&id).unwrap_or(snapshot))
    }

    /// Runs (or reruns) a child's thread in the background.
    fn start(self: &Arc<Self>, id: &str) {
        let children = self.clone();
        let task_id = id.to_owned();
        let handle = tokio::spawn(async move { children.run(&task_id).await });
        self.update(id, |record| {
            record.abort = Some(handle.abort_handle());
            record.snapshot.status = Status::Queued;
        });
    }

    async fn run(self: Arc<Self>, id: &str) {
        let Ok(_permit) = self.slots.clone().acquire_owned().await else { return };
        let Some((env, model, thinking, system, tools, schema)) = self.records.lock().ok().and_then(|records| {
            records.iter().find(|r| r.snapshot.id == id).map(|r| {
                (r.env.clone(), r.model.clone(), r.thinking, r.system.clone(), r.tools.clone(), r.schema.clone())
            })
        }) else {
            return;
        };
        self.update(id, |r| r.snapshot.status = Status::Running);
        if let Some(hooks) = &env.hooks {
            let snapshot = self.snapshot(id);
            hooks
                .run(hooks::Role::Parent, hooks::Event {
                    name: "SubagentStart",
                    cwd: &env.cwd,
                    target: snapshot.as_ref().map(|s| s.profile.as_str()).unwrap_or_default(),
                    payload: json!({ "agentId": id, "profile": snapshot.as_ref().map(|s| s.profile.clone()) }),
                    full: json!({ "task": snapshot.as_ref().map(|s| s.task.clone()) }),
                })
                .await;
        }
        loop {
            let history = self.records.lock().ok().and_then(|r| r.iter().find(|r| r.snapshot.id == id).map(|r| r.history.clone()));
            let Some(history) = history else { return };
            let outcome = self.turn(id, &env, &model, thinking, &system, &tools, history).await;
            match outcome {
                Err(error) => {
                    self.update(id, |r| {
                        r.snapshot.status = Status::Failed;
                        r.snapshot.error = Some(error);
                    });
                    break;
                }
                Ok(used_tools) => {
                    // Inputs sent while it ran start another turn.
                    let more = self
                        .records
                        .lock()
                        .ok()
                        .and_then(|mut records| {
                            let record = records.iter_mut().find(|r| r.snapshot.id == id)?;
                            let inputs = std::mem::take(&mut record.inputs);
                            record.snapshot.pending_inputs = 0;
                            for input in &inputs {
                                record.history.push(ChatMessage::user(input.clone()));
                            }
                            Some(!inputs.is_empty())
                        })
                        .unwrap_or(false);
                    if more {
                        continue;
                    }
                    let summary = self.snapshot(id).map(|s| s.summary).unwrap_or_default();
                    if needs_final_summary(&summary, used_tools, schema.is_some()) {
                        self.final_summary(id, &env, &model, &system).await;
                    }
                    self.update(id, |r| {
                        if r.snapshot.status == Status::Running {
                            r.snapshot.status = Status::Completed;
                        }
                    });
                    break;
                }
            }
        }
        self.update(id, |r| r.abort = None);
        if let Some(hooks) = &env.hooks {
            let snapshot = self.snapshot(id);
            hooks
                .run(hooks::Role::Parent, hooks::Event {
                    name: "SubagentStop",
                    cwd: &env.cwd,
                    target: snapshot.as_ref().map(|s| s.profile.as_str()).unwrap_or_default(),
                    payload: json!({ "agentId": id, "status": snapshot.as_ref().map(|s| s.status.label()) }),
                    full: json!({ "summary": snapshot.as_ref().map(|s| s.summary.clone()) }),
                })
                .await;
        }
    }

    /// One turn of a child; whether it used any tool.
    #[allow(clippy::too_many_arguments)]
    async fn turn(
        &self,
        id: &str,
        env: &Env,
        model: &ResolvedModel,
        thinking: Level,
        system: &str,
        tools: &HashSet<String>,
        history: Vec<ChatMessage>,
    ) -> Result<bool, String> {
        let unlocked: HashSet<String> = tools.iter().filter(|t| t.starts_with("mcp__")).cloned().collect();
        let turn = Turn {
            cwd: Some(env.cwd.clone()),
            toolbox: env.toolbox.clone(),
            hooks: env.hooks.clone(),
            role: hooks::Role::Subagent,
            allowed: Some(tools.clone()),
            ask: false,
            thinking,
            unlocked: Arc::new(Mutex::new(unlocked)),
            ..Turn::new(model.clone(), Some(system.to_owned()), history)
        };
        let (sender, mut receiver) = mpsc::unbounded_channel();
        let client = env.client.clone();
        let job = tokio::spawn(async move { run_turn(&client, turn, sender).await });
        let mut used_tools = false;
        while let Some(event) = receiver.recv().await {
            match event {
                AgentEvent::Assistant(message) => {
                    let text = match &message {
                        ChatMessage::Assistant { text, tool_calls, .. } if tool_calls.is_empty() => Some(text.trim().to_owned()),
                        _ => None,
                    };
                    self.update(id, |r| {
                        r.history.push(message);
                        if let Some(text) = text {
                            r.snapshot.summary = text;
                        }
                    });
                }
                AgentEvent::ToolResults(message) => self.update(id, |r| r.history.push(message)),
                AgentEvent::Compacted(history) => self.update(id, |r| r.history = history),
                AgentEvent::ToolStarted { description, .. } => {
                    used_tools = true;
                    self.update(id, |r| {
                        r.snapshot.notes.push(description);
                        let len = r.snapshot.notes.len();
                        if len > 20 {
                            r.snapshot.notes.drain(..len - 20);
                        }
                    });
                }
                AgentEvent::ApprovalNeeded { respond, .. } => {
                    let _ = respond.send(crate::agent::Decision::Deny);
                }
                AgentEvent::HostCall { respond, .. } => {
                    let _ = respond.send(ToolOutput { content: "Not available to subagents.".into(), is_error: true });
                }
                _ => {}
            }
        }
        match job.await {
            Ok(Ok(())) => Ok(used_tools),
            Ok(Err(error)) => Err(format!("{error:#}")),
            Err(error) => Err(format!("the child stopped: {error}")),
        }
    }

    /// Asks a child that ended on narration for a proper handoff.
    async fn final_summary(&self, id: &str, env: &Env, model: &ResolvedModel, system: &str) {
        let Some(mut history) = self.records.lock().ok().and_then(|r| r.iter().find(|r| r.snapshot.id == id).map(|r| r.history.clone())) else {
            return;
        };
        history.push(ChatMessage::user(
            [
                "Produce the final subagent handoff now: what you found, your conclusions, and any unfinished items.",
                "Do not call tools. Do not announce next steps or plans.",
                "Use the evidence already gathered in this child thread.",
                "Return concise findings with concrete file paths and explicit uncertainty.",
                "If your previous message already was the complete handoff, restate it as-is — do not pad it.",
                "Your entire response will be returned to the parent as the subagent's answer.",
            ]
            .join("\n"),
        ));
        let turn = Turn { allowed: Some(HashSet::new()), ask: false, ..Turn::new(model.clone(), Some(system.to_owned()), history) };
        let (sender, mut receiver) = mpsc::unbounded_channel();
        let client = env.client.clone();
        let job = tokio::spawn(async move { run_turn(&client, turn, sender).await });
        let mut text = String::new();
        while let Some(event) = receiver.recv().await {
            if let AgentEvent::Assistant(ChatMessage::Assistant { text: t, tool_calls, .. }) = event {
                if tool_calls.is_empty() {
                    text = t;
                }
            }
        }
        let _ = job.await;
        if !text.trim().is_empty() {
            self.update(id, |r| r.snapshot.summary = text.trim().to_owned());
        }
    }

    /// Waits until a target is final, or the timeout.
    pub async fn wait(&self, ids: Option<Vec<String>>, timeout: Duration) -> Vec<Snapshot> {
        let targets = |all: Vec<Snapshot>| -> Vec<Snapshot> {
            match &ids {
                Some(ids) => all.into_iter().filter(|s| ids.contains(&s.id)).collect(),
                None => all.into_iter().filter(|s| s.status != Status::Closed && !s.workflow).collect(),
            }
        };
        let mut changes = self.subscribe();
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let current = targets(self.snapshots());
            if current.is_empty() || current.iter().any(|s| s.status.is_final()) {
                return current;
            }
            if tokio::time::timeout_at(deadline, changes.changed()).await.is_err() {
                return current;
            }
        }
    }

    /// Follow-up for a child: queued while it runs, a new turn when it is done.
    pub fn send(self: &Arc<Self>, id: &str, message: &str, interrupt: bool) -> Result<Snapshot, String> {
        let snapshot = self.snapshot(id).ok_or_else(|| format!("Error: unknown agent_id \"{id}\"."))?;
        if snapshot.status == Status::Closed {
            return Err(format!("Error: {} ({id}) is closed.", snapshot.nickname));
        }
        if !snapshot.status.is_final() && !interrupt {
            self.update(id, |r| {
                r.inputs.push(message.to_owned());
                r.snapshot.pending_inputs = r.inputs.len();
            });
            return Ok(self.snapshot(id).unwrap_or(snapshot));
        }
        self.update(id, |r| {
            if let Some(abort) = r.abort.take() {
                abort.abort();
            }
            r.history.push(ChatMessage::user(message.to_owned()));
            r.snapshot.error = None;
        });
        self.start(id);
        Ok(self.snapshot(id).unwrap_or(snapshot))
    }

    pub fn close(&self, id: &str) -> Result<Snapshot, String> {
        self.snapshot(id).ok_or_else(|| format!("Error: unknown agent_id \"{id}\"."))?;
        self.update(id, |r| {
            if let Some(abort) = r.abort.take() {
                abort.abort();
            }
            r.snapshot.status = Status::Closed;
        });
        self.snapshot(id).ok_or_else(|| "gone".into())
    }

    // --- Tools -----------------------------------------------------------------

    pub fn specs(cwd: Option<&Path>) -> Vec<ToolSpec> {
        let custom: Vec<String> = profiles(&crate::config::bubble_home(), cwd)
            .into_iter()
            .filter(|p| p.source != "builtin")
            .map(|p| format!("- {} — {}", p.name, one_line(&p.description, 120)))
            .collect();
        let profiles_note = if custom.is_empty() { String::new() } else { format!(" Available profiles:\n{}", custom.join("\n")) };
        let levels = ["off", "minimal", "low", "medium", "high", "xhigh", "max", "ultra"];
        vec![
            ToolSpec {
                name: "spawn_agent".into(),
                description: format!(
                    "Start a child subagent in the background and return its agent_id plus random nickname. The child has an independent thread; call wait_agent later to collect its result. Proactively delegate multi-file investigations whose intermediate steps would be noise in the main conversation. Best for helpers whose results you want to read and react to individually. For a uniform sweep over many items, or when member results should be aggregated before returning, weigh run_workflow instead. If the user asked for a workflow, an orchestration, or an agent team, that is run_workflow, not this tool. Do the work yourself when it takes only a couple of tool calls or needs conversation context — unless the user explicitly asks for a subagent, in which case spawn one. The child starts with zero context: write the task as a self-contained work order — state the goal, include known file paths or commands, and never make it rediscover knowledge you already hold. After spawning, do not duplicate the same delegated work locally; either wait for the child or do clearly non-overlapping work. A child may start as queued when concurrency slots are busy; it starts automatically, no action needed. Children are read-only.{profiles_note}"
                ),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "description": { "type": "string", "description": "Short task title for the activity card (under 60 characters). Put the full instructions in message." },
                        "agent_type": { "type": "string", "description": "Subagent profile or role name. Defaults to default. Built-in types include default, explorer, and worker; see the tool description for custom profiles." },
                        "agent": { "type": "string", "description": "Alias for agent_type." },
                        "category": { "type": "string", "description": "Optional semantic category label, such as quick, deep, explore, review, frontend, or writing." },
                        "model": { "type": "string", "description": "Optional per-call model for this child. Bare name uses the parent provider; provider:model selects cross-provider." },
                        "effort": { "type": "string", "enum": levels, "description": "Optional per-call thinking level for this child." },
                        "message": { "type": "string", "description": "Initial task for the subagent." },
                        "task": { "type": "string", "description": "Alias for message." },
                        "fork_context": { "type": "boolean", "description": "When true, copy recent parent conversation into the child thread." },
                    },
                    "additionalProperties": false,
                }),
            },
            ToolSpec {
                name: "wait_agent".into(),
                description: format!("Wait for one or more spawned subagents to reach a final status and return snapshots. {WAIT_GUIDANCE}"),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "agent_id": { "type": "string", "description": "A single agent id to wait for." },
                        "agent_ids": { "type": "array", "items": { "type": "string" }, "description": "Agent ids to wait for; returns when any target finishes. Prefer explicit IDs for remaining work. If omitted, considers all non-closed subagents, including already-finished ones." },
                        "timeout_ms": { "type": "number", "description": "Maximum wait time in milliseconds, not a mandatory delay; returns early when a target finishes. Choose according to your next decision. Defaults to 30000." },
                    },
                    "additionalProperties": false,
                }),
            },
            ToolSpec {
                name: "send_input".into(),
                description: "Send a follow-up message to an existing subagent thread. While running or queued, text input is queued and applied after its current turn. Use interrupt:true only to cancel and redirect it. A finished child continues with its context intact.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "agent_id": { "type": "string", "description": "Target subagent id." },
                        "message": { "type": "string", "description": "Follow-up message." },
                        "task": { "type": "string", "description": "Alias for message." },
                        "interrupt": { "type": "boolean", "description": "Cancel a running child before applying this input." },
                    },
                    "required": ["agent_id"],
                    "additionalProperties": false,
                }),
            },
            ToolSpec {
                name: "close_agent".into(),
                description: "Close a spawned subagent only when the delegated task is cancelled, stale, or no longer needed. Running children are cancelled before closing; do not close a child just because you started doing the same delegated work locally.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": { "agent_id": { "type": "string", "description": "Subagent id to close." } },
                    "required": ["agent_id"],
                    "additionalProperties": false,
                }),
            },
            ToolSpec {
                name: "list_agents".into(),
                description: "List this conversation's subagents with their current status. Use it to recall which children exist before spawning duplicates or narrating progress.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "status_filter": { "type": "array", "items": { "type": "string" }, "description": "Only include these statuses (queued, running, completed, failed, blocked, cancelled, closed)." },
                        "include_closed": { "type": "boolean", "description": "Include closed subagents. Defaults to false." },
                    },
                    "additionalProperties": false,
                }),
            },
        ]
    }

    pub fn owns(name: &str) -> bool {
        matches!(name, "spawn_agent" | "wait_agent" | "send_input" | "close_agent" | "list_agents")
    }

    pub async fn execute(self: &Arc<Self>, call: &ToolCall, history: &[ChatMessage]) -> ToolOutput {
        let args = call.arguments_json();
        let text = |key: &str| args[key].as_str().map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned);
        let error = |content: String| ToolOutput { content, is_error: true };
        match call.name.as_str() {
            "spawn_agent" => {
                let Some(message) = text("message").or_else(|| text("task")) else {
                    return error("Error: spawn_agent requires message or task.".into());
                };
                let effort = match args.get("effort").filter(|v| !v.is_null()) {
                    None => None,
                    Some(value) => match value.as_str().and_then(Level::parse) {
                        Some(level) => Some(level),
                        None => {
                            return error(format!(
                                "Error: effort must be one of off, minimal, low, medium, high, xhigh, max, ultra (got {value})."
                            ));
                        }
                    },
                };
                let fork = (args["fork_context"] == true).then(|| {
                    let start = history.len().saturating_sub(12);
                    history[start..].to_vec()
                });
                let options = SpawnOptions {
                    profile: text("agent_type").or_else(|| text("agent")).unwrap_or_else(|| "default".into()),
                    description: text("description"),
                    model: text("model"),
                    effort,
                    nickname: None,
                    fork_context: fork,
                    schema: None,
                    workflow: false,
                };
                match self.spawn(&message, options) {
                    Ok(snapshot) => ToolOutput {
                        content: [
                            format!("Spawned {} ({})", snapshot.nickname, snapshot.profile),
                            format!("agent_id: {}", snapshot.id),
                            format!("status: {}", snapshot.status.label()),
                            format!("route: {}", snapshot.model),
                            format!(
                                "next: continue useful non-overlapping work; call wait_agent for {} when its result is needed for your next step or final synthesis",
                                snapshot.id
                            ),
                            "counting: this spawn result creates one unique subagent; later wait_agent results for the same agent_id are updates, not additional subagents".into(),
                        ]
                        .join("\n"),
                        is_error: false,
                    },
                    Err(message) => error(message),
                }
            }
            "wait_agent" => {
                let mut ids: Vec<String> = Vec::new();
                if let Some(id) = text("agent_id") {
                    ids.push(id);
                }
                for id in args["agent_ids"].as_array().into_iter().flatten().filter_map(|v| v.as_str()) {
                    if !id.trim().is_empty() && !ids.iter().any(|i| i == id.trim()) {
                        ids.push(id.trim().to_owned());
                    }
                }
                let timeout = args["timeout_ms"]
                    .as_f64()
                    .filter(|n| *n > 0.0)
                    .map(|n| Duration::from_millis(n as u64).min(MAX_WAIT))
                    .unwrap_or(DEFAULT_WAIT);
                let snapshots = self.wait((!ids.is_empty()).then_some(ids), timeout).await;
                if snapshots.is_empty() {
                    return ToolOutput { content: "No subagents reached a final status before the timeout.".into(), is_error: false };
                }
                let mut lines = Vec::new();
                if snapshots.iter().all(|s| !s.status.is_final()) {
                    lines.push("wait_agent timed out before a delegated result was ready.".to_owned());
                    let queued = snapshots.iter().filter(|s| s.status == Status::Queued).count();
                    let running = snapshots.iter().filter(|s| s.status == Status::Running).count();
                    if queued > 0 {
                        lines.push(format!("{queued} queued for a concurrency slot; they start automatically."));
                    }
                    if running > 0 {
                        lines.push(format!("{running} still running."));
                    }
                    lines.push("Reassess whether a child's result blocks your next meaningful step: continue useful independent work if available, otherwise call wait_agent again with a suitable timeout. Do not duplicate the delegated work.".into());
                    lines.push(String::new());
                } else {
                    lines.push(format!("wait_agent: {} subagent{}", snapshots.len(), if snapshots.len() == 1 { "" } else { "s" }));
                    lines.push(String::new());
                }
                for snapshot in &snapshots {
                    lines.extend(format_snapshot(snapshot));
                    lines.push(String::new());
                }
                let failed = snapshots.iter().all(|s| matches!(s.status, Status::Failed | Status::Blocked));
                ToolOutput { content: lines.join("\n").trim().to_owned(), is_error: failed }
            }
            "send_input" => {
                let (Some(id), Some(message)) = (text("agent_id"), text("message").or_else(|| text("task"))) else {
                    return error("Error: send_input requires agent_id and message.".into());
                };
                match self.send(&id, &message, args["interrupt"] == true) {
                    Ok(snapshot) => ToolOutput {
                        content: format!(
                            "{} {} ({})\nagent_id: {}\nstatus: {}",
                            if snapshot.pending_inputs > 0 { "Queued input for" } else { "Sent input to" },
                            snapshot.nickname,
                            snapshot.profile,
                            snapshot.id,
                            snapshot.status.label()
                        ),
                        is_error: false,
                    },
                    Err(message) => error(message),
                }
            }
            "close_agent" => {
                let Some(id) = text("agent_id") else { return error("Error: close_agent requires agent_id.".into()) };
                match self.close(&id) {
                    Ok(snapshot) => ToolOutput {
                        content: format!(
                            "Closed {} ({})\nagent_id: {}\nstatus: {}",
                            snapshot.nickname,
                            snapshot.profile,
                            snapshot.id,
                            snapshot.status.label()
                        ),
                        is_error: false,
                    },
                    Err(message) => error(message),
                }
            }
            _ => {
                let filter: Vec<String> =
                    args["status_filter"].as_array().into_iter().flatten().filter_map(|v| v.as_str().map(str::to_owned)).collect();
                let include_closed = args["include_closed"] == true;
                let shown: Vec<Snapshot> = self
                    .snapshots()
                    .into_iter()
                    .filter(|s| include_closed || s.status != Status::Closed)
                    .filter(|s| filter.is_empty() || filter.iter().any(|f| f == s.status.label()))
                    .collect();
                if shown.is_empty() {
                    return ToolOutput {
                        content: "No subagents match. Spawn one with spawn_agent when delegation helps.".into(),
                        is_error: false,
                    };
                }
                let mut lines = vec![format!("{} subagent{}:", shown.len(), if shown.len() == 1 { "" } else { "s" })];
                for s in &shown {
                    lines.push(format!(
                        "- {} ({}) agent_id={} status={} task={}",
                        s.nickname,
                        s.profile,
                        s.id,
                        s.status.label(),
                        one_line(&s.task, 80)
                    ));
                }
                ToolOutput { content: lines.join("\n"), is_error: false }
            }
        }
    }
}

fn one_line(text: &str, max: usize) -> String {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() <= max { line } else { format!("{}...", line.chars().take(max - 3).collect::<String>()) }
}

fn format_snapshot(snapshot: &Snapshot) -> Vec<String> {
    let mut lines = vec![
        format!("## {} ({})", snapshot.nickname, snapshot.profile),
        format!("agent_id: {}", snapshot.id),
        format!("status: {}", snapshot.status.label()),
        format!("route: {}", snapshot.model),
        format!("task: {}", snapshot.task),
    ];
    if snapshot.status == Status::Queued {
        lines.push("queued: waiting for a concurrency slot; it starts automatically — no action needed".into());
    }
    if !snapshot.summary.is_empty() {
        lines.push(String::new());
        lines.push("Summary:".into());
        lines.push(snapshot.summary.clone());
    } else if snapshot.status == Status::Completed {
        lines.push(String::new());
        lines.push("Summary: (no final text summary was produced)".into());
    }
    if !snapshot.notes.is_empty() {
        lines.push(String::new());
        lines.push("Recent tool notes:".into());
        let start = snapshot.notes.len().saturating_sub(8);
        lines.extend(snapshot.notes[start..].iter().map(|n| format!("- {n}")));
    }
    if let Some(error) = &snapshot.error {
        lines.push(String::new());
        lines.push(format!("Error: {error}"));
    }
    if matches!(snapshot.status, Status::Failed | Status::Cancelled) {
        lines.push(String::new());
        lines.push(format!(
            "resume: call send_input with agent_id {} to continue this child with its context intact",
            snapshot.id
        ));
    }
    lines
}

fn child_system(nickname: &str, profile: &Profile, env: &Env, tools: &HashSet<String>) -> String {
    let mut sections = vec![
        format!(
            "You are subagent {nickname} of Bubble, working for a parent agent. Your agent profile is {}.",
            profile.name
        ),
        profile.prompt.clone(),
        format!(
            "You work in the folder {}; relative paths start there. You are read-only: you cannot change files or run commands, and you cannot ask the person anything. Your final message is returned to the parent as your answer.",
            env.cwd.display()
        ),
    ];
    if tools.contains("memory") {
        if let Some(memory) = crate::memory::prompt(&crate::memory::root()) {
            sections.push(memory);
        }
    }
    if let Some(project) = instructions::instructions(Some(&env.cwd)) {
        sections.push(project);
    }
    sections.into_iter().filter(|s| !s.trim().is_empty()).collect::<Vec<_>>().join("\n\n")
}

fn estimate_tokens(text: &str) -> f64 {
    text.chars()
        .map(|c| {
            let code = c as u32;
            let cjk = (0x2e80..=0x9fff).contains(&code) || (0xac00..=0xd7af).contains(&code) || (0xf900..=0xfaff).contains(&code);
            if cjk { 1.0 } else { 0.25 }
        })
        .sum()
}

fn intermediate(text: &str) -> bool {
    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        return false;
    }
    let lower = normalized.to_lowercase();
    const EN: &[&str] = &["let me", "i'll", "i will", "i need to", "i should", "i'm going to", "now i'll", "now i will"];
    const ZH: &[&str] = &["接下来", "下一步", "让我", "我将", "我先", "我来", "现在我", "我需要", "然后我"];
    EN.iter().any(|p| lower.starts_with(p) && lower[p.len()..].chars().next().is_none_or(|c| !c.is_alphanumeric()))
        || ZH.iter().any(|p| normalized.starts_with(p))
}

/// Whether a child that used tools ended without a real handoff.
fn needs_final_summary(summary: &str, used_tools: bool, structured: bool) -> bool {
    if summary.trim().is_empty() {
        return used_tools;
    }
    if !used_tools {
        return false;
    }
    (!structured && estimate_tokens(summary) < HANDOFF_TOKEN_FLOOR) || intermediate(summary)
}

/// Turns tools::describe-style progress into a short label.
pub fn label(call: &ToolCall) -> String {
    tools::describe(call)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT_DIR: AtomicUsize = AtomicUsize::new(0);

    fn temp() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("bubble-delegate-{}-{}", std::process::id(), NEXT_DIR.fetch_add(1, Ordering::SeqCst)));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn profiles_merge_builtins_user_and_project() {
        let home = temp();
        let project = temp();
        std::fs::create_dir_all(home.join("agents")).unwrap();
        std::fs::write(
            home.join("agents/reviewer.md"),
            "---\nname: reviewer\ndescription: Reviews diffs\nmodel: kimi:k3\ntools:\n  preset: readonly\n  exclude:\n    - web_fetch\nnicknames: [Rex, Rio]\n---\nReview carefully.\n",
        )
        .unwrap();
        std::fs::create_dir_all(project.join(".bubble/agents")).unwrap();
        std::fs::write(project.join(".bubble/agents/explorer.md"), "---\ndescription: Project explorer\ntools:\n  - read\n  - grep\n---\nLook around.").unwrap();
        std::fs::write(project.join(".bubble/agents/broken.md"), "no front matter").unwrap();
        let all = profiles(&home, Some(&project.join("src")));
        let reviewer = find(&all, "reviewer").unwrap();
        assert_eq!(reviewer.source, "user");
        assert_eq!(reviewer.model.as_deref(), Some("kimi:k3"));
        assert!(!reviewer.tools.contains(&"web_fetch".to_owned()) && reviewer.tools.contains(&"read".to_owned()));
        assert_eq!(reviewer.nicknames, ["Rex", "Rio"]);
        assert_eq!(reviewer.prompt, "Review carefully.");
        let explorer = find(&all, "explorer").unwrap();
        assert_eq!((explorer.source, explorer.tools.clone()), ("project", vec!["read".to_owned(), "grep".to_owned()]));
        assert_eq!(find(&all, "search").unwrap().name, "builtin:search");
        assert!(find(&all, "broken").is_none());
    }

    #[test]
    fn handoffs_that_only_narrate_get_a_final_summary() {
        assert!(needs_final_summary("", true, false));
        assert!(!needs_final_summary("", false, false));
        assert!(needs_final_summary("Let me check the other file.", true, false));
        assert!(needs_final_summary("让我再看看这个文件，然后总结一下目前发现的所有问题以及对应的修复方案和需要注意的地方，最后给出完整的结论。", true, false));
        assert!(!needs_final_summary("ok", false, false), "a short direct answer is complete");
        assert!(!needs_final_summary(&"The config is loaded in src/config.rs. ".repeat(10), true, false));
        assert!(!needs_final_summary("{\"ok\":true}", true, true), "short JSON is complete");
    }
}
