# Bubble

Bubble is a local coding agent with a desktop app and a terminal interface. It works inside a local project folder: it reads and edits files, runs commands behind configurable approval controls, searches and navigates code with language-server intelligence, browses the web, loads reusable skills, connects MCP tools, fans work out to subagents, and keeps persistent memory across sessions.

It is provider-agnostic. Bring an API key for OpenAI, Anthropic, Google, DeepSeek, Moonshot/Kimi, Zhipu, Z.AI, MiniMax, Groq, Together, Fireworks, a local OpenAI-compatible endpoint, and more; sign in to ChatGPT with OAuth; or sign in with OAuth to use a ChatGPT or Grok subscription directly.

---

## Desktop app

The desktop app is the primary direction for interactive development. It uses the existing Bubble SDK, providers and session history, with the Aegis desktop layout, working trace, KanBan, automations, pull requests and settings adapted for a standalone Bubble agent. Skills and model selection use Bubble directly.

```bash
npm ci
npm run desktop:install
npm run dev
```

`npm run dev` (or `npm run desktop:dev`) starts Electron with Vite hot updates. UI edits refresh in place; Electron/preload/shared code and Agent SDK edits compile and restart the desktop automatically. The first launch compiles the SDK and host, without a renderer production build. Stop everything with Ctrl+C. Use `npm run dev:tui` for terminal development.

`npm run desktop` still builds and starts the desktop without hot updates; `npm --prefix desktop start` opens the last compiled build. For a local macOS dogfood app and DMG, run `npm run desktop:package`; it includes the Agent runtime and never publishes. Use `npm run desktop:build` to check a build and `npm run desktop:test` to run desktop checks. See [desktop architecture and migration notes](docs/desktop-architecture.md) for runtime boundaries, data migration and development commands. The CLI remains available.

In **Settings → Providers → Bubble**, OpenAI offers **ChatGPT subscription** browser sign-in and a separate **API key** mode. **Grok Subscription** uses browser sign-in only. You can cancel a pending login or sign out from the same panel; OAuth tokens stay in the selected profile's Agent home and are never shown in the API key editor.

Desktop data is separated into three profiles (macOS paths shown):

| Command | Purpose | Desktop data | Agent data |
| --- | --- | --- | --- |
| `npm run dev` | Your persistent development/test sessions | `~/Library/Application Support/Bubble Dev` | `~/.bubble-dev` |
| `npm run desktop:qa` | Agent verification; a fresh profile each launch | System temporary directory `bubble-desktop-qa-*/desktop` | Same temporary root, `agent/` |
| `npm run desktop` / `npm --prefix desktop start` | Normal use with production data | `~/Library/Application Support/Bubble` | `~/.bubble` |

Sessions, board state, automations, UI preferences, model/account configuration, OAuth data, memory and MCP settings follow the selected profile. History import reads only that profile's Agent home. Dev sessions survive restarts; QA uses a different temporary root for every launch (and keeps that root across hot restarts). Configure development models separately: production history and credentials are not automatically copied. Existing production data is left in place. These profiles separate app data, not the project files you ask the Agent to edit.

Run automated verification with `npm run desktop:test`; manual/agent desktop verification must use `npm run desktop:qa`. If your development server is already running, use `PORT=4337 npm run desktop:qa`. QA paths are logged at startup and retained in the system temporary directory for diagnostics. The normal launch commands select their own profile and ignore inherited data-directory overrides.

## Requirements

- Node.js 22.19 or newer — Bubble's only runtime

The `npm install` step puts the `bubble` CLI on your PATH. When you run `bubble`, it starts directly on your Node.js runtime; no other runtime is required.

## Install

One-line install (macOS / Linux):

```bash
curl -fsSL https://raw.githubusercontent.com/DylanDDeng/bubble/main/install.sh | sh
```

Windows (PowerShell):

```powershell
powershell -c "irm https://raw.githubusercontent.com/DylanDDeng/bubble/main/install.ps1 | iex"
```

The installer checks for Node.js 22.19+, then installs the `bubble` CLI globally via npm.

Or install directly via npm:

```bash
npm install -g @bubblebrain-ai/bubble
```

To install from a local tarball:

```bash
npm install -g ./bubblebrain-ai-bubble-<version>.tgz
```

## Quick start

Start Bubble in the current directory:

```bash
bubble
```

On first launch, connect a provider:

- Run `/login` and pick an account: ChatGPT (OAuth, Codex models) or Grok Subscription, or
- Run `/provider` to select Grok Subscription or add a provider with an API key.

Then just type what you want done. Bubble plans, edits files, and runs commands, asking for approval where the current permission mode requires it.

Point Bubble at a different project:

```bash
bubble --cwd /path/to/project
```

Resume your last conversation:

```bash
bubble --resume
```

## Persistent headless sessions

To keep one agent session and its MCP connections alive across multiple prompts:

```bash
bubble -p --input-format stream-json --output-format stream-json \
  --model zhipuai-coding-plan:glm-5.3 --session my-session
```

Write one JSON message per line to stdin (keep stdin open between turns):

```json
{"type":"user","request_id":"turn-1","message":{"role":"user","content":"Inspect the page."}}
{"type":"user","request_id":"turn-2","message":{"role":"user","content":"Continue with the next action."}}
```

Stdout contains only newline-delimited JSON: one `system/init` event with the model
and `session_id`, `stream_event` messages wrapping native agent events (including
`text_delta`, `tool_start`, and `tool_end`), and one `result` per input. Results
include `is_error`, text in `result`, token usage, and the input `request_id` when
provided. This is Bubble's protocol; it does not claim full Claude Code protocol
compatibility. Input content may also be an array of text blocks.

Turns run sequentially in arrival order and retain the same agent history, MCP
connections, and tool discovery state. Malformed input and failed turns produce an
error result; the process remains available for the next input. An optional
`session_id` must match the initialized session. EOF drains submitted turns, then
releases resources. SIGINT/SIGTERM stop the process. Input lines are limited to
1 MiB. Permission settings remain unchanged unless explicitly overridden.

Both stream format flags require `-p`; positional prompts are not accepted in this
mode. To resume a saved session, pass both `--resume` and `--session <name>` so no
interactive picker is needed. Existing plain and single-result JSON print modes
retain their behavior.

## Embed with the SDK

The npm package exports `BubbleSdk` from both `@bubblebrain-ai/bubble` and
`@bubblebrain-ai/bubble/sdk`. Turns are serialized FIFO within one session and
remain parallel across different sessions.

```ts
import { BubbleSdk } from "@bubblebrain-ai/bubble/sdk";

const sdk = new BubbleSdk({ defaultCwd: process.cwd() });
const session = sdk.createSession();
const events = sdk.runTurn(session.id, { prompt: "Inspect this project" });

for await (const event of events) {
  if (event.type === "text_delta") process.stdout.write(event.content);
}

const queued = sdk.enqueueTurn(session.id, { prompt: "Run the tests next" });
void (async () => {
  for await (const event of queued) {
    // Render the queued turn's events.
  }
})();
```

While a turn owns the session slot, `sdk.steer(session.id, text)` adds text at
the next model-call boundary and returns a receipt: `steered` (injected into
the active turn), `queued` (the session is idle or the active turn can no
longer accept input, so the text becomes the next FIFO turn automatically),
or `rejected`. Every accepted receipt includes an `outcome` promise that
settles exactly once to `input_applied`, `input_queued`, or `input_rejected` —
even if the host drops the event iterator early.

Turns are driven by the SDK itself, not by iterator consumption: a returned
iterator is a replay subscription, so dropping it never stalls or cancels the
turn. `openSession(sessionId)` returns a durable handle whose `events` iterable
replays the whole session log from sequence 1 and `eventsFrom(n)` resumes after
the last sequence the host durably processed. Snapshot `handle.latestSequence`
before sending to subscribe only to new work, including automatic fallback
turns. Each turn ends with a `terminal` record (`completed`, `failed`, or
`cancelled`) after its execution slot is released. A completed turn may have
already started a queued successor; hosts should drain buffered records and
check `getSessionRunState(sessionId).active` before showing the session as idle.
`stop(sessionId)` is
Claude-style: it interrupts the active turn and keeps the queue; pass
`{ cancelQueued: true }` to clear both. `clearQueue(sessionId)` cancels only
queued turns, and `deleteSession(sessionId)` awaits teardown before the JSONL
file is removed.

## Model providers

Bubble ships with a catalog of built-in providers. Configure them inside the app — no environment variables required.

| How | What it does |
| --- | --- |
| `/login` | OAuth sign-in for ChatGPT or Grok Subscription; `/login grok` starts the Grok flow directly. |
| `/provider` | Open a picker to connect, switch, add, or remove a provider, including Grok Subscription. |
| `/model` | Pick the active model and reasoning effort. |

### Grok Subscription Provider

A Grok subscription works as a native model provider, exactly like ChatGPT
OAuth: sign in once in the browser and the subscription models (`grok-4.5`,
`grok-composer-2.5-fast`) drive Bubble's own agent loop — Bubble's tools,
skills, MCP servers, subagents, memory, permission modes, and approval UI all
apply as usual. `/model` selects the model and reasoning effort.

```bash
bubble
# In the TUI: /login grok
# or choose Grok Subscription from /provider
```

`/login grok` opens the xAI sign-in page in the default browser. If you have
already signed in with the official Grok CLI, Bubble imports that login and
skips the browser. Tokens are stored in `~/.bubble/auth.json` alongside other
provider credentials and refresh automatically. Use `/logout grok` to remove
this device's login.

Built-in providers include OpenAI, Anthropic, Google, DeepSeek, Moonshot (CN and international), Kimi for Coding, Zhipu AI, Z.AI, Alibaba DashScope, Doubao (Volcengine Ark), MiniMax, StepFun, MOSS, Groq, Together AI, Fireworks, and a `local` profile for any OpenAI-compatible endpoint (Ollama, vLLM, LM Studio, etc.).

For Doubao Seed models on Volcengine Ark, run `/provider --add doubao` and paste your Ark API key. The built-in endpoint is `https://ark.cn-beijing.volces.com/api/v3` and uses Ark's Responses API. The model picker exposes `minimal`, `low`, `medium`, and `high`, defaulting to `high`; `minimal` disables Ark thinking, while the other levels enable it.

For **Xiaomi MiMo Token Plan**, choose **Xiaomi MiMo Token Plan** in desktop
Settings → Providers, or run `/provider --add mimo-token-plan` in the CLI.
Enter the Token Plan key (`tp-…` for personal plans, `ttp-…` for team plans),
then select `/model mimo-token-plan:mimo-v2.6-pro` or `mimo-v2.6-flash`.
Bubble uses Anthropic Messages at
`https://token-plan-cn.xiaomimimo.com/anthropic/v1/messages`, with streaming,
tools, and a thinking on/off switch (on by default). Both models have a 1M
context window and a 128K output cap. V2.5 Pro and V2.5 are also available;
their announced retirement date is October 21, 2026. Use the plan-specific key,
not a pay-as-you-go API key. See the
[official integration guide](https://mimo.mi.com/docs/zh-CN/tokenplan/integration/tools-overview).

For Nex-N2.6 on MOSS, run `/provider --add moss`, paste an API key created at
`platform.moss.cn`, then select `/model moss:nex-n2.6`. The model always
thinks, so it has no reasoning-effort setting. It has a 1M context window and
a 131K output cap.

### Custom providers and models

For StepFun's standard API, run `/provider --add stepfun-api`, enter your API
key, then select `/model stepfun-api:water18-0910`. This uses OpenAI-compatible
Chat Completions at `https://api.stepfun.com/v1`, with streaming and agent tools.
The existing `stepfun` provider continues to use Step Plan. The API model uses
server-default reasoning; effort controls and pricing are not assumed.

For full control — custom base URLs, self-hosted gateways, extra models, or pinning a protocol — define providers in `~/.bubble/models.json`:

```json
{
  "providers": {
    "my-gateway": {
      "baseURL": "https://gateway.internal/v1",
      "apiKey": "sk-...",
      "protocol": "openai-chat",
      "models": [
        { "id": "my-model", "name": "My Model" }
      ]
    }
  }
}
```

`protocol` accepts `openai-chat` (default), `anthropic-messages`, or `ark-responses`. Entries in `models.json` take precedence over the built-in catalog.

### Reasoning effort

Models that support it expose a reasoning-effort control: `off`, `minimal`, `low`, `medium`, `high`, `xhigh`, `max`, `ultra`. Set it with `/model <id> --reasoning-effort <level>` in the app, or at launch with `--reasoning` (medium) or `--reasoning-effort <level>`.

## Permission modes

Bubble gates risky actions behind a permission mode. Press `Tab` to cycle modes during a session, or set a default with `--plan` / `--dangerously-skip-permissions` / project settings.

| Mode | Behavior |
| --- | --- |
| Default (Build) | Workspace file edits and writes auto-approve, except protected files (`.git/`, `.bubble/settings*.json`, `.claude/settings*.json`). Bash, MCP tools and edits outside the workspace prompt unless covered by an allow rule or an "allow for this session" approval. |
| Plan | Read-only investigation. The agent proposes a plan and waits for your approval before making changes. |
| Bypass | Auto-approves every tool and disables all safety prompts. Enable deliberately with `--dangerously-skip-permissions`. |

Allow/deny rules are configured per scope and persisted across sessions. Manage them in-app with `/permissions`, or edit the settings files directly:

- `~/.bubble/settings.json` — user scope (applies everywhere)
- `<project>/.bubble/settings.json` — project scope (commit to share with your team)
- `<project>/.bubble/settings.local.json` — local overrides (gitignore)

The two project files arrive with the repository, so what they grant — allow rules, `mcpServers` (which Bubble would start) and LSP server definitions — only loads once you trust the folder. Like Kimi Code, Bubble asks when it opens such a folder (at startup in the terminal, on the first message in the desktop app), listing what would be enabled; any later change to those settings asks again. Deny rules, and switches that only turn LSP servers off, always apply; deny rules apply even in Bypass mode.

Bash allow rules understand compound commands: every command in `a && b | c` must be covered by some allow rule, and commands with `$(...)`, backticks or file redirections always prompt. Deny rules match any part of the line. "Don't ask again this session" remembers the exact command only. MCP tools are named `mcp__<server>__<tool>`; `mcp__<server>` allows every tool of that server.

Rules use a simple pattern syntax, for example:

```json
{
  "permissions": {
    "defaultMode": "default",
    "allow": [
      "Bash(git status)",
      "Bash(npm run:*)",
      "Read(./src/**)",
      "WebFetch(domain:github.com)"
    ],
    "deny": ["Read(~/.ssh/**)"]
  }
}
```

## What Bubble can do

**Files and code.** Read, write, and make targeted edits; list directories with ls; find files through Bash with find or rg --files; search contents with ripgrep; and navigate with language-server operations (go-to-definition, find references, hover, document/workspace symbols, call hierarchy).

**Shell and dev servers.** Run bounded bash commands with streaming output, and start/stop/inspect long-running dev servers (`npm run dev`, Vite, Next, etc.) with readiness checks and captured logs.

**Web.** Search the web and fetch/extract page contents on demand.

**Skills.** Drop reusable instructions and assets into a `SKILL.md`-based directory and invoke them with `/<skill-name>`. Bubble discovers skills from `~/.bubble/skills`, `~/.agents/skills`, `~/.claude/skills`, and the project's `.bubble/skills`. Browse them with `/skills`.

**MCP tools.** Connect Model Context Protocol servers (stdio, HTTP, or SSE) under the `mcpServers` key of any settings file. Their tools and prompts become available to the agent; manage connections with `/mcp`.

**Subagents.** Bubble can spawn background subagents with independent context, send them follow-ups, wait on their results, and fan a task out across a team — with concurrency limits and token budgets. Define custom agent profiles in `~/.bubble/agents` or a project's `.bubble/agents`.

**Persistent memory.** A background pipeline distills durable facts, preferences, and decisions from past sessions and recalls them later. Inspect and maintain it with `/memory status`, `/memory search <query>`, and `/memory refresh`.

**Sessions.** Every conversation is saved. Resume the latest with `bubble --resume`, or browse and switch sessions in-app with `/session`. Use `/rewind` to roll the conversation — and optionally your file edits — back to an earlier point.

## Useful slash commands

| Command | Description |
| --- | --- |
| `/help` | List available commands. |
| `/model` | Switch model and reasoning effort. |
| `/provider`, `/login`, `/logout` | Connect and manage providers. |
| `/session`, `/rewind`, `/clear` | Manage conversation history. |
| `/skills` | Open the searchable skills picker. |
| `/mcp` | List or reconnect MCP servers. |
| `/memory` | Inspect and maintain persistent memory. |
| `/permissions` | View or edit allow/deny rules. |
| `/context`, `/stats`, `/compact` | Inspect context usage, model stats, and compact the session. |
| `/lsp`, `/hooks` | Manage language servers and lifecycle hooks. |
| `/theme` | Adjust the interface theme. |
| `/feedback` | Send feedback or report a bug. |

## Non-interactive mode

Run a single prompt and print the result — useful for scripts and pipelines:

```bash
bubble --print "summarize what this repo does"
echo "fix the failing test" | bubble --print
```

Combine with `--model`, `--cwd`, and `--plan` as needed.

## CLI reference

```text
Usage:
  bubble [options] [prompt]              Start interactive TUI
  bubble update [--check]                Update to the latest version (alias: upgrade)
  bubble serve --feishu [options]        Run as a Feishu bot host

Options (default):
  -m, --model <model>      Model to use
  --cwd <dir>              Working directory (default: current)
  -k, --api-key <key>      API key for the active provider
  -r, --resume             Resume a previous session (latest by default)
  --session <name>         Session name to create or resume
  --reasoning              Enable reasoning mode at medium effort
  --reasoning-effort <l>   Set reasoning effort: off|minimal|low|medium|high|xhigh|max|ultra
  --plan                   Start in plan mode (read-only investigation; propose before executing)
  --dangerously-skip-permissions
                           Enable bypass mode (auto-approve EVERY tool; disables all safety prompts)
  -p, --print              Non-interactive mode (single prompt)
  -v, --version            Print the installed version and exit
  -h, --help               Show this help
```

Run `bubble update` at any time to upgrade to the latest published version.

## Configuration and storage

Bubble keeps everything under `~/.bubble`:

```text
~/.bubble/
  config.json          User preferences (theme, default model, recent models)
  models.json          Custom providers and models
  auth.json            OAuth credentials (file mode 0600)
  settings.json        User-scope permissions, MCP servers, hooks
  sessions/            Saved conversations, grouped by project directory
  memories/            Persistent memory store
  skills/              User skills
  agents/              Custom subagent profiles
```

Environment variables:

- `BUBBLE_HOME` — override the data directory (defaults to `~/.bubble`).
- `BUBBLE_DEV=1` — use `~/.bubble-dev` instead, for development.

### Network configuration

ChatGPT OAuth and GPT/Codex requests respect the standard proxy variables:

```bash
export HTTPS_PROXY=http://proxy.example.com:8080
export HTTP_PROXY=http://proxy.example.com:8080
export NO_PROXY=localhost,127.0.0.1
```

If your network uses a corporate or custom HTTPS CA:

```bash
NODE_EXTRA_CA_CERTS=/absolute/path/to/ca.pem bubble
```

`BUBBLE_EXTRA_CA_CERTS` applies the same trust to Bubble's ChatGPT requests specifically. Do not disable TLS verification with `NODE_TLS_REJECT_UNAUTHORIZED=0`.

## Development

```bash
npm install         # install dependencies
npm run build      # compile TypeScript to dist/
npm test           # run the test suite (vitest)
npm start          # run the built agent
```

`npm run dev:tui` compiles and launches the terminal interface in one step.

## Feishu host (optional)

Bubble can also run as a Feishu (Lark) bot host:

```bash
bubble serve --feishu --setup
```

`--setup` runs the binding wizard, `--kill-old` replaces a conflicting instance for the same App ID, and `--dry-run` connects once and exits as a smoke test.


## 环境变量

### BUBBLE_PROVIDER_REQUEST_TIMEOUT_MS

控制 LLM provider 请求的超时时间(毫秒)。

- **未设置**(默认):**无超时限制**,适合 streaming API 和深度推理模型
- **设置为正整数**(如 `300000`):强制 5 分钟超时(适合 CI 等需要 fail-fast 的场景)

**默认行为**:

Bubble 默认**不设置超时**。对于 streaming API,模型会持续发送 chunk,连接在完成时自然关闭。这对深度推理模型(如 GLM-5.2 max reasoning)尤其重要,因为单个请求可能需要 10+ 分钟。

**仅在需要强制超时时设置**:

```bash
# CI 环境中强制 5 分钟超时
export BUBBLE_PROVIDER_REQUEST_TIMEOUT_MS=300000
```
