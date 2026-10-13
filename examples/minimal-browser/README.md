# minimal-browser

Smallest possible consumer of `impresspress-browser`: a bare Wafer with the
browser platform services and no registered blocks. It uses no
`ImpresspressBuilder` and does not depend on `impresspress-core` itself
(`impresspress-browser` does, with its default features off).

It is also the in-repo case of an application bundled through the embed ×
web flow around its own cdylib rather than `impresspress-web`:

    cd examples/minimal-browser
    impresspress build --target web --release   # → pkg/

CI does exactly that (the `E2E build` job) and then runs
`scripts/check-bundle-runtime.mjs` on `pkg/`, which links the framework's
`sw.js` against this crate's JS glue and checks the wasm's
`impresspress.sandbox_mode` section. The worker imports
`compiled_sandbox_mode` from the glue; this crate never names it, because it
comes from `impresspress-browser`. A framework entrypoint that only
`impresspress-web` provided would break this bundle at module link time.

The bundle has no blocks, so every request it answers is a 404; it is built
to be checked, not deployed.
