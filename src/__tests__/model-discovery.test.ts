/**
 * Generic OpenAI-compatible /models discovery.
 *
 * Vendor catalogs probed 2026-08-04 are neither complete nor clean: zhipuai
 * omits glm-5.2, stepfun omits step-3.7-flash (both usable), kimi-for-coding
 * ships ids that share nothing with the builtin list, alibaba returns 236
 * entries including image/audio models, fireworks 412s. Discovery therefore
 * augments the curated catalog rather than replacing it.
 */

import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, describe, expect, it, vi } from "vitest";
import {
  ProviderRegistry,
  isLikelyChatModelId,
  isOpenAICompatibleProtocol,
  type ProviderProfile,
} from "../provider-registry.js";
import { clearDynamicModelMetadata, getBuiltinModel, listBuiltinModels, listDynamicModelMetadata } from "../model-catalog.js";
import { getCodexClientVersion } from "../provider-openai-codex.js";
import type { UserConfig } from "../config.js";

afterEach(() => {
  vi.unstubAllGlobals();
});

function fakeUserConfig(providers: ProviderProfile[]): UserConfig {
  return {
    getProviders: () => providers.slice(),
    setProviders: () => undefined,
    getDefaultProvider: () => undefined,
    setDefaultProvider: () => undefined,
  } as unknown as UserConfig;
}

function isolatedRegistry(providers: ProviderProfile[]): ProviderRegistry {
  const registry = new ProviderRegistry(fakeUserConfig(providers));
  (registry.getAuthStorage() as any).save = () => {};
  (registry as any).modelConfig = {
    getAllProviders: () => ({}),
    getCustomModels: () => [],
    hasProvider: () => false,
    getLoadError: () => undefined,
    getProviderConfig: () => undefined,
    getApiKey: () => undefined,
    getBaseURL: () => undefined,
    getProtocol: () => undefined,
    getPath: () => "/dev/null",
  };
  return registry;
}

function stubModelsResponse(ids: string[]): ReturnType<typeof vi.fn> {
  const fetchMock = vi.fn(async () =>
    new Response(JSON.stringify({ data: ids.map((id) => ({ id })) }), { status: 200 }));
  vi.stubGlobal("fetch", fetchMock);
  return fetchMock;
}

const stepfun: ProviderProfile = {
  id: "stepfun",
  name: "StepFun",
  baseURL: "https://api.stepfun.com/step_plan/v1",
  apiKey: "sk-test",
  enabled: true,
};

describe("OpenAI-compatible model discovery", () => {
  it("lists only the catalog for MOSS, whose /models is mostly not chat models", async () => {
    const provider: ProviderProfile = {
      id: "moss",
      name: "MOSS",
      baseURL: "https://api.mosi.cn/v1",
      apiKey: "sk-test",
      enabled: true,
    };
    // Ids the endpoint returned on 2026-11-21. None of these but the Nex
    // snapshot answers a chat request, and no id filter would know that.
    const fetchMock = stubModelsResponse([
      "moss-ttsd-1.0", "moss-vl-1.0", "moss-vl-realtime-1.0", "nex-n2.6", "nex-n2.6-2026-11-05", "tob-kimi-k3",
    ]);

    const result = await isolatedRegistry([provider]).discoverModels(provider);

    expect(result.models.map((model) => model.id)).toEqual(["nex-n2.6"]);
    expect(result.models[0]).toMatchObject({ contextWindow: 1_000_000, reasoningLevels: ["medium"] });
    expect(result).toMatchObject({ source: "static", authoritative: true });
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it("merges newly-added curated models into a stale non-authoritative cache", async () => {
    const provider: ProviderProfile = {
      id: "zhipuai-coding-plan",
      name: "Zhipu AI Coding Plan",
      baseURL: "https://open.bigmodel.cn/api/coding/paas/v4",
      apiKey: "sk-test",
      enabled: true,
    };
    const registry = isolatedRegistry([provider]);
    const key = (registry as any).modelDiscoveryKey(provider);
    (registry as any).modelDiscoveryCache.set(key, {
      result: {
        models: [{ id: "glm-5.2", name: "GLM-5.2", providerId: provider.id }],
        source: "remote",
        authoritative: false,
      },
      expiresAt: Date.now() + 60_000,
      identityKey: "stale",
      providerId: provider.id,
    });
    const fetchMock = vi.fn();
    vi.stubGlobal("fetch", fetchMock);

    const result = await registry.discoverModels(provider);

    expect(result.source).toBe("cache");
    expect(result.models).toContainEqual(expect.objectContaining({
      id: "glm-5.3-flash",
      tier: "fast",
      contextWindow: 1_000_000,
      reasoningLevels: ["low", "high", "max"],
      defaultReasoningLevel: "max",
    }));
    expect(result.models.map((model) => model.id)).toContain("glm-5.3");
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it("adds remote-only models to the curated catalog", async () => {
    stubModelsResponse(["step-3.5-flash", "step-9-future"]);
    const registry = isolatedRegistry([stepfun]);

    const result = await registry.discoverModels(stepfun);
    const ids = result.models.map((model) => model.id);

    expect(ids).toContain("step-9-future");
    // Curated entries the vendor list omits must survive (this one is usable
    // and priced despite never appearing in /models).
    expect(ids).toContain("step-3.7-flash");
    // Union membership is not a closed allowlist.
    expect(result.authoritative).toBe(false);
  });

  it("keeps curated metadata for ids present in both lists", async () => {
    stubModelsResponse(["step-3.7-flash"]);
    const registry = isolatedRegistry([stepfun]);

    const result = await registry.discoverModels(stepfun);
    const curated = result.models.filter((model) => model.id === "step-3.7-flash");

    expect(curated).toHaveLength(1);
    expect(curated[0].reasoningLevels?.length).toBeGreaterThan(0);
  });

  it("filters non-chat modalities out of noisy vendor catalogs", async () => {
    stubModelsResponse(["chat-next", "qwen-image-3.0", "text-embedding-v4", "cosyvoice-tts", "paraformer-asr"]);
    const alibaba: ProviderProfile = {
      id: "alibaba",
      name: "Alibaba",
      baseURL: "https://dashscope.aliyuncs.com/compatible-mode/v1",
      apiKey: "sk-test",
      enabled: true,
    };
    const registry = isolatedRegistry([alibaba]);

    const ids = (await registry.discoverModels(alibaba)).models.map((m) => m.id);

    expect(ids).toContain("chat-next");
    expect(ids).not.toContain("qwen-image-3.0");
    expect(ids).not.toContain("text-embedding-v4");
    expect(ids).not.toContain("cosyvoice-tts");
    expect(ids).not.toContain("paraformer-asr");
  });

  it("falls back to the curated catalog when /models fails", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => new Response("nope", { status: 412 })));
    const registry = isolatedRegistry([stepfun]);

    const result = await registry.discoverModels(stepfun);

    expect(result.source).toBe("static");
    expect(result.models.map((m) => m.id)).toContain("step-3.7-flash");
  });

  it("does not probe providers without an API key", async () => {
    const fetchMock = stubModelsResponse(["whatever"]);
    const registry = isolatedRegistry([{ ...stepfun, apiKey: "" }]);

    await registry.discoverModels({ ...stepfun, apiKey: "" });

    expect(fetchMock).not.toHaveBeenCalled();
  });

  it("classifies protocols and model ids", () => {
    expect(isOpenAICompatibleProtocol(undefined)).toBe(true);
    expect(isOpenAICompatibleProtocol("openai-chat")).toBe(true);
    expect(isOpenAICompatibleProtocol("openai-responses")).toBe(false);
    expect(isOpenAICompatibleProtocol("anthropic-messages")).toBe(false);
    expect(isOpenAICompatibleProtocol("ark-responses")).toBe(false);

    expect(isLikelyChatModelId("kimi-k3")).toBe(true);
    expect(isLikelyChatModelId("moonshot-v1-128k-vision-preview")).toBe(true);
    expect(isLikelyChatModelId("qwen-image-3.0-pro")).toBe(false);
    expect(isLikelyChatModelId("text-embedding-3-large")).toBe(false);
  });
});

describe("Gemini model discovery cache", () => {
  it("recomputes locally-derived reasoning levels when an old disk cache is restored", async () => {
    const previousBubbleHome = process.env.BUBBLE_HOME;
    const previousVitest = process.env.VITEST;
    const bubbleHome = mkdtempSync(join(tmpdir(), "bubble-gemini-cache-"));
    const provider: ProviderProfile = {
      id: "google",
      name: "Google Gemini",
      baseURL: "https://generativelanguage.googleapis.com/v1beta",
      apiKey: "g-key",
      enabled: true,
      protocol: "ai-sdk",
    };

    try {
      process.env.BUBBLE_HOME = bubbleHome;
      // Build the identity-aware key without loading disk state first.
      process.env.VITEST = "true";
      const probe = isolatedRegistry([provider]);
      const key = (probe as any).modelDiscoveryKey(provider);
      writeFileSync(join(bubbleHome, "model-discovery-cache.json"), JSON.stringify({
        [key]: {
          result: {
            models: [{
              id: "gemini-3.8-flash",
              name: "Gemini 3.8 Flash",
              providerId: "google",
              reasoningLevels: ["minimal", "low", "medium", "high"],
              contextWindow: 1_048_576,
            }],
            source: "remote",
            authoritative: true,
          },
          expiresAt: Date.now() + 60_000,
          identityKey: "old-build",
          providerId: "google",
          protocol: "ai-sdk",
        },
      }));

      clearDynamicModelMetadata("google");
      process.env.VITEST = "false";
      const registry = isolatedRegistry([provider]);

      // Startup overlay is already corrected before /model is opened.
      expect(getBuiltinModel("google", "gemini-3.8-flash")?.reasoningLevels)
        .toEqual(["off", "low", "medium", "high"]);

      const fetchMock = vi.fn();
      vi.stubGlobal("fetch", fetchMock);
      const result = await registry.discoverModels(provider);
      expect(result.source).toBe("cache");
      expect(result.models[0]?.reasoningLevels).toEqual(["off", "low", "medium", "high"]);
      expect(fetchMock).not.toHaveBeenCalled();
    } finally {
      clearDynamicModelMetadata("google");
      rmSync(bubbleHome, { recursive: true, force: true });
      if (previousBubbleHome === undefined) delete process.env.BUBBLE_HOME;
      else process.env.BUBBLE_HOME = previousBubbleHome;
      if (previousVitest === undefined) delete process.env.VITEST;
      else process.env.VITEST = previousVitest;
    }
  });
});

describe("Gemini model discovery cache across upgrades", () => {
  it.each([
    {
      // Every build up to 0.0.65: same identity, no scope.
      build: "that wrote no selection scope",
      scope: null,
      cached: ["gemini-4-argon", "gemini-3.8-flash", "gemini-3.7-flash", "gemini-3.6-flash", "gemini-3.5-flash"],
    },
    {
      // The builds just before Neon (0.0.66 onwards).
      build: "that knew Carbon and Hydrogen but not Neon",
      scope: "tiers:argon,carbon,hydrogen,pro,flash",
      cached: ["gemini-4-argon", "gemini-4-carbon", "gemini-4-hydrogen", "gemini-3.8-flash", "gemini-3.7-flash"],
    },
  ])("does not reuse a list selected by a build $build", async ({ scope, cached }) => {
    const previousBubbleHome = process.env.BUBBLE_HOME;
    const previousVitest = process.env.VITEST;
    const bubbleHome = mkdtempSync(join(tmpdir(), "bubble-gemini-upgrade-"));
    const provider: ProviderProfile = {
      id: "google",
      name: "Google Gemini",
      baseURL: "https://generativelanguage.googleapis.com/v1beta",
      apiKey: "g-key",
      enabled: true,
      protocol: "ai-sdk",
    };
    const cachedModel = (id: string) => ({
      id, name: id, providerId: "google", reasoningLevels: ["low", "medium", "high"], contextWindow: 1_048_576,
    });

    try {
      process.env.BUBBLE_HOME = bubbleHome;
      process.env.VITEST = "true";
      const probe = isolatedRegistry([provider]);
      const currentKey = (probe as any).modelDiscoveryKey(provider) as string;
      const legacyKey = JSON.stringify([...(JSON.parse(currentKey) as unknown[]).slice(0, 5), scope]);
      expect(currentKey).not.toBe(legacyKey);
      expect(currentKey).toContain("carbon");
      expect(currentKey).toContain("hydrogen");
      expect(currentKey).toContain("neon");

      writeFileSync(join(bubbleHome, "model-discovery-cache.json"), JSON.stringify({
        [legacyKey]: {
          result: {
            models: cached.map(cachedModel),
            source: "remote",
            authoritative: true,
          },
          expiresAt: Date.now() + 23 * 60 * 60 * 1000,
          identityKey: "old-build",
          providerId: "google",
          protocol: "ai-sdk",
        },
      }));

      clearDynamicModelMetadata("google");
      process.env.VITEST = "false";
      const registry = isolatedRegistry([provider]);
      const remote = ["gemini-4-argon", "gemini-4-carbon", "gemini-4-neon", "gemini-4-hydrogen", "gemini-3.8-flash", "gemini-3.7-flash", "gemini-3.6-flash", "gemini-3.5-flash"];
      const fetchMock = vi.fn(async () => new Response(JSON.stringify({
        models: remote.map((id) => ({
          name: `models/${id}`,
          displayName: id,
          inputTokenLimit: 1_048_576,
          supportedGenerationMethods: ["generateContent"],
        })),
      }), { status: 200, headers: { "content-type": "application/json" } }));
      vi.stubGlobal("fetch", fetchMock);

      const result = await registry.discoverModels(provider);
      expect(fetchMock).toHaveBeenCalledTimes(1);
      expect(result.source).toBe("remote");
      expect(result.models.map((model) => model.id)).toEqual([
        "gemini-4-argon",
        "gemini-4-carbon",
        "gemini-4-neon",
        "gemini-4-hydrogen",
        "gemini-3.8-flash",
      ]);
      // Remote discovery carries no thinking contract: Neon's comes from this build.
      expect(result.models[2]).toMatchObject({
        id: "gemini-4-neon",
        reasoningLevels: ["low", "medium", "high", "xhigh"],
        defaultReasoningLevel: "high",
        contextWindow: 1_048_576,
      });
    } finally {
      clearDynamicModelMetadata("google");
      rmSync(bubbleHome, { recursive: true, force: true });
      if (previousBubbleHome === undefined) delete process.env.BUBBLE_HOME;
      else process.env.BUBBLE_HOME = previousBubbleHome;
      if (previousVitest === undefined) delete process.env.VITEST;
      else process.env.VITEST = previousVitest;
    }
  });
});

describe("grok subscription discovery", () => {
  const grokProvider: ProviderProfile = {
    id: "grok",
    name: "Grok Subscription",
    baseURL: "https://cli-chat-proxy.grok.com/v1",
    apiKey: "",
    enabled: true,
  };

  it("surfaces remote-only models through the refreshing subscription fetch", async () => {
    const previousProxy = process.env.BUBBLE_SYSTEM_PROXY;
    process.env.BUBBLE_SYSTEM_PROXY = "0";
    try {
      const fetchMock = vi.fn(async (url: RequestInfo | URL, init?: RequestInit) => {
        expect(String(url)).toBe("https://cli-chat-proxy.grok.com/v1/models");
        const headers = new Headers(init?.headers);
        expect(headers.get("user-agent")).toBe("grok-cli");
        expect(headers.get("x-grok-client-version")).toBe("0.2.93");
        expect(headers.get("authorization")).toBe("Bearer access-token");
        return new Response(JSON.stringify({ data: [
          { id: "grok-4.5" },
          { id: "grok-5-next", name: "Grok 5" },
        ] }), { status: 200 });
      });
      vi.stubGlobal("fetch", fetchMock);

      const registry = isolatedRegistry([grokProvider]);
      registry.getAuthStorage().set("grok", {
        type: "oauth",
        accessToken: "access-token",
        refreshToken: "refresh-token",
        expiresAt: Date.now() + 24 * 60 * 60 * 1000,
      });
      const configured = registry.getConfigured().find((provider) => provider.id === "grok");
      expect(configured?.authType).toBe("oauth");

      const result = await registry.discoverModels(configured!);
      const ids = result.models.map((model) => model.id);

      expect(result.authoritative).toBe(true);
      // Curated entries survive even when the remote list omits them.
      expect(ids).toContain("grok-4.5");
      expect(ids).toContain("grok-composer-2.5-fast");
      // A newly-released remote-only model is surfaced without a code change.
      expect(ids).toContain("grok-5-next");
      // Its metadata lands in the dynamic overlay for routing/picker lookups.
      expect(getBuiltinModel("grok", "grok-5-next")?.name).toBe("Grok 5");
      // A flagship grok-N.M id infers the same ladder as curated grok-4.5, so it
      // is selectable with low/medium/high effort instead of being skipped.
      expect(getBuiltinModel("grok", "grok-5-next")?.reasoningLevels).toEqual(["low", "medium", "high"]);
      expect(getBuiltinModel("grok", "grok-5-next")?.defaultReasoningLevel).toBe("high");
      expect(getBuiltinModel("grok", "grok-5-next")?.contextWindow).toBe(500000);
    } finally {
      process.env.BUBBLE_SYSTEM_PROXY = previousProxy;
      vi.unstubAllGlobals();
    }
  });
});

describe("ChatGPT (openai oauth) discovery scope", () => {
  const oauthProvider: ProviderProfile = {
    id: "openai",
    name: "OpenAI",
    baseURL: "https://chatgpt.com/backend-api",
    apiKey: "eyJ.eyJodHRwczovL2FwaS5vcGVuYWkuY29tL2F1dGgiOnsiY2hhdGdwdF9hY2NvdW50X2lkIjoiYWNjdC0xIn19.sig",
    enabled: true,
    authType: "oauth",
  };

  it("keys the discovery cache by the claimed Codex client version", () => {
    const registry = isolatedRegistry([oauthProvider]);
    const key = (registry as any).modelDiscoveryKey(oauthProvider) as string;
    // A catalog written by an older build (or another running Bubble sharing
    // the disk cache) under a lower pin must not satisfy this build's lookup.
    expect(key).toContain(`codex-client:${getCodexClientVersion()}`);
    const apiKeyProvider: ProviderProfile = { ...oauthProvider, authType: "api", baseURL: "https://api.openai.com/v1" };
    expect((registry as any).modelDiscoveryKey(apiKeyProvider)).not.toContain("codex-client:");
  });

  it("warms discovery for account-scoped providers only, and only when nothing fresh is cached", async () => {
    const registry = isolatedRegistry([oauthProvider]);
    const discover = vi.spyOn(registry, "discoverModels").mockResolvedValue({ models: [], source: "remote", authoritative: true });
    await registry.warmModelDiscovery("openai");
    expect(discover).toHaveBeenCalledTimes(1);

    vi.spyOn(registry, "getCachedDiscoverySnapshot").mockReturnValue({
      models: [], source: "remote", complete: true, expiresAt: Date.now() + 60_000, identityKey: "acct",
    });
    await registry.warmModelDiscovery("openai");
    expect(discover).toHaveBeenCalledTimes(1);

    const apiRegistry = isolatedRegistry([{ ...oauthProvider, id: "deepseek", authType: "api", baseURL: "https://api.deepseek.com" }]);
    const apiDiscover = vi.spyOn(apiRegistry, "discoverModels");
    await apiRegistry.warmModelDiscovery("deepseek");
    expect(apiDiscover).not.toHaveBeenCalled();
  });

  it("keeps the confirmed catalog for routing past the freshness minute and across a failed refresh", async () => {
    const registry = isolatedRegistry([oauthProvider]);
    const provider = registry.getConfigured().find((item) => item.id === "openai")!;
    const key = (registry as any).modelDiscoveryKey(provider) as string;
    const base = Date.now();
    const nowSpy = vi.spyOn(Date, "now").mockReturnValue(base);
    try {
      (registry as any).modelDiscoveryCache.set(key, {
        result: { models: [{ id: "gpt-6-astra", name: "GPT-6-Astra", providerId: "openai" }], source: "remote", authoritative: true },
        expiresAt: base + 60_000,
        confirmed: { models: [{ id: "gpt-6-astra", name: "GPT-6-Astra", providerId: "openai" }], until: base + 24 * 60 * 60 * 1000 },
        identityKey: "acct",
        providerId: "openai",
        authType: "oauth",
      });
      expect(registry.getCachedDiscoverySnapshot("openai")?.complete).toBe(true);

      // Freshness minute passed: still confirmed for routing.
      nowSpy.mockReturnValue(base + 5 * 60_000);
      const stale = registry.getCachedDiscoverySnapshot("openai");
      expect(stale?.complete).toBe(true);
      expect(stale?.models.map((m) => m.id)).toEqual(["gpt-6-astra"]);

      // A failed refresh replaces the live result but carries the confirmation.
      (registry as any).modelDiscoveryCache.set(key, {
        result: { models: [{ id: "gpt-5.4-mini", name: "mini", providerId: "openai" }], source: "fallback", authoritative: false, error: "boom" },
        expiresAt: base + 5 * 60_000 + 10_000,
        confirmed: (registry as any).modelDiscoveryCache.get(key).confirmed,
        identityKey: "acct",
        providerId: "openai",
        authType: "oauth",
      });
      const afterFailure = registry.getCachedDiscoverySnapshot("openai");
      expect(afterFailure?.complete).toBe(true);
      expect(afterFailure?.models.map((m) => m.id)).toEqual(["gpt-6-astra"]);

      // Past the confirmation horizon nothing is trusted anymore.
      nowSpy.mockReturnValue(base + 25 * 60 * 60 * 1000);
      expect(registry.getCachedDiscoverySnapshot("openai")).toBeUndefined();
      expect(registry.getCachedDiscoverySnapshot("openai", { allowExpiredConfirmation: true })?.models.map(m => m.id))
        .toEqual(["gpt-6-astra"]);
    } finally {
      nowSpy.mockRestore();
    }
  });

  it("a complete discovery result records a confirmation horizon; a failure carries the previous one", async () => {
    const registry = isolatedRegistry([oauthProvider]);
    const provider = registry.getConfigured().find((item) => item.id === "openai")!;
    const key = (registry as any).modelDiscoveryKey(provider) as string;
    vi.spyOn(registry as any, "performModelDiscovery").mockResolvedValueOnce({
      models: [{ id: "gpt-6-astra", name: "GPT-6-Astra", providerId: "openai" }], source: "remote", authoritative: true,
    });
    await registry.discoverModels(provider, { forceRefresh: true });
    const confirmed = (registry as any).modelDiscoveryCache.get(key).confirmed;
    expect(confirmed?.models.map((m: { id: string }) => m.id)).toEqual(["gpt-6-astra"]);
    expect(confirmed?.until).toBeGreaterThan(Date.now() + 60_000);

    vi.spyOn(registry as any, "performModelDiscovery").mockResolvedValueOnce({
      models: [], source: "fallback", authoritative: false, error: "network down",
    });
    await registry.discoverModels(provider, { forceRefresh: true });
    expect((registry as any).modelDiscoveryCache.get(key).confirmed).toBe(confirmed);
    expect(registry.getCachedDiscoverySnapshot("openai")?.complete).toBe(true);
  });

  it("keeps the confirmation readable while a forced refresh is pending", async () => {
    const registry = isolatedRegistry([oauthProvider]);
    const provider = registry.getConfigured().find(item => item.id === "openai")!;
    const discovery = vi.spyOn(registry as any, "performModelDiscovery");
    discovery.mockResolvedValueOnce({
      models: [{ id: "gpt-fixture", name: "Fixture", providerId: "openai" }], source: "remote", authoritative: true,
    });
    await registry.discoverModels(provider, { forceRefresh: true });
    let finish!: (value: unknown) => void;
    discovery.mockReturnValueOnce(new Promise(resolve => { finish = resolve; }));
    const pending = registry.discoverModels(provider, { forceRefresh: true });
    expect(registry.getCachedDiscoverySnapshot("openai")?.models.map(m => m.id)).toEqual(["gpt-fixture"]);
    finish({ models: [], source: "fallback", authoritative: false, error: "offline" });
    await pending;
    expect(registry.getCachedDiscoverySnapshot("openai")?.models.map(m => m.id)).toEqual(["gpt-fixture"]);
  });

  it("persists a retained confirmation to disk when the live result is an error", () => {
    const previousBubbleHome = process.env.BUBBLE_HOME;
    const previousVitest = process.env.VITEST;
    const bubbleHome = mkdtempSync(join(tmpdir(), "bubble-confirmed-cache-"));
    try {
      process.env.BUBBLE_HOME = bubbleHome;
      process.env.VITEST = "false";
      const registry = isolatedRegistry([oauthProvider]);
      const provider = registry.getConfigured().find((item) => item.id === "openai")!;
      const key = (registry as any).modelDiscoveryKey(provider) as string;
      const until = Date.now() + 12 * 60 * 60 * 1000;
      (registry as any).modelDiscoveryCache.set(key, {
        result: { models: [], source: "fallback", authoritative: false, error: "network down" },
        expiresAt: Date.now() + 10_000,
        confirmed: { models: [{ id: "gpt-6-astra", name: "GPT-6-Astra", providerId: "openai" }], until },
        identityKey: "acct-1",
        providerId: "openai",
        authType: "oauth",
      });
      (registry as any).saveDiscoveryDiskCache();
      const written = JSON.parse(readFileSync(join(bubbleHome, "model-discovery-cache.json"), "utf8"));
      expect(written[key]?.result?.models?.map((m: { id: string }) => m.id)).toEqual(["gpt-6-astra"]);
      expect(written[key]?.result?.error).toBeUndefined();
      expect(written[key]?.expiresAt).toBe(until);

      // A restart during the outage restores the confirmation for routing.
      const restarted = isolatedRegistry([oauthProvider]);
      const snapshot = restarted.getCachedDiscoverySnapshot("openai");
      expect(snapshot?.complete).toBe(true);
      expect(snapshot?.models.map((m) => m.id)).toEqual(["gpt-6-astra"]);
    } finally {
      clearDynamicModelMetadata("openai-codex");
      rmSync(bubbleHome, { recursive: true, force: true });
      if (previousBubbleHome === undefined) delete process.env.BUBBLE_HOME;
      else process.env.BUBBLE_HOME = previousBubbleHome;
      if (previousVitest === undefined) delete process.env.VITEST;
      else process.env.VITEST = previousVitest;
    }
  });

  it("bounds the startup wait but lets a slow discovery finish in the background", async () => {
    const registry = isolatedRegistry([oauthProvider]);
    let settle!: () => void;
    const pending = new Promise<{ models: []; source: "remote"; authoritative: true }>((resolve) => {
      settle = () => resolve({ models: [], source: "remote", authoritative: true });
    });
    const discover = vi.spyOn(registry, "discoverModels").mockReturnValue(pending as never);

    const started = Date.now();
    await registry.waitForModelDiscovery("openai", 30);
    expect(Date.now() - started).toBeLessThan(1_000);
    expect(discover).toHaveBeenCalledTimes(1);

    // A fast discovery resolves the wait without hitting the cap.
    const quick = isolatedRegistry([oauthProvider]);
    vi.spyOn(quick, "discoverModels").mockResolvedValue({ models: [], source: "remote", authoritative: true });
    await quick.waitForModelDiscovery("openai", 5_000);
    settle();
  });
});

describe("what a saved model list may say at startup", () => {
  const BARE = (id: string, extra: Record<string, unknown> = {}) => ({ id, name: id, providerId: "google", ...extra });

  /** A home whose models.json lists Google models, as a user would write it. */
  function withListedGoogleModels(
    ids: string[],
    run: (home: { path: string; currentKey: string; start: () => ProviderRegistry }) => void | Promise<void>,
  ) {
    return async () => {
      const previousBubbleHome = process.env.BUBBLE_HOME;
      const previousVitest = process.env.VITEST;
      const bubbleHome = mkdtempSync(join(tmpdir(), "bubble-saved-list-"));
      try {
        process.env.BUBBLE_HOME = bubbleHome;
        writeFileSync(join(bubbleHome, "models.json"), JSON.stringify({
          providers: { google: { apiKey: "g-key", models: ids.map((id) => ({ id })) } },
        }));
        const start = () => {
          const registry = new ProviderRegistry(fakeUserConfig([]));
          (registry.getAuthStorage() as any).save = () => {};
          return registry;
        };
        // The key this configuration looks its list up under, read without loading disk state.
        process.env.VITEST = "true";
        const probe = start();
        const google = probe.getConfigured().find((provider) => provider.id === "google")!;
        expect(google).toMatchObject({ apiKey: "g-key", protocol: "ai-sdk" });
        const currentKey = (probe as any).modelDiscoveryKey(google) as string;
        clearDynamicModelMetadata("google");
        process.env.VITEST = "false";
        await run({ path: bubbleHome, currentKey, start });
      } finally {
        clearDynamicModelMetadata("google");
        rmSync(bubbleHome, { recursive: true, force: true });
        if (previousBubbleHome === undefined) delete process.env.BUBBLE_HOME;
        else process.env.BUBBLE_HOME = previousBubbleHome;
        if (previousVitest === undefined) delete process.env.VITEST;
        else process.env.VITEST = previousVitest;
      }
    };
  }

  const saveAll = (path: string, lists: Array<{ key: string; models: unknown[]; source?: string }>) =>
    writeFileSync(join(path, "model-discovery-cache.json"), JSON.stringify(Object.fromEntries(lists.map((list) => [list.key, {
      result: { models: list.models, source: list.source ?? "static", authoritative: true },
      expiresAt: Date.now() + 23 * 60 * 60 * 1000,
      identityKey: "an-older-build",
      providerId: "google",
      protocol: "ai-sdk",
    }]))));
  const save = (path: string, key: string, models: unknown[], source = "static") => saveAll(path, [{ key, models, source }]);
  /** The key an older build, with other selection rules, saved the same provider's list under. */
  const olderBuildKey = (currentKey: string) =>
    JSON.stringify([...(JSON.parse(currentKey) as unknown[]).slice(0, 5), "tiers:argon,carbon,hydrogen,pro,flash"]);
  /** What the overlay alone holds: `getBuiltinModel` would answer from the catalog where the overlay is silent. */
  const overlaid = (id: string) => listDynamicModelMetadata("google").find((model) => model.id === id);

  it("says nothing when it was saved under another key", withListedGoogleModels(["gemini-4-hydrogen"], ({ path, currentKey, start }) => {
    // The same provider as an older build saved it: its own selection rules in
    // the key. Its list says things this build's catalog does not: another
    // default and window for Hydrogen, and a model the catalog has no entry for.
    const olderKey = olderBuildKey(currentKey);
    expect(olderKey).not.toBe(currentKey);
    save(path, olderKey, [
      BARE("gemini-4-hydrogen", { reasoningLevels: ["low", "medium", "high"], defaultReasoningLevel: "low", contextWindow: 123_456 }),
      BARE("gemini-3.6-flash", { reasoningLevels: ["minimal", "low", "medium", "high"], contextWindow: 123_456 }),
    ], "remote");

    const registry = start();

    // Nothing of it is in the overlay, so the catalog's own Hydrogen stands and
    // the model only that list knows stays unknown.
    expect(listDynamicModelMetadata("google")).toEqual([]);
    expect(getBuiltinModel("google", "gemini-4-hydrogen")).toMatchObject({
      reasoningLevels: ["low", "medium", "high", "xhigh"], defaultReasoningLevel: "high", contextWindow: 1_048_576,
    });
    expect(getBuiltinModel("google", "gemini-3.6-flash")).toBeUndefined();
    // The other build's list is kept for that build; it is not thrown away.
    expect((registry as any).modelDiscoveryCache.get(olderKey)?.result.models.map((model: { id: string }) => model.id))
      .toEqual(["gemini-4-hydrogen", "gemini-3.6-flash"]);
    // And it is not this configuration's catalog.
    expect(registry.getCachedDiscoverySnapshot("google")).toBeUndefined();
  }));

  it("is the provider's list when it was saved under today's key", withListedGoogleModels([], ({ path, currentKey, start }) => {
    // Google's own list as an older build with today's key saved it. The
    // catalog has neither model, so whatever is known of them after the launch
    // can only have come from this list. Both ladders are that build's guesses.
    save(path, currentKey, [
      BARE("gemini-3.6-flash", { reasoningLevels: ["low", "medium", "high"], contextWindow: 2_000_000 }),
      BARE("gemini-flash-latest", { reasoningLevels: ["minimal", "low", "medium", "high"], contextWindow: 2_000_000 }),
    ], "remote");

    const registry = start();

    // The id's family decides the ladder, the saved list the window.
    expect(getBuiltinModel("google", "gemini-3.6-flash")).toMatchObject({
      reasoningLevels: ["minimal", "low", "medium", "high"], contextWindow: 2_000_000,
    });
    expect(getBuiltinModel("google", "gemini-3.6-flash")?.reasoningLevelsAssumed).toBeUndefined();
    // An id with no version implies no ladder: the guess is gone, from the
    // overlay and from the list that will be saved again.
    expect(overlaid("gemini-flash-latest")).toMatchObject({ reasoningLevels: ["off"], reasoningLevelsAssumed: true, contextWindow: 2_000_000 });
    const kept = (registry as any).modelDiscoveryCache.get(currentKey).result.models as Array<{ id: string; reasoningLevels?: string[] }>;
    expect(kept.map((model) => model.id)).toEqual(["gemini-3.6-flash", "gemini-flash-latest"]);
    expect(kept[1].reasoningLevels).toBeUndefined();
    expect(registry.getCachedDiscoverySnapshot("google")).toMatchObject({ complete: true });
  }));

  it("stays on disk for the key it belongs to when another key's lookup rewrites the file", withListedGoogleModels([], async ({ path, currentKey, start }) => {
    const olderKey = olderBuildKey(currentKey);
    const theirs = [BARE("gemini-3.6-flash", { reasoningLevels: ["minimal", "low", "medium", "high"], contextWindow: 123_456 })];
    saveAll(path, [
      { key: olderKey, models: theirs, source: "remote" },
      { key: currentKey, models: [BARE("gemini-3.5-flash", { reasoningLevels: ["minimal", "low", "medium", "high"], contextWindow: 2_000_000 })], source: "remote" },
    ]);
    const registry = start();
    expect(listDynamicModelMetadata("google").map((model) => model.id)).toEqual(["gemini-3.5-flash"]);

    const fetchMock = vi.fn(async () => new Response(JSON.stringify({
      models: [{ name: "models/gemini-4-neon", displayName: "Gemini 4 Neon", inputTokenLimit: 1_048_576, supportedGenerationMethods: ["generateContent"] }],
    }), { status: 200, headers: { "content-type": "application/json" } }));
    vi.stubGlobal("fetch", fetchMock);
    const google = registry.getConfigured().find((provider) => provider.id === "google")!;
    const result = await registry.discoverModels(google, { forceRefresh: true });
    expect(fetchMock).toHaveBeenCalledTimes(1);
    expect(result).toMatchObject({ source: "remote", authoritative: true });

    // The successful lookup wrote the file again: this key's list is the new
    // one, and the other key's list is there as it was.
    const written = JSON.parse(readFileSync(join(path, "model-discovery-cache.json"), "utf8"));
    expect(written[currentKey].result.models.map((model: { id: string }) => model.id)).toEqual(["gemini-4-neon"]);
    expect(written[olderKey].result).toMatchObject({ source: "remote", authoritative: true });
    expect(written[olderKey].result.models).toEqual(theirs);
    expect(written[olderKey]).toMatchObject({ identityKey: "an-older-build", providerId: "google", protocol: "ai-sdk" });
    // It still feeds nothing.
    expect(listDynamicModelMetadata("google").map((model) => model.id)).toEqual(["gemini-4-neon"]);
  }));

  it("is read again from models.json when it was the local list", withListedGoogleModels(
    ["gemini-4-hydrogen", "gemini-4-neon", "gemini-flash-latest"],
    async ({ path, currentKey, start }) => {
      // Saved under today's key, but by a build that kept bare entries and
      // guessed a ladder for an id that names no version. A model since
      // removed from models.json is still in it.
      save(path, currentKey, [
        BARE("gemini-4-hydrogen", { reasoningLevels: ["low", "medium", "high", "xhigh"] }),
        BARE("gemini-flash-latest", { reasoningLevels: ["minimal", "low", "medium", "high"] }),
        BARE("gemini-2.5-pro", { reasoningLevels: ["low", "medium", "high"], defaultReasoningLevel: "low", contextWindow: 123_456 }),
      ]);

      const registry = start();

      // Before any lookup: models.json decides which models there are, and the
      // catalog what the ones it knows can do.
      expect(getBuiltinModel("google", "gemini-4-hydrogen")).toMatchObject({
        reasoningLevels: ["low", "medium", "high", "xhigh"], defaultReasoningLevel: "high", contextWindow: 1_048_576,
      });
      expect(getBuiltinModel("google", "gemini-4-neon")).toMatchObject({ defaultReasoningLevel: "high", contextWindow: 1_048_576 });
      expect(getBuiltinModel("google", "gemini-4-hydrogen")?.reasoningLevelsAssumed).toBeUndefined();
      // The unversioned alias is the user's own entry: selectable, on a ladder marked as a placeholder.
      // The older build's guess for it is gone.
      expect(overlaid("gemini-flash-latest")).toMatchObject({ reasoningLevels: ["off"], reasoningLevelsAssumed: true });
      // Not in models.json any more, so the saved list does not bring an overlay for it.
      expect(listDynamicModelMetadata("google").map((model) => model.id)).toEqual(["gemini-4-hydrogen", "gemini-4-neon", "gemini-flash-latest"]);
      expect(getBuiltinModel("google", "gemini-2.5-pro")).toMatchObject({ defaultReasoningLevel: "high", contextWindow: 1_048_576 });

      const fetchMock = vi.fn();
      vi.stubGlobal("fetch", fetchMock);
      const google = registry.getConfigured().find((provider) => provider.id === "google")!;
      const result = await registry.discoverModels(google);
      expect(fetchMock).not.toHaveBeenCalled();
      expect(result.models.map((model) => model.id)).toEqual(["gemini-4-hydrogen", "gemini-4-neon", "gemini-flash-latest"]);
      expect(result.models[2].reasoningLevels).toBeUndefined();
    },
  ));
});

describe("models.json entries that name a catalog model", () => {
  it("take the catalog's attributes and keep what the user set", () => {
    const registry = isolatedRegistry([]);
    const listed = (models: Array<Record<string, unknown>>) => {
      (registry as any).modelConfig.getCustomModels = (providerId: string) => models.map((model) => ({ name: model.id, providerId, ...model }));
    };

    // A ChatGPT sign-in is provider "openai" on the Codex catalog: the transport attributes come along.
    listed([{ id: "gpt-5.6-terra" }, { id: "my-own-model", tier: "fast" }]);
    const codex = (registry as any).customModelsForProvider({ id: "openai", authType: "oauth" });
    expect(codex[0]).toMatchObject({
      id: "gpt-5.6-terra",
      providerId: "openai",
      defaultReasoningLevel: "medium",
      contextWindow: 372000,
      tier: "strong",
      useResponsesLite: true,
      toolOutputTokenLimit: 10000,
    });
    // An id the catalog does not have is left exactly as the user wrote it.
    expect(codex[1]).toEqual({ id: "my-own-model", name: "my-own-model", providerId: "openai", tier: "fast" });

    // The user's tier wins over the catalog's; an unset one is the catalog's.
    listed([{ id: "gemini-4-neon", tier: "fast", name: "Neon (mine)" }, { id: "gemini-4-hydrogen" }]);
    const google = (registry as any).customModelsForProvider({ id: "google" });
    expect(google[0]).toMatchObject({ name: "Neon (mine)", tier: "fast", defaultReasoningLevel: "high", contextWindow: 1_048_576 });
    expect(google[1]).toMatchObject({ tier: "balanced", reasoningLevels: ["low", "medium", "high", "xhigh"], defaultReasoningLevel: "high" });
  });
});

/**
 * A Bubble home of its own, with the saved-list file switched on as it is
 * outside the test runner.
 */
interface BubbleHomeFixture {
  /** models.json, as the user wrote it. */
  configure(providers: Record<string, { apiKey: string; models?: string[] }>): void;
  /** A launch. It reads models.json, and the lists earlier lookups saved unless `savedLists` is false. */
  launch(options?: { savedLists?: boolean }): ProviderRegistry;
  /** model-discovery-cache.json as it is now. */
  saved(): Record<string, { result: { models: Array<{ id: string }>; source: string; authoritative: boolean }; expiresAt: number }>;
}

async function inBubbleHome(run: (home: BubbleHomeFixture) => Promise<void>): Promise<void> {
  const previousBubbleHome = process.env.BUBBLE_HOME;
  const previousVitest = process.env.VITEST;
  const path = mkdtempSync(join(tmpdir(), "bubble-model-list-"));
  const clearOverlays = () => ["google", "grok", "stepfun"].forEach(clearDynamicModelMetadata);
  try {
    process.env.BUBBLE_HOME = path;
    process.env.VITEST = "false";
    clearOverlays();
    await run({
      configure: (providers) => writeFileSync(join(path, "models.json"), JSON.stringify({
        providers: Object.fromEntries(Object.entries(providers).map(([id, provider]) => [id, {
          apiKey: provider.apiKey,
          ...(provider.models ? { models: provider.models.map((model) => ({ id: model })) } : {}),
        }])),
      })),
      launch: ({ savedLists = true } = {}) => {
        // Only the constructor reads the file; lookups made later still write it.
        process.env.VITEST = savedLists ? "false" : "true";
        try {
          const registry = new ProviderRegistry(fakeUserConfig([]));
          (registry.getAuthStorage() as any).save = () => {};
          return registry;
        } finally {
          process.env.VITEST = "false";
        }
      },
      saved: () => JSON.parse(readFileSync(join(path, "model-discovery-cache.json"), "utf8")),
    });
  } finally {
    clearOverlays();
    rmSync(path, { recursive: true, force: true });
    if (previousBubbleHome === undefined) delete process.env.BUBBLE_HOME;
    else process.env.BUBBLE_HOME = previousBubbleHome;
    if (previousVitest === undefined) delete process.env.VITEST;
    else process.env.VITEST = previousVitest;
  }
}

/** Not the catalog's 1,048,576, so a window read back can be told apart from the catalog's. */
const REMOTE_WINDOW = 2_000_000;

/** What the providers' own model endpoints list. A provider left out answers 500. */
function stubRemoteCatalogs(catalogs: { google?: string[]; openaiCompatible?: string[] }): ReturnType<typeof vi.fn> {
  const fetchMock = vi.fn(async (url: RequestInfo | URL) => {
    const ids = String(url).includes("generativelanguage.googleapis.com") ? catalogs.google : catalogs.openaiCompatible;
    if (!ids) return new Response("not expected", { status: 500 });
    return new Response(JSON.stringify(String(url).includes("generativelanguage.googleapis.com")
      ? { models: ids.map((id) => ({ name: `models/${id}`, displayName: id, inputTokenLimit: REMOTE_WINDOW, supportedGenerationMethods: ["generateContent"] })) }
      : { data: ids.map((id) => ({ id })) }), { status: 200, headers: { "content-type": "application/json" } });
  });
  vi.stubGlobal("fetch", fetchMock);
  return fetchMock;
}

const configured = (registry: ProviderRegistry, providerId: string): ProviderProfile =>
  registry.getConfigured().find((provider) => provider.id === providerId)!;

/** Puts the saved lists in a registry's memory, as its own lookups of a moment ago would have left them. */
function rememberSavedLists(registry: ProviderRegistry, saved: ReturnType<BubbleHomeFixture["saved"]>): void {
  for (const [key, entry] of Object.entries(saved)) {
    const complete = entry.result.source === "remote" && entry.result.authoritative;
    (registry as any).modelDiscoveryCache.set(key, {
      ...structuredClone(entry),
      expiresAt: Date.now() + 60_000,
      confirmed: complete ? { models: structuredClone(entry.result.models), until: entry.expiresAt } : undefined,
    });
  }
}

describe("a model is the same model whichever way its list was produced", () => {
  type Known = { levels?: string[]; defaultLevel?: string; window?: number };
  const GOOGLE_LISTS = ["gemini-4-argon", "gemini-4-neon", "gemini-3.8-flash", "gemini-3.6-flash"];
  const NEON_LEVELS = ["low", "medium", "high", "xhigh"];
  const FLASH_3_LEVELS = ["minimal", "low", "medium", "high"];

  const lists: Array<{
    list: string;
    /** In models.json from the start. */
    listed?: string[];
    /** What Google's endpoint lists. */
    remote?: string[];
    /** Written to models.json after the first lookup saved its list. */
    listedAfterwards?: string[];
    /** The whole list, in order, and what is known of each model. */
    expected: Record<string, Known>;
  }> = [
    {
      list: "models.json naming a model the catalog has",
      listed: ["gemini-4-neon"],
      expected: { "gemini-4-neon": { levels: NEON_LEVELS, defaultLevel: "high", window: 1_048_576 } },
    },
    {
      list: "models.json naming a versioned model the catalog lacks",
      listed: ["gemini-3.6-flash"],
      expected: { "gemini-3.6-flash": { levels: FLASH_3_LEVELS } },
    },
    {
      list: "models.json naming an id with no version",
      listed: ["gemini-flash-latest"],
      // No ladder is known: the overlay carries the marked placeholder.
      expected: { "gemini-flash-latest": {} },
    },
    {
      list: "Google's own list",
      remote: GOOGLE_LISTS,
      expected: {
        "gemini-4-argon": { levels: ["low", "medium", "high"], defaultLevel: "medium", window: REMOTE_WINDOW },
        "gemini-4-neon": { levels: NEON_LEVELS, defaultLevel: "high", window: REMOTE_WINDOW },
        "gemini-3.8-flash": { levels: ["off", "low", "medium", "high"], window: REMOTE_WINDOW },
        "gemini-3.6-flash": { levels: FLASH_3_LEVELS, window: REMOTE_WINDOW },
      },
    },
    {
      list: "models.json written after Google's own list was saved under the same key",
      remote: GOOGLE_LISTS,
      listedAfterwards: ["gemini-4-neon"],
      // The catalog's Neon, not the saved list's: the window says which.
      expected: { "gemini-4-neon": { levels: NEON_LEVELS, defaultLevel: "high", window: 1_048_576 } },
    },
  ];

  const paths: Record<string, (home: BubbleHomeFixture) => Promise<Awaited<ReturnType<ProviderRegistry["discoverModels"]>> | undefined>> = {
    "a fresh lookup": async (home) => {
      const registry = home.launch();
      clearDynamicModelMetadata("google");
      const result = await registry.discoverModels(configured(registry, "google"), { forceRefresh: true });
      expect(result.source).not.toBe("cache");
      return result;
    },
    "a lookup answered from the cache": async (home) => {
      // Nothing is restored at launch here: the cached list is in memory only,
      // so what is known afterwards is what answering from the cache made of it.
      const registry = home.launch({ savedLists: false });
      rememberSavedLists(registry, home.saved());
      clearDynamicModelMetadata("google");
      const fetchMock = stubRemoteCatalogs({});
      const result = await registry.discoverModels(configured(registry, "google"));
      expect(result.source).toBe("cache");
      expect(fetchMock).not.toHaveBeenCalled();
      return result;
    },
    "a launch that restores the saved list": async (home) => {
      clearDynamicModelMetadata("google");
      home.launch();
      // No lookup: the launch alone has to have said it.
      return undefined;
    },
  };

  for (const { list, listed, remote, listedAfterwards, expected } of lists) {
    it.each(Object.keys(paths))(`${list}, by %s`, (path) => inBubbleHome(async (home) => {
      // Every case starts from a list an earlier lookup saved.
      home.configure({ google: { apiKey: "g-key", models: listed } });
      stubRemoteCatalogs({ google: remote });
      const first = home.launch();
      const earlier = await first.discoverModels(configured(first, "google"));
      expect(earlier.source).toBe(remote ? "remote" : "static");
      expect(Object.keys(home.saved())).toEqual([(first as any).modelDiscoveryKey(configured(first, "google"))]);
      if (listedAfterwards) home.configure({ google: { apiKey: "g-key", models: listedAfterwards } });

      const result = await paths[path](home);

      const members = Object.keys(expected);
      const overlay = listDynamicModelMetadata("google");
      expect(overlay.map((model) => model.id)).toEqual(members);
      if (result) expect(result.models.map((model) => model.id)).toEqual(members);
      for (const [id, known] of Object.entries(expected)) {
        const overlaid = overlay.find((model) => model.id === id)!;
        expect({
          reasoningLevels: overlaid.reasoningLevels,
          defaultReasoningLevel: overlaid.defaultReasoningLevel,
          contextWindow: overlaid.contextWindow,
          reasoningLevelsAssumed: overlaid.reasoningLevelsAssumed,
        }, `${id} in the overlay`).toEqual({
          reasoningLevels: known.levels ?? ["off"],
          defaultReasoningLevel: known.defaultLevel,
          contextWindow: known.window,
          reasoningLevelsAssumed: known.levels ? undefined : true,
        });
        // The overlay is what the rest of Bubble asks.
        expect(getBuiltinModel("google", id)).toBe(overlaid);
        if (!result) continue;
        const returned = result.models.find((model) => model.id === id)!;
        expect({
          reasoningLevels: returned.reasoningLevels,
          defaultReasoningLevel: returned.defaultReasoningLevel,
          contextWindow: returned.contextWindow,
        }, `${id} in the returned list`).toEqual({
          reasoningLevels: known.levels,
          defaultReasoningLevel: known.defaultLevel,
          contextWindow: known.window,
        });
      }
    }));
  }
});

describe("models listed in models.json", () => {
  // The catalog's own models of a provider, in its order: a list that only adds
  // to the catalog starts with them, whichever they are this month.
  const builtinIds = (providerId: string) => listBuiltinModels(providerId).map((model) => model.id);

  it("are told the same on a second lookup within the minute as on the first", () => inBubbleHome(async (home) => {
    home.configure({ google: { apiKey: "k", models: ["gemini-3.6-flash"] } });
    const registry = home.launch();
    const google = configured(registry, "google");

    const first = await registry.discoverModels(google);
    expect(first.source).toBe("static");
    expect(first.models[0].reasoningLevels).toEqual(["minimal", "low", "medium", "high"]);
    const known = getBuiltinModel("google", "gemini-3.6-flash");
    expect(known).toMatchObject({ reasoningLevels: ["minimal", "low", "medium", "high"] });
    expect(known?.reasoningLevelsAssumed).toBeUndefined();

    const second = await registry.discoverModels(google);
    expect(second.source).toBe("cache");
    expect(second.models).toEqual(first.models);
    expect(getBuiltinModel("google", "gemini-3.6-flash")).toEqual(known);
  }));

  it.each([
    {
      provider: "google",
      // Google's list is the complete one: it was the confirmed membership.
      remote: { google: ["gemini-4-argon", "gemini-3.6-flash"] },
      before: { source: "remote", authoritative: true, ids: ["gemini-4-argon", "gemini-3.6-flash"] },
      listed: ["gemini-4-neon"],
      overlay: [{ id: "gemini-4-neon", reasoningLevels: ["low", "medium", "high", "xhigh"], defaultReasoningLevel: "high", contextWindow: 1_048_576 }],
    },
    {
      provider: "stepfun",
      // An OpenAI-compatible list only adds to the catalog; it is saved as a union.
      remote: { openaiCompatible: ["step-3.7-flash", "step-9-future"] },
      before: { source: "remote", authoritative: false, ids: [...builtinIds("stepfun"), "step-9-future"] },
      listed: ["step-3.7-flash"],
      overlay: [],
    },
    {
      provider: "grok",
      remote: { openaiCompatible: ["grok-4.5", "grok-5-next"] },
      before: { source: "remote", authoritative: false, ids: [...builtinIds("grok"), "grok-5-next"] },
      listed: ["grok-composer-2.5-fast"],
      overlay: [{ id: "grok-composer-2.5-fast", contextWindow: 200_000 }],
    },
  ])("are the list of $provider even when its own list was saved first under the same key", ({ provider: providerId, remote, before, listed, overlay }) => inBubbleHome(async (home) => {
    // Before the user listed anything: the provider's own list is looked up and saved.
    home.configure({ [providerId]: { apiKey: "k" } });
    stubRemoteCatalogs(remote);
    const earlier = home.launch();
    const key = (earlier as any).modelDiscoveryKey(configured(earlier, providerId)) as string;
    const looked = await earlier.discoverModels(configured(earlier, providerId));
    expect({ source: looked.source, authoritative: looked.authoritative, ids: looked.models.map((model) => model.id) }).toEqual(before);
    const savedBefore = home.saved();
    expect(savedBefore[key].result.models.map((model) => model.id)).toEqual(before.ids);

    // Listing models does not change the key the saved list is found under.
    home.configure({ [providerId]: { apiKey: "k", models: listed } });
    const fetchMock = stubRemoteCatalogs({});
    const expectTheListedModels = async (registry: ProviderRegistry) => {
      expect(listDynamicModelMetadata(providerId).map((model) => model.id)).toEqual(overlay.map((model) => model.id));
      overlay.forEach((model, index) => expect(listDynamicModelMetadata(providerId)[index]).toMatchObject(model));
      // The saved list is not this provider's membership any more, confirmed or otherwise.
      const snapshot = registry.getCachedDiscoverySnapshot(providerId);
      expect(snapshot?.models.map((model) => model.id)).toEqual(listed);
      expect(snapshot?.complete).toBe(false);
    };

    // At launch, before any lookup.
    [providerId].forEach(clearDynamicModelMetadata);
    const launched = home.launch();
    expect((launched as any).modelDiscoveryKey(configured(launched, providerId))).toBe(key);
    await expectTheListedModels(launched);
    const afterLaunch = await launched.discoverModels(configured(launched, providerId));
    expect(afterLaunch.models.map((model) => model.id)).toEqual(listed);
    await expectTheListedModels(launched);

    // And when the saved list is only in memory, on the lookup it answers.
    [providerId].forEach(clearDynamicModelMetadata);
    const running = home.launch({ savedLists: false });
    rememberSavedLists(running, savedBefore);
    const revision = running.getRoutingRevision();
    const answered = await running.discoverModels(configured(running, providerId));
    expect(answered.source).toBe("cache");
    expect(answered.models.map((model) => model.id)).toEqual(listed);
    await expectTheListedModels(running);
    // Who the members are changed, once: routing is told, and not told again.
    expect(running.getRoutingRevision()).toBe(revision + 1);
    await running.discoverModels(configured(running, providerId));
    expect(running.getRoutingRevision()).toBe(revision + 1);

    // And on a forced lookup, which would otherwise carry the earlier confirmation forward.
    [providerId].forEach(clearDynamicModelMetadata);
    const refreshing = home.launch({ savedLists: false });
    rememberSavedLists(refreshing, savedBefore);
    const refreshed = await refreshing.discoverModels(configured(refreshing, providerId), { forceRefresh: true });
    expect(refreshed).toMatchObject({ source: "static", authoritative: true });
    expect(refreshed.models.map((model) => model.id)).toEqual(listed);
    await expectTheListedModels(refreshing);

    expect(fetchMock).not.toHaveBeenCalled();
  }));

  it("do not look like a new membership on every lookup when an id is listed twice", () => inBubbleHome(async (home) => {
    home.configure({ google: { apiKey: "k", models: ["gemini-4-neon", "gemini-3.6-flash", "gemini-4-neon"] } });
    const registry = home.launch();
    const google = configured(registry, "google");

    await registry.discoverModels(google);
    const revision = registry.getRoutingRevision();
    // Answered from the cache: the same models as a moment ago, so routing has nothing to rebuild.
    for (let lookup = 0; lookup < 3; lookup++) {
      expect((await registry.discoverModels(google)).source).toBe("cache");
      expect(registry.getRoutingRevision()).toBe(revision);
    }
    // Nor on a lookup that goes back to models.json and finds the same list.
    await registry.discoverModels(google, { forceRefresh: true });
    expect(registry.getRoutingRevision()).toBe(revision);
  }));

  it("are told the same by a lookup that a later lookup overtook", () => inBubbleHome(async (home) => {
    home.configure({ google: { apiKey: "k", models: ["gemini-3.6-flash", "gemini-4-neon", "gemini-flash-latest"] } });
    const registry = home.launch();
    const google = configured(registry, "google");

    const [overtaken, latest] = await Promise.all([
      registry.discoverModels(google, { forceRefresh: true }),
      registry.discoverModels(google, { forceRefresh: true }),
    ]);

    expect(latest.source).toBe("static");
    expect(overtaken).toMatchObject({ source: "fallback", authoritative: false, error: "Model catalog changed while it was refreshing." });
    expect(overtaken.models).toEqual(latest.models);
    expect(overtaken.models[0].reasoningLevels).toEqual(["minimal", "low", "medium", "high"]);
  }));
});
