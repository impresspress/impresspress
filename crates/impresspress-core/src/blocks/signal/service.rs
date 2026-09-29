//! The room store: one row per WebRTC handshake, keyed by a six-character
//! code, holding the host's offer and then the guest's answer.
//!
//! Timestamps are ISO-8601 `%Y-%m-%dT%H:%M:%SZ`, which sorts lexically, which
//! is why the expiry comparison below is a string compare — the same
//! convention `auth/repo/oauth_pkce.rs` uses for its own `expires_at` check.
//! No `exec_raw`/`query_raw`: every read and write goes through the typed
//! `wafer_core::clients::database` client, using the `_count` form of update
//! (`db::update_by_filters_count`, not bare `update_by_filters`) so a
//! filtered write with no PK in hand can tell "updated one row" from
//! "matched nothing" — the same reason every other filtered-update call site
//! in this codebase (`products/repo/{offers,refunds,seller_accounts,
//! purchases}.rs`) uses the `_count` form.

use std::collections::HashMap;

use serde_json::{json, Value};
use wafer_block::db::{Filter, FilterOp};
use wafer_core::clients::database as db;
use wafer_run::{context::Context, ConfigVar, ErrorCode, InputType, WaferError};

use crate::db_read::{self, Bound};

pub const TABLE: &str = "impresspress__signal__rooms";

/// Characters a room code is drawn from, and how many of them there are.
/// The pairs that are read wrong out loud or off a screen are absent: no
/// 0/O, no 1/I/L, no U. Mirrored by the client's own generator (spec §18.3).
pub const CODE_ALPHABET: &str = "23456789ABCDEFGHJKMNPQRSTVWXYZ";
pub const CODE_LEN: usize = 6;

/// Config keys, all block-scoped (`{ORG}__{BLOCK}__*`).
pub const TTL_KEY: &str = "IMPRESSPRESS__SIGNAL__ROOM_TTL_SECONDS";
pub const MAX_SDP_KEY: &str = "IMPRESSPRESS__SIGNAL__MAX_SDP_BYTES";
pub const STUN_KEY: &str = "IMPRESSPRESS__SIGNAL__STUN_URLS";
pub const DEFAULT_TTL_SECONDS: i64 = 600;
/// Longest room lifetime [`TTL_KEY`] can grant: a day. The TTL is added to the
/// current time to stamp a room's expiry, and an unbounded value reaches past
/// the last date chrono can represent, where that addition panics.
pub const MAX_TTL_SECONDS: i64 = 86_400;
pub const DEFAULT_MAX_SDP_BYTES: usize = 16_384;
pub const DEFAULT_STUN_URLS: &str = "stun:stun.l.google.com:19302";

/// The three config vars this block reads, for `BlockInfo::config_keys` —
/// so they show up on the admin Variables screen (the block has no admin
/// page of its own to render them on).
pub fn config_vars() -> Vec<ConfigVar> {
    vec![
        ConfigVar::new(
            TTL_KEY,
            &format!(
                "How long a signalling room stays open before it expires and its \
                 code is free again (1 to {MAX_TTL_SECONDS} seconds)"
            ),
            &DEFAULT_TTL_SECONDS.to_string(),
        )
        .name("Room TTL (seconds)")
        .input_type(InputType::Number),
        ConfigVar::new(
            MAX_SDP_KEY,
            "Largest SDP blob (offer or answer) a room is allowed to hold",
            &DEFAULT_MAX_SDP_BYTES.to_string(),
        )
        .name("Max SDP size (bytes)")
        .input_type(InputType::Number),
        ConfigVar::new(
            STUN_KEY,
            "Comma-separated STUN server URLs handed to the client's \
             RTCPeerConnection — never compiled into the game itself",
            DEFAULT_STUN_URLS,
        )
        .name("STUN server URLs"),
    ]
}

#[derive(Debug)]
pub enum RoomError {
    /// A live room already holds this code. The host rolls another.
    Taken,
    /// No such room, or it expired (and has been deleted on the way out).
    Gone,
    /// An answer already stands — a third browser may not take a paired room.
    Answered,
    BadCode,
    TooBig,
    /// A failed store call, its code kept so a WRAP denial stays a 403 and a
    /// quota a 429 (`crud::db_error_internal`).
    Db(WaferError),
}

fn db_err(e: WaferError) -> RoomError {
    RoomError::Db(e)
}

/// Current UTC time as an ISO-8601 string with a literal `Z` suffix
/// (`%Y-%m-%dT%H:%M:%SZ`). Sorts lexically, which is what makes the
/// `expires_at` string-compare below correct.
fn now_iso() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// `now + secs` in the same format as [`now_iso`]. `secs` may be negative —
/// tests use that to manufacture an already-expired row.
fn iso_plus_seconds(secs: i64) -> String {
    (chrono::Utc::now() + chrono::Duration::seconds(secs))
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string()
}

fn map_str(m: &HashMap<String, Value>, key: &str) -> String {
    m.get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_default()
}

fn code_filter(code: &str) -> Vec<Filter> {
    vec![Filter {
        field: "code".into(),
        operator: FilterOp::Equal,
        value: json!(code),
    }]
}

/// [`code_filter`] plus a compare-and-swap condition: only a row whose
/// `answer_sdp` is still empty matches. This is what makes `answer_room`'s
/// write a real CAS rather than an unconditional update keyed on `code`
/// alone — a `code`-only filter matches (and overwrites) the row whether or
/// not another write already set the answer, since `code` is the primary
/// key and therefore always matches exactly one row while the room exists.
fn answer_cas_filter(code: &str) -> Vec<Filter> {
    let mut filters = code_filter(code);
    filters.push(Filter {
        field: "answer_sdp".into(),
        operator: FilterOp::Equal,
        value: json!(""),
    });
    filters
}

/// Whether `code` is exactly `CODE_LEN` characters of `CODE_ALPHABET`.
pub fn valid_code(code: &str) -> bool {
    code.chars().count() == CODE_LEN && code.chars().all(|c| CODE_ALPHABET.contains(c))
}

/// A live room's two SDP fields, as read off one row. `answer_sdp` is empty
/// until answered.
struct RoomRow {
    offer_sdp: String,
    answer_sdp: String,
}

/// Read the row for `code`, treating a past `expires_at` as missing —
/// deleting it on the way out, the same discipline `oauth_pkce::take` uses
/// for its own expiry check. `Gone` covers both "no such code" and "expired".
async fn fetch_live(ctx: &dyn Context, code: &str) -> Result<RoomRow, RoomError> {
    let rows = db_read::list_bounded(
        ctx,
        TABLE,
        code_filter(code),
        Bound::UniqueKey("signal rooms.code is the table's PRIMARY KEY"),
    )
    .await
    .map_err(db_err)?;
    let Some(record) = rows.into_iter().next() else {
        return Err(RoomError::Gone);
    };
    let expires_at = map_str(&record.data, "expires_at");
    if expires_at.as_str() < now_iso().as_str() {
        // Present but expired: drop it here so the next reader doesn't pay
        // for the same discovery, exactly as `open_room`'s own sweep would.
        let _ = db::delete_by_filters_count(ctx, TABLE, code_filter(code)).await;
        return Err(RoomError::Gone);
    }
    Ok(RoomRow {
        offer_sdp: map_str(&record.data, "offer_sdp"),
        answer_sdp: map_str(&record.data, "answer_sdp"),
    })
}

/// What a failed [`create_row`] against `code`'s uniqueness means: a PK
/// collision is `Taken`, and every backend reports one as
/// `ErrorCode::AlreadyExists`. Because the refused write itself is the
/// answer, the two-hosts-roll-the-same-code race has no gap to lose: the
/// loser of that race gets `Taken` (409), never a raw `Db` (500). Anything
/// else is a database fault.
fn taken_or_db_error(error: WaferError) -> RoomError {
    if error.code == ErrorCode::AlreadyExists {
        return RoomError::Taken;
    }
    RoomError::Db(error)
}

/// The write half of [`open_room`]: build the row and insert it
/// unconditionally — no "is this code already live" check. `open_room` runs
/// that check first; this is its own function so a genuine two-hosts-
/// same-code race (both passing the check before either creates) is
/// provable directly: call this twice with the same code, bypassing the
/// check entirely, and the second call is what exercises the real
/// primary-key collision `taken_or_db_error` maps to `Taken`.
async fn create_row(
    ctx: &dyn Context,
    code: &str,
    sdp: &str,
    ttl_secs: i64,
) -> Result<(), RoomError> {
    let mut data: HashMap<String, Value> = HashMap::new();
    data.insert("code".into(), json!(code));
    data.insert("offer_sdp".into(), json!(sdp));
    data.insert("answer_sdp".into(), json!(""));
    data.insert("created_at".into(), json!(now_iso()));
    data.insert("expires_at".into(), json!(iso_plus_seconds(ttl_secs)));
    match db::create(ctx, TABLE, data).await {
        Ok(_) => Ok(()),
        Err(e) => Err(taken_or_db_error(e)),
    }
}

/// Put the host's offer up under `code`, which also creates the room.
/// Sweeps rows past their expiry first, so an abandoned room never holds a
/// code hostage and there is no background job to own.
pub async fn open_room(
    ctx: &dyn Context,
    code: &str,
    sdp: &str,
    ttl_secs: i64,
) -> Result<(), RoomError> {
    if !valid_code(code) {
        return Err(RoomError::BadCode);
    }
    sweep(ctx, &now_iso()).await?;
    // The sweep above has already dropped anything expired, so a row still
    // here for this code is live. This check narrows the race window but
    // does not close it — `create_row`'s own error mapping is what closes
    // it, for the two concurrent callers that both pass this check.
    let existing = db_read::list_bounded(
        ctx,
        TABLE,
        code_filter(code),
        Bound::UniqueKey("signal rooms.code is the table's PRIMARY KEY"),
    )
    .await
    .map_err(db_err)?;
    if !existing.is_empty() {
        return Err(RoomError::Taken);
    }
    create_row(ctx, code, sdp, ttl_secs).await
}

/// The host's offer. `Gone` when the code is unknown or expired.
pub async fn offer_for(ctx: &dyn Context, code: &str) -> Result<String, RoomError> {
    Ok(fetch_live(ctx, code).await?.offer_sdp)
}

/// The write half of `answer_room`: set `answer_sdp` only if it is still
/// empty (the [`answer_cas_filter`] condition), and report how many rows
/// that compare-and-swap actually touched. `0` means the row no longer
/// matches — either a racing write already set a non-empty `answer_sdp`, or
/// the room expired/was swept — between whatever the caller read and this
/// call; `answer_room` re-reads to tell those two cases apart. Split out so
/// a test can call it directly a second time, bypassing `answer_room`'s own
/// front-door "already answered" check, to force the exact race the CAS
/// filter exists to resolve.
async fn write_answer_if_empty(ctx: &dyn Context, code: &str, sdp: &str) -> Result<i64, RoomError> {
    let mut data: HashMap<String, Value> = HashMap::new();
    data.insert("answer_sdp".into(), json!(sdp));
    db::update_by_filters_count(ctx, TABLE, answer_cas_filter(code), data)
        .await
        .map_err(db_err)
}

/// Write the guest's answer into an unanswered room. `Gone` if the row was
/// deleted (expiry, or a concurrent `take_answer`) between the read that
/// found it live and the write; `Answered` if a second guest's write landed
/// first. Both are the `count == 0` case of the CAS in
/// [`write_answer_if_empty`] — a re-read is what tells them apart, since the
/// count alone doesn't say which happened.
pub async fn answer_room(ctx: &dyn Context, code: &str, sdp: &str) -> Result<(), RoomError> {
    if !valid_code(code) {
        return Err(RoomError::BadCode);
    }
    let row = fetch_live(ctx, code).await?;
    if !row.answer_sdp.is_empty() {
        return Err(RoomError::Answered);
    }
    let count = write_answer_if_empty(ctx, code, sdp).await?;
    if count == 0 {
        return Err(match fetch_live(ctx, code).await {
            // Present (and live) means someone else's answer is what's
            // there now — the room itself didn't vanish.
            Ok(_) => RoomError::Answered,
            // Absent or expired: the room is what's gone, not the answer.
            Err(e) => e,
        });
    }
    Ok(())
}

/// The guest's answer, and the room with it: `Ok(None)` while nobody has
/// answered, `Ok(Some(sdp))` once one has — and then the row is deleted, so
/// a handshake is single-use. `Gone` when there is no room at all.
///
/// Deliberately two calls (a read, then an unconditional delete keyed only
/// by `code`) rather than one unconditional take on every poll: consuming on
/// every poll would delete the room the moment the host asks before anyone
/// has answered. A lost race between two polls both reading a fresh answer
/// just means the loser's delete matches nothing — the sdp it already read
/// is still what it returns, and the other side's next poll sees `Gone` (a
/// join that failed rather than one that half-worked).
///
/// The paragraph above is now the ONLY reason this is two calls. It used to
/// have a second one: `DbExec`'s shared `take_where` dispatched its
/// `DELETE … RETURNING` through `run_fetch`, the read path, so against a
/// file-backed `SQLiteDatabaseService::open` (every real native deployment)
/// the delete reached a read-only connection, failed with "attempt to write a
/// readonly database", and `run_fetch` swallowed that failure per-row — the
/// row silently survived and a poll after the first handed the answer out
/// again instead of 404ing. That is fixed upstream (`take_where` now runs
/// through `DbExec::run_execute_returning`, the write path), so
/// `db::take_by_filters` is no longer unsafe here; the polling semantics
/// above are what still rule it out.
pub async fn take_answer(ctx: &dyn Context, code: &str) -> Result<Option<String>, RoomError> {
    let row = fetch_live(ctx, code).await?;
    if row.answer_sdp.is_empty() {
        return Ok(None);
    }
    db::delete_by_filters_count(ctx, TABLE, code_filter(code))
        .await
        .map_err(db_err)?;
    Ok(Some(row.answer_sdp))
}

/// Delete every row whose `expires_at < cutoff`. Returns how many.
pub async fn sweep(ctx: &dyn Context, cutoff: &str) -> Result<u64, RoomError> {
    let n = db::delete_by_filters_count(
        ctx,
        TABLE,
        vec![Filter {
            field: "expires_at".into(),
            operator: FilterOp::LessThan,
            value: json!(cutoff),
        }],
    )
    .await
    .map_err(db_err)?;
    Ok(n as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestContext;

    fn sdp(n: usize) -> String {
        "v=0\r\n".repeat(n)
    }

    #[tokio::test]
    async fn a_room_holds_an_offer_then_an_answer_then_is_gone() {
        let ctx = TestContext::with_signal().await;
        open_room(&ctx, "AB2CD3", &sdp(1), 600).await.expect("open");
        assert_eq!(offer_for(&ctx, "AB2CD3").await.expect("offer"), sdp(1));
        // Nobody has answered yet: waiting, not gone.
        assert!(take_answer(&ctx, "AB2CD3").await.expect("poll").is_none());
        answer_room(&ctx, "AB2CD3", &sdp(2)).await.expect("answer");
        assert_eq!(
            take_answer(&ctx, "AB2CD3").await.expect("take"),
            Some(sdp(2))
        );
        // Single use: the row went with the answer.
        assert!(matches!(
            take_answer(&ctx, "AB2CD3").await,
            Err(RoomError::Gone)
        ));
        assert!(matches!(
            offer_for(&ctx, "AB2CD3").await,
            Err(RoomError::Gone)
        ));
    }

    #[tokio::test]
    async fn a_live_code_cannot_be_opened_twice() {
        let ctx = TestContext::with_signal().await;
        open_room(&ctx, "AB2CD3", &sdp(1), 600).await.expect("open");
        assert!(matches!(
            open_room(&ctx, "AB2CD3", &sdp(1), 600).await,
            Err(RoomError::Taken)
        ));
    }

    #[tokio::test]
    async fn an_expired_code_is_free_again_and_reads_as_gone() {
        let ctx = TestContext::with_signal().await;
        open_room(&ctx, "AB2CD3", &sdp(1), -10).await.expect("open");
        assert!(matches!(
            offer_for(&ctx, "AB2CD3").await,
            Err(RoomError::Gone)
        ));
        // Read dropped it; and even without the read, create sweeps.
        open_room(&ctx, "AB2CD3", &sdp(3), 600)
            .await
            .expect("reopen");
        assert_eq!(offer_for(&ctx, "AB2CD3").await.expect("offer"), sdp(3));
    }

    #[tokio::test]
    async fn a_paired_room_refuses_a_second_answer() {
        let ctx = TestContext::with_signal().await;
        open_room(&ctx, "AB2CD3", &sdp(1), 600).await.expect("open");
        answer_room(&ctx, "AB2CD3", &sdp(2)).await.expect("answer");
        assert!(matches!(
            answer_room(&ctx, "AB2CD3", &sdp(3)).await,
            Err(RoomError::Answered)
        ));
    }

    #[tokio::test]
    async fn answering_a_room_that_never_existed_is_gone_not_a_create() {
        let ctx = TestContext::with_signal().await;
        assert!(matches!(
            answer_room(&ctx, "AB2CD3", &sdp(1)).await,
            Err(RoomError::Gone)
        ));
    }

    #[test]
    fn a_code_is_six_unambiguous_characters() {
        assert!(valid_code("AB2CD3"));
        assert!(!valid_code("AB2CD")); // short
        assert!(!valid_code("AB2CD3X")); // long
        assert!(!valid_code("AB2CD0")); // a zero reads as an O
        assert!(!valid_code("ab2cd3")); // the alphabet is upper case
        assert!(!valid_code("AB2CD/")); // and it is not a path
    }

    /// `create_row` is `open_room`'s write half with the "already live"
    /// check skipped entirely — calling it twice, directly, with the same
    /// code is exactly the race two hosts rolling the same code produce
    /// (both pass the check, then both create): the primary-key collision
    /// on the second insert must map to `Taken`, not surface as a raw `Db`.
    #[tokio::test]
    async fn a_create_after_a_create_with_the_same_code_bypassing_the_check_is_taken() {
        let ctx = TestContext::with_signal().await;
        create_row(&ctx, "AB2CD3", &sdp(1), 600)
            .await
            .expect("first create");
        assert!(matches!(
            create_row(&ctx, "AB2CD3", &sdp(2), 600).await,
            Err(RoomError::Taken)
        ));
    }

    /// `answer_room`'s own "already answered" check is sequential — it
    /// can't catch two truly concurrent writers, only two writes that fully
    /// `.await` one after another. The CAS filter in
    /// [`write_answer_if_empty`] is what actually has to refuse a second
    /// writer, so this calls it directly (bypassing `answer_room`'s
    /// front-door check, which a genuine race would also bypass) to prove
    /// the store itself — not just the application-level ordering — is what
    /// keeps the first answer.
    #[tokio::test]
    async fn a_racing_second_write_cannot_overwrite_the_first_answer() {
        let ctx = TestContext::with_signal().await;
        open_room(&ctx, "AB2CD3", &sdp(1), 600).await.expect("open");
        answer_room(&ctx, "AB2CD3", &sdp(2))
            .await
            .expect("first answer");
        let count = write_answer_if_empty(&ctx, "AB2CD3", &sdp(3))
            .await
            .expect("racing write");
        assert_eq!(
            count, 0,
            "the CAS must refuse a write once an answer already stands"
        );
        assert_eq!(
            take_answer(&ctx, "AB2CD3").await.expect("take"),
            Some(sdp(2)),
            "the first answer must survive the racing write"
        );
    }

    /// Production Cloudflare/D1 runs with `WAFER_RUN__DATABASE__STRICT_SCHEMA`
    /// on (see `impresspress-cloudflare/src/database.rs`), where the shared
    /// `db::create`/`db::update_by_filters_count` defaults' lazy
    /// `ALTER TABLE ADD COLUMN` for the synthesized `id` and stamped
    /// `updated_at` columns is switched off — a write against an undeclared
    /// column fails loudly instead. This is the regression test for that:
    /// every default (non-strict) unit test above would still pass even if
    /// the migration forgot `id`/`updated_at`, because lazy column-add
    /// papers over the gap. This one can't, because
    /// `TestContext::set_strict_schema` disables that lazy path the same
    /// way production's config flag does.
    #[tokio::test]
    async fn a_room_can_be_opened_and_answered_under_strict_schema() {
        let ctx = TestContext::with_signal().await;
        ctx.set_strict_schema(true);
        open_room(&ctx, "AB2CD3", &sdp(1), 600)
            .await
            .expect("open under strict schema");
        assert_eq!(offer_for(&ctx, "AB2CD3").await.expect("offer"), sdp(1));
        answer_room(&ctx, "AB2CD3", &sdp(2))
            .await
            .expect("answer under strict schema");
        assert_eq!(
            take_answer(&ctx, "AB2CD3").await.expect("take"),
            Some(sdp(2))
        );
    }
}
