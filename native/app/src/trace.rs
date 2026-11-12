//! What a conversation shows of a turn, as a messenger would: what was
//! said, what waits for the person, and what went wrong. How a bot got
//! there (the tools it called, what they printed, its reasoning) is not
//! shown: the person asked for an outcome, not a log. While a bot works,
//! one row at the end says in a few words what it is doing.

use bubble_store::MessageRole;

use crate::avatar::Mood;
use crate::state::{Line, LineKind, ToolStatus, TurnStatus};

/// One thing the conversation shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Block {
    /// A line as it is: a message, a question, an approval that waits, a
    /// failure or a notice. `close`: it follows another thing the same bot
    /// said, and sits near it.
    Line { id: String, close: bool },
    /// What the running turn does now, at the end of the conversation.
    Status { id: String, turn: String, status: TurnStatus, started_at: i64 },
    /// A turn that ended before its bot was done: the person stopped it,
    /// or the app closed. A word at its end says so.
    Ended { id: String, status: TurnStatus },
}

impl Block {
    pub fn id(&self) -> &str {
        match self {
            Block::Line { id, .. } | Block::Status { id, .. } | Block::Ended { id, .. } => id,
        }
    }
}

/// A running turn: queued, working or waiting on the person.
pub fn running(status: TurnStatus) -> bool {
    status.live() || status == TurnStatus::Queued
}

fn says_something(line: &Line) -> bool {
    line.kind == LineKind::Message(MessageRole::Assistant) && !line.text.trim().is_empty()
}

/// Whether a line is the person's to see.
fn shown(line: &Line) -> bool {
    match &line.kind {
        LineKind::Message(MessageRole::User | MessageRole::System | MessageRole::External) => true,
        LineKind::Message(MessageRole::Assistant) => says_something(line),
        LineKind::Message(_) => false,
        // Asked and not yet answered: the person's move. Once decided it
        // is part of how the work went.
        LineKind::Approval { decided, .. } => decided.is_none(),
        LineKind::Question { .. } | LineKind::Failure => true,
        // That older messages were summarized is the agent's housekeeping.
        LineKind::Notice => line.text != crate::state::CHECKPOINT_NOTE,
        LineKind::Tool { .. } | LineKind::Reasoning | LineKind::Turn { .. } | LineKind::RunStart(_) => false,
    }
}

/// Lays the lines out; `busy` while the last turn is still running.
pub fn layout(lines: &[Line], busy: bool) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut after_reply = false;
    // The turn being laid out, when it ended before its bot was done.
    let mut cut_short: Option<(String, TurnStatus)> = None;
    let close_turn = |cut_short: &mut Option<(String, TurnStatus)>, blocks: &mut Vec<Block>| {
        if let Some((turn, status)) = cut_short.take() {
            blocks.push(Block::Ended { id: format!("ended:{turn}"), status });
        }
    };
    for line in lines {
        match line.kind {
            // What sets a bot going begins the next turn.
            LineKind::Message(MessageRole::User | MessageRole::System | MessageRole::External) => close_turn(&mut cut_short, &mut blocks),
            LineKind::Turn { status: status @ (TurnStatus::Stopped | TurnStatus::Interrupted), .. } => cut_short = Some((line.id.clone(), status)),
            _ => {}
        }
        if shown(line) {
            let reply = says_something(line);
            blocks.push(Block::Line { id: line.id.clone(), close: reply && after_reply });
            after_reply = reply;
        }
    }
    close_turn(&mut cut_short, &mut blocks);
    if !busy {
        return blocks;
    }
    // The turn in progress: the newest one with a header, else (a
    // conversation from before turns had headers) whatever runs now.
    let header = lines.iter().rev().find_map(|line| match line.kind {
        LineKind::Turn { status, .. } => Some((line.id.clone(), status, line.at)),
        _ => None,
    });
    let (turn, status, started_at) = match header {
        Some((id, status, at)) if running(status) => (id, status, at),
        Some(_) => return blocks,
        None => (String::new(), TurnStatus::Working, lines.last().map_or(0, |line| line.at)),
    };
    // A reply being written says by itself that the bot is at it.
    let writing = lines.last().is_some_and(|line| line.pending && says_something(line));
    if !writing {
        blocks.push(Block::Status { id: format!("status:{turn}"), turn, status, started_at });
    }
    blocks
}

/// Only user scrolling changes follow intent. Content growth alone must
/// not make a reader who is following the bottom appear to scroll away.
pub fn near_bottom(offset: f32, viewport: f32, content: f32) -> bool {
    content - viewport - offset <= 48.0
}

/// What a bot is doing, in a few words, and how it looks doing it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Activity {
    pub phrase: String,
    pub mood: Mood,
}

impl Activity {
    /// It is the person it waits for (or a free slot): nothing moves.
    pub fn waiting(&self) -> bool {
        self.mood == Mood::Waiting
    }
}

const THINKING: [&str; 3] = ["Thinking it over", "Working it out", "Putting it together"];
const WORKING: [&str; 4] = ["Working on it", "Getting it done", "Making progress", "On it"];

/// One of `phrases`, the same one for the same `key`: a step keeps its
/// words for as long as it lasts.
fn pick(phrases: &[&'static str], key: &str) -> &'static str {
    // FNV-1a: small and the same on every run.
    let hash = key.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3));
    phrases[(hash % phrases.len() as u64) as usize]
}

/// What a tool call is, said as something a person does.
fn doing(name: &str, title: &str, key: &str) -> String {
    match name {
        "bash" | "background_task" | "task_output" | "kill_task" => "Running commands".to_owned(),
        "read" | "grep" | "glob" | "ls" | "lsp" | "notebook_read" => "Reading the files".to_owned(),
        "write" | "edit" | "multi_edit" | "apply_patch" | "notebook_edit" => "Writing the changes".to_owned(),
        "web_search" => "Searching the web".to_owned(),
        "web_fetch" => "Reading the web".to_owned(),
        "spawn_agent" | "run_workflow" | "wait_workflow" | "agent_batch" => "Working with helpers".to_owned(),
        // "Messaged Dev" is how the call was recorded.
        "message_bot" => match title.strip_prefix("Messaged ").filter(|name| !name.is_empty() && *name != "a bot") {
            Some(name) => format!("Messaging {name}"),
            None => "Messaging a bot".to_owned(),
        },
        "create_bot" => "Setting up a bot".to_owned(),
        "list_bots" | "get_bot" | "list_projects" | "update_bot" | "link_github" => "Checking with the team".to_owned(),
        "remember" | "forget" | "recall" | "memory" => "Checking its notes".to_owned(),
        "list_routines" | "add_routine" | "remove_routine" => "Setting its routines".to_owned(),
        "skill" | "skill_search" | "tool_search" => "Getting ready".to_owned(),
        name if name.starts_with("github_") => "Working on GitHub".to_owned(),
        name if name.starts_with("mcp__") => "Using a connected app".to_owned(),
        _ => pick(&WORKING, key).to_owned(),
    }
}

/// How a bot looks while it uses a tool.
fn look(name: &str) -> Mood {
    match name {
        "bash" | "background_task" | "task_output" | "kill_task" => Mood::Running,
        "read" | "grep" | "glob" | "ls" | "lsp" | "notebook_read" | "web_fetch" | "recall" | "skill" => Mood::Reading,
        "write" | "edit" | "multi_edit" | "apply_patch" | "notebook_edit" | "remember" | "forget" | "add_routine" => Mood::Writing,
        "web_search" | "skill_search" | "tool_search" => Mood::Searching,
        "message_bot" | "create_bot" | "list_bots" | "get_bot" | "update_bot" | "spawn_agent" | "run_workflow" | "wait_workflow" | "agent_batch" => {
            Mood::Messaging
        }
        name if name.starts_with("github_") => Mood::Messaging,
        _ => Mood::Working,
    }
}

/// What the running turn `turn` does now.
pub fn activity(lines: &[Line], turn: &str, status: TurnStatus) -> Activity {
    let waits = |phrase: &str| Activity { phrase: phrase.to_owned(), mood: Mood::Waiting };
    if status == TurnStatus::Queued {
        return waits("Waiting for a free slot");
    }
    let mine: Vec<&Line> = lines.iter().filter(|l| turn.is_empty() || (l.turn_id.as_deref() == Some(turn) && l.id != turn)).collect();
    let asked = mine.iter().any(|l| {
        matches!(
            l.kind,
            LineKind::Question { answers: None, .. } | LineKind::Approval { decided: None, .. } | LineKind::Tool { status: ToolStatus::NeedsInput, .. }
        )
    });
    if asked || status == TurnStatus::NeedsInput {
        return waits("Waiting for you");
    }
    let step = mine.iter().rev().find(|l| matches!(l.kind, LineKind::Tool { status: ToolStatus::Running, .. }));
    if let Some(step) = step {
        let LineKind::Tool { name, .. } = &step.kind else { unreachable!("a tool line") };
        return Activity { phrase: doing(name, &step.text, &step.id), mood: look(name) };
    }
    // It has used no tool yet: it read the message and is writing back, as
    // far as anyone can tell. Once it has, the pauses between its steps
    // are thinking.
    if !mine.iter().any(|l| matches!(l.kind, LineKind::Tool { .. })) {
        return Activity { phrase: "Typing".to_owned(), mood: Mood::Typing };
    }
    let key = mine.last().map_or(turn, |last| last.id.as_str());
    Activity { phrase: pick(&THINKING, key).to_owned(), mood: Mood::Thinking }
}

/// What a turn that was cut short says at its end.
pub fn ended(status: TurnStatus) -> &'static str {
    match status {
        TurnStatus::Interrupted => "Interrupted: Bubble closed while it worked.",
        _ => "You stopped it.",
    }
}

/// How long a turn has run, once that is worth saying: "3m", "1h 5m".
pub fn elapsed(millis: i64) -> Option<String> {
    let minutes = millis / 60_000;
    match minutes {
        ..=0 => None,
        1..=59 => Some(format!("{minutes}m")),
        _ if minutes % 60 == 0 => Some(format!("{}h", minutes / 60)),
        _ => Some(format!("{}h {}m", minutes / 60, minutes % 60)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bubble_core::Decision;

    fn line(id: &str, kind: LineKind, text: &str) -> Line {
        Line::new(id, kind, text).in_turn("turn-u")
    }
    fn tool(id: &str, name: &str, text: &str, status: ToolStatus) -> Line {
        line(id, LineKind::Tool { name: name.into(), status, output: String::new() }, text)
    }
    fn header(status: TurnStatus) -> Line {
        Line { at: 1_000, ..Line::new("turn-u", LineKind::Turn { status, completed_at: None }, "") }
    }
    fn said(id: &str, text: &str) -> Line {
        line(id, LineKind::Message(MessageRole::Assistant), text)
    }
    fn ids(blocks: &[Block]) -> Vec<&str> {
        blocks.iter().map(Block::id).collect()
    }

    #[test]
    fn a_finished_turn_shows_what_was_said_and_nothing_of_how() {
        let lines = vec![
            Line::new("u", LineKind::Message(MessageRole::User), "do it"),
            header(TurnStatus::Completed),
            line("r", LineKind::Reasoning, ""),
            said("c", "I'll look at the code first."),
            tool("t1", "read", "Read a.rs", ToolStatus::Done),
            tool("t2", "bash", "Ran cargo test", ToolStatus::Done),
            line("a", LineKind::Approval { summary: "Run".into(), detail: "cargo test".into(), rememberable: false, decided: Some(Decision::AllowOnce) }, ""),
            line("n", LineKind::Notice, crate::state::CHECKPOINT_NOTE),
            said("empty", "  "),
            said("f", "Done: the tests pass."),
        ];
        let blocks = layout(&lines, false);
        assert_eq!(ids(&blocks), ["u", "c", "f"]);
        // What a bot says after something it said sits close to it.
        assert_eq!(blocks[1], Block::Line { id: "c".into(), close: false });
        assert_eq!(blocks[2], Block::Line { id: "f".into(), close: true });
    }

    #[test]
    fn what_waits_for_the_person_and_what_went_wrong_show() {
        let lines = vec![
            Line::new("u", LineKind::Message(MessageRole::User), "do it"),
            header(TurnStatus::Failed),
            line("a", LineKind::Approval { summary: "Run".into(), detail: "rm x".into(), rememberable: false, decided: None }, ""),
            line("q", LineKind::Question { asks: Vec::new(), answers: Some(Vec::new()) }, ""),
            line("x", LineKind::Failure, "The model service refused."),
            line("n", LineKind::Notice, "Taken out of the queue"),
        ];
        assert_eq!(ids(&layout(&lines, false)), ["u", "a", "q", "x", "n"]);
    }

    #[test]
    fn a_running_turn_ends_with_one_row_until_the_reply_is_being_written() {
        let mut lines = vec![Line::new("u", LineKind::Message(MessageRole::User), "do it"), header(TurnStatus::Working), tool("t", "bash", "Ran x", ToolStatus::Running)];
        let blocks = layout(&lines, true);
        assert_eq!(ids(&blocks), ["u", "status:turn-u"]);
        assert_eq!(blocks[1], Block::Status { id: "status:turn-u".into(), turn: "turn-u".into(), status: TurnStatus::Working, started_at: 1_000 });
        // The reply streams in: it is the sign of life now.
        lines.push(Line { pending: true, ..said("f", "Almost") });
        assert_eq!(ids(&layout(&lines, true)), ["u", "f"]);
        // A reply that has not started is not one yet.
        lines.last_mut().unwrap().text.clear();
        assert_eq!(ids(&layout(&lines, true)), ["u", "status:turn-u"]);
        // Nothing runs: no row, whatever an old header says.
        assert_eq!(ids(&layout(&lines[..3], false)), ["u"]);
    }

    #[test]
    fn a_turn_cut_short_says_so_at_its_end() {
        let lines = vec![
            Line::new("u", LineKind::Message(MessageRole::User), "do it"),
            header(TurnStatus::Stopped),
            said("p", "I had got this far."),
            Line::new("u2", LineKind::Message(MessageRole::User), "go on"),
            Line { at: 2_000, ..Line::new("turn-u2", LineKind::Turn { status: TurnStatus::Interrupted, completed_at: None }, "") },
        ];
        let blocks = layout(&lines, false);
        // After what the bot had said, before what the person wrote next.
        assert_eq!(ids(&blocks), ["u", "p", "ended:turn-u", "u2", "ended:turn-u2"]);
        assert_eq!(blocks[2], Block::Ended { id: "ended:turn-u".into(), status: TurnStatus::Stopped });
        assert_eq!(ended(TurnStatus::Stopped), "You stopped it.");
        assert!(ended(TurnStatus::Interrupted).starts_with("Interrupted"));
        // A turn that finished, or failed with its reason shown, says nothing more.
        let done = vec![Line::new("u", LineKind::Message(MessageRole::User), "do it"), header(TurnStatus::Completed), said("f", "Done.")];
        assert_eq!(ids(&layout(&done, false)), ["u", "f"]);
    }

    #[test]
    fn what_a_bot_does_is_said_as_a_person_would() {
        let at = |lines: &[Line], status| activity(lines, "turn-u", status);
        let working = |name: &str, text: &str| at(&[header(TurnStatus::Working), tool("t", name, text, ToolStatus::Running)], TurnStatus::Working);
        assert_eq!(working("bash", "Ran npm test").phrase, "Running commands");
        assert_eq!(working("read", "Read src/a.rs").phrase, "Reading the files");
        assert_eq!(working("edit", "Edited src/a.rs").phrase, "Writing the changes");
        assert_eq!(working("web_search", "Searched the web for x").phrase, "Searching the web");
        assert_eq!(working("message_bot", "Messaged Dev").phrase, "Messaging Dev");
        assert_eq!(working("message_bot", "Messaged a bot").phrase, "Messaging a bot");
        assert_eq!(working("github_comment", "Commented").phrase, "Working on GitHub");
        assert_eq!(working("mcp__linear__list", "Listed").phrase, "Using a connected app");
        assert!(WORKING.contains(&working("something_new", "Did it").phrase.as_str()));
        // Its face goes with what it does.
        assert_eq!(working("bash", "Ran x").mood, Mood::Running);
        assert_eq!(working("read", "Read a").mood, Mood::Reading);
        assert_eq!(working("edit", "Edited a").mood, Mood::Writing);
        assert_eq!(working("web_search", "Searched").mood, Mood::Searching);
        assert_eq!(working("message_bot", "Messaged Dev").mood, Mood::Messaging);
        assert_eq!(working("something_new", "Did it").mood, Mood::Working);

        // Nothing of the command, the path or the query is in the words.
        assert!(!working("bash", "Ran rm -rf build").phrase.contains("rm"));

        // Between steps it thinks, in the same words for as long as it does.
        let thinking = [header(TurnStatus::Working), tool("t", "read", "Read a", ToolStatus::Done), line("r", LineKind::Reasoning, "")];
        let first = at(&thinking, TurnStatus::Working);
        assert!(THINKING.contains(&first.phrase.as_str()));
        assert_eq!(first.mood, Mood::Thinking);
        assert_eq!(first, at(&thinking, TurnStatus::Working));

        // Before its first step it is writing back, as a person would be.
        let fresh = [header(TurnStatus::Working), line("r", LineKind::Reasoning, "")];
        assert_eq!(at(&fresh, TurnStatus::Working), Activity { phrase: "Typing".into(), mood: Mood::Typing });
        assert_eq!(at(&[header(TurnStatus::Working)], TurnStatus::Working).mood, Mood::Typing);

        // It waits for the person: nothing moves.
        let asked = [header(TurnStatus::Working), line("a", LineKind::Approval { summary: "Run".into(), detail: "x".into(), rememberable: false, decided: None }, "")];
        assert_eq!(at(&asked, TurnStatus::Working), Activity { phrase: "Waiting for you".into(), mood: Mood::Waiting });
        assert_eq!(at(&[header(TurnStatus::Queued)], TurnStatus::Queued), Activity { phrase: "Waiting for a free slot".into(), mood: Mood::Waiting });
    }

    #[test]
    fn how_long_is_said_from_a_minute_on() {
        assert_eq!(elapsed(59_000), None);
        assert_eq!(elapsed(60_000).as_deref(), Some("1m"));
        assert_eq!(elapsed(12 * 60_000 + 30_000).as_deref(), Some("12m"));
        assert_eq!(elapsed(60 * 60_000).as_deref(), Some("1h"));
        assert_eq!(elapsed(65 * 60_000).as_deref(), Some("1h 5m"));
    }
}
