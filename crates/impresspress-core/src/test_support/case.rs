//! The one sentence-case rule the label guards share: the crawl's
//! (`htmx_guard::every_label_is_in_sentence_case`, every heading, table
//! header, form label, button, summary, stat label, badge and tab a page
//! renders) and the config label guard
//! (`config_vars::shared_vars_tests::every_declared_config_var_is_named_in_sentence_case`).
//! A new surface adds itself to one of them rather than writing the rule
//! again.

/// The proper nouns a label may capitalise after its first word, spelled as
/// their owners spell them: "Enable OAuth", "Connect Stripe". The first word is capitalised
/// anyway, so a label that opens with a name ("GitHub client ID", "Stripe
/// secret key") needs no entry. A noun is added here when a label needs it
/// after its first word, and not before.
const PROPER_NOUNS: &[&str] = &["OAuth", "Stripe"];

/// Whether `label` capitalises its first word and only that, plus any
/// acronym and any of [`PROPER_NOUNS`]: "API keys", "SQL editor", "STUN
/// server URLs" and "Enable OAuth" are; "Audit Logs", "Post-Login redirect",
/// "Max Age (Seconds)" and "api keys" are not.
///
/// Words are split at whitespace and hyphens, so the second half of a
/// hyphenated word is a word of its own, and are read from their first
/// letter, so a parenthesised word is checked like any other. A word after
/// one that ends a sentence (`.`, `!`, `?`) starts the next sentence, so it
/// may be capitalised: "A single price. Best for most products." An acronym is
/// a word with no lowercase letter, or one whose only lowercase letter is a
/// plural `s` ("URLs").
pub fn is_sentence_case(label: &str) -> bool {
    let first_is_capital = label.chars().next().is_some_and(|c| !c.is_lowercase());
    let words: Vec<&str> = label.split(|c: char| c.is_whitespace() || c == '-').collect();
    first_is_capital
        && words.windows(2).all(|pair| {
            let (before, raw) = (pair[0], pair[1]);
            let word = raw.trim_matches(|c: char| !c.is_alphanumeric());
            let starts_upper = word.chars().next().is_some_and(char::is_uppercase);
            // A new sentence starts with a capital too.
            let new_sentence = before.ends_with(['.', '!', '?']);
            !starts_upper || new_sentence || is_acronym(word) || PROPER_NOUNS.contains(&word)
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
        "A single price paid once. Best for most products.",
        "Connect Stripe",
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
        "A single price. Best For most products.",
        "Create a Stripe Customer",
    ] {
        assert!(!is_sentence_case(bad), "{bad}");
    }
}
