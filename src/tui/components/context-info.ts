import chalk from "chalk";
import {
  matchesKey,
  truncateToWidth,
  visibleWidth,
  type Component,
  type Focusable,
  type TuiMouseEvent,
} from "@bubblebrain-ai/pi-tui";
import type { ContextUsageSnapshot } from "../../context/usage.js";
import {
  AUTOCOMPACT_BUFFER_TOKENS,
  MIN_WINDOW_FOR_RESERVE,
  OUTPUT_RESERVE_TOKENS,
} from "../../context/budget.js";
import { describeThinking } from "../formatting/thinking-display.js";
import { parseTerminalMouseWheel } from "../model/terminal-mouse.js";
import { darkTheme, type Theme } from "../model/theme.js";
import { themeBackground, themeDim, themeForeground } from "../model/theme-style.js";

const TABS = ["Context usage", "Usage limit", "Session info"] as const;
type ContextInfoTab = 0 | 1 | 2;

const MAX_PANEL_HEIGHT = 30;
const WIDE_CONTENT_BREAKPOINT = 50;

interface HitRange {
  start: number;
  end: number;
  row: number;
}

export interface ContextInfoPanelData {
  snapshot: ContextUsageSnapshot;
  sessionId?: string;
  cwd: string;
  thinking: string;
  permissionMode: string;
  turnCount: number;
  toolCallCount: number;
  compactionCount: number;
  mcpServerCount: number;
}

export interface ContextInfoComponentOptions {
  getTerminalRows(): number;
  onClose(): void;
  onRender(): void;
  copySessionId?(): Promise<void>;
  theme?: Theme;
}

interface UsageCategory {
  glyph: "◆" | "◇";
  tokens: number;
  color(value: string): string;
}

function fit(value: string, width: number): string {
  if (width <= 0) return "";
  const clipped = truncateToWidth(value, width, "");
  return clipped + " ".repeat(Math.max(0, width - visibleWidth(clipped)));
}

function formatTokenNumber(value: number): string {
  const count = Math.max(0, value);
  if (count < 1_000) return `${Math.round(count)}`;
  if (count < 1_000_000) {
    const scaled = count / 1_000;
    return `${scaled >= 10 ? scaled.toFixed(0) : scaled.toFixed(1)}k`;
  }
  const scaled = count / 1_000_000;
  return `${scaled >= 10 ? scaled.toFixed(0) : scaled.toFixed(1)}m`;
}

function formatTokens(value: number): string {
  return `${formatTokenNumber(value)} tokens`;
}

function formatPercent(value: number, total: number | undefined, precision = 1): string {
  if (!total || total <= 0) return "-";
  const percent = (Math.max(0, value) / total) * 100;
  if (percent > 0 && percent < 0.1) return "<0.1%";
  if (precision === 2) return `${percent.toFixed(2)}%`;
  if (percent >= 10) return `${percent.toFixed(0)}%`;
  return `${percent.toFixed(1)}%`;
}

function autoCompactThreshold(contextWindow: number): number {
  if (contextWindow >= MIN_WINDOW_FOR_RESERVE) {
    return Math.max(0, contextWindow - OUTPUT_RESERVE_TOKENS - AUTOCOMPACT_BUFFER_TOKENS);
  }
  return Math.floor(contextWindow * 0.75);
}

/** Largest-remainder allocation keeps the categorical grid at exactly 100 cells. */
export function allocateContextCells(values: number[]): number[] {
  const safe = values.map((value) => Math.max(0, Number.isFinite(value) ? value : 0));
  const total = safe.reduce((sum, value) => sum + value, 0);
  if (total <= 0) return safe.map((_value, index) => index === safe.length - 1 ? 100 : 0);
  const exact = safe.map((value) => (value / total) * 100);
  const result = exact.map(Math.floor);
  let remaining = 100 - result.reduce((sum, value) => sum + value, 0);
  const order = exact
    .map((value, index) => ({ index, remainder: value - result[index]! }))
    .sort((a, b) => b.remainder - a.remainder || a.index - b.index);
  for (let index = 0; remaining > 0; index = (index + 1) % order.length) {
    result[order[index]!.index]! += 1;
    remaining -= 1;
  }
  return result;
}

function buildGrid(categories: UsageCategory[], rowLength: number): string[] {
  const counts = allocateContextCells(categories.map((category) => category.tokens));
  const cells = categories.flatMap((category, index) => (
    Array.from({ length: counts[index] ?? 0 }, () => category.color(category.glyph))
  ));
  const rows: string[] = [];
  for (let index = 0; index < 100; index += rowLength) {
    rows.push(cells.slice(index, index + rowLength).join(" "));
  }
  return rows;
}

function buildLegendRow(
  glyph: string,
  label: string,
  tokens: number,
  total: number | undefined,
  contentWidth: number,
  color: (value: string) => string,
  detail?: string,
  theme?: Theme,
): string[] {
  const marker = color(glyph);
  const tokenText = formatTokens(tokens);
  const percent = formatPercent(tokens, total);
  if (contentWidth < WIDE_CONTENT_BREAKPOINT) {
    const data = `  ${tokenText.padStart(12)}  ${percent.padStart(6)}${detail ? ` · ${detail}` : ""}`;
    return [
      `${marker} ${theme ? themeForeground(theme.muted, label) : chalk.gray(label)}`,
      theme ? themeDim(theme.dim, data) : chalk.dim(data),
    ];
  }
  const styledLabel = theme ? themeForeground(theme.muted, label.padEnd(20)) : chalk.gray(label.padEnd(20));
  const line = `${marker} ${styledLabel} ${tokenText.padStart(11)}  ${percent.padStart(6)}${detail ? ` · ${detail}` : ""}`;
  return [line];
}

export function buildContextUsageLines(data: ContextInfoPanelData, contentWidth: number, theme?: Theme): string[] {
  const { snapshot } = data;
  const systemTokens = snapshot.buckets.systemPrompt.tokens;
  const messageTokens = snapshot.buckets.other.tokens;
  const overheadTokens = snapshot.buckets.tools.tokens
    + snapshot.buckets.skills.tokens
    + snapshot.buckets.deferredTools.tokens;
  const freeTokens = snapshot.freeTokens ?? 0;
  const total = snapshot.contextWindow;
  const categories: UsageCategory[] = [
    { glyph: "◆", tokens: systemTokens, color: (value) => theme ? themeForeground(theme.muted, value) : chalk.gray(value) },
    { glyph: "◆", tokens: messageTokens, color: (value) => theme ? themeForeground(theme.agent, value) : chalk.white(value) },
    { glyph: "◆", tokens: overheadTokens, color: (value) => theme ? themeForeground(theme.thinking, value) : chalk.magenta(value) },
    { glyph: "◇", tokens: freeTokens, color: (value) => theme ? themeDim(theme.dim, value) : chalk.gray.dim(value) },
  ];
  const rowLength = contentWidth >= WIDE_CONTENT_BREAKPOINT ? 20 : 10;
  const lines: string[] = [
    theme ? chalk.bold(themeForeground(theme.inputText, "Context")) : chalk.bold.white("Context"),
    "",
    theme ? themeForeground(theme.muted, `${formatTokenNumber(snapshot.usedTokens)} / ${total ? formatTokenNumber(total) : "unknown"} tokens (${formatPercent(snapshot.usedTokens, total, 2)})`) : chalk.gray(`${formatTokenNumber(snapshot.usedTokens)} / ${total ? formatTokenNumber(total) : "unknown"} tokens (${formatPercent(snapshot.usedTokens, total, 2)})`),
    theme ? themeForeground(theme.muted, snapshot.modelId || "unknown model") : chalk.gray(snapshot.modelId || "unknown model"),
    "",
    ...buildGrid(categories, rowLength),
    "",
    ...buildLegendRow("◆", "System prompt", systemTokens, total, contentWidth, (value) => theme ? themeForeground(theme.muted, value) : chalk.gray(value), undefined, theme),
    ...buildLegendRow("◆", "Messages", messageTokens, total, contentWidth, (value) => theme ? themeForeground(theme.agent, value) : chalk.white(value), undefined, theme),
  ];
  if (overheadTokens > 0) {
    lines.push(...buildLegendRow("◆", "Reasoning/overhead", overheadTokens, total, contentWidth, (value) => theme ? themeForeground(theme.thinking, value) : chalk.magenta(value), undefined, theme));
  }
  lines.push(
    ...buildLegendRow("◇", "Free", freeTokens, total, contentWidth, (value) => theme ? themeDim(theme.dim, value) : chalk.gray.dim(value), undefined, theme),
    "",
    ...buildLegendRow("◈", "Tool definitions", snapshot.buckets.tools.tokens, total, contentWidth, (value) => theme ? themeForeground(theme.accent, value) : chalk.cyan(value), `${snapshot.toolCount} tools`, theme),
    ...buildLegendRow("◈", "Skills", snapshot.buckets.skills.tokens, total, contentWidth, (value) => theme ? themeForeground(theme.accent, value) : chalk.cyan(value), `${snapshot.skillCount} skills`, theme),
    ...buildLegendRow("◈", "MCP servers", snapshot.buckets.deferredTools.tokens, total, contentWidth, (value) => theme ? themeForeground(theme.accent, value) : chalk.cyan(value), `${data.mcpServerCount} servers`, theme),
    "",
  );

  if (total && total > 0) {
    const threshold = autoCompactThreshold(total);
    const remaining = Math.max(0, threshold - snapshot.usedTokens);
    const thresholdPercent = Math.round((threshold / total) * 100);
    const text = `Auto-compact at ${thresholdPercent}% · ~${formatTokenNumber(remaining)} tokens remaining`;
    lines.push(theme
      ? themeForeground(remaining <= total * 0.1 ? theme.warning : theme.muted, text)
      : remaining <= total * 0.1 ? chalk.yellow(text) : chalk.gray(text));
  } else {
    lines.push(theme ? themeForeground(theme.muted, "Auto-compact threshold unavailable") : chalk.gray("Auto-compact threshold unavailable"));
  }
  lines.push(
    "",
    theme ? themeDim(theme.dim, `Turns: ${data.turnCount} · Tool calls: ${data.toolCallCount} · Compactions: ${data.compactionCount}`) : chalk.dim(`Turns: ${data.turnCount} · Tool calls: ${data.toolCallCount} · Compactions: ${data.compactionCount}`),
  );
  return lines;
}

function buildUsageLimitLines(data: ContextInfoPanelData, theme?: Theme): string[] {
  return [
    theme ? chalk.bold(themeForeground(theme.inputText, "Usage limit")) : chalk.bold.white("Usage limit"),
    "",
    theme ? themeForeground(theme.muted, "Provider billing and plan limits are not available for this session.") : chalk.gray("Provider billing and plan limits are not available for this session."),
    theme ? themeDim(theme.dim, "Context-window usage is available in the Context usage tab.") : chalk.dim("Context-window usage is available in the Context usage tab."),
    "",
    theme ? themeDim(theme.dim, `Provider: ${data.snapshot.providerId || "unknown"}`) : chalk.dim(`Provider: ${data.snapshot.providerId || "unknown"}`),
  ];
}

/**
 * A model with real grades shows the grade. One that only switches thinking on
 * and off, or always thinks, has no grade to show: its stored level is a
 * stand-in, so the row says whether it thinks instead.
 */
export function thinkingRow(data: Pick<ContextInfoPanelData, "snapshot" | "thinking">): [string, string] {
  const thinking = describeThinking(data.snapshot.providerId, data.snapshot.modelId, data.thinking);
  if (thinking.kind === "always") return ["Thinking", "always on"];
  if (thinking.kind === "toggle") return ["Thinking", thinking.on ? "on" : "off"];
  return ["Reasoning effort", thinking.level];
}

function buildSessionInfoLines(data: ContextInfoPanelData, theme?: Theme): string[] {
  const rows: Array<[string, string]> = [
    ["Session ID", data.sessionId ?? "unavailable"],
    ["Working directory", data.cwd],
    ["Provider", data.snapshot.providerId || "unknown"],
    ["Model", data.snapshot.modelId || "unknown"],
    thinkingRow(data),
    ["Permission mode", data.permissionMode],
    ["Turns", `${data.turnCount}`],
    ["Tool calls", `${data.toolCallCount}`],
    ["Compactions", `${data.compactionCount}`],
  ];
  return [
    theme ? chalk.bold(themeForeground(theme.inputText, "Session info")) : chalk.bold.white("Session info"),
    "",
    ...rows.flatMap(([label, value]) => [theme ? themeForeground(theme.muted, label) : chalk.gray(label), `  ${value}`, ""]),
  ];
}

export class ContextInfoComponent implements Component, Focusable {
  focused = false;
  private activeTab: ContextInfoTab = 0;
  private offset = 0;
  private bodyRows = 0;
  private contentRows = 0;
  private frameWidth = 0;
  private frameHeight = 0;
  private tabHits: HitRange[] = [];
  private copyHit: HitRange | undefined;
  private hoveredTab: number | undefined;
  private closeHovered = false;
  private copyHovered = false;
  private copyStatus: "idle" | "copying" | "copied" | "failed" = "idle";

  constructor(
    private readonly data: ContextInfoPanelData,
    private readonly options: ContextInfoComponentOptions,
  ) {}

  render(width: number): string[] {
    const theme = this.options.theme ?? darkTheme;
    const frameWidth = Math.max(1, Math.floor(width));
    const frameHeight = Math.max(1, Math.min(MAX_PANEL_HEIGHT, this.options.getTerminalRows() - 4));
    this.frameWidth = frameWidth;
    this.frameHeight = frameHeight;
    if (frameWidth < 8 || frameHeight < 5) {
      return [truncateToWidth("Context usage · Esc close", frameWidth, "")].slice(0, frameHeight);
    }

    const innerWidth = frameWidth - 2;
    const horizontal = "─".repeat(Math.max(0, frameWidth - 2));
    const topDashCount = Math.max(0, frameWidth - 7);
    const close = this.closeHovered ? chalk.bold(themeForeground(theme.inputText, "[✗]")) : themeForeground(theme.muted, "[✗]");
    const top = themeForeground(theme.border, `┌${"─".repeat(topDashCount)}`) + ` ${close} ` + themeForeground(theme.border, "┐");
    const bottom = themeForeground(theme.border, `└${horizontal}┘`);
    const separator = themeForeground(theme.border, `├${horizontal}┤`);

    const tabLines = this.renderTabs(innerWidth);
    const contentWidth = Math.max(1, innerWidth - 4);
    const content = this.activeTab === 0
      ? buildContextUsageLines(this.data, contentWidth, this.options.theme)
      : this.activeTab === 1
        ? buildUsageLimitLines(this.data, this.options.theme)
        : buildSessionInfoLines(this.data, this.options.theme);
    this.bodyRows = Math.max(0, frameHeight - (4 + tabLines.length));
    this.contentRows = content.length;
    const maxOffset = Math.max(0, content.length - this.bodyRows);
    this.offset = Math.min(this.offset, maxOffset);
    const visible = content.slice(this.offset, this.offset + this.bodyRows);
    while (visible.length < this.bodyRows) visible.push("");
    const body = visible.map((line) => this.frameLine(`  ${fit(line, contentWidth)}  `, innerWidth));
    const footer = this.frameLine(this.renderFooter(innerWidth), innerWidth);
    return [top, ...tabLines.map((line) => this.frameLine(line, innerWidth)), separator, ...body, footer, bottom]
      .slice(0, frameHeight)
      .map((line) => truncateToWidth(line, frameWidth, ""));
  }

  handleInput(data: string): void {
    const wheel = parseTerminalMouseWheel(data);
    if (wheel.length > 0) {
      for (const direction of wheel) this.scroll(direction === "up" ? -3 : 3);
      return;
    }
    if (matchesKey(data, "escape")) return this.options.onClose();
    if (matchesKey(data, "tab") || matchesKey(data, "right") || data === "l") return this.stepTab(1);
    if (matchesKey(data, "shift+tab") || matchesKey(data, "left") || data === "h") return this.stepTab(-1);
    if (data === "1" || data === "2" || data === "3") return this.selectTab((Number(data) - 1) as ContextInfoTab);
    if (matchesKey(data, "up") || data === "k") return this.scroll(-1);
    if (matchesKey(data, "down") || data === "j") return this.scroll(1);
    if (matchesKey(data, "pageUp")) return this.scroll(-Math.max(1, this.bodyRows - 1));
    if (matchesKey(data, "pageDown")) return this.scroll(Math.max(1, this.bodyRows - 1));
    if (matchesKey(data, "home")) return this.setOffset(0);
    if (matchesKey(data, "end") || data === "G") return this.setOffset(Math.max(0, this.contentRows - this.bodyRows));
    if (data === "c" && this.data.sessionId && this.options.copySessionId) void this.copySessionId();
  }

  handleMouse(event: TuiMouseEvent): boolean {
    if (event.kind === "leave") {
      const changed = this.closeHovered || this.copyHovered || this.hoveredTab !== undefined;
      this.closeHovered = false;
      this.copyHovered = false;
      this.hoveredTab = undefined;
      return changed;
    }
    const overClose = event.y === 0 && event.x >= Math.max(0, this.frameWidth - 6) && event.x < this.frameWidth - 1;
    const tabIndex = this.tabHits.findIndex((hit) => (
      event.y === hit.row && event.x >= hit.start && event.x < hit.end
    ));
    const overCopy = event.y === this.frameHeight - 2
      && !!this.copyHit
      && event.x >= this.copyHit.start
      && event.x < this.copyHit.end;
    if (event.kind === "move") {
      const changed = this.closeHovered !== overClose
        || this.copyHovered !== overCopy
        || this.hoveredTab !== (tabIndex >= 0 ? tabIndex : undefined);
      this.closeHovered = overClose;
      this.copyHovered = overCopy;
      this.hoveredTab = tabIndex >= 0 ? tabIndex : undefined;
      return changed;
    }
    if (event.release || (event.button & 3) !== 0) return false;
    if (overClose) {
      this.options.onClose();
      return true;
    }
    if (tabIndex >= 0) {
      this.selectTab(tabIndex as ContextInfoTab);
      return true;
    }
    if (overCopy && this.data.sessionId && this.options.copySessionId) {
      void this.copySessionId();
      return true;
    }
    return false;
  }

  invalidate(): void {}

  private frameLine(content: string, width: number): string {
    const theme = this.options.theme ?? darkTheme;
    return `${themeForeground(theme.border, "│")}${fit(content, width)}${themeForeground(theme.border, "│")}`;
  }

  private renderTabs(innerWidth: number): string[] {
    const theme = this.options.theme ?? darkTheme;
    const rows: Array<{ pieces: string[]; width: number }> = [{ pieces: ["  "], width: 2 }];
    this.tabHits = [];
    for (let index = 0; index < TABS.length; index++) {
      const label = TABS[index]!;
      let row = rows[rows.length - 1]!;
      let gap = row.width > 2 ? 3 : 0;
      if (row.width + gap + visibleWidth(label) > innerWidth && row.width > 2) {
        row = { pieces: ["  "], width: 2 };
        rows.push(row);
        gap = 0;
      }
      if (gap > 0) {
        row.pieces.push("   ");
        row.width += 3;
      }
      const start = 1 + row.width; // outer border plus content offset
      this.tabHits.push({ start, end: start + visibleWidth(label), row: rows.length });
      const active = index === this.activeTab;
      const hovered = index === this.hoveredTab;
      const styled = active
        ? chalk.bold(themeForeground(theme.inputText, themeBackground(theme.traceSelectedBg, label)))
        : hovered
          ? themeForeground(theme.inputText, themeBackground(theme.traceHoverBg, label))
          : themeForeground(theme.muted, label);
      row.pieces.push(styled);
      row.width += visibleWidth(label);
    }
    return rows.map((row) => fit(row.pieces.join(""), innerWidth));
  }

  private renderFooter(innerWidth: number): string {
    const theme = this.options.theme ?? darkTheme;
    let plain: string;
    if (innerWidth >= 78 && this.data.sessionId) {
      plain = "Tab switch  |  ↑/↓ scroll  |  c copy session ID  |  Esc close";
    } else if (innerWidth >= 54) {
      plain = "Tab switch  |  ↑/↓ scroll  |  Esc close";
    } else {
      plain = "↑/↓ scroll  |  Esc close";
    }
    if (this.copyStatus === "copying") plain = "Copying session ID…";
    if (this.copyStatus === "copied") plain = "Session ID copied  |  Esc close";
    if (this.copyStatus === "failed") plain = "Copy failed  |  Esc close";
    const left = Math.max(0, Math.floor((innerWidth - visibleWidth(plain)) / 2));
    const copyStart = plain.indexOf("c copy session ID");
    this.copyHit = copyStart >= 0
      ? { start: 1 + left + copyStart, end: 1 + left + copyStart + "c copy session ID".length, row: this.frameHeight - 2 }
      : undefined;
    const styled = this.copyHovered && copyStart >= 0
      ? `${plain.slice(0, copyStart)}${themeForeground(theme.inputText, themeBackground(theme.traceHoverBg, "c copy session ID"))}${plain.slice(copyStart + "c copy session ID".length)}`
      : plain;
    return fit(`${" ".repeat(left)}${themeDim(theme.dim, styled)}`, innerWidth);
  }

  private stepTab(direction: 1 | -1): void {
    const next = (this.activeTab + direction + TABS.length) % TABS.length;
    this.selectTab(next as ContextInfoTab);
  }

  private selectTab(tab: ContextInfoTab): void {
    if (tab === this.activeTab) return;
    this.activeTab = tab;
    this.offset = 0;
    this.options.onRender();
  }

  private scroll(delta: number): void {
    this.setOffset(this.offset + delta);
  }

  private setOffset(value: number): void {
    const next = Math.max(0, Math.min(Math.max(0, this.contentRows - this.bodyRows), value));
    if (next === this.offset) return;
    this.offset = next;
    this.options.onRender();
  }

  private async copySessionId(): Promise<void> {
    if (!this.options.copySessionId || this.copyStatus === "copying") return;
    this.copyStatus = "copying";
    this.options.onRender();
    try {
      await this.options.copySessionId();
      this.copyStatus = "copied";
    } catch {
      this.copyStatus = "failed";
    }
    this.options.onRender();
  }
}
