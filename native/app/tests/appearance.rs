//! Appearance, headless: choosing Dark turns the text light, and the
//! choice is kept for the next start.

use std::time::Duration;

use nsh_engine::{EngineConfig, Root};
use nsh_testing::Headless;

fn is_light(app: &Headless, selector: &str) -> bool {
    let color = app.screen().span_style(selector).unwrap().color;
    let rgba: nsh_core::Rgba = color.into();
    rgba.r > 0.8 && rgba.g > 0.8 && rgba.b > 0.8
}

#[test]
fn dark_appearance_is_chosen_and_kept() {
    let root = std::env::temp_dir().join(format!("bubble-appearance-test-{}", std::process::id()));
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(
        home.join("config.json"),
        r#"{"providers":[{"id":"mock","name":"Mock","baseURL":"http://127.0.0.1:9","apiKey":"k","enabled":true}],"defaultModel":"mock:test-model"}"#,
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
    {
        let mut app = Headless::start(EngineConfig::new(Root::of::<bubble_app::App>())).unwrap();
        app.set_viewport(nsh_core::element::size(nsh_core::element::px(1280.0), nsh_core::element::px(820.0))).unwrap();
        app.wait_for_within("What should we work on?", long).unwrap();
        assert!(!is_light(&app, "text:What should we work on?"), "light by default");
        app.press("id:nav-settings").unwrap();
        app.press("label:Appearance Dark").unwrap();
        app.wait(Duration::from_millis(100)).unwrap();
        assert!(is_light(&app, "text:Appearance"), "dark: light text");
    }
    let mut app = Headless::start(EngineConfig::new(Root::of::<bubble_app::App>())).unwrap();
    app.wait_for_within("What should we work on?", long).unwrap();
    app.wait(Duration::from_millis(100)).unwrap();
    assert!(is_light(&app, "text:What should we work on?"), "still dark after a restart");
    std::fs::remove_dir_all(&root).ok();
}
