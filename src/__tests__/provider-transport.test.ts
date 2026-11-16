import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { Agent, ProxyAgent } from "undici";
import {
  createProviderDispatcher,
  createProviderFetch,
  isProviderTransportError,
  normalizeProviderNetworkError,
  shouldEnableFetchVerbose,
} from "../network/provider-transport.js";
import { getSystemProxyForUrl } from "../network/system-proxy.js";

vi.mock("../network/system-proxy.js", () => ({
  getSystemProxyForUrl: vi.fn(() => undefined),
}));

const getSystemProxyForUrlMock = vi.mocked(getSystemProxyForUrl);

describe("provider transport", () => {
  beforeEach(() => {
    getSystemProxyForUrlMock.mockReset();
    getSystemProxyForUrlMock.mockReturnValue(undefined);
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("connects directly without proxy or custom CA env", async () => {
    const fetchMock = vi.fn(async (_input: RequestInfo | URL, init?: RequestInit) => {
      expect((init as RequestInit & { dispatcher?: unknown })?.dispatcher).toBeInstanceOf(Agent);
      return new Response("ok");
    });
    const fetch = createProviderFetch({ providerName: "Anthropic", fetch: fetchMock, env: {} });

    await expect(fetch("https://api.anthropic.com/v1/messages")).resolves.toBeInstanceOf(Response);
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });

  it("attaches an undici dispatcher when proxy env is configured", async () => {
    const fetchMock = vi.fn(async (_input: RequestInfo | URL, init?: RequestInit) => {
      expect((init as RequestInit & { dispatcher?: unknown })?.dispatcher).toBeInstanceOf(ProxyAgent);
      return new Response("ok");
    });
    const fetch = createProviderFetch({
      providerName: "Anthropic",
      fetch: fetchMock,
      env: {
        HTTPS_PROXY: "http://proxy.example.test:8080",
        NO_PROXY: "localhost,127.0.0.1",
      },
    });

    await fetch("https://api.anthropic.com/v1/messages");
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });

  it("bypasses proxy env when NO_PROXY matches the request host", async () => {
    const fetchMock = vi.fn(async (_input: RequestInfo | URL, init?: RequestInit) => {
      expect((init as RequestInit & { dispatcher?: unknown })?.dispatcher).toBeInstanceOf(Agent);
      return new Response("ok");
    });
    const fetch = createProviderFetch({
      providerName: "Anthropic",
      fetch: fetchMock,
      env: {
        HTTPS_PROXY: "http://proxy.example.test:8080",
        NO_PROXY: "api.anthropic.com,.internal.test,localhost:3000",
      },
    });

    await fetch("https://api.anthropic.com/v1/messages");
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });

  it("matches NO_PROXY entries with leading dot and port", async () => {
    const fetchMock = vi.fn(async (_input: RequestInfo | URL, init?: RequestInit) => {
      expect((init as RequestInit & { dispatcher?: unknown })?.dispatcher).toBeInstanceOf(Agent);
      return new Response("ok");
    });
    const fetch = createProviderFetch({
      providerName: "Local provider",
      fetch: fetchMock,
      env: {
        HTTP_PROXY: "http://proxy.example.test:8080",
        HTTPS_PROXY: "http://proxy.example.test:8080",
        NO_PROXY: ".internal.test,localhost:3000",
      },
    });

    await fetch("https://models.internal.test/v1/messages");
    await fetch("http://localhost:3000/v1/messages");
    expect(fetchMock).toHaveBeenCalledTimes(2);
  });

  it("falls back to the macOS system proxy when no proxy env is set", async () => {
    getSystemProxyForUrlMock.mockReturnValue("http://127.0.0.1:7897");
    const fetchMock = vi.fn(async (_input: RequestInfo | URL, init?: RequestInit) => {
      expect((init as RequestInit & { dispatcher?: unknown })?.dispatcher).toBeInstanceOf(ProxyAgent);
      return new Response("ok");
    });
    const fetch = createProviderFetch({ providerName: "Anthropic", fetch: fetchMock, env: {} });

    await fetch("https://api.anthropic.com/v1/messages");
    expect(fetchMock).toHaveBeenCalledTimes(1);
    expect(getSystemProxyForUrlMock).toHaveBeenCalled();
  });

  it("prefers proxy env variables over the system proxy", async () => {
    getSystemProxyForUrlMock.mockReturnValue("http://127.0.0.1:7897");
    const fetchMock = vi.fn(async () => new Response("ok"));
    const fetch = createProviderFetch({
      providerName: "Anthropic",
      fetch: fetchMock,
      env: { HTTPS_PROXY: "http://proxy.example.test:8080" },
    });

    await fetch("https://api.anthropic.com/v1/messages");
    expect(getSystemProxyForUrlMock).not.toHaveBeenCalled();
  });

  it("does not consult the system proxy when NO_PROXY matches the host", async () => {
    getSystemProxyForUrlMock.mockReturnValue("http://127.0.0.1:7897");
    const fetchMock = vi.fn(async (_input: RequestInfo | URL, init?: RequestInit) => {
      expect((init as RequestInit & { dispatcher?: unknown })?.dispatcher).toBeInstanceOf(Agent);
      return new Response("ok");
    });
    const fetch = createProviderFetch({
      providerName: "Anthropic",
      fetch: fetchMock,
      env: { NO_PROXY: "api.anthropic.com" },
    });

    await fetch("https://api.anthropic.com/v1/messages");
    expect(getSystemProxyForUrlMock).not.toHaveBeenCalled();
  });

  it("mentions the detected system proxy in normalized network errors", () => {
    getSystemProxyForUrlMock.mockReturnValue("http://127.0.0.1:7897");
    const error = normalizeProviderNetworkError(
      new Error("The socket connection was closed unexpectedly"),
      {
        providerName: "Anthropic",
        input: "https://api.anthropic.com/v1/messages",
        env: {},
      },
    );

    expect(error.message).toContain("OS system proxy at http://127.0.0.1:7897");
    expect(error.message).toContain("BUBBLE_SYSTEM_PROXY=0");
  });

  it("creates a dispatcher when custom CA env is configured", () => {
    const dir = mkdtempSync(join(tmpdir(), "bubble-provider-ca-"));
    try {
      const certPath = join(dir, "ca.pem");
      writeFileSync(certPath, "-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----\n");

      expect(createProviderDispatcher({ BUBBLE_EXTRA_CA_CERTS: certPath }, "https://api.anthropic.com", "Anthropic")).toBeTruthy();
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });

  it("reports invalid custom CA paths clearly", () => {
    expect(() => createProviderDispatcher({ BUBBLE_EXTRA_CA_CERTS: "/missing/ca.pem" }, "https://api.anthropic.com", "Anthropic"))
      .toThrow(/Failed to read Anthropic custom CA certificate/);
  });

  it("enables verbose fetch from provider-specific or generic env", () => {
    expect(shouldEnableFetchVerbose({ BUBBLE_ANTHROPIC_FETCH_VERBOSE: "1" }, "BUBBLE_ANTHROPIC_FETCH_VERBOSE")).toBe(true);
    expect(shouldEnableFetchVerbose({ BUBBLE_PROVIDER_FETCH_VERBOSE: "true" }, "BUBBLE_ANTHROPIC_FETCH_VERBOSE")).toBe(true);
    expect(shouldEnableFetchVerbose({ BUBBLE_ANTHROPIC_FETCH_VERBOSE: "0" }, "BUBBLE_ANTHROPIC_FETCH_VERBOSE")).toBe(false);
  });

  it("adds verbose to fetch init when enabled", async () => {
    const fetchMock = vi.fn(async (_input: RequestInfo | URL, init?: RequestInit) => {
      expect((init as RequestInit & { verbose?: boolean })?.verbose).toBe(true);
      return new Response("ok");
    });
    const fetch = createProviderFetch({
      providerName: "Anthropic",
      fetch: fetchMock,
      env: { BUBBLE_ANTHROPIC_FETCH_VERBOSE: "yes" },
      verboseEnvVar: "BUBBLE_ANTHROPIC_FETCH_VERBOSE",
    });

    await fetch("https://api.anthropic.com/v1/messages");
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });

  it("normalizes certificate errors with provider and origin guidance", () => {
    const error = normalizeProviderNetworkError(
      new Error("unknown certificate verification error"),
      {
        providerName: "Anthropic",
        input: "https://api.anthropic.com/v1/messages",
        env: {},
      },
    );

    expect(error.message).toContain("Anthropic connection failed");
    expect(error.message).toContain("Target origin: https://api.anthropic.com");
    expect(error.message).toContain("TLS certificate verification failed");
    expect(error.message).toContain("NODE_EXTRA_CA_CERTS");
    expect(error.message).toContain("NODE_TLS_REJECT_UNAUTHORIZED=0");
  });

  it("wraps transport errors from fetch while preserving cause", async () => {
    const original = new Error("The socket connection was closed unexpectedly");
    const fetch = createProviderFetch({
      providerName: "Anthropic",
      fetch: vi.fn(async () => {
        throw original;
      }),
      env: {},
    });

    await expect(fetch("https://api.anthropic.com/v1/messages")).rejects.toMatchObject({
      message: expect.stringContaining("Anthropic connection failed"),
      cause: original,
    });
  });

  it("does not double-wrap an already normalized error", () => {
    const original = new Error("unknown certificate verification error");
    const wrappedOnce = normalizeProviderNetworkError(original, {
      providerName: "Anthropic",
      input: "https://api.anthropic.com/v1/messages",
      env: {},
    });
    const wrappedTwice = normalizeProviderNetworkError(wrappedOnce, {
      providerName: "Anthropic",
      input: "https://api.anthropic.com/v1/messages",
      env: {},
    });

    expect(wrappedTwice).toBe(wrappedOnce);
    expect(wrappedTwice.message.match(/Original error:/g)).toHaveLength(1);
  });

  it("preserves non-network errors unchanged", () => {
    const original = new Error("invalid request body");

    expect(normalizeProviderNetworkError(original, { providerName: "Anthropic" })).toBe(original);
    expect(isProviderTransportError(original)).toBe(false);
    expect(isProviderTransportError(new Error("socket hang up"))).toBe(true);
  });

  it("classifies request/response timeouts as transport errors", () => {
    // Bun fetch throws this exact prose; openai-node uses a named error.
    expect(isProviderTransportError(new Error("The operation timed out."))).toBe(true);
    expect(isProviderTransportError(Object.assign(new Error("x"), { name: "TimeoutError" }))).toBe(true);
    expect(isProviderTransportError(new Error("Request timed out."))).toBe(true);
    expect(isProviderTransportError(Object.assign(new Error("y"), { name: "APIConnectionTimeoutError" }))).toBe(true);
    // No over-match: an ordinary app-layer error is not a transport error.
    expect(isProviderTransportError(new Error("invalid request body"))).toBe(false);
  });

  it.each(["UND_ERR_SOCKET", "ECONNRESET", "ETIMEDOUT"])("recognizes nested Error.code %s without matching the message", (code) => {
    const cause = Object.assign(new Error("other side closed"), { name: "SocketError", code });
    expect(isProviderTransportError(new TypeError("terminated", { cause }))).toBe(true);
  });

  it("does not classify bare termination or user cancellation as transport failures", () => {
    expect(isProviderTransportError(new TypeError("terminated"))).toBe(false);
    expect(isProviderTransportError(new DOMException("The operation was aborted", "AbortError"))).toBe(false);
  });

  it("does NOT rewrap a plain timeout into the proxy/TLS advice message", () => {
    // Timeout patterns live outside isProviderNetworkErrorText so a bare
    // timeout is returned as-is, never decorated with misleading proxy advice.
    const timeout = new Error("The operation timed out.");
    const result = normalizeProviderNetworkError(timeout, { providerName: "zai" });
    expect(result).toBe(timeout);
    expect(result.message).not.toContain("connection failed before Bubble received a response");
  });
});
