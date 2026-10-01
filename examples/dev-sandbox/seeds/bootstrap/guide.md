# Building the site in this sandbox

This sandbox seeded a site built on **Bootstrap 5.3.8**, vendored under
`site/vendor/bootstrap/` (stock build, nothing customised, MIT — see
`vendor/bootstrap/LICENSE.txt`). Keep that directory, and never
`dev_read_file` anything under it: a minified framework file is hundreds
of KiB of context that tells you nothing. The welcome page,
`site/index.html`, is a worked example of the framework in use.

## How `site/` works

- Every file under `site/` is published verbatim, and every write publishes
  a new generation immediately — there is no separate deploy.
- `site/index.html` is the entrypoint, served at `/`. A subdirectory is a
  route: `site/blog/index.html` serves at `/blog/`, `site/about.html` at
  `/about.html`.
- Read a file before overwriting it: `dev_write_file` takes the file's
  current `sha256` as `expected_sha256`; a new file takes `null`.
- `dev_rollback` republishes an earlier generation when something regresses.

## Page skeleton

Every page you write carries this `<head>`. No stylesheet of your own is
needed; add one only for what Bootstrap's utilities cannot express.

```html
<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  <title>Page title</title>
  <link rel="stylesheet" href="/vendor/bootstrap/bootstrap.min.css" />
  <script src="/vendor/bootstrap/bootstrap.bundle.min.js" defer></script>
  <script src="/b/webmcp/webmcp.js" defer></script>
</head>
```

`/b/webmcp/webmcp.js` gives a visitor's own browser agent the site's public
tools — the shop's, and any compiled block's agent tools. Without the tag a
visitor's agent sees a plain page.

## Bootstrap here

Prefer the components the framework already styles:

- Layout: `.container`, `.row` / `.col-*`, the spacing utilities (`py-5`,
  `mb-3`, `g-4`).
- Navigation: `.navbar` with `.navbar-brand`; a `.btn.btn-primary` for the
  main action.
- Content: `.card` / `.card-body` grids for products and features;
  `.display-5` and `.lead` for a hero; `.badge` for tags.
- Forms: `.form-control`, `.form-label`, `.form-select`, `.btn`.
- Feedback: `.alert`. `.modal` and `.collapse` work from `data-bs-toggle`
  attributes because the bundle is loaded; a `.toast` is shown from script
  with `bootstrap.Toast.getOrCreateInstance(el).show()`.
- Theme: add `data-bs-theme="dark"` on `<html>` for a dark site.

Bootstrap Icons are **not** vendored; use text, Unicode or an inline SVG.

## The shop

Products are managed with the `shop_*` tools on this page. A page reads them
through two public pieces:

- `GET /b/products/catalog` lists active products as JSON:
  `{"records": [...], "total_count": N, "page": 1, "page_size": M}`. Each
  record carries `id`, `name`, `slug`, `description`, `image_url`, `tags`,
  `category`, `currency`, `stock`, `metadata` and `fulfillment_kind`. Pass
  `?page=2` for the next page; `?page_size=` goes up to 100. Render the
  list into a `.card` grid with a small script (`esc` keeps a product's
  text from being read as markup):

```html
<div id="products" class="row g-4"></div>
<script>
  const esc = s => String(s ?? '').replace(/[&<>"']/g, c => `&#${c.charCodeAt(0)};`);
  fetch('/b/products/catalog').then(r => r.json()).then(({ records }) => {
    document.getElementById('products').innerHTML = records.map(p => `
      <div class="col-md-4"><div class="card h-100">
        ${p.image_url ? `<img class="card-img-top" src="${esc(p.image_url)}" alt="">` : ''}
        <div class="card-body">
          <h3 class="h5 card-title">${esc(p.name)}</h3>
          <p class="card-text">${esc(p.description)}</p>
          <a class="btn btn-outline-primary" href="/product.html?id=${encodeURIComponent(p.id)}">View</a>
        </div>
      </div></div>`).join('');
  });
</script>
```

- `<impresspress-product product-id="…"></impresspress-product>` renders one
  product's price and buy button. Load
  `<script src="/b/products/storefront.js" defer></script>` once per page.
  Attributes: `product-id` (required), `presentation` (`hosted`, `embedded`
  or `payment_link`; default `hosted`), `payment-link-id` (with
  `presentation="payment_link"`; default: the offer's first link),
  `success-url` and `cancel-url` (where checkout returns; default: this
  page), `api-base` (default: this origin), `credentials` (`same-origin`,
  `omit` or `include`).

A product appears in the catalog once `shop_update_product` sets
`status: "active"`; it can be bought once it has a published offer
(`shop_create_offer`, then `shop_publish_offer`).

## Calling a backend block from a page

A block you compiled serves under `/b/<name>/`. Call it with
`fetch('/b/<name>/…')` — same origin, so no CORS or credentials setup — and
send and read JSON.

## What a write refuses

- A path outside `site/` or `blocks/<name>/`, a `..` segment, or a name that
  clashes with an existing file or directory.
- A file over 512 KiB; more than 2,000 files; more than 64 MiB of stored
  content in the workspace; more than 16 backend blocks.
- A site file at a URL the runtime reserves for its own static files, which
  would never be shown: `site/manifest.json` (served at `/manifest.json`),
  `site/sw.js`, the shell's `site/vendor/sql-wasm*` files, or anything under
  `site/snippets/`, `site/seed/` or `site/cdn-cgi/`. The refusal names the
  rule; pick another path (e.g. `site/app.json`). The rest of
  `site/vendor/` is yours.
- A stale `expected_sha256`: the refusal carries the current hash, so
  re-read and retry.

## Workflow

1. `dev_status`, then this reference.
2. Read `site/index.html` (for its hash and as the example), then overwrite
   it with your page.
3. Add pages and assets with further writes: `dev_write_files` for a
   scaffold of several pages (one generation for the whole batch),
   `dev_write_file` for one file (one generation each).
4. Stock the shop with `shop_*`, then check the live site at `/`.
5. `dev_export` when done.
