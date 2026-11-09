//! A write-capable subagent end to end: the parent spawns an implementer,
//! which writes in its own git worktree and is stopped from pushing; the
//! parent's tree is untouched and the child is kept on disk.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::process::{Command, Stdio};

use serde_json::{Value, json};

fn events_for(request: &Value) -> String {
    let messages = request["messages"].as_array().cloned().unwrap_or_default();
    let system = messages.first().and_then(|m| m["content"].as_str()).unwrap_or_default();
    let last = messages.last().cloned().unwrap_or_default();
    let content = last["content"].as_str().unwrap_or_default().to_owned();
    let tool_results = messages.iter().filter(|m| m["role"] == "tool").count();
    if system.contains("You are subagent") {
        // The implementer: write, try to push, report.
        return match tool_results {
            0 => call("write", json!({ "path": "hello.txt", "content": "hello from the child\n" })),
            1 => call("bash", json!({ "command": "git push origin main" })),
            _ => say(&format!("Wrote hello.txt. Push said: {content}")),
        };
    }
    if last["role"] == "tool" {
        if content.starts_with("Spawned") {
            return call("wait_agent", json!({ "timeout_ms": 20000 }));
        }
        return say(&format!("Child report:\n{content}"));
    }
    call("spawn_agent", json!({ "agent_type": "implementer", "description": "Add hello", "message": "Add hello.txt" }))
}

fn call(name: &str, arguments: Value) -> String {
    format!(
        "data: {{\"choices\":[{{\"delta\":{{\"tool_calls\":[{{\"index\":0,\"id\":\"call_{name}\",\"function\":{{\"name\":\"{name}\",\"arguments\":{}}}}}]}},\"finish_reason\":\"tool_calls\"}}]}}\n\ndata: [DONE]\n\n",
        serde_json::to_string(&arguments.to_string()).unwrap()
    )
}

fn say(text: &str) -> String {
    format!(
        "data: {{\"choices\":[{{\"delta\":{{\"content\":{}}},\"finish_reason\":\"stop\"}}]}}\n\ndata: [DONE]\n\n",
        serde_json::to_string(text).unwrap()
    )
}

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
                let request: Value = serde_json::from_slice(&body).unwrap_or_default();
                let events = events_for(&request);
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

#[test]
fn an_implementer_writes_in_its_worktree_and_cannot_push() {
    let root = std::env::temp_dir().join(format!("bubble-implementer-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let (home, project) = (root.join("home"), root.join("project"));
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&project).unwrap();
    let git = |args: &[&str]| assert!(Command::new("git").args(args).current_dir(&project).output().unwrap().status.success());
    git(&["init", "-q"]);
    std::fs::write(project.join("README.md"), "demo\n").unwrap();
    git(&["add", "."]);
    git(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "init"]);
    std::fs::write(
        home.join("config.json"),
        format!(
            r#"{{"providers":[{{"id":"mock","name":"Mock","baseURL":"{}","apiKey":"k","enabled":true}}],"defaultModel":"mock:test-model"}}"#,
            model_server()
        ),
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_bubble"))
        .args(["-p", "implement it", "--dangerously-skip-permissions", "--output-format", "json", "--cwd"])
        .arg(&project)
        .env("BUBBLE_HOME", &home)
        .env("BUBBLE_NO_SHELL_ENV", "1")
        .env("BUBBLE_WEB_SEARCH_URL", "http://127.0.0.1:9/mcp")
        .env("BUBBLE_MEMORY_AUTO", "0")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let result: Value = serde_json::from_str(stdout.trim()).unwrap();
    let report = result["text"].as_str().unwrap();

    assert!(report.contains("status: completed"), "{report}");
    let worktree = report
        .lines()
        .find_map(|l| l.strip_prefix("Worktree with changes: "))
        .unwrap_or_else(|| panic!("the worktree was kept: {report}"));
    assert_eq!(std::fs::read_to_string(std::path::Path::new(worktree).join("hello.txt")).unwrap(), "hello from the child\n");
    assert!(report.contains("new file: hello.txt"), "{report}");
    assert!(!project.join("hello.txt").exists(), "the parent's tree is untouched");
    assert_eq!(result["changes"]["changed_files"], 0, "git in the project saw nothing");

    // The child is kept on disk for the task.
    let task = result["sessionId"].as_str().unwrap();
    let saved: Value = serde_json::from_str(
        &std::fs::read_to_string(home.join("native/subagents").join(task).join("agent_1.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(saved["snapshot"]["status"], "completed");
    assert_eq!(saved["snapshot"]["worktree"]["changed"], true);
    assert!(
        saved["history"].to_string().contains("Blocked by worktree policy: pushing"),
        "the push was refused by code, not by the prompt"
    );

    let _ = Command::new("git").args(["worktree", "remove", "--force", worktree]).current_dir(&project).output();
    let _ = std::fs::remove_dir_all(&root);
}
