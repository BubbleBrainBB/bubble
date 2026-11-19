import { afterEach, describe, expect, it } from "vitest";
import { createAiSdkProvider, fetchGeminiModels, geminiReasoningLevels, selectLatestGeminiModels } from "../provider-ai-sdk.js";
import { RateLimitError } from "../network/errors.js";
import { isProviderStreamInterruption } from "../network/retry.js";
import type { ProviderFetch } from "../network/provider-transport.js";
import type { ProviderMessage, StreamChunk, ThinkingLevel } from "../types.js";

const sse = (objs: unknown[]) => objs.map((o) => `data: ${JSON.stringify(o)}\n\n`).join("");

const HAPPY_CHUNKS = [
  {
    candidates: [{
      content: { parts: [{ text: "Thinking about it.", thought: true, thoughtSignature: "SIG_REASONING" }], role: "model" },
    }],
  },
  {
    candidates: [{
      content: { parts: [{ text: "Checking now. " }], role: "model" },
    }],
  },
  {
    candidates: [{
      content: {
        parts: [{
          functionCall: { name: "get_weather", args: { city: "Beijing" } },
          thoughtSignature: "SIG_TOOL",
        }],
        role: "model",
      },
      finishReason: "STOP",
    }],
    usageMetadata: {
      promptTokenCount: 120,
      candidatesTokenCount: 45,
      totalTokenCount: 205,
      thoughtsTokenCount: 40,
      cachedContentTokenCount: 80,
    },
  },
];

function sseResponse(body: string): Response {
  return new Response(body, { status: 200, headers: { "content-type": "text/event-stream" } });
}

function makeProvider(fetchImpl: ProviderFetch) {
  return createAiSdkProvider({ providerId: "google", apiKey: "test-key", fetch: fetchImpl });
}

async function collect(iterable: AsyncIterable<StreamChunk>): Promise<StreamChunk[]> {
  const chunks: StreamChunk[] = [];
  for await (const chunk of iterable) chunks.push(chunk);
  return chunks;
}

const USER_MESSAGES: ProviderMessage[] = [
  { role: "system", content: "You are helpful." },
  { role: "user", content: "Weather in Beijing?" },
];

const WEATHER_TOOL = {
  name: "get_weather",
  description: "Get weather",
  parameters: { type: "object" as const, properties: { city: { type: "string" } }, required: ["city"] },
};

afterEach(() => {
  delete process.env.BUBBLE_PROVIDER_MAX_RETRIES;
});

describe("ai-sdk provider stream translation", () => {
  it("translates the full event sequence with tool-call contract and usage", async () => {
    const provider = makeProvider(async () => sseResponse(sse(HAPPY_CHUNKS)));
    const chunks = await collect(provider.streamChat(USER_MESSAGES, {
      model: "gemini-2.5-flash",
      tools: [WEATHER_TOOL],
      toolChoice: "auto",
    }));

    const types = chunks.map((chunk) => chunk.type);
    expect(types).toEqual([
      "reasoning_delta",
      "provider_content_block",
      "text",
      "tool_call", // isStart
      "tool_call", // args delta
      "tool_call", // isEnd
      "provider_content_block",
      "usage",
      "done",
    ]);

    expect(chunks[0]).toMatchObject({ type: "reasoning_delta", content: "Thinking about it." });
    expect(chunks[1]).toMatchObject({
      type: "provider_content_block",
      provider: "google",
      block: { type: "reasoning", text: "Thinking about it.", thoughtSignature: "SIG_REASONING" },
    });
    expect(chunks[2]).toMatchObject({ type: "text", content: "Checking now. " });

    const [start, delta, end] = chunks.filter((c) => c.type === "tool_call");
    expect(start).toMatchObject({ name: "get_weather", isStart: true, isEnd: false });
    expect(delta).toMatchObject({ isStart: false, isEnd: false });
    expect(end).toMatchObject({ isStart: false, isEnd: true, argumentsFull: '{"city":"Beijing"}' });
    expect((start as { id: string }).id).toBe((end as { id: string }).id);

    expect(chunks[6]).toMatchObject({
      type: "provider_content_block",
      provider: "google",
      block: { type: "tool-call", thoughtSignature: "SIG_TOOL" },
    });

    expect(chunks[7]).toMatchObject({
      type: "usage",
      usage: {
        promptTokens: 120,
        completionTokens: 85,
        promptCacheHitTokens: 80,
        promptCacheMissTokens: 40,
        reasoningTokens: 40,
        totalTokens: 205,
      },
    });
  });

  it("omits the usage chunk when the response has no usage metadata", async () => {
    const provider = makeProvider(async () => sseResponse(sse([HAPPY_CHUNKS[1], {
      candidates: [{ content: { parts: [] as unknown[], role: "model" }, finishReason: "STOP" }],
    }])));
    const chunks = await collect(provider.streamChat(USER_MESSAGES, { model: "gemini-2.5-flash" }));
    expect(chunks.some((chunk) => chunk.type === "usage")).toBe(false);
    expect(chunks.at(-1)).toEqual({ type: "done" });
  });
});

describe("ai-sdk provider rate limiting", () => {
  const rateLimited = () => new Response(
    JSON.stringify({ error: { code: 429, message: "Resource exhausted", status: "RESOURCE_EXHAUSTED" } }),
    { status: 429, headers: { "content-type": "application/json", "retry-after": "7" } },
  );

  it('throws RateLimitError immediately under "defer" without transport retries', async () => {
    let calls = 0;
    const provider = makeProvider(async () => { calls++; return rateLimited(); });
    const error = await collect(provider.streamChat(USER_MESSAGES, {
      model: "gemini-2.5-flash",
      rateLimitPolicy: "defer",
    })).then(() => undefined, (e: unknown) => e);

    expect(error).toBeInstanceOf(RateLimitError);
    expect((error as RateLimitError).status).toBe(429);
    expect((error as RateLimitError).retryAfterMs).toBe(7000);
    expect(calls).toBe(1);
  });

  it('retries under "handle" and surfaces RateLimitError after the budget', async () => {
    process.env.BUBBLE_PROVIDER_MAX_RETRIES = "1";
    let calls = 0;
    const provider = makeProvider(async () => { calls++; return rateLimited(); });
    const error = await collect(provider.streamChat(USER_MESSAGES, {
      model: "gemini-2.5-flash",
    })).then(() => undefined, (e: unknown) => e);

    expect(error).toBeInstanceOf(RateLimitError);
    expect(calls).toBe(2);
  });
});

describe("ai-sdk provider stream interruption", () => {
  it("wraps mid-stream failures after content in ProviderStreamInterruptedError", async () => {
    const body = sse([HAPPY_CHUNKS[1]]) + "data: {broken json\n\n";
    const provider = makeProvider(async () => sseResponse(body));
    const chunks: StreamChunk[] = [];
    const error = await (async () => {
      try {
        for await (const chunk of provider.streamChat(USER_MESSAGES, { model: "gemini-2.5-flash" })) {
          chunks.push(chunk);
        }
        return undefined;
      } catch (e) {
        return e;
      }
    })();

    expect(chunks.some((chunk) => chunk.type === "text")).toBe(true);
    expect(isProviderStreamInterruption(error)).toBe(true);
  });

  it("does not treat non-2xx errors as empty replies", async () => {
    const provider = makeProvider(async () => new Response(
      JSON.stringify({ error: { code: 400, message: "Invalid argument", status: "INVALID_ARGUMENT" } }),
      { status: 400, statusText: "Bad Request", headers: { "content-type": "application/json" } },
    ));
    await expect(collect(provider.streamChat(USER_MESSAGES, { model: "gemini-2.5-flash" })))
      .rejects.toThrow(/Invalid argument|400/);
  });
});

describe("ai-sdk provider request building", () => {
  async function captureBody(
    messages: ProviderMessage[],
    options: { model: string; thinkingLevel?: ThinkingLevel },
  ): Promise<Record<string, any>> {
    let body: Record<string, any> = {};
    const provider = makeProvider(async (_url, init) => {
      body = JSON.parse(String(init?.body ?? "{}"));
      return sseResponse(sse([HAPPY_CHUNKS[1], {
        candidates: [{ content: { parts: [] as unknown[], role: "model" }, finishReason: "STOP" }],
      }]));
    });
    await collect(provider.streamChat(messages, options));
    return body;
  }

  it("extracts system messages into systemInstruction", async () => {
    const body = await captureBody(USER_MESSAGES, { model: "gemini-2.5-flash" });
    expect(body.systemInstruction).toEqual({ parts: [{ text: "You are helpful." }] });
    expect(body.contents).toHaveLength(1);
    expect(body.contents[0].role).toBe("user");
  });

  it("maps data-URL images to inlineData parts", async () => {
    const body = await captureBody([
      {
        role: "user",
        content: [
          { type: "text", text: "What is this?" },
          { type: "image_url", image_url: { url: "data:image/jpeg;base64,aGVsbG8=" } },
        ],
      },
    ], { model: "gemini-2.5-flash" });
    const parts = body.contents[0].parts;
    expect(parts[0]).toEqual({ text: "What is this?" });
    expect(parts[1]).toEqual({ inlineData: { mimeType: "image/jpeg", data: "aGVsbG8=" } });
  });

  it("backfills toolName on tool results and replays thought signatures", async () => {
    const body = await captureBody([
      { role: "user", content: "Weather?" },
      {
        role: "assistant",
        content: "",
        toolCalls: [{ id: "call_1", name: "get_weather", arguments: '{"city":"Beijing"}' }],
        providerMetadata: {
          google: {
            contentBlocks: [
              { type: "reasoning", text: "Thinking about it.", thoughtSignature: "SIG_REASONING" },
              { type: "tool-call", toolCallId: "call_1", thoughtSignature: "SIG_TOOL" },
            ],
          },
        },
      },
      { role: "tool", toolCallId: "call_1", content: "Sunny, 25C" },
    ], { model: "gemini-2.5-flash" });

    const assistantParts = body.contents[1].parts;
    expect(assistantParts[0]).toMatchObject({ text: "Thinking about it.", thought: true, thoughtSignature: "SIG_REASONING" });
    const functionCallPart = assistantParts.find((part: any) => part.functionCall);
    expect(functionCallPart).toMatchObject({
      functionCall: { name: "get_weather", args: { city: "Beijing" } },
      thoughtSignature: "SIG_TOOL",
    });

    const toolParts = body.contents[2].parts;
    expect(toolParts[0].functionResponse).toMatchObject({ name: "get_weather" });
  });

  it("maps thinking levels to thinking_level on Gemini 3 and budgets on 2.5", async () => {
    const gemini3 = await captureBody(USER_MESSAGES, { model: "gemini-3-pro-preview", thinkingLevel: "max" });
    expect(gemini3.generationConfig.thinkingConfig).toEqual({ thinkingLevel: "high", includeThoughts: true });

    const ultra = await captureBody(USER_MESSAGES, { model: "gemini-3-pro-preview", thinkingLevel: "ultra" });
    expect(ultra.generationConfig.thinkingConfig).toEqual({ thinkingLevel: "high", includeThoughts: true });

    const gemini25 = await captureBody(USER_MESSAGES, { model: "gemini-2.5-flash", thinkingLevel: "medium" });
    expect(gemini25.generationConfig.thinkingConfig).toEqual({ thinkingBudget: 8192, includeThoughts: true });

    const off = await captureBody(USER_MESSAGES, { model: "gemini-2.5-flash", thinkingLevel: "off" });
    expect(off.generationConfig.thinkingConfig).toEqual({ thinkingBudget: 0 });
  });

  it("sends thinking_level to Gemini 4 and never a budget or MINIMAL", async () => {
    for (const level of ["low", "medium", "high"] as const) {
      const body = await captureBody(USER_MESSAGES, { model: "gemini-4-argon", thinkingLevel: level });
      expect(body.generationConfig.thinkingConfig).toEqual({ thinkingLevel: level, includeThoughts: true });
    }
    const max = await captureBody(USER_MESSAGES, { model: "gemini-4-argon", thinkingLevel: "max" });
    expect(max.generationConfig.thinkingConfig).toEqual({ thinkingLevel: "high", includeThoughts: true });
    // Side calls (titles, compaction) request "off"; Gemini 4 has no off switch.
    for (const level of ["off", "minimal"] as const) {
      const body = await captureBody(USER_MESSAGES, { model: "gemini-4-argon", thinkingLevel: level });
      expect(body.generationConfig.thinkingConfig).toEqual({ thinkingLevel: "low", includeThoughts: true });
    }
  });

  it.each(["gemini-4-hydrogen", "gemini-4-neon"])("sends %s its native xhigh level", async (model) => {
    for (const level of ["low", "medium", "high"] as const) {
      const body = await captureBody(USER_MESSAGES, { model, thinkingLevel: level });
      expect(body.generationConfig.thinkingConfig).toEqual({ thinkingLevel: level, includeThoughts: true });
    }
    for (const level of ["off", "minimal"] as const) {
      const body = await captureBody(USER_MESSAGES, { model, thinkingLevel: level });
      expect(body.generationConfig.thinkingConfig).toEqual({ thinkingLevel: "low", includeThoughts: true });
    }
    // The SDK schema stops at "high"; xhigh is restored on the wire and the
    // internal carrier header never reaches Google.
    for (const level of ["xhigh", "max"] as const) {
      let body: Record<string, any> = {};
      let headers = new Headers();
      const provider = makeProvider(async (_url, init) => {
        body = JSON.parse(String(init?.body ?? "{}"));
        headers = new Headers(init?.headers);
        return sseResponse(sse([HAPPY_CHUNKS[1], {
          candidates: [{ content: { parts: [] as unknown[], role: "model" }, finishReason: "STOP" }],
        }]));
      });
      await collect(provider.streamChat(USER_MESSAGES, { model, thinkingLevel: level }));
      expect(body.generationConfig.thinkingConfig).toEqual({ thinkingLevel: "xhigh", includeThoughts: true });
      expect(body.contents).toHaveLength(1);
      expect(headers.has("x-bubble-thinking-level")).toBe(false);
      expect(headers.get("x-goog-api-key")).toBe("test-key");
      expect(headers.get("content-type")).toBe("application/json");
    }
    // Argon and Carbon have no xhigh: they must keep clamping to high.
    for (const model of ["gemini-4-argon", "gemini-4-carbon"]) {
      const body = await captureBody(USER_MESSAGES, { model, thinkingLevel: "xhigh" });
      expect(body.generationConfig.thinkingConfig).toEqual({ thinkingLevel: "high", includeThoughts: true });
    }
  });
});

describe("ai-sdk provider complete()", () => {
  it("returns joined text from doGenerate", async () => {
    const provider = makeProvider(async () => new Response(JSON.stringify({
      candidates: [{
        content: { parts: [{ text: "Hello " }, { text: "world" }], role: "model" },
        finishReason: "STOP",
      }],
      usageMetadata: { promptTokenCount: 5, candidatesTokenCount: 2, totalTokenCount: 7 },
    }), { status: 200, headers: { "content-type": "application/json" } }));

    await expect(provider.complete([{ role: "user", content: "hi" }], { model: "gemini-2.5-flash" }))
      .resolves.toBe("Hello world");
  });

  it("does not send thinkingBudget 0 to Gemini 4 for default side calls", async () => {
    let body: Record<string, any> = {};
    const provider = makeProvider(async (_url, init) => {
      body = JSON.parse(String(init?.body ?? "{}"));
      return new Response(JSON.stringify({
        candidates: [{ content: { parts: [{ text: "Title" }], role: "model" }, finishReason: "STOP" }],
      }), { status: 200, headers: { "content-type": "application/json" } });
    });

    for (const model of ["gemini-4-argon", "gemini-4-neon"]) {
      await expect(provider.complete([{ role: "user", content: "hi" }], { model }))
        .resolves.toBe("Title");
      expect(body.generationConfig.thinkingConfig).toEqual({ thinkingLevel: "low", includeThoughts: true });
    }
  });

  it("sends Gemini 4 Neon its native xhigh on a non-streaming call too", async () => {
    let body: Record<string, any> = {};
    let headers = new Headers();
    const provider = makeProvider(async (_url, init) => {
      body = JSON.parse(String(init?.body ?? "{}"));
      headers = new Headers(init?.headers);
      return new Response(JSON.stringify({
        candidates: [{ content: { parts: [{ text: "Done" }], role: "model" }, finishReason: "STOP" }],
      }), { status: 200, headers: { "content-type": "application/json" } });
    });

    await expect(provider.complete([{ role: "user", content: "hi" }], { model: "gemini-4-neon", thinkingLevel: "xhigh" }))
      .resolves.toBe("Done");
    expect(body.generationConfig.thinkingConfig).toEqual({ thinkingLevel: "xhigh", includeThoughts: true });
    expect(headers.has("x-bubble-thinking-level")).toBe(false);
  });
});

describe("Gemini model discovery", () => {
  const entry = (name: string, extra: Record<string, unknown> = {}) => ({
    name: `models/${name}`,
    displayName: name,
    inputTokenLimit: 1048576,
    supportedGenerationMethods: ["generateContent"],
    ...extra,
  });

  it("keeps the newest five text-model families, GA over preview", () => {
    const models = selectLatestGeminiModels([
      entry("gemini-2.5-flash"),
      entry("gemini-2.5-flash-preview-09-2025"),
      entry("gemini-2.5-flash-lite"),
      entry("gemini-2.5-pro"),
      entry("gemini-3-pro-preview"),
      entry("gemini-3-flash-preview"),
      entry("gemini-3.1-pro-preview"),
      entry("gemini-3.5-flash"),
      entry("gemini-2.0-flash-001"),
      entry("gemini-2.5-flash-image"),
      entry("gemini-2.5-flash-preview-tts"),
      entry("gemini-embedding-001", { supportedGenerationMethods: ["embedContent"] }),
      entry("gemini-2.5-flash-native-audio-latest"),
    ]);

    expect(models.map((m) => m.id)).toEqual([
      "gemini-3.5-flash",
      "gemini-3.1-pro-preview",
      "gemini-3-pro-preview",
      "gemini-3-flash-preview",
      "gemini-2.5-pro",
    ]);
    expect(models[0].contextWindow).toBe(1048576);
    expect(models[1].defaultReasoningLevel).toBe("high");
  });

  it("keeps named-tier Gemini 4 Argon and ranks it first with Google's medium default", () => {
    const models = selectLatestGeminiModels([
      entry("gemini-3.8-flash"),
      entry("gemini-3.6-flash"),
      entry("gemini-3.5-flash"),
      entry("gemini-3.5-flash-lite"),
      entry("gemini-3.1-pro-preview"),
      entry("gemini-4-argon", { displayName: "Gemini 4 Argon", outputTokenLimit: 32768 }),
    ]);

    expect(models.map((m) => m.id)).toEqual([
      "gemini-4-argon",
      "gemini-3.8-flash",
      "gemini-3.6-flash",
      "gemini-3.5-flash",
      "gemini-3.5-flash-lite",
    ]);
    expect(models[0]).toEqual({
      id: "gemini-4-argon",
      name: "Gemini 4 Argon",
      contextWindow: 1048576,
      reasoningLevels: ["low", "medium", "high"],
    });
  });

  it("keeps Gemini 4 Carbon and Hydrogen and ranks them after Argon, ahead of Flash", () => {
    const models = selectLatestGeminiModels([
      entry("gemini-3.8-flash"),
      entry("gemini-3.7-flash"),
      entry("gemini-3.6-flash"),
      entry("gemini-3.5-flash"),
      entry("gemini-4-hydrogen", { displayName: "Gemini 4 Hydrogen", outputTokenLimit: 65536 }),
      entry("gemini-4-carbon", { displayName: "Gemini 4 Carbon", outputTokenLimit: 32768 }),
      entry("gemini-4-argon", { displayName: "Gemini 4 Argon", outputTokenLimit: 32768 }),
    ]);

    expect(models.map((m) => m.id)).toEqual([
      "gemini-4-argon",
      "gemini-4-carbon",
      "gemini-4-hydrogen",
      "gemini-3.8-flash",
      "gemini-3.7-flash",
    ]);
    expect(models[1]).toEqual({
      id: "gemini-4-carbon",
      name: "Gemini 4 Carbon",
      contextWindow: 1048576,
      reasoningLevels: ["low", "medium", "high"],
    });
    expect(models[2]).toEqual({
      id: "gemini-4-hydrogen",
      name: "Gemini 4 Hydrogen",
      contextWindow: 1048576,
      reasoningLevels: ["low", "medium", "high", "xhigh"],
    });
  });

  it("keeps Gemini 4 Neon between Carbon and Hydrogen, leaving one Flash in the top five", () => {
    const models = selectLatestGeminiModels([
      entry("gemini-3.8-flash"),
      entry("gemini-3.7-flash"),
      entry("gemini-4-hydrogen", { displayName: "Gemini 4 Hydrogen", outputTokenLimit: 65536 }),
      entry("gemini-4-neon", { displayName: "Gemini 4 Neon", outputTokenLimit: 65536 }),
      entry("gemini-4-carbon", { displayName: "Gemini 4 Carbon", outputTokenLimit: 32768 }),
      entry("gemini-4-argon", { displayName: "Gemini 4 Argon", outputTokenLimit: 32768 }),
    ]);

    expect(models.map((m) => m.id)).toEqual([
      "gemini-4-argon",
      "gemini-4-carbon",
      "gemini-4-neon",
      "gemini-4-hydrogen",
      "gemini-3.8-flash",
    ]);
    // No defaultReasoningLevel here: the registry takes Neon's high from the catalog.
    expect(models[2]).toEqual({
      id: "gemini-4-neon",
      name: "Gemini 4 Neon",
      contextWindow: 1048576,
      reasoningLevels: ["low", "medium", "high", "xhigh"],
    });
  });

  it("matches a tier by its whole name and keeps the stable Neon over its previews", () => {
    const ids = (names: string[]) => selectLatestGeminiModels(names.map((name) => entry(name))).map((m) => m.id);

    // A longer word that merely starts with a tier name is some other model.
    // Asked alone: next to the real Neon it would lose the family anyway.
    expect(ids(["gemini-4-neonextra"])).toEqual([]);
    expect(ids(["gemini-4-hydrogenx", "gemini-4-flashy", "gemini-4-neon-litex"])).toEqual(["gemini-4-neon-litex"]);
    expect(ids(["gemini-4-neonextra", "gemini-4-hydrogenx", "gemini-4-neon"])).toEqual(["gemini-4-neon"]);
    expect(ids(["gemini-4-neon-preview-11-2026", "gemini-4-neon"])).toEqual(["gemini-4-neon"]);
    // With no stable id listed, the preview stands in for the family.
    expect(ids(["gemini-4-neon-preview-11-2026"])).toEqual(["gemini-4-neon-preview-11-2026"]);
    // Suffixed ids of the older families still match.
    expect(ids(["gemini-3.5-flash-lite", "gemini-3.1-pro-preview", "gemini-2.5-flash-preview-09-2025"])).toEqual([
      "gemini-3.5-flash-lite",
      "gemini-3.1-pro-preview",
      "gemini-2.5-flash-preview-09-2025",
    ]);
  });

  it("lists no Gemini id without a version", () => {
    // Moving aliases and names from before the numbered releases.
    const unversioned = ["gemini-flash-latest", "gemini-flash-lite-latest", "gemini-pro-latest", "gemini-pro", "gemini-exp-1206"];
    expect(selectLatestGeminiModels(unversioned.map((name) => entry(name)))).toEqual([]);
    expect(selectLatestGeminiModels([...unversioned, "gemini-4-neon"].map((name) => entry(name))).map((m) => m.id)).toEqual(["gemini-4-neon"]);
    // Their ids say nothing about thinking levels either; a numbered release
    // from before thinking control is known to have none.
    for (const id of unversioned) expect(geminiReasoningLevels(id)).toBeUndefined();
    expect(geminiReasoningLevels("gemini-2.0-flash")).toEqual(["off"]);
  });

  it("prefers GA ids over dated previews within a family", () => {
    const models = selectLatestGeminiModels([
      entry("gemini-2.5-flash-preview-09-2025"),
      entry("gemini-2.5-flash"),
    ]);
    expect(models.map((m) => m.id)).toEqual(["gemini-2.5-flash"]);
  });

  it("derives reasoning levels by family", () => {
    expect(geminiReasoningLevels("gemini-4-argon")).toEqual(["low", "medium", "high"]);
    expect(geminiReasoningLevels("gemini-4-carbon")).toEqual(["low", "medium", "high"]);
    expect(geminiReasoningLevels("gemini-4-hydrogen")).toEqual(["low", "medium", "high", "xhigh"]);
    expect(geminiReasoningLevels("gemini-4-neon")).toEqual(["low", "medium", "high", "xhigh"]);
    // A dated or preview id of a verified release keeps its ladder, as does
    // any suffix that is not the -lite tier ("-litex" is just a suffix).
    expect(geminiReasoningLevels("gemini-4-neon-preview-11-2026")).toEqual(["low", "medium", "high", "xhigh"]);
    expect(geminiReasoningLevels("gemini-4-neon-litex")).toEqual(["low", "medium", "high", "xhigh"]);
    expect(geminiReasoningLevels("gemini-4-hydrogen-preview-11-2026")).toEqual(["low", "medium", "high", "xhigh"]);
    // xhigh is not guessed for releases nobody has probed: there it clamps to high.
    for (const unverified of ["gemini-4-neon-lite", "gemini-4-neon-lite-preview-12-2026", "gemini-4-hydrogen-lite", "gemini-4.1-neon", "gemini-5-neon", "gemini-4-neonextra"]) {
      expect(geminiReasoningLevels(unverified)).toEqual(["low", "medium", "high"]);
    }
    expect(geminiReasoningLevels("gemini-3.8-flash")).toEqual(["off", "low", "medium", "high"]);
    expect(geminiReasoningLevels("gemini-3.7-flash")).toEqual(["off", "low", "medium", "high"]);
    expect(geminiReasoningLevels("gemini-3.5-flash")).toEqual(["minimal", "low", "medium", "high"]);
    expect(geminiReasoningLevels("gemini-3.1-pro-preview")).toEqual(["low", "medium", "high"]);
    expect(geminiReasoningLevels("gemini-2.5-pro")).toEqual(["low", "medium", "high"]);
    expect(geminiReasoningLevels("gemini-2.5-flash")).toEqual(["off", "low", "medium", "high"]);
    expect(geminiReasoningLevels("gemini-2.0-flash")).toEqual(["off"]);
  });

  it("fetches with the API key header and surfaces HTTP failures", async () => {
    let requested = "";
    let header = "";
    const models = await fetchGeminiModels({
      apiKey: "test-key",
      fetch: async (url, init) => {
        requested = String(url);
        header = String((init?.headers as Record<string, string>)?.["x-goog-api-key"]);
        return new Response(JSON.stringify({ models: [entry("gemini-3.5-flash")] }), {
          status: 200,
          headers: { "content-type": "application/json" },
        });
      },
    });
    expect(requested).toBe("https://generativelanguage.googleapis.com/v1beta/models?pageSize=1000");
    expect(header).toBe("test-key");
    expect(models.map((m) => m.id)).toEqual(["gemini-3.5-flash"]);

    await expect(fetchGeminiModels({
      apiKey: "k",
      fetch: async () => new Response("nope", { status: 403 }),
    })).rejects.toThrow(/403/);
  });
});

describe("ai-sdk provider registration", () => {
  it("rejects unknown provider ids with a clear error", () => {
    expect(() => createAiSdkProvider({ providerId: "not-a-thing", apiKey: "k" }))
      .toThrow(/no AI SDK backend is registered/);
  });
});
