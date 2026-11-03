// show_widget: its name is recognized however a provider namespaces it, the
// chat reads a widget only from a real input, and the tool validates what the
// chat will draw (non-empty, size cap, a note for blocked external URLs).
import assert from 'node:assert/strict';
import {
  isShowWidgetToolName,
  readWidgetFrameMessage,
  visibleWidgetText,
  widgetFromToolInput,
  WIDGET_HTML_MAX_CHARS,
  WIDGET_PROMPT_MAX_CHARS,
  WIDGET_STATE_MAX_CHARS,
} from '../../src/shared/widget';
import { showWidgetTool, SHOW_WIDGET_DESCRIPTION } from '../../src/electron/libs/widget-tool';

for (const name of ['show_widget', 'mcp__bubble__show_widget', 'bubble-sessions_show_widget', 'bubble-widgets.show_widget', 'bubble-sessions/show_widget', 'Show_Widget']) {
  assert.equal(isShowWidgetToolName(name), true, name);
}
for (const name of ['showwidget', 'show_widgets', 'render_ui', 'mcp__x__show_widget_now', 42, null]) {
  assert.equal(isShowWidgetToolName(name), false, String(name));
}

assert.deepEqual(widgetFromToolInput({ html: '<b>x</b>', title: '  Sales  ' }), { html: '<b>x</b>', title: 'Sales' });
assert.deepEqual(widgetFromToolInput({ html: '<b>x</b>', title: '' }), { html: '<b>x</b>' });
assert.equal(widgetFromToolInput({ html: '   ' }), null);
assert.equal(widgetFromToolInput({ title: 'no html' }), null);
assert.equal(widgetFromToolInput('<b>x</b>'), null);

// What a frame posts is untrusted: the chat checks and normalizes every message.
const read = (message: Record<string, unknown>) => readWidgetFrameMessage({ source: 'bubble-widget', ...message });
assert.equal(readWidgetFrameMessage({ type: 'ready' }), null, 'foreign source');
assert.equal(readWidgetFrameMessage('ready'), null);
assert.equal(read({ type: 'eval' }), null, 'unknown type');
assert.deepEqual(read({ type: 'size', height: 120.4 }), { source: 'bubble-widget', type: 'size', height: 120 });
for (const height of [0, -5, Number.NaN, Infinity, '300']) assert.equal(read({ type: 'size', height }), null, String(height));
assert.deepEqual(read({ type: 'prompt', text: '  Explain  ' }), { source: 'bubble-widget', type: 'prompt', text: 'Explain' });
assert.equal(read({ type: 'prompt', text: '   ' }), null);
assert.equal(read({ type: 'prompt', text: 'x'.repeat(WIDGET_PROMPT_MAX_CHARS + 1) }), null);
assert.equal(read({ type: 'prompt', text: { toString: () => 'x' } }), null);
assert.deepEqual(read({ type: 'open-link', url: 'HTTPS://Example.com/a b' }), { source: 'bubble-widget', type: 'open-link', url: 'https://example.com/a%20b' });
for (const url of ['javascript:alert(1)', 'file:///etc/passwd', 'bubble-widget://frame/index.html', 'notaurl', `https://x.com/${'a'.repeat(9000)}`,
  `https://trusted.example${'.a'.repeat(30)}@attacker.example/collect`, 'https://user:pass@example.com/']) {
  assert.equal(read({ type: 'open-link', url }), null, url.slice(0, 40));
}
assert.equal(read({ type: 'copy', text: '' }), null);
// What a confirmation shows: hidden characters drawn as code points; line
// breaks, tabs and emoji joiners as they are.
assert.equal(visibleWidgetText('Explain\u202Ereversed\u200B!'), 'Explain⟨U+202E⟩reversed⟨U+200B⟩!');
assert.equal(visibleWidgetText('a\nb\tc 👩\u200D💻'), 'a\nb\tc 👩\u200D💻');
assert.equal(visibleWidgetText('bell\u0007'), 'bell⟨U+0007⟩');
assert.deepEqual(read({ type: 'state', state: { tab: 2, nested: [1, 'a', null] } }), { source: 'bubble-widget', type: 'state', state: { tab: 2, nested: [1, 'a', null] } });
assert.deepEqual(read({ type: 'state', state: { when: new Date(0), skip: undefined } }), { source: 'bubble-widget', type: 'state', state: { when: '1970-01-01T00:00:00.000Z' } }, 'stored as JSON would store it');
assert.deepEqual(read({ type: 'state' }), { source: 'bubble-widget', type: 'state', state: null });
const loop: Record<string, unknown> = {};
loop.self = loop;
assert.equal(read({ type: 'state', state: loop }), null, 'cyclic');
assert.equal(read({ type: 'state', state: { n: 1n } }), null, 'BigInt');
assert.equal(read({ type: 'state', state: 'x'.repeat(WIDGET_STATE_MAX_CHARS) }), null, 'oversized');
// A compact value that expands exponentially as JSON (one object referenced
// twice at each level, as a message carries it) is refused at the cap, not
// after building the whole string.
let shared: unknown = { leaf: 1 };
for (let level = 0; level < 24; level += 1) shared = { a: shared, b: shared };
const started = performance.now();
assert.equal(read({ type: 'state', state: structuredClone(shared) }), null, 'shared-reference expansion');
assert.ok(performance.now() - started < 200, `refused within the budget (${Math.round(performance.now() - started)} ms)`);
assert.equal(read({ type: 'focus' }), null, 'no message type the chat does not handle');
assert.equal((read({ type: 'error', message: 'e'.repeat(2000) }) as { message: string }).message.length, 500);
assert.deepEqual(read({ type: 'error', message: 42 }), { source: 'bubble-widget', type: 'error', message: 'Widget error' });

async function main() {
  const ok = await showWidgetTool({ html: '<div>chart</div>' });
  assert.equal(ok.isError, undefined);
  assert.match(ok.content[0].text, /shown to the user/);
  assert.equal((await showWidgetTool({ html: ' ' })).isError, true);
  assert.equal((await showWidgetTool({})).isError, true);
  const big = await showWidgetTool({ html: 'x'.repeat(WIDGET_HTML_MAX_CHARS + 1) });
  assert.equal(big.isError, true);
  assert.match(big.content[0].text, /limit/);
  const external = await showWidgetTool({ html: '<script src="https://cdn.example.com/x.js"></script>' });
  assert.equal(external.isError, undefined, 'an external URL is a note, not a failure');
  assert.match(external.content[0].text, /no network/);
  assert.equal((await showWidgetTool({ html: '<script src="bubble-widget://lib/chart.js"></script>' })).content[0].text.includes('no network'), false);

  // The description teaches the sandbox's rules.
  for (const part of ['bubble-widget://lib/chart.js', '--text-primary', 'bubble.sendPrompt', 'No network', 'proactively', 'Never write files or start a server', "'bubble-theme'"]) {
    assert.ok(SHOW_WIDGET_DESCRIPTION.includes(part), part);
  }
  console.log('widget-tool: name matching, input parsing, frame message boundary, validation and description passed');
}

void main().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});

// A script that doesn't parse is refused with where it broke, so the agent can fix it.
void (async () => {
  const { findWidgetScriptSyntaxError } = await import('../../src/electron/libs/widget-tool');
  const broken = '<div id="a"></div>\n<script>\nconst ok = 1;\n</script>\n<script>\nfunction f( {\n  return 1;\n}\n</script>';
  const message = findWidgetScriptSyntaxError(broken);
  assert.ok(message && message.startsWith('script 2, line '), String(message));
  const refused = await showWidgetTool({ html: broken });
  assert.equal(refused.isError, true);
  assert.match(refused.content[0].text, /not shown: script 2/);
  // Library tags, JSON data and module scripts aren't compiled as classic scripts.
  assert.equal(findWidgetScriptSyntaxError('<script src="bubble-widget://lib/d3.js"></script><script type="application/json">{"a":1}</script><script type="module">import x from "y"; export {}</script><script>const a = `x`;</script>'), null);
  console.log('widget-tool: syntax errors refused with location; libraries, JSON and modules skipped');
})().catch((error) => { console.error(error); process.exitCode = 1; });

// The guide: core always, topics on request, every topic knows the others.
void (async () => {
  const { widgetGuideTool, SHOW_WIDGET_DESCRIPTION: description } = await import('../../src/electron/libs/widget-tool');
  const core = (await widgetGuideTool({})).content[0].text;
  assert.match(core, /# Widget guide/);
  assert.match(core, /More topics: chart, diagram, interactive, mockup/);
  assert.ok(!core.includes('## Charts'));
  const chart = (await widgetGuideTool({ topics: ['chart', 'nonsense'] })).content[0].text;
  assert.match(chart, /## Charts/);
  assert.ok(!chart.includes('## Diagrams'));
  // Every class the guide names is in the shell's design system.
  const { WIDGET_DESIGN_CSS } = await import('../../src/electron/widget-design-system');
  for (const name of ['.metrics', '.metric', '.panel', '.card', '.legend', '.swatch', '.chart', '.badge', '.row', '.stack', '.grid', '.arr', '.leader', '.box', '.th', '.ts', '.c-teal', '.tint-amber', '--series-8', '--grid', '--axis', '--bg-danger']) {
    assert.ok(WIDGET_DESIGN_CSS.includes(name), name);
  }
  assert.match(description, /call widget_guide/);
  console.log('widget-tool: guide topics on demand; the design system has every class the guide names');
})().catch((error) => { console.error(error); process.exitCode = 1; });
