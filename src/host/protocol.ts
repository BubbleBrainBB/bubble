/**
 * The wire protocol of `bubble host`: JSON-RPC 2.0, one message per line,
 * over the process's stdin and stdout. A native app starts the process and
 * drives Bubble's agent through it.
 *
 * Both sides send requests. The app asks for sessions, turns and lists; the
 * host asks the app for what only a person or the app can give: a yes to a
 * tool call, an answer to a question, the result of a tool the app runs.
 *
 * App to host
 *   initialize        {}                                   -> HostInfo
 *   session/create    { cwd, id? }                         -> { id, cwd }
 *   session/list      {}                                   -> { sessions }
 *   session/history   { sessionId }                        -> { messages }
 *   session/state     { sessionId }                        -> run state
 *   session/delete    { sessionId }                        -> {}
 *   turn/start        TurnStartParams                      -> {}
 *   turn/steer        { sessionId, content }               -> { accepted, disposition, reason? }
 *   turn/stop         { sessionId, cancelQueued? }         -> { stopped }
 *   conversation/fresh { sessionId }                       -> { outcome: "done" | "pending", part? }
 *   models/list       { refresh? }                         -> ModelsResult
 *   skills/list       { cwd?, skillPaths? }                -> { skills, problems }
 *   mcp/status        { cwd? }                             -> { servers }
 *   mcp/reconnect     { cwd?, name }                       -> { server }
 *   auth/list         {}                                   -> { accounts }
 *   auth/login        { providerId }                       -> { account }
 *   auth/logout       { providerId }                       -> { account }
 *   text/complete     { prompt, model? }                   -> { text }
 *   config/reload     {}                                   -> {}
 *   shutdown          {}                                   -> {}
 *
 * Host to app (requests; the app must answer each)
 *   approval/request  { sessionId, toolCallId?, request }  -> ApprovalDecision
 *   question/request  { sessionId, request }               -> { answers | null }
 *   plan/request      { sessionId, plan }                  -> { approved }
 *   trust/request     { sessionId, cwd, pending }          -> { trusted }
 *   tool/call         { sessionId, toolCallId?, name, args } -> { content, isError? }
 *
 * Host to app (notifications)
 *   session/event     { sessionId, turnId, sequence, event, terminal? }
 *   turn/started      { sessionId, providerId, model, thinkingLevel, mode, tools, skills }
 *   conversation/fresh { sessionId, trigger, part }  as a turn starts: the agent of a long conversation is on an empty thread from here
 *   request/cancelled { id }      a host request the app need not answer any more
 *   auth/progress     { providerId, message }
 *   log               { message }
 */

import type { AgentEvent, ContentPart, PermissionMode, ThinkingLevel } from "../types.js";
import type { ConversationTurn } from "../sdk/conversation.js";
import type { SdkSessionTerminal } from "../sdk/index.js";

/**
 * 2: long conversations (`conversation` on turn/start, conversation/fresh).
 * An app that sends `conversation` to a host of version 1 would have it
 * ignored without a word, so the version says which host it has.
 */
export const HOST_PROTOCOL_VERSION = 2;

export interface HostInfo {
  protocolVersion: number;
  bubbleVersion: string;
  home: string;
}

/** A tool the app runs itself; the host forwards each call as `tool/call`. */
export interface HostToolSpec {
  name: string;
  description: string;
  /** JSON Schema of the arguments. */
  parameters: Record<string, unknown>;
  /**
   * The call changes nothing, or the app asks the person itself. Without it
   * the host asks for approval (`approval/request`) before every call.
   */
  readOnly?: boolean;
}

export interface TurnStartParams {
  sessionId: string;
  prompt: string | ContentPart[];
  /** The folder this turn works in; the session's own when absent. */
  cwd?: string;
  /** "provider:model"; the configured default when absent. */
  model?: string;
  mode?: PermissionMode;
  planExitMode?: Exclude<PermissionMode, "plan">;
  thinkingLevel?: ThinkingLevel;
  /** What the agent is called in its own system prompt; "Bubble" when absent. */
  agentName?: string;
  appendSystemPrompt?: string;
  systemPrompt?: string;
  allowedTools?: string[];
  hostTools?: HostToolSpec[];
  skillPaths?: string[];
  /** Compact the session no later than this many tokens of context, however large the model's window. */
  compactAtTokens?: number;
  /**
   * This session is a conversation that does not end (sdk/conversation.ts):
   * `sessionId` stays the app's name for it, while the agent moves on to an
   * empty thread once its context holds more than the limit. `brief` is
   * what the app keeps outside the conversation, used when the agent has to
   * be told what went before; `standing` is how things stand now, sent with
   * every message.
   */
  conversation?: ConversationTurn;
}

/** The agent of a long conversation started on an empty thread. */
export interface ConversationFreshParams {
  sessionId: string;
  /** "auto": its context had filled up. "manual": someone asked. */
  trigger: "auto" | "manual";
  /** Which thread of the conversation it is on now, counting from 1. */
  part: number;
}

export interface SessionEventParams {
  sessionId: string;
  turnId: string;
  sequence: number;
  event: AgentEvent;
  terminal?: SdkSessionTerminal;
}

export interface HostModel {
  /** "provider:model", what turn/start takes. */
  reference: string;
  id: string;
  name: string;
  contextWindow?: number;
  thinkingLevels: ThinkingLevel[];
  defaultThinkingLevel: ThinkingLevel;
}

export interface HostProvider {
  id: string;
  name: string;
  /** Signed in with a subscription rather than an API key. */
  oauth: boolean;
  models: HostModel[];
  /** Why the list may be incomplete (discovery failed and fell back). */
  error?: string;
}

export interface ModelsResult {
  defaultModel: string;
  defaultThinkingLevel?: ThinkingLevel;
  providers: HostProvider[];
}

export interface HostSkill {
  name: string;
  description: string;
  source: "user" | "project" | "configured";
  /** The skills folder it was found in. */
  folder: string;
  /** Loaded only when the person names it; the model is not told about it. */
  manualOnly: boolean;
  enabled: boolean;
}

export interface HostSkillProblem {
  message: string;
  file?: string;
}

export interface HostAccount {
  providerId: string;
  name: string;
  signedIn: boolean;
}

export interface JsonRpcRequest {
  jsonrpc: "2.0";
  id?: string | number;
  method: string;
  params?: unknown;
}

export interface JsonRpcResponse {
  jsonrpc: "2.0";
  id: string | number;
  result?: unknown;
  error?: { code: number; message: string };
}
