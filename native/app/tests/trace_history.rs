//! History from before turns had headers (replies, and tool calls that
//! no longer show), followed by a new turn. Check actual layout frames as
//! well as tree order: a correct projection alone is not enough.
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bubble_core::{ChatMessage, ToolCall, ToolResult};
use bubble_store::{MessageRole, NewTask, Origin, Store};
use nsh_core::element::{px, size};
use nsh_core::{MountBatch, MountItem, NodeId};
use nsh_engine::{EngineConfig, Root};
use nsh_testing::Headless;
use serde_json::json;

#[derive(Default)]
struct Geometry {
    parents: HashMap<NodeId, NodeId>,
    frames: HashMap<NodeId, (f32, f32)>,
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
                    self.frames.insert(*id, (frame.origin.x.0, frame.origin.y.0));
                }
                MountItem::Text { id, text, .. } => {
                    self.texts.insert(*id, text.text.clone());
                }
                _ => {}
            }
        }
    }
    fn position(&self, snippet: &str) -> (f32, f32) {
        self.texts
            .iter()
            .filter(|(_, text)| text.contains(snippet))
            .filter_map(|(id, _)| {
                let mut at = *id;
                let (mut x, mut y) = (0.0, 0.0);
                for _ in 0..50 {
                    if let Some(&(dx, dy)) = self.frames.get(&at) {
                        x += dx;
                        y += dy;
                    }
                    match self.parents.get(&at) {
                        Some(parent) => at = *parent,
                        None => break,
                    }
                }
                Some((x, y))
            })
            .max_by(|a, b| a.0.total_cmp(&b.0))
            .unwrap_or_else(|| panic!("no main-transcript frame for {snippet:?}"))
    }
}

fn seed(store: &Store) {
    let task = store
        .create_task(NewTask {
            title: "Original task card",
            project_cwd: None,
            origin: Origin::Manager,
        })
        .unwrap();
    let run = store.manager_run(None).unwrap();
    store
        .append_message(&run.id, MessageRole::User, "Original request")
        .unwrap();
    for (id, name, output) in [
        ("list", "list_tasks", "Existing tasks".to_owned()),
        (
            "create",
            "create_task",
            format!(
                "Created task {} \"Original task card\". Its agent is working on it.",
                task.id
            ),
        ),
    ] {
        let message = ChatMessage::Assistant {
            text: String::new(),
            reasoning: "**Planning the dispatch**".into(),
            tool_calls: vec![ToolCall {
                id: id.into(),
                name: name.into(),
                arguments: "{}".into(),
            }],
            state: serde_json::Value::Null,
        };
        store
            .append_message_with(
                &run.id,
                MessageRole::Assistant,
                "",
                Some(&serde_json::to_string(&message).unwrap()),
            )
            .unwrap();
        let result = ChatMessage::Tool {
            results: vec![ToolResult {
                call_id: id.into(),
                content: output,
                is_error: false,
            }],
        };
        store
            .append_message_with(
                &run.id,
                MessageRole::Tool,
                "",
                Some(&serde_json::to_string(&result).unwrap()),
            )
            .unwrap();
    }
    store
        .append_message_with(
            &run.id,
            MessageRole::Assistant,
            "Original final answer",
            Some(&serde_json::to_string(&ChatMessage::assistant("Original final answer")).unwrap()),
        )
        .unwrap();
}

#[test]
fn earlier_answers_stay_above_the_next_prompt_in_layout() {
    let root = std::env::temp_dir().join(format!("bubble-trace-history-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (release, receive) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        // The agent also asks which models the service has; the request
        // for a reply is the one answered here.
        let mut stream = loop {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut length = 0;
            let mut first = String::new();
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
                if first.is_empty() {
                    first = line.clone();
                }
                if let Some(n) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = n.trim().parse().unwrap();
                }
            }
            reader.read_exact(&mut vec![0; length]).unwrap();
            if first.starts_with("POST ") {
                break stream;
            }
            stream.write_all(b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n").ok();
        };
        let reasoning = format!(
            "data: {}\n\n",
            json!({"choices":[{"delta":{"reasoning_content":"**Checking current state**"}}]})
        );
        let answer = format!(
            "data: {}\n\ndata: [DONE]\n\n",
            json!({"choices":[{"delta":{"content":"New final answer"}}]})
        );
        write!(stream,"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{reasoning}",reasoning.len()+answer.len()).unwrap();
        stream.flush().unwrap();
        if receive.recv_timeout(Duration::from_secs(25)).is_ok() {
            stream.write_all(answer.as_bytes()).unwrap();
        }
    });
    std::fs::write(root.join("config.json"),json!({"providers":[{"id":"mock","name":"Mock","baseURL":format!("http://{address}"),"apiKey":"k","enabled":true}],"defaultModel":"mock:m"}).to_string()).unwrap();
    seed(&Store::open(root.join("native/bubble.db")).unwrap());
    // SAFETY: this is the only test in the binary, before engine threads.
    unsafe {
        std::env::set_var("BUBBLE_HOME", &root);
        std::env::set_var("BUBBLE_MEMORY_AUTO", "0");
        std::env::set_var("BUBBLE_NO_SHELL_ENV", "1");
        std::env::set_var("BUBBLE_GH", root.join("no-gh"));
    }
    let mut app = Headless::start(EngineConfig::new(Root::of::<bubble_app::App>())).unwrap();
    app.set_viewport(size(px(1488.0), px(1500.0))).unwrap();
    app.wait_for("Original final answer").unwrap();
    let geometry = Arc::new(Mutex::new(Geometry::default()));
    let capture = geometry.clone();
    app.observe(move |_, batch| capture.lock().unwrap().apply(batch));
    // Away and back, so the conversation is laid out again under the observer.
    app.press("id:nav-settings").unwrap();
    app.press("id:nav-home").unwrap();
    app.wait_for("Original final answer").unwrap();
    app.type_text("id:composer", "New follow-up request").unwrap();
    app.submit("id:composer").unwrap();
    // The new turn is at work: one row at the end says so.
    app.wait_for_within("id:activity", Duration::from_secs(10)).unwrap();
    let assert_order = |app: &Headless| {
        let texts = app.screen().texts();
        let snippets = ["Original request", "Original final answer", "New follow-up request"];
        let indices = snippets.map(|s| texts.iter().rposition(|t| t.contains(s)).unwrap());
        assert!(indices.windows(2).all(|w| w[0] < w[1]), "tree order: {texts:?}");
        let frames = geometry.lock().unwrap();
        let positions = snippets.map(|s| frames.position(s));
        assert!(
            positions.windows(2).all(|w| w[0].1 < w[1].1),
            "layout order: {positions:?}"
        );
    };
    assert_order(&app);
    release.send(()).unwrap();
    app.wait_for("New final answer").unwrap();
    assert_order(&app);
    drop(app);
    std::fs::remove_dir_all(root).unwrap();
}
