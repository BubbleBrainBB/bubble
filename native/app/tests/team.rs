//! Bots working together: one bot asks another and gets its answer back,
//! and the main bot hears from the bot it asked only once that bot has its
//! own answer in.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::time::Duration;

use nsh_engine::{EngineConfig, Root};
use nsh_testing::Headless;

/// A stand-in model that plays three parts, told apart by their prompts:
/// the main bot hands "consult" to Writer and relays its report; Writer
/// asks Checker, waits, and passes on what Checker said; Checker answers.
fn model_server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            std::thread::spawn(move || {
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
                    return;
                }
                let request: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
                let system = request["messages"][0]["content"].as_str().unwrap_or_default().to_owned();
                // What was said, without the reminders the agent adds for the model.
                let last = request["messages"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .skip(1)
                    .filter(|m| !m["content"].as_str().unwrap_or_default().starts_with("<bubble_internal_reminder"))
                    .last()
                    .unwrap_or_default();
                let said = last["content"].as_str().unwrap_or_default().to_owned();
                let second_line = said.lines().nth(1).unwrap_or_default().to_owned();
                let from_tool = last["role"] == "tool";
                let events = if system.starts_with("You are Bubble, the person's main bot") {
                    if said.starts_with("[Report from the bot Writer: finished]") {
                        text_events(&format!("Writer reported: {second_line}"))
                    } else if from_tool {
                        text_events("Asked Writer.")
                    } else {
                        tool_call_events("message_bot", r#"{"bot":"Writer","message":"Consult the checker about x"}"#)
                    }
                // A bot goes by its own name from the first words of its prompt.
                } else if system.starts_with("You are Writer,") {
                    if said.starts_with("[Report from the bot Checker: finished]") {
                        text_events(&format!("Checker says: {second_line}"))
                    } else if from_tool {
                        text_events("Waiting for Checker.")
                    } else {
                        tool_call_events("message_bot", r#"{"bot":"Checker","message":"Check x"}"#)
                    }
                } else if system.starts_with("You are Checker,") {
                    text_events(&format!("Checked: fine. You asked: {second_line}"))
                } else {
                    text_events("Unexpected speaker.")
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
fn bots_ask_each_other_and_answers_go_back_to_whoever_asked() {
    let root = std::env::temp_dir().join(format!("bubble-native-team-{}", std::process::id()));
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
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
    let store = bubble_store::Store::open(home.join("native/bubble.db")).unwrap();
    for name in ["Writer", "Checker"] {
        store.create_task(bubble_store::NewTask { title: name, project_cwd: None, origin: bubble_store::Origin::User }).unwrap();
    }
    drop(store);

    let mut app = Headless::start(EngineConfig::new(Root::of::<bubble_app::App>())).unwrap();
    app.set_viewport(nsh_core::element::size(nsh_core::element::px(1280.0), nsh_core::element::px(1400.0))).unwrap();
    let long = Duration::from_secs(15);
    app.wait_for_within("id:nav-home", long).unwrap();

    // The person asks the main bot, which hands the work to Writer.
    app.type_text("id:composer", "Please consult on x").unwrap();
    app.submit("text:Please consult on x").unwrap();
    app.wait_for_within("Asked Writer.", long).unwrap();

    // Writer asked Checker and waits; Checker's answer comes back to it, and
    // only then does the main bot hear from Writer, once.
    app.wait_for_within("Writer reported: Checker says: Checked: fine. You asked: Check x", long).unwrap();
    // The main bot's conversation marks, in passing, that it wrote to
    // Writer and that Writer's answer came, once, when it was done.
    app.wait_for("label:Message from Writer").unwrap();
    assert_eq!(app.screen().count("text:Message from").unwrap(), 1, "the main bot heard from Writer once, when it was done");
    assert_eq!(app.screen().count("text:Messaged").unwrap(), 1);
    assert!(!app.shows("text:/Writer reported: Waiting/"), "a bot still waiting on another has nothing to report yet");
    // The answer itself is in Writer's conversation: a press on the mark goes there.
    assert!(!app.shows("text:/^Checker says: Checked: fine/"));
    app.press("label:Message from Writer").unwrap();
    app.wait_for("text:Message Writer").unwrap();
    app.wait_for("text:/^Checker says: Checked: fine/").unwrap();
    // From there, the mark of Checker's answer goes on to Checker's.
    app.wait_for("From Bubble").unwrap();
    app.press("label:Message from Checker").unwrap();
    app.wait_for("text:Message Checker").unwrap();

    // What is asked of a bot shows where the person's own words would,
    // with who asked; an answer that came back to it is a mark.
    app.wait_for("From Writer").unwrap();
    app.wait_for("Check x").unwrap();
    app.press("id:side-Writer").unwrap();
    app.wait_for("From Bubble").unwrap();
    app.wait_for("Consult the checker about x").unwrap();
    // It wrote to Checker, and Checker's answer came back to it.
    app.wait_for("text:Messaged").unwrap();
    app.wait_for("label:Message from Checker").unwrap();
    app.wait_for("Checker says: Checked: fine. You asked: Check x").unwrap();

    std::fs::remove_dir_all(&root).ok();
}
