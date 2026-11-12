//! The triage session (design §4.4): GitHub notifications no rule placed
//! (mentions, comments, state changes) are decided by a hidden turn of the
//! main bot's model with five tools: list_bots, get_bot, attach_event,
//! create_task_from_event and ignore_event. It appears in no list; its
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

/// A batch waits this long after its newest event before triage starts.
const QUIET_MS: i64 = 30_000;

thread_local! {
    static RUNNING: Cell<bool> = const { Cell::new(false) };
}

pub const PROMPT: &str = "You triage GitHub notifications for the person, on Bubble's behalf. Each notification below is \
something on GitHub that mentions them or a thread they take part in; no rule placed it. Decide each one, with a tool call per notification:\n\
- attach_event when it adds to work a bot already has (call list_bots and get_bot to see what exists); that bot gets it.\n\
- give_event_to_bot when it asks for new work: a short title and a self-contained brief (what is asked, where to read it with \
`gh`, what done means). Leave bot out to give it to the bot that looks after the repository. The work waits for the person's \
yes unless start is true and the repository allows that; set start only when the work is clear and safe to begin without them.\n\
- ignore_event when it needs nothing (a thank-you, a bot, news that changes no work), with a short reason.\n\
Decide every notification; then answer with one line saying what you did.";

pub fn specs() -> Vec<ToolSpec> {
    let manager = crate::manager::specs();
    let mut specs: Vec<ToolSpec> = manager.into_iter().filter(|s| s.name == "list_bots" || s.name == "get_bot").collect();
    specs.extend([
        ToolSpec {
            name: "attach_event".into(),
            description: "Give a notification to a bot that already has that work: it is linked to the bot and the bot is told.".into(),
            parameters: json!({
                "type": "object",
                "properties": { "event_id": { "type": "string" }, "bot": { "type": "string", "description": "Its id or its name" } },
                "required": ["event_id", "bot"]
            }),
        },
        ToolSpec {
            name: "give_event_to_bot".into(),
            description: "Make a notification into work for a bot. It waits for the person's yes unless start is true and the repository allows that.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "event_id": { "type": "string" },
                    "bot": { "type": "string", "description": "Its id or its name; the repository's own bot when left out" },
                    "title": { "type": "string" },
                    "brief": { "type": "string" },
                    "start": { "type": "boolean" }
                },
                "required": ["event_id", "title", "brief"]
            }),
        },
        ToolSpec {
            name: "ignore_event".into(),
            description: "Mark a notification as needing nothing.".into(),
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
    format!(
        "{} | {} in {} #{} | \"{}\"",
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
            let bot = match text("bot").as_str() {
                "" => crate::manager::bot_for_project_or_new(
                    app,
                    &home.project,
                    &format!("Looks after the GitHub repository {repo}: its issues, pull requests and reviews."),
                ),
                named => match crate::manager::find(app, named) {
                    Ok(bot) => Some(bot.task.id),
                    Err(message) => return error(message),
                },
            };
            let Some(bot) = bot else { return error("No bot could take it.") };
            let start = args["start"] == true && home.level == Some(Level::Act);
            let saved = app.with_store(|s| {
                s.link(&bot, &kind, &repo, number)?;
                let mut kept = p.clone();
                kept["brief"] = json!(brief);
                s.set_event_payload(&id, &kept.to_string())?;
                s.set_event_disposition(&id, "created", Some(&bot))
            });
            if saved.is_none() {
                return error("The notification could not be saved.");
            }
            let standing = if start { crate::manager::Standing::Accept } else { crate::manager::Standing::Propose };
            let asker = format!("{repo}#{number}");
            crate::manager::bring(app, &bot, crate::manager::Sender::Github, &title, &brief, &asker, false, standing);
            app.refresh_tasks();
            app.task_revision.update(|revision| *revision += 1);
            ok(format!(
                "Gave it to bot {bot} as \"{title}\"; {}.",
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
    if bubble_store::now_ms() - newest < quiet {
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
        app.refresh_away();
    });
}
