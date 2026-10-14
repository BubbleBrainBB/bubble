import { GENERATED_MODEL_DATA, type GeneratedModelData } from "./model-data.generated.js";
import type { TokenUsage, UsageCost } from "./types.js";

export type { UsageCost } from "./types.js";

export type PricingCurrency = UsageCost["currency"];

export interface ModelPricingRate {
  inputCacheHitPerMillion: number;
  inputCacheMissPerMillion: number;
  /**
   * Rate for tokens written INTO the cache, when the provider charges a premium
   * for it (Anthropic: 1.25x base input for the 5-minute TTL). Optional —
   * without it, cache writes are billed at the plain miss rate, which is what
   * every provider modelled here did before Anthropic caching was enabled.
   */
  inputCacheWritePerMillion?: number;
  outputPerMillion: number;
}

export interface UtcPricingWindow {
  /** Inclusive minute since 00:00 UTC. */
  startMinute: number;
  /** Exclusive minute since 00:00 UTC. */
  endMinute: number;
}

export interface ModelPricingTariff extends ModelPricingRate {
  /**
   * Optional recurring UTC time-of-use tariff. Top-level rates are the peak
   * tariff; outside these windows, `offPeak` applies. DeepSeek introduced this
   * pricing shape for the V4 family on 2026-08-16.
   */
  peakWindowsUtc?: UtcPricingWindow[];
  /** UTC weekdays with peak windows (0 = Sunday). Omitted means every day. */
  peakWeekdaysUtc?: number[];
  offPeak?: ModelPricingRate;
  /** When the recurring tariff became effective. */
  effectiveFrom?: string;
  /** Rate used before `effectiveFrom` (for historical session statistics). */
  prior?: ModelPricingTariff;
  effectiveUntil?: string;
  original?: ModelPricingTariff;
}

export interface ModelPricing extends ModelPricingTariff {
  providerId: string;
  modelId: string;
  currency: PricingCurrency;
}

// Official USD prices checked 2026-09-10:
// https://api-docs.deepseek.com/quick_start/pricing
// Exact transition times: https://api-docs.deepseek.com/news/news260910
const DEEPSEEK_PEAK_WINDOWS_UTC: UtcPricingWindow[] = [
  { startMinute: 60, endMinute: 240 },
  { startMinute: 360, endMinute: 600 },
];
const DEEPSEEK_PEAK_WEEKDAYS_UTC = [1, 2, 3, 4, 5];
const DEEPSEEK_V41_FLASH_TARIFF: ModelPricingTariff = {
  inputCacheHitPerMillion: 0.006,
  inputCacheMissPerMillion: 0.3,
  outputPerMillion: 1.2,
  peakWindowsUtc: DEEPSEEK_PEAK_WINDOWS_UTC,
  peakWeekdaysUtc: DEEPSEEK_PEAK_WEEKDAYS_UTC,
  offPeak: {
    inputCacheHitPerMillion: 0.003,
    inputCacheMissPerMillion: 0.15,
    outputPerMillion: 0.6,
  },
};

/**
 * Hand-maintained corrections layered over the generated models.dev snapshot.
 * An entry here shadows the generated entry with the same (providerId, modelId).
 * Only exceptions belong here — facts the upstream catalog cannot carry
 * (promo expiry dates, non-USD billing) or gets wrong. Everything else comes
 * from src/model-data.generated.ts via `npm run refresh-models`.
 */
export const MODEL_PRICING_OVERRIDES: ModelPricing[] = [
  ...[
    "muse-spark-1.3-contributor-free",
    "muse-spark-1.2-contributor-free",
    "space-bunny-free",
  ].map((modelId): ModelPricing => ({
    providerId: "opencode-zen",
    modelId,
    currency: "USD",
    inputCacheHitPerMillion: 0,
    inputCacheMissPerMillion: 0,
    outputPerMillion: 0,
  })),
  {
    providerId: "opencode-zen",
    modelId: "muse-spark-1.2",
    currency: "USD",
    inputCacheHitPerMillion: 0.15,
    inputCacheMissPerMillion: 1.25,
    outputPerMillion: 4.25,
  },
  // Gemini 3.7/3.8 Flash launch pricing. Google's standard paid-tier prices
  // double on 2027-01-01; keep both rates so historical and future /stats are
  // priced against the tariff that was actually active.
  ...["gemini-3.7-flash", "gemini-3.8-flash"].map((modelId): ModelPricing => ({
    providerId: "google",
    modelId,
    currency: "USD",
    inputCacheHitPerMillion: 0.075,
    inputCacheMissPerMillion: 0.75,
    outputPerMillion: 3.75,
    effectiveUntil: "2027-01-01T00:00:00Z",
    original: {
      inputCacheHitPerMillion: 0.15,
      inputCacheMissPerMillion: 1.5,
      outputPerMillion: 7.5,
    },
  })),
  // Gemini 4 Argon introductory pricing through 2026-11-30; standard rates from
  // 2026-12-01. Implicit caching only (95% off input), so no cache-write rate.
  // https://ai.google.dev/gemini-api/docs/pricing
  {
    providerId: "google",
    modelId: "gemini-4-argon",
    currency: "USD",
    inputCacheHitPerMillion: 0.1,
    inputCacheMissPerMillion: 2,
    outputPerMillion: 10,
    effectiveUntil: "2026-12-01T00:00:00Z",
    original: {
      inputCacheHitPerMillion: 0.2,
      inputCacheMissPerMillion: 4,
      outputPerMillion: 20,
    },
  },
  {
    providerId: "deepseek",
    modelId: "deepseek-flash",
    currency: "USD",
    ...DEEPSEEK_V41_FLASH_TARIFF,
    effectiveFrom: "2026-09-10T04:00:00Z",
  },
  // Preserve older tariffs for historical /stats. Both legacy Flash IDs now
  // serve V4.1 Flash; Pro switches on 2026-09-14 at 04:00 UTC. A transition
  // must select the whole tariff, including its peak/off-peak schedule.
  ...[
    { modelId: "deepseek-v4-flash", kind: "flash" },
    { modelId: "deepseek-v4-flash-vision-exp", kind: "flash" },
    { modelId: "deepseek-v4-pro", kind: "pro" },
  ].map(({ modelId, kind }): ModelPricing => {
    const peak = kind === "pro"
      ? { inputCacheHitPerMillion: 0.044, inputCacheMissPerMillion: 1.32, outputPerMillion: 3.96 }
      : { inputCacheHitPerMillion: 0.014, inputCacheMissPerMillion: 0.44, outputPerMillion: 1.32 };
    const offPeak = kind === "pro"
      ? { inputCacheHitPerMillion: 0.022, inputCacheMissPerMillion: 0.66, outputPerMillion: 1.98 }
      : { inputCacheHitPerMillion: 0.007, inputCacheMissPerMillion: 0.22, outputPerMillion: 0.66 };
    const prior = kind === "pro"
      ? { inputCacheHitPerMillion: 0.003625, inputCacheMissPerMillion: 0.435, outputPerMillion: 0.87 }
      : { inputCacheHitPerMillion: 0.0028, inputCacheMissPerMillion: 0.14, outputPerMillion: 0.28 };
    // The 2026-08-16 schedule was already Monday-Friday only; the earlier
    // every-day model here was an omission, so applying the weekday rule to
    // this tariff corrects August weekend history rather than repricing it.
    const previous: ModelPricingTariff = {
      ...peak,
      peakWindowsUtc: DEEPSEEK_PEAK_WINDOWS_UTC,
      peakWeekdaysUtc: DEEPSEEK_PEAK_WEEKDAYS_UTC,
      offPeak,
      effectiveFrom: "2026-08-16T16:00:00Z",
      // The vision model launched after the tariff change; it has no valid
      // pre-change usage. Stable Flash/Pro retain their former rates so /stats
      // does not reprice old sessions using today's schedule.
      ...(modelId === "deepseek-v4-flash-vision-exp" ? {} : { prior }),
    };
    return {
      providerId: "deepseek",
      modelId,
      currency: "USD",
      ...(kind === "flash" ? {
        ...DEEPSEEK_V41_FLASH_TARIFF,
        effectiveFrom: "2026-09-10T04:00:00Z",
        prior: previous,
      } : {
        ...previous,
        effectiveUntil: "2026-09-14T04:00:00Z",
        original: DEEPSEEK_V41_FLASH_TARIFF,
      }),
    };
  }),
  // Official current prices, verified 2026-09-23:
  // https://platform.claude.com/docs/en/about-claude/pricing
  // Cache reads are 2.5% (Fable 5.1) / 5% (Opus 5.5), not the older 10%.
  {
    providerId: "anthropic",
    modelId: "claude-fable-5-1",
    currency: "USD",
    inputCacheHitPerMillion: 0.25,
    inputCacheMissPerMillion: 10,
    inputCacheWritePerMillion: 12.5,
    outputPerMillion: 50,
  },
  {
    providerId: "anthropic",
    modelId: "claude-opus-5-5",
    currency: "USD",
    inputCacheHitPerMillion: 0.2,
    inputCacheMissPerMillion: 4,
    inputCacheWritePerMillion: 5,
    outputPerMillion: 20,
  },
  // The current official Sonnet 5 price is $2/$10 without an expiry.
  {
    providerId: "anthropic",
    modelId: "claude-sonnet-5",
    currency: "USD",
    inputCacheHitPerMillion: 0.2,
    inputCacheMissPerMillion: 2,
    inputCacheWritePerMillion: 2.5,
    outputPerMillion: 10,
  },
  // The StepFun step plan bills in CNY; models.dev normalizes prices to USD.
  {
    providerId: "stepfun",
    modelId: "step-3.7-flash",
    currency: "CNY",
    inputCacheHitPerMillion: 0.27,
    inputCacheMissPerMillion: 1.35,
    outputPerMillion: 8.1,
  },
];

function pricingFromGenerated(entry: GeneratedModelData): ModelPricing {
  return {
    providerId: entry.providerId,
    modelId: entry.modelId,
    currency: "USD",
    // Without a cache discount upstream, cached input bills at the plain rate.
    inputCacheHitPerMillion: entry.cacheReadPerMillion ?? entry.inputPerMillion,
    inputCacheMissPerMillion: entry.inputPerMillion,
    ...(entry.cacheWritePerMillion !== undefined
      ? { inputCacheWritePerMillion: entry.cacheWritePerMillion }
      : {}),
    outputPerMillion: entry.outputPerMillion,
  };
}

const overriddenKeys = new Set(
  MODEL_PRICING_OVERRIDES.map((item) => `${item.providerId}\0${item.modelId}`),
);

export const MODEL_PRICING: ModelPricing[] = [
  ...GENERATED_MODEL_DATA
    .filter((entry) => !overriddenKeys.has(`${entry.providerId}\0${entry.modelId}`))
    .map(pricingFromGenerated),
  ...MODEL_PRICING_OVERRIDES,
];

export function getModelPricing(providerId: string, modelId: string): ModelPricing | undefined {
  return MODEL_PRICING.find((item) => item.providerId === providerId && item.modelId === modelId);
}

function pricingRateAt(pricing: ModelPricingTariff, at: Date): ModelPricingRate {
  if (pricing.prior && pricing.effectiveFrom && at.getTime() < Date.parse(pricing.effectiveFrom)) {
    return pricingRateAt(pricing.prior, at);
  }
  if (pricing.original && pricing.effectiveUntil && at.getTime() >= Date.parse(pricing.effectiveUntil)) {
    return pricingRateAt(pricing.original, at);
  }
  if (!pricing.offPeak || !pricing.peakWindowsUtc?.length) return pricing;
  const minute = at.getUTCHours() * 60 + at.getUTCMinutes();
  const isPeakDay = !pricing.peakWeekdaysUtc || pricing.peakWeekdaysUtc.includes(at.getUTCDay());
  const isPeak = isPeakDay && pricing.peakWindowsUtc.some((window) =>
    minute >= window.startMinute && minute < window.endMinute);
  return isPeak ? pricing : pricing.offPeak;
}

export function calculateUsageCost(
  providerId: string,
  modelId: string,
  usage: TokenUsage,
  at: Date = new Date(),
): UsageCost | undefined {
  const pricing = getModelPricing(providerId, modelId);
  if (!pricing) return undefined;
  const rate = pricingRateAt(pricing, at);

  const hasCacheBreakdown =
    typeof usage.promptCacheHitTokens === "number"
    || typeof usage.promptCacheMissTokens === "number"
    || typeof usage.cacheCreationTokens === "number";
  const hit = usage.promptCacheHitTokens ?? 0;
  const miss = hasCacheBreakdown
    ? usage.promptCacheMissTokens ?? Math.max(0, usage.promptTokens - hit)
    : usage.promptTokens;
  // `miss` already contains the cache-write tokens (see mergeAnthropicUsage),
  // so writes must be netted out before being re-priced — adding a write term
  // on top of the untouched miss term would bill them at miss + write.
  const write = rate.inputCacheWritePerMillion !== undefined
    ? Math.min(usage.cacheCreationTokens ?? 0, miss)
    : 0;
  const missOnly = miss - write;
  const cost =
    (hit / 1_000_000) * rate.inputCacheHitPerMillion
    + (write / 1_000_000) * (rate.inputCacheWritePerMillion ?? rate.inputCacheMissPerMillion)
    + (missOnly / 1_000_000) * rate.inputCacheMissPerMillion
    + (usage.completionTokens / 1_000_000) * rate.outputPerMillion;

  return {
    currency: pricing.currency,
    cost,
    estimated: !hasCacheBreakdown,
  };
}
