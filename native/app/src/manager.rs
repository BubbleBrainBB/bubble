//! The Manager's tools: it sees the tasks and the projects, starts tasks
//! with a brief, follows up on them and closes them. The app answers these
//! calls itself (host tools); they change only Bubble's own state, so they
//! run without asking.

use std::path::Path;

use bubble_core::{ToolCall, ToolOutput, ToolSpec};
use bubble_store::{Lifecycle, MessageRole, NewTask, Origin, TaskState};
use serde_json::{Value, json};

use crate::state::AppState;
use crate::ui::{age, project_name, state_label};

pub const PROMPT: &str = "You are Bubble's Manager, the person's first point of contact in the Bubble desktop app. \
They tell you what needs doing; you organize the work and hand it out. Answer plainly and briefly, in the language they write in.\n\
\n\
You do not do project work yourself. Anything that needs reading or changing files, running commands or more than a short answer \
becomes a task: call create_task with a clear title, the project folder, and a self-contained brief (the goal, where to look, \
what done means). The task's agent sees only your brief, so put everything it needs in it. One request may become several tasks \
when the parts are independent.\n\
\n\
Before creating a task, call list_tasks: when the same work already exists, use continue_task to send it what is new instead. \
Use get_task to see how a task is going before reporting on it. When work is about a GitHub issue or pull request, \
pass its number to create_task (or link_github later) so reviews, comments and checks there reach the task. When the person says a task is finished or unwanted, update_task it. \
Tasks run on their own and may wait for the person's approval of changes and commands; the person answers those on the task. \
After starting or updating tasks, say in a sentence or two what you did.";

pub fn specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "list_tasks".into(),
            description: "List tasks with their id, state, title, project and last change. Done and canceled ones only with include_done.".into(),
            parameters: json!({
                "type": "object",
                "properties": { "include_done": { "type": "boolean" } }
            }),
        },
        ToolSpec {
            name: "get_task".into(),
            description: "One task in detail: its state, project, runs, and the agent's latest reply.".into(),
            parameters: json!({
                "type": "object",
                "properties": { "task_id": { "type": "string" } },
                "required": ["task_id"]
            }),
        },
        ToolSpec {
            name: "list_projects".into(),
            description: "The project folders tasks have used, by name and path.".into(),
            parameters: json!({ "type": "object", "properties": {} }),
        },
        ToolSpec {
            name: "create_task".into(),
            description: "Create a task and start its agent on the brief. project is an absolute folder path or the name of a known project; leave it out for work without files.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "title": { "type": "string", "description": "Short, in the person's language" },
                    "project": { "type": "string" },
                    "brief": { "type": "string", "description": "Everything the task's agent needs: goal, context, what done means" },
                    "issue": { "type": "integer", "description": "A GitHub issue of the project's repository the task is about" },
                    "pull_request": { "type": "integer", "description": "A GitHub pull request of the project's repository the task is about" }
                },
                "required": ["title", "brief"]
            }),
        },
        ToolSpec {
            name: "link_github".into(),
            description: "Link a task to an issue or a pull request of its project's GitHub repository, so reviews, comments and checks there reach the task.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "task_id": { "type": "string" },
                    "issue": { "type": "integer" },
                    "pull_request": { "type": "integer" }
                },
                "required": ["task_id"]
            }),
        },
        ToolSpec {
            name: "continue_task".into(),
            description: "Send a task's agent a follow-up message; it starts a new turn on the task.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "task_id": { "type": "string" },
                    "message": { "type": "string" }
                },
                "required": ["task_id", "message"]
            }),
        },
        ToolSpec {
            name: "update_task".into(),
            description: "Mark a task done, canceled or open again, or rename it.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "task_id": { "type": "string" },
                    "status": { "type": "string", "enum": ["open", "done", "canceled"] },
                    "title": { "type": "string" }
                },
                "required": ["task_id"]
            }),
        },
    ]
}

fn ok(text: impl Into<String>) -> ToolOutput {
    ToolOutput { content: text.into(), is_error: false }
}

fn error(text: impl Into<String>) -> ToolOutput {
    ToolOutput { content: text.into(), is_error: true }
}

fn arg<'a>(args: &'a Value, name: &str) -> &'a str {
    args.get(name).and_then(Value::as_str).map(str::trim).unwrap_or_default()
}

/// Answers one of the Manager's calls.
pub fn execute(app: AppState, call: &ToolCall) -> ToolOutput {
    let args = call.arguments_json();
    match call.name.as_str() {
        "list_tasks" => list_tasks(app, args["include_done"].as_bool().unwrap_or(false)),
        "get_task" => get_task(app, arg(&args, "task_id")),
        "list_projects" => list_projects(app),
        "create_task" => {
            let output = create_task(app, arg(&args, "title"), arg(&args, "project"), arg(&args, "brief"));
            match (output.is_error, task_id_in(&output.content)) {
                (false, Some(id)) => {
                    let linked = link_github(app, &id, &args);
                    if linked.is_error { error(format!("{} {}", output.content, linked.content)) } else { output }
                }
                _ => output,
            }
        }
        "link_github" => link_github(app, arg(&args, "task_id"), &args),
        "continue_task" => continue_task(app, arg(&args, "task_id"), arg(&args, "message")),
        "update_task" => update_task(app, arg(&args, "task_id"), arg(&args, "status"), arg(&args, "title")),
        other => error(format!("There is no tool called {other}.")),
    }
}

fn list_tasks(app: AppState, include_done: bool) -> ToolOutput {
    let tasks = app.tasks.get_untracked();
    let rows: Vec<String> = tasks
        .iter()
        .filter(|t| include_done || !matches!(t.state, TaskState::Done | TaskState::Canceled))
        .map(|t| {
            let project = t.task.project_cwd.as_deref().map(project_name).unwrap_or_else(|| "no project".into());
            let mut row = format!(
                "{} | {} | {} | {} | changed {} ago",
                t.task.id,
                state_label(t.state),
                t.task.title,
                project,
                age(t.task.updated_at)
            );
            if let Some(error) = t.last_error.as_deref().filter(|_| t.state == TaskState::Failed) {
                row.push_str(&format!(" | error: {}", error.lines().next().unwrap_or_default()));
            }
            row
        })
        .collect();
    if rows.is_empty() {
        ok(if include_done { "There are no tasks." } else { "There are no open tasks." })
    } else {
        ok(format!("id | state | title | project | last change\n{}", rows.join("\n")))
    }
}

fn get_task(app: AppState, task_id: &str) -> ToolOutput {
    let Some(summary) = app.tasks.with_untracked(|tasks| tasks.iter().find(|t| t.task.id == task_id).cloned()) else {
        return error(format!("There is no task {task_id}. Call list_tasks for the ids."));
    };
    let detail = app.with_store(|s| {
        let runs = s.runs_for_task(task_id)?;
        let mut latest_reply = None;
        for run in runs.iter().rev() {
            if let Some(message) = s
                .messages(&run.id)?
                .into_iter()
                .rev()
                .find(|m| m.role == MessageRole::Assistant && !m.text.trim().is_empty())
            {
                latest_reply = Some(message.text);
                break;
            }
        }
        Ok((runs.len(), latest_reply))
    });
    let (runs, latest_reply) = detail.unwrap_or((0, None));
    let mut text = format!(
        "Task {}\nTitle: {}\nState: {}\nProject: {}\nRuns: {runs}\nLast change: {} ago",
        summary.task.id,
        summary.task.title,
        state_label(summary.state),
        summary.task.project_cwd.as_deref().unwrap_or("none"),
        age(summary.task.updated_at),
    );
    if let Some(error) = summary.last_error.as_deref().filter(|_| summary.state == TaskState::Failed) {
        text.push_str(&format!("\nError: {error}"));
    }
    match latest_reply {
        Some(reply) => {
            let reply: String = reply.chars().take(2000).collect();
            text.push_str(&format!("\nThe agent's latest reply:\n{reply}"));
        }
        None => text.push_str("\nThe agent has not replied yet."),
    }
    ok(text)
}

fn list_projects(app: AppState) -> ToolOutput {
    let projects = app.projects_untracked();
    if projects.is_empty() {
        return ok("No projects yet. Pass an absolute folder path to create_task.");
    }
    ok(projects.iter().map(|p| format!("{} | {p}", project_name(p))).collect::<Vec<_>>().join("\n"))
}

/// A project given by path or by the name of a known one.
fn resolve_project(app: AppState, project: &str) -> Result<Option<String>, String> {
    if project.is_empty() {
        return Ok(None);
    }
    let path = Path::new(project);
    if path.is_absolute() {
        return if path.is_dir() {
            Ok(Some(project.trim_end_matches('/').to_owned()))
        } else {
            Err(format!("{project} is not a folder."))
        };
    }
    let known = app.projects_untracked();
    known
        .iter()
        .find(|p| project_name(p).eq_ignore_ascii_case(project))
        .cloned()
        .map(Some)
        .ok_or_else(|| {
            let names: Vec<String> = known.iter().map(|p| project_name(p)).collect();
            if names.is_empty() {
                format!("No project is called {project}. Pass its absolute folder path.")
            } else {
                format!("No project is called {project}. Known: {}. Or pass an absolute folder path.", names.join(", "))
            }
        })
}

fn create_task(app: AppState, title: &str, project: &str, brief: &str) -> ToolOutput {
    if title.is_empty() || brief.is_empty() {
        return error("A task needs a title and a brief.");
    }
    let project = match resolve_project(app, project) {
        Ok(project) => project,
        Err(message) => return error(message),
    };
    let Some(task) =
        app.with_store(|s| s.create_task(NewTask { title, project_cwd: project.as_deref(), origin: Origin::Manager }))
    else {
        return error("The task could not be saved.");
    };
    app.refresh_tasks();
    let conversation = app.task_conversation(&task.id);
    match conversation.post(app, brief.to_owned(), MessageRole::System) {
        Ok(started) => {
            let place = project.as_deref().map(|p| format!(" in {}", project_name(p))).unwrap_or_default();
            let how = if started { "Its agent is working on it." } else { "It is queued until another task finishes." };
            ok(format!("Created task {} \"{title}\"{place}. {how}", task.id))
        }
        Err(message) => error(format!("Created task {} but could not start it: {message}", task.id)),
    }
}

fn continue_task(app: AppState, task_id: &str, message: &str) -> ToolOutput {
    if message.is_empty() {
        return error("The message is empty.");
    }
    if app.tasks.with_untracked(|tasks| !tasks.iter().any(|t| t.task.id == task_id)) {
        return error(format!("There is no task {task_id}. Call list_tasks for the ids."));
    }
    let conversation = app.task_conversation(task_id);
    match conversation.post(app, message.to_owned(), MessageRole::System) {
        Ok(true) => ok(format!("Sent to task {task_id}; its agent is working on it.")),
        Ok(false) => ok(format!("Sent to task {task_id}; it is queued until another task finishes.")),
        Err(message) => error(message),
    }
}

fn update_task(app: AppState, task_id: &str, status: &str, title: &str) -> ToolOutput {
    if app.tasks.with_untracked(|tasks| !tasks.iter().any(|t| t.task.id == task_id)) {
        return error(format!("There is no task {task_id}. Call list_tasks for the ids."));
    }
    if !title.is_empty() {
        app.with_store(|s| s.rename_task(task_id, title));
    }
    let lifecycle = match status {
        "" => None,
        "open" => Some(Lifecycle::Open),
        "done" => Some(Lifecycle::Done),
        "canceled" => Some(Lifecycle::Canceled),
        other => return error(format!("Unknown status {other}; use open, done or canceled.")),
    };
    if let Some(lifecycle) = lifecycle {
        app.set_lifecycle(task_id, lifecycle);
    } else {
        app.refresh_tasks();
    }
    ok(format!("Updated task {task_id}."))
}

/// Links a task to the issue or pull request in `args`, if any.
fn link_github(app: AppState, task_id: &str, args: &Value) -> ToolOutput {
    let issue = args["issue"].as_u64();
    let pr = args["pull_request"].as_u64();
    if issue.is_none() && pr.is_none() {
        return ok("Nothing to link.");
    }
    let project = app.with_store(|s| s.get_task(task_id)).flatten().and_then(|t| t.project_cwd);
    let Some(repo) = project.as_deref().and_then(|p| app.repo_of(p)) else {
        return error("The task's project is not a GitHub repository, so nothing was linked.");
    };
    let mut linked = Vec::new();
    for (kind, number) in [(bubble_github::Kind::Issue, issue), (bubble_github::Kind::PullRequest, pr)] {
        if let Some(number) = number {
            let owner = app.with_store(|s| s.link(task_id, kind.as_str(), &repo, number as i64));
            match owner {
                Some(owner) if owner != task_id => {
                    return error(format!("{} in {repo} already belongs to task {owner}.", kind.label(number)));
                }
                Some(_) => linked.push(kind.label(number)),
                None => return error("The link could not be saved."),
            }
        }
    }
    app.refresh_tasks();
    ok(format!("Linked {} in {repo}.", linked.join(" and ")))
}

/// The id of the task a tool's output names (`task_` and 32 hex digits).
pub fn task_id_in(output: &str) -> Option<String> {
    let start = output.find("task_")?;
    let id: String = output[start..]
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    (id.len() > "task_".len()).then_some(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_ids_are_found_in_outputs() {
        assert_eq!(
            task_id_in("Created task task_0123abcd \"Ship\" in demo. Its agent is working on it.").as_deref(),
            Some("task_0123abcd")
        );
        assert_eq!(task_id_in("Updated."), None);
    }
}
