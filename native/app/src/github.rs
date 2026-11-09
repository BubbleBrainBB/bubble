//! GitHub in the app: a loop that follows the issues and pull requests
//! tasks are linked to and the person's notifications, routes what changed
//! to the tasks, and the GitHub tools a task's agent may use.
//!
//! Each repository has a level, set in Settings: off (not watched),
//! observe (record only), prepare (agents work; anything published asks
//! first) or act (agents also publish without asking; merging is never an
//! agent's).

use std::path::PathBuf;
use std::time::Duration;

use bubble_core::{Permission, ToolCall, ToolOutput, ToolSpec};
use bubble_github::{Change, Gh, Kind, Notification, Snapshot, changes, number_in_url};
use bubble_store::{Event, Lifecycle, MessageRole, NewEvent, NewTask, Origin};
use nsh_core::prelude::*;
use serde_json::{Value, json};
use tokio::sync::oneshot;

use crate::state::{AppState, Conversation};

const SOURCE: &str = "github";
const NOTIFICATIONS_CURSOR: &str = "github-notifications";
const DEFAULT_INTERVAL: Duration = Duration::from_secs(60);

/// How far Bubble may go on a repository.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Observe,
    Prepare,
    Act,
}

impl Level {
    pub fn as_str(self) -> &'static str {
        match self {
            Level::Observe => "observe",
            Level::Prepare => "prepare",
            Level::Act => "act",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "observe" => Some(Level::Observe),
            "prepare" => Some(Level::Prepare),
            "act" => Some(Level::Act),
            _ => None,
        }
    }

    pub fn label(level: Option<Self>) -> &'static str {
        match level {
            None => "Off",
            Some(Level::Observe) => "Observe",
            Some(Level::Prepare) => "Prepare",
            Some(Level::Act) => "Act",
        }
    }

    pub fn hint(level: Option<Self>) -> &'static str {
        match level {
            None => "Not watched",
            Some(Level::Observe) => "Records what happens; starts nothing",
            Some(Level::Prepare) => "Agents work on new issues and reviews; comments, PRs and pushes wait for you",
            Some(Level::Act) => "Agents also comment, push and open PRs without asking; merging stays yours",
        }
    }
}

/// The settings key of a repository's level.
pub fn level_key(repo: &str) -> String {
    format!("github:{repo}")
}

/// A project folder that is a GitHub repository.
#[derive(Clone, Debug, PartialEq)]
pub struct Repo {
    pub name: String,
    pub project: String,
    pub level: Option<Level>,
}

fn poll_interval() -> Duration {
    std::env::var("BUBBLE_GH_POLL_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_INTERVAL)
}

/// Starts the loop; it runs while the app does.
pub fn start(app: AppState) {
    let Some(gh) = app.gh() else {
        app.github_status.set(Some("GitHub CLI not found. Install gh and run `gh auth login`.".into()));
        return;
    };
    spawn(async move {
        match background({
            let gh = gh.clone();
            async move { gh.login().await }
        })
        .await
        {
            Ok(Ok(login)) => app.github_status.set(Some(format!("Signed in with GitHub CLI as {login}"))),
            Ok(Err(error)) => app.github_status.set(Some(format!("{error:#}"))),
            Err(_) => {}
        }
        loop {
            tick(app, &gh).await;
            timer(poll_interval()).await;
        }
    });
}

/// One look at GitHub.
async fn tick(app: AppState, gh: &Gh) {
    let repos = app.github_repos();
    let watched: Vec<&Repo> = repos.iter().filter(|r| r.level.is_some()).collect();
    if watched.is_empty() {
        return;
    }
    // New tasks first, so their issues get a first look in this same
    // tick: what changes after it reaches the agent, even when the task
    // starts before the next tick.
    discover(app, gh, &watched).await;
    follow_links(app, gh, &watched).await;
    deliver_pending(app);
}

fn level_of(repos: &[&Repo], repo: &str) -> Option<Level> {
    repos.iter().find(|r| r.name == repo).and_then(|r| r.level)
}

/// Looks at every linked issue and pull request of an open task.
async fn follow_links(app: AppState, gh: &Gh, repos: &[&Repo]) {
    let links = app.with_store(|s| s.watched_links()).unwrap_or_default();
    for link in links {
        let Some(level) = level_of(repos, &link.repo) else { continue };
        let Some(kind) = Kind::parse(&link.kind) else { continue };
        let number = link.number as u64;
        let fetched = background({
            let gh = gh.clone();
            let repo = link.repo.clone();
            async move { gh.snapshot(kind, &repo, number).await }
        })
        .await;
        let now = match fetched {
            Ok(Ok(now)) => now,
            Ok(Err(error)) => {
                log::warn!("github: {} {}: {error:#}", link.repo, number);
                continue;
            }
            Err(_) => continue,
        };
        let before: Option<Snapshot> = link.snapshot.as_deref().and_then(|s| serde_json::from_str(s).ok());
        if let Ok(text) = serde_json::to_string(&now) {
            app.with_store(|s| s.set_link_snapshot(&link.id, &text));
        }
        for change in changes(before.as_ref(), &now) {
            route_change(app, &link.task_id, &link.repo, kind, number, &change, level);
        }
    }
}

/// Records a change on a linked item and decides what it means for the task.
fn route_change(app: AppState, task_id: &str, repo: &str, kind: Kind, number: u64, change: &Change, level: Level) {
    let item = kind.label(number);
    let message = match change {
        Change::Comment(c) => Some(format!("@{} commented on {item} in {repo}:\n\n{}", c.author, quote(&c.body))),
        Change::Review(r) => {
            Some(format!("@{} reviewed {item} in {repo} ({}):\n\n{}", r.author, r.state.to_lowercase().replace('_', " "), quote(&r.body)))
        }
        Change::ChecksFailed { sha, names } => Some(format!(
            "Checks failed on {item} in {repo} (commit {}): {}. Look at the failures with `gh pr checks {number} --repo {repo}` and fix them.",
            sha.chars().take(7).collect::<String>(),
            names.join(", ")
        )),
        _ => None,
    };
    let acts = message.is_some() && !change.is_own_or_bot() && level != Level::Observe;
    let disposition = if acts { "pending" } else { "attached" };
    let payload = message.clone().unwrap_or_else(|| change.kind().to_owned());
    let recorded = app.with_store(|s| {
        s.record_event(NewEvent {
            source: SOURCE,
            dedupe_key: &change.dedupe_key(repo, number),
            kind: change.kind(),
            repo: Some(repo),
            number: Some(number as i64),
            actor: change.actor(),
            payload: &payload,
            task_id: Some(task_id),
            disposition,
        })
    });
    if !matches!(recorded, Some(Some(_))) {
        return;
    }
    if matches!(change, Change::Merged | Change::Closed) {
        app.set_lifecycle(task_id, Lifecycle::Done);
    }
}

fn quote(body: &str) -> String {
    let body = body.trim();
    if body.is_empty() {
        return "(no text)".into();
    }
    body.lines().map(|l| format!("> {l}")).collect::<Vec<_>>().join("\n")
}

/// Turns new assignments and review requests into tasks.
async fn discover(app: AppState, gh: &Gh, repos: &[&Repo]) {
    let since = app.with_store(|s| s.cursor(NOTIFICATIONS_CURSOR)).flatten();
    let fetched = background({
        let gh = gh.clone();
        async move { gh.notifications(since.as_deref()).await }
    })
    .await;
    let notifications = match fetched {
        Ok(Ok(n)) => n,
        Ok(Err(error)) => {
            log::warn!("github notifications: {error:#}");
            return;
        }
        Err(_) => return,
    };
    if let Some(latest) = notifications.iter().map(|n| n.updated_at.clone()).max() {
        app.with_store(|s| s.set_cursor(NOTIFICATIONS_CURSOR, &latest));
    }
    for notification in notifications {
        let Some(repo) = repos.iter().find(|r| r.name == notification.repo) else { continue };
        let Some(level) = repo.level else { continue };
        handle_notification(app, repo, level, &notification);
    }
}

fn handle_notification(app: AppState, repo: &Repo, level: Level, n: &Notification) {
    let linked = app.with_store(|s| s.task_for_link(n.kind.as_str(), &n.repo, n.number as i64)).flatten();
    let starts_work = matches!(n.reason.as_str(), "assign" | "review_requested");
    let disposition = match (&linked, starts_work, level) {
        (Some(_), _, _) => "attached",
        (None, true, Level::Prepare | Level::Act) => "created",
        _ => "ignored",
    };
    let brief = brief_for(n);
    let payload = json!({ "reason": n.reason, "title": n.title, "brief": brief }).to_string();
    let recorded = app.with_store(|s| {
        s.record_event(NewEvent {
            source: SOURCE,
            dedupe_key: &n.dedupe_key(),
            kind: &n.reason,
            repo: Some(&n.repo),
            number: Some(n.number as i64),
            actor: None,
            payload: &payload,
            task_id: linked.as_deref(),
            disposition,
        })
    });
    let Some(Some(event)) = recorded else { return };
    if disposition != "created" {
        return;
    }
    let title = format!("#{} {}", n.number, n.title);
    let Some(task) = app.with_store(|s| {
        let task = s.create_task(NewTask { title: &title, project_cwd: Some(&repo.project), origin: Origin::Github })?;
        s.link(&task.id, n.kind.as_str(), &n.repo, n.number as i64)?;
        s.set_event_disposition(&event.id, "created", Some(&task.id))?;
        Ok(task)
    }) else {
        return;
    };
    app.refresh_tasks();
    if level == Level::Act {
        let _ = app.task_conversation(&task.id).post(app, brief, MessageRole::External);
    }
}

fn brief_for(n: &Notification) -> String {
    let item = n.kind.label(n.number);
    let why = match n.reason.as_str() {
        "assign" => "You were assigned",
        "review_requested" => "Your review was requested on",
        _ => "There is news on",
    };
    let read = match n.kind {
        Kind::Issue => format!("gh issue view {} --repo {} --comments", n.number, n.repo),
        Kind::PullRequest => format!("gh pr view {} --repo {} --comments` and `gh pr diff {} --repo {}", n.number, n.repo, n.number, n.repo),
    };
    let goal = match (n.kind, n.reason.as_str()) {
        (Kind::PullRequest, "review_requested") => "Review it: read the change, run what helps, and draft review comments; post nothing without the person's yes.",
        (Kind::Issue, _) => "Work on it in this project: find the cause, make the change, run the tests, and say what you did.",
        _ => "Find out what it needs and do it.",
    };
    format!("{why} {item} in {}: \"{}\". Read it with `{read}`. {goal}", n.repo, n.title)
}

/// Brings waiting events to their tasks, when the task can take them.
fn deliver_pending(app: AppState) {
    let pending = app.with_store(|s| s.pending_events()).unwrap_or_default();
    for event in pending {
        deliver(app, &event);
    }
}

fn deliver(app: AppState, event: &Event) {
    let Some(task_id) = event.task_id.as_deref() else { return };
    let conversation = app.task_conversation(task_id);
    if conversation.busy.get_untracked() {
        return;
    }
    if conversation.post(app, event.payload.clone(), MessageRole::External).is_ok() {
        app.with_store(|s| s.set_event_disposition(&event.id, "attached", None));
    }
}

/// The brief a GitHub task was created with, while it has not started.
pub fn brief_of(app: AppState, task_id: &str) -> Option<String> {
    let events = app.with_store(|s| s.events_for_task(task_id))?;
    events
        .iter()
        .find(|e| e.disposition == "created")
        .and_then(|e| serde_json::from_str::<Value>(&e.payload).ok())
        .and_then(|v| v["brief"].as_str().map(str::to_owned))
}

/// Starts a GitHub task on its brief.
pub fn start_task(app: AppState, task_id: &str) {
    if let Some(brief) = brief_of(app, task_id) {
        let conversation = app.task_conversation(task_id);
        if let Err(message) = conversation.post(app, brief, MessageRole::External) {
            log::warn!("starting {task_id}: {message}");
        }
    }
}

// --- The tools of a task's agent ------------------------------------------------

/// The GitHub tools for a task in `project`, when the folder is a GitHub
/// repository and gh is there.
pub fn task_tools(app: AppState, project: Option<&str>) -> Vec<ToolSpec> {
    let has_repo = project.and_then(|p| app.repo_of(p)).is_some();
    if !has_repo || app.gh().is_none() {
        return Vec::new();
    }
    vec![
        ToolSpec {
            name: "github_comment".into(),
            description: "Comment on an issue or a pull request of this project's GitHub repository, as the person.".into(),
            parameters: json!({
                "type": "object",
                "properties": { "number": { "type": "integer" }, "body": { "type": "string" } },
                "required": ["number", "body"]
            }),
        },
        ToolSpec {
            name: "github_open_pr".into(),
            description: "Open a pull request from the project's current branch (push it first with git push -u origin HEAD). Returns its URL; the task is linked to it.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "title": { "type": "string" },
                    "body": { "type": "string" },
                    "base": { "type": "string", "description": "Target branch; default the repository's" }
                },
                "required": ["title", "body"]
            }),
        },
        ToolSpec {
            name: "github_close_issue".into(),
            description: "Close an issue of this project's repository.".into(),
            parameters: json!({
                "type": "object",
                "properties": { "number": { "type": "integer" } },
                "required": ["number"]
            }),
        },
    ]
}

pub fn is_task_tool(name: &str) -> bool {
    matches!(name, "github_comment" | "github_open_pr" | "github_close_issue")
}

/// Runs a task agent's GitHub call: asks first unless the repository is set
/// to act, then answers on `respond`.
pub fn handle_call(
    app: AppState,
    conversation: Conversation,
    run_id: &str,
    task_id: Option<String>,
    call: ToolCall,
    respond: oneshot::Sender<ToolOutput>,
) {
    let project = task_id.as_deref().and_then(|id| app.with_store(|s| s.get_task(id)).flatten()).and_then(|t| t.project_cwd);
    let Some((project, repo)) = project.and_then(|p| app.repo_of(&p).map(|r| (p, r))) else {
        let _ = respond.send(ToolOutput { content: "This task's project is not a GitHub repository.".into(), is_error: true });
        return;
    };
    let Some(gh) = app.gh() else {
        let _ = respond.send(ToolOutput { content: "GitHub CLI is not installed.".into(), is_error: true });
        return;
    };
    let args = call.arguments_json();
    let number = args["number"].as_u64().unwrap_or(0);
    let (summary, detail) = match call.name.as_str() {
        "github_comment" => (format!("Comment on #{number} in {repo}"), args["body"].as_str().unwrap_or_default().to_owned()),
        "github_open_pr" => (format!("Open a pull request in {repo}"), args["title"].as_str().unwrap_or_default().to_owned()),
        _ => (format!("Close issue #{number} in {repo}"), args["title"].as_str().unwrap_or(&format!("#{number}")).to_owned()),
    };
    let level = app.repo_level(&repo);
    let answer = (level != Some(Level::Act)).then(|| {
        conversation.ask(
            app,
            run_id,
            &call.id,
            Permission { grant: format!("github:{}", call.id), summary, detail, rememberable: false },
        )
    });
    let task_id = task_id.unwrap_or_default();
    spawn(async move {
        if let Some(answer) = answer {
            match answer.await {
                Ok(bubble_core::Decision::Deny) | Err(_) => {
                    let _ = respond.send(ToolOutput {
                        content: "The person declined this. Do not retry it as is; ask them or take another way.".into(),
                        is_error: true,
                    });
                    return;
                }
                Ok(_) => {}
            }
        }
        let name = call.name.clone();
        let result = background(async move {
            match name.as_str() {
                "github_comment" => {
                    let body = args["body"].as_str().unwrap_or_default().to_owned();
                    gh.comment(&repo, number, &body).await.map(|url| (format!("Commented: {url}"), None))
                }
                "github_open_pr" => {
                    let title = args["title"].as_str().unwrap_or_default().to_owned();
                    let body = args["body"].as_str().unwrap_or_default().to_owned();
                    let base = args["base"].as_str().map(str::to_owned);
                    gh.open_pr(&PathBuf::from(&project), &title, &body, base.as_deref())
                        .await
                        .map(|url| (format!("Opened {url}"), number_in_url(&url).map(|n| (repo.clone(), n))))
                }
                _ => gh.close_issue(&repo, number).await.map(|_| (format!("Closed issue #{number}"), None)),
            }
        })
        .await;
        let output = match result {
            Ok(Ok((text, link))) => {
                if let Some((repo, pr)) = link {
                    app.with_store(|s| s.link(&task_id, Kind::PullRequest.as_str(), &repo, pr as i64));
                    app.refresh_tasks();
                }
                ToolOutput { content: text, is_error: false }
            }
            Ok(Err(error)) => ToolOutput { content: format!("{error:#}"), is_error: true },
            Err(error) => ToolOutput { content: format!("{error}"), is_error: true },
        };
        let _ = respond.send(output);
    });
}

/// "Issue #7", "PR #12", and the page each opens.
pub fn link_chips(app: AppState, task_id: &str) -> Vec<(String, String)> {
    app.with_store(|s| s.links_for_task(task_id))
        .unwrap_or_default()
        .into_iter()
        .filter_map(|link| {
            let kind = Kind::parse(&link.kind)?;
            let (label, path) = match kind {
                Kind::Issue => (format!("Issue #{}", link.number), "issues"),
                Kind::PullRequest => (format!("PR #{}", link.number), "pull"),
            };
            Some((label, format!("https://github.com/{}/{path}/{}", link.repo, link.number)))
        })
        .collect()
}
