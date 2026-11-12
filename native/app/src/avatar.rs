//! A bot's face: a soft shape in one color with two eyes, as a messenger
//! shows who you are talking to. Until a person picks a look, a bot's id
//! picks one, so it is the same everywhere and after every restart.
//!
//! A face is still where it only says who the bot is. Where it says what
//! the bot is doing (in its row of the list, at the end of its
//! conversation while it works) its eyes move with it: see [`Mood`].

use nsh_core::element::ImageSrc;
use nsh_core::prelude::*;

/// What a bot is doing, as its face shows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mood {
    /// Nothing: a still face (its eyes still follow a pointer that comes near).
    Still,
    /// Writing back: it looks down at what it writes, its eyes going along the line.
    Typing,
    /// Between steps: it looks up, and slowly from side to side.
    Thinking,
    /// Reading files or a page: its eyes go along a line, and back to the next.
    Reading,
    /// Changing files: it looks down, with small quick glances.
    Writing,
    /// Searching: it looks all around.
    Searching,
    /// Running commands: it looks from one side to the other, quickly.
    Running,
    /// Talking to another bot: it looks over to the side, and nods.
    Messaging,
    /// Something else: it looks from side to side.
    Working,
    /// Waiting for the person or its turn: it looks ahead, and blinks.
    Waiting,
    /// It has said something the person has not read: wide-eyed, with a little bounce.
    Pleased,
    /// Its last turn failed: its eyes are low and half shut.
    Troubled,
}

/// The outlines, in a 48 by 48 box.
const SHAPES: [&str; 5] = [
    // A blob.
    r#"<path d="M24 3.5c11 0 20.5 7.5 20.5 20S36 44.5 24 44.5 3.5 36.5 3.5 24 13 3.5 24 3.5z"/>"#,
    // A bean.
    r#"<path d="M15 7c9-5 22-2 27 9s1 24-12 27-23-3-26-15S7 11.500 15 7z"/>"#,
    // An arch.
    r#"<path d="M7 44.500V22C7 12 14.500 4 24 4s17 8 17 18v22.500z"/>"#,
    // A capsule.
    r#"<rect x="3" y="10" width="42" height="28" rx="14"/>"#,
    // A cloud.
    r#"<path d="M13.500 40a10.500 10.500 0 0 1-2-20.800A13 13 0 0 1 36.800 17 11.500 11.500 0 0 1 35 40z"/>"#,
];

const COLORS: [&str; 8] = ["#F26B3A", "#3DBE9F", "#4CC26E", "#F0464B", "#B5763E", "#4C8DF6", "#8B6CF0", "#E7A93A"];

/// Where a bot's eyes are in the 48 by 48 box: left, top, width, height.
const EYE_BOXES: [(f32, f32, f32, f32); 2] = [(18.5, 15.0, 4.2, 9.5), (26.5, 15.0, 4.2, 9.5)];
/// How far they lean, in degrees.
const EYE_TILT: f32 = -16.0;
/// The main bot's.
const MAIN_EYE_BOXES: [(f32, f32, f32, f32); 2] = [(18.5, 13.0, 5.0, 12.0), (28.0, 13.0, 5.0, 12.0)];
const MAIN_EYE_TILT: f32 = -24.0;
const MAIN_SHAPE: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 48 48"><circle cx="24" cy="24" r="22" fill="#0B0B0B"/></svg>"##;

const EYES: &str = r##"<rect x="18.500" y="15" width="4.200" height="9.500" rx="2.100" fill="#FFFFFF" transform="rotate(-16 20.600 19.750)"/><rect x="26.500" y="15" width="4.200" height="9.500" rx="2.100" fill="#FFFFFF" transform="rotate(-16 28.600 19.750)"/>"##;

/// The main bot: a black disc.
const MAIN: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 48 48"><circle cx="24" cy="24" r="22" fill="#0B0B0B"/><rect x="18.500" y="13" width="5" height="12" rx="2.500" fill="#FFFFFF" transform="rotate(-24 21 19)"/><rect x="28" y="13" width="5" height="12" rx="2.500" fill="#FFFFFF" transform="rotate(-24 30.500 19)"/></svg>"##;

fn pick(seed: &str) -> (usize, usize) {
    // FNV-1a: small, stable, and spread well enough over a dozen bots.
    let hash = seed.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3));
    ((hash % SHAPES.len() as u64) as usize, ((hash / 7) % COLORS.len() as u64) as usize)
}

/// How many shapes and colors there are to choose from.
pub const SHAPE_COUNT: usize = SHAPES.len();
pub const COLOR_COUNT: usize = COLORS.len();

/// The shape and color a bot has until a person picks others.
pub fn default_face(seed: &str) -> (usize, usize) {
    pick(seed)
}

/// The face of the bot `seed` names (its id), as SVG.
#[cfg(test)]
pub fn markup(seed: &str) -> String {
    let (shape, color) = pick(seed);
    face_markup(shape, color)
}

/// A face of a given shape and color, as SVG.
pub fn face_markup(shape: usize, color: usize) -> String {
    let (shape, color) = (shape % SHAPES.len(), color % COLORS.len());
    format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 48 48"><g fill="{}">{}</g>{EYES}</svg>"#,
        COLORS[color], SHAPES[shape]
    )
}

/// A bot's face, `size` points wide: the look the person picked for it,
/// or the one its id gives it. It follows a change of look.
pub fn bot(app: crate::state::AppState, id: &str, size: f32) -> Svg {
    let id = id.to_owned();
    svg(move || {
        let (shape, color) = app.face_of(&id);
        ImageSrc::svg_markup(&face_markup(shape, color))
    })
    .size(px(size))
    .flex_shrink_0()
}

/// The main bot's face.
pub fn main(size: f32) -> Svg {
    svg(ImageSrc::svg_markup(MAIN)).size(px(size)).flex_shrink_0()
}

/// A face without its eyes, as SVG: the eyes are drawn over it, to move.
fn shape_markup(shape: usize, color: usize) -> String {
    let (shape, color) = (shape % SHAPES.len(), color % COLORS.len());
    format!(r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 48 48"><g fill="{}">{}</g></svg>"#, COLORS[color], SHAPES[shape])
}

/// How near a pointer has to come for a face to look at it, in points.
const NOTICES: f32 = 220.0;

/// How far the eyes of a face `size` wide turn toward a pointer at
/// `pointer`, when the face's middle is at `middle` (both in the window):
/// right over the face they look straight out, further off further aside,
/// up to `reach`; `None` when the pointer is too far to notice.
fn toward(pointer: (f32, f32), middle: (f32, f32), size: f32, reach: f32) -> Option<(f32, f32)> {
    let (dx, dy) = (pointer.0 - middle.0, pointer.1 - middle.1);
    let far = (dx * dx + dy * dy).sqrt();
    (far < NOTICES).then(|| {
        let pull = (far / (size * 0.9)).min(1.0) * reach / far.max(0.001);
        (dx * pull, dy * pull)
    })
}

/// A face whose eyes show `mood`, and look at the pointer when it comes
/// near: a shape, and two eyes over it that glance about and blink.
fn alive(app: crate::state::AppState, size: f32, shape: &str, eyes: [(f32, f32, f32, f32); 2], tilt: f32, mood: Mood) -> View {
    let scale = size / 48.0;
    // How far a glance goes.
    let reach = size * 0.085;
    // What the mood does with the eyes: where they look and how open they are.
    let (across, down, lids, wide) = (animated_value(0.0), animated_value(0.0), animated_value(1.0), animated_value(1.0));
    let glide = |ms: f32| Timing::new(ms, Easing::EaseInOut);
    let to = |value: &AnimatedValue, target: f32, ms: f32| Animation::timing(value, target, glide(ms));
    let run = |steps: Vec<Animation>| {
        let _ = Animation::sequence(steps).looped(None).start();
    };
    // The eyes shut and open again every `rest` milliseconds, from `open`.
    let blink = |rest: f32, open: f32| run(vec![Animation::delay(rest), to(&lids, 0.08, 80.0), to(&lids, open, 140.0)]);
    match mood {
        Mood::Still => {}
        Mood::Waiting => blink(2600.0, 1.0),
        Mood::Typing => {
            let _ = to(&down, reach * 0.8, 240.0).start();
            // Along the line it writes, and back.
            run(vec![to(&across, reach * 0.7, 900.0), to(&across, -reach * 0.7, 260.0)]);
            blink(3200.0, 1.0);
        }
        Mood::Thinking => {
            // Not as far up as aside: the top of a face is near.
            let _ = to(&down, -reach * 0.75, 360.0).start();
            run(vec![to(&across, -reach * 0.8, 1300.0), Animation::delay(450.0), to(&across, reach * 0.8, 1300.0), Animation::delay(450.0)]);
            blink(3600.0, 1.0);
        }
        Mood::Reading => {
            // A line, the next line, and back to the top.
            run(vec![
                to(&across, -reach, 200.0),
                to(&across, reach, 1100.0),
                to(&across, -reach, 220.0),
                to(&across, reach, 1100.0),
                to(&across, -reach, 220.0),
            ]);
            run(vec![to(&down, -reach * 0.3, 200.0), Animation::delay(1100.0), to(&down, reach * 0.4, 220.0), Animation::delay(1100.0), Animation::delay(220.0)]);
            blink(4200.0, 1.0);
        }
        Mood::Writing => {
            let _ = to(&down, reach * 0.9, 220.0).start();
            run(vec![to(&across, reach * 0.5, 180.0), to(&across, -reach * 0.5, 180.0), to(&across, reach * 0.2, 140.0), Animation::delay(520.0)]);
            blink(3400.0, 1.0);
        }
        Mood::Searching => {
            // Around: right, down, left, up.
            run(vec![to(&across, reach, 420.0), Animation::delay(420.0), to(&across, -reach, 420.0), Animation::delay(420.0)]);
            run(vec![Animation::delay(420.0), to(&down, reach * 0.8, 420.0), Animation::delay(420.0), to(&down, -reach * 0.8, 420.0)]);
            blink(4600.0, 1.0);
        }
        Mood::Running => {
            run(vec![to(&across, reach, 260.0), Animation::delay(380.0), to(&across, -reach, 320.0), Animation::delay(380.0)]);
            blink(3000.0, 1.0);
        }
        Mood::Messaging => {
            let _ = to(&across, reach, 300.0).start();
            // A nod, now and then.
            run(vec![Animation::delay(900.0), to(&down, reach * 0.6, 160.0), to(&down, 0.0, 200.0)]);
            blink(3800.0, 1.0);
        }
        Mood::Working => {
            run(vec![to(&across, reach, 420.0), Animation::delay(700.0), to(&across, -reach, 560.0), Animation::delay(700.0)]);
            blink(4200.0, 1.0);
        }
        Mood::Pleased => {
            let _ = to(&wide, 1.18, 200.0).start();
            run(vec![to(&down, -reach * 0.5, 220.0), to(&down, 0.0, 260.0), Animation::delay(1600.0)]);
            blink(3000.0, 1.0);
        }
        Mood::Troubled => {
            let _ = to(&down, reach * 0.7, 300.0).start();
            let _ = to(&lids, 0.55, 300.0).start();
            blink(5200.0, 0.55);
        }
    }

    // The pointer, when it is near: the eyes turn to it, and open a little
    // wider. `free` is how much of the mood's own glancing is left then.
    let (look_x, look_y, free, alert) = (animated_value(0.0), animated_value(0.0), animated_value(1.0), animated_value(1.0));
    let own = element_ref();
    // Where the face's middle is in the window, and when that was asked.
    let middle = stored(None::<(f32, f32)>);
    let asked = stored(0_i64);
    let watching = stored(false);
    let (look_x_to, look_y_to, free_to, alert_to) = (look_x.clone(), look_y.clone(), free.clone(), alert.clone());
    effect(move || {
        let pointer = app.pointer.get();
        let now = bubble_store::now_ms();
        // Lists scroll and panels open: ask again where the face is, now and then.
        if pointer.is_some() && now - asked.get() > 400 {
            asked.replace(now);
            spawn(async move {
                if let Some(found) = own.measure().await {
                    middle.replace(Some((found.page.x.0 + size / 2.0, found.page.y.0 + size / 2.0)));
                }
            });
        }
        let turn = match (pointer, middle.get()) {
            // A little further than its own glances go: it is looking at something.
            (Some(pointer), Some(middle)) => toward(pointer, middle, size, reach * 1.25),
            _ => None,
        };
        match turn {
            Some((x, y)) => {
                let quick = Timing::new(90.0, Easing::EaseOut);
                let _ = Animation::timing(&look_x_to, x, quick).start();
                let _ = Animation::timing(&look_y_to, y, quick).start();
                if !watching.replace(true) {
                    let _ = Animation::timing(&free_to, 0.0, Timing::new(160.0, Easing::EaseOut)).start();
                    let _ = Animation::timing(&alert_to, 1.14, Timing::new(160.0, Easing::EaseOut)).start();
                }
            }
            None if watching.replace(false) => {
                let back = Timing::new(320.0, Easing::EaseInOut);
                for (value, rest) in [(&look_x_to, 0.0), (&look_y_to, 0.0), (&free_to, 1.0), (&alert_to, 1.0)] {
                    let _ = Animation::timing(value, rest, back).start();
                }
            }
            None => {}
        }
    });

    let gaze_x = across.mul(&free).add(&look_x);
    let gaze_y = down.mul(&free).add(&look_y);
    let open = wide.mul(&alert);
    view()
        .size(px(size))
        .flex_shrink_0()
        .element_ref(&own)
        .child(svg(ImageSrc::svg_markup(shape)).absolute().top(px(0.0)).left(px(0.0)).size(px(size)))
        .child(
            view()
                .absolute()
                .top(px(0.0))
                .left(px(0.0))
                .size(px(size))
                .bind_translate_x(&gaze_x)
                .bind_translate_y(&gaze_y)
                .children(eyes.map(|(left, top, width, height)| {
                    // The lid closes over the eye; the eye leans inside it,
                    // and grows a little when something has its attention.
                    view()
                        .absolute()
                        .left(px(left * scale))
                        .top(px(top * scale))
                        .w(px(width * scale))
                        .h(px(height * scale))
                        .bind_transform(nsh_core::TransformKind::ScaleY, &lids)
                        .child(
                            view()
                                .w_full()
                                .h_full()
                                .bind_scale(&open)
                                .child(view().w_full().h_full().rounded(px(width * scale / 2.0)).bg(rgb(0xFFFFFF)).rotate(deg(tilt))),
                        )
                })),
        )
}

/// A bot's face showing what it is doing; it follows a change of look and
/// of mood, and a pointer that comes near.
pub fn bot_doing(app: crate::state::AppState, id: &str, size: f32, mood: impl Fn() -> Mood + 'static) -> impl IntoElement {
    let id = id.to_owned();
    let mood = computed(mood);
    view().size(px(size)).flex_shrink_0().child(dynamic(move || {
        let (shape, color) = app.face_of(&id);
        alive(app, size, &shape_markup(shape, color), EYE_BOXES, EYE_TILT, mood.get())
    }))
}

/// The main bot's face showing what it is doing.
pub fn main_doing(app: crate::state::AppState, size: f32, mood: impl Fn() -> Mood + 'static) -> impl IntoElement {
    let mood = computed(mood);
    view().size(px(size)).flex_shrink_0().child(dynamic(move || alive(app, size, MAIN_SHAPE, MAIN_EYE_BOXES, MAIN_EYE_TILT, mood.get())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eyes_turn_to_a_pointer_that_is_near() {
        let (size, reach) = (40.0, 4.0);
        let look = |x: f32, y: f32| toward((x, y), (100.0, 100.0), size, reach);
        // Right over the face: straight out.
        assert_eq!(look(100.0, 100.0), Some((0.0, 0.0)));
        // Off to the right, as far as the eyes go, and no further however far.
        let (x, y) = look(180.0, 100.0).unwrap();
        assert!((x - reach).abs() < 0.001 && y.abs() < 0.001, "{x}, {y}");
        assert_eq!(look(180.0, 100.0), look(250.0, 100.0));
        // Up and to the left: both, and still no further than the reach.
        let (x, y) = look(40.0, 40.0).unwrap();
        assert!(x < 0.0 && y < 0.0 && ((x * x + y * y).sqrt() - reach).abs() < 0.001);
        // Close by, it turns only part of the way.
        let (x, _) = look(110.0, 100.0).unwrap();
        assert!(x > 0.0 && x < reach);
        // Too far to notice.
        assert_eq!(look(100.0 + NOTICES + 1.0, 100.0), None);
    }

    #[test]
    fn a_bot_keeps_its_face_and_bots_differ() {
        assert_eq!(markup("bot-1"), markup("bot-1"));
        let faces: std::collections::HashSet<String> = (0..24).map(|i| markup(&format!("bot-{i}"))).collect();
        assert!(faces.len() >= 8, "two dozen bots share only {} faces", faces.len());
        assert!(markup("x").starts_with("<svg") && markup("x").ends_with("</svg>"));
    }
}
