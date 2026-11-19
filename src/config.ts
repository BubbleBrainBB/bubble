/**
 * User-level configuration manager.
 *
 * Uses a single JSON file in Bubble home, normally ~/.bubble/config.json.
 */

import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { getBubbleHome } from "./bubble-home.js";
import { sanitizeAgentCategories, type AgentCategoriesConfig } from "./agent/categories.js";
import { sanitizeAgentRouting, type AgentRoutingConfig } from "./agent/routing-catalog.js";
import type { ProviderProfile } from "./provider-registry.js";
import type { ThinkingLevel } from "./types.js";
import { isThinkingLevel } from "./variant/thinking-level.js";
import { isProviderModelAllowed } from "./provider-model-policy.js";

// openai-codex is an internal transport profile surfaced through the visible
// OpenAI OAuth provider. OpenRouter is a regular user-configurable provider.
const HIDDEN_PROVIDER_IDS = new Set(["openai-codex"]);

function getConfigPath(): string {
  return join(getBubbleHome(), "config.json");
}

function isHiddenProviderId(providerId?: string): boolean {
  return !!providerId && HIDDEN_PROVIDER_IDS.has(providerId);
}

function modelProviderId(model: string): string | undefined {
  if (!model.includes(":")) return undefined;
  return model.split(":", 1)[0];
}

function isAllowedConfiguredModel(model: string): boolean {
  if (!model.includes(":")) return true;
  const [providerId, ...modelParts] = model.split(":");
  return isProviderModelAllowed(providerId, modelParts.join(":"));
}

function sanitizeRecentModels(models?: string[]): string[] | undefined {
  if (!models) return undefined;
  return models.filter((model) =>
    !isHiddenProviderId(modelProviderId(model)) && isAllowedConfiguredModel(model));
}

function sanitizeProviders(providers?: ProviderProfile[]): ProviderProfile[] | undefined {
  if (!providers) return undefined;
  return providers.filter((provider) => !isHiddenProviderId(provider.id));
}

function sanitizeDefaultModel(model?: string): string | undefined {
  if (!model) return undefined;
  return isHiddenProviderId(modelProviderId(model)) || !isAllowedConfiguredModel(model)
    ? undefined
    : model;
}

/** JSON can hold null or a level a later version dropped; neither is a level. */
function sanitizeThinkingLevel(level: unknown): ThinkingLevel | undefined {
  return typeof level === "string" && isThinkingLevel(level) ? level : undefined;
}

function sanitizeDefaultProvider(providerId?: string): string | undefined {
  return isHiddenProviderId(providerId) ? undefined : providerId;
}

export type ThemeMode = "auto" | "light" | "dark";

export interface ThemeConfig {
  mode: ThemeMode;
  overrides?: Record<string, string>;
  explicit?: boolean;
}

export interface UserConfigData {
  defaultModel?: string;
  defaultThinkingLevel?: ThinkingLevel;
  skillPaths?: string[];
  /** Skill names disabled from invocation/search until re-enabled in /skills. */
  skills?: { disabled?: string[]; catalogChars?: number };
  /**
   * Three shapes are accepted on disk so we can evolve without breaking
   * existing configs:
   *   - `"auto" | "light" | "dark"` — mode only
   *   - `{ mode, overrides? }` — mode + optional per-key palette overrides
   *   - `Record<string, string>` (legacy) — treated as `{ mode: "dark", overrides }`
   *     so users who customized colors before light-mode existed keep their
   *     palette and stay on dark, which was the only palette at the time.
   */
  theme?: ThemeMode | ThemeConfig | Record<string, string>;
  recentModels?: string[];
  apiKey?: string;
  providers?: ProviderProfile[];
  defaultProvider?: string;
  agentCategories?: AgentCategoriesConfig;
  /** Subagent model-routing knobs: autoTier (default true), allowCrossProvider (default true). */
  agentRouting?: Partial<AgentRoutingConfig>;
  subagents?: SubagentsUserConfig;
  /** Background tasks: completion wakes and Grok-style foreground demotion. */
  tasks?: {
    /** Auto-resume the Agent when an owned task finishes. Defaults ON. */
    autoResume?: boolean;
    /** Move long foreground Execute calls into Tasks automatically. Defaults ON. */
    autoBackground?: boolean;
    /** Foreground wait budget before automatic demotion. Defaults to 15 seconds. */
    foregroundWaitMs?: number;
  };
}

export interface SubagentsUserConfig {
  /** Global cap on concurrently running children. Default 8. */
  maxActiveSubagents?: number;
}

function sanitizeSubagentsConfig(value: unknown): SubagentsUserConfig | undefined {
  if (!value || typeof value !== "object" || Array.isArray(value)) return undefined;
  const raw = value as Record<string, unknown>;
  const out: SubagentsUserConfig = {};
  if (typeof raw.maxActiveSubagents === "number" && Number.isFinite(raw.maxActiveSubagents)) {
    out.maxActiveSubagents = Math.max(1, Math.floor(raw.maxActiveSubagents));
  }
  return Object.keys(out).length > 0 ? out : undefined;
}

function sanitizeTheme(
  value: UserConfigData["theme"],
): ThemeConfig | undefined {
  if (value == null) return undefined;
  if (typeof value === "string") {
    return value === "auto" || value === "light" || value === "dark"
      ? { mode: value, explicit: true }
      : undefined;
  }
  if (typeof value !== "object" || Array.isArray(value)) return undefined;
  // Discriminate the new `{ mode, overrides }` shape from the legacy
  // `Record<string, string>` shape. A legacy config has no `mode` key.
  const maybeNew = value as Partial<ThemeConfig> & Record<string, unknown>;
  if (typeof maybeNew.mode === "string") {
    const mode = maybeNew.mode;
    if (mode !== "auto" && mode !== "light" && mode !== "dark") return undefined;
    const overrides = isStringMap(maybeNew.overrides) ? maybeNew.overrides : undefined;
    const explicit = maybeNew.explicit === true ? true : undefined;
    return {
      mode,
      ...(overrides ? { overrides } : {}),
      ...(explicit ? { explicit } : {}),
    };
  }
  const overrides = pickStringEntries(value as Record<string, unknown>);
  if (Object.keys(overrides).length === 0) return undefined;
  return { mode: "dark", overrides };
}

function isStringMap(value: unknown): value is Record<string, string> {
  if (!value || typeof value !== "object" || Array.isArray(value)) return false;
  return Object.values(value).every((entry) => typeof entry === "string");
}

function pickStringEntries(value: Record<string, unknown>): Record<string, string> {
  return Object.fromEntries(
    Object.entries(value).filter(([, v]) => typeof v === "string"),
  ) as Record<string, string>;
}

export class UserConfig {
  private data: UserConfigData = {};

  constructor() {
    this.load();
  }

  private load() {
    const configPath = getConfigPath();
    if (!existsSync(configPath)) return;
    try {
      const raw = readFileSync(configPath, "utf-8");
      const parsed = JSON.parse(raw) as UserConfigData;
      this.data = {
        ...parsed,
        defaultModel: sanitizeDefaultModel(parsed.defaultModel),
        recentModels: sanitizeRecentModels(parsed.recentModels),
        providers: sanitizeProviders(parsed.providers),
        defaultProvider: sanitizeDefaultProvider(parsed.defaultProvider),
        defaultThinkingLevel: sanitizeThinkingLevel(parsed.defaultThinkingLevel),
        agentCategories: sanitizeAgentCategories(parsed.agentCategories),
        subagents: sanitizeSubagentsConfig(parsed.subagents),
        theme: sanitizeTheme(parsed.theme),
      };
    } catch {
      this.data = {};
    }
  }

  private save() {
    const configPath = getConfigPath();
    const dir = dirname(configPath);
    if (!existsSync(dir)) {
      mkdirSync(dir, { recursive: true });
    }
    writeFileSync(configPath, JSON.stringify(this.data, null, 2) + "\n");
  }

  getDefaultModel(): string | undefined {
    return sanitizeDefaultModel(this.data.defaultModel)
      ?? sanitizeRecentModels(this.data.recentModels)?.[0];
  }

  setDefaultModel(model: string) {
    this.data.defaultModel = sanitizeDefaultModel(model);
    this.save();
  }

  getDefaultThinkingLevel(): ThinkingLevel | undefined {
    return this.data.defaultThinkingLevel;
  }

  setDefaultThinkingLevel(level: ThinkingLevel) {
    this.data.defaultThinkingLevel = level;
    this.save();
  }

  getRecentModels(): string[] {
    return sanitizeRecentModels(this.data.recentModels)?.slice() ?? [];
  }

  /** Auto-resume on background-task completion (design §2.3b). Default ON. */
  getTasksAutoResume(): boolean {
    return this.data.tasks?.autoResume !== false;
  }

  /** Grok-style foreground wait budget. `false` disables automatic demotion. */
  getTasksAutoBackgroundAfterMs(): number | false {
    if (this.data.tasks?.autoBackground === false) return false;
    const configured = this.data.tasks?.foregroundWaitMs;
    if (typeof configured !== "number" || !Number.isFinite(configured)) return 15_000;
    return Math.max(1_000, Math.min(300_000, Math.floor(configured)));
  }

  pushRecentModel(model: string) {
    if (isHiddenProviderId(modelProviderId(model)) || !isAllowedConfiguredModel(model)) {
      return;
    }
    const recent = this.data.recentModels ?? [];
    const uniq = [model, ...recent.filter((m) => m !== model)];
    const sanitized = sanitizeRecentModels(uniq.slice(0, 10));
    this.data.recentModels = sanitized;
    this.data.defaultModel = sanitized?.[0];
    this.save();
  }

  getApiKey(): string | undefined {
    return this.data.apiKey;
  }

  setApiKey(key: string) {
    this.data.apiKey = key;
    this.save();
  }

  getProviders(): ProviderProfile[] {
    return sanitizeProviders(this.data.providers)?.slice() ?? [];
  }

  setProviders(providers: ProviderProfile[]) {
    this.data.providers = sanitizeProviders(providers);
    this.save();
  }

  getDefaultProvider(): string | undefined {
    return sanitizeDefaultProvider(this.data.defaultProvider);
  }

  setDefaultProvider(id: string) {
    this.data.defaultProvider = sanitizeDefaultProvider(id);
    this.save();
  }

  getSkillPaths(): string[] {
    return Array.isArray(this.data.skillPaths) ? this.data.skillPaths.slice() : [];
  }

  setSkillPaths(paths: string[]) {
    this.data.skillPaths = paths.slice();
    this.save();
  }

  /** Skill catalog budget in characters; undefined uses the default, 0 turns the catalog off. */
  getSkillCatalogChars(): number | undefined {
    const value = this.data.skills?.catalogChars;
    return typeof value === "number" && Number.isFinite(value) && value >= 0
      ? Math.min(Math.floor(value), 200_000)
      : undefined;
  }

  getDisabledSkills(): string[] {
    const disabled = this.data.skills?.disabled;
    if (!Array.isArray(disabled)) return [];
    return [...new Set(disabled.filter((name): name is string => (
      typeof name === "string" && /^[a-zA-Z0-9][a-zA-Z0-9._-]*$/.test(name)
    )))].sort((a, b) => a.localeCompare(b));
  }

  setDisabledSkills(names: string[]) {
    const disabled = [...new Set(names.filter((name) => (
      /^[a-zA-Z0-9][a-zA-Z0-9._-]*$/.test(name)
    )))].sort((a, b) => a.localeCompare(b));
    if (disabled.length === 0) {
      const next = { ...(this.data.skills ?? {}) };
      delete next.disabled;
      if (Object.keys(next).length === 0) delete this.data.skills;
      else this.data.skills = next;
    } else {
      this.data.skills = { ...(this.data.skills ?? {}), disabled };
    }
    this.save();
  }

  getTheme(): ThemeConfig {
    const theme = sanitizeTheme(this.data.theme);
    return theme ?? { mode: "auto" };
  }

  getThemeMode(): ThemeMode {
    return this.getTheme().mode;
  }

  getThemeOverrides(): Record<string, string> {
    return this.getTheme().overrides ?? {};
  }

  setThemeMode(mode: ThemeMode) {
    const current = this.getTheme();
    this.data.theme = current.overrides
      ? { mode, overrides: current.overrides, explicit: true }
      : { mode, explicit: true };
    this.save();
  }

  setThemeOverrides(overrides: Record<string, string>) {
    const current = this.getTheme();
    this.data.theme = Object.keys(overrides).length === 0
      ? { mode: current.mode, ...(current.explicit ? { explicit: true } : {}) }
      : { mode: current.mode, overrides: { ...overrides }, ...(current.explicit ? { explicit: true } : {}) };
    this.save();
  }

  getAgentCategories(): AgentCategoriesConfig {
    return sanitizeAgentCategories(this.data.agentCategories);
  }

  getAgentRouting(): AgentRoutingConfig {
    return sanitizeAgentRouting(this.data.agentRouting);
  }

  getSubagents(): SubagentsUserConfig {
    return sanitizeSubagentsConfig(this.data.subagents) ?? {};
  }
}

export function shouldProbeTerminalTheme(_config: ThemeConfig): boolean {
  // Always probe: even a forced theme needs the real terminal background to
  // decide between inheriting it (theme matches the terminal) and painting
  // its own canvas (forced theme mismatching the terminal, where the
  // palette's foregrounds would otherwise be unreadable).
  return true;
}

export function effectiveThemeModeForTerminal(
  config: ThemeConfig,
  detectedTheme: Exclude<ThemeMode, "auto">,
): ThemeMode {
  if (isLegacyBareDarkTheme(config) && detectedTheme === "light") return "auto";
  return config.mode;
}

function isLegacyBareDarkTheme(config: ThemeConfig): boolean {
  return config.mode === "dark" && config.explicit !== true && !config.overrides;
}

/** Mask an API key for safe display. */
export function maskKey(key: string): string {
  if (key.length <= 12) return "****";
  return key.slice(0, 6) + "..." + key.slice(-4);
}
