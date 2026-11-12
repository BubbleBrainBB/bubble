//! A task's menu, headless: run it again on a model (a new run, its divider
//! saying why), link a PR, cancel it.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::time::Duration;

use nsh_engine::{EngineConfig, Root};
use nsh_testing::Headless;

fn events(_body: &str) -> String {
    say("Hello from the stand-in model.")
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
            let events = events(&String::from_utf8_lossy(&body));
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
fn a_task_runs_again_links_a_pr_and_is_canceled() {
    let root = std::env::temp_dir().join(format!("bubble-task-menu-test-{}", std::process::id()));
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let url = model_server();
    std::fs::write(
        home.join("config.json"),
        format!(r#"{{"providers":[{{"id":"mock","name":"Mock","baseURL":"{url}","apiKey":"k","enabled":true}}],"defaultModel":"mock:test-model"}}"#),
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
        .create_task(bubble_store::NewTask { title: "Menu task", project_cwd: None, origin: bubble_store::Origin::User })
        .unwrap();
    let mut app = Headless::start(EngineConfig::new(Root::of::<bubble_app::App>())).unwrap();
    app.set_viewport(nsh_core::element::size(nsh_core::element::px(1280.0), nsh_core::element::px(820.0))).unwrap();
    let long = Duration::from_secs(10);

    app.wait_for_within("id:nav-home", long).unwrap();
    app.press("id:side-Menu task").unwrap();
    app.type_text("id:composer", "Do the thing").unwrap();
    app.submit("text:Do the thing").unwrap();
    app.wait_for_within("Hello from the stand-in model.", long).unwrap();

    // Link a PR by its link.
    app.press("label:Bot actions").unwrap();
    app.press("Link an issue or PR").unwrap();
    app.type_text("text:#12, owner/repo#12 or a GitHub link", "https://github.com/acme/demo/pull/4").unwrap();
    app.press("Link").unwrap();
    app.wait_for("PR #4").unwrap();

    // Archive: the bot stops and leaves the list, and the main bot is back.
    app.press("label:Bot actions").unwrap();
    app.press("Archive bot").unwrap();
    app.wait_for_gone("id:side-Menu task", long).unwrap();
    app.wait_for("What should we work on?").unwrap();

    std::fs::remove_dir_all(&root).ok();
}
