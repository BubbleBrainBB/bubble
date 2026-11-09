# Bubble Native：NSH + Rust 重写计划

状态：**v1，执行中**（2026-11-08）。

## 决定

2026-11-08 用户拍板：

- 桌面端改用 [NSH / Native Shell](https://nsh.dev/docs/)（longbridge，Rust 优先的原生 UI 框架：macOS 用 AppKit，iOS 用 UIKit，另有 Linux / Windows / Web）。
- 界面用 Rust 写，不用 TSX。
- Agent 核心也用 Rust 重写，不再让桌面端经 TypeScript SDK 调用。
- Electron 桌面版（`desktop/`）立即停止维护。未提交的 `feat/desktop-local-task-first` 改动搁置、不删。

产品形态沿用 [desktop-manager-design.md](desktop-manager-design.md)：Manager 首页、Task 实体、GitHub 事件、Inbox、Settings。设计稿在 Artifact "Bubble Desktop Manager"。那份文档里的 Electron 具体实现（IPC、`hostTools`、zustand 等）作废，概念模型、数据表、GitHub 轮询与路由、自主级别、界面结构仍然有效。

## 规模

TypeScript 核心（不含 TUI）约 35k 行，主要部分：

| 部分 | 行数（约） | Rust 版处理 |
|---|---|---|
| agent 循环、上下文、压缩（`agent.ts`、`agent/`、`context/`） | 1 万+ | 分阶段重写 |
| providers（`provider-*.ts`） | 9k | 先做 OpenAI 兼容 + Anthropic 兼容，OAuth 类后做 |
| 工具（`tools/`） | 5k | 先做 read / write / edit / bash / grep / ls |
| 审批与权限（`approval/`、`permissions/`） | 1.9k | 第一阶段就要 |
| MCP、skills、memory、hooks、LSP、子代理、workflow | 6k+ | 后续阶段 |
| 飞书、external-runtime、slash 命令、TUI | — | 不进 Rust 版（或另议） |

这是数月量级的工作。目标是尽早有一个能日常用的纵向切片，而不是一次性对齐 TS 版全部功能。

## 结构

仓库新增 `native/`，Cargo workspace：

```text
native/
  Cargo.toml                 workspace
  crates/
    bubble-core/             Agent 核心：配置、provider、消息模型、agent 循环、工具、审批
    bubble-store/            SQLite（rusqlite）：tasks / runs(sessions) / messages / events / links
    bubble-manager/          Manager 与任务调度：建任务、派 Run、事件路由（后续阶段）
    bubble-github/           gh 轮询与差分（后续阶段）
  app/                       NSH 应用（Rust 组件），nsh.json 在这里
```

- 核心不依赖 NSH，可单独 `cargo test`，以后 CLI 或 Mac mini Host 也能复用。
- 界面只经 `bubble-manager` / `bubble-store` 的 Rust API 拿数据，不直接碰 provider。
- 配置复用 `~/.bubble/config.json` 与 `auth.json` 的现有格式，用户不用重新配 provider。

## 阶段

| 阶段 | 内容 | 完成标准 |
|---|---|---|
| N0 脚手架 | workspace、NSH 应用骨架（侧栏 + Home + Tasks 页按设计稿）、`bubble-store` 建表 | `nsh run` 出窗口；`cargo test` 过 |
| N1 能聊天 | `bubble-core`：读 config、OpenAI 兼容与 Anthropic 兼容流式、消息模型；Home 输入框发消息、流式显示回复；会话落库 | 用你现有的默认 provider 在 Home 里来回对话，重启后记录还在 |
| N2 能干活 | 工具 read / write / edit / bash / grep / ls；审批（ask / allow once / deny）；任务 Run 在项目目录里执行 | 在任务页让 Agent 改一个文件、跑一条命令，审批卡片进 Inbox |
| N3 Manager | Manager 工具（建任务、跟进、查任务）、任务卡片、多 Run、并发上限与排队、Settings 里的模型与并发 | 设计稿 Home 那段对话能真实跑通 |
| N4 GitHub | 订阅轮询、事件路由、对外动作工具与自主级别、通知发现与分诊 | 设计稿任务页的"Run 2 由 review 触发"真实发生 |
| N5 补齐 | 上下文压缩、MCP、skills、memory、ChatGPT / Grok OAuth、子代理 | 按日常使用的缺口排序 |

每阶段结束都能用，可以单独提交。

## 待定（不阻塞 N0–N2）

- ~~TS CLI 何时、是否换用 Rust 核心~~：2026-11-09 定为 npm 同名包按平台分发 Rust 二进制、作为新大版本替换 TS 版（发布由用户决定何时触发）。
- ~~旧会话历史（`~/.bubble/sessions`）要不要导入~~：2026-11-09 定为全部导入（已做）。
- 打包与签名：CLI 走 npm 不需要签名；桌面应用（NSH 打的 Bubble.app）仍未签名，需要 Apple Developer ID 证书，由用户提供。

## 进度

- 2026-11-08：N0 完成，N1 主体完成（未提交）。
  - `bubble-store`：全部表、状态推算（新增"待查看"：Run 跑完、任务未关闭）、中断恢复，8 个测试。
  - `bubble-core`：读 `~/.bubble/config.json`、OpenAI 兼容与 Anthropic 兼容流式、provider `headers`、按 TS 版名单请求 usage，12 个测试（含本地假服务器）。
  - `app`：侧栏、Home（Manager 对话，流式 + Markdown）、Inbox、Tasks（新建任务、选项目文件夹）、任务页（多 Run、标记完成）、Settings（Manager 与任务模型、并发上限），1 个无头 UI 测试走完全流程。
  - `nsh package --target macos` 出 `Bubble.app`；用真实 `kimi-for-coding:k3` 在 Home 收到回复。
  - 未做：推理内容展示、ChatGPT / Grok 登录类 provider、Settings 模型选择改成下拉、深色模式。

- 2026-11-08（续）：界面改为照 Electron 版 Bubble 的样式（试过 Slack 式，用户否决）。N2 完成（未提交）：
  - `bubble-core`：结构化消息（文本 / 工具调用 / 工具结果 / 推理），两种协议的工具调用流式解析；Kimi 类在带工具调用的回复里回传推理、DeepSeek 每条回传；工具 read / write / edit / bash（走登录 shell 拿到 PATH，超时 120s，输出截断）/ grep（遵守 .gitignore）/ ls；agent 循环最多 50 步，写文件、改文件、跑命令先问人（允许一次 / 本任务内允许 / 拒绝；"本任务内"只记完全相同的命令或本项目的文件改动）。26 个测试。
  - `bubble-store`：schema v2，消息加结构化 `content` 列，v1 库自动迁移。10 个测试。
  - `app`：对话和运行中的回合挂在应用根上，切页面不会中断；工具行可展开看输出；审批卡片；等你时任务显示"需要你"、Inbox 亮点；中断的工具调用在历史里补上"未运行"结果。无头测试走完审批全流程。
  - 用真实 `kimi-for-coding:k3` 在临时项目里跑通：列目录 → 请求运行命令 → 允许 → 文件写出 → 回复总结。
  - 未做：停止按钮、并发上限的排队、系统通知、上下文压缩。

- 2026-11-08（续）：N3 完成（未提交）：
  - `bubble-core`：agent 循环支持"宿主工具"（由 app 回答的工具调用），没有项目的回合不提供文件和命令工具。27 个测试。
  - `app`：Manager 工具 list_tasks / get_task / list_projects / create_task / continue_task / update_task；Manager 的提示词要求先查已有任务再建、brief 要自成一体；它建的任务在 Home 里显示成跟随状态的卡片，点开进入任务；任务页把 brief 显示为"From Manager"。
  - 并发上限生效：超过上限的任务排队（"Queued until another task finishes"），有任务结束就按先来后到启动；重启后排队中的任务继续排队。
  - 无头测试：上限设 1，Manager 建任务 → 等批准；手动建第二个任务 → 排队；批准第一个 → 完成后第二个自动开始 → 批准 → 完成。
  - 真实 Kimi K3 实测：在 Home 说"在 demo-project 里加一个 CHANGELOG.md"，Manager 先查任务和项目，建任务并写出完整 brief，任务 agent 两次请求批准后写出 CHANGELOG.md，卡片变成"Ready for review"。
  - 未做：停止按钮、系统通知、上下文压缩、Manager 被任务完成唤醒汇报。

- 2026-11-08（续）：N4 完成（未提交）：
  - 新 crate `bubble-github`：经 `gh`（以本人身份；`BUBBLE_GH` 可替换成测试脚本）读 PR / issue 快照、通知；快照差分出事件（新评论、新 review、CI 失败 / 转绿、合并、关闭、重开），首次查看只当基线；自己和机器人的评论不触发。7 个测试。
  - `bubble-store`：任务与 issue / PR 的关联（一个只属于一个任务）、事件（按去重键只记一次）、通知游标、`external` 消息角色。13 个测试。
  - `bubble-core`：`git push` 和 `gh` 写操作一律每次询问，审批卡片不出现"本任务内允许"。28 个测试。
  - `app`：每 60 秒轮询一次（只看在 Settings 里开了的仓库）。级别：关 / 只看 / 准备 / 执行。已关联的 issue / PR 的新评论、review、CI 失败作为"From GitHub"消息交给任务（准备及以上）；合并或关闭则任务标完成；通知里指派给你的 issue、请你 review 的 PR 变成"待开始"任务，带 brief 和关联，点 Start 开工（执行级别自动开工）。任务 agent 多了 github_comment / github_open_pr / github_close_issue（执行级别外都要批准；开 PR 自动关联）。Manager 建任务可带 issue / PR 号，另有 link_github。任务顶栏显示 Issue / PR 标签，点开浏览器。
  - 无头测试：假 gh + 假模型，指派的 issue → 待开始任务 → Start → 新评论送到 agent → agent 回评论要批准（无"本任务内允许"）→ 批准后假 gh 收到评论。
  - 真实环境：打包后的 App 通过登录 shell 找到 gh，识别本仓库为 BubbleBrainBB/bubble，设为"只看"后轮询正常（不建任务、不发任何东西）。
  - 未做：用模型分诊 @提及等其他通知、Home 的"你离开期间"汇总、合并永远不提供工具（设计如此）。

- 2026-11-08（续）：定为最终只保留 Rust 版。补齐第 1 批完成（未提交）：
  - 停止：运行中的任务随时可停（模型请求和正在跑的命令一起结束），已流出的回复保留，排队中的任务可撤出队列。
  - 项目规则：读 `~/.bubble/AGENTS.md`、项目根（向上找 .git）的 `AGENTS.md` 和 `.bubble/AGENTS.md`，格式与 TS 版一致，任务带全部、Manager 只带全局。
  - 推理设置：按 TS 版目录移植各模型的上下文窗口和推理档位（向下取最近的档位），Kimi / DeepSeek / GLM / 百炼 / 阿里云托管 Kimi / MiniMax / StepFun / Anthropic 各自的请求字段、温度规则和推理回传方式；用 config 的 `defaultThinkingLevel`，输入框显示实际档位；推理过程流式显示，回复后可展开"Show reasoning"。
  - 上下文压缩：沿用 TS 版阈值（窗口 − 3.3 万 token；未知窗口 3.2 万）和估算（以上次实际用量为锚），保留第一条指令（8192 字以内）、最后一条指令和最近两组工具调用，其余交给模型按 TS 版提示词写交接摘要；结果存为检查点，之后的回合从检查点开始。未移植 TS 版的兜底：摘要失败时的启发式压缩、provider 报超长后的恢复。
  - 测试 70 个全过（core 43、store 13、github 7、app 7）。真实 Kimi K3：档位显示 high、推理可展开；项目规则确认进入系统提示（无头测试断言），但 K3 在已有对话里没照做。

- 2026-11-08（续）：第 2 批完成（未提交）：
  - 订阅凭据：读 `~/.bubble/auth.json` 的 `openai` / `grok`，到期前 5 分钟刷新；刷新持 `auth.json.lock`（O_EXCL，30 秒算死锁），拿锁后重读、别的进程已刷新就直接用，写回走临时文件 + rename、权限 0600。请求 401 时强制刷新一次再重试。登录本身仍用 CLI 的 `/login`（原生 PKCE 登录未做）。
  - Responses 协议三种方言：ChatGPT Codex（`chatgpt.com/backend-api/codex/responses`，Codex 头、prompt_cache_key；lite 模型把工具和指令放进 input）、火山 Ark（系统消息内联、thinking 开关）、OpenAI 兼容（`openai-responses`）。
  - Grok 订阅走 `cli-chat-proxy.grok.com/v1`，客户端版本改为 1.0.13（服务端已拒绝 0.2.93，TS 版的 Grok 因此不可用）。
  - 模型发现：启动时向已登录的订阅要模型列表（ChatGPT `codex/models`、Grok `/models`），上下文窗口、推理档位、lite 形态以发现结果为准，加入 Settings 的模型列表。Settings 新增"Reasoning"档位（存本地设置，覆盖 config 默认）和"Subscriptions"登录状态。
  - 重试：408/429/5xx/529 和连接错误最多重试 4 次，带抖动退避，Retry-After 最多等 60 秒。
  - 测试 85 个全过（core 58、store 13、github 7、app 7）。真实验证：ChatGPT gpt-6.1-sol / gpt-5.6-terra 文本和工具调用、Grok grok-4.7 工具调用、Grok token 刷新写回；应用里 Manager 换成 gpt-6.1-sol 后对话正常。
  - 未移植：openai-responses 的加密推理回放、Google 原生协议（Google 走 OpenAI 兼容地址可用）、豆包非流式的特殊处理。

- 2026-11-08（续）：第 3 批完成（未提交）：
  - MCP：读 `~/.bubble/settings.json` 和项目 `.bubble/settings.json` / `settings.local.json` 的 `mcpServers`（后者覆盖前者，`${VAR}` 展开），stdio 和 HTTP（含 sse）两种传输，自写 JSON-RPC（initialize、分页 tools/list、tools/call，回应 ping），不依赖 SDK。连接由应用级 Hub 持有、各任务共享，失败的 5 分钟后再试，Settings 可手动 Retry。工具名 `mcp__<server>__<tool>`，与 TS 一样默认"延迟加载"：系统提示列名字，模型用 `tool_search` 载入 schema 后才能调用；每次调用都要批准（可"本任务内允许"）。stdio 子进程用登录 shell 的环境（从 Finder 打开的应用没有 PATH），应用退出时随之结束。项目里的服务器直接信任（项目是用户自己添加的文件夹），与 TS 的"打开时询问信任"不同。
  - 技能：扫描 `~/.bubble/skills`、`~/.agents/skills`、`~/.claude/skills`、项目 `.bubble/skills` 和 config 的 `skillPaths`，同名取第一个；`skills.disabled` 关闭。系统提示里放技能目录（预算 12000 字，每条描述 120 字），`skill` 载入正文、`skill_search` 按名称/标签/描述/中文双字搜索，均照搬 TS。比 TS 多支持 YAML 块标量（`description: |`），本机有 32 个技能这样写，TS 里它们的描述只剩一个"|"。
  - 网页：`web_search` / `web_fetch` 走 Exa 托管 MCP，参数和 `BUBBLE_WEB_SEARCH_URL` / `BUBBLE_EXA_API_KEY` 同 TS。
  - 只给任务（有项目文件夹时），Manager 不带这些工具。Settings 新增 MCP 服务器状态（工具数或错误 + Retry）、技能数量和无法使用的数量、网页工具说明。
  - 测试 101 个全过（core 73、store 13、github 7、app 8，其中新增无头测试：假 stdio MCP 服务器 → Settings 显示状态 → 任务里 tool_search → 批准 → 调用 → 结果回到模型）。真实验证：Kimi K3 在 demo-project 里用 tool_search 载入 arxiv 的 search_papers、批准后查到 1706.03762，又用 web_search 查到 Rust 1.99.1；问 ImageMagick 时自己载入了 imagemagick-conversion 技能。
  - 未做：MCP 的 prompts（TS 里是斜杠命令）、OAuth 类 MCP、`/技能名 任务` 的输入快捷方式、按规则放行 MCP 工具（TS 的 `mcp__server` 规则）。

- 2026-11-08（续）：第 4 批完成（未提交）：
  - Hooks：读 settings.json 的 `hooks`（user / project / local，规则格式、优先级排序、超时、输出上限、`onError`、`include`、`exposeToModel`、`inheritToSubagents` 同 TS），命令收 JSON 信封，退出码 2 或 `decision: deny` 拒绝。接线：UserPromptSubmit、PreModelCall、PreToolUse、PostToolUse/Failure、PermissionRequest/Result、Stop、StopFailure、PreCompact/PostCompact、SubagentStart/Stop；只有前四类可拒绝（同 TS）。模型上下文以 `<system-reminder>` 附在用户消息或工具结果后。日志写 `~/.bubble/hooks/日期.jsonl`。项目 hooks 直接信任（与 MCP 一致，TS 要 `/hooks trust`）。Session*、Notification、Steer* 事件暂无对应时机未接。
  - 记忆：读路径同 TS（memory_summary.md 进系统提示、`memory` 工具读摘要或搜索）。写路径：应用启动后台跑 phase 1（最近 24 个任务，消息数 ≥4 且有变化的才读）和 phase 2（本应用的提取 + TS 的 state.sqlite 里已有的 704 条，取最新 40 条整合），提示词、文件格式、脱敏同 TS；模型用任务模型；`BUBBLE_MEMORY_AUTO=0` 关闭。比 TS 多：JSON 解析失败再问一次（实测 K3 首次整合输出了坏 JSON）、phase 2 失败会在下次启动重试。Settings 显示记忆状态。
  - 子代理：`spawn_agent` / `wait_agent` / `send_input` / `close_agent` / `list_agents`，内置 default / explorer / worker 和 search 等四个 subtask 档案，加 `~/.bubble/agents/*.md`、项目 `.bubble/agents/*.md`（格式同 TS）。子代理只读、不能提问（需审批的工具直接拦下）、不能再派子代理；同时最多 8 个，其余排队；收尾只有叙述时追加一次"最终交接"回合（同 TS 的阈值）。任务页对话下方列出子代理，点开看进度和结论；Stop 会一并停掉。
  - 工作流：`run_workflow` 在 QuickJS（rquickjs，独立线程）里跑模型写的 JS，`agent()` 派只读子代理，`parallel` / `pipeline` / `phase` / `log` / `args` / `budget` 前奏、确定性限制（禁 Date / Math.random）、10 秒计算中断、提交前语法检查、schema 返回 JSON，都同 TS。与 TS 不同：工具阻塞到结束（不设 `wait_workflow`），Stop 会取消。
  - 测试 120 个全过（core 90、store 14、github 7、app 9；新增无头测试：任务派子代理并收结果、跑两个成员的工作流）。真实验证（Kimi K3）：explorer 子代理调查 hooks 触发点（8 次只读工具）结论准确；工作流并行 3 个 agent 按 schema 返回 {file, purpose}；应用里新建任务派子代理，对话下方显示 running → completed；启动时记忆管线提取 2 个任务并与 TS 记忆整合。
  - 未做：可写子代理（git worktree 隔离的 implementer）、按 429 自适应的调度、按模型档位的 category 路由、子代理的预算统计（workflow 的 budget 恒为不限）、子代理跨应用重启续跑。

- 2026-11-08（续）：第 5 批第一部分完成（未提交）：Rust CLI，`native/cli`，二进制名 `bubble`（未安装到 PATH，不影响 npm 版）。
  - 参数同 TS：`-p`、`-m`、`--cwd`、`-r`、`--session`、`--reasoning(-effort)`、`--plan`、`--dangerously-skip-permissions`、`--output-format plain|json|stream-json`、`--input-format stream-json`、`-v`、`-h`；命令行带 prompt 即单次运行，`-p` 无 prompt 时读管道。
  - 审批同 TS：默认（Build）模式文件改动自动通过，命令和 MCP 工具在无界面时拒绝（"No interactive UI is available…"）；`--plan` 拒绝一切改动；bypass 全部放行。
  - 输出同 TS：plain 流式文本加 `[Tool: x]` / `[Result: …]`；json 只输出一个对象（text、stopReason、sessionId、num_turns、num_tool_calls、usage、usage_reported），出错时输出 error 对象并退出码 1；stream-json 常驻会话（init、每个事件一行 stream_event、每轮一个 result、坏输入回 invalid_input）。usage 只有输入/输出，缓存分项为 0（provider 尚未解析缓存命中）；json 不含 TS 的 changes / compaction 字段。
  - 系统提示词照搬 TS 的组成（按模型家族的开场、环境与工具列表、工作准则、模式说明、委派策略、AGENTS.md），去掉本运行时没有的部分（exit_plan_mode、question、路由菜单、worktree 子代理）；技能、记忆、延迟工具由 toolbox 追加。工具与 app 任务相同：文件/命令、技能、网页、记忆、MCP、hooks、子代理、工作流。
  - 会话存在同一个 bubble.db：每个 CLI 会话是一个 origin=cli 的任务（项目为 cwd），每次进程是它的一个 run，因此会出现在桌面应用里、也会进入记忆管线。`--session 名字` 续接同名会话，`-r` 续接该目录最新的 CLI 会话。数据库加了 5 秒 busy_timeout，应用和 CLI 可同时写。
  - 交互模式目前是行模式（不是全屏 TUI）：逐行读 prompt，流式输出，工具一行一条，命令前问 y / a（本会话总是允许）/ n，Ctrl-C 停当前回合，`/exit` 退出，支持 `-r` / `--session` 续接。
  - 测试 130 个全过（新增 CLI 10 个：参数、Build/Plan/bypass 三种审批、json 统计、会话续接与管道输入、stream-json 多轮、交互模式审批）。真实验证：K3 单次运行列目录读文件；json 模式里文件改动自动通过、`wc` 被拒（同 TS）、用量统计正确；`-r -m openai:gpt-6.1-sol` 续接上一会话并复述做过的事。
  - 未做：全屏 TUI（TS 版的 pi-tui 界面：多行编辑、斜杠命令、模型切换、会话选择、审批卡片等）、`serve --feishu`、`update`、缓存 token 统计、json 的 changes 字段。

- 2026-11-08（续）：第 5 批第二部分完成（未提交）：全屏 TUI，界面用 NSH 组件写。
  - NSH 没有终端后端，新写了一个：`native/crates/bubble-term`（约 1300 行）。引擎照常在点坐标里排版，一列 = 8 点、一行 = 16 点；文字用自写的 CellTextEngine 量（按终端显示宽度，CJK 占两列，按词换行，`number_of_lines` 截断加省略号）。引擎的 mount batch 建成原生视图树（视图、文字、输入框、滚动视图、keymap、焦点命令），画进字符格：背景色、边框画成方框字符（圆角用 ╭╮╰╯）、粗体/斜体/下划线、真彩色前景、透明度低的整棵子树变暗、滚动视图裁剪并带滚动条。写终端只发变化的格子（同步输出 ?2026，无闪烁）。按键先给 app 的 `on_key` 快捷键，再给聚焦的输入框（平台自己做文本编辑：光标移动、Ctrl-A/E/U/K/W、Alt/Shift+Enter 或 Ctrl-J 换行、粘贴走 bracketed paste）；点击按下最近的可按视图或聚焦输入框，滚轮和 PgUp/PgDn 滚动。剪贴板走 OSC 52。另有 `Virtual`：不占终端跑 app、打字点击读屏，测试用。
  - TUI 本身（`native/cli/src/tui/`）是普通 NSH 组件：signals、`each`、`when`、`dynamic`、nsh-base 的 input。布局：欢迎区（像素猫 + 模型、目录）和对话在可滚动区；下方依次是运行状态（转圈、秒数、排队数、Esc 停止）、审批卡片（y / a / n 键或点击）、选择器、斜杠命令提示、多行输入框（边框颜色随 Build/Plan/bypass）、一行页脚（模式、模型与档位、目录、会话名、上下文占用）。助手回复按 markdown 画（标题、列表、引用、代码块、行内代码和粗体），推理折叠为一行，工具一行一条带状态色点和 3 行输出预览，Ctrl-O 展开全部。
  - 斜杠命令：/model（无参数开选择器）、/effort、/plan（Tab 也可切换）、/session（选择并续接本目录会话）、/new、/clear、/help、/quit。Ctrl-C：运行中停止 → 清空输入 → 退出；Ctrl-D 空输入退出；运行中可继续输入，按顺序排队。会话在后台任务里跑（`runner.rs`），停止走 Notify 而不是 SIGINT（raw 模式收不到）。
  - 什么时候进 TUI：没有 prompt、标准输入和输出都是终端；`BUBBLE_TUI=0` 退回行模式，`BUBBLE_TUI_MOUSE=0` 不接管鼠标（接管时多数终端按住 Shift/Option 仍可选字）。
  - 顺带修的问题：(1) 抓登录 shell 环境的 `zsh -ilc` 会抢走终端的前台进程组，导致 TUI（以及行模式）收不到按键；现在它和 bash 工具、MCP stdio 服务器、hooks 的子进程都用 setsid 起在独立会话里，没有控制终端，也就不会和 TUI 抢屏幕或读用户输入。(2) 只打开不说话的会话退出时删除，`/session` 只列有消息的会话。(3) GitHub 轮询改为先发现新任务、再看已关联的 issue，新任务当场拿到基线快照；原来要等下一轮（生产 60 秒），期间的新评论会被当成基线吞掉（app 的 GitHub 测试因此偶发失败）。
  - 测试 136 个全过（新增：终端后端单元测试和一个 NSH 小应用的打字/快捷键/点击测试；TUI 在虚拟终端上对假模型跑完整一轮：写文件、命令审批按 y、markdown 回复、斜杠提示、模型选择器切换、Ctrl-C 退出）。真实验证：在 pty + 终端模拟器里用 Kimi K3 跑，读文件、审批后跑 `wc -c`、回答渲染正确；改窗口大小、PgUp、会话选择器正常。
  - 未做：输入历史（上下键翻以前的 prompt）、`@文件` 补全、图片粘贴、/compact /context /mcp /skills /login 等其余命令、主题切换（目前按深色终端配色）、TS 的 diff 预览。

- 2026-11-08（续）：第 6 批完成（未提交）：让 Rust CLI 能替代 TS CLI 日常使用。
  - 原生登录：`bubble-core/src/login.rs`，PKCE + 本地回调，参数照搬 TS（ChatGPT 固定 1455 端口 `/auth/callback`，Grok 动态端口 `/callback` + nonce + `referrer=grok-build`）；授权码换 token 后在刷新锁下写 `auth.json`（格式同 TS，其他条目不动）；Grok 先尝试导入官方 Grok CLI 已有的登录（`~/.bubble/runtimes/grok/grok-home/auth.json` 或 `~/.grok/auth.json`）。TUI 加 `/login`、`/logout`（无参数弹选择器，等待浏览器时 Esc 取消）；桌面应用 Settings 的订阅一栏加了 Sign in / Sign out 按钮，不再需要 TS CLI 登录。验证：两家授权页都正常显示登录页（参数被接受），没有实际登录；换 token 与保存用假 token 服务器测试。
  - TUI 输入历史：与 TS 共用 `~/.bubble/input-history.jsonl`（同格式，按目录过滤）；输入框为空、或正显示一条未改动的历史时，上下键翻历史，否则上下键照常移动光标。
  - `@文件`：输入 `@` 后按名称/路径打分补全（同 TS 规则），Tab 或回车填入；发送时把提到的文件内容附在 prompt 后（≤200KB 内联，超出只写说明，项目外的路径忽略），格式同 TS。文件列表按 .gitignore 过滤，每轮结束后刷新。
  - 新命令：`/compact [要保留什么]`（立即压缩，焦点文字加进摘要提示）、`/context`（系统提示、工具、消息各占多少，占窗口百分比）、`/mcp`（服务器状态，`/mcp reconnect 名字` 重连）、`/skills`（选择器，选中后填入 `/技能名 `）；`/技能名 任务` 快捷方式按 TS 的措辞让模型先载入该技能。
  - 缓存 token：三种协议都解析缓存命中（OpenAI `prompt_tokens_details.cached_tokens` / DeepSeek `prompt_cache_hit_tokens`、Responses `input_tokens_details.cached_tokens`、Anthropic `cache_read/creation_input_tokens`）和推理 token。顺带修了 Anthropic 的输入计数只算未缓存部分的问题（导致上下文占用偏低、压缩触发偏晚）。JSON 输出的 usage 按 TS 口径：`input_tokens` 是未缓存部分，三项相加为完整 prompt。
  - JSON 的 `changes` 字段：移植 TS 的 change-tracker，运行前记下 git 里已脏的文件，结束后报告本次新改的文件数和被改动的已有测试文件（删了几行）；不在 git 仓库时不输出该字段。
  - 测试 144 个全过（新增：登录流程/错误 state/Grok 导入、ISO 时间解析、缓存 token、change-tracker、提及补全与展开；TUI 端到端加了 `@` 补全、历史、/context、/mcp、/compact）。真实验证：pty 里用真实配置看 /context、/mcp（arxiv 19 个工具、paper 连接失败如实显示）、/skills 选择器、`@` 补全。
  - 仍未做：图片粘贴、主题切换、diff 预览、MCP prompts 与 OAuth 服务器、子代理加强（可写 worktree、429 自适应、档位路由、预算、重启续跑）、Google 原生协议与 Responses 加密推理回放、Session/Notification hooks、发布打包与 TS 退役。

- 2026-11-08（续）：第 7 批完成（未提交）：子代理加强，对齐 TS 现行行为。
  - 可写子代理：内置 `implementer` 档案（`mode: write_worktree`，用户/项目档案也可声明）。子代理在运行时分配的分离 worktree 里工作（`git worktree add --detach <临时目录> HEAD`，进入与父代理相同的子目录），父代理的工作区从不被碰。权限由代码判定（`bubble-core/src/worktree.rs`，照搬 TS 的 WorktreeApprovalController）：写/改文件必须在 worktree 内；命令里禁止 `git push` / `git remote` / `git worktree` / `sudo`，引用 worktree 外的绝对路径（/dev /tmp /usr /bin /opt /etc 除外）一律拒绝；其他需审批的工具都拒绝。结束时没有改动的 worktree 自动删除，有改动的保留，`wait_agent` 给出路径和 diff stat，由父代理审阅后自己应用。只有 worktree 检查完才标记为结束，等待的父代理能同时拿到 diff。工作流成员仍只读。委派策略提示词恢复了 TS 的"互不相干的改动组才委派写任务"条款。
  - 调度：用 Scheduler 取代固定信号量。全局最多 8 个，按类别再限（quick/explore 3、deep/review 2、frontend 1、writing 2，可用 config `agentCategories` 覆盖）。子代理在 provider 自身重试用完后仍遇 429 时：容量降为当前运行数（之后每 2 秒最多再降 1，最低 1），该子代理带着上下文排队，3/6/12 秒后重试（最多 3 次）；安静 3 分钟后容量加 1，直到恢复 8。
  - 档位路由：从 TS 模型目录生成 85 个模型的 fast/balanced/strong 档位表（ChatGPT 订阅即 `openai`）。`spawn_agent` 和工作流 `agent()` 的 `category` 现在真正生效：quick/explore 选父代理同一 provider 下更快一档的模型并用 low 档位，deep/review 沿用父模型并用 high 档位；只往下路由、父模型档位未知时不动（同 TS 的 rank guard）。优先级：调用指定 > 档案 > 类别 > 继承。
  - 预算：按 TS 2026-07-06 的决定只做记账、不设上限（子代理只因任务完成而停止）。每个子代理统计 token（`wait_agent` 显示），工作流的 `budget.spent()` 返回成员已用 token，`total` 为 null。
  - 重启续跑：任务的子代理记录（快照、历史、模型、档位等）存到 `~/.bubble/native/subagents/<任务id>/agent_N.json`，每轮结束和状态变化时写入。应用或 CLI 重新打开该任务时恢复：已结束的照旧，运行中被切断的标为失败（"Interrupted when Bubble closed"），`send_input` 带着完整上下文继续；新 id 接着编号。桌面应用打开任务即显示这些子代理，可写子代理的展开项显示保留的 worktree 和 diff。
  - 测试 150 个全过（新增：worktree 创建/清理/策略、档位路由、调度器类别上限与 429 降容、磁盘恢复、端到端 implementer：写进 worktree、push 被代码拒绝、项目不变、记录落盘）。真实验证（Kimi K3）：让 implementer 给 calc.py 加 multiply 和测试，它在 worktree 里改了两个文件并跑通测试，父代理报告了 worktree 路径和 diff stat，项目目录未变；我另行在 worktree 里复核 diff 和测试后删除了它。
  - 未做：子代理运行中被关闭/取消时不检查 worktree（保留并显示路径，不给 diff stat）；TS 的启动速率间隔（burst 4 + 500ms）没移植；路由菜单未按 TS 的完整格式放进工具描述（只给类别说明，`/context` 不显示）。

- 2026-11-09：第 8 批完成（未提交）：零散缺口。
  - Hooks：`SessionStart`（会话打开，payload 含 resumed / printMode / provider / model）和 `SessionEnd`（各模式退出时）同 TS；`Notification` TS 声明了但从未触发，这里在 TUI 和行模式弹出审批时触发（"Bubble needs your approval: …"），可用来接桌面通知。
  - 浅色主题：启动时问终端背景色（OSC 11，后跟一个 DA 查询，不支持 OSC 11 的终端立即结束等待），再看 `COLORFGBG`，默认深色，同 TS；结果作为 NSH 的颜色方案传给引擎，TUI 的颜色都改成 NSH 的 AdaptiveColor 明暗成对值，由 NSH 自己按方案取色。`/theme auto|light|dark` 即时切换（`set_color_scheme`）并写回 config.json 的 `theme`（与 TS 同字段）。
  - diff 预览：成功的 `edit` 下方显示改动（LCS 行 diff，变化前后各留 2 行上下文，删除红、添加绿，"+N added, -M removed"），折叠 20 行，Ctrl-O 展开，同 TS；恢复会话时也显示。
  - 图片：核心的用户消息加了 `images`，三种协议各按自己的格式发送（OpenAI `image_url` data URL、Anthropic base64 image block、Responses `input_image`），压缩估算按每张约 1600 token。TUI 里 Ctrl+V 读剪贴板图片（macOS osascript，Linux wl-paste / xclip），在输入框插入 `[image#1.png]`；粘贴或拖入的图片文件路径在发送时读入并替换成标签，同 TS；超过约 3.7MB 的图片拒绝（未做 TS 的自动缩放）。
  - MCP prompts：连接时读 `prompts/list`，`/提示名 参数` 按位置（支持引号）填参数、`prompts/get` 取回、按 TS 规则拼成一条用户消息发给代理；命令提示里也列出 MCP prompts 和技能。`/mcp` 状态显示 prompts 数量。
  - MCP OAuth（TS 没有）：HTTP 服务器回 401 时记下 `WWW-Authenticate`；`/mcp login 名字`（桌面 Settings 里是 Sign in 按钮）按 MCP 授权规范走：受保护资源元数据 → 授权服务器元数据（RFC 8414 路径规则，兼容 openid-configuration 和旧规范的默认地址）→ 动态客户端注册 → 浏览器 PKCE 登录（带 `resource` 参数）→ token 存 `~/.bubble/mcp-auth.json`（0600），之后请求自动带 Bearer，快过期时用 refresh token 刷新；`/mcp logout 名字` 删除。用 Python 假服务器测了整套流程。
  - Google 原生协议：config 里 protocol 为 `ai-sdk`（TS 的叫法）、`gemini` 或 `google` 时走 `models/{model}:streamGenerateContent?alt=sse`：系统指令、函数声明（JSON schema 去掉 Gemini 不收的字段）、图片 inlineData、按调用名回传函数结果、Gemini 3 用 thinkingLevel / 2.5 用 thinkingBudget 并取回思考摘要、用量含缓存与思考 token。以前 Rust 版遇到 `ai-sdk` 直接报不支持。
  - 推理回放：助手消息加了 provider 状态字段，按"同一 provider + 同一模型"才回放：Anthropic 的 thinking 块及签名（含 redacted_thinking，未签名的不回放）放在内容最前；Responses（OpenAI 方言请求加 `include: reasoning.encrypted_content`，Codex 原本就请求但没回放）把加密推理项放回对应调用之前；Gemini 原样回放模型的 parts 及 thoughtSignature。顺带修了 Rust 版 Anthropic 开思考 + 工具调用时从不回传 thinking 块的问题（真 Anthropic API 会因此拒绝请求）。
  - 其他修复：TUI 里完整输入的命令（如 `/mcp`）直接执行，不再被同前缀的技能提示抢走；GitHub 测试在冷启动时先等首屏再点击（之前偶发失败的原因）。
  - 测试 163 个全过。真实验证：Gemini 2.5 Flash 和 Gemini 3.1 Pro（原生协议，后者一轮并行两次调用并回放签名）、Claude Sonnet 4.6（开 high 思考，三轮工具调用）、ChatGPT gpt-6.1-sol（Responses，带缓存命中 19712 token）都完成了"ls + 读文件 + 回答"的工具任务。
  - Anthropic prompt cache（发现缺失后补上）：官方 Anthropic 才加断点，同 TS：系统提示、最后一个工具、消息里两处（最新一条写入，上一次请求结束处读回），最多 4 个，`BUBBLE_ANTHROPIC_MESSAGE_CACHE=0` 关掉消息部分。同一真实任务：之前 40558 输入 token 全按新输入计费，之后只有 5 个未缓存，26970 读缓存、13583 写缓存。
  - 未做：桌面应用的输入框还不能粘贴图片；图片不自动缩放。

- 2026-11-09：第 9 批完成（未提交）：发布与退役 TS 的准备。用户定：旧会话全部导入；npm 同名包按平台分发；飞书继续缓。
  - 旧会话导入：`bubble-store/src/import.rs` 读 TS 的 `~/.bubble/sessions/<目录>/<时间>.jsonl`（metadata 的标题/目录/模型，user_message 含图片，assistant_message 及按 id 挂上的 tool_call，tool_result 合并成一步，summary 变成检查点 = 摘要 + 进行中的那一轮，旧格式 message；marker / todos / provider_error 略过；没有用户消息的文件跳过）。每个会话成为该目录下已完成（done）的 CLI 任务，保留原始时间，原文件不动；每个文件记一次，重复导入无效；导入的任务带标记，记忆 phase 1 跳过（TS 记忆已读过）。`bubble import-sessions` 手动执行，TUI 和桌面应用首次启动时在后台自动执行一次。真实导入：先备份了 bubble.db，1249 个会话导入、493 个空会话跳过、0 失败，用时 4 秒；`/session` 能看到并续接。桌面应用只在已是项目的文件夹里显示导入的会话（否则侧栏会多出 99 个文件夹分组），其余在 CLI 的 `/session` 里。
  - npm 打包（未发布）：`native/npm/`：主包 `@bubblebrain-ai/bubble`（optionalDependencies 列出 5 个平台包，`bin/bubble.js` 启动器找到本机平台的二进制运行，转发 SIGTERM/SIGHUP、Ctrl+C 交给终端、退出码原样返回）；`build.mjs --version x.y.z [--target …] [--pack]` 构建并组装平台包 `@bubblebrain-ai/bubble-{darwin-arm64,darwin-x64,linux-x64,linux-arm64,win32-x64}`，版本号通过 `BUBBLE_VERSION` 编进二进制（`--version`、TUI 欢迎区都用它）。`.github/workflows/native-release.yml`：手动触发，5 个平台各自构建、校验 `--version`、上传产物；只有勾选 publish 才用 `NPM_TOKEN` 先发平台包再发主包。本机验证：打出 darwin-arm64 的两个 tgz，装进临时 prefix，`bubble --version` 为 1.0.0-native.0、`-p` 的退出码正确、全屏 TUI 经启动器正常运行并退出。Linux/Windows 未在本机编译（缺交叉 C 工具链），依赖树里没有 GTK/X11 之类的桌面库，首次跑 CI 时才会真正验证。
  - `bubble update [--check]`：同 TS：查 npm 的 latest，按二进制真实路径判断安装方式（npm / bun / pnpm / yarn 用对应命令全局安装；Homebrew 和源码构建给出提示），`--check` 只报告。
  - 测试 166 个全过。

## 修订记录

- v1（2026-11-08）：初稿。
- v1.1（2026-11-08）：补进度。工具链要求 Rust ≥ 1.99（nsh-core 0.4 用到新标准库特性）。
