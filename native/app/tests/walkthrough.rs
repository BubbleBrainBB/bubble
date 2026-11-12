//! The whole flow with a real model: not run with the others, since it
//! spends the person's model allowance and takes minutes.
//!
//!     cargo test -p bubble-app --test walkthrough -- --ignored --nocapture
//!
//! It copies the person's `~/.bubble/config.json` (and sign-ins) into a
//! scratch home, makes two scratch repositories and a stand-in `gh`, and
//! lets the real model do the rest: sort two issues that come in, make the
//! bot the work needs, do the work once the person says yes, then take a
//! request the person gives the main bot for the other project. It prints
//! what happened; reading that is the check.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use bubble_store::{MessageRole, Store, TicketStatus};
use nsh_engine::{EngineConfig, Root};
use nsh_testing::Headless;
use serde_json::json;

const REPO: &str = "bubble-walkthrough/demo";
const OTHER: &str = "bubble-walkthrough/tools";

/// A stand-in `gh`, also first on the bots' PATH, so nothing they run
/// reaches GitHub.
fn fake_gh(dir: &Path) -> PathBuf {
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let script = bin.join("gh");
    std::fs::write(
        &script,
        format!(
            r#"#!/bin/sh
case "$*" in
  "api user --jq .login") echo tester ;;
  api\ notifications*) echo '[]' ;;
  api\ repos/{REPO}/issues\?state=open*)
    echo '[{{"number":12,"title":"hello.sh: add a --version flag that prints 0.1.0","created_at":"2099-01-01T00:00:00Z","user":{{"login":"alice","type":"User"}}}},{{"number":13,"title":"Thanks for this, works great!","created_at":"2099-01-01T00:01:00Z","user":{{"login":"bob","type":"User"}}}}]' ;;
  api\ repos/{OTHER}/issues\?state=open*) echo '[]' ;;
  issue\ view\ 12*)
    case "$*" in
      *--json*) echo '{{"title":"hello.sh: add a --version flag that prints 0.1.0","state":"OPEN","url":"https://github.com/{REPO}/issues/12","comments":[]}}' ;;
      *) printf 'title:\thello.sh: add a --version flag that prints 0.1.0\nstate:\tOPEN\nauthor:\talice\n--\nRunning `sh hello.sh --version` should print 0.1.0 and exit. Without the flag it should greet as it does now.\n' ;;
    esac ;;
  *) echo "gh (stand-in): nothing for: $*" >&2; exit 1 ;;
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

fn repository(folder: &Path, remote: &str, files: &[(&str, &str)]) {
    std::fs::create_dir_all(folder).unwrap();
    for (name, content) in files {
        std::fs::write(folder.join(name), content).unwrap();
    }
    for args in [
        vec!["init", "-q"],
        vec!["remote", "add", "origin", remote],
        vec!["add", "."],
        vec!["-c", "user.name=t", "-c", "user.email=t@example.com", "commit", "-q", "-m", "first"],
    ] {
        assert!(std::process::Command::new("git").args(&args).current_dir(folder).status().unwrap().success(), "git {args:?}");
    }
}

fn shell(folder: &Path, command: &str) -> String {
    let out = std::process::Command::new("sh").arg("-c").arg(command).current_dir(folder).output().unwrap();
    format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)).trim().to_owned()
}

/// Says yes to whatever a bot asks to run, until `done`; false on timeout.
fn allow_until(app: &mut Headless, limit: Duration, mut done: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < limit {
        if done() {
            return true;
        }
        if app.shows("Allow once") {
            let _ = app.press("Allow once");
        }
        let _ = app.wait_for_within("text:/^never shown$/", Duration::from_millis(700));
    }
    false
}

fn bots(store: &Store) -> Vec<(String, String, String, Option<String>)> {
    store
        .list_tasks()
        .unwrap_or_default()
        .into_iter()
        .filter(|t| t.task.origin != bubble_store::Origin::Cli)
        .map(|t| {
            let role = store.setting(&format!("bot_role:{}", t.task.id)).ok().flatten().unwrap_or_default();
            (t.task.id, t.task.title, role, t.task.project_cwd)
        })
        .collect()
}

#[test]
#[ignore = "spends the person's model allowance; run it by name"]
fn the_whole_flow_with_a_real_model() {
    let real = PathBuf::from(std::env::var("HOME").unwrap()).join(".bubble");
    let root = std::env::temp_dir().join(format!("bubble-walkthrough-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    std::fs::create_dir_all(home.join("native")).unwrap();
    for file in ["config.json", "auth.json"] {
        if real.join(file).is_file() {
            std::fs::copy(real.join(file), home.join(file)).unwrap();
        }
    }
    let (demo, tools) = (root.join("demo"), root.join("tools"));
    repository(&demo, &format!("https://github.com/{REPO}.git"), &[("hello.sh", "#!/bin/sh\necho \"hello\"\n"), ("README.md", "# demo\n\nRun `sh hello.sh`.\n")]);
    repository(&tools, &format!("https://github.com/{OTHER}.git"), &[("README.md", "# tools\n\nSmall scripts.\n")]);
    let gh = fake_gh(&root);
    {
        let store = Store::open(home.join("native/bubble.db")).unwrap();
        store.set_setting("projects", Some(&json!([demo.to_string_lossy(), tools.to_string_lossy()]).to_string())).unwrap();
        store.set_setting(&format!("github:{REPO}"), Some("prepare")).unwrap();
        store.set_setting(&format!("github:{OTHER}"), Some("prepare")).unwrap();
    }
    // SAFETY: the only test in this binary, before the engine starts.
    unsafe {
        std::env::set_var("BUBBLE_HOME", &home);
        std::env::set_var("BUBBLE_GH", &gh);
        std::env::set_var("BUBBLE_GH_POLL_MS", "1000");
        std::env::set_var("BUBBLE_TRIAGE_QUIET_MS", "0");
        std::env::set_var("BUBBLE_NO_SHELL_ENV", "1");
        std::env::set_var("PATH", format!("{}:{}", root.join("bin").display(), std::env::var("PATH").unwrap_or_default()));
    }
    let mut app = Headless::start(EngineConfig::new(Root::of::<bubble_app::App>())).unwrap();
    app.set_viewport(nsh_core::element::size(nsh_core::element::px(1280.0), nsh_core::element::px(1000.0))).unwrap();
    let store = Store::open(home.join("native/bubble.db")).unwrap();
    let started = Instant::now();
    let stamp = |what: &str| println!("\n=== {what} (t+{}s)", started.elapsed().as_secs());

    // 1. Two issues come in; the main bot sorts them, with no bot to give them to yet.
    app.wait_for_within("id:nav-home", Duration::from_secs(30)).unwrap();
    let sorted = allow_until(&mut app, Duration::from_secs(240), || {
        store.untriaged_events().is_ok_and(|e| e.is_empty()) && store.events_since(0).is_ok_and(|e| e.len() >= 2)
    });
    stamp("1. what came in, and what the main bot made of it");
    println!("sorted in time: {sorted}");
    for event in store.events_since(0).unwrap_or_default() {
        let payload: serde_json::Value = serde_json::from_str(&event.payload).unwrap_or_default();
        println!(
            "  #{} \"{}\" -> {}{}",
            event.number.unwrap_or_default(),
            payload["title"].as_str().unwrap_or_default(),
            event.disposition,
            payload["ignored_reason"].as_str().map(|r| format!(" ({r})")).unwrap_or_default()
        );
    }
    println!("bots: {:?}", bots(&store).iter().map(|(_, name, role, _)| format!("{name} [{role}]")).collect::<Vec<_>>());
    let proposed = store.proposed_tickets().unwrap_or_default();
    for ticket in &proposed {
        println!("proposed: \"{}\" priority {} in {:?}\n  brief: {}", ticket.title, ticket.priority, ticket.project, ticket.text);
    }
    let Some(ticket) = proposed.first().cloned() else {
        println!("NOTHING WAS PROPOSED; screen: {:?}", app.screen().texts());
        return;
    };

    // 2. The person says yes in the digest; the bot does the work in the repository's folder.
    app.press("id:nav-home").unwrap();
    app.wait_for_within("text:/wants? your yes/", Duration::from_secs(10)).unwrap();
    app.press(&format!("label:Give: {}", ticket.title)).unwrap();
    let worker = store.get_ticket(&ticket.id).unwrap().unwrap().task_id;
    let name = bots(&store).into_iter().find(|(id, ..)| *id == worker).map(|(_, name, ..)| name).unwrap_or_default();
    app.press(&format!("id:side-{name}")).unwrap();
    let finished = allow_until(&mut app, Duration::from_secs(420), || {
        store.get_ticket(&ticket.id).ok().flatten().is_some_and(|t| !matches!(t.status, TicketStatus::Accepted | TicketStatus::Proposed))
    });
    let after = store.get_ticket(&ticket.id).unwrap().unwrap();
    stamp("2. the work, once given");
    println!("finished in time: {finished}; ticket: {}", after.status.as_str());
    println!("the bot ({name}) reported:\n{}", after.report.unwrap_or_default());
    println!("bots now: {:?}", bots(&store).iter().map(|(_, name, role, at)| format!("{name} [{role}] in {at:?}")).collect::<Vec<_>>());
    println!("demo: git status:\n{}", shell(&demo, "git status --short; git log --oneline | head -3"));
    println!("demo: sh hello.sh --version -> {}", shell(&demo, "sh hello.sh --version"));
    println!("demo: sh hello.sh -> {}", shell(&demo, "sh hello.sh"));

    // 3. The person asks the main bot for something in the other project.
    app.press("id:nav-home").unwrap();
    let ask = "让开发的 bot 在 tools 这个项目的 README.md 末尾加一行 “Maintained by Bubble bots.”，做完告诉我。";
    app.type_text("id:composer", ask).unwrap();
    app.submit("id:composer").unwrap();
    let main_run = store.manager_run(None).unwrap();
    // The bot may ask before it writes: its questions show in its own conversation.
    let handed = allow_until(&mut app, Duration::from_secs(120), || {
        store.tickets_for_task(&worker, 8).is_ok_and(|t| t.len() >= 2) || bots(&store).len() > 1
    });
    let target = bots(&store).into_iter().find(|(id, ..)| store.tickets_for_task(id, 8).is_ok_and(|t| t.iter().any(|t| t.id != ticket.id))).map(|(_, name, ..)| name);
    if let Some(target) = &target {
        app.press(&format!("id:side-{target}")).unwrap();
    }
    let relayed = allow_until(&mut app, Duration::from_secs(420), || {
        store.messages(&main_run.id).is_ok_and(|messages| {
            let report = messages.iter().rposition(|m| m.role == MessageRole::System);
            report.is_some_and(|at| messages[at..].iter().any(|m| m.role == MessageRole::Assistant && !m.text.trim().is_empty()))
        })
    });
    stamp("3. a request to the main bot, for the other project");
    println!("handed on in time: {handed}; relayed in time: {relayed}; went to: {target:?}");
    for message in store.messages(&main_run.id).unwrap_or_default() {
        let who = match message.role {
            MessageRole::User => "person",
            MessageRole::Assistant => "main bot",
            MessageRole::System => "report",
            _ => continue,
        };
        if !message.text.trim().is_empty() {
            println!("  [{who}] {}", message.text.trim().replace('\n', "\n      "));
        }
    }
    println!("bots at the end: {:?}", bots(&store).iter().map(|(_, name, role, at)| format!("{name} [{role}] in {at:?}")).collect::<Vec<_>>());
    println!("tools: README.md ends with: {:?}", shell(&tools, "tail -n 2 README.md"));
    println!("tools: git status:\n{}", shell(&tools, "git status --short"));
    for (id, name, ..) in bots(&store) {
        for ticket in store.tickets_for_task(&id, 8).unwrap_or_default() {
            println!("ticket of {name}: \"{}\" [{}] in {:?}", ticket.title, ticket.status.as_str(), ticket.project);
        }
    }
    let notes = shell(&home, "find native/memory -type f 2>/dev/null | while read f; do echo \"$f:\"; cat \"$f\"; done");
    println!("notes kept:\n{}", if notes.is_empty() { "(none)".to_owned() } else { notes });

    drop(app);
    std::fs::remove_dir_all(&root).ok();
}
