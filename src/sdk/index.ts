/**
 * Bubble SDK — the programmatic embedding surface for external hosts.
 *
 * Two layers:
 *  1. `BubbleSdk` — a high-level facade that owns config/provider resolution and
 *     assembles a full agent turn (tools, approvals, questions, plan mode, MCP,
 *     skills, hooks, session persistence) behind a single `runTurn()` async
 *     iterator of `AgentEvent`s. This is what a host like an Electron app should
 *     consume: wire the three interaction callbacks (approval / question / plan)
 *     to its own UI and map the event stream to its own message format.
 *  2. Curated re-exports of the underlying building blocks, for hosts that need
 *     to assemble a custom loop the facade doesn't cover.
 *
 * Runs under plain Node (>=20); nothing here touches the TUI layer.
 */

import os from "node:os";
import { rmSync } from "node:fs";
import { resolve as resolvePath } from "node:path";
import { randomUUID } from "node:crypto";

import { Agent, AgentAbortError } from "../agent.js";
import { AgentRunInputQueue } from "../agent/input-controller.js";
import {
  SessionTurnCoordinator,
  type SessionTurnReservation,
} from "./session-turn-coordinator.js";
import { ReplayEventLog } from "./replay-event-log.js";
import { SessionManager, type SessionSummary } from "../session.js";
import { isPermissionModeReminder } from "../prompt/reminders.js";
import { PermissionAwareApprovalController } from "../approval/controller.js";
import { BashAllowlist } from "../approval/session-cache.js";
import { SettingsManager } from "../permissions/settings.js";
import {
  isRepoConfigTrusted,
  mergedRepoCapabilities,
  readRepoSettings,
  trustRepoConfig,
  type RepoCapabilities,
} from "../permissions/trust.js";
import type { ApprovalDecision, ApprovalRequest } from "../approval/types.js";
import { createAllTools, buildToolPromptOptions, type PlanController } from "../tools/index.js";
import { buildSystemPrompt } from "../system-prompt.js";
import { FileStateTracker } from "../tools/file-state.js";
import { BudgetLedger } from "../agent/budget-ledger.js";
import { UserConfig } from "../config.js";
import { ProviderRegistry, encodeModel, decodeModel, displayModel } from "../provider-registry.js";
import { createProviderInstance } from "../provider.js";
import type { ResolvedSubagentRoute } from "../agent/categories.js";
import { getDefaultThinkingLevel } from "../variant/variant-resolver.js";
import { QuestionController, type QuestionAnswer, type QuestionRequest } from "../question/controller.js";
import { assertProviderModelAllowed } from "../provider-model-policy.js";
import { SkillRegistry } from "../skills/registry.js";
import { parseSkillInvocation } from "../skills/invocation.js";
import type { SkillSummary } from "../skills/types.js";
import { GoalStore } from "../goal/store.js";
import { McpManager, gateMcpTools } from "../mcp/manager.js";
import type { McpServerState } from "../mcp/types.js";
import { loadMcpConfig } from "../mcp/config.js";
import { ExternalHookController } from "../hooks/controller.js";
import { buildMemoryPrompt } from "../memory/store.js";
import { calculateUsageCost } from "../model-pricing.js";
import { purgeUnsafeMemorySources } from "../memory/session-policy.js";
import { recordMemoryCitations } from "../memory/usage.js";
import type {
  AgentEvent,
  AgentRunInput,
  ContentPart,
  Message,
  PermissionMode,
  Provider,
  ThinkingLevel,
  ToolRegistryEntry,
} from "../types.js";

// ── Facade types ───────────────────────────────────────────────────────────

export interface BubbleSdkOptions {
  /** Optional process-local replay window. Active readers never lose unread
   * events. Older cursors fail explicitly; restore durable session history.
   * Omit for unlimited replay (the public SDK compatibility default). The
   * first opener sets the shared process-local window for that session. */
  sessionEventRetention?: number;
  /** Fallback working directory for sessions created without an explicit cwd. */
  defaultCwd?: string;
  /**
   * Load MCP servers from user/project settings (default true). Hermetic hosts
   * (evals, tests) set false so runs never depend on ambient MCP config.
   */
  mcp?: boolean;
}

/** Resolved turn configuration, reported via onStart (e.g. for a host's system-init event). */
export interface TurnStartInfo {
  providerId: string;
  model: string;
  thinkingLevel: ThinkingLevel;
  mode: PermissionMode;
  tools: string[];
  skills: string[];
}

/** Host-side interaction callbacks for one turn. All optional; missing handlers fail safe (reject). */
export interface TurnHandlers {
  /**
   * Tool call needs user permission (bash / edit / write / …). When the turn is
   * aborted, any still-pending request auto-resolves to reject — the host's
   * promise may settle later and is then ignored.
   */
  onApproval?: (req: ApprovalRequest) => Promise<ApprovalDecision>;
  /** Agent asked the user a structured question. Return answers, or null to decline. */
  onQuestion?: (req: QuestionRequest) => Promise<QuestionAnswer[] | null>;
  /** Plan-mode proposal. Return true to approve executing the plan. */
  onPlanApproval?: (planMarkdown: string) => Promise<boolean>;
  /**
   * Folder trust (Kimi Code style): the session folder's `.bubble` settings
   * would enable allow rules, MCP servers or LSP servers the user has not
   * trusted. Return true to trust them. Asked at most once per folder per SDK
   * instance; without a handler, or on false, they stay off.
   */
  onProjectTrust?: (request: { cwd: string; pending: RepoCapabilities }) => Promise<boolean>;
  /** Fired once per turn after provider/model/tools are resolved, before the first event. */
  onStart?: (info: TurnStartInfo) => void;
}

export interface RunTurnOptions extends TurnHandlers {
  /**
   * The folder this turn works in, when it is not the session's own: tools,
   * project settings, trust, MCP servers and the prompt's working directory
   * follow it. The session stays where it was made, so one conversation can
   * work in several projects, a turn at a time. Follow-up turns the SDK
   * starts for this turn reuse it.
   */
  cwd?: string;
  /**
   * Turn-scoped host capabilities. Never cached globally or given to subagents;
   * follow-up turns the SDK starts for this turn (queued input, pending steers)
   * reuse this turn's options, these included.
   */
  hostTools?: ToolRegistryEntry[];
  /**
   * Extra skill directories for this turn, e.g. skills a host app ships with.
   * Searched after the user's own roots, so a user skill of the same name wins.
   * Like hostTools, follow-up turns the SDK starts for this turn reuse them.
   */
  skillPaths?: string[];
  /** Plain text, or content parts (text + base64 images) for attachments. */
  prompt: string | ContentPart[];
  /** Preserve an admitted input if cancelled before initialization records it. */
  preserveInterruptedInput?: boolean;
  /** "provider:model" or bare model id (resolved against the default provider). */
  model?: string;
  mode?: PermissionMode;
  /**
   * Mode the turn switches to when the user approves a plan (only relevant
   * when `mode` is "plan"). Hosts pass the mode the user had selected before
   * entering plan mode, so approving a plan does not silently downgrade
   * bypassPermissions to default. Defaults to "default".
   */
  planExitMode?: Exclude<PermissionMode, "plan">;
  thinkingLevel?: ThinkingLevel;
  /**
   * Extra text appended to the built system prompt. Lets hosts (and the eval
   * harness) A/B prompt variants without forking the prompt builder.
   */
  appendSystemPrompt?: string;
  /**
   * What the built system prompt calls the agent ("You are …"). A host with
   * several agents names each one; "Bubble" when absent.
   */
  agentName?: string;
  /**
   * Replaces the built system prompt, for a host whose agent has a role of its
   * own (a coordinator that hands work out and never touches files, say).
   * appendSystemPrompt still applies after it.
   */
  systemPrompt?: string;
  /**
   * Offer only these built-in and MCP tools. Host tools are always offered.
   * Omit for all of them; an empty list leaves the host tools alone, and then
   * MCP servers are not started either.
   */
  allowedTools?: string[];
  /**
   * Creates the provider instance for a subagent route whose provider differs
   * from the turn's provider (e.g. spawn_agent with `model: "provider:model"`).
   * When omitted the SDK builds a default factory over its own registry —
   * same semantics as the TUI's createProviderForRoute (OAuth prepare, then
   * configured+enabled+keyed profile lookup). Pass your own to override.
   */
  providerFactory?: (route: ResolvedSubagentRoute) => Provider | Promise<Provider>;
  signal?: AbortSignal;
}

export interface SdkSessionRef {
  id: string;
  cwd: string;
}

export interface SdkSessionRunState {
  /** A turn owns the session slot, including setup and teardown. */
  active: boolean;
  queuedTurns: number;
  pendingSteers: number;
  phase: "idle" | "reserved" | "starting" | "active" | "stopping" | "deleted";
}

export type SdkSteerOutcome = Extract<AgentEvent, {
  type: "input_applied" | "input_queued" | "input_rejected";
}>;

export type SdkSteerResult =
  | {
      accepted: true;
      disposition: "steered";
      input: AgentRunInput;
      outcome: Promise<SdkSteerOutcome>;
    }
  | {
      accepted: true;
      disposition: "queued";
      input: AgentRunInput;
      turnId: string;
      outcome: Promise<SdkSteerOutcome>;
    }
  | {
      accepted: false;
      disposition: "rejected";
      reason: "unknown_session" | "session_deleted" | "ownership_conflict";
    };

export interface SdkSessionEvent {
  sequence: number;
  sessionId: string;
  turnId: string;
  event: AgentEvent;
  /**
   * Marks a settled turn after cleanup and admission of any fallback steer.
   * Successful turns mark agent_end; failures synthesize a turn_end record.
   */
  terminal?: SdkSessionTerminal;
}

export type SdkSessionTerminal =
  | { kind: "completed" }
  | { kind: "failed" | "cancelled"; message: string };

export interface SdkStopOptions {
  /** Match Claude-style interruption: queued messages survive by default. */
  cancelQueued?: boolean;
}

export interface SdkSessionHandle extends SdkSessionRef {
  /** Current replay cursor; snapshot before send to observe only new work. */
  readonly latestSequence: number;
  /** Events from every turn in this session. Opening another handle reconnects from sequence 1. */
  readonly events: AsyncIterable<SdkSessionEvent>;
  /** Reconnect after the last sequence the host durably processed. */
  eventsFrom(afterSequence: number): AsyncIterable<SdkSessionEvent>;
  send(options: RunTurnOptions): AsyncGenerator<AgentEvent>;
  steer(content: string): SdkSteerResult;
  stop(options?: SdkStopOptions): number;
  close(): void;
}

interface PendingSteerOutcome {
  input: AgentRunInput;
  promise: Promise<SdkSteerOutcome>;
  resolve(outcome: SdkSteerOutcome): void;
}

interface SdkTurnRuntime {
  sessionId: string;
  ownerKey: string;
  reservation: SessionTurnReservation;
  inputController: AgentRunInputQueue;
  outcomes: Map<string, PendingSteerOutcome>;
  options: RunTurnOptions;
  events: ReplayEventLog<AgentEvent>;
  started: boolean;
  inputRecorded: boolean;
  finished: Promise<void>;
}

// ── Facade ─────────────────────────────────────────────────────────────────

export class BubbleSdk {
  readonly userConfig = new UserConfig();
  readonly registry = new ProviderRegistry(this.userConfig);

  private readonly defaultCwd: string;
  private readonly mcpEnabled: boolean;
  private readonly sessionEventRetention: number;
  private readonly cwdBySession = new Map<string, string>();
  private readonly bashAllowlists = new Map<string, BashAllowlist>();
  private readonly sessionGrants = new Map<string, Set<string>>();
  private readonly turnCoordinator = new SessionTurnCoordinator();
  private readonly turnRuntimes = new Map<string, SdkTurnRuntime>();
  private readonly lastTurnOptions = new Map<string, Omit<RunTurnOptions, "prompt" | "signal">>();
  private readonly mcpToolsByCwd = new Map<string, Promise<ToolRegistryEntry[]>>();
  private readonly mcpManagersByCwd = new Map<string, Promise<McpManager | null>>();
  /** Folders whose trust question was already put to the host. */
  private readonly projectTrustAsked = new Set<string>();
  private readonly inputIdNonce = randomUUID().slice(0, 6);
  private nextTurnInputPrefix = 0;
  private nextDetachedInputId = 0;
  /** id -> {cwd,file} rebuilt from disk so sessions survive host restarts. */
  private sessionIndex = new Map<string, { cwd: string; file: string }>();

  constructor(options: BubbleSdkOptions = {}) {
    this.defaultCwd = options.defaultCwd || process.env.BUBBLE_CWD || os.homedir();
    this.mcpEnabled = options.mcp !== false;
    this.sessionEventRetention = options.sessionEventRetention ?? Infinity;
    if (this.sessionEventRetention !== Infinity && (!Number.isSafeInteger(this.sessionEventRetention) || this.sessionEventRetention < 1)) {
      throw new Error('sessionEventRetention must be a positive integer');
    }
  }

  // ── Sessions ─────────────────────────────────────────────────────────────

  listSessions(): SessionSummary[] {
    const sessions = SessionManager.listAllSessions().filter((session) => !this.turnCoordinator.isDeleted(session.name));
    this.sessionIndex = new Map(
      sessions.map((s) => [s.name, { cwd: s.cwd ?? s.cwdLabel ?? this.defaultCwd, file: s.file }]),
    );
    return sessions;
  }

  createSession(options: { cwd?: string; id?: string } = {}): SdkSessionRef {
    const cwd = options.cwd || this.defaultCwd;
    const id = options.id || `sdk-${Date.now().toString(36)}-${randomUUID().slice(0, 8)}`;
    this.turnCoordinator.revive(id);
    SessionManager.create(cwd, sessionFileName(id));
    processSessionLocations.set(id, cwd);
    this.cwdBySession.set(id, cwd);
    return { id, cwd };
  }

  openSession(sessionId: string): SdkSessionHandle {
    const resolved = this.resolveSession(sessionId);
    if (!resolved) throw new Error(`Unknown session: ${sessionId}`);
    const ownerKey = resolved.manager.getSessionFile();
    const eventLog = sessionEventLogFor(ownerKey, this.sessionEventRetention);
    const closed = new AbortController();
    return {
      id: sessionId,
      cwd: resolved.cwd,
      get latestSequence() { return eventLog.length; },
      get events() { return eventLog.iterate({ signal: closed.signal }); },
      eventsFrom: (afterSequence) => eventLog.iterate({
        from: Math.max(0, afterSequence),
        signal: closed.signal,
      }),
      send: (options) => this.runTurn(sessionId, options),
      steer: (content) => this.steer(sessionId, content),
      stop: (options) => this.stop(sessionId, options),
      close: () => closed.abort(),
    };
  }

  getHistory(sessionId: string): Message[] {
    if (this.turnCoordinator.isDeleted(sessionId)) return [];
    return this.resolveSession(sessionId)?.manager.getMessages() ?? [];
  }

  /** Preserve an accepted desktop input cancelled before runTurn was admitted. */
  recordInterruptedInput(sessionId: string, prompt: string | ContentPart[]): void {
    const owner = this.ownerForSession(sessionId);
    if (owner && owner !== this) return owner.recordInterruptedInput(sessionId, prompt);
    if (this.turnCoordinator.isDeleted(sessionId) || this.getSessionRunState(sessionId).active) {
      throw new Error(`Cannot record interrupted input while session is active or deleted: ${sessionId}`);
    }
    const resolved = this.resolveSession(sessionId);
    if (!resolved) throw new Error(`Unknown session: ${sessionId}`);
    this.persistInterruptedInput(resolved, prompt);
  }

  private persistInterruptedInput(resolved: { manager: SessionManager; cwd: string }, prompt: string | ContentPart[]): void {
    const session = resolved.manager;
    session.getMessages();
    const revision = session.getRevision();
    session.updateMetadata({ cwd: resolved.cwd });
    session.appendMessage({ role: "user", content: prompt }, revision);
    session.appendMessage({
      role: "assistant",
      content: "[Interrupted by user before execution started.]",
      error: { name: "MessageAbortedError", message: "Turn stopped during initialization.", aborted: true },
    }, session.getRevision());
  }

  async deleteSession(sessionId: string): Promise<void> {
    const owner = this.ownerForSession(sessionId);
    if (owner && owner !== this) return owner.deleteSession(sessionId);
    const resolved = this.resolveSession(sessionId);
    for (const runtime of this.runtimesForSession(sessionId)) runtime.inputController.closePendingInputs();
    await this.turnCoordinator.delete(sessionId);
    if (resolved) rmSync(resolved.manager.getSessionFile(), { force: true });
    this.cwdBySession.delete(sessionId);
    this.bashAllowlists.delete(sessionId);
    this.sessionGrants.delete(sessionId);
    this.lastTurnOptions.delete(sessionId);
    this.sessionIndex.delete(sessionId);
    processSessionLocations.delete(sessionId);
    if (resolved) {
      releaseSessionEventLog(resolved.manager.getSessionFile());
      releaseProcessOwner(resolved.manager.getSessionFile(), this);
    }
  }

  /** Interrupt the active turn. Queued turns survive unless cancelQueued is true. */
  stop(sessionId: string, options: SdkStopOptions = {}): number {
    const owner = this.ownerForSession(sessionId);
    if (owner && owner !== this) return owner.stop(sessionId, options);
    const current = this.turnCoordinator.getCurrent(sessionId);
    const runtime = current ? this.turnRuntimes.get(current.id) : undefined;
    runtime?.inputController.closePendingInputs();
    // A reserved turn can release its slot synchronously when cancelled.
    // Clear the queue first so cancellation cannot promote an unwanted input.
    const queued = options.cancelQueued ? this.turnCoordinator.clearQueue(sessionId) : 0;
    return queued + this.turnCoordinator.stopCurrent(sessionId);
  }

  /** Stop and wait for persistence, cleanup, and terminal event publication. */
  async stopAndWait(sessionId: string, options: SdkStopOptions = {}): Promise<number> {
    const owner = this.ownerForSession(sessionId);
    if (owner && owner !== this) return owner.stopAndWait(sessionId, options);
    const current = this.turnCoordinator.getCurrent(sessionId);
    const stopping = this.runtimesForSession(sessionId).filter(runtime =>
      options.cancelQueued || runtime.reservation.id === current?.id);
    const stopped = this.stop(sessionId, options);
    await Promise.all(stopping.map(runtime => runtime.finished));
    return stopped;
  }

  /** Cancel queued turns without interrupting the turn that owns the session slot. */
  clearQueue(sessionId: string): number {
    const owner = this.ownerForSession(sessionId);
    if (owner && owner !== this) return owner.clearQueue(sessionId);
    return this.turnCoordinator.clearQueue(sessionId);
  }

  getSessionRunState(sessionId: string): SdkSessionRunState {
    const owner = this.ownerForSession(sessionId);
    if (owner && owner !== this) return owner.getSessionRunState(sessionId);
    const state = this.turnCoordinator.getState(sessionId);
    const current = this.turnCoordinator.getCurrent(sessionId);
    const runtime = current ? this.turnRuntimes.get(current.id) : undefined;
    return {
      active: state.active,
      queuedTurns: state.queued,
      pendingSteers: runtime?.outcomes.size ?? 0,
      phase: state.phase,
    };
  }

  /**
   * Add text to the turn that currently owns the session slot. Every accepted
   * input exposes an outcome promise, independent of event-stream consumption.
   */
  steer(sessionId: string, content: string): SdkSteerResult {
    if (this.turnCoordinator.isDeleted(sessionId)) {
      return { accepted: false, disposition: "rejected", reason: "session_deleted" };
    }
    const owner = this.ownerForSession(sessionId);
    if (owner && owner !== this) return owner.steer(sessionId, content);
    const resolved = this.resolveSession(sessionId);
    if (!resolved) {
      return { accepted: false, disposition: "rejected", reason: "unknown_session" };
    }
    const current = this.turnCoordinator.getCurrent(sessionId);
    const runtime = current ? this.turnRuntimes.get(current.id) : undefined;
    if (runtime) {
      const input = runtime.inputController.tryEnqueue(content);
      if (input) {
        const pending = pendingSteerOutcome(input);
        runtime.outcomes.set(input.id, pending);
        return { accepted: true, disposition: "steered", input, outcome: pending.promise };
      }
    }
    return this.queueDetachedInput(sessionId, content, resolved);
  }

  enqueueTurn(sessionId: string, options: RunTurnOptions): AsyncGenerator<AgentEvent> {
    return this.runTurn(sessionId, options);
  }

  queueTurn(sessionId: string, options: RunTurnOptions): AsyncGenerator<AgentEvent> {
    return this.enqueueTurn(sessionId, options);
  }

  // ── Discovery (composer pickers) ─────────────────────────────────────────

  listSkills(cwd?: string, skillPaths: string[] = []): SkillSummary[] {
    const registry = new SkillRegistry({
      cwd: cwd || this.defaultCwd,
      skillPaths: [...this.userConfig.getSkillPaths(), ...skillPaths],
      disabledSkills: this.userConfig.getDisabledSkills(),
    });
    return registry.summaries();
  }

  /**
   * Ask the host once per folder (onProjectTrust) before this turn loads the
   * repository's allow rules, MCP servers and LSP servers. Trusting restarts
   * the folder's MCP servers so newly trusted ones are available right away.
   */
  private async resolveProjectTrust(cwd: string, options: RunTurnOptions, signal: AbortSignal): Promise<void> {
    if (!options.onProjectTrust || this.projectTrustAsked.has(cwd)) return;
    // Read once: what gets trusted is exactly what the host was shown.
    const raw = readRepoSettings(cwd);
    if (isRepoConfigTrusted(cwd, raw)) return;
    this.projectTrustAsked.add(cwd);
    const trusted = await awaitWithAbort(
      options.onProjectTrust({ cwd, pending: mergedRepoCapabilities(raw) }),
      signal,
    );
    if (!trusted) return;
    trustRepoConfig(cwd, raw);
    const manager = this.mcpManagersByCwd.get(cwd);
    this.mcpManagersByCwd.delete(cwd);
    this.mcpToolsByCwd.delete(cwd);
    await (await manager?.catch(() => null))?.shutdown().catch(() => undefined);
  }

  /**
   * The MCP servers of a folder and how each connection stands. Starts them
   * if no turn has yet (the same lazy start a turn does).
   */
  async getMcpStatus(cwd?: string): Promise<McpServerState[]> {
    const folder = cwd || this.defaultCwd;
    await this.mcpToolsFor(folder);
    const manager = await this.mcpManagersByCwd.get(folder);
    return manager?.getStates() ?? [];
  }

  /** Reconnect one MCP server of a folder; null when it is not configured. */
  async reconnectMcpServer(name: string, cwd?: string): Promise<McpServerState | null> {
    const folder = cwd || this.defaultCwd;
    await this.mcpToolsFor(folder);
    const manager = await this.mcpManagersByCwd.get(folder);
    if (!manager) return null;
    const state = await manager.reconnect(name);
    // The cached tool list predates the reconnect.
    this.mcpToolsByCwd.set(folder, Promise.resolve(manager.getToolEntries()));
    return state;
  }

  /**
   * Stop every running turn and the MCP servers this instance started. Call
   * before the host process exits; the instance is not usable afterwards.
   */
  async shutdown(): Promise<void> {
    const sessions = new Set([...this.turnRuntimes.values()].map((runtime) => runtime.sessionId));
    await Promise.all([...sessions].map((id) => this.stopAndWait(id, { cancelQueued: true }).catch(() => 0)));
    const managers = [...this.mcpManagersByCwd.values()];
    this.mcpManagersByCwd.clear();
    this.mcpToolsByCwd.clear();
    await Promise.all(managers.map(async (pending) => (await pending.catch(() => null))?.shutdown().catch(() => undefined)));
  }

  /** Configured providers + default model, for a host's model picker. */
  getModelConfig(): {
    defaultProviderId: string;
    defaultModel: string;
    providers: Array<{ id: string; baseURL?: string; hasApiKey: boolean }>;
  } {
    const configured = this.registry.getConfigured().filter((p) => p.enabled);
    return {
      defaultProviderId: this.registry.getDefault()?.id ?? "",
      defaultModel: this.userConfig.getDefaultModel() ?? "",
      providers: configured.map((p) => ({ id: p.id, baseURL: p.baseURL, hasApiKey: Boolean(p.apiKey) })),
    };
  }

  /**
   * One-off text from the model, outside any session: no tools, no system
   * prompt, nothing written to a transcript. For small host chores such as
   * naming a task, where a full agent turn would be wasteful.
   */
  async completeText(options: { prompt: string; model?: string; signal?: AbortSignal }): Promise<string> {
    const { provider, model } = this.resolveProvider(randomUUID(), options.model);
    return provider.complete([{ role: "user", content: options.prompt }], {
      model: decodeModel(model).modelId,
      temperature: 0.2,
      thinkingLevel: "off",
      abortSignal: options.signal,
    });
  }

  // ── The turn ─────────────────────────────────────────────────────────────

  /**
   * Reserve and start a session turn immediately. Execution is driven by the
   * SDK's own pump, not by consumption of the returned iterator: the iterator
   * is a replay subscription, so a host may drop it (or reconnect later via
   * `openSession`) without stalling or cancelling the turn. Cancel with
   * `stop()`, the options signal, or `deleteSession()`.
   */
  runTurn(sessionId: string, options: RunTurnOptions): AsyncGenerator<AgentEvent> {
    const resolved = this.resolveSession(sessionId);
    if (!resolved) throw new Error(`Unknown session: ${sessionId}`);
    const ownerKey = resolved.manager.getSessionFile();
    const owner = processSessionOwners.get(ownerKey);
    if (owner && owner !== this) return owner.runTurn(sessionId, options);
    claimProcessOwner(ownerKey, this);
    try {
      const runtime = this.startOwnedTurn(sessionId, options, resolved, ownerKey);
      return runtime.events.iterate();
    } catch (error) {
      // A synchronous reserve() failure (e.g. pre-aborted signal) must not
      // leave this SDK pinned as process owner of a session it never ran.
      if (!this.turnCoordinator.getState(sessionId).active) {
        releaseProcessOwner(ownerKey, this);
      }
      throw error;
    }
  }

  private startOwnedTurn(
    sessionId: string,
    options: RunTurnOptions,
    resolved: { manager: SessionManager; cwd: string },
    ownerKey: string,
  ): SdkTurnRuntime {
    const reservation = this.turnCoordinator.reserve(sessionId, options.signal);
    const runtime: SdkTurnRuntime = {
      sessionId,
      ownerKey,
      reservation,
      inputController: new AgentRunInputQueue(`sdk-turn-${this.inputIdNonce}-${++this.nextTurnInputPrefix}`),
      outcomes: new Map(),
      options,
      events: new ReplayEventLog<AgentEvent>(),
      started: reservation.phase !== "queued",
      inputRecorded: false,
      finished: Promise.resolve(),
    };
    this.lastTurnOptions.set(sessionId, inheritableTurnOptions(options));
    this.turnRuntimes.set(reservation.id, runtime);
    runtime.finished = this.pumpTurn(runtime, resolved);
    return runtime;
  }

  private async pumpTurn(
    runtime: SdkTurnRuntime,
    resolved: { manager: SessionManager; cwd: string },
  ): Promise<void> {
    let failure: unknown;
    let completedEvent: AgentEvent | undefined;
    try {
      for await (const event of this.runReservedTurn(runtime, runtime.options, resolved)) {
        runtime.events.append(event);
        // Publish success after the pump's finally releases its slot.
        // A host can then distinguish idle from an automatically queued steer.
        if (event.type === "agent_end") completedEvent = event;
        else this.publishSessionEvent(runtime, event);
      }
    } catch (error) {
      failure = error;
      // Setup can fail before runReservedTurn installs its own cleanup.
      for (const event of this.rejectOutstandingSteers(runtime,
        runtime.reservation.signal.aborted ? "turn_cancelled" : "turn_failed")) {
        runtime.events.append(event);
        this.publishSessionEvent(runtime, event);
      }
      // Desktop users already submitted this input. Even if setup was
      // interrupted before Agent.run appended it, keep it for a later resume.
      // Cancelled queued inputs never owned the turn and must stay discarded.
      if (runtime.options.preserveInterruptedInput && runtime.started && !runtime.inputRecorded
        && runtime.reservation.signal.aborted && !this.turnCoordinator.isDeleted(runtime.sessionId)) {
        try {
          this.persistInterruptedInput(resolved, runtime.options.prompt);
        } catch (persistError) {
          failure = persistError;
        }
      }
    } finally {
      runtime.inputController.closePendingInputs();
      // Release only after the interrupted input is durable. Keep the runtime
      // discoverable until the terminal event is published for stopAndWait.
      runtime.reservation.finish();
      if (failure !== undefined) {
        // The session-level log never closes per turn, so replay subscribers
        // need an explicit terminal record — otherwise a failed turn is
        // indistinguishable from one that is still running.
        this.publishSessionEvent(
          runtime,
          { type: "turn_end", willContinue: false },
          {
            kind: failure instanceof AgentAbortError || runtime.reservation.signal.aborted
              ? "cancelled"
              : "failed",
            message: failure instanceof Error ? failure.message : String(failure),
          },
        );
      } else {
        this.publishSessionEvent(runtime, completedEvent ?? { type: "agent_end" }, { kind: "completed" });
      }
      runtime.events.close(failure);
      if (this.turnRuntimes.get(runtime.reservation.id) === runtime) {
        this.turnRuntimes.delete(runtime.reservation.id);
      }
      if (!this.turnCoordinator.getState(runtime.sessionId).active) {
        releaseProcessOwner(runtime.ownerKey, this);
      }
    }
  }

  private async *runReservedTurn(
    runtime: SdkTurnRuntime,
    options: RunTurnOptions,
    resolved: { manager: SessionManager; cwd: string },
  ): AsyncGenerator<AgentEvent> {
    const { sessionId, reservation, inputController } = runtime;
    const { manager: session } = resolved;
    const cwd = options.cwd?.trim() ? resolvePath(options.cwd.trim()) : resolved.cwd;
    const mode: PermissionMode = options.mode ?? "default";
    const planExitMode: Exclude<PermissionMode, "plan"> =
      options.planExitMode === "bypassPermissions" ? "bypassPermissions" : "default";
    const abortSignal = reservation.signal;

    await reservation.waitForStart();
    runtime.started = true;

    let agentRef: Agent | undefined;
    let streamCompleted = false;
    const hookController = new ExternalHookController({ cwd, sessionId });
    await this.resolveProjectTrust(cwd, options, abortSignal);
    // Same allow/deny rules as the TUI (user, project and local settings;
    // repository allow rules only once trusted). Deny rules bind even under
    // bypassPermissions.
    const ruleSet = new SettingsManager(cwd).getMerged().ruleSet;
    // Settles as reject the moment the turn aborts, so a tool blocked on a
    // host approval (or question) can never hang the abort path.
    const abortedDecision = new Promise<ApprovalDecision>((resolve) => {
      const decide = () => resolve({ action: "reject", feedback: "Turn aborted" });
      if (abortSignal.aborted) decide();
      else abortSignal.addEventListener("abort", decide, { once: true });
    });
    const approvalController = new PermissionAwareApprovalController({
      getMode: () => agentRef?.mode ?? mode,
      handlerRef: {
        current: (req: ApprovalRequest) =>
          options.onApproval
            ? Promise.race([options.onApproval(req), abortedDecision])
            : Promise.resolve<ApprovalDecision>({
                action: "reject",
                feedback: "Host provided no approval handler",
              }),
      },
      bashAllowlist: this.bashAllowlistFor(sessionId),
      sessionGrants: this.sessionGrantsFor(sessionId),
      cwd,
      // Read once per turn: edits to settings files apply from the next turn.
      getRuleSet: () => ruleSet,
      externalHooks: hookController,
    });
    const fileStateTracker = new FileStateTracker(cwd);
    const planController: PlanController = {
      getMode: () => agentRef?.mode ?? mode,
      requestApproval: async (plan: string) => {
        const approved = options.onPlanApproval
          ? await awaitWithAbort(options.onPlanApproval(plan), abortSignal)
          : false;
        // exit_plan_mode applies getExitMode() itself on approval.
        return approved
          ? { action: "approve" as const, plan }
          : { action: "reject" as const, reason: "Plan rejected by host" };
      },
      setMode: (m) => agentRef?.setMode(m),
      getExitMode: () => planExitMode,
    };
    const questionController = new QuestionController();
    const unsubscribeQuestions = questionController.subscribe((event) => {
      if (event.type !== "asked") return;
      const request = event.request;
      if (!options.onQuestion) {
        questionController.reject(request.id);
        return;
      }
      options.onQuestion(request).then(
        (answers) =>
          answers ? questionController.reply(request.id, answers) : questionController.reject(request.id),
        () => questionController.reject(request.id),
      );
    });
    abortSignal.addEventListener("abort", () => questionController.rejectAll(), { once: true });

    try {
      const skillRegistry = new SkillRegistry({
        cwd,
        skillPaths: [...this.userConfig.getSkillPaths(), ...(options.skillPaths ?? [])],
        disabledSkills: this.userConfig.getDisabledSkills(),
      });
      const allowedTools = options.allowedTools ? new Set(options.allowedTools) : undefined;
      const offered = (tool: ToolRegistryEntry) => !allowedTools || allowedTools.has(tool.name);
      const tools = createAllTools(cwd, skillRegistry, {
        approvalController,
        fileStateTracker,
        planController,
        questionController,
        goalStore: new GoalStore(),
        checkpoints: () => session.getCheckpoints(),
      }).filter(offered);
      if (!allowedTools || [...allowedTools].some((name) => name.startsWith("mcp__"))) {
        tools.push(
          ...gateMcpTools(await awaitWithAbort(this.mcpToolsFor(cwd), abortSignal), approvalController).filter(offered),
        );
      }
      const hostTools = options.hostTools ?? [];
      const registered = new Set(tools.map(tool => tool.name));
      for (const tool of hostTools) {
        if (registered.has(tool.name)) throw new Error(`Duplicate host tool: ${tool.name}`);
        registered.add(tool.name);
        const guarded = {
          ...tool,
          execute: async (args: Parameters<ToolRegistryEntry["execute"]>[0], ctx: Parameters<ToolRegistryEntry["execute"]>[1]) => {
            throwAbortSignal(abortSignal);
            return tool.execute(args, { ...ctx, abortSignal });
          },
          cloneForChild: (): ToolRegistryEntry => ({ ...tool, enabled: () => false, execute: async () => ({ content: "Host capability is not available to this child.", isError: true }) }),
        };
        tools.push(...(tool.readOnly ? [guarded] : gateMcpTools([guarded], approvalController)));
      }
      throwAbortSignal(abortSignal);

      const promptCacheKey = session.getOrCreatePromptCacheKey();
      if (!this.turnCoordinator.isDeleted(sessionId)) {
        // The session's own folder, not this turn's: a host restart finds
        // the session where it was made.
        session.updateMetadata({ cwd: resolved.cwd });
      }
      const { provider, providerId, model } = this.resolveProvider(promptCacheKey, options.model);
      const thinkingLevel =
        options.thinkingLevel ??
        this.userConfig.getDefaultThinkingLevel() ??
        getDefaultThinkingLevel(providerId, decodeModel(model).modelId);
      // Same ordering as the TUI: the provenance gate must run before memories
      // enter the system prompt.
      purgeUnsafeMemorySources(cwd);
      // A host prompt of its own brings its own context: memories and the
      // skill catalog belong to the coding prompt.
      const memoryPrompt = options.systemPrompt === undefined ? buildMemoryPrompt(cwd) : undefined;
      const skills = tools.some((tool) => tool.name === "skill") ? skillRegistry.summaries() : [];
      const builtSystemPrompt = options.systemPrompt ?? buildSystemPrompt({
        agentName: options.agentName?.trim() || "Bubble",
        skills,
        skillCatalogChars: this.userConfig.getSkillCatalogChars(),
        configuredProvider: providerId || "none",
        configuredModel: model ? displayModel(model) : "none",
        configuredModelId: model || "none",
        thinkingLevel,
        mode,
        workingDir: cwd,
        ...buildToolPromptOptions(tools.filter((t) => !t.deferred)),
        memoryPrompt,
      });
      const systemPrompt = options.appendSystemPrompt
        ? `${builtSystemPrompt}\n\n${options.appendSystemPrompt.trim()}`
        : builtSystemPrompt;

      // Host-driven mode changes (picker, /plan, leaving plan) go into the log
      // as markers, so the rebuilt history states the mode where it changed —
      // after any older tool result that claimed a different mode.
      if (latestRecordedMode(session) !== mode && !this.turnCoordinator.isDeleted(sessionId)) {
        session.appendMarker("mode_switch", mode);
      }
      const history = session.getMessages();
      let contextRevision = session.getRevision();
      // The constructor's leading context minus its mode reminder: the history
      // carries the mode timeline itself (session-log toMessages).
      let leadingContext: Message[] = [];
      // A fence rejection means the durable log diverged from this turn's
      // resident snapshot (a foreign append or clear landed mid-flight). Refuse
      // the write, then roll the resident transcript back to the file's truth so
      // the rejected message does not linger and later writes are not wedged
      // behind the stale revision.
      const persistFenced = (write: () => void): void => {
        try {
          write();
        } catch (error) {
          // Any refused write (fence rejection, busy lock, I/O) leaves the agent's
          // already-pushed message unpersisted. getMessages() refreshes, so read
          // the history first and adopt the revision of that same snapshot.
          try {
            const reloaded = session.getMessages();
            contextRevision = session.getRevision();
            agent.messages = [...leadingContext, ...reloaded];
          } catch { /* surface the original write failure */ }
          throw error;
        }
      };
      const agent = new Agent({
        provider,
        providerId,
        model,
        sessionID: session.getSessionFile(),
        tools,
        systemPrompt,
        temperature: 0.2,
        thinkingLevel,
        mode,
        budgetLedger: new BudgetLedger(),
        fileStateTracker,
        skills,
        skillCatalogChars: this.userConfig.getSkillCatalogChars(),
        memoryPrompt,
        externalHooks: hookController,
        // Cross-provider subagent routes (spawn_agent with "provider:model")
        // need a factory; without one the child blocks with
        // "no provider factory is configured". Default mirrors the TUI's
        // createProviderForRoute (OAuth prepare + configured+enabled+keyed
        // profile lookup); hosts may override per turn.
        providerFactory: options.providerFactory ?? this.defaultProviderFactory(),
        onMessageAppend: (message: Message) => {
          if (message.role === "system" || message.role === "meta") return;
          if (this.turnCoordinator.isDeleted(sessionId)) return;
          if (message.role === "user") runtime.inputRecorded = true;
          persistFenced(() => {
            session.appendMessage(message, contextRevision);
            contextRevision = session.getRevision();
          });
          if (message.role === "assistant") recordMemoryCitations(cwd, message.content);
        },
        onProviderError: (error) => {
          if (this.turnCoordinator.isDeleted(sessionId)) return;
          persistFenced(() => session.appendProviderError(error, contextRevision));
        },
        getContextRevision: () => contextRevision,
        onContextCheckpoint: (checkpoint) => {
          if (this.turnCoordinator.isDeleted(sessionId)) throw new Error("Session deleted before context commit");
          persistFenced(() => {
            session.commitContextCheckpoint(checkpoint, contextRevision);
            contextRevision = session.getRevision();
          });
        },
        onModeUpdate: (m: PermissionMode) => {
          if (!this.turnCoordinator.isDeleted(sessionId)) {
            persistFenced(() => {
              session.appendMarker("mode_switch", m, contextRevision);
              contextRevision = session.getRevision();
            });
          }
        },
      });
      agentRef = agent;

      leadingContext = agent.messages.filter((message) =>
        !(message.role === "meta" && isPermissionModeReminder(message.content)),
      );
      if (history.length > 0) {
        // Leading context (system prompt + deferred-tools reminder, never
        // persisted) stays ahead of the history, at the same position every
        // turn, so the provider's prefix cache covers the previous turn. The
        // mode reminder comes from the history's own mode markers instead, so
        // it is ordered after anything older that mentioned a mode.
        agent.messages = [...leadingContext, ...history];
      }

      reservation.markActive();
      options.onStart?.({
        providerId,
        model,
        thinkingLevel,
        mode,
        tools: tools.map((t) => t.name),
        skills: skills.map((s) => s.name),
      });

      // Match the TUI: a leading "/<skill-name> <task>" becomes an explicit
      // load-this-skill instruction, so invocation never depends on the model
      // recognizing the slash syntax on its own.
      const prompt = rewriteSkillInvocationPrompt(options.prompt, skillRegistry);

      const bareModelId = decodeModel(model).modelId;
      for await (const event of agent.run(prompt, cwd, {
        abortSignal,
        inputController,
      })) {
        const sdkEvent = this.handleSteerEvent(runtime, event, resolved);
        yield attachTurnCost(sdkEvent, providerId, bareModelId);
      }
      streamCompleted = true;
      for (const event of this.rejectOutstandingSteers(runtime, "no_continuation", resolved)) yield event;
    } catch (error) {
      const reason = abortSignal.aborted ? "turn_cancelled" : "turn_failed";
      for (const event of this.rejectOutstandingSteers(runtime, reason)) yield event;
      throw error;
    } finally {
      inputController.closePendingInputs();
      this.rejectOutstandingSteers(
        runtime,
        streamCompleted ? "no_continuation" : abortSignal.aborted ? "turn_cancelled" : "turn_failed",
        streamCompleted ? resolved : undefined,
      );
      unsubscribeQuestions();
      questionController.rejectAll();
    }
  }

  // ── Internals ────────────────────────────────────────────────────────────

  private runtimesForSession(sessionId: string): SdkTurnRuntime[] {
    return [...this.turnRuntimes.values()].filter((runtime) => runtime.sessionId === sessionId);
  }

  private settleSteerFromEvent(runtime: SdkTurnRuntime, event: AgentEvent): void {
    if (event.type !== "input_applied" && event.type !== "input_rejected") return;
    const pending = runtime.outcomes.get(event.id);
    if (!pending) return;
    runtime.outcomes.delete(event.id);
    pending.resolve(event);
  }

  private handleSteerEvent(
    runtime: SdkTurnRuntime,
    event: AgentEvent,
    resolved: { manager: SessionManager; cwd: string },
  ): AgentEvent {
    if (event.type === "input_rejected" && event.reason === "no_continuation") {
      const pending = runtime.outcomes.get(event.id);
      if (pending) return this.queuePendingSteer(runtime, pending, resolved, event);
    }
    this.settleSteerFromEvent(runtime, event);
    return event;
  }

  private rejectOutstandingSteers(
    runtime: SdkTurnRuntime,
    reason: "no_continuation" | "turn_failed" | "turn_cancelled",
    resolved?: { manager: SessionManager; cwd: string },
  ): AgentEvent[] {
    runtime.inputController.closePendingInputs();
    const events: AgentEvent[] = [];
    for (const [id, pending] of runtime.outcomes) {
      const event: Extract<AgentEvent, { type: "input_rejected" }> = {
        type: "input_rejected",
        id,
        content: pending.input.content,
        reason,
        target: "next_turn",
      };
      runtime.outcomes.delete(id);
      if (reason === "no_continuation" && resolved) {
        events.push(this.queuePendingSteer(runtime, pending, resolved, event));
      } else {
        pending.resolve(event);
        events.push(event);
      }
    }
    return events;
  }

  private queuePendingSteer(
    runtime: SdkTurnRuntime,
    pending: PendingSteerOutcome,
    resolved: { manager: SessionManager; cwd: string },
    rejected: Extract<AgentEvent, { type: "input_rejected" }>,
  ): Extract<AgentEvent, { type: "input_queued" | "input_rejected" }> {
    try {
      const queuedRuntime = this.startOwnedTurn(
        runtime.sessionId,
        { ...inheritableTurnOptions(runtime.options), prompt: pending.input.content },
        resolved,
        runtime.ownerKey,
      );
      const queued: Extract<AgentEvent, { type: "input_queued" }> = {
        type: "input_queued",
        id: pending.input.id,
        content: pending.input.content,
        turnId: queuedRuntime.reservation.id,
        target: "next_turn",
      };
      // The follow-up turn's own subscribers also see the queue marker; the
      // session-level log already gets it once via this turn's yielded event.
      queuedRuntime.events.append(queued);
      runtime.outcomes.delete(pending.input.id);
      pending.resolve(queued);
      return queued;
    } catch {
      runtime.outcomes.delete(pending.input.id);
      pending.resolve(rejected);
      return rejected;
    }
  }

  private queueDetachedInput(
    sessionId: string,
    content: string,
    resolved: { manager: SessionManager; cwd: string },
  ): SdkSteerResult {
    const ownerKey = resolved.manager.getSessionFile();
    try {
      claimProcessOwner(ownerKey, this);
      const input: AgentRunInput = {
        id: `sdk-steer-${this.inputIdNonce}-${++this.nextDetachedInputId}`,
        content,
        submittedAt: Date.now(),
      };
      const runtime = this.startOwnedTurn(
        sessionId,
        { ...(this.lastTurnOptions.get(sessionId) ?? {}), prompt: content },
        resolved,
        ownerKey,
      );
      const queued: Extract<AgentEvent, { type: "input_queued" }> = {
        type: "input_queued",
        id: input.id,
        content,
        turnId: runtime.reservation.id,
        target: "next_turn",
      };
      runtime.events.append(queued);
      this.publishSessionEvent(runtime, queued);
      return {
        accepted: true,
        disposition: "queued",
        input,
        turnId: runtime.reservation.id,
        outcome: Promise.resolve(queued),
      };
    } catch {
      releaseProcessOwner(ownerKey, this);
      return { accepted: false, disposition: "rejected", reason: "ownership_conflict" };
    }
  }

  private publishSessionEvent(
    runtime: SdkTurnRuntime,
    event: AgentEvent,
    terminal?: SdkSessionTerminal,
  ): void {
    const log = sessionEventLogFor(runtime.ownerKey, this.sessionEventRetention);
    log.append({
      sequence: log.length + 1,
      sessionId: runtime.sessionId,
      turnId: runtime.reservation.id,
      event,
      ...(terminal ? { terminal } : {}),
    });
  }

  private ownerForSession(sessionId: string): BubbleSdk | undefined {
    const resolved = this.resolveSession(sessionId);
    return resolved ? processSessionOwners.get(resolved.manager.getSessionFile()) : undefined;
  }

  private bashAllowlistFor(sessionId: string): BashAllowlist {
    let allowlist = this.bashAllowlists.get(sessionId);
    if (!allowlist) {
      allowlist = new BashAllowlist();
      this.bashAllowlists.set(sessionId, allowlist);
    }
    return allowlist;
  }

  private sessionGrantsFor(sessionId: string): Set<string> {
    let grants = this.sessionGrants.get(sessionId);
    if (!grants) {
      grants = new Set();
      this.sessionGrants.set(sessionId, grants);
    }
    return grants;
  }

  /**
   * Default cross-provider subagent factory over this SDK instance's own
   * registry — the same semantics as the TUI's createProviderForRoute. The
   * registry is reloaded first so a key added after construction works.
   */
  private defaultProviderFactory(): (route: ResolvedSubagentRoute) => Promise<Provider> {
    return async (route) => {
      const providerId = route.providerId;
      if (!providerId) {
        throw new Error(`Subagent route for model "${route.model}" did not include a provider.`);
      }
      // getConfigured() reads config.json from disk every call, so keys
      // added after SDK construction are picked up without a reload call.
      const target = this.registry.getConfigured().find((item) => item.id === providerId);
      if (target?.authType === "oauth") {
        await this.registry.prepareProvider(providerId);
      }
      if (!target?.enabled || !target.apiKey) {
        throw new Error(
          `Subagent route requires provider "${providerId}", but it is not configured or has no active credentials.`,
        );
      }
      return createProviderInstance({
        providerId,
        apiKey: target.apiKey,
        baseURL: target.baseURL,
        protocol: target.protocol,
        headers: target.headers,
        openAICodexAuth: this.registry.createOpenAICodexAuthAdapter(providerId),
        grokAuth: this.registry.createGrokAuthAdapter(providerId),
      });
    };
  }

  /**
   * MCP servers are started lazily, once per cwd, and restarted only when the
   * repository's settings get trusted (resolveProjectTrust).
   */
  private mcpToolsFor(cwd: string): Promise<ToolRegistryEntry[]> {
    if (!this.mcpEnabled) return Promise.resolve([]);
    let cached = this.mcpToolsByCwd.get(cwd);
    if (!cached) {
      const managerPromise = (async () => {
        const loaded = loadMcpConfig({ cwd });
        if (loaded.servers.length === 0) return null;
        const manager = new McpManager({ servers: loaded.servers });
        await manager.start();
        return manager;
      })().catch(() => null);
      this.mcpManagersByCwd.set(cwd, managerPromise);
      cached = managerPromise.then((manager) => manager?.getToolEntries() ?? []);
      this.mcpToolsByCwd.set(cwd, cached);
    }
    return cached;
  }

  private resolveSession(sessionId: string): { manager: SessionManager; cwd: string } | undefined {
    if (this.turnCoordinator.isDeleted(sessionId)) return undefined;
    const memCwd = this.cwdBySession.get(sessionId);
    if (memCwd) {
      // create() only resolves the path and loads iff the file exists, so this
      // covers both fresh (lazily persisted, not yet on disk) and resumed sessions.
      return { manager: SessionManager.create(memCwd, sessionFileName(sessionId)), cwd: memCwd };
    }
    if (this.sessionIndex.size === 0) this.listSessions();
    const entry = this.sessionIndex.get(sessionId);
    if (!entry) {
      // A session created in this process but not yet persisted to disk is
      // still resolvable (and routable) for other SDK instances.
      const registeredCwd = processSessionLocations.get(sessionId);
      if (!registeredCwd) return undefined;
      this.cwdBySession.set(sessionId, registeredCwd);
      return {
        manager: SessionManager.create(registeredCwd, sessionFileName(sessionId)),
        cwd: registeredCwd,
      };
    }
    // The disk entry supersedes the pre-persistence registry entry.
    processSessionLocations.delete(sessionId);
    this.cwdBySession.set(sessionId, entry.cwd);
    return { manager: new SessionManager(entry.file), cwd: entry.cwd };
  }

  private resolveProvider(
    promptCacheKey: string,
    explicitModel?: string,
  ): { provider: Provider; providerId: string; model: string } {
    const configuredModel = explicitModel || this.userConfig.getDefaultModel();
    const defaultProvider = this.registry.getDefault();
    const fallbackProviderId = defaultProvider?.id ?? "";
    const normalized = configuredModel
      ? configuredModel.includes(":")
        ? configuredModel
        : fallbackProviderId
          ? encodeModel(fallbackProviderId, configuredModel)
          : ""
      : "";
    const { providerId: effId, modelId: effModelId } = normalized
      ? decodeModel(normalized)
      : { providerId: undefined, modelId: "" };
    const activeProviderId = effId || fallbackProviderId;
    if (effModelId) assertProviderModelAllowed(activeProviderId, effModelId);
    const target =
      this.registry.getConfigured().find((p) => p.id === activeProviderId) || defaultProvider;
    if (!target?.apiKey) {
      throw new Error("No provider with an API key configured — run the Bubble CLI once to set one up");
    }
    const activeModel = effModelId ? encodeModel(activeProviderId, effModelId) : "";
    const provider = createProviderInstance({
      providerId: activeProviderId,
      apiKey: target.apiKey,
      baseURL: target.baseURL,
      promptCacheKey,
      protocol: target.protocol,
      headers: target.headers,
      // Main SDK turns need the same per-request OAuth renewal as the TUI
      // and child provider factory, including calls after a long-running tool.
      openAICodexAuth: this.registry.createOpenAICodexAuthAdapter(activeProviderId),
      grokAuth: this.registry.createGrokAuthAdapter(activeProviderId),
    });
    return { provider, providerId: activeProviderId, model: activeModel };
  }
}

/**
 * Process-local ownership routes every producer to one SDK runner. Hosts that
 * run multiple OS processes still need a broker or distributed per-session
 * lease; two processes cannot safely share a local JSONL transcript directly.
 */
const processSessionOwners = new Map<string, BubbleSdk>();
const sessionEventLogs = new Map<string, ReplayEventLog<SdkSessionEvent>>();
/** sessionId -> cwd for sessions created in this process but not yet on disk. */
const processSessionLocations = new Map<string, string>();

function claimProcessOwner(ownerKey: string, sdk: BubbleSdk): void {
  const existing = processSessionOwners.get(ownerKey);
  if (existing && existing !== sdk) {
    throw new Error(`SDK session is owned by another runner: ${ownerKey}`);
  }
  processSessionOwners.set(ownerKey, sdk);
}

function releaseProcessOwner(ownerKey: string, sdk: BubbleSdk): void {
  if (processSessionOwners.get(ownerKey) === sdk) processSessionOwners.delete(ownerKey);
}

function sessionEventLogFor(ownerKey: string, retention = Infinity): ReplayEventLog<SdkSessionEvent> {
  let log = sessionEventLogs.get(ownerKey);
  if (!log) {
    log = new ReplayEventLog<SdkSessionEvent>(retention);
    sessionEventLogs.set(ownerKey, log);
  }
  return log;
}

/** End live iterators and drop the buffered events of a deleted session. */
function releaseSessionEventLog(ownerKey: string): void {
  const log = sessionEventLogs.get(ownerKey);
  if (!log) return;
  log.close();
  sessionEventLogs.delete(ownerKey);
}

function inheritableTurnOptions(
  options: RunTurnOptions,
): Omit<RunTurnOptions, "prompt" | "signal"> {
  const { prompt: _prompt, signal: _signal, ...inherited } = options;
  return inherited;
}

function sessionFileName(sessionId: string): string {
  return sessionId.endsWith(".jsonl") ? sessionId : `${sessionId}.jsonl`;
}

function pendingSteerOutcome(input: AgentRunInput): PendingSteerOutcome {
  let resolve!: (outcome: SdkSteerOutcome) => void;
  const promise = new Promise<SdkSteerOutcome>((done) => {
    resolve = done;
  });
  return { input, promise, resolve };
}

function throwAbortSignal(signal: AbortSignal): void {
  if (!signal.aborted) return;
  if (signal.reason instanceof Error) throw signal.reason;
  throw new AgentAbortError(typeof signal.reason === "string" ? signal.reason : "SDK turn cancelled.");
}

/** The mode the session log last recorded ("default" before any switch). */
function latestRecordedMode(session: SessionManager): PermissionMode {
  const entries = session.getEntries();
  for (let index = entries.length - 1; index >= 0; index--) {
    const entry = entries[index];
    if (entry.type === "marker" && entry.kind === "mode_switch"
      && (entry.value === "default" || entry.value === "plan" || entry.value === "bypassPermissions")) {
      return entry.value;
    }
  }
  return "default";
}

function awaitWithAbort<T>(promise: Promise<T>, signal: AbortSignal): Promise<T> {
  if (signal.aborted) {
    try {
      throwAbortSignal(signal);
    } catch (error) {
      return Promise.reject(error);
    }
  }
  return new Promise<T>((resolve, reject) => {
    const onAbort = () => {
      cleanup();
      try {
        throwAbortSignal(signal);
      } catch (error) {
        reject(error);
      }
    };
    const cleanup = () => signal.removeEventListener("abort", onAbort);
    signal.addEventListener("abort", onAbort, { once: true });
    promise.then(
      (value) => {
        cleanup();
        resolve(value);
      },
      (error) => {
        cleanup();
        reject(error);
      },
    );
  });
}

/**
 * Attach a `cost` field to turn_end events that carry usage, priced via the
 * static pricing table. Pricing is an accounting concern, so it rides along
 * here in the SDK rather than in the agent core. Multi-step turns emit several
 * turn_end events; each cost covers only that step's usage, so hosts sum them.
 * Events for unpriced models (or without usage) pass through untouched.
 */
export function attachTurnCost(
  event: AgentEvent,
  providerId: string,
  modelId: string,
  at: Date = new Date(),
): AgentEvent {
  if (event.type !== "turn_end" || !event.usage) return event;
  const cost = calculateUsageCost(providerId, modelId, event.usage, at);
  return cost ? { ...event, cost } : event;
}

/**
 * If the prompt (or its first text part) is a "/<skill-name> <task>" skill
 * invocation, rewrite it into the same explicit execution prompt the TUI
 * sends. Non-matching prompts pass through untouched.
 */
export function rewriteSkillInvocationPrompt(
  prompt: string | ContentPart[],
  registry: SkillRegistry,
): string | ContentPart[] {
  if (typeof prompt === "string") {
    return parseSkillInvocation(prompt, registry)?.actualPrompt ?? prompt;
  }
  const index = prompt.findIndex((part) => part.type === "text");
  if (index === -1) return prompt;
  const part = prompt[index] as Extract<ContentPart, { type: "text" }>;
  const rewritten = parseSkillInvocation(part.text, registry)?.actualPrompt;
  if (!rewritten) return prompt;
  const next = prompt.slice();
  next[index] = { ...part, text: rewritten };
  return next;
}

// ── Building blocks (escape hatch for custom hosts) ────────────────────────

export { Agent, AgentAbortError, type AgentOptions, type AgentRunOptions } from "../agent.js";
export { AgentRunInputQueue } from "../agent/input-controller.js";
export {
  SessionManager,
  getSessionsDir,
  type SessionSummary,
  type UserTurn,
  type RewindResult,
} from "../session.js";
export { PermissionAwareApprovalController } from "../approval/controller.js";
export { BashAllowlist } from "../approval/session-cache.js";
export type { ApprovalController, ApprovalDecision, ApprovalRequest } from "../approval/types.js";
export type { RepoCapabilities } from "../permissions/trust.js";
export { createAllTools, buildToolPromptOptions, type PlanController } from "../tools/index.js";
export { buildSystemPrompt } from "../system-prompt.js";
export { FileStateTracker } from "../tools/file-state.js";
export { BudgetLedger } from "../agent/budget-ledger.js";
export { UserConfig } from "../config.js";
export {
  ProviderRegistry,
  encodeModel,
  decodeModel,
  displayModel,
  type ProviderProfile,
  type ModelInfo,
} from "../provider-registry.js";
export { createProviderInstance } from "../provider.js";
export { getDefaultThinkingLevel } from "../variant/variant-resolver.js";
export {
  QuestionController,
  QuestionRejectedError,
  type QuestionAnswer,
  type QuestionEvent,
  type QuestionRequest,
} from "../question/controller.js";
export { SkillRegistry } from "../skills/registry.js";
export { parseSkillInvocation, type SkillInvocation } from "../skills/invocation.js";
export type { SkillSummary } from "../skills/types.js";
export { GoalStore } from "../goal/store.js";
export { McpManager } from "../mcp/manager.js";
export type { McpServerState, McpServerStatus } from "../mcp/types.js";
export { loadMcpConfig } from "../mcp/config.js";
export { ExternalHookController } from "../hooks/controller.js";
export {
  calculateUsageCost,
  getModelPricing,
  type ModelPricing,
  type PricingCurrency,
} from "../model-pricing.js";
export { registry as slashCommandRegistry } from "../slash-commands/index.js";
export type { SlashCommandContext } from "../slash-commands/types.js";
export type {
  AgentEvent,
  AgentInputController,
  AgentRunInput,
  ContentPart,
  Message,
  PermissionMode,
  Provider,
  ThinkingLevel,
  TokenUsage,
  UsageCost,
  ToolRegistryEntry,
  ToolResult,
  ToolUpdate,
} from "../types.js";
