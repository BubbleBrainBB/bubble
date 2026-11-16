/**
 * How long Bubble waits on a provider, from the one setting a person has for
 * it: BUBBLE_PROVIDER_REQUEST_TIMEOUT_MS.
 */

/**
 * Largest value that survives the OpenAI SDK's internal `timeout + 1000`
 * agent-grace (core.js minAgentTimeout) without overflowing Node's 32-bit
 * timers; ~24.8 days.
 */
export const MAX_TIMER_MS = 2_147_482_647; // 2**31 - 1 - 1000

/**
 * Resolve the provider request timeout (ms) from the operator override.
 *
 * Default is effectively NO TIMEOUT — safe for streaming APIs where the model
 * sends chunks continuously. But Node's timers are 32-bit: a duration above
 * 2**31-1 ms overflows, which makes Node print a TimeoutOverflowWarning to
 * stderr (corrupting the Ink TUI) AND silently clamp the timeout to 1ms,
 * aborting the request almost immediately. So we use the largest SAFE timer
 * value as the "no timeout" sentinel — never Number.MAX_SAFE_INTEGER — and
 * clamp any operator-supplied value into range too.
 */
export function resolveRequestTimeoutMs(raw: string | undefined): number {
  const requested = parsePositiveInt(raw);
  return Math.min(requested ?? MAX_TIMER_MS, MAX_TIMER_MS);
}

/**
 * The longest a provider may stay silent: before its response headers, and
 * between two pieces of a body. 0 means no limit.
 *
 * Node's fetch cuts both at 300 seconds. A model that thinks, or writes a
 * long tool call, in silence for longer than that is not a broken connection:
 * cutting it fails the reply, and the retry waits the same five minutes
 * again. So the limit is the request timeout the person set, and there is
 * none when they set none.
 */
export function resolveIdleTimeoutMs(env: NodeJS.ProcessEnv = process.env): number {
  const requestTimeoutMs = resolveRequestTimeoutMs(env.BUBBLE_PROVIDER_REQUEST_TIMEOUT_MS);
  return requestTimeoutMs >= MAX_TIMER_MS ? 0 : requestTimeoutMs;
}

/**
 * How long a one-shot completion may take, start to finish, when the person
 * set no request timeout. Node's fetch used to cut such a call after 300
 * seconds of silence; this is a limit on the whole call instead, so one that
 * keeps receiving data past it is ended too.
 */
export const DEFAULT_COMPLETION_DEADLINE_MS = 300_000;

/**
 * The deadline for a one-shot completion whose caller cannot cancel it
 * (titles, memory).
 *
 * A streamed reply is watched and can be stopped, so it may stay silent as
 * long as the model needs. These calls run unseen: without a deadline, a
 * connection that stays open and sends nothing holds them forever. They get
 * the request timeout the person set, and a default when they set none.
 */
export function resolveCompletionDeadlineMs(env: NodeJS.ProcessEnv = process.env): number {
  return resolveIdleTimeoutMs(env) || DEFAULT_COMPLETION_DEADLINE_MS;
}

function parsePositiveInt(raw: string | undefined): number | undefined {
  if (!raw?.trim()) return undefined;
  const value = Number(raw);
  return Number.isInteger(value) && value > 0 ? value : undefined;
}
