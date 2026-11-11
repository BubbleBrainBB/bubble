/**
 * `bubble host`: Bubble's agent behind a line-delimited JSON-RPC pipe, for an
 * app that is not written in JavaScript (protocol.ts lists the messages).
 * Everything goes through the SDK, so a turn here is the turn the terminal
 * and other SDK hosts run: same tools, permission rules, hooks and session
 * files.
 */

import { homedir } from "node:os";
import { dirname } from "node:path";
import { createInterface } from "node:readline";
import type { Readable, Writable } from "node:stream";

import {
  BubbleSdk,
  encodeModel,
  getDefaultThinkingLevel,
  type ApprovalDecision,
  type ApprovalRequest,
  type QuestionAnswer,
  type RunTurnOptions,
  type SdkSessionHandle,
  type ToolRegistryEntry,
} from "../sdk/index.js";
import { SkillRegistry } from "../skills/registry.js";
import { getAvailableThinkingLevels } from "../variant/variant-resolver.js";
import { getBubbleHome } from "../bubble-home.js";
import type { AgentEvent, ThinkingLevel } from "../types.js";
import {
  HOST_PROTOCOL_VERSION,
  type HostAccount,
  type HostInfo,
  type HostModel,
  type HostProvider,
  type HostSkill,
  type HostSkillProblem,
  type HostToolSpec,
  type JsonRpcRequest,
  type JsonRpcResponse,
  type ModelsResult,
  type SessionEventParams,
  type TurnStartParams,
} from "./protocol.js";

export interface HostServerOptions {
  input: Readable;
  output: Writable;
  /** Defaults to a new SDK instance. */
  sdk?: BubbleSdk;
  bubbleVersion?: string;
  /** Sign-in, injectable for tests. */
  login?: (providerId: string, onStatus: (message: string) => void) => Promise<void>;
}

/** JSON-RPC error codes this host uses. */
const METHOD_NOT_FOUND = -32601;
const INVALID_PARAMS = -32602;
const INTERNAL_ERROR = -32603;

/** The subscriptions a person can sign in to. */
const SUBSCRIPTIONS: Array<{ providerId: string; name: string }> = [
  { providerId: "openai", name: "ChatGPT" },
  { providerId: "grok", name: "Grok" },
];

class HostError extends Error {
  constructor(readonly code: number, message: string) {
    super(message);
  }
}

interface WatchedSession {
  handle: SdkSessionHandle;
  /** Tool calls that started and have not ended, for matching approvals to calls. */
  running: Map<string, { name: string; args: Record<string, unknown> }>;
}

interface Outstanding {
  sessionId: string;
  resolve(value: unknown): void;
  /** What the SDK gets when the request is withdrawn. */
  fallback: unknown;
}

export class HostServer {
  private readonly sdk: BubbleSdk;
  private readonly output: Writable;
  private readonly input: Readable;
  private readonly bubbleVersion: string;
  private readonly login: NonNullable<HostServerOptions["login"]>;
  private readonly sessions = new Map<string, WatchedSession>();
  private readonly outstanding = new Map<string, Outstanding>();
  private nextRequestId = 0;
  private closed = false;
  private finish: () => void = () => {};

  constructor(options: HostServerOptions) {
    this.sdk = options.sdk ?? new BubbleSdk();
    this.input = options.input;
    this.output = options.output;
    this.bubbleVersion = options.bubbleVersion ?? "0.0.0";
    this.login = options.login ?? ((providerId, onStatus) => signIn(this.sdk, providerId, onStatus));
  }

  /** Serves until the input ends or the app asks to shut down. */
  async run(): Promise<void> {
    const done = new Promise<void>((resolve) => {
      this.finish = resolve;
    });
    const lines = createInterface({ input: this.input, crlfDelay: Infinity });
    lines.on("line", (line) => this.receive(line));
    lines.on("close", () => this.finish());
    await done;
    this.closed = true;
    lines.close();
    for (const [id] of this.outstanding) this.withdraw(id);
    for (const session of this.sessions.values()) session.handle.close();
    await this.sdk.shutdown();
  }

  // ── Wire ─────────────────────────────────────────────────────────────────

  private send(message: JsonRpcRequest | JsonRpcResponse): void {
    if (this.closed) return;
    this.output.write(`${JSON.stringify(message)}\n`);
  }

  private notify(method: string, params: unknown): void {
    this.send({ jsonrpc: "2.0", method, params });
  }

  private receive(line: string): void {
    const text = line.trim();
    if (!text) return;
    let message: any;
    try {
      message = JSON.parse(text);
    } catch {
      this.notify("log", { message: "Ignored a line that is not JSON." });
      return;
    }
    if (typeof message?.method === "string") {
      void this.dispatch(message as JsonRpcRequest);
      return;
    }
    // An answer to one of the host's own requests.
    if (message?.id === undefined || message.id === null) return;
    const pending = this.outstanding.get(String(message.id));
    if (!pending) return;
    this.outstanding.delete(String(message.id));
    pending.resolve(message.error ? pending.fallback : message.result);
  }

  private async dispatch(request: JsonRpcRequest): Promise<void> {
    const reply = request.id !== undefined && request.id !== null;
    try {
      const result = await this.handle(request.method, (request.params ?? {}) as Record<string, any>);
      if (reply) this.send({ jsonrpc: "2.0", id: request.id!, result: result ?? {} });
    } catch (error) {
      if (!reply) return;
      const code = error instanceof HostError ? error.code : INTERNAL_ERROR;
      const message = error instanceof Error ? error.message : String(error);
      this.send({ jsonrpc: "2.0", id: request.id!, error: { code, message } });
    }
  }

  /**
   * Asks the app and waits. `fallback` is what the caller gets if the app
   * answers with an error or the request is withdrawn (the turn ended, the
   * app went away): always the refusing answer, never the permissive one.
   */
  private ask<T>(sessionId: string, method: string, params: Record<string, unknown>, fallback: T): Promise<T> {
    if (this.closed) return Promise.resolve(fallback);
    const id = `h${++this.nextRequestId}`;
    return new Promise<T>((resolve) => {
      this.outstanding.set(id, { sessionId, resolve: resolve as (value: unknown) => void, fallback });
      this.send({ jsonrpc: "2.0", id, method, params: { sessionId, ...params } });
    });
  }

  private withdraw(id: string): void {
    const pending = this.outstanding.get(id);
    if (!pending) return;
    this.outstanding.delete(id);
    this.notify("request/cancelled", { id });
    pending.resolve(pending.fallback);
  }

  // ── Methods ──────────────────────────────────────────────────────────────

  private async handle(method: string, params: Record<string, any>): Promise<unknown> {
    switch (method) {
      case "initialize":
        return {
          protocolVersion: HOST_PROTOCOL_VERSION,
          bubbleVersion: this.bubbleVersion,
          home: getBubbleHome(),
        } satisfies HostInfo;
      case "session/create": {
        const cwd = requireString(params, "cwd");
        return this.sdk.createSession({ cwd, id: optionalString(params, "id") });
      }
      case "session/list":
        return { sessions: this.sdk.listSessions() };
      case "session/history":
        return {
          messages: this.sdk
            .getHistory(requireString(params, "sessionId"))
            .filter((message) => message.role !== "system" && message.role !== "meta"),
        };
      case "session/state":
        return this.sdk.getSessionRunState(requireString(params, "sessionId"));
      case "session/delete": {
        const sessionId = requireString(params, "sessionId");
        this.sessions.get(sessionId)?.handle.close();
        this.sessions.delete(sessionId);
        await this.sdk.deleteSession(sessionId);
        return {};
      }
      case "turn/start":
        return this.startTurn(params as TurnStartParams);
      case "turn/steer": {
        const result = this.sdk.steer(requireString(params, "sessionId"), requireString(params, "content"));
        return result.accepted
          ? { accepted: true, disposition: result.disposition }
          : { accepted: false, disposition: result.disposition, reason: result.reason };
      }
      case "turn/stop":
        return {
          stopped: this.sdk.stop(requireString(params, "sessionId"), { cancelQueued: params.cancelQueued === true }),
        };
      case "models/list":
        return this.listModels(params.refresh === true);
      case "skills/list":
        return this.listSkills(optionalString(params, "cwd"), stringList(params.skillPaths));
      case "mcp/status":
        return { servers: await this.sdk.getMcpStatus(optionalString(params, "cwd")) };
      case "mcp/reconnect":
        return {
          server: await this.sdk.reconnectMcpServer(requireString(params, "name"), optionalString(params, "cwd")),
        };
      case "auth/list":
        return { accounts: this.accounts() };
      case "auth/login": {
        const providerId = this.subscription(requireString(params, "providerId"));
        await this.login(providerId, (message) => this.notify("auth/progress", { providerId, message }));
        return { account: this.accounts().find((account) => account.providerId === providerId) };
      }
      case "auth/logout": {
        const providerId = this.subscription(requireString(params, "providerId"));
        const storage = this.sdk.registry.getAuthStorage();
        for (const key of this.sdk.registry.getOAuthLoginKeys(providerId)) storage.remove(key);
        return { account: this.accounts().find((account) => account.providerId === providerId) };
      }
      case "text/complete":
        return {
          text: await this.sdk.completeText({
            prompt: requireString(params, "prompt"),
            model: optionalString(params, "model"),
          }),
        };
      case "config/reload":
        reloadConfig(this.sdk);
        return {};
      case "shutdown":
        // Answer first: the reply would be dropped once the server closes.
        setImmediate(() => this.finish());
        return {};
      default:
        throw new HostError(METHOD_NOT_FOUND, `Unknown method: ${method}`);
    }
  }

  // ── Turns ────────────────────────────────────────────────────────────────

  private startTurn(params: TurnStartParams): Record<string, never> {
    const sessionId = requireString(params as unknown as Record<string, any>, "sessionId");
    const prompt = params.prompt;
    if (typeof prompt !== "string" && !Array.isArray(prompt)) {
      throw new HostError(INVALID_PARAMS, "prompt must be a string or a list of content parts");
    }
    // Subscribe before the turn starts so its first event is not missed.
    this.watch(sessionId);
    const options: RunTurnOptions = {
      prompt,
      model: params.model,
      mode: params.mode,
      planExitMode: params.planExitMode,
      thinkingLevel: params.thinkingLevel,
      appendSystemPrompt: params.appendSystemPrompt,
      systemPrompt: params.systemPrompt,
      allowedTools: params.allowedTools,
      skillPaths: params.skillPaths,
      // The person already sent it; a stop during setup must not lose it.
      preserveInterruptedInput: true,
      hostTools: (params.hostTools ?? []).map((spec) => this.hostTool(sessionId, spec)),
      onStart: (info) => this.notify("turn/started", { sessionId, ...info }),
      onApproval: (request) => this.requestApproval(sessionId, request),
      onQuestion: async (request) =>
        (await this.ask<{ answers: QuestionAnswer[] | null }>(sessionId, "question/request", { request }, { answers: null }))
          ?.answers ?? null,
      onPlanApproval: async (plan) =>
        (await this.ask<{ approved: boolean }>(sessionId, "plan/request", { plan }, { approved: false }))?.approved === true,
      onProjectTrust: async ({ cwd, pending }) =>
        (await this.ask<{ trusted: boolean }>(sessionId, "trust/request", { cwd, pending }, { trusted: false }))?.trusted ===
        true,
    };
    // The session subscription carries the events; this iterator is a second
    // reader the host does not need.
    void this.sdk.runTurn(sessionId, options).return(undefined);
    return {};
  }

  private watch(sessionId: string): WatchedSession {
    const existing = this.sessions.get(sessionId);
    if (existing) return existing;
    const handle = this.sdk.openSession(sessionId);
    const watched: WatchedSession = { handle, running: new Map() };
    this.sessions.set(sessionId, watched);
    void this.pump(sessionId, watched, handle.latestSequence);
    return watched;
  }

  private async pump(sessionId: string, watched: WatchedSession, from: number): Promise<void> {
    try {
      for await (const record of watched.handle.eventsFrom(from)) {
        const event = record.event;
        if (event.type === "tool_start") watched.running.set(event.id, { name: event.name, args: event.args });
        if (event.type === "tool_end") watched.running.delete(event.id);
        this.notify("session/event", {
          sessionId,
          turnId: record.turnId,
          sequence: record.sequence,
          event: slim(event),
          ...(record.terminal ? { terminal: record.terminal } : {}),
        } satisfies SessionEventParams);
        if (record.terminal) {
          watched.running.clear();
          // Nothing the turn asked can be answered any more.
          for (const [id, pending] of this.outstanding) {
            if (pending.sessionId === sessionId) this.withdraw(id);
          }
        }
      }
    } catch {
      // The session was deleted or the log closed; nothing more to forward.
    }
  }

  private async requestApproval(sessionId: string, request: ApprovalRequest): Promise<ApprovalDecision> {
    // Let the tool_start that precedes this request reach the pump first.
    await new Promise((resolve) => setImmediate(resolve));
    const toolCallId = this.callFor(sessionId, request);
    const fallback: ApprovalDecision = { action: "reject", feedback: "No answer from the app" };
    const decision = await this.ask<ApprovalDecision>(
      sessionId,
      "approval/request",
      { ...(toolCallId ? { toolCallId } : {}), request },
      fallback,
    );
    if (decision?.action === "approve") {
      return { action: "approve", feedback: decision.feedback, remember: decision.remember === "session" ? "session" : undefined };
    }
    return { action: "reject", feedback: typeof decision?.feedback === "string" ? decision.feedback : undefined };
  }

  /** The running tool call an approval request belongs to, when it can be told. */
  private callFor(sessionId: string, request: ApprovalRequest): string | undefined {
    if (request.type === "external_tool" && request.toolCallId) return request.toolCallId;
    const running = [...(this.sessions.get(sessionId)?.running ?? [])];
    const named = (name: string) => running.filter(([, call]) => call.name === name);
    let candidates: typeof running;
    switch (request.type) {
      case "bash":
        candidates = named("bash").filter(([, call]) => call.args.command === request.command);
        if (candidates.length === 0) candidates = named("bash");
        break;
      case "edit":
      case "write":
        candidates = named(request.type).filter(([, call]) => samePath(call.args.path, request.path));
        if (candidates.length === 0) candidates = named(request.type);
        break;
      case "lsp":
        candidates = named("lsp");
        break;
      case "agent_profile":
        candidates = named("spawn_agent");
        break;
      case "external_tool":
        candidates = named(request.title);
        break;
      default:
        candidates = [];
    }
    if (candidates.length > 0) return candidates[0][0];
    return running.length === 1 ? running[0][0] : undefined;
  }

  private hostTool(sessionId: string, spec: HostToolSpec): ToolRegistryEntry {
    if (!spec || typeof spec.name !== "string" || !spec.name) {
      throw new HostError(INVALID_PARAMS, "every host tool needs a name");
    }
    return {
      name: spec.name,
      description: typeof spec.description === "string" ? spec.description : "",
      parameters: (spec.parameters ?? { type: "object", properties: {} }) as unknown as ToolRegistryEntry["parameters"],
      readOnly: spec.readOnly === true,
      execute: async (args, ctx) => {
        const fallback = { content: "The app did not run this tool.", isError: true };
        const result = await this.ask<{ content?: unknown; isError?: boolean }>(
          sessionId,
          "tool/call",
          { ...(ctx.toolCall?.id ? { toolCallId: ctx.toolCall.id } : {}), name: spec.name, args },
          fallback,
        );
        return {
          content: typeof result?.content === "string" ? result.content : JSON.stringify(result?.content ?? ""),
          isError: result?.isError === true,
        };
      },
    };
  }

  // ── Lists ────────────────────────────────────────────────────────────────

  private async listModels(refresh: boolean): Promise<ModelsResult> {
    const registry = this.sdk.registry;
    const providers: HostProvider[] = [];
    for (const profile of registry.getEnabled()) {
      if (profile.authType === "oauth") {
        // An expired sign-in refreshes here, so the list is the account's own.
        await registry.prepareProvider(profile.id).catch(() => undefined);
      }
      const discovered = await registry
        .discoverModels(profile, { forceRefresh: refresh })
        .catch((error: unknown) => ({ models: [], error: error instanceof Error ? error.message : String(error) }));
      const models: HostModel[] = discovered.models.map((model) => {
        const levels = model.reasoningLevels?.length
          ? model.reasoningLevels
          : getAvailableThinkingLevels(profile.id, model.id);
        const preferred = model.defaultReasoningLevel ?? getDefaultThinkingLevel(profile.id, model.id);
        return {
          reference: encodeModel(profile.id, model.id),
          id: model.id,
          name: model.name || model.id,
          contextWindow: model.contextWindow,
          thinkingLevels: levels,
          defaultThinkingLevel: levels.includes(preferred) ? preferred : (levels[levels.length - 1] ?? "off"),
        };
      });
      providers.push({
        id: profile.id,
        name: profile.name,
        oauth: profile.authType === "oauth",
        models,
        ...(discovered.error ? { error: discovered.error } : {}),
      });
    }
    return {
      defaultModel: this.sdk.userConfig.getDefaultModel() ?? "",
      defaultThinkingLevel: this.sdk.userConfig.getDefaultThinkingLevel() as ThinkingLevel | undefined,
      providers,
    };
  }

  /** Every skill found, usable or switched off, and what could not be read. */
  private listSkills(cwd: string | undefined, skillPaths: string[]): { skills: HostSkill[]; problems: HostSkillProblem[] } {
    const registry = new SkillRegistry({
      cwd: cwd ?? homedir(),
      skillPaths: [...this.sdk.userConfig.getSkillPaths(), ...skillPaths],
      disabledSkills: this.sdk.userConfig.getDisabledSkills(),
    });
    return {
      skills: registry.all().map((skill) => ({
        name: skill.meta.name,
        description: skill.meta.description,
        source: skill.source,
        folder: dirname(skill.rootDir),
        manualOnly: skill.meta.disableModelInvocation,
        enabled: registry.isEnabled(skill.meta.name),
      })),
      // A skill found again in a later folder is the same skill, not a broken one.
      problems: registry
        .getDiagnostics()
        .filter((problem) => !problem.message.startsWith("Duplicate skill name"))
        .map((problem) => ({ message: problem.message, ...(problem.filePath ? { file: problem.filePath } : {}) })),
    };
  }

  private accounts(): HostAccount[] {
    const registry = this.sdk.registry;
    registry.getAuthStorage().reload();
    return SUBSCRIPTIONS.map(({ providerId, name }) => ({
      providerId,
      name,
      signedIn: registry.getOAuthLoginKeys(providerId).length > 0,
    }));
  }

  private subscription(providerId: string): string {
    if (!SUBSCRIPTIONS.some((entry) => entry.providerId === providerId)) {
      throw new HostError(INVALID_PARAMS, `No sign-in for provider: ${providerId}`);
    }
    return providerId;
  }
}

/**
 * Streaming tool arguments repeat everything received so far in each delta;
 * over a pipe that is quadratic for a long write. The delta alone is enough.
 */
function slim(event: AgentEvent): AgentEvent {
  if (event.type === "tool_call_delta") return { ...event, arguments: "" };
  return event;
}

function samePath(a: unknown, b: string): boolean {
  if (typeof a !== "string") return false;
  return a === b || b.endsWith(`/${a.replace(/^\.\//, "")}`);
}

function requireString(params: Record<string, any>, key: string): string {
  const value = params?.[key];
  if (typeof value !== "string" || !value) throw new HostError(INVALID_PARAMS, `${key} is required`);
  return value;
}

function optionalString(params: Record<string, any>, key: string): string | undefined {
  const value = params?.[key];
  return typeof value === "string" && value ? value : undefined;
}

function stringList(value: unknown): string[] {
  return Array.isArray(value) ? value.filter((item): item is string => typeof item === "string") : [];
}

/** Read config.json and models.json again; a file caught mid-write keeps the old state. */
function reloadConfig(sdk: BubbleSdk): void {
  const userConfig = sdk.userConfig as unknown as { load?: () => void; data?: { providers?: unknown } };
  const previous = userConfig.data;
  const hadProviders = Array.isArray(previous?.providers) && previous.providers.length > 0;
  try {
    userConfig.load?.();
  } catch {
    // keep what was loaded
  }
  if (previous && hadProviders && !Array.isArray(userConfig.data?.providers)) userConfig.data = previous;
  try {
    (sdk.registry.getModelConfig() as unknown as { load?: () => void }).load?.();
  } catch {
    // keep what was loaded
  }
  sdk.registry.getAuthStorage().reload();
}

async function signIn(sdk: BubbleSdk, providerId: string, onStatus: (message: string) => void): Promise<void> {
  const storage = sdk.registry.getAuthStorage();
  if (providerId === "grok") {
    const { loginGrok, importGrokCliCredentials } = await import("../oauth/grok.js");
    if (!storage.has(providerId)) {
      // Someone who already ran `grok login` skips the browser.
      const imported = importGrokCliCredentials();
      if (imported) {
        storage.set(providerId, { type: "oauth", ...imported });
        onStatus("Imported the existing Grok CLI sign-in.");
        return;
      }
    }
    const tokens = await loginGrok({ onStatus });
    storage.set(providerId, {
      type: "oauth",
      accessToken: tokens.accessToken,
      refreshToken: tokens.refreshToken,
      expiresAt: tokens.expiresAt,
    });
    return;
  }
  const { loginOpenAICodex } = await import("../oauth/openai-codex.js");
  const tokens = await loginOpenAICodex({ onStatus });
  storage.set(providerId, {
    type: "oauth",
    accessToken: tokens.accessToken,
    refreshToken: tokens.refreshToken,
    expiresAt: tokens.expiresAt,
    idToken: tokens.idToken,
    accountId: tokens.accountId,
  });
}
