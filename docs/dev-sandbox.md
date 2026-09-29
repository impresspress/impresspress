# The dev sandbox

`dev.impresspress.org` is a browser-local sandbox for building an ImpressPress
site with a WebMCP-capable AI agent. Everything runs in your browser tab:
the ImpressPress service worker, an in-browser SQL database, OPFS (Origin
Private File System) storage for the workspace, and — once you compile a
backend block — an in-browser Rust-to-WebAssembly compiler. There is no
server behind any of it. When you like the result, one export downloads it
as a static bundle you can serve yourself.

Every visitor who opens the page gets their own instance: their own service
worker and their own OPFS database, created fresh on first load. Nothing you
do — writing a page, compiling a block, stocking the shop — reaches this
codebase, any other visitor, or any server. The only thing every visitor
shares is the static welcome bundle the site first boots with.

## Opening it with an agent

You need a Chromium-based browser with WebMCP support (see
[Browser requirements](#browser-requirements) below). Open the site: the
landing page shows the throwaway admin credentials —

- Email: `admin@example.com`
- Password: `admin123`

— and why it's fine that they're public: this is a per-browser, per-visitor
instance with nothing of consequence behind it. Sign in and you land on
`/b/dev`, the workspace. Have your agent call `dev_status` first — it
reports the active generation, the compiler state and a summary of the
workspace — then it can read and write files, scaffold and compile backend
blocks, stock the shop, and export.

The page includes a "Suggested prompt" you can copy and paste, which walks
an agent through building a small shop end to end: a home page, three
products, a published offer for each, and a script tag that gives a
visitor's *own* agent the shop's tools once the page is live.

## The workspace

`/b/dev` has three panes: a file tree and editor, the live site rendered in
an iframe that reloads after each change, and a progress/log panel.

The workspace has two areas:

- `site/` — published verbatim to the live site. Writing or deleting a file
  under `site/` (`dev_write_file`, `dev_delete_file`) publishes immediately.
- `blocks/<name>/` — a backend block's Rust source. Writing here only stages
  source; nothing runs until the block is compiled (see
  [Backend blocks](#backend-blocks) below).

`dev_list_files` and `dev_read_file` read the workspace; a write or delete
takes the file's last-seen `sha256` as `expected_sha256`, so an agent never
overwrites an edit it hasn't read.

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

## Backend blocks

A backend block is a small Rust crate under `blocks/<name>/`, compiled to
WebAssembly in the browser and run by an in-process WebAssembly
interpreter — a normal ImpressPress block, minus everything that needs a
crate registry:

    blocks/<name>/
      Cargo.toml            crate-type cdylib, opt-level "z", lto, panic = "abort", no dependencies
      src/lib.rs             your code: declares the block, its endpoints and agent tools
      src/wafer_guest.rs      vendored support module — ABI plumbing, request/response types,
                              database/storage/config/log calls, a JSON-schema builder

`<name>` matches `^[a-z][a-z0-9-]{1,31}$`; the block is registered as
`site/<name>` and its routes live under `/b/<name>/`. Only Rust's standard
library is available — no crates.io dependencies and no procedural macros,
because the in-browser compiler doesn't do dependency resolution. A block
can read and write its own database tables and its own storage folder, read
config, and log; it cannot reach the network, and it cannot call another
block.

### Starting one

Don't write those three files by hand — `dev_create_block` writes them, from
one of two templates:

- `hello` — one public `GET` and nothing else, the smallest block that
  serves something;
- `table` — a newsletter block: a claimed collection, a table created in
  `init`, a public write endpoint with an agent tool, and two admin reads.

`src/wafer_guest.rs` is written byte for byte from the copy the sandbox
itself compiles against. It is about 1,500 lines of ABI plumbing that has to
be an exact copy — an agent asked to reproduce it from the reference would
get it approximately right, and the failure would surface as a trap inside
the interpreter rather than as a compile error. The other two files come out
already carrying the block's name everywhere it has to appear at once: the
crate name, the block id `site/<name>`, the route prefix `/b/<name>/`, the
collection prefix `site__<name>__` and the config prefix `SITE__<NAME>__`
(a hyphen in the name is `_` in those two prefixes, as the runtime spells a
block's resources: `my-shop` owns `site__my_shop__*` and `SITE__MY_SHOP__*`).

Scaffolding only stages source, the same as any other write under `blocks/`;
nothing serves until the block is compiled. If anything already exists under
`blocks/<name>/` the call is refused rather than overwriting — a directory
with a stray file in it is a block someone started, and replacing two of its
three files would leave a crate that is neither.

`dev_read_reference` returns the authoring guide: the block API, the host
services (database, storage, config, logging), what each refusal diagnostic
means, the limits, and the complete source of both templates — spliced
in at render time from the very files `dev_create_block` writes, so the guide
cannot drift from them. Read it before writing Rust.

### Compiling one

The Compile button — or the `dev_compile_block` tool — compiles
`blocks/<name>/` in the browser. First use downloads about 72 MiB of
compiler assets; after that, a cold start takes about 10 seconds and a
compile takes about 40 seconds. A failed compile returns diagnostics (file,
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
localhost.** The starter credentials above are printed in this file and on
every sandbox's own welcome page, and the export carries the account that
uses them — so until you change it, anyone who gets the folder, or who can
reach the host you serve it from, is an admin of the exported site. Sign in
as `admin@example.com` and change it at `/b/auth/change-password`. The
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

In practice that means a **Chromium-based browser with WebMCP support**.
Safari does not implement the `credentialless` cross-origin-embedder-policy
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

## See also

- [`docs/superpowers/specs/2026-09-02-dev-sandbox-design.md`](superpowers/specs/2026-09-02-dev-sandbox-design.md) —
  the full design, including every decision this guide only summarizes.
- [`examples/dev-sandbox/README.md`](../examples/dev-sandbox/README.md) —
  building, serving and deploying the sandbox bundle itself.
