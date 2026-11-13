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
mod rules;
mod settings;
mod sidebar;
mod state;
mod trace;
mod transcript;
mod triage;
mod ui;
mod worktree;

use nsh_core::element::desktop::{set_title_bar, title_bar};
use nsh_core::prelude::*;

use palette::*;
pub use state::{AppState, Route};

/// What a TSX entry may import from `bubble/native` (none uses it yet).
#[native_module("bubble/native")]
fn native(registry: &mut Registry) {
    registry.register::<App>();
}

/// Scroll bars are the app's own: a thin thumb at the edge that shows while
/// something scrolls and goes when it stops, in place of the system's bar,
/// which is wide wherever the system is set to always show it.
fn thin_scroll_bars() {
    use nsh_base::{ScrollbarLook, ScrollbarMode, ScrollbarMotion, ScrollbarTheme, set_scrollbar_theme};
    use nsh_core::element::{Style, StyleRefinement};
    let mut look = ScrollbarLook::default();
    look.style = Style::from(StyleRefinement::default().absolute().top(px(0.0)).bottom(px(0.0)).right(px(5.0)).w(px(4.0)).py(px(10.0)));
    look.thumb_style = Style::from(StyleRefinement::default().rounded_full().bg(scroll_thumb()));
    let mut motion = ScrollbarMotion::default();
    motion.idle = 1200.0;
    motion.enter = 120.0;
    motion.exit = 300.0;
    let mut theme = ScrollbarTheme::default();
    theme.mode = ScrollbarMode::Scrolling;
    theme.motion = motion;
    theme.min_thumb_length = px(32.0);
    theme.vertical = Some(look);
    set_scrollbar_theme(Some(theme));
}

/// The app's root.
#[component]
pub fn App() -> impl IntoElement {
    let app = AppState::load();
    provide_context(app);
    set_color_scheme(app.appearance.get_untracked());
    thin_scroll_bars();
    set_title_bar(nsh_core::desktop::TitleBarStyle::Hidden, Some(px(TOP_BAR)));
    view()
        .flex_1()
        .flex_row()
        .bg(pane())
        .text_color(ink())
        // Where the pointer is, for the faces that look at it.
        .on_mouse_move(move |event: &nsh_core::MouseEvent| app.pointer.set(Some((event.page_position.x.0, event.page_position.y.0))))
        .on_mouse_leave(move |_: &nsh_core::MouseEvent| app.pointer.set(None))
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
                        Route::Search => pages::search(app).into_element(),
                    })
                }))),
        )
        // Settings, over all of it.
        .child(when(move || app.settings.get().is_some(), move || settings::dialog(app)))
}
