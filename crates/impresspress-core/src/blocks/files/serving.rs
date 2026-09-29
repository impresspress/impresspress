//! How a user-uploaded object is handed to a browser.
//!
//! Both download paths — the authenticated
//! `GET /b/storage/api/buckets/{bucket}/objects/{key}` and the public share
//! link `GET /b/storage/direct/{token}` — serve bytes a *user* uploaded, under
//! a content type that same user chose, from the application's own origin.
//! That is the classic stored-XSS shape: upload `payload.html` (or an SVG with
//! a `<script>` in it), send someone the link, and the script runs on the
//! app's origin with the victim's session.
//!
//! [`user_object_leading_meta`] is the single answer to that, used by both
//! paths so neither can serve an object on weaker terms than the other:
//!
//! - **An allowlist decides what may render inline.** Only types that cannot
//!   carry script at all — raster images, audio, video, PDF, plain text — get
//!   `Content-Disposition: inline`. Everything else, `text/html` and
//!   `image/svg+xml` included, is an `attachment`: the browser saves it
//!   instead of rendering it, so nothing executes. The allowlist is the fix;
//!   the two headers below are what make it airtight and what cover the
//!   attachment path.
//! - **`X-Content-Type-Options: nosniff`** pins the declared type. Without it
//!   a browser is free to sniff HTML out of a body served as `image/png` and
//!   render it, which would route around the allowlist entirely.
//! - **A sandbox CSP on the attachment path.** If a browser ignored the
//!   disposition and rendered an attachment anyway, `sandbox` puts it in an
//!   opaque origin with scripting off, so it still cannot reach the session
//!   it was aimed at.
//!
//! The inline allowlist deliberately carries no CSP: `sandbox` blocks plugins,
//! and Chrome's PDF viewer is one, so applying it there would trade a working
//! PDF preview for defence-in-depth on a set of types that cannot execute in
//! the first place.
//!
//! Two things reach these headers from an uploader and are therefore shaped
//! here rather than echoed: the content type, which is validated by
//! [`normalized_content_type`] because a header value cannot carry a control
//! character, and the filename, which [`content_disposition`] emits in both
//! RFC 6266 forms because an object key is not restricted to ASCII and a
//! header value is.

use wafer_run::MetaEntry;

/// Response header that stops content-type sniffing. The declared type is the
/// only type the browser may treat the body as.
const NOSNIFF_HEADER: (&str, &str) = ("X-Content-Type-Options", "nosniff");

/// Response header served with every object that is NOT on the inline
/// allowlist: an opaque origin, no scripts, no plugins, no subresources — so a
/// browser that renders an `attachment` anyway renders something inert.
const SANDBOX_CSP_HEADER: (&str, &str) = (
    "Content-Security-Policy",
    "default-src 'none'; sandbox; frame-ancestors 'none'",
);

/// The `Content-Disposition` filename for an object with no usable key.
const FALLBACK_FILENAME: &str = "download";

/// What an object whose content type the block cannot read is served as —
/// including one whose backend reports no type at all, since the empty string
/// is not a media type either. [`normalized_content_type`] is the single place
/// that decides this, for both download paths.
const FALLBACK_CONTENT_TYPE: &str = "application/octet-stream";

/// The stored content type, or [`FALLBACK_CONTENT_TYPE`] when it is not a
/// well-formed media type.
///
/// The type is whatever the uploader's client put in the multipart part
/// header, and [`crate::multipart::extract_multipart_file`] splits those
/// headers on `\r\n` only — so a header terminated with a bare `LF` carries
/// that byte (and anything after it) into the stored value. Echoing it back
/// would put a control character into a response header: both wasm adapters
/// fail closed on that today, which turns into a permanently undownloadable
/// object rather than a header injection, but "this object cannot be fetched,
/// ever" is not an acceptable resting place either.
///
/// So the type is validated before it is echoed, in two parts, because they
/// fail differently:
/// - The **essence** must be `token/token` in RFC 9110 token characters. If it
///   is not, there is nothing left to serve and the answer is
///   [`FALLBACK_CONTENT_TYPE`] — which is not on the inline allowlist, so a
///   type the block could not read can only ever become an attachment.
/// - **Parameters** must be printable ASCII. If they are not, only they are
///   dropped: `image/png; name=café.png` is a perfectly good PNG with an
///   unusable parameter, and discarding the essence too would demote it to an
///   `application/octet-stream` attachment — losing the inline preview over a
///   filename hint no part of the response needs.
fn normalized_content_type(content_type: &str) -> String {
    /// RFC 9110 `tchar`.
    fn is_token(s: &str) -> bool {
        !s.is_empty()
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
    }

    let trimmed = content_type.trim();
    let (essence, params) = match trimmed.split_once(';') {
        Some((essence, params)) => (essence.trim(), Some(params)),
        None => (trimmed, None),
    };

    if !essence
        .split_once('/')
        .is_some_and(|(top, sub)| is_token(top) && is_token(sub))
    {
        return FALLBACK_CONTENT_TYPE.to_string();
    }
    match params {
        Some(p) if !p.chars().all(|c| (' '..='~').contains(&c)) => essence.to_string(),
        _ => trimmed.to_string(),
    }
}

/// Whether an object of `content_type` may be rendered **inline** by the
/// browser.
///
/// The list is exactly the types that cannot carry executable script:
/// - raster images, but **not** `image/svg+xml` (or any other `+xml` image) —
///   an SVG is a document, it can hold `<script>`, and it is the type this
///   allowlist exists to keep out of an inline response;
/// - audio and video, which are decoded, never parsed as a document;
/// - `application/pdf`, so previews keep working — the viewer runs a PDF's own
///   scripting in its own sandbox, never on this origin;
/// - `text/plain`, which paired with `X-Content-Type-Options: nosniff` is
///   displayed as text and can never be sniffed into markup.
///
/// Anything else — `text/html`, `application/xhtml+xml`, `image/svg+xml`,
/// `application/javascript`, an unknown type, or nothing at all — is served as
/// an attachment.
fn renders_inline_safely(content_type: &str) -> bool {
    // A content type may carry parameters (`text/plain; charset=utf-8`); only
    // the type/subtype decides.
    let essence = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();

    if essence == "application/pdf" || essence == "text/plain" {
        return true;
    }
    let Some((top, sub)) = essence.split_once('/') else {
        return false;
    };
    match top {
        // `+xml` is the marker of an XML-based image — SVG today, and whatever
        // else adopts the convention — every one of which is a document that
        // can hold script.
        "image" => !sub.ends_with("+xml") && sub != "xml",
        "audio" | "video" => true,
        _ => false,
    }
}

/// The filename to advertise for `key`: its last path segment, with the
/// characters that would break out of the quoted `filename="…"` form removed.
///
/// A key is validated at upload (`storage::validation::is_valid_storage_key`)
/// and can still contain `/` — it is a path within the bucket — so the last
/// segment is the file, and a key that ends in `/` or is otherwise unusable
/// falls back to [`FALLBACK_FILENAME`] rather than producing an empty
/// `filename=""`.
fn disposition_filename(key: &str) -> String {
    let name: String = key
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .chars()
        .filter(|c| *c != '"' && *c != '\\' && !c.is_control())
        .collect();
    if name.trim().is_empty() {
        FALLBACK_FILENAME.to_string()
    } else {
        name
    }
}

/// The whole `Content-Disposition` value for `key`, per RFC 6266.
///
/// A header value has to be ASCII, and an object key is not: nothing in
/// `is_valid_storage_key` restricts it to ASCII, so `日本語.pdf` and emoji
/// filenames are ordinary uploads. On Cloudflare `Headers.set` throws for a
/// code point above U+00FF, which would turn this header into a 500 on a
/// download that worked before it existed.
///
/// So the value carries both forms RFC 6266 defines, which is exactly what
/// they are for:
/// - `filename="…"` — an ASCII fold, every non-ASCII character replaced with
///   `_`, for a client that reads only this one.
/// - `filename*=UTF-8''…` — the real name, percent-encoded (via
///   [`crate::util::url_path_encode`], whose RFC 3986 unreserved set is a
///   subset of RFC 5987's `attr-char`, so over-encoding is the only
///   difference). Every current browser prefers this one.
///
/// The `filename*` parameter is emitted only when the fold actually lost
/// something; a plain ASCII name keeps the single-parameter form.
fn content_disposition(inline: bool, key: &str) -> String {
    let name = disposition_filename(key);
    let ascii: String = name
        .chars()
        .map(|c| if c.is_ascii() { c } else { '_' })
        .collect();

    let kind = if inline { "inline" } else { "attachment" };
    if ascii == name {
        format!("{kind}; filename=\"{ascii}\"")
    } else {
        format!(
            "{kind}; filename=\"{ascii}\"; filename*=UTF-8''{}",
            crate::util::url_path_encode(&name)
        )
    }
}

/// The leading `Meta` frame for streaming a user-uploaded object: the
/// streaming opt-in marker and content type from
/// [`crate::streaming::download_leading_meta`], plus the disposition and
/// security headers this module's doc explains, plus any `extra_headers` the
/// caller adds (the share path's `Cache-Control`).
///
/// Every response this builds carries `X-Content-Type-Options: nosniff`. The
/// disposition is `inline` only for the types
/// [`renders_inline_safely`] admits; everything else is an `attachment` and
/// additionally carries the sandbox CSP.
///
/// The content type is served as [`normalized_content_type`] leaves it, and
/// the allowlist reads the same normalized value — so a malformed stored type
/// cannot both be echoed into a header and be judged inline.
pub(in crate::blocks::files) fn user_object_leading_meta(
    content_type: &str,
    key: &str,
    extra_headers: &[(&str, &str)],
) -> Vec<MetaEntry> {
    let content_type = normalized_content_type(content_type);
    let inline = renders_inline_safely(&content_type);
    let disposition = content_disposition(inline, key);

    let mut headers: Vec<(&str, &str)> = vec![
        ("Content-Disposition", disposition.as_str()),
        NOSNIFF_HEADER,
    ];
    if !inline {
        headers.push(SANDBOX_CSP_HEADER);
    }
    headers.extend_from_slice(extra_headers);
    crate::streaming::download_leading_meta(&content_type, &headers)
}

#[cfg(test)]
mod tests {
    use wafer_run::MetaGet;

    use super::*;

    fn header<'m>(meta: &'m [MetaEntry], name: &str) -> Option<&'m str> {
        MetaGet::get(meta, &format!("resp.header.{name}"))
    }

    /// The types an uploader can use to get script onto this origin are the
    /// ones that must never come back `inline`.
    #[test]
    fn active_content_types_are_never_inline() {
        for active in [
            "text/html",
            "text/html; charset=utf-8",
            "TEXT/HTML",
            "application/xhtml+xml",
            "image/svg+xml",
            "image/svg+xml; charset=utf-8",
            "application/javascript",
            "text/xml",
            "application/octet-stream",
            "",
            "nonsense",
        ] {
            assert!(
                !renders_inline_safely(active),
                "{active} must not render inline"
            );
        }
    }

    /// Previews keep working for the types that cannot execute.
    #[test]
    fn inert_content_types_still_preview_inline() {
        for inert in [
            "image/png",
            "image/jpeg",
            "IMAGE/PNG",
            "image/webp",
            "application/pdf",
            "text/plain; charset=utf-8",
            "audio/mpeg",
            "video/mp4",
        ] {
            assert!(renders_inline_safely(inert), "{inert} must render inline");
        }
    }

    #[test]
    fn an_active_type_is_an_attachment_with_nosniff_and_a_sandbox_csp() {
        let meta = user_object_leading_meta("text/html", "nested/payload.html", &[]);

        assert_eq!(
            header(&meta, "Content-Disposition"),
            Some("attachment; filename=\"payload.html\"")
        );
        assert_eq!(header(&meta, "X-Content-Type-Options"), Some("nosniff"));
        assert_eq!(
            header(&meta, "Content-Security-Policy"),
            Some(SANDBOX_CSP_HEADER.1)
        );
    }

    #[test]
    fn an_inert_type_is_inline_and_still_carries_nosniff() {
        let meta = user_object_leading_meta("image/png", "pic.png", &[]);

        assert_eq!(
            header(&meta, "Content-Disposition"),
            Some("inline; filename=\"pic.png\"")
        );
        assert_eq!(header(&meta, "X-Content-Type-Options"), Some("nosniff"));
        assert_eq!(
            header(&meta, "Content-Security-Policy"),
            None,
            "the sandbox CSP would cost the PDF/image preview the allowlist exists to keep"
        );
    }

    /// The filename is a header value, so a key cannot close the quoted form
    /// and add parameters of its own, and a key with no usable last segment
    /// still produces a filename.
    #[test]
    fn the_filename_cannot_break_out_of_the_header() {
        assert_eq!(disposition_filename("a/b/c.png"), "c.png");
        assert_eq!(
            disposition_filename("evil\".png"),
            "evil.png",
            "a quote would end the filename parameter"
        );
        assert_eq!(disposition_filename("line\r\nbreak.png"), "linebreak.png");
        assert_eq!(disposition_filename("dir/"), FALLBACK_FILENAME);
        assert_eq!(disposition_filename(""), FALLBACK_FILENAME);
    }

    /// A header value is ASCII, an object key is not, and the authenticated
    /// download had no `Content-Disposition` at all before this module. On
    /// Cloudflare `Headers.set` throws above U+00FF, so an ASCII-only fold
    /// would have turned `日本語.pdf` into a 500 on a download that worked
    /// before. Both RFC 6266 forms go out: the fold for a client that reads
    /// only `filename=`, and the percent-encoded real name for every current
    /// browser.
    #[test]
    fn a_non_ascii_filename_survives_as_rfc_6266_and_the_header_stays_ascii() {
        let meta = user_object_leading_meta("application/pdf", "docs/日本語.pdf", &[]);
        let disposition = header(&meta, "Content-Disposition").expect("a disposition");

        assert_eq!(
            disposition,
            "inline; filename=\"___.pdf\"; filename*=UTF-8''%E6%97%A5%E6%9C%AC%E8%AA%9E.pdf"
        );
        assert!(
            meta.iter().all(|e| e.value.is_ascii()),
            "every header value must be ASCII or the Workers runtime throws: {meta:?}"
        );
    }

    /// A plain ASCII name keeps the single-parameter form — `filename*` is
    /// for the names that need it, not noise on every response.
    #[test]
    fn an_ascii_filename_keeps_the_single_parameter_form() {
        assert_eq!(
            content_disposition(false, "report.bin"),
            "attachment; filename=\"report.bin\""
        );
    }

    /// The stored content type comes from the uploader's multipart part
    /// header, whose headers [`crate::multipart::extract_multipart_file`]
    /// splits on `\r\n` only — a bare `LF` carries into the value. Echoing
    /// that into a response header makes the object permanently undownloadable
    /// on both wasm adapters. An essence that is not a media type is served as
    /// `application/octet-stream`, which is not on the inline allowlist.
    #[test]
    fn a_malformed_content_type_is_replaced_not_echoed() {
        for malformed in [
            "text/html\nX-Injected: 1",
            "text/html\r\nX-Injected: 1",
            "text/html\u{0}",
            "image/png\u{7f}",
            "imagé/png",
            "notatype",
            "/png",
            "image/",
            "",
        ] {
            assert_eq!(
                normalized_content_type(malformed),
                FALLBACK_CONTENT_TYPE,
                "{malformed:?} is not a media type this block will echo"
            );
        }

        let meta = user_object_leading_meta("text/html\nX-Injected: 1", "a.html", &[]);
        assert_eq!(
            wafer_run::MetaGet::get(&meta, wafer_block::meta::META_RESP_CONTENT_TYPE),
            Some(FALLBACK_CONTENT_TYPE),
        );
        assert_eq!(
            header(&meta, "Content-Disposition"),
            Some("attachment; filename=\"a.html\""),
            "a type the block could not read is never inline",
        );
    }

    /// A well-formed type keeps its parameters — normalization is a guard,
    /// not a rewrite.
    #[test]
    fn a_well_formed_content_type_is_served_unchanged() {
        for ok in [
            "text/plain; charset=utf-8",
            "application/pdf",
            "image/svg+xml",
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        ] {
            assert_eq!(normalized_content_type(ok), ok);
        }
        assert_eq!(
            normalized_content_type("  image/png  "),
            "image/png",
            "surrounding whitespace is not part of the type"
        );
    }

    /// A good essence with an unusable parameter loses the parameter, not the
    /// type. `image/png; name=café.png` is a perfectly good PNG; falling back
    /// wholesale would demote it to an `application/octet-stream` attachment
    /// and lose the inline preview over a filename hint nothing in the
    /// response reads — the filename comes from the object key.
    #[test]
    fn a_bad_parameter_costs_the_parameter_not_the_type() {
        assert_eq!(
            normalized_content_type("image/png; name=café.png"),
            "image/png"
        );
        assert_eq!(
            normalized_content_type("text/plain; charset=utf-8\nX-Injected: 1"),
            "text/plain"
        );

        let meta = user_object_leading_meta("image/png; name=café.png", "caf\u{e9}.png", &[]);
        assert_eq!(
            wafer_run::MetaGet::get(&meta, wafer_block::meta::META_RESP_CONTENT_TYPE),
            Some("image/png"),
        );
        assert!(
            header(&meta, "Content-Disposition").is_some_and(|d| d.starts_with("inline;")),
            "the image must still preview inline: {meta:?}"
        );
    }

    /// Caller-supplied headers ride along with the security ones rather than
    /// replacing them.
    #[test]
    fn extra_headers_are_appended() {
        let meta = user_object_leading_meta(
            "image/png",
            "pic.png",
            &[("Cache-Control", "private, max-age=3600")],
        );
        assert_eq!(
            header(&meta, "Cache-Control"),
            Some("private, max-age=3600")
        );
        assert_eq!(header(&meta, "X-Content-Type-Options"), Some("nosniff"));
    }
}
