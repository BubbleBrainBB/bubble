//! Ordered, stable desktop transcript items. Both live events and saved
//! checkpoints use this model; display grouping never owns agent state.

use bubble_core::{ChatMessage, Decision};
use bubble_store::MessageRole;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolStatus {
    Queued,
    Running,
    NeedsInput,
    Done,
    Failed,
    Denied,
    Interrupted,
}

impl ToolStatus {
    pub fn unfinished(self) -> bool {
        matches!(self, Self::Queued | Self::Running | Self::NeedsInput)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TurnStatus {
    Queued,
    Working,
    NeedsInput,
    Completed,
    Stopped,
    Failed,
    Interrupted,
}

impl TurnStatus {
    pub fn live(self) -> bool {
        matches!(self, Self::Working | Self::NeedsInput)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Phase {
    #[default]
    Legacy,
    Streaming,
    Commentary,
    Final,
    Partial,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum LineKind {
    Message(MessageRole),
    Reasoning,
    Turn {
        status: TurnStatus,
        completed_at: Option<i64>,
    },
    Tool {
        name: String,
        status: ToolStatus,
        output: String,
    },
    Approval {
        summary: String,
        detail: String,
        rememberable: bool,
        #[serde(with = "decision")]
        decided: Option<Decision>,
    },
    RunStart(usize),
    Failure,
    Notice,
    /// The agent asks the person something and waits for the answer.
    Question {
        asks: Vec<Ask>,
        /// `None` while it waits. Then the labels chosen for each ask, in
        /// order; empty when the person skipped it or the turn ended first.
        answers: Option<Vec<Vec<String>>>,
    },
}

/// One thing the agent asks, with the choices it offers.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Ask {
    /// A word or two naming what is asked.
    pub header: String,
    pub question: String,
    /// Each choice: its label, and what choosing it means.
    pub options: Vec<(String, String)>,
    /// More than one choice may be picked.
    pub multiple: bool,
}

impl Ask {
    /// The asks in the agent's `question` request.
    pub fn from_request(request: &serde_json::Value) -> Vec<Ask> {
        let text = |value: &serde_json::Value, key: &str| value.get(key).and_then(|v| v.as_str()).unwrap_or_default().to_owned();
        request
            .get("questions")
            .and_then(|q| q.as_array())
            .map(|questions| {
                questions
                    .iter()
                    .map(|q| Ask {
                        header: text(q, "header"),
                        question: text(q, "question"),
                        options: q
                            .get("options")
                            .and_then(|o| o.as_array())
                            .map(|options| options.iter().map(|o| (text(o, "label"), text(o, "description"))).collect())
                            .unwrap_or_default(),
                        multiple: q.get("multiple").and_then(|m| m.as_bool()).unwrap_or(false),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Line {
    pub id: String,
    pub kind: LineKind,
    pub text: String,
    pub pending: bool,
    pub reasoning: String,
    pub at: i64,
    pub images: usize,
    #[serde(default)]
    pub turn_id: Option<String>,
    #[serde(default)]
    pub phase: Phase,
    #[serde(default)]
    pub call_id: Option<String>,
    #[serde(default)]
    pub child_ids: Vec<String>,
    /// The end of what a running command printed so far; never saved.
    #[serde(default, skip_serializing)]
    pub live: String,
    /// A file change as diff lines (`+`, `-` or ` ` first), for edits.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub diff: String,
}

/// How much of a running command's output a line keeps.
const LIVE_KEPT: usize = 8000;

/// Adds what a running command printed, keeping only its end.
pub fn live_output(lines: &mut [Line], turn: &str, call: &str, chunk: &str) {
    if let Some(line) = tool_mut(lines, turn, call) {
        line.live.push_str(chunk);
        if line.live.len() > LIVE_KEPT {
            let mut cut = line.live.len() - LIVE_KEPT;
            while !line.live.is_char_boundary(cut) {
                cut += 1;
            }
            line.live.drain(..cut);
        }
    }
}

impl Line {
    pub(crate) fn new(id: impl Into<String>, kind: LineKind, text: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            kind,
            text: text.into(),
            pending: false,
            reasoning: String::new(),
            at: bubble_store::now_ms(),
            images: 0,
            turn_id: None,
            phase: Phase::Legacy,
            call_id: None,
            child_ids: Vec::new(),
            live: String::new(),
            diff: String::new(),
        }
    }

    pub fn in_turn(mut self, turn: &str) -> Self {
        self.turn_id = Some(turn.to_owned());
        self
    }
}

// Decisions in a historical approval are display data, never executable
// grants. Refuse's static runtime reason need not cross this boundary.
mod decision {
    use super::*;
    pub fn serialize<S: serde::Serializer>(value: &Option<Decision>, s: S) -> Result<S::Ok, S::Error> {
        value
            .map(|d| match d {
                Decision::AllowOnce => "once",
                Decision::AllowForTask => "task",
                Decision::Refuse("Turn ended") => "interrupted",
                _ => "denied",
            })
            .serialize(s)
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Decision>, D::Error> {
        Ok(Option::<String>::deserialize(d)?.map(|s| match s.as_str() {
            "once" => Decision::AllowOnce,
            "task" => Decision::AllowForTask,
            "interrupted" => Decision::Refuse("Turn ended"),
            _ => Decision::Deny,
        }))
    }
}

pub fn begin_turn(lines: &mut Vec<Line>, turn: &str, at: i64, status: TurnStatus) {
    lines.push(Line {
        at,
        ..Line::new(
            turn,
            LineKind::Turn {
                status,
                completed_at: None,
            },
            "",
        )
        .in_turn(turn)
    });
}

pub fn set_turn_status(lines: &mut [Line], turn: &str, status: TurnStatus) {
    if let Some(line) = lines.iter_mut().find(|l| l.id == turn) {
        if let LineKind::Turn { status: current, .. } = &mut line.kind {
            *current = status;
        }
    }
}

pub fn begin_model(lines: &mut Vec<Line>, turn: &str, step: usize) -> String {
    let id = format!("{turn}:model-{step}");
    lines.push(Line {
        pending: true,
        ..Line::new(format!("{id}:reasoning"), LineKind::Reasoning, "").in_turn(turn)
    });
    id
}

pub fn text(lines: &mut Vec<Line>, turn: &str, id: &str, piece: &str) {
    if let Some(thought) = lines.iter_mut().find(|l| l.id == format!("{id}:reasoning")) {
        thought.pending = false;
    }
    if !lines.iter().any(|l| l.id == id) {
        lines.push(Line {
            pending: true,
            phase: Phase::Streaming,
            ..Line::new(id, LineKind::Message(MessageRole::Assistant), "").in_turn(turn)
        });
    }
    if let Some(line) = lines.iter_mut().find(|l| l.id == id) {
        line.text.push_str(piece);
    }
}

pub fn reasoning(lines: &mut [Line], id: &str, piece: &str) {
    if let Some(line) = lines.iter_mut().find(|l| l.id == format!("{id}:reasoning")) {
        line.reasoning.push_str(piece);
    }
}

pub fn assistant(lines: &mut Vec<Line>, turn: &str, id: &str, message: &ChatMessage) {
    let ChatMessage::Assistant {
        text: content,
        tool_calls,
        reasoning,
        ..
    } = message
    else {
        return;
    };
    let thought_id = format!("{id}:reasoning");
    if let Some(thought) = lines.iter_mut().find(|l| l.id == thought_id) {
        thought.reasoning = reasoning.clone();
        thought.pending = false;
    }
    if reasoning.trim().is_empty() {
        lines.retain(|l| l.id != thought_id);
    }
    if !content.is_empty() || lines.iter().any(|l| l.id == id) {
        if !lines.iter().any(|l| l.id == id) {
            text(lines, turn, id, "");
        }
        let line = lines.iter_mut().find(|l| l.id == id).unwrap();
        line.text = content.clone();
        line.pending = false;
        line.phase = if tool_calls.is_empty() {
            Phase::Final
        } else {
            Phase::Commentary
        };
    }
    for (index, call) in tool_calls.iter().enumerate() {
        lines.push(Line {
            call_id: Some(call.id.clone()),
            diff: change_of(call),
            ..Line::new(
                format!("{id}:tool-{index}"),
                LineKind::Tool {
                    name: call.name.clone(),
                    status: ToolStatus::Queued,
                    output: String::new(),
                },
                bubble_core::tools::describe(call),
            )
            .in_turn(turn)
        });
    }
}

/// How many diff lines an edit keeps.
const DIFF_KEPT: usize = 400;

/// What an edit or a write changes, as diff lines: the replaced text
/// against its replacement, with up to three unchanged lines around.
pub fn change_of(call: &bubble_core::ToolCall) -> String {
    let args = call.arguments_json();
    let text = |name: &str| args[name].as_str().unwrap_or_default().to_owned();
    let mut out: Vec<String> = Vec::new();
    match call.name.as_str() {
        "write" => out.extend(text("content").lines().map(|l| format!("+{l}"))),
        "edit" => {
            let (old, new) = (text("old_string"), text("new_string"));
            let (old, new): (Vec<&str>, Vec<&str>) = (old.lines().collect(), new.lines().collect());
            let head = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
            let tail = old[head..].iter().rev().zip(new[head..].iter().rev()).take_while(|(a, b)| a == b).count();
            out.extend(old[head.saturating_sub(3)..head].iter().map(|l| format!(" {l}")));
            out.extend(old[head..old.len() - tail].iter().map(|l| format!("-{l}")));
            out.extend(new[head..new.len() - tail].iter().map(|l| format!("+{l}")));
            out.extend(old[old.len() - tail..].iter().take(3).map(|l| format!(" {l}")));
        }
        _ => return String::new(),
    }
    out.truncate(DIFF_KEPT);
    out.join("\n")
}

pub fn tool_mut<'a>(lines: &'a mut [Line], turn: &str, call: &str) -> Option<&'a mut Line> {
    lines
        .iter_mut()
        .rev()
        .find(|l| l.turn_id.as_deref() == Some(turn) && l.call_id.as_deref() == Some(call))
}

/// Terminal cleanup is scoped to this turn, including on errors and stop.
pub fn finish(lines: &mut [Line], turn: &str, status: TurnStatus, at: i64) {
    for line in lines.iter_mut().filter(|l| l.turn_id.as_deref() == Some(turn)) {
        if line.pending && line.phase == Phase::Streaming {
            line.phase = Phase::Partial;
        }
        line.pending = false;
        match &mut line.kind {
            LineKind::Turn {
                status: current,
                completed_at,
            } => {
                *current = status;
                *completed_at = Some(at);
            }
            LineKind::Tool { status, output, .. } if status.unfinished() => {
                *status = ToolStatus::Interrupted;
                *output = "Interrupted before it finished.".into();
            }
            LineKind::Approval { decided, .. } if decided.is_none() => *decided = Some(Decision::Refuse("Turn ended")),
            // Nobody can answer a turn that is over.
            LineKind::Question { answers, .. } if answers.is_none() => *answers = Some(Vec::new()),
            _ => {}
        }
    }
}

/// A crash has no known end time. Keep the last checkpoint, mark it
/// interrupted, and never invent successful completion or a duration.
pub fn recover(lines: &mut [Line]) {
    let interrupted: Vec<_> = lines
        .iter()
        .filter_map(|l| match l.kind {
            LineKind::Turn { status, .. } if status.live() => Some(l.id.clone()),
            _ => None,
        })
        .collect();
    for turn in interrupted {
        finish(lines, &turn, TurnStatus::Interrupted, 0);
        if let Some(line) = lines.iter_mut().find(|l| l.id == turn) {
            if let LineKind::Turn { completed_at, .. } = &mut line.kind {
                *completed_at = None;
            }
        }
    }
}

/// Applies checkpoints at their prompt anchors. Old/CLI history keeps its
/// message projection. Corrupt or future checkpoints never erase history.
pub fn overlay(lines: &mut Vec<Line>, checkpoints: Vec<(String, String)>) {
    for (anchor, content) in checkpoints {
        let Ok(mut saved) = serde_json::from_str::<Vec<Line>>(&content) else {
            continue;
        };
        let Some(start) = lines.iter().position(|l| l.id == anchor) else {
            continue;
        };
        let turn = format!("turn-{anchor}");
        let unique: std::collections::HashSet<_> = saved.iter().map(|l| &l.id).collect();
        if unique.len() != saved.len()
            || !matches!(saved.first(), Some(l) if l.id == turn && matches!(l.kind, LineKind::Turn { .. }))
            || saved.iter().any(|l| l.turn_id.as_deref() != Some(&turn))
        {
            continue;
        }
        recover(&mut saved);
        let end = lines[start + 1..]
            .iter()
            .position(|l| {
                matches!(
                    l.kind,
                    LineKind::Message(MessageRole::User | MessageRole::System | MessageRole::External)
                        | LineKind::RunStart(_)
                )
            })
            .map_or(lines.len(), |n| start + 1 + n);
        lines.splice(start + 1..end, saved);
    }
}

/// Child state is kept by the runtime; its presentation stays at its
/// dispatch anchor, across later turns and repeated waits.
pub fn anchor_children(lines: &mut [Line], children: &[bubble_core::delegate::Snapshot]) {
    for child in children {
        if lines.iter().any(|l| l.child_ids.contains(&child.id)) {
            continue;
        }
        let Some(call_id) = &child.parent_call_id else { continue };
        if let Some(spawn) = lines.iter_mut().rev().find(|l| {
            l.call_id.as_ref() == Some(call_id)
                && matches!(&l.kind, LineKind::Tool { name, .. } if name == "spawn_agent" || name == "run_workflow")
        }) {
            spawn.child_ids.push(child.id.clone());
        }
    }
}

/// Restored children can be linked using their recorded spawn output.
/// Unlinked legacy children stay in the legacy list, never migrate to a
/// new dispatch simply because another agent was started later.
pub fn restore_child_anchors(lines: &mut [Line], children: &[bubble_core::delegate::Snapshot]) {
    for child in children {
        if lines.iter().any(|l| l.child_ids.contains(&child.id)) {
            continue;
        }
        if let Some(line) = lines.iter_mut().find(|l| {
            matches!(&l.kind,
                LineKind::Tool { name, output, .. } if (name == "spawn_agent" || name == "run_workflow")
                    && output.lines().any(|s| s.trim() == format!("agent_id: {}", child.id))
            )
        }) {
            line.child_ids.push(child.id.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bubble_core::ToolCall;

    fn response(text: &str, calls: &[&str]) -> ChatMessage {
        ChatMessage::Assistant {
            text: text.into(),
            reasoning: "**Inspecting**".into(),
            state: serde_json::Value::Null,
            tool_calls: calls
                .iter()
                .map(|id| ToolCall {
                    id: (*id).into(),
                    name: "read".into(),
                    arguments: "{\"path\":\"a.rs\"}".into(),
                })
                .collect(),
        }
    }

    #[test]
    fn edits_record_what_changed_with_a_little_context() {
        let call = |name: &str, args: serde_json::Value| ToolCall { id: "x".into(), name: name.into(), arguments: args.to_string() };
        let edit = call("edit", serde_json::json!({"path":"a","old_string":"a\nb\nc\nd\ne","new_string":"a\nb\nC\nd\ne"}));
        assert_eq!(change_of(&edit), " a\n b\n-c\n+C\n d\n e");
        assert_eq!(change_of(&call("write", serde_json::json!({"path":"n","content":"x\ny"}))), "+x\n+y");
        assert_eq!(change_of(&call("read", serde_json::json!({"path":"n"}))), "");
    }

    #[test]
    fn tools_start_in_order_and_reused_provider_ids_do_not_rename_history() {
        let mut lines = Vec::new();
        begin_turn(&mut lines, "t", 10, TurnStatus::Working);
        let id = begin_model(&mut lines, "t", 1);
        text(&mut lines, "t", &id, "Looking");
        let stable = lines.last().unwrap().id.clone();
        assistant(&mut lines, "t", &id, &response("Looking", &["a", "b"]));
        assert_eq!(lines.iter().find(|l| l.id == stable).unwrap().phase, Phase::Commentary);
        if let LineKind::Tool { status, .. } = &mut tool_mut(&mut lines, "t", "a").unwrap().kind {
            *status = ToolStatus::Running;
        }
        assert!(matches!(
            tool_mut(&mut lines, "t", "b").unwrap().kind,
            LineKind::Tool {
                status: ToolStatus::Queued,
                ..
            }
        ));
        let old = tool_mut(&mut lines, "t", "a").unwrap().id.clone();
        let next = begin_model(&mut lines, "t", 2);
        assistant(&mut lines, "t", &next, &response("Next", &["a"]));
        assert_ne!(tool_mut(&mut lines, "t", "a").unwrap().id, old);
        assert!(lines.iter().any(|l| l.id == old));
    }

    #[test]
    fn partial_text_reasoning_stop_and_times_survive_a_checkpoint() {
        let mut lines = Vec::new();
        begin_turn(&mut lines, "t", 10, TurnStatus::Working);
        let id = begin_model(&mut lines, "t", 1);
        reasoning(&mut lines, &id, "**Inspecting**");
        text(&mut lines, "t", &id, "Partial");
        finish(&mut lines, "t", TurnStatus::Stopped, 90);
        let mut saved: Vec<Line> = serde_json::from_str(&serde_json::to_string(&lines).unwrap()).unwrap();
        recover(&mut saved);
        assert_eq!(saved, lines);
        assert!(
            saved
                .iter()
                .any(|l| l.text == "Partial" && l.phase == Phase::Partial && !l.pending)
        );
        assert!(saved.iter().any(|l| l.reasoning == "**Inspecting**"));
    }

    #[test]
    fn crash_recovery_never_turns_partial_work_into_a_final_answer() {
        let mut lines = Vec::new();
        begin_turn(&mut lines, "t", 10, TurnStatus::Working);
        let id = begin_model(&mut lines, "t", 1);
        text(&mut lines, "t", &id, "Partial");
        recover(&mut lines);
        assert!(matches!(
            lines[0].kind,
            LineKind::Turn {
                status: TurnStatus::Interrupted,
                completed_at: None
            }
        ));
        assert!(lines.iter().any(|l| l.phase == Phase::Partial));
        assert!(!lines.iter().any(|l| l.pending));
    }

    #[test]
    fn children_bind_to_their_dispatch_even_before_the_queued_tool_starts() {
        let child = |id: &str, call: &str| {
            serde_json::from_value::<bubble_core::delegate::Snapshot>(serde_json::json!({
                "id":id,"parent_call_id":call,"nickname":id,"profile":"explorer","task":"inspect",
                "description":null,"status":"queued","model":"mock:m","summary":"","notes":[],
                "error":null,"workflow":false,"pending_inputs":0
            }))
            .unwrap()
        };
        let mut lines = vec![
            Line {
                call_id: Some("first".into()),
                ..Line::new(
                    "a",
                    LineKind::Tool {
                        name: "spawn_agent".into(),
                        status: ToolStatus::Queued,
                        output: String::new(),
                    },
                    "",
                )
            },
            Line {
                call_id: Some("second".into()),
                ..Line::new(
                    "b",
                    LineKind::Tool {
                        name: "spawn_agent".into(),
                        status: ToolStatus::Queued,
                        output: String::new(),
                    },
                    "",
                )
            },
        ];
        let children = [child("Ada", "first"), child("Bo", "second")];
        anchor_children(&mut lines, &children);
        anchor_children(&mut lines, &children); // repeated notifications do not duplicate rows
        assert_eq!(lines[0].child_ids, ["Ada"]);
        assert_eq!(lines[1].child_ids, ["Bo"]);
    }

    #[test]
    fn a_bad_checkpoint_keeps_the_underlying_history() {
        let mut lines = vec![
            Line::new("u", LineKind::Message(MessageRole::User), "hello"),
            Line::new("reply", LineKind::Message(MessageRole::Assistant), "kept"),
        ];
        let before = lines.clone();
        overlay(&mut lines, vec![("u".into(), "broken JSON".into())]);
        overlay(&mut lines, vec![("u".into(), "[]".into())]);
        assert_eq!(lines, before);
    }

    #[test]
    fn stopping_a_question_preserves_interruption_instead_of_claiming_the_person_denied_it() {
        let mut lines = Vec::new();
        begin_turn(&mut lines, "t", 10, TurnStatus::NeedsInput);
        lines.push(
            Line::new(
                "approval",
                LineKind::Approval {
                    summary: "Run a command".into(),
                    detail: "sleep 20".into(),
                    rememberable: true,
                    decided: None,
                },
                "",
            )
            .in_turn("t"),
        );
        finish(&mut lines, "t", TurnStatus::Stopped, 90);
        let restored: Vec<Line> = serde_json::from_str(&serde_json::to_string(&lines).unwrap()).unwrap();
        assert_eq!(restored, lines);
        assert!(matches!(
            restored[1].kind,
            LineKind::Approval {
                decided: Some(Decision::Refuse("Turn ended")),
                ..
            }
        ));
    }
}
