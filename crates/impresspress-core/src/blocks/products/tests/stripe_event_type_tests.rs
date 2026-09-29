//! One list of Stripe event types, read by everything that names them.
//!
//! The webhook dispatcher and the Stripe setup page both have to know which
//! `type` values this block does something with: the dispatcher to route the
//! delivery, the page to tell an operator which events to subscribe the
//! destination to. Until this PR each spelled the set out on its own — 21
//! string literals in `stripe.rs`'s match, 21 more in `pages.rs`'s `<li>`
//! list, and 21 again in `docs/products-stripe-commerce.md` — with nothing
//! comparing them. A type added to the dispatcher and forgotten on the page
//! is invisible: the operator never subscribes to it, Stripe never delivers
//! it, and the missing side effect looks like a bug in the handler that was
//! never called.
//!
//! [`StripeEventType`] is now the one list. The dispatcher matches on it
//! exhaustively (a new variant is a compile error until it is routed), the
//! page renders `StripeEventType::ALL`, and the tests below pin the two
//! remaining places a spelling could drift: the wire literal each variant
//! serializes to, and the operator documentation.

use wafer_run::{Block as _, InputStream};

use super::{
    super::{contracts::StripeEventType, ProductsBlock},
    harness::ctx,
};
use crate::test_support::{admin_msg, output_html};

/// The wire spelling of one variant, through the same serde path the page
/// renders and the dispatcher decodes with.
fn wire(value: StripeEventType) -> String {
    crate::util::wire_str(&value)
}

/// Every literal below is a Stripe event type name, which is Stripe's
/// vocabulary and not ours: a variant renamed without its `serde(rename)`
/// silently stops matching real deliveries.
#[test]
fn every_variant_carries_the_event_name_stripe_sends() {
    let expected = [
        (StripeEventType::AccountUpdated, "account.updated"),
        (
            StripeEventType::CheckoutSessionCompleted,
            "checkout.session.completed",
        ),
        (
            StripeEventType::CheckoutSessionAsyncPaymentSucceeded,
            "checkout.session.async_payment_succeeded",
        ),
        (
            StripeEventType::CheckoutSessionAsyncPaymentFailed,
            "checkout.session.async_payment_failed",
        ),
        (
            StripeEventType::PaymentIntentSucceeded,
            "payment_intent.succeeded",
        ),
        (
            StripeEventType::PaymentIntentPaymentFailed,
            "payment_intent.payment_failed",
        ),
        (
            StripeEventType::PaymentIntentProcessing,
            "payment_intent.processing",
        ),
        (
            StripeEventType::PaymentIntentRequiresAction,
            "payment_intent.requires_action",
        ),
        (
            StripeEventType::PaymentIntentCanceled,
            "payment_intent.canceled",
        ),
        (
            StripeEventType::CustomerSubscriptionUpdated,
            "customer.subscription.updated",
        ),
        (
            StripeEventType::CustomerSubscriptionDeleted,
            "customer.subscription.deleted",
        ),
        (StripeEventType::InvoicePaid, "invoice.paid"),
        (
            StripeEventType::InvoicePaymentSucceeded,
            "invoice.payment_succeeded",
        ),
        (
            StripeEventType::InvoicePaymentFailed,
            "invoice.payment_failed",
        ),
        (
            StripeEventType::ChargeDisputeCreated,
            "charge.dispute.created",
        ),
        (
            StripeEventType::ChargeDisputeUpdated,
            "charge.dispute.updated",
        ),
        (
            StripeEventType::ChargeDisputeClosed,
            "charge.dispute.closed",
        ),
        (StripeEventType::RefundCreated, "refund.created"),
        (StripeEventType::RefundUpdated, "refund.updated"),
        (StripeEventType::RefundFailed, "refund.failed"),
        (StripeEventType::ChargeRefunded, "charge.refunded"),
    ];

    for (variant, literal) in expected {
        assert_eq!(wire(variant), literal, "{variant:?} changed its wire name");
        assert_eq!(
            StripeEventType::from_wire(literal),
            Some(variant),
            "{literal} no longer decodes to {variant:?}"
        );
    }

    let names: Vec<String> = StripeEventType::ALL.iter().copied().map(wire).collect();
    assert_eq!(
        names,
        expected
            .iter()
            .map(|(_, l)| l.to_string())
            .collect::<Vec<_>>(),
        "ALL is generated from the same declaration as the variants, so this \
         fails only when a variant is added, removed or reordered — update \
         the table above with it"
    );
}

/// **The behaviour an unknown type must keep.**
///
/// Stripe delivers every event type the destination is subscribed to, and a
/// Stripe account can be subscribed to more than this block handles (the
/// dashboard's "all events" option subscribes to every type there is). An
/// unrecognised `type` is therefore normal traffic, not an error: it decodes
/// to `None` and the dispatcher's `None` arm ignores it, exactly as the old
/// `_ =>` arm did. `webhook_unhandled_event_is_acknowledged_and_sealed` in
/// `stripe_tests` pins the end-to-end half of that.
#[test]
fn an_event_type_the_block_does_not_handle_is_not_a_variant() {
    for unhandled in [
        // Real Stripe types this block is not subscribed to.
        "payment_intent.created",
        "customer.subscription.trial_will_end",
        "charge.succeeded",
        "invoice.upcoming",
        // A delivery with no `type` at all reads as the empty string.
        "",
        // Neighbours of real variants, to prove the match is exact.
        "account.update",
        "checkout.session.completed ",
        "ACCOUNT.UPDATED",
    ] {
        assert_eq!(
            StripeEventType::from_wire(unhandled),
            None,
            "{unhandled:?} is not one of the handled types"
        );
    }
}

/// The `<code>` contents of the event-type list on the Stripe setup page.
///
/// Sliced between the `<summary>` that introduces the list and the end of
/// the `<ul>` that holds it, so the page's other `<code>` blocks (the
/// webhook path, for one) are not mistaken for event names.
fn rendered_event_list(html: &str) -> Vec<String> {
    const SUMMARY: &str = "Show required Stripe event types";
    let start = html
        .find(SUMMARY)
        .expect("the Stripe setup page introduces its event-type list");
    let list = &html[start..];
    let end = list.find("</ul>").expect("the event-type list is a <ul>");
    let list = &list[..end];

    let mut out = Vec::new();
    let mut rest = list;
    while let Some(open) = rest.find("<code>") {
        rest = &rest[open + "<code>".len()..];
        let close = rest.find("</code>").expect("an opened <code> is closed");
        out.push(rest[..close].to_string());
        rest = &rest[close..];
    }
    out
}

/// The page told an operator which events to subscribe to by re-spelling
/// them; now it renders the dispatcher's own list, so the two cannot part.
#[tokio::test]
async fn the_setup_page_lists_exactly_the_event_types_the_dispatcher_handles() {
    let ctx = ctx().await;
    let block = ProductsBlock::new();
    let html = output_html(
        block
            .handle(
                &ctx,
                admin_msg("retrieve", "/b/products/admin/stripe"),
                InputStream::empty(),
            )
            .await,
    )
    .await;

    assert_eq!(
        rendered_event_list(&html),
        StripeEventType::ALL
            .iter()
            .copied()
            .map(wire)
            .collect::<Vec<_>>(),
    );
}

/// The third copy of the list: the operator-facing setup documentation.
///
/// It is the page's instructions in prose, and a reader configuring a Stripe
/// destination from it subscribes to what it says. A type handled here but
/// missing there is a delivery that never arrives, which is why the doc is
/// checked by a test rather than by whoever remembers.
#[test]
fn the_setup_documentation_lists_the_same_event_types() {
    const DOC: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/products-stripe-commerce.md"
    );
    let doc = std::fs::read_to_string(DOC).expect("the Stripe setup documentation is readable");

    const HEADING: &str =
        "Subscribe to platform events and, when Connect selling is enabled, connected-account \
         events:";
    let start = doc
        .find(HEADING)
        .expect("the documentation introduces its event-type list");

    let listed: Vec<String> = doc[start + HEADING.len()..]
        .lines()
        .skip_while(|line| line.trim().is_empty())
        .take_while(|line| line.starts_with("- `"))
        .map(|line| {
            line.trim_start_matches("- `")
                .trim_end_matches('`')
                .to_string()
        })
        .collect();

    assert_eq!(
        listed,
        StripeEventType::ALL
            .iter()
            .copied()
            .map(wire)
            .collect::<Vec<_>>(),
    );
}
