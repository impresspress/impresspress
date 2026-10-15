# Test fixtures for `tests/cloudflare_entry_module.rs`

`worker-build-0.7.5-shim.js` is `src/js/shim.js` from the `worker-build` crate,
version 0.7.5 (crates.io, github.com/cloudflare/workers-rs), **unmodified**.
It is licensed under the Apache License 2.0; the crate's licence is included
verbatim as `LICENSE-worker-build`. The test fills the shim's placeholders in a
temporary copy at run time; this file is never edited.

`index.js`, `workers-stub.mjs` and `driver.mjs` are this repository's own
stand-ins (MIT, like the rest of the repository).
