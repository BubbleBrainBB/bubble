//! The icon rail on the window chrome, and the sidebar inside the card:
//! the Manager, New Task, and every project with its tasks.

use std::collections::HashSet;

use bubble_store::{TaskState, TaskSummary};
use nsh_core::prelude::*;

use crate::palette::*;
use crate::state::{AppState, Route};
use crate::ui::{icon, project_name, state_icon};

// Icons draw in `currentColor`, the text color of the element they sit in.
pub const CHAT_ICON: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><path d="M3 3.5h10a1 1 0 0 1 1 1v6a1 1 0 0 1-1 1H7l-3 2.5v-2.5H3a1 1 0 0 1-1-1v-6a1 1 0 0 1 1-1z" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linejoin="round"/><path d="M5 6.5h6M5 8.75h4" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round"/></svg>"##;
const INBOX_ICON: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><path d="M2.5 9.5l1.8-6h7.4l1.8 6v3a1 1 0 0 1-1 1h-9a1 1 0 0 1-1-1z" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linejoin="round"/><path d="M2.5 9.5h3l1 1.5h3l1-1.5h3" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linejoin="round"/></svg>"##;
const TASKS_ICON: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><rect x="2.5" y="2.5" width="11" height="11" rx="2" fill="none" stroke="currentColor" stroke-width="1.3"/><path d="M5.5 8.1l1.6 1.6 3.4-3.4" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round"/></svg>"##;
const SETTINGS_ICON: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><circle cx="8" cy="8" r="2" fill="none" stroke="currentColor" stroke-width="1.3"/><path d="M8 1.75v1.5M8 12.75v1.5M1.75 8h1.5M12.75 8h1.5M3.6 3.6l1.05 1.05M11.35 11.35l1.05 1.05M3.6 12.4l1.05-1.05M11.35 4.65l1.05-1.05" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round"/></svg>"##;
const COMPOSE_ICON: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><path d="M9.5 2.5H4a1.5 1.5 0 0 0-1.5 1.5v8A1.5 1.5 0 0 0 4 13.5h8a1.5 1.5 0 0 0 1.5-1.5V6.5" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round"/><path d="M12 2l2 2-5.5 5.5H6.5v-2z" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linejoin="round"/></svg>"##;
pub const FOLDER_ICON: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><path d="M2 4.5a1 1 0 0 1 1-1h3l1.5 1.5H13a1 1 0 0 1 1 1V12a1 1 0 0 1-1 1H3a1 1 0 0 1-1-1z" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linejoin="round"/></svg>"##;
const FOLDER_PLUS_ICON: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><path d="M2 4.5a1 1 0 0 1 1-1h3l1.5 1.5H13a1 1 0 0 1 1 1V12a1 1 0 0 1-1 1H3a1 1 0 0 1-1-1z" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linejoin="round"/><path d="M8 7.25v3.5M6.25 9h3.5" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round"/></svg>"##;

/// How many tasks a project shows before "Show more".
const FOLDED: usize = 5;

// --- The rail ------------------------------------------------------------------

pub fn rail(app: AppState) -> impl IntoElement {
    let inbox_count = move || {
        app.tasks.with(|tasks| {
            tasks
                .iter()
                .filter(|t| matches!(t.state, TaskState::NeedsYou | TaskState::Failed | TaskState::Review))
                .count()
        })
    };
    view()
        .w(px(RAIL_WIDTH))
        .flex_shrink_0()
        .items_center()
        .pt(px(2.0))
        .pb(px(10.0))
        .gap(px(6.0))
        .child(rail_button(app, CHAT_ICON, "Home", Route::Home, || false))
        .child(rail_button(app, INBOX_ICON, "Inbox", Route::Inbox, move || inbox_count() > 0))
        .child(rail_button(app, TASKS_ICON, "Tasks", Route::Tasks, || false))
        .child(view().flex_1())
        .child(rail_button(app, SETTINGS_ICON, "Settings", Route::Settings, || false))
}

fn rail_button(
    app: AppState,
    svg_markup: &'static str,
    label: &'static str,
    route: Route,
    dot: impl Fn() -> bool + Copy + 'static,
) -> impl IntoElement {
    let target = route.clone();
    let current = computed(move || {
        let now = app.route.get();
        now == route || (route == Route::Tasks && matches!(now, Route::Task(_)))
    });
    view()
        .size(px(28.0))
        .rounded(px(7.0))
        .items_center()
        .justify_center()
        .bg(move || if current.get() { item_active() } else { chrome() })
        .hover(|s| s.bg(item_hover()))
        .cursor(Cursor::Pointer)
        .on_press(move || app.route.set(target.clone()))
        .label(label)
        .test_id(&format!("nav-{}", label.to_lowercase()))
        .child(icon(svg_markup, 17.0).text_color(move || if current.get() { ink() } else { secondary() }))
        .child(when(dot, || {
            view().absolute().top(px(3.0)).right(px(3.0)).size(px(6.0)).rounded_full().bg(needs())
        }))
}

// --- The sidebar ---------------------------------------------------------------

/// One project's tasks, as the sidebar groups them.
#[derive(Clone, PartialEq)]
struct Group {
    /// The project's folder; empty for tasks without one.
    cwd: String,
    tasks: Vec<TaskSummary>,
}

fn groups(app: AppState) -> Vec<Group> {
    let mut groups: Vec<Group> = Vec::new();
    let mut tasks = app.tasks.get();
    tasks.sort_by(|a, b| b.task.updated_at.cmp(&a.task.updated_at));
    for task in tasks {
        let cwd = task.task.project_cwd.clone().unwrap_or_default();
        match groups.iter_mut().find(|g| g.cwd == cwd) {
            Some(group) => group.tasks.push(task),
            None => groups.push(Group { cwd, tasks: vec![task] }),
        }
    }
    // Tasks without a project come last.
    groups.sort_by_key(|g| g.cwd.is_empty());
    groups
}

pub fn sidebar(app: AppState) -> impl IntoElement {
    let unfolded = signal(HashSet::<String>::new());
    let home_current = computed(move || app.route.get() == Route::Home);
    view()
        .w(px(SIDEBAR_WIDTH))
        .flex_shrink_0()
        .bg(side())
        .child(
            view()
                .h(px(48.0))
                .px(px(16.0))
                .flex_row()
                .items_center()
                .child(text("Bubble").text_size(px(TITLE)).font_weight(FontWeight::SEMIBOLD)),
        )
        .child(
            view()
                .px(px(8.0))
                .gap(px(1.0))
                .child(
                    side_row(move || home_current.get())
                        .on_press(move || app.route.set(Route::Home))
                        .child(icon(CHAT_ICON, 15.0).text_color(secondary()))
                        .child(text("Manager").text_size(px(BODY)).text_color(ink())),
                )
                .child(
                    side_row(|| false)
                        .on_press(move || {
                            app.composing_task.set(true);
                            app.route.set(Route::Tasks);
                        })
                        .child(icon(COMPOSE_ICON, 15.0).text_color(secondary()))
                        .child(text("New Task").text_size(px(BODY)).text_color(ink())),
                ),
        )
        .child(
            view()
                .h(px(36.0))
                .mt(px(14.0))
                .pl(px(16.0))
                .pr(px(10.0))
                .flex_row()
                .items_center()
                .justify_between()
                .child(text("Projects").text_size(px(BODY)).text_color(muted()))
                .child(
                    view()
                        .size(px(22.0))
                        .rounded(px(6.0))
                        .items_center()
                        .justify_center()
                        .cursor(Cursor::Pointer)
                        .hover(|s| s.bg(item_hover()))
                        .on_press(move || {
                            app.composing_task.set(true);
                            app.route.set(Route::Tasks);
                        })
                        .label("Task in a new project")
                        .child(icon(FOLDER_PLUS_ICON, 15.0).text_color(muted())),
                ),
        )
        .child(
            scroll().flex_1().child(
                view().px(px(8.0)).pb(px(12.0)).gap(px(10.0)).child(each(
                    move || groups(app),
                    |group: &Group| group.cwd.clone(),
                    move |group, _| project_group(app, group, unfolded),
                )),
            ),
        )
}

fn project_group(app: AppState, group: ReadSignal<Group>, unfolded: Signal<HashSet<String>>) -> impl IntoElement {
    let name = move || {
        let cwd = group.get().cwd;
        if cwd.is_empty() { "No project".to_owned() } else { project_name(&cwd) }
    };
    let is_open = move || unfolded.with(|set| set.contains(&group.get().cwd));
    let shown = move || {
        let all = group.get().tasks;
        if is_open() { all } else { all.into_iter().take(FOLDED).collect() }
    };
    let hidden = move || group.get().tasks.len().saturating_sub(FOLDED);
    view()
        .gap(px(1.0))
        .child(
            view()
                .h(px(30.0))
                .px(px(8.0))
                .flex_row()
                .items_center()
                .gap(px(8.0))
                .child(icon(FOLDER_ICON, 15.0).text_color(secondary()))
                .child(text(name).flex_1().text_size(px(BODY)).text_color(ink()).number_of_lines(1)),
        )
        .child(each(
            shown,
            |task: &TaskSummary| task.task.id.clone(),
            move |task, _| {
                let current = computed(move || matches!(app.route.get(), Route::Task(id) if id == task.get().task.id));
                let in_motion = move || {
                    matches!(
                        task.get().state,
                        TaskState::NeedsYou | TaskState::Running | TaskState::Queued | TaskState::Failed
                    )
                };
                side_row(move || current.get())
                    .test_id(&format!("side-{}", task.get_untracked().task.title))
                    .pl(px(31.0))
                    .on_press(move || app.route.set(Route::Task(task.get_untracked().task.id)))
                    .child(
                        text(move || task.get().task.title)
                            .flex_1()
                            .text_size(px(BODY))
                            .text_color(move || if current.get() { ink() } else { secondary() })
                            .number_of_lines(1),
                    )
                    .child(when(in_motion, move || state_icon(move || task.get().state, 12.0)))
            },
        ))
        .child(when(move || hidden() > 0, move || {
            text(move || if is_open() { "Show less".to_owned() } else { "Show more".to_owned() })
                .pl(px(39.0))
                .py(px(5.0))
                .text_size(px(BODY))
                .text_color(muted())
                .cursor(Cursor::Pointer)
                .on_press(move || {
                    let cwd = group.get_untracked().cwd;
                    unfolded.update(|set| {
                        if !set.remove(&cwd) {
                            set.insert(cwd);
                        }
                    });
                })
        }))
}

/// A sidebar row: a soft fill while it is where the window is.
fn side_row(is_current: impl Fn() -> bool + Copy + 'static) -> View {
    view()
        .h(px(30.0))
        .px(px(8.0))
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .rounded(px(7.0))
        .cursor(Cursor::Pointer)
        .bg(move || if is_current() { item_active() } else { side() })
        .hover(|s| s.bg(item_hover()))
}
