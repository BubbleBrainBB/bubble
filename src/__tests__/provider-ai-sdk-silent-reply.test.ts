import http from "node:http";
import net from "node:net";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createAiSdkProvider } from "../provider-ai-sdk.js";
import { isProviderStreamInterruption, providerStreamRetryLimit } from "../network/retry.js";
import type { ProviderMessage, StreamChunk } from "../types.js";

vi.mock("../network/system-proxy.js", () => ({
  getSystemProxyForUrl: vi.fn(() => undefined),
}));

/**
 * Gemini sends a tool call whole, so while it writes a long one the reply
 * carries nothing at all. Node's fetch cuts a body after 300 seconds without
 * a byte; the Gemini transport must not inherit that, directly or through a
 * proxy, and waits only as long as the configured request timeout.
 *
 * Waiting out the real 300 seconds is too slow for a test, so these prove the
 * wiring: the limit on silence is the one set through
 * BUBBLE_PROVIDER_REQUEST_TIMEOUT_MS (a short one cuts a short silence), and
 * with none set the same silence is waited out.
 */
describe("gemini silent reply", () => {
  let server: http.Server | undefined;
  let proxy: net.Server | undefined;
  let requests = 0;
  let tunnels = 0;
  const sockets = new Set<net.Socket>();

  beforeEach(() => {
    requests = 0;
    tunnels = 0;
    for (const name of ["HTTP_PROXY", "http_proxy", "HTTPS_PROXY", "https_proxy", "ALL_PROXY", "all_proxy", "NO_PROXY", "no_proxy"]) {
      vi.stubEnv(name, "");
    }
  });

  afterEach(async () => {
    vi.unstubAllEnvs();
    for (const socket of sockets) socket.destroy();
    sockets.clear();
    if (server) {
      server.closeAllConnections();
      await new Promise<void>((resolve) => server!.close(() => resolve()));
      server = undefined;
    }
    if (proxy) {
      await new Promise<void>((resolve) => proxy!.close(() => resolve()));
      proxy = undefined;
    }
  });

  const sse = (payload: unknown) => `data: ${JSON.stringify(payload)}\n\n`;
  const textChunk = (text: string, finishReason?: string) => ({
    candidates: [{ content: { parts: [{ text }], role: "model" }, ...(finishReason ? { finishReason } : {}) }],
  });

  /** A reply that stays silent for `silenceMs` and then finishes, after an opening piece or after none. */
  async function providerWithSilence(silenceMs: number, options: { opening?: boolean } = {}) {
    server = http.createServer((_req, res) => {
      requests += 1;
      res.writeHead(200, { "content-type": "text/event-stream" });
      if (options.opening !== false) res.write(sse(textChunk("thinking")));
      else res.flushHeaders();
      setTimeout(() => res.end(sse(textChunk(" done", "STOP"))), silenceMs);
    });
    await new Promise<void>((resolve) => server!.listen(0, "127.0.0.1", resolve));
    const address = server.address();
    if (address === null || typeof address !== "object") throw new Error("no server address");
    return createAiSdkProvider({
      providerId: "google",
      apiKey: "test-key",
      baseURL: `http://127.0.0.1:${address.port}/v1beta`,
    });
  }

  /** A forward proxy that tunnels CONNECT requests, the way a local proxy app does. */
  async function startProxy(): Promise<string> {
    proxy = net.createServer((client) => {
      sockets.add(client);
      client.on("error", () => {});
      client.once("data", (head) => {
        const [, target] = /^CONNECT (\S+) HTTP/.exec(head.toString()) ?? [];
        if (!target) return client.destroy();
        tunnels += 1;
        const [host, port] = target.split(":");
        const upstream = net.connect(Number(port), host, () => {
          client.write("HTTP/1.1 200 Connection Established\r\n\r\n");
          client.pipe(upstream);
          upstream.pipe(client);
        });
        sockets.add(upstream);
        upstream.on("error", () => client.destroy());
      });
    });
    await new Promise<void>((resolve) => proxy!.listen(0, "127.0.0.1", resolve));
    const address = proxy.address();
    if (address === null || typeof address !== "object") throw new Error("no proxy address");
    return `http://127.0.0.1:${address.port}`;
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

  const request: ProviderMessage[] = [{ role: "user", content: "hi" }];
  const model = { model: "gemini-2.5-flash" };
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

  it("cuts a silence longer than the request timeout the person set, and says so", async () => {
    vi.stubEnv("BUBBLE_PROVIDER_REQUEST_TIMEOUT_MS", "300");
    const provider = await providerWithSilence(1500);

    const result = await drain(provider.streamChat(request, model));

    expect(result.outcome).toBe("threw");
    expect(text(result.received)).toBe("thinking");
    expect(isProviderStreamInterruption(result.error)).toBe(true);
    expect((result.error as Error).message).toBe(
      "Gemini stream interrupted: the model sent nothing for 0.3s, the limit set by BUBBLE_PROVIDER_REQUEST_TIMEOUT_MS",
    );
    // The same request would run into the same limit: one more try, not ten.
    expect(providerStreamRetryLimit(result.error as never)).toBe(1);
  });

  it("waits out a silence shorter than the request timeout the person set", async () => {
    vi.stubEnv("BUBBLE_PROVIDER_REQUEST_TIMEOUT_MS", "5000");
    const provider = await providerWithSilence(1200);

    const result = await drain(provider.streamChat(request, model));

    expect(result.outcome).toBe("ended");
    expect(text(result.received)).toBe("thinking done");
  });

  it("re-issues a reply cut before any content once, then reports the timeout", async () => {
    vi.stubEnv("BUBBLE_PROVIDER_REQUEST_TIMEOUT_MS", "300");
    const provider = await providerWithSilence(1500, { opening: false });

    const result = await drain(provider.streamChat(request, model));

    expect(result.outcome).toBe("threw");
    expect(requests).toBe(2);
    expect(isProviderStreamInterruption(result.error)).toBe(false);
    expect((result.error as Error).message).toBe(
      "Gemini request timed out: the model sent nothing for 0.3s, the limit set by BUBBLE_PROVIDER_REQUEST_TIMEOUT_MS",
    );
  });

  describe("through a proxy", () => {
    it("waits out a silence when no request timeout is set", async () => {
      vi.stubEnv("BUBBLE_PROVIDER_REQUEST_TIMEOUT_MS", "");
      vi.stubEnv("HTTP_PROXY", await startProxy());
      const provider = await providerWithSilence(1500);

      const result = await drain(provider.streamChat(request, model));

      expect(tunnels).toBe(1);
      expect(result.outcome).toBe("ended");
      expect(text(result.received)).toBe("thinking done");
    });

    it("cuts a silence longer than the request timeout the person set", async () => {
      vi.stubEnv("BUBBLE_PROVIDER_REQUEST_TIMEOUT_MS", "300");
      vi.stubEnv("HTTP_PROXY", await startProxy());
      const provider = await providerWithSilence(1500);

      const result = await drain(provider.streamChat(request, model));

      expect(tunnels).toBe(1);
      expect(result.outcome).toBe("threw");
      expect(isProviderStreamInterruption(result.error)).toBe(true);
      expect(text(result.received)).toBe("thinking");
    });
  });
});
