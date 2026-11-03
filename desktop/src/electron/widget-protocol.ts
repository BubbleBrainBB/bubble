// `bubble-widget://` serves generative-UI widgets: a fixed shell page the chat
// loads in a sandboxed iframe (allow-scripts only, so an opaque origin with no
// access to the app), and the chart libraries a widget may load. The widget's
// own HTML never travels in a URL: the chat posts it to the shell, which
// renders it under the shell's CSP (no network, no forms, no navigation).

import { app, clipboard, protocol, type WebContents } from 'electron';
import { ipcMainHandle } from './util';
import * as fs from 'fs';
import * as path from 'path';
import { WIDGET_DESIGN_CSS, WIDGET_SVG_DEFS } from './widget-design-system';
import {
  WIDGET_COPY_MAX_CHARS,
  WIDGET_FOCUS_BLINK_FEATURE,
  WIDGET_MESSAGE_SOURCE,
  WIDGET_PROMPT_MAX_CHARS,
  WIDGET_SCHEME,
  WIDGET_STATE_MAX_CHARS,
  type WidgetLibraryName,
} from '../shared/widget';

const LIBRARY_FILES: Record<WidgetLibraryName, [pkg: string, file: string]> = {
  'chart.js': ['chart.js', 'dist/chart.umd.min.js'],
  'd3.js': ['d3', 'dist/d3.min.js'],
  'echarts.js': ['echarts', 'dist/echarts.min.js'],
};

export const WIDGET_FRAME_CSP = [
  "default-src 'none'",
  `script-src 'unsafe-inline' 'unsafe-eval' ${WIDGET_SCHEME}:`,
  "style-src 'unsafe-inline'",
  'img-src data: blob:',
  'font-src data:',
  'media-src data: blob:',
  "connect-src 'none'",
  "frame-src 'none'",
  'worker-src blob:',
  "form-action 'none'",
  "base-uri 'none'",
].join('; ');

// dist-electron/electron/ sits two levels under the app root, in dev and in
// the packaged app.asar alike; the app path is a fallback.
async function readLibrary(pkg: string, file: string): Promise<Buffer> {
  const roots = [path.join(__dirname, '..', '..'), app.getAppPath()];
  let lastError: unknown = null;
  for (const base of roots) {
    try {
      return await fs.promises.readFile(path.join(base, 'node_modules', pkg, file));
    } catch (error) {
      lastError = error;
    }
  }
  throw lastError;
}

/** Copying needs the main process: the page clipboard needs document focus, which may sit in a widget frame. */
export function registerWidgetIpc(): void {
  ipcMainHandle('widget:copy-text', (_event, text: unknown) => {
    clipboard.writeText(String(text ?? '').slice(0, WIDGET_COPY_MAX_CHARS));
    return true;
  });
}

/**
 * Must run before `app` is ready. Also enables the Blink feature behind the
 * frames' focus-without-user-activation policy: without it a widget could
 * focus itself while the user types in the composer and take their key
 * presses.
 */
export function registerWidgetSchemePrivileged(): void {
  protocol.registerSchemesAsPrivileged([
    { scheme: WIDGET_SCHEME, privileges: { standard: true, secure: true } },
  ]);
  const features = app.commandLine.getSwitchValue('enable-blink-features').split(',').filter(Boolean);
  if (!features.includes(WIDGET_FOCUS_BLINK_FEATURE)) {
    app.commandLine.appendSwitch('enable-blink-features', [...features, WIDGET_FOCUS_BLINK_FEATURE].join(','));
  }
}

export function installWidgetProtocol(): void {
  protocol.handle(WIDGET_SCHEME, async (request) => {
    const url = new URL(request.url);
    if (url.host === 'frame') {
      return new Response(WIDGET_SHELL_HTML, {
        headers: {
          'Content-Type': 'text/html; charset=utf-8',
          'Content-Security-Policy': WIDGET_FRAME_CSP,
          'Cache-Control': 'no-store',
        },
      });
    }
    if (url.host === 'lib') {
      const name = url.pathname.replace(/^\//, '') as WidgetLibraryName;
      const entry = LIBRARY_FILES[name];
      if (entry) {
        try {
          const body = await readLibrary(entry[0], entry[1]);
          return new Response(new Uint8Array(body), {
            headers: { 'Content-Type': 'text/javascript; charset=utf-8', 'Cache-Control': 'max-age=86400' },
          });
        } catch (error) {
          console.warn(`[widget] failed to load library ${name}:`, error);
        }
      }
    }
    return new Response('Not found', { status: 404 });
  });
}

/**
 * A widget frame stays on its shell: a script setting `location` (or a link
 * the shell didn't catch) must not turn the chat into an arbitrary web page.
 */
export function guardWidgetFrameNavigation(contents: WebContents): void {
  contents.on('will-frame-navigate', (details) => {
    if (details.isMainFrame) return;
    const current = details.frame?.url ?? '';
    if (current.startsWith(`${WIDGET_SCHEME}:`) && !details.url.startsWith(`${WIDGET_SCHEME}:`)) {
      details.preventDefault();
    }
  });
}

// ── Shell ───────────────────────────────────────────────────────────────────

const BOOTSTRAP = String.raw`
(() => {
  const SOURCE = ${JSON.stringify(WIDGET_MESSAGE_SOURCE)};
  const PROMPT_MAX = ${WIDGET_PROMPT_MAX_CHARS};
  const STATE_MAX = ${WIDGET_STATE_MAX_CHARS};
  const root = document.getElementById('bubble-root');
  // Held from the start: a widget script can shadow window.parent, not this.
  const host = window.parent;
  const post = (message) => host.postMessage(Object.assign({ source: SOURCE }, message), '*');
  let state = null;
  // 'empty' → 'streaming' (partial markup, no scripts) → 'rendered' (final, scripts ran).
  let phase = 'empty';

  // Actions that leave the widget start from a real click or key press. The
  // chat doesn't rely on this (a script can post to it directly): it shows
  // each request under the widget and acts only on the user's confirmation.
  const activated = () => !navigator.userActivation || navigator.userActivation.isActive;

  const bubble = {
    sendPrompt(text) {
      if (!activated()) { console.warn('bubble.sendPrompt needs a user click.'); return false; }
      const value = String(text ?? '').trim().slice(0, PROMPT_MAX);
      if (!value) return false;
      post({ type: 'prompt', text: value });
      return true;
    },
    openLink(url) {
      if (!activated()) { console.warn('bubble.openLink needs a user click.'); return false; }
      const value = String(url ?? '');
      if (!/^https?:\/\//i.test(value)) return false;
      post({ type: 'open-link', url: value });
      return true;
    },
    copy(text) {
      if (!activated()) { console.warn('bubble.copy needs a user click.'); return false; }
      post({ type: 'copy', text: String(text ?? '') });
      return true;
    },
    getState() { return state; },
    setState(value) {
      let json;
      try { json = JSON.stringify(value ?? null); } catch { return false; }
      if (json.length > STATE_MAX) { console.warn('bubble.setState: state too large.'); return false; }
      state = JSON.parse(json);
      post({ type: 'state', state });
      return true;
    },
  };
  Object.defineProperty(window, 'bubble', { value: Object.freeze(bubble) });

  // The sandbox blocks the page clipboard; widgets often write the standard
  // calls anyway, so route them through the app.
  const copyThroughApp = (text) => bubble.copy(text)
    ? Promise.resolve()
    : Promise.reject(new DOMException('Copying needs a user click.', 'NotAllowedError'));
  try {
    if (navigator.clipboard) Object.defineProperty(navigator.clipboard, 'writeText', { value: copyThroughApp, configurable: true });
    else Object.defineProperty(navigator, 'clipboard', { value: { writeText: copyThroughApp }, configurable: true });
  } catch {}
  const execCommand = document.execCommand.bind(document);
  document.execCommand = (command, ...rest) => {
    if (String(command).toLowerCase() !== 'copy') return execCommand(command, ...rest);
    const active = document.activeElement;
    const text = active && (active instanceof HTMLTextAreaElement || active instanceof HTMLInputElement)
      ? active.value.slice(active.selectionStart ?? 0, active.selectionEnd ?? active.value.length)
      : String(window.getSelection() || '');
    if (!text) return false;
    return bubble.copy(text);
  };
  // Common spelling in generative-UI prompts.
  window.sendPrompt = bubble.sendPrompt;

  document.addEventListener('click', (event) => {
    const link = event.target instanceof Element ? event.target.closest('a[href]') : null;
    if (!link) return;
    const href = link.getAttribute('href') || '';
    if (href.startsWith('#')) return;
    event.preventDefault();
    bubble.openLink(link.href);
  }, true);

  const applyTheme = (theme) => {
    if (!theme) return;
    const style = document.documentElement.style;
    for (const [name, value] of Object.entries(theme.variables || {})) style.setProperty(name, value);
    document.documentElement.classList.toggle('dark', !!theme.dark);
    document.documentElement.style.colorScheme = theme.dark ? 'dark' : 'light';
    if (phase === 'rendered') window.dispatchEvent(new CustomEvent('bubble-theme', { detail: { dark: !!theme.dark } }));
  };

  let lastHeight = -1;
  // The tallest the content has been while CSS motion runs, so an animation
  // that grows and shrinks it (an endless one included) keeps the card at
  // full height instead of clipping it or resizing the chat every frame.
  let motionPeak = 0;
  const measure = () => Math.ceil(Math.max(root.scrollHeight, root.getBoundingClientRect().height));
  const reportSize = () => {
    // The empty shell's 0px would overwrite the widget's remembered height.
    if (phase === 'empty') return;
    // The root contains its absolutely positioned content (tooltips, labels),
    // so scrollHeight counts what spills out of the normal flow; missing it
    // leaves a few pixels to scroll, i.e. a scrollbar beside the widget.
    const height = Math.max(measure(), motionPeak);
    // A rendered widget never measures 0 except while its frame is torn down.
    if (height <= 0) return;
    if (height !== lastHeight) { lastHeight = height; post({ type: 'size', height }); }
  };
  // New content sizes the card afresh.
  const contentChanged = () => { motionPeak = 0; reportSize(); };
  // A width change alone is not new content: a classic scrollbar coming and
  // going as motion overflows the frame flips the width, and must not drop the
  // peak. Content the new width reflows changes the height.
  let rootHeight = -1;
  new ResizeObserver(() => {
    const height = root.getBoundingClientRect().height;
    if (height === rootHeight) { reportSize(); return; }
    rootHeight = height;
    contentChanged();
  }).observe(root);
  // Absolutely positioned content (a tooltip appearing, a label moving) changes
  // the page height without resizing the root.
  let sizeFrame = 0;
  new MutationObserver(() => {
    cancelAnimationFrame(sizeFrame);
    sizeFrame = requestAnimationFrame(contentChanged);
  }).observe(root, { childList: true, subtree: true, attributes: true, characterData: true });
  // CSS alone can move content too, with no DOM change and no resize of the
  // root: a :hover or :focus rule, a transition, a keyframe animation. When
  // something starts, measure each frame until the style settles, finite
  // motion ends and endless motion has run a while (long enough to see a full
  // cycle of most); endless motion is then sampled a few times a second.
  const SETTLE_MS = 120;
  const FULL_RATE_MS = 2000;
  const ENDLESS_SAMPLE_MS = 250;
  let followFrame = 0;
  let followTimer = 0;
  let settleUntil = 0;
  let fullRateUntil = 0;
  const step = () => {
    followFrame = 0;
    followTimer = 0;
    const now = performance.now();
    const animations = document.getAnimations().filter((animation) => animation.playState === 'running' && animation.effect);
    motionPeak = animations.length ? Math.max(motionPeak, measure()) : 0;
    reportSize();
    const finite = animations.some((animation) => Number.isFinite(animation.effect.getComputedTiming().endTime));
    if (now < settleUntil || finite || (animations.length && now < fullRateUntil)) followFrame = requestAnimationFrame(step);
    else if (animations.length) followTimer = setTimeout(step, ENDLESS_SAMPLE_MS);
  };
  const follow = () => {
    const now = performance.now();
    settleUntil = now + SETTLE_MS;
    fullRateUntil = now + FULL_RATE_MS;
    if (followTimer) { clearTimeout(followTimer); followTimer = 0; }
    if (!followFrame) followFrame = requestAnimationFrame(step);
  };
  for (const type of [
    'transitionrun', 'transitionend', 'transitioncancel', 'animationstart', 'animationend', 'animationcancel',
    'pointerover', 'pointerout', 'focusin', 'focusout', 'input', 'change',
  ]) document.addEventListener(type, follow, { capture: true, passive: true });
  // A loop restarting is not new motion: keep sampling, at the current rate.
  document.addEventListener('animationiteration', () => {
    if (!followFrame && !followTimer) followFrame = requestAnimationFrame(step);
  }, { capture: true, passive: true });

  const report = (message) => post({ type: 'error', message: String(message).slice(0, 500) });
  window.addEventListener('error', (event) => report(event.message || 'Script error'));
  window.addEventListener('unhandledrejection', (event) => report(event.reason && event.reason.message || event.reason || 'Unhandled rejection'));

  // ── Drawing ──
  // Partial HTML parses like any HTML (open elements close themselves); only
  // a tag cut mid-way is dropped. Scripts never run from a partial widget.
  const parse = (html, partial) => {
    let source = String(html || '');
    if (partial) {
      const open = source.lastIndexOf('<');
      if (open > source.lastIndexOf('>')) source = source.slice(0, open);
    }
    const template = document.createElement('template');
    template.innerHTML = source;
    const scripts = Array.from(template.content.querySelectorAll('script'));
    for (const script of scripts) script.remove();
    if (partial) {
      // A draft is inert: inline handlers wait for the finished render, which
      // syncs them back on.
      for (const element of Array.from(template.content.querySelectorAll('*'))) {
        for (const attr of Array.from(element.attributes)) if (/^on/i.test(attr.name)) element.removeAttribute(attr.name);
      }
      // SVG labels and dots the script will place have no position yet and
      // would pile up in the corner at 0,0; they appear when it runs.
      for (const element of Array.from(template.content.querySelectorAll('svg text, svg circle, svg ellipse'))) {
        const [a, b] = element.localName === 'text' ? ['x', 'y'] : ['cx', 'cy'];
        if (element.hasAttribute(a) || element.hasAttribute(b) || element.querySelector('textPath')) continue;
        if (element.closest('[transform], defs, symbol, marker, pattern, clipPath, mask')) continue;
        element.setAttribute('visibility', 'hidden');
      }
    }
    return { fragment: template.content, scripts };
  };

  root.addEventListener('animationend', (event) => {
    if (event.animationName === 'bubble-enter' && event.target instanceof Element) event.target.classList.remove('bubble-enter');
  });

  const syncAttributes = (target, source) => {
    for (const attr of Array.from(target.attributes)) if (!source.hasAttribute(attr.name)) target.removeAttribute(attr.name);
    for (const attr of Array.from(source.attributes)) if (target.getAttribute(attr.name) !== attr.value) target.setAttribute(attr.name, attr.value);
  };

  // Update the live tree in place, so what is already drawn stays put while
  // the rest streams in; new elements fade in.
  const morph = (target, source, animate) => {
    const current = Array.from(target.childNodes);
    const next = Array.from(source.childNodes);
    next.forEach((node, index) => {
      const existing = current[index];
      if (!existing) {
        if (animate && node.nodeType === 1) node.classList.add('bubble-enter');
        target.appendChild(node);
      } else if (existing.nodeType !== node.nodeType || existing.nodeName !== node.nodeName) {
        if (animate && node.nodeType === 1) node.classList.add('bubble-enter');
        target.replaceChild(node, existing);
      } else if (node.nodeType === 1) {
        const entering = existing.classList.contains('bubble-enter');
        syncAttributes(existing, node);
        if (entering) existing.classList.add('bubble-enter');
        morph(existing, node, animate);
      } else if (existing.nodeValue !== node.nodeValue) {
        existing.nodeValue = node.nodeValue;
      }
    });
    for (let index = current.length - 1; index >= next.length; index -= 1) current[index].remove();
  };

  // The bundled libraries, and how a widget can need one without loading it:
  // models often forget the tag or point it at a CDN (blocked here).
  const LIBRARIES = {
    chart: { src: ${JSON.stringify(`${WIDGET_SCHEME}://lib/chart.js`)}, url: /(?:^|\/)chart(?:\.umd)?(?:\.min)?\.js(?:$|[?#])|\/chart\.js(?:@|\/|$)/, use: /\bnew\s+Chart\s*\(|\bChart\s*\.\s*(?:register|defaults|getChart)\b/ },
    d3: { src: ${JSON.stringify(`${WIDGET_SCHEME}://lib/d3.js`)}, url: /(?:^|\/)d3(?:@[^/]*)?(?:\/|\.min\.js|\.js|$)/, use: /\bd3\s*\.\s*[a-zA-Z]/ },
    echarts: { src: ${JSON.stringify(`${WIDGET_SCHEME}://lib/echarts.js`)}, url: /echarts/, use: /\becharts\s*\.\s*[a-zA-Z]/ },
  };
  const libraryForUrl = (src) => {
    const value = String(src || '').toLowerCase();
    if (!value || value.startsWith(${JSON.stringify(`${WIDGET_SCHEME}:`)})) return null;
    for (const [name, library] of Object.entries(LIBRARIES)) if (library.url.test(value)) return name;
    return null;
  };
  const load = (src) => new Promise((resolve) => {
    const script = document.createElement('script');
    script.src = src;
    script.onload = resolve;
    script.onerror = () => { report('Failed to load ' + src); resolve(); };
    document.head.appendChild(script);
  });

  // Run the widget's scripts in order once all of its markup is in place,
  // waiting for each library to load before the scripts that use it.
  const runScripts = async (scripts) => {
    const loaded = new Set();
    for (const script of scripts) {
      const name = libraryForUrl(script.getAttribute('src'));
      if (name) script.setAttribute('src', LIBRARIES[name].src);
      if (script.getAttribute('src')) for (const [key, library] of Object.entries(LIBRARIES)) if (script.getAttribute('src') === library.src) loaded.add(key);
    }
    const inline = scripts.filter((script) => !script.getAttribute('src')).map((script) => script.textContent || '').join('\n');
    for (const [name, library] of Object.entries(LIBRARIES)) {
      if (!loaded.has(name) && library.use.test(inline)) await load(library.src);
    }
    for (const old of scripts) {
      const script = document.createElement('script');
      for (const attr of Array.from(old.attributes)) script.setAttribute(attr.name, attr.value);
      script.textContent = old.textContent;
      const pending = script.src ? new Promise((resolve) => { script.onload = resolve; script.onerror = () => { report('Failed to load ' + script.src); resolve(); }; }) : null;
      root.appendChild(script);
      if (pending) await pending;
    }
  };

  window.addEventListener('message', (event) => {
    if (event.source !== parent) return;
    const message = event.data;
    if (!message || message.source !== SOURCE) return;
    if (message.type === 'theme') { applyTheme(message.theme); return; }
    if (message.type === 'stream' && phase !== 'rendered') {
      if (phase === 'empty') applyTheme(message.theme);
      phase = 'streaming';
      morph(root, parse(message.html, true).fragment, true);
      reportSize();
      return;
    }
    if (message.type === 'render' && phase !== 'rendered') {
      const streamed = phase === 'streaming';
      phase = 'rendered';
      state = message.state ?? null;
      applyTheme(message.theme);
      const { fragment, scripts } = parse(message.html, false);
      morph(root, fragment, streamed);
      reportSize();
      runScripts(scripts).then(() => {
        document.dispatchEvent(new Event('DOMContentLoaded'));
        window.dispatchEvent(new Event('load'));
        requestAnimationFrame(reportSize);
      });
    }
  });

  post({ type: 'ready' });
})();
`;

const WIDGET_SHELL_HTML = `<!doctype html>
<html>
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<style>
  *, *::before, *::after { box-sizing: border-box; }
  html, body { margin: 0; padding: 0; background: transparent; }
  /* The chat sizes the frame to the content; only a widget taller than the
     cap scrolls, and nothing scrolls sideways. */
  html { overflow-x: hidden; scrollbar-width: thin; scrollbar-color: color-mix(in srgb, var(--text-primary, #888) 25%, transparent) transparent; }
  body {
    color: var(--text-primary, #1f2328);
    font-family: var(--font-sans, -apple-system, BlinkMacSystemFont, "Segoe UI", system-ui, sans-serif);
    font-size: 14px;
    line-height: 1.6;
    overflow-wrap: anywhere;
    -webkit-font-smoothing: antialiased;
  }
  /* The positioning container for absolutely positioned content, so its
     scrollHeight (what the chat sizes the frame to) includes it. */
  #bubble-root { display: flow-root; position: relative; }
  a { color: var(--accent, #2563eb); }
  code, pre { font-family: var(--font-mono, ui-monospace, Menlo, monospace); }
  svg { max-width: 100%; }
  canvas { max-width: 100%; }
  .bubble-enter { animation: bubble-enter .24s ease-out both; }
  @keyframes bubble-enter { from { opacity: 0; } }
</style>
<style>${WIDGET_DESIGN_CSS}</style>
</head>
<body>
${WIDGET_SVG_DEFS}
<div id="bubble-root"></div>
<script>${BOOTSTRAP}</script>
</body>
</html>`;
