import { isShowWidgetToolName, isStreamingWidgetInput, widgetFromToolInput } from '../../shared/widget';
import type { WorkstreamEntry } from './workstream';

export type WidgetEntry = Extract<WorkstreamEntry, { type: 'tool' | 'memory' }>;
type NoteEntry = Extract<WorkstreamEntry, { type: 'note' }>;

export function isWidgetEntry(entry: WorkstreamEntry): entry is WidgetEntry {
  return entry.type === 'tool' && isShowWidgetToolName(entry.toolName) && widgetFromToolInput(entry.block.input) !== null;
}

/**
 * A widget the tool refused (its scripts don't parse) disappears, so the
 * agent's corrected call is the one that shows. Until the tool answers, the
 * chat draws the markup without running scripts.
 */
export function isRejectedWidget(entry: WidgetEntry): boolean {
  return entry.status === 'error' || entry.result?.is_error === true;
}

/** Drawn as markup only: still being written, or not yet accepted by the tool. */
export function isDraftWidget(entry: WidgetEntry): boolean {
  return entry.status !== 'success' || isStreamingWidgetInput(entry.block.input);
}

function hasVisibleText(entry: NoteEntry): boolean {
  return (entry.detail || entry.summary || '').trim().length > 0;
}

/**
 * What a finished turn shows outside its collapsed trace, in order: the
 * widgets it drew and — from the first widget on — the reply text between and
 * after them, so an explanation told as visual → text → visual reads the same
 * once the turn is done. Text before the first widget (working notes) stays
 * in the trace.
 */
export function pinnedTurnEntries(entries: WorkstreamEntry[]): WorkstreamEntry[] {
  const firstWidget = entries.findIndex((entry) => isWidgetEntry(entry) && !isRejectedWidget(entry));
  if (firstWidget < 0) return [];
  return entries.filter((entry, index) => {
    if (isWidgetEntry(entry)) return !isRejectedWidget(entry);
    return index > firstWidget && entry.type === 'note' && hasVisibleText(entry);
  });
}
