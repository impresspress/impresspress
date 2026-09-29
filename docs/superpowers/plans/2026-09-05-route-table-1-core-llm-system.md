# Route table single source, PR 1: core + llm + system

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `EndpointRoute<H>` carry everything a `BlockEndpoint` carries so a block's `const ROUTES` table both dispatches requests and generates `info().endpoints`, and migrate the first two blocks (`llm`, `system`) onto it with no change to any declared path's auth level or schema.

**Architecture:** `endpoint_match::EndpointRoute<H>` gains a required `AuthLevel`, summary/description, four optional schema-producer function pointers, tags, deprecated and agent-tool fields, all `const`-constructible through `public/authenticated/admin` constructors and `const fn` builders. `endpoint_match::declare(&ROUTES) -> Vec<BlockEndpoint>` feeds `info()`. A new endpoint-surface snapshot test records every block's `(method, path, auth, tool)` lines so a migration proves it changed nothing. `llm` already dispatches on a table and only gains metadata; `system` moves from a hand-written `if`/`strip_prefix` handler to a two-row table.

**Tech Stack:** Rust (edition per workspace), `wafer-run` at rev `7d47e5e` (`BlockEndpoint`, `AuthLevel`, `HttpMethod`, `AgentTool`), `schemars` 1, `serde_json`, `tokio` tests, nightly `rustfmt`, `clippy -D warnings`.

**Spec:** `docs/superpowers/specs/2026-09-05-route-table-single-source-design.md` (this plan implements its "PR 1" and amends its section 2, see Task 0).

## Global Constraints

- No change to wafer-run. `BlockEndpoint`, `AuthLevel`, `HttpMethod` and `AgentTool` are consumed as they are at rev `7d47e5e`.
- Every OpenAPI snapshot under `crates/impresspress-core/tests/snapshots/*.openapi.json` is byte-identical at the end of every task. Never run `UPDATE_OPENAPI_SNAPSHOTS=1` against an `*.openapi.json` file in this PR.
- Every endpoint-surface snapshot `*.endpoints.json` is byte-identical at the end of every task, except `system.endpoints.json` in Task 4, whose exact new content is given there.
- Every row of a migrated block's table names its auth level through `EndpointRoute::public`, `::authenticated` or `::admin`. `EndpointRoute::new` is for not-yet-migrated tables only and declares `Admin`.
- Migrated blocks (`llm`, `system`) read path variables only through `msg.var(..)` after `endpoint_match::dispatch` bound them. No `path_param(`, `strip_prefix("/b`, or `starts_with("/b` remains in `blocks/llm/` or `blocks/system.rs`. Exception, kept and commented: `llm`'s inter-block `/b/llm/api/internal/default-target` guard, which is not an HTTP endpoint.
- TDD: write the test, run it and see it fail for the expected reason, then implement. Each task ends with a commit.
- Format with `cargo +nightly fmt --all`. Lint with `cargo clippy -p impresspress-core --all-targets -- -D warnings`. `cargo test -p impresspress-core --no-fail-fast` has one known unrelated failure, `lockfile_loads_remote_block` (wafer-run compiled without `wasmi` in `-p` builds); every other test must pass.
- Work happens in the worktree `/home/joris/Programs/suppers-ai/impresspress-worktrees/phase0` on branch `phase1/route-table-core` (already created from `origin/main`; the spec commit `52c6c20b` is on it). The session's worktree guard refuses compound shell commands that contain `git` or shell variables: put such commands in a script file under the scratchpad directory and run `bash <script>`.
- Commits end with the two trailer lines the session requires:
  `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>` and
  `Claude-Session: https://claude.ai/code/session_01Vfp8g6U7PQTG1kgw2834Tp`.
- Open PR #8 (`fix/llm-config-delete-route`) touches the same llm table. If it has merged into `main` before Task 3 starts, rebase this branch on `origin/main` first and carry its `DeleteConfig` row as described in Task 3, Step 3.

---

## File structure

| File | Responsibility after this PR |
|---|---|
| `crates/impresspress-core/src/endpoint_match.rs` | The row type `EndpointRoute<H>` with constructors and builders, `SchemaFn`, `schema_of<T>`, `declare`, the matcher (`match_template`, `dispatch`, `dispatch_path`, `endpoint_auth`). Unchanged matcher semantics. |
| `crates/impresspress-core/tests/endpoint_surface.rs` | New. Endpoint-surface snapshot test over `impresspress_core::blocks::all_block_infos()` (+ `dev` under `block-dev`). |
| `crates/impresspress-core/tests/snapshots/*.endpoints.json` | New. One file per block, sorted `METHOD path auth [tool=name]` lines. |
| `crates/impresspress-core/src/blocks/llm/mod.rs` | `ROUTES` carries the metadata that `info()` used to list; `info()` calls `declare(ROUTES)`. |
| `crates/impresspress-core/src/blocks/llm/routes/providers.rs` | Handlers read `msg.var("id")`; the `PROVIDERS_PREFIX` constant and `path_param` import go. |
| `crates/impresspress-core/src/blocks/llm/routes/models.rs` | `extract_model_path` reads only the bound variables. |
| `crates/impresspress-core/src/blocks/llm/routes/test_support.rs` | Gains `routed(msg)`, which runs a message through the block's own table before a handler test uses it. |
| `crates/impresspress-core/src/blocks/system.rs` | `Route { Health, Asset }`, a two-row `ROUTES`, `handle` dispatches; `static_asset` stays as the manifest lookup. |
| `docs/superpowers/specs/2026-09-05-route-table-single-source-design.md` | Section 2 rewritten (no in-segment parameters; system declares one row); PR 7 list gains `util::path_param`. |

---

### Task 0: Amend the spec for the one-row system decision

**Files:**
- Modify: `docs/superpowers/specs/2026-09-05-route-table-single-source-design.md`

**Why this task exists:** while planning, per-asset rows turned out to reintroduce an ordering hazard. `itim-latin-{hash}.woff2` matches the filename `itim-latin-ext-abc.woff2` (prefix `itim-latin-`, suffix `.woff2`, everything between bound to `hash`), and `impresspress-logo-{hash}.png` matches `impresspress-logo-2x-abc.png` the same way, so the right answer would depend on which row comes first. The exact-filename manifest lookup the system block already has was introduced to remove precisely that hazard (see the comment above `static_asset` in `system.rs`). One row with a whole-segment `{filename}` keeps it removed and needs no new matcher syntax.

- [ ] **Step 1: Replace section 2 of the spec**

Find the heading `### 2. Core: the matcher accepts an in-segment parameter` and replace that whole section (up to, not including, `### 3. Blocks:`) with:

```markdown
### 2. Core: no new template syntax; system declares one asset row

The one block whose declarations were unmatchable is `system`, which declares
each embedded asset as `/b/static/app-{hash}.css` with the parameter inside a
segment. Rather than teach the matcher in-segment parameters, `system`
declares one row, `GET /b/static/{filename}`, and the handler looks the bound
filename up in the build-time asset manifest by exact match, which is what it
does today. Two reasons:

- Asset filenames are content-hashed, so the exact-filename lookup already is
  the hash check: a stale URL is a 404 and never receives new bytes under an
  `immutable` cache header.
- Per-asset rows would make `itim-latin-{hash}.woff2` also match
  `itim-latin-ext-abc.woff2` (literal prefix, literal suffix, everything
  between bound), and `impresspress-logo-{hash}.png` also match the `-2x-`
  logo. The right answer would depend on table order, which is the hazard the
  exact lookup was introduced to remove.

The system surface snapshot therefore changes in PR 1 from the per-asset lines
to two lines, `GET /health public` and `GET /b/static/{filename} public`. The
router's access decision for an asset request is unchanged: `endpoint_auth`
resolves the bound filename to the declared `Public`, which is what lets PR 7
delete the `STATIC_PREFIX` carve-out.

`normalize_template` is deleted. The one colon-style template
(`userportal`'s `:hash`) is rewritten to `{hash}` in the PR that migrates
that block. Until then the shim stays, so the delete is in PR 3, not PR 1.
```

- [ ] **Step 2: Update the three places that referenced in-segment parameters**

In the `## Goal` list, item 1, replace

```
   The only allowed diff is a PR that
   deliberately adds a declaration for a path the block already served, and
   that diff is reviewed line by line.
```

with

```
   The only allowed diffs are a PR that
   deliberately adds a declaration for a path the block already served, and
   PR 1's replacement of `system`'s per-asset lines by one `{filename}` row;
   each such diff is reviewed line by line.
```

In `### 5. Testing`, the **Core** bullet, replace

```
  in-segment `{hash}` matches `app-abc.css` and binds `abc`, rejects
  `app-.css`, rejects a segment with the wrong suffix, and rejects a
  template with two parameters in one segment; the three constructors set
  the auth they name; `normalize_template` no longer exists.
```

with

```
  `schema_of::<T>` produces the same value as `BlockEndpoint::input::<T>`;
  the three constructors set the auth they name and `new` sets `Admin`;
  `normalize_template` no longer exists.
```

In `### 6. Sequencing`, item 1, replace

```
   Extend
   `EndpointRoute`, add `declare` and `schema_of`, add in-segment
   parameters. Migrate `llm` (already has a table; it gains metadata and
   drops its `info()` list) and `system` (the first block whose declaration
   becomes matchable). Both snapshots byte-identical for every block.
```

with

```
   Extend
   `EndpointRoute`, add `declare` and `schema_of`. Migrate `llm` (already has
   a table; it gains metadata and drops its `info()` list) and `system` (one
   `{filename}` row replaces the per-asset declarations, see section 2).
   Both snapshots byte-identical for every block except `system`'s surface
   snapshot, which changes as section 2 describes.
```

- [ ] **Step 3: Add the two PR 7 deletions the plan discovered**

In `### 6. Sequencing`, item 7, replace

```
   `PreparedRoute.router_final`, `EndpointRoute::new`, `dispatch_path`;
```

with

```
   `PreparedRoute.router_final`, `EndpointRoute::new`, `dispatch_path`,
   `util::path_param`;
```

In `### 3. Blocks`, after the paragraph beginning `Templates are written as the path appears on the wire`, add:

```markdown
One path read stays outside the matcher: `llm`'s
`/b/llm/api/internal/default-target`, answered only when `ctx.caller_id()` is
set, is an inter-block call and not an HTTP endpoint. Declaring it would
publish it. It keeps its handler-owned guard, with a comment saying why.
```

- [ ] **Step 4: Commit**

Write the message to a scratchpad file, then run a script that does `git add docs/superpowers/specs/2026-09-05-route-table-single-source-design.md docs/superpowers/plans/2026-09-05-route-table-1-core-llm-system.md` and `git commit -F <file>`:

```
docs: system declares one asset row; plan for phase 1 PR 1

Per-asset `{hash}` rows would make the latin / latin-ext font and the two
logo sizes depend on table order, the hazard the exact-filename lookup
already removed. The system block keeps that lookup behind one
`/b/static/{filename}` row and the matcher needs no new syntax.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01Vfp8g6U7PQTG1kgw2834Tp
```

---

### Task 1: Endpoint-surface snapshot test and baselines

**Files:**
- Create: `crates/impresspress-core/tests/endpoint_surface.rs`
- Create: `crates/impresspress-core/tests/snapshots/*.endpoints.json` (generated)

**Interfaces:**
- Consumes: `impresspress_core::blocks::all_block_infos() -> Vec<wafer_run::BlockInfo>` (exists, generated by `feature_block_manifest!` in `blocks/mod.rs`); `impresspress_core::test_support::real_block_infos()` (exists, `test-support` feature, includes `dev` under `block-dev`).
- Produces: the snapshot files every later task compares against.

- [ ] **Step 1: Write the test**

```rust
//! Per-block endpoint-surface snapshots.
//!
//! One line per `info().endpoints` entry, `METHOD path auth [tool=name]`,
//! sorted. The OpenAPI snapshot beside this one lists only endpoints that
//! carry a schema (`BlockEndpoint::has_schema`), so a page or a schema-less
//! API can be added, dropped or moved to another auth level without it
//! noticing. This file is the contract for the part of the surface the
//! router enforces: which (method, path) pairs a block declares and the
//! level each requires.
//!
//! Regenerate with
//! `UPDATE_OPENAPI_SNAPSHOTS=1 cargo test -p impresspress-core --test endpoint_surface`
//! (and once more with `--features block-dev` for the dev block) and review
//! every changed line: a new line is a path the router now admits at that
//! level, and a changed level is a security decision.

use std::path::PathBuf;

use wafer_run::BlockInfo;

fn snapshot_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots")
}

/// `impresspress/auth-ui` -> `auth_ui`, the same file stem the OpenAPI
/// snapshots use, so the two files for one block sit next to each other.
fn slug(block_name: &str) -> String {
    block_name
        .rsplit('/')
        .next()
        .unwrap_or(block_name)
        .replace('-', "_")
}

fn surface_lines(info: &BlockInfo) -> Vec<String> {
    let mut lines: Vec<String> = info
        .endpoints
        .iter()
        .map(|ep| {
            let mut line = format!("{} {} {}", ep.method, ep.path, ep.auth);
            if let Some(tool) = &ep.agent_tool {
                line.push_str(&format!(" tool={}", tool.name));
            }
            line
        })
        .collect();
    lines.sort();
    lines
}

/// Every block whose `info()` the runtime registers. `all_block_infos` covers
/// the manifest blocks plus `llm`; `dev` has its own constructor and joins
/// only when compiled in.
fn surface_block_infos() -> Vec<BlockInfo> {
    #[allow(unused_mut)]
    let mut infos = impresspress_core::blocks::all_block_infos();
    #[cfg(feature = "block-dev")]
    infos.extend(
        impresspress_core::test_support::real_block_infos()
            .into_iter()
            .filter(|info| info.name == "impresspress/dev"),
    );
    infos
}

#[test]
fn endpoint_surface_matches_committed_snapshots() {
    let updating = std::env::var("UPDATE_OPENAPI_SNAPSHOTS").is_ok();
    std::fs::create_dir_all(snapshot_dir()).expect("create snapshot dir");

    let mut failures = Vec::new();
    for info in surface_block_infos() {
        let mut actual =
            serde_json::to_string_pretty(&surface_lines(&info)).expect("serialize surface");
        actual.push('\n');
        let path = snapshot_dir().join(format!("{}.endpoints.json", slug(&info.name)));

        if updating || !path.exists() {
            std::fs::write(&path, &actual).expect("write snapshot");
            continue;
        }

        let expected = std::fs::read_to_string(&path).expect("read snapshot");
        if expected != actual {
            failures.push(format!(
                "\n=== {} ===\nEndpoint surface differs from {}.\n\
                 Review EVERY changed line: a new line is a path the router now admits at \
                 that level; a changed level is a security decision.\n\
                 Accept with: UPDATE_OPENAPI_SNAPSHOTS=1 cargo test -p impresspress-core \
                 --test endpoint_surface",
                info.name,
                path.display()
            ));
        }
    }

    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn slug_matches_the_openapi_snapshot_stems() {
    assert_eq!(slug("impresspress/auth-ui"), "auth_ui");
    assert_eq!(slug("impresspress/llm"), "llm");
}

#[test]
fn surface_lines_are_sorted_and_carry_the_tool_name() {
    use wafer_run::{AuthLevel, BlockEndpoint};
    let info = BlockInfo::new("impresspress/probe", "0.0.1", "http-handler@v1", "t").endpoints(
        vec![
            BlockEndpoint::post("/b/probe/api/things")
                .auth(AuthLevel::Admin)
                .agent_tool("make_thing", "Makes a thing"),
            BlockEndpoint::get("/b/probe/").auth(AuthLevel::Authenticated),
        ],
    );
    assert_eq!(
        surface_lines(&info),
        vec![
            "GET /b/probe/ authenticated".to_string(),
            "POST /b/probe/api/things admin tool=make_thing".to_string(),
        ]
    );
}
```

- [ ] **Step 2: Run the two unit tests and see them pass, then run the snapshot test once to generate baselines**

Run: `cargo test -p impresspress-core --test endpoint_surface`
Expected: three tests pass; `ls crates/impresspress-core/tests/snapshots/*.endpoints.json` lists `admin`, `auth_ui`, `email`, `files`, `legalpages`, `llm`, `messages`, `products`, `system`, `tickets`, `userportal`, `vector` (12 files; `email` is `[]`).

Run: `cargo test -p impresspress-core --features block-dev --test endpoint_surface`
Expected: pass; `dev.endpoints.json` now also exists.

- [ ] **Step 3: Prove the comparison bites**

Edit `crates/impresspress-core/tests/snapshots/llm.endpoints.json` by hand: change one line's `admin` to `public`. Run `cargo test -p impresspress-core --test endpoint_surface`.
Expected: FAIL with `=== impresspress/llm === Endpoint surface differs`.
Restore the file with `git checkout -- crates/impresspress-core/tests/snapshots/llm.endpoints.json` is NOT possible yet (untracked); instead re-run with `UPDATE_OPENAPI_SNAPSHOTS=1 cargo test -p impresspress-core --test endpoint_surface` and confirm the line is back to `admin`.

- [ ] **Step 4: Sanity-read two baselines**

Open `system.endpoints.json` and confirm it lists `GET /health public` plus per-asset lines such as `GET /b/static/app-{hash}.css public` (8 assets in the base list, 12 with the `block-llm` and `block-files` additions, both default features, so expect 13 lines). Open `llm.endpoints.json` and confirm 18 lines with the five `/b/llm/...` pages at `admin`.

- [ ] **Step 5: Commit**

```
test(core): snapshot every block's declared endpoint surface

The OpenAPI snapshot lists only schema-carrying endpoints, so a page or a
schema-less API can change auth level without it noticing. This records
`METHOD path auth [tool=name]` per block and is the gate the route-table
migration is measured against.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01Vfp8g6U7PQTG1kgw2834Tp
```

Files to add: `crates/impresspress-core/tests/endpoint_surface.rs` and `crates/impresspress-core/tests/snapshots/*.endpoints.json`.

---

### Task 2: `EndpointRoute` carries the declaration; `declare` and the schema producers

> **As executed:** the single `schema_of<T>` below became two producers,
> `request_schema_of<T>` (goes through `BlockEndpoint::input::<T>`, the
> deserialize contract, also what `path_params`/`query_params` use) and
> `response_schema_of<T>` (goes through `BlockEndpoint::output::<T>`, the
> serialize contract). The upstream builders pin draft 2020-12, inline
> subschemas and drop `$schema` in a private function, and the two contracts
> publish different `required` lists for `#[serde(default)]` and
> `skip_serializing_if` fields, so a bare `schema_for!` is not byte-identical.
> The tests in `endpoint_match.rs` prove each producer against its builder on
> a type where the contracts differ. Later tasks use the two names.

**Files:**
- Modify: `crates/impresspress-core/src/endpoint_match.rs:140-161` (the `EndpointRoute` struct and `impl`), plus new items and tests.

**Interfaces:**
- Produces:
  - `pub type SchemaFn = fn() -> serde_json::Value;`
  - `pub fn schema_of<T: schemars::JsonSchema>() -> serde_json::Value`
  - `pub struct EndpointRoute<H> { pub method: HttpMethod, pub template: &'static str, pub handler: H, pub auth: AuthLevel, pub summary: &'static str, pub description: &'static str, pub input: Option<SchemaFn>, pub output: Option<SchemaFn>, pub path_params: Option<SchemaFn>, pub query_params: Option<SchemaFn>, pub tags: &'static [&'static str], pub deprecated: bool, pub agent_tool: Option<(&'static str, &'static str)> }`
  - `impl<H: Copy> EndpointRoute<H>`: `pub const fn public(method, template, handler) -> Self`, `pub const fn authenticated(..)`, `pub const fn admin(..)`, `pub const fn new(..)` (declares `Admin`), and `const fn` builders `summary(&'static str)`, `description(&'static str)`, `input(SchemaFn)`, `output(SchemaFn)`, `path_params(SchemaFn)`, `query_params(SchemaFn)`, `tags(&'static [&'static str])`, `deprecated()`, `agent_tool(&'static str, &'static str)`, each taking `mut self` and returning `Self`.
  - `pub fn declare<H: Copy>(table: &[EndpointRoute<H>]) -> Vec<wafer_run::BlockEndpoint>`
- Unchanged: `dispatch`, `dispatch_path`, `match_template`, `endpoint_auth` signatures.

- [ ] **Step 1: Write the failing tests**

Append inside `mod tests` in `endpoint_match.rs`:

```rust
    fn probe_schema() -> serde_json::Value {
        serde_json::json!({ "type": "object", "properties": { "id": { "type": "string" } } })
    }

    #[test]
    fn constructors_set_the_auth_they_name() {
        assert_eq!(
            EndpointRoute::public(HttpMethod::Get, "/b/x/", 1u8).auth,
            AuthLevel::Public
        );
        assert_eq!(
            EndpointRoute::authenticated(HttpMethod::Get, "/b/x/", 1u8).auth,
            AuthLevel::Authenticated
        );
        assert_eq!(
            EndpointRoute::admin(HttpMethod::Get, "/b/x/", 1u8).auth,
            AuthLevel::Admin
        );
    }

    /// `new` is the dispatch-only constructor the not-yet-migrated tables
    /// still use. If one of those tables ever reaches `declare` by mistake it
    /// must over-protect, never publish a public row by omission.
    #[test]
    fn new_declares_admin() {
        assert_eq!(
            EndpointRoute::new(HttpMethod::Get, "/b/x/", 1u8).auth,
            AuthLevel::Admin
        );
    }

    #[test]
    fn declare_maps_every_row_field() {
        use wafer_run::BlockEndpoint;
        const TABLE: &[EndpointRoute<u8>] = &[EndpointRoute::admin(
            HttpMethod::Post,
            "/b/x/api/things/{id}",
            1u8,
        )
        .summary("Make a thing")
        .description("Longer text")
        .input(probe_schema)
        .output(probe_schema)
        .path_params(probe_schema)
        .query_params(probe_schema)
        .tags(&["x", "things"])
        .deprecated()
        .agent_tool("make_thing", "Makes a thing")];

        let eps: Vec<BlockEndpoint> = declare(TABLE);
        assert_eq!(eps.len(), 1);
        let ep = &eps[0];
        assert_eq!(ep.method, HttpMethod::Post);
        assert_eq!(ep.path, "/b/x/api/things/{id}");
        assert_eq!(ep.auth, AuthLevel::Admin);
        assert_eq!(ep.summary, "Make a thing");
        assert_eq!(ep.description, "Longer text");
        assert_eq!(ep.input_schema, Some(probe_schema()));
        assert_eq!(ep.output_schema, Some(probe_schema()));
        assert_eq!(ep.path_params, Some(probe_schema()));
        assert_eq!(ep.query_params, Some(probe_schema()));
        assert_eq!(ep.tags, vec!["x".to_string(), "things".to_string()]);
        assert!(ep.deprecated);
        let tool = ep.agent_tool.as_ref().expect("agent tool declared");
        assert_eq!(tool.name, "make_thing");
        assert_eq!(tool.description, "Makes a thing");
    }

    /// A row with no metadata must produce exactly what the upstream builders
    /// produce from `BlockEndpoint::get(path)` alone, so a block that only
    /// ever set method, path and auth serializes the same bytes as before.
    #[test]
    fn declare_leaves_unset_metadata_at_the_upstream_defaults() {
        use wafer_run::BlockEndpoint;
        let eps = declare(&[EndpointRoute::public(HttpMethod::Get, "/b/x/", 1u8)]);
        let ep = &eps[0];
        let bare = BlockEndpoint::get("/b/x/");
        assert_eq!(ep.auth, AuthLevel::Public);
        assert_eq!(ep.summary, bare.summary);
        assert_eq!(ep.description, bare.description);
        assert_eq!(ep.input_schema, bare.input_schema);
        assert_eq!(ep.output_schema, bare.output_schema);
        assert_eq!(ep.path_params, bare.path_params);
        assert_eq!(ep.query_params, bare.query_params);
        assert_eq!(ep.tags, bare.tags);
        assert_eq!(ep.deprecated, bare.deprecated);
        assert!(ep.agent_tool.is_none());
    }

    #[test]
    fn declare_preserves_table_order() {
        let eps = declare(&[
            EndpointRoute::public(HttpMethod::Get, "/b/x/api/things", 1u8),
            EndpointRoute::public(HttpMethod::Post, "/b/x/api/things", 2u8),
        ]);
        assert_eq!(eps[0].method, HttpMethod::Get);
        assert_eq!(eps[1].method, HttpMethod::Post);
    }

    #[test]
    fn schema_of_matches_the_upstream_derive() {
        use wafer_run::BlockEndpoint;
        #[derive(schemars::JsonSchema)]
        #[allow(dead_code)]
        struct Probe {
            id: String,
            count: u32,
        }
        assert_eq!(
            schema_of::<Probe>(),
            BlockEndpoint::get("/b/x")
                .input::<Probe>()
                .input_schema
                .expect("upstream derive sets the schema")
        );
    }

    /// Metadata is declaration only; the matcher reads method, template and
    /// handler and nothing else.
    #[test]
    fn dispatch_ignores_row_metadata() {
        let mut msg = Message::new("test");
        msg.set_meta("req.action", "retrieve");
        msg.set_meta("req.resource", "/b/x/api/things/t-1");
        let table = [EndpointRoute::admin(HttpMethod::Get, "/b/x/api/things/{id}", 7u8)
            .summary("s")
            .tags(&["x"])];
        assert_eq!(dispatch(&mut msg, &table), Some(7u8));
        assert_eq!(msg.var("id"), "t-1");
    }
```

- [ ] **Step 2: Run the tests and see them fail to compile**

Run: `cargo test -p impresspress-core --lib endpoint_match::tests`
Expected: compile errors, `no function or associated item named `public``, `no function or associated item named `admin``, `cannot find function `declare``, `cannot find function `schema_of``.

- [ ] **Step 3: Replace the `EndpointRoute` struct and `impl` (lines 140–161) with the declaration-carrying row**

```rust
/// A function that produces a JSON Schema on demand.
///
/// Rows hold one of these instead of a `serde_json::Value` so a block's table
/// can stay a `const`; [`declare`] calls it once per `info()`. For a
/// `schemars` type pass [`schema_of::<T>`] uncalled; for a hand-written
/// schema pass the function that builds it.
pub type SchemaFn = fn() -> serde_json::Value;

/// JSON Schema for `T`, produced exactly as `BlockEndpoint::input::<T>()`
/// produces it, so a row that names `schema_of::<T>` serializes the same
/// bytes the builder did.
pub fn schema_of<T: schemars::JsonSchema>() -> serde_json::Value {
    serde_json::to_value(schemars::schema_for!(T)).unwrap_or(serde_json::Value::Null)
}

/// One row of a block's route table: what `handle()` dispatches on **and**
/// what `info().endpoints` is generated from (see [`declare`]).
///
/// `method`, `template` and `handler` drive matching; everything else is the
/// declaration the router and the OpenAPI/WebMCP projections read. A row
/// always names its [`AuthLevel`] through [`Self::public`],
/// [`Self::authenticated`] or [`Self::admin`]; there is no constructor that
/// defaults to `Public`, because the upstream `BlockEndpoint` default of
/// `Public` is how an unmarked endpoint used to become world-readable by
/// omission.
pub struct EndpointRoute<H> {
    /// HTTP method this route answers (mapped to a wire action internally).
    pub method: HttpMethod,
    /// Path template (`/b/x/{id}`, `/b/x/{rest...}`, …) as it appears on the wire.
    pub template: &'static str,
    /// Block-defined handler discriminator returned to `handle()`.
    pub handler: H,
    /// Level the router enforces before dispatching to this row.
    pub auth: AuthLevel,
    /// Short summary shown in the admin/OpenAPI UI.
    pub summary: &'static str,
    /// Longer description for OpenAPI / docs.
    pub description: &'static str,
    /// Request-body schema producer, if the endpoint takes a body.
    pub input: Option<SchemaFn>,
    /// Response-body schema producer, if the endpoint answers JSON.
    pub output: Option<SchemaFn>,
    /// URL path-parameter schema producer.
    pub path_params: Option<SchemaFn>,
    /// Query-parameter schema producer.
    pub query_params: Option<SchemaFn>,
    /// OpenAPI tags.
    pub tags: &'static [&'static str],
    /// Whether the endpoint is published as deprecated.
    pub deprecated: bool,
    /// `(name, description)` when the endpoint is exposed as a WebMCP tool.
    pub agent_tool: Option<(&'static str, &'static str)>,
}

impl<H: Copy> EndpointRoute<H> {
    const fn with_auth(
        method: HttpMethod,
        template: &'static str,
        handler: H,
        auth: AuthLevel,
    ) -> Self {
        Self {
            method,
            template,
            handler,
            auth,
            summary: "",
            description: "",
            input: None,
            output: None,
            path_params: None,
            query_params: None,
            tags: &[],
            deprecated: false,
            agent_tool: None,
        }
    }

    /// A row anyone may call. Every public row is a decision: the handler
    /// must gate itself by token, signature or shared secret, or need no gate.
    pub const fn public(method: HttpMethod, template: &'static str, handler: H) -> Self {
        Self::with_auth(method, template, handler, AuthLevel::Public)
    }

    /// A row any logged-in caller may call.
    pub const fn authenticated(method: HttpMethod, template: &'static str, handler: H) -> Self {
        Self::with_auth(method, template, handler, AuthLevel::Authenticated)
    }

    /// A row only an admin may call.
    pub const fn admin(method: HttpMethod, template: &'static str, handler: H) -> Self {
        Self::with_auth(method, template, handler, AuthLevel::Admin)
    }

    /// Dispatch-only row for a table that still declares its endpoints by
    /// hand in `info()` rather than through [`declare`]. Declares `Admin`, so
    /// that if such a table does reach `declare` by mistake it over-protects
    /// and shows up in the endpoint-surface snapshot instead of publishing a
    /// public row. Removed once the last such table is migrated.
    pub const fn new(method: HttpMethod, template: &'static str, handler: H) -> Self {
        Self::with_auth(method, template, handler, AuthLevel::Admin)
    }

    /// Set the short summary text.
    pub const fn summary(mut self, summary: &'static str) -> Self {
        self.summary = summary;
        self
    }

    /// Set the longer description text.
    pub const fn description(mut self, description: &'static str) -> Self {
        self.description = description;
        self
    }

    /// Declare the request-body schema.
    pub const fn input(mut self, schema: SchemaFn) -> Self {
        self.input = Some(schema);
        self
    }

    /// Declare the response-body schema.
    pub const fn output(mut self, schema: SchemaFn) -> Self {
        self.output = Some(schema);
        self
    }

    /// Declare the path-parameter schema.
    pub const fn path_params(mut self, schema: SchemaFn) -> Self {
        self.path_params = Some(schema);
        self
    }

    /// Declare the query-parameter schema.
    pub const fn query_params(mut self, schema: SchemaFn) -> Self {
        self.query_params = Some(schema);
        self
    }

    /// Set the OpenAPI tag list.
    pub const fn tags(mut self, tags: &'static [&'static str]) -> Self {
        self.tags = tags;
        self
    }

    /// Publish the endpoint as deprecated.
    pub const fn deprecated(mut self) -> Self {
        self.deprecated = true;
        self
    }

    /// Expose the endpoint as a WebMCP tool with this name and description.
    pub const fn agent_tool(mut self, name: &'static str, description: &'static str) -> Self {
        self.agent_tool = Some((name, description));
        self
    }
}

/// The `BlockEndpoint`s a table declares, in table order, built through the
/// upstream builders so the result is what a hand-written `info()` list
/// produced. Each schema producer is called once.
pub fn declare<H: Copy>(table: &[EndpointRoute<H>]) -> Vec<BlockEndpoint> {
    table
        .iter()
        .map(|row| {
            let mut ep = match row.method {
                HttpMethod::Get => BlockEndpoint::get(row.template),
                HttpMethod::Post => BlockEndpoint::post(row.template),
                HttpMethod::Patch => BlockEndpoint::patch(row.template),
                HttpMethod::Delete => BlockEndpoint::delete(row.template),
            }
            .summary(row.summary)
            .description(row.description)
            .auth(row.auth)
            .tags(row.tags);
            if let Some(schema) = row.input {
                ep = ep.input_schema(schema());
            }
            if let Some(schema) = row.output {
                ep = ep.output_schema(schema());
            }
            if let Some(schema) = row.path_params {
                ep = ep.path_params_schema(schema());
            }
            if let Some(schema) = row.query_params {
                ep = ep.query_params_schema(schema());
            }
            if row.deprecated {
                ep = ep.deprecated();
            }
            if let Some((name, description)) = row.agent_tool {
                ep = ep.agent_tool(name, description);
            }
            ep
        })
        .collect()
}
```

Change the import at the top of the file to `use wafer_run::{AuthLevel, BlockEndpoint, HttpMethod, Message};`.

If the compiler rejects a `const fn` builder with `E0493: destructor of EndpointRoute<H> cannot be evaluated at compile-time`, rewrite each builder as `pub const fn summary(self, summary: &'static str) -> Self { Self { summary, ..self } }` (struct update moves every field, none is dropped).

- [ ] **Step 4: Run the tests and see them pass**

Run: `cargo test -p impresspress-core --lib endpoint_match::tests`
Expected: all pass, including the seven new ones and the 24 that already existed.

- [ ] **Step 5: Confirm nothing else moved**

Run: `cargo test -p impresspress-core --test openapi_snapshot --test endpoint_surface`
Expected: pass (no block calls `declare` yet).

- [ ] **Step 6: Format, lint, commit**

Run: `cargo +nightly fmt -p impresspress-core` then `cargo clippy -p impresspress-core --all-targets -- -D warnings`.

```
refactor(core): let a route row carry its endpoint declaration

`EndpointRoute` gains the auth level, summary, description, schema
producers, tags, deprecation and agent-tool fields a `BlockEndpoint`
carries, all const-constructible, and `declare(&ROUTES)` turns a table
into the `info().endpoints` list. A row names its auth level through
`public` / `authenticated` / `admin`; `new` stays for the tables not yet
migrated and declares `Admin` so a stray call over-protects.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01Vfp8g6U7PQTG1kgw2834Tp
```

---

### Task 3: Migrate `llm` onto the declaring table

**Files:**
- Modify: `crates/impresspress-core/src/blocks/llm/mod.rs:13-24` (imports), `:26-106` (`Route`, `ROUTES`), `:446-568` (`info()` endpoints), `:596-611` (handle comment)
- Modify: `crates/impresspress-core/src/blocks/llm/routes/providers.rs:7-26` (imports, `PROVIDERS_PREFIX`), `:181`, `:253`, `:280` (id reads), `:547-577` (test)
- Modify: `crates/impresspress-core/src/blocks/llm/routes/models.rs:26-45` (`extract_model_path`), `:240-345` (tests)
- Modify: `crates/impresspress-core/src/blocks/llm/routes/test_support.rs` (add `routed`)

**Interfaces:**
- Consumes: `EndpointRoute::{admin, authenticated}`, builders, `declare`, `request_schema_of`, `response_schema_of` from Task 2.
- Produces: `pub(super) fn routed(msg: Message) -> Message` in `llm/routes/test_support.rs` (binds path vars through `blocks::llm::ROUTES`; panics if no row matches).

- [ ] **Step 1: Write the failing table test in `llm/mod.rs`**

Add a new test module at the end of `llm/mod.rs`:

```rust
#[cfg(test)]
mod table_tests {
    use std::sync::Arc;

    use wafer_run::Block as _;

    use super::*;

    /// `info().endpoints` is generated from `ROUTES`; nothing else declares
    /// an endpoint for this block.
    #[test]
    fn info_endpoints_come_from_the_table() {
        let block = LlmBlock::new(Arc::new(provider_admin::NoopProviderAdmin));
        let declared = block.info().endpoints;
        assert_eq!(declared.len(), ROUTES.len());
        for (ep, row) in declared.iter().zip(ROUTES) {
            assert_eq!(ep.method, row.method, "{}", row.template);
            assert_eq!(ep.path, row.template);
            assert_eq!(ep.auth, row.auth, "{}", row.template);
        }
    }
}
```

- [ ] **Step 2: Run it and see it fail**

Run: `cargo test -p impresspress-core --lib blocks::llm::table_tests`
Expected: FAIL. The declared list is ordered API-first while `ROUTES` is pages-first, so the first `zip` pair mismatches on `method`/`path` (or, if the first pair happens to agree, on `auth`).

- [ ] **Step 3: Rewrite `ROUTES` with the metadata from `info()`**

Replace the `ROUTES` const (lines 48–106) with:

```rust
/// The block's HTTP surface: what `handle()` dispatches on and what
/// `info().endpoints` is generated from. Sub-resource templates
/// (`.../discover-models`, `.../load`, `.../status`) precede the generic
/// `.../{id}` / `.../models` templates so the specific route wins.
/// `{id}`/`{backend_id}`/`{model_id}` are bound into `req.param.*`.
///
/// The chat UI is reached from the ADMIN sidebar (nav_groups::admin
/// "Communication" group); the pre-refactor `handle()` gated every non-API
/// page on `is_admin`, so the pages are declared `Admin` to keep that exact
/// outcome as the single, centrally enforced policy.
const ROUTES: &[EndpointRoute<Route>] = &[
    // UI pages
    EndpointRoute::admin(HttpMethod::Get, "/b/llm/", Route::ChatPage).summary("Chat UI"),
    EndpointRoute::admin(HttpMethod::Get, "/b/llm/threads/{id}", Route::ThreadPage)
        .summary("Chat UI (thread permalink)"),
    EndpointRoute::admin(HttpMethod::Get, "/b/llm/settings", Route::SettingsPage)
        .summary("LLM settings page"),
    EndpointRoute::admin(HttpMethod::Get, "/b/llm/providers", Route::ProvidersPage)
        .summary("Providers admin"),
    EndpointRoute::admin(HttpMethod::Get, "/b/llm/models", Route::ModelsPage)
        .summary("Models admin"),
    // Chat API
    EndpointRoute::authenticated(HttpMethod::Post, "/b/llm/api/chat", Route::Chat)
        .summary("Send a chat message")
        .input(request_schema_of::<contracts::ChatRequest>)
        .output(response_schema_of::<contracts::ChatResponse>),
    // Same request as `/api/chat`; the response is `text/event-stream`, one
    // `data:` frame per `ChatChunk`, then `data: [DONE]` (or `event: error`).
    // No `.output(..)`: it would publish an `application/json` schema for a
    // body this endpoint never sends, and the frame type is wafer-run's
    // `ChatChunk`, which carries no JsonSchema derive to mirror.
    EndpointRoute::authenticated(HttpMethod::Post, "/b/llm/api/chat/stream", Route::ChatStream)
        .summary("Send a chat message (SSE streaming)")
        .input(request_schema_of::<contracts::ChatRequest>),
    // Provider CRUD (specific sub-resource first)
    EndpointRoute::admin(
        HttpMethod::Post,
        "/b/llm/api/providers/{id}/discover-models",
        Route::DiscoverModels,
    )
    .summary("Discover provider models via /v1/models")
    .path_params(provider_id_path_schema)
    .output(response_schema_of::<contracts::DiscoveredModelsResponse>),
    EndpointRoute::admin(HttpMethod::Get, "/b/llm/api/providers", Route::ListProviders)
        .summary("List configured LLM providers")
        .output(response_schema_of::<contracts::ProviderListResponse>),
    EndpointRoute::admin(HttpMethod::Post, "/b/llm/api/providers", Route::CreateProvider)
        .summary("Create LLM provider")
        .input(request_schema_of::<contracts::CreateProviderRequest>)
        .output(response_schema_of::<contracts::ProviderView>),
    EndpointRoute::admin(HttpMethod::Patch, "/b/llm/api/providers/{id}", Route::UpdateProvider)
        .summary("Update LLM provider")
        .path_params(provider_id_path_schema)
        .input(request_schema_of::<contracts::UpdateProviderRequest>)
        .output(response_schema_of::<contracts::ProviderView>),
    EndpointRoute::admin(HttpMethod::Delete, "/b/llm/api/providers/{id}", Route::DeleteProvider)
        .summary("Delete LLM provider")
        .path_params(provider_id_path_schema)
        .output(response_schema_of::<contracts::ProviderDeleteResponse>),
    // Models (specific sub-resources first)
    EndpointRoute::authenticated(
        HttpMethod::Get,
        "/b/llm/api/models/{backend_id}/{model_id}/status",
        Route::ModelStatus,
    )
    .summary("Model status (ready / loading / unloaded)")
    .path_params(model_path_schema)
    .output(response_schema_of::<contracts::ModelStatusResponse>),
    // Takes no body; answers `text/event-stream`, one `data:` frame per
    // `LoadProgress`, then `data: [DONE]`. No `.output(..)` for the same
    // reason as `/api/chat/stream`.
    EndpointRoute::admin(
        HttpMethod::Post,
        "/b/llm/api/models/{backend_id}/{model_id}/load",
        Route::LoadModel,
    )
    .summary("Load a model (SSE progress)")
    .path_params(model_path_schema),
    EndpointRoute::admin(
        HttpMethod::Post,
        "/b/llm/api/models/{backend_id}/{model_id}/unload",
        Route::UnloadModel,
    )
    .summary("Unload a model")
    .path_params(model_path_schema)
    .output(response_schema_of::<contracts::ModelUnloadResponse>),
    EndpointRoute::authenticated(HttpMethod::Get, "/b/llm/api/models", Route::ListModels)
        .summary("List available models (aggregated across backends)")
        .output(response_schema_of::<contracts::ModelListResponse>),
    // Config
    EndpointRoute::authenticated(HttpMethod::Get, "/b/llm/api/config", Route::GetConfig)
        .summary("Get default provider/model config")
        .output(response_schema_of::<contracts::LlmConfigResponse>),
    EndpointRoute::authenticated(HttpMethod::Post, "/b/llm/api/config", Route::PostConfig)
        .summary("Update per-thread provider/model override")
        .input(request_schema_of::<contracts::ConfigUpdateRequest>)
        .output(response_schema_of::<contracts::ConfigUpdateResponse>),
];
```

Change the import block to `use crate::{endpoint_match::{self, request_schema_of, response_schema_of, EndpointRoute}, http::{err_bad_request, err_internal, err_not_found, ok_json}, util::json_map};` and drop `BlockEndpoint` from the `wafer_run` import (keep `HttpMethod`).

**If PR #8 has merged**, `Route` also has `DeleteConfig` and `info()` has a `BlockEndpoint::delete("/b/llm/api/config/{id}")` entry with `.auth(AuthLevel::Authenticated)`, `.path_params_schema(override_id_path_schema())` and `.output::<contracts::ConfigDeleteResponse>()`. Carry it as the last row, copying #8's summary text verbatim:

```rust
    EndpointRoute::authenticated(HttpMethod::Delete, "/b/llm/api/config/{id}", Route::DeleteConfig)
        .summary("<summary text from PR #8>")
        .path_params(override_id_path_schema)
        .output(response_schema_of::<contracts::ConfigDeleteResponse>),
```

and in `handle_delete_config` replace `crate::util::path_param(msg, "id", "/b/llm/api/config/")` with `msg.var("id")`.

- [ ] **Step 4: Replace the hand-written `endpoints(vec![...])` in `info()` with the table**

Replace lines 472–568 (from `.endpoints(vec![` through the closing `])` before `.config_keys(`) with:

```rust
        .endpoints(endpoint_match::declare(ROUTES))
```

Delete the now-unused `use wafer_run::AuthLevel;` at the top of `info()`. Update the comment above `provider_id_path_schema` so it no longer mentions `path_param`: replace the sentence `every handler reads the id with `path_param(msg, "id", ..)` by name` with `every handler reads the id with `msg.var("id")` by name`; do the same for the `model_path_schema` comment (`routes::models::extract_model_path` reads both by name` is still true).

Update the comment above the `dispatch` call in `handle` (`bound into req.param.* for the handlers' path_param readers`) to `bound into req.param.* for the handlers' msg.var readers`.

- [ ] **Step 5: Run the table test and both snapshot tests**

Run: `cargo test -p impresspress-core --lib blocks::llm::table_tests`
Expected: PASS.

Run: `cargo test -p impresspress-core --test openapi_snapshot --test endpoint_surface`
Expected: PASS with no diff. If `llm.openapi.json` differs, do not regenerate. Diff the failure output: a schema difference means a `request_schema_of::<T>` or `response_schema_of::<T>` names the wrong `T` or the wrong contract or a `.path_params(..)`/`.input(..)`/`.output(..)` is missing from one row; compare the row against the deleted `info()` entry for the same path and fix the row.

- [ ] **Step 6: Write the failing test for `routed` in `llm/routes/test_support.rs`**

Append to `llm/routes/test_support.rs`:

```rust
/// Run `msg` through the block's own route table so `{id}` / `{backend_id}`
/// / `{model_id}` are bound the way they are on the wire, then hand the
/// message to a handler directly. Panics when no row matches: a test that
/// sends an unroutable path would otherwise exercise the handler's
/// "missing id" branch by accident.
pub(super) fn routed(mut msg: Message) -> Message {
    let route = crate::endpoint_match::dispatch(&mut msg, crate::blocks::llm::ROUTES);
    assert!(
        route.is_some(),
        "no llm route matches {} {}",
        msg.action(),
        msg.path()
    );
    msg
}
```

(`ROUTES` is private to `blocks::llm`; `routes::test_support` is a descendant module, so it can name it.)

- [ ] **Step 7: Replace `extract_provider_id_from_path` in `providers.rs` with the table-binding test**

Replace the test at lines 547–577 with:

```rust
    /// Provider handlers read the id the table bound, nothing else.
    #[test]
    fn provider_id_is_bound_by_the_table() {
        use crate::blocks::llm::routes::test_support::routed;
        let m = routed(admin_msg("update", "/b/llm/api/providers/abc123"));
        assert_eq!(m.var("id"), "abc123");

        let m2 = routed(admin_msg(
            "create",
            "/b/llm/api/providers/abc123/discover-models",
        ));
        assert_eq!(m2.var("id"), "abc123");

        // A path with no id segment matches no row and binds nothing; the
        // handler then answers InvalidArgument (see `update_provider_requires_id`).
        let mut m3 = admin_msg("delete", "/b/llm/api/providers/");
        assert!(crate::endpoint_match::dispatch(&mut m3, crate::blocks::llm::ROUTES).is_none());
        assert_eq!(m3.var("id"), "");
    }
```

- [ ] **Step 8: Run providers tests and see the new one pass while the wire-shape tests still pass through `path_param`'s fallback**

Run: `cargo test -p impresspress-core --lib blocks::llm::routes::providers`
Expected: PASS (nothing has changed in the handlers yet; `provider_id_is_bound_by_the_table` passes because `dispatch` already binds).

- [ ] **Step 9: Switch the three provider handlers to `msg.var("id")` and watch the direct-call tests fail**

In `providers.rs` replace each `let id = path_param(msg, "id", PROVIDERS_PREFIX).to_string();` (lines 181, 253, 280) with `let id = msg.var("id").to_string();`. Delete `const PROVIDERS_PREFIX: &str = "/b/llm/api/providers/";` and its doc comment, and remove `util::path_param,` from the `use crate::{...}` import.

Run: `cargo test -p impresspress-core --lib blocks::llm::routes::providers`
Expected: FAIL in `update_provider_rejects_an_empty_protocol`, `update_provider_refuses_an_inline_api_key`, `provider_endpoints_never_emit_the_resolved_api_key` and `provider_endpoints_publish_exactly_the_view_fields`: the handler now answers `InvalidArgument: Missing provider ID` because these tests build the message by hand and never ran dispatch. `update_provider_requires_id` and `delete_provider_requires_id` still pass (they send no id and expect that error).

- [ ] **Step 10: Route the direct-call tests through the table**

In `providers.rs` tests, wrap every message whose path carries an id in `routed(..)`. Add `routed` to the `routes::test_support::{...}` import at the top of `mod tests`. The changes:

- line 413 and line 496: `let msg = routed(admin_msg("update", "/b/llm/api/providers/row-1"));`
- line 671 and line 749: `&routed(admin_msg("update", &format!("/b/llm/api/providers/{id}"))),`
- the `discover_models` call near line 760: `&routed(admin_msg("create", &format!("/b/llm/api/providers/{id}/discover-models"))),`
- line 788: `&routed(admin_msg("delete", &format!("/b/llm/api/providers/{id}"))),`

Any `list_providers` / `create_provider` call (`/b/llm/api/providers`, no id) stays as it is.

Run: `cargo test -p impresspress-core --lib blocks::llm::routes::providers`
Expected: PASS.

- [ ] **Step 11: Make `extract_model_path` read only the bound variables, test-first**

Replace the test `extract_model_path_from_suffix` (models.rs, from line 305 to the end of that test) with:

```rust
    /// Model handlers read the ids the table bound, nothing else.
    #[test]
    fn model_path_is_bound_by_the_table() {
        use crate::blocks::llm::routes::test_support::routed;
        let m = routed(user_msg("retrieve", "/b/llm/api/models/openai/gpt-4o/status"));
        assert_eq!(
            extract_model_path(&m),
            ("openai".to_string(), "gpt-4o".to_string())
        );

        // A model id with dots and dashes is one segment.
        let m2 = routed(admin_msg("create", "/b/llm/api/models/webllm/llama-3.1-8b/load"));
        assert_eq!(
            extract_model_path(&m2),
            ("webllm".to_string(), "llama-3.1-8b".to_string())
        );

        // Missing model id: no row matches, nothing is bound, the handler
        // answers InvalidArgument (see `unload_model_requires_path_vars`).
        let mut m3 = admin_msg("create", "/b/llm/api/models/openai/");
        assert!(crate::endpoint_match::dispatch(&mut m3, crate::blocks::llm::ROUTES).is_none());
        assert_eq!(extract_model_path(&m3), (String::new(), String::new()));
    }
```

Run: `cargo test -p impresspress-core --lib blocks::llm::routes::models::tests::model_path_is_bound_by_the_table`
Expected: FAIL on the third assertion: the fallback still splits the path and returns `("openai", "")`.

Replace `extract_model_path` (models.rs lines 26–45) with:

```rust
/// `(backend_id, model_id)` as bound by the block's route table for
/// `/b/llm/api/models/{backend_id}/{model_id}/...`. Either is empty when the
/// request matched no row.
fn extract_model_path(msg: &Message) -> (String, String) {
    (
        msg.var("backend_id").to_string(),
        msg.var("model_id").to_string(),
    )
}
```

In the `unload_model_acknowledges` test (line 249) wrap the message: `&routed(admin_msg("create", "/b/llm/api/models/openai-main/gpt-4o/unload")),`. Add `routed` to that test module's `test_support` import.

Run: `cargo test -p impresspress-core --lib blocks::llm::routes::models`
Expected: PASS (`load_model_requires_path_vars`, `unload_model_requires_path_vars`, `model_status_requires_path_vars` still pass: their paths match no row, so nothing is bound and the handler rejects).

- [ ] **Step 12: Whole-crate check and the grep gate for llm**

Run: `cargo test -p impresspress-core --no-fail-fast`
Expected: everything passes except the known `lockfile_loads_remote_block`.

Run: `grep -rn 'path_param(\|strip_prefix("/b\|starts_with("/b' crates/impresspress-core/src/blocks/llm/`
Expected: no output.

Run: `grep -n 'internal/default-target' crates/impresspress-core/src/blocks/llm/mod.rs`
Expected: the one guard in `handle`, whose comment already says it is not a declared HTTP endpoint. Extend that comment's last sentence to: `It is NOT a declared HTTP endpoint (declaring it would publish it), so it stays a handler-owned guard ahead of the matcher; this is the one path read in this block outside \`endpoint_match::dispatch\`.`

- [ ] **Step 13: Format, lint, commit**

Run: `cargo +nightly fmt -p impresspress-core` then `cargo clippy -p impresspress-core --all-targets -- -D warnings`.

```
refactor(llm): declare the HTTP surface from the route table

`ROUTES` now carries the summary, auth level and schemas that `info()`
listed by hand, and `info()` is `declare(ROUTES)`. Handlers read `{id}`,
`{backend_id}` and `{model_id}` only as the table bound them; the
`path_param` prefix-strip fallback and `PROVIDERS_PREFIX` go. Handler
tests that build a message by hand run it through the real table first.
OpenAPI and endpoint-surface snapshots unchanged.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01Vfp8g6U7PQTG1kgw2834Tp
```

---

### Task 4: Migrate `system` onto a two-row table

**Files:**
- Modify: `crates/impresspress-core/src/blocks/system.rs` (whole file except `static_asset` and the four `system_handle_serves_*` tests)
- Modify: `crates/impresspress-core/tests/snapshots/system.endpoints.json` (regenerated; exact content below)

**Interfaces:**
- Consumes: `EndpointRoute::public`, `declare`, `dispatch` from Task 2; `crate::ui::assets::{ASSETS, AssetEntry}` (fields `logical`, `filename`, `hash`, `content_type`, `len`; generated by `build.rs`); `routing::STATIC_PREFIX`.
- Produces: nothing other blocks use.

- [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `system.rs` (these use `NopCtx`, already defined there):

```rust
    use wafer_run::HttpMethod;

    /// `info().endpoints` is generated from `ROUTES`.
    #[test]
    fn info_endpoints_come_from_the_table() {
        let declared = SystemBlock::new().info().endpoints;
        assert_eq!(declared.len(), ROUTES.len());
        for (ep, row) in declared.iter().zip(ROUTES) {
            assert_eq!(ep.method, row.method, "{}", row.template);
            assert_eq!(ep.path, row.template);
            assert_eq!(ep.auth, row.auth, "{}", row.template);
        }
    }

    /// The asset row's template is the same literal the URL builders use.
    #[test]
    fn asset_row_sits_under_the_static_prefix() {
        let row = ROUTES
            .iter()
            .find(|r| matches!(r.handler, Route::Asset))
            .expect("asset row");
        assert_eq!(row.method, HttpMethod::Get);
        assert_eq!(row.template, "/b/static/{filename}");
        assert!(row.template.starts_with(routing::STATIC_PREFIX));
        assert_eq!(row.auth, wafer_run::AuthLevel::Public);
    }

    /// Every file in the build-time manifest resolves to the asset row with
    /// its exact filename bound, whatever the hash happens to be.
    #[test]
    fn every_manifest_asset_dispatches_to_the_asset_row() {
        for entry in crate::ui::assets::ASSETS {
            let url = format!("{}{}", routing::STATIC_PREFIX, entry.filename);
            let mut msg = Message::new(format!("retrieve:{url}"));
            msg.set_meta(wafer_run::META_REQ_ACTION, "retrieve");
            msg.set_meta(wafer_run::META_REQ_RESOURCE, &url);
            let route = crate::endpoint_match::dispatch(&mut msg, ROUTES);
            assert!(matches!(route, Some(Route::Asset)), "{url}");
            assert_eq!(msg.var("filename"), entry.filename);
        }
    }

    /// A URL with a stale hash names a file that is not in the manifest and
    /// must 404, never receive the current bytes under an `immutable` header.
    #[tokio::test]
    #[cfg(feature = "embed-assets")]
    async fn a_stale_hash_is_not_found() {
        let block = SystemBlock::new();
        let url = "/b/static/app-0000000000000000.css";
        let mut msg = Message::new(format!("retrieve:{url}"));
        msg.set_meta(wafer_run::META_REQ_ACTION, "retrieve");
        msg.set_meta(wafer_run::META_REQ_RESOURCE, url);
        let out = block.handle(&NopCtx, msg, InputStream::empty()).await;
        assert!(crate::test_support::output_is_error(out, "NotFound").await);
    }

    /// Nothing under `/b/static/` with more than one segment is served, so a
    /// traversal never reaches the manifest lookup.
    #[test]
    fn a_nested_static_path_matches_no_row() {
        let url = "/b/static/../../etc/passwd";
        let mut msg = Message::new(format!("retrieve:{url}"));
        msg.set_meta(wafer_run::META_REQ_ACTION, "retrieve");
        msg.set_meta(wafer_run::META_REQ_RESOURCE, url);
        assert!(crate::endpoint_match::dispatch(&mut msg, ROUTES).is_none());
    }
```

Also change `webmcp_script_asset_is_publicly_reachable` to look the endpoint up by the new template: replace `.find(|e| e.path == "/b/static/webmcp-{hash}.js")` with `.find(|e| e.path == "/b/static/{filename}")` and the `expect` text with `"static asset endpoint not declared in SystemBlock::info()"`. Its doc comment's first sentence mentions the per-asset list; replace `this is the endpoint declaration this block adds alongside htmx/CSS/fonts (`system.rs`'s `endpoints` list)` with `the one asset row in `ROUTES` is that declaration`.

- [ ] **Step 2: Run and see them fail to compile**

Run: `cargo test -p impresspress-core --lib blocks::system::tests`
Expected: compile errors, `cannot find value `ROUTES``, `cannot find type `Route``.

- [ ] **Step 3: Rewrite the block**

Replace everything in `system.rs` from the `use` lines through the end of the `impresspress_feature_block!` invocation (keep `static_asset` as it is) with:

```rust
use wafer_run::{BlockInfo, HttpMethod, InstanceMode, OutputStream};

use crate::{
    endpoint_match::{self, EndpointRoute},
    http::{err_not_found, ok_json, ResponseBuilder},
    routing,
};

/// Resolve a hashed filename (the part after `/b/static/`) to its bytes and
/// content type. Exact-match against the build-time manifest — no prefix
/// scanning, so no ordering hazard between `itim-latin-` and `itim-latin-ext-`.
#[cfg(feature = "embed-assets")]
pub(crate) fn static_asset(filename: &str) -> Option<(&'static [u8], &'static str)> {
    let e = crate::ui::assets::ASSETS
        .iter()
        .find(|e| e.filename == filename)?;
    Some((crate::ui::assets::bytes(e.logical)?, e.content_type))
}

#[derive(Clone, Copy)]
enum Route {
    Health,
    /// One embedded asset, addressed by its content-hashed filename.
    Asset,
}

/// The block's HTTP surface. Every embedded asset (CSS, htmx, the WebMCP
/// script, fonts, logos, favicon) is served from the one `{filename}` row:
/// filenames are content-hashed (`app-{hash}.css`), the lookup is by exact
/// filename against the build-time manifest, and a stale hash is therefore a
/// 404. One row rather than one per asset keeps `itim-latin-` /
/// `itim-latin-ext-` (and the two logo sizes) from depending on table order,
/// which a per-asset `{hash}` template would reintroduce.
const ROUTES: &[EndpointRoute<Route>] = &[
    EndpointRoute::public(HttpMethod::Get, "/health", Route::Health).summary("Health check"),
    EndpointRoute::public(HttpMethod::Get, "/b/static/{filename}", Route::Asset)
        .summary("Embedded static asset (content-hashed filename)"),
];

/// Serve the manifest entry named `filename`, or 404.
fn serve_asset(filename: &str) -> OutputStream {
    #[cfg(feature = "embed-assets")]
    if let Some((body, content_type)) = static_asset(filename) {
        return ResponseBuilder::new()
            .set_header("Cache-Control", "public, max-age=31536000, immutable")
            .body(body.to_vec(), content_type);
    }
    // Either the filename is not in the manifest (a stale or made-up hash),
    // or assets were not compiled in. In the second case the deployer is
    // responsible for publishing them and pointing IMPRESSPRESS_ASSET_BASE_URL
    // at them; reaching this arm means that did not happen.
    #[cfg(not(feature = "embed-assets"))]
    let _ = filename;
    err_not_found("not found")
}

crate::impresspress_feature_block! {
    /// System health checks and embedded static assets (`impresspress/system`).
    pub struct SystemBlock;
    name: "impresspress/system",
    info: |_this| {
        BlockInfo::new("impresspress/system", "0.0.1", "http-handler@v1", "System health and embedded static assets")
            .instance_mode(InstanceMode::Singleton)
            .category(wafer_run::BlockCategory::Infrastructure)
            .description("Core system services including health checks and embedded static assets (CSS, JavaScript).")
            .endpoints(endpoint_match::declare(ROUTES))
    },
    handle: |_this, _ctx, msg, _input| {
        let Some(route) = endpoint_match::dispatch(&mut msg, ROUTES) else {
            return err_not_found("not found");
        };
        match route {
            Route::Health => ok_json(&serde_json::json!({"status": "ok"})),
            Route::Asset => serve_asset(msg.var("filename")),
        }
    },
}
```

Notes for the implementer: the macro binds `msg` as `let mut msg = msg;`, so `&mut msg` compiles. `ResponseBuilder` and `err_not_found` are already imported today. If `ok_json` needs a different import path, keep whatever the current file imports.

- [ ] **Step 4: Run the system tests**

Run: `cargo test -p impresspress-core --lib blocks::system`
Expected: PASS, including the four `system_handle_serves_*` tests (they send real hashed URLs through `handle`, which now dispatches to `Route::Asset`).

- [ ] **Step 5: Regenerate the system surface snapshot and check it is exactly the two lines**

Run: `cargo test -p impresspress-core --test endpoint_surface`
Expected: FAIL for `impresspress/system` only.

Run: `UPDATE_OPENAPI_SNAPSHOTS=1 cargo test -p impresspress-core --test endpoint_surface`, then `cat crates/impresspress-core/tests/snapshots/system.endpoints.json`.
Expected content:

```json
[
  "GET /b/static/{filename} public",
  "GET /health public"
]
```

Run: `git status --short crates/impresspress-core/tests/snapshots/`
Expected: only `system.endpoints.json` modified. If any other snapshot changed, the regenerate touched something it should not have: `git checkout -- <that file>` and find out why before continuing.

- [ ] **Step 6: Routing tests still hold**

Run: `cargo test -p impresspress-core --lib routing::tests`
Expected: PASS. `anonymous_static_asset_request_is_not_denied` and `webmcp_script_asset_is_publicly_reachable` pass no `BlockInfo` and rely on the still-present `router_declared_public(STATIC_PREFIX, ..)` carve-out, which PR 7 removes; nothing in this task touches `routing.rs`.

- [ ] **Step 7: Grep gate and full run**

Run: `grep -n 'path_param(\|strip_prefix(\|starts_with(' crates/impresspress-core/src/blocks/system.rs`
Expected: no output.

Run: `cargo test -p impresspress-core --no-fail-fast`
Expected: everything passes except the known `lockfile_loads_remote_block`.

- [ ] **Step 8: Format, lint, commit**

Run: `cargo +nightly fmt -p impresspress-core` then `cargo clippy -p impresspress-core --all-targets -- -D warnings`.

```
refactor(system): serve assets from one declared {filename} row

The per-asset `app-{hash}.css` declarations could never match a request
(`{hash}` binds a whole segment), so the router needed a carve-out to
keep assets public. One `GET /b/static/{filename}` row is matchable,
declared public, and keeps the exact-filename manifest lookup, so a
stale hash stays a 404 and the latin / latin-ext fonts and the two logo
sizes cannot depend on table order. The system endpoint-surface
snapshot changes from thirteen lines (`/health` plus twelve assets) to two.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01Vfp8g6U7PQTG1kgw2834Tp
```

Files to add: `crates/impresspress-core/src/blocks/system.rs`, `crates/impresspress-core/tests/snapshots/system.endpoints.json`.

---

### Task 5: Verify and open the PR

**Files:** none new.

- [ ] **Step 1: Full verification**

Run each, from the worktree root:

```
cargo +nightly fmt --all -- --check
cargo clippy -p impresspress-core --all-targets -- -D warnings
cargo test -p impresspress-core --no-fail-fast
cargo test -p impresspress-core --features block-dev --test endpoint_surface --test openapi_snapshot
```

Expected: fmt clean; clippy clean; tests all pass except `lockfile_loads_remote_block`; both snapshot tests pass under `block-dev` too.

Run: `git status --short`
Expected: clean (every snapshot the `block-dev` run touched was already committed in Task 1; if `dev.endpoints.json` shows as modified, the dev block's declared surface changed in this PR, which it must not have: investigate).

Run: `git diff origin/main --stat -- crates/impresspress-core/tests/snapshots/`
Expected: only `*.endpoints.json` files added, and `system.endpoints.json` is among the added files (it was created in Task 1 and modified in Task 4, so it shows as added relative to `main`). No `*.openapi.json` appears.

- [ ] **Step 2: Push and open the PR against the fork's `main`**

Use the scratchpad `ship-branch.sh` pattern (`git push -u origin phase1/route-table-core` then `gh pr create --base main --title ... --body-file ...`). Title:

`refactor(core): route rows carry their endpoint declaration; migrate llm and system`

Body:

```markdown
Phase 1, PR 1 of 7 of `docs/superpowers/specs/2026-09-05-route-table-single-source-design.md`.

## What changes

- `endpoint_match::EndpointRoute<H>` carries the auth level, summary, description, schema producers, tags, deprecation and agent-tool a `BlockEndpoint` carries. Rows name their level through `public` / `authenticated` / `admin`. `declare(&ROUTES)` generates `info().endpoints`.
- New `tests/endpoint_surface.rs` snapshots every block's `METHOD path auth [tool]` lines. This is the gate the migration is measured against; the OpenAPI snapshot only sees schema-carrying endpoints.
- `llm`: `ROUTES` is the declaration; handlers read bound path variables only. OpenAPI and surface snapshots byte-identical.
- `system`: one `GET /b/static/{filename}` row replaces twelve per-asset declarations that no request could match. Exact-filename manifest lookup kept, so a stale hash is still a 404.

## Surface snapshot changes

Only `system.endpoints.json`, thirteen lines to two:

- `GET /b/static/{filename} public`
- `GET /health public`

Every other `*.endpoints.json` is a new baseline equal to what the block declared on `main`.

## Spec amendment

Section 2 of the spec now says the system block declares one row instead of the matcher learning in-segment `{hash}` parameters: per-asset rows would make the latin / latin-ext fonts and the two logo sizes depend on table order, the hazard the exact lookup removed.

## Not in this PR

The `router_declared_public(STATIC_PREFIX, ..)` carve-out stays until PR 7 removes `router_final`. `EndpointRoute::new` stays (declares `Admin`) until the other tables migrate.

🤖 Generated with [Claude Code](https://claude.com/claude-code)

https://claude.ai/code/session_01Vfp8g6U7PQTG1kgw2834Tp
```

- [ ] **Step 3: Record the PR number**

Add a row to section "7a. Phase 0 status" of `docs/CODE_REVIEW_2026-09-05.md` is NOT required (that table is Phase 0). Instead update the memory file `phase0-review-fixes.md` under the session's memory directory: Phase 1 PR 1 opened as `#<n>` on branch `phase1/route-table-core`; PRs 2–7 follow, each with its own plan.

---

## Self-review

**Spec coverage (PR 1 scope):**
- Section 1 (row fields, constructors, builders, `schema_of`, `declare`, `new` declares Admin, `dispatch` unchanged): Task 2.
- Section 2 as amended (one system row, no new syntax): Task 0 and Task 4.
- Section 3 for `llm` (wire paths, `msg.var` only, internal default-target guard kept and commented): Task 3.
- Section 5 surface snapshot test and baseline-first ordering: Task 1 runs before any migration. OpenAPI snapshot unchanged: checked in Tasks 2, 3, 5.
- Section 6 item 1 deliverables: Tasks 2–4. Carve-out left in place: Task 4 Step 6.
- Not in PR 1 by design: `normalize_template` deletion (PR 3), router changes (PR 7), other blocks (PRs 2–6).

**Placeholder scan:** the only placeholder is `<summary text from PR #8>` in Task 3 Step 3, which is conditional on an external merge and tells the implementer exactly where to copy the text from. `#<n>` in Task 5 Step 3 is the PR number that does not exist until Step 2 runs.

**Type consistency:** `SchemaFn`, `schema_of`, `declare`, `EndpointRoute::{public, authenticated, admin, new}`, builders `.summary/.description/.input/.output/.path_params/.query_params/.tags/.deprecated/.agent_tool` are used with the same names and signatures in Tasks 2, 3 and 4. `routed(msg: Message) -> Message` is defined in Task 3 Step 6 and used in Steps 7, 10, 11. `Route::Asset` is a unit variant in Task 4 throughout. `ROUTES` is a private `const` in both blocks; `routes::test_support` reaches `blocks::llm::ROUTES` as a descendant module.
