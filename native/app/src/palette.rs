//! The app's colors and sizes, taken from the Electron Bubble app
//! (`desktop/src/ui/index.css`): a light gray window chrome, one raised card
//! holding the sidebar and the content, gray user bubbles, black send
//! button. Four type sizes, nothing under 12 px.

use nsh_core::prelude::*;

/// The window around the card: the top strip and the icon rail.
pub fn chrome() -> Rgba {
    rgb(0xF4F4F4)
}
/// The sidebar inside the card.
pub fn side() -> Rgba {
    rgb(0xF8F8F8)
}
/// The content pane.
pub fn pane() -> Rgba {
    rgb(0xFCFCFC)
}
pub fn paper() -> Rgba {
    rgb(0xFFFFFF)
}
pub fn ink() -> Rgba {
    rgb(0x1A1A1A)
}
pub fn secondary() -> Rgba {
    rgb(0x525252)
}
pub fn muted() -> Rgba {
    rgb(0x8F8F8F)
}
/// Edges of the card and dividers inside it.
pub fn hairline() -> Rgba {
    rgba(0x0000_0017)
}
/// Inputs and buttons with an outline.
pub fn border() -> Rgba {
    rgba(0x0000_0024)
}
pub fn item_hover() -> Rgba {
    rgba(0x0000_000A)
}
pub fn item_active() -> Rgba {
    rgba(0x0000_000F)
}
pub fn user_bubble() -> Rgba {
    rgb(0xEBEBEB)
}
pub fn tertiary() -> Rgba {
    rgb(0xF5F5F7)
}
pub fn needs() -> Rgba {
    rgb(0xE5532D)
}
pub fn failed() -> Rgba {
    rgb(0xDC2626)
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
