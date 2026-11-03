import type { WorkstreamEntry } from './workstream';
import { summarizeWorkstreamEntries, type SummarizeWorkstreamEntriesOptions, type WorkstreamStage } from './workstream-stages';
import { deriveReadableToolDisplay, formatReadableToolSummary } from './tool-summary';
import { isWidgetEntry } from './turn-pins';

export type ToolActivity = Extract<WorkstreamEntry, { type: 'tool' | 'memory' }>;
export type ActivityUnit =
  | { kind: 'group'; id: string; entries: WorkstreamEntry[] }
  | { kind: 'tasks'; id: string; entries: Extract<WorkstreamEntry, { type: 'task' }>[] }
  | { kind: 'standalone'; id: string; entry: WorkstreamEntry };
export type ActivityHeader =
  | { kind: 'summary'; key: 'summary' }
  | { kind: 'thinking'; key: 'thinking'; label: string }
  | { kind: 'active'; key: string; entry: WorkstreamEntry };

/** The renderer consumes normalized Bubble events, never tool-name guesses for liveness. */
export function isActivityPending(entry: WorkstreamEntry): boolean {
  if (entry.type === 'thinking') return entry.state === 'active';
  if (entry.type === 'approval') return entry.state === 'waiting';
  if (entry.type === 'note') return entry.state === 'streaming';
  return 'status' in entry && entry.status === 'pending';
}

export function reasoningHeading(entries: WorkstreamEntry[]): string | undefined {
  for (let i = entries.length - 1; i >= 0; i--) {
    const entry = entries[i];
    if (entry.type !== 'thinking') continue;
    const text = (entry.detail || entry.summary).trim();
    const heading = text.match(/^\*\*([^\n]+?)\*\*/)?.[1]
      || text.match(/^#{1,6}\s+([^\n]+)/)?.[1];
    if (heading?.trim()) return heading.trim();
  }
  return undefined;
}

/** Standalone events form real chronological boundaries, including failures and questions. */
export function buildActivityUnits(entries: WorkstreamEntry[]): ActivityUnit[] {
  const units: ActivityUnit[] = [];
  const children = new Map(entries.filter((entry) => entry.type === 'task').map(entry => [entry.block.id, entry]));
  let buffer: WorkstreamEntry[] = [];
  const flush = () => {
    if (buffer.length) units.push({ kind: 'group', id: `activity:${buffer[0].id}`, entries: buffer });
    buffer = [];
  };
  for (const entry of entries) {
    // Native gn filters reasoning before qe assigns group boundaries and keys.
    // Its latest heading is supplied separately by the turn.
    if (entry.type === 'thinking') continue;
    // A wait is an operation on an existing child, not another child/status row.
    // Hide it only if every target has a visible row and retains the operation
    // in its model. Unknown targets and waits for children in other turns stay visible.
    if (entry.type === 'tool' && entry.subagentControl?.action === 'wait_agent'
      && entry.subagentControl.allTargetsLinked
      && entry.subagentControl.targets.length > 0
      && entry.subagentControl.targets.every(target => children.get(target.anchorId)?.subagent?.operations?.some(op => op.id === entry.id))) {
      flush();
      continue;
    }
    if (entry.type === 'task') {
      flush();
      const previous = units.at(-1);
      if (previous?.kind === 'tasks' && entry.sourceMessageUuid
        && previous.entries[0].sourceMessageUuid === entry.sourceMessageUuid) previous.entries.push(entry);
      else units.push({ kind: 'tasks', id: `agents:${entry.id}`, entries: [entry] });
    } else if (entry.type === 'note' || entry.type === 'error' || entry.type === 'approval'
      || (entry.type === 'tool' && (entry.subagentWait || entry.subagentControl || entry.kind === 'computer_use'
        || entry.result?.images?.length || entry.result?.mediaRefs?.length))
      // A widget is part of the answer, drawn where the agent placed it.
      || isWidgetEntry(entry)) {
      flush();
      units.push({ kind: 'standalone', id: entry.id, entry });
    } else buffer.push(entry);
  }
  flush();
  return units;
}

/** Matches Codex's latest/open activity slice: completed tools don't imply a completed turn. */
export function getActivityHeader(entries: WorkstreamEntry[], live: boolean, turnReasoningHeading?: string): ActivityHeader {
  if (!live) return { kind: 'summary', key: 'summary' };
  // Native W retains reasoning inside a trailing exploration slice; it does
  // not end exploration or create another activity row.
  let lastToolIndex = entries.length - 1;
  while (lastToolIndex >= 0 && entries[lastToolIndex].type === 'thinking') lastToolIndex--;
  const lastTool = entries[lastToolIndex];
  if (lastTool && isExploration(lastTool)) {
    let current = lastTool;
    for (let i = lastToolIndex; i >= 0; i--) {
      const entry = entries[i];
      if (entry.type === 'thinking') continue;
      if (!isExploration(entry)) break;
      if (isActivityPending(entry)) { current = entry; break; }
    }
    return { kind: 'active', key: `active:${current.id}`, entry: current };
  }
  for (let i = entries.length - 1; i >= 0; i--) {
    const entry = entries[i];
    if (entry.type !== 'thinking' && isActivityPending(entry)) {
      return { kind: 'active', key: `active:${entry.id}`, entry };
    }
  }
  return { kind: 'thinking', key: 'thinking', label: turnReasoningHeading || reasoningHeading(entries) || 'Thinking' };
}

export function activeActivityLabel(entry: WorkstreamEntry): string {
  if (isExploration(entry)) return formatReadableToolSummary(deriveReadableToolDisplay(entry.toolName, entry.block.input, 'pending')) || entry.summary;
  if (entry.type === 'tool' || entry.type === 'memory') {
    if (entry.kind === 'file_change') return 'Editing files';
    const input = entry.block.input as Record<string, unknown> | null;
    if (entry.kind === 'command_execution' && input && typeof input === 'object') {
      const command = typeof input.command === 'string' ? input.command : typeof input.cmd === 'string' ? input.cmd : '';
      if (command.trim()) return `Running ${command.trim()}`;
    }
  }
  return entry.summary;
}

export function isExploration(entry: WorkstreamEntry): entry is ToolActivity & { kind: 'file_read' | 'pattern_search' } {
  return (entry.type === 'tool' || entry.type === 'memory')
    && (entry.kind === 'file_read' || entry.kind === 'pattern_search');
}

function mcpIdentity(entry: WorkstreamEntry): string | undefined {
  if (entry.type !== 'tool' || entry.kind !== 'mcp_tool_call' || entry.status !== 'success' || !/^mcp[_:]/i.test(entry.toolName)
    || entry.result?.images?.length || entry.result?.mediaRefs?.length) return;
  // The qualified tool name includes the server. Different rendered targets must not merge.
  return JSON.stringify([entry.toolName, entry.summary]);
}

/** Native activity-group details coalesce only identical successful MCP calls. */
export function activityDetailGroups(entries: WorkstreamEntry[]): WorkstreamEntry[][] {
  const groups: WorkstreamEntry[][] = [];
  for (const entry of entries) {
    if (entry.type === 'thinking') continue;
    const previous = groups.at(-1);
    const last = previous?.at(-1);
    const identity = mcpIdentity(entry);
    if (last && identity && identity === mcpIdentity(last)) previous!.push(entry);
    else groups.push([entry]);
  }
  return groups;
}

export function buildActivityStages(entries: WorkstreamEntry[], options: SummarizeWorkstreamEntriesOptions = {}): WorkstreamStage[] {
  return activityDetailGroups(entries).flatMap(group => {
    const stages = summarizeWorkstreamEntries(group, options);
    if (group.length > 1 && mcpIdentity(group[0])) {
      return [{ ...stages[0], entries: group, count: group.length,
        title: `${group[0].summary} · ${group.length} calls` }];
    }
    // Each native read/search/list is an individual detail row, not another
    // aggregated "Explored N files" disclosure inside the activity group.
    return group.length === 1 && isExploration(group[0])
      ? stages.map(stage => ({ ...stage, title: group[0].summary })) : stages;
  });
}

/** Native yT omits unfinished read/search/list rows until they have a result. */
export function hasActivityDetail(entry: WorkstreamEntry): boolean {
  if (entry.type === 'tool' && entry.kind === 'file_change' && entry.status === 'pending') {
    const input = entry.block.input as Record<string, unknown> | undefined;
    if (input?.__aegisToolCallStreaming && !input.path && !input.file_path) return false;
  }
  return entry.type !== 'thinking' && !(isExploration(entry) && isActivityPending(entry));
}
