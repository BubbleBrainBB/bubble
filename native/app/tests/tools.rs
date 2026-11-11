//! Skills, MCP servers and web tools, in Settings and in a task: a stand-in
//! model loads an MCP tool with tool_search, then calls it after the
//! person's yes.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::time::Duration;

use nsh_engine::{EngineConfig, Root};
use nsh_testing::Headless;

/// The task's agent: asked to echo, it loads the echo tool, calls it, then
/// reports what came back. Records whether the skill list and the deferred
/// tool reached the system prompt.
fn model_server(seen: &Path) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let seen = seen.to_path_buf();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut length = 0usize;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap_or(0);
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).ok();
            let request: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
            let system = request["messages"][0]["content"].as_str().unwrap_or_default().to_owned();
            if system.contains("- notes-helper: Keeps notes tidy") && system.contains("mcp__echo__echo") {
                std::fs::write(&seen, "").ok();
            }
            if system.contains("gave this task full access") {
                std::fs::write(seen.with_extension("full"), "").ok();
            }
            let last = request["messages"].as_array().and_then(|m| m.last()).cloned().unwrap_or_default();
            let result = last["content"].as_str().unwrap_or_default().to_owned();
            let events = if last["role"] != "tool" {
                tool_call_events("tool_search", "{\"query\":\"select:mcp__echo__echo\"}")
            } else if result.starts_with("<functions>") {
                tool_call_events("mcp__echo__echo", "{\"text\":\"hi\"}")
            } else {
                text_events(&format!("The server said: {result}"))
            };
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{events}",
                events.len()
            );
            stream.write_all(response.as_bytes()).ok();
        }
    });
    format!("http://{address}")
}

fn tool_call_events(name: &str, arguments: &str) -> String {
    format!(
        "data: {{\"choices\":[{{\"delta\":{{\"tool_calls\":[{{\"index\":0,\"id\":\"call_{name}\",\"function\":{{\"name\":\"{name}\",\"arguments\":{}}}}}]}},\"finish_reason\":\"tool_calls\"}}]}}\n\ndata: [DONE]\n\n",
        serde_json::to_string(arguments).unwrap()
    )
}

fn text_events(reply: &str) -> String {
    format!(
        "data: {{\"choices\":[{{\"delta\":{{\"content\":{}}}}}]}}\n\ndata: [DONE]\n\n",
        serde_json::to_string(reply).unwrap()
    )
}

const ECHO_SERVER: &str = r#"
import json, sys
for line in sys.stdin:
    m = json.loads(line)
    reply = None
    if m.get("method") == "initialize":
        reply = {"capabilities": {"tools": {}}}
    elif m.get("method") == "tools/list":
        reply = {"tools": [{"name": "echo", "description": "Echo text", "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}}}]}
    elif m.get("method") == "tools/call":
        reply = {"content": [{"type": "text", "text": "echo: " + m["params"]["arguments"]["text"]}]}
    if reply is not None:
        sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": m["id"], "result": reply}) + "\n"); sys.stdout.flush()
"#;

/// Waits until `selector` matches `count` times, failing if a question
/// shows up meanwhile.
fn wait_for_count(app: &mut Headless, selector: &str, count: usize, within: Duration) {
    let deadline = std::time::Instant::now() + within;
    while app.screen().count(selector).unwrap() < count {
        assert!(std::time::Instant::now() < deadline, "{selector} never reached {count}");
        assert_eq!(app.screen().count("text:Allow once").unwrap(), 0, "full access asked anyway");
        app.wait(Duration::from_millis(50)).unwrap();
    }
}

#[test]
fn tasks_use_mcp_tools_and_settings_show_what_is_there() {
    let root = std::env::temp_dir().join(format!("bubble-native-tools-{}", std::process::id()));
    let home = root.join("home");
    let project = root.join("demo");
    std::fs::create_dir_all(home.join("skills/notes-helper")).unwrap();
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(home.join("skills/notes-helper/SKILL.md"), "---\ndescription: |\n  Keeps notes tidy.\n---\nSort them.").unwrap();
    std::fs::write(root.join("echo.py"), ECHO_SERVER).unwrap();
    let seen = root.join("prompt.seen");
    let url = model_server(&seen);
    std::fs::write(
        home.join("config.json"),
        format!(
            r#"{{"providers":[{{"id":"mock","name":"Mock","baseURL":"{url}","apiKey":"k","enabled":true}}],"defaultModel":"mock:test-model"}}"#
        ),
    )
    .unwrap();
    std::fs::write(
        home.join("settings.json"),
        serde_json::json!({ "mcpServers": {
            "echo": { "command": "python3", "args": [root.join("echo.py")] },
            "gone": { "command": "/nonexistent/bubble-mcp" },
        }})
        .to_string(),
    )
    .unwrap();
    // SAFETY: the only test in the binary, set before the engine starts any
    // thread that reads the environment.
    unsafe {
        std::env::set_var("BUBBLE_HOME", &home);
        std::env::set_var("BUBBLE_NO_SHELL_ENV", "1");
    }

    // New work starts with the Manager; this test starts from a task already made.
    bubble_store::Store::open(home.join("native/bubble.db"))
        .unwrap()
        .create_task(bubble_store::NewTask { title: "Echo", project_cwd: Some(project.to_str().unwrap()), origin: bubble_store::Origin::User })
        .unwrap();
    let mut app = Headless::start(EngineConfig::new(Root::of::<bubble_app::App>())).unwrap();
    app.set_viewport(nsh_core::element::size(nsh_core::element::px(1280.0), nsh_core::element::px(1400.0)))
        .unwrap();
    let long = Duration::from_secs(10);

    app.wait_for("What should we work on?").unwrap();

    // Settings: the person's servers, connected or not, and their skills.
    app.press("id:nav-settings").unwrap();
    app.wait_for_within("text:1 tool", long).unwrap();
    app.wait_for_within("text:/could not start/", long).unwrap();
    app.wait_for("label:Retry gone").unwrap();
    // Only the test's own folder is certain; the person's may add more.
    app.wait_for("text:/^\\d+ skills? in .*home/skills/").unwrap();

    // A task loads the tool, asks before calling it, and gets its answer.
    app.press("id:nav-tasks").unwrap();
    app.press("id:side-Echo").unwrap();
    app.type_text("text:Message this task", "Echo hi").unwrap();
    app.submit("text:Echo hi").unwrap();
    // The calls so far fold into one row; the question stays out, and the
    // group holding the waiting call opens by itself.
    app.wait_for_within("text:Waiting for your approval", long).unwrap();
    app.wait_for_within("Use echo from the echo MCP server", long).unwrap();
    app.press("label:Show work").unwrap();
    app.wait_for("text:Loaded tools").unwrap();
    app.press("Allow once").unwrap();
    app.wait_for_within("text:The server said: echo: hi", long).unwrap();
    // Done, the turn's work folds under "Worked for"; opened, it is all there.
    app.wait_for("text:/^Worked for /").unwrap();
    app.press("label:Worked").unwrap();
    // The group opened itself only while its call waited; it opens again.
    app.press("text:Called 2 tools").unwrap();
    app.wait_for("text:Used echo from echo").unwrap();
    assert!(seen.exists(), "the skill list and the deferred tool reached the system prompt");

    // A command waits for a yes; turning on full access answers it.
    app.type_text("id:composer", "Echo again").unwrap();
    app.submit("id:composer").unwrap();
    app.wait_for_within("text:Allow once", long).unwrap();
    app.press("label:Access").unwrap();
    app.press("label:Full access").unwrap();
    app.wait_for("text:Full access").unwrap();
    wait_for_count(&mut app, "text:The server said: echo: hi", 2, long);

    // With full access the next message goes ahead without asking, and
    // the agent is told so.
    app.type_text("id:composer", "Echo once more").unwrap();
    app.submit("id:composer").unwrap();
    wait_for_count(&mut app, "text:The server said: echo: hi", 3, long);
    assert!(seen.with_extension("full").exists(), "the agent was told it has full access");

    // Back to asking first.
    app.press("label:Access").unwrap();
    app.press("label:Ask first").unwrap();
    app.wait_for("text:Ask first").unwrap();

    std::fs::remove_dir_all(&root).ok();
}
