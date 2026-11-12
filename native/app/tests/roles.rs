//! Bots are roles, headless: three issues come in from two repositories and
//! the main bot hands them to the one developer. The first starts at once;
//! the second, which finds the developer busy, goes to a copy of it, in a
//! checkout of its own of the same repository; the third, in a repository
//! that only prepares, waits for the person's yes in the one digest, and
//! then the developer does it in that other folder, in the same
//! conversation.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::time::Duration;

use bubble_store::{NewTask, Origin, Store};
use nsh_engine::{EngineConfig, Root};
use nsh_testing::Headless;
use serde_json::{Value, json};

fn say(text: &str) -> String {
    format!("data: {{\"choices\":[{{\"delta\":{{\"content\":{}}}}}]}}\n\ndata: [DONE]\n\n", serde_json::to_string(text).unwrap())
}

/// Several tool calls in one reply.
fn calls(name: &str, each: &[Value]) -> String {
    let list: Vec<Value> = each
        .iter()
        .enumerate()
        .map(|(index, arguments)| json!({ "index": index, "id": format!("call_{index}"), "function": { "name": name, "arguments": arguments.to_string() } }))
        .collect();
    format!("data: {}\n\ndata: [DONE]\n\n", json!({ "choices": [{ "delta": { "tool_calls": list }, "finish_reason": "tool_calls" }] }))
}

fn events(body: &str) -> String {
    let request: Value = serde_json::from_str(body).unwrap_or_default();
    let messages = request["messages"].as_array().cloned().unwrap_or_default();
    let system = messages.first().and_then(|m| m["content"].as_str()).unwrap_or_default();
    let last = messages.last().cloned().unwrap_or_default();
    let said = last["content"].as_str().unwrap_or_default();
    // The main bot, handing out what GitHub brought: every issue is the
    // developer's, to start where the repository lets it.
    if system.contains("sorting what came in from GitHub") {
        if last["role"] == "tool" {
            return say("Sorted.");
        }
        let gives: Vec<Value> = said
            .lines()
            .filter(|line| line.starts_with("evt_"))
            .map(|line| {
                let number = line.split('#').nth(1).and_then(|rest| rest.split_whitespace().next()).unwrap_or_default();
                json!({
                    "event_id": line.split(" | ").next().unwrap_or_default(),
                    "bot": "Dev",
                    "title": format!("Fix #{number}"),
                    "brief": format!("Fix issue #{number}."),
                    "start": true
                })
            })
            .collect();
        return calls("give_event_to_bot", &gives);
    }
    // A bot says who it is and where it works: what its prompt told it.
    if let Some(name) = system.split("You are \"").nth(1).and_then(|rest| rest.split('"').next()) {
        // The developer is slow on its first piece of work, so the next finds it busy.
        if name == "Dev" && said.contains("Fix issue #12.") {
            std::thread::sleep(Duration::from_millis(2500));
        }
        let folder = system
            .split("you work in the project folder ")
            .nth(1)
            .and_then(|rest| rest.split(": the work at hand").next())
            .and_then(|path| Path::new(path).file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "no folder".to_owned());
        let apart = if system.contains("a checkout of your own of the same repository") { ", in a checkout of its own" } else { "" };
        let folder = if apart.is_empty() { folder } else { "demo".to_owned() };
        return say(&format!("{name} did it in {folder}{apart}."));
    }
    say("Noted.")
}

fn model_server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            // Each on its own: a slow reply holds up no other bot.
            std::thread::spawn(move || {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let (mut length, mut first) = (0usize, String::new());
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
                let events = events(&String::from_utf8_lossy(&body));
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

/// A fake `gh`: two issues opened in acme/demo, one in acme/tools.
fn fake_gh(dir: &Path) -> std::path::PathBuf {
    let script = dir.join("gh");
    std::fs::write(
        &script,
        r#"#!/bin/sh
case "$*" in
  "api user --jq .login") echo tester ;;
  api\ notifications*) echo '[]' ;;
  api\ repos/acme/demo/issues\?state=open*)
    echo '[{"number":12,"title":"Crash on start","created_at":"2099-01-01T00:00:00Z","user":{"login":"alice","type":"User"}},{"number":13,"title":"Wrong total","created_at":"2099-01-01T00:01:00Z","user":{"login":"bob","type":"User"}}]' ;;
  api\ repos/acme/tools/issues\?state=open*)
    echo '[{"number":5,"title":"Flag ignored","created_at":"2099-01-01T00:02:00Z","user":{"login":"carol","type":"User"}}]' ;;
  issue\ view*) echo '{"title":"An issue","state":"OPEN","url":"https://github.com/acme/x/issues/1","comments":[]}' ;;
  *) echo "unexpected: $*" >&2; exit 1 ;;
esac
"#,
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    script
}

fn repository(folder: &Path, remote: &str) {
    std::fs::create_dir_all(folder).unwrap();
    std::fs::write(folder.join("README.md"), "A project.\n").unwrap();
    for args in [
        vec!["init", "-q"],
        vec!["remote", "add", "origin", remote],
        vec!["add", "."],
        vec!["-c", "user.name=t", "-c", "user.email=t@example.com", "commit", "-q", "-m", "first"],
    ] {
        assert!(std::process::Command::new("git").args(&args).current_dir(folder).status().unwrap().success(), "git {args:?}");
    }
}

#[test]
fn work_goes_to_a_role_and_the_folder_comes_with_the_work() {
    let root = std::env::temp_dir().join(format!("bubble-roles-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let (demo, tools) = (root.join("demo"), root.join("tools"));
    std::fs::create_dir_all(&home).unwrap();
    repository(&demo, "https://github.com/acme/demo.git");
    repository(&tools, "https://github.com/acme/tools.git");
    let url = model_server();
    std::fs::write(
        home.join("config.json"),
        format!(r#"{{"providers":[{{"id":"mock","name":"Mock","baseURL":"{url}","apiKey":"k","enabled":true}}],"defaultModel":"mock:test-model"}}"#),
    )
    .unwrap();
    // One developer, tied to no folder; two known projects, one where bots
    // may start on their own and one where they wait for a yes.
    {
        let store = Store::open(home.join("native/bubble.db")).unwrap();
        let dev = store.create_task(NewTask { title: "Dev", project_cwd: None, origin: Origin::User }).unwrap();
        store.set_setting(&format!("bot_role:{}", dev.id), Some("Developer")).unwrap();
        store.set_setting("projects", Some(&json!([demo.to_string_lossy(), tools.to_string_lossy()]).to_string())).unwrap();
        store.set_setting("github:acme/demo", Some("act")).unwrap();
        store.set_setting("github:acme/tools", Some("prepare")).unwrap();
    }
    let gh = fake_gh(&root);
    // SAFETY: the only test in this binary, before the engine starts.
    unsafe {
        std::env::set_var("BUBBLE_HOME", &home);
        std::env::set_var("BUBBLE_GH", &gh);
        std::env::set_var("BUBBLE_GH_POLL_MS", "300");
        std::env::set_var("BUBBLE_TRIAGE_QUIET_MS", "0");
        std::env::set_var("BUBBLE_NO_SHELL_ENV", "1");
        std::env::set_var("BUBBLE_MEMORY_AUTO", "0");
    }
    let mut app = Headless::start(EngineConfig::new(Root::of::<bubble_app::App>())).unwrap();
    app.set_viewport(nsh_core::element::size(nsh_core::element::px(1280.0), nsh_core::element::px(900.0))).unwrap();
    let long = Duration::from_secs(20);

    // The developer shows with its role.
    app.wait_for_within("id:side-Dev", long).unwrap();
    app.wait_for("Developer").unwrap();

    // The second issue found the developer busy: a copy of it took it, so
    // it did not wait. Both are developers; no bot was made per issue.
    if let Err(e) = app.wait_for_within("id:side-Dev 2", long) {
        panic!("{e}\nTEXTS: {:?}", app.screen().texts());
    }
    assert_eq!(app.screen().count("text:Developer").unwrap(), 2, "the copy has the role too");
    assert!(!app.shows("id:side-Dev 3"), "the work that waits for a yes made no bot");

    // Each did its issue in the repository's folder, which came with the
    // work; the copy in a checkout of its own, since the developer was in
    // that folder.
    app.press("id:side-Dev").unwrap();
    app.wait_for_within("Dev did it in demo.", long).unwrap();
    app.press("id:side-Dev 2").unwrap();
    app.wait_for_within("Dev 2 did it in demo, in a checkout of its own.", long).unwrap();

    // What waits for a yes is in one place, with the bot that would take it.
    app.press("id:nav-home").unwrap();
    app.wait_for_within("1 thing wants your yes", long).unwrap();
    app.wait_for("text:Fix #5").unwrap();
    app.wait_for("text:acme/tools#5, for Dev").unwrap();
    app.press("label:Give: Fix #5").unwrap();
    app.wait_for_gone("1 thing wants your yes", long).unwrap();

    // The developer, free again, does it: in the other repository's
    // folder, in the same conversation as before.
    app.press("id:side-Dev").unwrap();
    app.wait_for_within("Dev did it in tools.", long).unwrap();
    app.wait_for("Dev did it in demo.").unwrap();
    assert!(!app.shows("id:side-Dev 3"));

    drop(app);
    std::fs::remove_dir_all(&root).ok();
}
