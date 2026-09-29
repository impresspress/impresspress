//! Scanning helpers shared by the source- and stylesheet-reading guards in
//! `ui`.
//!
//! Several guards read the same two artefacts. `ui/mod.rs`'s undefined-class
//! guard and `ui/components/badge.rs`'s stylesheet-parity guard both need a
//! `ui/styles/**/*.css` file with its comments removed and its rules split into
//! selector/body pairs; `ui/mod.rs`'s component-class scan and the
//! hand-written-markup ratchets in `badge.rs` and `components/table.rs` both
//! need a `.rs` source with its comments removed, and those two ratchets need
//! the same shorthand counter and the same source-tree walk on top of it. Every
//! one of them lives in a `#[cfg(test)] mod tests`, so none can reach another's
//! private helpers — which is exactly how the second copy of each appeared.
//! This module is the one copy.

use std::collections::{BTreeMap, HashSet};

/// Non-nested `/* ... */` stripper — CSS comments never nest, so this is
/// exact, not a heuristic.
pub(crate) fn strip_css_comments(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '/' && chars.peek() == Some(&'*') {
            chars.next();
            while let Some(c2) = chars.next() {
                if c2 == '*' && chars.peek() == Some(&'/') {
                    chars.next();
                    break;
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

/// Every `(selector, body)` rule in a comment-stripped stylesheet.
///
/// Selector text is everything since the last `{`/`}`/`;` boundary, up to (not
/// including) the rule's `{`; the body is everything up to the *matching* `}`,
/// found by brace counting rather than by splitting on the next `}`. An at-rule
/// whose body itself contains rules (`@media`, `@supports`, `@layer`) is
/// recursed into rather than reported, so the rules nested inside one are
/// returned individually — splitting on braces instead would attribute the
/// first nested rule's body to the at-rule's prelude and lose it. An at-rule
/// with a declaration body and no nested rule (`@font-face`) comes back as an
/// ordinary rule whose selector happens to start with `@`.
pub(crate) fn css_rules(css_no_comments: &str) -> Vec<(String, String)> {
    let chars: Vec<char> = css_no_comments.chars().collect();
    let mut out = Vec::new();
    collect_rules(&chars, &mut out);
    out
}

fn collect_rules(chars: &[char], out: &mut Vec<(String, String)>) {
    let mut last_boundary = 0usize;
    let mut i = 0usize;
    while i < chars.len() {
        match chars[i] {
            '{' => {
                let selector: String = chars[last_boundary..i].iter().collect();
                let mut depth = 1usize;
                let mut j = i + 1;
                while j < chars.len() && depth > 0 {
                    match chars[j] {
                        '{' => depth += 1,
                        '}' => depth -= 1,
                        _ => {}
                    }
                    j += 1;
                }
                // `j` is one past the matching `}` (or the end of input for an
                // unterminated block, which is then treated as the body).
                let body_end = if depth == 0 { j - 1 } else { j };
                let body = &chars[i + 1..body_end];
                if selector.trim_start().starts_with('@') && body.contains(&'{') {
                    collect_rules(body, out);
                } else {
                    out.push((selector, body.iter().collect()));
                }
                i = j;
                last_boundary = i;
            }
            '}' | ';' => {
                i += 1;
                last_boundary = i;
            }
            _ => i += 1,
        }
    }
}

/// Every `.classname` token appearing in `text`. Used on a CSS selector, and
/// it does not need to know the selector's full grammar — comma-separated
/// lists, compound selectors (`.foo.bar`), descendant combinators
/// (`.foo .bar`), pseudo-classes/elements, attribute selectors — extracting
/// every `.ident` substring finds every class in all of them alike.
pub(crate) fn collect_class_tokens(text: &str, out: &mut HashSet<String>) {
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '.'
            && matches!(chars.get(i + 1), Some(c) if c.is_ascii_alphabetic() || *c == '_')
        {
            let start = i + 1;
            let mut j = start;
            while j < chars.len()
                && (chars[j].is_ascii_alphanumeric() || chars[j] == '_' || chars[j] == '-')
            {
                j += 1;
            }
            out.insert(chars[start..j].iter().collect());
            i = j;
            continue;
        }
        i += 1;
    }
}

/// Every class any rule in a comment-stripped stylesheet defines, including
/// rules nested inside `@media`/`@supports` blocks.
pub(crate) fn collect_css_classes(css_no_comments: &str, out: &mut HashSet<String>) {
    for (selector, _body) in css_rules(css_no_comments) {
        collect_class_tokens(&selector, out);
    }
}

/// A copy of `src` with every comment character replaced by a space (newlines
/// kept as newlines, so both char offsets and line numbers still line up with
/// the original), and everything else — string literals included — left alone.
///
/// The counterpart of `scan_delimited_block`'s mask, which blanks string
/// literals too. The scans that use this one read a class list *out of* a
/// string literal (`.classes("text-11 mr-1")`) or count markup that a string
/// can legitimately contain, so they need the literals kept and only the prose
/// removed: a comment that spells a class or a `.badge` shorthand out in
/// running text renders nothing, and must not register as if it did.
///
/// Rust's block comments nest, so the depth is counted rather than closing on
/// the first `*/`. A `'` opens a char literal only when the source looks like
/// `'x'` or `'\n'`; otherwise it is a lifetime (`Badge<'a>`), and mistaking one
/// for the other would swallow the rest of the file.
pub(crate) fn mask_rust_comments(src: &str) -> String {
    enum St {
        Code,
        Str,
        RawStr(usize),
        Char,
        LineComment,
        BlockComment(usize),
    }
    let chars: Vec<char> = src.chars().collect();
    let n = chars.len();
    fn blank(c: char, out: &mut String) {
        out.push(if c == '\n' { '\n' } else { ' ' });
    }
    let mut out = String::with_capacity(src.len());
    let mut state = St::Code;
    let mut i = 0usize;
    while i < n {
        let c = chars[i];
        match state {
            St::LineComment => {
                blank(c, &mut out);
                if c == '\n' {
                    state = St::Code;
                }
                i += 1;
            }
            St::BlockComment(depth) => {
                if c == '/' && chars.get(i + 1) == Some(&'*') {
                    out.push_str("  ");
                    state = St::BlockComment(depth + 1);
                    i += 2;
                    continue;
                }
                if c == '*' && chars.get(i + 1) == Some(&'/') {
                    out.push_str("  ");
                    state = if depth == 1 {
                        St::Code
                    } else {
                        St::BlockComment(depth - 1)
                    };
                    i += 2;
                    continue;
                }
                blank(c, &mut out);
                i += 1;
            }
            St::Str => {
                out.push(c);
                if c == '\\' && i + 1 < n {
                    out.push(chars[i + 1]);
                    i += 2;
                    continue;
                }
                if c == '"' {
                    state = St::Code;
                }
                i += 1;
            }
            St::RawStr(hashes) => {
                if c == '"' {
                    let mut k = i + 1;
                    let mut cnt = 0;
                    while k < n && chars[k] == '#' && cnt < hashes {
                        k += 1;
                        cnt += 1;
                    }
                    if cnt == hashes {
                        out.extend(chars[i..k].iter());
                        i = k;
                        state = St::Code;
                        continue;
                    }
                }
                out.push(c);
                i += 1;
            }
            St::Char => {
                out.push(c);
                if c == '\\' && i + 1 < n {
                    out.push(chars[i + 1]);
                    i += 2;
                    continue;
                }
                if c == '\'' {
                    state = St::Code;
                }
                i += 1;
            }
            St::Code => {
                if c == '/' && chars.get(i + 1) == Some(&'/') {
                    out.push_str("  ");
                    state = St::LineComment;
                    i += 2;
                    continue;
                }
                if c == '/' && chars.get(i + 1) == Some(&'*') {
                    out.push_str("  ");
                    state = St::BlockComment(1);
                    i += 2;
                    continue;
                }
                if c == 'r' {
                    let mut k = i + 1;
                    let mut hashes = 0usize;
                    while k < n && chars[k] == '#' {
                        hashes += 1;
                        k += 1;
                    }
                    if k < n && chars[k] == '"' {
                        out.extend(chars[i..=k].iter());
                        i = k + 1;
                        state = St::RawStr(hashes);
                        continue;
                    }
                }
                if c == '"' {
                    out.push(c);
                    state = St::Str;
                    i += 1;
                    continue;
                }
                if c == '\'' && (chars.get(i + 1) == Some(&'\\') || chars.get(i + 2) == Some(&'\''))
                {
                    out.push(c);
                    state = St::Char;
                    i += 1;
                    continue;
                }
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

/// Count maud's bare `.<class>` class shorthand in `src` — the form
/// `div .card { … }` writes, as opposed to a `class="…"` attribute or a
/// dynamic `.(expr)`. `src` is expected comment-masked
/// (`mask_rust_comments`), since a comment renders nothing.
///
/// A `.` opens a class token only at a maud "new selector position": the very
/// start of the input, or after whitespace, `{`, `}` or `;`. That boundary set
/// is `ui/mod.rs`'s `find_shorthand_classes`, and it is what separates a real
/// shorthand from an ordinary Rust field access or method call
/// (`readiness.reasons`, `.into_iter()`), which is always preceded by an
/// identifier character. **Whitespace alone is not enough**: maud accepts
/// `div{.table` and `};.badge` with no space, so a whitespace-only rule counts
/// markup written that way as zero — a ratchet that reports clean while the
/// markup it guards is being added, and a later "the list is empty, so delete
/// the rules" cleanup would then strip chrome off a live page.
///
/// A match is only counted when the character after the class name does not
/// continue an identifier, so `.badge-success`, `.badge--tone-red` and
/// `.table-container` are excluded by construction: those follow the bare class
/// on the same element (or are a different class entirely), and counting them
/// too would count one element several times.
pub(crate) fn count_bare_class_shorthand(src: &str, class: &str) -> usize {
    let needle = format!(".{class}");
    let bytes = src.as_bytes();
    let mut count = 0;
    for (i, _) in src.match_indices(&needle) {
        let at_boundary = match i.checked_sub(1) {
            None => true,
            Some(p) => {
                let c = bytes[p];
                c.is_ascii_whitespace() || c == b'{' || c == b'}' || c == b';'
            }
        };
        let next = bytes.get(i + needle.len()).copied();
        let continues_class =
            next.is_some_and(|c| c == b'-' || c == b'_' || c.is_ascii_alphanumeric());
        if at_boundary && !continues_class {
            count += 1;
        }
    }
    count
}

/// Every `.rs` file under this crate's `src/` that writes `.{class}` by hand,
/// with the count it writes, keyed by path relative to `src/` with `/`
/// separators. Files named in `skip` are not read at all.
///
/// This is the walk behind both hand-written-markup ratchets. Each ratchet
/// skips the file it lives in: that file spells its own class out in an
/// assertion message and in the doc comment naming the rules to delete, and
/// string literals are deliberately *not* masked, so a ratchet reading its own
/// source would fail against itself the moment somebody reworded the message.
pub(crate) fn hand_written_class_shorthand(class: &str, skip: &[&str]) -> BTreeMap<String, usize> {
    let src_root = concat!(env!("CARGO_MANIFEST_DIR"), "/src");
    let mut found = BTreeMap::new();
    for entry in walkdir::WalkDir::new(src_root)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().is_some_and(|x| x == "rs"))
    {
        let rel = entry
            .path()
            .strip_prefix(src_root)
            .unwrap()
            .to_string_lossy()
            .replace(std::path::MAIN_SEPARATOR, "/");
        if skip.contains(&rel.as_str()) {
            continue;
        }
        let src = mask_rust_comments(&std::fs::read_to_string(entry.path()).unwrap());
        let count = count_bare_class_shorthand(&src, class);
        if count > 0 {
            found.insert(rel, count);
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn css_rules_reads_inside_an_at_rule() {
        // The bug this parser exists to avoid: splitting on `}` attributes the
        // first rule inside a media query to the query's prelude and drops it.
        let rules =
            css_rules("@media (min-width: 40em) { .a { background: red; } .b { color: red; } }");
        assert_eq!(
            rules,
            vec![
                (" .a ".to_string(), " background: red; ".to_string()),
                (" .b ".to_string(), " color: red; ".to_string()),
            ]
        );
    }

    #[test]
    fn css_rules_keeps_a_declaration_only_at_rule_whole() {
        let rules = css_rules("@font-face { src: url(x); }\n.c { background: blue; }");
        assert_eq!(rules.len(), 2);
        assert_eq!(rules[0].0.trim(), "@font-face");
        assert_eq!(rules[1].0.trim(), ".c");
    }

    #[test]
    fn collect_css_classes_sees_a_nested_rule() {
        let mut found = HashSet::new();
        collect_css_classes(
            &strip_css_comments("/* .commented */ @media screen { .nested { color: red; } }"),
            &mut found,
        );
        assert_eq!(found, HashSet::from(["nested".to_string()]));
    }

    #[test]
    fn mask_rust_comments_blanks_prose_and_keeps_literals() {
        let src = "// .classes(\"ghost\")\nlet x = \"real // not-a-comment\"; /* /* nested */ still */ let y = 1;";
        let masked = mask_rust_comments(src);
        assert_eq!(masked.chars().count(), src.chars().count());
        assert!(!masked.contains("ghost"), "comment survived: {masked}");
        assert!(
            masked.contains("\"real // not-a-comment\""),
            "string literal was masked: {masked}"
        );
        assert!(
            !masked.contains("still"),
            "nested comment survived: {masked}"
        );
        assert!(
            masked.contains("let y = 1;"),
            "code after comment lost: {masked}"
        );
    }

    #[test]
    fn count_bare_class_shorthand_reads_every_boundary_maud_accepts() {
        // The spaced form both ratchets were written against, plus the three
        // unspaced boundaries maud also accepts. A whitespace-only rule sees
        // one of these four.
        assert_eq!(count_bare_class_shorthand("div .table { }", "table"), 1);
        assert_eq!(count_bare_class_shorthand("div{.table{}}", "table"), 1);
        assert_eq!(count_bare_class_shorthand("div{}.table{}", "table"), 1);
        assert_eq!(count_bare_class_shorthand("(x);.table{}", "table"), 1);
        assert_eq!(count_bare_class_shorthand(".table { }", "table"), 1);
    }

    #[test]
    fn count_bare_class_shorthand_skips_longer_classes_and_rust_syntax() {
        // A longer class starting with the same name is a different class, and
        // a modifier written beside the bare one would double-count the
        // element that carries both.
        assert_eq!(
            count_bare_class_shorthand("div .table-container { table .table { } }", "table"),
            1
        );
        assert_eq!(
            count_bare_class_shorthand(" .badge .badge--tone-red", "badge"),
            1
        );
        // Field access and method calls are preceded by an identifier
        // character, never by a boundary.
        assert_eq!(
            count_bare_class_shorthand("cfg.table_name; x.table()", "table"),
            0
        );
    }

    #[test]
    fn mask_rust_comments_does_not_mistake_a_lifetime_for_a_char_literal() {
        let src = "struct Badge<'a> { x: &'a str } // gone\nfn f() {}";
        let masked = mask_rust_comments(src);
        assert!(
            !masked.contains("gone"),
            "comment survived a lifetime: {masked}"
        );
        assert!(masked.contains("fn f() {}"));
    }
}
