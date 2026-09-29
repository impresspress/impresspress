# Design note: agent-editable front pages (`impresspress/pages`)

**Status:** design note, not a plan. Nothing is scheduled. Written 2026-08-28
out of the WebMCP work (#74–#81), which ended with the question "could an agent
change the site itself?".

**Do not start this before the WebMCP Challenge submission (2026-09-03).** It is
substantially larger than #79–#81 and it changes the UI layer rather than
annotating what exists.

---

## 1. The question

Could an agent update the site — content, layout, code — live, through WebMCP?

The honest answer depends entirely on which of those three you mean, because
they are not the same problem.

| Layer | Runtime-editable today? | Where it lives |
|---|---|---|
| Config / branding | **Yes** | `WAFER_RUN_SHARED__*` rows in D1, read per request by `SiteConfig::load` (`ui/mod.rs:38`) |
| Content | **Yes** | `legalpages` (draft → preview → publish), products/offers/groups |
| Layout | **No** | `ui/templates.rs` — six Maud templates compiled into the wasm |
| Code | **No** | wasm rebuild + the two-stage Cloudflare deploy |

So "change the copy and the branding" already works. "Change the layout" does
not, and no tool design fixes that: layout is code today. It becomes editable
only if layout first becomes **data**.

Actual code changes should never be a WebMCP tool. That is a coding agent
opening a PR against CI, with the deploy human-gated — a different product.
Conflating the two is where this idea usually goes wrong.

## 2. Sections, not a template language

Layout-as-data has two shapes, and only one of them is right here.

**Parameterised (recommended).** The database stores *choices*: which sections
appear, in what order, with what settings. Renderers stay compiled Maud.

```rust
#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Section {
    Hero { heading: String, image_url: String, align: Align },
    ProductGrid { columns: u8, group_id: String },
    Faq { items: Vec<FaqItem> },
}
```

A page is `{ "route": "/about", "sections": [ ... ] }`. Nothing from the
database is ever treated as markup.

**Template source in the DB (rejected).** Storing Liquid/Jinja text and
rendering it per request:

- Maud is a compile-time proc macro. There is no runtime template engine in the
  build; adding minijinja or Tera means a new dependency in a wasm bundle
  already at 5.56 MB against an 8 MB warn line, plus per-request parse cost on
  Worker cold starts.
- Maud escapes by default. A DB-sourced template renders DB text *as markup*, so
  every layout row becomes an XSS vector on our own origin — the
  `EMBEDDED_SCRIPTS` problem generalised to the whole page.
- It breaks the rule `ui/templates.rs` states in its own doc comment: "no
  bespoke page HTML outside this module."
- Agent-writability stops being reasonable-about. A typed field can be
  classified per variable; a template blob is all-or-nothing, and "all" means
  arbitrary markup and script.

The CMS world converged on the parameterised shape (Gutenberg block attributes,
Shopify section schemas) and moved away from free-form templates deliberately:
sections are diffable, validatable, migratable and safe.

**The derive machinery from #74–#81 makes this cheap.** One `JsonSchema` derive
per section type yields the admin form (`ui/settings_form.rs` already derives
forms from declared metadata), the `/openapi.json` schema, and the agent tool's
`inputSchema`. Each renderer is one Maud function.

### On off-the-shelf themes

Existing Liquid themes are **not** portable. A Shopify theme is written against
Shopify's object model (`product`, `collection`, `cart`, `settings`,
metafields) and its dialect (`{% section %}`, `{% render %}`, `{% paginate %}`,
`money`, `image_url`, `asset_url`). The Rust `liquid` crate implements core
Liquid, not that dialect. Running Dawn would mean reimplementing Shopify's
storefront domain model — the language is the easy 10%. Jekyll themes have the
same problem against a smaller model.

What *is* reusable: HTML/CSS design kits (Tailwind UI, Flowbite, HTML5 UP),
because markup and CSS carry no data-model dependency. Port them into Maud.
Shipping two or three themes ourselves is the realistic path; nobody inherits
Shopify's theme ecosystem for free.

## 3. Shape of the block

Everything needed already exists as a convention:

- **Opt-in block.** `impresspress_feature_block!` (`blocks/feature_block.rs`)
  plus a Cargo feature and the admin enable/disable toggle. Users who do not
  want it do not compile it.
- **Block-scoped config.** `IMPRESSPRESS__PAGES__*`, declared by the block via
  `.config_keys(...)`, exactly as `tickets` does. **`SiteConfig` stays general
  and does not grow.**
- **Per-page layout is data, not config.** Config vars are site-global
  key → value; a page document belongs in the block's own table
  (`impresspress__pages__pages`, per the `{org}__{block}__{name}` rule).

Three things get called "templates" — keep them apart:

1. **Section types** — the vocabulary. The real work. Start with four or five.
2. **Themes** — CSS variables plus section defaults. Cheap once sections exist.
3. **Page presets** — pre-filled section lists to start from. Also cheap.

### Name: `impresspress/pages`

Not `wafer-run/web-dynamic`, and **do not rename `wafer-run/web`**.

- The new block is an impresspress feature block (it needs the Maud UI kit,
  `SiteConfig`, and the products block for a `product_grid` section). Naming it
  as a pair with a wafer-run block implies a cross-repo symmetry that does not
  exist.
- Block names are named for what they are here — `products`, `tickets`,
  `legalpages`, `userportal`. Not for their mechanism.
- "web" is already overloaded: the `wafer-run/web` block, the
  `impresspress-web` crate, `crates/impresspress-web/pkg`.
- **Renaming `wafer-run/web` would be a data migration, not a rename.** Block
  names namespace storage (`blocks/storage.rs`): `store::get(ctx, "public", k)`
  resolves to `wafer-run/web/public/k`, cross-block reads use
  `@wafer-run/web/public`, and the CLI has the literal path
  `data/storage/wafer-run/web/site`. A rename relocates every stored asset in
  every deployment, plus `flows/site_main.rs` and the wafer-site / gizza-site
  consumers.

`wafer-run/web` is **not** replaced by this. It serves files you built and
deployed (R2 + immutable cache headers, unbeatable for static); `pages` renders
rows you edited. They compose — `pages` would still want static serving for
images, CSS and JS.

## 4. Two decisions to settle before any code

**Routing.** `routing.rs:182` states the convention: all block routes live under
`/b/{block}/...`. Marketing pages want `/about`, not `/b/pages/about`. And `/`
already has a special case (`routing.rs:530`): `HAS_LANDING_PAGE=true` hands off
to `wafer-run/web`, otherwise it redirects to portal or login. Adding `pages`
makes that a three-way choice, so `WAFER_RUN_SHARED__HAS_LANDING_PAGE` wants to
become a select (`ConfigVar` supports `InputType::Select` with `options`) —
something like `none | static | pages`. This touches shared routing, so it is
the part that is *not* purely additive.

**Chrome.** Does `pages` own only its own public pages, or restyle the global
shell? Owning its own pages keeps the dependency direction clean and the block
genuinely optional. Restyling the shell means core `ui/` consults an optional
block — an inverted dependency that has to degrade correctly when the block is
compiled out or toggled off (a layout-provider hook defaulting to today's
behaviour). Recommend the former: themes matter for storefront and marketing
pages, not for `form_page` and `dashboard_page`. Nobody wants a themable admin
table.

## 5. Letting an agent edit it

The section model is what makes this safe enough to consider; it does not make
it safe by itself.

The threat is specific, and it is why the WebMCP design spec put admin writes
out of scope: a tool's `execute` runs in the visitor's page with their session
cookie, same-origin, so it passes CSRF by construction. The agent decides what
to call by reading the page — and sellers control product descriptions. A
seller writes instructions into a description; an admin browses with an agent;
it executes with their authority.

**Agent proposes, human commits.** This is the shape that got `start_checkout`
approved: a write tool that is safe *structurally* because it returns a Stripe
URL and cannot take money.

- Draft-only verbs. `legalpages` already splits `save` from `publish`. Give the
  agent the draft verb; publishing stays human.
- A pending-changes table for config, surfaced as a diff to approve.
- Classify agent-writability **per variable** on `ConfigVar`, beside
  `input_type` / `warning` / `sensitive`, so each block declares which of its
  vars an agent may propose. That gets `PRIMARY_COLOR` without ever getting
  `EMBEDDED_SCRIPTS`.

Do not rely on confirmation semantics in the tool description. The spec requires
them and they are worth writing, but they are advisory — an agent can ignore
prose. They cannot be the only control.

The stronger version, later: the root problem is ambient authority, and the fix
is a non-ambient credential — an explicit, time-boxed "edit session" the admin
starts, which write tools must present. Then merely browsing with an agent
cannot write anything.

**Public pages are the right first write surface.** Failure mode is a bad
landing page: visible immediately, revertible in one row, no security
consequence. Compare an agent that can flip `ALLOW_SIGNUP`.

## 6. Prerequisite

**impresspress#78 blocks all of this on Cloudflare.** Shared
`WAFER_RUN_SHARED__*` variables have no read path on Workers, and nothing sets
`variables.block`, which the lazy per-block loader filters on — so config edits
never reach the running Worker. Locally this works today; on Cloudflare
"change it and the site changes" does not. #78 is the first commit of this
effort, not the last.

> **Correction (2026-09-14).** "the lazy per-block loader filters on
> [`variables.block`]" no longer describes the code: `D1ConfigSource` reads
> the whole variables table in one unfiltered query and groups by `block` in
> memory, issuing no `WHERE block = ?`. The column still scopes a row to its
> block — it is what the grouping keys on — so a row without it still reaches
> no block. Only the query strategy changed; the note above is left as the
> record of what was observed on 2026-08-28.

Related: whatever caching is added must bump the KV `cfg:v1:config_version`
stamp on write. A raw D1 write does not — that is exactly what bit the demo
deploy, where the seeded bootstrap-admin row sat unread until the stamp moved.

## 7. Suggested first slice

Prove the loop end to end, once, narrowly:

1. #78 fixed (prerequisite).
2. `impresspress/pages` block with four or five section types and one theme.
3. Its own public routes, own pages only, shell untouched.
4. Admin editor for the section list.
5. One agent tool: `propose_page_layout`, draft-only, human publishes.

Vocabulary breadth and a second theme are easy afterwards and teach nothing new.
