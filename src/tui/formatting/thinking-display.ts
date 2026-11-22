import { decodeModel } from "../../provider-registry.js";
import { isThinkingOnlyModel, isThinkingToggleModel } from "../../provider-transform.js";

/**
 * How a model's thinking state is worded for the user.
 *
 * - "effort": the level is a real grade the provider receives (or "off" on a
 *   model that has grades), so it is shown as is.
 * - "always": the model thinks and cannot be told not to; it has no grades.
 * - "toggle": thinking is an on/off switch with no grades.
 *
 * The last two store "medium" internally as a stand-in for "on". That value is
 * never sent as an effort grade and must not be shown as one.
 */
export type ThinkingDisplay =
  | { kind: "effort"; level: string }
  | { kind: "always" }
  | { kind: "toggle"; on: boolean };

/**
 * `model` may be the bare id ("nex-n2.6") or the stored "provider:id" form
 * ("moss:nex-n2.6"), which is what Agent.model holds. The catalog is keyed by
 * the bare id, so the prefix comes off before the lookup. A colon inside a
 * bare id ("llama3:8b") is left alone when it does not name the provider.
 */
export function describeThinking(provider: string | undefined, model: string, level: string | undefined): ThinkingDisplay {
  const decoded = decodeModel(model);
  const prefixed = decoded.providerId !== undefined && (!provider || decoded.providerId === provider);
  const providerId = provider || (prefixed ? decoded.providerId : undefined) || "";
  const modelId = prefixed ? decoded.modelId : model;
  if (isThinkingOnlyModel(providerId, modelId)) return { kind: "always" };
  if (isThinkingToggleModel(providerId, modelId)) return { kind: "toggle", on: !!level && level !== "off" };
  return { kind: "effort", level: level || "off" };
}
