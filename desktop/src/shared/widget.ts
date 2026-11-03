// Generative UI: an agent answers with a small interactive HTML widget that
// renders inline in the conversation (sandboxed, themed, no network).
//
// The agent calls `show_widget` with the widget's HTML. The tool itself only
// acknowledges; the chat renders the widget from the tool call's own input, so
// a widget replays from history exactly as it was drawn. While the call is
// still being written, the chat draws the partial HTML as it streams.

export const SHOW_WIDGET_TOOL = 'show_widget';
/** Companion tool: the design guide, loaded before drawing. */
export const WIDGET_GUIDE_TOOL = 'widget_guide';

/** Upper bound on one widget's HTML (the tool rejects anything larger). */
export const WIDGET_HTML_MAX_CHARS = 200_000;

/** Custom scheme serving the widget shell and the bundled libraries. */
export const WIDGET_SCHEME = 'bubble-widget';
export const WIDGET_FRAME_URL = `${WIDGET_SCHEME}://frame/index.html`;
/**
 * Permissions policy for a widget frame: it can't focus itself without a
 * click or key press in it, so it can't pull the user's typing out of the
 * composer. Takes effect with the Blink feature below, which the main process
 * enables.
 */
export const WIDGET_FRAME_ALLOW = "focus-without-user-activation 'none'";
export const WIDGET_FOCUS_BLINK_FEATURE = 'BlockingFocusWithoutUserActivation';

/** Libraries a widget can load with `<script src="bubble-widget://lib/<name>">`. */
export const WIDGET_LIBRARIES = {
  'chart.js': 'Chart.js 4 (global `Chart`)',
  'd3.js': 'D3 7 (global `d3`)',
  'echarts.js': 'ECharts 6 (global `echarts`)',
} as const;
export type WidgetLibraryName = keyof typeof WIDGET_LIBRARIES;

export interface ShowWidgetInput {
  title?: string;
  html: string;
}

/** Tool names arrive bare (`show_widget`) or namespaced (`mcp__x__show_widget`, `x_show_widget`, …). */
export function isShowWidgetToolName(name: unknown): boolean {
  if (typeof name !== 'string') return false;
  const normalized = name.trim().toLowerCase();
  return normalized === SHOW_WIDGET_TOOL || /[_./:-]show_widget$/.test(normalized);
}

export function isWidgetGuideToolName(name: unknown): boolean {
  if (typeof name !== 'string') return false;
  const normalized = name.trim().toLowerCase();
  return normalized === WIDGET_GUIDE_TOOL || /[_./:-]widget_guide$/.test(normalized);
}

/** The widget a show_widget call drew, or null when its input isn't one. */
export function widgetFromToolInput(input: unknown): ShowWidgetInput | null {
  if (!input || typeof input !== 'object') return null;
  const record = input as Record<string, unknown>;
  const html = typeof record.html === 'string' ? record.html : '';
  if (!html.trim()) return null;
  const title = typeof record.title === 'string' && record.title.trim() ? record.title.trim() : undefined;
  return { html, ...(title ? { title } : {}) };
}

/**
 * Marks a show_widget tool_use preview whose `html` is still being written.
 * Previews are broadcast to the chat but never persisted: the finished call
 * replaces them under the same message id.
 */
export const WIDGET_STREAMING_FLAG = '__bubbleWidgetStreaming';

export function isStreamingWidgetInput(input: unknown): boolean {
  return Boolean(input) && typeof input === 'object' && (input as Record<string, unknown>)[WIDGET_STREAMING_FLAG] === true;
}

// ── Bridge between the chat and a widget frame (postMessage) ───────────────

export const WIDGET_MESSAGE_SOURCE = 'bubble-widget';

/** Chat → widget. */
export type WidgetHostMessage =
  /** Partial HTML while the call streams: markup and styles only, no scripts. */
  | { source: typeof WIDGET_MESSAGE_SOURCE; type: 'stream'; html: string; theme: WidgetTheme }
  /** The finished widget: drawn once, then its scripts run. */
  | { source: typeof WIDGET_MESSAGE_SOURCE; type: 'render'; html: string; theme: WidgetTheme; state: unknown }
  | { source: typeof WIDGET_MESSAGE_SOURCE; type: 'theme'; theme: WidgetTheme };

/** Widget → chat. */
export type WidgetFrameMessage =
  | { source: typeof WIDGET_MESSAGE_SOURCE; type: 'ready' }
  | { source: typeof WIDGET_MESSAGE_SOURCE; type: 'size'; height: number }
  | { source: typeof WIDGET_MESSAGE_SOURCE; type: 'prompt'; text: string }
  | { source: typeof WIDGET_MESSAGE_SOURCE; type: 'open-link'; url: string }
  | { source: typeof WIDGET_MESSAGE_SOURCE; type: 'copy'; text: string }
  | { source: typeof WIDGET_MESSAGE_SOURCE; type: 'state'; state: unknown }
  | { source: typeof WIDGET_MESSAGE_SOURCE; type: 'error'; message: string };

export interface WidgetTheme {
  /** CSS custom properties (`--text-primary` …) as the app currently has them. */
  variables: Record<string, string>;
  dark: boolean;
}

/** Saved widget state (`bubble.setState`) is capped so one widget can't bloat storage. */
export const WIDGET_STATE_MAX_CHARS = 50_000;
/** Prompts a widget sends are capped, like anything a page could type. */
export const WIDGET_PROMPT_MAX_CHARS = 4_000;
export const WIDGET_COPY_MAX_CHARS = 1_000_000;
export const WIDGET_URL_MAX_CHARS = 8_192;
export const WIDGET_ERROR_MAX_CHARS = 500;

/**
 * JSON-safe copy of a widget's state within the cap, or undefined when it
 * isn't one. Size is counted while JSON.stringify walks the value, so it stops
 * at the cap: a compact value (one object referenced twice at each of a few
 * levels, which a message carries as is) would expand exponentially before a
 * check on the finished string.
 */
export function normalizeWidgetState(value: unknown): unknown {
  let size = 0;
  let json: string | undefined;
  try {
    json = JSON.stringify(value ?? null, function (this: unknown, key: string, entry: unknown) {
      // At least what this entry writes: its key in an object, a string's
      // quotes and characters, one character otherwise.
      size += 1 + (typeof entry === 'string' ? entry.length + 2 : 0) + (key && !Array.isArray(this) ? key.length + 3 : 0);
      if (size > WIDGET_STATE_MAX_CHARS) throw new RangeError('Widget state too large');
      return entry;
    });
  } catch {
    // Too large, cycles, BigInt, nesting too deep.
    return undefined;
  }
  if (json === undefined || json.length > WIDGET_STATE_MAX_CHARS) return undefined;
  return JSON.parse(json) as unknown;
}

function httpUrl(value: string): string | null {
  if (value.length > WIDGET_URL_MAX_CHARS) return null;
  try {
    const url = new URL(value);
    if (url.protocol !== 'http:' && url.protocol !== 'https:') return null;
    // A user name can pass for the host: https://trusted.example@other.example/.
    if (url.username || url.password) return null;
    return url.href;
  } catch {
    return null;
  }
}

// Controls and format characters (bidi overrides, zero-width spaces, …) can
// reorder or hide what a confirmation shows; line breaks, tabs and the
// zero-width joiner emoji are built with stay.
const HIDDEN_CHARACTER = /(?![\n\t\u200D])[\p{Cc}\p{Cf}]/gu;

/**
 * Text a widget asks the chat to send or copy, as the user should read it
 * before confirming: every hidden character drawn as its code point.
 */
export function visibleWidgetText(text: string): string {
  return text.replace(HIDDEN_CHARACTER, (character) =>
    `⟨U+${(character.codePointAt(0) ?? 0).toString(16).toUpperCase().padStart(4, '0')}⟩`);
}

/**
 * A frame's message, checked and normalized. Everything a widget sends is
 * untrusted: its scripts can post any message directly, so the shell's
 * `bubble` helpers are conveniences, and this is the boundary.
 */
export function readWidgetFrameMessage(data: unknown): WidgetFrameMessage | null {
  if (!data || typeof data !== 'object') return null;
  const record = data as Record<string, unknown>;
  if (record.source !== WIDGET_MESSAGE_SOURCE) return null;
  const source = WIDGET_MESSAGE_SOURCE;
  switch (record.type) {
    case 'ready':
      return { source, type: 'ready' };
    case 'size': {
      const height = typeof record.height === 'number' && Number.isFinite(record.height) ? Math.round(record.height) : 0;
      return height > 0 ? { source, type: 'size', height: Math.min(height, 1_000_000) } : null;
    }
    case 'prompt': {
      const text = typeof record.text === 'string' ? record.text.trim() : '';
      return text && text.length <= WIDGET_PROMPT_MAX_CHARS ? { source, type: 'prompt', text } : null;
    }
    case 'open-link': {
      const url = typeof record.url === 'string' ? httpUrl(record.url) : null;
      return url ? { source, type: 'open-link', url } : null;
    }
    case 'copy': {
      const text = typeof record.text === 'string' ? record.text : '';
      return text && text.length <= WIDGET_COPY_MAX_CHARS ? { source, type: 'copy', text } : null;
    }
    case 'state': {
      const state = normalizeWidgetState(record.state);
      return state === undefined ? null : { source, type: 'state', state };
    }
    case 'error':
      return {
        source,
        type: 'error',
        message: typeof record.message === 'string' && record.message ? record.message.slice(0, WIDGET_ERROR_MAX_CHARS) : 'Widget error',
      };
    default:
      return null;
  }
}
