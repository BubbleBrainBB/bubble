//! Pieces every screen uses: icons, buttons, the conversation and its
//! composer, styled as the Electron Bubble app's.

use std::rc::Rc;

use bubble_core::Decision;
use bubble_store::{MessageRole, TaskState};
use nsh_base::text_view::TextViewClasses;
use nsh_core::element::ImageSrc;
use nsh_core::prelude::*;

use crate::palette::*;
use crate::state::{AppState, Conversation, Line, LineKind, ToolStatus, TurnStatus};
use crate::trace::Block;

/// An icon from SVG markup drawn in `currentColor`; set its color with
/// `.text_color(..)`.
pub fn icon(markup: &'static str, size: f32) -> Svg {
    svg(ImageSrc::svg_markup(markup)).size(px(size)).flex_shrink_0()
}

/// The icon of a task's state.
pub fn state_icon(state: impl Fn() -> TaskState + 'static, size: f32) -> Svg {
    svg(move || ImageSrc::svg_markup(state_markup(state())))
        .w(px(size))
        .h(px(size))
        .flex_shrink_0()
}

fn state_markup(state: TaskState) -> &'static str {
    match state {
        TaskState::NeedsYou => r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><circle cx="8" cy="8" r="7" fill="#E5532D"/><path d="M6.1 5.2v5.6M9.9 5.2v5.6" stroke="#FFFFFF" stroke-width="1.75" stroke-linecap="round"/></svg>"##,
        TaskState::Running => r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><circle cx="8" cy="8" r="6.25" fill="none" stroke="#4F46E5" stroke-width="1.5"/><path d="M8 8V4a4 4 0 0 1 0 8z" fill="#4F46E5"/></svg>"##,
        TaskState::Failed => r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><circle cx="8" cy="8" r="7" fill="#DC2626"/><path d="M5.75 5.75l4.5 4.5m0-4.5l-4.5 4.5" stroke="#FFFFFF" stroke-width="1.5" stroke-linecap="round"/></svg>"##,
        TaskState::Queued => r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><circle cx="8" cy="8" r="6.25" fill="none" stroke="#8F8F8F" stroke-width="1.5" stroke-dasharray="2.2 2.2"/></svg>"##,
        TaskState::NotStarted | TaskState::Waiting => r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><circle cx="8" cy="8" r="6.25" fill="none" stroke="#8F8F8F" stroke-width="1.5"/></svg>"##,
        TaskState::Review => r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><circle cx="8" cy="8" r="7" fill="#4F46E5"/><path d="M5 8.2l2.1 2.1L11 6.1" stroke="#FFFFFF" stroke-width="1.5" fill="none" stroke-linecap="round" stroke-linejoin="round"/></svg>"##,
        TaskState::Done => r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><circle cx="8" cy="8" r="7" fill="#A3A3A3"/><path d="M5 8.2l2.1 2.1L11 6.1" stroke="#FFFFFF" stroke-width="1.5" fill="none" stroke-linecap="round" stroke-linejoin="round"/></svg>"##,
        TaskState::Canceled => r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><circle cx="8" cy="8" r="6.25" fill="none" stroke="#8F8F8F" stroke-width="1.5"/><path d="M5.5 10.5l5-5" stroke="#8F8F8F" stroke-width="1.5" stroke-linecap="round"/></svg>"##,
    }
}

pub fn state_label(state: TaskState) -> &'static str {
    match state {
        TaskState::NeedsYou => "Needs you",
        TaskState::Running => "Running",
        TaskState::Queued => "Queued",
        TaskState::Failed => "Failed",
        TaskState::Review => "Ready for review",
        TaskState::NotStarted => "To start",
        TaskState::Waiting => "Waiting",
        TaskState::Done => "Done",
        TaskState::Canceled => "Canceled",
    }
}

/// The last folder of a project's path.
pub fn project_name(cwd: &str) -> String {
    cwd.trim_end_matches('/').rsplit('/').next().unwrap_or(cwd).to_owned()
}

pub fn age(timestamp: i64) -> String {
    let minutes = (bubble_store::now_ms() - timestamp).max(0) / 60_000;
    match minutes {
        0 => "now".into(),
        1..=59 => format!("{minutes}m"),
        60..=1439 => format!("{}h", minutes / 60),
        _ => format!("{}d", minutes / 1440),
    }
}

/// A quiet outlined button.
pub fn button(label: &str, on_press: impl Fn() + 'static) -> View {
    view()
        .h(px(28.0))
        .px(px(12.0))
        .justify_center()
        .rounded(px(7.0))
        .border(px(LINE))
        .border_color(border())
        .bg(pane())
        .window_drag(false)
        .cursor(Cursor::Pointer)
        .hover(|s| s.bg(tertiary()))
        .on_press(on_press)
        .child(text(label.to_owned()).text_size(px(BODY)).text_color(ink()))
}

/// The one strong action on a screen: black, as the send button.
pub fn primary_button(label: &str, on_press: impl Fn() + 'static) -> View {
    view()
        .h(px(28.0))
        .px(px(12.0))
        .justify_center()
        .rounded(px(7.0))
        .bg(ink())
        .window_drag(false)
        .cursor(Cursor::Pointer)
        .active(|s| s.opacity(0.8))
        .on_press(on_press)
        .child(text(label.to_owned()).text_size(px(BODY)).font_weight(FontWeight::MEDIUM).text_color(paper()))
}

/// Markdown in the conversation's type.
fn markdown(source: String) -> AnyElement {
    nsh_base::text_view::text_view()
        .markdown(SharedString::from(source))
        .selectable(true)
        .classes(TextViewClasses {
            root: Some("text-[13px] leading-[22px] text-[#1A1A1A] dark:text-[#ECECEC]".into()),
            heading: Some("text-[15px] font-semibold".into()),
            ..Default::default()
        })
        .into_element()
}

/// The conversation, centered, newest at the bottom and kept in view.
pub fn conversation_view(
    app: AppState,
    conversation: Conversation,
    empty_title: &'static str,
    empty_hint: &'static str,
) -> impl IntoElement {
    let list = element_ref();
    let following = conversation.following;
    following.set(true);
    let content_height = signal(0.0_f32);
    conversation.on_change(move || {
        if following.get_untracked() { list.scroll_to_end(false); }
    });
    let empty = move || conversation.lines.with(|lines| lines.is_empty());
    // Turns whose folded work the person opened.
    let opened = conversation.opened;
    let project = conversation
        .task_id
        .get()
        .and_then(|id| app.with_store(|s| s.get_task(&id)).flatten())
        .and_then(|t| t.project_cwd);
    view().flex_1().min_h(px(0.0)).child(scroll().flex_1().test_id("chat-transcript").element_ref(&list)
        .on_scroll(move |event: &nsh_core::element::ScrollEvent| {
            let height = event.content_size.height.0;
            // Layout-driven size changes do not represent reader intent.
            if (height - content_height.get_untracked()).abs() < 0.5 {
                following.set(crate::trace::near_bottom(event.offset.y.0, event.size.height.0, height));
            }
            content_height.set(height);
        }).child(
        view()
            .w_full()
            .max_w(px(READING_WIDTH))
            .mx(auto())
            .px(px(24.0))
            .pt(px(28.0))
            .pb(px(16.0))
            .on_layout(move |_: &nsh_core::geometry::Bounds<Points>| {
                if following.get_untracked() { list.scroll_to_end(false); }
            })
            .child(when(empty, move || {
                view()
                    .pt(px(120.0))
                    .items_center()
                    .gap(px(8.0))
                    .child(text(empty_title).text_size(px(HEADING)).font_weight(FontWeight::SEMIBOLD))
                    .child(text(empty_hint).text_size(px(BODY)).text_color(muted()))
            }))
            .child(each(
                move || conversation.lines.with(|lines| crate::trace::layout(lines, conversation.busy.get())),
                |block: &Block| block.id().to_owned(),
                move |block, _| block_view(app, conversation, block, opened, project.clone()),
            ))
            .child(subagents_view(conversation, None)),
    )).child(when(move || !following.get(), move || {
        button("Jump to latest", move || {
            following.set(true);
            list.scroll_to_end(true);
        }).self_center().mb(px(8.0))
    }))
}

/// When something was said, as a messenger writes it over a message:
/// "Today 8:51 AM", "Yesterday 3:02 PM", "Nov 9, 3:02 PM".
pub fn stamp(at: i64, now: chrono::DateTime<chrono::Local>) -> String {
    use chrono::TimeZone;
    let Some(time) = chrono::Local.timestamp_millis_opt(at).single() else { return String::new() };
    let clock = time.format("%-I:%M %p");
    let days = (now.date_naive() - time.date_naive()).num_days();
    match days {
        0 => format!("Today {clock}"),
        1 => format!("Yesterday {clock}"),
        _ if time.format("%Y").to_string() == now.format("%Y").to_string() => format!("{} {clock}", time.format("%b %-d,")),
        _ => format!("{} {clock}", time.format("%b %-d, %Y,")),
    }
}

/// The time over a message that opens a turn, in the middle of the column.
fn stamp_line(line: ReadSignal<Line>) -> impl IntoElement {
    when(move || line.get().at > 0, move || {
        text(move || stamp(line.get().at, chrono::Local::now())).self_center().pb(px(8.0)).text_size(px(SMALL)).text_color(muted())
    })
}

/// One block of the transcript, hidden while the turn it belongs to is
/// folded.
fn block_view(
    app: AppState,
    conversation: Conversation,
    block: ReadSignal<Block>,
    opened: Signal<std::collections::HashSet<String>>,
    project: Option<String>,
) -> AnyElement {
    let shown = move || block.get().folded_by().is_none_or(|turn| opened.with(|o| o.contains(turn)));
    let content = match block.get_untracked() {
        Block::Worked { id, .. } => worked_divider(block, id, opened).into_element(),
        Block::Turn { id, .. } => turn_header(block, conversation, id).into_element(),
        Block::Status { .. } => status_line(block, conversation, project).into_element(),
        Block::Group { .. } => when(shown, move || group_view(block, conversation, project.clone())).into_element(),
        Block::Line { id, .. } => when(shown, move || {
            let id = id.clone();
            let project = project.clone();
            // The line itself, kept live as it streams or finishes.
            each(
                move || conversation.lines.with(|lines| lines.iter().filter(|l| l.id == id).cloned().collect::<Vec<_>>()),
                |line: &Line| line.id.clone(),
                move |line, _| line_view(app, conversation, line, project.clone()),
            )
        })
        .into_element(),
    };
    // A keyed transcript item must own a real native sibling. Returning a
    // bare If/Each region lets late descendant updates attach after the
    // next row (notably a legacy reply followed by a new user prompt).
    // Keep the sibling anchored, including while its work is folded.
    // Steps of the trace sit close together; messages and headers apart.
    let step = match block.get_untracked() {
        Block::Group { .. } | Block::Status { .. } => true,
        Block::Line { id, .. } => conversation.lines.with_untracked(|lines| {
            lines.iter().find(|l| l.id == id).is_some_and(|l| match &l.kind {
                LineKind::Reasoning => true,
                LineKind::Tool { name, .. } => !matches!(name.as_str(), "create_task" | "continue_task" | "spawn_agent" | "run_workflow"),
                _ => false,
            })
        }),
        _ => false,
    };
    view()
        .w_full()
        .pt(px(if step { 8.0 } else { 18.0 }))
        .test_id(&format!("trace-row:{}", block.get_untracked().id()))
        .style_when(move || !shown(), |style| style.hidden())
        .child(content)
        .into_element()
}

macro_rules! tabler {
    ($($path:literal),*) => {
        concat!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round">"#,
            $($path,)*
            "</svg>"
        )
    };
}

// The trace's icons, from Tabler as the Aegis trace's are.
const GLYPH_EXPLORE: &str = tabler!(r#"<path d="M11 19h-6a2 2 0 0 1 -2 -2v-11a2 2 0 0 1 2 -2h4l3 3h7a2 2 0 0 1 2 2v2.5"/>"#, r#"<path d="M15 18a3 3 0 1 0 6 0a3 3 0 1 0 -6 0"/>"#, r#"<path d="M20.2 20.2l1.8 1.8"/>"#);
const GLYPH_COMMAND: &str = tabler!(r#"<path d="M8 9l3 3l-3 3"/>"#, r#"<path d="M13 15h3"/>"#, r#"<path d="M3 6a2 2 0 0 1 2 -2h14a2 2 0 0 1 2 2v12a2 2 0 0 1 -2 2h-14a2 2 0 0 1 -2 -2z"/>"#);
const GLYPH_EDIT: &str = tabler!(r#"<path d="M14 3v4a1 1 0 0 0 1 1h4"/>"#, r#"<path d="M17 21h-10a2 2 0 0 1 -2 -2v-14a2 2 0 0 1 2 -2h7l5 5v11a2 2 0 0 1 -2 2z"/>"#, r#"<path d="M12 10v4"/>"#, r#"<path d="M10 12h4"/>"#, r#"<path d="M10 17h4"/>"#);
const GLYPH_WEB: &str = tabler!(r#"<path d="M3 12a9 9 0 1 0 18 0a9 9 0 0 0 -18 0"/>"#, r#"<path d="M3.6 9h16.8"/>"#, r#"<path d="M3.6 15h16.8"/>"#, r#"<path d="M11.5 3a17 17 0 0 0 0 18"/>"#, r#"<path d="M12.5 3a17 17 0 0 1 0 18"/>"#);
const GLYPH_TOOL: &str = tabler!(r#"<path d="M9.785 6l8.215 8.215l-2.054 2.054a5.81 5.81 0 1 1 -8.215 -8.215l2.054 -2.054z"/>"#, r#"<path d="M4 20l3.5 -3.5"/>"#, r#"<path d="M15 4l-3.5 3.5"/>"#, r#"<path d="M20 9l-3.5 3.5"/>"#);
const GLYPH_REASONING: &str = tabler!(r#"<path d="M3 12h1m8 -9v1m8 8h1m-15.4 -6.4l.7 .7m12.1 -.7l-.7 .7"/>"#, r#"<path d="M9 16a5 5 0 1 1 6 0a3.5 3.5 0 0 0 -1 3a2 2 0 0 1 -4 0a3.5 3.5 0 0 0 -1 -3"/>"#, r#"<path d="M9.7 17h4.6"/>"#);
const GLYPH_APPROVAL: &str = tabler!(r#"<path d="M12 3a12 12 0 0 0 8.5 3a12 12 0 0 1 -8.5 15a12 12 0 0 1 -8.5 -15a12 12 0 0 0 8.5 -3"/>"#, r#"<path d="M12 8v4"/>"#, r#"<path d="M12 16h.01"/>"#);
const GLYPH_CHEVRON: &str = tabler!(r#"<path d="M9 6l6 6l-6 6"/>"#);
const GLYPH_SPINNER: &str = tabler!(r#"<path d="M12 3a9 9 0 1 0 9 9"/>"#);
const GLYPH_INTERRUPTED: &str = tabler!(r#"<path d="M8.56 3.69a9 9 0 0 0 -2.92 1.95"/>"#, r#"<path d="M3.69 8.56a9 9 0 0 0 -.69 3.44"/>"#, r#"<path d="M3.69 15.44a9 9 0 0 0 1.95 2.92"/>"#, r#"<path d="M8.56 20.31a9 9 0 0 0 3.44 .69"/>"#, r#"<path d="M15.44 20.31a9 9 0 0 0 2.92 -1.95"/>"#, r#"<path d="M20.31 15.44a9 9 0 0 0 .69 -3.44"/>"#, r#"<path d="M20.31 8.56a9 9 0 0 0 -1.95 -2.92"/>"#, r#"<path d="M15.44 3.69a9 9 0 0 0 -3.44 -.69"/>"#);

fn kind_glyph(kind: crate::trace::StageKind) -> &'static str {
    use crate::trace::{Kind, StageKind};
    match kind {
        StageKind::Reasoning => GLYPH_REASONING,
        StageKind::Tool(Kind::Explore) => GLYPH_EXPLORE,
        StageKind::Tool(Kind::Command) => GLYPH_COMMAND,
        StageKind::Tool(Kind::Edit) => GLYPH_EDIT,
        StageKind::Tool(Kind::Web) => GLYPH_WEB,
        StageKind::Tool(Kind::Other) => GLYPH_TOOL,
    }
}

fn tool_glyph(line: &Line) -> &'static str {
    match &line.kind {
        LineKind::Tool { name, .. } => kind_glyph(crate::trace::StageKind::Tool(crate::trace::kind_of(name, &line.text))),
        _ => GLYPH_TOOL,
    }
}

/// How a trace row reads: `Active` lifts its title while the step runs,
/// `Waiting` turns it amber, `Error` turns its icon red.
#[derive(Clone, Copy, PartialEq)]
enum Tone {
    Plain,
    Active,
    Waiting,
    Error,
}

/// The one glyph at a row's end.
#[derive(Clone, Copy, PartialEq)]
enum Trail {
    Nothing,
    Spinner,
    Chevron(bool),
    Interrupted,
}

fn toggle(opened: Signal<std::collections::HashSet<String>>, key: String) {
    opened.update(|o| {
        if !o.remove(&key) {
            o.insert(key);
        }
    });
}

/// A spinning arc, while a step or a turn runs.
fn spinner(size: f32) -> impl IntoElement {
    let angle = animated_value(0.0);
    let _ = Animation::timing(&angle, 360.0, Timing::new(900.0, Easing::Linear)).looped(None).start();
    icon(GLYPH_SPINNER, size).text_color(muted()).bind_rotate(&angle)
}

/// A chevron pointing right, down once its disclosure is open.
fn chevron(size: f32, open: impl Fn() -> bool + 'static) -> impl IntoElement {
    icon(GLYPH_CHEVRON, size).text_color(faint()).rotate(move || if open() { deg(90.0) } else { deg(0.0) })
}

/// A label's opacity while its step runs: a slow breath now and then, in
/// place of the Aegis trace's shimmer sweep.
fn breath() -> AnimatedValue {
    let value = animated_value(1.0);
    let _ = Animation::sequence([
        Animation::timing(&value, 0.45, Timing::new(700.0, Easing::EaseInOut)),
        Animation::timing(&value, 1.0, Timing::new(700.0, Easing::EaseInOut)),
        Animation::delay(1600.0),
    ])
    .looped(None)
    .start();
    value
}

/// A step's title on one line; breathing while `active`.
fn step_title(title: Rc<dyn Fn() -> String>, color: impl Fn() -> AdaptiveColor + 'static, active: impl Fn() -> bool + 'static) -> View {
    let active = Rc::new(active);
    let still = active.clone();
    let (moving_title, still_title) = (title.clone(), title);
    let color = Rc::new(color);
    let still_color = color.clone();
    let line = |title: Rc<dyn Fn() -> String>, color: Rc<dyn Fn() -> AdaptiveColor>| {
        text(move || title())
            .min_w(px(0.0))
            .number_of_lines(1)
            .text_size(px(SMALL))
            .line_height(px(20.0))
            .text_color(move || color())
    };
    view()
        .flex_shrink(1.0)
        .min_w(px(0.0))
        .child(when(move || active(), move || line(moving_title.clone(), color.clone()).bind_opacity(&breath())))
        .child(when(move || !still(), move || line(still_title.clone(), still_color.clone())))
}

/// "+3 −1": what a change adds and removes.
fn diff_stat_view(stat: impl Fn() -> Option<(usize, usize)> + 'static) -> impl IntoElement {
    dynamic(move || {
        stat().filter(|(a, r)| a + r > 0).map(|(a, r)| {
            view()
                .flex_row()
                .flex_shrink_0()
                .gap(px(4.0))
                .child(text(format!("+{a}")).text_size(px(SMALL)).text_color(added()))
                .child(text(format!("\u{2212}{r}")).text_size(px(SMALL)).text_color(removed()))
        })
    })
}

/// One step of an agent's trace: kind icon, title, small facts, trailing
/// glyph. Every title shares one left edge.
fn trace_row(
    glyph: impl Fn() -> &'static str + 'static,
    title: impl Fn() -> String + 'static,
    meta: impl Fn() -> Option<(String, bool)> + 'static,
    tone: impl Fn() -> Tone + 'static,
    trail: impl Fn() -> Trail + 'static,
) -> View {
    trace_row_with(glyph, title, meta, || None, tone, trail)
}

/// A trace row with a change's line counts among its facts.
fn trace_row_with(
    glyph: impl Fn() -> &'static str + 'static,
    title: impl Fn() -> String + 'static,
    meta: impl Fn() -> Option<(String, bool)> + 'static,
    stat: impl Fn() -> Option<(usize, usize)> + 'static,
    tone: impl Fn() -> Tone + 'static,
    trail: impl Fn() -> Trail + 'static,
) -> View {
    let tone = Rc::new(tone);
    let trail = Rc::new(trail);
    let (icon_tone, title_tone, meta_tone, active_tone) = (tone.clone(), tone.clone(), tone.clone(), tone);
    let (spins, chevrons, open, stopped) = (trail.clone(), trail.clone(), trail.clone(), trail);
    view()
        .w_full()
        .flex_row()
        .items_center()
        .gap(px(6.0))
        .min_h(px(22.0))
        .child(
            svg(move || ImageSrc::svg_markup(glyph()))
                .size(px(14.0))
                .flex_shrink_0()
                .text_color(move || match icon_tone() {
                    Tone::Error => failed(),
                    Tone::Waiting => needs(),
                    _ => faint(),
                }),
        )
        .child(step_title(
            Rc::new(title),
            move || match title_tone() {
                Tone::Active | Tone::Waiting => secondary(),
                _ => muted(),
            },
            move || active_tone() == Tone::Active,
        ))
        .child(diff_stat_view(stat))
        .child(dynamic(move || {
            let waiting = meta_tone() == Tone::Waiting;
            meta().map(|(fact, error)| {
                text(fact).flex_shrink_0().text_size(px(SMALL)).text_color(if error {
                    failed()
                } else if waiting {
                    needs()
                } else {
                    muted()
                })
            })
        }))
        .child(when(move || spins() == Trail::Spinner, || spinner(12.0)))
        .child(when(move || matches!(chevrons(), Trail::Chevron(_)), move || {
            let open = open.clone();
            chevron(12.0, move || open() == Trail::Chevron(true))
        }))
        .child(when(move || stopped() == Trail::Interrupted, || icon(GLYPH_INTERRUPTED, 12.0).text_color(faint())))
}

/// A row's children and details: indented to its title, under a guide at
/// its icon's center.
fn nest() -> View {
    view().ml(px(6.5)).pl(px(14.5)).border_l(px(1.0)).border_l_color(hairline())
}

/// A step's toggle: "Worked for 1m 20s ›", "Explored 3 files ›".
fn disclosure(label: impl Fn() -> String + 'static, open: impl Fn() -> bool + 'static) -> View {
    let open = Rc::new(open);
    let tint = open.clone();
    view()
        .flex_row()
        .items_center()
        .gap(px(6.0))
        .cursor(Cursor::Pointer)
        .child(
            text(label)
                .flex_shrink(1.0)
                .min_w(px(0.0))
                .number_of_lines(1)
                .text_size(px(BODY))
                .line_height(px(20.0))
                .text_color(move || if tint() { secondary() } else { muted() }),
        )
        .child(chevron(14.0, move || open()))
}

/// The model's reasoning, as a step: "Reasoning ›" opens it.
fn reasoning_row(line: ReadSignal<Line>, opened: Signal<std::collections::HashSet<String>>) -> AnyElement {
    let key = stored(format!("reasoning:{}", line.get_untracked().id));
    let open = move || opened.with(|o| o.contains(&key.get()));
    view()
        .child(
            trace_row(
                || GLYPH_REASONING,
                move || if line.get().pending { "Thinking".into() } else { "Reasoning".into() },
                || None,
                move || if line.get().pending { Tone::Active } else { Tone::Plain },
                move || Trail::Chevron(open()),
            )
            .cursor(Cursor::Pointer)
            .label("Reasoning")
            .on_press(move || toggle(opened, key.get())),
        )
        .child(when(open, move || {
            nest().pb(px(4.0)).child(
                text(move || line.get().reasoning.trim().to_owned())
                    .selectable(true)
                    .text_size(px(SMALL))
                    .line_height(px(20.0))
                    .text_color(secondary()),
            )
        }))
        .into_element()
}

/// How much of a tool's output a row shows when opened.
const OUTPUT_SHOWN: usize = 4000;

/// How many lines of a running command's output an opened row shows.
const LIVE_LINES: usize = 6;
/// How many lines of a change an opened edit shows.
const DIFF_SHOWN: usize = 200;

/// A change's lines, added in green and removed in red, scrolling past a
/// few screens of it.
fn diff_view(diff: String) -> AnyElement {
    let rows: Vec<AnyElement> = diff
        .lines()
        .take(DIFF_SHOWN)
        .map(|line| {
            let (mark, rest) = line.split_at(line.len().min(1));
            let (color, tint) = match mark {
                "+" => (added(), Some(added_tint())),
                "-" => (removed(), Some(removed_tint())),
                _ => (secondary(), None),
            };
            let row = view().w_full().px(px(10.0)).child(
                text(format!("{} {rest}", if mark.is_empty() { " " } else { mark }))
                    .selectable(true)
                    .font_family("Menlo")
                    .text_size(px(SMALL))
                    .line_height(px(18.0))
                    .text_color(color),
            );
            match tint {
                Some(tint) => row.bg(tint).into_element(),
                None => row.into_element(),
            }
        })
        .collect();
    view()
        .rounded(px(8.0))
        .border(px(LINE))
        .border_color(border())
        .overflow_hidden()
        .child(scroll().max_h(px(288.0)).child(view().w_full().py(px(4.0)).child(rows)))
        .into_element()
}

/// A tool call: one row; pressing it shows what the tool gave back: a
/// running command's latest output, an edit's change, else its result.
fn tool_row(line: ReadSignal<Line>, project: Option<String>, opened: Signal<std::collections::HashSet<String>>) -> AnyElement {
    let key = stored(format!("tool-detail:{}", line.get_untracked().id));
    let open = move || opened.with(|o| o.contains(&key.get()));
    let status = move || match line.get().kind {
        LineKind::Tool { status, .. } => status,
        _ => ToolStatus::Done,
    };
    let output = move || match line.get().kind {
        LineKind::Tool { output, .. } => output,
        _ => String::new(),
    };
    let running = move || status() == ToolStatus::Running;
    let live = move || if running() { crate::trace::output_tail(&line.get().live, LIVE_LINES) } else { String::new() };
    let has_diff = move || line.with(|l| !l.diff.is_empty()) && !matches!(status(), ToolStatus::Failed | ToolStatus::Denied);
    let expandable = move || !output().trim().is_empty() || has_diff() || !live().is_empty();
    view()
        .child(
            trace_row_with(
                move || tool_glyph(&line.get()),
                move || crate::trace::tool_title(&line.get(), project.as_deref()),
                move || match status() {
                    ToolStatus::Failed => Some(("Failed".into(), true)),
                    ToolStatus::Denied => Some(("Denied".into(), false)),
                    ToolStatus::Interrupted => Some(("Interrupted".into(), false)),
                    _ => None,
                },
                move || has_diff().then(|| crate::trace::diff_stat(&line.get().diff)),
                move || match status() {
                    ToolStatus::Queued | ToolStatus::Running => Tone::Active,
                    ToolStatus::NeedsInput => Tone::Waiting,
                    ToolStatus::Failed => Tone::Error,
                    _ => Tone::Plain,
                },
                move || match status() {
                    ToolStatus::Running => Trail::Spinner,
                    ToolStatus::Interrupted => Trail::Interrupted,
                    _ if expandable() => Trail::Chevron(open()),
                    _ => Trail::Nothing,
                },
            )
            .cursor(Cursor::Pointer)
            .on_press(move || toggle(opened, key.get())),
        )
        // While it runs: the end of what it printed, as a terminal shows it.
        .child(when(move || open() && !live().is_empty(), move || {
            nest().pb(px(4.0)).child(
                text(live)
                    .font_family("Menlo")
                    .text_size(px(SMALL))
                    .line_height(px(18.0))
                    .text_color(muted()),
            )
        }))
        .child(when(move || open() && live().is_empty() && has_diff(), move || {
            nest().pt(px(2.0)).pb(px(6.0)).child(dynamic(move || diff_view(line.get().diff)))
        }))
        .child(when(move || open() && live().is_empty() && !has_diff() && !output().trim().is_empty(), move || {
            nest().pt(px(2.0)).pb(px(6.0)).child(
                view()
                    .px(px(10.0))
                    .py(px(8.0))
                    .rounded(px(8.0))
                    .border(px(LINE))
                    .border_color(border())
                    .bg(tertiary())
                    .child(
                        text(move || {
                            let current = line.get();
                            let full = output();
                            let full = match &current.kind {
                                LineKind::Tool { name, .. } if name == "bash" => crate::trace::command_output(&full).to_owned(),
                                _ => full,
                            };
                            let shown: String = full.chars().take(OUTPUT_SHOWN).collect();
                            let shown = if shown.len() < full.len() { format!("{shown}\n…") } else { shown };
                            match &current.kind {
                                LineKind::Tool { name, .. } if name == "bash" => {
                                    let command = current.text.strip_prefix("Ran ").unwrap_or(&current.text);
                                    format!("$ {command}\n{}", shown.trim_end())
                                }
                                _ => shown.trim_end().to_owned(),
                            }
                        })
                        .selectable(true)
                        .font_family("Menlo")
                        .text_size(px(SMALL))
                        .line_height(px(18.0))
                        .text_color(secondary()),
                    ),
            )
        }))
        .into_element()
}

/// One step of a group: a lone call, a reasoning, or a run of looking
/// around that opens onto its calls.
fn stage_view(stage: ReadSignal<crate::trace::Stage>, project: Option<String>, opened: Signal<std::collections::HashSet<String>>) -> AnyElement {
    use crate::trace;
    let key = stored(stage.get_untracked().id());
    let open = move || opened.with(|o| o.contains(&key.get()));
    let merged = move || stage.with(|s| s.members.len() > 1);
    let rows = {
        let project = project.clone();
        move || {
            let project = project.clone();
            each(
                move || stage.get().members,
                |line: &Line| line.id.clone(),
                move |line, _| {
                    if matches!(line.get_untracked().kind, LineKind::Tool { .. }) {
                        tool_row(line, project.clone(), opened)
                    } else {
                        reasoning_row(line, opened)
                    }
                },
            )
        }
    };
    let single = rows.clone();
    view()
        .test_id(&format!("trace-{}", stage.get_untracked().id()))
        .child(when(move || !merged(), move || single()))
        .child(when(merged, move || {
            let project = project.clone();
            let rows = rows.clone();
            view()
                .child(
                    trace_row(
                        move || kind_glyph(stage.get().kind),
                        move || trace::stage_title(&stage.get(), project.as_deref()),
                        move || match stage.with(|s| s.failures()) {
                            0 => None,
                            n => Some((format!("{n} failed"), true)),
                        },
                        move || {
                            let s = stage.get();
                            if s.waiting() { Tone::Waiting } else if s.running() || s.queued() { Tone::Active } else { Tone::Plain }
                        },
                        move || {
                            let s = stage.get();
                            if s.running() { Trail::Spinner } else if s.interrupted() { Trail::Interrupted } else { Trail::Chevron(open()) }
                        },
                    )
                    .cursor(Cursor::Pointer)
                    .on_press(move || toggle(opened, key.get())),
                )
                .child(when(open, move || nest().child(rows())))
        }))
        .into_element()
}

/// Tool calls in a row: one step as it is; several under a row saying
/// what they did (what runs now, while they run), opening onto each.
fn group_view(block: ReadSignal<Block>, conversation: Conversation, project: Option<String>) -> AnyElement {
    use crate::trace;
    let key = stored(block.get_untracked().id().to_owned());
    let closed = stored(format!("closed:{}", block.get_untracked().id()));
    let opened = conversation.opened;
    let members = move || {
        let ids = match block.get() {
            Block::Group { members, .. } => members,
            _ => Vec::new(),
        };
        conversation.lines.with(|lines| ids.iter().filter_map(|id| lines.iter().find(|l| &l.id == id).cloned()).collect::<Vec<Line>>())
    };
    let stages = move || trace::stages(&members());
    let multi = move || stages().len() > 1;
    // Open while a step in it waits on the person, until they close it.
    let waits = move || stages().iter().any(trace::Stage::waiting);
    let open = move || opened.with(|o| o.contains(&key.get()) || (waits() && !o.contains(&closed.get())));
    let flip = move || {
        let now = open();
        opened.update(|o| {
            if now {
                o.remove(&key.get());
                o.insert(closed.get());
            } else {
                o.insert(key.get());
                o.remove(&closed.get());
            }
        })
    };
    let active = move || matches!(block.get(), Block::Group { active: true, .. });
    // The step that runs or waits now, else the first.
    let lead = move || {
        let all = stages();
        all.iter().rev().find(|s| s.running() || s.waiting() || s.queued()).cloned().or_else(|| all.first().cloned())
    };
    let each_stage = {
        let project = project.clone();
        move || {
            let project = project.clone();
            each(stages, |stage: &trace::Stage| stage.id(), move |stage, _| stage_view(stage, project.clone(), opened))
        }
    };
    let single = each_stage.clone();
    let header_project = project.clone();
    view()
        .child(when(move || !multi(), move || single()))
        .child(when(multi, move || {
            let project = header_project.clone();
            let each_stage = each_stage.clone();
            view()
                .child(
                    trace_row(
                        move || lead().map(|s| kind_glyph(s.kind)).unwrap_or(GLYPH_TOOL),
                        move || match lead() {
                            Some(s) if active() && (s.running() || s.waiting() || s.queued()) => trace::stage_title(&s, project.as_deref()),
                            _ => trace::summary(&stages()),
                        },
                        move || match stages().iter().map(trace::Stage::failures).sum::<usize>() {
                            0 => None,
                            n => Some((format!("{n} failed"), true)),
                        },
                        move || match lead() {
                            Some(s) if s.waiting() => Tone::Waiting,
                            Some(s) if active() && (s.running() || s.queued()) => Tone::Active,
                            _ => Tone::Plain,
                        },
                        move || Trail::Chevron(open()),
                    )
                    .cursor(Cursor::Pointer)
                    .label("Work")
                    .test_id(&format!("trace-{}", block.get_untracked().id()))
                    .on_press(move || flip()),
                )
                .child(when(open, move || nest().child(each_stage())))
        }))
        .into_element()
}

/// "Worked for 1m 20s ›" above a finished turn's reply, with a rule under
/// it; opens the work.
fn worked_toggle(label: impl Fn() -> String + 'static, key: String, opened: Signal<std::collections::HashSet<String>>) -> View {
    let stored_key = stored(key);
    view()
        .w_full()
        .gap(px(8.0))
        .child(
            disclosure(label, move || opened.with(|o| o.contains(&stored_key.get())))
                .label("Worked")
                .on_press(move || toggle(opened, stored_key.get())),
        )
        .child(view().w_full().h(px(0.0)).border_t(px(LINE)).border_color(border()))
}

/// "Worked for 1m 20s" above a legacy turn's reply; opens the work.
fn worked_divider(block: ReadSignal<Block>, id: String, opened: Signal<std::collections::HashSet<String>>) -> impl IntoElement {
    worked_toggle(
        move || match block.get() {
            Block::Worked { millis, .. } => format!("Worked for {}", crate::trace::duration(millis)),
            _ => String::new(),
        },
        id,
        opened,
    )
}

/// What a turn's header shows.
#[derive(Clone, Copy, PartialEq)]
enum Header {
    Nothing,
    /// Running, with finished work folded: what it did, opening onto it.
    Live,
    /// Done: "Worked for …", opening onto the work.
    Worked,
    /// Stopped, failed or cut off: said once, the work stays open.
    Ended,
}

/// A turn keeps one header and one disclosure identity for its lifetime:
/// while it runs, a row saying what its folded work did; once done,
/// "Worked for …" over the work folded before its reply.
fn turn_header(block: ReadSignal<Block>, conversation: Conversation, id: String) -> impl IntoElement {
    use crate::trace;
    let opened = conversation.opened;
    let mode = move || match block.get() {
        Block::Turn { status, live_fold, .. } if trace::running(status) => if live_fold { Header::Live } else { Header::Nothing },
        Block::Turn { status: TurnStatus::Completed, collapsible: true, .. } => Header::Worked,
        Block::Turn { status: TurnStatus::Completed, .. } => Header::Nothing,
        Block::Turn { .. } => Header::Ended,
        _ => Header::Nothing,
    };
    let elapsed = move || match block.get() {
        Block::Turn { started_at, completed_at, .. } => completed_at.map(|end| trace::duration(end - started_at)),
        _ => None,
    };
    let live_id = id.clone();
    let turn = stored(id.clone());
    view()
        .w_full()
        .child(when(move || mode() == Header::Live, move || {
            let key = stored(trace::live_key(&live_id));
            disclosure(
                move || {
                    // What the folded work did: its finished calls.
                    let turn = turn.get();
                    conversation.lines.with(|lines| {
                        let mine: Vec<Line> = lines
                            .iter()
                            .filter(|l| l.turn_id.as_deref() == Some(turn.as_str()) && l.id != turn)
                            .filter(|l| match &l.kind {
                                LineKind::Tool { name, status, .. } => {
                                    !status.unfinished() && !matches!(name.as_str(), "create_task" | "continue_task" | "spawn_agent" | "run_workflow")
                                }
                                LineKind::Reasoning => !l.reasoning.trim().is_empty(),
                                _ => false,
                            })
                            .cloned()
                            .collect();
                        if mine.is_empty() { "Show activity".to_owned() } else { trace::summary(&trace::stages(&mine)) }
                    })
                },
                move || opened.with(|o| o.contains(&key.get())),
            )
            .label("Show work")
            .on_press(move || toggle(opened, key.get()))
        }))
        .child(when(move || mode() == Header::Worked, move || {
            worked_toggle(move || format!("Worked for {}", elapsed().unwrap_or_else(|| "a while".into())), id.clone(), opened)
        }))
        .child(when(move || mode() == Header::Ended, move || {
            view()
                .w_full()
                .gap(px(8.0))
                .child(
                    text(move || {
                        let Block::Turn { status, .. } = block.get() else { return String::new() };
                        let after = elapsed().map(|d| format!(" after {d}")).unwrap_or_default();
                        match status {
                            TurnStatus::Stopped => format!("You stopped{after}"),
                            TurnStatus::Failed => format!("Failed{after}"),
                            _ => format!("Interrupted{after}"),
                        }
                    })
                    .label("Turn status")
                    .text_size(px(BODY))
                    .line_height(px(20.0))
                    .text_color(muted()),
                )
                .child(view().w_full().h(px(0.0)).border_t(px(LINE)).border_color(border()))
        }))
}

/// How many lines of the current step's output the status panel shows.
const STATUS_TAIL: usize = 4;

/// A running turn's last row: what it does now, and for how long; with a
/// command running, it opens onto the command and its latest output.
fn status_line(block: ReadSignal<Block>, conversation: Conversation, project: Option<String>) -> impl IntoElement {
    use crate::trace;
    let now = signal(bubble_store::now_ms());
    set_interval(std::time::Duration::from_secs(1), move || now.set(bubble_store::now_ms()));
    let opened = conversation.opened;
    let key = stored(format!("status-step:{}", block.get_untracked().id()));
    let activity = move || match block.get() {
        Block::Status { turn, status, .. } => conversation.lines.with(|lines| trace::activity(lines, &turn, status, project.as_deref())),
        _ => (String::new(), false),
    };
    let step = move || match block.get() {
        Block::Status { turn, .. } => conversation.lines.with(|lines| trace::running_step(lines, &turn)),
        _ => None,
    };
    let command = move || {
        step().filter(|l| matches!(&l.kind, LineKind::Tool { name, .. } if name == "bash"))
            .map(|l| l.text.strip_prefix("Ran ").unwrap_or(&l.text).to_owned())
    };
    let tail = move || step().map(|l| trace::output_tail(&l.live, STATUS_TAIL)).unwrap_or_default();
    let can_open = move || command().is_some() || !tail().is_empty();
    let open = move || can_open() && opened.with(|o| o.contains(&key.get()));
    let label = activity.clone();
    let panel_label = activity.clone();
    let tint = activity.clone();
    let waiting = move || activity().1;
    let spinning = waiting.clone();
    let (breathing, step_label) = (waiting.clone(), Rc::new(move || label().0) as Rc<dyn Fn() -> String>);
    view()
        .gap(px(4.0))
        .child(when(open, move || {
            let panel_label = panel_label.clone();
            let title_label = panel_label.clone();
            view()
                .gap(px(6.0))
                .px(px(10.0))
                .py(px(8.0))
                .rounded(px(10.0))
                .border(px(LINE))
                .border_color(border())
                .child(
                    view()
                        .flex_row()
                        .items_center()
                        .gap(px(8.0))
                        .child(text(move || title_label().0).flex_shrink(1.0).min_w(px(0.0)).number_of_lines(1).text_size(px(SMALL)).text_color(ink()))
                        // The command itself, when the title does not already say it.
                        .child(
                            text(move || command().filter(|c| !panel_label().0.contains(c.as_str())).unwrap_or_default())
                                .flex_1()
                                .min_w(px(0.0))
                                .number_of_lines(1)
                                .font_family("Menlo")
                                .text_size(px(SMALL))
                                .text_color(muted()),
                        )
                        .child(
                            text(move || step().map(|l| trace::duration(now.get() - l.at)).unwrap_or_default())
                                .flex_shrink_0()
                                .text_size(px(SMALL))
                                .text_color(muted()),
                        ),
                )
                .child(when(move || !tail().is_empty(), move || {
                    view().px(px(10.0)).py(px(6.0)).rounded(px(6.0)).bg(tertiary()).child(
                        text(tail)
                            .font_family("Menlo")
                            .text_size(px(SMALL))
                            .line_height(px(18.0))
                            .text_color(secondary()),
                    )
                }))
        }))
        .child(
            view()
                .flex_row()
                .items_center()
                .gap(px(6.0))
                .min_h(px(22.0))
                .label("Turn status")
                .cursor(Cursor::Pointer)
                .on_press(move || if can_open() { toggle(opened, key.get()) })
                .child(when(move || !spinning(), || spinner(13.0)))
                .child(when(waiting, || icon(GLYPH_APPROVAL, 13.0).text_color(needs())))
                .child(step_title(step_label, move || if tint().1 { needs() } else { secondary() }, move || !breathing()))
                .child(
                    text(move || match block.get() {
                        Block::Status { started_at, .. } if now.get() - started_at >= 1000 => format!("· {}", trace::duration(now.get() - started_at)),
                        _ => String::new(),
                    })
                    .flex_shrink_0()
                    .text_size(px(SMALL))
                    .line_height(px(20.0))
                    .text_color(muted()),
                )
                // Up while the panel above is closed, down once it is open.
                .child(when(can_open, move || {
                    icon(GLYPH_CHEVRON, 12.0).text_color(faint()).rotate(move || if open() { deg(90.0) } else { deg(-90.0) })
                })),
        )
}

/// The children a task's agent started: who, doing what, and how it went;
/// a finished one opens to its answer.
fn subagents_view(conversation: Conversation, anchor: Option<ReadSignal<Line>>) -> impl IntoElement {
    use bubble_core::delegate::{Snapshot, Status};
    let included = move |child: &Snapshot| {
        match anchor {
            Some(line) => line.get().child_ids.contains(&child.id),
            None => !conversation.lines.with(|lines| lines.iter().any(|l| l.child_ids.contains(&child.id))) && child.status != Status::Closed,
        }
    };
    let visible = move || conversation.subagents.with(|s| s.iter().any(included));
    when(visible, move || {
        view()
            .gap(px(6.0))
            .child(text("Subagents").text_size(px(SMALL)).text_color(muted()))
            .child(each(
                move || conversation.subagents.get().into_iter().filter(included).collect::<Vec<_>>(),
                |child: &Snapshot| child.id.clone(),
                move |child, _| {
                    let key = stored(format!("child:{}", child.get_untracked().id));
                    let opened = conversation.opened;
                    let open = move || opened.with(|o| o.contains(&key.get()));
                    let status_color = move || match child.get().status {
                        Status::Failed | Status::Blocked => failed(),
                        Status::Completed => ink(),
                        _ => muted(),
                    };
                    let title = move || {
                        let c = child.get();
                        c.description.clone().unwrap_or(c.task).lines().next().unwrap_or_default().to_owned()
                    };
                    view()
                        .gap(px(8.0))
                        .px(px(12.0))
                        .py(px(8.0))
                        .rounded(px(10.0))
                        .border(px(LINE))
                        .border_color(border())
                        .bg(pane())
                        .child(
                            view()
                                .flex_row()
                                .items_center()
                                .gap(px(10.0))
                                .cursor(Cursor::Pointer)
                                .on_press(move || { opened.update(|o| { if !o.remove(&key.get()) { o.insert(key.get()); } }); })
                                .label(move || format!("Subagent {}", child.get().nickname))
                                .child(text(move || child.get().nickname).text_size(px(BODY)).text_color(ink()))
                                .child(text(move || child.get().profile).text_size(px(SMALL)).text_color(muted()))
                                .child(text(title).flex_1().min_w(px(0.0)).number_of_lines(1).text_size(px(SMALL)).text_color(secondary()))
                                .child(text(move || child.get().status.label()).text_size(px(SMALL)).text_color(status_color)),
                        )
                        .child(when(move || open(), move || {
                            // Follows the child: its latest step, then its answer.
                            dynamic(move || {
                                let c = child.get();
                                if !c.summary.is_empty() && c.status.is_final() {
                                    let worktree = c.worktree.as_ref().filter(|w| w.changed == Some(true)).map(|w| {
                                        if w.applied {
                                            "Its changes were applied to the project.".to_owned()
                                        } else if w.removed {
                                            "Its changes were discarded.".to_owned()
                                        } else {
                                            format!("Changes kept for review: open Changes to apply or discard them.\n{}", w.diff_stat)
                                        }
                                    });
                                    view()
                                        .gap(px(6.0))
                                        .child(markdown(c.summary.clone()))
                                        .child(when(
                                            {
                                                let shown = worktree.is_some();
                                                move || shown
                                            },
                                            move || text(worktree.clone().unwrap_or_default()).text_size(px(SMALL)).text_color(muted()),
                                        ))
                                        .into_element()
                                } else if let Some(error) = c.error.clone() {
                                    text(error).text_size(px(SMALL)).text_color(failed()).into_element()
                                } else {
                                    let note = c.notes.last().map(|n| format!("{n}…")).unwrap_or_else(|| "Starting…".into());
                                    text(note).text_size(px(SMALL)).text_color(muted()).number_of_lines(2).into_element()
                                }
                            })
                        }))
                },
            ))
    })
}

fn line_view(app: AppState, conversation: Conversation, line: ReadSignal<Line>, project: Option<String>) -> AnyElement {
    match line.get_untracked().kind {
        LineKind::Tool { name, .. } if name == "create_task" || name == "continue_task" => task_card_line(app, conversation, line),
        LineKind::Tool { ref name, .. } if name == "spawn_agent" || name == "run_workflow" => view()
            .gap(px(6.0)).child(tool_row(line, project, conversation.opened)).child(subagents_view(conversation, Some(line))).into_element(),
        LineKind::Tool { .. } => tool_row(line, project, conversation.opened),
        LineKind::Reasoning => reasoning_row(line, conversation.opened),
        LineKind::Turn { .. } => view().into_element(),
        LineKind::Approval { .. } => {
            let decided = move || matches!(line.get().kind, LineKind::Approval { decided: Some(_), .. });
            let undecided = decided.clone();
            view()
                .child(when(move || !undecided(), move || approval_card(app, conversation, line)))
                .child(when(decided, move || approval_row(line)))
                .into_element()
        }
        LineKind::Message(MessageRole::User) => view()
            .child(stamp_line(line))
            .child(
                view()
            .self_end()
            .max_w(percentage(0.8))
            .items_end()
            .gap(px(4.0))
            .child(when(move || !line.get().text.trim().is_empty(), move || {
                view()
                    .bg(user_bubble())
                    .rounded(px(18.0))
                    .px(px(16.0))
                    .py(px(9.0))
                    .child(text(move || line.get().text).text_size(px(BODY)).line_height(px(21.0)).text_color(ink()))
            }))
            .child(when(move || line.get().images > 0, move || {
                text(move || match line.get().images {
                    1 => "1 image".to_owned(),
                    n => format!("{n} images"),
                })
                .text_size(px(SMALL))
                .text_color(muted())
            })),
            )
            .into_element(),
        LineKind::Message(MessageRole::Assistant) => dynamic(move || {
            let current = line.get();
            let waiting_on_person = move || {
                conversation
                    .lines
                    .with(|lines| lines.iter().any(|l| matches!(l.kind, LineKind::Approval { decided: None, .. })))
            };
            if current.pending && current.text.is_empty() && waiting_on_person() {
                view().into_element()
            } else if current.pending && current.text.is_empty() && conversation.queued.get() {
                text("Queued until another bot finishes").text_size(px(BODY)).text_color(muted()).into_element()
            } else if current.pending && current.text.is_empty() {
                // Reasoning shows as it streams: its last few lines.
                let tail: Vec<&str> = current.reasoning.lines().filter(|l| !l.trim().is_empty()).collect();
                let tail = tail[tail.len().saturating_sub(3)..].join("\n");
                view()
                    .gap(px(4.0))
                    .child(text("Thinking…").text_size(px(BODY)).text_color(muted()))
                    .child((!tail.is_empty()).then(|| {
                        text(tail).text_size(px(SMALL)).line_height(px(18.0)).text_color(muted()).number_of_lines(3)
                    }))
                    .into_element()
            } else {
                // What the bot says is a bubble; its reasoning is part of
                // the work above it, not of the reply.
                view()
                    .child((!current.text.trim().is_empty()).then(|| {
                        view().self_start().max_w(percentage(1.0)).px(px(14.0)).py(px(10.0)).rounded(px(18.0)).bg(bot_bubble()).child(markdown(current.text.clone()))
                    }))
                    .into_element()
            }
        }),
        LineKind::Message(MessageRole::Tool | MessageRole::Checkpoint) => view().into_element(),
        LineKind::Message(MessageRole::External) => view()
            .gap(px(6.0))
            .child(stamp_line(line))
            .child(text("From GitHub").text_size(px(SMALL)).text_color(muted()))
            .child(
                view()
                    .px(px(14.0))
                    .py(px(10.0))
                    .rounded(px(18.0))
                    .bg(tertiary())
                    .child(text(move || line.get().text).text_size(px(BODY)).line_height(px(21.0))),
            )
            .into_element(),
        LineKind::Message(MessageRole::System) => view()
            .gap(px(6.0))
            .child(stamp_line(line))
            .child(text("From Bubble").text_size(px(SMALL)).text_color(muted()))
            .child(
                view()
                    .px(px(14.0))
                    .py(px(10.0))
                    .rounded(px(18.0))
                    .bg(tertiary())
                    .child(text(move || line.get().text).text_size(px(BODY)).line_height(px(21.0))),
            )
            .into_element(),
        LineKind::RunStart(index) => view()
            .pt(px(8.0))
            .gap(px(8.0))
            .child(
                view()
                    .flex_row()
                    .gap(px(8.0))
                    .child(text(format!("Run {index}")).text_size(px(SMALL)).text_color(secondary()))
                    .child(text(move || line.get().text).text_size(px(SMALL)).text_color(muted()).number_of_lines(1)),
            )
            .child(view().h(px(0.0)).border_t(px(LINE)).border_color(hairline()))
            .into_element(),
        LineKind::Notice => text(move || line.get().text).text_size(px(SMALL)).text_color(muted()).into_element(),
        LineKind::Failure => view()
            .flex_row()
            .gap(px(8.0))
            .child(state_icon(|| TaskState::Failed, 14.0).mt(px(4.0)))
            .child(
                text(move || line.get().text)
                    .flex_1()
                    .text_size(px(BODY))
                    .line_height(px(21.0))
                    .text_color(failed()),
            )
            .into_element(),
    }
}

/// The Manager started or followed up on a task: a card that follows the
/// task's state and opens it.
fn task_card_line(app: AppState, conversation: Conversation, line: ReadSignal<Line>) -> AnyElement {
    let task_id = move || match line.get().kind {
        LineKind::Tool { status: ToolStatus::Done, output, .. } => crate::manager::task_id_in(&output),
        _ => None,
    };
    dynamic(move || {
        let Some(id) = task_id() else { return tool_row(line, None, conversation.opened) };
        let lookup = id.clone();
        let summary = move || app.tasks.with(|tasks| tasks.iter().find(|t| t.task.id == lookup).cloned());
        let for_icon = summary.clone();
        let for_title = summary.clone();
        let for_meta = summary.clone();
        let followed_up = matches!(line.get_untracked().kind, LineKind::Tool { ref name, .. } if name == "continue_task");
        view()
            .gap(px(6.0))
            .child(when(move || followed_up, || text("Followed up").text_size(px(SMALL)).text_color(muted())))
            .child(
                view()
                    .max_w(px(520.0))
                    .px(px(14.0))
                    .py(px(10.0))
                    .gap(px(3.0))
                    .rounded(px(12.0))
                    .border(px(LINE))
                    .border_color(border())
                    .bg(pane())
                    .cursor(Cursor::Pointer)
                    .hover(|s| s.bg(tertiary()))
                    .on_press(move || app.route.set(crate::state::Route::Task(id.clone())))
                    .child(
                        view()
                            .flex_row()
                            .items_center()
                            .gap(px(8.0))
                            .child(state_icon(move || for_icon().map(|s| s.state).unwrap_or(TaskState::NotStarted), 14.0))
                            .child(
                                text(move || for_title().map(|s| s.task.title).unwrap_or_default())
                                    .flex_1()
                                    .text_size(px(BODY))
                                    .font_weight(FontWeight::MEDIUM)
                                    .number_of_lines(1),
                            ),
                    )
                    .child(
                        text(move || match for_meta() {
                            Some(s) => {
                                let project = s.task.project_cwd.as_deref().map(project_name).unwrap_or_else(|| "No project".into());
                                format!("{}, {project}", state_label(s.state))
                            }
                            None => "This task was removed".to_owned(),
                        })
                        .pl(px(22.0))
                        .text_size(px(SMALL))
                        .text_color(muted()),
                    ),
            )
            .into_element()
    })
}

/// An answered request, as a step of the trace: what it asked, and the
/// answer.
fn approval_row(line: ReadSignal<Line>) -> AnyElement {
    let decided = move || match line.get().kind {
        LineKind::Approval { decided, .. } => decided,
        _ => None,
    };
    trace_row(
        || GLYPH_APPROVAL,
        move || match line.get().kind {
            LineKind::Approval { summary, .. } => summary,
            _ => String::new(),
        },
        move || {
            decided().map(|d| match d {
                Decision::AllowOnce => ("Allowed once".to_owned(), false),
                Decision::AllowForTask => ("Allowed for this task".to_owned(), false),
                Decision::Refuse("Turn ended") => ("Interrupted".to_owned(), false),
                Decision::Deny | Decision::Refuse(_) => ("Denied".to_owned(), true),
            })
        },
        || Tone::Plain,
        || Trail::Nothing,
    )
    .into_element()
}

/// The agent waits for a yes: what it wants to do, and three answers.
fn approval_card(app: AppState, conversation: Conversation, line: ReadSignal<Line>) -> AnyElement {
    let parts = move || match line.get().kind {
        LineKind::Approval { summary, detail, decided, .. } => (summary, detail, decided),
        _ => (String::new(), String::new(), None),
    };
    let rememberable = matches!(line.get_untracked().kind, LineKind::Approval { rememberable: true, .. });
    let id = move || line.get_untracked().id;
    let answer = move |decision: Decision| move || conversation.decide(app, &id(), decision);
    view()
        .gap(px(10.0))
        .p(px(14.0))
        .rounded(px(12.0))
        .border(px(LINE))
        .border_color(border())
        .bg(pane())
        .child(text(move || parts().0).text_size(px(BODY)).font_weight(FontWeight::SEMIBOLD))
        .child(
            view().px(px(12.0)).py(px(8.0)).rounded(px(8.0)).bg(tertiary()).child(
                text(move || parts().1)
                    .font_family("Menlo")
                    .text_size(px(SMALL))
                    .line_height(px(18.0))
                    .text_color(ink()),
            ),
        )
        .child(dynamic(move || match parts().2 {
            None => view()
                .flex_row()
                .gap(px(8.0))
                .child(primary_button("Allow once", answer(Decision::AllowOnce)))
                .child(when(move || rememberable, move || button("Allow for this task", answer(Decision::AllowForTask))))
                .child(button("Deny", answer(Decision::Deny)))
                .into_element(),
            Some(decision) => text(match decision {
                Decision::AllowOnce => "Allowed once",
                Decision::AllowForTask => "Allowed for this task",
                Decision::Refuse("Turn ended") => "Interrupted",
                Decision::Deny | Decision::Refuse(_) => "Denied",
            })
            .text_size(px(SMALL))
            .text_color(muted())
            .into_element(),
        }))
        .into_element()
}

const STOP_ICON: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><rect x="4.5" y="4.5" width="7" height="7" rx="1.5" fill="currentColor"/></svg>"##;
const SEND_ICON: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><path d="M8 13V3M3.5 7.5L8 3l4.5 4.5" stroke="currentColor" stroke-width="1.75" fill="none" stroke-linecap="round" stroke-linejoin="round"/></svg>"##;

/// Whose composer it is: the Manager's, or a task's (with its own model and
/// effort, and its project's files to mention).
#[derive(Clone, PartialEq)]
pub enum ComposerFor {
    Manager,
    Task(String),
}

const ATTACH_ICON: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><path d="M8 3.5v9M3.5 8h9" stroke="currentColor" stroke-width="1.5" fill="none" stroke-linecap="round"/></svg>"##;
const REMOVE_ICON: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><path d="M5 5l6 6m0-6l-6 6" stroke="#FFFFFF" stroke-width="1.6" fill="none" stroke-linecap="round"/></svg>"##;

/// The person's messages in a conversation, oldest first, without repeats
/// in a row: what Up and Down bring back.
fn sent_before(conversation: Conversation) -> Vec<String> {
    conversation.lines.with_untracked(|lines| {
        let mut sent: Vec<String> = Vec::new();
        for line in lines {
            if line.kind == LineKind::Message(MessageRole::User) && !line.text.trim().is_empty() {
                if sent.last() != Some(&line.text) {
                    sent.push(line.text.clone());
                }
            }
        }
        sent
    })
}

/// A small text menu in the composer's bottom row.
fn composer_menu(
    label: &'static str,
    shown: impl Fn() -> String + 'static,
    options: impl Fn() -> Vec<(String, bool, Rc<dyn Fn()>)> + 'static,
) -> impl IntoElement {
    toned_menu(label, shown, || false, options)
}

/// A composer menu whose shown value turns red while `warn` holds.
fn toned_menu(
    label: &'static str,
    shown: impl Fn() -> String + 'static,
    warn: impl Fn() -> bool + 'static,
    options: impl Fn() -> Vec<(String, bool, Rc<dyn Fn()>)> + 'static,
) -> impl IntoElement {
    nsh_base::menu()
        .placement(Placement::Top)
        .align(Align::End)
        .offset(px(6.0))
        .popup_style(|s| {
            s.bg(pane()).border(px(LINE)).border_color(border()).rounded(px(10.0)).p(px(4.0)).min_w(px(200.0)).shadow_lg()
        })
        .trigger(
            view()
                .h(px(28.0))
                .px(px(8.0))
                .justify_center()
                .rounded(px(6.0))
                .cursor(Cursor::Pointer)
                .hover(|s| s.bg(tertiary()))
                .label(label)
                .child(
                    text(shown)
                        .text_size(px(BODY))
                        .text_color(move || if warn() { failed() } else { secondary() })
                        .number_of_lines(1),
                ),
        )
        .child(dynamic(move || {
            options()
                .into_iter()
                .map(|(name, chosen, pick)| {
                    nsh_base::menu_item()
                        .accessibility_label(name.clone())
                        .rounded(px(6.0))
                        .cursor(Cursor::Pointer)
                        .hover(|s| s.bg(tertiary()))
                        .bg(if chosen { tertiary() } else { pane() })
                        .on_select(move || pick())
                        .child(
                            view()
                                .h(px(30.0))
                                .px(px(10.0))
                                .justify_center()
                                .child(text(name).text_size(px(BODY)).text_color(ink()).number_of_lines(1)),
                        )
                        .into_element()
                })
                .collect::<Vec<_>>()
        }))
}

/// The model and effort pickers: a task's own, or the Manager's (which are
/// Settings').
fn model_pickers(app: AppState, target: ComposerFor) -> impl IntoElement {
    use bubble_core::Level as ThinkingLevel;
    use bubble_store::RunRole;
    let task_id = match &target {
        ComposerFor::Task(id) => Some(id.clone()),
        ComposerFor::Manager => None,
    };
    let current = {
        let task_id = task_id.clone();
        move || match &task_id {
            Some(id) => app.task_model_of(id),
            None => {
                app.manager_model.get();
                app.model_for(RunRole::Manager)
            }
        }
    };
    let level = {
        let task_id = task_id.clone();
        move || match &task_id {
            Some(id) => app.task_thinking_of(id),
            None => app.thinking.get(),
        }
    };
    let model_options = {
        let task_id = task_id.clone();
        let current = current.clone();
        move || {
            let chosen = current();
            let mut models = app.models.get();
            if let Some(c) = &chosen {
                if !models.contains(c) {
                    models.push(c.clone());
                }
            }
            models
                .into_iter()
                .map(|model| {
                    let task_id = task_id.clone();
                    let picked = model.clone();
                    let pick: Rc<dyn Fn()> = Rc::new(move || match &task_id {
                        Some(id) => app.set_task_model(id, Some(picked.clone())),
                        None => app.set_model_setting(RunRole::Manager, Some(picked.clone())),
                    });
                    let is = chosen.as_deref() == Some(model.as_str());
                    (model, is, pick)
                })
                .collect::<Vec<_>>()
        }
    };
    let levels = {
        let current = current.clone();
        move || current().map(|m| app.levels_of(&m)).unwrap_or_default()
    };
    let level_options = {
        let task_id = task_id.clone();
        let levels = levels.clone();
        let level = level.clone();
        move || {
            let now = level();
            levels()
                .into_iter()
                .map(|l: ThinkingLevel| {
                    let task_id = task_id.clone();
                    let pick: Rc<dyn Fn()> = Rc::new(move || match &task_id {
                        Some(id) => app.set_task_thinking(id, Some(l)),
                        None => app.set_thinking(l),
                    });
                    (l.as_str().to_owned(), l == now, pick)
                })
                .collect::<Vec<_>>()
        }
    };
    let shown_level = {
        let current = current.clone();
        let level = level.clone();
        move || {
            let Some(reference) = current() else { return String::new() };
            app.effective_level(&reference, level()).as_str().to_owned()
        }
    };
    let thinks = move || levels().iter().any(|l| *l != ThinkingLevel::Off);
    view()
        .flex_row()
        .items_center()
        .child(task_id.clone().map(|id| access_menu(app, id)))
        .child(composer_menu("Model", move || current().unwrap_or_else(|| "No model".into()), model_options))
        .child(when(thinks, move || composer_menu("Effort", shown_level.clone(), level_options.clone())))
}

/// Whether a task's commands ask first or go ahead (Full access). Full
/// access holds while the person sent the latest message; work GitHub or
/// the Manager hands over still asks.
fn access_menu(app: AppState, task_id: String) -> impl IntoElement {
    let full = {
        let task_id = task_id.clone();
        move || app.task_full_access(&task_id)
    };
    let shown = {
        let full = full.clone();
        move || if full() { "Full access".to_owned() } else { "Ask first".to_owned() }
    };
    let options = {
        let full = full.clone();
        move || {
            let on = full();
            [("Ask first", false), ("Full access", true)]
                .into_iter()
                .map(|(name, value)| {
                    let task_id = task_id.clone();
                    let pick: Rc<dyn Fn()> = Rc::new(move || app.set_task_full_access(&task_id, value));
                    (name.to_owned(), on == value, pick)
                })
                .collect::<Vec<_>>()
        }
    };
    toned_menu("Access", shown, full, options)
}

/// Where the person writes: text, `@file` mentions of the task's project,
/// pictures (pasted or attached), Up and Down for what they sent before,
/// and the model and effort it goes to.
pub fn composer(
    app: AppState,
    conversation: Conversation,
    placeholder: String,
    target: ComposerFor,
) -> impl IntoElement {
    use bubble_core::Image;
    use bubble_core::mentions;
    let draft = signal(String::new());
    let images = signal(Vec::<Image>::new());
    let problem = signal(None::<String>);
    let focused = signal(false);
    let recall = signal(None::<usize>);
    let files = signal(None::<Vec<String>>);
    let project = match &target {
        ComposerFor::Task(id) => app
            .with_store(|s| s.get_task(id))
            .flatten()
            .and_then(|t| t.project_cwd)
            .filter(|p| std::path::Path::new(p).is_dir()),
        ComposerFor::Manager => None,
    };
    let add_image = move |result: Result<Image, String>| match result {
        Ok(image) => {
            problem.set(None);
            images.update(|list| list.push(image));
        }
        Err(message) => problem.set(Some(message)),
    };
    let project_for_send = project.clone();
    let send: Rc<dyn Fn()> = Rc::new(move || {
        let text = draft.get_untracked();
        let attached = images.get_untracked();
        if (text.trim().is_empty() && attached.is_empty()) || conversation.busy.get_untracked() {
            return;
        }
        let expanded = project_for_send
            .as_deref()
            .map(|cwd| mentions::expand(text.trim(), std::path::Path::new(cwd)))
            .filter(|e| e != text.trim());
        draft.set(String::new());
        images.set(Vec::new());
        recall.set(None);
        problem.set(None);
        conversation.send_with(app, text, expanded, attached);
    });
    // `@` mentions: the project's files, listed once the first `@` is typed.
    let suggestions = move || -> Vec<String> {
        let current = draft.get();
        let Some((_, query)) = mentions::mention_at_end(&current) else { return Vec::new() };
        let found = files.with(|f| f.as_ref().map(|f| mentions::matching(f, query, 6)).unwrap_or_default());
        // A mention already complete needs no list.
        if found.iter().any(|f| f == query) { Vec::new() } else { found }
    };
    let accept = move |path: String| {
        draft.update(|d| {
            if let Some((at, _)) = mentions::mention_at_end(d) {
                d.truncate(at);
                d.push_str(&format!("@{path} "));
            }
        });
    };
    let project_for_files = project.clone();
    let on_change = move |value: &str| {
        draft.set(value.to_owned());
        if value.contains('@') && files.with_untracked(Option::is_none) {
            if let Some(cwd) = project_for_files.clone() {
                files.set(Some(Vec::new()));
                spawn(async move {
                    if let Ok(list) = background(async move { mentions::list(std::path::Path::new(&cwd)) }).await {
                        files.set(Some(list));
                    }
                });
            }
        }
    };
    // Up and Down: while the draft is empty or one brought back, and the
    // composer has the keyboard.
    let recalling = move || {
        focused.get()
            && suggestions().is_empty()
            && (draft.with(|d| d.is_empty())
                || recall.get().is_some_and(|i| sent_before(conversation).get(i) == Some(&draft.get())))
    };
    let send_on_submit = send.clone();
    let send_on_press = send.clone();
    let can_send =
        move || !conversation.busy.get() && !(draft.with(|d| d.trim().is_empty()) && images.with(|i| i.is_empty()));
    let attach = move || {
        let picked = pick_paths(
            nsh_core::file_dialog::OpenDialog::files()
                .multiple(true)
                .filter(nsh_core::file_dialog::FileFilter::new("Images", ["png", "jpg", "jpeg", "gif", "webp"]))
                .button_label("Attach"),
        );
        spawn(async move {
            if let Ok(Some(paths)) = picked.await {
                for path in paths {
                    add_image(Image::read(&path));
                }
            }
        });
    };
    view().px(px(24.0)).pb(px(18.0)).pt(px(6.0)).child(
        view()
            .w_full()
            .max_w(px(READING_WIDTH - 48.0))
            .mx(auto())
            .bg(pane())
            .border(px(LINE))
            .border_color(border())
            .rounded(px(22.0))
            .shadow_lg()
            .shadow_color(shadow())
            .child(when(recalling, move || {
                on_key_with("up", KeyOptions { when: nsh_core::keymap::When::Always, repeat: true }, move |_| {
                    let sent = sent_before(conversation);
                    if sent.is_empty() {
                        return;
                    }
                    let at = match recall.get_untracked() {
                        Some(i) => i.saturating_sub(1),
                        None => sent.len() - 1,
                    };
                    recall.set(Some(at));
                    draft.set(sent[at].clone());
                });
                on_key_with("down", KeyOptions { when: nsh_core::keymap::When::Always, repeat: true }, move |_| {
                    let sent = sent_before(conversation);
                    match recall.get_untracked() {
                        Some(i) if i + 1 < sent.len() => {
                            recall.set(Some(i + 1));
                            draft.set(sent[i + 1].clone());
                        }
                        Some(_) => {
                            recall.set(None);
                            draft.set(String::new());
                        }
                        None => {}
                    }
                });
                view()
            }))
            .child(when(move || !suggestions().is_empty(), move || {
                on_key_with("tab", KeyOptions { when: nsh_core::keymap::When::Always, repeat: false }, move |_| {
                    if let Some(first) = suggestions().into_iter().next() {
                        accept(first);
                    }
                });
                view()
                    .px(px(8.0))
                    .pt(px(8.0))
                    .gap(px(2.0))
                    .child(each(
                        suggestions,
                        |path: &String| path.clone(),
                        move |path, _| {
                            view()
                                .h(px(28.0))
                                .px(px(10.0))
                                .justify_center()
                                .rounded(px(6.0))
                                .cursor(Cursor::Pointer)
                                .hover(|s| s.bg(tertiary()))
                                .on_press(move || accept(path.get_untracked()))
                                .label(move || format!("Mention {}", path.get()))
                                .child(text(move || path.get()).text_size(px(BODY)).text_color(ink()).number_of_lines(1))
                        },
                    ))
            }))
            .child(when(move || images.with(|i| !i.is_empty()), move || {
                view().flex_row().flex_wrap().gap(px(8.0)).px(px(14.0)).pt(px(12.0)).child(each(
                    move || images.get().into_iter().enumerate().collect::<Vec<_>>(),
                    |(index, image): &(usize, Image)| format!("{index}-{}", image.data.len()),
                    move |entry, _| {
                        let (index, image) = entry.get_untracked();
                        view()
                            .size(px(52.0))
                            .rounded(px(8.0))
                            .overflow_hidden()
                            .border(px(LINE))
                            .border_color(border())
                            .label(format!("Image {}", index + 1))
                            .child(nsh_core::element::image(image.data_url()).size(px(52.0)))
                            .child(
                                view()
                                    .absolute()
                                    .top(px(3.0))
                                    .right(px(3.0))
                                    .size(px(16.0))
                                    .rounded_full()
                                    .items_center()
                                    .justify_center()
                                    .bg(rgba(0x0000_0099))
                                    .cursor(Cursor::Pointer)
                                    .label(format!("Remove image {}", index + 1))
                                    .on_press(move || {
                                        images.update(|list| {
                                            if index < list.len() {
                                                list.remove(index);
                                            }
                                        })
                                    })
                                    .child(icon(REMOVE_ICON, 10.0)),
                            )
                    },
                ))
            }))
            .child(
                nsh_base::input()
                    .value(draft)
                    .placeholder(placeholder.clone())
                    .placeholder_color(move || color(muted()))
                    .multiline(true)
                    .submit_behavior(SubmitBehavior::Submit)
                    .auto_focus(true)
                    .min_h(px(46.0))
                    .px(px(18.0))
                    .pt(px(14.0))
                    .text_size(px(BODY))
                    .text_color(ink())
                    .test_id("composer")
                    .on_focus(move || focused.set(true))
                    .on_blur(move || focused.set(false))
                    .on_change(on_change)
                    .on_paste(move |paste: &PasteEvent| {
                        for file in paste.files.iter().filter(|f| f.mime.starts_with("image/")) {
                            add_image(Image::from_bytes(&file.data, &file.mime));
                        }
                        for uri in &paste.uris {
                            let path = std::path::PathBuf::from(uri.strip_prefix("file://").unwrap_or(uri));
                            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or_default();
                            if Image::media_type_of(ext).is_some() {
                                add_image(Image::read(&path));
                            }
                        }
                    })
                    .on_submit(move |_: &str| send_on_submit()),
            )
            .child(dynamic(move || {
                problem.get().map(|message| {
                    text(message).px(px(18.0)).pt(px(4.0)).text_size(px(SMALL)).text_color(failed())
                })
            }))
            .child(
                view()
                    .flex_row()
                    .items_center()
                    .gap(px(6.0))
                    .pl(px(10.0))
                    .pr(px(10.0))
                    .pb(px(10.0))
                    .child(
                        view()
                            .size(px(28.0))
                            .rounded_full()
                            .items_center()
                            .justify_center()
                            .cursor(Cursor::Pointer)
                            .hover(|s| s.bg(tertiary()))
                            .label("Attach images")
                            .on_press(attach)
                            .child(icon(ATTACH_ICON, 16.0).text_color(secondary())),
                    )
                    .child(view().flex_1())
                    .child(model_pickers(app, target))
                    .child(dynamic(move || {
                        let send_on_press = send_on_press.clone();
                        if conversation.busy.get() {
                            view()
                                .size(px(32.0))
                                .rounded_full()
                                .items_center()
                                .justify_center()
                                .bg(ink())
                                .cursor(Cursor::Pointer)
                                .on_press(move || conversation.stop(app))
                                .label("Stop")
                                .child(icon(STOP_ICON, 15.0).text_color(paper()))
                                .into_element()
                        } else {
                            view()
                                .size(px(32.0))
                                .rounded_full()
                                .items_center()
                                .justify_center()
                                .bg(ink())
                                .opacity(move || if can_send() { 1.0_f32 } else { 0.2_f32 })
                                .cursor(Cursor::Pointer)
                                .on_press(move || send_on_press())
                                .label("Send")
                                .child(icon(SEND_ICON, 15.0).text_color(paper()))
                                .into_element()
                        }
                    })),
            ),
    )
}
