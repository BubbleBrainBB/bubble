import { toolImageObservation } from "./context/tool-images.js";
/**
 * Agent - The core decision loop.
 * It maintains message state, calls the LLM, executes tools, and auto-continues.
 */

import { compactCurrentTurnToolGroups, compactMessages, isCompactionSummaryMessage, splitLeadingContext } from "./context/compact.js";
import { createContextCheckpoint, checkpointMessages, hasCompleteToolGroups, type ContextCheckpoint } from "./context/checkpoint.js";
import { randomUUID } from "node:crypto";

import {
  estimateToolDefinitionsTokens,
  getContextBudget,
  getMaxInputTokens,
} from "./context/budget.js";
import { buildContextUsageSnapshot, type ContextUsageSnapshot } from "./context/usage.js";
import { isContextOverflowError } from "./context/overflow.js";
import {
  computeRetryDelayMs,
  isProviderStreamInterruption,
  providerStreamRetryLimit,
  sleepBeforeRetry,
} from "./network/retry.js";
import { projectMessages } from "./context/projector.js";
import { aggressivePruneMessages, pruneMessages, markStableCurrentToolResultsForCache, markToolResultCacheStable } from "./context/prune.js";
import { normalizeToolResultForModel, truncateToolOutputForModel } from "./context/tool-output-truncate.js";
import { buildDeferredToolsReminder, buildToolFreezeReminder, isPermissionModeReminder, reminderForMode } from "./prompt/reminders.js";
import type { AgentEvent, AgentInputController, AgentRunInput, ContentPart, PermissionMode, Message, ParsedToolCall, Provider, ThinkingLevel, TokenUsage, ToolDefinition, ToolMessage, ToolResult, ToolRegistryEntry, ToolUpdate } from "./types.js";
import { HookBus, type TurnHooks, type TurnHookState } from "./orchestrator/hooks.js";
import type { ExternalHookController } from "./hooks/controller.js";
import {
  normalizeHookInput,
  truncateHookText,
  type HookCombinedResult,
  type HookProgressEvent,
  type HookRunRequest,
} from "./hooks/index.js";
import { createDefaultHooks } from "./orchestrator/default-hooks.js";
import { buildTaskLifecycleReminder } from "./agent/task-lifecycle-reminder.js";
import type { AgentCategoriesConfig, ResolvedSubagentRoute } from "./agent/categories.js";
import { DEFAULT_AGENT_ROUTING, sanitizeAgentRouting, type AgentRoutingConfig, type RoutableModelEntry, type RoutableModelIndex, type RoutingSnapshotAccessor } from "./agent/routing-catalog.js";
import { SubagentRouter } from "./agent/subagent/router.js";
import { normalizeWaitTimeout, SubagentRuntime, type ChildAgentLike, type ChildAgentSpec } from "./agent/subagent/runtime.js";
import { BudgetLedger } from "./agent/budget-ledger.js";
import { WorkflowLedger } from "./agent/workflow/runs.js";
import { type WorkflowRunSnapshot } from "./agent/workflow/control.js";
import type { AgentProfile, SubagentRunResult } from "./agent/profiles.js";
import type { SubagentThreadSnapshot } from "./agent/subagent-control.js";
import { SubagentStore } from "./agent/subagent-store.js";
import { EMPTY_ASSISTANT_FALLBACK, throwIfAborted, isAbortLikeError, isAbortError } from "./agent/abort-errors.js";
import { appendProviderContentBlock, buildProviderRequestFingerprint } from "./agent/provider-fingerprint.js";
import { countUserTurns, estimateResidentChars, estimateToolPayloadChars, getCurrentHeapUsed, RESIDENT_HISTORY_HEAP_HARD_LIMIT, RESIDENT_HISTORY_KEEP_RECENT_TURNS } from "./agent/resident-history.js";
import { cancelledToolResult, createModelInterruptedMessage, findMissingRequiredArgs, isSubagentLifecycleTool, lastProviderMessage, shouldAppendModelInterruptedBoundary } from "./agent/transcript-helpers.js";
import { createUpdateQueue } from "./agent/update-queue.js";
export { createUpdateQueue } from "./agent/update-queue.js";
import { type RateLimitPolicy } from "./network/errors.js";
import { isHiddenToolResult } from "./agent/tool-visibility.js";
import {
  createStreamingInternalReminderSanitizer,
  sanitizeAssistantProviderMetadata,
  sanitizeInternalReasoningText,
  sanitizeInternalReminderBlocks,
} from "./agent/internal-reminder-sanitizer.js";
import { debugReasoningStream, summarizeDebugText } from "./reasoning-debug.js";
import type { SkillSummary } from "./skills/types.js";
import type { FileStateTracker } from "./tools/file-state.js";
import { buildToolPromptOptions } from "./tools/prompt-metadata.js";
import { stopAutoServersForSession } from "./tools/server-manager.js";
import {
  createSanitizedProviderError,
  type ProviderErrorContext,
  type SanitizedProviderError,
} from "./provider-error-record.js";
import {
  summarizeAgentEventForTrace,
  summarizeTraceError,
  summarizeTraceMessage,
  summarizeTraceToolResult,
  summarizeTraceValue,
  traceEvent,
} from "./debug-trace.js";

const MAX_CONSECUTIVE_OVERFLOW_RECOVERIES = 2;
const MAX_EMPTY_ASSISTANT_RECOVERIES = 1;
const EMPTY_ASSISTANT_RECOVERY_REMINDER =
  "The previous model response contained no user-visible assistant content and no tool calls. " +
  "Respond now with a concise, user-visible answer, or call an available tool if more work is required. " +
  "Do not put the final answer only in hidden reasoning.";

class LocalContextPreflightError extends Error {
  constructor(estimatedTokens: number, maxInputTokens: number) {
    super(
      `Local request preflight: estimated prompt length ${estimatedTokens} tokens exceeds maximum prompt length ${maxInputTokens}.`,
    );
    this.name = "LocalContextPreflightError";
  }
}
export { AgentAbortError, SubagentAbortError } from "./agent/abort-errors.js";
// Model-facing interruption boundary. Persisted into the transcript so the
// next turn sees an explicit stop instead of a dangling request — but it must
// never render in the UI as if the assistant said it (the TUIs strip it and
// show their own interrupt indicator instead).
export const INTERRUPTED_ASSISTANT_CONTENT =
  "Interrupted by user. The prior request was stopped and should not be resumed unless the user asks.";

function agentEventFromHookProgress(event: HookProgressEvent): AgentEvent {
  const source = `${event.source.scope}:${event.source.index}`;
  if (event.type === "hook_start") {
    return {
      type: "hook_start",
      eventName: event.eventName,
      hookId: event.hookId,
      source,
    };
  }
  if (event.type === "hook_end") {
    return {
      type: "hook_end",
      eventName: event.eventName,
      hookId: event.hookId,
      source,
      elapsedMs: event.elapsedMs ?? 0,
      decision: event.decision ?? "allow",
      reason: event.reason,
    };
  }
  return {
    type: "hook_error",
    eventName: event.eventName,
    hookId: event.hookId,
    source,
    elapsedMs: event.elapsedMs,
    decision: event.decision,
    reason: event.reason,
    error: event.error ?? "Hook failed.",
  };
}

/** Runtime tuning for the subagent scheduler. */
export interface AgentSubagentRuntimeConfig {
  maxActiveSubagents?: number;
  launchBurst?: number;
  launchIntervalMs?: number;
  rateLimitMaxAttempts?: number;
  rateLimitBackoffMs?: number[];
  transportRetryMaxAttempts?: number;
  transportRetryBackoffMs?: number[];
  /**
   * Directory for persisted child state (design §7). Defaults to
   * `<session>.subagents` next to the session file when a session exists.
   */
  persistDir?: string;
}

export interface AgentOptions {
  provider: Provider;
  sessionID?: string;
  providerId?: string;
  model: string;
  tools: ToolRegistryEntry[];
  temperature?: number;
  thinkingLevel?: ThinkingLevel;
  mode?: PermissionMode;
  steps?: number;
  maxTurns?: number;
  taskBudget?: { total: number };
  systemPrompt?: string;
  onMessageAppend?: (message: Message) => void;
  /** Persistable, allowlisted diagnostics for terminal provider failures. */
  onProviderError?: (error: SanitizedProviderError) => void;
  onToolResult?: (toolName: string, result: ToolResult) => void;
  onModeUpdate?: (mode: PermissionMode) => void;
  /**
   * Fired when MULTI-TURN compaction rewrites resident history, with the
   * summary text. Hosts persist it (SessionManager.applyLLMCompaction) so the
   * compacted state — including the cumulative file blocks — survives resume;
   * without this, meta summaries live only in memory (session-log drops meta).
   * Sub-turn summaries never fire it: they cover only the current turn, and a
   * session-level summary entry would make resume skip earlier history.
   */
  onCompactionApplied?: (summary: string) => void;
  /** Compact no later than this many tokens of context (context/budget.ts); the model's window alone decides when absent. */
  compactAtTokens?: number;
  /** Required durable commit hook for hosts; exceptions abort the context switch. */
  onContextCheckpoint?: (checkpoint: ContextCheckpoint) => void;
  getContextRevision?: () => string;
  hooks?: TurnHooks[];
  externalHooks?: ExternalHookController;
  agentRole?: "parent" | "subagent";
  subAgentId?: string;
  budgetLedger?: BudgetLedger;
  budgetSource?: { runId: string; subAgentId?: string };
  skills?: SkillSummary[];
  /** Skill catalog budget in characters for rebuilt system prompts (0 = off). */
  skillCatalogChars?: number;
  memoryPrompt?: string;
  fileStateTracker?: FileStateTracker;
  agentCategories?: AgentCategoriesConfig;
  /**
   * Routing config held directly on the Agent, independent of any snapshot:
   * the cross-provider lock must hold even in hosts that wire no routing
   * data (design §7.0).
   */
  agentRouting?: Partial<AgentRoutingConfig>;
  /**
   * Live routing-snapshot accessor (design §1.5). Host-constructed; caches by
   * registry revision. When absent, catalog-dependent features (tier routing,
   * menu, unknown-model validation) are simply off.
   */
  routingSnapshot?: RoutingSnapshotAccessor;
  /**
   * Cross-provider routable model index (design v3.6): powers the user-named
   * model reminder and near-match correction of mistyped provider:model ids.
   */
  routableModels?: RoutableModelIndex;
  providerFactory?: (route: ResolvedSubagentRoute) => Provider | Promise<Provider>;
  subagents?: AgentSubagentRuntimeConfig;
  /** Subagent routes use "defer" so the scheduler is the single 429 backoff layer (design §4.5). */
  rateLimitPolicy?: RateLimitPolicy;
}

export interface AgentRunOptions {
  abortSignal?: AbortSignal;
  inputController?: AgentInputController;
  /** Child follow-ups continue at a safe boundary, including a tool-free answer. */
  continueOnPendingInput?: boolean;
  /** Local-only presentation metadata persisted with the submitted user turn. */
  userMessageUi?: Extract<Message, { role: "user" }>["ui"];
  /**
   * Internal: re-enter the loop without appending the input as a new user
   * message. Used by the subagent scheduler's rate-limit re-entry so a child
   * history contains exactly one copy of its input (design doc §4.5).
   */
  resumeWithoutInput?: boolean;
}

export class Agent {
  /**
   * Backing store for the public `messages` accessor. All whole-array
   * rewrites (clear/compact/rewind/session switch/subagent fork) go through
   * the setter, which notifies `onContextChanged` subscribers — that push
   * channel is what keeps derived UI (e.g. the footer usage readout) in sync
   * without per-path refresh calls that are easy to forget.
   */
  private _messages: Message[] = [];
  private contextChangedListeners = new Set<() => void>();
  private notifyingContextChange = false;

  get messages(): Message[] {
    return this._messages;
  }

  set messages(value: Message[]) {
    this._messages = value;
    this.notifyContextChanged();
  }
  private provider: Provider;
  private sessionID?: string;
  /**
   * Bridge to the unified process manager's background tasks, wired by the
   * host (main.ts) when the host supports them (background-tasks design
   * §2.3a). list() is pre-filtered to the agent's bound session.
   */
  backgroundTasks?: {
    list: () => import("./tasks/manager.js").BackgroundTaskInfo[];
    version: () => number;
    outputTail: (id: string) => string | undefined;
    kill?: (id: string) => Promise<import("./tasks/manager.js").BackgroundTaskInfo | undefined>;
    subscribe?: (listener: () => void) => () => void;
  };
  private lastTaskReminderVersion = -1;
  /**
   * Once-per-session latch for the large-task delegation nudge
   * (large-task-delegation design §2): goal-loop continuations rebuild
   * TurnHookState every run, so a per-turn latch would nag on every turn.
   * If the model read the nudge once and chose not to delegate, repeating
   * it is noise.
   */
  private _providerId: string;
  private _model: string;
  private tools: Map<string, ToolRegistryEntry> = new Map();
  private unlockedDeferred: Set<string> = new Set();
  private temperature: number;
  private thinkingLevel: ThinkingLevel;
  private _mode: PermissionMode;
  private _modeVersion = 0;
  private onModeUpdate?: (mode: PermissionMode) => void;
  private onCompactionApplied?: (summary: string) => void;
  private compactAtTokens?: number;
  private onContextCheckpoint?: (checkpoint: ContextCheckpoint) => void;
  private getContextRevision?: () => string;
  private lastCompactionId?: string;
  private onMessageAppend?: (message: Message) => void;
  private onProviderError?: (error: SanitizedProviderError) => void;
  private onToolResult?: (toolName: string, result: ToolResult) => void;
  private hookDefinitions: TurnHooks[];
  private externalHooks?: ExternalHookController;
  private agentRole: "parent" | "subagent";
  private subAgentId?: string;
  private maxTurns?: number;
  private taskBudget?: { total: number };
  private budgetLedger?: BudgetLedger;
  private budgetSource: { runId: string; subAgentId?: string };
  private skillSummaries: SkillSummary[];
  private skillCatalogChars?: number;
  private memoryPrompt?: string;
  private fileStateTracker?: FileStateTracker;
  private agentCategories: AgentCategoriesConfig;
  private agentRouting: AgentRoutingConfig;
  /** Owns the routing decision chain, the §6 detector state and the §4 menu. */
  private readonly router: SubagentRouter;
  private providerFactory?: (route: ResolvedSubagentRoute) => Provider | Promise<Provider>;
  /**
   * Subagent thread lifecycle + workflow script execution: the store,
   * scheduler, ChildRunner and their dispatch/ingestion plumbing.
   */
  private readonly subagents: SubagentRuntime;
  /**
   * Background dynamic-workflow runs (option C Phase 4): run records, waiters,
   * cancellation and the pending-delivery set. Execution stays on Agent.
   */
  private readonly workflowLedger: WorkflowLedger;
  private subagentsConfig: AgentSubagentRuntimeConfig;
  private readonly rateLimitPolicy?: RateLimitPolicy;
  private lastInputTokens: number | null = null;
  private lastAnchorMessageCount: number | null = null;
  // How often each compaction path actually rewrote history this run.
  // Surfaced via getCompactionStats() for print-mode telemetry: without it,
  // "did the model ever stop seeing the original instruction?" is
  // unanswerable from the outside. `fired` counts successful compaction
  // computations including ones whose rewrite was later rejected — a
  // fired-but-never-written gap is the churn signal.
  private compactionStats = { resident: 0, subturn: 0, llm: 0, heuristic: 0, overflow: 0, fired: 0, droppedMessages: 0 };
  private contextEvents: AgentEvent[] = [];

  constructor(options: AgentOptions) {
    this.provider = options.provider;
    this.sessionID = options.sessionID;
    this._providerId = options.providerId ?? "";
    this._model = options.model;
    this.temperature = options.temperature ?? 0.2;
    this.thinkingLevel = options.thinkingLevel ?? "off";
    this._mode = options.mode ?? "default";
    this.onMessageAppend = options.onMessageAppend;
    this.onProviderError = options.onProviderError;
    this.onToolResult = options.onToolResult;
    this.onModeUpdate = options.onModeUpdate;
    this.onCompactionApplied = options.onCompactionApplied;
    this.compactAtTokens = options.compactAtTokens;
    this.onContextCheckpoint = options.onContextCheckpoint;
    this.getContextRevision = options.getContextRevision;
    this.hookDefinitions = options.hooks ?? [];
    this.externalHooks = options.externalHooks;
    this.agentRole = options.agentRole ?? "parent";
    this.subAgentId = options.subAgentId;
    this.maxTurns = options.maxTurns ?? options.steps;
    this.taskBudget = options.taskBudget;
    this.budgetLedger = options.budgetLedger;
    this.budgetSource = options.budgetSource ?? { runId: this.sessionID ?? "agent" };
    this.skillSummaries = options.skills ?? [];
    this.skillCatalogChars = options.skillCatalogChars;
    this.memoryPrompt = options.memoryPrompt;
    this.fileStateTracker = options.fileStateTracker;
    this.agentCategories = options.agentCategories ?? {};
    this.agentRouting = options.agentRouting
      ? sanitizeAgentRouting(options.agentRouting)
      : { ...DEFAULT_AGENT_ROUTING };
    // parentRoute is a thunk, not a snapshot: /model reassigns provider/model/
    // thinking mid-session and children must inherit the CURRENT values.
    this.router = new SubagentRouter({
      parentRoute: () => ({
        providerId: this.providerId,
        model: this.apiModel,
        thinkingLevel: this.thinkingLevel,
      }),
      categories: this.agentCategories,
      routing: this.agentRouting,
      snapshot: options.routingSnapshot,
      routableModels: options.routableModels,
    });
    this.providerFactory = options.providerFactory;
    this.workflowLedger = new WorkflowLedger({
      execute: (cwd, opts) => this.subagents.executeWorkflow(cwd, opts),
    });
    this.subagentsConfig = options.subagents ?? {};
    this.rateLimitPolicy = options.rateLimitPolicy;
    const persistDir = this.deriveSubagentPersistDir();
    // `self` so the parent adapter's getters read through to this agent.
    const self = this;
    this.subagents = new SubagentRuntime({
      parent: {
        // Live accessors, never snapshots — /model reassigns these mid-session.
        get provider() { return self.provider; },
        get providerId() { return self.providerId; },
        get apiModel() { return self.apiModel; },
        get thinkingLevel() { return self.thinkingLevel; },
        get memoryPrompt() { return self.memoryPrompt; },
        get providerFactory() { return self.providerFactory; },
        // Unfiltered on purpose: profile admission and child tool selection
        // must see deferred-but-registered tools (an explicit include in a
        // profile is the author pre-unlocking them).
        allTools: () => [...this.tools.values()],
        getSkillSummaries: () => this.getSkillSummaries(),
        getSkillCatalogChars: () => this.getSkillCatalogChars(),
        createChild: (spec) => this.createChildAgent(spec),
        recordProviderError: (error, context) => this.recordProviderError(error, context),
        runExternalHook: (input, abortSignal) => this.runExternalHook(input as any, abortSignal),
      },
      router: this.router,
      categories: this.agentCategories,
      config: this.subagentsConfig,
      persistDir,
    });

    if (options.systemPrompt) {
      this._messages.push({ role: "system", content: options.systemPrompt });
    }

    for (const tool of options.tools) {
      this.tools.set(tool.name, tool);
    }

    // If the agent boots in a non-default mode, inject the corresponding reminder so the
    // model sees the active rules on its very first turn. Default mode needs no reminder.
    if (this._mode !== "default") {
      this.injectModeReminder();
    }

    // Advertise any deferred tools so the model knows they exist and how to
    // reach them. Keeps the per-turn tool list small; schemas load on demand.
    this.injectDeferredToolsReminder();
  }

  /**
   * Re-inject the deferred-tools advertisement. Hosts that reassign
   * agent.messages (session resume, Feishu conversation rebuild) drop the
   * constructor-injected meta reminder — without re-injection the model has
   * no way to discover deferred tools, since they are also filtered out of
   * the prompt's tool list.
   */
  injectDeferredToolsReminder(): void {
    const deferredNames = [...this.tools.values()]
      .filter((t) => t.deferred)
      .map((t) => t.name);
    if (deferredNames.length > 0) {
      this.injectSystemReminder(buildDeferredToolsReminder(deferredNames));
    }
  }

  /**
   * Rebinds the agent to a new session (TUI session switch). Keeps
   * ownerSessionId on newly spawned background tasks and managed servers
   * accurate (background-tasks design §2.2c).
   */
  setSessionID(sessionID: string | undefined): void {
    this.sessionID = sessionID;
    // The TUI reuses one Agent across session switches, so the subagent
    // persist directory must follow the session — otherwise children spawned
    // after a switch land in the OLD session's directory and the new
    // session's persisted children never load (known-defects #2).
    this.subagents.repointPersistDir(this.deriveSubagentPersistDir());
  }

  /**
   * Children persist next to the session file so a later process can resume
   * them via send_input (design §7). An explicit subagents.persistDir config
   * always wins over the sessionID derivation. Child agents themselves
   * (agentRole "subagent") never persist children — no recursion exists.
   */
  private deriveSubagentPersistDir(): string | undefined {
    if (this.agentRole !== "parent") return undefined;
    return this.subagentsConfig.persistDir
      ?? (this.sessionID?.endsWith(".jsonl") ? this.sessionID.replace(/\.jsonl$/, ".subagents") : undefined);
  }

  getSessionID(): string | undefined {
    return this.sessionID;
  }

  /**
   * Background-task reminder, state-change gated (design §2.3a): returns a
   * reminder only when the owned task set changed since the last emission
   * (started/finished/killed). The reminder channel is append-only, so
   * emitting on every model call while tasks are merely alive would stack
   * persistent duplicates that neither pruning nor compaction removes.
   */
  consumeBackgroundTaskReminder(): string | undefined {
    if (!this.backgroundTasks) return undefined;
    const version = this.backgroundTasks.version();
    if (version === this.lastTaskReminderVersion) return undefined;
    this.lastTaskReminderVersion = version;
    return buildTaskLifecycleReminder({
      tasks: this.backgroundTasks.list(),
      outputTail: (id) => this.backgroundTasks?.outputTail(id),
      toolsAvailable: this.unlockedDeferred?.has("task_output") ?? false,
    });
  }

  private async runExternalHook(
    request: HookRunRequest,
    abortSignal?: AbortSignal,
  ): Promise<{ result: HookCombinedResult; events: AgentEvent[] }> {
    const events: AgentEvent[] = [];
    if (!this.externalHooks) {
      return {
        result: {
          eventName: request.eventName,
          decision: "allow",
          modelContext: [],
          results: [],
          diagnostics: [],
          matched: 0,
        },
        events,
      };
    }
    const result = await this.externalHooks.runEvent({
      agentRole: this.agentRole,
      subAgentId: this.subAgentId,
      sessionId: this.sessionID,
      ...request,
    }, {
      abortSignal,
      onProgress: (event) => events.push(agentEventFromHookProgress(event)),
    });
    return { result, events };
  }

  private injectHookModelContext(result: HookCombinedResult): void {
    for (const context of result.modelContext) {
      this.injectSystemReminder(`[Hook ${result.eventName}] ${context}`);
    }
  }

  /** Whether a tool is registered on this agent (e.g. delegation tools on parents). */
  hasToolAvailable(name: string): boolean {
    return this.tools.has(name);
  }

  /** Unlock a list of deferred tools so they're included in subsequent turns. */
  unlockDeferredTools(names: string[]): void {
    for (const n of names) {
      if (this.tools.has(n)) this.unlockedDeferred.add(n);
    }
  }

  /** All deferred tools in this session (for tool_search to inspect). */
  listDeferredTools(): ToolRegistryEntry[] {
    return [...this.tools.values()].filter((t) => t.deferred);
  }

  getSystemPromptToolOptions(): Pick<import("./system-prompt.js").SystemPromptOptions, "tools" | "toolSnippets" | "guidelines" | "modelRoutingPrompt" | "memoryPrompt" | "skills" | "skillCatalogChars"> {
    return {
      // Rebuilt prompts (model switch, /skills changes) keep the Skills catalog.
      skills: this.skillSummaries,
      skillCatalogChars: this.skillCatalogChars,
      ...buildToolPromptOptions(this.getActiveToolEntries()),
      // Rendered through the live accessor (design §1.5), so every host-
      // triggered prompt rebuild picks up the current catalog.
      modelRoutingPrompt: this.buildModelRoutingPromptSection(),
      // Model switches rebuild the prompt from these options; without this the
      // persistent memory (buildMemoryPrompt) is silently dropped on switch.
      memoryPrompt: this.memoryPrompt,
    };
  }

  getSkillSummaries(): SkillSummary[] {
    return this.skillSummaries.slice();
  }

  getSkillCatalogChars(): number | undefined {
    return this.skillCatalogChars;
  }

  /** Refresh the live Skill catalog after /skills reloads or changes enablement; hosts then rebuild the system prompt. */
  setSkillSummaries(skills: SkillSummary[]): void {
    this.skillSummaries = skills.slice();
    this.notifyContextChanged();
  }

  /** Current routing menu (design §4); undefined when no accessor is wired. */
  buildModelRoutingPromptSection(): string | undefined {
    return this.router.promptSection();
  }

  /**
   * Routing menu rendered for a prospective parent route — used by model-
   * switch transactions to build the NEXT prompt before mutating the agent
   * (design §1.5).
   */
  renderModelRoutingPromptFor(parent: { providerId: string; model: string }): string | undefined {
    return this.router.promptSectionFor(parent);
  }

  /**
   * Push subscription for derived-context consumers (TUI footer, hosts).
   * Fires whenever resident context mutates in a way that changes a
   * `getContextUsageSnapshot()` reading: whole-array message rewrites,
   * appended messages, system-prompt replacement, or model/provider switches
   * (window size / estimation basis). Returns an unsubscribe function.
   */
  onContextChanged(listener: () => void): () => void {
    this.contextChangedListeners.add(listener);
    return () => {
      this.contextChangedListeners.delete(listener);
    };
  }

  private notifyContextChanged(): void {
    if (this.notifyingContextChange) return;
    this.notifyingContextChange = true;
    try {
      for (const listener of [...this.contextChangedListeners]) {
        try {
          listener();
        } catch (error) {
          // A broken subscriber must never corrupt agent state mid-mutation,
          // but leave a trace — otherwise a throwing consumer (e.g. the footer
          // readout hitting a pathological message) fails silently forever.
          traceEvent("context_listener_error", {
            error: error instanceof Error ? error.message : String(error),
          }, {
            sessionFile: this.sessionID,
            provider: this._providerId || "none",
            model: this.apiModel || "none",
          });
        }
      }
    } finally {
      this.notifyingContextChange = false;
    }
  }

  getContextUsageSnapshot(): ContextUsageSnapshot {
    return buildContextUsageSnapshot({
      providerId: this.providerId,
      modelId: this.apiModel,
      messages: this.messages,
      toolEntries: this.getActiveToolEntries(),
      deferredToolEntries: this.listDeferredTools(),
      skills: this.skillSummaries,
    });
  }

  resetContextUsageAnchor(): void {
    this.lastInputTokens = null;
    this.lastAnchorMessageCount = null;
    this.fileStateTracker?.invalidateReadHistory();
  }

  /** Whether a given tool is deferred and not yet unlocked. */
  isDeferredAndLocked(name: string): boolean {
    const tool = this.tools.get(name);
    return !!tool?.deferred && !this.unlockedDeferred.has(name);
  }

  private getActiveToolEntries(): ToolRegistryEntry[] {
    return [...this.tools.values()]
      .filter((tool) => !tool.deferred || this.unlockedDeferred.has(tool.name));
  }

  injectSystemReminder(content: string): void {
    this.appendMessage({ role: "meta", kind: "system-reminder", content });
  }

  injectModeReminder(): void {
    const reminder = reminderForMode(this._mode);
    const last = this.messages.at(-1);
    if (
      last?.role === "meta"
      && last.kind === "system-reminder"
      && last.content === reminder
      && last.includeInLlm !== false
    ) {
      return;
    }
    this.retireStaleModeReminders();
    this.injectSystemReminder(reminder);
  }

  /**
   * Mode reminders are enter-announcements with no cancel semantics, so a stale
   * one keeps describing a mode the session left long ago. Shift+Tab cycles
   * through plan on the way to bypassPermissions, which used to leave a live
   * "Plan mode is now ACTIVE" (plus its "call exit_plan_mode" rule) in history
   * for the rest of the session — the model then planned instead of building,
   * and hunted for a tool that is filtered out outside plan mode.
   *
   * Hidden rather than spliced: keeping the array shape intact leaves message
   * indices (usage anchors, compaction bookkeeping) valid, and the transcript
   * still shows what happened. Cache cost is one invalidated prefix per real
   * mode switch — the no-op path above keeps repeat injections free.
   */
  private retireStaleModeReminders(): void {
    for (const message of this.messages) {
      if (message.role !== "meta") continue;
      if (message.kind !== "system-reminder") continue;
      if (message.includeInLlm === false) continue;
      if (!isPermissionModeReminder(message.content)) continue;
      message.includeInLlm = false;
    }
  }

  get role(): "parent" | "subagent" {
    return this.agentRole;
  }

  get model(): string {
    return this._model;
  }

  set model(value: string) {
    if (this._model === value) return;
    this._model = value;
    // Context window (and thus usage readings) is model-derived.
    this.notifyContextChanged();
  }

  get providerId(): string {
    return this._providerId;
  }

  set providerId(value: string) {
    if (this._providerId === value) return;
    this._providerId = value;
    // Token estimation basis is provider-specific.
    this.notifyContextChanged();
  }

  get apiModel(): string {
    if (this._model.includes(":")) {
      return this._model.split(":").slice(1).join(":");
    }
    return this._model;
  }

  setProvider(provider: Provider) {
    this.provider = provider;
  }

  complete(
    messages: Message[],
    options?: { model?: string; temperature?: number; thinkingLevel?: ThinkingLevel; abortSignal?: AbortSignal },
  ): Promise<string> {
    return this.provider.complete(projectMessages(messages), {
      model: options?.model ?? this.apiModel,
      temperature: options?.temperature ?? this.temperature,
      thinkingLevel: options?.thinkingLevel ?? this.thinkingLevel,
      abortSignal: options?.abortSignal,
    });
  }

  get thinking(): ThinkingLevel {
    return this.thinkingLevel;
  }

  set thinking(value: ThinkingLevel) {
    this.thinkingLevel = value;
  }

  get reasoning(): ThinkingLevel {
    return this.thinkingLevel;
  }

  set reasoning(value: ThinkingLevel) {
    this.thinkingLevel = value;
  }

  get mode(): PermissionMode {
    return this._mode;
  }

  set mode(value: PermissionMode) {
    this.setMode(value);
  }

  setMode(value: PermissionMode): void {
    if (this._mode === value) return;
    this._mode = value;
    this._modeVersion += 1;
    this.injectModeReminder();
    this.onModeUpdate?.(value);
  }

  /** Internal: snapshot counter that bumps on every mode change. Used by run loop. */
  get modeVersion(): number {
    return this._modeVersion;
  }

  setSystemPrompt(prompt: string) {
    const systemMessage: Extract<Message, { role: "system" }> = { role: "system", content: prompt };
    if (this.messages[0]?.role === "system") {
      if (this.messages[0].content === prompt) return;
      this.messages[0] = systemMessage;
      this.notifyContextChanged();
      return;
    }
    this.messages.unshift(systemMessage);
    this.notifyContextChanged();
  }

  async *run(
    userInput: string | ContentPart[],
    cwd: string,
    options: AgentRunOptions = {},
  ): AsyncIterable<AgentEvent> {
    const abortSignal = options.abortSignal;
    const inputController = options.inputController;
    const traceContext = {
      cwd,
      sessionFile: this.sessionID,
      provider: this._providerId || "none",
      model: this.apiModel || "none",
    };
    const runId = randomUUID();
    const emit = (event: AgentEvent): AgentEvent => {
      traceEvent("agent_event", summarizeAgentEventForTrace(event), traceContext);
      return event;
    };
    traceEvent("agent_run_start", {
      input: summarizeTraceValue(userInput),
      mode: this._mode,
      messageCount: this.messages.length,
      toolCount: this.tools.size,
      deferredUnlocked: this.unlockedDeferred.size,
    }, traceContext);
    throwIfAborted(abortSignal);
    const hookBus = new HookBus();
    for (const hooks of createDefaultHooks()) {
      hookBus.register(hooks);
    }
    for (const hooks of this.hookDefinitions) {
      hookBus.register(hooks);
    }
    const hookState: TurnHookState = {};
    const reminderQueue: string[] = [];
    const queueReminder = (reminder: string) => {
      reminderQueue.push(reminder);
    };
    const pendingInputCount = () => inputController?.pendingInputCount() ?? 0;
    const applyPendingInputs = async (): Promise<AgentEvent[]> => {
      const pendingInputs = inputController?.drainPendingInputs() ?? [];
      if (pendingInputs.length === 0) return [];
      const events: AgentEvent[] = [];
      for (const input of pendingInputs) {
        const hook = await this.runExternalHook({
          eventName: "SteerInputApplied",
          cwd,
          runId,
          target: "current_turn",
          payload: {
            id: input.id,
            target: "current_turn",
            ...normalizeHookInput(input.content),
          },
          fullPayload: { prompt: input.content },
        }, abortSignal);
        events.push(...hook.events);
        this.injectHookModelContext(hook.result);
        this.appendMessage({ role: "user", content: input.content });
        // A steer is a new ask: the tests-touched disclosure must be able to
        // fire again for the follow-up's closing pass.
        hookState.testsTouchedDisclosed = false;
        events.push({
          type: "input_applied",
          id: input.id,
          content: input.content,
          target: "current_turn",
        });
      }
      events.push({ type: "input_pending_changed", pending: pendingInputCount() });
      return events;
    };
    const rejectPendingInputs = async (reason: "no_continuation"): Promise<AgentEvent[]> => {
      // Seal before the first await. A host can submit in reaction to the final
      // turn boundary; accepting after this drain would orphan that input.
      const pendingInputs: AgentRunInput[] = inputController?.closePendingInputs?.()
        ?? inputController?.drainPendingInputs()
        ?? [];
      if (pendingInputs.length === 0) return [];
      const events: AgentEvent[] = [];
      for (const input of pendingInputs) {
        const hook = await this.runExternalHook({
          eventName: "QueuedInputRejected",
          cwd,
          runId,
          target: "next_turn",
          payload: {
            id: input.id,
            reason,
            target: "next_turn",
            ...normalizeHookInput(input.content),
          },
          fullPayload: { prompt: input.content },
        }, abortSignal);
        events.push(...hook.events);
        this.injectHookModelContext(hook.result);
        events.push({
          type: "input_rejected",
          id: input.id,
          content: input.content,
          reason,
          target: "next_turn",
        });
      }
      events.push({ type: "input_pending_changed", pending: pendingInputCount() });
      return events;
    };
    const flushQueuedReminders = () => {
      for (const reminder of reminderQueue.splice(0, reminderQueue.length)) {
        this.injectSystemReminder(reminder);
      }
    };

    if (!options.resumeWithoutInput) {
      const promptHook = await this.runExternalHook({
        eventName: "UserPromptSubmit",
        cwd,
        runId,
        target: typeof userInput === "string" ? userInput : "content_parts",
        payload: normalizeHookInput(userInput),
        fullPayload: { prompt: userInput },
      }, abortSignal);
      for (const event of promptHook.events) yield emit(event);
      if (promptHook.result.decision === "deny") {
        const message = promptHook.result.reason
          ?? `Prompt blocked by hook ${promptHook.result.sourceHookId ?? "<unknown>"}.`;
        yield emit({ type: "turn_start" });
        yield emit({ type: "text_delta", content: message });
        yield emit({ type: "turn_end", willContinue: false });
        for (const event of await rejectPendingInputs("no_continuation")) yield emit(event);
        yield emit({ type: "agent_end" });
        return;
      }
      this.injectHookModelContext(promptHook.result);
      this.appendMessage({
        role: "user",
        content: userInput,
        ...(options.userMessageUi ? { ui: { ...options.userMessageUi } } : {}),
      });
    }
    await hookBus.runBeforeTurn({
      agent: this,
      cwd,
      input: userInput,
      state: hookState,
      queueReminder,
      flushReminders: flushQueuedReminders,
    });
    flushQueuedReminders();

    let consecutiveOverflowRecoveries = 0;
    let consecutiveEmptyAssistantRecoveries = 0;
    let consecutiveStreamInterruptionRetries = 0;
    let step = 0;
    let autoServersStopped = false;
    const stopOwnedAutoServers = async () => {
      if (autoServersStopped) return;
      autoServersStopped = true;
      await stopAutoServersForSession(this.sessionID);
    };

    let currentAssistantMsg: Extract<Message, { role: "assistant" }> | undefined;
    let currentAssistantAppended = false;

    try {
      while (true) {
      throwIfAborted(abortSignal);
      flushQueuedReminders();
      // Background child completions surface before the next inference turn
      // without requiring a wait_agent call (design §5).
      for (const notice of this.subagents.drainIngestionNotices()) {
        this.injectSystemReminder(notice);
      }
      this.flushWorkflowDeliveries();
      for (const update of this.subagents.drainToolUpdates()) yield emit(update);
      for (const event of await applyPendingInputs()) yield emit(event);
      yield emit({ type: "turn_start" });
      step += 1;
      (hookState as any).turnCount = step;
      if (this.taskBudget) {
        (hookState as any).taskBudget = {
          total: this.taskBudget.total,
          spent: (hookState as any).taskBudget?.spent ?? 0,
        };
      }
      let forceTextOnlyReason = (hookState as any).forceTextOnlyReason as string | undefined;
      if (!forceTextOnlyReason && this.maxTurns !== undefined && step >= this.maxTurns) {
        forceTextOnlyReason = "The configured maximum turns for this agent have been reached.";
        (hookState as any).forceTextOnlyReason = forceTextOnlyReason;
      }
      if (forceTextOnlyReason) {
        this.injectSystemReminder(buildToolFreezeReminder(forceTextOnlyReason));
      }

      const assistantMsg: Extract<Message, { role: "assistant" }> = {
        role: "assistant",
        content: "",
        reasoning: "",
        toolCalls: [],
        model: this._model,
        providerId: this.providerId,
        modelId: this.apiModel,
      };

      const streamingToolCalls = new Map<string, { id: string; name: string; args: string; argsCorrupt?: boolean }>();
      const textSanitizer = createStreamingInternalReminderSanitizer();
      const reasoningSanitizer = createStreamingInternalReminderSanitizer();
      let turnUsage: TokenUsage | undefined;
      let turnSystemFingerprint: string | undefined;
      let assistantAppended = false;
      currentAssistantMsg = assistantMsg;
      currentAssistantAppended = false;

      let toolEntries = Array.from(this.tools.values())
        .filter((t) => !t.deferred || this.unlockedDeferred.has(t.name))
        .filter((t) => t.enabled?.() !== false);
      const beforeModelCallCtx = {
        agent: this,
        cwd,
        input: userInput,
        state: hookState,
        queueReminder,
        flushReminders: flushQueuedReminders,
        toolEntries,
        disableTools: (reason: string) => {
          (hookState as any).forceTextOnlyReason = reason;
        },
      };
      await hookBus.runBeforeModelCall(beforeModelCallCtx);
      toolEntries = beforeModelCallCtx.toolEntries;
      const preModelHook = await this.runExternalHook({
        eventName: "PreModelCall",
        cwd,
        runId,
        target: this.apiModel,
        payload: {
          providerId: this.providerId,
          model: this.apiModel,
          mode: this._mode,
          toolCount: toolEntries.length,
          ...normalizeHookInput(userInput),
        },
        fullPayload: { prompt: userInput },
      }, abortSignal);
      for (const event of preModelHook.events) yield emit(event);
      this.injectHookModelContext(preModelHook.result);
      flushQueuedReminders();
      const textOnly = !!(hookState as any).forceTextOnlyReason;
      const toolDefinitions: ToolDefinition[] = toolEntries
        .map((t) => ({
          name: t.name,
          description: t.description,
          parameters: t.parameters,
        }));
      const toolDefinitionTokens = estimateToolDefinitionsTokens(toolDefinitions, this.providerId);
      let currentRequestMessageCount = this.messages.length;
      const persistTerminalProviderError = (error: unknown) => this.persistProviderError(error, {
        messageCount: currentRequestMessageCount,
        toolCount: toolDefinitions.length,
      });

      // LLM-driven compaction runs ahead of projector's algorithmic passes. If
      // it succeeds, this.messages is replaced with [preserved system+meta] +
      // [LLM summary] + [last user msg], and the projector becomes a no-op for
      // budget. If it fails (network error, etc.), the projector's existing
      // algorithmic fallback still kicks in.
      for await (const event of this.maybeCompactWithLLM(toolDefinitionTokens, abortSignal)) yield emit(event);
      for (const event of this.contextEvents.splice(0)) yield emit(event);

      const bufferedStreamingToolCallIds = new Set<string>();
      let currentRequestEstimate: number | undefined;
      let currentContextWindow: number | undefined;
      try {
        markStableCurrentToolResultsForCache(this.messages);
        const projectedMessages = projectMessages(this.messages, {
          // Semantic compaction is committed above; projection only renders/thins.
          mode: "pruned",
          providerId: this.providerId,
          modelId: this.apiModel,
          // Projection changes message identity/count (meta rendering, pruning,
          // chain repair), so a resident-array anchor index is not safe here.
          // A fresh estimate also lets us account for current tool schemas.
          additionalInputTokens: toolDefinitionTokens,
        });
        currentRequestMessageCount = projectedMessages.length;
        const requestBudget = getContextBudget(
          this.providerId,
          this.apiModel,
          projectedMessages,
          { additionalInputTokens: toolDefinitionTokens },
        );
        currentRequestEstimate = requestBudget.estimatedTokens;
        currentContextWindow = requestBudget.contextWindow;
        yield emit({ type: "context_usage", usedTokens: currentRequestEstimate, contextWindow: currentContextWindow, estimated: true });
        const maxInputTokens = getMaxInputTokens(requestBudget.contextWindow);
        if (maxInputTokens !== undefined && currentRequestEstimate > maxInputTokens) {
          throw new LocalContextPreflightError(currentRequestEstimate, maxInputTokens);
        }
        const providerStartedAt = Date.now();
        let streamTextChars = 0;
        let streamReasoningChars = 0;
        let streamToolCallDeltas = 0;
        traceEvent("provider_stream_start", {
          residentMessageCount: this.messages.length,
          projectedMessageCount: projectedMessages.length,
          toolCount: toolDefinitions.length,
          thinkingLevel: this.thinkingLevel,
          mode: this._mode,
          requestFingerprint: buildProviderRequestFingerprint(
            projectedMessages,
            toolDefinitions,
            this.providerId,
            toolDefinitions.length > 0 ? (textOnly ? "none" : "auto") : undefined,
          ),
        }, traceContext);
        const stream = this.provider.streamChat(projectedMessages, {
          model: this.apiModel,
          tools: toolDefinitions,
          toolChoice: toolDefinitions.length > 0 ? (textOnly ? "none" : "auto") : undefined,
          temperature: this.temperature,
          thinkingLevel: this.thinkingLevel,
          abortSignal,
          rateLimitPolicy: this.rateLimitPolicy,
        });

        for await (const chunk of stream) {
          throwIfAborted(abortSignal);
          switch (chunk.type) {
            case "text":
              {
                const sanitizedDelta = textSanitizer.push(chunk.content);
                if (sanitizedDelta) {
                  assistantMsg.content += sanitizedDelta;
                  streamTextChars += sanitizedDelta.length;
                  yield emit({ type: "text_delta", content: sanitizedDelta });
                }
              }
              break;
            case "reasoning_delta":
              {
                const sanitizedDelta = reasoningSanitizer.push(chunk.content);
                if (sanitizedDelta) {
                  debugReasoningStream({
                    stage: "agent_receive",
                    providerId: this._providerId,
                    modelId: this.apiModel,
                    turnStep: step,
                    beforeLength: assistantMsg.reasoning?.length ?? 0,
                    delta: summarizeDebugText(sanitizedDelta),
                    afterLength: (assistantMsg.reasoning?.length ?? 0) + sanitizedDelta.length,
                  });
                  assistantMsg.reasoning = (assistantMsg.reasoning || "") + sanitizedDelta;
                  streamReasoningChars += sanitizedDelta.length;
                  yield emit({ type: "reasoning_delta", content: sanitizedDelta });
                }
              }
              break;

            case "provider_content_block":
              appendProviderContentBlock(assistantMsg, chunk.provider, chunk.block);
              break;

            case "tool_call":
              // `toolChoice: "none"` is a governance boundary, not merely a
              // provider hint. Ignore any provider-side violation so a forced
              // text-only turn can never reach the tool execution path.
              if (textOnly) {
                traceEvent("text_only_tool_call_ignored", {
                  id: chunk.id,
                  name: chunk.name,
                  isStart: chunk.isStart,
                  isEnd: chunk.isEnd,
                }, traceContext);
                break;
              }
              if (chunk.isStart) {
                streamingToolCalls.set(chunk.id, { id: chunk.id, name: chunk.name, args: "" });
                if (!bufferedStreamingToolCallIds.has(chunk.id)) {
                  yield emit({ type: "tool_call_start", id: chunk.id, name: chunk.name });
                }
              }
              if (!streamingToolCalls.has(chunk.id)) {
                streamingToolCalls.set(chunk.id, { id: chunk.id, name: chunk.name, args: "" });
              }
              const currentToolCall = streamingToolCalls.get(chunk.id);
              if (currentToolCall) {
                currentToolCall.name = chunk.name || currentToolCall.name;
                currentToolCall.args += chunk.arguments;
                if (chunk.argumentsFull !== undefined) {
                  currentToolCall.args = chunk.argumentsFull;
                }
                if (chunk.argumentsCorrupt) {
                  currentToolCall.argsCorrupt = true;
                }
                if (chunk.arguments) {
                  streamToolCallDeltas += 1;
                  if (!bufferedStreamingToolCallIds.has(chunk.id)) {
                    yield emit({
                      type: "tool_call_delta",
                      id: currentToolCall.id,
                      name: currentToolCall.name,
                      argumentsDelta: chunk.arguments,
                      arguments: currentToolCall.args,
                    });
                  }
                }
              }
              if (chunk.isEnd && currentToolCall) {
                assistantMsg.toolCalls!.push({
                  id: currentToolCall.id,
                  name: currentToolCall.name,
                  arguments: currentToolCall.args,
                  ...(currentToolCall.argsCorrupt ? { argsCorrupt: true } : {}),
                });
                if (!bufferedStreamingToolCallIds.has(chunk.id)) {
                  yield emit({
                    type: "tool_call_end",
                    id: currentToolCall.id,
                    name: currentToolCall.name,
                    arguments: currentToolCall.args,
                  });
                }
                streamingToolCalls.delete(chunk.id);
              }
              break;

            case "usage":
              turnUsage = chunk.usage;
              assistantMsg.usage = chunk.usage;
              this.budgetLedger?.recordUsage(chunk.usage, this.budgetSource);
              this.lastInputTokens = chunk.usage.promptTokens;
              this.lastAnchorMessageCount = this.messages.length;
              if ((hookState as any).taskBudget) {
                (hookState as any).taskBudget.spent += chunk.usage.promptTokens + chunk.usage.completionTokens;
                if ((hookState as any).taskBudget.spent >= (hookState as any).taskBudget.total) {
                  (hookState as any).forceTextOnlyReason = "The configured task budget for this agent has been exhausted.";
                }
              }
              break;

            case "response_metadata":
              turnSystemFingerprint = chunk.systemFingerprint;
              assistantMsg.systemFingerprint = chunk.systemFingerprint;
              break;
          }
          for (const update of this.subagents.drainToolUpdates()) yield emit(update);
        }
        const flushedText = textSanitizer.flush();
        if (flushedText) {
          assistantMsg.content += flushedText;
          streamTextChars += flushedText.length;
          yield emit({ type: "text_delta", content: flushedText });
        }

        const flushedReasoning = reasoningSanitizer.flush();
        if (flushedReasoning) {
          debugReasoningStream({
            stage: "agent_receive_flush",
            providerId: this._providerId,
            modelId: this.apiModel,
            turnStep: step,
            beforeLength: assistantMsg.reasoning?.length ?? 0,
            delta: summarizeDebugText(flushedReasoning),
            afterLength: (assistantMsg.reasoning?.length ?? 0) + flushedReasoning.length,
          });
          assistantMsg.reasoning = (assistantMsg.reasoning || "") + flushedReasoning;
          streamReasoningChars += flushedReasoning.length;
          yield emit({ type: "reasoning_delta", content: flushedReasoning });
        }
        traceEvent("provider_stream_end", {
          elapsedMs: Date.now() - providerStartedAt,
          textChars: streamTextChars,
          reasoningChars: streamReasoningChars,
          toolCallDeltas: streamToolCallDeltas,
          toolCalls: assistantMsg.toolCalls?.length ?? 0,
          usage: turnUsage,
        }, traceContext);

        throwIfAborted(abortSignal);
        const assistantHasContent = assistantMsg.content.trim().length > 0;
        const assistantHasToolCalls = !!assistantMsg.toolCalls && assistantMsg.toolCalls.length > 0;
        if (!assistantHasContent && !assistantHasToolCalls) {
          if (consecutiveEmptyAssistantRecoveries < MAX_EMPTY_ASSISTANT_RECOVERIES) {
            consecutiveEmptyAssistantRecoveries += 1;
            this.injectSystemReminder(EMPTY_ASSISTANT_RECOVERY_REMINDER);
            yield emit({ type: "turn_end", usage: turnUsage, systemFingerprint: turnSystemFingerprint, willContinue: true });
            continue;
          }

          assistantMsg.content = EMPTY_ASSISTANT_FALLBACK;
          assistantMsg.reasoning = "";
          yield emit({ type: "text_delta", content: assistantMsg.content });
        }

        this.appendMessage(assistantMsg);
        assistantAppended = true;
        currentAssistantAppended = true;
      } catch (error) {
        traceEvent("provider_stream_error", {
          error: summarizeTraceError(error),
        }, traceContext);
        if (assistantAppended) {
          if (!isAbortLikeError(error, abortSignal)) persistTerminalProviderError(error);
          throw error;
        }
        const streamInterruption = isProviderStreamInterruption(error) ? error : undefined;
        const streamRetryLimit = streamInterruption
          ? providerStreamRetryLimit(streamInterruption)
          : 0;
        if (
          streamInterruption
          && !isAbortLikeError(error, abortSignal)
          && consecutiveStreamInterruptionRetries < streamRetryLimit
        ) {
          // The provider stream died after partial content. The half-built
          // assistantMsg was never appended to this.messages, and the next
          // turn_start resets the streaming display, so re-issuing the whole
          // request is safe.
          consecutiveStreamInterruptionRetries += 1;
          this.persistProviderError(error, {
            messageCount: currentRequestMessageCount,
            toolCount: toolDefinitions.length,
            retry: { attempt: consecutiveStreamInterruptionRetries, maxAttempts: streamRetryLimit },
          });
          yield emit({
            type: "provider_retry",
            attempt: consecutiveStreamInterruptionRetries,
            maxAttempts: streamRetryLimit,
            reason: streamInterruption.message,
          });
          await sleepBeforeRetry(
            computeRetryDelayMs(consecutiveStreamInterruptionRetries, {
              retryAfterMs: streamInterruption.retryAfterMs,
            }),
            abortSignal,
          ).catch(() => undefined);
          continue;
        }
        if (!isContextOverflowError(error)) {
          if (!isAbortLikeError(error, abortSignal) && shouldAppendModelInterruptedBoundary(this.messages)) {
            this.appendMessage(createModelInterruptedMessage(error, {
              model: this._model,
              providerId: this.providerId,
              modelId: this.apiModel,
            }));
            assistantAppended = true;
          }
          if (!isAbortLikeError(error, abortSignal)) persistTerminalProviderError(error);
          throw error;
        }
        if (consecutiveOverflowRecoveries >= MAX_CONSECUTIVE_OVERFLOW_RECOVERIES) {
          persistTerminalProviderError(error);
          throw error;
        }
        const messagesBeforeRecovery = this.messages;
        const revisionBeforeRecovery = this.getContextRevision?.();
        const failedRequestEstimate = currentRequestEstimate ?? getContextBudget(
          this.providerId,
          this.apiModel,
          projectMessages(this.messages, {
            mode: "pruned",
            providerId: this.providerId,
            modelId: this.apiModel,
            additionalInputTokens: toolDefinitionTokens,
          }),
          { additionalInputTokens: toolDefinitionTokens },
        ).estimatedTokens;
        const candidate = await this.recoverFromOverflow(
          consecutiveOverflowRecoveries,
          error instanceof LocalContextPreflightError ? 0.999_999 : 0.9,
          abortSignal,
        );
        if (!candidate) { persistTerminalProviderError(error); throw error; }
        const droppedMessages = messagesBeforeRecovery.length - candidate.length;
        const recoveredRequestEstimate = getContextBudget(
          this.providerId,
          this.apiModel,
          projectMessages(candidate, {
            mode: "pruned",
            providerId: this.providerId,
            modelId: this.apiModel,
            additionalInputTokens: toolDefinitionTokens,
          }),
          { additionalInputTokens: toolDefinitionTokens },
        ).estimatedTokens;
        // Never issue a retry that is the same size or larger. Apart from
        // avoiding a useless retry loop, this proves that already-completed
        // tool calls stay historical data rather than being executed again.
        const madeProgress = recoveredRequestEstimate < failedRequestEstimate;
        const meaningfulProviderProgress = error instanceof LocalContextPreflightError
          || recoveredRequestEstimate <= Math.floor(failedRequestEstimate * 0.9);
        if (!madeProgress || !meaningfulProviderProgress) {
          persistTerminalProviderError(error);
          throw error;
        }
        this.applyContextCheckpoint(candidate, "overflow", undefined, revisionBeforeRecovery, abortSignal);
        consecutiveOverflowRecoveries += 1;
        this.compactionStats.overflow += 1;
        this.compactionStats.fired += 1;
        this.compactionStats.droppedMessages += droppedMessages;
        traceEvent("compaction_fired", {
          path: "overflow",
          droppedMessages,
          beforeEstimatedTokens: failedRequestEstimate,
          afterEstimatedTokens: recoveredRequestEstimate,
        });
        yield emit({ type: "context_recovered", droppedMessages, reason: "overflow" });
        yield emit({ type: "context_compaction", status: "completed", compactionId: this.lastCompactionId, persisted: !!this.onContextCheckpoint, preTokens: failedRequestEstimate, postTokens: recoveredRequestEstimate, contextWindow: currentContextWindow });
        continue;
      }

      consecutiveOverflowRecoveries = 0;
      consecutiveEmptyAssistantRecoveries = 0;
      consecutiveStreamInterruptionRetries = 0;

      if (turnUsage && Number.isFinite(turnUsage.promptTokens)) {
        // Provider prompt usage already includes cached input; never add cache hits twice.
        yield emit({ type: "context_usage", usedTokens: turnUsage.promptTokens + (turnUsage.completionTokens || 0), contextWindow: currentContextWindow, estimated: false });
      }
      // Execute tools if any
      if (assistantMsg.toolCalls && assistantMsg.toolCalls.length > 0) {
        const parsedCalls: ParsedToolCall[] = [];
        for (let index = 0; index < assistantMsg.toolCalls.length; index++) {
          const tc = assistantMsg.toolCalls[index];
          try {
            parsedCalls.push({
              ...tc,
              parsedArgs: JSON.parse(tc.arguments),
              ...(tc.argsCorrupt ? { argsCorrupt: true } : {}),
            });
          } catch {
            parsedCalls.push({ ...tc, parsedArgs: {}, argsCorrupt: true });
          }
        }
        const executedResults: ToolResult[] = [];
        const appendCancelledToolMessages = (startIndex: number) => {
          for (let pendingIndex = startIndex; pendingIndex < parsedCalls.length; pendingIndex++) {
            const pending = parsedCalls[pendingIndex];
            const pendingResult = cancelledToolResult(pending.name);
            this.appendMessage({
              role: "tool",
              toolCallId: pending.id,
              content: pendingResult.content,
              metadata: pendingResult.metadata,
              isError: pendingResult.isError,
            });
            executedResults.push(pendingResult);
          }
        };
        for (let index = 0; index < parsedCalls.length; index++) {
          if (abortSignal?.aborted) {
            appendCancelledToolMessages(index);
            throwIfAborted(abortSignal);
          }
          let tc = parsedCalls[index];
          let blockedResult: ToolResult | undefined;
          // run_workflow must be the only tool call in its response: it starts
          // a background orchestration whose result lands next turn, and
          // sibling calls racing it defeat the serialized fan-out contract.
          if (tc.name === "run_workflow" && parsedCalls.length > 1) {
            blockedResult = {
              content: [
                "run_workflow must be the only tool call in your response.",
                "Re-issue run_workflow alone — one call, nothing else in the same message — and run other tools after it returns.",
              ].join(" "),
              isError: true,
              status: "blocked",
            };
          }
          await hookBus.runBeforeToolCall({
            agent: this,
            cwd,
            input: userInput,
            state: hookState,
            queueReminder,
            flushReminders: flushQueuedReminders,
            toolCall: tc,
            blockedResult,
            replaceToolCall: (toolCall) => {
              tc = toolCall;
            },
            blockToolCall: (result) => {
              blockedResult = result;
            },
          });
          const preToolHook = await this.runExternalHook({
            eventName: "PreToolUse",
            cwd,
            runId,
            target: tc.name,
            payload: {
              id: tc.id,
              name: tc.name,
              argsPreview: truncateHookText(tc.arguments, 1000),
            },
            fullPayload: {
              toolArgs: tc.parsedArgs,
              toolArguments: tc.arguments,
            },
          }, abortSignal);
          for (const event of preToolHook.events) yield emit(event);
          this.injectHookModelContext(preToolHook.result);
          if (preToolHook.result.decision === "deny") {
            blockedResult = {
              content: preToolHook.result.reason
                ?? `Tool call blocked by hook ${preToolHook.result.sourceHookId ?? "<unknown>"}.`,
              isError: true,
              metadata: {
                hook: {
                  eventName: "PreToolUse",
                  hookId: preToolHook.result.sourceHookId,
                },
              },
            };
          }
          assistantMsg.toolCalls[index] = {
            id: tc.id,
            name: tc.name,
            arguments: tc.arguments,
          };
          flushQueuedReminders();
          if (bufferedStreamingToolCallIds.has(tc.id) && !isHiddenToolResult(blockedResult)) {
            yield emit({ type: "tool_call_start", id: tc.id, name: tc.name });
            if (tc.arguments) {
              yield emit({
                type: "tool_call_delta",
                id: tc.id,
                name: tc.name,
                argumentsDelta: tc.arguments,
                arguments: tc.arguments,
              });
            }
            yield emit({ type: "tool_call_end", id: tc.id, name: tc.name, arguments: tc.arguments });
          }
          if (isHiddenToolResult(blockedResult)) {
            let result = normalizeToolResultForModel(
              blockedResult,
              this.providerId,
              this.apiModel,
            );
            await hookBus.runAfterToolCall({
              agent: this,
              cwd,
              input: userInput,
              state: hookState,
              queueReminder,
              flushReminders: flushQueuedReminders,
              toolCall: tc,
              result,
              replaceResult: (next) => {
                result = next;
              },
            });
            result = normalizeToolResultForModel(result, this.providerId, this.apiModel);
            const postToolHook = await this.runExternalHook({
              eventName: result.isError ? "PostToolUseFailure" : "PostToolUse",
              cwd,
              runId,
              target: tc.name,
              payload: {
                id: tc.id,
                name: tc.name,
                argsPreview: truncateHookText(tc.arguments, 1000),
                resultPreview: truncateHookText(result.content, 1000),
                isError: result.isError === true,
              },
              fullPayload: {
                toolArgs: tc.parsedArgs,
                toolArguments: tc.arguments,
                toolResult: result,
              },
            }, abortSignal);
            for (const event of postToolHook.events) yield emit(event);
            this.injectHookModelContext(postToolHook.result);
            traceEvent("speculative_read_blocked", {
              id: tc.id,
              name: tc.name,
              args: summarizeTraceValue(tc.parsedArgs),
              result: summarizeTraceToolResult(result),
            }, traceContext);
            this.appendMessage({
              role: "tool",
              toolCallId: tc.id,
              content: result.content,
              metadata: result.metadata,
              isError: result.isError,
            });
            executedResults.push(result);
            flushQueuedReminders();
            continue;
          }
          const toolStartedAt = Date.now();
          traceEvent("tool_execute_start", {
            id: tc.id,
            name: tc.name,
            args: summarizeTraceValue(tc.parsedArgs),
            argsCorrupt: tc.argsCorrupt,
          }, traceContext);
          yield emit({ type: "tool_start", id: tc.id, name: tc.name, args: tc.parsedArgs });
          const modeVersionBefore = this._modeVersion;
          const updateQueue = createUpdateQueue<ToolUpdate>();
          let result: ToolResult;
          if (blockedResult) {
            result = blockedResult;
          } else {
            const toolExecution = this.executeTool(tc, cwd, abortSignal, (update) => updateQueue.push(update));
            let settled = false;
            let cancelledByAbort = false;
            let resolved: ToolResult | undefined;
            let rejected: unknown;
            void toolExecution
              .then((value) => {
                resolved = value;
              })
              .catch((error) => {
                rejected = error;
              })
              .finally(() => {
                settled = true;
                updateQueue.wake();
              });

            const unsubscribe = this.subagents.subscribe(updateQueue.wake);
            try {
              while (!settled || updateQueue.hasItems() || this.subagents.hasPendingUpdates()) {
                for (const update of updateQueue.drain()) {
                  yield emit({ type: "tool_update", id: tc.id, name: tc.name, update });
                }
                for (const update of this.subagents.drainToolUpdates()) yield emit(update);
                // A wake() that fires while this generator is suspended at a
                // yield above finds no parked waiter. The queue latches such a
                // wake, and this synchronous re-check covers the same case from
                // the other side — an update pushed during the yield must not
                // stall until the next unrelated wake or the tool settles.
                if (!settled && !this.subagents.hasPendingUpdates()) {
                  const waitStatus = await updateQueue.wait(abortSignal);
                  if (waitStatus === "aborted" && !settled) {
                    cancelledByAbort = true;
                    break;
                  }
                }
              }
            } finally {
              unsubscribe();
            }
            if (cancelledByAbort) {
              result = cancelledToolResult(tc.name);
            } else {
              if (rejected) throw rejected;
              result = resolved ?? { content: `Error: Tool "${tc.name}" returned no result`, isError: true };
            }
          }
          // Hooks receive a bounded canonical result. This prevents a tool
          // from handing an unbounded multi-megabyte value to PostToolUse.
          result = normalizeToolResultForModel(result, this.providerId, this.apiModel);
          await hookBus.runAfterToolCall({
            agent: this,
            cwd,
            input: userInput,
            state: hookState,
            queueReminder,
            flushReminders: flushQueuedReminders,
            toolCall: tc,
            result,
            replaceResult: (next) => {
              result = next;
            },
          });
          // A hook is allowed to replace the result, so enforce the invariant
          // again before persistence, callbacks, UI events and provider reuse.
          result = normalizeToolResultForModel(result, this.providerId, this.apiModel);
          const postToolHook = await this.runExternalHook({
            eventName: result.isError ? "PostToolUseFailure" : "PostToolUse",
            cwd,
            runId,
            target: tc.name,
            payload: {
              id: tc.id,
              name: tc.name,
              argsPreview: truncateHookText(tc.arguments, 1000),
              resultPreview: truncateHookText(result.content, 1000),
              isError: result.isError === true,
            },
            fullPayload: {
              toolArgs: tc.parsedArgs,
              toolArguments: tc.arguments,
              toolResult: result,
            },
          }, abortSignal);
          for (const event of postToolHook.events) yield emit(event);
          this.injectHookModelContext(postToolHook.result);
          // Honor the model's server-declared per-tool-output token cap (e.g.
          // gpt-5.5 reports 10000). Without this, 4-5 large file reads in a row
          // blow past the input window even though our local estimate looks fine.
          const truncatedOutput = truncateToolOutputForModel(
            result.content,
            this.providerId,
            this.apiModel,
          );
          traceEvent("tool_execute_end", {
            id: tc.id,
            name: tc.name,
            elapsedMs: Date.now() - toolStartedAt,
            result: summarizeTraceToolResult(result),
            outputTruncation: {
              truncated: truncatedOutput.truncated,
              originalTokens: truncatedOutput.originalTokens,
              finalTokens: truncatedOutput.finalTokens,
              originalBytes: truncatedOutput.originalBytes,
              finalBytes: truncatedOutput.finalBytes,
              limit: truncatedOutput.limit,
              hardByteLimit: truncatedOutput.hardByteLimit,
            },
          }, traceContext);
          const toolMessage: ToolMessage = {
            role: "tool",
            toolCallId: tc.id,
            content: truncatedOutput.content,
            metadata: result.metadata,
            isError: result.isError,
          };
          // Stamp before appending: appendMessage persists to the session log
          // synchronously, so a mark added on a later turn never reaches disk.
          markToolResultCacheStable(tc.name, toolMessage);
          this.appendMessage(toolMessage);
          flushQueuedReminders();
          this.onToolResult?.(tc.name, result);
          executedResults.push(result);
          yield emit({ type: "tool_end", id: tc.id, name: tc.name, result });
          for (const event of this.contextEvents.splice(0)) yield emit(event);
          for (const update of this.subagents.drainToolUpdates()) yield emit(update);
          if (this._modeVersion !== modeVersionBefore) {
            yield emit({ type: "mode_changed", mode: this._mode });
          }
          if (abortSignal?.aborted) {
            appendCancelledToolMessages(index + 1);
            throwIfAborted(abortSignal);
          }
        }

        const imageObservation = toolImageObservation(executedResults);
        if (imageObservation) this.appendMessage(imageObservation);

        // A checkpoint must contain the whole batch, never provider repair's
        // placeholder for a sibling that has not executed yet.
        this.compactResidentHistory();
        for (const event of this.contextEvents.splice(0)) yield emit(event);

        await hookBus.runBeforeContinuation({
          agent: this,
          cwd,
          input: userInput,
          state: hookState,
          queueReminder,
          flushReminders: flushQueuedReminders,
          toolCalls: parsedCalls,
          toolResults: executedResults,
          requestTextOnlyTurn: (reason: string) => {
            (hookState as any).forceTextOnlyReason = reason;
          },
        });
        flushQueuedReminders();

        yield emit({ type: "turn_end", usage: turnUsage, systemFingerprint: turnSystemFingerprint, willContinue: true });

        // Auto-continue: if we have tool results, the LLM needs to respond to them.
        // Emitting the turn boundary keeps UI renderers aligned with the persisted
        // assistant/tool message sequence instead of merging the next answer into
        // the tool-call turn.
        continue;
      }

      await hookBus.runAfterTurn({
        agent: this,
        cwd,
        input: userInput,
        state: hookState,
        queueReminder,
        flushReminders: flushQueuedReminders,
      });
      flushQueuedReminders();
      const stopHook = await this.runExternalHook({
        eventName: "Stop",
        cwd,
        runId,
        target: "turn",
        payload: {
          providerId: this.providerId,
          model: this.apiModel,
          mode: this._mode,
          assistantChars: assistantMsg.content.length,
          toolCalls: assistantMsg.toolCalls?.length ?? 0,
        },
      }, abortSignal);
      for (const event of stopHook.events) yield emit(event);
      const willContinue = !!(hookState as any).forceContinuationReason
        || (options.continueOnPendingInput === true && pendingInputCount() > 0);
      yield emit({ type: "turn_end", usage: turnUsage, systemFingerprint: turnSystemFingerprint, willContinue });
      if (willContinue) {
        delete (hookState as any).forceContinuationReason;
        continue;
      }
      // A follow-up can arrive while the consumer handles turn_end. Recheck
      // synchronously before closing admission so an accepted input is not lost.
      if (options.continueOnPendingInput && pendingInputCount() > 0) continue;
      for (const event of await rejectPendingInputs("no_continuation")) yield emit(event);
      break;
    }

      for (const update of this.subagents.drainToolUpdates()) yield emit(update);
      await stopOwnedAutoServers();
      yield emit({ type: "agent_end" });
    } catch (error) {
      if (isAbortError(error, abortSignal)) {
        const appendedBoundary = this.appendInterruptedAssistantBoundary(
          currentAssistantMsg,
          currentAssistantAppended,
        );
        traceEvent("agent_run_interrupted", {
          appendedBoundary,
          messageCount: this.messages.length,
        }, traceContext);
      } else {
        const stopFailureHook = await this.runExternalHook({
          eventName: "StopFailure",
          cwd,
          runId,
          target: "run_error",
          payload: {
            error: summarizeTraceError(error),
          },
        }, abortSignal);
        for (const event of stopFailureHook.events) yield emit(event);
      }
      throw error;
    } finally {
      await stopOwnedAutoServers();
      traceEvent("agent_run_end", {
        messageCount: this.messages.length,
      }, traceContext);
    }
  }

  private async recoverFromOverflow(attempt: number, maximumSizeRatio = 0.999_999, abortSignal?: AbortSignal): Promise<Message[] | undefined> {
    const originalMessages = this.messages;
    const beforeBytes = serializedMessageBytes(originalMessages);
    const smaller = (candidate: Message[] | undefined): candidate is Message[] =>
      !!candidate && serializedMessageBytes(candidate) <= Math.floor(beforeBytes * maximumSizeRatio);
    throwIfAborted(abortSignal);
    if (attempt === 0) {
      const pruned = aggressivePruneMessages(originalMessages);
      if (smaller(pruned)) return pruned;
    }

    // Unlike ordinary pruning, overflow recovery is allowed to thin the most
    // recent completed tool batch. Keeping it protected is exactly what made
    // a one-turn 6MB grep result impossible to recover from.
    const emergencyToolLimit = attempt === 0 ? 16 * 1024 : 4 * 1024;
    const thinnedLatestTools = originalMessages.map((message): Message => {
      if (message.role !== "tool") return message;
      const bounded = truncateToolOutputForModel(
        message.content,
        this.providerId,
        this.apiModel,
        { hardByteLimit: emergencyToolLimit },
      );
      return bounded.truncated ? { ...message, content: bounded.content } : message;
    });
    if (smaller(thinnedLatestTools)) return thinnedLatestTools;

    const keepRecentTurns = attempt >= 1 ? 1 : 2;
    // Use the same budget-aware summarizer for single- and multi-turn history.
    const { compactWithLLM } = await import("./context/llm-compactor.js");
    const singleTurnResult = await compactWithLLM(originalMessages, {
      provider: this.provider,
      modelId: this.apiModel,
      providerId: this.providerId,
      abortSignal,
    });
    throwIfAborted(abortSignal);
    if (singleTurnResult.compacted && smaller(singleTurnResult.messages)) return singleTurnResult.messages;

    const fallback = compactMessages(originalMessages, { keepRecentTurns });
    if (fallback.compacted && smaller(fallback.messages)) return fallback.messages;

    const subturn = compactCurrentTurnToolGroups(originalMessages, {
      keepRecentGroups: attempt === 0 ? 1 : 0,
    });
    if (subturn.compacted && smaller(subturn.messages)) return subturn.messages;
    // A two-turn text history has nothing to give at keepRecentTurns 2 and no
    // tool groups; returning undefined on attempt 0 aborts before attempt 1.
    if (keepRecentTurns > 1) {
      const lastTurnOnly = compactMessages(originalMessages, { keepRecentTurns: 1 });
      if (lastTurnOnly.compacted && smaller(lastTurnOnly.messages)) return lastTurnOnly.messages;
    }
    // Fail explicitly rather than silently deleting the original instruction.
    return undefined;
  }

  compactResidentHistory(): void {
    this.maybeCompactResidentHistory();
  }

  private async *maybeCompactWithLLM(additionalInputTokens = 0, abortSignal?: AbortSignal): AsyncGenerator<AgentEvent> {
    if (!this.providerId || !this.apiModel) return;
    if (this.messages.length === 0) return;

    const tail = this.lastAnchorMessageCount !== null
      ? this.messages.slice(this.lastAnchorMessageCount)
      : undefined;
    const budget = getContextBudget(this.providerId, this.apiModel, this.messages, {
      usageAnchorTokens: this.lastInputTokens ?? undefined,
      tailMessages: tail,
      // Usage anchors already include the schemas sent with that request.
      additionalInputTokens: this.lastInputTokens === null ? additionalInputTokens : 0,
      compactAtTokens: this.compactAtTokens,
    });
    if (!budget.shouldCompact) return;

    yield { type: "context_compaction", status: "started", preTokens: budget.estimatedTokens, contextWindow: budget.contextWindow };
    let completed = false;
    const revision = this.getContextRevision?.();
    try {
      const { compactWithLLM } = await import("./context/llm-compactor.js");
      let result = await compactWithLLM(this.messages, {
        provider: this.provider,
        providerId: this.providerId,
        modelId: this.apiModel,
        abortSignal,
      });
      throwIfAborted(abortSignal);
      let path: "llm" | "heuristic" | "subturn" = "llm";
      if (!result.compacted) {
        // The request projection no longer compacts, so this fallback must
        // escalate as far as the former budgeted passes did (down to one kept
        // turn / one kept tool group) or a two-turn text history has no way
        // under the window when the summarizer is unavailable. Take the first
        // pass that fits the budget, otherwise the smallest one that fired.
        const passes: Array<["heuristic" | "subturn", () => ReturnType<typeof compactMessages>]> = [
          ["heuristic", () => compactMessages(this.messages, { keepRecentTurns: 2 })],
          ["heuristic", () => compactMessages(this.messages, { keepRecentTurns: 1 })],
          ["subturn", () => compactCurrentTurnToolGroups(this.messages, { keepRecentGroups: 2 })],
          ["subturn", () => compactCurrentTurnToolGroups(this.messages, { keepRecentGroups: 1 })],
        ];
        let smallest = Infinity;
        for (const [passPath, pass] of passes) {
          const candidate = pass();
          if (!candidate.compacted || !candidate.messages) continue;
          const candidateBudget = getContextBudget(this.providerId, this.apiModel, candidate.messages, {
            additionalInputTokens,
            compactAtTokens: this.compactAtTokens,
          });
          if (candidateBudget.estimatedTokens < smallest) {
            smallest = candidateBudget.estimatedTokens;
            result = candidate;
            path = passPath;
          }
          if (!candidateBudget.shouldCompact) break;
        }
      }
      const beforeEstimate = getContextBudget(this.providerId, this.apiModel, this.messages, { additionalInputTokens }).estimatedTokens;
      const candidateEstimate = result.messages
        ? getContextBudget(this.providerId, this.apiModel, result.messages, { additionalInputTokens }).estimatedTokens : beforeEstimate;
      if (result.compacted && result.messages && candidateEstimate < beforeEstimate) {
        this.applyContextCheckpoint(result.messages, "auto", result.summary, revision, abortSignal);
        this.compactionStats[path] += 1;
        this.compactionStats.fired += 1;
        traceEvent("compaction_fired", { path });
        completed = true;
        const after = getContextBudget(this.providerId, this.apiModel, this.messages, { additionalInputTokens });
        yield { type: "context_compaction", status: "completed", compactionId: this.lastCompactionId, persisted: !!this.onContextCheckpoint, preTokens: budget.estimatedTokens, postTokens: after.estimatedTokens, contextWindow: budget.contextWindow };
      }
    } finally {
      if (!completed) yield { type: "context_compaction", status: "failed", preTokens: budget.estimatedTokens, contextWindow: budget.contextWindow };
    }
    // If LLM compaction failed for any reason, leave this.messages alone —
    // the projector's algorithmic budgeted-mode passes will still try.
  }

  /** Snapshot of how often each compaction path rewrote history this run. */
  getCompactionStats(): { resident: number; subturn: number; llm: number; heuristic: number; overflow: number; fired: number; droppedMessages: number } {
    return { ...this.compactionStats };
  }

  /**
   * Stream a 9-section handoff summary of `oldMessages` from the session model.
   * Powers the manual `/compact` command: streaming (rather than `complete()`)
   * is what lets the TUI show live progress as the summary is produced.
   *
   * `onDelta` receives the full accumulated text and the latest delta on each
   * chunk. Returns the trimmed summary, or "" if the model produced nothing
   * (the caller falls back to heuristic compaction in that case). Throws only
   * if the provider stream itself errors.
   */
  async summarizeForCompaction(
    oldMessages: Message[],
    onDelta?: (full: string, delta: string) => void,
    abortSignal?: AbortSignal,
    userContext?: string,
  ): Promise<string> {
    if (oldMessages.length === 0) return "";
    const { buildCompactionPromptMessages } = await import("./context/compact-llm.js");
    const { collectCompactionFileOps, isCompactionSummaryMessage, messageText } = await import("./context/compact.js");
    const { appendFileBlocks, stripFileBlocks } = await import("./context/compaction-files.js");
    // File blocks stay out of the model round-trip: stripped from the input
    // here, merged deterministically, re-appended to the returned summary.
    const priorSummaries = oldMessages.filter(isCompactionSummaryMessage);
    const fileOps = collectCompactionFileOps(
      oldMessages.filter((message) => !isCompactionSummaryMessage(message)),
      priorSummaries,
    );
    const promptInput = oldMessages.map((message) =>
      isCompactionSummaryMessage(message) && typeof message.content === "string"
        ? { ...message, content: stripFileBlocks(messageText(message)) }
        : message,
    );
    const promptMessages = buildCompactionPromptMessages(promptInput, userContext);
    const stream = this.provider.streamChat(promptMessages, {
      model: this.apiModel,
      temperature: 0.2,
      thinkingLevel: "off",
      abortSignal,
    });
    let full = "";
    for await (const chunk of stream) {
      if (chunk.type === "text" && chunk.content) {
        full += chunk.content;
        onDelta?.(full, chunk.content);
      }
    }
    // Strip any internal reminder markup the summarizer may have reproduced from
    // the transcript: this summary is both displayed in the compaction card and
    // re-injected as a `Previous conversation summary` system message.
    const summary = sanitizeInternalReminderBlocks(full).trim();
    if (!summary) return "";
    return appendFileBlocks(summary, fileOps);
  }

  // ---- Subagent lifecycle: delegated to SubagentRuntime. ----
  // These stay on Agent as one-line forwards so every existing caller
  // (tools/agent-lifecycle.ts, orchestrator/default-hooks.ts) is unchanged.

  async runSubAgent(
    input: string | ContentPart[],
    cwd: string,
    options: Parameters<SubagentRuntime["runSubAgent"]>[2],
  ): Promise<SubagentRunResult> {
    return this.subagents.runSubAgent(input, cwd, options);
  }

  async spawnSubAgent(
    input: string | ContentPart[],
    cwd: string,
    options: Parameters<SubagentRuntime["spawnSubAgent"]>[2],
  ): Promise<SubagentThreadSnapshot> {
    return this.subagents.spawnSubAgent(input, cwd, options);
  }

  async waitSubAgents(options: { agentIds?: string[]; timeoutMs?: number } = {}): Promise<SubagentThreadSnapshot[]> {
    return this.subagents.waitSubAgents(options);
  }

  async sendSubAgentInput(
    agentId: string,
    input: string | ContentPart[],
    cwd: string,
    options: { interrupt?: boolean; parentToolCallId?: string; abortSignal?: AbortSignal } = {},
  ): Promise<SubagentThreadSnapshot> {
    return this.subagents.sendSubAgentInput(agentId, input, cwd, options);
  }

  async closeSubAgent(agentId: string): Promise<SubagentThreadSnapshot> {
    return this.subagents.closeSubAgent(agentId);
  }

  /**
   * Live (non-final) children only — the delegation-nudge gate
   * (large-task-delegation design §2). listSubAgents() is a grows-only
   * session history (finished + resumed children stay forever), so it must
   * never be used to answer "am I currently delegating?".
   */
  activeSubAgentCount(): number {
    return this.subagents.activeSubAgentCount();
  }

  listSubAgents(): SubagentThreadSnapshot[] {
    return this.subagents.listSubAgents();
  }

  /** Read-only child transcript for the TUI inspector. */
  getSubAgentMessages(agentId: string): Message[] {
    const record = this.subagentStore.get(agentId);
    const messages = record?.agent?.messages ?? record?.messages ?? [];
    return messages.map((message) => ({ ...message }));
  }

  /** Marks a child's full summary as delivered to parent context (design §3.3). */
  markSubagentDelivered(agentId: string): void {
    this.subagents.markSubagentDelivered(agentId);
  }

  /**
   * The child store, still reachable at its old name. Several tests inspect a
   * spawned child's record (and its live child Agent) through
   * `(agent as any).subagentStore`; keeping this forward means the extraction
   * changed no caller, test or otherwise.
   */
  get subagentStore(): SubagentStore {
    return this.subagents.store;
  }

  /** run_workflow children are workflowInternal and invisible to listSubAgents. */
  hasRunningWorkflow(): boolean {
    return this.workflowLedger.hasRunning();
  }


  /**
   * Dynamic workflow (option C): runs an LLM-authored JS orchestration script in
   * a QuickJS sandbox. Each agent() call in the script becomes a real scheduled
   * subagent (same route resolution, ChildRunner, scheduler, schema validation
   * as spawn_agent), so the script expresses deterministic control flow while
   * the runtime keeps owning concurrency/budget/retry.
   *
   * Foreground entry point (used by `-p`/headless and tests): awaits to
   * completion and returns the result. Background runs go through startWorkflow.
   */
  async runWorkflow(
    cwd: string,
    options: {
      script: string;
      args?: unknown;
      parentToolCallId: string;
      emitUpdate?: (update: ToolUpdate) => void;
      abortSignal?: AbortSignal;
      ensureProfileTrusted?: (profile: AgentProfile) => Promise<{ content: string | unknown } | undefined>;
    },
  ): Promise<{ result: { ok: true; value: unknown } | { ok: false; error: string }; agentCount: number; logs: string[]; snapshots: SubagentThreadSnapshot[] }> {
    return this.subagents.executeWorkflow(cwd, {
      script: options.script,
      args: options.args,
      parentToolCallId: options.parentToolCallId,
      abortSignal: options.abortSignal,
      directEmit: options.emitUpdate,
      ensureProfileTrusted: options.ensureProfileTrusted,
    });
  }

  /**
   * Starts a workflow in the BACKGROUND (option C Phase 4): returns a runId
   * immediately; the script runs detached, its agents stream progress through
   * the queued channel (drained at turn boundaries like spawn_agent), and its
   * result is ingested at the next turn. Collect explicitly with waitWorkflow.
   */
  startWorkflow(
    cwd: string,
    options: {
      script: string;
      args?: unknown;
      title?: string;
      parentToolCallId: string;
      abortSignal?: AbortSignal;
      ensureProfileTrusted?: (profile: AgentProfile) => Promise<{ content: string | unknown } | undefined>;
    },
  ): { runId: string; title: string } {
    return this.workflowLedger.start(cwd, options);
  }

  /** Blocks until a background workflow reaches a final state (or times out). */
  async waitWorkflow(runId: string, timeoutMs?: number): Promise<WorkflowRunSnapshot | undefined> {
    return this.workflowLedger.wait(runId, normalizeWaitTimeout(timeoutMs));
  }

  /** Cancels a running background workflow. */
  closeWorkflow(runId: string): WorkflowRunSnapshot | undefined {
    return this.workflowLedger.close(runId);
  }

  listWorkflows(): WorkflowRunSnapshot[] {
    return this.workflowLedger.list();
  }

  /** Injects completed background-workflow results before the next turn (§5 analog). */
  private flushWorkflowDeliveries(): void {
    for (const notice of this.workflowLedger.drainDeliveryNotices()) {
      this.injectSystemReminder(notice);
    }
  }


  /**
   * Resolves a child's model route. Priority, highest first (design v2 §1.1):
   *   call-site override (model/effort)  >  profile.model  >  category  >  inherit parent.
   * The call-site override is what lets the model say "opus for this reviewer,
   * haiku for these twenty scouts" per spawn/batch member at request time.
   */
  /** Kept as a thin delegator: routing decisions now live in SubagentRouter. */
  resolveRouteForSubagent(
    profile: AgentProfile,
    category: string | undefined,
    override?: { model?: string; effort?: ThinkingLevel },
  ): ResolvedSubagentRoute {
    return this.router.resolve(profile, category, override);
  }

  /** Routable catalog across runnable providers (design v3.6); undefined when unwired. */
  listRoutableModels(): RoutableModelEntry[] | undefined {
    return this.router.listRoutableModels();
  }




  /**
   * The ONE place a child Agent is constructed. The runtime hands over what it
   * resolved (provider/route/tools/prompt); everything inherited from the
   * parent is filled in here, which is also what keeps the deliberately
   * ABSENT arguments absent — `onMessageAppend` (a child must not append to
   * the parent's session log), `sessionID` (a child must not stop the
   * parent's auto servers) and the routing catalog (children route by
   * default, not from the parent's menu).
   */
  private createChildAgent(spec: ChildAgentSpec): ChildAgentLike {
    const child = new Agent({
      provider: spec.provider,
      providerId: spec.providerId,
      model: spec.model,
      tools: spec.tools,
      temperature: this.temperature,
      thinkingLevel: spec.thinkingLevel,
      mode: spec.mode,
      maxTurns: spec.maxTurns,
      budgetLedger: this.budgetLedger,
      budgetSource: spec.budgetSource,
      systemPrompt: spec.systemPrompt,
      hooks: this.hookDefinitions,
      externalHooks: this.externalHooks,
      agentRole: "subagent",
      subAgentId: spec.subAgentId,
      agentCategories: this.agentCategories,
      providerFactory: this.providerFactory,
      // The scheduler owns 429 backoff for children; the transport must not
      // stack its own retries on top (design §4.5).
      rateLimitPolicy: "defer",
    });
    if (spec.resumeMessages && spec.resumeMessages.length > 0) {
      child.messages = spec.resumeMessages;
    } else if (spec.forkContext) {
      child.messages = this.forkMessagesForSubagent(spec.systemPrompt);
    }
    return child;
  }

  private forkMessagesForSubagent(childSystemPrompt: string): Message[] {
    const forked = this.messages
      .filter((message) => {
        if (message.role === "system" || message.role === "meta") return false;
        if (message.role === "assistant" && message.toolCalls?.some((call) => isSubagentLifecycleTool(call.name))) {
          return false;
        }
        if (message.role === "tool" && message.metadata?.kind === "subagent") {
          return false;
        }
        return true;
      })
      .slice(-20);
    return [{ role: "system", content: childSystemPrompt }, ...forked];
  }


  private maybeCompactResidentHistory(): void {
    if (this.messages.length === 0 || !hasCompleteToolGroups(this.messages)) {
      return;
    }

    const before = this.messages;
    const beforeChars = estimateResidentChars(before);
    const beforeToolChars = estimateToolPayloadChars(before);
    // Compact canonical history, not the provider projection: projection
    // renders runtime meta as user text and can synthesize missing tool results.
    let candidate = pruneMessages(before);

    const budget = this.providerId && this.apiModel
      ? getContextBudget(this.providerId, this.apiModel, candidate)
      : undefined;
    const heapUsed = getCurrentHeapUsed();
    const keepRecentTurns = countUserTurns(candidate) > 10
      ? 2
      : RESIDENT_HISTORY_KEEP_RECENT_TURNS;
    // Cliff-edge only (docs/harness-thinning.md, wave 2): history is the
    // model's working memory and is NEVER trimmed early. The message-count
    // and char-size triggers that used to fire here mid-run created re-read
    // loops - the model lost what it had read and read it again. Compaction
    // now waits for the context-window budget itself; the heap guard stays
    // as process life-support, not policy.
    const shouldAggressivelyPrune = heapUsed >= RESIDENT_HISTORY_HEAP_HARD_LIMIT;
    // Model summarization runs at the request boundary. Resident compaction is
    // an emergency heap guard, not a competing token-triggered lossy policy.
    const shouldCompact = shouldAggressivelyPrune && !!budget?.shouldCompact;

    if (shouldAggressivelyPrune) {
      candidate = aggressivePruneMessages(candidate);
    }

    let compactedPath: "resident" | "subturn" | undefined;
    let residentSummary: string | undefined;
    if (shouldCompact) {
      const compacted = compactMessages(candidate, { keepRecentTurns });
      if (compacted.compacted && compacted.messages) {
        candidate = compacted.messages as typeof candidate;
        compactedPath = "resident";
        residentSummary = compacted.summary;
        traceEvent("compaction_fired", { path: "resident", droppedEntries: compacted.droppedEntries });
      } else {
        // Single-instruction runs (one real user turn) never satisfy the
        // turn-level compactor; fold old tool groups within the turn instead.
        const subturn = compactCurrentTurnToolGroups(candidate, { keepRecentGroups: 3 });
        if (subturn.compacted && subturn.messages) {
          candidate = subturn.messages as typeof candidate;
          compactedPath = "subturn";
          residentSummary = subturn.summary;
          traceEvent("compaction_fired", { path: "subturn", droppedEntries: subturn.droppedEntries });
        }
      }
      if (compactedPath) this.compactionStats.fired += 1;
    }

    const afterChars = estimateResidentChars(candidate);
    const afterToolChars = estimateToolPayloadChars(candidate);
    // Chars-not-increasing is a NECESSARY condition: the old OR gate let a
    // fewer-but-bigger candidate (summary stacking) rewrite history.
    if (
      afterChars <= beforeChars
      && (
        afterChars < beforeChars
        || afterToolChars < beforeToolChars
        || candidate.length < before.length
      )
    ) {
      if (!compactedPath) {
        // Request-only thinning must not mutate durable history. Under heap
        // pressure below the token threshold, the pruned candidate still has to
        // replace resident memory or the life-support guard frees nothing; the
        // session log keeps the full tool output.
        if (shouldAggressivelyPrune) {
          this.messages = candidate;
          this.lastInputTokens = null;
          this.lastAnchorMessageCount = null;
          this.fileStateTracker?.invalidateReadHistory();
        }
        return;
      }
      this.applyContextCheckpoint(candidate, "resident", residentSummary, this.getContextRevision?.());
      if (compactedPath === "resident") {
        this.compactionStats.resident += 1;
      }
      if (compactedPath === "subturn") this.compactionStats.subturn += 1;
      if (compactedPath && budget) {
        this.contextEvents.push({ type: "context_compaction", status: "completed", compactionId: this.lastCompactionId, persisted: !!this.onContextCheckpoint, preTokens: budget.estimatedTokens,
          postTokens: getContextBudget(this.providerId, this.apiModel, candidate).estimatedTokens, contextWindow: budget.contextWindow });
      }
    }
  }

  /** Persist conversational context first; keep live reminders outside the durable projection. */
  private applyContextCheckpoint(messages: Message[], reason: ContextCheckpoint["reason"], summary?: string,
    baseRevision?: string, abortSignal?: AbortSignal): void {
    throwIfAborted(abortSignal);
    const checkpoint = createContextCheckpoint(messages, reason, summary, baseRevision);
    this.onContextCheckpoint?.(checkpoint);
    if (!this.onContextCheckpoint && summary?.trim()) this.onCompactionApplied?.(summary);
    const { leading } = splitLeadingContext(messages);
    // The compactor may have folded away reminders from its candidate. Their
    // lifecycle belongs to the live Agent, not the durable summary/checkpoint.
    const runtimeReminders = splitLeadingContext(this.messages).body.filter(message =>
      message.role === "meta" && message.includeInLlm !== false && !isCompactionSummaryMessage(message));
    this.messages = [...leading, ...checkpointMessages(checkpoint), ...runtimeReminders];
    this.lastCompactionId = checkpoint.compactionId;
    this.lastInputTokens = null;
    this.lastAnchorMessageCount = null;
    this.fileStateTracker?.invalidateReadHistory();
  }

  private appendMessage(message: Message) {
    if (message.role === "assistant" && message.content) {
      message.content = sanitizeInternalReminderBlocks(message.content);
    }
    if (message.role === "assistant" && message.reasoning) {
      message.reasoning = sanitizeInternalReasoningText(message.reasoning);
    }
    if (message.role === "assistant" && message.providerMetadata) {
      message.providerMetadata = sanitizeAssistantProviderMetadata(message.providerMetadata, message.modelId ?? message.model);
    }
    this.messages.push(message);
    traceEvent("agent_message_append", {
      message: summarizeTraceMessage(message),
      messageCount: this.messages.length,
    }, {
      sessionFile: this.sessionID,
      provider: this._providerId || "none",
      model: this.apiModel || "none",
    });
    this.onMessageAppend?.(message);
    this.notifyContextChanged();
  }

  private persistProviderError(
    error: unknown,
    request: { messageCount: number; toolCount: number; retry?: ProviderErrorContext["retry"] },
  ): void {
    this.recordProviderError(error, {
      providerId: this.providerId,
      modelId: this.apiModel,
      model: this._model,
      thinkingLevel: this.thinkingLevel,
      messageCount: request.messageCount,
      toolCount: request.toolCount,
      retry: request.retry,
    });
  }

  private recordProviderError(error: unknown, context: ProviderErrorContext): void {
    if (!this.onProviderError) return;
    try {
      this.onProviderError(createSanitizedProviderError(error, context));
    } catch {
      // Diagnostics are best-effort and must never replace the provider error.
    }
  }

  private appendInterruptedAssistantBoundary(
    currentAssistant: Extract<Message, { role: "assistant" }> | undefined,
    currentAssistantAppended: boolean,
  ): boolean {
    const last = lastProviderMessage(this.messages);
    if (last?.role === "assistant" && last.error?.aborted) {
      return false;
    }

    const partialText = !currentAssistantAppended ? currentAssistant?.content.trim() : "";
    const content = partialText
      ? `${partialText}\n\n${INTERRUPTED_ASSISTANT_CONTENT}`
      : INTERRUPTED_ASSISTANT_CONTENT;

    this.appendMessage({
      role: "assistant",
      content,
      reasoning: !currentAssistantAppended ? currentAssistant?.reasoning : undefined,
      error: {
        name: "MessageAbortedError",
        message: "Assistant response was interrupted by the user.",
        aborted: true,
      },
    });
    return true;
  }

  private async executeTool(
    toolCall: ParsedToolCall,
    cwd: string,
    abortSignal?: AbortSignal,
    emitUpdate?: (update: ToolUpdate) => void,
  ): Promise<ToolResult> {
    throwIfAborted(abortSignal);
    if (toolCall.name === "exit_plan_mode" && this._mode !== "plan") {
      return {
        content:
          "Ignored exit_plan_mode because plan mode is not active. " +
          "Continue with the user's request directly using the regular tools.",
      };
    }

    const tool = this.tools.get(toolCall.name);
    if (!tool) {
      return {
        content: `Error: Unknown tool "${toolCall.name}"`,
        isError: true,
      };
    }

    if (this._mode === "plan" && !tool.readOnly) {
      return {
        content:
          `Error: Tool "${toolCall.name}" is not allowed in plan mode. ` +
          `In plan mode you may only use read-only tools (read, ls, grep, web_search, web_fetch, spawn_agent, wait_agent, send_input, skill_search, skill, tool_search, question, exit_plan_mode). ` +
          `To modify files or run commands, present your proposal and call exit_plan_mode so the user can review and approve it.`,
        isError: true,
      };
    }

    if (tool.deferred && !this.unlockedDeferred.has(tool.name)) {
      return {
        content:
          `Error: Tool "${toolCall.name}" is a deferred tool; its schema is not yet loaded. ` +
          `Call tool_search first with query "select:${toolCall.name}" to load its schema, then retry.`,
        isError: true,
      };
    }

    if (toolCall.argsCorrupt) {
      return {
        content:
          `Error: The arguments for "${toolCall.name}" failed to parse as JSON, indicating the provider returned truncated or malformed tool arguments. ` +
          `Re-issue the call with valid JSON arguments; do not assume the previous attempt ran.`,
        isError: true,
        status: "blocked",
        metadata: { kind: "security", reason: "args_corrupt" },
      };
    }

    let preparedArgs = toolCall.parsedArgs;
    if (tool.prepareArguments) {
      try {
        preparedArgs = tool.prepareArguments(preparedArgs);
      } catch (err) {
        return {
          content:
            `Error: Tool "${toolCall.name}" arguments could not be normalized before execution: ` +
            `${err instanceof Error ? err.message : String(err)}. Re-issue the call with valid arguments.`,
          isError: true,
          status: "blocked",
          metadata: { kind: "security", reason: "args_prepare_failed" },
        };
      }
    }

    const missingRequired = findMissingRequiredArgs(tool.parameters, preparedArgs);
    if (missingRequired.length > 0) {
      return {
        content:
          `Error: Tool "${toolCall.name}" was called without required argument${missingRequired.length === 1 ? "" : "s"}: ${missingRequired.map((name) => `"${name}"`).join(", ")}. ` +
          `Re-issue the call with all required fields filled. Do not assume the previous attempt ran with default values.`,
        isError: true,
        status: "blocked",
        metadata: { kind: "security", reason: "missing_required_args", missing: missingRequired },
      };
    }

    try {
      return await tool.execute(preparedArgs, {
        cwd,
        sessionID: this.sessionID,
        abortSignal,
        toolCall: { id: toolCall.id, name: toolCall.name },
        agent: this,
        emitUpdate,
      });
    } catch (err: any) {
      return {
        content: `Error executing ${toolCall.name}: ${err.message || String(err)}`,
        isError: true,
      };
    }
  }
}

function serializedMessageBytes(messages: Message[]): number {
  return Buffer.byteLength(JSON.stringify(messages), "utf8");
}
