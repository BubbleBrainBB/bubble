import http from "node:http";
import { afterEach, describe, expect, it, vi } from "vitest";
import { createProviderInstance } from "../provider.js";
import { isProviderStreamInterruption } from "../network/retry.js";
import type { StreamChunk } from "../types.js";

/**
 * A model that says nothing for a while is thinking, not gone. Node's fetch
 * cuts a body after 300 seconds without a byte; the generic provider must
 * not inherit that, and waits only as long as the configured request timeout.
 *
 * Waiting out the real 300 seconds is too slow for a test, so these prove the
 * wiring instead: the limit on silence is the one set through
 * BUBBLE_PROVIDER_REQUEST_TIMEOUT_MS (a short one cuts a short silence), and
 * with none set the same silence is waited out. That the unset case has no
 * limit at all, rather than a long one, was checked by hand against a reply
 * silent for 330 seconds.
 */
describe("openai-chat silent reply", () => {
  let server: http.Server | undefined;

  afterEach(async () => {
    vi.unstubAllEnvs();
    if (server) {
      server.closeAllConnections();
      await new Promise<void>((resolve) => server!.close(() => resolve()));
      server = undefined;
    }
  });

  const sse = (payload: unknown) => `data: ${JSON.stringify(payload)}\n\n`;

  /** A reply that starts, stays silent for `silenceMs`, then finishes. */
  async function providerWithSilence(silenceMs: number) {
    server = http.createServer((_req, res) => {
      res.writeHead(200, { "content-type": "text/event-stream" });
      res.write(sse({ choices: [{ delta: { content: "thinking" } }] }));
      setTimeout(() => {
        res.end(sse({ choices: [{ delta: { content: " done" }, finish_reason: "stop" }] }) + "data: [DONE]\n\n");
      }, silenceMs);
    });
    await new Promise<void>((resolve) => server!.listen(0, "127.0.0.1", resolve));
    const address = server.address();
    if (address === null || typeof address !== "object") throw new Error("no server address");
    return createProviderInstance({
      providerId: "deepseek",
      baseURL: `http://127.0.0.1:${address.port}/v1`,
      apiKey: "test-key",
    });
  }

  async function drain(stream: AsyncIterable<StreamChunk>) {
    const received: StreamChunk[] = [];
    try {
      for await (const chunk of stream) received.push(chunk);
      return { outcome: "ended" as const, received };
    } catch (error) {
      return { outcome: "threw" as const, received, error };
    }
  }

  const request = [{ role: "user" as const, content: "hi" }];
  const model = { model: "deepseek-v4-pro" };
  const text = (chunks: StreamChunk[]) =>
    chunks.map((chunk) => (chunk.type === "text" ? chunk.content : "")).join("");

  it("waits out a silence when no request timeout is set", async () => {
    vi.stubEnv("BUBBLE_PROVIDER_REQUEST_TIMEOUT_MS", "");
    const provider = await providerWithSilence(1500);

    const result = await drain(provider.streamChat(request, model));

    expect(result.outcome).toBe("ended");
    expect(text(result.received)).toBe("thinking done");
    expect(result.received.at(-1)).toEqual({ type: "done" });
  });

  it("cuts a silence longer than the request timeout the person set, as an interruption", async () => {
    vi.stubEnv("BUBBLE_PROVIDER_REQUEST_TIMEOUT_MS", "300");
    const provider = await providerWithSilence(1500);

    const result = await drain(provider.streamChat(request, model));

    expect(result.outcome).toBe("threw");
    expect(isProviderStreamInterruption(result.error)).toBe(true);
    expect(text(result.received)).toBe("thinking");
  });

  it("waits out a silence shorter than the request timeout the person set", async () => {
    vi.stubEnv("BUBBLE_PROVIDER_REQUEST_TIMEOUT_MS", "5000");
    const provider = await providerWithSilence(1200);

    const result = await drain(provider.streamChat(request, model));

    expect(result.outcome).toBe("ended");
    expect(text(result.received)).toBe("thinking done");
  });
});
