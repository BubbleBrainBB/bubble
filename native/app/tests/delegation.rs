//! Subagents and workflows in a task, with a stand-in model that answers
//! the task's agent and its children differently.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::time::Duration;

use nsh_engine::{EngineConfig, Root};
use nsh_testing::Headless;

const SCRIPT: &str = "export const meta = { name: 'check-both' };\nconst found = await parallel(['alpha', 'beta'].map((x) => () => agent('Check ' + x, { label: x })));\nreturn found;";

fn model_server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            std::thread::spawn(move || {
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
                let last = request["messages"].as_array().and_then(|m| m.last()).cloned().unwrap_or_default();
                let content = last["content"].as_str().unwrap_or_default().to_owned();
                let events = if system.contains("You are subagent") {
                    // A child: answers what it was asked.
                    let asked = content.lines().next().unwrap_or_default().to_owned();
                    if asked.starts_with("Check ") {
                        text_events(&format!("Checked {}", &asked[6..]))
                    } else {
                        text_events("The answer is 42.")
                    }
                } else if last["role"] == "tool" {
                    if content.starts_with("Spawned") {
                        tool_call_events("wait_agent", "{\"timeout_ms\": 10000}")
                    } else if let Some(summary) = content.split("Summary:\n").nth(1) {
                        text_events(&format!("Child said: {}", summary.lines().next().unwrap_or_default()))
                    } else if content.starts_with("workflow") {
                        let result = content.split("Result:\n").nth(1).unwrap_or_default().split_whitespace().collect::<Vec<_>>().join(" ");
                        text_events(&format!("Workflow gave {result}"))
                    } else {
                        text_events(&format!("Unexpected: {content}"))
                    }
                } else if content.contains("Delegate") {
                    tool_call_events(
                        "spawn_agent",
                        "{\"agent_type\":\"explorer\",\"description\":\"Find the answer\",\"message\":\"Find the answer in this project.\"}",
                    )
                } else if content.contains("Run a workflow") {
                    tool_call_events("run_workflow", &serde_json::json!({ "script": SCRIPT }).to_string())
                } else {
                    text_events("Hello.")
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{events}",
                    events.len()
                );
                stream.write_all(response.as_bytes()).ok();
            });
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

#[test]
fn tasks_delegate_to_subagents_and_workflows() {
    let root = std::env::temp_dir().join(format!("bubble-native-delegation-{}", std::process::id()));
    let home = root.join("home");
    let project = root.join("demo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&project).unwrap();
    let url = model_server();
    std::fs::write(
        home.join("config.json"),
        format!(
            r#"{{"providers":[{{"id":"mock","name":"Mock","baseURL":"{url}","apiKey":"k","enabled":true}}],"defaultModel":"mock:test-model"}}"#
        ),
    )
    .unwrap();
    // SAFETY: the only test in the binary, set before the engine starts any
    // thread that reads the environment.
    unsafe {
        std::env::set_var("BUBBLE_HOME", &home);
        std::env::set_var("BUBBLE_NO_SHELL_ENV", "1");
        std::env::set_var("BUBBLE_MEMORY_AUTO", "0");
    }

    let mut app = Headless::start(EngineConfig::new(Root::of::<bubble_app::App>())).unwrap();
    app.set_viewport(nsh_core::element::size(nsh_core::element::px(1280.0), nsh_core::element::px(1400.0)))
        .unwrap();
    let long = Duration::from_secs(15);
    app.wait_for("What should we work on?").unwrap();

    app.press("id:nav-tasks").unwrap();
    app.press("text:New task").unwrap();
    app.type_text("text:What needs doing?", "Investigate").unwrap();
    app.on_pick_paths(vec![project.clone()]).unwrap();
    app.press("Choose a project folder").unwrap();
    app.wait_for("demo").unwrap();
    app.press("Create").unwrap();

    // A child is started, shown under the conversation, and collected.
    app.type_text("text:Message this task", "Delegate please").unwrap();
    app.submit("text:Delegate please").unwrap();
    app.wait_for_within("text:Child said: The answer is 42.", long).unwrap();
    app.wait_for("text:Subagents").unwrap();
    app.wait_for("text:explorer").unwrap();
    app.wait_for("text:completed").unwrap();
    app.wait_for("text:Started a subagent: Find the answer").unwrap();

    // A workflow fans out to two children and returns their answers.
    app.type_text("text:Message this task", "Run a workflow").unwrap();
    app.submit("text:Run a workflow").unwrap();
    app.wait_for_within(r#"text:Workflow gave [ "Checked alpha", "Checked beta" ]"#, long).unwrap();
    app.wait_for("text:alpha").unwrap();

    std::fs::remove_dir_all(&root).ok();
}
