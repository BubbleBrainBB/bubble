//! A small NSH app on a virtual terminal: drawn, typed into, clicked.

use std::time::Duration;

use bubble_term::Virtual;
use nsh_core::prelude::*;
use nsh_engine::{EngineConfig, Root};

#[component]
fn Demo() -> impl IntoElement {
    let draft = signal(String::new());
    let sent = signal(Vec::<String>::new());
    let count = signal(0);
    on_key("ctrl+k", move |_| count.update(|c| *c += 1));
    view()
        .flex_1()
        .child(
            view()
                .flex_row()
                .gap(px(8.0))
                .child(text("Hello terminal").font_weight(FontWeight::BOLD))
                .child(text(move || format!("keys {}", count.get()))),
        )
        .child(
            scroll().flex_1().child(view().child(each(
                move || sent.get().into_iter().enumerate().collect::<Vec<_>>(),
                |(i, _): &(usize, String)| *i,
                |item, _| text(move || format!("> {}", item.get().1)),
            ))),
        )
        .child(
            view()
                .border(px(1.0))
                .rounded(px(4.0))
                .px(px(8.0))
                .py(px(16.0))
                .child(
                    nsh_base_free_input(draft, sent),
                ),
        )
        .child(view().on_press(move || sent.update(|s| s.push("pressed".into()))).child(text("[ Press me ]")))
}

fn nsh_base_free_input(draft: Signal<String>, sent: Signal<Vec<String>>) -> impl IntoElement {
    nsh_base::input()
        .value(draft)
        .placeholder("Say something")
        .multiline(true)
        .submit_behavior(SubmitBehavior::Submit)
        .auto_focus(true)
        .on_change(move |v: &str| draft.set(v.to_owned()))
        .on_submit(move |_: &str| {
            let text = draft.get_untracked();
            sent.update(|s| s.push(text));
            draft.set(String::new());
        })
}

#[test]
fn an_app_draws_types_and_clicks() {
    let mut term = Virtual::start(EngineConfig::new(Root::of::<Demo>()), 40, 12).unwrap();
    term.wait_for("Hello terminal", Duration::from_secs(5)).unwrap();
    println!("{}\n----\n{}", term.text(), term.tree.dump());
    assert!(term.text().contains("Say something"));
    term.type_text("hi there");
    term.wait_for("hi there", Duration::from_secs(2)).unwrap();
    term.key("enter");
    term.wait_for("> hi there", Duration::from_secs(2)).unwrap();
    assert!(!term.text().contains("Say something") || term.text().contains("> hi there"));
    term.key("ctrl+k");
    term.wait_for("keys 1", Duration::from_secs(2)).unwrap();
    term.click("Press me").unwrap();
    term.wait_for("> pressed", Duration::from_secs(2)).unwrap();
    println!("{}", term.text());
}
