import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import type { Message } from "../../types.js";
import {
  CONVERSATION_LIMITS,
  Conversations,
  conversationBrief,
  conversationLimit,
  conversationPrompt,
  readOwnChat,
  withoutConversationBlocks,
} from "../conversation.js";

const user = (content: string): Message => ({ role: "user", content });
const assistant = (content: string): Message => ({ role: "assistant", content });

describe("how full a long conversation may get", () => {
  it("is capped in tokens on a very large window", () => {
    expect(conversationLimit(1_048_576)).toBe(500_000);
  });

  it("follows the window when that is smaller", () => {
    // 60% of the window, which is under 80% of where the session compacts (167k).
    expect(conversationLimit(200_000)).toBe(120_000);
    // Under 40k the session compacts at three quarters; 80% of that is 60% of the window.
    expect(conversationLimit(30_000)).toBe(18_000);
  });

  it("stays under the session's own compaction when the window is not known", () => {
    expect(conversationLimit(0)).toBe(25_600);
  });

  it("takes the host's own figures", () => {
    expect(conversationLimit(1_048_576, { tokens: 100_000 })).toBe(100_000);
    expect(conversationLimit(200_000, { share: 0.5 })).toBe(100_000);
  });
});

describe("the threads of a long conversation", () => {
  let dir: string;
  let made: string[];
  const open = (thread: string) => void made.push(thread);

  beforeEach(() => {
    dir = mkdtempSync(join(tmpdir(), "bubble-conversations-"));
    made = [];
  });
  afterEach(() => rmSync(dir, { recursive: true, force: true }));

  /** Turns that end with the context at `used` of a million-token window. */
  function answer(all: Conversations, id: string, used: number, turns = 1) {
    for (let i = 0; i < turns; i++) {
      const entered = all.beforeTurn(id, open);
      all.noteUsage(id, used, 1_048_576);
      all.afterTurn(id, { completed: true, briefed: entered.brief });
    }
  }

  it("runs on its first session until the context is over the limit", () => {
    const all = new Conversations(dir);
    expect(all.known("main")).toBe(false);
    answer(all, "main", 400_000, 3);
    expect(all.threads("main")).toEqual(["main"]);
    expect(all.state("main").pending).toBeNull();

    answer(all, "main", 510_000);
    expect(all.state("main").pending).toBe("auto");
    // Nothing moves until the next turn is sent.
    expect(all.thread("main")).toBe("main");

    const entered = all.beforeTurn("main", open);
    expect(entered).toEqual({ thread: "main.t2", started: "auto", brief: true });
    expect(made).toEqual(["main.t2"]);
    expect(all.threads("main")).toEqual(["main", "main.t2"]);
  });

  it("is kept across a restart", () => {
    answer(new Conversations(dir), "main", 510_000);
    const again = new Conversations(dir);
    expect(again.known("main")).toBe(true);
    expect(again.beforeTurn("main", open).thread).toBe("main.t2");
    expect(new Conversations(dir).thread("main")).toBe("main.t2");
  });

  it("goes on briefing until a turn has gone through with it", () => {
    const all = new Conversations(dir);
    answer(all, "main", 510_000);
    expect(all.beforeTurn("main", open).brief).toBe(true);
    all.afterTurn("main", { completed: false, briefed: true });
    expect(all.beforeTurn("main", open)).toMatchObject({ thread: "main.t2", started: null, brief: true });
    all.afterTurn("main", { completed: true, briefed: true });
    expect(all.beforeTurn("main", open).brief).toBe(false);
  });

  it("does not start over again at once, nor before the context has grown", () => {
    const all = new Conversations(dir);
    answer(all, "main", 510_000);
    // An empty thread that is over the limit already: its notes and tools fill it.
    answer(all, "main", 505_000, 6);
    expect(all.threads("main")).toEqual(["main", "main.t2"]);
    expect(all.state("main")).toMatchObject({ baseline: 505_000, freshTurns: 6, pending: null });

    // Grown by a quarter of the limit over where it started: now it is due again.
    answer(all, "main", 640_000);
    expect(all.state("main").pending).toBe("auto");
  });

  it("waits a few turns after a fresh context", () => {
    const all = new Conversations(dir);
    answer(all, "main", 510_000);
    all.beforeTurn("main", open);
    all.noteUsage("main", 10_000, 1_048_576);
    all.afterTurn("main", { completed: true, briefed: true });
    answer(all, "main", 700_000, CONVERSATION_LIMITS.turnsBetweenFresh - 2);
    expect(all.state("main").pending).toBeNull();
    answer(all, "main", 700_000);
    expect(all.state("main").pending).toBe("auto");
  });

  it("leaves a window with no room for an empty thread to the session", () => {
    const all = new Conversations(dir);
    all.beforeTurn("small", open);
    all.noteUsage("small", 15_000, 16_000);
    all.afterTurn("small", { completed: true, briefed: false });
    expect(all.state("small").pending).toBeNull();
  });

  it("is no longer due once the session made room itself", () => {
    const all = new Conversations(dir);
    answer(all, "main", 510_000);
    expect(all.state("main").pending).toBe("auto");
    // A turn that joined the running one ended with the context compacted.
    all.noteUsage("main", 90_000, 1_048_576);
    all.afterTurn("main", { completed: true, briefed: false });
    expect(all.state("main").pending).toBeNull();
  });

  it("starts afresh when someone asks: now, or with the next turn when one runs", () => {
    const all = new Conversations(dir);
    expect(all.requestFresh("main", { busy: false, empty: true }, open)).toEqual({ outcome: "done" });
    expect(made).toEqual([]);

    expect(all.requestFresh("main", { busy: false, empty: false }, open)).toEqual({ outcome: "done", thread: "main.t2" });
    expect(all.state("main")).toMatchObject({ brief: true, freshTurns: 0, baseline: "due" });

    expect(all.requestFresh("main", { busy: true, empty: false }, open)).toEqual({ outcome: "pending" });
    // The running turn's end does not take the person's request away.
    all.noteUsage("main", 1_000, 1_048_576);
    all.afterTurn("main", { completed: true, briefed: true });
    expect(all.beforeTurn("main", open)).toMatchObject({ thread: "main.t3", started: "manual" });
  });

  it("does not start afresh by itself when the host said not to", () => {
    const all = new Conversations(dir);
    all.beforeTurn("main", open);
    all.noteUsage("main", 900_000, 1_048_576);
    all.afterTurn("main", { completed: true, briefed: false, automatic: false });
    expect(all.state("main").pending).toBeNull();
  });

  it("forgets a conversation that was deleted", () => {
    const all = new Conversations(dir);
    answer(all, "main", 510_000);
    all.forget("main");
    expect(all.known("main")).toBe(false);
    expect(new Conversations(dir).threads("main")).toEqual(["main"]);
  });
});

describe("what a message of a long conversation carries", () => {
  it("puts the briefing first and the standing last, and takes both off again", () => {
    const sent = conversationPrompt("ship it", { brief: "Before: nothing.", standing: "Dev is working." }) as string;
    expect(sent.indexOf("<conversation_brief>")).toBe(0);
    expect(sent.indexOf("ship it")).toBeLessThan(sent.indexOf("<conversation_standing>"));
    expect(withoutConversationBlocks(sent)).toBe("ship it");
    expect(conversationPrompt("ship it", {})).toBe("ship it");
  });

  it("keeps pictures where they were", () => {
    const sent = conversationPrompt([{ type: "text", text: "look" }, { type: "image_url", image_url: { url: "data:x" } }], { standing: "Idle." });
    expect(sent).toHaveLength(3);
    expect((sent as any[])[2].text).toContain("Idle.");
  });

  it("tells an agent on an empty thread how the conversation ended and what the host kept", () => {
    const left: Message[] = [
      user("first thing"),
      assistant("did the first thing"),
      user("second thing\n\n<conversation_standing>\nDev is working.\n</conversation_standing>"),
      { role: "assistant", content: "", toolCalls: [{ id: "c", name: "list_bots", arguments: "{}" }] } as Message,
      { role: "tool", toolCallId: "c", content: "[]" },
      assistant("did the second thing. Which folder?"),
    ];
    const brief = conversationBrief(left, "Reports of what you handed out:\n- Dev finished the changelog.");
    expect(brief).toContain("Your context starts afresh here");
    expect(brief).toContain("read_own_chat");
    expect(brief).toContain('- You were sent: "first thing" You answered: "did the first thing"');
    expect(brief).toContain('- You were sent: "second thing" You answered: "did the second thing. Which folder?"');
    expect(brief).not.toContain("Dev is working.");
    expect(brief.indexOf("first thing")).toBeLessThan(brief.indexOf("second thing"));
    expect(brief.trimEnd().endsWith("- Dev finished the changelog.")).toBe(true);
  });

  it("quotes only the last turns, the latest answer at greater length", () => {
    const left: Message[] = [];
    for (let i = 1; i <= 9; i++) left.push(user(`ask ${i}`), assistant(`answer ${i} ${"x".repeat(3_000)} end ${i}`));
    const brief = conversationBrief(left, undefined, 2);
    expect(brief).not.toContain('"ask 3"');
    expect(brief).toContain('"ask 9"');
    // The start and the end of a long answer are both there.
    expect(brief).toContain("answer 8 ");
    expect(brief).toContain("end 8");
    // A quote that was cut says where the whole of it is; one that was not says nothing.
    expect(brief).toContain('You were sent: "ask 8" You answered: "answer 8 ');
    expect(brief).toMatch(/end 8" \(cut short; read_own_chat with around "part 2 #16" shows all of it\)/);
    const latest = brief.split("\n").find((line) => line.includes('"ask 9"'))!;
    const earlier = brief.split("\n").find((line) => line.includes('"ask 8"'))!;
    expect(latest.length).toBeGreaterThan(earlier.length * 2);
    expect(brief.length).toBeLessThan(CONVERSATION_LIMITS.recap + 1_000);
    expect(conversationBrief([user("short"), assistant("fine")])).not.toContain("cut short");
  });

  it("says only that the context is new when nothing was said before", () => {
    expect(conversationBrief([])).not.toContain("The end of the conversation");
  });
});

describe("read_own_chat", () => {
  const threads: Message[][] = [
    [user("The staging key is in 1Password under Bubble Staging"), assistant("Noted, I will not ask again."), user("发布说明用中文写"), assistant("好的，发布说明以后用中文。")],
    [user("what did I say about the key?"), assistant("Let me look.")],
  ];

  it("finds a message in a thread left behind, newest first, with its anchor", () => {
    const found = readOwnChat(threads, { query: "key" }).content;
    expect(found.indexOf("[part 2 #1]")).toBeLessThan(found.indexOf("[part 1 #1]"));
    expect(found).toContain("[part 1 #1] To you: The staging key is in 1Password");
  });

  it("wants every word, and finds text that has no spaces between words", () => {
    expect(readOwnChat(threads, { query: "staging 1password" }).content).toContain("[part 1 #1]");
    expect(readOwnChat(threads, { query: "staging lastpass" }).content).toContain("Nothing in this conversation");
    expect(readOwnChat(threads, { query: "发布说明" }).content).toContain("[part 1 #4] You: 好的");
  });

  it("shows a message in full with its neighbours", () => {
    const shown = readOwnChat(threads, { around: "part 1 #2" }).content;
    expect(shown).toContain("[part 1 #1]");
    expect(shown).toContain("[part 1 #2] You: Noted, I will not ask again.");
    expect(shown).toContain("[part 1 #4]");
    expect(shown).not.toContain("[part 2 #1]");
  });

  it("says so when there is nothing to do", () => {
    expect(readOwnChat(threads, {}).isError).toBe(true);
    expect(readOwnChat(threads, { around: "part 9 #9" }).isError).toBe(true);
  });
});
