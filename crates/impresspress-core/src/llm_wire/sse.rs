//! Provider-agnostic Server-Sent Events transport parsing.
//!
//! The OpenAI and Anthropic streaming decoders share the same wire framing —
//! accumulate raw bytes, decode them as UTF-8 across network-chunk
//! boundaries, split on the blank line that ends a frame, and parse each
//! frame's `event:` / `data:` lines — and differ only in how they interpret a
//! decoded frame. [`SseFrameStream`] owns the shared transport half; each
//! provider's decoder layers its own per-frame semantics on top.

use wafer_core::interfaces::llm::service::ChatChunk;

/// One decoded SSE frame: a blank-line-terminated block of `key: value` lines.
///
/// `event` is the last `event:` value seen in the frame (if any); `data` is the
/// `data:` lines joined with `\n` (empty when the frame carried none — e.g. a
/// comment or keepalive).
pub struct SseFrame {
    /// Read only by the Anthropic decoder, which is gated on `feature = "llm"`
    /// (reqwest + tokio, neither of which builds for wasm32). OpenAI's wire
    /// ignores `event:` entirely, so in a build that carries only the OpenAI
    /// consumer — `impresspress-browser` takes this crate with no default
    /// features — the field is parsed and never read. The suppression is scoped
    /// to exactly that configuration so a genuinely dead field still warns in
    /// the build that has both readers. `not(test)` is part of that scope: the
    /// module's own tests assert on `event`, so in a test build of the
    /// no-`llm` shape the field has a reader and the lint is silent.
    #[cfg_attr(
        all(not(feature = "llm"), not(test)),
        expect(
            dead_code,
            reason = "outside the tests only the Anthropic decoder reads it, and \
                      that decoder is `feature = \"llm\"`-gated"
        )
    )]
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
    /// What the transport layer dropped while producing this batch. Non-empty
    /// means part of the answer is gone, so the chunks here are a prefix of
    /// what the model said and nothing after the loss can be trusted to join
    /// onto them — the consumer delivers this batch and then fails the
    /// stream rather than emitting a reply with a hole in it.
    pub lost: FeedLoss,
}

/// What a [`feed`](SseFrameStream::feed) lost, if anything.
///
/// Both kinds can happen in one feed, so this is a set rather than a single
/// reason. What is lost is part of the answer, so a consumer that must not
/// deliver a reply with a hole in it treats a non-empty loss as fatal — the
/// native provider service (`blocks::llm::providers::service`) ends the
/// stream with an error. Keeping this a plain value rather than a log line
/// keeps the type free of a `tracing` dependency and leaves that decision to
/// the consumer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FeedLoss {
    /// A byte sequence that is not valid UTF-8 and not merely an incomplete
    /// tail was replaced with U+FFFD, and decoding continued after it. What
    /// those bytes meant is gone even though the frame around them survives.
    pub invalid_utf8: bool,
    /// One frame exceeded [`MAX_PENDING_FRAME_BYTES`] without a terminating
    /// blank line; its bytes are dropped up to and including the next blank
    /// line so the buffer cannot grow without bound. The whole frame is gone,
    /// which for a text delta is a missing piece of the answer.
    pub frame_too_large: bool,
}

impl FeedLoss {
    /// True when anything at all was dropped.
    pub fn any(self) -> bool {
        self.invalid_utf8 || self.frame_too_large
    }
}

impl std::fmt::Display for FeedLoss {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match (self.invalid_utf8, self.frame_too_large) {
            (true, true) => f.write_str("invalid UTF-8 and an oversized frame"),
            (true, false) => f.write_str("invalid UTF-8"),
            (false, true) => f.write_str("an oversized frame"),
            (false, false) => f.write_str("nothing"),
        }
    }
}

/// Cap on the bytes one un-terminated frame may occupy — the text after the
/// last blank line, not the whole buffer, so frames the consumer has not
/// drained yet never count against it. A provider that frames its stream in a
/// way this parser does not recognise (or a hostile one that never terminates
/// a frame) would otherwise grow `buf` for as long as the connection lives.
/// Real frames are a few kilobytes at most, so a mebibyte is far above
/// anything legitimate.
pub const MAX_PENDING_FRAME_BYTES: usize = 1024 * 1024;

/// Incremental SSE transport parser: accumulates raw bytes and yields complete
/// frames on demand. Knows nothing about chunk semantics.
///
/// Feeds are transport chunks, not message boundaries: a multi-byte character
/// or a frame separator can be split across two of them, so the decoded tail
/// of a feed is carried over rather than discarded.
pub struct SseFrameStream {
    /// Decoded text with line endings normalised to `\n`, awaiting frame
    /// extraction.
    buf: String,
    /// The trailing bytes of a UTF-8 sequence the last feed cut in half. They
    /// are decoded once the rest of the sequence arrives; at most three bytes
    /// by construction.
    partial: Vec<u8>,
    /// The last decoded character was a `\r`, which already produced a `\n` in
    /// `buf`. A `\n` at the head of the next text is that CR's LF half and is
    /// dropped, so a CRLF split across feeds still counts as one line ending.
    pending_cr: bool,
    /// A frame passed [`MAX_PENDING_FRAME_BYTES`] and is being discarded until
    /// the next blank line, so its tail is never parsed as a frame of its own.
    dropping_frame: bool,
    /// While `dropping_frame`, the byte index in `buf` where that frame
    /// starts. Everything before it is complete frames the consumer has not
    /// drained yet, which must neither be discarded nor searched for the
    /// separator that ends the oversized one.
    drop_from: usize,
}

impl Default for SseFrameStream {
    fn default() -> Self {
        Self::new()
    }
}

impl SseFrameStream {
    pub fn new() -> Self {
        Self {
            buf: String::new(),
            partial: Vec::new(),
            pending_cr: false,
            dropping_frame: false,
            drop_from: 0,
        }
    }

    /// Append raw bytes to the buffer, decoding as much of them as forms whole
    /// UTF-8 characters and holding an incomplete trailing sequence back for
    /// the next feed.
    ///
    /// Returns what was lost, if anything (see [`FeedLoss`]); everything
    /// decodable is buffered regardless, so the caller can drain the frames
    /// that did arrive before it acts on the loss.
    ///
    /// Callers must drain with [`next_frame`](Self::next_frame) until it
    /// returns `None` before the next feed. Buffered-but-undrained frames do
    /// not count towards [`MAX_PENDING_FRAME_BYTES`] — only the unterminated
    /// tail does — so a consumer that stops draining while the provider keeps
    /// sending complete frames is the one case that still grows memory.
    pub fn feed(&mut self, bytes: &[u8]) -> FeedLoss {
        let mut loss = FeedLoss::default();

        // Re-attach the sequence the previous feed could not finish decoding.
        let joined: Vec<u8>;
        let mut rest: &[u8] = if self.partial.is_empty() {
            bytes
        } else {
            joined = self
                .partial
                .drain(..)
                .chain(bytes.iter().copied())
                .collect();
            &joined
        };

        loop {
            match std::str::from_utf8(rest) {
                Ok(text) => {
                    self.push_text(text);
                    break;
                }
                Err(e) => {
                    let valid = e.valid_up_to();
                    let prefix = std::str::from_utf8(&rest[..valid])
                        .expect("valid_up_to() bounds a valid UTF-8 prefix");
                    self.push_text(prefix);
                    match e.error_len() {
                        // A genuinely invalid sequence: substitute the
                        // replacement character and keep decoding, so one bad
                        // byte costs one character rather than the rest of the
                        // stream. Substituting rather than deleting is what
                        // the Unicode standard's decoder does, and it keeps
                        // the line structure the framing depends on: dropping
                        // the bytes outright can leave a line that was not
                        // empty looking empty, which invents a frame boundary
                        // where the provider sent none. Routing it through
                        // `push_text` is also what clears a pending CR — the
                        // bytes between a CR and an LF mean they are two line
                        // endings, not one.
                        Some(len) => {
                            loss.invalid_utf8 = true;
                            self.push_text(char::REPLACEMENT_CHARACTER.encode_utf8(&mut [0; 4]));
                            rest = &rest[valid + len..];
                        }
                        // A character split across this chunk boundary: hold
                        // its head for the next feed to complete.
                        None => {
                            self.partial.extend_from_slice(&rest[valid..]);
                            break;
                        }
                    }
                }
            }
        }

        if self.enforce_frame_cap() {
            loss.frame_too_large = true;
        }
        loss
    }

    /// Append decoded text, normalising SSE's three line endings (`\r\n`,
    /// `\r`, `\n`) to `\n` so [`next_frame`](Self::next_frame) recognises a
    /// blank line in any of them. A `\r` at the end of a feed emits its `\n`
    /// immediately and remembers itself, so the `\n` that may open the next
    /// feed is not mistaken for a second line ending — which is what would
    /// otherwise split a frame in half mid-CRLF.
    fn push_text(&mut self, text: &str) {
        // Text with no CR needs no rewriting, so an LF-framed stream costs one
        // copy rather than a per-character walk.
        if !self.pending_cr && !text.contains('\r') {
            self.buf.push_str(text);
            return;
        }
        for ch in text.chars() {
            match ch {
                '\n' if self.pending_cr => self.pending_cr = false,
                '\r' => {
                    self.pending_cr = true;
                    self.buf.push('\n');
                }
                other => {
                    self.pending_cr = false;
                    self.buf.push(other);
                }
            }
        }
    }

    /// Bound the unterminated tail of `buf`. Returns true when this call
    /// started dropping an oversized frame, so [`feed`](Self::feed) reports it
    /// once rather than on every feed that follows.
    fn enforce_frame_cap(&mut self) -> bool {
        if self.dropping_frame {
            // Search only the tail being discarded: a separator earlier in
            // `buf` belongs to a complete frame the consumer has yet to drain.
            match self.buf[self.drop_from..].find("\n\n") {
                // The oversized frame finally ended: drop it and resume.
                Some(rel) => {
                    let end = self.drop_from + rel + 2;
                    self.buf.replace_range(self.drop_from..end, "");
                    self.dropping_frame = false;
                }
                // Still inside it.
                None => self.shrink_dropped_frame(),
            }
            return false;
        }
        let start = self.pending_frame_start();
        if self.buf.len() - start <= MAX_PENDING_FRAME_BYTES {
            return false;
        }
        self.drop_from = start;
        self.dropping_frame = true;
        self.shrink_dropped_frame();
        true
    }

    /// Byte index where the frame still awaiting its terminator starts — just
    /// past the last blank line, or the start of the buffer when none has
    /// arrived yet.
    fn pending_frame_start(&self) -> usize {
        self.buf.rfind("\n\n").map_or(0, |i| i + 2)
    }

    /// Discard the oversized frame's buffered bytes, keeping the complete
    /// frames before it and its own final character — which may be the first
    /// half of the separator that ends it.
    fn shrink_dropped_frame(&mut self) {
        let keep_from = self
            .buf
            .char_indices()
            .next_back()
            .map_or(self.buf.len(), |(i, _)| i);
        if keep_from > self.drop_from {
            self.buf.replace_range(self.drop_from..keep_from, "");
        }
    }

    /// Pull the next complete blank-line-terminated frame, with its `event:` /
    /// `data:` lines parsed. Returns `None` when no complete frame is buffered
    /// yet — the partial tail stays for the next [`feed`](Self::feed).
    pub fn next_frame(&mut self) -> Option<SseFrame> {
        let sep = self.buf.find("\n\n")?;
        let raw = self.buf[..sep].to_string();
        self.buf.drain(..=sep + 1);
        // Draining from the front moves everything after it, including the
        // oversized frame this stream may be discarding. That frame always
        // starts after this separator: `enforce_frame_cap` leaves no blank
        // line inside the region being dropped, so the first one in the
        // buffer is always ahead of `drop_from`.
        if self.dropping_frame {
            debug_assert!(
                self.drop_from >= sep + 2,
                "the dropped frame starts at {} but a separator was found at {sep}",
                self.drop_from
            );
            self.drop_from = self.drop_from.saturating_sub(sep + 2);
        }
        Some(parse_frame(&raw))
    }

    /// Bytes received that are not yet part of any frame: an unterminated
    /// frame, or the head of a character the stream was cut in the middle of.
    ///
    /// Whitespace is not counted — a provider may end its body with a stray
    /// newline and still have said everything it had to say. Anything else
    /// means the body stopped mid-frame, which a consumer deciding whether a
    /// completion is whole wants to know.
    pub fn has_unparsed_input(&self) -> bool {
        !self.partial.is_empty() || !self.buf.trim().is_empty()
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
        // Partial frame — no blank line yet.
        assert!(!s.feed(b"data: hello").any());
        assert!(s.next_frame().is_none());
        // Completing it yields exactly one frame.
        assert!(!s.feed(b"\n\n").any());
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
    fn invalid_utf8_becomes_a_replacement_char_and_is_reported() {
        let mut s = SseFrameStream::new();
        // 0xFF is never valid in UTF-8 — and it must cost exactly itself: the
        // frame it sits in still arrives, with the bad byte replaced.
        let mut bytes = b"data: o".to_vec();
        bytes.extend_from_slice(&[0xff]);
        bytes.extend_from_slice(b"k\n\n");
        assert_eq!(
            s.feed(&bytes),
            FeedLoss {
                invalid_utf8: true,
                frame_too_large: false
            }
        );
        assert_eq!(
            s.next_frame().expect("frame").data,
            "o\u{fffd}k",
            "the corrupt byte is visible in the payload, not silently dropped"
        );
        // A subsequent valid feed still works (buffer wasn't corrupted).
        assert!(!s.feed(b"data: next\n\n").any());
        assert_eq!(s.next_frame().expect("frame").data, "next");
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

    /// Drain every frame the stream currently holds, joining their `data`.
    fn drain(s: &mut SseFrameStream) -> String {
        let mut out = String::new();
        while let Some(f) = s.next_frame() {
            out.push_str(&f.data);
        }
        out
    }

    /// A network chunk boundary is a transport boundary, not a character
    /// boundary: reqwest hands over whatever arrived. Splitting a frame at
    /// *every* byte offset must still decode to the same text — the split that
    /// lands inside a multi-byte character is the one that used to lose both
    /// halves (an incomplete lead sequence ends chunk N, bare continuation
    /// bytes open chunk N+1, so both chunks failed `from_utf8`).
    #[test]
    fn a_character_split_across_chunks_survives_at_every_offset() {
        for payload in ["héllo 🙂 wörld", "日本語のテキスト", "🙂🙂🙂"] {
            let wire = format!("data: {payload}\n\n");
            let bytes = wire.as_bytes();
            for split in 0..=bytes.len() {
                let mut s = SseFrameStream::new();
                assert!(
                    !s.feed(&bytes[..split]).any(),
                    "a split at {split} is a chunk boundary, not a decode error"
                );
                assert!(!s.feed(&bytes[split..]).any(), "second half of {split}");
                assert_eq!(
                    drain(&mut s),
                    payload,
                    "payload {payload:?} lost content when split at byte {split}"
                );
            }
        }
    }

    /// The same, with the character split three ways — one continuation byte
    /// per feed — which is what a slow connection produces.
    #[test]
    fn a_character_split_byte_by_byte_survives() {
        let wire = "data: 🙂 ok\n\n";
        let mut s = SseFrameStream::new();
        for b in wire.as_bytes() {
            assert!(
                !s.feed(&[*b]).any(),
                "byte-at-a-time feed is never a discard"
            );
        }
        assert_eq!(drain(&mut s), "🙂 ok");
    }

    /// A multi-byte character split across the boundary must not corrupt the
    /// *next* frame either: the halves belong to one character, so the frames
    /// around them stay intact.
    #[test]
    fn frames_after_a_split_character_are_not_corrupted() {
        let wire = "data: {\"t\":\"é\"}\n\ndata: {\"t\":\"b\"}\n\n";
        let bytes = wire.as_bytes();
        for split in 0..=bytes.len() {
            let mut s = SseFrameStream::new();
            s.feed(&bytes[..split]);
            s.feed(&bytes[split..]);
            let mut frames = Vec::new();
            while let Some(f) = s.next_frame() {
                frames.push(f.data);
            }
            assert_eq!(
                frames,
                vec!["{\"t\":\"é\"}".to_string(), "{\"t\":\"b\"}".to_string()],
                "split at {split} corrupted the frame sequence"
            );
        }
    }

    /// SSE terminates a line with CRLF, LF or CR, so a blank line — the frame
    /// separator — can arrive as any of the three. A provider that uses CRLF
    /// used to yield no frame at all while its bytes accumulated forever.
    #[test]
    fn crlf_and_cr_framing_yield_frames() {
        for wire in [
            "event: e\r\ndata: hi\r\n\r\n",
            "event: e\rdata: hi\r\r",
            "event: e\r\ndata: hi\n\n",
        ] {
            let mut s = SseFrameStream::new();
            assert!(!s.feed(wire.as_bytes()).any());
            let f = s
                .next_frame()
                .unwrap_or_else(|| panic!("{wire:?} must yield a frame"));
            assert_eq!(f.event.as_deref(), Some("e"), "wire {wire:?}");
            assert_eq!(f.data, "hi", "wire {wire:?}");
        }
    }

    /// A CRLF separator split between the CR and the LF is still one line
    /// ending, not two — otherwise the frame would be cut in half.
    #[test]
    fn a_crlf_split_across_feeds_is_one_line_ending() {
        let wire = "data: hi\r\n\r\n";
        let bytes = wire.as_bytes();
        for split in 0..=bytes.len() {
            let mut s = SseFrameStream::new();
            s.feed(&bytes[..split]);
            s.feed(&bytes[split..]);
            let mut frames = Vec::new();
            while let Some(f) = s.next_frame() {
                frames.push(f.data);
            }
            assert_eq!(frames, vec!["hi".to_string()], "split at {split}");
        }
    }

    /// A bad byte must not invent a frame boundary.
    ///
    /// Deleting an invalid sequence can leave a line that carried content
    /// looking empty, and an empty line is exactly what ends a frame — so a
    /// single corrupt byte would split one frame into two and hand the
    /// decoder half a payload. Substituting the replacement character keeps
    /// the line, so the frame stays whole.
    #[test]
    fn an_invalid_byte_does_not_split_the_frame_it_lands_in() {
        let mut s = SseFrameStream::new();
        // The bad byte is the whole of its line, between two LFs: delete it
        // and that line is empty, which ends the frame. This is the input that
        // separates substitution from deletion — with a CR before the bad byte
        // the two agree by accident, because a surviving pending CR would eat
        // the LF after it and rejoin what deletion had split.
        let mut bytes = b"data: a\n".to_vec();
        bytes.push(0xff);
        bytes.extend_from_slice(b"\ndata: b\n\n");
        assert!(s.feed(&bytes).invalid_utf8, "the loss is reported");
        let f = s.next_frame().expect("one frame");
        assert_eq!(
            f.data, "a\nb",
            "the bad byte's line is not a blank line, so the frame is still one frame"
        );
        assert!(s.next_frame().is_none(), "and there is no second frame");
    }

    /// Both kinds of loss in one feed are both reported — the invalid sequence
    /// is not hidden by the overflow that follows it.
    #[test]
    fn a_feed_that_loses_two_ways_reports_both() {
        let mut s = SseFrameStream::new();
        let mut bytes = vec![0xff];
        bytes.extend_from_slice(&vec![b'x'; MAX_PENDING_FRAME_BYTES + 1]);
        assert_eq!(
            s.feed(&bytes),
            FeedLoss {
                invalid_utf8: true,
                frame_too_large: true
            }
        );
    }

    /// The cap bounds one frame, not the buffer: frames the consumer has not
    /// drained yet are its to collect, and must not be discarded as though
    /// they were one runaway frame. This guards the naive whole-buffer cap
    /// rather than a bug that shipped — the implementation this replaced let
    /// the same case through for a different reason (it skipped the cap
    /// entirely whenever the buffer held any separator at all).
    #[test]
    fn undrained_complete_frames_do_not_trip_the_cap() {
        let mut s = SseFrameStream::new();
        // Well past the cap in total, but every frame is complete and small.
        let frame = format!("data: {}\n\n", "y".repeat(8 * 1024));
        let mut frames = 0;
        while s.buf.len() < 2 * MAX_PENDING_FRAME_BYTES {
            assert!(
                !s.feed(frame.as_bytes()).any(),
                "complete frames are not a loss"
            );
            frames += 1;
        }
        let mut drained = 0;
        while let Some(f) = s.next_frame() {
            assert_eq!(f.data.len(), 8 * 1024);
            drained += 1;
        }
        assert_eq!(drained, frames, "every buffered frame is still retrievable");
    }

    /// A frame dropped for being oversized must not take the complete frames
    /// buffered ahead of it — nor find its terminator in one of them.
    #[test]
    fn dropping_an_oversized_frame_spares_the_frames_before_it() {
        let mut s = SseFrameStream::new();
        assert!(!s.feed(b"data: keep me\n\n").any());
        // Now an unterminated frame that runs past the cap, fed in pieces.
        let mut tripped = false;
        for _ in 0..20 {
            tripped |= s.feed(&vec![b'z'; 64 * 1024]).frame_too_large;
        }
        assert!(tripped, "the oversized frame is reported");
        // The frame buffered before it survives...
        assert_eq!(
            s.next_frame().expect("the earlier frame").data,
            "keep me",
            "a complete frame must not be discarded with the oversized one"
        );
        assert!(s.next_frame().is_none(), "the dropped frame yields nothing");
        // ...and the stream resumes on the frame after the dropped one.
        assert!(!s.feed(b"tail of the big one\n\ndata: ok\n\n").any());
        assert_eq!(drain(&mut s), "ok");
    }

    /// A body that stops mid-frame or mid-character leaves bytes that never
    /// became a frame, which is how a consumer tells a clean end from a cut.
    #[test]
    fn unparsed_input_reports_a_cut_body() {
        let mut s = SseFrameStream::new();
        assert!(!s.has_unparsed_input(), "a fresh stream holds nothing");
        s.feed(b"data: whole\n\n");
        while s.next_frame().is_some() {}
        assert!(
            !s.has_unparsed_input(),
            "a body that ended on a frame boundary is not a cut"
        );
        // Trailing whitespace is not a cut either.
        s.feed(b"\n");
        assert!(!s.has_unparsed_input());
        // Half a frame is.
        s.feed(b"data: half");
        assert!(s.has_unparsed_input());
        // So is half a character.
        let mut s2 = SseFrameStream::new();
        s2.feed(&"🙂".as_bytes()[..2]);
        assert!(s2.has_unparsed_input());
    }

    /// An un-terminated frame cannot grow without bound: past the cap its
    /// bytes are dropped until the next blank line, and the stream recovers on
    /// the frame after it.
    #[test]
    fn an_unterminated_frame_is_bounded_and_the_stream_recovers() {
        let mut s = SseFrameStream::new();
        let mut overflows = 0;
        // 2 MiB of a single frame that never ends.
        for _ in 0..32 {
            if s.feed(&vec![b'x'; 64 * 1024]).frame_too_large {
                overflows += 1;
            }
        }
        assert_eq!(overflows, 1, "the overflow is reported once, not per feed");
        assert!(
            s.buf.len() <= MAX_PENDING_FRAME_BYTES,
            "buffer stayed bounded, got {} bytes",
            s.buf.len()
        );
        // The rest of the oversized frame is discarded, and the next frame
        // decodes normally.
        assert!(!s.feed(b"more junk\n\ndata: ok\n\n").any());
        assert_eq!(drain(&mut s), "ok");
    }
}
