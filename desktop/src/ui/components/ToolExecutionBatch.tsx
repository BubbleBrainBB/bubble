import { useEffect, useMemo, useState } from 'react';
import { ChevronRight } from './icons';
import type { ContentBlock, PermissionRequestPayload, ToolStatus, StreamMessage } from '../types';
import { AssistantWorkstream, PinnedNoteIdsProvider, PinnedOutsideProvider, TurnPinnedEntries } from './AssistantWorkstream';
import { pinnedTurnEntries } from '../utils/turn-pins';
import {
  createBatchWorkstreamModel,
  type ToolResultBlock,
  type WorkstreamModel,
} from '../utils/workstream';
import { TodoProgressCard } from './TodoProgressCard';
import { WorkstreamDisclosureState } from './WorkstreamDisclosureState';
import { GeneratedMediaGallery } from './GeneratedMediaGallery';
import { WorkstreamCollapse } from './WorkstreamPrimitives';
import type { GeneratedMediaItem } from '../utils/generated-media';

type AssistantMessage = StreamMessage & { type: 'assistant' };

interface ToolExecutionBatchProps {
  messages: AssistantMessage[];
  toolStatusMap: Map<string, ToolStatus>;
  toolResultsMap: Map<string, ToolResultBlock>;
  isSessionRunning: boolean;
  isTurnRunning?: boolean;
  /** True only for the active work group in the transcript. Without this gate,
   * every historical batch would also report `state==='running'` while the
   * session is mid-turn and show stale activity as running. */
  isLastBatch?: boolean;
  startedAt?: number;
  /** Final duration for a completed turn, resolved by the transcript timeline. */
  durationMs?: number;
  /** Subagent messages keyed by Task tool_use id — nests them under Task rows. */
  subagentMessagesByParent?: Map<string, StreamMessage[]>;
  liveTrace?: {
    partialText?: string;
    partialThinking?: string;
    permissionRequests?: PermissionRequestPayload[];
  };
  /** Streamed stdout/stderr tails keyed by tool_use id (running tools only). */
  toolLiveOutputMap?: Map<string, string>;
  expanded?: boolean;
  defaultExpanded?: boolean;
  canCollapse?: boolean;
  onExpandedChange?: (expanded: boolean) => void;
  resetKey?: string | number | null;
  generatedMedia?: GeneratedMediaItem[];
  mediaCwd?: string | null;
  showIdleActivity?: boolean;
}

export function ToolExecutionBatch({
  messages,
  toolStatusMap,
  toolResultsMap,
  isSessionRunning,
  isTurnRunning,
  isLastBatch = false,
  startedAt,
  durationMs,
  subagentMessagesByParent,
  liveTrace,
  toolLiveOutputMap,
  expanded,
  defaultExpanded,
  canCollapse,
  onExpandedChange,
  resetKey,
  generatedMedia,
  mediaCwd,
  showIdleActivity,
}: ToolExecutionBatchProps) {
  const batchIsRunning = isSessionRunning && isLastBatch;
  const model = useMemo(
    () =>
      createBatchWorkstreamModel({
        messages,
        toolStatusMap,
        toolResultsMap,
        isSessionRunning: batchIsRunning,
        startedAt,
        durationMs,
        subagentMessagesByParent,
        liveTrace: batchIsRunning ? liveTrace : undefined,
        toolLiveOutputMap: batchIsRunning ? toolLiveOutputMap : undefined,
      }),
    [messages, toolResultsMap, toolStatusMap, batchIsRunning, startedAt, durationMs, subagentMessagesByParent, liveTrace, toolLiveOutputMap]
  );

  const disclosureResetKey = resetKey ?? messages[0]?.uuid;
  return (
    <WorkstreamDisclosure
      model={model}
      isRunning={batchIsRunning}
      isTurnRunning={isTurnRunning ?? batchIsRunning}
      expanded={expanded}
      defaultExpanded={defaultExpanded}
      allowCollapse={canCollapse}
      onExpandedChange={onExpandedChange}
      resetKey={disclosureResetKey}
      generatedMedia={generatedMedia}
      mediaCwd={mediaCwd}
      showIdleActivity={showIdleActivity}
    />
  );
}

export function WorkstreamDisclosure({
  model,
  isRunning,
  isTurnRunning = isRunning,
  defaultExpanded = false,
  allowCollapse,
  expanded,
  onExpandedChange,
  resetKey,
  generatedMedia,
  mediaCwd,
  showIdleActivity,
}: {
  model: WorkstreamModel;
  isRunning: boolean;
  isTurnRunning?: boolean;
  defaultExpanded?: boolean;
  allowCollapse?: boolean;
  expanded?: boolean;
  onExpandedChange?: (expanded: boolean) => void;
  resetKey?: string | number | null;
  generatedMedia?: GeneratedMediaItem[];
  mediaCwd?: string | null;
  showIdleActivity?: boolean;
}) {
  const isControlled = typeof expanded === 'boolean';
  // A user choice wins over lifecycle defaults for the lifetime of this turn.
  const [choice, setChoice] = useState<{ key: typeof resetKey; expanded: boolean }>();
  const interrupted = model.entries.some((entry) =>
    (entry.type === 'tool' || entry.type === 'task' || entry.type === 'memory') && entry.status === 'interrupted');
  // The turn's terminal signal wins over an earlier interrupted/retried tool.
  // Standalone callers without turn metadata still retain stopped work.
  const canCollapse = !isRunning && (allowCollapse ?? !interrupted);
  const resolvedExpanded = !canCollapse || (isControlled ? expanded
    : choice && choice.key === resetKey ? choice.expanded : defaultExpanded);
  const setExpanded = (nextExpanded: boolean) => {
    if (!isControlled) setChoice({ key: resetKey, expanded: nextExpanded });
    onExpandedChange?.(nextExpanded);
  };
  // Reply text between a finished turn's widgets shows with them, outside the trace.
  const pinnedNoteIds = useMemo(
    () => new Set(canCollapse ? pinnedTurnEntries(model.entries).filter(entry => entry.type === 'note').map(entry => entry.id) : []),
    [canCollapse, model.entries]
  );

  if (model.entries.length === 0 && model.todoProgress) {
    return <TodoProgressCard state={model.todoProgress} className="my-2" />;
  }

  if (model.entries.length === 0) {
    return null;
  }

  return (
    <WorkstreamDisclosureState key={resetKey}>
      {(canCollapse || isTurnRunning) && <WorkstreamToggle
        expanded={resolvedExpanded}
        model={model}
        running={isTurnRunning}
        canCollapse={canCollapse}
        onToggle={() => setExpanded(!resolvedExpanded)}
        onShowDenied={() => setExpanded(true)}
      />}
      {/* Live media belongs beside its tool result, before subsequent narration.
          Collapsible traces keep any remaining media outside the hidden body. */}
      <WorkstreamCollapse open={resolvedExpanded} variant="turn">
        <PinnedOutsideProvider value={canCollapse}>
          <PinnedNoteIdsProvider value={pinnedNoteIds}>
            <AssistantWorkstream
              model={model}
              isTurnInProgress={isRunning}
              generatedMedia={canCollapse ? undefined : generatedMedia}
              mediaCwd={mediaCwd}
              showIdleActivity={showIdleActivity}
            />
          </PinnedNoteIdsProvider>
        </PinnedOutsideProvider>
      </WorkstreamCollapse>
      {canCollapse ? <TurnPinnedEntries entries={model.entries} /> : null}
      {canCollapse && generatedMedia?.length ? <GeneratedMediaGallery items={generatedMedia} cwd={mediaCwd ?? null} /> : null}
    </WorkstreamDisclosureState>
  );
}

function WorkstreamToggle({
  expanded,
  model,
  running,
  canCollapse,
  onToggle,
  onShowDenied,
}: {
  expanded: boolean;
  model: WorkstreamModel;
  running: boolean;
  canCollapse: boolean;
  onToggle: () => void;
  onShowDenied: () => void;
}) {
  const count = model.messageCount ?? model.entries.length;
  const denied = model.entries.filter(entry => entry.type === 'approval' && entry.state === 'denied').length;
  const label = <WorkstreamTimeLabel running={running} startedAt={model.startedAt} durationMs={model.durationMs} messageCount={count} />;

  return (
    <div className="workstream-toggle-row my-2">
      <div className="flex min-w-0 flex-wrap items-center gap-x-2">
      {canCollapse ? <button type="button" onClick={onToggle}
        className="workstream-text group flex min-w-0 items-center gap-1.5 py-0.5 text-left text-[var(--text-muted)] transition-colors hover:text-[var(--text-primary)]"
        aria-expanded={expanded}>
        <span className="min-w-0 truncate">{label}</span>
        <ChevronRight className={`h-3.5 w-3.5 shrink-0 transition-transform ${expanded ? 'rotate-90' : ''}`} />
      </button> : <div className="workstream-text py-0.5 text-[var(--text-muted)]">{label}</div>}
      {denied > 0 && <button type="button" data-denied-action-count onClick={onShowDenied}
        className="workstream-text text-[var(--text-muted)] hover:text-[var(--text-primary)]">
        {denied} {denied === 1 ? 'action' : 'actions'} denied
      </button>}
      </div>
      <div className="mt-1 w-full border-t border-[var(--border)]" data-workstream-divider />
    </div>
  );
}

/** One turn clock, independent of streamed text, tool identity and disclosure state. */
function WorkstreamTimeLabel({ running, startedAt, durationMs, messageCount }: {
  running: boolean; startedAt?: number; durationMs?: number; messageCount: number;
}) {
  const [now, setNow] = useState(Date.now);
  const validStart = typeof startedAt === 'number' && Number.isFinite(startedAt);
  useEffect(() => {
    if (!running || !validStart) return;
    setNow(Date.now());
    const timer = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(timer);
  }, [running, validStart, startedAt]);
  const elapsed = validStart ? Math.max(0, now - startedAt) : 0;
  const label = running ? (elapsed < 1000 ? 'Working' : `Working for ${formatElapsed(elapsed)}`)
    : typeof durationMs === 'number' && Number.isFinite(durationMs)
      ? `Worked for ${formatElapsed(durationMs)}`
      : `${messageCount} previous message${messageCount === 1 ? '' : 's'}`;
  return <span data-workstream-time className="tabular-nums">{label}</span>;
}

function formatElapsed(ms: number): string {
  const totalSeconds = Math.max(0, Math.floor(ms / 1000));
  if (totalSeconds < 60) return `${totalSeconds}s`;
  const minutes = Math.floor(totalSeconds / 60);
  const seconds = totalSeconds % 60;
  return seconds === 0 ? `${minutes}m` : `${minutes}m ${seconds}s`;
}

export type ToolUseBlock = ContentBlock & { type: 'tool_use' };
