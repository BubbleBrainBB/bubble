import { stripVTControlCharacters } from "node:util";
import { afterEach, describe, expect, it, vi } from "vitest";
import {
  Editor,
  ScrollView,
  TuiAltScreen,
  TuiMainScreen,
  visibleWidth,
  VStack,
  type EditorTheme,
} from "@bubblebrain-ai/pi-tui";
import { VirtualTerminal } from "@bubblebrain-ai/pi-tui/testing";
import { StreamingMessageComponent } from "../tui/components/streaming-message.js";
import { ResponsiveTranscriptComponent } from "../tui/components/responsive-transcript.js";
import { COMPOSER_EDITOR_OPTIONS, COMPOSER_EDITOR_THEME } from "../tui/composer-style.js";
import {
  projectAssistantRows,
  renderMessage,
  renderReasoning,
  TRANSCRIPT_RAIL_COLUMNS,
  type TranscriptTheme,
} from "../tui/components/transcript.js";
import { ResponsiveFooterComponent } from "../tui/footer.js";
import { createFlushScheduler } from "../tui/run.js";
import type { DisplayMessage } from "../tui/model/display-history.js";

const strip = (value: string): string => stripVTControlCharacters(value);
/** Empty rail in front of assistant prose. */
const RAIL = " ".repeat(TRANSCRIPT_RAIL_COLUMNS);

function rendered(component: StreamingMessageComponent, width = 80): string[] {
  return [...component.render(width), ...component.activityLane.render(width)]
    .map((row) => strip(row).trimEnd());
}

afterEach(() => {
  vi.useRealTimers();
});

const plainTheme: TranscriptTheme = {
  userBg: (text) => text,
  userText: (text) => text,
  accent: (text) => text,
  dim: (text) => text,
  error: (text) => text,
  success: (text) => text,
};

const editorTheme: EditorTheme = {
  borderColor: (text) => text,
  selectList: {
    selectedPrefix: (text) => text,
    selectedText: (text) => text,
    description: (text) => text,
    scrollInfo: (text) => text,
    noMatch: (text) => text,
  },
};

describe("pi-tui working trace parity", () => {
  it("keeps live reasoning in a five-line Grok rail above the spinner", () => {
    const component = new StreamingMessageComponent();
    component.update({
      content: "",
      reasoning: "\nR1\n\nR2\nR3\nR4\nR5\nR6\nR7\n",
      tools: [],
      parts: [],
      phase: "thinking",
    }, 80);

    const rows = rendered(component);
    expect(rows).toContain("┃  ◆ Thinking…");
    expect(rows).toContain("┃  R3");
    expect(rows).toContain("┃  R7");
    expect(rows).not.toContain("┃  R1");
    expect(rows).not.toContain("┃  R2");
    expect(rows.indexOf("┃  ◆ Thinking…")).toBeLessThan(rows.findIndex((row) => row.includes("working through the request")));
  });

  it("counts a file being written toward the received tokens", () => {
    const component = new StreamingMessageComponent();
    const write = (rawArguments: string) => {
      const tool = { id: "w1", name: "write", args: { path: "engine.cpp" }, status: "pending" as const, rawArguments };
      component.update({
        content: "",
        reasoning: "x".repeat(400),
        tools: [tool],
        parts: [{ type: "tools", toolCalls: [tool] }],
        phase: "working",
      }, 80);
      return rendered(component).at(-1);
    };

    // 400 chars of reasoning, then the tool call's arguments as they stream in.
    expect(write("")).toContain("writing changes (↓100 tok)");
    expect(write("y".repeat(4_000))).toContain("writing changes (↓1.1k tok)");
    expect(write("y".repeat(40_000))).toContain("writing changes (↓10k tok)");
  });

  it("keeps the status cadence while Grok entries render above it", () => {
    const component = new StreamingMessageComponent();

    component.update({ content: "", reasoning: "", tools: [], parts: [], phase: "thinking" }, 80);
    expect(rendered(component).at(-1)).toContain("mapping the workspace");

    component.update({
      content: "answer",
      reasoning: "thought",
      tools: [],
      parts: [{ type: "text", content: "answer" }],
      phase: "thinking",
    }, 80);
    let rows = rendered(component);
    expect(rows).toContain("┃  ◆ Thinking…");
    expect(rows).toContain("┃  thought");
    expect(rows.some((row) => row.includes("writing the response"))).toBe(true);
    expect(rows).toContain(`${RAIL}answer`);
    expect(rows.join("\n")).not.toContain("● answer");
    expect(rows.indexOf(`${RAIL}answer`)).toBeLessThan(rows.findIndex((row) => row.includes("writing the response")));

    const read = { id: "read-1", name: "read", args: { path: "README.md" }, status: "running" as const };
    component.update({
      content: "answer",
      reasoning: "thought",
      tools: [read],
      parts: [{ type: "tools", toolCalls: [read] }, { type: "text", content: "answer" }],
      phase: "working",
    }, 80);
    rows = rendered(component);
    expect(rows.at(-1)).toContain("reading files");
    expect(rows.join("\n")).toContain("◆ Read 1 file running");
    expect(rows.join("\n")).toContain("README.md");
    expect(rows.indexOf("┃  thought")).toBeLessThan(rows.findIndex((row) => row.includes("◆ Read")));
    expect(rows.findIndex((row) => row.includes("◆ Read"))).toBeLessThan(rows.indexOf(`${RAIL}answer`));
  });

  it("keeps live and settled multiline answers on the same rail projection", () => {
    const width = 12;
    const content = "alpha beta 这是多行 answer";
    const expected = projectAssistantRows(content, { columns: width }).map(strip);
    const component = new StreamingMessageComponent();
    component.update({
      content,
      reasoning: "",
      tools: [],
      parts: [{ type: "text", content }],
      phase: "thinking",
    }, width);

    const live = rendered(component, width);
    const start = live.findIndex((row) => row === expected[0]);
    expect(live.slice(start, start + expected.length)).toEqual(expected);

    const settled = renderMessage({ role: "assistant", content }, { columns: width })
      .map(strip)
      .slice(0, -1);
    expect(settled).toEqual(expected);
    // Every prose row sits behind the shared empty rail, never deeper.
    expect(expected.every((row) => row.length === 0 || (row.startsWith(RAIL) && !/^\s/u.test(row.slice(RAIL.length))))).toBe(true);
    expect(expected.join("\n")).not.toContain("●");
  });

  it("keeps ordered Thinking, commentary, full tool lifecycle, and answer trace", () => {
    const component = new StreamingMessageComponent();
    const pending = {
      id: "bash-1",
      name: "bash",
      args: {},
      rawArguments: '{"command":"npm test"}',
      status: "pending" as const,
    };
    component.update({
      content: "I will verify.Done.",
      reasoning: "inspect the tests",
      tools: [pending],
      parts: [
        { type: "text", content: "I will verify." },
        { type: "tools", toolCalls: [pending] },
        { type: "text", content: "Done." },
      ],
      phase: "working",
    }, 80);

    let text = rendered(component).join("\n");
    expect(text).toContain("┃  ◆ Thinking…");
    expect(text).toContain("◆ Execute npm test running");
    expect(text.indexOf("I will verify.")).toBeLessThan(text.indexOf("Execute npm test"));
    expect(text.indexOf("Execute npm test")).toBeLessThan(text.indexOf("Done."));
    const firstFrameRows = rendered(component);
    const reasoningBodyAt = firstFrameRows.indexOf("┃  inspect the tests");
    const commentaryAt = firstFrameRows.indexOf(`${RAIL}I will verify.`);
    const toolAt = firstFrameRows.findIndex((row) => row.includes("◆ Execute npm test running"));
    const answerAt = firstFrameRows.indexOf(`${RAIL}Done.`);
    expect(reasoningBodyAt).toBeGreaterThan(0);
    expect(commentaryAt).toBeGreaterThan(0);
    expect(firstFrameRows[reasoningBodyAt + 1]).toBe("");
    expect(firstFrameRows[commentaryAt + 1]).toBe("");
    expect(firstFrameRows[toolAt + 1]).toBe("");
    expect(answerAt).toBe(toolAt + 2);

    const running = { ...pending, args: { command: "npm test" }, status: "running" as const, result: "42 tests passed" };
    component.update({
      content: "I will verify.",
      reasoning: "inspect the tests",
      tools: [running],
      parts: [{ type: "text", content: "I will verify." }, { type: "tools", toolCalls: [running] }],
      phase: "working",
    }, 80);
    text = rendered(component).join("\n");
    expect(text).toContain("◆ Execute npm test running");
    expect(text).toContain("42 tests passed");

    const completed = { ...running, status: "completed" as const };
    component.update({
      content: "Done.",
      reasoning: "inspect the tests",
      tools: [completed],
      parts: [{ type: "tools", toolCalls: [completed] }, { type: "text", content: "Done." }],
      phase: "working",
    }, 80);
    text = rendered(component).join("\n");
    expect(text).not.toContain("Working");
    expect(text).not.toContain(" running");
    expect(text).toContain("1 line output · Ctrl+O to view");
    expect(text).toContain("┃  ◆ Thinking…");
  });

  it("rotates the spinner and idle phrase on the Ink cadence, then cleans up", () => {
    vi.useFakeTimers();
    const component = new StreamingMessageComponent();
    component.update({ content: "", reasoning: "", tools: [], parts: [], phase: "thinking" }, 80);
    component.startSpinner();
    const initial = rendered(component).at(-1)!;

    component.advanceAnimationFrame(100);
    const nextFrame = rendered(component).at(-1)!;
    expect(nextFrame).not.toBe(initial);

    component.advanceAnimationFrame(1_400);
    expect(rendered(component).at(-1)).toContain("reading the room");

    component.clearToNothing();
    expect(rendered(component)).toEqual([""]);
    expect(vi.getTimerCount()).toBe(0);
  });

  it("keeps the same five-line minimal reasoning surface after settle", () => {
    const compact = renderReasoning("\nR1\n\nR2\nR3\nR4\nR5\nR6\nR7", { columns: 80 }).map(strip);
    expect(compact).toEqual([
      "┃  ◆ Thinking",
      "┃  R3",
      "┃  R4",
      "┃  R5",
      "┃  R6",
      "┃  R7",
      "┃  … (Ctrl+T to expand)",
      "",
    ]);
    expect(compact.join("\n")).not.toContain("└─");
  });

  it("keeps every live and settled trace row within the terminal width", () => {
    for (const width of [1, 4, 12, 20, 40, 80]) {
      const component = new StreamingMessageComponent();
      component.update({
        content: "",
        reasoning: "这是非常长的中文 reasoning 🫧\n第二行也很长 🚦",
        tools: [{
          id: "bash-wide",
          name: "bash",
          args: { command: "npm test -- --run 这是非常长的命令参数 🚦" },
          status: "running",
          result: "这是非常长的运行结果 🫧",
        }],
        parts: [{
          type: "tools",
          toolCalls: [{
            id: "bash-wide",
            name: "bash",
            args: { command: "npm test -- --run 这是非常长的命令参数 🚦" },
            status: "running",
            result: "这是非常长的运行结果 🫧",
          }],
        }],
        phase: "thinking",
      }, width);
      expect(component.render(width).every((row) => visibleWidth(row) <= width)).toBe(true);
      expect(component.activityLane.render(width).every((row) => visibleWidth(row) <= width)).toBe(true);
      expect(renderReasoning("这是非常长的中文 reasoning 🫧\n第二行", { columns: width })
        .every((row) => visibleWidth(row) <= width)).toBe(true);
    }
  });

  it("keeps the reasoning-to-composer distance stable when the spinner clears", async () => {
    const terminal = new VirtualTerminal(80, 14);
    const tui = new TuiAltScreen(terminal);
    let messages: DisplayMessage[] = [
      ...Array.from({ length: 10 }, (_, index): DisplayMessage => ({
        key: `history-${index}`,
        role: "assistant",
        content: `history-${index}`,
      })),
      { key: "current-user", role: "user", content: "inspect" },
    ];
    const transcript = new ResponsiveTranscriptComponent(() => ({ messages, options: { theme: plainTheme } }));
    const streaming = new StreamingMessageComponent();
    const reasoning = [
      "TRACE_REASONING_1",
      "TRACE_REASONING_2",
      "TRACE_REASONING_3",
      "TRACE_REASONING_4",
      "TRACE_REASONING_5",
      "TRACE_REASONING_6",
      "TRACE_REASONING_7",
    ].join("\n");
    streaming.update({
      content: "",
      reasoning,
      tools: [],
      parts: [],
      phase: "thinking",
    }, 80, { theme: plainTheme });
    const editor = new Editor(tui, COMPOSER_EDITOR_THEME, COMPOSER_EDITOR_OPTIONS);
    const scroll = new ScrollView(new VStack([transcript, streaming]), { follow: "end", primary: true });
    tui.setLayoutRoot(new VStack([
      { component: scroll, basis: 0, grow: 1, minSize: 0 },
      { component: streaming.activityLane, basis: "auto", shrink: 0 },
      { component: editor, basis: "auto", shrink: 0 },
    ]));
    tui.setFocus(editor);
    tui.start();
    await terminal.waitForRender();

    const distanceToComposer = () => {
      const rows = terminal.getViewport();
      const reasoningAt = rows.findIndex((row) => row.includes("◆ Thinking"));
      const composerAt = rows.findIndex((row) => row.includes("┌"));
      expect(reasoningAt).toBeGreaterThanOrEqual(0);
      expect(composerAt).toBeGreaterThan(reasoningAt);
      return composerAt - reasoningAt;
    };
    const liveDistance = distanceToComposer();
    expect(liveDistance).toBeGreaterThanOrEqual(3);

    messages = [...messages, {
      key: "settled-reasoning",
      role: "assistant",
      content: "",
      reasoning,
    }];
    streaming.clearToNothing();
    tui.requestRender();
    await terminal.waitForRender();

    expect(distanceToComposer()).toBe(liveDistance);
    expect(streaming.activityLane.render(80)).toEqual([""]);
    tui.stop({ preserveScreen: true });
  });

  it("orders user, live Thinking, spinner, composer and footer in the current viewport", async () => {
    const terminal = new VirtualTerminal(80, 18);
    const tui = new TuiMainScreen(terminal);
    let messages: DisplayMessage[] = [{ key: "user", role: "user", content: "TRACE_USER_MARKER" }];
    const transcript = new ResponsiveTranscriptComponent(() => ({ messages, options: { theme: plainTheme } }));
    const streaming = new StreamingMessageComponent();
    streaming.update({ content: "", reasoning: "TRACE_LIVE_REASONING", tools: [], parts: [], phase: "thinking" }, 80);
    const editor = new Editor(tui, editorTheme);
    const footer = new ResponsiveFooterComponent(() => ({
      agent: { model: "TRACE_FOOTER", getContextUsageSnapshot: () => ({ usedTokens: 0, contextWindow: 1_000 }) },
      cwd: "~/trace",
    }));
    tui.addChild(transcript);
    tui.addChild(streaming);
    tui.addChild(streaming.activityLane);
    tui.addChild(editor);
    tui.addChild(footer);
    tui.setFocus(editor);
    tui.start();
    terminal.sendInput("Q");
    await terminal.waitForRender();

    let viewport = terminal.getViewport();
    const userAt = viewport.findIndex((row) => row.includes("TRACE_USER_MARKER"));
    const thinkingAt = viewport.findIndex((row) => row.includes("◆ Thinking"));
    const spinnerAt = viewport.findIndex((row) => row.includes("working through the request"));
    const composerAt = viewport.findIndex((row) => row.includes("Q"));
    const footerAt = viewport.findIndex((row) => row.includes("TRACE_FOOTER"));
    expect([userAt, thinkingAt, spinnerAt, composerAt, footerAt].every((index) => index >= 0)).toBe(true);
    expect(userAt).toBeLessThan(thinkingAt);
    expect(thinkingAt).toBeLessThan(spinnerAt);
    expect(spinnerAt).toBeLessThan(composerAt);
    expect(composerAt).toBeLessThan(footerAt);

    terminal.resize(20, 5);
    const narrowTool = {
      id: "narrow",
      name: "bash",
      args: { command: "npm test -- --run very-long-command" },
      status: "running" as const,
      result: "still running",
    };
    streaming.update({
      content: "TRACE_LIVE_ANSWER",
      reasoning: "TRACE_LIVE_REASONING",
      tools: [narrowTool],
      parts: [{ type: "tools", toolCalls: [narrowTool] }, { type: "text", content: "TRACE_LIVE_ANSWER" }],
      phase: "working",
    }, 20);
    terminal.sendInput("R");
    await terminal.waitForRender();
    expect(tui.getFocusedComponent()).toBe(editor);
    expect(editor.getText()).toBe("QR");
    expect(terminal.getViewport().some((row) => row.includes("QR"))).toBe(true);
    expect(streaming.render(20).every((row) => visibleWidth(row) <= 20)).toBe(true);

    terminal.resize(80, 18);
    await terminal.waitForRender();

    messages = [...messages, {
      key: "assistant",
      role: "assistant",
      content: "TRACE_SETTLED_ANSWER",
      reasoning: "TRACE_SETTLED_REASONING",
    }];
    streaming.clearToNothing();
    tui.requestRender();
    await terminal.waitForRender();
    viewport = terminal.getViewport();
    expect(viewport.some((row) => row.includes("TRACE_SETTLED_ANSWER"))).toBe(true);
    expect(viewport.some((row) => row.includes("mapping the workspace") || row.includes("working through the request"))).toBe(false);
    expect(viewport.some((row) => row.includes("└─"))).toBe(false);
    tui.stop();
  });
});

describe("production streaming flush scheduler", () => {
  it("coalesces a burst, allows the next burst, and cancels pending work", () => {
    vi.useFakeTimers();
    const scheduler = createFlushScheduler();
    const first = vi.fn();
    for (let index = 0; index < 10; index += 1) scheduler.scheduleFlush(40, first);

    expect(vi.getTimerCount()).toBe(1);
    vi.advanceTimersByTime(40);
    expect(first).toHaveBeenCalledTimes(1);

    const second = vi.fn();
    scheduler.scheduleFlush(40, second);
    expect(vi.getTimerCount()).toBe(1);
    scheduler.cancelFlush();
    scheduler.cancelFlush();
    vi.advanceTimersByTime(40);
    expect(second).not.toHaveBeenCalled();
  });
});
