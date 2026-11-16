import { afterEach, describe, expect, it, vi } from "vitest";
import { withCompletionDeadline } from "../provider.js";
import { DEFAULT_COMPLETION_DEADLINE_MS, resolveCompletionDeadlineMs } from "../network/request-timeout.js";
import type { Provider, ProviderMessage } from "../types.js";

/**
 * The transport waits on a provider as long as a model may think. A one-shot
 * completion (a title, a memory pass) runs unseen and
 * its caller cannot cancel it, so it must not inherit that patience: a
 * connection that stays open and sends nothing would hold it forever.
 */
const request: ProviderMessage[] = [{ role: "user", content: "hi" }];

async function settle<T>(promise: Promise<T>) {
  const startedAt = Date.now();
  try {
    return { outcome: "resolved" as const, value: await promise, elapsedMs: Date.now() - startedAt };
  } catch (error) {
    return { outcome: "rejected" as const, error: error as Error, elapsedMs: Date.now() - startedAt };
  }
}

describe("completion deadline", () => {
  afterEach(() => {
    vi.unstubAllEnvs();
  });

  it("is the request timeout the person set, and 300 seconds when they set none", () => {
    expect(resolveCompletionDeadlineMs({})).toBe(DEFAULT_COMPLETION_DEADLINE_MS);
    expect(DEFAULT_COMPLETION_DEADLINE_MS).toBe(300_000);
    expect(resolveCompletionDeadlineMs({ BUBBLE_PROVIDER_REQUEST_TIMEOUT_MS: "45000" })).toBe(45_000);
  });

  describe("around any provider", () => {
    function provider(complete: Provider["complete"]): Provider {
      return withCompletionDeadline({
        async *streamChat() {
          yield { type: "done" };
        },
        complete,
      });
    }

    it("ends a completion that never answers, and aborts its request", async () => {
      vi.stubEnv("BUBBLE_PROVIDER_REQUEST_TIMEOUT_MS", "200");
      let seen: AbortSignal | undefined;
      const stalled = provider((_messages, options) => new Promise((_resolve, reject) => {
        seen = options?.abortSignal;
        seen?.addEventListener("abort", () => reject(seen!.reason));
      }));

      const result = await settle(stalled.complete(request));

      expect(result.outcome).toBe("rejected");
      expect(result.elapsedMs).toBeLessThan(2_000);
      expect(result.outcome === "rejected" && result.error.name).toBe("TimeoutError");
      expect(result.outcome === "rejected" && result.error.message).toBe(
        "Provider request timed out: no complete reply after 0s.",
      );
      expect(seen?.aborted).toBe(true);
    });

    it("releases the caller even when the transport ignores the abort", async () => {
      vi.stubEnv("BUBBLE_PROVIDER_REQUEST_TIMEOUT_MS", "200");
      const deaf = provider(() => new Promise(() => {}));

      const result = await settle(deaf.complete(request));

      expect(result.outcome).toBe("rejected");
      expect(result.elapsedMs).toBeLessThan(2_000);
    });

    it("leaves a completion alone when its caller can cancel it", async () => {
      vi.stubEnv("BUBBLE_PROVIDER_REQUEST_TIMEOUT_MS", "200");
      const caller = new AbortController();
      let seen: AbortSignal | undefined;
      const slow = provider((_messages, options) => new Promise((resolve) => {
        seen = options?.abortSignal;
        setTimeout(() => resolve("summary"), 700);
      }));

      const result = await settle(slow.complete(request, { abortSignal: caller.signal }));

      expect(result).toMatchObject({ outcome: "resolved", value: "summary" });
      expect(seen).toBe(caller.signal);
    });

    it("returns a completion that answers in time, with its options intact", async () => {
      vi.stubEnv("BUBBLE_PROVIDER_REQUEST_TIMEOUT_MS", "5000");
      let seenModel: string | undefined;
      const quick = provider(async (_messages, options) => {
        seenModel = options?.model;
        return "a title";
      });

      await expect(quick.complete(request, { model: "glm-5.3", temperature: 0.3 })).resolves.toBe("a title");
      expect(seenModel).toBe("glm-5.3");
    });

    it("passes a completion's own failure through", async () => {
      vi.stubEnv("BUBBLE_PROVIDER_REQUEST_TIMEOUT_MS", "5000");
      const failing = provider(async () => {
        throw new Error("401 bad key");
      });

      await expect(failing.complete(request)).rejects.toThrow("401 bad key");
    });
  });
});
