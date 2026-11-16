/**
 * The undici dispatcher every provider request goes through.
 *
 * One place builds it so that no transport can be left on undici's defaults:
 * those cut a reply after 300 seconds without response headers, or without a
 * byte of body, which a model working in silence runs past. Direct, proxied
 * and custom-CA connections all wait as long as `idleTimeoutMs` says.
 */

import { rootCertificates } from "node:tls";
import { Agent, ProxyAgent, type Dispatcher } from "undici";

export interface ProviderDispatcherOptions {
  /** Proxy URI; absent for a direct connection. */
  proxy?: string;
  /** Extra CA certificates (PEM), trusted on top of Node's roots. */
  ca?: string[];
  /**
   * The longest wait for response headers, and between two pieces of a body.
   * 0: no limit.
   */
  idleTimeoutMs: number;
}

/**
 * One connection pool per route: transports ask for a dispatcher on every
 * request, and provider instances are made every turn.
 */
const dispatchers = new Map<string, Dispatcher>();

export function providerDispatcher(options: ProviderDispatcherOptions): Dispatcher {
  const idleTimeoutMs = Number.isFinite(options.idleTimeoutMs) && options.idleTimeoutMs > 0
    ? Math.floor(options.idleTimeoutMs)
    : 0;
  const ca = options.ca ?? [];
  const key = JSON.stringify([options.proxy ?? "", idleTimeoutMs, ca]);
  let dispatcher = dispatchers.get(key);
  if (!dispatcher) {
    dispatcher = buildDispatcher(options.proxy, ca, idleTimeoutMs);
    dispatchers.set(key, dispatcher);
  }
  return dispatcher;
}

function buildDispatcher(proxy: string | undefined, ca: string[], idleTimeoutMs: number): Dispatcher {
  const waits = { headersTimeout: idleTimeoutMs, bodyTimeout: idleTimeoutMs };
  const tls = ca.length > 0 ? { ca: [...rootCertificates, ...ca] } : undefined;
  if (proxy) {
    return new ProxyAgent({
      uri: proxy,
      ...waits,
      ...(tls ? { requestTls: tls, proxyTls: tls } : {}),
    });
  }
  return new Agent({ ...waits, ...(tls ? { connect: tls } : {}) });
}
