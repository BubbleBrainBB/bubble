import { createDesignTools } from '../../design/tools';
import { createWidgetHostTools } from '../widget-tool';
import { isShowWidgetToolName, WIDGET_STREAMING_FLAG } from '../../../shared/widget';
import { settleDesignTurn } from '../../design/service';
import { assertBubbleSessionReader } from '../bubble-session-reader';
import { getBubbleModelConfig } from '../bubble-settings';
import { bubbleModelSelectionError } from '../../../shared/bubble-model-selection';
import { isProjectFileApproval } from './project-access';
import { EventEmitter } from 'events';
import { readFile } from 'fs/promises';
import { v4 as uuidv4 } from 'uuid';
import type {
  AcpPermissionInput,
  AskUserQuestionInput,
  Attachment,
  BubblePermissionMode,
  BubblePlanExitMode,
  BubbleSubagentState,
  ContentBlock,
  PermissionResult,
  ProviderListSkillsInput,
  ProviderListSkillsResult,
  StreamMessage,
  Usage,
} from '../../../shared/types';
import { buildPromptText } from './bubble-prompt-text';
import { partialJsonStringFields, toolInputPreview } from './tool-input-preview';
import { StreamDeltaCoalescer } from '../../../shared/stream-delta-coalescer';
import type {
  ProviderAdapter,
  ProviderAdapterCapabilities,
  ProviderKind,
  ProviderRuntimeEvent,
  ProviderSendTurnInput,
  ProviderSession,
  ProviderSessionStartInput,
  ProviderSessionStatus,
} from './types';
import {
  getBubbleSdk,
  getBubbleSessionManager,
  bundledSkillPaths,
  type BubbleAgentEvent,
  type BubbleApprovalDecision,
  type BubbleApprovalRequest,
  type BubbleRepoCapabilities,
  type BubbleContentPart,
  type BubbleQuestionPrompt,
  type BubbleQuestionRequest,
  type BubbleSdkInstance,
  type BubbleSubagentUpdate,
  type BubbleTokenUsage,
} from './bubble-sdk-loader';
import type { ProviderRewindAnchor, ProviderRewindResult } from './types';

const ONE_SHOT_TIMEOUT_MS = 120_000;

const CAPABILITIES: ProviderAdapterCapabilities = {
  sessionModelSwitch: true,
  skillDiscovery: true,
  pluginDiscovery: false,
  mcpServers: true,
  imageAttachments: true,
  forkThread: false,
  compactThread: true,
  planMode: true,
};

type PendingBubbleRequest =
  | { kind: 'approval'; resolve: (decision: BubbleApprovalDecision) => void }
  | {
      kind: 'question';
      questions: BubbleQuestionPrompt[];
      resolve: (answers: string[][] | null) => void;
    }
  | { kind: 'plan'; resolve: (approved: boolean) => void };

type BubbleAssistantAccumulator = {
  uuid: string;
  text: string;
  thinking: string;
  createdAt: number;
};

type PendingBubblePreparation = {
  finished: Promise<void>;
  resolve: () => void;
  error?: unknown;
};

function pendingPreparation(): PendingBubblePreparation {
  let resolve!: () => void;
  const finished = new Promise<void>(done => { resolve = done; });
  return { finished, resolve };
}

// Cancelling metadata lookup must not wait for a remote catalog response.
async function duringSetup<T>(work: Promise<T>, signal: AbortSignal): Promise<T> {
  let cancel!: () => void;
  const aborted = new Promise<never>((_resolve, reject) => {
    cancel = () => reject(new Error('Bubble task stopped during preparation.'));
    if (signal.aborted) cancel();
    else signal.addEventListener('abort', cancel, { once: true });
  });
  try { return await Promise.race([work, aborted]); }
  finally { signal.removeEventListener('abort', cancel); }
}

type BubbleWidgetStream = {
  args: string;
  lastEmitAt: number;
  sent?: string;
  timer?: ReturnType<typeof setTimeout>;
};

/** A streaming widget redraws at most this often (each preview carries the whole partial HTML). */
const WIDGET_PREVIEW_INTERVAL_MS = 100;

type ActiveBubbleSession = {
  threadId: string;
  providerSessionId: string;
  status: ProviderSessionStatus;
  cwd: string;
  model?: string;
  permissionMode?: BubblePermissionMode;
  /** Mode restored when a plan is approved (the composer's non-plan mode). */
  planExitMode?: BubblePlanExitMode;
  /** Composer-selected thinking level (open set); undefined = SDK/model default. */
  thinkingLevel?: string;
  /** Model context window from the provider registry (Bubble's turn usage carries none). */
  contextWindow?: number | null;
  lastContextTokens?: number;
  turnActive: boolean;
  abortController: AbortController | null;
  setupAbort: AbortController;
  startup: PendingBubblePreparation;
  preparation?: PendingBubblePreparation;
  pendingRequests: Map<string, PendingBubbleRequest>;
  currentAssistant: BubbleAssistantAccumulator | null;
  pendingRetryId?: string;
  emittedToolCallIds: Set<string>;
  streamingToolCalls?: Map<string, { createdAt: number; input: Record<string, string>; name: string; parentToolUseId?: string }>;
  /** show_widget calls being written: their latest arguments and preview pacing. */
  widgetStreams?: Map<string, BubbleWidgetStream>;
  emittedToolResultIds: Set<string>;
  usage: Usage;
  totalCostUsd: number;
  durationStartMs: number;
  durationEndMs?: number;
  /** Subagent streams keyed by the SPAWNING tool_call id (the UI's nesting
   * key). Mirrors the Kimi adapter's subagentStreams/subagentParents pair. */
  subagentStreams: Map<string, BubbleSubagentStream>;
  /** tool_call id -> first-seen timestamp; anchors duration for finished lanes. */
  subagentStartedAt: Map<string, number>;
  subagentStates?: Map<string, BubbleSubagentState>;
  /** tool_call id -> tool name, so spawn results can be identified later. */
  toolNames: Map<string, string>;
  /** spawn_agent/run_workflow results held back until the lane's terminal
   * subagent_update — these tools are fire-and-forget (the SDK returns
   * "Spawned X (queued)" immediately), so emitting the tool_result right
   * away would flip the UI lane to Done while the child is still running. */
  heldSpawnResults: Map<string, { content?: string; isError?: boolean }>;
};

/** Accumulated child narration under one spawn lane (nested transcript). */
type BubbleSubagentStream = {
  text: string;
  thinking: string;
  toolCallIds: Set<string>;
};

function isRecord(value: unknown): value is Record<string, unknown> {
  return !!value && typeof value === 'object' && !Array.isArray(value);
}

function getString(value: unknown): string {
  return typeof value === 'string' ? value : '';
}

function getNumber(value: unknown): number | undefined {
  return typeof value === 'number' && Number.isFinite(value) ? value : undefined;
}

function normalizeToolName(toolName: string): string {
  const normalized = toolName.trim();
  if (!normalized) {
    return 'Tool';
  }
  const compact = normalized.replace(/[_\-\s]/g, '').toLowerCase();
  if (compact === 'bash' || compact === 'shell') return 'Bash';
  if (compact === 'read') return 'Read';
  if (compact === 'write') return 'Write';
  if (compact === 'edit') return 'Edit';
  if (compact === 'grep') return 'Grep';
  if (compact === 'glob') return 'Glob';
  if (compact === 'ls') return 'LS';
  if (compact === 'todowrite') return 'TodoWrite';
  if (compact === 'webfetch') return 'WebFetch';
  if (compact === 'websearch') return 'WebSearch';
  if (compact === 'question') return 'AskUserQuestion';
  if (compact === 'exitplanmode') return 'ExitPlanMode';
  return normalized;
}

function parseToolArguments(raw: string): Record<string, unknown> {
  if (!raw.trim()) {
    return {};
  }
  try {
    const parsed = JSON.parse(raw) as unknown;
    return isRecord(parsed) ? parsed : { value: parsed };
  } catch {
    return { raw };
  }
}

function usageFromBubble(
  usage: BubbleTokenUsage | undefined,
  contextWindow?: number | null
): Usage | null {
  if (!usage) {
    return null;
  }
  const input = Math.max(0, Math.round(getNumber(usage.promptTokens) || 0));
  const output = Math.max(0, Math.round(getNumber(usage.completionTokens) || 0));
  const cacheRead = Math.max(0, Math.round(getNumber(usage.promptCacheHitTokens) || 0));
  const cacheWrite = Math.max(0, Math.round(getNumber(usage.cacheCreationTokens) || 0));
  const total = getNumber(usage.totalTokens);
  return {
    input_tokens: input,
    output_tokens: output,
    cache_read_input_tokens: cacheRead,
    cache_creation_input_tokens: cacheWrite,
    total_tokens: total !== undefined ? Math.round(total) : input + output + cacheRead + cacheWrite,
    context_window: contextWindow || null,
  };
}

function addUsage(target: Usage, usage: Usage | null): void {
  if (!usage) {
    return;
  }
  target.input_tokens += usage.input_tokens || 0;
  target.output_tokens += usage.output_tokens || 0;
  target.cache_read_input_tokens =
    (target.cache_read_input_tokens || 0) + (usage.cache_read_input_tokens || 0);
  target.cache_creation_input_tokens =
    (target.cache_creation_input_tokens || 0) + (usage.cache_creation_input_tokens || 0);
  target.total_tokens = (target.total_tokens || 0) + (usage.total_tokens || 0);
  if (usage.context_window) {
    target.context_window = usage.context_window;
  }
}

function createEmptyUsage(): Usage {
  return {
    input_tokens: 0,
    output_tokens: 0,
    cache_read_input_tokens: 0,
    cache_creation_input_tokens: 0,
    total_tokens: 0,
    context_window: null,
  };
}

// Bubble's ContentPart uses OpenAI-style image_url parts, so attachments are
// inlined as base64 data URLs.
async function buildPromptParts(
  text: string,
  attachments: Attachment[] | undefined
): Promise<string | BubbleContentPart[]> {
  const imageAttachments = attachments?.filter((attachment) => attachment.kind === 'image') || [];
  if (imageAttachments.length === 0) {
    return text;
  }
  const parts: BubbleContentPart[] = [];
  if (text.trim()) {
    parts.push({ type: 'text', text });
  }
  for (const attachment of imageAttachments) {
    const buffer = await readFile(attachment.path);
    const mimeType = attachment.mimeType || 'image/png';
    parts.push({
      type: 'image_url',
      image_url: { url: `data:${mimeType};base64,${buffer.toString('base64')}` },
    });
  }
  return parts;
}

function describeApproval(request: BubbleApprovalRequest): { toolName: string; question: string } {
  const type = getString(request.type);
  const path = getString(request.path);
  switch (type) {
    case 'bash': {
      const command = getString(request.command);
      return { toolName: 'Bash', question: command ? `Run command: ${command}` : 'Run a command' };
    }
    case 'edit':
      return { toolName: 'Edit', question: path ? `Edit ${path}` : 'Edit a file' };
    case 'write':
      return { toolName: 'Write', question: path ? `Write ${path}` : 'Write a file' };
    case 'patch': {
      const paths = Array.isArray(request.paths)
        ? request.paths.map((value) => getString(value)).filter(Boolean)
        : [];
      return {
        toolName: 'Edit',
        question: paths.length > 0 ? `Apply patch to ${paths.join(', ')}` : 'Apply a patch',
      };
    }
    case 'lsp':
      return {
        toolName: 'LSP',
        question: `Run LSP ${getString(request.operation) || 'operation'}${path ? ` on ${path}` : ''}`,
      };
    case 'agent_profile':
      return {
        toolName: 'AgentProfile',
        question: `Trust project agent profile "${getString(request.name) || 'unknown'}"`,
      };
    case 'external_tool':
      return {
        toolName: getString(request.title) || 'Tool',
        question: getString(request.title) || 'Run an external tool',
      };
    default:
      return { toolName: type || 'Tool', question: 'Bubble is requesting permission' };
  }
}

const APPROVE_SESSION_OPTION_ID = 'approve_session';

function describeRepoCapabilities(caps: BubbleRepoCapabilities): string[] {
  return [
    ...caps.allow.map((rule) => `allow rule ${rule}`),
    ...caps.mcpServers.map((name) => `MCP server ${name} (Bubble starts it)`),
    ...caps.lspServers.map((id) => `LSP server ${id} (custom command, env or options)`),
  ];
}


function buildApprovalInput(request: BubbleApprovalRequest): AcpPermissionInput {
  const described = describeApproval(request);
  // The SDK marks settings/.git writes as protected: say why this edit asks.
  const question = request.protectedPath === true
    ? `${described.question} (protected file: can change permissions or run code)`
    : described.question;
  // The SDK only sets sessionGrant when one approval can safely cover later
  // calls (a single simple bash command, an MCP tool).
  const sessionGrant = getString(request.sessionGrant);
  return {
    kind: 'acp-permission',
    provider: 'bubble',
    question,
    title: question,
    toolName: described.toolName,
    options: [
      { optionId: 'approve', name: 'Approve', kind: 'allow_once' },
      ...(sessionGrant
        ? [{ optionId: APPROVE_SESSION_OPTION_ID, name: `Always allow ${sessionGrant} this session`, kind: 'allow_always' }]
        : []),
      { optionId: 'reject', name: 'Reject', kind: 'reject_once' },
    ],
    toolCall: request,
  };
}

function buildQuestionInput(request: BubbleQuestionRequest): AskUserQuestionInput {
  return {
    questions: request.questions.map((question) => ({
      question: question.question,
      ...(question.header?.trim() ? { header: question.header.trim() } : {}),
      options: question.options.map((option) => ({
        label: option.label,
        ...(option.description?.trim() ? { description: option.description.trim() } : {}),
      })),
      ...(question.multiple === true ? { multiSelect: true } : {}),
    })),
  };
}

function splitQuestionAnswer(value: string): string[] {
  return value
    .split(',')
    .map((answer) => answer.trim())
    .filter(Boolean);
}

function buildQuestionAnswers(
  questions: BubbleQuestionPrompt[],
  decision: PermissionResult
): string[][] {
  const answers = isRecord(decision.updatedInput?.answers)
    ? (decision.updatedInput.answers as Record<string, unknown>)
    : null;
  return questions.map((question) => splitQuestionAnswer(getString(answers?.[question.question])));
}

export class BubbleSdkAdapter implements ProviderAdapter {
  readonly provider: ProviderKind = 'bubble';
  readonly displayName = 'Bubble';
  readonly capabilities = CAPABILITIES;
  readonly events = new EventEmitter();

  private sessions = new Map<string, ActiveBubbleSession>();
  private readonly streamMessages = new StreamDeltaCoalescer();

  constructor() {
    this.streamMessages.setEmitter(({ sessionId, message }) => {
      this.events.emit('event', { type: 'message', threadId: sessionId, message });
    });
  }

  async startSession(input: ProviderSessionStartInput): Promise<ProviderSession> {
    const cwd = input.cwd || process.cwd();
    // Publish a stable cursor before the first await. The desktop can persist
    // it even if the user stops while the SDK itself is still loading.
    const providerSessionId = input.resumeSessionId?.trim() || `sdk-${uuidv4()}`;
    const previous = this.sessions.get(input.threadId);
    this.disposeSession(input.threadId);
    const session: ActiveBubbleSession = {
      threadId: input.threadId,
      providerSessionId,
      status: 'running',
      cwd,
      model: input.model,
      permissionMode: input.bubblePermissionMode,
      planExitMode: input.bubblePlanExitMode,
      thinkingLevel: input.bubbleThinkingLevel?.trim() || undefined,
      turnActive: false,
      abortController: null,
      setupAbort: new AbortController(),
      startup: pendingPreparation(),
      pendingRequests: new Map(),
      currentAssistant: null,
      emittedToolCallIds: new Set(),
      emittedToolResultIds: new Set(),
      usage: createEmptyUsage(),
      totalCostUsd: 0,
      durationStartMs: Date.now(),
      subagentStreams: new Map(),
      subagentStartedAt: new Map(),
      toolNames: new Map(),
      heldSpawnResults: new Map(),
    };
    this.sessions.set(input.threadId, session);

    try {
      this.emit({
        type: 'system_init',
        threadId: input.threadId,
        sessionId: providerSessionId,
        model: session.model,
      });
      const sdk = await getBubbleSdk(cwd);
      if (previous) {
        await previous.startup.finished;
        await previous.preparation?.finished;
        await sdk.stopAndWait(previous.providerSessionId, { cancelQueued: true });
      }
      if (input.prompt || input.attachments?.length) {
        this.resolveSessionId(sdk, input.resumeSessionId, cwd, providerSessionId);
        await this.sendTurnForSession(session, {
          threadId: input.threadId,
          prompt: input.prompt,
          attachments: input.attachments,
          model: input.model,
          bubblePermissionMode: input.bubblePermissionMode,
          bubblePlanExitMode: input.bubblePlanExitMode,
          bubbleThinkingLevel: input.bubbleThinkingLevel,
        });
      } else if (!session.setupAbort.signal.aborted) {
        this.assertConfigured(sdk);
        session.model = await duringSetup(this.resolveCatalogModel(sdk, input.model), session.setupAbort.signal);
        this.resolveSessionId(sdk, input.resumeSessionId, cwd, providerSessionId);
      }
      return {
        threadId: input.threadId,
        provider: 'bubble',
        providerSessionId,
        status: session.status,
        model: session.model,
      };
    } catch (error) {
      session.startup.error = error;
      // A failed fresh start must not leave an unusable resume cursor in the
      // desktop database. Stopped starts keep theirs for their saved input.
      if (!input.resumeSessionId && !session.setupAbort.signal.aborted) {
        this.emit({ type: 'system_init', threadId: input.threadId, sessionId: '', model: session.model });
      }
      if (this.sessions.get(input.threadId) === session) this.sessions.delete(input.threadId);
      throw error;
    } finally {
      session.startup.resolve();
    }
  }

  async sendTurn(input: ProviderSendTurnInput): Promise<void> {
    const session = this.sessions.get(input.threadId);
    if (!session) {
      throw new Error(`No Bubble session found for thread "${input.threadId}"`);
    }
    if (session.setupAbort.signal.aborted) throw new Error('Bubble session is stopping. Please retry after it stops.');
    await this.sendTurnForSession(session, input);
  }

  private async sendTurnForSession(session: ActiveBubbleSession, input: ProviderSendTurnInput): Promise<void> {
    if (session.turnActive) {
      // The SDK owns admission and the end-of-turn race: unapplied steering
      // automatically becomes a new turn, observed by the session event loop.
      if (input.attachments?.length) throw new Error('Queue attachments until the current Bubble turn finishes.');
      const sdk = await getBubbleSdk(session.cwd);
      if (this.sessions.get(input.threadId) !== session || session.status === 'stopped') {
        throw new Error('Bubble session stopped before steering could be delivered.');
      }
      if (session.turnActive) {
        this.flushAssistant(session, 'commentary');
        const result = sdk.steer(session.providerSessionId, input.prompt);
        if (!result.accepted) throw new Error(`Bubble could not accept steering: ${result.reason}`);
        return;
      }
      // The observed turn settled during SDK loading; send a normal turn.
    }

    // Manual `/compact` is handled locally (mirrors Claude Code): it compacts
    // the persisted session and emits a compact_boundary instead of a model
    // turn. The pre-reset usage snapshot is the best preTokens approximation.
    const trimmedPrompt = input.prompt.trim();
    if (!input.attachments?.length && trimmedPrompt.toLowerCase() === '/compact') {
      await this.runCompact(session, session.lastContextTokens || 0);
      return;
    }

    if (session.preparation) throw new Error('Bubble is already preparing a turn. Please retry.');
    const preparation = pendingPreparation();
    session.preparation = preparation;
    try {
      const sdk = await getBubbleSdk(session.cwd);
      // Finish materializing attachments even when stopped: the continuation
      // must retain their bytes, not depend on an attachment file still existing.
      const text = buildPromptText(input.prompt, input.attachments);
      const prompt = await buildPromptParts(text, input.attachments);
      if (typeof prompt === 'string' && !prompt.trim()) return;
      let model: string;
      try {
        if (session.setupAbort.signal.aborted) throw new Error('Preparation stopped');
        this.assertConfigured(sdk);
        model = await duringSetup(this.resolveCatalogModel(sdk, input.model ?? session.model), session.setupAbort.signal);
      } catch (error) {
        if (!session.setupAbort.signal.aborted) throw error;
        sdk.recordInterruptedInput(session.providerSessionId, prompt);
        return;
      }
      if (session.setupAbort.signal.aborted) {
        sdk.recordInterruptedInput(session.providerSessionId, prompt);
        return;
      }
      if (session.turnActive || this.sessions.get(input.threadId) !== session) {
        throw new Error('Bubble session changed while preparing the turn. Please retry.');
      }
      session.model = model;
      if (input.bubblePermissionMode) {
        session.permissionMode = input.bubblePermissionMode;
        // Paired with the mode: a send that omits the exit mode falls back to
        // default instead of inheriting an earlier send's escalation.
        session.planExitMode = input.bubblePlanExitMode;
      }
      // Per-turn thinking level: a string switches it; undefined keeps the
      // session's current level (the warm envelope omits it for non-bubble
      // turns and when the composer has no selection — both mean "default").
      if (typeof input.bubbleThinkingLevel === 'string') {
        session.thinkingLevel = input.bubbleThinkingLevel.trim() || undefined;
      }
      session.status = 'running';
      session.turnActive = true;
      session.durationStartMs = Date.now();
      session.durationEndMs = undefined;
      session.usage = createEmptyUsage();
      session.totalCostUsd = 0;
      session.currentAssistant = null;
      session.pendingRetryId = undefined;
      // Reserve SDK execution before broadcasting running, so an immediate
      // follow-up inherits this turn's model and approval callbacks.
      void this.runTurnLoop(session, prompt, session.model, sdk);
    } catch (error) {
      preparation.error = error;
      throw error;
    } finally {
      preparation.resolve();
      if (session.preparation === preparation) session.preparation = undefined;
    }
  }

  async stopSession(threadId: string): Promise<void> {
    const session = this.sessions.get(threadId);
    if (!session) {
      return;
    }
    session.status = 'stopped';
    session.setupAbort.abort();
    session.abortController?.abort();
    this.dismissAllRequests(session);
    const preparing = session.preparation;
    try {
      await session.startup.finished;
      await preparing?.finished;
      if (session.startup.error) throw session.startup.error;
      if (preparing?.error) throw preparing.error;
      const sdk = await getBubbleSdk(session.cwd);
      // The replacement runner awaits this promise. SDK stop() only requests
      // cancellation; wait for durable history and the old terminal event too.
      await sdk.stopAndWait(session.providerSessionId, { cancelQueued: true });
    } finally {
      if (this.sessions.get(threadId) === session) {
        this.sessions.delete(threadId);
        this.emit({ type: 'status_change', threadId, status: 'stopped' });
      }
    }
  }

  disposeSession(threadId: string): boolean {
    this.streamMessages.discardSession(threadId);
    const session = this.sessions.get(threadId);
    if (!session) {
      return false;
    }
    // Map entry first so dispose stays idempotent even if teardown throws.
    this.sessions.delete(threadId);
    try {
      session.status = 'stopped';
      session.setupAbort.abort();
      session.abortController?.abort();
      // Emits per-requestId permission_dismissed — the one emission dispose
      // allows (clears stranded cards; cannot be misread by stop gates).
      this.dismissAllRequests(session);
    } catch (error) {
      console.warn('[BubbleSdkAdapter] disposeSession cleanup failed:', error);
    }
    return true;
  }

  async stopAll(): Promise<void> {
    const threadIds = Array.from(this.sessions.keys());
    await Promise.all(threadIds.map((threadId) => this.stopSession(threadId)));
  }

  listSessions(): ProviderSession[] {
    return Array.from(this.sessions.values()).map((session) => ({
      threadId: session.threadId,
      provider: 'bubble',
      providerSessionId: session.providerSessionId,
      status: session.status,
      model: session.model,
    }));
  }

  hasSession(threadId: string): boolean {
    return this.sessions.has(threadId);
  }

  async respondToRequest(
    threadId: string,
    requestId: string,
    decision: PermissionResult
  ): Promise<void> {
    const session = this.sessions.get(threadId);
    if (!session) {
      throw new Error(`No Bubble session found for thread "${threadId}"`);
    }
    const pending = session.pendingRequests.get(requestId);
    if (!pending) {
      // Stale card (already dismissed by stop/abort) — race-safe no-op.
      return;
    }
    session.pendingRequests.delete(requestId);

    if (pending.kind === 'approval') {
      pending.resolve(
        decision.behavior === 'allow'
          ? {
              action: 'approve',
              ...(decision.message ? { feedback: decision.message } : {}),
              ...(decision.updatedInput?.optionId === APPROVE_SESSION_OPTION_ID ? { remember: 'session' as const } : {}),
            }
          : { action: 'reject', feedback: decision.message?.trim() || 'Denied by user' }
      );
      return;
    }
    if (pending.kind === 'question') {
      pending.resolve(
        decision.behavior === 'allow' ? buildQuestionAnswers(pending.questions, decision) : null
      );
      return;
    }
    // Plan approval renders as a question card, so "Stay in plan mode" also
    // arrives as behavior:'allow' — only the explicit approve answer may exit
    // plan mode (mirrors the Claude runner's ExitPlanMode check).
    if (decision.behavior !== 'allow') {
      pending.resolve(false);
      return;
    }
    const answers = isRecord(decision.updatedInput?.answers)
      ? (decision.updatedInput.answers as Record<string, unknown>)
      : null;
    const selectedAnswer = answers
      ? Object.values(answers).find(
          (value): value is string => typeof value === 'string' && value.trim().length > 0
        ) || ''
      : '';
    pending.resolve(selectedAnswer === 'Approve and execute');
  }

  /**
   * One-off prompt (environment summaries etc.) on a throwaway session.
   * Bubble persists sessions lazily on the first message, so the temp session
   * is deleted afterwards — otherwise every one-shot would leave a junk
   * entry in the user's Bubble session/resume list.
   */
  async runOneShot(
    input: ProviderSessionStartInput
  ): Promise<{ text: string; sessionId?: string; model?: string }> {
    const cwd = input.cwd || process.cwd();
    const sdk = await getBubbleSdk(cwd);
    this.assertConfigured(sdk);
    let model = await this.resolveCatalogModel(sdk, input.model, true);
    const { id } = sdk.createSession({ cwd });
    let text = '';
    const abortController = new AbortController();
    const timer = setTimeout(() => abortController.abort(), ONE_SHOT_TIMEOUT_MS);
    timer.unref?.();
    try {
      const promptText = buildPromptText(input.prompt, input.attachments);
      const prompt = await buildPromptParts(promptText, input.attachments);
      // No approval/question handlers on purpose: the SDK fail-safes them to
      // reject, so a one-shot can never block on an interactive card.
      const stream = sdk.runTurn(id, {
        prompt,
        model,
        signal: abortController.signal,
        onStart: (info) => {
          assertBubbleSessionReader(prompt, info.tools, id);
          model = info.model || model;
        },
      });
      for await (const event of stream) {
        if (event.type === 'text_delta') {
          text += getString((event as { content?: unknown }).content);
        }
      }
    } finally {
      clearTimeout(timer);
      try {
        sdk.deleteSession(id);
      } catch (error) {
        console.warn('[BubbleSdkAdapter] failed to delete one-shot session:', error);
      }
    }
    return { text: text.trim(), sessionId: id, model };
  }

  async listSkills(input: ProviderListSkillsInput): Promise<ProviderListSkillsResult> {
    const sdk = await getBubbleSdk(input.cwd);
    const skills = sdk.listSkills(input.cwd, bundledSkillPaths());
    return {
      skills: skills.map((skill) => ({
        name: skill.name,
        description: skill.description || undefined,
        path: skill.name,
        enabled: true,
        scope: skill.source || undefined,
      })),
      source: 'bubble-sdk',
    };
  }

  /**
   * User turns the persisted session can rewind to (oldest first). The entry
   * id also names the checkpoint turn, so it doubles as the `/rewind` anchor
   * passed back into `rewind()`.
   */
  async listRewindAnchors(threadId: string): Promise<ProviderRewindAnchor[]> {
    const session = this.sessions.get(threadId);
    if (!session) {
      return [];
    }
    try {
      const sdk = await getBubbleSdk(session.cwd);
      const manager = await getBubbleSessionManager(sdk, session.providerSessionId);
      return manager.listUserTurns().map((turn) => ({
        id: turn.id,
        preview: turn.preview,
        text: turn.text,
      }));
    } catch (error) {
      console.warn('[BubbleSdkAdapter] failed to list rewind anchors:', error);
      return [];
    }
  }

  /**
   * Restore files and/or truncate the persisted session to just before the
   * given anchor (a user-turn entry id). `dryRun` reports what a files rewind
   * would change without executing. The Aegis-side transcript truncation is
   * handled by the IPC layer; this only mutates the SDK's on-disk session.
   */
  async rewind(
    threadId: string,
    anchorId: string,
    scope: 'conversation' | 'files' | 'both',
    dryRun?: boolean
  ): Promise<ProviderRewindResult> {
    const session = this.sessions.get(threadId);
    if (!session) {
      return { ok: false, message: 'No Bubble session found for thread.', filesAvailable: false };
    }

    try {
      const sdk = await getBubbleSdk(session.cwd);
      const manager = await getBubbleSessionManager(sdk, session.providerSessionId);
      const checkpoints = manager.getCheckpoints();

      if (dryRun) {
        const filesChanged = checkpoints.filesTouchedSince?.(anchorId) ?? [];
        return {
          ok: true,
          filesAvailable: true,
          files: { canRewind: true, filesChanged },
          removedPrompt: null,
        };
      }

      let filesOutcome: ProviderRewindResult['files'] = null;
      if (scope === 'files' || scope === 'both') {
        const restored = await checkpoints.restoreTo(anchorId);
        filesOutcome = {
          canRewind: true,
          filesChanged: [...restored.restored, ...restored.deleted],
          ...(restored.failed.length > 0
            ? { error: `Failed to restore: ${restored.failed.join(', ')}` }
            : {}),
        };
      }

      let removedPrompt: string | null = null;
      if (scope === 'conversation' || scope === 'both') {
        const result = manager.rewindToEntry(anchorId);
        if (!result) {
          return {
            ok: false,
            message: 'The rewind anchor was not found in this Bubble session.',
            filesAvailable: true,
            files: filesOutcome,
          };
        }
        removedPrompt = result.targetText || null;
      }

      return { ok: true, filesAvailable: true, files: filesOutcome, removedPrompt };
    } catch (error) {
      return {
        ok: false,
        message: error instanceof Error ? error.message : String(error),
        filesAvailable: false,
      };
    }
  }

  /**
   * Run manual `/compact` locally: compact the persisted session and emit the
   * same transcript markers Claude Code does (a compact_boundary, then a
   * result that finalizes the turn).
   */
  private async runCompact(session: ActiveBubbleSession, preTokens: number): Promise<void> {
    session.status = 'running';
    session.turnActive = true;
    session.durationStartMs = Date.now();
    session.durationEndMs = undefined;
    session.usage = createEmptyUsage();
    session.totalCostUsd = 0;
    session.currentAssistant = null;
    this.emit({ type: 'status_change', threadId: session.threadId, status: 'running' });

    try {
      const sdk = await getBubbleSdk(session.cwd);
      const manager = await getBubbleSessionManager(sdk, session.providerSessionId);
      const plan = manager.getCompactionPlan();
      const compacted = plan ? manager.compact().compacted : false;

      if (compacted) {
        this.emitMessage(session, {
          type: 'system',
          subtype: 'compact_boundary',
          uuid: `bubble-compact:${session.threadId}:${uuidv4()}`,
          session_id: session.threadId,
          compactMetadata: { trigger: 'manual', preTokens },
        });
        session.lastContextTokens = undefined;
        this.emitMessage(session, { type: 'system', subtype: 'bubble_context', uuid: uuidv4(), model: session.model || '', context: null });
      } else {
        this.emitAssistantText(session, 'Session is already compact enough.');
      }

      this.finishTurn(session, null);
    } catch (error) {
      this.finishTurn(session, error instanceof Error ? error : new Error(String(error)));
    } finally {
      session.turnActive = false;
    }
  }

  private emitAssistantText(session: ActiveBubbleSession, text: string): void {
    this.emitMessage(session, {
      type: 'assistant',
      uuid: `bubble-assistant:${session.threadId}:${uuidv4()}`,
      createdAt: Date.now(),
      message: { content: [{ type: 'text', text }] },
    });
  }

  // ── Turn loop ──────────────────────────────────────────────────────────

  private async runTurnLoop(
    session: ActiveBubbleSession,
    prompt: string | BubbleContentPart[],
    model: string | undefined,
    sdk: BubbleSdkInstance
  ): Promise<void> {
    const abortController = new AbortController();
    session.abortController = abortController;
    let closeEvents: (() => void) | undefined;
    try {
      if (abortController.signal.aborted) return;
      const handle = sdk.openSession(session.providerSessionId);
      const stream = handle.eventsFrom(handle.latestSequence);
      closeEvents = () => handle.close();
      abortController.signal.addEventListener('abort', () => {
        sdk.stop(session.providerSessionId, { cancelQueued: true });
        handle.close();
      }, { once: true });
      // runTurn eagerly starts execution. The session subscription also sees
      // SDK-created fallback turns, which the original turn iterator cannot.
      sdk.runTurn(session.providerSessionId, {
        prompt,
        preserveInterruptedInput: true,
        hostTools: [...createDesignTools(session.threadId, abortController.signal), ...createWidgetHostTools()],
        skillPaths: bundledSkillPaths(),
        ...(model ? { model } : {}),
        ...(session.permissionMode ? { mode: session.permissionMode } : {}),
        ...(session.planExitMode ? { planExitMode: session.planExitMode } : {}),
        ...(session.thinkingLevel ? { thinkingLevel: session.thinkingLevel } : {}),
        signal: abortController.signal,
        onStart: (info) => {
          assertBubbleSessionReader(prompt, info.tools, session.threadId);
          session.model = info.model || session.model;
          // Bubble's TokenUsage has no context window; resolve it from the
          // registry catalog so the composer context indicator has a ceiling.
          void this.resolveContextWindow(session, info.providerId, info.model);
          this.emit({
            type: 'system_init',
            threadId: session.threadId,
            sessionId: session.providerSessionId,
            model: session.model,
          });
        },
        onApproval: (request) => this.requestApproval(session, request),
        onQuestion: (request) => this.requestQuestion(session, request),
        onPlanApproval: (planMarkdown) => this.requestPlanApproval(session, planMarkdown),
        onProjectTrust: (request) => this.requestProjectTrust(session, request.pending),
      });
      this.emit({ type: 'status_change', threadId: session.threadId, status: 'running' });
      for await (const record of stream) {
        if (session.status === 'stopped' || this.sessions.get(session.threadId) !== session) break;
        this.handleBubbleEvent(session, record.event);
        if (record.terminal?.kind === 'failed' || record.terminal?.kind === 'cancelled') {
          throw new Error(record.terminal.message);
        }
        if (record.terminal?.kind === 'completed' && record.sequence === handle.latestSequence
          && !sdk.getSessionRunState(session.providerSessionId).active) break;
      }
      session.turnActive = false;
      session.abortController = null;
      this.dismissAllRequests(session);
      this.finishTurn(session, null);
    } catch (error) {
      abortController.abort();
      session.turnActive = false;
      session.abortController = null;
      this.dismissAllRequests(session);
      this.finishTurn(session, error instanceof Error ? error : new Error(String(error)));
    } finally {
      closeEvents?.();
      if (session.abortController === abortController) {
        session.abortController = null;
        session.turnActive = false;
        this.dismissAllRequests(session);
      }
      // Complete, error and stop all end here; finishTurn returns early on stop.
      try {
        settleDesignTurn(session.threadId);
      } catch (error) {
        console.warn('[design] Failed to settle comments after turn', error);
      }
    }
  }

  private handleBubbleEvent(session: ActiveBubbleSession, event: BubbleAgentEvent): void {
    switch (event.type) {
      case 'provider_retry': {
        const retry = event as { attempt: number; maxAttempts: number };
        // Display-only commit: the SDK excludes partial output from retries.
        this.flushAssistant(session, 'commentary');
        this.interruptStreamingTools(session);
        const uuid = `bubble-retry:${session.threadId}:${uuidv4()}`;
        session.pendingRetryId = uuid;
        this.emitMessage(session, { type: 'system', subtype: 'api_retry', uuid,
          session_id: session.threadId, attempt: retry.attempt,
          maxRetries: retry.maxAttempts, errorStatus: null });
        return;
      }
      case 'text_delta':
      case 'reasoning_delta': {
        const delta = getString((event as { content?: unknown }).content);
        if (!delta) {
          return;
        }
        this.resolveRetry(session);
        const accumulator = this.ensureCurrentAssistant(session);
        if (event.type === 'reasoning_delta') {
          accumulator.thinking += delta;
        } else {
          accumulator.text += delta;
        }
        this.emitMessage(session, {
          type: 'stream_event',
          event: {
            type: 'content_block_delta',
            index: 0,
            delta:
              event.type === 'reasoning_delta'
                ? { type: 'thinking_delta', thinking: delta }
                : { type: 'text_delta', text: delta },
          },
        });
        return;
      }
      case 'tool_call_start':
      case 'tool_call_delta': {
        this.resolveRetry(session);
        const toolEvent = event as { id: string; name: string; arguments?: string };
        if (isShowWidgetToolName(toolEvent.name)) this.handleStreamingWidget(session, toolEvent.id, toolEvent.name, toolEvent.arguments);
        else this.handleStreamingToolUse(session, toolEvent.id, toolEvent.name, toolEvent.arguments);
        return;
      }
      case 'tool_call_end': {
        this.resolveRetry(session);
        const toolEvent = event as Extract<BubbleAgentEvent, { type: 'tool_call_end' }>;
        this.handleToolUse(
          session,
          toolEvent.id,
          toolEvent.name,
          parseToolArguments(toolEvent.arguments)
        );
        return;
      }
      case 'tool_start': {
        this.resolveRetry(session);
        const toolEvent = event as Extract<BubbleAgentEvent, { type: 'tool_start' }>;
        this.handleToolUse(session, toolEvent.id, toolEvent.name, toolEvent.args);
        return;
      }
      case 'tool_end': {
        const toolEvent = event as Extract<BubbleAgentEvent, { type: 'tool_end' }>;
        this.handleToolResult(session, toolEvent.id, toolEvent.result);
        return;
      }
      case 'context_usage': {
        const snapshot = event as Extract<BubbleAgentEvent, { type: 'context_usage' }>;
        this.emitContextSnapshot(session, snapshot.usedTokens, snapshot.contextWindow, snapshot.estimated);
        return;
      }
      case 'context_compaction': {
        const compact = event as Extract<BubbleAgentEvent, { type: 'context_compaction' }>;
        this.flushAssistant(session, 'commentary');
        if (compact.status === 'completed') {
          this.emitMessage(session, { type: 'system', subtype: 'compact_boundary',
            uuid: compact.compactionId ? `bubble-compact:${compact.compactionId}` : uuidv4(), session_id: session.threadId,
            compactMetadata: { trigger: 'auto', preTokens: compact.preTokens, postTokens: compact.postTokens,
              compactionId: compact.compactionId, persisted: compact.persisted } });
          if (typeof compact.postTokens === 'number') this.emitContextSnapshot(session, compact.postTokens, compact.contextWindow, true);
        } else {
          this.emitMessage(session, { type: 'system', subtype: 'compact_status', uuid: uuidv4(), session_id: session.threadId,
            status: compact.status, trigger: 'auto' });
        }
        return;
      }
      case 'turn_end': {
        const turnEvent = event as Extract<BubbleAgentEvent, { type: 'turn_end' }>;
        this.flushAssistant(session, turnEvent.willContinue === false ? 'final_answer'
          : turnEvent.willContinue === true ? 'commentary' : undefined);
        addUsage(session.usage, usageFromBubble(turnEvent.usage, session.contextWindow));
        // Per-step priced usage; hosts sum them. The unified result field is
        // USD-labelled, so non-USD costs are dropped rather than mislabelled.
        const cost = turnEvent.cost;
        if (cost && cost.currency === 'USD' && getNumber(cost.cost) !== undefined) {
          session.totalCostUsd += cost.cost;
        }
        return;
      }
      case 'mode_changed': {
        // Plan approval makes the SDK setMode('default') mid-turn; report it
        // so the composer's plan pill exits like Claude's does.
        const mode = getString((event as { mode?: unknown }).mode);
        if (mode === 'default' || mode === 'plan' || mode === 'bypassPermissions') {
          session.permissionMode = mode;
          this.emit({
            type: 'permission_mode_changed',
            threadId: session.threadId,
            provider: 'bubble',
            mode,
          });
        }
        return;
      }
      case 'subagent_update': {
        // Flattened payload: the fields live at the event top level
        // (drainToolUpdates yields buildSubagentUpdate's return as-is).
        this.handleSubagentUpdate(session, event as unknown as BubbleSubagentUpdate);
        return;
      }
      case 'tool_update': {
        // Spawn/wait tool execution drains child progress as tool_update
        // frames — update carries the SAME flattened subagent payload.
        const toolEvent = event as Extract<BubbleAgentEvent, { type: 'tool_update' }>;
        const update = toolEvent.update as unknown as BubbleSubagentUpdate | undefined;
        if (update && typeof update.parentToolCallId === 'string') {
          this.handleSubagentUpdate(session, update);
        }
        return;
      }
      // Other event kinds (hooks / future additions) have no UI
      // mapping yet and fall through untouched.
      default:
        return;
    }
  }

  /**
   * Nest one SDK subagent lifecycle frame under its spawning tool_use id so
   * the existing subagent board / SubagentPanel (Claude/Codex/Kimi parity)
   * picks it up without provider-specific UI. The child's own events arrive
   * re-keyed (their ids are unique per session), so we can emit them as
   * nested messages with `parentToolUseId` — same wire shape Kimi uses.
   */
  private handleSubagentUpdate(session: ActiveBubbleSession, update: BubbleSubagentUpdate): void {
    const parentToolCallId = getString(update.parentToolCallId);
    if (!parentToolCallId) return;
    if (!session.subagentStartedAt.has(parentToolCallId)) {
      session.subagentStartedAt.set(parentToolCallId, Date.now());
    }

    const states = session.subagentStates ??= new Map();
    const prior = states.get(update.subAgentId);
    const snapshot = Array.isArray(update.metadata?.subagents) ? update.metadata.subagents[0] : undefined;
    const info = isRecord(snapshot) ? snapshot : {};
    const event = update.childEvent as { type?: string; name?: string; args?: Record<string, unknown>; attempt?: number; maxAttempts?: number } | undefined;
    let activity = prior?.activity || 'Starting';
    if (event?.type === 'tool_start' || event?.type === 'tool_call_end' || event?.type === 'tool_call_start' || event?.type === 'tool_call_delta') {
      const target = getString(event.args?.path) || getString(event.args?.file_path);
      const file = target.split('/').pop();
      activity = `${normalizeToolName(event.name || 'Tool')}${file ? ` · ${file}` : ''}`;
    } else if (event?.type === 'tool_end') activity = 'Analyzing results';
    else if (event?.type === 'reasoning_delta') activity = 'Analyzing';
    else if (event?.type === 'text_delta') activity = 'Writing response';
    else if (event?.type === 'provider_retry') activity = `Reconnecting ${event.attempt}/${event.maxAttempts}`;
    const state: BubbleSubagentState = {
      agentId: update.subAgentId, anchorId: parentToolCallId,
      nickname: update.nickname || prior?.nickname || update.agentName,
      role: update.agentName, task: getString(info.task) || prior?.task || '',
      status: update.status, activity,
      startedAt: getNumber(info.createdAt) ?? prior?.startedAt ?? Date.now(),
      updatedAt: getNumber(info.updatedAt) ?? Date.now(),
      pendingInputCount: getNumber(info.pendingInputCount) ?? 0,
      inputDelivery: info.inputDelivery as BubbleSubagentState['inputDelivery'],
    };
    // Persist at most one activity snapshot per second during token streaming.
    // A status/delivery change is immediate and the UUID upserts one record.
    const workflowChild = update.metadata?.mode === 'workflow';
    if (!workflowChild && (!prior || state.status !== prior.status || state.inputDelivery !== prior.inputDelivery
      || state.pendingInputCount !== prior.pendingInputCount || state.activity !== prior.activity
      || state.updatedAt - prior.updatedAt >= 1000)) {
      states.set(state.agentId, state);
      this.emitMessage(session, {
        type: 'assistant', uuid: `bubble-sub-state:${session.threadId}:${state.agentId}`,
        parentToolUseId: parentToolCallId, createdAt: state.startedAt,
        bubbleSubagent: state, message: { content: [] },
      });
    }

    // The spawning tool_use card may not have landed yet (queued frames can
    // race the main-loop tool_call_end) — the standard handleToolUse emits it.
    const child = update.childEvent;
    if (child?.type === 'provider_retry') {
      this.flushSubagentStream(session, parentToolCallId);
      this.interruptStreamingTools(session, parentToolCallId);
    }
    if (child && (child.type === 'text_delta' || child.type === 'reasoning_delta')) {
      const delta = getString((child as { content?: unknown }).content);
      if (delta) {
        let stream = session.subagentStreams.get(parentToolCallId);
        if (!stream) {
          stream = { text: '', thinking: '', toolCallIds: new Set() };
          session.subagentStreams.set(parentToolCallId, stream);
        }
        if (child.type === 'reasoning_delta') stream.thinking += delta;
        else stream.text += delta;
      }
    } else if (child && (child.type === 'tool_call_start' || child.type === 'tool_call_delta')) {
      const tool = child as { id: string; name: string; arguments?: string };
      this.handleStreamingToolUse(session, tool.id, tool.name, tool.arguments, parentToolCallId);
    } else if (child && (child.type === 'tool_start' || child.type === 'tool_call_end')) {
      // Flush narration buffered before the child's first tool card.
      this.flushSubagentStream(session, parentToolCallId);
      const childToolId = getString((child as { id?: unknown }).id);
      const childToolName = getString((child as { name?: unknown }).name);
      if (childToolId && childToolName) {
        // flush deleted the stream above — re-create it so subsequent frames
        // (and the matching tool_end) still have a lane. Without this the
        // tool_use card never emitted and tool_end produced an orphan result.
        let stream = session.subagentStreams.get(parentToolCallId);
        if (!stream) {
          stream = { text: '', thinking: '', toolCallIds: new Set() };
          session.subagentStreams.set(parentToolCallId, stream);
        }
        if (!session.emittedToolCallIds.has(childToolId)) {
          stream.toolCallIds.add(childToolId);
          session.emittedToolCallIds.add(childToolId);
          const preview = session.streamingToolCalls?.get(childToolId);
          session.streamingToolCalls?.delete(childToolId);
          this.emitMessage(session, {
            type: 'assistant',
            uuid: `bubble-sub-tool-use:${session.threadId}:${childToolId}`,
            parentToolUseId: parentToolCallId,
            ...(preview ? { createdAt: preview.createdAt } : {}),
            message: {
              content: [
                {
                  type: 'tool_use',
                  id: childToolId,
                  name: normalizeToolName(childToolName),
                  input: isRecord((child as { args?: unknown }).args)
                    ? ((child as { args?: unknown }).args as Record<string, unknown>)
                    : parseToolArguments(getString((child as { arguments?: unknown }).arguments)),
                },
              ],
            },
          });
        }
      }
    } else if (child && child.type === 'tool_end') {
      const childToolId = getString((child as { id?: unknown }).id);
      const childResult = (child as { result?: { content?: unknown; isError?: boolean } }).result;
      if (childToolId) {
        this.emitMessage(session, {
          type: 'user',
          uuid: `bubble-sub-tool-result:${session.threadId}:${childToolId}:${uuidv4()}`,
          parentToolUseId: parentToolCallId,
          message: {
            content: [
              {
                type: 'tool_result',
                tool_use_id: childToolId,
                content: getString(childResult?.content),
                is_error: childResult?.isError === true,
              },
            ],
          },
        });
      }
    }

    // Terminal statuses commit the lane's buffered narration (the child's
    // final summary arrives as `message`/summaryDelta, not as text_delta) and
    // release the held spawn tool_result so the UI lane flips to done NOW,
    // not at spawn time.
    if (
      update.status === 'completed' ||
      update.status === 'failed' ||
      update.status === 'cancelled' ||
      update.status === 'blocked'
    ) {
      this.flushSubagentStream(session, parentToolCallId);
      const held = session.heldSpawnResults.get(parentToolCallId);
      if (held && !workflowChild) {
        session.heldSpawnResults.delete(parentToolCallId);
        this.emitToolResult(session, parentToolCallId, {
          content: update.message || `Subagent ${state.nickname}: ${state.status}`,
          isError: state.status === 'failed' || state.status === 'blocked',
        });
      }
    }
  }

  /** Commit a subagent lane's accumulated thinking/text as nested messages. */
  private flushSubagentStream(session: ActiveBubbleSession, parentToolCallId: string): void {
    const stream = session.subagentStreams.get(parentToolCallId);
    if (!stream) return;
    session.subagentStreams.delete(parentToolCallId);
    if (stream.thinking) {
      this.emitMessage(session, {
        type: 'assistant',
        uuid: `bubble-sub-thinking:${session.threadId}:${parentToolCallId}:${uuidv4()}`,
        parentToolUseId: parentToolCallId,
        message: { content: [{ type: 'thinking', thinking: stream.thinking }] },
      });
    }
    if (stream.text) {
      this.emitMessage(session, {
        type: 'assistant',
        uuid: `bubble-sub-assistant:${session.threadId}:${parentToolCallId}:${uuidv4()}`,
        parentToolUseId: parentToolCallId,
        message: { content: [{ type: 'text', text: stream.text }] },
      });
    }
  }

  private handleStreamingToolUse(session: ActiveBubbleSession, id: string, name: string, argumentsText?: string, parentToolUseId?: string): void {
    if (!id || session.emittedToolCallIds.has(id) || session.emittedToolResultIds.has(id)) return;
    const calls = session.streamingToolCalls ??= new Map();
    const previous = calls.get(id);
    const input = argumentsText === undefined ? previous?.input ?? {} : toolInputPreview(argumentsText);
    if (previous && JSON.stringify(previous.input) === JSON.stringify(input)) return;
    // End the preceding narration before the new activity starts. Keep one
    // identity and timestamp through preview, execution, history and completion.
    if (!previous) {
      if (parentToolUseId) this.flushSubagentStream(session, parentToolUseId);
      else this.flushAssistant(session, 'commentary');
    }
    const createdAt = previous?.createdAt ?? Date.now();
    calls.set(id, { createdAt, input, name, parentToolUseId });
    this.emitMessage(session, {
      type: 'assistant', uuid: `bubble-${parentToolUseId ? 'sub-' : ''}tool-use:${session.threadId}:${id}`, createdAt,
      ...(parentToolUseId ? { parentToolUseId } : {}),
      message: { content: [{ type: 'tool_use', id, name: normalizeToolName(name),
        input: { ...input, __aegisToolCallStreaming: true } }] },
    });
  }

  /**
   * A show_widget call draws in the chat while it is written: its partial
   * `html` goes out as a tool_use preview, paced to one redraw per interval
   * (with a trailing one, so a pause still shows the latest markup). Previews
   * are broadcast but not persisted; the finished call replaces them under
   * the same id, and only the title survives an interruption.
   */
  private handleStreamingWidget(session: ActiveBubbleSession, id: string, name: string, argumentsText?: string): void {
    if (!id || session.emittedToolCallIds.has(id) || session.emittedToolResultIds.has(id)) return;
    const calls = session.streamingToolCalls ??= new Map();
    if (!calls.has(id)) {
      this.flushAssistant(session, 'commentary');
      calls.set(id, { createdAt: Date.now(), input: {}, name });
    }
    const streams = session.widgetStreams ??= new Map();
    let stream = streams.get(id);
    if (!stream) streams.set(id, stream = { args: '', lastEmitAt: 0 });
    if (argumentsText !== undefined) stream.args = argumentsText;
    const wait = WIDGET_PREVIEW_INTERVAL_MS - (Date.now() - stream.lastEmitAt);
    if (wait <= 0) this.emitWidgetPreview(session, id);
    else stream.timer ??= setTimeout(() => this.emitWidgetPreview(session, id), wait);
  }

  private emitWidgetPreview(session: ActiveBubbleSession, id: string): void {
    const stream = session.widgetStreams?.get(id);
    const call = session.streamingToolCalls?.get(id);
    if (!stream || !call) return;
    if (stream.timer) clearTimeout(stream.timer);
    stream.timer = undefined;
    stream.lastEmitAt = Date.now();
    const { title, html } = partialJsonStringFields(stream.args, ['title', 'html']);
    const key = `${title ?? ''}\u0000${html ?? ''}`;
    if (stream.sent === key) return;
    stream.sent = key;
    call.input = title ? { title } : {};
    this.emitMessage(session, {
      type: 'assistant', uuid: `bubble-tool-use:${session.threadId}:${id}`, createdAt: call.createdAt,
      message: { content: [{ type: 'tool_use', id, name: normalizeToolName(call.name),
        input: { ...call.input, ...(html ? { html } : {}), __aegisToolCallStreaming: true, [WIDGET_STREAMING_FLAG]: true } }] },
    });
  }

  private dropWidgetStream(session: ActiveBubbleSession, id?: string): void {
    for (const [streamId, stream] of session.widgetStreams ?? []) {
      if (id !== undefined && streamId !== id) continue;
      if (stream.timer) clearTimeout(stream.timer);
      session.widgetStreams?.delete(streamId);
    }
  }

  private interruptStreamingTools(session: ActiveBubbleSession, parentToolUseId?: string): void {
    for (const [id, call] of session.streamingToolCalls ?? []) {
      if (call.parentToolUseId !== parentToolUseId) continue;
      session.streamingToolCalls?.delete(id);
      // A widget keeps its title (from the latest arguments, which may be
      // ahead of the paced preview) and none of its half-written HTML.
      const widgetArgs = session.widgetStreams?.get(id)?.args;
      const { title } = widgetArgs === undefined ? {} : partialJsonStringFields(widgetArgs, ['title']);
      const input = widgetArgs === undefined ? call.input : title ? { title } : {};
      this.dropWidgetStream(session, id);
      this.emitMessage(session, {
        type: 'assistant', uuid: `bubble-${parentToolUseId ? 'sub-' : ''}tool-use:${session.threadId}:${id}`,
        createdAt: call.createdAt, ...(parentToolUseId ? { parentToolUseId } : {}),
        message: { content: [{ type: 'tool_use', id, name: normalizeToolName(call.name),
          input: { ...input, __aegisToolCallInterrupted: true } }] },
      });
    }
  }

  private handleToolUse(
    session: ActiveBubbleSession,
    toolCallId: string,
    toolName: string,
    args: unknown
  ): void {
    if (!toolCallId || session.emittedToolCallIds.has(toolCallId)) {
      return;
    }
    // Ordering: the transcript expects the assistant text that preceded the
    // tool call to land before the tool_use card.
    this.flushAssistant(session);
    const preview = session.streamingToolCalls?.get(toolCallId);
    session.streamingToolCalls?.delete(toolCallId);
    this.dropWidgetStream(session, toolCallId);
    session.emittedToolCallIds.add(toolCallId);
    session.toolNames.set(toolCallId, toolName);
    if (toolName === 'wait_agent' && isRecord(args) && !getString(args.agent_id)
      && (!Array.isArray(args.agent_ids) || args.agent_ids.length === 0)) {
      // Capture the targets at invocation, not when a later render sees them
      // completed. This is display metadata, not a change to SDK arguments.
      args = { ...args, agent_ids: [...(session.subagentStates?.values() ?? [])]
        .filter(state => state.status !== 'closed').map(state => state.agentId) };
    }
    this.emitMessage(session, {
      type: 'assistant',
      uuid: `bubble-tool-use:${session.threadId}:${toolCallId}`,
      ...(preview ? { createdAt: preview.createdAt } : {}),
      message: {
        content: [
          {
            type: 'tool_use',
            id: toolCallId,
            name: normalizeToolName(toolName),
            input: isRecord(args) ? args : { value: args },
          },
        ],
      },
    });
  }

  private handleToolResult(
    session: ActiveBubbleSession,
    toolCallId: string,
    result: { content?: string; isError?: boolean }
  ): void {
    if (!toolCallId || session.emittedToolResultIds.has(toolCallId)) {
      return;
    }
    session.emittedToolResultIds.add(toolCallId);
    // Fire-and-forget spawn: hold the result until the lane's terminal
    // subagent_update so the UI lane stays pending/running while the child
    // works (Claude's Task resolves only when the child finishes; Bubble's
    // spawn_agent returns "Spawned X (queued)" immediately).
    const toolName = (session.toolNames.get(toolCallId) || '').toLowerCase();
    // A workflow call returns its aggregate outcome after every phase. A single
    // member's terminal status must never complete or rename the whole workflow.
    if (toolName === 'spawn_agent') {
      const state = [...(session.subagentStates?.values() ?? [])].find((value) => value.anchorId === toolCallId);
      if (result.isError || (state && !['running', 'queued'].includes(state.status))) {
        this.emitToolResult(session, toolCallId, state ? {
          content: `Subagent ${state.nickname}: ${state.status}`,
          isError: state.status === 'failed' || state.status === 'blocked',
        } : result);
      } else session.heldSpawnResults.set(toolCallId, result);
      return;
    }
    this.emitToolResult(session, toolCallId, result);
  }

  /** Emit a (possibly held) tool_result — shared by the direct path and the
   * subagent terminal release. */
  private emitToolResult(
    session: ActiveBubbleSession,
    toolCallId: string,
    result: { content?: string; isError?: boolean }
  ): void {
    this.emitMessage(session, {
      type: 'user',
      uuid: `bubble-tool-result:${session.threadId}:${toolCallId}:${uuidv4()}`,
      message: {
        content: [
          {
            type: 'tool_result',
            tool_use_id: toolCallId,
            content: getString(result?.content),
            is_error: result?.isError === true,
          },
        ],
      },
    });
  }

  private flushAssistant(session: ActiveBubbleSession, phase?: 'commentary' | 'final_answer'): void {
    const accumulator = session.currentAssistant;
    if (!accumulator) {
      return;
    }
    session.currentAssistant = null;
    const blocks: ContentBlock[] = [];
    if (accumulator.thinking) {
      blocks.push({ type: 'thinking', thinking: accumulator.thinking });
    }
    if (accumulator.text) {
      blocks.push({ type: 'text', text: accumulator.text });
    }
    if (blocks.length === 0) {
      return;
    }
    this.emitMessage(session, {
      type: 'assistant',
      uuid: accumulator.uuid,
      phase,
      createdAt: accumulator.createdAt,
      message: { content: blocks },
    });
  }

  /** Every call still being written, top-level and in subagents. */
  private interruptAllStreamingTools(session: ActiveBubbleSession): void {
    const parents = new Set([...session.streamingToolCalls?.values() ?? []].map((call) => call.parentToolUseId));
    for (const parentToolUseId of parents) this.interruptStreamingTools(session, parentToolUseId);
  }

  private finishTurn(session: ActiveBubbleSession, error: Error | null): void {
    this.resolveRetry(session);
    // A turn that ends (stop, error, cut-off output) mid tool call closes the
    // call as interrupted under its id, replacing the preview the chat drew:
    // what the chat shows then is what history replays (a widget keeps only
    // its title, never half-written HTML that isn't saved).
    this.interruptAllStreamingTools(session);
    session.streamingToolCalls?.clear();
    this.dropWidgetStream(session);
    this.flushAssistant(session);
    // Safety net: commit any subagent lane that never saw its terminal frame
    // (interrupt, transport error) so its buffered narration isn't lost — and
    // release any spawn results still held (their child never reported a
    // terminal status; leaving them held would strand the UI lane pending).
    for (const parentToolCallId of [...session.subagentStreams.keys()]) {
      this.flushSubagentStream(session, parentToolCallId);
    }
    for (const [toolCallId, held] of [...session.heldSpawnResults.entries()]) {
      session.heldSpawnResults.delete(toolCallId);
      this.emitToolResult(session, toolCallId, held);
    }
    if (session.status === 'stopped') {
      return;
    }
    session.durationEndMs = Date.now();
    if (error) {
      session.status = 'error';
      this.emitResult(session, 'error');
      this.emit({ type: 'status_change', threadId: session.threadId, status: 'error' });
      this.emit({ type: 'error', threadId: session.threadId, error });
      return;
    }
    session.status = 'completed';
    this.emitResult(session);
    this.emit({ type: 'status_change', threadId: session.threadId, status: 'completed' });
  }

  private resolveRetry(session: ActiveBubbleSession): void {
    const retryId = session.pendingRetryId;
    if (!retryId) return;
    session.pendingRetryId = undefined;
    this.emitMessage(session, { type: 'system', subtype: 'api_retry_resolved',
      uuid: `${retryId}:resolved`, session_id: session.threadId, retryId });
  }

  private emitResult(session: ActiveBubbleSession, subtype = 'success'): void {
    this.emitMessage(session, {
      type: 'result',
      subtype,
      duration_ms: Math.max(0, (session.durationEndMs || Date.now()) - session.durationStartMs),
      total_cost_usd: session.totalCostUsd,
      usage: session.usage,
      model: session.model,
    });
  }

  private emitContextSnapshot(session: ActiveBubbleSession, usedTokens: number, contextWindow: number | undefined, estimated: boolean): void {
    const window = contextWindow || session.contextWindow;
    if (!Number.isFinite(usedTokens) || usedTokens < 0 || !window || !Number.isFinite(window) || window <= 0) return;
    session.lastContextTokens = usedTokens;
    this.emitMessage(session, { type: 'system', subtype: 'bubble_context', uuid: uuidv4(), model: session.model || '',
      context: { usedTokens, contextWindow: window, estimated } });
  }

  private ensureCurrentAssistant(session: ActiveBubbleSession): BubbleAssistantAccumulator {
    if (session.currentAssistant) {
      return session.currentAssistant;
    }
    session.currentAssistant = {
      uuid: `bubble-assistant:${session.threadId}:${uuidv4()}`,
      text: '',
      thinking: '',
      createdAt: Date.now(),
    };
    return session.currentAssistant;
  }

  // ── Approvals / questions / plan mode ──────────────────────────────────

  private requestApproval(
    session: ActiveBubbleSession,
    request: BubbleApprovalRequest
  ): Promise<BubbleApprovalDecision> {
    if (session.status === 'stopped' || this.sessions.get(session.threadId) !== session) {
      return Promise.resolve({ action: 'reject', feedback: 'Session is no longer active.' });
    }
    if (isProjectFileApproval(session.threadId, session.cwd, session.permissionMode, request)) {
      return Promise.resolve({ action: 'approve' });
    }
    const requestId = uuidv4();
    const input = buildApprovalInput(request);
    return new Promise<BubbleApprovalDecision>((resolve) => {
      session.pendingRequests.set(requestId, { kind: 'approval', resolve });
      this.emit({
        type: 'permission_request',
        threadId: session.threadId,
        requestId,
        toolName: input.toolName,
        input,
      });
    });
  }

  private requestQuestion(
    session: ActiveBubbleSession,
    request: BubbleQuestionRequest
  ): Promise<string[][] | null> {
    if (session.status === 'stopped' || this.sessions.get(session.threadId) !== session) {
      return Promise.resolve(null);
    }
    const requestId = request.id || uuidv4();
    const input = buildQuestionInput(request);
    if (input.questions.length === 0) {
      return Promise.resolve(null);
    }
    return new Promise<string[][] | null>((resolve) => {
      session.pendingRequests.set(requestId, {
        kind: 'question',
        questions: request.questions,
        resolve,
      });
      this.emit({
        type: 'permission_request',
        threadId: session.threadId,
        requestId,
        toolName: 'AskUserQuestion',
        input,
      });
    });
  }

  /**
   * Folder trust, the Kimi Code way: before the first turn loads this folder's
   * .bubble settings (allow rules, MCP servers, LSP servers), ask once.
   */
  private requestProjectTrust(session: ActiveBubbleSession, pending: BubbleRepoCapabilities): Promise<boolean> {
    if (session.status === 'stopped' || this.sessions.get(session.threadId) !== session) {
      return Promise.resolve(false);
    }
    const requestId = uuidv4();
    const question = "Trust this folder? Its .bubble settings would enable:\n"
      + describeRepoCapabilities(pending).map((line) => `- ${line}`).join('\n');
    const input: AcpPermissionInput = {
      kind: 'acp-permission',
      provider: 'bubble',
      question,
      title: 'Trust this folder?',
      toolName: 'Folder trust',
      options: [
        { optionId: 'approve', name: 'Trust this folder', kind: 'allow_once' },
        { optionId: 'reject', name: "Don't trust", kind: 'reject_once' },
      ],
      toolCall: { type: 'project_trust', ...pending },
    };
    return new Promise<boolean>((resolve) => {
      session.pendingRequests.set(requestId, {
        kind: 'approval',
        resolve: (decision) => resolve(decision.action === 'approve'),
      });
      this.emit({
        type: 'permission_request',
        threadId: session.threadId,
        requestId,
        toolName: input.toolName,
        input,
      });
    });
  }

  private requestPlanApproval(session: ActiveBubbleSession, planMarkdown: string): Promise<boolean> {
    if (session.status === 'stopped' || this.sessions.get(session.threadId) !== session) {
      return Promise.resolve(false);
    }
    const requestId = uuidv4();
    return new Promise<boolean>((resolve) => {
      session.pendingRequests.set(requestId, { kind: 'plan', resolve });
      this.emit({
        type: 'permission_request',
        threadId: session.threadId,
        requestId,
        toolName: 'ExitPlanMode',
        input: {
          plan: planMarkdown,
          questions: [
            {
              header: 'Plan approval',
              question: 'Approve this plan and let Bubble start implementing it?',
              options: [
                {
                  label: 'Approve and execute',
                  description: 'Exit plan mode and continue with implementation.',
                },
                {
                  label: 'Stay in plan mode',
                  description: 'Keep planning without executing tools yet.',
                },
              ],
            },
          ],
        },
      });
    });
  }

  private dismissAllRequests(session: ActiveBubbleSession): void {
    for (const [requestId, pending] of session.pendingRequests) {
      this.emit({ type: 'permission_dismissed', threadId: session.threadId, requestId });
      if (pending.kind === 'approval') {
        pending.resolve({ action: 'reject', feedback: 'Session stopped.' });
      } else if (pending.kind === 'question') {
        pending.resolve(null);
      } else {
        pending.resolve(false);
      }
    }
    session.pendingRequests.clear();
  }

  // ── Context window resolution ──────────────────────────────────────────

  /** modelString → contextWindow (null = looked up, catalog has none). */
  private static contextWindowCache = new Map<string, number | null>();

  private async resolveContextWindow(
    session: ActiveBubbleSession,
    providerId: string,
    model: string
  ): Promise<void> {
    const key = `${providerId}:${model}`;
    const cached = BubbleSdkAdapter.contextWindowCache.get(key);
    if (cached !== undefined) {
      session.contextWindow = cached;
      return;
    }
    let contextWindow: number | null = null;
    try {
      const sdk = await getBubbleSdk(session.cwd);
      const profile = sdk.registry.getEnabled().find((entry) => entry.id === providerId);
      if (profile) {
        const models = (await sdk.registry.listModels(profile)) || [];
        const bareModel = model.startsWith(`${providerId}:`)
          ? model.slice(providerId.length + 1)
          : model;
        const match = models.find(
          (entry) => entry.id === model || entry.id === bareModel
        );
        contextWindow =
          typeof match?.contextWindow === 'number' && match.contextWindow > 0
            ? match.contextWindow
            : null;
      }
    } catch (error) {
      console.warn('[BubbleSdkAdapter] failed to resolve model context window:', error);
      return; // Leave uncached so a later turn can retry.
    }
    BubbleSdkAdapter.contextWindowCache.set(key, contextWindow);
    session.contextWindow = contextWindow;
  }

  // ── Session resolution ─────────────────────────────────────────────────

  /**
   * Resume through the durable index, including inputs stopped during setup.
   * A missing resume record must never silently turn a follow-up into a new
   * conversation with no original task.
   */
  private resolveSessionId(
    sdk: BubbleSdkInstance,
    resumeSessionId: string | undefined,
    cwd: string,
    newSessionId: string
  ): string {
    const normalized = resumeSessionId?.trim();
    if (normalized) {
      const match = sdk.listSessions().find((summary) => summary.name === normalized);
      if (match) return normalized;
      throw new Error(`Cannot resume Bubble session "${normalized}": its saved history is missing. No new session was created.`);
    }
    return sdk.createSession({ cwd, id: newSessionId }).id;
  }

  private async resolveCatalogModel(sdk: BubbleSdkInstance, requested?: string, allowDefault = false): Promise<string> {
    const catalog = await getBubbleModelConfig(sdk);
    // Background one-shots may omit a selection, but their default must still
    // come from the available catalog, never the SDK's unvalidated saved value.
    const model = requested?.trim() || (allowDefault ? catalog.defaultModel || catalog.options[0] : undefined);
    const error = bubbleModelSelectionError(model, catalog.options);
    if (error) throw new Error(error);
    return model!;
  }

  private assertConfigured(sdk: BubbleSdkInstance): void {
    let configured = false;
    try {
      const config = sdk.getModelConfig();
      configured = config.providers.some((provider) => provider.hasApiKey);
    } catch (error) {
      throw new Error(
        `Bubble configuration could not be read: ${error instanceof Error ? error.message : String(error)}`
      );
    }
    if (!configured) {
      throw new Error(
        'Bubble has no configured provider credentials. Open Settings → Providers to configure a provider/API key.'
      );
    }
  }

  private emitMessage(session: ActiveBubbleSession, message: StreamMessage): void {
    this.emit({ type: 'message', threadId: session.threadId, message });
  }

  private emit(event: ProviderRuntimeEvent): void {
    if (event.type === 'message') {
      if (this.streamMessages.push({ sessionId: event.threadId, message: event.message })) return;
    } else if ('threadId' in event && event.threadId) {
      // Status, approval and error boundaries must never overtake text.
      this.streamMessages.flushSession(event.threadId);
    }
    this.events.emit('event', event);
  }
}
