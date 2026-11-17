import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { PassThrough } from "node:stream";
import { createInterface } from "node:readline";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { BubbleSdk, type Message, type Provider } from "../../sdk/index.js";
import { HostServer } from "../server.js";

type Step =
  | { text: string }
  | { tool: { id: string; name: string; args: Record<string, unknown> } };

/** A model that plays `steps` in order and records what each request held. */
function scripted(steps: Step[]) {
  const seen: Array<{ messages: Message[]; tools: string[] }> = [];
  const provider: Provider = {
    async *streamChat(messages, options) {
      seen.push({ messages: structuredClone(messages), tools: (options.tools ?? []).map((tool) => tool.name) });
      const step = steps[seen.length - 1] ?? { text: "nothing left" };
      if ("text" in step) yield { type: "text", content: step.text };
      else {
        yield {
          type: "tool_call",
          id: step.tool.id,
          name: step.tool.name,
          arguments: JSON.stringify(step.tool.args),
          isStart: true,
          isEnd: true,
        };
      }
      yield { type: "done" };
    },
    async complete() {
      return "a title";
    },
  };
  return { provider, seen };
}

/** The app's side of the pipe. */
class App {
  readonly toHost = new PassThrough();
  readonly fromHost = new PassThrough();
  readonly notifications: Array<{ method: string; params: any }> = [];
  /** How the app answers the host's requests, by method. */
  readonly answers = new Map<string, (params: any, id: string) => unknown | Promise<unknown>>();
  readonly asked: Array<{ id: string; method: string; params: any }> = [];
  private readonly waiting = new Map<number, { resolve(value: any): void; reject(error: Error): void }>();
  private readonly watchers = new Set<() => void>();
  private nextId = 0;

  constructor() {
    createInterface({ input: this.fromHost }).on("line", (line) => {
      const message = JSON.parse(line);
      if (typeof message.method !== "string") {
        const pending = this.waiting.get(message.id);
        this.waiting.delete(message.id);
        if (message.error) pending?.reject(new Error(message.error.message));
        else pending?.resolve(message.result);
      } else if (message.id !== undefined) {
        this.asked.push(message);
        const answer = this.answers.get(message.method);
        if (answer) {
          void Promise.resolve(answer(message.params, message.id)).then((result) => {
            if (result !== undefined) this.write({ jsonrpc: "2.0", id: message.id, result });
          });
        }
      } else {
        this.notifications.push(message);
      }
      for (const watcher of this.watchers) watcher();
    });
  }

  write(message: unknown) {
    this.toHost.write(`${JSON.stringify(message)}\n`);
  }

  call<T = any>(method: string, params: unknown = {}): Promise<T> {
    const id = ++this.nextId;
    return new Promise<T>((resolve, reject) => {
      this.waiting.set(id, { resolve, reject });
      this.write({ jsonrpc: "2.0", id, method, params });
    });
  }

  /** Resolves once `check` holds, looking again after every message. */
  until<T>(check: () => T | undefined | false, what: string): Promise<T> {
    return new Promise<T>((resolve, reject) => {
      const timer = setTimeout(() => {
        this.watchers.delete(look);
        reject(new Error(`timed out waiting for ${what}`));
      }, 5000);
      const look = () => {
        const found = check();
        if (!found) return;
        clearTimeout(timer);
        this.watchers.delete(look);
        resolve(found);
      };
      this.watchers.add(look);
      look();
    });
  }

  events(sessionId: string) {
    return this.notifications
      .filter((n) => n.method === "session/event" && n.params.sessionId === sessionId)
      .map((n) => n.params);
  }

  terminal(sessionId: string) {
    return this.until(() => this.events(sessionId).find((e) => e.terminal)?.terminal, "the turn to end");
  }
}

describe("bubble host", () => {
  let home: string;
  let project: string;
  let app: App;
  let running: Promise<void>;

  function start(provider: Provider, model = "test:model") {
    const sdk = new BubbleSdk({ defaultCwd: project, mcp: false });
    (sdk as any).resolveProvider = () => ({ provider, providerId: model.split(":")[0], model });
    app = new App();
    running = new HostServer({ input: app.toHost, output: app.fromHost, sdk, bubbleVersion: "9.9.9" }).run();
  }

  beforeEach(() => {
    home = mkdtempSync(join(tmpdir(), "bubble-host-home-"));
    project = mkdtempSync(join(tmpdir(), "bubble-host-project-"));
    vi.stubEnv("BUBBLE_HOME", home);
  });

  afterEach(async () => {
    await app.call("shutdown");
    await running;
    vi.unstubAllEnvs();
    rmSync(home, { recursive: true, force: true });
    rmSync(project, { recursive: true, force: true });
  });

  it("runs a turn: a command the person allows, a tool the app runs, then the reply", async () => {
    const { provider, seen } = scripted([
      { tool: { id: "call-1", name: "bash", args: { command: "printf hi > out.txt" } } },
      { tool: { id: "call-2", name: "note", args: { text: "remember" } } },
      { text: "All done." },
    ]);
    start(provider);
    app.answers.set("approval/request", () => ({ action: "approve" }));
    app.answers.set("tool/call", (params) => ({ content: `noted: ${params.args.text}` }));

    expect(await app.call("initialize")).toMatchObject({ protocolVersion: 2, bubbleVersion: "9.9.9", home });
    const session = await app.call("session/create", { cwd: project });
    await app.call("turn/start", {
      sessionId: session.id,
      prompt: "write the file",
      hostTools: [{ name: "note", description: "Keep a note", parameters: { type: "object", properties: {} }, readOnly: true }],
    });
    expect(await app.terminal(session.id)).toEqual({ kind: "completed" });

    // The yes was asked for the command, tied to its tool call.
    const approval = app.asked.find((request) => request.method === "approval/request")!;
    expect(approval.params).toMatchObject({
      sessionId: session.id,
      toolCallId: "call-1",
      request: { type: "bash", command: "printf hi > out.txt" },
    });
    expect(readFileSync(join(project, "out.txt"), "utf8")).toBe("hi");

    // The app's tool ran in the app, and the model read its result.
    expect(app.asked.find((request) => request.method === "tool/call")!.params).toMatchObject({
      toolCallId: "call-2",
      name: "note",
      args: { text: "remember" },
    });
    expect(seen[2].messages.some((m) => m.role === "tool" && m.content === "noted: remember")).toBe(true);

    const types = app.events(session.id).map((e) => e.event.type);
    expect(types).toContain("tool_start");
    expect(types).toContain("tool_end");
    expect(app.events(session.id).some((e) => e.event.type === "text_delta" && e.event.content === "All done.")).toBe(true);
    expect(app.notifications.find((n) => n.method === "turn/started")!.params).toMatchObject({
      sessionId: session.id,
      model: "test:model",
    });

    // The conversation is Bubble's own session: history comes back from it.
    const { messages } = await app.call("session/history", { sessionId: session.id });
    expect(messages.map((m: Message) => m.role)).toEqual(["user", "assistant", "tool", "assistant", "tool", "assistant"]);
  });

  it("refuses the command when the person says no", async () => {
    const { provider, seen } = scripted([
      { tool: { id: "call-1", name: "bash", args: { command: "printf hi > out.txt" } } },
      { text: "Left it alone." },
    ]);
    start(provider);
    app.answers.set("approval/request", () => ({ action: "reject", feedback: "not now" }));
    const session = await app.call("session/create", { cwd: project });
    await app.call("turn/start", { sessionId: session.id, prompt: "write the file" });
    expect(await app.terminal(session.id)).toEqual({ kind: "completed" });
    expect(existsSync(join(project, "out.txt"))).toBe(false);
    const result = seen[1].messages.find((m) => m.role === "tool");
    expect(result && "content" in result ? String(result.content) : "").toContain("not now");
  });

  it("withdraws a question nobody answered when the turn is stopped", async () => {
    const { provider } = scripted([{ tool: { id: "call-1", name: "bash", args: { command: "sleep 30" } } }]);
    start(provider);
    // No answer registered: the request stays open.
    const session = await app.call("session/create", { cwd: project });
    await app.call("turn/start", { sessionId: session.id, prompt: "wait" });
    const asked = await app.until(() => app.asked.find((request) => request.method === "approval/request"), "the question");
    expect((await app.call("turn/stop", { sessionId: session.id })).stopped).toBe(1);
    expect(await app.terminal(session.id)).toMatchObject({ kind: "cancelled" });
    await app.until(
      () => app.notifications.find((n) => n.method === "request/cancelled" && n.params.id === asked.id),
      "the question to be withdrawn",
    );
  });

  it("gives a host with its own role its prompt and only the tools it names", async () => {
    const { provider, seen } = scripted([{ text: "Here is the plan." }]);
    start(provider);
    const session = await app.call("session/create", { cwd: project });
    await app.call("turn/start", {
      sessionId: session.id,
      prompt: "plan the week",
      systemPrompt: "You coordinate tasks.",
      allowedTools: [],
      hostTools: [{ name: "create_task", description: "Start a task", parameters: { type: "object", properties: {} }, readOnly: true }],
    });
    expect(await app.terminal(session.id)).toEqual({ kind: "completed" });
    expect(seen[0].tools).toEqual(["create_task"]);
    const system = seen[0].messages.find((m) => m.role === "system");
    expect(system?.content).toBe("You coordinate tasks.");
  });

  it("calls the agent by the name the app gives it", async () => {
    const { provider, seen } = scripted([{ text: "On it." }]);
    start(provider);
    const session = await app.call("session/create", { cwd: project });
    await app.call("turn/start", { sessionId: session.id, prompt: "hello", agentName: "Docs" });
    expect(await app.terminal(session.id)).toEqual({ kind: "completed" });
    const system = String(seen[0].messages.find((m) => m.role === "system")?.content ?? "");
    expect(system.startsWith("You are Docs,")).toBe(true);
  });

  it("continues a session a later process opens by id", async () => {
    const first = scripted([{ text: "First answer." }]);
    start(first.provider);
    const session = await app.call("session/create", { cwd: project, id: "desk-run-1" });
    expect(session.id).toBe("desk-run-1");
    await app.call("turn/start", { sessionId: session.id, prompt: "one" });
    await app.terminal(session.id);
    await app.call("shutdown");
    await running;

    const second = scripted([{ text: "Second answer." }]);
    start(second.provider);
    await app.call("session/create", { cwd: project, id: "desk-run-1" });
    await app.call("turn/start", { sessionId: "desk-run-1", prompt: "two" });
    await app.terminal("desk-run-1");
    const contents = second.seen[0].messages.map((m) => ("content" in m ? m.content : ""));
    expect(contents).toContain("First answer.");
    expect(contents).toContain("two");
  });

  it("moves a long conversation on to an empty thread once its context is over the limit", async () => {
    const { provider, seen } = scripted([
      { text: "First answer." },
      { text: "Second answer." },
      { tool: { id: "call-1", name: "read_own_chat", args: { query: "harbour" } } },
      { text: "It is 4417." },
      { text: "Fourth answer." },
    ]);
    // A model whose window is known, and far larger than the limit below.
    start(provider, "google:gemini-4-carbon");
    const session = await app.call("session/create", { cwd: project, id: "desk-main" });
    const conversation = { limit: { tokens: 20_000 }, standing: "Dev is idle.", brief: "What you handed out: nothing yet." };
    const turn = async (prompt: string) => {
      const before = app.events(session.id).filter((e) => e.terminal).length;
      await app.call("turn/start", { sessionId: session.id, prompt, conversation });
      await app.until(() => app.events(session.id).filter((e) => e.terminal).length > before, "the turn to end");
    };
    const said = (request: { messages: Message[] }) => request.messages.map((m) => (typeof m.content === "string" ? m.content : "")).join("\n");
    const fresh = () => app.notifications.filter((n) => n.method === "conversation/fresh").map((n) => n.params);

    // The first turn fills the context past the limit; nothing moves yet.
    await turn(`The harbour code is 4417. ${"filler ".repeat(30_000)}`);
    expect(said(seen[0])).toContain("<conversation_standing>\nDev is idle.");
    expect(said(seen[0])).not.toContain("<conversation_brief>");
    expect(seen[0].tools).toContain("read_own_chat");
    expect(fresh()).toEqual([]);

    // The next one runs on an empty thread: told how the conversation ended and what the app kept, not sent the conversation.
    await turn("second");
    expect(fresh()).toEqual([{ sessionId: "desk-main", trigger: "auto", part: 2 }]);
    const second = said(seen[1]);
    expect(second).toContain("<conversation_brief>\nYour context starts afresh here");
    expect(second).toContain('You answered: "First answer."');
    expect(second).toContain("What you handed out: nothing yet.");
    expect(second).toContain("Dev is idle.");
    // Of the long first message only its start is quoted.
    const sent = seen[1].messages.filter((m) => m.role === "user" && String(m.content).includes("second")).map((m) => String(m.content));
    expect(sent).toHaveLength(1);
    expect(sent[0].length).toBeLessThan(2_000);
    // Reminders the agent's own runtime adds are not part of what was said.
    expect(sent[0]).not.toContain("bubble_internal_reminder");
    expect(seen[1].messages.some((m) => m.role === "assistant")).toBe(false);
    // The app still knows it by the one id: events, state and history are the thread's in use.
    expect(app.events("desk-main").filter((e) => e.terminal)).toHaveLength(2);
    expect(await app.call("session/state", { sessionId: "desk-main" })).toMatchObject({ active: false });
    const history = (await app.call("session/history", { sessionId: "desk-main" })).messages as Message[];
    expect(history.filter((m) => m.role === "assistant").map((m) => m.content)).toEqual(["Second answer."]);

    // What was said on the thread left behind can still be found, and the briefing is not given twice.
    await turn("what was the harbour code?");
    expect(said(seen[2])).not.toContain("<conversation_brief>\nYour context starts afresh here\n");
    expect(seen[2].messages.filter((m) => m.role === "user" && String(m.content).includes("<conversation_brief>"))).toHaveLength(1);
    const found = seen[3].messages.find((m) => m.role === "tool")!;
    expect(found.content).toContain("[part 1 #1] To you: The harbour code is 4417.");
    expect(fresh()).toHaveLength(1);

    // The person asks for a fresh context: at once, since nothing runs.
    expect(await app.call("conversation/fresh", { sessionId: "desk-main" })).toEqual({ outcome: "done", part: 3 });
    await turn("fourth");
    // The answer said so; no turn began on it unannounced.
    expect(fresh()).toHaveLength(1);
    expect(said(seen[4])).toContain('You were sent: "what was the harbour code?" You answered: "It is 4417."');
    expect(seen[4].messages.some((m) => m.role === "assistant")).toBe(false);

    // It goes with every thread it ran on.
    const mine = async () => ((await app.call("session/list")).sessions as Array<{ id?: string; name?: string; file?: string }>).filter((s) => JSON.stringify(s).includes("desk-main"));
    expect((await mine()).length).toBe(3);
    await app.call("session/delete", { sessionId: "desk-main" });
    expect(await mine()).toEqual([]);
    expect(existsSync(join(home, "conversations", "desk-main.json"))).toBe(false);
  });

  it("finds a long conversation's thread again after the host was restarted", async () => {
    const first = scripted([{ text: "First answer." }]);
    start(first.provider, "google:gemini-4-carbon");
    await app.call("session/create", { cwd: project, id: "desk-main" });
    await app.call("turn/start", { sessionId: "desk-main", prompt: "one", conversation: {} });
    await app.terminal("desk-main");
    // A fresh context nobody has written on yet: its session is not on disk.
    expect(await app.call("conversation/fresh", { sessionId: "desk-main" })).toEqual({ outcome: "done", part: 2 });
    await app.call("shutdown");
    await running;

    const second = scripted([{ text: "Second answer." }]);
    start(second.provider, "google:gemini-4-carbon");
    await app.call("session/create", { cwd: project, id: "desk-main" });
    expect(await app.call("session/state", { sessionId: "desk-main" })).toMatchObject({ active: false });
    await app.call("turn/start", { sessionId: "desk-main", prompt: "two", conversation: { brief: "Kept by the app." } });
    await app.terminal("desk-main");
    // Still on the empty thread, and told now what it was owed.
    const sent = second.seen[0].messages.filter((m) => m.role === "user").map((m) => String(m.content)).join("\n");
    expect(sent).toContain('You were sent: "one" You answered: "First answer."');
    expect(sent).toContain("Kept by the app.");
    expect(second.seen[0].messages.some((m) => m.role === "assistant")).toBe(false);
    expect(app.notifications.filter((n) => n.method === "conversation/fresh")).toEqual([]);
  });

  it("leaves a session alone that is not a long conversation", async () => {
    const { provider, seen } = scripted([{ text: "One." }, { text: "Two." }]);
    start(provider, "google:gemini-4-carbon");
    const session = await app.call("session/create", { cwd: project, id: "desk-plain" });
    await app.call("turn/start", { sessionId: session.id, prompt: `big ${"filler ".repeat(30_000)}` });
    await app.terminal(session.id);
    await app.call("turn/start", { sessionId: session.id, prompt: "again" });
    await app.until(() => app.events(session.id).filter((e) => e.terminal).length === 2, "the second turn to end");
    expect(seen[1].messages.filter((m) => m.role === "assistant").map((m) => m.content)).toEqual(["One."]);
    expect(seen[1].tools).not.toContain("read_own_chat");
    expect(existsSync(join(home, "conversations"))).toBe(false);
    // Nothing to leave behind on a session that was never a conversation's and holds nothing.
    await app.call("session/create", { cwd: project, id: "desk-empty" });
    expect(await app.call("conversation/fresh", { sessionId: "desk-empty" })).toEqual({ outcome: "done" });
    expect(app.notifications.filter((n) => n.method === "conversation/fresh")).toEqual([]);
  });

  it("lists skills and accounts, and names what it does not know", async () => {
    mkdirSync(join(home, "skills/notes-helper"), { recursive: true });
    writeFileSync(join(home, "skills/notes-helper/SKILL.md"), "---\nname: notes-helper\ndescription: Keeps notes tidy.\n---\nSort them.");
    start(scripted([]).provider);
    const { skills } = await app.call("skills/list", { cwd: project });
    expect(skills.some((skill: { name: string }) => skill.name === "notes-helper")).toBe(true);
    const { accounts } = await app.call("auth/list");
    expect(accounts).toEqual([
      { providerId: "openai", name: "ChatGPT", signedIn: false },
      { providerId: "grok", name: "Grok", signedIn: false },
    ]);
    expect((await app.call("text/complete", { prompt: "name this" })).text).toBe("a title");
    await expect(app.call("nope")).rejects.toThrow("Unknown method: nope");
    await expect(app.call("turn/start", { sessionId: "missing", prompt: "x" })).rejects.toThrow("Unknown session");
    await expect(app.call("auth/login", { providerId: "acme" })).rejects.toThrow("No sign-in for provider: acme");
  });
});
