// Chat Markdown: bold and italics next to CJK punctuation render as emphasis.
// Plain CommonMark refuses `**“…”**` followed by a CJK letter (the closing
// delimiter is preceded by punctuation and followed by a letter), so models'
// Chinese/Japanese answers showed literal asterisks.
import assert from 'node:assert/strict';
import { unified } from 'unified';
import remarkParse from 'remark-parse';
import { MARKDOWN_REMARK_PLUGINS } from '../../src/ui/render/markdown-plugins';

function emphasis(markdown: string): string[] {
  const processor = unified().use(remarkParse);
  for (const plugin of MARKDOWN_REMARK_PLUGINS) processor.use(plugin as never);
  const tree = processor.runSync(processor.parse(markdown));
  const found: string[] = [];
  const walk = (node: { type: string; children?: unknown[] }) => {
    if (node.type === 'strong' || node.type === 'emphasis') {
      const text = JSON.stringify(node).match(/"value":"([^"]*)"/g)?.map((part) => part.slice(9, -1)).join('') ?? '';
      found.push(`${node.type}:${text}`);
    }
    for (const child of (node.children ?? []) as typeof node[]) walk(child);
  };
  walk(tree as never);
  return found;
}

assert.deepEqual(emphasis('就是**“利息再生利息”**带来的额外增长'), ['strong:“利息再生利息”'], 'closing ** after a quote, before a letter');
assert.deepEqual(emphasis('**“引号”**开头'), ['strong:“引号”']);
assert.deepEqual(emphasis('结尾是**（括号）**。'), ['strong:（括号）'], 'opening ** after a letter, before a bracket');
assert.deepEqual(emphasis('这是*「强调」*内容'), ['emphasis:「强调」'], 'single-star emphasis too');
assert.deepEqual(emphasis('日本語の**「強調」**です'), ['strong:「強調」'], 'Japanese brackets');

// Behaviour that already worked is unchanged.
assert.deepEqual(emphasis('这是**重点**内容'), ['strong:重点']);
assert.deepEqual(emphasis('英文 **bold** and *it* text'), ['strong:bold', 'emphasis:it']);
assert.deepEqual(emphasis('a**b**c'), ['strong:b']);
assert.deepEqual(emphasis('2 * 3 * 4 and snake_case_name'), [], 'arithmetic and identifiers are not emphasis');
assert.deepEqual(emphasis('`**“不是加粗”**`'), [], 'code spans stay literal');

console.log('markdown-cjk: emphasis beside CJK punctuation renders; Latin, arithmetic and code are unchanged');
