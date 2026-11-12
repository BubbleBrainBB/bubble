//! The sidebar's pages, headless: Pull requests (a task for one, a merge
//! the person confirms), Automations (made, run now), Skills, Search, and
//! a long Done list folded.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::time::Duration;

use bubble_store::{Lifecycle, NewTask, Origin, Store};
use nsh_engine::{EngineConfig, Root};
use nsh_testing::Headless;

fn events(body: &str) -> String {
    let request: serde_json::Value = serde_json::from_str(body).unwrap_or_default();
    let messages = request["messages"].as_array().cloned().unwrap_or_default();
    let system = messages.first().and_then(|m| m["content"].as_str()).unwrap_or_default();
    let last = messages.last().cloned().unwrap_or_default();
    let said = last["content"].as_str().unwrap_or_default();
    // The main bot hands a pull request to the bot that fits, linked to it.
    if system.contains("the person's main bot") {
        if last["role"] == "tool" {
            return say("Gave pull request 7 to Set up.");
        }
        if let Some(rest) = said.split("in the project folder ").nth(1).filter(|_| said.contains("pull request #7")) {
            let project = rest.split(": link it").next().unwrap_or_default();
            let arguments = serde_json::json!({
                "bot": "Set up",
                "message": "Look after pull request #7 in acme/demo: read it and say what it needs.",
                "project": project,
                "pull_request": 7
            });
            return call("message_bot", &arguments.to_string());
        }
    }
    say("On it.")
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

/// A fake `gh`: one open pull request, #7, which can be merged.
fn fake_gh(dir: &Path) -> std::path::PathBuf {
    let script = dir.join("gh");
    let d = dir.display();
    std::fs::write(
        &script,
        format!(
            r#"#!/bin/sh
case "$*" in
  "api user --jq .login") echo tester ;;
  api\ notifications*) echo '[]' ;;
  "pr list --repo acme/demo"*) echo '[{{"number":7,"title":"Add docs","url":"https://github.com/acme/demo/pull/7","isDraft":false,"state":"OPEN"}}]' ;;
  "pr merge 7 --repo acme/demo --squash") touch "{d}/merged" ;;
  "pr view 7 --repo acme/demo"*) echo '{{"title":"Add docs","state":"OPEN","url":"https://github.com/acme/demo/pull/7","headRefOid":"abc","comments":[],"reviews":[],"statusCheckRollup":[]}}' ;;
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
fn the_sidebar_pages_work() {
    let root = std::env::temp_dir().join(format!("bubble-pages-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let project = root.join("demo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&project).unwrap();
    for args in [vec!["init", "-q"], vec!["remote", "add", "origin", "https://github.com/acme/demo.git"]] {
        assert!(std::process::Command::new("git").args(&args).current_dir(&project).status().unwrap().success());
    }
    let skill = home.join("skills").join("release-notes");
    std::fs::create_dir_all(&skill).unwrap();
    std::fs::write(skill.join("SKILL.md"), "---\nname: release-notes\ndescription: Writes release notes from merged pull requests.\n---\nSteps.").unwrap();
    let url = model_server();
    std::fs::write(
        home.join("config.json"),
        format!(r#"{{"providers":[{{"id":"mock","name":"Mock","baseURL":"{url}","apiKey":"k","enabled":true}}],"defaultModel":"mock:test-model"}}"#),
    )
    .unwrap();
    // 25 finished tasks, for the folded Done list.
    let store = Store::open(home.join("native").join("bubble.db")).unwrap();
    for n in 1..=25 {
        let task = store.create_task(NewTask { title: &format!("Old {n}"), project_cwd: None, origin: Origin::User }).unwrap();
        store.set_task_lifecycle(&task.id, Lifecycle::Done, None).unwrap();
    }
    drop(store);
    let gh = fake_gh(&root);
    // SAFETY: the only test in this binary, before the engine starts.
    unsafe {
        std::env::set_var("BUBBLE_HOME", &home);
        std::env::set_var("BUBBLE_GH", &gh);
        std::env::set_var("BUBBLE_GH_POLL_MS", "300");
        std::env::set_var("BUBBLE_NO_SHELL_ENV", "1");
        std::env::set_var("BUBBLE_MEMORY_AUTO", "0");
    }

    // New work starts with the Manager; this test starts from a task already made.
    bubble_store::Store::open(home.join("native/bubble.db"))
        .unwrap()
        .create_task(bubble_store::NewTask { title: "Set up", project_cwd: Some(project.to_str().unwrap()), origin: bubble_store::Origin::User })
        .unwrap();
    let mut app = Headless::start(EngineConfig::new(Root::of::<bubble_app::App>())).unwrap();
    app.set_viewport(nsh_core::element::size(nsh_core::element::px(1280.0), nsh_core::element::px(1600.0))).unwrap();
    let long = Duration::from_secs(10);

    // What was closed is not among the bots.
    app.wait_for_within("id:nav-home", long).unwrap();
    app.wait_for("id:side-Set up").unwrap();
    assert_eq!(app.screen().count("text:/^Old \\d+$/").unwrap(), 0);

    // A project that is a watched repository.
    app.press("id:side-Set up").unwrap();
    app.press("id:nav-settings").unwrap();
    app.wait_for_within("acme/demo", long).unwrap();
    app.press("label:Prepare for acme/demo").unwrap();

    // Pull requests: a task for #7, then a merge the person confirms.
    app.press("id:nav-settings").unwrap();
    app.press("id:nav-pull-requests").unwrap();
    if let Err(e) = app.wait_for_within("text:#7 Add docs", long) {
        panic!("{e}\nTEXTS: {:?}", app.screen().texts());
    }
    // The main bot is the one who hands work out: it gives the pull
    // request to the bot that fits, linked so reviews and checks reach it.
    app.press("Give to a bot").unwrap();
    app.wait_for_within("Gave pull request 7 to Set up.", long).unwrap();
    app.press("id:side-Set up").unwrap();
    app.wait_for_within("PR #7", long).unwrap();
    app.wait_for("text:Message Set up").unwrap();
    app.wait_for_within("On it.", long).unwrap();
    app.press("id:nav-settings").unwrap();
    app.press("id:nav-pull-requests").unwrap();
    app.wait_for_within("Open bot", long).unwrap();
    app.press("Merge").unwrap();
    app.press("Squash and merge").unwrap();
    app.wait_for_within("text:/Merged/", long).unwrap();
    assert!(root.join("merged").exists(), "gh pr merge ran");

    // Routines live with the bot that runs them: made in its details, then
    // run now. The prompt goes to that bot's conversation as its routine's;
    // no bot is made for a run.
    app.press("id:side-Set up").unwrap();
    app.press("id:bot-details-toggle").unwrap();
    app.wait_for("No routines yet.").unwrap();
    app.press("Add a routine").unwrap();
    app.type_text("text:Name", "Nightly check").unwrap();
    app.type_text("text:What should the bot do each time?", "Check the nightly build").unwrap();
    app.press("Every few minutes").unwrap();
    app.press("Save routine").unwrap();
    app.wait_for("text:/^Every hour, next /").unwrap();
    app.press("Run now").unwrap();
    app.wait_for("From your routine Nightly check").unwrap();
    app.wait_for("text:Check the nightly build").unwrap();
    app.wait_for_within("On it.", long).unwrap();
    assert!(!app.shows("id:side-Nightly check"), "a routine's run is not a bot");
    // The run is on record with the work the bot was given.
    app.wait_for_within("text:From its routine; finished", long).unwrap();
    app.press("id:bot-details-toggle").unwrap();

    // Skills: listed, and found by what they do.
    app.press("id:nav-settings").unwrap();
    app.press("id:nav-skills").unwrap();
    app.wait_for("release-notes").unwrap();
    app.type_text("id:skills-search", "merged pull").unwrap();
    app.wait_for("release-notes").unwrap();
    app.clear("id:skills-search").unwrap();
    app.type_text("id:skills-search", "nothing like it").unwrap();
    app.wait_for_gone("release-notes", long).unwrap();

    // Search: a task found by what was said in it.
    app.press("id:nav-search").unwrap();
    app.wait_for("id:search").unwrap();
    app.clear("id:search").unwrap();
    app.type_text("id:search", "nightly build").unwrap();
    app.wait_for("label:Found in Set up").unwrap();
    app.press("label:Found in Set up").unwrap();
    app.wait_for("text:Message Set up").unwrap();

    // A bot is a role: made with a name no other bot has, its role, and
    // what it is for. No folder: that comes with its work.
    app.press("id:nav-new-bot").unwrap();
    app.type_text("id:bot-name", "Set up").unwrap();
    app.press("Create bot").unwrap();
    app.wait_for("A bot called Set up exists. Choose another name.").unwrap();
    app.clear("id:bot-name").unwrap();
    app.type_text("id:bot-name", "Docs").unwrap();
    app.type_text("id:bot-role", "Writer").unwrap();
    app.type_text("id:bot-instruction", "Keep the docs in step with the code").unwrap();
    assert!(!app.shows("Choose a folder"), "a bot is not made in a folder");
    app.press("Create bot").unwrap();
    app.wait_for("id:side-Docs").unwrap();
    // Its conversation is open, named after it, and it says hello itself.
    app.wait_for("text:Message Docs").unwrap();
    app.wait_for("Made by you.").unwrap();
    app.wait_for_within("On it.", long).unwrap();
    // Its role shows by its name, and it works in no folder until work names one.
    app.wait_for("Writer").unwrap();
    app.wait_for("No folder").unwrap();

    std::fs::remove_dir_all(&root).ok();
}
