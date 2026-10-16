use crate::blocks::auth_ui::oauth;

pub mod admin_buttons;
pub mod dashboard;
pub mod profile;
pub mod security;
pub mod sessions;

/// A stored provider key (a linked account's `provider`, a session's
/// `oauth.<provider>`) as people read it: the provider's
/// [`label`](oauth::spec::OAuthProviderSpec::label), or the key itself for a
/// provider this build no longer supports (a stored link or session can
/// outlive one).
fn provider_label(name: &str) -> &str {
    oauth::spec::lookup(name).map_or(name, |p| p.label)
}

#[cfg(test)]
mod tests {
    use super::provider_label;

    #[test]
    fn provider_label_names_a_known_provider_and_passes_an_unknown_one_through() {
        assert_eq!(provider_label("github"), "GitHub");
        assert_eq!(provider_label("google"), "Google");
        assert_eq!(provider_label("microsoft"), "Microsoft");
        assert_eq!(provider_label("twitter"), "twitter");
    }
}
