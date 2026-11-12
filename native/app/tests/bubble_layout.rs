//! A reply's bubble is as wide as what it says, and as tall: a list in it
//! counts toward its width like a paragraph does, so nothing a bot wrote
//! hangs out of its bubble or under the message after it.
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use bubble_core::ChatMessage;
use bubble_store::{MessageRole, Store};
use nsh_core::element::{px, size};
use nsh_core::{MountBatch, MountItem, NodeId};
use nsh_engine::{EngineConfig, Root};
use nsh_testing::Headless;
use serde_json::json;

const LONG_ITEM: &str = "A first bot whose line runs well past the closing question and a good way further";
const CLOSING: &str = "Restart it?";
const NEXT: &str = "Tell me about them";
const NOTE: &str = "A note whose text is a good deal longer than the word after it";
const AFTER_NOTE: &str = "Done.";

#[derive(Default)]
struct Geometry {
    parents: HashMap<NodeId, NodeId>,
    /// Each node's place in its parent, and its size.
    frames: HashMap<NodeId, (f32, f32, f32, f32)>,
    texts: HashMap<NodeId, String>,
}

impl Geometry {
    fn apply(&mut self, batch: &MountBatch) {
        for item in &batch.items {
            match item {
                MountItem::Insert { parent, child, .. } => {
                    self.parents.insert(*child, *parent);
                }
                MountItem::Remove { id } => {
                    self.parents.remove(id);
                }
                MountItem::Delete { id } => {
                    self.parents.remove(id);
                    self.frames.remove(id);
                    self.texts.remove(id);
                }
                MountItem::Frame { id, frame } => {
                    self.frames.insert(*id, (frame.origin.x.0, frame.origin.y.0, frame.size.width.0, frame.size.height.0));
                }
                MountItem::Text { id, text, .. } => {
                    self.texts.insert(*id, text.text.clone());
                }
                _ => {}
            }
        }
    }

    /// Where the text holding `snippet` is in the window: left, top,
    /// right, bottom.
    fn place(&self, snippet: &str) -> (f32, f32, f32, f32) {
        let (id, _) = self
            .texts
            .iter()
            .filter(|(id, text)| text.contains(snippet) && self.frames.contains_key(*id))
            .max_by(|a, b| self.origin(*a.0).0.total_cmp(&self.origin(*b.0).0))
            .unwrap_or_else(|| panic!("no frame for {snippet:?}"));
        let (x, y) = self.origin(*id);
        let (_, _, width, height) = self.frames[id];
        (x, y, x + width, y + height)
    }

    fn origin(&self, id: NodeId) -> (f32, f32) {
        let (mut at, mut x, mut y) = (id, 0.0, 0.0);
        for _ in 0..60 {
            if let Some(&(dx, dy, _, _)) = self.frames.get(&at) {
                x += dx;
                y += dy;
            }
            match self.parents.get(&at) {
                Some(parent) => at = *parent,
                None => break,
            }
        }
        (x, y)
    }
}

#[test]
fn what_a_reply_says_stays_inside_its_bubble() {
    let root = std::env::temp_dir().join(format!("bubble-layout-{}", std::process::id()));
    std::fs::create_dir_all(root.join("native")).unwrap();
    // A model service that has nothing: no turn runs here.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            while reader.read_line(&mut line).unwrap_or(0) > 0 && line != "\r\n" {
                line.clear();
            }
            stream.write_all(b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n").ok();
        }
    });
    std::fs::write(
        root.join("config.json"),
        json!({"providers":[{"id":"mock","name":"Mock","baseURL":format!("http://{address}"),"apiKey":"k","enabled":true}],"defaultModel":"mock:m"}).to_string(),
    )
    .unwrap();
    let reply = format!("There are two bots:\n\n**project-one (2)**\n\n- {LONG_ITEM}\n- Second, idle\n\n{CLOSING}");
    {
        let store = Store::open(root.join("native/bubble.db")).unwrap();
        let run = store.manager_run(None).unwrap();
        store.append_message(&run.id, MessageRole::User, "How many bots are there").unwrap();
        store
            .append_message_with(&run.id, MessageRole::Assistant, &reply, Some(&serde_json::to_string(&ChatMessage::assistant(reply.as_str())).unwrap()))
            .unwrap();
        store.append_message(&run.id, MessageRole::User, NEXT).unwrap();
        // A note's text sits in what its row has left: it asks for no width.
        let noted = format!("> [!NOTE]\n> {NOTE}\n\n{AFTER_NOTE}");
        store
            .append_message_with(&run.id, MessageRole::Assistant, &noted, Some(&serde_json::to_string(&ChatMessage::assistant(noted.as_str())).unwrap()))
            .unwrap();
    }
    // SAFETY: this is the only test in the binary, before engine threads.
    unsafe {
        std::env::set_var("BUBBLE_HOME", &root);
        std::env::set_var("BUBBLE_MEMORY_AUTO", "0");
        std::env::set_var("BUBBLE_NO_SHELL_ENV", "1");
        std::env::set_var("BUBBLE_GH", root.join("no-gh"));
    }
    let mut app = Headless::start(EngineConfig::new(Root::of::<bubble_app::App>())).unwrap();
    app.set_viewport(size(px(1280.0), px(1400.0))).unwrap();
    app.wait_for(AFTER_NOTE).unwrap();
    let geometry = Arc::new(Mutex::new(Geometry::default()));
    let capture = geometry.clone();
    app.observe(move |_, batch| capture.lock().unwrap().apply(batch));
    // Away and back, so the conversation is laid out under the observer.
    app.press("id:nav-settings").unwrap();
    app.press("id:nav-home").unwrap();
    app.wait_for(AFTER_NOTE).unwrap();
    {
        let frames = geometry.lock().unwrap();
        let (item, closing, next) = (frames.place(LONG_ITEM), frames.place(CLOSING), frames.place(NEXT));
        // The long item is on one line: the bubble made room for it.
        assert!(item.3 - item.1 < 30.0, "the list item wrapped in a bubble with room to spare: {item:?}");
        // What follows the reply starts under all of it.
        assert!(closing.3 <= next.1, "the reply's last line {closing:?} reaches under the next message {next:?}");
        assert!(item.3 <= closing.1, "the list {item:?} runs into the line after it {closing:?}");
        let (note, after) = (frames.place(NOTE), frames.place(AFTER_NOTE));
        assert!(note.3 - note.1 < 30.0 && note.2 - note.0 > 200.0, "the note was squeezed: {note:?}");
        assert!(note.3 <= after.1, "the note {note:?} runs into the line after it {after:?}");
    }
    drop(app);
    std::fs::remove_dir_all(root).unwrap();
}
