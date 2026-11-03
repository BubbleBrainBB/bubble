import { activeActivityLabel, buildActivityStages, buildActivityUnits, getActivityHeader, hasActivityDetail, isActivityPending, reasoningHeading } from '../utils/activity-units';
import { shortChildTask } from '../utils/bubble-subagent-view';
import { useWorkstreamDisclosure } from './WorkstreamDisclosureState';
import { ToolResultContent, ToolOutputPanel } from './ToolResultContent';
import { ActivityAnimationScope, ActivityDisclosureHeader, ActivitySummary, WorkstreamActivityLabel, WorkstreamCollapse, WorkstreamScrollArea } from './WorkstreamPrimitives';
import { AttachmentPreviewGrid } from './AttachmentPreviewGrid';
import { createContext, useContext, useEffect, useMemo, useState } from 'react';
import {
  ChevronRight,
  CircleDashed,
  CircleX,
  FileDiff,
  Search,
  Globe,
  Brain,
  Books,
  FolderOpen,
  FolderSearch,
  Plug,
  LoaderCircle,
  ShieldAlert,
  SquareTerminal,
} from './icons';
import { useAppStore } from '../store/useAppStore';
import {
  type ToolResultBlock,
  type ToolUseBlock,
  type WorkstreamEntry,
  type WorkstreamModel,
  getToolInputContent,
  getToolInputFilePath,
  getToolInputNewText,
  getToolInputOldText,
  getToolResultDiffContent,
  getToolResultOutputContent,
  safeJsonStringify,
} from '../utils/workstream';
import {
  createUnifiedDiffHunks,
  extractUnifiedDiffFilePath,
  parseUnifiedDiff,
  type UnifiedDiffHunk,
} from '../utils/unified-diff';
import { DiffHunkView } from './UnifiedDiffView';
import { ComputerUseAppIcon } from './ComputerUseAppIcon';
import { TodoProgressCard } from './TodoProgressCard';
import { DiffStatLabel } from './DiffStatLabel';
import { useTurnDiffContext } from './TurnDiffContext';
import { StructuredResponse } from './StructuredResponse';
import type { ChangeRecord } from '../utils/change-records';
import {
  getStageChangeRecords,
  getWorkstreamFailureCount,
  getWorkstreamStageActivityKind,
  getExplorationPresentation,
  getWorkstreamSummaryIconStage,
  formatWorkstreamStageSummary,
  summarizeWorkstreamEntries,
  type WorkstreamStage,
  type WorkstreamStageCommand,
  type WorkstreamStageFile,
} from '../utils/workstream-stages';
import { FileTypeIcon } from './FileTypeIcon';
import { SubagentAvatar } from './SubagentAvatar';
import { getSubagentPersona } from '../utils/subagent-persona';
import { GeneratedMediaGallery } from './GeneratedMediaGallery';
import { isMediaGenerationTool, type GeneratedMediaItem } from '../utils/generated-media';
import { widgetFromToolInput } from '../../shared/widget';
import { InlineWidget } from './widget/InlineWidget';
import { isDraftWidget, isRejectedWidget, isWidgetEntry, pinnedTurnEntries, type WidgetEntry } from '../utils/turn-pins';

interface AssistantWorkstreamProps {
  model: WorkstreamModel;
  isTurnInProgress: boolean;
  showIdleActivity?: boolean;
  className?: string;
  generatedMedia?: GeneratedMediaItem[];
  mediaCwd?: string | null;
}

const MAX_TRACE_TEXT_CHARS = 20_000;
const MAX_TITLE_CHARS = 800;

export function AssistantWorkstream({
  model,
  isTurnInProgress,
  showIdleActivity = true,
  className = '',
  generatedMedia,
  mediaCwd,
}: AssistantWorkstreamProps) {
  // Hooks must run in the same order every render — keep useMemo above any
  // conditional early return.
  const groups = useMemo(() => buildActivityUnits(model.entries), [model.entries]);
  const turnReasoningHeading = reasoningHeading(model.entries);
  const lastMediaGroupIndex = useMemo(() => {
    if (!generatedMedia?.length) return -1;
    const mediaToolIds = new Set(
      generatedMedia.map((item) => item.toolUseId).filter((id): id is string => Boolean(id))
    );
    let lastCompact = -1;
    let lastMedia = -1;
    groups.forEach((group, index) => {
      if (group.kind !== 'group') return;
      lastCompact = index;
      const hasReadyMediaTool = group.entries.some(
        (entry) =>
          entry.type === 'tool' &&
          entry.status !== 'pending' &&
          entry.status !== 'error' &&
          (isMediaGenerationTool(entry.toolName) || mediaToolIds.has(entry.block.id))
      );
      if (hasReadyMediaTool) lastMedia = index;
    });
    return lastMedia >= 0 ? lastMedia : lastCompact >= 0 ? lastCompact : groups.length - 1;
  }, [generatedMedia, groups]);

  if ((model.entries.length === 0 || (groups.length === 0 && (!showIdleActivity || !isTurnInProgress))) && !model.todoProgress) {
    return null;
  }

  return (
    <div className={`workstream-activities my-2 ${className}`.trim()}>
      {groups.map((group, idx) => {
        const groupKey = group.id;
        const live = showIdleActivity && isTurnInProgress && idx === groups.length - 1;
        const body = group.kind === 'tasks' ? (
          <SubagentGroup key={groupKey} entries={group.entries} />
        ) : group.kind === 'standalone' ? (
          group.entry.type === 'note' ? <TextSegment key={groupKey} entry={group.entry} />
            : group.entry.type === 'tool' && (group.entry.subagentWait || group.entry.subagentControl)
              ? <SubagentWaitRow key={groupKey} entry={group.entry} />
              : isWidgetEntry(group.entry) ? <TraceWidget key={groupKey} entry={group.entry} />
              : <EntryRow key={groupKey} entry={group.entry} />
        ) : (
          <CompactGroup key={groupKey} entries={group.entries} live={live} turnReasoningHeading={turnReasoningHeading} cwd={mediaCwd} />
        );
        if (idx !== lastMediaGroupIndex || !generatedMedia?.length) {
          return body;
        }
        return (
          <div key={`${groupKey}-media`}>
            {body}
            <GeneratedMediaGallery items={generatedMedia} cwd={mediaCwd ?? null} />
          </div>
        );
      })}
      {showIdleActivity && isTurnInProgress && groups.at(-1)?.kind !== 'group'
        && !model.entries.some(entry => entry.type === 'approval' && entry.state === 'waiting'
          || entry.type === 'tool' && entry.subagentWait
          || entry.type === 'note' && entry.state === 'streaming'
          // The widget drawing itself is the activity.
          || isWidgetEntry(entry) && entry.status === 'pending')
        ? <WorkingFooter label={turnReasoningHeading || 'Thinking'} reasoning={Boolean(turnReasoningHeading)} /> : null}
      {model.todoProgress ? (
        <div className="my-2">
          <TodoProgressCard state={model.todoProgress} />
        </div>
      ) : null}
    </div>
  );
}

// ── Text segment (assistant narration during the trace) ─────────────────────

function TextSegment({
  entry,
}: {
  entry: Extract<WorkstreamEntry, { type: 'note' }>;
}) {
  const pinned = useContext(PinnedNoteIds);
  const text = entry.detail || entry.summary;
  // Shown with the turn's widgets instead (see TurnPinnedEntries).
  if (pinned.has(entry.id) || !text.trim()) return null;
  const displayText = truncateWithNotice(text, MAX_TRACE_TEXT_CHARS);
  return (
    <div className="workstream-text my-2 min-w-0 overflow-x-auto">
      <StructuredResponse content={displayText} streaming={entry.state === 'streaming'} />
    </div>
  );
}

// ── Chat widgets (show_widget) ──────────────────────────────────────────────

/**
 * Widgets and the reply text around them belong to the answer, not to the
 * work trace: when a finished turn's trace collapses, the turn renders them
 * outside the collapsible body (in order, see pinnedTurnEntries) and the
 * trace leaves them out.
 */
const PinnedOutside = createContext(false);
export const PinnedOutsideProvider = PinnedOutside.Provider;
const PinnedNoteIds = createContext<ReadonlySet<string>>(new Set());
export const PinnedNoteIdsProvider = PinnedNoteIds.Provider;

export function TurnPinnedEntries({ entries }: { entries: WorkstreamEntry[] }) {
  const pinned = useMemo(() => pinnedTurnEntries(entries), [entries]);
  if (pinned.length === 0) return null;
  return (
    <>
      {pinned.map((entry) =>
        isWidgetEntry(entry) ? (
          <WidgetFromEntry key={entry.id} entry={entry} />
        ) : entry.type === 'note' ? (
          <div key={entry.id} className="workstream-text my-2 min-w-0 overflow-x-auto">
            <StructuredResponse content={truncateWithNotice(entry.detail || entry.summary, MAX_TRACE_TEXT_CHARS)} />
          </div>
        ) : null
      )}
    </>
  );
}

/**
 * A call still being written, or not yet answered by the tool, draws its
 * markup without running its scripts (a refused one then disappears); once
 * accepted it comes alive in the same frame.
 */
function widgetProps(entry: WidgetEntry) {
  const widget = widgetFromToolInput(entry.block.input);
  if (!widget || isRejectedWidget(entry)) return null;
  return { toolUseId: entry.block.id, html: widget.html, title: widget.title, draft: isDraftWidget(entry) };
}

function WidgetFromEntry({ entry }: { entry: WidgetEntry }) {
  const props = widgetProps(entry);
  return props ? <InlineWidget {...props} /> : null;
}

/** A widget inside the trace. Collapsed turns draw their widgets outside instead. */
function TraceWidget({ entry }: { entry: WidgetEntry }) {
  const outside = useContext(PinnedOutside);
  return outside ? null : <WidgetFromEntry entry={entry} />;
}

// ── Compact group with overflow ─────────────────────────────────────────────

function CompactGroup({ entries, live, turnReasoningHeading, cwd }: { entries: WorkstreamEntry[]; live: boolean; turnReasoningHeading?: string; cwd?: string | null }) {
  const { changeRecordsByToolUseId, onOpenDiff } = useTurnDiffContext();
  const stages = useMemo(() => buildActivityStages(entries, { changeRecordsByToolUseId }), [entries, changeRecordsByToolUseId]);
  const detailStages = useMemo(() => buildActivityStages(entries.filter(hasActivityDetail), { changeRecordsByToolUseId }), [entries, changeRecordsByToolUseId]);
  const header = getActivityHeader(entries, live, turnReasoningHeading);
  const [expanded, setExpanded] = useWorkstreamDisclosure(`activity:${entries[0].id}`);
  const activities = entries.filter(entry => entry.type !== 'thinking');
  const activeStage = header.kind === 'active'
    ? summarizeWorkstreamEntries([header.entry], { changeRecordsByToolUseId })[0] : undefined;
  // A completed single item is already its own disclosure. Don't add a group wrapper.
  if (header.kind === 'summary' && activities.length === 1 && !isActivityPending(activities[0]) && stages.length === 1
    && (stages[0].kind !== 'edit' || stages[0].files.length === 1)) {
    return <StageRow stage={stages[0]} onOpenDiff={onOpenDiff} cwd={cwd} />;
  }
  const label = header.kind === 'thinking' ? header.label
    : header.kind === 'active' ? activeActivityLabel(header.entry)
    : stages.length ? formatWorkstreamStageSummary(stages) : 'Thought';
  const authoredHeading = header.kind === 'thinking' && Boolean(turnReasoningHeading || reasoningHeading(entries));
  const canExpand = detailStages.length > 0;
  const iconStage = activeStage || (header.kind === 'summary' ? getWorkstreamSummaryIconStage(stages) : undefined);
  return <div className="my-2 space-y-1" data-activity-unit={entries[0].id} data-activity-state={header.kind}>
    <button type="button" data-workstream-group aria-expanded={canExpand ? expanded : undefined} disabled={!canExpand}
      onClick={() => canExpand && setExpanded(!expanded)}
      className="workstream-text group flex max-w-full items-center gap-1.5 text-left text-[var(--text-muted)] enabled:hover:text-[var(--text-primary)] disabled:cursor-default">
      <ActivitySummary summaryKey={header.key} immediate={header.kind === 'summary'}>
        {iconStage && <StageKindIcon stage={iconStage} />}
        {authoredHeading && <ReasoningMark />}
        <span className={`min-w-0 truncate${authoredHeading ? ' italic' : ''}`}>
          <WorkstreamActivityLabel active={header.kind !== 'summary'}>{label}</WorkstreamActivityLabel>
        </span>
      </ActivitySummary>
      <FailureCount entries={entries} />
      {canExpand && <ChevronRight className={`h-3 w-3 shrink-0 transition-transform ${expanded ? 'rotate-90' : ''}`} />}
    </button>
    <WorkstreamCollapse open={expanded && canExpand}>
      <ActivityAnimationScope enabled={false}><WorkstreamScrollArea bounded={false} followKey={header.kind === 'active' ? header.entry.id : undefined}>
        {detailStages.map(stage => <StageRow key={stage.id} stage={stage} onOpenDiff={onOpenDiff} cwd={cwd} />)}
      </WorkstreamScrollArea></ActivityAnimationScope>
    </WorkstreamCollapse>
  </div>;
}

// ── Stage summary rows ─────────────────────────────────────────────────────

function FailureCount({ entries }: { entries: WorkstreamEntry[] }) {
  const count = getWorkstreamFailureCount(entries);
  return count > 0 ? <span className="shrink-0 text-[var(--text-muted)]">({count} failed)</span> : null;
}

function StageRow({
  stage,
  onOpenDiff,
  cwd,
}: {
  stage: WorkstreamStage;
  cwd?: string | null;
  onOpenDiff?: (
    record: ChangeRecord,
    scope?: { records: ChangeRecord[]; label?: string; turnKey?: string }
  ) => void;
}) {
  const [expanded, setExpanded] = useWorkstreamDisclosure(
    `activity-detail:${stage.entries[0].id}`,
    stage.defaultExpanded
  );
  const openFile = useAppStore(state => state.openProjectFileInRightPanel);

  if (stage.kind === 'explore' && stage.status === 'success') {
    return <ExplorationRow stage={stage} cwd={cwd} />;
  }

  const hasDetails = stage.entries.some(hasRawEntryDetail);
  const canExpand =
    stage.files.length > 0 ||
    stage.commands.length > 0 ||
    hasDetails ||
    (stage.kind === 'computer_use' && stage.entries.some(hasComputerUseStageDetail));
  const isPending = stage.status === 'pending';
  const editFile = stage.kind === 'edit' && stage.files.length === 1 ? stage.files[0] : undefined;
  if (editFile) {
    const absolute = editFile.filePath.startsWith('/');
    const root = cwd || (absolute ? editFile.filePath.slice(0, editFile.filePath.lastIndexOf('/')) || '/' : null);
    const verb = stage.title.endsWith(editFile.fileName) ? stage.title.slice(0, -editFile.fileName.length).trim() : stage.title;
    const label = <span className="inline-flex items-center gap-1.5">
      <StageKindIcon stage={stage} />
      <WorkstreamActivityLabel active={isPending}>{verb}</WorkstreamActivityLabel>
      <button type="button" title={editFile.filePath} data-agent-activity-file-link disabled={!root}
        className="hover:underline" onClick={() => root && openFile({cwd:root,path:editFile.filePath,external:absolute})}>{editFile.fileName}</button>
      <DiffStatLabel additions={stage.addedLines} deletions={stage.removedLines} muted />
    </span>;
    const hasDiff = Boolean(editFile.record?.diffContent);
    return <div className="workstream-text" data-workstream-stage={stage.id} data-edit-stage>
      {hasDiff ? <ActivityDisclosureHeader expanded={expanded} onToggle={() => setExpanded(!expanded)}>{label}</ActivityDisclosureHeader> : label}
      <WorkstreamCollapse open={expanded && hasDiff}>
        {editFile.record && <TraceFileDiff record={editFile.record} />}
      </WorkstreamCollapse>
      {stage.status === 'error' ? <StageGenericDetail entries={stage.entries} /> : null}
    </div>;
  }
  const titleClass = isPending || stage.status === 'waiting'
      ? 'text-[var(--text-secondary)]'
      : 'text-[var(--text-muted)] group-hover/stage:text-[var(--text-primary)]';

  return (
    <div className="group/stage" data-workstream-stage={stage.id}>
      <button
        type="button"
        onClick={() => canExpand && setExpanded(!expanded)}
        disabled={!canExpand}
        className={`flex w-full items-center gap-1.5 workstream-text text-left transition-colors disabled:opacity-100 ${
          canExpand ? '' : 'cursor-default'
        }`}
        aria-expanded={canExpand ? expanded : undefined}
      >
        <StageKindIcon stage={stage} />
        <span className={`min-w-0 truncate ${titleClass}`}><WorkstreamActivityLabel active={isPending}>{stage.title}</WorkstreamActivityLabel></span>
        <FailureCount entries={stage.entries} />
        {stage.kind === 'edit' ? (
          <DiffStatLabel additions={stage.addedLines} deletions={stage.removedLines} muted />
        ) : null}
        <StageStatusGlyph stage={stage} expanded={expanded} canExpand={canExpand} />
      </button>
      <WorkstreamCollapse open={expanded && canExpand}>
        <StageDetails stage={stage} onOpenDiff={onOpenDiff} />
      </WorkstreamCollapse>
    </div>
  );
}

/** Native completed exploration is an inline action, with a direct file link for reads. */
function ExplorationRow({ stage, cwd }: { stage: WorkstreamStage; cwd?: string | null }) {
  const openFile = useAppStore(state => state.openProjectFileInRightPanel);
  const file = stage.files[0];
  const absolute = file?.filePath.startsWith('/');
  const root = cwd || (absolute ? file.filePath.slice(0, file.filePath.lastIndexOf('/')) || '/' : null);
  return <div data-workstream-stage={stage.id} data-exploration-detail
    className="workstream-text flex max-w-full min-w-0 items-center gap-1.5 text-[var(--text-muted)]">
    <StageKindIcon stage={stage} />
    <span className="min-w-0 truncate">
      {file && root ? <>Read <button type="button" data-agent-activity-file-link
        title={file.filePath} className="cursor-pointer hover:text-[var(--text-primary)] hover:underline"
        onClick={() => openFile({ cwd: root, path: file.filePath, external: absolute })}>{file.fileName}</button></> : stage.title}
    </span>
  </div>;
}

function StageKindIcon({ stage }: { stage: WorkstreamStage }) {
  const kind = getWorkstreamStageActivityKind(stage);
  const className = `h-4 w-4 flex-shrink-0 ${
    stage.status === 'waiting'
        ? 'text-amber-600'
        : 'text-[var(--text-muted)]/55'
  }`;

  if (kind === 'edit') return <FileDiff className={className} />;
  if (kind === 'command') return <SquareTerminal className={className} />;
  if (kind === 'approval') return <ShieldAlert className={className} />;
  if (kind === 'computer_use') {
    return <ComputerUseAppIcon app={stage.computerUseApp} className={className} />;
  }
  if (kind === 'error') return <CircleX className={className} />;
  if (kind === 'web') return <Globe className={className} />;
  if (kind === 'memory') return <Brain className={className} />;
  if (kind === 'other') return <Plug className={className} />;
  if (kind === 'explore') {
    const { icon } = getExplorationPresentation(stage.entries);
    if (icon === 'read') return <Books className={className} />;
    if (icon === 'list') return <FolderOpen className={className} />;
    if (icon === 'explore') return <FolderSearch className={className} />;
  }
  return <Search className={className} />;
}

function StageStatusGlyph({
  stage,
  expanded,
  canExpand,
}: {
  stage: WorkstreamStage;
  expanded: boolean;
  canExpand: boolean;
}) {
  if (stage.status === 'waiting') {
    return <ShieldAlert className="h-3 w-3 flex-shrink-0 text-amber-600" />;
  }
  if (stage.status === 'interrupted') {
    return <CircleDashed className="h-3 w-3 flex-shrink-0 text-[var(--text-muted)]/60" />;
  }
  if (!canExpand || (stage.kind === 'command' && stage.status === 'success' && !expanded)) return null;

  return (
    <ChevronRight
      className={`h-3 w-3 flex-shrink-0 text-[var(--text-muted)]/45 transition-transform ${
        expanded ? 'rotate-90' : ''
      }`}
    />
  );
}

function StageDetails({
  stage,
  onOpenDiff,
}: {
  stage: WorkstreamStage;
  onOpenDiff?: (
    record: ChangeRecord,
    scope?: { records: ChangeRecord[]; label?: string; turnKey?: string }
  ) => void;
}) {
  const showGenericDetail = stage.files.length === 0 && stage.commands.length === 0 && stage.kind !== 'computer_use';
  const images = Array.from(new Map(stage.entries.flatMap((entry) =>
    entry.type === 'tool' ? entry.result?.images || [] : []).map((image) => [image.id, image])).values());

  return (
    <div className="workstream-details mb-1 space-y-1">
      {stage.files.length > 0 ? (
        <StageFilesDetail stage={stage} onOpenDiff={onOpenDiff} />
      ) : null}
      {images.length > 0 && !showGenericDetail && stage.kind !== 'computer_use' ? <AttachmentPreviewGrid attachments={images} align="start" /> : null}
      {stage.commands.length > 0 ? <StageCommandsDetail commands={stage.commands} /> : null}
      {stage.kind === 'computer_use' ? <StageComputerUseDetail entries={stage.entries} /> : null}
      {showGenericDetail ? (
        <StageGenericDetail entries={stage.entries} />
      ) : stage.status === 'error' ? (
        <StageFailureNotes entries={stage.entries} />
      ) : null}
    </div>
  );
}

// Command failures already show their output in StageCommandsDetail, so the
// failure notes only cover the remaining failed entries (e.g. a rejected Edit).
function hasComputerUseStageDetail(entry: WorkstreamEntry): boolean {
  if (entry.type !== 'tool' && entry.type !== 'task' && entry.type !== 'memory') return false;
  return hasEntryDetail(entry);
}

function StageComputerUseDetail({ entries }: { entries: WorkstreamEntry[] }) {
  return <StageGenericDetail entries={entries} />;
}

function StageGenericDetail({ entries }: { entries: WorkstreamEntry[] }) {
  const tools = entries.filter(
    (entry): entry is Extract<WorkstreamEntry, { type: 'tool' | 'task' | 'memory' }> =>
      entry.type === 'tool' || entry.type === 'task' || entry.type === 'memory'
  );
  if (tools.length === 0) return <StageErrorFallback entries={entries} />;
  return (
    <div className="space-y-2">
      {tools.map((entry) => (
        <ToolEntryDetail key={entry.id} entry={entry} />
      ))}
    </div>
  );
}

function ComputerUseScreenshots({ refs }: { refs: NonNullable<ToolResultBlock['mediaRefs']> }) {
  const [previews, setPreviews] = useState<Record<string, string | null>>({});

  useEffect(() => {
    let cancelled = false;
    void Promise.all(
      refs.map(async (ref) => {
        try {
          const dataUrl = await window.electron.readComputerUseArtifact(ref.sessionId, ref.sha256);
          return [ref.sha256, dataUrl] as const;
        } catch {
          return [ref.sha256, null] as const;
        }
      })
    ).then((entries) => {
      if (cancelled) return;
      setPreviews(Object.fromEntries(entries));
    });
    return () => {
      cancelled = true;
    };
  }, [refs]);

  return (
    <div className="flex flex-wrap gap-2">
      {refs.map((ref) => (
        <div
          key={ref.sha256}
          className="overflow-hidden rounded border border-[var(--border)]/50 bg-[var(--bg-secondary)]"
        >
          {previews[ref.sha256] ? (
            <img src={previews[ref.sha256] || ''} alt="Computer use screenshot" className="max-h-40 max-w-full" />
          ) : (
            <div className="px-2 py-1 text-[11px] text-[var(--text-muted)]">Screenshot saved</div>
          )}
        </div>
      ))}
    </div>
  );
}

function StageFailureNotes({ entries }: { entries: WorkstreamEntry[] }) {
  const failed = entries.filter(isFailedNonCommandEntry);
  if (failed.length === 0) return null;

  return (
    <div className="space-y-1">
      {failed.map((entry) => (
        <FailureNote key={entry.id} entry={entry} />
      ))}
    </div>
  );
}

function isFailedNonCommandEntry(entry: WorkstreamEntry): boolean {
  if ('kind' in entry && entry.kind === 'command_execution') return false;
  if (entry.type === 'error') return true;
  if (entry.type === 'approval') return entry.state === 'denied';
  if (entry.type === 'tool' || entry.type === 'task' || entry.type === 'memory') {
    return entry.status === 'error';
  }
  return false;
}

function getEntryFailureText(entry: WorkstreamEntry): string {
  if (entry.type === 'tool' || entry.type === 'task' || entry.type === 'memory') {
    const output = getToolResultOutputContent(entry.result).trim();
    if (output) return output;
  }
  return (entry.detail || '').trim();
}

function FailureNote({ entry }: { entry: WorkstreamEntry }) {
  const text = getEntryFailureText(entry);
  return (
    <div className="overflow-hidden rounded-sm border border-[var(--border)]/45 bg-[var(--bg-secondary)]/30">
      <div
        className={`flex items-center gap-1.5 px-2 py-1 text-[11px] text-[var(--error)] ${
          text ? 'border-b border-[var(--border)]/45' : ''
        }`}
      >
        <CircleX className="h-3 w-3 flex-shrink-0" />
        <span className="min-w-0 flex-1 truncate">{entry.summary}</span>
      </div>
      {text ? <TailClampedOutput text={text} toneClass="text-[var(--error)]" /> : null}
    </div>
  );
}

function StageErrorFallback({
  entries,
}: {
  entries: WorkstreamEntry[];
}) {
  const visibleEntries = entries.filter(hasRawEntryDetail);
  if (visibleEntries.length === 0) return null;

  return (
    <div className="space-y-px">
      {visibleEntries.map((entry) => (
        <EntryRow key={entry.id} entry={entry} showChangeHint={false} />
      ))}
    </div>
  );
}

function StageFilesDetail({
  stage,
  onOpenDiff,
}: {
  stage: WorkstreamStage;
  onOpenDiff?: (
    record: ChangeRecord,
    scope?: { records: ChangeRecord[]; label?: string; turnKey?: string }
  ) => void;
}) {
  const records = getStageChangeRecords(stage);
  return (
    <div className="space-y-px">
      {stage.files.map((file) => <div key={file.id}>
        <StageFileRow
          file={file}
          records={records}
          onOpenDiff={onOpenDiff}
        />
        {stage.kind === 'edit' && file.record?.diffContent
          ? <TraceFileDiff record={file.record} /> : null}
      </div>)}
    </div>
  );
}

/** Only mounted inside an open activity disclosure; the persisted per-call
 * patch stays inspectable without rendering a growing JSON argument stream. */
function TraceFileDiff({ record }: { record: ChangeRecord }) {
  const hunks = useMemo(() => parseUnifiedDiff(record.diffContent || ''), [record.diffContent]);
  return <div data-trace-file-diff={record.filePath} aria-label={`Changes to ${record.fileName}`}
    tabIndex={0} className="my-2 max-h-80 overflow-auto rounded-md border border-[var(--border)]/50">
    {hunks.map((hunk, index) => <DiffHunkView key={index} hunk={hunk} />)}
  </div>;
}

function StageFileRow({
  file,
  records,
  onOpenDiff,
}: {
  file: WorkstreamStageFile;
  records: ChangeRecord[];
  onOpenDiff?: (
    record: ChangeRecord,
    scope?: { records: ChangeRecord[]; label?: string; turnKey?: string }
  ) => void;
}) {
  const clickable = Boolean(file.record && onOpenDiff);
  const operationLabel = formatFileOperation(file.operation);
  const body = (
    <>
      <FileTypeIcon name={file.fileName} className="h-3.5 w-3.5 flex-shrink-0 opacity-75" />
      <span className="w-12 flex-shrink-0 text-[var(--text-muted)]/60">{operationLabel}</span>
      <span
        className={`min-w-0 flex-1 truncate font-mono ${
          clickable ? 'text-[var(--accent)] group-hover/file:underline' : 'text-[var(--text-secondary)]'
        }`}
      >
        {file.filePath}
      </span>
      <DiffStatLabel additions={file.addedLines} deletions={file.removedLines} />
    </>
  );

  if (clickable && file.record) {
    return (
      <button
        type="button"
        onClick={() =>
          onOpenDiff?.(file.record!, {
            records,
            label: stageFileScopeLabel(records.length),
          })
        }
        title={file.filePath}
        className="group/file flex w-full items-center gap-1.5 rounded-sm px-1 py-0.5 text-left text-[11px] leading-5 transition-colors hover:bg-[var(--bg-tertiary)]/30"
      >
        {body}
      </button>
    );
  }

  return (
    <div
      title={file.filePath}
      className="flex w-full items-center gap-1.5 rounded-sm px-1 py-0.5 text-[11px] leading-5"
    >
      {body}
    </div>
  );
}

function StageCommandsDetail({ commands }: { commands: WorkstreamStageCommand[] }) {
  return (
    <div className="space-y-1">
      {commands.map((command) => (
        <ToolOutputPanel key={command.id} language="shell" isError={command.status === 'error'}
          text={`$ ${command.command}${command.output ? `\n${command.output}` : ''}`} />
      ))}
    </div>
  );
}

const OUTPUT_PREVIEW_LINES = 8;

function TailClampedOutput({ text, toneClass }: { text: string; toneClass: string }) {
  const [showFull, setShowFull] = useState(false);
  const lines = text.split('\n');
  const hiddenLines = lines.length - OUTPUT_PREVIEW_LINES;
  const displayText =
    showFull || hiddenLines <= 0
      ? truncateWithNotice(text, MAX_TRACE_TEXT_CHARS)
      : lines.slice(-OUTPUT_PREVIEW_LINES).join('\n');

  return (
    <>
      {hiddenLines > 0 ? (
        <button
          type="button"
          onClick={() => setShowFull((value) => !value)}
          className="flex w-full items-center px-2 py-0.5 text-left text-[11px] text-[var(--text-muted)]/70 transition-colors hover:text-[var(--text-secondary)]"
        >
          {showFull ? 'Show fewer lines' : `Show ${hiddenLines} earlier lines`}
        </button>
      ) : null}
      <pre
        className={`whitespace-pre-wrap break-words px-2 py-1 font-mono text-[11px] leading-5 ${toneClass}`}
      >
        {displayText}
      </pre>
    </>
  );
}

function formatFileOperation(operation: WorkstreamStageFile['operation']): string {
  if (operation === 'write' || operation === 'added') return 'Created';
  if (operation === 'delete' || operation === 'deleted') return 'Deleted';
  if (operation === 'renamed') return 'Moved';
  if (operation === 'read') return 'Read';
  if (operation === 'search') return 'Searched';
  return 'Edited';
}

function stageFileScopeLabel(count: number): string {
  return count === 1 ? 'Selected file changes' : `${count} files changed in this stage`;
}

function hasRawEntryDetail(entry: WorkstreamEntry): boolean {
  if (entry.type === 'tool' || entry.type === 'task' || entry.type === 'memory') {
    return hasEntryDetail(entry);
  }
  return Boolean(entry.detail);
}

// ── Subagent stage (Task tool calls) ────────────────────────────────────────
// Dispatch details and named lifecycle events stay outside ordinary activity groups.
// Full child traces open through their stable Bubble anchor in the detail panel.

type TaskEntry = Extract<WorkstreamEntry, { type: 'task' }>;

function SubagentGroup({ entries }: { entries: TaskEntry[] }) {
  return <div className="my-2 space-y-2" data-subagent-group>
    {entries.map(entry => <SubagentStatusRow key={entry.id} entry={entry} />)}
  </div>;
}

/**
 * One stable row per child. The row itself says who, what state, and what
 * task; its disclosure adds what the row cannot fit: the full task when the
 * inline line was cut, the tools the child actually used, and any failure.
 * Scheduler calls (wait/send/close) never appear here.
 */
function SubagentStatusRow({ entry }: { entry: TaskEntry }) {
  const [expanded, setExpanded] = useWorkstreamDisclosure(`agent-action:${entry.id}`);
  const { changeRecordsByToolUseId, onOpenDiff } = useTurnDiffContext();
  const runtime = entry.subagent?.runtime;
  const persona = getSubagentPersona(runtime?.agentId || entry.block.id, runtime?.role || entry.subagent?.agentType,
    shortChildTask(entry.subagent?.description || ''), runtime?.nickname);
  const awaitingCreation = entry.toolName === 'spawn_agent' && !runtime;
  const label = awaitingCreation
    ? entry.status === 'error' ? 'Failed to create' : entry.status === 'interrupted' ? 'Creation interrupted'
      : entry.result ? 'Created' : 'Creating…'
    : entry.status === 'error' ? 'Failed' : entry.status === 'interrupted' ? 'Interrupted'
    : entry.status === 'success' ? 'Finished' : runtime?.status === 'queued' ? 'Queued' : 'Working…';
  const name = awaitingCreation ? 'Agent' : persona.persona;
  const description = entry.subagent?.description || getTaskDescription(entry) || entry.summary;
  const taskLine = awaitingCreation ? '' : shortChildTask(firstTaskLine(description));
  const taskCut = taskLine !== description.trim();
  const progress = buildSubagentProgress(entry, awaitingCreation);
  const childEntries = entry.subagent?.entries || [];
  const stages = useMemo(() => buildActivityStages(childEntries.filter(hasActivityDetail), { changeRecordsByToolUseId }),
    [childEntries, changeRecordsByToolUseId]);
  const failed = Boolean(entry.result?.is_error);
  return <div data-subagent-lifecycle={entry.status} data-subagent-anchor={entry.block.id}>
    <div className="workstream-text flex min-w-0 items-center gap-1.5 text-[var(--text-muted)]">
      <SubagentAvatar id={runtime?.agentId || entry.block.id} hue={persona.colorHue} size={16} />
      <ActivityDisclosureHeader expanded={expanded} onToggle={() => setExpanded(!expanded)}>
        <button type="button" data-subagent-row={entry.block.id} data-subagent-status={entry.status}
          title={buildSubagentLaneTitle(entry, description)} aria-label={`Open ${name} subagent`}
          className="rounded-sm hover:text-[var(--text-primary)] focus-visible:outline-2 focus-visible:outline-offset-2"
          onClick={() => useAppStore.getState().openSubagentPanel(entry.block.id)}>{name}</button>
        <span aria-live="polite" data-subagent-state-label> · {label}</span>
        {taskLine && <span data-subagent-task-line title={safeTitle(description)}> · {taskLine}</span>}
        {progress && <span data-subagent-progress className="text-[var(--text-muted)]/70"> · {progress}</span>}
      </ActivityDisclosureHeader>
    </div>
    <WorkstreamCollapse open={expanded}>
      <div className="workstream-details space-y-2 py-2" data-subagent-details={entry.block.id}>
        {taskCut && (
          <div className="workstream-text whitespace-pre-wrap break-words text-[var(--text-secondary)]">
            {truncateWithNotice(description, MAX_TRACE_TEXT_CHARS)}
          </div>
        )}
        {stages.length > 0 ? (
          <ActivityAnimationScope enabled={false}><WorkstreamScrollArea bounded={false}>
            {stages.map(stage => <StageRow key={stage.id} stage={stage} onOpenDiff={onOpenDiff} cwd={null} />)}
          </WorkstreamScrollArea></ActivityAnimationScope>
        ) : !taskCut && !failed && (
          <div className="workstream-text text-[var(--text-muted)]" data-subagent-no-activity>
            {entry.status === 'pending' ? 'No tool activity yet' : 'No tool activity'}
          </div>
        )}
        {failed && <ToolEntryDetail entry={entry} />}
      </div>
    </WorkstreamCollapse>
  </div>;
}

/** First line of the task, shown inline on the child row. */
function firstTaskLine(description: string): string {
  return description.split('\n').map(line => line.trim()).find(Boolean) || '';
}

/**
 * Inline progress for the child row: what it is doing right now while it
 * runs (runtime activity, else its latest tool), and how much it did once it
 * settled (tool count, duration). Changes only on child events, never on a
 * clock, so the row does not churn every second.
 */
function buildSubagentProgress(entry: TaskEntry, awaitingCreation: boolean): string | null {
  if (awaitingCreation) return null;
  const trace = entry.subagent;
  const parts: string[] = [];
  const toolCount = trace?.toolCount ?? 0;
  const tools = toolCount > 0 ? `${toolCount} ${toolCount === 1 ? 'tool' : 'tools'}` : null;
  if (entry.status === 'pending') {
    const activity = trace?.runtime?.activity?.trim() || latestChildActivity(trace?.entries || []);
    if (tools) parts.push(tools);
    if (activity) parts.push(activity);
  } else {
    if (tools) parts.push(tools);
    if (typeof trace?.durationMs === 'number') parts.push(formatElapsed(trace.durationMs));
  }
  return parts.length > 0 ? parts.join(' · ') : null;
}

function latestChildActivity(entries: WorkstreamEntry[]): string | null {
  for (let i = entries.length - 1; i >= 0; i--) {
    const child = entries[i];
    if (child.type !== 'tool' && child.type !== 'memory' && child.type !== 'task') continue;
    const text = isActivityPending(child) ? activeActivityLabel(child) : child.summary;
    return text?.trim() || null;
  }
  return null;
}

function SubagentWaitRow({ entry }: { entry: Extract<WorkstreamEntry, { type: 'tool' | 'memory' }> }) {
  const [expanded, setExpanded] = useWorkstreamDisclosure(`agent-control:${entry.id}`);
  const targets = entry.subagentWait || entry.subagentControl?.targets || [];
  const active = entry.status === 'pending';
  const action = entry.subagentControl?.action;
  const label = entry.subagentWait ? entry.summary
    : entry.status === 'error' ? 'Agent operation failed'
    : entry.status === 'interrupted' ? 'Agent operation interrupted'
    : action === 'send_input' ? active ? 'Messaging' : 'Messaged'
    : action === 'close_agent' ? active ? 'Closing' : 'Closed'
    : active ? 'Waiting for' : 'Waited for';
  return <div className="my-2" data-agent-control={entry.id}>
    <div data-subagent-wait={entry.subagentWait ? entry.id : undefined} className="workstream-text">
      <ActivityDisclosureHeader expanded={expanded} onToggle={() => setExpanded(!expanded)}>
        <WorkstreamActivityLabel active={active}>{label}</WorkstreamActivityLabel>{' '}
        {targets.map((target, index) => <span key={target.anchorId}>
          {index > 0 && (index === targets.length - 1 ? ' and ' : ', ')}
          <button type="button" data-agent-target className="hover:text-[var(--text-primary)] focus-visible:outline-2 rounded-sm"
            onClick={() => useAppStore.getState().openSubagentPanel(target.anchorId)}>{target.name}</button>
        </span>)}
      </ActivityDisclosureHeader>
    </div>
    <WorkstreamCollapse open={expanded}><div className="workstream-details"><ToolEntryDetail entry={entry} /></div></WorkstreamCollapse>
  </div>;
}

function SubagentLane({ entry }: { entry: TaskEntry; standalone?: boolean }) {
  return <SubagentGroup entries={[entry]} />;
}

/** Fuller hover info (agent type, tool count, duration) kept off the line. */
function buildSubagentLaneTitle(entry: TaskEntry, description: string): string | undefined {
  const trace = entry.subagent;
  const parts: string[] = [];
  if (trace?.agentType) parts.push(trace.agentType);
  if (trace && trace.toolCount > 0) {
    parts.push(`${trace.toolCount} ${trace.toolCount === 1 ? 'tool' : 'tools'}`);
  }
  if (typeof trace?.durationMs === 'number') {
    parts.push(formatElapsed(trace.durationMs));
  }
  return safeTitle(parts.length > 0 ? `${description}\n${parts.join(' · ')}` : description);
}

function getTaskDescription(entry: TaskEntry): string | null {
  const input = isRecord(entry.block.input) ? entry.block.input : {};
  const description = input.description;
  if (typeof description === 'string' && description.trim()) {
    return description;
  }
  // Bubble's spawn surface uses message/task instead of prompt.
  for (const key of ['prompt', 'message', 'task'] as const) {
    const value = input[key];
    if (typeof value === 'string' && value.trim()) {
      return value.trim();
    }
  }
  return null;
}

// ── Entry row dispatcher ────────────────────────────────────────────────────

function EntryRow({ entry, showChangeHint = true }: { entry: WorkstreamEntry; showChangeHint?: boolean }) {
  if (entry.type === 'task') {
    // Task entries render as a subagent chip row rather than a generic tool
    // row — clicking the chip opens the subagent's tab in the detail panel,
    // where its full working trace lives.
    return <SubagentLane entry={entry} standalone />;
  }
  if (entry.type === 'thinking') {
    return null;
  }
  if (entry.type === 'note') {
    return <StreamingNoteRow entry={entry} />;
  }
  if (entry.type === 'approval') {
    return <ApprovalRow entry={entry} />;
  }
  if (entry.type === 'error') {
    return <ErrorRow entry={entry} />;
  }
  return <ToolRow entry={entry} showChangeHint={showChangeHint} />;
}

function StreamingNoteRow({
  entry,
}: {
  entry: Extract<WorkstreamEntry, { type: 'note' }>;
}) {
  const isStreaming = entry.state === 'streaming';
  return (
    <div
      className="flex items-baseline gap-1.5 py-0.5 text-[12px] leading-5 text-[var(--text-muted)]/55"
      title={safeTitle(entry.detail)}
    >
      <span className="min-w-0 flex-1 truncate">{entry.summary}</span>
      {isStreaming ? (
        <span className="inline-flex h-1 w-1 flex-shrink-0 rounded-full bg-[var(--text-muted)]/45 animate-pulse" />
      ) : null}
    </div>
  );
}

// ── Approval row ────────────────────────────────────────────────────────────

function ApprovalRow({
  entry,
}: {
  entry: Extract<WorkstreamEntry, { type: 'approval' }>;
}) {
  const tone =
    entry.state === 'approved'
      ? 'text-emerald-600'
      : entry.state === 'denied'
        ? 'text-[var(--error)]'
        : 'text-amber-600';

  return (
    <div data-approval-state={entry.state} className="workstream-text flex items-start gap-1.5 py-0.5 text-[var(--text-secondary)]">
      <ShieldAlert className={`h-3.5 w-3.5 flex-shrink-0 ${tone}`} />
      <span className="min-w-0 flex-1 break-words">{entry.summary}
        {entry.detail && entry.detail !== entry.summary && <span className="block text-[var(--text-muted)]">{entry.detail}</span>}
      </span>
      <span className={`flex-shrink-0 ${tone}`}>
        {entry.state}
      </span>
    </div>
  );
}

// ── Error row ───────────────────────────────────────────────────────────────

function ErrorRow({
  entry,
}: {
  entry: Extract<WorkstreamEntry, { type: 'error' }>;
}) {
  return (
    <div role="alert" className="workstream-text flex items-baseline gap-1.5 py-0.5 text-[var(--error)]">
      <CircleX className="h-3.5 w-3.5 flex-shrink-0" />
      <span className="min-w-0 flex-1 break-words">{entry.summary}
        {entry.detail && entry.detail !== entry.summary && <span className="block whitespace-pre-wrap">{truncateWithNotice(entry.detail, MAX_TRACE_TEXT_CHARS)}</span>}
      </span>
    </div>
  );
}

// ── Tool/task/memory row (the compact Synara-style line) ────────────────────

function ToolRow({
  entry,
  showChangeHint = true,
}: {
  entry: Extract<WorkstreamEntry, { type: 'tool' | 'task' | 'memory' }>;
  showChangeHint?: boolean;
}) {
  const [expanded, setExpanded] = useState(false);
  const { changeRecordByToolUseId, onOpenDiff } = useTurnDiffContext();
  const changeRecord = changeRecordByToolUseId.get(entry.block.id) || null;

  const canExpand = hasEntryDetail(entry);
  const isPending = entry.status === 'pending';
  const isError = entry.status === 'error';

  const summaryClass = isError
    ? 'text-[var(--error)]'
    : isPending
      ? 'text-[var(--text-muted)]/70'
      : 'text-[var(--text-muted)]/55 group-hover:text-[var(--text-secondary)]';

  return (
    <div className="group">
      <button
        type="button"
        onClick={() => canExpand && setExpanded((v) => !v)}
        disabled={!canExpand}
        title={safeTitle(entry.detail || entry.summary)}
        className={`flex w-full items-baseline gap-1.5 py-0.5 text-left text-[12px] leading-5 transition-colors disabled:opacity-100 ${
          canExpand ? '' : 'cursor-default'
        }`}
      >
        <span className={`min-w-0 flex-1 truncate ${summaryClass}`}>{entry.summary}</span>
        <RightStatusGlyph entry={entry} canExpand={canExpand} expanded={expanded} />
      </button>

      {showChangeHint && changeRecord ? (
        <EditedFileHint record={changeRecord} onOpen={onOpenDiff} />
      ) : null}

      {isPending && entry.type === 'tool' && entry.liveOutput ? (
        <LiveToolOutputTail text={entry.liveOutput} />
      ) : null}

      {expanded && canExpand ? (
        <div className="mb-1 ml-1 border-l border-[var(--border)]/50 pl-3">
          <ToolEntryDetail entry={entry} />
        </div>
      ) : null}
    </div>
  );
}

/**
 * Terminal-style tail of a running tool's streamed stdout/stderr: the last
 * few lines only — the full output arrives with the tool result. Lines with
 * carriage-return rewrites (progress bars) collapse to their final frame.
 */
function LiveToolOutputTail({ text }: { text: string }) {
  const tail = useMemo(() => {
    const lines = text
      .replace(/\r\n/g, '\n')
      .split('\n')
      .map((line) => line.split('\r').pop() || '')
      .filter((line) => line.trim().length > 0);
    return lines.slice(-6).join('\n');
  }, [text]);

  if (!tail) return null;
  return (
    <pre className="mb-1 ml-1 max-h-28 overflow-hidden whitespace-pre-wrap break-all border-l border-[var(--border)]/50 pl-3 font-mono text-[11px] leading-4 text-[var(--text-muted)]/70">
      {tail}
    </pre>
  );
}

function RightStatusGlyph({
  entry,
  canExpand,
  expanded,
}: {
  entry: Extract<WorkstreamEntry, { type: 'tool' | 'task' | 'memory' }>;
  canExpand: boolean;
  expanded: boolean;
}) {
  if (entry.status === 'pending') {
    return <LoaderCircle className="h-3 w-3 flex-shrink-0 animate-spin text-[var(--text-muted)]/55" />;
  }
  if (entry.status === 'error') {
    return <CircleX className="h-3 w-3 flex-shrink-0 text-[var(--error)]" />;
  }
  if (entry.status === 'interrupted') {
    return <CircleDashed className="h-3 w-3 flex-shrink-0 text-[var(--text-muted)]/60" />;
  }
  if (!canExpand) return null;
  return (
    <ChevronRight
      className={`h-3 w-3 flex-shrink-0 text-[var(--text-muted)]/45 transition-transform ${
        expanded ? 'rotate-90' : ''
      }`}
    />
  );
}

function EditedFileHint({
  record,
  onOpen,
}: {
  record: ChangeRecord;
  onOpen?: (record: ChangeRecord) => void;
}) {
  const verb =
    record.operation === 'write' ? 'Created' : record.operation === 'delete' ? 'Deleted' : 'Edited';
  const clickable = Boolean(onOpen);
  const fileName = record.fileName || record.filePath;

  const body = (
    <>
      <span className="text-[var(--text-muted)]/60">{verb}</span>
      <span
        className={`max-w-[28rem] truncate font-mono ${
          clickable ? 'text-[var(--accent)] group-hover:underline' : 'text-[var(--text-secondary)]'
        }`}
      >
        {fileName}
      </span>
      {record.addedLines + record.removedLines > 0 ? (
        <DiffStatLabel additions={record.addedLines} deletions={record.removedLines} />
      ) : null}
    </>
  );

  if (clickable) {
    return (
      <button
        type="button"
        onClick={() => onOpen?.(record)}
        title={record.filePath}
        className="group ml-0.5 mt-0.5 inline-flex items-baseline gap-1.5 text-left text-[11px] leading-5 transition-opacity"
      >
        {body}
      </button>
    );
  }
  return (
    <div
      title={record.filePath}
      className="ml-0.5 mt-0.5 inline-flex items-baseline gap-1.5 text-[11px] leading-5"
    >
      {body}
    </div>
  );
}

// ── Tool detail (args / output / diff), shown when row is expanded ──────────

function hasEntryDetail(
  entry: Extract<WorkstreamEntry, { type: 'tool' | 'task' | 'memory' }>
): boolean {
  const inputRecord = isRecord(entry.block.input) ? getPublicToolInput(entry.block.input) : {};
  return Boolean(
    entry.detail || entry.result || ('liveOutput' in entry && entry.liveOutput) || Object.keys(inputRecord).length
  );
}

function ToolEntryDetail({
  entry,
}: {
  entry: Extract<WorkstreamEntry, { type: 'tool' | 'task' | 'memory' }>;
}) {
  const inputRecord = isRecord(entry.block.input) ? getPublicToolInput(entry.block.input) : {};
  const contentStr = ('liveOutput' in entry && entry.liveOutput) || entry.result?.displayContent || entry.result?.content || entry.detail || '';
  const diffContent =
    entry.toolName === 'Write' || entry.toolName === 'Edit' || entry.toolName === 'Delete'
      ? getToolResultDiffContent(entry.result)
      : null;
  const diffFilePath =
    (diffContent ? extractUnifiedDiffFilePath(diffContent) : null) || getToolInputFilePath(inputRecord);
  const diffHunks = useMemo(() => {
    if (diffContent) {
      return parseUnifiedDiff(diffContent);
    }
    if (entry.toolName === 'Edit') {
      const oldText = getToolInputOldText(inputRecord);
      const newText = getToolInputNewText(inputRecord);
      if (oldText !== null && newText !== null) {
        return createUnifiedDiffHunks(oldText, newText, { contextLines: 3 });
      }
    }
    if (entry.toolName === 'Write') {
      const content = getToolInputContent(inputRecord);
      if (content) {
        return buildWritePreviewHunks(content);
      }
    }
    return [];
  }, [diffContent, entry.toolName, inputRecord]);

  return (
    <div className="my-1 space-y-2 text-[12px]">
      {entry.result?.images?.length ? <AttachmentPreviewGrid attachments={entry.result.images} align="start" /> : null}
      {entry.result?.mediaRefs && entry.result.mediaRefs.length > 0 ? (
        <ComputerUseScreenshots refs={entry.result.mediaRefs} />
      ) : null}
      {diffHunks.length > 0 ? (
        <div>
          <div className="mb-1 text-[11px] font-medium uppercase tracking-[0.08em] text-[var(--text-muted)]">
            {diffFilePath || entry.summary}
          </div>
          <div className="overflow-hidden border border-[var(--border)]/45 bg-[var(--bg-secondary)]/35">
            <div className="max-h-72 overflow-auto">
              {diffHunks.map((hunk, index) => (
                <DiffHunkView key={`${hunk.oldStart}-${hunk.newStart}-${index}`} hunk={hunk} />
              ))}
            </div>
          </div>
        </div>
      ) : null}

      <ToolResultContent
        content={contentStr}
        pending={entry.status === 'pending' || Boolean(entry.result?.images?.length || entry.result?.mediaRefs?.length)}
        isError={entry.status === 'error'}
        raw={{ callId: entry.block.id, invocation: { tool: entry.toolName, arguments: inputRecord }, result: entry.result ?? null }}
      />
    </div>
  );
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return !!value && typeof value === 'object' && !Array.isArray(value);
}

function getPublicToolInput(input: Record<string, unknown>): Record<string, unknown> {
  return Object.fromEntries(
    Object.entries(input).filter(([key]) => !key.startsWith('__aegis'))
  );
}

function truncateWithNotice(value: string, maxChars: number): string {
  if (value.length <= maxChars) {
    return value;
  }
  const omitted = value.length - maxChars;
  return `${value.slice(0, maxChars).trimEnd()}\n\n[Output truncated by Aegis: ${omitted.toLocaleString()} characters hidden]`;
}

function safeTitle(value: string | undefined): string | undefined {
  if (!value) {
    return undefined;
  }
  return truncateWithNotice(value, MAX_TITLE_CHARS);
}

function buildWritePreviewHunks(content: string): UnifiedDiffHunk[] {
  const lines = content.replace(/\r\n/g, '\n').replace(/\r/g, '\n').split('\n');
  if (lines[lines.length - 1] === '') {
    lines.pop();
  }
  return [
    {
      oldStart: 0,
      oldLines: 0,
      newStart: 1,
      newLines: lines.length,
      lines: lines.map((line, index) => ({
        type: 'addition',
        oldLineNumber: null,
        newLineNumber: index + 1,
        text: line,
      })),
    },
  ];
}

// ── Idle activity label (used when no current tool owns the status) ─────────

export function WorkingFooter({ label = 'Thinking', reasoning = false }: { label?: string; reasoning?: boolean }) {
  return (
    <div data-thinking-footer className="workstream-text my-2 flex items-center gap-1.5 text-[var(--text-muted)]">
      {reasoning && <ReasoningMark />}
      <span className={`min-w-0 truncate${reasoning ? ' italic' : ''}`}>
        <WorkstreamActivityLabel active>{label}</WorkstreamActivityLabel>
      </span>
    </div>
  );
}

/**
 * Marks a status line whose words were written by the model (its latest
 * reasoning heading) rather than by the app. System labels like "Working for
 * 1m" and "Frances · Working…" are plain; a model heading such as "Waiting
 * for next input" would otherwise read as the app reporting its own state.
 */
function ReasoningMark() {
  return <Brain aria-label="Model reasoning" data-reasoning-heading className="h-3 w-3 shrink-0 opacity-70" />;
}

function formatElapsed(ms: number): string {
  const totalSeconds = Math.max(0, Math.floor(ms / 1000));
  if (totalSeconds < 60) return `${totalSeconds}s`;
  const minutes = Math.floor(totalSeconds / 60);
  const seconds = totalSeconds % 60;
  return seconds === 0 ? `${minutes}m` : `${minutes}m ${seconds}s`;
}

// ── Response divider ("Response · Worked for Xs") ───────────────────────────

export function ResponseDivider({ durationMs }: { durationMs: number | undefined }) {
  const elapsed = typeof durationMs === 'number' ? formatElapsed(durationMs) : null;
  return (
    <div className="my-4 flex items-center gap-3 px-1">
      <div className="h-px flex-1 bg-[var(--border)]/60" />
      <span className="text-[11px] tracking-[0.04em] text-[var(--text-muted)]/80">
        Response{elapsed ? ` · Worked for ${elapsed}` : ''}
      </span>
      <div className="h-px flex-1 bg-[var(--border)]/60" />
    </div>
  );
}

// Re-export utility types for callers that import alongside.
export type { ToolResultBlock, ToolUseBlock };
