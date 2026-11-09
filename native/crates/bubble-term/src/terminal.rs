//! The real terminal: raw mode on the alternate screen, the app's frames
//! written as the cells that changed, keys, pastes, clicks and the wheel
//! read on a thread of their own.

use std::fmt::Write as _;
use std::io::{self, Write};
use std::sync::mpsc::Sender;

use crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, KeyboardEnhancementFlags, MouseButton, MouseEventKind,
};
use crossterm::{execute, terminal};
use nsh_core::EventKind;
use nsh_core::platform::ColorScheme;
use nsh_core::keymap::{Key, Keystroke, Modifiers};
use nsh_engine::EngineConfig;

use crate::model::{Out, Tree};
use crate::paint::{BOLD, Cell, DIM, Grid, ITALIC, REVERSE, UNDERLINE, paint};
use crate::{EXIT, Message, Started, deliver, resize, start};

/// How the terminal is used.
#[derive(Clone, Debug)]
pub struct Options {
    /// Clicks and the wheel go to the app (most terminals still select
    /// text with Shift or Option held).
    pub mouse: bool,
    /// The background the app is told it has; `None`: asked of the
    /// terminal ([`background_scheme`]).
    pub scheme: Option<ColorScheme>,
}

impl Default for Options {
    fn default() -> Self {
        Options { mouse: true, scheme: None }
    }
}

/// Whether the terminal's background is light or dark: asked of the
/// terminal (OSC 11), else `COLORFGBG`, else dark — as the TypeScript TUI
/// decides. Call it with the terminal in raw mode.
pub fn background_scheme() -> ColorScheme {
    query_background(std::time::Duration::from_millis(300))
        .or_else(|| colorfgbg(&std::env::var("COLORFGBG").unwrap_or_default()))
        .unwrap_or(ColorScheme::Dark)
}

fn colorfgbg(value: &str) -> Option<ColorScheme> {
    let background: u8 = value.rsplit(';').next()?.trim().parse().ok()?;
    match background {
        0..=6 => Some(ColorScheme::Dark),
        7..=15 => Some(ColorScheme::Light),
        _ => None,
    }
}

/// The scheme of an OSC 11 answer (`rgb:RRRR/GGGG/BBBB`).
fn parse_background(answer: &str) -> Option<ColorScheme> {
    let rgb = answer.split("rgb:").nth(1)?;
    let channels: Vec<f64> = rgb
        .split(['/', '\x07', '\x1b'])
        .take(3)
        .map(|c| {
            let hex: String = c.chars().take_while(char::is_ascii_hexdigit).collect();
            let max = 16f64.powi(hex.len() as i32) - 1.0;
            u32::from_str_radix(&hex, 16).ok().map(|v| f64::from(v) / max)
        })
        .collect::<Option<Vec<_>>>()?;
    let [r, g, b] = channels[..] else { return None };
    let luminance = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    Some(if luminance > 0.5 { ColorScheme::Light } else { ColorScheme::Dark })
}

/// Asks the terminal for its background; a device-attributes question
/// after it ends the wait at once on terminals that do not answer OSC 11.
#[cfg(unix)]
fn query_background(timeout: std::time::Duration) -> Option<ColorScheme> {
    use std::io::Read;
    let mut out = io::stdout();
    out.write_all(b"\x1b]11;?\x1b\\\x1b[c").ok()?;
    out.flush().ok()?;
    let deadline = std::time::Instant::now() + timeout;
    let mut answer = Vec::new();
    let mut stdin = io::stdin();
    loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            break;
        }
        let mut poll = libc::pollfd { fd: 0, events: libc::POLLIN, revents: 0 };
        // SAFETY: one valid pollfd for the call.
        let ready = unsafe { libc::poll(&mut poll, 1, left.as_millis() as libc::c_int) };
        if ready <= 0 {
            break;
        }
        let mut buffer = [0u8; 256];
        let Ok(n) = stdin.read(&mut buffer) else { break };
        if n == 0 {
            break;
        }
        answer.extend_from_slice(&buffer[..n]);
        // The device-attributes answer (`ESC [ ? ... c`) comes last.
        let text = String::from_utf8_lossy(&answer);
        if text.contains("\x1b[?") && text.trim_end().ends_with('c') {
            break;
        }
    }
    parse_background(&String::from_utf8_lossy(&answer))
}

#[cfg(not(unix))]
fn query_background(_: std::time::Duration) -> Option<ColorScheme> {
    None
}

/// Puts the terminal back however `run` ends.
struct Restore {
    mouse: bool,
    enhanced: bool,
}

impl Drop for Restore {
    fn drop(&mut self) {
        restore(self.mouse, self.enhanced);
    }
}

fn restore(mouse: bool, enhanced: bool) {
    let mut out = io::stdout();
    if enhanced {
        let _ = execute!(out, event::PopKeyboardEnhancementFlags);
    }
    if mouse {
        let _ = execute!(out, event::DisableMouseCapture);
    }
    let _ = execute!(out, event::DisableBracketedPaste, crossterm::cursor::Show, terminal::LeaveAlternateScreen);
    let _ = terminal::disable_raw_mode();
}

/// Runs `config`'s app on this terminal until it calls [`crate::exit`]
/// (or the person presses Ctrl+C where the app does not take it).
pub fn run(config: EngineConfig, options: Options) -> io::Result<()> {
    let (mut width, mut height) = terminal::size()?;
    terminal::enable_raw_mode()?;
    let scheme = options.scheme.unwrap_or_else(background_scheme);
    let mut out = io::stdout();
    execute!(out, terminal::EnterAlternateScreen, event::EnableBracketedPaste, crossterm::cursor::Hide)?;
    if options.mouse {
        execute!(out, event::EnableMouseCapture)?;
    }
    // Shift+Enter told from Enter, where the terminal can.
    let enhanced = terminal::supports_keyboard_enhancement().unwrap_or(false)
        && execute!(out, event::PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)).is_ok();
    let _restore = Restore { mouse: options.mouse, enhanced };
    let previous = std::panic::take_hook();
    let mouse = options.mouse;
    std::panic::set_hook(Box::new(move |info| {
        restore(mouse, enhanced);
        previous(info);
    }));

    let Started { engine, mut environment, sender, receiver } = start(config, width, height, scheme)?;
    if let Ok(mut exit) = EXIT.lock() {
        *exit = Some(sender.clone());
    }
    read_input(sender);
    let control = engine.control();
    let mut tree = Tree::new();
    let mut shown: Option<Grid> = None;
    let mut clipboard = None;
    let mut cursor = None;
    let mut painted = paint(&tree, width, height);
    'run: while let Ok(first) = receiver.recv() {
        let mut next = Some(first);
        let mut dirty = false;
        while let Some(message) = next.take() {
            let mut outs = Vec::new();
            match message {
                Message::Exit => break 'run,
                Message::Batch(batch) => {
                    trace(|| format!("batch of {} items", batch.items.len()));
                    tree.apply(&batch, &mut outs);
                    dirty = true;
                }
                Message::Error(_) => {}
                Message::Input(event) => match event {
                    Event::Key(key) if key.kind != KeyEventKind::Release => {
                        let stroke = keystroke(&key);
                        let mut keys = Vec::new();
                        tree.key(stroke, &mut keys);
                        trace(|| format!("key {stroke:?} focused {:?} -> {keys:?}", tree.focused));
                        let fired = keys.iter().any(|o| matches!(o, Out::Event(_, EventKind::KeyCommand, _)));
                        if !fired && stroke.modifiers.ctrl && stroke.key == Key::Char('c') {
                            break 'run;
                        }
                        if keys.is_empty()
                            && let Some(rows) = match stroke.key {
                                Key::PageUp => Some(-(i32::from(height) - 2)),
                                Key::PageDown => Some(i32::from(height) - 2),
                                _ => None,
                            }
                            && let Some(main) = tree.main_scroll()
                        {
                            tree.scroll_by(main, rows, &mut keys);
                        }
                        outs.extend(keys);
                        dirty = true;
                    }
                    Event::Paste(text) => {
                        tree.paste(&text, &mut outs);
                        dirty = true;
                    }
                    Event::Mouse(mouse) => {
                        let cell = (mouse.column, mouse.row);
                        if let Some(target) = painted.hit(cell) {
                            match mouse.kind {
                                MouseEventKind::Down(MouseButton::Left) => tree.click(target, &mut outs),
                                MouseEventKind::ScrollUp => tree.wheel(target, -3, &mut outs),
                                MouseEventKind::ScrollDown => tree.wheel(target, 3, &mut outs),
                                _ => {}
                            }
                            dirty = true;
                        }
                    }
                    Event::Resize(w, h) => {
                        (width, height) = (w, h);
                        resize(&control, &mut environment, w, h);
                        shown = None;
                        dirty = true;
                    }
                    _ => {}
                },
            }
            if deliver(&control, outs, &mut clipboard) {
                break 'run;
            }
            next = receiver.try_recv().ok();
        }
        if let Some(text) = clipboard.take() {
            // OSC 52: the terminal puts it on the system clipboard.
            let _ = write!(out, "\x1b]52;c;{}\x07", base64(text.as_bytes()));
        }
        if dirty {
            painted = paint(&tree, width, height);
            draw(&mut out, shown.as_ref(), &painted.grid, cursor != painted.cursor || shown.is_none(), painted.cursor)?;
            cursor = painted.cursor;
            shown = Some(painted.grid.clone());
        }
    }
    if let Ok(mut exit) = EXIT.lock() {
        *exit = None;
    }
    drop(engine);
    Ok(())
}

/// Appends `line` to the file `BUBBLE_TERM_TRACE` names, if it names one.
fn trace(line: impl FnOnce() -> String) {
    if let Some(path) = std::env::var_os("BUBBLE_TERM_TRACE") {
        if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(file, "{}", line());
        }
    }
}

fn read_input(sender: Sender<Message>) {
    std::thread::spawn(move || {
        loop {
            match event::read() {
                Ok(event) => {
                    trace(|| format!("input {event:?}"));
                    if sender.send(Message::Input(event)).is_err() {
                        break;
                    }
                }
                Err(error) => {
                    trace(|| format!("input error {error}"));
                    break;
                }
            }
        }
    });
}

fn keystroke(key: &KeyEvent) -> Keystroke {
    let m = key.modifiers;
    let mut modifiers = Modifiers {
        ctrl: m.contains(KeyModifiers::CONTROL),
        alt: m.contains(KeyModifiers::ALT),
        shift: m.contains(KeyModifiers::SHIFT),
        cmd: m.contains(KeyModifiers::SUPER),
    };
    let key = match key.code {
        KeyCode::Char(c) => {
            // The character already has its Shift; Control sends the
            // letter itself.
            if !modifiers.ctrl && !modifiers.alt {
                modifiers.shift = false;
            }
            Key::Char(if modifiers.ctrl { c.to_ascii_lowercase() } else { c })
        }
        KeyCode::Enter => Key::Enter,
        KeyCode::Esc => Key::Escape,
        KeyCode::Tab => Key::Tab,
        KeyCode::BackTab => {
            modifiers.shift = true;
            Key::Tab
        }
        KeyCode::Backspace => Key::Backspace,
        KeyCode::Delete => Key::Delete,
        KeyCode::Insert => Key::Insert,
        KeyCode::Up => Key::Up,
        KeyCode::Down => Key::Down,
        KeyCode::Left => Key::Left,
        KeyCode::Right => Key::Right,
        KeyCode::Home => Key::Home,
        KeyCode::End => Key::End,
        KeyCode::PageUp => Key::PageUp,
        KeyCode::PageDown => Key::PageDown,
        KeyCode::F(n) => Key::F(n),
        _ => Key::F(0),
    };
    Keystroke { modifiers, key }
}

fn sgr(cell: &Cell, out: &mut String) {
    out.push_str("\x1b[0");
    for (bit, code) in [(BOLD, ";1"), (DIM, ";2"), (ITALIC, ";3"), (UNDERLINE, ";4"), (REVERSE, ";7")] {
        if cell.attrs & bit != 0 {
            out.push_str(code);
        }
    }
    if let Some(fg) = cell.fg {
        let _ = write!(out, ";38;2;{};{};{}", fg >> 16, (fg >> 8) & 0xff, fg & 0xff);
    }
    if let Some(bg) = cell.bg {
        let _ = write!(out, ";48;2;{};{};{}", bg >> 16, (bg >> 8) & 0xff, bg & 0xff);
    }
    out.push('m');
}

/// Writes the cells of `grid` that differ from `shown` (all of them
/// without one), in one synchronized update.
fn draw(out: &mut impl Write, shown: Option<&Grid>, grid: &Grid, moved: bool, cursor: Option<(u16, u16)>) -> io::Result<()> {
    let mut text = String::from("\x1b[?2026h");
    if shown.is_none() {
        text.push_str("\x1b[0m\x1b[2J");
    }
    let mut style: Option<(Option<u32>, Option<u32>, u8)> = None;
    for y in 0..grid.height {
        let mut x = 0;
        while x < grid.width {
            let Some(cell) = grid.get(x, y) else { break };
            let same = shown.and_then(|s| s.get(x, y)) == Some(cell);
            if same || cell.tail {
                x += 1;
                continue;
            }
            let _ = write!(text, "\x1b[{};{}H", y + 1, x + 1);
            // The run of changed cells from here.
            while x < grid.width {
                let Some(cell) = grid.get(x, y) else { break };
                if shown.and_then(|s| s.get(x, y)) == Some(cell) && !cell.tail {
                    break;
                }
                if !cell.tail {
                    let key = (cell.fg, cell.bg, cell.attrs);
                    if style != Some(key) {
                        sgr(cell, &mut text);
                        style = Some(key);
                    }
                    text.push(if cell.ch == '\0' { ' ' } else { cell.ch });
                }
                x += 1;
            }
        }
    }
    text.push_str("\x1b[0m");
    match cursor {
        Some((x, y)) => {
            let _ = write!(text, "\x1b[{};{}H\x1b[?25h", y + 1, x + 1);
        }
        None if moved => text.push_str("\x1b[?25l"),
        None => {}
    }
    text.push_str("\x1b[?2026l");
    out.write_all(text.as_bytes())?;
    out.flush()
}

fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16) | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8) | u32::from(*chunk.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(TABLE[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn backgrounds_are_read_from_answers() {
        use super::ColorScheme;
        assert_eq!(super::parse_background("\x1b]11;rgb:ffff/ffff/ffff\x1b\\\x1b[?62c"), Some(ColorScheme::Light));
        assert_eq!(super::parse_background("\x1b]11;rgb:1e1e/1e1e/2020\x07"), Some(ColorScheme::Dark));
        assert_eq!(super::parse_background("\x1b[?62c"), None);
        assert_eq!(super::colorfgbg("15;0"), Some(ColorScheme::Dark));
        assert_eq!(super::colorfgbg("0;15"), Some(ColorScheme::Light));
        assert_eq!(super::colorfgbg(""), None);
    }

    #[test]
    fn base64_matches_the_standard() {
        assert_eq!(super::base64(b"hi"), "aGk=");
        assert_eq!(super::base64(b"hello!"), "aGVsbG8h");
    }
}
