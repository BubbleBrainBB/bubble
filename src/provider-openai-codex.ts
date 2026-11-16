import { createHash } from "node:crypto";
import { THINKING_LEVELS, type Provider, type ProviderMessage, type ReasoningEffort, type StreamChunk, type ThinkingLevel, type TokenUsage, type ToolChoiceMode, type ToolDefinition } from "./types.js";
import type { OAuthCredentials } from "./oauth/types.js";
import { waitForOAuth } from "./oauth/refresh-control.js";
import { getBuiltinModel, listBuiltinModels } from "./model-catalog.js";
import { resolveProviderRequestConfig } from "./provider-transform.js";
import { chatGptFetch, type ChatGptFetch } from "./network/chatgpt-transport.js";
import { logProviderTransportFailure } from "./network/provider-transport-log.js";
import {
  computeRetryDelayMs,
  getProviderMaxRetries,
  isRetryableHttpStatus,
  ProviderStreamInterruptedError,
  retryLimitFor,
  sleepBeforeRetry,
} from "./network/retry.js";

export interface CodexModelDescriptor {
  id: string;
  displayName?: string;
  contextWindow?: number;
  useResponsesLite?: boolean;
  reasoningLevels?: ReasoningEffort[];
  defaultReasoningLevel?: ReasoningEffort;
  priority?: number;
  visibility?: string;
  minimalClientVersion?: string;
  /** Server-declared per-tool-output token cap (truncation_policy.limit when mode=tokens). */
  toolOutputTokenLimit?: number;
}

const DEFAULT_CODEX_BASE_URL = "https://chatgpt.com/backend-api";
const OPENAI_BETA_RESPONSES = "responses=experimental";
const TOKEN_REFRESH_GRACE_MS = 5 * 60 * 1000;
// OpenAI gates new codex models server-side by client_version (each model carries a
// `minimal_client_version`). Track a recent real Codex CLI release; override via env
// when OpenAI lifts the gate again before we cut a new release.
// 2026-09-17: gpt-6-astra requires 0.153.0.
// 2026-10-06: gpt-6.1-sol requires 0.159.0, the version the Codex app sends.
const CODEX_CLIENT_VERSION = process.env.BUBBLE_CODEX_CLIENT_VERSION?.trim() || "0.159.0";
/** The Codex client version Bubble claims; the account catalog the server returns depends on it. */
export function getCodexClientVersion(): string {
  return CODEX_CLIENT_VERSION;
}
const MODEL_DISCOVERY_PATHS = [
  `/codex/models?client_version=${CODEX_CLIENT_VERSION}`,
  "/models",
];

export function isOpenAICodexBaseUrl(baseURL: string): boolean {
  const normalized = baseURL.trim().replace(/\/+$/, "");
  return normalized === DEFAULT_CODEX_BASE_URL || normalized.startsWith(`${DEFAULT_CODEX_BASE_URL}/`);
}

export function getOpenAICodexFallbackModels(): string[] {
  return listBuiltinModels("openai-codex").map((model) => model.id);
}

export interface OpenAICodexAuthAdapter {
  getCredentials: () => OAuthCredentials | undefined | Promise<OAuthCredentials | undefined>;
  refreshCredentials: (current?: OAuthCredentials) => Promise<OAuthCredentials>;
  isExpired?: (credentials: OAuthCredentials, graceMs: number) => boolean;
}

export function extractChatGptAccountId(accessToken: string): string | undefined {
  try {
    const parts = accessToken.split(".");
    if (parts.length !== 3) return undefined;
    const payload = JSON.parse(Buffer.from(parts[1], "base64url").toString("utf-8")) as Record<string, any>;
    const auth = payload["https://api.openai.com/auth"];
    if (typeof auth?.chatgpt_account_id === "string" && auth.chatgpt_account_id) {
      return auth.chatgpt_account_id;
    }
    if (typeof auth?.account_id === "string" && auth.account_id) {
      return auth.account_id;
    }
    return undefined;
  } catch {
    return undefined;
  }
}

export function createOpenAICodexProvider(options: {
  providerId?: string;
  apiKey: string;
  baseURL: string;
  thinkingLevel?: ThinkingLevel;
  promptCacheKey?: string;
  auth?: OpenAICodexAuthAdapter;
  fetch?: ChatGptFetch;
}): Provider {
  const sessionId = globalThis.crypto?.randomUUID?.() ?? `bubble_${Date.now()}`;
  const fetchImpl = options.fetch ?? chatGptFetch;
  let refreshPromise: Promise<OAuthCredentials> | undefined;

  // `rejectedAccessToken`: the token a 401 was returned for. If storage already
  // holds a different one, another process rotated it while the request was in
  // flight — use that instead of spending its refresh token on a second rotation.
  async function resolveRequestAuth(
    forceRefresh = false,
    rejectedAccessToken?: string,
  ): Promise<{ accessToken: string; accountId: string }> {
    let credentials = await options.auth?.getCredentials();
    if (credentials && options.auth) {
      const expired = options.auth.isExpired
        ? options.auth.isExpired(credentials, TOKEN_REFRESH_GRACE_MS)
        : Date.now() >= credentials.expiresAt - TOKEN_REFRESH_GRACE_MS;
      const alreadyRotated = rejectedAccessToken !== undefined
        && !!credentials.accessToken
        && credentials.accessToken !== rejectedAccessToken;
      const mustRefresh = (forceRefresh && !alreadyRotated) || !credentials.accessToken || expired;
      if (mustRefresh && credentials.refreshToken) {
        if (!refreshPromise) {
          refreshPromise = options.auth.refreshCredentials(credentials).finally(() => {
            refreshPromise = undefined;
          });
        }
        credentials = await refreshPromise;
      }
    }

    const accessToken = credentials?.accessToken || options.apiKey;
    const accountId = credentials?.accountId || extractChatGptAccountId(accessToken);
    if (!accountId) {
      throw new Error("Failed to extract chatgpt_account_id from ChatGPT OAuth token.");
    }
    return { accessToken, accountId };
  }

  async function* streamChat(
    messages: ProviderMessage[],
    chatOptions: { model: string; tools?: ToolDefinition[]; toolChoice?: ToolChoiceMode; temperature?: number; thinkingLevel?: ThinkingLevel; abortSignal?: AbortSignal }
  ): AsyncIterable<StreamChunk> {
    const requestConfig = resolveProviderRequestConfig(
      "openai-codex",
      chatOptions.model,
      chatOptions.thinkingLevel ?? options.thinkingLevel ?? "off",
    );
    const useResponsesLite = getBuiltinModel("openai-codex", chatOptions.model)?.useResponsesLite === true;
    const body = JSON.stringify(
      buildRequestBody(messages, {
        model: chatOptions.model,
        tools: chatOptions.tools,
        toolChoice: chatOptions.toolChoice,
        reasoningEffort: requestConfig.reasoningEffort,
        sessionId,
        providerId: options.providerId,
        promptCacheKey: options.promptCacheKey,
        useResponsesLite,
      })
    );

    let sentAccessToken: string | undefined;
    const sendRequest = async (forceRefresh = false) => {
      chatOptions.abortSignal?.throwIfAborted();
      const { accessToken, accountId } = await waitForOAuth(
        resolveRequestAuth(forceRefresh, forceRefresh ? sentAccessToken : undefined),
        chatOptions.abortSignal,
      );
      chatOptions.abortSignal?.throwIfAborted();
      sentAccessToken = accessToken;
      return fetchImpl(resolveCodexUrl(options.baseURL), buildCodexRequestInit({
        accessToken,
        accountId,
        sessionId,
        signal: chatOptions.abortSignal,
        body,
        useResponsesLite,
      }));
    };

    for (let attempt = 0; ; attempt++) {
      const requestId = globalThis.crypto.randomUUID();
      const startedAt = Date.now();
      let responseStatus: number | undefined;
      let receivedEvents = 0;
      let lastEventAt: number | undefined;
      let sawParsedSseEvent = false;
      let currentToolCall:
        | {
            id: string;
            name: string;
            args: string;
            started: boolean;
          }
        | undefined;

      // OpenAI streams one reasoning summary per part; each part is a
      // self-contained markdown block. Separate parts so Thinking does not
      // read as one run-on line.
      let summaryPartsSeen = 0;
      try {
        let response = await sendRequest();
        responseStatus = response.status;

        if (!response.ok) {
          const errorText = await response.text().catch(() => "");
          if (response.status === 401 && options.auth && isTokenExpiredError(errorText)) {
            response = await sendRequest(true);
            responseStatus = response.status;
          } else {
            throw new Error(`${response.status} status code${errorText ? `: ${errorText}` : " (no body)"}`);
          }
        }

        if (!response.ok) {
          const errorText = await response.text().catch(() => "");
          throw new Error(`${response.status} status code${errorText ? `: ${errorText}` : " (no body)"}`);
        }

        for await (const event of parseSse(response)) {
          sawParsedSseEvent = true;
          receivedEvents += 1;
          lastEventAt = Date.now();
          const type = typeof event.type === "string" ? event.type : undefined;
          if (!type) continue;

          if (type === "error") {
            const message = typeof event.message === "string" ? event.message : JSON.stringify(event);
            throw new Error(message);
          }

          if (type === "response.failed") {
            const message = typeof (event.response as any)?.error?.message === "string"
              ? (event.response as any).error.message
              : "Codex response failed";
            throw new Error(message);
          }

          if (type === "response.output_item.added") {
            const item = (event as any).item;
            if (item?.type === "function_call" && typeof item.call_id === "string" && typeof item.name === "string") {
              currentToolCall = {
                id: item.call_id,
                name: item.name,
                args: typeof item.arguments === "string" ? item.arguments : "",
                started: true,
              };
              yield {
                type: "tool_call",
                id: currentToolCall.id,
                name: currentToolCall.name,
                arguments: "",
                isStart: true,
                isEnd: false,
              };
            }
            continue;
          }

          if (type === "response.output_text.delta" || type === "response.refusal.delta") {
            const delta = typeof (event as any).delta === "string" ? (event as any).delta : "";
            if (delta) {
              yield { type: "text", content: delta };
            }
            continue;
          }

          if (type === "response.reasoning_summary_part.added") {
            summaryPartsSeen += 1;
            if (summaryPartsSeen > 1) {
              yield { type: "reasoning_delta", content: "\n\n" };
            }
            continue;
          }

          if (type === "response.reasoning_summary_text.delta") {
            const delta = typeof (event as any).delta === "string" ? (event as any).delta : "";
            if (delta) {
              yield { type: "reasoning_delta", content: delta };
            }
            continue;
          }

          if (type === "response.function_call_arguments.delta" && currentToolCall) {
            const delta = typeof (event as any).delta === "string" ? (event as any).delta : "";
            if (delta) {
              currentToolCall.args += delta;
              yield {
                type: "tool_call",
                id: currentToolCall.id,
                name: currentToolCall.name,
                arguments: delta,
                isStart: false,
                isEnd: false,
              };
            }
            continue;
          }

          if (type === "response.function_call_arguments.done" && currentToolCall) {
            const finalArgs = typeof (event as any).arguments === "string" ? (event as any).arguments : currentToolCall.args;
            if (finalArgs.startsWith(currentToolCall.args)) {
              const tail = finalArgs.slice(currentToolCall.args.length);
              if (tail) {
                currentToolCall.args = finalArgs;
                yield {
                  type: "tool_call",
                  id: currentToolCall.id,
                  name: currentToolCall.name,
                  arguments: tail,
                  isStart: false,
                  isEnd: false,
                };
              }
            } else {
              currentToolCall.args = finalArgs;
            }
            continue;
          }

          if (type === "response.output_item.done" && currentToolCall) {
            const item = (event as any).item;
            if (item?.type === "function_call" && item.call_id === currentToolCall.id) {
              yield {
                type: "tool_call",
                id: currentToolCall.id,
                name: currentToolCall.name,
                arguments: "",
                isStart: false,
                isEnd: true,
              };
              currentToolCall = undefined;
            }
            continue;
          }

          if (type === "response.completed" || type === "response.done" || type === "response.incomplete") {
            const usage = (event as any).response?.usage;
            if (usage) {
              yield {
                type: "usage",
                usage: normalizeOpenAICodexUsage(usage),
              };
            }
            continue;
          }
        }

        yield { type: "done" };
        return;
      } catch (error) {
        const delegateRetry = sawParsedSseEvent
          && !chatOptions.abortSignal?.aborted
          && isTransientCodexTransportError(error);
        const retry = shouldRetryCodexTransportError({
          error, attempt, sawParsedSseEvent, signal: chatOptions.abortSignal,
        });
        logProviderTransportFailure(error, {
          providerId: options.providerId ?? "openai-codex",
          modelId: chatOptions.model,
          thinkingLevel: chatOptions.thinkingLevel ?? options.thinkingLevel ?? "off",
          messageCount: messages.length,
          toolCount: chatOptions.tools?.length ?? 0,
          sessionId, requestId, attempt: attempt + 1, responseStatus, receivedEvents,
          elapsedMs: Date.now() - startedAt,
          lastEventAgeMs: lastEventAt === undefined ? undefined : Date.now() - lastEventAt,
          decision: chatOptions.abortSignal?.aborted ? "cancelled"
            : delegateRetry ? "delegate_retry" : retry ? "retry" : "fail",
        });
        if (delegateRetry) {
          // Partial content already surfaced — the agent loop discards the
          // half-built assistant message and re-issues the whole request.
          throw new ProviderStreamInterruptedError(
            "ChatGPT connection interrupted while receiving a response.",
            { cause: error },
          );
        }
        if (!retry) {
          throw error;
        }
        await sleepBeforeRetry(computeRetryDelayMs(attempt + 1), chatOptions.abortSignal);
      }
    }
  }

  async function complete(
    messages: ProviderMessage[],
    chatOptions?: { model?: string; temperature?: number; thinkingLevel?: ThinkingLevel; abortSignal?: AbortSignal }
  ): Promise<string> {
    let content = "";
    for await (const chunk of streamChat(messages, {
      model: chatOptions?.model ?? "gpt-5.5",
      temperature: chatOptions?.temperature,
      thinkingLevel: chatOptions?.thinkingLevel,
      abortSignal: chatOptions?.abortSignal,
    })) {
      if (chunk.type === "text") {
        content += chunk.content;
      }
    }
    return content;
  }

  return { streamChat, complete };
}

function isTokenExpiredError(errorText: string): boolean {
  return /token_expired|session expired/i.test(errorText);
}

export function normalizeOpenAICodexUsage(usage: any): TokenUsage {
  const promptTokens = typeof usage?.input_tokens === "number" ? usage.input_tokens : 0;
  const cachedTokens = typeof usage?.input_tokens_details?.cached_tokens === "number"
    ? usage.input_tokens_details.cached_tokens
    : undefined;

  return {
    promptTokens,
    completionTokens: typeof usage?.output_tokens === "number" ? usage.output_tokens : 0,
    promptCacheHitTokens: cachedTokens,
    promptCacheMissTokens: cachedTokens !== undefined ? Math.max(0, promptTokens - cachedTokens) : undefined,
    reasoningTokens: typeof usage?.output_tokens_details?.reasoning_tokens === "number"
      ? usage.output_tokens_details.reasoning_tokens
      : undefined,
    totalTokens: typeof usage?.total_tokens === "number" ? usage.total_tokens : undefined,
  };
}

export function buildOpenAICodexPromptCacheKey(input: {
  seed?: string;
  providerId?: string;
  model: string;
}): string | undefined {
  const seed = input.seed?.trim();
  if (!seed) return undefined;

  return createHash("sha256")
    .update(`bubble:${input.providerId || "openai-codex"}:${input.model}:${seed}`)
    .digest("hex");
}

export async function fetchOpenAICodexModels(options: {
  baseURL: string;
  accessToken: string;
  fetch?: ChatGptFetch;
}): Promise<CodexModelDescriptor[]> {
  const result = await fetchOpenAICodexModelCatalog(options);
  return result.descriptors;
}

export interface OpenAICodexModelCatalogResult {
  descriptors: CodexModelDescriptor[];
  status: "success" | "unavailable";
  /** Why each discovery path failed, when status is "unavailable". */
  reason?: string;
}

/**
 * Fetches the account-scoped Codex catalog while preserving the distinction
 * between an authoritative empty catalog and discovery being unavailable.
 */
export async function fetchOpenAICodexModelCatalog(options: {
  baseURL: string;
  accessToken: string;
  fetch?: ChatGptFetch;
  /** Bounds both catalog GETs; a stalled warm-up must not pin the in-flight slot. */
  signal?: AbortSignal;
  /** Per-request timeout, so a slow first path still leaves the fallback a full window. */
  timeoutMs?: number;
}): Promise<OpenAICodexModelCatalogResult> {
  const accountId = extractChatGptAccountId(options.accessToken);
  if (!accountId) {
    return { descriptors: [], status: "unavailable", reason: "access token has no ChatGPT account id" };
  }
  const fetchImpl = options.fetch ?? chatGptFetch;
  const failures: string[] = [];

  for (const path of MODEL_DISCOVERY_PATHS) {
    const signal = options.timeoutMs === undefined
      ? options.signal
      : options.signal
        ? AbortSignal.any([options.signal, AbortSignal.timeout(options.timeoutMs)])
        : AbortSignal.timeout(options.timeoutMs);
    const startedAt = Date.now();
    const response = await fetchImpl(resolveRelativeUrl(options.baseURL, path), {
      method: "GET",
      headers: buildBaseHeaders(
        options.accessToken,
        accountId,
        globalThis.crypto?.randomUUID?.() ?? `bubble_${Date.now()}`,
        { accept: "application/json" },
      ),
      signal,
    }).catch((error: unknown) => {
      failures.push(`${path}: ${error instanceof Error ? error.message : String(error)} after ${Date.now() - startedAt}ms`);
      return undefined;
    });

    if (!response) continue;
    if (!response.ok) {
      failures.push(`${path}: HTTP ${response.status} after ${Date.now() - startedAt}ms`);
      continue;
    }

    const parsed = await response.json()
      .then((payload) => ({ ok: true as const, payload }))
      .catch(() => ({ ok: false as const }));
    if (!parsed.ok) {
      failures.push(`${path}: invalid JSON`);
      continue;
    }
    const payload = parsed.payload;
    if (!Array.isArray(payload) && !(payload && typeof payload === "object"
      && (Array.isArray(payload.models) || Array.isArray(payload.data)))) {
      failures.push(`${path}: unexpected payload`);
      continue;
    }

    return {
      descriptors: sortCodexModelDescriptors(extractCodexModelDescriptors(payload)),
      status: "success",
    };
  }

  return { descriptors: [], status: "unavailable", reason: failures.join("; ") };
}

function buildRequestBody(
  messages: ProviderMessage[],
  options: {
    model: string;
    tools?: ToolDefinition[];
    toolChoice?: ToolChoiceMode;
    reasoningEffort?: ThinkingLevel;
    sessionId?: string;
    providerId?: string;
    promptCacheKey?: string;
    useResponsesLite?: boolean;
  }
) {
  const instructions = messages
    .filter((message) => message.role === "system")
    .map((message) => message.content)
    .join("\n\n");

  const input = messages.flatMap((message) => convertMessage(message));
  const functionTools = (options.tools ?? []).map((tool) => ({
    type: "function",
    name: tool.name,
    description: tool.description,
    parameters: tool.parameters,
  }));
  const liteFunctionTools = options.toolChoice === "none" ? [] : functionTools;
  if (options.useResponsesLite) {
    input.unshift({
      type: "additional_tools",
      role: "developer",
      tools: liteFunctionTools,
    });
    if (instructions) {
      input.splice(1, 0, {
        role: "developer",
        content: [{ type: "input_text", text: instructions }],
      });
    }
  }
  const body: Record<string, unknown> = {
    model: options.model,
    store: false,
    stream: true,
    instructions: options.useResponsesLite ? "" : instructions || undefined,
    input,
    include: ["reasoning.encrypted_content"],
    prompt_cache_key: buildOpenAICodexPromptCacheKey({
      seed: options.promptCacheKey ?? options.sessionId,
      providerId: options.providerId,
      model: options.model,
    }),
    tool_choice: options.useResponsesLite
      ? "auto"
      : options.tools && options.tools.length > 0 ? options.toolChoice ?? "auto" : undefined,
    parallel_tool_calls: options.useResponsesLite ? false : true,
    text: { verbosity: "medium" },
    client_metadata: options.useResponsesLite ? {
      "x-codex-installation-id": options.sessionId,
      session_id: options.sessionId,
      thread_id: options.sessionId,
      "x-codex-window-id": options.sessionId,
    } : undefined,
  };

  if (!options.useResponsesLite && functionTools.length > 0) {
    body.tools = functionTools;
  }

  if (options.reasoningEffort && options.reasoningEffort !== "off") {
    // Codex exposes Ultra as a client-level preset: it keeps the session/UI
    // state at Ultra (which enables proactive delegation) but sends Max over
    // the Responses wire protocol. Match the official Codex client mapping.
    const wireEffort = options.reasoningEffort === "ultra" ? "max" : options.reasoningEffort;
    body.reasoning = {
      effort: wireEffort,
      // The Codex backend's default_reasoning_summary is "none" for every
      // current model, so without an explicit summary request no
      // response.reasoning_summary_text.delta events arrive and the TUI never
      // shows Thinking. Ask for the server-chosen summary like Codex CLI does.
      summary: "auto",
      ...(options.useResponsesLite ? { context: "all_turns" } : {}),
    };
  }

  return body;
}

function convertMessage(message: ProviderMessage): Array<Record<string, unknown>> {
  if (message.role === "system") {
    return [];
  }

  if (message.role === "user") {
    if (typeof message.content === "string") {
      return [{
        role: "user",
        content: [{ type: "input_text", text: message.content }],
      }];
    }

    return [{
      role: "user",
      content: message.content.map((part) => {
        if (part.type === "text") {
          return { type: "input_text", text: part.text };
        }
        return { type: "input_image", detail: "auto", image_url: part.image_url.url };
      }),
    }];
  }

  if (message.role === "assistant") {
    const items: Array<Record<string, unknown>> = [];
    if (message.content) {
      items.push({
        type: "message",
        role: "assistant",
        status: "completed",
        content: [{ type: "output_text", text: message.content, annotations: [] }],
      });
    }
    if (message.toolCalls) {
      for (const toolCall of message.toolCalls) {
        items.push({
          type: "function_call",
          call_id: toolCall.id,
          name: toolCall.name,
          arguments: toolCall.arguments || "{}",
        });
      }
    }
    return items;
  }

  return [{
    type: "function_call_output",
    call_id: message.toolCallId,
    output: message.content,
  }];
}

async function* parseSse(response: Response): AsyncIterable<Record<string, unknown>> {
  if (!response.body) return;

  const reader = response.body.getReader();
  const decoder = new TextDecoder();
  let buffer = "";

  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      buffer += decoder.decode(value, { stream: true });

      let boundary = buffer.indexOf("\n\n");
      while (boundary >= 0) {
        const chunk = buffer.slice(0, boundary);
        buffer = buffer.slice(boundary + 2);

        const data = chunk
          .split("\n")
          .filter((line) => line.startsWith("data:"))
          .map((line) => line.slice(5).trim())
          .join("\n")
          .trim();

        if (data && data !== "[DONE]") {
          try {
            yield JSON.parse(data) as Record<string, unknown>;
          } catch {
            // Ignore malformed events.
          }
        }

        boundary = buffer.indexOf("\n\n");
      }
    }
  } finally {
    try {
      await reader.cancel();
    } catch {
      // Ignore cleanup errors.
    }
  }
}

function buildCodexRequestInit(options: {
  accessToken: string;
  accountId: string;
  sessionId: string;
  signal?: AbortSignal;
  body: string;
  useResponsesLite?: boolean;
}): RequestInit {
  const init: RequestInit & { verbose?: boolean } = {
    method: "POST",
    headers: buildSseHeaders(
      options.accessToken,
      options.accountId,
      options.sessionId,
      options.useResponsesLite,
    ),
    signal: options.signal,
    body: options.body,
    keepalive: false,
  };
  if (/^(1|true|yes)$/i.test(process.env.BUBBLE_CODEX_FETCH_VERBOSE ?? "")) {
    init.verbose = true;
  }
  return init;
}

function shouldRetryCodexTransportError(input: {
  error: unknown;
  attempt: number;
  sawParsedSseEvent: boolean;
  signal?: AbortSignal;
}): boolean {
  if (input.signal?.aborted) return false;
  if (input.sawParsedSseEvent) return false;
  if (input.attempt >= retryLimitFor(input.error, getProviderMaxRetries())) return false;
  return isTransientCodexTransportError(input.error) || isRetryableCodexHttpError(input.error);
}

function isRetryableCodexHttpError(error: unknown): boolean {
  if (!(error instanceof Error)) return false;
  const match = error.message.match(/^(\d{3}) status code/);
  if (!match) return false;
  return isRetryableHttpStatus(Number(match[1]));
}

function isTransientCodexTransportError(error: unknown): boolean {
  const text = errorMessageChain(error).join("\n");
  if (/\bAbortError\b/i.test(text)) return false;
  return [
    /The socket connection was closed unexpectedly/i,
    /\bConnectionClosed\b/i,
    /\bECONNRESET\b/i,
    /\bUND_ERR_SOCKET\b/i,
    /\bUND_ERR_(?:CONNECT|HEADERS|BODY)_TIMEOUT\b/i,
    /\bEPIPE\b/i,
    /socket hang up/i,
    /fetch failed/i,
    /Unable to connect\. Is the computer able to access the url\?/i,
    /unknown certificate verification error/i,
    /certificate (?:verify|verification) (?:failed|error)/i,
    /unable to verify (?:the )?(?:first )?certificate/i,
    /UNABLE_TO_(?:VERIFY_LEAF_SIGNATURE|GET_ISSUER_CERT_LOCALLY)/i,
    /SELF_SIGNED_CERT_IN_CHAIN/i,
    /CERT_(?:HAS_EXPIRED|UNTRUSTED|INVALID)/i,
  ].some((pattern) => pattern.test(text));
}

function errorMessageChain(error: unknown): string[] {
  const messages: string[] = [];
  let current: unknown = error;
  for (let depth = 0; current && depth < 6; depth++) {
    if (current instanceof Error) {
      messages.push(current.name, current.message);
      const code = (current as Error & { code?: unknown }).code;
      if (typeof code === "string") messages.push(code);
      current = (current as Error & { cause?: unknown }).cause;
      continue;
    }
    if (typeof current === "object") {
      const record = current as Record<string, unknown>;
      for (const key of ["name", "code", "message"]) {
        if (typeof record[key] === "string") messages.push(record[key]);
      }
      current = record.cause;
      continue;
    }
    messages.push(String(current));
    break;
  }
  return messages;
}

function buildBaseHeaders(
  accessToken: string,
  accountId: string,
  sessionId: string,
  extraHeaders?: Record<string, string>
): Headers {
  const headers = new Headers(extraHeaders);
  headers.set("Authorization", `Bearer ${accessToken}`);
  headers.set("ChatGPT-Account-Id", accountId);
  // The ChatGPT Codex backend gates newly listed models on the Codex client
  // identity as well as client_version. Keep the official originator token and
  // identify Bubble explicitly as a User-Agent suffix.
  headers.set("originator", "codex_cli_rs");
  headers.set("User-Agent", `codex_cli_rs/${CODEX_CLIENT_VERSION} (bubble)`);
  headers.set("session_id", sessionId);
  return headers;
}

function buildSseHeaders(
  accessToken: string,
  accountId: string,
  sessionId: string,
  useResponsesLite = false,
): Headers {
  const headers = buildBaseHeaders(accessToken, accountId, sessionId, {
    accept: "text/event-stream",
    "content-type": "application/json",
  });
  headers.set("OpenAI-Beta", OPENAI_BETA_RESPONSES);
  if (useResponsesLite) {
    headers.set("x-openai-internal-codex-responses-lite", "true");
    headers.set("thread_id", sessionId);
    headers.set("x-codex-window-id", sessionId);
    headers.set("x-codex-installation-id", sessionId);
  }
  return headers;
}

function resolveCodexUrl(baseURL: string): string {
  const normalized = (baseURL.trim() || DEFAULT_CODEX_BASE_URL).replace(/\/+$/, "");
  if (normalized.endsWith("/codex/responses")) return normalized;
  if (normalized.endsWith("/codex")) return `${normalized}/responses`;
  return `${normalized}/codex/responses`;
}

function resolveRelativeUrl(baseURL: string, path: string): string {
  const normalized = (baseURL.trim() || DEFAULT_CODEX_BASE_URL).replace(/\/+$/, "");
  return `${normalized}${path}`;
}

function extractCodexModelDescriptors(payload: unknown): CodexModelDescriptor[] {
  const out: CodexModelDescriptor[] = [];
  const seen = new Set<string>();

  const isCodexId = (value: unknown): value is string =>
    typeof value === "string" && /^gpt-|^codex-/i.test(value);

  const pickId = (record: Record<string, unknown>): string | undefined => {
    for (const key of ["slug", "id", "model_slug", "model"]) {
      const v = record[key];
      if (isCodexId(v)) return v;
    }
    return undefined;
  };

  const buildDescriptor = (record: Record<string, unknown>, id: string): CodexModelDescriptor => {
    const desc: CodexModelDescriptor = { id };

    const displayName = record.display_name;
    if (typeof displayName === "string" && displayName) desc.displayName = displayName;

    const ctx = record.context_window;
    if (typeof ctx === "number" && ctx > 0) desc.contextWindow = ctx;

    const useResponsesLite = record.use_responses_lite;
    if (typeof useResponsesLite === "boolean") desc.useResponsesLite = useResponsesLite;

    const visibility = record.visibility;
    if (typeof visibility === "string") desc.visibility = visibility;

    const minVer = record.minimal_client_version;
    if (typeof minVer === "string") desc.minimalClientVersion = minVer;

    const priority = record.priority;
    if (typeof priority === "number" && Number.isFinite(priority)) desc.priority = priority;

    const levels = record.supported_reasoning_levels;
    if (Array.isArray(levels)) {
      const efforts: ReasoningEffort[] = [];
      for (const level of levels) {
        const effort = (level as Record<string, unknown> | null | undefined)?.effort;
        if (
          typeof effort === "string"
          && (THINKING_LEVELS as readonly string[]).includes(effort)
          && !efforts.includes(effort as ReasoningEffort)
        ) {
          efforts.push(effort as ReasoningEffort);
        }
      }
      desc.reasoningLevels = efforts;

      const defaultLevel = record.default_reasoning_level;
      if (
        typeof defaultLevel === "string"
        && efforts.includes(defaultLevel as ReasoningEffort)
      ) {
        desc.defaultReasoningLevel = defaultLevel as ReasoningEffort;
      }
    }

    const truncPolicy = record.truncation_policy as Record<string, unknown> | undefined;
    if (truncPolicy && truncPolicy.mode === "tokens") {
      const limit = truncPolicy.limit;
      if (typeof limit === "number" && limit > 0) {
        desc.toolOutputTokenLimit = limit;
      }
    }

    return desc;
  };

  const visit = (value: unknown) => {
    if (Array.isArray(value)) {
      for (const item of value) visit(item);
      return;
    }
    if (!value || typeof value !== "object") return;

    const record = value as Record<string, unknown>;
    const id = pickId(record);
    if (id && !seen.has(id)) {
      seen.add(id);
      out.push(buildDescriptor(record, id));
    }

    for (const child of Object.values(record)) {
      if (child && typeof child === "object") visit(child);
    }
  };

  visit(payload);
  return out;
}

// Extracts the family version from a codex slug (e.g. "gpt-5.5-codex" → 5005,
// "gpt-6-astra" → 6000). Used so models from a newer family float to the top
// even before the static catalog knows about them. Integer-major slugs must
// rank above every older major.minor family, not fall to zero.
function parseCodexFamilyRank(id: string): number {
  const match = id.match(/(\d+)(?:\.(\d+))?/);
  if (!match) return 0;
  return parseInt(match[1], 10) * 1000 + (match[2] ? parseInt(match[2], 10) : 0);
}

export function sortCodexModelDescriptors(descriptors: CodexModelDescriptor[]): CodexModelDescriptor[] {
  const preferred = new Map<string, number>(
    getOpenAICodexFallbackModels().map((id, index) => [id, index]),
  );
  return [...descriptors].sort((left, right) => {
    const leftFamily = parseCodexFamilyRank(left.id);
    const rightFamily = parseCodexFamilyRank(right.id);
    if (leftFamily !== rightFamily) return rightFamily - leftFamily;
    const leftPriority = left.priority ?? Number.MAX_SAFE_INTEGER;
    const rightPriority = right.priority ?? Number.MAX_SAFE_INTEGER;
    if (leftPriority !== rightPriority) return leftPriority - rightPriority;
    const leftRank = preferred.get(left.id) ?? Number.MAX_SAFE_INTEGER;
    const rightRank = preferred.get(right.id) ?? Number.MAX_SAFE_INTEGER;
    if (leftRank !== rightRank) return leftRank - rightRank;
    return left.id.localeCompare(right.id);
  });
}
