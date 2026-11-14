//! The triage session (design §4.4): GitHub notifications no rule placed
//! (mentions, comments, state changes, newly opened issues and pull
//! requests) are decided by a hidden turn of the
//! main bot's model: it is the main bot handing work out, in the
//! background, with list_bots, get_bot, create_bot, attach_event,
//! give_event_to_bot and ignore_event. It appears in no list; its
//! decisions are written to the events, where the "while you were away"
//! row and the tasks show them.

use std::cell::Cell;
use std::sync::Arc;

use bubble_core::{AgentEvent, Decision, ToolCall, ToolOutput, ToolSpec};
use bubble_store::{Event, RunRole};
use nsh_core::prelude::*;
use serde_json::{Value, json};

use crate::engine::{self, HostTurn, TurnEvent};
use crate::github::{self, Level};
use crate::state::AppState;

/// A batch waits this long after its newest event before triage starts,
/// so what comes in together is sorted together.
const QUIET_MS: i64 = 15_000;

thread_local! {
    static RUNNING: Cell<bool> = const { Cell::new(false) };
    /// A look is due when the batch has been quiet long enough.
    static DUE: Cell<bool> = const { Cell::new(false) };
}

pub const PROMPT: &str = "You are Bubble, the person's main bot, sorting what came in from GitHub while they were busy. They do not \
see this conversation, only what becomes of each item. Each item below is something in a repository they have Bubble watch: an issue or \
pull request newly opened (reason `opened`, with who opened it; the person's own issue is them asking for it), an assignment or a \
review request, a mention, or news on a thread they take part in. Decide each one, with one tool call per item:\n\
- ignore_event when it needs no work (a thank-you, a duplicate, a question already answered, news that changes nothing), with a short \
reason.\n\
- attach_event when it adds to work a bot already has (get_bot shows what a bot is on); that bot gets it.\n\
- give_event_to_bot when it asks for new work. Choose the bot by its role with list_bots: bots are roles on the team (a developer, a \
reviewer), not keepers of one repository, and the same bot works in any project. Give a short title and a self-contained brief that \
says no more than the item asks: what is asked, where to read it with `gh`, what done means. Do not add work the item does not ask for, \
and do not ask for a pull request unless the item does. Set priority to first for what blocks someone or was asked of the person by \
name, to later for what can wait, and leave it out otherwise. You need not check whether the bot is busy: work for a busy bot goes to a \
free bot of its role, or to a copy of it.\n\
- When no bot's role fits the work, make one first with create_bot (a name, a role, instructions), then give it the work.\n\
Whether new work begins at once is not yours to say: it does when it comes from the person or someone who works on the repository, \
and waits for the person's yes when it comes from anyone else.\n\
Decide every item; then answer with one line saying what you did.";

pub fn specs() -> Vec<ToolSpec> {
    let manager = crate::manager::specs();
    let mut specs: Vec<ToolSpec> = manager.into_iter().filter(|s| matches!(s.name.as_str(), "list_bots" | "get_bot")).collect();
    specs.extend([
        ToolSpec {
            name: "create_bot".into(),
            description: "Make a new long-lived bot for a role no bot has yet: a role on the team, not tied to a folder. Then give it the work.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "Short, in the person's language; no other bot has it" },
                    "role": { "type": "string", "description": "Its role in a word or two: Developer, Reviewer, Writer" },
                    "instructions": { "type": "string", "description": "What the bot is responsible for and how it should work" }
                },
                "required": ["name", "role"]
            }),
        },
        ToolSpec {
            name: "attach_event".into(),
            description: "Give an item to a bot that already has that work: it is linked to the bot and the bot is told.".into(),
            parameters: json!({
                "type": "object",
                "properties": { "event_id": { "type": "string" }, "bot": { "type": "string", "description": "Its id or its name" } },
                "required": ["event_id", "bot"]
            }),
        },
        ToolSpec {
            name: "give_event_to_bot".into(),
            description: "Make an item into work for a bot, done in the item's project folder. It begins at once when it comes from the person or someone who works on the repository; from anyone else it waits for the person's yes.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "event_id": { "type": "string" },
                    "bot": { "type": "string", "description": "The id or name of the bot whose role fits" },
                    "title": { "type": "string" },
                    "brief": { "type": "string" },
                    "priority": { "type": "string", "enum": ["first", "normal", "later"] }
                },
                "required": ["event_id", "bot", "title", "brief"]
            }),
        },
        ToolSpec {
            name: "ignore_event".into(),
            description: "Mark an item as needing nothing.".into(),
            parameters: json!({
                "type": "object",
                "properties": { "event_id": { "type": "string" }, "reason": { "type": "string" } },
                "required": ["event_id", "reason"]
            }),
        },
    ]);
    specs
}

fn payload(event: &Event) -> Value {
    serde_json::from_str(&event.payload).unwrap_or_else(|_| json!({ "text": event.payload }))
}

/// A notification as the triage model reads it.
fn describe(event: &Event) -> String {
    let p = payload(event);
    let by = p["author"].as_str().map(|author| format!(" by @{author}")).unwrap_or_default();
    // Asked of the person by name: an assignment, a review request.
    let pressing = if p["priority"] == "first" { " | asked of the person by name" } else { "" };
    format!(
        "{} | {}{by} in {} #{} | \"{}\"{pressing}",
        event.id,
        event.kind,
        event.repo.as_deref().unwrap_or("?"),
        event.number.unwrap_or_default(),
        p["title"].as_str().unwrap_or_default()
    )
}

fn ok(text: impl Into<String>) -> ToolOutput {
    ToolOutput { content: text.into(), is_error: false }
}

fn error(text: impl Into<String>) -> ToolOutput {
    ToolOutput { content: text.into(), is_error: true }
}

/// Answers one of the triage session's calls, for the events it was given.
fn execute(app: AppState, call: &ToolCall, batch: &[Event]) -> ToolOutput {
    let args = call.arguments_json();
    let text = |key: &str| args[key].as_str().map(str::trim).unwrap_or_default().to_owned();
    if matches!(call.name.as_str(), "list_bots" | "get_bot") {
        return crate::manager::execute(app, &crate::manager::Sender::Main, call);
    }
    if call.name == "create_bot" {
        // Its work is on its way: it has no need to ask where to start.
        return crate::manager::make_bot(app, &json!({ "name": text("name"), "role": text("role"), "instructions": text("instructions") }), false);
    }
    let id = text("event_id");
    let Some(event) = batch.iter().find(|e| e.id == id) else {
        return error(format!("There is no notification {id} in this batch."));
    };
    let still_open = app.with_store(|s| s.get_event(&id)).flatten().is_some_and(|e| e.disposition == "untriaged");
    if !still_open {
        return error(format!("Notification {id} is already decided."));
    }
    let p = payload(event);
    let (repo, number) = (event.repo.clone().unwrap_or_default(), event.number.unwrap_or_default());
    let kind = p["kind"].as_str().unwrap_or("issue").to_owned();
    match call.name.as_str() {
        "attach_event" => {
            let task_id = match crate::manager::find(app, &text("bot")) {
                Ok(bot) => bot.task.id,
                Err(message) => return error(message),
            };
            // The task's agent reads what happened; it goes out when the
            // task can take it.
            let message = p["brief"].as_str().unwrap_or(&event.payload).to_owned();
            let done = app.with_store(|s| {
                s.link(&task_id, &kind, &repo, number)?;
                s.set_event_payload(&id, &message)?;
                s.set_event_disposition(&id, "pending", Some(&task_id))
            });
            if done.is_none() {
                return error("The notification could not be saved.");
            }
            github::first_look(app);
            github::deliver_pending(app);
            ok(format!("Gave {id} to bot {task_id}."))
        }
        "give_event_to_bot" => {
            let (title, brief) = (text("title"), text("brief"));
            if title.is_empty() || brief.is_empty() {
                return error("The work needs a title and a brief.");
            }
            let repos = app.github_repos();
            let Some(home) = repos.iter().find(|r| r.name == repo) else {
                return error(format!("{repo} is not a watched project."));
            };
            let bot = match crate::manager::find(app, &text("bot")) {
                Ok(bot) => bot.task.id,
                Err(message) => return error(format!("{message} When no bot's role fits, make one with create_bot.")),
            };
            // A bot was given it in the meantime (an assignment came in
            // after it was opened): one piece of work, not two.
            if let Some(holder) = app.with_store(|s| s.task_for_link(&kind, &repo, number)).flatten() {
                app.with_store(|s| s.set_event_disposition(&id, "attached", Some(&holder)));
                return ok(format!("Bot {holder} already has {repo}#{number}; nothing was added."));
            }
            // Where bots may work, work begins without asking (what they
            // publish is another matter, and has its own asking). Except
            // what a stranger wrote: that waits for the person.
            let known = p["known"] == true;
            let start = matches!(home.level, Some(Level::Prepare | Level::Act)) && known;
            let author = p["author"].as_str().filter(|a| !a.is_empty()).map(|a| format!("@{a}"));
            let why = match (known, &author) {
                (true, _) => None,
                (false, Some(author)) => Some(format!("from {author}, who does not work on the repository")),
                (false, None) => Some("from someone who may not work on the repository".to_owned()),
            };
            let priority = match args["priority"].as_str().or_else(|| p["priority"].as_str()) {
                Some("first") => crate::manager::FIRST,
                Some("later") => crate::manager::LATER,
                _ => crate::manager::NORMAL,
            };
            let standing = if start { crate::manager::Standing::Accept } else { crate::manager::Standing::Propose };
            let asker = format!("{repo}#{number}");
            let work = crate::manager::Work {
                from: crate::manager::Sender::Github,
                title: &title,
                text: &brief,
                asker: &asker,
                explicit: false,
                // The work is in the repository's folder.
                project: Some(home.project.as_str()),
                priority,
                why_asked: why.as_deref(),
            };
            // Whichever bot of that role is free takes it.
            let Some(ticket) = crate::manager::bring(app, &bot, work, standing, true) else {
                return error("The work could not be saved.");
            };
            let worker = ticket.task_id.clone();
            let saved = app.with_store(|s| {
                s.link(&worker, &kind, &repo, number)?;
                let mut kept = p.clone();
                kept["brief"] = json!(brief);
                s.set_event_payload(&id, &kept.to_string())?;
                s.set_event_disposition(&id, "created", Some(&worker))
            });
            if saved.is_none() {
                return error("The item could not be saved.");
            }
            github::first_look(app);
            app.refresh_tasks();
            app.task_revision.update(|revision| *revision += 1);
            let name = crate::manager::Sender::Bot(worker.clone()).name(app);
            ok(format!(
                "Gave it to {name} ({worker}) as \"{title}\"; {}.",
                if start { "the bot is working on it" } else { "it waits for the person's yes" }
            ))
        }
        "ignore_event" => {
            let mut kept = p.clone();
            kept["ignored_reason"] = json!(text("reason"));
            app.with_store(|s| {
                s.set_event_payload(&id, &kept.to_string())?;
                s.set_event_disposition(&id, "ignored", None)
            });
            ok(format!("Ignored {id}."))
        }
        other => error(format!("There is no tool called {other}.")),
    }
}

/// Starts a triage turn when a batch has been quiet long enough and none
/// is running.
pub fn maybe_run(app: AppState) {
    if RUNNING.with(Cell::get) {
        return;
    }
    let batch = app.with_store(|s| s.untriaged_events()).unwrap_or_default();
    let Some(newest) = batch.iter().map(|e| e.created_at).max() else { return };
    let quiet = std::env::var("BUBBLE_TRIAGE_QUIET_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(QUIET_MS);
    let wait = quiet - (bubble_store::now_ms() - newest);
    if wait > 0 {
        // Sorted as soon as the batch is quiet, not at the next look at
        // GitHub, which may be a minute away.
        if !DUE.with(|due| due.replace(true)) {
            spawn(async move {
                timer(std::time::Duration::from_millis(wait as u64 + 50)).await;
                DUE.with(|due| due.set(false));
                maybe_run(app);
            });
        }
        return;
    }
    let Some(reference) = app.model_for(RunRole::Triage) else { return };
    RUNNING.with(|r| r.set(true));
    let listing: Vec<String> = batch.iter().map(describe).collect();
    let message = format!("Notifications to decide (id | reason in repository #number | title):\n{}", listing.join("\n"));
    // A session of its own that nothing lists, gone when the turn is done.
    let session = format!("desk-triage-{}", bubble_store::now_ms());
    let turn = HostTurn {
        session_id: session.clone(),
        home: crate::state::scratch_folder().to_string_lossy().into_owned(),
        cwd: crate::state::scratch_folder().to_string_lossy().into_owned(),
        text: message,
        pictures: Vec::new(),
        model: reference,
        thinking: bubble_core::Level::Low.as_str().to_owned(),
        agent_name: None,
        append_system_prompt: None,
        system_prompt: Some(PROMPT.to_owned()),
        // Only the triage tools: it decides, it does not work.
        allowed_tools: Some(Vec::new()),
        host_tools: specs(),
        recap: None,
    };
    let batch = Arc::new(batch);
    let hosts = app.hosts.get();
    spawn(async move {
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel::<TurnEvent>();
        let job = background(async move {
            let host = hosts.get().await?;
            let result = engine::run(host.clone(), turn, sender, Arc::new(tokio::sync::Notify::new())).await;
            let _ = host.delete_session(&session).await;
            result
        });
        while let Some(event) = receiver.recv().await {
            match event {
                TurnEvent::Agent(AgentEvent::HostCall { call, respond }) => {
                    let _ = respond.send(execute(app, &call, &batch));
                }
                TurnEvent::Agent(AgentEvent::ApprovalNeeded { respond, .. }) => {
                    let _ = respond.send(Decision::Deny);
                }
                _ => {}
            }
        }
        if let Ok(Err(error)) = job.await {
            log::warn!("triage: {error:#}");
        }
        // What it left undecided is recorded, so it is not asked again.
        for event in batch.iter() {
            let open = app.with_store(|s| s.get_event(&event.id)).flatten().is_some_and(|e| e.disposition == "untriaged");
            if open {
                app.with_store(|s| s.set_event_disposition(&event.id, "noted", None));
            }
        }
        RUNNING.with(|r| r.set(false));
        app.refresh_tasks();
        app.refresh_sorting();
    });
}
