//! The TypeScript CLI's sessions (`~/.bubble/sessions/<folder>/<time>.jsonl`)
//! brought into the store, once: each becomes a finished command-line task
//! in its folder with its conversation (images, tool calls and results,
//! compaction summaries as checkpoints) and its original times, so
//! `/session`, `-r` and the desktop app show it. The files stay as they
//! were; a file already imported is skipped.

use std::path::{Path, PathBuf};

use anyhow::Result;
use bubble_core::{ChatMessage, Image, ToolCall, ToolResult};
use rusqlite::params;
use serde_json::Value;

use crate::{Lifecycle, MessageRole, Origin, RunRole, RunStatus, Store, new_id};

/// What an import did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub imported: usize,
    /// Already imported, or with nothing to keep (no message of the person).
    pub skipped: usize,
    /// Files that could not be read.
    pub failed: Vec<String>,
}

/// The setting that says the import ran.
pub const DONE: &str = "ts-sessions-imported";

/// One message as kept: role, display text, the model's view, when.
struct Kept {
    role: MessageRole,
    text: String,
    content: String,
    at: i64,
}

/// A session read from its file.
struct Session {
    title: String,
    cwd: String,
    model: Option<String>,
    messages: Vec<Kept>,
}

fn timestamp(value: &Value) -> Option<i64> {
    value.as_i64().or_else(|| value.as_str().and_then(|s| s.parse().ok()))
}

/// The folder a session directory stands for (`_Users_me_proj` →
/// `/Users/me/proj`), when the metadata does not say.
fn folder_of(directory: &str) -> String {
    directory.replace('_', "/")
}

fn title_of(text: &str) -> String {
    let line = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("Imported session").trim();
    if line.chars().count() > 60 { format!("{}…", line.chars().take(59).collect::<String>()) } else { line.to_owned() }
}

fn user_message(content: &Value) -> Option<ChatMessage> {
    match content {
        Value::String(text) if !text.trim().is_empty() => Some(ChatMessage::user(text.clone())),
        Value::Array(parts) => {
            let mut text = Vec::new();
            let mut images = Vec::new();
            for part in parts {
                match part["type"].as_str() {
                    Some("text") => text.push(part["text"].as_str().unwrap_or_default().to_owned()),
                    Some("image_url") => {
                        let url = part["image_url"]["url"].as_str().or(part["image_url"].as_str()).unwrap_or_default();
                        if let Some((head, data)) = url.strip_prefix("data:").and_then(|u| u.split_once(";base64,")) {
                            images.push(Image { media_type: head.to_owned(), data: data.to_owned() });
                        }
                    }
                    _ => {}
                }
            }
            let text = text.join("\n");
            (!text.trim().is_empty() || !images.is_empty()).then(|| ChatMessage::user_with_images(text, images))
        }
        _ => None,
    }
}

fn read(path: &Path, directory: &str) -> Result<Option<Session>> {
    let text = std::fs::read_to_string(path)?;
    let mut title = None;
    let mut cwd = None;
    let mut model = None;
    // The conversation as the model saw it, and when each message came.
    let mut history: Vec<(ChatMessage, i64)> = Vec::new();
    let mut kept: Vec<Kept> = Vec::new();
    // The assistant message tool calls attach to, by its entry id.
    let mut assistant_id: Option<String> = None;
    let mut first_user = None;
    let mut last_at = 0;
    for line in text.lines() {
        let Ok(entry) = serde_json::from_str::<Value>(line) else { continue };
        let at = timestamp(&entry["timestamp"]).unwrap_or(last_at);
        last_at = last_at.max(at);
        match entry["type"].as_str().unwrap_or_default() {
            "metadata" => {
                let meta = &entry["metadata"];
                title = title.or(meta["title"].as_str().map(str::to_owned));
                cwd = cwd.or(meta["cwd"].as_str().map(str::to_owned));
                model = model.or(meta["model"].as_str().map(str::to_owned));
            }
            "user_message" | "message" => {
                let message = if entry["type"] == "message" { &entry["data"] } else { &entry["message"] };
                match message["role"].as_str() {
                    Some("user") => {
                        if let Some(user) = user_message(&message["content"]) {
                            if let ChatMessage::User { text, .. } = &user {
                                first_user.get_or_insert(text.clone());
                            }
                            history.push((user, at));
                            assistant_id = None;
                        }
                    }
                    Some("assistant") => {
                        let text = message["content"].as_str().unwrap_or_default().to_owned();
                        history.push((ChatMessage::assistant(text), at));
                        assistant_id = entry["id"].as_str().map(str::to_owned);
                    }
                    _ => {}
                }
            }
            "assistant_message" => {
                let message = &entry["message"];
                history.push((
                    ChatMessage::Assistant {
                        text: message["content"].as_str().unwrap_or_default().to_owned(),
                        reasoning: message["reasoning"].as_str().unwrap_or_default().to_owned(),
                        tool_calls: Vec::new(),
                        state: Value::Null,
                    },
                    at,
                ));
                assistant_id = entry["id"].as_str().map(str::to_owned);
            }
            "tool_call" => {
                let call = &entry["toolCall"];
                let owner = entry["id"].as_str().and_then(|id| id.split(":tool:").next()).map(str::to_owned);
                let tool = ToolCall {
                    id: call["id"].as_str().unwrap_or_default().to_owned(),
                    name: call["name"].as_str().unwrap_or_default().to_owned(),
                    arguments: match &call["arguments"] {
                        Value::String(s) => s.clone(),
                        other => other.to_string(),
                    },
                };
                // Onto its assistant message (the latest one when unnamed).
                if owner.is_none() || owner == assistant_id {
                    if let Some((ChatMessage::Assistant { tool_calls, .. }, _)) = history.iter_mut().rev().find(|(m, _)| matches!(m, ChatMessage::Assistant { .. })) {
                        tool_calls.push(tool);
                    }
                }
            }
            "tool_result" => {
                let message = &entry["message"];
                let result = ToolResult {
                    call_id: message["toolCallId"].as_str().unwrap_or_default().to_owned(),
                    content: match &message["content"] {
                        Value::String(s) => s.clone(),
                        other => other.to_string(),
                    },
                    is_error: message["isError"].as_bool().unwrap_or(false),
                };
                match history.last_mut() {
                    Some((ChatMessage::Tool { results }, _)) => results.push(result),
                    _ => history.push((ChatMessage::Tool { results: vec![result] }, at)),
                }
            }
            "summary" => {
                // The conversation from here: the summary, then the turn in
                // progress (from the person's last message), as TypeScript
                // kept it.
                let summary = entry["summary"].as_str().unwrap_or_default();
                flush(&mut history, &mut kept);
                let tail_start = kept.iter().rposition(|k| k.role == MessageRole::User).unwrap_or(kept.len());
                let mut compacted = vec![ChatMessage::user(format!(
                    "{}\n{summary}\n</bubble_internal_context>",
                    bubble_core::compact::SUMMARY_MARKER
                ))];
                compacted.extend(kept[tail_start..].iter().filter_map(|k| serde_json::from_str(&k.content).ok()));
                kept.push(Kept {
                    role: MessageRole::Checkpoint,
                    text: "Earlier messages were summarized to stay within the model's context".into(),
                    content: serde_json::to_string(&compacted)?,
                    at,
                });
            }
            _ => {}
        }
    }
    flush(&mut history, &mut kept);
    let Some(first) = first_user else { return Ok(None) };
    Ok(Some(Session {
        title: title.filter(|t| !t.trim().is_empty()).unwrap_or_else(|| title_of(&first)),
        cwd: cwd.unwrap_or_else(|| folder_of(directory)),
        model,
        messages: kept,
    }))
}

/// Moves the messages read so far into the kept list.
fn flush(history: &mut Vec<(ChatMessage, i64)>, kept: &mut Vec<Kept>) {
    for (message, at) in history.drain(..) {
        let (role, text) = match &message {
            ChatMessage::User { text, .. } => (MessageRole::User, text.clone()),
            ChatMessage::Assistant { text, .. } => (MessageRole::Assistant, text.clone()),
            ChatMessage::Tool { .. } => (MessageRole::Tool, String::new()),
        };
        if let Ok(content) = serde_json::to_string(&message) {
            kept.push(Kept { role, text, content, at });
        }
    }
}

impl Store {
    /// Imports one session file (unless it already was); the new task's id.
    fn import_file(&self, path: &Path, directory: &str) -> Result<Option<String>> {
        let source = path.display().to_string();
        let key = format!("import:{source}");
        if self.setting(&key)?.is_some() {
            return Ok(None);
        }
        let Some(session) = read(path, directory)? else {
            self.set_setting(&key, Some(""))?;
            return Ok(None);
        };
        let created = session.messages.iter().map(|m| m.at).filter(|t| *t > 0).min().unwrap_or_else(crate::now_ms);
        let updated = session.messages.iter().map(|m| m.at).max().unwrap_or(created).max(created);
        let task_id = new_id("task");
        let run_id = new_id("run");
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO tasks (id, title, project_cwd, lifecycle, waiting_on, origin, created_at, updated_at, closed_at)
             VALUES (?1, ?2, ?3, ?4, NULL, ?5, ?6, ?7, ?7)",
            params![task_id, session.title, session.cwd, Lifecycle::Done.as_str(), Origin::Cli.as_str(), created, updated],
        )?;
        tx.execute(
            "INSERT INTO runs (id, task_id, role, model, cwd, status, last_error, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, ?7, ?8)",
            params![run_id, task_id, RunRole::Task.as_str(), session.model, session.cwd, RunStatus::Idle.as_str(), created, updated],
        )?;
        for (seq, message) in session.messages.iter().enumerate() {
            tx.execute(
                "INSERT INTO messages (id, run_id, seq, role, text, content, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![new_id("msg"), run_id, seq as i64 + 1, message.role.as_str(), message.text, message.content, message.at.max(created)],
            )?;
        }
        for (key, value) in [(key.as_str(), task_id.as_str()), (&format!("imported-task:{task_id}"), source.as_str())] {
            tx.execute(
                "INSERT INTO settings (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, value],
            )?;
        }
        tx.commit()?;
        Ok(Some(task_id))
    }

    /// Imports every session under `root` (`~/.bubble/sessions`).
    pub fn import_ts_sessions(&self, root: &Path) -> Result<Report> {
        let mut report = Report::default();
        let Ok(folders) = std::fs::read_dir(root) else { return Ok(report) };
        let mut files: Vec<(PathBuf, String)> = Vec::new();
        for folder in folders.flatten().filter(|f| f.path().is_dir()) {
            let directory = folder.file_name().to_string_lossy().into_owned();
            if let Ok(entries) = std::fs::read_dir(folder.path()) {
                files.extend(
                    entries.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "jsonl")).map(|p| (p, directory.clone())),
                );
            }
        }
        files.sort();
        for (path, directory) in files {
            match self.import_file(&path, &directory) {
                Ok(Some(_)) => report.imported += 1,
                Ok(None) => report.skipped += 1,
                Err(error) => report.failed.push(format!("{}: {error:#}", path.display())),
            }
        }
        self.set_setting(DONE, Some(&crate::now_ms().to_string()))?;
        Ok(report)
    }

    /// The tasks that came from imported sessions.
    pub fn imported_task_ids(&self) -> Result<std::collections::HashSet<String>> {
        let conn = self.conn();
        let mut statement = conn.prepare("SELECT substr(key, 15) FROM settings WHERE key LIKE 'imported-task:%'")?;
        let ids = statement.query_map([], |row| row.get::<_, String>(0))?.collect::<rusqlite::Result<_>>()?;
        Ok(ids)
    }

    /// Whether task `id` came from an imported session.
    pub fn is_imported(&self, id: &str) -> bool {
        self.setting(&format!("imported-task:{id}")).ok().flatten().is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sessions_come_in_once_with_their_conversation() {
        let root = std::env::temp_dir().join(format!("bubble-import-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let folder = root.join("_Users_me_proj");
        std::fs::create_dir_all(&folder).unwrap();
        let lines = [
            r#"{"id":"metadata","type":"metadata","metadata":{"model":"kimi:k3","cwd":"/Users/me/proj","title":"Fix the build"},"timestamp":1000}"#,
            r#"{"id":"1","type":"user_message","message":{"role":"user","content":[{"type":"text","text":"why does it fail?"},{"type":"image_url","image_url":{"url":"data:image/png;base64,AAAA"}}]},"timestamp":1000}"#,
            r#"{"id":"2","type":"assistant_message","message":{"role":"assistant","content":"Looking.","reasoning":"check logs"},"timestamp":2000}"#,
            r#"{"id":"2:tool:1","type":"tool_call","toolCall":{"id":"c1","name":"bash","arguments":"{\"command\":\"make\"}"},"timestamp":2000}"#,
            r#"{"id":"3","type":"tool_result","message":{"role":"tool","toolCallId":"c1","content":"error: x","isError":true},"timestamp":3000}"#,
            r#"{"id":"4","type":"marker","kind":"skill_activated","value":"x","timestamp":3000}"#,
            r#"{"id":"5","type":"summary","summary":"Goal: fix the build","timestamp":4000}"#,
            r#"{"id":"6","type":"user_message","message":{"role":"user","content":"and now?"},"timestamp":5000}"#,
        ];
        std::fs::write(folder.join("2026-01-01T00-00-00-000Z.jsonl"), lines.join("\n")).unwrap();
        std::fs::write(folder.join("empty.jsonl"), r#"{"id":"metadata","type":"metadata","metadata":{}}"#).unwrap();
        let store = Store::open_in_memory().unwrap();
        let report = store.import_ts_sessions(&root).unwrap();
        assert_eq!(report, Report { imported: 1, skipped: 1, failed: Vec::new() });
        let task = store.latest_cli_task("/Users/me/proj").unwrap().unwrap();
        assert_eq!(task.title, "Fix the build");
        assert_eq!((task.created_at, task.updated_at), (1000, 5000), "the session's own times");
        assert_eq!(task.lifecycle, Lifecycle::Done);
        assert!(store.is_imported(&task.id));
        assert!(store.imported_task_ids().unwrap().contains(&task.id));
        let run = &store.runs_for_task(&task.id).unwrap()[0];
        let messages = store.messages(&run.id).unwrap();
        let history = crate::history(&messages);
        // After the summary: the summary, the turn it closed (kept as TS
        // kept its newest turn), then the person's new message.
        assert_eq!(history.len(), 5, "{history:?}");
        assert!(matches!(&history[0], ChatMessage::User { text, .. } if text.contains("Goal: fix the build")));
        assert!(matches!(&history[4], ChatMessage::User { text, .. } if text == "and now?"));
        // The full conversation is kept for display, image and call included.
        let first: ChatMessage = serde_json::from_str(messages[0].content.as_deref().unwrap()).unwrap();
        assert!(matches!(first, ChatMessage::User { images, .. } if images.len() == 1));
        let reply: ChatMessage = serde_json::from_str(messages[1].content.as_deref().unwrap()).unwrap();
        assert!(matches!(reply, ChatMessage::Assistant { tool_calls, reasoning, .. } if tool_calls[0].id == "c1" && reasoning == "check logs"));
        // Again: nothing new.
        assert_eq!(store.import_ts_sessions(&root).unwrap().imported, 0);
    }
}
