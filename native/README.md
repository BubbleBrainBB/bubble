# Bubble Native

The Rust rewrite of Bubble's desktop app on [NSH](https://nsh.dev/docs/). Plan and phases: [docs/native-rewrite-plan.md](../docs/native-rewrite-plan.md).

```text
crates/bubble-core    configuration (~/.bubble/config.json) and streaming providers
crates/bubble-store   SQLite: tasks, runs, messages, links, events, settings
app                   the NSH app (Rust components)
```

## Requirements

- Rust 1.99 or newer (`rustup update stable`): `nsh-core` 0.4 uses standard-library features 1.93 lacks.
- The `nsh` CLI 0.4 (`curl -fsSL https://nsh.dev/install.sh | sh`) for `nsh run` and `nsh package`.
- Xcode on macOS.

Behind a proxy, cargo may need it spelled out: `CARGO_HTTP_PROXY=http://127.0.0.1:7890`.

## Run and test

```sh
cargo run -p bubble-app          # the app in a native window
cargo test                       # core, store and a headless run of the app
cd app && nsh package --target macos
```

The app reads providers and the default model from `~/.bubble/config.json` (`$BUBBLE_HOME` overrides the folder) and keeps its own data in `~/.bubble/native/bubble.db`. OpenAI-compatible and Anthropic-compatible providers with an API key work today; sign-in providers (ChatGPT, Grok) come later.
