import { readFileSync } from "node:fs";
import { delimiter } from "node:path";
import type { Dispatcher } from "undici";
import { providerDispatcher } from "./provider-dispatcher.js";
import { resolveIdleTimeoutMs } from "./request-timeout.js";
import { getSystemProxyForUrl } from "./system-proxy.js";

export type ProviderFetch = (input: RequestInfo | URL, init?: RequestInit) => Promise<Response>;

export interface ProviderFetchOptions {
  providerName: string;
  fetch?: ProviderFetch;
  env?: NodeJS.ProcessEnv;
  verboseEnvVar?: string;
}

type RequestInitWithProviderOptions = RequestInit & {
  dispatcher?: Dispatcher;
  verbose?: boolean;
};

export function providerFetch(input: RequestInfo | URL, init: RequestInit | undefined, options: ProviderFetchOptions): Promise<Response> {
  return createProviderFetch(options)(input, init);
}

export function createProviderFetch(options: ProviderFetchOptions): ProviderFetch {
  const env = options.env ?? process.env;
  const fetchImpl = options.fetch ?? ((input, init) => globalThis.fetch(input, init));

  return async (input, init) => {
    try {
      const requestInit = withProviderNetworkOptions(input, init, {
        env,
        providerName: options.providerName,
        verboseEnvVar: options.verboseEnvVar,
      });
      return await fetchImpl(input, requestInit);
    } catch (error) {
      throw normalizeProviderNetworkError(error, {
        providerName: options.providerName,
        input,
        env,
      });
    }
  };
}

/**
 * Every provider request gets a dispatcher, proxied or not: left on Node's
 * default one, a reply is cut after 300 seconds of silence.
 */
export function createProviderDispatcher(
  env: NodeJS.ProcessEnv = process.env,
  input?: RequestInfo | URL,
  providerName = "provider",
): Dispatcher {
  return providerDispatcher({
    proxy: input ? nodeProxyForUrl(input, env) : defaultNodeProxy(env),
    ca: loadExtraCaCertificates(env, providerName),
    idleTimeoutMs: resolveIdleTimeoutMs(env),
  });
}

export function withProviderNetworkOptions(
  input: RequestInfo | URL,
  init: RequestInit | undefined,
  options: {
    env?: NodeJS.ProcessEnv;
    providerName?: string;
    verboseEnvVar?: string;
  } = {},
): RequestInitWithProviderOptions {
  const env = options.env ?? process.env;
  const providerName = options.providerName ?? "provider";
  const next = { ...(init ?? {}) } as RequestInitWithProviderOptions;

  next.dispatcher = createProviderDispatcher(env, input, providerName);

  if (shouldEnableFetchVerbose(env, options.verboseEnvVar)) {
    next.verbose = true;
  }

  return next;
}

export function normalizeProviderNetworkError(
  error: unknown,
  options: {
    providerName: string;
    input?: RequestInfo | URL;
    env?: NodeJS.ProcessEnv;
  },
): Error {
  const env = options.env ?? process.env;
  // Already normalized (e.g. by createProviderFetch) — wrapping again would
  // nest "Original error:" messages.
  if (error instanceof Error && error.message.includes("connection failed before Bubble received a response")) {
    return error;
  }
  const text = errorMessageChain(error).join("\n");
  if (!isProviderNetworkErrorText(text)) {
    return error instanceof Error ? error : new Error(String(error));
  }

  const origin = originFromInput(options.input);
  const providerLabel = options.providerName || "provider";
  const systemProxy = hasProxyEnv(env) ? undefined : getSystemProxyForUrl(options.input ? urlFromInput(options.input) : undefined, env);
  const message = [
    `${providerLabel} connection failed before Bubble received a response.`,
    origin ? `Target origin: ${origin}.` : undefined,
    isCertificateErrorText(text)
      ? "TLS certificate verification failed. If you are on a corporate proxy, VPN, or HTTPS inspection network, start Bubble with NODE_EXTRA_CA_CERTS=/absolute/path/to/ca.pem or BUBBLE_EXTRA_CA_CERTS=/absolute/path/to/ca.pem."
      : "This looks like a proxy or network transport failure.",
    hasProxyEnv(env)
      ? "Bubble is using proxy environment variables for provider requests. Make sure NO_PROXY includes localhost,127.0.0.1 and any direct-connect hosts."
      : systemProxy
        ? `Bubble is routing this request through the OS system proxy at ${systemProxy} (detected automatically). Check that the proxy app is running and healthy, or set BUBBLE_SYSTEM_PROXY=0 to disable system proxy detection.`
        : "If your network requires a proxy, set HTTPS_PROXY or HTTP_PROXY, and set NO_PROXY=localhost,127.0.0.1.",
    hasCustomCaEnv(env)
      ? "A custom CA environment variable is configured."
      : "No custom CA environment variable is configured.",
    "Do not disable TLS verification with NODE_TLS_REJECT_UNAUTHORIZED=0.",
    `Original error: ${firstMeaningfulErrorMessage(error) || "unknown network error"}`,
  ].filter(Boolean).join(" ");

  return new Error(message, { cause: error });
}

export function isProviderTransportError(error: unknown): boolean {
  const text = errorMessageChain(error).join("\n");
  return isProviderNetworkErrorText(text) || isProviderTimeoutErrorText(text);
}

/**
 * Request/response timeouts surface as prose rather than errno tokens — e.g.
 * Bun fetch throws a DOMException named "TimeoutError" with message
 * "The operation timed out.", and openai-node raises APIConnectionTimeoutError.
 * These are kept OUT of isProviderNetworkErrorText on purpose: that predicate
 * drives normalizeProviderNetworkError's proxy/TLS/CA advice, and a plain
 * timeout must not be rewrapped into a misleading "check your proxy" message.
 */
export function isProviderTimeoutErrorText(text: string): boolean {
  return [
    /operation timed out/i,
    /request timed out/i,
    /\bTimeoutError\b/i,
    /\bAPIConnectionTimeoutError\b/i,
    /\bESOCKETTIMEDOUT\b/i,
  ].some((pattern) => pattern.test(text));
}

export function shouldEnableFetchVerbose(env: NodeJS.ProcessEnv = process.env, providerVerboseEnvVar?: string): boolean {
  const providerValue = providerVerboseEnvVar ? env[providerVerboseEnvVar] : undefined;
  return isTruthyEnv(providerValue) || isTruthyEnv(env.BUBBLE_PROVIDER_FETCH_VERBOSE);
}

function hasProxyEnv(env: NodeJS.ProcessEnv): boolean {
  return Boolean(env.HTTPS_PROXY || env.https_proxy || env.HTTP_PROXY || env.http_proxy || env.ALL_PROXY || env.all_proxy);
}

function hasCustomCaEnv(env: NodeJS.ProcessEnv): boolean {
  return Boolean(env.NODE_EXTRA_CA_CERTS?.trim() || env.BUBBLE_EXTRA_CA_CERTS?.trim());
}

function nodeProxyForUrl(input: RequestInfo | URL, env: NodeJS.ProcessEnv): string | undefined {
  const url = urlFromInput(input);
  if (!url || shouldBypassProxy(url, env)) return undefined;
  if (url.protocol === "https:") return env.HTTPS_PROXY ?? env.https_proxy ?? env.ALL_PROXY ?? env.all_proxy ?? getSystemProxyForUrl(url, env);
  if (url.protocol === "http:") return env.HTTP_PROXY ?? env.http_proxy ?? env.ALL_PROXY ?? env.all_proxy ?? getSystemProxyForUrl(url, env);
  return undefined;
}

function defaultNodeProxy(env: NodeJS.ProcessEnv): string | undefined {
  return env.HTTPS_PROXY ?? env.https_proxy ?? env.HTTP_PROXY ?? env.http_proxy ?? env.ALL_PROXY ?? env.all_proxy
    ?? getSystemProxyForUrl(new URL("https://system-proxy-default.invalid/"), env);
}

function urlFromInput(input: RequestInfo | URL): URL | undefined {
  if (input instanceof URL) return input;
  if (typeof input === "string") return URL.canParse(input) ? new URL(input) : undefined;
  const url = (input as Request).url;
  return URL.canParse(url) ? new URL(url) : undefined;
}

function originFromInput(input: RequestInfo | URL | undefined): string | undefined {
  if (!input) return undefined;
  return urlFromInput(input)?.origin;
}

function shouldBypassProxy(url: URL, env: NodeJS.ProcessEnv): boolean {
  const noProxy = (env.NO_PROXY ?? env.no_proxy ?? "").trim();
  if (!noProxy) return false;
  if (noProxy === "*") return true;
  const hostname = url.hostname.toLowerCase();
  const port = url.port;
  return noProxy
    .split(/[,\s]+/)
    .filter(Boolean)
    .some((entry) => noProxyEntryMatches(entry.toLowerCase(), hostname, port));
}

function noProxyEntryMatches(entry: string, hostname: string, port: string): boolean {
  const [entryHost, entryPort] = entry.includes(":") ? entry.split(":") : [entry, ""];
  if (entryPort && entryPort !== port) return false;
  if (entryHost === hostname) return true;
  if (entryHost.startsWith("*.")) return hostname.endsWith(entryHost.slice(1));
  if (entryHost.startsWith(".")) return hostname.endsWith(entryHost);
  return false;
}

function loadExtraCaCertificates(env: NodeJS.ProcessEnv, providerName: string): string[] {
  const paths = extraCaCertificatePaths(env);
  return paths.map((path) => {
    try {
      return readFileSync(path, "utf-8");
    } catch (error) {
      throw new Error(`Failed to read ${providerName} custom CA certificate at ${path}. Check NODE_EXTRA_CA_CERTS or BUBBLE_EXTRA_CA_CERTS.`, {
        cause: error,
      });
    }
  });
}

function extraCaCertificatePaths(env: NodeJS.ProcessEnv): string[] {
  const bubbleValue = env.BUBBLE_EXTRA_CA_CERTS?.trim();
  if (bubbleValue) {
    return bubbleValue.split(delimiter).map((item) => item.trim()).filter(Boolean);
  }

  const nodeValue = env.NODE_EXTRA_CA_CERTS?.trim();
  return nodeValue ? [nodeValue] : [];
}

function isProviderNetworkErrorText(text: string): boolean {
  return [
    /fetch failed/i,
    /network.*failed/i,
    /socket connection was closed unexpectedly/i,
    /\bConnectionClosed\b/i,
    /\bECONNRESET\b/i,
    /\bECONNREFUSED\b/i,
    /\bETIMEDOUT\b/i,
    /\bEPIPE\b/i,
    /\bUND_ERR_/i,
    /socket hang up/i,
    /Unable to connect\. Is the computer able to access the url\?/i,
    /certificate/i,
    /unable to verify/i,
    /self[- ]signed/i,
  ].some((pattern) => pattern.test(text));
}

function isCertificateErrorText(text: string): boolean {
  return [
    /unknown certificate verification error/i,
    /certificate (?:verify|verification) (?:failed|error)/i,
    /unable to verify (?:the )?(?:first )?certificate/i,
    /UNABLE_TO_(?:VERIFY_LEAF_SIGNATURE|GET_ISSUER_CERT_LOCALLY)/i,
    /SELF_SIGNED_CERT_IN_CHAIN/i,
    /DEPTH_ZERO_SELF_SIGNED_CERT/i,
    /CERT_(?:HAS_EXPIRED|UNTRUSTED|INVALID)/i,
    /self[- ]signed certificate/i,
  ].some((pattern) => pattern.test(text));
}

function firstMeaningfulErrorMessage(error: unknown): string | undefined {
  return errorMessageChain(error).find((item) => item && item !== "Error");
}

function errorMessageChain(error: unknown): string[] {
  const messages: string[] = [];
  let current: unknown = error;
  for (let depth = 0; current && depth < 8; depth++) {
    if (current instanceof Error) {
      messages.push(current.name, current.message);
      // Node/undici stores errno on Error.code, including nested causes such
      // as TypeError("terminated") -> SocketError { code: "UND_ERR_SOCKET" }.
      const codedError = current as Error & { code?: unknown; cause?: unknown };
      if (typeof codedError.code === "string") messages.push(codedError.code);
      current = codedError.cause;
      continue;
    }
    if (typeof current === "object") {
      const record = current as Record<string, unknown>;
      for (const key of ["name", "code", "message"]) {
        if (typeof record[key] === "string") messages.push(record[key]);
      }
      current = record.cause;
      continue;
    }
    messages.push(String(current));
    break;
  }
  return messages;
}

function isTruthyEnv(value: string | undefined): boolean {
  return /^(1|true|yes)$/i.test(value ?? "");
}
