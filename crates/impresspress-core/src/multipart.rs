//! Request-side `multipart/form-data` parsing.
//!
//! Browsers upload files via `FormData`, which fetch/XHR send as a
//! `multipart/form-data` body: the file bytes are wrapped in a boundary
//! envelope with per-part headers. A handler that stores the raw request
//! body therefore stores the *envelope*, not the file — [`extract_multipart_file`]
//! pulls out the file part so upload handlers can store the actual content.
//!
//! This is a deliberately small, dependency-free, synchronous parser over an
//! already-buffered body (upload handlers buffer through a quota-capped
//! collect anyway) — NOT a general streaming multipart implementation. It
//! implements the subset of RFC 7578/2046 that real user agents produce:
//! CRLF line endings, `--{boundary}` delimiters at line starts, and a
//! closing `--{boundary}--` delimiter. Bodies that violate that framing
//! (e.g. a truncated body with no closing delimiter) yield `None` rather
//! than a guess, so callers can reject them as malformed.
//!
//! The counterpart request-body helpers for the other browser form encoding
//! (`application/x-www-form-urlencoded`) live in [`crate::util::parse_form_body`].

/// One file part extracted from a `multipart/form-data` body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MultipartFile {
    /// The part's body — the actual file bytes.
    pub content: Vec<u8>,
    /// `filename` parameter of the part's `Content-Disposition`, if present.
    pub filename: Option<String>,
    /// The part's own `Content-Type` header, if present.
    pub content_type: Option<String>,
}

/// Extract the boundary parameter from a `multipart/form-data` content type.
///
/// Returns `None` when the mime type is not `multipart/form-data`, no
/// (non-empty) `boundary=` parameter is present, or the boundary value
/// contains a control character (CR, LF, or other ASCII control byte) —
/// which is also the caller's "is this request multipart at all?" predicate.
/// Handles quoted boundaries and case-insensitive mime/parameter names.
///
/// Control characters (notably a raw CR/LF) are illegal in a boundary per
/// RFC 2046 §5.1.1's `bcharsnospace` grammar; rejecting them here — before
/// the boundary is ever turned into a line-framing delimiter — keeps a
/// malformed boundary from producing overlapping delimiter-line positions
/// downstream in [`extract_multipart_file`].
pub fn multipart_boundary(content_type: &str) -> Option<String> {
    let mut parts = content_type.split(';');
    let mime = parts.next()?.trim();
    if !mime.eq_ignore_ascii_case("multipart/form-data") {
        return None;
    }
    for param in parts {
        let param = param.trim();
        let Some((name, value)) = param.split_once('=') else {
            continue;
        };
        if !name.trim().eq_ignore_ascii_case("boundary") {
            continue;
        }
        let value = value.trim();
        let value = value
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .unwrap_or(value);
        if value.is_empty() {
            continue;
        }
        if value.bytes().any(|b| b.is_ascii_control()) {
            // A malformed boundary (e.g. containing a raw CRLF) — fail the
            // whole parse closed rather than keep scanning for another
            // `boundary=` parameter.
            return None;
        }
        return Some(value.to_string());
    }
    None
}

/// Extract the file part from a buffered `multipart/form-data` body.
///
/// `content_type` is the request's `Content-Type` header (it carries the
/// boundary). The file part is the first part whose `Content-Disposition`
/// has a `filename` parameter, falling back to the first part named `file`
/// (the field name the impresspress file browser uses) when no part declares a
/// filename. Returns `None` when the content type is not multipart, the
/// framing is malformed, or no file part exists.
pub fn extract_multipart_file(body: &[u8], content_type: &str) -> Option<MultipartFile> {
    let boundary = multipart_boundary(content_type)?;
    let delimiter = format!("--{boundary}");
    let delimiter = delimiter.as_bytes();

    // Delimiter occurrences that start a line (position 0 or preceded by
    // CRLF, per RFC 2046 §5.1.1 — a boundary-looking byte run *inside* part
    // content does not start a line and must not split the part).
    let mut positions = Vec::new();
    let mut from = 0;
    while let Some(pos) = find(body, delimiter, from) {
        if pos == 0 || (pos >= 2 && &body[pos - 2..pos] == b"\r\n") {
            positions.push(pos);
        }
        from = pos + 1;
    }

    // Each part spans two consecutive delimiters; the closing `--{boundary}--`
    // is itself found by the scan above, so it terminates the last part.
    let mut named_file_fallback: Option<MultipartFile> = None;
    for pair in positions.windows(2) {
        let (start, end) = (pair[0], pair[1]);
        let after = start + delimiter.len();
        if body[after..].starts_with(b"--") {
            // Closing delimiter — nothing follows but the epilogue.
            break;
        }
        // Skip optional transport padding (spaces/tabs), then require the
        // CRLF that ends the delimiter line.
        let mut cursor = after;
        while cursor < end && (body[cursor] == b' ' || body[cursor] == b'\t') {
            cursor += 1;
        }
        // `cursor` can exceed `end` when two line-start delimiter positions
        // overlap (only reachable if a malformed boundary — e.g. one
        // containing a raw CRLF — ever got this far); guard the slice so
        // that can never index-panic even if `multipart_boundary`'s
        // control-char rejection above is ever bypassed or loosened.
        if cursor > end || !body[cursor..end].starts_with(b"\r\n") {
            continue;
        }
        cursor += 2;
        // Part content ends at the CRLF that precedes the next delimiter
        // (guaranteed present — `end` qualified via the line-start check).
        let Some(content_end) = end.checked_sub(2) else {
            continue;
        };
        if cursor > content_end {
            continue;
        }
        let part = &body[cursor..content_end];

        // Split part headers from part content on the empty line.
        let (headers_raw, content) = if let Some(rest) = part.strip_prefix(b"\r\n".as_slice()) {
            // A (legal, if unusual) part with zero headers.
            (&[] as &[u8], rest)
        } else if let Some(headers_end) = find(part, b"\r\n\r\n", 0) {
            (&part[..headers_end], &part[headers_end + 4..])
        } else {
            continue; // No header/content separator — malformed part.
        };

        let mut disposition = String::new();
        let mut part_content_type: Option<String> = None;
        for line in String::from_utf8_lossy(headers_raw).split("\r\n") {
            let Some((name, value)) = line.split_once(':') else {
                continue;
            };
            let value = value.trim();
            if name.trim().eq_ignore_ascii_case("content-disposition") {
                disposition = value.to_string();
            } else if name.trim().eq_ignore_ascii_case("content-type") {
                part_content_type = Some(value.to_string());
            }
        }

        let filename = disposition_param(&disposition, "filename");
        let field_name = disposition_param(&disposition, "name");
        let file = MultipartFile {
            content: content.to_vec(),
            filename,
            content_type: part_content_type,
        };
        if file.filename.is_some() {
            return Some(file);
        }
        if named_file_fallback.is_none() && field_name.as_deref() == Some("file") {
            named_file_fallback = Some(file);
        }
    }
    named_file_fallback
}

/// Extract a (possibly quoted) parameter value from a `Content-Disposition`
/// header value, e.g. `filename` from
/// `form-data; name="file"; filename="index.html"`. Parameter-name matching
/// is exact-but-case-insensitive, so `filename*` (RFC 5987 extended syntax)
/// does not match `filename`.
fn disposition_param(disposition: &str, param: &str) -> Option<String> {
    for token in disposition.split(';') {
        let Some((name, value)) = token.split_once('=') else {
            continue;
        };
        if !name.trim().eq_ignore_ascii_case(param) {
            continue;
        }
        let value = value.trim();
        let value = value
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .unwrap_or(value);
        return Some(value.to_string());
    }
    None
}

/// First occurrence of `needle` in `haystack` at or after `from`.
fn find(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || from > haystack.len() {
        return None;
    }
    haystack[from..]
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|pos| pos + from)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact envelope shape observed live (WebKit `FormData` upload of
    /// `index.html` through the file browser): the extracted content must be
    /// the inner file bytes — NOT the envelope — plus the part's filename
    /// and content type.
    #[test]
    fn webkit_form_boundary_envelope_extracts_inner_file_bytes() {
        // An HTML *fragment* (no doctype/page-root tags): the parser is
        // content-agnostic, so keeping page-chrome markers out of the fixture
        // keeps the coarse `scripts/grep-guard-html.sh` guard happy.
        let file_bytes: &[u8] = b"<h1>hello from impresspress</h1>\n<p>an uploaded page</p>\n";
        let boundary = "----WebKitFormBoundaryqHHDhrDMqZoc7sHW";
        let mut body = Vec::new();
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        body.extend_from_slice(
            b"Content-Disposition: form-data; name=\"file\"; filename=\"index.html\"\r\n",
        );
        body.extend_from_slice(b"Content-Type: text/html\r\n\r\n");
        body.extend_from_slice(file_bytes);
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());

        let content_type = format!("multipart/form-data; boundary={boundary}");
        let file = extract_multipart_file(&body, &content_type).expect("file part");
        assert_eq!(
            file.content, file_bytes,
            "must be the file, not the envelope"
        );
        assert_eq!(file.filename.as_deref(), Some("index.html"));
        assert_eq!(file.content_type.as_deref(), Some("text/html"));
    }

    /// Binary content survives intact: interior CRLFs, NUL bytes, and even a
    /// `\r\n--`-prefixed run that *almost* looks like a delimiter must not
    /// split or truncate the part.
    #[test]
    fn binary_content_with_crlf_and_near_boundary_bytes_is_preserved() {
        let file_bytes: &[u8] = b"\x00\x01\r\n--not-the-boundary\r\nmore\x00bytes\r\n";
        let boundary = "b0undary123";
        let mut body = Vec::new();
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        body.extend_from_slice(
            b"Content-Disposition: form-data; name=\"file\"; filename=\"blob.bin\"\r\n",
        );
        body.extend_from_slice(b"Content-Type: application/octet-stream\r\n\r\n");
        body.extend_from_slice(file_bytes);
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());

        let file =
            extract_multipart_file(&body, &format!("multipart/form-data; boundary={boundary}"))
                .expect("file part");
        assert_eq!(file.content, file_bytes);
    }

    /// A text field before the file (the common multi-field `FormData` shape)
    /// is skipped: the part WITH a filename wins.
    #[test]
    fn picks_the_file_part_not_a_preceding_text_field() {
        let boundary = "XyZ";
        let body = concat!(
            "--XyZ\r\n",
            "Content-Disposition: form-data; name=\"description\"\r\n",
            "\r\n",
            "a text field, not the file\r\n",
            "--XyZ\r\n",
            "Content-Disposition: form-data; name=\"upload\"; filename=\"a.txt\"\r\n",
            "Content-Type: text/plain\r\n",
            "\r\n",
            "file content\r\n",
            "--XyZ--\r\n",
        )
        .as_bytes();

        let file =
            extract_multipart_file(body, &format!("multipart/form-data; boundary={boundary}"))
                .expect("file part");
        assert_eq!(file.content, b"file content");
        assert_eq!(file.filename.as_deref(), Some("a.txt"));
    }

    /// No part has a filename, but one is named `file` (the field name the
    /// file browser uses) — that part is the fallback.
    #[test]
    fn falls_back_to_the_part_named_file_when_no_filename() {
        let body = concat!(
            "--B\r\n",
            "Content-Disposition: form-data; name=\"file\"\r\n",
            "\r\n",
            "content without filename\r\n",
            "--B--\r\n",
        )
        .as_bytes();

        let file = extract_multipart_file(body, "multipart/form-data; boundary=B")
            .expect("fallback file part");
        assert_eq!(file.content, b"content without filename");
        assert_eq!(file.filename, None);
        assert_eq!(file.content_type, None);
    }

    /// A part without its own `Content-Type` header reports `None` so the
    /// caller can apply its fallback (extension-based detection).
    #[test]
    fn part_without_content_type_reports_none() {
        let body = concat!(
            "--B\r\n",
            "Content-Disposition: form-data; name=\"file\"; filename=\"x.bin\"\r\n",
            "\r\n",
            "data\r\n",
            "--B--\r\n",
        )
        .as_bytes();

        let file =
            extract_multipart_file(body, "multipart/form-data; boundary=B").expect("file part");
        assert_eq!(file.content_type, None);
        assert_eq!(file.filename.as_deref(), Some("x.bin"));
    }

    /// Zero-length files are legal and round-trip as empty content.
    #[test]
    fn empty_file_part_yields_empty_content() {
        let body = concat!(
            "--B\r\n",
            "Content-Disposition: form-data; name=\"file\"; filename=\"empty.txt\"\r\n",
            "\r\n",
            "\r\n",
            "--B--\r\n",
        )
        .as_bytes();

        let file =
            extract_multipart_file(body, "multipart/form-data; boundary=B").expect("file part");
        assert_eq!(file.content, b"");
    }

    /// Malformed / non-multipart inputs yield `None` instead of a guess.
    #[test]
    fn rejects_non_multipart_and_malformed_bodies() {
        // Not multipart at all.
        assert_eq!(extract_multipart_file(b"raw body", "text/plain"), None);
        // Multipart mime but no boundary parameter.
        assert_eq!(extract_multipart_file(b"x", "multipart/form-data"), None);
        // Truncated body: no closing delimiter, so the part has no end.
        let truncated = concat!(
            "--B\r\n",
            "Content-Disposition: form-data; name=\"file\"; filename=\"a\"\r\n",
            "\r\n",
            "content that never terminates",
        )
        .as_bytes();
        assert_eq!(
            extract_multipart_file(truncated, "multipart/form-data; boundary=B"),
            None
        );
        // No file part (text fields only).
        let no_file = concat!(
            "--B\r\n",
            "Content-Disposition: form-data; name=\"note\"\r\n",
            "\r\n",
            "text\r\n",
            "--B--\r\n",
        )
        .as_bytes();
        assert_eq!(
            extract_multipart_file(no_file, "multipart/form-data; boundary=B"),
            None
        );
    }

    #[test]
    fn boundary_parsing_handles_quotes_case_and_extra_params() {
        assert_eq!(
            multipart_boundary("multipart/form-data; boundary=abc"),
            Some("abc".to_string())
        );
        assert_eq!(
            multipart_boundary("multipart/form-data; boundary=\"quoted-b\""),
            Some("quoted-b".to_string())
        );
        assert_eq!(
            multipart_boundary("Multipart/Form-Data; charset=utf-8; Boundary=xyz"),
            Some("xyz".to_string())
        );
        assert_eq!(multipart_boundary("multipart/form-data"), None);
        assert_eq!(multipart_boundary("multipart/form-data; boundary="), None);
        assert_eq!(multipart_boundary("application/json"), None);
        assert_eq!(
            multipart_boundary("multipart/mixed; boundary=abc"),
            None,
            "only form-data bodies are form uploads"
        );
    }

    /// Regression: a boundary containing a raw CRLF must not panic.
    ///
    /// Before the fix, `boundary="\r\n--"` produced overlapping delimiter-line
    /// positions (the same CRLF run satisfies the line-start check for two
    /// consecutive `positions` entries), so `end - start < delimiter.len()`
    /// and `cursor` (`= start + delimiter.len()`) exceeded `end`, panicking
    /// the `body[cursor..end]` slice with "slice index starts at 6 but ends
    /// at 4". This is not reachable via HTTP (header values forbid raw
    /// CR/LF at the axum and Cloudflare transports), but `extract_multipart_file`
    /// is `pub` and must be self-contained-safe against untrusted input.
    #[test]
    fn crlf_containing_boundary_does_not_panic_and_returns_none() {
        let content_type = "multipart/form-data; boundary=\"\r\n--\"";
        let body = b"--\r\n--\r\n--\r\n";

        // The malformed boundary is rejected up front...
        assert_eq!(multipart_boundary(content_type), None);
        // ...so the parse as a whole fails closed instead of panicking.
        assert_eq!(extract_multipart_file(body, content_type), None);
    }

    /// Any ASCII control byte in the boundary (not just CR/LF) is rejected —
    /// RFC 2046 §5.1.1's `bcharsnospace` grammar doesn't allow control
    /// characters in a boundary at all.
    #[test]
    fn boundary_with_other_control_chars_is_rejected() {
        assert_eq!(
            multipart_boundary("multipart/form-data; boundary=\"a\tb\""),
            None,
            "tab is a control character"
        );
        assert_eq!(
            multipart_boundary("multipart/form-data; boundary=\"a\u{0007}b\""),
            None,
            "BEL is a control character"
        );
        assert_eq!(
            multipart_boundary("multipart/form-data; boundary=\"a\u{007f}b\""),
            None,
            "DEL is a control character"
        );
    }

    /// `filename*` (RFC 5987) must not be confused with `filename`.
    #[test]
    fn extended_filename_star_param_does_not_match_filename() {
        assert_eq!(
            disposition_param("form-data; name=\"f\"; filename*=utf-8''x", "filename"),
            None
        );
        assert_eq!(
            disposition_param(
                "form-data; name=\"f\"; filename=\"plain.txt\"; filename*=utf-8''x",
                "filename"
            ),
            Some("plain.txt".to_string())
        );
    }
}
