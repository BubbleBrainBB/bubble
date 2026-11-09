//! Keeping a conversation inside the model's context window: estimate how
//! big the next request is, and when it nears the window, have the model
//! write a handoff summary of the older part. Ported from the TypeScript
//! core (`context/budget.ts`, `context/llm-compactor.ts`).

use std::collections::HashMap;

use anyhow::{Result, bail};

use crate::config::ResolvedModel;
use crate::model::Level;
use crate::provider::{ChatMessage, ChatRequest, ToolSpec, stream_chat};

const OUTPUT_RESERVE_TOKENS: u64 = 20_000;
const AUTOCOMPACT_BUFFER_TOKENS: u64 = 13_000;
const MIN_WINDOW_FOR_RESERVE: u64 = 40_000;
const UNKNOWN_WINDOW_TRIGGER: u64 = 32_000;
const KEEP_RECENT_GROUPS: usize = 2;
const PINNED_INSTRUCTION_MAX_CHARS: usize = 8192;
const TOOL_PAYLOAD_CAP: usize = 4000;

pub const SUMMARY_MARKER: &str = "<bubble_internal_context kind=\"compaction-summary\">";

pub const PROMPT: &str = "You are performing a CONTEXT CHECKPOINT COMPACTION. The conversation below is about to be removed from the model's context and replaced by your summary. Another LLM will resume the task seeing only your summary, the user's original instruction, and the most recent messages. Anything you leave out is gone.

Write a handoff summary that lets it continue without repeating the investigation. Cover:
- Goal and constraints: what the user wants, in their terms. Explicit requirements, preferences, and anything they ruled out. Quote exact wording where precision matters.
- Decisions made and why, including approaches that were tried and rejected, so they are not retried.
- Current state: what is done and verified, what is in progress, what is broken or still unverified. Keep verified facts distinct from assumptions.
- Specifics needed to continue: file paths, identifiers, commands, error messages, config values, ids, URLs. Exact, never paraphrased.
- Next steps in order, starting with the immediate one.

Length follows content: as long as needed to preserve the above, and no longer. Spend words on what cannot be cheaply re-derived (decisions, constraints, findings). Do not reproduce file contents or tool output that can simply be read again, and do not narrate the conversation turn by turn. The lists of files read and modified are recorded separately; name only the files that matter for the next steps.

Carry forward every fact from prior summaries and the original user constraints. If the input says parts of the history were omitted, say so explicitly.";

const SUMMARY_PREFIX: &str = "Another language model previously worked on this task and produced this handoff summary. Build on what's already done; avoid re-running the same investigation. Summary:";

/// Tokens in a text: CJK characters count one each, the rest one per four.
pub fn estimate_text(text: &str) -> u64 {
    let mut cjk = 0u64;
    let mut other = 0u64;
    for c in text.chars() {
        if matches!(c as u32, 0x3000..=0x9FFF | 0xAC00..=0xD7AF | 0xF900..=0xFAFF | 0xFF00..=0xFFEF) {
            cjk += 1;
        } else {
            other += 1;
        }
    }
    cjk + other.div_ceil(4)
}

pub fn estimate_messages(messages: &[ChatMessage]) -> u64 {
    messages
        .iter()
        .map(|message| match message {
            ChatMessage::User { text } => estimate_text(text) + 8,
            ChatMessage::Assistant { text, tool_calls, .. } => {
                estimate_text(text)
                    + 8
                    + tool_calls.iter().map(|c| estimate_text(&c.name) + estimate_text(&c.arguments) + 12).sum::<u64>()
            }
            ChatMessage::Tool { results } => results.iter().map(|r| estimate_text(&r.content) + 8).sum(),
        })
        .sum()
}

pub fn estimate_tools(tools: &[ToolSpec]) -> u64 {
    if tools.is_empty() {
        return 0;
    }
    tools
        .iter()
        .map(|t| estimate_text(&t.name) + estimate_text(&t.description) + estimate_text(&t.parameters.to_string()) + 16)
        .sum::<u64>()
        + 16
}

/// What the next request weighs: the provider's last count plus what came
/// since, or a padded estimate of everything.
pub fn estimate_request(anchor: Option<(u64, usize)>, history: &[ChatMessage], tools: &[ToolSpec]) -> u64 {
    match anchor {
        Some((prompt_tokens, count)) if count <= history.len() => {
            prompt_tokens + (estimate_messages(&history[count..]) as f64 * 1.15).ceil() as u64
        }
        _ => ((estimate_messages(history) + estimate_tools(tools)) as f64 * 1.25).ceil() as u64,
    }
}

/// Whether a request of `estimate` tokens should be compacted first.
pub fn should_compact(window: Option<u64>, estimate: u64) -> bool {
    match window {
        Some(window) if window >= MIN_WINDOW_FOR_RESERVE => {
            estimate >= window - OUTPUT_RESERVE_TOKENS - AUTOCOMPACT_BUFFER_TOKENS
        }
        Some(window) => estimate as f64 >= window as f64 * 0.75,
        None => estimate >= UNKNOWN_WINDOW_TRIGGER,
    }
}

fn is_summary(message: &ChatMessage) -> bool {
    matches!(message, ChatMessage::User { text } if text.starts_with(SUMMARY_MARKER))
}

/// How a history splits for compaction.
#[derive(Debug, PartialEq)]
pub struct Plan {
    /// Kept before the summary: the first instruction, cut to its pin.
    pub head: Vec<ChatMessage>,
    /// Handed to the summarizer.
    pub middle: Vec<ChatMessage>,
    /// What the pin cut off the first instruction.
    pub overflow: Option<String>,
    /// Kept after the summary: the last instruction and the newest groups.
    pub tail: Vec<ChatMessage>,
}

/// Splits `history`; `None` when there is nothing old enough to summarize.
pub fn plan(history: &[ChatMessage]) -> Option<Plan> {
    let real_user = |m: &ChatMessage| matches!(m, ChatMessage::User { .. }) && !is_summary(m);
    let first = history.iter().position(real_user)?;
    let last = history.iter().rposition(real_user)?;
    // Groups after the last instruction: a reply and what follows it.
    let mut groups: Vec<(usize, usize)> = Vec::new();
    let mut index = last + 1;
    while index < history.len() {
        let start = index;
        index += 1;
        while index < history.len() && matches!(history[index], ChatMessage::Tool { .. }) {
            index += 1;
        }
        groups.push((start, index));
    }
    let keep_from = groups.len().saturating_sub(KEEP_RECENT_GROUPS);
    let tail_groups_start = groups.get(keep_from).map(|g| g.0).unwrap_or(history.len());

    let mut head = Vec::new();
    let mut overflow = None;
    if let ChatMessage::User { text } = &history[first] {
        let chars: Vec<char> = text.chars().collect();
        if chars.len() > PINNED_INSTRUCTION_MAX_CHARS {
            head.push(ChatMessage::user(chars[..PINNED_INSTRUCTION_MAX_CHARS].iter().collect::<String>()));
            overflow = Some(chars[PINNED_INSTRUCTION_MAX_CHARS..].iter().collect());
        } else {
            head.push(history[first].clone());
        }
    }
    let mut middle: Vec<ChatMessage> = history[..first].to_vec();
    let mut tail = Vec::new();
    if first == last {
        middle.extend_from_slice(&history[first + 1..tail_groups_start]);
    } else {
        middle.extend_from_slice(&history[first + 1..last]);
        middle.extend_from_slice(&history[last + 1..tail_groups_start]);
        tail.push(history[last].clone());
    }
    tail.extend_from_slice(&history[tail_groups_start..]);
    if middle.is_empty() {
        return None;
    }
    Some(Plan { head, middle, overflow, tail })
}

fn clip(text: &str, cap: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= cap {
        return text.to_owned();
    }
    let half = cap / 2;
    format!(
        "{}\n… [{} characters omitted] …\n{}",
        chars[..half].iter().collect::<String>(),
        chars.len() - cap,
        chars[chars.len() - half..].iter().collect::<String>()
    )
}

/// The part to summarize, as text for the summarizer.
pub fn serialize(plan: &Plan) -> String {
    let mut names: HashMap<String, String> = HashMap::new();
    let mut lines = Vec::new();
    for message in &plan.middle {
        match message {
            ChatMessage::User { text } if is_summary(message) => lines.push(format!("[Prior compaction summary]\n{text}")),
            ChatMessage::User { text } => lines.push(format!("USER: {text}")),
            ChatMessage::Assistant { text, tool_calls, .. } => {
                if !text.trim().is_empty() {
                    lines.push(format!("ASSISTANT: {text}"));
                }
                for call in tool_calls {
                    names.insert(call.id.clone(), call.name.clone());
                    lines.push(format!("TOOL_CALL[{}]: {}", call.name, clip(&call.arguments, TOOL_PAYLOAD_CAP)));
                }
            }
            ChatMessage::Tool { results } => {
                for result in results {
                    let name = names.get(&result.call_id).map(String::as_str).unwrap_or("tool");
                    lines.push(format!("TOOL_RESULT[{name}]: {}", clip(&result.content, TOOL_PAYLOAD_CAP)));
                }
            }
        }
    }
    if let Some(overflow) = &plan.overflow {
        lines.push(format!("[Original instruction beyond retained pin]\n{overflow}"));
    }
    lines.join("\n\n")
}

/// The files the summarized part read and changed, kept beside the summary.
fn file_lists(plan: &Plan) -> String {
    let mut read: Vec<String> = Vec::new();
    let mut modified: Vec<String> = Vec::new();
    for message in &plan.middle {
        if let ChatMessage::Assistant { tool_calls, .. } = message {
            for call in tool_calls {
                let Some(path) = call.arguments_json()["path"].as_str().map(str::to_owned) else { continue };
                let list = match call.name.as_str() {
                    "read" => &mut read,
                    "edit" | "write" => &mut modified,
                    _ => continue,
                };
                if !list.contains(&path) {
                    list.push(path);
                }
            }
        }
    }
    let render = |tag: &str, list: &[String]| {
        if list.is_empty() {
            return String::new();
        }
        let mut shown: Vec<String> = list.iter().take(200).cloned().collect();
        if list.len() > 200 {
            shown.push(format!("(+{} more)", list.len() - 200));
        }
        format!("\n<{tag}>\n{}\n</{tag}>", shown.join("\n"))
    };
    format!("{}{}", render("read-files", &read), render("modified-files", &modified))
}

/// The history rebuilt around a summary.
pub fn rebuild(plan: Plan, summary: &str) -> Vec<ChatMessage> {
    let files = file_lists(&plan);
    let summary = ChatMessage::user(format!(
        "{SUMMARY_MARKER}\nPrevious conversation summary:\n{SUMMARY_PREFIX}\n{}{files}\n</bubble_internal_context>",
        summary.trim()
    ));
    let mut history = plan.head;
    history.push(summary);
    history.extend(plan.tail);
    history
}

/// Has the model summarize the old part of `history`; the new history.
pub async fn compact(client: &reqwest::Client, model: &ResolvedModel, history: &[ChatMessage]) -> Result<Option<Vec<ChatMessage>>> {
    compact_with(client, model, history, None).await
}

/// [`compact`], keeping what the person asked to keep (`/compact <focus>`).
pub async fn compact_with(
    client: &reqwest::Client,
    model: &ResolvedModel,
    history: &[ChatMessage],
    focus: Option<&str>,
) -> Result<Option<Vec<ChatMessage>>> {
    let Some(plan) = plan(history) else { return Ok(None) };
    let system = match focus.map(str::trim).filter(|f| !f.is_empty()) {
        Some(focus) => format!("{PROMPT}\n\nThe person asked that the summary especially preserve: {focus}"),
        None => PROMPT.to_owned(),
    };
    let request = ChatRequest {
        system: Some(system),
        messages: vec![ChatMessage::user(serialize(&plan))],
        tools: Vec::new(),
        max_tokens: 8192,
        thinking: Level::Off,
    };
    let outcome = stream_chat(client, model, &request, |_| {}).await?;
    let summary = outcome.text.trim();
    if summary.is_empty() {
        bail!("the model wrote an empty summary");
    }
    if estimate_text(summary) >= estimate_messages(&plan.middle) {
        bail!("the summary is no shorter than what it replaces");
    }
    Ok(Some(rebuild(plan, summary)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{ToolCall, ToolResult};

    fn call(id: &str, name: &str, path: &str) -> ChatMessage {
        ChatMessage::Assistant {
            text: String::new(),
            reasoning: String::new(),
            tool_calls: vec![ToolCall { id: id.into(), name: name.into(), arguments: format!("{{\"path\":\"{path}\"}}") }],
        }
    }

    fn result(id: &str, content: &str) -> ChatMessage {
        ChatMessage::Tool { results: vec![ToolResult { call_id: id.into(), content: content.into(), is_error: false }] }
    }

    #[test]
    fn estimates_count_cjk_as_one_each() {
        assert_eq!(estimate_text("你好"), 2);
        assert_eq!(estimate_text("abcdefgh"), 2);
    }

    #[test]
    fn compaction_triggers_near_the_window() {
        assert!(!should_compact(Some(1_048_576), 900_000));
        assert!(should_compact(Some(1_048_576), 1_048_576 - 33_000));
        assert!(should_compact(Some(32_000), 24_000));
        assert!(should_compact(None, 32_000));
        assert!(!should_compact(None, 31_999));
    }

    #[test]
    fn a_long_turn_keeps_its_instruction_and_newest_groups() {
        let history = vec![
            ChatMessage::user("Fix the parser"),
            call("1", "read", "src/a.rs"),
            result("1", "fn a()"),
            call("2", "edit", "src/a.rs"),
            result("2", "ok"),
            call("3", "read", "src/b.rs"),
            result("3", "fn b()"),
            ChatMessage::assistant("Done."),
        ];
        let plan = plan(&history).unwrap();
        assert_eq!(plan.head, [ChatMessage::user("Fix the parser")]);
        assert_eq!(plan.middle.len(), 4, "the two oldest groups");
        assert_eq!(plan.tail.len(), 3, "a call with its result, then the reply");
        assert!(matches!(plan.tail[0], ChatMessage::Assistant { .. }));

        let rebuilt = rebuild(plan, "Edited src/a.rs; next look at b.");
        assert!(matches!(&rebuilt[1], ChatMessage::User { text } if text.starts_with(SUMMARY_MARKER)
            && text.contains("<read-files>\nsrc/a.rs\n</read-files>")
            && text.contains("<modified-files>\nsrc/a.rs\n</modified-files>")));
        assert_eq!(rebuilt.len(), 5);
    }

    #[test]
    fn later_turns_keep_the_last_instruction_and_fold_prior_summaries() {
        let history = vec![
            ChatMessage::user("Build the app"),
            ChatMessage::user(format!("{SUMMARY_MARKER}\nold summary\n</bubble_internal_context>")),
            ChatMessage::assistant("Built it."),
            ChatMessage::user("Now add tests"),
            call("9", "read", "src/lib.rs"),
            result("9", "…"),
        ];
        let plan = plan(&history).unwrap();
        assert_eq!(plan.tail[0], ChatMessage::user("Now add tests"));
        let text = serialize(&plan);
        assert!(text.contains("[Prior compaction summary]"));
        assert!(text.contains("ASSISTANT: Built it."));
    }

    #[test]
    fn a_short_history_has_nothing_to_compact() {
        assert!(plan(&[ChatMessage::user("hi"), ChatMessage::assistant("hello")]).is_none());
    }
}
