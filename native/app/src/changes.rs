//! A task's Changes panel: what its agent changed in the project since the
//! last commit, and what its implementer subagents kept in their worktrees,
//! which the person applies to the project or throws away.

use std::path::PathBuf;

use bubble_core::delegate::Snapshot;
use bubble_core::worktree;
use nsh_base::DiffLayout;
use nsh_core::prelude::*;

use crate::palette::*;
use crate::state::AppState;
use crate::ui::*;

/// Whose changes the panel shows.
#[derive(Clone, Debug, PartialEq)]
enum Source {
    Project,
    /// A subagent's kept worktree, by the subagent's id.
    Worktree(String),
}

/// The subagents whose worktrees hold changes to review.
fn kept(children: &[Snapshot]) -> Vec<Snapshot> {
    children
        .iter()
        .filter(|c| c.status.is_final() && c.worktree.as_ref().is_some_and(|w| w.changed == Some(true) && !w.removed))
        .cloned()
        .collect()
}

fn tab(label: String, chosen: impl Fn() -> bool + 'static, on_press: impl Fn() + 'static) -> impl IntoElement {
    let chosen = std::rc::Rc::new(chosen);
    let for_bg = chosen.clone();
    view()
        .h(px(26.0))
        .px(px(10.0))
        .justify_center()
        .rounded(px(6.0))
        .bg(move || if for_bg() { tertiary() } else { pane() })
        .cursor(Cursor::Pointer)
        .hover(|s| s.bg(tertiary()))
        .on_press(on_press)
        .label(label.clone())
        .child(text(label).text_size(px(BODY)).text_color(move || if chosen() { ink() } else { secondary() }))
}

pub fn panel(app: AppState, task_id: String) -> impl IntoElement {
    let conversation = app.task_conversation(&task_id);
    let project: Option<PathBuf> = app
        .with_store(|s| s.get_task(&task_id))
        .flatten()
        .and_then(|t| t.project_cwd)
        .map(PathBuf::from)
        .filter(|p| p.is_dir());
    let selected = signal(Source::Project);
    let patch = signal(None::<Result<String, String>>);
    let problem = signal(None::<String>);
    let working = signal(false);
    let generation = stored(0u64);
    // Loads the chosen source again when it changes, a turn ends or a
    // subagent finishes.
    effect(move || {
        let source = selected.get();
        let busy = conversation.busy.get();
        conversation.subagents.with(|_| ());
        if busy && source == Source::Project {
            return;
        }
        let this = generation.with(|g| {
            *g += 1;
            *g
        });
        let project = project.clone();
        let children = conversation.children();
        spawn(async move {
            let loaded = background(async move {
                match source {
                    Source::Project => match project {
                        Some(dir) => worktree::working_changes(&dir).await,
                        None => Err("This task has no project folder.".into()),
                    },
                    Source::Worktree(id) => children.worktree_patch(&id).await,
                }
            })
            .await;
            if generation.with(|g| *g) == this {
                if let Ok(result) = loaded {
                    patch.set(Some(result));
                }
            }
        });
    });
    // A source that is gone (applied, thrown away) falls back to the project.
    effect(move || {
        let kept_ids: Vec<String> = conversation.subagents.with(|c| kept(c).into_iter().map(|c| c.id).collect());
        if let Source::Worktree(id) = selected.get_untracked() {
            if !kept_ids.contains(&id) {
                selected.set(Source::Project);
            }
        }
    });
    let act = move |apply: bool| {
        let Source::Worktree(id) = selected.get_untracked() else { return };
        let children = conversation.children();
        working.set(true);
        problem.set(None);
        spawn(async move {
            let done = background(async move {
                if apply { children.apply_worktree(&id).await } else { children.discard_worktree(&id).await }
            })
            .await;
            working.set(false);
            match done {
                Ok(Ok(())) => selected.set(Source::Project),
                Ok(Err(message)) => problem.set(Some(message)),
                Err(_) => {}
            }
        });
    };
    view()
        .w(px(520.0))
        .flex_shrink_0()
        .border_l(px(LINE))
        .border_color(hairline())
        .bg(pane())
        .child(
            view()
                .flex_row()
                .items_center()
                .gap(px(4.0))
                .px(px(12.0))
                .h(px(44.0))
                .border_b(px(LINE))
                .border_color(hairline())
                .child(tab("Project".into(), move || selected.get() == Source::Project, move || selected.set(Source::Project)))
                .child(dynamic(move || {
                    conversation
                        .subagents
                        .with(|c| kept(c))
                        .into_iter()
                        .map(|child| {
                            let id = child.id.clone();
                            let chosen_id = id.clone();
                            tab(
                                format!("{}'s worktree", child.nickname),
                                move || selected.get() == Source::Worktree(chosen_id.clone()),
                                move || selected.set(Source::Worktree(id.clone())),
                            )
                            .into_element()
                        })
                        .collect::<Vec<_>>()
                }))
                .child(view().flex_1())
                .child(when(move || matches!(selected.get(), Source::Worktree(_)), move || {
                    view()
                        .flex_row()
                        .gap(px(6.0))
                        .opacity(move || if working.get() { 0.4_f32 } else { 1.0 })
                        .child(button("Discard", move || act(false)))
                        .child(primary_button("Apply", move || act(true)))
                })),
        )
        .child(dynamic(move || {
            problem.get().map(|message| text(message).px(px(14.0)).py(px(8.0)).text_size(px(SMALL)).text_color(failed()))
        }))
        .child(dynamic(move || match patch.get() {
            None => text("Loading…").p(px(16.0)).text_size(px(BODY)).text_color(muted()).into_element(),
            Some(Err(message)) => text(message).p(px(16.0)).text_size(px(BODY)).text_color(muted()).into_element(),
            Some(Ok(patch)) if patch.trim().is_empty() => text("No changes since the last commit.")
                .p(px(16.0))
                .text_size(px(BODY))
                .text_color(muted())
                .into_element(),
            Some(Ok(patch)) => view()
                .flex_1()
                .min_h(px(0.0))
                .child(
                    nsh_base::diff_view()
                        .patch(patch)
                        .layout(DiffLayout::Unified)
                        .font_size(px(12.0))
                        .line_height(px(18.0))
                        .size_full(),
                )
                .into_element(),
        }))
}
