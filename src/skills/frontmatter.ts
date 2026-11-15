export interface ParsedFrontmatter {
  attributes: Record<string, unknown>;
  body: string;
}

export function parseFrontmatter(raw: string): ParsedFrontmatter {
  if (!raw.startsWith("---\n") && !raw.startsWith("---\r\n")) {
    return { attributes: {}, body: raw };
  }

  const normalized = raw.replace(/\r\n/g, "\n");
  const end = normalized.indexOf("\n---\n", 4);
  if (end === -1) {
    return { attributes: {}, body: raw };
  }

  const frontmatter = normalized.slice(4, end);
  const body = normalized.slice(end + 5);
  return {
    attributes: parseFrontmatterBlock(frontmatter),
    body,
  };
}

function parseFrontmatterBlock(block: string): Record<string, unknown> {
  const attributes: Record<string, unknown> = {};
  const lines = block.split("\n");
  let currentListKey: string | null = null;

  for (let index = 0; index < lines.length; index++) {
    const line = lines[index];
    if (!line.trim()) continue;

    const listMatch = line.match(/^\s*-\s+(.*)$/);
    if (listMatch && currentListKey) {
      const current = attributes[currentListKey];
      if (Array.isArray(current)) {
        current.push(parseScalar(listMatch[1].trim()));
      } else {
        attributes[currentListKey] = [parseScalar(listMatch[1].trim())];
      }
      continue;
    }

    const keyValueMatch = line.match(/^([A-Za-z0-9_-]+):\s*(.*)$/);
    if (!keyValueMatch) {
      currentListKey = null;
      continue;
    }

    const [, key, rawValue] = keyValueMatch;
    // A block scalar (`key: |` or `key: >`): the indented lines that follow
    // are the value, kept as lines or folded into one.
    const blockScalar = rawValue.trim().match(/^([|>])[+-]?\d*$/);
    if (blockScalar) {
      const collected: string[] = [];
      while (index + 1 < lines.length && (/^\s+\S/.test(lines[index + 1]) || !lines[index + 1].trim())) {
        collected.push(lines[++index]);
      }
      const indent = Math.min(
        ...collected.filter((text) => text.trim()).map((text) => text.length - text.trimStart().length),
      );
      const text = collected.map((entry) => (entry.trim() ? entry.slice(indent) : ""));
      attributes[key] = (blockScalar[1] === "|" ? text.join("\n") : text.join(" ").replace(/\s+/g, " ")).trim();
      currentListKey = null;
      continue;
    }
    if (!rawValue.trim()) {
      attributes[key] = [];
      currentListKey = key;
      continue;
    }

    attributes[key] = parseScalar(rawValue.trim());
    currentListKey = null;
  }

  return attributes;
}

function parseScalar(value: string): unknown {
  const unquoted = value.replace(/^"(.*)"$/, "$1").replace(/^'(.*)'$/, "$1");
  if (unquoted === "true") return true;
  if (unquoted === "false") return false;
  if (/^-?\d+$/.test(unquoted)) return Number.parseInt(unquoted, 10);
  return unquoted;
}

