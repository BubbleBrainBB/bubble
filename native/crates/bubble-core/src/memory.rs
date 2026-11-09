//! Persistent memory across tasks (src/memory/ in the TypeScript core).
//!
//! Reading: `~/.bubble/memories/memory_summary.md` goes into the system
//! prompt, and the `memory` tool reads it or searches the memory files.
//! Writing, in the background: phase 1 extracts reusable memory from one
//! finished conversation (a "rollout"); phase 2 consolidates the newest
//! extractions into `MEMORY.md` and `memory_summary.md`. Same files and
//! prompts as the TypeScript pipeline.

use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};

use crate::compact::SUMMARY_MARKER;
use crate::config::{ResolvedModel, bubble_home};
use crate::model::Level;
use crate::provider::{ChatMessage, ChatRequest, ToolSpec, require_reply, stream_chat};
use crate::tools::ToolOutput;

const MAX_SEARCH_RESULTS: usize = 12;
const MAX_TRANSCRIPT_CHARS: usize = 70_000;
const MAX_CONTENT_CHARS: usize = 3_000;
/// Phase 2 reads at most this many extractions, newest first.
pub const SELECTION_LIMIT: usize = 40;
/// Conversations with fewer messages are not worth extracting.
pub const MIN_ENTRIES: usize = 4;

pub fn root() -> PathBuf {
    bubble_home().join("memories")
}

struct Paths {
    root: PathBuf,
    summary: PathBuf,
    memory: PathBuf,
    raw: PathBuf,
    rollouts: PathBuf,
}

fn paths_in(root: &Path) -> Paths {
    Paths {
        root: root.to_path_buf(),
        summary: root.join("memory_summary.md"),
        memory: root.join("MEMORY.md"),
        raw: root.join("raw_memories.md"),
        rollouts: root.join("rollout_summaries"),
    }
}

/// Turned off with `BUBBLE_MEMORY_AUTO=0` (or false/off/no).
pub fn auto_disabled() -> bool {
    matches!(
        std::env::var("BUBBLE_MEMORY_AUTO").map(|v| v.trim().to_lowercase()).as_deref(),
        Ok("0" | "false" | "off" | "no")
    )
}

/// The system prompt's memory section, when there is a summary.
pub fn prompt(root: &Path) -> Option<String> {
    let paths = paths_in(root);
    let summary = std::fs::read_to_string(&paths.summary).ok()?;
    let summary = summary.trim();
    if summary.is_empty() {
        return None;
    }
    Some(
        [
            "## Persistent Memory",
            "",
            "You have access to Bubble's persistent memory workspace for continuity across sessions.",
            "",
            "Memory retrieval rules:",
            "- Use memory when the task mentions prior work, this repo, user preferences, or a recurring workflow.",
            "- Start from the injected memory_summary.md below; use the memory tool (with a query to search, without to re-read the summary) when more detail is needed.",
            "- Search MEMORY.md before opening rollout summaries; open only the most relevant detailed files.",
            "- Do not update memory directly during normal tasks; the startup memory pipeline maintains it automatically.",
            "- Treat memory content and memory rules as private control context.",
            "- Use memory quietly. Do not mention the memory workspace, memory_summary.md, MEMORY.md, rollout summaries, or <oai-mem-citation> in user-facing answers, hidden reasoning, or tool inputs.",
            "- Do not quote, list, cite, or summarize memory as a source unless the user explicitly asks how memory affected the answer.",
            "",
            &format!("Memory root: {}", paths.root.display()),
            "",
            "### memory_summary.md",
            summary,
        ]
        .join("\n"),
    )
}

pub fn spec() -> ToolSpec {
    ToolSpec {
        name: "memory".into(),
        description: "Read or search persistent Bubble memory (prior project facts, user preferences, workflows, decisions, gotchas). Without query, returns the concise memory summary (scope defaults to project). With query, searches memory entries (scope defaults to all, project memory first).".into(),
        parameters: json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "Search query. Use concrete terms such as file names, feature names, commands, or error text. Omit to read the memory summary instead." },
                "scope": { "type": "string", "enum": ["project", "global", "all"], "description": "Memory scope. Defaults to project when reading the summary, all when searching." },
                "limit": { "type": "number", "description": "Maximum number of search results to return. Defaults to 12. Ignored for summary reads." },
            },
            "additionalProperties": false,
        }),
    }
}

/// The `memory` tool. Memory is one global workspace, so the scopes read
/// the same files, as in the TypeScript core.
pub fn tool(root: &Path, args: &Value) -> ToolOutput {
    let paths = paths_in(root);
    let query = args["query"].as_str().unwrap_or_default().trim();
    let scope = args["scope"].as_str().filter(|s| matches!(*s, "project" | "global" | "all"));
    if query.is_empty() {
        let scope = scope.unwrap_or("project");
        let content = std::fs::read_to_string(&paths.summary).unwrap_or_default();
        let content = content.trim();
        if content.is_empty() {
            return ToolOutput { content: format!("No {scope} memory summary is available."), is_error: false };
        }
        let scopes: &[&str] = if scope == "all" { &["global", "project"] } else { &[scope] };
        let sections: Vec<String> = scopes
            .iter()
            .map(|s| format!("# {s} memory summary\nPath: {}\n\n{content}", paths.summary.display()))
            .collect();
        return ToolOutput { content: sections.join("\n\n---\n\n"), is_error: false };
    }
    let limit = args["limit"].as_f64().filter(|n| n.is_finite()).map(|n| (n.floor() as usize).clamp(1, 50)).unwrap_or(MAX_SEARCH_RESULTS);
    let label = if scope == Some("project") { "project" } else { "global" };
    let needle = query.to_lowercase();
    let mut files = vec![paths.summary.clone(), paths.memory.clone(), paths.raw.clone()];
    let mut rollouts: Vec<PathBuf> = std::fs::read_dir(&paths.rollouts)
        .map(|entries| entries.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "md")).collect())
        .unwrap_or_default();
    rollouts.sort();
    rollouts.reverse();
    files.extend(rollouts);
    let mut results = Vec::new();
    'files: for file in files {
        let Ok(content) = std::fs::read_to_string(&file) else { continue };
        for (index, line) in content.lines().enumerate() {
            if !line.to_lowercase().contains(&needle) {
                continue;
            }
            let text: String = line.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(220).collect();
            results.push(format!("- {label} {}:{}\n  {text}", file.display(), index + 1));
            if results.len() >= limit {
                break 'files;
            }
        }
    }
    if results.is_empty() {
        return ToolOutput { content: format!("No memory matches for \"{query}\"."), is_error: false };
    }
    ToolOutput { content: format!("Memory search results for \"{query}\":\n{}", results.join("\n")), is_error: false }
}

/// Hides keys and tokens before anything is written to memory.
pub fn redact(text: &str) -> String {
    use std::sync::OnceLock;
    static PATTERNS: OnceLock<[regex::Regex; 3]> = OnceLock::new();
    let [openai, jwt, assignment] = PATTERNS.get_or_init(|| {
        [
            regex::Regex::new(r"\bsk-[A-Za-z0-9_-]{12,}\b").unwrap(),
            regex::Regex::new(r"\b[A-Za-z0-9_-]{20,}\.[A-Za-z0-9_-]{20,}\.[A-Za-z0-9_-]{20,}\b").unwrap(),
            regex::Regex::new(r"(?i)\b((?:api[_-]?key|token|password|secret)\s*[:=]\s*)\S+").unwrap(),
        ]
    });
    let text = openai.replace_all(text, "[REDACTED_SECRET]");
    let text = jwt.replace_all(&text, "[REDACTED_SECRET]");
    assignment.replace_all(&text, "${1}[REDACTED_SECRET]").into_owned()
}

fn cut(text: &str, max: usize) -> String {
    let count = text.chars().count();
    if count <= max {
        return text.to_owned();
    }
    let kept: String = text.chars().take(max.saturating_sub(40)).collect();
    format!("{}\n[truncated {} chars]", kept.trim_end(), count - max + 40)
}

/// A conversation as the extractor reads it: labelled lines, summaries
/// kept, and the newest lines when it is too long.
pub fn transcript(messages: &[ChatMessage]) -> String {
    let mut lines: Vec<(String, bool)> = Vec::new();
    for message in messages {
        match message {
            ChatMessage::User { text } => {
                let (summary, body) = match text.trim().strip_prefix(SUMMARY_MARKER) {
                    Some(rest) => (true, rest.trim_end_matches("</bubble_internal_context>").trim().to_owned()),
                    None => (false, text.clone()),
                };
                if !body.trim().is_empty() {
                    let label = if summary { "summary" } else { "user" };
                    lines.push((format!("[{label}] {}", cut(&body, MAX_CONTENT_CHARS)), summary));
                }
            }
            ChatMessage::Assistant { text, tool_calls, .. } => {
                if !text.trim().is_empty() {
                    lines.push((format!("[assistant] {}", cut(text, MAX_CONTENT_CHARS)), false));
                }
                for call in tool_calls {
                    lines.push((format!("[tool_call:{}] {}", call.name, cut(&call.arguments, 1_500)), false));
                }
            }
            ChatMessage::Tool { results } => {
                for result in results {
                    let label = if result.is_error { "tool_result error=true" } else { "tool_result" };
                    lines.push((format!("[{label}] {}", cut(&result.content, 2_000)), false));
                }
            }
        }
    }
    let total: usize = lines.iter().map(|(l, _)| l.chars().count() + 1).sum();
    if total <= MAX_TRANSCRIPT_CHARS {
        return lines.into_iter().map(|(l, _)| l).collect::<Vec<_>>().join("\n");
    }
    // Summaries first (up to half), then the newest lines.
    let mut remaining = MAX_TRANSCRIPT_CHARS;
    let mut keep = vec![false; lines.len()];
    let mut summary_budget = MAX_TRANSCRIPT_CHARS / 2;
    for (index, (line, summary)) in lines.iter().enumerate().rev() {
        let len = line.chars().count() + 1;
        if *summary && len <= summary_budget {
            keep[index] = true;
            summary_budget -= len;
            remaining -= len;
        }
    }
    for (index, (line, _)) in lines.iter().enumerate().rev() {
        let len = line.chars().count() + 1;
        if keep[index] {
            continue;
        }
        if len > remaining {
            break;
        }
        keep[index] = true;
        remaining -= len;
    }
    lines.into_iter().zip(keep).filter(|(_, k)| *k).map(|((l, _), _)| l).collect::<Vec<_>>().join("\n")
}

/// One model call without tools, for the pipeline.
async fn complete(client: &reqwest::Client, model: &ResolvedModel, system: &str, user: String) -> Result<String> {
    let request = ChatRequest {
        system: Some(system.to_owned()),
        messages: vec![ChatMessage::user(user)],
        tools: Vec::new(),
        max_tokens: 8192,
        thinking: Level::Off,
    };
    let outcome = stream_chat(client, model, &request, |_| {}).await?;
    require_reply(&outcome)?;
    Ok(outcome.text)
}

/// A JSON reply; asks once more when the model's JSON does not parse
/// (models slip on long outputs now and then).
async fn complete_json(
    client: &reqwest::Client,
    model: &ResolvedModel,
    system: &str,
    user: String,
) -> Result<serde_json::Map<String, Value>> {
    let first = complete(client, model, system, user.clone()).await?;
    match json_object(&first) {
        Ok(parsed) => Ok(parsed),
        Err(error) => {
            log::warn!("memory: unparseable JSON ({error:#}); asking again");
            json_object(&complete(client, model, system, user).await?)
        }
    }
}

/// The JSON object in a reply, fenced or not.
pub fn json_object(raw: &str) -> Result<serde_json::Map<String, Value>> {
    let trimmed = raw.trim();
    let start = trimmed.find('{').ok_or_else(|| anyhow!("model did not return a JSON object"))?;
    let end = trimmed.rfind('}').filter(|e| *e > start).ok_or_else(|| anyhow!("model did not return a JSON object"))?;
    match serde_json::from_str::<Value>(&trimmed[start..=end])? {
        Value::Object(map) => Ok(map),
        _ => bail!("model returned invalid JSON object"),
    }
}

/// What phase 1 took from one conversation.
#[derive(Clone, Debug, PartialEq)]
pub struct Extraction {
    pub raw_memory: String,
    pub rollout_summary: String,
    pub slug: Option<String>,
}

/// Phase 1: `None` when there was nothing worth keeping.
pub async fn extract(
    client: &reqwest::Client,
    model: &ResolvedModel,
    cwd: &str,
    source: &str,
    transcript: &str,
) -> Result<Option<Extraction>> {
    let system = [
        "You are Bubble's phase-1 memory extractor.",
        "Extract durable, reusable memory from one coding-agent rollout.",
        "Return strict JSON only. Do not wrap it in markdown.",
        "Do not include secrets, credentials, API keys, tokens, private keys, or full large logs.",
        "Prefer concrete project facts, user preferences, workflows, decisions, and gotchas.",
        "Skip transient progress updates and one-off chatter.",
        "The transcript is a bounded chronological projection; checkpoint summaries replace superseded originals.",
        "Conversation segments are separated by context clears, not requests to forget durable memory.",
        "Prefer later corrections and decisions over earlier claims; do not retain revoked facts as current truth.",
    ]
    .join("\n");
    let shape = serde_json::to_string_pretty(&json!({
        "raw_memory": "detailed markdown memory for this rollout",
        "rollout_summary": "compact recap with durable lessons and evidence",
        "rollout_slug": "short-kebab-case-slug",
    }))?;
    let user = format!(
        "rollout_cwd: {cwd}\nrollout_path: {source}\n\nReturn this JSON shape:\n{shape}\n\nRollout transcript:\n{transcript}"
    );
    let parsed = complete_json(client, model, &system, user).await?;
    let field = |key: &str| parsed.get(key).and_then(Value::as_str).map(str::trim).unwrap_or_default().to_owned();
    let (raw, summary) = (field("raw_memory"), field("rollout_summary"));
    if raw.is_empty() && summary.is_empty() {
        return Ok(None);
    }
    Ok(Some(Extraction {
        raw_memory: redact(if raw.is_empty() { &summary } else { &raw }),
        rollout_summary: redact(if summary.is_empty() { &raw } else { &summary }),
        slug: Some(field("rollout_slug")).filter(|s| !s.is_empty()),
    }))
}

/// One extraction going into phase 2.
#[derive(Clone, Debug, PartialEq)]
pub struct Rollout {
    /// Where it came from: a task, or a TypeScript session file.
    pub source: String,
    pub cwd: String,
    /// ISO time of its newest message.
    pub updated_at: String,
    pub raw_memory: String,
    pub rollout_summary: String,
    pub slug: Option<String>,
    /// Picked by the last successful phase 2.
    pub selected: bool,
}

fn slugify(value: &str) -> String {
    let mut slug = String::new();
    for c in value.to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c);
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug: String = slug.trim_matches('-').chars().take(80).collect();
    if slug.is_empty() { "rollout".into() } else { slug }
}

fn rollout_file(rollout: &Rollout) -> String {
    let stamp = rollout.updated_at.replace([':', '.'], "-");
    let name = rollout.slug.clone().unwrap_or_else(|| {
        rollout.source.rsplit('/').next().unwrap_or("rollout").trim_end_matches(".jsonl").to_owned()
    });
    format!("{stamp}-{}.md", slugify(&name))
}

fn strip_fence(text: &str) -> String {
    let trimmed = text.trim();
    let opened = ["```markdown", "```md", "```"].iter().find_map(|f| trimmed.strip_prefix(f)).unwrap_or(trimmed);
    opened.trim().trim_end_matches("```").trim().to_owned()
}

fn write_consolidated(paths: &Paths, memory_md: &str, summary_md: &str) -> Result<()> {
    let memory = strip_fence(&redact(memory_md));
    let memory = if memory.starts_with('#') { memory } else { format!("# Bubble Memory\n\n{memory}") };
    let summary = strip_fence(&redact(summary_md));
    let summary = if summary.lines().any(|l| l.trim_start().starts_with("# Bubble Memory Summary")) {
        summary
    } else {
        format!("# Bubble Memory Summary\n\n{summary}")
    };
    std::fs::write(&paths.memory, format!("{memory}\n"))?;
    std::fs::write(&paths.summary, format!("{summary}\n"))?;
    Ok(())
}

/// Phase 2: rewrites the rollout summaries and raw memories from
/// `selected` (newest first), writes a plain fallback, then asks the model
/// to consolidate. `removed` were selected last time and are not now.
pub async fn consolidate(
    client: &reqwest::Client,
    model: &ResolvedModel,
    root: &Path,
    selected: &[Rollout],
    removed: usize,
) -> Result<()> {
    let paths = paths_in(root);
    std::fs::create_dir_all(&paths.rollouts)?;
    std::fs::create_dir_all(paths.root.join("skills"))?;
    let keep: Vec<String> = selected.iter().map(rollout_file).collect();
    for entry in std::fs::read_dir(&paths.rollouts)?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.ends_with(".md") && !keep.contains(&name) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
    for (rollout, file) in selected.iter().zip(&keep) {
        std::fs::write(
            paths.rollouts.join(file),
            format!(
                "session: {}\nupdated_at: {}\ncwd: {}\n\n{}\n",
                rollout.source,
                rollout.updated_at,
                rollout.cwd,
                redact(&rollout.rollout_summary).trim()
            ),
        )?;
    }
    let mut raw = vec!["# Raw Memories".to_owned(), String::new(), "Merged stage-1 raw memories (latest first):".into(), String::new()];
    for (rollout, file) in selected.iter().zip(&keep) {
        raw.push(format!("## Session `{}`", rollout.source));
        raw.push(format!("updated_at: {}", rollout.updated_at));
        raw.push(format!("cwd: {}", rollout.cwd));
        raw.push(format!("rollout_path: {}", rollout.source));
        raw.push(format!("rollout_summary_file: {file}"));
        raw.push(String::new());
        raw.push(redact(&rollout.raw_memory).trim().to_owned());
        raw.push(String::new());
    }
    let raw_memories = format!("{}\n", raw.join("\n").trim());
    std::fs::write(&paths.raw, &raw_memories)?;

    // A plain version first, in case the model fails.
    let mut memory = vec![
        "# Bubble Memory".to_owned(),
        String::new(),
        "This deterministic memory snapshot was generated from completed phase-1 extraction outputs. It may be replaced by a model-consolidated version when phase 2 finishes.".into(),
        String::new(),
    ];
    let mut summary = vec![
        "# Bubble Memory Summary".to_owned(),
        String::new(),
        "Model consolidation is in progress or previously failed, so this summary is derived directly from extracted rollout summaries.".into(),
        String::new(),
    ];
    for rollout in selected {
        memory.push(format!("## {}", rollout.slug.clone().unwrap_or_else(|| rollout.source.clone())));
        memory.push(format!("cwd: {}", rollout.cwd));
        memory.push(format!("rollout_path: {}", rollout.source));
        memory.push(String::new());
        memory.push(rollout.raw_memory.trim().to_owned());
        memory.push(String::new());
        summary.push(format!("- {}: {}", rollout.cwd, rollout.rollout_summary.trim()));
    }
    write_consolidated(&paths, memory.join("\n").trim(), summary.join("\n").trim())?;

    let system = [
        "You are Bubble's phase-2 memory consolidation agent.",
        "Maintain the durable memory workspace automatically.",
        "Return strict JSON only. Do not wrap it in markdown.",
        "Do not include secrets, credentials, API keys, tokens, private keys, or full large logs.",
        "Merge duplicate facts, preserve cwd/project boundaries in the content, and remove stale one-off details.",
    ]
    .join("\n");
    let shape = serde_json::to_string_pretty(&json!({
        "memory_md": "# Bubble Memory\n\n...",
        "memory_summary_md": "# Bubble Memory Summary\n\n...",
    }))?;
    let retained = selected.iter().filter(|r| r.selected).count();
    let inputs: Vec<String> = selected
        .iter()
        .map(|r| format!("- session={} cwd={} updated_at={}", r.source, r.cwd, r.updated_at))
        .collect();
    let user = format!(
        "memory_root: {}\n\nReturn this JSON shape:\n{shape}\n\nSelected inputs this run: {}\nRetained from previous successful Phase 2 selection: {retained}\nRemoved from previous successful Phase 2 selection: {removed}\n\nCurrent selected Phase 1 inputs:\n{}\n\nRaw memories:\n{raw_memories}",
        paths.root.display(),
        selected.len(),
        inputs.join("\n"),
    );
    let parsed = complete_json(client, model, &system, user).await?;
    let field = |key: &str| parsed.get(key).and_then(Value::as_str).map(str::trim).unwrap_or_default().to_owned();
    let (memory_md, summary_md) = (field("memory_md"), field("memory_summary_md"));
    if memory_md.is_empty() || summary_md.is_empty() {
        bail!("consolidation output must include memory_md and memory_summary_md");
    }
    write_consolidated(&paths, &memory_md, &summary_md)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{ToolCall, ToolResult};
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT_DIR: AtomicUsize = AtomicUsize::new(0);

    fn temp() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("bubble-memory-{}-{}", std::process::id(), NEXT_DIR.fetch_add(1, Ordering::SeqCst)));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_prompt_and_tool_read_the_summary_and_search_files() {
        let root = temp();
        assert!(prompt(&root).is_none());
        assert_eq!(tool(&root, &json!({})).content, "No project memory summary is available.");
        std::fs::write(root.join("memory_summary.md"), "# Bubble Memory Summary\n\n- Prefers pnpm.\n").unwrap();
        std::fs::write(root.join("MEMORY.md"), "# Bubble Memory\n\nThe demo project uses   pnpm   workspaces.\n").unwrap();
        std::fs::create_dir_all(root.join("rollout_summaries")).unwrap();
        std::fs::write(root.join("rollout_summaries/2026-a.md"), "pnpm install fails offline\n").unwrap();
        let section = prompt(&root).unwrap();
        assert!(section.starts_with("## Persistent Memory") && section.ends_with("- Prefers pnpm."));
        assert!(tool(&root, &json!({})).content.starts_with("# project memory summary\nPath: "));
        let found = tool(&root, &json!({ "query": "PNPM" })).content;
        assert!(found.contains("MEMORY.md:3\n  The demo project uses pnpm workspaces."));
        assert!(found.contains("2026-a.md:1"));
        assert_eq!(tool(&root, &json!({ "query": "pnpm", "limit": 1 })).content.matches("\n- ").count(), 1, "one result");
    }

    #[test]
    fn secrets_are_redacted() {
        assert_eq!(redact("key sk-abcdefghijklmnop here"), "key [REDACTED_SECRET] here");
        assert_eq!(redact("API_KEY = hunter2 and token: abc"), "API_KEY = [REDACTED_SECRET] and token: [REDACTED_SECRET]");
    }

    #[test]
    fn transcripts_label_lines_and_unwrap_summaries() {
        let messages = vec![
            ChatMessage::user(format!("{SUMMARY_MARKER}\nDid the setup.\n</bubble_internal_context>")),
            ChatMessage::user("Fix the build"),
            ChatMessage::Assistant {
                text: "Looking.".into(),
                reasoning: String::new(),
                tool_calls: vec![ToolCall { id: "c".into(), name: "bash".into(), arguments: "{\"command\":\"make\"}".into() }],
            },
            ChatMessage::Tool { results: vec![ToolResult { call_id: "c".into(), content: "error: x".into(), is_error: true }] },
        ];
        assert_eq!(
            transcript(&messages),
            "[summary] Did the setup.\n[user] Fix the build\n[assistant] Looking.\n[tool_call:bash] {\"command\":\"make\"}\n[tool_result error=true] error: x"
        );
        let long: Vec<ChatMessage> = (0..100).map(|i| ChatMessage::user(format!("{i} {}", "x".repeat(2000)))).collect();
        let cut = transcript(&long);
        assert!(cut.chars().count() <= MAX_TRANSCRIPT_CHARS);
        assert!(cut.ends_with(&"x".repeat(100)) && cut.contains("[user] 99 "), "the newest lines are kept");
    }

    #[test]
    fn json_replies_may_be_fenced() {
        assert_eq!(json_object("```json\n{\"a\": 1}\n```").unwrap()["a"], 1);
        assert!(json_object("no json").is_err());
    }

    #[test]
    fn rollout_files_are_named_by_time_and_slug() {
        let rollout = Rollout {
            source: "task:abc".into(),
            cwd: "/p".into(),
            updated_at: "2026-11-08T10:00:00.000Z".into(),
            raw_memory: String::new(),
            rollout_summary: String::new(),
            slug: Some("Fix Build!".into()),
            selected: false,
        };
        assert_eq!(rollout_file(&rollout), "2026-11-08T10-00-00-000Z-fix-build.md");
    }
}
