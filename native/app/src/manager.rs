//! The team: the main bot, which coordinates, and the bots, which do the
//! work and can ask each other. These are the tools they reach each other
//! with, and the delivery of what they say.
//!
//! The app answers the calls itself (host tools); they change only Bubble's
//! own state, so they run without asking.

use std::path::Path;

use bubble_core::{ToolCall, ToolOutput, ToolSpec};
use bubble_store::{Lifecycle, MessageRole, NewTask, Origin, TaskState, TaskSummary};
use serde_json::{Value, json};

use crate::state::AppState;
use crate::ui::{age, project_name};

/// The main bot's name, as the person and the bots know it.
pub const MAIN_BOT: &str = "Bubble";

pub const PROMPT: &str = "You are Bubble, the person's main bot in the Bubble desktop app: their first point of contact, and the one who \
coordinates their other bots. Take as little of the person's attention as you can, and give as much value as you can. Answer plainly \
and briefly, in the language they write in.\n\
\n\
You do not do project work yourself. A question you can answer from what you know, you answer. Anything that needs reading or changing \
files, running commands or more than a short answer goes to a bot. Bots are long-lived: each has a name, a purpose, usually a project \
folder, and one continuing conversation, so it knows what it did before.\n\
\n\
Handing work out:\n\
- Call list_bots first. When a bot's purpose and folder fit the work, send it there with message_bot. Most work belongs to a bot that exists.\n\
- Call create_bot only when no bot covers the work and it is the kind that will come again: give the bot a name the person would use, \
its purpose in a sentence or two, the project folder, and its first message.\n\
- A bot sees only your message, not this conversation: put in everything it needs (the goal, where to look, what done means). \
Independent pieces go to different bots in the same turn.\n\
- When the work is about a GitHub issue or pull request, pass its number (to create_bot, or link_github) so reviews, comments and \
checks there reach the bot.\n\
\n\
Afterwards:\n\
- Say once, in a sentence, whom you gave what. Then stop; do not wait or check.\n\
- A bot's answer comes back to you as a message that begins with [Report from …]. Tell the person only what they can use or must \
decide: the result, the question the bot needs answered, or that it could not be done and why. One or two sentences; the full report \
is in the bot's conversation. A report needs no reply to the bot unless there is more work for it.\n\
- Report honestly: a draft is a draft, a failure is a failure.\n\
- Bots may wait for the person's approval of commands; the person answers those in the bot's conversation.\n\
- When the person says a bot is no longer needed, archive it with update_bot.";

/// What a bot is told about working with the others.
pub const BOT_TEAMWORK: &str = "You are one of several bots. list_bots shows the others; message_bot asks one of them for something that \
is theirs to do, and its answer comes back to you as a message that begins with [Report from …], so end your turn when you have nothing \
else to do meanwhile. A message that begins with [From …] is a request from the main bot or another bot: your final reply goes back to \
them, so make it complete and honest: what you did, what is left, and the one question you need answered, if any. Say a draft is a \
draft and a failure is a failure. While you work, stay quiet; ask the person only when you cannot go on without them.";

/// Who a message between bots is from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Sender {
    Main,
    Bot(String),
}

impl Sender {
    pub fn name(&self, app: AppState) -> String {
        match self {
            Sender::Main => MAIN_BOT.to_owned(),
            Sender::Bot(id) => app
                .tasks
                .with_untracked(|tasks| tasks.iter().find(|t| &t.task.id == id).map(|t| t.task.title.clone()))
                .unwrap_or_else(|| "a bot".to_owned()),
        }
    }
}

/// A message on its way from one bot to another.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Incoming {
    pub from: Sender,
    pub text: String,
    /// A request (`None`) is owed an answer; a report is that answer, and
    /// says how the work ended: "finished", "failed", "stopped by the person".
    pub report: Option<&'static str>,
}

/// The tools of the main bot.
pub fn specs() -> Vec<ToolSpec> {
    let mut specs = vec![
        ToolSpec {
            name: "get_bot".into(),
            description: "One bot in detail: its purpose, folder, state, and its latest reply.".into(),
            parameters: json!({
                "type": "object",
                "properties": { "bot": { "type": "string", "description": "Its id or its name" } },
                "required": ["bot"]
            }),
        },
        ToolSpec {
            name: "list_projects".into(),
            description: "The project folders the bots work in, by name and path.".into(),
            parameters: json!({ "type": "object", "properties": {} }),
        },
        ToolSpec {
            name: "create_bot".into(),
            description: "Make a new long-lived bot and give it its first message. project is an absolute folder path or the name of a known project; leave it out for a bot that only talks.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "Short, in the person's language; what they would call it" },
                    "purpose": { "type": "string", "description": "What the bot is for, in a sentence or two; it keeps this as its standing instructions" },
                    "project": { "type": "string" },
                    "message": { "type": "string", "description": "Its first piece of work: goal, context, what done means" },
                    "issue": { "type": "integer", "description": "A GitHub issue of the project's repository the work is about" },
                    "pull_request": { "type": "integer", "description": "A GitHub pull request of the project's repository the work is about" }
                },
                "required": ["name", "message"]
            }),
        },
        ToolSpec {
            name: "link_github".into(),
            description: "Link a bot to an issue or a pull request of its project's GitHub repository, so reviews, comments and checks there reach it.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "bot": { "type": "string", "description": "Its id or its name" },
                    "issue": { "type": "integer" },
                    "pull_request": { "type": "integer" }
                },
                "required": ["bot"]
            }),
        },
        ToolSpec {
            name: "update_bot".into(),
            description: "Rename a bot, change what it is for, or archive it (it stops and leaves the list; restore brings it back).".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "bot": { "type": "string", "description": "Its id or its name" },
                    "name": { "type": "string" },
                    "purpose": { "type": "string" },
                    "status": { "type": "string", "enum": ["archived", "restored"] }
                },
                "required": ["bot"]
            }),
        },
    ];
    specs.extend(bot_specs());
    specs
}

/// The tools every bot has for working with the others.
pub fn bot_specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "list_bots".into(),
            description: "The bots: id, name, state, folder and what each is for. Archived ones only with include_archived.".into(),
            parameters: json!({
                "type": "object",
                "properties": { "include_archived": { "type": "boolean" } }
            }),
        },
        ToolSpec {
            name: "message_bot".into(),
            description: "Send a bot a message; it works on it in its own conversation and its answer comes back to you as a message when it is done.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "bot": { "type": "string", "description": "Its id or its name" },
                    "message": { "type": "string", "description": "Self-contained: the bot sees only this" }
                },
                "required": ["bot", "message"]
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

/// Answers a team call made by `caller`. Only the main bot makes and
/// changes bots.
pub fn execute(app: AppState, caller: &Sender, call: &ToolCall) -> ToolOutput {
    let args = call.arguments_json();
    let main_only = !matches!(call.name.as_str(), "list_bots" | "message_bot");
    if main_only && *caller != Sender::Main {
        return error(format!("Only {MAIN_BOT}, the main bot, can use {}.", call.name));
    }
    match call.name.as_str() {
        "list_bots" => list_bots(app, caller, args["include_archived"].as_bool().unwrap_or(false)),
        "get_bot" => match find(app, arg(&args, "bot")) {
            Ok(bot) => get_bot(app, &bot),
            Err(message) => error(message),
        },
        "list_projects" => list_projects(app),
        "create_bot" => {
            let output = create_bot(app, arg(&args, "name"), arg(&args, "purpose"), arg(&args, "project"), arg(&args, "message"));
            match (output.is_error, task_id_in(&output.content)) {
                (false, Some(id)) => {
                    let linked = link_github(app, &id, &args);
                    if linked.is_error { error(format!("{} {}", output.content, linked.content)) } else { output }
                }
                _ => output,
            }
        }
        "link_github" => match find(app, arg(&args, "bot")) {
            Ok(bot) => link_github(app, &bot.task.id, &args),
            Err(message) => error(message),
        },
        "message_bot" => message_bot(app, caller, arg(&args, "bot"), arg(&args, "message")),
        "update_bot" => match find(app, arg(&args, "bot")) {
            Ok(bot) => update_bot(app, &bot, arg(&args, "name"), args.get("purpose").and_then(Value::as_str), arg(&args, "status")),
            Err(message) => error(message),
        },
        other => error(format!("There is no tool called {other}.")),
    }
}

/// The bot `reference` names: by id, or by its name when one bot has it.
fn find(app: AppState, reference: &str) -> Result<TaskSummary, String> {
    if reference.is_empty() {
        return Err("Say which bot: its id or its name. Call list_bots for them.".into());
    }
    let tasks = app.tasks.get_untracked();
    if let Some(bot) = tasks.iter().find(|t| t.task.id == reference) {
        return Ok(bot.clone());
    }
    let named: Vec<&TaskSummary> = tasks
        .iter()
        .filter(|t| t.task.origin != Origin::Cli && t.task.title.eq_ignore_ascii_case(reference))
        .collect();
    // An archived namesake does not stand in the way of the one at work.
    let live: Vec<&TaskSummary> = named.iter().copied().filter(|t| !archived(t)).collect();
    match (live.as_slice(), named.as_slice()) {
        ([one], _) | ([], [one]) => Ok((*one).clone()),
        ([], []) => Err(format!("There is no bot {reference}. Call list_bots for the ids and names.")),
        _ => Err(format!("More than one bot is called {reference}. Use its id from list_bots.")),
    }
}

fn archived(bot: &TaskSummary) -> bool {
    matches!(bot.task.lifecycle, Lifecycle::Done | Lifecycle::Canceled)
}

/// How a bot stands, in words a bot reads.
fn standing(bot: &TaskSummary) -> &'static str {
    if archived(bot) {
        return "archived";
    }
    match bot.state {
        TaskState::NeedsYou => "waiting for the person",
        TaskState::Running => "working",
        TaskState::Queued => "queued",
        TaskState::Failed => "failed on its last turn",
        _ => "idle",
    }
}

fn list_bots(app: AppState, caller: &Sender, include_archived: bool) -> ToolOutput {
    let tasks = app.tasks.get_untracked();
    let rows: Vec<String> = tasks
        .iter()
        .filter(|t| t.task.origin != Origin::Cli)
        .filter(|t| include_archived || !archived(t))
        .map(|t| {
            let folder = t.task.project_cwd.as_deref().map(project_name).unwrap_or_else(|| "no folder".into());
            let purpose = app.bot_instruction(&t.task.id);
            let purpose = purpose.lines().next().unwrap_or_default();
            let you = if matches!(caller, Sender::Bot(id) if id == &t.task.id) { " (you)" } else { "" };
            format!(
                "{} | {}{you} | {} | {folder} | {}",
                t.task.id,
                t.task.title,
                standing(t),
                if purpose.is_empty() { "no stated purpose" } else { purpose }
            )
        })
        .collect();
    if rows.is_empty() {
        ok(if include_archived { "There are no bots." } else { "There are no bots yet." })
    } else {
        ok(format!("id | name | state | folder | purpose\n{}", rows.join("\n")))
    }
}

fn get_bot(app: AppState, bot: &TaskSummary) -> ToolOutput {
    let id = &bot.task.id;
    let latest_reply = app
        .with_store(|s| {
            for run in s.runs_for_task(id)?.iter().rev() {
                if let Some(message) =
                    s.messages(&run.id)?.into_iter().rev().find(|m| m.role == MessageRole::Assistant && !m.text.trim().is_empty())
                {
                    return Ok(Some(message.text));
                }
            }
            Ok(None)
        })
        .flatten();
    let purpose = app.bot_instruction(id);
    let mut text = format!(
        "Bot {id}\nName: {}\nState: {}\nFolder: {}\nPurpose: {}\nLast change: {} ago",
        bot.task.title,
        standing(bot),
        bot.task.project_cwd.as_deref().unwrap_or("none"),
        if purpose.is_empty() { "none stated" } else { &purpose },
        age(bot.task.updated_at),
    );
    if let Some(error) = bot.last_error.as_deref().filter(|_| bot.state == TaskState::Failed) {
        text.push_str(&format!("\nError: {error}"));
    }
    match latest_reply {
        Some(reply) => {
            let reply: String = reply.chars().take(2000).collect();
            text.push_str(&format!("\nIts latest reply:\n{reply}"));
        }
        None => text.push_str("\nIt has not replied yet."),
    }
    ok(text)
}

fn list_projects(app: AppState) -> ToolOutput {
    let projects = app.projects_untracked();
    if projects.is_empty() {
        return ok("No projects yet. Pass an absolute folder path to create_bot.");
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

fn create_bot(app: AppState, name: &str, purpose: &str, project: &str, message: &str) -> ToolOutput {
    if name.is_empty() || message.is_empty() {
        return error("A bot needs a name and a first message.");
    }
    let taken = app
        .tasks
        .with_untracked(|tasks| tasks.iter().any(|t| t.task.origin != Origin::Cli && !archived(t) && t.task.title.eq_ignore_ascii_case(name)));
    if taken {
        return error(format!("A bot called {name} exists. Send it the work with message_bot, or choose another name."));
    }
    let project = match resolve_project(app, project) {
        Ok(project) => project,
        Err(message) => return error(message),
    };
    let Some(task) =
        app.with_store(|s| s.create_task(NewTask { title: name, project_cwd: project.as_deref(), origin: Origin::Manager }))
    else {
        return error("The bot could not be saved.");
    };
    app.set_bot_instruction(&task.id, purpose);
    app.refresh_tasks();
    let place = project.as_deref().map(|p| format!(" in {}", project_name(p))).unwrap_or_default();
    let started = deliver(app, &Sender::Bot(task.id.clone()), Incoming { from: Sender::Main, text: message.to_owned(), report: None });
    match started {
        Ok(true) => ok(format!("Made bot {} \"{name}\"{place}. It is working on the message; its answer will come to you.", task.id)),
        Ok(false) => ok(format!("Made bot {} \"{name}\"{place}. It starts when another bot finishes; its answer will come to you.", task.id)),
        Err(message) => error(format!("Made bot {} but could not start it: {message}", task.id)),
    }
}

fn message_bot(app: AppState, caller: &Sender, bot: &str, message: &str) -> ToolOutput {
    if message.is_empty() {
        return error("The message is empty.");
    }
    let target = match find(app, bot) {
        Ok(target) => target,
        Err(message) => return error(message),
    };
    if matches!(caller, Sender::Bot(id) if id == &target.task.id) {
        return error("That is you. Do the work yourself, or pick another bot.");
    }
    if archived(&target) {
        return error(format!("{} is archived; only the main bot can restore it.", target.task.title));
    }
    let name = target.task.title.clone();
    let to = Sender::Bot(target.task.id.clone());
    let sent = deliver(app, &to, Incoming { from: caller.clone(), text: message.to_owned(), report: None });
    if let (Ok(_), Sender::Bot(me)) = (&sent, caller) {
        // Its own asker hears from this bot once the answer is in.
        app.task_conversation(me).awaits(&target.task.id);
    }
    match sent {
        Ok(true) => ok(format!("Sent to {name} ({}). It is working on it; its answer will come to you as a message.", target.task.id)),
        Ok(false) => ok(format!("Sent to {name} ({}). It takes it up when it is free; its answer will come to you as a message.", target.task.id)),
        Err(message) => error(message),
    }
}

fn update_bot(app: AppState, bot: &TaskSummary, name: &str, purpose: Option<&str>, status: &str) -> ToolOutput {
    let id = &bot.task.id;
    if !name.is_empty() {
        app.with_store(|s| s.rename_task(id, name));
    }
    if let Some(purpose) = purpose {
        app.set_bot_instruction(id, purpose);
    }
    match status {
        "" => app.refresh_tasks(),
        "archived" => app.cancel_task(id),
        "restored" => app.set_lifecycle(id, Lifecycle::Open),
        other => return error(format!("Unknown status {other}; use archived or restored.")),
    }
    ok(format!("Updated bot {id}."))
}

/// Links a bot to the issue or pull request in `args`, if any.
fn link_github(app: AppState, task_id: &str, args: &Value) -> ToolOutput {
    let issue = args["issue"].as_u64();
    let pr = args["pull_request"].as_u64();
    if issue.is_none() && pr.is_none() {
        return ok("Nothing to link.");
    }
    let project = app.with_store(|s| s.get_task(task_id)).flatten().and_then(|t| t.project_cwd);
    let Some(repo) = project.as_deref().and_then(|p| app.repo_of(p)) else {
        return error("The bot's folder is not a GitHub repository, so nothing was linked.");
    };
    let mut linked = Vec::new();
    for (kind, number) in [(bubble_github::Kind::Issue, issue), (bubble_github::Kind::PullRequest, pr)] {
        if let Some(number) = number {
            let owner = app.with_store(|s| s.link(task_id, kind.as_str(), &repo, number as i64));
            match owner {
                Some(owner) if owner != task_id => {
                    return error(format!("{} in {repo} already belongs to bot {owner}.", kind.label(number)));
                }
                Some(_) => linked.push(kind.label(number)),
                None => return error("The link could not be saved."),
            }
        }
    }
    app.refresh_tasks();
    ok(format!("Linked {} in {repo}.", linked.join(" and ")))
}

/// Hands `incoming` to the conversation of `to`. `Ok(true)`: its turn
/// started; `Ok(false)`: it waits (the bot is busy, or no slot is free).
pub fn deliver(app: AppState, to: &Sender, incoming: Incoming) -> Result<bool, String> {
    let conversation = match to {
        Sender::Main => app.manager_conversation(),
        Sender::Bot(id) => {
            if app.tasks.with_untracked(|tasks| !tasks.iter().any(|t| &t.task.id == id)) {
                return Err(format!("There is no bot {id}."));
            }
            app.task_conversation(id)
        }
    };
    conversation.receive(app, incoming)
}

/// What the model reads for a message between bots: who it is from, and
/// whether it asks or answers.
pub fn framed(app: AppState, incoming: &Incoming) -> String {
    let name = incoming.from.name(app);
    let who = match &incoming.from {
        Sender::Main => format!("the main bot {name}"),
        Sender::Bot(_) => format!("the bot {name}"),
    };
    match incoming.report {
        Some(outcome) => format!("[Report from {who}: {outcome}]\n{}", incoming.text),
        None => format!("[From {who}]\n{}", incoming.text),
    }
}

/// The id of the bot a tool's output names (`task_` and 32 hex digits).
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
            task_id_in("Made bot task_0123abcd \"Docs\" in demo. It is working on the message; its answer will come to you.").as_deref(),
            Some("task_0123abcd")
        );
        assert_eq!(task_id_in("Updated."), None);
    }

    #[test]
    fn every_bot_can_reach_the_others_and_only_the_main_bot_makes_them() {
        let bot: Vec<String> = bot_specs().into_iter().map(|s| s.name).collect();
        assert_eq!(bot, ["list_bots", "message_bot"]);
        let main: Vec<String> = specs().into_iter().map(|s| s.name).collect();
        for name in ["create_bot", "update_bot", "get_bot", "list_bots", "message_bot"] {
            assert!(main.iter().any(|n| n == name), "the main bot has {name}");
        }
    }
}
