# Admin redesign: shared style layer, navy chrome, and detachable assets

> **Superseded in part (2026-10-23).** The 2026-10 admin layout redesign reversed this spec's stat tiles (icon, uppercase label, sparkline) and replaced its chart cards. Each line it superseded carries a **[superseded 2026-10: …]** marker naming what replaced it; everything else stands.

**Date:** 2026-09-01
**Branch:** `feat/admin-redesign` (worktree `.claude/worktrees/admin-redesign`, based on `8b48bcd`)
**Repos touched:** `impresspress` (producer), `impresspress/site` (consumer — the CDN)

## Goal

Three things that are really one thing:

1. Redesign the admin login and dashboard to match the supplied mockups — navy
   chrome, red accent, a neutral type voice.
2. Consolidate the shared style/component layer so pages compose components
   instead of hand-rolling markup with inline styles.
3. Make every asset (CSS, JS, fonts, logos, favicon) *detachable*: compiled into
   the binary for a single-file native deploy, or fetched externally for a small
   Cloudflare Worker wasm.

They are one thing because the redesign is what forces the component layer to
exist, and the component layer is what makes the asset set a well-defined,
publishable unit.

## Current state

`crates/impresspress-core/src/ui/` (~4.9k lines) is already a shared UI module:
maud + htmx, server-side rendered.

| File | Lines | Role |
|---|---|---|
| `templates.rs` | 850 | 6 page templates (list, detail, form, dashboard, …) |
| `assets.rs` | 701 | `include_str!`/`include_bytes!` + content-hashed URLs |
| `settings_form.rs` | 706 | Settings renderer |
| `mod.rs` | 642 | `SiteConfig`, `UserInfo`, `Page::response()` |
| `components.rs` | 611 | `stat_card`, `data_table`, `badge`, `modal`, `button`, `avatar`, … |
| `shell.rs` | 384 | Sidebar + topbar + body chrome |
| `nav_groups.rs` | 351 | Nav structure |
| `sidebar.rs` | 291 | Sidebar rendering |
| `icons.rs` | 242 | **42 inline lucide SVG paths** |
| `palette.rs` | 82 | ⌘K command palette |
| `layout.rs` | 53 | Full HTML page wrapper |

Assets live in `ui/assets/`, ~370 KB total, all embedded:

| Asset | Size |
|---|---|
| fonts (Itim latin + latin-ext woff2) | 84.5 KB |
| `htmx.min.js` | 50.9 KB |
| `components.css` | 46.4 KB |
| `impresspress-logo.png` | 45.6 KB |
| `marked.min.js` (feature `block-llm`) | 36.5 KB |
| `layout.css` | 28.4 KB |
| `purify.min.js` (feature `block-llm`) | 22.2 KB |
| `llm-chat.js` (feature `block-llm`) | 19.5 KB |
| `files-browser.js` (feature `block-files`) | 14.6 KB |
| `impresspress-logo-long.png` | 9.6 KB |
| `favicon.ico` | 5.8 KB |
| `tokens.css`, `charts.css`, `base.css` | 6.6 KB |

Three assets already have runtime URL overrides
(`WAFER_RUN_SHARED__LOGO_URL`, `..._LOGO_ICON_URL`, `..._FAVICON_URL`) — a
partial, logo-only version of the flag this spec generalises.

### What the mockups change

| | Today | Target |
|---|---|---|
| Sidebar | White rounded card, inset with margin | Flush full-height navy, no radius |
| Login panel | Orange (`#f0480f`) | Navy `#02112a`, logo top-left, headline bottom-left |
| Login form | Bordered centred card | Card-less on `#fdfdfd`, full-width red CTA |
| Dashboard order | Charts → stats → tables | Stats → charts → tables |
| Stat tiles | Label + value + icon | Icon + uppercase label + value + **sparkline** **[superseded 2026-10: stats are `components::{Stat, stat_row}`, a number and a sentence-case label, with no icon, sparkline or uppercase]** |
| Chart types | Column bars only | Line + area with gridlines, y-ticks, endpoint dots |
| Type | Itim everywhere | System stack; Itim for the wordmark only |

### Constraint discovered during exploration

`cdn.impresspress.org` does not exist — no DNS record, and
`~/Programs/suppers-ai/impresspress/site` is a Vite/Preact marketing site with
no wrangler config and no static-asset deploy. It has to be built.

## Decisions

Resolved with the user during brainstorming:

| Decision | Choice | Rationale |
|---|---|---|
| Reskin scope | Whole admin chrome **plus** a design pass on the other page archetypes | The chrome is shared; two coexisting visual systems would be the code smell `CLAUDE.md`'s root-cause rule forbids |
| CDN | Build it in this work | Producer→consumer across repos, per workspace rules |
| Asset origin when not embedded | Deployment's **own R2** first; `cdn.impresspress.org` as the no-R2 fallback | No cross-org runtime dependency; forks stay self-contained; deploys stay atomic |
| Accent | `#fd3534` everywhere, retiring `#f0480f` | One accent; matches the crab logo and the login mockup |
| Type | System stack for UI, Itim for the wordmark | 0 bytes, no FOUT, keeps the brand voice, turns 85 KB of woff2 into a lazy asset |

Two decisions follow from the requirement rather than from preference:

- **The embed/external switch must be a cargo feature.** Only compiling out
  `include_bytes!` shrinks the wasm; a runtime env var cannot remove bytes from
  a binary.
- **The base URL stays runtime-overridable on top of that**, so a self-hoster
  can mirror the asset set without rebuilding.

## Design

### 1. Shared style layer

The folder largely exists. The actual problem is that **419 inline `style="…"`
attributes across `blocks/` bypass it**:

| File | Count |
|---|---|
| `blocks/products/pages.rs` | 133 |
| `blocks/llm/pages.rs` | 44 |
| `blocks/messages/pages.rs` | 30 |
| `blocks/legalpages/pages.rs` | 23 |
| `blocks/llm/ui.rs` | 21 |
| `blocks/admin/pages/permissions.rs` | 21 |
| `blocks/userportal/pages/admin_buttons.rs` | 19 |
| `blocks/admin/pages/blocks.rs` | 19 |
| `blocks/admin/pages/variables.rs` | 18 |
| others | ~91 |
| `ui/settings_form.rs` | 28 |

So this work is as much enforcement as creation.

```
ui/
  styles/
    tokens.css
    base.css
    components/   button card table form badge modal nav
                  toast palette stat chart auth   (.css each)
    layouts/      shell.css  page.css  auth-split.css
  components/
    mod.rs button.rs card.rs table.rs form.rs badge.rs modal.rs
    stat.rs chart.rs empty.rs pagination.rs avatar.rs auth.rs
  templates.rs shell.rs sidebar.rs layout.rs icons.rs
  nav_groups.rs palette.rs settings_form.rs assets.rs
```

`css_bundle()` keeps an **explicit ordered list** of files — never a glob.
`CLAUDE.md`: *"No magic code or implicit mapping layers."* Order stays
tokens → base → components (alphabetical) → layouts, and the existing
`css_bundle_includes_all_layers` test is extended to pin it.

Today's 46 KB `components.css` splits along the same seams as the Rust
components, so a component's markup and its styles are found in two files with
the same name.

#### New and moved components

| Component | Status | Why |
|---|---|---|
| `sparkline(series, color)` | New | Stat tiles in the mockup carry one; inline SVG polyline, no library **[superseded 2026-10: stats are `components::{Stat, stat_row}`, a number and a sentence-case label, with no icon, sparkline or uppercase]** |
| `line_chart_card(...)` | New | The New-users/Errors cards are line+area with gridlines, y-ticks and endpoint dots. `bar_chart_card` (a private fn in `dashboard.rs`) only draws bars **[superseded 2026-10: the dashboard's charts are `components::ChartCard`; `line_chart_card`/`bar_chart_card` are gone]** |
| `bar_chart_card(...)` | Moved | From `blocks/admin/pages/dashboard.rs` into `ui/components/chart.rs` — it is shared UI, not page code **[superseded 2026-10: the dashboard's charts are `components::ChartCard`; `line_chart_card`/`bar_chart_card` are gone]** |
| `stat_card(...)` | Extended | Gains icon, optional sparkline, accent **[superseded 2026-10: stats are `components::{Stat, stat_row}`, a number and a sentence-case label, with no icon, sparkline or uppercase]** |
| `alert(variant, msg)` | New | Replaces the hand-inlined `#error` / `#info` divs in the auth pages |
| `oauth_button(provider)` | New | `login.rs` currently inlines ~10 CSS declarations per button |
| `auth_panel(config, tagline)` | Moved | From `blocks::auth::brand_panel` into `ui/components/auth.rs`, where shared chrome belongs |
| `form_field(...)` | New | Absorbs the repeated label+input+error triples |

#### Which templates get the design pass

"Whole admin plus a design pass on the rest" resolves to these, all in
`templates.rs`. Naming them removes the ambiguity about what "the rest" covers:

| Template | Treatment |
|---|---|
| `auth_split` + `BrandPanel` | Redesigned — navy panel, card-less form |
| `dashboard_page` + `StatTile` | Redesigned — reorder, icon + sparkline tiles **[superseded 2026-10: stats are `components::{Stat, stat_row}`, a number and a sentence-case label, with no icon, sparkline or uppercase]** |
| `list_page` + `PageHeader` | Design pass — table, filter row, pagination |
| `detail_page` + `DetailHero`/`DetailMeta` | Design pass — hero, meta grid |
| `form_page` + `FormSection` | Design pass — fields, sections, actions |
| `tabbed_page` | Design pass — tab bar |
| `account_card_page` + `AccountCard` | Design pass |
| `status_page` | Design pass — error/404 states |
| `chat_page` | Restyle only; LLM chat internals unchanged |
| `public_page` + `PublicPage` | Restyle only; it is public-facing, not admin chrome |

#### Inline-style cleanup rule

Every `style="…"` in `blocks/*/pages*.rs` and `ui/settings_form.rs` is replaced
by a component or a class. **One exception, which is not a smell:** a *dynamic
value* passed as a CSS custom property (`--size`, `--chart-color`, progress
widths). That is the correct mechanism for handing a runtime number to a
stylesheet, and it stays.

### 2. Visual redesign

Colours sampled from the mockups directly (`PIL`, dominant-colour per region),
not eyeballed:

```css
--primary-color:  #fd3534;   /* accent, large text, borders, active rail   */
--primary-button: #d92320;   /* white-on-red surfaces — see WCAG note      */
--primary-hover:  #e02523;

--navy-900: #02112a;         /* login brand panel                          */
--navy-800: #0a1122;         /* sidebar                                    */
--navy-700: #172136;         /* sidebar active / hover row                 */

--sidebar-text-muted: #94a3b8;
--surface-1: #ffffff;
--surface-2: #f8f8f9;
```

`--primary-color` **keeps its name**. `WAFER_RUN_SHARED__PRIMARY_COLOR` is a
public config var that themes it; renaming the property would be a breaking
config change for no benefit. Only the value moves.

`BRAND_ACCENT_HEX` in `assets.rs` (used for inline styles in emails, where CSS
variables do not work) moves to `#fd3534` with it. The existing
`brand_accent_matches_tokens_css` test already pins the two together.

- **Sidebar** — flush, full-height, `--navy-800`, no radius or margin. Section
  labels in `--sidebar-text-muted`. Active row: `--navy-700` background with a
  3px `--primary-color` left border. Footer: avatar + email + role above a
  hairline divider.
- **Topbar** — structurally unchanged; the mockup matches the existing
  crumbs / `|` / subtitle / *Quick jump* layout. Restyle only.
- **Dashboard** — reorder to stats → charts → tables. `dashboard_page`'s
  argument order changes accordingly.
- **Login** — `auth_split` loses the card. Because signup, reset-password and
  verify all share `auth_split`, they inherit the new treatment for free.
- **Type** — system stack (`system-ui, -apple-system, "Segoe UI", Roboto,
  "Helvetica Neue", Arial, sans-serif`) for UI text. Itim is scoped to
  `.brand__wordmark`, which also demotes 85 KB of woff2 from render-blocking to
  lazy.

### 3. Embed vs external assets

#### Build-time manifest

A `build.rs` in `impresspress-core` hashes every file in `ui/assets/` and
`ui/styles/` and generates an asset manifest — `(logical_name, hash, size,
content_type)`.

This is load-bearing, not incidental: with the bytes compiled out there is
nothing left to hash at runtime, so the content-hashed URLs the cache-busting
scheme depends on could not otherwise be produced. One manifest serves both
modes and is also the thing the CLI publishes.

#### Feature

New cargo feature `embed-assets` on `impresspress-core`, in `default`.

- **On:** `include_str!`/`include_bytes!` as today; `/b/static/…` served from
  memory by the `impresspress/system` block. Unchanged behaviour.
- **Off:** no `include_*`; URLs resolve externally.

The Cloudflare build already runs
`--no-default-features --features target-cloudflare`
(`cli/helpers/cloudflare/build.rs:97`), so **it gets the lean path
automatically**. This is the same fail-safe-lean feature pattern the crate
already documents for blocks: *"'forgot to opt out' fails safe (lean) instead
of fails bloated."*

#### URL resolution

Resolved once at startup, in strict order:

1. `IMPRESSPRESS_ASSET_BASE_URL` is set → use verbatim.
   (`IMPRESSPRESS_*`, no `__` — infrastructure, never in the DB, per `CLAUDE.md`.)
2. `embed-assets` on → `/b/static/`, served from memory.
3. An R2/storage backend is configured → `/b/static/`, streamed from R2 by the
   worker.
4. Otherwise → `https://cdn.impresspress.org/ui/v{CARGO_PKG_VERSION}/`.

Branch 3 is the point of choosing "own R2": the `/b/static/` **URL contract does
not change**, so the existing route, the existing `.impresspress/releases/v1/`
immutable prefix and the existing `ReleaseManifest` machinery all keep working.
Only the source of the bytes moves. Same-origin means no cross-origin font CORS
and no third-party availability dependency.

`deploy cloudflare` uploads the manifest's files to R2 under the immutable
prefix when `embed-assets` is off.

Expected saving: ~370 KB of embedded assets out of the wasm.

### 4. CDN (site repo)

In `~/Programs/suppers-ai/impresspress/site`, a `cdn/` Cloudflare Worker serving
`/ui/v{version}/*`:

- `Cache-Control: public, max-age=31536000, immutable`
- `Access-Control-Allow-Origin: *` — required, fonts are cross-origin here
- Version-pinned by crate version, so publishing a new version never breaks a
  deployment pinned to an old one.

Publishing copies the manifest's asset set into the site repo under
`/ui/v{version}/`.

**Manual step the user must do:** add the `cdn.impresspress.org` DNS record and
create the Cloudflare project. Everything else is automatable.

## Testing

- **Manifest** — generation is deterministic; hashes match file contents.
- **URL resolution** — one test per branch (1–4), including precedence when
  several conditions hold at once.
- **No-embed build** — asserts the asset bytes are genuinely absent, so the
  feature cannot silently stop saving space.
- **CSS bundle** — extend `css_bundle_includes_all_layers` to pin the explicit
  file order.
- **Contrast** — assert the new foreground/background pairs against WCAG AA, so
  the bug in §Risks cannot recur.
- **Visual** — all ~40 Playwright baselines regenerate. **CI's
  `regen-visual-baselines.yml` is canonical**; a local run is not, because the
  local wafer-run patch skews results (see below).

### Build environment caveat

The parent checkout has a gitignored `.cargo/config.toml` that `[patch]`es every
`wafer-*` crate to the sibling `../wafer-run` working copy. Cargo's config
discovery walks ancestor directories, and this worktree lives at
`.claude/worktrees/admin-redesign` *inside* that checkout — so **it inherits the
patch**, with `../wafer-run` resolving relative to the config file's directory.

That sibling is currently on branch `feat/config-var-select-number` (`9aa570d`),
two commits behind `origin/main`, and predates `BlockEndpoint::agent_tool`.
`Cargo.toml` pins `61e68a0`, which has it. The result is 8 compile errors in
`impresspress-core` that have nothing to do with this work — and the parent
checkout is in the same state right now.

Worked around with a worktree-local, untracked `.cargo/config.toml` that
re-points the same patch keys at the pinned rev's source, so this worktree
builds what CI builds. Nothing in the parent checkout or the sibling repo is
modified. Delete the file to revert.

## Risks

### The mockup's red fails WCAG AA

Measured: `#fd3534` on white is **3.66:1**. AA requires 4.5:1 for normal text.

Resolution: `--primary-button: #d92320` (**4.99:1**) for white-on-red surfaces;
`#fd3534` stays for large text, borders and accents. Visually near-identical,
actually compliant.

### The same bug is already in `main`

`--primary-button: #e5571f` carries the comment *"bumped for WCAG AA at small
sizes."* It measures **3.67:1** and does not pass. The comment asserts a
property the value does not have. Fixed here rather than ported forward.

### Navy sidebar contrast

Verified while choosing tokens: white on `--navy-800` is 18.81:1 ✓, and
`--sidebar-text-muted` `#94a3b8` is 7.34:1 ✓. The existing `--text-muted`
(`#64748b`) is only **3.95:1** on navy and must **not** be reused there — hence
the separate token.

### Baseline churn

~40 visual baselines change at once, which makes the diff hard to review for
regressions. Mitigation: land the token/chrome change and the page redesigns as
separate reviewable commits, so each baseline diff has one cause.

### Scope

This is a large change touching ~40 files across two repos. The workspace rule
is explicit that this is correct — *"if the right fix touches several repos,
touch all of them, in producer→consumer order"* — but it means the
implementation plan must sequence carefully rather than land as one commit.

## Out of scope

- Redesigning the public marketing site.
- Changing the nav structure or information architecture (`nav_groups.rs`) —
  this is a visual and structural refactor, not a UX re-think.
- Dark mode. The mockups define one theme.
- Replacing htmx or the SSR approach.
