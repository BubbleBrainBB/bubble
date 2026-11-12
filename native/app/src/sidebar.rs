//! The sidebar, as a messenger has it: the main bot on top, then every bot
//! with the newest thing said in its conversation, and Settings at the
//! bottom. Nothing else lives here: what bots do shows in their
//! conversations.

use bubble_store::{MessageRole, TaskState, TaskSummary};
use nsh_core::prelude::*;

use crate::avatar;
use crate::manager::MAIN_BOT;
use crate::palette::*;
use crate::state::{AppState, Route};
use crate::ui::icon;

// Icons draw in `currentColor`, the text color of the element they sit in.
const SETTINGS_ICON: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><circle cx="8" cy="8" r="2" fill="none" stroke="currentColor" stroke-width="1.3"/><path d="M8 1.75v1.5M8 12.75v1.5M1.75 8h1.5M12.75 8h1.5M3.6 3.6l1.05 1.05M11.35 11.35l1.05 1.05M3.6 12.4l1.05-1.05M11.35 4.65l1.05-1.05" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round"/></svg>"##;
pub const FOLDER_ICON: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><path d="M2 4.5a1 1 0 0 1 1-1h3l1.5 1.5H13a1 1 0 0 1 1 1V12a1 1 0 0 1-1 1H3a1 1 0 0 1-1-1z" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linejoin="round"/></svg>"##;
const SEARCH_ICON: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><circle cx="7" cy="7" r="4.25" fill="none" stroke="currentColor" stroke-width="1.4"/><path d="M10.25 10.25L13.5 13.5" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round"/></svg>"##;
const PLUS_ICON: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><path d="M8 3v10M3 8h10" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round"/></svg>"##;

/// A round button raised off the sidebar.
fn round_button(markup: &'static str, label: &'static str, id: &'static str, on_press: impl Fn() + 'static) -> impl IntoElement {
    view()
        .size(px(30.0))
        .rounded_full()
        .items_center()
        .justify_center()
        .bg(paper())
        .border(px(LINE))
        .border_color(hairline())
        .cursor(Cursor::Pointer)
        .window_drag(false)
        .hover(|s| s.bg(tertiary()))
        .label(label)
        .test_id(id)
        .on_press(on_press)
        .child(icon(markup, 15.0).text_color(ink()))
}

/// What a bot's row says under its name: what it is doing when that needs
/// saying, else the newest thing said.
fn preview_of(app: AppState, bot: &TaskSummary) -> String {
    match bot.state {
        TaskState::NeedsYou => return "Waiting for you".to_owned(),
        TaskState::Running => return "Working…".to_owned(),
        TaskState::Queued => return "Waiting for a free slot".to_owned(),
        _ => {}
    }
    // Work brought to it that nobody asked for outright waits for a yes.
    if let Some(title) = app.proposed.with(|all| all.iter().find(|t| t.task_id == bot.task.id).map(|t| t.title.clone())) {
        return format!("Wants a yes: {title}");
    }
    if bot.state == TaskState::Failed {
        return "Failed on its last turn".to_owned();
    }
    let preview = app.previews.with(|p| p.get(&bot.task.id).cloned());
    match preview {
        Some(preview) => {
            let line = plain(&preview.text);
            if preview.role == MessageRole::User { format!("You: {line}") } else { line }
        }
        None => "No messages yet".to_owned(),
    }
}

/// A message as one line of plain words: its markdown marks say nothing
/// in a list.
fn plain(text: &str) -> String {
    let words = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut line = String::with_capacity(words.len());
    for word in words.split(' ') {
        // Heading and list marks standing alone.
        if !word.is_empty() && word.chars().all(|c| matches!(c, '#' | '-' | '*' | '>')) {
            continue;
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.extend(word.chars().filter(|c| !matches!(c, '`' | '*')));
    }
    line
}

/// Whether a bot cannot go on, or start what was brought to it, without
/// the person: a question or an approval, a failure, or proposed work.
fn needs_person(app: AppState, bot: &TaskSummary) -> bool {
    matches!(bot.state, TaskState::NeedsYou | TaskState::Failed) || app.proposed.with(|all| all.iter().any(|t| t.task_id == bot.task.id))
}

pub fn sidebar(app: AppState) -> impl IntoElement {
    let home_current = computed(move || app.route.get() == Route::Home);
    // The main bot marks what waits on the person anywhere: a question, or a failure.
    let main_busy = move || app.manager_conversation().busy.get();
    let main_waits = move || app.manager_conversation().waits_on_person();
    view()
        .w(px(SIDEBAR_WIDTH))
        .flex_shrink_0()
        .bg(side())
        .border_r(px(LINE))
        .border_color(hairline())
        .child(
            // The window's controls sit on the left of this strip.
            view()
                .h(px(TOP_BAR))
                .flex_shrink_0()
                .flex_row()
                .items_center()
                .justify_end()
                .gap(px(8.0))
                .pr(px(14.0))
                .window_drag(true)
                .child(round_button(SEARCH_ICON, "Search", "nav-search", move || app.route.set(Route::Search)))
                .child(round_button(PLUS_ICON, "New bot", "nav-new-bot", move || app.route.set(Route::NewBot))),
        )
        .child(
            view()
                .items_center()
                .gap(px(6.0))
                .pt(px(6.0))
                .pb(px(14.0))
                .cursor(Cursor::Pointer)
                .label("Main bot")
                .test_id("nav-home")
                .on_press(move || app.route.set(Route::Home))
                .child(
                    view()
                        .p(px(4.0))
                        .rounded_full()
                        .bg(move || if home_current.get() { item_active() } else { side() })
                        // Its eyes follow what it does.
                        .child(avatar::main_doing(app, 56.0, move || {
                            if !main_busy() {
                                avatar::Mood::Still
                            } else if main_waits() {
                                avatar::Mood::Waiting
                            } else {
                                avatar::Mood::Working
                            }
                        }))
                        .child(when(main_busy, move || {
                            view()
                                .absolute()
                                .right(px(6.0))
                                .bottom(px(6.0))
                                .size(px(10.0))
                                .rounded_full()
                                .bg(move || if main_waits() { needs() } else { online() })
                                .border(px(2.0))
                                .border_color(side())
                        })),
                )
                .child(text(MAIN_BOT).text_size(px(SMALL)).text_color(ink())),
        )
        .child(
            scroll().flex_1().child(
                view()
                    .px(px(10.0))
                    .pb(px(10.0))
                    .gap(px(2.0))
                    // What needs the person comes first, under its own heading;
                    // when nothing does, there is no heading.
                    .child(when(move || app.bots().iter().any(|bot| needs_person(app, bot)), || {
                        text("Needs you").px(px(10.0)).pt(px(2.0)).pb(px(4.0)).text_size(px(SMALL)).text_color(muted())
                    }))
                    .child(each(
                        move || app.bots().into_iter().filter(|bot| needs_person(app, bot)).collect::<Vec<_>>(),
                        |bot: &TaskSummary| bot.task.id.clone(),
                        move |bot, _| bot_row(app, bot),
                    ))
                    .child(when(move || app.bots().iter().any(|bot| needs_person(app, bot)), || view().h(px(10.0))))
                    .child(each(
                        move || app.bots().into_iter().filter(|bot| !needs_person(app, bot)).collect::<Vec<_>>(),
                        |bot: &TaskSummary| bot.task.id.clone(),
                        move |bot, _| bot_row(app, bot),
                    ))
                    .child(when(move || app.bots().is_empty(), || {
                        text("No bots yet. Make one with +, or ask Bubble for one.")
                            .px(px(10.0))
                            .pt(px(10.0))
                            .text_size(px(SMALL))
                            .text_color(muted())
                    })),
            ),
        )
        .child(
            view().px(px(14.0)).pb(px(14.0)).pt(px(8.0)).child(
                view()
                    .h(px(34.0))
                    .flex_row()
                    .items_center()
                    .justify_center()
                    .gap(px(8.0))
                    .rounded_full()
                    .bg(move || if app.route.get() == Route::Settings { tertiary() } else { paper() })
                    .border(px(LINE))
                    .border_color(hairline())
                    .cursor(Cursor::Pointer)
                    .hover(|s| s.bg(tertiary()))
                    .label("Settings")
                    .test_id("nav-settings")
                    .on_press(move || app.route.set(Route::Settings))
                    .child(icon(SETTINGS_ICON, 15.0).text_color(secondary()))
                    .child(text("Settings").text_size(px(BODY)).text_color(ink())),
            )
            // A development build's data is not the person's own: say so.
            .child(bubble_core::config::development_data().then(|| {
                text("Development data").self_center().pt(px(8.0)).text_size(px(SMALL)).text_color(muted())
            })),
        )
}

fn bot_row(app: AppState, bot: ReadSignal<TaskSummary>) -> impl IntoElement {
    let id = bot.get_untracked().task.id;
    let current = {
        let id = id.clone();
        computed(move || matches!(app.route.get(), Route::Task(open) if open == id))
    };
    let unread = {
        let id = id.clone();
        move || !current.get() && app.unread(&id)
    };
    let needs_you = move || needs_person(app, &bot.get());
    let open = id.clone();
    view()
        .h(px(58.0))
        .px(px(10.0))
        .flex_row()
        .items_center()
        .gap(px(10.0))
        .rounded(px(12.0))
        .cursor(Cursor::Pointer)
        .bg(move || if current.get() { item_active() } else { side() })
        .hover(|s| s.bg(item_hover()))
        .test_id(&format!("side-{}", bot.get_untracked().task.title))
        .on_press(move || {
            app.mark_seen(&open);
            app.route.set(Route::Task(open.clone()));
        })
        // A face that shows what the bot is doing.
        .child({
            let (mine, news) = (id.clone(), unread.clone());
            avatar::bot_doing(app, &id, 38.0, move || match bot.get().state {
                // What it is at, as its conversation would say it.
                TaskState::Running => app.task_conversation(&mine).mood(),
                TaskState::NeedsYou | TaskState::Queued => avatar::Mood::Waiting,
                TaskState::Failed => avatar::Mood::Troubled,
                // It said something the person has not read yet.
                _ if news() => avatar::Mood::Pleased,
                _ => avatar::Mood::Still,
            })
        })
        .child(
            view()
                .flex_1()
                .min_w(px(0.0))
                .gap(px(2.0))
                .child(
                    // Its name, and its role on the team beside it.
                    view()
                        .flex_row()
                        .items_center()
                        .gap(px(6.0))
                        .child(
                            text(move || bot.get().task.title)
                                .flex_shrink(1.0)
                                .min_w(px(0.0))
                                .text_size(px(BODY))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(ink())
                                .number_of_lines(1),
                        )
                        .child({
                            let id = id.clone();
                            text(move || {
                                app.task_revision.get();
                                app.bot_role(&id)
                            })
                            .flex_shrink_0()
                            .text_size(px(SMALL))
                            .text_color(muted())
                            .number_of_lines(1)
                        }),
                )
                .child(
                    text(move || preview_of(app, &bot.get()))
                        .text_size(px(SMALL))
                        .text_color(move || if needs_you() { needs() } else { muted() })
                        .number_of_lines(1),
                ),
        )
        .child(when(unread, || view().size(px(8.0)).rounded_full().bg(online()).flex_shrink_0()))
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_preview_is_plain_words() {
        assert_eq!(super::plain("## Done\n\n- Added `a.ts` and **b.ts**"), "Done Added a.ts and b.ts");
        assert_eq!(super::plain("  spaced   out  "), "spaced out");
    }
}
