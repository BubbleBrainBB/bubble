//! Text in character cells: every character is as wide as the terminal
//! shows it (one or two columns), every line one row. The engine measures
//! and lays text out with [`CellTextEngine`]; the painter draws the lines
//! of its [`CellLayout`].

use std::ops::Range;
use std::sync::Arc;

use nsh_core::geometry::{Points, Size, px, size};
use nsh_core::{AttributedText, TextEngine, TextLayout, TextLine};
use unicode_width::UnicodeWidthChar;

/// Points per column.
pub const CELL_W: f32 = 8.0;
/// Points per row.
pub const CELL_H: f32 = 16.0;

/// The columns a character takes (control characters none).
pub fn char_width(c: char) -> usize {
    if c == '\t' {
        return 4;
    }
    c.width().unwrap_or(0)
}

/// The columns `text` takes on one line.
pub fn text_width(text: &str) -> usize {
    text.chars().map(char_width).sum()
}

/// The rows of `chars` in `columns` (no limit: `None`): ranges of
/// character indices, line breaks left out. Words move to the next row
/// whole where they fit on one.
pub fn wrap(chars: &[char], columns: Option<usize>) -> Vec<Range<usize>> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut width = 0;
    // The last space on this row: where it can break.
    let mut space: Option<usize> = None;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\n' {
            lines.push(start..i);
            i += 1;
            start = i;
            width = 0;
            space = None;
            continue;
        }
        let w = char_width(c);
        if let Some(max) = columns
            && width + w > max.max(1)
            && i > start
        {
            if c == ' ' {
                // A space at the edge ends the row and goes.
                lines.push(start..i);
                i += 1;
                start = i;
                width = 0;
                space = None;
                continue;
            }
            match space {
                Some(at) if at > start => {
                    lines.push(start..at);
                    start = at + 1;
                }
                _ => {
                    lines.push(start..i);
                    start = i;
                }
            }
            width = chars[start..i].iter().map(|c| char_width(*c)).sum();
            space = None;
            continue;
        }
        if c == ' ' {
            space = Some(i);
        }
        width += w;
        i += 1;
    }
    lines.push(start..chars.len());
    lines
}

/// What the engine prepares once per text.
pub struct Prepared {
    chars: Vec<char>,
    max_lines: u32,
    no_wrap: bool,
}

/// A text laid out in a box: its rows.
#[derive(Debug, Clone)]
pub struct CellLayout {
    pub chars: Vec<char>,
    pub lines: Vec<Range<usize>>,
    /// The last row was cut short: it ends in an ellipsis.
    pub truncated: bool,
}

impl CellLayout {
    /// The UTF-16 offset of character `index`.
    pub fn utf16_at(&self, index: usize) -> u32 {
        self.chars[..index.min(self.chars.len())].iter().map(|c| c.len_utf16() as u32).sum()
    }
}

fn columns(width: Points) -> usize {
    ((width.0 + 0.01) / CELL_W).floor().max(0.0) as usize
}

fn lines_of(prepared: &Prepared, max: Option<usize>) -> (Vec<Range<usize>>, bool) {
    let mut lines = wrap(&prepared.chars, if prepared.no_wrap { None } else { max });
    let limit = prepared.max_lines as usize;
    if limit == 0 || lines.len() <= limit {
        return (lines, false);
    }
    lines.truncate(limit);
    // The last row takes what fits of the rest, by character, and leaves a
    // column for the ellipsis.
    if let (Some(last), Some(max)) = (lines.last_mut(), max) {
        let mut width = 0;
        let mut end = last.start;
        while end < prepared.chars.len() && prepared.chars[end] != '\n' {
            let w = char_width(prepared.chars[end]);
            if width + w + 1 > max {
                break;
            }
            width += w;
            end += 1;
        }
        // The ellipsis goes after the row.
        last.end = end;
    }
    (lines, true)
}

/// Lays text out in cells.
#[derive(Clone, Copy, Default)]
pub struct CellTextEngine;

impl TextEngine for CellTextEngine {
    type Prepared = Prepared;

    fn prepare(&self, text: &AttributedText) -> Prepared {
        Prepared { chars: text.text.chars().collect(), max_lines: text.max_lines, no_wrap: text.no_wrap }
    }

    fn measure(&self, prepared: &Prepared, max_width: Option<Points>) -> Size<Points> {
        let max = max_width.map(columns);
        let (lines, _) = lines_of(prepared, max);
        let mut widest = lines
            .iter()
            .map(|l| prepared.chars[l.clone()].iter().map(|c| char_width(*c)).sum::<usize>())
            .max()
            .unwrap_or(0);
        if let Some(max) = max {
            widest = widest.min(max);
        }
        size(px(widest as f32 * CELL_W), px(lines.len().max(1) as f32 * CELL_H))
    }

    fn layout(&self, prepared: &Prepared, area: Size<Points>) -> TextLayout {
        let (lines, truncated) = lines_of(prepared, Some(columns(area.width).max(1)));
        TextLayout(Arc::new(CellLayout { chars: prepared.chars.clone(), lines, truncated }))
    }

    fn lines(&self, layout: &TextLayout) -> Vec<TextLine> {
        let Some(layout) = layout.downcast::<CellLayout>() else { return Vec::new() };
        layout
            .lines
            .iter()
            .enumerate()
            .map(|(row, range)| {
                let width = layout.chars[range.clone()].iter().map(|c| char_width(*c)).sum::<usize>();
                TextLine {
                    start: layout.utf16_at(range.start),
                    end: layout.utf16_at(range.end),
                    y: px(row as f32 * CELL_H),
                    width: px(width as f32 * CELL_W),
                    height: px(CELL_H),
                    ascender: px(CELL_H * 0.8),
                    descender: px(CELL_H * 0.2),
                    cap_height: px(CELL_H * 0.6),
                    x_height: px(CELL_H * 0.45),
                    ..TextLine::default()
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(text: &str, columns: usize) -> Vec<String> {
        let chars: Vec<char> = text.chars().collect();
        wrap(&chars, Some(columns)).into_iter().map(|r| chars[r].iter().collect()).collect()
    }

    #[test]
    fn words_wrap_whole_and_breaks_stay() {
        assert_eq!(rows("hello brave new world", 11), ["hello brave", "new world"]);
        assert_eq!(rows("abcdefghij", 4), ["abcd", "efgh", "ij"]);
        assert_eq!(rows("one\n\ntwo", 10), ["one", "", "two"]);
        assert_eq!(rows("", 10), [""]);
        // Wide characters take two columns.
        assert_eq!(rows("你好世界", 5), ["你好", "世界"]);
    }

    #[test]
    fn measuring_counts_cells() {
        let engine = CellTextEngine;
        let text = AttributedText { text: "hello world".into(), ..Default::default() };
        let prepared = engine.prepare(&text);
        assert_eq!(engine.measure(&prepared, None), size(px(88.0), px(16.0)));
        assert_eq!(engine.measure(&prepared, Some(px(48.0))), size(px(40.0), px(32.0)));
    }
}
