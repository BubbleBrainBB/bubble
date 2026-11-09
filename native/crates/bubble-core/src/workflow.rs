//! `run_workflow`: a JavaScript orchestration script the model writes, run
//! in a QuickJS sandbox, whose one capability is `agent()` — a read-only
//! subagent (src/agent/workflow/ in the TypeScript core). `parallel`,
//! `pipeline`, `phase`, `log`, `args` and `budget` are a prelude over it.
//!
//! The VM lives on its own thread. `agent()` asks the host for an id and
//! parks a JS promise under it; the host runs the child on the async
//! runtime and settles that id when it finishes, so many agents run at once
//! and failures stay inside the script. Unlike the TypeScript tool, this
//! one blocks until the workflow ends (Stop cancels it).

use std::cell::Cell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rquickjs::{CatchResultExt, Context, Ctx, Function, Runtime};
use serde_json::{Value, json};

use crate::delegate::{Children, SpawnOptions, Status};
use crate::model::Level;
use crate::provider::{ToolCall, ToolSpec};
use crate::tools::ToolOutput;

pub const TOOL: &str = "run_workflow";
const MAX_AGENTS: u64 = 1000;
/// How long the script may compute between awaits.
const COMPUTE_DEADLINE: Duration = Duration::from_secs(10);
const RESULT_CHARS: usize = 8000;

const PRELUDE: &str = r#"
globalThis.__pending = {};
globalThis.__settle = (id, ok, payload) => {
  const waiter = __pending[id];
  delete __pending[id];
  if (!waiter) return;
  if (ok) waiter[0](payload); else waiter[1](new Error(payload));
};
globalThis.agent = (prompt, opts) => new Promise((resolve, reject) => {
  const id = __agent(JSON.stringify({ prompt: String(prompt ?? ""), opts: opts || {} }));
  __pending[id] = [resolve, reject];
}).then((raw) => JSON.parse(raw));
globalThis.parallel = (thunks) => {
  if (!Array.isArray(thunks)) throw new TypeError("parallel() expects an array of functions");
  if (thunks.some((t) => typeof t !== "function")) {
    throw new TypeError("parallel() expects an array of functions, not promises. Wrap each call: () => agent(...)");
  }
  return Promise.all(thunks.map((t) => {
    try { return Promise.resolve(t()).catch(() => null); } catch (_e) { return Promise.resolve(null); }
  }));
};
globalThis.pipeline = (items, ...stages) => {
  if (!Array.isArray(items)) throw new TypeError("pipeline() expects an array as the first argument");
  if (stages.some((s) => typeof s !== "function")) {
    throw new TypeError("pipeline() stages must be functions: pipeline(items, item => ..., result => ...)");
  }
  return Promise.all(items.map(async (item, i) => {
    let v = item;
    for (const stage of stages) {
      try { v = await stage(v, item, i); } catch (_e) { return null; }
    }
    return v;
  }));
};
globalThis.phase = (t) => __phase(String(t ?? ""));
globalThis.log = (m) => __log(String(m ?? ""));
globalThis.budget = {
  get total() { return null; },
  spent() { return __spent(); },
  remaining() { return Number.MAX_SAFE_INTEGER; },
};
"#;

/// Removes ambient nondeterminism so a run is reproducible.
const DETERMINISM: &str = r#"
delete globalThis.Date;
Object.defineProperty(globalThis, "Date", {
  configurable: false,
  get() { throw new Error("Workflow scripts must be deterministic: Date.now()/new Date() are unavailable — pass timestamps in via args"); },
});
delete globalThis.WeakRef;
delete globalThis.FinalizationRegistry;
Math.random = () => { throw new Error("Workflow scripts must be deterministic: Math.random() is unavailable"); };
"#;

pub fn spec() -> ToolSpec {
    ToolSpec {
        name: TOOL.into(),
        description: [
            "Run an LLM-authored JavaScript orchestration script (dynamic workflow) that coordinates many subagents with deterministic control flow.",
            "This is THE tool whenever the user asks for a workflow, an orchestration, or an agent team — even a small one; do not substitute parallel spawn_agent calls for an explicit request.",
            "Also use it for tasks that need loops, conditional fan-out, or staged pipelines over dozens of subagents whose intermediate steps should stay out of this conversation — e.g. a codebase-wide audit, a migration, or cross-checked research.",
            "The script's only capability is agent(prompt, opts?) — each call spawns a sandboxed readonly subagent; opts may set {model, effort, agentType, category, schema, label}. Give each agent a short unique label. Also available: parallel(thunks), pipeline(items, ...stages), phase(title), log(msg), the global args, and budget {total (null: no limit), spent() (member tokens so far), remaining()}.",
            "End the script with `return <value>`; that value (only) comes back to you. Return distilled conclusions, not raw agent transcripts — reduce inside the script (plain JS filtering or a final synthesis agent); a return value past ~8000 chars reaches you truncated. The script has no filesystem/shell/network/clock/random access. run_workflow must be the ONLY tool call in your response; it blocks until the workflow finishes.",
            "A failed or blocked agent() resolves to null inside parallel/pipeline (a bare await agent() throws instead): check for null slots and return which items failed alongside the results — never silently drop them. Keep a final synthesis/aggregation step inside try/catch (or parallel) so its failure cannot discard every completed member's work.",
            "Write plain JavaScript only — no TypeScript syntax, no import/require, and no Date or Math.random (they throw; pass timestamps in via args).",
            "parallel() takes an array of FUNCTIONS, not promises: await parallel(items.map(item => () => agent(...))). Passing promises throws a TypeError.",
            "Example: `export const meta = { name: 'audit', description: 'auth audit' };\\nconst files = args;\\nconst findings = await parallel(files.map(f => () => agent('Audit '+f+' for missing auth', { label: f, schema: SCHEMA })));\\nreturn findings.filter(Boolean);`",
        ]
        .join(" "),
        parameters: json!({
            "type": "object",
            "properties": {
                "script": { "type": "string", "description": "The JavaScript orchestration script. Starts with `export const meta = {name, description}`; ends with `return <value>`." },
                "args": { "description": "Optional JSON value exposed to the script as the global `args` (e.g. a list of target paths or a question)." },
                "title": { "type": "string", "description": "Optional short label shown in the UI." },
            },
            "required": ["script"],
            "additionalProperties": false,
        }),
    }
}

/// `export const meta = …` and `export function …` as plain declarations.
fn strip_exports(script: &str) -> String {
    use std::sync::OnceLock;
    static EXPORT: OnceLock<regex::Regex> = OnceLock::new();
    EXPORT
        .get_or_init(|| regex::Regex::new(r"(^|\n)\s*export\s+(const|let|var|function|class|async)\b").unwrap())
        .replace_all(script, "$1$2")
        .into_owned()
}

/// The source the VM runs: the script as an async function body.
fn wrapped(script: &str) -> String {
    [
        "globalThis.__wfdone = false; globalThis.__wfresult = null; globalThis.__wferror = null;",
        "(async () => {",
        &strip_exports(script),
        "})().then(",
        "  (r) => { globalThis.__wfresult = JSON.stringify(r === undefined ? null : r); globalThis.__wfdone = true; },",
        "  (e) => { globalThis.__wferror = (e && e.message) ? String(e.message) : String(e); globalThis.__wfdone = true; }",
        ");",
    ]
    .join("\n")
}

/// Compiles without running, so a broken script fails at once.
pub fn precheck(script: &str) -> Result<(), String> {
    let runtime = Runtime::new().map_err(|e| e.to_string())?;
    let context = Context::full(&runtime).map_err(|e| e.to_string())?;
    context.with(|ctx| {
        // Defining the async function compiles the body without calling it.
        let source = format!("(async () => {{\n{}\n}})", strip_exports(script));
        ctx.eval::<(), _>(source).catch(&ctx).map_err(|e| format!("workflow script error: {e}"))
    })
}

/// One `agent()` call.
#[derive(Clone, Debug, PartialEq)]
pub struct AgentSpec {
    pub prompt: String,
    pub opts: Value,
}

type Dispatch = Arc<dyn Fn(AgentSpec) -> futures_util::future::BoxFuture<'static, Result<Value, String>> + Send + Sync>;
/// The tokens the members used so far (`budget.spent()`; there is no limit,
/// as in the TypeScript runtime: a child stops only when its task is done).
pub type Spent = Arc<dyn Fn() -> f64 + Send + Sync>;

/// Runs a script to its end: its return value, or why it failed.
pub async fn run(
    script: String,
    args: Value,
    dispatch: Dispatch,
    logs: Arc<Mutex<Vec<String>>>,
    spent: Spent,
) -> Result<Value, String> {
    let handle = tokio::runtime::Handle::current();
    let cancel = Arc::new(AtomicBool::new(false));
    let (done, result) = tokio::sync::oneshot::channel();
    let flag = cancel.clone();
    std::thread::Builder::new()
        .name("bubble-workflow".into())
        .spawn(move || {
            let _ = done.send(run_vm(&script, &args, handle, dispatch, flag, logs, spent));
        })
        .map_err(|e| e.to_string())?;
    // Dropping this future (Stop) ends the VM.
    struct Cancel(Arc<AtomicBool>);
    impl Drop for Cancel {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    let _cancel = Cancel(cancel);
    result.await.unwrap_or_else(|_| Err("the workflow stopped".into()))
}

fn run_vm(
    script: &str,
    args: &Value,
    handle: tokio::runtime::Handle,
    dispatch: Dispatch,
    cancel: Arc<AtomicBool>,
    logs: Arc<Mutex<Vec<String>>>,
    spent: Spent,
) -> Result<Value, String> {
    let runtime = Runtime::new().map_err(|e| e.to_string())?;
    runtime.set_memory_limit(256 * 1024 * 1024);
    let computing_since = Arc::new(Mutex::new(Instant::now()));
    {
        let since = computing_since.clone();
        let cancel = cancel.clone();
        runtime.set_interrupt_handler(Some(Box::new(move || {
            cancel.load(Ordering::SeqCst) || since.lock().map(|s| s.elapsed() > COMPUTE_DEADLINE).unwrap_or(false)
        })));
    }
    let context = Context::full(&runtime).map_err(|e| e.to_string())?;
    let (settled_tx, settled_rx) = std::sync::mpsc::channel::<(u64, Result<Value, String>)>();
    let outstanding = Rc::new(Cell::new(0usize));
    let count = Rc::new(Cell::new(0u64));

    context.with(|ctx| -> Result<(), String> {
        let fail = |e: rquickjs::CaughtError<'_>| format!("workflow script error: {e}");
        ctx.eval::<(), _>(DETERMINISM).catch(&ctx).map_err(fail)?;
        let globals = ctx.globals();
        {
            let outstanding = outstanding.clone();
            let count = count.clone();
            let sender = settled_tx.clone();
            let handle = handle.clone();
            let dispatch = dispatch.clone();
            let agent = Function::new(ctx.clone(), move |ctx: Ctx<'_>, spec: String| -> rquickjs::Result<f64> {
                if count.get() >= MAX_AGENTS {
                    return Err(rquickjs::Exception::throw_message(&ctx, &format!("workflow exceeded the {MAX_AGENTS}-agent cap")));
                }
                count.set(count.get() + 1);
                let id = count.get();
                outstanding.set(outstanding.get() + 1);
                let spec: Value = serde_json::from_str(&spec).unwrap_or_default();
                let spec = AgentSpec { prompt: spec["prompt"].as_str().unwrap_or_default().to_owned(), opts: spec["opts"].clone() };
                let sender = sender.clone();
                let future = dispatch(spec);
                handle.spawn(async move {
                    let _ = sender.send((id, future.await));
                });
                Ok(id as f64)
            })
            .map_err(|e| e.to_string())?;
            globals.set("__agent", agent).map_err(|e| e.to_string())?;
        }
        {
            let logs = logs.clone();
            let phase = Function::new(ctx.clone(), move |title: String| {
                if let Ok(mut logs) = logs.lock() {
                    logs.push(format!("== {title}"));
                }
            })
            .map_err(|e| e.to_string())?;
            globals.set("__phase", phase).map_err(|e| e.to_string())?;
        }
        {
            let logs = logs.clone();
            let log = Function::new(ctx.clone(), move |message: String| {
                if let Ok(mut logs) = logs.lock() {
                    logs.push(message);
                }
            })
            .map_err(|e| e.to_string())?;
            globals.set("__log", log).map_err(|e| e.to_string())?;
        }
        {
            let spent = spent.clone();
            let total = Function::new(ctx.clone(), move || spent()).map_err(|e| e.to_string())?;
            globals.set("__spent", total).map_err(|e| e.to_string())?;
        }
        ctx.eval::<(), _>(format!("globalThis.args = {};", serde_json::to_string(args).unwrap_or("null".into())))
            .catch(&ctx)
            .map_err(fail)?;
        ctx.eval::<(), _>(PRELUDE).catch(&ctx).map_err(fail)?;
        if let Ok(mut since) = computing_since.lock() {
            *since = Instant::now();
        }
        ctx.eval::<(), _>(wrapped(script)).catch(&ctx).map_err(fail)?;
        Ok(())
    })?;
    drop(settled_tx);

    let is_done = || context.with(|ctx| ctx.globals().get::<_, bool>("__wfdone").unwrap_or(false));
    loop {
        if let Ok(mut since) = computing_since.lock() {
            *since = Instant::now();
        }
        loop {
            match runtime.execute_pending_job() {
                Ok(true) => continue,
                Ok(false) => break,
                // An exception inside a job rejects its promise; the script sees it.
                Err(_) => continue,
            }
        }
        if is_done() {
            break;
        }
        if cancel.load(Ordering::SeqCst) {
            return Err("workflow aborted".into());
        }
        if outstanding.get() == 0 && !runtime.is_job_pending() {
            break;
        }
        match settled_rx.recv_timeout(Duration::from_millis(200)) {
            Ok((id, result)) => {
                outstanding.set(outstanding.get().saturating_sub(1));
                context.with(|ctx| {
                    let settle: Function = ctx.globals().get("__settle")?;
                    let (ok, payload) = match result {
                        Ok(value) => (true, serde_json::to_string(&value).unwrap_or("null".into())),
                        Err(error) => (false, error),
                    };
                    settle.call::<_, ()>((id as f64, ok, payload))
                })
                .map_err(|e| e.to_string())?;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    context.with(|ctx| {
        let globals = ctx.globals();
        if !globals.get::<_, bool>("__wfdone").unwrap_or(false) {
            return Err("the workflow stopped without finishing (an agent() was never awaited, or the script stalled)".into());
        }
        if let Ok(Some(error)) = globals.get::<_, Option<String>>("__wferror") {
            if !error.is_empty() {
                return Err(error);
            }
        }
        let raw: Option<String> = globals.get("__wfresult").unwrap_or(None);
        Ok(raw.and_then(|r| serde_json::from_str(&r).ok()).unwrap_or(Value::Null))
    })
}

/// The JSON value in a child's answer (for `schema` agents).
fn json_in(text: &str) -> Option<Value> {
    let trimmed = text.trim();
    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        return Some(value);
    }
    let unfenced = trimmed
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    if let Ok(value) = serde_json::from_str::<Value>(unfenced) {
        return Some(value);
    }
    let start = trimmed.find(['{', '['])?;
    let end = trimmed.rfind(['}', ']'])?;
    serde_json::from_str(&trimmed[start..=end]).ok()
}

fn title_of(script: &str, given: Option<&str>) -> String {
    if let Some(title) = given.map(str::trim).filter(|t| !t.is_empty()) {
        return title.to_owned();
    }
    regex::Regex::new(r#"meta\s*=\s*\{[^}]*name\s*:\s*['"]([^'"]+)['"]"#)
        .ok()
        .and_then(|re| re.captures(script).map(|c| c[1].to_owned()))
        .unwrap_or_else(|| "workflow".into())
}

/// The `run_workflow` tool.
pub async fn execute(children: &Arc<Children>, call: &ToolCall) -> ToolOutput {
    let args = call.arguments_json();
    let Some(script) = args["script"].as_str().filter(|s| !s.trim().is_empty()).map(str::to_owned) else {
        return ToolOutput { content: "Error: run_workflow requires a non-empty script.".into(), is_error: true };
    };
    if let Err(error) = precheck(&script) {
        return ToolOutput {
            content: format!("run_workflow rejected before launch: {error}. Fix the script and re-issue run_workflow."),
            is_error: true,
        };
    }
    let title = title_of(&script, args["title"].as_str());
    let members: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let dispatch: Dispatch = {
        let children = children.clone();
        let members = members.clone();
        Arc::new(move |spec: AgentSpec| {
            let children = children.clone();
            let members = members.clone();
            Box::pin(async move {
                let opts = &spec.opts;
                let text = |key: &str| opts[key].as_str().map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned);
                let effort = match text("effort") {
                    Some(level) => Some(Level::parse(&level).ok_or_else(|| format!("unknown effort {level}"))?),
                    None => None,
                };
                let schema = opts.get("schema").filter(|s| !s.is_null()).cloned();
                let snapshot = children.spawn(&spec.prompt, SpawnOptions {
                    profile: text("agentType").unwrap_or_else(|| "default".into()),
                    description: text("label"),
                    model: text("model"),
                    effort,
                    nickname: text("label"),
                    fork_context: None,
                    schema: schema.clone(),
                    workflow: true,
                    category: text("category"),
                })?;
                if let Ok(mut members) = members.lock() {
                    members.push(snapshot.id.clone());
                }
                let done = loop {
                    let found = children.wait(Some(vec![snapshot.id.clone()]), Duration::from_secs(600)).await;
                    match found.into_iter().next() {
                        Some(s) if s.status.is_final() => break s,
                        Some(_) => continue,
                        None => return Err("the agent disappeared".into()),
                    }
                };
                match done.status {
                    Status::Completed if schema.is_some() => json_in(&done.summary)
                        .ok_or_else(|| format!("{} did not return JSON matching the schema", done.nickname)),
                    Status::Completed => Ok(Value::String(done.summary)),
                    other => Err(done.error.unwrap_or_else(|| format!("{} {}", done.nickname, other.label()))),
                }
            })
        })
    };
    // Members still running when the workflow is stopped are closed.
    struct CloseMembers(Arc<Children>, Arc<Mutex<Vec<String>>>);
    impl Drop for CloseMembers {
        fn drop(&mut self) {
            for id in self.1.lock().map(|m| m.clone()).unwrap_or_default() {
                if self.0.snapshot(&id).is_some_and(|s| !s.status.is_final()) {
                    let _ = self.0.close(&id);
                }
            }
        }
    }
    let _close = CloseMembers(children.clone(), members.clone());
    let logs = Arc::new(Mutex::new(Vec::new()));
    let spent: Spent = {
        let children = children.clone();
        let members = members.clone();
        Arc::new(move || {
            let ids = members.lock().map(|m| m.clone()).unwrap_or_default();
            ids.iter().filter_map(|id| children.snapshot(id)).map(|s| s.tokens as f64).sum()
        })
    };
    let result = run(script, args["args"].clone(), dispatch, logs.clone(), spent).await;
    let logs = logs.lock().map(|l| l.clone()).unwrap_or_default();
    let agents = members.lock().map(|m| m.len()).unwrap_or(0);
    let failed = members
        .lock()
        .map(|m| m.iter().filter(|id| children.snapshot(id).is_some_and(|s| matches!(s.status, Status::Failed | Status::Blocked | Status::Cancelled))).count())
        .unwrap_or(0);
    let log_lines = if logs.is_empty() {
        String::new()
    } else {
        let start = logs.len().saturating_sub(20);
        format!("\n\nLog:\n{}", logs[start..].join("\n"))
    };
    match result {
        Err(error) => ToolOutput { content: format!("workflow \"{title}\" failed: {error}{log_lines}"), is_error: true },
        Ok(value) => {
            let text = match &value {
                Value::String(s) => s.clone(),
                other => serde_json::to_string_pretty(other).unwrap_or_default(),
            };
            let shown = if text.chars().count() > RESULT_CHARS {
                format!(
                    "{}\n[truncated: {} more chars]",
                    text.chars().take(RESULT_CHARS).collect::<String>(),
                    text.chars().count() - RESULT_CHARS
                )
            } else {
                text
            };
            let warning =
                if failed > 0 { format!("\n{failed} of {agents} agents failed; their slots are null in the result.") } else { String::new() };
            ToolOutput {
                content: format!("workflow \"{title}\" completed ({agents} agents).{warning}{log_lines}\n\nResult:\n{shown}"),
                is_error: false,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn echo_dispatch() -> Dispatch {
        Arc::new(|spec: AgentSpec| {
            Box::pin(async move {
                tokio::time::sleep(Duration::from_millis(20)).await;
                if spec.prompt.contains("fail") {
                    Err("it failed".into())
                } else {
                    Ok(json!(format!("did {}", spec.prompt)))
                }
            })
        })
    }

    #[tokio::test]
    async fn scripts_fan_out_and_return_values() {
        let logs = Arc::new(Mutex::new(Vec::new()));
        let script = r#"export const meta = { name: 'fan' };
phase('work');
const results = await parallel(args.map((a) => () => agent(a)));
log('done ' + results.length);
return { results, piped: await pipeline(['x'], (v) => agent(v + '1'), (r) => r.toUpperCase()) };"#;
        let value = run(script.into(), json!(["a", "fail b", "c"]), echo_dispatch(), logs.clone(), Arc::new(|| 0.0)).await.unwrap();
        assert_eq!(value, json!({ "results": ["did a", null, "did c"], "piped": ["DID X1"] }));
        assert_eq!(*logs.lock().unwrap(), ["== work", "done 3"]);
    }

    #[tokio::test]
    async fn bare_failures_throw_and_nondeterminism_is_blocked() {
        let logs = Arc::new(Mutex::new(Vec::new()));
        let failed = run("return await agent('fail');".into(), Value::Null, echo_dispatch(), logs.clone(), Arc::new(|| 0.0)).await;
        assert_eq!(failed, Err("it failed".into()));
        let date = run("return Date.now();".into(), Value::Null, echo_dispatch(), logs.clone(), Arc::new(|| 0.0)).await;
        assert!(date.unwrap_err().contains("deterministic"));
        let promises = run("return await parallel([agent('a')]);".into(), Value::Null, echo_dispatch(), logs, Arc::new(|| 0.0)).await;
        assert!(promises.unwrap_err().contains("array of functions"));
    }

    #[tokio::test]
    async fn runaway_loops_are_interrupted() {
        let started = Instant::now();
        let result = run("while (true) {}".into(), Value::Null, echo_dispatch(), Arc::new(Mutex::new(Vec::new())), Arc::new(|| 0.0)).await;
        assert!(result.is_err());
        assert!(started.elapsed() < COMPUTE_DEADLINE + Duration::from_secs(5));
    }

    #[test]
    fn syntax_errors_fail_the_precheck_without_running() {
        assert!(precheck("export const meta = { name: 'x' };\nreturn 1;").is_ok());
        assert!(precheck("const x: number = 1;").unwrap_err().starts_with("workflow script error"));
        assert!(precheck("while (true) {}").is_ok(), "compiling does not run it");
    }

    #[test]
    fn titles_and_json_answers() {
        assert_eq!(title_of("export const meta = { name: 'audit', description: 'x' };", None), "audit");
        assert_eq!(title_of("return 1", Some("Mine")), "Mine");
        assert_eq!(json_in("```json\n{\"a\":1}\n```"), Some(json!({ "a": 1 })));
        assert_eq!(json_in("Here: [1, 2] done"), Some(json!([1, 2])));
        assert_eq!(json_in("none"), None);
    }
}
