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
    /// Nothing: a still face.
    Still,
    /// Writing back: it looks down at what it writes.
    Typing,
    /// Between steps: it looks up, and from side to side slowly.
    Thinking,
    /// Using a tool: it looks from one side to the other.
    Working,
    /// Waiting for the person or its turn: it looks ahead, and blinks.
    Waiting,
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

/// A face whose eyes show `mood`: a shape, and two eyes over it that look
/// about and blink. A still mood runs nothing.
fn alive(size: f32, shape: &str, eyes: [(f32, f32, f32, f32); 2], tilt: f32, mood: Mood) -> View {
    let scale = size / 48.0;
    // How far a glance goes.
    let reach = size * 0.045;
    let (across, down, lids) = (animated_value(0.0), animated_value(0.0), animated_value(1.0));
    let glide = |ms: f32| Timing::new(ms, Easing::EaseInOut);
    let blink_every = |rest: f32| {
        let _ = Animation::sequence([
            Animation::delay(rest),
            Animation::timing(&lids, 0.1, Timing::new(80.0, Easing::EaseInOut)),
            Animation::timing(&lids, 1.0, Timing::new(140.0, Easing::EaseInOut)),
        ])
        .looped(None)
        .start();
    };
    match mood {
        Mood::Still => {}
        Mood::Waiting => blink_every(2600.0),
        Mood::Typing => {
            let _ = Animation::timing(&down, reach * 0.9, glide(260.0)).start();
            blink_every(3200.0);
        }
        Mood::Thinking => {
            let _ = Animation::timing(&down, -reach, glide(360.0)).start();
            let _ = Animation::sequence([
                Animation::timing(&across, -reach * 0.7, glide(1300.0)),
                Animation::delay(500.0),
                Animation::timing(&across, reach * 0.7, glide(1300.0)),
                Animation::delay(500.0),
            ])
            .looped(None)
            .start();
            blink_every(3600.0);
        }
        Mood::Working => {
            let _ = Animation::sequence([
                Animation::timing(&across, reach, glide(420.0)),
                Animation::delay(700.0),
                Animation::timing(&across, -reach, glide(560.0)),
                Animation::delay(700.0),
            ])
            .looped(None)
            .start();
            blink_every(4200.0);
        }
    }
    view()
        .size(px(size))
        .flex_shrink_0()
        .child(svg(ImageSrc::svg_markup(shape)).absolute().top(px(0.0)).left(px(0.0)).size(px(size)))
        .child(
            view()
                .absolute()
                .top(px(0.0))
                .left(px(0.0))
                .size(px(size))
                .bind_translate_x(&across)
                .bind_translate_y(&down)
                .children(eyes.map(|(left, top, width, height)| {
                    // The lid closes over the eye; the eye leans inside it.
                    view()
                        .absolute()
                        .left(px(left * scale))
                        .top(px(top * scale))
                        .w(px(width * scale))
                        .h(px(height * scale))
                        .bind_transform(nsh_core::TransformKind::ScaleY, &lids)
                        .child(view().w_full().h_full().rounded(px(width * scale / 2.0)).bg(rgb(0xFFFFFF)).rotate(deg(tilt)))
                })),
        )
}

/// A bot's face showing what it is doing; it follows a change of look and
/// of mood.
pub fn bot_doing(app: crate::state::AppState, id: &str, size: f32, mood: impl Fn() -> Mood + 'static) -> impl IntoElement {
    let id = id.to_owned();
    let mood = computed(mood);
    view().size(px(size)).flex_shrink_0().child(dynamic(move || {
        let (shape, color) = app.face_of(&id);
        alive(size, &shape_markup(shape, color), EYE_BOXES, EYE_TILT, mood.get())
    }))
}

/// The main bot's face showing what it is doing.
pub fn main_doing(size: f32, mood: impl Fn() -> Mood + 'static) -> impl IntoElement {
    let mood = computed(mood);
    view().size(px(size)).flex_shrink_0().child(dynamic(move || alive(size, MAIN_SHAPE, MAIN_EYE_BOXES, MAIN_EYE_TILT, mood.get())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bot_keeps_its_face_and_bots_differ() {
        assert_eq!(markup("bot-1"), markup("bot-1"));
        let faces: std::collections::HashSet<String> = (0..24).map(|i| markup(&format!("bot-{i}"))).collect();
        assert!(faces.len() >= 8, "two dozen bots share only {} faces", faces.len());
        assert!(markup("x").starts_with("<svg") && markup("x").ends_with("</svg>"));
    }
}
