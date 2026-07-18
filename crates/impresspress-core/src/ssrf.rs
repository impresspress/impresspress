//! URL-level SSRF precheck for fetchers that cannot resolve DNS before they
//! connect — most notably the Cloudflare Workers `fetch` API, which performs
//! its own DNS + connect inside the runtime, so the native
//! `wafer-net-security` `SsrfFilteringResolver` resolve-before-connect guard
//! never runs on that path.
//!
//! The heavy lifting is delegated to the shared `wafer-net-security`
//! classifier ([`wafer_core::security::is_blocked_url`], re-exported from that
//! crate): scheme, `localhost`, and every private/loopback/link-local/CGNAT
//! IPv4/IPv6 literal (including the IPv6-embedded-v4 forms — NAT64, 6to4,
//! IPv4-mapped, IPv4-compatible). This module only *adds* the one thing a
//! URL/host classifier that special-cases `localhost` alone cannot know: the
//! well-known cloud instance-metadata **DNS hostnames** (e.g.
//! `metadata.google.internal`). It does NOT reimplement IP parsing.
//!
//! ## Honest boundary (documented, not overstated)
//!
//! This is a *literal* URL/host precheck. It rejects a request whose URL
//! textually names an internal target. It does **not** — and on a Worker
//! **cannot** — defend against DNS rebinding: a public-looking hostname that
//! resolves to a private IP at connect time passes this check, because the
//! Workers `fetch` API exposes no resolve-before-connect hook. That residual
//! case is Cloudflare's own subrequest-SSRF layer to catch. On the native
//! side the `SsrfFilteringResolver` closes it (the IP that is validated is the
//! IP that is dialed); this precheck is defense-in-depth on top of both.

/// Well-known cloud instance-metadata service **DNS hostnames**.
///
/// The IP-literal metadata endpoints are already rejected by
/// [`wafer_core::security::is_blocked_url`] via its existing arms —
/// AWS/Azure/OpenStack `169.254.169.254` and GCP `[fd00:ec2::254]` are
/// link-local / unique-local, and Alibaba's `100.100.100.200` is CGNAT
/// (`100.64.0.0/10`). What that classifier cannot know is the *name* form,
/// since it special-cases only `localhost` among hostnames. GCP's metadata
/// server is routinely reached by name (`metadata.google.internal`), so it is
/// listed here explicitly.
///
/// The bare short-name `metadata` is listed alongside the FQDN: on GCP the
/// metadata server is commonly addressed as `http://metadata/…`, which the
/// instance's DNS search domain expands to `metadata.google.internal`. A URL
/// host of `metadata` therefore reaches the same endpoint but would not match
/// the FQDN entry, so it is denied explicitly.
///
/// Kept as a small, documented security constant (not app/domain config, so
/// not a CLAUDE.md "hardcoded domain value" — it is the SSRF analogue of the
/// `localhost` string the shared classifier itself hardcodes). The natural
/// long-term home is `wafer-net-security::is_blocked_url` so the native fetch
/// path and registry downloads share it; that is flagged as a producer
/// follow-up rather than landed here, to keep this consumer change buildable
/// against the current pin.
const CLOUD_METADATA_HOSTS: &[&str] = &["metadata.google.internal", "metadata"];

/// True when `host` is a well-known cloud instance-metadata DNS hostname.
///
/// `host` is compared case-insensitively and with a single trailing FQDN dot
/// (`metadata.google.internal.`) stripped, since either form resolves to the
/// same metadata endpoint.
pub fn is_cloud_metadata_host(host: &str) -> bool {
    let host = host.strip_suffix('.').unwrap_or(host);
    CLOUD_METADATA_HOSTS
        .iter()
        .any(|blocked| host.eq_ignore_ascii_case(blocked))
}

/// URL-level SSRF precheck: `true` when the URL should be refused before any
/// outbound request is dispatched.
///
/// Composes the shared literal-URL classifier
/// ([`wafer_core::security::is_blocked_url`] — scheme / `localhost` / all
/// private-IP literal forms) with the cloud-metadata hostname denylist above.
/// An unparseable URL is treated as blocked (the shared classifier already
/// returns `true` for it). See the module docs for the DNS-rebinding boundary.
pub fn is_ssrf_blocked_url(url: &str) -> bool {
    if wafer_core::security::is_blocked_url(url) {
        return true;
    }
    // Only reached when the URL parsed and its host is NOT an IP literal or
    // `localhost`; check the surviving domain against the metadata denylist.
    match url::Url::parse(url) {
        Ok(parsed) => {
            matches!(parsed.host(), Some(url::Host::Domain(h)) if is_cloud_metadata_host(h))
        }
        // Unreachable in practice (is_blocked_url already blocked unparseable
        // URLs), but fail closed rather than allowing on a parse discrepancy.
        Err(_) => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_cloud_metadata_ip_literals() {
        // AWS/Azure/GCP/OpenStack IMDS (link-local v4).
        assert!(is_ssrf_blocked_url(
            "http://169.254.169.254/latest/meta-data/"
        ));
        // GCP IPv6 metadata (unique-local).
        assert!(is_ssrf_blocked_url("http://[fd00:ec2::254]/"));
        // Alibaba metadata (CGNAT 100.64.0.0/10).
        assert!(is_ssrf_blocked_url("http://100.100.100.200/"));
    }

    #[test]
    fn blocks_cloud_metadata_hostname() {
        // GCP metadata server reached by name — the case the shared literal-IP
        // classifier cannot catch (it special-cases only `localhost`).
        assert!(is_ssrf_blocked_url("http://metadata.google.internal/"));
        assert!(is_ssrf_blocked_url(
            "http://metadata.google.internal/computeMetadata/v1/"
        ));
        // Case- and trailing-dot-insensitive (FQDN form).
        assert!(is_ssrf_blocked_url("https://Metadata.Google.Internal./x"));
        assert!(is_cloud_metadata_host("metadata.google.internal"));
        assert!(is_cloud_metadata_host("METADATA.GOOGLE.INTERNAL."));
        assert!(!is_cloud_metadata_host("metadata.google.internal.evil.com"));
    }

    #[test]
    fn blocks_bare_metadata_short_name() {
        // `http://metadata/` — GCP's metadata server reached via the DNS
        // search-domain short-name, which expands to metadata.google.internal
        // on-instance. The bare host `metadata` would not match the FQDN entry,
        // so it is denied explicitly.
        assert!(is_ssrf_blocked_url("http://metadata/"));
        assert!(is_ssrf_blocked_url("http://metadata/computeMetadata/v1/"));
        assert!(is_ssrf_blocked_url("https://Metadata./x")); // case + trailing dot
        assert!(is_cloud_metadata_host("metadata"));
        assert!(is_cloud_metadata_host("METADATA."));
        // A longer host that merely starts with "metadata" is not the endpoint.
        assert!(!is_cloud_metadata_host("metadata.example.com"));
    }

    #[test]
    fn blocks_private_loopback_link_local_cgnat() {
        assert!(is_ssrf_blocked_url("http://10.0.0.1/")); // RFC1918
        assert!(is_ssrf_blocked_url("http://172.16.0.1/")); // RFC1918
        assert!(is_ssrf_blocked_url("http://192.168.1.1/")); // RFC1918
        assert!(is_ssrf_blocked_url("http://127.0.0.1/")); // loopback
        assert!(is_ssrf_blocked_url("http://169.254.1.1/")); // link-local
        assert!(is_ssrf_blocked_url("http://100.64.0.1/")); // CGNAT
    }

    #[test]
    fn blocks_localhost_and_non_http_schemes() {
        assert!(is_ssrf_blocked_url("http://localhost/admin"));
        assert!(is_ssrf_blocked_url("http://localhost:8080/admin"));
        assert!(is_ssrf_blocked_url("file:///etc/passwd"));
        assert!(is_ssrf_blocked_url("gopher://127.0.0.1/"));
        assert!(is_ssrf_blocked_url("not-a-url"));
    }

    #[test]
    fn blocks_ipv6_loopback_private_and_embedded_v4() {
        assert!(is_ssrf_blocked_url("http://[::1]/")); // loopback
        assert!(is_ssrf_blocked_url("http://[fc00::1]/")); // unique-local
        assert!(is_ssrf_blocked_url("http://[fe80::1]/")); // link-local
        assert!(is_ssrf_blocked_url("http://[::ffff:10.0.0.1]/")); // IPv4-mapped private
        assert!(is_ssrf_blocked_url("http://[64:ff9b::a9fe:a9fe]/")); // NAT64 → 169.254.169.254
        assert!(is_ssrf_blocked_url("http://[2002:7f00:1::]/")); // 6to4 → 127.0.0.1
    }

    #[test]
    fn allows_normal_public_hosts() {
        assert!(!is_ssrf_blocked_url("https://api.openai.com/v1/models"));
        assert!(!is_ssrf_blocked_url("https://example.com/path"));
        assert!(!is_ssrf_blocked_url("http://93.184.216.34/")); // public v4
        assert!(!is_ssrf_blocked_url("https://[2606:4700:4700::1111]/")); // public v6
        assert!(!is_ssrf_blocked_url(
            "https://api.anthropic.com/v1/messages"
        ));
    }
}
