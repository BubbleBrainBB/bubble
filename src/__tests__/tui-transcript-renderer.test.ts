/**
 * Transcript renderer tests (Phase 5): pin the row contracts the legacy
 * message-list suites guarded — user card padding, wrap width, reasoning
 * collapse, tool glyph states, error visibility.
 */
import { describe, expect, it } from "vitest";
import {
  renderMessage,
  projectTranscript,
  renderToolTrace,
  renderTranscript,
  renderUserCard,
  TRANSCRIPT_RAIL_COLUMNS,
  wrapPlain,
} from "../tui/components/transcript.js";
import { ResponsiveTranscriptComponent } from "../tui/components/responsive-transcript.js";
import { formatWelcomeModel, renderWelcomeBanner } from "../tui/components/welcome.js";
import type { DisplayMessage } from "../tui/model/display-history.js";
import { TraceInteractionState } from "../tui/model/trace-interaction.js";
import chalk from "chalk";
import stringWidth from "string-width";

chalk.level = 0; // strip ANSI for assertions

const strip = (text: string): string => text.replace(/\u001b\[[0-9;]*m/g, "");
/** Empty rail: prose, notices and errors start at the shared body column. */
const RAIL = " ".repeat(TRANSCRIPT_RAIL_COLUMNS);

const msg = (partial: Partial<DisplayMessage>): DisplayMessage =>
  ({ key: "k", role: "assistant", content: "", ...partial }) as DisplayMessage;

const defaultMarkedTheme = {
  userBg: (text: string) => text,
  userText: (text: string) => text,
  accent: (text: string) => text,
  dim: (text: string) => text,
  error: (text: string) => text,
  success: (text: string) => text,
};

describe("transcript renderer", () => {
  it("renders a compact user card with exact terminal-cell width", () => {
    const rows = renderUserCard("你好啊", { columns: 40 });
    expect(rows).toHaveLength(3); // symmetric top/body/bottom padding
    expect(rows[1]).toContain("›");
    expect(rows[1]).toContain("你好啊");
    expect(rows[0].trim()).toBe("");
    expect(rows[2].trim()).toBe("");
    expect(rows.every((row) => stringWidth(strip(row)) === 38)).toBe(true);
  });

  it("wraps CJK by terminal cells without overflowing the painted edge", () => {
    const rows = renderUserCard("看下这个项目现在到底是在干什么以及还剩下哪些工作需要继续完成", { columns: 24 });
    expect(rows.length).toBeGreaterThan(2);
    expect(rows.every((row) => stringWidth(strip(row)) === 22)).toBe(true);
    expect(rows.at(-1)?.trim()).toBe("");
  });

  it("reprojects the same transcript from message state at the current width", () => {
    const messages = [msg({ key: "u", role: "user", content: "你好，resize should reflow this message" })];
    const component = new ResponsiveTranscriptComponent(() => ({ messages }));

    const narrow = component.render(24);
    const wide = component.render(60);

    expect(narrow.length).toBeGreaterThan(wide.length);
    expect(narrow.at(-1)).toBe("");
    expect(wide.at(-1)).toBe("");
    expect(narrow.slice(0, -1).every((row) => stringWidth(strip(row)) === 22)).toBe(true);
    expect(wide.slice(0, -1).every((row) => stringWidth(strip(row)) === 58)).toBe(true);
    expect(narrow.map(strip).join("\n")).toContain("resize");
    expect(wide.map(strip).join("\n")).toContain("resize should reflow this message");
  });

  it("wraps long user text within the card and aligns continuation lines", () => {
    const rows = renderUserCard("word ".repeat(20).trim(), { columns: 30 });
    expect(rows.length).toBeGreaterThan(4);
    for (const row of rows) {
      expect(stringWidth(strip(row))).toBe(28);
    }
  });

  it("keeps every settled row within the real terminal width down to one column", () => {
    const messages: DisplayMessage[] = [
      msg({ key: "u", role: "user", content: "缩窗以后这条很长的用户消息仍然不能越界" }),
      msg({
        key: "a",
        reasoning: "reasoning remains visible while the final answer settles",
        content: "先检查最终回答",
        toolCalls: [{ id: "read", name: "read", args: { path: "a/very/long/path/README.md" }, result: "ok", status: "completed" }],
        parts: [
          { type: "text", content: "先检查" },
          { type: "tools", toolCalls: [{ id: "read", name: "read", args: { path: "a/very/long/path/README.md" }, result: "ok", status: "completed" }] },
          { type: "text", content: "最终回答" },
        ],
      }),
      msg({ key: "notice", content: "普通通知也必须在窄终端内换行", syntheticKind: "ui_notice" }),
      msg({ key: "error", role: "error", content: "错误消息也不能越过终端宽度" }),
    ];

    for (const columns of [1, 4, 12, 20, 40, 80]) {
      const rows = renderTranscript(messages, { columns });
      expect(rows.every((row) => stringWidth(strip(row)) <= columns), `overflow at ${columns} columns`).toBe(true);
    }
  });

  it("renders assistant text wrapped to the terminal width", () => {
    const rows = renderMessage(msg({ content: "a".repeat(90) }), { columns: 40 });
    const text = rows.map((r) => strip(r));
    expect(text.some((line) => line.length <= 38 && line.startsWith(`${RAIL}a`))).toBe(true);
    expect(rows[rows.length - 1]).toBe("");
  });

  it("keeps Compact completion terse until its trace-style detail is expanded", () => {
    const interaction = new TraceInteractionState();
    const message = msg({
      key: "compact-1",
      content: "Compaction completed in 1.2s.",
      syntheticKind: "ui_compact_summary",
      compactionSummary: "Goal: preserve authentication behavior\nNext: run the full test suite",
    });
    const options = { columns: 72, traceInteraction: interaction };

    const collapsed = renderMessage(message, options).map(strip).join("\n");
    expect(collapsed).toContain("◆ Compaction completed in 1.2s.");
    expect(collapsed).not.toContain("preserve authentication behavior");

    const groupKey = "compact:compact-1";
    interaction.activate({ kind: "group", key: groupKey, groupKey, foldable: true }, 2);
    const expanded = renderMessage(message, options).map(strip).join("\n");
    expect(expanded).toContain("⌄ Compaction completed in 1.2s.");
    expect(expanded).toContain("Goal: preserve authentication behavior");
    expect(expanded).toContain("Next: run the full test suite");
  });

  it("renders the Grok-style settled reasoning surface", () => {
    const collapsed = renderMessage(msg({ reasoning: "line one\nline two" }), { columns: 60 });
    expect(collapsed.map(strip)).toEqual(["┃  ◆ Thinking", "┃  line one", "┃  line two", ""]);

    const expanded = renderMessage(msg({ reasoning: "line one" }), { columns: 60, showReasoning: true });
    expect(expanded.map(strip)).toEqual(["┃  ◆ Thinking", "┃  line one", ""]);
  });

  it("starts user text, reasoning, tool entries and the answer on the same rail column", () => {
    const rows = renderTranscript(
      [
        msg({ role: "user", content: "go" }),
        msg({
          reasoning: "plan",
          content: "Final answer.",
          toolCalls: [{ id: "t", name: "bash", args: { command: "npm test" }, result: "ok" }],
        }),
      ],
      { columns: 60 },
    ).map(strip);
    const bodyColumn = (row: string): number => stringWidth(row) - stringWidth(row.replace(/^[\s›┃│]+/u, ""));
    const find = (needle: string) => rows.find((row) => row.includes(needle))!;
    for (const needle of ["go", "◆ Thinking", "plan", "◆ Execute", "Final answer."]) {
      expect(bodyColumn(find(needle)), needle).toBe(TRANSCRIPT_RAIL_COLUMNS);
    }
  });

  it("keeps long collapsed reasoning within every terminal width", () => {
    const reasoning = "We need answer user asks in Chinese: 你是啥模型. ".repeat(20);
    for (const columns of [24, 40, 80, 120, 256]) {
      const rows = renderMessage(msg({ reasoning }), { columns });
      expect(stringWidth(strip(rows[0]!))).toBeLessThanOrEqual(columns);
    }
    const wide = renderMessage(msg({ reasoning }), { columns: 256 }).map(strip).join("\n");
    expect(wide).toContain("Thinking");
    expect(wide).not.toContain("└─");
  });

  it("uses Grok's shared diamond marker for every tool state", () => {
    const opts = { columns: 60 };
    const pending = renderToolTrace({ id: "1", name: "bash", args: { command: "ls" } }, opts);
    const ok = renderToolTrace({ id: "1", name: "bash", args: { command: "ls" }, result: "done" }, opts);
    const err = renderToolTrace({ id: "1", name: "bash", args: { command: "ls" }, result: "boom", isError: true }, opts);

    expect(strip(pending)).toContain("◆ bash");
    expect(strip(ok)).toContain("◆ bash");
    expect(strip(err)).toContain("◆ bash");
    expect([pending, ok, err].every((row) => !/[●✔✗]/u.test(strip(row)))).toBe(true);
    // Command preview is surfaced.
    expect(strip(ok)).toContain("ls");
  });

  it("colors a failed tool's diamond and label as one error entry", () => {
    const row = renderToolTrace(
      { id: "1", name: "bash", args: { command: "false" }, result: "boom", isError: true },
      {
        columns: 60,
        theme: { ...defaultMarkedTheme, error: (text) => `<error>${text}</error>` },
      },
    );
    expect(row).toContain("<error>◆ bash false</error>");
  });

  it("verbose trace appends the result preview", () => {
    const row = renderToolTrace({ id: "1", name: "bash", args: {}, result: "first line\nsecond" }, { columns: 60, verboseTrace: true });
    expect(strip(row)).toContain("first line");
    expect(strip(row)).not.toContain("second");
  });

  it("keeps successful items and error details in a partially failed group", () => {
    const rows = renderMessage(msg({
      toolCalls: [
        { id: "ok", name: "read", args: { path: "good.ts" }, result: "ok", status: "completed" },
        { id: "bad", name: "read", args: { path: "bad.ts" }, result: "permission denied", isError: true, status: "failed" },
      ],
    }), { columns: 80 }).map(strip);
    const text = rows.join("\n");
    expect(text).toContain("good.ts");
    expect(text).toContain("bad.ts");
    expect(text).toContain("permission denied");
    expect(text).toContain("1 error");
  });

  it("uses Grok-style independent folds for a Read group and its members", () => {
    const interaction = new TraceInteractionState();
    const messages = [msg({
      toolCalls: [
        { id: "read-a", name: "read", args: { path: "a.ts" }, result: "line a", status: "completed" },
        { id: "read-b", name: "read", args: { path: "b.ts" }, result: "line b", status: "completed" },
      ],
    })];
    const render = () => projectTranscript(messages, { columns: 80, traceInteraction: interaction });

    let projection = render();
    expect(projection.rows.map(strip).join("\n")).toContain("◆ Read 2 files");
    expect(projection.rows.map(strip).join("\n")).not.toContain("a.ts");
    const group = projection.traceTargets.find((target) => target?.kind === "group");
    expect(group?.kind).toBe("group");

    interaction.activate(group!, 1);
    expect(render().rows.map(strip).join("\n")).toContain("› Read 2 files");
    interaction.activate(group!, 2);
    projection = render();
    const expanded = projection.rows.map(strip).join("\n");
    expect(expanded).toContain("⌄ Read 2 files");
    expect(expanded).toContain("a.ts");
    expect(expanded).toContain("b.ts");
    expect(expanded).not.toContain("line a");

    const item = projection.traceTargets.find((target) => target?.kind === "item" && target.toolId === "read-a");
    expect(item?.kind).toBe("item");
    interaction.activate(item!, 1);
    interaction.activate(item!, 2);
    expect(render().rows.map(strip).join("\n")).toContain("line a");
  });

  it("reports a failed subagent once on its launch row; wait echoes add no second row", () => {
    const failed = {
      kind: "subagent" as const,
      mode: "lifecycle",
      subagents: [{ subAgentId: "child_9", agentName: "explorer", nickname: "Jean", status: "failed", error: "Provider rejected a request parameter." }],
    };
    const rows = renderTranscript([
      msg({ role: "user", content: "看下这个项目是在干嘛的" }),
      msg({
        key: "a1",
        parts: [{ type: "tools", toolCalls: [{ id: "spawn", name: "spawn_agent", args: { message: "inspect" }, result: "Spawned Jean", status: "completed", metadata: failed }] }],
      }),
      msg({
        key: "a2",
        parts: [{ type: "tools", toolCalls: [
          { id: "wait", name: "wait_agent", args: { agent_id: "child_9" }, result: "wait_agent: 1 subagent", status: "completed", metadata: failed },
          { id: "read-1", name: "read", args: { path: "README.md" }, result: "# readme", status: "completed" },
        ] }],
      }),
    ], { columns: 80 }).map(strip);
    const text = rows.join("\n");
    expect(text).toContain("◆ Subagent Jean 1 error");
    expect(text.match(/1 error/g)).toHaveLength(1);
    // The wait echo never renders; the later Read row follows the launch directly.
    expect(text).not.toContain("Wait");
    expect(text.indexOf("Subagent Jean")).toBeLessThan(text.indexOf("◆ Read"));
  });

  it("renders a completed subagent launch as an inspector action instead of a stale running fold", () => {
    const interaction = new TraceInteractionState();
    const projection = projectTranscript([msg({
      toolCalls: [{
        id: "spawn",
        name: "spawn_agent",
        args: { message: "review" },
        result: "completed",
        status: "completed",
        metadata: {
          kind: "subagent",
          subagents: [{ subAgentId: "child-1", nickname: "Karen", status: "completed", summary: "full review" }],
        },
      }],
    })], { columns: 80, traceInteraction: interaction });

    expect(projection.rows.map(strip).join("\n")).toContain("Subagent Karen completed");
    expect(projection.traceTargets.find((target) => target?.action)).toMatchObject({
      foldable: false,
      action: { kind: "open-subagent", subAgentId: "child-1" },
    });
  });

  it("renders background task lifecycle rows as task-inspector actions", () => {
    const interaction = new TraceInteractionState();
    const projection = projectTranscript([msg({
      toolCalls: [{
        id: "task-lifecycle:task_0001:3000",
        name: "bash",
        args: { command: "npm test", description: "Run tests" },
        status: "completed",
        startedAt: 1_000,
        metadata: {
          kind: "shell",
          background: true,
          taskId: "task_0001",
          taskLifecycle: "completed",
          endedAt: 3_000,
          exitCode: 0,
          outputLines: 4,
        },
      }],
    })], { columns: 100, traceInteraction: interaction });

    expect(projection.rows.map(strip).join("\n")).toContain("Task completed Run tests task_0001 · in 2s · exit 0 · 4 lines");
    expect(projection.traceTargets.find((target) => target?.action)).toMatchObject({
      action: { kind: "open-task", taskId: "task_0001" },
    });
  });

  it("renders Grok's dim hover border and brighter selected border without reflow", () => {
    const interaction = new TraceInteractionState();
    const messages = [msg({
      toolCalls: [
        { id: "read-a", name: "read", args: { path: "a.ts" }, result: "line a", status: "completed" },
        { id: "read-b", name: "read", args: { path: "b.ts" }, result: "line b", status: "completed" },
      ],
    })];
    const render = () => projectTranscript(messages, { columns: 40, traceInteraction: interaction });
    const idle = render();
    const target = idle.traceTargets.find((candidate) => candidate?.kind === "group");
    expect(target?.kind).toBe("group");
    const idleRows = idle.rows.map(strip);
    const idleHeader = idleRows.findIndex((row) => row.includes("◆ Read 2 files"));
    expect(idleRows[idleHeader - 1]).toBe("");
    expect(idleRows[idleHeader + 1]).toBe("");

    interaction.hover(target);
    const hoveredRows = render().rows.map(strip);
    const hoveredHeader = hoveredRows.findIndex((row) => row.includes("◆ Read 2 files"));
    expect(hoveredRows).toHaveLength(idleRows.length);
    expect(hoveredRows[hoveredHeader - 1]).toMatch(/^┌ +┐$/u);
    expect(hoveredRows[hoveredHeader]).toMatch(/^│  ◆ Read 2 files +│$/u);
    expect(hoveredRows[hoveredHeader + 1]).toMatch(/^└ +┘$/u);
    expect(stringWidth(hoveredRows[hoveredHeader - 1]!)).toBe(38);
    expect(stringWidth(hoveredRows[hoveredHeader]!)).toBe(38);

    interaction.activate(target!, 1);
    const selectedRows = render().rows.map(strip);
    expect(selectedRows.join("\n")).toContain("› Read 2 files");
    expect(selectedRows[selectedRows.findIndex((row) => row.includes("› Read 2 files")) - 1]).toMatch(/^┌ +┐$/u);
    expect(selectedRows.some((row) => row.includes("│  › Read 2 files"))).toBe(true);
  });

  it("paints the full trace interior on hover and keeps a stronger surface when selected", () => {
    const previousLevel = chalk.level;
    chalk.level = 3;
    try {
      const interaction = new TraceInteractionState();
      const messages = [msg({
        toolCalls: [{
          id: "execute-hover",
          name: "bash",
          args: { command: "npm test", description: "run tests" },
          result: "passed",
          status: "completed",
        }],
      })];
      const render = () => projectTranscript(messages, { columns: 40, traceInteraction: interaction });
      const idle = render();
      const target = idle.traceTargets.find((candidate) => candidate?.kind === "group");
      expect(target?.kind).toBe("group");

      interaction.hover(target);
      const hovered = render().rows.find((row) => strip(row).includes("◆ Execute run tests"));
      expect(hovered).toContain("\x1b[48;2;35;35;35m");
      expect(stringWidth(strip(hovered!))).toBe(38);

      interaction.activate(target!, 1);
      const selected = render().rows.find((row) => strip(row).includes("› Execute run tests"));
      expect(selected).toContain("\x1b[48;2;43;43;43m");
      expect(stringWidth(strip(selected!))).toBe(38);
    } finally {
      chalk.level = previousLevel;
    }
  });

  it("selects and double-click expands Find Files like Grok", () => {
    const interaction = new TraceInteractionState();
    const messages = [msg({
      toolCalls: [{
        id: "glob",
        name: "glob",
        args: { pattern: "src/**/*.ts" },
        result: "src/a.ts\nsrc/b.ts",
        status: "completed",
        metadata: { matches: 2 },
      }],
    })];
    const render = () => projectTranscript(messages, { columns: 60, traceInteraction: interaction });

    let projection = render();
    const target = projection.traceTargets.find((candidate) => candidate?.kind === "group");
    expect(target).toMatchObject({ kind: "group", foldable: true });
    expect(projection.rows.map(strip).join("\n")).toContain("◆ Find Files 2 files");
    expect(projection.rows.map(strip).join("\n")).not.toContain("src/a.ts");

    interaction.activate(target!, 1);
    expect(render().rows.map(strip).join("\n")).toContain("│  › Find Files 2 files");

    interaction.activate(target!, 2);
    projection = render();
    const expanded = projection.rows.map(strip).join("\n");
    expect(expanded).toContain("⌄ Find Files 2 files");
    expect(expanded).toContain("src/a.ts");
    expect(expanded).toContain("src/b.ts");

    interaction.activate(target!, 2);
    expect(render().rows.map(strip).join("\n")).not.toContain("src/a.ts");
  });

  it("selects and double-click expands Execute output like Grok", () => {
    const interaction = new TraceInteractionState();
    const messages = [msg({
      toolCalls: [{
        id: "execute",
        name: "bash",
        args: { command: "printf 'one\\ntwo\\n'", description: "print lines" },
        result: "one\ntwo",
        status: "completed",
      }],
    })];
    const render = () => projectTranscript(messages, { columns: 60, traceInteraction: interaction });

    let projection = render();
    const target = projection.traceTargets.find((candidate) => candidate?.kind === "group");
    expect(target).toMatchObject({ kind: "group", foldable: true });
    const collapsed = projection.rows.map(strip).join("\n");
    expect(collapsed).toContain("◆ Execute print lines");
    expect(collapsed).not.toContain("printf");
    expect(collapsed).not.toContain("one");

    interaction.activate(target!, 1);
    expect(render().rows.map(strip).join("\n")).toContain("│  › Execute print lines");

    interaction.activate(target!, 2);
    projection = render();
    const expanded = projection.rows.map(strip).join("\n");
    expect(expanded).toContain("⌄ Execute print lines");
    expect(expanded).toContain("printf 'one\\ntwo\\n'");
    expect(expanded).toContain("one");
    expect(expanded).toContain("two");
  });

  it("renders a semantic bash read as one interactive Read entry", () => {
    const interaction = new TraceInteractionState();
    const projection = projectTranscript([msg({
      toolCalls: [{
        id: "bash-read",
        name: "bash",
        args: { command: "head -30 design-qa.md" },
        result: "stdout:\nreport",
        status: "completed",
        metadata: { kind: "read", path: "design-qa.md" },
      }],
    })], { columns: 80, traceInteraction: interaction });
    const text = projection.rows.map(strip).join("\n");

    expect(text).toContain("◆ Read design-qa.md");
    expect(text).not.toContain("Execute");
    expect(projection.traceTargets.some((target) => target?.kind === "item")).toBe(true);
  });

  it("shows the real Execute command below a description while running or failed", () => {
    const running = renderMessage(msg({
      toolCalls: [{
        id: "run",
        name: "bash",
        args: { command: "pwd", description: "check cwd" },
        status: "running",
      }],
    }), { columns: 80 }).map(strip).join("\n");
    expect(running).toContain("Execute check cwd running");
    expect(running).toContain("  pwd");

    const failed = renderMessage(msg({
      toolCalls: [{
        id: "fail",
        name: "bash",
        args: { command: "pwd", description: "check cwd" },
        status: "failed",
        result: "boom",
        isError: true,
      }],
    }), { columns: 80 }).map(strip).join("\n");
    expect(failed).toContain("Execute check cwd 1 error");
    expect(failed).toContain("  pwd");
    expect(failed).toContain("boom");
  });

  it("error messages render on a single red line", () => {
    const rows = renderMessage(msg({ role: "error", content: "boom happened" }), { columns: 40 });
    expect(rows).toHaveLength(2);
    expect(strip(rows[0]!)).toBe(`${RAIL}boom happened`);
    expect(strip(rows[0]!)).not.toMatch(/^[■◆●]/u);
  });

  it("renders notices and interrupts muted without answer or status markers", () => {
    const markedTheme = {
      ...defaultMarkedTheme,
      dim: (text: string) => `<dim>${text}</dim>`,
    };
    for (const syntheticKind of ["ui_notice", "ui_interrupt"] as const) {
      const rows = renderMessage(msg({ content: "ordinary notice", syntheticKind }), {
        columns: 40,
        theme: markedTheme,
      });
      expect(rows[0]).toBe(`<dim>${RAIL}ordinary notice</dim>`);
      expect(strip(rows[0]!)).not.toMatch(/[■◆●]/u);
    }
  });

  it("orders reasoning above tool traces above content", () => {
    const rows = renderMessage(
      msg({
        reasoning: "pondering",
        content: "final answer",
        toolCalls: [{ id: "t", name: "read", args: { path: "/x" } }],
      }),
      { columns: 60 },
    );
    const text = rows.map((r) => strip(r)).join("\n");
    const reasoningAt = text.indexOf("pondering");
    const toolAt = text.indexOf("Read");
    const contentAt = text.indexOf("final answer");
    expect(reasoningAt).toBeLessThan(toolAt);
    expect(toolAt).toBeLessThan(contentAt);
  });

  it("keeps provider-turn Thinking while rendering tools as diamond entries", () => {
    const rows = renderTranscript(
      [
        msg({ key: "u", role: "user", content: "inspect" }),
        msg({ key: "a1", reasoning: "plan one", toolCalls: [{ id: "t1", name: "bash", args: { command: "ls" }, result: "ok" }] }),
        msg({ key: "a2", reasoning: "plan two", toolCalls: [{ id: "t2", name: "read", args: { path: "README.md" }, result: "ok" }] }),
        msg({ key: "a3", content: "done" }),
      ],
      { columns: 60 },
    );
    const text = rows.map(strip).join("\n");
    expect(text.match(/Thinking/g)).toHaveLength(2);
    expect(text).toContain("plan one");
    expect(text).toContain("plan two");
    expect(text).not.toContain("Working");
    expect(text.match(/◆ (Execute|Read)/g)).toHaveLength(2);
    expect(text.indexOf("Execute")).toBeLessThan(text.indexOf("Read"));
    expect(text.indexOf("Read")).toBeLessThan(text.indexOf("done"));
    const plainRows = rows.map(strip);
    const executeAt = plainRows.findIndex((row) => row.includes("◆ Execute"));
    const secondThinkingAt = plainRows.findIndex((row, index) => index > executeAt && row.includes("◆ Thinking"));
    expect(plainRows[secondThinkingAt - 1]).toBe("");
  });

  it("preserves commentary/tool order from display parts inside Working", () => {
    const rows = renderTranscript(
      [
        msg({ role: "user", content: "go" }),
        msg({
          reasoning: "plan",
          content: "I will inspect.\nResult follows.",
          toolCalls: [{ id: "t", name: "read", args: { path: "/x" }, result: "ok" }],
          parts: [
            { type: "text", content: "I will inspect." },
            { type: "tools", toolCalls: [{ id: "t", name: "read", args: { path: "/x" }, result: "ok" }] },
            { type: "text", content: "Result follows." },
          ],
        }),
      ],
      { columns: 60 },
    );
    const text = rows.map(strip).join("\n");
    expect(text.indexOf("I will inspect.")).toBeLessThan(text.indexOf("Read"));
    expect(text.indexOf("Read")).toBeLessThan(text.indexOf("Result follows."));
    const plainRows = rows.map(strip);
    const commentaryAt = plainRows.indexOf(`${RAIL}I will inspect.`);
    const toolDetailAt = plainRows.findIndex((row) => row.includes("/x"));
    const answerAt = plainRows.indexOf(`${RAIL}Result follows.`);
    expect(commentaryAt).toBeGreaterThan(0);
    expect(plainRows[commentaryAt + 1]).toBe("");
    expect(plainRows[toolDetailAt + 1]).toBe("");
    expect(answerAt).toBe(toolDetailAt + 2);
  });

  it("wrapPlain honors explicit newlines and long words", () => {
    expect(wrapPlain("ab\ncd", 10)).toEqual(["ab", "cd"]);
    const wrapped = wrapPlain("aaaaaaaa bb", 8);
    expect(wrapped[0]).toBe("aaaaaaaa");
    expect(wrapped[1]).toBe("bb");
  });

  it("empty-content assistant with tools still renders the trace", () => {
    const rows = renderTranscript([msg({ content: "", toolCalls: [{ id: "t", name: "grep", args: {} }] })], { columns: 60 });
    expect(rows.some((r) => strip(r).includes("Search"))).toBe(true);
  });
});

describe("pi-tui welcome banner", () => {
  const data = {
    cwd: "~/bb/my-coding-agent-pi-tui",
    session: "session-123.jsonl",
    model: "deepseek-v4-pro",
    provider: "deepseek",
    thinking: "max",
  };

  it("restores the boxed product banner and live session metadata", () => {
    const rows = renderWelcomeBanner(data, 100).map(strip);
    const text = rows.join("\n");
    expect(text).toContain("Welcome to Bubble!");
    expect(text).toContain("I am a cat");
    expect(text).toContain("Directory:");
    expect(text).toContain("Session:");
    expect(text).toContain("Model:");
    expect(text).toContain("Version:");
    expect(rows.every((row) => stringWidth(row) <= 100)).toBe(true);
  });

  it("never overflows tiny terminals", () => {
    for (const width of [1, 4, 12, 23, 24, 40]) {
      const rows = renderWelcomeBanner(data, width);
      expect(rows.every((row) => stringWidth(strip(row)) <= width)).toBe(true);
    }
  });

  it("formats ordinary reasoning effort as part of the model unit", () => {
    expect(formatWelcomeModel(data)).toBe("deepseek-v4-pro with max effort · deepseek");
  });

  it("shows a fixed effort grade but hides the always-thinking placeholder", () => {
    expect(formatWelcomeModel({ model: "kimi/kimi-k3.1-pro", provider: "alibaba", thinking: "max" }))
      .toBe("kimi/kimi-k3.1-pro with max effort · alibaba");
    expect(formatWelcomeModel({ model: "kimi-k2.7-code", provider: "alibaba", thinking: "medium" }))
      .toBe("kimi-k2.7-code · thinking mode · alibaba");
  });
});

describe("transcript markdown pipeline", () => {
  it("routes assistant content through the injected markdown renderer", () => {
    const calls: Array<{ text: string; width: number }> = [];
    const rows = renderMessage(msg({ content: "# Title\n\nbody" }), {
      columns: 50,
      markdownRenderer: (text, width) => {
        calls.push({ text, width });
        return ["<MD>", ...text.split("\n")];
      },
    });
    expect(calls).toEqual([{ text: "# Title\n\nbody", width: 50 - 2 - TRANSCRIPT_RAIL_COLUMNS }]);
    expect(rows[0]).toBe(`${RAIL}<MD>`);
    expect(rows[rows.length - 1]).toBe("");
  });

  it("falls back to plain wrapping without a renderer", () => {
    const rows = renderMessage(msg({ content: "plain words" }), { columns: 50 });
    expect(rows[0]).toBe(`${RAIL}plain words`);
  });

  it("keeps paragraph breaks empty instead of painting the rail", () => {
    const rows = renderMessage(msg({ content: "first\n\nsecond" }), { columns: 50 });
    expect(rows).toEqual([`${RAIL}first`, "", `${RAIL}second`, ""]);
  });
});
