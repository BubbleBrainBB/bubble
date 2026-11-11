//! The real desktop/agent stream, paused at protocol boundaries: tool
//! admission, approval, first answer token, stop, transport error, restart.
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

use nsh_core::element::{px, size};
use nsh_engine::{EngineConfig, Root};
use nsh_testing::Headless;
use serde_json::{Value, json};

fn delta(value: Value) -> String {
    format!("data: {}\n\n", json!({"choices":[{"delta":value}]}))
}

fn request(stream: &TcpStream) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut length = 0;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
            break;
        }
        if let Some(n) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            length = n.trim().parse().unwrap();
        }
    }
    reader.read_exact(&mut vec![0; length]).unwrap();
}

fn header(stream: &mut TcpStream, length: usize) {
    write!(
        stream,
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {length}\r\nconnection: close\r\n\r\n"
    )
    .unwrap();
}

fn model() -> (String, mpsc::Sender<()>, mpsc::Sender<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (text_tx, text_rx) = mpsc::channel();
    let (end_tx, end_rx) = mpsc::channel();
    std::thread::spawn(move || {
        for (index, stream) in listener.incoming().enumerate() {
            let Ok(mut stream) = stream else { break };
            request(&stream);
            match index {
                0 => {
                    let events = delta(json!({"reasoning_content":"**Inspecting files**"}))
                        + &delta(json!({"content":"I will inspect the project."}))
                        + &delta(json!({"tool_calls":[
                            {"index":0,"id":"same","function":{"name":"bash","arguments":"{\"command\":\"printf first\"}"}},
                            {"index":1,"id":"next","function":{"name":"bash","arguments":"{\"command\":\"printf second\"}"}}
                        ]}))
                        + "data: [DONE]\n\n";
                    header(&mut stream, events.len());
                    stream.write_all(events.as_bytes()).unwrap();
                }
                1 => {
                    let reasoning = delta(json!({"reasoning_content":"**Preparing answer**"}));
                    let text = delta(json!({"content":"The final answer starts here."}));
                    let end = delta(json!({"content":" It is complete."})) + "data: [DONE]\n\n";
                    header(&mut stream, reasoning.len() + text.len() + end.len());
                    stream.write_all(reasoning.as_bytes()).unwrap();
                    stream.flush().unwrap();
                    if text_rx.recv_timeout(Duration::from_secs(30)).is_err() {
                        break;
                    }
                    stream.write_all(text.as_bytes()).unwrap();
                    stream.flush().unwrap();
                    if end_rx.recv_timeout(Duration::from_secs(30)).is_err() {
                        break;
                    }
                    stream.write_all(end.as_bytes()).unwrap();
                }
                2 | 3 => {
                    let events = delta(json!({"reasoning_content":"**Keep this reasoning**"}))
                        + &delta(
                            json!({"content":if index == 2 { "Keep this stopped reply." } else { "Keep this failed reply." }}),
                        );
                    header(&mut stream, events.len() + 1000);
                    stream.write_all(events.as_bytes()).unwrap();
                    stream.flush().unwrap();
                    if index == 2 {
                        // Stopping cancels this in-flight request.
                        let mut byte = [0];
                        let _ = stream.read(&mut byte);
                    }
                    // Index 3 deliberately closes a truncated HTTP body.
                }
                _ => break,
            }
        }
    });
    (format!("http://{address}"), text_tx, end_tx)
}

fn start() -> Headless {
    let mut app = Headless::start(EngineConfig::new(Root::of::<bubble_app::App>())).unwrap();
    app.set_viewport(size(px(1280.0), px(1800.0))).unwrap();
    app.wait_for("What should we work on?").unwrap();
    app
}

fn stored(home: &Path) -> Vec<(String, Vec<Value>)> {
    let store = bubble_store::Store::open(home.join("native/bubble.db")).unwrap();
    let task = store.list_tasks().unwrap().remove(0);
    let run = store.runs_for_task(&task.task.id).unwrap().remove(0);
    store
        .turn_transcripts(&run.id)
        .unwrap()
        .into_iter()
        .map(|(a, c)| (a, serde_json::from_str(&c).unwrap()))
        .collect()
}

fn ordered(app: &Headless, snippets: &[&str]) {
    let text = app.screen().texts();
    let indices = snippets
        .iter()
        .map(|s| {
            text.iter()
                .position(|t| t.contains(s))
                .unwrap_or_else(|| panic!("missing {s:?}: {text:?}"))
        })
        .collect::<Vec<_>>();
    assert!(indices.windows(2).all(|w| w[0] < w[1]), "wrong order: {text:?}");
}

#[test]
fn live_order_disclosures_terminal_states_and_restart_agree() {
    let root = std::env::temp_dir().join(format!("bubble-trace-test-{}", std::process::id()));
    let home = root.join("home");
    let project = root.join("demo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&project).unwrap();
    let (url, text, end) = model();
    std::fs::write(home.join("config.json"), json!({"providers":[{"id":"mock","name":"Mock","baseURL":url,"apiKey":"k","enabled":true}],"defaultModel":"mock:test-model"}).to_string()).unwrap();
    // SAFETY: one test in this binary; no engine threads exist yet.
    unsafe {
        std::env::set_var("BUBBLE_HOME", &home);
        std::env::set_var("BUBBLE_NO_SHELL_ENV", "1");
        std::env::set_var("BUBBLE_MEMORY_AUTO", "0");
        std::env::set_var("BUBBLE_GH", root.join("no-gh"));
    }
    let long = Duration::from_secs(15);
    // New work starts with the Manager; this test starts from a task already made.
    bubble_store::Store::open(home.join("native/bubble.db"))
        .unwrap()
        .create_task(bubble_store::NewTask { title: "Trace regression", project_cwd: Some(project.to_str().unwrap()), origin: bubble_store::Origin::User })
        .unwrap();
    let mut app = start();
    app.press("id:nav-tasks").unwrap();
    app.press("id:side-Trace regression").unwrap();
    app.type_text("id:composer", "Inspect").unwrap();
    app.submit("id:composer").unwrap();
    app.wait_for_within("Allow once", long).unwrap();
    let (anchor, _) = stored(&home).remove(0);
    // A running turn folds its work into one row, the status line at its end.
    app.wait_for("text:Waiting for your approval").unwrap();
    app.press("label:Show work").unwrap();
    // Lone calls are rows of their own, with no group row over them.
    app.wait_for("Queued: Ran printf second").unwrap();
    ordered(
        &app,
        &[
            "I will inspect",
            "Waiting for approval: Ran printf first",
            "Queued: Ran printf second",
        ],
    );
    app.press("Allow once").unwrap();
    app.wait_for_within("text:printf second", long).unwrap();
    app.press("Allow once").unwrap();
    app.wait_for_within("Preparing answer", long).unwrap();
    let second_row = "text:Ran printf second";
    let node = app.screen().press_target(second_row).unwrap().unwrap();
    // The open row and its output survive the first answer token.
    app.press(second_row).unwrap();
    app.wait_for("text:/^\\$ printf second\\nsecond/").unwrap();
    text.send(()).unwrap();
    app.wait_for_within("The final answer starts here.", long).unwrap();
    assert_eq!(app.screen().press_target(second_row).unwrap(), Some(node));
    app.wait_for("text:/^\\$ printf second\\nsecond/").unwrap();
    end.send(()).unwrap();
    app.wait_for_within("The final answer starts here. It is complete.", long)
        .unwrap();
    app.wait_for("text:/^Worked for /").unwrap();
    app.press("label:Worked").unwrap();
    // The raw-output disclosure remains open after folding.
    app.wait_for("text:/^\\$ printf second\\nsecond/").unwrap();
    ordered(
        &app,
        &[
            "I will inspect",
            "Ran printf first",
            "Ran printf second",
            "The final answer starts",
        ],
    );

    app.type_text("id:composer", "Stop this").unwrap();
    app.submit("id:composer").unwrap();
    app.wait_for_within("Keep this stopped reply.", long).unwrap();
    app.press("label:Stop").unwrap();
    app.wait_for_within("text:/^You stopped/", long).unwrap();
    app.wait_for("Keep this stopped reply.").unwrap();
    app.type_text("id:composer", "Fail this").unwrap();
    app.submit("id:composer").unwrap();
    app.wait_for_within("text:/^Failed/", long).unwrap();
    app.wait_for("Keep this failed reply.").unwrap();
    let before = stored(&home);
    assert_eq!(before.len(), 3);
    for (index, expected) in ["Completed", "Stopped", "Failed"].iter().enumerate() {
        assert_eq!(before[index].1[0]["kind"]["Turn"]["status"], *expected);
        assert!(before[index].1.iter().all(|item| item["pending"] == false));
    }
    drop(app);

    let mut app = start();
    app.press("text:Trace regression").unwrap();
    app.wait_for_within("Keep this stopped reply.", long).unwrap();
    app.wait_for("Keep this failed reply.").unwrap();
    app.wait_for("text:/^You stopped/").unwrap();
    app.wait_for("text:/^Failed/").unwrap();
    assert_eq!(
        stored(&home),
        before,
        "restoring must not rewrite terminal state or timing"
    );
    app.press("label:Worked").unwrap();
    app.wait_for("I will inspect the project.").unwrap();
    ordered(
        &app,
        &[
            "I will inspect",
            "The final answer starts",
            "Keep this stopped reply",
            "Keep this failed reply",
        ],
    );
    drop(app);
    std::fs::remove_dir_all(root).unwrap();
}
