//! The app driven headless, on the real engine, with a stand-in model.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::time::Duration;

use nsh_engine::{EngineConfig, Root};
use nsh_testing::Headless;

const COMMAND: &str = "echo shipped > out.txt";

/// A stand-in model, streaming OpenAI's format.
///
/// The Manager (offered `create_task`): asked to "ship", it creates a task in
/// `project`; given the result, it says "Started it."; otherwise it greets.
/// A task's agent (offered `bash`): it asks to run `COMMAND`, then says
/// "All done." once the result comes back.
fn model_server(project: &Path) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let project = project.display().to_string();
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
            let body = String::from_utf8_lossy(&body);
            if body.contains("Always mention bananas") {
                std::fs::write(Path::new(&project).join("rules.seen"), "").ok();
            }
            // Only the newest message decides: a tool result, or what was asked.
            let request: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
            let last = request["messages"].as_array().and_then(|m| m.last()).cloned().unwrap_or_default();
            let answered = last["role"] == "tool";
            let asked = last["content"].as_str().unwrap_or_default().to_owned();
            let events = if body.contains("\"create_task\"") {
                if answered {
                    text_events("Started it.")
                } else if body.contains("Please ship") {
                    let arguments = format!(
                        "{{\"title\":\"Ship the notes\",\"project\":{},\"brief\":\"Write out.txt with the word shipped.\"}}",
                        json_string(&project)
                    );
                    tool_call_events("create_task", &arguments)
                } else {
                    text_events("Hello from the stand-in model.")
                }
            } else if asked.contains("Wait for me") {
                tool_call_events("bash", &format!("{{\"command\":{}}}", json_string("sleep 20")))
            } else if body.contains("\"bash\"") {
                if answered {
                    text_events("All done.")
                } else {
                    tool_call_events("bash", &format!("{{\"command\":{}}}", json_string(COMMAND)))
                }
            } else {
                text_events("Hello from the stand-in model.")
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
        json_string(arguments)
    )
}

fn text_events(reply: &str) -> String {
    let mut events = String::new();
    for word in reply.split_inclusive(' ') {
        events.push_str(&format!("data: {{\"choices\":[{{\"delta\":{{\"content\":{}}}}}]}}\n\n", json_string(word)));
    }
    events.push_str("data: [DONE]\n\n");
    events
}

fn json_string(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

#[test]
fn the_manager_starts_tasks_that_queue_ask_and_finish() {
    let root = std::env::temp_dir().join(format!("bubble-native-test-{}", std::process::id()));
    let home = root.join("home");
    let project = root.join("project");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(project.join("AGENTS.md"), "Always mention bananas.").unwrap();
    let url = model_server(&project);
    std::fs::write(
        home.join("config.json"),
        format!(
            r#"{{"providers":[{{"id":"mock","name":"Mock","baseURL":"{url}","apiKey":"k","enabled":true}}],"defaultModel":"mock:test-model"}}"#
        ),
    )
    .unwrap();
    // SAFETY: this is the only test in the binary, set before the engine
    // starts any thread that reads the environment.
    unsafe { std::env::set_var("BUBBLE_HOME", &home) };

    let mut app = Headless::start(EngineConfig::new(Root::of::<bubble_app::App>())).unwrap();
    app.set_viewport(nsh_core::element::size(nsh_core::element::px(1280.0), nsh_core::element::px(820.0)))
        .unwrap();
    let long = Duration::from_secs(10);

    // Home: the Manager answers a plain message itself.
    app.wait_for("What should we work on?").unwrap();
    app.wait_for("text:mock:test-model").unwrap();
    app.type_text("text:Message Manager", "Say hello").unwrap();
    app.submit("text:Say hello").unwrap();
    app.wait_for_within("text:/Hello from the stand-in model/", long).unwrap();

    // One task at a time.
    app.press("id:nav-settings").unwrap();
    app.wait_for("Tasks running at once").unwrap();
    for _ in 0..3 {
        app.press("label:Fewer tasks at once").unwrap();
    }
    app.wait_for("1").unwrap();

    // The Manager turns a request into a task, shown as a card; the task's
    // agent starts at once and stops to ask before its command.
    app.press("id:nav-home").unwrap();
    app.type_text("text:Message Manager", "Please ship the notes").unwrap();
    app.submit("text:Please ship the notes").unwrap();
    app.wait_for_within("Started it.", long).unwrap();
    app.wait_for("Ship the notes").unwrap();
    app.wait_for_within("text:/^Needs you, project$/", long).unwrap();

    // A second task, started by the person, waits for the free slot.
    app.press("id:nav-tasks").unwrap();
    app.press("text:New task").unwrap();
    app.type_text("text:What needs doing?", "Second task").unwrap();
    // The form starts on the latest project.
    app.press("Create").unwrap();
    app.type_text("text:Message this task", "Write the file").unwrap();
    app.submit("text:Write the file").unwrap();
    app.wait_for("Queued until another task finishes").unwrap();

    // The first task gets its yes and finishes, which starts the second.
    app.press("id:side-Ship the notes").unwrap();
    app.wait_for("From Manager").unwrap();
    app.wait_for("Write out.txt with the word shipped.").unwrap();
    app.press("Allow once").unwrap();
    app.wait_for_within("All done.", long).unwrap();
    assert_eq!(std::fs::read_to_string(project.join("out.txt")).unwrap().trim(), "shipped");
    std::fs::remove_file(project.join("out.txt")).unwrap();
    assert!(project.join("rules.seen").exists(), "the project's AGENTS.md reached the task's system prompt");

    app.press("id:side-Second task").unwrap();
    app.wait_for_within("Allow once", long).unwrap();
    app.press("Allow once").unwrap();
    app.wait_for_within("All done.", long).unwrap();
    assert_eq!(std::fs::read_to_string(project.join("out.txt")).unwrap().trim(), "shipped");

    // Stop ends a running command at once, and the task can go on.
    app.type_text("text:Message this task", "Wait for me").unwrap();
    app.submit("text:Wait for me").unwrap();
    app.wait_for_within("Allow once", long).unwrap();
    app.press("Allow once").unwrap();
    let started = std::time::Instant::now();
    app.press("label:Stop").unwrap();
    app.wait_for_within("Stopped", long).unwrap();
    app.wait_for("text:Ran sleep 20").unwrap();
    assert!(started.elapsed() < Duration::from_secs(10), "stopping does not wait for the command");
    app.wait_for("label:Send").unwrap();

    std::fs::remove_dir_all(&root).ok();
}
