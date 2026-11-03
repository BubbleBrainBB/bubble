import { useEffect, useRef, useState } from 'react';
import { visibleWidgetText } from '../../../shared/widget';
import { cn } from '@/ui/lib/utils';
import type { WidgetRequest } from '../../store/useWidgetRequestStore';
import { ArrowUp, Copy, ExternalLink, X } from '../icons';
import { iconButtonClassName } from '../ui/icon-button';

/**
 * The confirm button works once it has sat still this long: a widget can't
 * swap in a request, or move the button under the pointer (by resizing itself
 * or anything above it), just before the user clicks.
 */
export const REQUEST_STEADY_MS = 500;

const KINDS = {
  prompt: { title: 'Send to the chat', action: 'Send', Icon: ArrowUp },
  'open-link': { title: 'Open in the browser', action: 'Open', Icon: ExternalLink },
  copy: { title: 'Copy to the clipboard', action: 'Copy', Icon: Copy },
} as const;

const confirmClassName =
  'shrink-0 rounded-md border border-[var(--border)] px-2.5 py-1 text-[12px] font-medium text-[var(--text-primary)] transition-colors hover:bg-[var(--sidebar-item-hover)] disabled:cursor-not-allowed disabled:opacity-50';

/**
 * What a widget asks the chat to do for the user, shown in full under it: the
 * chat can't see input inside the widget, so it never infers that the user
 * meant it; the user's click here carries it out. Mount one per request (key
 * it by the request id), so a new request starts unconfirmable.
 */
export function WidgetRequestBar({
  request,
  onConfirm,
  onDismiss,
}: {
  request: WidgetRequest;
  onConfirm: () => void;
  onDismiss: () => void;
}) {
  const confirmRef = useRef<HTMLButtonElement | null>(null);
  // Where the confirm button is, and since when.
  const place = useRef({ at: '', since: performance.now() });
  const [armed, setArmed] = useState(false);

  const steady = () => {
    const box = confirmRef.current?.getBoundingClientRect();
    const at = box ? `${box.left},${box.top},${box.width},${box.height}` : '';
    const now = performance.now();
    if (at !== place.current.at) place.current = { at, since: now };
    return Boolean(at) && now - place.current.since >= REQUEST_STEADY_MS;
  };

  useEffect(() => {
    let frame = 0;
    const watch = () => {
      setArmed(steady());
      frame = requestAnimationFrame(watch);
    };
    frame = requestAnimationFrame(watch);
    return () => cancelAnimationFrame(frame);
    // steady reads refs only.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const { title, action, Icon } = KINDS[request.kind];
  const link = request.kind === 'open-link' ? new URL(request.text) : null;
  return (
    <div
      className="mt-1.5 rounded-md border border-[var(--border)] bg-[var(--bg-secondary)] text-[13px]"
      data-widget-request={request.kind}
      role="group"
      aria-label={`The widget asks: ${title}`}
    >
      <div className="flex items-center gap-2 py-1 pl-2.5 pr-1">
        <Icon className="h-3.5 w-3.5 shrink-0 text-[var(--text-muted)]" aria-hidden />
        <span className="min-w-0 flex-1 text-[12px] text-[var(--text-secondary)]">{title}</span>
        <button
          ref={confirmRef}
          type="button"
          className={confirmClassName}
          disabled={!armed}
          // Checked again at the click itself, not only through `disabled`.
          onClick={() => {
            if (steady()) onConfirm();
          }}
          data-widget-confirm
        >
          {action}
        </button>
        <button
          type="button"
          className={cn(iconButtonClassName, 'h-6 w-6 rounded-md')}
          aria-label="Dismiss"
          title="Dismiss"
          onClick={onDismiss}
        >
          <X className="h-3.5 w-3.5" />
        </button>
      </div>
      {/* All of it, scrollable and reachable by keyboard: what is confirmed is what is shown. */}
      <div
        className="max-h-40 overflow-auto border-t border-[var(--border)] px-2.5 py-1.5 [overflow-wrap:anywhere]"
        tabIndex={0}
        data-widget-request-text
      >
        {link ? (
          <>
            <div className="font-medium text-[var(--text-primary)]" data-widget-request-host>
              {link.host}
            </div>
            <div className="whitespace-pre-wrap text-[12px] text-[var(--text-muted)]">{link.href}</div>
          </>
        ) : (
          <div className="whitespace-pre-wrap text-[var(--text-primary)]">{visibleWidgetText(request.text)}</div>
        )}
      </div>
    </div>
  );
}
