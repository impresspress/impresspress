# The dev sandbox

The dev sandbox — `dev.impresspress.org`, and one more address for each
other [template](#templates) — is a browser-local sandbox for building an
ImpressPress site with an AI agent: one whose browser has WebMCP, or one
that can only drive the page (see [Without WebMCP](#without-webmcp)). Everything runs in your
browser tab: the ImpressPress service worker, an in-browser SQL database,
OPFS (Origin Private File System) storage for the workspace, and — once you
compile a backend block — an in-browser Rust-to-WebAssembly compiler. There
is no server behind any of it. When you like the result, one export
downloads it as a static bundle you can serve yourself.

Every visitor who opens the page gets their own instance: their own service
worker and their own OPFS database, created fresh on first load. Nothing you
do — writing a page, compiling a block, stocking the shop — reaches this
codebase, any other visitor, or any server. The only thing every visitor
shares is the static welcome bundle the site first boots with.

## Opening it with an agent

You need a Chromium-based browser (see
[Browser requirements](#browser-requirements) below); WebMCP support is what
lets an agent call the tools directly, and an agent without it uses the
[Tool console](#without-webmcp). Open the site and follow **Open
workspace**: it goes to `/b/dev/enter`, which signs you in as the sandbox's
admin and lands you on `/b/dev`, the workspace, with nothing to type.

When the session expires, open `/b/dev/enter` again. The instance's admin
account is a throwaway — this is a per-browser, per-visitor instance with
nothing of consequence behind it — and the workspace page shows the email and
password this instance is configured with, for the login form at
`/b/auth/login`. No page or document prints a fixed password, because it can
be changed: if you change the admin's password in your instance, one-click
entry stops working for it — the entry page says so and links the login page.

An agent that has not opened the sandbox yet reads `/llms.txt`: what the
sandbox is, how to get in, the two ways to call the tools, and the site
guide. The static host serves it, so it is there for a client with no service
worker and for one that runs no JavaScript — which is told there that it
cannot build anything and must say so. The boot page (what every path
answers with until the service worker is installed) says the same in two
sentences and links both `/llms.txt` and `/b/dev/enter`. Once the sandbox is
running, `/llms.txt` is answered by the runtime: the same text until the site
has an `llms.txt` of its own (`site/llms.txt`), and that file from then on.
Opening `/llms.txt` in a tab shows the text in every state: before the
service worker is installed, while the boot page is still starting (the boot
page lets a navigation that has begun finish rather than reloading itself
over it), and once the runtime answers. The sandbox's text is plain ASCII
(`seeds/check-seeds.py` refuses anything else), because a static host serves
it with no charset and a browser would not decode it as UTF-8.
A sandbox opened in a browser before this existed picks the text up on its
next load. An [export](#export) never carries the sandbox's text or its boot
page wording — the exported boot page is titled with the site's own name —
and a site's own `llms.txt` is exported twice: for the exported runtime, and
at the folder's root so a static host serves it to readers that run no
JavaScript.

*Changed 2026-10-02: the link used to lead to the login form, and the visitor
or their agent typed the credentials. One-click entry exists only in the
sandbox's workspace; an [exported bundle](#export) has no `/b/dev/enter`.*

Have your agent call `dev_status` first — it
reports the active generation, the compiler state and a summary of the
workspace — then it can read and write files, scaffold and compile backend
blocks, stock the shop, and export.

The page includes a "Suggested prompt" you can copy and paste — each
template's own. The prompt comes from the sandbox's seed (its
`sandbox.json`), so the panel is absent when the seed carries none; both
templates' prompts walk an agent through building a small shop end to end: a
home page, three products, a published offer for each, and a script tag that
gives a visitor's *own* agent the shop's tools once the page is live.

## Templates

A sandbox is seeded from a **template**: what generation 0 holds, and the
site-authoring guide `dev_read_reference` serves as `site_markdown`. Each
template is its own sandbox; pick one by opening its address.
https://impresspress.org/build always opens the default sandbox, so a link
to it keeps working when the default changes.

| Template | Sandbox | What it ships |
|---|---|---|
| bootstrap (default) | https://build-bootstrap.impresspress.org | Bootstrap 5 vendored under `site/vendor/bootstrap/`, a Bootstrap-built welcome page, a guide to the framework and the shop pieces |
| blank | https://dev.impresspress.org | A minimal welcome page and stylesheet, the same guide without a framework |

`dev_status` reports the template's name, and the workspace page names it
in its guide pane, above the suggested prompt. There is no switching
templates inside a workspace: a template is a starting point, and
everything after it is yours.

## The workspace

`/b/dev` has a file tree and editor, the live site rendered in an iframe
that reloads after each change, a progress/log panel, and a Tool console.

The workspace has two areas:

- `site/` — published verbatim to the live site. Writing or deleting a file
  under `site/` (`dev_write_file`, `dev_write_files`, `dev_delete_file`)
  publishes immediately.
- `blocks/<name>/` — a backend block's Rust source. Writing here only stages
  source; nothing runs until the block is compiled (see
  [Backend blocks](#backend-blocks) below).

`dev_list_files` and `dev_read_file` read the workspace; a write or delete
takes the file's last-seen `sha256` as `expected_sha256`, so an agent never
overwrites an edit it hasn't read. `dev_write_files` writes several files at
once — all under `site/`, publishing one generation, or all under one
`blocks/<name>/`, staging only. Every hash is checked before anything is
written; any mismatch refuses the whole batch and lists every conflict.

### Generations, rollback and retention

Every successful change — a site write or delete, a block compile, removing
a block, or a rollback — creates a new **generation** and publishes it
immediately; there is no separate confirmation step. `dev_list_generations`
lists the ledger newest-first; `dev_get_generation` reads one generation's
full manifest and what it changed relative to the one it came from;
`dev_rollback` republishes an earlier generation as a *new* one — rollback
never rewrites history, it appends a generation that copies an old one's
files and blocks.

The ledger doesn't grow forever: it keeps the 20 most recent generations,
plus whichever one is currently live (however far it has fallen behind) and
any generation still mid-activation. Everything else is deleted, and that's
also the practical bound on how far back `dev_rollback` can reach. A
generation's `Superseded` status only means a later generation replaced it —
it isn't a countdown to deletion, and a generation still shows its real
outcome (`Active`, `Failed`, and so on) for as long as the ledger keeps it.

### The progress panel

Every mutating tool call reports the same phases the panel shows live —
validating, rebuilding the runtime (only when the block set changed),
publishing, active — so you can watch, and diagnose, what an agent's change
is doing without leaving the page.

### Without WebMCP

An agent whose browser has no WebMCP — a cloud browser driving the page, say
— is not handed the tools, so the page offers them as controls. The **Tool
console** lists every tool the page publishes (`#dev-console-tool`), shows
the selected tool's description and input schema, takes its arguments as
JSON (`#dev-console-args`, pre-filled with the required properties), and on
**Run** (`#dev-console-run`) shows the result and whether it is an error
(`#dev-console-result`). It runs the very functions a WebMCP agent's calls
run, so a write from the console publishes, refreshes the file tree and
reloads the preview exactly as a tool call does. The guide pane at the top
of the page says which case you are in: "This browser has no WebMCP: use the
Tool console below, or the file editor", or that the tools are registered.

## Backend blocks

A backend block is a small Rust crate under `blocks/<name>/`, compiled to
WebAssembly in the browser and run by an in-process WebAssembly
interpreter — a normal ImpressPress block, minus everything that needs a
crate registry:

    blocks/<name>/
      Cargo.toml            crate-type cdylib, opt-level "z", panic = "abort",
                            one dependency: wafer_guest = { path = "../../wafer_guest" }
      src/lib.rs             your code: declares the block, its endpoints and agent tools

The SDK is the `wafer_guest` crate — ABI plumbing, request/response types,
database/storage/config/log calls, a JSON-schema builder, and the `export!`
macro a block's `lib.rs` calls to wire its `block()` and `init()` to the
host. It is not part of the block and not a workspace file: the sandbox
serves it at `GET /b/dev/api/guest`, and the compiler places it at
`../../wafer_guest` beside the blocks, the one path every block's manifest
names.

`<name>` matches `^[a-z][a-z0-9-]{1,31}$`; the block is registered as
`site/<name>` and its routes live under `/b/<name>/`. Only Rust's standard
library and `wafer_guest` are available — no crates.io dependencies and no
procedural macros, because the in-browser compiler doesn't do dependency
resolution. A block can read and write its own database tables and its own
storage folder, read config, and log; it cannot reach the network, and it
cannot call another block.

### Starting one

Don't write those two files by hand — `dev_create_block` writes them, from
one of two templates:

- `hello` — one public `GET` and nothing else, the smallest block that
  serves something;
- `table` — a newsletter block: a claimed collection, a table created in
  `init`, a public write endpoint with an agent tool, and two admin reads.

Both come out already carrying what an agent writing them by hand would have
to get exactly right: the path dependency on `wafer_guest`, the
`wafer_guest::export!(block, init);` line, and the block's name everywhere
it has to appear at once: the crate name, the block id `site/<name>`, the
route prefix `/b/<name>/`, the collection prefix `site__<name>__` and the
config prefix `SITE__<NAME>__` (a hyphen in the name is `_` in those two
prefixes, as the runtime spells a block's resources: `my-shop` owns
`site__my_shop__*` and `SITE__MY_SHOP__*`).

Scaffolding only stages source, the same as any other write under `blocks/`;
nothing serves until the block is compiled. If anything already exists under
`blocks/<name>/` the call is refused rather than overwriting — a directory
with a stray file in it is a block someone started, and writing a template
over it would leave a crate that is neither.

`dev_read_reference` returns the authoring guide: the block API, the host
services (database, storage, config, logging), what each refusal diagnostic
means, the limits, and the complete source of both templates — spliced
in at render time from the very files `dev_create_block` writes, so the guide
cannot drift from them. Read it before writing Rust.

The same response carries `site_markdown` — the site-authoring guide this
sandbox's seed ships: the page skeleton, the shop pieces, what a write
refuses — `template`, the seed's name (`dev_status` reports it too), and
`suggested_prompt`, the task the workspace page suggests for that template
(empty when it suggests none or the sandbox has no seed). Read
`site_markdown` before writing under `site/`.

### Compiling one

The Compile button — or the `dev_compile_block` tool — compiles
`blocks/<name>/` in the browser. The toolchain (about 72 MiB, downloaded
on first visit) downloads and starts in the background as soon as the
workspace has a block — on page load, or when the first block is
scaffolded — and a workspace with no block never loads the compiler.
Starting a session includes building the `wafer_guest` crate once, about
30 seconds; a compile that arrives before that has finished waits for it.
After that, each compile rebuilds only the block's own crate and takes a few
seconds (measured: about 2 seconds for the `hello` template and about 6 for
`table`). A block from before the SDK was a crate — three files, with its own
`src/wafer_guest.rs` — still compiles, rebuilding its copy every time, in
about 20 seconds, and staging still checks it against the guest version its
own vendored copy states. A failed compile returns diagnostics (file,
line, column, message) without touching the live site; a successful one is
validated, staged and activated automatically, the same as any other
change.

Limits: at most 16 blocks per workspace; one compile at a time, with a
120-second timeout; a compiled block's artifact must be 4 MiB or smaller; a
source file (workspace-wide, not just under `blocks/`) must be 512 KiB or
smaller.

## Stocking the shop

The `shop_*` tools are curated projections of the products admin API,
registered only on `/b/dev`:

`shop_list_products`, `shop_create_product`, `shop_update_product`,
`shop_delete_product`, `shop_restore_product`, `shop_list_groups`,
`shop_create_group`, `shop_list_offers`, `shop_create_offer`,
`shop_update_offer`, `shop_publish_offer`, `shop_archive_offer`.

A new product starts in `draft` and is invisible to shoppers until
`shop_update_product` sets `status: "active"`. A new offer starts in
`draft` and is unpurchasable until `shop_publish_offer` publishes it.
Orders, refunds, payment links, sellers, provider/Stripe settings, users,
roles and site settings are deliberately out of reach of these tools — an
agent working on `/b/dev` can build and price a catalog, not move money or
touch accounts.

Anyone who opens `/` — the same browser, or an incognito window — sees the
published catalog as an anonymous shopper. There's no cart, and no real
checkout: without a Stripe key configured, starting a checkout returns an
honest error instead of pretending to take payment.

## Export

The Export button — or the `dev_export` tool — downloads a zip you can
serve from any ordinary static file host. (`dev_export_manifest` answers the
same question without downloading anything: every file the bundle would
carry, with its size, and how many rows of each data table.) The zip holds:

- the runtime shell (the same files the sandbox itself serves, with the
  developer-mode flag turned off and the in-browser compiler's assets left
  out entirely);
- `seed/site/**` — your site files;
- `seed/blocks/<name>.wasm` and `seed/blocks/<name>/src/**` — every backend
  block's compiled artifact *and* its source, so the export stays editable
  and recompilable, not a binary drop;
- `seed/wafer_guest/**` — the guest SDK crate the blocks depend on by path,
  whenever there is at least one block, so `cargo build --release --target
  wasm32-wasip1` inside `seed/blocks/<name>/` works on your own machine;
- `seed/data.json` — a snapshot of your shop's data;
- a `README.md` explaining how to serve it and what it contains.

**What `data.json` carries:** products, offers, groups, types, templates
and presets, non-sensitive site configuration, and your own admin account —
`users`, its password hash, and its role assignment, `Replace`d as a set so
a fresh copy's own bootstrap admin is gone once yours is imported. You own
that account, and the export's README says so.

**What it deliberately leaves out:** everything scoped to *this running
instance* rather than to the shop — orders, purchases, refunds,
entitlements, payment links, seller accounts, Stripe/provider operations
and webhook events, sessions, tokens, API keys, and the audit log. On the
rows that are exported, every Stripe/provider-linkage column (a product's
`stripe_product_id` and `seller_account_id`; an offer's `stripe_product_id`,
`stripe_price_id` and `sync_status`; an offer component's `stripe_price_id`)
is reset to "not yet synced" — those ids point at Stripe objects belonging
to *this* instance, not wherever you re-host the export.

Importing runs only on a fresh instance (nothing published yet). It applies
`users`, then `local_credentials`, then `user_roles` — in that order, so
each row's `user_id` already exists by the time it's referenced — and
upserts every other table by id, so importing the same export twice
converges to the same state instead of duplicating rows. This isn't wrapped
in one database transaction: the typed database client the sandbox uses
doesn't expose a cross-call transaction, so a crash partway through an
import can leave some tables updated and others not. It's safe to just
import the same bundle again — every write is keyed on the snapshot's own
row ids.

To serve an exported bundle, unzip it and point an ordinary static file
server at the unzipped directory:

```sh
python3 -m http.server -d <unzipped dir>
```

Then open the printed URL and sign in with your account. The exported
bundle always boots with the in-browser workspace turned off — there is no
`/b/dev` on it, and no in-browser compiler.

**Change the admin password before serving an export anywhere but
localhost.** Every sandbox is seeded with the same starter admin account
(its email and password are shown on the workspace page), and the export
carries that account — so until you change it, anyone who gets the folder, or who can
reach the host you serve it from, is an admin of the exported site. Sign in
as that admin and change it on `/b/userportal/security`. The
"they're throwaway because this is a per-browser instance with nothing of
consequence behind it" premise stops holding the moment a copy of the
instance leaves the browser.

`seed/data.json` carries the user accounts, including their password
hashes — PBKDF2-HMAC-SHA256, since the sandbox is a browser deployment (the
native binary's Argon2id is too slow in wasm). They are in there so signing
in to the copy works with the same credentials; treat the folder the way you
would treat any export of an account table.

## Browser requirements

`/b/dev` has to be cross-origin isolated: the sandbox sets
`Cross-Origin-Opener-Policy: same-origin` and
`Cross-Origin-Embedder-Policy: credentialless` so the document gets
`crossOriginIsolated`, which is what makes `SharedArrayBuffer` — and so the
in-browser Rust compiler — available. The workspace and its database live
in OPFS.

In practice that means a **Chromium-based browser** — with WebMCP support
for an agent to call the tools directly, or without it through the
[Tool console](#without-webmcp). Safari does not implement the `credentialless` cross-origin-embedder-policy
mode the sandbox relies on, so it gets no cross-origin isolation and no
in-browser compiler. Firefox is untested.

## Resetting

Clearing this site's data in your browser (or opening it in a fresh or
incognito profile) throws the instance away completely: the service worker,
the database, everything under `site/` and `blocks/`. There is no
server-side copy to fall back on — export first if you want to keep
anything.

## Known limits

- No crates.io dependencies for backend blocks — standard library only, and
  no procedural macros.
- No network access from a backend block, and no cross-block calls — a
  block's *frontend* talks to other blocks over HTTP like any page does.
- No cart, no multi-product checkout.
- No real payments — without a configured Stripe key, checkout returns an
  honest error rather than pretending to charge.
- No collaboration — one visitor's browser is one instance, with nothing to
  share it with.
- Workspace quotas: up to 2,000 files, 512 KiB per file, 64 MiB of stored
  content in total, and 16 backend blocks.
- `dev_write_files` writes at most 64 files per batch; a larger change is
  several batches, and so several generations.
- No switching templates inside a workspace; each template is its own
  sandbox.

## See also

- [`docs/superpowers/specs/2026-09-02-dev-sandbox-design.md`](superpowers/specs/2026-09-02-dev-sandbox-design.md) —
  the full design, including every decision this guide only summarizes.
- [`examples/dev-sandbox/README.md`](../examples/dev-sandbox/README.md) —
  building, serving and deploying the sandbox bundle itself.
