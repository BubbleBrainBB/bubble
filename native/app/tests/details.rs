//! A bot's details: who it is, what it remembers and the routines it runs.
//! The person edits them by hand; the bot changes its notes and routines
//! when asked in chat; and after a turn the person drove, what they said
//! that should last is kept without the bot having to think of it.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::time::Duration;

use nsh_engine::{EngineConfig, Root};
use nsh_testing::Headless;

/// A stand-in model. Asked to review an exchange (a request that does not
/// stream), it keeps one note. Otherwise it saves a shared note or sets up
/// a routine when asked to, and says "Noted." to the rest. The system
/// prompt of the latest streamed request is written to `seen`.
fn model_server(seen: &Path) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let seen = seen.to_path_buf();
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
            if request["stream"] != true {
                // The memory review: one note worth keeping.
                let answer = serde_json::json!({ "choices": [{ "index": 0, "finish_reason": "stop", "message": {
                    "role": "assistant", "content": "{\"add\": [\"Prefers pnpm over npm\"], \"remove\": []}"
                } }] })
                .to_string();
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{answer}",
                    answer.len()
                );
                stream.write_all(response.as_bytes()).ok();
                continue;
            }
            std::fs::write(&seen, request["messages"][0]["content"].as_str().unwrap_or_default()).ok();
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
            let events = if last["role"] == "tool" {
                text_events(&format!("Done: {}", said.lines().next().unwrap_or_default()))
            } else if said.contains("every Friday") {
                tool_call_events(
                    "add_routine",
                    r#"{"name":"Weekly summary","prompt":"Summarize the pull requests merged this week","every":"week","day":"Friday","at":"17:00"}"#,
                )
            } else if said.contains("tell every bot") {
                tool_call_events("remember", r#"{"text":"The release branch is called stable","shared":true}"#)
            } else {
                text_events("Noted.")
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
fn a_bot_is_edited_remembers_and_sets_up_its_routines() {
    let root = std::env::temp_dir().join(format!("bubble-native-details-{}", std::process::id()));
    let home = root.join("home");
    let project = root.join("demo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&project).unwrap();
    let seen = root.join("prompt.txt");
    let url = model_server(&seen);
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
        std::env::set_var("BUBBLE_GH", root.join("no-gh"));
    }
    let bot = bubble_store::Store::open(home.join("native/bubble.db"))
        .unwrap()
        .create_task(bubble_store::NewTask { title: "Helper", project_cwd: Some(project.to_str().unwrap()), origin: bubble_store::Origin::User })
        .unwrap();

    let mut app = Headless::start(EngineConfig::new(Root::of::<bubble_app::App>())).unwrap();
    app.set_viewport(nsh_core::element::size(nsh_core::element::px(1480.0), nsh_core::element::px(2200.0))).unwrap();
    let long = Duration::from_secs(15);
    app.wait_for_within("id:nav-home", long).unwrap();
    app.press("id:side-Helper").unwrap();

    // Its details open from its name over the conversation.
    app.press("id:bot-details-toggle").unwrap();
    app.wait_for("id:bot-details").unwrap();

    // A note the person writes by hand.
    app.type_text("id:memory-add", "Uses tabs, not spaces").unwrap();
    app.press("Add").unwrap();
    app.wait_for("text:Uses tabs, not spaces").unwrap();

    // A new name, what it is for, and another look.
    app.clear("id:details-name").unwrap();
    app.type_text("id:details-name", "Scribe").unwrap();
    app.type_text("id:details-purpose", "Keeps the changelog in step with what ships").unwrap();
    app.press("Save").unwrap();
    app.wait_for("text:Saved").unwrap();
    // The list, the name over the conversation and the composer follow.
    assert!(!app.shows("text:Helper"));
    app.wait_for("text:Message Scribe").unwrap();
    app.press("label:Another shape").unwrap();
    app.press("label:Another color").unwrap();

    // The bot goes by its new name, knows what it is for, and reads its notes.
    app.type_text("id:composer", "Use pnpm from now on").unwrap();
    app.submit("id:composer").unwrap();
    app.wait_for_within("Noted.", long).unwrap();
    let prompt = std::fs::read_to_string(&seen).unwrap();
    assert!(prompt.starts_with("You are Scribe,"), "{}", &prompt[..prompt.len().min(80)]);
    assert!(prompt.contains("Keeps the changelog in step with what ships"));
    assert!(prompt.contains("- Uses tabs, not spaces"));

    // What the person said that should last is kept after the turn, without
    // the bot having called anything.
    app.wait_for_within("text:Prefers pnpm over npm", long).unwrap();

    // Asked in chat, it saves a note for every bot.
    app.type_text("id:composer", "Please tell every bot the release branch name").unwrap();
    app.submit("id:composer").unwrap();
    app.wait_for_within("Done: Saved for every bot.", long).unwrap();
    app.wait_for("text:The release branch is called stable").unwrap();

    // Asked in chat, it sets a routine up for itself.
    app.type_text("id:composer", "Summarize merged pull requests every Friday").unwrap();
    app.submit("id:composer").unwrap();
    app.wait_for_within("text:/^Done: Set up routine .*Weekly summary/", long).unwrap();
    app.wait_for("text:Weekly summary").unwrap();
    app.wait_for("text:/^Every Friday at 17:00, next /").unwrap();

    // A note the person removes is gone, for the bot too.
    app.press("label:Remove note: Uses tabs, not spaces").unwrap();
    app.wait_for_gone("text:Uses tabs, not spaces", long).unwrap();
    drop(app);

    let store = bubble_store::Store::open(home.join("native/bubble.db")).unwrap();
    assert_eq!(store.get_task(&bot.id).unwrap().unwrap().title, "Scribe");
    assert!(store.setting(&format!("bot_face:{}", bot.id)).unwrap().is_some(), "the look the person picked is kept");
    let notes = std::fs::read_to_string(home.join("native/memory/bots").join(format!("{}.md", bot.id))).unwrap();
    assert!(notes.contains("- Prefers pnpm over npm [added: ") && !notes.contains("Uses tabs"), "{notes}");
    let team = std::fs::read_to_string(home.join("native/memory/team.md")).unwrap();
    assert!(team.contains("- The release branch is called stable [added: "), "{team}");

    std::fs::remove_dir_all(&root).ok();
}
