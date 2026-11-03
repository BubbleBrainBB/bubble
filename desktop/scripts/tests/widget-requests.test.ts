// What a widget asks the chat to do waits per widget until the user settles
// it: a new request replaces the old one with a new id, settling an old id
// does nothing, and pending requests stay bounded.
import assert from 'node:assert/strict';
import { useWidgetRequestStore } from '../../src/ui/store/useWidgetRequestStore';

const store = useWidgetRequestStore.getState;
store().ask('w1', 'prompt', 'Explain');
const first = store().requests.w1;
assert.equal(first.kind, 'prompt');
store().ask('w1', 'copy', '#75E1A4');
const second = store().requests.w1;
assert.notEqual(second.id, first.id, 'each request is its own');
assert.deepEqual({ kind: second.kind, text: second.text }, { kind: 'copy', text: '#75E1A4' }, 'the newer request replaces the older');
store().settle('w1', first.id);
assert.equal(store().requests.w1?.id, second.id, 'settling a replaced request leaves the current one');
store().settle('w1', second.id);
assert.equal(store().requests.w1, undefined);

store().ask('kept', 'open-link', 'https://example.com/');
for (let index = 0; index < 150; index += 1) store().ask(`flood-${index}`, 'prompt', 'x');
assert.ok(Object.keys(store().requests).length <= 100, 'bounded');
assert.equal(store().requests.kept, undefined, 'the oldest pending request goes first');
assert.ok(store().requests['flood-149']);

console.log('widget-requests: one current request per widget, stale settles ignored, bounded');
