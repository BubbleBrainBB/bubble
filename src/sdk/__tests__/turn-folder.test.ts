import { mkdtempSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, describe, expect, it } from "vitest";
import { BubbleSdk, type Message, type Provider } from "../index.js";

// One conversation, two projects: the session is made in `home`, and a turn
// may name another folder to work in.
const home = realpathSync(mkdtempSync(join(tmpdir(), "bubble-turn-folder-home-")));
const other = realpathSync(mkdtempSync(join(tmpdir(), "bubble-turn-folder-other-")));
writeFileSync(join(home, "note.txt"), "the note at home");
writeFileSync(join(other, "note.txt"), "the note in the other project");
afterAll(() => {
  rmSync(home, { recursive: true, force: true });
  rmSync(other, { recursive: true, force: true });
});

const drain = async (events: AsyncIterable<unknown>) => {
  for await (const _ of events) {
  }
};

const textOf = (message: Message | undefined) =>
  typeof message?.content === "string" ? message.content : JSON.stringify(message?.content ?? "");

describe("a turn's own folder", () => {
  it("works in the folder the turn names and leaves the session where it was made", async () => {
    const seen: { system: string; read: string }[] = [];
    let call = 0;
    const provider: Provider = {
      async *streamChat(messages) {
        call++;
        // Odd calls ask for the note by a relative path; even ones see it.
        if (call % 2 === 1) {
          yield { type: "tool_call", id: `read-${call}`, name: "read", arguments: JSON.stringify({ path: "note.txt" }), isStart: true, isEnd: true };
        } else {
          const result = [...messages].reverse().find((m) => m.role === "tool");
          seen.push({ system: textOf(messages.find((m) => m.role === "system")), read: textOf(result) });
          yield { type: "text", content: "Read it." };
        }
        yield { type: "done" };
      },
      async complete() {
        return "";
      },
    };
    const sdk = new BubbleSdk({ defaultCwd: home, mcp: false });
    (sdk as any).resolveProvider = () => ({ provider, providerId: "test", model: "test:model" });
    const session = sdk.createSession({ cwd: home });

    await drain(sdk.runTurn(session.id, { prompt: "read the note", cwd: other }));
    await drain(sdk.runTurn(session.id, { prompt: "read the note again" }));

    expect(seen).toHaveLength(2);
    expect(seen[0].read).toContain("the note in the other project");
    expect(seen[0].system).toContain(other);
    expect(seen[1].read).toContain("the note at home");
    expect(seen[1].system).toContain(home);
    expect(seen[1].system).not.toContain(other);

    // Both turns are one conversation, kept where the session was made.
    expect(sdk.openSession(session.id).cwd).toBe(home);
    expect(sdk.getHistory(session.id).filter((m) => m.role === "user")).toHaveLength(2);
    const listed = sdk.listSessions().find((s) => s.name === session.id);
    expect(listed?.cwd ?? listed?.cwdLabel).toBe(home);
  });
});
