/**
 * Structured output for print mode (`-p --output-format json`).
 *
 * Built for benchmark/CI adapters (Pier, terminal-bench): stdout carries
 * exactly one JSON object; all human-facing noise goes to stderr. Field
 * names follow the grok-build/Claude Code convention (text, stopReason,
 * sessionId, usage.{input_tokens,...}, num_turns) so existing harness
 * adapters port with minimal changes.
 */

import type { AgentEvent, TokenUsage } from "./types.js";

export type PrintOutputFormat = "plain" | "json";

export function parseOutputFormat(raw: string | undefined): PrintOutputFormat | undefined {
  if (raw === undefined) return undefined;
  if (raw === "plain" || raw === "json") return raw;
  return undefined;
}

/**
 * Anthropic's usage decomposition, which the field names already claimed to
 * follow: the four token buckets are DISJOINT and sum to `total_tokens`.
 *
 *   input_tokens + cache_read_input_tokens + cache_creation_input_tokens
 *     + output_tokens === total_tokens
 *
 * `input_tokens` is therefore the uncached remainder only — it does NOT include
 * cache reads. It used to carry the whole prompt total, which was harmless while
 * cache reads were a flat few thousand tokens per turn, but message-level prompt
 * caching makes reads 70-90% of the prompt, and a harness that priced
 * `input_tokens` at the full input rate would then overstate cost ~2x.
 */
export interface PrintUsageSummary {
  input_tokens: number;
  cache_read_input_tokens: number;
  cache_creation_input_tokens: number;
  output_tokens: number;
  reasoning_tokens: number;
  total_tokens: number;
}

export interface PrintRunSummary {
  /** Final answer: the last turn's streamed text. */
  text: string;
  num_turns: number;
  num_tool_calls: number;
  usage: PrintUsageSummary;
  /**
   * False when no turn carried provider usage (some endpoints — notably the
   * Grok subscription proxy — omit it): zeros then mean "not reported",
   * not "free". Mirrors the goal engine's markTokenUsageUnavailable and
   * grok-build's usage_is_incomplete convention.
   */
  usage_reported: boolean;
}

export class PrintRunCollector {
  private turnText = "";
  private lastTurnText = "";
  private turns = 0;
  private toolCalls = 0;
  private usageReported = false;
  private readonly usage: PrintUsageSummary = {
    input_tokens: 0,
    cache_read_input_tokens: 0,
    cache_creation_input_tokens: 0,
    output_tokens: 0,
    reasoning_tokens: 0,
    total_tokens: 0,
  };

  onEvent(event: AgentEvent): void {
    switch (event.type) {
      case "turn_start":
        this.turns += 1;
        this.turnText = "";
        break;
      case "text_delta":
        this.turnText += event.content;
        break;
      case "provider_retry":
        // The retried response supersedes the partial text.
        this.turnText = "";
        break;
      case "tool_start":
        this.toolCalls += 1;
        break;
      case "turn_end":
        // Usage reaches AgentEvent only on turn_end (the stream-level
        // "usage" chunk is aggregated into it by the agent loop).
        if (this.turnText.trim()) this.lastTurnText = this.turnText;
        if (event.usage) this.addUsage(event.usage);
        break;
      default:
        break;
    }
  }

  private addUsage(usage: TokenUsage): void {
    this.usageReported = true;
    const prompt = usage.promptTokens ?? 0;
    const cacheRead = usage.promptCacheHitTokens ?? 0;
    // promptCacheMissTokens already includes creation, so creation has to be
    // netted out of the uncached remainder rather than added alongside it.
    const cacheCreation = usage.cacheCreationTokens ?? 0;
    const uncached = usage.promptCacheMissTokens !== undefined
      ? Math.max(0, usage.promptCacheMissTokens - cacheCreation)
      : Math.max(0, prompt - cacheRead - cacheCreation);
    this.usage.input_tokens += uncached;
    this.usage.cache_read_input_tokens += cacheRead;
    this.usage.cache_creation_input_tokens += cacheCreation;
    this.usage.output_tokens += usage.completionTokens ?? 0;
    this.usage.reasoning_tokens += usage.reasoningTokens ?? 0;
    this.usage.total_tokens += usage.totalTokens
      ?? ((usage.promptTokens ?? 0) + (usage.completionTokens ?? 0));
  }

  summary(): PrintRunSummary {
    return {
      text: (this.lastTurnText || this.turnText).trim(),
      num_turns: this.turns,
      num_tool_calls: this.toolCalls,
      usage: { ...this.usage },
      usage_reported: this.usageReported,
    };
  }
}

/**
 * Harness-observed change footprint of the run (git ground truth). Printed
 * by the harness, not the model — a run cannot omit or misstate what it
 * touched ("no breaking changes" next to deleted test assertions).
 */
export interface PrintChangeSummary {
  changed_files: number;
  modified_existing_tests: Array<{ path: string; deleted_lines: number }>;
}

/**
 * Per-path counts of context compactions during the run. Lets benchmark
 * harnesses correlate outcomes with "did the model ever lose sight of the
 * full history" without access to the in-container session files.
 */
export interface PrintCompactionStats {
  resident: number;
  subturn: number;
  llm: number;
  overflow: number;
  /** Successful compaction computations incl. rejected rewrites (churn signal). */
  fired: number;
  droppedMessages: number;
}

export function formatPrintJson(input: {
  summary: PrintRunSummary;
  sessionId?: string;
  stopReason?: "end_turn" | "error" | "cancelled";
  compaction?: PrintCompactionStats;
  changes?: PrintChangeSummary;
}): string {
  return JSON.stringify({
    text: input.summary.text,
    stopReason: input.stopReason ?? "end_turn",
    ...(input.sessionId ? { sessionId: input.sessionId } : {}),
    num_turns: input.summary.num_turns,
    num_tool_calls: input.summary.num_tool_calls,
    usage: input.summary.usage,
    usage_reported: input.summary.usage_reported,
    ...(input.compaction ? { compaction: input.compaction } : {}),
    ...(input.changes ? { changes: input.changes } : {}),
  });
}

export function formatPrintJsonError(input: {
  message: string;
  summary?: PrintRunSummary;
  sessionId?: string;
  compaction?: PrintCompactionStats;
  changes?: PrintChangeSummary;
}): string {
  return JSON.stringify({
    type: "error",
    message: input.message,
    stopReason: "error",
    ...(input.sessionId ? { sessionId: input.sessionId } : {}),
    ...(input.summary
      ? {
          text: input.summary.text,
          num_turns: input.summary.num_turns,
          num_tool_calls: input.summary.num_tool_calls,
          usage: input.summary.usage,
        }
      : {}),
    ...(input.compaction ? { compaction: input.compaction } : {}),
    ...(input.changes ? { changes: input.changes } : {}),
  });
}

/**
 * Print mode ends when its turn settles. When nothing is left on the event
 * loop that could settle it (a reply stream that neither ends nor fails),
 * Node exits on its own with code 0, and the run would pass for a success
 * with whatever partial text it had printed.
 *
 * `onStalled` runs once if the loop drains before the returned function is
 * called. Call the returned function as soon as the turn settles either way.
 */
export function watchForStalledTurn(
  onStalled: () => void,
  proc: {
    on(event: "beforeExit", listener: () => void): unknown;
    off(event: "beforeExit", listener: () => void): unknown;
  } = process,
): () => void {
  const onBeforeExit = () => {
    proc.off("beforeExit", onBeforeExit);
    onStalled();
  };
  proc.on("beforeExit", onBeforeExit);
  return () => {
    proc.off("beforeExit", onBeforeExit);
  };
}
