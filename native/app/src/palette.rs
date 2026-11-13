//! The app's colors and sizes. The window is laid out as a messenger is
//! (the Grok Bot desktop app is the reference): a light gray sidebar of
//! bots beside the white conversation. Four type sizes, nothing under
//! 12 px. Each color has a dark twin, which styles follow with the
//! appearance.

use nsh_core::prelude::*;

fn pair(light: u32, dark: u32) -> AdaptiveColor {
    AdaptiveColor::new(rgb(light), rgb(dark))
}

fn pair_alpha(light: u32, dark: u32) -> AdaptiveColor {
    AdaptiveColor::new(rgba(light), rgba(dark))
}

/// The sidebar.
pub fn side() -> AdaptiveColor {
    pair(0xF3F3F3, 0x181818)
}
/// The conversation and the pages.
pub fn pane() -> AdaptiveColor {
    pair(0xFDFDFD, 0x1F1F1F)
}
/// What a bot says: a bubble a step off the pane.
pub fn bot_bubble() -> AdaptiveColor {
    pair(0xF1F1F1, 0x2A2A2A)
}
/// Something new, or someone there: the dot beside a bot.
pub fn online() -> AdaptiveColor {
    pair(0x2F6FED, 0x5B8DEF)
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
/// What the person wrote: the strongest surface in the conversation, so
/// it is told at a glance from what a bot said (gray). Black with white
/// words in light; the reverse in dark, where black would not show.
pub fn user_bubble() -> AdaptiveColor {
    pair(0x0B0B0B, 0xECECEC)
}
pub fn user_bubble_text() -> AdaptiveColor {
    pair(0xFFFFFF, 0x1A1A1A)
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
/// Something is there and works: the dot beside it.
pub fn working_color() -> AdaptiveColor {
    pair(0x2E9E5B, 0x4CC26E)
}
/// What is behind a dialog, dimmed.
pub fn scrim() -> AdaptiveColor {
    pair_alpha(0x0000_003D, 0x0000_0080)
}
/// Shadows under raised surfaces.
pub fn shadow() -> AdaptiveColor {
    pair_alpha(0x0F17_2A14, 0x0000_0066)
}

/// Captions, times, counts.
pub const SMALL: f32 = 12.0;
/// Conversation text and controls (`--conversation-font-size`).
pub const BODY: f32 = 13.0;
/// Empty-state and settings headings.
pub const HEADING: f32 = 18.0;

pub const LINE: f32 = 0.5;
pub const TOP_BAR: f32 = 52.0;
pub const SIDEBAR_WIDTH: f32 = 280.0;
pub const READING_WIDTH: f32 = 720.0;
