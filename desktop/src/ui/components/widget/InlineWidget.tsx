import { memo, useContext, useEffect, useRef, useState } from 'react';
import { toast } from 'sonner';
import {
  WIDGET_FRAME_ALLOW,
  WIDGET_FRAME_URL,
  WIDGET_MESSAGE_SOURCE,
  readWidgetFrameMessage,
  type WidgetHostMessage,
  type WidgetTheme,
} from '../../../shared/widget';
import { ImageStudioSessionContext } from '../../lib/image-studio';
import { useAppStore } from '../../store/useAppStore';
import { useWidgetRequestStore, type WidgetRequest } from '../../store/useWidgetRequestStore';
import { useWidgetStateStore } from '../../store/useWidgetStateStore';
import { openUrlInBrowserSession } from '../../utils/html-preview';
import { rememberedHeight, rememberHeight } from '../../utils/widget-heights';
import { WidgetRequestBar } from './WidgetRequestBar';

/** Tall widgets scroll inside this height so one chart can't swallow the chat. */
const MAX_HEIGHT_PX = 720;
const MAX_VIEWPORT_SHARE = 0.8;
const INITIAL_HEIGHT_PX = 96;
/** Widgets this far outside the viewport give up their frame (state is kept). */
const KEEP_ALIVE_MARGIN = '1500px 0px';

/** The app's theme as CSS variables (theme tokens live on <html>'s inline style). */
export function readWidgetTheme(): WidgetTheme {
  const root = document.documentElement;
  const variables: Record<string, string> = {};
  for (let index = 0; index < root.style.length; index += 1) {
    const name = root.style.item(index);
    if (name.startsWith('--')) variables[name] = root.style.getPropertyValue(name).trim();
  }
  return { variables, dark: root.classList.contains('dark') };
}

/** The chat scrolls in its own container; the keep-alive margin must apply there. */
function scrollParentOf(element: HTMLElement): HTMLElement | null {
  for (let node = element.parentElement; node; node = node.parentElement) {
    const { overflowY } = getComputedStyle(node);
    if (overflowY === 'auto' || overflowY === 'scroll' || overflowY === 'overlay') return node;
  }
  return null;
}

function maxHeight(): number {
  return Math.min(MAX_HEIGHT_PX, Math.max(240, Math.round(window.innerHeight * MAX_VIEWPORT_SHARE)));
}

/** Saved widget state is written at most this often (a dragged slider sends many). */
const STATE_SAVE_INTERVAL_MS = 250;

/**
 * A show_widget call, drawn inline in the conversation: a sandboxed frame
 * (scripts only, opaque origin) running the agent's HTML under the widget
 * shell's CSP, themed like the app and sized to its content. While the call is
 * a draft, its markup is drawn in place as it grows (no scripts); once the
 * tool accepts it, the same frame finishes it and runs its scripts.
 */
export const InlineWidget = memo(function InlineWidget({
  toolUseId,
  html,
  title,
  draft = false,
}: {
  /** Keys the widget's saved state and height. */
  toolUseId: string;
  html: string;
  title?: string;
  /**
   * The call is still being written or not yet accepted: its markup is drawn
   * as it grows, scripts held back. Turning false runs it in the same frame.
   */
  draft?: boolean;
}) {
  const sessionId = useContext(ImageStudioSessionContext);
  const containerRef = useRef<HTMLDivElement | null>(null);
  const frameRef = useRef<HTMLIFrameElement | null>(null);
  const readyRef = useRef(false);
  const latestRef = useRef({ html, draft });
  latestRef.current = { html, draft };
  const [nearViewport, setNearViewport] = useState(false);
  const [contentHeight, setContentHeight] = useState(() => rememberedHeight(toolUseId) ?? INITIAL_HEIGHT_PX);
  const [limit, setLimit] = useState(maxHeight);
  const [error, setError] = useState<string | null>(null);
  const request = useWidgetRequestStore((store) => store.requests[toolUseId]);

  // Frames far off screen are dropped; the placeholder keeps their height.
  useEffect(() => {
    const element = containerRef.current;
    if (!element) return;
    const observer = new IntersectionObserver(
      ([entry]) => setNearViewport(entry.isIntersecting),
      { root: scrollParentOf(element), rootMargin: KEEP_ALIVE_MARGIN }
    );
    observer.observe(element);
    return () => observer.disconnect();
  }, []);

  useEffect(() => {
    const onResize = () => setLimit(maxHeight());
    window.addEventListener('resize', onResize);
    return () => window.removeEventListener('resize', onResize);
  }, []);

  const post = (message: WidgetHostMessage) => frameRef.current?.contentWindow?.postMessage(message, '*');
  const draw = () => {
    if (!readyRef.current) return;
    const { html: current, draft: partial } = latestRef.current;
    if (partial) {
      post({ source: WIDGET_MESSAGE_SOURCE, type: 'stream', html: current, theme: readWidgetTheme() });
    } else {
      post({
        source: WIDGET_MESSAGE_SOURCE,
        type: 'render',
        html: current,
        theme: readWidgetTheme(),
        state: useWidgetStateStore.getState().states[toolUseId]?.value ?? null,
      });
    }
  };

  // The bridge: draw on `ready`, then answer the widget's requests.
  useEffect(() => {
    if (!nearViewport) return;
    readyRef.current = false;
    // The latest state goes to storage at once, then at most once per interval.
    let pendingState: { value: unknown } | null = null;
    let stateTimer: ReturnType<typeof setTimeout> | null = null;
    let lastStateSaveAt = 0;
    const saveState = () => {
      stateTimer = null;
      if (!pendingState) return;
      lastStateSaveAt = Date.now();
      useWidgetStateStore.getState().save(toolUseId, pendingState.value);
      pendingState = null;
    };
    const onMessage = (event: MessageEvent) => {
      const frame = frameRef.current;
      if (!frame || event.source !== frame.contentWindow) return;
      const message = readWidgetFrameMessage(event.data);
      if (!message) return;
      switch (message.type) {
        case 'ready':
          setError(null);
          readyRef.current = true;
          draw();
          break;
        case 'size':
          rememberHeight(toolUseId, message.height);
          setContentHeight(message.height);
          break;
        case 'state':
          pendingState = { value: message.state };
          if (stateTimer) break;
          stateTimer = setTimeout(saveState, Math.max(0, STATE_SAVE_INTERVAL_MS - (Date.now() - lastStateSaveAt)));
          break;
        // Acting for the user waits for them: the request shows under the widget.
        case 'prompt':
          if (sessionId) useWidgetRequestStore.getState().ask(toolUseId, 'prompt', message.text);
          break;
        case 'open-link':
          if (sessionId) useWidgetRequestStore.getState().ask(toolUseId, 'open-link', message.url);
          break;
        case 'copy':
          useWidgetRequestStore.getState().ask(toolUseId, 'copy', message.text);
          break;
        case 'error':
          setError(message.message);
          break;
      }
    };
    window.addEventListener('message', onMessage);

    // Follow theme changes (variables and light/dark live on <html>).
    let frame = 0;
    const themeObserver = new MutationObserver(() => {
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(() => post({ source: WIDGET_MESSAGE_SOURCE, type: 'theme', theme: readWidgetTheme() }));
    });
    themeObserver.observe(document.documentElement, { attributes: true, attributeFilter: ['style', 'class'] });
    return () => {
      readyRef.current = false;
      if (stateTimer) clearTimeout(stateTimer);
      saveState();
      window.removeEventListener('message', onMessage);
      themeObserver.disconnect();
      cancelAnimationFrame(frame);
    };
    // draw reads the latest props through latestRef.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [nearViewport, sessionId, toolUseId]);

  // Each streamed chunk, and the finished widget, redraw the live frame.
  useEffect(draw, [html, draft]);

  const confirmRequest = (confirmed: WidgetRequest) => {
    useWidgetRequestStore.getState().settle(toolUseId, confirmed.id);
    if (confirmed.kind === 'copy') {
      void (window.electron.copyWidgetText?.(confirmed.text) ?? navigator.clipboard.writeText(confirmed.text)).then(
        () => toast.success('Copied'),
        () => toast.error('Could not copy')
      );
      return;
    }
    if (!sessionId) return;
    if (confirmed.kind === 'prompt') {
      useAppStore.getState().requestChatInjection({ sessionId, text: confirmed.text, mode: 'replace', submit: true, source: 'widget' });
      return;
    }
    void openUrlInBrowserSession({ sessionId, url: confirmed.text })
      .then(() => useAppStore.getState().openRightUtilityTab('browser', { instantReveal: true }))
      .catch((reason) => toast.error(`Could not open the link: ${reason instanceof Error ? reason.message : String(reason)}`));
  };


  const height = Math.min(contentHeight, limit);
  return (
    <div ref={containerRef} className="my-3 min-w-0" data-chat-widget={toolUseId} data-widget-draft={draft || undefined}>
      <div style={{ height }} className="relative w-full">
        {nearViewport ? (
          <iframe
            ref={frameRef}
            src={WIDGET_FRAME_URL}
            // Scripts only: an opaque origin with no access to the app, no
            // forms, popups or top-level navigation.
            sandbox="allow-scripts"
            // No focus without a click or key press in the frame: a widget
            // can't pull the user's typing out of the composer.
            allow={WIDGET_FRAME_ALLOW}
            referrerPolicy="no-referrer"
            title={title || 'Interactive widget'}
            className="block h-full w-full border-0 bg-transparent"
            style={{ colorScheme: 'normal' }}
          />
        ) : null}
      </div>
      {request && !draft ? (
        <WidgetRequestBar
          key={request.id}
          request={request}
          onConfirm={() => confirmRequest(request)}
          onDismiss={() => useWidgetRequestStore.getState().settle(toolUseId, request.id)}
        />
      ) : null}
      {error ? (
        <div className="mt-1 text-[12px] text-[var(--text-muted)]" role="status">
          Widget error: {error}
        </div>
      ) : null}
    </div>
  );
});
