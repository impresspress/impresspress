//! Workspace paths: what the sandbox will accept as a file name, where it
//! lives, how big the whole thing may get, and what a file is served as.
//!
//! The workspace has exactly two areas — `site/…` (published verbatim to
//! `wafer-run/web/site`) and `blocks/<name>/…` (compiled into a guest). Every
//! caller-supplied path is validated here before anything else looks at it, so
//! there is one definition of "a workspace path" rather than one per handler.

/// Largest single file the sandbox stores, in bytes.
///
/// The whole body is held in memory twice on the way in (the JSON envelope and
/// the decoded bytes) and again on the way out, on a runtime that may be a
/// browser tab or a Worker isolate. 512 KiB is comfortably above any hand-
/// written source file or page asset and well below the point where that
/// buffering matters.
pub const MAX_FILE_BYTES: usize = 512 * 1024;

/// Largest number of files the workspace may hold.
pub const MAX_FILES: usize = 2_000;

/// Largest total size of the workspace's files, in bytes.
pub const MAX_WORKSPACE_BYTES: u64 = 64 * 1024 * 1024;

/// Largest number of distinct blocks the workspace may define.
///
/// Every block in the workspace is a guest the runtime rebuild has to load,
/// validate and keep resident, so this bounds activation cost, not disk.
pub const MAX_BLOCKS: usize = 16;

/// Longest a single `/`-separated path segment may be, in bytes.
pub const MAX_SEGMENT_BYTES: usize = 255;

/// Longest a whole workspace path may be, in bytes.
pub const MAX_PATH_BYTES: usize = 1024;

/// Suffix the browser storage backend reserves for a file's metadata sidecar.
///
/// **Mirrored in `crates/impresspress-browser/js/bridge.js` as `META_SUFFIX`**
/// (used by `metaName` / `isMetaName`, and refused by `splitKey`). OPFS stores
/// no content type of its own, so the browser backend writes one beside every
/// blob under `<leaf>.__meta__` and hides those names from `list`. A storage
/// key that ends in it would name somebody's sidecar, so `splitKey` throws —
/// and that throw arrives *after* `workspace.json` has been saved, which is
/// why the suffix is refused here instead (see [`validate_path`]).
pub const META_SUFFIX: &str = ".__meta__";

/// The two halves of the workspace a valid path can land in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceArea {
    /// Under `site/` — published as static files by the site publisher.
    Site,
    /// Under `blocks/<name>/` — source for the named block's guest.
    Block(String),
}

/// Why a path was refused. The `Display` text is what the 400 carries, so it
/// has to name the offending part rather than merely restate the rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathError {
    /// The path was the empty string.
    Empty,
    /// The path exceeded [`MAX_PATH_BYTES`].
    TooLong,
    /// A `/`-separated segment was empty, relative (`.` / `..`), too long, or
    /// carried a backslash or a control character.
    BadSegment(String),
    /// The path did not start with `site/` or `blocks/<name>/`, or named an
    /// area root with no file under it.
    OutsideWorkspace,
    /// The path was `blocks/<name>/…` but `<name>` is not a legal block name.
    BadBlockName(String),
    /// A `/`-separated segment ended in [`META_SUFFIX`], which the browser
    /// storage backend reserves for its own metadata sidecars.
    MetadataSidecar(String),
}

impl std::fmt::Display for PathError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(f, "path must not be empty"),
            Self::TooLong => write!(f, "path must be at most {MAX_PATH_BYTES} bytes"),
            Self::BadSegment(segment) => write!(
                f,
                "path segment {segment:?} is not allowed: every segment must be non-empty, \
                 at most {MAX_SEGMENT_BYTES} bytes, and free of `.`, `..`, `\\` and control \
                 characters"
            ),
            Self::OutsideWorkspace => {
                write!(f, "path must name a file under `site/` or `blocks/<name>/`")
            }
            Self::BadBlockName(name) => {
                write!(f, "block name {name:?} is not allowed: {BLOCK_NAME_RULE}")
            }
            Self::MetadataSidecar(segment) => write!(
                f,
                "path segment {segment:?} is not allowed: {META_SUFFIX:?} is reserved for the \
                 storage backend's own metadata sidecars; rename the file"
            ),
        }
    }
}

/// Validate a workspace-relative path and report which area it lands in.
///
/// The rules are deliberately narrow *and* deliberately not narrower: a space
/// is a legitimate character inside a segment (`site/my page.html` is a real
/// page a user may create), so only the shapes that break `/`-splitting or
/// escape the workspace are refused — an empty, `.` or `..` segment, a
/// backslash (which some filesystems and every Windows client treat as a
/// separator), and control characters.
///
/// Nothing here normalizes: a path that says `..` is rejected, never rewritten,
/// because rewriting would store the file somewhere other than the caller
/// asked for. The same reasoning governs `wafer_block::wrap`'s
/// `is_traversal_safe_path`, which refuses the same shape one layer down.
///
/// # The metadata sidecar
///
/// [`META_SUFFIX`] is refused on **every** segment, not merely the last one
/// `bridge.js::splitKey` checks: a *directory* named `page.html.__meta__`
/// lands in the same OPFS directory as the sidecar of a sibling file named
/// `page.html`, so the two would fight over one name.
///
/// Refusing it here rather than leaving it to the storage backend is what
/// makes the failure recoverable. A site path is only handed to `put` at
/// publish time, by which point `workspace.json` has already been saved — so
/// the `TypeError` `splitKey` throws would recur on *every* later site write,
/// with nothing in the message to say that the fix is to delete a file
/// written some time ago.
pub fn validate_path(path: &str) -> Result<WorkspaceArea, PathError> {
    if path.is_empty() {
        return Err(PathError::Empty);
    }
    if path.len() > MAX_PATH_BYTES {
        return Err(PathError::TooLong);
    }
    let segments: Vec<&str> = path.split('/').collect();
    for segment in &segments {
        if segment.is_empty()
            || *segment == "."
            || *segment == ".."
            || segment.len() > MAX_SEGMENT_BYTES
            || segment.contains('\\')
            || segment.chars().any(char::is_control)
        {
            return Err(PathError::BadSegment((*segment).to_string()));
        }
        if segment.ends_with(META_SUFFIX) {
            return Err(PathError::MetadataSidecar((*segment).to_string()));
        }
    }
    match segments.as_slice() {
        ["site", rest @ ..] if !rest.is_empty() => Ok(WorkspaceArea::Site),
        ["blocks", name, rest @ ..] if !rest.is_empty() => {
            if !block_name_is_valid(name) {
                return Err(PathError::BadBlockName((*name).to_string()));
            }
            Ok(WorkspaceArea::Block((*name).to_string()))
        }
        _ => Err(PathError::OutsideWorkspace),
    }
}

/// The rule [`block_name_is_valid`] enforces, in words — stated **once**.
///
/// Two agent-facing diagnostics render it: [`PathError::BadBlockName`] and
/// [`super::validation::name_format_diagnostic`]. They are the only feedback
/// an agent gets when a name is refused, so a stale copy does not merely
/// mislead — it makes the retry loop non-self-correcting, because the agent
/// picks its next name from what the message said was legal. Both messages
/// therefore read the rule from here rather than restating it.
///
/// The underscore is absent from the alphabet on purpose, and the *reason* is
/// on [`block_name_is_valid`] rather than in this string: a message the agent
/// has to act on says what is allowed, not what is not.
pub const BLOCK_NAME_RULE: &str = "it must match `^[a-z][a-z0-9-]{1,31}$` — 2 to 32 characters, \
                                   a lowercase letter followed by lowercase letters, digits and \
                                   hyphens, with no doubled hyphen and no trailing hyphen (a \
                                   block name is also a directory segment, a crate name and half \
                                   of a block id)";

/// Whether `name` is a legal block name: `^[a-z][a-z0-9-]{1,31}$`.
///
/// A block name becomes a directory segment, a crate name and half of a
/// registered block id (`site/{name}`), so it is restricted to the
/// intersection all three accept.
///
/// # Why no underscore
///
/// `wafer_run::runtime::validate_block_name` refuses an underscore in either
/// segment of a block id outright ("underscore not allowed in block segment,
/// use hyphen"), so `site/my_shop` could never be registered. The reason is
/// WRAP: `wafer_block::wrap::resource_prefix` maps `-` to `_` to build a
/// resource prefix from a block id (`site/my-shop` → `site__my_shop__`), and
/// an id that already contained `_` would produce a prefix two different ids
/// could claim. Refusing the character here is what turns that into a
/// diagnostic the agent can act on instead of a registration failure inside
/// a runtime rebuild.
///
/// The block's *resources* use that same prefix — `site__my_shop__notes`,
/// `SITE__MY_SHOP__GREETING` — which `wrap::resource_owner` maps back to
/// `site/my-shop`, the block's own id. The hyphenated spelling
/// (`site__my-shop__notes`) is not a second name for them: the database
/// strips the hyphen from a table name before building SQL, so it would be
/// authorized as `site/my-shop`'s and then used as `site__myshop__notes`.
pub fn block_name_is_valid(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_lowercase() {
        return false;
    }
    let rest: Vec<char> = chars.collect();
    (1..=31).contains(&rest.len())
        && rest
            .iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '-')
        // wafer-run additionally refuses a doubled hyphen and a trailing one;
        // a leading one cannot occur because the first character is checked
        // above. Refusing them here keeps this function the single answer to
        // "can this block be registered?".
        && !name.contains("--")
        && !name.ends_with('-')
}

/// A lower bound on how many bytes `encoded` decodes to, for padded
/// standard base64.
///
/// The one place the arithmetic lives, because two callers enforce two
/// different limits with it: a workspace write against
/// [`MAX_FILE_BYTES`], and a staged artifact against
/// `validation::MAX_ARTIFACT_BYTES`. Both check it *before* decoding, so an
/// over-large body is refused without a second allocation the size of the
/// first.
///
/// It must never over-estimate, or a legal payload would be refused: base64
/// carries three bytes per four characters, of which at most two are padding.
pub fn min_base64_decoded_len(encoded: &str) -> usize {
    (encoded.len() / 4).saturating_mul(3).saturating_sub(2)
}

/// The content type the site publisher serves `path` with, and the one the
/// read endpoint consults to decide utf8 vs base64.
///
/// Deliberately *not* `wafer_core::mime::mime_for_ext_str`, which this
/// otherwise mirrors. Three entries differ, and each difference is the reason
/// this table exists:
///
/// * `rs` and `toml` are absent upstream and would fall through to
///   `application/octet-stream` — which would make the sandbox hand back a
///   block's own Rust source as base64. They are the workspace's most-edited
///   files.
/// * `md` is `text/plain` here rather than `text/markdown`: the sandbox
///   publishes the file, it does not render it.
/// * `json` carries no charset, matching what the site publisher writes into
///   `wafer-run/web/site` and what a generation manifest records.
pub fn content_type_for(path: &str) -> &'static str {
    match extension_of(path).as_str() {
        "html" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "application/javascript; charset=utf-8",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "txt" | "md" | "rs" | "toml" => "text/plain; charset=utf-8",
        "wasm" => "application/wasm",
        "woff2" => "font/woff2",
        _ => UNKNOWN_CONTENT_TYPE,
    }
}

/// What [`content_type_for`] answers when the extension says nothing.
///
/// A fallback, not a claim: it means "this file's type is unknown", which is
/// exactly why [`may_be_text`] treats it differently from a type that really
/// does describe binary content.
pub const UNKNOWN_CONTENT_TYPE: &str = "application/octet-stream";

/// Whether a content type describes text.
///
/// `image/svg+xml` and the `application/*` textual formats are the reason this
/// is not `starts_with("text/")`: they are source a user edits.
pub fn is_textual(content_type: &str) -> bool {
    content_type.starts_with("text/")
        || content_type.starts_with("application/json")
        || content_type.starts_with("application/javascript")
        || content_type.starts_with("image/svg+xml")
}

/// Whether the read endpoint may offer a file as a JSON string rather than
/// base64 — that is, whether its type is text or simply unknown.
///
/// [`UNKNOWN_CONTENT_TYPE`] counts, and that is the point: `.gitignore`,
/// `README`, `LICENSE` and `Dockerfile` all have no extension the table
/// recognizes, and all of them are text a user edits. Offering them as text —
/// and falling back to base64 the moment the bytes turn out not to be valid
/// UTF-8 — is what keeps them editable, while a type that really does describe
/// binary content (`image/png`, `application/wasm`, `font/woff2`) is never
/// offered as text. Nothing about the *stored* content type changes:
/// [`content_type_for`] still answers `application/octet-stream`, which is
/// what the site publisher serves.
pub fn may_be_text(content_type: &str) -> bool {
    is_textual(content_type) || content_type == UNKNOWN_CONTENT_TYPE
}

/// The lowercase extension of `path`'s last segment, or `""` when it has none.
///
/// A leading dot does not start an extension (`.gitignore` has none), matching
/// `std::path::Path::extension`.
fn extension_of(path: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or(path);
    match name.rfind('.') {
        Some(0) | None => String::new(),
        Some(dot) => name[dot + 1..].to_ascii_lowercase(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn site_and_block_files_are_accepted() {
        assert_eq!(validate_path("site/index.html"), Ok(WorkspaceArea::Site));
        assert_eq!(
            validate_path("site/assets/img/logo.png"),
            Ok(WorkspaceArea::Site)
        );
        assert_eq!(
            validate_path("blocks/hello/src/lib.rs"),
            Ok(WorkspaceArea::Block("hello".to_string()))
        );
    }

    /// A space is a normal character in a file name and must survive: the
    /// rules exist to stop `/`-splitting and workspace escape, not to impose
    /// a naming style on the user's pages.
    #[test]
    fn spaces_and_unicode_inside_a_segment_are_fine() {
        assert_eq!(validate_path("site/my page.html"), Ok(WorkspaceArea::Site));
        assert_eq!(
            validate_path("site/héllo wörld.md"),
            Ok(WorkspaceArea::Site)
        );
    }

    #[test]
    fn area_roots_without_a_file_under_them_are_refused() {
        for path in ["site", "site/", "blocks", "blocks/hello", "blocks/hello/"] {
            assert!(
                matches!(
                    validate_path(path),
                    Err(PathError::OutsideWorkspace) | Err(PathError::BadSegment(_))
                ),
                "{path} must not name a file"
            );
        }
    }

    #[test]
    fn traversal_and_separator_tricks_are_refused() {
        assert_eq!(validate_path(""), Err(PathError::Empty));
        for path in [
            "../x",
            "site/../../etc",
            "site//a",
            "/site/a.css",
            "site/./a.css",
            "site/a\\b",
            "site/a\u{0}b",
            "site/a\nb",
        ] {
            assert!(
                matches!(validate_path(path), Err(PathError::BadSegment(_))),
                "{path} must be a bad segment"
            );
        }
        assert_eq!(validate_path("sw.js"), Err(PathError::OutsideWorkspace));
        assert_eq!(
            validate_path("public/index.html"),
            Err(PathError::OutsideWorkspace)
        );
    }

    /// A path naming a metadata sidecar is refused *here*, not by the storage
    /// backend. `bridge.js::splitKey` throws on the same shape, but only at
    /// publish time — after `workspace.json` has been saved, which wedges
    /// every later site write on a `TypeError` from a JS bridge.
    ///
    /// Every segment is checked, not only the leaf: a directory named
    /// `index.html.__meta__` collides with the sidecar of a sibling file
    /// named `index.html`.
    #[test]
    fn metadata_sidecar_names_are_refused_on_every_segment() {
        for (path, segment) in [
            ("site/index.html.__meta__", "index.html.__meta__"),
            ("site/.__meta__", ".__meta__"),
            ("site/index.html.__meta__/a.css", "index.html.__meta__"),
            ("blocks/hello/src/lib.rs.__meta__", "lib.rs.__meta__"),
        ] {
            assert_eq!(
                validate_path(path),
                Err(PathError::MetadataSidecar(segment.to_string())),
                "{path} names a metadata sidecar"
            );
        }
        // The message has to say what to do about it, and name the suffix.
        let refusal = validate_path("site/index.html.__meta__")
            .expect_err("refused")
            .to_string();
        assert!(refusal.contains(META_SUFFIX), "{refusal}");
        assert!(refusal.contains("rename"), "{refusal}");
        // A path that merely *contains* the suffix is fine — only the tail of
        // a segment is what the backend reads as a sidecar.
        assert_eq!(
            validate_path("site/a.__meta__b.html"),
            Ok(WorkspaceArea::Site)
        );
    }

    #[test]
    fn oversized_paths_and_segments_are_refused() {
        let long_segment = "a".repeat(MAX_SEGMENT_BYTES + 1);
        assert_eq!(
            validate_path(&format!("site/{long_segment}")),
            Err(PathError::BadSegment(long_segment))
        );
        // At the limit is fine.
        assert_eq!(
            validate_path(&format!("site/{}", "a".repeat(MAX_SEGMENT_BYTES))),
            Ok(WorkspaceArea::Site)
        );
        // Total length is checked before segments, so a path built of legal
        // segments still trips it.
        let deep = format!("site/{}", vec!["ab"; 400].join("/"));
        assert!(deep.len() > MAX_PATH_BYTES);
        assert_eq!(validate_path(&deep), Err(PathError::TooLong));
    }

    #[test]
    fn the_base64_bound_never_over_estimates() {
        use base64ct::{Base64, Encoding};

        // The bound backs two size limits, and over-estimating would refuse a
        // legal payload — so check it against real encodings across every
        // padding case.
        for len in [0usize, 1, 2, 3, 4, 5, 6, 100, 4 * 1024 * 1024 + 1] {
            let encoded = Base64::encode_string(&vec![0u8; len]);
            let bound = min_base64_decoded_len(&encoded);
            assert!(
                bound <= len,
                "bound {bound} over-estimates {len} bytes ({} chars)",
                encoded.len(),
            );
            // And it is tight enough to be useful: never more than the two
            // padding bytes short.
            assert!(len - bound <= 2, "bound {bound} is loose for {len} bytes");
        }
    }

    /// A hyphenated block name owns its underscore-spelled resources.
    ///
    /// This is the invariant the `cap-collection` / `cap-config` namespaces
    /// rest on: WRAP's own-namespace check calls `resource_owner` on the
    /// resource and compares it to the caller's block id, and the prefix the
    /// sandbox requires is `resource_prefix` of that id. Pinned against the
    /// pinned wafer-block, because a change to either mapping would silently
    /// turn every declared collection into a cross-block reach.
    #[test]
    fn a_hyphenated_block_owns_its_underscore_spelled_resources() {
        assert!(block_name_is_valid("my-shop"));
        assert_eq!(
            wafer_block::wrap::resource_prefix("site/my-shop"),
            "site__my_shop__",
        );
        assert_eq!(
            wafer_block::wrap::resource_owner("site__my_shop__notes"),
            Some("site/my-shop".to_string()),
        );
        assert_eq!(
            wafer_block::wrap::resource_owner("SITE__MY_SHOP__GREETING"),
            Some("site/my-shop".to_string()),
        );
        // And a single-word name, the common case.
        assert_eq!(
            wafer_block::wrap::resource_owner("site__hello__notes"),
            Some("site/hello".to_string()),
        );
    }

    #[test]
    fn block_names_follow_the_declared_pattern() {
        for good in [
            "ab",
            "hello",
            "my-shop",
            "a1",
            &format!("a{}", "b".repeat(31)),
        ] {
            assert!(block_name_is_valid(good), "{good} must be valid");
        }
        for bad in [
            "",
            "a",
            "1abc",
            "_abc",
            "Abc",
            // An underscore can never be registered as half of a block id —
            // wafer-run refuses it, and WRAP's `-`↔`_` mapping is why.
            "my_shop",
            "hello_world",
            // wafer-run refuses these shapes too.
            "my--shop",
            "shop-",
            "ab cd",
            "abc.def",
            &format!("a{}", "b".repeat(32)),
        ] {
            assert!(!block_name_is_valid(bad), "{bad} must be invalid");
        }
    }

    #[test]
    fn a_bad_block_name_is_reported_as_such_not_as_a_bad_segment() {
        assert_eq!(
            validate_path("blocks/Bad Name/src/lib.rs"),
            Err(PathError::BadBlockName("Bad Name".to_string()))
        );
    }

    /// The refusal an agent reads has to describe the alphabet that is
    /// actually enforced. It once taught `_`, which wafer-run refuses in a
    /// block id — so the retry failed for the same reason as the first try.
    #[test]
    fn the_block_name_refusal_teaches_the_rule_that_is_enforced() {
        let refusal = validate_path("blocks/my_shop/src/lib.rs")
            .expect_err("refused")
            .to_string();
        assert!(refusal.contains("my_shop"), "{refusal}");
        assert!(refusal.contains(BLOCK_NAME_RULE), "{refusal}");
        // The rule is rendered from one place, and that place names the
        // hyphen and never offers the underscore.
        assert!(BLOCK_NAME_RULE.contains("[a-z0-9-]"), "{BLOCK_NAME_RULE}");
        assert!(BLOCK_NAME_RULE.contains("hyphen"), "{BLOCK_NAME_RULE}");
        assert!(
            !BLOCK_NAME_RULE.contains('_'),
            "the rule must not offer an underscore: {BLOCK_NAME_RULE}"
        );
    }

    #[test]
    fn content_types_cover_the_workspace_file_kinds() {
        for (path, expected) in [
            ("site/index.html", "text/html; charset=utf-8"),
            ("site/a.css", "text/css; charset=utf-8"),
            ("site/app.js", "application/javascript; charset=utf-8"),
            ("site/app.mjs", "application/javascript; charset=utf-8"),
            ("site/data.json", "application/json"),
            ("site/logo.svg", "image/svg+xml"),
            ("site/dot.png", "image/png"),
            ("site/photo.JPG", "image/jpeg"),
            ("site/photo.jpeg", "image/jpeg"),
            ("site/anim.gif", "image/gif"),
            ("site/pic.webp", "image/webp"),
            ("site/favicon.ico", "image/x-icon"),
            ("site/notes.txt", "text/plain; charset=utf-8"),
            ("site/readme.md", "text/plain; charset=utf-8"),
            ("blocks/hello/src/lib.rs", "text/plain; charset=utf-8"),
            ("blocks/hello/Cargo.toml", "text/plain; charset=utf-8"),
            ("site/mod.wasm", "application/wasm"),
            ("site/font.woff2", "font/woff2"),
            ("site/unknown.xyz", "application/octet-stream"),
            ("site/noext", "application/octet-stream"),
            ("site/.gitignore", "application/octet-stream"),
        ] {
            assert_eq!(content_type_for(path), expected, "{path}");
        }
    }

    /// The read endpoint answers `utf8` exactly when this says so, so a
    /// wrong answer here is a `.rs` file handed back base64-encoded.
    #[test]
    fn textual_types_are_the_ones_read_hands_back_as_utf8() {
        for path in [
            "site/index.html",
            "site/a.css",
            "site/app.js",
            "site/data.json",
            "site/logo.svg",
            "site/notes.txt",
            "blocks/hello/src/lib.rs",
            "blocks/hello/Cargo.toml",
        ] {
            assert!(is_textual(content_type_for(path)), "{path} must be textual");
            assert!(may_be_text(content_type_for(path)));
        }
        for path in ["site/dot.png", "site/font.woff2", "site/mod.wasm"] {
            assert!(
                !is_textual(content_type_for(path)),
                "{path} must not be textual"
            );
            assert!(
                !may_be_text(content_type_for(path)),
                "{path} is known-binary and must never be offered as text"
            );
        }
    }

    /// A file the table cannot classify is not thereby binary. `.gitignore`,
    /// `README` and `LICENSE` are the files a user is most likely to add
    /// without an extension, and all of them are text.
    #[test]
    fn an_unknown_type_may_still_be_text_without_becoming_one() {
        for path in [
            "blocks/hello/.gitignore",
            "blocks/hello/README",
            "site/LICENSE",
            "site/unknown.xyz",
        ] {
            assert_eq!(
                content_type_for(path),
                UNKNOWN_CONTENT_TYPE,
                "{path} stores as octet-stream"
            );
            assert!(
                !is_textual(content_type_for(path)),
                "{path} is not a text media type"
            );
            assert!(
                may_be_text(content_type_for(path)),
                "{path} must still be offered as text when its bytes are UTF-8"
            );
        }
    }
}
