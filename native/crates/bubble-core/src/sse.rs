//! A server-sent events reader: bytes in, `(event, data)` pairs out.

/// One event: its `event:` name (empty when the stream sends none) and its
/// `data:` lines joined with newlines.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SseEvent {
    pub event: String,
    pub data: String,
}

/// Collects bytes as they arrive and yields complete events.
#[derive(Default)]
pub struct SseParser {
    buffer: Vec<u8>,
    event: String,
    data: Vec<String>,
}

impl SseParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a chunk; returns the events it completed.
    pub fn push(&mut self, chunk: &[u8]) -> Vec<SseEvent> {
        self.buffer.extend_from_slice(chunk);
        let mut events = Vec::new();
        while let Some(end) = self.buffer.iter().position(|&b| b == b'\n') {
            let mut line: Vec<u8> = self.buffer.drain(..=end).collect();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            let line = String::from_utf8_lossy(&line);
            if let Some(event) = self.line(&line) {
                events.push(event);
            }
        }
        events
    }

    /// Ends the stream: an event left without its blank line still counts.
    pub fn finish(&mut self) -> Option<SseEvent> {
        if !self.buffer.is_empty() {
            let rest = String::from_utf8_lossy(&std::mem::take(&mut self.buffer)).into_owned();
            if let Some(event) = self.line(&rest) {
                return Some(event);
            }
        }
        self.dispatch()
    }

    fn line(&mut self, line: &str) -> Option<SseEvent> {
        if line.is_empty() {
            return self.dispatch();
        }
        if line.starts_with(':') {
            return None;
        }
        let (field, value) = match line.split_once(':') {
            Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
            None => (line, ""),
        };
        match field {
            "event" => self.event = value.to_owned(),
            "data" => self.data.push(value.to_owned()),
            _ => {}
        }
        None
    }

    fn dispatch(&mut self) -> Option<SseEvent> {
        if self.data.is_empty() {
            self.event.clear();
            return None;
        }
        Some(SseEvent {
            event: std::mem::take(&mut self.event),
            data: std::mem::take(&mut self.data).join("\n"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_split_across_chunks_come_out_whole() {
        let mut parser = SseParser::new();
        assert!(parser.push(b"event: message_start\nda").is_empty());
        let events = parser.push(b"ta: {\"a\":1}\r\n\r\ndata: two\n");
        assert_eq!(events, [SseEvent { event: "message_start".into(), data: "{\"a\":1}".into() }]);
        assert_eq!(parser.finish(), Some(SseEvent { event: String::new(), data: "two".into() }));
    }

    #[test]
    fn comments_and_blank_events_are_skipped() {
        let mut parser = SseParser::new();
        let events = parser.push(b": keep-alive\n\nevent: ping\n\ndata: a\ndata: b\n\n");
        assert_eq!(events, [SseEvent { event: String::new(), data: "a\nb".into() }]);
    }
}
