//! The main bot's conversation does not end: its agent goes on from an
//! empty context, told what the app keeps outside the conversation, and a
//! line across the conversation marks where its memory of it ends.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use nsh_engine::{EngineConfig, Root};
use nsh_testing::Headless;

/// A stand-in model, streaming OpenAI's format: it answers the Nth chat
/// request with "Answer N." and keeps what each request held.
fn model_server() -> (String, Arc<Mutex<Vec<serde_json::Value>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let kept = seen.clone();
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
            // The host also asks which models there are; that is no turn.
            let events = if request["messages"].is_array() {
                let mut seen = kept.lock().unwrap();
                seen.push(request);
                format!("data: {{\"choices\":[{{\"delta\":{{\"content\":\"Answer {}.\"}}}}]}}\n\ndata: [DONE]\n\n", seen.len())
            } else {
                "{\"data\":[]}".to_owned()
            };
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{events}",
                events.len()
            );
            stream.write_all(response.as_bytes()).ok();
        }
    });
    (format!("http://{address}"), seen)
}

/// Everything a request's messages of `role` said.
fn said(request: &serde_json::Value, role: &str) -> String {
    request["messages"]
        .as_array()
        .map(|messages| messages.iter().filter(|m| m["role"] == role).filter_map(|m| m["content"].as_str()).collect::<Vec<_>>().join("\n"))
        .unwrap_or_default()
}

/// Where each of `texts` first shows on the screen, top to bottom.
fn order(app: &Headless, texts: &[&str]) -> Vec<usize> {
    let shown = app.screen().texts();
    texts.iter().map(|text| shown.iter().position(|t| t.contains(text)).unwrap_or_else(|| panic!("{text:?} is not on screen: {shown:?}"))).collect()
}

#[test]
fn the_main_bot_goes_on_from_an_empty_context_and_the_conversation_stays() {
    let root = std::env::temp_dir().join(format!("bubble-long-conversation-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let (url, seen) = model_server();
    std::fs::write(
        home.join("config.json"),
        format!(r#"{{"providers":[{{"id":"mock","name":"Mock","baseURL":"{url}","apiKey":"k","enabled":true}}],"defaultModel":"mock:test-model"}}"#),
    )
    .unwrap();
    // SAFETY: the only test in this binary, set before the engine starts.
    unsafe {
        std::env::set_var("BUBBLE_HOME", &home);
        std::env::set_var("BUBBLE_MEMORY_AUTO", "0");
    }
    let long = Duration::from_secs(15);
    let viewport = nsh_core::element::size(nsh_core::element::px(1280.0), nsh_core::element::px(900.0));
    {
        let mut app = Headless::start(EngineConfig::new(Root::of::<bubble_app::App>())).unwrap();
        app.set_viewport(viewport).unwrap();
        app.wait_for("What should we work on?").unwrap();

        // Every message goes with how things stand, read from the app's records.
        app.type_text("id:composer", "The harbour code is 4417").unwrap();
        app.submit("id:composer").unwrap();
        app.wait_for_within("text:Answer 1.", long).unwrap();
        {
            let seen = seen.lock().unwrap();
            let first = &seen[0];
            let sent = said(first, "user");
            assert!(sent.contains("The harbour code is 4417"), "{sent}");
            assert!(sent.contains("<conversation_standing>\nHow things stand now ("), "{sent}");
            assert!(sent.contains("There are no bots yet."), "{sent}");
            assert!(!sent.contains("<conversation_brief>"), "nothing went before: {sent}");
            // It is told that the conversation does not last, and can search it.
            assert!(said(first, "system").contains("This conversation does not last."));
            let tools: Vec<&str> = first["tools"].as_array().unwrap().iter().filter_map(|t| t["function"]["name"].as_str()).collect();
            assert!(tools.contains(&"read_own_chat") && tools.contains(&"remember"), "{tools:?}");
        }
        assert!(!app.shows("Context starts afresh here"));

        // The person asks for a fresh context. Nothing runs, so it starts at once.
        app.press("id:nav-settings").unwrap();
        app.press("id:settings-models").unwrap();
        app.wait_for("text:Main bot's context").unwrap();
        app.press("label:Start the main bot's context afresh").unwrap();
        app.press("id:settings-close").unwrap();
        app.wait_for_within("Context starts afresh here", long).unwrap();

        // The next message reaches an agent that has none of the conversation,
        // only how it ended and what the app kept.
        app.type_text("id:composer", "What next?").unwrap();
        app.submit("id:composer").unwrap();
        app.wait_for_within("text:Answer 2.", long).unwrap();
        {
            let seen = seen.lock().unwrap();
            let second = &seen[1];
            let sent = said(second, "user");
            assert!(sent.contains("<conversation_brief>\nYour context starts afresh here."), "{sent}");
            assert!(sent.contains("- You were sent: \"The harbour code is 4417\" You answered: \"Answer 1.\""), "{sent}");
            assert!(sent.contains("list_bots and get_bot show every bot"), "{sent}");
            assert!(sent.contains("What next?") && sent.contains("<conversation_standing>"), "{sent}");
            assert_eq!(said(second, "assistant"), "", "the earlier reply is quoted, not replayed");
        }
        // The person still sees all of it, with a line where the bot's memory ends.
        let at = order(&app, &["The harbour code is 4417", "Answer 1.", "Context starts afresh here", "What next?", "Answer 2."]);
        assert!(at.windows(2).all(|pair| pair[0] < pair[1]), "{at:?}");

        // A third message: the briefing was given once.
        app.type_text("id:composer", "And then?").unwrap();
        app.submit("id:composer").unwrap();
        app.wait_for_within("text:Answer 3.", long).unwrap();
        let seen = seen.lock().unwrap();
        let third = said(&seen[2], "user");
        assert_eq!(third.matches("<conversation_brief>").count(), 1, "{third}");
        assert_eq!(said(&seen[2], "assistant"), "Answer 2.");
    }

    // After a restart the line is where it was, and the agent is on the same thread.
    let mut app = Headless::start(EngineConfig::new(Root::of::<bubble_app::App>())).unwrap();
    app.set_viewport(viewport).unwrap();
    app.wait_for_within("Context starts afresh here", long).unwrap();
    let at = order(&app, &["Answer 1.", "Context starts afresh here", "What next?", "Answer 2.", "And then?", "Answer 3."]);
    assert!(at.windows(2).all(|pair| pair[0] < pair[1]), "{at:?}");
    app.type_text("id:composer", "Still there?").unwrap();
    app.submit("id:composer").unwrap();
    app.wait_for_within("text:Answer 4.", long).unwrap();
    let seen = seen.lock().unwrap();
    assert_eq!(said(&seen[3], "assistant"), "Answer 2.\nAnswer 3.");
    assert!(!app.shows("text:/Context starts afresh here.*Context starts afresh here/"));

    drop(app);
    std::fs::remove_dir_all(&root).ok();
}
