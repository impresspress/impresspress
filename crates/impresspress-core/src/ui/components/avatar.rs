//! Avatar (Phase 1)

/// An [`avatar`]'s size.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub enum AvatarSize {
    Sm,
    Md,
    Lg,
}

/// Avatar — flat brand-orange circle with the seed's first character. The
/// initial varies per user, but the background does not: a single brand
/// color across every avatar is calmer to scan than a wall of per-hash
/// hues, and the colored variants previously made the admin lists feel
/// like distinct personas instead of rows of the same shape.
///
/// Decorative (`aria-hidden`): an avatar always sits beside the name it is
/// the initial of, so a screen reader announcing it would read that
/// initial before the name ("A, admin@example.com").
pub fn avatar(seed: &str, size: AvatarSize) -> maud::Markup {
    use maud::html;
    let initial = seed.chars().next().unwrap_or('?').to_ascii_uppercase();
    let size_class = match size {
        AvatarSize::Sm => "avatar--sm",
        AvatarSize::Md => "avatar--md",
        AvatarSize::Lg => "avatar--lg",
    };
    html! {
        span class={ "avatar " (size_class) } aria-hidden="true" { (initial) }
    }
}

/// A person as a list of accounts shows them: the avatar beside the email,
/// one `span.user-identity` (styled in `table.css`). The admin dashboard's
/// "Recent users" and the Users table both render an account this way.
///
/// The email is a [`breakable_id`](super::breakable_id), and the CSS lets an
/// address with no break opportunity wrap too, so a long one wraps beside
/// its avatar rather than pushing the row past its table. The avatar is
/// decorative, so the cell reads as the email alone.
pub fn user_identity(email: &str) -> maud::Markup {
    maud::html! {
        span .user-identity {
            (avatar(email, AvatarSize::Md))
            span { (super::breakable_id(email)) }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn avatar_uses_brand_color_for_every_seed() {
        // Background lives in CSS (`.avatar { background: var(--primary-color) }`),
        // so the rendered markup carries no inline gradient/hue style — every
        // avatar is the same flat brand-orange circle, only the initial varies.
        let a = avatar("alice@example.com", AvatarSize::Md).into_string();
        let b = avatar("bob@example.com", AvatarSize::Md).into_string();
        for s in [&a, &b] {
            assert!(!s.contains("linear-gradient"), "no gradient in markup: {s}");
            assert!(!s.contains("hsl("), "no inline hue in markup: {s}");
            assert!(s.contains("avatar--md"), "size class present: {s}");
        }
        assert!(a.contains(">A</span>"), "alice initial A: {a}");
        assert!(b.contains(">B</span>"), "bob initial B: {b}");
    }

    /// The initial repeats the name beside it, so assistive technology
    /// skips it.
    #[test]
    fn avatar_is_hidden_from_assistive_technology() {
        assert_eq!(
            avatar("alice@example.com", AvatarSize::Md).into_string(),
            r#"<span class="avatar avatar--md" aria-hidden="true">A</span>"#
        );
    }

    /// The avatar first, then the email with its break opportunities; the
    /// avatar is hidden, so the email is the cell's whole accessible name.
    #[test]
    fn a_user_identity_is_the_avatar_beside_the_email() {
        assert_eq!(
            user_identity("ann@example.com").into_string(),
            concat!(
                r#"<span class="user-identity">"#,
                r#"<span class="avatar avatar--md" aria-hidden="true">A</span>"#,
                r#"<span>ann<wbr>@example<wbr>.com</span></span>"#,
            )
        );
    }

    #[test]
    fn avatar_is_deterministic_per_seed() {
        // Two calls with the same seed render identical markup.
        let a = avatar("alice@example.com", AvatarSize::Md).into_string();
        let b = avatar("alice@example.com", AvatarSize::Md).into_string();
        assert_eq!(a, b);
    }
}
