import {
  type AutocompleteArgumentSuggestions,
  CombinedAutocompleteProvider,
  type AutocompleteItem,
  type AutocompleteProvider,
  type AutocompleteSuggestions,
  type SlashCommand as TuiSlashCommand,
} from "@bubblebrain-ai/pi-tui";
import type { UnifiedCommand } from "../slash-commands/unified.js";
import type { SkillSummary } from "../skills/types.js";
import type { TuiMode } from "@bubblebrain-ai/pi-tui";
import {
  BUILTIN_PROVIDERS,
  encodeModel,
  isUserVisibleProvider,
  type ModelInfo,
  type ProviderProfile,
  type ProviderRegistry,
} from "../provider-registry.js";
import {
  getAvailableThinkingLevels,
  isThinkingOnlyLevels,
  isThinkingToggleModel,
  normalizeThinkingLevel,
} from "../provider-transform.js";
import type { ThinkingLevel } from "../types.js";
import {
  GROK_SUBSCRIPTION_PROVIDER_ID,
  isGrokSubscriptionProviderId,
} from "../external-runtime/grok-provider.js";
import type { ResolvedTheme, ThemeMode } from "./model/theme.js";
import {
  discoverModelProviderGroups,
  getVisibleModelProviders,
  localModelsForProvider,
  type ModelPickerRegistry,
  type ModelProviderGroup,
} from "./model-picker-data.js";

export interface ComposerAutocompleteSources {
  cwd: string;
  commands(): UnifiedCommand[];
  skills(): SkillSummary[];
  uiMode?(): TuiMode;
  registry?: ComposerPickerRegistry;
  thinkingLevel?(): ThinkingLevel;
  providerId?(): string;
  themeMode?(): ThemeMode;
  detectedTheme?(): ResolvedTheme;
  /** True while the session is bound to the legacy Grok external runtime. */
  grokRuntimeActive?(): boolean;
  onModelSuggestionsChanged?(): void;
  fdPath?: string | null;
}

type ModelCompletionSource = (
  argumentPrefix: string,
) => AutocompleteArgumentSuggestions | AutocompleteItem[] | null;

type ProviderCompletionSource = ModelCompletionSource;
type ThemeCompletionSource = ModelCompletionSource;
export type AuthCommandName = "login" | "logout";
type AuthCompletionSource = (
  command: AuthCommandName,
  argumentPrefix: string,
) => AutocompleteArgumentSuggestions | AutocompleteItem[] | null;

type ComposerPickerRegistry = ModelPickerRegistry
  & Pick<ProviderRegistry, "getConfigured" | "getDefault">
  & Partial<Pick<ProviderRegistry, "getOAuthLoginKeys">>;

const MODEL_COMMAND_PREFIX = "/model ";
const REASONING_EFFORT_SEPARATOR = " --reasoning-effort ";
const PROVIDER_COMMAND_PREFIX = "/provider ";
const THEME_COMMAND_PREFIX = "/theme ";
const AUTH_COMMAND_PREFIX: Record<AuthCommandName, string> = {
  login: "/login ",
  logout: "/logout ",
};

const EFFORT_DESCRIPTIONS: Record<ThinkingLevel, string> = {
  off: "no reasoning effort",
  minimal: "fastest reasoning",
  low: "light reasoning",
  medium: "balanced reasoning",
  high: "deeper reasoning",
  xhigh: "extra high reasoning",
  max: "maximum provider effort",
  ultra: "maximum effort with delegation",
};

type ModelSelection = {
  provider: ProviderProfile;
  model: ModelInfo;
  value: string;
  levels: ThinkingLevel[];
};

function selectableModels(groups: ModelProviderGroup[]): ModelSelection[] {
  const seen = new Set<string>();
  const selections: ModelSelection[] = [];
  for (const { provider, models } of groups) {
    for (const model of models) {
      const value = encodeModel(provider.id, model.id);
      if (seen.has(value)) continue;
      seen.add(value);
      const declaredLevels = model.reasoningLevels ?? getAvailableThinkingLevels(provider.id, model.id);
      selections.push({
        provider,
        model,
        value,
        levels: declaredLevels.length > 0 ? declaredLevels : ["off"],
      });
    }
  }
  return selections;
}

function preferredThinkingLevel(selection: ModelSelection, current: ThinkingLevel): ThinkingLevel {
  if (selection.levels.includes(current)) return current;
  if (
    selection.model.defaultReasoningLevel &&
    selection.levels.includes(selection.model.defaultReasoningLevel)
  ) {
    return selection.model.defaultReasoningLevel;
  }
  return normalizeThinkingLevel(current, selection.levels);
}

function effortPhase(
  selections: ModelSelection[],
  argumentPrefix: string,
): { selection: ModelSelection; query: string } | null {
  const separatorIndex = argumentPrefix.indexOf(REASONING_EFFORT_SEPARATOR);
  if (separatorIndex < 0) return null;
  const modelValue = argumentPrefix.slice(0, separatorIndex);
  const selection = selections.find((candidate) => candidate.value === modelValue);
  if (!selection || selection.levels.length <= 1) return null;
  return {
    selection,
    query: argumentPrefix.slice(separatorIndex + REASONING_EFFORT_SEPARATOR.length),
  };
}

function modelCatalogKey(groups: ModelProviderGroup[]): string {
  return JSON.stringify(groups.map(({ provider, models }) => [
    provider.id,
    provider.baseURL,
    provider.authType ?? "api",
    provider.protocol ?? "default",
    provider.apiKey,
    models.map((model) => [
      model.id,
      model.name,
      model.reasoningLevels,
      model.defaultReasoningLevel,
      model.contextWindow,
      model.useResponsesLite,
      model.toolOutputTokenLimit,
      model.tier,
    ]),
  ]));
}

export function buildModelAutocompleteItems(
  groups: ModelProviderGroup[],
  argumentPrefix = "",
  currentThinkingLevel: ThinkingLevel = "off",
): AutocompleteItem[] {
  const selections = selectableModels(groups);
  const effort = effortPhase(selections, argumentPrefix);
  if (effort) {
    const toggle = isThinkingToggleModel(effort.selection.provider.id, effort.selection.model.id);
    const query = effort.query.trim().toLowerCase();
    const baseValue = `${effort.selection.value}${REASONING_EFFORT_SEPARATOR}`;
    return effort.selection.levels.flatMap((level) => {
      const label = toggle ? (level === "off" ? "off" : "on") : level;
      const description = toggle
        ? (level === "off" ? "thinking disabled" : "thinking enabled")
        : EFFORT_DESCRIPTIONS[level];
      if (query && !`${label} ${level} ${description}`.toLowerCase().includes(query)) return [];
      return [{
        value: `${baseValue}${level}`,
        label,
        description,
        submitOnSelect: true,
      }];
    });
  }

  const query = argumentPrefix.trim().toLowerCase();
  const items: AutocompleteItem[] = [];

  for (const selection of selections) {
    const { provider, model, value, levels } = selection;
    const label = model.name || model.id;
    // A model with one fixed grade skips the effort step, so name the grade
    // here or the user never learns which effort it runs at.
    const fixedEffort = levels.length === 1 && levels[0] !== "off" && !isThinkingOnlyLevels(levels)
      ? `${levels[0]} effort`
      : undefined;
    const description = [provider.name, label === model.id ? undefined : model.id, fixedEffort]
      .filter(Boolean)
      .join(" · ");
    const searchable = `${value} ${label} ${description}`.toLowerCase();
    if (query && !searchable.includes(query)) continue;

    const level = preferredThinkingLevel(selection, currentThinkingLevel);
    items.push({
      value: levels.length > 1
        ? `${value}${REASONING_EFFORT_SEPARATOR}`
        : `${value}${REASONING_EFFORT_SEPARATOR}${level}`,
      label,
      description,
      submitOnSelect: levels.length <= 1,
    });
  }

  return items;
}

export function buildProviderAutocompleteItems(
  registry: Pick<ProviderRegistry, "getConfigured" | "getModelConfig">,
  argumentPrefix = "",
  currentProviderId?: string,
): AutocompleteItem[] {
  const query = argumentPrefix.trim().toLowerCase();
  if (query.startsWith("--")) return [];

  const configured = registry.getConfigured();
  const configuredById = new Map(configured.map((provider) => [provider.id, provider]));
  const seen = new Set<string>();
  const candidates = [
    ...BUILTIN_PROVIDERS.filter((provider) => isUserVisibleProvider(provider.id)),
    ...configured.filter((provider) => isUserVisibleProvider(provider.id)),
  ];

  return candidates.flatMap((provider) => {
    if (seen.has(provider.id)) return [];
    seen.add(provider.id);

    const profile = configuredById.get(provider.id);
    const builtin = BUILTIN_PROVIDERS.find((candidate) => candidate.id === provider.id);
    const managedByFile = registry.getModelConfig().hasProvider(provider.id);
    const status = profile?.apiKey
      ? (profile.authType === "oauth" ? "OAuth connected" : "Configured")
      : builtin?.supportsOAuth
        ? "OAuth available"
        : "Needs API key";
    const current = provider.id === currentProviderId;
    const action = profile?.apiKey || provider.id === "grok" ? "--set" : "--add";
    const source = managedByFile ? " · models.json" : "";
    const description = `${current ? "Current · " : ""}${provider.id} · ${status}${source}`;
    const searchable = `${provider.id} ${provider.name} ${description}`.toLowerCase();
    if (query && !searchable.includes(query)) return [];

    return [{
      value: `${action} ${provider.id}`,
      label: provider.name,
      description,
      submitOnSelect: true,
    }];
  });
}

export interface AuthAccountState {
  isSignedIn(providerId: string): boolean;
  /** Session is bound to the legacy Grok external runtime. */
  grokRuntimeActive?: boolean;
}

/**
 * What picking a row will actually do. Provider-specific because the handlers
 * differ: /login openai always runs a fresh OAuth flow, while /login grok
 * reuses stored credentials and only opens the browser when they fail.
 */
function authActionHint(
  command: AuthCommandName,
  providerId: string,
  signedIn: boolean,
  grokRuntimeActive: boolean,
): string {
  const grok = isGrokSubscriptionProviderId(providerId);
  if (command === "login") {
    if (!signedIn) return "opens the browser";
    return grok ? "reuses the stored sign-in · /logout grok first to switch accounts" : "sign in again";
  }
  if (grok && grokRuntimeActive) return "ends the active Grok session and starts a fresh one";
  return signedIn ? "removes this device's credentials" : "nothing to remove";
}

/**
 * Accounts for /login and /logout. Derived from the catalog so a new OAuth
 * provider is offered without touching this list.
 */
export function buildAuthAutocompleteItems(
  command: AuthCommandName,
  argumentPrefix = "",
  state: AuthAccountState = { isSignedIn: () => false },
): AutocompleteItem[] {
  const query = argumentPrefix.trim().toLowerCase();
  return BUILTIN_PROVIDERS.flatMap((provider) => {
    if (!provider.supportsOAuth || !isUserVisibleProvider(provider.id)) return [];
    const grok = isGrokSubscriptionProviderId(provider.id);
    const runtimeActive = grok && state.grokRuntimeActive === true;
    const signedIn = state.isSignedIn(provider.id);
    const status = signedIn ? "Signed in" : runtimeActive ? "Active session" : "Not signed in";
    const hint = authActionHint(command, provider.id, signedIn, runtimeActive);
    const description = `${provider.id} · ${status} · ${hint}`;
    // Match the account only. The status/hint prose is shared by every row,
    // so searching it would make "open" match Grok via "opens the browser".
    // Include every id the handler accepts so a typed alias still matches.
    const aliases = grok ? ` ${GROK_SUBSCRIPTION_PROVIDER_ID}` : "";
    const searchable = `${provider.id} ${provider.name}${aliases}`.toLowerCase();
    if (query && !searchable.includes(query)) return [];
    return [{ value: provider.id, label: provider.name, description, submitOnSelect: true }];
  });
}

export function buildThemeAutocompleteItems(
  argumentPrefix = "",
  detectedTheme: ResolvedTheme = "dark",
): AutocompleteItem[] {
  const query = argumentPrefix.trim().toLowerCase();
  const candidates: Array<{ value: ThemeMode; label: string; description: string }> = [
    { value: "auto", label: "Auto", description: `Match terminal · currently ${detectedTheme}` },
    { value: "light", label: "Light", description: "Light surfaces and dark text" },
    { value: "dark", label: "Dark", description: "Dark surfaces and light text" },
  ];
  return candidates.flatMap((candidate) => {
    const searchable = `${candidate.value} ${candidate.label} ${candidate.description}`.toLowerCase();
    if (query && !searchable.includes(query)) return [];
    return [{ ...candidate, submitOnSelect: true }];
  });
}

/**
 * Build the command surface in execution-priority order. The registry remains
 * live so MCP prompts that connect after startup appear on the next keypress.
 */
export function buildComposerSlashCommands(
  commands: UnifiedCommand[],
  skills: SkillSummary[],
  uiMode: TuiMode = "regular",
  modelCompletions?: ModelCompletionSource,
  providerCompletions?: ProviderCompletionSource,
  themeCompletions?: ThemeCompletionSource,
  authCompletions?: AuthCompletionSource,
): TuiSlashCommand[] {
  const result = new Map<string, TuiSlashCommand>();
  const add = (command: TuiSlashCommand) => {
    if (!result.has(command.name)) result.set(command.name, command);
  };

  // Renderer-local commands execute before the shared registry.
  if (uiMode === "regular") {
    add({ name: "fullscreen", description: "Open the alternate-screen transcript view" });
  }

  for (const command of commands.filter((entry) => entry.source === "builtin")) {
    if (command.name === "model" && modelCompletions) {
      add({
        name: command.name,
        description: command.description,
        argumentHint: "<model>",
        submitOnSelect: false,
        argumentInputHint: {
          prompt: "⌕ ",
          placeholder: "Search models…",
          valuePrefix: "/model ",
        },
        keepArgumentMenuOnEmpty: true,
        argumentEmptyMessage: "No matching models",
        getArgumentCompletions: modelCompletions,
      });
    } else if (command.name === "provider" && providerCompletions) {
      add({
        name: command.name,
        description: command.description,
        argumentHint: "<provider>",
        submitOnSelect: false,
        argumentInputHint: {
          prompt: "⌕ ",
          placeholder: "Search providers…",
          valuePrefix: PROVIDER_COMMAND_PREFIX,
        },
        keepArgumentMenuOnEmpty: true,
        argumentEmptyMessage: "No matching providers",
        getArgumentCompletions: providerCompletions,
      });
    } else if (command.name === "theme" && themeCompletions) {
      add({
        name: command.name,
        description: command.description,
        argumentHint: "<auto|light|dark>",
        submitOnSelect: false,
        argumentInputHint: {
          prompt: "◐ ",
          placeholder: "Select theme…",
          valuePrefix: THEME_COMMAND_PREFIX,
        },
        keepArgumentMenuOnEmpty: true,
        argumentEmptyMessage: "No matching themes",
        getArgumentCompletions: themeCompletions,
      });
    } else if ((command.name === "login" || command.name === "logout") && authCompletions) {
      const authCommand: AuthCommandName = command.name;
      add({
        name: command.name,
        description: command.description,
        argumentHint: "<account>",
        submitOnSelect: false,
        argumentInputHint: {
          prompt: "⌕ ",
          placeholder: "Select account…",
          valuePrefix: AUTH_COMMAND_PREFIX[authCommand],
        },
        // No keep-open-on-empty: an empty menu swallows Enter, and a typed id
        // the list does not show must still reach the handler's own error.
        getArgumentCompletions: (prefix) => authCompletions(authCommand, prefix),
      });
    } else {
      add({ name: command.name, description: command.description });
    }
  }

  for (const skill of skills) {
    const source = skill.source ? ` · ${skill.source}` : "";
    add({
      name: skill.name,
      argumentHint: "<request>",
      submitOnSelect: false,
      description: `[skill${source}] ${skill.description}`,
    });
  }

  for (const command of commands.filter((entry) => entry.source !== "builtin")) {
    const source = command.sourceLabel ? `mcp:${command.sourceLabel}` : command.source;
    add({ name: command.name, description: `[${source}] ${command.description}` });
  }

  return [...result.values()];
}

/** A live adapter around pi-tui's command/file completion implementation. */
export class ComposerAutocompleteProvider implements AutocompleteProvider {
  private discoveredModels: { key: string; groups: ModelProviderGroup[] } | null = null;
  private modelRefresh: { key: string; task: Promise<void> } | null = null;

  constructor(private readonly sources: ComposerAutocompleteSources) {}

  async getSuggestions(
    lines: string[],
    cursorLine: number,
    cursorCol: number,
    options: { signal: AbortSignal; force?: boolean },
  ): Promise<AutocompleteSuggestions | null> {
    return this.delegate().getSuggestions(lines, cursorLine, cursorCol, options);
  }

  applyCompletion(
    lines: string[],
    cursorLine: number,
    cursorCol: number,
    item: AutocompleteItem,
    prefix: string,
  ): { lines: string[]; cursorLine: number; cursorCol: number } {
    return this.delegate().applyCompletion(lines, cursorLine, cursorCol, item, prefix);
  }

  shouldTriggerFileCompletion(lines: string[], cursorLine: number, cursorCol: number): boolean {
    return this.delegate().shouldTriggerFileCompletion?.(lines, cursorLine, cursorCol) ?? true;
  }

  private delegate(): CombinedAutocompleteProvider {
    return new CombinedAutocompleteProvider(
      buildComposerSlashCommands(
        this.sources.commands(),
        this.sources.skills(),
        this.sources.uiMode?.() ?? "regular",
        this.sources.registry ? (prefix) => this.getModelCompletions(prefix) : undefined,
        this.sources.registry ? (prefix) => this.getProviderCompletions(prefix) : undefined,
        (prefix) => this.getThemeCompletions(prefix),
        (command, prefix) => this.getAuthCompletions(command, prefix),
      ),
      this.sources.cwd,
      this.sources.fdPath ?? null,
    );
  }

  private getAuthCompletions(
    command: AuthCommandName,
    argumentPrefix: string,
  ): AutocompleteArgumentSuggestions {
    const registry = this.sources.registry;
    return {
      items: buildAuthAutocompleteItems(command, argumentPrefix, {
        isSignedIn: (id) => (registry?.getOAuthLoginKeys?.(id).length ?? 0) > 0,
        grokRuntimeActive: this.sources.grokRuntimeActive?.() ?? false,
      }),
      inputHint: {
        prompt: "⌕ ",
        placeholder: "Select account…",
        valuePrefix: AUTH_COMMAND_PREFIX[command],
      },
    };
  }

  private getThemeCompletions(argumentPrefix: string): AutocompleteArgumentSuggestions {
    const items = buildThemeAutocompleteItems(argumentPrefix, this.sources.detectedTheme?.() ?? "dark");
    const current = this.sources.themeMode?.() ?? "auto";
    return {
      items,
      inputHint: {
        prompt: "◐ ",
        placeholder: "Select theme…",
        valuePrefix: THEME_COMMAND_PREFIX,
      },
      keepOpenOnEmpty: true,
      emptyMessage: "No matching themes",
      ...(argumentPrefix.length === 0 ? { preferredValue: current } : {}),
    };
  }

  private getProviderCompletions(argumentPrefix: string): AutocompleteArgumentSuggestions | null {
    const registry = this.sources.registry;
    if (!registry || argumentPrefix.trimStart().startsWith("--")) return null;

    const currentProviderId = this.sources.providerId?.() ?? registry.getDefault()?.id;
    const items = buildProviderAutocompleteItems(registry, argumentPrefix, currentProviderId);
    return {
      items,
      inputHint: {
        prompt: "⌕ ",
        placeholder: "Search providers…",
        valuePrefix: PROVIDER_COMMAND_PREFIX,
      },
      keepOpenOnEmpty: true,
      emptyMessage: "No matching providers",
      ...(argumentPrefix.length === 0 && currentProviderId
        ? { preferredValue: `--set ${currentProviderId}` }
        : {}),
    };
  }

  private getModelCompletions(argumentPrefix: string): AutocompleteArgumentSuggestions | null {
    const registry = this.sources.registry;
    if (!registry) return null;

    const localGroups = getVisibleModelProviders(registry).map((provider) => ({
      provider,
      models: localModelsForProvider(registry, provider),
    }));
    const key = modelCatalogKey(localGroups);
    const groups = this.discoveredModels?.key === key ? this.discoveredModels.groups : localGroups;

    this.refreshModelCatalog(key);
    const currentThinkingLevel = this.sources.thinkingLevel?.() ?? "off";
    const items = buildModelAutocompleteItems(groups, argumentPrefix, currentThinkingLevel);
    const selections = selectableModels(groups);
    const effort = effortPhase(selections, argumentPrefix);
    if (!effort) {
      return {
        items,
        inputHint: {
          prompt: "⌕ ",
          placeholder: "Search models…",
          valuePrefix: MODEL_COMMAND_PREFIX,
        },
        keepOpenOnEmpty: true,
        emptyMessage: "No matching models",
      };
    }

    const toggle = isThinkingToggleModel(effort.selection.provider.id, effort.selection.model.id);
    const baseValue = `${effort.selection.value}${REASONING_EFFORT_SEPARATOR}`;
    const preferredLevel = preferredThinkingLevel(effort.selection, currentThinkingLevel);
    return {
      items,
      inputHint: {
        prompt: "◆ ",
        placeholder: toggle ? "Select thinking mode…" : "Select reasoning effort…",
        valuePrefix: `${MODEL_COMMAND_PREFIX}${baseValue}`,
        backValue: MODEL_COMMAND_PREFIX,
      },
      keepOpenOnEmpty: true,
      emptyMessage: toggle ? "No matching modes" : "No matching efforts",
      ...(effort.query.length === 0
        ? { preferredValue: `${baseValue}${preferredLevel}` }
        : {}),
    };
  }

  /**
   * Remote discovery enriches the already-visible local catalog. It is shared
   * per provider snapshot and runs all providers concurrently, so opening the
   * composer menu never waits on network latency.
   */
  private refreshModelCatalog(key: string): void {
    const registry = this.sources.registry;
    if (!registry || this.discoveredModels?.key === key || this.modelRefresh?.key === key) return;

    const task = discoverModelProviderGroups(registry).then((groups) => {
      const currentGroups = getVisibleModelProviders(registry).map((provider) => ({
        provider,
        models: localModelsForProvider(registry, provider),
      }));
      if (modelCatalogKey(currentGroups) !== key) return;
      this.discoveredModels = { key, groups };
      this.sources.onModelSuggestionsChanged?.();
    }).finally(() => {
      if (this.modelRefresh?.task === task) this.modelRefresh = null;
    });
    this.modelRefresh = { key, task };
  }
}
