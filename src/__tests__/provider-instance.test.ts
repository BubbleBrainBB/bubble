import { beforeEach, describe, expect, it, vi } from "vitest";
import type { StreamChunk, ToolDefinition } from "../types.js";

const { createMock } = vi.hoisted(() => ({
  createMock: vi.fn(),
}));

vi.mock("openai", () => ({
  default: vi.fn().mockImplementation(function MockOpenAI() {
    return {
      chat: {
        completions: {
          create: createMock,
        },
      },
    };
  }),
}));

async function* fromArray<T>(items: T[]): AsyncIterable<T> {
  for (const item of items) yield item;
}

async function collect(stream: AsyncIterable<StreamChunk>): Promise<StreamChunk[]> {
  const out: StreamChunk[] = [];
  for await (const chunk of stream) out.push(chunk);
  return out;
}

function makeSseResponse(events: Array<Record<string, unknown>>): Response {
  const encoder = new TextEncoder();
  const body = new ReadableStream<Uint8Array>({
    start(controller) {
      for (const event of events) {
        controller.enqueue(encoder.encode(`data: ${JSON.stringify(event)}\n\n`));
      }
      controller.close();
    },
  });
  return new Response(body, { status: 200 });
}

describe("createProviderInstance", () => {
  beforeEach(() => {
    createMock.mockReset();
    vi.unstubAllGlobals();
    vi.unstubAllEnvs();
  });

  it("requests stream usage for DeepSeek so cost can be calculated", async () => {
    let body: any;
    createMock.mockImplementation(async (input) => {
      body = input;
      return fromArray([
        {
          usage: {
            prompt_tokens: 100,
            prompt_cache_hit_tokens: 40,
            prompt_cache_miss_tokens: 60,
            completion_tokens: 20,
          },
          choices: [{ delta: {} }],
        },
      ]);
    });

    const { createProviderInstance } = await import("../provider.js");
    const provider = createProviderInstance({
      providerId: "deepseek",
      apiKey: "sk-test",
      baseURL: "https://api.deepseek.com",
    });

    const chunks = await collect(provider.streamChat([{ role: "user", content: "hi" }], {
      model: "deepseek-v4-pro",
    }));

    expect(body.stream_options).toEqual({ include_usage: true });
    expect(chunks.some((chunk) => chunk.type === "usage")).toBe(true);
  });

  it("sends DeepSeek Vision image blocks with V4 thinking fields", async () => {
    let body: any;
    createMock.mockImplementation(async (input) => {
      body = input;
      return fromArray([{ choices: [{ delta: { content: "cat" }, finish_reason: "stop" }] }]);
    });

    const { createProviderInstance } = await import("../provider.js");
    const provider = createProviderInstance({
      providerId: "deepseek",
      apiKey: "sk-test",
      baseURL: "https://api.deepseek.com",
    });

    await collect(provider.streamChat([{
      role: "user",
      content: [
        { type: "text", text: "What is in this image?" },
        { type: "image_url", image_url: { url: "data:image/png;base64,aGVsbG8=" } },
      ],
    }], {
      model: "deepseek-v4-flash-vision-exp",
      thinkingLevel: "high",
    }));

    expect(body.model).toBe("deepseek-v4-flash-vision-exp");
    expect(body.messages[0].content).toEqual([
      { type: "text", text: "What is in this image?" },
      { type: "image_url", image_url: { url: "data:image/png;base64,aGVsbG8=" } },
    ]);
    expect(body.thinking).toEqual({ type: "enabled" });
    expect(body.reasoning_effort).toBe("high");
    expect(body.stream_options).toEqual({ include_usage: true });
  });

  it.each([
    ["off", { type: "disabled" }, undefined],
    ["low", { type: "enabled" }, "low"],
  ] as const)("serializes DeepSeek Vision %s thinking mode", async (thinkingLevel, thinking, effort) => {
    let body: any;
    createMock.mockImplementation(async (input) => {
      body = input;
      return fromArray([{ choices: [{ delta: { content: "ok" }, finish_reason: "stop" }] }]);
    });

    const { createProviderInstance } = await import("../provider.js");
    const provider = createProviderInstance({
      providerId: "deepseek",
      apiKey: "sk-test",
      baseURL: "https://api.deepseek.com",
    });

    await collect(provider.streamChat([{ role: "user", content: "hi" }], {
      model: "deepseek-v4-flash-vision-exp",
      thinkingLevel,
    }));

    expect(body.thinking).toEqual(thinking);
    expect(body.reasoning_effort).toBe(effort);
  });

  it.each([
    ["openai", "https://api.openai.com/v1", "gpt-4o"],
    ["moonshot-cn", "https://api.moonshot.cn/v1", "kimi-k2.7-code"],
    ["moonshot-intl", "https://api.moonshot.ai/v1", "kimi-k2.7-code"],
    ["zhipuai", "https://open.bigmodel.cn/api/paas/v4", "glm-5.2"],
    ["zai-coding-plan", "https://api.z.ai/api/coding/paas/v4", "glm-5.2"],
  ])("requests stream usage for %s-compatible streaming", async (providerId, baseURL, model) => {
    let body: any;
    createMock.mockImplementation(async (input) => {
      body = input;
      return fromArray([
        {
          usage: {
            prompt_tokens: 12,
            completion_tokens: 3,
          },
          choices: [{ delta: {} }],
        },
      ]);
    });

    const { createProviderInstance } = await import("../provider.js");
    const provider = createProviderInstance({
      providerId,
      apiKey: "sk-test",
      baseURL,
    });

    const chunks = await collect(provider.streamChat([{ role: "user", content: "hi" }], {
      model,
    }));

    expect(body.stream_options).toEqual({ include_usage: true });
    expect(chunks).toContainEqual({
      type: "usage",
      usage: {
        promptTokens: 12,
        completionTokens: 3,
        outputTokensReported: true,
        promptCacheHitTokens: undefined,
        promptCacheMissTokens: undefined,
        reasoningTokens: undefined,
        totalTokens: undefined,
      },
    });
  });

  it("preserves OpenAI-compatible tools while disabling tool calls", async () => {
    let body: any;
    createMock.mockImplementation(async (input) => {
      body = input;
      return fromArray([{ choices: [{ delta: { content: "ok" }, finish_reason: "stop" }] }]);
    });

    const readTool: ToolDefinition = {
      name: "read",
      description: "Read a file",
      parameters: {
        type: "object",
        properties: { path: { type: "string" } },
        required: ["path"],
      },
    };
    const { createProviderInstance } = await import("../provider.js");
    const provider = createProviderInstance({
      providerId: "openai",
      apiKey: "sk-test",
      baseURL: "https://api.openai.com/v1",
    });

    await collect(provider.streamChat([{ role: "user", content: "hi" }], {
      model: "gpt-4o",
      tools: [readTool],
      toolChoice: "none",
    }));

    expect(body.tools?.map((tool: any) => tool.function.name)).toEqual(["read"]);
    expect(body.tool_choice).toBe("none");
  });

  describe("GLM tool-call streaming", () => {
    const writeTool: ToolDefinition = {
      name: "write",
      description: "Write a file",
      parameters: {
        type: "object",
        properties: { path: { type: "string" }, content: { type: "string" } },
        required: ["path", "content"],
      },
    };

    async function requestBody(
      provider: { providerId: string; baseURL: string },
      model: string,
      tools?: ToolDefinition[],
    ): Promise<any> {
      let body: any;
      createMock.mockImplementation(async (input) => {
        body = input;
        return fromArray([{ choices: [{ delta: { content: "ok" }, finish_reason: "stop" }] }]);
      });
      const { createProviderInstance } = await import("../provider.js");
      const instance = createProviderInstance({ ...provider, apiKey: "sk-test" });
      await collect(instance.streamChat([{ role: "user", content: "hi" }], { model, tools }));
      return body;
    }

    // GLM sends a tool call whole unless asked: without the flag a file being
    // written shows nothing until it is finished.
    it.each([
      ["zhipuai-coding-plan", "https://open.bigmodel.cn/api/coding/paas/v4", "glm-5.3"],
      ["zhipuai-coding-plan", "https://open.bigmodel.cn/api/coding/paas/v4", "glm-4.7"],
      ["zhipuai", "https://open.bigmodel.cn/api/paas/v4", "glm-5.2"],
      ["zai", "https://api.z.ai/api/paas/v4", "glm-5.1"],
      ["zai-coding-plan", "https://api.z.ai/api/coding/paas/v4", "glm-5-turbo"],
    ])("asks %s (%s) %s to stream tool calls", async (providerId, baseURL, model) => {
      const body = await requestBody({ providerId, baseURL }, model, [writeTool]);

      expect(body.tool_stream).toBe(true);
      expect(body.stream).toBe(true);
    });

    it("leaves the flag out of a request that carries no tools", async () => {
      const body = await requestBody(
        { providerId: "zhipuai-coding-plan", baseURL: "https://open.bigmodel.cn/api/coding/paas/v4" },
        "glm-5.3",
      );

      expect(body).not.toHaveProperty("tool_stream");
    });

    it("does not send the flag to other providers", async () => {
      const body = await requestBody(
        { providerId: "deepseek", baseURL: "https://api.deepseek.com" },
        "deepseek-v4-pro",
        [writeTool],
      );

      expect(body).not.toHaveProperty("tool_stream");
    });
  });

  it("uses Fireworks Kimi request defaults", async () => {
    let body: any;
    createMock.mockImplementation(async (input) => {
      body = input;
      return fromArray([{ choices: [{ delta: {}, finish_reason: "stop" }] }]);
    });

    const { createProviderInstance } = await import("../provider.js");
    const provider = createProviderInstance({
      providerId: "fireworks",
      apiKey: "sk-test",
      baseURL: "https://api.fireworks.ai/inference/v1",
    });

    await collect(provider.streamChat([{
      role: "assistant",
      content: "",
      reasoning: "tool reasoning",
      toolCalls: [{ id: "read:1", name: "read", arguments: "{\"path\":\"a\"}" }],
    }], {
      model: "accounts/fireworks/models/kimi-k2p6",
    }));

    expect(body.max_tokens).toBe(32768);
    expect(body.parallel_tool_calls).toBeUndefined();
    expect(body.messages[0].reasoning_content).toBeUndefined();
  });

  it("uses StepFun Step Plan reasoning effort request shape", async () => {
    let body: any;
    createMock.mockImplementation(async (input) => {
      body = input;
      return fromArray([{ choices: [{ delta: {}, finish_reason: "stop" }] }]);
    });

    const { createProviderInstance } = await import("../provider.js");
    const provider = createProviderInstance({
      providerId: "stepfun",
      apiKey: "sk-test",
      baseURL: "https://api.stepfun.com/step_plan/v1",
    });

    await collect(provider.streamChat([{
      role: "assistant",
      content: "",
      reasoning: "tool reasoning",
      toolCalls: [{ id: "read:1", name: "read", arguments: "{\"path\":\"a\"}" }],
    }], {
      model: "step-3.7-flash",
      thinkingLevel: "high",
    }));

    expect(body.reasoning_effort).toBe("high");
    expect(body.reasoning).toBeUndefined();
    expect(body.messages[0].reasoning_content).toBeUndefined();
  });

  it("routes StepFun API tools through chat completions without plan reasoning fields", async () => {
    const { getBuiltinProvider, getBuiltinModel } = await import("../model-catalog.js");
    const builtin = getBuiltinProvider("stepfun-api")!;
    expect(builtin.baseURL).toBe("https://api.stepfun.com/v1");
    expect(builtin.protocol).toBe("openai-chat");
    expect(getBuiltinProvider("stepfun")?.baseURL).toContain("/step_plan/");
    expect(getBuiltinModel("stepfun-api", "water18-0910")?.reasoningLevels).toEqual([]);
    createMock.mockResolvedValue(fromArray([{ choices: [{ delta: {}, finish_reason: "stop" }] }]));
    const { createProviderInstance } = await import("../provider.js");
    const provider = createProviderInstance({ ...builtin, providerId: builtin.id, apiKey: "sk-test" });
    await collect(provider.streamChat([
      { role: "assistant", content: "", reasoning: "previous thinking", toolCalls: [{ id: "call-1", name: "add", arguments: "{}" }] },
      { role: "tool", toolCallId: "call-1", content: "42" },
    ], {
      model: "water18-0910",
      thinkingLevel: "high",
      tools: [{ name: "add", description: "Add numbers", parameters: { type: "object", properties: {} } }],
    }));
    const body = createMock.mock.calls[0][0];
    expect(body.model).toBe("water18-0910");
    expect(body.stream).toBe(true);
    expect(body.stream_options).toEqual({ include_usage: true });
    expect(body.tools[0].function.name).toBe("add");
    expect(body.messages[1]).toEqual({ role: "tool", tool_call_id: "call-1", content: "42" });
    expect(body.messages[0].reasoning_content).toBeUndefined();
    expect(body.reasoning_effort).toBeUndefined();
    expect(body.reasoning).toBeUndefined();
  });

  it("sends the selected OpenRouter reasoning effort", async () => {
    let body: any;
    createMock.mockImplementation(async (input) => {
      body = input;
      return fromArray([{ choices: [{ delta: {}, finish_reason: "stop" }] }]);
    });

    const { createProviderInstance } = await import("../provider.js");
    const provider = createProviderInstance({
      providerId: "openrouter",
      apiKey: "sk-or-test",
      baseURL: "https://openrouter.ai/api/v1",
    });

    await collect(provider.streamChat([{ role: "user", content: "hi" }], {
      model: "stealth/ox-alpha",
      thinkingLevel: "high",
    }));

    expect(body.model).toBe("stealth/ox-alpha");
    expect(body.reasoning).toEqual({ effort: "high" });
    expect(body.reasoning_effort).toBeUndefined();
  });

  it("surfaces OpenRouter terminal SSE errors as retryable stream failures", async () => {
    createMock.mockImplementation(async () => fromArray([{
      provider: "Ox Alpha upstream",
      error: {
        code: 504,
        message: "Provider timed out during generation",
        metadata: { error_type: "provider_timeout" },
      },
      choices: [{ delta: { content: "" }, finish_reason: "error" }],
    }]));

    const { createProviderInstance } = await import("../provider.js");
    const provider = createProviderInstance({
      providerId: "openrouter",
      apiKey: "sk-or-test",
      baseURL: "https://openrouter.ai/api/v1",
    });

    await expect(collect(provider.streamChat([{ role: "user", content: "hi" }], {
      model: "stealth/ox-alpha",
      thinkingLevel: "high",
    }))).rejects.toMatchObject({
      name: "ProviderStreamInterruptedError",
      maxRetries: 1,
      message: expect.stringContaining("Provider timed out during generation"),
    });
  });

  it("defers OpenRouter streamed rate limits to the subagent scheduler", async () => {
    createMock.mockImplementation(async () => fromArray([{
      error: {
        code: 429,
        message: "Rate limit exceeded",
        metadata: {
          error_type: "rate_limit_exceeded",
          availability: { retry_after: 3 },
        },
      },
      choices: [{ delta: { content: "" }, finish_reason: "error" }],
    }]));

    const { createProviderInstance } = await import("../provider.js");
    const provider = createProviderInstance({
      providerId: "openrouter",
      apiKey: "sk-or-test",
      baseURL: "https://openrouter.ai/api/v1",
    });

    await expect(collect(provider.streamChat([{ role: "user", content: "hi" }], {
      model: "stealth/ox-alpha",
      rateLimitPolicy: "defer",
    }))).rejects.toMatchObject({
      name: "RateLimitError",
      status: 429,
      retryAfterMs: 3_000,
      message: expect.stringContaining("Rate limit exceeded"),
    });
  });

  it("rejects non-Ox OpenRouter models before any provider request", async () => {
    createMock.mockImplementation(async () =>
      fromArray([{ choices: [{ delta: {}, finish_reason: "stop" }] }]));

    const { createProviderInstance } = await import("../provider.js");
    const provider = createProviderInstance({
      providerId: "openrouter",
      apiKey: "sk-or-test",
      baseURL: "https://openrouter.ai/api/v1",
    });

    await expect(collect(provider.streamChat([{ role: "user", content: "hi" }], {
      model: "openai/gpt-5.6",
    }))).rejects.toThrow(/openrouter.*fixed.*stealth\/ox-alpha/i);
    await expect(provider.complete([{ role: "user", content: "hi" }], {
      model: "anthropic/claude-sonnet-4.6",
    })).rejects.toThrow(/openrouter.*fixed.*stealth\/ox-alpha/i);
    expect(createMock).not.toHaveBeenCalled();
  });

  it("uses Volcengine Ark Responses API for Doubao", async () => {
    const requestInits: RequestInit[] = [];
    const fetchMock = vi.fn(async (_input: RequestInfo | URL, init?: RequestInit) => {
      requestInits.push(init ?? {});
      return makeSseResponse([
        {
          type: "response.completed",
          response: {
            usage: {
              input_tokens: 10,
              output_tokens: 5,
              output_tokens_details: { reasoning_tokens: 2 },
              total_tokens: 15,
            },
          },
        },
      ]);
    });
    vi.stubGlobal("fetch", fetchMock);

    const { createProviderInstance } = await import("../provider.js");
    const provider = createProviderInstance({
      providerId: "doubao",
      apiKey: "sk-test",
      baseURL: "https://ark.cn-beijing.volces.com/api/v3",
    });

    await collect(provider.streamChat([{
      role: "user",
      content: [
        {
          type: "image_url",
          image_url: { url: "https://arkdoc.tos-cn-beijing.volces.com/images/get-started/project-demo.png" },
        },
        {
          type: "text",
          text: "请根据图片实现一个短视频网站首页项目。",
        },
      ],
    }], {
      model: "doubao-seed-2-1-pro-260628",
      thinkingLevel: "high",
    }));

    const body = JSON.parse(String(requestInits[0].body));
    expect(fetchMock).toHaveBeenCalledWith("https://ark.cn-beijing.volces.com/api/v3/responses", expect.any(Object));
    expect(createMock).not.toHaveBeenCalled();
    expect(body.model).toBe("doubao-seed-2-1-pro-260628");
    expect(body.store).toBe(false);
    expect(body.stream).toBe(true);
    expect(body.thinking).toEqual({ type: "enabled" });
    expect(body.reasoning_effort).toBeUndefined();
    expect(body.input[0].content).toEqual([
      {
        type: "input_image",
        image_url: "https://arkdoc.tos-cn-beijing.volces.com/images/get-started/project-demo.png",
      },
      {
        type: "input_text",
        text: "请根据图片实现一个短视频网站首页项目。",
      },
    ]);
  });

  it("maps Doubao minimal reasoning to disabled Ark thinking", async () => {
    const requestInits: RequestInit[] = [];
    const fetchMock = vi.fn(async (_input: RequestInfo | URL, init?: RequestInit) => {
      requestInits.push(init ?? {});
      return makeSseResponse([{ type: "response.completed", response: {} }]);
    });
    vi.stubGlobal("fetch", fetchMock);

    const { createProviderInstance } = await import("../provider.js");
    const provider = createProviderInstance({
      providerId: "doubao",
      apiKey: "sk-test",
      baseURL: "https://ark.cn-beijing.volces.com/api/v3",
    });

    await collect(provider.streamChat([{ role: "user", content: "hi" }], {
      model: "doubao-seed-2-1-pro-260628",
      thinkingLevel: "minimal",
    }));

    const body = JSON.parse(String(requestInits[0].body));
    expect(body.thinking).toEqual({ type: "disabled" });
  });

  it.each([undefined, "openai-responses", "openai-chat"] as const)("routes Space Bunny via chat with profile protocol %s", async protocol => {
    createMock.mockResolvedValueOnce(fromArray([
      { choices: [{ delta: { reasoning_content: "Consider the tool" } }] },
      { choices: [{ delta: { tool_calls: [{ index: 0, id: "call_1", type: "function", function: { name: "probe", arguments: '{"value":7}' } }] } }] },
      { choices: [], usage: { prompt_tokens: 10, completion_tokens: 5, total_tokens: 15 } },
    ]));
    const { createProviderInstance } = await import("../provider.js");
    const provider = createProviderInstance({ providerId: "opencode-zen", apiKey: "fixture", baseURL: "https://opencode.ai/zen/v1", protocol });
    const chunks = await collect(provider.streamChat([{ role: "user", content: "Use probe" }], {
      model: "space-bunny-free", thinkingLevel: "max",
      tools: [{ name: "probe", description: "Probe", parameters: { type: "object", properties: { value: { type: "number" } } } }],
    }));
    expect(createMock.mock.calls[0][0]).toMatchObject({ model: "space-bunny-free", stream: true, reasoning_effort: "max", stream_options: { include_usage: true } });
    expect(createMock.mock.calls[0][0].input).toBeUndefined();
    expect(chunks).toContainEqual({ type: "reasoning_delta", content: "Consider the tool" });
    expect(chunks.some(chunk => chunk.type === "tool_call" && chunk.name === "probe")).toBe(true);
    expect(chunks.some(chunk => chunk.type === "usage" && chunk.usage.totalTokens === 15)).toBe(true);
    createMock.mockResolvedValueOnce({ choices: [{ message: { content: "OK" } }] });
    expect(await provider.complete([
      { role: "assistant", content: "", reasoning: "Consider the tool", toolCalls: [{ id: "call_1", name: "probe", arguments: '{"value":7}' }] },
      { role: "tool", toolCallId: "call_1", content: "7" },
    ], { model: "space-bunny-free", thinkingLevel: "low" })).toBe("OK");
    expect(createMock.mock.calls[1][0]).toMatchObject({ reasoning_effort: "low", messages: [
      { role: "assistant", reasoning_content: "Consider the tool" }, { role: "tool", tool_call_id: "call_1" },
    ] });
  });

  it("uses OpenCode Zen Responses API for Muse without Ark-only fields", async () => {
    vi.stubEnv("HTTPS_PROXY", "http://127.0.0.1:7890");
    vi.stubEnv("NO_PROXY", "");
    const requestInits: RequestInit[] = [];
    const fetchMock = vi.fn(async (_input: RequestInfo | URL, init?: RequestInit) => {
      requestInits.push(init ?? {});
      return makeSseResponse([
        {
          type: "response.output_item.done",
          output_index: 0,
          item: {
            id: "rs_1",
            type: "reasoning",
            summary: [],
            encrypted_content: "encrypted-reasoning",
          },
        },
        {
          type: "response.completed",
          response: {
            usage: {
              input_tokens: 12,
              input_tokens_details: { cached_tokens: 4 },
              output_tokens: 3,
              output_tokens_details: { reasoning_tokens: 2 },
              total_tokens: 15,
            },
          },
        },
      ]);
    });
    vi.stubGlobal("fetch", fetchMock);

    const { createProviderInstance } = await import("../provider.js");
    const provider = createProviderInstance({
      providerId: "opencode-zen",
      apiKey: "zen-test",
      baseURL: "https://opencode.ai/zen/v1",
    });

    const chunks = await collect(provider.streamChat([{ role: "user", content: "hi" }], {
      model: "muse-spark-1.3-contributor-free",
      thinkingLevel: "xhigh",
      temperature: 0.2,
    }));

    const body = JSON.parse(String(requestInits[0].body));
    expect(fetchMock).toHaveBeenCalledWith("https://opencode.ai/zen/v1/responses", expect.any(Object));
    expect((requestInits[0] as RequestInit & { dispatcher?: unknown }).dispatcher).toBeTruthy();
    expect(body).toMatchObject({
      model: "muse-spark-1.3-contributor-free",
      store: false,
      stream: true,
      include: ["reasoning.encrypted_content"],
      reasoning: { effort: "xhigh", summary: "auto" },
      temperature: 0.2,
    });
    expect(body.thinking).toBeUndefined();
    expect(chunks).toContainEqual({
      type: "provider_content_block",
      provider: "openai",
      block: {
        type: "reasoning",
        summary: [],
        encrypted_content: "encrypted-reasoning",
      },
    });
    expect(chunks).toContainEqual({
      type: "usage",
      usage: {
        promptTokens: 12,
        completionTokens: 3,
        promptCacheHitTokens: 4,
        promptCacheMissTokens: 8,
        reasoningTokens: 2,
        totalTokens: 15,
      },
    });
  });

  it("replays Muse Responses reasoning items before tool outputs", async () => {
    const requestInits: RequestInit[] = [];
    const fetchMock = vi.fn(async (_input: RequestInfo | URL, init?: RequestInit) => {
      requestInits.push(init ?? {});
      return makeSseResponse([{ type: "response.completed", response: {} }]);
    });
    vi.stubGlobal("fetch", fetchMock);

    const { createProviderInstance } = await import("../provider.js");
    const provider = createProviderInstance({
      providerId: "opencode-zen",
      apiKey: "zen-test",
      baseURL: "https://opencode.ai/zen/v1",
    });

    await collect(provider.streamChat([
      { role: "user", content: "Use the tool" },
      {
        role: "assistant",
        content: "",
        providerMetadata: {
          openai: {
            contentBlocks: [{
              type: "reasoning",
              summary: [],
              encrypted_content: "encrypted-reasoning",
            }],
          },
        },
        toolCalls: [{ id: "call_1", name: "read", arguments: '{"path":"a.ts"}' }],
      },
      { role: "tool", toolCallId: "call_1", content: "file contents" },
    ], {
      model: "muse-spark-1.2",
      thinkingLevel: "high",
    }));

    const body = JSON.parse(String(requestInits[0].body));
    expect(body.input.slice(1)).toEqual([
      { type: "reasoning", summary: [], encrypted_content: "encrypted-reasoning" },
      {
        type: "function_call",
        call_id: "call_1",
        name: "read",
        arguments: '{"path":"a.ts"}',
        status: "completed",
      },
      { type: "function_call_output", call_id: "call_1", output: "file contents" },
    ]);
  });

  it("streams tool calls through Doubao Ark Responses API", async () => {
    const writeTool: ToolDefinition = {
      name: "write",
      description: "Write a file",
      parameters: {
        type: "object",
        properties: {
          path: { type: "string" },
          content: { type: "string" },
        },
        required: ["path", "content"],
      },
    };
    const requestInits: RequestInit[] = [];
    const fetchMock = vi.fn(async (_input: RequestInfo | URL, init?: RequestInit) => {
      requestInits.push(init ?? {});
      return makeSseResponse([
        {
          type: "response.output_item.added",
          output_index: 0,
          item: {
            id: "fc_1",
            type: "function_call",
            call_id: "call_write",
            name: "write",
            arguments: "",
          },
        },
        {
          type: "response.function_call_arguments.delta",
          output_index: 0,
          delta: "{\"path\":\"index.html\",",
        },
        {
          type: "response.function_call_arguments.delta",
          output_index: 0,
          delta: "\"content\":\"<html></html>\"}",
        },
        {
          type: "response.function_call_arguments.done",
          output_index: 0,
          arguments: "{\"path\":\"index.html\",\"content\":\"<html></html>\"}",
        },
        {
          type: "response.output_item.done",
          output_index: 0,
          item: {
            id: "fc_1",
            type: "function_call",
            call_id: "call_write",
            name: "write",
            arguments: "{\"path\":\"index.html\",\"content\":\"<html></html>\"}",
          },
        },
        {
          type: "response.completed",
          response: {
            usage: {
              input_tokens: 10,
              output_tokens: 5,
              total_tokens: 15,
            },
          },
        },
      ]);
    });
    vi.stubGlobal("fetch", fetchMock);

    const { createProviderInstance } = await import("../provider.js");
    const provider = createProviderInstance({
      providerId: "doubao",
      apiKey: "sk-test",
      baseURL: "https://ark.cn-beijing.volces.com/api/v3",
    });

    const chunks = await collect(provider.streamChat([{ role: "user", content: "write a file" }], {
      model: "doubao-seed-2-1-pro-260628",
      thinkingLevel: "high",
      tools: [writeTool],
    }));

    const body = JSON.parse(String(requestInits[0].body));
    expect(body.stream).toBe(true);
    expect(body.tools?.map((tool: any) => tool.name)).toEqual(["write"]);
    expect(body.tools[0].parameters).toEqual(writeTool.parameters);
    expect(body.reasoning_effort).toBeUndefined();

    expect(chunks).toContainEqual({
      type: "usage",
      usage: {
        promptTokens: 10,
        completionTokens: 5,
        promptCacheHitTokens: undefined,
        promptCacheMissTokens: undefined,
        reasoningTokens: undefined,
        totalTokens: 15,
      },
    });
    expect(chunks.filter((chunk) => chunk.type === "tool_call")).toEqual([
      { type: "tool_call", id: "call_write", name: "write", arguments: "", isStart: true, isEnd: false },
      {
        type: "tool_call",
        id: "call_write",
        name: "write",
        arguments: "{\"path\":\"index.html\",",
        isStart: false,
        isEnd: false,
      },
      {
        type: "tool_call",
        id: "call_write",
        name: "write",
        arguments: "\"content\":\"<html></html>\"}",
        isStart: false,
        isEnd: false,
      },
      {
        type: "tool_call",
        id: "call_write",
        name: "write",
        arguments: "",
        argumentsFull: "{\"path\":\"index.html\",\"content\":\"<html></html>\"}",
        argumentsCorrupt: undefined,
        isStart: false,
        isEnd: true,
      },
    ]);
    expect(chunks.at(-1)).toEqual({ type: "done" });
  });

  it("marks malformed Doubao Ark Responses tool calls as corrupt", async () => {
    const writeTool: ToolDefinition = {
      name: "write",
      description: "Write a file",
      parameters: {
        type: "object",
        properties: {
          path: { type: "string" },
          content: { type: "string" },
        },
        required: ["path", "content"],
      },
    };

    const fetchMock = vi.fn(async () => makeSseResponse([
      {
        type: "response.output_item.added",
        output_index: 0,
        item: {
          id: "fc_1",
          type: "function_call",
          call_id: "call_write",
          name: "write",
          arguments: "",
        },
      },
      {
        type: "response.function_call_arguments.delta",
        output_index: 0,
        delta: "{\"path\":\"index.html\",\"content\":\"",
      },
      {
        type: "response.incomplete",
        response: {},
      },
    ]));
    vi.stubGlobal("fetch", fetchMock);

    const { createProviderInstance } = await import("../provider.js");
    const provider = createProviderInstance({
      providerId: "doubao",
      apiKey: "sk-test",
      baseURL: "https://ark.cn-beijing.volces.com/api/v3",
    });

    const chunks = await collect(provider.streamChat([{ role: "user", content: "write a file" }], {
      model: "doubao-seed-2-1-pro-260628",
      thinkingLevel: "high",
      tools: [writeTool],
    }));

    expect(chunks.filter((chunk) => chunk.type === "tool_call").at(-1)).toEqual({
      type: "tool_call",
      id: "call_write",
      name: "write",
      arguments: "",
      argumentsFull: "{}",
      argumentsCorrupt: true,
      isStart: false,
      isEnd: true,
    });
  });

  it("disables parallel tool calls for providers that require serialized tool calls", async () => {
    const tool: ToolDefinition = {
      name: "read",
      description: "Read a file",
      parameters: {
        type: "object",
        properties: { path: { type: "string" } },
        required: ["path"],
      },
    };

    let fireworksBody: any;
    createMock.mockImplementation(async (input) => {
      fireworksBody = input;
      return fromArray([{ choices: [{ delta: {}, finish_reason: "stop" }] }]);
    });

    const { createProviderInstance } = await import("../provider.js");
    const fireworks = createProviderInstance({
      providerId: "fireworks",
      apiKey: "sk-test",
      baseURL: "https://api.fireworks.ai/inference/v1",
    });

    await collect(fireworks.streamChat([{ role: "user", content: "hi" }], {
      model: "accounts/fireworks/models/kimi-k2p6",
      tools: [tool],
    }));

    expect(fireworksBody.parallel_tool_calls).toBe(false);

    let openaiBody: any;
    createMock.mockImplementation(async (input) => {
      openaiBody = input;
      return fromArray([{ choices: [{ delta: {}, finish_reason: "stop" }] }]);
    });

    const openai = createProviderInstance({
      providerId: "openai",
      apiKey: "sk-test",
      baseURL: "https://api.openai.com/v1",
    });

    await collect(openai.streamChat([{ role: "user", content: "hi" }], {
      model: "gpt-4o",
      tools: [tool],
    }));

    expect(openaiBody.parallel_tool_calls).toBeUndefined();
  });
  it("sends user-configured headers on the generic chat path", async () => {
    createMock.mockResolvedValue(fromArray([]));
    const { createProviderInstance } = await import("../provider.js");
    const OpenAI = (await import("openai")).default as unknown as ReturnType<typeof vi.fn>;
    OpenAI.mockClear();

    createProviderInstance({
      providerId: "kimi-for-coding",
      apiKey: "sk-test",
      baseURL: "https://api.kimi.com/coding/v1",
      headers: { "User-Agent": "claude-cli/1.0 (external, cli)", "x-custom": "1" },
    });

    expect(OpenAI).toHaveBeenCalledWith(expect.objectContaining({
      defaultHeaders: expect.objectContaining({
        "User-Agent": "claude-cli/1.0 (external, cli)",
        "x-custom": "1",
      }),
    }));
  });

  it("merges user headers over Ark defaults", async () => {
    const requestInits: RequestInit[] = [];
    const fetchMock = vi.fn(async (_input: RequestInfo | URL, init?: RequestInit) => {
      requestInits.push(init ?? {});
      return makeSseResponse([{ type: "response.completed", response: { usage: { input_tokens: 1, output_tokens: 1, total_tokens: 2 } } }]);
    });
    vi.stubGlobal("fetch", fetchMock);

    const { createProviderInstance } = await import("../provider.js");
    const provider = createProviderInstance({
      providerId: "doubao",
      apiKey: "sk-test",
      baseURL: "https://ark.cn-beijing.volces.com/api/v3",
      headers: { "User-Agent": "my-approved-client" },
    });
    await collect(provider.streamChat([{ role: "user", content: "hi" }], { model: "doubao-seed" }));

    const headers = requestInits[0]?.headers as Record<string, string>;
    expect(headers["User-Agent"]).toBe("my-approved-client");
    expect(headers.Authorization).toBe("Bearer sk-test");
  });

  it("merges user headers into Anthropic-protocol requests", async () => {
    const requestInits: RequestInit[] = [];
    const fetchMock = vi.fn(async (_input: RequestInfo | URL, init?: RequestInit) => {
      requestInits.push(init ?? {});
      return new Response("bad request", { status: 400 });
    });
    vi.stubGlobal("fetch", fetchMock);

    const { createProviderInstance } = await import("../provider.js");
    const provider = createProviderInstance({
      providerId: "kimi-for-coding",
      apiKey: "sk-test",
      baseURL: "https://api.kimi.com/coding",
      protocol: "anthropic-messages",
      headers: { "user-agent": "claude-cli/1.0 (external, cli)" },
    });
    await expect(
      collect(provider.streamChat([{ role: "user", content: "hi" }], { model: "kimi-k2.7-code" })),
    ).rejects.toThrow();

    const headers = requestInits[0]?.headers as Record<string, string>;
    expect(headers["user-agent"]).toBe("claude-cli/1.0 (external, cli)");
    expect(headers["x-api-key"]).toBe("sk-test");
  });
});

describe("sanitizeProviderHeaders", () => {
  it("keeps string values, drops the rest, and returns undefined when empty", async () => {
    const { sanitizeProviderHeaders } = await import("../provider-registry.js");
    expect(sanitizeProviderHeaders({ "User-Agent": "x", bad: 42, worse: null })).toEqual({ "User-Agent": "x" });
    expect(sanitizeProviderHeaders({})).toBeUndefined();
    expect(sanitizeProviderHeaders("nope")).toBeUndefined();
    expect(sanitizeProviderHeaders(undefined)).toBeUndefined();
  });
});

describe("streaming usage opt-in", () => {
  it("requests stream usage for coding-plan endpoints that withhold it otherwise", async () => {
    // kimi-for-coding streams no usage at all without the flag (verified live),
    // which silently disabled cost and context accounting for those sessions.
    for (const providerId of ["kimi-for-coding", "bailian-token-plan"]) {
      let body: any;
      createMock.mockImplementation(async (input) => {
        body = input;
        return fromArray([{ choices: [{ delta: { content: "hi" } }] }]);
      });
      const { createProviderInstance } = await import("../provider.js");
      const provider = createProviderInstance({
        providerId,
        apiKey: "sk-test",
        baseURL: "https://example.invalid/v1",
      });
      await collect(provider.streamChat([{ role: "user", content: "hi" }], { model: "m" }));
      expect(body.stream_options, providerId).toEqual({ include_usage: true });
    }
  });
});

describe("MOSS Chat Completions", () => {
  const tools = [{ name: "probe", description: "Probe", parameters: { type: "object" as const, properties: { value: { type: "number" } } } }];

  it("sends only the fields MOSS documents and reads the stream it documents", async () => {
    // The shape of the documented stream: reasoning, a tool call in fragments,
    // then usage alone in a chunk whose `choices` is empty.
    createMock.mockResolvedValueOnce(fromArray([
      { choices: [{ index: 0, delta: { reasoning_content: "Consider the tool" } }] },
      { choices: [{ index: 0, delta: { tool_calls: [{ index: 0, id: "call_1", type: "function", function: { name: "probe", arguments: '{"val' } }] } }] },
      { choices: [{ index: 0, delta: { tool_calls: [{ index: 0, function: { arguments: 'ue":7}' } }] }, finish_reason: "tool_calls" }] },
      {
        choices: [],
        usage: {
          prompt_tokens: 22,
          completion_tokens: 22,
          total_tokens: 44,
          prompt_tokens_details: { cached_tokens: 0 },
          completion_tokens_details: { reasoning_tokens: 18 },
        },
      },
    ]));
    const { createProviderInstance } = await import("../provider.js");
    const { getBuiltinProvider } = await import("../model-catalog.js");
    const provider = createProviderInstance({
      providerId: "moss",
      apiKey: "sk-test",
      baseURL: getBuiltinProvider("moss")!.baseURL,
    });

    const chunks = await collect(provider.streamChat([{ role: "user", content: "Use probe" }], {
      model: "nex-n2.6",
      thinkingLevel: "off",
      tools,
    }));

    const body = createMock.mock.calls.at(-1)![0];
    expect(Object.keys(body).filter((key) => body[key] !== undefined).sort()).toEqual([
      "max_tokens", "messages", "model", "stream", "thinking", "tool_choice", "tools",
    ]);
    expect(body).toMatchObject({ model: "nex-n2.6", stream: true, max_tokens: 131072, tool_choice: "auto" });
    // Asked for with thinking off, which this model does not have.
    expect(body.thinking).toEqual({ type: "enabled" });
    expect(chunks).toContainEqual({ type: "reasoning_delta", content: "Consider the tool" });
    expect(chunks.some((chunk) => chunk.type === "tool_call" && chunk.name === "probe" && chunk.isEnd && chunk.argumentsFull === '{"value":7}')).toBe(true);
    expect(chunks.some((chunk) => chunk.type === "usage"
      && chunk.usage.promptTokens === 22
      && chunk.usage.completionTokens === 22
      && chunk.usage.reasoningTokens === 18)).toBe(true);
  });

  it("returns a tool call's reasoning with its result", async () => {
    createMock.mockResolvedValueOnce(fromArray([{ choices: [{ index: 0, delta: { content: "7" } }] }]));
    const { createProviderInstance } = await import("../provider.js");
    const provider = createProviderInstance({ providerId: "moss", apiKey: "sk-test", baseURL: "https://api.mosi.cn/v1" });

    await collect(provider.streamChat([
      { role: "user", content: "Use probe" },
      { role: "assistant", content: "", reasoning: "Consider the tool", toolCalls: [{ id: "call_1", name: "probe", arguments: '{"value":7}' }] },
      { role: "tool", toolCallId: "call_1", content: "7" },
    ], { model: "nex-n2.6", tools }));

    expect(createMock.mock.calls.at(-1)![0].messages).toMatchObject([
      { role: "user" },
      { role: "assistant", reasoning_content: "Consider the tool", tool_calls: [{ id: "call_1" }] },
      { role: "tool", tool_call_id: "call_1", content: "7" },
    ]);
  });
});

