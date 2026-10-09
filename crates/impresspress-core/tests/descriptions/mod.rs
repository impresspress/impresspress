//! The walk behind every "published descriptions carry no maintainer notes"
//! gate.
//!
//! A `///` line on a contract type or field is published: schemars turns it
//! into the schema's `description`, and from there it reaches `/openapi.json`,
//! the generated TypeScript SDK, the WebMCP manifest and the dev sandbox's
//! `tools.json` — text an agent reads to decide what to send. Notes written
//! for the maintainer (which table backs a view, which SQL a filter runs,
//! which function enforces a rule, which file a type lives in) belong in `//`
//! comments beside the code. The markers below are the shapes such notes
//! take; a description carrying one is a leak unless an [`CALLER_FACING`]
//! entry names the exact words and says why a caller needs them.
//!
//! Included with `mod descriptions;` from each test root that publishes a
//! surface (`openapi_snapshot.rs`, `dev_tools_manifest.rs`).
//! `tests/descriptions/` has no `main.rs`, so cargo does not build it as a
//! test target of its own.

use std::collections::BTreeSet;

use serde_json::Value;

/// Wording a marker matches but a caller genuinely needs, by the surfaces it
/// is published on. The scopes are the names the gates check under: a block
/// name for its slice of `/openapi.json`, `webmcp` for the global WebMCP
/// manifest, `dev.tools` for the dev sandbox's `tools.json`.
///
/// An entry excuses a marker only when the marker lies inside an occurrence
/// of `fragment` (whitespace collapsed), and every entry must still excuse
/// something in each of its scopes a run checks — a stale entry is a failure,
/// so rewording the description cannot leave an exemption behind.
pub struct CallerFacing {
    pub scopes: &'static [&'static str],
    pub fragment: &'static str,
    pub why: &'static str,
}

pub const CALLER_FACING: &[CallerFacing] = &[
    // -- admin: the SQL explorer answers about the caller's own query -----
    CallerFacing {
        scopes: &["admin"],
        fragment: "a statement that names a table holding credentials is refused",
        why: "the SQL explorer takes a SQL statement; which tables it refuses is its contract",
    },
    CallerFacing {
        scopes: &["admin"],
        fragment: "The result's column names in the order the query returned them",
        why: "a column of the caller's own query result",
    },
    CallerFacing {
        scopes: &["admin"],
        fragment: "Empty when no row matched: the column list is read off the rows",
        why: "says when the caller's result carries no column names",
    },
    CallerFacing {
        scopes: &["admin"],
        fragment: "The row's `id` column as text",
        why: "the `id` column of the caller's own query result",
    },
    CallerFacing {
        scopes: &["admin"],
        fragment: "Every column of the row, name → value",
        why: "a row of the caller's own query result",
    },
    CallerFacing {
        scopes: &["admin"],
        fragment: "Two result columns with one name collapse into one entry",
        why: "a row of the caller's own query result",
    },
    CallerFacing {
        scopes: &["admin"],
        fragment: "One row of a query result",
        why: "a row of the caller's own query result",
    },
    CallerFacing {
        scopes: &["admin"],
        fragment: "Number of rows.",
        why: "how many rows the caller's own query returned",
    },
    CallerFacing {
        scopes: &["admin"],
        fragment: "The rows, in the order the query returned them",
        why: "the rows of the caller's own query result",
    },
    // -- admin: published values ------------------------------------------
    CallerFacing {
        scopes: &["admin"],
        fragment: "`\"http-handler@v1\"`",
        why: "an example of the interface identifier value the field carries",
    },
    // -- dev: the caller writes, compiles and exports blocks --------------
    CallerFacing {
        scopes: &["dev", "dev.tools"],
        fragment: "`wafer_guest` crate",
        why: "the guest SDK crate every block the caller writes depends on",
    },
    CallerFacing {
        scopes: &["dev"],
        fragment: "src/lib.rs",
        why: "a source file of the block crate the caller writes",
    },
    CallerFacing {
        scopes: &["dev"],
        fragment: "1-based column in `file`",
        why: "a compiler diagnostic's position in the caller's source file",
    },
    CallerFacing {
        scopes: &["dev", "dev.tools"],
        fragment: "a table created in `init`",
        why: "the `table` template's block creates a database table; that is what it scaffolds",
    },
    CallerFacing {
        scopes: &["dev", "dev.tools"],
        fragment: "`table` is a newsletter block with a database table",
        why: "names the `table` template the caller picks",
    },
    CallerFacing {
        scopes: &["dev", "dev.tools"],
        fragment: "Rows the data snapshot carries, per table",
        why: "the export's `tables` map is keyed by the site's data tables",
    },
    CallerFacing {
        scopes: &["dev", "dev.tools"],
        fragment: "the rows of each data table",
        why: "what an export carries of the site's data tables",
    },
];

/// One published surface, checked on its own: its text, and the names a
/// caller of THAT surface can use. A name another block publishes is not
/// vocabulary here — prose in one block that names another block's field is
/// still naming something its own caller cannot see.
pub struct Scope {
    pub name: &'static str,
    pub texts: Vec<String>,
    pub vocabulary: BTreeSet<String>,
    /// Descriptions found on a field — the value of a `properties` entry.
    field_texts: usize,
    /// Descriptions and summaries found anywhere else (operations, tools,
    /// whole schemas).
    other_texts: usize,
}

impl Scope {
    /// `node` is the surface as published. Its caller can also use the names
    /// in `also_reads` (documents the surface is projected from) and
    /// `extra_names` (names no document carries — tools a page script
    /// registers, say).
    pub fn new(
        name: &'static str,
        node: &Value,
        also_reads: &[&Value],
        extra_names: &[String],
    ) -> Self {
        let mut texts = Vec::new();
        let (mut field_texts, mut other_texts) = (0, 0);
        published_text(node, false, &mut texts, &mut field_texts, &mut other_texts);
        let mut names = BTreeSet::new();
        vocabulary(node, &mut names);
        for document in also_reads {
            vocabulary(document, &mut names);
        }
        names.extend(extra_names.iter().cloned());
        Self {
            name,
            texts,
            vocabulary: names,
            field_texts,
            other_texts,
        }
    }
}

/// Every `description` and `summary` string under `node`, whitespace
/// collapsed: rustdoc wraps sentences, so a phrase can land with a newline
/// in the middle of it. `is_field` is whether `node` is the value of a
/// `properties` entry, which is what the floor in [`check`] counts.
fn published_text(
    node: &Value,
    is_field: bool,
    out: &mut Vec<String>,
    field_texts: &mut usize,
    other_texts: &mut usize,
) {
    match node {
        Value::Object(map) => {
            for key in ["description", "summary"] {
                if let Some(Value::String(text)) = map.get(key) {
                    out.push(text.split_whitespace().collect::<Vec<_>>().join(" "));
                    if is_field {
                        *field_texts += 1;
                    } else {
                        *other_texts += 1;
                    }
                }
            }
            for (key, value) in map {
                if key == "properties" {
                    if let Value::Object(fields) = value {
                        for field in fields.values() {
                            published_text(field, true, out, field_texts, other_texts);
                        }
                        continue;
                    }
                }
                published_text(value, false, out, field_texts, other_texts);
            }
        }
        Value::Array(items) => items
            .iter()
            .for_each(|v| published_text(v, false, out, field_texts, other_texts)),
        _ => {}
    }
}

/// Every check, over every scope: the walk's floor, the leaks, and the
/// exemptions that excused nothing.
///
/// The floor is structural rather than a count that rewording could move: a
/// scope must publish descriptions both on its operations or tools and on
/// the fields of their schemas. A walk that stopped at the top level — or
/// never got there — fails it instead of passing over nothing.
pub fn check(scopes: &[Scope]) -> Vec<String> {
    let mut failures = Vec::new();
    let mut used = BTreeSet::new();
    for scope in scopes {
        if scope.field_texts == 0 || scope.other_texts == 0 {
            failures.push(format!(
                "[{}] the walk found {} field and {} other descriptions; a published \
                 surface has both, so the walk is looking in the wrong place and this \
                 gate would pass forever",
                scope.name, scope.field_texts, scope.other_texts
            ));
        }
        failures.extend(leaks(scope, &mut used));
    }
    let checked: Vec<&str> = scopes.iter().map(|scope| scope.name).collect();
    failures.extend(stale(&checked, &used));
    failures
}

/// The names a caller can use, read off a published document: property
/// keys, parameter and tool `name`s, enum and const values, and component
/// schema names. A backticked identifier in prose that is none of these is
/// the name of code.
fn vocabulary(node: &Value, out: &mut BTreeSet<String>) {
    match node {
        Value::Object(map) => {
            for key in ["properties", "schemas", "$defs"] {
                if let Some(Value::Object(named)) = map.get(key) {
                    out.extend(named.keys().cloned());
                }
            }
            if let Some(Value::String(name)) = map.get("name") {
                out.insert(name.clone());
            }
            for key in ["enum", "const"] {
                match map.get(key) {
                    Some(Value::Array(values)) => {
                        out.extend(values.iter().filter_map(|v| v.as_str().map(str::to_string)))
                    }
                    Some(Value::String(value)) => {
                        out.insert(value.clone());
                    }
                    _ => {}
                }
            }
            map.values().for_each(|v| vocabulary(v, out));
        }
        Value::Array(items) => items.iter().for_each(|v| vocabulary(v, out)),
        _ => {}
    }
}

/// The leaks in `scope`, each as `[scope] [marker] text`. Records in `used`
/// each [`CALLER_FACING`] entry (by index) that excused a match here, with
/// the scope it excused it in.
fn leaks(scope: &Scope, used: &mut BTreeSet<(usize, String)>) -> Vec<String> {
    let Scope {
        name: scope,
        texts,
        vocabulary,
        ..
    } = scope;
    // Each marker names storage or implementation, never caller meaning: a
    // table name prefix, the words for stored tables and columns, SQL, a
    // source or migration file, a Rust path or intra-doc link, Rust's
    // `Option`/`Vec`/`HashMap` spelling of what the wire calls null, array
    // and object, a JSON Schema keyword discussed as a design choice, and
    // the internal surfaces (server-rendered pages, the WebMCP manifest) a
    // field was justified by, a section of a design document, the words
    // "handler" and "row" (how code, not a caller, names an endpoint and a
    // record) and SQL aggregates. Configuration keys (`IMPRESSPRESS__LLM__…`)
    // are upper case and are operator-facing, so the table prefix is matched
    // in lower case only.
    let markers = regex::Regex::new(concat!(
        r"\b(?:impresspress|wafer_run|wafer_core)__[a-z]",
        r"|(?i:\blike\s+')|\bIS (?:NOT )?NULL\b",
        r"|(?i:\bcolumns?\b|\btables?\b)",
        r"|\.sql\b|\.rs\b|::|\[`[^`]+`\]",
        r"|`(?:None|Some)\b|`(?:Option|Vec|HashMap)<",
        r"|(?i:\bwriteonly\b|\bwebmcp\b|\bssr\b)|§",
        r"|(?i:\bhandlers?\b|\brows?\b)|\b(?:SUM|COUNT|AVG|MIN|MAX)\(",
    ))
    .expect("marker pattern compiles");
    // A backticked snake_case or CamelCase identifier is published vocabulary
    // or the name of code (`soft_delete`, `RecordListView`).
    let identifier =
        regex::Regex::new(r"`([a-z][a-z0-9]*(?:_[a-z0-9]+)+|[A-Z][a-z0-9]+(?:[A-Z][a-z0-9]*)+)`")
            .expect("identifier pattern compiles");
    let backticked = regex::Regex::new(r"`([^`]+)`").expect("backtick pattern compiles");

    let mut out = Vec::new();
    for text in texts {
        let mut found: Vec<(std::ops::Range<usize>, String)> = markers
            .find_iter(text)
            .map(|m| (m.range(), m.as_str().to_string()))
            .collect();
        for caps in identifier.captures_iter(text) {
            if !vocabulary.contains(&caps[1]) {
                let whole = caps.get(0).expect("whole match");
                found.push((
                    whole.range(),
                    format!("`{}` is not a published name", &caps[1]),
                ));
            }
        }
        // A marker inside a backticked published name (`columns`, `tables`)
        // is that name, not prose about storage.
        let published: Vec<std::ops::Range<usize>> = backticked
            .captures_iter(text)
            .filter(|caps| vocabulary.contains(&caps[1]))
            .map(|caps| caps.get(0).expect("whole match").range())
            .collect();
        found.retain(|(range, _)| {
            !published
                .iter()
                .any(|name| name.start <= range.start && range.end <= name.end)
        });
        for (range, marker) in found {
            let excused = CALLER_FACING.iter().enumerate().find(|(_, entry)| {
                entry.scopes.contains(scope)
                    && text
                        .match_indices(entry.fragment)
                        .any(|(at, fragment)| at <= range.start && range.end <= at + fragment.len())
            });
            match excused {
                Some((index, _)) => {
                    used.insert((index, scope.to_string()));
                }
                None => out.push(format!("[{scope}] [{marker}] {text}")),
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// The [`CALLER_FACING`] entries that excused nothing in one of their scopes
/// this run checked: that wording is gone, so the exemption must go too.
fn stale(checked: &[&str], used: &BTreeSet<(usize, String)>) -> Vec<String> {
    let mut out = Vec::new();
    for (index, entry) in CALLER_FACING.iter().enumerate() {
        for scope in entry.scopes.iter().filter(|s| checked.contains(s)) {
            if !used.contains(&(index, scope.to_string())) {
                out.push(format!(
                    "CALLER_FACING entry {:?} ({}) excused nothing in `{scope}`",
                    entry.fragment, entry.why
                ));
            }
        }
    }
    out
}

/// The tools the dev page registers itself (`registerPageTool({ … })` in
/// `dev.js`), read off the script the block serves, as `{"tools": [ … ]}`.
///
/// Their definitions are JavaScript object literals rather than a document
/// any endpoint serves, so they are read here with [`js_literal`]: every
/// key of each definition except its `execute` function, with every value a
/// literal. A description built at run time (a variable, a call) is refused
/// rather than skipped, and so is a `registerPageTool` call given anything
/// but a literal, except the one forwarding loop whose tools come from
/// `tools.json` (checked as `dev.tools`). So no description the page
/// publishes can sit outside the gate.
#[cfg(feature = "block-dev")]
pub fn dev_page_tools() -> Value {
    let js = impresspress_core::blocks::dev::assets::dev_js();
    const CALL: &str = "registerPageTool(";
    let mut tools = Vec::new();
    // The two places the name is followed by something other than a literal:
    // the function's own definition, and the loop that forwards each
    // `tools.json` tool (whose descriptions `dev.tools` checks) to it.
    let mut forwarded = Vec::new();
    for (at, _) in js.match_indices(CALL) {
        let rest = js[at + CALL.len()..].trim_start();
        if rest.starts_with('{') {
            tools.push(js_literal(&mut JsLexer::new(rest)));
            continue;
        }
        let argument = &rest[..rest.find(')').expect("a closed argument list")];
        let site = if js[..at].ends_with("function ") {
            "definition"
        } else {
            "call"
        };
        forwarded.push(format!("{site}({argument})"));
    }
    // Any other non-literal argument is a tool whose definition this reader
    // cannot see — so it is refused, not skipped. A new one is either written
    // as a literal or added here with the reason it is not a page tool.
    assert_eq!(
        forwarded,
        ["definition(options)", "call(options)"],
        "dev.js passes `registerPageTool` something other than an object literal; the \
         description gate can only read a literal definition"
    );
    let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
    assert!(
        names.contains(&"dev_export") && names.contains(&"dev_compile_block"),
        "dev.js registers `dev_compile_block` and `dev_export` itself; the reader found {names:?}"
    );
    serde_json::json!({ "tools": tools })
}

/// The names of [`dev_page_tools`]. They are published to the agent beside
/// `tools.json`'s, so prose may name them.
#[cfg(feature = "block-dev")]
pub fn dev_page_tool_names() -> Vec<String> {
    dev_page_tools()["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .map(|tool| {
            tool["name"]
                .as_str()
                .expect("every page tool has a name")
                .to_string()
        })
        .collect()
}

/// A token of the JavaScript the page tools are written in. Only as much of
/// the language as an object literal and the function bodies it holds need:
/// strings, punctuation, and runs of identifier characters. Comments and
/// whitespace are dropped.
#[cfg(feature = "block-dev")]
#[derive(Debug, PartialEq)]
enum JsToken {
    Str(String),
    /// A template literal with a `${…}` in it: text computed at run time.
    Interpolated,
    Punct(char),
    Word(String),
}

#[cfg(feature = "block-dev")]
struct JsLexer<'a> {
    chars: std::iter::Peekable<std::str::Chars<'a>>,
    peeked: Option<JsToken>,
}

#[cfg(feature = "block-dev")]
impl<'a> JsLexer<'a> {
    fn new(src: &'a str) -> Self {
        Self {
            chars: src.chars().peekable(),
            peeked: None,
        }
    }

    fn peek(&mut self) -> &JsToken {
        if self.peeked.is_none() {
            self.peeked = Some(self.lex());
        }
        self.peeked.as_ref().expect("just filled")
    }

    fn next(&mut self) -> JsToken {
        match self.peeked.take() {
            Some(token) => token,
            None => self.lex(),
        }
    }

    fn lex(&mut self) -> JsToken {
        loop {
            let c = self
                .chars
                .next()
                .expect("dev.js ended inside a page tool definition");
            match c {
                c if c.is_whitespace() => continue,
                '/' if self.chars.peek() == Some(&'/') => {
                    for c in self.chars.by_ref() {
                        if c == '\n' {
                            break;
                        }
                    }
                }
                '/' if self.chars.peek() == Some(&'*') => {
                    self.chars.next();
                    let mut last = ' ';
                    for c in self.chars.by_ref() {
                        if last == '*' && c == '/' {
                            break;
                        }
                        last = c;
                    }
                }
                '\'' | '"' | '`' => return self.string(c),
                c if c.is_alphanumeric() || matches!(c, '_' | '$' | '.') => {
                    let mut word = c.to_string();
                    while let Some(&c) = self.chars.peek() {
                        if !(c.is_alphanumeric() || matches!(c, '_' | '$' | '.')) {
                            break;
                        }
                        word.push(c);
                        self.chars.next();
                    }
                    return JsToken::Word(word);
                }
                c => return JsToken::Punct(c),
            }
        }
    }

    /// The rest of a string literal opened by `quote`, unescaped. A
    /// backslash before a newline is a line continuation and adds nothing.
    fn string(&mut self, quote: char) -> JsToken {
        let mut out = String::new();
        let mut interpolated = false;
        loop {
            match self
                .chars
                .next()
                .expect("unterminated string literal in dev.js")
            {
                c if c == quote => {
                    return match interpolated {
                        true => JsToken::Interpolated,
                        false => JsToken::Str(out),
                    }
                }
                '$' if quote == '`' && self.chars.peek() == Some(&'{') => {
                    // Past the expression. Only a function body can hold
                    // one ([`js_literal`] refuses it), so its text is not
                    // needed, only its end.
                    interpolated = true;
                    let mut depth = 0;
                    for c in self.chars.by_ref() {
                        match c {
                            '{' => depth += 1,
                            '}' if depth == 1 => break,
                            '}' => depth -= 1,
                            _ => {}
                        }
                    }
                }
                '\\' => match self.chars.next().expect("escape at the end of dev.js") {
                    '\n' => {}
                    'n' => out.push('\n'),
                    't' => out.push('\t'),
                    c @ ('\\' | '\'' | '"' | '`' | '/') => out.push(c),
                    c => panic!("unhandled escape `\\{c}` in a page tool definition"),
                },
                c => out.push(c),
            }
        }
    }
}

/// One JavaScript literal — object, array, string (`+`-concatenated
/// strings included), number, `true`, `false` or `null` — as JSON.
///
/// A tool's `execute` is code, not something the tool publishes, so it is
/// skipped whole: a function expression (`execute: async function …`) with
/// its body, or the name of a function defined elsewhere. Any other value
/// that is not a literal panics, `execute`'s included (an arrow function,
/// say), with a message that says which.
#[cfg(feature = "block-dev")]
fn js_literal(lexer: &mut JsLexer<'_>) -> Value {
    match lexer.next() {
        JsToken::Punct('{') => {
            let mut map = serde_json::Map::new();
            loop {
                let key = match lexer.next() {
                    JsToken::Punct('}') => return Value::Object(map),
                    JsToken::Word(key) | JsToken::Str(key) => key,
                    other => {
                        panic!("expected an object key in a page tool definition, got {other:?}")
                    }
                };
                assert_eq!(lexer.next(), JsToken::Punct(':'), "after key `{key}`");
                if key == "execute" {
                    match lexer.peek() {
                        JsToken::Word(w) if w == "async" || w == "function" => skip_function(lexer),
                        JsToken::Word(_) => {
                            lexer.next();
                        }
                        other => panic!(
                            "a page tool's `execute` is neither a function expression nor a \
                             function's name ({other:?}); teach this reader its shape"
                        ),
                    }
                } else {
                    map.insert(key, js_literal(lexer));
                }
                match lexer.next() {
                    JsToken::Punct(',') => {}
                    JsToken::Punct('}') => return Value::Object(map),
                    other => {
                        panic!("expected `,` or `}}` in a page tool definition, got {other:?}")
                    }
                }
            }
        }
        JsToken::Punct('[') => {
            let mut items = Vec::new();
            loop {
                if lexer.peek() == &JsToken::Punct(']') {
                    lexer.next();
                    return Value::Array(items);
                }
                items.push(js_literal(lexer));
                match lexer.next() {
                    JsToken::Punct(',') => {}
                    JsToken::Punct(']') => return Value::Array(items),
                    other => panic!("expected `,` or `]` in a page tool definition, got {other:?}"),
                }
            }
        }
        JsToken::Str(mut text) => {
            while lexer.peek() == &JsToken::Punct('+') {
                lexer.next();
                match lexer.next() {
                    JsToken::Str(more) => text.push_str(&more),
                    other => panic!(
                        "a page tool definition concatenates a non-literal onto a string: {other:?}"
                    ),
                }
            }
            Value::String(text)
        }
        JsToken::Word(word) => match word.as_str() {
            "true" => Value::Bool(true),
            "false" => Value::Bool(false),
            "null" => Value::Null,
            number => serde_json::from_str(number).unwrap_or_else(|_| {
                panic!(
                    "`{number}` in a page tool definition is not a literal; a value built at \
                     run time would publish text no gate reads"
                )
            }),
        },
        other => panic!("expected a literal in a page tool definition, got {other:?}"),
    }
}

/// Past a function expression: its header, then its body up to the brace
/// that closes it.
#[cfg(feature = "block-dev")]
fn skip_function(lexer: &mut JsLexer<'_>) {
    while lexer.next() != JsToken::Punct('{') {}
    let mut depth = 1;
    while depth > 0 {
        match lexer.next() {
            JsToken::Punct('{') => depth += 1,
            JsToken::Punct('}') => depth -= 1,
            _ => {}
        }
    }
}
