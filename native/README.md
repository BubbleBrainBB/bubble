# Bubble Native

Bubble's desktop app on [NSH](https://nsh.dev/docs/). The interface is Rust; the agent is Bubble's TypeScript core, started as a child process (`bubble host --stdio`, JSON-RPC over stdin and stdout) and driven through the SDK. Plan and phases: [docs/native-rewrite-plan.md](../docs/native-rewrite-plan.md).

```text
app                   the NSH app (Rust components)
crates/bubble-host    client for `bubble host --stdio`: starts the agent and talks to it
crates/bubble-store   SQLite: bots, runs, messages, tickets, settings
crates/bubble-github  GitHub through the gh CLI
crates/bubble-core    shared types, configuration and sign-in state (its own agent loop is not used by the app)
scripts               packaging
```

## Requirements

- Rust 1.99 or newer (`rustup update stable`): `nsh-core` 0.4 uses standard-library features 1.93 lacks.
- The `nsh` CLI 0.4 (`curl -fsSL https://nsh.dev/install.sh | sh`) for `nsh run` and `nsh package`.
- Xcode on macOS.
- Node.js 22.19 or newer to run the agent from a development build. A packaged app carries its own.

Behind a proxy, cargo may need it spelled out: `CARGO_HTTP_PROXY=http://127.0.0.1:7890`.

## Run and test

```sh
(cd .. && npm run build)                 # the agent a development build runs: ../dist/main.js
cargo run -p bubble-app                  # the app in a native window
cargo test                               # core, store and a headless run of the app
node scripts/package-macos.mjs           # target/package/Bubble.app, with the agent and Node inside
```

A development build runs the agent from this checkout's `dist/`, so build it again after changing the TypeScript core. `package-macos.mjs` puts this checkout's agent into the app, or a published one with `--bubble <version>`; the app finds it in `Contents/Resources/bubble` and falls back to `bubble` on the PATH (see `crates/bubble-host/src/launch.rs`). A development build keeps its data in `~/.bubble-dev`, a packaged app in `~/.bubble`.

The app reads providers and the default model from the agent, which reads `~/.bubble/config.json` (`$BUBBLE_HOME` overrides the folder), and keeps its own data in `native/bubble.db` under the same folder. Every provider the terminal Bubble has works, sign-in providers included.
