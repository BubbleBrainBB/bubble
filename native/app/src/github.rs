//! GitHub in the app: a loop that follows the issues and pull requests
//! bots are linked to, the person's notifications, and what is newly
//! opened in the watched repositories; it routes what changed to the bots.
//! Also the GitHub tools a bot may use.
//!
//! Each repository has a level, set in Settings: off (not watched),
//! observe (record only), prepare (agents work; anything published asks
//! first) or act (agents also publish without asking; merging is never an
//! agent's).

use std::path::PathBuf;
use std::time::Duration;

use bubble_core::{Permission, ToolCall, ToolOutput, ToolSpec};
use bubble_github::{Change, Gh, Kind, Notification, Opened, Snapshot, changes, number_in_url};
use bubble_store::{Event, MessageRole, NewEvent};
use nsh_core::prelude::*;
use serde_json::json;
use tokio::sync::oneshot;

use crate::state::{AppState, Conversation};

const SOURCE: &str = "github";
const NOTIFICATIONS_CURSOR: &str = "github-notifications";
/// How often the loop looks at GitHub. Not every look asks for everything:
/// see [`Pace`].
const DEFAULT_INTERVAL: Duration = Duration::from_secs(20);
/// GitHub asks that notifications be read no more often than this.
const NOTIFICATIONS_INTERVAL: Duration = Duration::from_secs(60);

/// How often each thing is asked for, so that looking often stays well
/// inside what GitHub allows in an hour however many repositories are
/// watched and issues followed.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Pace {
    /// The loop's own step.
    step: Duration,
    notifications: Duration,
    /// Set by hand: everything at that step, however much there is.
    fixed: bool,
}

impl Pace {
    fn current() -> Pace {
        match std::env::var("BUBBLE_GH_POLL_MS").ok().and_then(|v| v.parse().ok()).map(Duration::from_millis) {
            // Set by hand (and by tests): everything at that step.
            Some(step) => Pace { step, notifications: step, fixed: true },
            None => Pace { step: DEFAULT_INTERVAL, notifications: NOTIFICATIONS_INTERVAL, fixed: false },
        }
    }

    /// What was newly opened, per repository: every step, slower with
    /// many repositories (one request each; about 1800 an hour at most).
    fn opened(&self, repositories: usize) -> Duration {
        if self.fixed { self.step } else { self.step.max(Duration::from_secs(2) * repositories as u32) }
    }

    /// Each followed issue or pull request: every step, slower with many
    /// of them (one request each; about 900 an hour at most).
    fn link(&self, links: usize) -> Duration {
        if self.fixed { self.step } else { self.step.max(Duration::from_secs(4) * links as u32) }
    }
}

/// Whether something last looked at `last` (milliseconds; never when
/// `None`) is due again at `now`, when it is looked at every `every`. A
/// look a little early counts, so a step that comes a moment short of
/// the time does not put it off a whole step.
fn due(last: Option<i64>, now: i64, every: Duration) -> bool {
    last.is_none_or(|last| now - last >= every.as_millis() as i64 * 9 / 10)
}

/// When the loop last asked for what it does not ask for at every step.
#[derive(Default)]
struct Looked {
    notifications: Option<i64>,
    opened: Option<i64>,
}

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

/// Starts the loop; it runs while the app does.
pub fn start(app: AppState) {
    let Some(gh) = app.gh() else {
        app.github_status.set(Some("GitHub CLI not found. Install gh and run `gh auth login`.".into()));
        return;
    };
    spawn(async move {
        // Who the person is on GitHub: what they open there themselves is
        // told from what others do.
        let mut me = None;
        match background({
            let gh = gh.clone();
            async move { gh.login().await }
        })
        .await
        {
            Ok(Ok(login)) => {
                app.github_status.set(Some(format!("Signed in with GitHub CLI as {login}")));
                me = Some(login);
            }
            Ok(Err(error)) => app.github_status.set(Some(format!("{error:#}"))),
            Err(_) => {}
        }
        let mut looked = Looked::default();
        loop {
            tick(app, &gh, me.as_deref(), &mut looked).await;
            timer(Pace::current().step).await;
        }
    });
}

/// One look at GitHub.
async fn tick(app: AppState, gh: &Gh, me: Option<&str>, looked: &mut Looked) {
    let repos = app.github_repos();
    let watched: Vec<&Repo> = repos.iter().filter(|r| r.level.is_some()).collect();
    if watched.is_empty() {
        return;
    }
    let (pace, now) = (Pace::current(), bubble_store::now_ms());
    if due(looked.notifications, now, pace.notifications) {
        looked.notifications = Some(now);
        discover(app, gh, &watched).await;
    }
    if due(looked.opened, now, pace.opened(watched.len())) {
        looked.opened = Some(now);
        discover_opened(app, gh, &watched, me).await;
    }
    follow_links(app, gh, &watched, pace).await;
    deliver_pending(app);
    crate::triage::maybe_run(app);
    app.refresh_away();
}

fn level_of(repos: &[&Repo], repo: &str) -> Option<Level> {
    repos.iter().find(|r| r.name == repo).and_then(|r| r.level)
}

/// Looks at every linked issue and pull request of an open task.
async fn follow_links(app: AppState, gh: &Gh, repos: &[&Repo], pace: Pace) {
    let links = app.with_store(|s| s.watched_links()).unwrap_or_default();
    let (every, now) = (pace.link(links.len()), bubble_store::now_ms());
    for link in links {
        if !due(link.polled_at, now, every) {
            continue;
        }
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

/// Takes the first look at the issues and pull requests that were just
/// linked to a bot, now rather than at the loop's next turn: what is said
/// there from this moment on is new, and reaches the bot.
pub fn first_look(app: AppState) {
    let Some(gh) = app.gh() else { return };
    spawn(async move {
        let links = app.with_store(|s| s.watched_links()).unwrap_or_default();
        for link in links.into_iter().filter(|link| link.snapshot.is_none()) {
            let Some(kind) = Kind::parse(&link.kind) else { continue };
            let fetched = background({
                let (gh, repo, number) = (gh.clone(), link.repo.clone(), link.number as u64);
                async move { gh.snapshot(kind, &repo, number).await }
            })
            .await;
            let Ok(Ok(now)) = fetched else { continue };
            // The loop may have looked meanwhile; its look stands.
            let still_unseen = app
                .with_store(|s| s.watched_links())
                .unwrap_or_default()
                .iter()
                .any(|other| other.id == link.id && other.snapshot.is_none());
            if let (true, Ok(text)) = (still_unseen, serde_json::to_string(&now)) {
                app.with_store(|s| s.set_link_snapshot(&link.id, &text));
            }
        }
    });
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
    // A merged or closed item ends that piece of work, not the bot.
    let _ = level;
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

/// Where a repository's look at what was newly opened has got to.
fn opened_cursor(repo: &str) -> String {
    format!("github-opened:{repo}")
}

/// Finds the issues and pull requests opened in the watched repositories
/// since the last look. GitHub tells the person nothing of what they open
/// themselves, or of what is opened and neither assigned to them nor
/// mentions them, so the notifications alone miss these.
async fn discover_opened(app: AppState, gh: &Gh, repos: &[&Repo], me: Option<&str>) {
    for repo in repos {
        let Some(level) = repo.level else { continue };
        let key = opened_cursor(&repo.name);
        let since = match app.with_store(|s| s.cursor(&key)).flatten() {
            Some(since) => since,
            // A repository watched from now on: what was open before stays
            // where it was.
            None => {
                let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
                app.with_store(|s| s.set_cursor(&key, &now));
                now
            }
        };
        let fetched = background({
            let gh = gh.clone();
            let (name, since) = (repo.name.clone(), since.clone());
            async move { gh.opened_since(&name, &since).await }
        })
        .await;
        let opened = match fetched {
            Ok(Ok(opened)) => opened,
            Ok(Err(error)) => {
                log::warn!("github: what was opened in {}: {error:#}", repo.name);
                continue;
            }
            Err(_) => continue,
        };
        if let Some(latest) = opened.iter().map(|o| o.created_at.clone()).max() {
            app.with_store(|s| s.set_cursor(&key, &latest));
        }
        for item in &opened {
            handle_opened(app, repo, level, item, me);
        }
    }
}

/// Records something newly opened and says what becomes of it: work a bot
/// already has is left alone; an app's own (a dependency update) and the
/// person's own pull request are only written down; the rest is for the
/// triage to weigh, where the repository lets bots work.
fn handle_opened(app: AppState, repo: &Repo, level: Level, item: &Opened, me: Option<&str>) {
    // A bot that opened it, or was given it, linked it already.
    if app.with_store(|s| s.task_for_link(item.kind.as_str(), &repo.name, item.number as i64)).flatten().is_some() {
        return;
    }
    let own_pr = item.kind == Kind::PullRequest && me == Some(item.author.as_str());
    let disposition = match level {
        Level::Prepare | Level::Act if !item.by_app && !own_pr => "untriaged",
        _ => "noted",
    };
    let what = item.kind.label(item.number);
    let read = match item.kind {
        Kind::Issue => format!("gh issue view {} --repo {} --comments", item.number, repo.name),
        Kind::PullRequest => format!("gh pr view {} --repo {} --comments", item.number, repo.name),
    };
    let brief = format!("@{} opened {what} in {}: \"{}\". Read it with `{read}`.", item.author, repo.name, item.title);
    let payload =
        json!({ "reason": "opened", "title": item.title, "brief": brief, "kind": item.kind.as_str(), "author": item.author }).to_string();
    app.with_store(|s| {
        s.record_event(NewEvent {
            source: SOURCE,
            dedupe_key: &item.dedupe_key(&repo.name),
            kind: "opened",
            repo: Some(&repo.name),
            number: Some(item.number as i64),
            actor: Some(&item.author),
            payload: &payload,
            task_id: None,
            disposition,
        })
    });
}

fn handle_notification(app: AppState, repo: &Repo, level: Level, n: &Notification) {
    let linked = app.with_store(|s| s.task_for_link(n.kind.as_str(), &n.repo, n.number as i64)).flatten();
    // An assignment or a review request is work the person was asked for
    // by name: it goes ahead of what merely happened.
    let starts_work = matches!(n.reason.as_str(), "assign" | "review_requested");
    let weighed = starts_work || matches!(n.reason.as_str(), "mention" | "team_mention" | "author" | "comment" | "state_change");
    // Which bot takes new work is the main bot's to say (the triage): a
    // bot is a role, not the keeper of one repository.
    let disposition = match (&linked, level) {
        (Some(_), _) => "attached",
        (None, Level::Prepare | Level::Act) if weighed => "untriaged",
        (None, Level::Observe) => "noted",
        _ => "ignored",
    };
    let payload = json!({
        "reason": n.reason,
        "title": n.title,
        "brief": brief_for(n),
        "kind": n.kind.as_str(),
        "priority": if starts_work { "first" } else { "normal" },
    })
    .to_string();
    app.with_store(|s| {
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
    let _ = repo;
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
pub(crate) fn deliver_pending(app: AppState) {
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

/// What the person typed to link a task: a GitHub link, `owner/repo#12`,
/// or `#12` / `12` in the task's project's repository. The kind is `None`
/// when only GitHub can tell (an issue or a PR).
pub fn parse_link(input: &str, project_repo: Option<&str>) -> Option<(Option<Kind>, String, u64)> {
    let input = input.trim().trim_end_matches('/');
    if let Some(rest) = input.strip_prefix("https://github.com/").or_else(|| input.strip_prefix("github.com/")) {
        let parts: Vec<&str> = rest.split('/').collect();
        if parts.len() >= 4 {
            let kind = match parts[2] {
                "issues" => Kind::Issue,
                "pull" => Kind::PullRequest,
                _ => return None,
            };
            let number = parts[3].split(['#', '?']).next()?.parse().ok()?;
            return Some((Some(kind), format!("{}/{}", parts[0], parts[1]), number));
        }
        return None;
    }
    let (repo, number) = match input.split_once('#') {
        Some(("", number)) => (project_repo?.to_owned(), number),
        Some((repo, number)) if repo.matches('/').count() == 1 => (repo.to_owned(), number),
        Some(_) => return None,
        None => (project_repo?.to_owned(), input),
    };
    Some((None, repo, number.trim().parse().ok()?))
}

/// Links a task to the issue or PR the person typed, asking GitHub which
/// it is when the text does not say. `done` gets the error, if any.
pub fn link_task(app: AppState, task_id: &str, input: &str, done: impl FnOnce(Option<String>) + 'static) {
    let project = app.with_store(|s| s.get_task(task_id)).flatten().and_then(|t| t.project_cwd);
    let project_repo = project.as_deref().and_then(|p| app.repo_of(p));
    let Some((kind, repo, number)) = parse_link(input, project_repo.as_deref()) else {
        let hint = if project_repo.is_some() { "#12, owner/repo#12 or a GitHub link" } else { "owner/repo#12 or a GitHub link" };
        return done(Some(format!("Write {hint}.")));
    };
    let task_id = task_id.to_owned();
    let gh = app.gh();
    spawn(async move {
        let kind = match (kind, gh) {
            (Some(kind), _) => Ok(kind),
            (None, Some(gh)) => {
                let path = format!("repos/{repo}/issues/{number}");
                match background(async move { gh.run(&["api", &path, "--jq", ".pull_request.url // \"\""], None).await }).await {
                    Ok(Ok(out)) => Ok(if out.trim().is_empty() { Kind::Issue } else { Kind::PullRequest }),
                    Ok(Err(error)) => Err(format!("GitHub could not find {repo}#{number}: {error:#}")),
                    Err(_) => return,
                }
            }
            (None, None) => Err("Install gh, or paste the issue's or PR's link.".to_owned()),
        };
        let kind = match kind {
            Ok(kind) => kind,
            Err(message) => return done(Some(message)),
        };
        if app.with_store(|s| s.link(&task_id, kind.as_str(), &repo, number as i64)).is_none() {
            return done(Some("The link could not be saved.".into()));
        }
        app.task_revision.update(|v| *v += 1);
        done(None);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn looking_often_stays_inside_what_github_allows() {
        let pace = Pace { step: DEFAULT_INTERVAL, notifications: NOTIFICATIONS_INTERVAL, fixed: false };
        let an_hour = |every: Duration| 3600 / every.as_secs();
        // A few repositories and issues: each at every step.
        assert_eq!(pace.opened(1), Duration::from_secs(20));
        assert_eq!(pace.link(3), Duration::from_secs(20));
        // Many: slower each, so the hour's requests stay bounded.
        for count in [1usize, 5, 10, 40, 200] {
            assert!(an_hour(pace.opened(count)) * count as u64 <= 1800, "{count} repositories");
            assert!(an_hour(pace.link(count)) * count as u64 <= 900, "{count} links");
        }
        assert_eq!(an_hour(pace.notifications), 60);
        // A step set by hand is kept for everything.
        let fixed = Pace { step: Duration::from_millis(300), notifications: Duration::from_millis(300), fixed: true };
        assert_eq!(fixed.link(50), Duration::from_millis(300));
        // Never looked at is due; just looked at is not; a moment short of the time is.
        assert!(due(None, 1_000, pace.step));
        assert!(!due(Some(1_000), 5_000, pace.step));
        assert!(due(Some(1_000), 19_500, pace.step));
    }

    #[test]
    fn links_are_read_from_what_the_person_types() {
        assert_eq!(
            parse_link("https://github.com/acme/demo/pull/4/files", None),
            Some((Some(Kind::PullRequest), "acme/demo".into(), 4))
        );
        assert_eq!(parse_link("github.com/acme/demo/issues/7#x", None), Some((Some(Kind::Issue), "acme/demo".into(), 7)));
        assert_eq!(parse_link("acme/demo#12", None), Some((None, "acme/demo".into(), 12)));
        assert_eq!(parse_link("#12", Some("acme/demo")), Some((None, "acme/demo".into(), 12)));
        assert_eq!(parse_link("12", Some("acme/demo")), Some((None, "acme/demo".into(), 12)));
        assert_eq!(parse_link("#12", None), None);
        assert_eq!(parse_link("https://github.com/acme/demo/tree/main", None), None);
    }
}
