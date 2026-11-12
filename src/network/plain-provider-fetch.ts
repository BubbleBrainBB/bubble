/**
 * Node's own fetch for a provider reached directly, with the waits a model
 * needs.
 *
 * Node's fetch gives up after 300 seconds without response headers, and after
 * 300 seconds between two pieces of a body. A model that thinks in silence
 * for longer than that is not a broken connection: cutting it there fails the
 * reply, and the retry waits the same five minutes again. The transport this
 * replaces (the OpenAI SDK's bundled node-fetch) had no such limit, and the
 * request timeout is the one the person configures. So the limits follow that
 * timeout here, and are off when it is.
 *
 * A reply cut short of its content-length still fails the read at once: that
 * is the socket closing, not a timer.
 */

import { Agent } from "undici";
import type { ProviderFetch } from "./provider-transport.js";

/** One connection pool per timeout: provider instances are made every turn. */
const dispatchers = new Map<number, Agent>();

function dispatcherFor(idleTimeoutMs: number): Agent {
  let dispatcher = dispatchers.get(idleTimeoutMs);
  if (!dispatcher) {
    dispatcher = new Agent({ headersTimeout: idleTimeoutMs, bodyTimeout: idleTimeoutMs });
    dispatchers.set(idleTimeoutMs, dispatcher);
  }
  return dispatcher;
}

export interface PlainProviderFetchOptions {
  /**
   * The longest wait for response headers, and between two pieces of a body.
   * 0 or absent: no limit.
   */
  idleTimeoutMs?: number;
  fetch?: ProviderFetch;
}

export function createPlainProviderFetch(options: PlainProviderFetchOptions = {}): ProviderFetch {
  const requested = options.idleTimeoutMs ?? 0;
  const dispatcher = dispatcherFor(Number.isFinite(requested) && requested > 0 ? Math.floor(requested) : 0);
  const fetchImpl = options.fetch ?? ((input, init) => globalThis.fetch(input, init));
  return (input, init) => fetchImpl(input, { ...(init ?? {}), dispatcher } as RequestInit);
}
