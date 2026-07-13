//! Row-level access over `wafer_run__auth__personal_access_tokens`.

use std::collections::HashMap;

use serde_json::{json, Value};
use wafer_block::db::{Filter, FilterOp, SortField};
use wafer_core::clients::database as db;
use wafer_run::context::Context;

use super::{decode_hex, map_opt_str, map_str, now_iso, RepoError};
use crate::util::hex_encode;

pub const TABLE: &str = "wafer_run__auth__personal_access_tokens";

#[derive(Debug, Clone)]
pub struct PatRow {
    pub token_hash: Vec<u8>,
    pub user_id: String,
    pub name: String,
    pub scopes: Vec<String>,
    pub created_at: String,
    pub last_used_at: Option<String>,
    pub expires_at: Option<String>,
}

#[derive(Debug, Clone)]
pub struct NewPat {
    pub token_hash: Vec<u8>,
    pub user_id: String,
    pub name: String,
    pub scopes: Vec<String>,
    pub expires_at: Option<String>,
}

/// Decode the stored `token_hash` value back into raw bytes.
///
/// Storage format is a lowercase 64-char hex string (matches what
/// `sessions`/`tokens` already do — see review L215).
fn decode_bytes(v: &Value) -> Option<Vec<u8>> {
    match v {
        Value::String(s) => decode_hex(s),
        _ => None,
    }
}

/// Decode scopes from whatever shape the backend returned.
///
/// We stored a JSON-encoded array as a string. The sqlite service helpfully
/// auto-parses text starting with `[`/`{` back into a `Value::Array`, so the
/// repo layer accepts both the post-parsing array and the raw JSON string.
fn decode_scopes(v: &Value) -> Result<Vec<String>, RepoError> {
    match v {
        Value::Array(arr) => Ok(arr
            .iter()
            .filter_map(|x| x.as_str().map(str::to_owned))
            .collect()),
        Value::String(s) => serde_json::from_str::<Vec<String>>(s)
            .map_err(|e| RepoError::Db(format!("scopes json: {e}"))),
        Value::Null => Ok(Vec::new()),
        other => Err(RepoError::Db(format!(
            "scopes has unexpected shape: {other}"
        ))),
    }
}

fn row_from_map(m: &HashMap<String, Value>) -> Result<PatRow, RepoError> {
    let token_hash = m
        .get("token_hash")
        .and_then(decode_bytes)
        .ok_or_else(|| RepoError::Db("missing token_hash".into()))?;
    let scopes = match m.get("scopes") {
        Some(v) => decode_scopes(v)?,
        None => Vec::new(),
    };
    Ok(PatRow {
        token_hash,
        user_id: map_opt_str(m, "user_id")
            .ok_or_else(|| RepoError::Db("missing user_id".into()))?,
        name: map_str(m, "name"),
        scopes,
        created_at: map_str(m, "created_at"),
        last_used_at: map_opt_str(m, "last_used_at"),
        expires_at: map_opt_str(m, "expires_at"),
    })
}

pub async fn insert(ctx: &dyn Context, new: NewPat) -> Result<(), RepoError> {
    let now = now_iso();
    let scopes_json = serde_json::to_string(&new.scopes)
        .map_err(|e| RepoError::Db(format!("scopes ser: {e}")))?;
    let mut data: HashMap<String, Value> = HashMap::new();
    data.insert("token_hash".into(), json!(hex_encode(&new.token_hash)));
    data.insert("user_id".into(), json!(new.user_id));
    data.insert("name".into(), json!(new.name));
    data.insert("scopes".into(), json!(scopes_json));
    data.insert("created_at".into(), json!(now));
    if let Some(exp) = new.expires_at.as_deref() {
        data.insert("expires_at".into(), json!(exp));
    }
    db::create(ctx, TABLE, data)
        .await
        .map_err(|e| RepoError::Db(format!("pat insert: {e}")))?;
    Ok(())
}

/// List every PAT belonging to `user_id`, newest first.
///
/// Ordering is by `created_at DESC` so the UI can render "most recent at the
/// top". `token_hash` is returned on the row but API callers are expected to
/// strip it before serialising to the client.
pub async fn list_for_user(ctx: &dyn Context, user_id: &str) -> Result<Vec<PatRow>, RepoError> {
    let records = db::list_sorted(
        ctx,
        TABLE,
        vec![Filter {
            field: "user_id".into(),
            operator: FilterOp::Equal,
            value: json!(user_id),
        }],
        vec![SortField {
            field: "created_at".into(),
            desc: true,
        }],
    )
    .await
    .map_err(|e| RepoError::Db(format!("pat list: {e}")))?;
    records.iter().map(|r| row_from_map(&r.data)).collect()
}

pub async fn find_by_token_hash(
    ctx: &dyn Context,
    hash: &[u8],
) -> Result<Option<PatRow>, RepoError> {
    let rows = db::list_all(
        ctx,
        TABLE,
        vec![Filter {
            field: "token_hash".into(),
            operator: FilterOp::Equal,
            value: json!(hex_encode(hash)),
        }],
    )
    .await
    .map_err(|e| RepoError::Db(format!("pat select: {e}")))?;
    match rows.into_iter().next() {
        Some(r) => Ok(Some(row_from_map(&r.data)?)),
        None => Ok(None),
    }
}

/// Delete the PAT identified by `token_hash`, iff it belongs to `user_id`.
///
/// Returns `true` when exactly one row was deleted. A `false` result covers
/// three cases — no row, wrong owner, or already-deleted — because the
/// public `/auth/tokens/{id}` endpoint collapses all three into a 404 (so
/// probes can't distinguish "this token belongs to someone else" from "no
/// such token").
pub async fn delete_by_id(
    ctx: &dyn Context,
    user_id: &str,
    token_hash: &[u8],
) -> Result<bool, RepoError> {
    let n = db::delete_by_filters_count(
        ctx,
        TABLE,
        vec![
            Filter {
                field: "token_hash".into(),
                operator: FilterOp::Equal,
                value: json!(hex_encode(token_hash)),
            },
            Filter {
                field: "user_id".into(),
                operator: FilterOp::Equal,
                value: json!(user_id),
            },
        ],
    )
    .await
    .map_err(|e| RepoError::Db(format!("pat delete: {e}")))?;
    Ok(n > 0)
}

/// Bumps `last_used_at` for the row identified by `hash`. Silently no-ops if
/// the row is missing.
pub async fn touch_last_used(ctx: &dyn Context, hash: &[u8]) -> Result<(), RepoError> {
    let now = now_iso();
    let mut data: HashMap<String, Value> = HashMap::new();
    data.insert("last_used_at".into(), json!(now));
    db::update_by_filters(
        ctx,
        TABLE,
        vec![Filter {
            field: "token_hash".into(),
            operator: FilterOp::Equal,
            value: json!(hex_encode(hash)),
        }],
        data,
    )
    .await
    .map_err(|e| RepoError::Db(format!("pat touch: {e}")))?;
    Ok(())
}
