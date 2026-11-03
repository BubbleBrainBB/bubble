import type { CanonicalToolKind, ToolStatus } from '../types';
import {
  classifyComputerUseAction,
  formatComputerUseLabel,
  parseMcpToolName,
} from '../../shared/computer-use';
import { isShowWidgetToolName, isWidgetGuideToolName, widgetFromToolInput } from '../../shared/widget';

function isRecord(value: unknown): value is Record<string, unknown> {
  return !!value && typeof value === 'object' && !Array.isArray(value);
}

export function safeJsonStringify(value: unknown, space?: number): string {
  try {
    return JSON.stringify(
      value,
      (_key, v) => (typeof v === 'bigint' ? v.toString() : v),
      space
    );
  } catch {
    try {
      return String(value);
    } catch {
      return '[unserializable]';
    }
  }
}

function getStringField(input: unknown, key: string): string | null {
  if (!isRecord(input)) return null;
  const value = input[key];
  return typeof value === 'string' && value.trim() ? value : null;
}

function getProviderDisplayTitle(input: unknown): string | null {
  return (
    getStringField(input, '__aegisDisplayTitle') ||
    getStringField(input, 'displayTitle') ||
    getStringField(input, 'toolTitle')
  );
}

export function getToolSummary(name: string, input: unknown): string {
  const providerTitle = getProviderDisplayTitle(input);
  if (providerTitle) {
    return providerTitle;
  }
  // The widget's HTML is no summary; its title is.
  if (isShowWidgetToolName(name)) return widgetFromToolInput(input)?.title || getStringField(input, 'title') || 'Widget';
  if (isWidgetGuideToolName(name)) return 'Widget design guide';

  switch (name.trim().toLowerCase()) {
    case 'Bash':
    case 'bash':
      return getStringField(input, 'command') || getStringField(input, 'cmd') || '';
    case 'read':
    case 'read_image':
    case 'write':
    case 'edit':
    case 'delete':
      return (
        getStringField(input, 'file_path') ||
        getStringField(input, 'path') ||
        getStringField(input, 'filename') ||
        ''
      );
    case 'ls':
      return getStringField(input, 'path') || '.';
    case 'glob':
    case 'grep':
      return getStringField(input, 'pattern') || '';
    case 'askuserquestion':
    case 'question': {
      if (!isRecord(input)) return '';
      const questions = input.questions;
      if (!Array.isArray(questions) || questions.length === 0) return '';
      const first = questions[0];
      if (!isRecord(first)) return '';
      return getStringField(first, 'question') || '';
    }
    case 'task': {
      const desc = getStringField(input, 'description');
      if (desc) return desc;
      const prompt = getStringField(input, 'prompt');
      return prompt ? prompt.slice(0, 50) : '';
    }
    case 'image_gen':
    case 'image_edit':
    case 'image_to_video':
    case 'reference_to_video':
    case 'imagine':
    case 'imagine-video':
      return getStringField(input, 'prompt') || '';
    default: {
      const json = safeJsonStringify(input);
      return json.length > 80 ? json.slice(0, 80) : json;
    }
  }
}

// ── Readable display ────────────────────────────────────────────────────────
// Humanized "verb + target" rendering for tool calls. Verb conjugates with
// status so the live spinner reads "Reading file.ts" while the completed entry
// reads "Read file.ts". Inspired by Synara's deriveReadableCommandDisplay.

export interface ReadableToolDisplay {
  verb: string;
  target: string;
}

type VerbPair = readonly [present: string, past: string];

const TOOL_VERBS: Record<string, VerbPair> = {
  Read: ['Reading', 'Read'],
  Write: ['Writing', 'Wrote'],
  Edit: ['Editing', 'Edited'],
  MultiEdit: ['Editing', 'Edited'],
  Delete: ['Deleting', 'Deleted'],
  Glob: ['Finding', 'Found'],
  Grep: ['Searching', 'Searched'],
  LS: ['Listing', 'Listed'],
  WebFetch: ['Fetching', 'Fetched'],
  WebSearch: ['Searching', 'Searched'],
  Task: ['Running', 'Ran'],
  TodoWrite: ['Updating', 'Updated'],
  NotebookEdit: ['Editing', 'Edited'],
  read: ['Reading', 'Read'],
  read_image: ['Reading image', 'Read image'],
  write: ['Writing', 'Wrote'],
  edit: ['Editing', 'Edited'],
  delete: ['Deleting', 'Deleted'],
  glob: ['Finding', 'Found'],
  grep: ['Searching', 'Searched'],
  web_fetch: ['Fetching', 'Fetched'],
  web_search: ['Searching', 'Searched'],
  task: ['Running', 'Ran'],
  todo_write: ['Updating', 'Updated'],
  image_gen: ['Generating', 'Generated'],
  image_edit: ['Editing', 'Edited'],
  image_to_video: ['Animating', 'Animated'],
  reference_to_video: ['Animating', 'Animated'],
  imagine: ['Generating', 'Generated'],
  'imagine-video': ['Animating', 'Animated'],
};

const DEFAULT_VERB: VerbPair = ['Running', 'Ran'];

function pickVerb(pair: VerbPair, status: ToolStatus): string {
  return status === 'pending' ? pair[0] : pair[1];
}

function truncate(value: string, max: number): string {
  const trimmed = value.trim();
  if (trimmed.length <= max) return trimmed;
  return `${trimmed.slice(0, Math.max(0, max - 1)).trimEnd()}…`;
}

function lastPathSegment(path: string): string {
  const cleaned = path.replace(/^['"]|['"]$/g, '');
  const parts = cleaned.split('/').filter(Boolean);
  return parts[parts.length - 1] || cleaned;
}

const SHELL_WRAPPER_RE = /^(?:(?:[\w.-]+\/)+)?(?:bash|sh|zsh|fish|dash)\s+(?:-l\s+)?-l?c\s+(['"])([\s\S]*)\1\s*$/;

function unwrapShellCommand(command: string): string {
  const trimmed = command.trim();
  const match = trimmed.match(SHELL_WRAPPER_RE);
  if (match) {
    return match[2].trim();
  }
  return trimmed;
}

function splitFirstToken(command: string): { head: string; rest: string } {
  const trimmed = command.trim();
  const match = trimmed.match(/^(\S+)(?:\s+([\s\S]*))?$/);
  if (!match) return { head: '', rest: '' };
  return { head: match[1], rest: (match[2] || '').trim() };
}

const HEREDOC_PREVIEW_CHARS = 200;

interface ShellStatement {
  /** The statement as written (pipes included), plus a heredoc preview. */
  text: string;
  /** Pipeline stages as written, without heredoc bodies. */
  stages: string[];
}

function collapseWhitespace(value: string): string {
  return value.replace(/\s+/g, ' ').trim();
}

/**
 * Split a shell script into statements on `&&`, `||`, `;`, `&` and newlines,
 * ignoring separators inside quotes, `$(...)` and heredoc bodies. Pipeline
 * stages stay inside their statement: `grep x | head` is one unit of work.
 */
function splitShellStatements(script: string): ShellStatement[] {
  const statements: ShellStatement[] = [];
  // `owner` is the index of the statement that opened the heredoc.
  const pendingHeredocs: Array<{ delimiter: string; stripTabs: boolean; owner: number }> = [];
  let current = '';
  let stageStarts: number[] = [];
  let quote: string | null = null;
  let depth = 0;

  const push = () => {
    const bounds = [0, ...stageStarts, current.length + 1];
    const stages = bounds.slice(0, -1)
      .map((start, index) => collapseWhitespace(current.slice(start, bounds[index + 1] - 1)))
      .map(stripControlKeywords)
      .filter(Boolean);
    if (stages.length > 0) statements.push({ text: stripControlKeywords(collapseWhitespace(current)), stages });
    current = '';
    stageStarts = [];
  };

  for (let i = 0; i < script.length; i += 1) {
    const ch = script[i];
    const next = script[i + 1];

    if (quote) {
      current += ch;
      if (ch === '\\' && quote === '"' && next !== undefined) {
        current += next;
        i += 1;
      } else if (ch === quote) {
        quote = null;
      }
      continue;
    }

    if (ch === '\\' && next !== undefined) {
      current += next === '\n' ? ' ' : ch + next;
      i += 1;
      continue;
    }
    if (ch === "'" || ch === '"' || ch === '`') {
      quote = ch;
      current += ch;
      continue;
    }
    if (ch === '#' && (current === '' || /\s/.test(current[current.length - 1]))) {
      while (i + 1 < script.length && script[i + 1] !== '\n') i += 1;
      continue;
    }
    // `<<` opens a heredoc; `<<<` is a here-string and has no body.
    if (ch === '<' && next === '<' && script[i + 2] !== '<' && script[i - 1] !== '<' && depth === 0) {
      const match = script.slice(i + 2).match(/^(-?)\s*\\?(['"]?)([\w.-]+)\2/);
      if (match) pendingHeredocs.push({ delimiter: match[3], stripTabs: match[1] === '-', owner: statements.length });
    }

    if (ch === '\n') {
      if (depth === 0) push();
      else current += ' ';
      // Heredoc bodies are never statements of their own. A short prefix is
      // kept on the statement that opened them, so a `python3 - <<EOF` title
      // still hints at the script.
      while (pendingHeredocs.length > 0) {
        const { delimiter, stripTabs, owner } = pendingHeredocs.shift()!;
        while (i < script.length) {
          const lineEnd = script.indexOf('\n', i + 1);
          const line = script.slice(i + 1, lineEnd === -1 ? script.length : lineEnd);
          i = lineEnd === -1 ? script.length : lineEnd;
          // Bash ends a heredoc only on a line that is exactly the delimiter;
          // `<<-` additionally strips leading tabs (never spaces).
          if ((stripTabs ? line.replace(/^\t+/, '') : line) === delimiter) break;
          const statement = statements[owner];
          if (statement && statement.text.length < HEREDOC_PREVIEW_CHARS) {
            statement.text = collapseWhitespace(`${statement.text} ${line}`);
          }
        }
      }
      continue;
    }

    if (ch === '(') depth += 1;
    if (ch === ')' && depth > 0) depth -= 1;
    if (depth > 0 || ch === ')') {
      current += ch;
      continue;
    }

    if ((ch === '&' && next === '&') || (ch === '|' && next === '|')) {
      push();
      i += 1;
      continue;
    }
    if (ch === ';') {
      push();
      continue;
    }
    if (ch === '&') {
      const prev = current[current.length - 1];
      // `2>&1`, `>&2` and `&>` are redirections, not background separators.
      if (prev === '>' || prev === '<' || next === '>') {
        current += ch;
      } else {
        push();
      }
      continue;
    }
    if (ch === '|') {
      current += ch;
      if (next === '&') {
        current += next;
        i += 1;
      }
      stageStarts.push(current.length);
      continue;
    }
    current += ch;
  }
  push();
  return statements;
}

const SHELL_CONTROL_KEYWORD_RE = /^(?:do|then|else|done|fi|esac|\{|\}|!)(?=\s|$)\s*/;

function stripControlKeywords(statement: string): string {
  let value = statement;
  let match = value.match(SHELL_CONTROL_KEYWORD_RE);
  while (match) {
    value = value.slice(match[0].length);
    match = value.match(SHELL_CONTROL_KEYWORD_RE);
  }
  return value;
}

/**
 * Builtins that change shell state without running a program. `source` / `.`
 * (run a script) and `trap` (runs a handler later) are deliberately absent.
 */
const SHELL_SETUP_COMMANDS = new Set([
  'cd', 'pushd', 'popd', 'export', 'set', 'unset', 'shopt',
  'ulimit', 'umask', 'local', 'declare', 'readonly',
  'break', 'continue', 'true', ':',
]);

// One `NAME=value` word. Matched word by word from a loop: a single
// `(?:...)+` pattern backtracks exponentially on input like `x=a=a=a=...`.
const ASSIGNMENT_WORD_RE = /^[A-Za-z_]\w*\+?=(?:'[^']*'|"(?:[^"\\]|\\.)*"|[^\s'"]*)(?:\s+|$)/;

function isPureAssignment(statement: string): boolean {
  let rest = statement;
  while (rest) {
    const match = rest.match(ASSIGNMENT_WORD_RE);
    if (!match) return false;
    rest = rest.slice(match[0].length);
  }
  return true;
}

function isShellSetup(statement: string): boolean {
  if (!statement) return true;
  // A command substitution runs a program, e.g. `X=$(make)`.
  if (/\$\(|`/.test(statement)) return false;
  if (isPureAssignment(statement)) return true;
  return SHELL_SETUP_COMMANDS.has(splitFirstToken(statement).head);
}

/**
 * The statement a command row is about (the first one doing real work, not
 * `cd dir &&` or `VAR=...`) and how many other working statements it hides.
 */
function analyzeShellCommand(command: string): { lead: ShellStatement | null; working: ShellStatement[] } {
  const statements = splitShellStatements(unwrapShellCommand(command));
  const working = statements.filter((statement) => !isShellSetup(statement.stages[0]));
  return { lead: working[0] ?? statements[0] ?? null, working };
}

/**
 * A command row shows what the model said the command does (Bash's
 * `description`), or else the command itself. It never re-describes the
 * command from its text: what a program does with its arguments cannot be
 * read off shell syntax, and a guessed "Read 5" is worse than the literal.
 */
function describeBashCommand(command: string, description: string | null, status: ToolStatus): ReadableToolDisplay {
  if (/computer-use\/(?:skills\/)?computer-use\/SKILL\.md|skills\/computer-use\/SKILL\.md/.test(command)) {
    return {
      verb: pickVerb(['Reading', 'Read'], status),
      target: 'Computer Use skill',
    };
  }
  if (description) {
    return { verb: '', target: truncate(collapseWhitespace(description), 80) };
  }
  const { lead, working } = analyzeShellCommand(command);
  const text = truncate(lead ? lead.text : collapseWhitespace(command), 60);
  const hidden = Math.max(0, working.length - 1);
  return {
    verb: pickVerb(DEFAULT_VERB, status),
    target: hidden > 0 ? `${text} +${hidden} more` : text,
  };
}

export function deriveReadableToolDisplay(
  name: string,
  input: unknown,
  status: ToolStatus
): ReadableToolDisplay {
  const providerTitle = getProviderDisplayTitle(input);
  if (providerTitle) {
    return { verb: '', target: providerTitle };
  }

  if (isShowWidgetToolName(name)) {
    return { verb: status === 'pending' ? 'Drawing' : 'Drew', target: getStringField(input, 'title') || 'a widget' };
  }
  if (isWidgetGuideToolName(name)) {
    return { verb: status === 'pending' ? 'Reading' : 'Read', target: 'the widget design guide' };
  }
  if (name === 'wait_agent') return { verb: status === 'pending' ? 'Waiting for' : 'Waited for', target: 'subagent result' };
  if (name === 'send_input') return { verb: status === 'error' ? 'Could not send' : status === 'pending' ? 'Sending' : 'Sent', target: 'supplementary message' };
  if (name === 'close_agent') return { verb: status === 'pending' ? 'Stopping' : 'Stopped', target: 'subagent' };
  if (name === 'Bash' || name === 'bash') {
    const command = getStringField(input, 'command') || getStringField(input, 'cmd');
    if (command) {
      return describeBashCommand(command, getStringField(input, 'description'), status);
    }
    return { verb: pickVerb(DEFAULT_VERB, status), target: 'command' };
  }

  if (name === 'Read' || name === 'Write' || name === 'Edit' || name === 'Delete' || name === 'MultiEdit'
    || name === 'read' || name === 'write' || name === 'edit' || name === 'delete') {
    const path =
      getStringField(input, 'file_path') ||
      getStringField(input, 'path') ||
      getStringField(input, 'filename') ||
      '';
    const verbs = TOOL_VERBS[name] || DEFAULT_VERB;
    return { verb: pickVerb(verbs, status), target: path ? lastPathSegment(path) : 'file' };
  }

  if (name === 'LS' || name === 'ls') {
    const path = getStringField(input, 'path') || '.';
    return { verb: pickVerb(TOOL_VERBS.LS, status), target: path };
  }

  if (name === 'Glob' || name === 'glob') {
    const pattern = getStringField(input, 'pattern') || '';
    return { verb: pickVerb(TOOL_VERBS.Glob, status), target: pattern || 'pattern' };
  }

  if (name === 'Grep' || name === 'grep') {
    const pattern = getStringField(input, 'pattern') || '';
    return { verb: pickVerb(TOOL_VERBS.Grep, status), target: pattern ? `for ${truncate(pattern, 40)}` : 'pattern' };
  }

  if (name === 'WebFetch' || name === 'web_fetch') {
    const url = getStringField(input, 'url') || '';
    return { verb: pickVerb(TOOL_VERBS.WebFetch, status), target: url ? truncate(url, 60) : 'url' };
  }

  if (name === 'WebSearch' || name === 'web_search') {
    const query = getStringField(input, 'query') || '';
    return { verb: pickVerb(TOOL_VERBS.WebSearch, status), target: query ? truncate(query, 60) : 'query' };
  }

  if (name === 'Task' || name === 'task' || name === 'subagent' || name === 'spawn_agent' || name === 'run_workflow') {
    const desc =
      getStringField(input, 'description') ||
      getStringField(input, 'subagent_type') ||
      getStringField(input, 'prompt') ||
      getStringField(input, 'task') ||
      getStringField(input, 'message') ||
      '';
    return { verb: pickVerb(TOOL_VERBS.Task, status), target: desc ? truncate(desc, 60) : 'subagent task' };
  }

  if (name === 'TodoWrite' || name === 'todo_write') {
    return { verb: pickVerb(TOOL_VERBS.TodoWrite, status), target: 'todo list' };
  }

  if (name === 'NotebookEdit') {
    const path = getStringField(input, 'notebook_path') || getStringField(input, 'path') || '';
    return { verb: pickVerb(TOOL_VERBS.NotebookEdit, status), target: path ? lastPathSegment(path) : 'notebook' };
  }

  if (name === 'AskUserQuestion') {
    const summary = getToolSummary(name, input);
    return { verb: pickVerb(DEFAULT_VERB, status), target: summary ? truncate(summary, 60) : 'question' };
  }

  const action = classifyComputerUseAction({
    toolName: name,
    app: getStringField(input, 'app'),
    title: getProviderDisplayTitle(input),
  });
  if (action) {
    const mappedStatus =
      status === 'error' ? 'error' : status === 'interrupted' ? 'interrupted' : status === 'success' ? 'success' : 'pending';
    return { verb: '', target: formatComputerUseLabel(action, mappedStatus) };
  }

  // MCP-style names like "mcp__server__tool" — show just the tool segment.
  if (name.startsWith('mcp__')) {
    const parts = name.split('__');
    const toolName = parts[parts.length - 1] || name;
    const summary = getToolSummary(name, input);
    return {
      verb: pickVerb(DEFAULT_VERB, status),
      target: summary ? `${toolName} ${truncate(summary, 40)}`.trim() : toolName,
    };
  }

  // Unknown tool — fall back to the legacy summary.
  const fallback = getToolSummary(name, input);
  return {
    verb: pickVerb(DEFAULT_VERB, status),
    target: fallback ? truncate(fallback, 60) : name,
  };
}

export function formatReadableToolSummary(display: ReadableToolDisplay): string {
  return `${display.verb} ${display.target}`.trim();
}

// ── Canonical kind classification ───────────────────────────────────────────
// Maps a provider's tool name (currently Claude-shaped because CodexAdapter
// translates Codex events back into Anthropic wire format) to a canonical kind
// the UI can switch on without caring which provider produced it. New tool
// names from new providers slot in here rather than fanning out across UI
// components.

type ShellExplorerKind = 'file_read' | 'pattern_search' | 'filter';

interface ShellExplorer {
  kind: ShellExplorerKind;
  /** Options that make the program write files or run other programs. */
  unsafeOptions?: string[];
  /** More operands than this name an output file (`uniq in out`). */
  maxOperands?: number;
}

/**
 * Programs that only read, and the ways each can stop being read-only.
 * `filter` programs only count after a pipe. Anything not listed here is a
 * command: exploration is a claim that nothing but reading happened.
 */
const SHELL_EXPLORERS = new Map<string, ShellExplorer>([
  ['cat', { kind: 'file_read' }],
  ['head', { kind: 'file_read' }],
  ['tail', { kind: 'file_read' }],
  ['more', { kind: 'file_read' }],
  ['bat', { kind: 'file_read' }],
  ['less', { kind: 'file_read', unsafeOptions: ['-o', '-O', '--log-file', '--LOG-FILE'] }],
  ['ls', { kind: 'pattern_search' }],
  ['tree', { kind: 'pattern_search', unsafeOptions: ['-o'] }],
  ['grep', { kind: 'pattern_search' }],
  ['rg', { kind: 'pattern_search', unsafeOptions: ['--pre'] }],
  ['ag', { kind: 'pattern_search', unsafeOptions: ['--pager'] }],
  ['find', {
    kind: 'pattern_search',
    unsafeOptions: ['-delete', '-exec', '-execdir', '-ok', '-okdir', '-fprint', '-fprint0', '-fprintf', '-fls'],
  }],
  ['fd', { kind: 'pattern_search', unsafeOptions: ['-x', '--exec', '-X', '--exec-batch'] }],
  ['glob', { kind: 'pattern_search' }],
  ['wc', { kind: 'filter' }],
  ['sort', { kind: 'filter', unsafeOptions: ['-o', '--output'] }],
  ['uniq', { kind: 'filter', maxOperands: 1 }],
  ['cut', { kind: 'filter' }],
  ['tr', { kind: 'filter' }],
  ['nl', { kind: 'filter' }],
  ['column', { kind: 'filter' }],
]);

function hasOption(args: string[], option: string): boolean {
  return args.some((arg) =>
    arg === option
    || (option.startsWith('--') && arg.startsWith(`${option}=`))
    // A single-letter option can sit in a cluster (`-ro`) or carry its value (`-oout`).
    || (/^-[A-Za-z]$/.test(option) && /^-[A-Za-z0-9]+$/.test(arg) && arg.slice(1).includes(option[1])));
}

/** What a pipeline stage explores, or null when it may do anything but read. */
function getStageExploration(stage: string): ShellExplorerKind | null {
  const { head, rest } = splitFirstToken(stage);
  const explorer = SHELL_EXPLORERS.get(head);
  if (!explorer) return null;
  // Quoted text is an argument value, never an option.
  const args = stripQuoted(rest).split(/\s+/).filter(Boolean);
  if (explorer.unsafeOptions?.some((option) => hasOption(args, option))) return null;
  // A lone `-` is the stdin operand, not an option.
  const operands = args.filter((arg) => arg === '-' || !arg.startsWith('-'));
  if (explorer.maxOperands !== undefined && operands.length > explorer.maxOperands) {
    return null;
  }
  return explorer.kind;
}

function stripQuoted(text: string): string {
  return text.replace(/'[^']*'|"(?:[^"\\]|\\.)*"/g, '""');
}

/**
 * Whether a stage writes somewhere or takes a heredoc. Descriptor duplication
 * (`2>&1`) and `/dev/null` sinks write nothing; every other `>` does, and a
 * `<<` heredoc is inline input rather than a file being read.
 */
function hasWriteOrInlineInput(stage: string): boolean {
  const redirects = stripQuoted(stage)
    .replace(/(?:^|\s)(?:\d*|&)>>?\s*\/dev\/null(?=\s|$)/g, ' ')
    .replace(/(?:^|\s)\d*[<>]&[\d-](?=\s|$)/g, ' ');
  return />|<</.test(redirects);
}

/** How a statement explores, or null when it does anything but read. */
function getStatementExploration(statement: ShellStatement): 'file_read' | 'pattern_search' | null {
  if (statement.stages.some(hasWriteOrInlineInput)) return null;
  const [first, ...rest] = statement.stages.map(getStageExploration);
  if (rest.includes(null)) return null;
  return first === 'file_read' || first === 'pattern_search' ? first : null;
}

/**
 * Bash is grouped as exploration only when every working statement just
 * reads; anything that runs, writes or is not recognized is a command.
 */
function detectBashKind(command: string | null | undefined): CanonicalToolKind {
  if (!command) return 'command_execution';
  const { working } = analyzeShellCommand(command);
  const kinds = working.map(getStatementExploration);
  if (kinds.length === 0 || kinds.includes(null)) return 'command_execution';
  return kinds.includes('pattern_search') ? 'pattern_search' : 'file_read';
}

/** Whether an exploration tool call lists files rather than searching them. */
export function isFileListing(toolName: string, input: unknown): boolean {
  const normalized = toolName.trim().toLowerCase();
  if (normalized === 'ls') return true;
  if (normalized !== 'bash') return false;
  const command = getStringField(input, 'command') || getStringField(input, 'cmd');
  const lead = command ? analyzeShellCommand(command).lead : null;
  if (!lead) return false;
  const { head, rest } = splitFirstToken(lead.stages[0]);
  return head === 'ls' || head === 'tree' || (head === 'rg' && /(?:^|\s)--files(?:\s|$)/.test(rest));
}

export function classifyToolUse(toolName: string, input: unknown): CanonicalToolKind {
  const normalized = toolName.trim().toLowerCase();
  if (normalized === 'read' || normalized === 'read_image') return 'file_read';
  if (['write', 'edit', 'multiedit', 'delete', 'notebookedit'].includes(normalized)) {
    return 'file_change';
  }
  if (normalized === 'bash') {
    return detectBashKind(getStringField(input, 'command') || getStringField(input, 'cmd'));
  }
  if (normalized === 'grep' || normalized === 'glob' || normalized === 'ls') return 'pattern_search';
  if (normalized === 'websearch' || normalized === 'webfetch' || normalized === 'web_search' || normalized === 'web_fetch') return 'web_search';
  // The subagent-spawning tool is "Task" in the Claude Agent SDK and "Agent"
  // in this runtime; both carry a subagent_type input. Recognize either name,
  // or any tool that declares a subagent_type, so every runtime's subagent
  // runs render as a subagent board (not a generic tool row).
  if (normalized === 'task' || normalized === 'agent' || normalized === 'subagent') return 'subagent';
  if (getStringField(input, 'subagent_type')) return 'subagent';
  // Bubble's spawn surface: spawn_agent (one child) and run_workflow (a
  // schema'd multi-agent pipeline) both nest their children the same way.
  if (normalized === 'spawn_agent' || normalized === 'run_workflow') return 'subagent';
  // Cross-agent delegation (aegis-delegate MCP server): a delegate_task call
  // runs another agent whose trace mirrors into this session exactly like a
  // subagent — render it as one, not as a generic MCP row.
  if (normalized === 'delegate_task' || normalized.endsWith('__delegate_task')) return 'subagent';
  if (
    normalized === 'image_gen' ||
    normalized === 'image_edit' ||
    normalized === 'image_to_video' ||
    normalized === 'reference_to_video' ||
    normalized === 'imagine' ||
    normalized === 'imagine-video'
  ) {
    return 'image_view';
  }
  if (normalized === 'todowrite' || normalized === 'todo_write') return 'todo_update';
  if (normalized === 'askuserquestion' || normalized === 'question') return 'approval';
  if (
    toolName === 'remember_search' ||
    toolName === 'remember_get' ||
    toolName === 'remember_write' ||
    toolName === 'remember_recent' ||
    toolName.startsWith('aegis_memory_') ||
    toolName.endsWith('__remember_search') ||
    toolName.endsWith('__remember_get') ||
    toolName.endsWith('__remember_write') ||
    toolName.endsWith('__remember_recent')
  ) {
    return 'memory';
  }
  if (classifyComputerUseAction({ toolName, app: getStringField(input, 'app'), title: getProviderDisplayTitle(input) })) {
    return 'computer_use';
  }
  const parsed = parseMcpToolName(toolName);
  if (parsed && classifyComputerUseAction({ server: parsed.server, tool: parsed.tool, toolName })) {
    return 'computer_use';
  }
  if (toolName.startsWith('mcp__')) return 'mcp_tool_call';
  return 'unknown';
}
