//! The screen, in NSH components: the conversation scrolling above, the
//! question or picker that is open, the composer and a footer.
//!
//! The terminal platform makes a column 8 points wide and a row 16 tall,
//! so sizes here are in those steps: [`col`] and [`row`].

use std::time::Duration;

use bubble_core::prompt::Mode;
use nsh_core::keymap::When;
use nsh_core::prelude::*;

use super::state::{COMMANDS, Item, Kind, Line, Picker, ToolState, Tui};

const ACCENT: u32 = 0x8A7FC6;
const BRAND: u32 = 0xE89A6B;
const MUTED: u32 = 0x8C8C8C;
const TOOL: u32 = 0x59BCE8;
const OK: u32 = 0x4FB06D;
const WARN: u32 = 0xD7A13B;
const ERROR: u32 = 0xE5534B;
const THINK: u32 = 0xB48EAD;

/// `n` columns, in points.
pub fn col(n: f32) -> Points {
    px(n * bubble_term::CELL_W)
}

/// `n` rows, in points.
pub fn row(n: f32) -> Points {
    px(n * bubble_term::CELL_H)
}

fn color(hex: u32) -> Rgba {
    rgb(hex)
}

/// A box drawn in box characters, its content one cell in.
fn boxed(border: u32) -> View {
    view()
        .border(px(1.0))
        .rounded(px(4.0))
        .border_color(color(border))
        .pl(col(1.0) - px(1.0))
        .pr(col(1.0) - px(1.0))
        .pt(row(1.0) - px(1.0))
        .pb(row(1.0) - px(1.0))
}

fn muted(content: impl IntoReactive<SharedString>) -> Text {
    text(content).text_color(color(MUTED))
}

/// How long ago `timestamp` (ms) was.
pub fn age(timestamp: i64) -> String {
    let minutes = (bubble_store::now_ms() - timestamp).max(0) / 60_000;
    match minutes {
        0 => "just now".into(),
        1..=59 => format!("{minutes}m ago"),
        60..=1439 => format!("{}h ago", minutes / 60),
        _ => format!("{}d ago", minutes / 1440),
    }
}

fn home_relative(path: &str) -> String {
    match dirs_home() {
        Some(home) if path.starts_with(&home) => format!("~{}", &path[home.len()..]),
        _ => path.to_owned(),
    }
}

fn dirs_home() -> Option<String> {
    std::env::var("HOME").ok().filter(|h| !h.is_empty())
}

fn tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1e6)
    } else if n >= 1000 {
        format!("{}k", n / 1000)
    } else {
        n.to_string()
    }
}

const CAT: [&str; 7] =
    [" █▄    ▄█ ", " ███▄▄███ ", "██████████", "█  ████  █", "████▀▀████", "██████████", " ▀██████▀ "];

fn welcome(tui: Tui) -> impl IntoElement {
    let info = move || tui.info.get();
    view()
        .flex_row()
        .gap(col(3.0))
        .pt(row(1.0))
        .child(view().children(CAT.iter().map(|l| text(*l).text_color(color(BRAND)).into_element()).collect::<Vec<_>>()))
        .child(
            view()
                .flex_1()
                .min_w(px(0.0))
                .justify_center()
                .child(
                    view()
                        .flex_row()
                        .gap(col(1.0))
                        .child(text("Bubble").font_weight(FontWeight::BOLD))
                        .child(muted(format!("native {}", env!("CARGO_PKG_VERSION")))),
                )
                .child(muted(move || info().map(|i| format!("{} · {}", i.model, i.thinking.as_str())).unwrap_or_else(|| "Starting…".into())))
                .child(muted(move || info().map(|i| home_relative(&i.cwd)).unwrap_or_default()))
                .child(view().h(row(1.0)))
                .child(muted("/help for commands · Tab switches Build/Plan · Esc stops")),
        )
}

/// One line of the conversation.
fn line_view(tui: Tui, line: ReadSignal<Line>) -> AnyElement {
    let id = line.with_untracked(|l| l.id);
    // A tool right after a tool sits under it; everything else has a row
    // of space above.
    let gap = move || {
        let follows_tool = tui.lines.with(|lines| {
            let at = lines.iter().position(|l| l.id == id).unwrap_or(0);
            at > 0
                && matches!(lines[at - 1].kind, Kind::Tool { .. })
                && matches!(lines[at].kind, Kind::Tool { .. })
        });
        if follows_tool { px(0.0) } else { row(1.0) }
    };
    let kind = line.with_untracked(|l| std::mem::discriminant(&l.kind));
    let body = match line.with_untracked(|l| l.kind.clone()) {
        Kind::User => view()
            .flex_row()
            .child(text("› ").text_color(color(ACCENT)).font_weight(FontWeight::BOLD))
            .child(text(move || line.with(|l| l.text.clone())).flex_1().min_w(px(0.0)))
            .into_element(),
        Kind::Assistant => view()
            .flex_row()
            .child(text("● ").text_color(color(MUTED)))
            .child(view().flex_1().min_w(px(0.0)).child(dynamic(move || markdown(line.with(|l| l.text.clone())))))
            .into_element(),
        Kind::Reasoning => view()
            .child(
                text(move || {
                    if line.with(|l| l.live) {
                        "✻ Thinking…"
                    } else if tui.expanded.get() {
                        "✻ Thought"
                    } else {
                        "✻ Thought · ctrl+o to show"
                    }
                })
                .text_color(color(THINK))
                .italic(),
            )
            .child(when(
                move || tui.expanded.get() || line.with(|l| l.live),
                move || {
                    view()
                        .pl(col(2.0))
                        .child(text(move || last_lines(&line.with(|l| l.text.clone()), if tui.expanded.get() { usize::MAX } else { 6 })).text_color(color(MUTED)).italic())
                },
            ))
            .into_element(),
        Kind::Tool { .. } => tool_view(tui, line),
        Kind::Notice => muted(move || line.with(|l| l.text.clone())).into_element(),
        Kind::Error => view()
            .flex_row()
            .child(text("✗ ").text_color(color(ERROR)))
            .child(text(move || line.with(|l| l.text.clone())).text_color(color(ERROR)).flex_1().min_w(px(0.0)))
            .into_element(),
    };
    let _ = kind;
    view().pt(gap).child(body).into_element()
}

/// The last `count` lines of `text`.
fn last_lines(text: &str, count: usize) -> String {
    let lines: Vec<&str> = text.trim_end().lines().collect();
    let skip = lines.len().saturating_sub(count);
    lines[skip..].join("\n")
}

fn tool_view(tui: Tui, line: ReadSignal<Line>) -> AnyElement {
    let state = move || match line.with(|l| l.kind.clone()) {
        Kind::Tool { state, .. } => state,
        _ => ToolState::Done,
    };
    let output = move || match line.with(|l| l.kind.clone()) {
        Kind::Tool { output, .. } => output,
        _ => String::new(),
    };
    view()
        .child(
            view()
                .flex_row()
                .child(
                    text(move || "● ")
                        .text_color(move || {
                            color(match state() {
                                ToolState::Running => WARN,
                                ToolState::Done => OK,
                                ToolState::Failed | ToolState::Denied => ERROR,
                                ToolState::Stopped => MUTED,
                            })
                        }),
                )
                .child(text(move || line.with(|l| l.text.clone())).text_color(color(TOOL)).flex_1().min_w(px(0.0)).number_of_lines(
                    move || if tui.expanded.get() { 0u32 } else { 2u32 },
                )),
        )
        .child(when(
            move || !output().trim().is_empty() || matches!(state(), ToolState::Denied | ToolState::Stopped),
            move || {
                view().flex_row().pl(col(2.0)).child(muted("⎿ ")).child(
                    muted(move || {
                        let text = output();
                        match state() {
                            ToolState::Denied if text.trim().is_empty() => return "Denied".to_owned(),
                            ToolState::Stopped if text.trim().is_empty() => return "Stopped".to_owned(),
                            _ => {}
                        }
                        let all: Vec<&str> = text.trim_end().lines().collect();
                        let limit = if tui.expanded.get() { usize::MAX } else { 3 };
                        let mut shown = all.iter().take(limit).map(|l| truncate(l, 400)).collect::<Vec<_>>().join("\n");
                        if all.len() > limit {
                            shown.push_str(&format!("\n… +{} lines (ctrl+o to expand)", all.len() - limit));
                        }
                        shown
                    })
                    .text_color(move || color(if matches!(state(), ToolState::Failed | ToolState::Denied) { ERROR } else { MUTED }))
                    .flex_1()
                    .min_w(px(0.0)),
                )
            },
        ))
        .into_element()
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() > max { format!("{}…", text.chars().take(max).collect::<String>()) } else { text.to_owned() }
}

/// Markdown, drawn line by line: headings bold, code in its own color,
/// lists and quotes indented as written.
fn markdown(source: String) -> AnyElement {
    let mut blocks = Vec::new();
    let mut fence = false;
    for raw in source.trim_end().lines() {
        let trimmed = raw.trim_start();
        if trimmed.starts_with("```") {
            fence = !fence;
            continue;
        }
        let element = if fence {
            text(if raw.is_empty() { " ".to_owned() } else { raw.to_owned() }).text_color(color(TOOL)).into_element()
        } else if let Some(heading) = trimmed.strip_prefix('#') {
            text(heading.trim_start_matches('#').trim().to_owned()).font_weight(FontWeight::BOLD).into_element()
        } else if let Some(quote) = trimmed.strip_prefix('>') {
            view()
                .flex_row()
                .child(muted("│ "))
                .child(inline(quote.trim()).text_color(color(MUTED)).flex_1().min_w(px(0.0)))
                .into_element()
        } else if let Some((marker, rest)) = list_item(trimmed) {
            let indent = (raw.len() - trimmed.len()) as f32;
            view()
                .flex_row()
                .pl(col(indent))
                .child(text(marker))
                .child(inline(rest).flex_1().min_w(px(0.0)))
                .into_element()
        } else {
            inline(raw).into_element()
        };
        blocks.push(element);
    }
    view().children(blocks).into_element()
}

/// A list item's marker (`• ` or `1. `) and its text.
fn list_item(line: &str) -> Option<(String, &str)> {
    for bullet in ["- ", "* ", "+ "] {
        if let Some(rest) = line.strip_prefix(bullet) {
            return Some(("• ".into(), rest));
        }
    }
    let digits = line.chars().take_while(char::is_ascii_digit).count();
    if digits > 0 && line[digits..].starts_with(". ") {
        return Some((line[..digits + 2].to_owned(), &line[digits + 2..]));
    }
    None
}

/// A line with `code` and **bold** spans.
fn inline(line: &str) -> Text {
    let mut spans = Vec::new();
    let mut rest = line;
    while !rest.is_empty() {
        let code = rest.find('`');
        let bold = rest.find("**");
        let next = match (code, bold) {
            (Some(c), Some(b)) => Some(c.min(b)),
            (c, b) => c.or(b),
        };
        let Some(at) = next else {
            spans.push(text(rest.to_owned()).into_element());
            break;
        };
        if at > 0 {
            spans.push(text(rest[..at].to_owned()).into_element());
        }
        let is_code = rest[at..].starts_with('`');
        let marker = if is_code { "`" } else { "**" };
        let inner = &rest[at + marker.len()..];
        match inner.find(marker) {
            Some(end) => {
                let content = inner[..end].to_owned();
                spans.push(if is_code {
                    text(content).text_color(color(ACCENT)).into_element()
                } else {
                    text(content).font_weight(FontWeight::BOLD).into_element()
                });
                rest = &inner[end + marker.len()..];
            }
            None => {
                spans.push(text(rest[at..].to_owned()).into_element());
                break;
            }
        }
    }
    if spans.is_empty() {
        return text(" ");
    }
    text("").children(spans)
}

fn transcript(tui: Tui) -> impl IntoElement {
    let list = element_ref();
    tui.on_change(move || list.scroll_to_end(false));
    // The newest lines stay in view as the window or the panels under the
    // conversation change size.
    effect(move || {
        let _ = window();
        tui.picker.with(|_| ());
        tui.ask.with(|_| ());
        let _ = tui.turns.get();
        let _ = tui.draft.with(|d| d.starts_with('/'));
        list.scroll_to_end(false);
    });
    scroll().flex_1().element_ref(&list).child(
        view()
            .pb(row(1.0))
            .pr(col(1.0))
            .child(welcome(tui))
            .child(each(move || tui.lines.get(), |line: &Line| line.id, move |line, _| line_view(tui, line))),
    )
}

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

fn status(tui: Tui) -> impl IntoElement {
    let tick = signal(0usize);
    set_interval(Duration::from_millis(100), move || tick.update(|t| *t += 1));
    view().flex_row().pt(row(1.0)).child(
        text(move || {
            let t = tick.get();
            let seconds = (bubble_store::now_ms() - tui.started.get()).max(0) / 1000;
            let waiting = if tui.ask.with(Option::is_some) { "Waiting for you" } else { "Working" };
            let queued = tui.turns.get().saturating_sub(1);
            let queued = if queued > 0 { format!(" · {queued} queued") } else { String::new() };
            format!("{} {waiting}… {seconds}s{queued} · esc to stop", SPINNER[t % SPINNER.len()])
        })
        .text_color(color(ACCENT)),
    )
}

fn approval(tui: Tui) -> impl IntoElement {
    let rememberable = move || tui.ask.with(|a| a.as_ref().is_some_and(|a| a.rememberable));
    for (key, choice) in [("y", 'y'), ("a", 'a'), ("n", 'n')] {
        on_key_with(key, KeyOptions { when: When::Always, repeat: false }, move |_| tui.answer(choice));
    }
    let choice = move |label: &'static str, key: char| {
        view().on_press(move || tui.answer(key)).child(text(label).font_weight(FontWeight::BOLD))
    };
    boxed(WARN)
        .mt(row(1.0))
        .child(text(move || tui.ask.with(|a| a.as_ref().map(|a| a.summary.clone()).unwrap_or_default())).font_weight(FontWeight::BOLD))
        .child(
            text(move || tui.ask.with(|a| a.as_ref().map(|a| truncate(&a.detail, 2000)).unwrap_or_default()))
                .text_color(color(TOOL)),
        )
        .child(
            view()
                .flex_row()
                .flex_wrap()
                .gap(col(3.0))
                .pt(row(1.0))
                .child(choice("[y] Yes", 'y'))
                .child(when(rememberable, move || choice("[a] Yes, for this session", 'a')))
                .child(choice("[n] No", 'n')),
        )
}

fn picker_view(tui: Tui) -> impl IntoElement {
    let bind = |key: &str, f: Box<dyn Fn()>| on_key_with(key, KeyOptions { when: When::Always, repeat: true }, move |_| f());
    bind("up", Box::new(move || tui.move_selection(-1)));
    bind("down", Box::new(move || tui.move_selection(1)));
    bind("enter", Box::new(move || tui.choose()));
    bind("escape", Box::new(move || tui.picker.set(None)));
    let picker = move || tui.picker.get();
    boxed(ACCENT)
        .mt(row(1.0))
        .child(text(move || picker().map(|p| p.title).unwrap_or_default()).font_weight(FontWeight::BOLD))
        .child(dynamic(move || {
            let Some(Picker { items, selected, .. }) = picker() else { return view().into_element() };
            let window = 10usize;
            let first = selected.saturating_sub(window / 2).min(items.len().saturating_sub(window));
            view()
                .children(
                    items
                        .into_iter()
                        .enumerate()
                        .skip(first)
                        .take(window)
                        .map(|(i, item)| picker_row(tui, i, i == selected, item))
                        .collect::<Vec<_>>(),
                )
                .into_element()
        }))
        .child(muted("↑↓ choose · Enter select · Esc close"))
}

fn picker_row(tui: Tui, index: usize, selected: bool, item: Item) -> AnyElement {
    let mark = if selected { "› " } else { "  " };
    let label = text(format!("{mark}{}", item.label)).number_of_lines(1).flex_1().min_w(px(0.0));
    let label = if selected { label.text_color(color(ACCENT)).font_weight(FontWeight::BOLD) } else { label };
    view()
        .flex_row()
        .gap(col(2.0))
        .on_press(move || {
            tui.picker.update(|p| {
                if let Some(p) = p {
                    p.selected = index;
                }
            });
            tui.choose();
        })
        .child(label)
        .child(muted(item.detail))
        .into_element()
}

/// The commands the draft starts, while it is one word starting with `/`.
fn suggestions(draft: &str) -> Vec<(&'static str, &'static str)> {
    if !draft.starts_with('/') || draft.contains(char::is_whitespace) {
        return Vec::new();
    }
    COMMANDS.iter().copied().filter(|(c, _)| c.starts_with(draft) && *c != draft).collect()
}

fn suggestions_view(tui: Tui, at: Signal<usize>) -> impl IntoElement {
    let list = move || tui.draft.with(|d| suggestions(d));
    on_key_with("up", KeyOptions { when: When::Always, repeat: true }, move |_| {
        let count = list().len().max(1);
        at.update(|a| *a = (*a + count - 1) % count);
    });
    on_key_with("down", KeyOptions { when: When::Always, repeat: true }, move |_| {
        let count = list().len().max(1);
        at.update(|a| *a = (*a + 1) % count);
    });
    view().pt(row(1.0)).child(dynamic(move || {
        let selected = at.get();
        view()
            .children(
                list()
                    .into_iter()
                    .enumerate()
                    .map(|(i, (command, about))| {
                        let name = text(format!("{command:<10}"));
                        let name = if i == selected { name.text_color(color(ACCENT)).font_weight(FontWeight::BOLD) } else { name };
                        view()
                            .flex_row()
                            .on_press(move || tui.draft.set(format!("{command} ")))
                            .child(name)
                            .child(muted(about).number_of_lines(1).flex_1().min_w(px(0.0)))
                            .into_element()
                    })
                    .collect::<Vec<_>>(),
            )
            .into_element()
    }))
}

fn composer(tui: Tui, at: Signal<usize>) -> impl IntoElement {
    let border = move || {
        color(match tui.info.with(|i| i.as_ref().map(|i| i.mode)) {
            Some(Mode::Plan) => TOOL,
            Some(Mode::Bypass) => ERROR,
            _ => ACCENT,
        })
    };
    let submit = move || {
        let draft = tui.draft.get_untracked();
        let list = suggestions(&draft);
        let text = match list.get(at.get_untracked()) {
            Some((command, _)) => command.to_string(),
            None => draft,
        };
        tui.draft.set(String::new());
        at.set(0);
        tui.submit(text);
    };
    view()
        .mt(row(1.0))
        .border(px(1.0))
        .rounded(px(4.0))
        .border_color(border)
        .flex_row()
        .pl(col(1.0) - px(1.0))
        .pr(col(1.0) - px(1.0))
        .pt(row(1.0) - px(1.0))
        .pb(row(1.0) - px(1.0))
        .child(text("› ").text_color(border).font_weight(FontWeight::BOLD))
        .child(
            nsh_base::input()
                .value(tui.draft)
                .placeholder(move || {
                    if tui.busy() { "Queue a follow-up… (Esc stops)".to_owned() } else { "Ask Bubble, or / for commands".to_owned() }
                })
                .placeholder_color(color(MUTED))
                .multiline(true)
                .submit_behavior(SubmitBehavior::Submit)
                .auto_focus(true)
                .flex_1()
                .min_w(px(0.0))
                .min_h(row(1.0))
                .max_h(row(10.0))
                .on_change(move |value: &str| {
                    tui.draft.set(value.to_owned());
                    at.set(0);
                })
                .on_submit(move |_: &str| submit()),
        )
}

fn footer(tui: Tui) -> impl IntoElement {
    view().flex_row().gap(col(2.0)).pl(col(1.0)).child(
        text(move || {
            let Some(info) = tui.info.get() else { return String::new() };
            let mode = match info.mode {
                Mode::Build => "build",
                Mode::Plan => "⏸ plan",
                Mode::Bypass => "⚠ bypass",
            };
            let mut parts = vec![
                mode.to_owned(),
                format!("{} · {}", info.model, info.thinking.as_str()),
                home_relative(&info.cwd),
                info.title.clone(),
            ];
            let used = tui.context.get();
            if used > 0 {
                parts.push(match info.context_window {
                    Some(window) if window > 0 => format!("{}% of {}", used * 100 / window, tokens(window)),
                    _ => format!("{} tokens", tokens(used)),
                });
            }
            parts.join("  ")
        })
        .text_color(color(MUTED))
        .number_of_lines(1)
        .flex_1()
        .min_w(px(0.0)),
    )
}

/// The whole screen.
pub fn screen(tui: Tui) -> impl IntoElement {
    let at = signal(0usize);
    on_key_with("escape", KeyOptions { when: When::Always, repeat: false }, move |_| {
        if tui.picker.with_untracked(Option::is_some) {
            tui.picker.set(None);
        } else {
            tui.stop();
        }
    });
    on_key_with("ctrl+c", KeyOptions { when: When::Always, repeat: false }, move |_| {
        if tui.picker.with_untracked(Option::is_some) {
            tui.picker.set(None);
        } else if tui.busy() {
            tui.stop();
        } else if !tui.draft.with_untracked(String::is_empty) {
            tui.draft.set(String::new());
        } else {
            bubble_term::exit();
        }
    });
    on_key_with("ctrl+d", KeyOptions { when: When::Always, repeat: false }, move |_| {
        if tui.draft.with_untracked(String::is_empty) {
            bubble_term::exit();
        }
    });
    on_key_with("tab", KeyOptions { when: When::Always, repeat: false }, move |_| {
        let list = tui.draft.with_untracked(|d| suggestions(d));
        match list.get(at.get_untracked()) {
            Some((command, _)) => tui.draft.set(format!("{command} ")),
            None => tui.toggle_plan(),
        }
    });
    on_key_with("ctrl+o", KeyOptions { when: When::Always, repeat: false }, move |_| tui.expanded.update(|e| *e = !*e));
    view()
        .flex_1()
        .px(col(1.0))
        .child(transcript(tui))
        .child(when(move || tui.busy(), move || status(tui)))
        .child(when(move || tui.ask.with(Option::is_some), move || approval(tui)))
        .child(when(move || tui.picker.with(Option::is_some), move || picker_view(tui)))
        .child(when(
            move || tui.picker.with(Option::is_none) && tui.draft.with(|d| !suggestions(d).is_empty()),
            move || suggestions_view(tui, at),
        ))
        .child(composer(tui, at))
        .child(footer(tui))
}
