//! A terminal platform for NSH apps.
//!
//! The engine lays the app's views out as on any platform, in points; here
//! a column is [`CELL_W`] points and a row [`CELL_H`]. Its batches build a
//! model of the native views ([`model::Tree`]), which is painted into a
//! grid of cells ([`paint::paint`]) and written to the terminal as the
//! cells that changed. Keys go to the app's shortcuts (`on_key`) first,
//! then to the focused input, which this platform edits as a native text
//! field would; clicks press, the wheel and Page Up / Page Down scroll.
//!
//! [`run`] takes over the terminal until the app calls [`exit`];
//! [`Virtual`] runs an app on a terminal of its own for tests.

pub mod model;
pub mod paint;
mod terminal;
pub mod text;

use std::io;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use nsh_core::geometry::{Points, Size, px, size};
use nsh_core::keymap::{Key, Keystroke, Modifiers};
use nsh_core::platform::{ColorScheme, Idiom, Os};
use nsh_core::{EventKind, MountBatch};
use nsh_engine::{AppError, Engine, EngineConfig, EngineControl, MountSink};

pub use terminal::{Options, run};
pub use text::{CELL_H, CELL_W, CellTextEngine};

use model::{Out, Tree};
use paint::{Painted, paint};

/// What reaches the loop that owns the screen.
enum Message {
    Batch(MountBatch),
    Error(String),
    Input(crossterm::event::Event),
    Exit,
}

/// Where [`exit`] sends: the running terminal's loop.
static EXIT: Mutex<Option<Sender<Message>>> = Mutex::new(None);

/// Ends [`run`]: the terminal is given back and `run` returns.
pub fn exit() {
    if let Ok(exit) = EXIT.lock()
        && let Some(sender) = exit.as_ref()
    {
        let _ = sender.send(Message::Exit);
    }
}

struct Sink {
    sender: Sender<Message>,
    control: Arc<OnceLock<EngineControl>>,
}

impl MountSink for Sink {
    fn submit(&self, batch: MountBatch) {
        let _ = self.sender.send(Message::Batch(batch));
    }

    fn report(&self, error: &AppError) {
        log::error!("{error}");
        let _ = self.sender.send(Message::Error(error.to_string()));
    }

    fn request_frame(&self) {
        let control = self.control.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(16));
            if let Some(control) = control.get() {
                control.frame();
            }
        });
    }
}

/// The size in points of `columns` × `rows` cells.
pub fn points(columns: u16, rows: u16) -> Size<Points> {
    size(px(f32::from(columns) * CELL_W), px(f32::from(rows) * CELL_H))
}

/// `config` for a terminal of `columns` × `rows`: a desktop whose `mod` key
/// is Control, with reduced motion (nothing here animates).
pub fn configure(mut config: EngineConfig, columns: u16, rows: u16) -> EngineConfig {
    let viewport = points(columns, rows);
    config.viewport = viewport;
    let env = &mut config.environment;
    env.window = viewport;
    env.os = if cfg!(target_os = "macos") { Os::MacOs } else { Os::Linux };
    env.idiom = Idiom::Desktop;
    env.apple_keyboard = false;
    env.reduce_motion = true;
    env.pixel_ratio = 1.0;
    env.color_scheme = ColorScheme::Dark;
    config
}

/// A started app: its engine, the environment it was given (resizes send
/// it again with the new size), and where its batches come.
struct Started {
    engine: Engine,
    environment: nsh_engine::Environment,
    sender: Sender<Message>,
    receiver: Receiver<Message>,
}

fn start(config: EngineConfig, columns: u16, rows: u16) -> io::Result<Started> {
    let (sender, receiver) = channel();
    let control = Arc::new(OnceLock::new());
    let sink = Sink { sender: sender.clone(), control: control.clone() };
    let config = configure(config, columns, rows);
    let environment = config.environment;
    let engine = Engine::start(config, || CellTextEngine, sink)?;
    let _ = control.set(engine.control());
    Ok(Started { engine, environment, sender, receiver })
}

/// Tells the engine the terminal is now `columns` × `rows`.
fn resize(control: &EngineControl, environment: &mut nsh_engine::Environment, columns: u16, rows: u16) {
    let size = points(columns, rows);
    environment.window = size;
    control.set_environment(*environment);
    control.resize(size);
}

/// What the model asked for: events go to the engine; the clipboard and an
/// exit to the caller.
fn deliver(control: &EngineControl, outs: Vec<Out>, clipboard: &mut Option<String>) -> bool {
    let mut exit = false;
    for out in outs {
        match out {
            Out::Event(id, kind, payload) => control.dispatch_event(id, kind, payload),
            Out::Scroll(id, offset, applied) => control.scroll(id, offset, applied),
            Out::States(id, states) => control.set_states(id, states),
            Out::Clipboard(text) => *clipboard = Some(text),
            Out::Exit => exit = true,
        }
    }
    exit
}

/// A keystroke as `nsh/keymap` writes it (`"ctrl+c"`, `"enter"`, `"a"`).
pub fn keystroke(keys: &str) -> Keystroke {
    Keystroke::parse(keys, Os::Linux).unwrap_or(Keystroke { modifiers: Modifiers::default(), key: Key::Char(' ') })
}

/// An app on a terminal of its own: tests type, click and read the screen.
pub struct Virtual {
    engine: Engine,
    environment: nsh_engine::Environment,
    receiver: Receiver<Message>,
    pub tree: Tree,
    width: u16,
    height: u16,
    pub errors: Vec<String>,
    pub clipboard: Option<String>,
    pub exited: bool,
    _sender: Sender<Message>,
}

impl Virtual {
    pub fn start(config: EngineConfig, width: u16, height: u16) -> io::Result<Virtual> {
        let Started { engine, environment, sender, receiver } = start(config, width, height)?;
        if let Ok(mut exit) = EXIT.lock() {
            *exit = Some(sender.clone());
        }
        let mut terminal = Virtual {
            engine,
            environment,
            receiver,
            tree: Tree::new(),
            width,
            height,
            errors: Vec::new(),
            clipboard: None,
            exited: false,
            _sender: sender,
        };
        terminal.settle();
        Ok(terminal)
    }

    fn receive(&mut self, message: Message) {
        match message {
            Message::Batch(batch) => {
                let mut outs = Vec::new();
                self.tree.apply(&batch, &mut outs);
                self.send(outs);
            }
            Message::Error(error) => self.errors.push(error),
            Message::Exit => self.exited = true,
            Message::Input(_) => {}
        }
    }

    fn send(&mut self, outs: Vec<Out>) {
        if deliver(&self.engine.control(), outs, &mut self.clipboard) {
            self.exited = true;
        }
    }

    /// Applies batches until none comes for 100 ms (5 s at most).
    pub fn settle(&mut self) {
        let end = Instant::now() + Duration::from_secs(5);
        while Instant::now() < end {
            match self.receiver.recv_timeout(Duration::from_millis(100)) {
                Ok(message) => self.receive(message),
                Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => break,
            }
        }
    }

    pub fn screen(&self) -> Painted {
        paint(&self.tree, self.width, self.height)
    }

    pub fn text(&self) -> String {
        self.screen().grid.text()
    }

    /// Waits until the screen shows `text`.
    pub fn wait_for(&mut self, text: &str, timeout: Duration) -> Result<(), String> {
        let end = Instant::now() + timeout;
        loop {
            if self.text().contains(text) {
                return Ok(());
            }
            if Instant::now() >= end {
                return Err(format!("the screen never showed {text:?}; it shows:\n{}\nerrors: {:?}", self.text(), self.errors));
            }
            if let Ok(message) = self.receiver.recv_timeout(Duration::from_millis(20)) {
                self.receive(message);
            }
        }
    }

    /// Presses `keys` (`"ctrl+c"`, `"enter"`, `"up"`).
    pub fn key(&mut self, keys: &str) {
        let mut outs = Vec::new();
        self.tree.key(keystroke(keys), &mut outs);
        self.send(outs);
        self.settle();
    }

    /// Types `text`, a key per character.
    pub fn type_text(&mut self, text: &str) {
        for c in text.chars() {
            let mut outs = Vec::new();
            self.tree.key(Keystroke { modifiers: Modifiers::default(), key: Key::Char(c) }, &mut outs);
            self.send(outs);
        }
        self.settle();
    }

    pub fn paste(&mut self, text: &str) {
        let mut outs = Vec::new();
        self.tree.paste(text, &mut outs);
        self.send(outs);
        self.settle();
    }

    /// Clicks the first place the screen shows `text`.
    pub fn click(&mut self, text: &str) -> Result<(), String> {
        let screen = self.screen();
        let lines = screen.grid.lines();
        let (y, x) = lines
            .iter()
            .enumerate()
            .find_map(|(y, line)| line.find(text).map(|byte| (y, text::text_width(&line[..byte]))))
            .ok_or_else(|| format!("the screen does not show {text:?}:\n{}", lines.join("\n")))?;
        let cell = (x as u16, y as u16);
        let target = screen.hit(cell).ok_or_else(|| format!("nothing at {text:?} takes a click"))?;
        let mut outs = Vec::new();
        self.tree.click(target, &mut outs);
        self.send(outs);
        self.settle();
        Ok(())
    }

    /// Whether the focused input hears submits (for tests).
    pub fn focused_submits(&self) -> bool {
        self.tree.focused.and_then(|id| self.tree.node(id)).is_some_and(|n| n.hears(EventKind::SubmitEditing))
    }

    pub fn resize(&mut self, width: u16, height: u16) {
        self.width = width;
        self.height = height;
        resize(&self.engine.control(), &mut self.environment, width, height);
        self.settle();
    }
}
