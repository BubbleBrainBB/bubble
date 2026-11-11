//! The composer, headless: an `@file` mention sends the file along, a
//! pasted picture goes with the message, Up brings back what was sent, and
//! a task keeps its own model and effort.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

use nsh_core::{PasteEvent, PastedFile};
use nsh_engine::{EngineConfig, Root};
use nsh_testing::Headless;

/// Every request the model got, one per line.
static LOG: OnceLock<PathBuf> = OnceLock::new();

fn events(_body: &str) -> String {
    say("Seen it.")
}

fn composer_shows(app: &mut Headless, expected: &str) {
    for _ in 0..40 {
        if app.screen().input_text("id:composer").ok().flatten().unwrap_or_default() == expected {
            return;
        }
        app.wait(Duration::from_millis(50)).unwrap();
    }
    panic!("the composer shows {:?}, not {expected:?}", app.screen().input_text("id:composer"));
}

fn requests() -> String {
    std::fs::read_to_string(LOG.get().unwrap()).unwrap_or_default()
}

fn call(name: &str, arguments: &str) -> String {
    format!(
        "data: {{\"choices\":[{{\"delta\":{{\"tool_calls\":[{{\"index\":0,\"id\":\"call_{name}\",\"function\":{{\"name\":\"{name}\",\"arguments\":{}}}}}]}},\"finish_reason\":\"tool_calls\"}}]}}\n\ndata: [DONE]\n\n",
        serde_json::to_string(arguments).unwrap()
    )
}

fn say(text: &str) -> String {
    format!("data: {{\"choices\":[{{\"delta\":{{\"content\":{}}}}}]}}\n\ndata: [DONE]\n\n", serde_json::to_string(text).unwrap())
}

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
            let body = String::from_utf8_lossy(&body).into_owned();
            if let Some(log) = LOG.get() {
                let mut file = std::fs::OpenOptions::new().create(true).append(true).open(log).unwrap();
                writeln!(file, "{body}").ok();
            }
            let events = events(&body);
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{events}",
                events.len()
            );
            stream.write_all(response.as_bytes()).ok();
        }
    });
    format!("http://{address}")
}

#[test]
fn the_composer_mentions_files_takes_pictures_and_remembers() {
    let root = std::env::temp_dir().join(format!("bubble-composer-test-{}", std::process::id()));
    let home = root.join("home");
    let project = root.join("demo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(project.join("src")).unwrap();
    std::fs::write(project.join("src/lib.rs"), "fn marker_from_file() {}").unwrap();
    LOG.set(root.join("requests.log")).unwrap();
    let url = model_server();
    std::fs::write(
        home.join("config.json"),
        format!(
            r#"{{"providers":[{{"id":"mock","name":"Mock","baseURL":"{url}","apiKey":"k","enabled":true}},{{"id":"stepfun","name":"StepFun","baseURL":"{url}","apiKey":"k","enabled":true}}],"defaultModel":"mock:test-model","recentModels":["stepfun:step-3"]}}"#
        ),
    )
    .unwrap();
    // SAFETY: the only test in this binary, before the engine starts.
    unsafe {
        std::env::set_var("BUBBLE_HOME", &home);
        std::env::set_var("BUBBLE_GH", root.join("no-gh"));
        std::env::set_var("BUBBLE_NO_SHELL_ENV", "1");
        std::env::set_var("BUBBLE_MEMORY_AUTO", "0");
    }

    // New work starts with the Manager; this test starts from a task already made.
    bubble_store::Store::open(home.join("native/bubble.db"))
        .unwrap()
        .create_task(bubble_store::NewTask { title: "Composer task", project_cwd: Some(project.to_str().unwrap()), origin: bubble_store::Origin::User })
        .unwrap();
    let mut app = Headless::start(EngineConfig::new(Root::of::<bubble_app::App>())).unwrap();
    app.set_viewport(nsh_core::element::size(nsh_core::element::px(1280.0), nsh_core::element::px(820.0))).unwrap();
    let long = Duration::from_secs(10);

    app.wait_for_within("id:nav-tasks", long).unwrap();
    app.press("id:nav-tasks").unwrap();
    app.press("id:side-Composer task").unwrap();

    // An @ mention is completed from the project's files and sent with them.
    app.type_text("text:Message this task", "explain @src/li").unwrap();
    if let Err(e) = app.wait_for_within("label:Mention src/lib.rs", long) {
        panic!("{e}\nTEXTS: {:?}", app.screen().texts());
    }
    app.press("label:Mention src/lib.rs").unwrap();
    app.submit("text:explain @src/lib.rs").unwrap();
    app.wait_for_within("Seen it.", long).unwrap();
    assert!(requests().contains("marker_from_file"), "the mentioned file went with the message");
    app.wait_for("text:explain @src/lib.rs").unwrap();

    // A pasted picture goes with the next message.
    let png = vec![0x89, b'P', b'N', b'G', 13, 10, 26, 10, 0, 0, 0, 0];
    let mut paste = PasteEvent::default();
    paste.files.push(PastedFile::new("shot.png", "image/png", png));
    app.paste("text:Message this task", &paste).unwrap();
    app.wait_for("label:Image 1").unwrap();
    app.type_text("text:Message this task", "what is this").unwrap();
    app.submit("text:what is this").unwrap();
    app.wait_for_within("1 image", long).unwrap();
    app.wait_for_within("label:Send", long).unwrap();
    assert!(requests().contains("data:image/png;base64,iVBORw0K"), "the picture reached the model");
    assert!(!app.shows("label:Image 1"), "sent pictures leave the composer");

    // Up brings back the last message, then the one before.
    app.type_text("text:Message this task", "x").unwrap();
    app.press_key("backspace").unwrap();
    app.press_key("up").unwrap();
    composer_shows(&mut app, "what is this");
    app.press_key("up").unwrap();
    composer_shows(&mut app, "explain @src/lib.rs");
    app.press_key("down").unwrap();
    composer_shows(&mut app, "what is this");
    app.press_key("down").unwrap();
    composer_shows(&mut app, "");

    // The task's own model and effort.
    app.press("label:Model").unwrap();
    app.press("label:stepfun:step-3").unwrap();
    app.wait_for("text:stepfun:step-3").unwrap();
    app.press("label:Effort").unwrap();
    app.press("label:high").unwrap();
    app.wait_for("text:/^high$/").unwrap();
    app.type_text("text:Message this task", "once more").unwrap();
    app.submit("text:once more").unwrap();
    app.wait_for_within("label:Send", long).unwrap();
    // The agent writes this provider's requests over several lines.
    let sent = requests().replace("\": \"", "\":\"");
    assert!(sent.contains("\"model\":\"step-3\""), "the task's own model ran");

    std::fs::remove_dir_all(&root).ok();
}
