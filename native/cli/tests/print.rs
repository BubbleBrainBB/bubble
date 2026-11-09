//! The `bubble` binary in print mode, against a stand-in model.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

/// A stand-in OpenAI-style model. Asked to "make notes" it writes a file,
/// then runs a command, then answers with what the tools said. It records
/// every request body.
fn model_server(requests: Arc<Mutex<Vec<Value>>>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let requests = requests.clone();
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
                requests.lock().unwrap().push(request.clone());
                let messages = request["messages"].as_array().cloned().unwrap_or_default();
                let last = messages.last().cloned().unwrap_or_default();
                let tool_results: Vec<String> =
                    messages.iter().filter(|m| m["role"] == "tool").filter_map(|m| m["content"].as_str().map(str::to_owned)).collect();
                let events = if last["role"] == "tool" {
                    match tool_results.len() {
                        1 => tool_call("bash", &json!({ "command": "echo ran > ran.txt" }).to_string()),
                        _ => text(&format!("Done. Tools said: {}", tool_results.join(" | "))),
                    }
                } else if last["content"].as_str().unwrap_or_default().contains("make notes") {
                    tool_call("write", &json!({ "path": "notes.txt", "content": "hello" }).to_string())
                } else {
                    text(&format!("You said: {}", last["content"].as_str().unwrap_or_default()))
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

fn tool_call(name: &str, arguments: &str) -> String {
    format!(
        "data: {{\"choices\":[{{\"delta\":{{\"tool_calls\":[{{\"index\":0,\"id\":\"call_{name}\",\"function\":{{\"name\":\"{name}\",\"arguments\":{}}}}}]}},\"finish_reason\":\"tool_calls\"}}]}}\n\ndata: {{\"choices\":[],\"usage\":{{\"prompt_tokens\":100,\"completion_tokens\":10}}}}\n\ndata: [DONE]\n\n",
        serde_json::to_string(arguments).unwrap()
    )
}

fn text(reply: &str) -> String {
    format!(
        "data: {{\"choices\":[{{\"delta\":{{\"content\":{}}},\"finish_reason\":\"stop\"}}]}}\n\ndata: {{\"choices\":[],\"usage\":{{\"prompt_tokens\":50,\"completion_tokens\":5}}}}\n\ndata: [DONE]\n\n",
        serde_json::to_string(reply).unwrap()
    )
}

struct Setup {
    home: PathBuf,
    project: PathBuf,
    requests: Arc<Mutex<Vec<Value>>>,
}

fn setup(name: &str) -> Setup {
    let root = std::env::temp_dir().join(format!("bubble-cli-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let project = root.join("project");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&project).unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let url = model_server(requests.clone());
    std::fs::write(
        home.join("config.json"),
        format!(r#"{{"providers":[{{"id":"mock","name":"Mock","baseURL":"{url}","apiKey":"k","enabled":true}}],"defaultModel":"mock:test-model"}}"#),
    )
    .unwrap();
    Setup { home, project, requests }
}

fn bubble(setup: &Setup, args: &[&str], stdin: Option<&str>) -> (i32, String, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_bubble"))
        .args(args)
        .arg("--cwd")
        .arg(&setup.project)
        .env("BUBBLE_HOME", &setup.home)
        .env("BUBBLE_NO_SHELL_ENV", "1")
        .env("BUBBLE_WEB_SEARCH_URL", "http://127.0.0.1:9/mcp")
        .env("HOME", setup.home.parent().unwrap())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    if let Some(input) = stdin {
        child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
    } else {
        drop(child.stdin.take());
    }
    let output = child.wait_with_output().unwrap();
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

fn read(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

#[test]
fn build_mode_writes_files_but_refuses_commands() {
    let setup = setup("build");
    let (code, stdout, stderr) = bubble(&setup, &["-p", "make notes"], None);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(read(&setup.project.join("notes.txt")).as_deref(), Some("hello"));
    assert!(read(&setup.project.join("ran.txt")).is_none(), "commands need a yes nobody can give");
    assert!(stdout.contains("[Tool: write]") && stdout.contains("[Tool: bash]"));
    assert!(stdout.contains("No interactive UI is available to approve this tool call."));
    let system = setup.requests.lock().unwrap()[0]["messages"][0]["content"].as_str().unwrap_or_default().to_owned();
    assert!(system.contains("Permission mode: default Build mode") && system.contains("## Delegation policy"));
}

#[test]
fn bypass_runs_everything_and_json_reports_one_object() {
    let setup = setup("bypass");
    let (code, stdout, stderr) =
        bubble(&setup, &["-p", "make notes", "--dangerously-skip-permissions", "--output-format", "json"], None);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(read(&setup.project.join("ran.txt")).as_deref().map(str::trim), Some("ran"));
    assert_eq!(stdout.lines().count(), 1, "exactly one JSON object: {stdout}");
    let result: Value = serde_json::from_str(&stdout).unwrap();
    assert!(result["text"].as_str().unwrap().starts_with("Done. Tools said:"));
    assert_eq!(result["stopReason"], "end_turn");
    assert_eq!(result["num_turns"], 3);
    assert_eq!(result["num_tool_calls"], 2);
    assert_eq!(result["usage"]["input_tokens"], 250);
    assert_eq!(result["usage"]["output_tokens"], 25);
    assert_eq!(result["usage_reported"], true);
    assert!(result["sessionId"].as_str().unwrap().starts_with("task_"));
    assert!(result.get("changes").is_none(), "no git repository, no changes field");
    assert_eq!(result["usage"]["cache_read_input_tokens"], 0);
}

#[test]
fn json_reports_what_git_saw_change() {
    let setup = setup("changes");
    assert!(Command::new("git").args(["init", "-q"]).current_dir(&setup.project).status().unwrap().success());
    let (code, stdout, stderr) =
        bubble(&setup, &["-p", "make notes", "--dangerously-skip-permissions", "--output-format", "json"], None);
    assert_eq!(code, 0, "stderr: {stderr}");
    let result: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(result["changes"], json!({ "changed_files": 2, "modified_existing_tests": [] }), "notes.txt and ran.txt");
}

#[test]
fn plan_mode_refuses_changes() {
    let setup = setup("plan");
    let (code, stdout, _) = bubble(&setup, &["-p", "make notes", "--plan"], None);
    assert_eq!(code, 0);
    assert!(read(&setup.project.join("notes.txt")).is_none());
    assert!(stdout.contains("Plan mode is active."));
}

#[test]
fn sessions_resume_and_piped_prompts_work() {
    let setup = setup("resume");
    let (code, stdout, _) = bubble(&setup, &["-p", "--session", "notes"], Some("first message\n"));
    assert_eq!(code, 0);
    assert!(stdout.contains("You said: first message"));
    let (code, _, _) = bubble(&setup, &["-p", "second message", "--session", "notes"], None);
    assert_eq!(code, 0);
    let requests = setup.requests.lock().unwrap();
    let last = requests.last().unwrap()["messages"].to_string();
    assert!(last.contains("first message") && last.contains("You said: first message"), "the named session continued");
    drop(requests);
    let (_, _, _) = bubble(&setup, &["-p", "unrelated"], None);
    let (code, _, _) = bubble(&setup, &["-p", "third", "-r"], None);
    assert_eq!(code, 0);
    let requests = setup.requests.lock().unwrap();
    let last = requests.last().unwrap()["messages"].to_string();
    assert!(last.contains("unrelated") && !last.contains("first message"), "-r continues the newest session");
}

#[test]
fn stream_json_keeps_one_session_across_lines() {
    let setup = setup("stream");
    let input = [
        json!({ "type": "user", "message": { "role": "user", "content": "hello there" }, "request_id": "a" }).to_string(),
        "not json".to_owned(),
        json!({ "type": "user", "message": { "role": "user", "content": [{ "type": "text", "text": "again" }] }, "request_id": "b" }).to_string(),
    ]
    .join("\n");
    let (code, stdout, stderr) =
        bubble(&setup, &["-p", "--input-format", "stream-json", "--output-format", "stream-json"], Some(&format!("{input}\n")));
    assert_eq!(code, 0, "stderr: {stderr}");
    let lines: Vec<Value> = stdout.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(lines[0]["subtype"], "init");
    let results: Vec<&Value> = lines.iter().filter(|l| l["type"] == "result").collect();
    assert_eq!(results.len(), 3);
    assert_eq!(results[0]["result"], "You said: hello there");
    assert_eq!(results[0]["request_id"], "a");
    assert_eq!(results[1]["subtype"], "invalid_input");
    assert_eq!(results[2]["turn"], 2);
    assert!(lines.iter().any(|l| l["type"] == "stream_event" && l["event"]["type"] == "text_delta"));
    let last = setup.requests.lock().unwrap().last().unwrap()["messages"].to_string();
    assert!(last.contains("hello there") && last.contains("again"));
}

#[test]
fn interactive_sessions_read_prompts_and_ask_before_commands() {
    let setup = setup("interactive");
    let (code, stdout, stderr) = bubble(&setup, &[], Some("make notes\ny\nhello again\n/exit\n"));
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(stdout.contains("• Wrote notes.txt"));
    assert!(stdout.contains("? Run a command: echo ran > ran.txt"));
    assert_eq!(read(&setup.project.join("ran.txt")).as_deref().map(str::trim), Some("ran"), "the yes ran it");
    assert!(stdout.contains("You said: hello again"));
}
