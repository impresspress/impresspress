//! A block's own configuration, masked, read through the admin block.
//!
//! The variables table is the admin block's, and so is the rule that decides
//! which of its values may be shown: `util::is_sensitive_key`, which reads
//! the row's stored `sensitive` flag as well as the key. A block that shows
//! its settings to an administrator (the tickets Settings page) calls
//! [`read_own`] rather than reading values through the config client, which
//! knows neither the flag nor the mask. No block is granted the table for
//! this: the admin block answers the call in its own frame, and answers it
//! only with the keys the calling block declares.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use wafer_run::{context::Context, ErrorCode, InputStream, Message, OutputStream, WaferError};

use super::ADMIN_BLOCK_ID;
use crate::{platform_state::variables, util::is_sensitive_key};

/// The message kind of the call. A routed HTTP request's kind is
/// `"METHOD:/path"`, so no request from outside the runtime can carry it.
pub const KIND: &str = "admin.config.masked";

/// One of the calling block's declared settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaskedValue {
    /// The declared key.
    pub key: String,
    /// Whether a non-empty value is stored for it.
    pub set: bool,
    /// Whether the value is withheld: the row is flagged sensitive, or the
    /// key is one this build knows to hold a secret.
    pub sensitive: bool,
    /// The stored value; `None` when nothing is stored or it is sensitive.
    pub value: Option<String>,
}

/// The calling block's own declared settings, in declaration order, masked
/// by the admin block.
pub async fn read_own(ctx: &dyn Context) -> Result<Vec<MaskedValue>, WaferError> {
    let mut msg = Message::new(KIND);
    // The admin block's interface is `http-handler@v1`; a read is its
    // `retrieve` action.
    msg.set_meta(wafer_run::META_REQ_ACTION, "retrieve");
    let body = ctx
        .call_block(ADMIN_BLOCK_ID, msg, InputStream::empty())
        .await
        .collect_buffered()
        .await
        .map(|response| response.body)
        .map_err(WaferError::from)?;
    serde_json::from_slice(&body).map_err(|e| {
        WaferError::new(
            ErrorCode::Internal,
            format!("masked configuration answer did not decode: {e}"),
        )
    })
}

/// The admin block's answer to [`KIND`]: the declared settings of the block
/// that called, and only those. A call with no calling block (a top-level
/// dispatch) or from a block the runtime does not know is refused.
pub(super) async fn answer(ctx: &dyn Context) -> OutputStream {
    let Some(caller) = ctx.caller_id() else {
        return refuse("the masked configuration is answered only to a calling block");
    };
    let Some(info) = ctx.registered_blocks().iter().find(|b| b.name == caller) else {
        return refuse("the calling block is not registered");
    };
    let rows = match variables::list_all(ctx).await {
        Ok(rows) => rows,
        Err(error) => return OutputStream::error(error),
    };
    let stored: HashMap<&str, &variables::VariableRow> =
        rows.iter().map(|row| (row.key.as_str(), row)).collect();
    let values: Vec<MaskedValue> = info
        .config_keys
        .iter()
        .map(|var| {
            let row = stored.get(var.key.as_str());
            let value = row
                .map(|row| row.value.as_str())
                .filter(|value| !value.trim().is_empty());
            let sensitive =
                is_sensitive_key(&var.key, row.map_or(0, |row| i64::from(row.sensitive)));
            MaskedValue {
                key: var.key.clone(),
                set: value.is_some(),
                sensitive,
                value: value.filter(|_| !sensitive).map(str::to_string),
            }
        })
        .collect();
    match serde_json::to_vec(&values) {
        Ok(body) => OutputStream::respond(body),
        Err(e) => OutputStream::error(WaferError::new(ErrorCode::Internal, e.to_string())),
    }
}

fn refuse(reason: &str) -> OutputStream {
    OutputStream::error(WaferError::new(ErrorCode::PermissionDenied, reason))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::test_support::TestContext;

    /// A caller holding `vars`, with the admin block registered to answer.
    async fn caller(vars: Vec<wafer_run::ConfigVar>) -> TestContext {
        let mut ctx = TestContext::with_admin().await;
        ctx.register_block(ADMIN_BLOCK_ID, Arc::new(super::super::AdminBlock::new()));
        ctx.register_block_info(
            "acme/widget",
            wafer_run::BlockInfo::new("acme/widget", "0.1.0", "http-handler@v1", "Widget")
                .requires(vec![ADMIN_BLOCK_ID.into()])
                .config_keys(vars),
        );
        ctx.running_as("acme/widget")
    }

    async fn store(ctx: &TestContext, key: &str, value: &str, sensitive: bool) {
        variables::insert(
            &ctx.fixture(),
            variables::NewVariable {
                key: key.into(),
                value: value.into(),
                name: String::new(),
                description: String::new(),
                warning: String::new(),
                sensitive,
                updated_by: "test".into(),
                block: None,
            },
        )
        .await
        .expect("insert variable");
    }

    /// The stored flag masks a key that is neither declared secret nor named
    /// like one; the suffix masks one stored unflagged; a plain value is
    /// shown; an unset key says so; a key the caller does not declare is not
    /// answered at all.
    #[tokio::test]
    async fn the_caller_gets_its_own_keys_masked_by_the_full_rule() {
        let ctx = caller(vec![
            wafer_run::ConfigVar::new("ACME__WIDGET__COLOUR", "Colour", ""),
            wafer_run::ConfigVar::new("ACME__WIDGET__ACCOUNT", "Account", ""),
            wafer_run::ConfigVar::new("ACME__WIDGET__API_KEY", "Key", ""),
            wafer_run::ConfigVar::new("ACME__WIDGET__MODE", "Mode", "fast"),
        ])
        .await;
        store(&ctx, "ACME__WIDGET__COLOUR", "blue", false).await;
        store(&ctx, "ACME__WIDGET__ACCOUNT", "acct-123", true).await;
        store(&ctx, "ACME__WIDGET__API_KEY", "k-456", false).await;
        store(&ctx, "OTHER__BLOCK__SECRETLESS", "nope", false).await;

        let values = read_own(&ctx).await.expect("masked read");
        let value = |key: &str| values.iter().find(|v| v.key == key).cloned();
        assert_eq!(
            value("ACME__WIDGET__COLOUR").unwrap().value.as_deref(),
            Some("blue")
        );
        let account = value("ACME__WIDGET__ACCOUNT").unwrap();
        assert!(account.set && account.sensitive && account.value.is_none());
        let api_key = value("ACME__WIDGET__API_KEY").unwrap();
        assert!(api_key.set && api_key.sensitive && api_key.value.is_none());
        let mode = value("ACME__WIDGET__MODE").unwrap();
        assert!(!mode.set && mode.value.is_none());
        assert!(value("OTHER__BLOCK__SECRETLESS").is_none());
        assert_eq!(values.len(), 4);
    }

    /// Dispatched with no calling block, the call is refused.
    #[tokio::test]
    async fn a_call_with_no_calling_block_is_refused() {
        use wafer_run::Block;

        let ctx = TestContext::with_admin().await;
        let mut msg = Message::new(KIND);
        msg.set_meta(wafer_run::META_REQ_ACTION, "retrieve");
        let out = super::super::AdminBlock::new()
            .handle(&ctx, msg, InputStream::empty())
            .await;
        let error = out
            .collect_buffered()
            .await
            .map(|_| ())
            .map_err(WaferError::from);
        assert_eq!(error.unwrap_err().code, ErrorCode::PermissionDenied);
    }
}
