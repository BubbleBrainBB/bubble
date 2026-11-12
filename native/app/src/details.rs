//! A bot's details, beside its conversation: who it is (its face, name
//! and what it is for), where it works, the routines it runs, and the work
//! it was given by others, so that why it did something can be read back.

use std::rc::Rc;

use bubble_store::{Ticket, TicketOrigin, TicketStatus};
use nsh_core::prelude::*;

use crate::avatar;
use crate::palette::*;
use crate::state::{AppState, Route};
use crate::ui::{button, primary_button, project_name};

pub const WIDTH: f32 = 340.0;

fn heading(title: &'static str) -> Text {
    text(title).text_size(px(BODY)).font_weight(FontWeight::MEDIUM).text_color(ink())
}

fn input(value: Signal<String>, placeholder: &'static str, id: &'static str) -> nsh_base::Input {
    nsh_base::input()
        .value(value)
        .placeholder(placeholder.to_owned())
        .placeholder_color(move || color(muted()))
        .px(px(10.0))
        .py(px(7.0))
        .rounded(px(8.0))
        .border(px(LINE))
        .border_color(border())
        .bg(paper())
        .text_size(px(BODY))
        .text_color(ink())
        .test_id(id)
        .on_change(move |v: &str| value.set(v.to_owned()))
}

/// Who gave a bot a piece of work, and how it stands, in a line.
fn ticket_line(app: AppState, ticket: &Ticket) -> String {
    let from = match ticket.origin {
        TicketOrigin::Main => format!("From {}", crate::manager::MAIN_BOT),
        TicketOrigin::Bot => {
            let name = ticket.asker.as_deref().map(|id| crate::manager::Sender::Bot(id.to_owned()).name(app));
            format!("From {}", name.unwrap_or_else(|| "a bot".into()))
        }
        TicketOrigin::Github => format!("From GitHub{}", ticket.asker.as_deref().map(|a| format!(", {a}")).unwrap_or_default()),
        TicketOrigin::Routine => "From its routine".to_owned(),
    };
    let how = match ticket.status {
        TicketStatus::Proposed => "waits for your yes",
        TicketStatus::Recorded => "only written down",
        TicketStatus::Accepted => "being worked on",
        TicketStatus::Finished => "finished",
        TicketStatus::Failed => "failed",
        TicketStatus::Stopped => "stopped",
        TicketStatus::Dismissed => "dismissed",
    };
    // Whether someone asked outright, or it was read into an event.
    let grounds = if ticket.explicit { "" } else { ", inferred" };
    format!("{from}{grounds}; {how}")
}

pub fn panel(app: AppState, bot_id: String) -> impl IntoElement {
    let bot = app.with_store(|s| s.get_task(&bot_id)).flatten();
    let name = signal(bot.as_ref().map(|b| b.title.clone()).unwrap_or_default());
    let purpose = signal(app.bot_instruction(&bot_id));
    let saved = signal(false);
    let id = stored(bot_id.clone());

    let save = Rc::new(move || {
        let chosen = name.get_untracked();
        if !chosen.trim().is_empty() {
            app.rename_bot(&id.get(), chosen.trim());
        }
        app.set_bot_instruction(&id.get(), &purpose.get_untracked());
        saved.set(true);
    });
    let on_enter = save.clone();

    let folder = move || {
        app.task_revision.get();
        app.tasks.with(|tasks| tasks.iter().find(|t| t.task.id == id.get()).and_then(|t| t.task.project_cwd.clone()))
    };
    let choose_folder = move || {
        let picked = pick_paths(nsh_core::file_dialog::OpenDialog::folders().button_label("Use folder"));
        spawn(async move {
            if let Ok(Some(paths)) = picked.await {
                if let Some(path) = paths.into_iter().next() {
                    app.move_bot(&id.get(), Some(&path.display().to_string()));
                }
            }
        });
    };

    let tickets = move || {
        // Read again when work comes in, is decided or ends.
        app.tickets_revision.get();
        app.with_store(|s| s.tickets_for_task(&id.get(), 8)).unwrap_or_default()
    };

    view()
        .w(px(WIDTH))
        .flex_shrink_0()
        .border_l(px(LINE))
        .border_color(hairline())
        .bg(pane())
        .test_id("bot-details")
        .child(
            scroll().flex_1().child(
                view()
                    .px(px(18.0))
                    .pt(px(8.0))
                    .pb(px(20.0))
                    .gap(px(20.0))
                    // Its face, and the looks to choose from.
                    .child(
                        view()
                            .items_center()
                            .gap(px(12.0))
                            .child(avatar::bot(app, &bot_id, 72.0))
                            .child(
                                view()
                                    .flex_row()
                                    .gap(px(8.0))
                                    .child(button("Another shape", move || app.cycle_face(&id.get(), true)).label("Another shape"))
                                    .child(button("Another color", move || app.cycle_face(&id.get(), false)).label("Another color")),
                            ),
                    )
                    .child(
                        view()
                            .gap(px(8.0))
                            .child(heading("Name"))
                            .child(input(name, "What you call it", "details-name").h(px(34.0)).on_submit(move |_: &str| on_enter()))
                            .child(heading("What it is for"))
                            .child(
                                input(purpose, "Its standing instructions, in your words", "details-purpose").multiline(true).min_h(px(96.0)),
                            )
                            .child(
                                view()
                                    .flex_row()
                                    .items_center()
                                    .gap(px(10.0))
                                    .child(primary_button("Save", move || save()))
                                    .child(when(move || saved.get(), || text("Saved").text_size(px(SMALL)).text_color(muted()))),
                            ),
                    )
                    .child(
                        view()
                            .gap(px(8.0))
                            .child(heading("Folder"))
                            .child(
                                text(move || match folder() {
                                    Some(cwd) => cwd,
                                    None => "No folder: it only talks".to_owned(),
                                })
                                .text_size(px(SMALL))
                                .text_color(muted())
                                .number_of_lines(2),
                            )
                            .child(
                                view()
                                    .flex_row()
                                    .gap(px(8.0))
                                    .child(button("Choose a folder", choose_folder))
                                    .child(when(move || folder().is_some(), move || button("Work without a folder", move || app.move_bot(&id.get(), None)))),
                            )
                            .child(
                                text("After a move its conversation goes on in a fresh part that starts in the new folder.")
                                    .text_size(px(SMALL))
                                    .text_color(muted()),
                            ),
                    )
                    .child(view().gap(px(8.0)).child(heading("Routines")).child(crate::automations::section(app, bot_id.clone())))
                    .child(view().gap(px(8.0)).child(heading("Memory")).child(crate::memory::section(app, bot_id.clone())))
                    .child(
                        view()
                            .gap(px(8.0))
                            .child(heading("Work it was given"))
                            .child(when(move || tickets().is_empty(), || {
                                text("Nothing yet. What the main bot, other bots, GitHub and its routines bring it shows here.")
                                    .text_size(px(SMALL))
                                    .text_color(muted())
                            }))
                            .child(each(tickets, |ticket: &Ticket| format!("{}:{}", ticket.id, ticket.status.as_str()), move |ticket, _| {
                                view()
                                    .py(px(6.0))
                                    .gap(px(2.0))
                                    .border_b(px(LINE))
                                    .border_color(hairline())
                                    .child(text(move || ticket.get().title).text_size(px(BODY)).text_color(ink()).number_of_lines(1))
                                    .child(
                                        text(move || ticket_line(app, &ticket.get())).text_size(px(SMALL)).text_color(muted()).number_of_lines(1),
                                    )
                            })),
                    )
                    .child(
                        view().gap(px(8.0)).child(heading("Archive")).child(
                            view().flex_row().child(button("Archive this bot", move || {
                                app.cancel_task(&id.get());
                                app.details_open.set(false);
                                app.route.set(Route::Home);
                            })),
                        ),
                    )
                    .child(
                        text(move || match folder() {
                            Some(cwd) => format!("Works in {}", project_name(&cwd)),
                            None => String::new(),
                        })
                        .text_size(px(SMALL))
                        .text_color(faint()),
                    ),
            ),
        )
}
