import { EventEmitter } from "node:events";
import { describe, expect, it, vi } from "vitest";
import {
  PrintRunCollector,
  formatPrintJson,
  formatPrintJsonError,
  parseOutputFormat,
  watchForStalledTurn,
} from "../print-output.js";
import type { AgentEvent } from "../types.js";

function feed(events: AgentEvent[]): PrintRunCollector {
  const collector = new PrintRunCollector();
  for (const event of events) collector.onEvent(event);
  return collector;
}

describe("PrintRunCollector", () => {
  it("keeps the final turn's text as the answer across an agentic run", () => {
    const collector = feed([
      { type: "turn_start" },
      { type: "text_delta", content: "let me look..." },
      { type: "tool_start", id: "1", name: "read", args: {} },
      { type: "turn_end", willContinue: true },
      { type: "turn_start" },
      { type: "text_delta", content: "The answer " },
      { type: "text_delta", content: "is 42." },
      { type: "turn_end" },
    ] as AgentEvent[]);

    const summary = collector.summary();
    expect(summary.text).toBe("The answer is 42.");
    expect(summary.num_turns).toBe(2);
    expect(summary.num_tool_calls).toBe(1);
  });

  it("keeps the last non-empty turn text when the final turn is tool-only", () => {
    const collector = feed([
      { type: "turn_start" },
      { type: "text_delta", content: "Done: all tests pass." },
      { type: "turn_end", willContinue: true },
      { type: "turn_start" },
      { type: "tool_start", id: "1", name: "bash", args: {} },
      { type: "turn_end" },
    ] as AgentEvent[]);

    expect(collector.summary().text).toBe("Done: all tests pass.");
  });

  it("accumulates usage across turn_end events", () => {
    const collector = feed([
      { type: "turn_start" },
      { type: "turn_end", willContinue: true, usage: { promptTokens: 100, completionTokens: 20, promptCacheHitTokens: 60, reasoningTokens: 5, totalTokens: 120 } },
      { type: "turn_start" },
      { type: "turn_end", usage: { promptTokens: 200, completionTokens: 30 } },
    ] as AgentEvent[]);

    const usage = collector.summary().usage;
    // input_tokens is the UNCACHED remainder, not the prompt total: turn 1
    // contributes 100-60=40, turn 2 contributes its full 200.
    expect(usage.input_tokens).toBe(240);
    expect(usage.cache_read_input_tokens).toBe(60);
    expect(usage.cache_creation_input_tokens).toBe(0);
    expect(usage.output_tokens).toBe(50);
    expect(usage.reasoning_tokens).toBe(5);
    // totalTokens present (120) + fallback prompt+completion (230).
    expect(usage.total_tokens).toBe(350);
  });

  it("reports the four prompt buckets as disjoint, summing to total", () => {
    // A harness that priced the old grand-total input_tokens at the full input
    // rate would overstate cost roughly 2x once message-level caching makes
    // reads the bulk of the prompt. The buckets must not overlap.
    const collector = feed([
      { type: "turn_start" },
      {
        type: "turn_end",
        usage: {
          promptTokens: 1000,
          completionTokens: 40,
          promptCacheHitTokens: 700,
          promptCacheMissTokens: 300, // already includes the 250 written
          cacheCreationTokens: 250,
          totalTokens: 1040,
        },
      },
    ] as AgentEvent[]);

    const usage = collector.summary().usage;
    expect(usage.input_tokens).toBe(50);
    expect(usage.cache_read_input_tokens).toBe(700);
    expect(usage.cache_creation_input_tokens).toBe(250);
    expect(usage.output_tokens).toBe(40);
    expect(
      usage.input_tokens
      + usage.cache_read_input_tokens
      + usage.cache_creation_input_tokens
      + usage.output_tokens,
    ).toBe(usage.total_tokens);
  });

  it("discards partial text superseded by a provider retry", () => {
    const collector = feed([
      { type: "turn_start" },
      { type: "text_delta", content: "half an ans" },
      { type: "provider_retry", attempt: 1, maxAttempts: 3 },
      { type: "text_delta", content: "The full answer." },
      { type: "turn_end" },
    ] as AgentEvent[]);

    expect(collector.summary().text).toBe("The full answer.");
  });
});

describe("formatPrintJson", () => {
  it("emits a single parseable object with the documented fields", () => {
    const collector = feed([
      { type: "turn_start" },
      { type: "text_delta", content: "hi" },
      { type: "turn_end", usage: { promptTokens: 10, completionTokens: 2 } },
    ] as AgentEvent[]);

    const parsed = JSON.parse(formatPrintJson({
      summary: collector.summary(),
      sessionId: "2026-07-19T00-00-00-000Z.jsonl",
    }));

    expect(parsed).toMatchObject({
      text: "hi",
      stopReason: "end_turn",
      sessionId: "2026-07-19T00-00-00-000Z.jsonl",
      num_turns: 1,
      num_tool_calls: 0,
    });
    expect(parsed.usage.input_tokens).toBe(10);
    expect(parsed.usage.output_tokens).toBe(2);
    expect(parsed.usage_reported).toBe(true);
  });

  it("marks usage_reported false when the provider never sent usage", () => {
    const collector = feed([
      { type: "turn_start" },
      { type: "text_delta", content: "hi" },
      { type: "turn_end" },
    ] as AgentEvent[]);

    const parsed = JSON.parse(formatPrintJson({ summary: collector.summary() }));
    expect(parsed.usage_reported).toBe(false);
    expect(parsed.usage.total_tokens).toBe(0);
  });

  it("formats structured errors with partial progress", () => {
    const collector = feed([
      { type: "turn_start" },
      { type: "tool_start", id: "1", name: "bash", args: {} },
    ] as AgentEvent[]);

    const parsed = JSON.parse(formatPrintJsonError({
      message: "provider quota exceeded",
      summary: collector.summary(),
    }));

    expect(parsed.type).toBe("error");
    expect(parsed.stopReason).toBe("error");
    expect(parsed.message).toContain("quota");
    expect(parsed.num_tool_calls).toBe(1);
  });
});

describe("compaction stats field", () => {
  it("includes compaction counts when provided, omits when absent", () => {
    const collector = feed([{ type: "turn_start" }] as AgentEvent[]);
    const stats = { resident: 2, subturn: 1, llm: 1, overflow: 0, fired: 4, droppedMessages: 14 };

    const withStats = JSON.parse(formatPrintJson({ summary: collector.summary(), compaction: stats }));
    expect(withStats.compaction).toEqual(stats);

    const withoutStats = JSON.parse(formatPrintJson({ summary: collector.summary() }));
    expect(withoutStats).not.toHaveProperty("compaction");

    const errorWithStats = JSON.parse(formatPrintJsonError({ message: "boom", compaction: stats }));
    expect(errorWithStats.compaction).toEqual(stats);
  });
});

describe("parseOutputFormat", () => {
  it("accepts plain and json, rejects everything else", () => {
    expect(parseOutputFormat("plain")).toBe("plain");
    expect(parseOutputFormat("json")).toBe("json");
    expect(parseOutputFormat("stream-json")).toBeUndefined();
    expect(parseOutputFormat(undefined)).toBeUndefined();
  });
});

describe("watchForStalledTurn", () => {
  it("reports once when the event loop drains with the turn still pending", () => {
    const proc = new EventEmitter();
    const onStalled = vi.fn();
    watchForStalledTurn(onStalled, proc);

    proc.emit("beforeExit", 0);
    proc.emit("beforeExit", 0);

    expect(onStalled).toHaveBeenCalledTimes(1);
  });

  it("stays silent once the turn has settled", () => {
    const proc = new EventEmitter();
    const onStalled = vi.fn();
    const turnSettled = watchForStalledTurn(onStalled, proc);

    turnSettled();
    proc.emit("beforeExit", 0);

    expect(onStalled).not.toHaveBeenCalled();
    expect(proc.listenerCount("beforeExit")).toBe(0);
  });
});
