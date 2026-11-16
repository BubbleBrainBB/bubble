import { getAvailableThinkingLevels, normalizeThinkingLevel } from "./provider-transform.js";
import { getModelDefaultReasoningLevel } from "./model-catalog.js";
import { isClaudeWithBoundThinking } from "./anthropic-thinking.js";
import { RateLimitError, type RateLimitPolicy } from "./network/errors.js";
import { isProviderTransportError, normalizeProviderNetworkError, providerFetch } from "./network/provider-transport.js";
import {
  computeRetryDelayMs,
  getProviderMaxRetries,
  isRetryableHttpStatus,
  ProviderStreamInterruptedError,
  retryAfterMsFromResponse,
  retryLimitFor,
  sleepBeforeRetry,
} from "./network/retry.js";
import type { ContentPart, Provider, ProviderMessage, ProviderRawContentBlock, StreamChunk, ThinkingLevel, ToolChoiceMode, ToolDefinition, TokenUsage } from "./types.js";

const ANTHROPIC_VERSION = "2023-06-01";
const THINKING_BINDING_BETA = "thinking-binding-controls-2026-08-01";
const DEFAULT_MAX_TOKENS = 8192;
const ANTHROPIC_OPUS_LONG_OUTPUT_MAX_TOKENS = 128000;
const ANTHROPIC_LONG_OUTPUT_MAX_TOKENS = 64000;
// MiniMax thinking is always-on (M2.x) or default-on (M3), so the 8192 default
// gets consumed by thinking and the turn truncates before any visible text.
// MiniMax's docs recommend M3 128K, M2.x 64K output.
const MINIMAX_M3_MAX_TOKENS = 128000;
const MINIMAX_M2_MAX_TOKENS = 64000;
// MiMo counts thinking and visible text together; match its documented 128K cap.
const MIMO_MAX_TOKENS = 131072;
const ANTHROPIC_PROMPT_CACHE_CONTROL = { type: "ephemeral" } as const;
/** Hard API limit: a 5th cache_control breakpoint is a 400, not a warning. */
const ANTHROPIC_MAX_CACHE_BREAKPOINTS = 4;
const MINIMAX_PROMPT_CACHE_MODELS = new Set([
  "minimax-m2.7",
  "minimax-m2.7-highspeed",
  "minimax-m2.5",
  "minimax-m2.5-highspeed",
  "minimax-m2.1",
  "minimax-m2.1-highspeed",
  "minimax-m2",
  "m2-her",
]);

export interface AnthropicProviderOptions {
  providerId?: string;
  apiKey: string;
  baseURL: string;
  thinkingLevel?: ThinkingLevel;
  /** User-configured extra headers, merged last (client-identity gates). */
  headers?: Record<string, string>;
}

interface AnthropicRequest {
  model: string;
  max_tokens: number;
  messages: AnthropicMessage[];
  system?: string | AnthropicSystemBlock[];
  tools?: AnthropicTool[];
  tool_choice?: { type: "auto" | "any" | "none" };
  stream?: boolean;
  temperature?: number;
  thinking?: {
    type: "adaptive" | "enabled" | "disabled";
    display?: "summarized";
    block_binding?: { prefix_mismatch_behavior: "drop_block" };
  };
  output_config?: { effort: AnthropicEffort };
}

// Anthropic's reasoning-depth control (GA, no beta header). budget_tokens is
// removed on Opus 4.7+/Fable 5; effort is the replacement and rides in
// output_config (NOT top-level). There is no "minimal" on the Anthropic enum.
type AnthropicEffort = "low" | "medium" | "high" | "xhigh" | "max";

function anthropicEffortForLevel(level: ThinkingLevel): AnthropicEffort | undefined {
  switch (level) {
    case "off":
      return undefined;
    case "minimal":
    case "low":
      return "low";
    case "medium":
      return "medium";
    case "high":
      return "high";
    case "xhigh":
      return "xhigh";
    case "max":
      return "max";
    default:
      return undefined;
  }
}

type AnthropicCacheControl = typeof ANTHROPIC_PROMPT_CACHE_CONTROL;

interface AnthropicSystemBlock {
  type: "text";
  text: string;
  cache_control?: AnthropicCacheControl;
}

type AnthropicContentBlock =
  | { type: "text"; text: string; cache_control?: AnthropicCacheControl }
  | { type: "image"; source: { type: "url"; url: string } | { type: "base64"; media_type: string; data: string }; cache_control?: AnthropicCacheControl }
  | { type: "thinking"; thinking: string; signature?: string }
  | { type: "redacted_thinking"; data: string }
  | { type: "tool_use"; id: string; name: string; input: Record<string, unknown>; cache_control?: AnthropicCacheControl }
  | { type: "tool_result"; tool_use_id: string; content: string; is_error?: boolean; cache_control?: AnthropicCacheControl };

interface AnthropicMessage {
  role: "user" | "assistant";
  content: string | AnthropicContentBlock[];
}

interface AnthropicTool {
  name: string;
  description: string;
  input_schema: ToolDefinition["parameters"];
  cache_control?: AnthropicCacheControl;
}

interface AnthropicStreamBlockState {
  type: string;
  id?: string;
  name?: string;
  args: string;
  started: boolean;
  input?: Record<string, unknown>;
  raw: ProviderRawContentBlock;
  text: string;
  thinking: string;
  signature: string;
}

export function createAnthropicMessagesProvider(options: AnthropicProviderOptions): Provider {
  async function* streamChat(
    messages: ProviderMessage[],
    chatOptions: { model: string; tools?: ToolDefinition[]; toolChoice?: ToolChoiceMode; temperature?: number; thinkingLevel?: ThinkingLevel; abortSignal?: AbortSignal; rateLimitPolicy?: RateLimitPolicy },
  ): AsyncIterable<StreamChunk> {
    const body = buildAnthropicRequest(options, messages, {
      model: chatOptions.model,
      tools: chatOptions.tools,
      toolChoice: chatOptions.toolChoice,
      temperature: chatOptions.temperature,
      thinkingLevel: chatOptions.thinkingLevel,
      stream: true,
    });

    const events = streamAnthropicEventsWithRetry(options, {
      url: resolveAnthropicMessagesUrl(options.baseURL),
      model: body.model,
      stream: true,
      method: "POST",
      body: JSON.stringify(body),
      signal: chatOptions.abortSignal,
      rateLimitPolicy: chatOptions.rateLimitPolicy,
    });

    yield* translateAnthropicStream(events);
    yield { type: "done" };
  }

  async function complete(
    messages: ProviderMessage[],
    chatOptions?: { model?: string; temperature?: number; thinkingLevel?: ThinkingLevel; abortSignal?: AbortSignal },
  ): Promise<string> {
    const body = buildAnthropicRequest(options, messages, {
      model: chatOptions?.model ?? "claude-sonnet-4-6",
      temperature: chatOptions?.temperature,
      thinkingLevel: chatOptions?.thinkingLevel,
      stream: false,
    });

    const response = await fetchAnthropicResponseWithRetry(options, {
      url: resolveAnthropicMessagesUrl(options.baseURL),
      model: body.model,
      stream: false,
      method: "POST",
      body: JSON.stringify(body),
      signal: chatOptions?.abortSignal,
    });
    const data = await response.json() as { content?: Array<Record<string, unknown>>; input_transformations?: unknown };
    reportThinkingPrefixDrops(data.input_transformations);
    return extractAnthropicText(data.content).join("");
  }

  return { streamChat, complete };
}

export function buildAnthropicRequest(
  options: AnthropicProviderOptions,
  messages: ProviderMessage[],
  chatOptions: {
    model: string;
    tools?: ToolDefinition[];
    toolChoice?: ToolChoiceMode;
    temperature?: number;
    thinkingLevel?: ThinkingLevel;
    stream?: boolean;
  },
): AnthropicRequest {
  const mimo = isMiMoProvider(options);
  const modernClaude = isClaudeWithBoundThinking(chatOptions.model);
  const { system, messages: anthropicMessages } = toAnthropicMessages(
    messages, modernClaude ? "all" : shouldEchoThinking(options), !modernClaude,
  );
  const enablePromptCache = supportsAnthropicPromptCache(options, chatOptions.model);
  // MiMo only supports tool_choice=auto. Removing tools enforces "none".
  const tools: AnthropicTool[] | undefined = (mimo && chatOptions.toolChoice === "none" ? undefined : chatOptions.tools)?.map((tool) => ({
    name: tool.name,
    description: tool.description,
    input_schema: tool.parameters,
  }));
  // Anthropic caps a request at 4 cache_control breakpoints and rejects the
  // fifth outright, so the static ones are counted and the message region only
  // ever gets what is left.
  let staticBreakpoints = 0;
  if (enablePromptCache && tools && tools.length > 0) {
    tools[tools.length - 1] = {
      ...tools[tools.length - 1],
      cache_control: ANTHROPIC_PROMPT_CACHE_CONTROL,
    };
    staticBreakpoints += 1;
  }
  if (enablePromptCache && system) {
    staticBreakpoints += 1;
  }

  // Message-region breakpoints go to the official endpoint only. `baseURL` is
  // user-editable persisted config, so a corporate gateway can keep
  // providerId "anthropic" while pointing elsewhere — the URL check is the only
  // trustworthy signal. Streaming only: complete() has no next turn to read the
  // entry back.
  if (
    enablePromptCache
    && chatOptions.stream === true
    && isOfficialAnthropicBaseUrl(options.baseURL)
    && !messageCacheKillSwitchEnabled()
  ) {
    placeMessageCacheBreakpoints(
      anthropicMessages,
      ANTHROPIC_MAX_CACHE_BREAKPOINTS - staticBreakpoints,
    );
  }

  const effectiveThinkingLevel = normalizeThinkingLevel(
    chatOptions.thinkingLevel ?? options.thinkingLevel
      ?? (mimo ? "medium" : modernClaude || isClaude5WithOptionalThinking(chatOptions.model)
        ? getModelDefaultReasoningLevel(options.providerId || "", chatOptions.model) ?? "off" : "off"),
    mimo ? ["off", "medium"] : getAvailableThinkingLevels(options.providerId || "", chatOptions.model),
  );

  const body: AnthropicRequest = {
    model: chatOptions.model,
    max_tokens: resolveAnthropicMaxTokens(options, chatOptions.model),
    system: buildAnthropicSystem(system, enablePromptCache),
    messages: anthropicMessages,
    tools: tools && tools.length > 0 ? tools : undefined,
    tool_choice: tools && tools.length > 0 ? { type: mimo ? "auto" : chatOptions.toolChoice ?? "auto" } : undefined,
    stream: chatOptions.stream || undefined,
  };
  if (
    typeof chatOptions.temperature === "number"
    && shouldSendTemperature(options, chatOptions.model, effectiveThinkingLevel)
  ) {
    body.temperature = chatOptions.temperature;
  }

  if (mimo) {
    // MiMo defaults to thinking on and rejects Claude's adaptive mode/effort.
    body.thinking = { type: effectiveThinkingLevel === "off" ? "disabled" : "enabled" };
  } else if (effectiveThinkingLevel !== "off") {
    body.thinking = modernClaude ? { type: "adaptive", display: "summarized" } : { type: "adaptive" };
    if (modernClaude && isOfficialAnthropicBaseUrl(options.baseURL)) {
      // Checkpoints proactively reset thinking. Runtime reminders, restored
      // host context and tool changes can also change a prefix; let the API
      // drop invalid blocks instead of failing the turn. Applied on every
      // request so this behavior persists after restarts, per official guidance.
      body.thinking.block_binding = { prefix_mismatch_behavior: "drop_block" };
    }
    // Apply the selected reasoning depth via output_config.effort. Without this
    // every thinking request silently ran at Anthropic's default (high),
    // ignoring the chosen level. effort is an official-API feature, so only
    // send it to the official endpoint — anthropic-compatible third parties
    // (e.g. MiniMax) reject it. Levels are already clamped to the model's
    // supported set, so the value is always a valid effort for this model.
    if (isOfficialAnthropicBaseUrl(options.baseURL)) {
      const effort = anthropicEffortForLevel(effectiveThinkingLevel);
      if (effort) {
        body.output_config = { effort };
      }
    }
  }

  // Opus 5 and Sonnet 5 default to thinking on; omission cannot express Off.
  if (!mimo && effectiveThinkingLevel === "off" && isClaude5WithOptionalThinking(chatOptions.model)) {
    body.thinking = { type: "disabled" };
  }
  return body;
}

/**
 * Attach `cache_control` to the last cacheable block of `message`.
 *
 * Returns false when the message has nothing markable (empty block array, or a
 * body made only of thinking blocks, where `cache_control` is not permitted).
 */
function markLastCacheableBlock(message: AnthropicMessage): boolean {
  if (typeof message.content === "string") {
    if (!message.content) return false;
    // A breakpoint needs a block to sit on. Wrapping is safe: the API keys the
    // cache on canonicalised content, so the same message reverting to a bare
    // string on a later turn still matches this entry.
    message.content = [{
      type: "text",
      text: message.content,
      cache_control: ANTHROPIC_PROMPT_CACHE_CONTROL,
    }];
    return true;
  }

  const blocks = message.content;
  for (let index = blocks.length - 1; index >= 0; index--) {
    const block = blocks[index];
    if (block.type === "thinking" || block.type === "redacted_thinking") continue;
    blocks[index] = { ...block, cache_control: ANTHROPIC_PROMPT_CACHE_CONTROL };
    return true;
  }
  return false;
}

/**
 * Roll prompt-cache breakpoints along the conversation so each turn's context is
 * read back on the next turn instead of being re-billed as fresh input.
 *
 * Two breakpoints, and both are load-bearing:
 *
 * - the **tail** (last block of the last message) writes the entry that the next
 *   request reads;
 * - the **anchor** reads it. It goes on the message just before the final
 *   assistant message — which is exactly where the previous request ended, since
 *   consecutive agent-loop requests differ by one `[assistant, tool results]`
 *   step. Landing on the written position directly is what makes this correct
 *   for any turn size: Anthropic only looks back 20 content blocks from a
 *   breakpoint to find a prior write, and a turn with many parallel tool calls
 *   easily exceeds that (1 + 2N blocks for N calls). Measured on Opus 4.8 with
 *   30 parallel calls: tail-only and a fixed-distance anchor both collapse to an
 *   8.6% hit rate, while this anchor holds 77%.
 *
 * Mutates `messages` (built fresh per request by toAnthropicMessages) and
 * returns how many breakpoints it used, never more than `budget`.
 */
export function placeMessageCacheBreakpoints(messages: AnthropicMessage[], budget: number): number {
  // One-shot prompts (complete(): compaction, titles, memory) are excluded by
  // the caller's streaming gate, not here — a single-message first turn is a
  // real agent turn and marking its tail costs nothing, since writes are billed
  // from the highest hit to the last breakpoint regardless of how many sit in
  // between.
  if (budget <= 0 || messages.length === 0) return 0;

  let used = 0;
  const tailIndex = messages.length - 1;

  if (budget - used >= 2) {
    let lastAssistantIndex = -1;
    for (let index = tailIndex; index >= 0; index--) {
      if (messages[index].role === "assistant") {
        lastAssistantIndex = index;
        break;
      }
    }
    const anchorIndex = lastAssistantIndex - 1;
    // An assistant at the anchor position means consecutive assistant messages
    // merged, so this is not the previous request's boundary and the distance
    // back is unbounded — skip rather than guess.
    if (
      anchorIndex >= 0
      && anchorIndex < tailIndex
      && messages[anchorIndex].role !== "assistant"
      && markLastCacheableBlock(messages[anchorIndex])
    ) {
      used += 1;
    }
  }

  if (budget - used >= 1 && markLastCacheableBlock(messages[tailIndex])) {
    used += 1;
  }
  return used;
}

function messageCacheKillSwitchEnabled(): boolean {
  const raw = process.env.BUBBLE_ANTHROPIC_MESSAGE_CACHE;
  return raw === "0" || raw === "false";
}

export function resolveAnthropicMaxTokens(options: AnthropicProviderOptions, model: string): number {
  if (isMiMoProvider(options)) return MIMO_MAX_TOKENS;
  // MiniMax needs a large output budget so thinking doesn't starve the answer.
  if (isMiniMaxProvider(options)) {
    return /m3/i.test(model) ? MINIMAX_M3_MAX_TOKENS : MINIMAX_M2_MAX_TOKENS;
  }
  if (!isOfficialAnthropicBaseUrl(options.baseURL)) {
    return DEFAULT_MAX_TOKENS;
  }

  if (isFableModelWith128kOutput(model) || isOpusModelWith128kOutput(model)
    || isClaudeFamilyVersionAtLeast(model, "sonnet", 5, 0)) {
    return ANTHROPIC_OPUS_LONG_OUTPUT_MAX_TOKENS;
  }

  if (isSonnetOrHaikuModelWith64kOutput(model)) {
    return ANTHROPIC_LONG_OUTPUT_MAX_TOKENS;
  }

  return DEFAULT_MAX_TOKENS;
}

function buildAnthropicSystem(system: string, enablePromptCache: boolean): AnthropicRequest["system"] {
  if (!system) return undefined;
  if (!enablePromptCache) return system;
  return [{ type: "text", text: system, cache_control: ANTHROPIC_PROMPT_CACHE_CONTROL }];
}

export function supportsAnthropicPromptCache(options: AnthropicProviderOptions, model: string): boolean {
  const providerId = (options.providerId ?? "").toLowerCase();
  if (providerId === "anthropic" || isOfficialAnthropicBaseUrl(options.baseURL)) {
    return true;
  }
  if (!isMiniMaxAnthropicEndpoint(options)) {
    return false;
  }
  return MINIMAX_PROMPT_CACHE_MODELS.has(model.toLowerCase());
}

export function toAnthropicMessages(
  messages: ProviderMessage[],
  echoThinking: boolean | "all" = false,
  allowUnsignedThinking = true,
): { system: string; messages: AnthropicMessage[] } {
  const system: string[] = [];
  const out: AnthropicMessage[] = [];
  const thinkingReplayIndexes = getThinkingReplayIndexes(messages, echoThinking);

  for (let index = 0; index < messages.length; index++) {
    const message = messages[index];
    if (message.role === "system") {
      system.push(message.content);
      continue;
    }

    if (message.role === "tool") {
      pushAnthropicMessage(out, {
        role: "user",
        content: [{
          type: "tool_result",
          tool_use_id: message.toolCallId,
          content: message.content,
          ...(message.isError ? { is_error: true } : {}),
        }],
      });
      continue;
    }

    if (message.role === "assistant") {
      const content = buildAssistantAnthropicBlocks(message, thinkingReplayIndexes.has(index), allowUnsignedThinking);
      if (content.length > 0) {
        pushAnthropicMessage(out, { role: "assistant", content });
      }
      continue;
    }

    pushAnthropicMessage(out, {
      role: "user",
      content: typeof message.content === "string"
        ? message.content
        : contentPartsToAnthropicBlocks(message.content),
    });
  }

  return { system: system.join("\n\n"), messages: out };
}

function buildAssistantAnthropicBlocks(message: Extract<ProviderMessage, { role: "assistant" }>, includeThinking: boolean, allowUnsignedThinking: boolean): AnthropicContentBlock[] {
  const rawBlocks = message.providerMetadata?.anthropic?.contentBlocks;
  if (rawBlocks && rawBlocks.length > 0) {
    const blocks = rawBlocks
      .filter(isReplayableAssistantContentBlock)
      .filter((block) => includeThinking || !isThinkingContentBlock(block))
      .map((block) => cloneAnthropicContentBlock(block));
    if (blocks.length > 0) {
      return blocks;
    }
  }

  const content: AnthropicContentBlock[] = [];
  if (includeThinking && allowUnsignedThinking && message.reasoning?.trim()) {
    content.push({ type: "thinking", thinking: message.reasoning });
  }
  if (message.content.trim()) {
    content.push({ type: "text", text: message.content });
  }
  for (const toolCall of message.toolCalls ?? []) {
    content.push({
      type: "tool_use",
      id: toolCall.id,
      name: toolCall.name,
      input: parseToolInput(toolCall.arguments),
    });
  }
  return content;
}

function getThinkingReplayIndexes(messages: ProviderMessage[], echoThinking: boolean | "all"): Set<number> {
  const indexes = new Set<number>();
  if (!echoThinking) return indexes;
  // MiMo and bound Claude thinking need intact historical blocks across user
  // turns. Removing a middle block breaks the later Claude signature chain.
  if (echoThinking === "all") {
    messages.forEach((message, index) => {
      if (message.role === "assistant") indexes.add(index);
    });
    return indexes;
  }

  let lastUserIndex = -1;
  for (let index = 0; index < messages.length; index++) {
    if (messages[index].role === "user") {
      lastUserIndex = index;
    }
  }

  for (let index = Math.max(0, lastUserIndex + 1); index < messages.length; index++) {
    const message = messages[index];
    if (message.role === "assistant" && assistantHasToolUse(message)) {
      indexes.add(index);
    }
  }

  return indexes;
}

function assistantHasToolUse(message: Extract<ProviderMessage, { role: "assistant" }>): boolean {
  if (message.toolCalls && message.toolCalls.length > 0) return true;
  return message.providerMetadata?.anthropic?.contentBlocks?.some((block) => block.type === "tool_use") ?? false;
}

function isThinkingContentBlock(block: ProviderRawContentBlock): boolean {
  return block.type === "thinking" || block.type === "redacted_thinking";
}

function isReplayableAssistantContentBlock(block: ProviderRawContentBlock): boolean {
  switch (block.type) {
    case "text":
      return typeof block.text === "string";
    case "thinking":
      return typeof block.thinking === "string";
    case "redacted_thinking":
      return typeof block.data === "string";
    case "tool_use":
      return typeof block.id === "string" && typeof block.name === "string" && isObjectRecord(block.input);
    default:
      return false;
  }
}

function cloneAnthropicContentBlock(block: ProviderRawContentBlock): AnthropicContentBlock {
  return JSON.parse(JSON.stringify(block)) as AnthropicContentBlock;
}

export async function* translateAnthropicStream(events: AsyncIterable<Record<string, unknown>>): AsyncIterable<StreamChunk> {
  const blocks = new Map<number, AnthropicStreamBlockState>();
  let usage: TokenUsage | undefined;

  for await (const event of events) {
    const type = typeof event.type === "string" ? event.type : "";
    if (type === "message_start") {
      reportThinkingPrefixDrops((event.message as Record<string, unknown> | undefined)?.input_transformations);
      usage = mergeAnthropicUsage(usage, (event.message as Record<string, unknown> | undefined)?.usage);
      if (usage) yield { type: "usage", usage };
      continue;
    }
    if (type === "message_delta") {
      usage = mergeAnthropicUsage(usage, event.usage);
      if (usage) yield { type: "usage", usage };
      continue;
    }
    if (type === "error") {
      const err = event.error as Record<string, unknown> | undefined;
      throw new Error(`Anthropic stream error: ${String(err?.message || err?.type || "unknown error")}`);
    }
    if (type === "content_block_start") {
      const index = typeof event.index === "number" ? event.index : 0;
      const block = event.content_block as Record<string, unknown> | undefined;
      const blockType = typeof block?.type === "string" ? block.type : "";
      const raw = cloneProviderBlock(block, blockType);
      const state: AnthropicStreamBlockState = {
        type: blockType,
        id: typeof block?.id === "string" ? block.id : undefined,
        name: typeof block?.name === "string" ? block.name : undefined,
        args: "",
        started: false,
        input: isObjectRecord(block?.input) ? block.input : undefined,
        raw,
        text: typeof block?.text === "string" ? block.text : "",
        thinking: typeof block?.thinking === "string" ? block.thinking : "",
        signature: typeof block?.signature === "string" ? block.signature : "",
      };
      blocks.set(index, state);
      if (blockType === "text" && typeof block?.text === "string" && block.text) {
        yield { type: "text", content: block.text };
      }
      if (blockType === "thinking" && typeof block?.thinking === "string" && block.thinking) {
        yield { type: "reasoning_delta", content: block.thinking };
      }
      if (blockType === "tool_use" && state.id && state.name) {
        state.started = true;
        yield { type: "tool_call", id: state.id, name: state.name, arguments: "", isStart: true, isEnd: false };
      }
      continue;
    }
    if (type === "content_block_delta") {
      const index = typeof event.index === "number" ? event.index : 0;
      const state = blocks.get(index);
      const delta = event.delta as Record<string, unknown> | undefined;
      const deltaType = typeof delta?.type === "string" ? delta.type : "";
      if (deltaType === "text_delta" && typeof delta?.text === "string" && delta.text) {
        if (state) {
          state.text += delta.text;
          state.raw.text = state.text;
        }
        yield { type: "text", content: delta.text };
      } else if (deltaType === "thinking_delta" && typeof delta?.thinking === "string" && delta.thinking) {
        if (state) {
          state.thinking += delta.thinking;
          state.raw.thinking = state.thinking;
        }
        yield { type: "reasoning_delta", content: delta.thinking };
      } else if (deltaType === "signature_delta" && typeof delta?.signature === "string" && state) {
        state.signature += delta.signature;
        state.raw.signature = state.signature;
      } else if (deltaType === "input_json_delta" && state?.id && state.name && typeof delta?.partial_json === "string") {
        state.args += delta.partial_json;
        if (!state.started) {
          state.started = true;
          yield { type: "tool_call", id: state.id, name: state.name, arguments: "", isStart: true, isEnd: false };
        }
        if (delta.partial_json) {
          yield { type: "tool_call", id: state.id, name: state.name, arguments: delta.partial_json, isStart: false, isEnd: false };
        }
      }
      continue;
    }
    if (type === "content_block_stop") {
      const index = typeof event.index === "number" ? event.index : 0;
      const state = blocks.get(index);
      blocks.delete(index);
      if (state?.type === "tool_use" && state.id && state.name) {
        const finalArgs = state.args || JSON.stringify(state.input ?? {});
        state.raw.input = parseToolInput(normalizeToolArgs(finalArgs));
        yield { type: "provider_content_block", provider: "anthropic", block: state.raw };
        yield {
          type: "tool_call",
          id: state.id,
          name: state.name,
          arguments: "",
          argumentsFull: normalizeToolArgs(finalArgs),
          isStart: false,
          isEnd: true,
        };
      } else if (state && isReplayableAssistantContentBlock(state.raw)) {
        finalizeRawContentBlock(state);
        yield { type: "provider_content_block", provider: "anthropic", block: state.raw };
      }
    }
  }
}

export async function* readSseEvents(response: Response): AsyncIterable<Record<string, unknown>> {
  if (!response.body) {
    throw new Error("Anthropic Messages API returned an empty stream body.");
  }
  const reader = response.body.getReader();
  const decoder = new TextDecoder();
  let buffer = "";

  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      buffer += decoder.decode(value, { stream: true });
      let separator = buffer.indexOf("\n\n");
      while (separator >= 0) {
        const raw = buffer.slice(0, separator);
        buffer = buffer.slice(separator + 2);
        const event = parseSseEvent(raw);
        if (event) yield event;
        separator = buffer.indexOf("\n\n");
      }
    }
    buffer += decoder.decode();
    const event = parseSseEvent(buffer);
    if (event) yield event;
  } finally {
    reader.releaseLock();
  }
}

async function* streamAnthropicEventsWithRetry(
  options: AnthropicProviderOptions,
  request: {
    url: string;
    model: string;
    stream: true;
    method: "POST";
    body: string;
    signal?: AbortSignal;
    rateLimitPolicy?: RateLimitPolicy;
  },
): AsyncIterable<Record<string, unknown>> {
  const maxRetries = getProviderMaxRetries();

  for (let attempt = 0; ; attempt++) {
    // Connection-level failures and retryable HTTP statuses are retried
    // inside fetchAnthropicResponseWithRetry; an error thrown from it has
    // already exhausted its budget, so it propagates without another loop.
    const response = await fetchAnthropicResponseWithRetry(options, request);

    let sawSseEvent = false;
    try {
      for await (const event of readSseEvents(response)) {
        sawSseEvent = true;
        yield event;
      }
      return;
    } catch (error) {
      const normalized = normalizeAnthropicTransportError(error, request.url);
      if (sawSseEvent) {
        // Partial content already surfaced — only the agent loop can discard
        // the half-built assistant message and safely re-issue the request.
        if (!request.signal?.aborted && isProviderTransportError(error)) {
          throw new ProviderStreamInterruptedError(normalized.message, { cause: normalized });
        }
        throw normalized;
      }
      if (request.signal?.aborted || attempt >= retryLimitFor(error, maxRetries) || !isProviderTransportError(error)) {
        throw normalized;
      }
      await sleepBeforeRetry(computeRetryDelayMs(attempt + 1), request.signal);
    }
  }
}

async function fetchAnthropicResponseWithRetry(
  options: AnthropicProviderOptions,
  request: {
    url: string;
    model: string;
    stream: boolean;
    method: "POST";
    body: string;
    signal?: AbortSignal;
    rateLimitPolicy?: RateLimitPolicy;
  },
): Promise<Response> {
  const maxRetries = getProviderMaxRetries();

  for (let attempt = 0; ; attempt++) {
    let response: Response;
    try {
      response = await providerFetch(request.url, {
        method: request.method,
        headers: buildAnthropicHeaders(options, request.stream, request.model),
        body: request.body,
        signal: request.signal,
        keepalive: false,
      }, {
        providerName: "Anthropic",
        verboseEnvVar: "BUBBLE_ANTHROPIC_FETCH_VERBOSE",
      });
    } catch (error) {
      // No response received, so the request is safe to re-issue.
      if (request.signal?.aborted || attempt >= retryLimitFor(error, maxRetries) || !isProviderTransportError(error)) {
        throw normalizeAnthropicTransportError(error, request.url);
      }
      await sleepBeforeRetry(computeRetryDelayMs(attempt + 1), request.signal);
      continue;
    }
    if (response.ok) return response;

    const detail = await readAnthropicErrorDetail(response);

    // Rate-limit contract (design §4.5): under "defer" the transport performs
    // no 429 backoff and throws the typed error immediately; under "handle"
    // an exhausted 429 retry budget still surfaces as the typed error so the
    // caller can recognize it without string matching.
    if (response.status === 429) {
      const retryAfterMs = retryAfterMsFromResponse(response);
      if (request.rateLimitPolicy === "defer") {
        throw new RateLimitError(`Anthropic Messages API rate limited (429): ${detail || response.statusText}`, {
          status: 429,
          retryAfterMs,
        });
      }
      if (request.signal?.aborted || attempt >= maxRetries) {
        throw new RateLimitError(`Anthropic Messages API rate limited (429) after ${attempt + 1} attempts: ${detail || response.statusText}`, {
          status: 429,
          retryAfterMs,
        });
      }
      await sleepBeforeRetry(computeRetryDelayMs(attempt + 1, { retryAfterMs }), request.signal);
      continue;
    }

    const error = new Error(`Anthropic Messages API error ${response.status}: ${detail || response.statusText}`);

    if (request.signal?.aborted || attempt >= maxRetries || !isRetryableAnthropicHttpError(response.status, detail)) {
      throw error;
    }

    await sleepBeforeRetry(
      computeRetryDelayMs(attempt + 1, { retryAfterMs: retryAfterMsFromResponse(response) }),
      request.signal,
    );
  }
}

function resolveAnthropicMessagesUrl(baseURL: string): string {
  const normalized = baseURL.trim().replace(/\/+$/, "");
  if (normalized.endsWith("/v1/messages")) return normalized;
  if (normalized.endsWith("/v1")) return `${normalized}/messages`;
  return `${normalized || "https://api.anthropic.com"}/v1/messages`;
}

function buildAnthropicHeaders(options: AnthropicProviderOptions, stream: boolean, model: string): HeadersInit {
  const headers: Record<string, string> = {
    "content-type": "application/json",
    "x-api-key": options.apiKey,
    "anthropic-version": ANTHROPIC_VERSION,
  };
  if (shouldSendBearerAuth(options)) {
    headers.authorization = `Bearer ${options.apiKey}`;
  }
  if (stream) headers.accept = "text/event-stream";
  // User-configured headers win last: anthropic-protocol coding plans gate on
  // client identity (e.g. a specific User-Agent), configured per provider.
  const merged = { ...headers, ...(options.headers ?? {}) };
  if (isOfficialAnthropicBaseUrl(options.baseURL) && isClaudeWithBoundThinking(model)) {
    // Merge case-insensitively without losing user-configured beta features.
    const betaKeys = Object.keys(merged).filter(key => key.toLowerCase() === "anthropic-beta");
    const betas = betaKeys.flatMap(key => merged[key].split(",").map(value => value.trim()).filter(Boolean));
    for (const key of betaKeys) delete merged[key];
    merged["anthropic-beta"] = [...new Set([...betas, THINKING_BINDING_BETA])].join(",");
  }
  return merged;
}

function reportThinkingPrefixDrops(transformations: unknown): void {
  if (!Array.isArray(transformations)) return;
  const count = transformations.filter(item => item?.type === "thinking_dropped" && item?.reason === "prefix_binding_mismatch").length;
  if (count > 0) {
    // Count only; raw transformations, signatures and prompt text are private.
    console.warn(`[anthropic-thinking] API discarded ${count} thinking block(s) after a conversation prefix change.`);
  }
}

async function readAnthropicErrorDetail(response: Response): Promise<string> {
  try {
    return await response.text();
  } catch {
    return response.statusText;
  }
}

function normalizeAnthropicTransportError(error: unknown, url: string): Error {
  if (!isProviderTransportError(error)) {
    return error instanceof Error ? error : new Error(String(error));
  }
  return normalizeProviderNetworkError(error, {
    providerName: "Anthropic",
    input: url,
  });
}

function isRetryableAnthropicHttpError(status: number, detail: string): boolean {
  // "714 (1000)" is a transient MiniMax backend error surfaced as a 500.
  return isRetryableHttpStatus(status) || detail.includes("714 (1000)");
}

function parseSseEvent(raw: string): Record<string, unknown> | undefined {
  const dataLines: string[] = [];
  for (const line of raw.split(/\r?\n/)) {
    if (!line || line.startsWith(":")) continue;
    if (line.startsWith("data:")) {
      dataLines.push(line.slice(5).trimStart());
    }
  }
  if (dataLines.length === 0) return undefined;
  const data = dataLines.join("\n");
  if (!data || data === "[DONE]") return undefined;
  return JSON.parse(data) as Record<string, unknown>;
}

function cloneProviderBlock(block: Record<string, unknown> | undefined, fallbackType: string): ProviderRawContentBlock {
  const type = typeof block?.type === "string" && block.type ? block.type : fallbackType || "unknown";
  const clone = block ? JSON.parse(JSON.stringify(block)) as Record<string, unknown> : {};
  clone.type = type;
  return clone as ProviderRawContentBlock;
}

function finalizeRawContentBlock(state: AnthropicStreamBlockState): void {
  if (state.type === "text") {
    state.raw.text = state.text;
  } else if (state.type === "thinking") {
    state.raw.thinking = state.thinking;
    if (state.signature) {
      state.raw.signature = state.signature;
    }
  }
}

function contentPartsToAnthropicBlocks(parts: ContentPart[]): AnthropicContentBlock[] {
  const blocks: AnthropicContentBlock[] = [];
  for (const part of parts) {
    if (part.type === "text") {
      blocks.push({ type: "text", text: part.text });
      continue;
    }
    const image = part.image_url.url;
    const dataUrlMatch = image.match(/^data:([^;,]+);base64,(.+)$/);
    if (dataUrlMatch) {
      blocks.push({
        type: "image",
        source: {
          type: "base64",
          media_type: dataUrlMatch[1],
          data: dataUrlMatch[2],
        },
      });
    } else {
      blocks.push({ type: "image", source: { type: "url", url: image } });
    }
  }
  return blocks;
}

function pushAnthropicMessage(messages: AnthropicMessage[], next: AnthropicMessage): void {
  const last = messages.at(-1);
  if (!last || last.role !== next.role) {
    messages.push(next);
    return;
  }
  last.content = mergeAnthropicContent(last.content, next.content);
}

function mergeAnthropicContent(
  current: string | AnthropicContentBlock[],
  next: string | AnthropicContentBlock[],
): string | AnthropicContentBlock[] {
  const currentBlocks = typeof current === "string" ? [{ type: "text", text: current } satisfies AnthropicContentBlock] : current;
  const nextBlocks = typeof next === "string" ? [{ type: "text", text: next } satisfies AnthropicContentBlock] : next;
  return [...currentBlocks, ...nextBlocks];
}

function parseToolInput(raw: string): Record<string, unknown> {
  try {
    const parsed = JSON.parse(raw || "{}");
    return isObjectRecord(parsed) ? parsed : {};
  } catch {
    return {};
  }
}

function normalizeToolArgs(raw: string): string {
  try {
    JSON.parse(raw);
    return raw;
  } catch {
    return "{}";
  }
}

function isObjectRecord(value: unknown): value is Record<string, unknown> {
  return !!value && typeof value === "object" && !Array.isArray(value);
}

function extractAnthropicText(content: Array<Record<string, unknown>> | undefined): string[] {
  if (!content) return [];
  return content.flatMap((block) => block.type === "text" && typeof block.text === "string" ? [block.text] : []);
}

function mergeAnthropicUsage(current: TokenUsage | undefined, raw: unknown): TokenUsage | undefined {
  if (!isObjectRecord(raw)) return current;
  const rawInput = typeof raw.input_tokens === "number" ? raw.input_tokens : undefined;
  const rawCacheRead = typeof raw.cache_read_input_tokens === "number" ? raw.cache_read_input_tokens : undefined;
  const rawCacheCreation = typeof raw.cache_creation_input_tokens === "number" ? raw.cache_creation_input_tokens : undefined;
  const outputTokens = typeof raw.output_tokens === "number" ? raw.output_tokens : current?.completionTokens ?? 0;
  const hasPromptUsage = rawInput !== undefined || rawCacheRead !== undefined || rawCacheCreation !== undefined;

  let promptTokens = current?.promptTokens ?? 0;
  let promptCacheHitTokens = current?.promptCacheHitTokens;
  let promptCacheMissTokens = current?.promptCacheMissTokens;
  let cacheCreationTokens = current?.cacheCreationTokens;
  if (hasPromptUsage) {
    // Every field carries forward: a message_delta may report one of the three
    // without the others, and defaulting a missing one to 0 would deflate
    // promptTokens — which is the compaction anchor (agent lastInputTokens),
    // so an undercount makes the agent compact later than it should.
    // promptCacheMissTokens already includes cache creation, so the prior
    // uncached remainder has to net it back out before being reused.
    const priorUncachedInput = promptCacheMissTokens !== undefined
      ? Math.max(0, promptCacheMissTokens - (cacheCreationTokens ?? 0))
      : promptTokens;
    const inputTokens = rawInput ?? priorUncachedInput;
    const cacheRead = rawCacheRead ?? promptCacheHitTokens ?? 0;
    const cacheCreation = rawCacheCreation ?? cacheCreationTokens ?? 0;
    promptTokens = inputTokens + cacheRead + cacheCreation;
    promptCacheHitTokens = cacheRead;
    promptCacheMissTokens = inputTokens + cacheCreation;
    cacheCreationTokens = cacheCreation;
  }

  return {
    promptTokens,
    completionTokens: outputTokens,
    promptCacheHitTokens,
    promptCacheMissTokens,
    cacheCreationTokens,
    totalTokens: promptTokens + outputTokens,
  };
}

function shouldEchoThinking(options: AnthropicProviderOptions): boolean | "all" {
  if (isMiMoProvider(options)) return "all";
  return options.providerId?.startsWith("minimax") ?? false;
}

function shouldSendBearerAuth(options: AnthropicProviderOptions): boolean {
  return !isOfficialAnthropicBaseUrl(options.baseURL) || options.providerId?.startsWith("minimax") === true;
}

function shouldSendTemperature(options: AnthropicProviderOptions, model: string, thinkingLevel: ThinkingLevel): boolean {
  if (isMiMoProvider(options) && thinkingLevel !== "off") return false;
  if (!isOfficialAnthropicBaseUrl(options.baseURL)) return true;
  if (thinkingLevel !== "off") return false;
  return !isOpusModelWithoutSamplingControls(model)
    && !isFableModelWith128kOutput(model)
    && !isClaudeFamilyVersionAtLeast(model, "sonnet", 5, 0);
}

function isClaude5WithOptionalThinking(model: string): boolean {
  return isClaudeFamilyVersionAtLeast(model, "sonnet", 5, 0)
    || (isClaudeFamilyVersionAtLeast(model, "opus", 5, 0) && !isClaudeWithBoundThinking(model));
}

function isOpusModelWith128kOutput(model: string): boolean {
  return isClaudeFamilyVersionAtLeast(model, "opus", 4, 6);
}

function isFableModelWith128kOutput(model: string): boolean {
  return model.toLowerCase().startsWith("claude-fable-5");
}

function isSonnetOrHaikuModelWith64kOutput(model: string): boolean {
  const normalized = model.toLowerCase();
  return normalized.startsWith("claude-sonnet-4-6")
    || normalized.startsWith("claude-haiku-4-5");
}

function isOpusModelWithoutSamplingControls(model: string): boolean {
  return isClaudeFamilyVersionAtLeast(model, "opus", 4, 7);
}

function isClaudeFamilyVersionAtLeast(model: string, family: string, minMajor: number, minMinor: number): boolean {
  const normalized = model.toLowerCase();
  if (!normalized.startsWith(`claude-${family}-`)) return false;

  const [, , majorSegment, minorSegment] = normalized.split("-");
  const major = Number(majorSegment);
  if (!Number.isFinite(major)) return false;
  if (major > minMajor) return true;
  if (major < minMajor) return false;

  if (!minorSegment || minorSegment.length > 2) return minMinor === 0;
  const minor = Number(minorSegment);
  return Number.isFinite(minor) && minor >= minMinor;
}

function isMiniMaxAnthropicEndpoint(options: AnthropicProviderOptions): boolean {
  const providerId = (options.providerId ?? "").toLowerCase();
  if (providerId !== "minimax" && providerId !== "minimax-anthropic") return false;
  try {
    const url = new URL(options.baseURL);
    const host = url.hostname.toLowerCase();
    const path = url.pathname.toLowerCase();
    return (host === "api.minimax.io" || host === "api.minimaxi.com") && path.includes("/anthropic");
  } catch {
    return false;
  }
}

function isOfficialAnthropicBaseUrl(baseURL: string): boolean {
  try {
    return new URL(baseURL).hostname === "api.anthropic.com";
  } catch {
    return false;
  }
}

function isMiniMaxProvider(options: AnthropicProviderOptions): boolean {
  return (options.providerId || "").toLowerCase().startsWith("minimax")
    || options.baseURL.toLowerCase().includes("minimaxi");
}

function isMiMoProvider(options: AnthropicProviderOptions): boolean {
  if (options.providerId === "mimo-token-plan") return true;
  try {
    return ["api.xiaomimimo.com", "token-plan-cn.xiaomimimo.com", "token-plan-sgp.xiaomimimo.com", "token-plan-ams.xiaomimimo.com"]
      .includes(new URL(options.baseURL).hostname);
  } catch {
    return false;
  }
}
