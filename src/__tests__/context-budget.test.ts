import { describe, expect, it } from "vitest";
import { estimateContextTokens, getContextBudget } from "../context/budget.js";
import { buildContextUsageSnapshot, formatContextUsage } from "../context/usage.js";
import { buildDeferredToolsReminder } from "../prompt/reminders.js";
import { formatSkillsPrompt } from "../skills/format.js";
import type { Message } from "../types.js";

describe("context budget", () => {
  it("estimates tokens from message content", () => {
    const messages: Message[] = [
      { role: "system", content: "system instruction" },
      { role: "user", content: "hello world" },
      { role: "assistant", content: "reply" },
    ];

    expect(estimateContextTokens(messages)).toBeGreaterThan(0);
  });

  it("uses model catalog context windows when available", () => {
    const messages: Message[] = [
      { role: "user", content: "x".repeat(4000) },
    ];

    const budget = getContextBudget("openai-codex", "gpt-5.4", messages);
    expect(budget.contextWindow).toBe(272000);
    expect(budget.percent).toBeDefined();
  });

  it("flags prune and compact thresholds as usage grows", () => {
    const messages: Message[] = [
      { role: "user", content: "x".repeat(450000) },
    ];

    const budget = getContextBudget("openai", "gpt-4o", messages);
    expect(budget.shouldPrune).toBe(true);
    expect(budget.shouldCompact).toBe(true);
  });

  it("compacts no later than a ceiling, however large the window", () => {
    // A million-token window alone puts the line at about a million tokens.
    const anchored = (used: number, compactAtTokens?: number) =>
      getContextBudget("google", "gemini-4-carbon", [], { usageAnchorTokens: used, tailMessages: [], compactAtTokens }).shouldCompact;
    expect(getContextBudget("google", "gemini-4-carbon", []).contextWindow).toBe(1_048_576);
    expect(anchored(600_000)).toBe(false);
    expect(anchored(600_000, 500_000)).toBe(true);
    expect(anchored(499_999, 500_000)).toBe(false);
    // A ceiling above the window's own line changes nothing, and one that is not a size is no ceiling.
    expect(anchored(1_000_000, 5_000_000)).toBe(false);
    expect(anchored(1_020_000, 5_000_000)).toBe(true);
    expect(anchored(600_000, 0)).toBe(false);
    // On a window whose own line is lower, the window's line stands.
    const small = (used: number) =>
      getContextBudget("openai-codex", "gpt-5.4", [], { usageAnchorTokens: used, tailMessages: [], compactAtTokens: 500_000 }).shouldCompact;
    expect(small(238_000)).toBe(false);
    expect(small(240_000)).toBe(true);
  });

  it("uses fixed-reserve threshold on a large window", () => {
    // gpt-5.4 window 272k: compact threshold = 272k - 20k - 13k = 239k;
    // prune threshold = 272k - 20k - 50k = 202k.
    // No anchor → first-turn 1.25x safety margin applies to the estimate.
    // 500_000 chars → 125k heuristic → 156k after margin → under both thresholds.
    const safeMessages: Message[] = [{ role: "user", content: "x".repeat(500_000) }];
    const budgetSafe = getContextBudget("openai-codex", "gpt-5.4", safeMessages);
    expect(budgetSafe.shouldCompact).toBe(false);
    expect(budgetSafe.shouldPrune).toBe(false);

    // 1_000_000 chars → 250k heuristic → 313k after margin → both trip.
    const bigMessages: Message[] = [{ role: "user", content: "x".repeat(1_000_000) }];
    const budgetBig = getContextBudget("openai-codex", "gpt-5.4", bigMessages);
    expect(budgetBig.shouldCompact).toBe(true);
    expect(budgetBig.shouldPrune).toBe(true);
  });

  it("respects usage anchor from response when provided", () => {
    const messages: Message[] = [
      { role: "user", content: "old turn" },
      { role: "assistant", content: "old reply" },
      { role: "user", content: "new turn" },
    ];
    // Server says 200k tokens already used after message 2.
    // Tail is just message 3 (small). Total ~= 200k, above 272k's 239k compact threshold? No, 200 < 239.
    // Use 245k anchor instead to cross the threshold.
    const budget = getContextBudget("openai-codex", "gpt-5.4", messages, {
      usageAnchorTokens: 245_000,
      tailMessages: messages.slice(2),
    });
    expect(budget.estimatedTokens).toBeGreaterThan(245_000);
    expect(budget.shouldCompact).toBe(true);
  });

  it("splits context usage into system, tools, skills, and other buckets", () => {
    const skills = [{ name: "repo-review", description: "Review code carefully", source: "project" as const }];
    const messages: Message[] = [
      { role: "system", content: `Base system prompt\n\n${formatSkillsPrompt(skills)}` },
      { role: "meta", kind: "system-reminder", content: buildDeferredToolsReminder(["mcp__arxiv__search"]) },
      { role: "user", content: "please inspect the repo" },
      { role: "assistant", content: "I'll take a look." },
    ];

    const snapshot = buildContextUsageSnapshot({
      providerId: "openai",
      modelId: "gpt-4o",
      messages,
      toolEntries: [
        {
          name: "read",
          description: "Read a file",
          parameters: { type: "object", properties: { path: { type: "string" } }, required: ["path"] },
          execute: async () => ({ content: "" }),
        },
      ],
      deferredToolEntries: [
        {
          name: "mcp__arxiv__search",
          description: "[MCP:arxiv] Search papers",
          parameters: { type: "object", properties: { query: { type: "string" } }, required: ["query"] },
          execute: async () => ({ content: "" }),
          deferred: true,
        },
      ],
      skills,
    });

    expect(snapshot.contextWindow).toBe(128000);
    expect(snapshot.buckets.systemPrompt.tokens).toBeGreaterThan(0);
    expect(snapshot.buckets.tools.tokens).toBeGreaterThan(0);
    expect(snapshot.buckets.skills.tokens).toBeGreaterThan(0);
    expect(snapshot.buckets.deferredTools.tokens).toBeGreaterThan(0);
    expect(snapshot.buckets.other.tokens).toBeGreaterThan(0);
    expect(snapshot.usedTokens).toBe(
      snapshot.buckets.systemPrompt.tokens
      + snapshot.buckets.tools.tokens
      + snapshot.buckets.skills.tokens
      + snapshot.buckets.deferredTools.tokens
      + snapshot.buckets.other.tokens,
    );
    expect(formatContextUsage(snapshot)).toContain("Free space:");
  });
});
