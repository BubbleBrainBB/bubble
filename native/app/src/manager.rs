//! The team: the main bot, which coordinates, and the bots, which do the
//! work and can ask each other. These are the tools they reach each other
//! with, and the delivery of what they say.
//!
//! The app answers the calls itself (host tools); they change only Bubble's
//! own state, so they run without asking.

use std::path::Path;

use bubble_core::{ToolCall, ToolOutput, ToolSpec};
use bubble_store::{Lifecycle, MessageRole, NewTicket, Origin, TaskState, TaskSummary, Ticket, TicketOrigin, TicketStatus};
use serde_json::{Value, json};

use crate::state::AppState;
use crate::ui::{age, project_name};

/// The main bot's name, as the person and the bots know it.
pub const MAIN_BOT: &str = "Bubble";

pub const PROMPT: &str = "You are Bubble, the person's main bot in the Bubble desktop app: their first point of contact, and the one who \
runs their team of bots. Take as little of the person's attention as you can, and give as much value as you can. Answer plainly \
and briefly, in the language they write in.\n\
\n\
You do not do project work yourself. A question you can answer from what you know, you answer. Anything that needs reading or changing \
files, running commands or more than a short answer goes to a bot. A bot is a long-lived role on the team (a developer, a researcher, \
a writer): it has a name, a role, standing instructions and one continuing conversation, so it knows what it did before. A bot is not \
tied to a folder: the same developer works in whichever project the work is in, and the folder comes with the work.\n\
\n\
Handing work out:\n\
- Call list_bots first. Send the work to the bot whose role fits, with message_bot; set project to the folder the work is in \
(list_projects shows the known ones; an absolute path names a new one) and leave it out for work that needs no files.\n\
- A bot does one thing at a time. When the bot that fits is working and this is new, unrelated work, do not make the person wait: \
make a copy of it with create_bot (copy_of) and give the copy the work. Write to a working bot only about what it is doing.\n\
- Call create_bot when no bot's role covers the work and it is the kind that will come again: a name the person would use, a role \
of a word or two, its instructions, and brief (why the person wants it). Give its first piece of work as message when there is one; \
without one it greets the person itself and asks where to start.\n\
- A bot sees only your message, not this conversation: put in everything it needs (the goal, where to look, what done means). \
Independent pieces go to different bots in the same turn.\n\
- When the work is about a GitHub issue or pull request, pass its number with the message (issue or pull_request, with project) so \
reviews, comments and checks there reach the bot.\n\
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
draft and a failure is a failure. A request may say which folder its work is in (Work in: …); your tools start there for that work. \
While you work, stay quiet; ask the person only when you cannot go on without them.";

/// Who a message to a bot is from, when it is not the person.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Sender {
    Main,
    Bot(String),
    /// Something that happened on GitHub.
    Github,
    /// One of the bot's own routines, by name, on its schedule.
    Routine(String),
    /// The app, when a bot is made: what it is told to begin with.
    Setup,
}

impl Sender {
    pub fn name(&self, app: AppState) -> String {
        match self {
            Sender::Main => MAIN_BOT.to_owned(),
            Sender::Bot(id) => app
                .tasks
                .with_untracked(|tasks| tasks.iter().find(|t| &t.task.id == id).map(|t| t.task.title.clone()))
                .unwrap_or_else(|| "a bot".to_owned()),
            Sender::Github => "GitHub".to_owned(),
            Sender::Routine(name) => format!("your routine {name}"),
            Sender::Setup => "Setup".to_owned(),
        }
    }

    /// Whether the sender reads an answer: a bot does, an event does not.
    pub fn hears_back(&self) -> bool {
        matches!(self, Sender::Main | Sender::Bot(_))
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
    /// The ticket this request is the work of.
    pub ticket: Option<String>,
    /// The folder the work is in: the bot works there for it.
    pub project: Option<String>,
    /// How soon it wants doing among what waits: 0 first, 1 as it comes,
    /// 2 when there is time.
    pub priority: i64,
    /// What the person sees of it in the conversation, when that is not
    /// what the bot reads.
    pub shown: Option<String>,
}

/// As it comes: neither ahead of what waits nor behind it.
pub const NORMAL: i64 = 1;
/// Ahead of what waits.
pub const FIRST: i64 = 0;
/// When there is time.
pub const LATER: i64 = 2;

impl Incoming {
    pub fn request(from: Sender, text: impl Into<String>) -> Incoming {
        Incoming { from, text: text.into(), report: None, ticket: None, project: None, priority: NORMAL, shown: None }
    }

    /// The answer to a request: it is read before new work is begun.
    pub fn report(from: Sender, text: impl Into<String>, outcome: &'static str) -> Incoming {
        Incoming { report: Some(outcome), priority: FIRST, ..Incoming::request(from, text) }
    }

    pub fn in_project(mut self, project: Option<String>) -> Incoming {
        self.project = project;
        self
    }
}

/// The tools of the main bot.
pub fn specs() -> Vec<ToolSpec> {
    let mut specs = vec![
        ToolSpec {
            name: "get_bot".into(),
            description: "One bot in detail: its role, instructions, state, where it works now, and its latest reply.".into(),
            parameters: json!({
                "type": "object",
                "properties": { "bot": { "type": "string", "description": "Its id or its name" } },
                "required": ["bot"]
            }),
        },
        ToolSpec {
            name: "list_projects".into(),
            description: "The project folders the bots have worked in, by name and path.".into(),
            parameters: json!({ "type": "object", "properties": {} }),
        },
        ToolSpec {
            name: "create_bot".into(),
            description: "Make a new long-lived bot: a role on the team, not tied to a folder. With message it starts on that work; without, it greets the person and asks where to start. With copy_of it is a copy of that bot (same role and instructions, a conversation of its own), for work its original is too busy to take.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "Short, in the person's language; what they would call it. No other bot has it. Left out for a copy, which is numbered" },
                    "role": { "type": "string", "description": "Its role in a word or two, shown by its name: Developer, Researcher, Writer" },
                    "instructions": { "type": "string", "description": "What the bot is responsible for and how it should work; it keeps this as its standing instructions" },
                    "brief": { "type": "string", "description": "Why the person wants it, in a sentence; it reads this first" },
                    "copy_of": { "type": "string", "description": "The id or name of the bot to copy" },
                    "message": { "type": "string", "description": "Its first piece of work: goal, context, what done means" },
                    "project": { "type": "string", "description": "The folder the first work is in: an absolute path or the name of a known project" },
                    "issue": { "type": "integer", "description": "A GitHub issue of the project's repository the work is about" },
                    "pull_request": { "type": "integer", "description": "A GitHub pull request of the project's repository the work is about" }
                }
            }),
        },
        ToolSpec {
            name: "link_github".into(),
            description: "Link a bot to an issue or a pull request of a project's GitHub repository, so reviews, comments and checks there reach it. project is the folder the repository is in; the one the bot works in now when left out.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "bot": { "type": "string", "description": "Its id or its name" },
                    "project": { "type": "string" },
                    "issue": { "type": "integer" },
                    "pull_request": { "type": "integer" }
                },
                "required": ["bot"]
            }),
        },
        ToolSpec {
            name: "update_bot".into(),
            description: "Rename a bot, change its role or its instructions, or archive it (it stops and leaves the list; restore brings it back).".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "bot": { "type": "string", "description": "Its id or its name" },
                    "name": { "type": "string" },
                    "role": { "type": "string" },
                    "instructions": { "type": "string" },
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
            description: "The bots: id, name, role, state, where each works now and what it is for. Archived ones only with include_archived.".into(),
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
                    "message": { "type": "string", "description": "Self-contained: the bot sees only this" },
                    "project": { "type": "string", "description": "The folder the work is in: an absolute path or the name of a known project. Left out, the bot stays where it works now" },
                    "issue": { "type": "integer", "description": "A GitHub issue of the project's repository the work is about" },
                    "pull_request": { "type": "integer", "description": "A GitHub pull request of the project's repository the work is about" }
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
        "create_bot" => make_bot(app, &args, true),
        "link_github" => match find(app, arg(&args, "bot")) {
            Ok(bot) => {
                let project = match resolve_project(app, arg(&args, "project")) {
                    Ok(project) => project.or_else(|| bot.task.project_cwd.clone()),
                    Err(message) => return error(message),
                };
                link_github(app, &bot.task.id, &args, project.as_deref())
            }
            Err(message) => error(message),
        },
        "message_bot" => message_bot(app, caller, &args),
        "update_bot" => match find(app, arg(&args, "bot")) {
            Ok(bot) => update_bot(app, &bot, &args),
            Err(message) => error(message),
        },
        other => error(format!("There is no tool called {other}.")),
    }
}

/// The bot `reference` names: by id, or by its name when one bot has it.
pub fn find(app: AppState, reference: &str) -> Result<TaskSummary, String> {
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
            let role = app.bot_role(&t.task.id);
            let purpose = app.bot_instruction(&t.task.id);
            let purpose = purpose.lines().next().unwrap_or_default();
            let you = if matches!(caller, Sender::Bot(id) if id == &t.task.id) { " (you)" } else { "" };
            let waiting = if archived(t) { 0 } else { app.task_conversation(&t.task.id).waiting() };
            let state = match waiting {
                0 => standing(t).to_owned(),
                n => format!("{}, {n} waiting", standing(t)),
            };
            format!(
                "{} | {}{you} | {} | {state} | {folder} | {}",
                t.task.id,
                t.task.title,
                if role.is_empty() { "no role" } else { &role },
                if purpose.is_empty() { "no stated purpose" } else { purpose }
            )
        })
        .collect();
    if rows.is_empty() {
        ok(if include_archived { "There are no bots." } else { "There are no bots yet." })
    } else {
        ok(format!("id | name | role | state | works now in | what it is for\n{}", rows.join("\n")))
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
    let role = app.bot_role(id);
    let mut text = format!(
        "Bot {id}\nName: {}\nRole: {}\nState: {}\nWorks now in: {}\nInstructions: {}\nLast change: {} ago",
        bot.task.title,
        if role.is_empty() { "none stated" } else { &role },
        standing(bot),
        bot.task.project_cwd.as_deref().unwrap_or("no folder"),
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
        return ok("No projects yet. Pass an absolute folder path as project.");
    }
    ok(projects.iter().map(|p| format!("{} | {p}", project_name(p))).collect::<Vec<_>>().join("\n"))
}

/// A project given by path or by the name of a known one.
pub fn resolve_project(app: AppState, project: &str) -> Result<Option<String>, String> {
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

/// Makes the bot `args` describe (create_bot's). Without a first message it
/// greets the person, unless `greeting` is off: a bot made to take work
/// that is on its way has no need to ask where to start.
pub fn make_bot(app: AppState, args: &Value, greeting: bool) -> ToolOutput {
    let (name, message, copy_of) = (arg(args, "name"), arg(args, "message"), arg(args, "copy_of"));
    // Older callers said "purpose" for the instructions.
    let instructions = match arg(args, "instructions") {
        "" => arg(args, "purpose"),
        instructions => instructions,
    };
    let project = match resolve_project(app, arg(args, "project")) {
        Ok(project) => project,
        Err(message) => return error(message),
    };
    let (id, copied) = if copy_of.is_empty() {
        match app.create_bot(name, arg(args, "role"), instructions, Origin::Manager) {
            Ok(id) => (id, None),
            Err(message) => return error(format!("{message} When a bot of that name covers the work, send it with message_bot.")),
        }
    } else {
        let original = match find(app, copy_of) {
            Ok(original) => original,
            Err(message) => return error(message),
        };
        match app.duplicate_bot(&original.task.id) {
            Some(id) => (id, Some(original.task.title)),
            None => return error("The copy could not be made."),
        }
    };
    let made = app.tasks.with_untracked(|tasks| tasks.iter().find(|t| t.task.id == id).map(|t| t.task.title.clone())).unwrap_or_default();
    let what = match &copied {
        Some(original) => format!("Made bot {id} \"{made}\", a copy of {original}."),
        None => format!("Made bot {id} \"{made}\"."),
    };
    if message.is_empty() && !greeting {
        return ok(what);
    }
    if message.is_empty() {
        greet(app, &id, Some(&Sender::Main), arg(args, "brief"), copied.as_deref());
        return ok(format!("{what} It is greeting the person in its own conversation and asks them where to start."));
    }
    let linked = link_github(app, &id, args, project.as_deref());
    let place = project.as_deref().map(|p| format!(", in {}", project_name(p))).unwrap_or_default();
    let started = deliver(app, &Sender::Bot(id.clone()), Incoming::request(Sender::Main, message).in_project(project));
    let note = if linked.is_error { format!(" {}", linked.content) } else { String::new() };
    match started {
        Ok(true) => ok(format!("{what} It is working on the message{place}; its answer will come to you.{note}")),
        Ok(false) => ok(format!("{what} It starts when a slot is free{place}; its answer will come to you.{note}")),
        Err(message) => error(format!("{what} But it could not be started: {message}")),
    }
}

/// Starts a new bot off: it says hello to the person in its own
/// conversation and asks where to begin. `by`: who made it (the person
/// when `None`); `brief`: why; `copy_of`: the bot it is a copy of.
pub fn greet(app: AppState, bot_id: &str, by: Option<&Sender>, brief: &str, copy_of: Option<&str>) {
    if std::env::var("BUBBLE_NO_GREETING").is_ok_and(|v| v == "1") {
        return;
    }
    let brief = brief.trim();
    let (told, shown) = match (copy_of, by) {
        (Some(original), _) => (
            format!(
                "You were just made as a copy of {original}: the same role and instructions, a conversation and notes of your own. \
                 Greet the person in one short message, then ask in one short question what this copy should take on."
            ),
            format!("A copy of {original}."),
        ),
        (None, Some(maker)) => {
            let maker = maker.name(app);
            let why = if brief.is_empty() { String::new() } else { format!(" Why you exist: {brief}") };
            (
                format!(
                    "{maker}, the main bot, just made you for the person.{why}\nGreet the person in one short message, then ask in \
                     one short question where to start. Keep it brief."
                ),
                if brief.is_empty() { format!("Made by {maker}.") } else { format!("Made by {maker}: {brief}") },
            )
        }
        (None, None) => (
            "The person just made you. Greet them in one short message, then ask in one short question what you should start with."
                .to_owned(),
            "Made by you.".to_owned(),
        ),
    };
    let incoming = Incoming { shown: Some(shown), ..Incoming::request(Sender::Setup, format!("{told}\nWhere your instructions say otherwise, follow them.")) };
    if let Err(error) = deliver(app, &Sender::Bot(bot_id.to_owned()), incoming) {
        log::warn!("a new bot could not be greeted: {error}");
    }
}

fn message_bot(app: AppState, caller: &Sender, args: &Value) -> ToolOutput {
    let (bot, message) = (arg(args, "bot"), arg(args, "message"));
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
    let project = match resolve_project(app, arg(args, "project")) {
        Ok(project) => project,
        Err(message) => return error(message),
    };
    let name = target.task.title.clone();
    let to = Sender::Bot(target.task.id.clone());
    // What was already waiting for it, before this.
    let ahead = app.task_conversation(&target.task.id).waiting();
    let mut note = String::new();
    if *caller == Sender::Main {
        let linked = link_github(app, &target.task.id, args, project.as_deref().or(target.task.project_cwd.as_deref()));
        if linked.is_error {
            note = format!(" {}", linked.content);
        }
    }
    let sent = deliver(app, &to, Incoming::request(caller.clone(), message).in_project(project));
    if let (Ok(_), Sender::Bot(me)) = (&sent, caller) {
        // Its own asker hears from this bot once the answer is in.
        app.task_conversation(me).awaits(&target.task.id);
    }
    match sent {
        Ok(true) => ok(format!("Sent to {name} ({}). It is working on it; its answer will come to you as a message.{note}", target.task.id)),
        // The moment to say it: the work waits behind other work, and a
        // copy of the bot would take it now.
        Ok(false) if ahead > 0 && *caller == Sender::Main => ok(format!(
            "Sent to {name} ({}), which is busy: this waits behind {ahead} other piece{} of work. Its answer will come to you as a \
             message. If this is new work that should not wait, a copy of {name} can take such work now (create_bot with copy_of).{note}",
            target.task.id,
            if ahead == 1 { "" } else { "s" }
        )),
        Ok(false) => ok(format!("Sent to {name} ({}). It takes it up when it is free; its answer will come to you as a message.{note}", target.task.id)),
        Err(message) => error(message),
    }
}

fn update_bot(app: AppState, bot: &TaskSummary, args: &Value) -> ToolOutput {
    let id = &bot.task.id;
    let name = arg(args, "name");
    if !name.is_empty() {
        if let Err(message) = app.rename_bot(id, name) {
            return error(message);
        }
    }
    if let Some(role) = args.get("role").and_then(Value::as_str) {
        app.set_bot_role(id, role);
    }
    if let Some(instructions) = args.get("instructions").or_else(|| args.get("purpose")).and_then(Value::as_str) {
        app.set_bot_instruction(id, instructions);
    }
    match arg(args, "status") {
        "" => app.refresh_tasks(),
        "archived" => app.cancel_task(id),
        "restored" => {
            // A bot that took its name meanwhile keeps it.
            if app.name_taken(&bot.task.title, Some(id)) {
                return error(format!("Another bot is called {} now; rename one of them first.", bot.task.title));
            }
            app.set_lifecycle(id, Lifecycle::Open)
        }
        other => return error(format!("Unknown status {other}; use archived or restored.")),
    }
    ok(format!("Updated bot {id}."))
}

/// Links a bot to the issue or pull request in `args`, if any, of the
/// GitHub repository `project` is a checkout of.
fn link_github(app: AppState, task_id: &str, args: &Value, project: Option<&str>) -> ToolOutput {
    let issue = args["issue"].as_u64();
    let pr = args["pull_request"].as_u64();
    if issue.is_none() && pr.is_none() {
        return ok("Nothing to link.");
    }
    let Some(repo) = project.and_then(|p| app.repo_of(p)) else {
        return error("No project was named, or its folder is not a GitHub repository, so nothing was linked.");
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
    crate::github::first_look(app);
    ok(format!("Linked {} in {repo}.", linked.join(" and ")))
}

/// Hands `incoming` to the conversation of `to`. `Ok(true)`: its turn
/// started; `Ok(false)`: it waits (the bot is busy, or no slot is free).
///
/// A request from the main bot or another bot becomes a ticket of the bot
/// it goes to: the record of who asked for what, and how it went.
pub fn deliver(app: AppState, to: &Sender, mut incoming: Incoming) -> Result<bool, String> {
    let conversation = match to {
        Sender::Main => app.manager_conversation(),
        Sender::Bot(id) => {
            if app.tasks.with_untracked(|tasks| !tasks.iter().any(|t| &t.task.id == id)) {
                return Err(format!("There is no bot {id}."));
            }
            if incoming.report.is_none() && incoming.ticket.is_none() && incoming.from.hears_back() {
                let (origin, asker) = match &incoming.from {
                    Sender::Bot(sender) => (TicketOrigin::Bot, Some(sender.as_str())),
                    _ => (TicketOrigin::Main, None),
                };
                incoming.ticket = app
                    .with_store(|s| {
                        s.create_ticket(NewTicket {
                            task_id: id,
                            title: &title_of(&incoming.text),
                            text: &incoming.text,
                            origin,
                            asker,
                            explicit: true,
                            status: TicketStatus::Accepted,
                            project: incoming.project.as_deref(),
                            priority: incoming.priority,
                        })
                    })
                    .map(|ticket| ticket.id);
            }
            app.task_conversation(id)
        }
        Sender::Github | Sender::Routine(_) | Sender::Setup => return Err("Only a bot can be written to.".into()),
    };
    conversation.receive(app, incoming)
}

/// A few words naming a piece of work: the start of its first line.
pub fn title_of(text: &str) -> String {
    let line = text.lines().find(|l| !l.trim().is_empty()).unwrap_or_default().trim();
    if line.chars().count() > 80 { format!("{}…", line.chars().take(79).collect::<String>()) } else { line.to_owned() }
}

/// How far work nobody asked the bot for outright may go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Standing {
    /// Waits for the person's yes.
    Propose,
    /// Goes to the bot now.
    Accept,
}

/// A piece of work an event or a routine brings.
pub struct Work<'a> {
    pub from: Sender,
    pub title: &'a str,
    pub text: &'a str,
    /// Who or what asked: `owner/repo#12`, a routine's id.
    pub asker: &'a str,
    pub explicit: bool,
    /// The folder it is done in.
    pub project: Option<&'a str>,
    pub priority: i64,
}

/// Work for a bot that an event or a routine brings: a ticket, given to
/// the bot at once, held for the person's yes, or only recorded. With
/// `pooled`, work given at once goes to whichever bot of that role is free
/// (see [`worker_for`]); a routine's work stays with its own bot.
pub fn bring(app: AppState, bot_id: &str, work: Work<'_>, standing: Standing, pooled: bool) -> Option<Ticket> {
    let origin = match work.from {
        Sender::Routine(_) => TicketOrigin::Routine,
        Sender::Github => TicketOrigin::Github,
        Sender::Bot(_) => TicketOrigin::Bot,
        Sender::Main | Sender::Setup => TicketOrigin::Main,
    };
    let status = match standing {
        Standing::Propose => TicketStatus::Proposed,
        Standing::Accept => TicketStatus::Accepted,
    };
    let bot_id = if pooled && standing == Standing::Accept { worker_for(app, bot_id) } else { bot_id.to_owned() };
    let ticket = app.with_store(|s| {
        s.create_ticket(NewTicket {
            task_id: &bot_id,
            title: work.title,
            text: work.text,
            origin,
            asker: Some(work.asker),
            explicit: work.explicit,
            status,
            project: work.project,
            priority: work.priority,
        })
    })?;
    if standing == Standing::Accept {
        hand_over(app, &ticket, work.from);
    }
    app.refresh_tickets();
    if standing == Standing::Propose {
        // It waits for the person's yes: the main bot asks, where they talk.
        let bot = Sender::Bot(bot_id.clone()).name(app);
        let shown = match origin {
            TicketOrigin::Github => format!("{} came in: {}. Give it to {bot}?", work.asker, work.title),
            _ => format!("{} is ready for {bot}. Give it?", work.title),
        };
        app.tell(&shown, &format!("{shown} It waits for the person's yes."), None, Some(&ticket.id));
    }
    Some(ticket)
}

fn hand_over(app: AppState, ticket: &Ticket, from: Sender) {
    let incoming = Incoming {
        ticket: Some(ticket.id.clone()),
        project: ticket.project.clone(),
        priority: ticket.priority,
        ..Incoming::request(from, ticket.text.clone())
    };
    if let Err(error) = deliver(app, &Sender::Bot(ticket.task_id.clone()), incoming) {
        app.with_store(|s| s.set_ticket_status(&ticket.id, TicketStatus::Failed, Some(&error)));
    }
}

/// The person says yes to a proposed ticket: a bot gets the work, the one
/// it was meant for or another of its role that is free now.
pub fn accept_ticket(app: AppState, ticket_id: &str) {
    let Some(mut ticket) = app.with_store(|s| s.get_ticket(ticket_id)).flatten() else { return };
    if ticket.status != TicketStatus::Proposed {
        return;
    }
    let worker = match ticket.origin {
        // A routine's work is its own bot's.
        TicketOrigin::Routine => ticket.task_id.clone(),
        _ => worker_for(app, &ticket.task_id),
    };
    if worker != ticket.task_id {
        app.with_store(|s| s.set_ticket_task(ticket_id, &worker));
        // What happens on the issue reaches the bot that works on it.
        if let Some((repo, number)) = ticket.asker.as_deref().and_then(|asker| asker.rsplit_once('#')) {
            if let Ok(number) = number.parse::<i64>() {
                app.with_store(|s| s.move_links(&ticket.task_id, &worker, repo, number));
            }
        }
        ticket.task_id = worker;
        app.refresh_tasks();
    }
    app.with_store(|s| s.set_ticket_status(ticket_id, TicketStatus::Accepted, None));
    let from = match ticket.origin {
        TicketOrigin::Routine => Sender::Routine(ticket.title.clone()),
        _ => Sender::Github,
    };
    hand_over(app, &ticket, from);
    app.refresh_tickets();
}

/// The person turns a proposed ticket down.
pub fn dismiss_ticket(app: AppState, ticket_id: &str) {
    app.with_store(|s| s.set_ticket_status(ticket_id, TicketStatus::Dismissed, None));
    app.refresh_tickets();
}

/// The bot to give new work meant for `bot_id`, so that work does not wait
/// where it need not: the bot itself when it is free; else a free bot of
/// the same role; else a new copy of it, while the role has room for one;
/// else whichever of them has least waiting.
pub fn worker_for(app: AppState, bot_id: &str) -> String {
    let waiting = |id: &str| app.task_conversation(id).waiting();
    if waiting(bot_id) == 0 {
        return bot_id.to_owned();
    }
    let role = app.bot_role(bot_id).trim().to_lowercase();
    // A bot without a role has no others like it.
    let peers: Vec<String> = app
        .bots_untracked()
        .into_iter()
        .map(|bot| bot.task.id)
        .filter(|id| id == bot_id || (!role.is_empty() && app.bot_role(id).trim().to_lowercase() == role))
        .collect();
    if let Some(free) = peers.iter().find(|id| waiting(id) == 0) {
        return free.clone();
    }
    if !role.is_empty() && (peers.len() as u32) < app.max_role_bots() {
        if let Some(copy) = app.duplicate_bot(bot_id) {
            return copy;
        }
    }
    peers.into_iter().min_by_key(|id| waiting(id)).unwrap_or_else(|| bot_id.to_owned())
}

/// What the model reads for a message between bots: who it is from, and
/// whether it asks or answers.
pub fn framed(app: AppState, incoming: &Incoming) -> String {
    let name = incoming.from.name(app);
    let who = match &incoming.from {
        Sender::Main => format!("the main bot {name}"),
        Sender::Bot(_) => format!("the bot {name}"),
        Sender::Github | Sender::Routine(_) | Sender::Setup => name.clone(),
    };
    // Where the work is, when it comes with a folder.
    let place = match (&incoming.project, incoming.report) {
        (Some(project), None) => format!("\nWork in: {project}"),
        _ => String::new(),
    };
    match (&incoming.from, incoming.report) {
        (_, Some(outcome)) => format!("[Report from {who}: {outcome}]\n{}", incoming.text),
        (Sender::Setup, None) => format!("[Setup]\n{}", incoming.text),
        (Sender::Github, None) => format!("[From GitHub]{place}\n{}", incoming.text),
        (Sender::Routine(routine), None) => format!("[Your routine \"{routine}\", on its schedule]{place}\n{}", incoming.text),
        (_, None) => format!("[From {who}]{place}\n{}", incoming.text),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
