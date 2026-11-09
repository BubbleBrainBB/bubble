//! Bubble Native's storage: one SQLite file holding tasks, their runs (agent
//! sessions), messages, GitHub links, external events and settings.
//!
//! A task is the unit of work a person cares about; a run is one execution of
//! it (a session of the agent). What a task "needs from you right now" is not
//! stored: it is derived from the task's lifecycle and its latest run, so the
//! two can never disagree (docs/desktop-manager-design.md §2).

use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};

/// The schema version this build writes (`PRAGMA user_version`).
const SCHEMA_VERSION: i64 = 3;

/// Milliseconds since the Unix epoch.
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn new_id(prefix: &str) -> String {
    format!("{prefix}_{}", uuid::Uuid::new_v4().simple())
}

/// What a person or the Manager says about a task.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Lifecycle {
    Open,
    Waiting,
    Done,
    Canceled,
}

impl Lifecycle {
    fn as_str(self) -> &'static str {
        match self {
            Lifecycle::Open => "open",
            Lifecycle::Waiting => "waiting",
            Lifecycle::Done => "done",
            Lifecycle::Canceled => "canceled",
        }
    }

    fn parse(value: &str) -> Result<Self> {
        Ok(match value {
            "open" => Lifecycle::Open,
            "waiting" => Lifecycle::Waiting,
            "done" => Lifecycle::Done,
            "canceled" => Lifecycle::Canceled,
            other => bail!("unknown task lifecycle {other:?}"),
        })
    }
}

/// Where a task came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Origin {
    Manager,
    User,
    Github,
    /// A session of the command-line agent.
    Cli,
}

impl Origin {
    fn as_str(self) -> &'static str {
        match self {
            Origin::Manager => "manager",
            Origin::User => "user",
            Origin::Github => "github",
            Origin::Cli => "cli",
        }
    }

    fn parse(value: &str) -> Result<Self> {
        Ok(match value {
            "manager" => Origin::Manager,
            "user" => Origin::User,
            "github" => Origin::Github,
            "cli" => Origin::Cli,
            other => bail!("unknown task origin {other:?}"),
        })
    }
}

/// What a run is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RunRole {
    /// The Home conversation.
    Manager,
    /// One execution of a task.
    Task,
    /// Sorting incoming events.
    Triage,
}

impl RunRole {
    fn as_str(self) -> &'static str {
        match self {
            RunRole::Manager => "manager",
            RunRole::Task => "task",
            RunRole::Triage => "triage",
        }
    }

    fn parse(value: &str) -> Result<Self> {
        Ok(match value {
            "manager" => RunRole::Manager,
            "task" => RunRole::Task,
            "triage" => RunRole::Triage,
            other => bail!("unknown run role {other:?}"),
        })
    }
}

/// Where a run stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RunStatus {
    /// Waiting for a free slot under the concurrency limit.
    Queued,
    Running,
    /// Stopped on an approval or a question.
    NeedsInput,
    Failed,
    /// Finished its turn; the agent is idle.
    Idle,
}

impl RunStatus {
    fn as_str(self) -> &'static str {
        match self {
            RunStatus::Queued => "queued",
            RunStatus::Running => "running",
            RunStatus::NeedsInput => "needs_input",
            RunStatus::Failed => "failed",
            RunStatus::Idle => "idle",
        }
    }

    fn parse(value: &str) -> Result<Self> {
        Ok(match value {
            "queued" => RunStatus::Queued,
            "running" => RunStatus::Running,
            "needs_input" => RunStatus::NeedsInput,
            "failed" => RunStatus::Failed,
            "idle" => RunStatus::Idle,
            other => bail!("unknown run status {other:?}"),
        })
    }
}

/// What a task wants from a person now, derived, never stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TaskState {
    NeedsYou,
    Running,
    Queued,
    Failed,
    /// The agent finished its turn on a task nobody has closed: look at it.
    Review,
    NotStarted,
    Waiting,
    Done,
    Canceled,
}

impl TaskState {
    /// Lower comes first in lists: what wants a person, then what moves.
    pub fn rank(self) -> u8 {
        match self {
            TaskState::NeedsYou => 0,
            TaskState::Failed => 1,
            TaskState::Review => 2,
            TaskState::Running => 3,
            TaskState::Queued => 4,
            TaskState::NotStarted => 5,
            TaskState::Waiting => 6,
            TaskState::Done => 7,
            TaskState::Canceled => 8,
        }
    }

    /// Whether the task is still in motion (the sidebar's Active list).
    pub fn is_active(self) -> bool {
        matches!(
            self,
            TaskState::NeedsYou | TaskState::Running | TaskState::Queued | TaskState::Failed
        )
    }
}

/// Derives a task's state from its lifecycle and its latest run.
pub fn derive_task_state(lifecycle: Lifecycle, latest_run: Option<RunStatus>) -> TaskState {
    match lifecycle {
        Lifecycle::Done => return TaskState::Done,
        Lifecycle::Canceled => return TaskState::Canceled,
        Lifecycle::Open | Lifecycle::Waiting => {}
    }
    match latest_run {
        Some(RunStatus::NeedsInput) => TaskState::NeedsYou,
        Some(RunStatus::Running) => TaskState::Running,
        Some(RunStatus::Queued) => TaskState::Queued,
        Some(RunStatus::Failed) => TaskState::Failed,
        Some(RunStatus::Idle) | None if lifecycle == Lifecycle::Waiting => TaskState::Waiting,
        Some(RunStatus::Idle) => TaskState::Review,
        None => TaskState::NotStarted,
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub title: String,
    pub project_cwd: Option<String>,
    pub lifecycle: Lifecycle,
    pub waiting_on: Option<String>,
    pub origin: Origin,
    pub created_at: i64,
    pub updated_at: i64,
    pub closed_at: Option<i64>,
}

/// A task with what lists show about it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TaskSummary {
    pub task: Task,
    pub state: TaskState,
    pub latest_run_id: Option<String>,
    pub last_error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Run {
    pub id: String,
    pub task_id: Option<String>,
    pub role: RunRole,
    pub model: Option<String>,
    pub cwd: Option<String>,
    pub status: RunStatus,
    pub last_error: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// Who said a message.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MessageRole {
    User,
    Assistant,
    /// A note from the system (a brief from the Manager, an incoming event).
    System,
    /// Tool results going back to the model.
    Tool,
    /// Something that happened elsewhere (a review on GitHub) told to the
    /// agent.
    External,
    /// The model's history from here on, compacted (JSON list of messages).
    Checkpoint,
}

impl MessageRole {
    fn as_str(self) -> &'static str {
        match self {
            MessageRole::User => "user",
            MessageRole::Assistant => "assistant",
            MessageRole::System => "system",
            MessageRole::Tool => "tool",
            MessageRole::External => "external",
            MessageRole::Checkpoint => "checkpoint",
        }
    }

    fn parse(value: &str) -> Result<Self> {
        Ok(match value {
            "user" => MessageRole::User,
            "assistant" => MessageRole::Assistant,
            "system" => MessageRole::System,
            "tool" => MessageRole::Tool,
            "external" => MessageRole::External,
            "checkpoint" => MessageRole::Checkpoint,
            other => bail!("unknown message role {other:?}"),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    pub run_id: String,
    pub seq: i64,
    pub role: MessageRole,
    pub text: String,
    /// The message as the model sees it (JSON), when it is more than text:
    /// tool calls, tool results, reasoning.
    pub content: Option<String>,
    pub created_at: i64,
}

/// A task's link to something outside: a GitHub issue or pull request.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Link {
    pub id: String,
    pub task_id: String,
    /// `github_issue` or `github_pr`.
    pub kind: String,
    pub repo: String,
    pub number: i64,
    /// The last look at it (JSON), to tell what changed.
    pub snapshot: Option<String>,
    pub polled_at: Option<i64>,
}

/// Something that happened outside, and what became of it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub id: String,
    pub source: String,
    pub dedupe_key: String,
    pub kind: String,
    pub repo: Option<String>,
    pub number: Option<i64>,
    pub actor: Option<String>,
    pub payload: String,
    pub task_id: Option<String>,
    /// `pending` (waiting to reach its task), `attached`, `created`,
    /// `ignored`.
    pub disposition: String,
    pub created_at: i64,
}

pub struct NewEvent<'a> {
    pub source: &'a str,
    pub dedupe_key: &'a str,
    pub kind: &'a str,
    pub repo: Option<&'a str>,
    pub number: Option<i64>,
    pub actor: Option<&'a str>,
    pub payload: &'a str,
    pub task_id: Option<&'a str>,
    pub disposition: &'a str,
}

pub struct NewTask<'a> {
    pub title: &'a str,
    pub project_cwd: Option<&'a str>,
    pub origin: Origin,
}

pub struct NewRun<'a> {
    pub task_id: Option<&'a str>,
    pub role: RunRole,
    pub model: Option<&'a str>,
    pub cwd: Option<&'a str>,
    pub status: RunStatus,
}

/// The store. Cheap to clone; every clone shares one connection.
#[derive(Clone)]
pub struct Store {
    conn: Arc<Mutex<Connection>>,
}

impl Store {
    /// Opens (and creates or migrates) the database at `path`.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        // The app and the CLI may write at once.
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        Self::from_connection(conn)
    }

    /// A store that lives in memory (tests).
    pub fn open_in_memory() -> Result<Self> {
        Self::from_connection(Connection::open_in_memory()?)
    }

    fn from_connection(conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "foreign_keys", "ON")?;
        migrate(&conn)?;
        Ok(Store { conn: Arc::new(Mutex::new(conn)) })
    }

    fn conn(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    // --- Tasks ---------------------------------------------------------------

    pub fn create_task(&self, new: NewTask<'_>) -> Result<Task> {
        let now = now_ms();
        let task = Task {
            id: new_id("task"),
            title: new.title.trim().to_owned(),
            project_cwd: new.project_cwd.map(str::to_owned),
            lifecycle: Lifecycle::Open,
            waiting_on: None,
            origin: new.origin,
            created_at: now,
            updated_at: now,
            closed_at: None,
        };
        if task.title.is_empty() {
            bail!("a task needs a title");
        }
        self.conn().execute(
            "INSERT INTO tasks (id, title, project_cwd, lifecycle, waiting_on, origin, created_at, updated_at, closed_at)
             VALUES (?1, ?2, ?3, ?4, NULL, ?5, ?6, ?6, NULL)",
            params![
                task.id,
                task.title,
                task.project_cwd,
                task.lifecycle.as_str(),
                task.origin.as_str(),
                now
            ],
        )?;
        Ok(task)
    }

    pub fn get_task(&self, id: &str) -> Result<Option<Task>> {
        Ok(self
            .conn()
            .query_row("SELECT * FROM tasks WHERE id = ?1", [id], task_from_row)
            .optional()?)
    }

    pub fn set_task_lifecycle(&self, id: &str, lifecycle: Lifecycle, waiting_on: Option<&str>) -> Result<()> {
        let now = now_ms();
        let closed_at = matches!(lifecycle, Lifecycle::Done | Lifecycle::Canceled).then_some(now);
        let changed = self.conn().execute(
            "UPDATE tasks SET lifecycle = ?2, waiting_on = ?3, updated_at = ?4, closed_at = ?5 WHERE id = ?1",
            params![id, lifecycle.as_str(), waiting_on, now, closed_at],
        )?;
        if changed == 0 {
            bail!("no task {id}");
        }
        Ok(())
    }

    pub fn rename_task(&self, id: &str, title: &str) -> Result<()> {
        let title = title.trim();
        if title.is_empty() {
            bail!("a task needs a title");
        }
        self.conn().execute(
            "UPDATE tasks SET title = ?2, updated_at = ?3 WHERE id = ?1",
            params![id, title, now_ms()],
        )?;
        Ok(())
    }

    /// Every task with its derived state, what wants a person first, then
    /// the most recently changed.
    pub fn list_tasks(&self) -> Result<Vec<TaskSummary>> {
        let conn = self.conn();
        let mut statement = conn.prepare(
            "SELECT t.*, r.id AS run_id, r.status AS run_status, r.last_error AS run_error
             FROM tasks t
             LEFT JOIN runs r ON r.id = (
               SELECT id FROM runs WHERE task_id = t.id ORDER BY created_at DESC, rowid DESC LIMIT 1
             )",
        )?;
        let mut summaries = statement
            .query_map([], |row| {
                let task = task_from_row(row)?;
                let run_id: Option<String> = row.get("run_id")?;
                let run_status: Option<String> = row.get("run_status")?;
                let run_error: Option<String> = row.get("run_error")?;
                Ok((task, run_id, run_status, run_error))
            })?
            .map(|row| -> Result<TaskSummary> {
                let (task, latest_run_id, run_status, last_error) = row?;
                let status = run_status.as_deref().map(RunStatus::parse).transpose()?;
                Ok(TaskSummary {
                    state: derive_task_state(task.lifecycle, status),
                    task,
                    latest_run_id,
                    last_error,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        summaries.sort_by(|a, b| {
            a.state
                .rank()
                .cmp(&b.state.rank())
                .then(b.task.updated_at.cmp(&a.task.updated_at))
        });
        Ok(summaries)
    }

    // --- Runs ----------------------------------------------------------------

    pub fn create_run(&self, new: NewRun<'_>) -> Result<Run> {
        let now = now_ms();
        let run = Run {
            id: new_id("run"),
            task_id: new.task_id.map(str::to_owned),
            role: new.role,
            model: new.model.map(str::to_owned),
            cwd: new.cwd.map(str::to_owned),
            status: new.status,
            last_error: None,
            created_at: now,
            updated_at: now,
        };
        let conn = self.conn();
        conn.execute(
            "INSERT INTO runs (id, task_id, role, model, cwd, status, last_error, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, ?7, ?7)",
            params![
                run.id,
                run.task_id,
                run.role.as_str(),
                run.model,
                run.cwd,
                run.status.as_str(),
                now
            ],
        )?;
        if let Some(task_id) = &run.task_id {
            conn.execute("UPDATE tasks SET updated_at = ?2 WHERE id = ?1", params![task_id, now])?;
        }
        Ok(run)
    }

    pub fn get_run(&self, id: &str) -> Result<Option<Run>> {
        Ok(self
            .conn()
            .query_row("SELECT * FROM runs WHERE id = ?1", [id], run_from_row)
            .optional()?)
    }

    pub fn runs_for_task(&self, task_id: &str) -> Result<Vec<Run>> {
        let conn = self.conn();
        let mut statement =
            conn.prepare("SELECT * FROM runs WHERE task_id = ?1 ORDER BY created_at, rowid")?;
        let runs = statement
            .query_map([task_id], run_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(runs)
    }

    pub fn set_run_status(&self, id: &str, status: RunStatus, last_error: Option<&str>) -> Result<()> {
        let now = now_ms();
        let conn = self.conn();
        let changed = conn.execute(
            "UPDATE runs SET status = ?2, last_error = ?3, updated_at = ?4 WHERE id = ?1",
            params![id, status.as_str(), last_error, now],
        )?;
        if changed == 0 {
            bail!("no run {id}");
        }
        conn.execute(
            "UPDATE tasks SET updated_at = ?2 WHERE id = (SELECT task_id FROM runs WHERE id = ?1)",
            params![id, now],
        )?;
        Ok(())
    }

    /// The Home conversation's run: the latest manager run, made when there
    /// is none.
    pub fn manager_run(&self, model: Option<&str>) -> Result<Run> {
        let existing = self
            .conn()
            .query_row(
                "SELECT * FROM runs WHERE role = 'manager' ORDER BY created_at DESC, rowid DESC LIMIT 1",
                [],
                run_from_row,
            )
            .optional()?;
        match existing {
            Some(run) => Ok(run),
            None => self.create_run(NewRun {
                task_id: None,
                role: RunRole::Manager,
                model,
                cwd: None,
                status: RunStatus::Idle,
            }),
        }
    }

    /// Marks runs left running by a previous launch as failed: nothing is
    /// driving them any more.
    pub fn recover_interrupted_runs(&self) -> Result<usize> {
        Ok(self.conn().execute(
            "UPDATE runs SET status = 'failed', last_error = 'Interrupted when Bubble quit', updated_at = ?1
             WHERE status IN ('running', 'needs_input')",
            [now_ms()],
        )?)
    }

    // --- Messages ------------------------------------------------------------

    pub fn append_message(&self, run_id: &str, role: MessageRole, text: &str) -> Result<Message> {
        self.append_message_with(run_id, role, text, None)
    }

    /// Appends a message with its structured `content` (JSON).
    pub fn append_message_with(&self, run_id: &str, role: MessageRole, text: &str, content: Option<&str>) -> Result<Message> {
        let conn = self.conn();
        let seq: i64 = conn.query_row(
            "SELECT COALESCE(MAX(seq), 0) + 1 FROM messages WHERE run_id = ?1",
            [run_id],
            |row| row.get(0),
        )?;
        let message = Message {
            id: new_id("msg"),
            run_id: run_id.to_owned(),
            seq,
            role,
            text: text.to_owned(),
            content: content.map(str::to_owned),
            created_at: now_ms(),
        };
        conn.execute(
            "INSERT INTO messages (id, run_id, seq, role, text, content, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                message.id,
                message.run_id,
                message.seq,
                message.role.as_str(),
                message.text,
                message.content,
                message.created_at
            ],
        )?;
        Ok(message)
    }

    /// Replaces a message's text (a streamed reply saved as it grows).
    pub fn set_message_text(&self, id: &str, text: &str) -> Result<()> {
        self.conn()
            .execute("UPDATE messages SET text = ?2 WHERE id = ?1", params![id, text])?;
        Ok(())
    }

    pub fn messages(&self, run_id: &str) -> Result<Vec<Message>> {
        let conn = self.conn();
        let mut statement =
            conn.prepare("SELECT * FROM messages WHERE run_id = ?1 ORDER BY seq")?;
        let messages = statement
            .query_map([run_id], message_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(messages)
    }

    // --- Links ---------------------------------------------------------------

    /// Links `task_id` to an issue or a pull request; the task it already
    /// belongs to when it is linked elsewhere.
    pub fn link(&self, task_id: &str, kind: &str, repo: &str, number: i64) -> Result<String> {
        let conn = self.conn();
        let existing: Option<String> = conn
            .query_row(
                "SELECT task_id FROM task_links WHERE kind = ?1 AND repo = ?2 AND number = ?3",
                params![kind, repo, number],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(owner) = existing {
            return Ok(owner);
        }
        conn.execute(
            "INSERT INTO task_links (id, task_id, kind, repo, number, snapshot, polled_at) VALUES (?1, ?2, ?3, ?4, ?5, NULL, NULL)",
            params![new_id("link"), task_id, kind, repo, number],
        )?;
        Ok(task_id.to_owned())
    }

    pub fn links_for_task(&self, task_id: &str) -> Result<Vec<Link>> {
        let conn = self.conn();
        let mut statement = conn.prepare("SELECT * FROM task_links WHERE task_id = ?1 ORDER BY rowid")?;
        let links = statement.query_map([task_id], link_from_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(links)
    }

    /// The task linked to an issue or a pull request.
    pub fn task_for_link(&self, kind: &str, repo: &str, number: i64) -> Result<Option<String>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT task_id FROM task_links WHERE kind = ?1 AND repo = ?2 AND number = ?3",
                params![kind, repo, number],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// Links whose task is still open: the ones to keep an eye on.
    pub fn watched_links(&self) -> Result<Vec<Link>> {
        let conn = self.conn();
        let mut statement = conn.prepare(
            "SELECT l.* FROM task_links l JOIN tasks t ON t.id = l.task_id
             WHERE t.lifecycle IN ('open', 'waiting') ORDER BY l.rowid",
        )?;
        let links = statement.query_map([], link_from_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(links)
    }

    pub fn set_link_snapshot(&self, id: &str, snapshot: &str) -> Result<()> {
        self.conn().execute(
            "UPDATE task_links SET snapshot = ?2, polled_at = ?3 WHERE id = ?1",
            params![id, snapshot, now_ms()],
        )?;
        Ok(())
    }

    // --- Events --------------------------------------------------------------

    /// Records an event; `None` when one with the same key was seen before.
    pub fn record_event(&self, new: NewEvent<'_>) -> Result<Option<Event>> {
        let event = Event {
            id: new_id("evt"),
            source: new.source.to_owned(),
            dedupe_key: new.dedupe_key.to_owned(),
            kind: new.kind.to_owned(),
            repo: new.repo.map(str::to_owned),
            number: new.number,
            actor: new.actor.map(str::to_owned),
            payload: new.payload.to_owned(),
            task_id: new.task_id.map(str::to_owned),
            disposition: new.disposition.to_owned(),
            created_at: now_ms(),
        };
        let inserted = self.conn().execute(
            "INSERT OR IGNORE INTO events (id, source, dedupe_key, kind, repo, number, actor, payload, task_id, disposition, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                event.id,
                event.source,
                event.dedupe_key,
                event.kind,
                event.repo,
                event.number,
                event.actor,
                event.payload,
                event.task_id,
                event.disposition,
                event.created_at
            ],
        )?;
        Ok((inserted == 1).then_some(event))
    }

    pub fn set_event_disposition(&self, id: &str, disposition: &str, task_id: Option<&str>) -> Result<()> {
        self.conn().execute(
            "UPDATE events SET disposition = ?2, task_id = COALESCE(?3, task_id) WHERE id = ?1",
            params![id, disposition, task_id],
        )?;
        Ok(())
    }

    /// Events still waiting to reach their task.
    pub fn pending_events(&self) -> Result<Vec<Event>> {
        let conn = self.conn();
        let mut statement =
            conn.prepare("SELECT * FROM events WHERE disposition = 'pending' AND task_id IS NOT NULL ORDER BY created_at, rowid")?;
        let events = statement.query_map([], event_from_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(events)
    }

    pub fn events_for_task(&self, task_id: &str) -> Result<Vec<Event>> {
        let conn = self.conn();
        let mut statement = conn.prepare("SELECT * FROM events WHERE task_id = ?1 ORDER BY created_at, rowid")?;
        let events = statement.query_map([task_id], event_from_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(events)
    }

    // --- Cursors ---------------------------------------------------------------

    pub fn cursor(&self, source: &str) -> Result<Option<String>> {
        Ok(self
            .conn()
            .query_row("SELECT cursor FROM ingest_cursors WHERE source = ?1", [source], |row| row.get(0))
            .optional()?
            .flatten())
    }

    pub fn set_cursor(&self, source: &str, cursor: &str) -> Result<()> {
        self.conn().execute(
            "INSERT INTO ingest_cursors (source, cursor, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(source) DO UPDATE SET cursor = excluded.cursor, updated_at = excluded.updated_at",
            params![source, cursor, now_ms()],
        )?;
        Ok(())
    }

    // --- Settings ------------------------------------------------------------

    pub fn setting(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn()
            .query_row("SELECT value FROM settings WHERE key = ?1", [key], |row| row.get(0))
            .optional()?)
    }

    pub fn set_setting(&self, key: &str, value: Option<&str>) -> Result<()> {
        let conn = self.conn();
        match value {
            Some(value) => conn.execute(
                "INSERT INTO settings (key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, value],
            )?,
            None => conn.execute("DELETE FROM settings WHERE key = ?1", [key])?,
        };
        Ok(())
    }
}

/// The model's view of stored messages: a checkpoint replaces what came
/// before it, and a tool call whose results never came (the process quit
/// mid-turn) gets an "interrupted" result so the history stays valid.
pub fn history(messages: &[Message]) -> Vec<bubble_core::ChatMessage> {
    use bubble_core::ChatMessage;
    fn close_open_calls(history: &mut Vec<ChatMessage>, next: &ChatMessage) {
        let Some(ChatMessage::Assistant { tool_calls, .. }) = history.last() else { return };
        if tool_calls.is_empty() || matches!(next, ChatMessage::Tool { .. }) {
            return;
        }
        let results = tool_calls
            .iter()
            .map(|call| bubble_core::ToolResult {
                call_id: call.id.clone(),
                content: "Interrupted before it ran.".into(),
                is_error: true,
            })
            .collect();
        history.push(ChatMessage::Tool { results });
    }
    let mut history: Vec<ChatMessage> = Vec::new();
    for message in messages {
        if message.role == MessageRole::Checkpoint {
            if let Some(compacted) = message.content.as_deref().and_then(|c| serde_json::from_str::<Vec<ChatMessage>>(c).ok()) {
                history = compacted;
            }
            continue;
        }
        let parsed = message.content.as_deref().and_then(|c| serde_json::from_str::<ChatMessage>(c).ok());
        let item = match (message.role, parsed) {
            (_, Some(parsed)) => parsed,
            (MessageRole::User, None) => ChatMessage::user(message.text.clone()),
            (MessageRole::Assistant, None) => ChatMessage::assistant(message.text.clone()),
            (MessageRole::System | MessageRole::Tool | MessageRole::External | MessageRole::Checkpoint, None) => continue,
        };
        if let ChatMessage::Assistant { text, tool_calls, .. } = &item {
            if text.trim().is_empty() && tool_calls.is_empty() {
                continue;
            }
        }
        close_open_calls(&mut history, &item);
        history.push(item);
    }
    close_open_calls(&mut history, &ChatMessage::user(""));
    history
}

impl Store {
    /// The newest command-line session in a folder.
    pub fn latest_cli_task(&self, cwd: &str) -> Result<Option<Task>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT * FROM tasks WHERE origin = 'cli' AND project_cwd = ?1 ORDER BY updated_at DESC LIMIT 1",
                [cwd],
                task_from_row,
            )
            .optional()?)
    }

    /// A folder's command-line sessions that hold a conversation, newest
    /// first.
    pub fn cli_tasks(&self, cwd: &str, limit: usize) -> Result<Vec<Task>> {
        let conn = self.conn();
        let mut statement = conn.prepare(
            "SELECT * FROM tasks t WHERE origin = 'cli' AND project_cwd = ?1
               AND EXISTS (SELECT 1 FROM messages m JOIN runs r ON r.id = m.run_id WHERE r.task_id = t.id)
             ORDER BY updated_at DESC LIMIT ?2",
        )?;
        let tasks = statement.query_map(params![cwd, limit as i64], task_from_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(tasks)
    }

    /// Removes task `id` if none of its runs holds a message (a session
    /// opened and left without a word); whether it did.
    pub fn discard_if_empty(&self, id: &str) -> Result<bool> {
        let conn = self.conn();
        let messages: i64 = conn.query_row(
            "SELECT COUNT(*) FROM messages JOIN runs ON runs.id = messages.run_id WHERE runs.task_id = ?1",
            [id],
            |row| row.get(0),
        )?;
        if messages > 0 {
            return Ok(false);
        }
        Ok(conn.execute("DELETE FROM tasks WHERE id = ?1", [id])? > 0)
    }

    /// A named command-line session in a folder.
    pub fn cli_task_named(&self, cwd: &str, title: &str) -> Result<Option<Task>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT * FROM tasks WHERE origin = 'cli' AND project_cwd = ?1 AND title = ?2 ORDER BY updated_at DESC LIMIT 1",
                params![cwd, title],
                task_from_row,
            )
            .optional()?)
    }
}

/// What memory phase 1 took from one conversation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stage1 {
    /// `task:<id>`, or a TypeScript session file.
    pub source: String,
    pub cwd: String,
    /// How many messages it had when read.
    pub entry_count: i64,
    /// ISO time of its newest message.
    pub source_updated_at: String,
    pub generated_at: i64,
    pub raw_memory: String,
    pub rollout_summary: String,
    pub slug: Option<String>,
    /// Picked by the last successful phase 2.
    pub selected: bool,
}

fn stage1_from_row(row: &Row<'_>) -> rusqlite::Result<Stage1> {
    Ok(Stage1 {
        source: row.get("source")?,
        cwd: row.get("cwd")?,
        entry_count: row.get("entry_count")?,
        source_updated_at: row.get("source_updated_at")?,
        generated_at: row.get("generated_at")?,
        raw_memory: row.get("raw_memory")?,
        rollout_summary: row.get("rollout_summary")?,
        slug: row.get("slug")?,
        selected: row.get::<_, i64>("selected")? != 0,
    })
}

impl Store {
    pub fn stage1(&self, source: &str) -> Result<Option<Stage1>> {
        Ok(self.conn().query_row("SELECT * FROM memory_stage1 WHERE source = ?1", [source], stage1_from_row).optional()?)
    }

    pub fn upsert_stage1(&self, stage1: &Stage1) -> Result<()> {
        self.conn().execute(
            "INSERT INTO memory_stage1 (source, cwd, entry_count, source_updated_at, generated_at, raw_memory, rollout_summary, slug, selected)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 0)
             ON CONFLICT(source) DO UPDATE SET cwd = ?2, entry_count = ?3, source_updated_at = ?4, generated_at = ?5,
               raw_memory = ?6, rollout_summary = ?7, slug = ?8",
            params![
                stage1.source,
                stage1.cwd,
                stage1.entry_count,
                stage1.source_updated_at,
                stage1.generated_at,
                stage1.raw_memory,
                stage1.rollout_summary,
                stage1.slug
            ],
        )?;
        Ok(())
    }

    /// Every extraction, newest conversation first.
    pub fn all_stage1(&self) -> Result<Vec<Stage1>> {
        let conn = self.conn();
        let mut statement = conn.prepare("SELECT * FROM memory_stage1 ORDER BY source_updated_at DESC")?;
        let rows = statement.query_map([], stage1_from_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Marks what phase 2 consolidated, and only that.
    pub fn mark_stage1_selected(&self, sources: &[String]) -> Result<()> {
        let conn = self.conn();
        let tx = conn.unchecked_transaction()?;
        tx.execute("UPDATE memory_stage1 SET selected = 0", [])?;
        for source in sources {
            tx.execute("UPDATE memory_stage1 SET selected = 1 WHERE source = ?1", [source])?;
        }
        tx.commit()?;
        Ok(())
    }
}

/// The extractions the TypeScript pipeline made (its `state.sqlite`), read
/// only, so their memory carries over.
pub fn legacy_stage1(path: &Path) -> Vec<Stage1> {
    let Ok(conn) = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) else {
        return Vec::new();
    };
    let Ok(mut statement) = conn.prepare(
        "SELECT session_file, cwd, entry_count, source_updated_at, raw_memory, rollout_summary, rollout_slug, selected_for_phase2
         FROM memory_stage1_outputs",
    ) else {
        return Vec::new();
    };
    statement
        .query_map([], |row| {
            Ok(Stage1 {
                source: row.get(0)?,
                cwd: row.get(1)?,
                entry_count: row.get(2)?,
                source_updated_at: row.get(3)?,
                generated_at: 0,
                raw_memory: row.get(4)?,
                rollout_summary: row.get(5)?,
                slug: row.get(6)?,
                selected: row.get::<_, i64>(7)? != 0,
            })
        })
        .map(|rows| rows.flatten().collect())
        .unwrap_or_default()
}

fn migrate(conn: &Connection) -> Result<()> {
    let version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version > SCHEMA_VERSION {
        bail!("the database was written by a newer Bubble (schema {version})");
    }
    if version < 1 {
        conn.execute_batch(
            "BEGIN;
             CREATE TABLE tasks (
               id TEXT PRIMARY KEY,
               title TEXT NOT NULL,
               project_cwd TEXT,
               lifecycle TEXT NOT NULL,
               waiting_on TEXT,
               origin TEXT NOT NULL,
               created_at INTEGER NOT NULL,
               updated_at INTEGER NOT NULL,
               closed_at INTEGER
             );
             CREATE TABLE runs (
               id TEXT PRIMARY KEY,
               task_id TEXT REFERENCES tasks(id) ON DELETE CASCADE,
               role TEXT NOT NULL,
               model TEXT,
               cwd TEXT,
               status TEXT NOT NULL,
               last_error TEXT,
               created_at INTEGER NOT NULL,
               updated_at INTEGER NOT NULL
             );
             CREATE INDEX runs_task ON runs(task_id, created_at);
             CREATE TABLE messages (
               id TEXT PRIMARY KEY,
               run_id TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
               seq INTEGER NOT NULL,
               role TEXT NOT NULL,
               text TEXT NOT NULL,
               created_at INTEGER NOT NULL,
               UNIQUE (run_id, seq)
             );
             CREATE TABLE task_links (
               id TEXT PRIMARY KEY,
               task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
               kind TEXT NOT NULL,
               repo TEXT NOT NULL,
               number INTEGER NOT NULL,
               snapshot TEXT,
               polled_at INTEGER,
               UNIQUE (kind, repo, number)
             );
             CREATE TABLE events (
               id TEXT PRIMARY KEY,
               source TEXT NOT NULL,
               dedupe_key TEXT NOT NULL UNIQUE,
               kind TEXT NOT NULL,
               repo TEXT,
               number INTEGER,
               actor TEXT,
               payload TEXT NOT NULL,
               task_id TEXT REFERENCES tasks(id) ON DELETE SET NULL,
               disposition TEXT NOT NULL,
               created_at INTEGER NOT NULL
             );
             CREATE TABLE ingest_cursors (
               source TEXT PRIMARY KEY,
               cursor TEXT,
               updated_at INTEGER NOT NULL
             );
             CREATE TABLE settings (
               key TEXT PRIMARY KEY,
               value TEXT NOT NULL
             );
             PRAGMA user_version = 1;
             COMMIT;",
        )?;
    }
    if version < 2 {
        conn.execute_batch(
            "BEGIN;
             ALTER TABLE messages ADD COLUMN content TEXT;
             PRAGMA user_version = 2;
             COMMIT;",
        )?;
    }
    if version < 3 {
        conn.execute_batch(
            "BEGIN;
             CREATE TABLE memory_stage1 (
               source TEXT PRIMARY KEY,
               cwd TEXT NOT NULL,
               entry_count INTEGER NOT NULL,
               source_updated_at TEXT NOT NULL,
               generated_at INTEGER NOT NULL,
               raw_memory TEXT NOT NULL,
               rollout_summary TEXT NOT NULL,
               slug TEXT,
               selected INTEGER NOT NULL DEFAULT 0
             );
             PRAGMA user_version = 3;
             COMMIT;",
        )?;
    }
    Ok(())
}

fn task_from_row(row: &Row<'_>) -> rusqlite::Result<Task> {
    let lifecycle: String = row.get("lifecycle")?;
    let origin: String = row.get("origin")?;
    Ok(Task {
        id: row.get("id")?,
        title: row.get("title")?,
        project_cwd: row.get("project_cwd")?,
        lifecycle: Lifecycle::parse(&lifecycle).map_err(conversion_error)?,
        waiting_on: row.get("waiting_on")?,
        origin: Origin::parse(&origin).map_err(conversion_error)?,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
        closed_at: row.get("closed_at")?,
    })
}

fn run_from_row(row: &Row<'_>) -> rusqlite::Result<Run> {
    let role: String = row.get("role")?;
    let status: String = row.get("status")?;
    Ok(Run {
        id: row.get("id")?,
        task_id: row.get("task_id")?,
        role: RunRole::parse(&role).map_err(conversion_error)?,
        model: row.get("model")?,
        cwd: row.get("cwd")?,
        status: RunStatus::parse(&status).map_err(conversion_error)?,
        last_error: row.get("last_error")?,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
    })
}

fn message_from_row(row: &Row<'_>) -> rusqlite::Result<Message> {
    let role: String = row.get("role")?;
    Ok(Message {
        id: row.get("id")?,
        run_id: row.get("run_id")?,
        seq: row.get("seq")?,
        role: MessageRole::parse(&role).map_err(conversion_error)?,
        text: row.get("text")?,
        content: row.get("content")?,
        created_at: row.get("created_at")?,
    })
}

fn link_from_row(row: &Row<'_>) -> rusqlite::Result<Link> {
    Ok(Link {
        id: row.get("id")?,
        task_id: row.get("task_id")?,
        kind: row.get("kind")?,
        repo: row.get("repo")?,
        number: row.get("number")?,
        snapshot: row.get("snapshot")?,
        polled_at: row.get("polled_at")?,
    })
}

fn event_from_row(row: &Row<'_>) -> rusqlite::Result<Event> {
    Ok(Event {
        id: row.get("id")?,
        source: row.get("source")?,
        dedupe_key: row.get("dedupe_key")?,
        kind: row.get("kind")?,
        repo: row.get("repo")?,
        number: row.get("number")?,
        actor: row.get("actor")?,
        payload: row.get("payload")?,
        task_id: row.get("task_id")?,
        disposition: row.get("disposition")?,
        created_at: row.get("created_at")?,
    })
}

fn conversion_error(error: anyhow::Error) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, error.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(store: &Store, title: &str) -> Task {
        store
            .create_task(NewTask { title, project_cwd: Some("/tmp/bubble"), origin: Origin::Manager })
            .unwrap()
    }

    fn run(store: &Store, task_id: &str, status: RunStatus) -> Run {
        store
            .create_run(NewRun {
                task_id: Some(task_id),
                role: RunRole::Task,
                model: Some("deepseek:deepseek-chat"),
                cwd: Some("/tmp/bubble"),
                status,
            })
            .unwrap()
    }

    #[test]
    fn a_new_task_has_not_started() {
        let store = Store::open_in_memory().unwrap();
        task(&store, "Fix the task list");
        let tasks = store.list_tasks().unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].state, TaskState::NotStarted);
        assert_eq!(tasks[0].latest_run_id, None);
    }

    #[test]
    fn the_latest_run_decides_the_state() {
        let store = Store::open_in_memory().unwrap();
        let t = task(&store, "Fix the task list");
        let first = run(&store, &t.id, RunStatus::Failed);
        let second = run(&store, &t.id, RunStatus::Running);
        let summary = &store.list_tasks().unwrap()[0];
        assert_eq!(summary.state, TaskState::Running);
        assert_eq!(summary.latest_run_id.as_deref(), Some(second.id.as_str()));
        store.set_run_status(&second.id, RunStatus::NeedsInput, None).unwrap();
        assert_eq!(store.list_tasks().unwrap()[0].state, TaskState::NeedsYou);
        assert_eq!(store.runs_for_task(&t.id).unwrap().len(), 2);
        assert_eq!(store.runs_for_task(&t.id).unwrap()[0].id, first.id);
    }

    #[test]
    fn lifecycle_wins_over_runs() {
        let store = Store::open_in_memory().unwrap();
        let t = task(&store, "Ship 0.0.63");
        run(&store, &t.id, RunStatus::Idle);
        assert_eq!(store.list_tasks().unwrap()[0].state, TaskState::Review);
        store.set_task_lifecycle(&t.id, Lifecycle::Waiting, Some("review")).unwrap();
        assert_eq!(store.list_tasks().unwrap()[0].state, TaskState::Waiting);
        store.set_task_lifecycle(&t.id, Lifecycle::Done, None).unwrap();
        let summary = &store.list_tasks().unwrap()[0];
        assert_eq!(summary.state, TaskState::Done);
        assert!(summary.task.closed_at.is_some());
    }

    #[test]
    fn tasks_that_want_a_person_come_first() {
        let store = Store::open_in_memory().unwrap();
        let running = task(&store, "Running");
        let needs = task(&store, "Needs you");
        task(&store, "Not started");
        run(&store, &running.id, RunStatus::Running);
        run(&store, &needs.id, RunStatus::NeedsInput);
        let titles: Vec<_> = store.list_tasks().unwrap().into_iter().map(|s| s.task.title).collect();
        assert_eq!(titles, ["Needs you", "Running", "Not started"]);
    }

    #[test]
    fn messages_keep_their_order_and_can_grow() {
        let store = Store::open_in_memory().unwrap();
        let manager = store.manager_run(None).unwrap();
        assert_eq!(store.manager_run(None).unwrap().id, manager.id);
        store.append_message(&manager.id, MessageRole::User, "hello").unwrap();
        let reply = store.append_message(&manager.id, MessageRole::Assistant, "").unwrap();
        store.set_message_text(&reply.id, "hi there").unwrap();
        let messages = store.messages(&manager.id).unwrap();
        assert_eq!(messages.iter().map(|m| m.seq).collect::<Vec<_>>(), [1, 2]);
        assert_eq!(messages[1].text, "hi there");
    }

    #[test]
    fn interrupted_runs_are_marked_failed() {
        let store = Store::open_in_memory().unwrap();
        let t = task(&store, "Long job");
        let r = run(&store, &t.id, RunStatus::Running);
        assert_eq!(store.recover_interrupted_runs().unwrap(), 1);
        let r = store.get_run(&r.id).unwrap().unwrap();
        assert_eq!(r.status, RunStatus::Failed);
        assert_eq!(store.list_tasks().unwrap()[0].state, TaskState::Failed);
    }

    #[test]
    fn structured_content_is_kept() {
        let store = Store::open_in_memory().unwrap();
        let run = store.manager_run(None).unwrap();
        store
            .append_message_with(&run.id, MessageRole::Tool, "", Some("{\"role\":\"tool\",\"results\":[]}"))
            .unwrap();
        let messages = store.messages(&run.id).unwrap();
        assert_eq!(messages[0].role, MessageRole::Tool);
        assert!(messages[0].content.as_deref().unwrap().contains("results"));
    }

    #[test]
    fn a_version_one_database_gains_the_content_column() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE tasks (id TEXT PRIMARY KEY, title TEXT NOT NULL, project_cwd TEXT, lifecycle TEXT NOT NULL, waiting_on TEXT, origin TEXT NOT NULL, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL, closed_at INTEGER);
             CREATE TABLE runs (id TEXT PRIMARY KEY, task_id TEXT, role TEXT NOT NULL, model TEXT, cwd TEXT, status TEXT NOT NULL, last_error TEXT, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL);
             CREATE TABLE messages (id TEXT PRIMARY KEY, run_id TEXT NOT NULL, seq INTEGER NOT NULL, role TEXT NOT NULL, text TEXT NOT NULL, created_at INTEGER NOT NULL);
             INSERT INTO runs VALUES ('run_1', NULL, 'manager', NULL, NULL, 'idle', NULL, 1, 1);
             INSERT INTO messages VALUES ('msg_1', 'run_1', 1, 'user', 'hello', 1);
             PRAGMA user_version = 1;",
        )
        .unwrap();
        let store = Store::from_connection(conn).unwrap();
        let messages = store.messages("run_1").unwrap();
        assert_eq!(messages[0].text, "hello");
        assert_eq!(messages[0].content, None);
    }

    #[test]
    fn an_issue_links_to_one_task() {
        let store = Store::open_in_memory().unwrap();
        let first = task(&store, "Fix login");
        let second = task(&store, "Other");
        assert_eq!(store.link(&first.id, "github_issue", "acme/demo", 7).unwrap(), first.id);
        assert_eq!(store.link(&second.id, "github_issue", "acme/demo", 7).unwrap(), first.id, "already linked");
        assert_eq!(store.task_for_link("github_issue", "acme/demo", 7).unwrap().as_deref(), Some(first.id.as_str()));
        assert_eq!(store.watched_links().unwrap().len(), 1);
        store.set_task_lifecycle(&first.id, Lifecycle::Done, None).unwrap();
        assert!(store.watched_links().unwrap().is_empty(), "closed tasks are not watched");
    }

    #[test]
    fn events_are_seen_once_and_wait_for_their_task() {
        let store = Store::open_in_memory().unwrap();
        let t = task(&store, "Fix login");
        fn new_event<'a>(key: &'a str, task_id: &'a str) -> NewEvent<'a> {
            NewEvent {
                source: "github",
                dedupe_key: key,
                kind: "comment",
                repo: Some("acme/demo"),
                number: Some(7),
                actor: Some("lin"),
                payload: "{}",
                task_id: Some(task_id),
                disposition: "pending",
            }
        }
        let event = |key: &'static str| new_event(key, &t.id);
        let recorded = store.record_event(event("gh:comment:1")).unwrap().unwrap();
        assert!(store.record_event(event("gh:comment:1")).unwrap().is_none());
        assert_eq!(store.pending_events().unwrap().len(), 1);
        store.set_event_disposition(&recorded.id, "attached", None).unwrap();
        assert!(store.pending_events().unwrap().is_empty());
        assert_eq!(store.events_for_task(&t.id).unwrap()[0].disposition, "attached");
    }

    #[test]
    fn cursors_round_trip() {
        let store = Store::open_in_memory().unwrap();
        assert_eq!(store.cursor("github").unwrap(), None);
        store.set_cursor("github", "2026-11-08T10:00:00Z").unwrap();
        store.set_cursor("github", "2026-11-08T11:00:00Z").unwrap();
        assert_eq!(store.cursor("github").unwrap().as_deref(), Some("2026-11-08T11:00:00Z"));
    }

    #[test]
    fn settings_round_trip() {
        let store = Store::open_in_memory().unwrap();
        assert_eq!(store.setting("max_concurrent_runs").unwrap(), None);
        store.set_setting("max_concurrent_runs", Some("4")).unwrap();
        assert_eq!(store.setting("max_concurrent_runs").unwrap().as_deref(), Some("4"));
        store.set_setting("max_concurrent_runs", None).unwrap();
        assert_eq!(store.setting("max_concurrent_runs").unwrap(), None);
    }

    #[test]
    fn reopening_keeps_the_data() {
        let dir = std::env::temp_dir().join(format!("bubble-store-{}", uuid::Uuid::new_v4().simple()));
        let path = dir.join("bubble.db");
        {
            let store = Store::open(&path).unwrap();
            task(&store, "Persisted");
        }
        let store = Store::open(&path).unwrap();
        assert_eq!(store.list_tasks().unwrap()[0].task.title, "Persisted");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn memory_extractions_upsert_and_track_selection() {
        let store = Store::open_in_memory().unwrap();
        let mut stage1 = Stage1 {
            source: "task:a".into(),
            cwd: "/p".into(),
            entry_count: 4,
            source_updated_at: "2026-11-08T01:00:00.000Z".into(),
            generated_at: 1,
            raw_memory: "raw".into(),
            rollout_summary: "sum".into(),
            slug: None,
            selected: false,
        };
        store.upsert_stage1(&stage1).unwrap();
        stage1.source = "task:b".into();
        stage1.source_updated_at = "2026-11-08T02:00:00.000Z".into();
        store.upsert_stage1(&stage1).unwrap();
        stage1.entry_count = 9;
        store.upsert_stage1(&stage1).unwrap();
        let all = store.all_stage1().unwrap();
        assert_eq!(all.iter().map(|s| s.source.as_str()).collect::<Vec<_>>(), ["task:b", "task:a"]);
        assert_eq!(all[0].entry_count, 9);
        store.mark_stage1_selected(&["task:a".into()]).unwrap();
        assert!(store.stage1("task:a").unwrap().unwrap().selected);
        assert!(!store.stage1("task:b").unwrap().unwrap().selected);
        assert!(legacy_stage1(Path::new("/nonexistent/state.sqlite")).is_empty());
    }
}

#[cfg(test)]
mod discard_tests {
    use super::*;

    #[test]
    fn only_sessions_without_messages_are_discarded() {
        let store = Store::open_in_memory().unwrap();
        let task = |title: &str| store.create_task(NewTask { title, project_cwd: Some("/p"), origin: Origin::Cli }).unwrap();
        let run = |task: &Task| {
            store
                .create_run(NewRun { task_id: Some(&task.id), role: RunRole::Task, model: None, cwd: None, status: RunStatus::Idle })
                .unwrap()
        };
        let empty = task("empty");
        run(&empty);
        let used = task("used");
        let used_run = run(&used);
        store.append_message(&used_run.id, MessageRole::User, "hi").unwrap();
        assert_eq!(store.cli_tasks("/p", 10).unwrap().len(), 1, "only sessions with messages are listed");
        assert!(store.discard_if_empty(&empty.id).unwrap());
        assert!(store.get_task(&empty.id).unwrap().is_none());
        assert!(!store.discard_if_empty(&used.id).unwrap());
        assert_eq!(store.cli_tasks("/p", 10).unwrap().len(), 1);
    }
}
