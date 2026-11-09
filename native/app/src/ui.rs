//! Pieces every screen uses: icons, buttons, the conversation and its
//! composer, styled as the Electron Bubble app's.

use std::rc::Rc;

use bubble_core::Decision;
use bubble_store::{MessageRole, TaskState};
use nsh_base::text_view::TextViewClasses;
use nsh_core::element::ImageSrc;
use nsh_core::prelude::*;

use crate::palette::*;
use crate::state::{AppState, Conversation, Line, LineKind, ToolStatus};

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
            root: Some("text-[13px] leading-[22px] text-[#1A1A1A]".into()),
            heading: Some("text-[15px] font-semibold".into()),
            ..Default::default()
        })
        .into_element()
}

/// The model's reasoning, folded: "Thought it through" opens it.
fn reasoning_toggle(reasoning: String) -> AnyElement {
    let open = signal(false);
    view()
        .gap(px(6.0))
        .child(
            text(move || if open.get() { "Hide reasoning" } else { "Show reasoning" })
                .text_size(px(SMALL))
                .text_color(muted())
                .cursor(Cursor::Pointer)
                .on_press(move || open.update(|o| *o = !*o)),
        )
        .child(when(move || open.get(), move || {
            text(reasoning.clone()).text_size(px(SMALL)).line_height(px(18.0)).text_color(muted())
        }))
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
    conversation.on_change(move || list.scroll_to_end(true));
    let empty = move || conversation.lines.with(|lines| lines.is_empty());
    scroll().flex_1().element_ref(&list).child(
        view()
            .w_full()
            .max_w(px(READING_WIDTH))
            .mx(auto())
            .px(px(24.0))
            .pt(px(28.0))
            .pb(px(16.0))
            .gap(px(18.0))
            .child(when(empty, move || {
                view()
                    .pt(px(120.0))
                    .items_center()
                    .gap(px(8.0))
                    .child(text(empty_title).text_size(px(HEADING)).font_weight(FontWeight::SEMIBOLD))
                    .child(text(empty_hint).text_size(px(BODY)).text_color(muted()))
            }))
            .child(each(
                move || conversation.lines.get(),
                |line: &Line| line.id.clone(),
                move |line, _| line_view(app, conversation, line),
            ))
            .child(subagents_view(conversation)),
    )
}

/// The children a task's agent started: who, doing what, and how it went;
/// a finished one opens to its answer.
fn subagents_view(conversation: Conversation) -> impl IntoElement {
    use bubble_core::delegate::{Snapshot, Status};
    let visible = move || conversation.subagents.with(|s| s.iter().any(|s| s.status != Status::Closed));
    when(visible, move || {
        view()
            .gap(px(6.0))
            .child(text("Subagents").text_size(px(SMALL)).text_color(muted()))
            .child(each(
                move || conversation.subagents.get().into_iter().filter(|s| s.status != Status::Closed).collect::<Vec<_>>(),
                |child: &Snapshot| child.id.clone(),
                |child, _| {
                    let open = signal(false);
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
                                .on_press(move || open.update(|o| *o = !*o))
                                .label(move || format!("Subagent {}", child.get().nickname))
                                .child(text(move || child.get().nickname).text_size(px(BODY)).text_color(ink()))
                                .child(text(move || child.get().profile).text_size(px(SMALL)).text_color(muted()))
                                .child(text(title).flex_1().min_w(px(0.0)).number_of_lines(1).text_size(px(SMALL)).text_color(secondary()))
                                .child(text(move || child.get().status.label()).text_size(px(SMALL)).text_color(status_color)),
                        )
                        .child(when(move || open.get(), move || {
                            // Follows the child: its latest step, then its answer.
                            dynamic(move || {
                                let c = child.get();
                                if !c.summary.is_empty() && c.status.is_final() {
                                    markdown(c.summary.clone()).into_element()
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

fn line_view(app: AppState, conversation: Conversation, line: ReadSignal<Line>) -> AnyElement {
    match line.get_untracked().kind {
        LineKind::Tool { name, .. } if name == "create_task" || name == "continue_task" => task_card_line(app, line),
        LineKind::Tool { .. } => tool_line(line),
        LineKind::Approval { .. } => approval_card(app, conversation, line),
        LineKind::Message(MessageRole::User) => view()
            .self_end()
            .max_w(percentage(0.8))
            .bg(user_bubble())
            .rounded(px(10.0))
            .px(px(20.0))
            .py(px(10.0))
            .child(text(move || line.get().text).text_size(px(BODY)).line_height(px(21.0)).text_color(ink()))
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
                text("Queued until another task finishes").text_size(px(BODY)).text_color(muted()).into_element()
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
                view()
                    .gap(px(6.0))
                    .child((!current.reasoning.trim().is_empty()).then(|| reasoning_toggle(current.reasoning.clone())))
                    .child((!current.text.trim().is_empty()).then(|| markdown(current.text.clone())))
                    .into_element()
            }
        }),
        LineKind::Message(MessageRole::Tool | MessageRole::Checkpoint) => view().into_element(),
        LineKind::Message(MessageRole::External) => view()
            .gap(px(6.0))
            .child(text("From GitHub").text_size(px(SMALL)).text_color(muted()))
            .child(
                view()
                    .px(px(16.0))
                    .py(px(10.0))
                    .rounded(px(10.0))
                    .bg(tertiary())
                    .child(text(move || line.get().text).text_size(px(BODY)).line_height(px(21.0))),
            )
            .into_element(),
        LineKind::Message(MessageRole::System) => view()
            .gap(px(6.0))
            .child(text("From Manager").text_size(px(SMALL)).text_color(muted()))
            .child(
                view()
                    .px(px(16.0))
                    .py(px(10.0))
                    .rounded(px(10.0))
                    .bg(tertiary())
                    .child(text(move || line.get().text).text_size(px(BODY)).line_height(px(21.0))),
            )
            .into_element(),
        LineKind::RunStart(index) => view()
            .pt(px(8.0))
            .gap(px(8.0))
            .child(text(format!("Run {index}")).text_size(px(SMALL)).text_color(muted()))
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

const MARK_DONE: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><path d="M4 8.3l2.6 2.6L12 5.5" stroke="#8F8F8F" stroke-width="1.6" fill="none" stroke-linecap="round" stroke-linejoin="round"/></svg>"##;
const MARK_FAILED: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><path d="M5 5l6 6m0-6l-6 6" stroke="#DC2626" stroke-width="1.6" fill="none" stroke-linecap="round"/></svg>"##;
const MARK_DENIED: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><circle cx="8" cy="8" r="5" stroke="#8F8F8F" stroke-width="1.4" fill="none"/><path d="M4.6 11.4l6.8-6.8" stroke="#8F8F8F" stroke-width="1.4"/></svg>"##;
const MARK_RUNNING: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><circle cx="4" cy="8" r="1.3" fill="#8F8F8F"/><circle cx="8" cy="8" r="1.3" fill="#8F8F8F"/><circle cx="12" cy="8" r="1.3" fill="#8F8F8F"/></svg>"##;

/// How much of a tool's output a row shows when opened.
const OUTPUT_SHOWN: usize = 4000;

/// A tool call: one quiet line; pressing it shows what the tool gave back.
fn tool_line(line: ReadSignal<Line>) -> AnyElement {
    let open = signal(false);
    let status = move || match line.get().kind {
        LineKind::Tool { status, .. } => status,
        _ => ToolStatus::Done,
    };
    let output = move || match line.get().kind {
        LineKind::Tool { output, .. } => output,
        _ => String::new(),
    };
    view()
        .gap(px(6.0))
        .child(
            view()
                .flex_row()
                .items_center()
                .gap(px(6.0))
                .cursor(Cursor::Pointer)
                .on_press(move || open.update(|o| *o = !*o))
                .child(
                    svg(move || {
                        ImageSrc::svg_markup(match status() {
                            ToolStatus::Running => MARK_RUNNING,
                            ToolStatus::Done => MARK_DONE,
                            ToolStatus::Failed => MARK_FAILED,
                            ToolStatus::Denied => MARK_DENIED,
                        })
                    })
                    .size(px(14.0)),
                )
                .child(
                    text(move || line.get().text)
                        .flex_1()
                        .text_size(px(BODY))
                        .text_color(secondary())
                        .number_of_lines(1),
                ),
        )
        .child(when(move || open.get() && !output().is_empty(), move || {
            view()
                .ml(px(20.0))
                .px(px(12.0))
                .py(px(8.0))
                .rounded(px(8.0))
                .bg(tertiary())
                .child(
                    text(move || {
                        let text = output();
                        let shown: String = text.chars().take(OUTPUT_SHOWN).collect();
                        if shown.len() < text.len() { format!("{shown}\n…") } else { shown }
                    })
                    .font_family("Menlo")
                    .text_size(px(SMALL))
                    .line_height(px(18.0))
                    .text_color(secondary()),
                )
        }))
        .into_element()
}

/// The Manager started or followed up on a task: a card that follows the
/// task's state and opens it.
fn task_card_line(app: AppState, line: ReadSignal<Line>) -> AnyElement {
    let task_id = move || match line.get().kind {
        LineKind::Tool { status: ToolStatus::Done, output, .. } => crate::manager::task_id_in(&output),
        _ => None,
    };
    dynamic(move || {
        let Some(id) = task_id() else { return tool_line(line) };
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
                Decision::Deny | Decision::Refuse(_) => "Denied",
            })
            .text_size(px(SMALL))
            .text_color(muted())
            .into_element(),
        }))
        .into_element()
}

const STOP_ICON: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><rect x="4.5" y="4.5" width="7" height="7" rx="1.5" fill="#FFFFFF"/></svg>"##;
const SEND_ICON: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><path d="M8 13V3M3.5 7.5L8 3l4.5 4.5" stroke="#FFFFFF" stroke-width="1.75" fill="none" stroke-linecap="round" stroke-linejoin="round"/></svg>"##;

/// The message box under the conversation: Return sends.
pub fn composer(
    app: AppState,
    conversation: Conversation,
    placeholder: &'static str,
    model: impl Fn() -> String + 'static,
) -> impl IntoElement {
    let draft = signal(String::new());
    let send: Rc<dyn Fn()> = Rc::new(move || {
        let text = draft.get_untracked();
        if text.trim().is_empty() || conversation.busy.get_untracked() {
            return;
        }
        draft.set(String::new());
        conversation.send(app, text);
    });
    let send_on_submit = send.clone();
    let send_on_press = send.clone();
    let can_send = move || !conversation.busy.get() && !draft.with(|d| d.trim().is_empty());
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
            .shadow_color(rgba(0x0F17_2A14))
            .child(
                nsh_base::input()
                    .value(draft)
                    .placeholder(placeholder.to_owned())
                    .placeholder_color(muted())
                    .multiline(true)
                    .submit_behavior(SubmitBehavior::Submit)
                    .auto_focus(true)
                    .min_h(px(46.0))
                    .px(px(18.0))
                    .pt(px(14.0))
                    .text_size(px(BODY))
                    .text_color(ink())
                    .on_change(move |value: &str| draft.set(value.to_owned()))
                    .on_submit(move |_: &str| send_on_submit()),
            )
            .child(
                view()
                    .flex_row()
                    .items_center()
                    .justify_end()
                    .gap(px(10.0))
                    .pl(px(14.0))
                    .pr(px(10.0))
                    .pb(px(10.0))
                    .child(text(model).text_size(px(BODY)).text_color(secondary()))
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
                                .child(icon(STOP_ICON, 15.0))
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
                                .child(icon(SEND_ICON, 15.0))
                                .into_element()
                        }
                    })),
            ),
    )
}
