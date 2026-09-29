//! Subscription status: `/b/products/subscription` (authenticated).

use wafer_run::{context::Context, Message, OutputStream};

use crate::{
    blocks::{
        crud,
        products::{contracts::SubscriptionStatusResponse, repo},
    },
    http::{err_unauthenticated, ok_json},
};

pub(super) async fn handle_subscription(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let user_id = msg.user_id().to_string();
    if user_id.is_empty() {
        return err_unauthenticated("Not authenticated");
    }
    // A real repository failure must surface as an error, not be reported to
    // the caller as `{"subscription": null}` — indistinguishable from "you
    // have no subscription" and potentially misread as a cancellation.
    let subscription = match repo::subscriptions::subscription_for_user(ctx, &user_id).await {
        Ok(subscription) => subscription,
        Err(e) => return crud::db_error_internal(e, "Database error"),
    };
    ok_json(&SubscriptionStatusResponse { subscription })
}
