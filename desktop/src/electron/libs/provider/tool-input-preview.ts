const SIMPLE_ESCAPES: Record<string, string> = { '"': '"', '\\': '\\', '/': '/', b: '\b', f: '\f', n: '\n', r: '\r', t: '\t' };

/**
 * Top-level string fields of a JSON object that may still be streaming,
 * decoded as far as they go: a value cut mid-way comes back partial (an
 * escape cut mid-way is left out), so a growing field can be shown as it is
 * written. Fields that are not strings are skipped.
 */
export function partialJsonStringFields(argumentsText: string, keys: readonly string[]): Record<string, string> {
  const wanted = new Set(keys);
  const result: Record<string, string> = {};
  const text = argumentsText;
  let depth = 0;
  let expectsKey = false;
  let key: string | undefined;
  const readString = (start: number): { value: string; end: number; complete: boolean } => {
    let value = '';
    let i = start + 1;
    while (i < text.length) {
      const char = text[i];
      if (char === '"') return { value, end: i, complete: true };
      if (char !== '\\') { value += char; i += 1; continue; }
      const next = text[i + 1];
      if (next === undefined) break;
      if (next === 'u') {
        const hex = text.slice(i + 2, i + 6);
        if (!/^[0-9a-fA-F]{4}$/.test(hex)) break;
        value += String.fromCharCode(Number.parseInt(hex, 16));
        i += 6;
        continue;
      }
      value += SIMPLE_ESCAPES[next] ?? next;
      i += 2;
    }
    return { value, end: text.length, complete: false };
  };
  for (let i = 0; i < text.length; i++) {
    const char = text[i];
    if (char === '"') {
      const atTop = depth === 1;
      const isKey = atTop && expectsKey;
      // Only wanted values are decoded; everything else is skipped quickly.
      if (!isKey && !(atTop && key && wanted.has(key))) {
        for (i += 1; i < text.length && text[i] !== '"'; i += text[i] === '\\' ? 2 : 1);
        if (atTop) key = undefined;
        continue;
      }
      const read = readString(i);
      if (isKey) { key = read.value; expectsKey = false; }
      else if (key) { result[key] = read.value; key = undefined; }
      if (!read.complete) break;
      i = read.end;
    } else if (char === '{' || char === '[') {
      depth++;
      if (depth === 1 && char === '{') expectsKey = true;
      else if (depth === 2) key = undefined;
    } else if (char === '}' || char === ']') {
      depth--;
    } else if (char === ',' && depth === 1) {
      expectsKey = true;
      key = undefined;
    }
  }
  return result;
}

/** Small display-only fields from an incomplete JSON object. Never retain the
 * growing content/patch in desktop history for every token. Canonical arguments
 * replace this preview once the SDK completes the call. */
export function toolInputPreview(argumentsText: string): Record<string, string> {
  const text = argumentsText.slice(0, 16_384);
  const fields = new Set(['path', 'file_path', 'command', 'pattern', 'query', 'url']);
  const result: Record<string, string> = {};
  let depth = 0;
  let key: string | undefined;
  let expectsKey = false;
  for (let i = 0; i < text.length; i++) {
    const char = text[i];
    if (char === '"') {
      const start = i++;
      for (; i < text.length; i++) {
        if (text[i] === '\\') { i++; continue; }
        if (text[i] === '"') break;
      }
      if (i >= text.length) break;
      if (depth !== 1) continue;
      let value: string;
      try { value = JSON.parse(text.slice(start, i + 1)); } catch { break; }
      if (expectsKey) { key = value; expectsKey = false; }
      else if (key) {
        if (fields.has(key) && value.length <= 4096) result[key] = value;
        key = undefined;
      }
    } else if (char === '{' || char === '[') {
      depth++;
      if (depth === 1 && char === '{') expectsKey = true;
      else key = undefined;
    } else if (char === '}' || char === ']') {
      depth--;
    } else if (char === ',' && depth === 1) {
      expectsKey = true;
      key = undefined;
    } else if (depth === 1 && !/[\s:]/.test(char)) {
      key = undefined;
    }
  }
  return result;
}
