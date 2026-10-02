# dev.impresspress.org — an agentic website sandbox in the browser

**Date:** 2026-09-02
**Status:** design, approved in brainstorming; implementation plans follow
**Repos:** `wafer-run` (producer), `impresspress` (consumer)
**Supersedes, where they differ:** `docs/2026-09-02-agentic-browser-development-plan.md` (§3 activation, §6 gate, §12 preview, §14 tool set, §16 confirmation) — everything not contradicted here still applies.

## 1. Goal

A visitor opens `dev.impresspress.org` in a WebMCP-capable browser with their
AI agent, and the agent builds them a website — frontend pages and any backend
code — that runs immediately, in that tab, with nothing deployed behind it.
When they like it, one button exports the whole thing as a static bundle they
can serve locally.

Concretely, the demonstration that has to work end to end:

1. The agent builds a shop's pages.
2. The agent stocks it with products through the products block's admin API.
3. A shopper (another browser context, anonymous) browses those products.
4. The user exports the shop and it runs from a local static server.

Everything runs in the visitor's browser: the ImpressPress service worker
(products, admin, auth, `wafer-run/web`), a new `impresspress/dev` block, a
page-owned Rubrc compiler worker for Rust backend blocks, and wasmi to execute
those blocks. Every visitor has their own sql.js database and OPFS; there is
nothing shared to protect.

## 2. What exists and what this reuses

| Piece | Where | Reused how |
|---|---|---|
| WebMCP producer and consumer | wafer-run #323/#324; impresspress #72, #74–#81, #76, #77 | The global auth-filtered manifest discovers activated guest blocks' tools; `webmcp.js`'s registration code is reused for the page-scoped dev manifest |
| Browser runtime | `impresspress-browser`, `impresspress-web`, `impresspress-bundle` | The service worker, OPFS storage, sql.js database, SW↔page bridges, static-shell bundler |
| wasmi feasibility | `experiments/browser-service-worker-blocks/` | Proof guest becomes the smallest compatibility fixture; measured host cost +504 KB gz |
| Agentic browser plan | `docs/2026-09-02-agentic-browser-development-plan.md` | Architecture, storage foundation, runtime manager, validation rules, security list — adopted except as listed in §3 |
| Typed products contracts | `blocks/products/contracts.rs` (#80) | Source of every `shop_*` tool schema |
| Seeded local admin | `impresspress-web/src/config.rs` | `admin@example.com` / `admin123` is the sandbox's admin |

Agent product writes for production sites (`2026-09-01-agent-product-writes-design.md`,
Plans A and C) are unaffected: they remain the design for sites with real
customers. The sandbox's `shop_*` tools exist only on the trusted `/b/dev`
page of a throwaway browser-local instance.

## 3. Decisions

Each of these was a fork; the choice and its reason are recorded so the plans
do not re-litigate them.

1. **Auto-activation.** Every successful change (site file write, block
   compile, block removal, rollback) is validated and activated immediately.
   The existing plan's prepare → confirm(nonce) → activate gate and its
   `browser_dev_confirmations` table are dropped. Reason: the only thing at
   risk is the visitor's own throwaway instance; a generation ledger and a
   `dev_rollback` tool give back what the confirmation gate protected.
2. **The live site is the preview.** `/b/dev` embeds `/` in a sandboxed
   iframe and reloads it after each activation. There is no separate preview
   route or workspace-serving path.
3. **Curated shop tools on `/b/dev`.** The agent stocks the shop through
   page-scoped `shop_*` tools that call the existing typed products admin
   endpoints. They are absent from every other page and from the global
   manifest. Reason: Plan C is draft-only by design, so an agent could never
   make a product visible to the shopper; the sandbox's trust model is
   different from a production site's.
4. **Export = runtime + site + blocks + data.** The zip is the same static
   bundle dev.impresspress.org serves (dev mode off) plus a seed the service
   worker installs on first boot: site files, block artifacts and source, and
   a data snapshot with secrets and sessions excluded.
5. **v1 guests speak JSON everywhere (wafer-run).** Rubrc guests have no
   crates; host service calls today require MessagePack payloads. The wasmi
   loader already negotiates ABI v1 = JSON for `__wafer_handle` and
   `__wafer_lifecycle`; it is extended to host calls with a generic
   JSON↔MessagePack transcoder. Reason: root cause is in the producer; the
   alternative (a MessagePack codec vendored into every guest) drifts from
   `wafer-block`'s wire types.
6. **Table-scoped DDL (wafer-run).** `database.ddl` is a raw SQL string
   authorized on a global `__ddl__` resource, so a sandboxed guest with
   `ddl` could drop the products table. New structured ops
   (`create_table`, `create_index`, `add_column`, `drop_table`) are authorized
   on the table name.
7. **One-click entry, in the workspace only** *(amended 2026-10-02; this
   reverses the original decision, "No auto sign-in")*. The landing page's
   "Open workspace" link goes to `/b/dev/enter`, which signs the visitor in
   as the bootstrap admin and opens `/b/dev` with nothing to type. The
   landing page still shows the credentials: they are how a human signs back
   in once the session expires. Reason for the reversal: the original
   reasoning — "an agent filling the login form from the page is itself a
   fair demonstration" — did not survive contact with one. A visitor's agent
   in a cloud browser could not get past the form at all, and the form
   protects nothing here: the account is a per-browser throwaway whose
   password is printed beside the link. See amendment 21 for the mechanism
   and its boundary.
8. **Declared capabilities, granted exactly.** `allows_collection` matches
   exact names or `*`, so "own namespace" is enforced by validating that
   every declared collection/folder/key carries the block's prefix and then
   granting exactly the declared set.

## 4. Visitor experience

### 4.1 Landing page `/`

The activated site is served at `/` by `wafer-run/web`, so the landing page
is the **seeded starter site** — generation 0, installed on first boot through
the seed mechanism of §10. It says what dev.impresspress.org is ("a workspace
for building websites with a WebMCP-capable browser agent"), tells the human to
open it in such a browser, shows the local admin credentials and why they are
safe to publish (per-instance, browser-local), and links **Open workspace →
`/b/dev/enter`**, the one-click entry page (decision 7 as amended, amendment
21). Until 2026-10-02 the link was `/b/auth/login?redirect=/b/dev`; that URL
still works and is what the entry page falls back to.

When the agent builds the visitor's site, it replaces this page. That is the
intended lifecycle; the welcome page is just the first generation.

### 4.2 `/b/dev`

Admin-only. Three panes: file tree + editor; the live site in a sandboxed
iframe; a progress and log panel; and a **Tool console** (amendment 22) that
runs the page's tools by hand, for an agent whose browser has no WebMCP. A
visible "How this workspace works" section
gives the workflow, the file layout (`site/`, `blocks/<name>/`), the tool
names, and a suggested prompt the human can copy. Because `/b/dev` is our own
trusted document, instructing the agent from it is correct. The iframe's
`sandbox` attribute is NOT the prompt-injection boundary: it carries
`allow-scripts allow-same-origin` over same-origin content, so the framed
page can reach `parent.document.modelContext` (including `registerTool`) and
`parent.__impresspressWebmcp` directly — see amendment 7 for why and what the
attribute buys instead. What the sandbox actually relies on is that the
framed content is the admin's own site and every tool call is authorized
server-side, not that the iframe walls anything off; a separate preview
origin (or a credentialless frame, which would lose the service worker) is
the real fix and is a tracked follow-up.

On load the page starts prefetching the pinned Rubrc payload in the
background with a progress bar so the first compile does not pay the
download.

### 4.3 The agent loop

`dev_status` → read the reference and files it needs → `dev_write_file` for
site files (each write activates) → for backend code `dev_create_block`, edit
`blocks/<name>/src/lib.rs`, `dev_compile_block` (structured diagnostics on
failure; staged, validated and activated on success) → `shop_*` to stock the
shop → verify by calling the activated block's tool from the global manifest or
by reading the page → `dev_export` when done. `dev_rollback(generation)` if
something regressed.

Every mutating tool result carries `generation` and a `progress` list so the
agent knows what happened without the page; the page shows the same phases
live and reloads the iframe on `active`.

### 4.4 The shopper

Anonymous. Opens `/` (in the same browser, or an incognito context for the
test), sees the products the agent published, prices one through the
storefront widget, and — if a Stripe key were configured — would be handed a
checkout link. `start_checkout` stays an honest error result without a key;
the sandbox does not pretend to take money.

## 5. Architecture

    /b/dev document (Admin, COOP/COEP, no-store)
      ├── editor · live-site iframe (sandboxed) · progress panel
      ├── dev.js: registers tools from /b/dev/api/tools.json (+ compile, export)
      └── BrowserRustCompiler adapter
            └── dedicated compiler Worker (Rubrc toolchain, VFS, subordinate workers)
                  └── wasm32-wasip1 artifact  ──POST /b/dev/api/builds/stage──┐
                                                                                ▼
    ImpressPress service worker
      ├── impresspress/dev block
      │     ├── workspace: blobs + manifests (OPFS), generations (SQLite)
      │     ├── validation: hashes, BlockInfo, names, routes, capabilities, probe
      │     ├── activation queue (coalescing), journal, recovery
      │     ├── site publisher → wafer-run/web/site
      │     ├── tools.json (typed, page-scoped) · export · seed import
      │     └── shop tool projections → existing products admin endpoints
      ├── RuntimeManager: active Rc<Wafer>, candidate construction, atomic swap
      ├── wafer-run/web serves the activated site at /
      ├── WasmiBlock executes activated guests (declared caps, limits)
      └── /b/webmcp/manifest.json discovers activated guests' agent tools

The document coordinates; it talks to both the compiler worker and the
service worker. The service worker never creates or owns the compiler worker.

Changes against the existing plan:

| Existing plan | This design |
|---|---|
| `browser-devtools` feature off by default; `[dev] enabled` runtime gate | Same gate; on in `examples/dev-sandbox`; off in every export |
| Prepare → confirm(nonce) → activate | Auto-activate; ledger + rollback; no confirmations table |
| Preview served from the workspace folder | Live site is the preview |
| Capabilities compared to an approval list | Declared set validated against the block's namespace and granted exactly |
| Guest host calls are MessagePack | v1 guests speak JSON (wafer-run transcoder) |
| Raw DDL only | Table-scoped structured DDL ops |
| No product tools | Curated `shop_*` tools on `/b/dev` |
| Export = phase-8 backup | Export = runnable bundle + seed + data snapshot |
| `wafer-run/web` default caching | `cache_mode = "no-cache"` in the sandbox and in exports |

## 6. Backend guest contract

### 6.1 Project shape

    blocks/<name>/
      Cargo.toml            no dependencies; crate-type cdylib; opt-level "z", lto, panic=abort
      src/lib.rs            the user's code
      src/wafer_guest.rs    vendored support module (see 6.2)

`<name>` is `^[a-z][a-z0-9-]{1,31}$` (no `--`, no trailing `-`; see amendment 11);
the block's WAFER name is `site/<name>`;
its routes live under `/b/<name>/`. `wafer-run/` and `impresspress/` names,
and any route prefix a built-in block owns, are reserved and rejected.

Rubrc's "basic library support" covers in-crate modules, which is all the
template uses.

### 6.2 `wafer_guest.rs`

A single std-only file, versioned (`WAFER_GUEST_VERSION` constant), written
into every new block by `dev_create_block` and never regenerated silently —
an upgrade is an explicit write that shows in the file's hash. It owns:

- the four ABI exports (`__wafer_alloc`, `__wafer_info`, `__wafer_handle`,
  `__wafer_lifecycle`) and ABI-v1 JSON framing (no `__wafer_abi_version`
  export, which negotiates v1);
- a small JSON value type with parser and serializer;
- `Request` (method, path, query map, headers, body bytes, `user_id`,
  `user_email`, `roles` — read from the host-owned `auth.*` meta) and
  `Response` builders (status, content type, headers, JSON/text/bytes);
- `Ctx`, the handle passed to `init` and every handler, through which host
  calls are made and which carries the request's cancellation state;
- a JSON-schema builder (`Schema::object().prop(...).required(...)`,
  `string`, `integer`, `number`, `boolean`, `array`, `enum_of`) for tool
  schemas;
- `Block` assembly: name, summary, `requires`, declared capabilities,
  endpoints with optional `agent_tool(name, description, input, output)`,
  rendered into the exact `BlockInfo` JSON `wafer-block` parses;
- host-call helpers over `__wafer_host_stream_*`:
  `db::{get, list, create, update, delete, upsert, count, create_table,
  create_index, add_column}`, `storage::{get, put, delete, list}`,
  `config::get`, `log::{error, warn, info, debug}`;
- an `init(ctx)` hook the user implements for `LifecycleType::Init` (where
  `create_table` calls go); `Start`/`Stop` are handled by the module.

`lib.rs` is declarative:

    pub fn block() -> Block {
        Block::new("site/newsletter", "Newsletter signups")
            .requires(&["database", "logger"])
            .collection("site__newsletter__subscribers")
            .endpoint(Method::Post, "/b/newsletter/subscribe", subscribe)
                .agent_tool("subscribe_newsletter", "Subscribe an email address …",
                            Schema::object().prop("email", Schema::string()).required(&["email"]),
                            Schema::object().prop("ok", Schema::boolean()))
    }
    pub fn init(ctx: &Ctx) -> Result<(), Error> {
        db::create_table(ctx, "site__newsletter__subscribers", &[
            Column::text("id").primary_key(), Column::text("email").not_null(), Column::text("created_at"),
        ]).if_not_exists()
    }
    fn subscribe(req: &Request, ctx: &Ctx) -> Response { … }

Two templates ship: `hello` (one GET, no services) and `table` (the
newsletter block above). `dev_read_reference` returns the module's public API
documentation, the capability rules, both templates, and the limits.

### 6.3 Wire: a v1 guest speaks JSON everywhere (wafer-run)

`abi_codec_of` already yields `V1Json` for a guest without
`__wafer_abi_version`. For such a guest the wasmi loader:

- accepts the `stream_init` message as JSON (already true);
- on `stream_finish`, transcodes the accumulated request body
  `serde_json::Value → rmp_serde::to_vec_named` before `Context::call_block`;
- on `read_chunk`, transcodes each response frame MessagePack → JSON;
- on `take_error`, transcodes `WaferError` MessagePack → JSON;
- refuses `stream_attach` with `InvalidArgument` (attachments stay v2-only).

Byte fields are JSON integer arrays in both directions, matching the existing
v1 convention for request bodies; `serde_bytes` deserializes a sequence of
integers. Golden tests: the existing proof guest, plus a JSON-speaking guest
that round-trips `database.create`/`get`, `storage.put`/`get`, `config.get`
and a `create_table`.

### 6.4 Table-scoped DDL (wafer-run)

New database ops with structured DTOs, rendered through
`wafer-sql-utils::ddl::{build_create_table, build_create_index,
build_add_column, build_drop_table}` for the active backend:

    database.create_table  { table, columns: [{ name, kind, nullable, primary_key, default }], if_not_exists }
    database.create_index  { table, name, columns, unique, if_not_exists }
    database.add_column    { table, column }
    database.drop_table    { table, if_exists }

Each is authorized as `(table, ResourceType::Db, write)`, so the caller needs
`caps.ddl` **and** `allows_collection(table)`. Raw `database.ddl` is
unchanged for native blocks; sandboxed guests never receive it.

### 6.5 Capabilities

The guest declares `BlockInfo.capabilities`. Validation (§7.4) refuses
activation unless:

- every collection is `site__<name>__*`;
- every storage folder is `site/<name>/…`;
- every config key is `SITE__<NAME>__*`;
- `raw_sql = false`, `network = None`, `crypto = false`, `vector_indexes = None`;
- `callable_blocks ⊆ {database, storage, config, logger}` and equals
  `requires`;
- `ddl` may be true (it is table-scoped now).

The dev block then grants **exactly the declared set** through
`WasmiBlock::load_with_capabilities_and_limits`. A guest that declares
nothing gets `BlockCapabilities::none()`. Cross-block calls (a guest calling
products) are out of scope for v1; a guest's *frontend* talks to products over
HTTP like any page.

### 6.6 Limits

`ResourceLimits { fuel: Metered(100_000_000), memory_pages: 256 }` per call
(wafer-run defaults); artifact ≤ 4 MiB; ≤ 16 blocks per workspace; one
compile at a time; compile timeout 120 s; source file ≤ 512 KiB; ≤ 2 000 files
and ≤ 64 MiB of blobs per workspace. A guest trap fails that request with 500
and a log line and never poisons the outer runtime.

## 7. Generations and activation

### 7.1 Content model

The workspace is `site/**` and `blocks/<name>/**`. Every file version is one
content-addressed blob (`impresspress/dev/blobs/<sha256>`); the workspace and
every generation are manifests of `path → {sha256, size, content_type}`.
Nothing is edited in place.

### 7.2 Generations

One row per generation in `impresspress__dev__generations`:

    id, parent_id, status (Staged | Validating | Activating | Active | Failed | Superseded),
    site_manifest_json, block_manifest_json, manifest_sha256, cause
    (SiteWrite | SiteDelete | BlockCompile | BlockRemove | Rollback | Seed),
    created_at, activated_at, failure_message

Statuses and causes are closed Rust enums. A generation is created
automatically by:

- `dev_write_file` / `dev_delete_file` under `site/` — **site-only**, no
  runtime rebuild;
- a successful compile stage — block set changed, **runtime rebuild**;
- `dev_remove_block` — runtime rebuild;
- `dev_rollback(g)` — a new generation copying `g`'s manifests (append-only
  history; you never go back, you republish);
- seed import on cold boot — generation 0.

Writes under `blocks/` do not create a generation; only a compile does.

### 7.3 Activation

One serialized queue in the service worker. Requests arriving during an
activation coalesce: the queue keeps the latest desired manifest, and every
waiting caller resolves with the generation that contains its change.

1. Journal `desired_generation_id`, phase `Validating`.
2. Validate: every blob and artifact present and hash-verified; block
   validation (§7.4).
3. If the block set changed from the active generation: build a candidate
   `Rc<Wafer>` from the base registrations plus every block in the generation
   (`load_with_capabilities_and_limits`, `extra_block`, `add_route`), seal,
   init blocks with the already-loaded service handles, update WRAP grants.
   Phase `BuildingRuntime`.
4. Swap the active runtime, retaining the previous `Rc`.
5. Publish the site: write only files whose hash changed into
   `wafer-run/web/site`, delete files no longer in the manifest,
   `index.html` last. Phase `Publishing`.
6. Journal `active_generation_id`, increment `generation`, clear `desired`.
   Phase `Active`.

A failure before step 4 leaves the previous generation live and marks the new
one `Failed` with a message. A failure after step 4 restores the previous
`Rc` and re-publishes the previous site files before returning. On service
worker start, a non-empty `desired_generation_id` is a recovery journal:
initialization converges to it or restores the previous active generation
before serving requests.

Retention: the last 20 generations; older rows become `Superseded` and blobs
referenced by no retained generation are deleted.

`WAFER_RUN_SHARED__HAS_LANDING_PAGE=true` is seeded in the sandbox; the
starter site guarantees there is always a site to serve.

### 7.4 Block validation

Before a generation containing a block can activate:

- artifact hash matches the stored bytes; size ≤ limit;
- wasmi compiles and instantiates it under the declared capabilities and
  limits;
- `BlockInfo` parses and passes WAFER validation; its name is `site/<name>`
  and matches the manifest;
- route prefixes are normalized, under `/b/<name>/`, and collide with no
  built-in route; extra routes stay lower priority than built-ins;
- declared endpoint paths fall under the block's routes; endpoint auth is
  respected by the router as for any block;
- duplicate block names, route prefixes and agent tool names across the
  generation and the built-ins are rejected — the producer's manifest
  `seal()` check runs against the candidate;
- capabilities pass §6.5;
- `Init`, `Start` and one probe request complete without trapping under the
  granted capabilities.

Refusals are structured diagnostics in the tool result, never a transport
error.

### 7.5 Progress

Two channels, no push plumbing:

1. The page-local tool wrapper polls `GET /b/dev/api/status` every ~300 ms
   while a mutating call is in flight; the response carries
   `activation: { generation_id, phase, detail }`. The compiler worker posts
   `download(bytes, total) → initializing → compiling(stage)` to the page.
   The panel renders both; the iframe reloads on `Active`.
2. Every mutating tool result carries `structuredContent.generation` and
   `structuredContent.progress: [{ phase, ms }]`.

## 8. Compile pipeline

`BrowserRustCompiler` (`initialize(onProgress)`, `compile(snapshot,
options)`, `cancel()`, `dispose()`) hides Rubrc behind a narrow interface, as
the existing plan specifies: pinned revision, recorded license and asset
hashes, `new Worker(url, { type: "module" })`, subordinate-worker URL
resolution preserved after packaging, no coupling to Rubrc's UI.

`dev_compile_block(name)`:

1. The page reads `blocks/<name>/**` from the service worker.
2. The worker compiles for `wasm32-wasip1` with release-size settings; one
   compile at a time; 120 s timeout; the worker is terminated and recreated
   after an unrecoverable failure.
3. On failure: `{ success: false, diagnostics: [{ file, line, column,
   severity, message }], stdout, stderr, elapsed_ms }`.
4. On success: SHA-256 locally, then `POST /b/dev/api/builds/stage` with the
   artifact (base64 in typed JSON, ≤ 4 MiB), source manifest hash, compiler
   version and diagnostics.
5. The stage endpoint validates (§7.4) and enqueues activation; the tool
   result is `{ success, build_id, generation, diagnostics, progress }`.

Compiler assets are packaged as versioned same-origin files under
`/__impresspress_dev/compiler/<version>/`, on the service worker's bypass
list, with immutable caching; they are absent from every non-sandbox
distribution and from exports.

## 9. Tool surface

### 9.1 Mechanism

`GET /b/dev/api/tools.json` is a page-scoped WebMCP manifest generated from
typed Rust contracts by the same producer code as the global manifest.
`dev.js` registers those tools with `document.modelContext.registerTool`
under an `AbortController`, adds the two page-local tools (`dev_compile_block`,
`dev_export`), wraps every mutating tool with the progress poller, and aborts
all registrations on unload or when a `401` shows the session is gone. None
of these endpoints carry `agent_tool` metadata, so none appear in the global
manifest.

### 9.2 Workspace tools

| Tool | Kind | Effect |
|---|---|---|
| `dev_status` | read | generation, active blocks, compiler state, workspace summary; described as "call this first" |
| `dev_read_reference` | read | the guest authoring reference (§6.2) |
| `dev_list_files(prefix?)` | read | `[{ path, sha256, size }]` |
| `dev_read_file(path)` | read | `{ content, encoding: utf8 \| base64, sha256 }` |
| `dev_list_generations(limit?)` | read | ledger rows with per-generation file and block diffs |
| `dev_write_file(path, content, expected_sha256, encoding?)` | mutate | `expected_sha256: string \| null` (null = must not exist); under `site/` activates |
| `dev_delete_file(path, expected_sha256)` | mutate | under `site/` activates |
| `dev_create_block(name, template)` | mutate | scaffolds `blocks/<name>/` from `hello` or `table` |
| `dev_compile_block(name)` | page-local | §8 |
| `dev_remove_block(name)` | mutate | removes from the active set; source kept |
| `dev_rollback(generation)` | mutate | republishes that generation |
| `dev_export()` | page-local | §10; hands the browser a download; returns file list and size |

A hash mismatch returns the current hash and size so the agent can re-read
and retry.

### 9.3 Shop tools

Curated projections of existing products admin endpoints; names and
descriptions are the dev block's, schemas are the products contracts':

`shop_list_products`, `shop_create_product`, `shop_update_product` (sets
`status: active` — the public catalog filters on it), `shop_delete_product`,
`shop_restore_product`, `shop_list_groups`, `shop_create_group`,
`shop_list_offers`, `shop_create_offer`, `shop_update_offer`,
`shop_publish_offer`, `shop_archive_offer`.

Out: orders, refunds, payment links, sellers, presets, provider and Stripe
settings, users, roles, site settings.

Offer contracts embed the recursive `Condition`, which the producer refuses
today. Fixed at the source (§14): the self-contained schema builder keeps
`$defs` inside the tool schema instead of refusing recursion.

### 9.4 Rules

Names are stable and curated; input and output schemas are typed and
self-contained; every result includes `structuredContent`; mutating
descriptions state their side effects; compiler and validation errors are
`success: false` results; server auth and CSRF remain authoritative — hiding
a tool is never the security gate; no arbitrary fetch, route, shell, eval or
raw OPFS tool exists.

After an activation that changed the block set, `dev.js` re-fetches the
global manifest and re-registers those tools under a fresh `AbortSignal` so
the browser's `toolchange` event tells the agent about the new block's
tools. `webmcp.js` gains the same abort-and-re-register path and, where a
service worker is expected, waits on `navigator.serviceWorker.ready` before
its first manifest fetch — the cold-visitor race the browser-demo note
recorded.

## 10. Export and seed-on-boot

### 10.1 Export

`GET /b/dev/api/export` streams a zip (stored entries, written by the dev
block in Rust):

    README.md                     how to serve (npx serve, python -m http.server), the credentials,
                                  what is and is not inside
    index.html loader.js sw.js impresspress_web.js impresspress_web_bg.wasm vendor/…
                                  the runtime shell, read from the bundle's own asset manifest,
                                  with the bootstrap's dev flag rendered false and the compiler
                                  bypass prefix removed
    seed/manifest.json            schema_version 1, source generation, site files, blocks
                                  (name, artifact sha256, routes, capabilities, wafer_guest version)
    seed/site/**                  site files
    seed/blocks/<name>.wasm       artifacts
    seed/blocks/<name>/src/**     source, so the export is re-importable and editable
    seed/data.sql                 INSERT statements for an explicit table allowlist

`data.sql` allowlist: products, offers, groups, types, presets;
non-sensitive, non-infrastructure `variables` (e.g. `APP_NAME`,
`HAS_LANDING_PAGE`) and `block_settings`; `users` and `user_roles` (the
visitor's own sandbox accounts, password hashes included — they own them and
the README says so). Excluded: sessions, refresh and verification tokens,
audit log, purchases, refunds, payment links, provider operations, Stripe
events, webhook leases, and every variable that is sensitive.

The exported bundle serves the site with `cache_mode = "no-cache"` too: it
is a local preview and the user will re-export.

### 10.2 Seed-on-boot

On a cold boot with no active generation the service worker fetches
`/seed/manifest.json` (a bypass path). If present: verify every referenced
file's hash, write blobs and artifacts, apply `data.sql` after the built-in
migrations and admin init, and activate generation 0 (`cause = Seed`) through
the normal queue. dev.impresspress.org ships its welcome page this way; an
export ships the user's shop. One mechanism.

## 11. Persistent model

### 11.1 SQLite (dev-only migrations owned by `impresspress/dev`)

    impresspress__dev__generations     (§7.2)
    impresspress__dev__builds          id, block_name, source_manifest_sha256, artifact_sha256,
                                       block_info_json, diagnostics_json, compiler_version,
                                       status (Staged | Valid | Invalid), created_at
    impresspress__dev__runtime_state   singleton_id, active_generation_id, desired_generation_id,
                                       activation_phase, generation, updated_at

Table names follow the repo convention (`pub const TABLE` per repo module).

### 11.2 OPFS

    impresspress/dev/blobs/<sha256>
    impresspress/dev/artifacts/<sha256>.wasm
    impresspress/dev/workspace.json          current path → sha256 manifest
    wafer-run/web/site/**                    the activated site

The dev block holds an explicit WRAP grant for write access to
`@wafer-run/web/site` only. Hierarchical paths require the OPFS path fix from
the existing plan's §8, landed first.

### 11.3 Generation manifest

    {
      "schema_version": 1,
      "generation_id": "…", "parent_id": "…",
      "site": { "files": [{ "path": "index.html", "sha256": "…", "size": 1234, "content_type": "text/html; charset=utf-8" }] },
      "blocks": [{ "name": "site/newsletter", "artifact_sha256": "…",
                   "routes": [{ "prefix": "/b/newsletter/", "access": "Public" }],
                   "capabilities": { … BlockCapabilities … },
                   "wafer_guest_version": 1 }]
    }

Canonical JSON (sorted keys, no whitespace) is hashed into `manifest_sha256`.

## 12. HTTP contracts

All typed (`Serialize`, `Deserialize`, `JsonSchema`); every unsafe method
passes the existing CSRF policy and the admin router gate; every response is
`no-store`.

    GET  /b/dev                                 the workspace document (COOP/COEP)
    GET  /b/dev/api/status
    GET  /b/dev/api/tools.json
    GET  /b/dev/api/reference
    GET  /b/dev/api/files?prefix=
    POST /b/dev/api/files/read
    POST /b/dev/api/files/write
    POST /b/dev/api/files/delete
    POST /b/dev/api/blocks                      create from template
    POST /b/dev/api/blocks/{name}/remove
    POST /b/dev/api/builds/stage
    GET  /b/dev/api/generations?limit=
    GET  /b/dev/api/generations/{id}
    POST /b/dev/api/generations/{id}/rollback
    GET  /b/dev/api/export

The `shop_*` tools point at the products block's existing admin endpoints;
the dev block adds no products routes.

## 13. Security model for the sandbox

Kept from the existing plan: feature off by default and absent from normal
bundles; `IMPRESSPRESS__DEV__ENABLED` false by default and checked before
route registration; every `/b/dev` route Admin — except, since 2026-10-02,
the one-click entry page `/b/dev/enter`, which is Public and exists only in
a workspace (amendment 21); same-origin cookie + CSRF;
normalized relative paths with quotas; hash-verified content-addressed
blobs, artifacts and manifests; reserved names and collision checks;
deny-by-default capabilities; no network for guests; publication cannot write
the shell's files; last-known-good runtime and site; startup recovery;
validated compiler-worker messages; enforced resource limits; retained
source, logs and hashes.

Changed: no confirmation nonce — the ledger and `dev_rollback` replace it.
Added: `shop_*` tools exist only on `/b/dev`; the `/b/dev` page is trusted to
instruct the agent; the export's `data.sql` allowlist is a closed list in
Rust with a test that fails when a new products table appears without a
decision.

The prompt-injection boundary is unchanged: development and shop mutations
are registered only on `/b/dev`; the preview iframe is sandboxed; nothing a
shopper-facing page contains can reach the tools.

## 14. Producer (wafer-run) changes

One PR chain, landed and pinned before the consumer plans that need it:

1. **v1 JSON host calls** — §6.3.
2. **Table-scoped DDL ops** — §6.4; wire DTOs in `wafer-block::wire::database`,
   handler arms in `wafer-core::interfaces::database::handler`, builders
   already in `wafer-sql-utils::ddl`.
3. **`wafer-run/web` `cache_mode`** — `normal` (today's behaviour) or
   `no-cache` (every response `Cache-Control: no-cache`).
4. **Self-contained schemas that keep `$defs`** — the producer's
   self-contained builder retains `$defs` with rebased refs instead of
   refusing recursive types, and a projection API takes an explicit list of
   `(method, path, tool_name, description)` for page-scoped manifests. Also
   unblocks the 22 offer sites in `/openapi.json`.

Each is independently testable and none changes behaviour for existing
callers.

## 15. Hosting and deployment

`examples/dev-sandbox/` is the web-target consumer: `impresspress-web` with
`browser-devtools`, `impresspress.toml` with `[dev] enabled = true`,
`extra_bypass_prefix = ["/seed/", "/__impresspress_dev/compiler/"]`, the
welcome starter under `seed/`, built with `impresspress build --target web`.

Deployment is a Cloudflare Worker with static assets for the shell and an R2
binding for the compiler payload, both on `dev.impresspress.org`. Workers and
Pages cap single files at 25 MiB and Rubrc's rustc/llvm modules are expected
to exceed it; the plan measures the pinned revision's largest file and, if
under the cap, drops R2. Deploy config is `wrangler.toml` in the example, as
`examples/webmcp-demo` does; the CLI gains no target.

`Cross-Origin-Opener-Policy: same-origin` and
`Cross-Origin-Embedder-Policy: require-corp` are set by the service worker on
`/b/dev` responses only (the `coi-serviceworker` pattern; superseded by
amendment 14 — deployment-wide `credentialless`); the compiler
worker and its assets are same-origin, so no CORP headers are required. The
static host must serve `index.html` for unknown paths so a direct navigation
to `/b/dev` before the service worker exists still boots.

## 16. Verification

**wafer-run:** transcoder golden tests with a JSON-speaking guest; DDL
scoping — a guest with `collections: Only([a])` cannot create or drop `b`;
`cache_mode` header tests; `$defs` retention on a recursive type; selected
projection names and filters.

**impresspress unit:** manifest canonicalization and hashing; generation and
phase transitions; validation refusals (name, route, capability, duplicate
tool, trap); activation coalescing; journal recovery from every phase; site
publish diff and `index.html`-last ordering; retention and blob GC; export
zip contents; `data.sql` allowlist gate; `tools.json` under the snapshot gate;
feature-off: no routes, no migrations, no tools.

**browser (wasm-bindgen / Playwright):** nested OPFS; `Rc<Wafer>` retained
across an awaited request during a swap; service-worker restart reloads the
active generation; `crossOriginIsolated === true` on both `/b/dev` and `/`
(amendment 14 — deployment-wide `credentialless` when the sandbox is
active, not `/b/dev` alone); compiler worker start, cancel, crash recovery;
seed import on a fresh origin.

**The scenario e2e** (Playwright with the WebMCP polyfill `webmcp.spec.ts`
uses):

1. `/` shows the welcome page and credentials → log in → `/b/dev` registers
   exactly the expected tool set; no dev or shop tool on `/`.
2. `dev_create_block(newsletter, table)` → `dev_compile_block` → generation
   with a block; `POST /b/newsletter/subscribe` answers;
   `subscribe_newsletter` appears in the anonymous global manifest.
3. `dev_write_file(site/index.html, …)` and an asset → generation N; the
   iframe shows the new page.
4. `shop_create_product` ×3, `shop_create_group`, `shop_create_offer`,
   `shop_publish_offer`, `shop_update_product(status: active)`.
5. Fresh anonymous context → `/` lists the three products; the widget prices
   one; `list_products` returns them.
6. `dev_export` → unzip → serve statically → fresh context shows the same
   shop with the same products; `/b/dev` is absent.
7. `dev_rollback(N-1)` reverts the page; unregister the SW and reload → the
   active generation persists.

Step 2 needs Rubrc: the pinned payload lives in the CI cache and runs in its
own job; the fast suite covers activation with a precompiled fixture staged
over HTTP.

**Size and time:** outer wasm raw/opt/gz with and without `browser-devtools`;
compiler download, warm init and compile durations; runtime rebuild and
activation durations; explicit warning thresholds.

## 17. Phasing

| Plan | Repo | Delivers | Checkpoint |
|---|---|---|---|
| 0 | wafer-run | §14 items 1–4; impresspress pin bump | JSON guest round-trips DB/storage; DDL scoped; `$defs` kept |
| 1 | impresspress | hierarchical OPFS · `Rc<Wafer>` factory/manager · `browser-devtools` · dev block: blobs, generations, staging, auto-activation, journal · seed-on-boot | fixture block + site file activate over HTTP and survive restart; feature-off build unchanged |
| 2 | impresspress | `/b/dev` page · `tools.json` + `dev.js` · `dev_*` and `shop_*` tools · welcome starter · `examples/dev-sandbox` + deploy config · manifest re-registration | polyfilled agent builds a site and stocks the shop; shopper browses; deployed |
| 3 | impresspress | Rubrc adapter and worker · COOP/COEP · `wafer_guest.rs`, templates, reference · `dev_compile_block` · diagnostics · capability validation | newsletter block compiled in-browser, activated, its tool discovered and invoked |
| 4 | impresspress | export zip and `data.sql` allowlist · import · quotas and GC · docs · full scenario e2e · measurements | definition of done |

Plans 1 and 2 need only Plan 0's items 3 and 4; Plan 3 needs items 1 and 2.
Each plan is a reviewable PR chain; the default browser build never depends
on Rubrc.

## 18. Non-goals

- Editing built-in ImpressPress blocks or rebuilding the outer service worker
  in the browser.
- Cargo dependencies or procedural macros in guests (waits on Rubrc).
- TypeScript or framework frontends; a bundler worker is later work.
- Guests calling other blocks; guest network access.
- A cart; multi-product checkout.
- Payments in the sandbox (no Stripe key client-side).
- Deploying from the sandbox to Cloudflare or another origin — export is the
  hand-off.
- Multi-user collaboration; treating sandbox data as a protected secret.

## 19. Verification items carried into the plans

Facts assumed here that a plan must confirm before building on them:

- Rubrc's largest pinned asset size (decides R2 vs static assets, §15).
- That a service-worker-synthesized response's COOP/COEP makes the document
  `crossOriginIsolated` in current Chromium (the `coi-serviceworker`
  precedent says yes).
- That Rubrc's worker can be created from a same-origin module URL served
  by the host with the subordinate-worker paths intact after packaging.
- That `serde_bytes` fields in every wire DTO accept integer sequences
  (transcoder correctness).
- The current cost of re-running built-in block `Init` on a runtime rebuild;
  if material, add a reload lifecycle rather than skipping initialization.
- Whether the producer's schema builders needed for §14.4 are reachable
  without duplicating `self_contained_schema`.

## 20. Amendments from plan research (2026-09-02)

Facts found while writing the implementation plans that change details
above. Each is applied in the plans; the sections above are left as approved
and read together with this list.

1. **§3.5 / §6.3 — host-call codec is an explicit guest export, not the ABI
   version.** The existing v1-JSON test guests (`dispatch_guest`,
   `service_client_guest`, `attachment_dispatch`, `hostile_db_guest`) send
   MessagePack host-call payloads through the wafer-sdk while negotiating ABI
   v1, so keying the transcoder on `AbiCodec::V1Json` would break them. A
   guest opts in by exporting `__wafer_host_codec() -> i32` returning `1`
   (JSON); an absent export or `2` keeps MessagePack. `wafer_guest.rs`
   exports it; nothing else does. `stream_attach` is refused only for
   JSON-codec guests.
2. **§6.4 / §14.2 — schema ops mirror the trait that already exists.**
   `DatabaseService` already has `ensure_schema_table(&Table)`,
   `schema_add_column`, `schema_drop_table` and `schema_table_exists`, each
   owning its backend dialect. The wire ops are therefore
   `database.ensure_table` (the `TableDef` carries `indexes`, so there is no
   separate `create_index`), `database.add_column`, `database.drop_table`
   and `database.table_exists`. Authorization is two checks, both required:
   `(table, Db, write)` — WRAP namespace/grant plus `allows_collection` — and
   the existing `__ddl__` sentinel, which is what `caps.ddl` gates.
3. **§14 — a fifth producer item: framing.** `wafer-run/security-headers`
   emits `frame-ancestors 'none'` in a non-removable baseline and
   `X-Frame-Options: DENY` unconditionally, so the live-site iframe in §3.2
   is blocked today. The block gains a `frame_ancestors` config (`none`,
   the default, or `self`) that drives both headers. The sandbox sets
   `self`; nothing else changes. The sandbox CSP also adds
   `worker-src 'self' blob:` for the compiler worker (`merge_csp` passes
   `worker-src` through and strips `blob:` only from `script-src`).
4. **§15 — no R2.** Rubrc's own publish pipeline composes the four compiler
   modules into one `vfs.core-<hash>.wasm`, brotli-compresses it and splits
   it into 24 MiB parts with a JSON manifest, precisely to fit Cloudflare's
   25 MiB static-file cap. The sandbox packages those parts under
   `/__impresspress_dev/compiler/<version>/` as ordinary static assets.
5. **§14.4 — where `$defs` retention lives.** The producer side
   (`wafer-block`'s `self_contained_schema`) already keeps `$defs`; it is the
   consumer-side `inline_refs` in `wafer-core::discovery` that strips the
   table and refuses cycles. The change is there: keep reached definitions
   under the tool schema's `$defs`, rebase root `#` cycles to a named
   definition, hoist per-source tables into the merged input schema, and
   hoist into `components/schemas` for OpenAPI. The selected projection is
   `generate_webmcp_selected`, implemented by cloning the selected endpoints
   with a synthesized `AgentTool` and running `generate_webmcp_report`
   unchanged.
6. **§6.6 — the browser CSP** must carry `worker-src 'self' blob:` and
   `frame-src 'self'` for `/b/dev`; `'unsafe-eval'` in the current constant is
   already stripped by the block and `'wasm-unsafe-eval'` survives.

7. **§4.2 / Plan 2 — the preview iframe is same-origin, and `sandbox` grants
   it no isolation from the parent.** `sandbox="allow-scripts
   allow-same-origin allow-forms allow-popups"`: without `allow-same-origin`
   the framed site's calls to `/b/products/*` would be cross-origin and
   CORS-blocked, and the storefront widget dies — so `allow-same-origin` is
   unavoidable without a distinct preview origin. But `allow-scripts` plus
   `allow-same-origin` together on same-origin content means the framed page
   can read and write `parent.document.modelContext` (including
   `registerTool`, i.e. it can register its own tools ahead of the
   workspace's) and `parent.__impresspressWebmcp` (`webmcp.js` sets it, and
   `ui::layout` injects `webmcp.js` into `/b/dev` like every other SSR page —
   the parent does NOT expose nothing on `window`), and per the HTML spec
   such a frame can remove its own `sandbox` attribute and re-navigate
   itself out of the sandbox entirely. It also needs none of that: any
   script on `/` already runs same-origin with the admin's session cookie
   and a same-origin `fetch` passes the pipeline's Fetch-Metadata CSRF
   check, sandboxed iframe or not. What `sandbox` actually buys: no
   top-level navigation of the parent, no modals, no downloads, no
   pointer-lock/presentation — nothing about the parent's tools. The real
   model: the preview is trusted-equal to its parent; the isolation
   boundary is the browser-local instance itself (§2: `shop_*` tools exist
   only on the trusted `/b/dev` page of a throwaway browser-local instance),
   not the iframe.
   A distinct preview origin (or a credentialless frame, which would lose
   the service worker) is the actual fix and is a tracked follow-up.
8. **§8 / Plan 3 — `wafer_guest_version` comes from the page.** `BlockInfo`
   has no such field; the page reads the `WAFER_GUEST_VERSION` line of the
   vendored file and sends it in `StageBuildRequest`.
9. **§10 — the data snapshot is `seed/data.json`, not `data.sql`.** Rows per
   allowlisted table, applied through typed `upsert`/`create`/`delete_where`
   calls; no SQL text is generated or executed by the sandbox.
10. **§6.4 / §6.5 — a `schema` capability, not `ddl`.** wafer-run's raw
    `database.ddl` is gated by `caps.ddl` and runs arbitrary SQL, so granting
    `ddl` to a guest for `ensure_table` would also grant raw DDL. The producer
    adds `BlockCapabilities.schema` (structured schema ops on the block's own
    collections, sentinel `__schema__`) and the structured ops check
    `(table, Db)` **and** `__schema__`. Guests declare `schema: true`,
    `ddl: false`; validation refuses `ddl: true`. Storage-folder capabilities
    are prefixes (`site/<name>` admits `site/<name>/…`), and resources with
    `.`/`..` segments are refused at the capability layer and in the storage
    handler.

11. **§6.1 / Plan 1 Task 8 — block names have no underscores.** wafer-run
    rejects `_` in a block-name segment and `wrap::resource_owner` maps `_`
    to `-`, so `<name>` is `^[a-z][a-z0-9-]{1,31}$` (collections stay
    `site__<name>__*` with the hyphenated name inside). Guest validation is
    exhaustive over `BlockCapabilities` (a new producer field fails the
    build) and refuses any readable or writable `headers` policy (`masked`
    only narrows what a guest sees and is allowed); extra routes whose block
    declares endpoints are refined by the declared endpoint auth exactly like
    built-in routes; the executable half of validation is split into
    `inspect` (metadata under no capabilities) and `probe` (lifecycle + one
    request under the accepted capabilities), with the static rules between.

12. **§7.4 / Plan 1 Task 9 — the probe keys on host-call denials, not on
    traps.** The producer gives the host no trap signal it can attribute to
    a capability, so `probe` runs `Init`, `Start` and one request under a
    deny-all context that counts denials: a failure with at least one
    counted denial is reported as a capability problem, a failure with none
    is fatal. The residual gap is a guest that makes one host call and then
    traps in `Init` for an unrelated reason — it is excused as a capability
    problem. Closing it needs a producer-side trap signal.

13. **§7.3 / Plan 1 final review — restore reuses the retained runtime.**
    A failed activation restores the previous `Rc<Wafer>` that `rebuild`
    swapped out (`RuntimeControl::restore_previous`), never a second full
    rebuild; the retention window is the 20 most recent generation rows
    regardless of status, so an orphaned `Staged` row left by a crash
    between insert and the first journal write occupies a slot until it ages
    out. Seeded blocks pass the same capability and route rules as staged
    ones (`validate_spec`); only the rules that read the guest's `BlockInfo`
    (name mismatch, endpoint outside routes, duplicate tool name,
    `cap-requires` mismatch) wait for the runtime and are listed in
    `seed.rs`.

14. **§15 / Plan 2 Task 3 — cross-origin isolation is deployment-wide and
    `credentialless`.** A document whose COEP is not `unsafe-none` can only
    embed nested documents that also carry a compatible COEP; the HTML
    spec's navigation-response adherence check does not care about origin.
    So "COOP/COEP on `/b/dev` only" leaves the preview iframe of `/` blank.
    `wafer-run/security-headers` gains `cross_origin_isolation` ∈ {`none`
    (default), `credentialless`, `require-corp`} (wafer-run#327,
    `63891fac`); the browser runtime sets `credentialless` on every response
    when the sandbox is active, and `/b/dev` sets the same pair on its own
    document. `credentialless` rather than `require-corp` so an agent-built
    page can still show a cross-origin image whose host never set CORP.
    Safari does not implement `credentialless`, so it gets no isolation and
    no threaded compiler; the WebMCP browsers are Chromium-based.

    Cost to agent-built sites, not just to Safari: the adherence check above
    cuts both ways — a document with ANY non-`unsafe-none` COEP can only
    embed a nested document that ALSO carries a compatible COEP, and every
    document in the sandbox now carries one because it is deployment-wide.
    So a page the agent writes under `site/` cannot embed a third-party
    iframe at all — YouTube, a map, Stripe Embedded Checkout
    (`products/assets/storefront.js`'s `presentation: "embedded"`) — because
    none of those origins serve `Cross-Origin-Embedder-Policy`. Nothing
    regresses today: `script-src 'self' 'unsafe-inline'` already blocks
    `js.stripe.com`, and hosted checkout is a top-level redirect, which COOP
    does not touch. But "build me a shop" is exactly the workflow that meets
    this wall the moment embedded checkout or a video embed comes up, and an
    agent has no way to discover it short of the iframe silently failing to
    load — there is no error the sandbox can show, because the browser is
    the one refusing the navigation. This is the tradeoff the deployment-wide
    default accepts, not a defect in it; a page that genuinely needs a
    cross-origin embed has no workaround inside the sandbox today.

15. **§9 / §16 / Plan 2 Task 6 — a stable `/b/webmcp/webmcp.js` route.**
    `ui::layout` injects `webmcp.js` at the content-hashed
    `/b/static/webmcp-{hash}.js` (`ui::assets::webmcp_js_url()`) into every
    SSR page, but a page under `site/` is served verbatim by `wafer-run/web`
    and gets no injection — a visitor's agent sees no tools on an
    agent-built site, and the hashed URL is not discoverable outside an SSR
    document. Plan 2 Task 6's e2e worked around this by reading the hash off
    `/b/dev`. `pipeline.rs` now serves the same composed script at the
    stable, un-hashed `GET /b/webmcp/webmcp.js` (`ui::assets::
    WEBMCP_JS_STABLE_PATH`) — public, beside `/b/webmcp/manifest.json` — with
    `Cache-Control: no-cache` and an `ETag` of the script's short hash
    (`ui::assets::webmcp_js_hash()`, the same hash `webmcp_js_url()`
    embeds). Site pages must include
    `<script src="/b/webmcp/webmcp.js" defer></script>` to give visitors'
    agents the site's public tools; the `/b/dev` guide and
    `SUGGESTED_PROMPT` (`blocks/dev/page.rs`) tell the agent to, and the
    seed's `site/index.html` does.

16. **§6.2 / §7.4 / §8 / §13 / §15 / Plan 3 — the guest surface as built, and
    the rulings the plan settled.** Plan 3 landed `wafer_guest.rs`, the Rubrc
    packaging and the compile path; seven things it decided differ from, or
    are not written in, the sections above.

    *§6.2 overstates the module.* `db::` is
    `{ensure_table, create, get, list, update, delete, count}` — there is no
    `upsert`, no `create_table`, no `create_index` and no `add_column`.
    `ensure_table` takes a `TableDef` that carries its own indexes, which is
    the shape `DatabaseService` already had (amendment 2), so `create_index`
    would have been a second spelling of one wire op. `storage::{get, put,
    delete, list}`, `config::get` and `log::{error, warn, info, debug}` are as
    written. `Ctx` is a **unit type**: it carries no cancellation state and no
    handle, because nothing in the guest ABI can observe a cancelled request —
    it is the token that says a call is being made from inside a request or an
    `init`, and it is passed by reference so it can gain a body later without
    touching a signature. `log::*` is a direct host import
    (`__wafer_host_log`), not a cross-block call, so `wafer-run/logger` is not
    in `requires` and the module has no constant for it.

    *The service worker is the deployment's header layer.* Amendment 14 made
    cross-origin isolation deployment-wide but left "where" open. It is
    `sw.js`: the one thing that ships inside the bundle and sits in front of
    every same-origin request, where "every static host must be configured"
    has no enforcement point at all. Two consequences worth stating: every
    bypassed asset in a dev bundle now round-trips through the service
    worker's JS (streamed, not buffered), and this partly supersedes amendment
    14's account of where the isolation comes from — the browser runtime still
    sets the headers on what it renders, but the bypass list is covered by the
    worker.

    *`Diagnostic.code` is `Option<String>`.* rustc does not number every
    diagnostic, and a page that invented a code for the unnumbered ones would
    be a mapping layer over a wire that already says "absent". Every producer
    in the crate still sets one; the two page-produced diagnostics carry their
    own.

    *The guest `Method` has no `Put`.* Not a simplification — the runtime's
    endpoint type has no such method, so a block declaring one would fail to
    load. `Patch` for a partial update, `Post` for a replacement.

    *`--fast` composition, and who may use it.* `build-compiler.sh --fast`
    skips `wasm-opt` for local iteration and for the CI jobs that only need to
    know the compiler still works; `dist/manifest.json` records
    `"build": "fast"` and `verify-compiler-assets.mjs` refuses it unless
    `IMPRESSPRESS_COMPILER_ALLOW_FAST=1` says this is that job. The deploy
    path never sets it. The kind is recorded beside the composed component
    (`.build-kind`) rather than derived from the current invocation's flag,
    because phase 3 is skipped when the component already exists.

    *The adapter owns both compile budgets.* Start-up is guarded by a 360 s
    **silence** watchdog re-armed on every `progress`, not a ceiling — it must
    not pre-empt the worker's own 300 s per-step guards, and what it detects is
    a toolchain that has stopped talking. A compile gets 120 s, enforced
    page-side by sending `cancel` and terminating; the worker's own 10-minute
    limit is a backstop for a wedged shell.

    *§15 — the optimized `dist/` is a release asset.* Composing it is ~35
    minutes of `wasm-opt -Oz` peaking at 12.6 GB of RSS, which no
    GitHub-hosted runner has, and it is fully determined by `PIN.json`. So a
    developer builds it once per pin, publishes it as
    `compiler-dist-<version>.tar` on tag `compiler-<version>`
    (`compiler/pack-dist.sh`), and `deploy-dev-sandbox.yml` runs
    `compiler/fetch-dist.sh` — which verifies every file against the manifest
    inside, requires `"build": "full"`, and fails the deploy with instructions
    when there is no asset for the pin. The correctness-CI jobs try the same
    asset first and fall back to a cache and then to a `--fast` composition.

17. **§10 / Plan 4 Tasks 1–2 — the data snapshot's closed list, its
    provider-linkage reset, and its import order.** `data_snapshot.rs`'s
    `TABLE_ALLOWLIST`/`TABLE_EXCLUDED` between them must name every table
    the products, admin and auth blocks declare, and
    `every_declared_table_of_the_three_blocks_has_an_export_decision`
    (`tests/dev_data_snapshot.rs`) checks that closure against every table a
    `CREATE TABLE` statement in those blocks' migration directories actually
    creates, not against `BlockInfo.collections` alone — that advisory list
    had already fallen behind one real table
    (`impresspress__products__stripe_events`) once. `block_settings`
    (`admin_schema::BLOCK_SETTINGS_TABLE`) is on `TABLE_EXCLUDED`: it is this
    instance's own per-block enable flags and migration-hash tracking, not
    anything the visitor authored. On export, `reset_provider_linkage`
    clears every Stripe/provider-linkage column the three exported tables
    carry — `products.stripe_product_id`, `products.seller_account_id`,
    `offers.stripe_product_id`, `offers.stripe_price_id`,
    `offers.sync_status`, `offers.sync_error`, and
    `offer_components.stripe_price_id` — back to the same "not yet synced"
    defaults a brand-new row gets, because the importing instance has no
    Stripe account the exported ids belong to. `Mode::Replace` tables
    (`users`, `local_credentials`, `user_roles`) import in the fixed
    `REPLACE_ORDER`, not the snapshot's own incidental alphabetical order —
    `local_credentials.user_id` and `user_roles.user_id` are meaningful only
    once the `users` row they name exists, and alphabetically
    `"…user_roles"` sorts before `"…users"`. `import` is **not atomic**:
    `wafer_core::clients::database` exposes no cross-call transaction, so a
    crash partway through leaves whatever tables were already written in
    their new state and the rest in their old one — worth a `wafer-run`
    follow-up (a transaction/batch op), not a workaround here. It is
    idempotent instead: every write is keyed on the snapshot's own row ids,
    so re-importing the same snapshot converges rather than duplicates.
    `seed/data.json` is not a bare path — `SeedManifest.data` is an
    `Option<SeedFile>`, the same type every site file and block source entry
    in the bundle uses, so `seed::import` verifies its hash, size and
    content type exactly as it does for everything else instead of trusting
    one file unchecked.

18. **§7.2 / §7.3 / Plan 4 Task 4 — what retention keeps, and what "in
    flight" means at boot.** `retention::retained` (superseding amendment
    13's "a `Staged` row occupies a slot until it ages out") is the union of
    three sets, deduplicated by id: the 20 newest generation rows
    (`RETAINED_GENERATIONS`), the row that is currently `Active` however far
    it has fallen down the ledger, and every row still in flight (`Staged`,
    `Validating`, `Activating`) — because the activation journal may name
    one and boot convergence re-runs it. Everything outside that union is
    deleted, not relabelled; `Superseded` (`GenerationStatus::Superseded`,
    written only by the activation that replaces a row) is the status of a
    generation a later one supersedes, never an ageing marker — a `Failed`
    generation still reads `Failed` for as long as the ledger keeps it.
    `gc`'s reachability follows the same shape: a **blob** is reachable from
    a retained generation's site manifest *or* from a live workspace entry
    (a block's source tree lives in the workspace and in no generation at
    all), and an **artifact** is reachable from a retained generation's
    block manifest *or* from a build row still `Staged` — a compile that has
    stored its bytes but not yet reached a generation. Both roots are read
    only after the candidate listing, never before (`gc.rs`'s ordering
    invariant), so nothing stored after the listing starts needs a root to
    protect it. At boot, `activation::retire_abandoned` closes out what the
    previous process left running: every in-flight generation row the
    journal's `desired_generation_id` does not name is marked `Failed`
    (`ABANDONED_AT_BOOT`), and `repo::builds::retire_in_flight` settles every
    in-flight build row against the artifact hashes the active and
    journalled generation manifests vouch for — a vouched-for row is
    promoted (its `BlockInfo` is what a later collision check reads back),
    everything else is retired. Left alone, either would pin content against
    the workspace's 64 MiB quota (§6.6) for the life of the instance.

19. **§10.1 / §10.2 / §21 / Plan 4 Task 3 review — the exported runtime: the
    FEATURE decides the runtime, the FLAG decides the workspace.** §21 says
    "the exported bundle has it off", and the export duly renders
    `const DEV_ENABLED = false;` into `sw.js`. But `dev_active` was
    `feature_compiled && requested`, and EVERYTHING keyed on it — including
    `dev_runtime::install`, which is the only caller of `seed::import`. So an
    exported bundle imported nothing: the `seed/` the archive ships beside the
    shell was never read, `/` had no site, and the folder came up empty,
    contradicting §10.2 ("on a cold boot with no active generation the service
    worker fetches `/seed/manifest.json`" — a property of the deployment, not
    of whether anyone can edit it), §10.1's whole point, and the README the
    export writes.

    The gate is therefore split in three
    (`impresspress-web`'s `SandboxMode`): `Absent` (feature off — §13's
    "the sandbox is absent, not merely disabled" is unchanged), `Exported`
    (feature on, `dev: false`) and `Workspace` (feature on, `dev: true`).
    With the feature compiled in, the sandbox's **runtime** half always runs:
    the `impresspress/dev` block is registered (which is what runs its
    migrations and creates the ledger), its WRAP grants are handed over (which
    is what lets the boot path write the published site and the data-snapshot
    tables), `wafer-run/web` keeps `cache_mode = "no-cache"` (§10.1 says the
    exported bundle serves the site that way "too"), the generation's dynamic
    blocks are registered with their routes, `WAFER_RUN_SHARED__HAS_LANDING_PAGE`
    is force-set, and boot runs seed import → journal convergence → the
    rebuild. Only the **workspace** half is keyed on the flag: the `/b/dev`
    route, the widened CSP (`worker-src 'self' blob:; frame-src 'self'`),
    `frame_ancestors: self` and `cross_origin_isolation: credentialless`.
    (`IMPRESSPRESS_DEV_ENABLED` is no longer written to the database — the
    Plan 1 review fix removed it; the flag lives in the bundle's `sw.js`
    only. `HAS_LANDING_PAGE` is force-set in BOTH directions, `"false"` when
    the runtime is absent, so a stale `"true"` from an earlier sandbox on the
    same origin cannot outlive it.)

    Two consequences worth stating. An exported site *loses* cross-origin
    isolation — it has no compiler needing `SharedArrayBuffer` and no preview
    iframe to keep loadable, so it needs none — and gets back what isolation
    cost: a page an agent built there can embed the third-party iframe
    amendment 14 says the sandbox cannot (a video, a map, Stripe Embedded
    Checkout). And the dev block is *registered but
    unrouted* in `Exported`: §13's gate is the router ("not any check inside
    any handler"), and an unrouted block has no HTTP surface, so `/b/dev` is a
    404 there. It remains visible on `/b/admin/blocks` and in `/openapi.json`
    to that site's own admin, which is cosmetic and the accepted cost of
    running the block's real lifecycle rather than reproducing its migrations
    from the boot path under a grant on another block's table.

20. **§4.2 / §6.1 / §6.2 / §8 / amendment 8 — the guest SDK is a crate, built
    once per session (2026-09-30).** The design is
    `2026-09-29-dev-block-compile-speed-design.md`; this is what it changes
    here. §6.1's premise that Rubrc shares code only through in-crate modules
    was wrong: a path dependency builds and links fine, so the vendored
    `src/wafer_guest.rs` was a workaround for a constraint that did not
    exist. The SDK is now `crates/wafer-guest` (package `wafer_guest`, still
    std-only, `WAFER_GUEST_VERSION` still 2), a block is two files —
    `Cargo.toml` with `wafer_guest = { path = "../../wafer_guest" }` and a
    `lib.rs` that calls `wafer_guest::export!(block, init)` for the ABI
    exports §6.2 lists — and the crate lives beside the blocks, not in the
    workspace: `GET /b/dev/api/guest` serves it, and an export archive
    carries it at `seed/wafer_guest/`. §4.2's background prefetch is now real:
    the page starts the toolchain on load, or when the first block is
    scaffolded. §8's pipeline gains a session step: the worker is started
    with the guest crate and a warm-up block, builds the guest once into a
    target directory every block shares, and forces each block's rebuild
    with `cargo clean -p <crate>` rather than cleaning the whole target
    (Rubrc's VFS does not move mtimes, and `touch` was tried and does not
    work). Amendment 8's version now comes from the guest the page handed
    the compiler (the API's `version`), not from parsing a file in the
    workspace, and the `wafer-guest-version` refusal's remedy is to reload
    the page and compile again.
21. **§3 decision 7 / §4.1 / §13 — one-click entry (2026-10-02).** A
    visitor's ChatGPT agent, in a cloud browser, could not get past the login
    form; the owner decided the sandbox has no sign-in step. `GET
    /b/dev/enter` is a `Public` row of the dev block's route table. It is an
    HTML page, not a redirect, because the sandbox's runtime answers from a
    service worker and a synthetic response's `Set-Cookie` is not persisted —
    the login page sets its cookie from the login response's JSON for the
    same reason. The page is rendered with the bootstrap admin's email and
    password, read at request time from the config keys the auth block reads
    (`WAFER_RUN_SHARED__AUTH__BOOTSTRAP_ADMIN_EMAIL` / `_PASSWORD`, seeded
    for the browser build by `impresspress-web/src/config.rs`); its script
    posts them to `POST /b/auth/api/login`, sets the `auth_token` cookie as
    the login page does, and navigates to `/b/dev`. No token is minted
    outside the auth block's login, and the values are not written a second
    time anywhere (the workspace guide prints them from the same read). A
    visitor who already holds an admin session — `GET /b/dev/api/status`
    answers 200 — is sent straight to `/b/dev` without signing in again.
    When that login is refused — the admin's password was
    changed in this instance — or no password is configured, the page says
    one-click entry is off and links the normal login page.
    **The boundary.** The row exists only where the workspace does:
    `dev::WORKSPACE_ROUTES` registers `/b/dev/enter` (`Public`) ahead of
    `/b/dev` (`Admin`) in `SandboxMode::Workspace` and nowhere else. An
    exported bundle (`SandboxMode::Exported`) registers the block through
    `DevBlock::runtime_only`, which declares no endpoints, is given no
    route, and refuses every request in `handle`; a build without
    `browser-devtools` has no dev block at all. So an exported site, and
    every non-sandbox deployment, answers `/b/dev/enter` with a 404. The
    page is public only to the one visitor whose browser the instance lives
    in, and what it carries is what the welcome page already prints.
22. **§4.2 / §9.1 — the Tool console (2026-10-02).** The same agent would
    have had no tools had it signed in: its browser has no WebMCP. `/b/dev`
    gains a "Tool console" pane — a select of every tool the page publishes
    (`tools.json`'s plus the page-local `dev_compile_block` and
    `dev_export`), the selected tool's description and input schema, a JSON
    arguments box pre-filled with the schema's required properties, a Run
    button and a result box (`#dev-console`, `-tool`, `-args`, `-run`,
    `-result`). It is not a second implementation: `dev.js` publishes each
    tool once, onto one list, hands that list's entries to
    `document.modelContext` where it exists, and the console calls the same
    entry's `execute` — the same request builder (`webmcp-core.js`), session
    check and, for a mutating tool, progress panel and catch-up. The tools
    are fetched whether or not the browser has WebMCP, and the guide pane
    says which way they are reachable: "This browser has no WebMCP: use the
    Tool console below, or the file editor", or that the tools are
    registered. Both seeds' guides and suggested prompts point an agent
    without WebMCP at the console.

## 21. Definition of done

- `browser-devtools` is off by default and absent from a normal bundle; the
  exported bundle has it off.
- dev.impresspress.org serves the welcome site; login leads to a
  cross-origin-isolated `/b/dev` with exactly the documented tools.
- A WebMCP agent edits site files, scaffolds and compiles a Rust block in
  the browser, and sees it activated with structured progress — without
  confirmation clicks and without a rebuild of the outer service worker.
- Compile failures return usable diagnostics; validation refusals are
  structured; the previous generation stays live on any failure.
- The activated block's tool appears in the global manifest and can be
  invoked; rollback removes it and restores the site.
- The agent stocks the shop through `shop_*` and an anonymous shopper
  browses and prices the products.
- Export produces a bundle that boots the same shop from a local static
  server.
- The active generation survives service-worker restart; an interrupted
  activation converges on the next boot.
- The full unit, browser and scenario suites pass; sizes and durations are
  recorded with thresholds.
