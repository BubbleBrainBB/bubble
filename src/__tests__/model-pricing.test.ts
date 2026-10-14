import { describe, expect, it } from "vitest";
import { calculateUsageCost, getModelPricing } from "../model-pricing.js";

describe("model pricing", () => {
  it("prices Space Bunny Free input, output and cache tokens at zero", () => {
    expect(calculateUsageCost("opencode-zen", "space-bunny-free", {
      promptTokens: 1000, completionTokens: 200,
      promptCacheHitTokens: 500, promptCacheMissTokens: 500, cacheCreationTokens: 100,
    })).toEqual({ currency: "USD", cost: 0, estimated: false });
  });
  it("contains OpenCode Zen Muse Spark pricing", () => {
    expect(getModelPricing("opencode-zen", "muse-spark-1.2")).toMatchObject({
      currency: "USD",
      inputCacheHitPerMillion: 0.15,
      inputCacheMissPerMillion: 1.25,
      outputPerMillion: 4.25,
    });
    expect(calculateUsageCost("opencode-zen", "muse-spark-1.3-contributor-free", {
      promptTokens: 1_000_000,
      completionTokens: 1_000_000,
    })).toEqual({ currency: "USD", cost: 0, estimated: true });
  });

  it("contains Claude Fable 5 USD pricing", () => {
    // Anthropic bills a cache read at 0.1x base input and a 5-minute cache write
    // at 1.25x. The hit rate here used to be pinned at the full 10/M base price,
    // which overstated every cached Fable run tenfold.
    expect(getModelPricing("anthropic", "claude-fable-5")).toMatchObject({
      currency: "USD",
      inputCacheHitPerMillion: 1,
      inputCacheMissPerMillion: 10,
      inputCacheWritePerMillion: 12.5,
      outputPerMillion: 50,
    });
  });

  it("contains Claude Opus 4.8 pricing, which was missing entirely", () => {
    // Without an entry, calculateUsageCost returned undefined and Opus runs
    // displayed as unpriced — a $100+ benchmark run showed no cost at all.
    expect(getModelPricing("anthropic", "claude-opus-4-8")).toMatchObject({
      currency: "USD",
      inputCacheHitPerMillion: 0.5,
      inputCacheMissPerMillion: 5,
      inputCacheWritePerMillion: 6.25,
      outputPerMillion: 25,
    });
  });

  it("contains the September 10 Flash prices for its canonical and legacy names", () => {
    for (const modelId of ["deepseek-flash", "deepseek-v4-flash", "deepseek-v4-flash-vision-exp"]) {
      expect(getModelPricing("deepseek", modelId)).toMatchObject({
        currency: "USD",
        inputCacheHitPerMillion: 0.006,
        inputCacheMissPerMillion: 0.3,
        outputPerMillion: 1.2,
        peakWeekdaysUtc: [1, 2, 3, 4, 5],
        effectiveFrom: "2026-09-10T04:00:00Z",
        offPeak: {
          inputCacheHitPerMillion: 0.003,
          inputCacheMissPerMillion: 0.15,
          outputPerMillion: 0.6,
        },
      });
    }
  });

  it("keeps Pro prices until its September 14 rerouting", () => {
    expect(getModelPricing("deepseek", "deepseek-v4-pro")).toMatchObject({
      inputCacheHitPerMillion: 0.044,
      inputCacheMissPerMillion: 1.32,
      outputPerMillion: 3.96,
      offPeak: {
        inputCacheHitPerMillion: 0.022,
        inputCacheMissPerMillion: 0.66,
        outputPerMillion: 1.98,
      },
    });
  });

  it.each([
    ["2026-09-11T00:59:59Z", false],
    ["2026-09-11T01:00:00Z", true],
    ["2026-09-11T03:59:59Z", true],
    ["2026-09-11T04:00:00Z", false],
    ["2026-09-11T05:59:59Z", false],
    ["2026-09-11T06:00:00Z", true],
    ["2026-09-11T09:59:59Z", true],
    ["2026-09-11T10:00:00Z", false],
    ["2026-09-12T02:00:00Z", false],
    ["2026-09-13T07:00:00Z", false],
    ["2026-09-14T01:00:00Z", true],
  ])("prices Flash cache hits, misses, and output at %s", (timestamp, peak) => {
    for (const modelId of ["deepseek-flash", "deepseek-v4-flash", "deepseek-v4-flash-vision-exp"]) {
      const result = calculateUsageCost("deepseek", modelId, {
        promptTokens: 1_000_000,
        promptCacheHitTokens: 250_000,
        promptCacheMissTokens: 750_000,
        completionTokens: 500_000,
      }, new Date(timestamp));
      expect(result?.currency).toBe("USD");
      expect(result?.estimated).toBe(false);
      expect(result?.cost).toBeCloseTo((0.25 * 0.006 + 0.75 * 0.3 + 0.5 * 1.2) * (peak ? 1 : 0.5));
    }
  });

  it.each(["deepseek-v4-flash", "deepseek-v4-flash-vision-exp"])(
    "preserves %s history across the exact September 10 tariff boundary",
    (modelId) => {
      const usage = { promptTokens: 1_000_000, completionTokens: 1_000_000 };
      expect(calculateUsageCost("deepseek", modelId, usage, new Date("2026-09-10T03:59:59Z"))?.cost)
        .toBeCloseTo(0.44 + 1.32);
      expect(calculateUsageCost("deepseek", modelId, usage, new Date("2026-09-10T04:00:00Z"))?.cost)
        .toBeCloseTo(0.15 + 0.6);
    },
  );

  it("uses the full Flash schedule after the exact Pro rerouting boundary", () => {
    const usage = { promptTokens: 1_000_000, completionTokens: 1_000_000 };
    for (const [timestamp, expected] of [
      ["2026-09-14T03:59:59Z", 1.32 + 3.96],
      ["2026-09-14T04:00:00Z", 0.15 + 0.6],
      ["2026-09-14T06:00:00Z", 0.3 + 1.2],
      ["2026-09-19T06:00:00Z", 0.15 + 0.6],
    ] as const) {
      expect(calculateUsageCost("deepseek", "deepseek-v4-pro", usage, new Date(timestamp))?.cost)
        .toBeCloseTo(expected);
    }
  });

  it("contains StepFun step-3.7-flash CNY pricing", () => {
    expect(getModelPricing("stepfun", "step-3.7-flash")).toMatchObject({
      currency: "CNY",
      inputCacheHitPerMillion: 0.27,
      inputCacheMissPerMillion: 1.35,
      outputPerMillion: 8.1,
    });
  });

  it("prices Gemini 3.7/3.8 Flash across the launch-promo boundary", () => {
    for (const modelId of ["gemini-3.7-flash", "gemini-3.8-flash"]) {
      const usage = {
        promptTokens: 1_000_000,
        promptCacheHitTokens: 250_000,
        promptCacheMissTokens: 750_000,
        completionTokens: 1_000_000,
      };
      expect(calculateUsageCost("google", modelId, usage, new Date("2026-12-31T23:59:59Z")))
        .toEqual({ currency: "USD", cost: 0.25 * 0.075 + 0.75 * 0.75 + 3.75, estimated: false });
      expect(calculateUsageCost("google", modelId, usage, new Date("2027-01-01T00:00:00Z")))
        .toEqual({ currency: "USD", cost: 0.25 * 0.15 + 0.75 * 1.5 + 7.5, estimated: false });
    }
  });

  it("prices Gemini 4 Argon across the introductory-price boundary", () => {
    const usage = {
      promptTokens: 1_000_000,
      promptCacheHitTokens: 250_000,
      promptCacheMissTokens: 750_000,
      completionTokens: 1_000_000,
    };
    expect(calculateUsageCost("google", "gemini-4-argon", usage, new Date("2026-11-30T23:59:59Z")))
      .toEqual({ currency: "USD", cost: 0.25 * 0.1 + 0.75 * 2 + 10, estimated: false });
    expect(calculateUsageCost("google", "gemini-4-argon", usage, new Date("2026-12-01T00:00:00Z")))
      .toEqual({ currency: "USD", cost: 0.25 * 0.2 + 0.75 * 4 + 20, estimated: false });
  });

  it("calculates DeepSeek cache-aware cost", () => {
    const result = calculateUsageCost("deepseek", "deepseek-v4-pro", {
      promptTokens: 1_000_000,
      promptCacheHitTokens: 250_000,
      promptCacheMissTokens: 750_000,
      completionTokens: 500_000,
    }, new Date("2026-08-21T02:00:00Z"));

    expect(result).toEqual({
      currency: "USD",
      cost: 0.25 * 0.044 + 0.75 * 1.32 + 0.5 * 3.96,
      estimated: false,
    });
  });

  it("uses the DeepSeek off-peak tariff outside the UTC peak windows", () => {
    const result = calculateUsageCost("deepseek", "deepseek-v4-flash", {
      promptTokens: 1_000_000,
      completionTokens: 1_000_000,
    }, new Date("2026-08-21T05:00:00Z"));
    expect(result?.currency).toBe("USD");
    expect(result?.estimated).toBe(true);
    expect(result?.cost).toBeCloseTo(0.88);
  });

  it("treats August weekends inside the peak windows as off-peak", () => {
    // 2026-08-22 is a Saturday, 02:00 UTC sits inside the 01:00-04:00 window.
    // The August 16 schedule was already weekday-only, so this is off-peak.
    const result = calculateUsageCost("deepseek", "deepseek-v4-flash", {
      promptTokens: 1_000_000,
      completionTokens: 1_000_000,
    }, new Date("2026-08-22T02:00:00Z"));
    expect(result?.cost).toBeCloseTo(0.88);
  });

  it("does not reprice DeepSeek sessions from before the tariff change", () => {
    const result = calculateUsageCost("deepseek", "deepseek-v4-flash", {
      promptTokens: 1_000_000,
      completionTokens: 1_000_000,
    }, new Date("2026-08-16T15:59:59Z"));

    expect(result?.cost).toBeCloseTo(0.14 + 0.28);
  });

  it("calculates StepFun cache-aware CNY cost", () => {
    const result = calculateUsageCost("stepfun", "step-3.7-flash", {
      promptTokens: 1_000_000,
      promptCacheHitTokens: 250_000,
      promptCacheMissTokens: 750_000,
      completionTokens: 500_000,
    });

    expect(result).toEqual({
      currency: "CNY",
      cost: 0.25 * 0.27 + 0.75 * 1.35 + 0.5 * 8.1,
      estimated: false,
    });
  });
  it("prices Anthropic cache writes at the write rate, not the miss rate", () => {
    // promptCacheMissTokens already CONTAINS cacheCreationTokens, so a naive
    // "+ creation * writeRate" on top of the untouched miss term would bill the
    // written tokens at miss + write (2.25x base) instead of 1.25x.
    const result = calculateUsageCost("anthropic", "claude-opus-4-8", {
      promptTokens: 1_000_000,
      promptCacheHitTokens: 700_000,
      promptCacheMissTokens: 300_000,
      cacheCreationTokens: 250_000,
      completionTokens: 100_000,
    });

    expect(result).toEqual({
      currency: "USD",
      // 0.7M read @0.5 + 0.25M write @6.25 + 0.05M uncached @5 + 0.1M out @25
      cost: 0.7 * 0.5 + 0.25 * 6.25 + 0.05 * 5 + 0.1 * 25,
      estimated: false,
    });
  });

  it("falls back to the miss rate when a provider has no distinct write price", () => {
    const result = calculateUsageCost("deepseek", "deepseek-v4-flash", {
      promptTokens: 1_000_000,
      promptCacheHitTokens: 400_000,
      promptCacheMissTokens: 600_000,
      cacheCreationTokens: 200_000,
      completionTokens: 0,
    }, new Date("2026-08-21T05:00:00Z"));

    expect(result?.cost).toBeCloseTo(0.4 * 0.007 + 0.6 * 0.22);
  });

  it("prices a cached Opus run far below an uncached one", () => {
    const uncached = calculateUsageCost("anthropic", "claude-opus-4-8", {
      promptTokens: 1_000_000,
      completionTokens: 0,
    });
    const cached = calculateUsageCost("anthropic", "claude-opus-4-8", {
      promptTokens: 1_000_000,
      promptCacheHitTokens: 900_000,
      promptCacheMissTokens: 100_000,
      cacheCreationTokens: 90_000,
      completionTokens: 0,
    });

    expect(uncached!.cost).toBeCloseTo(5);
    expect(cached!.cost).toBeLessThan(uncached!.cost / 3);
  });

  it("uses current official Sonnet 5 cache-write pricing in September", () => {
    const result = calculateUsageCost("anthropic", "claude-sonnet-5", {
      promptTokens: 1_000_000,
      promptCacheMissTokens: 1_000_000,
      cacheCreationTokens: 1_000_000,
      completionTokens: 0,
    }, new Date("2026-09-01T00:00:00Z"));

    expect(result).toEqual({ currency: "USD", cost: 2.5, estimated: false });
  });
});
