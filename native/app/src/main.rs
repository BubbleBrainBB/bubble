//! Runs Bubble on the machine's native backend (`cargo run -p bubble-app`).

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

#[cfg(target_os = "linux")]
use nsh_platform_linux as platform;
#[cfg(target_os = "macos")]
use nsh_platform_macos as platform;
#[cfg(target_os = "windows")]
use nsh_platform_windows as platform;

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn main() -> impl std::process::Termination {
    env_logger::init();
    // A development build keeps its data apart from the person's own,
    // unless told where to keep it (BUBBLE_HOME) or not to (BUBBLE_DEV=0).
    if cfg!(debug_assertions) && std::env::var_os("BUBBLE_HOME").is_none() && std::env::var_os("BUBBLE_DEV").is_none() {
        // SAFETY: nothing else runs yet.
        unsafe { std::env::set_var("BUBBLE_DEV", "1") };
    }
    platform::run_app(
        platform::AppSource::Rust(platform::Root::of::<bubble_app::App>()),
        platform::AppOptions {
            title: "Bubble".to_owned(),
            #[cfg(any(target_os = "linux", target_os = "windows"))]
            app_id: "ai.bubblebrain.bubble".to_owned(),
            size: (1280, 820),
            ..Default::default()
        },
    )
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
compile_error!("Run this binary on macOS, Windows or Linux; use nsh run for iOS and Android.");
