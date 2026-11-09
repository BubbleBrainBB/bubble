//! Bubble Native: the window, laid out as the Electron Bubble app is: a top
//! strip and an icon rail on the window chrome, and one raised card holding
//! the sidebar and the screen the route shows.

mod github;
mod manager;
mod pages;
mod palette;
mod sidebar;
mod state;
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
    set_title_bar(nsh_core::desktop::TitleBarStyle::Hidden, Some(px(TOP_BAR)));
    view()
        .flex_1()
        .bg(chrome())
        .text_color(ink())
        .child(
            // The top strip: window controls, then the screen's title over the
            // content pane, and its actions.
            view()
                .h(px(TOP_BAR))
                .flex_shrink_0()
                .flex_row()
                .items_center()
                .window_drag(true)
                .child(view().w(px(RAIL_WIDTH + SIDEBAR_WIDTH)).flex_shrink_0())
                .child(
                    view()
                        .flex_1()
                        .min_w(px(0.0))
                        .flex_row()
                        .items_center()
                        .gap(px(8.0))
                        .pl(px(16.0))
                        .pr(move || title_bar().right + px(12.0))
                        .child(dynamic(move || {
                            let route = app.route.get();
                            untracked(move || pages::title_bar(app, route))
                        })),
                ),
        )
        .child(
            view()
                .flex_1()
                .flex_row()
                .min_h(px(0.0))
                .child(sidebar::rail(app))
                .child(
                    view()
                        .flex_1()
                        .flex_row()
                        .min_w(px(0.0))
                        .mr(px(8.0))
                        .mb(px(8.0))
                        .rounded(px(10.0))
                        .border(px(LINE))
                        .border_color(hairline())
                        .overflow_hidden()
                        .child(sidebar::sidebar(app))
                        .child(
                            view()
                                .flex_1()
                                .min_w(px(0.0))
                                .bg(pane())
                                .border_l(px(LINE))
                                .border_color(hairline())
                                .child(dynamic(move || {
                                    let route = app.route.get();
                                    untracked(move || match route {
                                        Route::Home => pages::home(app).into_element(),
                                        Route::Inbox => pages::inbox(app).into_element(),
                                        Route::Tasks => pages::tasks(app).into_element(),
                                        Route::Task(id) => pages::task(app, id).into_element(),
                                        Route::Settings => pages::settings(app).into_element(),
                                    })
                                })),
                        ),
                ),
        )
}
