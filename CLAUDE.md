# Development Guidelines

- Always fix the real issue. No code smells, no shortcuts, no workarounds.
- If the right fix requires touching many files, touch many files.
- No sync bridges (`poll_once`, `block_on`) to avoid propagating async. If something is async, callers must be async.
- No magic code or implicit mapping layers. Keep things explicit and easy to maintain. If a value has a prefix, it has the same prefix everywhere (env vars, D1, config API). No translation between representations.
- Config variable naming: `WAFER_RUN_SHARED__*` = shared app config (any block reads, admin writes). `{ORG}__{BLOCK}__*` = block-scoped (only owner + admin). `IMPRESSPRESS_*` (no `__`) = infrastructure, never in DB.
- Blocks declare their own config vars via `ConfigVar` in `BlockInfo::config_keys`. Shared vars are defined centrally in `impresspress-core/src/config_vars.rs`. No hardcoded lists — validation rules derived from conventions (suffix `_SECRET`/`_KEY` = sensitive, suffix `_URL` = validated, `input_type` = UI rendering).
- No raw SQL (`exec_raw` / `query_raw`) in block code — use `wafer-sql-utils` builders so backends stay swappable. If a builder is missing, add it to `wafer-sql-utils` rather than reaching back to raw SQL. Exceptions: admin SQL explorer, migration-file runners, test-fixture setup. See `../SQL_UTILS_PLAN.md`.
- Table names: each repo module owns its own `pub const TABLE: &str = "{org}__{block}__{name}"` (see `auth/repo/users.rs:12` for the canonical pattern). No central `*_COLLECTION` constants.
