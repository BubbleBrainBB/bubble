//! GitHub, headless: a fake `gh` and a stand-in model drive an assigned
//! issue from the notifications to a task, a comment back to its agent, and
//! the agent's reply comment through an approval.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::time::Duration;

use nsh_engine::{EngineConfig, Root};
use nsh_testing::Headless;

/// A task's stand-in agent: after a tool result it says "Commented."; told
/// about the new comment it comments back; otherwise "Looking into it."
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
            let body = String::from_utf8_lossy(&body);
            let events = if body.contains("\"tool_call_id\"") {
                text_events("Commented.")
            } else if body.contains("empty passwords") {
                let arguments = "{\"number\":7,\"body\":\"Handled empty passwords too.\"}";
                format!(
                    "data: {{\"choices\":[{{\"delta\":{{\"tool_calls\":[{{\"index\":0,\"id\":\"call_1\",\"function\":{{\"name\":\"github_comment\",\"arguments\":{}}}}}]}},\"finish_reason\":\"tool_calls\"}}]}}\n\ndata: [DONE]\n\n",
                    json_string(arguments)
                )
            } else {
                text_events("Looking into it.")
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

fn text_events(reply: &str) -> String {
    format!("data: {{\"choices\":[{{\"delta\":{{\"content\":{}}}}}]}}\n\ndata: [DONE]\n\n", json_string(reply))
}

fn json_string(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

/// A fake `gh`: one assigned issue in the notifications (once), the issue
/// with a new comment once `comment.flag` exists, and posted comments
/// appended to `posted.log`.
fn fake_gh(dir: &Path) -> std::path::PathBuf {
    let script = dir.join("gh");
    let d = dir.display();
    std::fs::write(
        &script,
        format!(
            r#"#!/bin/sh
case "$*" in
  "api user --jq .login") echo tester ;;
  api\ notifications*)
    if [ -f "{d}/notified" ]; then echo '[]'; else
      touch "{d}/notified"
      echo '[{{"id":"101","reason":"assign","updated_at":"2026-11-08T10:00:00Z","repository":{{"full_name":"acme/demo"}},"subject":{{"type":"Issue","title":"Login breaks","url":"https://api.github.com/repos/acme/demo/issues/7"}}}}]'
    fi ;;
  "issue view 7 --repo acme/demo"*)
    if [ -f "{d}/comment.flag" ]; then
      echo '{{"title":"Login breaks","state":"OPEN","url":"https://github.com/acme/demo/issues/7","comments":[{{"id":"IC_9","author":{{"login":"lin"}},"body":"Please also handle empty passwords","viewerDidAuthor":false}}]}}'
    else
      echo '{{"title":"Login breaks","state":"OPEN","url":"https://github.com/acme/demo/issues/7","comments":[]}}'
    fi ;;
  "api repos/acme/demo/issues/7/comments -X POST"*)
    echo "$*" >> "{d}/posted.log"
    echo "https://github.com/acme/demo/issues/7#issuecomment-1" ;;
  *) echo "unexpected: $*" >&2; exit 1 ;;
esac
"#
        ),
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
fn an_assigned_issue_becomes_a_task_that_answers_comments() {
    let root = std::env::temp_dir().join(format!("bubble-github-test-{}", std::process::id()));
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
        format!(
            r#"{{"providers":[{{"id":"mock","name":"Mock","baseURL":"{url}","apiKey":"k","enabled":true}}],"defaultModel":"mock:test-model"}}"#
        ),
    )
    .unwrap();
    let gh = fake_gh(&root);
    // SAFETY: the only test in this binary, before the engine starts.
    unsafe {
        std::env::set_var("BUBBLE_HOME", &home);
        std::env::set_var("BUBBLE_GH", &gh);
        std::env::set_var("BUBBLE_GH_POLL_MS", "300");
    }

    let mut app = Headless::start(EngineConfig::new(Root::of::<bubble_app::App>())).unwrap();
    app.set_viewport(nsh_core::element::size(nsh_core::element::px(1280.0), nsh_core::element::px(820.0)))
        .unwrap();
    let long = Duration::from_secs(10);

    // A first task makes the project, and so the repository, known.
    app.press("id:nav-tasks").unwrap();
    app.press("text:New task").unwrap();
    app.type_text("text:What needs doing?", "Set up").unwrap();
    app.on_pick_paths(vec![project.clone()]).unwrap();
    app.press("Choose a project folder").unwrap();
    app.wait_for("demo").unwrap();
    app.press("Create").unwrap();

    // Settings: the repository is found and set to prepare.
    app.press("id:nav-settings").unwrap();
    app.wait_for_within("Signed in with GitHub CLI as tester", long).unwrap();
    app.wait_for("acme/demo").unwrap();
    app.press("label:Prepare for acme/demo").unwrap();
    app.wait_for("text:/^Agents work on new issues and reviews/").unwrap();

    // The assignment becomes a task waiting to start, linked to the issue.
    app.press("id:nav-inbox").unwrap();
    app.wait_for_within("#7 Login breaks", long).unwrap();
    app.press("id:side-#7 Login breaks").unwrap();
    app.wait_for("Issue #7").unwrap();
    app.press("Start").unwrap();
    app.wait_for("From GitHub").unwrap();
    app.wait_for_within("Looking into it.", long).unwrap();

    // A new comment on the issue reaches the agent; its answer on GitHub
    // asks first, and "allow for this task" is not offered.
    std::fs::write(root.join("comment.flag"), "").unwrap();
    app.wait_for_within("text:/@lin commented on issue #7 in acme\\/demo/", long).unwrap();
    app.wait_for_within("Comment on #7 in acme/demo", long).unwrap();
    assert!(!app.shows("Allow for this task"));
    assert!(!root.join("posted.log").exists(), "nothing is posted before the yes");
    app.press("Allow once").unwrap();
    app.wait_for_within("Commented.", long).unwrap();
    let posted = std::fs::read_to_string(root.join("posted.log")).unwrap();
    assert!(posted.contains("body=Handled empty passwords too."), "{posted}");

    std::fs::remove_dir_all(&root).ok();
}
