//! Object search and recently-viewed listing.

use wafer_run::{context::Context, Message, OutputStream};

use crate::{
    blocks::files::repo,
    http::{err_bad_request, err_internal, ok_json},
};

pub(super) async fn handle_search(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let query = msg.query("q").to_string();
    if query.is_empty() {
        return err_bad_request("Missing search query");
    }

    let (_, page_size, offset) = msg.pagination_params(20);
    match repo::objects::search_completed(
        ctx,
        msg.user_id(),
        &query,
        page_size as i64,
        offset as i64,
    )
    .await
    {
        Ok(result) => ok_json(&result),
        Err(e) => err_internal("Search failed", e),
    }
}

pub(super) async fn handle_recent(ctx: &dyn Context, msg: &Message) -> OutputStream {
    match repo::views::list_recent_for_user(ctx, msg.user_id(), 20).await {
        Ok(result) => ok_json(&result),
        Err(e) => err_internal("Database error", e),
    }
}

#[cfg(test)]
mod integration_tests {
    use serde_json::json;

    use super::*;
    use crate::test_support::{auth_msg, output_json, TestContext};

    /// Seed an object-metadata row directly (bypassing `handle_upload_object`
    /// / the real storage backend, same as `seed_bucket` does for buckets) —
    /// enough to exercise `handle_search`'s DB query.
    async fn seed_object(ctx: &TestContext, bucket: &str, key: &str, owner: &str) {
        let data = crate::util::json_map(json!({
            "bucket": bucket,
            "key": key,
            "size": 0,
            "content_type": "application/octet-stream",
            "status": "complete",
            "uploaded_by": owner,
            "uploaded_at": crate::util::now_rfc3339(),
        }));
        repo::objects::seed(ctx, data).await.expect("seed object");
    }

    fn search_result_keys(v: &serde_json::Value) -> Vec<String> {
        v.get("records")
            .and_then(|r| r.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|rec| {
                        rec.get("data")
                            .and_then(|d| d.get("key"))
                            .and_then(|k| k.as_str())
                            .map(str::to_string)
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Regression test for SB-5. [`escape_like`] backslash-escapes `_`/`%`/`\`
    /// in the search term, but that escaping is only *effective* because
    /// `handle_search`'s `FilterOp::Like` filter now renders an explicit
    /// `ESCAPE '\'` clause (`wafer-sql-utils`, SB-5A) — SQLite's `LIKE` has NO
    /// default escape character, so a bare `\` in the pattern is just an
    /// ordinary literal byte without that clause.
    ///
    /// Seeds a file whose name contains `_` (`my_report.pdf`) alongside a
    /// decoy that an *unescaped* `_` wildcard would also match
    /// (`myXreport.pdf`); asserting the result is exactly the underscore file
    /// (not zero, not both) rules out either pre-SB-5A failure mode. Verified
    /// (2026-07-11) against wafer-run main (543e788, pre-ESCAPE): the pattern
    /// becomes `%my\_report%` with a literal backslash that appears in no
    /// real filename, so the query actually matched **zero** rows — worse
    /// than "underscore still wildcards", `escape_like`'s output broke search
    /// entirely on SQLite/D1. Against wafer-run `fix/sb5a-sql-like-escape`
    /// (b1e6c68, ESCAPE `'\'` present) this passes.
    #[tokio::test]
    async fn search_escapes_underscore_as_literal_not_wildcard() {
        let ctx = TestContext::with_files().await;
        seed_object(&ctx, "bucket", "my_report.pdf", "alice").await;
        // Decoy: only matches `%my_report%` if `_` is treated as a
        // single-char wildcard instead of the literal `_` it should be.
        seed_object(&ctx, "bucket", "myXreport.pdf", "alice").await;

        let mut msg = auth_msg("retrieve", "/b/storage/api/search", "alice");
        msg.set_meta("req.query.q", "my_report");

        let out = handle_search(&ctx, &msg).await;
        let keys = search_result_keys(&output_json(out).await);
        assert_eq!(
            keys,
            vec!["my_report.pdf"],
            "underscore in query must be escaped as a literal, not treated as a wildcard (got: {keys:?})"
        );
    }
}
