# Releasing Impresspress

## Version Scheme

Impresspress uses [Semantic Versioning](https://semver.org/): `MAJOR.MINOR.PATCH`

- **MAJOR** — breaking changes to CLI flags, config format, or stored data
- **MINOR** — new features, new blocks, new config options
- **PATCH** — bug fixes, security patches, dependency updates

## Upgrade Notes

Notes for operators upgrading an **existing** deployment. Every target
applies a release's pending block migrations before it serves it: a native
server at every boot, a Cloudflare deploy (`impresspress deploy`) before the
new version is promoted, a browser install on the first boot of a bundle that
changes them. Before 1.0, an upgrade may require wiping local data.

Older entries say "upgrade with `--run-migrations`"; that flag is gone, and
what it did happens on every native boot.

### Cloudflare: each invocation can log the D1 rows it read and wrote

**What changes.** The Worker now adds up, per invocation, the rows D1 reports
each query read and wrote (`meta.rows_read` and `meta.rows_written`, the
figures D1 bills by). When the invocation's D1 work is over, it logs one
line if it built a runtime, or if it read more rows than
`IMPRESSPRESS_D1_ROWS_LOG_THRESHOLD` (default 1000):

```
metric=d1_cost build=prepared statements=7 rows_read=44 rows_written=1 unmetered=0
```

- `build` is `prepared`, `dynamic` or `none`;
- `statements` counts every D1 query the invocation sent, the
  post-response work included (the request's audit row, deferred tasks,
  a streamed body's final writes);
- `unmetered` counts the queries whose rows D1 did not report: a failed
  query, and the admin SQL explorer's query, which D1 answers without
  row counts. A line whose `unmetered` is not 0 reports a floor.

Read the lines with `wrangler tail <worker> --format json` and filter on
`d1_cost`.

**Also changes.** Every D1 read except the SQL explorer's now goes through
D1's `all()`, the call whose answer carries the row counts, instead of
`raw()` or `first()`. The rows a read returns are the same.

**What to do.** Nothing. To log more invocations, set
`IMPRESSPRESS_D1_ROWS_LOG_THRESHOLD` as a Worker var (`0` logs every
invocation that read a row). A value that is not a whole number is reported
on every invocation as `metric=d1_rows_log_threshold_invalid`, and the
default applies.

### Admin: the system roles come from a migration (admin 010)

**What changes.** Admin's new migration `010_default_roles` seeds the two
system roles, `admin` and `user`, which admin's `Init` seeded until now.
`Init` decided by counting the roles table on every boot, and Cloudflare
runs every block's `Init` on every cold isolate, so each cold isolate sent
one `COUNT(*)` over the table: on one deployment about 10,500 calls a day.
The count was also read as 0 when it failed, so a failed read went on to
write. `Init` now reads nothing from the table, on every target.

**What 010 writes,** into an empty table only: `admin` ("Full access to all
resources") and `user` ("Standard user access"), both system roles with no
permissions, under the ids `seed-admin` and `seed-user`. A table holding any
role, system or not, gets nothing, so a database that already has the roles
gets no second set (as before: the guard is the whole table, as `Init`'s
count was).

**Behaviour change: an emptied roles table stays empty.** Until now,
emptying the table made the next boot (on Cloudflare, the next cold isolate)
seed both roles again. Once 010 has run, nothing does. The system roles
cannot be deleted from the IAM page, so only a delete through the SQL
explorer reaches this. With the table empty, the IAM page cannot grant a
role; to recover, create `admin` and `user` again from the IAM page (they
come back as ordinary roles, which can be renamed and deleted). The two
roles are now also written together, in one transaction: `Init` created them
one at a time, and a failure between the two left one role that no later
boot completed, because the table was no longer empty.

**Deploying it.** One file runs, in one transaction with its record, on
every target; on Cloudflare it is one D1 batch of two statements in
`/_deploy/prepare`. A database with roles keeps exactly the rows it had. A
database whose roles table is empty at the upgrade gets the two roles, as
its next boot's `Init` would have seeded them.

**Rollback.** A build from before 010 sees a record it does not know and
refuses it as "migrated by a newer build": a native server does not start, a
Cloudflare deploy through `impresspress deploy` stops, a browser bundle stops
without erasing. `wrangler rollback`, or promoting an older version, serves,
because the request path never plans migrations; that version's `Init`
counts the roles table on every cold isolate again, and seeds the roles
only into an empty table.

**Cloudflare budget.** A fresh Worker database now sends 487 statements for
its migrations, two more than the 485 before it (010's statement and its
ledger row), within half of the default 1000.

### Legalpages: the default documents come from a migration (legalpages 003)

**What changes.** Legalpages' new migration `003_default_documents`
publishes the default Terms of service and Privacy policy, which the block's
`Init` published until now. `Init` decided by counting the documents table on
every boot, and Cloudflare runs every block's `Init` on every cold isolate,
so each cold isolate read every stored document version, archived ones
included: on one deployment that was 305 rows on about 10,800 cold isolates
a day, 57% of its account's daily D1 reads. `Init` now reads nothing from the
table, on every target.

**What 003 writes,** into an empty table only: both documents published as
version 1 by `system`, with the titles and text `Init` used, under the ids
`seed-terms` and `seed-privacy`. A table holding any document, in any status,
gets nothing, so a database that already has the defaults gets no second set,
and a type an admin deleted while the other remains is not seeded again
(as before).

**Behaviour change: deleting every document is final.** Until now, deleting
every document made the next boot (on Cloudflare, the next cold isolate)
publish both defaults again. Once 003 has run, nothing does. Publish them
from the editor to have them back. This also ends a race: two isolates
that found the table empty together both published, the second publish of
each type taking version 2, which left an archived duplicate of each
default behind the live one.

**Deploying it.** One file runs, in one transaction with its record, on
every target; on Cloudflare it is one D1 batch of two statements in
`/_deploy/prepare`. A database with documents keeps exactly the rows it had.
A database whose table is empty at the upgrade gets the two defaults, as its
next boot's `Init` would have published them.

**Rollback.** A build from before 003 sees a record it does not know and
refuses it as "migrated by a newer build": a native server does not start, a
Cloudflare deploy through `impresspress deploy` stops, a browser bundle stops
without erasing. `wrangler rollback`, or promoting an older version, serves,
because the request path never plans migrations; that version's `Init`
counts the table on every cold isolate again, and publishes the defaults
only into an empty table.

**Cloudflare budget.** A fresh Worker database sent 485 statements for its
migrations at this release, two more than the 483 before it (003's statement
and its ledger row; 487 since admin 010, above), within half of the default
1000.

### Cloudflare: D1's daily limits and resets are reported as temporary

**What was wrong.** D1 reports a failed query only as text, and impresspress
decides from that text whether the failure is temporary (`Unavailable`) or a
fault (`Internal`). Its list of temporary texts was short. On the Free plan,
once the account passes its daily row read or row write limit, every query
fails with `D1_ERROR: Your account has exceeded D1's free tier daily row read
limit. …` until 00:00 UTC. That text, and D1's reset and unreachable-database
texts from Cloudflare's D1 error list, were not on impresspress's list, and
the list was matched case-sensitively. All of them were reported as faults.

**What changes.** These D1 texts are now classified `Unavailable`:

- the daily row read and row write limits;
- `D1 DB reset because its code was updated`;
- `Internal error while starting up D1 DB storage caused object to be
  reset` and `Internal error in D1 DB storage caused object to be reset`;
- `Replica disconnected from primary`;
- `Cannot resolve D1 DB due to transient issue on remote node`;
- `D1 DB's isolate exceeded its memory limit and was reset` and `D1 DB
  exceeded its CPU time limit and was reset`.

The texts now match in any letter case. A full database (`Exceeded maximum
DB size.`), a full account (`…maximum account storage limit…`) and a result
over D1's 50 MB limit stay faults.

What you see today, when a request's query fails with one of these texts:

- the error is logged as a warning (`database temporarily unavailable`), not
  as an error (`database internal error`);
- where a block passes the error code through, the answer is a 503 instead
  of a 500. Auth does: a signed-in request whose credential check hits one
  answers 503 `Authentication is temporarily unavailable`;
- elsewhere, the shared database-error handling still answers 500. That
  changes in a later release.

How a block's `Init` failure is treated does not change yet. On Cloudflare,
any `Init` failure still fails the runtime build, and the next request builds
again. The classification starts to matter when a later release keeps
serving with a degraded block, which then retries that block's `Init`
instead of failing it for good.

**What to do.** Nothing.

### wafer-run 96860baa: Admin → Database opens on Cloudflare

**What changes.** The wafer-run pin moves from `1272fd63` to `96860baa`
(wafer-run #463 and #470, nothing else). Admin → Database
(`GET /b/admin/api/database/tables` and the SSR page) answered 500 whenever
the database held a table whose name is not a plain identifier (lowercase
letters, digits and `_`): the listing returned it, and the row count then
refused the name before any query ran. Every Cloudflare D1 database holds
one, D1's internal `_cf_KV`, so on Cloudflare the page failed on every
load; natively and in the browser it failed once anything created a table
such as `Foo` or `a-b` outside the migrations. Now:

- a table under any name is listed with its row count, its name quoted as
  the catalog spells it;
- the internal tables of the engine are not listed at all: D1's `_cf_KV`
  and `_cf_METADATA` (which D1 refuses to read) and SQLite's `sqlite_…`
  tables. A table whose name merely resembles the `sqlite_` prefix
  (`sqlite1`, `sqlitex_data`) is listed again; the old filter read `_` as a
  wildcard and hid it.

Opening such a table's schema still takes a plain identifier, as before:
`/b/admin/api/database/tables/{name}/columns` answers 400 and the page says
the name is not a valid table name.

**What to do.** Nothing.

### Cloudflare: the site is served at `/`, as on native

**What changes.** `impresspress build/deploy --target cloudflare` stages the
site directory (`frontend/build/`, else `public/` — the same one the native
flows copy) under the storage folder `wafer-run/web/site`, the folder
`wafer-run/web` serves `/**` from. Before, it staged `public/` under logical
keys `public/…`, which nothing reads, so a Cloudflare deploy answered `/` and
every site path with a 404 while native served the site. `frontend/build/`
was not staged at all. `dist/` and `content/` are still staged under their
own names. The deploy log now names the site directory it staged.

**Who notices.**

- **A consumer with a `public/` (or `frontend/build/`) site:** the site is
  served after the next deploy. `/` itself serves it only with
  `WAFER_RUN_SHARED__HAS_LANDING_PAGE=true` (default `false` on every server
  target; see the README's "Serving your own site").
- **A repo with both `frontend/build/` and `public/`** now serves
  `frontend/build/` on Cloudflare, as native always did, and no longer
  uploads `public/` at all.
- **A consumer that worked around this with `cloudflare.r2.release_assets_dir
  = "public"` + `release_assets_prefix = "wafer-run/web/site"`:** every
  deploy now fails with an `R2 release asset key collision`, because the same
  files are staged twice. Remove both lines; the site is staged there
  without them. A workaround that pointed `release_assets_dir` at a directory
  other than the site directory keeps working as long as no file in it has
  the same path as one in the site.
- **`cloudflare.r2.release_assets_exclude` patterns written for site files**
  must now match `wafer-run/web/site/…` instead of `public/…`; a `public/…`
  pattern matches nothing any more.
- **A consumer block that read the storage folder `public`** (none in this
  repository or its known consumers) no longer finds those keys. Ship that
  directory explicitly: `release_assets_dir = "public"` +
  `release_assets_prefix = "public"`.

### Auth: the retired JWT blocklist table is dropped (auth 018)

**What changes.** Auth's new migration `018_drop_jwt_blocklist` drops
`wafer_run__auth__jwt_blocklist` and its index. Nothing has read or written
the table since "Auth: the JWT blocklist is retired" (below): an access token
authenticates only while its device's session row is live. Its rows name
tokens whose sign-in had already ended, so dropping them un-revokes nothing.

**Deploying it.** One file runs, in one transaction with its record, on
every target; on Cloudflare it is one D1 batch of two statements.
**Nobody is signed out**: no session, refresh-token, API-key or account row
changes. The admin's Database page lists one table fewer.

**A database without the table** was adopted from a wrong record or edited
by hand outside the app (`wrangler d1 execute`, `sqlite3`, the browser's
developer tools on its storage); every database this release upgrades
otherwise holds it. There 018 fails, not as a refusal: nothing of it
applies, it stays unrecorded, and the message names `wafer-run/auth`,
`018_drop_jwt_blocklist` and the database's own error (`no such table:
wafer_run__auth__jwt_blocklist` on SQLite and D1, `table
"wafer_run__auth__jwt_blocklist" does not exist` on PostgreSQL; D1 does not
say which statement failed). A native server refuses to start and a
Cloudflare deploy stops before promotion. In the browser it is the
`initialize` stage, which erases and starts afresh in a build rendered to
erase on recovery (the demo), as for any migration that fails ("Migrations:
recorded per file", below). The remedy is that entry's: restore a backup
taken before the upgrade, or recreate the database.

**Rollback.** A build from before 018 sees a record it does not know and
refuses it as "migrated by a newer build": a native server does not start, a
Cloudflare deploy through `impresspress deploy` stops, a browser bundle stops
without erasing. `wrangler rollback`, or promoting an older version, serves,
because the request path never plans migrations, and no build since the
blocklist was retired reads the table. A build from before that read it on
every authenticated request and fails those requests; downgrading across
migrations is not supported.

**Cloudflare budget.** A fresh Worker database sent 483 statements for its
migrations at this release, two more than the 481 before it (018's statement
and its ledger row; 485 since legalpages 003 and 487 since admin 010, both
above), within half of the default 1000.

### Native: an env file that cannot be loaded stops the boot; `serve --port` works

**What changes.**

- **A native server refuses to start when its `.env` cannot be loaded.**
  Before, a line it could not parse — typically an unquoted value with a
  space, `SITE_NAME=My Site` — silently ended the load there: every variable
  on and after that line was missing (in the reported run, the bootstrap
  admin was never created) and nothing was logged. Now the boot fails with
  the file and line number, e.g. `refusing to start: the env file cannot be
  loaded: /srv/site/.env:4: env file line cannot be parsed, …`. An unreadable
  `.env` fails the same way. No `.env` at all is still fine.
- **`IMPRESSPRESS_ENV_FILE` must name an existing file** (set but empty
  counts as unset), read relative to the process cwd as documented. A missing one used to print a warning and boot
  without it, and a relative name was also searched for in parent
  directories; now it stops the boot.
- **`impresspress serve --port N` now sets a sealed native server's port**,
  keeping the host of `IMPRESSPRESS_LISTEN` (`0.0.0.0` by default). It used
  to be ignored. For an embedded native app (your own binary) the flag is
  refused: that binary chooses its own address.
- **Embedders:** `impresspress_server::run` takes a fourth argument, the port
  override (`None` keeps `IMPRESSPRESS_LISTEN`), and
  `impresspress_native::load_dotenv` returns a `Result` the caller must not
  boot past.

**What to do.** Quote any `.env` value that contains spaces
(`KEY="two words"`) and check that a configured `IMPRESSPRESS_ENV_FILE`
exists, then restart.

### A bootstrap admin token survives a restart

**What was wrong.** With `WAFER_RUN_SHARED__AUTH__BOOTSTRAP_ADMIN_TOKEN` set
and no account yet, only the first runtime build started. Auth's `Init`
runs on every build (each native start, each Cloudflare isolate and
prepared plan, each browser rebuild), and each one inserted the token's
`bootstrap_tokens` row again. Its `token_hash` is UNIQUE, so every later
`Init` failed: a native restart refused to start, a Cloudflare deploy's
`/_deploy/verify` answered 5xx and the deploy was not promoted, and a
browser rebuild left auth failed. This lasted until someone redeemed the
token.

**What changes.** The first build that sees the token installs its row, and
every later build finds that row and leaves it as it is. The install is one
`INSERT … ON CONFLICT (token_hash) DO NOTHING`, so two isolates or processes
installing it at once leave one row, and neither fails.

- **The token's 24 hours run from that first install.** A rebuild does not
  restart them.
- **A token that expired unredeemed stays expired.** Configure a different
  token and restart or redeploy. The new token gets its own row. The old
  row stays in the table but can no longer be redeemed.
- **Configuring a different token does not revoke the old one.** If the old
  token has not expired, it stays redeemable until its 24 hours run out,
  even after the new one has created the admin, and it still grants admin as
  a bearer token. Do not rotate a token to shut out someone who has the old
  one; wait for it to expire. Making redemption spend every outstanding token
  is tracked as its own fix.
- **Upgrading needs no action.** A deployment stuck on this, with the token
  set and no account, starts on the new build. Its row is the one the first
  boot wrote, so if that boot was more than 24 hours ago the token has
  expired. Configure a different one.

### wafer-run 1272fd63: strict schema is the only behaviour, on every target

**What changes.** The wafer-run pin moves from `f0ca9c79` to `1272fd63`
(wafer-run #462). The database layer trusts the migrated schema on a native
server, in the browser (the demo, a dev sandbox) and on Cloudflare alike,
and there is no switch any more:

- **A write naming a column no migration declared fails**, and adds
  nothing. The backend answers `no such column` (or `table … has no column
  named …`), which a block reports as an internal database error, a 500.
  Before this release native and browser databases added the column on the
  first write that named it; Cloudflare already refused it.
- **A read, count or delete against a table no migration created fails**
  with the backend's `no such table`, instead of answering empty. One such
  read is on every fresh install: the runner reads admin's
  `block_settings` before admin's migrations have created it, and on that
  failure asks whether the table exists (`schema_table_exists`) before it
  takes the table as absent. So a fresh Worker database sends one statement
  more for its migrations than before, 481 rather than 480.
- **A filter, sort or guard naming a column the table lacks** is refused
  before it is sent, naming the column, on every target. Cloudflare used to
  skip that check, and SQLite then read the unknown name as a string
  literal: a filter matched nothing, a sort did not sort.
- **`WAFER_RUN__DATABASE__STRICT_SCHEMA` and
  `WAFER_RUN__POSTGRES__STRICT_SCHEMA` are gone.** `impresspress build
  --target cloudflare` no longer writes the Worker var. A
  `wrangler_overrides_path` file or a process export that still sets either
  does nothing; remove it. On native and in the browser the admin's
  Variables page listed the key under `wafer-run/database`; a row an admin
  saved for it there is now listed under **Unowned variables** (declared by
  no block) and does nothing. Delete it there with its delete button.
- **A sandbox (guest) block whose table does not declare `updated_at` is
  refused when it is activated.** The host refuses `database.ensure_table`
  for it, an `InvalidArgument` naming the table and the column, whatever
  SDK the block was compiled with, so a block compiled before the SDK check
  of "Schema: every column a write names is declared" (below) is caught
  too. The fix is the same: add
  `.column(Column::datetime("updated_at"))` to its `TableDef` and build the
  block again. A block that creates rows needs `created_at` as well, or its
  first create fails.
- **Cloudflare's audit row reserves 3 D1 statements, not 4**, because no
  write introspects for a lazy column any more: the id policy probe, the
  column list and the insert. So the lowest
  `IMPRESSPRESS_D1_QUERIES_PER_INVOCATION` a deploy may state is now 4. A
  D1 `count`, `sum`, `aggregate`, counted delete or increment on a table
  that nothing else in the isolate has read sends one more statement than
  before, the column list the name check reads; it is cached per isolate.
  Two cases read it twice, because the check re-reads an uncached list
  before it refuses: a checked operation (a filter, sort or guard, or one
  of those just named) against a table that does not exist, whose empty
  list is never cached, and one naming a column the table lacks on a cold
  cache. Both fail anyway; they cost one more statement first.

**Who notices.** Nobody, where every write path is declared: "Schema: every
column a write names is declared" (below) declared every one the test suites
and the strict-mode survey before it found. Code of your own (an application block, a sandbox block) that writes
a column its migrations do not declare now fails that write on native and in
the browser too. Declare the column in a migration; on a database where an
earlier build already created it lazily, see "Adding a column a write may
already have created" in `migration_helper`'s module docs. Cloudflare
behaves as before apart from the checks and counts above.

### Schema: every column a write names is declared (auth 017, dev 005, tickets 002, vector 002)

**What was wrong.** The database layer stamps `created_at` on every typed
insert and `updated_at` on every typed write. Some tables never declared
the columns a write names. Native and browser databases created such a
column on the first write that named it (lazy column-add, which ran
wherever strict schema was off, until the entry above made strict the only
behaviour). Cloudflare ran strict schema, so there the write failed:

- **Password reset, on every Cloudflare deployment.** No migration declared
  `users.reset_token` or `reset_token_expires`. Forgot-password stored
  nothing, sent no mail, and still gave its generic success answer (an
  address must not be told apart by the answer): the visitor saw no
  failure, and only the Worker's error log recorded one.
- **Tickets, on a Cloudflare build that enables `block-tickets`** (the
  `full` preset). Every audit event failed, and so did the "audit
  degraded" flag that is meant to record such a failure. A public
  submission answered 201 with no audit trail and no error anywhere. A note,
  an analysis, a status change's expiry and the maintenance pass's status
  row failed too.
- **Vector, on a Cloudflare build that enables `block-vector`.** Renaming a
  legacy mixed-case index wrote `registry.updated_at`.
- **The dev sandbox** (browser only) and the sandbox's `table` block
  template relied on lazy column-add for `updated_at`.

**What changes.**

- Four new migrations declare the columns: auth 017 (`reset_token`,
  `reset_token_expires`), dev 005 (`updated_at` on `generations`, `builds`
  and `seed_info`), tickets 002 (`updated_at` on `events` and `analyses`,
  `created_at` and `updated_at` on `maintenance`) and vector 002
  (`updated_at` on `registry`). Password reset, the tickets audit trail and
  the vector rename work on Cloudflare from this release on.
- **Tickets: a failed audit write still degrades, and a failure to record
  that now fails.** An audit event that cannot be written still leaves the
  ticket saved and sets the "audit degraded" flag. If the flag itself cannot
  be written, nothing would record the gap, so the request now fails with
  the database error instead of answering success. A maintenance pass that
  cannot write its status row reports `"status"` among its errors and
  answers 503, so a scheduler retries. Neither falls back to creating the
  singleton row any more; tickets' 001 creates it.
- **Sandbox blocks: `db::ensure_table` refuses a table without
  `updated_at`.** The guest SDK refuses it with `InvalidArgument`, naming
  the column, so a block fails when it is activated instead of on its
  first write. The `table` template and the reference page declare
  `created_at` and `updated_at`. A block that is already compiled keeps its
  old SDK until it is compiled again (since the entry above, the host
  refuses it at activation too); then add
  `.column(Column::datetime("updated_at"))` to its `TableDef`.
  `ensure_table` adds a declared column that an existing table lacks.
- The test fixtures run with strict schema (every `TestContext`, and every
  database the browser crate's `test_support::fresh_db` hands out), so a
  tested write naming an undeclared column fails its test. Native and
  browser deployments ran without it until the entry above.

**Two populations, one end state.** On native and in the browser, a
database that exercised these paths already holds lazily created stamp
columns. Examples: every sandbox that built or published, and every
profile with a ticket note or a maintenance pass. A database that did not
exercise them has no such columns. Both have the same record, so each
file is written to leave both the same:

- **SQLite, D1 and the browser:** dev 005, tickets 002 and vector 002
  rebuild their tables. Each creates a `_v2` table with the declared
  columns, copies the rows by column name, drops the old table and renames
  the new one. No foreign key references these tables. This has two effects
  nobody asked for, both silent:
  - Any other column that a write created lazily on those seven tables is
    dropped with its values. A column no migration declares is not schema.
  - `updated_at` on existing rows is set from the row's own data:
    `created_at` for generations, builds, events and analyses;
    `imported_at` for the seed record; the last pass's time for the
    maintenance row; and NULL for registry rows, which have no time of
    their own. This replaces any value a lazy write stamped. The
    maintenance row's `created_at` is NULL.
- **PostgreSQL:** the same columns are added with `ADD COLUMN IF NOT
  EXISTS`, and an `UPDATE` then writes the values listed above. Lazy
  column-add created exactly `<column> TEXT`, nullable with no default, so
  the declared column and the lazily created one are the same column. A
  test pins the type, nullability and default on a lazily created
  database.
- **The vector `model` index is renamed** to
  `idx_impresspress__vector__registry_model` on both dialects. On a vector
  database created before 2026-07-13, 001's name was already taken by the
  registry's earlier table, so that database now has the index too (see
  the ledger entry's "Upgraded but not equal to a fresh install").
- A fresh install runs the rebuilds over empty tables.

**Refused: a native or browser database that ever served a password
reset.** `users` cannot be rebuilt. Six foreign keys reference it with `ON
DELETE CASCADE`, so dropping the table would delete every credential,
session and grant. Auth 017 is therefore a plain `ADD COLUMN`. A database
that served a reset request while strict schema was off already holds both
columns. There 017 fails as one transaction, nothing of it runs, and auth
is refused by name: "its schema drifted: migration `017_reset_token_columns`
adds a column the database already has…". Every other block upgrades.

- **Who:** a native server's SQLite or PostgreSQL database, **including a
  `local-run` development database**, and a browser profile (the demo, a
  dev sandbox) on which anyone requested a password reset.
- **Cloudflare is not affected.** It has run strict schema since
  2026-07-17, so a D1 database created since then never stored a reset. An
  older D1 database that served a reset before then would be refused by
  `/_deploy/prepare`, and the current version keeps serving.
- **What you see:** a native server refuses to start with the message. A
  browser shows the stopped screen with the cause and **Reset local data
  and reload**, which erases only when chosen.
- **Remedy:** recreate the database (in the browser, Reset). To keep the
  data instead, drop the two columns and start again; 017 then adds them
  back empty:
  `ALTER TABLE wafer_run__auth__users DROP COLUMN reset_token;` and
  `ALTER TABLE wafer_run__auth__users DROP COLUMN reset_token_expires;`.
  Any outstanding reset link stops working either way.

**Cloudflare.** The next deploy's prepare runs the four files: two `ADD
COLUMN`s on `users`, and the rebuilds of the tickets and vector tables on
a Worker that compiles those blocks. Dev never runs on a Worker. The
fresh-install budget test now counts only the blocks
`impresspress-cloudflare` can compile, read from its `Cargo.toml`: a fresh
Worker database sent 480 statements at that release (481 since wafer-run
1272fd63, 483 since auth 018, 485 since legalpages 003 and 487 since admin
010, all above), within half of the default 1000.

### Cloudflare: workers-rs 0.8.7

**What changes.** The Worker crates build on `worker` 0.8.7 and
`worker-build` 0.8.7, which bring wasm-bindgen 0.2.129, js-sys and web-sys
0.3.106. The CLI pins `worker-build` 0.8.7 and CI's `wasm-bindgen-cli` is
0.2.129. The js-sys fork that keeps the executor alive after a CPU kill (the
entry below) is rebased onto 0.3.106: the published crate plus
wasm-bindgen/wasm-bindgen#5379 at its head `555c2586`, pinned by commit in the
workspace `Cargo.toml`.

- **A second reset path.** worker-build 0.8 compiles in wasm-bindgen's call
  guard: an exception that escapes any call into the wasm, a trap inside a
  poll included, makes the next call re-instantiate the module. Only an
  error the Rust code throws itself (a wrapped JS exception) is exempt; a JS
  exception from an imported function the Rust code does not catch becomes
  a "Critical error" and resets the instance too. It logs
  nothing; worker-build's shim still logs `Reinitializing Wasm application`
  for its own resets. A CPU kill throws nothing, so neither path sees one,
  and the fork stays.
- **The entry module imports `build/index.js`,** and so does the hasher's
  config. worker-build 0.8 still writes `build/worker/shim.mjs`, as an alias
  for configs written against 0.7.
- **`strip` is set by the Worker build.** wasm-bindgen 0.2.129 cannot build a
  `strip = true` profile for worker-build 0.8 (cloudflare/workers-rs#1014):
  the stripped module has lost the section that says it uses reference types.
  Every Worker build the CLI runs, and the `wrangler dev` build hook it
  generates, overrides it with `CARGO_PROFILE_RELEASE_STRIP=debuginfo`, and
  the CLI then removes the function-names section that keeps, so the upload
  stays the size it was (webmcp-demo: 2203.6 KiB gzip on 0.7.5, 2211.8 KiB on
  0.8.7). The release profile check no longer asks for `strip`. A
  `wrangler dev` build keeps the names.
- **Errors.** `worker::Error` is no longer `Send + Sync`, and a JS error that
  is not D1's prints as `"{name} [{code}]: {message}"`. Nothing in this
  workspace matches on either.
- **Tooling.** Node 22 refuses a worker-build 0.8 module (its catch wrappers
  use `try_table`) unless run with `--experimental-wasm-exnref`. Workers and
  `wrangler dev` run it as is.

**Who has to act.**
- **A consumer with its own `worker` dependency** must move it to
  `worker = "0.8"`: worker-build 0.8.7 refuses `worker` 0.7. A consumer that
  depends on `impresspress-cloudflare` by path (site) resolves two
  incompatible `worker` crates until it does, and stops type-checking.
- **The js-sys `[patch]` does not reach another workspace.** A Worker built
  outside this one uses stock js-sys unless it carries the same `[patch]`.
- **The first deploy after this release** builds and deploys a 0.8.7
  password-hasher Worker, and no 0.8.7 hasher has run on Cloudflare before
  it. It was checked locally (`wrangler dev --local`: bootstrap, sign-up and
  sign-in through its Durable Object). Deploy it with the default
  `--password-hasher deploy`, so `/_deploy/verify`'s hasher probe checks the
  new hasher before promotion, and sign in once afterwards.

### Migrations: recorded per file; a release runs only the files it adds

**What changes.**

- What a database has applied is recorded per file, in
  `impresspress__admin__applied_migrations`: the block, the file, the hash of
  its statements, when, whether it ran or was adopted, and for an adopted
  file the record it was adopted from. A release runs only the files that
  table does not record. The table is created by the runner the first time a
  block migrates.
- **Every file runs in one transaction with its record**, on every backend:
  all of it applies, or none of it does, and a file that fails stays
  unrecorded and is run again, whole, next time. On Cloudflare that is one
  D1 `batch()`. Nothing is tolerated: a statement that fails fails its file.
- Adding a migration no longer re-runs a block's earlier ones, so it signs
  nobody out and needs no one-off D1 budget. A migrating run of a block costs
  the pending files' statements, one ledger row per file, and the block's
  own few calls (creating the ledger table if missing, reading the ledger,
  writing the record); the first run of a block from before this release
  also writes one `adopted` row per file its record names, in one
  transaction.
- A settled deployment reads nothing new: a block whose recorded list matches
  the build's returns before touching the database, as before.
- The browser applies every block's pending migrations before it builds its
  runtime (the native server does so for admin's), so any block's refusal
  stops the boot by name.

**Two shipped files change in this release, before any record exists.**
Both had been written for the old runner, which re-ran whole lists and took
a duplicate `ADD COLUMN` as applied; neither can run as one transaction as
shipped. No database records a hash for either yet, so this is the one
moment they can change.

- **Auth 016** loses its leading `ADD COLUMN role`, which recreated the
  column for a re-run and was a duplicate on every database 016 had not run
  on, a fresh one included. It is now the copy into `user_roles` and the
  `DROP COLUMN`. A database that ran the old text has the same result and is
  adopted (below); nothing runs for it.
- **Admin 009** retires `block_settings.blessed_hash` instead of emptying it.
  Databases created before 2026-10-07 have the column and ones created since
  do not, under the same record, so the file must not depend on which it
  meets: on SQLite and D1 it rebuilds the table from the columns every
  database has, copied by name (five statements, one transaction); on
  PostgreSQL it is `DROP COLUMN IF EXISTS`. Every database ends with no
  `blessed_hash` column and every row's other values as they were. A
  database that ran the old text (which added and emptied the column) runs
  the new one too, and the column goes.

**Never edit a shipped migration's statements.** A build whose copy of an
applied file has a different hash refuses, naming the block, the file and
both hashes. Put the change in a new file. `migration-content-hashes.txt` in
`impresspress-core` records every shipped file's hash, and a test fails at
PR time when one moves. Comment lines are not hashed, but a database from
before this release is matched by the hash of its files' whole text, so a
comment edit needs the history table regenerated (below) and its diff
reviewed.

**Upgrading a database from before this release.** Each block's first
migrating run after the upgrade decides from what the block recorded:

- **Nothing recorded:** a fresh block; every file runs.
- **A list main carried** (every one since 2026-07-13, the exceptions below
  aside): it is looked up in the reviewed history table
  (`impresspress_core::migration_history`, generated from main's git
  history), which names the files that list had applied. Those are recorded
  as `adopted` without running, and the rest run. In practice: every such
  deployment runs admin's 009 (its first text is not counted as applied);
  one last migrated before 2026-10-22 also runs auth's 016; and any block
  further behind runs the files it lacks.
- **Unsupported, refused by name with nothing changed:** any list from
  before 2026-07-13 (tables were `solobase__*`); auth before 2026-09-06
  (its sessions table predates 012); products before 2026-07-21 (001's
  bodies were corrected); dev's 2026-10-03 first text; signal's 2026-09-13
  first text; admin's 2026-10-06 first 007; on PostgreSQL, legalpages' 002
  and products' 015 first texts. The message names the block, the stored
  hash, the state's date and the reason.
- **Unknown, refused by name with nothing changed:** a record main never
  carried — `applying` (left by a run that crashed before 2026-10-07), a
  branch build's list, a corrupt row.
- **Upgraded but not equal to a fresh install.** A database that reached a
  supported list straight from one before its block's horizon carries that
  list's record over a schema the list never fully built. The runner decides
  from the record alone, and these databases share their record with
  databases that are whole, so they are adopted with the gap
  (`migration-history-crossing.txt` records each case):
  - **auth, the one that breaks sign-in:** a browser profile last opened on
    an auth list from before 2026-09-06 and next opened on 2026-10-22's list
    or later (`e73b4cc2`; PostgreSQL `43907ff1`) keeps the sessions table
    keyed on the access token. It is adopted, nothing runs, and no device can
    stay signed in on it.
  - files, messages and legalpages databases created before 2026-07-13
    lack indexes their `001` names (the names were taken by the
    `solobase__` tables then). Queries are slower; nothing fails. Vector
    databases of that age lacked one too, until vector's 002 recreated it
    under a new name.
  - a signal database created on signal's 2026-09-13 first text and next
    migrated on today's list lacks `rooms.id` and `rooms.updated_at`. Writes
    to rooms fail.
  - on PostgreSQL, a products database from before 2026-07-21 keeps
    `disputes.livemode` in an older declaration.
  The remedy is to recreate the database. In the browser these profiles
  start, so the boot shell's **Reset** is not offered: clear the site's data
  in the browser's settings.
- **Drifted:** a file that meets a column a typed write already created (a
  browser profile that wrote with newer code before it migrated, possible
  on native and in the browser until strict schema became the only
  behaviour) is refused as drifted, its file rolled back.

**Remedy for a refusal.** Nothing is ever erased by the runner.

- **Native:** the server refuses to start with the message. Restore a backup
  taken before the upgrade, or recreate the database.
- **Cloudflare:** `/_deploy/prepare` fails, the deploy stops before
  promotion and the current version keeps serving; the message is in the
  deploy report. Remedy as native.
- **Browser:** `initialize()` rejects with an error named
  `MigrationRefused`; the boot shell shows the cause and **Reset local data
  and reload**, which erases only when the person chooses it, even in a
  build that erases on recovery.

A migration that **fails** rather than being refused — a statement the
database rejects on the data it holds — is the ordinary error it always was:
the file rolls back and runs again next time. In the browser it is the
`initialize` stage, and a build rendered to erase on recovery (the demo, with
`OPFS_WIPE_ON_RECOVERY`) erases the local database and starts afresh, by
design; every block's list now runs before the runtime is built, so this
applies to every block, not only admin.

**Rollback.**

- **Native:** an older binary that records migrations per file refuses to
  start ("migrated by a newer build", naming the files it does not ship). A
  binary from before this release ignores the record and re-runs its own
  list over the schema it finds: its old 016 fails its `DROP COLUMN role`
  on a database that already dropped it, and its old 009 adds
  `blessed_hash` back, so such a rollback fails on auth and is not
  supported.
- **Cloudflare:** `wrangler rollback`, or promoting an older version, serves:
  the request path never plans migrations, it logs `schema drift` and runs
  on the schema it finds. A version from before 2026-10-07 fails its
  `Init` without `blessed_hash` (its upsert names the column). Redeploying an
  older build through `impresspress deploy` runs its prepare: one that
  records per file fails the deploy as "migrated by a newer build" and the
  current version keeps serving; one from before this release re-runs its
  list, as natively.
- **Browser:** an older bundle that records per file refuses and stops
  without erasing; one from before this release re-runs its list, as
  natively.
- **Remedy for "migrated by a newer build":** downgrading across migrations
  is not supported. Run the newer build again, or restore a backup taken
  before the upgrade. Deleting ledger rows is not a remedy: the next newer
  build would run those files again, or refuse the record as having a gap.

**Tracking lost.** A block whose `block_settings` row was lost on its own
recovers: its files are still recorded, nothing runs, and the row is written
again. If both its row and its ledger rows are gone while its schema is
still there, the block looks fresh, its first file meets the existing schema
and is refused as "its tracking was lost", rolled back. Restore
`impresspress__admin__block_settings` and
`impresspress__admin__applied_migrations` from a backup taken before the
loss, or recreate the database.

**Cloudflare budget.** A fresh database's `/_deploy/prepare` sent 454
statements for its migrations at this release, 480 since the
stamped-column migrations, 481 since wafer-run 1272fd63, 483 since auth
018, 485 since legalpages 003 and 487 since admin 010 (counted by a test
that keeps it within half of the default budget of 1000). Leave
`[cloudflare].d1_queries_per_invocation` at its default; 50 is too low for a
fresh install.

**Older entries this makes false** say so where they stand: the JWT
blocklist entry's whole-list cost, the "a revoked device stays revoked"
entry's "D1 budget of 1000", the 016 entry's "Cloudflare: this deploy needs
a D1 budget of 1000" and its 016 text, the 009 entry (the column is now
dropped, not emptied, and the next deploy re-runs nothing), the native
auto-migration entry's "Recovery" and "Auth caveat", and the browser entry's
"any edit to the auth migration SQL still re-runs the auth set".

### Auth: the JWT blocklist is retired

**What changes.** Logout and revoking the device you are on no longer write
the access token's id (`jti`) to `wafer_run__auth__jwt_blocklist`, and no
request reads it. The blocklist had become a second record of the same end:
both of its writers delete the device's row first, and an access token
authenticates only while its device's row is live (the entry below), so a
signed-out token is refused without it. Signing out, revoking a device and
changing a password end access tokens exactly as before. Access tokens no
longer carry a `jti`; one issued before this release still has one, and it is
ignored. The retention sweep stops pruning the table, and its result (and the
Cloudflare cron's log line) no longer reports `jwt_blocklist_deleted`.

**Cost.** Every request carrying an access token sends D1 one statement fewer:
its device row, plus its account's sign-in version when that is not cached.
With the entry below, which added the device read, the two together add
nothing per request.

**The table stays, unused.** Dropping it is an auth migration, and any change
to the auth migration list re-runs the whole list at the D1 budget of 1000
(see the 016 entry). The drop waits until migrations are recorded per file.
Until then the table keeps whatever rows it had; nothing reads them, they
name tokens rather than hold them, and they stay out of every data export.
*Since "Migrations: recorded per file"* (above), adding an auth migration
runs only that file, so dropping the table no longer costs the whole list.
*Since auth 018* (above), the table is dropped.

**Deploying this release** adds no migration, re-runs nothing and signs
nobody out.

### Auth: revoking a device ends its access token; a password change ends every device and API key

**What changes.**

- **Revoking a device ends its access token at once.** An access token now
  authenticates only while its device, the row on **Account → Sessions**, is
  signed in. Before, revoking a device stopped its refresh token, but the
  access token it already held kept working until it expired: 30 minutes by
  default (`WAFER_RUN__AUTH__ACCESS_TOKEN_LIFETIME_SECS`, at most 24 hours).
  The same holds for every other way a device ends: logout, detected reuse of
  a stolen refresh token, a password change, and expiry.
- **Changing or resetting a password signs out every device and revokes the
  account's API keys.** Before, the devices' refresh tokens were revoked and
  their access tokens retired, but the devices stayed listed as signed in,
  and API keys kept working. A key is checked against its own row and its
  owner's status, not the account's devices or sign-in version, so a key
  minted by whoever held the old password survived a password reset. A key
  revoked this way keeps its row, marked revoked (`revoked_at`) exactly as a
  key revoked by hand is; create new keys after the change. A key-creation
  request that was already under way when the password changed is refused,
  and the key it wrote is revoked, as is one made on a session someone signed
  in with the old password while the change ran: creating a key checks the
  session (its device and its sign-in version) or key that asked for it again
  once the key exists. The change ends the devices, then retires every token
  issued before it, then revokes the keys, which is the order that makes
  that check sufficient. An admin changing a user's
  roles still leaves that user's keys alone.
- **A change or reset that could not sign the devices out or revoke the keys
  answers an error,** not success. A reset gives the link back, so retrying it
  finishes the job. A change is retried by signing in with the new password
  and changing it again.

**Cost.** Every request carrying an access token reads its device row: one
more D1 statement per request, read in the same round trip as the token's
other checks, so it adds no latency. It is not cached, because a revocation
must take effect on the next request. The JWT blocklist's retirement (the entry above)
takes one statement away again.

**Deploying this release** adds no migration and signs nobody out, with one
exception: a device whose row an earlier auth upgrade emptied, and which has
not refreshed since (see the previous entry), is now signed out at its next
request rather than at its next refresh.

### Auth: a revoked device stays revoked, even mid-refresh

**What changes.** A signed-in device is a row in `wafer_run__auth__sessions`,
the list at **Account → Sessions**, and that row is now what keeps the device
signed in.

- **Revoking a device while it refreshes its tokens ends it.** Before, a
  refresh that was in flight when the device was revoked could finish
  afterwards, hand the device a working refresh token and put it back on the
  list. Now a refresh whose device was revoked meanwhile is refused (`401`,
  "Refresh token has been revoked"), and any token it had already minted is
  revoked with the rest of the device's tokens. Logout follows the same rule
  for every device at once.
- **A device that has lost its row is signed out at its next refresh**
  instead of being silently listed again. Rows go when a device is revoked,
  when the user logs out, when reuse of a stolen refresh token is detected,
  and when they expire, all of which end the device anyway. An auth schema
  change no longer removes them (below).
- **A sign-in writes its refresh token and its device row in one write.** If
  either cannot be written, the sign-in fails with a `500` and leaves nothing
  behind. Before, a failed device row was logged and the sign-in succeeded
  with a device missing from the list. On Cloudflare this also takes one D1
  round trip off every sign-in (four, from five).

**An auth schema change no longer empties the device list.** The auth
migrations re-run as a set whenever any of them changes, and migration 012
opened by dropping and recreating `wafer_run__auth__sessions`, so every such
change emptied the list. Until now the list refilled as devices refreshed;
with the row now keeping the device signed in, it would have signed every
device out. The table's layout is now declared by migration 001, and 012 no
longer drops it, so device rows survive, as refresh tokens already do.

**Deploying this release re-runs the auth migrations once,** because 001 and
012 changed. On a deployment already on this layout (every one that applied
012, from September 2026) the re-run changes no data and signs nobody out.

- **One exception:** a device whose row an earlier auth upgrade emptied (016
  is the latest) and that has not refreshed since. Its row was never
  re-created, so it is signed out at its next refresh and signs in again.
- **A database last migrated before 012** (last booted before September
  2026) still has the old `sha256(access_token)`-keyed table, which is no
  longer converted. Every sign-in on it fails with a `500` (before strict
  schema became the only behaviour, SQLite and the browser added the
  missing columns on first write instead, leaving the sessions table with
  no key on `family`). Recreate that database (pre-1.0 upgrades may require
  wiping local data).
- **Cloudflare: this deploy needs a D1 budget of 1000,** for the same reason
  as the 016 upgrade below. Re-running the auth list is 60 statements for
  001–016, more than a budget of 50.
  *Since "Migrations: recorded per file"* (above), a later auth migration runs
  only its own file.

Access tokens already issued still work until they expire, as before; ending
them at revocation is a separate change.

### Auth: only this site's own pages can sign a browser in

**What changes.** The requests that hand out a session — `POST
/b/auth/api/login`, `/b/auth/api/signup`, `/b/auth/api/refresh` and
bootstrap-token redemption (`/b/auth/api/bootstrap`) — refuse a request a
browser reports as coming from another origin, with the same `403` ("This
request couldn't be verified as coming from this site…") a cross-site
mutation already gets. Before this, a page on any other site could post the
attacker's own credentials to the login route from a hidden `<form>` and
sign the visitor's browser into the attacker's account: the cookie check that
guards signed-in requests does not apply to a request that carries no
session yet.

- **"Another origin" includes origins listed in
  `WAFER_RUN_SHARED__CORS_ALLOWED_ORIGINS`.** CORS still lets such a page
  read this site's API responses, but it can no longer sign users in at
  all: the login, signup or refresh is refused outright, so it gets neither
  the session cookie nor the tokens in the response body. A separate front end that signed users in cross-origin
  must now sign them in on this site's own origin. No client in this
  repository did; the storefront widget only reads products.
- **A request with no browser headers at all is unaffected** (no
  `Sec-Fetch-Site`, `Origin` or `Referer`): API clients, `impresspress-js`
  outside a browser, scripts, `curl`. There is no visitor's browser to forge
  such a request from.
- **The OAuth callback is unaffected.** The provider sends the browser back
  with a cross-site navigation by design; the single-use OAuth state is what
  protects it, as before.
- **Login, signup and refresh now require `Content-Type:
  application/json`**, and answer `415` otherwise. Every in-repo client
  (the sign-in pages, `impresspress-js`, the dev sandbox's entry page)
  already sends it. A script that posted JSON without the header needs to
  add it.
- A refused refresh does not spend the refresh token it presented, and a
  refused signup creates no account.

### Auth: a role is a grant row; the upgrade ends every session

**What changes.** An account's role lived in two places: the inline
`wafer_run__auth__users.role` column, written once when the account was
created, and the `impresspress__admin__user_roles` grant rows admin's IAM
manages. Every role check read both, but IAM could change only the rows, so
the first admin — whose `admin` was the inline column — could not be demoted,
and the Users page showed them with no role at all.

Now a role is a `user_roles` row and nothing else:

- Auth migration 016 copies every account's inline role into a grant (unless
  the account already holds it) and drops the column. Every existing admin
  keeps admin, as a grant.
- An account is created holding its first grant, in the same write as the
  account: `user` at signup (password or OAuth), `admin` for the admin
  created at first boot or by redeeming the bootstrap token. A later change
  to anyone's roles, the first admin's included, is an IAM assign or removal.
  The last admin still cannot lose it: an admin cannot remove their own
  `admin`, and the `admin` role cannot be deleted.
- The Users page and the admin users API now list every role an account
  holds, `user` included.
- An access token without a `roles` claim carries no role. Nothing in this
  codebase mints the older single `role` claim, and it is no longer read.

**Upgrading ends every session, once.** Adding 016 changes the auth
migration list, so the deploy or boot that applies it re-runs the whole list,
and its earlier migrations drop the refresh-token and session tables. No
refresh token issued before the upgrade works afterwards, so every user signs
in again once their access token runs out. The access tokens themselves are
not revoked: they are self-contained, and 016 does not bump the
`auth_version` they are checked against. One issued before the upgrade keeps
authenticating, with the roles it was minted with, until it expires: 30
minutes after it was issued by default
(`WAFER_RUN__AUTH__ACCESS_TOKEN_LIFETIME_SECS`, at most 24 hours). Nothing
else is lost.

**Correction: the paragraph above is wrong about the mechanism and the
outcome.**

- **Refresh tokens survive the re-run.** Migration 004 has not dropped
  `wafer_run__auth__tokens` since the 015 release.
- **The re-run emptied the device list.** At this release, migration 012
  still dropped and recreated `wafer_run__auth__sessions`. Issuance re-created
  a device's row at its next refresh, so applying 016 signed nobody out.
- **What happens now.** The release "a revoked device stays revoked, even
  mid-refresh" (above) makes that row what keeps a device signed in. It also
  moves the table's layout into 001, so 012 no longer drops it.
  - Applying 016 with that release or later keeps every device row and signs
    nobody out.
  - A deployment that applied 016 earlier keeps one effect: a device that has
    not refreshed since is signed out at its next refresh after upgrading.

**Cloudflare: this deploy needs a D1 budget of 1000.** `/_deploy/prepare`
applies a block's pending migrations in one Worker invocation, and every D1
statement counts against `[cloudflare].d1_queries_per_invocation`, a refused
duplicate `ADD COLUMN` included. A change to the auth list re-runs all of it,
not only the new file: 64 statements for 001–016, before prepare's own
reads. A site that set the budget to 50 is refused partway through the auth
list (around 010), prepare fails before 016 runs, and the deploy is not
promoted; the live version keeps serving. Deploy this release with the
budget unset (the default, 1000) or set to 1000. A site that wants 50 for
day-to-day traffic can redeploy with it afterwards: that deploy re-runs
nothing, because every migration list's hash then matches. Until migrations
are recorded per file rather than per list, any later change to the auth
list costs the whole list again.
*Since "Migrations: recorded per file"* (above), they are: a later change to
the auth list costs only its new file.

**API change.** `GET /b/admin/api/users` no longer returns `role` on each
user. Read `roles`, which lists every role the account holds — the same list
its session carries. The `@impresspress/sdk` types are regenerated without it.

**Dev data snapshots are version 2.** A `seed/data.json` exported before this
release (version 1) is refused on import: its users rows carry the dropped
column. Export the sandbox again from a build that has applied 016.

*Since "Migrations: recorded per file"* (above), 016 no longer opens with
`ADD COLUMN role`: it is the copy and the `DROP COLUMN`, one transaction. A
database that ran the earlier text is adopted and runs nothing.

**Database support.** 016 needs `ALTER TABLE … DROP COLUMN`: SQLite 3.35 or
later, which the native server, the browser build and Cloudflare D1 all are,
or any supported PostgreSQL.

### Auth: the bootstrap admin email grants nothing at sign-in

**What changes.** `WAFER_RUN_SHARED__AUTH__BOOTSTRAP_ADMIN_EMAIL` no longer
makes anyone an admin when they sign in. Before, an account with that address
was created as `admin` by password signup and by OAuth signup, and was granted
`admin` again at every password login, token refresh and OAuth sign-in. With
email verification off, whoever registered an unclaimed bootstrap address
first was admin. An IAM removal of that admin was undone at their next
sign-in.

Now every signup creates a `user`, and login, refresh and OAuth sign-in only
read the roles already stored. The first admin comes only from first-boot
provisioning (email + password on an empty users table) or from redeeming the
bootstrap token. Every later admin comes from an admin's IAM assign, and an
IAM removal stays removed.

**Who has to act.** An operator who set the email, and then signed up with it
to become admin, now gets a `user` account. Provision the first admin at first
boot with the email and password together, as every documented setup does, or
redeem a bootstrap token. An existing admin keeps their role: nothing is
removed by this change.

### Auth: the bootstrap email is first-boot only, and a bootstrapped admin can use OAuth

**What changes.**

- **An email-only first boot is refused.** On a first boot (no accounts yet),
  `WAFER_RUN_SHARED__AUTH__BOOTSTRAP_ADMIN_EMAIL` set without
  `..._BOOTSTRAP_ADMIN_PASSWORD` or `..._BOOTSTRAP_ADMIN_TOKEN` now fails the
  auth block's `Init`, so the server refuses to start and names the missing
  keys. Before, that boot created nobody, and the email was granted admin at its
  first sign-in, which no longer happens (entry above). Once any account
  exists, the bootstrap values are ignored, as before: a later settings edit
  cannot stop a running site from booting.
- **The email may be cleared after provisioning.** After the first boot only
  the dev sandbox reads it: its one-click entry, which falls back to the login
  page when it is empty, and its site export, whose README then names "the
  account you signed in with" instead.
- **A bootstrapped admin's address counts as proven.** The admin created at
  first boot (email + password) or by redeeming a bootstrap token is recorded
  as proven by the bootstrap (`email_verified_by = "bootstrap"`), in the same
  write as the account. So it can later sign in with Google or GitHub when the
  provider asserts that address: the OAuth callback joins the existing admin
  account instead of refusing it as one that never confirmed its address. A
  provider that does not assert the address is still refused.

**Who has to act.**

- **Configure an address you own.** The bootstrap trusts the address the
  operator configured, or the one the token's holder typed. If it is someone
  else's (a typo, say), that address's owner can sign in to the admin account
  through any provider that asserts it. Changing that admin's password does
  not stop this, and an account's address cannot be changed. If your
  bootstrapped admin has an address you do not control, retire it before
  OAuth is on (`WAFER_RUN_SHARED__ENABLE_OAUTH`, off by default; turn it off
  first if it is on):
  1. sign in as the bootstrapped admin with its password;
  2. grant `admin` to an account at an address you control:
     `POST /b/admin/api/iam/user-roles` with
     `{"user_id": "<its id>", "role": "admin"}`;
  3. sign in as that account and delete, or disable, the bootstrapped admin
     on the **Users** page (an admin may delete or disable any account but
     their own);
  4. only then turn OAuth on. A deleted or disabled account is refused at
     every OAuth sign-in, and a deleted one keeps its address, so the
     address's owner cannot sign up with it either.
- **An operator whose first boot set only the email** must also set the
  password, or a bootstrap token.
- **Existing bootstrapped admins** stay unproven: there is no data migration.
  An admin who wants to link a provider proves the address once with a
  verification link or a password reset (see the OAuth adoption entry below).

### Browser: an unreadable database stops with its cause instead of starting empty

**What changes.** A browser install whose stored database could not be read
(OPFS unavailable, or the file's read failed) used to start on an empty
database, and its first flush wrote that empty database over the stored one.
It now stops with a visible error at the new stage `open`, keeps the file,
and offers "Try again". Builds rendered with `--opfs-wipe-on-recovery` do not
erase for it either: only a database that was read and found unusable
(stage `initialize`) is erased there.

**Who has to act.** Nobody. The runtime and its `sw.js` must come from the same
release, which `impresspress build` guarantees: an older `sw.js` reports the
new error as stage `initialize`.

### Cloudflare: `--password-hasher keep`, and verify probes the hasher

**What changes.**
- `impresspress deploy --target cloudflare --password-hasher keep` deploys the
  main Worker only. The password-hasher Worker is neither built nor deployed.
  The deploy reads the hasher's current deployment
  (`wrangler deployments status --json`). It refuses when no hasher is
  deployed, or when the deployment splits traffic across versions. It then
  reads that version's vars (`wrangler versions view --json`) and refuses
  unless they are exactly the ones this release would deploy: a
  `[cloudflare.password_hasher]` change such as `pepper_required = true`
  takes a hasher deploy, and `keep` says so instead of reporting it applied.
  It prints the version it keeps. The default, `--password-hasher deploy`, behaves as
  before: the hasher is built and deployed first.
- In both modes, the final candidate's `/_deploy/verify` now sends the
  deployed hasher one probe: a `verify` request in the candidate's protocol
  version, against a stored hash that names no scheme. A passing hasher
  answers `malformed_hash` in the version it was asked in. That costs no
  argon2 work, and nothing is written. The answer is reported as the
  response's `password_hasher` field, e.g.
  `{"protocol": 1, "answer": "malformed_hash"}`. Any other answer makes
  verify return a 503 that names the reason, and the version is not
  promoted. So `/_deploy/verify` now calls another Worker.
- The `/_deploy/*` response schema (`DEPLOY_RESPONSE_SCHEMA_VERSION`) is now
  2.

**Who has to act.** Use a CLI and a Worker from the same release. A CLI from
before this release refuses a Worker from this one, and the reverse, with an
"unsupported … response schema" error. A deploy from this workspace builds
both, so it needs nothing.

### Cloudflare: a request stopped at its CPU limit no longer stops the isolate's later requests

**What changes.** Every request and every `wait_until` task in the Worker is
polled by one task queue per wasm instance, js-sys's. With js-sys 0.3.99, a
request that Cloudflare stopped in the middle of a poll for exceeding its CPU
limit could leave that queue never polling anything again: every later request
in the isolate then failed at once with error 1101 ("code had hung") at about
0 ms of CPU, until Cloudflare retired the isolate. This build links an interim
fork of js-sys 0.3.99 that carries the upstream fix,
wasm-bindgen/wasm-bindgen#5379 (`[patch.crates-io]` in the workspace
`Cargo.toml`).

Now the stopped request fails on its own. Two costs remain:
- A request in flight in the same isolate whose wake was pending when the stop
  landed waits for the next wake of any task. If none comes first, Cloudflare
  may report it as hung.
- Each stop leaks the stopped request's task, and the part of the wasm shadow
  stack it was using, for the rest of the instance's life.

The fix does not make the Free plan's 10 ms CPU limit fit. When Cloudflare
enforces that limit strictly, cold requests that build a runtime (300–700 ms)
are still stopped.

**Who has to act.** Nobody who deploys from this workspace: the next
`impresspress deploy --target cloudflare` picks it up. The password-hasher
Worker is built from the same `Cargo.lock` and ships in the same deploy. A
Worker built outside this repository's workspace does not inherit the
`[patch]`. It needs the same entry until a wasm-bindgen release includes the
fix.

### Cloudflare: concurrent cold requests share one runtime build; new 503s, and a deploy step

**What changes.** A cold request on Cloudflare that finds another request in
the same isolate building the runtime now waits for that build and serves it,
instead of building a runtime of its own (each one is 13–20 MB of Wasm memory
against the 128 MB isolate limit; enough of them at once killed the isolate).
The wait lasts at most one build lease (10 s). Two cases that used to build or
answer 500 now answer **503 with `Retry-After: 1`**:

- the build the request waited for failed (the build's own request still
  answers the 500; the next cold request retries the build), and
- the request waited once, re-checked, and found another build in progress.

A scheduled (cron) run that lands in either case logs the error and skips
that run's sweep until the next schedule.

`impresspress deploy --target cloudflare` now reads the final candidate's
per-isolate memory from Cloudflare's Workers analytics after the concurrency
smoke and prints it (peak `wasmMemoryBytes` and `memoryUsageBytes` by request
status, against the 128 MB limit). It runs `wrangler auth token --json` for
the credential and polls the analytics for up to about 3 minutes while the
smoke's requests arrive. The step is informational: an unreadable token or
an analytics error prints a note and the deploy continues.

**Who has to act.** Nobody. A client or monitor that treats any 503 from a
cold isolate as an outage should honour `Retry-After`. A deploy run under a
credential with no bearer token (an API-key login) skips the memory report.

### Migrations: an older version still serving no longer re-applies its migrations (admin migration 009)

**What changes.** Admin migration 009 empties `blessed_hash` on every row of
`impresspress__admin__block_settings`, adding the column first on a database
created since it was retired. Nothing in this build reads it. A version built
before the native auto-migration release (2026-10-07) gated each block's
migrations on that column: on Cloudflare, while it kept serving during a
deploy — or for good, after a failed one — its next cold build after the new
version recorded a list re-applied its own whole list for every block it had
blessed. That overwrote the new version's records of what is applied, bumped
the KV config stamp (so the candidate's prepared plan went stale before
promotion) and re-ran old data repairs; had auth's list changed, it would have
signed every user out. It happened once each time the versions diverged, and
on every cold build while the re-apply kept failing part way (as it does when
it runs out of D1 statements). With the column empty, such a version logs
`schema drift` and serves on the schema it finds.

**When it takes effect.** Once a build carrying it has applied admin's list:
on Cloudflare at that deploy's `/_deploy/prepare`, natively at the next boot.
An older version serving before then still re-applies. Adding 009 changes
admin's list, so that deploy or boot re-runs admin's whole list (001-009);
every file in it is re-runnable. Rolling back to an older version with its
migrations applied blesses its rows again; the next deploy of this build
re-runs admin's list, because the older one re-recorded it, and empties them
again. One narrow case is not covered: an older version that receives its
first traffic while the deploy is applying 009 can read the rows from before
009 ran (D1 mid-deploy, or a stale KV copy in another location) and bless a
block other than admin again; nothing then re-runs 009 until admin's list
next changes.
*Since "Migrations: recorded per file"* (above), 009 is a different file:
it drops `blessed_hash` instead of emptying it (a table rebuild on SQLite and
D1), so a version from before 2026-10-07 can no longer write the column at
all — its `Init` fails instead of re-applying. 009 is recorded once applied
and never runs again.

**Who has to act.** Nobody. The guarantee that a still-serving older version
cannot change the schema holds only between versions on the same migration
protocol: keep traffic off an older version during a deploy where you can,
which also closes the case above.

### Branding: the built-in mark is the clay character — no action required

The square mark and favicon are now the clay character impresspress.org
uses, replacing the pixel-art crab: `impresspress-logo.png` (32px),
`impresspress-logo-2x.png` (64px) and `favicon.ico` (16/32/48), all
downscaled from one master by `scripts/brand/make-clay-mark.py`. Brand text
stays text; there is still no raster wordmark (see "the built-in raster
wordmark is gone" below).

**Stored URLs repair themselves.** The built-in URLs carry a content hash,
and a deployment holds the old ones in `WAFER_RUN_SHARED__LOGO_ICON_URL` and
`WAFER_RUN_SHARED__FAVICON_URL`. `seed_defaults` rewrites a row naming a
`/b/static/` file this build does not serve to the current default on the
first boot after the upgrade, and logs a warning with both values. A URL of
your own is never touched.

**The mark is no longer pixel art,** so the `.pixel-art` class
(nearest-neighbour scaling) is gone from the stylesheet, and the built-in
icon renders as one `<img>` with the 64px file as its `2x` `srcset`
candidate rather than a `<picture>`. The sign-in panel's icon gets that
candidate too. A white-labelled `LOGO_ICON_URL` renders as before.

**SDK (`@impresspress/sdk`):** `static/logo.png` and `static/favicon.ico` carry
the new art, byte-identical to the server's.

**`impresspress-web`:** its web app manifest's icon pointed at
`/b/system/api/favicon`, which nothing serves; it now lists `icon-192.png`
and `icon-512.png`, shipped beside the manifest.

### Browser: every page loads the model engines; `EMBEDDED_SCRIPTS` is the operator's alone

**What changes.** The browser runtime's pages load all three page-side model
engines (`/webllm-engine.js`, `/embed-engine.js`, `/t2i-engine.js`), from the
list the bundle ships them with (`impresspress-bundle`'s `PAGE_ENGINES`,
handed to the runtime as `initialize({ pageEngines })`). Before, they loaded
only the LLM engine, through a `WAFER_RUN_SHARED__EMBEDDED_SCRIPTS` value the
browser seeded, so an embedding or image request from a page under the
service worker was never answered. A model request now goes to the open page
that holds its model, and is refused at once, with a reason, when no page can
run it.

**Leftover row.** A browser database seeded by an earlier version still holds
`WAFER_RUN_SHARED__EMBEDDED_SCRIPTS = /webllm-engine.js`. It is harmless — a
script the runtime already loads is not rendered twice — but it is no longer
needed: unless it also lists scripts of your own, edit it to an empty value on
the Variables page (a shared variable cannot be deleted; empty means unset).

**Bundle and runtime together.** A `sw.js` built by an earlier
`impresspress-bundle` does not pass `pageEngines`; a runtime booted by one
logs a warning and its pages load no engine, so every model request is
refused. Rebuild the bundle with the matching CLI.

### Content types: one table, and the dev sandbox no longer stores them

**What changes.** Every server path types a file by its extension through
one table, `wafer_core::mime`, and every textual type there declares
`charset=utf-8`. That covers the native and browser runtimes, the dev
sandbox publisher and its export, `impresspress serve --target web` and the
Cloudflare release-asset upload. Types that change: `.md` is `text/markdown;
charset=utf-8` (the sandbox published it as `text/plain`), `.json`, `.map`
and `.svg` gain the charset, `.xml` and `.csv` are typed (they were
`application/octet-stream`), and `impresspress serve --target web` no longer
sends `.txt`, `.md` and other text files as `application/octet-stream`.

**Dev sandbox API, breaking.** A file's type is no longer stored: it follows
from its path. `GET /b/dev/api/files`, `POST /b/dev/api/files/write-batch`
and `POST /b/dev/api/blocks` no longer carry `content_type` on each file,
and the generations API's manifests do not either. A seed bundle's
`manifest.json` declares no `content_type`. One that a bundle exported by an
earlier version still declares is ignored: the file gets the type its path
derives.

**Dev sandbox data, one-way.** The first boot of this version rewrites what
an existing browser sandbox stored. `workspace.json` and every generation
lose the `content_type` on each file, and the live site is published again
so each file carries its new type. Until then a stored type is read and
ignored. An older runtime cannot read the rewritten data, so rolling the
runtime back breaks a sandbox this version has booted. The recovery is a
fresh sandbox: clear the site's data in the browser, or open it in a new
profile. If the republish fails, the browser console says so. The sandbox
keeps working, with its active site and blocks, and the next boot tries
again.

### Storage: the object listing's `prefix` is case-sensitive everywhere

**What changes.** `GET /b/storage/api/buckets/{name}/objects?prefix=` lists
the objects whose key starts with the prefix, with letter case significant
on every backend: object keys are case-sensitive, so `a/B` no longer lists
`a/b.txt` on SQLite and Cloudflare (D1), as it did before. PostgreSQL
deployments already matched case. Every character of the prefix is literal
(`%`, `_`, `*`, `?`, `[` and `\` match only themselves).

**Blocks.** A block's filter can use the new `starts_with` operator for the
same prefix match.

### Files API: listings publish views, not stored rows — a breaking change for API clients

**What changes.** The files block's record listings answered the stored
row, so its bookkeeping stamps went out with it. They now answer a view of
what the record means to the caller. The `{ records: [{ id, data }],
total_count, page, page_size }` envelope is unchanged, and so is every field
the SDK reads. These fields are no longer in `data`:

- `GET /b/storage/api/search`: `created_at` and `updated_at` (`uploaded_at`
  already says when).
- `GET /b/storage/api/recent`: `created_at` and `updated_at` (`viewed_at`
  is the instant; an entry is never modified).
- `GET /b/cloudstorage/shares` and `GET /b/cloudstorage/admin/shares`:
  `updated_at` (a share is never modified; `created_at` stays).
- `GET /b/cloudstorage/admin/access-logs`: `created_at` and `updated_at`
  (`accessed_at` is the instant).

The quota listings and the quota update answer the same fields as before.
The SDK's `FileViewRecord` no longer has `created_at` or `updated_at`. Drop
those fields from anything that reads them. No migration is involved.

### Search: the text is literal and ASCII case is ignored everywhere

**What changes.** The admin user search, the audit-log resource filter, the
request-log path search, product search and file search keep a row when the
field contains the typed text. `%` and `_` used to act as wildcards in the
admin and log searches (`a_b` matched `axb`); they now match only
themselves, as does `\`. ASCII letters match in either case on every
backend; PostgreSQL deployments used to need the exact case. The storage
object listing's `prefix` filter is not a search; see "Storage: the object
listing's `prefix` is case-sensitive everywhere".

**Blocks.** A block's filter can use the new `contains_ignore_case`
operator for the same search; `like` still takes a pattern.

### Native: pending migrations apply at every boot; `--run-migrations` is gone

**What changes.** `impresspress serve` (and any server built on
`impresspress_server::run`) applies every block's pending migrations before
it binds its socket, and refuses to start if any block fails to initialize —
a failed migration names the block, the file and the statement. A list whose
hash is recorded in `impresspress__admin__block_settings` is not run again.
Two processes booting against one database take turns (a PostgreSQL advisory
lock; on SQLite an OS file lock on `<database>.migrate.lock`). A
native PostgreSQL server migrates in the PostgreSQL dialect, chosen by
`IMPRESSPRESS_DB_TYPE`. Drop `--run-migrations` from scripts: it is now an
unknown-argument error.

**Recovery.** Fix the cause and start again: the failed list re-runs from its
first file. To roll back instead, start the previous binary; it re-runs its
own list.
*Since "Migrations: recorded per file"* (above): a failed boot runs again
from the file that failed, not from the list's first; and a previous binary
that records migrations per file refuses to start on a database a newer one
migrated. Only a binary from before that release re-runs its own list.

**Auth caveat.** Any change to auth's migration list re-runs it on the first
boot that applies it. When this was written, that was said to sign every user
out. It did not: refresh tokens had survived a re-run since the 015 release,
and the device list that migration 012 emptied refilled as devices refreshed.
Since the release "a revoked device stays revoked, even mid-refresh", 012 no
longer empties it either, so a re-run signs nobody out.
*Since "Migrations: recorded per file"* (above), a change to auth's list
runs only its new files.

### Browser builds: request log defaults to errors only

**What changes.** A browser build (a bundle's service worker, the dev
sandbox, an exported site) now keeps `request_logs` rows for server errors
(5xx) only — `IMPRESSPRESS_REQUEST_LOG=errors` — where it used to keep a row
for every request. The row a request does keep is written after its response
has gone out, as on Cloudflare, rather than on the way to it.

**Why.** The browser database persists by exporting the whole file to OPFS on
every flush. A row for every request grew that file, and so every later flush,
without bound, and each row cost an export of its own. A browser install is
one person's local instance: the error rows are what its log is for.

**Overriding it.** There is nothing to set in the browser. The policy is
infrastructure config written by the browser runtime factory
(`crates/impresspress-web/src/runtime_factory.rs`), not a variable: no admin
setting reaches it, and the rows an older bundle wrote stay until they are
deleted.

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
  (`--password-hasher keep` leaves the deployed hasher in place instead; see
  "Cloudflare: `--password-hasher keep`, and verify probes the hasher".)
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
*Since "Migrations: recorded per file"* (above), a new migration runs only
its own file, and an edit to an applied one is refused rather than re-run.

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

- The strict-schema switch (removed since; see "wafer-run 1272fd63" above)
  and the network limits (`WAFER_RUN__NETWORK__MAX_RESPONSE_BYTES`,
  `…__CONNECT_TIMEOUT_SECS`, `…__READ_TIMEOUT_SECS`,
  `…__REQUEST_TIMEOUT_SECS`, `…__STREAM_TIMEOUT_SECS`) reach their blocks
  through each block's start config. Native resolves them from the process
  environment beneath the variables table; Cloudflare read the switch from
  its Worker var as before. An invalid network limit now fails the `wafer-run/network` block's
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

### Dev sandbox: blocks depend on the `wafer_guest` crate

**What changes.** The guest SDK is a crate, `crates/wafer-guest`, instead of a
module copied into every block. A scaffolded block is two files: `Cargo.toml`,
whose one dependency is `wafer_guest = { path = "../../wafer_guest" }`, and
`src/lib.rs`, which says `use wafer_guest::*;` and
`wafer_guest::export!(block, init);`. The compiler builds the crate once when
its session starts (about 30 seconds) and then rebuilds only the block, so a
compile takes a few seconds instead of about 20. `GET /b/dev/api/guest` serves
the crate; the reference no longer carries `wafer_guest_module`. An export
archive now carries `seed/wafer_guest/` beside `seed/blocks/` whenever it has a
block. The compiler dist moves to `807ace9e.2`.

**Your blocks.** Nothing breaks. The guest ABI version is unchanged at 2, so a
block already compiled keeps serving, and a seed archive that carries one
still imports. A block scaffolded before this release — three files, with its
own `src/wafer_guest.rs` and `mod wafer_guest;` — still compiles, just slower:
it rebuilds its copy of the module on every compile, and staging still checks
it against the version its own vendored copy states, so a copy older than
version 2 is refused with `wafer-guest-version` as before.

**What to do.** Nothing is required. To get the fast compile for an existing
block, either scaffold it again and move your code across, or edit it in
place: in `Cargo.toml`, add `wafer_guest = { path = "../../wafer_guest" }`
under `[dependencies]` and set `lto = false` in `[profile.release]` (cargo
builds the guest crate with the block's profile, so a block whose profile
differs from the scaffolded one pays a guest rebuild of about 30 seconds); in
`src/lib.rs`, replace `mod wafer_guest;` (with its `#[cfg]` line) and
`use crate::wafer_guest::*;` with
`use wafer_guest::*;` and `wafer_guest::export!(block, init);`; then delete
`src/wafer_guest.rs` and compile again. Re-export any seed bundle afterwards
if you want it to carry the new layout.

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
`wafer_guest_module`, and a newly scaffolded block has it (superseded by the
entry above: the module is now a crate the sandbox serves, and a block depends
on it by path) — then compile and stage again. The block's own files are
unchanged. Re-export any seed bundle afterwards.

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

- `WAFER_RUN_SHARED__AUTH__BOOTSTRAP_ADMIN_EMAIL` — read only on the first
  boot, to provision the admin together with the password. No sign-in grants
  a role from it (see "the bootstrap admin email grants nothing at sign-in"),
  so a stale value is not a back door; you may clear it once you have your
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
signing in to fix it needs the credentials you were replacing. They are still
in the database, in plaintext; read them there and sign in with them, then
change the password at **Account → Security**:

```sql
SELECT key, value FROM impresspress__admin__variables
 WHERE key IN ('WAFER_RUN_SHARED__AUTH__BOOTSTRAP_ADMIN_EMAIL',
               'WAFER_RUN_SHARED__AUTH__BOOTSTRAP_ADMIN_PASSWORD');
```

If the stored email is an address you do not control, the password change is
not enough: that admin's address counts as proven (see "the bootstrap email is
first-boot only" above), so the address's owner can sign in to it through any
OAuth provider that asserts the address. Before OAuth is on, grant `admin` to
an account at an address you control, sign in as it, delete or disable the
bootstrapped admin, and only then turn OAuth on.

Releasing the email key does not help here: once the admin exists, the
bootstrap values are never read again, and no sign-in grants admin from the
email.

Release any other key without logging in by writing the released marker
directly in the database (`impresspress__admin__variables`), then restarting:

```sql
UPDATE impresspress__admin__variables
   SET updated_by = 'released-to-environment'
 WHERE key = 'WAFER_RUN_SHARED__ENABLE_OAUTH';
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
boot. On that release an upload of a new key still worked there and added the
column (strict schema was off by default on native). Replacing an object
answered `500` until the column existed — the take-over checks the row's
`claim_id`, and a condition never adds a column. Since strict schema became
the only behaviour (see "wafer-run 1272fd63" above), every upload fails on the
missing column until the migration runs, and every target runs it before it
serves.

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

**Without the migration, and during a rollout.** As for 004: on that release a
native deployment added the column on the first upload; since strict schema
became the only behaviour every upload fails until 005 runs, and every target
runs it before it serves. While a rollout is part-way, an isolate still
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

**What to set.** In Admin → Settings → Products, set **Platform country** to
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

**SDK (`@impresspress/sdk`):** `IMPRESSPRESS_ASSETS.logoLong` and
`static/logo_long.png` are removed — a breaking change for any consumer that
referenced them. `IMPRESSPRESS_ASSETS.logo` (the square mark) and
`favicon.ico` are unchanged in name and now carry the new art.

### Email: `WAFER_RUN_SHARED__SITE_URL` is gone, and mail is sent under your App name

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

**The default sender's display name is your App name.** With
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
one in `WAFER_RUN_SHARED__AUTH__BOOTSTRAP_ADMIN_EMAIL`. That account is a plain
user, as every sign-up is, and it is created unproven, so it cannot be adopted by
anyone; what it does is occupy the address, and the real owner then meets "an
account already uses this email address" when they try to link their own
provider.

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
warning for the llm block on each boot; on that release providers kept
working, because a native deployment then added the column on the first
provider write. Since strict schema became the only behaviour (see "wafer-run
1272fd63" above), a provider create or edit fails on the missing column until
the migration runs, and every target runs it before it serves.

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
- [ ] If this release ships a migration that repairs existing data or changes
      what users see (any auth migration signs everyone out), add an entry to
      [Upgrade Notes](#upgrade-notes): every target applies it on upgrade

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
