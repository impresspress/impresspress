//! Provider-agnostic Server-Sent Events transport parsing.
//!
//! The OpenAI and Anthropic streaming decoders share the same wire framing —
//! accumulate raw bytes, split on the `\n\n` frame separator, and parse each
//! frame's `event:` / `data:` lines — and differ only in how they interpret a
//! decoded frame. [`SseFrameStream`] owns the shared transport half; each
//! provider's decoder layers its own per-frame semantics on top.

use wafer_core::interfaces::llm::service::ChatChunk;

/// One decoded SSE frame: a `\n\n`-terminated block of `key: value` lines.
///
/// `event` is the last `event:` value seen in the frame (if any); `data` is the
/// `data:` lines joined with `\n` (empty when the frame carried none — e.g. a
/// comment or keepalive).
pub struct SseFrame {
    pub event: Option<String>,
    pub data: String,
}

/// A batch of decoded chunks plus the terminal flag, returned by each provider
/// decoder's `push`. Shared so the providers don't each redefine it.
#[derive(Debug, Default, PartialEq)]
pub struct DecodeBatch {
    pub chunks: Vec<ChatChunk>,
    /// True once the stream has terminated (e.g. OpenAI's `[DONE]` sentinel or
    /// Anthropic's `message_stop`). Callers should stop feeding once set.
    pub done: bool,
}

/// Incremental SSE transport parser: accumulates raw bytes and yields complete
/// frames on demand. Knows nothing about chunk semantics.
pub struct SseFrameStream {
    buf: String,
}

impl SseFrameStream {
    pub fn new() -> Self {
        Self { buf: String::new() }
    }

    /// Append raw bytes to the buffer. Returns `false` (buffering nothing) when
    /// `bytes` is not valid UTF-8, so the caller can emit its provider-tagged
    /// warning and bail. Keeps this type free of a `tracing` dependency.
    pub fn feed(&mut self, bytes: &[u8]) -> bool {
        match std::str::from_utf8(bytes) {
            Ok(text) => {
                self.buf.push_str(text);
                true
            }
            Err(_) => false,
        }
    }

    /// Pull the next complete `\n\n`-terminated frame, with its `event:` /
    /// `data:` lines parsed. Returns `None` when no complete frame is buffered
    /// yet — the partial tail stays for the next [`feed`](Self::feed).
    pub fn next_frame(&mut self) -> Option<SseFrame> {
        let sep = self.buf.find("\n\n")?;
        let raw = self.buf[..sep].to_string();
        self.buf.drain(..=sep + 1);
        Some(parse_frame(&raw))
    }
}

/// Parse one raw frame body (the text before a `\n\n`) into its `event` / `data`
/// fields. Strips a leading BOM per line, captures the last `event:` value, and
/// joins multiple `data:` lines with `\n` (the SSE spec's concatenation rule).
fn parse_frame(raw: &str) -> SseFrame {
    let mut event = None;
    let mut data = String::new();
    for line in raw.lines() {
        let line = line.trim_start_matches('\u{feff}');
        if let Some(rest) = line.strip_prefix("event:") {
            event = Some(rest.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("data:") {
            let rest = rest.trim_start();
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(rest);
        }
        // Other SSE fields (`id:`, `retry:`, comments) are ignored.
    }
    SseFrame { event, data }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yields_only_complete_frames() {
        let mut s = SseFrameStream::new();
        // Partial frame — no `\n\n` yet.
        assert!(s.feed(b"data: hello"));
        assert!(s.next_frame().is_none());
        // Completing it yields exactly one frame.
        assert!(s.feed(b"\n\n"));
        let f = s.next_frame().expect("frame");
        assert_eq!(f.data, "hello");
        assert!(s.next_frame().is_none());
    }

    #[test]
    fn joins_multiple_data_lines() {
        let mut s = SseFrameStream::new();
        s.feed(b"data: line1\ndata: line2\n\n");
        let f = s.next_frame().expect("frame");
        assert_eq!(f.data, "line1\nline2");
    }

    #[test]
    fn captures_and_trims_event_name() {
        let mut s = SseFrameStream::new();
        s.feed(b"event: content_block_delta\ndata: {}\n\n");
        let f = s.next_frame().expect("frame");
        assert_eq!(f.event.as_deref(), Some("content_block_delta"));
        assert_eq!(f.data, "{}");
    }

    #[test]
    fn strips_leading_bom() {
        let mut s = SseFrameStream::new();
        s.feed("\u{feff}data: x\n\n".as_bytes());
        let f = s.next_frame().expect("frame");
        assert_eq!(f.data, "x");
    }

    #[test]
    fn comment_or_keepalive_frame_has_empty_data() {
        let mut s = SseFrameStream::new();
        s.feed(b": keepalive\n\n");
        let f = s.next_frame().expect("frame");
        assert!(f.event.is_none());
        assert_eq!(f.data, "");
    }

    #[test]
    fn non_utf8_feed_returns_false_and_buffers_nothing() {
        let mut s = SseFrameStream::new();
        // 0xFF is never valid in UTF-8.
        assert!(!s.feed(&[0xff, 0xfe]));
        assert!(s.next_frame().is_none());
        // A subsequent valid feed still works (buffer wasn't corrupted).
        assert!(s.feed(b"data: ok\n\n"));
        assert_eq!(s.next_frame().expect("frame").data, "ok");
    }

    #[test]
    fn pull_drains_one_frame_at_a_time_leaving_the_rest() {
        let mut s = SseFrameStream::new();
        // Two complete frames plus a partial tail in one buffer.
        s.feed(b"data: a\n\ndata: b\n\ndata: c");
        assert_eq!(s.next_frame().expect("first").data, "a");
        // Pulling one frame must leave the second retrievable...
        assert_eq!(s.next_frame().expect("second").data, "b");
        // ...and the partial tail unparsed.
        assert!(s.next_frame().is_none());
        s.feed(b"\n\n");
        assert_eq!(s.next_frame().expect("third").data, "c");
    }
}
