//! The full-screen interactive session: an NSH app drawn in the terminal
//! by `bubble-term`, its agent a [`runner`] task on the runtime.

pub mod runner;
pub mod state;
pub mod view;

use std::sync::{Arc, Mutex};

use nsh_core::prelude::*;
use nsh_engine::{EngineConfig, Root};
use tokio::sync::Notify;

use crate::args::Args;
use state::Tui;

/// What the app starts with; set before the engine starts it.
static LAUNCH: Mutex<Option<(Args, tokio::runtime::Handle)>> = Mutex::new(None);

/// The app's root: the screen, and the runner it talks to.
#[component]
pub fn App() -> impl IntoElement {
    let args = LAUNCH.lock().ok().and_then(|mut l| l.take());
    let stop = Arc::new(Notify::new());
    let tui = Tui::new(stop.clone());
    if let Some((args, runtime)) = args {
        let (commands, incoming) = tokio::sync::mpsc::unbounded_channel();
        let (updates, mut received) = tokio::sync::mpsc::unbounded_channel();
        tui.connect(commands);
        runtime.spawn(runner::run(args, incoming, updates, stop));
        spawn(async move {
            while let Some(update) = received.recv().await {
                tui.apply(update);
            }
        });
    }
    view::screen(tui)
}

/// The engine configuration of the app with `args`, on `runtime`.
pub fn config(args: Args, runtime: tokio::runtime::Handle) -> EngineConfig {
    if let Ok(mut launch) = LAUNCH.lock() {
        *launch = Some((args, runtime.clone()));
    }
    let mut config = EngineConfig::new(Root::of::<App>());
    config.runtime = Some(runtime);
    config
}

/// Takes over the terminal until the person quits.
pub fn run(args: Args, runtime: tokio::runtime::Handle) -> std::io::Result<()> {
    let mouse = std::env::var("BUBBLE_TUI_MOUSE").map(|v| v != "0").unwrap_or(true);
    bubble_term::run(config(args, runtime), bubble_term::Options { mouse })
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::time::Duration;

    use bubble_term::Virtual;

    /// A stand-in model: asked to "make notes" it writes a file, then runs
    /// a command, then says what the tools said.
    fn model_server() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
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
                    let request: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
                    let messages = request["messages"].as_array().cloned().unwrap_or_default();
                    let last = messages.last().cloned().unwrap_or_default();
                    let results: Vec<String> = messages
                        .iter()
                        .filter(|m| m["role"] == "tool")
                        .filter_map(|m| m["content"].as_str().map(str::to_owned))
                        .collect();
                    let call = |name: &str, arguments: serde_json::Value| {
                        format!(
                            "data: {{\"choices\":[{{\"delta\":{{\"tool_calls\":[{{\"index\":0,\"id\":\"call_{name}\",\"function\":{{\"name\":\"{name}\",\"arguments\":{}}}}}]}},\"finish_reason\":\"tool_calls\"}}]}}\n\ndata: {{\"choices\":[],\"usage\":{{\"prompt_tokens\":100,\"completion_tokens\":10}}}}\n\ndata: [DONE]\n\n",
                            serde_json::to_string(&arguments.to_string()).unwrap()
                        )
                    };
                    let say = |reply: &str| {
                        format!(
                            "data: {{\"choices\":[{{\"delta\":{{\"content\":{}}},\"finish_reason\":\"stop\"}}]}}\n\ndata: {{\"choices\":[],\"usage\":{{\"prompt_tokens\":50,\"completion_tokens\":5}}}}\n\ndata: [DONE]\n\n",
                            serde_json::to_string(reply).unwrap()
                        )
                    };
                    let events = if last["role"] == "tool" {
                        match results.len() {
                            1 => call("bash", serde_json::json!({ "command": "echo ran > ran.txt" })),
                            _ => say(&format!("Done. **Tools** said: {}", results.join(" | "))),
                        }
                    } else if last["content"].as_str().unwrap_or_default().contains("make notes") {
                        call("write", serde_json::json!({ "path": "notes.txt", "content": "hello" }))
                    } else {
                        say(&format!("You said: {}", last["content"].as_str().unwrap_or_default()))
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

    #[test]
    fn the_screen_runs_a_conversation() {
        let root = std::env::temp_dir().join(format!("bubble-tui-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (home, project) = (root.join("home"), root.join("project"));
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            home.join("config.json"),
            format!(
                r#"{{"providers":[{{"id":"mock","name":"Mock","baseURL":"{}","apiKey":"k","enabled":true}}],"defaultModel":"mock:test-model","recentModels":["mock:other-model"]}}"#,
                model_server()
            ),
        )
        .unwrap();
        // SAFETY: set before anything reads them; no other test reads them.
        unsafe {
            std::env::set_var("BUBBLE_HOME", &home);
            std::env::set_var("BUBBLE_NO_SHELL_ENV", "1");
            std::env::set_var("BUBBLE_WEB_SEARCH_URL", "http://127.0.0.1:9/mcp");
        }
        let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        let argv = vec!["--cwd".to_owned(), project.display().to_string()];
        let args = crate::args::parse(&argv, std::env::current_dir().unwrap()).unwrap();
        let mut term = Virtual::start(super::config(args, runtime.handle().clone()), 100, 32).unwrap();
        let wait = |term: &mut Virtual, text: &str| term.wait_for(text, Duration::from_secs(10)).unwrap();
        wait(&mut term, "mock:test-model · high");
        wait(&mut term, "Ask Bubble, or / for commands");
        term.type_text("make notes");
        term.key("enter");
        wait(&mut term, "Run a command");
        let screen = term.text();
        println!("{screen}");
        assert!(screen.contains("› make notes"));
        assert!(screen.contains("Wrote notes.txt"));
        assert!(screen.contains("[y] Yes"));
        term.key("y");
        wait(&mut term, "Done. Tools said:");
        assert_eq!(std::fs::read_to_string(project.join("ran.txt")).unwrap().trim(), "ran");
        println!("{}", term.text());

        // Commands: the suggestions, then the model picker.
        term.type_text("/mo");
        wait(&mut term, "Switch model: /model");
        term.key("enter");
        wait(&mut term, "↑↓ choose");
        term.key("down");
        term.key("enter");
        wait(&mut term, "Model: mock:other-model");
        wait(&mut term, "mock:other-model · ");
        println!("{}", term.text());

        term.key("ctrl+c");
        assert!(term.exited, "Ctrl+C on an empty composer quits");
        drop(term);
        runtime.shutdown_timeout(Duration::from_millis(200));
    }
}
