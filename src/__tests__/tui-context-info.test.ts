import { describe, expect, it, vi } from "vitest";
import { visibleWidth } from "@bubblebrain-ai/pi-tui";
import {
  ContextInfoComponent,
  allocateContextCells,
  thinkingRow,
  type ContextInfoPanelData,
} from "../tui/components/context-info.js";

const ANSI_RE = /\x1b\[[0-?]*[ -/]*[@-~]/g;
const plain = (value: string) => value.replace(ANSI_RE, "");

function fixture(): ContextInfoPanelData {
  return {
    snapshot: {
      providerId: "openai-codex",
      modelId: "gpt-5.6-sol",
      contextWindow: 1_000_000,
      usedTokens: 37_000,
      freeTokens: 963_000,
      buckets: {
        systemPrompt: { label: "System prompt", tokens: 1_200 },
        tools: { label: "Tools", tokens: 3_000 },
        skills: { label: "Skills", tokens: 1_500 },
        deferredTools: { label: "Deferred/MCP", tokens: 1_300 },
        other: { label: "Other", tokens: 30_000 },
      },
      toolCount: 12,
      deferredToolCount: 4,
      skillCount: 21,
      messageCount: 15,
    },
    sessionId: "session-123",
    cwd: "~/project",
    thinking: "high",
    permissionMode: "default",
    turnCount: 5,
    toolCallCount: 12,
    compactionCount: 0,
    mcpServerCount: 4,
  };
}

function component(rows = 40) {
  const onClose = vi.fn();
  const onRender = vi.fn();
  return {
    instance: new ContextInfoComponent(fixture(), {
      getTerminalRows: () => rows,
      onClose,
      onRender,
      copySessionId: vi.fn(async () => {}),
    }),
    onClose,
    onRender,
  };
}

function gridRows(lines: string[]): string[] {
  return lines.map(plain).filter((line) => {
    const trimmed = line.replace(/^[│ ]+|[│ ]+$/g, "");
    return /^(?:[◆◇] ?){10,20}$/.test(trimmed);
  });
}

describe("ContextInfoComponent", () => {
  it("renders the Grok-style framed usage panel with a 5x20 grid", () => {
    const { instance } = component();
    const lines = instance.render(100);
    const text = lines.map(plain).join("\n");

    expect(lines).toHaveLength(30);
    expect(lines.every((line) => visibleWidth(line) <= 100)).toBe(true);
    expect(text).toContain("Context usage");
    expect(text).toContain("Usage limit");
    expect(text).toContain("Session info");
    expect(text).toContain("Reasoning/overhead");
    expect(text).toContain("Tool definitions");
    expect(text).toContain("Auto-compact at");
    expect(gridRows(lines)).toHaveLength(5);
    expect(gridRows(lines).reduce((count, line) => count + (line.match(/[◆◇]/g)?.length ?? 0), 0)).toBe(100);
  });

  it("switches to a 10x10 grid and stays within a narrow terminal", () => {
    const { instance } = component(40);
    const lines = instance.render(44);
    const text = lines.map(plain).join("\n");

    expect(lines).toHaveLength(30);
    expect(lines.every((line) => visibleWidth(line) <= 44)).toBe(true);
    expect(text).toContain("Session info");
    expect(gridRows(lines)).toHaveLength(10);
    expect(gridRows(lines).every((line) => (line.match(/[◆◇]/g)?.length ?? 0) === 10)).toBe(true);
  });

  it("supports tab navigation, scrolling, and Esc close", () => {
    const { instance, onClose, onRender } = component();
    instance.render(100);

    instance.handleInput("\t");
    expect(instance.render(100).map(plain).join("\n")).toContain("Provider billing and plan limits");
    instance.handleInput("\t");
    expect(instance.render(100).map(plain).join("\n")).toContain("session-123");
    instance.handleInput("1");
    instance.render(44);
    instance.handleInput("G");
    expect(onRender).toHaveBeenCalled();
    instance.handleInput("\x1b");
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it("never emits an over-wide line in a tiny terminal", () => {
    const { instance } = component(5);
    const lines = instance.render(7);
    expect(lines.length).toBeLessThanOrEqual(1);
    expect(lines.every((line) => visibleWidth(line) <= 7)).toBe(true);
  });
});

describe("allocateContextCells", () => {
  it("shows a real effort grade in session info", () => {
    const { instance } = component();
    instance.render(100);
    instance.handleInput("\t");
    instance.handleInput("\t");
    expect(instance.render(100).map(plain).join("\n")).toMatch(/Reasoning effort[\s│]+high/);
  });

  it("does not show a thinking placeholder as an effort grade", () => {
    const row = (providerId: string, modelId: string, thinking: string) => {
      const data = fixture();
      return thinkingRow({ snapshot: { ...data.snapshot, providerId, modelId }, thinking });
    };

    expect(row("openai-codex", "gpt-5.6-sol", "medium")).toEqual(["Reasoning effort", "medium"]);
    expect(row("anthropic", "claude-fable-5-1", "high")).toEqual(["Reasoning effort", "high"]);
    // Always thinks, no grades.
    expect(row("moss", "nex-n2.6", "medium")).toEqual(["Thinking", "always on"]);
    expect(row("moonshot-cn", "kimi-k2.7-code", "medium")).toEqual(["Thinking", "always on"]);
    // On/off only.
    expect(row("moonshot-cn", "kimi-k2.6", "medium")).toEqual(["Thinking", "on"]);
    expect(row("moonshot-cn", "kimi-k2.6", "off")).toEqual(["Thinking", "off"]);
    expect(row("zhipuai", "glm-5.1", "medium")).toEqual(["Thinking", "on"]);
    expect(row("zai-coding-plan", "glm-5-turbo", "off")).toEqual(["Thinking", "off"]);
    expect(row("zhipuai", "glm-5.2", "high")).toEqual(["Reasoning effort", "high"]);
    // The stored "provider:id" form resolves the same way.
    expect(row("moss", "moss:nex-n2.6", "medium")).toEqual(["Thinking", "always on"]);
  });

  it("always allocates exactly one hundred cells", () => {
    expect(allocateContextCells([1_200, 30_000, 5_800, 963_000]).reduce((sum, value) => sum + value, 0)).toBe(100);
    expect(allocateContextCells([0, 0, 0, 0])).toEqual([0, 0, 0, 100]);
  });
});
