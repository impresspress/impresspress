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
    CallerFacing {
        scopes: &["dev.tools"],
        fragment: "use `dev_export` to actually download it",
        why: "`dev_export` is a real tool: the dev page registers it itself, since its \
              result is a download rather than a tools.json call",
    },
];

/// Every `description` and `summary` string under `node`, whitespace
/// collapsed: rustdoc wraps sentences, so a phrase can land with a newline
/// in the middle of it.
pub fn published_text(node: &Value) -> Vec<String> {
    fn walk(node: &Value, out: &mut Vec<String>) {
        match node {
            Value::Object(map) => {
                for key in ["description", "summary"] {
                    if let Some(Value::String(text)) = map.get(key) {
                        out.push(text.split_whitespace().collect::<Vec<_>>().join(" "));
                    }
                }
                map.values().for_each(|v| walk(v, out));
            }
            Value::Array(items) => items.iter().for_each(|v| walk(v, out)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    walk(node, &mut out);
    out
}

/// The names a caller can use, read off a published document: property
/// keys, parameter and tool `name`s, enum and const values, and component
/// schema names. A backticked identifier in prose that is none of these is
/// the name of code.
pub fn vocabulary(node: &Value, out: &mut BTreeSet<String>) {
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

/// The leaks in `texts`, published under `scope`, each as `[marker] text`.
/// Records in `used` each [`CALLER_FACING`] entry (by index) that excused a
/// match here, with the scope it excused it in.
pub fn leaks(
    scope: &str,
    texts: &[String],
    vocabulary: &BTreeSet<String>,
    used: &mut BTreeSet<(usize, String)>,
) -> Vec<String> {
    // Each marker names storage or implementation, never caller meaning: a
    // table name prefix, the words for stored tables and columns, SQL, a
    // source or migration file, a Rust path or intra-doc link, Rust's
    // `Option`/`Vec`/`HashMap` spelling of what the wire calls null, array
    // and object, a JSON Schema keyword discussed as a design choice, and
    // the internal surfaces (server-rendered pages, the WebMCP manifest) a
    // field was justified by, and a section of a design document. Configuration keys (`IMPRESSPRESS__LLM__…`)
    // are upper case and are operator-facing, so the table prefix is matched
    // in lower case only.
    let markers = regex::Regex::new(concat!(
        r"\b(?:impresspress|wafer_run|wafer_core)__[a-z]",
        r"|(?i:\blike\s+')|\bIS (?:NOT )?NULL\b",
        r"|(?i:\bcolumns?\b|\btables?\b)",
        r"|\.sql\b|\.rs\b|::|\[`[^`]+`\]",
        r"|`(?:None|Some)\b|`(?:Option|Vec|HashMap)<",
        r"|(?i:\bwriteonly\b|\bwebmcp\b|\bssr\b)|§",
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
                entry.scopes.contains(&scope)
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
pub fn stale(checked: &[&str], used: &BTreeSet<(usize, String)>) -> Vec<String> {
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
