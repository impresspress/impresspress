//! Flow definitions for Impresspress.
//!
//! All API routing is handled by the `impresspress/router` block, which delegates
//! to `impresspress-core`'s shared pipeline. The only flow needed is `site-main`,
//! which dispatches API paths to the router and serves the SPA for everything
//! else. The wafer-core base flows (wafer-run/infra) provide middleware.

pub mod site_main;

use wafer_run::{RuntimeError, Wafer};

/// The blocks whose config this module contributes to, named once.
const ROUTER_BLOCK: &str = "wafer-run/router";
const WEB_BLOCK: &str = "wafer-run/web";
const CORS_BLOCK: &str = "wafer-run/cors";
const SECURITY_HEADERS_BLOCK: &str = "wafer-run/security-headers";

/// Register the site-main flow (used with impresspress/router).
///
/// `cors_allowed_origins` and `csp_directives` are the operator-configured
/// values of [`crate::config_vars::CORS_ALLOWED_ORIGINS_KEY`] and
/// [`crate::config_vars::CSP_DIRECTIVES_KEY`], resolved from config by the
/// builder. They configure the `wafer-run/cors` and `wafer-run/security-
/// headers` middleware steps of this flow — the two infrastructure blocks
/// have no other config channel, so without this a native/Cloudflare deploy
/// denies every cross-origin request and blocks embedded Stripe.js.
///
/// `existing_block_configs` is what the consumer already declared through
/// [`crate::builder::ImpresspressBuilder::block_config`], in declaration
/// order. It is threaded in rather than read back off the `Wafer` because
/// there is nothing to read it back with: `Wafer::add_block_config` is a map
/// **insert**, the map is `pub(crate)` in the producer, and so a second
/// declaration for one block name silently replaces the first. See
/// [`site_main_block_configs`] for what that cost and what is done about it.
///
/// # Errors
///
/// Returns the underlying `RuntimeError` if the runtime rejects the
/// generated route config or the embedded `site_main::JSON` (a build-time
/// invariant — failure here means the bundled flow JSON drifted from the
/// runtime's flow schema).
pub fn register_site_main(
    w: &mut Wafer,
    cors_allowed_origins: &str,
    csp_directives: &str,
    existing_block_configs: &[(String, serde_json::Value)],
) -> Result<(), RuntimeError> {
    for (name, config) in
        site_main_block_configs(cors_allowed_origins, csp_directives, existing_block_configs)
    {
        w.add_block_config(&name, config);
    }

    w.add_flow_json(site_main::JSON)
}

/// Every block config `register_site_main` installs, given what the consumer
/// already declared.
///
/// Split out from the registration itself so the decision — in particular
/// which keys survive a second declaration — is testable without a `Wafer`,
/// and so the untested remainder is a three-line loop.
///
/// # Why three of these merge and one does not
///
/// `Wafer::add_block_config` replaces a block's whole config, and for three
/// of the four blocks here that was a bug. Their config is not one setting
/// but a **bag** of them, and this function is not their only author:
///
/// * `wafer-run/security-headers` — the browser sandbox declares
///   `{"csp": "… worker-src 'self'; frame-src 'self'", "frame_ancestors": "self",
///   "allow_blob_workers": true}`
///   so the `/b/dev` page can spawn its compiler worker and frame its own
///   live preview. Replacing that with `{"csp": <shared directives>}` dropped
///   both — silently, visible only as a browser refusing to start a worker.
///   Plan 1 Task 10's e2e measured the two bundles' `Content-Security-Policy`
///   side by side and found them byte-identical.
/// * `wafer-run/web` — the sandbox declares `{"cache_mode": "no-cache"}`,
///   because a sandbox republishes the site under the same URLs on every
///   keystroke and a cached page shows the previous generation. Replacing it
///   with the three route keys dropped that too.
/// * `wafer-run/cors` — nothing declares a neighbouring key today; it merges
///   for the same reason, so the next one to do so is not a fourth bug.
///
/// **`wafer-run/router` is the exception and stays a replacement.** Its
/// config is one setting (`routes`), `site_main` owns the route table
/// outright, and a consumer that means to replace it says so with
/// [`crate::builder::ImpresspressBuilder::final_block_config`], which runs
/// after this. Merging a route *table* is not a well-defined operation
/// anyway — two tables are not a union, they are an ordering.
///
/// Within a merge, every key the flow does not set is preserved and the keys
/// it does set win, except `csp`: a `;`-separated directive list, which is
/// **combined directive by directive** — the consumer's directives first, a
/// directive both authors name getting the union of their sources. Joining
/// the two strings instead would repeat a directive, and the security-headers
/// block (like a browser) keeps only the first occurrence of a name, so the
/// shared value's `script-src https://js.stripe.com` would be dropped behind
/// the consumer's own `script-src`. `allowed_origins` is deliberately not
/// combined — it is a comma-separated allow-list with its own separator.
pub fn site_main_block_configs(
    cors_allowed_origins: &str,
    csp_directives: &str,
    existing_block_configs: &[(String, serde_json::Value)],
) -> Vec<(String, serde_json::Value)> {
    let mut out = vec![
        // Inject default routes into the router block config. A replacement,
        // not a merge — see the note above.
        (
            ROUTER_BLOCK.to_string(),
            serde_json::json!({ "routes": site_main::default_routes() }),
        ),
        // Serve from the "site" storage bucket as an SPA. The three route
        // keys are the flow's to decide; anything else the consumer set on
        // this block (`cache_mode`) is theirs and survives.
        (
            WEB_BLOCK.to_string(),
            merged(
                declared(existing_block_configs, WEB_BLOCK),
                serde_json::json!({
                    "web_root": "site", "web_spa": "true", "web_index": "index.html"
                }),
                &[],
            ),
        ),
    ];

    // Feed the CORS allow-list to the middleware step. Empty stays fail-closed
    // (the block denies all cross-origin requests); a value or `*` opens it.
    // An empty value adds nothing at all rather than an empty config, so a
    // consumer-declared `wafer-run/cors` config is left exactly as it was.
    if !cors_allowed_origins.is_empty() {
        out.push((
            CORS_BLOCK.to_string(),
            merged(
                declared(existing_block_configs, CORS_BLOCK),
                serde_json::json!({ "allowed_origins": cors_allowed_origins }),
                &[],
            ),
        ));
    }

    // Feed extra CSP directives to the security-headers step. The block merges
    // these over its hard baseline (widen-only), so this can grant the Stripe
    // origins embedded Checkout needs without weakening the default policy.
    if !csp_directives.is_empty() {
        out.push((
            SECURITY_HEADERS_BLOCK.to_string(),
            merged(
                declared(existing_block_configs, SECURITY_HEADERS_BLOCK),
                serde_json::json!({ "csp": csp_directives }),
                &["csp"],
            ),
        ));
    }

    out
}

/// The config a consumer declared for `name`, or `None`.
///
/// The **last** declaration wins, because that is what `add_block_config`'s
/// map insert does with the same list — this reads the map the builder is
/// about to build, not the list.
fn declared<'a>(
    block_configs: &'a [(String, serde_json::Value)],
    name: &str,
) -> Option<&'a serde_json::Value> {
    block_configs
        .iter()
        .rev()
        .find(|(candidate, _)| candidate == name)
        .map(|(_, config)| config)
}

/// `existing` with `additions` applied: keys in `csp_keys` are combined
/// directive by directive ([`combine_csp`]), every other key is overwritten,
/// and keys only `existing` has are kept.
///
/// A non-object `existing` (or `None`) starts from an empty object — there is
/// nothing to preserve in a config that is not a map of settings, and
/// silently returning it unchanged would drop the additions instead.
fn merged(
    existing: Option<&serde_json::Value>,
    additions: serde_json::Value,
    csp_keys: &[&str],
) -> serde_json::Value {
    let mut out = existing
        .and_then(|value| value.as_object())
        .cloned()
        .unwrap_or_default();
    let serde_json::Value::Object(additions) = additions else {
        return serde_json::Value::Object(out);
    };

    for (key, addition) in additions {
        let joined = csp_keys.contains(&key.as_str()).then(|| {
            let before = out.get(&key).and_then(|v| v.as_str()).unwrap_or("");
            combine_csp(before, addition.as_str().unwrap_or(""))
        });
        match joined {
            Some(joined) => out.insert(key, serde_json::Value::String(joined)),
            None => out.insert(key, addition),
        };
    }
    serde_json::Value::Object(out)
}

/// Two `;`-separated CSP directive lists as one: `before`'s directives in
/// order, then `after`'s that `before` does not name. A directive both name
/// keeps `before`'s sources and gains `after`'s it lacks, `'none'` included.
/// What `'none'` beside other sources means is left to the security-headers
/// block, which reads this value through `merge_csp`: in a directive it adds
/// sources to, `'none'` is dropped from a list that has others; in the
/// narrow-only `base-uri` and `form-action`, `'none'` wins over the rest.
/// Names compare ASCII-case-insensitively, as a browser compares them; each is
/// written as `before` (or, for a new one, `after`) spelled it.
fn combine_csp(before: &str, after: &str) -> String {
    let parse = |policy: &str| -> Vec<(String, Vec<String>)> {
        policy
            .split(';')
            .filter_map(|directive| {
                let mut tokens = directive.split_ascii_whitespace();
                let name = tokens.next()?.to_string();
                Some((name, tokens.map(str::to_string).collect()))
            })
            .collect()
    };
    let mut combined = parse(before);
    for (name, sources) in parse(after) {
        match combined
            .iter_mut()
            .find(|(existing, _)| existing.eq_ignore_ascii_case(&name))
        {
            Some((_, existing)) => {
                for source in sources {
                    if !existing.contains(&source) {
                        existing.push(source);
                    }
                }
            }
            None => combined.push((name, sources)),
        }
    }
    combined
        .into_iter()
        .map(|(name, sources)| {
            if sources.is_empty() {
                name
            } else {
                format!("{name} {}", sources.join(" "))
            }
        })
        .collect::<Vec<_>>()
        .join("; ")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What `impresspress-web`'s `RuntimeFactory` declares on a
    /// `browser-devtools` build with `[dev] enabled` (`runtime_factory.rs`):
    /// the app CSP plus the sandbox's two additions, and the frame-ancestors
    /// relaxation its live-site preview iframe needs.
    fn sandbox_security_headers() -> Vec<(String, serde_json::Value)> {
        vec![(
            SECURITY_HEADERS_BLOCK.to_string(),
            serde_json::json!({
                "csp": "default-src 'self'; worker-src 'self'; frame-src 'self'",
                "frame_ancestors": "self",
                "allow_blob_workers": true,
                "cross_origin_isolation": "credentialless",
            }),
        )]
    }

    /// The shared `WAFER_RUN_SHARED__CSP_DIRECTIVES` value a deployment that
    /// embeds Stripe Checkout carries.
    const STRIPE: &str = "script-src https://js.stripe.com; frame-src https://js.stripe.com";

    fn config_for<'a>(
        configs: &'a [(String, serde_json::Value)],
        name: &str,
    ) -> &'a serde_json::Value {
        configs
            .iter()
            .rev()
            .find(|(candidate, _)| candidate == name)
            .map(|(_, config)| config)
            .unwrap_or_else(|| panic!("no config for {name} in {configs:?}"))
    }

    /// The regression this whole function exists for: the shared directives
    /// must not take the consumer's config with them.
    ///
    /// Before the fix this was one `add_block_config` — a map insert — so the
    /// result was `{"csp": <Stripe only>}`: no `worker-src`, no
    /// `frame_ancestors`, and a `/b/dev` page whose compiler worker the
    /// browser would refuse to start. Plan 1 Task 10's e2e measured a sandbox
    /// bundle's `Content-Security-Policy` as byte-identical to a feature-off
    /// one, which is the same fact from the outside.
    #[test]
    fn shared_directives_merge_into_a_declared_security_headers_config() {
        let declared = sandbox_security_headers();
        let out = site_main_block_configs("", STRIPE, &declared);
        let merged = config_for(&out, SECURITY_HEADERS_BLOCK);

        let csp = merged["csp"].as_str().expect("csp is a string");
        // The consumer's directives survive…
        assert!(csp.contains("worker-src 'self'"), "{csp}");
        assert!(csp.contains("default-src 'self'"), "{csp}");
        // …the shared ones are added, a directive both name taking both
        // authors' sources…
        assert!(csp.contains("script-src https://js.stripe.com"), "{csp}");
        assert!(
            csp.contains("frame-src 'self' https://js.stripe.com"),
            "{csp}"
        );
        // …in that order, joined by exactly one separator.
        assert!(
            csp.find("worker-src").unwrap() < csp.find("script-src https://js.stripe.com").unwrap(),
            "the consumer's directives come first: {csp}"
        );
        assert!(!csp.contains(";;"), "no doubled separator: {csp}");

        // …and every other key the consumer set is still there. This is the
        // one that actually moves `frame-ancestors` off `'none'`: the
        // security-headers block rewrites that directive from the
        // `frame_ancestors` key at request time, whatever `csp` says.
        assert_eq!(merged["frame_ancestors"], serde_json::json!("self"));
        // …the knob that lets the compiler worker spawn blob-URL workers…
        assert_eq!(merged["allow_blob_workers"], serde_json::json!(true));
        // …including the isolation posture the preview iframe depends on: a
        // COEP document only embeds documents that carry COEP themselves.
        assert_eq!(
            merged["cross_origin_isolation"],
            serde_json::json!("credentialless")
        );
    }

    /// With nothing declared, the result is what it always was.
    #[test]
    fn with_no_declared_config_the_shared_directives_stand_alone() {
        let out = site_main_block_configs("", STRIPE, &[]);
        assert_eq!(
            config_for(&out, SECURITY_HEADERS_BLOCK),
            &serde_json::json!({ "csp": STRIPE }),
        );
    }

    /// A deployment with no shared directives must not lose the consumer's
    /// config either — the empty case adds nothing at all rather than an
    /// empty config that would replace it.
    #[test]
    fn empty_shared_directives_leave_a_declared_config_untouched() {
        let declared = sandbox_security_headers();
        let out = site_main_block_configs("", "", &declared);
        assert!(
            !out.iter().any(|(name, _)| name == SECURITY_HEADERS_BLOCK),
            "nothing to add means nothing is added: {out:?}"
        );
    }

    /// `allowed_origins` is a comma-separated list with its own separator, so
    /// it is replaced rather than joined — while the consumer's other keys on
    /// the same block survive.
    #[test]
    fn the_cors_allow_list_replaces_but_keeps_its_neighbours() {
        let declared = vec![(
            CORS_BLOCK.to_string(),
            serde_json::json!({ "allowed_origins": "https://old.example", "max_age": "600" }),
        )];
        let out = site_main_block_configs("https://new.example", "", &declared);
        assert_eq!(
            config_for(&out, CORS_BLOCK),
            &serde_json::json!({ "allowed_origins": "https://new.example", "max_age": "600" }),
        );
    }

    /// The web block merges: the flow owns the three route keys, the consumer
    /// keeps everything else.
    ///
    /// `cache_mode: "no-cache"` is the one that matters — the browser sandbox
    /// declares it because a sandbox republishes the site under the same URLs
    /// on every keystroke, so a cached asset shows the previous generation.
    /// Replacing the config dropped it, and the `/b/dev` preview would have
    /// served stale CSS after every edit.
    #[test]
    fn the_web_config_merges_but_the_flow_owns_the_route_keys() {
        let declared = vec![(
            WEB_BLOCK.to_string(),
            serde_json::json!({ "web_root": "somewhere-else", "cache_mode": "no-cache" }),
        )];
        let web = config_for(&site_main_block_configs("", "", &declared), WEB_BLOCK).clone();
        assert_eq!(web["web_root"], "site", "the flow owns the SPA root");
        assert_eq!(web["web_spa"], "true");
        assert_eq!(web["web_index"], "index.html");
        assert_eq!(
            web["cache_mode"], "no-cache",
            "a key the flow does not set is the consumer's and survives"
        );
    }

    /// The router is the one block still replaced wholesale: its config is
    /// one setting, `site_main` owns the route table, and two tables are an
    /// ordering rather than a union. A consumer that means to replace it uses
    /// `final_block_config`, which runs after this. Pinned so "merge
    /// everything" is never applied here by analogy.
    #[test]
    fn the_router_config_is_still_a_replacement() {
        let declared = vec![(
            ROUTER_BLOCK.to_string(),
            serde_json::json!({ "routes": [], "something_else": "kept?" }),
        )];
        let router = config_for(&site_main_block_configs("", "", &declared), ROUTER_BLOCK).clone();
        assert_ne!(router["routes"], serde_json::json!([]));
        assert!(
            router.get("something_else").is_none(),
            "the router config is replaced wholesale, not merged"
        );
    }

    /// `add_block_config` is a map insert, so of two declarations for one
    /// block the *last* is the one that will be in the map this merges into.
    #[test]
    fn the_last_declaration_for_a_block_is_the_one_merged_into() {
        let declared = vec![
            (
                SECURITY_HEADERS_BLOCK.to_string(),
                serde_json::json!({ "csp": "worker-src 'self'", "frame_ancestors": "self" }),
            ),
            (
                SECURITY_HEADERS_BLOCK.to_string(),
                serde_json::json!({ "csp": "img-src data:" }),
            ),
        ];
        let merged = config_for(
            &site_main_block_configs("", STRIPE, &declared),
            SECURITY_HEADERS_BLOCK,
        )
        .clone();
        let csp = merged["csp"].as_str().unwrap();
        assert!(csp.contains("img-src data:"), "{csp}");
        assert!(
            !csp.contains("worker-src"),
            "the first declaration is gone: {csp}"
        );
        assert!(merged.get("frame_ancestors").is_none());
    }

    /// A trailing separator on the consumer's value must not produce `";;"`,
    /// and an empty half must not produce a leading or dangling one.
    #[test]
    fn joining_never_produces_an_empty_directive() {
        let with_trailing = vec![(
            SECURITY_HEADERS_BLOCK.to_string(),
            serde_json::json!({ "csp": "worker-src 'self' blob:;  " }),
        )];
        let csp = config_for(
            &site_main_block_configs("", STRIPE, &with_trailing),
            SECURITY_HEADERS_BLOCK,
        )["csp"]
            .as_str()
            .unwrap()
            .to_string();
        assert_eq!(csp, format!("worker-src 'self' blob:; {STRIPE}"));

        let empty_declared = vec![(
            SECURITY_HEADERS_BLOCK.to_string(),
            serde_json::json!({ "csp": "", "frame_ancestors": "self" }),
        )];
        let merged = config_for(
            &site_main_block_configs("", STRIPE, &empty_declared),
            SECURITY_HEADERS_BLOCK,
        )
        .clone();
        assert_eq!(merged["csp"], serde_json::json!(STRIPE));
        assert_eq!(merged["frame_ancestors"], serde_json::json!("self"));
    }

    /// A directive both authors name is served once, with both authors'
    /// sources. The security-headers block keeps the first occurrence of a
    /// directive and refuses the rest, so two `script-src` entries would drop
    /// Stripe's host from the policy the browser enforces.
    #[test]
    fn a_directive_both_authors_name_survives_the_security_headers_merge() {
        let declared = vec![(
            SECURITY_HEADERS_BLOCK.to_string(),
            serde_json::json!({ "csp": "script-src 'self' 'wasm-unsafe-eval'; frame-src 'self'" }),
        )];
        let merged = config_for(
            &site_main_block_configs("", STRIPE, &declared),
            SECURITY_HEADERS_BLOCK,
        )
        .clone();
        let csp = merged["csp"].as_str().unwrap();
        assert_eq!(
            csp,
            "script-src 'self' 'wasm-unsafe-eval' https://js.stripe.com; \
             frame-src 'self' https://js.stripe.com"
        );
        let served = wafer_block_security_headers::merge_csp("", csp);
        assert!(served.refused.is_empty(), "{:?}", served.refused);
        assert!(
            served.policy.contains("https://js.stripe.com"),
            "{}",
            served.policy
        );
    }

    /// The `Content-Security-Policy` the security-headers block serves for
    /// `config`: its own baseline, merged with `config`'s `csp`.
    async fn served_csp(config: &serde_json::Value) -> String {
        use wafer_run::Block;

        let ctx = crate::test_support::TestContext::new().await;
        let block = wafer_block_security_headers::SecurityHeadersBlock::new();
        block
            .lifecycle(
                &ctx,
                wafer_run::LifecycleEvent {
                    event_type: wafer_run::LifecycleType::Init,
                    data: config.to_string().into_bytes(),
                },
            )
            .await
            .expect("security-headers init");
        let out = block
            .handle(
                &ctx,
                wafer_run::Message::new("retrieve:/"),
                wafer_run::InputStream::empty(),
            )
            .await;
        match out.collect_buffered().await {
            Err(wafer_run::streams::output::TerminalNotResponse::Continue(msg)) => msg
                .get_meta("resp.header.Content-Security-Policy")
                .to_string(),
            _ => panic!("security-headers is a middleware and continues"),
        }
    }

    /// The sources a served `policy` gives `name`.
    fn served_sources<'a>(policy: &'a str, name: &str) -> Vec<&'a str> {
        policy
            .split(';')
            .map(|directive| directive.split_ascii_whitespace().collect::<Vec<_>>())
            .find(|tokens| tokens.first() == Some(&name))
            .map(|tokens| tokens[1..].to_vec())
            .unwrap_or_default()
    }

    /// `'none'` from either author is served as the security-headers block
    /// reads it: it gives way to the other author's sources in a directive
    /// sources are added to, and it wins in the narrow-only `form-action` and
    /// `base-uri`, so a deliberate `'none'` there is never loosened to
    /// `'self'`.
    #[tokio::test]
    async fn none_is_served_as_the_security_headers_block_reads_it() {
        let declared = vec![(
            SECURITY_HEADERS_BLOCK.to_string(),
            serde_json::json!({ "csp": "frame-src 'none'; form-action 'none'; base-uri 'self'" }),
        )];
        let shared = "frame-src https://js.stripe.com; form-action 'self'; base-uri 'none'";
        let merged = config_for(
            &site_main_block_configs("", shared, &declared),
            SECURITY_HEADERS_BLOCK,
        )
        .clone();
        let policy = served_csp(&merged).await;

        assert_eq!(
            served_sources(&policy, "frame-src"),
            ["https://js.stripe.com"],
            "{policy}"
        );
        assert_eq!(
            served_sources(&policy, "form-action"),
            ["'none'"],
            "{policy}"
        );
        assert_eq!(served_sources(&policy, "base-uri"), ["'none'"], "{policy}");
    }

    /// A config that is not an object has no settings to preserve; the
    /// additions must still be applied rather than dropped.
    #[test]
    fn a_non_object_declared_config_is_replaced_not_honoured() {
        let declared = vec![(
            SECURITY_HEADERS_BLOCK.to_string(),
            serde_json::json!("not a config"),
        )];
        assert_eq!(
            config_for(
                &site_main_block_configs("", STRIPE, &declared),
                SECURITY_HEADERS_BLOCK
            ),
            &serde_json::json!({ "csp": STRIPE }),
        );
    }
}
