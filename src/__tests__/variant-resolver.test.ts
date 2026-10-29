import { describe, expect, it } from "vitest";
import {
  clearDynamicModelMetadata,
  getBuiltinModel,
  getBuiltinProvider,
  getModelContextWindow,
  getToolOutputTokenLimit,
  listBuiltinModels,
  registerDynamicModelMetadata,
} from "../model-catalog.js";
import { THINKING_LEVELS } from "../types.js";
import { getAvailableThinkingLevels, getDefaultThinkingLevel, isThinkingOnlyModel, isThinkingToggleModel, normalizeInheritedThinkingLevel, normalizeThinkingLevel } from "../variant/variant-resolver.js";
import { getNextThinkingLevel, isThinkingLevel } from "../variant/thinking-level.js";

describe("variant resolver", () => {
  it("returns model-specific thinking levels", () => {
    expect(getAvailableThinkingLevels("openai-codex", "gpt-5.1-codex-mini")).toEqual(["off", "medium", "high"]);
    expect(getAvailableThinkingLevels("deepseek", "deepseek-v4-flash")).toEqual(["off", "low", "high", "max"]);
    expect(getAvailableThinkingLevels("deepseek", "deepseek-v4-flash-vision-exp")).toEqual(["off", "low", "high", "max"]);
    expect(getAvailableThinkingLevels("deepseek", "deepseek-v4-pro")).toEqual(["off", "low", "high", "max"]);
  });

  it("keeps DeepSeek Flash effort selectable with high as the default", () => {
    expect(getAvailableThinkingLevels("deepseek", "deepseek-flash")).toEqual(["off", "low", "high", "max"]);
    expect(getDefaultThinkingLevel("deepseek", "deepseek-flash")).toBe("high");
    expect(isThinkingOnlyModel("deepseek", "deepseek-flash")).toBe(false);
    expect(getModelContextWindow("deepseek", "deepseek-flash")).toBe(1048576);
  });

  it("uses one canonical effort order including ultra", () => {
    expect(THINKING_LEVELS).toEqual(["off", "minimal", "low", "medium", "high", "xhigh", "max", "ultra"]);
    expect(isThinkingLevel("ultra")).toBe(true);
  });

  it("defines the exact GPT-6 / GPT-5.6 fallback catalog", () => {
    expect(listBuiltinModels("openai-codex").slice(0, 4)).toEqual([
      expect.objectContaining({
        id: "gpt-6-astra",
        reasoningLevels: ["low", "medium", "high", "xhigh", "max", "ultra"],
        defaultReasoningLevel: "medium",
        contextWindow: 372000,
        useResponsesLite: true,
        toolOutputTokenLimit: 10000,
      }),
      expect.objectContaining({
        id: "gpt-5.6-sol",
        reasoningLevels: ["low", "medium", "high", "xhigh", "max", "ultra"],
        defaultReasoningLevel: "low",
        contextWindow: 372000,
        useResponsesLite: true,
        toolOutputTokenLimit: 10000,
      }),
      expect.objectContaining({
        id: "gpt-5.6-terra",
        reasoningLevels: ["low", "medium", "high", "xhigh", "max", "ultra"],
        defaultReasoningLevel: "medium",
        contextWindow: 372000,
        useResponsesLite: true,
        toolOutputTokenLimit: 10000,
      }),
      expect.objectContaining({
        id: "gpt-5.6-luna",
        reasoningLevels: ["low", "medium", "high", "xhigh", "max"],
        defaultReasoningLevel: "medium",
        contextWindow: 372000,
        useResponsesLite: true,
        toolOutputTokenLimit: 10000,
      }),
    ]);
    expect(getModelContextWindow("openai", "gpt-5.6-sol")).toBe(372000);
    expect(getToolOutputTokenLimit("openai", "gpt-5.6-luna")).toBe(10000);
  });

  it("uses the DeepSeek v4 documented context window", () => {
    expect(getModelContextWindow("deepseek", "deepseek-v4-flash")).toBe(1048576);
    expect(getModelContextWindow("deepseek", "deepseek-v4-flash-vision-exp")).toBe(1048576);
    expect(getModelContextWindow("deepseek", "deepseek-v4-pro")).toBe(1048576);
  });

  it("chooses medium as the default when supported", () => {
    expect(getDefaultThinkingLevel("openai-codex", "gpt-5.4")).toBe("medium");
    expect(getDefaultThinkingLevel("deepseek", "deepseek-v4-flash")).toBe("high");
    expect(getDefaultThinkingLevel("deepseek", "deepseek-v4-flash-vision-exp")).toBe("high");
    expect(getDefaultThinkingLevel("deepseek", "deepseek-v4-pro")).toBe("high");
  });

  it("clamps unsupported levels downward", () => {
    expect(normalizeThinkingLevel("xhigh", ["off", "medium", "high"])).toBe("high");
    expect(normalizeThinkingLevel("minimal", ["off", "low", "medium"])).toBe("off");
    expect(normalizeThinkingLevel("ultra", getAvailableThinkingLevels("openai", "gpt-5.6-luna"))).toBe("max");
    expect(normalizeThinkingLevel("ultra", getAvailableThinkingLevels("anthropic", "claude-opus-4-8"))).toBe("max");
  });

  it("uses model defaults for unsupported inherited effort", () => {
    expect(normalizeInheritedThinkingLevel("openai", "gpt-5.6-sol", "off")).toBe("low");
    expect(normalizeInheritedThinkingLevel("openai", "gpt-5.6-sol", "minimal")).toBe("low");
    expect(normalizeInheritedThinkingLevel("openai", "gpt-5.6-terra", "off")).toBe("medium");
    expect(normalizeInheritedThinkingLevel("openai", "gpt-5.6-luna", "ultra")).toBe("medium");
  });

  it("clears OpenAI aliases from dynamic metadata", () => {
    registerDynamicModelMetadata({
      id: "gpt-dynamic-test",
      name: "dynamic",
      providerId: "openai-codex",
      reasoningLevels: ["ultra"],
    });
    expect(getBuiltinModel("openai", "gpt-dynamic-test")?.reasoningLevels).toEqual(["ultra"]);
    clearDynamicModelMetadata("openai");
    expect(getBuiltinModel("openai", "gpt-dynamic-test")).toBeUndefined();
  });

  it("cycles through only supported levels", () => {
    expect(getNextThinkingLevel("medium", ["off", "medium", "high"])).toBe("high");
    expect(getNextThinkingLevel("high", ["off", "medium", "high"])).toBe("off");
  });

  it("identifies provider thinking toggles that are not real effort grades", () => {
    expect(isThinkingToggleModel("kimi-for-coding", "kimi-k2.6")).toBe(true);
    expect(isThinkingToggleModel("kimi-for-coding", "kimi-k2.5")).toBe(true);
    expect(isThinkingToggleModel("zhipuai", "glm-5.1")).toBe(false);
  });

  it("includes the GLM-5.3 family in the Zhipu Coding Plan catalog", () => {
    const models = listBuiltinModels("zhipuai-coding-plan");
    expect(models.find((model) => model.id === "glm-5.3-flash")).toMatchObject({
      id: "glm-5.3-flash",
      tier: "fast",
      contextWindow: 1_000_000,
      reasoningLevels: ["low", "high", "max"],
      defaultReasoningLevel: "max",
    });
    expect(models.find((model) => model.id === "glm-5.3")).toMatchObject({
      id: "glm-5.3",
      tier: "strong",
      contextWindow: 1_000_000,
      reasoningLevels: ["low", "high", "max"],
      defaultReasoningLevel: "max",
    });
  });

  it("aligns built-in Kimi models with the current API surface", () => {
    // The first four are the plan slots GET /coding/v1/models actually serves
    // (probed 2026-08-04); the kimi-k2.* ids are kept for older keys.
    expect(listBuiltinModels("kimi-for-coding").map((model) => model.id)).toEqual([
      "k3",
      "k3-256k",
      "k3.1-pro",
      "k3.1-pro-highspeed",
      "kimi-for-coding",
      "kimi-for-coding-highspeed",
      "kimi-k2.7-code",
      "kimi-k2.7-code-highspeed",
      "kimi-k2.6",
      "kimi-k2.5",
    ]);
    expect(getAvailableThinkingLevels("kimi-for-coding", "kimi-k2.7-code-highspeed")).toEqual(["medium"]);
    expect(isThinkingOnlyModel("kimi-for-coding", "kimi-k2.7-code-highspeed")).toBe(true);
    expect(getAvailableThinkingLevels("kimi-for-coding", "kimi-k2.6")).toEqual(["off", "medium"]);
    expect(getAvailableThinkingLevels("kimi-for-coding", "kimi-k2.5")).toEqual(["off", "medium"]);
  });

  it("exposes K3's real effort grades rather than a thinking toggle", () => {
    // K3 takes reasoning_effort low/high/max (no "medium") and can disable
    // thinking; the 256k variant has no off switch.
    expect(getAvailableThinkingLevels("kimi-for-coding", "k3")).toEqual(["off", "low", "high", "max"]);
    expect(getAvailableThinkingLevels("kimi-for-coding", "k3-256k")).toEqual(["low", "high", "max"]);
    expect(isThinkingToggleModel("kimi-for-coding", "k3")).toBe(false);
    expect(isThinkingOnlyModel("kimi-for-coding", "k3-256k")).toBe(false);
    expect(getAvailableThinkingLevels("moonshot-cn", "kimi-k3")).toEqual(["off", "low", "high", "max"]);
  });

  it("keeps K3.1 Pro on effort grades with no off switch", () => {
    // The endpoint reports reasoning.type "only" and 400s on thinking disabled.
    for (const [providerId, modelId] of [
      ["kimi-for-coding", "k3.1-pro"],
      ["kimi-for-coding", "k3.1-pro-highspeed"],
      ["moonshot-cn", "kimi-k3.1-pro"],
      ["moonshot-intl", "kimi-k3.1-pro-highspeed"],
    ] as const) {
      expect(getAvailableThinkingLevels(providerId, modelId)).toEqual(["low", "high", "max"]);
    }
  });

  it("includes Alibaba DashScope qwen and Kimi models", () => {
    expect(getBuiltinProvider("alibaba")).toMatchObject({
      baseURL: "https://dashscope.aliyuncs.com/compatible-mode/v1",
    });
    expect(listBuiltinModels("alibaba").map((model) => model.id)).toEqual([
      "qwen3.6-plus",
      "qwen3.7-max",
      "kimi-k3",
      "kimi-k2.7-code",
      "kimi-k2.6",
      "kimi-k2.5",
      "kimi/kimi-k3.1-pro",
      "kimi/kimi-k3.1-pro-highspeed",
      "kimi/kimi-k3",
      "kimi/kimi-k2.7-code",
      "kimi/kimi-k2.7-code-highspeed",
      "kimi/kimi-k2.6",
    ]);
    expect(getAvailableThinkingLevels("alibaba", "kimi-k3")).toEqual(["off", "low", "high", "max"]);
    expect(getAvailableThinkingLevels("alibaba", "kimi/kimi-k3.1-pro")).toEqual(["max"]);
    expect(getAvailableThinkingLevels("alibaba", "kimi/kimi-k3")).toEqual(["max"]);
    expect(isThinkingOnlyModel("alibaba", "kimi/kimi-k3.1-pro")).toBe(false);
    expect(isThinkingToggleModel("alibaba", "kimi-k2.6")).toBe(true);
    expect(isThinkingOnlyModel("alibaba", "kimi-k2.7-code")).toBe(true);
    expect(getAvailableThinkingLevels("alibaba", "qwen3.7-max")).toEqual(["off"]);
    expect(getModelContextWindow("alibaba", "qwen3.6-plus")).toBe(1048576);
    expect(getModelContextWindow("alibaba", "qwen3.7-max")).toBe(1048576);
  });

  it("includes StepFun Step Plan reasoning models", () => {
    expect(getBuiltinProvider("stepfun")).toMatchObject({
      baseURL: "https://api.stepfun.com/step_plan/v1",
    });
    expect(listBuiltinModels("stepfun").map((model) => model.id)).toEqual([
      "step-3.7-flash",
      "step-3.5-flash-2603",
      "step-3.5-flash",
      "step-router-v1",
    ]);
    expect(getAvailableThinkingLevels("stepfun", "step-3.7-flash")).toEqual(["off", "low", "medium", "high"]);
    expect(getDefaultThinkingLevel("stepfun", "step-3.7-flash")).toBe("medium");
    expect(getModelContextWindow("stepfun", "step-3.7-flash")).toBe(256000);
  });

  it("includes Doubao Seed models on Volcengine Ark", () => {
    expect(getBuiltinProvider("doubao")).toMatchObject({
      name: "Doubao (Volcengine Ark)",
      baseURL: "https://ark.cn-beijing.volces.com/api/v3",
      protocol: "ark-responses",
    });
    expect(listBuiltinModels("doubao").map((model) => model.id)).toEqual([
      "doubao-seed-2-1-pro-260628",
    ]);
    expect(getAvailableThinkingLevels("doubao", "doubao-seed-2-1-pro-260628")).toEqual([
      "minimal",
      "low",
      "medium",
      "high",
    ]);
    expect(getDefaultThinkingLevel("doubao", "doubao-seed-2-1-pro-260628")).toBe("high");
  });

  it("includes Muse and Space Bunny on OpenCode Zen", () => {
    expect(getBuiltinProvider("opencode-zen")).toMatchObject({
      name: "OpenCode Zen",
      baseURL: "https://opencode.ai/zen/v1",
      protocol: "openai-responses",
    });
    expect(listBuiltinModels("opencode-zen").map((model) => model.id)).toEqual([
      "muse-spark-1.3-contributor-free",
      "muse-spark-1.2",
      "muse-spark-1.2-contributor-free",
      "space-bunny-free",
    ]);
    expect(getAvailableThinkingLevels("opencode-zen", "muse-spark-1.3-contributor-free"))
      .toEqual(["minimal", "low", "medium", "high", "xhigh"]);
    expect(getDefaultThinkingLevel("opencode-zen", "muse-spark-1.3-contributor-free"))
      .toBe("high");
    expect(getModelContextWindow("opencode-zen", "muse-spark-1.2")).toBe(1048576);
    expect(getAvailableThinkingLevels("opencode-zen", "space-bunny-free"))
      .toEqual(["low", "medium", "high", "xhigh", "max"]);
    expect(getDefaultThinkingLevel("opencode-zen", "space-bunny-free")).toBe("high");
    expect(getModelContextWindow("opencode-zen", "space-bunny-free")).toBe(1048576);
  });

  it("includes MiniMax agent models", () => {
    expect(getBuiltinProvider("minimax")).toMatchObject({
      name: "MiniMax Token Plan",
      baseURL: "https://api.minimaxi.com/anthropic",
      protocol: "anthropic-messages",
    });
    expect(getBuiltinProvider("minimax-anthropic")).toMatchObject({
      name: "MiniMax API",
      baseURL: "https://api.minimaxi.com/anthropic",
      protocol: "anthropic-messages",
    });
    expect(listBuiltinModels("minimax").map((model) => model.id)).toEqual([
      "MiniMax-M3",
      "MiniMax-M2.7",
      "MiniMax-M2.7-highspeed",
    ]);
    expect(listBuiltinModels("minimax-anthropic").map((model) => model.id)).toEqual([
      "MiniMax-M3",
      "MiniMax-M2.7",
      "MiniMax-M2.7-highspeed",
    ]);
    expect(getAvailableThinkingLevels("minimax", "MiniMax-M3")).toEqual(["off", "medium"]);
    expect(getDefaultThinkingLevel("minimax", "MiniMax-M3")).toBe("medium");
    expect(getModelContextWindow("minimax", "MiniMax-M3")).toBe(1000000);
    expect(getModelContextWindow("minimax", "MiniMax-M2.7")).toBe(204800);
  });

  it("includes Anthropic Messages models", () => {
    expect(getBuiltinProvider("anthropic")).toMatchObject({
      baseURL: "https://api.anthropic.com",
      protocol: "anthropic-messages",
    });
    expect(listBuiltinModels("anthropic").map((model) => model.id)).toEqual([
      "claude-fable-5-1",
      "claude-opus-5-5",
      "claude-sonnet-5",
      "claude-opus-5",
      "claude-fable-5",
      "claude-opus-4-8",
      "claude-sonnet-4-6",
      "claude-haiku-4-5-20251001",
    ]);
    expect(listBuiltinModels("anthropic").some((model) => model.id === "claude-mythos-5")).toBe(false);
    expect(getAvailableThinkingLevels("anthropic", "claude-fable-5")).toEqual(["low", "medium", "high", "xhigh", "max"]);
    expect(getDefaultThinkingLevel("anthropic", "claude-fable-5")).toBe("high");
    expect(normalizeThinkingLevel("off", getAvailableThinkingLevels("anthropic", "claude-fable-5"))).toBe("low");
    expect(getModelContextWindow("anthropic", "claude-fable-5")).toBe(1000000);
    expect(getAvailableThinkingLevels("anthropic", "claude-opus-4-8")).toEqual(["off", "low", "medium", "high", "xhigh", "max"]);
    expect(getDefaultThinkingLevel("anthropic", "claude-opus-4-8")).toBe("high");
    expect(getModelContextWindow("anthropic", "claude-opus-4-8")).toBe(1000000);
    expect(getAvailableThinkingLevels("anthropic", "claude-sonnet-4-6")).toEqual(["off", "low", "medium", "high", "max"]);
    expect(getDefaultThinkingLevel("anthropic", "claude-sonnet-4-6")).toBe("high");
  });

  it("includes Gemini 4 Argon with Google's documented thinking contract", () => {
    expect(listBuiltinModels("google")[0]?.id).toBe("gemini-4-argon");
    expect(getBuiltinModel("google", "gemini-4-argon")).toMatchObject({ name: "Gemini 4 Argon", tier: "strong" });
    expect(getAvailableThinkingLevels("google", "gemini-4-argon")).toEqual(["low", "medium", "high"]);
    expect(getDefaultThinkingLevel("google", "gemini-4-argon")).toBe("medium");
    expect(normalizeThinkingLevel("off", getAvailableThinkingLevels("google", "gemini-4-argon"))).toBe("low");
    expect(getModelContextWindow("google", "gemini-4-argon")).toBe(1048576);
  });
});
