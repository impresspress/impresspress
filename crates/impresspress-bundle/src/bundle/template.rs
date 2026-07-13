use std::{collections::BTreeMap, path::Path};

use anyhow::{bail, Context, Result};

/// Render a template by substituting `__KEY__` tokens with values from `vars`.
/// Fails if the rendered output still contains any `__…__` token, catching
/// missed placeholders before they reach the browser.
pub fn render_to_file(
    template_src: &Path,
    out: &Path,
    vars: &BTreeMap<String, String>,
) -> Result<()> {
    let body = std::fs::read_to_string(template_src)
        .with_context(|| format!("reading template {}", template_src.display()))?;
    let mut rendered = body;
    for (key, value) in vars {
        let token = format!("__{key}__");
        rendered = rendered.replace(&token, value);
    }
    if let Some(stray) = find_unresolved_placeholder(&rendered) {
        bail!(
            "unresolved placeholder {:?} in {}",
            stray,
            template_src.display()
        );
    }
    std::fs::write(out, rendered).with_context(|| format!("writing {}", out.display()))?;
    Ok(())
}

fn find_unresolved_placeholder(body: &str) -> Option<String> {
    // Look for __WORDCHARS__ — only flag tokens whose inner content is
    // [A-Z0-9][A-Z0-9_]*[A-Z0-9] (or a single [A-Z0-9]) so we don't
    // false-positive on arbitrary __ sequences in minified JS (e.g. __wbg__).
    //
    // Strategy: scan forward looking for `__`. When found, find the matching
    // closing `__` by searching for the next occurrence of `__` that is
    // preceded by a non-underscore. Then check that the inner span is entirely
    // composed of [A-Z0-9_] with no lowercase letters.
    let bytes = body.as_bytes();
    let mut i = 0;
    while i + 4 <= bytes.len() {
        if bytes[i] == b'_' && bytes[i + 1] == b'_' {
            let inner_start = i + 2;
            // Find the next `__` that can serve as a closing delimiter.
            // We scan from inner_start looking for `__`.
            let mut k = inner_start;
            let mut found_close = None;
            while k + 2 <= bytes.len() {
                if bytes[k] == b'_' && bytes[k + 1] == b'_' && k > inner_start {
                    found_close = Some(k);
                    break;
                }
                k += 1;
            }
            if let Some(close) = found_close {
                let inner = &bytes[inner_start..close];
                // Accept only if inner is non-empty and every byte is
                // [A-Z0-9_] with no lowercase letters (rejects wbg_init etc.)
                let is_placeholder = !inner.is_empty()
                    && inner.iter().all(|&b| {
                        b == b'_'
                            || (b as char).is_ascii_uppercase()
                            || (b as char).is_ascii_digit()
                    });
                if is_placeholder {
                    return Some(String::from_utf8_lossy(&bytes[i..close + 2]).into_owned());
                }
                // Skip past the opening __ we already examined.
                i += 2;
                continue;
            }
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(p: &Path, body: &str) {
        std::fs::write(p, body).unwrap();
    }

    #[test]
    fn substitutes_known_placeholders() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("in.tmpl");
        let out = tmp.path().join("out");
        write(
            &src,
            "import x from '__WASM_JS__';\n// build: __BUILD_ID__\n",
        );
        let mut vars = BTreeMap::new();
        vars.insert("WASM_JS".into(), "/impresspress_web-abcd1234.js".into());
        vars.insert("BUILD_ID".into(), "abcd1234".into());
        render_to_file(&src, &out, &vars).unwrap();
        let body = std::fs::read_to_string(&out).unwrap();
        assert_eq!(
            body,
            "import x from '/impresspress_web-abcd1234.js';\n// build: abcd1234\n"
        );
    }

    #[test]
    fn fails_on_unresolved_placeholder() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("in.tmpl");
        let out = tmp.path().join("out");
        write(&src, "x = __MISSING__;");
        let err = render_to_file(&src, &out, &BTreeMap::new()).unwrap_err();
        assert!(err.to_string().contains("__MISSING__"), "got: {err}");
    }

    #[test]
    fn ignores_minified_double_underscores() {
        // Minified JS sometimes has __wbg_foo__ (mixed case) — shouldn't trigger.
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("in.tmpl");
        let out = tmp.path().join("out");
        write(&src, "function __wbg_init__() {}");
        render_to_file(&src, &out, &BTreeMap::new()).unwrap();
    }
}
