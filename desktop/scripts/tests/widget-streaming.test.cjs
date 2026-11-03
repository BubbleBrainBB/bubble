// A show_widget call draws in the chat while the model writes it: the Bubble
// adapter sends its partial HTML as a paced tool_use preview (marked so it is
// never persisted), and the finished call replaces the preview under the same
// message id, so the chat keeps the same frame. An interrupted call (retry,
// stop, error) closes as interrupted with no half-written HTML, live and in
// history alike.
const { app } = require('electron');
const fs = require('node:fs'), path = require('node:path'), os = require('node:os');
const assert = require('node:assert/strict');
const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'bubble-widget-stream-'));
app.setPath('userData', path.join(dir, 'profile'));
process.env.BUBBLE_HOME = path.join(dir, 'agent');
const delay = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
app.whenReady().then(async () => {
  const { BubbleSdkAdapter } = require('../../dist-electron/electron/libs/provider/bubble-sdk-adapter');
  const { isStreamingWidgetInput } = require('../../dist-electron/shared/widget');
  const adapter = new BubbleSdkAdapter(), messages = [];
  adapter.events.on('event', (event) => { if (event.message) messages.push(event.message); });
  const session = { threadId: 't1', currentAssistant: null, status: 'running',
    subagentStreams: new Map(), subagentStartedAt: new Map(), toolNames: new Map(), heldSpawnResults: new Map(),
    emittedToolCallIds: new Set(), emittedToolResultIds: new Set(), usage: {}, totalCostUsd: 0, durationStartMs: Date.now() };
  const event = (value) => adapter.handleBubbleEvent(session, value);
  const uuid = (id) => `bubble-tool-use:t1:${id}`;
  const callsOf = (id) => messages.filter((m) => m.uuid === uuid(id));
  const inputOf = (message) => message.message.content[0].input;

  event({ type: 'text_delta', content: 'Here is how the pipeline flows.' });
  event({ type: 'tool_call_start', id: 'w', name: 'show_widget' });
  assert.equal(messages.at(-2).phase, 'commentary', 'the narration before the widget is flushed first');
  assert.equal(callsOf('w').length, 1, 'the widget shows up as soon as the call starts');
  assert.equal(isStreamingWidgetInput(inputOf(callsOf('w')[0])), true, 'previews are marked (never persisted)');

  // A burst of deltas is paced: at most one redraw per interval, plus a trailing one.
  const html = '<style>.d{height:80px}</style><div class="d">' + 'row '.repeat(400) + '</div><script>window.ran=1</script>';
  let args = '{"title":"Pipe\\"line","html":"';
  const started = Date.now();
  for (let i = 0; i < html.length; i += 20) {
    args = '{"title":"Pipe\\"line","html":' + JSON.stringify(html.slice(0, i + 20)).slice(0, -1);
    event({ type: 'tool_call_delta', id: 'w', name: 'show_widget', arguments: args });
  }
  const burst = callsOf('w').length;
  assert.ok(burst <= 3 + Math.ceil((Date.now() - started) / 100), `paced redraws (${burst} for ${Math.ceil(html.length / 20)} deltas)`);
  await delay(250);
  const trailing = callsOf('w').at(-1);
  assert.equal(inputOf(trailing).title, 'Pipe"line', 'the title is decoded as soon as it is complete');
  assert.equal(inputOf(trailing).html, html, 'the trailing redraw carries the latest markup');
  assert.equal(isStreamingWidgetInput(inputOf(trailing)), true);
  const count = callsOf('w').length;
  event({ type: 'tool_call_delta', id: 'w', name: 'show_widget', arguments: args });
  await delay(250);
  assert.equal(callsOf('w').length, count, 'an unchanged preview is not resent');

  // The finished call takes the preview's place.
  const full = { title: 'Pipe"line', html };
  event({ type: 'tool_call_end', id: 'w', name: 'show_widget', arguments: JSON.stringify(full) });
  const canonical = callsOf('w').at(-1);
  assert.equal(canonical.createdAt, callsOf('w')[0].createdAt, 'same row, same time');
  assert.deepEqual(inputOf(canonical), full, 'the finished call has no streaming marks');
  assert.equal(isStreamingWidgetInput(inputOf(canonical)), false, 'the finished call is persisted');
  const settled = messages.length;
  event({ type: 'tool_call_delta', id: 'w', name: 'show_widget', arguments: '{"html":"<p>late</p>"}' });
  await delay(250);
  assert.equal(messages.length, settled, 'late deltas never overwrite the finished call');

  // A provider retry discards the partial call: it closes as interrupted with
  // only its title (no half-written HTML), and its pending redraw never fires.
  event({ type: 'tool_call_start', id: 'retry', name: 'show_widget' });
  event({ type: 'tool_call_delta', id: 'retry', name: 'show_widget', arguments: '{"title":"Retry","html":"<div>half' });
  event({ type: 'tool_call_delta', id: 'retry', name: 'show_widget', arguments: '{"title":"Retry","html":"<div>half more' });
  event({ type: 'provider_retry', attempt: 1, maxAttempts: 3 });
  const closed = callsOf('retry').at(-1);
  assert.deepEqual(inputOf(closed), { title: 'Retry', __aegisToolCallInterrupted: true }, 'interrupted, title only');
  assert.equal(isStreamingWidgetInput(inputOf(closed)), false);
  const afterRetry = callsOf('retry').length;
  await delay(250);
  assert.equal(callsOf('retry').length, afterRetry, 'no redraw after the retry');

  // The turn stops mid-write: every call still being written closes as
  // interrupted under its id (a widget with only its title), replacing what
  // the chat drew, so the chat shows what history replays; no pending redraw
  // fires afterwards.
  event({ type: 'tool_call_start', id: 'cut', name: 'show_widget' });
  event({ type: 'tool_call_delta', id: 'cut', name: 'show_widget', arguments: '{"title":"Cut","html":"<div>half' });
  event({ type: 'tool_call_delta', id: 'cut', name: 'show_widget', arguments: '{"title":"Cut","html":"<div>half more' });
  event({ type: 'tool_call_start', id: 'cmd', name: 'bash' });
  event({ type: 'tool_call_delta', id: 'cmd', name: 'bash', arguments: '{"command":"ls -' });
  adapter.finishTurn(session, new Error('fixture interruption'));
  const afterStop = messages.length;
  const cut = callsOf('cut').at(-1);
  assert.deepEqual(inputOf(cut), { title: 'Cut', __aegisToolCallInterrupted: true }, 'the cut widget closes as interrupted, title only');
  assert.equal(isStreamingWidgetInput(inputOf(cut)), false, 'and that is what history keeps');
  assert.equal(inputOf(callsOf('cmd').at(-1)).__aegisToolCallInterrupted, true, 'any other call being written closes the same way');
  await delay(250);
  assert.equal(messages.length, afterStop, 'no preview after the turn stopped');
  assert.equal(session.widgetStreams?.size ?? 0, 0);
  assert.equal(session.streamingToolCalls?.size ?? 0, 0);

  console.log('PASS: show_widget streams paced, marked previews; the finished call replaces them in place; retry and stop close calls as interrupted with no HTML');
}).then(() => { fs.rmSync(dir, { recursive: true, force: true }); app.exit(0); }, (error) => { console.error(error); app.exit(1); });
