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
import { clearDynamicModelMetadata, getBuiltinModel } from "../model-catalog.js";
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
  it("does not reuse a list selected by a build that did not know the new tiers", async () => {
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
      // The key every build up to 0.0.65 wrote: same identity, no scope.
      const legacyKey = JSON.stringify([...(JSON.parse(currentKey) as unknown[]).slice(0, 5), null]);
      expect(currentKey).not.toBe(legacyKey);
      expect(currentKey).toContain("carbon");
      expect(currentKey).toContain("hydrogen");

      writeFileSync(join(bubbleHome, "model-discovery-cache.json"), JSON.stringify({
        [legacyKey]: {
          result: {
            models: ["gemini-4-argon", "gemini-3.8-flash", "gemini-3.7-flash", "gemini-3.6-flash", "gemini-3.5-flash"].map(cachedModel),
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
      const remote = ["gemini-4-argon", "gemini-4-carbon", "gemini-4-hydrogen", "gemini-3.8-flash", "gemini-3.7-flash", "gemini-3.6-flash", "gemini-3.5-flash"];
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
        "gemini-4-hydrogen",
        "gemini-3.8-flash",
        "gemini-3.7-flash",
      ]);
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
