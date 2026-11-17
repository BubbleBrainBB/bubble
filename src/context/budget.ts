import { getModelContextWindow } from "../model-catalog.js";
import type { AssistantProviderMetadata, Message, ToolDefinition } from "../types.js";
import { getTokenEstimator } from "./token-estimator.js";

export const OUTPUT_RESERVE_TOKENS = 20_000;
export const AUTOCOMPACT_BUFFER_TOKENS = 13_000;
export const PRUNE_BUFFER_TOKENS = 50_000;
export const MIN_WINDOW_FOR_RESERVE = 40_000;

// Safety margins applied to estimator-derived token counts. The estimator can
// undercount on dense / CJK / tool-payload content; treating its output as a
// hard floor means we'd routinely overshoot the real server-side count. These
// multipliers bias the budget decision toward earlier compaction.
const TAIL_SAFETY_MARGIN = 1.15;     // applied to estimated tail when anchored
const FIRST_TURN_SAFETY_MARGIN = 1.25; // applied when there's no anchor yet

export interface ContextBudget {
  estimatedTokens: number;
  contextWindow?: number;
  percent?: number;
  shouldPrune: boolean;
  shouldCompact: boolean;
}

export interface ContextBudgetOptions {
  /** Authoritative input-token count from the most recent response usage. */
  usageAnchorTokens?: number;
  /** Messages appended after the anchor (their tokens are estimated and added). */
  tailMessages?: Message[];
  /** Tokens outside the message list, primarily serialized tool definitions. */
  additionalInputTokens?: number;
  /**
   * Compact no later than this many tokens, however large the window. The
   * window alone puts the line where the context no longer fits: on a model
   * with a million-token window that is about a million tokens, reread with
   * every request. A host of long-lived agents sets a ceiling; without one
   * the line is the window's.
   */
  compactAtTokens?: number;
}

export function estimateMessageTokens(message: Message, providerId?: string): number {
  const estimate = (text: string) => estimateTextTokens(text, providerId);
  switch (message.role) {
    case "system":
    case "meta":
    case "tool":
      return estimate(message.content);
    case "assistant":
      return estimate(message.content)
        + estimate(message.reasoning ?? "")
        + estimateProviderMetadataOverhead(message.providerMetadata, providerId)
        + (message.toolCalls?.reduce((sum, toolCall) => sum + estimate(toolCall.arguments) + 12, 0) ?? 0)
        + 8;
    case "user":
      if (typeof message.content === "string") {
        return estimate(message.content) + 8;
      }
      return message.content.reduce((sum, part) => {
        if (part.type === "text") {
          return sum + estimate(part.text);
        }
        return sum + 256;
      }, 8);
  }
}

function estimateProviderMetadataOverhead(
  metadata: AssistantProviderMetadata | undefined,
  providerId?: string,
): number {
  const blocks = [
    ...(metadata?.anthropic?.contentBlocks ?? []),
    ...(metadata?.openai?.contentBlocks ?? []),
  ];
  if (blocks.length === 0) return 0;
  const estimate = (text: string) => estimateTextTokens(text, providerId);
  return blocks.reduce((sum, block) => {
    let overhead = 0;
    if (typeof block.signature === "string") overhead += estimate(block.signature);
    if (block.type === "redacted_thinking" && typeof block.data === "string") overhead += estimate(block.data);
    if (block.type === "reasoning" && typeof block.encrypted_content === "string") overhead += estimate(block.encrypted_content);
    return sum + overhead;
  }, 0);
}

export function estimateContextTokens(messages: Message[], providerId?: string): number {
  return messages.reduce((sum, message) => sum + estimateMessageTokens(message, providerId), 0);
}

export function getContextBudget(
  providerId: string,
  modelId: string,
  messages: Message[],
  options: ContextBudgetOptions = {},
): ContextBudget {
  const estimatedTokens = computeEstimatedTokens(providerId, messages, options);
  const contextWindow = getModelContextWindow(providerId, modelId);
  const percent = contextWindow ? Math.min(100, (estimatedTokens / contextWindow) * 100) : undefined;

  return {
    estimatedTokens,
    contextWindow,
    percent,
    shouldPrune: shouldTriggerPrune(estimatedTokens, contextWindow),
    shouldCompact: shouldTriggerCompact(estimatedTokens, contextWindow, options.compactAtTokens),
  };
}

function computeEstimatedTokens(providerId: string, messages: Message[], options: ContextBudgetOptions): number {
  const additionalInputTokens = Math.max(0, options.additionalInputTokens ?? 0);
  if (options.usageAnchorTokens !== undefined && options.tailMessages) {
    // Anchor is authoritative (server-reported input tokens from the last
    // response). Tail goes through our estimator and may undercount on dense /
    // tool-output content, so we inflate it by a small margin before adding.
    const tailEstimate = estimateContextTokens(options.tailMessages, providerId);
    return options.usageAnchorTokens
      + Math.ceil(tailEstimate * TAIL_SAFETY_MARGIN)
      + additionalInputTokens;
  }
  // First turn (or anchor lost): there's no server-reported baseline at all,
  // so apply a larger safety margin to the pure estimate.
  return Math.ceil(
    (estimateContextTokens(messages, providerId) + additionalInputTokens)
      * FIRST_TURN_SAFETY_MARGIN,
  );
}

/** Estimate the request-space occupied by tool schemas sent on every turn. */
export function estimateToolDefinitionsTokens(
  tools: ToolDefinition[],
  providerId?: string,
): number {
  if (tools.length === 0) return 0;
  // Providers wrap this array in their own function/tool envelope. The fixed
  // per-entry allowance covers names, discriminator keys and separators not
  // represented by the plain JSON payload.
  return estimateTextTokens(JSON.stringify(tools), providerId) + tools.length * 16 + 16;
}

/** Maximum estimated input size that still leaves useful room for an answer. */
export function getMaxInputTokens(contextWindow: number | undefined): number | undefined {
  if (!contextWindow) return undefined;
  return contextWindow >= MIN_WINDOW_FOR_RESERVE
    ? Math.max(
        1,
        contextWindow
          - OUTPUT_RESERVE_TOKENS
          - Math.max(AUTOCOMPACT_BUFFER_TOKENS, Math.floor(contextWindow * 0.05)),
      )
    : Math.max(1, Math.floor(contextWindow * 0.75));
}

function shouldTriggerPrune(estimatedTokens: number, contextWindow?: number): boolean {
  if (!contextWindow) {
    return estimatedTokens >= 16_000;
  }
  const threshold = contextWindow >= MIN_WINDOW_FOR_RESERVE
    ? contextWindow - OUTPUT_RESERVE_TOKENS - PRUNE_BUFFER_TOKENS
    : contextWindow * 0.55;
  return estimatedTokens >= threshold;
}

function shouldTriggerCompact(estimatedTokens: number, contextWindow?: number, ceiling?: number): boolean {
  const ofWindow = !contextWindow
    ? 32_000
    : contextWindow >= MIN_WINDOW_FOR_RESERVE
      ? contextWindow - OUTPUT_RESERVE_TOKENS - AUTOCOMPACT_BUFFER_TOKENS
      : contextWindow * 0.75;
  // A ceiling only ever brings the line forward.
  const threshold = typeof ceiling === "number" && ceiling > 0 ? Math.min(ofWindow, ceiling) : ofWindow;
  return estimatedTokens >= threshold;
}

export function estimateTextTokens(text: string, providerId?: string): number {
  if (!text) {
    return 0;
  }
  return getTokenEstimator(providerId).estimate(text);
}
