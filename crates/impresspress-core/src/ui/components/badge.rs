//! Badge — single source of truth for the small status pill.

use std::borrow::Cow;

use maud::{html, Markup};

/// Declare [`BadgeVariant`], the colour class each variant renders, and the
/// list of every variant, from one `Variant => "class"` table.
///
/// `badge_variants_cover_every_colour_class_in_the_stylesheet` asserts that the
/// variants' classes are exactly the colour classes `badge.css` paints, and
/// that assertion is only ever as good as the list it iterates. A hand-written
/// list beside a hand-written enum can silently disagree with it: add a
/// variant, map it to a class, forget the list, and the parity test keeps
/// comparing the same eleven against the same eleven while the new variant
/// renders a class no stylesheet rule defines. Nothing else would catch that
/// either — the variant class reaches the markup through maud's dynamic
/// `.(expr)` shorthand, which `ui`'s undefined-class guard documents itself as
/// unable to read. Generating the enum, the `class` match and `ALL` together
/// makes completeness a property of the declaration rather than of the author's
/// memory; there is no second list left to omit a variant from.
macro_rules! badge_variants {
    (
        $(#[$enum_meta:meta])*
        enum BadgeVariant {
            $($(#[$variant_meta:meta])* $variant:ident => $class:literal,)+
        }
    ) => {
        $(#[$enum_meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum BadgeVariant {
            $($(#[$variant_meta])* $variant,)+
        }

        impl BadgeVariant {
            /// The colour class this variant renders after the `badge` base
            /// class.
            fn class(self) -> &'static str {
                match self {
                    $(BadgeVariant::$variant => $class,)+
                }
            }

            /// Every variant, in stylesheet order. Test-only: the stylesheet
            /// parity assertion is its sole reader.
            #[cfg(test)]
            const ALL: &'static [BadgeVariant] = &[$(BadgeVariant::$variant,)+];
        }
    };
}

badge_variants! {
    /// Color variant for [`badge`]. Typed so call sites pick a variant by name
    /// rather than passing a class string; [`status_badge`] is the convenience
    /// that derives the variant from a status string.
    enum BadgeVariant {
        Success => "badge-success",
        Primary => "badge-primary",
        Danger => "badge-danger",
        Warning => "badge-warning",
        Info => "badge-info",
        Secondary => "badge-secondary",
        /// The five tone variants below are the shared colour set the block-detail
        /// modal uses for HTTP methods and auth levels. They are named after the
        /// colour rather than after a meaning because two unrelated enums share
        /// them — see the comment above `.badge--tone-brand` in `badge.css`.
        ToneBrand => "badge--tone-brand",
        ToneGreen => "badge--tone-green",
        ToneAmber => "badge--tone-amber",
        ToneRed => "badge--tone-red",
        ToneSlate => "badge--tone-slate",
    }
}

impl BadgeVariant {
    /// Map a free-form status string to a variant. Centralizes the
    /// status→color policy in one place (the only implicit mapping, and it's
    /// presentation, not data translation).
    fn from_status(status: &str) -> Self {
        match status.to_lowercase().as_str() {
            "active" | "enabled" | "completed" | "running" => BadgeVariant::Success,
            "inactive" | "disabled" | "stopped" => BadgeVariant::Danger,
            "pending" | "draft" => BadgeVariant::Warning,
            _ => BadgeVariant::Info,
        }
    }
}

/// A badge that carries more than a colour and a plain text label.
///
/// Call sites reach for this when the pill needs a utility class
/// (`.text-11`, `.mr-1`), a `title`, or markup content (`"v" (version)`).
/// [`badge`] is the plain-text shorthand and delegates here, so within
/// `blocks/admin/` — the area this type was widened for, and whose 39
/// hand-written pills it replaced — there is exactly one place that emits
/// `<span class="badge …">`. Elsewhere in the crate the pill is still written
/// out by hand: `HAND_WRITTEN_BADGES` names the eight files that do it and
/// counts each one's pills, and `blocks/llm/assets/llm-chat.js` builds more in
/// JavaScript, which no Rust-side scan sees at all.
pub struct Badge<'a> {
    variant: BadgeVariant,
    classes: &'a str,
    title: Option<&'a str>,
}

impl<'a> Badge<'a> {
    /// A badge of `variant` with no utility classes and no `title`.
    pub fn new(variant: BadgeVariant) -> Self {
        Badge {
            variant,
            classes: "",
            title: None,
        }
    }

    /// Utility classes appended after the variant class, space-separated in
    /// the order given — the same order the hand-written markup used.
    pub fn classes(mut self, classes: &'a str) -> Self {
        self.classes = classes;
        self
    }

    /// The pill's `title` attribute, emitted after `class`.
    pub fn title(mut self, title: &'a str) -> Self {
        self.title = Some(title);
        self
    }

    /// Render the pill around `content`.
    pub fn render(self, content: Markup) -> Markup {
        // One `class` value built by hand rather than two maud class
        // shorthands: an empty `.("")` would leave a trailing space in the
        // attribute and change the rendered bytes. Borrowed when there are no
        // utility classes — the majority of call sites, several of them once
        // per table row — so the common path renders the same bytes without
        // allocating.
        let class: Cow<'_, str> = if self.classes.is_empty() {
            Cow::Borrowed(self.variant.class())
        } else {
            Cow::Owned(format!("{} {}", self.variant.class(), self.classes))
        };
        html! {
            span .badge .(class) title=[self.title] { (content) }
        }
    }
}

/// Render a colored badge pill for an explicit variant. The plain-text
/// shorthand for [`Badge`]; [`status_badge`] delegates here.
pub fn badge(variant: BadgeVariant, label: &str) -> Markup {
    Badge::new(variant).render(html! { (label) })
}

/// Render a colored status badge, deriving the color from the status string.
pub fn status_badge(status: &str) -> Markup {
    // The variant is derived from the machine value; the label is humanized
    // so snake_case enums (`partially_refunded`) never leak underscores.
    badge(BadgeVariant::from_status(status), &status.replace('_', " "))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::ui::test_support::{css_rules, hand_written_class_shorthand, strip_css_comments};

    /// Every `.badge-…` rule in the stylesheet that paints a `background` — the
    /// convention that separates a colour class from a layout modifier
    /// (`.badge--center` sets only `justify-content`) and from the `.badge`
    /// base rule (which paints nothing).
    ///
    /// Comments are stripped first, and that is load-bearing rather than
    /// defensive: `badge.css` names `.badge-primary` and `.badge--tone-green`
    /// in prose, and a class discussed in a comment defines nothing.
    fn stylesheet_colour_classes() -> BTreeSet<String> {
        let css = strip_css_comments(include_str!("../styles/components/badge.css"));
        let mut found = BTreeSet::new();
        for (selector, body) in css_rules(&css) {
            if !body.contains("background") {
                continue;
            }
            for sel in selector.split(',') {
                let sel = sel.trim();
                if let Some(class) = sel.strip_prefix('.') {
                    if class.starts_with("badge-") {
                        found.insert(class.to_string());
                    }
                }
            }
        }
        found
    }

    #[test]
    fn badge_variants_cover_every_colour_class_in_the_stylesheet() {
        // The reason admin pages hand-wrote 39 badge spans: the type offered
        // four colours where `badge.css` paints eleven, so six of the eight
        // classes admin used could not be named through the component. The
        // parity is derived from the stylesheet rather than a second list, so
        // a colour added to `badge.css` fails here until it has a variant —
        // and, because `badge_variants!` generates `ALL` from the same table
        // as the enum, a variant added without a stylesheet rule fails here
        // too rather than slipping past an incomplete list.
        let defined = stylesheet_colour_classes();
        let variants: BTreeSet<String> = BadgeVariant::ALL
            .iter()
            .map(|v| v.class().to_string())
            .collect();
        assert_eq!(
            defined,
            variants,
            "BadgeVariant and badge.css disagree; \
             in the stylesheet only: {:?}; in the enum only: {:?}",
            defined.difference(&variants).collect::<Vec<_>>(),
            variants.difference(&defined).collect::<Vec<_>>(),
        );
    }

    /// Every file that still writes a badge pill in maud rather than through
    /// this module, with the number of pills it writes. A ratchet: a file that
    /// is not listed must emit none, and a listed file's count must be exact,
    /// so a migration cannot half-land and a new pill written in the syntax
    /// every one of these uses cannot appear unrecorded. Naming the count as well
    /// as the file is what makes it a gate rather than a note — an entry that
    /// only said "this file still has some" would still pass after a tenth was
    /// added.
    ///
    /// The scope is exactly maud's bare `.badge` class shorthand, which is the
    /// form every migrated site and every remaining one is written in. Three
    /// other ways to write the same pill pass at zero, deliberately rather
    /// than by oversight: a static `class="badge …"` attribute (the sibling
    /// guard in `ui/mod.rs` reads that form, and would flag an undefined class
    /// in it), a `class={ "badge" … }` expression, and a bare colour class
    /// with no `.badge` beside it. A ratchet on the one syntax in use is a
    /// gate on the migration; extending it to syntaxes nothing writes would be
    /// speculative. The counter is `test_support::count_bare_class_shorthand`,
    /// shared with `components::table`'s first-generation-table ratchet rather
    /// than copied a second time.
    ///
    /// Comments are excluded, since a comment renders nothing — a doc comment
    /// naming `.badge` in an administration file would otherwise fail this
    /// test with a message that forbids the only edit that would fix it. Test
    /// modules are *not* excluded: markup written in a test is still markup
    /// written by hand, and migrating it should have to lower a count here.
    ///
    /// `blocks/admin/` is absent because this pull request migrated its 39.
    /// The rest are phase 5 §8 candidates and out of scope here.
    const HAND_WRITTEN_BADGES: &[(&str, usize)] = &[
        ("blocks/files/pages_user/buckets.rs", 2),
        ("blocks/legalpages/pages.rs", 12),
        ("blocks/llm/ui.rs", 10),
        ("blocks/messages/pages.rs", 6),
        ("blocks/products/pages.rs", 6),
        ("blocks/tickets/pages.rs", 4),
        ("blocks/vector/pages_ui.rs", 4),
        // A unit-test fixture: the `DetailHero::badges` slot of
        // `templates::detail_page`, exercised by
        // `detail_page_renders_hero_sections_and_meta`. Not a page.
        ("ui/templates.rs", 1),
    ];

    #[test]
    fn only_the_declared_files_still_hand_write_badge_markup() {
        let expected: std::collections::BTreeMap<&str, usize> =
            HAND_WRITTEN_BADGES.iter().copied().collect();
        // Two files are read past rather than counted. This module is where
        // the markup is supposed to be written, and it spells `.badge` out in
        // its own prose and assertions; `ui/test_support.rs` holds the
        // counter and the fixtures that exercise it. Neither renders a page,
        // and string literals are deliberately not masked, so counting either
        // would make this test fail against its own text.
        let found = hand_written_class_shorthand(
            "badge",
            &["ui/components/badge.rs", "ui/test_support.rs"],
        );
        let found_refs: std::collections::BTreeMap<&str, usize> =
            found.iter().map(|(k, v)| (k.as_str(), *v)).collect();
        assert_eq!(
            found_refs, expected,
            "hand-written badge markup moved; update HAND_WRITTEN_BADGES only to \
             remove entries or lower counts"
        );
    }

    #[test]
    fn badge_carries_utility_classes_and_a_title_verbatim() {
        // The second reason admin hand-wrote badges: many of them carry a
        // spacing or type-scale utility class, one carries a `title`, and
        // several hold markup rather than a plain label. The rendered bytes
        // must match what the hand-written maud emitted, attribute order
        // included, or the migration moves a visual baseline.
        let rendered = Badge::new(BadgeVariant::Info)
            .classes("text-xs")
            .title("Database backend")
            .render(html! { "SQLite" " · " (3) " tables" });
        assert_eq!(
            rendered.into_string(),
            r#"<span class="badge badge-info text-xs" title="Database backend">SQLite · 3 tables</span>"#
        );
    }

    #[test]
    fn a_bare_badge_carries_no_trailing_space_and_no_title() {
        // Asserted against the literal bytes, not against `badge`: `badge`
        // delegates to the builder, so comparing the two would pass even if
        // both grew a trailing space in the class attribute. The 39 admin
        // pills this replaced emitted exactly this.
        let expected = r#"<span class="badge badge--tone-slate">http</span>"#;
        assert_eq!(
            Badge::new(BadgeVariant::ToneSlate)
                .render(html! { "http" })
                .into_string(),
            expected
        );
        assert_eq!(
            badge(BadgeVariant::ToneSlate, "http").into_string(),
            expected
        );
    }

    #[test]
    fn badge_renders_variant_class_and_label() {
        let s = badge(BadgeVariant::Success, "Online").into_string();
        assert!(s.contains("badge-success"), "variant class missing: {s}");
        assert!(s.contains(">Online</span>"), "label missing: {s}");
    }

    #[test]
    fn status_badge_delegates_to_badge_with_mapped_variant() {
        // status_badge is the single status-string entry point; it derives a
        // BadgeVariant and renders through the one `badge` function.
        assert!(status_badge("active")
            .into_string()
            .contains("badge-success"));
        assert!(status_badge("disabled")
            .into_string()
            .contains("badge-danger"));
        assert!(status_badge("pending")
            .into_string()
            .contains("badge-warning"));
        // Unknown status falls to the Info variant and keeps the label text.
        let unknown = status_badge("public").into_string();
        assert!(unknown.contains("badge-info"), "default variant: {unknown}");
        assert!(unknown.contains(">public</span>"), "label text: {unknown}");
    }

    #[test]
    fn status_badge_humanizes_snake_case_labels() {
        // Machine enum values must never leak underscores into the UI:
        // `partially_refunded` renders as "partially refunded".
        let partial = status_badge("partially_refunded").into_string();
        assert!(
            partial.contains(">partially refunded</span>"),
            "humanized label: {partial}"
        );
        assert!(
            !partial.contains("partially_refunded"),
            "raw enum: {partial}"
        );
    }
}
