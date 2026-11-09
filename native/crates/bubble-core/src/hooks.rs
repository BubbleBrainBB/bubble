//! Hooks: commands the person configures to run on agent events, which can
//! watch, add context for the model, or (for some events) deny
//! (src/hooks/ in the TypeScript core).
//!
//! ```json
//! "hooks": { "rules": [
//!   { "event": "PreToolUse", "matcher": "^bash$", "command": "~/bin/check", "onError": "block" }
//! ]}
//! ```
//!
//! in `~/.bubble/settings.json`, then a project's `.bubble/settings.json` and
//! `.bubble/settings.local.json`. The command gets a JSON envelope on stdin;
//! exit code 2, or `{"decision": "deny", "reason": …}` on stdout, denies.
//! `{"modelContext": …}` reaches the model when the rule or the output says
//! `exposeToModel`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

const DEFAULT_TIMEOUT_MS: u64 = 5000;
const MAX_TIMEOUT_MS: u64 = 600_000;
const DEFAULT_MAX_OUTPUT: usize = 64 * 1024;
const MAX_OUTPUT: usize = 1024 * 1024;

pub const EVENTS: &[&str] = &[
    "SessionStart",
    "SessionEnd",
    "UserPromptSubmit",
    "PreModelCall",
    "PreToolUse",
    "PostToolUse",
    "PostToolUseFailure",
    "PermissionRequest",
    "PermissionResult",
    "Stop",
    "StopFailure",
    "PreCompact",
    "PostCompact",
    "SubagentStart",
    "SubagentStop",
    "Notification",
    "SteerInputApplied",
    "QueuedInputRejected",
];

/// Events whose hooks may deny; a deny on any other is ignored.
const BLOCKABLE: &[&str] = &["UserPromptSubmit", "PreToolUse", "PermissionRequest", "PreCompact"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Parent,
    Subagent,
}

impl Role {
    fn label(self) -> &'static str {
        match self {
            Role::Parent => "parent",
            Role::Subagent => "subagent",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Rule {
    pub id: String,
    pub events: Vec<String>,
    pub matcher: Option<regex::Regex>,
    pub command: String,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub env: BTreeMap<String, String>,
    pub timeout: Duration,
    pub max_output: usize,
    /// On a crash, timeout or bad output: deny (`block`) or carry on.
    pub block_on_error: bool,
    /// Payload fields sent in full (`all` for every one).
    pub include: Vec<String>,
    pub expose_to_model: bool,
    pub inherit_to_subagents: bool,
    pub priority: i64,
    pub scope: &'static str,
    index: usize,
}

/// What hooks said about one event.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Outcome {
    pub denied: Option<String>,
    /// Lines for the model.
    pub context: Vec<String>,
    pub matched: usize,
}

/// One event to run hooks for.
pub struct Event<'a> {
    pub name: &'a str,
    pub cwd: &'a Path,
    /// What a matcher tests (a tool's name, the prompt…).
    pub target: &'a str,
    /// Always sent.
    pub payload: Value,
    /// Sent only to rules that `include` a field.
    pub full: Value,
}

/// The hooks for a project (or the person's own, with none).
#[derive(Clone, Debug, Default)]
pub struct Hooks {
    pub rules: Vec<Rule>,
    pub diagnostics: Vec<String>,
    home: PathBuf,
}

impl Hooks {
    pub fn load(home: &Path, project: Option<&Path>) -> Hooks {
        let mut hooks = Hooks { home: home.to_path_buf(), ..Hooks::default() };
        // A hook's own agent run must not set off hooks again.
        if std::env::var("BUBBLE_HOOK_DEPTH").as_deref() == Ok("1") {
            return hooks;
        }
        let mut files = vec![("user", home.join("settings.json"))];
        if let Some(project) = project {
            files.push(("project", project.join(".bubble").join("settings.json")));
            files.push(("local", project.join(".bubble").join("settings.local.json")));
        }
        for (scope, path) in files {
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            let raw: Value = match serde_json::from_str(&text) {
                Ok(raw) => raw,
                Err(error) => {
                    hooks.diagnostics.push(format!("{}: failed to parse hooks settings: {error}", path.display()));
                    continue;
                }
            };
            let rules = match &raw["hooks"] {
                Value::Null => continue,
                Value::Array(rules) => rules.clone(),
                Value::Object(settings) => {
                    if settings.get("enabled") == Some(&Value::Bool(false)) {
                        continue;
                    }
                    match settings.get("rules") {
                        Some(Value::Array(rules)) => rules.clone(),
                        _ => {
                            hooks.diagnostics.push(format!("{}: ignored hooks.rules - expected array.", path.display()));
                            continue;
                        }
                    }
                }
                _ => {
                    hooks.diagnostics.push(format!("{}: ignored hooks setting - expected object or array.", path.display()));
                    continue;
                }
            };
            for (index, raw) in rules.iter().enumerate() {
                match parse_rule(raw, scope, &path, index) {
                    Ok(rule) => hooks.rules.push(rule),
                    Err(message) => hooks.diagnostics.push(format!("{}: {message}", path.display())),
                }
            }
        }
        let scope_order = |s: &str| match s {
            "user" => 0,
            "project" => 1,
            _ => 2,
        };
        hooks.rules.sort_by(|a, b| {
            b.priority.cmp(&a.priority).then(scope_order(a.scope).cmp(&scope_order(b.scope))).then(a.index.cmp(&b.index))
        });
        hooks
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// Runs the matching hooks in order; the first deny on a blockable event
    /// stops the rest.
    pub async fn run(&self, role: Role, event: Event<'_>) -> Outcome {
        let mut outcome = Outcome::default();
        let candidates: Vec<&Rule> = self
            .rules
            .iter()
            .filter(|rule| rule.events.iter().any(|e| e == event.name))
            .filter(|rule| role == Role::Parent || rule.inherit_to_subagents)
            .filter(|rule| match &rule.matcher {
                Some(matcher) => {
                    let target = if event.target.is_empty() { event.payload.to_string() } else { event.target.to_owned() };
                    matcher.is_match(&target)
                }
                None => true,
            })
            .collect();
        outcome.matched = candidates.len();
        let blockable = BLOCKABLE.contains(&event.name);
        for rule in candidates {
            let envelope = envelope(rule, role, &event);
            let result = run_command(rule, &envelope, event.cwd).await;
            self.log(&event, rule, &result);
            outcome.context.extend(result.context);
            if let Some(reason) = result.denied {
                if blockable {
                    outcome.denied = Some(reason);
                    break;
                }
            }
        }
        outcome
    }

    fn log(&self, event: &Event<'_>, rule: &Rule, result: &RunResult) {
        let now = crate::time::now_utc();
        let dir = self.home.join("hooks");
        let entry = json!({
            "ts": now.iso,
            "level": if result.error.is_some() { if result.denied.is_some() { "error" } else { "warn" } } else { "info" },
            "eventName": event.name,
            "hookId": rule.id,
            "decision": if result.denied.is_some() { "deny" } else { "allow" },
            "message": result.error.clone().or_else(|| result.denied.clone()).unwrap_or_else(|| "Hook completed.".into()),
            "elapsedMs": result.elapsed_ms,
        });
        let _ = std::fs::create_dir_all(&dir);
        if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join(format!("{}.jsonl", now.date))) {
            use std::io::Write;
            let _ = writeln!(file, "{entry}");
        }
    }
}

fn looks_like_path(value: &str) -> bool {
    value.starts_with('.') || value.starts_with('/') || value.contains('/')
}

fn parse_rule(raw: &Value, scope: &'static str, path: &Path, index: usize) -> Result<Rule, String> {
    if !raw.is_object() {
        return Err(format!("ignored hook rule at index {index} - expected object."));
    }
    let mut events: Vec<String> = Vec::new();
    let listed: Vec<&Value> = match (&raw["events"], &raw["event"]) {
        (Value::Array(list), _) => list.iter().collect(),
        (_, Value::Null) => Vec::new(),
        (_, single) => vec![single],
    };
    for value in listed {
        if let Some(name) = value.as_str().filter(|n| EVENTS.contains(n)) {
            if !events.iter().any(|e| e == name) {
                events.push(name.to_owned());
            }
        }
    }
    if events.is_empty() {
        return Err("ignored hook rule - event must be a known hook event.".into());
    }
    let command = raw["command"].as_str().map(str::trim).unwrap_or_default();
    if command.is_empty() {
        return Err("ignored hook rule - command must be a non-empty string.".into());
    }
    if command.contains(['\0', '\r', '\n']) {
        return Err("ignored hook rule - command must not contain control characters.".into());
    }
    if scope == "project" && !looks_like_path(command) {
        return Err("ignored project hook rule - project hook command must be an absolute or relative executable path.".into());
    }
    let base = path.parent().unwrap_or(Path::new("/"));
    let resolve = |value: &str| {
        let expanded = expand_home(value);
        if expanded.is_absolute() { expanded } else { base.join(expanded) }
    };
    let command = if looks_like_path(command) { resolve(command).display().to_string() } else { command.to_owned() };
    let matcher = match raw["matcher"].as_str().map(str::trim).filter(|m| !m.is_empty()) {
        Some(pattern) => {
            Some(regex::Regex::new(pattern).map_err(|e| format!("ignored hook rule - invalid matcher regex: {e}"))?)
        }
        None => None,
    };
    let id = raw["id"]
        .as_str()
        .or(raw["name"].as_str())
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| format!("{scope}:{index}:{}:{command}", events.join("+")));
    let strings = |value: &Value| -> Vec<String> {
        value.as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect()).unwrap_or_default()
    };
    let clamp = |value: &Value, fallback: u64, min: u64, max: u64| -> u64 {
        value.as_f64().filter(|n| n.is_finite()).map(|n| (n.floor().max(min as f64) as u64).min(max)).unwrap_or(fallback)
    };
    let env = raw["env"]
        .as_object()
        .map(|o| {
            o.iter()
                .filter(|(k, _)| k.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_'))
                .filter(|(k, _)| k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
                .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_owned())))
                .collect()
        })
        .unwrap_or_default();
    let on_error = raw.get("onError").or(raw.get("failurePolicy"));
    Ok(Rule {
        id,
        events,
        matcher,
        command,
        args: strings(&raw["args"]),
        cwd: raw["cwd"].as_str().map(str::trim).filter(|c| !c.is_empty()).map(resolve),
        env,
        timeout: Duration::from_millis(clamp(&raw["timeoutMs"], DEFAULT_TIMEOUT_MS, 50, MAX_TIMEOUT_MS)),
        max_output: clamp(&raw["maxOutputBytes"], DEFAULT_MAX_OUTPUT as u64, 1024, MAX_OUTPUT as u64) as usize,
        block_on_error: on_error.and_then(Value::as_str) == Some("block"),
        include: strings(&raw["include"]),
        expose_to_model: raw["exposeToModel"] == true,
        inherit_to_subagents: raw["inheritToSubagents"] == true,
        priority: raw["priority"].as_f64().map(|p| p.floor().clamp(-10_000.0, 10_000.0) as i64).unwrap_or(0),
        scope,
        index,
    })
}

fn expand_home(value: &str) -> PathBuf {
    match value.strip_prefix("~/") {
        Some(rest) => dirs::home_dir().unwrap_or_default().join(rest),
        None => PathBuf::from(value),
    }
}

fn envelope(rule: &Rule, role: Role, event: &Event<'_>) -> Value {
    let mut payload = event.payload.as_object().cloned().unwrap_or_default();
    let mut redacted = Vec::new();
    if let Some(full) = event.full.as_object() {
        for (key, value) in full {
            if rule.include.iter().any(|i| i == "all" || i == key) {
                payload.insert(key.clone(), value.clone());
            } else {
                redacted.push(key.clone());
            }
        }
    }
    json!({
        "schemaVersion": 1,
        "eventName": event.name,
        "eventId": crate::time::unique_id(),
        "timestamp": crate::time::now_utc().iso,
        "cwd": event.cwd,
        "agentRole": role.label(),
        "target": event.target,
        "payload": payload,
        "redacted": redacted,
    })
}

struct RunResult {
    denied: Option<String>,
    context: Vec<String>,
    error: Option<String>,
    elapsed_ms: u128,
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max { text.to_owned() } else { format!("{}...", text.chars().take(max).collect::<String>()) }
}

fn first_non_empty(values: &[&str], fallback: &str) -> String {
    values.iter().map(|v| v.trim()).find(|v| !v.is_empty()).map(|v| truncate(v, 1000)).unwrap_or_else(|| fallback.to_owned())
}

async fn run_command(rule: &Rule, envelope: &Value, cwd: &Path) -> RunResult {
    let started = Instant::now();
    let fail = |error: String| RunResult {
        denied: rule.block_on_error.then(|| error.clone()),
        context: Vec::new(),
        error: Some(error),
        elapsed_ms: started.elapsed().as_millis(),
    };
    let mut command = Command::new(&rule.command);
    command
        .args(&rule.args)
        .current_dir(rule.cwd.as_deref().unwrap_or(cwd))
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    crate::detach_async(&mut command);
    for key in ["PATH", "HOME", "TMPDIR", "LANG", "LC_ALL", "SHELL"] {
        if let Some(value) = crate::mcp::shell_env().get(key) {
            command.env(key, value);
        }
    }
    command
        .envs(&rule.env)
        .env("BUBBLE_HOOK_DEPTH", "1")
        .env("BUBBLE_HOOK_ID", &rule.id)
        .env("BUBBLE_HOOK_EVENT", envelope["eventName"].as_str().unwrap_or_default())
        .env("BUBBLE_HOOK_EVENT_ID", envelope["eventId"].as_str().unwrap_or_default())
        .env("BUBBLE_HOOK_CWD", cwd);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => return fail(error.to_string()),
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(envelope.to_string().as_bytes()).await;
    }
    let mut stdout_pipe = child.stdout.take();
    let mut stderr_pipe = child.stderr.take();
    let limit = rule.max_output;
    let read_all = async {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let read_out = async {
            if let Some(pipe) = stdout_pipe.as_mut() {
                let _ = pipe.take(limit as u64 + 1).read_to_end(&mut out).await;
            }
        };
        let read_err = async {
            if let Some(pipe) = stderr_pipe.as_mut() {
                let _ = pipe.take(limit as u64 + 1).read_to_end(&mut err).await;
            }
        };
        tokio::join!(read_out, read_err);
        let status = child.wait().await;
        (out, err, status)
    };
    let (out, err, status) = match tokio::time::timeout(rule.timeout, read_all).await {
        Ok(done) => done,
        Err(_) => return fail(format!("Hook timed out after {}ms.", rule.timeout.as_millis())),
    };
    if out.len() > limit || err.len() > limit {
        return fail("Hook output exceeded the configured limit.".into());
    }
    let stdout = String::from_utf8_lossy(&out).into_owned();
    let stderr = String::from_utf8_lossy(&err).into_owned();
    let code = status.ok().and_then(|s| s.code());
    let parsed: Option<Value> = if stdout.trim().is_empty() {
        Some(json!({}))
    } else {
        serde_json::from_str::<Value>(stdout.trim()).ok().filter(Value::is_object)
    };
    let Some(parsed) = parsed else {
        return fail("Hook stdout was not valid JSON.".into());
    };
    let visible = parsed["visibleToModel"] == true || parsed["exposeToModel"] == true || rule.expose_to_model;
    let context: Vec<String> = if visible {
        match parsed.get("modelContext").or(parsed.get("context")) {
            Some(Value::String(text)) => vec![truncate(text, 4000)],
            Some(Value::Array(items)) => {
                items.iter().filter_map(|i| i.as_str()).map(|t| truncate(t, 4000)).take(8).collect()
            }
            _ => Vec::new(),
        }
    } else {
        Vec::new()
    };
    let decision = parsed["decision"].as_str();
    let reason = parsed["reason"].as_str().map(str::to_owned);
    let elapsed_ms = started.elapsed().as_millis();
    if code == Some(2) || matches!(decision, Some("deny" | "block")) {
        let reason = reason.unwrap_or_else(|| first_non_empty(&[&stderr, &stdout], "Hook denied the event."));
        return RunResult { denied: Some(reason), context, error: None, elapsed_ms };
    }
    if let Some(code) = code.filter(|c| *c != 0) {
        let error = first_non_empty(&[&stderr, &stdout], &format!("Hook exited with code {code}."));
        return RunResult { denied: rule.block_on_error.then(|| error.clone()), context, error: Some(error), elapsed_ms };
    }
    RunResult { denied: None, context, error: None, elapsed_ms }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT_DIR: AtomicUsize = AtomicUsize::new(0);

    fn temp() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("bubble-hooks-{}-{}", std::process::id(), NEXT_DIR.fetch_add(1, Ordering::SeqCst)));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn event<'a>(name: &'a str, cwd: &'a Path, target: &'a str) -> Event<'a> {
        Event { name, cwd, target, payload: json!({ "name": target }), full: json!({ "toolArgs": { "command": "ls" } }) }
    }

    #[test]
    fn rules_parse_sort_and_reject_bad_ones() {
        let home = temp();
        let project = temp();
        std::fs::write(
            home.join("settings.json"),
            json!({ "hooks": { "rules": [
                { "event": "Stop", "command": "/bin/true" },
                { "event": "PreToolUse", "command": "/bin/true", "priority": 5, "matcher": "^bash$" },
                { "event": "Nope", "command": "/bin/true" },
                { "event": "Stop", "command": "/bin/true", "matcher": "(" },
            ]}})
            .to_string(),
        )
        .unwrap();
        std::fs::create_dir_all(project.join(".bubble")).unwrap();
        std::fs::write(
            project.join(".bubble/settings.json"),
            json!({ "hooks": [{ "event": "Stop", "command": "check" }, { "event": "Stop", "command": "./check.sh" }] }).to_string(),
        )
        .unwrap();
        let hooks = Hooks::load(&home, Some(&project));
        assert_eq!(hooks.rules.len(), 3);
        assert_eq!(hooks.rules[0].priority, 5);
        assert_eq!(hooks.rules[2].command, project.join(".bubble/./check.sh").display().to_string());
        assert_eq!(hooks.diagnostics.len(), 3);
    }

    #[tokio::test]
    async fn exit_two_denies_blockable_events_only() {
        let dir = temp();
        let deny = script(&dir, "deny.sh", "cat > \"$(dirname \"$0\")/seen.json\"; echo 'not allowed' >&2; exit 2");
        let rule = |event: &str| json!({ "event": event, "command": deny, "include": ["toolArgs"] });
        std::fs::write(dir.join("settings.json"), json!({ "hooks": [rule("PreToolUse"), rule("PostToolUse")] }).to_string()).unwrap();
        let hooks = Hooks::load(&dir, None);
        let outcome = hooks.run(Role::Parent, event("PreToolUse", &dir, "bash")).await;
        assert_eq!(outcome.denied.as_deref(), Some("not allowed"));
        let seen: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("seen.json")).unwrap()).unwrap();
        assert_eq!(seen["eventName"], "PreToolUse");
        assert_eq!(seen["payload"]["toolArgs"]["command"], "ls");
        assert_eq!(seen["agentRole"], "parent");
        let observe = hooks.run(Role::Parent, event("PostToolUse", &dir, "bash")).await;
        assert_eq!(observe.denied, None, "observe-only events cannot deny");
        // Subagents only run rules that are inherited.
        assert_eq!(hooks.run(Role::Subagent, event("PreToolUse", &dir, "bash")).await.matched, 0);
        assert!(std::fs::read_dir(dir.join("hooks")).unwrap().count() > 0, "runs are logged");
    }

    #[tokio::test]
    async fn json_output_adds_context_and_failures_follow_the_policy() {
        let dir = temp();
        let context = script(&dir, "context.sh", r#"echo '{"modelContext": "Remember the style guide.", "exposeToModel": true}'"#);
        let broken = script(&dir, "broken.sh", "echo not json");
        let slow = script(&dir, "slow.sh", "sleep 5");
        std::fs::write(
            dir.join("settings.json"),
            json!({ "hooks": [
                { "event": "UserPromptSubmit", "command": context },
                { "event": "PreCompact", "command": broken },
                { "event": "PreToolUse", "command": slow, "timeoutMs": 100, "onError": "block" },
            ]})
            .to_string(),
        )
        .unwrap();
        let hooks = Hooks::load(&dir, None);
        let outcome = hooks.run(Role::Parent, event("UserPromptSubmit", &dir, "hello")).await;
        assert_eq!(outcome.context, ["Remember the style guide."]);
        assert_eq!(hooks.run(Role::Parent, event("PreCompact", &dir, "")).await.denied, None, "errors allow by default");
        let slow = hooks.run(Role::Parent, event("PreToolUse", &dir, "bash")).await;
        assert!(slow.denied.unwrap().contains("timed out"));
    }
}
