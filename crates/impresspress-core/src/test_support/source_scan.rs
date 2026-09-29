//! One walk over this crate's Rust sources, and one place that decides what
//! counts as a comment.
//!
//! Several gates in this crate are source scans: they assert that a shape
//! nothing in the type system can refuse — a raw table name, an unpaged read,
//! a hand-written error mapping, a config key spelled as a literal — appears
//! only where it is allowed to. Each of them needs the same two things: every
//! `.rs` file under a root, and a view of that file with its prose removed so
//! a gate's own explanation of what it bans is not itself a violation.
//!
//! The walk was written out four times, once per gate, and the comment
//! stripper three times, with independent sets of bugs available to each.
//! This module is the single copy. A gate states its root and its exemptions
//! and gets back [`SourceFile`]s; it picks a comment policy from the two
//! below.
//!
//! ## Why there are two comment policies and not one
//!
//! They are different rules, not two spellings of one rule.
//!
//! [`strip_line_comments`] drops a line that is *entirely* a comment and keeps
//! a trailing comment on a line of code. That is what a gate wants when the
//! thing it bans is a token that could be hidden behind a `//` on the same
//! line as the code using it — the trailing comment stays in the haystack, so
//! nothing hides there.
//!
//! [`code_before_comment`] cuts each line at its first `//`, keeping only the
//! code. That is what a gate wants when it matches a *call*: `foo(` inside a
//! trailing comment is prose about the call, not the call, and a gate that
//! counted it would fail on every line of documentation that names the
//! function it bans.
//!
//! Neither comment policy is a Rust parser. A `//` inside a string literal
//! ends the line for [`code_before_comment`], and a block comment (`/* .. */`)
//! is invisible to both. Both err the same way — a gate matching a token sees
//! at worst one line too few, so it can only under-report, never invent a
//! violation.
//!
//! [`strip_test_modules`] does not get that luxury and so does lex: its
//! mistakes delete production code from the haystack, which is silent. Its doc
//! says why, and which direction is dangerous.

use std::{
    fs,
    path::{Path, PathBuf},
};

/// One `.rs` file the walk reached.
pub struct SourceFile {
    /// Path relative to the walk's root, with `/` separators — what an
    /// allowlist entry and a failure message name.
    pub rel: String,
    /// The full path on disk, for a message that has to be openable.
    pub path: PathBuf,
    /// The file's contents, read once.
    pub text: String,
}

/// Every `.rs` file under a root, minus the directories and file names a gate
/// exempts.
///
/// Built with [`Self::crate_src`] (or [`Self::new`] for a subtree or, in a
/// gate's own self-test, a temporary one), narrowed with [`Self::skip_dir`] /
/// [`Self::skip_file`], and closed with [`Self::least`] — which is what keeps
/// a gate from passing because the walk reached nothing.
pub struct SourceWalk {
    root: PathBuf,
    skip_dirs: Vec<&'static str>,
    skip_files: Vec<&'static str>,
    least: usize,
}

impl SourceWalk {
    /// A walk rooted at `root`.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            skip_dirs: Vec::new(),
            skip_files: Vec::new(),
            least: 0,
        }
    }

    /// A walk over this crate's whole `src/` tree.
    ///
    /// `CARGO_MANIFEST_DIR` is `impresspress-core` both here and in the
    /// `tests/` integration crates, which are separate compilation units of
    /// the same package — so an integration gate and a `#[cfg(test)]` one see
    /// the same tree.
    pub fn crate_src() -> Self {
        Self::new(concat!(env!("CARGO_MANIFEST_DIR"), "/src"))
    }

    /// Skip every directory with this name, at any depth, and everything
    /// under it.
    pub fn skip_dir(mut self, name: &'static str) -> Self {
        self.skip_dirs.push(name);
        self
    }

    /// Skip every file with this name, at any depth.
    pub fn skip_file(mut self, name: &'static str) -> Self {
        self.skip_files.push(name);
        self
    }

    /// Panic unless the walk reaches at least `count` files.
    ///
    /// A source gate that scans nothing passes for exactly the same reason a
    /// clean codebase does. This is the floor that tells the two apart when a
    /// root moves or a filter goes wrong; it is deliberately not set on the
    /// temporary trees a gate's own self-test plants.
    pub fn least(mut self, count: usize) -> Self {
        self.least = count;
        self
    }

    /// Read every file the walk reaches, sorted by [`SourceFile::rel`].
    pub fn collect(&self) -> Vec<SourceFile> {
        let mut out = Vec::new();
        self.visit(&self.root, &mut out);
        out.sort_by(|a, b| a.rel.cmp(&b.rel));
        assert!(
            out.len() >= self.least,
            "the walk over {} reached {} files, fewer than the {} it claims to \
             scan; a gate built on it would pass on an empty result",
            self.root.display(),
            out.len(),
            self.least
        );
        out
    }

    fn visit(&self, dir: &Path, out: &mut Vec<SourceFile>) {
        for entry in fs::read_dir(dir).expect("read source dir") {
            let path = entry.expect("dir entry").path();
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            if path.is_dir() {
                if !self.skip_dirs.contains(&name.as_str()) {
                    self.visit(&path, out);
                }
                continue;
            }
            if path.extension().is_none_or(|ext| ext != "rs")
                || self.skip_files.contains(&name.as_str())
            {
                continue;
            }
            let rel = path
                .strip_prefix(&self.root)
                .expect("under root")
                .to_string_lossy()
                .replace('\\', "/");
            let text = fs::read_to_string(&path).expect("read source file");
            out.push(SourceFile { rel, path, text });
        }
    }
}

/// `src` without its full-line comments (`//`, `///`, `//!`). A trailing
/// comment on a line of code stays, so nothing hides behind a `//` on the
/// same line as the code it describes.
pub fn strip_line_comments(src: &str) -> String {
    src.lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The code part of one line: everything before its first `//`.
pub fn code_before_comment(line: &str) -> &str {
    line.split("//").next().unwrap_or(line)
}

/// `src` with every `#[cfg(test)]` item removed, and the production code
/// around them kept.
///
/// A test asserting on a shape is not a handler producing it, so a gate over
/// production code has to drop the test modules. What it must not do is drop
/// everything *after* the first one: `#[cfg(test)]` is not only the trailing
/// `mod tests`. In seventeen files of this crate the first one sits on a `use`
/// (`blocks/files/mod.rs`), a `pub(in ..) mod test_support;`
/// (`blocks/llm/routes/mod.rs`), a `thread_local!` (`config_generation.rs`), a
/// fixture `const` (`ui/components/badge.rs`) or a `pub(crate)`/`pub`/
/// `pub(super)` fixture fn. A gate that truncated there saw 15 of
/// `blocks/products/mod.rs`'s 422 lines and 621 of
/// `blocks/products/repo/purchases.rs`'s 2203 — 3,466 lines of block code
/// across `src/blocks` that the error door examines now and could not before.
/// (That figure is the gate's own view: `strip_line_comments` over this
/// function's output, minus the same over a truncating one, summed across the
/// 284 files it walks. Counted on the raw text, before comments are dropped,
/// the same difference is 4,851 lines.)
///
/// So the attribute is followed to the end of the item it applies to: a
/// braced item ends when its braces balance, an unbraced one at its `;`. The
/// scan starts at the text *after* the attribute on its own line, not at the
/// next line, so a single-line `#[cfg(test)] mod tests { .. }` ends on that
/// line instead of opening a hunt that eats the production code below it.
///
/// ## Why the braces are lexed and not just counted
///
/// Counting `{` and `}` in the raw text is wrong in two directions, and they
/// are not equally harmful:
///
/// * A stray `}` — `"}"`, `'}'`, or one in a doc comment — balances the item
///   early, so the scan resumes inside the test module and keeps its remaining
///   lines. The gate then sees test code as production and can only report
///   *more* than it should. That is a false failure: loud, and self-correcting.
/// * A stray `{` never balances, so the scan runs to end of file and drops
///   every production line below it. The gate sees *less* than it should and
///   goes quiet — the identical silent blindness this function exists to fix,
///   just arrived at from the other side.
///
/// The second is not hypothetical here: seven files in this crate
/// (`ui/mod.rs`, `ui/assets.rs`, `util.rs`, `kv.rs`, `blocks/dev/page.rs`,
/// `blocks/dev/workspace.rs`, `blocks/llm/routes/chat.rs`) have an unbalanced
/// brace inside a string in their test module. Today each of those modules is
/// the file's last item, so running to end of file happens to lose nothing —
/// the bug is live but currently harmless, which is the worst state to leave
/// it in, because it becomes real the moment someone adds a function below.
///
/// So [`code_braces`] lexes instead: it skips braces inside line and block
/// comments (block comments nest, as they do in Rust), string literals, raw
/// strings of any hash count, and char literals — distinguishing `'{'` from a
/// lifetime. [`tests::no_file_in_this_crate_is_stripped_to_end_of_file`] then
/// holds over the real tree, which is what makes that a checked claim.
pub fn strip_test_modules(src: &str) -> String {
    let (kept, _) = strip_test_modules_reporting(src);
    kept
}

/// [`strip_test_modules`], plus whether any item ran to end of file without
/// its braces balancing — the over-strip direction, which is silent.
fn strip_test_modules_reporting(src: &str) -> (String, bool) {
    let lines: Vec<&str> = src.lines().collect();
    let braces = code_braces(src);
    let mut kept: Vec<&str> = Vec::new();
    let mut ran_off_the_end = false;
    let mut at = 0;
    while at < lines.len() {
        let Some(tail_col) = test_only_attr_tail(lines[at]).map(|t| lines[at].len() - t.len())
        else {
            kept.push(lines[at]);
            at += 1;
            continue;
        };
        // Start on the attribute's OWN line, at the text after it. An attribute
        // alone on its line contributes nothing and the scan moves on; an item
        // written inline is closed by this first segment.
        let mut from = tail_col;
        let mut depth: i32 = 0;
        let mut braced = false;
        let mut closed = false;
        loop {
            for (_, delta) in braces[at].iter().filter(|(col, _)| *col >= from) {
                depth += delta;
                braced |= *delta > 0;
            }
            if braced && depth <= 0 {
                closed = true;
                at += 1;
                break;
            }
            let unbraced_end = !braced && lines[at][from..].trim_end().ends_with(';');
            at += 1;
            if unbraced_end {
                closed = true;
                break;
            }
            if at >= lines.len() {
                break;
            }
            from = 0;
        }
        ran_off_the_end |= !closed && braced;
    }
    (kept.join("\n"), ran_off_the_end)
}

/// Every brace that is really code, as `(byte column, +1 | -1)` per line.
///
/// Braces inside line comments, (nesting) block comments, string literals, raw
/// strings and char literals are not code and are left out. See
/// [`strip_test_modules`] for why counting them instead is a live bug.
fn code_braces(src: &str) -> Vec<Vec<(usize, i32)>> {
    enum Lex {
        Code,
        Line,
        Block(u32),
        Str,
        Raw(usize),
        Ch,
    }

    let b = src.as_bytes();
    let mut out: Vec<Vec<(usize, i32)>> = vec![Vec::new(); src.lines().count().max(1)];
    let (mut i, mut line, mut col) = (0usize, 0usize, 0usize);
    let mut state = Lex::Code;
    // Advance over an escape pair without letting it swallow a newline.
    macro_rules! skip_escape {
        () => {{
            if b.get(i + 1) == Some(&b'\n') {
                line += 1;
                col = 0;
                i += 2;
            } else {
                i += 2;
                col += 2;
            }
            continue;
        }};
    }
    while i < b.len() {
        let c = b[i];
        if c == b'\n' {
            if matches!(state, Lex::Line) {
                state = Lex::Code;
            }
            line += 1;
            col = 0;
            i += 1;
            continue;
        }
        match state {
            Lex::Code => {
                if c == b'/' && b.get(i + 1) == Some(&b'/') {
                    state = Lex::Line;
                } else if c == b'/' && b.get(i + 1) == Some(&b'*') {
                    state = Lex::Block(1);
                } else if c == b'"' {
                    state = Lex::Str;
                } else if c == b'r' {
                    let mut j = i + 1;
                    while b.get(j) == Some(&b'#') {
                        j += 1;
                    }
                    if b.get(j) == Some(&b'"') {
                        state = Lex::Raw(j - i - 1);
                        col += j + 1 - i;
                        i = j + 1;
                        continue;
                    }
                } else if c == b'\'' && is_char_literal(b, i) {
                    state = Lex::Ch;
                } else if c == b'{' || c == b'}' {
                    out[line].push((col, if c == b'{' { 1 } else { -1 }));
                }
                // The two-byte openers above consume their second byte here.
                if matches!(state, Lex::Line | Lex::Block(_)) {
                    i += 2;
                    col += 2;
                    continue;
                }
            }
            Lex::Line => {}
            Lex::Block(d) => {
                if c == b'/' && b.get(i + 1) == Some(&b'*') {
                    state = Lex::Block(d + 1);
                    i += 2;
                    col += 2;
                    continue;
                }
                if c == b'*' && b.get(i + 1) == Some(&b'/') {
                    state = if d == 1 { Lex::Code } else { Lex::Block(d - 1) };
                    i += 2;
                    col += 2;
                    continue;
                }
            }
            Lex::Str => {
                if c == b'\\' {
                    skip_escape!();
                }
                if c == b'"' {
                    state = Lex::Code;
                }
            }
            Lex::Raw(hashes) => {
                if c == b'"' {
                    let mut j = i + 1;
                    let mut seen = 0;
                    while seen < hashes && b.get(j) == Some(&b'#') {
                        j += 1;
                        seen += 1;
                    }
                    if seen == hashes {
                        state = Lex::Code;
                        col += j - i;
                        i = j;
                        continue;
                    }
                }
            }
            Lex::Ch => {
                if c == b'\\' {
                    skip_escape!();
                }
                if c == b'\'' {
                    state = Lex::Code;
                }
            }
        }
        i += 1;
        col += 1;
    }
    out
}

/// Does the `'` at `i` open a char literal rather than a lifetime?
///
/// `'{'` is a brace that must not be counted; `'a` in `&'a str` is not a
/// literal at all. A lifetime never starts with a backslash, and a one-char
/// literal always has its closing quote two bytes along.
fn is_char_literal(b: &[u8], i: usize) -> bool {
    match b.get(i + 1) {
        Some(b'\\') => true,
        Some(_) => b.get(i + 2) == Some(&b'\''),
        None => false,
    }
}

/// The text after a `#[cfg(..)]` on this line, if the attribute's predicate is
/// false whenever `test` is false — i.e. the item it applies to is compiled
/// *only* under `cfg(test)`.
///
/// That is a narrower rule than "a cfg mentioning `test`", deliberately.
/// `#[cfg(all(test, feature = "llm"))]` (`blocks/llm/providers/mod.rs`) is
/// test-only and is stripped. `#[cfg(any(feature = "postgres", test))]` is
/// **not**: those 25 items and the one `#[cfg(any(feature = "block-dev",
/// test))]` compile into the real `--features postgres` / `--features
/// block-dev` builds, which is what CI's "Tests (postgres feature)" job builds.
/// Stripping them would hide 26 pieces of live repo code from every gate here
/// — the blindness this module exists to remove, reintroduced by a matcher
/// that was merely more generous.
fn test_only_attr_tail(line: &str) -> Option<&str> {
    let rest = line.trim_start().strip_prefix("#[cfg(")?;
    let close = matching_paren(rest)?;
    let tail = rest[close + 1..].strip_prefix(']')?;
    is_test_only(&rest[..close]).then_some(tail)
}

/// The index in `s` of the `)` closing an already-open `(`.
fn matching_paren(s: &str) -> Option<usize> {
    let mut depth = 1i32;
    for (i, c) in s.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// Is this `cfg` predicate false whenever `test` is false?
///
/// True for `test` and for an `all(..)` with a test-only operand. Everything
/// else — `any(..)`, `not(..)`, a bare `feature = ".."` — is not, because the
/// item can be compiled without `cfg(test)`.
fn is_test_only(pred: &str) -> bool {
    let pred = pred.trim();
    if pred == "test" {
        return true;
    }
    match pred
        .strip_prefix("all(")
        .and_then(|inner| Some(&inner[..matching_paren(inner)?]))
    {
        Some(inner) => top_level_operands(inner).any(is_test_only),
        None => false,
    }
}

/// Split a predicate list on its depth-zero commas.
fn top_level_operands(inner: &str) -> impl Iterator<Item = &str> {
    let mut depth = 0i32;
    let mut start = 0;
    let mut out = Vec::new();
    for (i, c) in inner.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth == 0 => {
                out.push(&inner[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&inner[start..]);
    out.into_iter()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_line_comments_drops_full_line_comments_and_keeps_trailing_ones() {
        let src = "//! doc\nlet a = 1; // trailing\n/// more\n  // indented\nlet b = 2;\n";
        assert_eq!(
            strip_line_comments(src),
            "let a = 1; // trailing\nlet b = 2;"
        );
    }

    #[test]
    fn code_before_comment_keeps_only_the_code() {
        assert_eq!(code_before_comment("let a = 1; // note"), "let a = 1; ");
        assert_eq!(code_before_comment("// all of it"), "");
        assert_eq!(code_before_comment("let a = 1;"), "let a = 1;");
    }

    #[test]
    fn strip_test_modules_drops_the_module_and_keeps_the_rest() {
        let src = "pub fn f() {}\n\n#[cfg(test)]\nmod tests {\n    fn g() {}\n}\n";
        assert_eq!(strip_test_modules(src), "pub fn f() {}\n");
    }

    /// The whole reason this is not a truncation: production code lives after
    /// a `#[cfg(test)]` item in seventeen files of this crate, and a gate that
    /// stopped at the attribute never saw it.
    #[test]
    fn strip_test_modules_keeps_production_code_after_a_test_item() {
        let src = "#[cfg(test)]\nmod test_support;\n\npub fn handler() {}\n";
        assert_eq!(strip_test_modules(src), "\npub fn handler() {}");

        let src = "#[cfg(test)]\nmod tests {\n    fn g() { let m = maud! { p {} }; }\n}\n\npub fn after() {}\n";
        assert_eq!(strip_test_modules(src), "\npub fn after() {}");

        let src = "#[cfg(test)]\npub(crate) fn fixture() -> u8 {\n    1\n}\n\npub fn after() {}\n";
        assert_eq!(strip_test_modules(src), "\npub fn after() {}");
    }

    /// An item written entirely on the attribute's line ends on that line.
    ///
    /// Latent when this landed — no such line exists in this crate today — but
    /// it is the same failure as the truncating stripper this replaced: the
    /// scan would start on the line *below*, at `depth = 0`, and swallow
    /// production code, to end of file in the `mod` case.
    #[test]
    fn strip_test_modules_closes_an_item_written_on_the_attribute_line() {
        let src = "#[cfg(test)] mod tests { fn g() {} }\n\npub fn after() {}\n";
        assert_eq!(strip_test_modules(src), "\npub fn after() {}");

        let src = "#[cfg(test)] use std::sync::Arc;\n\npub fn after() {}\n";
        assert_eq!(strip_test_modules(src), "\npub fn after() {}");
    }

    /// `all(test, ..)` is test-only and goes; `any(.., test)` is not and stays.
    #[test]
    fn strip_test_modules_keeps_items_that_compile_outside_cfg_test() {
        let src = "#[cfg(all(test, feature = \"llm\"))]\nmod fake_provider {\n    fn f() {}\n}\n\npub fn after() {}\n";
        assert_eq!(strip_test_modules(src), "\npub fn after() {}");

        // Compiled by `--features postgres`, so it is production code and the
        // gates must keep seeing it.
        let src = "#[cfg(any(feature = \"postgres\", test))]\npub fn pg_only() {}\n";
        assert_eq!(strip_test_modules(src), src.trim_end());

        // `not(test)` is production by definition.
        let src = "#[cfg(not(test))]\npub fn real() {}\n";
        assert_eq!(strip_test_modules(src), src.trim_end());
    }

    /// The over-strip direction is silent, so it is measured, not assumed.
    ///
    /// A `{` inside a string literal or block comment would run a scan to end
    /// of file and drop every production line below it — invisible to a gate,
    /// which would simply report less. This fails if that happens anywhere in
    /// this crate.
    #[test]
    fn no_file_in_this_crate_is_stripped_to_end_of_file() {
        let offenders: Vec<String> = SourceWalk::crate_src()
            .least(100)
            .collect()
            .into_iter()
            .filter(|f| strip_test_modules_reporting(&f.text).1)
            .map(|f| f.rel)
            .collect();
        assert!(
            offenders.is_empty(),
            "a `#[cfg(test)]` item ran to end of file with unbalanced braces, \
             so every production line below it is invisible to the gates: {offenders:?}"
        );
    }

    /// The walk descends, honours both exemptions, and reads only Rust.
    #[test]
    fn the_walk_descends_and_honours_its_exemptions() {
        let root = std::env::temp_dir().join(format!("source-walk-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("nested/tests")).expect("temp tree");
        fs::write(root.join("top.rs"), "// top\n").expect("top");
        fs::write(root.join("nested/deep.rs"), "// deep\n").expect("deep");
        fs::write(root.join("nested/tests/fixture.rs"), "// exempt\n").expect("exempt");
        fs::write(root.join("nested/skipped.rs"), "// skipped\n").expect("skipped");
        fs::write(root.join("notes.txt"), "not rust\n").expect("non-rust");

        let found = SourceWalk::new(&root)
            .skip_dir("tests")
            .skip_file("skipped.rs")
            .collect();
        fs::remove_dir_all(&root).expect("clean up");

        let rels: Vec<&str> = found.iter().map(|f| f.rel.as_str()).collect();
        assert_eq!(rels, ["nested/deep.rs", "top.rs"]);
    }

    /// The floor fails rather than reporting an empty scan as clean.
    #[test]
    fn the_floor_refuses_a_walk_that_reached_nothing() {
        let root = std::env::temp_dir().join(format!("source-walk-floor-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("temp tree");
        let walk = SourceWalk::new(&root).least(1);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| walk.collect()));
        fs::remove_dir_all(&root).expect("clean up");
        assert!(
            result.is_err(),
            "a walk that reached no file reported a clean scan"
        );
    }
}
