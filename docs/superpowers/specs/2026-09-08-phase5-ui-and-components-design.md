# Phase 5 — UI, components and asset ownership

**Date:** 2026-09-08
**Status:** Decided 2026-09-08. The six rulings below are taken; they override the inventory that
follows wherever they differ, and they override `docs/CODE_REVIEW_2026-09-05.md` wherever they
contradict it — the review is three days and four phases stale, and the reconnaissance re-derived
every count in this document from the tree. Pull requests execute in the order of ruling 5.6.
Sections 0–8 below are the reconnaissance and stay as the record; they are not re-litigated. Note
that the rulings are numbered 5.1–5.6 after the phase and the inventory's own §5 has subsections of
the same numbers, so ruling headings carry the word "Ruling" and a bare `§5.n` always means the
inventory.
**Repos:** `impresspress` only, pull requests on the `Jsuppers` fork. No producer change — the
`wafer-run` pin does not move in this phase.
**Origin:** Phase 5 of `docs/CODE_REVIEW_2026-09-05.md` (§7, the phase-5 line at `:317`); theme T8;
§5.5; bugs B16 and B24; and the two phase-4 carry-forward items naming the language-model
administration page and the administration api-key revoke path.
**Base verified against:** `43870fff` ("Merge pull request #45 from Jsuppers/phase4/scheduled-and-contract"),
which contains phases 0–4. **Every line number in the review is stale**; each number below was
re-read from that tree.

**Corrections since the reconnaissance.** PR #46 (ruling 5.6's first pull request) merged on
2026-09-08, after this inventory was written, and closed two of its rows. Each claim it invalidated
carries an inline blockquote reading **Correction (PR #46, merged 2026-09-08)** immediately below
it, rather than being silently patched, so a reader can see which parts of the document were
checked against the tree after the fact and which were not. Everything else stands as written.

## Decisions taken

These are the rulings of 2026-09-08. They bind every phase 5 pull request and are not to be
re-litigated.

### Ruling 5.1 — Three review items are CLOSED as already fixed, not carried

The reconnaissance verified each against the tree with a named test. They are recorded here as
closed **with the evidence that closed them**, rather than deleted: a finding that was fixed by
other work is a different thing from a finding that was wrong, and only the row tells you which.

- **B24 (the wrapper divergence).** `blocks/admin/pages/mod.rs:45-64` is now pure delegation to
  `ui::shell_page`, which filters navigation at `ui/mod.rs:333`, and a regression test at `:71-98`
  asserts an unregistered block's entry is absent. No behavioural difference remains.
- **B16 (the dead delete button).** The route is declared at `blocks/llm/mod.rs:170-174` and tested
  at `:837`.
- **The language-model administration page** (carried in the phase-4 carry-forward from PR #44's
  review). `blocks/llm/ui.rs` now gates its write controls on the capability predicate at four
  sites, and the page that has no write control needs no gate.

**Dead controls generally are closed too.** The reconnaissance extracted all 351 route declarations
and matched every rendered URL against them. Zero misses. The one apparent mismatch is documented
at `blocks/admin/mod.rs:422-423`, where both verbs map to the same handler.

### Ruling 5.2 — "Administration is fully first-generation" is REFUTED and the phase is re-aimed

The review's claim does not hold. Administration is mixed: **34 second-generation component calls
already**. What is uniformly first-generation is narrower and more specific:

- **19 of 19 raw tables**, with zero uses of the shared table component.
- **35 of 35 hand-written buttons** — but these already carry the newer class names, so that split
  is markup-level, not stylesheet-level, and the button stylesheet contains no old class at all.

**The real two-generation split is the page header**, which the review did not name: an older
header component at **15 sites** rendering one element and class set, against a newer one at **30
sites** rendering a different element and class set. All **seven** administration sites pass an
empty title and therefore render nothing, so administration can cut over at no visual cost.

> **Correction (PR 6 of ruling 5.6, 2026-09-09).** The count of empty-titled sites is not seven of
> fifteen, it is **fifteen of fifteen** — files' seven (`pages_admin.rs` ×4, `pages_user/`
> {`cloudstorage`,`buckets`,`objects`}) and vector's one pass exactly the same
> `PageHeader { title: "", subtitle: None, primary_action: None }` as administration's seven. So
> the older renderer had no rendered output *anywhere*, in any deployment, and its `.page-header*`
> stylesheet rules styled nothing.
>
> That inverts the cheap path. A cutover to `components::page_header` would not have been free
> even for administration: that component renders its wrapper and an empty `h2.page-title`
> unconditionally, so an empty title through it emits
> `div.flex.items-center.justify-between.mb-4 > div > h2.page-title`. An empty heading generates
> no line box, so the cost is the `mb-4` wrapper's `1rem` bottom margin — not a heading box — on
> all fifteen pages, where they currently draw nothing at all. The free move was **deletion**, and
> that is what PR 6 did: `PageHeader`, `render_header`, the header parameter on `list_page` /
> `tabbed_page` / `dashboard_page`, and **nine** `.page-header*` rules — one in
> `components/card.css` and eight in `layouts/page.css` (the block itself, its `__text`,
> `__title`, `__subtitle` and `__action` elements, the `body:has(.products-tabs)` override, and
> one rule in each of two `@media` blocks) — plus the `.page-header` alternative of
> `components/nav.css`'s `.products-tabs +` rule, which is a shortened selector list rather than a
> tenth deleted rule. All 20 administration renders are byte-identical across the change.

Aim the phase at the table markup and the page header. Do not spend a pull request re-classing
buttons that already carry the right classes.

**The stat tile is refuted as a second renderer** — `ui/templates.rs` already delegates to
`components::stat_card`. Leave it alone.

### Ruling 5.3 — No live cross-site-scripting sink. Do the hygiene anyway, and say why

The reconnaissance traced all six interpolating attribute handlers to closed sets. This phase fixes
duplication and a latent hazard, not a live vulnerability. **Do not describe it as a security fix in
any pull request body.**

Two things are still worth doing on their merits: 114 attribute handlers across 111 lines in 23
files is the same logic written many times, and the configuration injections have no closing-tag
escape, which is a real hazard the day any of them carries operator-supplied text. The rule already
exists and is documented at `blocks/admin/pages/network.rs:62-65`; this phase applies it everywhere
else.

### Ruling 5.4 — The baseline-moving change ships alone

Exactly one pull request moves rendered output: migrating administration's tables to the shared
table component. It lands on its own branch, with no other change, because a baseline diff has to be
reviewable as a diff.

Two things about the baselines that the reconnaissance found and that bind:

- The suite's masking rule for owner and created columns only matches output from the shared table
  component. So migrating administration both moves those baselines and newly activates the mask.
  Expect both effects and say which lines are which.
- The visual job runs on pull requests only; the main-branch workflow has no such job. So a baseline
  that goes stale after a merge is invisible until the next pull request. That is how it went stale
  for two days earlier in this run.

For the pull requests expected not to move baselines, a non-empty regeneration is a **finding**, not
a chore. Report it before regenerating.

### Ruling 5.5 — Scope is the review's phase 5 line. Candidates are listed, not folded in

The reconnaissance surfaced tempting adjacent work: generalising the link gate to nine uncovered
blocks, rendering the legal-pages endpoint list from the route table (it currently names one verb
where the routes declare another and lists 8 of 17), reconciling the drifted duplicate card
renderer, three separate modal mechanisms, administration's oversized functions, and the
embedded-scripts accessor.

All of it stays out; §8 records it with its evidence. The one exception is the duplicate card
renderer: the two copies **have already drifted**, one using semantic classes and the other inline
colours, under reciprocal comments asking them to stay in sync. That is recorded as a defect rather
than a candidate, because a "keep in sync" comment that has already failed is a finding.

### Ruling 5.6 — Six pull requests, in this order

Consolidating the reconnaissance's first two, which are both asset plumbing with no rendered change:

1. **Assets get owners and hashes.** Block-owned static assets move out of the shared module,
   following the precedent already set by the development block, and the chrome JavaScript inlined
   into every page head becomes one hashed asset. No baseline move. **Shipped as PR #46, merged
   2026-09-08.**
2. **Event delegation everywhere.** The 111 handler lines, the three copied auto-show scripts, and
   the 430-character inline handler. No baseline move.
3. **Administration badges.** Needs the badge variants widened to match the stylesheet's six or
   seven. No baseline move expected.
4. **Administration tables onto the shared component.** The baseline mover. Lands alone.
5. **The product wizard's JavaScript to a file.** About 650 lines. The risk is the products
   lifecycle end-to-end test, which mocks its origins and will fail to find the new asset URLs
   unless the static path passes through. Check that first, not last.

   > **Correction (PR 5 of this ruling, 2026-09-09).** The named risk does not exist in the form
   > stated, and the risk that does exist is a different one. `products-lifecycle.spec.ts`
   > fabricates every page it tests in-process and serves it from `page.route()`; it never
   > reaches a server, so there is no request for a `/b/static/` URL to 404 and nothing needed
   > to be passed through. What it did do was read the Rust source of `pages.rs` and dig the
   > script constants out with a regular expression — and that regular expression is what broke.
   > Six specs did it (`products-lifecycle`, `-wizard`, `-manager`, `-seller-governance`,
   > `-catalog-admin`, `-webhook-admin`); all six now read the `.js` file, as
   > `products-storefront.spec.ts` always has. The eight lifecycle baselines are drawn from
   > fabricated markup that this pull request does not touch, so they cannot move for this
   > reason either.
6. **Delete what is now unused.** Stylesheet deletion is blocked at `ui/mod.rs:1725` until ten
   non-administration raw tables migrate, which is out of scope, so this ships as "delete what is
   unused" and records the rest.

   > **Shipped 2026-09-09.** Deleted: the first-generation page header (`PageHeader`,
   > `render_header`, its parameter on three templates, 15 empty call sites, nine CSS rules and one
   > selector alternative — see the correction under ruling 5.2), `templates::form_page` and its
   > `.form-bar` rule (its last caller went on 2026-07-11 in `b56c84f3`, which stopped
   > administration's Settings tabs nesting a per-tab `<form>` inside the template's outer one and
   > moved them to the form-less `tabbed_page`; nothing has called it in the two months since),
   > `templates::StatTile` (`dashboard_page` now takes rendered `stat_card`s), and four dead
   > families in `table.css`: `.table th.sortable`, the single-dash
   > `.pagination-info`/`-controls`/`-btn` family superseded by `components::pagination`'s
   > `.pagination__*`, the flat `.db-table-list` the grouped redesign replaced, and the
   > `.users-table`/`.col-created` pair that no code has ever rendered. `components/nav.css`'s
   > `.products-tabs + header` rule went with them: `.page-header` was always a `<header>`
   > element, and it was the only one that could sit directly after the tab strip, so the rule
   > matched nothing once the header was gone.
   >
   > **One intent is discarded with the `.users-table` pair, not just dead code.** Those two rules
   > were added deliberately, to stop the users table's Created date wrapping
   > (`white-space: nowrap` on `td[data-label="Created"]` and `.col-created`). Nothing ever
   > rendered either class, so the intent was never realised and deleting them changes no pixel —
   > but if that column wraps today, the fix is to apply the intent to the class the migrated table
   > actually carries (`.data-table`), not to resurrect these. Recorded here because the deletion
   > otherwise leaves no trace of it.
   >
   > Still blocked, as predicted: the `.table` / `.table-container` family. The ten raw tables are
   > legalpages ×3, tickets ×3, llm ×3, userportal ×1, and they are now pinned by
   > `ui/components/table.rs::only_the_declared_files_still_hand_write_a_first_generation_table`,
   > a ratchet on the badge test's model whose doc names the exact rules to delete when the list
   > empties. Files and vector, which §7's row 7 also named, have no raw tables.
   >
   > **Deliberately not folded in:** roughly 25 further dead rules across `form.css`, `card.css`,
   > `button.css`, `badge.css`, `stat.css`, `shell.css`, `auth-split.css` and `toast.css`, and five
   > never-constructed enum variants (`BtnVariant::{Secondary,Ghost,Danger}`, `CtrlSize::Lg`,
   > `AlertVariant::Info`) with the CSS they gate. None of it is two-generation drift; it
   > accumulated because nothing guards the *defined → rendered* direction, which is a finding of
   > its own. The button variants in particular must not be read as dead code: 87, 29 and 9 sites
   > write `.btn--secondary` / `.btn--ghost` / `.btn--danger` by hand across the tree, which is the
   > un-migrated button backlog ruling 5.2 left alone, not an unused API.

## Coordination notes

Things one pull request in this phase does that the reviewer of a later one has to expect.

- **PR #46's globals test is expected to be rewritten by pull request 2, and its removal is not a
  lost regression guard.** #46 added `chrome_js_keeps_the_modal_helpers_global`
  (`crates/impresspress-core/src/ui/assets.rs:1244`), which pins `openModal` and `closeModal` as
  top-level declarations in `chrome.js`. It exists only because attribute handlers call those two
  names from `onclick` strings, and it was worth adding because moving the modal section into
  `chrome.js` under an IIFE would have made them non-global and silently dead. Pull request 2
  replaces those handlers with event delegation, which is exactly what makes the pin obsolete. Its
  reviewer must expect the test to go or to change shape, and must not read that as coverage lost.
- **The volatile-value masking landed ahead of pull request 4, in its own change, and moved two
  baseline images without changing any rendered output.** Painting a mask over a region changes the
  screenshot; adding the attribute the mask keys on does not change what a browser draws. That is a
  different category from ruling 5.4's one baseline-moving pull request, which changes markup and
  therefore pixels. The two administration baselines it repainted —
  `admin-admin-dashboard-desktop-chrome-linux.png` and `admin-admin-network-desktop-chrome-linux.png`
  — were regenerated there, so pull request 4's diff is markup only.
- **The mask hooks are inside the table cells, not on the `<td>`.** `components::data_table`
  (`ui/components/table.rs:53`) emits each `<td>` itself and accepts only the cell's inner markup,
  which it carries through verbatim. There is no affordance for a caller-supplied cell attribute
  and pull request 4 is not being asked to add one; anything the migration must preserve therefore
  has to live in the cell's content.

---

## 0. Verdict summary — what reproduces, what is already closed

| Review claim | Status | Where it stands now |
|---|---|---|
| B24 — `admin_page` skips `retain_registered`, admin nav shows 404 blocks on CF/browser | **CLOSED (fixed)** | `blocks/admin/pages/mod.rs:45-64` is a pure delegation to `ui::shell_page`; regression test at `:71-99` |
| B16 — `hx-delete /b/llm/api/config/{id}` has no route | **CLOSED (fixed)** | route declared `blocks/llm/mod.rs:170-174`; test `blocks/llm/mod.rs:837-865` |
| Admin api-key revoke posts to a nonexistent `/revoke` path (carry-forward) | **CLOSED (fixed)** | now `PATCH`; guarded by `blocks/admin/mod.rs:1809` link test |
| LLM providers page offers controls a runtime cannot service (carry-forward) | **CLOSED (fixed)** | `blocks/llm/ui.rs:68,115,253,293` gate on `ProviderAdmin::manages_providers()` |
| `components.rs:7-13, 121-127, 203-209` orphaned section headers | **REFUTED / does not exist** | there is no `ui/components.rs`; it is a directory of 14 files (PR #85 redesign) |
| Admin is "fully gen-1", 35 legacy buttons | **PARTLY REFUTED** | 35 hand-written button *lines* is exact, but they already carry gen-2 BEM classes. The split is markup-level (`button .btn .btn--primary` vs `components::button()`), not class-level. Admin also makes 34 `components::` calls |
| Admin 19 raw tables | **REPRODUCES exactly** | 19 `table .table` lines across 9 admin page files |
| Admin 39 raw badges | **REPRODUCES exactly** | 39 raw `.badge` spans across 9 admin page files |
| Admin 7 `PageHeader`s all with `title: ""` | **REPRODUCES exactly** | 7 non-test sites, all empty-titled |
| `BadgeVariant` 4 variants vs 6 CSS classes | **REPRODUCES, and worse** | 4 variants, 6 `.badge-*` classes, **plus** 5 `.badge--tone-*` BEM classes = three generations |
| Products fully gen-2, 114 component calls | **REPRODUCES, grown** | `blocks/products/pages.rs` makes **198** `components::` references; 0 `templates::` uses |
| `templates::StatTile` is a gen-1 renderer | **REFUTED** | `ui/templates.rs:240` already delegates to `components::stat_card`. It is one redundant struct layer, not a divergent renderer |
| Three hand-rolled modals | **REPRODUCES in count, different shape** | two raw `div.modal-overlay` copies in admin + one native `<dialog>` in files = three mechanisms |
| JS embedded five ways | **REPRODUCES**, all five present | see §3 |
| `palette_js` 141 lines at `ui/assets.rs:295-433` | **REPRODUCES, moved** | 139 lines at `ui/assets.rs:426-564` |
| `PRODUCT_WIZARD_JS` ~480 lines at `pages.rs:1118-1520` | **REPRODUCES, moved** | 403 lines at `blocks/products/pages.rs:1276-1678` |
| `database.rs:90` ~430-char minified `oninput` | **REPRODUCES, same line** | `blocks/admin/pages/database.rs:90` |
| "the eye-toggle copied 3×" | **REFUTED (consolidated)** | one `pw_toggle_js()` at `blocks/auth_ui/pages/mod.rs:176`; a second, different eye-toggle survives inline at `ui/settings_form.rs:134` |
| `network.rs:101-104` documents the attribute-JS XSS sink and uses delegation | **REPRODUCES, moved** | `blocks/admin/pages/network.rs:62-65` and `:111-118` |
| Messages entry card rendered in Rust and JS with a keep-in-sync comment | **REPRODUCES, and the two have drifted** | see §4 |
| `ui/assets.rs` hosts block assets behind block cfgs (T2) | **REPRODUCES** | `ui/assets.rs:152,162,172,180` |

**Two of the review's four named phase-5 defects (B16, B24) are already fixed and must be closed,
not carried.** What remains is real but is a *migration* backlog, not a bug backlog — with one
exception (§5.4, the un-generalized link gate).

> **Correction (PR #46, merged 2026-09-08).** The last two rows of this table are now closed. The
> four block-owned assets have moved out of `ui/assets.rs` to `blocks/llm/assets/` and
> `blocks/files/assets/`, and `palette_js`/`drawer_js`/`toast_js`/`modal_js` no longer exist as
> inline raw strings — they are one hashed `ui/assets/chrome.js`. Every `ui/assets.rs` line number
> in this inventory predates that move and is stale; the surrounding claims are not.

---

## 1. The two component generations

### 1.1 What each generation is

**Gen-2 widget layer** — `crates/impresspress-core/src/ui/components/` (13 files, 1,262 lines),
exports at `ui/components/mod.rs:15-27`:

| Export | File:line | CSS |
|---|---|---|
| `button`, `tab_navigation`, `BtnVariant`, `Tab` | `ui/components/button.rs:93, 32, 60, 17` | `ui/styles/components/button.css` (BEM only) |
| `data_table`, `TableCol` | `ui/components/table.rs:20, 4` | `table.css:73-95` (`.data-table*`) |
| `badge`, `status_badge`, `BadgeVariant` | `ui/components/badge.rs:41, 48, 9` | `badge.css` |
| `page_header` | `ui/components/card.rs:9` | `.page-title` / `.page-subtitle` |
| `stat_card` | `ui/components/stat.rs:6` | `stat.css` |
| `modal` | `ui/components/modal.rs:8` | `modal.css` |
| `empty_state`, `pagination`, `avatar`, `search_input*`, `alert`, `auth_panel`, `oauth_button`, charts | `empty.rs:3`, `pagination.rs:3`, `avatar.rs:17`, `form.rs:9,14`, `auth.rs:81,38,89`, `chart.rs:7,57,109` | per-file CSS |

**Gen-1 page-template layer** — `ui/templates.rs` (1,000 lines). *Not uniformly legacy:* it uses
BEM (`.page-header__title`) and `dashboard_page` already delegates to `components::stat_card`
(`ui/templates.rs:240`). Its genuinely gen-1 pieces are:

- `PageHeader` (`ui/templates.rs:12-16`) + `render_header` (`:18-33`) — a **second, incompatible
  page-header renderer** to `components::page_header`. See §1.3, the single largest divergence.
- `StatTile` (`ui/templates.rs:214-219`) — a struct-shaped indirection over `stat_card`; five
  fields carried only so `dashboard_page` can loop. Redundant, not divergent.
- The `.table-container > table.table` idiom the templates' callers hand-write (the templates
  themselves take pre-rendered table markup).

**Gen-1 CSS still alive:** `ui/styles/components/table.css:2-23` (`.table-container`, `.table`,
`.table th/td`, `.table th.sortable`) and `badge.css` lines 8-26 (`.badge-success/-danger/-warning/
-info/-primary/-secondary`). `button.css` carries **no** gen-1 `.btn-primary` class — buttons are
fully BEM at the CSS layer. That half of the review's claim does not reproduce.

### 1.2 Current counts, by area

| Area | `components::` refs | raw `table .table` | raw `.badge` spans | hand-written `.btn` lines | `templates::` uses |
|---|---|---|---|---|---|
| **products** (`pages.rs`, 3,935 lines) | **198** | 1 | 6 | 172 BEM tokens (all via components or literal markup) | **0** |
| **admin** (12 page files, 4,632 lines) | **34** | **19** | **39** | **35** (28 `<button>`, 7 `<a class=btn>`) | 8 (7 `PageHeader{title:""}` + layout fns) |
| llm (`pages.rs`+`ui.rs`) | 3 | 3 | 10 | 18 | 5 |
| legalpages | 2 | 3 | 12 | 10 | 3 |
| tickets | 6 | 3 | 4 | 13 | 2 |
| files | 13 | 0 | 2 | 6 | 7 (`PageHeader` ×7) |
| userportal | 9 | 1 | 0 | 12 | 3 |
| vector | 4 | 0 | 4 | 2 | 1 (`PageHeader`) |
| messages | 0 | 0 | 6 | 5 | 1 |
| auth_ui | 7 | 0 | 0 | 5 | 8 |
| dev | 0 | 0 | 0 | 7 | 0 |
| **tree total** | — | **29** | **83** | — | — |

`data_table` call sites tree-wide: **17** (16 in `blocks/products/pages.rs`, 1 in
`blocks/files/storage/admin.rs`). **Admin has zero.**

**Verdict on the review's two headline claims.**
*Products is fully second-generation* — **confirmed and stronger than reported** (198 vs the
reported 114; zero `templates::` uses; the one `table .table` is in a non-page helper).
*Admin is fully first-generation* — **refuted as stated.** Admin is mixed: 34 `components::`
calls (`users.rs` 12, `logs.rs` 6, `dashboard.rs`/`permissions.rs`/`variables.rs` 4 each,
`network.rs` 2, `blocks.rs`/`storage.rs` 1, plus `tab_navigation`/`empty_state`/`Tab` imported
unqualified in `blocks.rs:9`, `database.rs:183`, `logs.rs:11`, `users.rs:18`). What is *uniformly*
gen-1 in admin is the **table** (19/19 raw, 0 `data_table`) and the **button** (35/35 hand-written,
0 `components::button`). Badges are 39 raw against 6 `components::badge` calls.

### 1.3 The header divergence (the real "two generations")

Two renderers for the same element, with different HTML *and* different class sets:

```
ui/templates.rs:18-33     header.page-header
                            > div.page-header__text > h2.page-header__title, p.page-header__subtitle
                            > div.page-header__action

ui/components/card.rs:9   div.flex.items-center.justify-between.mb-4
                            > div > h2.page-title, p.page-subtitle
                            > div
```

Callers split cleanly along the same line as everything else:

- `templates::PageHeader` — **15 sites**: admin ×7 (`database.rs:373`, `logs.rs:59`,
  `dashboard.rs:381`, `blocks.rs:215`, `settings.rs:81`, `storage.rs:48`, `users.rs:77` — **all
  seven with `title: ""`**, so `render_header` short-circuits to empty markup at
  `ui/templates.rs:19-21`), files ×7 (`pages_admin.rs:153,296,422,515`,
  `pages_user/{cloudstorage.rs:162, buckets.rs:193, objects.rs:269}`), vector ×1
  (`pages_ui.rs:178`).
- `components::page_header` — **30 sites**: products 17, tickets 5, llm 3, legalpages 2,
  userportal 2, auth_ui 1.

**This is the item that moves every baseline** if unified. Because all seven admin `PageHeader`s
pass `title: ""`, admin renders *nothing* through it today — so admin can be cut over to
`components::page_header` with no pixel change, and the divergence survives only for files and
vector. That is the cheap path.

> **Correction (PR 6 of ruling 5.6, 2026-09-09).** Files' seven and vector's one pass `title: ""`
> too — see the correction under ruling 5.2. There was no divergence left to survive: the older
> renderer painted nothing on any page, so the whole of it was deleted rather than cut over, and
> `components::page_header` is now the only page-header renderer in the tree.
> `ui/mod.rs::the_first_generation_page_header_stays_deleted` pins that in both directions: no
> `.page-header*` class in maud markup under `src/blocks` or `src/ui`, and no `.page-header*` rule
> in any stylesheet — the shared `ui/styles/` bundle *and* the block-local sheets a block serves
> with its own pages, which are as live to those pages as the bundle is.

### 1.4 The modal divergence (three mechanisms)

| Mechanism | Sites | Open/close |
|---|---|---|
| `components::modal` → `div.modal-overlay[hidden]` (`ui/components/modal.rs:10`) | 6: `admin/pages/users.rs:400,496`, `admin/pages/permissions.rs:298`, `admin/pages/variables.rs:52`, `vector/pages_ui.rs:33`, `userportal/pages/admin_buttons.rs:266` | `openModal()`/`closeModal()` from `ui/assets.rs:567-578` |
| hand-rolled `div.modal-overlay` copy | 2: `admin/pages/blocks.rs:205`, `admin/pages/variables.rs:86` | per-page inline `<script>` calling `removeAttribute('hidden')` (`blocks.rs:331,463`, `variables.rs:579`) |
| native `<dialog class="modal modal--bucket-create">` | 1: `files/pages_user/buckets.rs:86` | `showModal()` from `ui/assets/files-browser.js` |

`components::modal` itself carries an attribute handler (`ui/components/modal.rs:16`,
`onclick={"closeModal('" (id) "')"}`) — interpolated, though `id` is always a caller literal.

---

## 2. The wrapper divergence — **closed**

`blocks/admin/pages/mod.rs:45-64`:

```rust
pub(crate) async fn admin_page(ctx, msg, title, topbar: Topbar<'_>, content) -> OutputStream {
    ui::shell_page(ctx, msg, Shell { title, nav: NavKind::Admin, crumbs: topbar.crumbs,
                                     subtitle: topbar.subtitle,
                                     primary_action: topbar.primary_action }, content).await
}
```

`ui::shell_page` (`ui/mod.rs:299-305`) → `shell_document` (`:318-348`), which at `ui/mod.rs:327-333`
builds `registered: HashSet<&str>` from `ctx.registered_blocks()` and calls
`nav_groups::retain_registered(&mut groups, &registered)` (`ui/nav_groups.rs:33`).

`admin_page` therefore has **no** behavioural difference from `shell_page` — it is a two-argument
convenience (`NavKind::Admin` + `Topbar` destructuring). The doc comment at
`blocks/admin/pages/mod.rs:37-44` says so explicitly, and a regression test
`admin_shell_hides_nav_entries_for_unregistered_blocks` (`:71-98`) asserts an unregistered block's
`href="/b/llm/"` is absent while `href="/b/admin/logs"` survives.

`ui/mod.rs:291-294` still carries the "replaced the six per-block `*_page` wrappers" note; admin is
no longer the exception it names.

**Remaining per-block wrappers** (all thin, all delegate to `shell_page`): `tickets/pages.rs:554`
`shell()`, `userportal/pages/mod.rs:16` `account_page()`, `llm/ui.rs:49,370`. `shell_page` is
called directly from 18 sites in `blocks/products/pages.rs` and from every other block.

**Nothing to do here. Close B24 and the T8 wrapper bullet.**

---

## 3. The five ways JavaScript is embedded — current census

### Way 1 — Rust functions returning raw-string JS (shared chrome), `ui/assets.rs`

| Fn | Lines | Emitted from |
|---|---|---|
| `palette_js()` | `ui/assets.rs:426-564` (**139**) | `ui/mod.rs:213` inline `<script>` on every shelled page |
| `drawer_js()` | `ui/assets.rs:594-620` (27) | `ui/mod.rs:214` |
| `toast_js()` | `ui/assets.rs:398-421` (24) | `ui/layout.rs:45` |
| `modal_js()` | `ui/assets.rs:567-578` (12) | `ui/layout.rs:46` |
| `webmcp_js()` | `ui/assets.rs:291-293` | served as the hashed `/b/static/webmcp-<hash>.js`; body assembled by `build.rs` from `assets/webmcp-core.js` (110 lines) + `assets/webmcp.js` (138) |

**202 lines of JS inlined into the `<head>` of every page**, uncached, unhashed, re-sent on every
request. This is the "one hashed `admin.js`" target — though note it is not admin-specific; it is
chrome-wide.

> **Correction (PR #46, merged 2026-09-08).** Done. The four accessors are gone and the four inline
> `<script>` blocks are one `<script src="/b/static/chrome-<hash>.js">` served from
> `crates/impresspress-core/src/ui/assets/chrome.js`. The count of 202 in the heading counted the
> Rust wrappers as well as the JavaScript inside them; #46's own body reconciled the two to the
> smaller figure.

Block-local equivalents: `blocks/auth_ui/pages/mod.rs` `login_script()` (65 lines, `:191`),
`signup_script()` (65, `:277`), `oauth_button_script()` (15, `:141`), `pw_toggle_js()` (3, `:176`);
`blocks/files/pages_user/mod.rs:50` `render_bootstrap_script()` (16);
`blocks/products/pages.rs:2542` `stripe_setup_js()` (**159**), `:2935` `commerce_portal_js()` (29).

### Way 2 — `include_str!` files

| Asset | Lines | Owner | Served how |
|---|---|---|---|
| `ui/assets/htmx.min.js` | 1 (minified) | shared | `ui/assets.rs:107`, hashed `/b/static/` |
| `ui/assets/llm-chat.js` | **507** | *llm's*, in `ui/` | `ui/assets.rs:172`, `#[cfg(feature = "block-llm")]` |
| `ui/assets/marked.min.js` | 6 | *llm's*, in `ui/` | `ui/assets.rs:152`, `#[cfg(block-llm)]` |
| `ui/assets/purify.min.js` | 3 | *llm's*, in `ui/` | `ui/assets.rs:162`, `#[cfg(block-llm)]` |
| `ui/assets/files-browser.js` | **422** | *files'*, in `ui/` | `ui/assets.rs:180`, `#[cfg(block-files)]` |
| `blocks/products/assets/storefront.js` | 573 | products | `blocks/products/handlers/commerce.rs:27,73` — served unhashed, `Cache-Control: max-age=300` |
| `blocks/dev/assets/dev.js` | 1,657 | dev | `blocks/dev/assets.rs:19,59` — composed, served from `/b/dev/static/dev.js` at Admin tier |
| `blocks/dev/assets/compiler-adapter.js` | 905 | dev | `blocks/dev/assets.rs:49,92` |

**T2 reproduces:** four block-owned assets live in `ui/assets.rs` behind `#[cfg(feature =
"block-llm")]` / `#[cfg(feature = "block-files")]` — core reaching down into blocks. The
**precedent for the fix already exists and is documented**: `blocks/dev/assets.rs:1-9` states
exactly why dev's assets live with the block and are served from the block's own tier, and
products does the same. Caveat: `llm-chat.js` has a second consumer —
`blocks/messages/pages.rs` (context-detail conversation lens) — so moving it to `blocks/llm/`
creates a messages→llm asset dependency that must be resolved deliberately.

> **Correction (PR #46, merged 2026-09-08).** Done: the assets are at `blocks/llm/assets/` and
> `blocks/files/assets/`, the `#[cfg(feature = "block-llm")]` / `block-files` arms are out of
> `ui/assets.rs`, and `blocks/llm/assets.rs` owns the accessors. The caveat above was itself
> imprecise and no dependency had to be resolved: `blocks/messages/pages.rs` never loaded
> `llm-chat.js`. What it has is a *duplicate* of one of that file's functions, hand-written in Rust
> under a keep-in-sync comment — which is §4's drifted card renderer, not an asset consumer. The
> only site that serves the file is the llm block's own page, plus the system block's route test.

### Way 3 — inline `script { PreEscaped(…) }` blocks inside page functions

**35 sites**, of which the significant ones:

| Site | Content |
|---|---|
| `blocks/products/pages.rs:1276-1678` | `PRODUCT_WIZARD_JS`, **403 lines** |
| `blocks/legalpages/pages.rs:181-277` | `EDITOR_JS`, 97 lines |
| `blocks/products/pages.rs:2249` | `PRODUCT_MANAGER_JS`, 39 lines |
| `blocks/tickets/pages.rs:14` | `ADMIN_FORM_JS`, 28 lines, emitted **three times** (`:177`, `:375`, `:476`) |
| `blocks/admin/pages/network.rs:66-83` | 18-line delegated click handler, guarded by `window.__networkDetailBound` |
| `blocks/admin/pages/permissions.rs:223-286` | inline script (63 lines) |
| `ui/sidebar.rs:167-193` | inline script |
| `blocks/products/pages.rs:2289` | `PRODUCT_CATALOG_ADMIN_JS`, 12 lines |
| `blocks/llm/ui.rs:233` | `ADD_PROVIDER_JS`, 9 lines |
| `blocks/products/pages.rs:919` / `:3240` | `SELLER_ADMIN_JS` / `ORDER_DETAIL_JS` (minified, 1 long line each) |
| `blocks/admin/pages/blocks.rs:331,463`, `variables.rs:579` | one-line `removeAttribute('hidden')` modal auto-show, copied **3×** |

### Way 4 — attribute event handlers

**114 occurrences on 111 lines across 23 files.**

| File | count | | File | count |
|---|---|---|---|---|
| `blocks/products/pages.rs` | 59 | | `ui/settings_form.rs` | 4 |
| `blocks/llm/pages.rs` | 6 | | `blocks/admin/pages/users.rs` | 4 |
| `blocks/admin/pages/variables.rs` | 6 | | `ui/sidebar.rs` | 2 |
| `blocks/legalpages/pages.rs` | 5 | | `ui/components/modal.rs` | 2 |
| `blocks/admin/pages/permissions.rs` | 5 | | `blocks/vector/pages_ui.rs` | 2 |
| `blocks/admin/pages/blocks.rs` | 5 | | 12 more files | 1 each |

The single worst: `blocks/admin/pages/database.rs:90` — a **430-character minified `oninput`**
implementing the table-filter (hides `[data-db-table]` rows, collapses empty `[data-db-group]`s,
toggles `#db-filter-empty`). Same line number the review cited.

### Way 5 — `format!`-generated scripts

| Site | Interpolated value | Safe? |
|---|---|---|
| `ui/settings_form.rs:308` → `submit_js()` (`:214-237`) | `post_url`, `serde_json::to_string`-encoded at `:215` | **Yes**, pinned by `submit_js_interpolates_post_url_safely` (`:527-533`) |
| `blocks/userportal/pages/admin_buttons.rs:309` | record `id` into `openModal('edit-btn-{id}')` | **Yes**, guarded by `is_safe_dom_id` with the SEC-058 rationale at `:243-250` |
| `blocks/products/pages.rs:908` | `window.__sellerAdminConfig={config}` — `serde_json::json!` at `:840` of `action_url`+`action` | Currently safe (URLs/enum) |
| `blocks/products/pages.rs:2230` | `window.__productManagerConfig={page_config}` — `json!` at `:2089` | Currently safe |
| `blocks/products/pages.rs:3548` | `window.__orderDetailConfig={…}` — `json!` at `:3310` | Currently safe |
| `ui/components/button.rs:103-105` | `PreEscaped(format!(r#"<button class="{class}" {extra}>…"#))` — **`extra_attrs` is raw, unescaped attribute text** | Safe today: no caller interpolates data into it |
| `ui/icons.rs:9` | icon SVG | n/a |

### 3.1 Which of these are XSS sinks, and which are duplication

**The rule and where it is written.** `blocks/admin/pages/network.rs:62-65` and `:111-118`:

> Delegated click handler — the row carries `data-detail-*` attributes (maud-escaped) instead of
> an `onclick` JS-string literal, which maud does **NOT** escape and so let an attacker-controlled
> request path break out and run script in an admin's session.

That file is the only place the rule is stated, and one of very few that applies it. `data-*` +
one delegated `document.addEventListener` is used at `network.rs:66-83` and inside
`ui/assets/files-browser.js` / `llm-chat.js`; **nowhere else in Rust page code.**

**Live sinks: none.** All six interpolating attribute handlers were traced to code-controlled
values:

| Site | Interpolated | Origin |
|---|---|---|
| `blocks/admin/pages/blocks.rs:149` `onchange` | `active_tab` | one of four literals, `blocks.rs:31-36` matches the query param down to a closed set |
| `ui/settings_form.rs:134` `onclick` (eye-toggle) | `var.key` | `ConfigVar` declaration, code-defined |
| `ui/settings_form.rs:152` `onchange` | `var.key` | same |
| `ui/components/modal.rs:16` `onclick` | `id` | caller literal at all 6 sites |
| `ui/components/auth.rs:93` `onclick` | `provider` | fixed provider list |
| `blocks/auth_ui/pages/mod.rs:168` `onclick` | none (literal `togglePw(this)`) | — |

The other 108 attribute handlers are fully literal strings — **pure duplication**, not sinks. But
the *pattern* is the hazard: any future page that interpolates a name, path or title into one of
these attributes is a stored-XSS bug with no test to catch it, and the codebase currently offers no
door that prevents it. `products/pages.rs`'s 59 handlers are the largest such surface.

**Latent hazard, worth naming:** none of the five `PreEscaped(format!("window.__x={json};…"))`
sites escapes `</script>` in the serialized JSON. Every current payload is an id, URL or enum, so
none can carry it — but there is no helper and no test enforcing that, and `page_config` is the
obvious place a product name or seller display name gets added.

**Sixth embedding way not in the review's taxonomy:** `SiteConfig::embedded_scripts`
(`ui/mod.rs:33`, populated from a config CSV at `:69`, rendered at `ui/templates.rs:520`) injects
admin-configurable external `<script src>` tags into public pages. Deliberate feature; noted so the
census is complete.

---

## 4. Rendered twice, in Rust and in JavaScript

**One instance, and the two copies have already drifted.**

- Rust: `blocks/messages/pages.rs:29-72` `entry_card(record)`. Comment at `:38-42`:
  > "Keep in sync with `messageCardHtml` in ui/assets/llm-chat.js — same cards, JS-rendered."
- JS: `crates/impresspress-core/src/ui/assets/llm-chat.js:52-80` `messageCardHtml(role, content,
  date, opts)`. Comment at `:57-59`:
  > "Brand-consistent card accents — keep in sync with `entry_card` in blocks/messages/pages.rs
  > (the SSR renderer of the same cards)."

**They are not in sync.** Rust emits semantic classes:
`div.card.message-card--user | --neutral | --warning` with `span.badge`/`.badge-warning`, and
`p.message-card__content`. JS emits **inline hex styles**:
`background:#fff1e6;border-left:3px solid var(--primary-color)` (user),
`background:var(--surface-3);border-left:3px solid var(--border-color)` (assistant/other),
`background:#fefce8;border-left:3px solid #eab308` (system), plus
`<span class="badge" style="font-size:0.7rem">` for the model badge. The Rust side also renders an
`artifact` content-type chip and a date column the JS side does not model, and the JS side has a
markdown/`DOMPurify` path the Rust side does not. The pair cannot converge by class rename alone.

Note the JS side's inline styles would fail `ui/mod.rs:913` `pages_carry_no_static_inline_styles`
if the same markup were written in Rust — the guard only scans `.rs` files.

**Adjacent, same shape:** `blocks/dev/paths.rs:38` declares a constant "**Mirrored in
`crates/impresspress-browser/js/bridge.js` as `META_SUFFIX`**" — a second Rust↔JS hand-sync, not a
double render.

---

## 5. Dead controls — checked against the route tables, not by eye

Method: extracted all 351 `EndpointRoute::*(HttpMethod::X, "path", …)` declarations across
`blocks/`, normalized `{param}`/`:param` → `{p}`, and matched every `hx-get/post/patch/put/delete`
and `data-detail-url` attribute rendered by any block page (34 non-test control URLs), plus every
`/b/…` literal reachable from a `fetch(` / `htmx.ajax` call in Rust-embedded JS and in the `.js`
assets. Scripts kept at
`/tmp/…/scratchpad/p5/{sweep2.py,routes_of.py}`.

### 5.1 Result: **zero unmatched controls.**

The one apparent miss — `blocks/admin/pages/variables.rs:539`
`form hx-put={"/b/admin/variables/" (key)}` against `EndpointRoute::admin(HttpMethod::Patch,
"/b/admin/variables/{key}", …)` (`blocks/admin/mod.rs:424-429`) — is intentional and documented at
`blocks/admin/mod.rs:422-423`: "The edit form sends PUT; both PUT and PATCH map to the `update`
action the matcher compares."

The three `/b/static/…` and `/b/webmcp/manifest.json` misses are test fixtures and
pipeline-served paths outside `EndpointRoute` tables (`blocks/system.rs:328,340`,
`blocks/llm/pages.rs:882`, `ui/assets/webmcp.js:39`), and `/b/storage/direct` is declared at
`blocks/files/mod.rs:168`.

**B16 and the api-key-revoke dead control are both fixed. Close them.**

### 5.2 Runtime-cannot-service controls: fixed for providers, none left open

`blocks/llm/ui.rs` now reads `block.provider_admin.manages_providers()` at `:68` and threads it
through `render_providers_table(configs, manages)` (`:253`) and `provider_row(id, cfg, manages)`
(`:293`), rendering `cannot_manage_providers_notice()` (`:115`) instead of `add_provider_form()`
(`:138`) when false. `models_page` (`:370`) renders only `list_models` output with no write
control, so there is nothing to gate. **The carry-forward phase-5 note on the LM administration
page is closed**; re-check only if `models_page` gains a write control.

### 5.3 Second listings of a block's surface — **both still hand-spelled, one drifted**

| Page | Source | Drift vs `ROUTES` |
|---|---|---|
| `blocks/legalpages/pages.rs:283-…` `endpoints_page`, rendered inline as three `table .table`s | hand-written `<tr>`s | **Wrong method:** `:346-350` says `POST /b/legalpages/api/documents/:id/publish`; `blocks/legalpages/mod.rs:134` declares `PATCH`. **Wrong syntax:** `:id` where the routes use `{id}`. **Omits** `GET /b/legalpages/api/documents/{id}` (`mod.rs:140`) and all six `/b/legalpages/admin/*` endpoints (`mod.rs:70-115`). Lists 8 of 17 declared routes |
| `blocks/tickets/pages.rs:545-547` iterating `super::ENDPOINT_REFERENCE` (`blocks/tickets/mod.rs:216-262`) | 11 hand-written tuples | No method drift. **Omits** `/b/tickets/submitted`, `/b/tickets/admin`, `/b/tickets/admin/{types,settings,endpoints}`, `PATCH /b/tickets/api/admin/types/{id}` — 15 of 21 declared routes covered |

### 5.4 The real remaining gap: the link gate is not generalized

`blocks/admin/mod.rs:1591-…` `mod page_link_tests` (`LINK_ATTRS` at `:1616-1624`, `links_in` at
`:1628-1650`, `every_link_an_admin_page_emits_resolves_to_a_declared_row` at `:1809`) is the
mechanism that keeps admin honest, and `blocks/products/tests/page_link_tests.rs` is its twin.
**Nine SSR-page blocks have no equivalent:** llm, files, legalpages, tickets, messages, vector,
userportal, auth_ui, dev. My sweep says they are clean *today*; nothing keeps them clean.

---

## 6. What the visual baseline suite covers, and what would move it

### 6.1 Where the baselines live and what runs them

Three baseline trees, **92 PNGs total** (the `chore/baselines-out-of-mcp-scratch` move landed —
they are in Playwright's per-spec `*-snapshots/` dirs, not `.playwright-mcp/`, which is gitignored
at `.gitignore:19-24`):

| Tree | PNGs | Spec | Config | Tolerance |
|---|---|---|---|---|
| `crates/impresspress-web/tests/e2e/visual-baseline.spec.ts-snapshots/` | **64** | `visual-baseline.spec.ts` (215 lines) | `tests/playwright.visual-baseline.config.ts` (native server :8093 in CI, admin `storageState`) | `maxDiffPixelRatio: 0.01` (`:73`) |
| `crates/impresspress-web/tests/e2e/products-lifecycle.spec.ts-snapshots/` | 8 | `products-lifecycle.spec.ts` (mocked `page.route()` origins, no server) | `tests/playwright.config.ts` | Playwright default |
| `examples/tests/products-examples.spec.ts-snapshots/` | 20 | `products-examples.spec.ts`, 10 slugs × 2 | `examples/products.playwright.config.ts` (static `http.server` :4178) | `maxDiffPixelRatio: 0.015` |

- Desktop = `devices['Desktop Chrome']`; mobile = `{375, 812}` for both web suites, `{390, 844}`
  for examples.
- **Masks (admin describes only), `visual-baseline.spec.ts:103-106, 178-181, 208-211`:**
  `[data-relative-time], .relative-time, time` **and `td[data-label="Owner"], td[data-label=
  "Created"], td[data-label="Created By"]`.** The `data-label` attribute is emitted **only by
  `components::data_table`** (`ui/components/table.rs:53`) — raw `table .table` markup emits none.
  See §6.3, this is load-bearing.

  > **Correction (the masking pull request, landed with this document).** There is now a third
  > locator in each of those three arrays, `[data-volatile-metric], .stat-card:has-text("Avg
  > Response") .stat-value`, and the `time` half of the first locator has acquired matches it did
  > not have: `blocks/admin/pages/network.rs` now wraps its two wall-clock stamps in `<time>` and
  > its three duration figures in `<span data-volatile-metric>`. The wrappers sit inside the cell
  > rather than on the `<td>`, precisely because `data_table` owns the `<td>` and takes only inner
  > markup — see the coordination notes. Line numbers in this bullet predate that change.
- **CI:** `.github/workflows/ci.yml` job `e2e-visual` (line 1136) runs the suite **on
  `pull_request` only**. `.github/workflows/ci-main.yml` has **no `e2e-visual` job** — post-merge
  pushes to main never run it. `products-browser` (`ci.yml:1225`) and `product-examples`
  (`ci.yml:1264`) run in both.
- **The regen workflow cannot bake in a sub-tolerance change.** Playwright 1.59's
  `--update-snapshots` rewrites only the baselines whose comparison *fails*; one that still passes
  is left byte-identical on disk. So a change that repaints or shifts fewer than
  `maxDiffPixelRatio` pixels — a newly added mask, for instance — produces "No baseline drift —
  nothing to commit" and the stale image stays committed, to surface later mixed into whatever
  larger change does exceed the tolerance. The way to force it is to delete the baseline in a
  commit and let the regen recreate it: verified on 1.59.1 that a missing snapshot is written and
  the run still exits 0, and the workflow's commit step already uses `git status --porcelain`
  specifically so it picks the recreated file up as untracked.
- **Regen:** `.github/workflows/regen-visual-baselines.yml`, `workflow_dispatch` with a `branch`
  input; runs `--update-snapshots` for all three suites and commits as `github-actions[bot]`. Per
  the standing note, a bot push does **not** retrigger PR checks unless `BASELINE_PUSH_TOKEN` is
  set — push again yourself after a regen.

### 6.2 Coverage by area

| Area | Baselines | Notes |
|---|---|---|
| **admin** | 8, **desktop only** — `admin-admin-{blocks,dashboard,database,email,network,permissions,users,variables}` | mobile deliberately excluded (spec comments `:121-127`); `admin-storage`, `admin-logs`, `admin-storage-shares` dropped for row-accumulation drift (`:16-23, 61-65`) |
| **products** | 16 admin + 12 portal + 8 lifecycle + 20 examples = **56** | the most-covered area by far |
| **userportal** | 8 (desktop+mobile) | |
| **auth** | 4 login/signup + 2 `admin-portal-orgs` | |
| **files** (`/b/storage/*`, `/b/cloudstorage/*`) | 10 | thin/empty-state seeds only; two routes deliberately dropped |
| **llm** | 2 (`admin-llm-chat` desktop+mobile) | chat page only; `/b/llm/providers`, `/b/llm/models`, `/b/llm/settings` **have none** |
| **vector** | 1 (`admin-vector-list-desktop`, empty state) | detail page deliberately out of scope |
| root/status | 2 | |
| **legalpages** | **none** | |
| **tickets** | **none** | |
| **messages** | **none** | |
| **dev** | **none** | functional specs only, no `toHaveScreenshot` |

### 6.3 Which inventoried changes would move baselines

**Would move baselines (wire contract):**

1. **Admin's 19 raw tables → `data_table`.** Different chrome entirely: `.data-table` has
   `border: 1px solid`, `border-radius`, `background: var(--surface-1)`, a **sticky** `thead` on
   `--surface-3`, `--text-xs` uppercase headers with `letter-spacing: 0.04em`, `1px dashed`
   row separators and a `--space-2/--space-3` cell padding, against `.table`'s flat `--space-3`
   padding, `--bg-secondary` header and solid `1px solid` separators (`table.css:2-23` vs
   `:73-95`). Moves all 8 admin baselines. **Second-order effect:** `data_table` emits
   `data-label` on every `<td>`, which **activates the `td[data-label="Owner"|"Created"|"Created
   By"]` mask** in `visual-baseline.spec.ts:104-106` on admin pages for the first time — columns
   that are currently *compared* would become *masked*. That is a good outcome (less drift), but
   it means the regen is not reviewable as a pure pixel diff.
2. **Unifying the two page-header renderers** — different element, different classes, different
   spacing. Would move every baseline in every suite. *Unless* scoped to admin only, where all
   seven `PageHeader`s pass `title: ""` and render empty markup, making an admin-only cutover
   pixel-neutral.
3. **Deleting `.table-container` / `.table` CSS** while any of the 10 non-admin raw tables remain.
   Guarded: `ui/mod.rs:1725` `pages_use_only_classes_defined_in_the_stylesheet` fails the build
   first, so this cannot silently ship — but it means the CSS deletion must land with, or after,
   the last markup migration.

**Should NOT move baselines, but must be verified:**

4. **Admin's 35 hand-written buttons → `components::button`.** `button()` emits
   `class="btn btn--primary btn--md"`; `button.css:29-38` gives the bare `.btn` **`--md`'s exact
   padding and font-size** ("The base carries `--md`'s padding so a bare `.btn` (no size modifier)
   is a medium button"), so adding `btn--md` is a visual no-op. Two blockers: `BtnVariant`
   (`ui/components/button.rs:60-65`) has no `Success`, and `blocks/admin/pages/users.rs:211` uses
   `btn--success`; and `button()` only emits `<button>`, while 7 of admin's 35 are
   `<a class="btn …">` anchors (`blocks.rs:90,94,192,354`, and 3 more). Needs a `Success` variant
   and either an anchor variant or those 7 left alone.
5. **Admin's 39 raw badges → `components::badge`.** Emits `span.badge.badge-{success|danger|
   warning|info}` — byte-identical to the raw markup for those four. But admin also uses
   `badge-primary` (`users.rs:192,376`, `blocks.rs:448`, `permissions.rs:530`), `badge-secondary`
   (`permissions.rs:112,191,528`, `variables.rs:417,447`) and `badge--tone-slate`
   (`blocks.rs:342,442,456`) — six of the eight classes have no variant. Needs the enum widened to
   6–7 variants, or those sites left raw. Nine of the 39 also compute the class inline from a
   status code (`network.rs:200`, `logs.rs:125`, `dashboard.rs:323`, `blocks.rs:398,403`,
   `permissions.rs:523`) and need `status_badge`-style helpers.
6. **`data-action` delegation replacing the 111 attribute-handler lines and the 3 copied
   `removeAttribute('hidden')` scripts.** Changes attributes only, not layout. `database.rs:90`'s
   `oninput` filter is the only one with visible behaviour (row hiding) — it needs a behaviour test,
   not a screenshot.
7. **Moving `PRODUCT_WIZARD_JS` to a file and the block-owned-asset move.** Turns inline
   `<script>` into `<script src>`. `visual-baseline.spec.ts` waits `networkidle`, so the desktop
   suite should settle — **but `products-lifecycle.spec.ts` mocks its origins with `page.route()`
   and would 404 the new asset URL**, and it has no `maxDiffPixelRatio` override. That spec's 8
   baselines cover exactly the products flows this change touches. Verify the mock passes through
   `/b/static/*` before assuming it is safe.

   > **Correction (PR 5 of ruling 5.6, 2026-09-09).** Verified, and the second half is wrong.
   > `products-lifecycle.spec.ts` builds every page it exercises as a string in the spec file and
   > fulfils it from `page.route()`, inlining the script source into that string. It issues no
   > request for a page asset, so a new `/b/static/` URL cannot 404 there and the mock needed no
   > pass-through. The real coupling was to `pages.rs` as a *text file*: six product specs
   > extracted the script constants with a regular expression over the Rust source, and all six
   > had to be pointed at the `.js` files instead.

**Blind spots to bear in mind:** legalpages, tickets, messages and dev have **no visual coverage at
all**, and llm has coverage only for the chat page. `blocks/legalpages/pages.rs` (627 lines),
`blocks/tickets/pages.rs` (672) and `blocks/llm/ui.rs` (706) can be changed without any pixel
signal. Rust render tests are the only gate there.

---

## 7. Proposed pull-request split

Strictly within `docs/CODE_REVIEW_2026-09-05.md:317`. Two of its five clauses are already
satisfied (`shell_page`) or already fixed (B16/B24), so five PRs remain plus one closing cleanup.

| # | Branch | What | Baselines |
|---|---|---|---|
| 1 | `ui/block-owned-static-assets` | Move `llm-chat.js`, `marked.min.js`, `purify.min.js` from `ui/assets/` to `blocks/llm/assets/`; `files-browser.js` to `blocks/files/assets/`. Delete the four `#[cfg(feature = "block-…")]` arms from `ui/assets.rs:152,162,172,180`. Follow the documented precedent in `blocks/dev/assets.rs:1-9`. Resolve the messages→llm-chat.js consumer explicitly (`blocks/messages/pages.rs`). | **No move** — script URLs change, pixels do not. Verify `admin-llm-chat*` and `admin-storage-*` still load their JS. |
| 2 | `ui/hashed-chrome-js` | The "one hashed `admin.js`" clause. Move `palette_js`/`drawer_js`/`toast_js`/`modal_js` (202 inlined lines, `ui/assets.rs:398-620`) into a real `ui/assets/chrome.js`, served hashed through `ui/assets.rs::bytes` like `htmx.min.js`. Emit one `<script src>` from `ui/mod.rs:213-214` and `ui/layout.rs:45-46` instead of four inline blocks. | **No move.** Removes 202 lines from every page's `<head>`. |
| 3 | `ui/data-action-delegation` | Convert the 111 attribute-handler lines to `data-action`/`data-*` and one delegated listener in the PR-2 `chrome.js`, applying the rule stated at `blocks/admin/pages/network.rs:62-65`. Includes `database.rs:90`'s 430-char `oninput`, the three copied `removeAttribute('hidden')` scripts (`blocks.rs:331,463`, `variables.rs:579`) and the two eye-toggles (`settings_form.rs:134`, `auth_ui/pages/mod.rs:176`). Add a render test that no page emits an `on*=` attribute. | **No move**; needs a behaviour test for the DB filter. |
| 4 | `ui/admin-badges-and-buttons` | Admin's 39 raw badges → `components::badge`, widening `BadgeVariant` to cover `primary`/`secondary`/`tone-slate` and adding status-derived helpers for the 9 computed sites. Admin's 28 `<button class="btn …">` → `components::button`, adding a `Success` variant; leave the 7 `<a class="btn">` anchors or add an anchor variant. | **Should not move** — verify against the CSS equivalences in §6.3(4)(5). Run regen; expect an empty diff. If it is non-empty, that is a finding. |
| 5 | `ui/admin-data-table` | Admin's 19 raw `table .table` → `components::data_table`. | **MOVES all 8 admin baselines**, and newly activates the `td[data-label]` mask. Land alone; regen; re-push manually so CI reruns. |
| 6 | `products/wizard-js-to-file` | `PRODUCT_WIZARD_JS` (403 lines) + `PRODUCT_MANAGER_JS` (39) + `PRODUCT_CATALOG_ADMIN_JS` (12) + `SELLER_ADMIN_JS` + `ORDER_DETAIL_JS` + `stripe_setup_js()` (159) + `commerce_portal_js()` (29) → `blocks/products/assets/*.js`, served the way `storefront.js` already is (`handlers/commerce.rs:73`). Keep the `window.__*Config` bootstrap inline (it is per-request), and add a `</script>`-safe JSON helper for the five `PreEscaped(format!(…))` sites while there. | **Risk.** `visual-baseline.spec.ts` waits `networkidle` and should be fine; `products-lifecycle.spec.ts` mocks its origins and will 404 the new URLs unless the mock passes `/b/products/static/*` through. Its 8 baselines cover exactly these flows. |
| 7 | `ui/delete-gen1-templates` | Cut admin's 7 `PageHeader{title:""}` sites to `components::page_header` (they render nothing today, so this is free), delete `templates::StatTile` in favour of `stat_card` at the 5 `dashboard.rs` sites, and delete `.table-container`/`.table` from `table.css:2-23` once nothing renders them. | Admin `PageHeader` removal is pixel-neutral; `StatTile` removal is pixel-neutral (`templates.rs:240` already delegates). **CSS deletion is blocked until files/vector/llm/legalpages/tickets/userportal's 10 remaining raw tables are migrated** — `ui/mod.rs:1725` will fail the build. Ship it as "delete what is unused", not "delete gen-1 wholesale". |

> **Superseded by ruling 5.6.** The ruling consolidates rows 1 and 2 — both are asset plumbing
> with no rendered change — into a single pull request, which shipped as **PR #46**. The table's
> seven rows therefore map onto the ruling's six: 1+2 = ruling PR 1 (merged), 3 = 2, 4 = 3, 5 = 4,
> 6 = 5, 7 = 6. Read the ruling for the sequence and this table for the detail of each row.

**Argued order.** 1 → 2 → 3 → 4 → 5 → 6 → 7.

- **1 before 2** because 1 establishes where block assets live before 2 adds a new shared one.
- **2 before 3** because 3 needs a served, hashed script to put the delegated listener in;
  otherwise the delegation lands in a fifth inline block and the census gets worse.
- **3 before 4/5** because it is attribute-only and pixel-neutral, so it lands while the baselines
  are still known-good. If 4 or 5 lands first, a subsequent regen mixes markup and behaviour
  changes in one diff.
- **4 before 5** because 4 is expected pixel-neutral and 5 is not: keeping the one baseline-moving
  PR isolated is what makes the regen reviewable. If 4's regen is non-empty, that is evidence of a
  CSS equivalence I got wrong, and it is cheap to find with 4 alone in flight.
- **5 alone**, then regen + manual re-push.
- **6 late** because its risk is in a different suite (`products-lifecycle`) and should not be
  entangled with an admin regen.
- **7 last** because the guard test makes deletion possible only after every migration, and
  because it is the only PR whose value is purely subtractive.

**Baseline-moving PRs: #5 only, with #6 as a suite-availability risk rather than a pixel risk.**
PRs 1, 2, 3, 4, 7 should each produce an empty `--update-snapshots` diff; a non-empty one is a
finding to explain, not a diff to accept. Remember `ci-main.yml` has no `e2e-visual` job — the only
signal is on the PR itself.

---

## 8. Adjacent work found, deliberately NOT folded in

Each is real; none is inside the phase-5 scope line.

1. **Generalize the link gate (§5.4).** `blocks/admin/mod.rs:1591`'s `page_link_tests` and its
   products twin cover 2 of 11 SSR-page blocks. llm, files, legalpages, tickets, messages, vector,
   userportal, auth_ui and dev have none. Carry-forward already names this as the pattern to reuse.
   Best as its own PR; it belongs with routing/T1 discipline, not UI migration.
2. **Render `legalpages::endpoints_page` and `tickets::ENDPOINT_REFERENCE` from `ROUTES`
   / `info().endpoints` (§5.3).** legalpages is actively wrong (`POST` vs declared `PATCH`,
   `:id` vs `{id}`, 8 of 17 routes). Carry-forward lists it under phase 5, but the phase-5 line
   does not scope it and neither page has a visual baseline. Small, self-contained, high value.
3. **Reconcile `entry_card` ↔ `messageCardHtml` (§4).** The "keep in sync" comments are false
   today: Rust uses semantic classes, JS uses inline hex. Either give the JS the same classes and
   delete the styles, or delete the comments and state that they are two different surfaces.
   Touches `blocks/messages/pages.rs:29-72` and `ui/assets/llm-chat.js:52-80`.
4. **A `</script>`-safe JSON-into-script helper** for the five `window.__*Config` sites. Folded
   into PR 6 above as a one-function addition; if that feels like scope creep, split it out.
5. **`ui/components/button.rs:93-105` builds HTML by `format!` with an unescaped `extra_attrs`
   passthrough.** No caller interpolates data into it today, but it is the one component that
   cannot be made safe by maud's escaping. Consider a typed attribute list.
6. **Three modal mechanisms (§1.4).** Unifying `<dialog>` (files) with `div.modal-overlay`
   (everything else) is a behaviour change (`showModal()` vs `hidden`, focus trapping, backdrop)
   and would move the `admin-storage-buckets` baselines. Out of scope for a class migration.
7. **Admin god functions (review §5.5, `docs/CODE_REVIEW_2026-09-05.md:237`).** Current sizes:
   `blocks/admin/pages/dashboard.rs` 588 lines, `permissions.rs` 586, `variables.rs` 719,
   `blocks.rs` 655. Splitting them would collide with PRs 4/5 line-for-line; do it after, not
   during.
8. **`SiteConfig::embedded_scripts`** (`ui/mod.rs:33,69`, `ui/templates.rs:520`) — an
   admin-configurable external-`<script src>` injection on public pages. A deliberate feature; not
   part of the five-ways census but worth a security look of its own.
9. **`llm/ui.rs`'s three page surfaces have no visual baseline** (§6.2). If PR 4/5's approach is
   ever extended to llm, add `/b/llm/providers` and `/b/llm/models` to `ADMIN_ROUTES` first.
