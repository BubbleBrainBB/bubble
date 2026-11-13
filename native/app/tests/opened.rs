//! Newly opened issues, headless: GitHub tells the person nothing of an
//! issue nobody assigned to them, so the app looks at what was opened in a
//! watched repository itself. Someone's issue is weighed by the triage
//! session (a stand-in model) and proposed to the repository's bot; a
//! dependency updater's pull request is only written down.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::time::Duration;

use nsh_engine::{EngineConfig, Root};
use nsh_testing::Headless;

fn events(body: &str) -> String {
    let request: serde_json::Value = serde_json::from_str(body).unwrap_or_default();
    let messages = request["messages"].as_array().cloned().unwrap_or_default();
    let system = messages.first().and_then(|m| m["content"].as_str()).unwrap_or_default();
    let last = messages.last().cloned().unwrap_or_default();
    if system.contains("sorting what came in from GitHub") {
        if last["role"] == "tool" {
            return say("Gave the crash to the repository's bot.");
        }
        let listing = last["content"].as_str().unwrap_or_default();
        // Only the person's issue is to decide, and it says who opened it.
        let lines: Vec<&str> = listing.lines().filter(|l| l.starts_with("evt_")).collect();
        if lines.len() != 1 || !lines[0].contains("opened by @alice in acme/demo #12") {
            return say(&format!("Unexpected listing: {listing}"));
        }
        let id = lines[0].split(" | ").next().unwrap_or_default();
        let arguments = serde_json::json!({
            "event_id": id,
            "bot": "Set up",
            "title": "Fix the crash on start",
            "brief": "@alice opened acme/demo#12: the app crashes on start. Read it with `gh issue view 12 --repo acme/demo` and fix it.",
            "start": false
        });
        return call("give_event_to_bot", &arguments.to_string());
    }
    say("Looking into it.")
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

/// A fake `gh`: no notifications; the repository lists an issue someone
/// opened and a pull request of a dependency updater.
fn fake_gh(dir: &Path) -> std::path::PathBuf {
    let script = dir.join("gh");
    std::fs::write(
        &script,
        r#"#!/bin/sh
case "$*" in
  "api user --jq .login") echo tester ;;
  api\ notifications*) echo '[]' ;;
  api\ repos/acme/demo/issues\?state=open*)
    echo '[{"number":12,"title":"Crash on start","created_at":"2099-01-01T00:00:00Z","user":{"login":"alice","type":"User"}},{"number":13,"title":"Bump serde","created_at":"2099-01-01T00:05:00Z","user":{"login":"dependabot[bot]","type":"Bot"},"pull_request":{"url":"x"}}]' ;;
  "issue view 12 --repo acme/demo"*)
    echo '{"title":"Crash on start","state":"OPEN","url":"https://github.com/acme/demo/issues/12","comments":[]}' ;;
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

#[test]
fn a_newly_opened_issue_is_weighed_and_proposed_to_the_repositorys_bot() {
    let root = std::env::temp_dir().join(format!("bubble-opened-test-{}", std::process::id()));
    let home = root.join("home");
    let project = root.join("demo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&project).unwrap();
    for args in [vec!["init", "-q"], vec!["remote", "add", "origin", "https://github.com/acme/demo.git"]] {
        assert!(std::process::Command::new("git").args(&args).current_dir(&project).status().unwrap().success());
    }
    let url = model_server();
    std::fs::write(
        home.join("config.json"),
        format!(r#"{{"providers":[{{"id":"mock","name":"Mock","baseURL":"{url}","apiKey":"k","enabled":true}}],"defaultModel":"mock:test-model"}}"#),
    )
    .unwrap();
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

    // New work starts with the Manager; this test starts from a task already made.
    bubble_store::Store::open(home.join("native/bubble.db"))
        .unwrap()
        .create_task(bubble_store::NewTask { title: "Set up", project_cwd: None, origin: bubble_store::Origin::User })
        .unwrap();
    let mut app = Headless::start(EngineConfig::new(Root::of::<bubble_app::App>())).unwrap();
    app.set_viewport(nsh_core::element::size(nsh_core::element::px(1280.0), nsh_core::element::px(820.0))).unwrap();
    let long = Duration::from_secs(15);

    app.wait_for_within("id:nav-home", long).unwrap();
    // No bot has worked anywhere yet, so no project is known: the person
    // adds the folder in Settings, and its repository shows, to be watched.
    app.press("id:nav-settings").unwrap();
    app.press("id:settings-projects").unwrap();
    app.wait_for("text:/^None yet\\. Add one/").unwrap();
    app.on_pick_paths(vec![project.clone()]).unwrap();
    app.press("Add a project folder").unwrap();
    app.wait_for_within("acme/demo", long).unwrap();
    app.press("label:Level for acme/demo").unwrap();
    app.press("label:Prepare for acme/demo").unwrap();
    app.wait_for("text:Prepare").unwrap();
    app.press("id:settings-close").unwrap();

    // The issue is weighed and proposed to the repository's bot; the
    // updater's pull request is written down, and Home says both.
    app.press("id:nav-home").unwrap();
    if let Err(e) = app.wait_for_within("text:/While you were away, GitHub brought 2 things: 1 work for a bot, 1 recorded/", long) {
        panic!("{e}\nTEXTS: {:?}", app.screen().texts());
    }
    app.press("label:While you were away").unwrap();
    app.wait_for("text:/acme\\/demo #13 opened: Bump serde \\(recorded\\)/").unwrap();
    app.wait_for("text:/acme\\/demo #12 opened: Crash on start \\(work for a bot\\)/").unwrap();
    app.press("text:/acme\\/demo #12 opened/").unwrap();
    // It opens the bot that looks after the repository, with the work
    // waiting for a yes there.
    app.wait_for("text:Message Set up").unwrap();
    app.wait_for("Fix the crash on start").unwrap();
    app.wait_for("Give it to the bot").unwrap();

    std::fs::remove_dir_all(&root).ok();
}
