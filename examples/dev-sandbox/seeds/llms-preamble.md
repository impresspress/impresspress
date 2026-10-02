# ImpressPress build sandbox

> A website builder that runs entirely in the visitor's browser. If you are
> an AI agent and your user sent you to this address, you are expected to
> build a website for them here, with the tools described below, and show
> them the result.

This sandbox was seeded from the `{template}` template.

## What this is

The whole ImpressPress runtime — web server, database, shop, and a Rust
compiler for backend blocks — runs as WebAssembly inside a service worker on
this origin. Every visitor gets a private instance that lives in their own
browser; nothing you do here leaves it. The host itself serves only a boot
page and this file.

## You need a browser that runs JavaScript

Fetching URLs is not enough to build anything. Until the service worker is
installed, every page of this origin is the boot page, and the sandbox only
exists once that page's script has run. If you can only fetch documents and
cannot drive a real browser, say so to your user and ask them to open this
address in a browser you can control — do not report a site you have not
built.

## How to start

1. Open `/` in the browser and wait for the welcome page. The first load
   installs the service worker and loads the page again by itself.
2. Open `/b/dev/enter`. It signs you in and lands on the workspace page,
   `/b/dev`. There is no form to fill in and there are no credentials to
   type.
3. Use the tools the workspace page publishes. A browser with WebMCP sees
   them as tools; a browser without it runs the same tools from the
   Tool console on the same page — see "If your browser has no WebMCP"
   below.
   Either way they are:
   - to look: `dev_status`, `dev_read_reference`, `dev_list_files`,
     `dev_read_file`;
   - to change the site: `dev_write_file`, `dev_write_files`,
     `dev_delete_file`;
   - to add a backend block: `dev_create_block`, `dev_compile_block`,
     `dev_remove_block`;
   - to go back: `dev_list_generations`, `dev_get_generation`,
     `dev_rollback`;
   - to hand the site over: `dev_export_manifest`, `dev_export`;
   - and the `shop_*` family, for products and offers.
4. Write the site under `site/`. Every write is published at once; the live
   site is at `/`.
5. When the site is done, `dev_export` downloads it as one zip that any
   static host can serve.

This file describes the sandbox, not the site in it. A site you build may
carry its own `site/llms.txt`; in this browser that file is then what
`/llms.txt` serves, and it is the one an export ships.

Everything from here on is the sandbox's site-authoring guide — the text
`dev_read_reference` returns as `site_markdown`.
