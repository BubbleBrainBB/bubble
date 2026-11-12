//! The agent asks the person a question with choices, waits for the pick,
//! and goes on with it.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::time::Duration;

use nsh_engine::{EngineConfig, Root};
use nsh_testing::Headless;

/// A stand-in model: it asks which format to use, then says what was picked.
fn model_server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut length = 0usize;
            let mut first = String::new();
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
                if first.is_empty() {
                    first = line.clone();
                }
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap_or(0);
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).ok();
            if !first.starts_with("POST ") {
                stream.write_all(b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n").ok();
                continue;
            }
            let request: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
            let results: Vec<String> = request["messages"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .filter(|m| m["role"] == "tool")
                .map(|m| m["content"].as_str().unwrap_or_default().to_owned())
                .collect();
            let asked_twice = request["messages"].to_string().contains("Ask me again");
            let events = match (results.len(), asked_twice) {
                (0, _) | (1, true) => tool_call_events(
                    "question",
                    r#"{"questions":[{"header":"Format","question":"Which format should the notes use?","options":[{"label":"Markdown","description":"Headings and lists"},{"label":"Plain text","description":"No markup"}],"custom":false}]}"#,
                ),
                (n, _) => text_events(&format!("The answer was: {}", results[n - 1].replace('\n', " "))),
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

#[test]
fn the_agent_asks_and_goes_on_with_the_answer() {
    let root = std::env::temp_dir().join(format!("bubble-native-question-{}", std::process::id()));
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
        std::env::set_var("BUBBLE_MEMORY_AUTO", "0");
        std::env::set_var("BUBBLE_NO_SHELL_ENV", "1");
        std::env::set_var("BUBBLE_GH", root.join("no-gh"));
    }
    bubble_store::Store::open(home.join("native/bubble.db"))
        .unwrap()
        .create_task(bubble_store::NewTask { title: "Notes", project_cwd: Some(project.to_str().unwrap()), origin: bubble_store::Origin::User })
        .unwrap();

    let mut app = Headless::start(EngineConfig::new(Root::of::<bubble_app::App>())).unwrap();
    app.set_viewport(nsh_core::element::size(nsh_core::element::px(1280.0), nsh_core::element::px(1400.0))).unwrap();
    let long = Duration::from_secs(15);
    app.wait_for_within("id:nav-home", long).unwrap();
    app.press("id:side-Notes").unwrap();

    // The agent asks; the bot shows as waiting for the person, the question
    // stays out of the folded work, and picking a choice answers it.
    app.type_text("id:composer", "Write the notes").unwrap();
    app.submit("text:Write the notes").unwrap();
    app.wait_for_within("Which format should the notes use?", long).unwrap();
    app.wait_for("text:Waiting for your answer").unwrap();
    app.wait_for("text:Waiting for you").unwrap();
    app.wait_for("Headings and lists").unwrap();
    app.press("label:Answer Markdown").unwrap();
    app.wait_for_within("text:/^The answer was: .*Markdown/", long).unwrap();
    assert!(!app.shows("text:Waiting for you"), "nothing waits on the person once they answered");
    // The answered question is part of the turn's work, folded with it.
    app.wait_for("text:/^Worked for /").unwrap();
    app.press("label:Worked").unwrap();
    app.wait_for("text:Answered").unwrap();
    app.wait_for("Which format should the notes use?").unwrap();

    // Skipping tells the agent the person will not answer.
    app.type_text("id:composer", "Ask me again").unwrap();
    app.submit("text:Ask me again").unwrap();
    app.wait_for_within("Skip", long).unwrap();
    app.press("Skip").unwrap();
    app.wait_for_within("text:/^The answer was: .*dismissed/", long).unwrap();
    app.wait_for_within("label:Send", long).unwrap();

    std::fs::remove_dir_all(&root).ok();
}
