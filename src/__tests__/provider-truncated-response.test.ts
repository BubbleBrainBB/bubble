import http from "node:http";
import { afterEach, describe, expect, it } from "vitest";
import { Agent } from "../agent.js";
import { createProviderInstance } from "../provider.js";
import { isProviderStreamInterruption } from "../network/retry.js";
import type { StreamChunk } from "../types.js";

/**
 * Regression: an OpenAI-compatible reply that stops short of what it promised
 * must be retried, never left pending and never taken for a finished answer.
 *
 * Two ways a reply stops short:
 *  - the socket closes before `content-length` bytes arrived (a proxy or
 *    gateway that sends a fixed length). The OpenAI SDK's bundled node-fetch
 *    only notices a premature close on chunked bodies, so the stream neither
 *    ended nor failed and the turn stayed "running" forever;
 *  - the body ends cleanly but never carried `[DONE]` or a finish_reason.
 */
describe("openai-chat truncated response", () => {
  let server: http.Server | undefined;
  let requests = 0;

  afterEach(async () => {
    if (server) {
      server.closeAllConnections();
      await new Promise<void>((resolve) => server!.close(() => resolve()));
      server = undefined;
    }
    requests = 0;
  });

  async function startProvider(handler: (res: http.ServerResponse, request: number) => void) {
    server = http.createServer((_req, res) => handler(res, ++requests));
    await new Promise<void>((resolve) => server!.listen(0, "127.0.0.1", resolve));
    const address = server.address();
    if (address === null || typeof address !== "object") throw new Error("no server address");
    return createProviderInstance({
      providerId: "deepseek",
      baseURL: `http://127.0.0.1:${address.port}/v1`,
      apiKey: "test-key",
    });
  }

  const sse = (payload: unknown) => `data: ${JSON.stringify(payload)}\n\n`;
  const delta = (content: string) => sse({ choices: [{ delta: { content } }] });
  const stop = sse({ choices: [{ delta: {}, finish_reason: "stop" }] });

  /** One delta, 1000 more bytes declared than sent, then the socket closes. */
  function cutShortOfContentLength(res: http.ServerResponse, content: string) {
    const body = delta(content);
    res.writeHead(200, {
      "content-type": "text/event-stream",
      "content-length": String(Buffer.byteLength(body) + 1000),
    });
    res.write(body);
    setTimeout(() => res.socket?.destroy(), 20);
  }

  /** Settles with what the stream did, or "pending" when it does nothing for 2s. */
  async function drain(stream: AsyncIterable<StreamChunk>) {
    const received: StreamChunk[] = [];
    const settled = (async () => {
      try {
        for await (const chunk of stream) received.push(chunk);
        return { outcome: "ended" as const, received };
      } catch (error) {
        return { outcome: "threw" as const, received, error };
      }
    })();
    let timer: NodeJS.Timeout | undefined;
    const pending = new Promise<{ outcome: "pending"; received: StreamChunk[]; error?: unknown }>((resolve) => {
      timer = setTimeout(() => resolve({ outcome: "pending", received }), 2000);
    });
    try {
      return await Promise.race([settled, pending]);
    } finally {
      clearTimeout(timer);
    }
  }

  const request = [{ role: "user" as const, content: "hi" }];
  const model = { model: "deepseek-v4-pro" };

  it("throws a stream interruption when the socket closes short of content-length", async () => {
    const provider = await startProvider((res) => cutShortOfContentLength(res, "partial"));

    const result = await drain(provider.streamChat(request, model));

    expect(result.outcome).toBe("threw");
    expect(isProviderStreamInterruption(result.error)).toBe(true);
    expect(result.received).toContainEqual({ type: "text", content: "partial" });
    expect(result.received.some((chunk) => chunk.type === "done")).toBe(false);
  });

  it("throws a stream interruption when the body ends without [DONE] or a finish_reason", async () => {
    const provider = await startProvider((res) => {
      res.writeHead(200, { "content-type": "text/event-stream" });
      res.end(delta("partial"));
    });

    const result = await drain(provider.streamChat(request, model));

    expect(result.outcome).toBe("threw");
    expect(isProviderStreamInterruption(result.error)).toBe(true);
    expect((result.error as Error).message).toContain("finish_reason");
    expect(result.received.some((chunk) => chunk.type === "done")).toBe(false);
  });

  it("accepts a finish_reason without [DONE]", async () => {
    const provider = await startProvider((res) => {
      res.writeHead(200, { "content-type": "text/event-stream" });
      res.end(delta("full answer") + stop);
    });

    const result = await drain(provider.streamChat(request, model));

    expect(result.outcome).toBe("ended");
    expect(result.received.at(-1)).toEqual({ type: "done" });
  });

  it("accepts [DONE] without a finish_reason, also when split across network chunks", async () => {
    const provider = await startProvider((res) => {
      res.writeHead(200, { "content-type": "text/event-stream" });
      res.write(delta("full answer") + "data: [DO");
      setTimeout(() => res.end("NE]\n\n"), 20);
    });

    const result = await drain(provider.streamChat(request, model));

    expect(result.outcome).toBe("ended");
    expect(result.received).toContainEqual({ type: "text", content: "full answer" });
    expect(result.received.at(-1)).toEqual({ type: "done" });
  });

  it("is retried by the agent, which keeps only the complete answer", async () => {
    const provider = await startProvider((res, attempt) => {
      if (attempt === 1) {
        cutShortOfContentLength(res, "cut");
        return;
      }
      res.writeHead(200, { "content-type": "text/event-stream" });
      res.end(delta("complete answer") + stop + "data: [DONE]\n\n");
    });
    const agent = new Agent({ provider, providerId: "deepseek", model: model.model, tools: [] });

    const events: Array<{ type: string }> = [];
    const run = (async () => {
      for await (const event of agent.run("test", process.cwd())) events.push(event);
      return "ended" as const;
    })();
    let timer: NodeJS.Timeout | undefined;
    const outcome = await Promise.race([
      run,
      new Promise<"pending">((resolve) => { timer = setTimeout(() => resolve("pending"), 3000); }),
    ]);
    clearTimeout(timer);

    expect(outcome).toBe("ended");
    expect(requests).toBe(2);
    expect(events.filter((event) => event.type === "provider_retry")).toHaveLength(1);
    expect(agent.messages.filter((message) => message.role === "assistant").map((message) => message.content))
      .toEqual(["complete answer"]);
  });
});
