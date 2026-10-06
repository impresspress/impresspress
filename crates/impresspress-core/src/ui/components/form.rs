//! Form controls: the list search box, the password field and its reveal
//! toggle.

use maud::{html, Markup};

use crate::ui::icons;

/// A list page's search box: searches as the operator types, and the search
/// is part of the page's URL.
///
/// Each settled term (a 300ms pause, or Enter) is a GET of `href` plus
/// `name=<term>` that re-renders the page body (`main#content`) and
/// REPLACES the current history entry's URL with it; Clear does the same.
/// So the address bar always names the search on screen (a reload or a
/// shared link shows that list), while typing adds no history entries: Back
/// leaves the searched page in one step instead of walking back through
/// partial terms. Paging, sort and filter links still push, as navigations.
/// Every other control the server renders on the page — pagination, sort
/// links, filter toggles, the "Results for" summary — is rendered from the
/// same URL as the list, never left carrying the previous term.
///
/// Replacing the box under the cursor costs nothing:
/// - `id` is stable across renders, which is what htmx keys its focus
///   restore on: the new box takes focus and the caret where the old one
///   had them.
/// - The box carries `data-search-input`, and `chrome.js` drops a response
///   for a term the box no longer holds — the operator typed on while it was
///   in flight. Swapping it in would put the shorter term back in the box
///   and eat what was typed; the pending trigger searches the longer one.
///
/// For a screen reader: the box sits in a `search` landmark; after each
/// search `chrome.js` puts the box's `data-search-status` ("12 results for
/// “ali”") into the page's `#search-status` live region, which `ui::layout`
/// renders outside `main#content` so the swap cannot take it away; and
/// after Clear it puts focus back in the box (`data-search-clear` names it),
/// since the Clear link itself is gone.
pub struct SearchInput<'a> {
    /// Unique on the page and the same on every render of it.
    pub id: &'a str,
    /// The query parameter the page reads its term from.
    pub name: &'a str,
    /// The box's accessible name, shown as its placeholder.
    pub label: &'a str,
    /// The list's URL without this search and without a page number, so a
    /// new term starts at page 1 and keeps the page's other parameters.
    pub href: &'a str,
    /// The term the page is showing, as it read it from `name`.
    pub value: &'a str,
    /// How many rows match `value`, across every page of the list.
    pub result_count: u64,
}

impl SearchInput<'_> {
    /// `href` with the current term: the URL of the list on screen, which is
    /// what its pagination links extend, so paging keeps the search.
    pub fn results_href(&self) -> String {
        if self.value.is_empty() {
            return self.href.to_string();
        }
        let join = if self.href.contains('?') { '&' } else { '?' };
        format!(
            "{}{join}{}={}",
            self.href,
            self.name,
            crate::util::urlencode(self.value)
        )
    }

    /// What a search announces once its list is on screen: how many rows it
    /// found, for which term.
    pub fn status(&self) -> String {
        let count = match self.result_count {
            0 => "No results".to_string(),
            1 => "1 result".to_string(),
            n => format!("{n} results"),
        };
        if self.value.is_empty() {
            count
        } else {
            format!("{count} for \u{201c}{}\u{201d}", self.value)
        }
    }

    /// The box, preceded by a "Results for …" summary with a Clear link
    /// while a term is applied.
    ///
    /// The summary is one wrapping row: the sentence "Results for "…"" is a
    /// single item that wraps inside itself, and Clear follows it.
    pub fn render(&self) -> Markup {
        html! {
            @if !self.value.is_empty() {
                div .search-summary {
                    span .search-summary__text {
                        span .text-muted { "Results for " }
                        strong { "\"" (self.value) "\"" }
                    }
                    a .btn .btn--ghost .btn--sm
                        href=(self.href)
                        hx-get=(self.href)
                        hx-target="#content"
                        hx-replace-url="true"
                        data-search-clear=(self.id)
                    { (icons::x()) " Clear" }
                }
            }
            div .search-input role="search" {
                span .search-input-icon { (icons::search()) }
                input .form-input
                    type="search"
                    id=(self.id)
                    name=(self.name)
                    placeholder=(self.label)
                    // A placeholder is not an accessible name -- screen
                    // readers may ignore it, and it vanishes once the field
                    // has a value. The placeholder text already reads as a
                    // label ("Search by email or user ID..."), so it is
                    // reused verbatim rather than inventing a second wording
                    // to keep in sync.
                    aria-label=(self.label)
                    value=(self.value)
                    hx-get=(self.href)
                    hx-trigger="input changed delay:300ms, search"
                    hx-target="#content"
                    hx-replace-url="true"
                    data-search-input
                    data-search-status=(self.status())
                    autocomplete="off";
            }
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

    fn search<'a>(value: &'a str, href: &'a str) -> SearchInput<'a> {
        SearchInput {
            id: "users-search",
            name: "search",
            label: "Search users",
            href,
            value,
            result_count: 3,
        }
    }

    #[test]
    fn search_summary_is_one_text_item_plus_clear() {
        let s = search("bob", "/x").render().into_string();
        assert!(s.contains(r#"<div class="search-summary"><span class="search-summary__text">"#));
        assert!(s.contains("<strong>&quot;bob&quot;</strong>"), "{s}");
        assert!(s.contains("Clear"));
        let none = search("", "/x").render().into_string();
        assert!(!none.contains("search-summary"));
    }

    /// The search is in the URL (reload and links keep it) without a
    /// history entry per term, the box keeps one id across renders (htmx
    /// restores focus and caret by id), and it is marked for chrome.js's
    /// stale-response guard.
    #[test]
    fn the_search_box_replaces_its_url_and_keeps_its_id_across_renders() {
        let s = search("bob", "/b/admin/users").render().into_string();
        let input = &s[s.find("<input").expect("an input")..];
        let input = &input[..input.find('>').expect("a closed tag")];
        for attr in [
            r#"id="users-search""#,
            r#"name="search""#,
            r#"value="bob""#,
            r#"hx-get="/b/admin/users""#,
            r##"hx-target="#content""##,
            r#"hx-replace-url="true""#,
            "data-search-input",
        ] {
            assert!(input.contains(attr), "{attr} missing from {input}");
        }
        let empty = search("", "/b/admin/users").render().into_string();
        assert!(empty.contains(r#"id="users-search""#), "{empty}");
    }

    /// Clear takes the term out of the URL the same way typing put it in:
    /// replacing the entry, so Back still leaves the page in one step.
    #[test]
    fn clear_replaces_the_url_with_the_unsearched_one() {
        let s = search("bob", "/b/admin/logs?tab=audit")
            .render()
            .into_string();
        assert!(
            s.contains(
                r##"href="/b/admin/logs?tab=audit" hx-get="/b/admin/logs?tab=audit" hx-target="#content" hx-replace-url="true""##
            ),
            "{s}"
        );
    }

    /// The box is a search landmark, Clear names the box it returns focus
    /// to, and the box carries what its search announces.
    #[test]
    fn the_search_box_is_a_landmark_and_carries_its_announcement() {
        let s = search("ali", "/b/admin/users").render().into_string();
        assert!(
            s.contains(r#"<div class="search-input" role="search">"#),
            "{s}"
        );
        assert!(s.contains(r#"data-search-clear="users-search""#), "{s}");
        assert!(
            s.contains("data-search-status=\"3 results for \u{201c}ali\u{201d}\""),
            "{s}"
        );
    }

    #[test]
    fn the_status_counts_the_results_for_the_term() {
        let with = |value, result_count| {
            SearchInput {
                result_count,
                ..search(value, "/x")
            }
            .status()
        };
        assert_eq!(with("ali", 12), "12 results for \u{201c}ali\u{201d}");
        assert_eq!(with("ali", 1), "1 result for \u{201c}ali\u{201d}");
        assert_eq!(with("ali", 0), "No results for \u{201c}ali\u{201d}");
        assert_eq!(with("", 40), "40 results");
        assert_eq!(with("", 0), "No results");
    }

    #[test]
    fn results_href_carries_the_encoded_term_after_the_other_parameters() {
        assert_eq!(
            search("", "/b/admin/users").results_href(),
            "/b/admin/users"
        );
        assert_eq!(
            search("a&b c", "/b/admin/users").results_href(),
            "/b/admin/users?search=a%26b+c"
        );
        assert_eq!(
            search("x", "/b/admin/logs?tab=audit").results_href(),
            "/b/admin/logs?tab=audit&search=x"
        );
    }
}
