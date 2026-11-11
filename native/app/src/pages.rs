//! The screens: Home (the Manager), Tasks, a task, Settings; and
//! what each puts in the window's top strip.

use bubble_store::{Lifecycle, RunRole, TaskState, TaskSummary};
use std::rc::Rc;

use nsh_core::prelude::*;

use crate::palette::*;
use crate::sidebar::{CHAT_ICON, FOLDER_ICON};
use crate::state::{AppState, Route};
use crate::ui::*;

// --- The top strip ---------------------------------------------------------------

fn strip_title(label: impl IntoReactive<SharedString>) -> Text {
    text(label).text_size(px(BODY)).text_color(secondary()).number_of_lines(1)
}

/// The screen's title and actions, shown in the top strip over the pane.
pub fn title_bar(app: AppState, route: Route) -> AnyElement {
    let row = view().flex_1().min_w(px(0.0)).flex_row().items_center().gap(px(8.0));
    match route {
        Route::Home => row
            .child(icon(CHAT_ICON, 15.0).text_color(secondary()))
            .child(strip_title("Manager"))
            .into_element(),
        Route::PullRequests => row.child(strip_title("Pull requests")).into_element(),
        Route::Automations => row.child(strip_title("Automations")).into_element(),
        Route::Skills => row.child(strip_title("Skills")).into_element(),
        Route::Search => row.child(strip_title("Search")).into_element(),
        Route::Settings => row.child(strip_title("Settings")).into_element(),
        Route::Tasks => row.child(strip_title("Tasks").flex_1()).into_element(),
        Route::Task(id) => {
            let summary_id = id.clone();
            let summary = move || app.tasks.with(|tasks| tasks.iter().find(|t| t.task.id == summary_id).cloned());
            let for_title = summary.clone();
            let for_done = summary.clone();
            let chips_id = id.clone();
            let menu_id = id.clone();
            let start_id = id.clone();
            let can_start = {
                let id = id.clone();
                let summary = summary.clone();
                move || {
                    summary().map(|s| s.state == TaskState::NotStarted).unwrap_or(false)
                        && crate::github::brief_of(app, &id).is_some()
                }
            };
            row.child(icon(FOLDER_ICON, 15.0).text_color(secondary()))
                .child(
                    strip_title(move || {
                        SharedString::from(match for_title() {
                            Some(s) => match s.task.project_cwd {
                                Some(cwd) => format!("{}  /  {}", project_name(&cwd), s.task.title),
                                None => s.task.title,
                            },
                            None => String::new(),
                        })
                    })
                    .flex_1(),
                )
                .child(dynamic(move || {
                    app.task_revision.get();
                    crate::github::link_chips(app, &chips_id)
                        .into_iter()
                        .map(|(label, url)| {
                            view()
                                .h(px(24.0))
                                .px(px(9.0))
                                .justify_center()
                                .rounded(px(6.0))
                                .border(px(LINE))
                                .border_color(border())
                                .window_drag(false)
                                .cursor(Cursor::Pointer)
                                .hover(|s| s.bg(tertiary()))
                                .on_press(move || {
                                    let url = url.clone();
                                    spawn(async move {
                                        let _ = open_url(url).await;
                                    });
                                })
                                .child(text(label).text_size(px(SMALL)).text_color(secondary()))
                        })
                        .collect::<Vec<_>>()
                }))
                .child(when(can_start, move || {
                    let id = start_id.clone();
                    primary_button("Start", move || crate::github::start_task(app, &id))
                }))
                .child(dynamic(move || {
                    let done = for_done()
                        .map(|s| matches!(s.task.lifecycle, Lifecycle::Done | Lifecycle::Canceled))
                        .unwrap_or(false);
                    let id = id.clone();
                    if done {
                        button("Reopen", move || app.set_lifecycle(&id, Lifecycle::Open)).into_element()
                    } else {
                        button("Mark done", move || app.set_lifecycle(&id, Lifecycle::Done)).into_element()
                    }
                }))
                .child(dynamic(move || {
                    let label = if app.changes_open.get() { "Hide changes" } else { "Changes" };
                    button(label, move || app.changes_open.update(|o| *o = !*o))
                }))
                .child(task_menu(app, menu_id))
                .into_element()
        }
    }
}

const MORE_ICON: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><circle cx="3.5" cy="8" r="1.2" fill="currentColor"/><circle cx="8" cy="8" r="1.2" fill="currentColor"/><circle cx="12.5" cy="8" r="1.2" fill="currentColor"/></svg>"##;

fn menu_row(label: impl Into<SharedString>) -> impl IntoElement {
    view()
        .h(px(30.0))
        .px(px(10.0))
        .justify_center()
        .rounded(px(6.0))
        .child(text(label.into()).text_size(px(BODY)).text_color(ink()).number_of_lines(1))
}

fn menu_item_of(label: impl Into<SharedString>, on_select: impl Fn() + 'static) -> impl IntoElement {
    let label = label.into();
    nsh_base::menu_item()
        .accessibility_label(label.clone())
        .rounded(px(6.0))
        .cursor(Cursor::Pointer)
        .hover(|s| s.bg(tertiary()))
        .on_select(move || on_select())
        .child(menu_row(label))
}

/// The task's other actions: run it again on another model, link an issue
/// or PR, cancel it.
fn task_menu(app: AppState, task_id: String) -> impl IntoElement {
    nsh_base::menu()
        .placement(Placement::Bottom)
        .align(Align::End)
        .offset(px(6.0))
        .popup_style(|s| {
            s.bg(pane()).border(px(LINE)).border_color(border()).rounded(px(10.0)).p(px(4.0)).min_w(px(220.0)).shadow_lg()
        })
        .trigger(
            view()
                .size(px(28.0))
                .items_center()
                .justify_center()
                .rounded(px(6.0))
                .window_drag(false)
                .cursor(Cursor::Pointer)
                .hover(|s| s.bg(tertiary()))
                .label("Task actions")
                .child(icon(MORE_ICON, 16.0).text_color(secondary())),
        )
        .child(dynamic(move || {
            let conversation = app.task_conversation(&task_id);
            let lifecycle = app.tasks.with_untracked(|t| t.iter().find(|t| t.task.id == task_id).map(|t| t.task.lifecycle));
            let mut items: Vec<AnyElement> = Vec::new();
            if !conversation.busy.get() {
                let models = app.models.get_untracked();
                let id = task_id.clone();
                items.push(
                    nsh_base::submenu()
                        .trigger(
                            view()
                                .rounded(px(6.0))
                                .cursor(Cursor::Pointer)
                                .hover(|s| s.bg(tertiary()))
                                .child(menu_row("Run again on")),
                        )
                        .children(
                            models
                                .into_iter()
                                .map(|model| {
                                    let id = id.clone();
                                    let chosen = model.clone();
                                    menu_item_of(model, move || {
                                        let conversation = app.task_conversation(&id);
                                        if let Err(message) = conversation.rerun(app, chosen.clone()) {
                                            log::warn!("rerun {id}: {message}");
                                        }
                                    })
                                    .into_element()
                                })
                                .collect::<Vec<_>>(),
                        )
                        .into_element(),
                );
            }
            items.push(menu_item_of("Link an issue or PR", move || app.linking.set(true)).into_element());
            if !matches!(lifecycle, Some(Lifecycle::Done | Lifecycle::Canceled)) {
                items.push(nsh_base::menu_separator().h(px(LINE)).my(px(4.0)).bg(hairline()).into_element());
                let id = task_id.clone();
                items.push(menu_item_of("Cancel task", move || app.cancel_task(&id)).into_element());
            }
            items
        }))
}

/// "Link an issue or PR": what to link, typed or pasted.
fn link_bar(app: AppState, task_id: String) -> impl IntoElement {
    let draft = signal(String::new());
    let problem = signal(None::<String>);
    let busy = signal(false);
    let submit = Rc::new(move || {
        if busy.get_untracked() {
            return;
        }
        busy.set(true);
        crate::github::link_task(app, &task_id, &draft.get_untracked(), move |error| {
            busy.set(false);
            match error {
                Some(message) => problem.set(Some(message)),
                None => {
                    problem.set(None);
                    app.linking.set(false);
                }
            }
        });
    });
    let on_submit = submit.clone();
    view()
        .w_full()
        .max_w(px(READING_WIDTH - 48.0))
        .mx(auto())
        .mt(px(12.0))
        .gap(px(6.0))
        .child(
            view()
                .flex_row()
                .items_center()
                .gap(px(8.0))
                .child(
                    nsh_base::input()
                        .value(draft)
                        .placeholder("#12, owner/repo#12 or a GitHub link".to_owned())
                        .placeholder_color(move || color(muted()))
                        .auto_focus(true)
                        .flex_1()
                        .h(px(32.0))
                        .px(px(10.0))
                        .rounded(px(8.0))
                        .border(px(LINE))
                        .border_color(border())
                        .bg(pane())
                        .text_size(px(BODY))
                        .text_color(ink())
                        .on_change(move |value: &str| draft.set(value.to_owned()))
                        .on_submit(move |_: &str| on_submit()),
                )
                .child(primary_button("Link", move || submit()))
                .child(button("Cancel", move || app.linking.set(false))),
        )
        .child(dynamic(move || {
            problem.get().map(|message| text(message).text_size(px(SMALL)).text_color(failed()))
        }))
}

// --- Home --------------------------------------------------------------------

pub fn home(app: AppState) -> impl IntoElement {
    let conversation = app.manager_conversation();
    view()
        .flex_1()
        .child(conversation_view(
            app,
            conversation,
            "What should we work on?",
            "Tell Manager what needs doing.",
        ))
        .child(away_row(app))
        .child(composer(app, conversation, "Message Manager", ComposerFor::Manager))
}

/// What a GitHub event became, in a few words.
fn outcome(disposition: &str) -> &'static str {
    match disposition {
        "attached" => "went to its task",
        "pending" => "on its way to a task",
        "created" => "new task to start",
        "untriaged" => "being sorted",
        "ignored" => "needed nothing",
        _ => "recorded",
    }
}

/// "While you were away": what GitHub brought since the person last wrote
/// to the Manager, and what became of it; it opens to the list.
fn away_row(app: AppState) -> impl IntoElement {
    let open = signal(false);
    let summary = move || {
        let events = app.away.get();
        let count = |d: &[&str]| events.iter().filter(|e| d.contains(&e.disposition.as_str())).count();
        let parts: Vec<String> = [
            (count(&["attached", "pending"]), "went to tasks"),
            (count(&["created"]), "new tasks to start"),
            (count(&["untriaged"]), "being sorted"),
            (count(&["ignored"]), "needed nothing"),
            (count(&["noted"]), "recorded"),
        ]
        .into_iter()
        .filter(|(n, _)| *n > 0)
        .map(|(n, what)| format!("{n} {what}"))
        .collect();
        let things = if events.len() == 1 { "1 thing".to_owned() } else { format!("{} things", events.len()) };
        format!("While you were away, GitHub brought {things}: {}", parts.join(", "))
    };
    when(move || !app.away.with(Vec::is_empty), move || {
        view()
            .w_full()
            .max_w(px(READING_WIDTH - 48.0))
            .mx(auto())
            .px(px(24.0))
            .pt(px(8.0))
            .gap(px(6.0))
            .child(
                text(summary)
                    .text_size(px(SMALL))
                    .text_color(secondary())
                    .cursor(Cursor::Pointer)
                    .on_press(move || open.update(|o| *o = !*o))
                    .label("While you were away"),
            )
            .child(when(move || open.get(), move || {
                view().gap(px(4.0)).child(each(
                    move || app.away.get(),
                    |event: &bubble_store::Event| event.id.clone(),
                    move |event, _| away_item(app, event),
                ))
            }))
    })
}

fn away_item(app: AppState, event: ReadSignal<bubble_store::Event>) -> impl IntoElement {
    let line = move || {
        let e = event.get();
        let title = serde_json::from_str::<serde_json::Value>(&e.payload)
            .ok()
            .and_then(|p| p["title"].as_str().map(str::to_owned))
            .unwrap_or_else(|| e.payload.lines().next().unwrap_or_default().chars().take(80).collect());
        format!("{} #{} {}: {} ({})", e.repo.unwrap_or_default(), e.number.unwrap_or_default(), e.kind.replace('_', " "), title, outcome(&e.disposition))
    };
    text(line)
        .text_size(px(SMALL))
        .text_color(muted())
        .number_of_lines(1u32)
        .cursor(Cursor::Pointer)
        .on_press(move || {
            if let Some(task) = event.get_untracked().task_id {
                app.route.set(Route::Task(task));
            }
        })
}

// --- Task lists ----------------------------------------------------------------

fn task_row(app: AppState, task: ReadSignal<TaskSummary>) -> View {
    let project = move || task.get().task.project_cwd.map(|p| project_name(&p)).unwrap_or_default();
    let summary = move || {
        let task = task.get();
        match task.state {
            TaskState::Failed => task.last_error.unwrap_or_else(|| "Stopped with an error".into()),
            other => state_label(other).to_owned(),
        }
    };
    view()
        .flex_row()
        .items_center()
        .gap(px(12.0))
        .px(px(12.0))
        .py(px(9.0))
        .rounded(px(8.0))
        .cursor(Cursor::Pointer)
        .hover(|s| s.bg(item_hover()))
        .on_press(move || app.route.set(Route::Task(task.get_untracked().task.id)))
        .child(state_icon(move || task.get().state, 15.0))
        .child(
            view()
                .flex_1()
                .min_w(px(0.0))
                .gap(px(1.0))
                .child(text(move || task.get().task.title).text_size(px(BODY)).text_color(ink()).number_of_lines(1))
                .child(text(summary).text_size(px(SMALL)).text_color(muted()).number_of_lines(1)),
        )
        .child(text(project).text_size(px(SMALL)).text_color(muted()))
        .child(text(move || age(task.get().task.updated_at)).min_w(px(28.0)).text_size(px(SMALL)).text_color(muted()))
}

/// How many tasks a group of the Tasks page shows before "Show all".
const FOLD_AT: usize = 20;

fn grouped(app: AppState, groups: &'static [(&'static str, &'static [TaskState])], empty: &'static str) -> impl IntoElement {
    let any = move || app.tasks.with(|tasks| tasks.iter().any(|t| groups.iter().any(|(_, s)| s.contains(&t.state))));
    scroll().flex_1().child(
        view()
            .w_full()
            .max_w(px(READING_WIDTH + 80.0))
            .mx(auto())
            .px(px(20.0))
            .pt(px(12.0))
            .pb(px(24.0))
            .child(when(move || !any(), move || {
                view()
                    .pt(px(120.0))
                    .items_center()
                    .child(text(empty).text_size(px(BODY)).text_color(muted()))
            }))
            .child(
                groups
                    .iter()
                    .map(move |(label, states)| {
                        let states: &'static [TaskState] = states;
                        let members = move || {
                            app.tasks.with(|tasks| {
                                tasks.iter().filter(|t| states.contains(&t.state)).cloned().collect::<Vec<_>>()
                            })
                        };
                        let count = move || members().len();
                        let unfolded = signal(false);
                        let shown = move || {
                            let all = members();
                            if unfolded.get() || all.len() <= FOLD_AT { all } else { all.into_iter().take(FOLD_AT).collect() }
                        };
                        when(move || count() > 0, move || {
                            view()
                                .child(
                                    view()
                                        .flex_row()
                                        .items_center()
                                        .gap(px(6.0))
                                        .px(px(12.0))
                                        .pt(px(16.0))
                                        .pb(px(4.0))
                                        .child(text(*label).text_size(px(SMALL)).text_color(muted()))
                                        .child(text(move || count().to_string()).text_size(px(SMALL)).text_color(muted())),
                                )
                                .child(each(shown, |t: &TaskSummary| t.task.id.clone(), move |task, _| task_row(app, task)))
                                .child(when(move || count() > FOLD_AT, move || {
                                    text(move || {
                                        if unfolded.get() { "Show fewer".to_owned() } else { format!("Show all {}", count()) }
                                    })
                                    .px(px(12.0))
                                    .py(px(8.0))
                                    .text_size(px(BODY))
                                    .text_color(muted())
                                    .cursor(Cursor::Pointer)
                                    .on_press(move || unfolded.update(|u| *u = !*u))
                                }))
                        })
                    })
                    .collect::<Vec<_>>(),
            ),
    )
}

pub fn tasks(app: AppState) -> impl IntoElement {
    const GROUPS: &[(&str, &[TaskState])] = &[
        // What waits on the person comes first.
        ("Needs you", &[TaskState::NeedsYou]),
        ("Failed", &[TaskState::Failed]),
        ("Running", &[TaskState::Running, TaskState::Queued]),
        ("Ready for review", &[TaskState::Review]),
        ("To start", &[TaskState::NotStarted, TaskState::Waiting]),
        ("Done", &[TaskState::Done, TaskState::Canceled]),
    ];
    view().flex_1().child(grouped(app, GROUPS, "No tasks yet. Tell the Manager what needs doing."))
}

// --- A task --------------------------------------------------------------------

pub fn task(app: AppState, task_id: String) -> impl IntoElement {
    let conversation = app.task_conversation(&task_id);
    let link_id = task_id.clone();
    app.linking.set(false);
    let panel_id = task_id.clone();
    view()
        .flex_1()
        .flex_row()
        .child(
            view()
                .flex_1()
                .min_w(px(0.0))
                .child(when(move || app.linking.get(), move || link_bar(app, link_id.clone())))
                .child(conversation_view(
                    app,
                    conversation,
                    "What should the agent do?",
                    "Describe what the agent should do, then send.",
                ))
                .child(composer(app, conversation, "Message this task", ComposerFor::Task(task_id))),
        )
        .child(when(move || app.changes_open.get(), move || crate::changes::panel(app, panel_id.clone())))
}

// --- Skills ------------------------------------------------------------------

pub fn skills(app: AppState) -> impl IntoElement {
    let query = signal(String::new());
    let shown = move || {
        let q = query.get().to_lowercase();
        app.skill_list.with(|list| {
            list.iter()
                .filter(|s| q.is_empty() || s.name.to_lowercase().contains(&q) || s.description.to_lowercase().contains(&q))
                .cloned()
                .collect::<Vec<_>>()
        })
    };
    scroll().flex_1().child(
        view()
            .w_full()
            .max_w(px(READING_WIDTH + 80.0))
            .mx(auto())
            .px(px(20.0))
            .pt(px(16.0))
            .pb(px(24.0))
            .gap(px(8.0))
            .child(search_field("Search skills", query, "skills-search"))
            .child(
                text(move || {
                    let s = app.skills.get();
                    format!("{} skills, from {}. Tasks load the one a request needs.", app.skill_list.with(Vec::len), s.folders.join(", "))
                })
                .px(px(12.0))
                .text_size(px(SMALL))
                .text_color(muted()),
            )
            .child(each(shown, |s: &crate::state::SkillRow| s.name.clone(), move |skill, _| {
                let open = signal(false);
                view()
                    .px(px(12.0))
                    .py(px(10.0))
                    .gap(px(4.0))
                    .border_b(px(LINE))
                    .border_color(hairline())
                    .cursor(Cursor::Pointer)
                    .on_press(move || open.update(|o| *o = !*o))
                    .label(move || format!("Skill {}", skill.get().name))
                    .child(
                        view()
                            .flex_row()
                            .gap(px(8.0))
                            .child(text(move || skill.get().name).text_size(px(BODY)).text_color(ink()))
                            .child(
                                text(move || {
                                    let s = skill.get();
                                    if s.manual_only { format!("{}, only when asked by name", s.source) } else { s.source.to_owned() }
                                })
                                .text_size(px(SMALL))
                                .text_color(muted()),
                            ),
                    )
                    .child(
                        text(move || skill.get().description)
                            .text_size(px(SMALL))
                            .text_color(secondary())
                            .number_of_lines(move || if open.get() { 0_u32 } else { 2_u32 }),
                    )
                    .child(when(move || open.get(), move || {
                        view()
                            .flex_row()
                            .items_center()
                            .gap(px(8.0))
                            .child(text(move || skill.get().folder).flex_1().text_size(px(SMALL)).text_color(muted()).number_of_lines(1))
                            .child(button("Open folder", move || {
                                let url = format!("file://{}", skill.get_untracked().folder);
                                spawn(async move {
                                    let _ = open_url(url).await;
                                });
                            }))
                    }))
            })),
    )
}

/// A one-line search input.
fn search_field(placeholder: &'static str, query: Signal<String>, id: &'static str) -> impl IntoElement {
    nsh_base::input()
        .value(query)
        .placeholder(placeholder.to_owned())
        .placeholder_color(move || color(muted()))
        .auto_focus(true)
        .h(px(34.0))
        .px(px(12.0))
        .rounded(px(8.0))
        .border(px(LINE))
        .border_color(border())
        .bg(pane())
        .text_size(px(BODY))
        .text_color(ink())
        .test_id(id)
        .on_change(move |v: &str| query.set(v.to_owned()))
}

// --- Search ------------------------------------------------------------------

/// Around the first place `query` is in `text`, on one line.
fn snippet(text: &str, query: &str) -> String {
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let lower = flat.to_lowercase();
    let Some(at) = lower.find(&query.to_lowercase()) else { return flat.chars().take(120).collect() };
    let chars: Vec<(usize, char)> = flat.char_indices().collect();
    let index = chars.iter().position(|(i, _)| *i >= at).unwrap_or(0);
    let start = index.saturating_sub(50);
    let end = (index + query.chars().count() + 70).min(chars.len());
    let piece: String = chars[start..end].iter().map(|(_, c)| c).collect();
    format!("{}{}{}", if start > 0 { "…" } else { "" }, piece, if end < chars.len() { "…" } else { "" })
}

pub fn search(app: AppState) -> impl IntoElement {
    let query = signal(String::new());
    let tasks_found = move || {
        let q = query.get().trim().to_lowercase();
        if q.is_empty() {
            return Vec::new();
        }
        app.tasks.with(|tasks| {
            tasks
                .iter()
                .filter(|t| {
                    t.task.title.to_lowercase().contains(&q)
                        || t.task.project_cwd.as_deref().is_some_and(|p| project_name(p).to_lowercase().contains(&q))
                })
                .take(20)
                .cloned()
                .collect::<Vec<_>>()
        })
    };
    let messages_found = move || {
        let q = query.get().trim().to_owned();
        if q.chars().count() < 2 {
            return Vec::new();
        }
        let found = app.with_store(|s| s.search_messages(&q, 30)).unwrap_or_default();
        app.tasks.with_untracked(|tasks| {
            found
                .into_iter()
                .filter_map(|(task_id, text, at)| {
                    let title = tasks.iter().find(|t| t.task.id == task_id)?.task.title.clone();
                    Some((task_id, title, snippet(&text, &q), at))
                })
                .collect::<Vec<_>>()
        })
    };
    scroll().flex_1().child(
        view()
            .w_full()
            .max_w(px(READING_WIDTH + 80.0))
            .mx(auto())
            .px(px(20.0))
            .pt(px(16.0))
            .pb(px(24.0))
            .gap(px(8.0))
            .child(search_field("Search tasks and conversations", query, "search"))
            .child(when(move || !tasks_found().is_empty(), move || {
                view()
                    .child(text("Tasks").px(px(12.0)).pt(px(12.0)).pb(px(4.0)).text_size(px(SMALL)).text_color(muted()))
                    .child(each(tasks_found, |t: &TaskSummary| t.task.id.clone(), move |task, _| task_row(app, task)))
            }))
            .child(dynamic(move || {
                let found = messages_found();
                (!found.is_empty()).then(|| {
                    view()
                        .child(text("In conversations").px(px(12.0)).pt(px(12.0)).pb(px(4.0)).text_size(px(SMALL)).text_color(muted()))
                        .children(
                            found
                                .into_iter()
                                .map(|(task_id, title, piece, at)| {
                                    view()
                                        .px(px(12.0))
                                        .py(px(8.0))
                                        .gap(px(2.0))
                                        .rounded(px(8.0))
                                        .cursor(Cursor::Pointer)
                                        .hover(|s| s.bg(item_hover()))
                                        .label(format!("Found in {title}"))
                                        .on_press(move || app.route.set(Route::Task(task_id.clone())))
                                        .child(
                                            view()
                                                .flex_row()
                                                .gap(px(8.0))
                                                .child(text(title).flex_1().text_size(px(BODY)).text_color(ink()).number_of_lines(1))
                                                .child(text(age(at)).text_size(px(SMALL)).text_color(muted())),
                                        )
                                        .child(text(piece).text_size(px(SMALL)).text_color(secondary()).number_of_lines(2))
                                        .into_element()
                                })
                                .collect::<Vec<_>>(),
                        )
                })
            }))
            .child(when(
                move || query.with(|q| q.trim().chars().count() >= 2) && tasks_found().is_empty() && messages_found().is_empty(),
                move || view().pt(px(60.0)).items_center().child(text("Nothing found.").text_size(px(BODY)).text_color(muted())),
            )),
    )
}

// --- Settings ------------------------------------------------------------------

pub fn settings(app: AppState) -> impl IntoElement {
    scroll().flex_1().child(
        view()
            .w_full()
            .max_w(px(READING_WIDTH))
            .mx(auto())
            .px(px(28.0))
            .py(px(28.0))
            .gap(px(28.0))
            .child(text("Manager and tasks").text_size(px(HEADING)).font_weight(FontWeight::SEMIBOLD))
            .child(model_choice(
                app,
                RunRole::Manager,
                "Manager model",
                "Runs the Home conversation, plans and hands out tasks",
                app.manager_model,
            ))
            .child(model_choice(
                app,
                RunRole::Task,
                "Task model",
                "Used for every task unless you ask for another model",
                app.task_model,
            ))
            .child(
                view()
                    .flex_row()
                    .items_center()
                    .gap(px(16.0))
                    .child(
                        view()
                            .flex_1()
                            .gap(px(2.0))
                            .child(text("Tasks running at once").text_size(px(BODY)).text_color(ink()))
                            .child(text("Tasks beyond this wait in a queue").text_size(px(SMALL)).text_color(muted())),
                    )
                    .child(
                        view()
                            .flex_row()
                            .items_center()
                            .rounded(px(8.0))
                            .border(px(LINE))
                            .border_color(border())
                            .child(step_button("−", "Fewer tasks at once", move || {
                                app.set_max_runs(app.max_runs.get_untracked().saturating_sub(1))
                            }))
                            .child(text(move || app.max_runs.get().to_string()).min_w(px(24.0)).text_size(px(BODY)))
                            .child(step_button("+", "More tasks at once", move || {
                                app.set_max_runs(app.max_runs.get_untracked() + 1)
                            })),
                    ),
            )
            .child(reasoning_settings(app))
            .child(appearance_settings(app))
            .child(account_settings(app))
            .child(tool_settings(app))
            .child(github_settings(app))
            .child(when(move || app.problem.get().is_some(), move || {
                text(move || app.problem.get().unwrap_or_default()).text_size(px(SMALL)).text_color(failed())
            })),
    )
}

/// Light, dark, or as the system is.
fn appearance_settings(app: AppState) -> impl IntoElement {
    use nsh_core::platform::Appearance;
    let options = [("As the system", Appearance::System), ("Light", Appearance::Light), ("Dark", Appearance::Dark)];
    view()
        .gap(px(8.0))
        .child(
            view()
                .gap(px(2.0))
                .child(text("Appearance").text_size(px(BODY)).text_color(ink()))
                .child(text("Follows the system unless you choose").text_size(px(SMALL)).text_color(muted())),
        )
        .child(
            view().flex_row().gap(px(8.0)).children(
                options
                    .into_iter()
                    .map(|(label, appearance)| {
                        let chosen = move || app.appearance.get() == appearance;
                        view()
                            .h(px(30.0))
                            .px(px(12.0))
                            .justify_center()
                            .rounded(px(8.0))
                            .border(px(LINE))
                            .border_color(border())
                            .bg(move || if chosen() { tertiary() } else { pane() })
                            .cursor(Cursor::Pointer)
                            .hover(|s| s.bg(tertiary()))
                            .on_press(move || app.set_appearance(appearance))
                            .label(format!("Appearance {label}"))
                            .child(text(label).text_size(px(BODY)).text_color(move || if chosen() { ink() } else { secondary() }))
                            .into_element()
                    })
                    .collect::<Vec<_>>(),
            ),
        )
}

/// How hard models think, for every model; each takes the nearest level it
/// offers.
fn reasoning_settings(app: AppState) -> impl IntoElement {
    use bubble_core::Level;
    const LEVELS: [Level; 5] = [Level::Off, Level::Low, Level::Medium, Level::High, Level::Max];
    view()
        .flex_row()
        .flex_wrap()
        .items_center()
        .gap(px(16.0))
        .child(
            view()
                .flex_1()
                .min_w(px(200.0))
                .gap(px(2.0))
                .child(text("Reasoning").text_size(px(BODY)).text_color(ink()))
                .child(
                    text("How hard models think; each uses the nearest level it offers")
                        .text_size(px(SMALL))
                        .text_color(muted()),
                ),
        )
        .child(
            view()
                .flex_row()
                .gap(px(2.0))
                .p(px(2.0))
                .rounded(px(8.0))
                .bg(tertiary())
                .child(
                    LEVELS
                        .iter()
                        .map(|level| {
                            let level = *level;
                            let chosen = move || app.thinking.get() == level;
                            let label = match level {
                                Level::Off => "Off",
                                Level::Low => "Low",
                                Level::Medium => "Medium",
                                Level::High => "High",
                                _ => "Max",
                            };
                            view()
                                .h(px(26.0))
                                .px(px(10.0))
                                .justify_center()
                                .rounded(px(6.0))
                                .bg(move || if chosen() { pane() } else { tertiary() })
                                .cursor(Cursor::Pointer)
                                .on_press(move || app.set_thinking(level))
                                .label(format!("Reasoning {label}"))
                                .child(text(label).text_size(px(SMALL)).text_color(move || if chosen() { ink() } else { secondary() }))
                        })
                        .collect::<Vec<_>>(),
                ),
        )
}

/// The subscriptions signed in to (ChatGPT, Grok).
fn account_settings(app: AppState) -> impl IntoElement {
    view()
        .gap(px(8.0))
        .child(
            view()
                .gap(px(2.0))
                .child(text("Subscriptions").text_size(px(BODY)).text_color(ink()))
                .child(
                    text("Sign in with a ChatGPT or Grok plan; their models show in the lists above")
                        .text_size(px(SMALL))
                        .text_color(muted()),
                ),
        )
        .child(each(
            move || app.accounts.get(),
            |account: &crate::state::Account| account.subscription.label().to_owned(),
            move |account, _| {
                let subscription = account.with_untracked(|a| a.subscription);
                view()
                    .flex_row()
                    .items_center()
                    .gap(px(12.0))
                    .px(px(12.0))
                    .py(px(10.0))
                    .rounded(px(10.0))
                    .border(px(LINE))
                    .border_color(border())
                    .bg(pane())
                    .child(text(subscription.label()).text_size(px(BODY)).text_color(ink()).min_w(px(80.0)))
                    .child(text(move || account.get().status).flex_1().text_size(px(SMALL)).text_color(muted()))
                    .child(dynamic(move || {
                        if account.get().signed_in {
                            crate::ui::button("Sign out", move || app.sign_out(subscription)).into_element()
                        } else {
                            crate::ui::button("Sign in", move || app.sign_in(subscription)).into_element()
                        }
                    }))
            },
        ))
}

/// MCP servers, skills and web search: what task agents can use beyond the
/// project folder.
fn tool_settings(app: AppState) -> impl IntoElement {
    let heading = |title: &'static str, note: &'static str| {
        view()
            .gap(px(2.0))
            .child(text(title).text_size(px(BODY)).text_color(ink()))
            .child(text(note).text_size(px(SMALL)).text_color(muted()))
    };
    let row = || {
        view()
            .flex_row()
            .items_center()
            .gap(px(12.0))
            .px(px(12.0))
            .py(px(10.0))
            .rounded(px(10.0))
            .border(px(LINE))
            .border_color(border())
            .bg(pane())
    };
    view()
        .gap(px(8.0))
        .child(heading(
            "MCP servers",
            "From mcpServers in ~/.bubble/settings.json, and a project's .bubble/settings.json; each call asks first",
        ))
        .child(when(move || app.mcp_servers.get().is_empty(), || {
            text("No MCP servers configured.").text_size(px(SMALL)).text_color(muted())
        }))
        .child(each(
            move || app.mcp_servers.get(),
            |server: &crate::state::ServerRow| format!("{}:{}", server.scope, server.name),
            move |server, _| {
                row()
                    .child(text(move || server.get().name).text_size(px(BODY)).text_color(ink()).min_w(px(80.0)))
                    .child(text(move || server.get().scope).text_size(px(SMALL)).text_color(muted()))
                    .child(
                        text(move || server.get().status)
                            .flex_1()
                            .text_size(px(SMALL))
                            .text_color(move || if server.get().failed { failed() } else { muted() }),
                    )
                    .child(dynamic(move || {
                        let row = server.get();
                        let name = row.name.clone();
                        if row.failed {
                            button("Retry", move || app.retry_server(&name)).label(format!("Retry {}", row.name)).into_element()
                        } else {
                            view().into_element()
                        }
                    }))
            },
        ))
        .child(view().h(px(8.0)))
        .child(heading("Skills", "Listed for the agent, which loads one when a task matches"))
        .child(
            row().child(
                text(move || {
                    let skills = app.skills.get();
                    let mut line = format!(
                        "{} skill{} in {}",
                        skills.count,
                        if skills.count == 1 { "" } else { "s" },
                        if skills.folders.is_empty() { "no skill folders yet".to_owned() } else { skills.folders.join(", ") }
                    );
                    if skills.problems > 0 {
                        line.push_str(&format!("; {} could not be used", skills.problems));
                        if let Some(first) = &skills.first_problem {
                            line.push_str(&format!(" (first: {first})"));
                        }
                    }
                    line
                })
                .flex_1()
                .text_size(px(SMALL))
                .text_color(secondary()),
            ),
        )
        .child(view().h(px(8.0)))
        .child(heading("Web", "Search and page reading through Exa's hosted service"))
        .child(row().child(
            text("web_search and web_fetch are on for tasks").flex_1().text_size(px(SMALL)).text_color(secondary()),
        ))
        .child(view().h(px(8.0)))
        .child(heading("Memory", "What tasks learn, summarized for later ones in ~/.bubble/memories"))
        .child(row().child(text(move || app.memory_status.get()).flex_1().text_size(px(SMALL)).text_color(secondary())))
        .child(view().h(px(8.0)))
        .child(heading("Hooks", "Commands run on agent events, from hooks in settings.json"))
        .child(row().child(
            text(move || {
                let rules = app.hook_rules.get();
                if rules.is_empty() { "No hooks configured.".to_owned() } else { rules.join("\n") }
            })
            .flex_1()
            .text_size(px(SMALL))
            .text_color(secondary()),
        ))
}

/// Each project that is a GitHub repository, with how far Bubble may go.
fn github_settings(app: AppState) -> impl IntoElement {
    use crate::github::Level;
    const LEVELS: [Option<Level>; 4] = [None, Some(Level::Observe), Some(Level::Prepare), Some(Level::Act)];
    view()
        .gap(px(8.0))
        .child(
            view()
                .gap(px(2.0))
                .child(text("GitHub").text_size(px(BODY)).text_color(ink()))
                .child(
                    text(move || app.github_status.get().unwrap_or_else(|| "Looking for the GitHub CLI…".into()))
                        .text_size(px(SMALL))
                        .text_color(muted()),
                ),
        )
        .child(when(move || app.github_repos().is_empty(), || {
            text("Projects that are GitHub repositories show here once a task uses them.")
                .text_size(px(SMALL))
                .text_color(muted())
        }))
        .child(each(
            move || app.github_repos(),
            |repo: &crate::github::Repo| repo.name.clone(),
            move |repo, _| {
                view()
                    .flex_row()
                    .flex_wrap()
                    .items_center()
                    .gap(px(12.0))
                    .px(px(12.0))
                    .py(px(10.0))
                    .rounded(px(10.0))
                    .border(px(LINE))
                    .border_color(border())
                    .bg(pane())
                    .child(
                        view()
                            .flex_1()
                            .min_w(px(200.0))
                            .gap(px(2.0))
                            .child(text(move || repo.get().name).text_size(px(BODY)).text_color(ink()))
                            .child(text(move || Level::hint(repo.get().level)).text_size(px(SMALL)).text_color(muted())),
                    )
                    .child(
                        view()
                            .flex_row()
                            .gap(px(2.0))
                            .p(px(2.0))
                            .rounded(px(8.0))
                            .bg(tertiary())
                            .child(
                                LEVELS
                                    .iter()
                                    .map(|level| {
                                        let level = *level;
                                        let chosen = move || repo.get().level == level;
                                        view()
                                            .h(px(26.0))
                                            .px(px(10.0))
                                            .justify_center()
                                            .rounded(px(6.0))
                                            .bg(move || if chosen() { pane() } else { tertiary() })
                                            .cursor(Cursor::Pointer)
                                            .on_press(move || app.set_repo_level(&repo.get_untracked().name, level))
                                            .label(format!("{} for {}", Level::label(level), repo.get_untracked().name))
                                            .child(
                                                text(Level::label(level))
                                                    .text_size(px(SMALL))
                                                    .text_color(move || if chosen() { ink() } else { secondary() }),
                                            )
                                    })
                                    .collect::<Vec<_>>(),
                            ),
                    )
            },
        ))
}

fn step_button(label: &'static str, description: &'static str, on_press: impl Fn() + 'static) -> impl IntoElement {
    view()
        .w(px(30.0))
        .h(px(28.0))
        .items_center()
        .justify_center()
        .cursor(Cursor::Pointer)
        .hover(|s| s.bg(tertiary()))
        .on_press(on_press)
        .label(description)
        .child(text(label).text_size(px(BODY)))
}

/// A model picked from the configured ones; the first row follows the
/// default model.
fn model_choice(
    app: AppState,
    role: RunRole,
    label: &'static str,
    description: &'static str,
    chosen: Signal<Option<String>>,
) -> impl IntoElement {
    let options = move || {
        let mut options: Vec<Option<String>> = vec![None];
        options.extend(app.models.get().into_iter().map(Some));
        if let Some(current) = chosen.get() {
            if !options.contains(&Some(current.clone())) {
                options.push(Some(current));
            }
        }
        options
    };
    view()
        .gap(px(8.0))
        .child(
            view()
                .gap(px(2.0))
                .child(text(label).text_size(px(BODY)).text_color(ink()))
                .child(text(description).text_size(px(SMALL)).text_color(muted())),
        )
        .child(
            view()
                .rounded(px(10.0))
                .border(px(LINE))
                .border_color(border())
                .bg(pane())
                .overflow_hidden()
                .child(each(
                    options,
                    |option: &Option<String>| option.clone().unwrap_or_default(),
                    move |option, index| {
                        let is_chosen = move || chosen.get() == option.get();
                        let name = move || match option.get() {
                            Some(model) => model,
                            None => match app.default_model() {
                                Some(default) => format!("Default ({default})"),
                                None => "Default (none set in config.json)".to_owned(),
                            },
                        };
                        view()
                            .flex_row()
                            .items_center()
                            .gap(px(10.0))
                            .h(px(36.0))
                            .px(px(12.0))
                            .border_t(move || px(if index.get() == 0 { 0.0 } else { LINE }))
                            .border_color(hairline())
                            .bg(move || if is_chosen() { tertiary() } else { pane() })
                            .cursor(Cursor::Pointer)
                            .hover(|s| s.bg(tertiary()))
                            .on_press(move || app.set_model_setting(role, option.get_untracked()))
                            .child(
                                view()
                                    .size(px(14.0))
                                    .rounded_full()
                                    .border(move || px(if is_chosen() { 4.5 } else { 1.0 }))
                                    .border_color(move || if is_chosen() { ink() } else { muted() }),
                            )
                            .child(text(name).text_size(px(BODY)).text_color(ink()))
                    },
                )),
        )
}
