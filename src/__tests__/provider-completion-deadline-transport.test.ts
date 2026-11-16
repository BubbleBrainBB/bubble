import http from "node:http";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createProviderInstance } from "../provider.js";
import { resolveIdleTimeoutMs } from "../network/request-timeout.js";
import type { ProviderMessage } from "../types.js";

vi.mock("../network/system-proxy.js", () => ({
  getSystemProxyForUrl: vi.fn(() => undefined),
}));

// The real default is 300 seconds; a short one stands in for it. Everything
// else is as shipped, so the transport itself still waits without limit.
vi.mock("../network/request-timeout.js", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../network/request-timeout.js")>()),
  resolveCompletionDeadlineMs: () => 400,
}));

/**
 * With no request timeout set, the transport waits on a provider for as long
 * as it stays connected. A title or memory request must still end when the
 * connection opens and then says nothing.
 */
describe("a one-shot completion against a silent connection", () => {
  let server: http.Server | undefined;
  let closedByClient = 0;
  const request: ProviderMessage[] = [{ role: "user", content: "hi" }];

  beforeEach(() => {
    closedByClient = 0;
    for (const name of ["HTTP_PROXY", "http_proxy", "HTTPS_PROXY", "https_proxy", "ALL_PROXY", "all_proxy", "NO_PROXY", "no_proxy", "BUBBLE_PROVIDER_REQUEST_TIMEOUT_MS"]) {
      vi.stubEnv(name, "");
    }
    vi.stubEnv("BUBBLE_PROVIDER_MAX_RETRIES", "0");
  });

  afterEach(async () => {
    vi.unstubAllEnvs();
    if (server) {
      server.closeAllConnections();
      await new Promise<void>((resolve) => server!.close(() => resolve()));
      server = undefined;
    }
  });

  /** Answers 200 with headers and never sends a body. */
  async function silentServer(): Promise<string> {
    server = http.createServer((req, res) => {
      req.on("close", () => {
        if (!res.writableEnded) closedByClient += 1;
      });
      res.writeHead(200, { "content-type": "application/json" });
      res.flushHeaders();
    });
    await new Promise<void>((resolve) => server!.listen(0, "127.0.0.1", resolve));
    const address = server.address();
    if (address === null || typeof address !== "object") throw new Error("no server address");
    return `http://127.0.0.1:${address.port}`;
  }

  it.each([
    ["Gemini", { providerId: "google", protocol: "ai-sdk" as const, path: "/v1beta" }, "gemini-2.5-flash"],
    ["Anthropic", { providerId: "anthropic", protocol: "anthropic-messages" as const, path: "" }, "claude-haiku-4-5-20251001"],
    ["an OpenAI-compatible provider", { providerId: "deepseek", protocol: "openai-chat" as const, path: "/v1" }, "deepseek-v4-flash"],
  ])("ends on %s instead of being held forever", async (_name, route, model) => {
    // Nothing in the transport would end this wait.
    expect(resolveIdleTimeoutMs()).toBe(0);
    const origin = await silentServer();
    const provider = createProviderInstance({
      providerId: route.providerId,
      protocol: route.protocol,
      baseURL: `${origin}${route.path}`,
      apiKey: "test-key",
    });

    const startedAt = Date.now();
    const error = await provider.complete(request, { model, thinkingLevel: "off" }).then(() => undefined, (e: Error) => e);

    expect(error?.name).toBe("TimeoutError");
    expect(Date.now() - startedAt).toBeLessThan(5_000);
    // The request was torn down, not left open behind the caller's back.
    await vi.waitFor(() => expect(closedByClient).toBeGreaterThan(0));
  });

  it("is left to a caller that can cancel it", async () => {
    const origin = await silentServer();
    const provider = createProviderInstance({
      providerId: "google",
      protocol: "ai-sdk",
      baseURL: `${origin}/v1beta`,
      apiKey: "test-key",
    });
    const caller = new AbortController();

    const pending = provider.complete(request, { model: "gemini-2.5-flash", thinkingLevel: "off", abortSignal: caller.signal })
      .then(() => "resolved", () => "rejected");
    const early = await Promise.race([pending, new Promise((resolve) => setTimeout(() => resolve("still waiting"), 1_200))]);
    caller.abort();

    expect(early).toBe("still waiting");
    await expect(pending).resolves.toBe("rejected");
  });
});
