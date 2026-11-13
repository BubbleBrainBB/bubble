//! Settings, headless: a dialog over the window with its sections on the
//! left; one access setting for every bot; and the two lists of rules,
//! which are the agent's own, kept in settings.json with the rest of that
//! file left as it was.

use std::time::Duration;

use bubble_store::Store;
use nsh_engine::{EngineConfig, Root};
use nsh_testing::Headless;
use serde_json::Value;

#[test]
fn settings_is_a_dialog_with_one_access_for_every_bot_and_rules_kept_in_the_agents_file() {
    let root = std::env::temp_dir().join(format!("bubble-settings-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(
        home.join("config.json"),
        r#"{"providers":[{"id":"mock","name":"Mock","baseURL":"http://127.0.0.1:9","apiKey":"k","enabled":true}],"defaultModel":"mock:test-model"}"#,
    )
    .unwrap();
    // What the person already has in the agent's settings stays.
    std::fs::write(
        home.join("settings.json"),
        r#"{"mcpServers":{},"permissions":{"defaultMode":"default","allow":["Edit(docs/**)"]},"hooks":{}}"#,
    )
    .unwrap();
    // SAFETY: the only test in this binary, before the engine starts.
    unsafe {
        std::env::set_var("BUBBLE_HOME", &home);
        std::env::set_var("BUBBLE_GH", root.join("no-gh"));
        std::env::set_var("BUBBLE_NO_SHELL_ENV", "1");
        std::env::set_var("BUBBLE_MEMORY_AUTO", "0");
    }
    let long = Duration::from_secs(10);
    let settings = || -> Value { serde_json::from_str(&std::fs::read_to_string(home.join("settings.json")).unwrap()).unwrap() };
    {
        let mut app = Headless::start(EngineConfig::new(Root::of::<bubble_app::App>())).unwrap();
        app.set_viewport(nsh_core::element::size(nsh_core::element::px(1280.0), nsh_core::element::px(820.0))).unwrap();
        app.wait_for_within("What should we work on?", long).unwrap();

        // Over the conversation, which stays where it was; General first.
        app.press("id:nav-settings").unwrap();
        app.wait_for("Bots working at once").unwrap();
        app.wait_for("text:Data folder").unwrap();
        assert!(app.shows("What should we work on?"), "the conversation is still behind it");
        // A press beside it takes it away.
        app.press("id:settings-scrim").unwrap();
        assert!(!app.shows("Bots working at once"));

        // Models: what each kind of bot runs on is a dropdown.
        app.press("id:nav-settings").unwrap();
        app.press("id:settings-models").unwrap();
        app.wait_for("text:mock, from config.json").unwrap();
        app.press("label:Reasoning").unwrap();
        app.press("label:Reasoning Low").unwrap();
        app.wait_for("text:Low").unwrap();

        // Permissions: one choice, for every bot.
        app.press("id:settings-permissions").unwrap();
        app.wait_for("text:Applies to every bot. Reading never asks.").unwrap();
        app.wait_for("text:Ask first").unwrap();
        app.press("label:Approvals").unwrap();
        app.press("label:Full access").unwrap();
        app.wait_for("text:Full access").unwrap();

        // What is allowed without asking: the person's own rule shows as
        // written; a command is added as the agent writes rules.
        app.wait_for("text:Edit(docs/**)").unwrap();
        app.wait_for("text:No rules yet").unwrap();
        app.press("id:add-allow-rule").unwrap();
        app.type_text("id:allow-rule", "git status").unwrap();
        app.press("label:Add to allow").unwrap();
        app.wait_for("text:git status").unwrap();
        assert_eq!(settings()["permissions"]["allow"], serde_json::json!(["Edit(docs/**)", "Bash(git status:*)"]));

        // What is never allowed.
        app.press("id:add-deny-rule").unwrap();
        app.type_text("id:deny-rule", "echo $(date)").unwrap();
        app.press("label:Add to deny").unwrap();
        app.wait_for("text:/without parentheses/").unwrap();
        app.clear("id:deny-rule").unwrap();
        app.type_text("id:deny-rule", "git push --force").unwrap();
        app.press("label:Add to deny").unwrap();
        app.wait_for("text:git push --force").unwrap();
        assert!(!app.shows("text:/without parentheses/"));
        assert!(!app.shows("text:No rules yet"));
        assert_eq!(settings()["permissions"]["deny"], serde_json::json!(["Bash(git push --force:*)"]));

        // A rule is taken off; the rest of the file is as it was.
        app.press("label:Remove Edit(docs/**)").unwrap();
        app.wait_for_gone("text:Edit(docs/**)", long).unwrap();
        let now = settings();
        assert_eq!(now["permissions"]["allow"], serde_json::json!(["Bash(git status:*)"]));
        assert_eq!(now["permissions"]["defaultMode"], "default");
        assert!(now["mcpServers"].is_object() && now["hooks"].is_object());
        let text = std::fs::read_to_string(home.join("settings.json")).unwrap();
        assert!(text.find("mcpServers").unwrap() < text.find("permissions").unwrap() && text.find("permissions").unwrap() < text.find("hooks").unwrap(), "in the order it had");

        // The other sections.
        app.press("id:settings-projects").unwrap();
        app.wait_for("text:Folders your bots work in").unwrap();
        app.press("id:settings-tools").unwrap();
        app.wait_for("text:MCP servers").unwrap();
        app.wait_for("text:Hooks").unwrap();
        // What the agent has built in (web search) is not a setting.
        assert!(!app.shows("text:Web search"));
        app.press("id:settings-close").unwrap();
        assert!(!app.shows("text:MCP servers"));
    }
    // The access chosen is kept.
    let store = Store::open(home.join("native/bubble.db")).unwrap();
    assert_eq!(store.setting("access").unwrap().as_deref(), Some("full"));
    drop(store);
    let mut app = Headless::start(EngineConfig::new(Root::of::<bubble_app::App>())).unwrap();
    app.wait_for_within("What should we work on?", long).unwrap();
    app.press("id:nav-settings").unwrap();
    app.press("id:settings-permissions").unwrap();
    app.wait_for("text:Full access").unwrap();
    app.wait_for("text:git status").unwrap();
    drop(app);
    std::fs::remove_dir_all(&root).ok();
}
