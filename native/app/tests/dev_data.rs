//! A development build's data is kept apart from the person's own: its
//! bots, conversations and database live in `~/.bubble-dev`, which keeps
//! a configuration of its own when it has one and takes over how they set
//! the app up, and their own data is left as it was.

use std::time::Duration;

use bubble_store::{NewTask, Origin, Store};
use nsh_engine::{EngineConfig, Root};
use nsh_testing::Headless;

#[test]
fn development_data_starts_from_the_persons_setup_and_stays_apart() {
    let root = std::env::temp_dir().join(format!("bubble-dev-data-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let (theirs, ours) = (root.join(".bubble"), root.join(".bubble-dev"));
    std::fs::create_dir_all(theirs.join("native")).unwrap();
    // The person's own: a provider, a sign-in file, a preference and a bot.
    std::fs::write(
        theirs.join("config.json"),
        r#"{"providers":[{"id":"mock","name":"Mock","baseURL":"http://127.0.0.1:9","apiKey":"k","enabled":true}],"defaultModel":"mock:m"}"#,
    )
    .unwrap();
    std::fs::write(theirs.join("auth.json"), "{}").unwrap();
    {
        let store = Store::open(theirs.join("native/bubble.db")).unwrap();
        store.set_setting("thinking_level", Some("low")).unwrap();
        store.set_setting("max_concurrent_runs", Some("2")).unwrap();
        store.create_task(NewTask { title: "Mine", project_cwd: None, origin: Origin::User }).unwrap();
    }
    // The development home has a configuration of its own already, with
    // another default model: it is kept, and the model they work with
    // there is written down for both the main bot and the bots.
    std::fs::create_dir_all(&ours).unwrap();
    std::fs::write(
        ours.join("config.json"),
        r#"{"providers":[{"id":"mock","name":"Mock","baseURL":"http://127.0.0.1:9","apiKey":"dev-key","enabled":true}],"defaultModel":"mock:other"}"#,
    )
    .unwrap();
    // SAFETY: the only test in this binary, before the engine starts.
    unsafe {
        std::env::set_var("HOME", &root);
        std::env::remove_var("BUBBLE_HOME");
        std::env::set_var("BUBBLE_DEV", "1");
        std::env::set_var("BUBBLE_NO_SHELL_ENV", "1");
        std::env::set_var("BUBBLE_MEMORY_AUTO", "0");
        std::env::set_var("BUBBLE_GH", root.join("no-gh"));
    }
    // A desktop: there is a mouse.
    let mut config = EngineConfig::new(Root::of::<bubble_app::App>());
    config.environment.os = nsh_core::platform::Os::MacOs;
    config.environment.idiom = nsh_core::platform::Idiom::Desktop;
    let mut app = Headless::start(config).unwrap();
    app.set_viewport(nsh_core::element::size(nsh_core::element::px(1280.0), nsh_core::element::px(820.0))).unwrap();
    app.wait_for_within("id:nav-home", Duration::from_secs(15)).unwrap();

    // The app says whose data this is, and the person's bot is not in it.
    app.wait_for("Development data").unwrap();
    assert!(!app.shows("id:side-Mine"), "the person's own bots are not development data");
    app.wait_for("text:/^No bots yet/").unwrap();

    // It kept its own providers, and took over how they set the app up.
    assert!(std::fs::read_to_string(ours.join("config.json")).unwrap().contains("dev-key"), "its own configuration is kept");
    assert!(!ours.join("auth.json").exists(), "nothing is copied over a home that is set up");
    let kept = Store::peek_settings(
        ours.join("native/bubble.db"),
        &["thinking_level", "max_concurrent_runs", "development_seeded", "manager_model", "task_model"],
    );
    assert_eq!(kept.get("manager_model").map(String::as_str), Some("mock:m"));
    assert_eq!(kept.get("task_model").map(String::as_str), Some("mock:m"));
    assert_eq!(kept.get("thinking_level").map(String::as_str), Some("low"));
    assert_eq!(kept.get("max_concurrent_runs").map(String::as_str), Some("2"));
    assert_eq!(kept.get("development_seeded").map(String::as_str), Some("1"));

    // A bot made here is development data only.
    app.press("id:nav-new-bot").unwrap();
    app.type_text("id:bot-name", "Scratch").unwrap();
    app.press("Create bot").unwrap();
    app.wait_for("id:side-Scratch").unwrap();
    // The pointer over a bot, then over the main bot: their faces turn to
    // it, and the app goes on as before.
    app.hover("id:side-Scratch").unwrap();
    app.hover("id:nav-home").unwrap();
    app.press("id:nav-settings").unwrap();
    app.wait_for("Bots working at once").unwrap();
    drop(app);
    let theirs_db = Store::open(theirs.join("native/bubble.db")).unwrap();
    let names: Vec<String> = theirs_db.list_tasks().unwrap().into_iter().map(|t| t.task.title).collect();
    assert_eq!(names, ["Mine"], "the person's own data is as it was");
    assert!(theirs_db.setting("development_seeded").unwrap().is_none());
    assert!(!theirs.join("sessions").exists(), "the agent kept no conversation in the person's own data");

    std::fs::remove_dir_all(&root).ok();
}
