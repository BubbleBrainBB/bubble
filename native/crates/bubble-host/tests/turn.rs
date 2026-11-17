//! A turn through the real host process (the repository's built TypeScript
//! core, run with Node) against a stand-in model: the agent asks to run a
//! command, the person allows it, the app runs a tool of its own, and the
//! reply arrives.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::time::Duration;

use bubble_host::{Approval, ApprovalRequest, Event, Host, HostTool, Launch, SessionMessage, Terminal, TurnRequest};

/// An OpenAI-compatible model: first it runs a command, then calls the
/// app's tool, then answers with what the tool said.
fn model_server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
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
            let messages = request["messages"].as_array().cloned().unwrap_or_default();
            let results: Vec<String> =
                messages.iter().filter(|m| m["role"] == "tool").map(|m| m["content"].as_str().unwrap_or_default().to_owned()).collect();
            let events = match results.len() {
                0 => tool_call("call_1", "bash", "{\"command\":\"printf hi > out.txt\"}"),
                1 => tool_call("call_2", "note", "{\"text\":\"remember\"}"),
                _ => text(&format!("The note tool said: {}", results[1])),
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

fn tool_call(id: &str, name: &str, arguments: &str) -> String {
    format!(
        "data: {{\"choices\":[{{\"delta\":{{\"tool_calls\":[{{\"index\":0,\"id\":\"{id}\",\"function\":{{\"name\":\"{name}\",\"arguments\":{}}}}}]}},\"finish_reason\":\"tool_calls\"}}]}}\n\ndata: [DONE]\n\n",
        serde_json::to_string(arguments).unwrap()
    )
}

fn text(reply: &str) -> String {
    format!("data: {{\"choices\":[{{\"delta\":{{\"content\":{}}}}}]}}\n\ndata: [DONE]\n\n", serde_json::to_string(reply).unwrap())
}

/// The repository's built core, or `None` where it has not been built (a
/// checkout that only builds the Rust side).
fn built_core() -> Option<PathBuf> {
    let dist = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../dist");
    dist.join("host/index.js").is_file().then(|| dist.join("main.js"))
}

async fn next(messages: &mut tokio::sync::mpsc::UnboundedReceiver<SessionMessage>) -> SessionMessage {
    tokio::time::timeout(Duration::from_secs(30), messages.recv()).await.expect("the host went quiet").expect("the session closed")
}

#[tokio::test(flavor = "multi_thread")]
async fn a_turn_runs_through_the_real_host() {
    let Some(entry) = built_core() else {
        eprintln!("skipped: build the TypeScript core first (npm run build in the repository root)");
        return;
    };
    let root = std::env::temp_dir().join(format!("bubble-host-turn-{}", std::process::id()));
    let home = root.join("home");
    let project = root.join("project");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&project).unwrap();
    let url = model_server();
    std::fs::write(
        home.join("config.json"),
        format!(r#"{{"providers":[{{"id":"mock","name":"Mock","baseURL":"{url}","apiKey":"k","enabled":true}}],"defaultModel":"mock:test-model"}}"#),
    )
    .unwrap();

    let mut env: HashMap<String, String> = std::env::vars().collect();
    env.insert("BUBBLE_HOME".into(), home.to_string_lossy().into_owned());
    env.insert("BUBBLE_HOST_ENTRY".into(), entry.to_string_lossy().into_owned());
    // The stand-in model is on this machine.
    for key in ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "http_proxy", "https_proxy", "all_proxy"] {
        env.remove(key);
    }
    let host = Host::start(Launch::find(&env).unwrap()).await.unwrap();
    assert_eq!(host.info().home, home.to_string_lossy());

    let models = host.models(false).await.unwrap();
    assert_eq!(models.default_model, "mock:test-model");
    assert!(host.accounts().await.unwrap().iter().all(|account| !account.signed_in));

    let session = "desk-run-1";
    let project_path = project.to_string_lossy().into_owned();
    host.open_session(session, &project_path).await.unwrap();
    let mut messages = host.subscribe(session);
    host.start_turn(TurnRequest {
        session_id: session.into(),
        text: "Write the file and keep a note".into(),
        host_tools: vec![HostTool {
            name: "note".into(),
            description: "Keep a note".into(),
            parameters: serde_json::json!({ "type": "object", "properties": { "text": { "type": "string" } } }),
            read_only: true,
        }],
        ..Default::default()
    })
    .await
    .unwrap();

    let mut reply = String::new();
    let mut started = Vec::new();
    let mut asked = false;
    let mut noted = false;
    let ending = loop {
        match next(&mut messages).await {
            SessionMessage::Approval { id, tool_call_id, request } => {
                let ApprovalRequest::Bash { command, .. } = request else { panic!("only the command needs a yes") };
                assert_eq!(command, "printf hi > out.txt");
                assert_eq!(tool_call_id.as_deref(), Some("call_1"));
                assert!(!project.join("out.txt").exists(), "nothing ran before the yes");
                asked = true;
                host.approve(&id, &Approval::Once);
            }
            SessionMessage::ToolCall { id, tool_call_id, name, args } => {
                assert_eq!((name.as_str(), tool_call_id.as_deref()), ("note", Some("call_2")));
                assert_eq!(args["text"], "remember");
                noted = true;
                host.tool_result(&id, "noted", false);
            }
            SessionMessage::Event { event, terminal, .. } => {
                match event {
                    Event::TextDelta { content } => reply.push_str(&content),
                    Event::ToolStart { name, .. } => started.push(name),
                    _ => {}
                }
                if let Some(terminal) = terminal {
                    break terminal;
                }
            }
            SessionMessage::Lost(why) => panic!("the host stopped: {why}"),
            _ => {}
        }
    };
    assert_eq!(ending, Terminal::Completed);
    assert!(asked && noted);
    assert_eq!(started, ["bash", "note"]);
    assert_eq!(reply, "The note tool said: noted");
    assert_eq!(std::fs::read_to_string(project.join("out.txt")).unwrap(), "hi");

    // The conversation is in Bubble's own session file, where the terminal
    // can continue it.
    let history = host.history(session).await.unwrap();
    assert_eq!(history.first().unwrap()["role"], "user");
    assert_eq!(history.last().unwrap()["content"], "The note tool said: noted");
    let sessions = home.join("sessions");
    let files: Vec<_> = walk(&sessions).into_iter().filter(|p| p.file_name().is_some_and(|n| n == "desk-run-1.jsonl")).collect();
    assert_eq!(files.len(), 1, "the session file is under {}", sessions.display());

    host.shutdown().await;
    for _ in 0..50 {
        if !host.is_alive() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(!host.is_alive(), "the host exits when asked");
    assert!(host.accounts().await.is_err(), "a stopped host answers nothing");
    std::fs::remove_dir_all(&root).ok();
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(walk(&path));
        } else {
            found.push(path);
        }
    }
    found
}
