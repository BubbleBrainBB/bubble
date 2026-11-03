// A finished turn shows its widgets — and, from the first widget on, the
// reply text between and after them — outside the collapsed trace, in order.
// Working notes before the first widget, rejected widgets and other tools stay
// in the trace. A call is a draft (markup only) until the tool accepts it.
import assert from 'node:assert/strict';
import { buildActivityUnits } from '../../src/ui/utils/activity-units';
import { isDraftWidget, isWidgetEntry, pinnedTurnEntries, type WidgetEntry } from '../../src/ui/utils/turn-pins';
import { WIDGET_STREAMING_FLAG } from '../../src/shared/widget';
import type { WorkstreamEntry } from '../../src/ui/utils/workstream';

const note = (id: string, text: string) => ({ id, type: 'note', summary: text, detail: text }) as unknown as WorkstreamEntry;
const tool = (id: string, toolName: string, input: unknown, status = 'success', isError = false) =>
  ({ id, type: 'tool', toolName, status, block: { type: 'tool_use', id, name: toolName, input }, result: { is_error: isError } }) as unknown as WorkstreamEntry;
const widget = (id: string, status = 'success', isError = false) => tool(id, 'show_widget', { html: '<div></div>' }, status, isError);
const ids = (entries: WorkstreamEntry[]) => pinnedTurnEntries(entries).map((entry) => entry.id);

assert.deepEqual(
  ids([
    note('plan', 'Let me draw this.'),
    tool('guide', 'widget_guide', {}),
    widget('w1'),
    note('between', 'The stem pulls up; gravity pulls down.'),
    tool('bash', 'bash', { command: 'ls' }),
    widget('w2'),
    note('after', 'Everywhere, "down" means toward the center.'),
  ]),
  ['w1', 'between', 'w2', 'after'],
  'visual → text → visual, working notes and other tools stay in the trace'
);
assert.deepEqual(ids([note('a', 'plain answer'), tool('bash', 'bash', {})]), [], 'no widget, nothing pinned');
assert.deepEqual(
  ids([widget('bad', 'error', true), note('fix', 'Fixing the script.'), widget('good'), note('done', 'Here it is.')]),
  ['good', 'done'],
  'a rejected widget is not drawn and does not start the pinned run'
);
assert.deepEqual(
  ids([widget('stopped', 'interrupted'), note('after', 'Stopped mid-way.')]),
  ['stopped', 'after'],
  'a call the tool never answered (a stopped turn) still shows its markup'
);
assert.deepEqual(ids([widget('w'), note('blank', '   ')]), ['w'], 'blank text after a widget is skipped');

// Recognized only with real HTML; namespaced names count.
assert.equal(isWidgetEntry(tool('x', 'mcp__bubble__show_widget', { html: '<b>x</b>' })), true);
assert.equal(isWidgetEntry(tool('x', 'show_widget', { title: 'still writing' })), false, 'no HTML yet, no widget');

// Draft until the tool accepts it.
const asWidget = (entry: WorkstreamEntry) => entry as WidgetEntry;
assert.equal(isDraftWidget(asWidget(widget('p', 'pending'))), true, 'not answered yet');
assert.equal(isDraftWidget(asWidget(widget('s', 'success'))), false, 'accepted');
assert.equal(isDraftWidget(asWidget(tool('f', 'show_widget', { html: '<p>', [WIDGET_STREAMING_FLAG]: true }, 'success'))), true,
  'a streaming preview is a draft whatever its status');

// A widget is its own unit in the trace, where the agent placed it.
const units = buildActivityUnits([tool('a', 'mcp__docs__read', {}), widget('w'), tool('b', 'mcp__docs__read', {})]);
assert.deepEqual(units.map((unit) => unit.kind), ['group', 'standalone', 'group']);
assert.equal(units[1].kind === 'standalone' && units[1].entry.id, 'w');

console.log('turn-pins: widgets and the text between them pinned in order; drafts until accepted; widgets standalone in the trace');
