//! The tools an agent works with in a project folder: reading, searching
//! and listing run freely; writing, editing and shell commands ask first.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};

use crate::provider::{ToolCall, ToolSpec};

const READ_LINES: usize = 2000;
const READ_MAX_BYTES: usize = 256 * 1024;
const OUTPUT_MAX_CHARS: usize = 30_000;
const GREP_MAX_MATCHES: usize = 200;
const LS_MAX_ENTRIES: usize = 500;
const BASH_DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);
const BASH_MAX_TIMEOUT: Duration = Duration::from_secs(600);

/// What a tool gave back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolOutput {
    pub content: String,
    pub is_error: bool,
}

impl ToolOutput {
    fn ok(content: impl Into<String>) -> Self {
        ToolOutput { content: content.into(), is_error: false }
    }

    fn error(content: impl Into<String>) -> Self {
        ToolOutput { content: content.into(), is_error: true }
    }
}

/// A call that needs a person's yes before it runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Permission {
    /// What "allow for this task" remembers: one exact command, or file
    /// changes in the project.
    pub grant: String,
    /// One line a person reads to decide.
    pub summary: String,
    /// The command or the file it touches, shown as is.
    pub detail: String,
    /// Whether "allow for this task" may cover it. Pushes and writes on
    /// GitHub always ask.
    pub rememberable: bool,
}

/// Commands that publish: pushes and writes on GitHub through `gh`.
pub fn publishes(command: &str) -> bool {
    let command = command.to_ascii_lowercase();
    const WRITES: &[&str] = &[
        "git push",
        "gh pr create",
        "gh pr merge",
        "gh pr comment",
        "gh pr review",
        "gh pr close",
        "gh pr edit",
        "gh issue create",
        "gh issue comment",
        "gh issue close",
        "gh issue edit",
        "gh release create",
    ];
    WRITES.iter().any(|w| command.contains(w))
        || (command.contains("gh api")
            && ["-x post", "-x patch", "-x put", "-x delete", "--method"].iter().any(|m| command.contains(m)))
}

/// The tools on offer.
pub fn specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "read".into(),
            description: "Read a text file. Lines come numbered. Use offset and limit for long files.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File path, relative to the project or absolute" },
                    "offset": { "type": "integer", "description": "First line to read, from 1" },
                    "limit": { "type": "integer", "description": "How many lines to read" }
                },
                "required": ["path"]
            }),
        },
        ToolSpec {
            name: "write".into(),
            description: "Create or overwrite a file with the given content. Read a file before overwriting it.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "content": { "type": "string" }
                },
                "required": ["path", "content"]
            }),
        },
        ToolSpec {
            name: "edit".into(),
            description: "Replace an exact piece of a file. old_string must match exactly once unless replace_all is true. Read the file first.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "old_string": { "type": "string" },
                    "new_string": { "type": "string" },
                    "replace_all": { "type": "boolean" }
                },
                "required": ["path", "old_string", "new_string"]
            }),
        },
        ToolSpec {
            name: "bash".into(),
            description: "Run a shell command in the project folder and get its output. Use it for builds, tests, git and tools without their own function.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string" },
                    "timeout_seconds": { "type": "integer", "description": "Up to 600; default 120" }
                },
                "required": ["command"]
            }),
        },
        ToolSpec {
            name: "grep".into(),
            description: "Search file contents with a regular expression, skipping what .gitignore ignores. Returns path:line: text.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string" },
                    "path": { "type": "string", "description": "Folder or file to search; default the project" },
                    "glob": { "type": "string", "description": "Only files matching this glob, such as *.rs" }
                },
                "required": ["pattern"]
            }),
        },
        ToolSpec {
            name: "ls".into(),
            description: "List a folder's entries; folders end with /.".into(),
            parameters: json!({
                "type": "object",
                "properties": { "path": { "type": "string", "description": "Default the project folder" } }
            }),
        },
    ]
}

fn str_arg<'a>(args: &'a Value, name: &str) -> Result<&'a str> {
    args.get(name).and_then(Value::as_str).ok_or_else(|| anyhow!("missing argument {name}"))
}

fn resolve(cwd: &Path, path: &str) -> PathBuf {
    let path = Path::new(path.trim());
    if path.is_absolute() { path.to_owned() } else { cwd.join(path) }
}

/// Whether `call` must ask first, and what to ask.
pub fn permission(call: &ToolCall, cwd: &Path) -> Option<Permission> {
    let args = call.arguments_json();
    match call.name.as_str() {
        "bash" => {
            let command = args["command"].as_str().unwrap_or_default().trim().to_owned();
            let rememberable = !publishes(&command);
            let summary = if rememberable { "Run a command" } else { "Run a command that publishes" };
            Some(Permission { grant: format!("bash:{command}"), summary: summary.into(), detail: command, rememberable })
        }
        "write" | "edit" => {
            let path = args["path"].as_str().unwrap_or_default();
            let shown = resolve(cwd, path)
                .strip_prefix(cwd)
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| path.to_owned());
            let summary = if call.name == "write" { "Write a file" } else { "Edit a file" };
            Some(Permission { grant: "files".into(), summary: summary.into(), detail: shown, rememberable: true })
        }
        _ => None,
    }
}

/// One line saying what a call does, for the conversation.
pub fn describe(call: &ToolCall) -> String {
    let args = call.arguments_json();
    let arg = |name: &str| args[name].as_str().unwrap_or_default().to_owned();
    match call.name.as_str() {
        "read" => format!("Read {}", arg("path")),
        "write" => format!("Wrote {}", arg("path")),
        "edit" => format!("Edited {}", arg("path")),
        "bash" => format!("Ran {}", first_line(&arg("command"))),
        "grep" => format!("Searched for {}", arg("pattern")),
        "ls" => {
            let path = arg("path");
            let path = path.trim().trim_end_matches('/');
            format!("Listed {}", if path.is_empty() || path == "." { "the project" } else { path })
        }
        "list_tasks" => "Checked the tasks".to_owned(),
        "get_task" => "Looked at a task".to_owned(),
        "list_projects" => "Checked the projects".to_owned(),
        "create_task" => format!("Started a task: {}", arg("title")),
        "continue_task" => "Followed up on a task".to_owned(),
        "update_task" => "Updated a task".to_owned(),
        "link_github" => "Linked a task to GitHub".to_owned(),
        "github_comment" => format!("Commented on #{}", args["number"]),
        "github_open_pr" => format!("Opened a pull request: {}", arg("title")),
        "github_close_issue" => format!("Closed issue #{}", args["number"]),
        "skill" => format!("Loaded the {} skill", arg("name")),
        "skill_search" => format!("Looked for skills: {}", arg("query")),
        "web_search" => format!("Searched the web for {}", arg("query")),
        "web_fetch" => format!("Read {}", arg("url")),
        "tool_search" => "Loaded tools".to_owned(),
        "memory" => match args["query"].as_str().filter(|q| !q.trim().is_empty()) {
            Some(query) => format!("Searched memory for {query}"),
            None => "Read the memory summary".to_owned(),
        },
        "spawn_agent" => format!(
            "Started a subagent: {}",
            args["description"].as_str().or(args["message"].as_str()).or(args["task"].as_str()).map(|t| first_line(t)).unwrap_or_default()
        ),
        "wait_agent" => "Waited for subagents".to_owned(),
        "send_input" => "Sent a subagent a follow-up".to_owned(),
        "close_agent" => "Closed a subagent".to_owned(),
        "list_agents" => "Checked the subagents".to_owned(),
        "run_workflow" => format!("Ran a workflow{}", args["title"].as_str().map(|t| format!(": {t}")).unwrap_or_default()),
        other => match crate::mcp::parse_tool_name(other) {
            Some((server, tool)) => format!("Used {tool} from {server}"),
            None => format!("Used {other}"),
        },
    }
}

fn first_line(text: &str) -> String {
    let line = text.lines().next().unwrap_or_default();
    if text.lines().count() > 1 { format!("{line} …") } else { line.to_owned() }
}

/// Runs `call` in `cwd`. Failures become error output for the model rather
/// than errors of the run.
pub async fn execute(call: &ToolCall, cwd: &Path) -> ToolOutput {
    let args = call.arguments_json();
    let result = match call.name.as_str() {
        "read" => read(&args, cwd),
        "write" => write(&args, cwd),
        "edit" => edit(&args, cwd),
        "bash" => bash(&args, cwd).await,
        "grep" => grep(&args, cwd),
        "ls" => ls(&args, cwd),
        other => Err(anyhow!("there is no tool called {other}")),
    };
    match result {
        Ok(output) => output,
        Err(error) => ToolOutput::error(format!("{error:#}")),
    }
}

fn read(args: &Value, cwd: &Path) -> Result<ToolOutput> {
    let path = resolve(cwd, str_arg(args, "path")?);
    let bytes = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
    if bytes.contains(&0) {
        bail!("{} is not a text file", path.display());
    }
    let text = String::from_utf8_lossy(&bytes[..bytes.len().min(READ_MAX_BYTES)]).into_owned();
    let offset = args["offset"].as_u64().unwrap_or(1).max(1) as usize;
    let limit = args["limit"].as_u64().map(|n| n as usize).unwrap_or(READ_LINES);
    let lines: Vec<&str> = text.lines().collect();
    if lines.is_empty() {
        return Ok(ToolOutput::ok("(empty file)"));
    }
    let mut out = String::new();
    for (index, line) in lines.iter().enumerate().skip(offset - 1).take(limit) {
        out.push_str(&format!("{:>6}\t{line}\n", index + 1));
    }
    let shown_end = (offset - 1 + limit).min(lines.len());
    if shown_end < lines.len() || bytes.len() > READ_MAX_BYTES {
        out.push_str(&format!("… {} more lines; read on with offset {}\n", lines.len() - shown_end, shown_end + 1));
    }
    Ok(ToolOutput::ok(out))
}

fn write(args: &Value, cwd: &Path) -> Result<ToolOutput> {
    let path = resolve(cwd, str_arg(args, "path")?);
    let content = str_arg(args, "content")?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    std::fs::write(&path, content).with_context(|| format!("writing {}", path.display()))?;
    Ok(ToolOutput::ok(format!("Wrote {} bytes to {}", content.len(), path.display())))
}

fn edit(args: &Value, cwd: &Path) -> Result<ToolOutput> {
    let path = resolve(cwd, str_arg(args, "path")?);
    let old = str_arg(args, "old_string")?;
    let new = str_arg(args, "new_string")?;
    let replace_all = args["replace_all"].as_bool().unwrap_or(false);
    if old.is_empty() {
        bail!("old_string is empty; use write to create a file");
    }
    let text = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let count = text.matches(old).count();
    let updated = match count {
        0 => bail!("old_string does not appear in {}", path.display()),
        1 => text.replacen(old, new, 1),
        n if replace_all => {
            let _ = n;
            text.replace(old, new)
        }
        n => bail!("old_string appears {n} times in {}; add context to make it unique or set replace_all", path.display()),
    };
    std::fs::write(&path, updated).with_context(|| format!("writing {}", path.display()))?;
    Ok(ToolOutput::ok(format!("Replaced {} in {}", if count == 1 { "1 match".to_owned() } else { format!("{count} matches") }, path.display())))
}

/// The person's login shell, so commands find what their terminal finds
/// (an app opened from the Finder starts with a bare PATH).
fn login_shell() -> String {
    std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/bin/zsh".to_owned())
}

async fn bash(args: &Value, cwd: &Path) -> Result<ToolOutput> {
    let command = str_arg(args, "command")?;
    let timeout = args["timeout_seconds"]
        .as_u64()
        .map(Duration::from_secs)
        .unwrap_or(BASH_DEFAULT_TIMEOUT)
        .min(BASH_MAX_TIMEOUT);
    let mut child = tokio::process::Command::new(login_shell());
    child
        .arg("-lc")
        .arg(command)
        .current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    crate::detach_async(&mut child);
    let output = match tokio::time::timeout(timeout, child.output()).await {
        Ok(output) => output.with_context(|| format!("starting {}", login_shell()))?,
        Err(_) => return Ok(ToolOutput::error(format!("The command ran longer than {}s and was stopped.", timeout.as_secs()))),
    };
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !stderr.trim().is_empty() {
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&stderr);
    }
    let text = clip(&text);
    let code = output.status.code();
    if output.status.success() {
        Ok(ToolOutput::ok(if text.trim().is_empty() { "(no output)".to_owned() } else { text }))
    } else {
        let code = code.map(|c| c.to_string()).unwrap_or_else(|| "a signal".into());
        Ok(ToolOutput::error(format!("{text}\n(exit code {code})")))
    }
}

/// Keeps the start and the end of a long output.
fn clip(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= OUTPUT_MAX_CHARS {
        return text.to_owned();
    }
    let half = OUTPUT_MAX_CHARS / 2;
    let head: String = chars[..half].iter().collect();
    let tail: String = chars[chars.len() - half..].iter().collect();
    format!("{head}\n… {} characters left out …\n{tail}", chars.len() - OUTPUT_MAX_CHARS)
}

fn grep(args: &Value, cwd: &Path) -> Result<ToolOutput> {
    let pattern = regex::Regex::new(str_arg(args, "pattern")?).context("the pattern is not a valid regular expression")?;
    let root = args["path"].as_str().map(|p| resolve(cwd, p)).unwrap_or_else(|| cwd.to_owned());
    let mut walker = ignore::WalkBuilder::new(&root);
    walker.hidden(true);
    if let Some(glob) = args["glob"].as_str().filter(|g| !g.is_empty()) {
        let mut overrides = ignore::overrides::OverrideBuilder::new(&root);
        overrides.add(glob)?;
        walker.overrides(overrides.build()?);
    }
    let mut matches = Vec::new();
    'files: for entry in walker.build().flatten() {
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(entry.path()) else { continue };
        let shown = entry.path().strip_prefix(cwd).unwrap_or(entry.path()).display().to_string();
        for (index, line) in text.lines().enumerate() {
            if pattern.is_match(line) {
                let line: String = line.chars().take(300).collect();
                matches.push(format!("{shown}:{}: {line}", index + 1));
                if matches.len() >= GREP_MAX_MATCHES {
                    matches.push(format!("… stopped at {GREP_MAX_MATCHES} matches"));
                    break 'files;
                }
            }
        }
    }
    Ok(ToolOutput::ok(if matches.is_empty() { "No matches.".to_owned() } else { matches.join("\n") }))
}

fn ls(args: &Value, cwd: &Path) -> Result<ToolOutput> {
    let path = args["path"].as_str().filter(|p| !p.is_empty()).map(|p| resolve(cwd, p)).unwrap_or_else(|| cwd.to_owned());
    let mut entries: Vec<String> = std::fs::read_dir(&path)
        .with_context(|| format!("listing {}", path.display()))?
        .flatten()
        .map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            if entry.file_type().is_ok_and(|t| t.is_dir()) { format!("{name}/") } else { name }
        })
        .collect();
    entries.sort();
    let total = entries.len();
    entries.truncate(LS_MAX_ENTRIES);
    if total > LS_MAX_ENTRIES {
        entries.push(format!("… {} more", total - LS_MAX_ENTRIES));
    }
    Ok(ToolOutput::ok(if entries.is_empty() { "(empty folder)".to_owned() } else { entries.join("\n") }))
}

#[cfg(test)]
mod tests {
    use super::*;

    static NEXT_DIR: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    fn call(name: &str, args: Value) -> ToolCall {
        ToolCall { id: "1".into(), name: name.into(), arguments: args.to_string() }
    }

    fn folder() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("bubble-tools-{}-{}", std::process::id(), NEXT_DIR.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/a.rs"), "fn main() {\n    println!(\"hi\");\n}\n").unwrap();
        std::fs::write(dir.join(".gitignore"), "target/\n").unwrap();
        std::fs::create_dir_all(dir.join("target")).unwrap();
        std::fs::write(dir.join("target/skip.rs"), "fn main() {}\n").unwrap();
        dir
    }

    #[tokio::test]
    async fn read_numbers_lines_and_pages() {
        let dir = folder();
        let out = execute(&call("read", json!({"path": "src/a.rs"})), &dir).await;
        assert!(!out.is_error);
        assert!(out.content.contains("     2\t    println!(\"hi\");"));
        let out = execute(&call("read", json!({"path": "src/a.rs", "offset": 2, "limit": 1})), &dir).await;
        assert!(out.content.starts_with("     2\t"));
        assert!(out.content.contains("1 more lines; read on with offset 3"));
        let missing = execute(&call("read", json!({"path": "nope.txt"})), &dir).await;
        assert!(missing.is_error);
    }

    #[tokio::test]
    async fn edit_requires_a_unique_match() {
        let dir = folder();
        std::fs::write(dir.join("b.txt"), "one two one").unwrap();
        let twice = execute(&call("edit", json!({"path": "b.txt", "old_string": "one", "new_string": "1"})), &dir).await;
        assert!(twice.is_error);
        assert!(twice.content.contains("appears 2 times"));
        let all = execute(&call("edit", json!({"path": "b.txt", "old_string": "one", "new_string": "1", "replace_all": true})), &dir).await;
        assert!(!all.is_error, "{}", all.content);
        assert_eq!(std::fs::read_to_string(dir.join("b.txt")).unwrap(), "1 two 1");
    }

    #[tokio::test]
    async fn write_creates_folders() {
        let dir = folder();
        let out = execute(&call("write", json!({"path": "docs/new/notes.md", "content": "hello"})), &dir).await;
        assert!(!out.is_error, "{}", out.content);
        assert_eq!(std::fs::read_to_string(dir.join("docs/new/notes.md")).unwrap(), "hello");
    }

    #[tokio::test]
    async fn bash_runs_in_the_project_and_reports_failure() {
        let dir = folder();
        let out = execute(&call("bash", json!({"command": "ls src && echo done"})), &dir).await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("a.rs") && out.content.contains("done"));
        let failed = execute(&call("bash", json!({"command": "echo oops >&2; exit 3"})), &dir).await;
        assert!(failed.is_error);
        assert!(failed.content.contains("oops") && failed.content.contains("exit code 3"));
    }

    #[tokio::test]
    async fn grep_skips_ignored_files_and_honors_globs() {
        let dir = folder();
        // `ignore` reads .gitignore only inside a git repository.
        std::process::Command::new("git").arg("init").arg("-q").current_dir(&dir).status().unwrap();
        let out = execute(&call("grep", json!({"pattern": "fn main"})), &dir).await;
        assert!(out.content.contains("src/a.rs:1:"), "{}", out.content);
        assert!(!out.content.contains("skip.rs"), "{}", out.content);
        let none = execute(&call("grep", json!({"pattern": "fn main", "glob": "*.md"})), &dir).await;
        assert_eq!(none.content, "No matches.");
    }

    #[tokio::test]
    async fn ls_marks_folders() {
        let dir = folder();
        let out = execute(&call("ls", json!({})), &dir).await;
        assert!(out.content.contains("src/") && out.content.contains(".gitignore"));
    }

    #[test]
    fn only_changes_and_commands_ask() {
        let dir = PathBuf::from("/work/project");
        assert!(permission(&call("read", json!({"path": "a"})), &dir).is_none());
        let bash = permission(&call("bash", json!({"command": " cargo test "})), &dir).unwrap();
        assert_eq!(bash.grant, "bash:cargo test");
        let edit = permission(&call("edit", json!({"path": "/work/project/src/a.rs"})), &dir).unwrap();
        assert_eq!(edit.grant, "files");
        assert_eq!(edit.detail, "src/a.rs");
    }

    #[test]
    fn publishing_commands_always_ask() {
        let dir = PathBuf::from("/work/project");
        let push = permission(&call("bash", json!({"command": "git add -A && git push -u origin HEAD"})), &dir).unwrap();
        assert!(!push.rememberable);
        assert!(!permission(&call("bash", json!({"command": "gh api repos/a/b/issues/1/comments -X POST -f body=x"})), &dir).unwrap().rememberable);
        assert!(permission(&call("bash", json!({"command": "gh pr view 3"})), &dir).unwrap().rememberable);
    }

    #[test]
    fn long_output_keeps_both_ends() {
        let text = "a".repeat(OUTPUT_MAX_CHARS) + &"b".repeat(100);
        let clipped = clip(&text);
        assert!(clipped.starts_with('a') && clipped.ends_with('b'));
        assert!(clipped.contains("100 characters left out"));
    }
}
