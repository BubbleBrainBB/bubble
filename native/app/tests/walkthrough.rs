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
//!
//! `the_harder_cases_with_a_real_model` is a second run, by the same rule:
//! six things of different kinds come in at once, and the person then asks
//! the main bot for what it should answer itself, what it should remember,
//! what cannot be done, and two things at once. Run one of the two by name:
//! they share the process's environment.
//!
//!     cargo test -p bubble-app --test walkthrough the_harder -- --ignored --nocapture

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use bubble_store::{MessageRole, RunStatus, Store, TicketStatus};
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

/// How many times a yes was given to a bot that asked to run something.
static YESES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn yes(app: &mut Headless) {
    if app.press("Allow once").is_ok() {
        YESES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Says yes to whatever a bot asks to run, until `done`; false on timeout.
fn allow_until(app: &mut Headless, limit: Duration, mut done: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < limit {
        if done() {
            return true;
        }
        if app.shows("Allow once") {
            yes(app);
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

    // 2. The issues are a stranger's, so the main bot asks in its conversation;
    // the person says yes there, and the bot does the work in the repository's folder.
    app.press("id:nav-home").unwrap();
    app.wait_for_within(&format!("label:Give: {}", ticket.title), Duration::from_secs(10)).unwrap();
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

/// A stand-in `gh` with six things newly opened in the demo repository, of
/// different kinds and from different people. It writes down every call,
/// and apart from those whatever a bot tries to post.
fn fake_gh_many(dir: &Path) -> PathBuf {
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let script = bin.join("gh");
    let text = r#"#!/bin/sh
echo "$*" >> "@CALLS@"
show() {
  case "$1" in
    21) T='hello.sh accepts unknown flags without complaint'; A=alice
        B='Running sh hello.sh --foo prints hello as if nothing were wrong. For any flag it does not know it should print: usage: hello.sh (to stderr) and exit with status 2.' ;;
    22) T='How do I run this on Windows?'; A=bob
        B='I only have Windows. Is there a way to run hello.sh there?' ;;
    23) T='README: say what hello.sh prints'; A=carol
        B='Add one sentence under the title of README.md saying that the script prints hello.' ;;
    24) T='Add an MIT LICENSE file'; A=tester
        B='Add a LICENSE file with the MIT license. Copyright holder: Demo Authors. Year: 2026.' ;;
    25) T='Typos in flags are silently ignored'; A=dave
        B='sh hello.sh -x just prints hello. I think this is the same as #21.' ;;
    26) T='Fix typo in README'; A=erin
        B='Changes Run to Run it with in the README. No other change.' ;;
    *) echo "gh (stand-in): no such item: $1" >&2; exit 1 ;;
  esac
  case "$2" in
    json) printf '{"number":%s,"title":"%s","body":"%s","state":"OPEN","author":{"login":"%s"},"url":"https://github.com/@REPO@/issues/%s","comments":[],"labels":[],"assignees":[]}\n' "$1" "$T" "$B" "$A" "$1" ;;
    *) printf 'title:\t%s\nstate:\tOPEN\nauthor:\t%s\nnumber:\t%s\n--\n%s\n' "$T" "$A" "$1" "$B" ;;
  esac
}
number() { for a in "$@"; do case "$a" in [0-9]*) echo "$a"; return ;; esac; done; }
form() { case "$*" in *--json*) echo json ;; *) echo text ;; esac; }
case "$*" in
  "api user --jq .login") echo tester ;;
  api\ notifications*) echo '[]' ;;
  api\ repos/@REPO@/issues\?state=open*) cat <<'EOF'
[{"number":21,"title":"hello.sh accepts unknown flags without complaint","created_at":"2099-01-01T00:00:00Z","author_association":"NONE","user":{"login":"alice","type":"User"}},
 {"number":22,"title":"How do I run this on Windows?","created_at":"2099-01-01T00:01:00Z","author_association":"NONE","user":{"login":"bob","type":"User"}},
 {"number":23,"title":"README: say what hello.sh prints","created_at":"2099-01-01T00:02:00Z","author_association":"MEMBER","user":{"login":"carol","type":"User"}},
 {"number":24,"title":"Add an MIT LICENSE file","created_at":"2099-01-01T00:03:00Z","author_association":"OWNER","user":{"login":"tester","type":"User"}},
 {"number":25,"title":"Typos in flags are silently ignored","created_at":"2099-01-01T00:04:00Z","author_association":"NONE","user":{"login":"dave","type":"User"}},
 {"number":26,"title":"Fix typo in README","created_at":"2099-01-01T00:05:00Z","author_association":"NONE","user":{"login":"erin","type":"User"},"pull_request":{}}]
EOF
  ;;
  api\ repos/@OTHER@/issues\?state=open*) echo '[]' ;;
  pr\ list*) echo '[]' ;;
  issue\ view*|pr\ view*) show "$(number "$@")" "$(form "$@")" ;;
  pr\ diff*) cat <<'EOF'
diff --git a/README.md b/README.md
--- a/README.md
+++ b/README.md
@@ -1,3 +1,3 @@
 # demo
 
-Run `sh hello.sh`.
+Run it with `sh hello.sh`.
EOF
  ;;
  pr\ checks*) echo 'no checks reported' ;;
  issue\ comment*|pr\ comment*|pr\ review*|pr\ create*|pr\ merge*|pr\ close*|issue\ close*|issue\ create*|issue\ edit*|*-X\ POST*|*--method\ POST*)
    echo "$*" >> "@WRITES@"; echo "https://github.com/@REPO@/issues/0#stand-in" ;;
  *) echo "gh (stand-in): nothing for: $*" >&2; exit 1 ;;
esac
"#
    .replace("@CALLS@", &dir.join("gh-calls.log").to_string_lossy())
    .replace("@WRITES@", &dir.join("gh-writes.log").to_string_lossy())
    .replace("@REPO@", REPO)
    .replace("@OTHER@", OTHER);
    std::fs::write(&script, text).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    script
}

/// Every conversation's run: the main bot's and each bot's.
fn runs(store: &Store) -> Vec<bubble_store::Run> {
    let mut all: Vec<_> = store.manager_run(None).into_iter().collect();
    for (id, ..) in bots(store) {
        all.extend(store.runs_for_task(&id).unwrap_or_default());
    }
    all
}

fn tickets(store: &Store) -> Vec<(String, bubble_store::Ticket)> {
    bots(store).into_iter().flat_map(|(id, name, ..)| store.tickets_for_task(&id, 32).unwrap_or_default().into_iter().map(move |t| (name.clone(), t))).collect()
}

/// Waits until nothing is going on: nothing left to sort, no work begun
/// and unfinished, no conversation in a turn. Says yes to whatever a bot
/// asks to run, in whichever conversation it asks. False on timeout.
fn settle(app: &mut Headless, store: &Store, limit: Duration) -> bool {
    let start = Instant::now();
    let mut quiet: Option<Instant> = None;
    while start.elapsed() < limit {
        for (id, name, ..) in bots(store) {
            if store.runs_for_task(&id).unwrap_or_default().iter().any(|r| r.status == RunStatus::NeedsInput) {
                let _ = app.press(&format!("id:side-{name}"));
                let _ = app.wait_for_within("text:/^never shown$/", Duration::from_millis(400));
                if app.shows("Allow once") {
                    yes(app);
                }
            }
        }
        let busy = runs(store).iter().any(|r| matches!(r.status, RunStatus::Queued | RunStatus::Running | RunStatus::NeedsInput))
            || !store.untriaged_events().is_ok_and(|e| e.is_empty())
            || tickets(store).iter().any(|(_, t)| t.status == TicketStatus::Accepted);
        if busy {
            quiet = None;
        } else if quiet.get_or_insert_with(Instant::now).elapsed() > Duration::from_secs(6) {
            return true;
        }
        let _ = app.wait_for_within("text:/^never shown$/", Duration::from_millis(700));
    }
    false
}

fn cut(text: &str, limit: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= limit { text.to_owned() } else { format!("{}… [{} more characters]", text.chars().take(limit).collect::<String>(), text.chars().count() - limit) }
}

/// What was said in a conversation from `from` on, as the person would read it.
fn said(store: &Store, run_id: &str, from: usize, who: &str) -> usize {
    let messages = store.messages(run_id).unwrap_or_default();
    for message in messages.iter().skip(from) {
        let role = match message.role {
            MessageRole::User => "person",
            MessageRole::Assistant => who,
            MessageRole::System => "report/request",
            _ => continue,
        };
        if !message.text.trim().is_empty() {
            println!("  [{role}] {}", cut(&message.text, 900).replace('\n', "\n      "));
        }
    }
    messages.len()
}

fn notes(home: &Path) -> String {
    let found = shell(home, "find native/memory -type f 2>/dev/null | sort | while read f; do echo \"$f:\"; sed 's/^/    /' \"$f\"; done");
    if found.is_empty() { "(none)".to_owned() } else { found }
}

#[test]
#[ignore = "spends the person's model allowance; run it by name"]
fn the_harder_cases_with_a_real_model() {
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
    repository(
        &demo,
        &format!("https://github.com/{REPO}.git"),
        &[
            ("hello.sh", "#!/bin/sh\necho \"hello\"\n"),
            ("test.sh", "#!/bin/sh\n[ \"$(sh hello.sh)\" = \"hello\" ] && echo ok || { echo FAILED; exit 1; }\n"),
            ("README.md", "# demo\n\nRun `sh hello.sh`.\n"),
        ],
    );
    repository(&tools, &format!("https://github.com/{OTHER}.git"), &[("README.md", "# tools\n\nSmall scripts.\n")]);
    let gh = fake_gh_many(&root);
    {
        let store = Store::open(home.join("native/bubble.db")).unwrap();
        store.set_setting("projects", Some(&json!([demo.to_string_lossy(), tools.to_string_lossy()]).to_string())).unwrap();
        store.set_setting(&format!("github:{REPO}"), Some("prepare")).unwrap();
        store.set_setting(&format!("github:{OTHER}"), Some("prepare")).unwrap();
    }
    // SAFETY: run by name, so the only test in this process, before the engine starts.
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
    let stamp = |what: &str| {
        println!("\n=== {what} (t+{}s, {} yeses to commands so far)", started.elapsed().as_secs(), YESES.load(std::sync::atomic::Ordering::Relaxed))
    };
    let team = |store: &Store| {
        bots(store)
            .iter()
            .map(|(id, name, role, at)| {
                let place = store.setting(&format!("bot_place:{id}")).ok().flatten();
                format!("{name} [{role}] in {:?}{}", at.as_deref().map(|p| p.rsplit('/').next().unwrap_or(p)), place.map(|p| format!(" (works at {p})")).unwrap_or_default())
            })
            .collect::<Vec<_>>()
    };
    // Tickets not printed yet, or printed in another state.
    let printed = std::cell::RefCell::new(std::collections::HashMap::<String, String>::new());
    let work = |store: &Store| {
        for (name, ticket) in tickets(store) {
            let state = ticket.status.as_str().to_owned();
            if printed.borrow_mut().insert(ticket.id.clone(), state.clone()).as_deref() == Some(state.as_str()) {
                continue;
            }
            println!(
                "ticket of {name}: \"{}\" [{}] priority {} from {:?}\n  asked: {}\n  reported: {}",
                cut(&ticket.title, 90),
                ticket.status.as_str(),
                ticket.priority,
                ticket.asker,
                cut(&ticket.text, 500).replace('\n', "\n         "),
                cut(ticket.report.as_deref().unwrap_or("(nothing yet)"), 700).replace('\n', "\n            ")
            );
        }
    };

    // 1. Six things come in at once.
    app.wait_for_within("id:nav-home", Duration::from_secs(30)).unwrap();
    let sorted = allow_until(&mut app, Duration::from_secs(300), || {
        store.untriaged_events().is_ok_and(|e| e.is_empty()) && store.events_since(0).is_ok_and(|e| e.len() >= 6)
    });
    stamp("1. six things came in; what the main bot made of each");
    println!("sorted in time: {sorted}");
    for event in store.events_since(0).unwrap_or_default() {
        let payload: serde_json::Value = serde_json::from_str(&event.payload).unwrap_or_default();
        let holder = event.task_id.as_ref().and_then(|id| bots(&store).into_iter().find(|(bot, ..)| bot == id)).map(|(_, name, ..)| name);
        println!(
            "  #{} by @{} (known: {}) \"{}\" -> {}{}{}",
            event.number.unwrap_or_default(),
            event.actor.clone().unwrap_or_default(),
            payload["known"],
            payload["title"].as_str().unwrap_or_default(),
            event.disposition,
            holder.map(|name| format!(" -> {name}")).unwrap_or_default(),
            payload["ignored_reason"].as_str().map(|r| format!(" ({r})")).unwrap_or_default()
        );
    }
    println!("bots: {:?}", team(&store));
    work(&store);
    // `BUBBLE_WALKTHROUGH_SORT_ONLY=1` stops here: the sorting alone, which takes half a minute.
    if std::env::var_os("BUBBLE_WALKTHROUGH_SORT_ONLY").is_some() {
        return;
    }

    // 2. What came from people who work on the repository begins at once.
    let done = settle(&mut app, &store, Duration::from_secs(600));
    stamp("2. the work that began without asking, and what waits for a yes");
    println!("settled in time: {done}");
    if !done {
        println!("screen: {:?}", app.screen().texts());
    }
    println!("bots: {:?}", team(&store));
    work(&store);
    println!("demo: git status:\n{}", shell(&demo, "git status --short; git worktree list; git branch -a"));
    println!("demo: README.md:\n{}", shell(&demo, "cat README.md"));
    println!("demo: LICENSE starts: {}", shell(&demo, "head -n 3 LICENSE 2>&1"));
    let main_run = store.manager_run(None).unwrap();
    println!("the main bot's conversation so far:");
    let mut seen_main = said(&store, &main_run.id, 0, "main bot");
    println!("what bots tried to post to GitHub:\n{}", shell(&root, "cat gh-writes.log 2>/dev/null || echo '(nothing)'"));

    // 3. What the person asks the main bot.
    let asks = [
        ("3a. a question it can answer itself", "一句话回答：git rebase 和 git merge 最主要的区别是什么？"),
        ("3b. something to remember", "记住两件事：以后给我的汇报都用中文，而且不超过三句话；另外不管哪个项目，改完代码都不要自动提交，等我说了再提交。"),
        ("3c. something that cannot be done as asked", "让开发 bot 在 tools 项目里运行 make test，把失败的测试修好。"),
        ("3d. two unrelated things at once", "两件互不相关的事，请同时进行：1) 在 demo 项目的 README.md 里加一节 Usage，写明 sh hello.sh 的用法；2) 在 tools 项目新建 scripts/clean.sh，内容是删除当前目录下的 *.tmp 文件。"),
    ];
    for (what, ask) in asks {
        app.press("id:nav-home").unwrap();
        app.type_text("id:composer", ask).unwrap();
        app.submit("id:composer").unwrap();
        let done = settle(&mut app, &store, Duration::from_secs(600));
        stamp(what);
        println!("settled in time: {done}");
        if !done {
            println!("screen: {:?}", app.screen().texts());
        }
        seen_main = said(&store, &main_run.id, seen_main, "main bot");
        work(&store);
        println!("bots: {:?}", team(&store));
    }
    println!("tools: git status:\n{}", shell(&tools, "git status --short; git log --oneline | head -3; cat scripts/clean.sh 2>&1"));
    println!("demo: git status:\n{}", shell(&demo, "git status --short; git log --oneline | head -5; git worktree list"));
    println!("notes kept so far:\n{}", notes(&home));

    // 4. The person talks to a bot directly: something worth keeping, then nothing worth keeping.
    let Some((dev, name, ..)) = bots(&store).into_iter().next() else {
        println!("NO BOT TO TALK TO");
        return;
    };
    let dev_run = store.runs_for_task(&dev).unwrap_or_default().into_iter().next();
    for (what, ask) in [
        ("4a. told to a bot: a fact about the project and a habit of the person", "demo 这个项目的测试以后都用 sh test.sh 跑；另外我写 shell 脚本习惯用两个空格缩进。现在在 demo 里跑一下测试，告诉我结果。"),
        ("4b. told to a bot: nothing that should last", "好的，谢谢，辛苦了。"),
    ] {
        app.press(&format!("id:side-{name}")).unwrap();
        let before = dev_run.as_ref().map(|run| store.messages(&run.id).unwrap_or_default().len()).unwrap_or_default();
        app.type_text("id:composer", ask).unwrap();
        app.submit("id:composer").unwrap();
        let done = settle(&mut app, &store, Duration::from_secs(300));
        // The look back at the exchange for notes runs after the turn.
        let _ = app.wait_for_within("text:/^never shown$/", Duration::from_secs(30));
        stamp(what);
        println!("settled in time: {done}");
        if let Some(run) = &dev_run {
            said(&store, &run.id, before, &name);
        }
        println!("notes kept now:\n{}", notes(&home));
    }

    // What each bot's conversation looks like to the person, from its start.
    stamp("5. each bot's conversation, as the person reads it");
    for (id, name, ..) in bots(&store) {
        println!("--- {name}");
        for run in store.runs_for_task(&id).unwrap_or_default() {
            said(&store, &run.id, 0, &name);
        }
    }
    said(&store, &main_run.id, seen_main, "main bot");
    println!("what bots tried to post to GitHub:\n{}", shell(&root, "cat gh-writes.log 2>/dev/null || echo '(nothing)'"));
    println!("gh calls bots and the app made that the stand-in had nothing for:\n{}", shell(&root, "sort gh-calls.log | uniq -c | sort -rn | grep -v 'api notifications\\|issues?state=open\\|api user' | head -30"));

    drop(app);
    std::fs::remove_dir_all(&root).ok();
}
