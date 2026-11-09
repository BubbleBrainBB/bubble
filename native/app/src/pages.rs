//! The screens: Home (the Manager), Inbox, Tasks, a task, Settings; and
//! what each puts in the window's top strip.

use bubble_store::{Lifecycle, RunRole, TaskState, TaskSummary};
use nsh_core::file_dialog::OpenDialog;
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
        Route::Inbox => row.child(strip_title("Inbox")).into_element(),
        Route::Settings => row.child(strip_title("Settings")).into_element(),
        Route::Tasks => row
            .child(strip_title("Tasks").flex_1())
            .child(when(move || !app.composing_task.get(), move || {
                primary_button("New task", move || app.composing_task.set(true))
            }))
            .into_element(),
        Route::Task(id) => {
            let summary_id = id.clone();
            let summary = move || app.tasks.with(|tasks| tasks.iter().find(|t| t.task.id == summary_id).cloned());
            let for_title = summary.clone();
            let for_done = summary.clone();
            let chips = crate::github::link_chips(app, &id);
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
                .child(
                    chips
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
                        .collect::<Vec<_>>(),
                )
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
                .into_element()
        }
    }
}

// --- Home --------------------------------------------------------------------

pub fn home(app: AppState) -> impl IntoElement {
    let conversation = app.manager_conversation();
    let model = move || app.model_label(RunRole::Manager);
    view()
        .flex_1()
        .child(conversation_view(
            app,
            conversation,
            "What should we work on?",
            "Tell Manager what needs doing.",
        ))
        .child(composer(app, conversation, "Message Manager", model))
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
                                .child(each(members, |t: &TaskSummary| t.task.id.clone(), move |task, _| task_row(app, task)))
                        })
                    })
                    .collect::<Vec<_>>(),
            ),
    )
}

pub fn inbox(app: AppState) -> impl IntoElement {
    const GROUPS: &[(&str, &[TaskState])] = &[
        ("Needs you", &[TaskState::NeedsYou]),
        ("Failed", &[TaskState::Failed]),
        ("Ready for review", &[TaskState::Review]),
        ("To start", &[TaskState::NotStarted]),
    ];
    view().flex_1().child(grouped(app, GROUPS, "Nothing needs you right now."))
}

pub fn tasks(app: AppState) -> impl IntoElement {
    const GROUPS: &[(&str, &[TaskState])] = &[
        ("Needs you", &[TaskState::NeedsYou]),
        ("Running", &[TaskState::Running, TaskState::Queued]),
        ("Failed", &[TaskState::Failed]),
        ("Ready for review", &[TaskState::Review]),
        ("To start", &[TaskState::NotStarted, TaskState::Waiting]),
        ("Done", &[TaskState::Done, TaskState::Canceled]),
    ];
    view()
        .flex_1()
        .child(when(move || app.composing_task.get(), move || new_task_form(app, app.composing_task)))
        .child(grouped(app, GROUPS, "No tasks yet. Start one with New task."))
}

fn new_task_form(app: AppState, composing: Signal<bool>) -> impl IntoElement {
    let title = signal(String::new());
    let project = signal(app.projects().into_iter().next());
    let create = move || {
        let title_text = title.get_untracked();
        if title_text.trim().is_empty() {
            return;
        }
        if let Some(id) = app.create_task(&title_text, project.get_untracked().as_deref()) {
            composing.set(false);
            app.route.set(Route::Task(id));
        }
    };
    let choose_folder = move || {
        let picked = pick_paths(OpenDialog::folders().button_label("Use folder"));
        spawn(async move {
            match picked.await {
                Ok(Some(paths)) => {
                    if let Some(path) = paths.into_iter().next() {
                        project.set(Some(path.to_string_lossy().into_owned()));
                    }
                }
                Ok(None) => {}
                Err(error) => log::warn!("no folder dialog: {error}"),
            }
        });
    };
    view()
        .w_full()
        .max_w(px(READING_WIDTH + 80.0))
        .mx(auto())
        .px(px(20.0))
        .pt(px(20.0))
        .child(
            view()
                .gap(px(10.0))
                .bg(pane())
                .border(px(LINE))
                .border_color(border())
                .rounded(px(16.0))
                .shadow_md()
                .shadow_color(rgba(0x0F17_2A10))
                .p(px(14.0))
                .child(
                    nsh_base::input()
                        .value(title)
                        .placeholder("What needs doing?".to_owned())
                        .placeholder_color(muted())
                        .auto_focus(true)
                        .px(px(4.0))
                        .text_size(px(TITLE))
                        .text_color(ink())
                        .on_change(move |value: &str| title.set(value.to_owned()))
                        .on_submit(move |_: &str| create()),
                )
                .child(
                    view()
                        .flex_row()
                        .items_center()
                        .gap(px(8.0))
                        .child(
                            view()
                                .flex_1()
                                .min_w(px(0.0))
                                .flex_row()
                                .items_center()
                                .gap(px(6.0))
                                .cursor(Cursor::Pointer)
                                .on_press(choose_folder)
                                .child(icon(FOLDER_ICON, 14.0).text_color(secondary()))
                                .child(
                                    text(move || match project.get() {
                                        Some(path) => project_name(&path),
                                        None => "Choose a project folder".to_owned(),
                                    })
                                    .text_size(px(BODY))
                                    .text_color(secondary())
                                    .number_of_lines(1),
                                ),
                        )
                        .child(button("Cancel", move || composing.set(false)))
                        .child(primary_button("Create", create)),
                ),
        )
}

// --- A task --------------------------------------------------------------------

pub fn task(app: AppState, task_id: String) -> impl IntoElement {
    let conversation = app.task_conversation(&task_id);
    let model = move || app.model_label(RunRole::Task);
    view()
        .flex_1()
        .child(conversation_view(
            app,
            conversation,
            "What should the agent do?",
            "Describe what the agent should do, then send.",
        ))
        .child(composer(app, conversation, "Message this task", model))
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
            .child(account_settings(app))
            .child(tool_settings(app))
            .child(github_settings(app))
            .child(when(move || app.problem.get().is_some(), move || {
                text(move || app.problem.get().unwrap_or_default()).text_size(px(SMALL)).text_color(failed())
            })),
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
                    text("Sign-ins made with the Bubble CLI (/login); their models show in the lists above")
                        .text_size(px(SMALL))
                        .text_color(muted()),
                ),
        )
        .child(when(move || app.accounts.get().is_empty(), || {
            text("Not signed in to ChatGPT or Grok.").text_size(px(SMALL)).text_color(muted())
        }))
        .child(each(
            move || app.accounts.get(),
            |account: &(String, String)| account.0.clone(),
            |account, _| {
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
                    .child(text(move || account.get().0).text_size(px(BODY)).text_color(ink()).min_w(px(80.0)))
                    .child(text(move || account.get().1).flex_1().text_size(px(SMALL)).text_color(muted()))
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
                    .child(when(move || server.get().failed, move || {
                        button("Retry", move || app.retry_server(&server.get_untracked().name))
                            .label(format!("Retry {}", server.get_untracked().name))
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
