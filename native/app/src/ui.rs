//! Pieces every screen uses: icons, buttons, the conversation and its
//! composer, styled as the Electron Bubble app's.

use std::rc::Rc;

use bubble_core::Decision;
use bubble_store::{MessageRole, TaskState};
use nsh_base::text_view::TextViewClasses;
use nsh_core::element::ImageSrc;
use nsh_core::prelude::*;

use crate::palette::*;
use crate::state::{AppState, Conversation, Line, LineKind};
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

/// How the conversation sets a document's parts.
fn reading_classes() -> TextViewClasses {
    TextViewClasses {
        root: Some("text-[13px] leading-[22px] text-[#1A1A1A] dark:text-[#ECECEC]".into()),
        heading: Some("text-[15px] font-semibold".into()),
        // An item is as wide as what it says, not what is left of its row:
        // a bubble sized by its content then makes room for its lists too.
        list_item_content: Some("flex-auto".into()),
        ..Default::default()
    }
}

/// The same, on the dark bubble of what was said to a bot (light in dark).
fn reading_classes_on_ink() -> TextViewClasses {
    TextViewClasses {
        root: Some("text-[13px] leading-[22px] text-[#FFFFFF] dark:text-[#1A1A1A]".into()),
        code: Some("bg-[#FFFFFF26] text-[#FFFFFF] dark:bg-[#0000001A] dark:text-[#1A1A1A]".into()),
        code_block: Some("bg-[#FFFFFF1F] dark:bg-[#0000000F]".into()),
        code_block_code: Some("text-[#FFFFFF] dark:text-[#1A1A1A]".into()),
        link: Some("text-[#FFFFFF] dark:text-[#1A1A1A] underline".into()),
        blockquote: Some("text-[#FFFFFFCC] dark:text-[#1A1A1ACC]".into()),
        ..reading_classes()
    }
}

/// Whether some block of `blocks` takes the width it is given and asks for
/// none of its own: an alert's and a folded section's text sit in what is
/// left of a row.
fn takes_given_width(blocks: &[nsh_base::text_view::Block]) -> bool {
    use nsh_base::text_view::Block;
    blocks.iter().any(|block| match block {
        Block::Blockquote { alert: Some(_), .. } | Block::Details { .. } => true,
        Block::Blockquote { children, .. } => takes_given_width(children),
        Block::List { items, .. } => items.iter().any(|item| takes_given_width(&item.children)),
        _ => false,
    })
}

/// What a bot said, in its bubble: as wide as what it says, up to the
/// column. A reply with a block that asks for no width of its own, or with
/// footnotes (set the same way), gets the whole column instead, since a
/// bubble sized by such content would be sized without it.
fn reply_bubble(source: &str) -> AnyElement {
    let document = nsh_base::text_view::Parser::new().markdown(source);
    let fits = !takes_given_width(&document.blocks) && document.footnotes.is_empty();
    let bubble = view().px(px(14.0)).py(px(10.0)).rounded(px(18.0)).bg(bot_bubble());
    let bubble = if fits { bubble.self_start().max_w(percentage(1.0)) } else { bubble.w_full() };
    bubble
        .child(nsh_base::text_view::text_view().document(document).selectable(true).classes(reading_classes()))
        .into_element()
}

/// A mark in passing that this bot and another exchanged a message: a small
/// line in the middle of the conversation with the other bot's face and
/// name ("Message from", "Messaged"). What was said is in the other bot's
/// conversation, and a press on the mark goes there.
fn bot_mark(app: AppState, verb: &str, name: String) -> AnyElement {
    let verb = verb.to_owned();
    // Where the other bot's conversation is: the main bot's is home.
    let (face, there) = if name == crate::manager::MAIN_BOT {
        (Some(crate::avatar::main(16.0).into_element()), Some(crate::state::Route::Home))
    } else {
        match crate::manager::find(app, &name) {
            Ok(bot) => (Some(crate::avatar::bot(app, &bot.task.id, 16.0).into_element()), Some(crate::state::Route::Task(bot.task.id))),
            // A bot that is gone: its name, and nowhere to go.
            Err(_) => (None, None),
        }
    };
    let mark = view()
        .self_center()
        .flex_row()
        .items_center()
        .gap(px(6.0))
        .child(text(verb.clone()).text_size(px(SMALL)).text_color(muted()))
        .child(face)
        .child(text(name.clone()).text_size(px(SMALL)).text_color(secondary()).number_of_lines(1));
    match there {
        Some(route) => mark
            .cursor(Cursor::Pointer)
            .label(format!("{verb} {name}"))
            .on_press(move || {
                if let crate::state::Route::Task(id) = &route {
                    app.mark_seen(id);
                }
                app.route.set(route.clone());
            })
            .into_element(),
        None => mark.into_element(),
    }
}

/// Something told to the person in the main bot's conversation. A bot's
/// report on work nobody asked for there: whose it is and what of, then
/// what the bot said. Or work that waits for their yes: the main bot asks,
/// with the two answers under it until one is given.
fn told(app: AppState, line: ReadSignal<Line>) -> AnyElement {
    use bubble_store::{TicketOrigin, TicketStatus};
    let (id, words) = line.with_untracked(|line| (line.id.clone(), line.text.clone()));
    let bot_of = move |ticket: &bubble_store::Ticket| {
        app.tasks.with(|tasks| tasks.iter().find(|t| t.task.id == ticket.task_id).map(|t| t.task.title.clone())).unwrap_or_else(|| "a bot".to_owned())
    };
    match app.told_from(&id) {
        Some(name) => {
            let verb = match untracked(|| app.told_ticket(&id)) {
                Some(ticket) => {
                    let what = match (ticket.origin, &ticket.asker) {
                        (TicketOrigin::Github, Some(asker)) => asker.clone(),
                        _ => "A routine".to_owned(),
                    };
                    match ticket.status {
                        TicketStatus::Failed => format!("{what} failed, says"),
                        TicketStatus::Stopped => format!("{what} stopped, says"),
                        _ => format!("{what} done by"),
                    }
                }
                None => "Message from".to_owned(),
            };
            view().gap(px(8.0)).child(bot_mark(app, &verb, name)).child(reply_bubble(&words)).into_element()
        }
        None => view()
            .gap(px(8.0))
            .child(reply_bubble(&words))
            .child(dynamic(move || {
                let ticket = app.told_ticket(&id)?;
                let bot = bot_of(&ticket);
                let (accept, dismiss, title) = (ticket.id.clone(), ticket.id.clone(), ticket.title.clone());
                Some(match ticket.status {
                    TicketStatus::Proposed => view()
                        .flex_row()
                        .gap(px(8.0))
                        .child(primary_button(&format!("Give to {bot}"), move || crate::manager::accept_ticket(app, &accept)).label(format!("Give: {title}")))
                        .child(button("Dismiss", move || crate::manager::dismiss_ticket(app, &dismiss)).label(format!("Dismiss: {title}")))
                        .into_element(),
                    TicketStatus::Dismissed => text("Dismissed").pl(px(4.0)).text_size(px(SMALL)).text_color(muted()).into_element(),
                    _ => text(format!("Given to {bot}")).pl(px(4.0)).text_size(px(SMALL)).text_color(muted()).into_element(),
                })
            }))
            .into_element(),
    }
}

/// From this many characters on, a message to a bot shows only its beginning
/// until the person opens it, this many points of it.
const LONG_MESSAGE: usize = 700;
const LONG_MESSAGE_SHOWN: f32 = 176.0;

/// What someone other than the person said to this bot (the main bot,
/// another bot, GitHub, a routine): where the person's own words would be
/// and looking as they do, with who said it over it. Whoever a bot is
/// answering, its conversation reads the same way.
fn said_to_bot(from: impl Fn() -> String + 'static, line: ReadSignal<Line>) -> AnyElement {
    let document = nsh_base::text_view::Parser::new().markdown(&line.get_untracked().text);
    let fits = !takes_given_width(&document.blocks) && document.footnotes.is_empty();
    let bubble = view().bg(user_bubble()).rounded(px(18.0)).px(px(16.0)).py(px(9.0));
    // As wide as what it says, and no wider than its place: without the
    // limit on the bubble itself a long message is laid out on one line.
    let bubble = if fits { bubble.max_w(percentage(1.0)) } else { bubble.w_full() };
    // A long one (a bot's whole report) shows its beginning: the person
    // reads what their bot makes of it, and opens it when they want it all.
    let long = line.get_untracked().text.chars().count() > LONG_MESSAGE;
    let open = signal(!long);
    let words = view()
        .style_when(move || !open.get(), |style| style.max_h(px(LONG_MESSAGE_SHOWN)).overflow_hidden())
        .child(nsh_base::text_view::text_view().document(document).selectable(true).classes(reading_classes_on_ink()));
    view()
        .child(stamp_line(line))
        .child(
            view()
                .self_end()
                .max_w(percentage(0.8))
                .items_end()
                .gap(px(4.0))
                .child(text(from).text_size(px(SMALL)).text_color(muted()))
                .child(bubble.child(words))
                .child(long.then(|| {
                    text(move || if open.get() { "Show less" } else { "Show all" })
                        .text_size(px(SMALL))
                        .text_color(secondary())
                        .cursor(Cursor::Pointer)
                        .on_press(move || open.update(|open| *open = !*open))
                })),
        )
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
                move |block, _| block_view(app, conversation, block),
            )),
    )).child(when(move || !following.get(), move || {
        // Over the end of the conversation, in the middle: a small round
        // button with an arrow down, which takes no room of its own.
        view()
            .absolute()
            .bottom(px(8.0))
            .left(percentage(0.5))
            .ml(px(-14.0))
            .size(px(28.0))
            .items_center()
            .justify_center()
            .rounded_full()
            .bg(paper())
            .border(px(LINE))
            .border_color(border())
            .shadow_md()
            .shadow_color(shadow())
            .cursor(Cursor::Pointer)
            .hover(|s| s.bg(tertiary()))
            .label("Jump to latest")
            .test_id("jump-to-latest")
            .on_press(move || {
                following.set(true);
                list.scroll_to_end(true);
            })
            .child(icon(DOWN_ICON, 14.0).text_color(secondary()))
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

/// One thing the conversation shows.
fn block_view(app: AppState, conversation: Conversation, block: ReadSignal<Block>) -> AnyElement {
    let (content, gap) = match block.get_untracked() {
        Block::Status { .. } => (activity_row(app, conversation, block).into_element(), 14.0),
        Block::Ended { status, .. } => (text(crate::trace::ended(status)).text_size(px(SMALL)).text_color(muted()).into_element(), 10.0),
        Block::Line { id, close } => {
            // The line itself, kept live as it streams or finishes.
            let line = each(
                move || conversation.lines.with(|lines| lines.iter().filter(|l| l.id == id).cloned().collect::<Vec<_>>()),
                |line: &Line| line.id.clone(),
                move |line, _| line_view(app, conversation, line),
            );
            // What a bot says after something it said sits close to it.
            (line.into_element(), if close { 6.0 } else { 18.0 })
        }
    };
    // A keyed transcript item must own a real native sibling. Returning a
    // bare If/Each region lets late descendant updates attach after the
    // next row (notably a reply followed by a new prompt).
    view().w_full().pt(px(gap)).test_id(&format!("trace-row:{}", block.get_untracked().id())).child(content).into_element()
}

/// Words with a light passing over them, a letter at a time: the sign
/// that a bot is at work. Still, and dimmer, while it waits.
pub fn shimmer(phrase: String, waiting: bool) -> AnyElement {
    if waiting {
        return text(phrase).text_size(px(BODY)).text_color(muted()).into_element();
    }
    // The light starts before the first letter and leaves after the last.
    const REACH: f32 = 0.35;
    let light = animated_value(-REACH);
    let _ = Animation::timing(&light, 1.0 + REACH, Timing::new(2200.0, Easing::Linear)).looped(None).start();
    // Each letter with whether a space comes before it: a space set by
    // itself takes no room, so it is the next letter's margin.
    let mut letters: Vec<(char, bool)> = Vec::new();
    let mut spaced = false;
    for letter in phrase.chars() {
        if letter.is_whitespace() {
            spaced = !letters.is_empty();
        } else {
            letters.push((letter, spaced));
            spaced = false;
        }
    }
    let last = letters.len().saturating_sub(1).max(1) as f32;
    view()
        .flex_row()
        // One phrase to a screen reader, not a row of letters.
        .accessible(true)
        .label(phrase.clone())
        .accessibility_elements_hidden(true)
        .children(letters.into_iter().enumerate().map(|(index, (letter, spaced))| {
            let at = index as f32 / last;
            let brightness = light.interpolate(&[at - REACH, at, at + REACH], &[0.4, 1.0, 0.4]);
            text(letter.to_string())
                .ml(px(if spaced { BODY * 0.3 } else { 0.0 }))
                .text_size(px(BODY))
                .text_color(ink())
                .bind_opacity(&brightness)
        }))
        .into_element()
}

/// Three dots that rise one after another: someone is writing back.
fn typing_dots() -> impl IntoElement {
    let beat = animated_value(0.0);
    let _ = Animation::timing(&beat, 1.0, Timing::new(1400.0, Easing::Linear)).looped(None).start();
    view()
        .flex_row()
        .items_center()
        .gap(px(4.0))
        .h(px(30.0))
        .px(px(12.0))
        .rounded(px(15.0))
        .bg(bot_bubble())
        .accessible(true)
        .label("Typing")
        // Each a little behind the one before it.
        .children([0.229_f32, 0.114, 0.0].map(|lead| {
            let at = beat.add(lead).modulo(1.0);
            let rise = at.interpolate(&[0.0, 0.3, 0.6, 1.0], &[0.0, -2.0, 0.0, 0.0]);
            let glow = at.interpolate(&[0.0, 0.3, 0.6, 1.0], &[0.35, 1.0, 0.35, 0.35]);
            view().size(px(6.0)).rounded_full().bg(secondary()).bind_translate_y(&rise).bind_opacity(&glow)
        }))
}

/// What the bot is doing now, at the end of its conversation while a turn
/// runs: its face, a few words, and how long it has been at it.
fn activity_row(app: AppState, conversation: Conversation, block: ReadSignal<Block>) -> impl IntoElement {
    let now = signal(bubble_store::now_ms());
    set_interval(std::time::Duration::from_secs(10), move || now.set(bubble_store::now_ms()));
    // A memo: the words change with the step, not with every word the
    // bot writes or command line it prints, so the light is not restarted.
    let doing = computed(move || match block.get() {
        Block::Status { turn, status, .. } => conversation.lines.with(|lines| crate::trace::activity(lines, &turn, status)),
        _ => crate::trace::Activity { phrase: String::new(), mood: crate::avatar::Mood::Waiting },
    });
    // Its eyes follow what it does.
    let mood = move || doing.get().mood;
    let face = match conversation.task_id.get() {
        Some(id) => crate::avatar::bot_doing(app, &id, 28.0, mood).into_element(),
        None => crate::avatar::main_doing(app, 28.0, mood).into_element(),
    };
    view()
        .flex_row()
        .items_center()
        .gap(px(10.0))
        .test_id("activity")
        .child(face)
        .child(dynamic(move || {
            let doing = doing.get();
            // Writing back shows as dots, as a messenger shows it.
            if doing.mood == crate::avatar::Mood::Typing { typing_dots().into_element() } else { shimmer(doing.phrase.clone(), doing.waiting()) }
        }))
        .child(
            text(move || match block.get() {
                Block::Status { started_at, .. } => crate::trace::elapsed(now.get() - started_at).unwrap_or_default(),
                _ => String::new(),
            })
            .text_size(px(SMALL))
            .text_color(muted()),
        )
}

fn line_view(app: AppState, conversation: Conversation, line: ReadSignal<Line>) -> AnyElement {
    match line.get_untracked().kind {
        // That it wrote to another bot, or made one: a mark in passing.
        LineKind::Tool { ref name, .. } if crate::trace::went_to_a_bot(name) => {
            let said = line.get_untracked().text;
            match (said.strip_prefix("Messaged "), said.strip_prefix("Made a bot: ")) {
                (Some(bot), _) if bot != "a bot" => bot_mark(app, "Messaged", bot.to_owned()),
                (_, Some(bot)) if !bot.is_empty() => bot_mark(app, "Made", bot.to_owned()),
                _ => view().into_element(),
            }
        }
        // How a bot got there is not the conversation's to show.
        LineKind::Tool { .. } | LineKind::Reasoning | LineKind::Turn { .. } | LineKind::RunStart(_) => view().into_element(),
        LineKind::Question { .. } => question_card(app, conversation, line),
        // Asked and not answered yet: the card. Once decided it goes.
        LineKind::Approval { .. } => {
            let undecided = move || matches!(line.get().kind, LineKind::Approval { decided: None, .. });
            view().child(when(undecided, move || approval_card(app, conversation, line))).into_element()
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
                    .child(text(move || line.get().text).text_size(px(BODY)).line_height(px(21.0)).text_color(user_bubble_text()))
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
        // What the bot says is a bubble; while it has said nothing yet,
        // the row at the end of the conversation speaks for it.
        LineKind::Message(MessageRole::Assistant) => dynamic(move || {
            let current = line.get();
            view().child((!current.text.trim().is_empty()).then(|| reply_bubble(&current.text))).into_element()
        }),
        LineKind::Message(MessageRole::Tool | MessageRole::Checkpoint) => view().into_element(),
        LineKind::Message(MessageRole::Told) => told(app, line),
        LineKind::Message(MessageRole::External) => said_to_bot(|| "From GitHub".to_owned(), line),
        // A bot's answer coming back is a mark in passing: this bot says
        // what came of it, and the whole of it is in that bot's
        // conversation, a press away.
        LineKind::Message(MessageRole::System) if app.is_reply(&line.get_untracked().id) => {
            bot_mark(app, "Message from", app.sender_label(&line.get_untracked().id))
        }
        LineKind::Message(MessageRole::System) => said_to_bot(move || format!("From {}", app.sender_label(&line.get().id)), line),
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

/// The agent waits for a yes: what it wants to do, and three answers.
/// What the agent asks the person: each ask with its choices. One ask with
/// one choice to make is answered by picking it; more than that is picked
/// and then sent.
fn question_card(app: AppState, conversation: Conversation, line: ReadSignal<Line>) -> AnyElement {
    let (asks, id) = match line.get_untracked() {
        Line { kind: LineKind::Question { asks, .. }, id, .. } => (asks, id),
        _ => return view().into_element(),
    };
    let answered = move || match line.get().kind {
        LineKind::Question { answers, .. } => answers,
        _ => None,
    };
    let at_once = asks.len() == 1 && !asks[0].multiple;
    // What is picked so far, per ask.
    let picked = signal(vec![Vec::<String>::new(); asks.len()]);
    let complete = move || picked.with(|all| all.iter().all(|one| !one.is_empty()));
    let send_id = stored(id);
    let waiting = move || answered().is_none();
    view()
        .gap(px(12.0))
        .p(px(14.0))
        .rounded(px(12.0))
        .border(px(LINE))
        .border_color(border())
        .bg(pane())
        .children(
            asks.iter()
                .cloned()
                .enumerate()
                .map(|(index, ask)| {
                    let multiple = ask.multiple;
                    view()
                        .gap(px(8.0))
                        .child(when(
                            {
                                let shown = !ask.header.trim().is_empty();
                                move || shown
                            },
                            {
                                let header = ask.header.clone();
                                move || text(header.clone()).text_size(px(SMALL)).text_color(muted())
                            },
                        ))
                        .child(text(ask.question.clone()).text_size(px(BODY)).font_weight(FontWeight::SEMIBOLD).line_height(px(20.0)))
                        .child(view().gap(px(6.0)).children(ask.options.iter().cloned().map(move |(label, description)| {
                            let chosen = {
                                let label = label.clone();
                                move || match answered() {
                                    Some(given) => given.get(index).is_some_and(|g| g.contains(&label)),
                                    None => picked.with(|all| all[index].contains(&label)),
                                }
                            };
                            let press = label.clone();
                            let is_chosen = chosen.clone();
                            view()
                                .px(px(12.0))
                                .py(px(8.0))
                                .gap(px(2.0))
                                .rounded(px(8.0))
                                .border(px(LINE))
                                .border_color(move || if is_chosen() { ink() } else { border() })
                                .bg(move || if chosen() { tertiary() } else { pane() })
                                .cursor(Cursor::Pointer)
                                .hover(|s| s.bg(tertiary()))
                                .label(format!("Answer {label}"))
                                .on_press(move || {
                                    if !waiting() {
                                        return;
                                    }
                                    if at_once {
                                        conversation.answer_question(app, &send_id.get(), Some(vec![vec![press.clone()]]));
                                        return;
                                    }
                                    picked.update(|all| {
                                        let mine = &mut all[index];
                                        if let Some(at) = mine.iter().position(|l| l == &press) {
                                            mine.remove(at);
                                        } else {
                                            if !multiple {
                                                mine.clear();
                                            }
                                            mine.push(press.clone());
                                        }
                                    });
                                })
                                .child(text(label).text_size(px(BODY)).text_color(ink()))
                                .child(when(
                                    {
                                        let shown = !description.trim().is_empty();
                                        move || shown
                                    },
                                    move || text(description.clone()).text_size(px(SMALL)).text_color(muted()),
                                ))
                        })))
                })
                .collect::<Vec<_>>(),
        )
        .child(dynamic(move || match answered() {
            None => view()
                .flex_row()
                .gap(px(8.0))
                .child(when(move || !at_once, move || {
                    primary_button("Send answer", move || {
                        if complete() {
                            conversation.answer_question(app, &send_id.get(), Some(picked.get_untracked()));
                        }
                    })
                    .opacity(move || if complete() { 1.0_f32 } else { 0.45_f32 })
                }))
                .child(button("Skip", move || conversation.answer_question(app, &send_id.get(), None)))
                .into_element(),
            Some(given) if given.iter().all(Vec::is_empty) => text("Not answered").text_size(px(SMALL)).text_color(muted()).into_element(),
            Some(_) => text("Answered").text_size(px(SMALL)).text_color(muted()).into_element(),
        }))
        .into_element()
}

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

const DOWN_ICON: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><path d="M8 3v10M3.5 8.5L8 13l4.5-4.5" stroke="currentColor" stroke-width="1.5" fill="none" stroke-linecap="round" stroke-linejoin="round"/></svg>"##;
const STOP_ICON: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><rect x="4.5" y="4.5" width="7" height="7" rx="1.5" fill="currentColor"/></svg>"##;
const SEND_ICON: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><path d="M8 13V3M3.5 7.5L8 3l4.5 4.5" stroke="currentColor" stroke-width="1.75" fill="none" stroke-linecap="round" stroke-linejoin="round"/></svg>"##;

/// Whose composer it is: the Manager's, or a bot's (with its project's
/// files to mention). Which model answers is set in Settings, and for one
/// bot in its details.
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
/// Where a bot works for what the person writes next: a known project,
/// another folder, or none. Work others send it names its own.
fn project_picker(app: AppState, bot: String, files: Signal<Option<Vec<String>>>) -> impl IntoElement {
    let id = stored(bot);
    let current = move || {
        app.task_revision.get();
        app.tasks.with(|tasks| tasks.iter().find(|t| t.task.id == id.get()).and_then(|t| t.task.project_cwd.clone()))
    };
    let choose = move |folder: Option<String>| {
        app.set_bot_project(&id.get(), folder.as_deref());
        // The files to mention are the new folder's.
        files.set(None);
    };
    composer_menu(
        "Project",
        move || current().map(|p| project_name(&p)).unwrap_or_else(|| "No folder".to_owned()),
        move || {
            let now = current();
            let mut options: Vec<(String, bool, Rc<dyn Fn()>)> = app
                .projects()
                .into_iter()
                .filter(|p| std::path::Path::new(p).is_dir())
                .take(8)
                .map(|p| {
                    let chosen = now.as_deref() == Some(p.as_str());
                    let folder = p.clone();
                    (project_name(&p), chosen, Rc::new(move || choose(Some(folder.clone()))) as Rc<dyn Fn()>)
                })
                .collect();
            options.push((
                "Choose a folder…".to_owned(),
                false,
                Rc::new(move || {
                    let picked = pick_paths(nsh_core::file_dialog::OpenDialog::folders().button_label("Work here"));
                    spawn(async move {
                        if let Ok(Some(paths)) = picked.await {
                            if let Some(path) = paths.into_iter().next() {
                                choose(Some(path.display().to_string()));
                            }
                        }
                    });
                }),
            ));
            options.push(("No folder".to_owned(), now.is_none(), Rc::new(move || choose(None))));
            options
        },
    )
}

fn composer_menu(
    label: &'static str,
    shown: impl Fn() -> String + 'static,
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
                        .text_color(secondary())
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

/// Where the person writes: text, `@file` mentions of the task's project,
/// pictures (pasted or attached), and Up and Down for what they sent before.
pub fn composer(
    app: AppState,
    conversation: Conversation,
    placeholder: Rc<dyn Fn() -> String>,
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
    // Where the bot works now: it can change between messages, with the
    // work or by the person's choice here.
    let bot = stored(match &target {
        ComposerFor::Task(id) => Some(id.clone()),
        ComposerFor::Manager => None,
    });
    let project = move || {
        bot.get().and_then(|id| app.bot_project(&id)).filter(|p| std::path::Path::new(p).is_dir())
    };
    // The composer is one line, with its buttons on that line, until what
    // is written takes more than one; then the words get its whole width
    // and the buttons a row of their own under them. It goes back to one
    // line when it is empty again.
    let expanded = signal(false);
    // It grows with what is written up to this height, and only then
    // scrolls: until then there is nothing to scroll, so no bar.
    const TALLEST: f32 = 200.0;
    let full = signal(false);
    let settle = move || {
        use nsh_core::{LayoutAnimation, LayoutCurve, LayoutTransition};
        animate_layout(LayoutAnimation {
            create: Some(LayoutTransition::new(LayoutCurve::EaseOut, 160.0)),
            update: Some(LayoutTransition::new(LayoutCurve::Spring { damping: 0.82 }, 300.0)),
            delete: None,
        })
    };
    let add_image = move |result: Result<Image, String>| match result {
        Ok(image) => {
            problem.set(None);
            settle();
            images.update(|list| list.push(image));
        }
        Err(message) => problem.set(Some(message)),
    };
    let send: Rc<dyn Fn()> = Rc::new(move || {
        let text = draft.get_untracked();
        let attached = images.get_untracked();
        if (text.trim().is_empty() && attached.is_empty()) || conversation.busy.get_untracked() {
            return;
        }
        let expanded = project()
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
    let on_change = move |value: &str| {
        draft.set(value.to_owned());
        if value.contains('@') && files.with_untracked(Option::is_none) {
            if let Some(cwd) = project() {
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
    let tall = computed(move || {
        expanded.get() || images.with(|i| !i.is_empty()) || !suggestions().is_empty() || problem.with(Option::is_some)
    });
    // Whatever changes the draft (typing, sending, Up and Down, a mention
    // taken), its shape follows, moving rather than jumping.
    let lines = stored(0usize);
    effect(move || {
        let (empty, breaks) = draft.with(|d| (d.is_empty(), d.matches('\n').count()));
        let next = if empty { false } else { breaks > 0 || expanded.get_untracked() };
        if next != expanded.get_untracked() || breaks != lines.get() {
            settle();
        }
        lines.with(|lines| *lines = breaks);
        expanded.set(next);
    });
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
    let attach_button = move || {
        view()
            .size(px(28.0))
            .flex_shrink_0()
            .rounded_full()
            .items_center()
            .justify_center()
            .cursor(Cursor::Pointer)
            .hover(|s| s.bg(tertiary()))
            .label("Attach images")
            .on_press(attach)
            .child(icon(ATTACH_ICON, 16.0).text_color(secondary()))
    };
    // Where the bot works, and send (or stop, while it is at work).
    let actions = Rc::new(move || {
        let send_on_press = send_on_press.clone();
        view().flex_row().items_center().flex_shrink_0().gap(px(6.0)).child(bot.get().map(|id| project_picker(app, id, files))).child(dynamic(move || {
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
        }))
    });
    let (actions_beside, actions_under) = (actions.clone(), actions);
    view().px(px(24.0)).pb(px(18.0)).pt(px(6.0)).child(
        view()
            .w_full()
            .max_w(px(READING_WIDTH - 48.0))
            .mx(auto())
            .bg(pane())
            .border(px(LINE))
            .border_color(border())
            .rounded(move || px(if tall.get() { 18.0 } else { 22.0 }))
            .transition_all()
            .duration(200.0)
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
                // The words, with the buttons beside them while they are one line.
                view()
                    .flex_row()
                    .items_center()
                    .child(when(move || !tall.get(), move || view().pl(px(8.0)).child(attach_button())))
                    .child(
                        nsh_base::input()
                            .value(draft)
                            .placeholder({
                                let placeholder = placeholder.clone();
                                move || placeholder()
                            })
                            .placeholder_color(move || color(muted()))
                            .multiline(true)
                            .submit_behavior(SubmitBehavior::Submit)
                            .auto_focus(true)
                            .flex_1()
                            .min_w(px(0.0))
                            .min_h(px(44.0))
                            .max_h(px(TALLEST))
                            .scrollbar(move || full.get())
                            .px(move || px(if tall.get() { 16.0 } else { 8.0 }))
                            .pt(move || px(if tall.get() { 14.0 } else { 12.0 }))
                            .pb(move || px(if tall.get() { 6.0 } else { 12.0 }))
                            .text_size(px(BODY))
                            .text_color(ink())
                            .test_id("composer")
                            .on_focus(move || focused.set(true))
                            .on_blur(move || focused.set(false))
                            // One line that no longer fits beside the buttons is more than one.
                            .on_layout(move |frame: &Bounds<Points>| {
                                full.set(frame.size.height.0 >= TALLEST - 1.0);
                                if frame.size.height.0 > 50.0 && !expanded.get_untracked() && !draft.with_untracked(|d| d.is_empty()) {
                                    settle();
                                    expanded.set(true);
                                }
                            })
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
                    .child(when(move || !tall.get(), move || view().pr(px(6.0)).child(actions_beside()))),
            )
            .child(dynamic(move || {
                problem.get().map(|message| {
                    text(message).px(px(16.0)).pt(px(4.0)).text_size(px(SMALL)).text_color(failed())
                })
            }))
            .child(when(move || tall.get(), move || {
                view()
                    .flex_row()
                    .items_center()
                    .pl(px(8.0))
                    .pr(px(6.0))
                    .pb(px(6.0))
                    .pt(px(2.0))
                    .child(attach_button())
                    .child(view().flex_1())
                    .child(actions_under())
            })),
    )
}
