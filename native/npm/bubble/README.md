# Bubble

A terminal coding agent.

```bash
npm install -g @bubblebrain-ai/bubble
bubble
```

Bubble is a native program. This package runs the build for your machine,
which npm installs from one of the platform packages
(`@bubblebrain-ai/bubble-darwin-arm64`, `-darwin-x64`, `-linux-x64`,
`-linux-arm64`, `-win32-x64`).

- `bubble` — the full-screen session in the current folder
- `bubble -p "…"` — one prompt, printed (`--output-format json|stream-json`)
- `bubble update` — install the latest version
- `bubble --help` — everything else

Settings, sign-ins and sessions live in `~/.bubble`, shared with the desktop
app and with earlier versions of Bubble.
