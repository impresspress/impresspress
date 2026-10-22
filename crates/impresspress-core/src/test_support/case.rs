//! The one sentence-case rule the label guards share: the tab guard
//! (`htmx_guard::every_tab_is_a_sentence_case_text_label`) and the config
//! label guard (`config_vars::tests::every_declared_config_var_is_named_in_sentence_case`).
//! A later case ratchet adds its surfaces here rather than writing the rule
//! again.

/// The proper nouns a label may capitalise after its first word. Each is a
/// product or company a declared label names ("Enable OAuth", "GitHub client
/// ID", "Stripe secret key"), and is spelled as its owner spells it.
const PROPER_NOUNS: &[&str] = &[
    "GitHub",
    "Google",
    "Mailgun",
    "Microsoft",
    "OAuth",
    "Stripe",
    "Turnstile",
];

/// Whether `label` capitalises its first word and only that, plus any
/// acronym and any of [`PROPER_NOUNS`]: "API keys", "SQL editor", "STUN
/// server URLs" and "Enable OAuth" are; "Audit Logs", "Post-Login redirect",
/// "Max Age (Seconds)" and "api keys" are not.
///
/// Words are split at whitespace and hyphens, so the second half of a
/// hyphenated word is a word of its own, and are read from their first
/// letter, so a parenthesised word is checked like any other. An acronym is
/// a word with no lowercase letter, or one whose only lowercase letter is a
/// plural `s` ("URLs").
pub fn is_sentence_case(label: &str) -> bool {
    let first_is_capital = label.chars().next().is_some_and(|c| !c.is_lowercase());
    first_is_capital
        && label
            .split(|c: char| c.is_whitespace() || c == '-')
            .skip(1)
            .map(|word| word.trim_matches(|c: char| !c.is_alphanumeric()))
            .all(|word| {
                let starts_upper = word.chars().next().is_some_and(char::is_uppercase);
                !starts_upper || is_acronym(word) || PROPER_NOUNS.contains(&word)
            })
}

/// A word with no lowercase letter, or with a plural `s` as its only one.
fn is_acronym(word: &str) -> bool {
    let all_capitals = |w: &str| !w.chars().any(char::is_lowercase);
    all_capitals(word)
        || word
            .strip_suffix('s')
            .is_some_and(|stem| stem.chars().count() >= 2 && all_capitals(stem))
}

#[test]
fn sentence_case_allows_acronyms_and_proper_nouns_and_refuses_a_capitalised_word() {
    for ok in [
        "API keys",
        "SQL editor",
        "Database & config",
        "Explore WASM blocks",
        "Overview",
        "STUN server URLs",
        "Enable OAuth",
        "GitHub client ID",
        "Post-login redirect",
        "Reply-to address",
        "Room TTL (seconds)",
        "Seller application fee (bps)",
    ] {
        assert!(is_sentence_case(ok), "{ok}");
    }
    for bad in [
        "API Keys",
        "Audit Logs",
        "Database & Config",
        "Storage Access",
        "api keys",
        "audit logs",
        "Post-Login redirect",
        "Max age (Seconds)",
        "Session Lifetime (days)",
        "Stripe Secret Key",
        "Allow Signup",
    ] {
        assert!(!is_sentence_case(bad), "{bad}");
    }
}
