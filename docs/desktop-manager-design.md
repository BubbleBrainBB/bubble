# Bubble Desktop：Manager 首页 + Task 实体 + GitHub 事件

状态：**设计稿 v1，待评审**（2026-11-08）。未开始实现。

方向来源：2026-11-08 用户拍板——桌面版改造成"人只跟 Manager 说事，Manager 建任务、派 Agent、外部事件也汇到这里并跟进到办完"的形态。已定：

- 首页是 Manager 对话（推翻 2026-11-04 的"不要主对话"；Task 仍是一等公民）。
- Manager 固定跑在 Bubble 上。
- 第一个外部来源是 GitHub Issue / PR；飞书暂缓。
- 不做 Slack 式 Channel，Project 只是上下文容器。

本文所有"现状"描述都对照过源码，行号以撰写当天为准。

---

## 0. 现状与差距

| 部件 | 现状 | 差距 |
|---|---|---|
| 任务 | 没有实体。`desktop/src/ui/utils/task-state.ts` 把每个可见 Session 当成一个任务，状态从 `status / permissionRequests / runtimeNotice / lastTurnError` 推算 | 需要真正的 Task 实体，一个 Task 挂多个 Session、多个外部链接 |
| 旧看板任务 | `BoardTask`（`desktop/src/ui/store/useBoardStore.ts:75`）有 stage、`sessionIds[]`、事件日志、PR 字段，但只存在渲染进程的 `cowork-board-storage`（zustand persist），不在 SQLite | 主进程看不到，不能作为事件挂靠点 |
| 派发 | `delegate-service.ts` 启动的执行会话是隐藏的（`hiddenFromThreads: true`），消息只镜像进父会话、不存在子会话自己名下，执行登记只在内存。Bubble 会话目前**没有** `delegate_task` 工具（只给 Claude/Codex/Kimi 接了） | Manager 派发要生成用户能点进去的可见任务，不能复用这条隐藏镜像路径 |
| 运行时 | `agent-loop.ts:26` 只允许 `bubble` provider | 执行任务的"不同 Agent"= 不同模型/配置的 Bubble 会话，不是别的 CLI |
| 给 Bubble 加工具 | SDK 的 `RunTurnOptions.hostTools`（`src/sdk/index.ts:130`）是每轮注入的进程内工具；适配器已用它注入设计工具和 widget（`bubble-sdk-adapter.ts:946`），工具构造时把 session id 烤进去 | 天然接缝。Manager 工具走同样方式 |
| 追加系统提示 | SDK 支持 `appendSystemPrompt`（`src/sdk/index.ts:156, 783`），但桌面加载器的类型镜像（`bubble-sdk-loader.ts:119-139`）没声明，适配器也没传 | 需要打通 |
| 长对话 | SDK 自动压缩上下文（`src/agent.ts` 的 `context_compaction`），适配器已渲染 `compact_boundary` | Manager 单会话可依赖它 |
| 收件箱 | 未提交的 `InboxView` / `TasksView` / `NewTaskModal` / `TaskNavigation` 已按 needs / running / failed / done 组织 | 改为读 Task 实体；语义即 Attention |
| GitHub | `pull-requests.ts` 全部经 `gh`（用户本人登录、不存 token），只有 PR 列表/详情/评论/合并，全 GitHub 搜索、不按项目、不轮询；`git-pull-requests.ts` 能从 origin 解析 owner/repo；`session-pull-requests.ts` 能把 PR 挂到会话 | 没有 Issue、没有通知、没有轮询、没有事件存储 |
| 定时 | `automation-scheduler.ts` 30s 一跳，到点开 `bypassPermissions` 会话 | 以后可改为产出事件；本期不动 |
| 存储迁移 | `session-store.ts` 用 better-sqlite3，`CREATE TABLE IF NOT EXISTS` + `ensureColumn`（:648）幂等加列，无版本号 | 新表沿用同一模式 |

---

## 1. 概念模型

```text
Event ──(规则挂靠 / 分诊)──▶ Task ──▶ Run(Session) × N
  ▲                          │
  └──── Link(issue/pr) ◀─────┘          Project = cwd，上下文容器
```

- **Task**：一件要办成的事。有标题、所属项目、生命周期状态、若干 Run、若干 Link、一条事件时间线。**Task 有没有办成**由它决定，不由某次 Session 是否结束决定。
- **Run**：Task 的一次执行，就是一个普通的可见 Bubble Session，带 `task_id`。一个 Task 可以有多个 Run（修复→按 review 改→按 CI 再改）。
- **Link**：Task 关联的外部对象，本期只有 GitHub `issue` / `pull_request`。一个 Link 只属于一个 Task。
- **Event**：外部或内部发生的一件事（PR 新评论、CI 失败、Issue 指派给我）。有去重键，处理结果是"挂到某 Task / 新建 Task / 忽略 / 待分诊"。
- **Manager**：一个特殊的 Bubble 会话，首页就是它的对话。它不亲自干活，只通过工具管理 Task。

不引入 Channel；Project 继续等于 `cwd`。

---

## 2. 数据层（主进程 SQLite）

新增表，沿用 `CREATE TABLE IF NOT EXISTS` + `ensureColumn`：

```sql
tasks (
  id TEXT PRIMARY KEY,
  title TEXT NOT NULL,
  project_cwd TEXT,                 -- 可空：不属于任何项目
  lifecycle TEXT NOT NULL,          -- 'open' | 'waiting' | 'done' | 'canceled'
  waiting_on TEXT,                  -- lifecycle='waiting' 时：'review' | 'ci' | 'reply' | ...
  origin TEXT NOT NULL,             -- 'manager' | 'user' | 'github' | 'backfill' | 'board'
  created_at INTEGER, updated_at INTEGER, closed_at INTEGER
)

task_links (
  id TEXT PRIMARY KEY,
  task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
  kind TEXT NOT NULL,               -- 'github_issue' | 'github_pr'
  repo TEXT NOT NULL,               -- 'owner/name'
  number INTEGER NOT NULL,
  snapshot TEXT,                    -- 上次轮询的 JSON 快照，用于差分
  polled_at INTEGER,
  UNIQUE (kind, repo, number)
)

events (
  id TEXT PRIMARY KEY,
  source TEXT NOT NULL,             -- 'github'
  dedupe_key TEXT NOT NULL UNIQUE,  -- 例 'gh:comment:123456' / 'gh:checks:owner/r#42:<sha>:failure'
  kind TEXT NOT NULL,               -- 'issue_assigned' | 'pr_comment' | 'pr_review' | 'checks_failed' | 'pr_merged' | ...
  repo TEXT, number INTEGER,
  actor TEXT,                       -- GitHub login
  payload TEXT NOT NULL,            -- 精简后的 JSON（正文截断）
  task_id TEXT REFERENCES tasks(id),
  disposition TEXT NOT NULL,        -- 'pending' | 'attached' | 'created' | 'ignored'
  created_at INTEGER
)

ingest_cursors (source TEXT PRIMARY KEY, cursor TEXT, updated_at INTEGER)
```

`sessions` 加列：`task_id TEXT`（Run 所属任务）、`session_role TEXT`（`NULL` | `'manager'` | `'triage'`）。

**Task 的"现在需要谁"不存库，继续推算**：`lifecycle` 是人/Manager 写的事实，`needs / running / failed` 从它最新 Run 的会话状态推出（沿用 `getTaskState`）。这样不会出现存储状态和会话状态打架。推算优先级：任一 Run 有待审批 → `needs`；任一 Run 在跑 → `running`；最新 Run 出错 → `failed`；否则看 `lifecycle`。

**回填**（首次迁移，幂等）：每个现有可见、非草稿、非 `dm` 的会话建一个 `origin='backfill'` 的 Task，1:1 挂上。这样 Tasks / Inbox 切到读 Task 后列表不变。

**旧 BoardTask**：见 §9 待决 1。

IPC：新增 `task.list / task.get / task.update / task.create / task.events` 请求，以及 `task.changed` 广播（与 `session.status` 同一 `server-event` 通道）。渲染进程新增 `useTaskStore`，Tasks / Inbox / 侧栏改读它。

---

## 3. Manager

### 3.1 身份与运行位置

- 一个 `session_role='manager'` 的 Bubble 会话，全局唯一（按需创建、可"重开"成新会话，旧的归档）。
- 不在任何项目里运行：沿用已有的 `userData/direct-message-runtime` 目录（`ipc-handlers.ts:366`，原 `dm` 作用域的遗留运行目录），避免 Manager 的内置文件/命令工具碰到项目代码。
- 从 Tasks / Inbox / 项目列表里排除（同 `hiddenFromThreads` 的过滤点）。
- 权限模式固定 `default`。

### 3.2 工具（`hostTools`，构造时烤入 Manager 会话 id）

| 工具 | 只读 | 作用 |
|---|---|---|
| `list_tasks(filter)` | 是 | 按项目/状态列任务，返回 id、标题、推算状态、最新 Run 一句话摘要、Links |
| `get_task(id)` | 是 | 单个任务详情 + 最近事件 + 最新 Run 末尾结论 |
| `list_projects()` | 是 | 已知项目（cwd、名称、GitHub repo） |
| `create_task({title, project, brief, model?})` | 否* | 建 Task 并立即启动第一个 Run；返回 task id |
| `continue_task({id, message})` | 否* | 给任务最新 Run 发一条跟进消息；Run 已结束则在同会话续跑 |
| `start_run({id, brief, model?})` | 否* | 为任务新开一个 Run（换模型或换思路时） |
| `update_task({id, lifecycle?, title?, project?})` | 否* | 改状态/标题/归属 |
| `link_github({id, repo, kind, number})` | 否* | 把 Issue/PR 挂到任务 |

\* 这些工具只改本地状态、不对外，标为 `readOnly: true` 以免每次都弹审批（SDK 对非只读 host tool 一律走 `gateMcpTools` 审批，`src/sdk/index.ts:753`、`src/mcp/manager.ts:354`）。副作用：`readOnly` 在 plan 模式下也放行；Manager 不用 plan 模式，可接受。对外动作见 §5。

### 3.3 系统提示（`appendSystemPrompt`）

每轮注入两段：

1. 固定的角色说明：你是调度者；超过一两句能回答的事都建任务交出去；不要自己改项目文件；建任务前先 `list_tasks` 看是否已有同一件事；brief 要自包含（目标、项目、验收标准、已知线索）。
2. 动态"现况摘要"：进行中任务（≤20 条，一行一条）+ 上次用户发言以来新到、未读的事件计数。数据从 DB 现算，不依赖 Manager 记忆。

需要改的地方：`bubble-sdk-loader.ts` 的类型镜像补 `appendSystemPrompt`；`ProviderSessionStartInput` / 适配器的 `runTurnLoop` 按 `session_role` 拼装 `hostTools` 和 `appendSystemPrompt`。

### 3.4 长对话

单会话 + SDK 自动压缩。任务真相在 DB，不在对话里，压缩丢细节也不影响调度。若实测压缩后质量下降，再加"跨天自动开新 Manager 会话、首屏带昨日摘要"，本期不做。

### 3.5 Manager 不被事件唤醒

外部事件**不**往 Manager 对话里自动插一轮模型调用：一是费钱，二是会和用户正在打的字交错。事件的展示由原生 UI 负责（§6.2 的"离开期间"行和 Inbox），Manager 只在用户下一次发言时通过现况摘要知道发生了什么。需要模型判断的事件交给独立的分诊会话（§4.4）。

---

## 4. GitHub 接入

### 4.1 监视范围

- 只监视**项目对应的仓库**：对每个项目 cwd 用 `git-pull-requests.ts` 已有的 origin 解析得到 `owner/repo`；项目设置里加一个"监视 GitHub"开关，默认关。
- 不扫用户名下全部仓库。

### 4.2 两个轮询器（主进程，`gh` 执行，沿用 `runGh` 的错误分类）

**A. 发现轮询**——找新工作：

- `gh api notifications`（带 `since` 游标与 `If-Modified-Since`；304 不计入限流），按 `X-Poll-Interval` 节奏（通常 60s），桌面窗口失焦时放缓到 5 分钟。
- 只保留仓库在监视集合内、`reason ∈ {assign, review_requested, mention, author, comment, state_change}` 的条目。
- 每条映射成一个 Event，去重键用 `thread id + updated_at`。
- 前提待实测：通知接口要求经典 token 带 `notifications` 或 `repo` 权限，细粒度 token 不支持。`gh auth login` 默认权限含 `repo`，但 M4 开工前要用真实账号跑一次 `gh api notifications` 确认；不可用时退化为按监视仓库轮询 `gh search issues/prs --assignee @me / --review-requested @me`（PR 列表已经在用同类命令）。

**B. 订阅轮询**——跟进已挂靠的对象：

- 对每个所属任务未关闭的 `task_links` 行，每 2 分钟（失焦 10 分钟）拉一次：
  - PR：`gh pr view N --repo R --json state,mergedAt,headRefOid,reviewDecision,reviews,comments,statusCheckRollup`
  - Issue：`gh issue view N --repo R --json state,comments,assignees`
- 与 `snapshot` 差分，产出细粒度 Event：新评论（键=comment id）、新 review（键=review id）、CI 结论变化（键=head sha + 结论）、merged / closed / reopened。
- 通知轮询里命中已有 Link 的条目直接丢弃，以订阅轮询的细粒度事件为准，避免一件事两条事件。

全部 `gh` 调用经一个可注入的 runner，测试用假 runner 回放 JSON。

### 4.3 事件路由：规则优先，模型兜底

1. **命中已有 Link** → 挂到该 Task（`disposition='attached'`），再按 §4.5 决定动作。不调模型。
2. **作者是自己**（`actor == gh 当前登录`）→ 记录、挂靠，但不触发任何动作（自己在网页上的操作，以及本应用代你发出的评论，都不能反过来触发 Agent）。
3. **机器人账号**（`[bot]` 后缀）且不是 CI 结论 → `ignored`。
4. 其余（新 Issue 指派、新 review 请求、被 @）→ `pending`，交分诊。

### 4.4 分诊会话

- 每批 pending 事件（防抖 30s）启动一个 `session_role='triage'` 的隐藏 Bubble 会话，模型可配置（默认用便宜的）。
- 工具只有：`list_tasks`、`get_task`、`attach_event(event, task)`、`create_task_from_event(event, title, project, brief, start: boolean)`、`ignore_event(event, reason)`。
- `create_task_from_event` 的 `start` 是否允许为 true 由该仓库的自主级别决定（§5）；默认只建任务、不开跑，任务进 Inbox 等人点"开始"。
- 分诊会话本身不出现在任何列表里，但它的决定写进 `events` 表，可在任务时间线和"离开期间"里看到。

### 4.5 挂靠后的动作

| 事件 | 默认动作 |
|---|---|
| PR 新 review / 评论（非自己） | 任务标记需要关注；若仓库自主级别 ≥ 准备，自动对最新 Run `continue_task`，消息里附评论原文 |
| CI 失败 | 同上，附失败 check 名称与日志链接 |
| CI 全绿且 review 通过 | 任务进 Inbox"可合并"，合并永远由人点 |
| PR merged / Issue closed | `lifecycle='done'` |
| Issue 新评论（非自己） | 任务标记需要关注 |

---

## 5. 对外动作与自主级别

`gh` 用的是用户本人账号，Agent 在 GitHub 上说的话就是用户本人说的话。

**三级自主，按仓库设置，默认"准备"：**

| 级别 | 能做 |
|---|---|
| 只看 | 只建事件、不建任务、不开跑 |
| 准备（默认） | 建任务、开跑、本地改代码、跑测试、生成对外动作的草稿 |
| 执行 | 在准备基础上，对外动作免审批（合并除外） |

**对外动作走专用工具**，作为 `hostTools` 注入任务 Run：`github_comment`、`github_open_pr`、`github_push_branch`、`github_close_issue`。

- 工具只能作用于**该 Run 所属项目的仓库**（构造时从项目 origin 解析并烤入，参数里不收 repo），这样构造时就知道该仓库的自主级别。
- "准备"级别：工具 `readOnly: false`，被 SDK 的 `gateMcpTools` 拦成一张待审批卡片，自然进入 Inbox；卡片完整显示草稿正文，点允许才执行。
- "执行"级别：构造时把工具标成 `readOnly: true`，SDK 不再拦截。工具不能在内部自行跳过审批——门在 SDK 层，工具拿不到。
- 这几个工具的审批卡片**不提供"本任务内一直允许"**：适配器 `buildApprovalInput`（`bubble-sdk-adapter.ts:343`）对 MCP 类工具会给出 `sessionGrant` 选项，一次允许就会放行该任务后续所有评论；对 `github_*` 需屏蔽。
- **合并不提供工具**，只能在 Pull Requests 页手动点。

**残余风险**：任务 Run 仍有 shell，理论上能直接跑 `gh pr comment` 或 `git push`。缓解：Manager/分诊创建的 Run 一律 `default` 权限模式（禁止 `bypassPermissions`，与定时任务不同），shell 写操作照常要审批；任务 brief 指示用专用工具。是否需要在核心 `src/approval/danger.ts` 里把 `git push` / `gh ... create|comment|close|merge` 列为任务会话中的强制询问，见 §9 待决 4。

---

## 6. 界面

遵守现有视觉规则（Linear 方向、字号 ≤4 种且 ≥12px、0.5px 线、不用面包屑/中圆点等）。

### 6.1 侧栏

```text
Bubble                     [搜索] [新任务]
  Home            ← Manager 对话（新的默认落地页）
  Inbox     3     ← 即 Attention：需要你处理的
  Tasks
  Pull requests
  Automations
  Skills
Projects
  bubble-code
  ...
Active
  ● 修复任务列表不刷新        ← 最多 8 条，needs 在前、running 在后
  ● 回复 PR #42 的 review
```

- 默认落地从 `tasks` 改为 `home`（`useAppStore.ts:798` 与 rehydrate 白名单 `:2759-2765`、`useTabsStore.newTab`）。
- 名称沿用 Inbox，不改叫 Attention。

### 6.2 Home（Manager 对话）

- 复用 ChatPane，绑定 Manager 会话。
- **任务卡片**：对 `create_task` / `continue_task` / `start_run` 的工具条目，原生渲染一行卡片（参照 `AssistantWorkstream.tsx` 里 `SubagentStatusRow` 的做法：按工具条目识别、读 store 实时状态），显示状态图标、标题、项目、一句话摘要；点击打开任务。不用 show_widget iframe（那是 Agent 写的 HTML，宿主实时更新要额外打通 postMessage）。
- **"离开期间"行**：用户回到 Home 且上次发言后有新事件时，在对话末尾、输入框上方原生渲染一行汇总（"GitHub 来了 5 件事：2 件已挂到任务，1 件新建任务待开始，2 件忽略"），点开展开列表。它不是对话消息，不进 Manager 上下文。
- 输入框沿用现有 composer；Manager 会话不显示项目选择器。

### 6.3 任务页

- 头部沿用 11-04 定稿：`Tasks › 状态图标 标题 ···`、右侧分页器；标题下加 Link 标签（`Issue #12`、`PR #34`，点开去 Pull requests 页或浏览器）。
- 正文：任务所有 Run 的对话记录按时间串起来，Run 之间一条细分隔线写"第 2 次执行 · 起因：PR review"。底部输入框发给最新 Run。
- `···` 菜单：标记完成、取消、换模型重跑、挂 Issue/PR。

### 6.4 Inbox

读 Task 推算状态，分组：需要你（审批，含 GitHub 草稿）/ 失败 / 待开始（分诊建的任务）/ 已完成待查看。详情面板沿用 `ComposerPendingPermissionPanel`；GitHub 草稿卡片额外完整显示正文。

### 6.5 新建任务

`NewTaskModal` 保留，作为"不经过 Manager 直接建任务"的入口，建出的 Task `origin='user'`。

---

## 7. 分阶段

每阶段独立可合、可回退。

| 阶段 | 内容 | 用户可见变化 | 验证 |
|---|---|---|---|
| M1 Task 实体 | §2 全部：表、`sessions.task_id`、回填、IPC、`useTaskStore`；Tasks / Inbox / 任务页改读 Task；Run 分隔线 | 几乎无（列表照旧），任务页能显示多 Run | 迁移幂等测试；回填 1:1；状态推算单测（沿用 `task-model.test.ts`） |
| M2 Manager | §3 + §6.1/6.2：Manager 会话、hostTools、appendSystemPrompt 打通、Home、任务卡片、侧栏 Active | 首页变 Manager 对话 | 无头 Electron + 假模型：说一句话 → 建任务 → Run 启动 → 卡片状态跟着变 |
| M3 GitHub 订阅 | §4.2B、§4.3 规则 1-3、§4.5、§5：Link、订阅轮询、对外动作工具与审批 | 任务能挂 PR/Issue；review/CI 自动回到任务；草稿进 Inbox | 假 `gh` runner 回放：新评论只产一条事件；自己的评论不触发；草稿审批前不执行 |
| M4 GitHub 发现 + 分诊 | §4.1、§4.2A、§4.4、"离开期间"行、仓库自主级别设置 | 新 Issue 指派/被请求 review 会自动出现为任务 | 假通知流：去重、游标续传、304、分诊三种结局 |

M1 不依赖 GitHub，M3 不依赖 M2（任务页手动挂 PR 也能用），但建议按顺序做。

---

## 8. 不做 / 推后

- 飞书、邮件、日历等其他来源（Event 表结构已按多来源设计）。
- 常驻在线：桌面版关着就不轮询，重开后按游标补拉。挪到 Mac mini Host 另立设计。
- Webhook（桌面无公网地址）。
- 定时任务改造为事件来源。
- Manager 跨天轮换、个人记忆（Aegis memory 目前对 Bubble 关闭：`runner.ts:47`）。
- 多个 Manager / 按项目的 Manager。

---

## 9. 已决（2026-11-08）

1. **旧 BoardTask**：一次性导入成 Task（stage → lifecycle：backlog/todo→open 未开跑，working→open，review→waiting，done→done，canceled→canceled，`sessionIds` → Runs），导入成功后删除 BoardView 和 `cowork-board-storage`。归入 M1。
2. **Manager 模型**：Settings 里单独一项，用户自选；未设置时跟随全局默认。
3. **任务 Run 默认模型**：Settings 里单独一项，用户自选；未设置时跟随全局默认。Manager 只在用户明确要求时为某个任务换模型。
4. **审批不写死**：不在核心层加强制规则。权限交给用户选——Settings 里按仓库选自主级别（只看 / 准备 / 执行，默认准备），另有一个"任务会话中 `git push` 与 `gh` 写操作总是询问"的开关，默认开、用户可关。
5. **并发上限**：暂定 4，超出排队，任务显示"排队中"。

下一步：先出设计稿（Home、侧栏、任务页、Inbox、Settings 新增项），用户看过再开工 M1。

---

## 修订记录

- v1（2026-11-08）：初稿。依据三份源码梳理（会话存储与启动、Bubble 工具注入与派发、界面与 GitHub 代码），关键接缝已逐一核对行号。
- v1.1（2026-11-08）：自查对照源码后修正 §5——原稿写"执行级别由工具自身短路放行"不成立（审批门在 SDK `gateMcpTools`，工具内部绕不过），改为构造时按仓库自主级别决定 `readOnly`；补充屏蔽 `sessionGrant`，否则一次允许会放行整个任务后续的对外评论。
- v1.2（2026-11-08）：§9 五项由用户拍板，改为"已决"：看板导入后删除；Manager 与任务模型都进 Settings；审批不写死、由用户按仓库选级别并可关闭 push/gh 写操作询问；并发暂定 4。
