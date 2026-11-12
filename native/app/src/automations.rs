//! Routines: recurring work a bot does on a schedule (each day at a time,
//! each week on a day, or every so many minutes). A routine belongs to a
//! bot; when it is due its prompt goes to that bot's conversation, as a
//! piece of work the person asked for when the routine was written. They
//! are kept in the store's settings; a loop looks every half minute. A bot
//! can also set its own up when asked in chat.

use std::time::Duration;

use chrono::{Datelike, Local, NaiveTime, TimeZone};
use nsh_core::prelude::*;
use serde::{Deserialize, Serialize};

use crate::palette::*;
use bubble_core::{ToolCall, ToolOutput, ToolSpec};
use serde_json::json;

use crate::state::AppState;
use crate::ui::*;

/// The settings key the list is kept under (JSON).
pub const KEY: &str = "automations";

const DAYS: [&str; 7] = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Schedule {
    /// Each day at `time` (`HH:MM`, local).
    Daily { time: String },
    /// Each week on `day` (0 is Monday) at `time`.
    Weekly { day: u32, time: String },
    /// Every `minutes` minutes.
    Every { minutes: u32 },
}

impl Schedule {
    pub fn describe(&self) -> String {
        match self {
            Schedule::Daily { time } => format!("Every day at {time}"),
            Schedule::Weekly { day, time } => format!("Every {} at {time}", DAYS[*day as usize % 7]),
            Schedule::Every { minutes: 1 } => "Every minute".into(),
            Schedule::Every { minutes } if minutes % 60 == 0 => match minutes / 60 {
                1 => "Every hour".into(),
                hours => format!("Every {hours} hours"),
            },
            Schedule::Every { minutes } => format!("Every {minutes} minutes"),
        }
    }

    /// The first time it fires after `after` (ms since the epoch).
    pub fn next_after(&self, after: i64) -> Option<i64> {
        let at_time = |date: chrono::NaiveDate, time: &str| {
            let time = NaiveTime::parse_from_str(time, "%H:%M").ok()?;
            Local.from_local_datetime(&date.and_time(time)).earliest().map(|t| t.timestamp_millis())
        };
        let start = Local.timestamp_millis_opt(after).single()?.date_naive();
        match self {
            Schedule::Every { minutes } => Some(after + i64::from((*minutes).max(1)) * 60_000),
            Schedule::Daily { time } => (0..3).filter_map(|d| at_time(start + chrono::Days::new(d), time)).find(|t| *t > after),
            Schedule::Weekly { day, time } => (0..15)
                .map(|d| start + chrono::Days::new(d))
                .filter(|date| date.weekday().num_days_from_monday() == *day % 7)
                .filter_map(|date| at_time(date, time))
                .find(|t| *t > after),
        }
    }
}

/// `HH:MM` as typed ("9:05" too), or `None`.
pub fn clock_time(text: &str) -> Option<String> {
    let time = NaiveTime::parse_from_str(text.trim(), "%H:%M").ok()?;
    Some(time.format("%H:%M").to_string())
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Automation {
    pub id: String,
    pub name: String,
    /// The project folder its tasks run in.
    pub project: String,
    pub prompt: String,
    pub schedule: Schedule,
    pub enabled: bool,
    pub created_at: i64,
    #[serde(default)]
    pub last_run: Option<i64>,
    /// The task its last run started.
    #[serde(default)]
    pub last_task: Option<String>,
    /// The bot whose routine this is; found (or made) from the project on
    /// its first run when it was saved without one.
    #[serde(default)]
    pub bot: Option<String>,
}

impl Automation {
    pub fn next_run(&self) -> Option<i64> {
        self.schedule.next_after(self.last_run.unwrap_or(self.created_at))
    }

    pub fn is_due(&self, now: i64) -> bool {
        self.enabled && self.next_run().is_some_and(|next| next <= now)
    }
}

pub fn load(saved: Option<&str>) -> Vec<Automation> {
    saved.and_then(|s| serde_json::from_str(s).ok()).unwrap_or_default()
}

fn save(app: AppState, list: Vec<Automation>) {
    if let Ok(text) = serde_json::to_string(&list) {
        app.with_store(|s| s.set_setting(KEY, Some(&text)));
    }
    app.automations.set(list);
}

fn update(app: AppState, id: &str, f: impl FnOnce(&mut Automation)) {
    let mut list = app.automations.get_untracked();
    if let Some(found) = list.iter_mut().find(|a| a.id == id) {
        f(found);
    }
    save(app, list);
}

/// Runs a routine now: its prompt goes to its bot, as a piece of work the
/// person asked for when they wrote the routine.
pub fn run_now(app: AppState, id: &str) {
    let Some(automation) = app.automations.with_untracked(|l| l.iter().find(|a| a.id == id).cloned()) else { return };
    // A routine is its bot's: without the bot there is nobody to run it.
    let Some(bot) = automation.bot.clone().filter(|bot| app.bots_untracked().iter().any(|t| &t.task.id == bot)) else {
        log::warn!("routine {}: its bot is gone", automation.name);
        return;
    };
    crate::manager::bring(
        app,
        &bot,
        crate::manager::Work {
            from: crate::manager::Sender::Routine(automation.name.clone()),
            title: &automation.name,
            text: &automation.prompt,
            asker: &automation.id,
            explicit: true,
            // It runs in the folder its bot worked in when it was written.
            project: Some(automation.project.as_str()).filter(|p| !p.is_empty()),
            priority: crate::manager::NORMAL,
        },
        crate::manager::Standing::Accept,
        false,
    );
    update(app, id, |a| {
        a.last_run = Some(bubble_store::now_ms());
        a.last_task = Some(bot.clone());
        a.bot = Some(bot);
    });
}

/// Looks for due automations every half minute.
pub fn start(app: AppState) {
    let every = std::env::var("BUBBLE_AUTOMATION_TICK_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(30_000);
    spawn(async move {
        loop {
            timer(Duration::from_millis(every)).await;
            let now = bubble_store::now_ms();
            let due: Vec<String> =
                app.automations.with_untracked(|l| l.iter().filter(|a| a.is_due(now)).map(|a| a.id.clone()).collect());
            for id in due {
                run_now(app, &id);
            }
        }
    });
}

fn when_label(at: i64) -> String {
    let Some(time) = Local.timestamp_millis_opt(at).single() else { return String::new() };
    let today = Local::now().date_naive();
    if time.date_naive() == today {
        format!("today {}", time.format("%H:%M"))
    } else if time.date_naive() == today + chrono::Days::new(1) {
        format!("tomorrow {}", time.format("%H:%M"))
    } else {
        time.format("%a %H:%M").to_string()
    }
}

// --- The page ------------------------------------------------------------------

/// A toggle among a few choices.
fn choice(label: String, chosen: impl Fn() -> bool + 'static, on_press: impl Fn() + 'static) -> impl IntoElement {
    let chosen = std::rc::Rc::new(chosen);
    let for_text = chosen.clone();
    view()
        .h(px(28.0))
        .px(px(10.0))
        .justify_center()
        .rounded(px(7.0))
        .border(px(LINE))
        .border_color(border())
        .bg(move || if chosen() { tertiary() } else { pane() })
        .cursor(Cursor::Pointer)
        .hover(|s| s.bg(tertiary()))
        .on_press(on_press)
        .label(label.clone())
        .child(text(label).text_size(px(BODY)).text_color(move || if for_text() { ink() } else { secondary() }))
}

fn field(placeholder: &'static str, value: Signal<String>, multiline: bool) -> impl IntoElement {
    nsh_base::input()
        .value(value)
        .placeholder(placeholder.to_owned())
        .placeholder_color(move || color(muted()))
        .multiline(multiline)
        .min_h(px(if multiline { 72.0 } else { 32.0 }))
        .px(px(10.0))
        .py(px(7.0))
        .rounded(px(8.0))
        .border(px(LINE))
        .border_color(border())
        .bg(pane())
        .text_size(px(BODY))
        .text_color(ink())
        .on_change(move |v: &str| value.set(v.to_owned()))
}

fn form(app: AppState, composing: Signal<bool>, bot: String) -> impl IntoElement {
    let name = signal(String::new());
    let prompt = signal(String::new());
    let project = app.with_store(|s| s.get_task(&bot)).flatten().and_then(|t| t.project_cwd).unwrap_or_default();
    let kind = signal("daily");
    let time = signal("09:00".to_owned());
    let day = signal(0u32);
    let minutes = signal("60".to_owned());
    let problem = signal(None::<String>);
    let create = move || {
        let schedule = match kind.get_untracked() {
            "weekly" => clock_time(&time.get_untracked()).map(|time| Schedule::Weekly { day: day.get_untracked(), time }),
            "every" => minutes.get_untracked().trim().parse::<u32>().ok().filter(|m| *m > 0).map(|minutes| Schedule::Every { minutes }),
            _ => clock_time(&time.get_untracked()).map(|time| Schedule::Daily { time }),
        };
        let (name, prompt) = (name.get_untracked().trim().to_owned(), prompt.get_untracked().trim().to_owned());
        let problem_text = if name.is_empty() {
            Some("Give it a name.")
        } else if prompt.is_empty() {
            Some("Write what the bot should do.")
        } else if schedule.is_none() {
            Some(if kind.get_untracked() == "every" { "Write a number of minutes." } else { "Write the time as 09:00." })
        } else {
            None
        };
        if let Some(text) = problem_text {
            return problem.set(Some(text.into()));
        }
        let mut list = app.automations.get_untracked();
        list.push(Automation {
            id: format!("auto_{}", bubble_core::time::unique_id()),
            name,
            project: project.clone(),
            prompt,
            schedule: schedule.unwrap_or(Schedule::Every { minutes: 60 }),
            enabled: true,
            created_at: bubble_store::now_ms(),
            last_run: None,
            last_task: None,
            bot: Some(bot.clone()),
        });
        save(app, list);
        composing.set(false);
    };
    view()
        .p(px(12.0))
        .gap(px(10.0))
        .rounded(px(12.0))
        .border(px(LINE))
        .border_color(border())
        .bg(paper())
        .child(field("Name", name, false))
        .child(field("What should the bot do each time?", prompt, true))
        .child(
            view()
                .flex_row()
                .flex_wrap()
                .gap(px(6.0))
                .child(choice("Daily".into(), move || kind.get() == "daily", move || kind.set("daily")))
                .child(choice("Weekly".into(), move || kind.get() == "weekly", move || kind.set("weekly")))
                .child(choice("Every few minutes".into(), move || kind.get() == "every", move || kind.set("every"))),
        )
        .child(when(move || kind.get() == "weekly", move || {
            view().flex_row().flex_wrap().gap(px(6.0)).children(
                (0..7u32)
                    .map(|d| choice(DAYS[d as usize][..3].to_owned(), move || day.get() == d, move || day.set(d)).into_element())
                    .collect::<Vec<_>>(),
            )
        }))
        .child(dynamic(move || {
            if kind.get() == "every" {
                view()
                    .flex_row()
                    .items_center()
                    .gap(px(8.0))
                    .child(view().w(px(90.0)).child(field("Minutes", minutes, false)))
                    .child(text("minutes").text_size(px(BODY)).text_color(muted()))
                    .into_element()
            } else {
                view()
                    .flex_row()
                    .items_center()
                    .gap(px(8.0))
                    .child(text("At").text_size(px(BODY)).text_color(muted()))
                    .child(view().w(px(90.0)).child(field("09:00", time, false)))
                    .into_element()
            }
        }))
        .child(dynamic(move || problem.get().map(|p| text(p).text_size(px(SMALL)).text_color(failed()))))
        .child(
            view()
                .flex_row()
                .justify_end()
                .gap(px(8.0))
                .child(button("Cancel", move || composing.set(false)))
                .child(primary_button("Save routine", create)),
        )
}

fn row(app: AppState, automation: ReadSignal<Automation>) -> impl IntoElement {
    let id = move || automation.get_untracked().id;
    let detail = move || {
        let a = automation.get();
        let next = if a.enabled {
            a.next_run().map(|n| format!(", next {}", when_label(n))).unwrap_or_default()
        } else {
            ", off".into()
        };
        format!("{}{next}", a.schedule.describe())
    };
    view()
        .gap(px(6.0))
        .py(px(8.0))
        .border_b(px(LINE))
        .border_color(hairline())
        .child(text(move || automation.get().name).text_size(px(BODY)).text_color(ink()).number_of_lines(1))
        .child(text(detail).text_size(px(SMALL)).text_color(muted()).number_of_lines(1))
        .child(
            view()
                .flex_row()
                .flex_wrap()
                .gap(px(6.0))
                .child(button("Run now", move || run_now(app, &id())))
                .child(dynamic(move || {
                    let on = automation.get().enabled;
                    button(if on { "Turn off" } else { "Turn on" }, move || update(app, &id(), |a| a.enabled = !on))
                }))
                .child(button("Delete", move || remove(app, &id()))),
        )
}

fn remove(app: AppState, id: &str) {
    let list = app.automations.get_untracked().into_iter().filter(|a| a.id != id).collect();
    save(app, list);
}

/// The routines of `bot`.
pub fn of_bot(app: AppState, bot: &str) -> Vec<Automation> {
    app.automations.get().into_iter().filter(|a| a.bot.as_deref() == Some(bot)).collect()
}

/// A bot's routines, in its details: the list, and the form that adds one.
pub fn section(app: AppState, bot: String) -> impl IntoElement {
    let composing = signal(false);
    let list_bot = bot.clone();
    let empty_bot = bot.clone();
    view()
        .gap(px(8.0))
        .child(
            text("Recurring work this bot does on a schedule. You can also ask it in chat to set one up.")
                .text_size(px(SMALL))
                .text_color(muted()),
        )
        .child(when(move || of_bot(app, &empty_bot).is_empty() && !composing.get(), || {
            text("No routines yet.").text_size(px(SMALL)).text_color(muted())
        }))
        .child(each(move || of_bot(app, &list_bot), |a: &Automation| a.id.clone(), move |automation, _| row(app, automation)))
        .child(when(move || !composing.get(), move || button("Add a routine", move || composing.set(true))))
        .child(when(move || composing.get(), move || form(app, composing, bot.clone())))
}

// --- The bot's own tools --------------------------------------------------------

/// The tools a bot sets its routines up with.
pub fn specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "list_routines".into(),
            description: "Your routines: recurring work you do on a schedule, with each one's id, schedule and next run.".into(),
            parameters: json!({ "type": "object", "properties": {} }),
        },
        ToolSpec {
            name: "add_routine".into(),
            description: "Set up a routine for yourself: when it is due, its prompt comes to you as a message and you do the work. Only when the person asks for recurring work.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "Short, in the person's language" },
                    "prompt": { "type": "string", "description": "What to do each time, self-contained" },
                    "every": { "type": "string", "enum": ["day", "week", "minutes"], "description": "day: each day at `at`; week: each week on `day` at `at`; minutes: every `minutes` minutes" },
                    "at": { "type": "string", "description": "Local time as HH:MM, for day and week" },
                    "day": { "type": "string", "description": "The weekday in English, for week" },
                    "minutes": { "type": "integer", "description": "How many minutes between runs, for minutes" }
                },
                "required": ["name", "prompt", "every"]
            }),
        },
        ToolSpec {
            name: "remove_routine".into(),
            description: "Remove one of your routines by its id.".into(),
            parameters: json!({
                "type": "object",
                "properties": { "id": { "type": "string" } },
                "required": ["id"]
            }),
        },
    ]
}

pub fn is_routine_tool(name: &str) -> bool {
    matches!(name, "list_routines" | "add_routine" | "remove_routine")
}

/// The schedule a bot described, or what is wrong with it.
pub fn schedule_from(args: &serde_json::Value) -> Result<Schedule, String> {
    let text = |key: &str| args.get(key).and_then(|v| v.as_str()).map(str::trim).unwrap_or_default();
    let at = || clock_time(text("at")).ok_or_else(|| "Give the time as HH:MM in `at`, such as 09:00.".to_owned());
    match text("every") {
        "day" => Ok(Schedule::Daily { time: at()? }),
        "week" => {
            let day = DAYS
                .iter()
                .position(|d| d.eq_ignore_ascii_case(text("day")))
                .ok_or_else(|| "Give the weekday in `day`, such as Monday.".to_owned())?;
            Ok(Schedule::Weekly { day: day as u32, time: at()? })
        }
        "minutes" => match args.get("minutes").and_then(|m| m.as_u64()) {
            Some(minutes) if minutes >= 5 => Ok(Schedule::Every { minutes: minutes as u32 }),
            Some(_) => Err("A routine runs at most every 5 minutes.".into()),
            None => Err("Give the number of minutes in `minutes`.".into()),
        },
        other => Err(format!("Unknown `every` {other:?}; use day, week or minutes.")),
    }
}

/// Answers a routine call made by the bot `bot`.
pub fn execute(app: AppState, bot: &str, call: &ToolCall) -> ToolOutput {
    let args = call.arguments_json();
    let text = |key: &str| args.get(key).and_then(|v| v.as_str()).map(str::trim).unwrap_or_default().to_owned();
    let ok = |content: String| ToolOutput { content, is_error: false };
    let error = |content: String| ToolOutput { content, is_error: true };
    match call.name.as_str() {
        "list_routines" => {
            let mine = untracked(|| of_bot(app, bot));
            if mine.is_empty() {
                return ok("You have no routines.".into());
            }
            ok(format!(
                "id | name | schedule | next run\n{}",
                mine.iter()
                    .map(|a| {
                        let next = if a.enabled { a.next_run().map(when_label).unwrap_or_default() } else { "off".into() };
                        format!("{} | {} | {} | {next}", a.id, a.name, a.schedule.describe())
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            ))
        }
        "add_routine" => {
            let (name, prompt) = (text("name"), text("prompt"));
            if name.is_empty() || prompt.is_empty() {
                return error("A routine needs a name and a prompt.".into());
            }
            let schedule = match schedule_from(&args) {
                Ok(schedule) => schedule,
                Err(message) => return error(message),
            };
            let project = app.with_store(|s| s.get_task(bot)).flatten().and_then(|t| t.project_cwd).unwrap_or_default();
            let automation = Automation {
                id: format!("auto_{}", bubble_core::time::unique_id()),
                name: name.clone(),
                project,
                prompt,
                schedule,
                enabled: true,
                created_at: bubble_store::now_ms(),
                last_run: None,
                last_task: None,
                bot: Some(bot.to_owned()),
            };
            let said = format!("Set up routine {} \"{name}\": {}.", automation.id, automation.schedule.describe().to_lowercase());
            let mut list = app.automations.get_untracked();
            list.push(automation);
            save(app, list);
            ok(said)
        }
        "remove_routine" => {
            let id = text("id");
            if !untracked(|| of_bot(app, bot)).iter().any(|a| a.id == id) {
                return error(format!("You have no routine {id}. Call list_routines for the ids."));
            }
            remove(app, &id);
            ok(format!("Removed routine {id}."))
        }
        other => error(format!("There is no tool called {other}.")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bot_describes_a_schedule_in_words() {
        assert_eq!(schedule_from(&json!({ "every": "day", "at": "9:05" })), Ok(Schedule::Daily { time: "09:05".into() }));
        assert_eq!(
            schedule_from(&json!({ "every": "week", "day": "friday", "at": "17:00" })),
            Ok(Schedule::Weekly { day: 4, time: "17:00".into() })
        );
        assert_eq!(schedule_from(&json!({ "every": "minutes", "minutes": 30 })), Ok(Schedule::Every { minutes: 30 }));
        assert!(schedule_from(&json!({ "every": "minutes", "minutes": 1 })).unwrap_err().contains("5 minutes"));
        assert!(schedule_from(&json!({ "every": "day" })).unwrap_err().contains("HH:MM"));
        assert!(schedule_from(&json!({ "every": "week", "day": "someday", "at": "09:00" })).unwrap_err().contains("weekday"));
        assert!(schedule_from(&json!({ "every": "fortnight" })).is_err());
    }

    fn at(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> i64 {
        Local.with_ymd_and_hms(y, mo, d, h, mi, 0).earliest().unwrap().timestamp_millis()
    }

    #[test]
    fn schedules_fire_after_the_last_run() {
        // 2026-11-09 is a Monday.
        let daily = Schedule::Daily { time: "09:00".into() };
        assert_eq!(daily.next_after(at(2026, 11, 9, 8, 0)), Some(at(2026, 11, 9, 9, 0)));
        assert_eq!(daily.next_after(at(2026, 11, 9, 9, 0)), Some(at(2026, 11, 10, 9, 0)), "not twice in a day");
        let weekly = Schedule::Weekly { day: 2, time: "18:30".into() };
        assert_eq!(weekly.next_after(at(2026, 11, 9, 8, 0)), Some(at(2026, 11, 11, 18, 30)));
        assert_eq!(weekly.next_after(at(2026, 11, 11, 19, 0)), Some(at(2026, 11, 18, 18, 30)));
        let every = Schedule::Every { minutes: 15 };
        assert_eq!(every.next_after(1_000), Some(1_000 + 15 * 60_000));
        assert_eq!(Schedule::Every { minutes: 120 }.describe(), "Every 2 hours");
        assert_eq!(weekly.describe(), "Every Wednesday at 18:30");
        assert_eq!(clock_time("9:05"), Some("09:05".into()));
        assert_eq!(clock_time("25:00"), None);

        let mut automation = Automation {
            id: "a".into(),
            name: "n".into(),
            project: "/p".into(),
            prompt: "p".into(),
            schedule: every,
            enabled: true,
            created_at: 0,
            last_run: None,
            last_task: None,
            bot: None,
        };
        assert!(automation.is_due(15 * 60_000) && !automation.is_due(15 * 60_000 - 1));
        automation.enabled = false;
        assert!(!automation.is_due(i64::MAX / 2));
        let saved = serde_json::to_string(&vec![automation.clone()]).unwrap();
        assert_eq!(load(Some(&saved)), vec![automation]);
        assert!(load(Some("not json")).is_empty());
    }
}
