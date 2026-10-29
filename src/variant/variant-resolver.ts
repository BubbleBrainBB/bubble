import type { ThinkingLevel } from "../types.js";
import { getBuiltinModel, getModelDefaultReasoningLevel } from "../model-catalog.js";
import { clampThinkingLevel } from "./thinking-level.js";

const KIMI_TOGGLE_PROVIDER_IDS = new Set(["moonshot-cn", "moonshot-intl", "kimi-for-coding", "alibaba"]);
const KIMI_THINKING_TOGGLE_MODELS = new Set(["kimi-k2.5", "kimi-k2.6", "kimi/kimi-k2.6"]);

export function getAvailableThinkingLevels(providerId: string, modelId: string): ThinkingLevel[] {
  return getBuiltinModel(providerId, modelId)?.reasoningLevels ?? ["off"];
}

export function getDefaultThinkingLevel(providerId: string, modelId: string): ThinkingLevel {
  const levels = getAvailableThinkingLevels(providerId, modelId);
  const explicitDefault = getModelDefaultReasoningLevel(providerId, modelId);
  if (explicitDefault && levels.includes(explicitDefault)) return explicitDefault;
  return levels.includes("medium") ? "medium" : levels[0] || "off";
}

/**
 * A lone "medium" means the model is always in thinking mode with no
 * user-facing grades (e.g. kimi-k2.7-code) — the level value is an internal
 * placeholder and must not be shown to the user as an effort grade. Any other
 * lone level (e.g. DashScope's max-only Kimi K3) is a real fixed grade that
 * the provider sends verbatim, so it is shown.
 */
export function isThinkingOnlyLevels(levels: readonly ThinkingLevel[]): boolean {
  return levels.length === 1 && levels[0] === "medium";
}

export function isThinkingOnlyModel(providerId = "", modelId = ""): boolean {
  return isThinkingOnlyLevels(getAvailableThinkingLevels(providerId, modelId));
}

/**
 * Some providers expose thinking as a true enabled/disabled switch. We may store
 * the enabled state as "medium" internally for compatibility with ThinkingLevel,
 * but it is not a real user-facing effort grade.
 */
export function isThinkingToggleModel(providerId = "", modelId = ""): boolean {
  const normalizedProvider = providerId.toLowerCase();
  const normalizedModel = modelId.toLowerCase();
  if (normalizedProvider === "mimo-token-plan") return true;
  if (normalizedProvider.includes("minimax") || normalizedModel.includes("minimax")) return true;
  return KIMI_TOGGLE_PROVIDER_IDS.has(normalizedProvider)
    && KIMI_THINKING_TOGGLE_MODELS.has(normalizedModel);
}

export function normalizeThinkingLevel(
  level: ThinkingLevel,
  supportedLevels: readonly ThinkingLevel[],
): ThinkingLevel {
  return clampThinkingLevel(level, supportedLevels);
}

/**
 * Normalizes effort inherited from a previous model or restored session.
 *
 * Unlike an explicit CLI/config choice, an inherited unsupported value does
 * not express a direction the user just chose for the target model. Prefer the
 * target model's declared default in that case, then fall back to the canonical
 * downward clamp for legacy models without an explicit default.
 */
export function normalizeInheritedThinkingLevel(
  providerId: string,
  modelId: string,
  inheritedLevel: ThinkingLevel,
): ThinkingLevel {
  const supportedLevels = getAvailableThinkingLevels(providerId, modelId);
  if (supportedLevels.includes(inheritedLevel)) return inheritedLevel;

  const modelDefault = getModelDefaultReasoningLevel(providerId, modelId);
  if (modelDefault && supportedLevels.includes(modelDefault)) return modelDefault;

  return clampThinkingLevel(inheritedLevel, supportedLevels);
}
