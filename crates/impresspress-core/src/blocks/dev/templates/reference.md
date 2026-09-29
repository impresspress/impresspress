# Writing a backend block

A block is one Rust crate, compiled to `wasm32-wasip1` in the browser and
activated into the running site. Read this before writing any Rust: the
compiler in this sandbox is not the one on your laptop, and the rules below
are what the sandbox refuses a block for.

Scaffold one with `dev_create_block` rather than writing the files by hand —
it writes the vendored support module for you, and that module is the whole
SDK.

## Layout

```
blocks/<name>/
  Cargo.toml            # no dependencies, ever
  src/lib.rs            # your block
  src/wafer_guest.rs    # vendored support module — do not edit
```

`<name>` is 2–32 characters matching `^[a-z][a-z0-9-]{1,31}$`, with no
doubled hyphen and no trailing hyphen. It is a directory, a crate name and
half of a block id all at once, so the alphabet is the intersection of what
all three accept. There is no underscore: the runtime refuses one in a block
id.

**The crate is flat: `Cargo.toml` at the root, every source file directly in
`src/`, and no other directory.** Put every module beside `lib.rs` —
`src/handlers.rs`, not `src/handlers/mod.rs` — and keep `tests/`, `assets/`
and the like out of `blocks/<name>/` entirely. The sandbox refuses any path
that needs a directory it does not have, with a `nested-source` diagnostic:
the browser toolchain writes each file into its VFS by path, and whether that
creates the intermediate directories has not been verified, so a crate laid
out that way would fail somewhere inside rustc rather than here.

A block named `<name>` is registered as `site/<name>` and everything else
follows from that:

| What | Shape | Example for `blocks/newsletter` |
| --- | --- | --- |
| Block id | `site/<name>` | `site/newsletter` |
| Routes | `/b/<name>/…` | `/b/newsletter/subscribe` |
| Collections | `site__<name>__*` | `site__newsletter__subscribers` |
| Storage folder | `site/<name>` and below | `site/newsletter/uploads` |
| Config keys | `SITE__<NAME>__*` | `SITE__NEWSLETTER__FROM_ADDRESS` |

A hyphen in the name stays a hyphen in the block id, the routes and the
storage folder, and is spelled `_` in collections and config keys:
`blocks/my-shop` claims `site__my_shop__*` and `SITE__MY_SHOP__*`. A
collection name is lowercase letters, digits and `_` only — the database uses
it as written.

Naming anything outside those is refused when the block is staged, with a
diagnostic (`cap-collection`, `cap-folder`, `cap-config`,
`endpoint-outside-routes`) naming the entry.

## `Cargo.toml`: no dependencies

The compiler runs in your browser and **has no registry access**. It can
build `core` and `std` and nothing else, so the `[dependencies]` table is
empty and stays empty. Adding a crate does not produce a slow build — it
produces a build that cannot start.

Everything you would reach for a crate for is in `src/wafer_guest.rs`: JSON,
the request/response types, schemas, and the database / storage / config
clients. No `serde`, no `serde_json`, no `uuid`, no `chrono`.

There are no procedural macros either, so there is nothing to derive. A
struct that has to cross the wire is built as a `json::Json` value by hand.

`[package] name` must stay equal to the block's directory name, and if you
add a `[lib] name` it must too. Between them they decide what cargo calls the
built `.wasm`, and the sandbox reads that one file back out of the VFS by the
block's name — rename either and the build succeeds with nothing to collect.
The sandbox refuses the mismatch before it compiles, with a `package-name`
diagnostic. (Hyphens become underscores in the file name, as cargo does it:
`my-shop` builds `my_shop.wasm`.)

## The `block()` and `init()` functions

`src/lib.rs` must define exactly two public functions. The vendored module's
ABI exports call them; nothing else is required of you.

```rust
pub fn block() -> Block;
pub fn init(ctx: &Ctx) -> Result<(), String>;
```

`block()` is the block's declaration — its id, the resources it claims, and
the endpoints it serves. It is not documentation: the sandbox validates it,
and the capabilities it implies are the **only** authority the compiled block
gets. A collection you do not claim is a `PermissionDenied` at run time.

```rust
Block::new("site/newsletter", "Newsletter signups")
    .version("0.2.0")                            // defaults to "0.1.0"
    .requires(&[DATABASE])                       // DATABASE / STORAGE / CONFIG
    .collection("site__newsletter__subscribers") // also turns on `schema`
    .storage_folder("site/newsletter")
    .config_key("SITE__NEWSLETTER__FROM_ADDRESS")
    .endpoint(/* … */)
```

`.version(..)` is the version the block reports about itself, and it is only
that: the sandbox versions a deployment by generation, so nothing routes,
caches or refuses on it. Set it if it means something to you, leave it alone
if it does not.

`init()` runs once, when the block is activated, and is where a block creates
its tables. It is run again on every activation, so it must be idempotent —
`db::ensure_table` is. Returning `Err` fails the activation, so a block never
serves in a half-configured state.

## Requests and responses

A handler is a plain function:

```rust
fn subscribe(request: &Request, ctx: &Ctx) -> Response { /* … */ }
```

`Request` is already decoded:

| Field / method | What it holds |
| --- | --- |
| `request.method`, `request.path` | Uppercased method, path without the query |
| `request.param("id")` | A `{id}` captured by the route template |
| `request.query("page")` | A decoded query parameter |
| `request.header("accept")` | A request header; names are lowercase. `cookie` and `authorization` never arrive (see [Headers](#headers)) |
| `request.json()` | The body as `json::Json` |
| `request.body` | The raw body bytes |
| `request.user_id`, `request.user_email`, `request.roles` | Who is calling; `None` when nobody is signed in |
| `request.has_role("admin")` | Whether the caller holds a role |

`Response` is built with one of three constructors and optional headers:

```rust
Response::json(200, &json::Json::obj().set("ok", json::Json::Bool(true)))
Response::text(404, "not found")
Response::bytes(200, "image/png", pixels).header("Cache-Control", "no-store")
```

### Headers

The host decides which headers cross the block's boundary, in both
directions, and a block cannot change that:

- **In:** a block never sees the request's `Cookie`, `Authorization` or
  `Proxy-Authorization` header. The admin's session cookie travels on every
  same-origin request, `/b/<name>/` included, and it stops at the host. Who is
  calling reaches the block as `request.user_id`, `request.user_email` and
  `request.roles` instead.
- **Out:** a block cannot set `Set-Cookie`, `Location`, `Refresh`,
  `Clear-Site-Data`, any `Access-Control-*` (CORS) header,
  `Strict-Transport-Security`, `X-Frame-Options` or
  `Content-Security-Policy` (or its `-Report-Only` twin). The host drops them
  from every answer the block gives, an error included. The site's own
  middleware supplies the CORS and security headers.
- Every other header (`Cache-Control`, `X-*`, …) passes through as the block
  set it.

The `headers.readable` / `headers.writable` capabilities that would lift these
rules are refused at staging (`cap-headers`): a sandbox block serves an
unauthenticated route, so it is never trusted with the session.

Return an error `Response` for anything a caller can cause. A handler that
panics **traps the instance** — the crate is built with `panic = "abort"`, so
there is no unwinding to catch and the request fails with a 500 that says
nothing useful.

### Working with JSON

`json::Json` is the sandbox's own JSON value. Build objects by chaining
`set`, read them with `get` plus one of the `as_*` accessors:

```rust
use json::Json;

let body = request.json().unwrap_or(Json::Null);
let Some(email) = body.get("email").and_then(Json::as_str) else {
    return Response::text(400, "`email` is required");
};

let out = Json::obj()
    .set("email", Json::str(email))
    .set("count", Json::int(3))
    .set("tags", Json::Arr(vec![Json::str("a"), Json::str("b")]));
```

`as_str`, `as_i64`, `as_f64`, `as_bool` and `as_array` all return `None` when
the value is of another type — `as_i64` on `1.5` is `None`, never a truncated
`1`.

`Json::parse(text) -> Result<Json, String>` reads one complete document —
trailing data is an error, not something ignored — and `value.render()`
writes it back as compact JSON. `Response::json` renders for you, so these
two are for the text you handle yourself: a JSON column read out of a record,
a body you want to log.

## Routes and path params

Every endpoint's path must start with the block's own `/b/<name>/` prefix.
A `{name}` segment captures a path parameter:

```rust
Endpoint::new(Method::Get, "/b/newsletter/subscribers/{id}", get_subscriber)
    .auth(Auth::Admin)
    .summary("Read one subscriber")
```

Matching is per whole segment, so `/b/x/items/{id}` matches `/b/x/items/7`
and never `/b/x/items/7/tags`. The first endpoint declared that matches wins,
so declare the specific route before the general one. A request that matches
nothing gets a `404`.

`Method` is `Get`, `Post`, `Patch` or `Delete`. There is no `Put`: the
runtime's endpoint type has no such method, so a block that declared one
would fail to load at all. Use `Patch` for a partial update and `Post` for a
replacement.

`Auth` is the tier the **router** enforces before the handler runs:

- `Auth::Public` — anyone, signed in or not.
- `Auth::Authenticated` — any signed-in user.
- `Auth::Admin` — the `admin` role.

A path under your prefix that no endpoint declares falls back to
`Authenticated`, so forgetting to declare an endpoint fails closed rather
than open.

## Agent tools and schemas

`.input(..)` and `.output(..)` publish an endpoint's JSON Schema into
`/openapi.json`. `.agent_tool(name, description)` additionally exposes it to
agents — the schema then becomes the tool's `inputSchema`, which is what an
agent reads to work out how to call it.

```rust
Endpoint::new(Method::Post, "/b/newsletter/subscribe", subscribe)
    .auth(Auth::Public)
    .summary("Subscribe an email address")
    .input(
        Schema::object()
            .prop("email", Schema::string().describe("Email address"))
            .required(&["email"]),
    )
    .output(Schema::object().prop("ok", Schema::boolean()))
    .agent_tool(
        "subscribe_newsletter",
        "Subscribe an email address to the newsletter. Creates a subscriber \
         row; duplicates are rejected.",
    )
```

`Schema` builds a JSON Schema fragment: `object()`, `string()`, `integer()`,
`number()`, `boolean()`, `array(items)`, `enum_of(&["a", "b"])`, then `.prop`,
`.required` and `.describe`.

Write the tool description for the agent that has to decide whether to call
it, and **name the side effect** — "creates a subscriber row", not "handles
subscriptions".

Tool names are unique across the whole deployment. Staging a block whose tool
name another block already claims is refused with a `tool-name-duplicate`
diagnostic, so rename the ones a template gave you if you scaffold two blocks
from it.

## Database

`db::*` speaks to `wafer-run/database`. Declare `.requires(&[DATABASE])` and
claim every collection you touch.

A **record** is `{"id": …, "data": {…}}` — the id is the primary key, `data`
holds the row's columns. The host generates an id when your data carries
none, and stamps `created_at` on insert and `updated_at` on every write.

### `db::ensure_table`

```rust
db::ensure_table(
    ctx,
    TableDef::new("site__newsletter__subscribers")
        .column(Column::text("id").primary_key())
        .column(Column::text("email").not_null().unique())
        .column(Column::datetime("created_at").default_now())
        .index(&["created_at"], false),
)?;
```

Columns are `string`, `text`, `int`, `int64`, `float`, `bool`, `datetime`,
`json` and `blob`, and are **nullable by default** — `.not_null()`,
`.primary_key()`, `.unique()`, `.auto_increment()`, `.default_now()` and
`.default_value(..)` tighten that.

This is the only schema tool a block gets, and it is enough: raw SQL and raw
DDL are never granted. Claiming a collection is what turns on the capability
that authorizes it, so `ensure_table` on an unclaimed table is refused.

### Reads and writes

```rust
let record = db::create(ctx, TABLE, Json::obj().set("email", Json::str(email)))?;
let record = db::get(ctx, TABLE, id)?;                 // NotFound if absent
let record = db::update(ctx, TABLE, id, Json::obj().set("email", Json::str(new)))?;
db::delete(ctx, TABLE, id)?;

let rows = db::list(
    ctx,
    TABLE,
    ListOptions::new()
        .filter("email", "like", Json::str("%@example.com"))
        .sort("created_at", true)   // true = descending
        .limit(50)
        .offset(0),
)?;

let n = db::count(ctx, TABLE, &[Filter::new("email", "eq", Json::str(email))])?;
```

Filter operators are `eq`, `neq`, `gt`, `gte`, `lt`, `lte`, `like`, `in`,
`is_null` and `is_not_null`. Anything else is refused by the host.

Always set a `limit` on a list a user can grow. A list with no `limit`
returns every matching row; `limit(0)` is refused, and so is an `offset`
without a `limit`.

## Storage

`storage::*` speaks to `wafer-run/storage`. Declare
`.requires(&[STORAGE])` and `.storage_folder("site/<name>")`.

```rust
storage::put(ctx, "site/newsletter", "logo.png", &bytes, "image/png")?;
let (bytes, content_type) = storage::get(ctx, "site/newsletter", "logo.png")?;
let keys = storage::list(ctx, "site/newsletter", "")?;
storage::delete(ctx, "site/newsletter", "logo.png")?;
```

An object lives at `{folder}/{key}` and the host authorizes on that whole
string. A key with a `.` or `..` segment is refused outright, whatever you
claimed.

## Config

`config::*` speaks to `wafer-run/config`. Declare `.requires(&[CONFIG])` and
`.config_key("SITE__<NAME>__…")` for every key you read; an admin sets their
values.

```rust
let from = config::get(ctx, "SITE__NEWSLETTER__FROM_ADDRESS")?
    .unwrap_or_else(|| "hello@example.com".to_string());
```

`None` means the key is unset. Never hardcode a deployment-specific value —
that is what config is for.

## Logging

```rust
log::error("could not reach the database");
log::warn("…");
log::info("…");
log::debug("…");
```

Log lines go to the deployment's logs, not to any HTTP response, so they are
for the operator. Never put a host error's message in a response body on a
public route: it can name internals. Log it, and answer with something
generic.

Logging needs no declaration: it is a direct host call, not a cross-block
one, so `wafer-run/logger` does not belong in `.requires(..)`.

## Errors from the host

Every `db::*`, `storage::*` and `config::*` call returns
`Result<_, HostError>`. `HostError::code` is the host's error code by name —
`NotFound`, `PermissionDenied`, `InvalidArgument`, `AlreadyExists`,
`Internal`, … — so match on the code, not on the message:

```rust
match db::get(ctx, TABLE, id) {
    Ok(record) => Response::json(200, &record),
    Err(error) if error.code == "NotFound" => Response::text(404, "no such row"),
    Err(error) => {
        log::error(&format!("site/newsletter: {error}"));
        Response::text(503, "temporarily unavailable")
    }
}
```

A write that duplicates a primary or `unique()` key fails with
`AlreadyExists`, so a unique column is its own duplicate check — insert, and
answer `409` on that code, rather than counting first.

A `PermissionDenied` almost always means the block did not claim the resource
it just tried to reach. Fix the declaration in `block()`, not the call.

## Limits

| Limit | Value |
| --- | --- |
| Target | `wasm32-wasip1`, `std` only |
| Dependencies | **none** — the compiler has no registry |
| Proc macros | none |
| Compiled artifact | ≤ 4 MiB |
| Workspace file | ≤ 512 KiB |
| Workspace total | ≤ 64 MiB of stored blobs |
| Blocks per workspace | 16 |
| Network | none — a block cannot make outbound requests |
| Cross-block calls | only `wafer-run/database`, `wafer-run/storage`, `wafer-run/config` |
| Raw SQL / raw DDL | never granted |
| Crypto, vector indexes | never granted |
| Sensitive headers | never granted — see [Headers](#headers) |
| Compiles at a time | one |

The size settings in the scaffolded `[profile.release]` (`opt-level = "z"`,
`lto`, `codegen-units = 1`, `panic = "abort"`, `strip`) are what keep a block
well inside the 4 MiB limit. Do not remove them.

A block needing something on the "never granted" list is not a block: put
that work in a page, which talks to other origins over HTTP like any web
page, or in a built-in block.

## What a refusal looks like

Compiling and staging answer with a list of **diagnostics**, each carrying a
stable `code`. A refusal is a `200` with `success: false`, not a transport
error — match on the code and fix the named thing:

```json
{
  "success": false,
  "diagnostics": [
    {
      "severity": "error",
      "code": "cap-collection",
      "message": "the guest declares the collection \"site__other__rows\", which is outside site__newsletter__*",
      "file": null, "line": null, "column": null
    }
  ]
}
```

Diagnostics from `rustc` carry `file`, `line` and `column`; the sandbox's own
rules carry a `code` and no position, because they are statements about your
declaration rather than about a span.

The codes you are most likely to see:

| Code | What to change |
| --- | --- |
| `name-mismatch` | `Block::new` must say `site/<name>` for the directory the sources are in |
| `endpoint-outside-routes` | An endpoint path outside `/b/<name>/` |
| `cap-collection` / `cap-folder` / `cap-config` | A claim outside your namespace |
| `cap-ddl` / `cap-raw-sql` | Never granted; use `db::ensure_table` and the typed ops |
| `cap-headers` | Never granted; see [Headers](#headers) |
| `cap-callable` / `cap-requires-mismatch` | `requires` must name exactly the platform services you use |
| `tool-name-duplicate` | Another block already publishes that agent tool name |
| `route-collision` | Another block, or a built-in route, already serves that prefix |
| `binary-source` | A file under `blocks/<name>/` the toolchain cannot read as text |
| `package-name` | `Cargo.toml`'s `[package] name` must be the block's directory name |
| `nested-source` | A file in any subdirectory — the crate is `Cargo.toml` plus a flat `src/` |
| `artifact-too-large` | Restore the `[profile.release]` size settings |
| `wafer-guest-version` | The block was built against an older `wafer_guest.rs`: replace its `src/wafer_guest.rs` with `wafer_guest_module` from `GET /b/dev/api/reference`, then compile again |
| `guest-load` / `guest-info` / `guest-init` / `guest-probe` | The module was loaded and something failed at that stage — the message is the host's |

## Template: `hello`

The smallest block that serves anything.

```rust
{{TEMPLATE_HELLO}}
```

## Template: `table`

A block with a table behind it.

```rust
{{TEMPLATE_TABLE}}
```
