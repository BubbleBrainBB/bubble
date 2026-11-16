/**
 * Shared retry policy for provider transports.
 *
 * Connection-level failures (nothing received yet) and retryable HTTP
 * statuses are retried inside the provider with exponential backoff.
 * Mid-stream interruptions (content already surfaced to the UI) are
 * signalled with ProviderStreamInterruptedError so the agent loop can
 * discard the partial assistant message and re-issue the whole request.
 */

const DEFAULT_MAX_RETRIES = 4;
const MAX_CONFIGURABLE_RETRIES = 10;
const BASE_DELAY_MS = 1000;
const MAX_DELAY_MS = 32_000;
const MAX_RETRY_AFTER_MS = 60_000;

export const MAX_STREAM_INTERRUPTION_RETRIES = 10;

/**
 * A reply cut by the idle limit gets one more try, for the case where the
 * connection stalled. A model that is simply working past the limit hits it
 * again on every identical request, so each further retry only adds one more
 * full wait before the person hears about it.
 */
const MAX_IDLE_TIMEOUT_RETRIES = 1;

const IDLE_TIMEOUT_CODES = new Set(["UND_ERR_HEADERS_TIMEOUT", "UND_ERR_BODY_TIMEOUT"]);

const RETRYABLE_HTTP_STATUSES = new Set([408, 429, 500, 502, 503, 504, 529]);

export class ProviderStreamInterruptedError extends Error {
  readonly isProviderStreamInterruption = true;
  readonly retryAfterMs?: number;
  readonly maxRetries?: number;

  constructor(message: string, options?: { cause?: unknown; retryAfterMs?: number; maxRetries?: number }) {
    super(message, options);
    this.name = "ProviderStreamInterruptedError";
    this.retryAfterMs = options?.retryAfterMs;
    this.maxRetries = options?.maxRetries;
  }
}

export function isProviderStreamInterruption(error: unknown): error is ProviderStreamInterruptedError {
  return !!error
    && typeof error === "object"
    && (error as { isProviderStreamInterruption?: unknown }).isProviderStreamInterruption === true;
}

export function providerStreamRetryLimit(error: ProviderStreamInterruptedError): number {
  if (error.maxRetries === undefined) return retryLimitFor(error, MAX_STREAM_INTERRUPTION_RETRIES);
  return Math.max(0, Math.min(MAX_CONFIGURABLE_RETRIES, Math.floor(error.maxRetries)));
}

/**
 * True when the provider stayed silent past the idle limit: no response
 * headers, or no further piece of a body, in time. The transport reports it
 * as a coded error somewhere under whatever wrapped it.
 */
export function isResponseIdleTimeout(error: unknown): boolean {
  let current: unknown = error;
  for (let depth = 0; current && typeof current === "object" && depth < 8; depth++) {
    const { code, cause } = current as { code?: unknown; cause?: unknown };
    if (typeof code === "string" && IDLE_TIMEOUT_CODES.has(code)) return true;
    current = cause;
  }
  return false;
}

/** How many times a failed request may be re-issued, given a budget of `maxRetries`. */
export function retryLimitFor(error: unknown, maxRetries: number): number {
  return isResponseIdleTimeout(error) ? Math.min(maxRetries, MAX_IDLE_TIMEOUT_RETRIES) : maxRetries;
}

export function getProviderMaxRetries(env: NodeJS.ProcessEnv = process.env): number {
  const raw = env.BUBBLE_PROVIDER_MAX_RETRIES?.trim();
  if (!raw) return DEFAULT_MAX_RETRIES;
  const value = Number(raw);
  if (!Number.isInteger(value) || value < 0) return DEFAULT_MAX_RETRIES;
  return Math.min(value, MAX_CONFIGURABLE_RETRIES);
}

export function isRetryableHttpStatus(status: number): boolean {
  return RETRYABLE_HTTP_STATUSES.has(status);
}

/**
 * Equal-jitter exponential backoff: attempt 1 → 0.5-1s, 2 → 1-2s, 3 → 2-4s,
 * 4 → 4-8s, capped at 32s. A retry-after hint from the server wins (capped
 * at 60s) since it reflects actual load shedding.
 */
export function computeRetryDelayMs(attempt: number, options?: { retryAfterMs?: number }): number {
  if (process.env.NODE_ENV === "test") return 0;
  if (options?.retryAfterMs !== undefined && options.retryAfterMs >= 0) {
    return Math.min(options.retryAfterMs, MAX_RETRY_AFTER_MS);
  }
  const cap = Math.min(MAX_DELAY_MS, BASE_DELAY_MS * 2 ** Math.max(0, attempt - 1));
  return Math.floor(cap / 2 + Math.random() * (cap / 2));
}

export function retryAfterMsFromResponse(response: Response): number | undefined {
  const header = response.headers.get("retry-after")?.trim();
  if (!header) return undefined;
  const seconds = Number(header);
  if (Number.isFinite(seconds) && seconds >= 0) return Math.round(seconds * 1000);
  const date = Date.parse(header);
  if (!Number.isNaN(date)) return Math.max(0, date - Date.now());
  return undefined;
}

export function sleepBeforeRetry(ms: number, signal?: AbortSignal): Promise<void> {
  if (signal?.aborted) return Promise.reject(toAbortError(signal));
  return new Promise((resolve, reject) => {
    const onAbort = () => {
      clearTimeout(timeout);
      reject(toAbortError(signal));
    };
    const timeout = setTimeout(() => {
      signal?.removeEventListener("abort", onAbort);
      resolve();
    }, ms);
    signal?.addEventListener("abort", onAbort, { once: true });
  });
}

function toAbortError(signal?: AbortSignal): Error {
  if (signal?.reason instanceof Error) return signal.reason;
  const error = new Error(typeof signal?.reason === "string" ? signal.reason : "Request retry aborted.");
  error.name = "AbortError";
  return error;
}
