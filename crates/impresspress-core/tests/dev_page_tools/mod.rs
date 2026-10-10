//! The tools the dev page registers itself, read off the `dev.js` the block
//! serves — the one place their names and descriptions are written.
//!
//! Every test that needs the page's own tools reads them here rather than
//! keeping a list: the description gate (`descriptions`, from
//! `dev_tools_manifest.rs` and `openapi_snapshot.rs`) checks what they
//! publish, and `dev_seed_prompts.rs` checks that seed prose names only tools
//! the page has. A tool added to `dev.js` is therefore seen by all of them
//! without an edit.
//!
//! Included with `mod dev_page_tools;` from each test root that needs it.
//! `tests/dev_page_tools/` has no `main.rs`, so cargo does not build it as a
//! test target of its own. Gated on `block-dev`: without it there is no dev
//! block and no `dev.js`.
#![cfg(feature = "block-dev")]

use serde_json::Value;

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
#[derive(Debug, PartialEq)]
enum JsToken {
    Str(String),
    /// A template literal with a `${…}` in it: text computed at run time.
    Interpolated,
    Punct(char),
    Word(String),
}

struct JsLexer<'a> {
    chars: std::iter::Peekable<std::str::Chars<'a>>,
    peeked: Option<JsToken>,
}

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
