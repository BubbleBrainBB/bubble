//! Bubble Native: the window, laid out as a messenger is: the bots on the
//! left, and the conversation with the one that is open (or a page) beside
//! them.

mod automations;
mod avatar;
mod changes;
mod details;
mod engine;
mod github;
mod manager;
mod memory;
mod pages;
mod palette;
mod prs;
mod sidebar;
mod state;
mod trace;
mod transcript;
mod triage;
mod ui;

use nsh_core::element::desktop::{set_title_bar, title_bar};
use nsh_core::prelude::*;

use palette::*;
pub use state::{AppState, Route};

/// What a TSX entry may import from `bubble/native` (none uses it yet).
#[native_module("bubble/native")]
fn native(registry: &mut Registry) {
    registry.register::<App>();
}

/// The app's root.
#[component]
pub fn App() -> impl IntoElement {
    let app = AppState::load();
    provide_context(app);
    set_color_scheme(app.appearance.get_untracked());
    set_title_bar(nsh_core::desktop::TitleBarStyle::Hidden, Some(px(TOP_BAR)));
    view()
        .flex_1()
        .flex_row()
        .bg(pane())
        .text_color(ink())
        .child(sidebar::sidebar(app))
        .child(
            view()
                .flex_1()
                .min_w(px(0.0))
                .child(
                    // Who the conversation is with, over it, and its actions.
                    view()
                        .h(px(TOP_BAR))
                        .flex_shrink_0()
                        .flex_row()
                        .items_center()
                        .window_drag(true)
                        .pl(px(16.0))
                        .pr(move || title_bar().right + px(14.0))
                        .child(dynamic(move || {
                            let route = app.route.get();
                            untracked(move || pages::title_bar(app, route))
                        })),
                )
                .child(view().flex_1().min_h(px(0.0)).child(dynamic(move || {
                    let route = app.route.get();
                    untracked(move || match route {
                        Route::Home => pages::home(app).into_element(),
                        Route::Tasks => pages::tasks(app).into_element(),
                        Route::Task(id) => pages::task(app, id).into_element(),
                        Route::NewBot => pages::new_bot(app).into_element(),
                        Route::PullRequests => prs::page(app).into_element(),
                        Route::Skills => pages::skills(app).into_element(),
                        Route::Search => pages::search(app).into_element(),
                        Route::Settings => pages::settings(app).into_element(),
                    })
                }))),
        )
}
