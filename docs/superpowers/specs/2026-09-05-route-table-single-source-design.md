# One route table per block

**Date:** 2026-09-05
**Status:** Implemented (PRs #12–#17 and this one, PR 7: router cleanup)
**Repo:** `impresspress` only (no wafer-run change)
**Origin:** Phase 1 of `docs/CODE_REVIEW_2026-09-05.md` (section 7)

## Goal

Every feature block keeps exactly one description of its HTTP surface: a
`const` table of rows, each row naming the method, the wire path template,
the handler the block runs, the auth level the router enforces, and the
OpenAPI metadata the block publishes. That table is what `handle()` dispatches
on and what `info().endpoints` is generated from. Nothing else in the block
matches a path.

Success is measured three ways, in this order:

1. Two per-block snapshots under `crates/impresspress-core/tests/snapshots/`
   are byte-identical before and after each PR: the existing OpenAPI
   snapshot (the schema contract) and a new endpoint-surface snapshot (the
   auth contract, see Testing). The only allowed diffs are a PR that
   deliberately adds a declaration for a path the block already served, and
   PR 1's replacement of `system`'s per-asset lines by one `{filename}` row;
   each such diff is reviewed line by line.
2. `grep` finds no `starts_with("/b`, `strip_prefix("/b`, hand-written
   `match (action, path)` arms, or `path_param(` calls in
   `crates/impresspress-core/src/blocks/`. Only the shared matcher reads a
   path.
3. `routing.rs` no longer has `Route::router_declared_public`, the
   `router_final` field, or a per-path carve-out for a block. Its table is
   one prefix per block plus the inspector proxy.

## Why

The review found the same path spelled in up to four places per block: the
`BlockEndpoint` declaration in `info()`, a `starts_with` guard chain or
`match` arm in `handle()`, a `path_param` prefix string in the handler, and
sometimes a `router_declared_public` carve-out in `routing.rs`. They have
drifted. Today on `main`:

- `auth-ui` declares 18 method-and-path pairs and serves 29
  (`blocks/auth_ui/mod.rs`, declarations at lines 198–276, arms at
  314–376). Nine of the eleven undeclared pairs are kept reachable by eight
  `router_declared_public` carve-outs in `routing.rs`; the other two (API-key
  revoke and delete) are reachable only because the router's fail-closed
  default happens to be the level the handler wants. The block also declares
  `/b/auth/...` and then strips `/b` before matching on `/auth/...`, so the
  declared string and the matched string are never the same literal, and
  its rate-limit table matches the stripped form a third time.
- `files` declares `/b/storage/...` and `/b/cloudstorage/` but serves
  `/b/cloudstorage/shares`, `/b/cloudstorage/quota`,
  `/admin/b/cloudstorage/...` and `/admin/storage/...` from `cloud.rs` and
  `storage/admin.rs`. The last two are not wire paths at all: the admin block
  receives `/b/admin/api/cloudstorage/...` and `/b/admin/api/storage/...`,
  rewrites `req.resource`, and forwards through `call_block`.
- `system` declares `/b/static/app-{hash}.css` but no request can ever match
  it, because `{hash}` only binds a whole segment. The router papers over
  that with `router_declared_public(STATIC_PREFIX, ...)`.
- `products` declares `/b/products/webhooks` as Public and additionally
  carries a `router_declared_public` carve-out for it, "for boot or tests
  where BlockInfo metadata is unavailable".
- `userportal` declares `/b/userportal/sessions/:hash` in colon style, and
  the matcher keeps a `normalize_template` shim just to translate it.

Each of those is a place a security decision can be made twice with two
answers. The router's `endpoint_auth` takes the strictest match, which fails
safe, but "fails safe" here means an anonymous asset request 403s unless a
carve-out exists, and a carve-out is a path the router admits without the
block having said so.

## Design

### 1. Core: `EndpointRoute<H>` becomes the declaration

`crates/impresspress-core/src/endpoint_match.rs` already has
`EndpointRoute<H> { method, template, handler }` and `dispatch()`. The row
grows to carry everything `BlockEndpoint` carries, all `const`-constructible:

```rust
pub struct EndpointRoute<H> {
    pub method: HttpMethod,
    pub template: &'static str,
    pub handler: H,
    pub auth: AuthLevel,
    pub summary: &'static str,
    pub description: &'static str,
    pub input: Option<fn() -> serde_json::Value>,
    pub output: Option<fn() -> serde_json::Value>,
    pub path_params: Option<fn() -> serde_json::Value>,
    pub query_params: Option<fn() -> serde_json::Value>,
    pub tags: &'static [&'static str],
    pub deprecated: bool,
    pub agent_tool: Option<(&'static str, &'static str)>,
}
```

Rules for the row:

- `auth` is required. The constructors are `EndpointRoute::public(method,
  template, handler)`, `::authenticated(...)`, `::admin(...)`. There is no
  constructor that defaults to `Public`, because the upstream
  `BlockEndpoint::auth` default of `Public` is how an unmarked endpoint became
  world-readable by omission. The existing `EndpointRoute::new(method,
  template, handler)` has nine callers today (every block that already has
  a dispatch table). It stays until each is migrated, and in the meantime
  PR 1 makes it set `auth: AuthLevel::Admin`, so a not-yet-migrated row that
  reaches `declare` by mistake over-protects and shows up in the surface
  snapshot rather than exposing anything. PR 7 deletes it.
- Metadata is set through `const fn` builders that take `self` and return
  `Self`: `.summary(&'static str)`, `.description(..)`, `.tags(&[..])`,
  `.deprecated()`, `.agent_tool(name, description)`, and the four schema
  slots `.input(f)`, `.output(f)`, `.path_params(f)`, `.query_params(f)`,
  each taking a `fn() -> serde_json::Value`.
- Schema producers are plain functions. For a `schemars` type the block
  writes `request_schema_of::<contracts::LoginRequest>` for a body, path or
  query schema and `response_schema_of::<contracts::MeResponse>` for a
  response schema, two new generic functions in `endpoint_match.rs`. The
  upstream builders derive under different serde contracts (deserialize for
  what a client sends, serialize for what the server emits) with settings
  that are private to wafer-block, so each producer goes through the
  matching upstream builder on a throwaway endpoint rather than copying those
  settings, and a row serializes the same bytes the hand-written list did.
  For the hand-written path-param schemas that already exist as functions
  (`provider_id_path_schema`, `id_path_schema`, ...) the block passes the
  function name. Function pointers are `const`, so the table stays a `const`
  and nothing is built at startup that was not built before.

`pub fn declare<H>(table: &[EndpointRoute<H>]) -> Vec<BlockEndpoint>` maps
each row to a `BlockEndpoint` through the upstream builders, in table order,
calling each schema producer once. `info()` becomes
`.endpoints(endpoint_match::declare(ROUTES))`. Where a block today appends
feature-gated endpoints (`system` under `block-llm` / `block-files`), the
table is split into a base `const` and cfg-gated `const` slices, and
`declare` is called on each; the block never builds a `Vec<BlockEndpoint>` by
hand.

`dispatch()` keeps its signature. It reads only `method`, `template` and
`handler`, so a row's metadata has no effect on matching. `dispatch_path()`
exists for the products block's rewritten sub-path; once products dispatches
on wire paths (PR 6) it has no caller and PR 7 deletes it.

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

`{name...}` may also be followed by a trailing slash, meaning a folder-style
listing; the path must end in `/` and the bound remainder must be non-empty.
This is the shape files already declared for nested folder pages; the matcher
did not support it.

The system surface snapshot therefore changes in PR 1 from the per-asset lines
to two lines, `GET /health public` and `GET /b/static/{filename} public`. The
router's access decision for an asset request is unchanged: `endpoint_auth`
resolves the bound filename to the declared `Public`, which is what lets PR 7
delete the `STATIC_PREFIX` carve-out.

`normalize_template` is deleted. The one colon-style template
(`userportal`'s `:hash`) is rewritten to `{hash}` in the PR that migrates
that block. Until then the shim stays, so the delete is in PR 3, not PR 1.

### 3. Blocks: one table, wire paths, no path reads outside the matcher

Every block ends up with:

```rust
enum Route { LoginPage, Login, Me, ... }

const ROUTES: &[EndpointRoute<Route>] = &[ ... ];

handle: |this, ctx, mut msg, input| {
    let Some(route) = endpoint_match::dispatch(&mut msg, ROUTES) else {
        return err_not_found("not found");
    };
    match route { ... }
}
```

Templates are written as the path appears on the wire, `/b/auth/api/login`,
never a stripped or re-rooted form. Path variables are read only through
`msg.var("id")` after `dispatch` bound them. Every `path_param(msg, "id",
"/b/x/")` call and every `strip_prefix` on a path is deleted with the arm it
served.

One path read stays outside the matcher: `llm`'s
`/b/llm/api/internal/default-target`, answered only when `ctx.caller_id()` is
set, is an inter-block call and not an HTTP endpoint. Declaring it would
publish it. It keeps its handler-owned guard, with a comment saying why.

Three blocks need a specific decision.

**auth-ui.** The eleven undeclared method-and-path pairs become rows with
the level the handler already enforces. Nine are `public`, each gated inside
the handler by a token, signature or shared secret rather than a session:
`GET /b/auth/reset-password`, `GET /b/auth/oauth/callback`, `GET` and
`POST /b/auth/api/verify`, `POST /b/auth/api/resend-verification`,
`POST /b/auth/api/forgot-password`, `POST /b/auth/api/reset-password`,
`GET /b/auth/api/oauth/providers`, and `POST /b/auth/api/oauth/sync-user`.
Two are `authenticated`: `PATCH` and `DELETE /b/auth/api/api-keys/{id}`.
The `/b`-stripping is removed and every arm compares the wire path. The
rate-limit table (`RATE_LIMIT_ROUTES`, `mod.rs:40–95`) stops matching path
strings: it becomes a function from the `Route` variant to a limit, applied
after `dispatch` has chosen the variant, so the block has one path matcher.
The auth-ui surface snapshot gains eleven entries, each reviewed.

**files.** The admin-side storage APIs stop being reached through the admin
block. Today the wire path is `/b/admin/api/cloudstorage/...` or
`/b/admin/api/storage/...`, which the router sends to the admin block, which
rewrites `req.resource` and calls the files block. The router dispatches by
prefix and `/b/admin/` belongs to the admin block, so the files block cannot
declare those wire paths itself. They move under the prefixes the router
already sends to files: `/b/cloudstorage/admin/shares`,
`/b/cloudstorage/admin/access-logs`, `/b/cloudstorage/admin/quotas`,
`/b/cloudstorage/admin/quotas/{id}`, `/b/storage/admin/api/buckets` and
`/b/storage/admin/api/stats`, each an `admin` row the router enforces from
the declaration. The admin block's two delegation arms and the
`req.resource` rewrite are deleted; the admin storage page
(`admin/pages/storage.rs`) never called the files block and is untouched.
The two `cloud.rs` user paths (`/b/cloudstorage/shares`,
`/b/cloudstorage/quota`) and the share delete become declared
`authenticated` rows, as do the three `/b/storage/api` paths the sub-table
served but never declared: `GET /b/storage/api/search`,
`GET /b/storage/api/recent` and `DELETE /b/storage/api/buckets/{name}`. The
two object rows declare `{key...}`, the template dispatch always matched, in
place of the `{key}` no nested key could match; that re-templates two surface
lines and one OpenAPI path key. The `starts_with` chain in
`files/mod.rs:141–246` is replaced by the table. The files surface snapshot
gains the thirteen rows listed here (six `admin`, seven `authenticated`); the
admin surface snapshot does not change, because the admin block never
declared the delegated paths.

This is the one place the spec departs from the design agreed in chat, which
said the files block would declare `/b/admin/api/storage/...`. That path is
unreachable for the files block without a per-path router entry, which is
the kind of entry this phase removes.

**products.** The block dispatches today in two hops: `mod.rs:1526–1537`
rewrites `/admin/b/products/...` to a sub-path, then `handlers/dispatch.rs`
matches an `ADMIN_ROUTES` table over the rewritten path. Both tables merge
into one `ROUTES` over wire paths. The gate a handler needs beyond the
router's auth check (seller ownership, Stripe configured, and so on) is data
on the handler enum variant, not a second path match. The
`router_declared_public("/b/products/webhooks", ..)` carve-out is deleted,
because the block declares the webhook `public` and the router reads that
declaration. The "boot or tests where BlockInfo metadata is unavailable"
case its comment describes does not exist at runtime: the pipeline always
hands `route_to_block` the registered infos. The one place it happens is
`stripe_webhook_carveout_stays_reachable_with_no_session`, which passes an
empty slice on purpose; that test is rewritten to pass the products block's
info and assert the same anonymous dispatch.

### 4. Router: the prefix table stops making per-path decisions

In `routing.rs`:

- `Route::router_declared_public` and the `router_final` field are deleted,
  with the eight auth-ui carve-outs, the static carve-out, and the products
  webhook carve-out. `route_to_block` and `effective_access` lose their
  `router_final` branches. The access decision is always
  `route.access.max(declared_access(...))`, and `declared_access` keeps its
  fail-closed `Authenticated` default for an undeclared path.
- `ROUTES` keeps one prefix per block and the inspector proxy. The
  `/b/admin/settings` and `/b/legalpages/admin` and `/b/legalpages/api`
  entries stay for now: they raise the prefix tier to `Admin` for paths
  whose blocks are not yet migrated. The final PR removes any prefix entry
  whose tier is fully expressed by the block's declared rows.
- `PreparedRoute.router_final` is deleted from `prepared_plan.rs` and
  `PREPARED_RUNTIME_PLAN_SCHEMA_VERSION` goes from 1 to 2, so a plan
  exported by an older build is rejected at import rather than silently read
  with a field missing. `refine_undeclared` stays; it is the consumer-route
  policy and is unrelated.
- The prefix table stays a hand-written `const` through this phase. Whether
  it can be derived from the blocks' tables is decided in the final PR with
  all blocks migrated; it is not decided here.

`ExtraRoute`, `extra_route_access`, `feature_gate_name`, and the WebMCP
manifest projection are untouched.

### 5. Testing

Test-first, per PR:

- **Two snapshots are the contract.** The existing OpenAPI snapshot only
  lists endpoints that carry a schema (`BlockEndpoint::has_schema`), so a
  page or a schema-less API can be added, dropped or re-levelled without it
  noticing. PR 1 adds `tests/endpoint_surface.rs`: for every registered
  block it writes `tests/snapshots/<block>.endpoints.json`, one line per
  `info().endpoints` entry with method, path, auth and agent-tool name,
  sorted by path then method, regenerated with the same
  `UPDATE_OPENAPI_SNAPSHOTS=1` switch. Each PR runs both tests before
  touching a block and again after. A PR that declares a previously
  undeclared path regenerates the surface snapshot, and the PR description
  lists every added line with the handler line that enforces that level
  today.
- **Core.** `endpoint_match` tests cover: `declare` maps every field
  (including a schema producer being called and `agent_tool` being set);
  `request_schema_of::<T>` produces the same value as
  `BlockEndpoint::input::<T>` and `response_schema_of::<T>` the same value
  as `BlockEndpoint::output::<T>`, on a type where the two contracts differ;
  the three constructors set the auth they name and `new` sets `Admin`;
  `normalize_template` no longer exists.
- **Router.** The existing routing tests that assert a carve-out
  (`static_prefix_route_is_router_declared_public`,
  `stripe_webhook_carveout_stays_reachable_with_no_session`,
  `effective_access_agrees_with_the_router_for_a_router_final_route`) are
  rewritten to assert the same reachability through the block's declaration:
  an anonymous request for a hashed asset, the Stripe webhook, and an OAuth
  callback each dispatch without a carve-out in the table.
- **Blocks.** Each migrated block gets a table test that every row's
  template matches at least one request the block's existing tests send, and
  that every path the block served before (listed from the old match arms in
  the PR) resolves to a row. The existing handler tests keep passing
  unchanged, since they send wire paths already.
- **Prepared plan.** A test that a plan exported at schema version 1, with
  `router_final` present, is rejected at import, and that a version 2 plan
  round-trips.

Verification per PR is the Phase 0 script set: nightly `fmt --check`,
`clippy -p impresspress-core --all-targets -D warnings`, `cargo test -p
impresspress-core --no-fail-fast` (the `lockfile_loads_remote_block` failure
is a known wasmi-feature artefact and is not a gate), and the wasm test run
for `impresspress-cloudflare` when a PR touches anything under
`prepared_plan.rs`.

### 6. Sequencing

Seven PRs against `main`, each independently green and mergeable:

1. **Core + llm + system.** Add the endpoint-surface snapshot test and
   commit the baseline for every block before any other change. Extend
   `EndpointRoute`, add `declare` and the two schema producers. Migrate `llm` (already has
   a table; it gains metadata and drops its `info()` list) and `system` (one
   `{filename}` row replaces the per-asset declarations, see section 2).
   Both snapshots byte-identical for every block except `system`'s surface
   snapshot, which changes as section 2 describes. The static carve-out is
   not removed yet, because the router still has the `router_final` branch;
   PR 7 removes it.
2. **messages, vector, legalpages, tickets, dev.** Blocks whose tables
   already exist or whose dispatch is a flat match. Snapshots byte-identical.
3. **userportal + auth-ui.** Colon template rewritten, `normalize_template`
   deleted, auth-ui's eleven rows added, `/b`-stripping removed, rate limits
   keyed on the variant. Auth-ui surface snapshot grows by eleven reviewed
   lines.
4. **files.** Admin storage APIs move under the files prefixes, admin
   delegation deleted, `starts_with` chain replaced. Files surface snapshot
   grows by eight reviewed lines.
5. **admin.** `route.rs` and `api_norm` replaced by the table.
6. **products.** Two-hop dispatch merged, handler gates moved to enum data,
   webhook carve-out no longer needed.
7. **Router cleanup.** Delete `router_declared_public`, `router_final`,
   `PreparedRoute.router_final`, `EndpointRoute::new`, `dispatch_path`,
   `util::path_param`;
   bump the plan schema version; drop any prefix entry the blocks' rows
   fully express; decide whether the prefix table is derived or kept.

PR 1 is the only one that changes a shared type; PRs 2 through 6 are
independent of each other and can land in any order once PR 1 is in. PR 7
requires all of them.

## Non-goals

- Turning `impresspress_feature_block!` into a trait. The macro's
  `info` / `handle` closures are where the table is consumed; that is a
  separate phase.
- Any change to wafer-run. `BlockEndpoint`, `AuthLevel`, `HttpMethod` and
  `AgentTool` are consumed as they are at rev `7d47e5e`.
- Adding schemas to endpoints that have none today. `declare` carries what
  the block already declares; widening the contract is its own review.
- Changing what any path returns, or which caller may reach it, beyond
  turning an implicit "the handler checks a token" into an explicit
  `public` row that the snapshot shows.

## As built

The seven PRs landed as sequenced. Where the implementation departed from
or refined the design above, one line each with the PR that made it:

- `system` declares one `GET /b/static/{filename}` row and looks the bound
  filename up in the build-time asset manifest; the per-asset
  `app-{hash}.css` declarations are gone (#12).
- `request_schema_of::<T>` and `response_schema_of::<T>` go through the
  upstream `BlockEndpoint::input` / `::output` builders on a throwaway
  endpoint rather than copying wafer-block's private derive settings, so a
  row serializes the bytes the hand-written list did (#12).
- `endpoint_auth` mirrors `dispatch`'s trailing-slash retry, so the router
  gates a bare index path (`GET /b/llm`) at the `/b/llm/` row's declared
  level instead of the `Authenticated` default (#12, review fix).
- `match_template` accepts `{name...}/`, a rest parameter followed by a
  trailing slash (the path must end in `/`, the remainder must be
  non-empty), for files' nested folder pages (#15).
- files declares thirteen rows, not eight: the six relocated admin rows,
  the four `cloudstorage` user rows (share list, share create, share
  delete, quota), plus
  `GET /b/storage/api/search`, `GET /b/storage/api/recent` and
  `DELETE /b/storage/api/buckets/{name}`, served but never declared, now
  `authenticated`; the two object rows use `{key...}` in place of `{key}`,
  which changes two surface lines and one OpenAPI path key (#15).
- products serves each handler at exactly its declared spelling: the 51
  aliases the old `strip_prefix("/b/products/api")` rewrite produced, and
  the webhook's non-POST and suffixed shapes, answer 404 (#17).
- Router cleanup (PR 7): a carve-out was a prefix entry admitting every
  method under it, a declaration admits its exact template, so anonymous
  requests for the shapes only the prefix admitted (`GET
  /b/products/webhooks`, `GET /b/auth/api/verify/extra`, `GET /b/static/a/b`)
  are denied before dispatch instead of reaching the block's 404; an
  undeclared path under the former `Admin` entries `/b/legalpages/admin`
  and `/b/legalpages/api` falls to `Authenticated` and is answered 404 by
  the block's table dispatch (the deleted `/b/admin/settings` entry sat
  under `/b/admin/`, which stays `Admin`, so nothing changed there). The
  prefix table stays
  hand-written (fourteen entries: one prefix per block, `/health` and
  `/b/static/` for system, `/b/storage/` and `/b/cloudstorage/` for files,
  the inspector proxy), kept honest by a two-way test against
  `blocks::all_block_infos()` and a pairwise-disjointness test.
  `PREPARED_RUNTIME_PLAN_SCHEMA_VERSION` is 2.
