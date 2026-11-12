#!/usr/bin/env node

/**
 * Main entry point - assembles all layers and runs the agent.
 */

import chalk from "chalk";
import { Agent } from "./agent.js";
import { BudgetLedger } from "./agent/budget-ledger.js";
import { enterWorkingDirectory, parseArgs, printHelp } from "./cli.js";
import { effectiveThemeModeForTerminal, shouldProbeTerminalTheme, UserConfig } from "./config.js";
import { createProviderInstance, createUnavailableProvider } from "./provider.js";
import { resolveConfiguredModel } from "./model-selection.js";
import { getAvailableThinkingLevels, getDefaultThinkingLevel, normalizeThinkingLevel } from "./provider-transform.js";
import { ProviderRegistry, displayModel, encodeModel, decodeModel } from "./provider-registry.js";
import { SessionManager } from "./session.js";
import { SessionContextFence } from "./session-context-fence.js";
import { createSessionTitleUpdater, type SessionTitleUpdater } from "./session-title.js";
import { buildSystemPrompt } from "./system-prompt.js";
import { createRoutableModelIndex, createRoutingSnapshotAccessor } from "./agent/routing-catalog.js";
import { buildModelRoutingPrompt } from "./prompt/routing.js";

/** Upper bound on blocking startup for OAuth catalog discovery (one HTTP round trip normally). */
const MODEL_DISCOVERY_STARTUP_WAIT_MS = 3_000;
import { SkillRegistry } from "./skills/registry.js";
import { buildToolPromptOptions, createAllTools, type PlanController, type ToolSearchController } from "./tools/index.js";
import { getProcessManager } from "./tasks/manager.js";
import { PromotionChannel } from "./tasks/promotion.js";
import { runStreamJsonSession } from "./stream-json.js";
import { PrintRunCollector, formatPrintJson, formatPrintJsonError, watchForStalledTurn, type PrintChangeSummary } from "./print-output.js";
import { FileStateTracker } from "./tools/file-state.js";
import { GoalStore } from "./goal/store.js";
import { PermissionAwareApprovalController } from "./approval/controller.js";
import { BashAllowlist } from "./approval/session-cache.js";
import type { ApprovalDecision, ApprovalRequest } from "./approval/types.js";
import { SettingsManager } from "./permissions/settings.js";
import { ExternalHookController } from "./hooks/index.js";
import { getLspService } from "./lsp/index.js";
import { loadMcpConfig } from "./mcp/config.js";
import { promptRepoTrust } from "./permissions/trust-prompt.js";
import { McpManager, gateMcpTools } from "./mcp/manager.js";
import type { PermissionMode, PlanDecision } from "./types.js";
import { normalizeInheritedThinkingLevel } from "./variant/variant-resolver.js";
import { QuestionController } from "./question/index.js";
import {
  buildMemoryPrompt,
  formatMemoryStartupResult,
  purgeUnsafeMemorySources,
  recordMemoryCitations,
  runMemoryPhase2,
  runMemoryStartupPipeline,
  startMemoryStartupTask,
} from "./memory/index.js";
import { basename } from "node:path";
import { normalizeSingleLine, truncateVisual } from "./text-display.js";
import { BUBBLE_WORDMARK, type BubbleWordmarkTone } from "./tui/wordmark.js";
import {
  configureDebugTrace,
  summarizeAgentEventForTrace,
  summarizeTraceMessage,
  traceEvent,
} from "./debug-trace.js";
import { shouldRejectGrokSessionInPrintMode } from "./external-runtime/session-policy.js";
import type { ExternalRuntimeManager } from "./external-runtime/types.js";
import { rmSync } from "node:fs";
import { splitLeadingContext } from "./context/compact.js";

type TerminalTheme = "light" | "dark";

async function main() {
  const args = parseArgs(process.argv.slice(2));

  if (process.argv.includes("-h") || process.argv.includes("--help")) {
    printHelp();
    process.exit(0);
  }

  if (process.argv.includes("-v") || process.argv.includes("--version")) {
    const { getCurrentVersion } = await import("./update/index.js");
    console.log(`v${getCurrentVersion()}`);
    process.exit(0);
  }

  // --cwd means "run as if launched there" (like git -C): every layer, including
  // the TUI, shell children and relative paths, then sees the same directory.
  const cwdError = enterWorkingDirectory(args.cwd);
  if (cwdError) {
    console.error(`bubble: ${cwdError}`);
    process.exit(1);
  }

  if (args.command === "host") {
    const [{ runHostCommand }, { getCurrentVersion }] = await Promise.all([
      import("./host/index.js"),
      import("./update/index.js"),
    ]);
    process.exit(await runHostCommand({ bubbleVersion: getCurrentVersion() }));
  }

  if (args.command === "update") {
    const { runUpdateCommand } = await import("./update/index.js");
    const code = await runUpdateCommand({ checkOnly: args.checkOnly });
    process.exit(code);
  }

  if (args.command === "serve") {
    if (args.serveHost !== "feishu") {
      console.error(chalk.red("Usage: bubble serve --feishu [--setup | --kill-old | --dry-run]"));
      process.exit(2);
    }
    const { serveFeishu } = await import("./feishu/index.js");
    await serveFeishu({
      setup: args.setup,
      killOld: args.killOld,
      dryRun: args.dryRun,
    });
    return;
  }

  const userConfig = new UserConfig();
  const registry = new ProviderRegistry(userConfig);
  const skillRegistry = new SkillRegistry({
    cwd: args.cwd,
    skillPaths: userConfig.getSkillPaths(),
    disabledSkills: userConfig.getDisabledSkills(),
    onDisabledSkillsChange: (disabledSkills) => userConfig.setDisabledSkills(disabledSkills),
  });
  const printMode = args.print || !!args.prompt;

  // Resolve configured providers only; do not auto-inject OpenRouter as a startup default.
  const providers = registry.getConfigured();

  if (providers.length === 0) {
    if (printMode) {
      console.error(chalk.red("Error: No provider configured. Start interactive mode and use /login or /provider --add <id>."));
      process.exit(1);
    }
    console.log(chalk.dim("No provider configured yet. Start with /login for ChatGPT or Grok, or /provider --add <id> for an API key."));
  }

  const defaultProvider = registry.getDefault();
  const unavailableProviderMessage = "No provider configured. Use /login for ChatGPT or Grok, or /provider --add <id> before sending a prompt.";

  let sessionPromptCacheKey: string | undefined;

  const provider = defaultProvider
    ? createProviderInstance({
        providerId: defaultProvider.id,
        apiKey: defaultProvider.apiKey,
        baseURL: defaultProvider.baseURL,
        thinkingLevel: args.thinkingLevel,
        promptCacheKey: sessionPromptCacheKey,
        protocol: defaultProvider.protocol,
        headers: defaultProvider.headers,
        openAICodexAuth: registry.createOpenAICodexAuthAdapter(defaultProvider.id),
        grokAuth: registry.createGrokAuthAdapter(defaultProvider.id),
      })
    : createUnavailableProvider(unavailableProviderMessage);
  const createProvider = (providerId: string, apiKey: string, baseURL: string) => {
    const profile = registry.getConfigured().find((provider) => provider.id === providerId);
    return createProviderInstance({
      providerId,
      apiKey,
      baseURL,
      thinkingLevel: args.thinkingLevel,
      promptCacheKey: sessionPromptCacheKey,
      protocol: profile?.protocol,
      headers: profile?.headers,
      openAICodexAuth: registry.createOpenAICodexAuthAdapter(providerId),
      grokAuth: registry.createGrokAuthAdapter(providerId),
    });
  };
  const createProviderForRoute = async (route: { providerId: string; model: string }) => {
    const providerId = route.providerId;
    if (!providerId) {
      throw new Error(`Subagent route for model "${route.model}" did not include a provider.`);
    }
    if (registry.supportsOAuth(providerId) && registry.getAuthStorage().has(providerId)) {
      await registry.prepareProvider(providerId);
    }
    const target = registry.getConfigured().find((item) => item.id === providerId);
    if (!target?.enabled || !target.apiKey) {
      throw new Error(`Subagent route requires provider "${providerId}", but it is not configured or has no active credentials.`);
    }
    return createProvider(providerId, target.apiKey, target.baseURL);
  };

  let agentRef: Agent | undefined;
  const planHandlerRef: { current?: (plan: string) => Promise<PlanDecision> } = {};
  const planController: PlanController = {
    getMode: () => agentRef?.mode ?? "default",
    requestApproval: (plan) =>
      planHandlerRef.current
        ? planHandlerRef.current(plan)
        : Promise.resolve({
            action: "reject",
            reason: "No interactive UI available to approve the plan.",
          }),
    setMode: (mode: PermissionMode) => {
      agentRef?.setMode(mode);
    },
  };
  const approvalHandlerRef: { current?: (req: ApprovalRequest) => Promise<ApprovalDecision> } = {};
  const questionController = new QuestionController();
  const bashAllowlist = new BashAllowlist();
  // Folder trust (Kimi Code style): the repository's .bubble settings may
  // enable allow rules, MCP servers and LSP servers; ask before loading them.
  if (!printMode && process.stdin.isTTY && process.stdout.isTTY) {
    await promptRepoTrust(args.cwd);
  }
  const settingsManager = new SettingsManager(args.cwd);
  for (const d of settingsManager.getMerged().diagnostics) {
    console.error(chalk.yellow(`[settings:${d.scope}] ${d.path}: ${d.message}`));
  }
  const hookController = new ExternalHookController({ cwd: args.cwd });
  for (const d of hookController.getConfig().diagnostics) {
    console.error(chalk.yellow(`[hooks:${d.scope}] ${d.path}: ${d.message}`));
  }
  const approvalController = new PermissionAwareApprovalController({
    getMode: () => agentRef?.mode ?? "default",
    handlerRef: approvalHandlerRef,
    bashAllowlist,
    sessionGrants: new Set<string>(),
    cwd: args.cwd,
    getRuleSet: () => settingsManager.getMerged().ruleSet,
    externalHooks: hookController,
  });
  const toolSearchController: ToolSearchController = {
    listDeferred: () => agentRef?.listDeferredTools() ?? [],
    unlock: (names) => agentRef?.unlockDeferredTools(names),
  };
  const lspService = getLspService(args.cwd, settingsManager.getMerged().lsp);
  const fileStateTracker = new FileStateTracker(args.cwd);
  // Shared between the goal tool (model-facing update_goal) and the
  // TUI's auto-continuation engine / status-line indicator.
  const goalStore = new GoalStore();
  // Background tasks are TUI-only (design §2.0): print mode exits (and reaps)
  // right after the single run, so a backgrounded command would be killed
  // immediately after the model was told it started. Feishu/desktop hosts
  // never reach this call site and stay off until they wire wake seams.
  const processManager = getProcessManager();
  const allowBackgroundTasks = !printMode;
  const promotionChannel = new PromotionChannel();
  const tools = createAllTools(args.cwd, skillRegistry, {
    planController,
    approvalController,
    questionController: printMode ? undefined : questionController,
    toolSearchController,
    lspService,
    fileStateTracker,
    goalStore,
    processManager,
    allowBackgroundTasks,
    promotionChannel,
    autoBackgroundAfterMs: userConfig.getTasksAutoBackgroundAfterMs(),
    // Lazy: sessionManager is resolved after tools are created.
    checkpoints: () => sessionManager?.getCheckpoints(),
  });

  // Bring up MCP servers (if any). Failures are captured per-server and never
  // block the rest of startup; /mcp surfaces status at runtime.
  const mcpLoaded = loadMcpConfig({ cwd: args.cwd });
  for (const d of mcpLoaded.diagnostics) {
    console.error(chalk.yellow(`[mcp:${d.scope}] ${d.path}: ${d.message}`));
  }
  // Silent diagnostics: connect failures are captured per-server and shown by
  // /mcp on demand. Printing them at startup (or mid-session, where stderr
  // writes corrupt the Ink display) only duplicates that surface.
  const mcpManager = new McpManager({ servers: mcpLoaded.servers, onDiagnostic: () => {} });
  let externalRuntime: ExternalRuntimeManager | undefined;
  if (mcpLoaded.servers.length > 0) {
    await mcpManager.start();
    tools.push(...gateMcpTools(mcpManager.getToolEntries(), approvalController));
  }

  // Expose MCP prompts as slash commands. Queried live at each lookup so
  // /mcp reconnect picks up new prompts without restarting the process.
  {
    const { registry: slashRegistry } = await import("./slash-commands/index.js");
    slashRegistry.addDynamicSource(() => mcpManager.getPromptCommands());
  }
  // Signal-based shutdown for Ctrl-C / kill. Normal /quit cleanup happens after
  // the TUI renderer has been destroyed, avoiding native teardown races.
  const shutdownMcp = async () => {
    try {
      await mcpManager.shutdown();
    } catch {
      // ignore — we're exiting anyway
    }
  };
  const shutdownExternalRuntime = async () => {
    try {
      await externalRuntime?.dispose();
    } catch {
      // ignore — we're exiting anyway
    }
  };
  const shutdownSidecars = async () => {
    // Signal path (design §2.2b layer 2): kill-only, no escalation wait —
    // the process is about to exit and shutdownRuntime never runs here.
    processManager.reapTasksSync();
    await Promise.allSettled([shutdownMcp(), shutdownExternalRuntime()]);
  };
  process.once("SIGINT", () => { void shutdownSidecars().then(() => process.exit(130)); });
  process.once("SIGTERM", () => { void shutdownSidecars().then(() => process.exit(143)); });

  // Session management:
  // - default: always start a fresh session
  // - --resume --session <name>: restore the named session
  // - --resume (no name): show interactive picker
  let sessionManager: SessionManager | undefined;
  let resumedExistingSession = false;
  // Resolved before any TUI render so picker and main TUI share the same value
  // and we only run OSC 11 once.
  let preResolvedTheme: TerminalTheme | undefined;

  if (args.resume && !args.sessionName) {
    const currentSessions = SessionManager.summarizeSessionsForCwd(args.cwd);
    const allSessions = SessionManager.listAllSessions();
    if (currentSessions.length === 0 && allSessions.length === 0) {
      console.log(chalk.dim("No previous sessions found — starting a fresh one."));
    } else {
      const themeConfig = userConfig.getTheme();
      if (shouldProbeTerminalTheme(themeConfig)) {
        const { detectTerminalTheme } = await import("./tui/detect-theme.js");
        preResolvedTheme = await detectTerminalTheme();
      } else {
        preResolvedTheme = themeConfig.mode === "light" ? "light" : "dark";
      }
      const pickerThemeMode = effectiveThemeModeForTerminal(themeConfig, preResolvedTheme);
      const pickerResolvedTheme = pickerThemeMode === "auto" ? preResolvedTheme : pickerThemeMode;
      const { runSessionPicker } = await import("./tui/run-session-picker.js");
      const { canvasBackgroundFor } = await import("./tui/model/theme.js");
      // Same rule as the main TUI: a forced theme mismatching the terminal
      // paints its canvas so its foregrounds stay readable.
      const pickerCanvas = themeConfig.overrides?.background
        ? undefined
        : canvasBackgroundFor(pickerThemeMode, pickerResolvedTheme, preResolvedTheme);
      const picked = await runSessionPicker({
        currentCwd: args.cwd,
        currentSessions,
        allSessions,
        resolvedTheme: pickerResolvedTheme,
        themeOverrides: pickerCanvas
          ? { ...themeConfig.overrides, background: pickerCanvas }
          : themeConfig.overrides,
      });
      if (picked) {
        sessionManager = new SessionManager(picked);
        resumedExistingSession = true;
      }
    }
  } else if (args.resume) {
    sessionManager = SessionManager.resume(args.cwd, args.sessionName);
    resumedExistingSession = !!sessionManager;
  }

  if (!sessionManager) {
    sessionManager = args.sessionName && !args.resume
      ? SessionManager.create(args.cwd, args.sessionName)
      : SessionManager.createFresh(args.cwd);
    resumedExistingSession = false;
  }
  sessionPromptCacheKey = sessionManager.getOrCreatePromptCacheKey();

  // Model resolution:
  // 1. CLI flag  2. Session metadata  3. User-configured default model
  // No implicit built-in model fallback.
  const fallbackProviderId = defaultProvider?.id || "";
  const sessionModel = sessionManager?.getMetadata().model;
  const defaultModel = userConfig.getDefaultModel();
  const sessionThinkingLevel = sessionManager?.getMetadata().thinkingLevel;
  const configuredThinkingLevel = userConfig.getDefaultThinkingLevel();
  const normalizedConfiguredModel = resolveConfiguredModel({
    cliModel: args.model,
    sessionModel,
    defaultModel,
    fallbackProviderId,
  });
  const { providerId: effectiveProviderId, modelId: effectiveModelId } = normalizedConfiguredModel
    ? decodeModel(normalizedConfiguredModel)
    : { providerId: undefined, modelId: "" };
  let activeProviderId = effectiveProviderId || fallbackProviderId;
  // getConfigured() resolves the legacy openai-codex auth alias; the raw
  // auth key would skip token refresh and discovery for such logins.
  if (registry.getConfigured().find((item) => item.id === activeProviderId)?.authType === "oauth") {
    try {
      await registry.prepareProvider(activeProviderId);
    } catch (error) {
      // A dead login must not lock the user out: /login only exists inside the
      // session, so start anyway and let the first request surface the error.
      console.error(chalk.yellow(`[auth] ${error instanceof Error ? error.message : String(error)}`));
    }
    // The routing prompt below is composed once; give the account catalog a
    // bounded chance to land first so the menu the model reads matches what a
    // spawn will actually get (design §1.5).
    await registry.waitForModelDiscovery(activeProviderId, MODEL_DISCOVERY_STARTUP_WAIT_MS);
  }
  const activeProvider = registry.getConfigured().find((p) => p.id === activeProviderId) || defaultProvider;
  const activeModel = activeProvider && effectiveModelId
    ? encodeModel(activeProviderId, effectiveModelId)
    : "";
  if (!activeModel && !activeProvider) {
    activeProviderId = "";
  }
  const initialThinkingLevel = activeModel
    ? args.thinkingLevel !== undefined
      ? normalizeThinkingLevel(
          args.thinkingLevel,
          getAvailableThinkingLevels(activeProviderId, effectiveModelId),
        )
      : sessionThinkingLevel !== undefined
        ? normalizeInheritedThinkingLevel(activeProviderId, effectiveModelId, sessionThinkingLevel)
        : configuredThinkingLevel !== undefined
          ? normalizeThinkingLevel(
              configuredThinkingLevel,
              getAvailableThinkingLevels(activeProviderId, effectiveModelId),
            )
          : getDefaultThinkingLevel(activeProviderId, effectiveModelId)
    : (args.thinkingLevel ?? sessionThinkingLevel ?? configuredThinkingLevel ?? "off");
  const initialMode: PermissionMode = args.mode ?? "default";
  const skillSummaries = skillRegistry.summaries();
  // This synchronous provenance gate must run before buildMemoryPrompt: the
  // background startup pipeline is too late once text has entered the native
  // provider's system prompt.
  purgeUnsafeMemorySources(args.cwd);
  const memoryPrompt = buildMemoryPrompt(args.cwd);
  // Routing snapshot accessor (model-routing design §1.5): built before the
  // system prompt because startup composes the prompt before the Agent exists;
  // the same accessor is then handed to the Agent constructor.
  const agentRouting = userConfig.getAgentRouting();
  const routingSnapshotAccessor = createRoutingSnapshotAccessor(
    registry,
    () => userConfig.getAgentCategories(),
    () => agentRouting,
  );
  const initialRoutingParent = activeProviderId && activeModel
    ? { providerId: activeProviderId, model: decodeModel(activeModel).modelId }
    : undefined;
  const systemPrompt = buildSystemPrompt({
    agentName: "Bubble",
    skills: skillSummaries,
    skillCatalogChars: userConfig.getSkillCatalogChars(),
    configuredProvider: activeProviderId || "none",
    configuredModel: activeModel ? displayModel(activeModel) : "none",
    configuredModelId: activeModel || "none",
    thinkingLevel: initialThinkingLevel,
    mode: initialMode,
    workingDir: args.cwd,
    ...buildToolPromptOptions(tools.filter((tool) => !tool.deferred)),
    memoryPrompt,
    modelRoutingPrompt: initialRoutingParent
      ? buildModelRoutingPrompt(routingSnapshotAccessor(initialRoutingParent), agentRouting)
      : undefined,
  });
  const traceInfo = configureDebugTrace({
    cwd: args.cwd,
    sessionFile: sessionManager?.getSessionFile(),
    provider: activeProviderId || "none",
    model: activeModel || "none",
    renderer: printMode ? "print" : "pi-tui",
  });
  if (traceInfo.enabled) {
    traceEvent("run_start", {
      tracePath: traceInfo.path,
      rawEnabled: traceInfo.rawEnabled,
      resumed: resumedExistingSession,
      printMode,
      mode: initialMode,
      thinkingLevel: initialThinkingLevel,
      tools: tools.length,
      cwd: args.cwd,
    });
  }
  const budgetLedger = new BudgetLedger();
  let sessionTitleUpdater: SessionTitleUpdater | undefined;
  let contextFence = new SessionContextFence(sessionManager);
  // A fence rejection means the durable log diverged from this host's resident
  // history (a foreign append/clear/rewind landed while a turn was in flight).
  // The write is refused — and the resident transcript must roll back to the
  // file's truth, or the rejected message lingers in the TUI while every later
  // fenced write keeps failing on the stale revision until a manual reload.
  const persistFenced = (write: () => void): void => {
    try {
      write();
    } catch (error) {
      // Any refused write (fence rejection, busy lock, I/O) leaves the agent's
      // already-pushed message unpersisted, so always restore the file's truth.
      try {
        const history = contextFence.reloadHistory();
        const head = splitLeadingContext(agent.messages).leading;
        agent.messages = [...head, ...history];
      } catch {
        // Surface the original write failure; the next write retries the reload.
      }
      throw error;
    }
  };
  const agent = new Agent({
    provider: activeProvider
      ? createProvider(activeProviderId, activeProvider.apiKey, activeProvider.baseURL)
      : provider,
    providerId: activeProviderId || "",
    model: activeModel,
    sessionID: sessionManager?.getSessionFile(),
    tools,
    systemPrompt,
    temperature: 0.2,
    thinkingLevel: initialThinkingLevel,
    mode: initialMode,
    onMessageAppend: (message) => {
      if (!sessionManager) return;
      if (message.role === "system") return;
      // Runtime meta messages are ephemeral; don't persist them —
      // they will be re-injected as needed on resume based on the current mode.
      if (message.role === "meta") return;
      const manager = sessionManager;
      persistFenced(() => manager.appendMessage(message, contextFence.getRevision()));
      traceEvent("session_message_persisted", {
        message: summarizeTraceMessage(message),
      });
      sessionTitleUpdater?.handlePersistedMessage(message);
      if (message.role === "assistant") {
        recordMemoryCitations(args.cwd, message.content);
      }
    },
    onProviderError: (error) => {
      persistFenced(() => sessionManager?.appendProviderError(error, contextFence.getRevision()));
    },
    getContextRevision: () => contextFence.getRevision(),
    onContextCheckpoint: (checkpoint) => {
      if (!sessionManager) throw new Error("No session available for context commit");
      const manager = sessionManager;
      persistFenced(() => manager.commitContextCheckpoint(checkpoint, contextFence.getRevision()));
    },
    onToolResult: (toolName, result) => {
      if (!sessionManager) return;
      if (toolName !== "skill" || result.isError) return;
      const match = result.content.match(/^Skill:\s+([^\n]+)$/m);
      if (match?.[1]) {
        const manager = sessionManager;
        persistFenced(() => manager.appendMarker("skill_activated", match[1]!.trim(), contextFence.getRevision()));
      }
    },
    onModeUpdate: (mode) => {
      persistFenced(() => sessionManager?.appendMarker("mode_switch", mode, contextFence.getRevision()));
    },
    budgetLedger,
    skills: skillSummaries,
    skillCatalogChars: userConfig.getSkillCatalogChars(),
    memoryPrompt,
    fileStateTracker,
    agentCategories: userConfig.getAgentCategories(),
    agentRouting,
    routingSnapshot: routingSnapshotAccessor,
    routableModels: createRoutableModelIndex(registry),
    subagents: userConfig.getSubagents(),
    providerFactory: createProviderForRoute,
    externalHooks: hookController,
  });
  agentRef = agent;
  if (allowBackgroundTasks) {
    // Reminder bridge (design §2.3a): list() is owner-filtered at call time so
    // the reminder never leaks another session's tasks after a switch.
    agent.backgroundTasks = {
      list: () => processManager.listTasks(agent.getSessionID()),
      version: () => processManager.getTaskStateVersion(),
      outputTail: (id) => processManager.taskOutputTail(id, 2000),
      kill: (id) => processManager.killTask(id),
      subscribe: (listener) => processManager.onChange(() => listener()),
    };
  }
  if (sessionManager) {
    sessionTitleUpdater = createSessionTitleUpdater({
      sessionManager,
      complete: (messages, completeOptions) => agent.complete(messages, completeOptions),
    });
  }
  if (sessionManager) {
    sessionManager.updateMetadata({
      ...(agent.model ? { model: agent.model } : {}),
      cwd: args.cwd,
      thinkingLevel: agent.thinking,
      reasoningEffort: agent.thinking,
    });
  }
  await hookController.runEvent({
    eventName: "SessionStart",
    cwd: args.cwd,
    sessionId: sessionManager?.getSessionFile(),
    agentRole: "driver",
    target: "session",
    payload: {
      resumed: resumedExistingSession,
      printMode,
      providerId: agent.providerId,
      model: agent.apiModel,
    },
  });

  const flushMemory = async () => {
    // Codex-style memory runs at startup over historical rollouts. Exit should
    // not perform an ad-hoc extraction of the just-finished session.
  };
  const shutdownRuntime = async () => {
    try {
      await hookController.runEvent({
        eventName: "SessionEnd",
        cwd: args.cwd,
        sessionId: sessionManager?.getSessionFile(),
        agentRole: "driver",
        target: "session",
        payload: {
          providerId: agent.providerId,
          model: agent.apiModel,
        },
      });
    } catch {
      // Shutdown must still release child processes when a hook fails.
    }
    const results = await Promise.allSettled([
      flushMemory(),
      shutdownMcp(),
      shutdownExternalRuntime(),
      lspService.shutdown(),
      // Graceful task reaping (design §2.2b layer 1): SIGTERM → SIGKILL.
      processManager.shutdownTasks(),
    ]);
    for (const result of results) {
      if (result.status === "rejected") {
        // Shutdown is best-effort; never turn exit into a fatal error.
      }
    }
  };
  const runMemoryCompaction = async () =>
    formatMemoryStartupResult(await runMemoryStartupPipeline({
      cwd: args.cwd,
      complete: (messages, completeOptions) => agent.complete(messages, completeOptions),
      model: agent.apiModel,
    }));
  const runMemorySummary = async () => {
    const result = await runMemoryPhase2({
      cwd: args.cwd,
      complete: (messages, completeOptions) => agent.complete(messages, completeOptions),
      model: agent.apiModel,
    });
    return `Memory Phase 2 ${result.status}: selected ${result.selected}${result.reason ? ` (${result.reason})` : ""}.`;
  };
  const runMemoryRefresh = runMemoryCompaction;

  startMemoryStartupTask({
    cwd: args.cwd,
    complete: (messages, completeOptions) => agent.complete(messages, completeOptions),
    model: agent.apiModel,
  });

  if (activeModel && args.model && normalizedConfiguredModel === agent.model) {
    userConfig.pushRecentModel(agent.model);
  }

  // Restore session if requested
  if (resumedExistingSession && sessionManager) {
    const history = contextFence.reloadHistory();
    if (history.length > 0) {
      agent.messages = [{ role: "system", content: systemPrompt }, ...history];
      // Reassigning agent.messages drops any runtime meta reminder injected during
      // construction. Re-inject if the agent is starting in plan mode.
      if (agent.mode === "plan") {
        agent.injectModeReminder();
      }
      agent.injectDeferredToolsReminder();
      // JSON print mode reserves stdout for the single result object.
      (args.outputFormat === "json" || args.outputFormat === "stream-json" ? console.error : console.log)(
        chalk.dim(`Resumed session: ${sessionManager.getSessionFile()}`),
      );
    }
  }

  try {
    // Print mode: single prompt, then exit
    if (args.print || args.prompt) {
      if (shouldRejectGrokSessionInPrintMode(
        sessionManager?.getMetadata().externalRuntime,
        true,
      )) {
        console.error(chalk.red(
          "Error: Grok subscription sessions are interactive. Resume this workspace session in the TUI, or start a fresh native session for --print.",
        ));
        process.exitCode = 1;
        return;
      }
      if (args.inputFormat === "stream-json") {
        await runStreamJsonSession({
          input: process.stdin,
          write: (message) => { process.stdout.write(JSON.stringify(message) + "\n"); },
          sessionId: basename(sessionManager.getSessionFile()),
          model: agent.model,
          run: (prompt) => agent.run(prompt, args.cwd),
          onEvent: (event) => traceEvent("print_agent_event", summarizeAgentEventForTrace(event)),
        });
        return;
      }
      const prompt = args.prompt || (await readPipedStdin()) || "";
      if (!prompt) {
        console.error(chalk.red("Error: No prompt provided."));
        process.exit(1);
      }

      // JSON mode (benchmark/CI adapters): stdout carries exactly ONE JSON
      // object; streaming text and tool logs move to stderr-silence. Plain
      // mode is byte-for-byte the previous behavior.
      const jsonOutput = args.outputFormat === "json";
      const collector = new PrintRunCollector();
      // Harness-side change footprint (git ground truth) — printed by the
      // harness so the run cannot omit or misstate what it touched.
      const changeBaseline = jsonOutput
        ? await (await import("./agent/change-tracker.js")).captureGitBaseline(args.cwd)
        : null;
      const collectChanges = async (): Promise<PrintChangeSummary | undefined> => {
        if (!changeBaseline) return undefined;
        const { detectRunChanges } = await import("./agent/change-tracker.js");
        const detected = await detectRunChanges(args.cwd, changeBaseline).catch(() => null);
        if (!detected) return undefined;
        return {
          changed_files: detected.changedFiles.length,
          modified_existing_tests: detected.modifiedExistingTests.map((t) => ({
            path: t.path,
            deleted_lines: t.deletedLines,
          })),
        };
      };
      const printSessionId = sessionManager
        ? basename(sessionManager.getSessionFile())
        : undefined;
      let printedTurnText = false;
      // A turn that can no longer settle must not read as a success: without
      // this the process exits 0 with a partial answer (plain) or no output
      // at all (json).
      const turnSettled = watchForStalledTurn(() => {
        const message = "The turn stopped before the model's reply was complete: nothing was left running that could finish it.";
        process.exitCode = 1;
        if (!jsonOutput) {
          if (printedTurnText) process.stdout.write("\n");
          console.error(chalk.red(`Error: ${message}`));
          return;
        }
        void collectChanges().catch(() => undefined).then((changes) => {
          process.stdout.write(formatPrintJsonError({
            message,
            summary: collector.summary(),
            sessionId: printSessionId,
            compaction: agent.getCompactionStats(),
            changes,
          }) + "\n");
        });
      });
      try {
        for await (const event of agent.run(prompt, args.cwd)) {
          traceEvent("print_agent_event", summarizeAgentEventForTrace(event));
          collector.onEvent(event);
          if (jsonOutput) {
            if (event.type === "provider_retry") {
              console.error(chalk.yellow(
                `[Stream interrupted; retrying (${event.attempt}/${event.maxAttempts})]`,
              ));
            }
            continue;
          }
          if (event.type === "turn_start") {
            printedTurnText = false;
          } else if (event.type === "provider_retry") {
            // The stream died mid-response and the agent re-issues the whole
            // request. Text already on stdout cannot be un-printed, so at least
            // separate the retried response and say what happened.
            if (printedTurnText) process.stdout.write("\n");
            console.error(chalk.yellow(
              `[Stream interrupted; retrying (${event.attempt}/${event.maxAttempts}) — the partial text above is superseded by the retried response]`,
            ));
          } else if (event.type === "text_delta") {
            printedTurnText = true;
            process.stdout.write(event.content);
          } else if (event.type === "tool_start") {
            console.log(chalk.cyan(`\n[Tool: ${event.name}]`));
          } else if (event.type === "tool_end") {
            const color = event.result.isError ? chalk.red : chalk.dim;
            console.log(color(`[Result: ${event.result.content.slice(0, 200)}${event.result.content.length > 200 ? "..." : ""}]`));
          }
        }
        if (jsonOutput) {
          process.stdout.write(formatPrintJson({
            summary: collector.summary(),
            sessionId: printSessionId,
            compaction: agent.getCompactionStats(),
            changes: await collectChanges(),
          }) + "\n");
        } else {
          console.log();
        }
      } catch (error) {
        if (!jsonOutput) throw error;
        // Structured failure: adapters distinguish agent errors from crashes
        // by the JSON error object + non-zero exit.
        process.stdout.write(formatPrintJsonError({
          message: error instanceof Error ? error.message : String(error),
          summary: collector.summary(),
          sessionId: printSessionId,
          compaction: agent.getCompactionStats(),
          changes: await collectChanges(),
        }) + "\n");
        process.exitCode = 1;
      } finally {
        turnSettled();
      }

      return;
    }

    const themeConfig = userConfig.getTheme();
    let detectedTheme: "light" | "dark" = "dark";
    if (preResolvedTheme) {
      detectedTheme = preResolvedTheme;
    } else if (shouldProbeTerminalTheme(themeConfig)) {
      // Probe before the renderer owns stdin. OSC 11 needs raw mode, and the
      // runtime renderer can consume the reply before startup code sees it.
      const { detectTerminalTheme } = await import("./tui/detect-theme.js");
      detectedTheme = await detectTerminalTheme();
    } else {
      detectedTheme = themeConfig.mode === "light" ? "light" : "dark";
    }
    const effectiveThemeMode = effectiveThemeModeForTerminal(themeConfig, detectedTheme);
    // In-place session switch for the /session picker: rebind every closure
    // that persists to the session (onMessageAppend, markers, title updater)
    // by reassigning the outer `sessionManager`, then replace the agent's
    // history the same way startup resume does.
    const activateSession = (next: SessionManager): { manager: SessionManager } | { error: string } => {
      try {
        const history = next.getMessages();
        const historyRevision = next.getRevision();
        const nextPromptCacheKey = next.getOrCreatePromptCacheKey();
        const nextTitleUpdater = createSessionTitleUpdater({
          sessionManager: next,
          complete: (messages, completeOptions) => agent.complete(messages, completeOptions),
        });
        next.updateMetadata({
          ...(agent.model ? { model: agent.model } : {}),
          cwd: args.cwd,
          thinkingLevel: agent.thinking,
          reasoningEffort: agent.thinking,
        });
        // Keep the live system/meta head (mode reminders survive the switch),
        // mirroring the /rewind history-replacement pattern.
        const head = splitLeadingContext(agent.messages).leading;
        const nextMessages = [...head, ...history];

        // Commit only after every file read/write and reconstruction step has
        // succeeded. Callers can safely prepare candidate sessions without a
        // failed switch rebinding persistence or replacing the live history.
        if (next.getRevision() !== historyRevision) throw new Error("Session changed while switching; retry");
        contextFence.dispose();
        contextFence = new SessionContextFence(next);
        sessionManager = next;
        sessionPromptCacheKey = nextPromptCacheKey;
        sessionTitleUpdater = nextTitleUpdater;
        agent.messages = nextMessages;
        // These update only live in-memory/UI mirrors after the persistence
        // commit. A best-effort callback failure must not report the switch as
        // rolled back after the outer session binding has already changed.
        try { agent.resetContextUsageAnchor(); } catch { /* derived counter only */ }
        return { manager: next };
      } catch (error) {
        return { error: error instanceof Error ? error.message : String(error) };
      }
    };
    const switchSession = (sessionFile: string): { manager: SessionManager } | { error: string } => {
      try {
        return activateSession(new SessionManager(sessionFile));
      } catch (error) {
        return { error: error instanceof Error ? error.message : String(error) };
      }
    };
    const createFreshSession = (cwd: string): { manager: SessionManager } | { error: string } => {
      let fresh: SessionManager | undefined;
      try {
        fresh = SessionManager.createFresh(cwd);
        const result = activateSession(fresh);
        if ("error" in result) rmSync(fresh.getSessionFile(), { force: true });
        return result;
      } catch (error) {
        if (fresh) rmSync(fresh.getSessionFile(), { force: true });
        return { error: error instanceof Error ? error.message : String(error) };
      }
    };

    const commonOptions = {
      sessionManager,
      switchSession,
      createFreshSession,
      createProvider,
      registry,
      skillRegistry,
      planHandlerRef,
      approvalHandlerRef,
      questionController,
      bashAllowlist,
      settingsManager,
      lspService,
      mcpManager,
      goalStore,
      hookController,
      flushMemory,
      runMemoryCompaction,
      runMemorySummary,
      runMemoryRefresh,
      processManager,
      tasksAutoResume: userConfig.getTasksAutoResume(),
      promotionChannel,
    };
    // Grok Subscription is a first-class interactive provider target. Manager
    // construction is deliberately lazy: it does not inspect, spawn, or reach
    // the network until the user selects Grok or runs /login grok. Print,
    // Feishu, help, and version paths return before this import.
    const { createGrokRuntimeManager } = await import("./external-runtime/grok-runtime.js");
    externalRuntime = createGrokRuntimeManager({
      workspace: args.cwd,
      approvalController,
      getPermissionMode: () => agentRef?.mode ?? "default",
    });
    const { startStartupUpdateCheck } = await import("./update/index.js");
    const updateCheck = await startStartupUpdateCheck();
    const updateNotice = updateCheck.notice;
    const { runTui } = await import("./tui/run.js");
    const summary = await runTui(agent, args, {
      ...commonOptions,
      themeMode: effectiveThemeMode,
      themeOverrides: themeConfig.overrides,
      detectedTheme,
      onThemeModeChange: (mode) => userConfig.setThemeMode(mode),
      updateNotice: updateNotice ?? undefined,
      updateNoticeRefresh: updateCheck.refreshed,
      externalRuntime,
    });
    const exitWallMs = summary?.wallMs;

    if (sessionManager) {
      printExitSummary(sessionManager, {
        resumed: resumedExistingSession,
        theme: detectedTheme,
        wallMs: exitWallMs,
      });
    }
  } finally {
    traceEvent("run_shutdown_start");
    await shutdownRuntime();
    traceEvent("run_shutdown_end");
  }
}

function printExitSummary(
  sessionManager: SessionManager,
  options: { resumed: boolean; theme: TerminalTheme; wallMs?: number },
) {
  if (!process.stdout.isTTY) return;
  const sessionName = basename(sessionManager.getSessionFile());
  const sessionId = sessionName.replace(/\.jsonl$/, "");
  const title = truncateVisual(normalizeSingleLine(sessionManager.getMetadata().title ?? ""), 64);
  const sessionLabel = title || `${options.resumed ? "Session" : "New session"} - ${sessionId}`;
  const continueCommand = `bubble --resume --session ${sessionName}`;
  const colors = options.theme === "light"
    ? {
        markMuted: chalk.hex("#8C8C8C"),
        markStrong: chalk.hex("#1C1C1C"),
        markBrand: chalk.hex("#8B4A00"),
        label: chalk.hex("#6F7377"),
        value: chalk.hex("#171717").bold,
      }
    : {
        markMuted: chalk.hex("#9CA3AF"),
        markStrong: chalk.hex("#F4F4F5"),
        markBrand: chalk.hex("#F5A742"),
        label: chalk.hex("#808080"),
        value: chalk.hex("#EEEEEE").bold,
      };
  const label = (value: string) => colors.label(value.padEnd(10));
  const logoColor = (tone: BubbleWordmarkTone) => {
    switch (tone) {
      case "brand": return colors.markBrand;
      case "ink": return colors.markStrong;
      case "stone": return colors.markMuted;
      case "soft": return colors.label;
      case "caption": return colors.label;
    }
  };

  for (const line of BUBBLE_WORDMARK) {
    if (line.segments) {
      console.log(line.segments.map((segment) => logoColor(segment.tone)(segment.text)).join(""));
    } else {
      console.log(logoColor(line.tone ?? "caption")(line.text ?? ""));
    }
  }
  console.log();
  console.log(`${label("Session")}${colors.value(sessionLabel)}`);
  console.log(`${label("Continue")}${colors.value(continueCommand)}`);
  if (options.wallMs !== undefined) {
    console.log(`${label("Duration")}${colors.value(formatWallDuration(options.wallMs))}`);
  }
}

function formatWallDuration(ms: number): string {
  const totalSeconds = Math.max(0, Math.round(ms / 1000));
  if (totalSeconds < 60) return `${totalSeconds}s`;
  const minutes = Math.floor(totalSeconds / 60);
  const seconds = totalSeconds % 60;
  if (minutes < 60) return `${minutes}m ${seconds}s`;
  const hours = Math.floor(minutes / 60);
  const minutesRest = minutes % 60;
  return `${hours}h ${minutesRest}m ${seconds}s`;
}

async function readPipedStdin(): Promise<string | undefined> {
  if (process.stdin.isTTY) return undefined;
  return new Promise((resolve) => {
    let data = "";
    process.stdin.setEncoding("utf8");
    process.stdin.on("data", (chunk) => (data += chunk));
    process.stdin.on("end", () => resolve(data.trim() || undefined));
    process.stdin.resume();
  });
}

main()
  .then(() => {
    // Preserve an exit code set by a handled failure path (e.g. print-mode
    // agent errors set exitCode = 1 and return normally) — exit(0) here
    // would silently override it.
    void exitAfterFlush(typeof process.exitCode === "number" ? process.exitCode : 0);
  })
  .catch((err) => {
    console.error(chalk.red(`Fatal error: ${err.message}`));
    void exitAfterFlush(1);
  });

async function exitAfterFlush(code: number): Promise<void> {
  await Promise.all([
    flushStream(process.stdout),
    flushStream(process.stderr),
  ]);
  process.exit(code);
}

function flushStream(stream: NodeJS.WriteStream): Promise<void> {
  if (stream.destroyed || stream.writableEnded) return Promise.resolve();
  return new Promise((resolve) => {
    stream.write("", () => resolve());
  });
}
