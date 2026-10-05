//! Form controls: the search input, the password field and its reveal toggle.

use maud::{html, Markup};

use crate::ui::icons;

/// Render a search input with htmx-powered search.
/// If `current_value` is non-empty, shows a "Results for X" banner with a clear button.
pub fn search_input(name: &str, placeholder: &str, hx_get: &str, hx_target: &str) -> Markup {
    search_input_with_value(name, placeholder, hx_get, hx_target, "")
}

/// Search input with a pre-filled value and results banner.
///
/// The banner is one wrapping row: the sentence "Results for "…"" is a
/// single item that wraps inside itself, and Clear follows it. It used to be
/// three flex items that each shrank to their own column on a phone
/// ("Results / for" stacked beside the term beside Clear).
pub fn search_input_with_value(
    name: &str,
    placeholder: &str,
    hx_get: &str,
    hx_target: &str,
    current_value: &str,
) -> Markup {
    html! {
        @if !current_value.is_empty() {
            div .search-summary {
                span .search-summary__text {
                    span .text-muted { "Results for " }
                    strong { "\"" (current_value) "\"" }
                }
                a .btn .btn--ghost .btn--sm
                    href=(hx_get)
                    hx-get=(hx_get)
                    hx-target=(hx_target)
                { (icons::x()) " Clear" }
            }
        }
        div .search-input {
            span .search-input-icon { (icons::search()) }
            input .form-input
                type="search"
                name=(name)
                placeholder=(placeholder)
                // A placeholder is not an accessible name -- screen readers
                // may ignore it, and it vanishes once the field has a value.
                // The placeholder text already reads as a label ("Search by
                // email or user ID..."), so it is reused verbatim rather
                // than inventing a second wording to keep in sync.
                aria-label=(placeholder)
                value=(current_value)
                hx-get=(hx_get)
                hx-trigger="input changed delay:300ms, search"
                hx-target=(hx_target)
                autocomplete="off";
        }
    }
}

/// The show/hide button for the password field `target_id`. Place both in a
/// `.value-reveal-wrapper`, input first.
///
/// A toggle button: `label` is its one constant name ("Show password") and
/// `aria-pressed` says whether the value is showing, so the name never has
/// to be swapped. chrome.js's `reveal-toggle` verb flips the field's `type`
/// and `aria-pressed`; CSS shows the eye while masked and the eye-off while
/// shown, keyed off `aria-pressed`, so the icon cannot disagree with the
/// state a screen reader hears.
///
/// While the field is empty the button is hidden (`form.css`): there is
/// nothing to show, and an eye on a blank secret field reads as "reveal the
/// stored value", which the field never holds. That rule keys off
/// `:placeholder-shown`, so the input MUST carry a placeholder.
pub fn reveal_toggle(target_id: &str, label: &str) -> Markup {
    html! {
        button type="button" .reveal-toggle
            aria-label=(label)
            aria-pressed="false"
            data-action="reveal-toggle"
            data-reveal-target=(target_id)
        {
            span .reveal-toggle__show aria-hidden="true" { (icons::eye()) }
            span .reveal-toggle__hide aria-hidden="true" { (icons::eye_off()) }
        }
    }
}

/// What a password field holds, which decides what the browser and password
/// managers do with it.
pub enum PasswordPurpose {
    /// The account's existing password (sign-in, confirming a change):
    /// `autocomplete="current-password"`, so a manager fills it, and no
    /// length rule — an old password predating the policy must still fit.
    Current,
    /// A password being chosen (signup, reset, change, bootstrap, and its
    /// confirmation): `autocomplete="new-password"`, so a manager offers to
    /// generate one, and `minlength` = the length the server enforces
    /// (`auth::helpers::password_min_length`), so the browser stops a short
    /// one before the round trip with the same number the API would refuse it
    /// for.
    New { min_length: usize },
}

/// The password field `id` (also its `name`) with its show/hide toggle
/// ([`reveal_toggle`]: a toggle button whose eye / eye-off icon and
/// `aria-pressed` follow the field). Every password field — the signed-out
/// auth pages and the portal's Security page — is this one control, so
/// `autocomplete`, `minlength` and the toggle cannot differ between them.
///
/// `placeholder` must not be empty: the toggle hides itself while the field
/// is empty by keying off `:placeholder-shown` (see [`reveal_toggle`]).
pub fn password_field(id: &str, placeholder: &str, purpose: PasswordPurpose) -> Markup {
    let (autocomplete, minlength) = match purpose {
        PasswordPurpose::Current => ("current-password", None),
        PasswordPurpose::New { min_length } => ("new-password", Some(min_length)),
    };
    html! {
        div .value-reveal-wrapper {
            input
                type="password"
                class="form-input"
                id=(id)
                name=(id)
                placeholder=(placeholder)
                autocomplete=(autocomplete)
                required
                minlength=[minlength];
            (reveal_toggle(id, "Show password"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_current_password_field_is_filled_by_managers_and_has_no_length_rule() {
        let s = password_field("password", "Enter your password", PasswordPurpose::Current)
            .into_string();
        assert!(s.contains(r#"autocomplete="current-password""#), "{s}");
        assert!(!s.contains("minlength"), "{s}");
        assert!(s.contains(r#"name="password""#), "{s}");
        assert!(s.contains(r#"aria-label="Show password""#), "{s}");
        assert!(s.contains(r#"aria-pressed="false""#), "{s}");
        assert!(s.contains(r#"data-reveal-target="password""#), "{s}");
    }

    #[test]
    fn a_new_password_field_carries_the_enforced_minimum() {
        let s = password_field(
            "newpw",
            "Min 12 characters",
            PasswordPurpose::New { min_length: 12 },
        )
        .into_string();
        assert!(s.contains(r#"autocomplete="new-password""#), "{s}");
        assert!(s.contains(r#"minlength="12""#), "{s}");
    }

    #[test]
    fn reveal_toggle_is_an_unpressed_toggle_button_with_a_constant_name() {
        let s = reveal_toggle("pw", "Show password").into_string();
        assert!(s.contains(r#"type="button""#), "{s}");
        assert!(s.contains(r#"aria-label="Show password""#), "{s}");
        assert!(s.contains(r#"aria-pressed="false""#), "{s}");
        assert!(s.contains(r#"data-reveal-target="pw""#), "{s}");
        // Both icons ship; CSS picks one from aria-pressed.
        assert!(s.contains("reveal-toggle__show") && s.contains("reveal-toggle__hide"));
        assert!(!s.contains("data-reveal-show") && !s.contains("data-reveal-hide"));
    }

    #[test]
    fn search_summary_is_one_text_item_plus_clear() {
        let s = search_input_with_value("q", "Search", "/x", "#t", "bob").into_string();
        assert!(s.contains(r#"<div class="search-summary"><span class="search-summary__text">"#));
        assert!(s.contains("<strong>&quot;bob&quot;</strong>"), "{s}");
        assert!(s.contains("Clear"));
        let none = search_input("q", "Search", "/x", "#t").into_string();
        assert!(!none.contains("search-summary"));
    }
}
