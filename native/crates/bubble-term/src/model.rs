//! The native views as the engine's batches leave them, and what the
//! person's keys, clicks and wheel do to them: the events sent back.

use std::collections::HashMap;
use std::sync::Arc;

use nsh_core::geometry::{Bounds, Point, Points, Size, point, px};
use nsh_core::keymap::{Key, KeyMatch, Keymap, Keystroke};
use nsh_core::ViewStyle;
use nsh_core::tailwind::state::FOCUSED;
use nsh_core::{
    AttributedText, Command, EventKind, EventMask, EventPayload, InputConfig, MountBatch, MountItem, NodeId, PayloadValue,
    ROOT_ID, SubmitBehavior, TextLayout, ViewKind,
};

use crate::text::{CELL_H, CELL_W, char_width, wrap};

/// What the model asks of the engine (or the terminal).
#[derive(Debug)]
pub enum Out {
    Event(NodeId, EventKind, EventPayload),
    Scroll(NodeId, Point<Points>, u32),
    States(NodeId, u16),
    Clipboard(String),
    Exit,
}

pub struct TextContent {
    pub text: Arc<AttributedText>,
    pub layout: TextLayout,
    pub content: Bounds<Points>,
}

pub struct Input {
    pub config: InputConfig,
    pub text: String,
    /// Edits made here: a value from the engine based on fewer is stale.
    pub count: u32,
    /// The caret, in characters.
    pub caret: usize,
    pub content: Option<Bounds<Points>>,
    /// The column the caret keeps going up and down.
    goal: Option<usize>,
}

#[derive(Default)]
pub struct Scroll {
    pub horizontal: bool,
    pub content: Size<Points>,
    pub offset: Point<Points>,
    pub applied: u32,
}

pub struct Node {
    pub kind: ViewKind,
    pub parent: Option<NodeId>,
    pub children: Vec<NodeId>,
    pub frame: Bounds<Points>,
    pub style: Box<ViewStyle>,
    pub text: Option<TextContent>,
    pub events: EventMask,
    pub input: Option<Input>,
    pub scroll: Option<Scroll>,
    pub label: Option<Arc<str>>,
    tracked: u16,
    states: u16,
}

impl Node {
    fn new(kind: ViewKind) -> Node {
        Node {
            kind,
            parent: None,
            children: Vec::new(),
            frame: Bounds::default(),
            style: Box::default(),
            text: None,
            events: EventMask::default(),
            input: None,
            scroll: None,
            label: None,
            tracked: 0,
            states: 0,
        }
    }

    pub fn hears(&self, kind: EventKind) -> bool {
        self.events.contains(kind)
    }
}

pub struct Tree {
    pub nodes: HashMap<NodeId, Node>,
    pub focused: Option<NodeId>,
    keymap: Option<Arc<Keymap>>,
}

impl Default for Tree {
    fn default() -> Self {
        Tree::new()
    }
}

/// A click's or the wheel's place: a column and row.
pub type Cell = (u16, u16);

impl Tree {
    pub fn new() -> Tree {
        let mut nodes = HashMap::new();
        nodes.insert(ROOT_ID, Node::new(ViewKind::View));
        Tree { nodes, focused: None, keymap: None }
    }

    pub fn node(&self, id: NodeId) -> Option<&Node> {
        self.nodes.get(&id)
    }

    fn detach(&mut self, id: NodeId) {
        if let Some(parent) = self.nodes.get_mut(&id).and_then(|n| n.parent.take())
            && let Some(parent) = self.nodes.get_mut(&parent)
        {
            parent.children.retain(|c| *c != id);
        }
    }

    /// Applies a batch; the scrolls it asked for are reported.
    pub fn apply(&mut self, batch: &MountBatch, out: &mut Vec<Out>) {
        let mut ends = Vec::new();
        let mut scrolled = Vec::new();
        // Scroll views showing their end keep showing it when they get
        // shorter (a panel opening under a conversation).
        let at_end: Vec<(NodeId, Points)> = self
            .nodes
            .iter()
            .filter_map(|(id, node)| {
                let scroll = node.scroll.as_ref()?;
                let max = scroll.content.height - node.frame.size.height;
                (!scroll.horizontal && max > Points::ZERO && scroll.offset.y.0 >= max.0 - 0.5)
                    .then_some((*id, node.frame.size.height))
            })
            .collect();
        for item in &batch.items {
            match item {
                MountItem::Create { id, kind } => {
                    let mut node = Node::new(*kind);
                    if *kind == ViewKind::Scroll {
                        node.scroll = Some(Scroll::default());
                    }
                    self.nodes.insert(*id, node);
                }
                MountItem::Insert { parent, child, before } => {
                    self.detach(*child);
                    if let Some(node) = self.nodes.get_mut(child) {
                        node.parent = Some(*parent);
                    }
                    if let Some(parent) = self.nodes.get_mut(parent) {
                        let at = before.and_then(|b| parent.children.iter().position(|c| *c == b)).unwrap_or(parent.children.len());
                        parent.children.insert(at, *child);
                    }
                }
                MountItem::Remove { id } => self.detach(*id),
                MountItem::Delete { id } => {
                    self.detach(*id);
                    if let Some(node) = self.nodes.remove(id) {
                        for child in node.children {
                            if let Some(child) = self.nodes.get_mut(&child) {
                                child.parent = None;
                            }
                        }
                    }
                    if self.focused == Some(*id) {
                        self.focused = None;
                    }
                }
                MountItem::Frame { id, frame } => {
                    if let Some(node) = self.nodes.get_mut(id) {
                        node.frame = *frame;
                    }
                }
                MountItem::ViewStyle { id, style } => {
                    if let Some(node) = self.nodes.get_mut(id) {
                        node.style = style.clone();
                    }
                }
                MountItem::Text { id, text, layout, content } => {
                    if let Some(node) = self.nodes.get_mut(id) {
                        node.text = Some(TextContent { text: text.clone(), layout: layout.clone(), content: *content });
                    }
                }
                MountItem::Events { id, events } => {
                    if let Some(node) = self.nodes.get_mut(id) {
                        node.events = *events;
                    }
                }
                MountItem::TrackStates { id, states } => {
                    if let Some(node) = self.nodes.get_mut(id) {
                        node.tracked = *states;
                        node.states &= *states;
                    }
                }
                MountItem::AccessibilityLabel { id, label } => {
                    if let Some(node) = self.nodes.get_mut(id) {
                        node.label = label.clone();
                    }
                }
                MountItem::ScrollConfig { id, horizontal, .. } => {
                    if let Some(node) = self.nodes.get_mut(id) {
                        node.scroll.get_or_insert_with(Scroll::default).horizontal = *horizontal;
                    }
                }
                MountItem::ScrollContent { id, size } => {
                    if let Some(scroll) = self.nodes.get_mut(id).and_then(|n| n.scroll.as_mut()) {
                        scroll.content = *size;
                    }
                }
                MountItem::ScrollBy { id, dx, dy } => {
                    if let Some(scroll) = self.nodes.get_mut(id).and_then(|n| n.scroll.as_mut()) {
                        scroll.offset = point(scroll.offset.x + *dx, scroll.offset.y + *dy);
                        scroll.applied = scroll.applied.wrapping_add(1);
                        scrolled.push(*id);
                    }
                }
                MountItem::Input { id, config } => {
                    let auto = config.auto_focus;
                    if let Some(node) = self.nodes.get_mut(id) {
                        match &mut node.input {
                            Some(input) => input.config = (**config).clone(),
                            None => {
                                node.input = Some(Input {
                                    config: (**config).clone(),
                                    text: String::new(),
                                    count: 0,
                                    caret: 0,
                                    content: None,
                                    goal: None,
                                })
                            }
                        }
                    }
                    if auto && self.focused.is_none() {
                        self.focus(Some(*id), out);
                    }
                }
                MountItem::InputValue { id, text, count } => {
                    if let Some(input) = self.nodes.get_mut(id).and_then(|n| n.input.as_mut())
                        && *count >= input.count
                    {
                        let at_end = input.caret >= input.text.chars().count();
                        input.text = text.clone();
                        let length = input.text.chars().count();
                        input.caret = if at_end { length } else { input.caret.min(length) };
                    }
                }
                MountItem::InputContent { id, content } => {
                    if let Some(input) = self.nodes.get_mut(id).and_then(|n| n.input.as_mut()) {
                        input.content = Some(*content);
                    }
                }
                MountItem::Keymap(keymap) => self.keymap = Some(keymap.clone()),
                MountItem::Command { id, command } => match command {
                    Command::Focus => {
                        if self.nodes.get(id).is_some_and(|n| n.input.is_some()) {
                            self.focus(Some(*id), out);
                        }
                    }
                    Command::Blur => {
                        if self.focused == Some(*id) {
                            self.focus(None, out);
                        }
                    }
                    Command::ScrollTo { x, y, .. } => {
                        if let Some(scroll) = self.nodes.get_mut(id).and_then(|n| n.scroll.as_mut()) {
                            scroll.offset = point(*x, *y);
                            scroll.applied = scroll.applied.wrapping_add(1);
                            scrolled.push(*id);
                        }
                    }
                    // After the batch: its content may change in it.
                    Command::ScrollToEnd { .. } => ends.push(*id),
                    Command::SetClipboard(text) => out.push(Out::Clipboard(text.clone())),
                    Command::ExitApp => out.push(Out::Exit),
                    _ => {}
                },
                _ => {}
            }
        }
        for (id, height) in at_end {
            if let Some(node) = self.nodes.get(&id)
                && node.frame.size.height.0 < height.0 - 0.5
                && !ends.contains(&id)
            {
                ends.push(id);
            }
        }
        for id in ends {
            if let Some(node) = self.nodes.get_mut(&id)
                && let Some(scroll) = node.scroll.as_mut()
            {
                if scroll.horizontal {
                    scroll.offset.x = (scroll.content.width - node.frame.size.width).max(Points::ZERO);
                } else {
                    scroll.offset.y = (scroll.content.height - node.frame.size.height).max(Points::ZERO);
                }
                scroll.applied = scroll.applied.wrapping_add(1);
                scrolled.push(id);
            }
        }
        // Content that shrank pulls the offsets back.
        for node in self.nodes.values_mut() {
            if let Some(scroll) = node.scroll.as_mut() {
                let max_y = (scroll.content.height - node.frame.size.height).max(Points::ZERO);
                let max_x = (scroll.content.width - node.frame.size.width).max(Points::ZERO);
                scroll.offset = point(scroll.offset.x.min(max_x).max(Points::ZERO), scroll.offset.y.min(max_y).max(Points::ZERO));
            }
        }
        scrolled.dedup();
        for id in scrolled {
            if let Some(scroll) = self.nodes.get(&id).and_then(|n| n.scroll.as_ref()) {
                out.push(Out::Scroll(id, scroll.offset, scroll.applied));
            }
        }
    }

    fn send(&self, id: NodeId, kind: EventKind, payload: EventPayload, out: &mut Vec<Out>) {
        if self.nodes.get(&id).is_some_and(|n| n.hears(kind)) {
            out.push(Out::Event(id, kind, payload));
        }
    }

    fn set_state(&mut self, id: NodeId, bit: u16, on: bool, out: &mut Vec<Out>) {
        if let Some(node) = self.nodes.get_mut(&id) {
            let before = node.states;
            node.states = if on { node.states | bit } else { node.states & !bit } & node.tracked;
            if node.states != before {
                out.push(Out::States(id, node.states));
            }
        }
    }

    /// Moves the keyboard focus to input `to`.
    pub fn focus(&mut self, to: Option<NodeId>, out: &mut Vec<Out>) {
        if self.focused == to {
            return;
        }
        if let Some(old) = self.focused.take() {
            self.set_state(old, FOCUSED, false, out);
            self.send(old, EventKind::Blur, EventPayload::None, out);
        }
        if let Some(new) = to {
            self.focused = Some(new);
            self.set_state(new, FOCUSED, true, out);
            self.send(new, EventKind::Focus, EventPayload::None, out);
        }
    }

    /// The inputs that take the focus, in the order they are drawn.
    fn inputs(&self) -> Vec<NodeId> {
        let mut found = Vec::new();
        let mut stack = vec![ROOT_ID];
        while let Some(id) = stack.pop() {
            let Some(node) = self.nodes.get(&id) else { continue };
            if node.style.hidden {
                continue;
            }
            if node.input.as_ref().is_some_and(|i| i.config.editable) {
                found.push(id);
            }
            stack.extend(node.children.iter().rev());
        }
        found
    }

    fn changed(&mut self, id: NodeId, out: &mut Vec<Out>) {
        let Some(input) = self.nodes.get_mut(&id).and_then(|n| n.input.as_mut()) else { return };
        input.count += 1;
        input.goal = None;
        let payload = EventPayload::Text { text: input.text.clone(), count: input.count };
        let caret = input.text.chars().take(input.caret).map(|c| c.len_utf16() as f64).sum::<f64>();
        // Always reported: the engine keeps the text for layout.
        out.push(Out::Event(id, EventKind::ChangeText, payload));
        let at = PayloadValue::Number(caret);
        self.send(id, EventKind::SelectionChange, EventPayload::Map(vec![("start", at.clone()), ("end", at)]), out);
    }

    /// A key the person pressed: the app's shortcuts take it first, then
    /// the focused input.
    pub fn key(&mut self, stroke: Keystroke, out: &mut Vec<Out>) {
        let editing = self.focused.is_some();
        if let Some(keymap) = &self.keymap
            && let KeyMatch::Fire(id) = keymap.find(stroke, editing, false)
        {
            out.push(Out::Event(ROOT_ID, EventKind::KeyCommand, EventPayload::Value(PayloadValue::Number(f64::from(id)))));
            return;
        }
        let m = stroke.modifiers;
        if stroke.key == Key::Tab && !m.ctrl && !m.alt {
            let inputs = self.inputs();
            if !inputs.is_empty() {
                let at = self.focused.and_then(|f| inputs.iter().position(|i| *i == f));
                let next = match (at, m.shift) {
                    (None, _) => 0,
                    (Some(i), false) => (i + 1) % inputs.len(),
                    (Some(i), true) => (i + inputs.len() - 1) % inputs.len(),
                };
                self.focus(Some(inputs[next]), out);
            }
            return;
        }
        let Some(id) = self.focused else { return };
        self.edit(id, stroke, out);
    }

    fn edit(&mut self, id: NodeId, stroke: Keystroke, out: &mut Vec<Out>) {
        let width = self.input_columns(id);
        let Some(input) = self.nodes.get_mut(&id).and_then(|n| n.input.as_mut()) else { return };
        if !input.config.editable {
            return;
        }
        let m = stroke.modifiers;
        let multiline = input.config.multiline;
        let mut chars: Vec<char> = input.text.chars().collect();
        let caret = input.caret.min(chars.len());
        let line_start = chars[..caret].iter().rposition(|c| *c == '\n').map_or(0, |i| i + 1);
        let line_end = chars[caret..].iter().position(|c| *c == '\n').map_or(chars.len(), |i| caret + i);
        let word_start = {
            let mut i = caret;
            while i > 0 && chars[i - 1].is_whitespace() {
                i -= 1;
            }
            while i > 0 && !chars[i - 1].is_whitespace() {
                i -= 1;
            }
            i
        };
        let word_end = {
            let mut i = caret;
            while i < chars.len() && chars[i].is_whitespace() {
                i += 1;
            }
            while i < chars.len() && !chars[i].is_whitespace() {
                i += 1;
            }
            i
        };
        enum Do {
            Move(usize),
            Replace(std::ops::Range<usize>, String),
            Vertical(bool),
            Return,
            Nothing,
        }
        let newline = Do::Replace(caret..caret, "\n".into());
        let action = match stroke.key {
            Key::Enter if multiline && (m.shift || m.alt) => newline,
            Key::Char('j') if multiline && m.ctrl => newline,
            Key::Enter => Do::Return,
            Key::Char('a') if m.ctrl => Do::Move(line_start),
            Key::Char('e') if m.ctrl => Do::Move(line_end),
            Key::Char('b') if m.ctrl => Do::Move(caret.saturating_sub(1)),
            Key::Char('f') if m.ctrl => Do::Move((caret + 1).min(chars.len())),
            Key::Char('u') if m.ctrl => Do::Replace(line_start..caret, String::new()),
            Key::Char('k') if m.ctrl => {
                Do::Replace(caret..if line_end == caret { (caret + 1).min(chars.len()) } else { line_end }, String::new())
            }
            Key::Char('w') if m.ctrl => Do::Replace(word_start..caret, String::new()),
            Key::Char('h') if m.ctrl => Do::Replace(caret.saturating_sub(1)..caret, String::new()),
            Key::Char('d') if m.ctrl => Do::Replace(caret..(caret + 1).min(chars.len()), String::new()),
            Key::Backspace if m.alt || m.ctrl => Do::Replace(word_start..caret, String::new()),
            Key::Backspace => Do::Replace(caret.saturating_sub(1)..caret, String::new()),
            Key::Delete => Do::Replace(caret..(caret + 1).min(chars.len()), String::new()),
            Key::Left if m.alt || m.ctrl => Do::Move(word_start),
            Key::Right if m.alt || m.ctrl => Do::Move(word_end),
            Key::Left => Do::Move(caret.saturating_sub(1)),
            Key::Right => Do::Move((caret + 1).min(chars.len())),
            Key::Home => Do::Move(line_start),
            Key::End => Do::Move(line_end),
            Key::Up => Do::Vertical(true),
            Key::Down => Do::Vertical(false),
            Key::Char(c) if !m.ctrl && !m.cmd && !(m.alt && c.is_ascii_alphabetic()) => Do::Replace(caret..caret, c.to_string()),
            _ => Do::Nothing,
        };
        match action {
            Do::Nothing => {}
            Do::Move(to) => {
                input.caret = to;
                input.goal = None;
            }
            Do::Vertical(up) => {
                let rows = wrap(&chars, width);
                let row = row_of(&rows, caret);
                let column = input.goal.unwrap_or_else(|| chars[rows[row].start..caret].iter().map(|c| char_width(*c)).sum());
                let target = if up { row.checked_sub(1) } else { Some(row + 1).filter(|r| *r < rows.len()) };
                input.caret = match target {
                    Some(target) => index_at(&chars, &rows[target], column),
                    None if up => 0,
                    None => chars.len(),
                };
                input.goal = Some(column);
            }
            Do::Replace(range, with) => {
                if range.is_empty() && with.is_empty() {
                    return;
                }
                if let Some(max) = input.config.max_length {
                    let length: usize = chars.iter().map(|c| c.len_utf16()).sum::<usize>() + with.encode_utf16().count();
                    if !with.is_empty() && length > max as usize {
                        return;
                    }
                }
                let caret = range.start + with.chars().count();
                chars.splice(range, with.chars());
                input.text = chars.into_iter().collect();
                input.caret = caret;
                self.changed(id, out);
            }
            Do::Return => {
                let behavior = input.config.submit();
                if behavior == SubmitBehavior::Newline {
                    chars.insert(caret, '\n');
                    input.text = chars.into_iter().collect();
                    input.caret = caret + 1;
                    self.changed(id, out);
                    return;
                }
                let payload = EventPayload::Text { text: input.text.clone(), count: input.count };
                self.send(id, EventKind::SubmitEditing, payload, out);
                if behavior == SubmitBehavior::BlurAndSubmit {
                    self.focus(None, out);
                }
            }
        }
    }

    /// Text pasted into the focused input.
    pub fn paste(&mut self, text: &str, out: &mut Vec<Out>) {
        let Some(id) = self.focused else { return };
        let Some(input) = self.nodes.get_mut(&id).and_then(|n| n.input.as_mut()) else { return };
        let text = nsh_core::text_edit::pasted(text, input.config.multiline);
        if text.is_empty() || !input.config.editable {
            return;
        }
        let mut chars: Vec<char> = input.text.chars().collect();
        let caret = input.caret.min(chars.len());
        let count = text.chars().count();
        chars.splice(caret..caret, text.chars());
        input.text = chars.into_iter().collect();
        input.caret = caret + count;
        self.changed(id, out);
    }

    /// The columns an input's text has.
    fn input_columns(&self, id: NodeId) -> Option<usize> {
        let node = self.nodes.get(&id)?;
        let input = node.input.as_ref()?;
        if !input.config.multiline {
            return None;
        }
        let width = input.content.map_or(node.frame.size.width, |c| c.size.width);
        Some(((width.0 + 0.01) / CELL_W).floor().max(1.0) as usize)
    }

    /// A click on `target` (what the painter found at the place): an input
    /// takes the focus, else the nearest view that takes presses is
    /// pressed.
    pub fn click(&mut self, target: NodeId, out: &mut Vec<Out>) {
        let mut at = Some(target);
        while let Some(id) = at {
            let Some(node) = self.nodes.get(&id) else { return };
            if node.input.as_ref().is_some_and(|i| i.config.editable) {
                if let Some(input) = self.nodes.get_mut(&id).and_then(|n| n.input.as_mut()) {
                    input.caret = input.text.chars().count();
                }
                self.focus(Some(id), out);
                return;
            }
            if node.hears(EventKind::Press) {
                for kind in [EventKind::PressDown, EventKind::PressUp, EventKind::Press] {
                    self.send(id, kind, EventPayload::None, out);
                }
                return;
            }
            at = node.parent;
        }
    }

    /// The wheel over `target`: the nearest scroll view moves `rows`.
    pub fn wheel(&mut self, target: NodeId, rows: i32, out: &mut Vec<Out>) {
        let mut at = Some(target);
        while let Some(id) = at {
            let Some(node) = self.nodes.get(&id) else { return };
            if node.scroll.as_ref().is_some_and(|s| !s.horizontal && s.content.height > node.frame.size.height) {
                self.scroll_by(id, rows, out);
                return;
            }
            at = node.parent;
        }
    }

    /// Moves scroll view `id` by `rows` (down when positive).
    pub fn scroll_by(&mut self, id: NodeId, rows: i32, out: &mut Vec<Out>) {
        let Some(node) = self.nodes.get_mut(&id) else { return };
        let height = node.frame.size.height;
        let Some(scroll) = node.scroll.as_mut() else { return };
        let max = (scroll.content.height - height).max(Points::ZERO);
        let y = (scroll.offset.y + px(rows as f32 * CELL_H)).max(Points::ZERO).min(max);
        if y != scroll.offset.y {
            scroll.offset.y = y;
            out.push(Out::Scroll(id, scroll.offset, scroll.applied));
        }
    }

    /// The tallest vertical scroll view: what Page Up and Page Down move.
    pub fn main_scroll(&self) -> Option<NodeId> {
        self.nodes
            .iter()
            .filter(|(_, n)| n.scroll.as_ref().is_some_and(|s| !s.horizontal) && n.parent.is_some())
            .max_by(|a, b| a.1.frame.size.height.0.total_cmp(&b.1.frame.size.height.0))
            .map(|(id, _)| *id)
    }

    /// The tree as text, for tests and debugging: kinds, labels, texts.
    pub fn dump(&self) -> String {
        let mut out = String::new();
        self.dump_node(ROOT_ID, 0, &mut out);
        out
    }

    fn dump_node(&self, id: NodeId, depth: usize, out: &mut String) {
        let Some(node) = self.nodes.get(&id) else { return };
        let text = node.text.as_ref().map(|t| format!(" {:?}", t.text.text)).unwrap_or_default();
        let input = node.input.as_ref().map(|i| format!(" input {:?}", i.text)).unwrap_or_default();
        out.push_str(&format!("{}{:?} #{id}{text}{input}\n", "  ".repeat(depth), node.kind));
        for child in &node.children {
            self.dump_node(*child, depth + 1, out);
        }
    }
}

/// The row of `rows` the caret at `index` is on.
pub fn row_of(rows: &[std::ops::Range<usize>], index: usize) -> usize {
    rows.iter().position(|r| index >= r.start && index <= r.end).unwrap_or(rows.len().saturating_sub(1))
}

/// The character at `column` of `row` (or its end).
fn index_at(chars: &[char], row: &std::ops::Range<usize>, column: usize) -> usize {
    let mut width = 0;
    for i in row.clone() {
        let w = char_width(chars[i]);
        if width + w > column {
            return i;
        }
        width += w;
    }
    row.end
}

/// Whether `cell` is inside the box of cells `area` (left, top, right,
/// bottom; exclusive ends).
pub fn contains(area: (i32, i32, i32, i32), cell: Cell) -> bool {
    let (x, y) = (i32::from(cell.0), i32::from(cell.1));
    x >= area.0 && x < area.2 && y >= area.1 && y < area.3
}
