# D1 statement batching — investigation + producer design

CODE_REVIEW "Cloudflare efficiency #2", *batching half*. (The *aggregates half*
— folding the admin dashboard's many `db::count` calls into fewer aggregate
statements — shipped in #38.)

A single logical read/CRUD op can expand to **more than one D1 round-trip**. The
canonical case is `list`, which the shared executor runs as a `COUNT(*)` query
plus a `SELECT` — two billed D1 statements, two network round-trips. D1 exposes
a native `batch()` API (many statements, one round-trip); the audit-log drain
already uses it via `D1DatabaseService::create_many` (#30). This document records
why extending batching to `list` (and `update`) is a **wafer-run producer
change**, not something the impresspress consumer can do correctly on its own,
and specifies that producer change precisely so it can be dispatched.

## TL;DR

- **Consumer-doable now: nothing new.** The only D1-`batch()`-reachable site in
  the consumer is the audit-log drain, already batched (#30). Every feature-block
  read/write reaches D1 through the `DatabaseService` trait across the WASM host
  boundary and cannot see, let alone batch, the count+select pair a `list`
  expands into.
- **`list` = count + select is producer-first.** The shared
  `wafer_core::interfaces::database::exec::DbExec::list` builds both statements
  together but executes them as **two separate awaited primitives**
  (`run_scalar_i64` then `run_fetch`). The D1 adapter receives them as two
  independent method calls and has no handle on the fact that they're one logical
  op. Duplicating the whole `list` orchestration inside the D1 adapter's
  `DatabaseService::list` (filter-tree folding, column-ensure, pagination math,
  strict-schema, skip_count) to batch the two data statements would fork shared
  logic into the consumer and drift — exactly the kind of hack the root-cause rule
  forbids.
- **The correct fix** is a new `DbExec` batch primitive with a **sequential
  default** (so sqlite/postgres/browser/mocks are unchanged) that **only D1
  overrides** with native `batch()`, plus rewriting `DbExec::list` to issue
  count+select through it. `update` (UPDATE + re-fetch) is a second, identical
  consumer of the same primitive.

## Investigation

### How a logical op reaches D1 today

```
feature block (WASM)
  → db::* client  → host database handler  → Arc<dyn DatabaseService>
      → D1DatabaseService (impresspress-cloudflare/src/database.rs)
          → DatabaseService::list  ── forwards to ──▶  DbExec::list  (wafer-core, shared)
                                                          │
                                                          ├─ run_scalar_i64(count_sql)   ← D1 round-trip 1
                                                          └─ run_fetch(select_sql)       ← D1 round-trip 2
```

The D1 adapter implements only the `DbExec` execution *primitives* (`run_fetch`,
`run_fetch_one`, `run_execute`, `run_scalar_i64`, `run_scalar_f64`,
`dbx_table_exists`) and forwards every `DatabaseService` method into the shared
`DbExec` default. All `get/list/count/create/update/...` orchestration lives once
in `wafer-core`, identical across SQLite, Postgres, the browser backend, and D1.

### The `list` expansion (`wafer-core .../database/exec.rs::list`)

```rust
// both statements are built together …
let (count_stmt, select_stmt) = { … build with shared filter-tree extra_cond … };

// … but executed as two independent awaited primitives:
let total_count = match count_stmt {
    Some(stmt) => Some(self.run_scalar_i64(&stmt.sql, &sea_values_to_json(stmt.values)).await?),
    None => None,   // opts.skip_count
};
let records = self.run_fetch(&select_stmt.sql, &sea_values_to_json(select_stmt.values)).await?;
```

Because these are two separate `.await`s of two separate trait methods, the D1
adapter sees `run_scalar_i64(…)` and later `run_fetch(…)` with **no signal they
belong to one `list`**. It cannot coalesce them. The statements are already both
in hand at the call site — the producer just executes them one after another.

### Why the introspection round-trips are out of scope here

In non-strict mode `list` also runs `table_present_for_op` and
`ensure_query_columns` (a table-exists probe and a column-list query, possibly an
`ALTER`). Those are **conditional and branchy** (the ALTER path depends on the
probe result) and are already addressed by the per-isolate schema cache and
STRICT_SCHEMA mode (#313): production CF sets `WAFER_RUN__DATABASE__STRICT_SCHEMA`,
which skips them entirely. So on the production hot path a `list` is exactly the
two data statements — count + select — and batching them is a clean 2 → 1.

### Consumer-side audit (what is / isn't reachable)

| Path | Multi-statement? | Batchable at consumer? |
|------|------------------|------------------------|
| Audit-log drain (`lib.rs` `waitUntil`) | N inserts | **Already batched** via `create_many` → `db.batch()` (#30). Rows are all `request_logs`, one shape, so one `batch()` per drain. |
| Feature-block `list` / `update` / etc. | count+select / update+refetch | **No** — dispatched inside shared `DbExec`; the consumer never holds the pair. |
| Feature-block bulk writes | one `create()` per row | **No** — each row is a separate host round-trip; there is no bulk-create op on `DatabaseService`. |

There is no untapped consumer-side batch site. The honest outcome is a producer
change; this consumer PR requests it and carries the design.

## Producer design (wafer-run / `wafer-core`)

### 1. New batch types + `DbExec::run_batch` primitive

Add to `wafer-core/src/interfaces/database/exec.rs` (co-located with `DbExec`;
re-exported from the `database` module):

```rust
/// One statement in a batch, tagged with how its result should be decoded.
/// `params` is the JSON form produced by `sea_values_to_json(stmt.values)`,
/// identical to what the single-statement primitives already take.
pub enum BatchOp<'a> {
    /// Row-returning; decode all rows to `Record`s (like `run_fetch`).
    Fetch { sql: &'a str, params: &'a [serde_json::Value] },
    /// Single `i64` scalar, e.g. `COUNT(*)` (like `run_scalar_i64`).
    ScalarI64 { sql: &'a str, params: &'a [serde_json::Value] },
    /// Non-row statement; yields the affected-row count (like `run_execute`).
    Execute { sql: &'a str, params: &'a [serde_json::Value] },
}

/// Result of one `BatchOp`, in the same position as its op.
pub enum BatchResult {
    Fetch(Vec<Record>),
    ScalarI64(i64),
    Execute(i64),
}
```

New primitive on the `DbExec` trait, **with a provided default** so no backend
except D1 needs to change:

```rust
/// Run `ops` as one backend round-trip when the backend can, returning one
/// `BatchResult` per op **in order**.
///
/// Default: execute sequentially via the single-statement primitives —
/// behaviour-identical to issuing them one-by-one, which is what every backend
/// does today. Backends with a native multi-statement API (D1 `batch()`)
/// override this to collapse the round-trips. Statements run in list order and
/// results are positionally aligned; a backend that batches transactionally
/// (D1) additionally gives the set a single consistent snapshot.
async fn run_batch(&self, ops: &[BatchOp<'_>]) -> Result<Vec<BatchResult>, DatabaseError> {
    let mut out = Vec::with_capacity(ops.len());
    for op in ops {
        out.push(match *op {
            BatchOp::Fetch { sql, params }    => BatchResult::Fetch(self.run_fetch(sql, params).await?),
            BatchOp::ScalarI64 { sql, params } => BatchResult::ScalarI64(self.run_scalar_i64(sql, params).await?),
            BatchOp::Execute { sql, params }  => BatchResult::Execute(self.run_execute(sql, params).await?),
        });
    }
    Ok(out)
}
```

sqlite, postgres, the browser backend, and the test mocks inherit this default →
**zero changes, byte-identical behaviour** (sequential, same order).

### 2. Rewrite `DbExec::list` to batch count+select

Replace the two separate awaits (only when a count is requested) with one
`run_batch`. The statements are already built together; bind the JSON params to
locals (owned `Vec<serde_json::Value>`, `Send`) so nothing `!Send` crosses the
await:

```rust
let total_count: Option<i64> = match count_stmt {
    Some(count) => {
        let count_params  = sea_values_to_json(count.values);
        let select_params = sea_values_to_json(select_stmt.values);
        let results = self.run_batch(&[
            BatchOp::ScalarI64 { sql: &count.sql,       params: &count_params },
            BatchOp::Fetch     { sql: &select_stmt.sql, params: &select_params },
        ]).await?;
        // results[0] = ScalarI64(count), results[1] = Fetch(records)
        let mut it = results.into_iter();
        let count = match it.next() { Some(BatchResult::ScalarI64(n)) => n, _ => /* internal invariant */ };
        let records = match it.next() { Some(BatchResult::Fetch(r)) => r, _ => /* internal invariant */ };
        // …assemble RecordList with records + Some(count)…
        return Ok(/* … */);
    }
    None => None, // skip_count: no count statement, nothing to batch
};
// skip_count path: single run_fetch as today.
```

`skip_count` keeps its single `run_fetch` (there is no second statement to
batch). The table-exists guard and `ensure_query_columns` stay exactly where they
are, ahead of the batch.

### 3. `update` = UPDATE + re-fetch (same primitive, follow-on)

`DbExec::update` runs `run_execute` (UPDATE-by-id) then `self.get()`
(`run_fetch_one` SELECT-by-id) — two round-trips. It can issue both as one
`run_batch([Execute{update}, Fetch{select_by_id}])`: read `results[0]` (Execute)
for the affected count (→ `NotFound` on 0, skipping the row), else decode
`results[1]` (Fetch) for the returned record. Same win, same primitive; ship
after `list` is proven.

### 4. D1 override (consumer, `impresspress-cloudflare/src/database.rs`)

Only D1 overrides `run_batch`, mirroring `create_many`'s use of `db.batch()`:

```rust
async fn run_batch(&self, ops: &[BatchOp<'_>]) -> Result<Vec<BatchResult>, DatabaseError> {
    let mut stmts = Vec::with_capacity(ops.len());
    for op in ops {
        let (sql, params) = op.sql_params();          // small accessor on BatchOp
        stmts.push(self.prepare_bind(sql, params)?);
    }
    let results = self.db.batch(stmts).await.map_err(db_err)?;  // ← ONE D1 round-trip
    let mut out = Vec::with_capacity(ops.len());
    for (op, r) in ops.iter().zip(results) {
        if !r.success() {
            return Err(DatabaseError::Internal(format!(
                "batch statement failed: {}", r.error().unwrap_or_else(|| "unknown".into()))));
        }
        out.push(match op {
            BatchOp::Fetch { .. } => {
                let rows: Vec<serde_json::Value> = r.results().map_err(db_err)?;
                BatchResult::Fetch(rows.into_iter().map(json_to_record).collect())
            }
            BatchOp::ScalarI64 { .. } => {
                let rows: Vec<serde_json::Value> = r.results().map_err(db_err)?;
                BatchResult::ScalarI64(scalar_i64(rows.into_iter().next())) // reuse existing helper
            }
            BatchOp::Execute { .. } => {
                let changes = r.meta().map_err(db_err)?.and_then(|m| m.changes).unwrap_or(0);
                BatchResult::Execute(changes as i64)
            }
        });
    }
    Ok(out)
}
```

`json_to_record`, `scalar_i64`, `first_scalar`, `prepare_bind`, `db_err` all
already exist in `database.rs`. `worker::D1Database::batch(Vec<D1PreparedStatement>)
-> Result<Vec<D1Result>>` returns results **in submission order** (same contract
`create_many` relies on); `D1Result` exposes `.success()`, `.error()`,
`.results::<T>()`, and `.meta().changes` — all already used in this file. This
override lands in the **consumer pin-bump PR** once the producer merges (it can't
compile against a `BatchOp`/`run_batch` that don't yet exist in the pinned rev).

### Correctness constraints

- **Ordering / positional alignment.** `run_batch` guarantees results are 1:1
  with `ops` in order. The default preserves it trivially; D1 `batch()` is
  documented to return results in submission order.
- **Consistency (a strict improvement).** Today count and select are two
  round-trips: a write landing between them can make `total_count` disagree with
  the rows returned. D1 `batch()` runs the set as one implicit transaction — the
  count and select see the **same snapshot**, so batching *removes* an existing
  minor inconsistency rather than introducing one.
- **Failure = whole-batch rollback.** D1 `batch()` rejects (and rolls back) if
  any statement errors; the `.await` returns `Err`. The per-result `!success()`
  guard is a belt-and-braces check, matching `create_many`.
- **Missing / unmigrated table.** `list` runs the batch only after
  `table_present_for_op` (strict mode: assumed present). A genuinely-missing table
  in strict mode makes the batch fail loudly — the intended strict-mode contract —
  so the batch path needs no `no such table` special-casing (unlike `get`'s
  `run_fetch_one`).
- **`skip_count`.** No count statement ⇒ no batch ⇒ single `run_fetch`, unchanged.

### Before / after (D1 billed statements per op)

| Op | Mode | Before | After |
|----|------|-------:|------:|
| `list` (with count) | strict (prod) | 2 (count + select) | **1** (batch) |
| `list` (`skip_count`) | strict | 1 | 1 (no change) |
| `list` (with count) | non-strict (native sqlite/pg) | 2 | 2 (sequential default, unchanged) |
| `update` *(follow-on)* | strict | 2 (update + refetch) | **1** (batch) |

`list` is the highest-frequency case — every paginated admin table / collection
view runs one. On D1 that halves the billed statements and network round-trips for
those reads.

## Rollout sequence

1. **wafer-run producer PR** — add `BatchOp`/`BatchResult` + `DbExec::run_batch`
   (sequential default); rewrite `DbExec::list` to batch count+select; unit-test
   the default against a mock (order + positional decode) and against in-memory
   SQLite `list` (identical `RecordList` to the pre-batch path). Optionally include
   `update` in the same PR or as an immediate follow-on.
2. **impresspress pin bump PR** — bump the wafer-run rev, add the D1 `run_batch`
   override above, deploy to wafer.run, and verify the statement drop end-to-end
   (D1 analytics / a driven admin list view). sqlite/postgres/browser need no
   change.

## Test plan (producer)

- Mock `DbExec` (extend the existing `BarrierExec`-style mock in
  `exec.rs::tests`): assert `run_batch` default returns results positionally
  aligned to `ops` and in order; assert each variant decodes to the matching
  primitive's output.
- In-memory SQLite (`wafer-block-sqlite`): a `list` with filters + pagination +
  a filter-tree returns a `RecordList` byte-identical to the pre-batch executor
  (sqlite keeps the sequential default, so this is a regression guard on the
  `list` rewrite, not on batching).
- D1 override correctness is covered by the pin-bump PR's live deploy (the
  `impresspress-cloudflare` crate only compiles on wasm32 and can't unit-test the
  D1 client), matching how `create_many` was validated.
