/**
 * A conversation that does not end.
 *
 * A long-lived agent (an app's main bot) has one conversation with the
 * person for weeks. Sent whole with every turn it only grows, and the
 * context compaction of a single session is there to keep a window from
 * overflowing, not to keep a conversation small: on a model with a
 * million-token window it starts at about a million tokens.
 *
 * So such a conversation runs on a line of sessions, its threads, one after
 * another. Between two turns, once the context holds more than a limit, the
 * agent starts on an empty thread and is told what went before: the end of
 * the conversation it no longer has, and whatever its host keeps outside it
 * (notes, a record of what it handed out). No summary is written and no
 * model is asked. The earlier threads stay on disk, and `read_own_chat`
 * searches them. What the person sees is the host's own record, which this
 * does not touch.
 *
 * The limits and the briefing follow Aegis's main bot
 * (coworker: src/electron/libs/main-bot); searching the earlier threads
 * follows DS Bot (Fei2411/DS-Bot: read_own_chat). Design:
 * docs/long-conversation-context-design.md.
 */

import { existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";

import { getBubbleHome } from "../bubble-home.js";
import { AUTOCOMPACT_BUFFER_TOKENS, MIN_WINDOW_FOR_RESERVE, OUTPUT_RESERVE_TOKENS } from "../context/budget.js";
import { isCompactionSummaryMessage, messageText } from "../context/compact.js";
import type { ContentPart, Message } from "../types.js";

export const CONVERSATION_LIMITS = {
  /**
   * How full the context may get before the agent starts on an empty thread
   * by itself (`conversationLimit`): this many tokens, never more than this
   * share of the model's window, and never more than this share of where
   * the session would compact on its own. So what the agent loses is decided
   * here, not by a summary; and the cap keeps a model with a very large
   * window from rereading all of it each turn.
   */
  contextTokens: 500_000,
  contextShare: 0.6,
  contextBelowCompaction: 0.8,
  /** A limit lower than this leaves an empty thread no room: such a window is left to the session's own compaction. */
  contextFloor: 16_000,
  /** The share of the limit the context has to have grown by since the thread started empty, or starting over frees nothing. */
  contextGrown: 0.25,
  /** Turns answered after a fresh context before another may start by itself. */
  turnsBetweenFresh: 5,
  /** Turns of the thread left behind that the briefing quotes, and the characters of them and of one message. */
  recapTurns: 6,
  recap: 10_000,
  recapAsked: 500,
  recapReply: 1_200,
  /** The agent's last answer, which the person may be replying to. */
  recapLatest: 4_000,
  /** Messages one search answers with, and the characters of each. */
  found: 8,
  foundText: 600,
  /** Characters of the message an anchor names, and how many messages on each side of it are shown. */
  aroundText: 4_000,
  aroundSpan: 2,
} as const;

export interface ConversationLimit {
  /** Never more than this many tokens. */
  tokens?: number;
  /** Never more than this share of the model's window. */
  share?: number;
  /** Never more than this share of where the session compacts on its own. */
  belowCompaction?: number;
}

/** What a turn of such a conversation carries. */
export interface ConversationTurn {
  /** What the host keeps outside the conversation, for an agent on an empty thread. Used only when a briefing is due. */
  brief?: string;
  /** How things stand now, read by the host from its own records. Goes with every message. */
  standing?: string;
  limit?: ConversationLimit;
  /** false: a fresh context starts only when someone asks. */
  automatic?: boolean;
}

export interface ConversationState {
  /** The sessions it ran on, oldest first; the last is the one in use. */
  threads: string[];
  /** Turns answered since the context last started afresh; null before the first time. */
  freshTurns: number | null;
  /** How full the context was when the thread in use had just started empty; "due" until the agent says. */
  baseline: number | "due" | null;
  /** A fresh context is due with the next turn: "auto" when the context filled up, else someone asked. */
  pending: "auto" | "manual" | null;
  /** The thread in use is empty and has yet to be told what went before. */
  brief: boolean;
  /** How full the context was when the agent last said. */
  usage: { used: number; window: number } | null;
}

/** Where a session compacts on its own (context/budget.ts, shouldTriggerCompact). */
function compactsAt(window: number): number {
  if (!(window > 0)) return 32_000;
  return window >= MIN_WINDOW_FOR_RESERVE
    ? window - OUTPUT_RESERVE_TOKENS - AUTOCOMPACT_BUFFER_TOKENS
    : Math.floor(window * 0.75);
}

/** How many tokens the context may hold before the agent starts on an empty thread by itself. */
export function conversationLimit(window: number, limit: ConversationLimit = {}): number {
  const tokens = positive(limit.tokens) ?? CONVERSATION_LIMITS.contextTokens;
  const share = fraction(limit.share) ?? CONVERSATION_LIMITS.contextShare;
  const below = fraction(limit.belowCompaction) ?? CONVERSATION_LIMITS.contextBelowCompaction;
  const ofWindow = window > 0 ? Math.floor(window * share) : tokens;
  // The session decides on an estimate that runs higher than what the agent reports, and within a turn: stay clear of it.
  return Math.min(tokens, ofWindow, Math.floor(compactsAt(window) * below));
}

function positive(value: unknown): number | undefined {
  return typeof value === "number" && Number.isFinite(value) && value > 0 ? value : undefined;
}

function fraction(value: unknown): number | undefined {
  return typeof value === "number" && value > 0 && value <= 1 ? value : undefined;
}

const fresh = (sessionId: string): ConversationState => ({
  threads: [sessionId],
  freshTurns: null,
  baseline: null,
  pending: null,
  brief: false,
  usage: null,
});

/**
 * The conversations of one Bubble home, by the id of their first thread:
 * the id the host goes on using for the whole conversation.
 */
export class Conversations {
  private readonly dir: string;
  private readonly cache = new Map<string, ConversationState>();

  constructor(dir: string = join(getBubbleHome(), "conversations")) {
    this.dir = dir;
  }

  private file(sessionId: string): string {
    return join(this.dir, `${encodeURIComponent(sessionId)}.json`);
  }

  private load(sessionId: string): ConversationState {
    const cached = this.cache.get(sessionId);
    if (cached) return cached;
    let state = fresh(sessionId);
    try {
      const read = JSON.parse(readFileSync(this.file(sessionId), "utf8")) as Partial<ConversationState>;
      if (Array.isArray(read.threads) && read.threads.length > 0 && read.threads[0] === sessionId) {
        state = {
          threads: read.threads.filter((thread): thread is string => typeof thread === "string"),
          freshTurns: typeof read.freshTurns === "number" ? read.freshTurns : null,
          baseline: read.baseline === "due" || typeof read.baseline === "number" ? read.baseline : null,
          pending: read.pending === "auto" || read.pending === "manual" ? read.pending : null,
          brief: read.brief === true,
          usage: read.usage && typeof read.usage.used === "number" ? { used: read.usage.used, window: Number(read.usage.window) || 0 } : null,
        };
      }
    } catch {
      // Not kept yet, or unreadable: a conversation on its first thread.
    }
    this.cache.set(sessionId, state);
    return state;
  }

  private save(sessionId: string, state: ConversationState): void {
    this.cache.set(sessionId, state);
    mkdirSync(this.dir, { recursive: true });
    writeFileSync(this.file(sessionId), `${JSON.stringify(state, null, 2)}\n`);
  }

  state(sessionId: string): ConversationState {
    return structuredClone(this.load(sessionId));
  }

  /** The session a turn of this conversation runs on now. */
  thread(sessionId: string): string {
    const threads = this.load(sessionId).threads;
    return threads[threads.length - 1];
  }

  /** Every session it ran on, oldest first. */
  threads(sessionId: string): string[] {
    return [...this.load(sessionId).threads];
  }

  /**
   * A turn is about to be sent. A fresh context that is due starts here:
   * `open` makes the session of the new thread. Says which session the turn
   * runs on, whether the thread was just started (and why), and whether the
   * agent has yet to be told what went before.
   */
  beforeTurn(sessionId: string, open: (thread: string) => void): { thread: string; started: "auto" | "manual" | null; brief: boolean } {
    const state = this.load(sessionId);
    const started = state.pending;
    if (started !== null) this.start(sessionId, open);
    const now = this.load(sessionId);
    return { thread: now.threads[now.threads.length - 1], started, brief: now.brief };
  }

  /** Puts the agent on an empty thread. Only between turns. */
  private start(sessionId: string, open: (thread: string) => void): string {
    const state = this.load(sessionId);
    const thread = `${sessionId}.t${state.threads.length + 1}`;
    open(thread);
    this.save(sessionId, {
      threads: [...state.threads, thread],
      freshTurns: 0,
      baseline: "due",
      pending: null,
      // Kept until a turn has gone through with it: one that fails may have told the agent nothing.
      brief: true,
      usage: null,
    });
    return thread;
  }

  /**
   * Someone asked for a fresh context: at once when nothing runs (`open`
   * makes the new session; the new thread's id comes back), else with the
   * next turn. `empty`: the thread in use has nothing in it, so there is
   * nothing to leave behind.
   */
  requestFresh(sessionId: string, at: { busy: boolean; empty: boolean }, open: (thread: string) => void): { outcome: "done" | "pending"; thread?: string } {
    if (at.empty) return { outcome: "done" };
    if (at.busy) {
      this.save(sessionId, { ...this.load(sessionId), pending: "manual" });
      return { outcome: "pending" };
    }
    return { outcome: "done", thread: this.start(sessionId, open) };
  }

  /** The agent said how full its context is. */
  noteUsage(sessionId: string, used: number, window: number | undefined): void {
    if (!Number.isFinite(used) || used < 0) return;
    const state = this.load(sessionId);
    state.usage = { used, window: typeof window === "number" && window > 0 ? window : 0 };
    if (state.baseline === "due" && used > 0) state.baseline = Math.round(used);
  }

  /**
   * A turn ended. With the context over its limit, a fresh one is due with
   * the next turn: before the session's own compaction decides what is
   * lost. Not again until a few turns were answered and the context has
   * grown since, so that one an empty thread already fills cannot keep it
   * starting over.
   */
  afterTurn(sessionId: string, turn: { completed: boolean; briefed: boolean; limit?: ConversationLimit; automatic?: boolean }): void {
    const state = { ...this.load(sessionId) };
    if (turn.completed) {
      if (turn.briefed) state.brief = false;
      if (state.freshTurns !== null) state.freshTurns += 1;
    }
    if (state.pending !== "manual") {
      const usage = state.usage;
      const turns = state.freshTurns ?? Number.POSITIVE_INFINITY;
      const limit = usage ? conversationLimit(usage.window, turn.limit) : 0;
      const baseline = typeof state.baseline === "number" ? state.baseline : 0;
      const full =
        turn.automatic !== false &&
        usage !== null &&
        turns >= CONVERSATION_LIMITS.turnsBetweenFresh &&
        limit >= CONVERSATION_LIMITS.contextFloor &&
        usage.used >= limit &&
        usage.used - baseline >= limit * CONVERSATION_LIMITS.contextGrown;
      // Due when an earlier turn ended, and no longer: the session has made room since.
      state.pending = full ? "auto" : null;
    }
    this.save(sessionId, state);
  }

  /** The conversation is gone; its sessions are the caller's to delete. */
  forget(sessionId: string): void {
    this.cache.delete(sessionId);
    rmSync(this.file(sessionId), { force: true });
  }

  /** Whether anything was ever kept of it: one that never left its first thread and never ran as a conversation has nothing. */
  known(sessionId: string): boolean {
    return this.cache.has(sessionId) ? true : existsSync(this.file(sessionId));
  }
}

// ── What a message of such a conversation looks like to the model ──────────

const BRIEF_OPEN = "<conversation_brief>";
const BRIEF_CLOSE = "</conversation_brief>";
const STANDING_OPEN = "<conversation_standing>";
const STANDING_CLOSE = "</conversation_standing>";

/** A message as it was written, without what was put around it for the model. */
export function withoutConversationBlocks(text: string): string {
  let rest = text;
  for (const [open, close] of [
    [BRIEF_OPEN, BRIEF_CLOSE],
    [STANDING_OPEN, STANDING_CLOSE],
  ]) {
    for (;;) {
      const from = rest.indexOf(open);
      if (from === -1) break;
      const to = rest.indexOf(close, from);
      rest = rest.slice(0, from) + (to === -1 ? "" : rest.slice(to + close.length));
    }
  }
  return rest.trim();
}

/**
 * The message the model reads: the briefing first when one is due, then
 * what was sent, then how things stand. The standing goes last and not into
 * the system prompt: a system prompt that changes with every turn would
 * cost the whole thread its cached prefix.
 */
export function conversationPrompt(prompt: string | ContentPart[], around: { brief?: string; standing?: string }): string | ContentPart[] {
  const before = around.brief?.trim() ? `${BRIEF_OPEN}\n${around.brief.trim()}\n${BRIEF_CLOSE}\n\n` : "";
  const after = around.standing?.trim() ? `\n\n${STANDING_OPEN}\n${around.standing.trim()}\n${STANDING_CLOSE}` : "";
  if (!before && !after) return prompt;
  if (typeof prompt === "string") return `${before}${prompt}${after}`;
  return [
    ...(before ? [{ type: "text" as const, text: before.trimEnd() }] : []),
    ...prompt,
    ...(after ? [{ type: "text" as const, text: after.trimStart() }] : []),
  ];
}

function oneLine(text: string, max: number): string {
  const line = text.replace(/\s+/g, " ").trim();
  return line.length > max ? `${line.slice(0, max - 1)}…` : line;
}

/** One line that keeps the start and the end of a long text: what the agent asked tends to come last. */
function bothEnds(text: string, max: number): string {
  const line = text.replace(/\s+/g, " ").trim();
  if (line.length <= max) return line;
  const half = Math.floor((max - 3) / 2);
  return `${line.slice(0, half)} … ${line.slice(-half)}`;
}

interface Said {
  who: "user" | "assistant";
  text: string;
}

/** What was said in a thread, in order: the messages to the agent and its answers, without tool calls and their results. */
function saidIn(messages: Message[]): Said[] {
  const said: Said[] = [];
  for (const message of messages) {
    if (message.role !== "user" && message.role !== "assistant") continue;
    if (isCompactionSummaryMessage(message)) continue;
    const text = message.role === "user" ? withoutConversationBlocks(messageText(message)) : messageText(message).trim();
    if (text) said.push({ who: message.role, text });
  }
  return said;
}

interface AnsweredTurn {
  asked: string;
  /** Where the message stands among what was said in its thread, counting from 1: `read_own_chat` names it so. */
  askedAt: number;
  reply: string;
  replyAt: number;
}

/** The turns of a thread that were answered, oldest first: what was sent and the last thing the agent said to it. */
function answeredTurns(messages: Message[]): AnsweredTurn[] {
  const turns: AnsweredTurn[] = [];
  let open: { asked: string; askedAt: number } | null = null;
  let reply: { text: string; at: number } | null = null;
  const close = () => {
    if (open && reply) turns.push({ ...open, reply: reply.text, replyAt: reply.at });
  };
  for (const [index, line] of saidIn(messages).entries()) {
    if (line.who === "user") {
      close();
      open = { asked: line.text, askedAt: index + 1 };
      reply = null;
    } else {
      reply = { text: line.text, at: index + 1 };
    }
  }
  close();
  return turns;
}

/** After a quote that was cut: where the whole of it is. */
function cutNote(whole: string, quoted: string, part: number, at: number): string {
  return whole.replace(/\s+/g, " ").trim().length > quoted.length ? ` (cut short; read_own_chat with around "part ${part} #${at}" shows all of it)` : "";
}

/**
 * For an agent on an empty thread: what the conversation it no longer has
 * was about. `left` is the thread left behind and `part` which thread of
 * the conversation it was; `kept` is what the host keeps outside the
 * conversation, in its own words.
 */
export function conversationBrief(left: Message[], kept?: string, part = 1): string {
  const lines = [
    "Your context starts afresh here. This conversation went on before, and the person still sees all of it, but you no longer have it. Everything said in it is kept: read_own_chat searches it. When the person refers to something that is neither below nor found there, ask them rather than guess.",
  ];
  // The latest turns first, as far as there is room: the person may be answering what was just asked.
  const told: string[] = [];
  let room: number = CONVERSATION_LIMITS.recap;
  const turns = answeredTurns(left).slice(-CONVERSATION_LIMITS.recapTurns).reverse();
  for (const [index, turn] of turns.entries()) {
    const asked = oneLine(turn.asked, CONVERSATION_LIMITS.recapAsked);
    const reply = bothEnds(turn.reply, index === 0 ? CONVERSATION_LIMITS.recapLatest : CONVERSATION_LIMITS.recapReply);
    // A quote that was cut says where the rest is: without that the agent takes the cut for all there was.
    const line =
      `- You were sent: "${asked}"${cutNote(turn.asked, asked, part, turn.askedAt)} ` +
      `You answered: "${reply}"${cutNote(turn.reply, reply, part, turn.replyAt)}`;
    if (line.length > room) break;
    room -= line.length;
    told.unshift(line);
  }
  if (told.length) lines.push("", "The end of the conversation, oldest first:", ...told);
  if (kept?.trim()) lines.push("", kept.trim());
  return lines.join("\n");
}

// ── Searching the conversation ─────────────────────────────────────────────

export const READ_OWN_CHAT_TOOL = "read_own_chat";

export const READ_OWN_CHAT_DESCRIPTION =
  "Search everything said in this conversation, also the earlier parts you no longer have in your context. " +
  "With query: the messages that hold those words, newest first, each with an anchor such as [part 2 #14]. " +
  "With around: the message an anchor names, in full, with the ones before and after it.";

export const READ_OWN_CHAT_PARAMETERS = {
  type: "object" as const,
  properties: {
    query: { type: "string", description: "Words to look for; a message has to hold all of them" },
    around: { type: "string", description: 'An anchor from an earlier search, such as "part 2 #14"' },
  },
};

interface OwnLine extends Said {
  part: number;
  seq: number;
}

function ownLines(threads: Message[][]): OwnLine[] {
  return threads.flatMap((messages, at) => saidIn(messages).map((line, index) => ({ ...line, part: at + 1, seq: index + 1 })));
}

function shown(line: OwnLine, text: string): string {
  return `[part ${line.part} #${line.seq}] ${line.who === "user" ? "To you" : "You"}: ${text}`;
}

/** A long message cut to the stretch around the first word found. */
function near(text: string, terms: string[], max: number): string {
  const flat = text.replace(/\s+/g, " ").trim();
  if (flat.length <= max) return flat;
  const lower = flat.toLowerCase();
  const at = Math.max(0, Math.min(...terms.map((term) => lower.indexOf(term)).filter((index) => index >= 0), flat.length));
  const from = Math.max(0, Math.min(at - Math.floor(max / 3), flat.length - max));
  return `${from > 0 ? "…" : ""}${flat.slice(from, from + max)}${from + max < flat.length ? "…" : ""}`;
}

/**
 * What `read_own_chat` answers. `threads` is the conversation, each thread's
 * messages in order, the oldest thread first.
 */
export function readOwnChat(threads: Message[][], args: { query?: unknown; around?: unknown }): { content: string; isError?: boolean } {
  const lines = ownLines(threads);
  const around = typeof args.around === "string" ? args.around.trim() : "";
  if (around) {
    const anchor = /(\d+)\D+(\d+)/.exec(around);
    const at = anchor ? lines.findIndex((line) => line.part === Number(anchor[1]) && line.seq === Number(anchor[2])) : -1;
    if (at === -1) return { content: `There is no message at "${around}". An anchor looks like "part 2 #14"; a search with query gives them.`, isError: true };
    const span = CONVERSATION_LIMITS.aroundSpan;
    return {
      content: lines
        .slice(Math.max(0, at - span), at + span + 1)
        .map((line) => shown(line, line === lines[at] ? bothEnds(line.text, CONVERSATION_LIMITS.aroundText) : oneLine(line.text, CONVERSATION_LIMITS.foundText)))
        .join("\n\n"),
    };
  }
  const query = typeof args.query === "string" ? args.query.trim().toLowerCase() : "";
  if (!query) return { content: "Give query (words to look for) or around (an anchor).", isError: true };
  const terms = query.split(/\s+/).filter(Boolean);
  const found = lines.filter((line) => {
    const text = line.text.toLowerCase();
    return terms.every((term) => text.includes(term));
  });
  if (found.length === 0) {
    return { content: `Nothing in this conversation holds "${query}". Try fewer or other words; if it is not here, ask the person.` };
  }
  const newest = found.slice(-CONVERSATION_LIMITS.found).reverse();
  return {
    content: [
      ...newest.map((line) => shown(line, near(line.text, terms, CONVERSATION_LIMITS.foundText))),
      ...(found.length > newest.length ? [`(${found.length - newest.length} older messages hold it too; narrow the words to see them.)`] : []),
    ].join("\n\n"),
  };
}
