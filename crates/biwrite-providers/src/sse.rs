//! Incremental Server-Sent Events parser (WHATWG event-stream format).
//!
//! Bytes are buffered until a full line is available, so multi-byte UTF-8
//! characters split across network chunks are handled correctly.

/// One dispatched event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SseEvent {
    /// The `event:` field, if any.
    pub event: Option<String>,
    /// All `data:` lines joined with `\n`.
    pub data: String,
}

#[derive(Debug, Default)]
pub struct SseParser {
    buf: Vec<u8>,
    event: Option<String>,
    data: Vec<String>,
}

impl SseParser {
    /// Feed received bytes; returns the events completed by them.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<SseEvent> {
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        while let Some(nl) = self.buf.iter().position(|b| *b == b'\n') {
            let mut line: Vec<u8> = self.buf.drain(..=nl).collect();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            let line = String::from_utf8_lossy(&line);
            if let Some(event) = self.line(&line) {
                out.push(event);
            }
        }
        out
    }

    /// End of stream: an event not terminated by a blank line is incomplete
    /// and discarded (as the SSE specification requires), so a connection
    /// cut mid-event is reported as an early end, not as bad data.
    pub fn finish(&mut self) {
        self.buf.clear();
        self.event = None;
        self.data.clear();
    }

    fn line(&mut self, line: &str) -> Option<SseEvent> {
        if line.is_empty() {
            return self.dispatch();
        }
        if line.starts_with(':') {
            return None; // comment / keep-alive
        }
        let (field, value) = match line.split_once(':') {
            Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
            None => (line, ""),
        };
        match field {
            "event" => self.event = Some(value.to_owned()),
            "data" => self.data.push(value.to_owned()),
            _ => {} // id, retry: unused
        }
        None
    }

    fn dispatch(&mut self) -> Option<SseEvent> {
        if self.data.is_empty() {
            self.event = None;
            return None;
        }
        Some(SseEvent {
            event: self.event.take(),
            data: std::mem::take(&mut self.data).join("\n"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_events_across_chunk_boundaries() {
        let stream = "event: message_start\ndata: {\"a\":1}\n\n: ping\n\ndata: 第一\ndata: 二\n\ndata: [DONE]\n\n";
        let bytes = stream.as_bytes();
        // Feed one byte at a time, splitting multi-byte characters.
        let mut p = SseParser::default();
        let mut events = Vec::new();
        for b in bytes {
            events.extend(p.feed(std::slice::from_ref(b)));
        }
        assert_eq!(
            events,
            vec![
                SseEvent {
                    event: Some("message_start".into()),
                    data: "{\"a\":1}".into()
                },
                SseEvent {
                    event: None,
                    data: "第一\n二".into()
                },
                SseEvent {
                    event: None,
                    data: "[DONE]".into()
                },
            ]
        );
    }

    #[test]
    fn crlf_and_unterminated_final_event_is_dropped() {
        let mut p = SseParser::default();
        let events = p.feed(b"data:x\r\n\r\ndata: {\"cut\": ");
        assert_eq!(
            events,
            vec![SseEvent {
                event: None,
                data: "x".into()
            }]
        );
        p.finish();
        assert!(p.feed(b"\n\n").is_empty());
    }
}
