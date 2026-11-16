/**
 * OpenAI-compatible Provider implementation.
 *
 * Works with OpenRouter, OpenAI, DeepSeek, Google, Groq, Together, and local OpenAI-compatible endpoints.
 */

import OpenAI from "openai";
import { appendFileSync } from "node:fs";
import { createAnthropicMessagesProvider } from "./provider-anthropic.js";
import { createArkResponsesProvider, createOpenAIResponsesProvider } from "./provider-ark-responses.js";
import { createAiSdkProvider } from "./provider-ai-sdk.js";
import { createOpenAICodexProvider, isOpenAICodexBaseUrl, type OpenAICodexAuthAdapter } from "./provider-openai-codex.js";
import {
  buildGrokSubscriptionHeaders,
  createGrokSubscriptionFetch,
  isGrokSubscriptionBaseUrl,
  type GrokAuthAdapter,
} from "./provider-grok.js";
import { getChatGptFetch } from "./network/chatgpt-transport.js";
import { createProviderFetch, type ProviderFetch } from "./network/provider-transport.js";
import { withChatStreamCompletionCheck } from "./network/chat-stream-completion.js";
import { createPlainProviderFetch } from "./network/plain-provider-fetch.js";
import { resolveCompletionDeadlineMs, resolveIdleTimeoutMs, resolveRequestTimeoutMs } from "./network/request-timeout.js";
import { chatRequestShape, isSpaceBunnyUpstreamRejection, recoverableSpaceBunnyError } from "./provider-zen-errors.js";
import { createProviderProtocolArtifactFilter } from "./provider-artifacts.js";
import { resolveProviderRequestConfig } from "./provider-transform.js";
import { debugReasoningStream, summarizeDebugText } from "./reasoning-debug.js";
import {
  isProviderResponseError,
  ProviderResponseError,
  RateLimitError,
  type RateLimitPolicy,
} from "./network/errors.js";
import { computeRetryDelayMs, isRetryableHttpStatus, ProviderStreamInterruptedError, sleepBeforeRetry } from "./network/retry.js";
import type { ProviderProtocol } from "./model-catalog.js";
import type { Provider, ProviderMessage, StreamChunk, ThinkingLevel, ToolChoiceMode, ToolDefinition } from "./types.js";
import { assertProviderModelAllowed } from "./provider-model-policy.js";

export { MAX_TIMER_MS, resolveRequestTimeoutMs } from "./network/request-timeout.js";

// Diagnostic logger for tool-args byte-loss investigation. Activate with
//   BUBBLE_DEBUG_TOOL_ARGS=/path/to/log.jsonl   (any writable path)
// Each line is a JSON record describing a transition. When debugging is off,
// the function is a no-op and free.
const TOOL_ARGS_DEBUG_PATH = process.env.BUBBLE_DEBUG_TOOL_ARGS?.trim();
function debugToolArgs(event: Record<string, unknown>): void {
  if (!TOOL_ARGS_DEBUG_PATH) return;
  try {
    appendFileSync(TOOL_ARGS_DEBUG_PATH, JSON.stringify({ t: Date.now(), ...event }) + "\n", "utf-8");
  } catch {
    // Diagnostic failures must not affect the model session.
  }
}

type ReasoningContentEcho = "tool_calls" | "all" | "none" | "minimax";
export type ToolArgsMergeMode = "delta" | "snapshot";

export interface TranslateOpenAIStreamOptions {
  toolArgsMergeMode?: ToolArgsMergeMode;
  reasoningMergeMode?: ToolArgsMergeMode;
  textMergeMode?: ToolArgsMergeMode;
  debugProviderId?: string;
  debugModelId?: string;
}

export function toChatCompletionsMessage(
  message: ProviderMessage,
  options: { reasoningContentEcho?: ReasoningContentEcho } = {},
): Record<string, unknown> {
  const reasoningContentEcho = options.reasoningContentEcho ?? "tool_calls";
  if (message.role === "assistant") {
    const out: Record<string, unknown> = {
      role: "assistant",
      content: message.content || null,
    };
    if (reasoningContentEcho === "all") {
      // DeepSeek thinking mode requires every assistant message to echo the
      // provider field, even when the original value is an empty string.
      out.reasoning_content = message.reasoning ?? "";
    }
    if (reasoningContentEcho === "minimax" && message.reasoning) {
      out.reasoning_details = [{
        type: "reasoning.text",
        id: "reasoning-text-1",
        format: "MiniMax-response-v1",
        index: 0,
        text: message.reasoning,
      }];
    }
    if (message.toolCalls && message.toolCalls.length > 0) {
      out.tool_calls = message.toolCalls.map((tc) => ({
        id: tc.id,
        type: "function",
        function: { name: tc.name, arguments: tc.arguments || "{}" },
      }));
      // Kimi-k2.5 with thinking enabled requires reasoning_content to be echoed
      // back on assistant messages that carry tool_calls.
      if (message.reasoning && reasoningContentEcho === "tool_calls") {
        out.reasoning_content = message.reasoning;
      }
    }
    return out;
  }
  if (message.role === "tool") {
    return { role: "tool", tool_call_id: message.toolCallId, content: message.content };
  }
  return { role: message.role, content: message.content };
}

export interface ProviderInstanceOptions {
  providerId?: string;
  apiKey: string;
  baseURL: string;
  /** Requested thinking level */
  thinkingLevel?: ThinkingLevel;
  /** Stable per-session seed for provider prompt caches. */
  promptCacheKey?: string;
  protocol?: ProviderProtocol;
  /**
   * User-configured extra request headers (provider entry `headers` in
   * config.json/models.json). Merged over protocol defaults, so a coding-plan
   * endpoint's client-identity gate (usually a User-Agent allowlist) can be
   * satisfied from config.
   */
  headers?: Record<string, string>;
  /** Dynamic OAuth access-token loader/refresh hook for ChatGPT Codex requests. */
  openAICodexAuth?: OpenAICodexAuthAdapter;
  /** Dynamic OAuth access-token loader/refresh hook for Grok subscription requests. */
  grokAuth?: GrokAuthAdapter;
}

export function createUnavailableProvider(message: string): Provider {
  async function* streamChat(): AsyncIterable<StreamChunk> {
    throw new Error(message);
  }

  async function complete(): Promise<string> {
    throw new Error(message);
  }

  return { streamChat, complete };
}

export function createProviderInstance(options: ProviderInstanceOptions): Provider {
  return withCompletionDeadline(buildProviderInstance(options));
}

/**
 * Bound `complete` for every provider in one place, so none can be left
 * waiting on a connection that has gone quiet.
 *
 * A caller that passes its own signal owns the wait, as a streamed reply's
 * does: someone is watching and can stop it. A caller that passes none
 * (titles, memory) cannot, so the call gets a deadline on its whole length.
 * The deadline aborts the request, and releases the caller even if a
 * transport ignores the abort.
 */
export function withCompletionDeadline(provider: Provider): Provider {
  return {
    streamChat: (messages, chatOptions) => provider.streamChat(messages, chatOptions),
    complete: async (messages, chatOptions) => {
      if (chatOptions?.abortSignal) return provider.complete(messages, chatOptions);

      const deadlineMs = resolveCompletionDeadlineMs();
      const deadline = new AbortController();
      let timer: ReturnType<typeof setTimeout> | undefined;
      const expired = new Promise<never>((_resolve, reject) => {
        timer = setTimeout(() => {
          const error = new Error(
            `Provider request timed out: no complete reply after ${Math.round(deadlineMs / 1000)}s.`,
          );
          error.name = "TimeoutError";
          reject(error);
          deadline.abort(error);
        }, deadlineMs);
      });
      const request = provider.complete(messages, { ...chatOptions, abortSignal: deadline.signal });
      // Once the deadline has answered, the aborted request's own rejection has no listener.
      request.catch(() => {});
      try {
        return await Promise.race([request, expired]);
      } finally {
        clearTimeout(timer);
      }
    },
  };
}

function buildProviderInstance(options: ProviderInstanceOptions): Provider {
  const protocol = resolveProviderProtocol(options);
  if (protocol === "anthropic-messages") {
    return createAnthropicMessagesProvider(options);
  }

  if (protocol === "ark-responses") {
    return createArkResponsesProvider(options);
  }

  if (protocol === "openai-responses") {
    if (options.providerId === "opencode-zen") {
      // Existing Zen profiles explicitly store Responses for Muse. Dispatch
      // by model at send time: Space Bunny rejects /responses with 400
      // ModelProtocolUnsupported and uses /chat/completions instead.
      const responses = createOpenAIResponsesProvider(options);
      const chat = buildProviderInstance({ ...options, protocol: "openai-chat" });
      const forModel = (model?: string) => model === "space-bunny-free" ? chat : responses;
      return {
        streamChat: (messages, chatOptions) => forModel(chatOptions.model).streamChat(messages, chatOptions),
        complete: (messages, chatOptions) => forModel(chatOptions?.model).complete(messages, chatOptions),
      };
    }
    return createOpenAIResponsesProvider(options);
  }

  if (protocol === "ai-sdk") {
    return createAiSdkProvider(options);
  }

  if (isOpenAICodexBaseUrl(options.baseURL)) {
    return createOpenAICodexProvider({
      ...options,
      providerId: options.providerId || "openai-codex",
      auth: options.openAICodexAuth,
    });
  }

  // Grok subscription rides the generic chat-completions path but needs the
  // CLI identity headers the proxy gates on, plus a fetch that keeps the
  // short-lived OAuth bearer fresh (and proxy-aware) across long sessions.
  const grokSubscription = options.providerId === "grok" || isGrokSubscriptionBaseUrl(options.baseURL);
  // User-configured headers merge over the built-in identity headers so a
  // config entry can override even the Grok defaults deliberately.
  const defaultHeaders = {
    ...(grokSubscription ? buildGrokSubscriptionHeaders() : {}),
    ...(options.headers ?? {}),
  };
  // Every client gets an explicit fetch. Left to itself the OpenAI SDK streams
  // through its bundled node-fetch on Node, which reports a premature close
  // only for chunked bodies: a reply cut short of its content-length left the
  // stream neither ended nor failed, and the turn hung. Node's own fetch fails
  // that read, and the completion check covers a body that ends cleanly but
  // early.
  const requestTimeoutMs = resolveRequestTimeoutMs(process.env.BUBBLE_PROVIDER_REQUEST_TIMEOUT_MS);
  const transportFetch: ProviderFetch = grokSubscription
    ? (options.grokAuth ? createGrokSubscriptionFetch(options.grokAuth) : getChatGptFetch())
    : options.providerId === "opencode-zen"
      ? createProviderFetch({ providerName: "OpenCode Zen" })
      // No limit on how long the model may stay silent unless the person
      // set a request timeout: Node's fetch would otherwise cut a reply
      // after 300 seconds without a byte.
      : createPlainProviderFetch({ idleTimeoutMs: resolveIdleTimeoutMs() });
  const client = new OpenAI({
    apiKey: options.apiKey,
    baseURL: options.baseURL,
    timeout: requestTimeoutMs,
    ...(Object.keys(defaultHeaders).length > 0 ? { defaultHeaders } : {}),
    fetch: withChatStreamCompletionCheck(transportFetch) as unknown as NonNullable<ConstructorParameters<typeof OpenAI>[0]>["fetch"],
  });

  const fallbackModel = "gpt-4o";

  async function* streamChat(
    messages: ProviderMessage[],
    chatOptions: { model: string; tools?: ToolDefinition[]; toolChoice?: ToolChoiceMode; temperature?: number; thinkingLevel?: ThinkingLevel; abortSignal?: AbortSignal; rateLimitPolicy?: RateLimitPolicy }
  ): AsyncIterable<StreamChunk> {
    assertProviderModelAllowed(options.providerId || "", chatOptions.model);
    const requestConfig = resolveProviderRequestConfig(
      options.providerId || "",
      chatOptions.model,
      chatOptions.thinkingLevel ?? options.thinkingLevel ?? "off",
    );
    const tools = chatOptions.tools?.map((t) => ({
      type: "function" as const,
      function: {
        name: t.name,
        description: t.description,
        parameters: t.parameters as any,
      },
    }));

    const body: any = {
      model: chatOptions.model,
      messages: messages.map((message) => toChatCompletionsMessage(message, {
        reasoningContentEcho: requestConfig.reasoningContentEcho ?? "tool_calls",
      })),
      tools: tools && tools.length > 0 ? tools : undefined,
      tool_choice: tools && tools.length > 0 ? chatOptions.toolChoice ?? "auto" : undefined,
      stream: true,
    };
    // Several OpenAI-compatible streaming APIs only emit final usage when this
    // flag is set. Without it, downstream goal/stat accounting can only report
    // "usage unavailable".
    if (shouldRequestStreamUsage(options)) {
      body.stream_options = { include_usage: true };
    }
    if (!requestConfig.omitTemperature) {
      body.temperature = chatOptions.temperature ?? 0.2;
    }

    if (requestConfig.extraBody) {
      Object.assign(body, requestConfig.extraBody);
    }

    if (tools && tools.length > 0 && requestConfig.parallelToolCalls !== undefined) {
      body.parallel_tool_calls = requestConfig.parallelToolCalls;
    }

    if (requestConfig.maxTokens !== undefined) {
      body.max_tokens = requestConfig.maxTokens;
    }

    if (requestConfig.reasoningEffort && requestConfig.reasoningEffort !== "off") {
      body.reasoning = { enabled: true };
    }

    const createCompletion = async (requestBody: any): Promise<any> => {
      try {
        return await client.chat.completions.create(requestBody as any, {
          signal: chatOptions.abortSignal,
          ...(chatOptions.rateLimitPolicy === "defer" ? { maxRetries: 0 } : {}),
        } as any);
      } catch (error: any) {
        if (!chatOptions.abortSignal?.aborted && isSpaceBunnyUpstreamRejection(options.providerId, chatOptions.model, error)) {
          throw Object.assign(recoverableSpaceBunnyError(error), { requestShape: chatRequestShape(requestBody) });
        }
        if (error?.status === 429) {
          const retryAfterHeader = error?.headers?.["retry-after"];
          const retryAfterSeconds = Number(retryAfterHeader);
          throw new RateLimitError(error?.message || "Rate limited (429)", {
            status: 429,
            retryAfterMs: Number.isFinite(retryAfterSeconds) ? Math.round(retryAfterSeconds * 1000) : undefined,
            cause: error,
          });
        }
        throw error;
      }
    };

    if (shouldUseNonStreamingToolCalls(options, tools, chatOptions.toolChoice)) {
      body.stream = false;
      delete body.stream_options;
      const response = await createCompletion(body);
      yield* translateOpenAIFullResponse(response);
      yield { type: "done" };
      return;
    }

    // Rate-limit contract (design §4.5): "defer" disables the SDK's own
    // retries so the caller is the single 429 backoff layer; either policy
    // surfaces a final 429 as a typed RateLimitError instead of a string.
    let stream: any;
    stream = (await createCompletion(body)) as any;

    // A socket drop while iterating the SSE stream must surface as
    // ProviderStreamInterruptedError so the agent loop re-issues the request;
    // a raw network error here aborts the whole run (anthropic/codex/ai-sdk
    // paths already wrap — this generic chat-completions path did not).
    try {
      yield* translateOpenAIStream(stream, {
        toolArgsMergeMode: resolveToolArgsMergeMode(options.providerId || "", options.baseURL || ""),
        reasoningMergeMode: resolveReasoningMergeMode(options.providerId || "", options.baseURL || ""),
        textMergeMode: resolveTextMergeMode(options.providerId || "", options.baseURL || ""),
        debugProviderId: options.providerId || "",
        debugModelId: chatOptions.model,
      });
    } catch (error) {
      if (chatOptions.abortSignal?.aborted) throw error;
      if (isSpaceBunnyUpstreamRejection(options.providerId, chatOptions.model, error)) {
        throw Object.assign(recoverableSpaceBunnyError(error), { requestShape: chatRequestShape(body) });
      }
      if (isProviderResponseError(error)) {
        const rateLimited = error.status === 429 || error.errorType === "rate_limit_exceeded";
        if (rateLimited && chatOptions.rateLimitPolicy === "defer") {
          throw new RateLimitError(error.message, {
            status: error.status ?? 429,
            retryAfterMs: error.retryAfterMs,
            cause: error,
          });
        }
        if (isRetryableProviderResponseError(error)) {
          // OpenRouter has already exhausted any pre-stream fallback before it
          // emits a terminal SSE error. One client retry is useful for a fresh
          // route; repeating the previous two-retry socket budget turns a
          // five-minute upstream timeout into a fifteen-minute apparent hang.
          throw new ProviderStreamInterruptedError(error.message, {
            cause: error,
            retryAfterMs: error.retryAfterMs,
            maxRetries: 1,
          });
        }
        throw error;
      }
      throw new ProviderStreamInterruptedError(
        `Provider stream interrupted: ${error instanceof Error ? error.message : String(error)}`,
        { cause: error },
      );
    }

    yield { type: "done" };
  }

  async function complete(messages: ProviderMessage[], chatOptions?: { model?: string; temperature?: number; thinkingLevel?: ThinkingLevel; abortSignal?: AbortSignal }): Promise<string> {
    const model = chatOptions?.model ?? fallbackModel;
    assertProviderModelAllowed(options.providerId || "", model);
    const requestConfig = resolveProviderRequestConfig(
      options.providerId || "",
      model,
      chatOptions?.thinkingLevel ?? options.thinkingLevel ?? "off",
    );
    const body: any = {
      model,
      messages: messages.map((message) => toChatCompletionsMessage(message, {
        reasoningContentEcho: requestConfig.reasoningContentEcho ?? "tool_calls",
      })),
    };
    if (!requestConfig.omitTemperature) {
      body.temperature = chatOptions?.temperature ?? 0.2;
    }

    if (requestConfig.extraBody) {
      Object.assign(body, requestConfig.extraBody);
    }

    if (requestConfig.maxTokens !== undefined) {
      body.max_tokens = requestConfig.maxTokens;
    }

    if (requestConfig.reasoningEffort && requestConfig.reasoningEffort !== "off") {
      body.reasoning = { enabled: true };
    }
    for (let attempt = 0; ; attempt++) {
      try {
        const response = await client.chat.completions.create(body, { signal: chatOptions?.abortSignal } as any);
        return response.choices[0]?.message?.content ?? "";
      } catch (error) {
        if (chatOptions?.abortSignal?.aborted || attempt >= 2 || !isSpaceBunnyUpstreamRejection(options.providerId, body.model, error)) throw error;
        await sleepBeforeRetry(computeRetryDelayMs(attempt + 1), chatOptions?.abortSignal);
      }
    }
  }

  return { streamChat, complete };
}

function resolveProviderProtocol(options: ProviderInstanceOptions): ProviderProtocol {
  if (options.protocol) return options.protocol;
  const providerId = (options.providerId || "").toLowerCase();
  const baseURL = (options.baseURL || "").toLowerCase();
  if (
    providerId === "opencode-zen"
  ) {
    return "openai-responses";
  }
  if (
    providerId === "doubao"
    && baseURL.replace(/\/+$/, "") === "https://ark.cn-beijing.volces.com/api/v3"
  ) {
    return "ark-responses";
  }
  if (
    providerId === "anthropic"
    || providerId.endsWith("-anthropic")
    || baseURL.includes("/anthropic")
  ) {
    return "anthropic-messages";
  }
  return "openai-chat";
}

function isMiniMaxOpenAICompatible(options: Pick<ProviderInstanceOptions, "providerId" | "baseURL">): boolean {
  const providerId = (options.providerId || "").toLowerCase();
  const baseURL = (options.baseURL || "").toLowerCase();
  return providerId === "minimax-openai"
    || (providerId === "minimax" && !baseURL.includes("/anthropic"))
    || baseURL.includes("api.minimaxi.com/v1")
    || baseURL.includes("api.minimax.io/v1");
}

function shouldRequestStreamUsage(options: Pick<ProviderInstanceOptions, "providerId" | "baseURL">): boolean {
  const providerId = (options.providerId || "").toLowerCase();
  return providerId === "openai"
    || providerId === "opencode-zen"
    || providerId === "deepseek"
    || providerId === "moonshot-cn"
    || providerId === "moonshot-intl"
    // Verified 2026-08-04: this endpoint streams NO usage at all without the
    // flag, so every coding-plan session was silently unpriced and left the
    // context-budget anchor guessing.
    || providerId === "kimi-for-coding"
    || providerId === "zhipuai"
    || providerId === "zhipuai-coding-plan"
    || providerId === "zai"
    || providerId === "zai-coding-plan"
    // Bailian sends usage either way; ask explicitly so it stays guaranteed.
    || providerId === "bailian-token-plan"
    || providerId === "stepfun-api"
    || isMiniMaxOpenAICompatible(options);
}

function shouldUseNonStreamingToolCalls(
  options: Pick<ProviderInstanceOptions, "providerId">,
  tools: unknown[] | undefined,
  toolChoice: ToolChoiceMode | undefined,
): boolean {
  return (options.providerId || "").toLowerCase() === "doubao"
    && !!tools
    && tools.length > 0
    && toolChoice !== "none";
}

// Some providers (notably Fireworks-hosted Kimi) stream tool-call arguments
// as repeated full snapshots in each delta instead of incremental chunks, so
// a naive `+=` produces `{"x":1}{"x":1}` — not valid JSON. Parse the raw
// stream; if it doesn't parse but contains a balanced `{…}` prefix or suffix
// that does, use that. Empty or unsalvageable input becomes `"{}"` so the
// downstream echo to the model is always valid JSON.
//
// `corrupt` is set when the original raw was non-empty but unsalvageable,
// signalling that the model's intended arguments were lost in transport;
// downstream code should refuse to execute the call instead of silently
// running with an empty args object.
export interface NormalizedToolArgs {
  args: string;
  corrupt: boolean;
}

export function normalizeToolArgsDetailed(raw: string): NormalizedToolArgs {
  const s = (raw ?? "").trim();
  if (!s) {
    debugToolArgs({ stage: "normalize", input: raw, output: "{}", reason: "empty" });
    return { args: "{}", corrupt: false };
  }
  try { JSON.parse(s); debugToolArgs({ stage: "normalize", input: raw, output: s, reason: "passthrough" }); return { args: s, corrupt: false }; } catch {}

  const firstBrace = extractBalancedJson(s, 0);
  if (firstBrace) {
    try { JSON.parse(firstBrace); } catch {
      debugToolArgs({ stage: "normalize", input: raw, output: "{}", reason: "first-brace-unparseable", firstBrace });
      return { args: "{}", corrupt: true };
    }
    // If the content after the first balanced object is another valid object
    // with the same parse, we've got a snapshot duplication — keep one copy.
    const rest = s.slice(firstBrace.length).trim();
    if (!rest) {
      debugToolArgs({ stage: "normalize", input: raw, output: firstBrace, reason: "single-brace" });
      return { args: firstBrace, corrupt: false };
    }
    try { JSON.parse(rest); debugToolArgs({ stage: "normalize", input: raw, output: firstBrace, reason: "snapshot-dedup", rest }); return { args: firstBrace, corrupt: false }; } catch {}
    debugToolArgs({ stage: "normalize", input: raw, output: firstBrace, reason: "trailing-junk-dropped", rest });
    return { args: firstBrace, corrupt: false };
  }
  debugToolArgs({ stage: "normalize", input: raw, output: "{}", reason: "no-balanced-json" });
  return { args: "{}", corrupt: true };
}

export function normalizeToolArgs(raw: string): string {
  return normalizeToolArgsDetailed(raw).args;
}

function resolveToolArgsMergeMode(providerId: string, baseURL: string): ToolArgsMergeMode {
  const id = providerId.toLowerCase();
  const url = baseURL.toLowerCase();
  // Fireworks-hosted Kimi has been observed to stream cumulative snapshots
  // rather than OpenAI-style argument deltas.
  if (id === "fireworks" || url.includes("fireworks.ai")) return "snapshot";
  return "delta";
}

function resolveReasoningMergeMode(providerId: string, baseURL: string): ToolArgsMergeMode {
  const id = providerId.toLowerCase();
  const url = baseURL.toLowerCase();
  if (id === "fireworks" || url.includes("fireworks.ai")) return "snapshot";
  if (id === "minimax" || url.includes("api.minimaxi.com") || url.includes("api.minimax.io")) return "snapshot";
  return "delta";
}

function resolveTextMergeMode(providerId: string, baseURL: string): ToolArgsMergeMode {
  const id = providerId.toLowerCase();
  const url = baseURL.toLowerCase();
  if (id === "minimax" || url.includes("api.minimaxi.com") || url.includes("api.minimax.io")) return "snapshot";
  return "delta";
}

function extractBalancedJson(s: string, start: number): string | null {
  if (s[start] !== "{") return null;
  let depth = 0;
  let inStr = false;
  let escape = false;
  for (let i = start; i < s.length; i++) {
    const c = s[i];
    if (escape) { escape = false; continue; }
    if (c === "\\" && inStr) { escape = true; continue; }
    if (c === '"') { inStr = !inStr; continue; }
    if (inStr) continue;
    if (c === "{") depth++;
    else if (c === "}") {
      depth--;
      if (depth === 0) return s.slice(start, i + 1);
    }
  }
  return null;
}

/**
 * Convert a non-streaming OpenAI-compatible chat-completions response into the
 * same chunk protocol used by the streaming adapter. This is used for provider
 * tool-call paths where streamed function arguments are not reliable enough to
 * execute safely.
 */
export async function* translateOpenAIFullResponse(response: any): AsyncIterable<StreamChunk> {
  if (typeof response?.system_fingerprint === "string" && response.system_fingerprint) {
    yield { type: "response_metadata", systemFingerprint: response.system_fingerprint };
  }
  const usageChunk = usageToStreamChunk(response?.usage);
  if (usageChunk) yield usageChunk;

  const choice = response?.choices?.[0];
  const finishReason = choice?.finish_reason;
  const truncatedByLength = finishReason === "length";
  const message = choice?.message;
  if (!message) return;

  const reasoningDetails = extractReasoningDetailsText(message.reasoning_details);
  const reasoning = reasoningDetails
    ?? (typeof message.reasoning === "string" ? message.reasoning : undefined)
    ?? (typeof message.thinking === "string" ? message.thinking : undefined)
    ?? (typeof message.reasoning_content === "string" ? message.reasoning_content : undefined);
  if (reasoning) {
    yield { type: "reasoning_delta", content: reasoning };
  }

  if (typeof message.content === "string" && message.content) {
    const textFilter = createProviderProtocolArtifactFilter();
    const cleaned = textFilter.push(message.content) + textFilter.flush();
    if (cleaned) {
      yield { type: "text", content: cleaned };
    }
  }

  const toolCalls = Array.isArray(message.tool_calls) ? message.tool_calls : [];
  for (let index = 0; index < toolCalls.length; index += 1) {
    const toolCall = toolCalls[index];
    const name = typeof toolCall?.function?.name === "string" ? toolCall.function.name : "";
    if (!name) continue;
    const id = typeof toolCall?.id === "string" && toolCall.id
      ? toolCall.id
      : `call_${index}`;
    const rawArgs = typeof toolCall?.function?.arguments === "string"
      ? toolCall.function.arguments
      : JSON.stringify(toolCall?.function?.arguments ?? {});
    const normalized = normalizeToolArgsDetailed(rawArgs);
    const corrupt = normalized.corrupt || truncatedByLength;
    debugToolArgs({
      stage: "full-response-tool-call",
      id,
      name,
      entryArgs: rawArgs,
      finalArgs: normalized.args,
      finishReason,
      corrupt,
    });
    yield { type: "tool_call", id, name, arguments: "", isStart: true, isEnd: false };
    if (rawArgs) {
      yield { type: "tool_call", id, name, arguments: rawArgs, isStart: false, isEnd: false };
    }
    yield {
      type: "tool_call",
      id,
      name,
      arguments: "",
      argumentsFull: normalized.args,
      argumentsCorrupt: corrupt || undefined,
      isStart: false,
      isEnd: true,
    };
  }
}

function usageToStreamChunk(usage: any): Extract<StreamChunk, { type: "usage" }> | undefined {
  if (!usage) return undefined;
  return {
    type: "usage",
    usage: {
      promptTokens: typeof usage.prompt_tokens === "number" ? usage.prompt_tokens : 0,
      completionTokens: typeof usage.completion_tokens === "number" ? usage.completion_tokens : 0,
      outputTokensReported: typeof usage.completion_tokens === "number",
      promptCacheHitTokens: typeof usage.prompt_cache_hit_tokens === "number"
        ? usage.prompt_cache_hit_tokens
        : typeof usage.prompt_tokens_details?.cached_tokens === "number"
          ? usage.prompt_tokens_details.cached_tokens
          : undefined,
      promptCacheMissTokens: typeof usage.prompt_cache_miss_tokens === "number"
        ? usage.prompt_cache_miss_tokens
        : typeof usage.prompt_tokens_details?.cached_tokens === "number" && typeof usage.prompt_tokens === "number"
          ? Math.max(0, usage.prompt_tokens - usage.prompt_tokens_details.cached_tokens)
          : undefined,
      reasoningTokens: typeof usage.completion_tokens_details?.reasoning_tokens === "number"
        ? usage.completion_tokens_details.reasoning_tokens
        : undefined,
      totalTokens: typeof usage.total_tokens === "number" ? usage.total_tokens : undefined,
    },
  };
}

const RETRYABLE_PROVIDER_ERROR_TYPES = new Set([
  "capacity_exhausted",
  "provider_timeout",
  "rate_limit_exceeded",
  "server",
  "temporarily_unavailable",
]);

function isRetryableProviderResponseError(error: ProviderResponseError): boolean {
  return (error.status !== undefined && isRetryableHttpStatus(error.status))
    || (error.errorType !== undefined && RETRYABLE_PROVIDER_ERROR_TYPES.has(error.errorType));
}

function positiveSecondsToMs(value: unknown): number | undefined {
  const seconds = typeof value === "number" ? value : Number(value);
  return Number.isFinite(seconds) && seconds >= 0 ? Math.round(seconds * 1000) : undefined;
}

/** Parse HTTP-200 in-band provider errors such as OpenRouter's SSE format. */
function providerResponseErrorFromChunk(chunk: any, providerId?: string): ProviderResponseError | undefined {
  const finishReason = chunk?.choices?.[0]?.finish_reason;
  const rawError = chunk?.error;
  if ((!rawError || typeof rawError !== "object") && finishReason !== "error") return undefined;

  const metadata = rawError && typeof rawError.metadata === "object" && rawError.metadata !== null
    ? rawError.metadata as Record<string, unknown>
    : undefined;
  const availability = metadata && typeof metadata.availability === "object" && metadata.availability !== null
    ? metadata.availability as Record<string, unknown>
    : undefined;
  const status = typeof rawError?.code === "number"
    ? rawError.code
    : typeof rawError?.status === "number"
      ? rawError.status
      : undefined;
  const errorType = typeof metadata?.error_type === "string"
    ? metadata.error_type
    : typeof rawError?.type === "string"
      ? rawError.type
      : undefined;
  const retryAfterMs = positiveSecondsToMs(
    availability?.retry_after ?? metadata?.retry_after ?? rawError?.retry_after,
  );
  const provider = typeof chunk?.provider === "string" && chunk.provider.trim()
    ? chunk.provider.trim()
    : providerId?.trim() || "Provider";
  const details = [
    status !== undefined ? String(status) : undefined,
    errorType,
  ].filter(Boolean).join(", ");
  const rawMessage = typeof rawError?.message === "string" && rawError.message.trim()
    ? rawError.message.trim()
    : "Generation ended with finish_reason=error.";
  const message = `${provider} stream error${details ? ` (${details})` : ""}: ${rawMessage}`;
  return new ProviderResponseError(message, {
    status,
    errorType,
    retryAfterMs,
    cause: rawError,
  });
}

/**
 * Convert an OpenAI-compatible chat-completions stream into our internal StreamChunk events.
 *
 * Multi-tool-call streams are tracked by `index`, but tool-call starts and
 * argument deltas are emitted as soon as they arrive so the TUI can render
 * partial write previews before the tool executes. End events are still flushed
 * in index order to keep multi-call turns deterministic.
 */
export async function* translateOpenAIStream(
  stream: AsyncIterable<any>,
  options: TranslateOpenAIStreamOptions = {},
): AsyncIterable<StreamChunk> {
  const toolCalls = new Map<number, { id: string; name: string; args: string; started: boolean; corrupt?: boolean }>();
  const textFilter = createProviderProtocolArtifactFilter();
  const toolArgsMergeMode = options.toolArgsMergeMode ?? "delta";
  const reasoningMergeMode = options.reasoningMergeMode ?? "delta";
  const textMergeMode = options.textMergeMode ?? "delta";
  let reasoningBuffer = "";
  let textBuffer = "";
  let rawChunkSeq = 0;
  let systemFingerprint: string | undefined;
  // DeepSeek (and some inference re-hosts) sometimes deliver reasoning twice:
  // once via a dedicated `reasoning_content` / `thinking` field, and again
  // embedded as `<think>...</think>` inside `delta.content`. Track whether we
  // have seen the dedicated channel; if yes, strip <think> blocks from text
  // silently instead of yielding a second reasoning_delta.
  let hasDedicatedReasoningChannel = false;

  function* flushToolCalls(): Generator<StreamChunk> {
    if (toolCalls.size === 0) return;
    const sorted = [...toolCalls.entries()].sort(([a], [b]) => a - b);
    for (const [, entry] of sorted) {
      if (!entry.id || !entry.name) continue;
      if (!entry.started) {
        yield { type: "tool_call", id: entry.id, name: entry.name, arguments: "", isStart: true, isEnd: false };
        entry.started = true;
        if (entry.args) {
          yield { type: "tool_call", id: entry.id, name: entry.name, arguments: entry.args, isStart: false, isEnd: false };
        }
      }
      const normalized = normalizeToolArgsDetailed(entry.args);
      const corrupt = normalized.corrupt || !!entry.corrupt;
      debugToolArgs({ stage: "flush-end", id: entry.id, name: entry.name, entryArgs: entry.args, finalArgs: normalized.args, corrupt });
      yield {
        type: "tool_call",
        id: entry.id,
        name: entry.name,
        arguments: "",
        argumentsFull: normalized.args,
        argumentsCorrupt: corrupt || undefined,
        isStart: false,
        isEnd: true,
      };
    }
    toolCalls.clear();
  }

  function* startToolCallIfReady(entry: { id: string; name: string; args: string; started: boolean; corrupt?: boolean }): Generator<StreamChunk> {
    if (entry.started || !entry.id || !entry.name) return;
    entry.started = true;
    yield { type: "tool_call", id: entry.id, name: entry.name, arguments: "", isStart: true, isEnd: false };
    if (entry.args) {
      yield { type: "tool_call", id: entry.id, name: entry.name, arguments: entry.args, isStart: false, isEnd: false };
    }
  }

  for await (const chunk of stream) {
    rawChunkSeq += 1;
    const choice = chunk.choices?.[0];
    const delta = choice?.delta;
    const usage = (chunk as any).usage ?? choice?.usage;
    const finishReason = choice?.finish_reason;
    const providerError = providerResponseErrorFromChunk(chunk, options.debugProviderId);
    if (providerError) throw providerError;

    if (
      typeof chunk?.system_fingerprint === "string"
      && chunk.system_fingerprint
      && chunk.system_fingerprint !== systemFingerprint
    ) {
      const nextSystemFingerprint = chunk.system_fingerprint;
      systemFingerprint = nextSystemFingerprint;
      yield { type: "response_metadata", systemFingerprint: nextSystemFingerprint };
    }

    debugReasoningStream({
      stage: "provider_raw",
      providerId: options.debugProviderId,
      modelId: options.debugModelId,
      chunkSeq: rawChunkSeq,
      finishReason,
      content: summarizeDebugText(delta?.content),
      reasoning: summarizeDebugText((delta as any)?.reasoning),
      thinking: summarizeDebugText((delta as any)?.thinking),
      reasoningContent: summarizeDebugText((delta as any)?.reasoning_content),
      reasoningDetails: summarizeDebugText(extractReasoningDetailsText((delta as any)?.reasoning_details)),
    });

    if (usage) {
      yield {
        type: "usage",
        usage: {
          promptTokens: typeof usage.prompt_tokens === "number" ? usage.prompt_tokens : 0,
          completionTokens: typeof usage.completion_tokens === "number" ? usage.completion_tokens : 0,
          outputTokensReported: typeof usage.completion_tokens === "number",
          promptCacheHitTokens: typeof usage.prompt_cache_hit_tokens === "number"
            ? usage.prompt_cache_hit_tokens
            : typeof usage.prompt_tokens_details?.cached_tokens === "number"
              ? usage.prompt_tokens_details.cached_tokens
              : undefined,
          promptCacheMissTokens: typeof usage.prompt_cache_miss_tokens === "number"
            ? usage.prompt_cache_miss_tokens
            : typeof usage.prompt_tokens_details?.cached_tokens === "number" && typeof usage.prompt_tokens === "number"
              ? Math.max(0, usage.prompt_tokens - usage.prompt_tokens_details.cached_tokens)
              : undefined,
          reasoningTokens: typeof usage.completion_tokens_details?.reasoning_tokens === "number"
            ? usage.completion_tokens_details.reasoning_tokens
            : undefined,
          totalTokens: typeof usage.total_tokens === "number" ? usage.total_tokens : undefined,
        },
      };
    }

    const reasoningDetails = extractReasoningDetailsText((delta as any)?.reasoning_details);
    const reasoningField = reasoningDetails !== undefined
      ? "reasoning_details"
      : (delta as any)?.reasoning !== undefined
      ? "reasoning"
      : (delta as any)?.thinking !== undefined
        ? "thinking"
        : (delta as any)?.reasoning_content !== undefined
          ? "reasoning_content"
          : undefined;
    const reasoning = reasoningDetails !== undefined
      ? reasoningDetails
      : reasoningField
        ? (delta as any)[reasoningField]
        : undefined;
    if (reasoning) {
      hasDedicatedReasoningChannel = true;
      const merged = mergeStreamingText(reasoningBuffer, reasoning, reasoningMergeMode);
      reasoningBuffer = merged.args;
      debugReasoningStream({
        stage: "provider_emit",
        providerId: options.debugProviderId,
        modelId: options.debugModelId,
        chunkSeq: rawChunkSeq,
        source: reasoningField,
        mergeMode: reasoningMergeMode,
        suppressed: !merged.delta,
        emitted: summarizeDebugText(merged.delta),
        buffer: summarizeDebugText(reasoningBuffer),
      });
      if (merged.delta) {
        yield { type: "reasoning_delta", content: merged.delta };
      }
    }

    if (delta?.content) {
      const mergedContent = mergeStreamingText(textBuffer, delta.content, textMergeMode);
      textBuffer = mergedContent.args;
      const content = mergedContent.delta;
      if (content) {
        const thinkMatch = content.match(/<think>([\s\S]*?)<\/think>/);
        if (thinkMatch) {
          if (thinkMatch[1] && !hasDedicatedReasoningChannel) {
            const merged = mergeStreamingText(reasoningBuffer, thinkMatch[1], reasoningMergeMode);
            reasoningBuffer = merged.args;
            debugReasoningStream({
              stage: "provider_emit",
              providerId: options.debugProviderId,
              modelId: options.debugModelId,
              chunkSeq: rawChunkSeq,
              source: "content_think",
              mergeMode: reasoningMergeMode,
              suppressed: !merged.delta,
              emitted: summarizeDebugText(merged.delta),
              buffer: summarizeDebugText(reasoningBuffer),
            });
            if (merged.delta) {
              yield { type: "reasoning_delta", content: merged.delta };
            }
          }
          const remaining = content.replace(/<think>[\s\S]*?<\/think>/, "");
          const cleaned = textFilter.push(remaining);
          if (cleaned) {
            yield { type: "text", content: cleaned };
          }
        } else {
          const cleaned = textFilter.push(content);
          if (cleaned) {
            yield { type: "text", content: cleaned };
          }
        }
      }
    }

    if (delta?.tool_calls) {
      for (const tc of delta.tool_calls) {
        const idx = typeof tc.index === "number" ? tc.index : 0;
        let entry = toolCalls.get(idx);
        if (!entry) {
          entry = { id: "", name: "", args: "", started: false };
          toolCalls.set(idx, entry);
        }
        if (tc.id) entry.id = tc.id;
        if (tc.function?.name) entry.name = tc.function.name;
        yield* startToolCallIfReady(entry);
        if (typeof tc.function?.arguments === "string" && tc.function.arguments) {
          debugToolArgs({ stage: "raw-chunk", id: entry.id, name: entry.name, idx, raw: tc.function.arguments });
          const merged = mergeToolArgumentDelta(entry.args, tc.function.arguments, toolArgsMergeMode);
          entry.args = merged.args;
          if (entry.started && merged.delta) {
            yield {
              type: "tool_call",
              id: entry.id,
              name: entry.name,
              arguments: merged.delta,
              isStart: false,
              isEnd: false,
            };
          }
        }
      }
    }

    if (finishReason === "length") {
      for (const entry of toolCalls.values()) {
        entry.corrupt = true;
      }
      yield* flushToolCalls();
    } else if (finishReason === "tool_calls") {
      yield* flushToolCalls();
    }
  }

  const remainingText = textFilter.flush();
  if (remainingText) {
    yield { type: "text", content: remainingText };
  }
  yield* flushToolCalls();
}

function extractReasoningDetailsText(value: unknown): string | undefined {
  if (!value) return undefined;
  const details = Array.isArray(value) ? value : [value];
  const parts = details.flatMap((item) => {
    if (!item || typeof item !== "object") return [];
    const record = item as Record<string, unknown>;
    const text = record.text ?? record.thinking ?? record.content;
    return typeof text === "string" ? [text] : [];
  });
  return parts.length > 0 ? parts.join("") : undefined;
}

function mergeToolArgumentDelta(current: string, incoming: string, mode: ToolArgsMergeMode): { args: string; delta: string } {
  if (!current) {
    debugToolArgs({ stage: "merge", branch: "empty-current", current, incoming, args: incoming, delta: incoming });
    return { args: incoming, delta: incoming };
  }
  if (!incoming) {
    debugToolArgs({ stage: "merge", branch: "empty-incoming", current, incoming, args: current, delta: "" });
    return { args: current, delta: "" };
  }

  if (mode === "snapshot") {
    // Snapshot streams repeat the current full argument buffer. Only treat a
    // chunk as duplicate when it is exactly equal, or as growth when it carries
    // the current buffer as a prefix. A suffix match is not enough: the next
    // legitimate delta can be a single trailing character like "0".
    if (incoming === current) {
      debugToolArgs({ stage: "merge", branch: "snapshot-dup", current, incoming, args: current, delta: "" });
      return { args: current, delta: "" };
    }
    if (incoming.startsWith(current)) {
      const delta = incoming.slice(current.length);
      debugToolArgs({ stage: "merge", branch: "snapshot-grow", current, incoming, args: incoming, delta });
      return { args: incoming, delta };
    }
  }

  debugToolArgs({ stage: "merge", branch: mode === "delta" ? "delta-append" : "snapshot-fallback-concat", current, incoming, args: current + incoming, delta: incoming });
  return { args: current + incoming, delta: incoming };
}

function mergeStreamingText(current: string, incoming: string, mode: ToolArgsMergeMode): { args: string; delta: string } {
  if (!current) return { args: incoming, delta: incoming };
  if (!incoming) return { args: current, delta: "" };
  if (mode === "snapshot") {
    if (incoming === current) return { args: current, delta: "" };
    if (incoming.startsWith(current)) return { args: incoming, delta: incoming.slice(current.length) };
  }
  return { args: current + incoming, delta: incoming };
}
