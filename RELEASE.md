# Releasing Impresspress

## Version Scheme

Impresspress uses [Semantic Versioning](https://semver.org/): `MAJOR.MINOR.PATCH`

- **MAJOR** — breaking changes to CLI flags, config format, or stored data
- **MINOR** — new features, new blocks, new config options
- **PATCH** — bug fixes, security patches, dependency updates

## Upgrade Notes

Notes for operators upgrading an **existing** deployment. On native,
migrations are gated: they run on a fresh install, or when the operator opts
in with `impresspress serve --run-migrations`. A Cloudflare deploy
(`impresspress deploy`, which has no such flag) always runs them: its
`/_deploy/prepare` funnel applies every block's migrations before the new
version is promoted. A browser install applies them on the first boot of a
bundle that changes them. So whenever a release's code half assumes a data
repair the migration half performs, it has to be called out here — on native
the two ship together but only one of them runs by default.

### Refresh tokens end at every `auth_version` bump

**What changes.** A refresh token now carries the account's `auth_version`
from when its sign-in began, and `/b/auth/api/refresh` refuses (and revokes)
one the account has moved past. Every bump therefore signs the account's
devices out at their next refresh, not only a password change or reset: a
role grant or removal, a role rename or delete that touches the account, a
disable followed by an enable, and a soft-delete. Before, those bumps retired
access tokens only and the next refresh minted a fresh one. This closes a
race: a sign-in that checked the OLD password while a password change ran
could write its refresh row after the change revoked every row, and that token
kept refreshing.

No migration. A refresh token issued before this release has no version claim
and reads as `0`, so it keeps working for an account whose `auth_version` has
never been bumped; on any other account the next refresh is refused and the
user signs in again, once.

**Who has to act.** Nobody. Expect users who have had a role, lifecycle or
password change at any point to be asked to sign in once after the upgrade,
and users whose role an admin changes to be asked to sign in again.

### Cloudflare: a new site deploys in one command

**What changes.**

- `impresspress deploy --target cloudflare` creates a site's main Worker
  itself. Before it builds, it asks Cloudflare whether the Worker exists
  (`wrangler deployments status`). When it does not, then after deploying the
  password-hasher Worker it deploys a placeholder under the main Worker's name
  that answers every request with a 503, sets `IMPRESSPRESS_DEPLOY_TOKEN` and
  `WAFER_RUN__AUTH__JWT_SECRET` on it (each from the same-named environment
  variable when set, otherwise generated), prints a generated deploy token
  once with the `export` line later deploys need, and continues through
  `/_deploy/prepare`, verification and promotion, which replaces the
  placeholder. No plain `wrangler deploy` of either Worker and no
  `impresspress deploy secret` is needed first any more.
- A deploy to a Worker that exists first sets whichever of those two secrets
  the Worker does not hold (it lists secret names, never values), so a first
  deploy that stopped part-way is finished by running it again. A generated
  deploy token is printed as soon as it is set; if that output is a CI log
  others can read, rotate it, or export the token before the first deploy.
- A deploy to a Worker that holds its deploy token still needs
  `IMPRESSPRESS_DEPLOY_TOKEN`, and without it now stops before the build,
  naming the command that sets a new one (`npx wrangler secret put
  IMPRESSPRESS_DEPLOY_TOKEN --name <worker_name>`).
- D1 runs 1,000 queries per invocation on Workers Free, not 50: a Free-plan
  Worker runs a fresh database's `/_deploy/prepare`, over 250 D1 queries, in
  one invocation, and the Workers limits page caps subrequests to internal
  services (D1, KV, R2) at 1,000 on Free. D1's own limits page still says 50.
  `/_deploy/prepare` applies every pending migration in one invocation, so a
  new site whose `[cloudflare].d1_queries_per_invocation` was 50 could never
  finish its first deploy.

**Who has to act.** A Cloudflare deploy that set
`d1_queries_per_invocation = 50` on the advice of the D1 statement budget
note below: remove the line, so the Worker runs on the default 1000. Keep a
lower value only if the Worker's `limits.subrequests` is below 1,000, and
then no lower than a fresh database's migrations need. Nobody else.

### API: a request with no identity is 401, not 403

**What changes.** An API call (any `Accept` that is not an HTML page) to a
protected route without a working credential — none at all, or one that did
not verify: expired, signed out, issued before a password change or a
disable, an API key that matches no row — is now answered `401` with
`WWW-Authenticate: Bearer realm="impresspress", ApiKey realm="impresspress"`
and the JSON body `{"code":"not_authenticated","error":"Unauthenticated",
"message":"authentication required"}`. It was `403` with `"error":"PermissionDenied"`. The handlers'
own identity checks behind the router (`/b/auth/api/me`, change-password,
API keys, products, files, the shared owner check, the user portal's profile
form, the sessions and linked-accounts buttons) answer `401` too, with the
same challenge; the profile form's was a `403`. A `401` about a credential
that is not an `Authorization` scheme (a wrong password at login, a
bootstrap or refresh token, a webhook signature) carries no challenge.

- `403` now means only "identified, but not allowed": a signed-in user
  without the admin role on an admin route, a CSRF origin refusal, a WRAP
  denial. A credential the server could not check because its database read
  failed keeps its `503` (or the `403`/`429` a WRAP refusal or quota gives
  it); that is not "signed out".
- Browser pages are unchanged: a page request with no session is still
  redirected to `/b/auth/login?redirect=…`.
- The JS SDK already treated `401` as "signed out" (`getUser()` resolves
  `null`); against earlier servers it threw on an anonymous call instead. A
  client of your own that read `403` as "sign in again" should read `401`.

### Cloudflare: passwords are hashed by a password-hasher Worker

**What changes.** The main Worker no longer hashes or verifies passwords.
`impresspress build --target cloudflare` now builds a second, small Worker —
the **password-hasher Worker**, named `<worker_name>-password-hasher` unless
`[cloudflare.password_hasher].worker_name` says otherwise — that exports one
stateless, SQLite-backed Durable Object class, `ImpresspressPasswordHasher`.
The main Worker binds it (`[[durable_objects.bindings]]`, binding
`IMPRESSPRESS_PASSWORD_HASHER`, `script_name` = the hasher) and sends every
password hash and verification there, spread across
`[cloudflare.password_hasher].shards` instances (default 8, 1-64; written into
the main Worker's `[vars]` as `IMPRESSPRESS_PASSWORD_HASHER_SHARDS`).

- New hashes are argon2id at OWASP's recommended cost, `m=19456,t=2,p=1` —
  what native writes — instead of the 4 MiB preset the Worker wrote before.
  A Free-plan Worker request is documented at 10 ms of CPU and this cost
  takes about 50-130 ms in wasm32; a Durable Object request is allowed far
  more. Stored hashes keep verifying (the cost comes from the stored string);
  nothing re-hashes them on sign-in.
- Why a separate Worker: Cloudflare generates no version preview URLs for a
  Worker that implements a Durable Object, and `impresspress deploy` prepares
  and verifies each version through one; and a Durable Object migration
  cannot ride `wrangler versions upload`. A Worker that only binds another
  Worker's class keeps its preview URLs.
- `impresspress deploy` deploys the hasher FIRST, with plain `wrangler
  deploy` (which applies its migration, tag `v1`, `new_sqlite_classes`), then
  takes the main Worker through the usual prepare/verify/promote funnel.
  Every main Worker version — previews included — calls whichever hasher is
  live, so the call is versioned: a hasher answers the protocol version of
  the main Worker release before it as well as its own
  (`impresspress_password::protocol`). `impresspress serve` runs both Workers
  in one `wrangler dev` session.
- **Deploy every release in turn.** The hasher answers the protocol version
  of the release before its own, not older ones. Deploying a release that is
  two protocol versions ahead of the live main Worker makes every password
  operation on the live site answer 503 from the moment the hasher deploys
  until the new main Worker is promoted.
- If the hasher cannot answer (missing binding, Durable Object error or
  non-200 status, an answer that does not parse or is in a protocol version
  the main Worker does not speak), the operation fails with a retryable 503
  "Authentication is temporarily unavailable": sign-in, sign-up, password
  change, password reset and bootstrap-token redemption alike, and nothing is
  written — a reset link or bootstrap token still works on the retry. If the
  hasher answers readably but wrongly (a hash weaker than it is meant to
  write, an outcome for another operation), a sign-in answers 503 and the
  others a 500. The main Worker never hashes a password itself instead.
- Each password operation adds a round trip to the Durable Object, about
  60-100 ms of wall time, on top of the hash itself. An instance runs one
  request at a time; shards spread a burst across instances.
- Durable Object requests count against the Workers Free plan's daily
  Durable Object allowance: 100,000 requests and 13,000 GB-s of duration a
  day, reset at 00:00 UTC. Every sign-in, sign-up and password change is one
  request, and so is a failed sign-in for an unknown email: it burns a
  verification on purpose, so that it takes as long as a wrong password for
  a real account, and skipping the call would tell an attacker which emails
  have accounts. Past either allowance, password operations fail
  with a 503 until the reset. **The login rate limit does not protect this
  allowance**: it is 30 requests a minute per IP (`WAFER_RUN_SHARED__RATE_LIMIT_AUTH`), about
  43,000 a day from one address, so three addresses can spend the whole
  day's allowance and lock every user out until 00:00 UTC. On a public
  Free-plan site, add a Cloudflare WAF rate-limiting rule on
  `/b/auth/api/login` and `/b/auth/api/signup`, or move to Workers Paid,
  where Durable Object requests are billed rather than capped.
- The hasher answers any Worker in the same Cloudflare account that binds
  its class by script name; see `impresspress_password::protocol` ("Who can
  call it").
- The main Worker's wasm is about 11 KB smaller (cargo's output, before
  wasm-opt); the hasher Worker is about 470 KB (185 KB gzipped) after
  `worker-build`.
- `impresspress_cloudflare::make_jwt_crypto_service(jwt_secret, peppers)` is
  now `make_crypto_service(jwt_secret, PasswordHasher::from_env(&env, shards))`
  (`impresspress_cloudflare::crypto_service::PasswordHasher`). The password
  pepper module moved from `impresspress_core::password_pepper` to
  `impresspress_password::pepper`.

**The pepper moves to the hasher.** The main Worker no longer reads
`IMPRESSPRESS_PASSWORD_PEPPER_KEY`, `…_PREVIOUS_KEYS` or `…_REQUIRED`, and the
build refuses them in its `[vars]`. They are the hasher's:

- keys: `openssl rand -base64 32 | npx wrangler secret put
  IMPRESSPRESS_PASSWORD_PEPPER_KEY --name <hasher worker name>` (and
  `IMPRESSPRESS_PASSWORD_PEPPER_PREVIOUS_KEYS` the same way). Secrets survive
  every deploy of the hasher.
- `REQUIRED`: `[cloudflare.password_hasher].pepper_required = true` in
  `impresspress.toml` (a hasher deploy replaces its vars, so it is written
  from there).

**Who has to act.**

- *A new site:* nothing; `impresspress deploy` creates the hasher, then the
  main Worker (see "Cloudflare: a new site deploys in one command").
- *An existing site without a pepper:* nothing; `impresspress deploy` creates
  the hasher on its first run.
- *An existing site with a pepper:* the hasher must hold the keys before the
  main Worker starts calling it, or every sign-in to a peppered account
  answers 503 (a pepper fault) and new hashes are written unpeppered.
  `impresspress deploy` checks this: after deploying the hasher it lists
  both Workers' secret NAMES (never values), and if the main Worker holds
  `IMPRESSPRESS_PASSWORD_PEPPER_KEY` or `…_PREVIOUS_KEYS` that the hasher
  does not, it stops before uploading any main Worker version and prints the
  `wrangler secret put … --name <hasher>` and `wrangler secret delete …
  --name <worker_name>` commands to run. To do it ahead of time, after
  `impresspress build --target cloudflare`:
  1. `npx wrangler deploy --config
     target/impresspress-cloudflare/wrangler-password-hasher.toml` (creates
     the hasher; the live main Worker still hashes on its own meanwhile);
  2. put the same key(s) on it with `wrangler secret put … --name <hasher>`,
     and set `pepper_required` if you had `REQUIRED` on;
  3. `impresspress deploy --target cloudflare`;
  4. then delete the now-unread secrets from the main Worker (`npx wrangler
     secret delete IMPRESSPRESS_PASSWORD_PEPPER_KEY --name <worker_name>`, and
     the previous keys) and drop any `REQUIRED` entry from your
     `wrangler.overrides.toml`, which the build now refuses.
- *Rolling the main Worker back* to a release before this one puts hashing
  back in the main Worker, at the old cost and with the main Worker's own
  secrets: keep the pepper secrets on it until you no longer need that
  rollback.

### wafer-run abecd3f3: a password hasher that is down is a 503

**What changes.** Setting a password — sign-up, change-password,
reset-password, bootstrap-token redemption — answers `503` "Authentication
is temporarily unavailable" when the crypto service reports that the backend
doing the hashing cannot be reached (`CryptoError::Unavailable`, which the
crypto block answers `ErrorCode::Unavailable`). It was a sanitized `500`.
Any other hashing failure is still a `500`; sign-in's `503` is unchanged.
Nothing is written first, so a retry succeeds: the reset link is not spent,
and a bootstrap token is now checked and the password hashed before the token
is consumed, so an outage no longer burns it.

- On Cloudflare the password-hasher Worker's client reports a missing
  binding, a failed Durable Object call, a non-200 status, an unparseable
  answer and a protocol-version mismatch as `Unavailable`. A readable answer
  it must not act on (a hash weaker than the hasher writes, an outcome for
  another operation, the hasher refusing to read the request) is a fault,
  a `500` outside sign-in.
- Native and browser hash in process and never report `Unavailable`, so
  nothing changes there.
- `CryptoError` gained the `Unavailable` variant and is not
  `#[non_exhaustive]`: an embedder with its own `CryptoService` or an
  exhaustive `match` on `CryptoError` needs an arm for it.

**Who has to act.** No one; a client that retries on `503` now retries
these requests too.

### WRAP grants: append-only has a column of its own (admin migration 005)

**What changes.** A custom WRAP grant stores append-only access in a new
`append` column of `impresspress__admin__wrap_grants` (`write = 0, append =
1`) instead of as `write = 2`. Migration 005 adds the column and moves any
`write = 2` row onto it. A row that sets both `write` and `append` is refused
and left out of the runtime's grants, as is a leftover `write = 2`.

**Why.** Releases from before append-only grants existed read `write` as a
flag, where any non-zero value means read-write. Rolling back to one after an
append-only row had been written would have turned that grant into a
read-write one. Such a release ignores the new column, so it now reads an
append-only row as read-only: a rollback narrows access instead of widening
it.

**What to expect.** The permissions form offers read-only and read-write, so
no deployment should hold a `write = 2` row and the repair should find nothing.
Native applies admin's DDL, 005 included, on every start, before the runtime
is built. A browser install applies it on the first boot of this bundle, and a
Cloudflare deploy in its `/_deploy/prepare` funnel before the new version is
promoted. The only window without the column is while `/_deploy/init` or
`/_deploy/prepare` builds its runtime, before the funnel migrates: grants
loaded then still decode (a missing `append` reads as unset), and the funnel
reloads them after migrating. This is an admin migration, not an auth one:
nobody is signed out.

### Browser: migrations run once per change, not on every boot

**What changes.** A browser install now runs a block's migrations when their
hash differs from the one recorded in its database — once, on the first boot
of a bundle that changes them — and skips them otherwise.

**Why.** The browser built its runtime before reading the recorded migration
state, so the gate treated every boot (every service-worker start, and every
dev-sandbox rebuild) as a fresh install and re-ran every block's full
migration set. That is where the "duplicate column name" warnings in the
worker console came from. For the auth block the set includes
`004_refresh_tokens`, which begins by dropping the refresh-token table.

**What to expect.** The first boot of this release re-runs nothing new: every
block's recorded hash already matches. From then on a browser install behaves
like a native one started with `--run-migrations`: loading a new bundle is its
deploy, so a changed migration applies once, with the same consequences it has
anywhere else: any edit to the auth migration SQL still re-runs the auth set
and signs every user out, once.

### wafer-run fa059808: password pepper, statement-budget codes, error headers

**What changes.**

- Password hashes can be **peppered** (optional, off until a key is set):
  the argon2id output is keyed with HMAC-SHA-256 by a secret held outside
  the database, so a stolen credential table cannot be cracked offline
  without the key too. New hashes are written as
  `$argon2id-hmac-sha256$…,pepper=<key id>$…`; stored hashes keep verifying
  (see *Who has to act*). The key is read from the process environment on
  native and from Worker secrets on Cloudflare, straight into the crypto
  service: it is never a `variables` row, never on either config surface, so
  no block and no admin page can read it. The browser target holds no pepper
  (nothing in a visitor's browser is secret), so a peppered hash carried into
  the dev sandbox cannot be verified there; its sign-in answers 503, not a
  wrong password.
- A request refused by the database's statement budget now says so in the
  error body's `code`: `database.statement_budget_exhausted` on the 429,
  `database.statement_budget_exceeds_limit` on a write larger than a whole
  invocation's limit, which is now a 400 (it was a sanitized 500). Neither
  can succeed if sent again unchanged, so a client must not auto-retry it;
  `/openapi.json`'s `info.description` says so, and the JS SDK has
  `isStatementBudgetError`. A page whose read the budget refused no longer
  says "try again later".
- On Cloudflare and in the browser, a streamed response whose headers cannot
  be sent is still answered with a 500, which now keeps the handler's
  security and CORS headers (`Content-Security-Policy`, `X-Frame-Options`,
  `X-Content-Type-Options`, …) and is sent `Cache-Control: no-store`.
- Blocks may declare an init time budget. impresspress declares none and
  sets no cap, so every Init — which runs the block's migrations — still runs
  as long as it takes.

**Who has to act.** Nobody, unless you turn the pepper on. To turn it on:

1. Generate a key: `openssl rand -base64 32`. **Back it up outside the
   deployment**: losing it locks out every account whose hash it peppered.
2. Set it as `IMPRESSPRESS_PASSWORD_PEPPER_KEY`:
   - native: in the process environment (a systemd `EnvironmentFile` with
     mode 0600, the orchestrator's secret store, or `.env`), then restart;
   - Cloudflare: a secret of the password-hasher Worker, which does the
     hashing (see *Cloudflare: passwords are hashed by a password-hasher
     Worker* above): `openssl rand -base64 32 | npx wrangler secret put
     IMPRESSPRESS_PASSWORD_PEPPER_KEY --name <hasher worker name>`. **Never
     under `[vars]`**: a var is plain text in the committed override file and
     in the dashboard, and the build refuses a pepper key there.
3. From then on, new and changed passwords are peppered. Existing hashes stay
   as they are (nothing re-hashes them on sign-in) and keep verifying.
4. Only once every stored hash is peppered, set
   `IMPRESSPRESS_PASSWORD_PEPPER_REQUIRED=true` (native env, or on Cloudflare
   `[cloudflare.password_hasher].pepper_required = true`); an unpeppered hash is
   then refused, so nobody who can write the credential table can plant a
   hash of a password they know. Check first — any user this lists is locked
   out once it is on (single quotes, so the shell leaves `$argon2id…` alone):

   ```sh
   # native SQLite
   sqlite3 data/impresspress.db 'SELECT user_id FROM wafer_run__auth__local_credentials WHERE password_hash NOT LIKE '"'"'$argon2id-hmac-sha256$%'"'"';'
   # Cloudflare D1
   npx wrangler d1 execute <database> --remote --command 'SELECT user_id FROM wafer_run__auth__local_credentials WHERE password_hash NOT LIKE '"'"'$argon2id-hmac-sha256$%'"'"';'
   ```

   Reset or recreate those accounts first. While `REQUIRED` is on, a sign-in
   to an account whose hash is still unpeppered answers 503 rather than a
   wrong-password 401 (and is logged as a configuration fault with its user
   id), so someone who knows such an account's password can tell it apart —
   one more reason to run the scan first. The value must be exactly `true`
   or `false`; anything else fails the boot (native) or every password
   operation (Cloudflare) with an error naming the variable.
5. To rotate: move the current key into
   `IMPRESSPRESS_PASSWORD_PEPPER_PREVIOUS_KEYS` (comma-separated, a secret of
   the password-hasher Worker on Cloudflare) and set a new current key. Old hashes keep verifying with the
   key they name, and a hash naming a key the deployment no longer holds
   fails sign-in with a 503. The boot log names each key by its id
   (`password pepper <id> (previous: [<id>, …])`); before dropping an old
   key, check no stored hash names its id:

   ```sh
   sqlite3 data/impresspress.db 'SELECT user_id FROM wafer_run__auth__local_credentials WHERE password_hash LIKE '"'"'%,pepper=<id>$%'"'"';'
   ```

   (the same query through `npx wrangler d1 execute … --command '…'` on D1).

A malformed key fails the boot the same way, naming the variable and never
printing the value.

### wafer-run e17debe4: D1 statement budget, argon2 ceiling, vector store

**What changes.**

- On Cloudflare, every request now tracks the D1 queries it sends against
  D1's per-invocation limit, and a `create_many` or `batch` that would not
  fit what the request has left is refused before it runs, as a 429 whose
  message gives the numbers, instead of failing part-way inside D1. The limit
  is the new Worker var `IMPRESSPRESS_D1_QUERIES_PER_INVOCATION`, default
  `1000`, which Workers Free and Paid both allow (see "Cloudflare: a new site
  deploys in one command" above). A value that is not a whole number from 5 (one more
  than the audit-row reservation below) to 1000, D1's maximum, fails every
  request with an error naming the var, however it was set. That 429 means the
  request did too much: retrying it does the same work and is refused again.
- Of that limit, each request holds 4 queries back for its own
  `request_logs` row, written after the response, so the row is written
  however much of the budget the request spent: a request's handlers can run
  996 of the default 1000. With `IMPRESSPRESS_REQUEST_LOG=off`
  nothing is held back. The row, and then any mail the request sends after
  its response (on everything the row left), run under that request's own
  budget. Before, a
  request wrote the rows and ran the deferred mail of whichever concurrent
  requests had finished before it, charged to its own budget, and lost them
  when that budget was spent.
- Native SQLite and Postgres have no such limit, and now take a
  `create_many` or `batch` of any size (the old fixed cap of 1000 is gone).
- A dev-sandbox data import (`seed/data.json`) is one transaction: an import
  that fails leaves every table, the users and their passwords included, as
  it was. Before, a failure after the users table was replaced could leave
  the instance with no account that can sign in.
- A stored argon2 password hash whose memory cost is above 46 MiB can no
  longer be used to sign in, on any target: it is refused before any
  derivation runs. Nothing
  impresspress writes comes near it (19 MiB native, 4 MiB Cloudflare); a hash
  imported from elsewhere at argon2-cffi's or RFC 9106's 64 MiB default is.
- A native build with `native-embedding` registers the `wafer-run/vector`
  store without loading an embedding model, so the vector store is there
  even when the model cannot be downloaded (embedding stays with
  `impresspress/fastembed`, which loads its model on first use).
- The browser registers an injected vector service and an injected
  embedding service each on its own. A build with `block-fastembed` refuses
  an injected embedding service, since `impresspress/fastembed` already
  serves embeddings there.

**Who has to act.** Nobody on Cloudflare: leave
`d1_queries_per_invocation` unset. An earlier version of this note told
Workers Free deploys to set it to `50`; that makes a fresh database's first
deploy fail, and is corrected in "Cloudflare: a new site deploys in one
command" above. The generated `wrangler.toml` writes the limit into `[vars]`
as `IMPRESSPRESS_D1_QUERIES_PER_INVOCATION` (`"1000"` when unset); a value
set through a `wrangler_overrides_path` file still wins, since overrides are
merged over the generated config. A value outside 5 to 1000 fails the build,
and the Worker refuses one set through an overrides file too. The budget
counts D1 queries only, not KV or R2 operations
(<https://developers.cloudflare.com/d1/platform/limits/>,
<https://developers.cloudflare.com/workers/platform/limits/#subrequests>). A user
whose password hash was imported from another system at more than 46 MiB:
reset the password. Nobody else.

### wafer-run 11723941: cut-off streams, Postgres settings, security headers

**What changes.**

- A response whose producer stops without finishing (it panicked, was
  cancelled, or returned early mid-body) is an error, not a success carrying a
  truncated body. On the native server that is a 500; on Cloudflare and in the
  browser, where the status is already sent when a streamed body fails, the
  body is aborted instead of ending cleanly, so a client sees a failed
  download rather than a short file that looks whole.
- A handler can no longer loosen a security header the site-wide
  `wafer-run/security-headers` step set; a stricter one is combined with it.
  A file download's sandboxing `Content-Security-Policy` is now sent beside
  the site policy (a browser enforces both) instead of replacing it.
- Postgres connects through sqlx 0.9, which reads two connection settings
  differently: a password taken from a `.pgpass` file (or the file
  `PGPASSFILE` names) is backslash-unescaped as libpq does, and an
  `options[key]=value` parameter in `IMPRESSPRESS_DB_URL` is escaped for you.
- Session tokens are signed over key-sorted claims. Tokens already issued
  keep verifying; nobody is signed out.

**Who has to act.** A Postgres deployment whose `.pgpass` password contains a
backslash: write each literal `\` as `\\`. One whose `IMPRESSPRESS_DB_URL`
hand-escapes an `options[...]` value (such as `my\ app`): write the plain
value, URL-encoded (`my%20app`). Nobody else.

### wafer-run 01239cf3: block config at Init, model cache, discovery

**What changes.**

- `WAFER_RUN__DATABASE__STRICT_SCHEMA` and the network limits
  (`WAFER_RUN__NETWORK__MAX_RESPONSE_BYTES`, `…__CONNECT_TIMEOUT_SECS`,
  `…__READ_TIMEOUT_SECS`, `…__REQUEST_TIMEOUT_SECS`,
  `…__STREAM_TIMEOUT_SECS`) reach their blocks through each block's start
  config. Native resolves them from the process environment beneath the
  variables table; Cloudflare reads STRICT_SCHEMA from the Worker var as
  before. An invalid network limit now fails the `wafer-run/network` block's
  start, naming the key, instead of the process's boot.
- `/openapi.json` and `/.well-known/agent.json` leave out every block the
  admin toggle has turned off, as `/b/webmcp/manifest.json` already did, and
  an operation the router gates above its declared level carries
  `bearerAuth`.
- The runtime refuses to start when two endpoints declare the same method on
  the same route (`{id}` and `{item_id}` count as the same).
- `WAFER_RUN__FASTEMBED__CACHE_DIR` is no longer read. A build with
  `block-fastembed` or `native-embedding` passes the model cache directory to
  `ImpresspressBuilder::model_cache_dir` and is refused without one. The
  `impresspress` binary passes `IMPRESSPRESS_MODEL_CACHE_DIR` (default
  `data/models`, the directory the old variable defaulted to); both features
  are off in its default build.

**Who has to act.** A native deployment built with `native-embedding` that
set `WAFER_RUN__FASTEMBED__CACHE_DIR`: set `IMPRESSPRESS_MODEL_CACHE_DIR` to
the same directory. A consumer calling `ImpresspressBuilder` with either
feature: call `.model_cache_dir(...)`. Nobody else: the other variables keep
their names and meaning.

### Config: a read that fails is an error, not the default

**What changes.** Every config read in impresspress now fails closed. A key
that is not set still reads as its declared default, but a read the config
block refuses (a missing grant, an undecodable request, a config block that is
not registered) is answered as the error it is: the page or API call shows the
403 or 500 instead of rendering with a default. Before, such a failure read as
the default — an unreadable `ALLOW_SIGNUP` read as "on", an unreadable
`__IMPRESSPRESS_RUNTIME_KIND__` read as "server", an unreadable Mailgun key as
"email not configured".

**Who has to act.** Nobody on a working deployment: every impresspress block
may read the keys it reads. A 403 that names no obvious cause after the upgrade
is a grant that was missing all along; the log line carries the key.

### Security headers: the CSP override is checked where it is saved

**What changes.** `WAFER_RUN_SHARED__CSP_DIRECTIVES` must be a policy the
security-headers block takes whole. The admin Variables page, the settings
forms and `config.set` refuse a value holding a character outside visible
ASCII (a smart quote or NBSP pasted from a document), a host wildcard or an
`http://` host in a script directive (`script-src https://*.js.stripe.com`
included — name `https://js.stripe.com`), a `report-uri` off this site,
`frame-ancestors`, or a repeated directive. The same check runs when a boot
seeds the variables table from the process environment: a value that fails it
is not stored, and the boot logs it at ERROR.

**Who has to act.** Check the stored value and the process environment before
upgrading: a character outside visible ASCII now fails the security-headers
block's start, so the runtime does not boot until it is corrected. Wildcards, `http://` script hosts and
off-site `report-uri` are no longer sent; the start logs each one as
`CSP config refused`. The shipped default is unaffected.

### Runtime: a missing block answers 501, and `requires` is checked at start

**What changes.** A request dispatched to a block that is not registered
answers 501 (`Unimplemented`) instead of 404, and so does an `OPTIONS`,
`TRACE` or `CONNECT` request routed to a block that serves only
`http-handler@v1` actions (it was a 400). A deployment whose blocks `require` a
block it does not register now refuses to start, naming both. impresspress's
own blocks list their soft dependencies separately, so no default build is
affected; a consumer's own block may need its `requires` corrected.

### LLM providers: a `/` in a provider name is refused

A provider's name is its backend id, and the llm service refuses a backend id
holding a `/`. Creating or renaming a provider to such a name is refused; a
stored provider row with one is skipped at load with a warning naming it.
Rename it to use the provider.

### Native: request bodies stream to the handler

The native listener now streams a request body instead of reading it whole
first. The 10 MiB cap and the body read timeout still apply; the timeout now
also counts the time the handler takes between reads. An upload that fails
part-way (dropped connection, cap, timeout) stores nothing, and the client gets
413, 408 or 400.

### CORS: no `Access-Control-Allow-Origin` without `Origin`

With `WAFER_RUN_SHARED__CORS_ALLOWED_ORIGINS` set, a request that carries no
`Origin` no longer gets the configured list back as
`Access-Control-Allow-Origin`, and every response carries `Vary: Origin`.

### Rate limits: counters are stamped in RFC 3339

**What changes.** The rate-counter rows (`wafer_run__auth__rate_limits`,
written by the Cloudflare rate limiter) now record
`created_at` / `updated_at` as RFC 3339 text on SQLite/D1, and the tickets
retention prune compares them against an RFC 3339 cutoff. On Postgres the
prune's cutoff was refused by the `TIMESTAMPTZ` binding, so every retention
pass listed `rate-counters` among its errors; that step now runs.

**Your data.** Nothing to migrate. A row written before the upgrade keeps the
`YYYY-MM-DD HH:MM:SS` spelling until its key is hit again; the prune sorts
such a row below any cutoff on its own calendar date, so an idle counter can
be swept up to a day early (after 48 hours rather than 72). Sweeping only
resets a counter, and the built-in windows are a minute or an hour.

The comment in `auth/migrations/008_rate_limits.postgres.sql` still says the
timestamps come from SQL `CURRENT_TIMESTAMP`; they are now bound by the
executor. It is left as written: any edit to the auth migration SQL re-runs
the auth migrations and signs every user out.

### Rate limits: an IPv6 client is one /64

**What changes.** Every IP-keyed rate-limit bucket (login, signup, password
reset, verify, token refresh, OAuth start, auth mail, share downloads, signal
rooms, anonymous commerce, and the tickets per-identity limit) now charges an
IPv6 client per /64 network instead of per address, and an IPv4-mapped IPv6
address (`::ffff:a.b.c.d`) as the IPv4 address it carries. IPv4 clients are
unchanged: one address, one bucket.

**Why.** A host picks the low 64 bits of its IPv6 address itself, so a client
keyed per address could rotate within its /64 and never reach a limit.

**Who has to act.** Nobody, usually. A /64 is one subscriber's network, so
users sharing one only share a budget when they are already one site (a home,
an office LAN); a deployment where that is common raises the category by key,
as for a shared IPv4 egress (`WAFER_RUN_SHARED__RATE_LIMIT_{NAME}`).

**Your data.** Nothing to migrate. Buckets are windowed counters, and no
request produces a full-address IPv6 key any more, so the old ones are never
charged again. On native they live in memory and go with the next restart or
eviction. On Cloudflare they are rows in the `rate_limits` table; a row is read
only by its own key, so a leftover one is harmless — it limits nobody and costs
only its storage. Nothing deletes them automatically: the tickets retention
prune (`POST /b/tickets/api/admin/retention/prune`, or a `tickets.maintenance`
message if the deployment schedules one) removes them along with every other
stale counter, and without it they stay.

### Dev sandbox: a hyphenated block spells its collections with `_`

**What changes.** A sandbox block whose name has a hyphen (`blocks/my-shop`,
registered as `site/my-shop`) now claims `site__my_shop__*` collections and
`SITE__MY_SHOP__*` config keys — the spelling the runtime derives for the block
id. The hyphenated spelling (`site__my-shop__*`, `SITE__MY-SHOP__*`) is refused
at staging with `cap-collection` / `cap-config`, and so is any collection name
that is not lowercase letters, digits and `_`. Blocks without a hyphen in their
name (`site__newsletter__*`) are unaffected.

**Why.** The database strips a hyphen from a table name before building SQL.
A `site/my-shop` block that claimed `site__my-shop__notes` had its table created
under that name, but every `create`, `list`, `update` and `count` ran against
`site__myshop__notes`: with no such table they failed or came back empty, and
when a block named `myshop` existed they read and wrote *its* rows.

**Your data.** Nothing to migrate. A hyphenated block's own tables
(`site__my-shop__*`) were created empty and no write could reach them, so no
rows live there. Rows in `site__myshop__*` belong to a block named `myshop`
(where one exists) and stay with it; a row a hyphenated block wrote there
carries no record of which block wrote it and is not moved. Once rebuilt, the
hyphenated block creates its table under the new name on `init` and starts
empty. The empty `site__my-shop__*` tables are left behind unused.

**An already-active hyphenated block stops working until it is rebuilt.** The
database now refuses a table name that is not lowercase letters, digits and `_`
instead of stripping it, so every request of a block accepted under the old
spelling that touches a `site__my-shop__*` collection fails with
`InvalidArgument` — it no longer reaches `site__myshop__*`, another block's
table. A block that also declares a `SITE__MY-SHOP__*` config key is refused
when the runtime registers it, so its generation does not load at all: on the
next boot the browser console reports that the dev sandbox could not load its
active blocks, and the site's pages keep serving without them.

**What to do.** Rename the block's collections and config keys to the `_`
spelling (a freshly scaffolded block already uses it), rebuild, stage. A seed
bundle exported with a hyphenated block under the old spelling is refused on
import with the same diagnostic; re-export it after the rebuild.

### Dev sandbox: a block never sees the session cookie, and cannot set one

**What changes.** A sandbox block no longer receives the request's `Cookie`,
`Authorization` or `Proxy-Authorization` header. The service worker attaches
the admin's session cookie to every same-origin request, `/b/<name>/` included,
and until now it reached the block. A block also cannot set `Set-Cookie`,
`Location`, `Refresh`, `Clear-Site-Data`, CORS, HSTS, `X-Frame-Options` or CSP
headers on any answer, an error included; before, an error's headers were
passed through as the block set them. The block reference (`reference.md`,
"Headers") states the contract.

**What to do.** Nothing, unless a block read the cookie or authorization
header: it must identify the caller through `request.user_id` /
`request.roles`, which the host fills in. Staging already refused a block that
declared either header (`cap-headers`), so no accepted block was granted them.

### Dev sandbox: blocks built before this release must be recompiled (guest ABI 2)

**What changes.** The host refuses a list query with a page size of `0`, and the
vendored `src/wafer_guest.rs` sent exactly that for every `db::list` without a
`.limit(n)`. The module now leaves the limit out (every matching row), which
changes what a compiled block sends, so its `WAFER_GUEST_VERSION` is 2.

**Your blocks.** A block compiled against version 1 keeps serving, but each of
its `db::list` calls without a `.limit(n)` now fails with `InvalidArgument`.
Staging refuses a version-1 build with the `wafer-guest-version` diagnostic,
and so does importing a seed bundle that carries one.

**What to do.** For each block: replace `blocks/<name>/src/wafer_guest.rs` with
the current module — `GET /b/dev/api/reference` returns it as
`wafer_guest_module`, and a newly scaffolded block has it — then compile and
stage again. The block's own files are unchanged. Re-export any seed bundle
afterwards.

### Native: proxy and connection settings for the HTTP listener

**What changes.** Six infrastructure variables configure the native HTTP
listener; each is unset by default, which keeps the listener's own default.

- `IMPRESSPRESS_TRUSTED_PROXIES` — comma-separated IPs or CIDR ranges of
  reverse proxies whose `X-Forwarded-For` is honored. Unset trusts none, so
  behind a proxy every client shares the proxy's address, and one rate-limit
  bucket. Set it when you run behind one.
- `IMPRESSPRESS_HEADER_READ_TIMEOUT_SECS`, `IMPRESSPRESS_BODY_READ_TIMEOUT_SECS`,
  `IMPRESSPRESS_WRITE_TIMEOUT_SECS` — how long a client may take to send its
  headers, to send its body (a slower body is answered `408`), and to read the
  response. Slow clients that used to hold a connection open indefinitely are
  now cut off; raise these if yours are legitimately slow.
- `IMPRESSPRESS_MAX_CONNECTIONS` — connections open at once; further clients
  wait in the accept backlog.
- `IMPRESSPRESS_SHUTDOWN_GRACE_SECS` — how long a stopping server lets open
  requests finish. Shutdown may now wait up to this long.

A value the listener cannot use stops the server at boot, with a message naming
the setting.

### Cloudflare: outbound redirects are followed, hop by hop

**What changes.** An outbound request from a block used to fail when the far
end answered with a redirect. The Worker now hands the redirect back to the
network service, which follows it as a new request: each hop is checked
against the calling block's network grant and the internal-address filter,
so a redirect to an address the block may not reach, or to an internal one,
is refused before it is contacted. The browser still refuses redirects.

### Logs: a block's log line names the block that wrote it

**What changes.** A line a block writes through the logger now leads with the
registered name of the block that wrote it, and carries the block's text as a
quoted `msg` field: `caller=site/shop msg="order placed" order=o_1`. Control
characters in the text are escaped, so one call is one line. Native logs
carry the same fields; a JSON log collector finds the block's text under
`msg` rather than `message`. Adjust any log query that matched on it.

### Vector: index names are lowercase, and existing mixed-case indexes move at start

**What changes.** An index name is 1 to 33 characters of lowercase letters,
digits and `_`. The database layer now refuses any other table name rather
than rewriting it, and an index's tables are named after it
(`impresspress__vector__{name}_meta` and its siblings), so an uppercase name
(`Docs`) can no longer be created, and a name longer than 33 characters would
give a table name longer than the 63 bytes PostgreSQL keeps.

**Your data.** Nothing to run. Each time the vector block starts, it moves every
index whose name has an uppercase letter to its lowercase name — the index's
tables, its entries and its keyword search (`vector.rename_index`, one
transaction per index on native SQLite and in the browser) — whether the
registry names it or only the vector store does. `Docs` becomes `docs` and
answers under that name with everything it held, and its registry row is
renamed with it. The move is logged at info level, and a start that finds
nothing to move does nothing.

**Two registry rows that differ only by case** (`Docs` beside `docs`) name one
index: SQLite compares table names without case, so the data exists once.
Rows that agree are duplicates, and the `Docs` row is removed. Rows that
disagree only on keyword search are told apart by the index itself, and the
row that matches it is kept under the lowercase name. Rows that disagree on
the model or the dimensions cannot be told apart — no vector store reports
them — so both rows are left, the index is not moved, and an error log names
both: delete the row that is wrong, and the next start moves the index. An
index that cannot be moved for any other reason is named in an error log and
tried again on the next start; the rest of the vector block keeps working.

**Until the move has run**, an index whose name has an uppercase letter makes
the vector admin's index list answer an error and cannot be opened, queried
or deleted. An index named with more than 33 characters cannot be opened,
queried or deleted under this release at all: delete it before upgrading.

### Config: your `.env` applies again, and one boot decides the ties

**What changes.** A `WAFER_RUN_SHARED__*` / `{ORG}__{BLOCK}__*` environment
variable used to be silently ignored from the second boot onward. It was seeded
with `INSERT OR IGNORE`, so it only ever landed on a virgin database; afterwards
a row existed, the insert was discarded, and nothing said so. From this release
the environment sets a key on every boot — **unless an admin has edited that key
through the admin UI**, in which case the stored row wins permanently and the
boot log says which key and why.

**The one-time upgrade boot.** Rows written before this release carry no record
of who wrote them, so an admin's settings-form edit and an earlier boot's env
seed look identical. On the first boot after upgrading, any key whose stored
value **differs** from a value you export is **kept as it is**, pinned, and
named in a WARN. Nothing is reverted, and no export is lost — it simply does not
apply until you say so. A key whose stored value already matches its export is
left alone silently.

**What to do.** Read the boot log. For each `NO EFFECT` line, decide which value
you want:

- *the environment's* — open **Admin → Settings → Variables**, find the key, and
  use **Reset to environment** (or `POST
  /b/admin/api/settings/{key}/reset-to-environment`), then restart. The export
  applies from then on, with no further intervention.
- *the stored one* — do nothing. The line stops once you remove the export.

**If the answer is "the environment, for all of them".** The upgrade boot pins
exactly the keys you had configured, so several keys is the ordinary case rather
than the rare one. **Reset all keys pinned at upgrade**, at the top of Admin →
Settings → Variables, releases every one of them in a single confirm, then one
restart. It is scoped to the keys *that boot* pinned and cannot touch a key an
admin edited in the UI — those stay, whether the edit was before the upgrade or
after it. The button only appears while at least one such key is pinned, and only
on a deployment that boots from a process environment, so on Cloudflare and in
the browser it is never shown.

After that boot the rule is simply: the environment sets a key until an admin
edits it in the UI.

**What this does not protect.** The upgrade boot can only resolve conflicts it
can actually see. This applies to changes made on a **block settings page**
(Products, Legal pages, User portal, Email, Auth) — a change made on **Admin →
Variables** records who made it and is protected outright, whatever your
deployment config says. Such a settings-page change is kept **only if, on that
boot, your deployment config exported that same key with a non-empty value that
differed from the stored one.** If any of those is not
true — the key is not in your config, or it is set to an empty value, or it is
set to the value already stored — the boot passes over it silently and the key
is ordinary from then on. **A later change to your deployment config then wins,
including over that pre-upgrade UI change.**

Concretely: you disabled OAuth in the UI, your compose file said nothing about
it at upgrade time, and months later you add
`WAFER_RUN_SHARED__ENABLE_OAUTH=true`. OAuth comes back on. Same for
`WAFER_RUN_SHARED__ALLOW_SIGNUP`.

The remedy is one action, and it is worth doing now rather than later:
**re-apply in the admin UI any setting you care about that you changed there
before upgrading.** That records it for good — an admin edit made *after* the
upgrade is always safe, whatever your deployment config says.

**Keys worth checking first**, because they decide who can get in:

- `WAFER_RUN_SHARED__AUTH__BOOTSTRAP_ADMIN_EMAIL` — **every** signup with this
  address is granted admin, not just the first one (`auth::initial_role_for`),
  so a stale value here is a standing back door. Clear it once you have your
  admin account.
- `WAFER_RUN_SHARED__AUTH__BOOTSTRAP_ADMIN_PASSWORD` and
  `..._BOOTSTRAP_ADMIN_TOKEN` — plaintext credentials; a cleared one now stays
  cleared across a restart even with the export still present.
- `WAFER_RUN_SHARED__ALLOW_SIGNUP` and `WAFER_RUN_SHARED__ENABLE_OAUTH` — if you
  turned either off in the UI during an incident, it stays off.
- `WAFER_RUN_SHARED__ENVIRONMENT` — if this deployment first booted as
  `development` and your deployment config later said `production`, the stored
  value is the **less secure** one (session cookies without `Secure`, and a
  wildcard `Access-Control-Allow-Origin` on discovery documents). Reset this key
  to the environment before anything else.
- Block-scoped credentials such as `IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY` —
  if you rotated one in the UI and your deployment config still carries the old
  one, the rotated value is what is kept. Neither value is printed in the log;
  compare the stored one where you issued it.

**Break glass — if a pin locks you out.** Every route above needs a working admin
login, and the keys most able to deny you one are pinnable. The case to know
about: a deployment with no admin user yet, whose stored
`WAFER_RUN_SHARED__AUTH__BOOTSTRAP_ADMIN_EMAIL` / `..._PASSWORD` are wrong, and
whose corrected values are in the deployment config. The upgrade boot keeps the
stored pair, and `auth::bootstrap` creates the first admin from **those** — so
signing in to fix it needs the credentials you were replacing.

Release a key without logging in by writing the released marker directly in the
database (`impresspress__admin__variables`), then restarting:

```sql
UPDATE impresspress__admin__variables
   SET updated_by = 'released-to-environment'
 WHERE key = 'WAFER_RUN_SHARED__AUTH__BOOTSTRAP_ADMIN_EMAIL';
```

`updated_by` is the whole mechanism, and `released-to-environment` is exactly
what the **Reset to environment** button writes. **Do not blank the column
instead.** An empty `updated_by` means "nothing has ever claimed this row",
which is the state the one-time upgrade pass looks for — so on a deployment that
has not recorded that pass yet, blanking the column can get the key pinned
straight back on the next boot, with no UI to tell you. The sentinel above reads
as "the environment owns this" and is correct either way.

Only native deployments can be in this position — Cloudflare and the browser
never seed from a process environment, so nothing there is ever pinned against
one.

**No migration.** Nothing to opt into, and the transition runs once per database
whether or not you pass `--run-migrations`. Cloudflare and browser deployments
are unaffected: neither seeds from a process environment, so neither has a tie
to break, and the **Reset to environment** control does not render there.

### Routing: the `/api` prefix is gone (Cloudflare deployments only)

**What changes.** A Cloudflare deployment used to accept an `/api`-prefixed
copy of every route: the Worker adapter stripped the prefix before dispatch, so
`POST /api/b/storage/api/buckets/photos/objects` reached the same handler as
`POST /b/storage/api/buckets/photos/objects`. That stripping is removed, and
`/api/...` now falls through to the SPA like any other unclaimed path.

**Why.** The prefix only ever worked on Cloudflare. The site-main flow's router
matches the path as received — `/`, `/b/**`, `/health`, `/openapi.json`,
`/.well-known/agent.json`, everything else to `wafer-run/web` — so on the
native and browser transports an `/api/...` request was served the SPA, and the
pipeline's own `/api` strip (which ran after routing) could never see one.
Neither strip was segment-bounded either: `/apiary/hives` became `ary/hives` on
every transport, and `/api/api/x` reached `/x` on Cloudflare and `/api/x`
elsewhere. One transport honouring a prefix the others do not is a routing
difference between deployments of the same app, which is worse than not having
the prefix.

**Who is affected.** Only a Cloudflare deployment with a client that calls
`/api/...` by hand. Nothing in this repository or in the TypeScript SDK does —
the SDK's README states there is no `/api/*` surface, and every block route
already carries its own `/b/<block>/api/...` path, which is untouched.

**What to do.** Drop the `/api` prefix from any such caller: `/api/b/x` →
`/b/x`. There is no migration and no config toggle.

Routing alone does not bring it back, either: adding `{ "path": "/api/**",
"block": "impresspress/router" }` to the flow's routes hands the router a
`req.resource` of `/api/b/x`, and `routing::route_to_block` matches prefixes
like `/b/storage/` against that string, so every such request answers 404. A
consumer who genuinely needs the prefix has to strip it before the router sees
it — a flow step of their own ahead of `wafer-run/router` that rewrites
`req.resource` — which is the piece this release removes.

### Files: bucket names are unique (migration 002) — upgrade with `--run-migrations`

**What changes.** A storage bucket's name is also its folder name in the object
store, and the `buckets` table had no unique index on it. A second user could
therefore create a bucket under a name someone else already held — every
backend's `create_folder` is idempotent, so nothing refused it — and the row
they got granted them read, overwrite and delete access to the first owner's
objects. Bucket names are now unique, and creating one that is taken answers
`409` with "A bucket with the name "…" already exists. Choose a different
name."

**The repair.** Migration `002_bucket_name_unique` deletes duplicate bucket rows
before creating the index, keeping the **earliest** row for each name (by
`created_at`, `id` as the tie-break). That is the access the later rows should
never have had; the folder and its objects stay with the one remaining owner,
and the object-metadata rows of whoever else uploaded into it are left alone —
those blobs are real and still charged to whoever uploaded them.

**Review your collisions before upgrading.** That last sentence has a
user-visible edge: an object the losing user uploaded stays in the winner's
bucket, so they lose access to their own file while their quota keeps being
charged for its bytes. In the case this fixes — a takeover — that is the
correct outcome. If a collision turns out to be two people who each meant to
have their own bucket, sort it out *before* you run the migration: have the
later user download what they need, or rename their bucket (create a new one
and re-upload), because afterwards only the winner can reach the folder.

**Upgrade with `--run-migrations`.** Without it the index is not created, and
the code half alone does not close the hole: the refusal comes from the
database, so a duplicate name is admitted exactly as before. The only signal is
the generic `schema drift; redeploy with --run-migrations to apply` warning each
boot logs for the files block.

**To see what will be deleted**, list the collisions from the admin SQL
explorer before upgrading. This runs on every backend — the per-owner rows come
back one per line rather than through a backend-specific aggregate
(SQLite/D1 has `GROUP_CONCAT`, Postgres has `string_agg`, and neither has the
other):

```sql
SELECT b.name, b.created_by, b.created_at, b.id
FROM impresspress__files__buckets AS b
WHERE EXISTS (
    SELECT 1 FROM impresspress__files__buckets AS other
    WHERE other.name = b.name AND other.id <> b.id
)
ORDER BY b.name, b.created_at, b.id;
```

The first row of each `name` group is the one that survives; the rest are what
the migration deletes.

### Files: uploaded objects download instead of rendering

**What changes.** `GET /b/storage/api/buckets/{bucket}/objects/{key}` and the
public share link `GET /b/storage/direct/{token}` serve bytes and a content type
an uploader chose, from the application's own origin. They now send
`X-Content-Type-Options: nosniff` on every object, and `Content-Disposition:
inline` only for types that cannot carry script — images (not SVG), audio,
video, PDF and plain text. Everything else, `text/html` and `image/svg+xml`
included, is served as an `attachment` with a sandbox `Content-Security-Policy`.

Image and PDF previews are unaffected. What changes for a user is that opening
an uploaded `.html` or `.svg` link downloads the file rather than displaying it.
**No migration.**

### Files: legacy share links (migration 003) — upgrade with `--run-migrations`

A public share link's token used to be a JWT, and the link handler verified
it before it read the share row — so a link stopped working when its JWT
aged out, whatever expiry its owner had picked. From this release a token
is opaque entropy addressing one row, and the row's `expires_at` is the
only thing that ends a link.

That matters on upgrade because the share dialog used to post its expiry
under a field name the server did not read, so almost every share created
through the UI has **no expiry on the row at all**. Reading those tokens as
opaque strings would make every one of those links live again —
permanently, and pointing at files whose owners believe the link is long
gone.

**The JWT's life changed once, so the repair has two arms.** Until
2026-05-14 the share JWT was signed for 365 days; SEC-055 shortened it to
30. Migration `003_legacy_share_token_expiry` gives a legacy row (a
JWT-shaped token, no expiry) 365 days from when it was minted if it
predates that change and 30 days if it does not — reproducing the lifetime
its token actually imposed. A link that is dead today stays dead; a link
minted in the one-year era and still working keeps working to its original
date.

The cutoff is the instant the code changed, not the instant your deployment
adopted it. If you upgraded past 2026-05-14 some time later, shares minted
in that gap really ran the one-year code and will be given 30 days here —
i.e. they expire. That arm errs toward "a dead link stays dead"; re-share
the file if one of them mattered.

**Upgrade with `--run-migrations`.** The code half refuses to serve a share
row that records no end, so without the migration every historical share
link answers "Share link is unavailable" — an outage for every link your
users have already sent, not a leak. The migration is what gives each of
them its correct remaining life back. Until it runs, the only other signal
is the generic `schema drift; redeploy with --run-migrations to apply`
warning each boot logs for the files block.

### Files: every share link now expires

**What changes.** A public share link is an unauthenticated bearer
credential: it is pasted into a chat or a document and never looked at
again. From this release every one of them has an end. The share dialog no
longer offers "Never", and a `POST /b/cloudstorage/shares` that names no
`expires_in_hours` gets the configured maximum rather than an unexpiring
link. A share row that somehow carries no expiry — an import, a restore, a
hand-written row — is refused by the public link rather than served.

**The maximum is yours to set.** `IMPRESSPRESS__FILES__MAX_SHARE_EXPIRY_HOURS`
(Admin → Settings → Variables) defaults to `8760` — one year, the ceiling
explicitly-supplied expiries were already held to. It is read per request,
so raising it for a deployment that genuinely needs long-lived public links
takes effect without a redeploy, and lowering it binds the next share
immediately. Links already issued keep the expiry they were given. An
expiry longer than the maximum is refused with a 400, as before.

**Who is affected.** Anyone who picked "Never" in the share dialog: those
links now get the configured maximum instead. Existing links are unchanged
by this — what bounds them is the repair above, which reproduces the
lifetime their token already had.

### Files: the file-count quota is per bucket, both caps are exact, and `reset_period_days` is gone

**The file-count cap is per bucket, as its name says.** `max_files_per_bucket`
(default `10000`, shown as "Max Files/Bucket" on the storage admin's Quotas tab)
used to be checked against a user's files summed over **all** their buckets,
so filling one bucket blocked uploads everywhere. It is now checked against
the files that user holds in the bucket being uploaded to. This **loosens**
enforcement: a user with several buckets can now store up to
`max_files_per_bucket` files in each. Nothing caps how many files a user holds
across buckets: `max_storage_bytes` is still over everything a user stores,
but it counts bytes, not files, so it bounds that total only by size. No
migration is involved.

**Both caps are exact.** An upload is held to `max_storage_bytes` and
`max_files_per_bucket` by the write that reserves it, as one atomic step, so
uploads running at the same time can no longer each pass against the same
usage and together exceed a cap. A refused upload answers as before: a 400
with `Storage quota exceeded` or `File count limit reached for this bucket
(max N)`. One case still leaves a user over `max_storage_bytes`: an admin's
upload replaces one of their objects, they use the room that frees, and the
admin's upload then fails — their object is put back, and their next upload
is refused until they are under the cap again.

**`reset_period_days` is removed — a breaking change for API clients.** It
was stored and published, but nothing ever enforced a reset period. It is no
longer in `GET /b/cloudstorage/quota`, `GET /b/cloudstorage/admin/quotas` or
the `PATCH /b/cloudstorage/admin/quotas/{id}` response, and a PATCH that names
it is refused with a 400 (`Unknown quota field`) instead of being stored. The
SDK's `getQuota()` type no longer has it. Drop the field from anything that
sends or reads it. The database column is left in place, unused; there is
nothing to do about it.

### Files: an upload's claim on a key is exact (migration 004) — upgrade with `--run-migrations`

**What changes.** An upload claims its `(bucket, key)` row before it stores
the bytes, and a replacement takes over the existing row. That take-over was
conditional on the row's `updated_at` timestamp, so two uploads of one key
whose stamps landed in the same millisecond (or on two Workers isolates whose
clocks disagreed) could both take the row: one row describing one upload while
the blob held the other. Migration `004_object_claim_id` adds a nullable
`claim_id` column to `impresspress__files__objects`; each reservation writes a
random token there, and the take-over, the completion and the rollback of a
failed upload each act only on the reservation the row still carries.
While a rollout is part-way, isolates still running the previous release take
rows over on the old `updated_at` check and leave `claim_id` unchanged, so the
token check cannot catch those take-overs; the gap is transient and closes once
every isolate runs this release.

**When the object is deleted mid-upload.** An upload whose object or bucket is
deleted while its bytes are being stored now answers `409` saying so (not that
another upload took the key), and the bytes it stored are deleted rather than
left recorded and charged nowhere.

**A retry after "Upload stored but could not be recorded" says what holds the
key.** Such an upload leaves its reservation in place for up to an hour, and
the user's retry used to be told "Another upload of this key is in progress".
It now answers `409` with "Your earlier upload of this key, started at …, has
not been recorded", saying when the key is released. The key is still held
for that hour: the row cannot tell a failed upload from one still running.

**Re-running the files migrations is safe for live data.** Adding 004 changes
the hash of the files block's migration set, so the next `--run-migrations`
boot (on Cloudflare, the first deploy of this release) re-runs **all** of them, 001 onwards, over
the existing tables — the way a new auth migration re-runs the auth set. For
files nothing is dropped and nothing live changes: 001 is `CREATE … IF NOT
EXISTS` throughout, 002's duplicate-bucket `DELETE` finds nothing once its
unique index exists, 003 only touches legacy share rows with no expiry, which
its first run already dated, and 004 is an `ADD COLUMN` that PostgreSQL skips
(`IF NOT EXISTS`) and SQLite/D1 answers with a duplicate-column error the
runner treats as done. Share links, buckets, stored objects and uploads in
flight come through byte-identical — pinned on SQLite by `replay_tests` and on
PostgreSQL by the CI step that replays the set over seeded rows. Existing rows
keep a `NULL` `claim_id`, which a take-over matches as it matches a token;
nothing is backfilled, and nobody is signed out.

**Without the migration.** Cloudflare deploys always run it. A native
deployment that skips `--run-migrations` logs the generic `schema drift;
redeploy with --run-migrations to apply` warning for the files block on each
boot. With strict schema off (the default there), an upload of a new key still
works and adds the column. Replacing an object answers `500` until the column
exists — the take-over checks the row's `claim_id`, and a condition never adds
a column — so run the migration rather than rely on a first upload. If you have turned
`WAFER_RUN__DATABASE__STRICT_SCHEMA` **on**, every upload fails on the missing
column until it runs.

### Files: each upload stores its bytes under a key of its own (migration 005) — upgrade with `--run-migrations`

**What was wrong.** Every upload wrote its bytes at the object key, so every
upload of one key wrote the same blob. The claim token (migration 004) kept the
metadata row exact, but not the bytes: an upload whose reservation outlived its
hour and was taken over could still store its bytes after the upload that took
over had finished — and overwrite them. That upload's row, complete and
describing its own upload, then served the first upload's content to everyone
who can read the file.

**What changes.** Each upload stores its bytes under a storage key derived from
the object key and its reservation (`reports/{claim}~q3.pdf` for
`reports/q3.pdf`), and migration `005_object_blob_key` adds a nullable
`blob_key` column to `impresspress__files__objects` naming the blob the row
serves. Downloads, share links and `GET /b/storage/api/buckets/{name}/objects`
resolve an object's bytes through its row. A replacement deletes the blob it
supersedes only after the row points at the new one; an upload that lost its
key, or whose object was deleted, deletes only its own blob. The hourly-TTL
sweep of abandoned uploads now deletes their blobs as well as their rows.

**Existing objects.** Nothing is copied or rewritten. Rows from before 005 keep
a `NULL` `blob_key`, which means their bytes are at the object key — where they
are — and they are served from there until a replacement supersedes them.

**Visible differences.**
- The object listing reads the metadata rows instead of listing storage. It
  names the same object keys, now includes uploads still in flight (as the
  object browser page already did), and its `prefix` filter follows the
  database's `LIKE`: case-insensitive for ASCII on SQLite and D1,
  case-sensitive on PostgreSQL.
- An upload that loses its key to another upload answers `409` "Another upload
  now holds this key, so this upload was not recorded; retry" (it used to say
  it "took too long", which was false when the object had been deleted and the
  key claimed again).
- A blob in storage that no row names is no longer downloadable by its key.
  Such blobs were already charged to nobody and absent from the object browser.

**Re-running the files migrations is safe for live data.** Adding 005 changes
the hash of the files block's migration set, so the next `--run-migrations`
boot (on Cloudflare, the first deploy of this release) re-runs **all** of them,
001 onwards, over the existing tables. As for 004, nothing is dropped and no
live row changes: 005 is an `ADD COLUMN` that PostgreSQL skips (`IF NOT
EXISTS`) and SQLite/D1 answer with a duplicate-column error the runner treats
as done. Pinned on SQLite by `replay_tests` and on PostgreSQL by the CI step
that replays the set over seeded rows. Nobody is signed out; files migrations
touch no auth table.

**Without the migration, and during a rollout.** As for 004: a native
deployment with strict schema off adds the column on the first upload, one with
`WAFER_RUN__DATABASE__STRICT_SCHEMA` on fails every upload until 005 runs, and
Cloudflare deploys always run it. While a rollout is part-way, an isolate still
on the previous release writes a replacement's bytes at the object key and
leaves `blob_key` as it was, so that row keeps serving the blob it named before
rather than the replacement, until the next upload of the key. The bytes that
isolate wrote at the object key are deleted when the object is next replaced,
deleted or swept; the gap closes once every isolate runs this release.

**Blobs that are logged, not reclaimed.** An upload whose bytes were stored but
whose row could not be recorded, and whose reservation another upload has since
taken over, leaves a blob no row names; the sweep cannot find it without
listing storage. It is logged at error level ("upload stored but not recorded")
with its blob key, as is any blob whose delete fails after its row is gone.

### Products: `PLATFORM_COUNTRY` no longer defaults to `US` — set it if you ship

**What changes.** `IMPRESSPRESS__PRODUCTS__PLATFORM_COUNTRY` now has one
default, and it is the empty one its setting has always declared. Checkout and
Payment Links used to default it to `US`, and to fall back to `US` for a value
they could not read, while seller onboarding defaulted it to empty and refused
an unreadable value. There is now one reader, and blank means "not configured"
everywhere.

**Who is affected.** Only a deployment that (a) has never set
`IMPRESSPRESS__PRODUCTS__PLATFORM_COUNTRY`, and (b) sells an offer whose
"collect shipping address" is on and whose "allowed shipping countries" list is
empty. Until now that combination silently produced a Checkout that would
accept a United States address and nothing else — including for a platform
that is not in the United States. It now refuses the checkout with
`this offer collects a shipping address but names no allowed countries`, and
the refusal is recorded against the order so it shows up in the seller
dashboard's recent failures.

Offers that list their own allowed shipping countries are unaffected; that
list always won and still does. Offers that do not collect a shipping address
are unaffected. Seller onboarding is unaffected: it already treated blank as
"no country" and let Stripe infer it.

**What to set.** In Admin → Settings → Products, set **Platform Country** to
your platform's two-letter country code (for example `NZ`), or list the
countries you ship to on each offer. A value that is not two ASCII letters is
now an error rather than a silent `US`, so fix any typo there at the same time.

Related, and not behaviour-changing for a valid configuration:
`IMPRESSPRESS__PRODUCTS__SELLER_APPLICATION_FEE_BPS` is now refused rather than
read as `0` when it is not a whole number of basis points between 0 and 10000.
A deployment whose fee is currently unreadable has been taking **no** platform
fee on connected-account sales; after this release those sales refuse until the
value is corrected.

### Products: every seller pays the current platform fee — check your sellers before upgrading

**What changes.** A seller used to keep the platform application fee
(`IMPRESSPRESS__PRODUCTS__SELLER_APPLICATION_FEE_BPS`) that was in force the
day they started Stripe onboarding. It was stored on their seller account and
nothing ever changed it. A seller onboarded while the fee was `0` was the one
exception: they were charged whatever the platform fee was at the time of each
sale, while their seller pages showed `0.00%`. From this release there is one
fee. Every seller's **new** Checkout Sessions and **newly created** Payment
Links carry the current platform fee, and every seller page, the seller API and
the admin seller pages show that same number. Per-seller rates are not
supported.

**Who is affected.** Sellers onboarded at a non-zero fee that differs from
today's platform fee. They were charged their onboarding rate. After the
upgrade they are charged the current rate. Sellers onboarded at `0`, or at
exactly today's fee, are charged what they were charged before; only the fee
their pages show changes.

**What does not change.** Anything Stripe already holds keeps the fee it was
created with. An existing Payment Link is reused as it is, because its fee is
not part of what identifies it. An existing subscription renews with the
`application_fee_percent` it was created with. Orders already placed are not
touched.

**Find the affected sellers before upgrading.** The stored per-seller fee is
still in the table (it is no longer read), so this read-only query works from
the admin SQL explorer on SQLite, Cloudflare D1 and PostgreSQL alike. Replace
`500` with your current `SELLER_APPLICATION_FEE_BPS`:

```sql
SELECT id, user_id, status, fee_basis_points
FROM impresspress__products__seller_accounts
WHERE fee_basis_points <> 0
  AND fee_basis_points <> 500
ORDER BY fee_basis_points, user_id;
```

Every row returned is a seller whose new checkouts and new Payment Links will
charge a different fee after the upgrade. `fee_basis_points` is the rate they
pay today.

**If you need to keep an old rate.** Set
`IMPRESSPRESS__PRODUCTS__SELLER_APPLICATION_FEE_BPS` to that rate before
upgrading. It then applies to every seller, since there is no per-seller
override. No migration is involved and no flag is needed.

### Products: `deleted_at` normalization (migration 020) — upgrade with `--run-migrations`

Product deletion is a soft delete, and `deleted_at` now carries a strict
two-value invariant: SQL NULL for a live product, an RFC3339 stamp for a
deleted one. The empty string is neither, and it now reads as **deleted**
everywhere — the public catalog, the storefront, the admin product list and
the per-seller product cap.

Earlier releases disagreed with themselves about `''`: the customer-facing
paths tested `!is_null && != ""`, so an empty string meant *live*, while the
list reads used `deleted_at IS NULL`. And `''` was reachable — until the
product handlers began refusing bodies that name an internally-owned column,
every create/update path forwarded the request body verbatim, so a client
sending `"deleted_at": ""` produced such a row.

Migration `020_normalize_blank_deleted_at` repairs those rows back to NULL.
**Upgrade with `--run-migrations`.** Without it the code half lands alone and
any affected product drops out of the catalog and the storefront with no admin
action; the only signal is the generic `schema drift; redeploy with
--run-migrations to apply` warning each boot logs for the products block.

### Products API: a canceled platform subscription reads `canceled` (migration 022) — upgrade with `--run-migrations`

**Wire change.** `GET /b/products/subscription` publishes the caller's
platform subscription, and a subscription ended by Stripe's
`customer.subscription.deleted` reported its status in the British spelling:

- before: `{"subscription": {"status": "cancelled", ...}}`
- after: `{"subscription": {"status": "canceled", ...}}`

`canceled` is Stripe's own spelling, and the one every other subscription
status in the products block already used — including an order's
`subscription_status`. No other value changes. The field is now described as an
enum in the OpenAPI document (`""`, `incomplete`, `incomplete_expired`,
`trialing`, `active`, `past_due`, `unpaid`, `paused`, `canceled`), so the
generated TypeScript type narrows from `string` to that union. A client that
compares against `"cancelled"` must compare against `"canceled"` instead.

Migration `022_canonical_subscription_status` rewrites the stored rows, so a
subscription canceled before the upgrade reads `canceled` too. **Upgrade with
`--run-migrations`.** The code no longer accepts the old spelling: until 022
runs, `GET /b/products/subscription` answers 500 for a user whose row still
holds `cancelled`, and a `customer.subscription.updated` or
`invoice.payment_failed` delivery for that subscription fails and is retried by
Stripe rather than applied. The migration only rewrites that
one value, so re-running it — the block replays its whole set whenever any of
its migrations changes — is harmless.

### Products API: internally-owned columns are now refused

The four product create/update endpoints (`POST`/`PATCH` under
`/b/products/api/admin/products` and `/b/products/api/products`) now answer
**400** naming any of `id`, `owner_kind`, `owner_id`, `created_by`,
`seller_account_id`, `approval_status`, `stripe_product_id`,
`current_version`, `submitted_at`, `published_at` and `deleted_at` that a
request body carries. Each of those columns has a dedicated writer that
maintains its invariants; none of them is a caller-supplied value on any tier
or verb.

**What each endpoint did before is not the same story, so check yours:**

- **Admin create** (`POST /b/products/api/admin/products`) forwarded the body
  to the database verbatim and applied its own defaults only for keys the
  body omitted. Every one of the eleven fields was **honoured**, `id`
  included — the database layer synthesizes a UUID only when `id` is absent.
  A seeding client that POSTs chosen ids has been getting those ids, and will
  now get a 400 for every such create. Drop `id` from the body and read the
  server-assigned one out of the response.
- **Seller create** (`POST /b/products/api/products`) overwrote `status`,
  `approval_status`, `owner_kind`, `owner_id` and `created_by` with its own
  values after parsing the body — those five were genuinely dropped, silently,
  behind a 200. The other six, `id` among them, were honoured.
- **Both PATCH paths** wrote every key in the body into the `UPDATE … SET`
  list, `id` included. That is the reason `id` is on the list at all: a
  `PATCH` body carrying one rewrote the product's primary key and orphaned
  every `line_items` / `offers` / `product_versions` / `entitlements` row
  pointing at it — and then the by-id re-read looked up an id that no longer
  existed and answered **"Product not found"**, so the caller was told the
  write had failed while the catalog had already been rewritten.

The admin and seller UIs send only caller-owned fields and are unaffected. An
API client that round-trips a whole product record back into a `PATCH` must
now send only the fields it is changing.

### Products: deleting a product is undoable

Deleting a product is now a soft delete: the row stays, with every
`line_items` / `offers` / `product_versions` / `entitlements` reference to it
intact, and only `deleted_at` changes. That is the point of the change — the
hard delete it replaces orphaned a completed order's line items.

Admin → Products has a **Deleted** tab listing those rows most-recently-
deleted first, with **Restore** on each. A deleted product is not editable
until it is restored.

Sellers get the same thing for their own products: **My Products** has the
same **Deleted** tab, showing only the caller's own deleted products, with
**Restore** (`POST /b/products/api/products/{id}/restore`) and **Close Stripe
surface** on each row. Both are scoped to the caller — another seller's
deleted product answers 404 on every path.

**Closing a deleted product's Stripe surface.** Soft delete touches nothing in
Stripe: a deleted product's Prices and Payment Links stay live in the connected
account and keep taking money, and deleting the product archives none of them.
Each row in a Deleted tab therefore also carries **Close Stripe surface**,
which opens a close-only manager for that product: archive its offers,
deactivate its payment links, nothing else. Use it *before* Restore if the
reason for the delete was that the product should stop selling — Restore puts
an active, approved product back into the public catalog immediately.

**Known gaps.**

- The close-only manager acts one offer and one link at a time. There is no
  "close everything" action, and nothing blocks Restore while a money surface
  is still open.
- A suspended seller cannot restore a deleted product, nor archive its offers
  or deactivate its payment links — those are all mutations a platform
  suspension stops (suspension already archives the seller's Stripe catalog).
  An administrator can do any of them on their behalf.

### Products: restoring a deleted product whose slug was taken

020 deliberately skips a row whose slug a live product of the same owner
already holds. Repairing it would violate migration 005's partial unique slug
index and abort the migration, which is unrecoverable in place: the hash never
gets stamped, so every later boot retries and re-fails, and on Cloudflare that
is a 500 on every request. A skipped row keeps its current half-state and stays
listed in the *deleted products* view (admin, or the owning seller's My
Products), where **Restore** is the remedy —
it reports the slug conflict in plain language instead of failing opaquely. To
find them:

```sql
SELECT id, owner_kind, owner_id, slug
FROM impresspress__products__products WHERE deleted_at = '';
```

Rename whichever product should not hold the slug, then restore. Re-running 020
is *not* the remedy: once applied, its hash is stamped and the migration
short-circuits for good.

### Branding: the built-in raster wordmark is gone — no action required

The bundled brand art is now a true pixel-art mark, and the long-form raster
wordmark (`impresspress-logo-long.png`) has been deleted along with its
`/b/static/impresspress-logo-long-{hash}.png` route. Brand text is text now:
`WAFER_RUN_SHARED__LOGO_URL` defaults to blank, and every surface that used to
show the wordmark — the sidebar, the auth cards and the userportal account
card — renders the square mark next to the app name instead.

**Why this needs a note.** Older releases declared that route's URL as
`LOGO_URL`'s *default*, and `seed_defaults` writes a declared default into the
`variables` table the first time it sees a key with no row. So an existing
deployment does not fall back to the new blank default: it holds a stored
`/b/static/impresspress-logo-long-{hash}.png`, pointing at a route this release
no longer serves. Left alone that is a silently broken image on every page.

**It repairs itself.** `seed_defaults` clears any `LOGO_URL` row still holding
that route back to blank, on the first boot after the upgrade, and logs a
warning naming the value it cleared. This deliberately does *not* ship as a
migration: migrations are gated on `--run-migrations` (see the top of this
section) and a broken logo gives an operator nothing to opt in *from*, whereas
`seed_defaults` runs on every boot's `Init` on all three targets. The match is
scoped to that one built-in route, so a white-labelled `LOGO_URL` of your own
is never touched.

**If you want a wordmark back,** set `WAFER_RUN_SHARED__LOGO_URL` to your own
image in Admin → Settings → Variables. It renders exactly as before.

**SDK (`@impresspress/js`):** `IMPRESSPRESS_ASSETS.logoLong` and
`static/logo_long.png` are removed — a breaking change for any consumer that
referenced them. `IMPRESSPRESS_ASSETS.logo` (the square mark) and
`favicon.ico` are unchanged in name and now carry the new art.

### Email: `WAFER_RUN_SHARED__SITE_URL` is gone, and mail is sent under your App Name

**`WAFER_RUN_SHARED__SITE_URL` is no longer a setting.** Its only reader was a
`welcome` email template that nothing ever sent, and it defaulted to the
project's own marketing domain. Both are removed, along with the equally unsent
`payment_failed` template: `email.send_template` now knows `verification` and
`password_reset` only, and any other name is a 400 as an unknown template.

**What to do.** Nothing is required. A deployment that booted an earlier release
still holds a `WAFER_RUN_SHARED__SITE_URL` row in its variables table. It is
harmless — nothing reads it — and it is listed on Admin → Settings → Variables
like any other key no block declares, where you can delete it. Setting
`WAFER_RUN_SHARED__SITE_URL` anywhere now has no effect: native boot no longer
copies it from the environment into the variables table, and no code reads it
on any target.

**The default sender's display name is your App Name.** With
`IMPRESSPRESS__EMAIL__MAILGUN_FROM` unset, mail used to go out as
`Impresspress <noreply@{your Mailgun domain}>` whatever the deployment was
called. It now carries `WAFER_RUN_SHARED__APP_NAME` — quoted, or RFC 2047
encoded when it is not plain ASCII — so recipients see the name you configured.
A `MAILGUN_FROM` you set yourself is sent unchanged, as before.

### Auth: OAuth sign-in now needs a *proven* address, and existing accounts have none

**What changes.** An OAuth identity may only join an existing local account when
both sides have proven the address: the provider asserts it is verified, and the
local row records who proved it. Before this release the callback matched on the
address alone, so anyone who could register `victim@example.com` with a password
— on a default install that is anyone, since `WAFER_RUN__AUTH__REQUIRE_VERIFICATION`
is off and the signup mails nothing — owned the account the victim's Google
sign-in landed in.

`users.email_verified` could not carry that decision. Signup writes it
`!REQUIRE_VERIFICATION`, so with verification off it means "this deployment does
not ask", not "somebody proved it". Migration 013 adds `email_verified_by`, which
names the act: `email_token` for a redeemed verification link, `oauth.<provider>`
for a provider that asserts verification.

**The upgrade consequence.** `email_verified_by` is **not backfilled**, and it
cannot be: a row that predates it may have been verified by a real mailed link or
may be a default-on-signup row, and backfilling would restore the takeover for
every squatted address. So on the first release that has it, **no existing
account can be linked to an OAuth provider** until its address is proven again.
Those users sign in with their passwords exactly as before, and the admin user
list still shows them as verified — that column is the policy flag and has not
changed meaning.

**What a user does about it.** Either, without an operator:

- **Ask for a verification link** — `POST /b/auth/api/resend-verification`, or
  the "resend" link on the verify page — and open it. Both the resend and the
  redemption key on `email_verified_by`, so an account the flag already calls
  verified is still offered a link and still records the proof when it redeems
  one.
- **Reset the password.** A redeemed reset link is mailbox proof of the same
  strength, so `POST /b/auth/api/reset-password` records it too.

Either one makes the account adoptable, permanently. There is nothing for an
operator to run, and no database edit is expected of anybody.

**A related nuisance, not a vulnerability.** A provider that asserts nothing
about the address it returns — Microsoft, whose `email` claim is a mutable tenant
attribute — can still create a local account holding *any* address, including the
one in `WAFER_RUN_SHARED__AUTH__BOOTSTRAP_ADMIN_EMAIL`. That account is created
unproven, so it is granted no admin role and cannot be adopted by anyone; what it
does is occupy the address, and the real owner then meets "an account already uses
this email address" when they try to link their own provider.

Recovery is **reset, then unlink**, and it takes both halves:

1. **Reset the password** at `/b/auth/forgot-password`. The link goes to the
   address, so only its owner can complete this. They now have a password, and
   the reset records the address proof.
2. **Unlink the other provider** at **Account → Security → Linked accounts**.
   The reset does *not* do this: it revokes their refresh tokens, so they lose
   the session within the access-token lifetime
   (`WAFER_RUN__AUTH__ACCESS_TOKEN_LIFETIME_SECS`, 30 minutes by default) rather
   than at once — but their `provider_links` row survives, and signing in with
   that provider again would put them straight back into the account. Removing
   the link is what evicts them.

That second step is new in this release — before it, nothing anywhere in the
product could remove a provider link. The page refuses to remove an account's
last way in, so set a password (step 1) before unlinking the only link.

### Auth: stored OAuth provider tokens are cleared (migration 014), and the device list empties once

**What changes.** An OAuth sign-in used to store the provider's access token in
`wafer_run__auth__provider_links.access_token`, in the clear. That token is a
live bearer credential for the user's account at Google, GitHub or Microsoft,
usable there by anyone who reads it, and nothing in Impresspress ever read it
back. Sign-ins now write the column empty, and auth migration 014 empties every
existing row, including links nobody signs in through any more. The column
itself stays, empty, and the admin SQL explorer keeps refusing the table.

**Upgrade with `--run-migrations`** to clear the tokens already stored. A
Cloudflare deploy runs migrations through `/_deploy/init` and gets it without
doing anything. A native deployment that skips the flag keeps the old tokens in
the table, and logs the `schema drift` warning for `wafer-run/auth` on each
boot, until it runs.

**What the migration run also does: every device leaves the session list.**
Auth migrations are re-run as a set whenever any auth migration changes, and
migration 012 in that set drops and recreates `wafer_run__auth__sessions`. So
the run that applies 014 also empties that table. Nothing authenticates against
it: it feeds the device list at **Account → Sessions**. After the upgrade that
list is empty, and each device reappears when it next refreshes its tokens.
Until a device reappears, its user cannot revoke that one device from the list;
**changing the password** still signs every device out, because it revokes the
refresh tokens rather than reading the list.

**Correction: this release's note said "nobody is signed out", and that was
wrong.** The reason given was that refresh tokens live in a separate table —
and so they do, in `wafer_run__auth__tokens`, which migration 004 in the same
re-run set opened by dropping. So the 014 upgrade, and every earlier auth
schema change, signed every user out within one access-token lifetime. 004 no
longer drops the table (see the API-key expiry note below), so from this
release onward an auth schema change keeps refresh tokens.

### Auth: an API key's expiry is a timestamp, and broken ones are revoked (migration 015)

**What changes.** `POST /b/auth/api/api-keys` used to store the `expires_at`
string it was sent, exactly as sent, and the lookup on every request compared
that string to the clock as **text**. Text order is time order only within one
format and one offset, so two whole classes of value were read wrong:

- an offset — `2026-09-23T20:00:00+09:00` is 11:00 UTC, but it sorts after
  `2026-09-23T12:00:00Z`, so the key kept authenticating for eight hours after
  it had expired;
- anything that is not a timestamp — `never` sorts after every timestamp there
  will ever be, so a key minted with it **never expired**.

The endpoint now answers `400` for an `expires_at` it cannot read as RFC 3339,
and for one already in the past; it accepts any offset and stores the instant
as `YYYY-MM-DDTHH:MM:SSZ`. The lookup parses the stored value instead of
comparing it as text, and treats an expiry it cannot parse as **expired** — a
key whose end date cannot be read has no enforceable end. A key now expires
*at* the instant it names rather than one second after it.

**Some existing keys stop working the moment you deploy, migrations or not.**
The parsing lookup is in the code half, so it applies immediately. Every stored
expiry that is not RFC 3339 is now read as expired, and RFC 3339 **requires a
UTC offset**. That includes shapes that look perfectly reasonable and that a
browser produces by default:

- `2027-01-31` — what `<input type="date">` posts;
- `2027-01-31T09:00` — what `<input type="datetime-local">` posts;
- `2027-01-31T09:00:00` — a timestamp with the offset left off;
- `2027-01-31T09:00:00+0900` — an ISO 8601 basic-form offset (no colon).

A key carrying any of these stops authenticating at once, **possibly months
before the date it names**. If you integrated against this endpoint with a date
picker, assume your keys are affected and reissue them with an explicit offset
(`2027-01-31T09:00:00Z`). Keys with no expiry at all are unaffected.

**Upgrade with `--run-migrations`** to make the column say so. Migration 015
respells a UTC expiry (`Z`, `z`, `+00:00`, `-00:00`, a space separator) as
`YYYY-MM-DDTHH:MM:SSZ` without moving the instant, and **revokes** every key
whose stored expiry the lookup cannot read, so the admin API-keys tab shows a
revoked key rather than one that quietly stopped working. The revoked set is
exactly the set the lookup refuses: all four shapes above, anything that is not
a timestamp at all, and timestamp-shaped values that name no instant — a day
the month does not have (`2026-02-31`, 29 February outside a leap year), a
field out of range (`T25:00:00Z`, a minute past 59, a second past 60 — 60
is a leap second and is read — an offset past `23:59`), a fraction with no offset after
it (`T12:00:00.5`), or anything trailing the offset. The stored text is left as
it was found on those rows: it is the only record of why the key was revoked.
Keys with no expiry are untouched. A sub-second fraction and a non-zero offset
are left as they stand: both are read correctly, and respelling either needs
arithmetic the migration deliberately does not do.

**Deploying this keeps everyone signed in; the device list empties.** Adding
migration 015 changes the auth block's SQL hash, so the migration run re-runs
the whole auth set. Migration 004 in that set used to open with `DROP TABLE IF
EXISTS wafer_run__auth__tokens`, the refresh-token table, and a refresh with no
stored row is refused — which is how every earlier auth schema change signed
every user out (see the correction in the migration-014 note above). This
release removes that DROP, and the set a migration run applies is the one
compiled into the binary doing the run, so this run already executes the 004
without it: refresh tokens, accounts, passwords, OAuth links and API keys all
survive. What does not is the session/device list at **Account → Sessions**:
migration 012 in the same set drops and recreates it, and it refills as each
device next refreshes its tokens.

### Auth: the OAuth start is rate-limited per IP, in a bucket of its own

**What changes.** `GET /b/auth/oauth/login` writes a PKCE state row on every
request, and it used to spend no rate-limit bucket at all. It now spends its own
IP-keyed bucket, `oauth_start`: **30 starts per 60 seconds per client address**
by default. It does not share the `auth` bucket that login, signup and the
password-reset endpoints spend, so a burst of OAuth starts cannot lock anyone
out of a password login, and loosening the one does not loosen the other.

**Who has to act.** A deployment that raised or disabled
`WAFER_RUN_SHARED__RATE_LIMIT_AUTH` — typically because many users share one
egress address (an office NAT, a campus, a proxy that does not forward the
client IP) — gets none of that headroom on the OAuth start: it is capped at the
30/60 default from the first request after the deploy, and users behind that
address see `429` on the provider buttons. Set the bucket by its own key:

```
WAFER_RUN_SHARED__RATE_LIMIT_OAUTH_START=300/60   # requests/seconds; 0 disables
```

Like every `WAFER_RUN_SHARED__RATE_LIMIT_*` category it is set by key — process
environment or the `variables` table — and no `ConfigVar` declares it, so it
does not appear as a field on any admin settings page. No migration is involved.

### Auth: `SESSION_LIFETIME_DAYS` must be a whole number from 1 to 3650

**What changes.** `WAFER_RUN_SHARED__AUTH__SESSION_LIFETIME_DAYS` is now
checked. Before this release, any positive number was accepted. A value above
about 95,000,000 crashed the native server on every login. A value of `0`, a
non-number such as `abc`, or a padded value such as ` 7` was quietly read as the
default of 7 days. From this release:

- The admin Variables page, the settings API and `CONFIG_SET` refuse any value
  outside 1–3650 with a 400.
- The boot seeders do not store an environment export that fails this rule.
  The boot log names it at ERROR ("refusing to seed this config key from the
  environment"), and the stored value, or else the default, stays in effect.
- A value **already stored** outside the range is refused when it is read.
  Login, signup, refresh, bootstrap redemption and the OAuth callback then
  answer with a 500 that names the key. The request that fails does not use up
  the refresh token, bootstrap token or OAuth state it presented, so sessions
  resume once the value is corrected.

**What to check before upgrading.** Look at the stored value (Admin → Settings →
Variables). If it is not a whole number from 1 to 3650, set a valid one before
you upgrade. After upgrading with a bad stored value, nobody can sign in or
refresh until it is fixed. The fix depends on who owns the row:

- *The environment owns the row* (no admin ever edited it). Export a valid value
  and restart; the boot replaces the row.
- *An admin edited the row.* Correct it on the Variables page from a session
  that is still signed in. If no session is left, the stored row has to be
  edited directly in the database.

### Auth: a refused database call is a 403, not a 500

**What changes.** When WRAP refused one of the auth routes' database calls — a
deployment missing the grant a table needs, or a row guard — the route answered
`500 Internal server error (ref: …)`, the same as an outage. It now answers
`403` (`Access denied`), and a quota the service enforces answers `429`, as every
other block already did. The auth settings and organizations pages answer the
same statuses as a styled page. A genuine fault is still the `500` with a
correlation id.

**Who has to act.** Only an alert or a client that treats a `5xx` from
`/b/auth/*` as the one sign of a misconfigured deployment: a missing grant now
shows up as `403`, and the server log line reads `database access denied`. A
signed-in client that sees `403` from `/b/auth/api/refresh` or `/b/auth/api/me`
should not treat it as a sign-out — the credential is intact; the deployment
refused the read. No migration is involved.

### LLM: a provider can name its token-budget field (migration 002)

**What changes.** Which field carries the output-token budget in a chat request
used to follow the provider's protocol and nothing else: `open_ai` sent
`max_completion_tokens`, `open_ai_compatible` sent `max_tokens`. That is right
for every endpoint but one. Azure OpenAI is configured as
`open_ai_compatible`, and its *reasoning* deployments accept only
`max_completion_tokens` — so that operator had no reachable configuration and
every chat turn came back `400`. Providers now carry an optional
**Token budget field**, on the form and on
`POST`/`PATCH /b/llm/api/providers`, which overrides the protocol's spelling.

**Nothing changes for existing providers.** The column is nullable and empty
means "follow the protocol", which is what every configured provider was
already doing. There is no backfill.

**The provider table has no edit form**, only add / discover / delete, so the
new field is reachable from the admin page when you *create* a provider. An
Azure provider that already exists is changed through
`PATCH /b/llm/api/providers/{id}` (or by re-creating it) until an edit form
exists.

**One admin-API change to know about if you script against it.** On
`PATCH /b/llm/api/providers/{id}`, sending `"key_var": null` used to be
accepted and do nothing; it now clears the variable, the same as the empty
string already did and the same as `"max_tokens_field": null` does. Omitting
the key still leaves the stored value alone.

**Upgrade with `--run-migrations`** to add the column. Cloudflare deploys run
the block's migrations through `/_deploy/init` on every deploy, so a Cloudflare
deployment gets it without doing anything. A native deployment that skips the
flag logs the generic `schema drift; redeploy with --run-migrations to apply`
warning for the llm block on each boot; providers keep working, because a
native deployment leaves `WAFER_RUN__DATABASE__STRICT_SCHEMA` off and the
column is then added on the first provider write. If you have turned strict
schema **on**, the migration is not optional — a provider create or edit will
fail on the missing column until it runs.

### Admin: a user holds each role once (migration 004), and deleting a role revokes it

**What changes.** Role grants (`impresspress__admin__user_roles`) are now unique
per user and role, on every deployment the migration reaches (see the last
paragraph). Before, two logins of the bootstrap-admin address at the same
moment could each grant `admin`, leaving two identical rows — and revoking the
role then deleted one of them, reported success, and left the user an admin
through the other. Deleting a role also left every grant of it in place: its
name kept appearing in the tokens its former holders were issued, and creating
a role of the same name later handed it straight back to them. Deleting a role
now revokes it from everyone who held it and invalidates the access tokens
issued to them while they had it, and the audit row names the role and how many
grants went with it.

**One admin-API change.** `POST /b/admin/api/iam/user-roles` now grants only a
role that exists: a role name with no definition answers `400` ("No role named
… exists. Create the role first.") instead of writing a grant. Create the role
on the Roles tab first if you script against this endpoint.

**The repair deletes rows.** Migration `004_user_roles_unique` deletes
duplicate grant rows before creating the index, keeping **one** row for each
user and role — the least by `created_at`, then `id`, in the database's text
ordering (the earliest, on SQLite; on Postgres under a non-`C` collation the
order is the locale's, which need not be chronological). Which twin survives
does not matter: every row it deletes repeats a grant the kept row still
makes, so nobody gains or loses a role — what goes is the extra row a revoke
could miss.

**Data snapshots.** A `/b/dev` data snapshot exported before this release can
repeat a grant too. Importing it keeps one row per user and role, by the same
rule, rather than failing on the new index.

**Roles you deleted before upgrading are still granted.** The migration does
not touch grants that name a role which no longer exists. To find them, run this
from the admin SQL explorer:

```sql
SELECT ur.user_id, ur.role, ur.id
FROM impresspress__admin__user_roles AS ur
WHERE NOT EXISTS (
    SELECT 1 FROM impresspress__admin__roles AS r WHERE r.name = ur.role
)
ORDER BY ur.role, ur.user_id;
```

and revoke each one with `DELETE /b/admin/api/iam/user-roles/{id}` — or
re-create the role on the Roles tab and delete it again, which now revokes all
of them at once.

**No flag is needed to apply it.** A native deployment runs the admin block's
schema files before every boot, and a Cloudflare deploy runs every block's
migrations through `/_deploy/init`, so the repair and the index are in place on
the first boot or deploy of this release. A native deployment that does not
pass `--run-migrations` still logs the generic `schema drift` warning for the
admin block until it does once; that warning is about the recorded hash, not
the schema.

**Browser installs get it on their next boot.** A browser install applies a
changed migration on the first boot of the bundle that carries it (see
"Browser: migrations run once per change" above), so the repair and the index
land there too. The role-delete revocation and the assign-endpoint check do
not depend on the index and apply everywhere.

### API: a duplicate names the field that is taken

**What changes.** A write refused because a unique value is taken still answers
`409` with `"error":"AlreadyExists"`, but its `message` now names the record,
the field and the value in one wording on every route: `A <record> with the
<field> "<value>" already exists. Choose a different <field>.` Routes that
used to say only "A record with the same key already exists" — LLM provider
create and update (`name`), product create and update on both the admin and
the seller API (`slug`), checkout preset create and update (`slug`), and
ticket type create (`key`) — now say which value is taken; roles,
permissions, variables and buckets, which already named it, now use the same
sentence. Restoring a product whose slug another live product took says so
the same way, followed by "Rename or delete that product, then restore this
one." A client that matched on the old message text must match on the `409`
status or the `AlreadyExists` code instead.

## The release workflow has never produced a release

Read this before you tag anything. No `v*` tag has ever existed in this
repository or upstream, so the
[Release workflow](../../actions/workflows/release.yml) has never run on a tag
and **no release has ever been published from it**. Its `publish` job has never
executed. Nothing below the dry run is a description of something observed
working end to end.

The only runs this workflow has are the dry runs introduced with it. That is
what the dry run is for: it is not optional pre-flight advice, it is the only
way anyone has ever seen any of this workflow run.

## Pre-Release Checklist

Before tagging a release, verify:

- [ ] `main` branch CI is green (check the [Actions tab](../../actions))
- [ ] Cross-platform builds pass (the `CI Main` workflow runs on every push to `main`)
- [ ] Update `version` in `Cargo.toml` workspace section to match the intended release
- [ ] **Run the release workflow as a dry run and see it green** (below)
- [ ] No known critical bugs (check [open issues](../../issues))
- [ ] Test the binary locally:
  ```bash
  cargo build -p impresspress --release
  ./target/release/impresspress
  ```
- [ ] If this release changes config variables or CLI flags, update the docs
- [ ] If this release ships a migration that repairs existing data, add an entry
      to [Upgrade Notes](#upgrade-notes) so operators know to pass
      `--run-migrations`

## Dry run — the pre-flight step

```bash
# Run everything the release does except creating the release.
gh workflow run release.yml --ref main -f dry_run=true

# Watch it.
gh run list --workflow=release.yml --limit 1
gh run watch <run-id>
```

A dry run executes, for real:

1. **`verify-tag`** — reads `version` from `Cargo.toml`'s `[workspace.package]`
   table and prints the tag you must push (`v<version>`). On a branch there is
   no tag to compare, so it reports the expected one; on a tag it fails the run
   if the two disagree.
2. **`build-wasm`** — the `impresspress-web` wasm, via the same
   `build-wasm.yml` every CI run uses.
3. **`build`** — all five cross-compile targets, packaged as `.tar.gz`/`.zip`
   and uploaded as run artifacts.

It does **not** run `publish`, so no GitHub Release, and no tag, is created.
A skipped `publish` does not turn a red run green: a run's conclusion is
failure if any job failed, whatever was skipped afterwards.

Dispatching a branch requires `dry_run: true`; a non-dry-run dispatch must
target a tag, because `gh release create --verify-tag` has nothing to verify
otherwise.

## Creating a Release

```bash
# 1. Make sure you're on main and up to date
git checkout main
git pull

# 2. Dry-run first (see above). Do not skip this — the publish path has never
#    run, so a dry run is the only evidence that anything before it works.
gh workflow run release.yml --ref main -f dry_run=true

# 3. Tag the release. The tag MUST be `v` + the workspace version in
#    Cargo.toml, or the `verify-tag` job fails the run before anything builds.
git tag v0.1.0

# 4. Push the tag — this triggers the release workflow
git push origin v0.1.0
```

The [Release workflow](../../actions/workflows/release.yml) is intended to:
1. Check the tag against `Cargo.toml`'s workspace version and stop if they disagree
2. Build binaries for all 5 platforms (Linux amd64/arm64, macOS amd64/arm64, Windows amd64)
3. Create a GitHub Release (`gh release create --verify-tag`, so the tag must
   already exist — the command will not invent one) with auto-generated notes
   from merged PRs

Step 3 has never executed. If it fails, re-run the failed `Publish Release`
job on that same run — step 2's artifacts are still attached to it, so nothing
rebuilds. A fresh dispatch does NOT reuse them: it starts a new run and
rebuilds all five targets, which is the fallback once the run's artifacts have
expired. Either way, do not retag.

## After Release

- [ ] Verify the [GitHub Release](../../releases) was created with all 5 platform artifacts
- [ ] Download and smoke-test at least one binary
- [ ] Announce in relevant channels if this is a notable release

## Hotfix Process

Branch protection prevents pushing directly to `main` — hotfixes follow the same PR flow:

The tag must equal `v` + `Cargo.toml`'s `[workspace.package] version`, so the
version bump is part of the hotfix PR, not an afterthought — `verify-tag` fails
the run otherwise, before anything builds.

```bash
# 1. Create a hotfix branch
git checkout main && git pull
git checkout -b hotfix/v0.1.1

# 2. Fix the bug AND bump [workspace.package] version to 0.1.1 in Cargo.toml,
#    then commit and push both together
git push -u origin hotfix/v0.1.1

# 3. Open a PR — CI must pass, 1 approval required
gh pr create --title "fix: critical bug description"

# 4. After merge, tag the patch release — v + the version just landed
git checkout main && git pull
git tag v0.1.1
git push origin v0.1.1
```

## Undoing a Release

If a release was tagged by mistake or contains a critical issue:

```bash
# Delete the tag locally and remotely
git tag -d v0.1.0
git push origin --delete v0.1.0
```

Then delete the GitHub Release from the [Releases page](../../releases). Note: users who already downloaded the binary still have it.
