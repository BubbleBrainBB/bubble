/**
 * AI SDK provider backend ("ai-sdk" protocol).
 *
 * Consumes AI SDK provider packages at the LanguageModelV3 spec layer
 * (`model.doStream`) rather than through the high-level `streamText` API:
 * pre-stream HTTP errors throw natively (which our retry/rate-limit contract
 * needs), tool-call inputs arrive as JSON strings (matching `argumentsFull`),
 * and no step machinery or extra dependencies come along.
 *
 * Registered providers: google (Gemini native API). Adding another AI SDK
 * provider is one entry in AI_SDK_PROVIDER_FACTORIES.
 */

import { createGoogleGenerativeAI } from "@ai-sdk/google";
import { APICallError } from "@ai-sdk/provider";
import type {
  LanguageModelV3,
  LanguageModelV3CallOptions,
  LanguageModelV3FilePart,
  LanguageModelV3Message,
  LanguageModelV3StreamPart,
  LanguageModelV3ToolResultOutput,
  LanguageModelV3Usage,
  SharedV3ProviderMetadata,
  SharedV3ProviderOptions,
} from "@ai-sdk/provider";
import { RateLimitError, type RateLimitPolicy } from "./network/errors.js";
import {
  ProviderStreamInterruptedError,
  computeRetryDelayMs,
  getProviderMaxRetries,
  isRetryableHttpStatus,
  sleepBeforeRetry,
} from "./network/retry.js";
import { createProviderFetch, isProviderTransportError, type ProviderFetch } from "./network/provider-transport.js";
import type {
  Provider,
  ProviderMessage,
  ProviderRawContentBlock,
  ReasoningEffort,
  StreamChunk,
  ThinkingLevel,
  TokenUsage,
  ToolChoiceMode,
  ToolDefinition,
} from "./types.js";

const GEMINI_DEFAULT_BASE_URL = "https://generativelanguage.googleapis.com/v1beta";
/**
 * Per-call channel from buildCallOptions to the fetch wrapper: carries a
 * thinking level the pinned @ai-sdk/google schema cannot express. Stripped
 * before the request leaves the process.
 */
const THINKING_LEVEL_OVERRIDE_HEADER = "x-bubble-thinking-level";

export interface AiSdkProviderOptions {
  providerId?: string;
  apiKey: string;
  baseURL?: string;
  thinkingLevel?: ThinkingLevel;
  /** User-configured extra headers, merged last (client-identity gates). */
  headers?: Record<string, string>;
  /** Transport override for tests; defaults to the shared provider fetch. */
  fetch?: ProviderFetch;
}

interface ChatOptions {
  model: string;
  tools?: ToolDefinition[];
  toolChoice?: ToolChoiceMode;
  temperature?: number;
  thinkingLevel?: ThinkingLevel;
  abortSignal?: AbortSignal;
  rateLimitPolicy?: RateLimitPolicy;
}

type ModelFactory = (options: AiSdkProviderOptions) => (modelId: string) => LanguageModelV3;

const AI_SDK_PROVIDER_FACTORIES: Record<string, ModelFactory> = {
  google: (options) => {
    const provider = createGoogleGenerativeAI({
      apiKey: options.apiKey,
      ...(options.baseURL ? { baseURL: normalizeBaseURL(options.baseURL) } : {}),
      ...(options.headers ? { headers: options.headers } : {}),
      fetch: withThinkingLevelOverride(options.fetch ?? createProviderFetch({
        providerName: "Google Gemini",
        verboseEnvVar: "BUBBLE_AI_SDK_FETCH_VERBOSE",
      })) as typeof globalThis.fetch,
    });
    return (modelId) => provider(modelId);
  },
};

export function isAiSdkProviderId(providerId: string | undefined): boolean {
  return !!providerId && providerId in AI_SDK_PROVIDER_FACTORIES;
}

export function createAiSdkProvider(options: AiSdkProviderOptions): Provider {
  const providerId = options.providerId || "google";
  const factory = AI_SDK_PROVIDER_FACTORIES[providerId];
  if (!factory) {
    const known = Object.keys(AI_SDK_PROVIDER_FACTORIES).join(", ");
    throw new Error(
      `Provider "${providerId}" is configured with protocol "ai-sdk" but no AI SDK backend is registered for it (known: ${known}).`,
    );
  }
  const getModel = factory(options);

  async function* streamChat(
    messages: ProviderMessage[],
    chatOptions: ChatOptions,
  ): AsyncIterable<StreamChunk> {
    const model = getModel(chatOptions.model);
    const callOptions = buildCallOptions(messages, chatOptions, options);
    const maxRetries = getProviderMaxRetries();

    for (let attempt = 0; ; attempt++) {
      let stream: ReadableStream<LanguageModelV3StreamPart>;
      try {
        ({ stream } = await model.doStream(callOptions));
      } catch (error) {
        // No stream established: the request is safe to classify and re-issue.
        handlePreStreamError(error, {
          attempt,
          maxRetries,
          rateLimitPolicy: chatOptions.rateLimitPolicy,
          signal: chatOptions.abortSignal,
        });
        await sleepBeforeRetry(
          computeRetryDelayMs(attempt + 1, { retryAfterMs: retryAfterMsFromError(error) }),
          chatOptions.abortSignal,
        );
        continue;
      }

      const translator = new StreamTranslator();
      let surfacedContent = false;
      try {
        for await (const part of stream) {
          // The SDK surfaces post-200 failures as error parts and lets the
          // stream "finish" normally; that would look like an empty reply to
          // the agent loop, so convert them back into throws here.
          if (part.type === "error") {
            throw coerceError(part.error);
          }
          for (const chunk of translator.translate(part)) {
            surfacedContent = surfacedContent || isContentChunk(chunk);
            yield chunk;
          }
        }
        yield { type: "done" };
        return;
      } catch (error) {
        if (chatOptions.abortSignal?.aborted) throw coerceError(error);
        if (surfacedContent) {
          // Partial content already reached the UI — only the agent loop can
          // discard the half-built assistant message and re-issue the request.
          throw new ProviderStreamInterruptedError(
            `Gemini stream interrupted: ${errorMessage(error)}`,
            { cause: error },
          );
        }
        if (attempt >= maxRetries || !isRetryableStreamError(error)) throw coerceError(error);
        await sleepBeforeRetry(computeRetryDelayMs(attempt + 1), chatOptions.abortSignal);
      }
    }
  }

  async function complete(
    messages: ProviderMessage[],
    completeOptions?: { model?: string; temperature?: number; thinkingLevel?: ThinkingLevel; abortSignal?: AbortSignal },
  ): Promise<string> {
    const modelId = completeOptions?.model;
    if (!modelId) throw new Error("ai-sdk provider requires an explicit model for complete().");
    const model = getModel(modelId);
    const callOptions = buildCallOptions(messages, {
      model: modelId,
      temperature: completeOptions?.temperature,
      thinkingLevel: completeOptions?.thinkingLevel ?? "off",
      abortSignal: completeOptions?.abortSignal,
    }, options);
    const result = await model.doGenerate(callOptions);
    return result.content
      .filter((part): part is Extract<typeof part, { type: "text" }> => part.type === "text")
      .map((part) => part.text)
      .join("");
  }

  return { streamChat, complete };
}

// ============================================================================
// Request building
// ============================================================================

function buildCallOptions(
  messages: ProviderMessage[],
  chatOptions: ChatOptions,
  providerOptions: AiSdkProviderOptions,
): LanguageModelV3CallOptions {
  const prompt = convertMessages(messages);
  const thinkingLevel = chatOptions.thinkingLevel ?? providerOptions.thinkingLevel;
  const thinking = buildGoogleThinkingOptions(chatOptions.model, thinkingLevel);
  const levelOverride = googleThinkingLevelOverride(chatOptions.model, thinkingLevel);
  return {
    prompt,
    ...(levelOverride ? { headers: { [THINKING_LEVEL_OVERRIDE_HEADER]: levelOverride } } : {}),
    ...(chatOptions.temperature !== undefined ? { temperature: chatOptions.temperature } : {}),
    ...(chatOptions.abortSignal ? { abortSignal: chatOptions.abortSignal } : {}),
    ...(chatOptions.tools?.length
      ? {
        tools: chatOptions.tools.map((tool) => ({
          type: "function" as const,
          name: tool.name,
          description: tool.description,
          inputSchema: tool.parameters as unknown as Record<string, unknown>,
        })),
        toolChoice: { type: chatOptions.toolChoice === "none" ? "none" as const : "auto" as const },
      }
      : {}),
    ...(thinking ? { providerOptions: { google: thinking } as SharedV3ProviderOptions } : {}),
  };
}

/**
 * Gemini 3+ models take a graded thinking_level; 2.5-era models take a token
 * budget. "off" (budget 0) is only offered in the catalog for models that
 * accept it (2.5 Flash); xhigh/max/ultra clamp to high here, and models with
 * a native xhigh get it back through googleThinkingLevelOverride.
 */
function buildGoogleThinkingOptions(
  modelId: string,
  level: ThinkingLevel | undefined,
): Record<string, unknown> | undefined {
  if (!level) return undefined;
  const version = geminiVersion(modelId);
  // Gemini 4 accepts only low/medium/high: MINIMAL is a 400 and thinkingBudget
  // is undocumented. Side calls (titles, compaction) request "off" by default,
  // so floor those at low rather than sending a value the API rejects.
  if (version >= 4 && (level === "off" || level === "minimal")) level = "low";
  if (level === "off") return { thinkingConfig: { thinkingBudget: 0 } };
  const clamped = level === "xhigh" || level === "max" || level === "ultra" ? "high" : level;
  if (version >= 3) {
    return { thinkingConfig: { thinkingLevel: clamped, includeThoughts: true } };
  }
  const budgets: Record<string, number> = { minimal: 512, low: 2048, medium: 8192, high: 24576 };
  return { thinkingConfig: { thinkingBudget: budgets[clamped] ?? 8192, includeThoughts: true } };
}

/**
 * The @ai-sdk/google v3 schema only admits minimal/low/medium/high, so a model
 * with a native xhigh (Gemini 4 Hydrogen) is sent "high" through the SDK and
 * the real level is written into the request body by the fetch wrapper.
 */
function googleThinkingLevelOverride(
  modelId: string,
  level: ThinkingLevel | undefined,
): "xhigh" | undefined {
  if (level !== "xhigh" && level !== "max" && level !== "ultra") return undefined;
  return geminiReasoningLevels(modelId).includes("xhigh") ? "xhigh" : undefined;
}

function withThinkingLevelOverride(fetchImpl: ProviderFetch): ProviderFetch {
  return async (input, init) => {
    const headers = new Headers(init?.headers);
    const level = headers.get(THINKING_LEVEL_OVERRIDE_HEADER);
    if (!level) return fetchImpl(input, init);
    headers.delete(THINKING_LEVEL_OVERRIDE_HEADER);
    let body = init?.body;
    if (typeof body === "string") {
      const parsed = JSON.parse(body) as { generationConfig?: { thinkingConfig?: Record<string, unknown> } };
      parsed.generationConfig = {
        ...parsed.generationConfig,
        thinkingConfig: { ...parsed.generationConfig?.thinkingConfig, thinkingLevel: level },
      };
      body = JSON.stringify(parsed);
    }
    return fetchImpl(input, { ...init, headers: Object.fromEntries(headers), body });
  };
}

function convertMessages(messages: ProviderMessage[]): LanguageModelV3Message[] {
  const out: LanguageModelV3Message[] = [];
  const toolNamesById = new Map<string, string>();

  for (const message of messages) {
    if (message.role === "system") {
      out.push({ role: "system", content: message.content });
      continue;
    }
    if (message.role === "user") {
      out.push({ role: "user", content: convertUserContent(message.content) });
      continue;
    }
    if (message.role === "assistant") {
      for (const call of message.toolCalls ?? []) toolNamesById.set(call.id, call.name);
      const content = convertAssistantContent(message);
      if (content.length > 0) out.push({ role: "assistant", content });
      continue;
    }
    // tool result
    out.push({
      role: "tool",
      content: [{
        type: "tool-result",
        toolCallId: message.toolCallId,
        toolName: toolNamesById.get(message.toolCallId) ?? "unknown_tool",
        output: toToolResultOutput(message.content, message.isError),
      }],
    });
  }
  return out;
}

function convertUserContent(
  content: string | Array<{ type: string; text?: string; image_url?: { url: string } }>,
): Array<{ type: "text"; text: string } | LanguageModelV3FilePart> {
  if (typeof content === "string") return [{ type: "text", text: content }];
  const parts: Array<{ type: "text"; text: string } | LanguageModelV3FilePart> = [];
  for (const part of content) {
    if (part.type === "text" && typeof part.text === "string") {
      parts.push({ type: "text", text: part.text });
    } else if (part.type === "image_url" && part.image_url?.url) {
      parts.push(toImageFilePart(part.image_url.url));
    }
  }
  return parts.length > 0 ? parts : [{ type: "text", text: "" }];
}

function toImageFilePart(url: string): LanguageModelV3FilePart {
  const dataUrl = /^data:([^;,]+)?(;base64)?,(.*)$/s.exec(url);
  if (dataUrl) {
    return {
      type: "file",
      mediaType: dataUrl[1] || "image/png",
      data: dataUrl[2] ? dataUrl[3] : decodeURIComponent(dataUrl[3]),
    };
  }
  const extension = /\.(png|jpe?g|gif|webp)(?:[?#]|$)/i.exec(url)?.[1]?.toLowerCase();
  const mediaType = extension === "jpg" || extension === "jpeg"
    ? "image/jpeg"
    : extension
      ? `image/${extension}`
      : "image/png";
  return { type: "file", mediaType, data: new URL(url) };
}

type AssistantContentPart =
  | { type: "text"; text: string; providerOptions?: SharedV3ProviderOptions }
  | { type: "reasoning"; text: string; providerOptions?: SharedV3ProviderOptions }
  | { type: "tool-call"; toolCallId: string; toolName: string; input: unknown; providerOptions?: SharedV3ProviderOptions };

function convertAssistantContent(
  message: Extract<ProviderMessage, { role: "assistant" }>,
): AssistantContentPart[] {
  const parts: AssistantContentPart[] = [];
  const blocks = message.providerMetadata?.google?.contentBlocks ?? [];
  const toolCallSignatures = new Map<string, string>();

  // Replay captured Gemini parts: signed reasoning comes back verbatim so the
  // thought signature round-trips; tool-call signatures re-attach by id.
  for (const block of blocks) {
    if (block.type === "reasoning" && typeof block.text === "string" && typeof block.thoughtSignature === "string") {
      parts.push({
        type: "reasoning",
        text: block.text,
        providerOptions: { google: { thoughtSignature: block.thoughtSignature } },
      });
    } else if (block.type === "tool-call" && typeof block.toolCallId === "string" && typeof block.thoughtSignature === "string") {
      toolCallSignatures.set(block.toolCallId, block.thoughtSignature);
    }
  }

  if (message.content) parts.push({ type: "text", text: message.content });

  for (const call of message.toolCalls ?? []) {
    const signature = toolCallSignatures.get(call.id);
    parts.push({
      type: "tool-call",
      toolCallId: call.id,
      toolName: call.name,
      input: parseToolArguments(call.arguments),
      ...(signature ? { providerOptions: { google: { thoughtSignature: signature } } } : {}),
    });
  }
  return parts;
}

function parseToolArguments(raw: string): Record<string, unknown> {
  if (!raw) return {};
  try {
    const parsed = JSON.parse(raw) as unknown;
    return parsed && typeof parsed === "object" ? parsed as Record<string, unknown> : {};
  } catch {
    return {};
  }
}

function toToolResultOutput(content: string, isError: boolean | undefined): LanguageModelV3ToolResultOutput {
  return isError ? { type: "error-text", value: content } : { type: "text", value: content };
}

function normalizeBaseURL(baseURL: string): string {
  return baseURL.trim().replace(/\/+$/, "");
}

// ============================================================================
// Stream translation
// ============================================================================

class StreamTranslator {
  private reasoningText = "";
  private reasoningSignature: string | undefined;
  private toolNamesById = new Map<string, string>();

  translate(part: LanguageModelV3StreamPart): StreamChunk[] {
    switch (part.type) {
      case "text-delta":
        return part.delta ? [{ type: "text", content: part.delta }] : [];

      case "reasoning-start":
        this.reasoningText = "";
        this.reasoningSignature = thoughtSignatureOf(part.providerMetadata);
        return [];

      case "reasoning-delta": {
        this.reasoningText += part.delta;
        this.reasoningSignature ??= thoughtSignatureOf(part.providerMetadata);
        return part.delta ? [{ type: "reasoning_delta", content: part.delta }] : [];
      }

      case "reasoning-end": {
        this.reasoningSignature ??= thoughtSignatureOf(part.providerMetadata);
        if (!this.reasoningSignature || !this.reasoningText) return [];
        const block: ProviderRawContentBlock = {
          type: "reasoning",
          text: this.reasoningText,
          thoughtSignature: this.reasoningSignature,
        };
        this.reasoningText = "";
        this.reasoningSignature = undefined;
        return [{ type: "provider_content_block", provider: "google", block }];
      }

      case "tool-input-start":
        this.toolNamesById.set(part.id, part.toolName);
        return [{ type: "tool_call", id: part.id, name: part.toolName, arguments: "", isStart: true, isEnd: false }];

      case "tool-input-delta": {
        if (!part.delta) return [];
        const name = this.toolNamesById.get(part.id) ?? "";
        return [{ type: "tool_call", id: part.id, name, arguments: part.delta, isStart: false, isEnd: false }];
      }

      case "tool-call": {
        const argumentsFull = typeof part.input === "string" ? part.input : JSON.stringify(part.input ?? {});
        const chunks: StreamChunk[] = [{
          type: "tool_call",
          id: part.toolCallId,
          name: part.toolName,
          arguments: "",
          isStart: false,
          isEnd: true,
          argumentsFull,
        }];
        const signature = thoughtSignatureOf(part.providerMetadata);
        if (signature) {
          chunks.push({
            type: "provider_content_block",
            provider: "google",
            block: { type: "tool-call", toolCallId: part.toolCallId, thoughtSignature: signature },
          });
        }
        return chunks;
      }

      case "finish": {
        const usage = translateUsage(part.usage);
        return usage ? [{ type: "usage", usage }] : [];
      }

      default:
        // stream-start, response-metadata, text-start/end, tool-input-end,
        // source, raw: nothing to surface.
        return [];
    }
  }
}

function thoughtSignatureOf(metadata: SharedV3ProviderMetadata | undefined): string | undefined {
  const signature = metadata?.google?.thoughtSignature;
  return typeof signature === "string" && signature.length > 0 ? signature : undefined;
}

function translateUsage(usage: LanguageModelV3Usage | undefined): TokenUsage | undefined {
  const inputTokens = usage?.inputTokens?.total;
  const outputTokens = usage?.outputTokens?.total;
  // Without a real input count the chunk would poison the agent's context
  // budget tracking (lastInputTokens), so skip rather than report zeros.
  if (!Number.isFinite(inputTokens)) return undefined;
  const result: TokenUsage = {
    promptTokens: inputTokens as number,
    completionTokens: Number.isFinite(outputTokens) ? outputTokens as number : 0,
  };
  const cacheRead = usage?.inputTokens?.cacheRead;
  const noCache = usage?.inputTokens?.noCache;
  const cacheWrite = usage?.inputTokens?.cacheWrite;
  const reasoning = usage?.outputTokens?.reasoning;
  if (Number.isFinite(cacheRead)) result.promptCacheHitTokens = cacheRead as number;
  if (Number.isFinite(noCache)) result.promptCacheMissTokens = noCache as number;
  if (Number.isFinite(cacheWrite)) result.cacheCreationTokens = cacheWrite as number;
  if (Number.isFinite(reasoning)) result.reasoningTokens = reasoning as number;
  if (Number.isFinite(inputTokens) && Number.isFinite(outputTokens)) {
    result.totalTokens = (inputTokens as number) + (outputTokens as number);
  }
  return result;
}

function isContentChunk(chunk: StreamChunk): boolean {
  return chunk.type === "text" || chunk.type === "reasoning_delta" || chunk.type === "tool_call";
}

// ============================================================================
// Error handling
// ============================================================================

/**
 * Classify a pre-stream error. Throws (RateLimitError / the original error)
 * when the request must not be retried here; returns normally when the caller
 * should back off and retry.
 */
function handlePreStreamError(
  error: unknown,
  context: { attempt: number; maxRetries: number; rateLimitPolicy?: RateLimitPolicy; signal?: AbortSignal },
): void {
  if (context.signal?.aborted) throw coerceError(error);

  const status = APICallError.isInstance(error) ? error.statusCode : undefined;
  if (status === 429) {
    const retryAfterMs = retryAfterMsFromError(error);
    if (context.rateLimitPolicy === "defer") {
      // Rate-limit contract: under "defer" the transport does no 429 backoff;
      // the subagent scheduler owns it.
      throw new RateLimitError(`Gemini API rate limited (429): ${errorMessage(error)}`, {
        status: 429,
        retryAfterMs,
        cause: error,
      });
    }
    if (context.attempt >= context.maxRetries) {
      throw new RateLimitError(
        `Gemini API rate limited (429) after ${context.attempt + 1} attempts: ${errorMessage(error)}`,
        { status: 429, retryAfterMs, cause: error },
      );
    }
    return;
  }

  const retryable = APICallError.isInstance(error)
    ? (error.isRetryable || (typeof status === "number" && isRetryableHttpStatus(status)))
    : isProviderTransportError(error);
  if (!retryable || context.attempt >= context.maxRetries) throw coerceError(error);
}

function retryAfterMsFromError(error: unknown): number | undefined {
  if (!APICallError.isInstance(error)) return undefined;
  const header = error.responseHeaders?.["retry-after"]?.trim();
  if (!header) return undefined;
  const seconds = Number(header);
  if (Number.isFinite(seconds) && seconds >= 0) return Math.round(seconds * 1000);
  const date = Date.parse(header);
  if (!Number.isNaN(date)) return Math.max(0, date - Date.now());
  return undefined;
}

function isRetryableStreamError(error: unknown): boolean {
  if (APICallError.isInstance(error)) {
    return error.isRetryable || (typeof error.statusCode === "number" && isRetryableHttpStatus(error.statusCode));
  }
  return isProviderTransportError(error);
}

function coerceError(error: unknown): Error {
  return error instanceof Error ? error : new Error(String(error));
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

// ============================================================================
// Model discovery (GET /v1beta/models)
// ============================================================================

export interface GeminiModelDescriptor {
  id: string;
  name: string;
  contextWindow?: number;
  reasoningLevels: ReasoningEffort[];
  defaultReasoningLevel?: ReasoningEffort;
}

interface GeminiModelListEntry {
  name?: string;
  displayName?: string;
  inputTokenLimit?: number;
  supportedGenerationMethods?: string[];
}

// Specialised variants that are not general text/coding models.
const GEMINI_MODEL_EXCLUDE = /(tts|image|audio|embedding|live|computer-use|robotics|banana|aqa|learnlm|gemma|imagen|veo)/i;

/**
 * Fetch Google's model list and keep the newest general-purpose text models.
 * Newly released Gemini versions appear without a catalog change; the static
 * BUILTIN_MODELS entries stay as the offline/no-key fallback.
 */
export async function fetchGeminiModels(options: {
  apiKey: string;
  baseURL?: string;
  fetch?: ProviderFetch;
  limit?: number;
}): Promise<GeminiModelDescriptor[]> {
  const baseURL = normalizeBaseURL(options.baseURL || GEMINI_DEFAULT_BASE_URL);
  const fetchImpl = options.fetch ?? createProviderFetch({
    providerName: "Google Gemini",
    verboseEnvVar: "BUBBLE_AI_SDK_FETCH_VERBOSE",
  });
  const response = await fetchImpl(`${baseURL}/models?pageSize=1000`, {
    headers: { "x-goog-api-key": options.apiKey },
  });
  if (!response.ok) {
    throw new Error(`Gemini model list failed (${response.status}): ${await response.text().catch(() => response.statusText)}`);
  }
  const data = await response.json() as { models?: GeminiModelListEntry[] };
  return selectLatestGeminiModels(data.models ?? [], options.limit ?? 5);
}

/** Model tiers live discovery keeps; any other name is filtered out. */
const GEMINI_TEXT_TIERS = ["argon", "carbon", "hydrogen", "pro", "flash"] as const;
const GEMINI_TEXT_MODEL_ID = new RegExp(`^gemini-(\\d+(?:\\.\\d+)?)-(${GEMINI_TEXT_TIERS.join("|")})(-lite)?`);
/**
 * Names the selection rules of selectLatestGeminiModels. It is part of the
 * discovery cache key: a list chosen by a build that did not know a tier (an
 * older install, or an older Bubble still running and sharing the disk cache)
 * must not satisfy this build, or new models stay hidden from /model for as
 * long as that cached list lives.
 */
export const GEMINI_MODEL_SELECTION_SCOPE = `tiers:${GEMINI_TEXT_TIERS.join(",")}`;

/**
 * Ranking: one entry per (version, tier) family — GA ids beat dated previews —
 * then newest version first, pro/argon before carbon before hydrogen before
 * flash before flash-lite, top N.
 */
export function selectLatestGeminiModels(
  entries: GeminiModelListEntry[],
  limit = 5,
): GeminiModelDescriptor[] {
  interface Candidate {
    id: string;
    entry: GeminiModelListEntry;
    version: number;
    tier: string;
    tierRank: number;
    isPreview: boolean;
  }

  // Gemini 4 introduced named tiers (argon, carbon, hydrogen). Unknown names
  // stay excluded rather than guessing whether they are text models. Argon
  // stays ahead of Carbon to match the static catalog's default model.
  const TIER_RANK: Record<string, number> = { argon: 0, pro: 0, carbon: 1, hydrogen: 2, flash: 3, "flash-lite": 4 };
  const candidates: Candidate[] = [];
  for (const entry of entries) {
    const id = (entry.name ?? "").replace(/^models\//, "");
    if (!id.startsWith("gemini-")) continue;
    if (GEMINI_MODEL_EXCLUDE.test(id)) continue;
    if (entry.supportedGenerationMethods && !entry.supportedGenerationMethods.includes("generateContent")) continue;
    const match = GEMINI_TEXT_MODEL_ID.exec(id);
    if (!match) continue;
    const tier = `${match[2]}${match[3] ?? ""}`;
    candidates.push({
      id,
      entry,
      version: Number.parseFloat(match[1]),
      tier,
      tierRank: TIER_RANK[tier] ?? 5,
      isPreview: id.includes("preview") || id.includes("exp"),
    });
  }

  // One winner per family: GA over preview, then the shortest (least-suffixed) id.
  const families = new Map<string, Candidate>();
  for (const candidate of candidates) {
    const key = `${candidate.version}-${candidate.tier}`;
    const current = families.get(key);
    if (
      !current
      || (current.isPreview && !candidate.isPreview)
      || (current.isPreview === candidate.isPreview && candidate.id.length < current.id.length)
    ) {
      families.set(key, candidate);
    }
  }

  return [...families.values()]
    .sort((a, b) => (b.version - a.version) || (a.tierRank - b.tierRank))
    .slice(0, limit)
    .map((candidate) => ({
      id: candidate.id,
      name: candidate.entry.displayName || candidate.id,
      contextWindow: Number.isFinite(candidate.entry.inputTokenLimit) ? candidate.entry.inputTokenLimit : undefined,
      reasoningLevels: geminiReasoningLevels(candidate.id),
      // Argon/Carbon (medium) and Hydrogen (high) take Google's defaults from the catalog.
      ...(candidate.tier === "pro" ? { defaultReasoningLevel: "high" as ReasoningEffort } : {}),
    }));
}

function geminiVersion(modelId: string): number {
  return Number.parseFloat(/^gemini-(\d+(?:\.\d+)?)/.exec(modelId)?.[1] ?? "0");
}

/** Mirrors the static catalog's per-family thinking support. */
export function geminiReasoningLevels(modelId: string): ReasoningEffort[] {
  const version = geminiVersion(modelId);
  const isPro = modelId.includes("-pro");
  // Hydrogen is the only Gemini 4 tier with xhigh; Argon and Carbon return 400 for it.
  if (version >= 4 && /^gemini-[\d.]+-hydrogen/.test(modelId)) return ["low", "medium", "high", "xhigh"];
  if (version >= 4) return ["low", "medium", "high"];
  // Live probes show 3.7 and 3.8 Flash share this newer ladder. Keep the
  // exception bounded to verified releases instead of guessing for 3.9+.
  if (!isPro && (version === 3.7 || version === 3.8)) {
    return ["off", "low", "medium", "high"];
  }
  if (version >= 3) {
    return isPro ? ["low", "medium", "high"] : ["minimal", "low", "medium", "high"];
  }
  if (version >= 2.5) {
    return isPro ? ["low", "medium", "high"] : ["off", "low", "medium", "high"];
  }
  return ["off"];
}
