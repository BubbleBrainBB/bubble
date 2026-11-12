//! The Pull requests page: the pull requests tasks are linked to, and the
//! open ones of the watched repositories, with their state and checks.
//! Merging happens only here, by the person (design §5): no tool merges.

use bubble_github::{Checks, Kind, PrSummary};
use nsh_core::prelude::*;

use crate::palette::*;
use crate::state::{AppState, Route};
use crate::ui::*;

/// A pull request as the page lists it.
#[derive(Clone, Debug, PartialEq)]
pub struct PrRow {
    pub repo: String,
    pub number: u64,
    pub title: String,
    /// OPEN, CLOSED or MERGED (as GitHub says).
    pub state: String,
    pub draft: bool,
    pub checks: Option<Checks>,
    pub url: String,
    pub task: Option<String>,
}

impl PrRow {
    fn key(&self) -> String {
        format!("{}#{}", self.repo, self.number)
    }

    fn status(&self) -> String {
        let state = match self.state.as_str() {
            "MERGED" => "Merged",
            "CLOSED" => "Closed",
            _ if self.draft => "Draft",
            _ => "Open",
        };
        match self.checks {
            Some(Checks::Failed) => format!("{state}, checks failing"),
            Some(Checks::Running) => format!("{state}, checks running"),
            Some(Checks::Passed) => format!("{state}, checks passing"),
            _ => state.to_owned(),
        }
    }
}

/// The linked pull requests, from the store (as last polled).
fn linked(app: AppState) -> Vec<PrRow> {
    let links = app.with_store(|s| s.links_of_kind(Kind::PullRequest.as_str())).unwrap_or_default();
    links
        .into_iter()
        .map(|link| {
            let snapshot = link.snapshot.as_deref().and_then(|s| serde_json::from_str::<bubble_github::Snapshot>(s).ok());
            PrRow {
                repo: link.repo.clone(),
                number: link.number as u64,
                title: snapshot.as_ref().map(|s| s.title.clone()).unwrap_or_default(),
                state: snapshot.as_ref().map(|s| s.state.clone()).unwrap_or_else(|| "OPEN".into()),
                draft: false,
                checks: snapshot.as_ref().map(|s| s.checks_state()),
                url: format!("https://github.com/{}/pull/{}", link.repo, link.number),
                task: Some(link.task_id),
            }
        })
        .collect()
}

/// Loads the list: the linked ones at once, then each watched repository's
/// open ones from GitHub.
fn load(app: AppState, rows: Signal<Vec<PrRow>>, problem: Signal<Option<String>>) {
    rows.set(linked(app));
    let Some(gh) = app.gh() else {
        problem.set(Some("Install the GitHub CLI (gh) to see open pull requests.".into()));
        return;
    };
    let repos: Vec<String> = app.github_repos().into_iter().filter(|r| r.level.is_some()).map(|r| r.name).collect();
    spawn(async move {
        let found = background(async move {
            let mut found: Vec<(String, anyhow::Result<Vec<PrSummary>>)> = Vec::new();
            for repo in repos {
                let prs = gh.open_prs(&repo).await;
                found.push((repo, prs));
            }
            found
        })
        .await
        .unwrap_or_default();
        let mut errors = Vec::new();
        rows.update(|rows| {
            for (repo, prs) in found {
                match prs {
                    Ok(prs) => {
                        for pr in prs {
                            match rows.iter_mut().find(|r| r.repo == repo && r.number == pr.number) {
                                Some(row) => {
                                    row.title = pr.title;
                                    row.state = if pr.state.is_empty() { "OPEN".into() } else { pr.state };
                                    row.draft = pr.draft;
                                }
                                None => rows.push(PrRow {
                                    repo: repo.clone(),
                                    number: pr.number,
                                    title: pr.title,
                                    state: if pr.state.is_empty() { "OPEN".into() } else { pr.state },
                                    draft: pr.draft,
                                    checks: None,
                                    url: pr.url,
                                    task: None,
                                }),
                            }
                        }
                    }
                    Err(error) => errors.push(format!("{repo}: {error:#}")),
                }
            }
            // Open ones first, then by repository and number.
            rows.sort_by(|a, b| {
                (a.state != "OPEN").cmp(&(b.state != "OPEN")).then(a.repo.cmp(&b.repo)).then(b.number.cmp(&a.number))
            });
        });
        problem.set((!errors.is_empty()).then(|| errors.join("\n")));
    });
}

/// Gives a pull request no bot follows to the bot that looks after its
/// repository: linked to it, so reviews and checks reach the bot, and
/// open for the person's first word about it.
fn give_to_bot(app: AppState, row: &PrRow) {
    let Some(project) = app.github_repos().into_iter().find(|r| r.name == row.repo).map(|r| r.project) else { return };
    let purpose = format!("Looks after the GitHub repository {}: its issues, pull requests and reviews.", row.repo);
    let Some(bot) = crate::manager::bot_for_project_or_new(app, &project, &purpose) else { return };
    if app.with_store(|s| s.link(&bot, Kind::PullRequest.as_str(), &row.repo, row.number as i64)).is_some() {
        app.refresh_tasks();
        app.route.set(Route::Task(bot));
    }
}

fn pr_row(app: AppState, row: ReadSignal<PrRow>, rows: Signal<Vec<PrRow>>, problem: Signal<Option<String>>) -> impl IntoElement {
    let confirming = signal(false);
    let merging = signal(false);
    let merge = move || {
        let PrRow { repo, number, .. } = row.get_untracked();
        let Some(gh) = app.gh() else { return };
        merging.set(true);
        spawn(async move {
            let done = background(async move { gh.merge_pr(&repo, number).await }).await;
            merging.set(false);
            confirming.set(false);
            match done {
                Ok(Ok(())) => {
                    let key = row.get_untracked().key();
                    rows.update(|rows| {
                        if let Some(r) = rows.iter_mut().find(|r| r.key() == key) {
                            r.state = "MERGED".into();
                        }
                    });
                }
                Ok(Err(error)) => problem.set(Some(format!("{error:#}"))),
                Err(_) => {}
            }
        });
    };
    let task_title = move || {
        let id = row.get().task?;
        app.tasks.with(|t| t.iter().find(|t| t.task.id == id).map(|t| t.task.title.clone()))
    };
    view()
        .px(px(12.0))
        .py(px(10.0))
        .gap(px(8.0))
        .border_b(px(LINE))
        .border_color(hairline())
        .child(
            view()
                .flex_row()
                .items_center()
                .gap(px(10.0))
                .child(
                    view()
                        .flex_1()
                        .min_w(px(0.0))
                        .gap(px(2.0))
                        .child(
                            text(move || {
                                let r = row.get();
                                format!("#{} {}", r.number, if r.title.is_empty() { "(not looked at yet)".into() } else { r.title })
                            })
                            .text_size(px(BODY))
                            .text_color(ink())
                            .number_of_lines(1),
                        )
                        .child(
                            text(move || {
                                let r = row.get();
                                match task_title() {
                                    Some(task) => format!("{}, {}, task: {task}", r.repo, r.status()),
                                    None => format!("{}, {}", r.repo, r.status()),
                                }
                            })
                            .text_size(px(SMALL))
                            .text_color(move || if row.get().checks == Some(Checks::Failed) { failed() } else { muted() })
                            .number_of_lines(1),
                        ),
                )
                .child(button("Open on GitHub", move || {
                    let url = row.get_untracked().url;
                    spawn(async move {
                        let _ = open_url(url).await;
                    });
                }))
                .child(dynamic(move || match row.get().task {
                    Some(task) => button("Open bot", move || app.route.set(Route::Task(task.clone()))).into_element(),
                    None => button("Give to its bot", move || give_to_bot(app, &row.get_untracked())).into_element(),
                }))
                .child(when(move || row.get().state == "OPEN" && !row.get().draft && !confirming.get(), move || {
                    button("Merge", move || confirming.set(true))
                })),
        )
        .child(when(move || confirming.get(), move || {
            view()
                .flex_row()
                .items_center()
                .gap(px(8.0))
                .px(px(12.0))
                .py(px(8.0))
                .rounded(px(8.0))
                .bg(tertiary())
                .opacity(move || if merging.get() { 0.5_f32 } else { 1.0 })
                .child(
                    text(move || format!("Squash and merge #{} into {}'s default branch?", row.get().number, row.get().repo))
                        .flex_1()
                        .text_size(px(BODY))
                        .text_color(ink()),
                )
                .child(button("Cancel", move || confirming.set(false)))
                .child(primary_button("Squash and merge", merge))
        }))
}

pub fn page(app: AppState) -> impl IntoElement {
    let rows = signal(Vec::<PrRow>::new());
    let problem = signal(None::<String>);
    load(app, rows, problem);
    scroll().flex_1().child(
        view()
            .w_full()
            .max_w(px(READING_WIDTH + 80.0))
            .mx(auto())
            .px(px(20.0))
            .pt(px(12.0))
            .pb(px(24.0))
            .child(
                view()
                    .flex_row()
                    .items_center()
                    .px(px(12.0))
                    .pt(px(8.0))
                    .pb(px(8.0))
                    .child(
                        text("Linked to tasks, and open in the repositories Bubble watches.")
                            .flex_1()
                            .text_size(px(BODY))
                            .text_color(muted()),
                    )
                    .child(button("Refresh", move || load(app, rows, problem))),
            )
            .child(dynamic(move || problem.get().map(|p| text(p).px(px(12.0)).text_size(px(SMALL)).text_color(failed()))))
            .child(when(move || rows.with(|r| r.is_empty()), move || {
                view().pt(px(100.0)).items_center().child(text("No pull requests.").text_size(px(BODY)).text_color(muted()))
            }))
            .child(each(move || rows.get(), PrRow::key, move |row, _| pr_row(app, row, rows, problem))),
    )
}
