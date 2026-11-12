//! How a conversation's working trace is laid out, after the Aegis
//! (coworker) agent trace: consecutive tool calls make one group of steps
//! ("Explored 3 files, ran 2 commands, and edited a file"); reasoning is a
//! step of its own; while a turn runs, its finished work folds into one
//! summary row with a status line at the end saying what it does now;
//! once its final reply is in, the work folds under "Worked for 1m 20s".
//! Pure, so it is tested without a screen.

use bubble_store::MessageRole;

use crate::state::{Line, LineKind, ToolStatus, TurnStatus};
use crate::transcript::Phase;

/// One thing the transcript shows, in order.
#[derive(Clone, Debug, PartialEq)]
pub enum Block {
    /// A line as it is: a message, a card, a lone tool call.
    Line { id: String, folded_by: Option<String> },
    /// Tool calls (and the reasoning between them) shown as one row.
    Group { id: String, members: Vec<String>, active: bool, folded_by: Option<String> },
    /// "Worked for …": opens the work folded before a turn's reply.
    Worked { id: String, millis: i64 },
    /// The same header lives from queueing through terminal completion:
    /// while the turn runs it opens the work folded under `live:<id>`
    /// (when there is any), once done the work folded under `<id>`.
    Turn { id: String, status: TurnStatus, started_at: i64, completed_at: Option<i64>, collapsible: bool, live_fold: bool },
    /// What a running turn does now, at the end of its trace.
    Status { id: String, turn: String, status: TurnStatus, started_at: i64 },
}

impl Block {
    pub fn id(&self) -> &str {
        match self {
            Block::Line { id, .. } | Block::Group { id, .. } | Block::Worked { id, .. } | Block::Turn { id, .. } | Block::Status { id, .. } => id,
        }
    }

    pub fn folded_by(&self) -> Option<&str> {
        match self {
            Block::Line { folded_by, .. } | Block::Group { folded_by, .. } => folded_by.as_deref(),
            Block::Worked { .. } | Block::Turn { .. } | Block::Status { .. } => None,
        }
    }
}

/// What a tool call did, for the group's summary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Explore,
    Command,
    Edit,
    Web,
    Other,
}

/// Read-only commands count as looking around, as in Codex.
const EXPLORING_COMMANDS: &[&str] =
    &["cat", "ls", "rg", "grep", "find", "head", "tail", "wc", "tree", "pwd", "fd", "stat", "file", "less", "nl"];

pub fn kind_of(name: &str, text: &str) -> Kind {
    match name {
        "read" | "ls" | "grep" | "glob" | "memory" | "skill_search" | "skill" => Kind::Explore,
        "edit" | "write" => Kind::Edit,
        "web_search" | "web_fetch" => Kind::Web,
        "bash" => {
            let command = text.strip_prefix("Ran ").unwrap_or(text).trim_start();
            let first = command.split_whitespace().next().unwrap_or("");
            let git_look = command.starts_with("git status") || command.starts_with("git log") || command.starts_with("git diff") || command.starts_with("git show");
            let sed_print = first == "sed" && command.contains(" -n");
            if EXPLORING_COMMANDS.contains(&first) || git_look || sed_print { Kind::Explore } else { Kind::Command }
        }
        _ => Kind::Other,
    }
}

/// Task cards stand on their own; every other tool call can be grouped.
fn groupable_tool(line: &Line) -> bool {
    matches!(&line.kind, LineKind::Tool { name, .. } if !matches!(name.as_str(), "create_bot" | "message_bot" | "create_task" | "continue_task" | "spawn_agent" | "run_workflow"))
}

/// The model's reasoning (or a reply still starting) with no text: part
/// of the work, never a row by itself.
fn is_thought(line: &Line) -> bool {
    line.kind == LineKind::Message(MessageRole::Assistant) && line.text.trim().is_empty()
}

fn starts_turn(line: &Line) -> bool {
    matches!(line.kind, LineKind::Message(MessageRole::User | MessageRole::System | MessageRole::External))
}

/// Lays the lines out; `busy` while the last turn is still running.
pub fn layout(lines: &[Line], busy: bool) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut opener: Option<usize> = None;
    let mut body_start = 0;
    for index in 0..=lines.len() {
        let boundary = index == lines.len() || starts_turn(&lines[index]) || matches!(lines[index].kind, LineKind::RunStart(_));
        if !boundary {
            continue;
        }
        if body_start < index {
            let running = busy && index == lines.len();
            turn(&lines[body_start..index], opener.map(|o| &lines[o]), running, &mut blocks);
        }
        if index < lines.len() {
            blocks.push(Block::Line { id: lines[index].id.clone(), folded_by: None });
            opener = starts_turn(&lines[index]).then_some(index);
            body_start = index + 1;
        }
    }
    blocks
}

fn turn(body: &[Line], opener: Option<&Line>, running: bool, blocks: &mut Vec<Block>) {
    if let Some(header) = body.first().filter(|l| matches!(l.kind, LineKind::Turn { .. })) {
        explicit_turn(header, &body[1..], blocks);
        return;
    }
    // The reply that ends the turn: the last one with text, once the turn
    // is over and nothing but notes came after it.
    let reply = body.iter().rposition(|l| l.kind == LineKind::Message(MessageRole::Assistant) && !l.text.trim().is_empty());
    // Only a reply that ends the turn: no more work, no failure after it
    // (Codex leaves stopped and failed turns open too).
    let ends_turn = |r: usize| body[r + 1..].iter().all(|l| is_thought(l) && !l.pending && l.reasoning.trim().is_empty());
    let fold_until = reply.filter(|&r| !running && ends_turn(r) && body[..r].iter().any(|l| !is_thought(l)));
    let worked = fold_until.map(|r| {
        let id = format!("worked-{}", opener.map_or(&body[0].id, |o| &o.id));
        let start = opener.map_or(body[0].at, |o| o.at);
        let end = body[..=r].iter().map(|l| l.at).max().unwrap_or(start);
        blocks.push(Block::Worked { id: id.clone(), millis: (end - start).max(0) });
        id
    });
    let mut group: Vec<&Line> = Vec::new();
    let flush = |group: &mut Vec<&Line>, blocks: &mut Vec<Block>, folded_by: Option<String>, active: bool| {
        if group.is_empty() {
            return;
        }
        let tools = group.iter().filter(|l| groupable_tool(l)).count();
        if tools == 0 {
            // Only reasoning: a reply still starting shows as "Thinking…".
            if let Some(pending) = group.iter().rev().find(|l| l.pending) {
                blocks.push(Block::Line { id: pending.id.clone(), folded_by });
            }
        } else if tools == 1 && !active {
            let tool = group.iter().find(|l| groupable_tool(l)).unwrap();
            blocks.push(Block::Line { id: tool.id.clone(), folded_by });
        } else {
            blocks.push(Block::Group {
                id: format!("group-{}", group[0].id),
                members: group.iter().map(|l| l.id.clone()).collect(),
                active,
                folded_by,
            });
        }
        group.clear();
    };
    for (index, line) in body.iter().enumerate() {
        // Task cards are results the person acts on: never folded away.
        let card = matches!(&line.kind, LineKind::Tool { .. }) && !groupable_tool(line);
        let folded_by = match fold_until {
            Some(r) if index < r && !card => worked.clone(),
            _ => None,
        };
        if groupable_tool(line) || is_thought(line) {
            group.push(line);
            continue;
        }
        let group_fold = match fold_until {
            Some(r) if index <= r => worked.clone(),
            _ => None,
        };
        flush(&mut group, blocks, group_fold, false);
        blocks.push(Block::Line { id: line.id.clone(), folded_by });
    }
    flush(&mut group, blocks, None, running);
}

/// The key a running turn folds its finished work under.
pub fn live_key(turn: &str) -> String {
    format!("live:{turn}")
}

/// A running turn: queued, working or waiting on the person.
pub fn running(status: TurnStatus) -> bool {
    status.live() || status == TurnStatus::Queued
}

/// Task cards and the agents a turn started: results the person acts on.
fn is_card(line: &Line) -> bool {
    matches!(&line.kind, LineKind::Tool { .. }) && !groupable_tool(line)
}

/// New turns use recorded semantics rather than guessing from the last
/// nonempty message. Group identities depend only on their first tool;
/// reasoning and a streaming message's first token cannot rename them.
fn explicit_turn(header: &Line, body: &[Line], blocks: &mut Vec<Block>) {
    let LineKind::Turn { status, completed_at } = header.kind else { return };
    let final_index = body.iter().position(|l| l.phase == Phase::Final && !l.text.trim().is_empty());
    let fold_until = final_index.filter(|&r| status == TurnStatus::Completed
        && body[..r].iter().any(|l| matches!(l.kind, LineKind::Tool { .. }) || !l.text.trim().is_empty() || !l.reasoning.trim().is_empty()));
    let live = running(status);
    // While running, only the latest message, what waits on the person and
    // cards stay out; the rest folds into the summary row.
    let latest = body.iter().rposition(|l| matches!(l.kind, LineKind::Message(_)) && !l.text.trim().is_empty());
    let stays_out = |index: usize, line: &Line| {
        Some(index) == latest
            || is_card(line)
            || matches!(line.kind, LineKind::Approval { decided: None, .. } | LineKind::Failure | LineKind::Notice | LineKind::RunStart(_))
    };
    let shows = |line: &Line| match &line.kind {
        LineKind::Tool { .. } | LineKind::Approval { .. } => true,
        LineKind::Reasoning => !line.reasoning.trim().is_empty(),
        LineKind::Message(_) => !line.text.trim().is_empty(),
        _ => false,
    };
    let live_fold = live && body.iter().enumerate().any(|(i, l)| shows(l) && !stays_out(i, l));
    let live_folded = live_key(&header.id);
    let fold = |index: usize, line: &Line| -> Option<String> {
        if live {
            return (!stays_out(index, line)).then(|| live_folded.clone());
        }
        fold_until.filter(|&r| index < r && !is_card(line)).map(|_| header.id.clone())
    };
    blocks.push(Block::Turn {
        id: header.id.clone(),
        status,
        started_at: header.at,
        completed_at,
        collapsible: fold_until.is_some(),
        live_fold,
    });
    let mut group = Vec::<&Line>::new();
    let flush = |group: &mut Vec<&Line>, blocks: &mut Vec<Block>| {
        if let Some(first) = group.first() {
            let folded_by = if live { Some(live_folded.clone()) } else { fold_until.map(|_| header.id.clone()) };
            let active = live && group.iter().any(|l| matches!(l.kind, LineKind::Tool { status: ToolStatus::Running | ToolStatus::NeedsInput | ToolStatus::Queued, .. }));
            blocks.push(Block::Group { id: format!("group-{}", first.id),
                members: group.iter().map(|l| l.id.clone()).collect(), active, folded_by });
        }
        group.clear();
    };
    for (index, line) in body.iter().enumerate() {
        if groupable_tool(line) {
            group.push(line);
            continue;
        }
        flush(&mut group, blocks);
        // Reasoning is a step of its own; an empty one shows nothing.
        if line.kind == LineKind::Reasoning && line.reasoning.trim().is_empty() {
            continue;
        }
        blocks.push(Block::Line { id: line.id.clone(), folded_by: fold(index, line) });
    }
    flush(&mut group, blocks);
    if live {
        blocks.push(Block::Status { id: format!("status:{}", header.id), turn: header.id.clone(), status, started_at: header.at });
    }
}

/// Only user scrolling changes follow intent. Content growth alone must
/// not make a reader who is following the bottom appear to scroll away.
pub fn near_bottom(offset: f32, viewport: f32, content: f32) -> bool {
    content - viewport - offset <= 48.0
}

/// "45s", "1m 20s", "2h 3m 5s"; "0s" under a second.
pub fn duration(millis: i64) -> String {
    let seconds = millis.max(0) / 1000;
    let (h, m, s) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    let mut parts = Vec::new();
    if h > 0 {
        parts.push(format!("{h}h"));
    }
    if m > 0 {
        parts.push(format!("{m}m"));
    }
    if s > 0 || parts.is_empty() {
        parts.push(format!("{s}s"));
    }
    parts.join(" ")
}

/// One step a group shows: a reasoning, a tool call, or several calls
/// that looked around (or searched the web) in a row.
#[derive(Clone, Debug, PartialEq)]
pub struct Stage {
    pub kind: StageKind,
    pub members: Vec<Line>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StageKind {
    Reasoning,
    Tool(Kind),
}

impl Stage {
    pub fn id(&self) -> String {
        format!("stage-{}", self.members[0].id)
    }

    fn statuses(&self) -> impl Iterator<Item = ToolStatus> + '_ {
        self.members.iter().filter_map(|l| match l.kind {
            LineKind::Tool { status, .. } => Some(status),
            _ => None,
        })
    }

    pub fn running(&self) -> bool {
        self.statuses().any(|s| matches!(s, ToolStatus::Running)) || (self.kind == StageKind::Reasoning && self.members.iter().any(|l| l.pending))
    }

    pub fn waiting(&self) -> bool {
        self.statuses().any(|s| matches!(s, ToolStatus::NeedsInput))
    }

    pub fn queued(&self) -> bool {
        self.statuses().any(|s| matches!(s, ToolStatus::Queued))
    }

    pub fn interrupted(&self) -> bool {
        self.statuses().any(|s| matches!(s, ToolStatus::Interrupted))
    }

    pub fn failures(&self) -> usize {
        self.statuses().filter(|s| matches!(s, ToolStatus::Failed)).count()
    }
}

/// A group's members as steps, in order: looking around and web searches
/// in a row merge; every command, edit and other call is its own.
pub fn stages(members: &[Line]) -> Vec<Stage> {
    let mut stages: Vec<Stage> = Vec::new();
    for line in members {
        let kind = match &line.kind {
            LineKind::Tool { name, .. } => StageKind::Tool(kind_of(name, &line.text)),
            _ if !line.reasoning.trim().is_empty() => StageKind::Reasoning,
            _ => continue,
        };
        let merges = matches!(kind, StageKind::Tool(Kind::Explore | Kind::Web));
        match stages.last_mut() {
            Some(last) if merges && last.kind == kind => last.members.push(line.clone()),
            _ => stages.push(Stage { kind, members: vec![line.clone()] }),
        }
    }
    stages
}

fn plural(count: usize, one: &str, many: &str) -> String {
    format!("{count} {}", if count == 1 { one } else { many })
}

/// What a run of looking around covered: "3 files, 2 searches".
fn explored(members: &[Line]) -> String {
    let mut files = std::collections::BTreeSet::new();
    let (mut searches, mut lists) = (0, 0);
    for line in members {
        let LineKind::Tool { name, .. } = &line.kind else { continue };
        let command = line.text.strip_prefix("Ran ").unwrap_or(&line.text).trim_start();
        let first = command.split_whitespace().next().unwrap_or("");
        match name.as_str() {
            "read" => { files.insert(line.text.clone()); }
            "grep" | "glob" | "skill_search" => searches += 1,
            "bash" if matches!(first, "cat" | "head" | "tail" | "less" | "nl" | "sed") => { files.insert(line.text.clone()); }
            "bash" if matches!(first, "rg" | "grep" | "find" | "fd") => searches += 1,
            _ => lists += 1,
        }
    }
    let parts: Vec<String> = [
        (files.len(), "file", "files"),
        (searches, "search", "searches"),
        (lists, "list", "lists"),
    ]
    .into_iter()
    .filter(|(n, ..)| *n > 0)
    .map(|(n, one, many)| plural(n, one, many))
    .collect();
    parts.join(", ")
}

/// A tool call's own line, by its state: "Running cargo test".
pub fn tool_title(line: &Line, project: Option<&str>) -> String {
    let label = shorten(&line.text, project);
    match line.kind {
        LineKind::Tool { status: ToolStatus::Queued, .. } => format!("Queued: {label}"),
        LineKind::Tool { status: ToolStatus::Running, .. } => in_progress(&label),
        LineKind::Tool { status: ToolStatus::NeedsInput, .. } => format!("Waiting for approval: {label}"),
        _ => label,
    }
}

/// A step's line: the call's own for a lone call, else what the run did.
pub fn stage_title(stage: &Stage, project: Option<&str>) -> String {
    match stage.kind {
        StageKind::Reasoning => if stage.running() { "Thinking".into() } else { "Reasoning".into() },
        _ if stage.members.len() == 1 => tool_title(&stage.members[0], project),
        StageKind::Tool(Kind::Web) => {
            if stage.running() { "Searching the web".into() } else { format!("Searched the web {}", plural(stage.members.len(), "time", "times")) }
        }
        _ => {
            if let Some(current) = stage.members.iter().rev().find(|l| matches!(l.kind, LineKind::Tool { status: ToolStatus::Running | ToolStatus::NeedsInput | ToolStatus::Queued, .. })) {
                return tool_title(current, project);
            }
            let verb = if stage.interrupted() { "Stopped exploring" } else { "Explored" };
            format!("{verb} {}", explored(&stage.members))
        }
    }
}

/// What a group's steps did, as one line: "Explored 3 files, ran 2
/// commands, and edited a file".
pub fn summary(stages: &[Stage]) -> String {
    if let Some(waiting) = stages.iter().find(|s| s.waiting()) {
        return stage_title(waiting, None);
    }
    let mut kinds: Vec<Kind> = Vec::new();
    for stage in stages {
        if let StageKind::Tool(kind) = stage.kind {
            if !kinds.contains(&kind) {
                kinds.push(kind);
            }
        }
    }
    let of = |kind: Kind| stages.iter().filter(move |s| s.kind == StageKind::Tool(kind)).flat_map(|s| s.members.iter());
    let parts: Vec<String> = kinds
        .into_iter()
        .map(|kind| match kind {
            Kind::Edit => {
                let files: std::collections::BTreeSet<&str> = of(kind).map(|l| l.text.as_str()).collect();
                format!("edited {}", plural(files.len(), "file", "files"))
            }
            Kind::Command => format!("ran {}", plural(of(kind).count(), "command", "commands")),
            Kind::Explore => format!("explored {}", explored(&of(kind).cloned().collect::<Vec<_>>())),
            Kind::Web => "searched the web".into(),
            Kind::Other => format!("called {}", plural(of(kind).count(), "tool", "tools")),
        })
        .collect();
    let joined = match parts.len() {
        0 if stages.iter().any(|s| s.kind == StageKind::Reasoning) => return "Reasoning".into(),
        0 => return "Completed work".into(),
        1 => parts[0].clone(),
        2 => format!("{} and {}", parts[0], parts[1]),
        n => format!("{}, and {}", parts[..n - 1].join(", "), parts[n - 1]),
    };
    let mut chars = joined.chars();
    chars.next().map(|c| c.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

/// The call a running turn waits on now, if any.
pub fn running_step(lines: &[Line], turn: &str) -> Option<Line> {
    lines
        .iter()
        .rev()
        .find(|l| l.turn_id.as_deref() == Some(turn) && matches!(l.kind, LineKind::Tool { status: ToolStatus::Running, .. }))
        .cloned()
}

/// The last `count` lines of a command's output as a terminal leaves
/// them: a line a carriage return rewrote (a progress bar) keeps its last
/// frame.
/// A command's output as a terminal would show it. The agent labels the
/// streams for the model ("stdout:" / "stderr:"); when only standard output
/// came, the label says nothing a person needs.
pub fn command_output(text: &str) -> &str {
    match text.strip_prefix("stdout:\n") {
        Some(rest) if !rest.contains("\nstderr:\n") => rest,
        _ => text,
    }
}

pub fn output_tail(text: &str, count: usize) -> String {
    let lines: Vec<&str> = text
        .split('\n')
        .map(|l| l.trim_end_matches('\r').rsplit('\r').next().unwrap_or_default())
        .filter(|l| !l.trim().is_empty())
        .collect();
    lines[lines.len().saturating_sub(count)..].join("\n")
}

/// Lines a diff adds and removes.
pub fn diff_stat(diff: &str) -> (usize, usize) {
    let count = |mark: char| diff.lines().filter(|l| l.starts_with(mark)).count();
    (count('+'), count('-'))
}

/// A running step in the present tense: "Ran ls" → "Running ls".
pub fn in_progress(text: &str) -> String {
    const VERBS: &[(&str, &str)] = &[
        ("Ran ", "Running "),
        ("Read ", "Reading "),
        ("Edited ", "Editing "),
        ("Wrote ", "Writing "),
        ("Searched ", "Searching "),
        ("Listed ", "Listing "),
        ("Used ", "Using "),
    ];
    for (done, doing) in VERBS {
        if let Some(rest) = text.strip_prefix(done) {
            return format!("{doing}{rest}");
        }
    }
    text.to_owned()
}

/// What the model is thinking about: the last heading or bold line of
/// its reasoning, else "Thinking".
pub fn thinking_label(reasoning: &str) -> String {
    reasoning
        .lines()
        .rev()
        .map(str::trim)
        .find_map(|line| {
            let heading = line.trim_start_matches('#').trim();
            if line.starts_with('#') && !heading.is_empty() {
                return Some(heading.to_owned());
            }
            let bold = line.strip_prefix("**")?.strip_suffix("**")?.trim();
            (!bold.is_empty()).then(|| bold.to_owned())
        })
        .unwrap_or_else(|| "Thinking".into())
}

/// A path as the person reads it: inside the project relative to it,
/// elsewhere under the home folder with `~`.
pub fn shorten(text: &str, project: Option<&str>) -> String {
    let mut out = text.to_owned();
    if let Some(project) = project.map(|p| p.trim_end_matches('/')).filter(|p| !p.is_empty()) {
        out = out.replace(&format!("{project}/"), "").replace(project, ".");
    }
    if let Some(home) = std::env::var_os("HOME").and_then(|h| h.into_string().ok()).filter(|h| h.len() > 1) {
        out = out.replace(&format!("{}/", home.trim_end_matches('/')), "~/");
    }
    out
}

/// What a running turn does now, and whether it waits on the person.
pub fn activity(lines: &[Line], turn: &str, status: TurnStatus, project: Option<&str>) -> (String, bool) {
    if status == TurnStatus::Queued {
        return ("Queued until another bot finishes".into(), false);
    }
    let mine: Vec<&Line> = lines.iter().filter(|l| l.turn_id.as_deref() == Some(turn) && l.id != turn).collect();
    let waiting = status == TurnStatus::NeedsInput
        || mine.iter().any(|l| matches!(l.kind, LineKind::Approval { decided: None, .. } | LineKind::Tool { status: ToolStatus::NeedsInput, .. }));
    if waiting {
        return ("Waiting for your approval".into(), true);
    }
    let running: Vec<&&Line> = mine.iter().filter(|l| matches!(l.kind, LineKind::Tool { status: ToolStatus::Running, .. })).collect();
    let agents = running.iter().filter(|l| matches!(&l.kind, LineKind::Tool { name, .. } if name == "spawn_agent")).count();
    if agents > 1 {
        return (format!("Running {agents} agents"), false);
    }
    if let Some(step) = running.last() {
        return (in_progress(&shorten(&step.text, project)), false);
    }
    match mine.last() {
        Some(l) if matches!(l.kind, LineKind::Message(MessageRole::Assistant)) && l.pending && !l.text.is_empty() => ("Writing…".into(), false),
        Some(l) if l.kind == LineKind::Reasoning && l.pending => {
            let label = thinking_label(&l.reasoning);
            (if label == "Thinking" { "Thinking…".into() } else { label }, false)
        }
        _ => ("Thinking…".into(), false),
    }
}


#[cfg(test)]
mod tests {
    #[test]
    fn a_command_that_only_printed_shows_without_the_stream_label() {
        assert_eq!(super::command_output("stdout:\nsecond"), "second");
        assert_eq!(super::command_output("stdout:\nout\nstderr:\nwarn"), "stdout:\nout\nstderr:\nwarn");
        assert_eq!(super::command_output("stderr:\nboom"), "stderr:\nboom");
        assert_eq!(super::command_output("(no output)"), "(no output)");
    }

    use super::*;

    fn line(id: &str, kind: LineKind, text: &str, at: i64) -> Line {
        Line { at, ..Line::new(id, kind, text) }
    }
    fn user(id: &str, at: i64) -> Line {
        line(id, LineKind::Message(MessageRole::User), "do it", at)
    }
    fn tool(id: &str, name: &str, text: &str, at: i64) -> Line {
        line(id, LineKind::Tool { name: name.into(), status: ToolStatus::Done, output: String::new() }, text, at)
    }
    fn thought(id: &str, at: i64) -> Line {
        Line { reasoning: "**Looking at the code**".into(), ..line(id, LineKind::Message(MessageRole::Assistant), "", at) }
    }
    fn reply(id: &str, at: i64) -> Line {
        line(id, LineKind::Message(MessageRole::Assistant), "Done.", at)
    }

    #[test]
    fn a_finished_turn_folds_its_work_under_worked_for() {
        let lines = vec![
            user("u", 0),
            thought("t1", 1_000),
            tool("a", "read", "Read src/a.rs", 2_000),
            thought("t2", 3_000),
            tool("b", "bash", "Ran cargo test", 4_000),
            tool("c", "edit", "Edited src/a.rs", 5_000),
            reply("r", 80_000),
        ];
        let blocks = layout(&lines, false);
        assert_eq!(blocks, vec![
            Block::Line { id: "u".into(), folded_by: None },
            Block::Worked { id: "worked-u".into(), millis: 80_000 },
            Block::Group {
                id: "group-t1".into(),
                members: vec!["t1".into(), "a".into(), "t2".into(), "b".into(), "c".into()],
                active: false,
                folded_by: Some("worked-u".into()),
            },
            Block::Line { id: "r".into(), folded_by: None },
        ]);
        assert_eq!(duration(80_000), "1m 20s");
    }

    #[test]
    fn a_running_turn_shows_its_live_group_and_folds_nothing() {
        let lines = vec![user("u", 0), tool("a", "read", "Read a", 1), Line { pending: true, ..thought("p", 2) }];
        let blocks = layout(&lines, true);
        assert_eq!(blocks, vec![
            Block::Line { id: "u".into(), folded_by: None },
            Block::Group { id: "group-a".into(), members: vec!["a".into(), "p".into()], active: true, folded_by: None },
        ]);
        // Before any tool, the starting reply shows as Thinking.
        let blocks = layout(&[user("u", 0), Line { pending: true, ..thought("p", 1) }], true);
        assert_eq!(blocks[1], Block::Line { id: "p".into(), folded_by: None });
    }

    #[test]
    fn cards_messages_and_failures_split_groups_and_lone_calls_stay_rows() {
        let approval =
            line("q", LineKind::Approval { summary: "Run x".into(), detail: String::new(), rememberable: true, decided: None }, "", 3);
        let lines = vec![
            user("u", 0),
            tool("a", "read", "Read a", 1),
            tool("b", "read", "Read b", 2),
            approval,
            tool("c", "bash", "Ran x", 4),
            line("f", LineKind::Failure, "stopped", 5),
        ];
        let blocks = layout(&lines, false);
        assert_eq!(blocks, vec![
            Block::Line { id: "u".into(), folded_by: None },
            Block::Group { id: "group-a".into(), members: vec!["a".into(), "b".into()], active: false, folded_by: None },
            Block::Line { id: "q".into(), folded_by: None },
            Block::Line { id: "c".into(), folded_by: None },
            Block::Line { id: "f".into(), folded_by: None },
        ]);

        // A message mid-way, then more work and a failure: nothing folds.
        let lines = vec![
            user("u", 0),
            tool("a", "read", "Read a", 1),
            reply("m", 2),
            tool("b", "read", "Read b", 3),
            line("f", LineKind::Failure, "stopped", 4),
        ];
        assert!(layout(&lines, false).iter().all(|b| b.folded_by().is_none() && !matches!(b, Block::Worked { .. })));

        // The Manager's task cards stay out when its turn folds.
        let lines = vec![user("u", 0), tool("k", "create_task", "Created a task", 1), reply("r", 2)];
        assert_eq!(layout(&lines, false), vec![
            Block::Line { id: "u".into(), folded_by: None },
            Block::Worked { id: "worked-u".into(), millis: 2 },
            Block::Line { id: "k".into(), folded_by: None },
            Block::Line { id: "r".into(), folded_by: None },
        ]);
    }

    #[test]
    fn summaries_read_like_the_aegis_trace() {
        let calls = |list: &[(&str, &str)]| {
            list.iter().enumerate().map(|(i, (n, t))| tool(&format!("t{i}"), n, t, i as i64)).collect::<Vec<_>>()
        };
        let mixed = stages(&calls(&[("read", "Read a"), ("grep", "Searched x"), ("bash", "Ran cargo test"), ("bash", "Ran npm i"), ("edit", "Edited a")]));
        // Looking around in a row is one step; each command and edit its own.
        assert_eq!(mixed.len(), 4);
        assert_eq!(stage_title(&mixed[0], None), "Explored 1 file, 1 search");
        assert_eq!(stage_title(&mixed[1], None), "Ran cargo test");
        assert_eq!(summary(&mixed), "Explored 1 file, 1 search, ran 2 commands, and edited 1 file");
        assert_eq!(summary(&stages(&calls(&[("bash", "Ran ls -la"), ("bash", "Ran git status --short")]))), "Explored 2 lists");
        assert_eq!(summary(&stages(&calls(&[("todo", "Updated todos"), ("web_search", "Searched the web for x")]))), "Called 1 tool and searched the web");
        assert_eq!(summary(&stages(&[thought("r", 0)])), "Reasoning");
        assert_eq!(in_progress("Ran cargo test"), "Running cargo test");
        assert_eq!(thinking_label("plan\n**Checking the tests**\nmore"), "Checking the tests");
        assert_eq!(thinking_label("just words"), "Thinking");
        assert_eq!(duration(400), "0s");
        assert_eq!(duration(60_000), "1m");
        assert_eq!(duration(7_385_000), "2h 3m 5s");
        assert_eq!(shorten("Read /p/x/src/a.rs in /p/x", Some("/p/x")), "Read src/a.rs in .");
    }

    #[test]
    fn streaming_first_token_and_completion_keep_the_same_tool_group() {
        use crate::transcript::{self, Phase};
        let mut lines = vec![user("u", 0)];
        transcript::begin_turn(&mut lines, "turn-u", 0, TurnStatus::Working);
        let mut t = tool("tool", "read", "Read a", 1).in_turn("turn-u");
        t.call_id = Some("reused".into());
        lines.push(t);
        let id = transcript::begin_model(&mut lines, "turn-u", 2);
        let before = layout(&lines, true);
        let group = before.iter().find(|b| matches!(b, Block::Group { .. })).unwrap().id().to_owned();
        transcript::text(&mut lines, "turn-u", &id, "First");
        let after = layout(&lines, true);
        assert!(after.iter().any(|b| b.id() == group));
        // While running, the work folds; the answer and the status line stay out.
        assert_eq!(after.iter().find(|b| b.id() == group).unwrap().folded_by(), Some("live:turn-u"));
        assert!(matches!(&after[1], Block::Turn { live_fold: true, .. }));
        let tail: Vec<&str> = after[after.len() - 2..].iter().map(Block::id).collect();
        assert_eq!(tail, [id.as_str(), "status:turn-u"]);
        assert_eq!(after[after.len() - 2].folded_by(), None);
        lines.iter_mut().find(|l| l.id == id).unwrap().phase = Phase::Final;
        transcript::finish(&mut lines, "turn-u", TurnStatus::Completed, 30_000);
        let done = layout(&lines, false);
        assert!(matches!(&done[1], Block::Turn { id, completed_at: Some(30_000), collapsible: true, .. } if id == "turn-u"));
        assert!(done.iter().any(|b| b.id() == group && b.folded_by() == Some("turn-u")));
        assert_eq!(done.last().unwrap().id(), id);
        assert_eq!(done.last().unwrap().folded_by(), None);
    }

    #[test]
    fn narration_breaks_groups_and_stopped_partial_answers_never_fold() {
        use crate::transcript::{self, Phase};
        let mut lines = vec![user("u", 0)];
        transcript::begin_turn(&mut lines, "turn-u", 0, TurnStatus::Working);
        lines.push(tool("a", "read", "Read a", 1).in_turn("turn-u"));
        lines.push(Line { phase: Phase::Commentary, ..reply("commentary", 2).in_turn("turn-u") });
        lines.push(tool("b", "bash", "Ran test", 3).in_turn("turn-u"));
        let id = transcript::begin_model(&mut lines, "turn-u", 3);
        transcript::text(&mut lines, "turn-u", &id, "Partial");
        transcript::finish(&mut lines, "turn-u", TurnStatus::Stopped, 40);
        let stopped = layout(&lines, false);
        assert_eq!(stopped.iter().map(Block::id).collect::<Vec<_>>(),
            ["u", "turn-u", "group-a", "commentary", "group-b", id.as_str()]);
        assert!(stopped.iter().all(|b| b.folded_by().is_none()));
        assert!(!stopped.iter().any(|b| matches!(b, Block::Turn { collapsible: true, .. })));
        let saved: Vec<Line> = serde_json::from_str(&serde_json::to_string(&lines).unwrap()).unwrap();
        assert_eq!(layout(&saved, false), stopped);
    }

    #[test]
    fn a_running_turn_keeps_its_latest_note_and_open_questions_out_of_the_fold() {
        use crate::transcript;
        let mut lines = vec![user("u", 0)];
        transcript::begin_turn(&mut lines, "turn-u", 0, TurnStatus::Working);
        lines.push(Line { phase: Phase::Commentary, ..reply("early", 1).in_turn("turn-u") });
        lines.push(tool("a", "read", "Read a", 2).in_turn("turn-u"));
        lines.push(Line { reasoning: "**Weighing it**".into(), ..line("r", LineKind::Reasoning, "", 3).in_turn("turn-u") });
        lines.push(Line { phase: Phase::Commentary, ..reply("late", 4).in_turn("turn-u") });
        let ask = LineKind::Approval { summary: "Run x".into(), detail: String::new(), rememberable: true, decided: None };
        lines.push(line("q", ask, "", 5).in_turn("turn-u"));
        let blocks = layout(&lines, true);
        let folded = |id: &str| blocks.iter().find(|b| b.id() == id).unwrap().folded_by().map(str::to_owned);
        assert_eq!(folded("early"), Some("live:turn-u".into()));
        assert_eq!(folded("group-a"), Some("live:turn-u".into()));
        assert_eq!(folded("r"), Some("live:turn-u".into()));
        assert_eq!(folded("late"), None);
        assert_eq!(folded("q"), None);
        assert_eq!(blocks.last().unwrap().id(), "status:turn-u");
        assert_eq!(activity(&lines, "turn-u", TurnStatus::NeedsInput, None), ("Waiting for your approval".into(), true));
        // Nothing finished yet: no summary row, only the status line.
        let fresh = vec![user("u", 0), line("turn-u", LineKind::Turn { status: TurnStatus::Working, completed_at: None }, "", 0).in_turn("turn-u")];
        assert!(matches!(&layout(&fresh, true)[1], Block::Turn { live_fold: false, .. }));
    }

    #[test]
    fn live_output_keeps_the_last_frames_and_diffs_count_their_lines() {
        assert_eq!(output_tail("a\nb\n\nc\r\n10%\r50%\r100%\nd\n", 3), "c\n100%\nd");
        assert_eq!(diff_stat(" keep\n-old\n+new\n+more"), (2, 1));
    }

    #[test]
    fn follow_only_resumes_near_the_bottom() {
        assert!(!near_bottom(950.0, 500.0, 1500.0));
        assert!(near_bottom(970.0, 500.0, 1500.0));
        assert!(near_bottom(0.0, 500.0, 200.0));
    }
}
