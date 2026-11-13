//! Settings: a dialog over whatever the window shows, its sections on the
//! left and each section's rows, grouped in cards, on the right. A choice
//! among a few is a dropdown: a button with its value and a small arrow,
//! and a menu whose chosen item is filled and checked.

use std::path::Path;
use std::rc::Rc;

use bubble_core::bubble_home;
use bubble_store::RunRole;
use nsh_core::prelude::*;

use crate::palette::*;
use crate::rules::{self, List, Rules};
use crate::state::{AppState, Route};
use crate::ui::{button, icon, project_name};

/// A part of Settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    General,
    Models,
    Permissions,
    Projects,
    Memory,
}

impl Section {
    const ALL: [Section; 5] = [Section::General, Section::Models, Section::Permissions, Section::Projects, Section::Memory];

    fn name(self) -> &'static str {
        match self {
            Section::General => "General",
            Section::Models => "Models",
            Section::Permissions => "Permissions",
            Section::Projects => "Projects",
            Section::Memory => "Memory",
        }
    }

    fn id(self) -> &'static str {
        match self {
            Section::General => "settings-general",
            Section::Models => "settings-models",
            Section::Permissions => "settings-permissions",
            Section::Projects => "settings-projects",
            Section::Memory => "settings-memory",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Section::General => GEAR_ICON,
            Section::Models => CHIP_ICON,
            Section::Permissions => SHIELD_ICON,
            Section::Projects => FOLDER_ICON,
            Section::Memory => BOOKMARK_ICON,
        }
    }
}

macro_rules! line_icon {
    ($($path:literal),+) => {
        concat!(
            r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round">"##,
            $(r##"<path d=""##, $path, r##""/>"##,)+
            "</svg>"
        )
    };
}

const GEAR_ICON: &str = line_icon!(
    "M10.325 4.317c.426 -1.756 2.924 -1.756 3.35 0a1.724 1.724 0 0 0 2.573 1.066c1.543 -.94 3.31 .826 2.37 2.37a1.724 1.724 0 0 0 1.065 2.572c1.756 .426 1.756 2.924 0 3.35a1.724 1.724 0 0 0 -1.066 2.573c.94 1.543 -.826 3.31 -2.37 2.37a1.724 1.724 0 0 0 -2.572 1.065c-.426 1.756 -2.924 1.756 -3.35 0a1.724 1.724 0 0 0 -2.573 -1.066c-1.543 .94 -3.31 -.826 -2.37 -2.37a1.724 1.724 0 0 0 -1.065 -2.572c-1.756 -.426 -1.756 -2.924 0 -3.35a1.724 1.724 0 0 0 1.066 -2.573c-.94 -1.543 .826 -3.31 2.37 -2.37c1 .608 2.296 .07 2.572 -1.065",
    "M9 12a3 3 0 1 0 6 0a3 3 0 0 0 -6 0"
);
const CHIP_ICON: &str = line_icon!(
    "M5 6a1 1 0 0 1 1 -1h12a1 1 0 0 1 1 1v12a1 1 0 0 1 -1 1h-12a1 1 0 0 1 -1 -1l0 -12",
    "M9 9h6v6h-6l0 -6",
    "M3 10h2",
    "M3 14h2",
    "M10 3v2",
    "M14 3v2",
    "M21 10h-2",
    "M21 14h-2",
    "M14 21v-2",
    "M10 21v-2"
);
const SHIELD_ICON: &str = line_icon!("M12 3a12 12 0 0 0 8.5 3a12 12 0 0 1 -8.5 15a12 12 0 0 1 -8.5 -15a12 12 0 0 0 8.5 -3");
const FOLDER_ICON: &str = line_icon!("M5 4h4l3 3h7a2 2 0 0 1 2 2v8a2 2 0 0 1 -2 2h-14a2 2 0 0 1 -2 -2v-11a2 2 0 0 1 2 -2");
const BOOKMARK_ICON: &str = line_icon!("M18 7v14l-6 -4l-6 4v-14a4 4 0 0 1 4 -4h4a4 4 0 0 1 4 4");
const CLOSE_ICON: &str = line_icon!("M18 6l-12 12", "M6 6l12 12");
const ARROW_ICON: &str = line_icon!("M6 9l6 6l6 -6");
const CHECK_ICON: &str = line_icon!("M5 12l5 5l10 -10");
const PLUS_ICON: &str = line_icon!("M12 5l0 14", "M5 12l14 0");
const MINUS_ICON: &str = line_icon!("M5 12l14 0");

// --- The dialog ------------------------------------------------------------------

/// Settings, over the window: a press beside it, its close button and
/// Escape take it away.
pub fn dialog(app: AppState) -> impl IntoElement {
    let close = move || app.settings.set(None);
    on_key("escape", move |_| close());
    view()
        .absolute()
        .inset(px(0.0))
        .items_center()
        .justify_center()
        .p(px(24.0))
        .window_drag(false)
        .child(view().absolute().inset(px(0.0)).bg(scrim()).window_drag(false).label("Close settings").test_id("settings-scrim").on_press(close))
        .child(
            view()
                .w(px(880.0))
                .max_w(percentage(1.0))
                .h(px(640.0))
                .max_h(percentage(1.0))
                .flex_row()
                .rounded(px(16.0))
                .bg(paper())
                .overflow_hidden()
                .shadow_lg()
                // A press on the dialog itself is not one beside it.
                .on_press(|| {})
                .child(sections(app))
                .child(
                    view()
                        .flex_1()
                        .min_w(px(0.0))
                        .child(
                            view()
                                .flex_row()
                                .items_center()
                                .flex_shrink_0()
                                .pt(px(24.0))
                                .pb(px(22.0))
                                .pl(px(32.0))
                                .pr(px(28.0))
                                .child(
                                    text(move || app.settings.get().map(Section::name).unwrap_or_default())
                                        .flex_1()
                                        .text_size(px(HEADING))
                                        .font_weight(FontWeight::SEMIBOLD),
                                )
                                .child(
                                    view()
                                        .size(px(28.0))
                                        .items_center()
                                        .justify_center()
                                        .rounded(px(8.0))
                                        .cursor(Cursor::Pointer)
                                        .hover(|s| s.bg(tertiary()))
                                        .label("Close")
                                        .test_id("settings-close")
                                        .on_press(close)
                                        .child(icon(CLOSE_ICON, 16.0).text_color(secondary())),
                                ),
                        )
                        .child(scroll().flex_1().min_h(px(0.0)).child(dynamic(move || {
                            let section = app.settings.get().unwrap_or(Section::General);
                            untracked(move || {
                                let page = match section {
                                    Section::General => general(app),
                                    Section::Models => models(app),
                                    Section::Permissions => permissions(app),
                                    Section::Projects => projects(app),
                                    Section::Memory => memory(app),
                                };
                                page.child(when(move || app.problem.get().is_some(), move || {
                                    text(move || app.problem.get().unwrap_or_default()).pl(px(4.0)).text_size(px(SMALL)).text_color(failed())
                                }))
                            })
                        }))),
                ),
        )
}

/// The sections, down the dialog's left side.
fn sections(app: AppState) -> impl IntoElement {
    view().w(px(200.0)).flex_shrink_0().bg(side()).border_r(px(LINE)).border_color(hairline()).px(px(10.0)).py(px(16.0)).gap(px(2.0)).children(
        Section::ALL
            .into_iter()
            .map(|section| {
                let current = move || app.settings.get() == Some(section);
                view()
                    .h(px(32.0))
                    .flex_row()
                    .items_center()
                    .gap(px(10.0))
                    .px(px(10.0))
                    .rounded(px(8.0))
                    .bg(move || if current() { item_active() } else { side() })
                    .cursor(Cursor::Pointer)
                    .hover(|s| s.bg(item_hover()))
                    .label(section.name())
                    .test_id(section.id())
                    .on_press(move || app.settings.set(Some(section)))
                    .child(icon(section.icon(), 16.0).text_color(move || if current() { ink() } else { secondary() }))
                    .child(dynamic(move || {
                        let on = current();
                        text(section.name())
                            .text_size(px(BODY))
                            .font_weight(if on { FontWeight::MEDIUM } else { FontWeight::NORMAL })
                            .text_color(if on { ink() } else { secondary() })
                    }))
                    .into_element()
            })
            .collect::<Vec<_>>(),
    )
}

// --- What the sections are made of ---------------------------------------------------

/// A section's groups, one under the other.
fn page() -> View {
    view().px(px(32.0)).pb(px(28.0)).gap(px(22.0))
}

/// Rows that belong together, under what they are.
fn group(name: &'static str, card: impl IntoElement) -> View {
    view().gap(px(8.0)).child(text(name).pl(px(4.0)).text_size(px(SMALL)).text_color(muted())).child(card)
}

/// A group whose name has an action beside it.
fn group_with(head: impl IntoElement, action: impl IntoElement, card: impl IntoElement) -> View {
    view().gap(px(8.0)).child(view().flex_row().items_end().gap(px(16.0)).child(view().flex_1().min_w(px(0.0)).pl(px(4.0)).child(head)).child(action)).child(card)
}

fn card() -> View {
    view().rounded(px(12.0)).border(px(LINE)).border_color(border()).py(px(6.0))
}

/// One setting: what it is, with a line about it, and its control after.
fn row(title: impl IntoReactive<SharedString>, note: Option<AnyElement>) -> View {
    view()
        .flex_row()
        .items_center()
        .gap(px(16.0))
        .min_h(px(44.0))
        .pl(px(16.0))
        .pr(px(14.0))
        .py(px(8.0))
        // What it is keeps its room: a long value after it gives way first.
        .child(view().flex_1().min_w(px(220.0)).child(text(title).text_size(px(BODY)).text_color(ink())).child(note))
}

fn note(words: impl IntoReactive<SharedString>) -> Option<AnyElement> {
    Some(text(words).text_size(px(SMALL)).text_color(muted()).into_element())
}

/// What a setting is now, where it is not changed here.
fn value(words: impl IntoReactive<SharedString>) -> Text {
    text(words).flex_shrink_0().text_size(px(BODY)).text_color(secondary())
}

/// A dot and a word: something is there and works.
fn working(words: impl IntoReactive<SharedString>) -> View {
    view().flex_row().items_center().flex_shrink_0().gap(px(7.0)).child(view().size(px(6.0)).rounded_full().bg(working_color())).child(value(words))
}

fn quiet_button(label: &str, on_press: impl Fn() + 'static) -> View {
    button(label, on_press).rounded(px(8.0)).bg(paper()).flex_shrink_0()
}

/// "+ Add …", beside a group's name.
fn add_button(label: &'static str, id: &'static str, on_press: impl Fn() + 'static) -> View {
    view()
        .h(px(28.0))
        .flex_row()
        .items_center()
        .flex_shrink_0()
        .gap(px(6.0))
        .pl(px(10.0))
        .pr(px(12.0))
        .rounded(px(8.0))
        .border(px(LINE))
        .border_color(border())
        .bg(paper())
        .cursor(Cursor::Pointer)
        .hover(|s| s.bg(tertiary()))
        .test_id(id)
        .on_press(on_press)
        .child(icon(PLUS_ICON, 14.0).text_color(secondary()))
        .child(text(label).text_size(px(BODY)).text_color(ink()))
}

/// One thing to pick in a dropdown.
pub struct Choice {
    name: String,
    /// A line under the name, where the name alone does not say enough.
    note: Option<&'static str>,
    /// What assistive tools (and tests) call it, when not its name.
    label: Option<String>,
    chosen: bool,
    pick: Rc<dyn Fn()>,
}

impl Choice {
    fn new(name: impl Into<String>, chosen: bool, pick: impl Fn() + 'static) -> Self {
        Choice { name: name.into(), note: None, label: None, chosen, pick: Rc::new(pick) }
    }

    fn note(mut self, note: &'static str) -> Self {
        self.note = Some(note);
        self
    }

    fn label(mut self, label: String) -> Self {
        self.label = Some(label);
        self
    }
}

/// A choice among a few: a button showing the one chosen, with a small
/// arrow, that opens a menu of them all, the chosen one filled and checked.
fn dropdown(label: impl Into<String>, shown: impl Fn() -> String + 'static, choices: impl Fn() -> Vec<Choice> + 'static) -> impl IntoElement {
    menu_of(label.into(), shown, choices, None)
}

/// A dropdown whose choices each have a line about them: its menu has one
/// width, which those lines wrap in.
fn described_dropdown(label: impl Into<String>, shown: impl Fn() -> String + 'static, choices: impl Fn() -> Vec<Choice> + 'static) -> impl IntoElement {
    menu_of(label.into(), shown, choices, Some(320.0))
}

fn menu_of(label: String, shown: impl Fn() -> String + 'static, choices: impl Fn() -> Vec<Choice> + 'static, width: Option<f32>) -> impl IntoElement {
    nsh_base::menu()
        .placement(Placement::Bottom)
        .align(Align::End)
        .offset(px(6.0))
        .popup_style(move |s| {
            let s = s.bg(paper()).border(px(LINE)).border_color(border()).rounded(px(12.0)).p(px(6.0)).gap(px(2.0)).shadow_lg();
            match width {
                Some(width) => s.w(px(width)),
                None => s.min_w(px(200.0)).max_w(px(420.0)),
            }
        })
        .trigger(
            view()
                .h(px(28.0))
                .flex_row()
                .items_center()
                .flex_shrink_0()
                .gap(px(4.0))
                .pl(px(12.0))
                .pr(px(8.0))
                .rounded(px(8.0))
                .border(px(LINE))
                .border_color(border())
                .bg(paper())
                .cursor(Cursor::Pointer)
                .hover(|s| s.bg(tertiary()))
                .label(label)
                .child(text(shown).text_size(px(BODY)).text_color(ink()).number_of_lines(1))
                .child(icon(ARROW_ICON, 14.0).text_color(muted())),
        )
        .child(dynamic(move || {
            choices()
                .into_iter()
                .map(|choice| {
                    let Choice { name, note, label, chosen, pick } = choice;
                    nsh_base::menu_item()
                        .accessibility_label(label.unwrap_or_else(|| name.clone()))
                        .rounded(px(8.0))
                        .cursor(Cursor::Pointer)
                        .hover(|s| s.bg(tertiary()))
                        .bg(if chosen { tertiary() } else { paper() })
                        .on_select(move || pick())
                        .child(
                            view()
                                .flex_row()
                                .items_center()
                                .gap(px(12.0))
                                .min_h(px(32.0))
                                .px(px(10.0))
                                .py(px(if note.is_some() { 6.0 } else { 0.0 }))
                                .child(
                                    // As wide as what it says, so the menu is too.
                                    view()
                                        .flex_auto()
                                        .min_w(px(0.0))
                                        .child(text(name).text_size(px(BODY)).text_color(ink()).number_of_lines(1))
                                        .child(note.map(|note| text(note).text_size(px(SMALL)).text_color(muted()))),
                                )
                                .child(chosen.then(|| icon(CHECK_ICON, 14.0).text_color(ink()))),
                        )
                        .into_element()
                })
                .collect::<Vec<_>>()
        }))
}

/// A small number, with less and more on its sides.
fn stepper(shown: impl Fn() -> String + 'static, fewer: &'static str, on_fewer: impl Fn() + 'static, more: &'static str, on_more: impl Fn() + 'static) -> View {
    let step = |markup: &'static str, label: &'static str, on_press: Box<dyn Fn()>| {
        view()
            .size(px(28.0))
            .items_center()
            .justify_center()
            .cursor(Cursor::Pointer)
            .hover(|s| s.bg(tertiary()))
            .label(label)
            .on_press(move || on_press())
            .child(icon(markup, 14.0).text_color(secondary()))
    };
    view()
        .flex_row()
        .items_center()
        .flex_shrink_0()
        .rounded(px(8.0))
        .border(px(LINE))
        .border_color(border())
        .overflow_hidden()
        .child(step(MINUS_ICON, fewer, Box::new(on_fewer)))
        .child(view().min_w(px(26.0)).items_center().child(text(shown).text_size(px(BODY)).text_color(ink())))
        .child(step(PLUS_ICON, more, Box::new(on_more)))
}

/// Shows a folder in the system's file manager.
fn show_folder(folder: &Path) {
    let _ = std::fs::create_dir_all(folder);
    run_opener(folder, false);
}

/// Opens a file to be read and changed, in the system's text editor.
fn open_file(file: &Path) {
    run_opener(file, true);
}

fn run_opener(path: &Path, as_text: bool) {
    let mut command = if cfg!(target_os = "macos") {
        let mut command = std::process::Command::new("open");
        if as_text {
            command.arg("-t");
        }
        command
    } else if cfg!(target_os = "windows") {
        std::process::Command::new("explorer")
    } else {
        std::process::Command::new("xdg-open")
    };
    if let Ok(mut child) = command.arg(path).spawn() {
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    }
}

/// A path as a person writes it: under their home, from `~`.
fn from_home(path: &Path) -> String {
    let shown = path.display().to_string();
    match std::env::var("HOME") {
        Ok(home) if !home.is_empty() && shown.starts_with(&home) => format!("~{}", &shown[home.len()..]),
        _ => shown,
    }
}

// --- General ---------------------------------------------------------------------

fn general(app: AppState) -> View {
    use nsh_core::platform::Appearance;
    const THEMES: [(&str, Appearance); 3] = [("As the system", Appearance::System), ("Light", Appearance::Light), ("Dark", Appearance::Dark)];
    let theme = dropdown(
        "Theme",
        move || {
            let now = app.appearance.get();
            THEMES.iter().find(|(_, appearance)| *appearance == now).map(|(name, _)| *name).unwrap_or("As the system").to_owned()
        },
        move || {
            let now = app.appearance.get();
            THEMES.iter().map(|(name, appearance)| Choice::new(*name, *appearance == now, move || app.set_appearance(*appearance))).collect()
        },
    );
    let home = bubble_home();
    let development = bubble_core::config::development_data();
    let version = format!("{}{}", env!("CARGO_PKG_VERSION"), if cfg!(debug_assertions) { ", development build" } else { "" });
    let shown_home = from_home(&home);
    page()
        .child(group("Appearance", card().child(row("Theme", None).child(theme))))
        .child(group(
            "Bots",
            card()
                .child(row("Bots working at once", note("Bots beyond this wait their turn")).child(stepper(
                    move || app.max_runs.get().to_string(),
                    "Fewer bots at once",
                    move || app.set_max_runs(app.max_runs.get_untracked().saturating_sub(1)),
                    "More bots at once",
                    move || app.set_max_runs(app.max_runs.get_untracked() + 1),
                )))
                .child(row("Bots of one role", note("When a bot is busy, new work goes to a copy of it, up to this many")).child(stepper(
                    move || {
                        app.task_revision.get();
                        app.max_role_bots().to_string()
                    },
                    "Fewer bots of one role",
                    move || app.set_max_role_bots(app.max_role_bots().saturating_sub(1)),
                    "More bots of one role",
                    move || app.set_max_role_bots(app.max_role_bots() + 1),
                ))),
        ))
        .child(group(
            "Data",
            card()
                .child(
                    row(
                        "Data folder",
                        note(if development {
                            "Development data, kept apart from your own in ~/.bubble"
                        } else {
                            "Your bots, their conversations and what they remember"
                        }),
                    )
                    // A long path is cut short; the folder itself is a press away.
                    .child(text(shown_home).flex_shrink(1.0).min_w(px(0.0)).number_of_lines(1).text_size(px(BODY)).text_color(secondary()))
                    .child(quiet_button(if cfg!(target_os = "macos") { "Show in Finder" } else { "Show folder" }, move || show_folder(&home))),
                )
                .child(row("Version", None).child(value(version))),
        ))
}

// --- Models ----------------------------------------------------------------------

/// The model a kind of bot runs on, picked from the configured ones; the
/// first choice follows config.json's default.
fn model_dropdown(app: AppState, role: RunRole, label: &'static str, chosen: Signal<Option<String>>) -> impl IntoElement {
    dropdown(
        label,
        move || chosen.get().or_else(|| app.default_model()).unwrap_or_else(|| "No model".to_owned()),
        move || {
            let now = chosen.get();
            let default = match app.default_model() {
                Some(default) => format!("Default ({default})"),
                None => "Default (none set in config.json)".to_owned(),
            };
            let mut choices = vec![Choice::new(default, now.is_none(), move || app.set_model_setting(role, None)).label(format!("{label}: default"))];
            let mut models = app.models.get();
            if let Some(current) = &now {
                if !models.contains(current) {
                    models.push(current.clone());
                }
            }
            for model in models {
                let picked = model.clone();
                let is = now.as_deref() == Some(model.as_str());
                choices.push(Choice::new(model.clone(), is, move || app.set_model_setting(role, Some(picked.clone()))).label(format!("{label}: {model}")));
            }
            choices
        },
    )
}

fn models(app: AppState) -> View {
    use bubble_core::Level;
    const LEVELS: [(&str, Level); 5] = [("Off", Level::Off), ("Low", Level::Low), ("Medium", Level::Medium), ("High", Level::High), ("Max", Level::Max)];
    let reasoning = dropdown(
        "Reasoning",
        move || {
            let now = app.thinking.get();
            LEVELS.iter().find(|(_, level)| *level == now).map(|(name, _)| *name).unwrap_or("Max").to_owned()
        },
        move || {
            let now = app.thinking.get();
            LEVELS.iter().map(|(name, level)| Choice::new(*name, *level == now, move || app.set_thinking(*level)).label(format!("Reasoning {name}"))).collect()
        },
    );
    let providers: Vec<String> = app.config.with(|config| config.providers.iter().filter(|p| p.enabled).map(|p| p.id.clone()).collect());
    let providers = match providers.as_slice() {
        [] => "None yet; add one in config.json".to_owned(),
        [one] => format!("{one}, from config.json"),
        [first @ .., last] => format!("{} and {last}, from config.json", first.join(", ")),
    };
    page()
        .child(group(
            "Defaults",
            card()
                .child(
                    row("Main bot's model", note("Runs the conversation with Bubble, which hands work to your bots"))
                        .child(model_dropdown(app, RunRole::Manager, "Main bot's model", app.manager_model)),
                )
                .child(row("Bots' model", note("Used by every bot unless you give one its own")).child(model_dropdown(app, RunRole::Task, "Bots' model", app.task_model)))
                .child(row("Reasoning", note("How hard models think; each uses the nearest level it offers")).child(reasoning)),
        ))
        .child(group(
            "Subscriptions",
            card().child(each(
                move || app.accounts.get(),
                |account: &crate::state::Account| account.subscription.label().to_owned(),
                move |account, _| {
                    let subscription = account.with_untracked(|a| a.subscription);
                    row(subscription.label(), note(move || account.get().status)).child(dynamic(move || {
                        if account.get().signed_in {
                            quiet_button("Sign out", move || app.sign_out(subscription)).into_element()
                        } else {
                            quiet_button("Sign in", move || app.sign_in(subscription)).into_element()
                        }
                    }))
                },
            )),
        ))
        .child(group(
            "API keys",
            card().child(row("Providers", note(providers)).child(quiet_button("Open config.json", || open_file(&bubble_home().join("config.json"))))),
        ))
}

// --- Permissions -----------------------------------------------------------------

fn permissions(app: AppState) -> View {
    let rules = signal(Rules::default());
    let problem = signal(None::<String>);
    match rules::load(&rules::path()) {
        Ok(found) => rules.set(found),
        Err(error) => problem.set(Some(error)),
    }
    let approvals = described_dropdown(
        "Approvals",
        move || if app.full_access.get() { "Full access" } else { "Ask first" }.to_owned(),
        move || {
            let full = app.full_access.get();
            vec![
                Choice::new("Ask first", !full, move || app.set_full_access(false)).note("Changes and commands wait for your yes"),
                Choice::new("Full access", full, move || app.set_full_access(true)).note("Changes and commands go ahead"),
            ]
        },
    );
    page()
        .child(card().child(row("Approvals", note("Applies to every bot. Reading never asks.")).child(approvals)))
        .child(rule_list(List::Allow, "Allow without asking", "Commands that start with one of these go ahead", "git status", "allow", rules, problem))
        .child(rule_list(List::Deny, "Never allow", "Refused even with full access", "git push --force", "deny", rules, problem))
        .child(dynamic(move || problem.get().map(|message| text(message).pl(px(4.0)).text_size(px(SMALL)).text_color(failed()))))
}

/// One of the two lists of rules: its rules, each with a way to take it
/// off, and a way to add one.
fn rule_list(
    list: List,
    name: &'static str,
    about: &'static str,
    example: &'static str,
    id: &'static str,
    rules: Signal<Rules>,
    problem: Signal<Option<String>>,
) -> View {
    let adding = signal(false);
    let draft = signal(String::new());
    let took = move |result: Result<Rules, String>| match result {
        Ok(now) => {
            rules.set(now);
            problem.set(None);
            true
        }
        Err(error) => {
            problem.set(Some(error));
            false
        }
    };
    let add = Rc::new(move || {
        if took(rules::add(&rules::path(), list, &draft.get_untracked())) {
            draft.set(String::new());
            adding.set(false);
        }
    });
    let cancel = move || {
        draft.set(String::new());
        adding.set(false);
        problem.set(None);
    };
    let (add_id, input_id): (&'static str, &'static str) = match list {
        List::Allow => ("add-allow-rule", "allow-rule"),
        List::Deny => ("add-deny-rule", "deny-rule"),
    };
    group_with(
        view().child(text(name).text_size(px(BODY)).text_color(ink())).child(text(about).text_size(px(SMALL)).text_color(muted())),
        add_button("Add a rule", add_id, move || adding.set(true)),
        card()
            .child(when(move || adding.get(), {
                let add = add.clone();
                move || {
                    let (on_enter, on_press) = (add.clone(), add.clone());
                    view()
                        .flex_row()
                        .items_center()
                        .gap(px(8.0))
                        .min_h(px(44.0))
                        .pl(px(12.0))
                        .pr(px(14.0))
                        .py(px(8.0))
                        .child(
                            nsh_base::input()
                                .value(draft)
                                .placeholder(example.to_owned())
                                .placeholder_color(move || color(muted()))
                                .auto_focus(true)
                                .flex_1()
                                .h(px(28.0))
                                .px(px(10.0))
                                .rounded(px(8.0))
                                .border(px(LINE))
                                .border_color(border())
                                .bg(paper())
                                .text_size(px(BODY))
                                .text_color(ink())
                                .test_id(input_id)
                                .on_change(move |v: &str| draft.set(v.to_owned()))
                                .on_submit(move |_: &str| on_enter()),
                        )
                        .child(quiet_button("Add", move || on_press()).label(format!("Add to {id}")))
                        .child(quiet_button("Cancel", cancel).label(format!("Cancel adding to {id}")))
                }
            }))
            .child(when(move || !adding.get() && rules.with(|r| r.of(list).is_empty()), || {
                view().min_h(px(44.0)).justify_center().px(px(16.0)).child(text("No rules yet").text_size(px(BODY)).text_color(muted()))
            }))
            .child(each(
                move || rules.with(|r| r.of(list).to_vec()),
                |rule: &String| rule.clone(),
                move |rule, _| {
                    let written = rule.get_untracked();
                    let shown = rules::shown(&written);
                    view()
                        .flex_row()
                        .items_center()
                        .gap(px(12.0))
                        .min_h(px(44.0))
                        .pl(px(16.0))
                        .pr(px(14.0))
                        .child(text(shown.clone()).flex_1().text_size(px(BODY)).text_color(ink()))
                        .child(
                            view()
                                .size(px(28.0))
                                .items_center()
                                .justify_center()
                                .rounded(px(8.0))
                                .cursor(Cursor::Pointer)
                                .hover(|s| s.bg(tertiary()))
                                .label(format!("Remove {shown}"))
                                .on_press(move || {
                                    took(rules::remove(&rules::path(), list, &written));
                                })
                                .child(icon(CLOSE_ICON, 14.0).text_color(muted())),
                        )
                },
            )),
    )
}

// --- Projects --------------------------------------------------------------------

fn projects(app: AppState) -> View {
    use crate::github::Level;
    const LEVELS: [Option<Level>; 4] = [None, Some(Level::Observe), Some(Level::Prepare), Some(Level::Act)];
    let add = move || {
        let picked = pick_paths(nsh_core::file_dialog::OpenDialog::folders().button_label("Add project"));
        spawn(async move {
            let Ok(Some(paths)) = picked.await else { return };
            let Some(folder) = paths.into_iter().next().map(|p| p.display().to_string()) else { return };
            app.remember_project(folder.trim_end_matches('/'));
        });
    };
    // "Signed in with GitHub CLI as x", or why it cannot be reached.
    let signed_in = move || app.github_status.get().and_then(|status| status.strip_prefix("Signed in with GitHub CLI as ").map(str::to_owned));
    let github_note = move || match app.github_status.get() {
        Some(status) if !status.starts_with("Signed in") => status,
        _ => "Bubble reads issues, pull requests and your notifications through it".to_owned(),
    };
    page()
        .child(group(
            "GitHub",
            card().child(row("GitHub CLI", note(github_note)).child(dynamic(move || match (signed_in(), app.github_status.get()) {
                (Some(login), _) => working(format!("Signed in as {login}")).into_element(),
                (None, None) => value("Looking for it…").into_element(),
                (None, Some(_)) => view().into_element(),
            }))),
        ))
        .child(group_with(
            text("Folders your bots work in").text_size(px(SMALL)).text_color(muted()),
            add_button("Add a project folder", "add-project", add),
            card()
                .child(when(move || app.project_folders().is_empty(), || {
                    view()
                        .min_h(px(44.0))
                        .justify_center()
                        .px(px(16.0))
                        .child(text("None yet. Add one, or have a bot work in one.").text_size(px(BODY)).text_color(muted()))
                }))
                .child(each(
                    move || app.project_folders(),
                    |folder: &crate::state::ProjectFolder| folder.path.clone(),
                    move |folder, _| {
                        let path = folder.with_untracked(|f| f.path.clone());
                        row(
                            project_name(&path),
                            note(move || folder.get().repo.unwrap_or_else(|| "Not a GitHub repository, so there is nothing to watch".to_owned())),
                        )
                        .child(dynamic(move || {
                            let repo = folder.get().repo?;
                            let (for_label, for_choices) = (repo.clone(), repo.clone());
                            Some(described_dropdown(
                                format!("Level for {for_label}"),
                                move || Level::label(folder.get().level).to_owned(),
                                move || {
                                    let now = folder.get().level;
                                    LEVELS
                                        .iter()
                                        .map(|level| {
                                            let (level, repo) = (*level, for_choices.clone());
                                            let label = format!("{} for {repo}", Level::label(level));
                                            Choice::new(Level::label(level), level == now, move || app.set_repo_level(&repo, level)).note(Level::hint(level)).label(label)
                                        })
                                        .collect()
                                },
                            ))
                        }))
                    },
                )),
        ))
        .child(group(
            "Pull requests",
            card().child(row("Open pull requests", note("Those linked to bots, and those open in the repositories you watch")).child(
                quiet_button("Open", move || {
                    app.settings.set(None);
                    app.route.set(Route::PullRequests);
                })
                .test_id("nav-pull-requests"),
            )),
        ))
}

// --- Memory ----------------------------------------------------------------------

/// What the bots keep across conversations: the notes every bot reads,
/// then each project's and each bot's own, where there are any.
fn memory(app: AppState) -> View {
    use crate::memory::{Scope, entries};
    let problem = signal(None::<String>);
    // Who else has notes: the projects, then the bots.
    let others = move || {
        app.memory_revision.get();
        let mut others: Vec<(Scope, String, &'static str)> = Vec::new();
        for folder in app.projects() {
            let scope = Scope::Project(folder.clone());
            if !entries(&scope).is_empty() {
                others.push((scope, project_name(&folder), "Read by whichever bot works in this project"));
            }
        }
        for bot in app.bots() {
            let scope = Scope::Bot(bot.task.id.clone());
            if !entries(&scope).is_empty() {
                others.push((scope, bot.task.title.clone(), "Its own"));
            }
        }
        others
    };
    page()
        .child(card().child(
            row("Notes", note("What your bots keep across conversations, as plain files"))
                .child(quiet_button(if cfg!(target_os = "macos") { "Show in Finder" } else { "Show folder" }, || show_folder(&crate::memory::root()))),
        ))
        .child(note_list(app, Scope::Team, "Every bot".to_owned(), "Read by all of them", true, problem))
        .child(each(
            others,
            |(scope, name, _): &(Scope, String, &'static str)| format!("{scope:?}:{name}"),
            move |other, _| {
                let (scope, name, about) = other.get_untracked();
                note_list(app, scope, name, about, false, problem)
            },
        ))
        .child(dynamic(move || problem.get().map(|message| text(message).pl(px(4.0)).text_size(px(SMALL)).text_color(failed()))))
}

/// The notes of one reader: each with a way to take it off, and, for the
/// notes every bot reads, a way to add one.
fn note_list(app: AppState, scope: crate::memory::Scope, name: String, about: &'static str, addable: bool, problem: Signal<Option<String>>) -> View {
    use crate::memory::{entries, forget, remember};
    let adding = signal(false);
    let draft = signal(String::new());
    let scope = stored(scope);
    let notes = move || {
        app.memory_revision.get();
        entries(&scope.get()).into_iter().enumerate().collect::<Vec<_>>()
    };
    let changed = move || app.memory_revision.update(|revision| *revision += 1);
    let add = Rc::new(move || match remember(&scope.get(), &draft.get_untracked()) {
        Ok(_) => {
            draft.set(String::new());
            adding.set(false);
            problem.set(None);
            changed();
        }
        Err(message) => problem.set(Some(message)),
    });
    let cancel = move || {
        draft.set(String::new());
        adding.set(false);
        problem.set(None);
    };
    let action = if addable { add_button("Add a note", "add-note", move || adding.set(true)).into_element() } else { view().into_element() };
    group_with(
        view().child(text(name).text_size(px(BODY)).text_color(ink())).child(text(about).text_size(px(SMALL)).text_color(muted())),
        action,
        card()
            .child(when(move || adding.get(), {
                let add = add.clone();
                move || {
                    let (on_enter, on_press) = (add.clone(), add.clone());
                    view()
                        .flex_row()
                        .items_center()
                        .gap(px(8.0))
                        .min_h(px(44.0))
                        .pl(px(12.0))
                        .pr(px(14.0))
                        .py(px(8.0))
                        .child(
                            nsh_base::input()
                                .value(draft)
                                .placeholder("Something every bot should remember".to_owned())
                                .placeholder_color(move || color(muted()))
                                .auto_focus(true)
                                .flex_1()
                                .h(px(28.0))
                                .px(px(10.0))
                                .rounded(px(8.0))
                                .border(px(LINE))
                                .border_color(border())
                                .bg(paper())
                                .text_size(px(BODY))
                                .text_color(ink())
                                .test_id("note")
                                .on_change(move |v: &str| draft.set(v.to_owned()))
                                .on_submit(move |_: &str| on_enter()),
                        )
                        .child(quiet_button("Add", move || on_press()).label("Add the note"))
                        .child(quiet_button("Cancel", cancel).label("Cancel adding the note"))
                }
            }))
            .child(when(move || !adding.get() && notes().is_empty(), || {
                view()
                    .min_h(px(44.0))
                    .justify_center()
                    .px(px(16.0))
                    .child(text("Nothing yet. Tell a bot to remember something for every bot, or add it here.").text_size(px(BODY)).text_color(muted()))
            }))
            .child(each(
                notes,
                |(index, note): &(usize, String)| format!("{index}:{note}"),
                move |entry, _| {
                    let note = entry.with_untracked(|(_, note)| note.clone());
                    let removed = note.clone();
                    view()
                        .flex_row()
                        .items_center()
                        .gap(px(12.0))
                        .min_h(px(44.0))
                        .pl(px(16.0))
                        .pr(px(14.0))
                        .py(px(8.0))
                        .child(text(note.clone()).flex_1().text_size(px(BODY)).text_color(ink()))
                        .child(
                            view()
                                .size(px(28.0))
                                .flex_shrink_0()
                                .items_center()
                                .justify_center()
                                .rounded(px(8.0))
                                .cursor(Cursor::Pointer)
                                .hover(|s| s.bg(tertiary()))
                                .label(format!("Remove note: {note}"))
                                .on_press(move || {
                                    if let Err(message) = forget(&scope.get(), &removed) {
                                        problem.set(Some(message));
                                    }
                                    changed();
                                })
                                .child(icon(CLOSE_ICON, 14.0).text_color(muted())),
                        )
                },
            )),
    )
}
