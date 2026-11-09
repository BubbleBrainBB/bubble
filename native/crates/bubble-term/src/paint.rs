//! The views drawn into a grid of cells: backgrounds, borders in box
//! characters, text with its colors and weights, inputs with their caret,
//! scroll views clipped and moved by their offsets.

use nsh_core::geometry::{Bounds, Point, Points, point};
use nsh_core::Color;
use nsh_core::{NodeId, ROOT_ID, TextAlign};

use crate::model::{Node, Tree, row_of};
use crate::text::{CELL_H, CELL_W, CellLayout, char_width, wrap};

pub const BOLD: u8 = 1;
pub const DIM: u8 = 2;
pub const ITALIC: u8 = 4;
pub const UNDERLINE: u8 = 8;
pub const REVERSE: u8 = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cell {
    pub ch: char,
    /// RGB; `None`: the terminal's own.
    pub fg: Option<u32>,
    pub bg: Option<u32>,
    pub attrs: u8,
    /// The right half of a wide character drawn in the cell before.
    pub tail: bool,
}

impl Default for Cell {
    fn default() -> Self {
        Cell { ch: ' ', fg: None, bg: None, attrs: 0, tail: false }
    }
}

/// A box of cells: left, top, right, bottom (exclusive ends).
pub type Area = (i32, i32, i32, i32);

fn intersect(a: Area, b: Area) -> Area {
    (a.0.max(b.0), a.1.max(b.1), a.2.min(b.2), a.3.min(b.3))
}

fn is_empty(a: Area) -> bool {
    a.0 >= a.2 || a.1 >= a.3
}

#[derive(Clone, Debug, PartialEq)]
pub struct Grid {
    pub width: u16,
    pub height: u16,
    pub cells: Vec<Cell>,
}

impl Grid {
    pub fn new(width: u16, height: u16) -> Grid {
        Grid { width, height, cells: vec![Cell::default(); usize::from(width) * usize::from(height)] }
    }

    pub fn get(&self, x: u16, y: u16) -> Option<&Cell> {
        (x < self.width && y < self.height).then(|| &self.cells[usize::from(y) * usize::from(self.width) + usize::from(x)])
    }

    fn at(&mut self, x: i32, y: i32) -> Option<&mut Cell> {
        if x < 0 || y < 0 || x >= i32::from(self.width) || y >= i32::from(self.height) {
            return None;
        }
        let w = usize::from(self.width);
        Some(&mut self.cells[y as usize * w + x as usize])
    }

    /// Writes `c` at `x, y` inside `clip`; a wide character takes the next
    /// cell too. The columns it took.
    fn put(&mut self, x: i32, y: i32, c: char, fg: Option<u32>, attrs: u8, clip: Area) -> i32 {
        let w = char_width(c).max(1) as i32;
        if y < clip.1 || y >= clip.3 || x < clip.0 || x + w > clip.2 {
            return w;
        }
        // A wide character cut in two leaves a blank.
        if let Some(cell) = self.at(x, y)
            && cell.tail
            && let Some(before) = self.at(x - 1, y)
        {
            before.ch = ' ';
        }
        let shown = if c == '\t' || c.is_control() { ' ' } else { c };
        if let Some(cell) = self.at(x, y) {
            cell.ch = shown;
            cell.fg = fg;
            cell.attrs = attrs;
            cell.tail = false;
        }
        for i in 1..w {
            if let Some(cell) = self.at(x + i, y) {
                cell.ch = if c == '\t' { ' ' } else { '\0' };
                cell.tail = c != '\t';
                cell.fg = fg;
                cell.attrs = attrs;
            }
        }
        w
    }

    /// The rows as plain text (for tests): wide characters once, trailing
    /// spaces dropped.
    pub fn lines(&self) -> Vec<String> {
        (0..self.height)
            .map(|y| {
                let row: String = (0..self.width)
                    .filter_map(|x| self.get(x, y))
                    .filter(|c| !c.tail)
                    .map(|c| c.ch)
                    .collect();
                row.trim_end().to_owned()
            })
            .collect()
    }

    pub fn text(&self) -> String {
        self.lines().join("\n")
    }
}

/// A drawn screen: its cells, where views are (for clicks and the wheel,
/// topmost last) and where the caret is.
pub struct Painted {
    pub grid: Grid,
    pub hits: Vec<(NodeId, Area)>,
    pub cursor: Option<(u16, u16)>,
}

impl Painted {
    /// The topmost view at `cell` that hears something.
    pub fn hit(&self, cell: (u16, u16)) -> Option<NodeId> {
        self.hits.iter().rev().find(|(_, area)| crate::model::contains(*area, cell)).map(|(id, _)| *id)
    }
}

/// An RGBA color as RGB, `None` when invisible.
fn rgb(color: Color) -> Option<u32> {
    let a = color.0 & 0xff;
    (a >= 0x40).then_some(color.0 >> 8)
}

/// A text color: the default (opaque black) is the terminal's own.
fn ink(color: Color) -> Option<u32> {
    if color.0 == 0x0000_00ff { None } else { rgb(color) }
}

fn cells(origin: Point<Points>, size: nsh_core::geometry::Size<Points>) -> Area {
    let l = (origin.x.0 / CELL_W).round() as i32;
    let t = (origin.y.0 / CELL_H).round() as i32;
    let r = ((origin.x.0 + size.width.0) / CELL_W).round() as i32;
    let b = ((origin.y.0 + size.height.0) / CELL_H).round() as i32;
    (l, t, r, b)
}

pub fn paint(tree: &Tree, width: u16, height: u16) -> Painted {
    let mut painted = Painted { grid: Grid::new(width, height), hits: Vec::new(), cursor: None };
    let screen = (0, 0, i32::from(width), i32::from(height));
    paint_node(tree, ROOT_ID, point(Points::ZERO, Points::ZERO), screen, 0, &mut painted);
    painted
}

fn paint_node(tree: &Tree, id: NodeId, origin: Point<Points>, clip: Area, inherited: u8, out: &mut Painted) {
    let Some(node) = tree.node(id) else { return };
    let style = &node.style;
    if style.hidden || style.opacity <= 0.05 {
        return;
    }
    let attrs = inherited | if style.opacity < 0.7 { DIM } else { 0 };
    let at = point(origin.x + node.frame.origin.x, origin.y + node.frame.origin.y);
    let area = if id == ROOT_ID { clip } else { cells(at, node.frame.size) };
    let visible = intersect(area, clip);
    if !is_empty(visible) {
        if let Some(bg) = rgb(style.background_color) {
            for y in visible.1..visible.3 {
                for x in visible.0..visible.2 {
                    if let Some(cell) = out.grid.at(x, y) {
                        *cell = Cell { bg: Some(bg), ..Cell::default() };
                    }
                }
            }
        }
        borders(node, area, visible, attrs, &mut out.grid);
        if let Some(text) = &node.text {
            paint_text(text, at, area, visible, attrs, &mut out.grid);
        }
        if node.input.is_some() {
            paint_input(tree, id, node, at, visible, attrs, out);
        }
        if !node.events.is_empty() || node.input.is_some() || node.scroll.is_some() {
            out.hits.push((id, visible));
        }
    }
    let clips = node.scroll.is_some() || style.overflow_hidden;
    let inner = if clips { visible } else { clip };
    if clips && is_empty(inner) {
        return;
    }
    let offset = node.scroll.as_ref().map(|s| s.offset).unwrap_or_default();
    let child_origin = point(at.x - offset.x, at.y - offset.y);
    for child in &node.children {
        paint_node(tree, *child, child_origin, inner, attrs, out);
    }
    if let Some(scroll) = &node.scroll
        && !scroll.horizontal
    {
        scrollbar(scroll, node, area, visible, &mut out.grid);
    }
}

/// A thin bar at the right edge of a scroll view whose content is taller.
fn scrollbar(scroll: &crate::model::Scroll, node: &Node, area: Area, visible: Area, grid: &mut Grid) {
    let rows = area.3 - area.1;
    let content = (scroll.content.height.0 / CELL_H).ceil() as i32;
    if rows <= 0 || content <= rows || area.2 - 1 >= visible.2 {
        return;
    }
    let _ = node;
    let thumb = ((rows * rows) as f32 / content as f32).ceil().max(1.0) as i32;
    let top = ((scroll.offset.y.0 / CELL_H) * rows as f32 / content as f32).round() as i32;
    let x = area.2 - 1;
    for y in area.1.max(visible.1)..area.3.min(visible.3) {
        let on = y - area.1 >= top && y - area.1 < top + thumb;
        if on && let Some(cell) = grid.at(x, y) {
            cell.ch = '┃';
            cell.fg = Some(0x80_80_80);
            cell.tail = false;
            cell.attrs = DIM;
        }
    }
}

fn borders(node: &Node, area: Area, clip: Area, attrs: u8, grid: &mut Grid) {
    let borders = &node.style.borders;
    let w = borders.widths;
    let (top, right, bottom, left) = (w.top.0 > 0.0, w.right.0 > 0.0, w.bottom.0 > 0.0, w.left.0 > 0.0);
    if !(top || right || bottom || left) {
        return;
    }
    let round = node.style.border_radius.0 > 0.0;
    let color = |i: usize| rgb(borders.colors[i]);
    let (l, t, r, b) = (area.0, area.1, area.2 - 1, area.3 - 1);
    if r < l || b < t {
        return;
    }
    if top {
        for x in l..=r {
            grid.put(x, t, '─', color(0), attrs, clip);
        }
    }
    if bottom && b > t {
        for x in l..=r {
            grid.put(x, b, '─', color(2), attrs, clip);
        }
    }
    if left {
        for y in t..=b {
            grid.put(l, y, '│', color(3), attrs, clip);
        }
    }
    if right && r > l {
        for y in t..=b {
            grid.put(r, y, '│', color(1), attrs, clip);
        }
    }
    let corners = [
        (top && left, l, t, if round { '╭' } else { '┌' }, color(0)),
        (top && right, r, t, if round { '╮' } else { '┐' }, color(0)),
        (bottom && left, l, b, if round { '╰' } else { '└' }, color(2)),
        (bottom && right, r, b, if round { '╯' } else { '┘' }, color(2)),
    ];
    for (on, x, y, c, fg) in corners {
        if on {
            grid.put(x, y, c, fg, attrs, clip);
        }
    }
}

fn span_attrs(style: &nsh_core::ResolvedTextStyle) -> u8 {
    let mut attrs = 0;
    if style.font_weight >= 600 {
        attrs |= BOLD;
    }
    if style.italic {
        attrs |= ITALIC;
    }
    if style.decoration.underline {
        attrs |= UNDERLINE;
    }
    attrs
}

fn paint_text(content: &crate::model::TextContent, at: Point<Points>, area: Area, clip: Area, attrs: u8, grid: &mut Grid) {
    let Some(layout) = content.layout.downcast::<CellLayout>() else { return };
    let text = &content.text;
    // Each character's style, from the spans' UTF-16 ranges.
    let mut styles = Vec::with_capacity(layout.chars.len());
    let mut unit = 0u32;
    let mut span = 0usize;
    for c in &layout.chars {
        while span < text.spans.len() && text.spans[span].end <= unit {
            span += 1;
        }
        let style = text.spans.get(span).filter(|s| s.start <= unit).or(text.spans.last());
        styles.push(style.map(|s| (ink(s.style.color), span_attrs(&s.style))).unwrap_or((None, 0)));
        unit += c.len_utf16() as u32;
    }
    let origin = point(at.x + content.content.origin.x, at.y + content.content.origin.y);
    let box_area = cells(origin, content.content.size);
    let columns = box_area.2 - box_area.0;
    let clip = intersect(clip, intersect(area, (box_area.0, box_area.1, box_area.2.max(box_area.0 + 1), i32::MAX)));
    for (row, range) in layout.lines.iter().enumerate() {
        let y = box_area.1 + row as i32;
        let width: i32 = layout.chars[range.clone()].iter().map(|c| char_width(*c) as i32).sum();
        let mut x = box_area.0
            + match text.align {
                TextAlign::Center => ((columns - width) / 2).max(0),
                TextAlign::Right => (columns - width).max(0),
                _ => 0,
            };
        let last = row + 1 == layout.lines.len();
        let mut style = (None, 0);
        for i in range.clone() {
            style = styles[i];
            x += grid.put(x, y, layout.chars[i], style.0, attrs | style.1, clip);
        }
        if last && layout.truncated {
            grid.put(x, y, '…', style.0, attrs | style.1, clip);
        }
    }
}

fn paint_input(tree: &Tree, id: NodeId, node: &Node, at: Point<Points>, clip: Area, attrs: u8, out: &mut Painted) {
    let Some(input) = &node.input else { return };
    let content: Bounds<Points> = input.content.unwrap_or(Bounds::new(point(Points::ZERO, Points::ZERO), node.frame.size));
    let area = cells(point(at.x + content.origin.x, at.y + content.origin.y), content.size);
    let clip = intersect(clip, area);
    if is_empty(clip) {
        return;
    }
    let columns = (area.2 - area.0).max(1) as usize;
    let rows = (area.3 - area.1).max(1) as usize;
    let focused = tree.focused == Some(id);
    let config = &input.config;
    let empty = input.text.is_empty();
    let shown: Vec<char> = if empty {
        config.placeholder.chars().collect()
    } else if config.secure {
        input.text.chars().map(|_| '•').collect()
    } else {
        input.text.chars().collect()
    };
    let (fg, a) = if empty {
        (config.placeholder_color.and_then(rgb).or(Some(0x80_80_80)), attrs)
    } else {
        (ink(config.style.color), attrs | span_attrs(&config.style))
    };
    let caret = if empty { 0 } else { input.caret.min(shown.len()) };
    if config.multiline {
        let lines = wrap(&shown, Some(columns));
        let caret_row = row_of(&lines, caret);
        let first = caret_row.saturating_sub(rows - 1);
        for (i, range) in lines.iter().enumerate().skip(first).take(rows) {
            let y = area.1 + (i - first) as i32;
            let mut x = area.0;
            for c in &shown[range.clone()] {
                x += out.grid.put(x, y, *c, fg, a, clip);
            }
        }
        if focused {
            let range = &lines[caret_row];
            let column: usize = shown[range.start..caret.max(range.start)].iter().map(|c| char_width(*c)).sum();
            let y = area.1 + (caret_row - first) as i32;
            let x = area.0 + column.min(columns - 1) as i32;
            if x >= clip.0 && x < clip.2 && y >= clip.1 && y < clip.3 {
                out.cursor = Some((x as u16, y as u16));
            }
        }
    } else {
        // One row, moved along to keep the caret in view.
        let before: usize = shown[..caret].iter().map(|c| char_width(*c)).sum();
        let skip = before.saturating_sub(columns - 1);
        let mut x = area.0;
        let mut column = 0;
        for c in &shown {
            let w = char_width(*c);
            if column >= skip {
                x += out.grid.put(x, area.1, *c, fg, a, clip);
            }
            column += w;
        }
        if focused {
            let x = area.0 + (before - skip) as i32;
            if x < clip.2 {
                out.cursor = Some((x as u16, area.1 as u16));
            }
        }
    }
}
