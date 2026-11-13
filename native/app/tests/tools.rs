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
            // The skills are in the system prompt; the tools still to be
            // loaded are announced in a reminder after it.
            let told = request["messages"].to_string();
            if system.contains("- notes-helper: Keeps notes tidy") && told.contains("mcp__echo__echo") {
                std::fs::write(&seen, "").ok();
            }
            if system.contains("gave their bots full access") {
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
        std::env::set_var("BUBBLE_MEMORY_AUTO", "0");
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
    app.press("id:settings-tools").unwrap();
    app.wait_for_within("text:1 tool", long).unwrap();
    app.wait_for("text:Connected").unwrap();
    // The one that cannot start says so in the agent's words, and can be tried again.
    app.wait_for_within("label:Retry gone", long).unwrap();
    // Only the test's own folder is certain; the person's may add more.
    app.wait_for("text:/^\\d+ skills?$/").unwrap();
    app.press("id:settings-close").unwrap();

    // A task loads the tool, asks before calling it, and gets its answer.
    app.press("id:side-Echo").unwrap();
    app.type_text("id:composer", "Echo hi").unwrap();
    app.submit("text:Echo hi").unwrap();
    // What it asks to do shows as a card; the calls it made to get there
    // do not show, and the row at the end says it waits for the person.
    app.wait_for_within("Use echo from the echo MCP server", long).unwrap();
    app.wait_for("text:Waiting for you").unwrap();
    assert!(!app.shows("text:Loaded tools"));
    app.press("Allow once").unwrap();
    app.wait_for_within("text:The server said: echo: hi", long).unwrap();
    // Done: what it said, and nothing of how. The card that was answered goes too.
    app.wait_for_within("label:Send", long).unwrap();
    assert!(!app.shows("text:/^Worked for /") && !app.shows("text:Used echo from echo") && !app.shows("Allow once"));
    assert!(seen.exists(), "the skill list and the deferred tool reached the system prompt");

    // A command waits for a yes; turning on full access answers it.
    app.type_text("id:composer", "Echo again").unwrap();
    app.submit("id:composer").unwrap();
    app.wait_for_within("text:Allow once", long).unwrap();
    // One setting, in Settings, for every bot.
    app.press("id:nav-settings").unwrap();
    app.press("id:settings-permissions").unwrap();
    app.press("label:Approvals").unwrap();
    app.press("label:Full access").unwrap();
    app.wait_for("text:Full access").unwrap();
    app.press("id:settings-close").unwrap();
    wait_for_count(&mut app, "text:The server said: echo: hi", 2, long);

    // With full access the next message goes ahead without asking, and
    // the agent is told so.
    app.type_text("id:composer", "Echo once more").unwrap();
    app.submit("id:composer").unwrap();
    wait_for_count(&mut app, "text:The server said: echo: hi", 3, long);
    assert!(seen.with_extension("full").exists(), "the agent was told it has full access");

    // Back to asking first.
    app.press("id:nav-settings").unwrap();
    app.press("id:settings-permissions").unwrap();
    app.press("label:Approvals").unwrap();
    app.press("label:Ask first").unwrap();
    app.wait_for("text:Ask first").unwrap();
    app.press("id:settings-close").unwrap();
    assert!(!app.shows("label:Access"), "a bot has no access of its own to set");

    std::fs::remove_dir_all(&root).ok();
}
