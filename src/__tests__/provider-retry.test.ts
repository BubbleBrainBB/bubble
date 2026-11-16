import { describe, expect, it } from "vitest";
import {
  computeRetryDelayMs,
  getProviderMaxRetries,
  isProviderStreamInterruption,
  isResponseIdleTimeout,
  isRetryableHttpStatus,
  ProviderStreamInterruptedError,
  providerStreamRetryLimit,
  retryAfterMsFromResponse,
  retryLimitFor,
} from "../network/retry.js";

describe("provider retry policy", () => {
  it("defaults to 4 retries and accepts a clamped env override", () => {
    expect(getProviderMaxRetries({})).toBe(4);
    expect(getProviderMaxRetries({ BUBBLE_PROVIDER_MAX_RETRIES: "2" })).toBe(2);
    expect(getProviderMaxRetries({ BUBBLE_PROVIDER_MAX_RETRIES: "0" })).toBe(0);
    expect(getProviderMaxRetries({ BUBBLE_PROVIDER_MAX_RETRIES: "99" })).toBe(10);
    expect(getProviderMaxRetries({ BUBBLE_PROVIDER_MAX_RETRIES: "abc" })).toBe(4);
    expect(getProviderMaxRetries({ BUBBLE_PROVIDER_MAX_RETRIES: "-1" })).toBe(4);
  });

  it("classifies retryable HTTP statuses", () => {
    for (const status of [408, 429, 500, 502, 503, 504, 529]) {
      expect(isRetryableHttpStatus(status)).toBe(true);
    }
    for (const status of [400, 401, 403, 404, 422]) {
      expect(isRetryableHttpStatus(status)).toBe(false);
    }
  });

  it("parses retry-after headers in seconds", () => {
    const response = new Response("", { status: 429, headers: { "retry-after": "3" } });
    expect(retryAfterMsFromResponse(response)).toBe(3000);
  });

  it("returns undefined for missing or invalid retry-after headers", () => {
    expect(retryAfterMsFromResponse(new Response("", { status: 429 }))).toBeUndefined();
    expect(retryAfterMsFromResponse(new Response("", { status: 429, headers: { "retry-after": "soon" } }))).toBeUndefined();
  });

  it("returns zero delay under NODE_ENV=test", () => {
    expect(computeRetryDelayMs(1)).toBe(0);
    expect(computeRetryDelayMs(4, { retryAfterMs: 5000 })).toBe(0);
  });

  it("identifies stream interruption errors across wrapping", () => {
    const error = new ProviderStreamInterruptedError("stream died", { cause: new Error("socket closed") });
    expect(error.name).toBe("ProviderStreamInterruptedError");
    expect(isProviderStreamInterruption(error)).toBe(true);
    expect(isProviderStreamInterruption(new Error("stream died"))).toBe(false);
    expect(isProviderStreamInterruption(undefined)).toBe(false);
  });

  describe("a reply cut by the idle limit", () => {
    // What Node's fetch raises: TypeError("terminated") over the coded undici error.
    const bodyTimeout = () => new TypeError("terminated", {
      cause: Object.assign(new Error("Body Timeout Error"), { code: "UND_ERR_BODY_TIMEOUT" }),
    });
    const headersTimeout = () => new TypeError("fetch failed", {
      cause: Object.assign(new Error("Headers Timeout Error"), { code: "UND_ERR_HEADERS_TIMEOUT" }),
    });

    it("is recognized under whatever wrapped it", () => {
      expect(isResponseIdleTimeout(bodyTimeout())).toBe(true);
      expect(isResponseIdleTimeout(headersTimeout())).toBe(true);
      expect(isResponseIdleTimeout(new Error("Failed to process successful response", { cause: bodyTimeout() }))).toBe(true);
      expect(isResponseIdleTimeout(new ProviderStreamInterruptedError("cut", { cause: bodyTimeout() }))).toBe(true);
    });

    it("is not confused with a dropped connection", () => {
      const socketClosed = new TypeError("terminated", {
        cause: Object.assign(new Error("other side closed"), { code: "UND_ERR_SOCKET" }),
      });
      expect(isResponseIdleTimeout(socketClosed)).toBe(false);
      expect(isResponseIdleTimeout(new Error("boom"))).toBe(false);
      expect(isResponseIdleTimeout(undefined)).toBe(false);
    });

    it("is re-issued once, where a dropped connection gets the full budget", () => {
      expect(retryLimitFor(bodyTimeout(), 4)).toBe(1);
      expect(retryLimitFor(bodyTimeout(), 0)).toBe(0);
      expect(retryLimitFor(new Error("ECONNRESET"), 4)).toBe(4);

      expect(providerStreamRetryLimit(new ProviderStreamInterruptedError("cut", { cause: bodyTimeout() }))).toBe(1);
      expect(providerStreamRetryLimit(new ProviderStreamInterruptedError("cut", { cause: new Error("ECONNRESET") }))).toBe(10);
    });

    it("keeps a limit the provider set itself", () => {
      expect(providerStreamRetryLimit(new ProviderStreamInterruptedError("cut", { cause: bodyTimeout(), maxRetries: 0 }))).toBe(0);
      expect(providerStreamRetryLimit(new ProviderStreamInterruptedError("cut", { cause: bodyTimeout(), maxRetries: 3 }))).toBe(3);
    });
  });
});
