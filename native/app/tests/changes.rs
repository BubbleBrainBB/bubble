//! The Changes panel, headless: the project's changes since the last
//! commit, and an implementer's kept worktree applied to the project.

use std::time::Duration;

use bubble_store::{NewTask, Origin, Store};
use nsh_engine::{EngineConfig, Root};
use nsh_testing::Headless;

fn git(dir: &std::path::Path, args: &[&str]) {
    let ok = std::process::Command::new("git").args(args).current_dir(dir).output().unwrap().status.success();
    assert!(ok, "git {args:?}");
}

#[test]
fn changes_show_and_a_worktree_is_applied() {
    let root = std::env::temp_dir().join(format!("bubble-changes-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let project = root.join("demo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&project).unwrap();
    git(&project, &["init", "-q"]);
    std::fs::write(project.join("a.txt"), "one\n").unwrap();
    git(&project, &["add", "."]);
    git(&project, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "init"]);
    // The agent's own edit, not committed.
    std::fs::write(project.join("notes.md"), "hello\n").unwrap();
    std::fs::write(
        home.join("config.json"),
        r#"{"providers":[{"id":"mock","name":"Mock","baseURL":"http://127.0.0.1:9","apiKey":"k","enabled":true}],"defaultModel":"mock:test-model"}"#,
    )
    .unwrap();

    // A task whose implementer finished with a change kept in its worktree.
    let store = Store::open(&home.join("native").join("bubble.db")).unwrap();
    let project_text = project.display().to_string();
    let task = store.create_task(NewTask { title: "Review task", project_cwd: Some(&project_text), origin: Origin::User }).unwrap();
    drop(store);
    let worktree = root.join("wt");
    git(&project, &["worktree", "add", "--detach", &worktree.display().to_string(), "HEAD"]);
    std::fs::write(worktree.join("a.txt"), "two\n").unwrap();
    let saved = serde_json::json!({
        "snapshot": {
            "id": "agent_1", "nickname": "Ivy", "profile": "implementer", "task": "Change a.txt",
            "description": "Change a.txt", "status": "completed", "model": "mock:test-model",
            "summary": "Changed a.txt.", "notes": [], "error": null, "workflow": false, "pending_inputs": 0,
            "worktree": {
                "path": worktree, "root": project, "cwd": worktree, "changed": true,
                "diff_stat": " a.txt | 2 +-", "removed": false
            }
        },
        "model": "mock:test-model", "thinking": "medium", "write": true,
        "system": "", "tools": [], "history": []
    });
    let dir = home.join("native").join("subagents").join(&task.id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("agent_1.json"), saved.to_string()).unwrap();

    // SAFETY: the only test in this binary, before the engine starts.
    unsafe {
        std::env::set_var("BUBBLE_HOME", &home);
        std::env::set_var("BUBBLE_GH", root.join("no-gh"));
        std::env::set_var("BUBBLE_NO_SHELL_ENV", "1");
        std::env::set_var("BUBBLE_MEMORY_AUTO", "0");
    }
    let mut app = Headless::start(EngineConfig::new(Root::of::<bubble_app::App>())).unwrap();
    app.set_viewport(nsh_core::element::size(nsh_core::element::px(1400.0), nsh_core::element::px(820.0))).unwrap();
    let long = Duration::from_secs(10);

    app.wait_for_within("id:nav-tasks", long).unwrap();
    app.press("id:nav-tasks").unwrap();
    app.press("id:side-Review task").unwrap();
    app.press("Changes").unwrap();
    if let Err(e) = app.wait_for_within("label:Ivy's worktree", long) {
        panic!("{e}\nTEXTS: {:?}", app.screen().texts());
    }
    app.wait_for_gone("Loading…", long).unwrap();
    assert!(!app.shows("No changes since the last commit."), "the new file is a change: {:?}", app.screen().texts());

    app.press("label:Ivy's worktree").unwrap();
    app.press("Apply").unwrap();
    app.wait_for_gone("label:Ivy's worktree", long).unwrap();
    assert_eq!(std::fs::read_to_string(project.join("a.txt")).unwrap(), "two\n", "applied to the project");
    assert!(!worktree.exists(), "the worktree is gone");

    std::fs::remove_dir_all(&root).ok();
}
