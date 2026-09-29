//! The products block's own browser assets.
//!
//! Seven scripts, one per page surface. They live with the block rather than
//! in [`crate::ui::assets`] for the reason `blocks::llm::assets` and
//! `blocks::dev::assets` both state: they are this block's, not the shared
//! chrome's, and a build without `block-products` must not carry them.
//!
//! They stay on the **shared, content-hashed `/b/static/` manifest** rather
//! than moving to a block-local tier, for the reason spelled out in
//! `blocks::llm::assets`: the manifest is what makes an asset detachable — a
//! `--no-default-features` Worker build compiles none of these bytes and
//! streams them from R2, and the CLI publishes them from `ASSETS`. Ownership
//! is about which module declares the file, not which route serves it.
//!
//! **`storefront.js` is deliberately not here.** It is served unhashed from
//! this block's own `/b/products/storefront.js` route
//! (`handlers::commerce::handle_storefront_widget`) because that URL is a
//! published integration contract: `pages::integration_snippet` renders it
//! into the copy-paste `<script src="https://YOUR-IMPRESSPRESS-DOMAIN/b/
//! products/storefront.js">` embed that lives on third-party sites. A
//! content-hashed URL would break every embed on the next edit, which is the
//! opposite of what hashing buys for a first-party page asset.

/// The five-step product creation wizard.
///
/// Loaded by two pages: the wizard itself (`/b/products/admin/new` and
/// `/b/products/my-products/new`), and the product manager, whose visual
/// offer editor reuses the wizard's variable/component row builders. The
/// file initialises the wizard only when the page bootstrapped
/// `window.__productWizardConfig`; see its trailing comment for why that
/// decision cannot live in the page.
#[cfg(feature = "embed-assets")]
const WIZARD_JS: &str = include_str!("assets/products-wizard.js");

/// The product manager: offer cards, the visual offer editor, Payment Link
/// presets, moderation and status changes. Loaded after [`WIZARD_JS`], whose
/// helpers it calls.
#[cfg(feature = "embed-assets")]
const MANAGER_JS: &str = include_str!("assets/products-manager.js");

/// The catalog administration page (groups, types and templates).
#[cfg(feature = "embed-assets")]
const CATALOG_ADMIN_JS: &str = include_str!("assets/products-catalog-admin.js");

/// The seller administration page's suspend/reactivate controls.
#[cfg(feature = "embed-assets")]
const SELLER_ADMIN_JS: &str = include_str!("assets/products-seller-admin.js");

/// The Stripe setup page: connection test, webhook event browser and
/// provider-operation reconciliation. Self-initialising — it fills its two
/// empty containers at the end of the file.
#[cfg(feature = "embed-assets")]
const STRIPE_SETUP_JS: &str = include_str!("assets/products-stripe-setup.js");

/// Seller onboarding, seller dashboard and buyer billing redirects. Loaded by
/// the portal pages and by the order detail page, which calls its
/// `commercePortalRedirect` from [`ORDER_DETAIL_JS`].
#[cfg(feature = "embed-assets")]
const COMMERCE_PORTAL_JS: &str = include_str!("assets/products-commerce-portal.js");

/// The order detail page's refund form and billing-portal control. Loaded
/// after [`COMMERCE_PORTAL_JS`].
#[cfg(feature = "embed-assets")]
const ORDER_DETAIL_JS: &str = include_str!("assets/products-order-detail.js");

/// Bytes for this block's manifest assets, or `None` for a key it does not
/// own. Called only through [`crate::blocks::static_asset_bytes`].
#[cfg(feature = "embed-assets")]
pub fn bytes(logical: &str) -> Option<&'static [u8]> {
    Some(match logical {
        "products-wizard.js" => WIZARD_JS.as_bytes(),
        "products-manager.js" => MANAGER_JS.as_bytes(),
        "products-catalog-admin.js" => CATALOG_ADMIN_JS.as_bytes(),
        "products-seller-admin.js" => SELLER_ADMIN_JS.as_bytes(),
        "products-stripe-setup.js" => STRIPE_SETUP_JS.as_bytes(),
        "products-commerce-portal.js" => COMMERCE_PORTAL_JS.as_bytes(),
        "products-order-detail.js" => ORDER_DETAIL_JS.as_bytes(),
        _ => return None,
    })
}

/// Wizard bundle URL with content hash, e.g.
/// `/b/static/products-wizard-a1b2c3d4.js`.
pub fn wizard_js_url() -> String {
    crate::ui::assets::url("products-wizard.js")
}

/// Product-manager bundle URL with content hash.
pub fn manager_js_url() -> String {
    crate::ui::assets::url("products-manager.js")
}

/// Catalog-administration bundle URL with content hash.
pub fn catalog_admin_js_url() -> String {
    crate::ui::assets::url("products-catalog-admin.js")
}

/// Seller-administration bundle URL with content hash.
pub fn seller_admin_js_url() -> String {
    crate::ui::assets::url("products-seller-admin.js")
}

/// Stripe-setup bundle URL with content hash.
pub fn stripe_setup_js_url() -> String {
    crate::ui::assets::url("products-stripe-setup.js")
}

/// Commerce-portal bundle URL with content hash.
pub fn commerce_portal_js_url() -> String {
    crate::ui::assets::url("products-commerce-portal.js")
}

/// Order-detail bundle URL with content hash.
pub fn order_detail_js_url() -> String {
    crate::ui::assets::url("products-order-detail.js")
}

/// Every logical key this module owns, in one place so the tests below and
/// [`crate::blocks::products::tests::page_link_tests`] can enumerate them
/// without re-listing the set.
pub const LOGICAL_KEYS: &[&str] = &[
    "products-wizard.js",
    "products-manager.js",
    "products-catalog-admin.js",
    "products-seller-admin.js",
    "products-stripe-setup.js",
    "products-commerce-portal.js",
    "products-order-detail.js",
];

#[cfg(test)]
mod tests {
    /// The block owns these bytes, and `ui::assets` reaches them only through
    /// the block registry's delegation — never through an arm of its own.
    #[test]
    #[cfg(feature = "embed-assets")]
    fn the_shared_manifest_serves_this_blocks_bytes() {
        for logical in super::LOGICAL_KEYS {
            assert_eq!(
                crate::ui::assets::bytes(logical),
                super::bytes(logical),
                "{logical} must resolve to this block's bytes"
            );
            assert!(super::bytes(logical).is_some(), "{logical} not embedded");
        }
    }

    #[test]
    fn every_key_has_a_hashed_manifest_url() {
        for logical in super::LOGICAL_KEYS {
            let url = crate::ui::assets::url(logical);
            let stem = logical.trim_end_matches(".js");
            let prefix = format!("/b/static/{stem}-");
            assert!(url.starts_with(&prefix), "unexpected URL {url}");
            let hash = url.trim_start_matches(&prefix).trim_end_matches(".js");
            assert_eq!(hash.len(), 8, "expected 8-char short hash, got: {hash}");
            assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
        }
    }

    /// The wizard file is loaded by the product manager as well, which has no
    /// wizard DOM. Its init must therefore be conditional on the page having
    /// bootstrapped a wizard config — an unconditional call would throw on
    /// every manager page load, and the manager's own init would never run.
    #[test]
    #[cfg(feature = "embed-assets")]
    fn the_wizard_initialises_itself_only_when_a_page_bootstrapped_it() {
        assert!(
            super::WIZARD_JS.contains("if(window.__productWizardConfig)initProductWizard();"),
            "the wizard bundle must gate its own init on the page config"
        );
        assert!(
            super::MANAGER_JS
                .trim_end()
                .ends_with("initProductManager();"),
            "the manager bundle must initialise itself"
        );
    }

    /// The two pages that carry a per-request bootstrap read it off `window`,
    /// so the inline `<script>` that sets it has to run first. Nothing here
    /// can prove ordering in a browser; what it can prove is that the file
    /// never sets the config itself, which is the mistake that would make the
    /// inline carrier dead code.
    #[test]
    #[cfg(feature = "embed-assets")]
    fn the_bundles_read_their_config_and_never_write_it() {
        for (js, name) in [
            (super::WIZARD_JS, "__productWizardConfig"),
            (super::MANAGER_JS, "__productManagerConfig"),
            (super::ORDER_DETAIL_JS, "__orderDetailConfig"),
        ] {
            assert!(js.contains(name), "{name} must be read by its bundle");
            assert!(
                !js.contains(&format!("window.{name}=")),
                "{name} is a per-request bootstrap; the bundle must not assign it"
            );
        }
    }
}
