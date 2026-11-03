import { create } from 'zustand';

/** Requests waiting for the user are kept for this many widgets at most. */
const MAX_PENDING = 100;

export type WidgetRequestKind = 'prompt' | 'open-link' | 'copy';

/**
 * Something a widget asked the chat to do for the user: send a prompt, open a
 * link, copy text. It waits under the widget until the user confirms or
 * dismisses it. Kept here, not in the widget's component, so it survives the
 * widget being drawn again elsewhere (a finished turn pins its widgets).
 */
export interface WidgetRequest {
  /** New for every request, so each one is confirmable only on its own. */
  id: number;
  kind: WidgetRequestKind;
  text: string;
}

interface WidgetRequestMemory {
  requests: Record<string, WidgetRequest>;
  ask: (toolUseId: string, kind: WidgetRequestKind, text: string) => void;
  /** Done with a request (confirmed or dismissed), if it is still the current one. */
  settle: (toolUseId: string, id: number) => void;
}

let nextId = 1;

export const useWidgetRequestStore = create<WidgetRequestMemory>()((set) => ({
  requests: {},
  ask: (toolUseId, kind, text) =>
    set((current) => {
      const requests = { ...current.requests };
      // Re-inserted last, so the oldest pending request is the first key.
      delete requests[toolUseId];
      requests[toolUseId] = { id: nextId++, kind, text };
      const ids = Object.keys(requests);
      for (const id of ids.slice(0, Math.max(0, ids.length - MAX_PENDING))) delete requests[id];
      return { requests };
    }),
  settle: (toolUseId, id) =>
    set((current) => {
      if (current.requests[toolUseId]?.id !== id) return current;
      const requests = { ...current.requests };
      delete requests[toolUseId];
      return { requests };
    }),
}));
