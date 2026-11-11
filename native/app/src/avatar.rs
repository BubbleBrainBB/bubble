//! A bot's face: a soft shape in one color with two eyes, as a messenger
//! shows who you are talking to. Until a person picks a look, a bot's id
//! picks one, so it is the same everywhere and after every restart.

use nsh_core::element::ImageSrc;
use nsh_core::prelude::*;

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

const EYES: &str = r##"<rect x="18.500" y="15" width="4.200" height="9.500" rx="2.100" fill="#FFFFFF" transform="rotate(-16 20.600 19.750)"/><rect x="26.500" y="15" width="4.200" height="9.500" rx="2.100" fill="#FFFFFF" transform="rotate(-16 28.600 19.750)"/>"##;

/// The main bot: a black disc.
const MAIN: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 48 48"><circle cx="24" cy="24" r="22" fill="#0B0B0B"/><rect x="18.500" y="13" width="5" height="12" rx="2.500" fill="#FFFFFF" transform="rotate(-24 21 19)"/><rect x="28" y="13" width="5" height="12" rx="2.500" fill="#FFFFFF" transform="rotate(-24 30.500 19)"/></svg>"##;

fn pick(seed: &str) -> (usize, usize) {
    // FNV-1a: small, stable, and spread well enough over a dozen bots.
    let hash = seed.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3));
    ((hash % SHAPES.len() as u64) as usize, ((hash / 7) % COLORS.len() as u64) as usize)
}

/// The face of the bot `seed` names (its id), as SVG.
pub fn markup(seed: &str) -> String {
    let (shape, color) = pick(seed);
    format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 48 48"><g fill="{}">{}</g>{EYES}</svg>"#,
        COLORS[color], SHAPES[shape]
    )
}

/// A bot's face, `size` points wide.
pub fn bot(seed: &str, size: f32) -> Svg {
    svg(ImageSrc::svg_markup(&markup(seed))).size(px(size)).flex_shrink_0()
}

/// The main bot's face.
pub fn main(size: f32) -> Svg {
    svg(ImageSrc::svg_markup(MAIN)).size(px(size)).flex_shrink_0()
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
