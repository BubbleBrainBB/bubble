import { readFileSync } from "node:fs";
import { delimiter } from "node:path";
import type { Dispatcher } from "undici";
import { providerDispatcher } from "./provider-dispatcher.js";
import { resolveIdleTimeoutMs } from "./request-timeout.js";
import { getSystemProxyForUrl } from "./system-proxy.js";

export type ChatGptFetch = (input: RequestInfo | URL, init?: RequestInit) => Promise<Response>;

export interface ChatGptFetchOptions {
  fetch?: ChatGptFetch;
  env?: NodeJS.ProcessEnv;
}

type RequestInitWithDispatcher = RequestInit & { dispatcher?: Dispatcher };

let cachedDefaultFetch: { signature: string; fetch: ChatGptFetch } | undefined;

export function chatGptFetch(input: RequestInfo | URL, init?: RequestInit): Promise<Response> {
  return getChatGptFetch()(input, init);
}

export function getChatGptFetch(env: NodeJS.ProcessEnv = process.env): ChatGptFetch {
  const signature = networkEnvSignature(env);
  if (!cachedDefaultFetch || cachedDefaultFetch.signature !== signature) {
    cachedDefaultFetch = {
      signature,
      fetch: createChatGptFetch({ env }),
    };
  }
  return cachedDefaultFetch.fetch;
}

export function createChatGptFetch(options: ChatGptFetchOptions = {}): ChatGptFetch {
  const env = options.env ?? process.env;
  const fetchImpl = options.fetch ?? ((input, init) => globalThis.fetch(input, init));
  const dispatcher = createChatGptDispatcher(env);

  return async (input, init) => {
    const requestInit = withChatGptNetworkOptions(input, init, env, dispatcher);
    try {
      return await fetchImpl(input, requestInit);
    } catch (error) {
      throw normalizeChatGptNetworkError(error, env);
    }
  };
}

export function createChatGptDispatcher(
  env: NodeJS.ProcessEnv = process.env,
  input?: RequestInfo | URL,
): Dispatcher {
  return providerDispatcher({
    proxy: input ? nodeProxyForUrl(input, env) : defaultNodeProxy(env),
    ca: loadExtraCaCertificates(env),
    idleTimeoutMs: resolveIdleTimeoutMs(env),
  });
}

export function withChatGptNetworkOptions(
  input: RequestInfo | URL,
  init: RequestInit | undefined,
  env: NodeJS.ProcessEnv = process.env,
  dispatcher = createChatGptDispatcher(env, input),
): RequestInitWithDispatcher {
  return { ...(init ?? {}), dispatcher } as RequestInitWithDispatcher;
}

export function normalizeChatGptNetworkError(error: unknown, env: NodeJS.ProcessEnv = process.env): Error {
  // Already normalized — wrapping again would nest "Original error:" messages.
  if (error instanceof Error && error.message.includes("connection failed before Bubble received a response")) {
    return error;
  }
  const text = errorMessageChain(error).join("\n");
  if (!isChatGptNetworkErrorText(text)) {
    return error instanceof Error ? error : new Error(String(error));
  }

  const systemProxy = hasProxyEnv(env) ? undefined : getSystemProxyForUrl(new URL("https://chatgpt.com/"), env);
  const message = [
    "ChatGPT connection failed before Bubble received a response.",
    isCertificateErrorText(text)
      ? "TLS certificate verification failed. If you are on a corporate proxy, VPN, or HTTPS inspection network, start Bubble with NODE_EXTRA_CA_CERTS=/absolute/path/to/ca.pem or BUBBLE_EXTRA_CA_CERTS=/absolute/path/to/ca.pem."
      : "This looks like a proxy or network transport failure.",
    hasProxyEnv(env)
      ? "Bubble is using proxy environment variables for ChatGPT requests. Make sure NO_PROXY includes localhost,127.0.0.1."
      : systemProxy
        ? `Bubble is routing this request through the OS system proxy at ${systemProxy} (detected automatically). Check that the proxy app is running and healthy, or set BUBBLE_SYSTEM_PROXY=0 to disable system proxy detection.`
        : "If your network requires a proxy, set HTTPS_PROXY or HTTP_PROXY, and set NO_PROXY=localhost,127.0.0.1.",
    "Do not disable TLS verification with NODE_TLS_REJECT_UNAUTHORIZED=0.",
    `Original error: ${firstMeaningfulErrorMessage(error) || "unknown network error"}`,
  ].join(" ");

  return new Error(message, { cause: error });
}

function hasProxyEnv(env: NodeJS.ProcessEnv): boolean {
  return Boolean(
    env.BUBBLE_CHATGPT_PROXY || env.bubble_chatgpt_proxy
    || env.HTTPS_PROXY || env.https_proxy || env.HTTP_PROXY || env.http_proxy || env.ALL_PROXY || env.all_proxy,
  );
}

function chatGptProxyOverride(env: NodeJS.ProcessEnv): string | undefined {
  return env.BUBBLE_CHATGPT_PROXY ?? env.bubble_chatgpt_proxy;
}

function nodeProxyForUrl(input: RequestInfo | URL, env: NodeJS.ProcessEnv): string | undefined {
  const url = urlFromInput(input);
  if (!url || shouldBypassProxy(url, env)) return undefined;
  const override = chatGptProxyOverride(env);
  if (override) return override;
  if (url.protocol === "https:") return env.HTTPS_PROXY ?? env.https_proxy ?? env.ALL_PROXY ?? env.all_proxy ?? getSystemProxyForUrl(url, env);
  if (url.protocol === "http:") return env.HTTP_PROXY ?? env.http_proxy ?? env.ALL_PROXY ?? env.all_proxy ?? getSystemProxyForUrl(url, env);
  return defaultNodeProxy(env);
}

function defaultNodeProxy(env: NodeJS.ProcessEnv): string | undefined {
  return chatGptProxyOverride(env)
    ?? env.HTTPS_PROXY ?? env.https_proxy ?? env.HTTP_PROXY ?? env.http_proxy ?? env.ALL_PROXY ?? env.all_proxy
    ?? getSystemProxyForUrl(new URL("https://system-proxy-default.invalid/"), env);
}

function urlFromInput(input: RequestInfo | URL): URL | undefined {
  if (input instanceof URL) return input;
  if (typeof input === "string") return URL.canParse(input) ? new URL(input) : undefined;
  const url = (input as Request).url;
  return URL.canParse(url) ? new URL(url) : undefined;
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

function loadExtraCaCertificates(env: NodeJS.ProcessEnv): string[] {
  const paths = extraCaCertificatePaths(env);
  return paths.map((path) => {
    try {
      return readFileSync(path, "utf-8");
    } catch (error) {
      throw new Error(`Failed to read ChatGPT custom CA certificate at ${path}. Check NODE_EXTRA_CA_CERTS or BUBBLE_EXTRA_CA_CERTS.`, {
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

function networkEnvSignature(env: NodeJS.ProcessEnv): string {
  return [
    env.BUBBLE_CHATGPT_PROXY,
    env.bubble_chatgpt_proxy,
    env.HTTP_PROXY,
    env.http_proxy,
    env.HTTPS_PROXY,
    env.https_proxy,
    env.ALL_PROXY,
    env.all_proxy,
    env.NO_PROXY,
    env.no_proxy,
    env.NODE_EXTRA_CA_CERTS,
    env.BUBBLE_EXTRA_CA_CERTS,
    env.BUBBLE_PROVIDER_REQUEST_TIMEOUT_MS,
    // Invalidate the cached fetch when the user toggles the OS proxy
    // (e.g. turning Clash system proxy on/off mid-session).
    getSystemProxyForUrl(new URL("https://chatgpt.com/"), env),
  ].join("\0");
}

function isChatGptNetworkErrorText(text: string): boolean {
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
      current = (current as Error & { cause?: unknown }).cause;
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
