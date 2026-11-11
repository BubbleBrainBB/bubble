//! The app's colors and sizes, taken from the Electron Bubble app
//! (`desktop/src/ui/index.css`): a light gray window chrome, one raised card
//! holding the sidebar and the content, gray user bubbles, black send
//! button. Four type sizes, nothing under 12 px. Each color has a dark
//! twin, which styles follow with the appearance.

use nsh_core::prelude::*;

fn pair(light: u32, dark: u32) -> AdaptiveColor {
    AdaptiveColor::new(rgb(light), rgb(dark))
}

fn pair_alpha(light: u32, dark: u32) -> AdaptiveColor {
    AdaptiveColor::new(rgba(light), rgba(dark))
}

/// The window around the card: the top strip and the icon rail.
pub fn chrome() -> AdaptiveColor {
    pair(0xF4F4F4, 0x161616)
}
/// The sidebar inside the card.
pub fn side() -> AdaptiveColor {
    pair(0xF8F8F8, 0x1B1B1B)
}
/// The content pane.
pub fn pane() -> AdaptiveColor {
    pair(0xFCFCFC, 0x1F1F1F)
}
pub fn paper() -> AdaptiveColor {
    pair(0xFFFFFF, 0x262626)
}
pub fn ink() -> AdaptiveColor {
    pair(0x1A1A1A, 0xECECEC)
}
pub fn secondary() -> AdaptiveColor {
    pair(0x525252, 0xB4B4B4)
}
pub fn muted() -> AdaptiveColor {
    pair(0x8F8F8F, 0x7E7E7E)
}
/// Icons and glyphs of the agent's trace, a step quieter than muted text.
pub fn faint() -> AdaptiveColor {
    pair(0xB8B8B8, 0x5C5C5C)
}
/// Edges of the card and dividers inside it.
pub fn hairline() -> AdaptiveColor {
    pair_alpha(0x0000_0017, 0xFFFF_FF17)
}
/// Inputs and buttons with an outline.
pub fn border() -> AdaptiveColor {
    pair_alpha(0x0000_0024, 0xFFFF_FF24)
}
pub fn item_hover() -> AdaptiveColor {
    pair_alpha(0x0000_000A, 0xFFFF_FF0D)
}
pub fn item_active() -> AdaptiveColor {
    pair_alpha(0x0000_000F, 0xFFFF_FF14)
}
pub fn user_bubble() -> AdaptiveColor {
    pair(0xEBEBEB, 0x2E2E2E)
}
pub fn tertiary() -> AdaptiveColor {
    pair(0xF5F5F7, 0x2A2A2A)
}
pub fn needs() -> AdaptiveColor {
    pair(0xE5532D, 0xF06A45)
}
pub fn failed() -> AdaptiveColor {
    pair(0xDC2626, 0xF87171)
}
/// Lines a change adds and removes, and their tints in a diff.
pub fn added() -> AdaptiveColor {
    pair(0x15803D, 0x4ADE80)
}
pub fn removed() -> AdaptiveColor {
    pair(0xB91C1C, 0xF87171)
}
pub fn added_tint() -> AdaptiveColor {
    pair_alpha(0x16A3_4A14, 0x22C5_5E1F)
}
pub fn removed_tint() -> AdaptiveColor {
    pair_alpha(0xDC26_2614, 0xEF44_441F)
}
/// Shadows under raised surfaces.
pub fn shadow() -> AdaptiveColor {
    pair_alpha(0x0F17_2A14, 0x0000_0066)
}

/// Captions, times, counts.
pub const SMALL: f32 = 12.0;
/// Conversation text and controls (`--conversation-font-size`).
pub const BODY: f32 = 13.0;
/// The workspace name, a list's titles.
pub const TITLE: f32 = 15.0;
/// Empty-state and settings headings.
pub const HEADING: f32 = 18.0;

pub const LINE: f32 = 0.5;
pub const TOP_BAR: f32 = 38.0;
pub const RAIL_WIDTH: f32 = 40.0;
pub const SIDEBAR_WIDTH: f32 = 236.0;
pub const READING_WIDTH: f32 = 720.0;
