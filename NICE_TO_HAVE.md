# Nice-to-Have Improvements

Low-priority improvements identified during code review. None are blocking, but each would improve the project's operational maturity.

## Performance

- **KV caching for project resolution in dispatch worker** — Currently every request queries D1 to resolve the project subdomain. A Cloudflare KV cache with a short TTL (e.g. 60s) would reduce latency and D1 load for hot projects. Requires cache invalidation on project config changes.

## Testing

- **Code coverage tracking with cargo-tarpaulin** — No coverage metrics are currently tracked. Integrating `cargo-tarpaulin` into CI would identify untested code paths and track coverage trends over time.

- **Multi-browser Playwright matrix** — E2E tests currently run Chrome only. Adding Firefox and Safari (webkit) to the Playwright config would catch browser-specific rendering and API issues.

- **Component-level frontend tests** — Frontend code has no unit tests (only E2E via Playwright). Adding Vitest for Preact component and utility function tests would catch regressions faster and without the overhead of full browser automation.

## Operations

- **Load/performance testing setup** — No load testing exists. A basic k6 or Artillery script targeting auth, storage, and admin endpoints would establish baseline throughput numbers and catch regressions.

## Scalability

- **Release asset key inventory scaling — resolved.** The inventory now lives in `{prefix}/keys.json` in R2 (digest-pinned, fetched once per isolate, fail-closed on a digest mismatch) instead of being inlined into the `IMPRESSPRESS_RELEASE_ASSET_KEYS_JSON` Worker var, so the Worker vars are O(1) regardless of asset count. The two secondary issues that came with the old approach — `manages_folder`'s O(n) scan over the full key set, and an abort message that named an infrastructure variable content authors had never heard of — are gone with it. Parsing and loading live in `impresspress-core/src/release_inventory.rs`, where they're covered by native tests.
