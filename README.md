# Impresspress

## Built with Codex and GPT-5.6

We used Codex and GPT-5.6 to design and build the products feature through an extensive [14-phase implementation plan](docs/2026-07-19-products-stripe-implementation-plan.md). We began by auditing the existing products block and agreeing on the architecture and safety constraints, then worked in small, verified slices across the commerce schema, pricing engine, Stripe and Connect integrations, admin and seller experiences, buyer storefront, SDK, and documentation.

Each phase paired implementation with focused tests and a running verification log, allowing the plan to evolve without losing unfinished work or weakening its original safeguards. The final workflow was exercised through Rust and SDK suites, end-to-end Chromium journeys, responsive visual checks, and ten distinct example storefronts covering the supported product and payment experiences.

## Run Impresspress locally

Install the Rust WASM target and the build tools, then build the web assets and native binary from the repository root:

```sh
rustup target add wasm32-unknown-unknown
cargo install just
cargo install wasm-pack --version 0.15.0
just build-debug
```

Start Impresspress with a first-run administrator account:

```sh
WAFER_RUN_SHARED__AUTH__BOOTSTRAP_ADMIN_EMAIL=admin@example.com \
WAFER_RUN_SHARED__AUTH__BOOTSTRAP_ADMIN_PASSWORD=admin123 \
IMPRESSPRESS_LISTEN=127.0.0.1:8090 \
./target/debug/impresspress serve --target native --run-migrations
```

Open <http://127.0.0.1:8090/b/auth/login> and sign in with `admin@example.com` and `admin123`. Local data is stored under `data/` by default.
